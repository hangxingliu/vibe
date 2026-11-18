# How provisioning scripts run

`vibe provision` copies a base disk, boots that copy, and runs shell scripts in the guest. It then publishes the copy as a named image. This page is the contract for the files in `provisioning/` and for a script path you pass yourself. Disk copy, Alpine preparation, and publish checks are in [How a VM boots](how-vm-boot.md).

The scripts do not run on the host. The host logs in as root on the serial console. It writes each script into a read-only session share, and the root shell runs it from there.

## Which scripts run

`build.rs` reads every `provisioning/*.sh` at compile time. The script name is the file stem (`golang.sh` becomes `golang`). The file bytes are stored with `include_str!`. Rebuild `vibe` after you edit a built-in script. Provision a new image to apply it. An existing `.raw` file stays as it is.

`vibe --help` lists every built-in except `base`. The second line of the file is the description: leading `#` characters and surrounding whitespace are removed. The list is sorted by name.

```text
vibe provision [OPTIONS] [@name | path/to/script.sh ...]
```

Arguments after the word `provision` are scripts, in the order you write them.

| Argument | What is loaded |
| --- | --- |
| `@name` | The built-in with that stem. An unknown name prints the help list and stops. |
| `path/to/script.sh` | That file, read on the host when the CLI is parsed. The action name is the path. |

`provisioning/base.sh` is always inserted first. Extra arguments follow it, in the order written. `vibe provision` with no script arguments runs only `base`. `vibe provision @rust @codex my.sh` runs `base`, then `rust`, then `codex`, then `my.sh`. Passing `@base` runs that script a second time. The second run hits the history guard in `base.sh` and returns.

When `default.raw` is missing, a plain `vibe` creates it by running every built-in (`ensure_default_image`). `base` is first. `build.rs` does not sort the directory listing, so the other scripts follow that listing. Pass `@name` arguments when the order matters. `--image` defaults to `default`.

`vibe provision --repl` on an image that already exists, without `--replace`, boots that image and runs no scripts.

`--script` is a different feature. It is a login action for a normal `vibe` run. `provision` rejects it.

## Order inside the guest

`provision_image` (`src/provision.rs`) builds one `SessionPlan` (`build_provision_plan`). `expand_session` (`src/session_plan.rs`) runs the phases in this order. The shell is root the whole time the scripts run.

