# Using Vibe

This is the full usage guide. The [readme](readme.md) has the short version.

## Commands

Vibe only does two things:

1. **Runs** VMs from a raw disk image file
2. **Provisions** VMs by booting a base raw disk image, running scripts in it, then saving the resulting raw disk image as a template

```
vibe [OPTIONS] [LOGIN-ACTIONS ...] [path/to/disk.raw]
vibe provision [OPTIONS] [PROVISION-OPTIONS] [@built-in | path/to/script.sh ...]
```

## Running a VM

When you run `vibe` in a project directory, it copies the default template (`~/.cache/vibe/default.raw`) to `.vibe/instance.raw`, boots it up, and attaches your terminal to this VM.

When you `exit` this shell, the VM is shutdown.
The disk state persists until you delete it.

`path/to/disk.raw`, when given, is booted as-is. Run time does not inspect or modify the disk. An unprepared current Debian image must be copied first with `vibe provision --base`.

## Making images

As a thoughtful person who reads the README, you'll probably appreciate the ability to create custom template images by running:

    vibe provision --image my-template @rust @codex my-custom-script.sh
    vibe provision --proxy socks5://127.0.0.1:1080 @rust

(Omitting `--image` provisions the default template. The proxy options, `--dns`, `--network`, and `-p`/`--publish` can be placed before or after `provision`.)
Scripts are run in order, and those prefixed with `@` are resolved against the built-in scripts shipped with Vibe.

In `my-custom-script.sh` I set up my tmux keybindings, favorite shell customizations, etc.

In a project directory, you can then run `vibe --image my-template` to use this template image (this flag is ignored if `.vibe/instance.raw` already exists).

