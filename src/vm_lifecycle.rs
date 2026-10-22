//! VM lifecycle helpers shared by the interactive runner and the headless
//! maintenance runner: start, request a guest shutdown, force stop, and
//! `run_headless`.

use std::{
    fs::{self, File},
    io::{self, Write},
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::io::RawFd,
    },
    sync::mpsc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::{AnyThread, rc::Retained};
use objc2_foundation::*;
use objc2_virtualization::*;

use crate::control::{Frame, FrameError, FrameReader};
use crate::vm_config::{SerialRole, VmSpec, create_vm_configuration};
use crate::vm_io::create_pipe;

const START_TIMEOUT: Duration = Duration::from_secs(60);

/// Maintenance VM: from start until `PREP-READY`. Measured boot is about 17s,
/// including limine's 10s menu.
pub(crate) const MAINT_FIRST_OUTPUT_TIMEOUT: Duration = Duration::from_secs(90);
/// Maintenance VM: from start until `PREP-RESULT`.
pub(crate) const MAINT_TOTAL_TIMEOUT: Duration = Duration::from_secs(300);
/// After a stop is requested, wait this long before forcing the VM off.
pub(crate) const STOP_TIMEOUT: Duration = Duration::from_secs(30);
/// After the VM has stopped, worker threads must leave within this long.
pub(crate) const WORKER_JOIN_TIMEOUT: Duration = Duration::from_secs(10);
/// Force-stop's completion is about 10ms; allow the state to reach Stopped.
const FORCE_STOP_SETTLE: Duration = Duration::from_secs(5);

/// Phase deadlines passed to [`run_headless`]. [`HeadlessDeadlines::maintenance`]
/// is used for the Alpine maintenance VM. Tests pass a shorter `first_output`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeadlessDeadlines {
    pub(crate) first_output: Duration,
    pub(crate) total: Duration,
    pub(crate) stop: Duration,
    pub(crate) worker_join: Duration,
}

impl HeadlessDeadlines {
    /// Deadlines for one Alpine maintenance VM.
    pub(crate) fn maintenance() -> Self {
        Self {
            first_output: MAINT_FIRST_OUTPUT_TIMEOUT,
            total: MAINT_TOTAL_TIMEOUT,
            stop: STOP_TIMEOUT,
            worker_join: WORKER_JOIN_TIMEOUT,
        }
    }
}

/// What the main thread should do on one turn of the headless run loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeadlessStep {
    /// Pump the run loop again.
    Pump,
    /// Ask the guest to stop.
    RequestStop,
    /// `stopWithCompletionHandler`. The guest ignored the request.
    ForceStop,
    /// The VM is no longer running, or force-stop did not settle.
    Return,
}

/// Snapshot the step function reads. Durations are relative to the run start
/// so tests can cover the timeouts without sleeping.
struct HeadlessProgress {
    elapsed: Duration,
    saw_ready: bool,
    saw_result: bool,
    /// When `PREP-RESULT` arrived, relative to the run start.
    result_for: Option<Duration>,
    /// When `request_stop` was issued, relative to the run start.
    stop_for: Option<Duration>,
    /// When force-stop was issued, relative to the run start.
    force_for: Option<Duration>,
    /// A protocol error or callback failure is already recorded.
    fault: bool,
    vm_running: bool,
}

fn headless_step(progress: &HeadlessProgress, deadlines: &HeadlessDeadlines) -> HeadlessStep {
    if !progress.vm_running {
        return HeadlessStep::Return;
    }
    if let Some(at) = progress.force_for {
        if progress.elapsed >= at + FORCE_STOP_SETTLE {
            return HeadlessStep::Return;
        }
        return HeadlessStep::Pump;
    }
    if let Some(at) = progress.stop_for {
        if progress.elapsed >= at + deadlines.stop {
            return HeadlessStep::ForceStop;
        }
        return HeadlessStep::Pump;
    }
    let timed_out = if progress.fault {
        true
    } else if !progress.saw_ready {
        progress.elapsed >= deadlines.first_output
    } else if !progress.saw_result {
        progress.elapsed >= deadlines.total
    } else {
        progress
            .result_for
            .is_some_and(|at| progress.elapsed >= at + deadlines.stop)
    };
    if timed_out {
        HeadlessStep::RequestStop
    } else {
        HeadlessStep::Pump
    }
}

fn timeout_reason(saw_ready: bool, saw_result: bool) -> &'static str {
    if !saw_ready {
        "timed out waiting for PREP-READY"
    } else if !saw_result {
        "timed out waiting for PREP-RESULT"
    } else {
        "timed out waiting for the VM to stop after PREP-RESULT"
    }
}

