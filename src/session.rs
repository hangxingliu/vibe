//! Login actions typed into the guest console after root login, and the
//! helpers that build the shell commands for them.

use std::{sync::Arc, time::Duration};

use crate::vm_user::VmUser;

mod actions;
mod failure;
mod login;
mod password;
mod shell;
#[cfg(test)]
mod test_support;

pub(crate) use actions::{VmOutput, spawn_login_actions_thread};
pub(crate) use failure::{DiskSource, LoginFailure, needs_provision_hint};
pub(crate) use password::{RootPassword, set_root_password_actions};
use shell::BASH_LOGOUT_SCRIPT;
pub(crate) use shell::{
    env_login_actions, generate_http_proxy_env_export_cmds, motd_login_action, shell_single_quote,
    shell_word, write_bashrc_actions, write_file_into_vm, write_user_bashrc_action,
    write_user_file_into_vm,
};

pub(crate) const DEFAULT_EXPECT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub(crate) enum LoginAction {
    Expect {
        text: String,
        timeout: Duration,
    },
    Send(String),
    Script {
        name: String,
        content: String,
    },
    /// Like [`LoginAction::Script`], but typed into the root shell and run as
    /// the `--vm-user` (`VIBE_VM_*` must be set) with `runuser` and a login
    /// bash in the user's HOME. Variables exported in the root shell are
    /// passed on.
    UserScript {
        name: String,
        content: String,
    },
    /// Logs in as root on the serial console and confirms with a readiness
    /// probe that a root shell is accepting input. Each wait is bounded by
    /// `timeout`. When `password` is set it is typed at a `Password:` prompt
    /// and is never included in logs or in [`LoginAction::label`].
    /// Only used by `legacy_login_prefix`.
    LegacyLogin {
        timeout: Duration,
        password: Option<Arc<RootPassword>>,
    },
    /// Waits until `marker` (echo is off), types `password`, then reads the
    /// status line that follows `done_marker`. The label never includes the
    /// password. Only used by `set_root_password_actions`.
    SecretSend {
        marker: String,
        done_marker: String,
        password: Arc<RootPassword>,
        timeout: Duration,
    },
    /// Runs a one-line `command` in the current shell and waits for its exit
    /// status. A non-zero status stops the session. The command may set
    /// `__vibe_notice` to one word, which the host reports as
    /// [`VmOutput::Notice`]. `label` is used in error prefixes; when
    /// `vm_user` is set, exit codes of the vm-user check are reported as
    /// [`LoginFailure::VmUser`].
    StatusCommand {
        label: String,
        command: String,
        timeout: Duration,
        vm_user: Option<VmUser>,
    },
    /// Starts a login shell of `user` with `su --login --pty` from the root
    /// shell and confirms with a probe that the new shell runs as `user`
    /// (and not as root). Exiting that shell returns to the root shell.
    /// A failure is reported as [`LoginFailure::SwitchFailed`].
    SwitchUser {
        user: VmUser,
        timeout: Duration,
    },
}

pub(crate) use LoginAction::*;

impl LoginAction {
    /// Short description used in error prefixes. `Send` text is not included
    /// because it may contain proxy URLs or other environment values.
    pub(crate) fn label(&self) -> String {
        match self {
            Expect { text, .. } => format!("expect '{text}'"),
            Send(_) => "send".to_string(),
            Script { name, .. } => format!("script '{name}'"),
            UserScript { name, .. } => format!("script '{name}' as vm-user"),
            LegacyLogin { .. } => "login".to_string(),
            SecretSend { .. } => "set root password".to_string(),
            StatusCommand { label, .. } => label.clone(),
            SwitchUser { .. } => "switch to vm-user".to_string(),
        }
    }
}

