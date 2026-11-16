//! The phases of an interactive session and the guest commands that mount
//! the directory shares.
//!
//! Callers describe a session with a [`SessionPlan`]; [`expand_session`]
//! turns it into the full list of login actions in a fixed order:
//!
//! 1. login: the legacy root login and internal shell setup
//! 2. root preparation: provision sets the root password and creates the
//!    `--vm-user` account; a run checks that account
//! 3. mounts: the VirtioFS staging mount, one bind mount per share, then the
//!    tmpfs masks
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

use crate::proxy::GuestProxy;
use crate::session::{LoginAction, shell_single_quote, shell_word};
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
    /// Mount the shared apt cache with `bindfs` so that only root (as `_apt`)
    /// can change it. Set for runs with a non-root `--vm-user`.
    pub(crate) restrict_apt_cache: bool,
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
            restrict_apt_cache: false,
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
        plan.restrict_apt_cache,
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
/// each share to its guest path.
///
/// For a non-root user, missing directories below HOME are created one level
/// at a time and owned by the user, so the user can still create files next
/// to a mount point. Existing directories are never changed: they may be
/// inside a share, and `chmod` there changes the host file. With
/// `restrict_apt_cache` the apt cache share is mounted with `bindfs` instead
/// of a bind mount.
pub(crate) fn mount_actions(
    shares: &[DirectoryShare],
    user: &VmUser,
    restrict_apt_cache: bool,
) -> Vec<LoginAction> {
    if shares.is_empty() {
        return Vec::new();
    }
    // `chmod -R` runs before the mount: afterwards it would reach host files.
    let mut actions = vec![LoginAction::Send(format!(
        " mkdir -p {STAGING_DIR} && chmod -R 700 /mnt/root && mount -t virtiofs {SHARED_DIRECTORIES_TAG} {STAGING_DIR}"
    ))];
    for share in shares {
        let staging = format!("{STAGING_DIR}/{}", shell_word(&share.tag()));
        if restrict_apt_cache && share.is_apt_cache() {
            actions.push(mount_apt_cache_action(&staging));
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

/// Mounts the apt cache so that `_apt` owns it and nobody else can write:
/// root's apt still downloads into the host cache, but the target user
/// cannot plant packages for root to install later. Falls back to a bind
/// mount, with a host warning, only when `bindfs` or `_apt` is missing; a
/// failing `bindfs` stops the session.
fn mount_apt_cache_action(staging: &str) -> LoginAction {
    LoginAction::StatusCommand {
        label: "mount apt cache".to_string(),
        command: format!(
            "mkdir -p {APT_CACHE_GUEST} && if command -v bindfs >/dev/null && __vibe_apt_uid=$(id -u _apt 2>/dev/null); then bindfs -o nonempty -u \"$__vibe_apt_uid\" -g 0 -p 0755,af-x {staging} {APT_CACHE_GUEST}/; else echo 'Warning: bindfs is not available; the shared apt cache is writable by every user in this VM.' >&2; __vibe_notice={APT_NO_BINDFS_NOTICE}; mount --bind {staging} {APT_CACHE_GUEST}; fi"
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

/// Installs `bindfs` during provision when the image does not have it, so the
/// image can later be run with a non-root `--vm-user`. In `vznat` mode the
/// guest proxy is passed to apt directly, because the base script saves it
/// in the apt configuration only later.
pub(crate) fn install_bindfs_action(guest_proxy: &Option<GuestProxy>) -> LoginAction {
    let apt_options = guest_proxy
        .as_ref()
        .map(|guest| {
            let url = shell_single_quote(&guest.apt_url);
            format!("-o Acquire::http::Proxy='{url}' -o Acquire::https::Proxy='{url}' ")
        })
        .unwrap_or_default();
    LoginAction::StatusCommand {
        label: "install bindfs".to_string(),
        command: format!(
            "command -v bindfs >/dev/null || {{ export DEBIAN_FRONTEND=noninteractive; apt-get {apt_options}install --yes bindfs || {{ apt-get {apt_options}update && apt-get {apt_options}install --yes bindfs; }}; }}"
        ),
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
        }
    }

    #[test]
    fn root_mounts_use_the_private_staging_directory() {
        let apt = share(GuestPath::new(APT_CACHE_GUEST), "/cache/apt");
        let cargo = share(GuestPath::new(".cargo"), "/cache/cargo");
        let rendered: Vec<String> =
            mount_actions(&[apt.clone(), cargo.clone()], &VmUser::root(), false)
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
        assert!(mount_actions(&[], &VmUser::root(), true).is_empty());
    }

    #[test]
    fn other_users_get_owned_home_dirs_and_a_bindfs_apt_cache() {
        let dev = VmUser::parse("dev").unwrap();
        let apt = share(GuestPath::new(APT_CACHE_GUEST), "/cache/apt");
        let mise = share(GuestPath::new(".local/share/mise"), "/cache/mise-cache");
        let actions = mount_actions(&[apt.clone(), mise.clone()], &dev, true);
        let rendered: Vec<String> = actions.iter().map(render).collect();
        let apt_staging = format!("/mnt/root/virtiofs-shared/{}", apt.tag());
        assert!(
            rendered[1]
                .starts_with("[mount apt cache] mkdir -p /var/cache/apt && if command -v bindfs")
        );
        assert!(rendered[1].contains(&format!(
            "bindfs -o nonempty -u \"$__vibe_apt_uid\" -g 0 -p 0755,af-x {apt_staging} /var/cache/apt/; else"
        )));
        assert!(rendered[1].contains("__vibe_notice=apt-no-bindfs; mount --bind"));
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

        // Without the restriction (provision) the apt cache is a plain bind mount.
        let plain: Vec<String> = mount_actions(&[apt], &dev, false)
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
            restrict_apt_cache: false,
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
        let plain = render(&install_bindfs_action(&None));
        assert_eq!(
            plain,
            "[install bindfs] command -v bindfs >/dev/null || { export DEBIAN_FRONTEND=noninteractive; apt-get install --yes bindfs || { apt-get update && apt-get install --yes bindfs; }; }"
        );
        let proxied = render(&install_bindfs_action(&Some(GuestProxy {
            url: "socks5://10.0.0.1:1080".to_string(),
            apt_url: "socks5h://10.0.0.1:1080".to_string(),
            no_proxy: String::new(),
        })));
        assert!(proxied.contains(
            "apt-get -o Acquire::http::Proxy='socks5h://10.0.0.1:1080' -o Acquire::https::Proxy='socks5h://10.0.0.1:1080' update"
        ));
    }
}