/// Called on the main thread for each accepted frame.
pub(crate) enum FrameDecision {
    Continue,
    /// Policy failure. The VM is stopped and this text is the outcome error.
    Fail(String),
}

/// How a headless run ended. The VM has been stopped (or the error says it
/// could not be) and the worker threads have been joined before this is returned.
pub(crate) struct HeadlessOutcome {
    /// Accepted frames, in arrival order.
    #[allow(dead_code)]
    pub(crate) frames: Vec<Frame>,
    pub(crate) error: Option<String>,
    /// Stopped without a timeout, protocol error, callback failure, or force-stop.
    pub(crate) stopped_normally: bool,
    pub(crate) forced_stop: bool,
    /// From the start request until the workers were joined.
    pub(crate) elapsed: Duration,
}

enum ControlEvent {
    Frames(Vec<Result<Frame, FrameError>>),
}

enum PumpEnd {
    Eof,
    Shutdown,
}

/// Creates a VM from `config` on the main dispatch queue, starts it, and runs
/// the main run loop until the start completion handler fires.
pub(crate) fn start_vm(
    config: &VZVirtualMachineConfiguration,
) -> Result<Retained<VZVirtualMachine>, Box<dyn std::error::Error>> {
    let queue = DispatchQueue::main();

    let vm = unsafe {
        VZVirtualMachine::initWithConfiguration_queue(VZVirtualMachine::alloc(), config, queue)
    };

    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let completion_handler = RcBlock::new(move |error: *mut NSError| {
        if error.is_null() {
            let _ = tx.send(Ok(()));
        } else {
            let err = unsafe { &*error };
            let _ = tx.send(Err(format!("{:?}", err.localizedDescription())));
        }
    });

    unsafe {
        vm.startWithCompletionHandler(&completion_handler);
    }

    let start_deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < start_deadline {
        unsafe {
            NSRunLoop::mainRunLoop().runMode_beforeDate(
                NSDefaultRunLoopMode,
                &NSDate::dateWithTimeIntervalSinceNow(0.1),
            )
        };

        match rx.try_recv() {
            Ok(result) => {
                result.map_err(|e| format!("Failed to start VM: {e}"))?;
                break;
            }
            Err(mpsc::TryRecvError::Empty) => continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err("VM start channel disconnected".into());
            }
        }
    }

    if Instant::now() >= start_deadline {
        return Err("Timed out waiting for VM to start".into());
    }

    Ok(vm)
}

/// Asks the guest to stop; falls back to a hard stop when a request is not possible.
pub(crate) fn request_stop(vm: &VZVirtualMachine) {
    unsafe {
        if vm.canRequestStop() {
            if let Err(err) = vm.requestStopWithError() {
                eprintln!("Failed to request VM stop: {err:?}");
            }
        } else if vm.canStop() {
            let handler = RcBlock::new(|_error: *mut NSError| {});
            vm.stopWithCompletionHandler(&handler);
        }
    }
}

/// Stops the VM without asking the guest. The completion runs on the VM queue;
/// the caller pumps the run loop until the state leaves Running.
pub(crate) fn force_stop(vm: &VZVirtualMachine) {
    unsafe {
        if vm.state() == VZVirtualMachineState::Stopped {
            return;
        }
        if !vm.canStop() {
            eprintln!(
                "Failed to force-stop VM: cannot stop from state {:?}",
                vm.state()
            );
            return;
        }
        let handler = RcBlock::new(|error: *mut NSError| {
            if !error.is_null() {
                let err = &*error;
                eprintln!("Failed to force-stop VM: {:?}", err.localizedDescription());
            }
        });
        vm.stopWithCompletionHandler(&handler);
    }
}

