//! Shell commands and files that login actions type into the guest.

use std::collections::HashMap;

use base64::{Engine, engine::general_purpose};

use crate::shares::DirectoryShare;
use crate::vm_user::VmUser;

use super::LoginAction;
use super::LoginAction::Send;

pub(super) const STEP_STATUS_MARKER_PREFIX: &str = "VIBE_STEP_STATUS_";

// #region scripts for the guest vm
pub(super) const BASHRC_SCRIPT: &str = include_str!("../bashrc.sh");
pub(super) const BASH_LOGOUT_SCRIPT: &str = include_str!("../bash_logout.sh");
// #endregion

/// Writes `src/bashrc.sh` to /root/.profile and sources it in the current shell.
/// The two actions always go together: a `.profile` the user may have changed is
/// never sourced again by Vibe.
pub(crate) fn write_bashrc_actions() -> Result<Vec<LoginAction>, Box<dyn std::error::Error>> {
    Ok(vec![
        Send(write_file_into_vm("/root/.profile", BASHRC_SCRIPT)?),
        Send(" . /root/.profile".to_string()),
    ])
}

/// Writes `src/bashrc.sh` to the target user's `~/.profile` (see
/// [`write_user_file_into_vm`]) without sourcing it; the user's login shell
/// reads it.
pub(crate) fn write_user_bashrc_action(
    user: &VmUser,
) -> Result<LoginAction, Box<dyn std::error::Error>> {
    Ok(Send(write_user_file_into_vm(
        ".profile",
        BASHRC_SCRIPT,
        user,
    )?))
}

/// Command typed into the root shell that writes `content` to `home_rel`
/// below the target user's HOME and makes it executable. For root this is
/// [`write_file_into_vm`] on `/root/<home_rel>`. For any other user the file
/// is written by that user through `runuser`, so it is owned by the user and
/// a symlink the user placed there cannot redirect a write made with root's
/// rights.
pub(crate) fn write_user_file_into_vm(
    home_rel: &str,
    content: &str,
    user: &VmUser,
) -> Result<String, Box<dyn std::error::Error>> {
    if user.is_root() {
        return write_file_into_vm(&format!("/root/{home_rel}"), content);
    }
    let b64 = general_purpose::STANDARD.encode(content);
    let path = crate::vm_user::GuestPath::new(home_rel).to_shell(user);
    Ok(format!(
        " echo '{b64}' | runuser -u \"$VIBE_VM_USER\" -- sh -c 'base64 --decode > \"$1\" && chmod +x \"$1\"' sh {path}"
    ))
}

pub(crate) fn shell_word(value: &str) -> String {
    let is_safe = |c: char| c.is_ascii_alphanumeric() || "-_./+,:@%=".contains(c);
    if !value.is_empty() && value.chars().all(is_safe) {
        value.to_string()
    } else {
        format!("'{}'", shell_single_quote(value))
    }
}

pub(crate) fn env_login_actions(env: &HashMap<String, String>) -> Vec<LoginAction> {
    env.iter()
        //leading space to keep env out of bash history; escape single quotes in value
        .map(|(name, value)| Send(format!(" export {name}='{}'", shell_single_quote(value))))
        .collect()
}

pub(crate) fn shell_single_quote(value: &str) -> String {
    value.replace('\'', r"'\''")
}

pub(crate) fn write_file_into_vm(
    vm_path: &str,
    file_content: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let b64 = general_purpose::STANDARD.encode(file_content);
    let quoted_path = shell_single_quote(vm_path);
    let command =
        format!(" echo '{b64}' | base64 --decode > '{quoted_path}'\nchmod +x '{quoted_path}'");
    Ok(command)
}

pub(crate) fn generate_http_proxy_env_export_cmds(proxy_url: &str, no_proxy: &str) -> String {
    let url = shell_single_quote(proxy_url);
    let no_proxy = shell_single_quote(no_proxy);
    format!(
        "export http_proxy='{url}'; export HTTP_PROXY='{url}'; export https_proxy='{url}'; export HTTPS_PROXY='{url}'; export no_proxy='{no_proxy}'; export NO_PROXY='{no_proxy}';"
    )
}

/// Variables of the root shell's mise activation; they would point the
/// user's own mise at root's state.
pub(super) const ROOT_MISE_VARIABLES: &str =
    "-u MISE_SHELL -u __MISE_DIFF -u __MISE_ORIG_PATH -u __MISE_SESSION";

