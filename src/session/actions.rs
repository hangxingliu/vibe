//! The thread that types login actions into the guest and reports their results.

use std::{
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use crate::vm_io::{OutputMonitor, VmInput, WaitResult};

use super::LoginAction;
use super::LoginAction::{
    Expect, LegacyLogin, Script, SecretSend, Send, StatusCommand, SwitchUser, UserScript,
};
use super::failure::LoginFailure;
use super::login::{legacy_login, switch_user};
use super::password::RootPassword;
use super::shell::{parse_status_line, script_command, status_command_and_marker};

pub(super) const SCRIPT_ACTION_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

pub(super) const LOGIN_ACTION_INPUT_CHUNK_BYTES: usize = 256;

pub(super) const LOGIN_ACTION_INPUT_CHUNK_DELAY: Duration = Duration::from_millis(10);

#[derive(Debug)]
pub(crate) enum VmOutput {
    /// The action at `index` (in the full action list) started.
    ActionStarted {
        index: usize,
    },
    LoginActionTimeout {
        index: usize,
        timeout: Duration,
    },
    LoginActionFailed {
        index: usize,
        status: u8,
    },
    LoginFailed(LoginFailure),
    /// The legacy login finished and the readiness probe confirmed a root shell.
    LoginReady {
        /// Whether the guest printed `Password:` and accepted the supplied
        /// password. A non-root session warns when it is false: the image
        /// has no root password.
        password_prompted: bool,
    },
    /// A [`LoginAction::StatusCommand`] set `__vibe_notice`.
    Notice(String),
    /// Every login action ran to completion.
    ActionsCompleted,
}

/// Outcome of one step of the actions thread.
pub(super) enum Step {
    Continue,
    /// Stop running actions: the VM stopped, its input closed, or a failure
    /// was already reported.
    Stop,
}

pub(super) fn send_text(input_tx: &mpsc::Sender<VmInput>, mut text: String) -> Step {
    // Type the newline so the command is actually submitted.
    text.push('\n');
    // Send text to VM in chunks so that we don't overrun its input buffer.
    for chunk in text.as_bytes().chunks(LOGIN_ACTION_INPUT_CHUNK_BYTES) {
        if input_tx.send(VmInput::Bytes(chunk.to_vec())).is_err() {
            // The VM input thread is gone, so the VM is shutting down.
            return Step::Stop;
        }
        thread::sleep(LOGIN_ACTION_INPUT_CHUNK_DELAY);
    }
    Step::Continue
}

/// Runs `login_actions` in order on a background thread. Failures are
/// reported on `vm_output_tx`; `VmOutput::ActionsCompleted` is sent only when
/// every action finished. The thread exits quietly when the output monitor is
/// closed or the VM input channel disconnects.
pub(crate) fn spawn_login_actions_thread(
    login_actions: Vec<LoginAction>,
    output_monitor: Arc<OutputMonitor>,
    input_tx: mpsc::Sender<VmInput>,
    vm_output_tx: mpsc::Sender<VmOutput>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let runner = ActionRunner {
            monitor: &output_monitor,
            input_tx: &input_tx,
            output_tx: &vm_output_tx,
        };
        for (index, action) in login_actions.into_iter().enumerate() {
            let _ = vm_output_tx.send(VmOutput::ActionStarted { index });
            if let Step::Stop = runner.run(index, action) {
                // The failure (if any) is already reported. A closed monitor
                // or input channel means the VM is stopping, and the VM side
                // reports that, so nothing more is sent here.
                return;
            }
        }
        // Sent only after every action succeeded; the VM side treats it as
        // "setup finished" and hands the console to the user.
        let _ = vm_output_tx.send(VmOutput::ActionsCompleted);
    })
}

/// The channels one login action needs.
struct ActionRunner<'a> {
    monitor: &'a OutputMonitor,
    input_tx: &'a mpsc::Sender<VmInput>,
    output_tx: &'a mpsc::Sender<VmOutput>,
}

