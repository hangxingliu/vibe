//! Host-side VM console I/O: stdin/stdout forwarding, the output monitor used by
//! login actions, terminal raw mode, and terminal resize forwarding.
//!
//! User keystrokes stay behind an input gate until the caller opens it (after
//! every automatic login action has finished). Bytes that arrive while the
//! gate is closed are discarded, not queued: replaying them later would mix
//! them into commands the host is still typing. Two consecutive Ctrl-C bytes
//! in that window ask the main thread to stop the VM.

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
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

pub enum VmInput {
    Bytes(Vec<u8>),
    Shutdown,
}

/// Result of waiting for console output.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WaitResult<T> {
    Found(T),
    Timeout,
    /// The monitor was closed (the VM stopped) before a match appeared.
    Closed,
}

#[derive(Default)]
struct MonitorState {
    buffer: String,
    bytes_received: usize,
    closed: bool,
}

/// Buffers VM console output so login actions can wait for text. Matched
/// output is consumed; anything after the match stays for the next wait.
#[derive(Default)]
pub struct OutputMonitor {
    state: Mutex<MonitorState>,
    condvar: Condvar,
}

impl OutputMonitor {
    pub(crate) fn push(&self, bytes: &[u8]) {
        let mut state = self.state.lock().unwrap();
        state.buffer.push_str(&String::from_utf8_lossy(bytes));
        state.bytes_received += bytes.len();
        self.condvar.notify_all();
    }

    /// Marks the output as finished and wakes every waiter. Text already in
    /// the buffer can still be matched; waits that need more output return
    /// `WaitResult::Closed` immediately.
    pub(crate) fn close(&self) {
        self.state.lock().unwrap().closed = true;
        self.condvar.notify_all();
    }

    /// Total number of bytes received from the VM so far.
    pub(crate) fn bytes_received(&self) -> usize {
        self.state.lock().unwrap().bytes_received
    }

    fn wait_until<T>(
        &self,
        timeout: Duration,
        mut try_match: impl FnMut(&mut String) -> Option<T>,
    ) -> WaitResult<T> {
        let deadline = Instant::now().checked_add(timeout);
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(found) = try_match(&mut state.buffer) {
                return WaitResult::Found(found);
            }
            if state.closed {
                return WaitResult::Closed;
            }
            state = match deadline {
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return WaitResult::Timeout;
                    }
                    self.condvar.wait_timeout(state, deadline - now).unwrap().0
                }
                None => self.condvar.wait(state).unwrap(),
            };
        }
    }

    /// Waits for `needle` and consumes the output up to the end of it.
    pub(crate) fn wait_for_text(&self, needle: &str, timeout: Duration) -> WaitResult<()> {
        self.wait_for_any(&[needle], timeout).map(|_| ())
    }

    /// Waits for whichever needle appears first in the output, consumes the
    /// output up to the end of that match, and returns the needle's index.
    /// When several needles start at the same position the earlier one in
    /// `needles` wins.
    pub(crate) fn wait_for_any(&self, needles: &[&str], timeout: Duration) -> WaitResult<usize> {
        self.wait_until(timeout, |buf| {
            let (index, start) = needles
                .iter()
                .enumerate()
                .filter_map(|(index, needle)| buf.find(needle).map(|start| (index, start)))
                .min_by_key(|&(index, start)| (start, index))?;
            buf.drain(..start + needles[index].len());
            Some(index)
        })
    }

    /// Waits for a complete line equal to `marker` (CRLF or LF terminated)
    /// followed by another complete line, consumes both, and returns the
    /// second line without its line ending.
    pub(crate) fn wait_for_line_after(
        &self,
        marker: &str,
        timeout: Duration,
    ) -> WaitResult<String> {
        self.wait_until(timeout, |buf| {
            let mut line_start = 0;
            let mut marker_seen = false;

            while let Some(line_end_offset) = buf[line_start..].find('\n') {
                let line_end = line_start + line_end_offset;
                let current_line = buf[line_start..line_end]
                    .strip_suffix('\r')
                    .unwrap_or(&buf[line_start..line_end]);

                if marker_seen {
                    let line = current_line.to_string();
                    buf.drain(..=line_end);
                    return Some(line);
                }

                marker_seen = current_line == marker;
                line_start = line_end + 1;
            }

            None
        })
    }

    pub(crate) fn snapshot(&self) -> String {
        self.state.lock().unwrap().buffer.clone()
    }
}

impl<T> WaitResult<T> {
    pub(crate) fn map<U>(self, f: impl FnOnce(T) -> U) -> WaitResult<U> {
        match self {
            WaitResult::Found(value) => WaitResult::Found(f(value)),
            WaitResult::Timeout => WaitResult::Timeout,
            WaitResult::Closed => WaitResult::Closed,
        }
    }
}

