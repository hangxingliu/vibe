//! Host-side VM console I/O: stdin/stdout forwarding, the output monitor used by
//! login actions, terminal raw mode, and terminal resize forwarding.

use std::{
    io::{self, Write},
    os::{
        fd::RawFd,
        unix::{
            io::{AsRawFd, OwnedFd},
            net::UnixStream,
        },
    },
    sync::{
        Arc, Condvar, Mutex,
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::Duration,
};

pub enum VmInput {
    Bytes(Vec<u8>),
    Shutdown,
}

#[derive(Default)]
pub struct OutputMonitor {
    buffer: Mutex<String>,
    condvar: Condvar,
}

impl OutputMonitor {
    fn push(&self, bytes: &[u8]) {
        self.buffer
            .lock()
            .unwrap()
            .push_str(&String::from_utf8_lossy(bytes));
        self.condvar.notify_all();
    }

    pub(crate) fn wait_for_text(&self, needle: &str, timeout: Duration) -> bool {
        let mut found = false;
        let (_unused, _timeout_result) = self
            .condvar
            .wait_timeout_while(self.buffer.lock().unwrap(), timeout, |buf| {
                if let Some((_, remaining)) = buf.split_once(needle) {
                    *buf = remaining.to_string();
                    found = true;
                    false
                } else {
                    true
                }
            })
            .unwrap();

        found
    }

    pub(crate) fn wait_for_line_after(&self, marker: &str, timeout: Duration) -> Option<String> {
        let mut line = None;
        let (_unused, _timeout_result) = self
            .condvar
            .wait_timeout_while(self.buffer.lock().unwrap(), timeout, |buf| {
                let mut line_start = 0;
                let mut marker_seen = false;

                while let Some(line_end_offset) = buf[line_start..].find('\n') {
                    let line_end = line_start + line_end_offset;
                    let current_line = buf[line_start..line_end]
                        .strip_suffix('\r')
                        .unwrap_or(&buf[line_start..line_end]);

                    if marker_seen {
                        line = Some(current_line.to_string());
                        let remaining = buf[line_end + 1..].to_string();
                        *buf = remaining;
                        return false;
                    }

                    marker_seen = current_line == marker;
                    line_start = line_end + 1;
                }

                true
            })
            .unwrap();

        line
    }

    pub(crate) fn snapshot(&self) -> String {
        self.buffer.lock().unwrap().clone()
    }
}

pub struct IoContext {
    pub input_tx: Sender<VmInput>,
    wakeup_write: OwnedFd,
    stdin_thread: thread::JoinHandle<()>,
    mux_thread: thread::JoinHandle<()>,
    resize_thread: thread::JoinHandle<()>,
    stdout_thread: thread::JoinHandle<()>,
}

#[must_use]
pub fn create_pipe() -> (OwnedFd, OwnedFd) {
    let (read_stream, write_stream) = UnixStream::pair().expect("Failed to create socket pair");
    (read_stream.into(), write_stream.into())
}

pub fn spawn_vm_io(
    output_monitor: Arc<OutputMonitor>,
    vm_output_fd: OwnedFd,
    vm_input_fd: OwnedFd,
    resize_control_fd: OwnedFd,
) -> IoContext {
    let (input_tx, input_rx): (Sender<VmInput>, Receiver<VmInput>) = mpsc::channel();

    // raw_guard is set when we've put the user's terminal into raw mode because we've attached stdin/stdout to the VM.
    let raw_guard = Arc::new(Mutex::new(None));

    let (wakeup_read, wakeup_write) = create_pipe();

    enum PollResult<'a> {
        Ready(&'a [u8]),
        Spurious,
        Shutdown,
        Error,
    }

    fn poll_with_wakeup(main_fd: RawFd, wakeup_fd: RawFd, buf: &mut [u8]) -> PollResult<'_> {
        let mut fds = [
            libc::pollfd {
                fd: main_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wakeup_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];

        let ret = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if ret <= 0 || fds[1].revents & libc::POLLIN != 0 {
            PollResult::Shutdown
        } else if fds[0].revents & libc::POLLIN != 0 {
            let n = unsafe { libc::read(main_fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                PollResult::Error
            } else if n == 0 {
                PollResult::Shutdown
            } else {
                PollResult::Ready(&buf[..(n as usize)])
            }
        } else {
            PollResult::Spurious
        }
    }

    // Copies from stdin to the VM; also polls wakeup_read to exit the thread when it's time to shutdown.
    let stdin_thread = thread::spawn({
        let input_tx = input_tx.clone();
        let raw_guard = raw_guard.clone();
        let wakeup_read = wakeup_read.try_clone().unwrap();

        move || {
            let mut buf = [0u8; 1024];
            loop {
                match poll_with_wakeup(libc::STDIN_FILENO, wakeup_read.as_raw_fd(), &mut buf) {
                    PollResult::Shutdown | PollResult::Error => break,
                    PollResult::Spurious => continue,
                    PollResult::Ready(bytes) => {
                        // discard input if the VM hasn't booted up yielded output yet (which triggers us entering raw_mode)
                        if raw_guard.lock().unwrap().is_none() {
                            continue;
                        }
                        if input_tx.send(VmInput::Bytes(bytes.to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });

    // Copies VM output to stdout; also polls wakeup_read to exit the thread when it's time to shutdown.
    let stdout_thread = thread::spawn({
        let raw_guard = raw_guard.clone();
        let wakeup_read = wakeup_read.try_clone().unwrap();

        move || {
            let mut stdout = std::io::stdout().lock();
            let mut buf = [0u8; 1024];
            loop {
                match poll_with_wakeup(vm_output_fd.as_raw_fd(), wakeup_read.as_raw_fd(), &mut buf)
                {
                    PollResult::Shutdown | PollResult::Error => break,
                    PollResult::Spurious => continue,
                    PollResult::Ready(bytes) => {
                        // enable raw mode, if we haven't already
                        let mut raw_guard_inner = raw_guard.lock().unwrap();
                        if raw_guard_inner.is_none()
                            && let Ok(guard) = enable_raw_mode(libc::STDIN_FILENO)
                        {
                            *raw_guard_inner = Some(guard);
                        }

                        if let Err(e) = stdout.write_all(bytes) {
                            eprintln!("[stdout_thread] write failed: {e:?}");
                            break;
                        }
                        let _ = stdout.flush();
                        output_monitor.push(bytes);
                    }
                }
            }
        }
    });

    // Copies data from mpsc channel into VM, so vibe can "type" stuff and run scripts.
    let mux_thread = thread::spawn(move || {
        let mut vm_writer = std::fs::File::from(vm_input_fd);
        loop {
            match input_rx.recv() {
                Ok(VmInput::Bytes(data)) => {
                    if let Err(e) = vm_writer.write_all(&data) {
                        eprintln!("[mux] write failed: {e:?}");
                        break;
                    }
                }
                Ok(VmInput::Shutdown) => break,
                Err(_) => break,
            }
        }
    });

    let resize_thread = thread::spawn({
        let wakeup_read = wakeup_read.try_clone().unwrap();
        move || {
            let mut writer = std::fs::File::from(resize_control_fd);
            let resize_fd = writer.as_raw_fd();
            let flags = unsafe { libc::fcntl(resize_fd, libc::F_GETFL) };
            if flags >= 0 {
                let _ = unsafe { libc::fcntl(resize_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
            }

            loop {
                let mut pollfd = libc::pollfd {
                    fd: wakeup_read.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                let poll_result = unsafe { libc::poll(&raw mut pollfd, 1, 200) };
                if poll_result > 0 && (pollfd.revents & libc::POLLIN) != 0 {
                    break;
                }

                if let Some((rows, cols)) = terminal_size(libc::STDOUT_FILENO) {
                    let message = format!("{rows} {cols}\n");
                    let bytes = message.as_bytes();
                    match writer.write(bytes) {
                        Ok(n) if n == bytes.len() => {}
                        Ok(_) => {}
                        Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                        Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                        Err(err) => {
                            eprintln!("[resize_thread] write failed: {err:?}");
                            break;
                        }
                    }
                }
            }
        }
    });

    IoContext {
        input_tx,
        wakeup_write,
        stdin_thread,
        mux_thread,
        resize_thread,
        stdout_thread,
    }
}

impl IoContext {
    pub fn shutdown(self) {
        let _ = self.input_tx.send(VmInput::Shutdown);
        unsafe { libc::write(self.wakeup_write.as_raw_fd(), b"x".as_ptr().cast(), 1) };
        let _ = self.stdin_thread.join();
        let _ = self.stdout_thread.join();
        let _ = self.mux_thread.join();
        let _ = self.resize_thread.join();
    }
}

fn terminal_size(fd: i32) -> Option<(u16, u16)> {
    let mut winsize: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut winsize) } != 0 {
        return None;
    }
    if winsize.ws_row == 0 || winsize.ws_col == 0 {
        return None;
    }
    Some((winsize.ws_row, winsize.ws_col))
}

fn enable_raw_mode(fd: i32) -> io::Result<RawModeGuard> {
    let mut attributes: libc::termios = unsafe { std::mem::zeroed() };

    if unsafe { libc::tcgetattr(fd, &raw mut attributes) } != 0 {
        return Err(io::Error::last_os_error());
    }

    let original = attributes;

    // Disable translation of carriage return to newline on input
    attributes.c_iflag &= !(libc::ICRNL);
    // Disable canonical mode (line buffering), echo, and signal generation
    attributes.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
    attributes.c_cc[libc::VMIN] = 0;
    attributes.c_cc[libc::VTIME] = 1;

    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw const attributes) } != 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(RawModeGuard { fd, original })
}

struct RawModeGuard {
    fd: i32,
    original: libc::termios,
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &raw const self.original);
        }
    }
}
