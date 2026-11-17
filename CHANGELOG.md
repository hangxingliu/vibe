# Changelog

All notable changes to this fork of Vibe are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The original repository is <https://github.com/lynaghk/vibe>.
Version 1.0.0 lists everything this fork changes compared to it (upstream version 0.1.0, as of commit `0858d69`).

## [1.0.0] - Unreleased

### Added

- `-p` / `--publish [udp:][HOST_ADDR:]HOST_PORT:GUEST_PORT` forwards a guest TCP or UDP port to the host.
  Repeat the flag for another port.
  Example: `-p 127.0.0.1:8080:80 -p udp:127.0.0.1:9002:9002`.
- `--proxy <URL>` sends VM traffic and host image downloads through an HTTP or SOCKS5 proxy.
  Accepted schemes are `http://`, `https://`, `socks5://`, and `socks5h://`.
  A URL with no scheme gets `http://`.
- In `nat` mode, `vibe-usernet` proxies outbound TCP. That path never falls back to a direct connection.
- In `vznat` mode, the guest receives `http_proxy` and `https_proxy`.
  `vibe provision` saves the proxy in the new image's apt configuration. SOCKS5 proxy is saved as `socks5h://`, which apt supports.
- `--proxy-bypass <LIST>` lists destinations that are reached directly.
  The list replaces the built-in loopback, link-local, and private ranges.
  An entry may be an IP, a CIDR, a `192.168.*` prefix, `localhost`, `*`, or `host:port`.
  A domain name applies only to host downloads.
  Traffic to the host through the gateway stays direct.
- `--[no-]proxy-udp` routes the VM's outbound UDP through a SOCKS5 proxy. The default is on for a SOCKS5 proxy.
- `--[no-]proxy-dns` sends gateway DNS to its upstreams through a SOCKS5 proxy over TCP. The default is on for a SOCKS5 proxy.
  An upstream in the bypass list is queried directly.
  An upstream that fails through the proxy answers `SERVFAIL`.
- `--[no-]strict-proxy` drops VM traffic that cannot use the proxy.
  Unproxied UDP, gateway DNS, and ICMP reach only the bypass list.
  When no gateway DNS upstream is in the bypass list, the answer is `REFUSED`.
- `--no-proxy-vm` leaves VM traffic unproxied. Host downloads still use the proxy.
- Startup prints the VM proxy and where that proxy came from. It also prints the handling of connection protocols by type.
  See [docs/network-proxy.md](docs/network-proxy.md).
- `--dns <ADDR>` sets an upstream DNS server for the VM. Repeat the flag for another server.
- `--ssh-key <PUBLIC_KEY_FILE>` installs a public key for the vm-user and starts the SSH server.
  Install script: [src/guest/install_ssh_key.sh](src/guest/install_ssh_key.sh).
  Root uses the same script. `authorized_keys` ends with a newline.
- `--git <rw | ro | no>` controls the project's `.git` directory.
  The default `ro` shares it read-only.
  `rw` shares it read-write.
  `no` hides `.git` with an empty tmpfs owned by root, mode 0555.
- Project directories are the current directory and the `--mount` shares.
  Each `.vibe` directory is hidden by an empty tmpfs owned by root, mode 0555.
  Each `node_modules` directory is kept in a guest cache.
  The guest reads and writes `node_modules` in that cache.
- Mount flags: `--[no-]mount-cwd`, `--[no-]mount-sys-cache`, `--[no-]mount-mise`, `--[no-]mount-tmux-conf`, and `--[no-]mount-agents-conf`.
  The last flag given wins.
- `--mount-tmux-conf` shares the host `~/.tmux` read-only. It copies `~/.tmux.conf` into the vm-user's HOME.
- `--write-bashrc` rewrites the vm-user's `~/.profile` with Vibe's bash configuration and loads it. Provisioning always writes that file.
- `--vm-user <NAME>` selects the guest account for the session (run and provision). The default is `root` (`--vm-user root` is that same account).
- Vibe logs in as root, mounts the shares, then switches with `su --login`.
  Share paths sit below that user's HOME (for example `/home/dev/proj` and `/home/dev/.cargo`).
  The apt cache stays at `/var/cache/apt`.
  A missing share directory is created and owned by the user.
- Login actions (`--send`, `--expect`, `--script`) run in that user's shell, in the project directory.
  `--env` is exported in that shell.
  `exit` returns to the root shell.
  `exit` in the root shell powers the VM off.
- A relative `--mount` guest path uses that user's HOME.
  `--write-bashrc`, `--mount-tmux-conf`, and `--ssh-key` use that HOME too.
