//! Why a login or a login action failed, and the hints printed for it.

use std::{path::Path, time::Duration};

use crate::vm_user::{VmUser, VmUserProblem};

use super::shell::shell_word;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
}
