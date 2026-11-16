//! Login actions typed into the guest console after root login, and the
//! helpers that build the shell commands for them.

use std::{
    collections::HashMap,
    fmt,
    hash::{BuildHasher, Hasher},
    path::Path,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose};

use crate::DirectoryShare;
use crate::vm_io::{OutputMonitor, VmInput, WaitResult};
use crate::vm_user::{VmUser, VmUserProblem};

pub(crate) const DEFAULT_EXPECT_TIMEOUT: Duration = Duration::from_secs(30);
const SCRIPT_ACTION_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
const STEP_STATUS_MARKER_PREFIX: &str = "VIBE_STEP_STATUS_";
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

fn is_password_control_char(c: char) -> bool {
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

const SWITCH_PROBE_MARKER_PREFIX: &str = "VIBE_SWITCHED_";
/// How long to wait for one switch probe before typing it again. The probe
/// is typed while `su` is still starting; if the new shell drops that
/// input, a later probe gets through.
const SWITCH_PROBE_RETRY: Duration = Duration::from_secs(10);

/// The line typed into the root shell to start the target user's session.
/// The root shell reads the whole line, so the notice is printed only after
/// the user's shell exits.
pub(crate) const SWITCH_USER_COMMAND: &str = " su --login --pty \"$VIBE_VM_USER\"; echo; echo 'Vibe: back in the root shell. Type exit again to power off the VM.'";

/// Builds the switch probe command and the marker line it prints; the line
/// after the marker is `<user name> <uid>` of the shell. The marker is split
/// like the readiness probe's.
fn switch_probe_command_and_marker(nonce: &str) -> (String, String) {
    let marker = format!("{SWITCH_PROBE_MARKER_PREFIX}{nonce}");
    let command = format!(
        " printf '\\n%s%s\\n%s %s\\n' '{SWITCH_PROBE_MARKER_PREFIX}' '{nonce}' \"$(id -un)\" \"$(id -u)\""
    );
    (command, marker)
}

/// Whether a switch probe line (`<user name> <uid>`) shows a shell of `user`
/// that is not running as root.
fn is_switched_to(line: &str, user: &VmUser) -> bool {
    let mut words = line.split_whitespace();
    words.next() == Some(user.as_str()) && words.next().is_some_and(|uid| uid != "0")
}

/// Types [`SWITCH_USER_COMMAND`] and waits for the probe of the new shell.
/// `Err(None)` means the VM stopped or its input closed.
fn switch_user(
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

/// Why the legacy root login did not reach a usable shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoginFailure {
    /// Nothing at all arrived on hvc0 while waiting for `login: `.
    NoSerialOutput { timeout: Duration },
    /// hvc0 produced output, but never a `login: ` prompt.
    NoLoginPrompt { timeout: Duration },
    /// The guest asked for a root password and none was supplied.
    PasswordRequired,
    /// The supplied root password was rejected.
    AuthFailed,
    /// The guest did not accept or reject the root password before `timeout`.
    AuthTimeout { timeout: Duration },
    /// Neither a password prompt nor the Debian banner followed `root`.
    NoBanner { timeout: Duration },
    /// The shell never answered the readiness probe.
    ProbeTimeout { timeout: Duration },
    /// The readiness probe answered, but the shell is not running as root.
    ProbeNotRoot { uid: String },
    /// The VM stopped before the login completed.
    VmStopped,
    /// The `--vm-user` account is missing or cannot be used.
    VmUser {
        user: VmUser,
        problem: VmUserProblem,
    },
    /// `su` did not reach a shell of the target user. `got` is the
    /// `<name> <uid>` the probe reported, or `None` when it did not answer
    /// within `timeout`.
    SwitchFailed {
        user: VmUser,
        got: Option<String>,
        timeout: Duration,
    },
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
                "[login] The image asks for a root password (disk: {disk}). Pass --root-password to log in."
            ),
            LoginFailure::AuthFailed => format!(
                "[login] root authentication failed with the given --root-password (disk: {disk})."
            ),
            LoginFailure::AuthTimeout { timeout } => format!(
                "[login] Sent the root password but saw neither a successful login nor 'Login incorrect' within {timeout:?} (disk: {disk})."
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
            LoginFailure::VmUser { user, problem } => {
                let name = user.as_str();
                let reason = problem.describe();
                match problem {
                    VmUserProblem::Missing => {
                        let absolute = std::path::absolute(Path::new(&disk.to_string()))
                            .unwrap_or_else(|_| Path::new(&disk.to_string()).to_path_buf());
                        let base = shell_word(&absolute.to_string_lossy());
                        format!(
                            "[login] vm-user '{name}' {reason} (disk: {disk}). Vibe only creates users during provision; create an image with: vibe provision --base {base} --image <new-name> --vm-user {name}"
                        )
                    }
                    _ => format!(
                        "[login] vm-user '{name}' {reason} (disk: {disk}). Vibe does not change existing accounts; fix the account in the image or choose another --vm-user."
                    ),
                }
            }
            LoginFailure::SwitchFailed {
                user,
                got: Some(got),
                ..
            } => format!(
                "[login] Switching to vm-user '{}' with su did not reach a shell of that user; the shell reported '{got}' (disk: {disk}). Check the account's login shell and the PAM configuration of su in the image.",
                user.as_str()
            ),
            LoginFailure::SwitchFailed {
                user,
                got: None,
                timeout,
            } => format!(
                "[login] The shell of vm-user '{}' did not answer within {timeout:?} after su (disk: {disk}). Check the account's login shell and its startup files.",
                user.as_str()
            ),
        }
    }

    /// Whether the failure may be caused by booting an image that Vibe has not
    /// prepared, so the error should suggest `vibe provision`.
    pub(crate) fn suggests_provision(&self) -> bool {
        matches!(
            self,
            LoginFailure::NoSerialOutput { .. }
                | LoginFailure::NoLoginPrompt { .. }
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

pub(crate) fn shell_word(value: &str) -> String {
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

pub(crate) fn generate_http_proxy_env_export_cmds(proxy_url: &str, no_proxy: &str) -> String {
    let url = shell_single_quote(proxy_url);
    let no_proxy = shell_single_quote(no_proxy);
    format!(
        "export http_proxy='{url}'; export HTTP_PROXY='{url}'; export https_proxy='{url}'; export HTTPS_PROXY='{url}'; export no_proxy='{no_proxy}'; export NO_PROXY='{no_proxy}';"
    )
}

/// Variables of the root shell's mise activation; they would point the
/// user's own mise at root's state.
const ROOT_MISE_VARIABLES: &str =
    "-u MISE_SHELL -u __MISE_DIFF -u __MISE_ORIG_PATH -u __MISE_SESSION";

/// Command and status marker of a [`LoginAction::Script`], or with
/// `as_vm_user` of a [`LoginAction::UserScript`].
fn script_command(id: &str, script: &str, as_vm_user: bool) -> (String, String) {
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
fn status_command_and_marker(id: &str, command: &str) -> (String, String) {
    let marker = format!("{STEP_STATUS_MARKER_PREFIX}{id}");
    let command = format!(
        " {{ {command}\n}} </dev/null\n __vibe_status=$?; printf '\\n%s%s\\n%s %s\\n' '{STEP_STATUS_MARKER_PREFIX}' '{id}' \"$__vibe_status\" \"${{__vibe_notice:-}}\"; unset __vibe_status __vibe_notice"
    );
    (command, marker)
}

/// Splits a status line from [`status_command_and_marker`] into the exit
/// status (`u8::MAX` when it is not a number) and the optional notice word.
fn parse_status_line(line: &str) -> (u8, Option<String>) {
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

/// Result of a legacy root login that reached a root shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoginInfo {
    /// The guest printed `Password:` and accepted the supplied password.
    pub(crate) password_prompted: bool,
}

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

const ROOT_PASSWORD_MARKER_PREFIX: &str = "VIBE_PW_";
const ROOT_PASSWORD_DONE_PREFIX: &str = "VIBE_PW_DONE_";
/// Typed right after the login, before the vm-user step and the share
/// mounts; the wait is the same length as the login waits.
const SET_ROOT_PASSWORD_TIMEOUT: Duration = Duration::from_secs(120);

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

fn legacy_login(
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
            let as_vm_user = matches!(a, UserScript { .. });
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
                Script { content, .. } | UserScript { content, .. } => {
                    let id = format!("script_{index}");
                    let (command, status_marker) = script_command(&id, &content, as_vm_user);
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
                LegacyLogin { timeout, password } => {
                    match legacy_login(timeout, password.as_deref(), &output_monitor, &input_tx) {
                        Ok(info) => {
                            let _ = vm_output_tx.send(VmOutput::LoginReady {
                                password_prompted: info.password_prompted,
                            });
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
                SwitchUser { user, timeout } => {
                    match switch_user(&user, timeout, &output_monitor, &input_tx) {
                        Ok(()) => Step::Continue,
                        Err(failure) => {
                            if let Some(failure) = failure {
                                let _ = vm_output_tx.send(VmOutput::LoginFailed(failure));
                            }
                            Step::Stop
                        }
                    }
                }
                StatusCommand {
                    command, timeout, ..
                } => {
                    let (command, marker) =
                        status_command_and_marker(&format!("step_{index}"), &command);
                    match send_text(&input_tx, command) {
                        Step::Stop => Step::Stop,
                        Step::Continue => {
                            match output_monitor.wait_for_line_after(&marker, timeout) {
                                WaitResult::Found(line) => {
                                    let (status, notice) = parse_status_line(&line);
                                    if let Some(notice) = notice {
                                        let _ = vm_output_tx.send(VmOutput::Notice(notice));
                                    }
                                    if status == 0 {
                                        Step::Continue
                                    } else {
                                        let _ = vm_output_tx
                                            .send(VmOutput::LoginActionFailed { index, status });
                                        Step::Stop
                                    }
                                }
                                WaitResult::Timeout => {
                                    let _ = vm_output_tx
                                        .send(VmOutput::LoginActionTimeout { index, timeout });
                                    Step::Stop
                                }
                                WaitResult::Closed => Step::Stop,
                            }
                        }
                    }
                }
                SecretSend {
                    marker,
                    done_marker,
                    password,
                    timeout,
                } => match output_monitor.wait_for_text(&marker, timeout) {
                    WaitResult::Timeout => {
                        let _ = vm_output_tx.send(VmOutput::LoginActionTimeout { index, timeout });
                        Step::Stop
                    }
                    WaitResult::Closed => Step::Stop,
                    WaitResult::Found(()) => {
                        if let Step::Stop = send_text(&input_tx, password.expose().to_string()) {
                            Step::Stop
                        } else {
                            match output_monitor.wait_for_line_after(&done_marker, timeout) {
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
                                    let _ = vm_output_tx
                                        .send(VmOutput::LoginActionTimeout { index, timeout });
                                    Step::Stop
                                }
                                WaitResult::Closed => Step::Stop,
                            }
                        }
                    }
                },
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

    fn password(value: &str) -> RootPassword {
        RootPassword::parse(value, "--root-password").unwrap()
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
    fn legacy_login_reports_a_password_prompt() {
        let monitor = Arc::new(OutputMonitor::default());
        let (input_rx, handle) = spawn_login(&monitor, LONG, None);
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

    #[test]
    fn legacy_login_prefix_starts_with_the_login() {
        let prefix = legacy_login_prefix(LONG, None).unwrap();
        assert!(matches!(
            prefix[0],
            LegacyLogin {
                timeout,
                password: None
            } if timeout == LONG
        ));
        assert!(prefix[1..].iter().all(|action| matches!(action, Send(_))));
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
    fn user_files_are_written_by_the_user_and_root_files_as_before() {
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

    #[test]
    fn vm_user_failures_explain_the_account_problem() {
        let dev = VmUser::parse("dev").unwrap();
        let missing = LoginFailure::VmUser {
            user: dev.clone(),
            problem: VmUserProblem::Missing,
        };
        let text = missing.describe(Path::new("/images/my disk.raw"));
        assert!(
            text.contains(
                "vibe provision --base '/images/my disk.raw' --image <new-name> --vm-user dev"
            ),
            "{text}"
        );
        assert!(!missing.suggests_provision());
        let shell = LoginFailure::VmUser {
            user: dev,
            problem: VmUserProblem::from_status(12).unwrap(),
        };
        let text = shell.describe(Path::new("/images/disk.raw"));
        assert!(text.contains("vm-user 'dev'"), "{text}");
        assert!(text.contains("does not change existing accounts"), "{text}");
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
    fn only_boot_failures_suggest_provision() {
        let timeout = LONG;
        assert!(LoginFailure::NoSerialOutput { timeout }.suggests_provision());
        assert!(LoginFailure::NoLoginPrompt { timeout }.suggests_provision());
        assert!(LoginFailure::VmStopped.suggests_provision());
        assert!(!LoginFailure::PasswordRequired.suggests_provision());
        assert!(!LoginFailure::AuthFailed.suggests_provision());
        assert!(!LoginFailure::AuthTimeout { timeout }.suggests_provision());
        assert!(!LoginFailure::NoBanner { timeout }.suggests_provision());
        assert!(!LoginFailure::ProbeTimeout { timeout }.suggests_provision());
        assert!(
            LoginFailure::NoSerialOutput { timeout }
                .describe(Path::new("disk.raw"))
                .starts_with("[boot] No output on the serial console")
        );
        assert_eq!(
            LoginFailure::PasswordRequired.describe(Path::new("disk.raw")),
            "[login] The image asks for a root password (disk: disk.raw). Pass --root-password to log in."
        );
        assert_eq!(
            LoginFailure::AuthFailed.describe(Path::new("/img/dev.raw")),
            "[login] root authentication failed with the given --root-password (disk: /img/dev.raw)."
        );
        let auth_timeout = LoginFailure::AuthTimeout { timeout }.describe(Path::new("disk.raw"));
        assert!(auth_timeout.starts_with("[login]"), "{auth_timeout}");
        assert!(auth_timeout.contains("root password"), "{auth_timeout}");
    }

    /// Drives a login up through the `Password:` prompt and returns the line
    /// typed in response.
    fn login_to_password_prompt(
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
