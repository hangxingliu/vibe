//! The guest account named by `--vm-user`, and guest paths that are
//! relative to its HOME.

use std::path::{Path, PathBuf};

use std::time::Duration;

use base64::{Engine, engine::general_purpose};

use crate::session::{LoginAction, shell_word};

const CHECK_VM_USER_SCRIPT: &str = include_str!("guest/check_vm_user.sh");
const ENSURE_VM_USER_SCRIPT: &str = include_str!("guest/ensure_vm_user.sh");
const VM_USER_STEP_TIMEOUT: Duration = Duration::from_secs(120);

/// Guest account selected for a run or a provision. Omitting `--vm-user`
/// selects `root`, and `--vm-user root` is the same value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VmUser(String);

impl VmUser {
    pub(crate) const MAX_LEN: usize = 32;

    pub(crate) fn root() -> Self {
        Self("root".to_string())
    }

    /// Accepts `^[a-z_][a-z0-9_-]*$`, length 1–32.
    /// A stricter subset of Debian's `useradd` names, and safe as a single
    /// path component under the host cache directory.
    pub(crate) fn parse(name: &str) -> Result<Self, String> {
        if !is_vm_user_name(name) {
            return Err(format!(
                "--vm-user '{name}' is invalid: use 1-{} characters matching ^[a-z_][a-z0-9_-]*$",
                Self::MAX_LEN
            ));
        }
        Ok(Self(name.to_string()))
    }

    /// The account name.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn is_root(&self) -> bool {
        self.0 == "root"
    }
}

/// Why the `--vm-user` account cannot be used, from the exit code of
/// `src/guest/check_vm_user.sh`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VmUserProblem {
    Missing,
    UidZero,
    ShellNotBash,
    BadHome,
    HomeOwner,
    HomeIsRoot,
}

impl VmUserProblem {
    pub(crate) fn from_status(status: u8) -> Option<Self> {
        Some(match status {
            10 => Self::Missing,
            11 => Self::UidZero,
            12 => Self::ShellNotBash,
            13 => Self::BadHome,
            14 => Self::HomeOwner,
            15 => Self::HomeIsRoot,
            _ => return None,
        })
    }

    pub(crate) fn describe(self) -> &'static str {
        match self {
            Self::Missing => "does not exist in this image",
            Self::UidZero => "has UID 0",
            Self::ShellNotBash => "does not have bash as its login shell",
            Self::BadHome => "does not have an existing absolute directory as its HOME",
            Self::HomeOwner => "does not own its HOME directory",
            Self::HomeIsRoot => "has /root as its HOME",
        }
    }
}

/// Runs a bundled guest script with `sh`, passing the account name.
fn run_guest_script(script: &str, user: &VmUser) -> String {
    let b64 = general_purpose::STANDARD.encode(script);
    format!(
        "echo '{b64}' | base64 --decode | sh -s -- {}",
        shell_word(user.as_str())
    )
}

/// Exports `VIBE_VM_USER`, `VIBE_VM_UID`, `VIBE_VM_GID`, and `VIBE_VM_HOME`
/// in the root shell, read from the guest account database. Mounts below the
/// user's HOME and later actions use them.
fn vm_user_env_action(user: &VmUser) -> LoginAction {
    let name = shell_word(user.as_str());
    LoginAction::Send(format!(
        " export VIBE_VM_USER={name} VIBE_VM_UID=\"$(id -u {name})\" VIBE_VM_GID=\"$(id -g {name})\" VIBE_VM_HOME=\"$(getent passwd {name} | cut -d: -f6)\""
    ))
}

/// Root actions for a run, and for `--repl` of an existing image, that check
/// an existing non-root `--vm-user` account and export its `VIBE_VM_*`
/// variables. Vibe never creates or changes an account on these paths.
/// Empty for root.
pub(crate) fn check_vm_user_actions(user: &VmUser) -> Vec<LoginAction> {
    if user.is_root() {
        return Vec::new();
    }
    vec![
        LoginAction::StatusCommand {
            label: "check vm-user".to_string(),
            command: format!(
                "__vibe_notice=$({})",
                run_guest_script(CHECK_VM_USER_SCRIPT, user)
            ),
            timeout: VM_USER_STEP_TIMEOUT,
            vm_user: Some(user.clone()),
        },
        vm_user_env_action(user),
    ]
}