- With a non-root `--vm-user`, a built-in provision script installs system packages as root.
  The user-level part installs mise, the tools mise installs, rustup, and the coding agents. That part runs as the vm-user.
  Your own scripts run as root. Those scripts see `VIBE_VM_USER`, `VIBE_VM_UID`, `VIBE_VM_GID`, and `VIBE_VM_HOME`.
- Provisioning writes Vibe's `.profile` for root and for the vm-user.
  Vibe refuses to save the image when that user's HOME holds a file owned by root (mount points excluded).
- `vibe provision --repl` opens a shell after the scripts.
  Change the image there before it is saved.
  For a non-root `--vm-user`, that shell is the user's shell.
  `exit` returns to root. `exit` there saves the image.
  With an existing image and no `--replace`, `provision --repl` boots that image and skips the scripts.
- Provisioning an image that already has Vibe's tools for another user prints a warning. User-level tools are not installed for the new vm-user.
- A non-root `--vm-user` is checked before the session starts.
  `provision --repl` of an existing image does this check too.
  Vibe stops when the account is missing, has UID 0, or its login shell is not bash.
  Vibe stops when HOME is missing, is not an absolute directory, is not owned by the account, or is `/root`.
  Vibe does not change an existing account.
- Membership in `root`, `sudo`, `adm`, `wheel`, or `admin` prints a warning.
  A root account with no password prints a warning too, because the vm-user could then become root with `su -`.
- `vibe provision --vm-user <NAME>` creates the account when it does not exist.
  `useradd` gives it the default UID, its own group, bash, and `/home/<NAME>`.
  The account gets no extra groups.
  A non-root `--vm-user` requires `--root-password` or `VIBE_ROOT_PASSWORD`.
  The provision history in the image records `vm_user:`.
- Every provision that runs scripts installs `bindfs`.
  A run with a non-root `--vm-user` mounts the shared apt cache through `bindfs`.
  Only root's apt can change that cache.
  Without `bindfs`, the cache is mounted writable (Vibe then prints a warning).
  Install script: [src/guest/install_bindfs.sh](src/guest/install_bindfs.sh).
- `--root-password <PASSWORD>` is the root password used to log in.
  When the flag is omitted, Vibe reads `VIBE_ROOT_PASSWORD`.
  The flag wins, including when its value is rejected.
- An empty password is an error. Control characters (U+0000 through U+001F, or U+007F) are an error.
  The error names `--root-password` or `VIBE_ROOT_PASSWORD`.
  The error does not print the password. Diagnostic output prints `***`.
  The value is visible in the host process list, like any other argument.
- A provision that runs scripts sets that password with `chpasswd` after login and before `.profile`.
  This covers a new image, `--repl`, and the first implicit `default.raw`. The password is not placed on a guest command line.
  `vibe`, and `provision --repl` of an existing image, use the password only to log in.
