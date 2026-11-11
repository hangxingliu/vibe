# Changelog

All notable changes to this fork of Vibe are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The original repository is <https://github.com/lynaghk/vibe>.
Version 1.0.0 lists everything this fork changes compared to it (upstream version 0.1.0, as of commit `0858d69`).

## 1.0.0 - 2026-11-12 (WIP)

### Added

- `-p` / `--publish [udp:][HOST_ADDR:]HOST_PORT:GUEST_PORT` forwards guest TCP or UDP ports to the host (repeatable).
  Example: `-p 127.0.0.1:8080:80 -p udp:127.0.0.1:9002:9002`.
- `--proxy <URL>` routes the VM's network connections through an HTTP or SOCKS5 proxy.
  With the default `nat` network mode, outbound TCP is proxied transparently by `vibe-usernet`; loopback, link-local, and private destinations are dialed directly.
  With other network modes, the proxy is configured through environment variables and `apt` settings inside the VM.
- `--proxy-udp` also routes outbound UDP through the SOCKS5 proxy given with `--proxy`.
- `--dns <ADDR>` sets custom upstream DNS servers for the VM (repeatable). With a `socks5://` proxy, queries to non-local upstreams are tunnelled through it.
- `--ssh-key <PUBLIC_KEY_FILE>` installs a public key in the VM and starts its SSH server.
- `--git <rw | ro | no>` controls access to the project's `.git` directory. The default `ro` shares it read-only; `no` hides it behind a tmpfs mount.
- Project directories (the current directory and `--mount` shares) get `.env` files and `.vibe` subfolders masked with tmpfs, and their `node_modules` directories are kept in a dedicated guest cache instead of on the host.
- Fine-grained control over default mounts: `--[no-]mount-cwd`, `--[no-]mount-sys-cache`, `--[no-]mount-mise`, `--[no-]mount-tmux-conf`, and `--[no-]mount-agents-conf`. The last form given wins.
- `--mount-tmux-conf` shares the host `~/.tmux` read-only and copies `~/.tmux.conf` into the VM.
- `--write-bashrc` rewrites `/root/.profile` with Vibe's built-in bash configuration and sources it.
- `vibe provision --repl` drops into an interactive shell after the provisioning scripts finish, so you can customise the image before it is saved. With an existing image (and no `--replace`), it boots that image for interactive use.
- Built-in provisioning scripts `@copilot`, `@golang`, and `@grok`.
- `VIBE_EXTRA_IMAGE_DIR` and `VIBE_PREFERRED_IMAGE_DIR` let you keep images in other directories, for example on an external drive. See [Keeping images on another disk](readme.md#keeping-images-on-another-disk).
- `vibe` and `vibe provision` print the directories shared into the VM, the files written into it, and the image directory lookup order before the VM starts.
- Keystrokes typed while login actions (`--send`, `--expect`, ...) are still running are discarded. Pressing Ctrl-C twice during that time stops the VM.
- Clearer errors when a VM never reaches a root shell, with a hint to run `vibe provision` for images that need it.

### Changed

- Newer Debian nocloud base images (which ask for a root password and run a firstboot wizard) are now supported. `vibe provision` prepares the base once in a headless Alpine maintenance VM before running scripts. Running an existing disk never inspects or modifies it.
- `--proxy`, `--proxy-udp`, `--dns`, `--network`, and `-p`/`--publish` work with `vibe provision` and may be placed before or after `provision`.
- The provisioning VM uses 4096 MB of RAM by default (was 2048 MB).
- Vibe's own agent configuration directories are kept in the cache directory and no longer mix with the host's configuration (credential isolation).
- Every boot writes a `.bash_logout` that powers off the VM on `exit`, and `vibe` exits promptly when the guest powers off.
- **Breaking:** The agent configuration directories (codex, copilot, claude, gemini, pi, grok, opencode) and the tmux configuration are no longer mounted by default. Pass `--mount-agents-conf` and `--mount-tmux-conf`.
- **Breaking:** Running a VM no longer rewrites or sources `/root/.profile`. Pass `--write-bashrc` to rewrite it with Vibe's built-in configuration and source it right away. Provisioning still writes and sources it.
- **Breaking:** While `default.raw` does not exist yet, `vibe` refuses `--no-mount-sys-cache` and `--no-mount-mise`. Create the default image explicitly with `vibe provision` first.

### Removed

- **Breaking:** `--no-default-mounts`. Use `--no-mount-cwd`, `--no-mount-sys-cache`, and `--no-mount-mise` instead.
  The `.vibe`/`.env`/`.git` masking and the `node_modules` share of project directories can no longer be turned off.