/// Boots `spec` with no terminal, stdin, or resize handling.
///
/// `hvc0` (the [`SerialRole::Log`] port) is copied into `log_path`. `hvc1`
/// (the [`SerialRole::Control`] port) is parsed with `control_reader` and each
/// frame is handed to `on_frame`. A timeout or protocol error requests a
/// guest stop, waits [`HeadlessDeadlines::stop`], then force-stops. The VM is
/// stopped and both worker threads are joined before the configuration and
/// file descriptors are released.
pub(crate) fn run_headless(
    mut spec: VmSpec<'_>,
    log_path: &std::path::Path,
    control_reader: FrameReader,
    deadlines: HeadlessDeadlines,
    mut on_frame: impl FnMut(&Frame) -> FrameDecision,
) -> Result<HeadlessOutcome, Box<dyn std::error::Error>> {
    let log_fd = take_host_read(&mut spec, SerialRole::Log)?;
    let control_fd = take_host_read(&mut spec, SerialRole::Control)?;
    if let Some(parent) = log_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let log_file = File::create(log_path)?;

    let (log_shutdown_write, log_shutdown_read) = create_pipe();
    let (control_shutdown_write, control_shutdown_read) = create_pipe();
    let (control_tx, control_rx) = mpsc::channel::<ControlEvent>();

    let log_thread = thread::Builder::new()
        .name("vibe-hvc0-log".into())
        .spawn(move || log_worker(log_fd, log_shutdown_read, log_file))?;
    let control_thread = thread::Builder::new()
        .name("vibe-hvc1-control".into())
        .spawn(move || {
            control_worker(
                control_fd,
                control_shutdown_read,
                control_reader,
                control_tx,
            )
        })?;

    let config = match create_vm_configuration(spec) {
        Ok(config) => config,
        Err(err) => {
            let _ = signal(&log_shutdown_write);
            let _ = signal(&control_shutdown_write);
            let _ = join_worker("log", log_thread, deadlines.worker_join);
            let _ = join_worker("control", control_thread, deadlines.worker_join);
            return Err(err);
        }
    };

    let vm = match start_vm(&config) {
        Ok(vm) => vm,
        Err(err) => {
            let _ = signal(&log_shutdown_write);
            let _ = signal(&control_shutdown_write);
            let _ = join_worker("log", log_thread, deadlines.worker_join);
            let _ = join_worker("control", control_thread, deadlines.worker_join);
            drop(config);
            return Err(err);
        }
    };
    // The first-output deadline covers guest boot, which starts once the VM is running.
    let started = Instant::now();

    let mut saw_ready = false;
    let mut saw_result = false;
    let mut result_at: Option<Instant> = None;
    let mut stop_requested_at: Option<Instant> = None;
    let mut force_at: Option<Instant> = None;
    let mut error: Option<String> = None;
    let mut frames = Vec::new();

    let state = loop {
        unsafe {
            NSRunLoop::mainRunLoop().runMode_beforeDate(
                NSDefaultRunLoopMode,
                &NSDate::dateWithTimeIntervalSinceNow(0.1),
            )
        };

        while let Ok(ControlEvent::Frames(batch)) = control_rx.try_recv() {
            for item in batch {
                match item {
                    Ok(frame) => {
                        if matches!(frame, Frame::PrepReady) {
                            saw_ready = true;
                        }
                        if matches!(frame, Frame::PrepResult { .. }) && result_at.is_none() {
                            saw_result = true;
                            result_at = Some(Instant::now());
                        }
                        if error.is_none()
                            && let FrameDecision::Fail(message) = on_frame(&frame)
                        {
                            error = Some(message);
                        }
                        frames.push(frame);
                    }
                    Err(err) => {
                        if error.is_none() {
                            error = Some(format!("control frame error: {err}"));
                        }
                    }
                }
            }
        }

        let state = unsafe { vm.state() };
        let vm_running =
            state != VZVirtualMachineState::Stopped && state != VZVirtualMachineState::Error;
        let progress = HeadlessProgress {
            elapsed: started.elapsed(),
            saw_ready,
            saw_result,
            result_for: result_at.map(|at| at.saturating_duration_since(started)),
            stop_for: stop_requested_at.map(|at| at.saturating_duration_since(started)),
            force_for: force_at.map(|at| at.saturating_duration_since(started)),
            fault: error.is_some(),
            vm_running,
        };
        match headless_step(&progress, &deadlines) {
            HeadlessStep::Pump => {}
            HeadlessStep::RequestStop => {
                if error.is_none() {
                    error = Some(timeout_reason(saw_ready, saw_result).to_string());
                }
                request_stop(&vm);
                stop_requested_at = Some(Instant::now());
            }
            HeadlessStep::ForceStop => {
                force_stop(&vm);
                force_at = Some(Instant::now());
            }
            HeadlessStep::Return => break state,
        }
    };

    let _ = signal(&log_shutdown_write);
    let _ = signal(&control_shutdown_write);
    // Drop the write ends so a missed signal still wakes poll with a hangup.
    drop(log_shutdown_write);
    drop(control_shutdown_write);

    if let Err(message) = join_worker("log", log_thread, deadlines.worker_join) {
        push_error(&mut error, message);
    }
    if let Err(message) = join_worker("control", control_thread, deadlines.worker_join) {
        push_error(&mut error, message);
    }
    while let Ok(ControlEvent::Frames(batch)) = control_rx.try_recv() {
        for frame in batch.into_iter().flatten() {
            frames.push(frame);
        }
    }

    // Release the configuration and the VM before returning so disk attachments
    // are closed. Workers only held the host ends, which are already joined.
    drop(vm);
    drop(config);

    if state == VZVirtualMachineState::Error {
        push_error(&mut error, "VM entered the error state".to_string());
    } else if state != VZVirtualMachineState::Stopped {
        push_error(&mut error, "VM did not stop".to_string());
    } else if error.is_none() && !saw_result {
        let message = if saw_ready {
            "VM stopped before PREP-RESULT"
        } else {
            "VM stopped before PREP-READY"
        };
        error = Some(message.to_string());
    }

    let forced_stop = force_at.is_some();
    Ok(HeadlessOutcome {
        frames,
        stopped_normally: error.is_none()
            && !forced_stop
            && state == VZVirtualMachineState::Stopped,
        error,
        forced_stop,
        elapsed: started.elapsed(),
    })
}

