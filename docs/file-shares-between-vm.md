# File shares between the Mac and the VM

This document explains how Vibe shares host directories with the VM, who decides the permissions, and which parts are not a security boundary.
It describes the current behavior. The measurements behind it were taken on an ARM Mac with a Debian 13 guest (kernel `6.12.116+deb13-arm64`).

## 1. How a share is set up

### Host side

- `run_main` (`src/main.rs`) collects `DirectoryShare` values: the project directory, `--mount` entries, the caches, `node_modules`, and a read-only `.git`.
- Each share gets a tag `<basename>_<hash>` (`DirectoryShare::tag`, a djb2 hash of the host path).
- `default_interactive_spec` (`src/vm_config.rs`) puts all shares into one VirtioFS device with the tag `shared`. The device holds a `VZMultipleDirectoryShare`. Each entry is a `VZSharedDirectory` with `readOnly` set from the share.
- Read-only is enforced by the host. The guest cannot turn it off, not even as root.

### Guest side

The session plan (`src/session_plan.rs`) mounts everything as root, before any user action runs:

1. `mkdir -p /mnt/root/virtiofs-shared && chmod -R 700 /mnt/root && mount -t virtiofs shared /mnt/root/virtiofs-shared`.
   This is the staging directory. `/mnt/root` is `0700` before the mount, so only root can reach it.
2. For each share, `mount --bind /mnt/root/virtiofs-shared/<tag> <target>`.
   A relative guest path is below the HOME of the `--vm-user` (`/root` for root, `/home/<name>` otherwise). Missing directories below a non-root HOME are created with `install -d -o "$VIBE_VM_UID" -g "$VIBE_VM_GID"`, so the user owns them.
3. Masks: `mount -t tmpfs -o mode=0555,uid=0,gid=0 tmpfs <path>`. A mask is an empty directory owned by root that nobody else can write.
4. For a non-root `--vm-user`, the apt cache is mounted with `bindfs` instead of a plain bind mount (see below).
5. Only then does the session switch to the user with `su --login --pty`.

### What is shared

| Host | Guest | Mode |
| --- | --- | --- |
| Current directory | `~/<project>` | read-write |
| `--mount HOST:GUEST[:MODE]` | `GUEST` (relative to `~`) | as given, default read-write |
| `<project>/.vibe` | masked with tmpfs | read-only, empty |
| `<project>/.git` | `--git ro` (default): separate read-only share. `--git rw`: part of the project share. `--git no`: masked | as listed |
| `<cache>/guest_node_modules/<tag>` | `<project>/node_modules`, only when the host has `node_modules/` | read-write |
| `<cache>/apt` | `/var/cache/apt`, shared by every `--vm-user` | read-write for root; read-only for a non-root user through `bindfs` |
| `<cache>/user-<name>/{cargo,mise-cache,yarn,m2,go}` | `~/.cargo`, `~/.local/share/mise`, `~/.yarn`, `~/.m2`, `~/go` | read-write |
| `<cache>/user-<name>/<agent>` (`--mount-agents-conf`) | the agent's configuration directory in `~` | read-write |
| `~/.tmux` (`--mount-tmux-conf`) | `~/.tmux` | read-only |

`<cache>` is `~/.cache/vibe`. `~` is the HOME of the `--vm-user`.
`.env` and other files are not masked (see section 4).

### The apt cache for a non-root user

Root's apt writes the shared apt cache. An ordinary user must not change it, or it could replace a `.deb` that root installs later.
So a non-root session mounts it with:

    bindfs -o nonempty -u "$(id -u _apt)" -g 0 -p 0755,af-x <staging>/<apt tag> /var/cache/apt/

- Files appear as `_apt:root`, directories `0755`, files `0644`. The user can read but not write, delete, `chmod`, or unmount.
- `_apt` can write `archives/partial`, and root's `apt-get` works. Downloads land in the host cache.
- `-o nonempty` is needed because `/var/cache/apt` in the image is not empty.
- Provisioning installs `bindfs` (it also pulls in `fuse3` and `libfuse2t64`).
- When `bindfs` or `_apt` is missing, Vibe falls back to a writable bind mount and prints a warning. Provision a new image, or run with `--no-mount-sys-cache`.
- When the `bindfs` process dies, `/var/cache/apt` becomes `Transport endpoint is not connected`. apt fails; it does not fall back to the writable share.

