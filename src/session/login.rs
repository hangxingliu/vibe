//! Logging in on the serial console and switching to the vm-user.

use std::{
    hash::{BuildHasher, Hasher},
    sync::mpsc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::vm_io::{OutputMonitor, VmInput, WaitResult};
use crate::vm_user::VmUser;

use super::actions::{Step, send_text};
use super::failure::LoginFailure;
use super::password::RootPassword;

pub(super) const READY_PROBE_MARKER_PREFIX: &str = "VIBE_READY_";

/// Returns a random hex string that is unique enough to tell this probe apart
/// from any other console output.
pub(super) fn probe_nonce() -> String {
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    hasher.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    format!("{:016x}", hasher.finish())
}

/// Builds the readiness probe command and the marker line it prints. The
/// marker is split into two `printf` arguments so the echoed command never
/// contains the complete marker line; the line after the marker is the uid
/// of the shell, which must be 0.
pub(super) fn readiness_probe_command_and_marker(nonce: &str) -> (String, String) {
    let marker = format!("{READY_PROBE_MARKER_PREFIX}{nonce}");
    let command =
        format!(" printf '\\n%s%s\\n%s\\n' '{READY_PROBE_MARKER_PREFIX}' '{nonce}' \"$(id -u)\"");
    (command, marker)
}

pub(super) const SWITCH_PROBE_MARKER_PREFIX: &str = "VIBE_SWITCHED_";
/// How long to wait for one switch probe before typing it again. The probe
/// is typed while `su` is still starting; if the new shell drops that
/// input, a later probe gets through.
pub(super) const SWITCH_PROBE_RETRY: Duration = Duration::from_secs(10);

/// The line typed into the root shell to start the target user's session.
/// The root shell reads the whole line, so the notice is printed only after
/// the user's shell exits.
pub(crate) const SWITCH_USER_COMMAND: &str = " su --login --pty \"$VIBE_VM_USER\"; echo; echo 'Vibe: back in the root shell. Type exit again to power off the VM.'";

/// Builds the switch probe command and the marker line it prints; the line
/// after the marker is `<user name> <uid>` of the shell. The marker is split
/// like the readiness probe's.
pub(super) fn switch_probe_command_and_marker(nonce: &str) -> (String, String) {
    let marker = format!("{SWITCH_PROBE_MARKER_PREFIX}{nonce}");
    let command = format!(
        " printf '\\n%s%s\\n%s %s\\n' '{SWITCH_PROBE_MARKER_PREFIX}' '{nonce}' \"$(id -un)\" \"$(id -u)\""
    );
    (command, marker)
}

/// Whether a switch probe line (`<user name> <uid>`) shows a shell of `user`
/// that is not running as root.
pub(super) fn is_switched_to(line: &str, user: &VmUser) -> bool {
    let mut words = line.split_whitespace();
    words.next() == Some(user.as_str()) && words.next().is_some_and(|uid| uid != "0")
}

/// Types [`SWITCH_USER_COMMAND`] and waits for the probe of the new shell.
/// `Err(None)` means the VM stopped or its input closed.
pub(super) fn switch_user(
    user: &VmUser,
    timeout: Duration,
    output_monitor: &OutputMonitor,
    input_tx: &mpsc::Sender<VmInput>,
) -> Result<(), Option<LoginFailure>> {
    if let Step::Stop = send_text(input_tx, SWITCH_USER_COMMAND.to_string()) {
        return Err(None);
    }
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(Some(LoginFailure::SwitchFailed {
                user: user.clone(),
                got: None,
                timeout,
            }));
        }
        let (command, marker) = switch_probe_command_and_marker(&probe_nonce());
        if let Step::Stop = send_text(input_tx, command) {
            return Err(None);
        }
        match output_monitor.wait_for_line_after(&marker, remaining.min(SWITCH_PROBE_RETRY)) {
            WaitResult::Found(line) if is_switched_to(&line, user) => return Ok(()),
            WaitResult::Found(line) => {
                return Err(Some(LoginFailure::SwitchFailed {
                    user: user.clone(),
                    got: Some(line.trim().to_string()),
                    timeout,
                }));
            }
            WaitResult::Timeout => {}
            WaitResult::Closed => return Err(None),
        }
    }
}

/// Result of a legacy root login that reached a root shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoginInfo {
    /// The guest printed `Password:` and accepted the supplied password.
    pub(crate) password_prompted: bool,
}

