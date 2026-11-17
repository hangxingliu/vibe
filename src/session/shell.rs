//! Shell commands and files that login actions type into the guest.

use std::collections::HashMap;

use base64::{Engine, engine::general_purpose};

use crate::guest_template::{replace_region, shell_assign};
use crate::session_dir::GuestFile;
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

const SCRIPT_WRAPPER: &str = include_str!("../guest/script_wrapper.sh");

/// Command and status marker of a [`LoginAction::Script`], or with
/// `as_vm_user` of a [`LoginAction::UserScript`].
///
/// The command is one `eval` of [`SCRIPT_WRAPPER`], base64-encoded so the
/// console shows that line and not the wrapper. A leading space keeps it out
/// of shell history. When `file` is set, the wrapper reads that path.
/// Otherwise it decodes `script`. The wrapper copies the script into
/// `/tmp/vibe-scripts-<uid>`, runs it with stdin from /dev/null, reactivates
/// mise after success, and prints:
///
/// ```text
/// VIBE_SCRIPT_STATUS_<id>
/// <exit status>
/// ```
///
/// The marker is not part of the typed command. The host waits for the line
/// the wrapper prints.
pub(super) fn script_command(
    id: &str,
    script: &str,
    file: Option<&GuestFile>,
    as_vm_user: bool,
) -> (String, String) {
    let src = file.map(GuestFile::as_str).unwrap_or("");
    let b64 = if file.is_none() {
        general_purpose::STANDARD.encode(script)
    } else {
        String::new()
    };
    // The directory default lives in the replaced region, so the render keeps it.
    let body = format!(
        "{}\n{}\n{}\n{}\n{}\n",
        shell_assign("__vibe_script_id", id),
        shell_assign("__vibe_script_src", src),
        shell_assign("__vibe_script_b64", &b64),
        shell_assign("__vibe_as_vm_user", if as_vm_user { "1" } else { "0" }),
        shell_assign("__vibe_script_dir_base", "/tmp/vibe-scripts"),
    );
    let rendered = replace_region(SCRIPT_WRAPPER, "INJECT_SCRIPT_WRAPPER_CONFIG", &body)
        .expect("script_wrapper.sh has region INJECT_SCRIPT_WRAPPER_CONFIG");
    let encoded = general_purpose::STANDARD.encode(rendered);
    let command = format!(" eval \"$(echo '{encoded}' | base64 --decode)\"");
    (command, format!("VIBE_SCRIPT_STATUS_{id}"))
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
    // A staging-only share is internal. The guest table lists the same shares
    // as the host file log.
    let shares: Vec<&DirectoryShare> = directory_shares
        .iter()
        .filter(|share| !share.staging_only)
        .collect();
    if shares.is_empty() {
        return Some(Send(" clear".into()));
    }

    let host_header = "Host";
    let guest_header = "Guest";
    let mode_header = "Mode";
    let mut host_width = host_header.len();
    let mut guest_width = guest_header.len();
    let mut mode_width = mode_header.len();
    let mut rows = Vec::with_capacity(shares.len());

    for share in shares {
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
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn motd_omits_a_staging_only_share() {
        let visible = DirectoryShare {
            host: std::path::PathBuf::from("/cache/apt"),
            guest: crate::vm_user::GuestPath::new("/var/cache/apt"),
            read_only: false,
            is_project_dir: false,
            staging_only: false,
        };
        let hidden = DirectoryShare {
            host: std::path::PathBuf::from("/cache/sessions/1"),
            guest: crate::vm_user::GuestPath::new("/mnt/root/virtiofs-shared"),
            read_only: true,
            is_project_dir: false,
            staging_only: true,
        };
        let Send(command) = motd_login_action(&[visible, hidden.clone()], &VmUser::root()).unwrap()
        else {
            panic!("expected a Send action");
        };
        assert!(command.contains("/cache/apt"), "{command}");
        assert!(!command.contains("/cache/sessions"), "{command}");
        let Send(only) = motd_login_action(&[hidden], &VmUser::root()).unwrap() else {
            panic!("expected a Send action");
        };
        assert_eq!(only, " clear");
    }

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

    fn decode_eval_line(command: &str) -> String {
        let marker = "echo '";
        let start = command.find(marker).expect(command) + marker.len();
        let end = command[start..].find("' | base64").expect(command) + start;
        let bytes = general_purpose::STANDARD
            .decode(&command[start..end])
            .expect("eval payload is base64");
        String::from_utf8(bytes).expect("wrapper is utf-8")
    }

    #[test]
    fn script_command_types_one_eval_line() {
        let script = "#!/bin/sh\nexit 4\n";
        let (command, marker) = script_command("script_0", script, None, false);
        assert_eq!(marker, "VIBE_SCRIPT_STATUS_script_0");
        assert_eq!(command.lines().count(), 1, "{command}");
        assert!(command.starts_with(' '), "{command}");
        assert!(!command.contains("VIBE_SCRIPT_STATUS_"), "{command}");
        assert!(!command.contains("exit 4"), "{command}");
        let rendered = decode_eval_line(&command);
        let b64 = general_purpose::STANDARD.encode(script);
        assert!(
            rendered.contains(&format!("__vibe_script_b64='{b64}'")),
            "{rendered}"
        );
        assert!(rendered.contains("__vibe_script_src=''"), "{rendered}");
        assert!(rendered.contains("__vibe_as_vm_user='0'"), "{rendered}");

        let private = std::path::Path::new("/tmp")
            .join(format!("vibe-scripts-{}", unsafe { libc::getuid() }));
        let _ = std::fs::remove_dir_all(&private);
        let run = eval_wrapper(&command);
        assert!(
            run.stdout.contains("VIBE_SCRIPT_STATUS_script_0\n4\n"),
            "{}",
            run.stdout
        );
        assert_clean_shell(&run);
        let _ = std::fs::remove_dir_all(&private);

        let cache = scratch_wrapper();
        let mut session = crate::session_dir::SessionDir::create(&cache).unwrap();
        let file = session.write("scripts/000-demo.sh", script).unwrap();
        let (command, marker) = script_command("script_2", script, Some(&file), true);
        assert_eq!(marker, "VIBE_SCRIPT_STATUS_script_2");
        assert_eq!(command.lines().count(), 1, "{command}");
        assert!(command.starts_with(' '), "{command}");
        assert!(!command.contains("VIBE_SCRIPT_STATUS_"), "{command}");
        assert!(!command.contains("exit 4"), "{command}");
        let rendered = decode_eval_line(&command);
        assert!(
            rendered.contains(&format!("__vibe_script_src='{}'", file.as_str())),
            "{rendered}"
        );
        assert!(rendered.contains("__vibe_script_b64=''"), "{rendered}");
        assert!(rendered.contains("__vibe_as_vm_user='1'"), "{rendered}");
        session.finish(true).unwrap();
        let _ = std::fs::remove_dir_all(&cache);
    }

    const SCRIPT_WRAPPER: &str = super::SCRIPT_WRAPPER;

    fn render_wrapper(id: &str, src: &str, b64: &str, dir_base: &str) -> String {
        use crate::guest_template::{replace_region, shell_assign};
        let body = format!(
            "{}\n{}\n{}\n{}\n{}\n",
            shell_assign("__vibe_script_id", id),
            shell_assign("__vibe_script_src", src),
            shell_assign("__vibe_script_b64", b64),
            shell_assign("__vibe_as_vm_user", "0"),
            shell_assign("__vibe_script_dir_base", dir_base),
        );
        replace_region(SCRIPT_WRAPPER, "INJECT_SCRIPT_WRAPPER_CONFIG", &body)
            .expect("script_wrapper.sh has region INJECT_SCRIPT_WRAPPER_CONFIG")
    }

    struct WrapperRun {
        stdout: String,
        stderr: String,
    }

    fn eval_wrapper(script: &str) -> WrapperRun {
        let mut child = std::process::Command::new("bash")
            .arg("-c")
            .arg(
                "eval \"$(cat)\"\n\
                 echo alive\n\
                 printf 'VARS=%s\\n' \"$(compgen -v __vibe_ || true)\"\n\
                 printf 'FUNCS=%s\\n' \"$(declare -F __vibe_script_text || true)\"\n",
            )
            .env("HOME", "/no-such-mise-home")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            output.status.success(),
            "status {:?}\nstdout {stdout}\nstderr {stderr}",
            output.status
        );
        WrapperRun { stdout, stderr }
    }

    fn assert_clean_shell(run: &WrapperRun) {
        assert!(run.stdout.contains("\nalive\n"), "{}", run.stdout);
        assert!(run.stdout.contains("VARS=\n"), "{}", run.stdout);
        assert!(run.stdout.contains("FUNCS=\n"), "{}", run.stdout);
        assert!(!run.stdout.contains("VARS=__vibe"), "{}", run.stdout);
    }

    fn script_dir(base: &std::path::Path) -> std::path::PathBuf {
        let uid = unsafe { libc::getuid() };
        base.join(format!("vibe-scripts-{uid}"))
    }

    fn scratch_wrapper() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "vibe-wrapper-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn script_wrapper_root_branch_runs_a_file_and_reports_its_status() {
        let dir = scratch_wrapper();
        let source = dir.join("exit3.sh");
        std::fs::write(&source, "#!/bin/sh\nexit 3\n").unwrap();
        let base = dir.join("vibe-scripts");
        let script = render_wrapper(
            "script_0",
            &source.to_string_lossy(),
            "",
            &base.to_string_lossy(),
        );
        let run = eval_wrapper(&script);
        let marker = "VIBE_SCRIPT_STATUS_script_0\n";
        let at = run.stdout.find(marker).expect(&run.stdout);
        assert!(
            run.stdout[at + marker.len()..].starts_with("3\n"),
            "{}",
            run.stdout
        );
        assert_clean_shell(&run);
        let created = script_dir(&dir);
        let mode = std::fs::metadata(&created).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn script_wrapper_root_branch_runs_inline_base64() {
        let dir = scratch_wrapper();
        let b64 = general_purpose::STANDARD.encode("#!/bin/sh\nexit 3\n");
        let base = dir.join("vibe-scripts");
        let script = render_wrapper("script_1", "", &b64, &base.to_string_lossy());
        let run = eval_wrapper(&script);
        let marker = "VIBE_SCRIPT_STATUS_script_1\n";
        let at = run.stdout.find(marker).expect(&run.stdout);
        assert!(
            run.stdout[at + marker.len()..].starts_with("3\n"),
            "{}",
            run.stdout
        );
        assert_clean_shell(&run);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn script_wrapper_reports_a_missing_source_file() {
        let dir = scratch_wrapper();
        let missing = dir.join("missing.sh");
        let base = dir.join("vibe-scripts");
        let script = render_wrapper(
            "script_2",
            &missing.to_string_lossy(),
            "",
            &base.to_string_lossy(),
        );
        let run = eval_wrapper(&script);
        assert!(
            run.stdout.contains("VIBE_SCRIPT_STATUS_script_2\n1\n"),
            "{}",
            run.stdout
        );
        assert!(
            run.stderr.contains("cannot write the script"),
            "{}",
            run.stderr
        );
        assert_clean_shell(&run);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn script_wrapper_refuses_a_symlinked_script_directory() {
        let dir = scratch_wrapper();
        let base = dir.join("vibe-scripts");
        let link = script_dir(&dir);
        std::os::unix::fs::symlink("/tmp", &link).unwrap();
        let source = dir.join("exit3.sh");
        std::fs::write(&source, "#!/bin/sh\nexit 3\n").unwrap();
        let script = render_wrapper(
            "script_3",
            &source.to_string_lossy(),
            "",
            &base.to_string_lossy(),
        );
        let run = eval_wrapper(&script);
        assert!(
            run.stdout.contains("VIBE_SCRIPT_STATUS_script_3\n1\n"),
            "{}",
            run.stdout
        );
        assert!(
            run.stderr.contains("cannot write the script"),
            "{}",
            run.stderr
        );
        assert_clean_shell(&run);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