/// Command and status marker of a [`LoginAction::Script`], or with
/// `as_vm_user` of a [`LoginAction::UserScript`].
///
/// The command is one shell `if` typed into the root shell:
/// 1. As root, print memory use and drop the page cache.
/// 2. Make (or reuse) the private script directory
///    `/tmp/vibe-scripts-<uid>` and write the base64-decoded script there.
/// 3. Run the script with stdin from /dev/null and keep its exit status.
///    A failed write sets the status to 1.
/// 4. After a success, activate mise again so later actions see new tools.
/// 5. Print an empty line, the marker line, and the status line.
///
/// Each uid gets its own directory with mode 700. Users cannot read each
/// other's scripts, and a directory that root made does not block another user.
///
/// The status lines look like this; `printf` joins the two marker halves:
///
/// ```text
/// VIBE_SCRIPT_STATUS_<id>
/// <exit status>
/// ```
pub(super) fn script_command(id: &str, script: &str, as_vm_user: bool) -> (String, String) {
    let b64 = general_purpose::STANDARD.encode(script);

    const STATUS_MARKER_PREFIX: &str = "VIBE_SCRIPT_STATUS_";
    let status_marker = format!("{STATUS_MARKER_PREFIX}{id}");

    // Run the script with stdin from /dev/null so long-running tools spawned by
    // the script cannot consume queued wrapper lines before the parent shell
    // sees them. Print a leading newline before the marker so terminal echo and
    // control sequences cannot merge the echoed printf command with the marker
    // line that wait_for_line_after expects. Reactivate mise after each
    // successful script so later scripts can use newly installed commands.
    // Only root can drop caches. Each uid gets its own private script
    // directory, so a directory created by root does not block another user
    // and users cannot read each other's scripts; an existing directory is
    // only reused when it is a real directory owned by the current user.
    //
    // For the vm-user, root creates the user's directory (or reuses it when
    // the user owns it) and the user writes the script into it, so a symlink
    // there cannot make root write elsewhere. A login shell reads the user's
    // profile and then execs the script: exiting a login shell would run
    // `~/.bash_logout`, whose last command replaces the script's status.
    let (prepare, write, run) = if as_vm_user {
        (
            "__vibe_script_dir=/tmp/vibe-scripts-$VIBE_VM_UID\n\
__vibe_script=\"$__vibe_script_dir/{id}.sh\"\n\
if { { mkdir -m 700 \"$__vibe_script_dir\" 2>/dev/null && chown \"$VIBE_VM_UID:$VIBE_VM_GID\" \"$__vibe_script_dir\"; } || { [ -d \"$__vibe_script_dir\" ] && [ ! -L \"$__vibe_script_dir\" ] && [ \"$(stat -c %u \"$__vibe_script_dir\")\" = \"$VIBE_VM_UID\" ]; }; }",
            "echo '{b64}' | runuser -u \"$VIBE_VM_USER\" -- sh -c 'base64 --decode >\"$1\" && chmod +x \"$1\"' sh \"$__vibe_script\"",
            format!(
                "(cd \"$VIBE_VM_HOME\" && runuser -u \"$VIBE_VM_USER\" -- env {ROOT_MISE_VARIABLES} HOME=\"$VIBE_VM_HOME\" USER=\"$VIBE_VM_USER\" LOGNAME=\"$VIBE_VM_USER\" SHELL=/bin/bash bash --login -c 'exec \"$0\"' \"$__vibe_script\") </dev/null"
            ),
        )
    } else {
        (
            "__vibe_script_dir=/tmp/vibe-scripts-$(id -u)\n\
__vibe_script=\"$__vibe_script_dir/{id}.sh\"\n\
if { mkdir -m 700 \"$__vibe_script_dir\" 2>/dev/null || { [ -d \"$__vibe_script_dir\" ] && [ ! -L \"$__vibe_script_dir\" ] && [ -O \"$__vibe_script_dir\" ]; }; }",
            "echo '{b64}' | base64 --decode >\"$__vibe_script\" && chmod +x \"$__vibe_script\"",
            "\"$__vibe_script\" </dev/null".to_string(),
        )
    };
    let prepare = prepare.replace("{id}", id);
    let write = write.replace("{b64}", &b64);
    let command = format!(
        " if [ \"$(id -u)\" = 0 ]; then free -h; echo 3 > /proc/sys/vm/drop_caches; echo 1 > /proc/sys/vm/compact_memory; fi\n\
{prepare} && {write}; then\n\
{run}\n\
status=$?\n\
else\n\
echo \"Vibe: cannot write the script into $__vibe_script_dir\" >&2\n\
status=1\n\
fi\n\
if [ \"$status\" -eq 0 ] && [ -x \"$HOME/.local/bin/mise\" ]; then\n\
eval \"$(\"$HOME/.local/bin/mise\" activate bash)\"\n\
fi\n\
printf '\\n%s%s\\n%s\\n' '{STATUS_MARKER_PREFIX}' '{id}' \"$status\""
    );

    (command, status_marker)
}