1. Legacy root login. When the guest prints `Password:`, the host types `--root-password` or `VIBE_ROOT_PASSWORD` with echo off. The password is never written into a script.
2. After login, that password is applied with `chpasswd` when one was given, so the published image has it. A non-root `--vm-user` is created when missing (`useradd`, bash, its own group) and checked. Provisioning a non-root user requires a root password.
3. VirtioFS shares are mounted. See [Shares](#shares) below.
4. `src/guest/install_bindfs.sh` runs from the session directory and installs `bindfs` when the image does not have it, so a later run can restrict the apt cache. In `vznat` mode the guest apt proxy is inside that script. The script runs in a child process and exports `DEBIAN_FRONTEND=noninteractive` itself.
5. `src/bashrc.sh` is written to `/root/.profile` and sourced. A non-root user also gets that file as `~/.profile`, written by `runuser` so the user owns it. Provision always does this. A normal run writes it only with `--write-bashrc`.
6. The root shell sources `provision-env.sh` from the session directory (template `src/guest/provision-env.sh`). It exports `DEBIAN_FRONTEND=noninteractive`, `VIBE_PROVISION_IMAGE`, `VIBE_PROVISION_BASE`, `VIBE_GIT_SHA`, `VIBE_BUILD_DATE`, `VIBE_PROVISION_SCRIPTS`, and `VIBE_PROVISION_VM_USER`.
7. The scripts run. Failures in this range are `[action N: ...]`, starting at 1.
8. A non-root provision then runs `check-vm-user-home` as root. Without `--repl`, the next script truncates `/etc/machine-id`. With `--repl`, that script installs a transient unit under `/run` that truncates the id at poweroff. The guest then prints `VIBE_PROVISION_SUCCESS`. Without `--repl`, the host types `systemctl poweroff`. These steps report as `[shutdown]`.

A non-root `--repl` switches to the vm-user after that success marker. `exit` returns to root. `exit` there powers off, and the image is saved. During the open shell the machine id is still the one from this boot.

## How one script is executed

Before boot, the host writes each script to `<cache>/sessions/<pid>-<nonce>/scripts/NNN-<name>.sh`. `NNN` counts the script actions from `000`. `<name>` is the basename, with characters outside `[A-Za-z0-9._-]` replaced by `_`. A built-in script split for a non-root user gets `.root.sh` and `.user.sh`. The guest sees that directory read-only, and only root can reach it. A run `--script` with a non-root `--vm-user` runs in that user's shell, so it is not staged and its text stays inline as base64.

`script_command` (`src/session/shell.rs`) renders `src/guest/script_wrapper.sh` and types one line into the root shell: ` eval "$(echo '<b64>' | base64 --decode)"`. The console shows only that line, not the wrapper or the script. Do not put secrets in a script: the host keeps a copy in the session directory. The root password is never part of that text.

The session directory is removed when the VM stops normally. After a failure, Vibe prints `Vibe kept the session scripts at <path>` and keeps the directory. Read the scripts there to see what ran. The next `SessionDir::create` removes a kept directory whose pid is gone.

The wrapper copies the script to `/tmp/vibe-scripts-<uid>/<id>.sh` and runs it. The directory is mode `700` and is owned by the uid that runs the script. `<id>` is `script_` plus the index in the full login-action list. Scripts should keep their state in the home directory or in `/etc`, not in `/tmp`.

When `scripts/include.sh` sits beside the staged script, the wrapper copies it to `/tmp/vibe-scripts-<uid>/include.sh`. The copy uses the same directory checks as the script. A failed copy prints `Vibe: cannot write include.sh into <dir>` and does not run the script. `vibe run` does not write `include.sh`.

The script process has stdin on `/dev/null`. A program started by the script must not read the console. A pipe inside the script still has its own stdin. Built-ins retry a pipe with `exec_retry bash -o pipefail -c 'curl ... | sh'`.

The host waits up to 24 hours for one script (`SCRIPT_ACTION_TIMEOUT`). The wrapper prints `VIBE_SCRIPT_STATUS_<id>` and the exit code. Exit 0 continues. Any other status stops the session. The image is not published. The work copy `<name>.tmp.<pid>` is deleted unless `VIBE_KEEP_FAILED_WORK=1`.

Each script is a new process. An `export` inside a script ends when that process exits. Pass later scripts a file.

Before each script, the root shell drops page cache and compacts memory. After exit 0, if `$HOME/.local/bin/mise` exists in that root shell, the root shell runs `mise activate bash`. That updates the root shell only.

A root script is executed directly (`"$__vibe_script"`). It inherits the root shell environment, including the sourced `/root/.profile`. It does not read `.profile` again.

A user script is different. From the user's home, root runs `runuser` and `bash --login -c 'exec ...'`. The login shell reads the user's `.profile`, then replaces itself with the script. The script inherits the exported environment. It does not read `.profile` again. The working directory is the user's home. Root's mise variables (`MISE_SHELL`, `__MISE_DIFF`, `__MISE_ORIG_PATH`, `__MISE_SESSION`) are unset for that process. `HOME`, `USER`, `LOGNAME`, and `SHELL` are set to the vm-user.

## The `# vibe:run-as-user` line

`RUN_AS_USER_MARKER` is the exact line `# vibe:run-as-user`. Trailing whitespace is ignored. The line must have no leading whitespace. The first match is the only split. A copy of that text inside `echo` or another comment does not split.

The host splits a script only when both are true:

- `--vm-user` is not root
- the script is a built-in: same name and same bytes as the embedded file

A path script fails the second check even when the bytes match a built-in. The marker is then an ordinary comment. The whole file runs as root.

For a built-in with a non-root user, `split_run_as_user` builds two scripts:

| Part | Contents | Who runs it |
| --- | --- | --- |
| Root | Bytes before the marker. | Root, and only when the part has a command. Blank lines and comments do not count. |
| User | The original `#!` line, then the bytes after the marker. The marker line itself is in neither part. | The vm-user, through `runuser`. |

The two parts are separate processes. They do not share functions or variables. Each built-in therefore repeats `set -euxo pipefail` and sources `include.sh` again after the marker.

With the default `--vm-user root`, there is no split. The file runs once, as root, marker included. Both halves share that process. Tools land under `/root`.

A split built-in uses two action numbers. The labels are `script 'name'` and `script 'name' as vm-user`.

`base` and `rust` have commands before the marker, so a non-root provision runs a root part and a user part. Every other built-in puts the marker above its commands. The root part is skipped. Only the user part runs.

After the scripts, a non-root provision runs `check-vm-user-home`. It lists paths under `$VIBE_VM_HOME` on that filesystem that are owned by uid 0, and it skips mount points. One such path aborts publish. The error prints at most 20 paths. Install user-level tools in the user part. Do not write that home as root.

## Environment

These variables are exported in the root shell before the scripts, except `VIBE_PROVISION_INCLUDE`. Child processes inherit the exported ones.

| Variable | When it is set | Value |
| --- | --- | --- |
| `VIBE_VM_USER` | Non-root `--vm-user` only | Account name |
| `VIBE_VM_UID` | Non-root `--vm-user` only | Uid from the guest |
| `VIBE_VM_GID` | Non-root `--vm-user` only | Gid from the guest |
| `VIBE_VM_HOME` | Non-root `--vm-user` only | Home from `getent` |
| `DEBIAN_FRONTEND` | Every provision | `noninteractive` |
| `VIBE_PROVISION_VM_USER` | Every provision | `--vm-user`, including `root` |
| `VIBE_PROVISION_IMAGE` | Every provision | Image name (file stem) |
| `VIBE_PROVISION_BASE` | Every provision | Host path of the base raw |
| `VIBE_GIT_SHA` | Every provision | Git SHA compiled into `vibe` |
| `VIBE_BUILD_DATE` | Every provision | UTC build date compiled into `vibe` |
| `VIBE_PROVISION_SCRIPTS` | Every provision | Script names, one per line, `base` first |
| `VIBE_PROVISION_INCLUDE` | Every provision script | `/tmp/vibe-scripts-<uid>/include.sh` |

The wrapper sets `VIBE_PROVISION_INCLUDE` on the script process only. The root shell does not keep it. A `--script` action on `vibe run` leaves the variable unset.

A root provision does not set `VIBE_VM_USER` or `VIBE_VM_HOME`. `base.sh` defaults them with `${VIBE_VM_USER:-root}` and `${VIBE_VM_HOME:-/root}`. A custom script should do the same.

`src/bashrc.sh` is the `.profile` written above. It exports `PATH` (cargo, bun, `~/.local/bin`), activates mise when `~/.local/bin/mise` exists, and sets the agent variables (`IS_SANDBOX`, `GEMINI_SANDBOX`, telemetry flags, and the rest of that file).

## Shares

Provision mounts cache shares only. It does not mount the project directory. It does not mount agent configuration (`--mount-agents-conf` is run-only).

| Share | Guest path | Host directory | Mounted unless |
| --- | --- | --- | --- |
| apt | `/var/cache/apt` | `<cache>/apt` (all users) | `--no-mount-sys-cache` |
| cargo, yarn, m2, go | `~/.cargo`, `~/.yarn`, `~/.m2`, `~/go` | `<cache>/user-<vm-user>/<name>` | `--no-mount-sys-cache` |
| mise | `~/.local/share/mise` | `<cache>/user-<vm-user>/mise-cache` | `--no-mount-mise` |

`~` is the home of `--vm-user`. During provision the apt share is a normal bind mount. A later run as a non-root user mounts it through `src/guest/mount_apt_cache.sh` when the image has `bindfs`.

`--no-mount-sys-cache` and `--no-mount-mise` write those tools into the image. Run that image with the same flag. Otherwise the share hides the copies inside the image.

## Proxy

In `nat` mode the `vibe-usernet` helper proxies guest TCP. `guest_proxy` is empty. Scripts receive no proxy variables.

In `vznat` mode the host fills the `INJECT_PROXY_CODE` region of `base.sh` before the split (`replace_region_if_present` in `src/guest_template.rs`). A file without that region is left unchanged. The replacement appends `Acquire::http::Proxy` and `Acquire::https::Proxy` to `/etc/apt/apt.conf.d/99timeout`, then exports `http_proxy`, `https_proxy`, `no_proxy`, and the upper-case names. That region is in the root part. The exports last for that one process. The apt file remains in the image, including credentials when the URL has them. Later scripts, and the user part of `base`, do not inherit those exports. `curl` and `mise` in those scripts do not read the apt file.

Leave the region empty in the source file. Keep it after the lines that create `99timeout`. A `vznat` provision replaces the whole region.

## Rules for a built-in script

Match the files already in `provisioning/`.

- Name the file `provisioning/<name>.sh`. `@<name>` selects it.
- Line 1 is a bash shebang (`#!/bin/bash` or `#!/usr/bin/env bash`). The user part is started with that line.
- Line 2 is `# One short description`. That text is the help line.
- Put `# vibe:run-as-user` on its own line. Every built-in has this line.
- Put `apt` and other system setup before the marker.
- Put mise, rustup, and other per-user tools after the marker.
- Start the user part with `set -euxo pipefail`, then `source "${VIBE_PROVISION_INCLUDE:?run this script with vibe provision}"`. The test `builtin_scripts_install_user_tools_as_the_vm_user` requires that `set` line. It rejects the text `apt ` in the user part.
- Call `exec_retry cmd` for one external command. Call `exec_retry bash -o pipefail -c 'curl ... | sh'` for a pipeline. `exec_retry` retries up to three times, waits 10 seconds between tries, then exits 1.
- The `||` inside `exec_retry` turns off `errexit` in a shell function it calls. Pass external commands only.
- For a local run, use `source "$(dirname "$0")/include.sh"`. Put a copy of `src/guest/provision_include.sh` beside the script. `source ./include.sh` reads the working directory. A user script's working directory is `$VIBE_VM_HOME`.
- Write the user's files as the user, under `$HOME` or the working directory (the user part starts in the user's home).
- Do not read stdin.
- Exit non-zero to fail the provision.
- Two-space indent. `scripts/format.sh` formats the `scripts/` directory with `shfmt --indent 2`. Keep `provisioning/` in that shape. `scripts/test.sh` does not scan `provisioning/`.