/// Root actions for a provision that runs scripts: create a missing non-root
/// `--vm-user` account, check it, and export its `VIBE_VM_*` variables. An
/// existing account is checked but not changed. Empty for root.
pub(crate) fn ensure_vm_user_actions(user: &VmUser) -> Vec<LoginAction> {
    if user.is_root() {
        return Vec::new();
    }
    vec![
        LoginAction::StatusCommand {
            label: "create vm-user".to_string(),
            command: format!(
                "__vibe_notice=$({} && {})",
                run_guest_script(ENSURE_VM_USER_SCRIPT, user),
                run_guest_script(CHECK_VM_USER_SCRIPT, user)
            ),
            timeout: VM_USER_STEP_TIMEOUT,
            vm_user: Some(user.clone()),
        },
        vm_user_env_action(user),
    ]
}

/// Where a share or mask appears in the guest.
///
/// The HOME of an existing account is only known inside the guest, so paths
/// below it stay relative until they are rendered. For root they render as
/// `/root/<rel>`; for any other user as `"${VIBE_VM_HOME}"/<rel>`, which the
/// root shell sets after checking the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GuestPath {
    Absolute(PathBuf),
    /// Relative to the target user's HOME.
    Home(PathBuf),
}

impl GuestPath {
    /// Absolute paths stay absolute; relative paths are below the target HOME.
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        if path.is_absolute() {
            Self::Absolute(path)
        } else {
            Self::Home(path)
        }
    }

    pub(crate) fn join(&self, child: impl AsRef<Path>) -> Self {
        match self {
            Self::Absolute(path) => Self::Absolute(path.join(child)),
            Self::Home(rel) => Self::Home(rel.join(child)),
        }
    }

    /// Shell word(s) for the path in a root shell command.
    pub(crate) fn to_shell(&self, user: &VmUser) -> String {
        match self {
            Self::Absolute(path) => shell_word(&path.to_string_lossy()),
            Self::Home(rel) if user.is_root() => {
                shell_word(&Path::new("/root").join(rel).to_string_lossy())
            }
            Self::Home(rel) if rel.as_os_str().is_empty() => "\"${VIBE_VM_HOME}\"".to_string(),
            Self::Home(rel) => format!(
                "\"${{VIBE_VM_HOME}}\"/{}",
                shell_word(&rel.to_string_lossy())
            ),
        }
    }

    /// Shell word(s) for the path in a command typed into the target user's
    /// own shell, where `VIBE_VM_HOME` is not set: paths below HOME use
    /// `"$HOME"` for a non-root user. Same as [`GuestPath::to_shell`] for root.
    pub(crate) fn to_user_shell(&self, user: &VmUser) -> String {
        match self {
            Self::Home(rel) if !user.is_root() && rel.as_os_str().is_empty() => {
                "\"$HOME\"".to_string()
            }
            Self::Home(rel) if !user.is_root() => {
                format!("\"$HOME\"/{}", shell_word(&rel.to_string_lossy()))
            }
            _ => self.to_shell(user),
        }
    }

    /// Human-readable form for host logs and the MOTD.
    pub(crate) fn display(&self, user: &VmUser) -> String {
        match self {
            Self::Absolute(path) => path.display().to_string(),
            Self::Home(rel) if user.is_root() => Path::new("/root").join(rel).display().to_string(),
            Self::Home(rel) if rel.as_os_str().is_empty() => format!("~{}", user.as_str()),
            Self::Home(rel) => format!("~{}/{}", user.as_str(), rel.display()),
        }
    }

    /// For a non-root user and a path below HOME: each directory from the
    /// first component down to the path itself, outermost first. They are
    /// created one at a time and owned by the user. Empty otherwise.
    pub(crate) fn home_dirs(&self, user: &VmUser) -> Vec<GuestPath> {
        let Self::Home(rel) = self else {
            return Vec::new();
        };
        if user.is_root() {
            return Vec::new();
        }
        let mut prefix = PathBuf::new();
        rel.components()
            .map(|component| {
                prefix.push(component);
                Self::Home(prefix.clone())
            })
            .collect()
    }
}