impl ActionRunner<'_> {
    fn run(&self, index: usize, action: LoginAction) -> Step {
        match action {
            Expect { text, timeout } => self.expect(index, &text, timeout),
            Send(text) => send_text(self.input_tx, text),
            Script { content, .. } => self.script(index, &content, false),
            UserScript { content, .. } => self.script(index, &content, true),
            LegacyLogin { timeout, password } => self.legacy_login(timeout, password.as_deref()),
            SwitchUser { user, timeout } => {
                self.login_step(switch_user(&user, timeout, self.monitor, self.input_tx))
            }
            StatusCommand {
                command, timeout, ..
            } => self.status_command(index, &command, timeout),
            SecretSend {
                marker,
                done_marker,
                password,
                timeout,
            } => self.secret_send(index, &marker, &done_marker, &password, timeout),
        }
    }

    fn report(&self, output: VmOutput) {
        let _ = self.output_tx.send(output);
    }

    fn expect(&self, index: usize, text: &str, timeout: Duration) -> Step {
        match self.monitor.wait_for_text(text, timeout) {
            WaitResult::Found(()) => Step::Continue,
            WaitResult::Timeout => {
                self.report(VmOutput::LoginActionTimeout { index, timeout });
                Step::Stop
            }
            WaitResult::Closed => Step::Stop,
        }
    }

    fn script(&self, index: usize, content: &str, as_vm_user: bool) -> Step {
        let id = format!("script_{index}");
        let (command, status_marker) = script_command(&id, content, as_vm_user);
        match send_text(self.input_tx, command) {
            Step::Stop => Step::Stop,
            Step::Continue => self.await_status(
                &status_marker,
                SCRIPT_ACTION_TIMEOUT,
                index,
                parse_plain_status,
            ),
        }
    }

    fn legacy_login(&self, timeout: Duration, password: Option<&RootPassword>) -> Step {
        let result = legacy_login(timeout, password, self.monitor, self.input_tx).map(|info| {
            self.report(VmOutput::LoginReady {
                password_prompted: info.password_prompted,
            });
        });
        self.login_step(result)
    }

    /// `Err(None)` means the VM stopped; that is reported by the VM side.
    fn login_step(&self, result: Result<(), Option<LoginFailure>>) -> Step {
        match result {
            Ok(()) => Step::Continue,
            Err(failure) => {
                if let Some(failure) = failure {
                    self.report(VmOutput::LoginFailed(failure));
                }
                Step::Stop
            }
        }
    }

    fn status_command(&self, index: usize, command: &str, timeout: Duration) -> Step {
        let (command, marker) = status_command_and_marker(&format!("step_{index}"), command);
        match send_text(self.input_tx, command) {
            Step::Stop => Step::Stop,
            Step::Continue => self.await_status(&marker, timeout, index, parse_status_line),
        }
    }

    fn secret_send(
        &self,
        index: usize,
        marker: &str,
        done_marker: &str,
        password: &RootPassword,
        timeout: Duration,
    ) -> Step {
        match self.monitor.wait_for_text(marker, timeout) {
            WaitResult::Timeout => {
                self.report(VmOutput::LoginActionTimeout { index, timeout });
                Step::Stop
            }
            WaitResult::Closed => Step::Stop,
            WaitResult::Found(()) => {
                match send_text(self.input_tx, password.expose().to_string()) {
                    Step::Stop => Step::Stop,
                    Step::Continue => {
                        self.await_status(done_marker, timeout, index, parse_plain_status)
                    }
                }
            }
        }
    }

    /// Waits for the status line after `marker` and turns it into a step.
    /// `parse` returns the exit status and an optional notice; the notice is
    /// reported before the status is checked. A non-zero status or a timeout
    /// is reported as a failure of action `index`.
    fn await_status(
        &self,
        marker: &str,
        timeout: Duration,
        index: usize,
        parse: fn(&str) -> (u8, Option<String>),
    ) -> Step {
        match self.monitor.wait_for_line_after(marker, timeout) {
            WaitResult::Found(line) => {
                let (status, notice) = parse(&line);
                if let Some(notice) = notice {
                    self.report(VmOutput::Notice(notice));
                }
                if status == 0 {
                    Step::Continue
                } else {
                    self.report(VmOutput::LoginActionFailed { index, status });
                    Step::Stop
                }
            }
            WaitResult::Timeout => {
                self.report(VmOutput::LoginActionTimeout { index, timeout });
                Step::Stop
            }
            WaitResult::Closed => Step::Stop,
        }
    }
}