/// Builds the command of a [`LoginAction::StatusCommand`] and the marker that
/// precedes its status line. The status line is `<exit status> <notice>`,
/// where the notice is empty unless the command set `__vibe_notice`. The
/// marker is split across two `printf` arguments so the echoed command never
/// contains it. The command runs in a group with stdin from /dev/null, so
/// tools such as apt-get cannot consume the status line typed after it; the
/// group still runs in the current shell and keeps `__vibe_notice`.
pub(super) fn status_command_and_marker(id: &str, command: &str) -> (String, String) {
    let marker = format!("{STEP_STATUS_MARKER_PREFIX}{id}");
    let command = format!(
        " {{ {command}\n}} </dev/null\n __vibe_status=$?; printf '\\n%s%s\\n%s %s\\n' '{STEP_STATUS_MARKER_PREFIX}' '{id}' \"$__vibe_status\" \"${{__vibe_notice:-}}\"; unset __vibe_status __vibe_notice"
    );
    (command, marker)
}

/// Splits a status line from [`status_command_and_marker`] into the exit
/// status (`u8::MAX` when it is not a number) and the optional notice word.
pub(super) fn parse_status_line(line: &str) -> (u8, Option<String>) {
    let mut words = line.split_whitespace();
    let status = words
        .next()
        .and_then(|word| word.parse().ok())
        .unwrap_or(u8::MAX);
    (status, words.next().map(str::to_string))
}

