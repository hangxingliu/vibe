//! Login actions typed into the guest console after root login, and the
//! helpers that build the shell commands for them.

use std::{
    collections::HashMap,
    hash::{BuildHasher, Hasher},
    path::Path,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose};

use crate::DirectoryShare;
use crate::vm_io::{OutputMonitor, VmInput, WaitResult};

pub(crate) const DEFAULT_EXPECT_TIMEOUT: Duration = Duration::from_secs(30);
const SCRIPT_ACTION_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
const LOGIN_ACTION_INPUT_CHUNK_BYTES: usize = 256;
const LOGIN_ACTION_INPUT_CHUNK_DELAY: Duration = Duration::from_millis(10);

// #region scripts for the guest vm
const BASHRC_SCRIPT: &str = include_str!("bashrc.sh");
const BASH_LOGOUT_SCRIPT: &str = include_str!("bash_logout.sh");
// #endregion

const READY_PROBE_MARKER_PREFIX: &str = "VIBE_READY_";

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
    /// Logs in as root on a legacy console (`login: ` prompt, no password) and
    /// confirms with a readiness probe that a root shell is accepting input.
    /// Each wait is bounded by `timeout`. Only used by `legacy_login_prefix`.
    LegacyLogin {
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
            LegacyLogin { .. } => "login".to_string(),
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
/// readiness probe, followed by the internal shell setup.
pub(crate) fn legacy_login_prefix(
    timeout: Duration,
) -> Result<Vec<LoginAction>, Box<dyn std::error::Error>> {
    Ok(vec![
        LegacyLogin { timeout },
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
        //
        Send(write_file_into_vm("/root/.profile", BASHRC_SCRIPT)?),
        Send(write_file_into_vm(
            "/root/.bash_logout",
            BASH_LOGOUT_SCRIPT,
        )?),
    ])
}

/// Returns a random hex string that is unique enough to tell this probe apart
/// from any other console output.
fn probe_nonce() -> String {
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
fn readiness_probe_command_and_marker(nonce: &str) -> (String, String) {
    let marker = format!("{READY_PROBE_MARKER_PREFIX}{nonce}");
    let command =
        format!(" printf '\\n%s%s\\n%s\\n' '{READY_PROBE_MARKER_PREFIX}' '{nonce}' \"$(id -u)\"");
    (command, marker)
}

/// Why the legacy root login did not reach a usable shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoginFailure {
    /// Nothing at all arrived on hvc0 while waiting for `login: `.
    NoSerialOutput { timeout: Duration },
    /// hvc0 produced output, but never a `login: ` prompt.
    NoLoginPrompt { timeout: Duration },
    /// The guest asked for a root password.
    PasswordRequired,
    /// Neither a password prompt nor the Debian banner followed `root`.
    NoBanner { timeout: Duration },
    /// The shell never answered the readiness probe.
    ProbeTimeout { timeout: Duration },
    /// The readiness probe answered, but the shell is not running as root.
    ProbeNotRoot { uid: String },
    /// The VM stopped before the login completed.
    VmStopped,
}

impl LoginFailure {
    pub(crate) fn describe(&self, disk: &Path) -> String {
        let disk = disk.display();
        match self {
            LoginFailure::NoSerialOutput { timeout } => format!(
                "[boot] No output on the serial console (hvc0) within {timeout:?} (disk: {disk}). The guest may be stuck in firstboot or failed to boot."
            ),
            LoginFailure::NoLoginPrompt { timeout } => format!(
                "[boot] The serial console (hvc0) produced output but no 'login: ' prompt within {timeout:?} (disk: {disk})."
            ),
            LoginFailure::PasswordRequired => format!(
                "[login] The image asks for a root password (disk: {disk}); it is not a legacy image that Vibe can manage. Newer Debian images have to be prepared by the default provision flow."
            ),
            LoginFailure::NoBanner { timeout } => format!(
                "[login] Sent 'root' but saw neither a password prompt nor the Debian banner within {timeout:?} (disk: {disk})."
            ),
            LoginFailure::ProbeTimeout { timeout } => format!(
                "[login] The root shell did not answer the readiness probe within {timeout:?} (disk: {disk})."
            ),
            LoginFailure::ProbeNotRoot { uid } => format!(
                "[login] The readiness probe reported uid '{uid}' instead of root (disk: {disk})."
            ),
            LoginFailure::VmStopped => {
                format!("[boot] The VM stopped before the root login completed (disk: {disk}).")
            }
        }
    }

    /// Whether the failure may be caused by booting an image that Vibe has not
    /// prepared, so the error should suggest `vibe provision`.
    pub(crate) fn suggests_provision(&self) -> bool {
        matches!(
            self,
            LoginFailure::NoSerialOutput { .. }
                | LoginFailure::NoLoginPrompt { .. }
                | LoginFailure::PasswordRequired
                | LoginFailure::VmStopped
        )
    }
}

/// Where the disk passed to `run_vm` came from; used to suggest the right
/// provision command when the boot fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DiskSource {
    /// A disk path given on the command line.
    External,
    /// The project instance disk in `.vibe/`.
    Instance,
    /// A named image from an image directory, booted directly.
    CachedImage(String),
}

