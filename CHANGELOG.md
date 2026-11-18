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
- `--proxy <URL>` routes the VM's network connections and the host-side image downloads through an HTTP or SOCKS5 proxy (`http://`, `https://`, `socks5://`, `socks5h://`; a URL without a scheme gets `http://`).
  With the default `nat` network mode, outbound TCP is proxied transparently by `vibe-usernet` and never falls back to a direct connection.
  With `vznat`, the proxy is exported to the guest as `http_proxy`/`https_proxy` and `vibe provision` saves it in the new image's `apt` configuration (SOCKS5 proxies as `socks5h://`, which is what apt supports).
- `--proxy-bypass <LIST>` sets the destinations reached directly (IPs, CIDRs, `192.168.*`-style prefixes, `localhost`, `*`, `host:port`). It replaces the built-in list of loopback, link-local, and private ranges. Domain names only apply to host downloads. Traffic to the host through the gateway is always direct.
- `--[no-]proxy-udp` routes the VM's outbound UDP through a SOCKS5 proxy (on by default for SOCKS5 proxies).
- `--[no-]proxy-dns` sends the gateway DNS to its upstreams through a SOCKS5 proxy over TCP (on by default for SOCKS5 proxies); upstreams in the bypass list are queried directly. Upstreams that fail through the proxy answer `SERVFAIL`.
- `--[no-]strict-proxy` drops VM traffic that cannot go through the proxy: unproxied UDP, gateway DNS, and ICMP only reach the bypass list. When no gateway DNS upstream is in the bypass list, queries are answered with `REFUSED`.
- `--no-proxy-vm` keeps the proxy away from the VM's traffic; host downloads still use it.
- A startup summary shows the VM's proxy, where it came from, how TCP, UDP, DNS (per upstream), and ICMP are handled, and the bypass list. See [docs/network-proxy.zh-Hans.md](docs/network-proxy.zh-Hans.md).
- `--dns <ADDR>` sets custom upstream DNS servers for the VM (repeatable).
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
- The proxy options, `--dns`, `--network`, and `-p`/`--publish` work with `vibe provision` and may be placed before or after `provision`.
- **Breaking:** Without `--proxy`, the proxy in `https_proxy`, `HTTPS_PROXY`, `http_proxy`, or `HTTP_PROXY` is used for the VM's traffic, for host downloads, and (with `vznat`) saved by `vibe provision` into the new image. `no_proxy`/`NO_PROXY` becomes the bypass list. Use `--no-proxy-vm` to keep the VM unproxied.
- **Breaking:** `no_proxy`/`NO_PROXY` (or `--proxy-bypass`) replaces the built-in bypass list instead of being ignored, so ranges such as `10.0.0.0/8` that used to be reached directly go through the proxy when they are not listed. Add them to `no_proxy` or `--proxy-bypass` to keep them direct.
- **Breaking:** With a SOCKS5 proxy, UDP is proxied by default; use `--no-proxy-udp` for the old behavior.
- **Breaking:** Host-side downloads always pass `--proxy` to curl, so `ALL_PROXY` is no longer used. Set `--proxy` or `https_proxy` instead.
- With `vznat`, the guest's `no_proxy`/`NO_PROXY` is the bypass list (it was always `127.0.0.1,localhost,192.168.*`), and `vibe provision` also saves a proxy that comes from the environment into the new image.
- The provisioning VM uses 4096 MB of RAM by default (was 2048 MB).
- Vibe's own agent configuration directories are kept in the cache directory and no longer mix with the host's configuration (credential isolation).
- Every boot writes a `.bash_logout` that powers off the VM on `exit`, and `vibe` exits promptly when the guest powers off.
- **Breaking:** The agent configuration directories (codex, copilot, claude, gemini, pi, grok, opencode) and the tmux configuration are no longer mounted by default. Pass `--mount-agents-conf` and `--mount-tmux-conf`.
- **Breaking:** Running a VM no longer rewrites or sources `/root/.profile`. Pass `--write-bashrc` to rewrite it with Vibe's built-in configuration and source it right away. Provisioning still writes and sources it.
- **Breaking:** While `default.raw` does not exist yet, `vibe` refuses `--no-mount-sys-cache` and `--no-mount-mise`. Create the default image explicitly with `vibe provision` first.

### Removed

- **Breaking:** `--no-default-mounts`. Use `--no-mount-cwd`, `--no-mount-sys-cache`, and `--no-mount-mise` instead.
  The `.vibe`/`.env`/`.git` masking and the `node_modules` share of project directories can no longer be turned off.