/// Error prefix for the login action at `index` in the full action list.
/// Actions in `user` (a sub-range of `caller`) are numbered from 1 in the
/// order the user gave them. Vibe's own actions after the user's ones (the
/// provision finalize and poweroff steps) are part of the shutdown; anything
/// else is part of the internal login and setup.
pub(crate) fn action_prefix(
    index: usize,
    caller: &std::ops::Range<usize>,
    user: &std::ops::Range<usize>,
    action: &LoginAction,
) -> String {
    if user.contains(&index) {
        format!("[action {}: {}]", index - user.start + 1, action.label())
    } else if caller.contains(&index) && index >= user.end {
        format!("[shutdown] {}", action.label())
    } else {
        format!("[login] setup {}", action.label())
    }
}

/// Actions that every VM boot starts with: the legacy root login with its
/// readiness probe, followed by the internal shell setup. It writes
/// `/root/.bash_logout` but never writes or sources `/root/.profile`; see
/// `write_bashrc_actions`.
pub(crate) fn legacy_login_prefix(
    timeout: Duration,
    password: Option<Arc<RootPassword>>,
) -> Result<Vec<LoginAction>, Box<dyn std::error::Error>> {
    Ok(vec![
        LegacyLogin { timeout, password },
        // Temporarily disable bash history and set commands starting with space to be ignored
        Send(" export HISTCONTROL=ignorespace".to_string()),
        Send(" unset HISTFILE".to_string()),
        // Our terminal is connected via /dev/hvc0 which Debian apparently keeps barebones.
        // We want sane terminal defaults like icrnl (translating carriage returns into newlines)
        Send(" stty -F /dev/hvc0 sane".to_string()),
        // In background, continuously read host terminal resizes sent over hvc1 and update hvc0.
        Send({
            // sorry for this nonsense, the string is so long it angers rustfmt =(
            const S: &str = " sh -c '(while IFS=\" \" read -r rows cols; do stty -F /dev/hvc0 rows \"$rows\" cols \"$cols\"; done) < /dev/hvc1 >/dev/null 2>&1 &'";
            S.to_string()
        }),
        Send(write_file_into_vm(
            "/root/.bash_logout",
            BASH_LOGOUT_SCRIPT,
        )?),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::*;

    #[test]
    fn action_prefix_numbers_user_actions_from_one() {
        let script = Script {
            name: "base".to_string(),
            content: String::new(),
        };
        // Caller actions 7..12: two Vibe setup actions, three user actions,
        // then one Vibe shutdown action.
        let caller = 7..13;
        let user = 9..12;
        assert_eq!(
            action_prefix(10, &caller, &user, &script),
            "[action 2: script 'base']"
        );
        let expect = Expect {
            text: "VIBE_PROVISION_SUCCESS".to_string(),
            timeout: LONG,
        };
        assert_eq!(
            action_prefix(11, &caller, &user, &expect),
            "[action 3: expect 'VIBE_PROVISION_SUCCESS']"
        );
        assert_eq!(
            action_prefix(8, &caller, &user, &Send(" cd project".to_string())),
            "[login] setup send"
        );
        let finalize = Script {
            name: "finalize-machine-id".to_string(),
            content: String::new(),
        };
        assert_eq!(
            action_prefix(12, &caller, &user, &finalize),
            "[shutdown] script 'finalize-machine-id'"
        );
        assert_eq!(
            action_prefix(
                2,
                &caller,
                &user,
                &Send("export HTTP_PROXY=http://secret".to_string())
            ),
            "[login] setup send"
        );
        assert!(
            !Send("export X=secret".to_string())
                .label()
                .contains("secret")
        );
    }

    #[test]
    fn empty_user_range_position_decides_setup_or_shutdown() {
        // `provision --repl` on an existing image passes only Vibe's `.profile`
        // actions (caller actions 7..9) and no user actions.
        let caller = 7..9;
        let profile = Send(" . /root/.profile".to_string());
        assert_eq!(
            action_prefix(8, &caller, &(9..9), &profile),
            "[login] setup send"
        );
        assert_eq!(
            action_prefix(8, &caller, &(7..7), &profile),
            "[shutdown] send"
        );
    }
}