impl Default for VmUser {
    fn default() -> Self {
        Self::root()
    }
}

fn is_vm_user_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first == '_') || name.len() > VmUser::MAX_LEN {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the check script with stub `getent`, `stat`, and `id` (the host
    /// may not have GNU `stat` or the account). Returns the exit code and stdout.
    fn check_with_stubs(passwd: Option<&str>, owner: &str, groups: &str) -> (i32, String) {
        let dir = std::env::temp_dir().join(format!(
            "vibe-check-vm-user-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bin = dir.join("bin");
        let home = dir.join("home");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let stub = |name: &str, body: String| {
            use std::os::unix::fs::PermissionsExt;
            let path = bin.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        match passwd {
            Some(entry) => {
                let entry = entry.replace("$HOME_DIR", &home.to_string_lossy());
                stub("getent", format!("echo '{entry}'"));
            }
            None => stub("getent", "exit 2".to_string()),
        }
        stub("stat", format!("echo {owner}"));
        stub("id", format!("echo '{groups}'"));
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(CHECK_VM_USER_SCRIPT)
            .arg("check_vm_user.sh")
            .arg("dev")
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        (
            output.status.code().unwrap(),
            String::from_utf8(output.stdout).unwrap(),
        )
    }

    #[test]
    fn check_script_exit_codes_match_the_reported_problems() {
        let ok = "dev:x:1000:1000::$HOME_DIR:/bin/bash";
        assert_eq!(
            check_with_stubs(Some(ok), "1000", "dev"),
            (0, String::new())
        );
        assert_eq!(
            check_with_stubs(Some(ok), "1000", "dev adm sudo users"),
            (0, "groups=adm,sudo\n".to_string())
        );
        let cases = [
            (None, "1000", VmUserProblem::Missing),
            (
                Some("dev:x:0:0::$HOME_DIR:/bin/bash"),
                "0",
                VmUserProblem::UidZero,
            ),
            (
                Some("dev:x:1000:1000::$HOME_DIR:/bin/sh"),
                "1000",
                VmUserProblem::ShellNotBash,
            ),
            (
                Some("dev:x:1000:1000::home:/bin/bash"),
                "1000",
                VmUserProblem::BadHome,
            ),
            (
                Some("dev:x:1000:1000::$HOME_DIR/none:/bin/bash"),
                "1000",
                VmUserProblem::BadHome,
            ),
            (Some(ok), "0", VmUserProblem::HomeOwner),
            (
                Some("dev:x:1000:1000::/root:/bin/bash"),
                "1000",
                VmUserProblem::HomeIsRoot,
            ),
        ];
        for (entry, owner, problem) in cases {
            let (code, stdout) = check_with_stubs(entry, owner, "dev");
            let status = u8::try_from(code).unwrap();
            assert_eq!(
                VmUserProblem::from_status(status),
                Some(problem),
                "{entry:?}"
            );
            assert_eq!(stdout, "", "{entry:?}");
        }
    }

    fn dev() -> VmUser {
        VmUser::parse("dev").unwrap()
    }

    #[test]
    fn root_paths_render_below_slash_root() {
        let root = VmUser::root();
        let cargo = GuestPath::new(".cargo");
        assert_eq!(cargo, GuestPath::Home(".cargo".into()));
        assert_eq!(cargo.to_shell(&root), "/root/.cargo");
        assert_eq!(cargo.display(&root), "/root/.cargo");
        let mise = GuestPath::new(".local/share/mise");
        assert_eq!(mise.to_shell(&root), "/root/.local/share/mise");
        assert!(mise.home_dirs(&root).is_empty());
        let apt = GuestPath::new("/var/cache/apt");
        assert_eq!(apt, GuestPath::Absolute("/var/cache/apt".into()));
        assert_eq!(apt.to_shell(&root), "/var/cache/apt");
        assert_eq!(apt.to_shell(&dev()), "/var/cache/apt");
        assert_eq!(
            GuestPath::new("proj").join(".git").to_shell(&root),
            "/root/proj/.git"
        );
    }

    #[test]
    fn other_users_render_below_vibe_vm_home() {
        let dev = dev();
        let mise = GuestPath::new(".local/share/mise");
        assert_eq!(mise.to_shell(&dev), "\"${VIBE_VM_HOME}\"/.local/share/mise");
        assert_eq!(mise.display(&dev), "~dev/.local/share/mise");
        assert_eq!(
            mise.home_dirs(&dev)
                .iter()
                .map(|path| path.to_shell(&dev))
                .collect::<Vec<_>>(),
            [
                "\"${VIBE_VM_HOME}\"/.local",
                "\"${VIBE_VM_HOME}\"/.local/share",
                "\"${VIBE_VM_HOME}\"/.local/share/mise",
            ]
        );
        assert!(GuestPath::new("/srv/x").home_dirs(&dev).is_empty());
        assert_eq!(GuestPath::new("").to_shell(&dev), "\"${VIBE_VM_HOME}\"");
    }

    #[test]
    fn user_shell_paths_use_home_for_other_users_only() {
        let project = GuestPath::new("my proj");
        assert_eq!(project.to_user_shell(&dev()), "\"$HOME\"/'my proj'");
        assert_eq!(GuestPath::new("").to_user_shell(&dev()), "\"$HOME\"");
        assert_eq!(
            project.to_user_shell(&VmUser::root()),
            project.to_shell(&VmUser::root())
        );
        assert_eq!(GuestPath::new("/srv/x").to_user_shell(&dev()), "/srv/x");
    }

    #[test]
    fn names_with_spaces_and_quotes_are_quoted() {
        let project = GuestPath::new("my project's");
        assert_eq!(
            project.to_shell(&VmUser::root()),
            "'/root/my project'\\''s'"
        );
        assert_eq!(
            project.join(".vibe").to_shell(&dev()),
            "\"${VIBE_VM_HOME}\"/'my project'\\''s/.vibe'"
        );
        assert_eq!(project.display(&dev()), "~dev/my project's");
    }

    #[test]
    fn accepts_root_and_ordinary_names() {
        assert_eq!(VmUser::parse("root").unwrap(), VmUser::root());
        assert_eq!(VmUser::default(), VmUser::root());
        assert_eq!(VmUser::parse("dev").unwrap().as_str(), "dev");
        assert_eq!(VmUser::parse("_a").unwrap().as_str(), "_a");
        assert_eq!(VmUser::parse("dev-user_1").unwrap().as_str(), "dev-user_1");
        let max = format!("a{}", "b".repeat(VmUser::MAX_LEN - 1));
        assert_eq!(max.len(), VmUser::MAX_LEN);
        assert_eq!(VmUser::parse(&max).unwrap().as_str(), max);
    }

    #[test]
    fn rejects_empty_uppercase_digit_slash_and_overlong_names() {
        for name in [
            "",
            "Dev",
            "A",
            "1dev",
            "dev/user",
            "dev.user",
            "-dev",
            "has space",
        ] {
            let err = VmUser::parse(name).unwrap_err();
            assert!(err.contains("--vm-user"), "{name}: {err}");
            assert!(err.contains(name), "{name}: {err}");
        }
        let overlong = "a".repeat(VmUser::MAX_LEN + 1);
        let err = VmUser::parse(&overlong).unwrap_err();
        assert!(err.contains("--vm-user"), "{err}");
        assert!(err.contains(&overlong), "{err}");
    }
}