## 2. The permission model

Three outcomes were possible when guest users other than root write to a share:

- R1: any guest UID can write a writable share.
- R2: only a guest UID equal to the host UID can write.
- R3: an idmapped mount (`X-mount.idmap`) maps guest UIDs to the host UID.

Measured result: R1. R2 does not hold, and R3 is not available.

### What decides access

- The host decides. Apple's VirtioFS server opens host files as the user who runs `vibe`.
  - A host `0555` directory gives `EACCES` to every guest UID, root included.
  - A host `0700` directory, or a host `0600` file, is readable and writable for every guest UID.
  - New files belong to your user on the Mac.
- A read-only share gives `EPERM` (not `EROFS`) to every guest UID, root included.
- The guest kernel also checks permissions, but it does not restrict anyone in practice:
  - `virtio_fs_fill_super` always sets `default_permissions` and `allow_other`. `/proc/self/mountinfo` does not show them; `fuse_show_options` prints them only for ordinary FUSE mounts.
  - `fuse_permission` calls `generic_permission`. On `EACCES` with attributes that were not just refreshed, it fetches the attributes again as the caller and checks again.
  - The server reports the caller's UID and GID as the owner, so the second check passes.

### Ownership as the guest sees it

- The owner of a shared file is whoever queried it last. Attributes are cached for about 1 second.
  - Root runs `stat`, and the file shows `0:0`. A second later, `dev` runs `stat` and it shows `dev:dev`. Root then sees `dev:dev` too.
- `chmod` in the guest changes the permission bits of the host file.
- `chown` returns success and changes nothing.
- Mount options cannot change this:
  - `mount -t virtiofs -o uid=1000,gid=1000` fails: `virtiofs: Unknown parameter 'uid'`. virtiofs accepts only `dax`.
  - `mount --bind -o uid=...` succeeds and ignores the option.
  - `X-mount.idmap` fails: `cannot set ID-mapping: Invalid argument`. The same mount on tmpfs works.

### Consequences

- `--vm-user` accounts use the default Debian UID (`useradd`, `UID_MIN` is `1000`). The host UID is not passed to the guest, and nothing maps UIDs.
- Every process in the VM that can reach a mount point can write every writable share and read every read-only share.
- Separation between guest users comes only from:
  - guest-local directories: the staging directory (`/mnt/root`, `0700`) and the user's HOME (`0700`);
  - read-only shares;
  - masks;
  - `bindfs` for the apt cache.

## 3. Common questions

**Why does `ls -l` show a different owner each time?**
The owner is the last caller, cached for about 1 second. It means nothing for access.

