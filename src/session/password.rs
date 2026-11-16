//! The root password option and the login actions that set it in the guest.

use std::{fmt, sync::Arc, time::Duration};

use super::LoginAction;
use super::LoginAction::{SecretSend, Send};
use super::login::probe_nonce;

/// Root password from `--root-password` or `VIBE_ROOT_PASSWORD`.
///
/// `Debug` and `Display` print `***`. The characters are available only
/// through [`RootPassword::expose`], at the moment they are typed.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RootPassword(String);

impl RootPassword {
    /// `source` is `--root-password` or `VIBE_ROOT_PASSWORD` and is named in
    /// the error. The password itself is not.
    pub(crate) fn parse(value: &str, source: &str) -> Result<Self, String> {
        if value.is_empty() {
            return Err(format!("{source} requires a non-empty password"));
        }
        if value.chars().any(is_password_control_char) {
            return Err(format!(
                "{source} contains a control character (U+0000 through U+001F, or U+007F)"
            ));
        }
        Ok(Self(value.to_string()))
    }

    /// Password characters to type into the guest. Do not log the result.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

pub(super) fn is_password_control_char(c: char) -> bool {
    let value = u32::from(c);
    value <= 0x1f || value == 0x7f
}

impl fmt::Debug for RootPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl fmt::Display for RootPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

pub(super) const ROOT_PASSWORD_MARKER_PREFIX: &str = "VIBE_PW_";
pub(super) const ROOT_PASSWORD_DONE_PREFIX: &str = "VIBE_PW_DONE_";
/// Typed right after the login, before the vm-user step and the share
/// mounts; the wait is the same length as the login waits.
pub(super) const SET_ROOT_PASSWORD_TIMEOUT: Duration = Duration::from_secs(120);

/// Actions that set the root password to `password` without placing it on a
/// command line. The guest turns echo off, prints a marker split across two
/// `printf` arguments, reads one line, and passes that line to `chpasswd`.
/// A run, and `provision --repl` of an existing image, do not add these actions.
pub(crate) fn set_root_password_actions(
    password: Arc<RootPassword>,
) -> Result<Vec<LoginAction>, Box<dyn std::error::Error>> {
    let nonce = probe_nonce();
    let marker = format!("{ROOT_PASSWORD_MARKER_PREFIX}{nonce}");
    let done_marker = format!("{ROOT_PASSWORD_DONE_PREFIX}{nonce}");
    // Markers are split across printf arguments so the echoed command line
    // never contains the complete marker.
    let command = format!(
        " stty -echo; printf '\\n%s%s\\n' '{ROOT_PASSWORD_MARKER_PREFIX}' '{nonce}'; \
IFS= read -r __vibe_pw; stty echo; printf 'root:%s\\n' \"$__vibe_pw\" | chpasswd; \
__vibe_status=$?; unset __vibe_pw; \
printf '\\n%s%s\\n%s\\n' '{ROOT_PASSWORD_DONE_PREFIX}' '{nonce}' \"$__vibe_status\""
    );
    debug_assert!(
        !command.contains(&marker),
        "echoed command must not contain the password-read marker"
    );
    debug_assert!(
        !command.contains(&done_marker),
        "echoed command must not contain the password-done marker"
    );
    Ok(vec![
        Send(command),
        SecretSend {
            marker,
            done_marker,
            password,
            timeout: SET_ROOT_PASSWORD_TIMEOUT,
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::failure::LoginFailure;
    use crate::session::test_support::*;
    use crate::session::{LoginAction::LegacyLogin, VmOutput, action_prefix, legacy_login_prefix};
    use crate::vm_io::OutputMonitor;
    use std::path::Path;

    #[test]
    fn root_password_debug_and_setup_actions_do_not_contain_the_password() {
        let secret = "s3cret-value";
        let value = password(secret);
        assert_eq!(format!("{value:?}"), "***");
        assert_eq!(value.to_string(), "***");
        assert!(!format!("{value:?}").contains(secret));

        let stored = Arc::new(value.clone());
        let prefix = legacy_login_prefix(LONG, Some(stored.clone())).unwrap();
        let rendered = format!("{prefix:?}");
        assert!(!rendered.contains(secret), "{rendered}");
        match &prefix[0] {
            LegacyLogin {
                password: Some(stored_password),
                ..
            } => assert_eq!(stored_password.expose(), secret),
            other => panic!("expected the login action, got {other:?}"),
        }

        let actions = set_root_password_actions(stored).unwrap();
        assert_eq!(actions.len(), 2);
        let rendered = format!("{actions:?}");
        assert!(!rendered.contains(secret), "{rendered}");
        let Send(command) = &actions[0] else {
            panic!("expected the chpasswd command");
        };
        assert!(command.starts_with(" stty -echo;"));
        assert!(command.contains("chpasswd"));
        assert!(!command.contains(secret), "{command}");
        let SecretSend {
            marker,
            done_marker,
            password: action_password,
            ..
        } = &actions[1]
        else {
            panic!("expected the secret send");
        };
        assert!(!command.contains(marker), "{command}");
        assert!(!command.contains(done_marker), "{command}");
        assert!(marker.starts_with(ROOT_PASSWORD_MARKER_PREFIX));
        assert!(done_marker.starts_with(ROOT_PASSWORD_DONE_PREFIX));
        assert_eq!(action_password.expose(), secret);
        assert_eq!(actions[1].label(), "set root password");
        assert!(!actions[1].label().contains(secret));
        let caller = 10..10;
        assert_eq!(
            action_prefix(3, &caller, &caller, &actions[1]),
            "[login] setup set root password"
        );
        assert_eq!(
            format!(
                "{} failed with exit code 7",
                action_prefix(3, &caller, &caller, &actions[1])
            ),
            "[login] setup set root password failed with exit code 7"
        );

        for failure in [
            LoginFailure::PasswordRequired,
            LoginFailure::AuthFailed,
            LoginFailure::AuthTimeout { timeout: LONG },
        ] {
            let text = format!("{failure:?} {}", failure.describe(Path::new("disk.raw")));
            assert!(!text.contains(secret), "{text}");
        }
    }

    #[test]
    fn secret_send_types_the_password_only_after_the_marker() {
        let secret = "s3cret-value";
        let actions = set_root_password_actions(Arc::new(password(secret))).unwrap();
        let SecretSend {
            marker,
            done_marker,
            ..
        } = &actions[1]
        else {
            panic!("expected the secret send");
        };
        let marker = marker.clone();
        let done_marker = done_marker.clone();

        let monitor = Arc::new(OutputMonitor::default());
        let (input_rx, output_rx, handle) = actions_thread(actions, &monitor);
        let command = typed_line(&input_rx);
        assert!(!command.contains(secret), "{command}");
        assert!(
            input_rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "password must not be typed before the marker"
        );
        // Echo of the command does not contain the complete marker.
        monitor.push(format!("{command}\r\n").as_bytes());
        assert!(
            input_rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "the echoed command must not be treated as the marker"
        );
        monitor.push(format!("\r\n{marker}\r\n").as_bytes());
        assert_eq!(typed_line(&input_rx), secret);
        monitor.push(format!("\r\n{done_marker}\r\n0\r\n").as_bytes());
        handle.join().unwrap();
        assert!(matches!(
            next_report(&output_rx),
            Some(VmOutput::ActionsCompleted)
        ));
    }

    #[test]
    fn secret_send_reports_a_nonzero_chpasswd_status() {
        let secret = "s3cret-value";
        let actions = set_root_password_actions(Arc::new(password(secret))).unwrap();
        let SecretSend {
            marker,
            done_marker,
            ..
        } = &actions[1]
        else {
            panic!("expected the secret send");
        };
        let marker = marker.clone();
        let done_marker = done_marker.clone();

        let monitor = Arc::new(OutputMonitor::default());
        let (input_rx, output_rx, handle) = actions_thread(actions, &monitor);
        let _command = typed_line(&input_rx);
        monitor.push(format!("\r\n{marker}\r\n").as_bytes());
        assert_eq!(typed_line(&input_rx), secret);
        monitor.push(format!("\r\n{done_marker}\r\n3\r\n").as_bytes());
        handle.join().unwrap();
        let reports: Vec<_> = output_rx.try_iter().collect();
        assert!(
            reports.iter().any(|output| matches!(
                output,
                VmOutput::LoginActionFailed {
                    index: 1,
                    status: 3
                }
            )),
            "{reports:?}"
        );
        let rendered = format!("{reports:?}");
        assert!(!rendered.contains(secret), "{rendered}");
    }
}
