//! The phases of an interactive session and the guest commands that mount
//! the directory shares.
//!
//! Callers describe a session with a [`SessionPlan`]; [`expand_session`]
//! turns it into the full list of login actions in a fixed order:
//!
//! 1. login: the legacy root login and internal shell setup
//! 2. root preparation: provision sets the root password and creates the
//!    `--vm-user` account; a run checks that account
//! 3. mounts: the VirtioFS staging mount, one bind mount per share (a
//!    staging-only share has no bind mount), then the tmpfs masks
//! 4. root actions: tmux, `.profile`, SSH key, provisioning scripts, ...
//! 5. switch: with `switch_user` and a non-root target user, `su --login
//!    --pty` into the user's login shell
//! 6. user actions: `cd`, `--env`, MOTD, and the caller's login actions
//! 7. trailer: mise activation and fzf key bindings
//!
//! Without the switch every action runs in the root shell, and the target
//! user only changes where paths below HOME point. After the switch the user
//! actions and the trailer run in the target user's shell; exiting it returns
//! to the root shell.

use std::{ops::Range, time::Duration};

use crate::guest_template::{replace_region, shell_assign};
use crate::proxy::GuestProxy;
use crate::session::{LoginAction, shell_word};
use crate::session_dir::GuestFile;
use crate::shares::DirectoryShare;
use crate::vm_config::SHARED_DIRECTORIES_TAG;
use crate::vm_user::{GuestPath, VmUser};

/// Where the VirtioFS device is mounted. `/mnt/root` is made `0700` before
/// the mount, so only root can reach the shares through it; users see each
/// share only at its bind mount.
pub(crate) const STAGING_DIR: &str = "/mnt/root/virtiofs-shared";
pub(crate) const APT_CACHE_GUEST: &str = "/var/cache/apt";
/// Notice set by the apt cache mount when `bindfs` is missing.
const APT_NO_BINDFS_NOTICE: &str = "apt-no-bindfs";
/// Prefix of the notice set by the vm-user check for privileged groups.
const VM_USER_GROUPS_NOTICE: &str = "groups=";
const MOUNT_APT_CACHE_TIMEOUT: Duration = Duration::from_secs(120);
const INSTALL_BINDFS_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const SWITCH_USER_TIMEOUT: Duration = Duration::from_secs(120);

/// What a caller wants to run in a session, grouped by phase. See the module
/// documentation for the order.
#[derive(Debug, Clone)]
pub(crate) struct SessionPlan {
    pub(crate) vm_user: VmUser,
    pub(crate) root_prepare: Vec<LoginAction>,
    /// Directories hidden behind an empty read-only tmpfs after the mounts.
    pub(crate) masks: Vec<GuestPath>,
    /// Script that mounts the shared apt cache with `bindfs`, so that only
    /// root (as `_apt`) can change it. Set for a run with a non-root
    /// `--vm-user` when the apt share is mounted. `None` uses a plain bind mount.
    pub(crate) apt_cache_script: Option<GuestFile>,
    pub(crate) root_actions: Vec<LoginAction>,
    pub(crate) user_actions: Vec<LoginAction>,
    /// The actions the user wrote, as a range of `root_actions` followed by
    /// `user_actions`. Errors number them `[action N: ...]` from 1; Vibe's
    /// own actions after them report as `[shutdown] ...`.
    pub(crate) numbered: Range<usize>,
    /// Warn on the host when the image has no root password.
    pub(crate) warn_passwordless_root: bool,
    /// Run `user_actions` in a login shell of a non-root `vm_user`, entered
    /// after `root_actions`. Ignored for root.
    pub(crate) switch_user: bool,
}

impl SessionPlan {
    /// A root session that runs `actions` after the mounts.
    pub(crate) fn root(actions: Vec<LoginAction>, numbered: Range<usize>) -> Self {
        Self {
            vm_user: VmUser::root(),
            root_prepare: Vec::new(),
            masks: Vec::new(),
            apt_cache_script: None,
            root_actions: actions,
            user_actions: Vec::new(),
            numbered,
            warn_passwordless_root: false,
            switch_user: false,
        }
    }
}