/// A status line that holds only the exit status. Anything else counts as
/// a failure (`u8::MAX`).
fn parse_plain_status(line: &str) -> (u8, Option<String>) {
    (line.trim().parse().unwrap_or(u8::MAX), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::*;
    use std::time::Instant;

    #[test]
    fn actions_thread_reports_completion_only_after_every_action() {
        let monitor = Arc::new(OutputMonitor::default());
        let actions = vec![
            Send("echo hi".to_string()),
            Expect {
                text: "hi".to_string(),
                timeout: LONG,
            },
        ];
        let (input_rx, output_rx, handle) = actions_thread(actions, &monitor);
        assert_eq!(typed_line(&input_rx), "echo hi");
        assert!(next_report(&output_rx).is_none());
        monitor.push(b"hi\r\n");
        handle.join().unwrap();
        assert!(matches!(
            next_report(&output_rx),
            Some(VmOutput::ActionsCompleted)
        ));
    }

    #[test]
    fn actions_thread_exits_promptly_when_the_monitor_closes() {
        let monitor = Arc::new(OutputMonitor::default());
        let actions = vec![Script {
            name: "sleep.sh".to_string(),
            content: "sleep 3600\n".to_string(),
        }];
        let (_input_rx, output_rx, handle) = actions_thread(actions, &monitor);
        thread::sleep(SHORT);
        let start = Instant::now();
        monitor.close();
        handle.join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(next_report(&output_rx).is_none());
    }

    #[test]
    fn actions_thread_does_not_panic_when_vm_input_is_gone() {
        let monitor = Arc::new(OutputMonitor::default());
        let actions = vec![Send("echo hi".to_string()), Send("echo again".to_string())];
        let (input_rx, output_rx, handle) = actions_thread(actions, &monitor);
        drop(input_rx);
        handle.join().expect("actions thread must not panic");
        assert!(next_report(&output_rx).is_none());
    }

    #[test]
    fn actions_thread_reports_the_index_of_a_failed_script() {
        let monitor = Arc::new(OutputMonitor::default());
        let actions = vec![
            Send("echo hi".to_string()),
            Script {
                name: "fail.sh".to_string(),
                content: "exit 3\n".to_string(),
            },
            Send("never".to_string()),
        ];
        let (input_rx, output_rx, handle) = actions_thread(actions, &monitor);
        assert_eq!(typed_line(&input_rx), "echo hi");
        let id = "script_1";
        // The wrapper is several lines; its last line prints the status marker.
        while !typed_line(&input_rx).contains("VIBE_SCRIPT_STATUS_") {}
        // Reply with the status line the wrapper prints after the script.
        let (_, marker) = script_command(id, "exit 3\n", false);
        monitor.push(format!("\r\n{marker}\r\n3\r\n").as_bytes());
        handle.join().unwrap();
        let started: Vec<usize> = output_rx
            .try_iter()
            .filter_map(|output| match output {
                VmOutput::ActionStarted { index } => Some(index),
                VmOutput::LoginActionFailed { index, status } => {
                    assert_eq!((index, status), (1, 3));
                    None
                }
                _ => panic!("unexpected report"),
            })
            .collect();
        assert_eq!(started, vec![0, 1]);
    }

    #[test]
    fn actions_thread_reports_status_command_notices_and_failures() {
        let monitor = Arc::new(OutputMonitor::default());
        let step = |label: &str| StatusCommand {
            label: label.to_string(),
            command: "check".to_string(),
            timeout: LONG,
            vm_user: None,
        };
        let actions = vec![step("first"), step("second"), Send("never".to_string())];
        let (input_rx, output_rx, handle) = actions_thread(actions, &monitor);
        let typed = typed_line(&input_rx);
        assert!(typed.starts_with(" { check\n} </dev/null\n"), "{typed}");
        assert!(typed.contains("VIBE_STEP_STATUS_"), "{typed}");
        monitor.push(b"\r\nVIBE_STEP_STATUS_step_0\r\n0 groups=sudo\r\n");
        let typed = typed_line(&input_rx);
        assert!(typed.starts_with(" { check\n} </dev/null\n"), "{typed}");
        assert!(typed.contains("VIBE_STEP_STATUS_"), "{typed}");
        monitor.push(b"\r\nVIBE_STEP_STATUS_step_1\r\n10 \r\n");
        handle.join().unwrap();
        let reports: Vec<String> = output_rx
            .try_iter()
            .filter_map(|output| match output {
                VmOutput::ActionStarted { .. } => None,
                other => Some(format!("{other:?}")),
            })
            .collect();
        assert_eq!(
            reports,
            [
                "Notice(\"groups=sudo\")",
                "LoginActionFailed { index: 1, status: 10 }"
            ]
        );
    }
}
