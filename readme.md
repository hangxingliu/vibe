This is a fork of <https://github.com/lynaghk/vibe> that adds port forwarding, proxy and DNS options, SSH access, stricter project directory isolation, finer control over default mounts, and support for newer Debian base images.

See [CHANGELOG.md](CHANGELOG.md) for everything that differs from the original repository, including breaking changes.

---

Vibe is a quick, zero-configuration way to spin up a Linux virtual machine on Mac to sandbox LLM agents:

```
$ cd my-project
$ vibe

░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░▒▓███████▓▒░░▒▓████████▓▒░
░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░
 ░▒▓█▓▒▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░
 ░▒▓█▓▒▒▓█▓▒░░▒▓█▓▒░▒▓███████▓▒░░▒▓██████▓▒░
  ░▒▓█▓▓█▓▒░ ░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░
  ░▒▓█▓▓█▓▒░ ░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░
   ░▒▓██▓▒░  ░▒▓█▓▒░▒▓███████▓▒░░▒▓████████▓▒░

Host                                         Guest                    Mode
-------------------------------------------  -----------------------  ----------
/Users/dev/work/my-project                   /root/my-project         read-write
/Users/dev/work/my-project/.git              /root/my-project/.git    read-only
/Users/dev/.cache/vibe/user-root/mise-cache  /root/.local/share/mise  read-write
/Users/dev/.cache/vibe/apt                   /var/cache/apt           read-write
/Users/dev/.cache/vibe/user-root/cargo       /root/.cargo             read-write
/Users/dev/.cache/vibe/user-root/yarn        /root/.yarn              read-write
/Users/dev/.cache/vibe/user-root/m2          /root/.m2                read-write
/Users/dev/.cache/vibe/user-root/go          /root/go                 read-write

root@vibe:~/my-project#
```

On my M1 MacBook Air it takes ~10 seconds to boot.


Dependencies:

- An ARM-based Mac running MacOS 13 (Ventura) or higher.
- A network connection is required on the first run to download and configure the Debian Linux base image.
- That's it!


## Why use Vibe?

- LLM agents are more fun to use with `--yolo`, since they're not always interrupting you to approve their commands.
- Sandboxing the agent in a VM lets it install/remove whatever tools its lil' transformer heart desires, *without* wrecking your actual machine.
- You control what the agent (and thus the upstream LLM provider) can actually see, by controlling exactly what's shared into the VM sandbox.
  (This project was inspired by me running `codex` *without* `--yolo` and seeing it reading files outside of the directory I started it in --- not cool, bro.)

I'm using virtual machines rather than containers because:

- Virtualization is more secure against malicious escapes than containers or the MacOS sandbox framework.
- Containers on MacOS require spinning up a virtual machine anyway.

Finally, as a matter of taste and style:

