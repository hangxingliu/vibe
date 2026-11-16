//! Host directories shared with the VM: cache shares, project masks, and
//! the host log of what a VM gets from the host.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::session_plan;
use crate::vm_user::{GuestPath, VmUser};

/// Whose cache a host cache directory holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheScope {
    /// One directory for every `--vm-user`: `<cache dir>/<name>`.
    Global,
    /// One directory per `--vm-user`: `<cache dir>/user-<user>/<name>`.
    User,
}

/// A host cache directory shared with the VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CacheShare {
    pub(crate) name: &'static str,
    pub(crate) scope: CacheScope,
    /// A relative path is below the HOME of the `--vm-user`.
    pub(crate) guest: &'static str,
}

impl CacheShare {
    pub(crate) const fn global(name: &'static str, guest: &'static str) -> Self {
        Self {
            name,
            scope: CacheScope::Global,
            guest,
        }
    }

    pub(crate) const fn user(name: &'static str, guest: &'static str) -> Self {
        Self {
            name,
            scope: CacheScope::User,
            guest,
        }
    }

    pub(crate) fn host_dir(&self, cache_dir: &Path, vm_user: &VmUser) -> PathBuf {
        match self.scope {
            CacheScope::Global => cache_dir.join(self.name),
            CacheScope::User => user_cache_dir(cache_dir, vm_user).join(self.name),
        }
    }
}

/// The host directory that holds the user caches of `vm_user`.
pub(crate) fn user_cache_dir(cache_dir: &Path, vm_user: &VmUser) -> PathBuf {
    cache_dir.join(format!("user-{}", vm_user.as_str()))
}

/// Mounted when running and provisioning unless `--no-mount-sys-cache`.
pub(crate) const SYS_CACHE_SHARES: &[CacheShare] = &[
    CacheShare::global("apt", session_plan::APT_CACHE_GUEST),
    CacheShare::user("cargo", ".cargo"),
    CacheShare::user("yarn", ".yarn"),
    CacheShare::user("m2", ".m2"),
    CacheShare::user("go", "go"),
];

/// Mounted when running and provisioning unless `--no-mount-mise`.
pub(crate) const MISE_CACHE_SHARE: CacheShare = CacheShare::user("mise-cache", ".local/share/mise");

/// Only mounted when running with `--mount-agents-conf`.
pub(crate) const AGENT_CONF_SHARES: &[CacheShare] = &[
    CacheShare::user("codex", ".codex"),
    CacheShare::user("copilot", ".copilot"),
    CacheShare::user("claude", ".claude"),
    CacheShare::user("gemini", ".gemini"),
    CacheShare::user("pi", ".pi"),
    CacheShare::user("grok", ".grok"),
    CacheShare::user("opencode", ".config/opencode"),
];

pub(crate) const RUN_NO_MOUNT_MISE_NOTE: &str = "Note: --no-mount-mise: the shared mise directory is not mounted; tools that mise installed into it during provisioning are not available in this VM.";
pub(crate) const RUN_NO_MOUNT_SYS_CACHE_NOTE: &str = "Note: --no-mount-sys-cache: the shared apt, cargo, yarn, m2, and go caches are not mounted; tools installed into them during provisioning (e.g. Rust) are not available in this VM.";
pub(crate) const PROVISION_NO_MOUNT_MISE_NOTE: &str = "Note: --no-mount-mise: mise tools are installed into the image itself. Run this image with --no-mount-mise to use them; otherwise the shared mise directory hides them.";
pub(crate) const PROVISION_NO_MOUNT_SYS_CACHE_NOTE: &str = "Note: --no-mount-sys-cache: tools installed into the cache directories (e.g. Rust in ~/.cargo) are written into the image itself. Run this image with --no-mount-sys-cache to use them; otherwise the shared cache directories hide them.";

