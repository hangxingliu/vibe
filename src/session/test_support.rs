//! Helpers shared by the session tests: a fake serial console and login steps.

use std::{
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use crate::vm_io::{OutputMonitor, VmInput};

use super::RootPassword;
use super::failure::LoginFailure;
use super::login::{LoginInfo, legacy_login};
use super::{LoginAction, VmOutput, spawn_login_actions_thread};

pub(super) const SHORT: Duration = Duration::from_millis(100);
pub(super) const LONG: Duration = Duration::from_secs(60);

/// Reads one line the actions thread typed into the VM.
pub(super) fn typed_line(input_rx: &mpsc::Receiver<VmInput>) -> String {
    let mut line = Vec::new();
    while !line.ends_with(b"\n") {
        match input_rx.recv_timeout(LONG).expect("expected VM input") {
            VmInput::Bytes(bytes) => line.extend(bytes),
            VmInput::Shutdown => panic!("unexpected shutdown"),
        }
    }
    line.pop();
    String::from_utf8(line).unwrap()
}

pub(super) fn spawn_login(
    monitor: &Arc<OutputMonitor>,
    timeout: Duration,
    password: Option<RootPassword>,
) -> (
    mpsc::Receiver<VmInput>,
    thread::JoinHandle<Result<LoginInfo, Option<LoginFailure>>>,
) {
    let (input_tx, input_rx) = mpsc::channel();
    let monitor = monitor.clone();
    let handle =
        thread::spawn(move || legacy_login(timeout, password.as_ref(), &monitor, &input_tx));
    (input_rx, handle)
}

pub(super) fn password(value: &str) -> RootPassword {
    RootPassword::parse(value, "--root-password").unwrap()
}

/// Answers a readiness probe the way bash on hvc0 would: echo the command,
/// then print its output.
pub(super) fn answer_probe(monitor: &OutputMonitor, command: &str, uid: &str) {
    let args: Vec<&str> = command.split('\'').collect();
    let marker = format!("{}{}", args[3], args[5]);
    monitor.push(format!("root@debian:~#{command}\r\n\r\n{marker}\r\n{uid}\r\n").as_bytes());
}

/// Drives a login up through the `Password:` prompt and returns the line
/// typed in response.
pub(super) fn login_to_password_prompt(
    monitor: &Arc<OutputMonitor>,
    timeout: Duration,
    password: Option<RootPassword>,
) -> (
    mpsc::Receiver<VmInput>,
    thread::JoinHandle<Result<LoginInfo, Option<LoginFailure>>>,
) {
    let (input_rx, handle) = spawn_login(monitor, timeout, password);
    monitor.push(b"debian login: ");
    assert_eq!(typed_line(&input_rx), "root");
    monitor.push(b"Password: ");
    (input_rx, handle)
}

pub(super) fn actions_thread(
    actions: Vec<LoginAction>,
    monitor: &Arc<OutputMonitor>,
) -> (
    mpsc::Receiver<VmInput>,
    mpsc::Receiver<VmOutput>,
    thread::JoinHandle<()>,
) {
    let (input_tx, input_rx) = mpsc::channel();
    let (output_tx, output_rx) = mpsc::channel();
    let handle = spawn_login_actions_thread(actions, monitor.clone(), input_tx, output_tx);
    (input_rx, output_rx, handle)
}

/// Next message other than `ActionStarted`, if one is queued.
pub(super) fn next_report(output_rx: &mpsc::Receiver<VmOutput>) -> Option<VmOutput> {
    output_rx
        .try_iter()
        .find(|output| !matches!(output, VmOutput::ActionStarted { .. }))
}