A path script follows the same guest wrapper, with three differences. The marker does not split it. It always runs as root. To run a command as the vm-user, call `runuser -u "${VIBE_VM_USER:-root}"`.

## What `base.sh` does

`base` runs before every other script. Later scripts assume it has prepared the image.

On every run it appends a block to `/root/vibe-image-history.txt` (time, image, base path, git SHA, build date, vm-user, script names).

When that file was already non-empty, the root part returns before packages. It warns when this vm-user has no `~/.local/state/vibe/base-user.done` and no executable `~/.local/bin/mise`. It does not create `/run/vibe/base-first-run`. With `--vm-user root`, that `exit` ends the process, so the lines after the marker do not run. With another user, the user part is a second script and it returns because the flag file is missing. Tools from the earlier provision stay as they were. A new account on an old image does not get them. Create that account from a fresh base: `vibe provision --image <new-name> --vm-user <name> --root-password <password>`.

On the first run the root part:

- sets short apt timeouts in `/etc/apt/apt.conf.d/99timeout`
- adds Debian Fast Forward and upgrades
- installs packages (`build-essential`, `curl`, `git`, `ripgrep`, `vim`, `tmux`, `openssh-server`, and the rest of the list in the file)
- disables the `ssh` service, allows root login, and turns password login off
- runs `growpart /dev/vda 1` and `resize2fs /dev/vda1` (the host has extended the raw file to 12 GiB when the copy was shorter)
- sets the hostname to `vibe`
- creates `/run/vibe/base-first-run` for this boot (`/run` is not in the published image)

The user part returns unless `/run/vibe/base-first-run` exists and `~/.local/state/vibe/base-user.done` does not. Otherwise it installs mise, writes `~/.config/mise/config.toml` (`uv` 0.11.3, `fzf`, `node` 24, `usage`), runs `mise install`, and creates the done stamp.

## The other built-in scripts

Help text is the second line of each file. "User part" is what a non-root provision runs. A root provision runs the whole file as root.

| `@name` | Root part | User part |
| --- | --- | --- |
| `rust` | `apt install sccache` | rustup (minimal, plus `rustfmt` and `clippy`) |
| `golang` | none | `mise use -g go@latest` |
| `claude` | none | mise tool `claude` |
| `codex` | none | `mise use -g npm:@openai/codex@latest` |
| `copilot` | none | GitHub Copilot install script |
| `gemini` | none | Antigravity install script |
| `grok` | none | xAI CLI install script |
| `pi` | none | `mise use -g npm:@earendil-works/pi-coding-agent@latest` |

Those user parts call `mise` or `curl`. They depend on the first-run user part of `base`.