- Built-in provisioning scripts: `@copilot`, `@golang`, and `@grok`.
- `VIBE_EXTRA_IMAGE_DIR` and `VIBE_PREFERRED_IMAGE_DIR` add more image directories.
  One use is an external drive.
  See [Keeping images on another disk](USAGE.md#keeping-images-on-another-disk).
  `vibe` and `vibe provision` print the directories shared into the VM.
  They also print the files written into the VM and the image directory lookup order. These happen before the VM starts.
- Keystrokes typed while login actions are still running are discarded. Press Ctrl-C twice during that time to stop the VM.
- A boot that never reaches a root shell reports the failed stage. When the image may need preparation, the error suggests `vibe provision`.
- A provision script can `source "$VIBE_PROVISION_INCLUDE"` to call `exec_retry`.

### Changed

- `vibe --help` is shorter and fits 90 columns. The full option reference moved to `USAGE.md`.
- `vibe --version` prints `https://github.com/hangxingliu/vibe/` as the repository URL.
- Newer Debian nocloud base images are supported. Those images ask for a root password and run a firstboot wizard.
  `vibe provision` prepares such a base once, in a headless Alpine maintenance VM, before the scripts.
  Running an existing disk never inspects or modifies that disk. See [docs/alpine-os-purpose.md](docs/alpine-os-purpose.md).
- The proxy options, `--dns`, `--network`, and `-p` / `--publish` work with `vibe provision`.
  Place them before or after `provision`.
- **Breaking:** With no `--proxy`, Vibe uses `https_proxy`, `HTTPS_PROXY`, `http_proxy`, or `HTTP_PROXY`, in that order.
  That proxy covers VM traffic and host downloads.
  With `vznat`, `vibe provision` also saves it into the new image.
  `no_proxy` or `NO_PROXY` becomes the bypass list. Pass `--no-proxy-vm` to leave the VM unproxied.
- **Breaking:** `no_proxy`, `NO_PROXY`, or `--proxy-bypass` replaces the built-in bypass list.
  A range such as `10.0.0.0/8` goes through the proxy when it is not listed.
  Add that range to `no_proxy` or `--proxy-bypass` to keep it direct.
- **Breaking:** With a SOCKS5 proxy, UDP is proxied by default. Pass `--no-proxy-udp` to send UDP directly.
- **Breaking:** Host downloads always pass `--proxy` to curl. `ALL_PROXY` is not read. Set `--proxy` or `https_proxy` instead.
- With `vznat`, the guest's `no_proxy` and `NO_PROXY` are the bypass list. `vibe provision` also saves a proxy that comes from the environment.
- The provisioning VM uses 4096 MB of RAM by default. The previous default was 2048 MB.
- **Breaking:** User caches live in `user-<NAME>/` under the cache directory, including for root.
  The names are `mise-cache`, `cargo`, `yarn`, `m2`, `go`, and the `--mount-agents-conf` directories.
  Example: `~/.cache/vibe/user-root/cargo`. The apt cache stays at `apt/`.
- An old user-cache directory is no longer mounted. Tools that an existing `default.raw` installed there are unavailable.
  Vibe prints a note when the old directory exists and the new one does not. Vibe does not move or delete the old directory.
  Move the old directory into `user-root/` by hand while no VM runs.
  Provision the image again to install the tools into the new cache.
- **Breaking:** The agent configuration directories (codex, copilot, claude, gemini, pi, grok, opencode) are not mounted by default.
  The tmux configuration is not mounted by default. Pass `--mount-agents-conf` and `--mount-tmux-conf`.
- Every boot writes `.bash_logout`.
  `exit` from the console powers the VM off. When the guest powers off, `vibe` exits promptly.
  A tmux server skips that poweroff, including a server started by another user. An SSH session skips that poweroff too.
- **Breaking:** Running a VM does not rewrite or load `~/.profile`. Pass `--write-bashrc` to rewrite the vm-user's `~/.profile` and load it.
  Provisioning still writes `.profile` for root and for a non-root vm-user.
- **Breaking:** While `default.raw` does not exist, `vibe` rejects `--no-mount-sys-cache` and `--no-mount-mise`.
  Create the default image with `vibe provision` first.
- `vibe --vm-user <NAME>` with a non-root user refuses to create a missing `default.raw` when no root password is set.
  The error shows the `vibe provision` command to use.
- A `Password:` prompt with no password is an error. A rejected password is an error.
  If the guest does not accept or reject the password in time, Vibe reports a timeout. These password errors do not suggest `vibe provision`.
  On an image that does not ask for a password, Vibe does not type a supplied password.
- The VirtioFS staging directory is `/mnt/root/virtiofs-shared`. It moved there from `/mnt/shared`. `/mnt/root` is accessible only by root.
- Scripts from `--script` and from provisioning are written to `/tmp/vibe-scripts-<uid>` (mode 0700).
  They moved there from `/tmp/vibe-scripts`.
- Run and provision create `<cache>/sessions/<pid>-<nonce>/` (mode 0700) and share it read-only on the VirtioFS staging mount.
  The share is not bind-mounted. The host file log and the guest share table omit it.
  The directory is removed when the VM stops normally. In case of an abnormal stop, a later run that finds the pid no longer exists will also remove this directory.
- A non-root run that mounts the apt share runs `src/guest/mount_apt_cache.sh`.
  `bindfs` stdout goes to stderr. The fallback notice is printed only after `mount --bind` succeeds.
- Provision scripts and `--script` files reach the VM through the read-only session share. They are no longer typed on the console.
  Each one is `scripts/NNN-<name>.sh` in the session directory, so a kept directory shows what ran.
  A run `--script` with a non-root `--vm-user` still goes inline, because that user cannot read the session directory.
- The console no longer shows the script wrapper. It shows one `eval` line per script.
- Provision sources `provision-env.sh` from the session directory. It exports `DEBIAN_FRONTEND=noninteractive` and the `VIBE_PROVISION_*`, `VIBE_GIT_SHA`, and `VIBE_BUILD_DATE` variables.

### Removed

- **Breaking:** `.env` files in a project directory are no longer masked, because bypasses are possible.
  See [docs/file-shares-between-vm.md](docs/file-shares-between-vm.md).
- **Breaking:** `--no-default-mounts` is removed.
  Use `--no-mount-cwd`, `--no-mount-sys-cache`, and `--no-mount-mise`.
  The `.vibe` mask and the `node_modules` share cannot be turned off. `--git` still controls `.git`.

### Fixed

- Network retries in built-in provision scripts never ran a second time under `set -e`. `exec_retry` now retries.