fn shell_word(value: &str) -> String {
    let is_safe = |c: char| c.is_ascii_alphanumeric() || "-_./+,:@%=".contains(c);
    if !value.is_empty() && value.chars().all(is_safe) {
        value.to_string()
    } else {
        format!("'{}'", shell_single_quote(value))
    }
}

/// Explains that the disk may need to be prepared by `vibe provision` and gives
/// a command for it. Vibe never inspects or modifies disks on the run path, so
/// the wording stays tentative. `<name>` is left for the user to choose.
pub(crate) fn needs_provision_hint(disk: &Path, source: &DiskSource) -> String {
    let absolute_disk = std::path::absolute(disk).unwrap_or_else(|_| disk.to_path_buf());
    let base = shell_word(&absolute_disk.to_string_lossy());
    let intro = "This may be a newer Debian image that has not been prepared by Vibe. \
Vibe does not inspect or modify disks at run time; use provision to create a prepared copy first. \
The original disk is not modified.";
    let steps = match source {
        DiskSource::External => format!(
            "  vibe provision --base {base} --image <name>\n\
Then run `vibe --image <name>` in a project, or boot <name>.raw from one of the Vibe image directories (see `vibe --help`)."
        ),
        DiskSource::Instance => format!(
            "  vibe provision --base {base} --image <name>\n\
Then move {base} away (or delete it) and run `vibe --image <name>` to create a new instance from the prepared image."
        ),
        DiskSource::CachedImage(name) => {
            let name = shell_word(name);
            format!(
                "  vibe provision --base {name} --image <new-name>\n\
Or regenerate the image from the default base with `vibe provision --image {name} --replace`."
            )
        }
    };
    format!("{intro}\n{steps}")
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

pub(crate) fn generate_http_proxy_env_export_cmds(proxy_url: &str) -> String {
    let url = shell_single_quote(proxy_url);
    format!(
        "export http_proxy='{url}'; export HTTP_PROXY='{url}'; export https_proxy='{url}'; export HTTPS_PROXY='{url}'; export no_proxy='127.0.0.1,localhost,192.168.*'; export NO_PROXY='127.0.0.1,localhost,192.168.*';"
    )
}

fn script_command_and_status_marker(id: &str, script: &str) -> (String, String) {
    let b64 = general_purpose::STANDARD.encode(script);
    let guest_dir = "/tmp/vibe-scripts";
    let guest_path = format!("{guest_dir}/{id}.sh");

    const STATUS_MARKER_PREFIX: &str = "VIBE_SCRIPT_STATUS_";
    let status_marker = format!("{STATUS_MARKER_PREFIX}{id}");

    // Run the script with stdin from /dev/null so long-running tools spawned by
    // the script cannot consume queued wrapper lines before the parent shell
    // sees them. Print a leading newline before the marker so terminal echo and
    // control sequences cannot merge the echoed printf command with the marker
    // line that wait_for_line_after expects. Reactivate mise after each
    // successful script so later scripts can use newly installed commands.
    let command = format!(
        " free -h; echo 3 > /proc/sys/vm/drop_caches; echo 1 > /proc/sys/vm/compact_memory;\n\
mkdir -p {guest_dir}\n\
echo '{b64}' | base64 --decode >{guest_path}\n\
chmod +x {guest_path}\n\
{guest_path} </dev/null\n\
status=$?\n\
if [ \"$status\" -eq 0 ] && [ -x \"$HOME/.local/bin/mise\" ]; then\n\
eval \"$(\"$HOME/.local/bin/mise\" activate bash)\"\n\
fi\n\
printf '\\n%s%s\\n%s\\n' '{STATUS_MARKER_PREFIX}' '{id}' \"$status\""
    );

    (command, status_marker)
}

pub(crate) fn motd_login_action(directory_shares: &[DirectoryShare]) -> Option<LoginAction> {
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
        let guest = share.guest.to_string_lossy().into_owned();
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
    LoginReady,
    /// Every login action ran to completion.
    ActionsCompleted,
}

/// Outcome of one step of the actions thread.
enum Step {
    Continue,
    /// Stop running actions: the VM stopped, its input closed, or a failure
    /// was already reported.
    Stop,
}

fn send_text(input_tx: &mpsc::Sender<VmInput>, mut text: String) -> Step {
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

fn legacy_login(
    timeout: Duration,
    output_monitor: &OutputMonitor,
    input_tx: &mpsc::Sender<VmInput>,
) -> Result<(), Option<LoginFailure>> {
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
    match output_monitor.wait_for_any(&["Password:", "Debian GNU/Linux", "login: "], timeout) {
        WaitResult::Found(PASSWORD_PROMPT) => return Err(Some(LoginFailure::PasswordRequired)),
        WaitResult::Found(_) => {}
        WaitResult::Timeout => return Err(Some(LoginFailure::NoBanner { timeout })),
        WaitResult::Closed => return Err(stopped()),
    }

    let (command, marker) = readiness_probe_command_and_marker(&probe_nonce());
    if let Step::Stop = send_text(input_tx, command) {
        return Err(stopped());
    }
    match output_monitor.wait_for_line_after(&marker, timeout) {
        WaitResult::Found(uid) if uid.trim() == "0" => Ok(()),
        WaitResult::Found(uid) => Err(Some(LoginFailure::ProbeNotRoot {
            uid: uid.trim().to_string(),
        })),
        WaitResult::Timeout => Err(Some(LoginFailure::ProbeTimeout { timeout })),
        WaitResult::Closed => Err(stopped()),
    }
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
        for (index, a) in login_actions.into_iter().enumerate() {
            let _ = vm_output_tx.send(VmOutput::ActionStarted { index });
            let step = match a {
                Expect { text, timeout } => match output_monitor.wait_for_text(&text, timeout) {
                    WaitResult::Found(()) => Step::Continue,
                    WaitResult::Timeout => {
                        let _ = vm_output_tx.send(VmOutput::LoginActionTimeout { index, timeout });
                        Step::Stop
                    }
                    WaitResult::Closed => Step::Stop,
                },
                Send(text) => send_text(&input_tx, text),
                Script { content, .. } => {
                    let id = format!("script_{index}");
                    let (command, status_marker) = script_command_and_status_marker(&id, &content);
                    match send_text(&input_tx, command) {
                        Step::Stop => Step::Stop,
                        Step::Continue => match output_monitor
                            .wait_for_line_after(&status_marker, SCRIPT_ACTION_TIMEOUT)
                        {
                            WaitResult::Found(status) => {
                                let status = status.trim().parse().unwrap_or(u8::MAX);
                                if status == 0 {
                                    Step::Continue
                                } else {
                                    let _ = vm_output_tx
                                        .send(VmOutput::LoginActionFailed { index, status });
                                    Step::Stop
                                }
                            }
                            WaitResult::Timeout => {
                                let _ = vm_output_tx.send(VmOutput::LoginActionTimeout {
                                    index,
                                    timeout: SCRIPT_ACTION_TIMEOUT,
                                });
                                Step::Stop
                            }
                            WaitResult::Closed => Step::Stop,
                        },
                    }
                }
                LegacyLogin { timeout } => {
                    match legacy_login(timeout, &output_monitor, &input_tx) {
                        Ok(()) => {
                            let _ = vm_output_tx.send(VmOutput::LoginReady);
                            Step::Continue
                        }
                        Err(failure) => {
                            if let Some(failure) = failure {
                                let _ = vm_output_tx.send(VmOutput::LoginFailed(failure));
                            }
                            Step::Stop
                        }
                    }
                }
            };
            if let Step::Stop = step {
                return;
            }
        }
        let _ = vm_output_tx.send(VmOutput::ActionsCompleted);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Instant;

    const SHORT: Duration = Duration::from_millis(100);
    const LONG: Duration = Duration::from_secs(60);

    /// Reads one line the actions thread typed into the VM.
    fn typed_line(input_rx: &mpsc::Receiver<VmInput>) -> String {
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

    fn spawn_login(
        monitor: &Arc<OutputMonitor>,
        timeout: Duration,
    ) -> (
        mpsc::Receiver<VmInput>,
        thread::JoinHandle<Result<(), Option<LoginFailure>>>,
    ) {
        let (input_tx, input_rx) = mpsc::channel();
        let monitor = monitor.clone();
        let handle = thread::spawn(move || legacy_login(timeout, &monitor, &input_tx));
        (input_rx, handle)
    }

    /// Answers a readiness probe the way bash on hvc0 would: echo the command,
    /// then print its output.
    fn answer_probe(monitor: &OutputMonitor, command: &str, uid: &str) {
        let args: Vec<&str> = command.split('\'').collect();
        let marker = format!("{}{}", args[3], args[5]);
        monitor.push(format!("root@debian:~#{command}\r\n\r\n{marker}\r\n{uid}\r\n").as_bytes());
    }

    #[test]
    fn legacy_login_confirms_a_root_shell() {
        let monitor = Arc::new(OutputMonitor::default());
        let (input_rx, handle) = spawn_login(&monitor, LONG);
        monitor.push(b"Debian GNU/Linux 13 debian hvc0\r\n\r\ndebian login: ");
        assert_eq!(typed_line(&input_rx), "root");
        monitor.push(b"root\r\nLinux debian 6.12\r\n\r\nThe programs included with the Debian GNU/Linux system are free software;\r\n");
        let probe = typed_line(&input_rx);
        assert!(probe.starts_with(" printf "));
        answer_probe(&monitor, &probe, "0");
        assert_eq!(handle.join().unwrap(), Ok(()));
    }

    #[test]
    fn legacy_login_reports_a_password_prompt() {
        let monitor = Arc::new(OutputMonitor::default());
        let (input_rx, handle) = spawn_login(&monitor, LONG);
        monitor.push(b"debian login: ");
        assert_eq!(typed_line(&input_rx), "root");
        monitor.push(b"root\r\nPassword: ");
        assert_eq!(
            handle.join().unwrap(),
            Err(Some(LoginFailure::PasswordRequired))
        );
    }

    #[test]
    fn legacy_login_rejects_a_non_root_shell() {
        let monitor = Arc::new(OutputMonitor::default());
        let (input_rx, handle) = spawn_login(&monitor, LONG);
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
        let (input_rx, handle) = spawn_login(&monitor, SHORT);
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
        let (_input_rx, handle) = spawn_login(&silent, SHORT);
        assert_eq!(
            handle.join().unwrap(),
            Err(Some(LoginFailure::NoSerialOutput { timeout: SHORT }))
        );

        let noisy = Arc::new(OutputMonitor::default());
        noisy.push(b"[  OK  ] Started some unit\r\n");
        let (_input_rx, handle) = spawn_login(&noisy, SHORT);
        assert_eq!(
            handle.join().unwrap(),
            Err(Some(LoginFailure::NoLoginPrompt { timeout: SHORT }))
        );
    }

    #[test]
    fn legacy_login_stops_quietly_when_the_vm_stops() {
        let monitor = Arc::new(OutputMonitor::default());
        let (_input_rx, handle) = spawn_login(&monitor, LONG);
        thread::sleep(SHORT);
        monitor.close();
        assert_eq!(handle.join().unwrap(), Err(None));

        let monitor = Arc::new(OutputMonitor::default());
        let (input_tx, input_rx) = mpsc::channel();
        drop(input_rx);
        monitor.push(b"login: ");
        assert_eq!(legacy_login(LONG, &monitor, &input_tx), Err(None));
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

    #[test]
    fn legacy_login_prefix_starts_with_the_login() {
        let prefix = legacy_login_prefix(LONG).unwrap();
        assert!(matches!(prefix[0], LegacyLogin { timeout } if timeout == LONG));
        assert!(prefix[1..].iter().all(|action| matches!(action, Send(_))));
    }

    fn actions_thread(
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
    fn next_report(output_rx: &mpsc::Receiver<VmOutput>) -> Option<VmOutput> {
        output_rx
            .try_iter()
            .find(|output| !matches!(output, VmOutput::ActionStarted { .. }))
    }

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
        let (_, marker) = script_command_and_status_marker(id, "exit 3\n");
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
    fn provision_hints_give_runnable_commands_with_absolute_paths() {
        let external = needs_provision_hint(Path::new("/tmp/disk.raw"), &DiskSource::External);
        assert!(external.contains("vibe provision --base /tmp/disk.raw --image <name>"));
        assert!(external.contains("vibe --image <name>"));
        assert!(external.contains("one of the Vibe image directories (see `vibe --help`)"));

        let relative = needs_provision_hint(Path::new("disk.raw"), &DiskSource::External);
        let expected = std::env::current_dir().unwrap().join("disk.raw");
        assert!(relative.contains(&format!(
            "vibe provision --base {} --image <name>",
            shell_word(&expected.to_string_lossy())
        )));

        let spaced = needs_provision_hint(
            Path::new("/tmp/my project/.vibe/instance.raw"),
            &DiskSource::Instance,
        );
        assert!(
            spaced.contains(
                "vibe provision --base '/tmp/my project/.vibe/instance.raw' --image <name>"
            )
        );
        assert!(spaced.contains("move '/tmp/my project/.vibe/instance.raw' away"));
        assert!(spaced.contains("vibe --image <name>"));

        let cached = needs_provision_hint(
            &PathBuf::from("/cache/vibe/demo.raw"),
            &DiskSource::CachedImage("demo".to_string()),
        );
        assert!(cached.contains("vibe provision --base demo --image <new-name>"));
        assert!(cached.contains("vibe provision --image demo --replace"));

        for hint in [external, relative, spaced, cached] {
            assert!(hint.contains("may be a newer Debian image"));
        }
    }

    #[test]
    fn only_boot_and_password_failures_suggest_provision() {
        let timeout = LONG;
        assert!(LoginFailure::NoSerialOutput { timeout }.suggests_provision());
        assert!(LoginFailure::NoLoginPrompt { timeout }.suggests_provision());
        assert!(LoginFailure::PasswordRequired.suggests_provision());
        assert!(LoginFailure::VmStopped.suggests_provision());
        assert!(!LoginFailure::NoBanner { timeout }.suggests_provision());
        assert!(!LoginFailure::ProbeTimeout { timeout }.suggests_provision());
        assert!(
            LoginFailure::NoSerialOutput { timeout }
                .describe(Path::new("disk.raw"))
                .starts_with("[boot] No output on the serial console")
        );
        assert!(
            LoginFailure::PasswordRequired
                .describe(Path::new("disk.raw"))
                .starts_with("[login]")
        );
    }
}