fn push_error(error: &mut Option<String>, message: String) {
    *error = Some(match error.take() {
        Some(previous) => format!("{previous}; {message}"),
        None => message,
    });
}

fn take_host_read(
    spec: &mut VmSpec<'_>,
    role: SerialRole,
) -> Result<OwnedFd, Box<dyn std::error::Error>> {
    let port = spec
        .serial_ports
        .iter_mut()
        .find(|port| port.role == role)
        .ok_or_else(|| format!("headless VM is missing a {} serial port", role.as_str()))?;
    port.host_read.take().ok_or_else(|| {
        format!(
            "headless VM {} serial port has no host read end",
            role.as_str()
        )
        .into()
    })
}

fn signal(fd: &OwnedFd) -> io::Result<()> {
    let byte = [1u8];
    let n = unsafe { libc::write(fd.as_raw_fd(), byte.as_ptr().cast(), 1) };
    if n < 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::BrokenPipe {
            return Ok(());
        }
        return Err(err);
    }
    Ok(())
}

fn join_worker(name: &str, handle: JoinHandle<()>, timeout: Duration) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = handle.join();
        let _ = tx.send(result);
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(format!("{name} thread panicked")),
        Err(_) => Err(format!("{name} thread did not exit within {timeout:?}")),
    }
}

fn set_nonblock(fd: RawFd) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags >= 0 {
        let _ = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    }
}