pub(crate) fn motd_login_action(
    directory_shares: &[DirectoryShare],
    vm_user: &VmUser,
) -> Option<LoginAction> {
    if directory_shares.is_empty() {
        return Some(Send(" clear".into()));
    }

    let host_header = "Host";
    let guest_header = "Guest";
    let mode_header = "Mode";
    let mut host_width = host_header.len();
    let mut guest_width = guest_header.len();
    let mut mode_width = mode_header.len();
    let mut rows = Vec::with_capacity(directory_shares.len());

    for share in directory_shares {
        let host = share.host.to_string_lossy().into_owned();
        let guest = share.guest.display(vm_user);
        let mode = if share.read_only {
            "read-only"
        } else {
            "read-write"
        }
        .to_string();
        host_width = host_width.max(host.len());
        guest_width = guest_width.max(guest.len());
        mode_width = mode_width.max(mode.len());
        rows.push((host, guest, mode));
    }

    let mut output = String::new();
    output.push_str(
        "
░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░▒▓███████▓▒░░▒▓████████▓▒░ 
░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░        
 ░▒▓█▓▒▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░        
 ░▒▓█▓▒▒▓█▓▒░░▒▓█▓▒░▒▓███████▓▒░░▒▓██████▓▒░   
  ░▒▓█▓▓█▓▒░ ░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░        
  ░▒▓█▓▓█▓▒░ ░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░        
   ░▒▓██▓▒░  ░▒▓█▓▒░▒▓███████▓▒░░▒▓████████▓▒░

",
    );
    output.push_str(&format!(
        "{host_header:<host_width$}  {guest_header:<guest_width$}  {mode_header}\n"
    ));
    output.push_str(&format!(
        "{:-<host_width$}  {:-<guest_width$}  {:-<mode_width$}\n",
        "",
        "",
        "",
        host_width = host_width,
        guest_width = guest_width,
        mode_width = mode_width
    ));

    for (host, guest, mode) in rows {
        output.push_str(&format!(
            "{host:<host_width$}  {guest:<guest_width$}  {mode}\n"
        ));
    }

    let command = format!(" clear && cat <<'VIBE_MOTD'\n{output}\nVIBE_MOTD");
    Some(Send(command))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::legacy_login_prefix;
    use crate::session::test_support::*;
    use crate::vm_io::{OutputMonitor, WaitResult};

    fn sent(actions: &[LoginAction]) -> Vec<&str> {
        actions
            .iter()
            .filter_map(|action| match action {
                Send(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn only_write_bashrc_actions_touch_the_profile() {
        let prefix = legacy_login_prefix(LONG, None).unwrap();
        let prefix = sent(&prefix);
        assert!(
            prefix
                .iter()
                .any(|text| text.contains("'/root/.bash_logout'"))
        );
        assert!(!prefix.iter().any(|text| text.contains("/root/.profile")));

        let bashrc = write_bashrc_actions().unwrap();
        let bashrc = sent(&bashrc);
        assert_eq!(bashrc.len(), 2);
        assert!(bashrc[0].contains("> '/root/.profile'"));
        assert_eq!(bashrc[1], " . /root/.profile");
    }

    #[test]
    fn user_files_are_written_by_the_user_and_root_files_by_root() {
        let dev = VmUser::parse("dev").unwrap();
        assert_eq!(
            write_user_file_into_vm(".tmux.conf", "x", &VmUser::root()).unwrap(),
            write_file_into_vm("/root/.tmux.conf", "x").unwrap()
        );
        assert_eq!(
            write_user_file_into_vm(".tmux.conf", "x", &dev).unwrap(),
            " echo 'eA==' | runuser -u \"$VIBE_VM_USER\" -- sh -c 'base64 --decode > \"$1\" && chmod +x \"$1\"' sh \"${VIBE_VM_HOME}\"/.tmux.conf"
        );
        let Send(profile) = write_user_bashrc_action(&dev).unwrap() else {
            panic!("expected a Send action");
        };
        assert!(profile.ends_with(" sh \"${VIBE_VM_HOME}\"/.profile"));
    }

    #[test]
    fn status_command_reports_status_and_notice_in_bash() {
        let run = |command: &str| {
            let (script, marker) = status_command_and_marker("t", command);
            // Typed into the shell's stdin like the guest console.
            let mut child = std::process::Command::new("bash")
                .arg("-s")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            {
                use std::io::Write;
                let mut stdin = child.stdin.take().unwrap();
                stdin.write_all(format!("{script}\n").as_bytes()).unwrap();
            }
            let output = child.wait_with_output().unwrap();
            let stdout = String::from_utf8(output.stdout).unwrap();
            let monitor = OutputMonitor::default();
            monitor.push(stdout.as_bytes());
            match monitor.wait_for_line_after(&marker, SHORT) {
                WaitResult::Found(line) => parse_status_line(&line),
                other => panic!("no status line in {stdout:?}: {other:?}"),
            }
        };
        assert_eq!(run("true"), (0, None));
        assert_eq!(run("(exit 12)"), (12, None));
        // A command that reads stdin cannot swallow the status line.
        assert_eq!(run("cat >/dev/null"), (0, None));
        assert_eq!(
            run("__vibe_notice=$(echo groups=adm,sudo)"),
            (0, Some("groups=adm,sudo".to_string()))
        );
    }

    #[test]
    fn script_wrapper_uses_a_private_directory_per_uid() {
        let (command, _) = script_command("script_0", "true\n", false);
        assert!(command.contains("/tmp/vibe-scripts-$(id -u)"), "{command}");
        assert!(command.contains("mkdir -m 700"), "{command}");
        assert!(
            command.starts_with(
                " if [ \"$(id -u)\" = 0 ]; then free -h; echo 3 > /proc/sys/vm/drop_caches"
            ),
            "{command}"
        );
    }

    #[test]
    fn user_script_wrapper_runs_the_script_as_the_vm_user() {
        let (command, marker) = script_command("script_4", "true\n", true);
        assert_eq!(marker, "VIBE_SCRIPT_STATUS_script_4");
        assert!(
            command.contains("__vibe_script_dir=/tmp/vibe-scripts-$VIBE_VM_UID\n"),
            "{command}"
        );
        assert!(
            command.contains("chown \"$VIBE_VM_UID:$VIBE_VM_GID\" \"$__vibe_script_dir\""),
            "{command}"
        );
        assert!(
            command.contains("[ \"$(stat -c %u \"$__vibe_script_dir\")\" = \"$VIBE_VM_UID\" ]"),
            "{command}"
        );
        // The user writes the file, not root.
        assert!(
            command.contains("| runuser -u \"$VIBE_VM_USER\" -- sh -c 'base64 --decode"),
            "{command}"
        );
        assert!(
            command.contains(
                "(cd \"$VIBE_VM_HOME\" && runuser -u \"$VIBE_VM_USER\" -- env -u MISE_SHELL"
            ),
            "{command}"
        );
        assert!(
            command.contains(
                "SHELL=/bin/bash bash --login -c 'exec \"$0\"' \"$__vibe_script\") </dev/null\n"
            ),
            "{command}"
        );
    }
}