- I wrote the entire README myself, 100% with my human brain.
- The Rust dependencies are the [Objc2](https://github.com/madsmtm/objc2) interop crates, the [lexopt](https://github.com/blyxxyz/lexopt) argument parser, and a few small crates (`libc`, `regex`, `base64`).
- There are no emoji anywhere in this repository.


## Install

Vibe is a single binary built with Rust, with a [bundled networking helper](/helpers/vibe-usernet) written in Go.

Download [the latest binary built by GitHub actions](https://github.com/lynaghk/vibe/releases/tag/latest) and put it somewhere on your `$PATH`:

    curl -LO https://github.com/lynaghk/vibe/releases/download/latest/vibe-macos-arm64.zip
    unzip vibe-macos-arm64.zip
    mkdir -p ~/.local/bin
    mv vibe ~/.local/bin
    export PATH="$HOME/.local/bin:$PATH"

If you use [mise-en-place](https://mise.jdx.dev/):

    mise use github:lynaghk/vibe@latest

I'm not making formal releases.
I recommend reading [CHANGELOG.md](CHANGELOG.md) and the commit history, and pinning to a specific version.

If you're building from a checkout, use mise to get the Rust and Go compilers:

    mise install --locked
    cargo build --locked


## Using Vibe

Vibe only does two things:

1. **Runs** VMs from a raw disk image file
2. **Provisions** VMs by booting a base raw disk image, running scripts in it, then saving the resulting raw disk image as a template

When you run `vibe` in a project directory, it copies the default template (`~/.cache/vibe/default.raw`) to `.vibe/instance.raw`, boots it up, and attaches your terminal to this VM.

When you `exit` this shell, the VM is shutdown.
The disk state persists until you delete it.

Where does this `default.raw` raw disk image come from?

When you first run `vibe`, a Debian Linux base image is downloaded and [all of the provisioning scripts](/provisioning/) are run against it.

As a thoughtful person who reads the README, you'll probably appreciate the ability to create custom template images by running:

    vibe provision --image my-template @rust @codex my-custom-script.sh
    vibe provision --proxy socks5://127.0.0.1:1080 @rust

(Omitting `--image` provisions the default template. The proxy options, `--dns`, `--network`, and `-p`/`--publish` can be placed before or after `provision`.)
Scripts are run in order, and those prefixed with `@` are resolved against the built-in scripts shipped with Vibe.

In `my-custom-script.sh` I set up my tmux keybindings, favorite shell customizations, etc.

In a project directory, you can then run `vibe --image my-template` to use this template image (this flag is ignored if `.vibe/instance.raw` already exists).

The [base provisioning script](/provisioning/base.sh) is always run when provisioning to install basic tools like gcc, [mise-en-place](https://mise.jdx.dev/), ripgrep, etc.
If you don't want this, you can make your own `.raw` disk images and copy them into `~/.cache/vibe/` (or any extra image directory, see [Keeping images on another disk](#keeping-images-on-another-disk)) to use them as templates.


```
Vibe is a quick way to spin up a Linux virtual machine on Mac to sandbox LLM agents.

vibe [OPTIONS] [LOGIN-ACTIONS ...] [path/to/disk.raw]
vibe provision [OPTIONS] [PROVISIONING-OPTIONS] [@built-in | path/to/script.sh ...]

Options may appear before or after `provision`.
`--base`, `--replace`, and `--repl` are only valid with `provision`.
Login actions, `--[no-]mount-cwd`, `--[no-]mount-tmux-conf`, `--[no-]mount-agents-conf`, `--[no-]write-bashrc`, `--env`, `--mount`, `--git`, and `--ssh-key` are only valid when running a VM.

`path/to/disk.raw`, when given, is booted as-is. Run time does not inspect or modify the disk. An unprepared current Debian image must be copied first with `vibe provision --base`.

Options:

  --help                                                    Print this help message.
  --version                                                 Print the version (commit SHA and build date).
  --image NAME                                              Run: template image (ignored if `.vibe/instance.raw` already exists).
                                                            Run time does not inspect or modify the disk. An unprepared current Debian image must be copied first with `vibe provision --base`.
                                                            Provision: name of the image to create (default `default`).
  --cpus COUNT                                              Number of virtual CPUs (default 2).
  --ram MEGABYTES                                           RAM size in megabytes (default 2048 when running, 4096 when provisioning).
  --vm-user NAME                                            Guest account the session is for (default `root`; run and provision).
                                                            Vibe logs in as root, mounts the shares, then switches to NAME with `su --login`:
                                                            login actions run as NAME, `exit` returns to the root shell, and `exit` there powers off.
                                                            Relative guest paths (project directory, caches except apt, `--mount`, `.tmux`) are below NAME's HOME.
                                                            Running checks that the account exists and uses bash; `provision` creates it (`useradd`, no extra groups).
                                                            Provisioning for a non-root NAME needs a root password. Running warns when root has no password
                                                            (NAME could then become root with `su -`).
  --root-password PASSWORD                                  Root password used to log in (default: the VIBE_ROOT_PASSWORD environment variable).
                                                            A provision that runs scripts sets it as the new image's root password.
                                                            The value is visible in the host process list.
  --no-mount-sys-cache                                      Do not mount the shared apt, cargo, yarn, m2, and go caches (run and provision).
                                                            Tools installed into them during provisioning (e.g. Rust) are not available.
  --no-mount-mise                                           Do not mount the shared mise data directory at ~/.local/share/mise (run and provision).
                                                            Tools installed into it by mise during provisioning are not available.
  --no-mount-cwd                                            Do not share the current directory at ~/<project> or cd into it.
  --mount-tmux-conf                                         Share host ~/.tmux (read-only) and copy host ~/.tmux.conf into the VM.
  --mount-agents-conf                                       Mount the shared coding agent configuration directories
                                                            (codex, copilot, claude, gemini, pi, grok, opencode) from the cache directory.
  --write-bashrc                                            Overwrite the vm-user's ~/.profile with Vibe's built-in bash configuration and load it.
                                                            Provisioning always does this.

  Each of the six options above also has the opposite form (`--mount-mise`, `--no-write-bashrc`, ...); the last one given wins.
  When running, `--no-mount-sys-cache` and `--no-mount-mise` require an existing default image (create it first with `vibe provision`).
  Project directories (the current directory and `--mount` shares) always have `.vibe` masked.
  Files such as `.env` appear in the VM like any other file in the share; keep secrets outside the project directory.
  `~` above means the HOME of the --vm-user (`/root` by default).
  The caches except apt are kept per --vm-user in the cache directory, under user-NAME/ (for example user-root/cargo).
  Writable shares can be written by every user in the VM; new files there belong to the host user running vibe.
  Git may report `dubious ownership` in a shared repository; set `safe.directory` in the VM if you need to.
  With a non-root --vm-user, the shared apt cache is mounted read-only for that user with bindfs (only root's apt
  changes it); on images without bindfs it is mounted writable and a warning is printed.

  --env NAME                                                Export host environment variable NAME inside VM.
                                                            Errors if NAME is unset or empty.
  --mount HOST_PATH:GUEST_PATH[:read-only | :read-write]    Mount HOST_PATH inside VM at GUEST_PATH (default mode `:read-write`)
                                                            A relative GUEST_PATH is below the vm-user's HOME.
                                                            Errors if HOST_PATH does not exist.
  --network <nat|vznat>                                     Guest networking mode (default `nat`).
                                                            `nat` uses Vibe's bundled user-mode network stack.
                                                            `vznat` uses Apple's VZNATNetworkDeviceAttachment.
  -p, --publish [udp:][HOST_ADDR:]HOST_PORT:GUEST_PORT      Forward a guest port to the host (repeatable).
                                                            Example: -p 127.0.0.1:8022:22 -p udp:8080:80
  --proxy <URL>                                             Proxy for VM traffic and host-side image downloads (http://, https://, socks5://, socks5h://;
                                                            a URL without a scheme gets http://). Without --proxy, https_proxy, HTTPS_PROXY,
                                                            http_proxy, and HTTP_PROXY are used, in that order (ALL_PROXY is not read).
                                                            `nat`: the VM's outbound TCP goes through the proxy and never falls back to direct.
                                                            `vznat`: the proxy is only exported to the guest as http_proxy/https_proxy (vibe cannot
                                                            enforce it), and `provision` saves it into the new image's apt configuration.
                                                            A startup summary shows which proxy is used and where it came from.
  --proxy-bypass <LIST>                                     Destinations reached directly (comma separated; default: no_proxy, then NO_PROXY, then a
                                                            built-in list of loopback, link-local, and private ranges). Replaces the built-in list.
                                                            Accepts IPs, CIDRs, IPv4 prefixes (`192.168.*`), localhost, `*`, and `host:port`.
                                                            Domain names only apply to host downloads. Traffic to the host via the gateway
                                                            192.168.5.2 is always direct.
  --[no-]proxy-udp                                          Route the VM's outbound UDP through the proxy (`nat` only). Default: on for SOCKS5,
                                                            off for HTTP proxies (which cannot carry UDP; --proxy-udp is then an error).
  --[no-]proxy-dns                                          Send the gateway DNS (queries to 192.168.5.2) to its upstreams through the proxy, over
                                                            TCP (`nat` only). Default: on for SOCKS5, off for HTTP proxies. Upstreams in the bypass
                                                            list are queried directly. DNS sent straight to another server (`dig @8.8.8.8`) is
                                                            ordinary UDP/TCP traffic and follows --proxy-udp and --strict-proxy instead.
  --[no-]strict-proxy                                       Drop VM traffic that cannot go through the proxy (`nat` only; default off). UDP and
                                                            gateway DNS that are not proxied only reach the bypass list; ICMP (ping) only reaches
                                                            the bypass list. Without it, such UDP, DNS, and ICMP go directly.
  --no-proxy-vm                                             Do not proxy VM traffic. --proxy and the proxy variables still apply to downloads.
  --dns <ADDR>                                              Upstream DNS server for the VM's gateway DNS (repeatable; e.g. --dns 8.8.8.8 --dns 1.1.1.1).
                                                            Default: the host's /etc/resolv.conf nameservers when DNS is proxied or strict, the
                                                            host system resolver otherwise. Proxied DNS sends these upstreams through the proxy too.
  
  --git <rw | ro | no>                                      How the .git directory is treated (default `ro`).
                                                            rw: share host .git as read-write.
                                                            ro: share host .git as read-only.
                                                            no: mask .git with tmpfs.
							    

Login actions (executed in order in the vm-user's shell, repeatable; not valid with `provision`):

  --script PATH_TO_SCRIPT                                   Run script in VM; stop if it exits non-zero.
  --send SOME_COMMAND                                       Type SOME_COMMAND followed by newline into the VM.
  --expect STRING [timeout-seconds]                         Wait for STRING to appear in console output before executing next login action.
                                                            If STRING does not appear within timeout (default 30 seconds), shutdown VM with error.
  --ssh-key <PUBLIC_KEY_FILE>                               Install SSH public key for the vm-user and start SSH server.

Provisioning creates a new named image by running (built-in) scripts. Options:

  --base NAME_OR_PATH                                       Use this existing image or path/to/image.raw as base for new image (default Debian Stable).
  --image NAME                                              Name for new image (default `default`).
  --replace                                                 Replace existing image with NAME, if one exists.
  --repl                                                    After provisioning (or when image already exists), drop into an interactive shell.
                                                            If the image does not exist or --replace is given: runs all scripts, then leaves
                                                            the shell open so you can customise further before the image is saved.
                                                            If the image already exists (and --replace is not given): boots the existing image
                                                            directly for interactive use without re-running any scripts.
                                                            With a non-root --vm-user, the shell is the vm-user's; `exit` returns to the
                                                            root shell, and `exit` there powers off (and saves a new image).
  --cpus COUNT                                              Number of virtual CPUs for the provisioning VM (default 2).
  --ram MEGABYTES                                           RAM size in megabytes for the provisioning VM (default 4096).

Built-in scripts install system packages as root and user-level tools (mise, rustup, coding agents) as the
--vm-user. Your own scripts run as root with VIBE_VM_USER and VIBE_VM_HOME set; use `runuser -u "$VIBE_VM_USER" -- ...`
to run commands as that user.
A new Debian base is first prepared once in a headless Alpine maintenance VM; older bases and Vibe images are used as-is.
Set the environment variable VIBE_KEEP_FAILED_WORK=1 to keep the work copy and maintenance files of a failed provision for debugging.

Image directories:

  Image names (`--image NAME`, `--base NAME`, the `default` image) and the Debian and Alpine base images are looked up
  in the image directories, in order; the first match is used. Paths given with `/` are used as-is.
  The default image directory is the cache directory. Two optional environment variables add more:

  VIBE_EXTRA_IMAGE_DIR=DIR[:DIR ...]                        Extra image directories (absolute paths, `:` separated), searched after the default one.
                                                            Directories that do not exist are listed as skipped.
  VIBE_PREFERRED_IMAGE_DIR=DIR                              Preferred directory: the default directory or an entry of VIBE_EXTRA_IMAGE_DIR.
                                                            It is searched first and must exist. Downloads and new images are written here;
                                                            an existing image is replaced where it was found.

  Shared caches (apt, cargo, ...), agent configuration, and maintenance logs always stay in the cache directory.
  The lookup order is printed when a VM is run or provisioned.

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
      Proxy:  socks5://***@192.168.1.88:2083 (from --proxy)
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

## Other notes

- Vibe VMs can reach the host at `192.168.5.2`.
  The host cannot reach the Vibe VMs, nor can VMs reach each other.
  DNS is handled by the host resolver, so VMs get VPN and split-DNS compatibility.

- This networking is based on a bundled gVisor/Lima-style user-mode network helper process, `vibe-usernet`, which is spawned automatically when you run `vibe`.
  I ended up with this solution because Apple's [VZNATNetworkDeviceAttachment](https://developer.apple.com/documentation/virtualization/vznatnetworkdeviceattachment) lost packets and VMs got wrecked whenever host networking changed (e.g., switching between wifi/ethernet/VPN). [VZBridgedNetworkDeviceAttachment](https://developer.apple.com/documentation/virtualization/vzbridgednetworkdeviceattachment) requires kowtowing to acquire the restricted [com.apple.vm.networking](https://developer.apple.com/documentation/BundleResources/Entitlements/com.apple.vm.networking) entitlement, which I'm not interested in doing.
  If you have suggestions for how to improve networking capabilities, please open an issue or PR!

- The default VM disk is 100 GiB, but since Apple Filesystem is copy-on-write and doesn't count zeros, disk space is only used when you actually write new blocks.
  You can use `du -h` to see how much space is actually consumed:

      $ ls -lah .vibe/instance.raw
      -rw-r--r--  1 dev  staff    100G Feb 11 21:57 .vibe/instance.raw

      $ du -h .vibe/instance.raw
      2.5G    .vibe/instance.raw

  If you need even more space within the VM, e.g., 500 GiB, run `truncate -s 500G .vibe/instance.raw` on your Mac and then within the VM run `growpart /dev/vda 1 && resize2fs /dev/vda1`.

- MacOS only lets binaries signed with the `com.apple.security.virtualization` entitlement run virtual machines, so `vibe` checks itself on startup and, if necessary, signs itself using `codesign`. SeCuRiTy!

- Debian "nocloud" is used as a base image because it historically booted directly to a root prompt.
  Newer nocloud images ask for a root password and run a firstboot wizard instead, so Vibe processes them once while provisioning (see [docs/how-vm-boot.md](docs/how-vm-boot.md)).
  The other images use [cloudinit](https://cloudinit.readthedocs.io/en/latest/), which I found much more complex:
  - Network requests are made during the boot process, and if you're offline they take several *minutes* to timeout before the login prompt is reached (thanks, `systemd-networkd-wait-online.service`).
  - Subsequent boots are much slower (at least, I couldn't easily figure out how to remove the associated cloud init machinery).

- Claude Code requires both your `~/.claude` folder (shared in the VM with `--mount-agents-conf`) and also the `~/.claude.json` file for auth credentials and session history.
  VirtioFS only works with folders, so there's not a nice way to "mount" the latter inside the VM.
  Here's what I recommend:
  - Run `claude` and login. (You can do this in a VM or on your actual machine if you trust `claude`.)
  - `mv ~/.claude.json ~/.claude/dot_claude_dot_json_should_have_been_here.json`
  - make a shell alias/script to launch Vibe as:

        vibe --mount-agents-conf --send "ln -fs ~/.claude/dot_claude_dot_json_should_have_been_here.json ~/.claude.json" \
             --send "IS_SANDBOX=1 claude --allow-dangerously-skip-permissions --dangerously-skip-permissions"


## Alternatives

Here's what I tried before writing this solution:

- [Sandboxtron](https://github.com/lynaghk/sandboxtron/) - My own little wrapper around Mac's `sandbox-exec`.
Turns out both Claude Code and Codex rely on this as well, and MacOS doesn't allow creating a sandbox from within a sandbox.
I considered writing my own sandboxing rules and running the agents `--yolo`, but didn't like the risk of configuration typos and/or Mac sandbox escapes (there are a lot --- I'm not an expert, but from [this HN discussion](https://news.ycombinator.com/item?id=42084588) I figured virtualization would be safer).

- [Lima](https://github.com/lima-vm/lima/), quick Linux VMs on Mac. I wanted to like this, ran into too many issues in first 30 minutes to trust it:
  - The recommended Debian image took 8 seconds to get to a login prompt, even after the VM was already running.
  - The CLI flags *mutate hidden state*. E.g., If you `limactl start --mount foo` and then later `limactl start --mount bar`, both `foo` and `bar` will be mounted.
  - Some capabilities are only available via yaml. E.g., the `--mount` CLI flag always mounts at the same path in the guest. If you want to mount at a different path, you have to do that via YAML.
  - There are many layers of inheritance/defaults, so even if you do write YAML, you can't see the full configuration.

- [Vagrant](https://developer.hashicorp.com/vagrant/) - I fondly remember using this back in the early 2010's, but based on this [2025 Reddit discussion](https://www.reddit.com/r/devops/comments/1axws75/vagrant_doesnt_support_mac_m1/) it seemed like running it on an ARM-based Mac was A Project and so I figured it'd be easier to roll my own thing.

- [Tart](https://tart.run/) - I found this via some positive HN comments, but unfortunately wasn't able to run the release binary from their GitHub because it's not signed.
They apparently hack around that when installing with homebrew, but I don't use homebrew either.
I tried cloning the repo and compiling myself, but the build failed with lots of language syntax errors despite the repo SHA is the same as one of their releases.
I assume this is a Swift problem and not Tart's, since this sort of mess happens most times when I try to build Swift. `¯\_(ツ)_/¯`

- [OrbStack](https://orbstack.dev/) - This looked nice, but seems mostly geared towards container stuff.
It runs a single VM, and I couldn't figure out how to have this VM run *without* my entire disk mounted inside of it.
I didn't want to run agents via containers, since containers aren't security boundaries.

- [Apple Container Framework](https://github.com/apple/container) - This looks technically promising, as it runs every container within a lightweight VM.
Unfortunately it requires MacOS 26 Tahoe, which wrecks [window resizing](https://news.ycombinator.com/item?id=46579864), adds [useless icons everywhere](https://news.ycombinator.com/item?id=46497712), and otherwise seems to be a mess.
Sorry excellent Apple programmers and hardware designers, I hope your management can reign in the haute couture folks before we all have to switch to Linux for professional computing.

- [QEMU](https://wiki.qemu.org/) - The first prototype of this app was [a single bash script](https://github.com/lynaghk/vibe/blob/1c82fd3b9fabf93abba2680fc856458e97a105cd/qemu.sh) wrapping `qemu`. This worked swimmingly, except for host/guest directory sharing, which ended up being a show-stopper. This is because QEMU doesn't support [virtiofs](https://virtio-fs.gitlab.io/) on Mac hosts, it only supports "9p", which is way slower ---  e.g., `mise use node@latest` takes > 10 minutes on 9p and 5 seconds on virtiofs.


## Roadmap / Collaboration

I wrote this software for myself, and I'm open to pull requests and otherwise collaborating on features that I'd personally use:

- resizing disk images
- running `vibe` against a disk image that's already running should connect to the already-running VM
  - the VM shouldn't shutdown until all host terminals have logged out
- if not the above, at least a check and throw a nice error message when you try to start a VM that's already running
- a way to make faster-booting even more minimal Linux virtual machines
  - this should be bootstrappable on Mac; i.e., if the only way to make a small Linux image is with Linux-only tools, the entire process should still be runnable on MacOS via intermediate VMs
- propagate an exit code from within VM to the `vibe` command
- a `--plan` flag which pretty-prints a CLI invocation with all of the default arguments shown
  - to keep ourselves honest, we should use the same codepath for the actual execution (maybe we can `exec` into the generated command?)
  - Being fully "explicit" is tricky due to flag interactions.
    E.g., the friendly `--mount` would need to be decomposed into two flags: One that exposes the host directory in the guest's staging area at `/mnt/root/virtiofs-shared/` and another flag `--send 'mount --bind ...'`to bind this to the desired guest location.

I'm not sure about (but open to discussing proposals via GitHub issues):

- running VMs in the background
- supporting Linux hosts
- supporting guests beyond Debian Linux
- using SSH as a login mechanism; this would eliminate the current stdin/stdout-to-console plumbing (yay!) but require additional setup/configuration (boo!)

I'm not interested in:

- anything related to Docker / containers / Kubernetes / distributed systems


When opening PRs, please ensure all commits have been formatted and pass tests. Run:

    /scripts/format.sh
    /scripts/test.sh