fn pump_until_shutdown(
    data_fd: OwnedFd,
    shutdown_fd: OwnedFd,
    mut on_bytes: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<PumpEnd> {
    let data = data_fd.as_raw_fd();
    let stop = shutdown_fd.as_raw_fd();
    set_nonblock(data);
    let mut buf = [0u8; 4096];
    loop {
        let mut fds = [
            libc::pollfd {
                fd: data,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: stop,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        let data_ready = fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0;
        if data_ready {
            loop {
                let n = unsafe { libc::read(data, buf.as_mut_ptr().cast(), buf.len()) };
                if n < 0 {
                    let err = io::Error::last_os_error();
                    if err.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    if err.kind() == io::ErrorKind::WouldBlock {
                        break;
                    }
                    return Err(err);
                }
                if n == 0 {
                    return Ok(PumpEnd::Eof);
                }
                on_bytes(&buf[..n as usize])?;
                if (n as usize) < buf.len() {
                    break;
                }
            }
        }
        if fds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            return Ok(PumpEnd::Shutdown);
        }
        if fds[0].revents & libc::POLLHUP != 0 {
            return Ok(PumpEnd::Eof);
        }
    }
}

fn log_worker(data_fd: OwnedFd, shutdown_fd: OwnedFd, mut log: File) {
    let write = |log: &mut File, bytes: &[u8]| -> io::Result<()> {
        log.write_all(bytes)?;
        log.flush()?;
        Ok(())
    };
    let end = pump_until_shutdown(data_fd, shutdown_fd, |bytes| write(&mut log, bytes));
    if let Err(err) = end {
        eprintln!("hvc0 log thread failed: {err}");
    }
    let _ = log.flush();
}

fn control_worker(
    data_fd: OwnedFd,
    shutdown_fd: OwnedFd,
    mut reader: FrameReader,
    tx: mpsc::Sender<ControlEvent>,
) {
    let end = pump_until_shutdown(data_fd, shutdown_fd, |bytes| {
        let batch = reader.push(bytes);
        if !batch.is_empty() && tx.send(ControlEvent::Frames(batch)).is_err() {
            return Err(io::Error::other("control channel closed"));
        }
        Ok(())
    });
    match end {
        Ok(PumpEnd::Eof) => {
            if let Err(err) = reader.finish() {
                let _ = tx.send(ControlEvent::Frames(vec![Err(err)]));
            }
        }
        Ok(PumpEnd::Shutdown) => {}
        Err(err) => {
            eprintln!("hvc1 control thread failed: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deadlines() -> HeadlessDeadlines {
        HeadlessDeadlines {
            first_output: Duration::from_secs(90),
            total: Duration::from_secs(300),
            stop: Duration::from_secs(30),
            worker_join: Duration::from_secs(10),
        }
    }

    fn running() -> HeadlessProgress {
        HeadlessProgress {
            elapsed: Duration::ZERO,
            saw_ready: false,
            saw_result: false,
            result_for: None,
            stop_for: None,
            force_for: None,
            fault: false,
            vm_running: true,
        }
    }

    #[test]
    fn maintenance_deadlines_have_the_expected_values() {
        assert_eq!(MAINT_FIRST_OUTPUT_TIMEOUT, Duration::from_secs(90));
        assert_eq!(MAINT_TOTAL_TIMEOUT, Duration::from_secs(300));
        assert_eq!(STOP_TIMEOUT, Duration::from_secs(30));
        assert_eq!(WORKER_JOIN_TIMEOUT, Duration::from_secs(10));
        assert_eq!(
            HeadlessDeadlines::maintenance(),
            HeadlessDeadlines {
                first_output: MAINT_FIRST_OUTPUT_TIMEOUT,
                total: MAINT_TOTAL_TIMEOUT,
                stop: STOP_TIMEOUT,
                worker_join: WORKER_JOIN_TIMEOUT,
            }
        );
    }

    #[test]
    fn headless_steps_follow_the_phase_timeouts() {
        let deadlines = deadlines();
        let mut progress = running();

        progress.elapsed = Duration::from_secs(89);
        assert_eq!(headless_step(&progress, &deadlines), HeadlessStep::Pump);

        progress.elapsed = Duration::from_secs(90);
        assert_eq!(
            headless_step(&progress, &deadlines),
            HeadlessStep::RequestStop
        );

        progress.saw_ready = true;
        progress.elapsed = Duration::from_secs(90);
        assert_eq!(headless_step(&progress, &deadlines), HeadlessStep::Pump);
        progress.elapsed = Duration::from_secs(300);
        assert_eq!(
            headless_step(&progress, &deadlines),
            HeadlessStep::RequestStop
        );

        progress.saw_result = true;
        progress.result_for = Some(Duration::from_secs(20));
        progress.elapsed = Duration::from_secs(49);
        assert_eq!(headless_step(&progress, &deadlines), HeadlessStep::Pump);
        progress.elapsed = Duration::from_secs(50);
        assert_eq!(
            headless_step(&progress, &deadlines),
            HeadlessStep::RequestStop
        );

        progress.stop_for = Some(Duration::from_secs(50));
        progress.elapsed = Duration::from_secs(79);
        assert_eq!(headless_step(&progress, &deadlines), HeadlessStep::Pump);
        progress.elapsed = Duration::from_secs(80);
        assert_eq!(
            headless_step(&progress, &deadlines),
            HeadlessStep::ForceStop
        );

        progress.force_for = Some(Duration::from_secs(80));
        progress.elapsed = Duration::from_secs(84);
        assert_eq!(headless_step(&progress, &deadlines), HeadlessStep::Pump);
        progress.elapsed = Duration::from_secs(85);
        assert_eq!(headless_step(&progress, &deadlines), HeadlessStep::Return);

        progress.vm_running = false;
        progress.elapsed = Duration::from_secs(1);
        progress.force_for = None;
        progress.stop_for = None;
        assert_eq!(headless_step(&progress, &deadlines), HeadlessStep::Return);
    }

    #[test]
    fn a_protocol_fault_requests_a_stop_before_the_deadline() {
        let deadlines = deadlines();
        let mut progress = running();
        progress.elapsed = Duration::from_secs(1);
        progress.fault = true;
        assert_eq!(
            headless_step(&progress, &deadlines),
            HeadlessStep::RequestStop
        );

        progress.stop_for = Some(Duration::from_secs(1));
        progress.elapsed = Duration::from_secs(31);
        assert_eq!(
            headless_step(&progress, &deadlines),
            HeadlessStep::ForceStop
        );
    }

    #[test]
    fn timeout_reasons_name_the_phase() {
        assert_eq!(
            timeout_reason(false, false),
            "timed out waiting for PREP-READY"
        );
        assert_eq!(
            timeout_reason(true, false),
            "timed out waiting for PREP-RESULT"
        );
        assert_eq!(
            timeout_reason(true, true),
            "timed out waiting for the VM to stop after PREP-RESULT"
        );
    }
}