/// Logs in as root on the serial console of an image without a Vibe login
/// service. The steps on the console are:
/// 1. Wait for `login: `, then type `root`.
/// 2. Wait for one of `Password:`, the Debian banner, or `login: ` again.
///    On a password prompt, type the password and wait for the banner,
///    `Last login`, or a rejection.
/// 3. Type the readiness probe and wait for its marker line and the uid.
///    Only uid 0 counts as a root shell.
///
/// The probe marker is split into two `printf` arguments. The console echoes
/// the typed command, and the echo must never look like the marker line.
pub(super) fn legacy_login(
    timeout: Duration,
    password: Option<&RootPassword>,
    output_monitor: &OutputMonitor,
    input_tx: &mpsc::Sender<VmInput>,
) -> Result<LoginInfo, Option<LoginFailure>> {
    // `Err(None)` means the VM stopped or its input closed; `run_vm` reports it.
    let stopped = || None;

    match output_monitor.wait_for_text("login: ", timeout) {
        WaitResult::Found(()) => {}
        WaitResult::Timeout if output_monitor.bytes_received() == 0 => {
            return Err(Some(LoginFailure::NoSerialOutput { timeout }));
        }
        WaitResult::Timeout => return Err(Some(LoginFailure::NoLoginPrompt { timeout })),
        WaitResult::Closed => return Err(stopped()),
    }

    if let Step::Stop = send_text(input_tx, "root".to_string()) {
        return Err(stopped());
    }

    // A getty that rejects the login prints the pre-login banner (which also
    // contains "Debian GNU/Linux") and `login: ` again; the readiness probe
    // below catches that case.
    const PASSWORD_PROMPT: usize = 0;
    let password_prompted =
        match output_monitor.wait_for_any(&["Password:", "Debian GNU/Linux", "login: "], timeout) {
            WaitResult::Found(PASSWORD_PROMPT) => {
                let Some(password) = password else {
                    return Err(Some(LoginFailure::PasswordRequired));
                };
                // Typed while the guest has already turned echo off for the prompt.
                // `send_text` does not log the text.
                if let Step::Stop = send_text(input_tx, password.expose().to_string()) {
                    return Err(stopped());
                }
                const LOGIN_INCORRECT: usize = 0;
                const LOGIN_AGAIN: usize = 3;
                match output_monitor.wait_for_any(
                    &[
                        "Login incorrect",
                        "Debian GNU/Linux",
                        "Last login",
                        "login: ",
                    ],
                    timeout,
                ) {
                    WaitResult::Found(LOGIN_INCORRECT | LOGIN_AGAIN) => {
                        return Err(Some(LoginFailure::AuthFailed));
                    }
                    WaitResult::Found(_) => true,
                    WaitResult::Timeout => return Err(Some(LoginFailure::AuthTimeout { timeout })),
                    WaitResult::Closed => return Err(stopped()),
                }
            }
            WaitResult::Found(_) => false,
            WaitResult::Timeout => return Err(Some(LoginFailure::NoBanner { timeout })),
            WaitResult::Closed => return Err(stopped()),
        };

    let (command, marker) = readiness_probe_command_and_marker(&probe_nonce());
    if let Step::Stop = send_text(input_tx, command) {
        return Err(stopped());
    }
    match output_monitor.wait_for_line_after(&marker, timeout) {
        WaitResult::Found(uid) if uid.trim() == "0" => Ok(LoginInfo { password_prompted }),
        WaitResult::Found(uid) => Err(Some(LoginFailure::ProbeNotRoot {
            uid: uid.trim().to_string(),
        })),
        WaitResult::Timeout => Err(Some(LoginFailure::ProbeTimeout { timeout })),
        WaitResult::Closed => Err(stopped()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::*;
    use std::path::Path;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn legacy_login_confirms_a_root_shell() {
        let monitor = Arc::new(OutputMonitor::default());
        let (input_rx, handle) = spawn_login(&monitor, LONG, None);
        monitor.push(b"Debian GNU/Linux 13 debian hvc0\r\n\r\ndebian login: ");
        assert_eq!(typed_line(&input_rx), "root");
        monitor.push(b"root\r\nLinux debian 6.12\r\n\r\nThe programs included with the Debian GNU/Linux system are free software;\r\n");
        let probe = typed_line(&input_rx);
        assert!(probe.starts_with(" printf "));
        answer_probe(&monitor, &probe, "0");
        assert_eq!(
            handle.join().unwrap(),
            Ok(LoginInfo {
                password_prompted: false
            })
        );
    }

    #[test]
    fn legacy_login_rejects_a_non_root_shell() {
        let monitor = Arc::new(OutputMonitor::default());
        let (input_rx, handle) = spawn_login(&monitor, LONG, None);
        monitor.push(b"debian login: ");
        typed_line(&input_rx);
        monitor.push(b"Debian GNU/Linux\r\n");
        let probe = typed_line(&input_rx);
        answer_probe(&monitor, &probe, "1000");
        assert_eq!(
            handle.join().unwrap(),
            Err(Some(LoginFailure::ProbeNotRoot {
                uid: "1000".to_string()
            }))
        );
    }

    #[test]
    fn legacy_login_probe_ignores_the_echoed_command() {
        let monitor = Arc::new(OutputMonitor::default());
        let (input_rx, handle) = spawn_login(&monitor, SHORT, None);
        monitor.push(b"debian login: ");
        typed_line(&input_rx);
        monitor.push(b"Debian GNU/Linux\r\n");
        let probe = typed_line(&input_rx);
        // Only the echo arrives; the shell never runs the command.
        monitor.push(format!("{probe}\r\n").as_bytes());
        assert_eq!(
            handle.join().unwrap(),
            Err(Some(LoginFailure::ProbeTimeout { timeout: SHORT }))
        );
    }

    #[test]
    fn legacy_login_distinguishes_silence_from_missing_prompt() {
        let silent = Arc::new(OutputMonitor::default());
        let (_input_rx, handle) = spawn_login(&silent, SHORT, None);
        assert_eq!(
            handle.join().unwrap(),
            Err(Some(LoginFailure::NoSerialOutput { timeout: SHORT }))
        );

        let noisy = Arc::new(OutputMonitor::default());
        noisy.push(b"[  OK  ] Started some unit\r\n");
        let (_input_rx, handle) = spawn_login(&noisy, SHORT, None);
        assert_eq!(
            handle.join().unwrap(),
            Err(Some(LoginFailure::NoLoginPrompt { timeout: SHORT }))
        );
    }

    #[test]
    fn legacy_login_stops_quietly_when_the_vm_stops() {
        let monitor = Arc::new(OutputMonitor::default());
        let (_input_rx, handle) = spawn_login(&monitor, LONG, None);
        thread::sleep(SHORT);
        monitor.close();
        assert_eq!(handle.join().unwrap(), Err(None));

        let monitor = Arc::new(OutputMonitor::default());
        let (input_tx, input_rx) = mpsc::channel();
        drop(input_rx);
        monitor.push(b"login: ");
        assert_eq!(legacy_login(LONG, None, &monitor, &input_tx), Err(None));
    }

    #[test]
    fn probe_nonces_differ_and_markers_are_split_in_the_command() {
        let nonce = probe_nonce();
        assert_eq!(nonce.len(), 16);
        assert_ne!(nonce, probe_nonce());
        let (command, marker) = readiness_probe_command_and_marker(&nonce);
        assert_eq!(marker, format!("{READY_PROBE_MARKER_PREFIX}{nonce}"));
        assert!(!command.contains(&marker));
    }

    fn spawn_switch(
        monitor: &Arc<OutputMonitor>,
        timeout: Duration,
    ) -> (
        mpsc::Receiver<VmInput>,
        thread::JoinHandle<Result<(), Option<LoginFailure>>>,
    ) {
        let (input_tx, input_rx) = mpsc::channel();
        let monitor = monitor.clone();
        let user = VmUser::parse("dev").unwrap();
        let handle = thread::spawn(move || switch_user(&user, timeout, &monitor, &input_tx));
        (input_rx, handle)
    }

    #[test]
    fn switch_user_runs_su_and_requires_the_target_user() {
        let monitor = Arc::new(OutputMonitor::default());
        let (input_rx, handle) = spawn_switch(&monitor, LONG);
        assert_eq!(typed_line(&input_rx), SWITCH_USER_COMMAND);
        assert!(SWITCH_USER_COMMAND.starts_with(" su --login --pty \"$VIBE_VM_USER\"; echo;"));
        let probe = typed_line(&input_rx);
        assert!(probe.contains("'VIBE_SWITCHED_' '"));
        answer_probe(&monitor, &probe, "dev 1000");
        assert_eq!(handle.join().unwrap(), Ok(()));

        // A failed su leaves the probe to the root shell.
        let (input_rx, handle) = spawn_switch(&monitor, LONG);
        typed_line(&input_rx);
        answer_probe(&monitor, &typed_line(&input_rx), "root 0");
        let failure = handle.join().unwrap().unwrap_err().unwrap();
        assert_eq!(
            failure,
            LoginFailure::SwitchFailed {
                user: VmUser::parse("dev").unwrap(),
                got: Some("root 0".to_string()),
                timeout: LONG,
            }
        );
        let message = failure.describe(Path::new("d.raw"));
        assert!(message.starts_with("[login] Switching to vm-user 'dev'"));
        assert!(!failure.suggests_provision());

        assert!(!is_switched_to("dev 0", &VmUser::parse("dev").unwrap()));
        assert!(!is_switched_to("devx 1000", &VmUser::parse("dev").unwrap()));
    }

    #[test]
    fn switch_user_times_out_without_an_answer() {
        let monitor = Arc::new(OutputMonitor::default());
        let (_input_rx, handle) = spawn_switch(&monitor, SHORT);
        let failure = handle.join().unwrap().unwrap_err().unwrap();
        assert!(matches!(
            failure,
            LoginFailure::SwitchFailed { got: None, .. }
        ));
        assert!(
            failure
                .describe(Path::new("d.raw"))
                .contains("did not answer within")
        );
    }

    #[test]
    fn passwordless_image_does_not_type_a_supplied_password() {
        let monitor = Arc::new(OutputMonitor::default());
        let (input_rx, handle) = spawn_login(&monitor, LONG, Some(password("s3cret-value")));
        monitor.push(b"debian login: ");
        assert_eq!(typed_line(&input_rx), "root");
        monitor.push(b"Debian GNU/Linux\r\n");
        let probe = typed_line(&input_rx);
        assert!(probe.starts_with(" printf "), "{probe}");
        assert!(!probe.contains("s3cret-value"));
        answer_probe(&monitor, &probe, "0");
        assert_eq!(
            handle.join().unwrap(),
            Ok(LoginInfo {
                password_prompted: false
            })
        );
    }

    #[test]
    fn correct_password_reaches_a_root_shell_via_last_login() {
        let secret = "s3cret-value";
        let monitor = Arc::new(OutputMonitor::default());
        let (input_rx, handle) = login_to_password_prompt(&monitor, LONG, Some(password(secret)));
        assert_eq!(typed_line(&input_rx), secret);
        monitor.push(b"Last login: Mon Nov 16 12:00:00 on hvc0\r\n");
        let probe = typed_line(&input_rx);
        assert!(!probe.contains(secret), "{probe}");
        answer_probe(&monitor, &probe, "0");
        assert_eq!(
            handle.join().unwrap(),
            Ok(LoginInfo {
                password_prompted: true
            })
        );
    }

    #[test]
    fn correct_password_accepts_the_debian_banner() {
        let monitor = Arc::new(OutputMonitor::default());
        let (input_rx, handle) =
            login_to_password_prompt(&monitor, LONG, Some(password("s3cret-value")));
        assert_eq!(typed_line(&input_rx), "s3cret-value");
        monitor.push(b"Debian GNU/Linux comes with ABSOLUTELY NO WARRANTY\r\n");
        let probe = typed_line(&input_rx);
        answer_probe(&monitor, &probe, "0");
        assert_eq!(
            handle.join().unwrap(),
            Ok(LoginInfo {
                password_prompted: true
            })
        );
    }

    #[test]
    fn login_incorrect_and_a_repeated_login_prompt_are_auth_failures() {
        let secret = "s3cret-value";
        let monitor = Arc::new(OutputMonitor::default());
        let (input_rx, handle) = login_to_password_prompt(&monitor, LONG, Some(password(secret)));
        assert_eq!(typed_line(&input_rx), secret);
        monitor.push(b"Login incorrect\r\n");
        let failure = handle.join().unwrap();
        assert_eq!(failure, Err(Some(LoginFailure::AuthFailed)));
        assert!(!format!("{failure:?}").contains(secret));

        let monitor = Arc::new(OutputMonitor::default());
        let (input_rx, handle) = login_to_password_prompt(&monitor, LONG, Some(password(secret)));
        assert_eq!(typed_line(&input_rx), secret);
        monitor.push(b"\r\ndebian login: ");
        assert_eq!(handle.join().unwrap(), Err(Some(LoginFailure::AuthFailed)));
    }

    #[test]
    fn password_prompt_without_a_password_names_the_option_and_hides_nothing_secret() {
        let monitor = Arc::new(OutputMonitor::default());
        let (input_rx, handle) = login_to_password_prompt(&monitor, LONG, None);
        // Nothing is typed after `root`; the prompt itself is the failure.
        assert!(
            input_rx.recv_timeout(SHORT).is_err(),
            "a missing password must not type anything"
        );
        assert_eq!(
            handle.join().unwrap(),
            Err(Some(LoginFailure::PasswordRequired))
        );
    }

    #[test]
    fn auth_times_out_after_the_password_is_typed() {
        let secret = "s3cret-value";
        let monitor = Arc::new(OutputMonitor::default());
        let (input_rx, handle) = login_to_password_prompt(&monitor, SHORT, Some(password(secret)));
        assert_eq!(typed_line(&input_rx), secret);
        assert_eq!(
            handle.join().unwrap(),
            Err(Some(LoginFailure::AuthTimeout { timeout: SHORT }))
        );
    }
}