**Why does git say `detected dubious ownership`?**
Git compares the repository owner with the current user. When root looked at the repository less than a second ago, `dev` sees root as the owner. Retrying a second later works.
Vibe does not change git's configuration. Set `safe.directory` in the VM if this bothers you.
Other tools that check ownership (ssh `StrictModes`, gpg's homedir check) can do the same with files in a share.

**Why does `chown` do nothing?**
The server keeps host files owned by your Mac user. The guest only sees the caller as the owner.

**Why can root not write a read-only share?**
The host refuses the write (`EPERM`). Root in the guest has no power over the host.

**Why is `/var/cache/apt` read-only for my user?**
It is shared with every image and run, and root installs packages from it. See "The apt cache for a non-root user".
The warning `bindfs is not available; the shared apt cache is writable by every user in this VM` means the image has no `bindfs`. Provision a new image.

**Why does apt fail with `Could not open file /var/cache/apt/archives/partial/... (13: Permission denied)`?**
This was seen while provisioning from a base image with an empty apt cache. Two runs each lost one download out of about 180. The third run, with most packages already cached, passed.
apt downloads as `_apt` into `archives/partial` (`0700`), while root processes look at the same directory. The likely cause: the owner the guest sees flips between callers, and a permission check loses that race.
Run the provision again. Each run keeps the packages it downloaded, so fewer downloads race.

## 4. Masks and their limits

Measured with a non-root user (UID `1000`) in the current layout:

- Staging cannot be reached: `ls`, `stat`, `/proc/1/root/...`, a symlink into staging, and `unshare -Urm` (list, new virtiofs mount, unmount) all fail. `/proc/self/mountinfo` still shows the paths and the tag.
- A mask cannot be removed: `umount` says `must be superuser to unmount`. `rm` and `mv` give `EBUSY`.
- Inside `unshare -Urm`, unmounting a mask fails, a non-recursive bind of the project fails (`EINVAL`), and `--rbind` copies the masks along.
- The read-only `.git` cannot be written or unmounted.

### Case-insensitive host volumes

The default APFS volume is case-insensitive, and the VirtioFS server looks names up with the host's rules.

- A file mask can be bypassed. Vibe used to mask `.env` with an empty file. `cat ~/proj/.ENV` then still printed the host `.env`, for root and for other users. `ln .ENV leak` created a hard link on the host.
- The guest cannot close this gap:
  - a name of n letters has 2^n case variants, plus Unicode normalization variants;
  - `VZSharedDirectory` cannot exclude a path;
  - Linux casefolding works only on ext4 and f2fs.
- So Vibe no longer masks `.env`, or any other file. **Keep secrets outside the project directory.** The agent in the VM can read every file in a share.
- Directory masks were not bypassed: `.GIT` and `.Git` reach the read-only `.git` share; `.VIBE` and `NODE_MODULES` give `ENOENT`.
  The likely reason: the Linux dcache allows only one alias per directory (`d_splice_alias`) but many per file (hard links). This is a kernel detail, not a contract. The `vm-user` integration check (below) tests it.

## 5. Reproducing and regression checks

### Integration checks

`tests/vm_user.rs` runs `src/vm_checks/vm_user.rs` through the `vibe` binary. The check provisions a `dev` image from a clone of `VIBE_VM_CHECK_DISK` and then runs it. It covers:

- writes to the project share, a writable `--mount`, and a user cache (R1);
- writes to a read-only share fail;
- staging is not reachable;
- `.git` and its case variants cannot be written;
- `.vibe` is a root-owned read-only empty directory;
- `.env` reads the same content as on the host;
- the apt cache is `fuse.bindfs` and read-only for `dev`, and root's apt works.

Run:

    VIBE_VM_CHECK_DISK=~/.cache/vibe/default.raw \
    VIBE_VM_CHECK_ARGS="--proxy <proxy> --dns 1.1.1.1" \
    cargo test --release --test vm_user -- --ignored --test-threads=1

When a macOS update changes VirtioFS, the R1 write checks and the read-only checks fail first. Rerun the probe scripts below to see what changed.

### Probe scripts

`docs/ai-plans/references/` holds three host/guest pairs. Each host script (`*-run.sh`) takes `[vibe binary] [image]`, works on an APFS clone, and deletes its temporary directory at the end. The VM proxy comes from `VIBE_PROBE_PROXY`.

| Scripts | Checks |
| --- | --- |
| `20261112-vm-user-virtiofs-probe{,-run}.sh` | Owners and modes, writes by UID 0, 1000, host UID, 1001; read-only shares; `chmod`/`chown`; `X-mount.idmap`; `unshare` |
| `20261114-vm-user-virtiofs-uid-probe{,-run}.sh` | `uid=`/`gid=` and FUSE mount options, bind mount options, attribute cache timing, git ownership |
| `20261116-vm-user-q4-layout-probe{,-run}.sh` | The current layout: staging, bind mounts, masks, case variants, `bindfs` for apt |

Other UIDs are entered with `setpriv --reuid --regid --clear-groups`, so no account is needed.

## 6. Sources

- Linux v6.12: `fs/fuse/virtio_fs.c` (`virtio_fs_parameters`, `virtio_fs_parse_param`, `virtio_fs_fill_super`), `fs/fuse/inode.c` (`fuse_show_options`), `fs/fuse/dir.c` (`fuse_permission`).
- Vibe: `src/main.rs` (`DirectoryShare`, `CacheShare`, `project_masks_and_shares`), `src/vm_config.rs` (VirtioFS device), `src/session_plan.rs` (staging, bind mounts, masks, `bindfs`).
- Boot sequence and the cache directory layout: [how-vm-boot.md](how-vm-boot.md).