The [base provisioning script](provisioning/base.sh) is always run when provisioning to install basic tools like gcc, [mise-en-place](https://mise.jdx.dev/), ripgrep, etc.
If you don't want this, you can make your own `.raw` disk images and copy them into `~/.cache/vibe/` (or any extra image directory, see [Keeping images on another disk](#keeping-images-on-another-disk)) to use them as templates.

## Default mounts and files

Before a VM starts, `vibe` and `vibe provision` print the directories shared into it and the files Vibe writes into it.

When running a VM:

| What | Default | Option |
| --- | --- | --- |
| Current directory at `~/<project>`, `cd` into it | on | `--no-mount-cwd` |
| `.vibe` masking (an empty read-only directory), `node_modules`, `.git` handling of project directories (current directory and `--mount`) | always | - |
| Shared caches `apt`, `cargo`, `yarn`, `m2`, `go` | on | `--no-mount-sys-cache` |
| Shared mise data at `~/.local/share/mise` | on | `--no-mount-mise` |
| Host `~/.tmux` (read-only) and `~/.tmux.conf` | off | `--mount-tmux-conf` |
| Agent configuration (codex, copilot, claude, gemini, pi, grok, opencode) from the cache directory | off | `--mount-agents-conf` |
| Write Vibe's `.profile` and source it | off | `--write-bashrc` |
| Write `.bash_logout` (power off on `exit`) | always | - |

Every option also has the opposite form (`--mount-mise`, `--no-write-bashrc`, ...); the last one given wins.
`~` is the HOME of the `--vm-user` (`/root` by default); the apt cache is always at `/var/cache/apt`.

The shared caches live in the cache directory (`~/.cache/vibe`). All users share `apt/`. The other caches are kept per `--vm-user` under `user-<NAME>/`, for example `user-root/cargo` or `user-dev/mise-cache`.
Before this layout, the caches sat directly in the cache directory (`cargo/`, `mise-cache/`, ...). Vibe no longer mounts those old directories, so tools that `default.raw` installed into them are missing.
Vibe prints a note while an old directory exists and its new one does not, but never moves or deletes it. Move it into `user-root/` by hand while no VM runs, or provision the image again.
Provisioning accepts `--[no-]mount-sys-cache` and `--[no-]mount-mise`, always writes and sources `.profile`, and never mounts the current directory, tmux, or agent configuration.

The shared caches are where provisioning installs many tools: `@rust` installs Rust into `~/.cargo`, and mise installs its tools into `~/.local/share/mise`.
So:

- Running with `--no-mount-sys-cache` or `--no-mount-mise` hides the tools that provisioning put into the shared directories (Vibe prints a note).
- Provisioning with `--no-mount-sys-cache` or `--no-mount-mise` installs those tools into the image itself. Run that image with the same options to use them; otherwise the shared directories are mounted over them and hide them.
- The default image is shared by all projects, so `vibe` only creates it with the shared directories mounted. If `default.raw` does not exist yet, `vibe --no-mount-mise` fails and asks you to run `vibe provision` (with or without these options) first.

## Running as a non-root user

By default everything in the VM runs as root. To give the agent an ordinary account instead, provision an image for it and run with the same `--vm-user`:

    vibe provision --image dev --vm-user dev --root-password 'something-secret' @codex
    vibe --image dev --vm-user dev --root-password 'something-secret'

Provisioning creates the account (`useradd`, its own group, bash, `/home/dev`), sets the root password, and installs the user-level tools of the built-in scripts (mise, rustup, coding agents) as `dev`. Your own provisioning scripts still run as root; they can use `runuser -u "$VIBE_VM_USER" -- ...`.
`VIBE_ROOT_PASSWORD` can be used instead of `--root-password`.

When running, Vibe logs in as root, mounts the shares below `/home/dev`, and then switches to `dev` with `su --login`. Login actions (`--send`, `--expect`, `--script`) run as `dev`.
`exit` returns to the root shell, and `exit` there powers the VM off.
In login actions, follow `--send exit` with `--expect 'back in the root shell'` before sending more; text sent while the user's shell closes is lost.
Without a root password, `dev` could become root with `su -`, so Vibe warns about images whose root account has no password.

Some things to know:

- Shares are not separated by guest user: every user in the VM can write the writable shares, and new files there belong to your user on the Mac.
  The separation comes from directories inside the VM (the staging directory and the HOME of `dev` are private) and from read-only shares and masks.
  [docs/file-shares-between-vm.md](docs/file-shares-between-vm.md) explains the permission model and its limits.
- The shared apt cache is mounted read-only for `dev` with `bindfs`, so only root's apt can change it. Images without `bindfs` (provisioning installs it) get a writable cache and a warning.
- Git in a shared repository may report `dubious ownership`, because shared files appear to belong to whoever looked at them last. Set `safe.directory` in the VM if this bothers you.
- An image provisioned for one user does not have the user-level tools of another user. Provision a new image from a fresh base for each user.

## Proxies

If your shell has `https_proxy`/`http_proxy` (or the upper-case forms) set, Vibe uses that proxy without being asked: for the VM's traffic and for downloading base images on the host.
`no_proxy`/`NO_PROXY` becomes the list of destinations that are reached directly.
`--proxy` and `--proxy-bypass` override the environment, and `--no-proxy-vm` keeps the proxy away from the VM (downloads still use it).

Every run starts with a short summary of what goes through the proxy and what does not, for example:

    VM network (nat):
      Proxy:  socks5://***@192.168.1.88:3003 (from --proxy)
      Strict: off
      TCP:    proxied
      UDP:    proxied
      DNS:    proxied (DNS-over-TCP); upstreams in the bypass list are queried directly
      ICMP:   direct (not proxied)
      Bypass: 127.0.0.1,localhost,192.168.* (from no_proxy); gateway 192.168.5.2 -> host is always direct

With a SOCKS5 proxy, UDP and the VM's DNS go through the proxy too. HTTP proxies only carry TCP, so Vibe warns that UDP and DNS go directly; add `--strict-proxy` to drop that traffic instead.
See [docs/network-proxy.md](docs/network-proxy.md) for how each option affects each kind of traffic.

## Keeping images on another disk

Disk images are large. If you'd rather keep them on an external drive, list extra image directories with two optional environment variables:

    export VIBE_EXTRA_IMAGE_DIR=/Volumes/ExternalDisk/VMImages:/Volumes/ExternalDisk2/VMImages
    export VIBE_PREFERRED_IMAGE_DIR=/Volumes/ExternalDisk2/VMImages

- `VIBE_EXTRA_IMAGE_DIR` is a `:` separated list of absolute paths. Directories that don't exist (say, an unplugged drive) are skipped.
- `VIBE_PREFERRED_IMAGE_DIR` must be `~/.cache/vibe` (the default directory, which follows `$XDG_CACHE_HOME`) or one of the extra directories, and it must exist. When unset, the default directory is preferred.

Image names (`--image NAME`, `--base NAME`, and the `default` image), the Debian base image, and the Alpine maintenance image are looked up in this order: the preferred directory, then the default directory, then the remaining extra directories. The first match wins.
Paths such as `vibe path/to/disk.raw`, `--base path/to/x.raw`, and `.vibe/instance.raw` are used as given.

Downloads and new images go to the preferred directory. Provisioning an image that already exists (with `--replace`) replaces it in the directory where it was found.
Debian and Alpine images in directories other than the preferred one are only read: never locked, repaired, or re-downloaded. A broken Alpine image there is skipped with a warning.
While a new Debian base is prepared, a temporary `maint/` folder is created next to the Alpine image in use (or in the preferred directory if that location is not writable).
The shared caches (`apt`, `cargo`, ...), agent configuration, `vibe-usernet`, and maintenance logs (`logs/`) always stay in the default directory.

So you don't forget about this setup, `vibe` and `vibe provision` print the lookup order when they start, and `vibe --help` ends with it:

    Image directories (lookup order):
      1. /Volumes/ExternalDisk2/VMImages (preferred)
      2. /Users/me/.cache/vibe (default)
      3. /Volumes/ExternalDisk/VMImages (missing, skipped)

When an image is found by name, Vibe prints its full path.

## Option reference

Options may appear before or after `provision`.
`--base`, `--replace`, and `--repl` are only valid with `provision`.
Login actions, `--[no-]mount-cwd`, `--[no-]mount-tmux-conf`, `--[no-]mount-agents-conf`, `--[no-]write-bashrc`, `--env`, `--mount`, `--git`, and `--ssh-key` are only valid when running a VM.

`path/to/disk.raw`, when given, is booted as-is. Run time does not inspect or modify the disk. An unprepared current Debian image must be copied first with `vibe provision --base`.

### General options (run and provision)

- `-h`, `--help`: Print the help.
- `--version`: Print the version (commit SHA and build date) and the repository URL.
- `--image NAME`: Run: template image (ignored if `.vibe/instance.raw` already exists).
  Run time does not inspect or modify the disk. An unprepared current Debian image must be copied first with `vibe provision --base`.
  Provision: name of the image to create (default `default`).
- `--cpus COUNT`: Number of virtual CPUs (default 2).
- `--ram MB`: RAM size in megabytes (default 2048 when running, 4096 when provisioning).
- `--vm-user NAME`: Guest account the session is for (default `root`).
  Vibe logs in as root, mounts the shares, then switches to NAME with `su --login`:
  login actions run as NAME, `exit` returns to the root shell, and `exit` there powers off.
  Relative guest paths (project directory, caches except apt, `--mount`, `.tmux`) are below NAME's HOME.
  Running checks that the account exists and uses bash; `provision` creates it (`useradd`, no extra groups).
  Provisioning for a non-root NAME needs a root password. Running warns when root has no password
  (NAME could then become root with `su -`).
- `--root-password PW`: Root password used to log in (default: the `VIBE_ROOT_PASSWORD` environment variable).
  A provision that runs scripts sets it as the new image's root password.
  The value is visible in the host process list.
- `--[no-]mount-sys-cache`: Mount the shared apt, cargo, yarn, m2, and go caches (default on).
  With `--no-mount-sys-cache`, tools installed into them during provisioning (e.g. Rust) are not available.
- `--[no-]mount-mise`: Mount the shared mise data directory at `~/.local/share/mise` (default on).
  With `--no-mount-mise`, tools installed into it by mise during provisioning are not available.

### Run options

- `--[no-]mount-cwd`: Share the current directory at `~/<project>` and cd into it (default on).
- `--[no-]mount-tmux-conf`: Share host `~/.tmux` (read-only) and copy host `~/.tmux.conf` into the VM (default off).
- `--[no-]mount-agents-conf`: Mount the shared coding agent configuration directories
  (codex, copilot, claude, gemini, pi, grok, opencode) from the cache directory (default off).
- `--[no-]write-bashrc`: Overwrite the vm-user's `~/.profile` with Vibe's built-in bash configuration and load it (default off).
  Provisioning always does this.
- `--env NAME`: Export host environment variable NAME inside the VM.
  Errors if NAME is unset or empty.
- `--mount HOST:GUEST[:MODE]`: Mount HOST inside the VM at GUEST. MODE is `read-write` (default) or `read-only`.
  A relative GUEST is below the vm-user's HOME.
  Errors if HOST does not exist.
- `--git MODE`: How the `.git` directory is treated (default `ro`).
  `rw`: share host `.git` as read-write.
  `ro`: share host `.git` as read-only.
  `no`: mask `.git` with tmpfs.

Rules for the switches above:

- Each `--[no-]NAME` switch has two forms (`--mount-mise`, `--no-write-bashrc`, ...); the last one given wins.
- When running, `--no-mount-sys-cache` and `--no-mount-mise` require an existing default image (create it first with `vibe provision`).
- Project directories (the current directory and `--mount` shares) always have `.vibe` masked.
- Files such as `.env` appear in the VM like any other file in the share; keep secrets outside the project directory.
- `~` above means the HOME of the `--vm-user` (`/root` by default).
- The caches except apt are kept per `--vm-user` in the cache directory, under `user-NAME/` (for example `user-root/cargo`).
- Writable shares can be written by every user in the VM; new files there belong to the host user running vibe.
- Git may report `dubious ownership` in a shared repository; set `safe.directory` in the VM if you need to.
- With a non-root `--vm-user`, the shared apt cache is mounted read-only for that user with bindfs (only root's apt
  changes it); on images without bindfs it is mounted writable and a warning is printed.

### Network and proxy options (run and provision)

- `--network MODE`: Guest networking mode (default `nat`).
  `nat` uses Vibe's bundled user-mode network stack.
  `vznat` uses Apple's VZNATNetworkDeviceAttachment.
- `-p SPEC`, `--publish SPEC`: Forward a guest port to the host (repeatable).
  SPEC is `[udp:][HOST_ADDR:]HOST_PORT:GUEST_PORT`.
  Example: `-p 127.0.0.1:8022:22 -p udp:8080:80`
- `--proxy URL`: Proxy for VM traffic and host-side image downloads (`http://`, `https://`, `socks5://`, `socks5h://`;
  a URL without a scheme gets `http://`). Without `--proxy`, `https_proxy`, `HTTPS_PROXY`,
  `http_proxy`, and `HTTP_PROXY` are used, in that order (`ALL_PROXY` is not read).
  `nat`: the VM's outbound TCP goes through the proxy and never falls back to direct.
  `vznat`: the proxy is only exported to the guest as `http_proxy`/`https_proxy` (vibe cannot
  enforce it), and `provision` saves it into the new image's apt configuration.
  A startup summary shows which proxy is used and where it came from.
- `--proxy-bypass LIST`: Destinations reached directly (comma separated; default: `no_proxy`, then `NO_PROXY`, then a
  built-in list of loopback, link-local, and private ranges). Replaces the built-in list.
  Accepts IPs, CIDRs, IPv4 prefixes (`192.168.*`), `localhost`, `*`, and `host:port`.
  Domain names only apply to host downloads. Traffic to the host via the gateway
  192.168.5.2 is always direct.
- `--[no-]proxy-udp`: Route the VM's outbound UDP through the proxy (`nat` only). Default: on for SOCKS5,
  off for HTTP proxies (which cannot carry UDP; `--proxy-udp` is then an error).
- `--[no-]proxy-dns`: Send the gateway DNS (queries to 192.168.5.2) to its upstreams through the proxy, over
  TCP (`nat` only). Default: on for SOCKS5, off for HTTP proxies. Upstreams in the bypass
  list are queried directly. DNS sent straight to another server (`dig @8.8.8.8`) is
  ordinary UDP/TCP traffic and follows `--proxy-udp` and `--strict-proxy` instead.
- `--[no-]strict-proxy`: Drop VM traffic that cannot go through the proxy (`nat` only; default off). UDP and
  gateway DNS that are not proxied only reach the bypass list; ICMP (ping) only reaches
  the bypass list. Without it, such UDP, DNS, and ICMP go directly.
- `--no-proxy-vm`: Do not proxy VM traffic. `--proxy` and the proxy variables still apply to downloads.
- `--dns ADDR`: Upstream DNS server for the VM's gateway DNS (repeatable; e.g. `--dns 8.8.8.8 --dns 1.1.1.1`).
  Default: the host's `/etc/resolv.conf` nameservers when DNS is proxied or strict, the
  host system resolver otherwise. Proxied DNS sends these upstreams through the proxy too.

### Login actions (run only)

Login actions are executed in order in the vm-user's shell. Each one can be repeated.

- `--script PATH`: Run the script in the VM; stop if it exits non-zero.
- `--send TEXT`: Type TEXT followed by a newline into the VM.
- `--expect TEXT [SECS]`: Wait for TEXT to appear in console output before executing the next login action.
  If TEXT does not appear within SECS seconds (default 30), shut down the VM with an error.
- `--ssh-key FILE`: Install the SSH public key FILE for the vm-user and start the SSH server.

### Provision options

Provisioning creates a new named image by running (built-in) scripts.

- `--base NAME|PATH`: Use this existing image or `path/to/image.raw` as the base for the new image (default Debian Stable).
- `--image NAME`: Name for the new image (default `default`).
- `--replace`: Replace the existing image with NAME, if one exists.
- `--repl`: After provisioning (or when the image already exists), drop into an interactive shell.
  If the image does not exist or `--replace` is given: runs all scripts, then leaves
  the shell open so you can customise further before the image is saved.
  If the image already exists (and `--replace` is not given): boots the existing image
  directly for interactive use without re-running any scripts.
  With a non-root `--vm-user`, the shell is the vm-user's; `exit` returns to the
  root shell, and `exit` there powers off (and saves a new image).
- `--cpus COUNT`: Number of virtual CPUs for the provisioning VM (default 2).
- `--ram MB`: RAM size in megabytes for the provisioning VM (default 4096).

Built-in scripts install system packages as root and user-level tools (mise, rustup, coding agents) as the
`--vm-user`. Your own scripts run as root with `VIBE_VM_USER` and `VIBE_VM_HOME` set; use `runuser -u "$VIBE_VM_USER" -- ...`
to run commands as that user.
A new Debian base is first prepared once in a headless Alpine maintenance VM; older bases and Vibe images are used as-is.

### Environment variables

- `VIBE_ROOT_PASSWORD`: Default for `--root-password`.
- `VIBE_EXTRA_IMAGE_DIR=DIR[:DIR ...]`: Extra image directories (absolute paths, `:` separated), searched after the default one.
  Directories that do not exist are listed as skipped.
- `VIBE_PREFERRED_IMAGE_DIR=DIR`: Preferred directory: the default directory or an entry of `VIBE_EXTRA_IMAGE_DIR`.
  It is searched first and must exist. Downloads and new images are written here;
  an existing image is replaced where it was found.
- `VIBE_KEEP_FAILED_WORK=1`: Keep the work copy and maintenance files of a failed provision for debugging.

Image directories:

Image names (`--image NAME`, `--base NAME`, the `default` image) and the Debian and Alpine base images are looked up
in the image directories, in order; the first match is used. Paths given with `/` are used as-is.
The default image directory is the cache directory. `VIBE_EXTRA_IMAGE_DIR` and `VIBE_PREFERRED_IMAGE_DIR` add more.
Shared caches (apt, cargo, ...), agent configuration, and maintenance logs always stay in the cache directory.
The lookup order is printed when a VM is run or provisioned.
See [Keeping images on another disk](#keeping-images-on-another-disk).

## Help output

`vibe --help` prints this, followed by the cache directory and the image directories:

```
Vibe spins up a Linux VM on Mac to sandbox LLM agents.

Usage:
  vibe [OPTIONS] [LOGIN-ACTIONS ...] [path/to/disk.raw]
  vibe provision [OPTIONS] [PROVISION-OPTIONS] [@built-in | path/to/script.sh ...]

Options may appear before or after `provision`.
Full reference: USAGE.md (https://github.com/hangxingliu/vibe/blob/main/USAGE.md)

Options (run and provision):
  -h, --help              Print this help.
  --version               Print the commit SHA, build date, and repository URL.
  --image NAME            Run: template image (ignored once .vibe/instance.raw exists).
                          Provision: image to create (default `default`).
  --cpus COUNT            Virtual CPUs (default 2).
  --ram MB                RAM in MB (default 2048; 4096 when provisioning).
  --vm-user NAME          Guest account for the session (default root).
  --root-password PW      Root password to log in (default $VIBE_ROOT_PASSWORD).
  --[no-]mount-sys-cache  Mount the shared apt, cargo, yarn, m2, and go caches (on).
  --[no-]mount-mise       Mount the shared mise data at ~/.local/share/mise (on).

Run options:
  --[no-]mount-cwd           Share the current directory at ~/<project> (on).
  --[no-]mount-tmux-conf     Share host ~/.tmux and copy ~/.tmux.conf (off).
  --[no-]mount-agents-conf   Mount the shared coding agent configuration (off).
  --[no-]write-bashrc        Write Vibe's ~/.profile and load it (off).
  --env NAME                 Export host environment variable NAME in the VM.
  --mount HOST:GUEST[:MODE]  Share HOST at GUEST. MODE: read-write (default)
                             or read-only. A relative GUEST is below the vm-user's HOME.
  --git MODE                 How to share .git. MODE: ro (default), rw, or no
                             (mask .git with tmpfs).

Network options (run and provision):
  --network MODE       MODE: nat (default, bundled user-mode stack) or vznat.
  -p, --publish SPEC   Forward a guest port to the host (repeatable).
                       SPEC: [udp:][HOST_ADDR:]HOST_PORT:GUEST_PORT
                       Example: -p 127.0.0.1:8022:22 -p udp:8080:80
  --proxy URL          Proxy for VM traffic and image downloads
                       (http, https, socks5, socks5h; default $https_proxy etc.).
  --proxy-bypass LIST  Destinations reached directly (comma separated).
  --[no-]proxy-udp     Proxy the VM's UDP (nat only; on for SOCKS5).
  --[no-]proxy-dns     Proxy gateway DNS over TCP (nat only; on for SOCKS5).
  --[no-]strict-proxy  Drop VM traffic the proxy cannot carry (nat only; off).
  --no-proxy-vm        Do not proxy VM traffic; downloads still use the proxy.
  --dns ADDR           Upstream DNS server for the gateway DNS (repeatable).

Login actions (run only; in order, in the vm-user's shell, repeatable):
  --script PATH         Run a script in the VM; stop if it fails.
  --send TEXT           Type TEXT and a newline.
  --expect TEXT [SECS]  Wait for TEXT; shut down with an error after SECS (default 30).
  --ssh-key FILE        Install a public key for the vm-user and start sshd.

Provision options:
  --base NAME|PATH  Base image (default Debian stable).
  --replace         Replace an existing image with the same name.
  --repl            Open a shell after the scripts run, or boot an existing image.

Environment:
  VIBE_ROOT_PASSWORD        Default for --root-password.
  VIBE_EXTRA_IMAGE_DIR      Extra image directories, `:` separated.
  VIBE_PREFERRED_IMAGE_DIR  Directory searched first; downloads and new images go here.
  VIBE_KEEP_FAILED_WORK=1   Keep the work files of a failed provision.

Built-in provisioning scripts:

  @claude     Install Anthropic's Claude.
  @codex      Install OpenAI's Codex.
  @copilot    Install Github Copilot.
  @gemini     Install Google's Gemini (antigravity).
  @golang     Install Go language.
  @grok       Install Grok Build
  @pi         Install Earendil's Pi.
  @rust       Install Rust with sccache.
```