pub struct IoContext {
    pub input_tx: Sender<VmInput>,
    /// Set when automatic login actions have finished and user keystrokes
    /// may be forwarded to the guest.
    pub(crate) input_open: Arc<AtomicBool>,
    /// Set when the user presses Ctrl-C twice while [`Self::input_open`] is still false.
    pub(crate) stop_requested: Arc<AtomicBool>,
    wakeup_write: OwnedFd,
    stdin_thread: thread::JoinHandle<()>,
    mux_thread: thread::JoinHandle<()>,
    resize_thread: thread::JoinHandle<()>,
    stdout_thread: thread::JoinHandle<()>,
}

/// What to do with one read from the host terminal.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum UserInputDecision {
    /// The gate is open. Forward the bytes unchanged.
    Forward,
    /// The gate is closed. Drop the bytes. `arm_ctrl_c` is true when the
    /// last byte was Ctrl-C, so the next Ctrl-C stops the VM. `hint` is true
    /// when this read armed that pending Ctrl-C and the user should be told.
    Discard { arm_ctrl_c: bool, hint: bool },
    /// The gate is closed and this read completed a pair of Ctrl-C bytes.
    Stop,
}

/// Decides how host keystrokes are treated. Closed-gate input is never
/// returned to the caller, so it cannot be replayed into a later command.
pub(crate) fn decide_user_input(
    input_open: bool,
    mut arm_ctrl_c: bool,
    bytes: &[u8],
) -> UserInputDecision {
    if input_open {
        return UserInputDecision::Forward;
    }
    let mut hint = false;
    for &byte in bytes {
        if byte == 0x03 {
            if arm_ctrl_c {
                return UserInputDecision::Stop;
            }
            arm_ctrl_c = true;
            hint = true;
        } else {
            arm_ctrl_c = false;
        }
    }
    UserInputDecision::Discard { arm_ctrl_c, hint }
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
    let input_open = Arc::new(AtomicBool::new(false));
    let stop_requested = Arc::new(AtomicBool::new(false));

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
        let input_open = input_open.clone();
        let stop_requested = stop_requested.clone();
        let wakeup_read = wakeup_read.try_clone().unwrap();

        move || {
            let mut buf = [0u8; 1024];
            // True after a Ctrl-C that was not followed by any other byte.
            let mut arm_ctrl_c = false;
            loop {
                match poll_with_wakeup(libc::STDIN_FILENO, wakeup_read.as_raw_fd(), &mut buf) {
                    PollResult::Shutdown | PollResult::Error => break,
                    PollResult::Spurious => continue,
                    PollResult::Ready(bytes) => {
                        // Before the first guest byte the terminal is still
                        // cooked, and Ctrl-C is a signal rather than input.
                        // Discard those bytes without counting them.
                        if raw_guard.lock().unwrap().is_none() {
                            continue;
                        }
                        match decide_user_input(
                            input_open.load(Ordering::Acquire),
                            arm_ctrl_c,
                            bytes,
                        ) {
                            UserInputDecision::Forward => {
                                arm_ctrl_c = false;
                                if input_tx.send(VmInput::Bytes(bytes.to_vec())).is_err() {
                                    break;
                                }
                            }
                            UserInputDecision::Discard {
                                arm_ctrl_c: armed,
                                hint,
                            } => {
                                arm_ctrl_c = armed;
                                if hint {
                                    // stderr: the stdout thread holds the stdout lock.
                                    // Raw mode does not translate newline to CRLF.
                                    eprintln!(
                                        "\r\nInput is closed until setup finishes. Press Ctrl-C again to stop the VM.\r"
                                    );
                                }
                            }
                            UserInputDecision::Stop => {
                                eprintln!("\r\nStopping the VM.\r");
                                stop_requested.store(true, Ordering::Release);
                                break;
                            }
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
        input_open,
        stop_requested,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    const SHORT: Duration = Duration::from_millis(50);
    const LONG: Duration = Duration::from_secs(60);

    #[test]
    fn wait_for_text_matches_text_split_across_chunks() {
        let monitor = Arc::new(OutputMonitor::default());
        let waiter = thread::spawn({
            let monitor = monitor.clone();
            move || monitor.wait_for_text("login: ", LONG)
        });
        for chunk in ["debian lo", "gi", "n: rest"] {
            thread::sleep(Duration::from_millis(10));
            monitor.push(chunk.as_bytes());
        }
        assert_eq!(waiter.join().unwrap(), WaitResult::Found(()));
        assert_eq!(monitor.snapshot(), "rest");
        assert_eq!(monitor.bytes_received(), "debian login: rest".len());
    }

    #[test]
    fn wait_for_any_picks_the_earliest_match_and_keeps_the_rest() {
        let monitor = OutputMonitor::default();
        monitor.push(b"Linux 6.12\r\nDebian GNU/Linux banner\r\nPassword: login: ");
        let needles = ["Password:", "Debian GNU/Linux", "login: "];
        assert_eq!(monitor.wait_for_any(&needles, SHORT), WaitResult::Found(1));
        assert_eq!(monitor.snapshot(), " banner\r\nPassword: login: ");
        assert_eq!(monitor.wait_for_any(&needles, SHORT), WaitResult::Found(0));
        assert_eq!(monitor.wait_for_any(&needles, SHORT), WaitResult::Found(2));
        assert_eq!(monitor.snapshot(), "");
    }

    #[test]
    fn wait_for_any_prefers_the_earlier_needle_on_ties() {
        let monitor = OutputMonitor::default();
        monitor.push(b"abcd");
        assert_eq!(
            monitor.wait_for_any(&["abc", "ab"], SHORT),
            WaitResult::Found(0)
        );
        assert_eq!(monitor.snapshot(), "d");
    }

    #[test]
    fn wait_for_line_after_handles_crlf_and_keeps_the_rest() {
        let monitor = OutputMonitor::default();
        monitor.push(b"$ printf 'MAR' 'KER'\r\n\r\nMARKER\r\n0\r\nnext prompt");
        assert_eq!(
            monitor.wait_for_line_after("MARKER", SHORT),
            WaitResult::Found("0".to_string())
        );
        assert_eq!(monitor.snapshot(), "next prompt");
    }

    #[test]
    fn wait_for_line_after_waits_for_a_complete_line() {
        let monitor = OutputMonitor::default();
        monitor.push(b"MARKER\n1");
        assert_eq!(
            monitor.wait_for_line_after("MARKER", SHORT),
            WaitResult::Timeout
        );
        monitor.push(b"7\nrest");
        assert_eq!(
            monitor.wait_for_line_after("MARKER", SHORT),
            WaitResult::Found("17".to_string())
        );
        assert_eq!(monitor.snapshot(), "rest");
    }

    #[test]
    fn waits_time_out_without_a_match() {
        let monitor = OutputMonitor::default();
        monitor.push(b"something else");
        let start = Instant::now();
        assert_eq!(monitor.wait_for_text("login: ", SHORT), WaitResult::Timeout);
        assert!(start.elapsed() >= SHORT);
        assert_eq!(monitor.snapshot(), "something else");
    }

    #[test]
    fn close_wakes_every_waiter() {
        let monitor = Arc::new(OutputMonitor::default());
        let start = Instant::now();
        let text_waiter = thread::spawn({
            let monitor = monitor.clone();
            move || monitor.wait_for_text("login: ", LONG)
        });
        let line_waiter = thread::spawn({
            let monitor = monitor.clone();
            move || monitor.wait_for_line_after("MARKER", Duration::from_secs(24 * 60 * 60))
        });
        thread::sleep(SHORT);
        monitor.close();
        assert_eq!(text_waiter.join().unwrap(), WaitResult::Closed);
        assert_eq!(line_waiter.join().unwrap(), WaitResult::Closed);
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn closed_gate_discards_keystrokes_and_does_not_return_them() {
        assert_eq!(
            decide_user_input(false, false, b"root\n"),
            UserInputDecision::Discard {
                arm_ctrl_c: false,
                hint: false,
            }
        );
        // A pending Ctrl-C is cleared by any other byte, so a later Ctrl-C
        // is a new first press rather than the second of a pair.
        assert_eq!(
            decide_user_input(false, true, b"x\x03"),
            UserInputDecision::Discard {
                arm_ctrl_c: true,
                hint: true,
            }
        );
    }

    #[test]
    fn closed_gate_stops_only_on_two_consecutive_ctrl_c() {
        assert_eq!(
            decide_user_input(false, false, &[0x03]),
            UserInputDecision::Discard {
                arm_ctrl_c: true,
                hint: true,
            }
        );
        assert_eq!(
            decide_user_input(false, true, &[0x03]),
            UserInputDecision::Stop
        );
        assert_eq!(
            decide_user_input(false, false, &[0x03, 0x03]),
            UserInputDecision::Stop
        );
        // Already armed, so the first byte of this read completes the pair.
        // The bytes after that Ctrl-C are not forwarded.
        assert_eq!(
            decide_user_input(false, true, &[0x03, b'a', 0x03]),
            UserInputDecision::Stop
        );
        assert_eq!(
            decide_user_input(false, false, &[0x03, b'a', 0x03]),
            UserInputDecision::Discard {
                arm_ctrl_c: true,
                hint: true,
            }
        );
    }

    #[test]
    fn open_gate_forwards_every_byte_including_ctrl_c() {
        assert_eq!(
            decide_user_input(true, true, &[0x03, 0x03]),
            UserInputDecision::Forward
        );
        assert_eq!(
            decide_user_input(true, false, b"echo ready\n"),
            UserInputDecision::Forward
        );
    }

    #[test]
    fn buffered_text_still_matches_after_close() {
        let monitor = OutputMonitor::default();
        monitor.push(b"login: ");
        monitor.close();
        assert_eq!(
            monitor.wait_for_text("login: ", LONG),
            WaitResult::Found(())
        );
        assert_eq!(monitor.wait_for_text("login: ", LONG), WaitResult::Closed);
    }
}