/// The full action list of a session.
pub(crate) struct ExpandedSession {
    pub(crate) actions: Vec<LoginAction>,
    /// The root and user actions of the plan.
    pub(crate) caller: Range<usize>,
    /// The actions the user wrote; a sub-range of `caller`.
    pub(crate) user: Range<usize>,
}

/// Puts `login` and the phases of `plan` in order. `shares` are the
/// directory shares of the VM, mounted in the given order.
pub(crate) fn expand_session(
    login: Vec<LoginAction>,
    plan: &SessionPlan,
    shares: &[DirectoryShare],
) -> ExpandedSession {
    let mut actions = login;
    actions.extend(plan.root_prepare.iter().cloned());
    actions.extend(mount_actions(
        shares,
        &plan.vm_user,
        plan.apt_cache_script.as_ref(),
    ));
    actions.extend(mask_actions(&plan.masks, &plan.vm_user));

    let switch = if plan.switch_user {
        switch_user_actions(&plan.vm_user)
    } else {
        Vec::new()
    };
    // `numbered` counts positions in root_actions ++ user_actions; positions
    // in user_actions (and an empty range right after root_actions) move past
    // the switch actions, so the switch reports as `[login] setup`.
    let root_len = plan.root_actions.len();
    let starts_after_root = plan.numbered.start >= root_len;
    let start = plan.numbered.start + if starts_after_root { switch.len() } else { 0 };
    let end = plan.numbered.end
        + if starts_after_root || plan.numbered.end > root_len {
            switch.len()
        } else {
            0
        };

    let caller_start = actions.len();
    actions.extend(plan.root_actions.iter().cloned());
    actions.extend(switch);
    actions.extend(plan.user_actions.iter().cloned());
    let caller = caller_start..actions.len();
    let user = caller_start + start.min(caller.len())..caller_start + end.min(caller.len());

    actions.push(LoginAction::Send(
        " [ -x \"${HOME}/.local/bin/mise\" ] && eval \"$(\"${HOME}/.local/bin/mise\" activate bash)\""
            .to_string(),
    ));
    actions.push(LoginAction::Send(
        " command -v fzf >/dev/null && eval \"$(fzf --bash)\"".to_string(),
    ));

    ExpandedSession {
        actions,
        caller,
        user,
    }
}

/// Enters a login shell of a non-root `user` (see [`LoginAction::SwitchUser`]).
/// Typed input cannot be pushed back into the root shell's terminal: the
/// switch first disables `TIOCSTI` where the kernel still allows it. Empty
/// for root.
pub(crate) fn switch_user_actions(user: &VmUser) -> Vec<LoginAction> {
    if user.is_root() {
        return Vec::new();
    }
    vec![
        LoginAction::Send(" sysctl -q -w dev.tty.legacy_tiocsti=0 2>/dev/null || true".to_string()),
        LoginAction::SwitchUser {
            user: user.clone(),
            timeout: SWITCH_USER_TIMEOUT,
        },
    ]
}

