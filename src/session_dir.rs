//! A private directory of files for one VM session.
//!
//! The directory is `<cache>/sessions/<pid>-<nonce>/`, mode 0700. It is shared
//! read-only and is not bind-mounted. Root reads files at
//! `/mnt/root/virtiofs-shared/<tag>/<rel>`. The directory is removed when the
//! VM stops normally. Otherwise it stays, and the next session creation
//! removes directories whose pid is no longer running.
//!
//! Nothing in the directory is a secret. The root password is typed with
//! `SecretSend` and is never written here.

use std::{
    fs::{self, OpenOptions},
    io::{self, ErrorKind, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use crate::cache::{self, private_dir};
use crate::session::LoginAction;
use crate::session::shell_word;
use crate::session_plan::{STAGING_DIR, SessionPlan};
use crate::shares::DirectoryShare;

/// Absolute guest path of a file in the session share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GuestFile(String);

impl GuestFile {
    /// The guest path, without shell quoting.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// Shell word for the guest path.
    pub(crate) fn as_shell(&self) -> String {
        shell_word(&self.0)
    }

    /// `sh -c "$(cat '<path>')" <name> <args>`: root reads the file, so the
    /// command also works after `runuser`. stdin stays free.
    ///
    /// `name` is quoted as a shell word. Each entry of `args` is appended as
    /// the caller wrote it: a [`shell_word`](crate::session::shell_word)
    /// result, or a root-shell expression such as `"${VIBE_VM_HOME}"` that
    /// must expand in the root shell.
    pub(crate) fn sh_command(&self, name: &str, args: &[String]) -> String {
        let mut command = format!("sh -c \"$(cat {})\" {}", self.as_shell(), shell_word(name));
        for arg in args {
            command.push(' ');
            command.push_str(arg);
        }
        command
    }
}

/// `<cache>/sessions/<pid>-<nonce>/`, shared read-only and never bind-mounted.
#[derive(Debug)]
pub(crate) struct SessionDir {
    host: PathBuf,
    share: DirectoryShare,
    /// Next `scripts/NNN-` sequence number.
    next: usize,
}

impl SessionDir {
    /// Sweeps dead-pid directories, then makes a new private one.
    pub(crate) fn create(cache_dir: &Path) -> io::Result<Self> {
        let parent = cache_dir.join("sessions");
        cache::sweep_stale(&parent)?;
        let host = private_dir(&parent, "")?;
        let share = DirectoryShare::staging_only(host.clone()).map_err(|err| {
            let _ = fs::remove_dir_all(&host);
            io::Error::other(err.to_string())
        })?;
        Ok(Self {
            host,
            share,
            next: 0,
        })
    }

    pub(crate) fn share(&self) -> &DirectoryShare {
        &self.share
    }

    /// Writes `rel` (mode 0600) and returns its guest path.
    /// `rel` must stay inside this directory.
    pub(crate) fn write(&mut self, rel: &str, content: &str) -> io::Result<GuestFile> {
        session_relative(rel)?;
        let path = self.host.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(content.as_bytes())?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        Ok(GuestFile(format!(
            "{STAGING_DIR}/{}/{}",
            self.share.tag(),
            rel
        )))
    }

    /// Writes every `Script` and `UserScript` in `plan` to `scripts/NNN-<name>.sh`
    /// and sets its `file`. `NNN` counts those actions from 000. `<name>` is the
    /// basename, with characters outside `[A-Za-z0-9._-]` replaced by `_`.
    /// A root part that is followed by the same-named user part uses
    /// `.root.sh`. That user part uses `.user.sh`.
    ///
    /// `user_actions` that run in a non-root vm-user's shell are not staged:
    /// that user cannot read `/mnt/root`, so those scripts stay inline.
    pub(crate) fn stage_scripts(
        &mut self,
        plan: &mut SessionPlan,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut roles = script_roles(&plan.root_actions, &plan.user_actions);
        let root_len = plan.root_actions.len();
        if plan.switch_user && !plan.vm_user.is_root() {
            roles[root_len..].fill(None);
        }
        self.apply_script_roles(&mut plan.root_actions, &roles[..root_len])?;
        self.apply_script_roles(&mut plan.user_actions, &roles[root_len..])?;
        Ok(())
    }

    fn apply_script_roles(
        &mut self,
        actions: &mut [LoginAction],
        roles: &[Option<ScriptRole>],
    ) -> Result<(), Box<dyn std::error::Error>> {
        for (action, role) in actions.iter_mut().zip(roles) {
            let Some(role) = role else {
                continue;
            };
            let (name, content) = match &*action {
                LoginAction::Script { name, content, .. }
                | LoginAction::UserScript { name, content, .. } => (name.clone(), content.clone()),
                _ => continue,
            };
            let rel = staged_script_rel(self.next, &name, *role);
            let file = self.write(&rel, &content).map_err(|err| {
                format!("[prepare] cannot write {rel} into the session directory: {err}")
            })?;
            match action {
                LoginAction::Script { file: slot, .. }
                | LoginAction::UserScript { file: slot, .. } => *slot = Some(file),
                _ => {}
            }
            self.next += 1;
        }
        Ok(())
    }

    /// Host directory. Tests read files the guest will see.
    #[cfg(test)]
    pub(crate) fn host(&self) -> &Path {
        &self.host
    }

    /// Removes the directory when the VM stopped normally. Otherwise keeps it
    /// and returns its path.
    pub(crate) fn finish(self, stopped_normally: bool) -> io::Result<Option<PathBuf>> {
        if stopped_normally {
            fs::remove_dir_all(&self.host)?;
            Ok(None)
        } else {
            Ok(Some(self.host.clone()))
        }
    }
}

/// Removes the directory when `stopped_normally` is true. Otherwise keeps it
/// and prints `Vibe kept the session scripts at <path>` on stderr.
pub(crate) fn close_session(
    session: SessionDir,
    stopped_normally: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = session.host.clone();
    match session.finish(stopped_normally) {
        Ok(Some(kept)) => {
            eprintln!("Vibe kept the session scripts at {}", kept.display());
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(err) => Err(format!(
            "[prepare] cannot remove the session directory {}: {err}",
            path.display()
        )
        .into()),
    }
}

#[derive(Clone, Copy)]
enum ScriptRole {
    Whole,
    Root,
    User,
}

fn script_roles(root: &[LoginAction], user: &[LoginAction]) -> Vec<Option<ScriptRole>> {
    let actions: Vec<&LoginAction> = root.iter().chain(user.iter()).collect();
    actions
        .iter()
        .enumerate()
        .map(|(index, action)| match action {
            LoginAction::UserScript { .. } => Some(ScriptRole::User),
            LoginAction::Script { name, .. } => {
                let split = matches!(
                    actions.get(index + 1),
                    Some(LoginAction::UserScript { name: next, .. }) if next == name
                );
                Some(if split {
                    ScriptRole::Root
                } else {
                    ScriptRole::Whole
                })
            }
            _ => None,
        })
        .collect()
}

/// `scripts/NNN-<name>.sh`, `.root.sh`, or `.user.sh`.
fn staged_script_rel(index: usize, name: &str, role: ScriptRole) -> String {
    let base = script_basename(name);
    let suffix = match role {
        ScriptRole::Whole => "sh",
        ScriptRole::Root => "root.sh",
        ScriptRole::User => "user.sh",
    };
    format!("scripts/{index:03}-{base}.{suffix}")
}

fn script_basename(name: &str) -> String {
    let raw = Path::new(name)
        .file_name()
        .map(|part| part.to_string_lossy().into_owned())
        .filter(|part| !part.is_empty())
        .unwrap_or_else(|| "script".to_string());
    let safe: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() {
        "script".to_string()
    } else {
        safe
    }
}

fn session_relative(rel: &str) -> io::Result<()> {
    if rel.is_empty()
        || Path::new(rel).is_absolute()
        || rel
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "session file path must stay inside the session directory",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn scratch() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "vibe-session-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    struct DirGuard(PathBuf);
    impl Drop for DirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn dead_pid() -> u32 {
        for pid in [2_147_483_646u32, 2_000_000_000, 1_500_000_000] {
            if pid_is_gone(pid) {
                return pid;
            }
        }
        panic!("could not find an unused pid");
    }

    fn pid_is_gone(pid: u32) -> bool {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        if pid <= 0 {
            return false;
        }
        let rc = unsafe { libc::kill(pid, 0) };
        if rc == 0 {
            return false;
        }
        io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    #[test]
    fn create_sweeps_a_dead_pid_directory_and_keeps_a_live_one() {
        let cache = DirGuard(scratch());
        let sessions = cache.0.join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let nonce = "0123456789abcdef0123456789abcdef";
        let dead = sessions.join(format!("{}-{nonce}", dead_pid()));
        let live = sessions.join(format!("{}-{nonce}", std::process::id()));
        fs::create_dir(&dead).unwrap();
        fs::create_dir(&live).unwrap();

        let session = SessionDir::create(&cache.0).unwrap();
        assert!(!dead.exists());
        assert!(live.exists());
        assert_ne!(session.host, live);
        assert!(session.share().staging_only);
        assert!(session.share().read_only);
        let mode = fs::metadata(&session.host).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        session.finish(true).unwrap();
    }

    #[test]
    fn write_returns_the_staging_guest_path_and_matches_the_host_file() {
        let cache = DirGuard(scratch());
        let mut session = SessionDir::create(&cache.0).unwrap();
        let host = session.host.clone();
        let content = "echo hi\n";
        let file = session.write("nested/hello.sh", content).unwrap();
        let expected = format!(
            "/mnt/root/virtiofs-shared/{}/nested/hello.sh",
            session.share().tag()
        );
        assert_eq!(file.as_str(), expected);
        assert_eq!(
            fs::read_to_string(host.join("nested/hello.sh")).unwrap(),
            content
        );
        let mode = fs::metadata(host.join("nested/hello.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(
            file.sh_command("hello.sh", &[crate::session::shell_word("a b")]),
            format!("sh -c \"$(cat {expected})\" hello.sh 'a b'")
        );
        // A root-shell expression is appended as written, so the root shell expands it.
        assert_eq!(
            file.sh_command("hello.sh", &["\"${VIBE_VM_HOME}\"".to_string()]),
            format!("sh -c \"$(cat {expected})\" hello.sh \"${{VIBE_VM_HOME}}\"")
        );

        let err = session.write("../escape.sh", "no").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(!cache.0.join("escape.sh").exists());

        assert_eq!(session.finish(true).unwrap(), None);
        assert!(!host.exists());
    }

    #[test]
    fn finish_removes_the_directory_only_after_a_normal_stop() {
        let cache = DirGuard(scratch());
        let kept = SessionDir::create(&cache.0).unwrap();
        let kept_path = kept.host.clone();
        assert_eq!(kept.finish(false).unwrap(), Some(kept_path.clone()));
        assert!(kept_path.is_dir());

        let removed = SessionDir::create(&cache.0).unwrap();
        let removed_path = removed.host.clone();
        assert!(kept_path.is_dir());
        assert_eq!(removed.finish(true).unwrap(), None);
        assert!(!removed_path.exists());
        assert!(kept_path.is_dir());
    }

    #[test]
    fn stage_scripts_writes_ordered_files_and_sanitizes_names() {
        let cache = DirGuard(scratch());
        let mut session = SessionDir::create(&cache.0).unwrap();
        let mut plan = crate::session_plan::SessionPlan::root(
            vec![
                LoginAction::Send("echo skip".into()),
                LoginAction::Script {
                    name: "base".into(),
                    content: "echo root\n".into(),
                    file: None,
                },
                LoginAction::UserScript {
                    name: "base".into(),
                    content: "echo user\n".into(),
                    file: None,
                },
                LoginAction::Script {
                    name: "check-vm-user-home".into(),
                    content: "echo check\n".into(),
                    file: None,
                },
            ],
            1..4,
        );
        plan.user_actions.push(LoginAction::Script {
            name: "/tmp/my script.sh".into(),
            content: "echo spaced\n".into(),
            file: None,
        });
        session.stage_scripts(&mut plan).unwrap();

        let expected = [
            ("000-base.root.sh", "echo root\n"),
            ("001-base.user.sh", "echo user\n"),
            ("002-check-vm-user-home.sh", "echo check\n"),
            ("003-my_script.sh.sh", "echo spaced\n"),
        ];
        let mut names = Vec::new();
        for (name, content) in expected {
            let host_file = session.host.join("scripts").join(name);
            assert_eq!(fs::read_to_string(&host_file).unwrap(), content, "{name}");
            assert!(!name.contains(' ') && !name.contains('/'), "{name}");
            names.push(name);
        }
        let listed = fs::read_dir(session.host.join("scripts")).unwrap();
        let mut listed: Vec<_> = listed
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        listed.sort();
        assert_eq!(listed, names);

        let files: Vec<_> = plan
            .root_actions
            .iter()
            .chain(plan.user_actions.iter())
            .filter_map(|action| match action {
                LoginAction::Script { content, file, .. }
                | LoginAction::UserScript { content, file, .. } => {
                    Some((content.as_str(), file.as_ref().unwrap().as_str()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(files.len(), expected.len());
        for ((content, guest), (name, body)) in files.iter().zip(expected) {
            assert_eq!(*content, body);
            assert!(guest.ends_with(&format!("/scripts/{name}")), "{guest}");
            assert!(!guest.contains(' '), "{guest}");
        }
        session.finish(true).unwrap();
    }

    #[test]
    fn stage_scripts_keeps_vm_user_shell_scripts_inline() {
        let cache = DirGuard(scratch());
        let mut session = SessionDir::create(&cache.0).unwrap();
        let script = |name: &str| LoginAction::Script {
            name: name.into(),
            content: "echo hi\n".into(),
            file: None,
        };
        let mut plan = crate::session_plan::SessionPlan::root(vec![script("root-part")], 0..1);
        plan.vm_user = crate::vm_user::VmUser::parse("dev").unwrap();
        plan.switch_user = true;
        plan.user_actions.push(script("--script"));
        session.stage_scripts(&mut plan).unwrap();

        let staged = |action: &LoginAction| match action {
            LoginAction::Script { file, .. } => file.is_some(),
            _ => unreachable!(),
        };
        assert!(staged(&plan.root_actions[0]));
        assert!(!staged(&plan.user_actions[0]));
        session.finish(true).unwrap();
    }
}