/// Project subfolders masked with tmpfs to prevent host data leaking into the VM.
pub(crate) const MASKED_SUBFOLDERS: &[&str] = &[".vibe"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitMode {
    Rw,
    Ro,
    No,
}

#[derive(Clone, Debug)]
pub(crate) struct DirectoryShare {
    pub(crate) host: PathBuf,
    pub(crate) guest: GuestPath,
    pub(crate) read_only: bool,
    pub(crate) is_project_dir: bool,
}

impl DirectoryShare {
    /// A relative `guest` path is below the HOME of the `--vm-user`.
    pub(crate) fn new(
        host: PathBuf,
        guest: impl Into<PathBuf>,
        read_only: bool,
        create_host_dir: bool,
        is_project_dir: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::at(
            host,
            GuestPath::new(guest),
            read_only,
            create_host_dir,
            is_project_dir,
        )
    }

    pub(crate) fn at(
        host: PathBuf,
        guest: GuestPath,
        read_only: bool,
        create_host_dir: bool,
        is_project_dir: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if !host.exists() {
            if create_host_dir {
                fs::create_dir_all(&host)?;
            } else {
                return Err(format!("Host path does not exist: {}", host.display()).into());
            }
        }
        Ok(Self {
            host,
            guest,
            read_only,
            is_project_dir,
        })
    }

    /// The shared apt cache, which a non-root session mounts with `bindfs`.
    pub(crate) fn is_apt_cache(&self) -> bool {
        !self.is_project_dir
            && self.guest == GuestPath::Absolute(session_plan::APT_CACHE_GUEST.into())
    }

    pub(crate) fn from_mount_spec(spec: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let parts: Vec<&str> = spec.split(':').collect();
        if parts.len() < 2 || parts.len() > 3 {
            return Err(format!("Invalid mount spec: {spec}").into());
        }
        let host = PathBuf::from(parts[0]);
        let guest = PathBuf::from(parts[1]);
        let read_only = if parts.len() == 3 {
            match parts[2] {
                "read-only" => true,
                "read-write" => false,
                _ => {
                    return Err(format!(
                        "Invalid mount mode '{}'; expected read-only or read-write",
                        parts[2]
                    )
                    .into());
                }
            }
        } else {
            false
        };
        DirectoryShare::new(host, guest, read_only, false, true)
    }

    pub(crate) fn tag(&self) -> String {
        let path_str = self.host.to_string_lossy();
        let hash = path_str.bytes().fold(5381u64, |h, b| {
            h.wrapping_mul(33).wrapping_add(u64::from(b))
        });
        let base_name = self
            .host
            .file_name()
            .map_or("share".into(), |s| s.to_string_lossy());
        format!("{base_name}_{hash:016x}")
    }
}

/// Masks and extra shares for the project directories (the current directory
/// and `--mount` shares) among `shares`: `.vibe` is always masked; `.git` is
/// masked or shared read-only by `git_mode`; `node_modules` gets a separate
/// share from the cache directory. The extra shares come after their parent
/// directories so they are mounted on top of them.
pub(crate) fn project_masks_and_shares(
    shares: &[DirectoryShare],
    git_mode: GitMode,
    cache_dir: &Path,
) -> Result<(Vec<GuestPath>, Vec<DirectoryShare>), Box<dyn std::error::Error>> {
    let mut masks = Vec::new();
    let mut extra_shares = Vec::new();
    for share in shares.iter().filter(|share| share.is_project_dir) {
        // Discourage read/write of project dir subfolders within the VM.
        for subfolder in MASKED_SUBFOLDERS {
            if share.host.join(subfolder).exists() {
                masks.push(share.guest.join(subfolder));
            }
        }

        if share.host.join("node_modules").is_dir() {
            let share_tag = share.tag();
            extra_shares.push(DirectoryShare::at(
                cache_dir.join(format!("guest_node_modules/{share_tag}")),
                share.guest.join("node_modules"),
                false,
                true,
                false,
            )?);
        }

        // Handle .git directory masking or read-only sharing
        let git_path = share.host.join(".git");
        if git_path.exists() {
            match git_mode {
                GitMode::No => masks.push(share.guest.join(".git")),
                GitMode::Ro => {
                    // VirtioFS can only share directories; skip if .git is a file
                    // (e.g. git submodules store a gitdir pointer file, not a directory).
                    if git_path.is_dir() {
                        extra_shares.push(DirectoryShare::at(
                            git_path,
                            share.guest.join(".git"),
                            true,
                            false,
                            false,
                        )?);
                    }
                }
                GitMode::Rw => {}
            }
        }
    }
    Ok((masks, extra_shares))
}

/// The cache directory shares to mount, in mount order: mise, the system
/// caches, then the coding agent configuration directories.
pub(crate) fn cache_share_specs(sys_cache: bool, mise: bool, agents_conf: bool) -> Vec<CacheShare> {
    let mut specs = Vec::new();
    if mise {
        specs.push(MISE_CACHE_SHARE);
    }
    if sys_cache {
        specs.extend_from_slice(SYS_CACHE_SHARES);
    }
    if agents_conf {
        specs.extend_from_slice(AGENT_CONF_SHARES);
    }
    specs
}

/// Creates (if needed) and shares the given cache directories of `vm_user`.
pub(crate) fn create_cache_shares(
    cache_dir: &Path,
    specs: &[CacheShare],
    vm_user: &VmUser,
) -> Result<Vec<DirectoryShare>, Box<dyn std::error::Error>> {
    specs
        .iter()
        .map(|spec| {
            DirectoryShare::new(
                spec.host_dir(cache_dir, vm_user),
                spec.guest,
                false,
                true,
                false,
            )
        })
        .collect()
}

/// Note about user caches that still sit in the old layout
/// (`<cache dir>/<name>`, before `user-<user>/`): printed when a user cache
/// directory about to be created does not exist yet but its old directory does.
/// Vibe never moves or deletes the old directories.
pub(crate) fn legacy_cache_notice(
    cache_dir: &Path,
    specs: &[CacheShare],
    vm_user: &VmUser,
) -> Option<String> {
    let old: Vec<&str> = specs
        .iter()
        .filter(|spec| spec.scope == CacheScope::User)
        .filter(|spec| {
            !spec.host_dir(cache_dir, vm_user).exists() && cache_dir.join(spec.name).is_dir()
        })
        .map(|spec| spec.name)
        .collect();
    let first = old.first()?;
    let user_dir = user_cache_dir(cache_dir, vm_user);
    let cache = cache_dir.display();
    let user_dir = user_dir.display();
    Some(format!(
        "Note: user caches now live in {user_dir}/. The old directories in {cache} ({}) are no longer mounted, so tools installed into them are not available. \
Vibe does not move or delete them. To reuse one, move it by hand while no VM runs, for example: rmdir {user_dir}/{first} && mv {cache}/{first} {user_dir}/{first}. Or provision the image again.",
        old.join(", ")
    ))
}

/// Host log of what a VM gets from the host: the directory shares and the
/// files Vibe writes into it. `written` holds (guest path, source) pairs.
/// With `restrict_apt_cache` the apt cache is marked as read-only for the
/// `vm_user` (mounted with `bindfs`).
pub(crate) fn describe_vm_files(
    stage: &str,
    shares: &[DirectoryShare],
    written: &[(&str, &str)],
    vm_user: &VmUser,
    restrict_apt_cache: bool,
) -> String {
    let mut text = format!("{stage}: directory shares:\n");
    if shares.is_empty() {
        text.push_str("  (none)\n");
    }
    for share in shares {
        let mode = if share.read_only {
            "read-only".to_string()
        } else if restrict_apt_cache && share.is_apt_cache() {
            format!(
                "read-write, read-only for {} via bindfs if the image has it",
                vm_user.as_str()
            )
        } else {
            "read-write".to_string()
        };
        text.push_str(&format!(
            "  {} -> {} ({mode})\n",
            share.host.display(),
            share.guest.display(vm_user)
        ));
    }
    text.push_str(&format!("{stage}: files written into the VM:\n"));
    if written.is_empty() {
        text.push_str("  (none)\n");
    }
    for (guest, source) in written {
        text.push_str(&format!("  {guest} ({source})\n"));
    }
    text
}

/// Notes printed when running without the shared tool directories.
pub(crate) fn run_share_notes(mount_sys_cache: bool, mount_mise: bool) -> Vec<&'static str> {
    [
        (!mount_mise).then_some(RUN_NO_MOUNT_MISE_NOTE),
        (!mount_sys_cache).then_some(RUN_NO_MOUNT_SYS_CACHE_NOTE),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Notes printed when provisioning without the shared tool directories.
pub(crate) fn provision_share_notes(mount_sys_cache: bool, mount_mise: bool) -> Vec<&'static str> {
    [
        (!mount_mise).then_some(PROVISION_NO_MOUNT_MISE_NOTE),
        (!mount_sys_cache).then_some(PROVISION_NO_MOUNT_SYS_CACHE_NOTE),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// tmux configuration for `--mount-tmux-conf`: host `~/.tmux` shared read-only
/// and the content and path of host `~/.tmux.conf`, each only when it exists
/// on the host.
#[allow(clippy::type_complexity)]
pub(crate) fn tmux_conf_mounts(
    home: &Path,
) -> Result<(Option<DirectoryShare>, Option<(String, PathBuf)>), Box<dyn std::error::Error>> {
    let tmux_dir = home.join(".tmux");
    let share = if tmux_dir.is_dir() {
        Some(DirectoryShare::new(tmux_dir, ".tmux", true, false, false)?)
    } else {
        None
    };

    let tmux_conf = home.join(".tmux.conf");
    let conf = if tmux_conf.is_file() {
        Some((fs::read_to_string(&tmux_conf)?, tmux_conf))
    } else {
        None
    };

    if share.is_none() && conf.is_none() {
        eprintln!(
            "Warning: --mount-tmux-conf: neither ~/.tmux nor ~/.tmux.conf exists on the host"
        );
    }
    Ok((share, conf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_caches_are_kept_per_vm_user_and_apt_is_global() {
        let cache = Path::new("/cache/vibe");
        let dev = VmUser::parse("dev").unwrap();
        let specs = cache_share_specs(true, true, true);
        for vm_user in [VmUser::root(), dev] {
            let user_dir = format!("/cache/vibe/user-{}", vm_user.as_str());
            for spec in &specs {
                let host = spec.host_dir(cache, &vm_user);
                if spec.name == "apt" {
                    assert_eq!(spec.scope, CacheScope::Global);
                    assert_eq!(host, Path::new("/cache/vibe/apt"));
                    assert_eq!(spec.guest, session_plan::APT_CACHE_GUEST);
                } else {
                    assert_eq!(spec.scope, CacheScope::User, "{spec:?}");
                    assert_eq!(host, Path::new(&user_dir).join(spec.name));
                    assert!(!spec.guest.starts_with('/'), "{spec:?}");
                }
            }
        }
    }

    #[test]
    fn legacy_cache_notice_lists_old_directories_without_a_new_one() {
        let dir = std::env::temp_dir().join(format!(
            "vibe-legacy-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = VmUser::root();
        let specs = cache_share_specs(true, true, false);
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(legacy_cache_notice(&dir, &specs, &root), None);

        // The global apt cache keeps its directory and never counts as old.
        for name in ["apt", "cargo", "mise-cache"] {
            fs::create_dir_all(dir.join(name)).unwrap();
        }
        let notice = legacy_cache_notice(&dir, &specs, &root).unwrap();
        assert!(notice.contains("(mise-cache, cargo)"), "{notice}");
        assert!(notice.contains(&format!("{}/user-root/", dir.display())));
        assert!(notice.contains("rmdir"), "{notice}");

        // Only shares about to be mounted count.
        assert!(
            !legacy_cache_notice(&dir, &cache_share_specs(true, false, false), &root)
                .unwrap()
                .contains("mise-cache")
        );

        // Once a new directory exists, its old one is not mentioned again.
        let created = create_cache_shares(&dir, &specs, &root).unwrap();
        assert_eq!(created[1].host, dir.join("apt"));
        assert!(dir.join("user-root/cargo").is_dir());
        assert_eq!(legacy_cache_notice(&dir, &specs, &root), None);

        // Another user gets its own directories, so the note shows up for it.
        let dev = VmUser::parse("dev").unwrap();
        let notice = legacy_cache_notice(&dir, &specs, &dev).unwrap();
        assert!(notice.contains("user-dev/"), "{notice}");
        fs::remove_dir_all(dir).unwrap();
    }
}