/// Mounts the VirtioFS device below a root-only directory and bind-mounts
/// each share to its guest path. A staging-only share is not bind-mounted;
/// root reads it under the staging directory. The staging mount still runs
/// when that share is the only one.
///
/// For a non-root user, missing directories below HOME are created one level
/// at a time and owned by the user, so the user can still create files next
/// to a mount point. Existing directories are never changed: they may be
/// inside a share, and `chmod` there changes the host file. When
/// `apt_cache_script` is set, the apt cache share is mounted by that script
/// instead of a bind mount.
pub(crate) fn mount_actions(
    shares: &[DirectoryShare],
    user: &VmUser,
    apt_cache_script: Option<&GuestFile>,
) -> Vec<LoginAction> {
    if shares.is_empty() {
        return Vec::new();
    }
    // `chmod -R` runs before the mount: afterwards it would reach host files.
    let mut actions = vec![LoginAction::Send(format!(
        " mkdir -p {STAGING_DIR} && chmod -R 700 /mnt/root && mount -t virtiofs {SHARED_DIRECTORIES_TAG} {STAGING_DIR}"
    ))];
    for share in shares {
        if share.staging_only {
            continue;
        }
        let staging = format!("{STAGING_DIR}/{}", shell_word(&share.tag()));
        if let Some(file) = apt_cache_script.filter(|_| share.is_apt_cache()) {
            actions.push(mount_apt_cache_action(file));
            continue;
        }
        let target = share.guest.to_shell(user);
        let home_dirs = share.guest.home_dirs(user);
        if home_dirs.is_empty() {
            actions.push(LoginAction::Send(format!(" mkdir -p {target}")));
        } else {
            let create = home_dirs
                .iter()
                .map(|dir| {
                    let dir = dir.to_shell(user);
                    format!(
                        "[ -d {dir} ] || install -d -o \"$VIBE_VM_UID\" -g \"$VIBE_VM_GID\" {dir}"
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            actions.push(LoginAction::Send(format!(" {create}")));
        }
        actions.push(LoginAction::Send(format!(
            " mount --bind {staging} {target}"
        )));
    }
    actions
}

const MOUNT_APT_CACHE_SCRIPT: &str = include_str!("guest/mount_apt_cache.sh");

/// Renders [`MOUNT_APT_CACHE_SCRIPT`]. `staging` is the apt share's path
/// under the VirtioFS staging directory.
pub(crate) fn render_mount_apt_cache(staging: &str) -> String {
    let body = format!(
        "{}\n{}\n{}\n",
        shell_assign("APT_CACHE_DIR", APT_CACHE_GUEST),
        shell_assign("STAGING_DIR", staging),
        shell_assign("NO_BINDFS_NOTICE", APT_NO_BINDFS_NOTICE),
    );
    replace_region(
        MOUNT_APT_CACHE_SCRIPT,
        "INJECT_MOUNT_APT_CACHE_CONFIG",
        &body,
    )
    .expect("mount_apt_cache.sh has region INJECT_MOUNT_APT_CACHE_CONFIG")
}

/// Mounts the apt cache so that `_apt` owns it and nobody else can write:
/// root's apt still downloads into the host cache, but the target user
/// cannot plant packages for root to install later. The script falls back
/// to a bind mount, and prints the notice word, only when `bindfs` or
/// `_apt` is missing; a failing `bindfs` stops the session.
fn mount_apt_cache_action(file: &GuestFile) -> LoginAction {
    LoginAction::StatusCommand {
        label: "mount apt cache".to_string(),
        command: format!(
            "__vibe_notice=$({})",
            file.sh_command("mount_apt_cache.sh", &[])
        ),
        timeout: MOUNT_APT_CACHE_TIMEOUT,
        vm_user: None,
    }
}

/// Hides each directory behind an empty tmpfs that only root can write.
pub(crate) fn mask_actions(masks: &[GuestPath], user: &VmUser) -> Vec<LoginAction> {
    masks
        .iter()
        .map(|mask| {
            LoginAction::Send(format!(
                " mount -t tmpfs -o mode=0555,uid=0,gid=0 tmpfs {}",
                mask.to_shell(user)
            ))
        })
        .collect()
}

const INSTALL_BINDFS_SCRIPT: &str = include_str!("guest/install_bindfs.sh");

/// Renders [`INSTALL_BINDFS_SCRIPT`]. An empty proxy leaves apt with no
/// proxy options. In `vznat` mode the guest proxy is passed to apt directly,
/// because the base script saves it in the apt configuration only later.
pub(crate) fn render_install_bindfs(guest_proxy: &Option<GuestProxy>) -> String {
    let url = guest_proxy
        .as_ref()
        .map(|guest| guest.apt_url.as_str())
        .unwrap_or("");
    let body = format!("{}\n", shell_assign("APT_PROXY_URL", url));
    replace_region(INSTALL_BINDFS_SCRIPT, "INJECT_INSTALL_BINDFS_CONFIG", &body)
        .expect("install_bindfs.sh has region INJECT_INSTALL_BINDFS_CONFIG")
}

/// Installs `bindfs` during provision when the image does not have it, so the
/// image can later be run with a non-root `--vm-user`. `file` is the rendered
/// [`INSTALL_BINDFS_SCRIPT`] in the session directory.
pub(crate) fn install_bindfs_action(file: &GuestFile) -> LoginAction {
    LoginAction::StatusCommand {
        label: "install bindfs".to_string(),
        command: file.sh_command("install_bindfs.sh", &[]),
        timeout: INSTALL_BINDFS_TIMEOUT,
        vm_user: None,
    }
}

/// Host warning for a notice word set by a guest step. Unknown notices are
/// ignored.
pub(crate) fn notice_message(notice: &str, user: &VmUser) -> Option<String> {
    let name = user.as_str();
    if notice == APT_NO_BINDFS_NOTICE {
        return Some(format!(
            "Warning: bindfs is not installed in this image, so the shared apt cache ({APT_CACHE_GUEST}) is mounted writable. \
User '{name}' can change the host's apt cache (for example archives/*.deb), and a later `apt install` as root may install tampered packages. \
Recreate the image with `vibe provision --base <image> --image <new-name>` (provision installs bindfs), or run with --no-mount-sys-cache."
        ));
    }
    let groups = notice.strip_prefix(VM_USER_GROUPS_NOTICE)?;
    Some(format!(
        "Warning: vm-user '{name}' is in the group(s) {}, which usually grant root access.",
        groups.replace(',', ", ")
    ))
}

/// Host warning for a non-root session on an image whose root account has
/// no password.
pub(crate) fn passwordless_root_warning(user: &VmUser) -> String {
    format!(
        "Warning: root has no password in this image; user '{}' can become root with 'su -'. \
Set one when creating the image with `vibe provision --root-password ...`.",
        user.as_str()
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    pub(crate) fn render(action: &LoginAction) -> String {
        match action {
            LoginAction::Send(text) => text.clone(),
            LoginAction::StatusCommand { label, command, .. } => {
                format!("[{label}] {command}")
            }
            other => format!("<{}>", other.label()),
        }
    }

    fn share(guest: GuestPath, tag_host: &str) -> DirectoryShare {
        DirectoryShare {
            host: PathBuf::from(tag_host),
            guest,
            read_only: false,
            is_project_dir: false,
            staging_only: false,
        }
    }

    #[test]
    fn root_mounts_use_the_private_staging_directory() {
        let apt = share(GuestPath::new(APT_CACHE_GUEST), "/cache/apt");
        let cargo = share(GuestPath::new(".cargo"), "/cache/cargo");
        let rendered: Vec<String> =
            mount_actions(&[apt.clone(), cargo.clone()], &VmUser::root(), None)
                .iter()
                .map(render)
                .collect();
        assert_eq!(
            rendered,
            [
                " mkdir -p /mnt/root/virtiofs-shared && chmod -R 700 /mnt/root && mount -t virtiofs shared /mnt/root/virtiofs-shared".to_string(),
                " mkdir -p /var/cache/apt".to_string(),
                format!(" mount --bind /mnt/root/virtiofs-shared/{} /var/cache/apt", apt.tag()),
                " mkdir -p /root/.cargo".to_string(),
                format!(" mount --bind /mnt/root/virtiofs-shared/{} /root/.cargo", cargo.tag()),
            ]
        );
        assert!(mount_actions(&[], &VmUser::root(), None).is_empty());
    }

    #[test]
    fn a_staging_only_share_has_no_bind_mount() {
        let session = DirectoryShare {
            host: PathBuf::from("/cache/sessions/example"),
            guest: GuestPath::new(STAGING_DIR),
            read_only: true,
            is_project_dir: false,
            staging_only: true,
        };
        let apt = share(GuestPath::new(APT_CACHE_GUEST), "/cache/apt");
        let mixed: Vec<String> =
            mount_actions(&[apt.clone(), session.clone()], &VmUser::root(), None)
                .iter()
                .map(render)
                .collect();
        assert!(mixed[0].contains("mount -t virtiofs"));
        assert_eq!(mixed.len(), 3);
        assert!(mixed[2].contains(&apt.tag()));
        assert!(!mixed.iter().any(|line| line.contains(&session.tag())));

        let only: Vec<String> = mount_actions(&[session], &VmUser::root(), None)
            .iter()
            .map(render)
            .collect();
        assert_eq!(only.len(), 1);
        assert!(only[0].contains("mount -t virtiofs shared"));
        assert!(!only[0].contains("mount --bind"));
    }

    #[test]
    fn other_users_get_owned_home_dirs_and_a_bindfs_apt_cache() {
        let dev = VmUser::parse("dev").unwrap();
        let apt = share(GuestPath::new(APT_CACHE_GUEST), "/cache/apt");
        let mise = share(GuestPath::new(".local/share/mise"), "/cache/mise-cache");
        let apt_staging = format!("/mnt/root/virtiofs-shared/{}", apt.tag());
        let script = render_mount_apt_cache(&apt_staging);
        assert!(
            script.contains(&format!("STAGING_DIR='{apt_staging}'")),
            "{script}"
        );
        let cache = DirGuard(scratch());
        let mut session = crate::session_dir::SessionDir::create(&cache.0).unwrap();
        let file = session.write("mount_apt_cache.sh", &script).unwrap();
        let actions = mount_actions(&[apt.clone(), mise.clone()], &dev, Some(&file));
        let rendered: Vec<String> = actions.iter().map(render).collect();
        assert!(
            rendered[1].starts_with("[mount apt cache] __vibe_notice=$(sh -c \"$(cat "),
            "{rendered:?}"
        );
        assert!(rendered[1].contains(file.as_str()), "{rendered:?}");
        assert_eq!(
            rendered[2],
            " [ -d \"${VIBE_VM_HOME}\"/.local ] || install -d -o \"$VIBE_VM_UID\" -g \"$VIBE_VM_GID\" \"${VIBE_VM_HOME}\"/.local; \
[ -d \"${VIBE_VM_HOME}\"/.local/share ] || install -d -o \"$VIBE_VM_UID\" -g \"$VIBE_VM_GID\" \"${VIBE_VM_HOME}\"/.local/share; \
[ -d \"${VIBE_VM_HOME}\"/.local/share/mise ] || install -d -o \"$VIBE_VM_UID\" -g \"$VIBE_VM_GID\" \"${VIBE_VM_HOME}\"/.local/share/mise"
        );
        assert_eq!(
            rendered[3],
            format!(
                " mount --bind /mnt/root/virtiofs-shared/{} \"${{VIBE_VM_HOME}}\"/.local/share/mise",
                mise.tag()
            )
        );
        assert_eq!(rendered.len(), 4);

        // Without the script (provision) the apt cache is a plain bind mount.
        let plain: Vec<String> = mount_actions(&[apt], &dev, None)
            .iter()
            .map(render)
            .collect();
        assert_eq!(plain[1], " mkdir -p /var/cache/apt");
        assert_eq!(
            plain[2],
            format!(" mount --bind {apt_staging} /var/cache/apt")
        );
    }

    #[test]
    fn masks_are_read_only_empty_tmpfs() {
        let masks = [GuestPath::new("proj/.vibe"), GuestPath::new("proj/.git")];
        let rendered: Vec<String> = mask_actions(&masks, &VmUser::root())
            .iter()
            .map(render)
            .collect();
        assert_eq!(
            rendered,
            [
                " mount -t tmpfs -o mode=0555,uid=0,gid=0 tmpfs /root/proj/.vibe",
                " mount -t tmpfs -o mode=0555,uid=0,gid=0 tmpfs /root/proj/.git",
            ]
        );
    }

    #[test]
    fn phases_are_expanded_in_order_and_number_only_user_actions() {
        let send = |text: &str| LoginAction::Send(text.to_string());
        let plan = SessionPlan {
            vm_user: VmUser::parse("dev").unwrap(),
            root_prepare: vec![send(" prepare")],
            masks: vec![GuestPath::new("p/.vibe")],
            apt_cache_script: None,
            root_actions: vec![send(" root")],
            user_actions: vec![send(" cd"), send(" mine")],
            numbered: 2..3,
            warn_passwordless_root: true,
            switch_user: false,
        };
        let shares = [share(GuestPath::new("p"), "/host/p")];
        let expanded = expand_session(vec![send(" login")], &plan, &shares);
        let rendered: Vec<String> = expanded.actions.iter().map(render).collect();
        assert_eq!(rendered[0], " login");
        assert_eq!(rendered[1], " prepare");
        assert!(rendered[2].contains("mount -t virtiofs"));
        assert!(rendered[4].starts_with(" mount --bind"));
        assert!(rendered[5].contains("tmpfs \"${VIBE_VM_HOME}\"/p/.vibe"));
        assert_eq!(&rendered[6..9], [" root", " cd", " mine"]);
        assert!(rendered[9].contains("mise"));
        assert!(rendered[10].contains("fzf"));
        assert_eq!(expanded.caller, 6..9);
        assert_eq!(expanded.user, 8..9);
    }

    #[test]
    fn the_switch_comes_between_root_and_user_actions() {
        let send = |text: &str| LoginAction::Send(text.to_string());
        let dev = VmUser::parse("dev").unwrap();
        let plan = SessionPlan {
            vm_user: dev.clone(),
            switch_user: true,
            user_actions: vec![send(" cd"), send(" mine")],
            ..SessionPlan::root(vec![send(" root")], 2..3)
        };
        let expanded = expand_session(vec![send(" login")], &plan, &[]);
        let rendered: Vec<String> = expanded.actions.iter().map(render).collect();
        assert_eq!(rendered[1], " root");
        assert!(rendered[2].starts_with(" sysctl -q -w dev.tty.legacy_tiocsti=0"));
        assert_eq!(rendered[3], "<switch to vm-user>");
        assert_eq!(&rendered[4..6], [" cd", " mine"]);
        assert!(rendered[6].contains("mise"));
        assert_eq!(expanded.caller, 1..6);
        assert_eq!(expanded.user, 5..6);

        // An empty range right after the root actions also moves past the
        // switch, which then reports as setup.
        let repl = SessionPlan {
            numbered: 1..1,
            user_actions: Vec::new(),
            ..plan.clone()
        };
        let expanded = expand_session(Vec::new(), &repl, &[]);
        assert_eq!(expanded.user, 3..3);
        // Numbered root actions (provision scripts) keep their positions.
        let scripts = SessionPlan {
            numbered: 0..1,
            ..repl.clone()
        };
        assert_eq!(expand_session(Vec::new(), &scripts, &[]).user, 0..1);

        // Root never switches.
        let root = SessionPlan {
            vm_user: VmUser::root(),
            ..plan
        };
        let rendered: Vec<String> = expand_session(Vec::new(), &root, &[])
            .actions
            .iter()
            .map(render)
            .collect();
        assert_eq!(&rendered[..3], [" root", " cd", " mine"]);
    }

    #[test]
    fn bindfs_install_uses_the_guest_proxy_only_in_vznat_mode() {
        let plain = render_install_bindfs(&None);
        assert!(plain.contains("APT_PROXY_URL=''\n"), "{plain}");
        let proxied = render_install_bindfs(&Some(GuestProxy {
            url: "socks5://10.0.0.1:1080".to_string(),
            apt_url: "socks5h://10.0.0.1:1080".to_string(),
            no_proxy: String::new(),
        }));
        assert!(
            proxied.contains("APT_PROXY_URL='socks5h://10.0.0.1:1080'\n"),
            "{proxied}"
        );
    }

    #[test]
    fn install_bindfs_script_retries_install_and_skips_apt_when_bindfs_exists() {
        let proxy = Some(GuestProxy {
            url: "socks5://10.0.0.1:1080".to_string(),
            apt_url: "socks5h://10.0.0.1:1080".to_string(),
            no_proxy: String::new(),
        });
        let log = run_install_bindfs(&render_install_bindfs(&proxy), false);
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 3, "{log}");
        for line in &lines {
            assert!(
                line.contains("Acquire::http::Proxy=socks5h://10.0.0.1:1080"),
                "{line}"
            );
            assert!(
                line.contains("Acquire::https::Proxy=socks5h://10.0.0.1:1080"),
                "{line}"
            );
        }
        assert!(lines[0].contains(" install "), "{log}");
        assert!(lines[1].contains(" update"), "{log}");
        assert!(lines[2].contains(" install "), "{log}");

        let skipped = run_install_bindfs(&render_install_bindfs(&proxy), true);
        assert_eq!(skipped, "", "{skipped}");

        let plain = run_install_bindfs(&render_install_bindfs(&None), false);
        assert!(!plain.contains("Acquire::"), "{plain}");
        let plain_lines: Vec<&str> = plain.lines().collect();
        assert_eq!(plain_lines.len(), 3, "{plain}");
    }

    /// Runs the rendered install script. `with_bindfs` puts a stub `bindfs` on `PATH`.
    /// The apt-get stub fails the first `install`. Returns the apt-get argument log.
    fn run_install_bindfs(script: &str, with_bindfs: bool) -> String {
        let dir = DirGuard(scratch());
        let bin = dir.0.join("bin");
        fs::create_dir(&bin).unwrap();
        let log = dir.0.join("apt.log");
        let count = dir.0.join("install.count");
        stub(
            &bin.join("apt-get"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\ninstall=0\nfor arg in \"$@\"; do\n  [ \"$arg\" = install ] && install=1\ndone\nif [ \"$install\" -eq 1 ]; then\n  n=0\n  [ -f {count} ] && n=$(cat {count})\n  n=$((n + 1))\n  printf '%s' \"$n\" > {count}\n  [ \"$n\" -eq 1 ] && exit 1\nfi\nexit 0\n",
                log = shell_word(&log.to_string_lossy()),
                count = shell_word(&count.to_string_lossy()),
            ),
        );
        if with_bindfs {
            stub(&bin.join("bindfs"), "#!/bin/sh\nexit 0\n");
        }
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "status {:?}\nstderr {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        fs::read_to_string(&log).unwrap_or_default()
    }

    fn stub(path: &std::path::Path, body: &str) {
        fs::write(path, body).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn mount_apt_cache_script_uses_bindfs_or_falls_back() {
        let staging = "/cache/apt-staging";
        let script = render_mount_apt_cache(staging);
        assert!(
            script.contains(&format!("STAGING_DIR='{staging}'")),
            "{script}"
        );

        let mounted = run_mount_apt_cache(&script, MountStubs::Bindfs { hang: false });
        assert_eq!(mounted.stdout, "");
        assert_eq!(
            mounted.bindfs_args,
            [
                "-o".to_string(),
                "nonempty".to_string(),
                "-u".to_string(),
                "42".to_string(),
                "-g".to_string(),
                "0".to_string(),
                "-p".to_string(),
                "0755,af-x".to_string(),
                mounted.staging_dir.clone(),
                format!("{}/", mounted.apt_dir),
            ]
        );
        assert!(mounted.mount_args.is_empty(), "{mounted:?}");

        let hung = run_mount_apt_cache(&script, MountStubs::Bindfs { hang: true });
        assert_eq!(hung.stdout, "");
        assert!(hung.elapsed.as_secs() < 3, "waited {:?}", hung.elapsed);

        let no_bindfs = run_mount_apt_cache(&script, MountStubs::NoBindfs);
        // `$(...)` drops the trailing newline. The host reads the notice word.
        assert_eq!(no_bindfs.stdout, "apt-no-bindfs");
        assert_eq!(
            no_bindfs.mount_args,
            [
                "--bind".to_string(),
                no_bindfs.staging_dir.clone(),
                no_bindfs.apt_dir.clone(),
            ]
        );
        assert!(
            no_bindfs.stderr.contains("bindfs is not available"),
            "{}",
            no_bindfs.stderr
        );

        let no_apt = run_mount_apt_cache(&script, MountStubs::NoAptUser);
        assert_eq!(no_apt.stdout, "apt-no-bindfs");
        assert!(no_apt.bindfs_args.is_empty(), "{no_apt:?}");
        assert_eq!(no_apt.mount_args[0], "--bind");
    }

    #[derive(Clone, Copy)]
    enum MountStubs {
        Bindfs { hang: bool },
        NoBindfs,
        NoAptUser,
    }

    #[derive(Debug)]
    struct MountRun {
        stdout: String,
        stderr: String,
        bindfs_args: Vec<String>,
        mount_args: Vec<String>,
        apt_dir: String,
        staging_dir: String,
        elapsed: std::time::Duration,
    }

    fn run_mount_apt_cache(script: &str, stubs: MountStubs) -> MountRun {
        let dir = DirGuard(scratch());
        let bin = dir.0.join("bin");
        let apt_dir = dir.0.join("apt");
        let staging = dir.0.join("staging");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&staging).unwrap();
        let staging_text = staging.display().to_string();
        let apt_text = apt_dir.display().to_string();
        let script = script.replace(
            &shell_assign("STAGING_DIR", "/cache/apt-staging"),
            &shell_assign("STAGING_DIR", &staging_text),
        );
        let script = script.replace(
            &shell_assign("APT_CACHE_DIR", APT_CACHE_GUEST),
            &shell_assign("APT_CACHE_DIR", &apt_text),
        );
        let bindfs_log = dir.0.join("bindfs.args");
        let mount_log = dir.0.join("mount.args");
        let pid_file = dir.0.join("bindfs.pid");
        let hang = matches!(stubs, MountStubs::Bindfs { hang: true });
        if !matches!(stubs, MountStubs::NoBindfs) {
            let hold = if hang {
                format!(
                    "sleep 30 &\necho $! > {}\n",
                    shell_word(&pid_file.to_string_lossy())
                )
            } else {
                String::new()
            };
            stub(
                &bin.join("bindfs"),
                &format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n{hold}exit 0\n",
                    shell_word(&bindfs_log.to_string_lossy())
                ),
            );
        }
        let id_body = if matches!(stubs, MountStubs::NoAptUser) {
            "#!/bin/sh\nexit 1\n"
        } else {
            "#!/bin/sh\n[ \"$1\" = -u ] && [ \"$2\" = _apt ] && echo 42 && exit 0\nexit 1\n"
        };
        stub(&bin.join("id"), id_body);
        stub(
            &bin.join("mount"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nexit 0\n",
                shell_word(&mount_log.to_string_lossy())
            ),
        );
        let script_path = dir.0.join("mount_apt_cache.sh");
        fs::write(&script_path, &script).unwrap();
        let err_path = dir.0.join("err");
        let started = std::time::Instant::now();
        let mut child = std::process::Command::new("bash")
            .arg("-c")
            .arg("notice=$(sh \"$1\" 2>\"$2\"); printf '%s' \"$notice\"")
            .arg("mount-test")
            .arg(&script_path)
            .arg(&err_path)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let finished = wait_ms(&mut child, 3000);
        let elapsed = started.elapsed();
        if !finished {
            let _ = child.kill();
            let _ = child.wait();
            kill_recorded(&pid_file);
            panic!("mount script did not return within 3s");
        }
        let output = child.wait_with_output().unwrap();
        kill_recorded(&pid_file);
        assert!(
            output.status.success(),
            "status {:?}\nstderr {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        MountRun {
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: fs::read_to_string(&err_path).unwrap_or_default(),
            bindfs_args: read_lines(&bindfs_log),
            mount_args: read_lines(&mount_log),
            apt_dir: apt_text,
            staging_dir: staging_text,
            elapsed,
        }
    }

    fn read_lines(path: &std::path::Path) -> Vec<String> {
        fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn kill_recorded(pid_file: &std::path::Path) {
        let Ok(text) = fs::read_to_string(pid_file) else {
            return;
        };
        let Ok(pid) = text.trim().parse::<i32>() else {
            return;
        };
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }

    fn wait_ms(child: &mut std::process::Child, ms: u64) -> bool {
        let start = std::time::Instant::now();
        loop {
            if child.try_wait().unwrap().is_some() {
                return true;
            }
            if start.elapsed() >= std::time::Duration::from_millis(ms) {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    fn scratch() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "vibe-guest-script-{}-{}",
            std::process::id(),
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
}
