# Vibe: Project Context & Instructions

Vibe is a lightweight, zero-configuration tool designed to spin up Linux virtual machines on ARM-based Macs to sandbox LLM agents. It prioritizes speed, security, and a minimal footprint.

## Project Overview

- **Purpose:** Provides a secure, isolated environment for LLM agents (like Claude Code, Codex, or Gemini CLI) to run commands without risking the host system.
- **Core Technology:** Built with **Rust** using Apple's **Virtualization framework** (via `objc2` interop).
- **Guest OS:** Uses a **Debian Linux** (nocloud) base image, optimized for fast boot (~10s).
- **Architecture:**
    - **Single Binary:** The entire tool is a single binary. The `vibe-usernet` network helper and the provisioning scripts are embedded in it.
    - **Direct Console:** Attaches the host terminal directly to the VM's serial console.
    - **Directory Sharing:** Uses **VirtioFS** for high-performance directory sharing between host and guest.
    - **Networking:** The default `nat` mode spawns the bundled Go user-mode network helper `vibe-usernet` (built from `helpers/vibe-usernet` on top of the `helpers/gvisor-tap-vsock` submodule), which survives host network changes. `vznat` uses Apple's `VZNATNetworkDeviceAttachment` instead.
- **Disk preparation:** New Debian nocloud images no longer ship an empty root password and stop in firstboot. `vibe provision` (including the implicit first `default.raw`) processes its private work copy of the base once in a headless Alpine maintenance VM (clear `/etc/machine-id`, empty the root password field) before running scripts. Running a disk (`disk.raw`, `.vibe/instance.raw`, `--image`) never probes or modifies it.
- **Security:** Automatically signs itself with the `com.apple.security.virtualization` entitlement if missing.

## Key Files & Directories

- `src/main.rs`: Entry point, CLI parsing (via `lexopt`), help text, and `main()` dispatch for run/provision (project directory shares and login actions). Its `#[cfg(test)] mod tests` covers CLI parsing.
- `src/vm_io.rs`: Host terminal and console I/O: `OutputMonitor` (cancellable waits returning `WaitResult::{Found, Timeout, Closed}`, `wait_for_any`, `close()`), `spawn_vm_io`, raw mode, terminal resize forwarding, and the input gate (`decide_user_input`: discard keystrokes until automatic actions finish; two consecutive Ctrl-C bytes while the gate is closed request a stop).
- `src/vm_config.rs`: Virtualization framework configuration. `VmSpec` (`DiskSpec`, `SerialRole`, `ShareDevice`), `create_vm_configuration`, `default_interactive_spec` (the historical single-disk interactive layout), EFI variable store, and `nsurl_from_path`.
- `src/vm_lifecycle.rs`: Starting a configured VM (`start_vm`), requesting a stop (`request_stop`), forcing a stop (`force_stop`), and `run_headless` (log file, control frames, phase deadlines).
- `src/vm_run.rs`: `run_vm()`: boots a disk, performs the root login and internal setup, then runs the login actions. Returns a `RunOutcome` (all actions completed, VM stopped normally, and the prefix of the caller action that was still running); action errors use `[action N: label]` (N counts the caller's actions from 1), Ctrl-C and forced stops use `[shutdown]`, and VM configuration or start errors use `[boot]`; fails with a `[boot]`/`[login]` diagnostic when the VM never reaches a root shell. It closes the `OutputMonitor` before joining the actions thread, so the host exits promptly when the guest powers off. `drive_interactive` is shared with `src/vm_checks/devices.rs`.
- `src/session.rs`: `LoginAction`, the login actions thread, `legacy_login_prefix()` (root login, `Password:` detection, readiness probe, internal setup), `LoginFailure` diagnostics, `LoginAction::label()` and `action_prefix()` (never include `--send` text, which may hold env values), `DiskSource` and `needs_provision_hint()`, and helpers that build guest shell commands.
- `src/image_dirs.rs`: `ImageDirs`, built once in `run_main` from `VIBE_EXTRA_IMAGE_DIR` (`:` separated absolute paths) and `VIBE_PREFERRED_IMAGE_DIR` (the default cache directory or an extra entry; must exist). Lookup order is preferred, default, then the other extras; missing extras are skipped. `find_file`, `existing`, and `describe` (the list printed at run/provision start and at the end of `--help`). Errors use the `[image-dirs]` prefix. Shares, `vibe-usernet`, and maintenance logs stay in the default directory.
- `src/images.rs`: Base image download and verification, image names/paths (`find_named_image`, `new_image_path`, `locate_debian_base` across the image directories), and instance disks. `fetch_verified` and `extract_disk_raw` are shared with the Alpine cache. The Debian download keeps its size check; its errors use the `[download]` prefix.
- `src/alpine.rs`: Pinned Alpine maintenance image (`ensure_alpine_image`): download, SHA-512, extract, and check partition 2's ext4 UUID. Download and repair happen only in the preferred image directory; a raw in another image directory is reused read-only when its UUID matches and otherwise skipped with a warning. The cached raw is a read-only input. Errors use the `[download]` prefix.
- `src/seed.rs`: NoCloud seed ISO (`build_seed`, `Manifest::serialize`). Writes `meta-data`, `user-data`, and `manifest` (`policy=minimal`), then `hdiutil makehybrid` with volume name `cidata`.
- `src/guest/maint.sh`: POSIX maintenance script run by tiny-cloud as `user-data`. Clears `/etc/machine-id` and the root password field. It does not print shadow contents.
- `src/maintenance.rs`: `process_disk`: host safety checks, Alpine clone, seed ISO, headless maintenance VM, step-order checks, log publish (to `logs/` in the default directory, with a header of full paths), and `verify_processed`. `maint/` is created next to the Alpine raw, falling back to the preferred image directory. `PrepareError` displays as `[prepare] <stage> (last PREP-STEP: ...)` or, for the host re-check, `[verify] host-verify`, followed by the published log path and, with `VIBE_KEEP_FAILED_WORK=1`, the kept `maint/<pid>-<nonce>/` directory. Provision calls it from `ensure_bootable`. The run path does not.
- `src/disk_prepare.rs`: `ensure_bootable` for a provision work copy: probe, skip or warn (`[probe]` warning for unsupported disks), or run Alpine maintenance. Not used by run.
- `src/provision.rs`: Provisioning flow (`ensure_default_image`, `provision_image`). Copies the base, calls `ensure_bootable` before growing the disk, then clears `/etc/machine-id` before publish. Errors use `[prepare]`, `[action N: label]`, `[shutdown]`, and `[verify]`. With `VIBE_KEEP_FAILED_WORK=1` a failed work copy `<name>.tmp.<pid>` is kept and its path is printed; otherwise it is removed.
- `src/gpt.rs`: Read-only GPT parser (`read_gpt`, `select_layout`, `scan_labels`) and filesystem label probes for ext4, FAT, and ISO9660. Alpine unpack, maintenance, and provision (through `ensure_bootable`) call it. The run path does not.
- `src/ext4ro.rs`: Bounded read-only ext4 reader (`Ext4Ro::open`, `read_file`, `lstat`) using `pread`. Symlinks are returned and not followed. Provision reads through `image_probe`. The run path does not.
- `src/control.rs`: Maintenance control frames (`PREP-READY`, `PREP-STEP`, `PREP-RESULT`), incremental `FrameReader`, and `new_nonce`. `run_headless` feeds hvc1 to `FrameReader`. Step order is enforced by `maintenance::observe_frame`.
- `src/cache.rs`: `FileLock` (`flock`), exclusive temp files, `clonefile` with a copy fallback, same-directory `replace`, `private_dir`, and `sweep_stale`. Alpine download takes `alpine.lock`. Maintenance locks the work copy through a sibling `{disk}.lock` (the disk image itself is not flocked; Virtualization.framework attaches it) and clones the Alpine raw into `maint/<pid>-<nonce>/` beside it. `ensure_bootable` calls that path and then removes the sibling lock file.
- `src/image_probe.rs`: `probe_disk`, pure `classify`, and `verify_processed`. Shadow plaintext is not stored. `process_disk` re-checks with `verify_processed`. Provision probes its work copy and the finished image. The run path does not call it. `VIBE_TRACE_PROBE` logs each call for tests.
- `tests/fixtures/make-ext4.sh`: Rebuilds the small gzip ext4 images used by the `ext4ro` and `image_probe` tests. The images are gitignored. Those tests run this script when any required image is missing.
- `src/networking.rs`: `NetworkMode` (`Nat` through the embedded `vibe-usernet` helper, `VzNat` through Apple's NAT attachment), proxy and DNS options, and port publishing.
- `src/vm_checks/`: VM checks for the `#[ignore]` integration tests, selected with `VIBE_VM_CHECK` and run by `main()` on the process main thread (Virtualization.framework needs it; `cargo test` threads are not the main thread). `devices.rs` (`read-only-disk`, `headless-timeout`), `maintenance.rs` (`maintenance-<name>`, for example `maintenance-all`), and `prepare.rs` (`probe`, `prepare` on `VIBE_VM_CHECK_DISK` with the Alpine cache at `VIBE_VM_CHECK_CACHE`). Not part of the CLI.
- `provisioning/base.sh`: Shell script used to initialize the default Debian image with essential tools (gcc, rust, mise, etc.).
- `src/bashrc.sh`: Guest-side bash configuration.
- `src/entitlements.plist`: Entitlements required for macOS virtualization.
- `build.rs`: Builds the Go `vibe-usernet` helper with `go build` (a stub on non-Apple hosts), embeds it and the provisioning scripts, and records the git SHA and build date.
- `helpers/vibe-usernet/`, `helpers/gvisor-tap-vsock/`: Source of the user-mode network helper (the latter is a git submodule).

## Building and Running

### Prerequisites
- ARM-based Mac (M1/M2/M3/M4).
- macOS 13 (Ventura) or higher.
- `codesign` tool (part of Xcode/Command Line Tools).

### Commands
- **Build:** `cargo build --release`
- **Run:** `cargo run -- [OPTIONS] [disk-image.raw]`
- **Test:** `cargo test` (CLI parsing is covered by `#[cfg(test)] mod tests` in `src/main.rs`; image directory parsing and lookup are covered in `src/image_dirs.rs` and `src/images.rs`; `OutputMonitor`, the input gate, and the login/actions logic have `#[cfg(test)] mod tests` in `src/vm_io.rs` and `src/session.rs`; GPT, ext4, control frames, cache helpers, and image classification have `#[cfg(test)] mod tests` in `src/gpt.rs`, `src/ext4ro.rs`, `src/control.rs`, `src/cache.rs`, and `src/image_probe.rs`; `VmSpec` shape and headless phase deadlines have tests in `src/vm_config.rs` and `src/vm_lifecycle.rs`; Alpine cache, seed manifest, and maintenance step order have tests in `src/alpine.rs`, `src/seed.rs`, and `src/maintenance.rs`. Prepare decisions and the machine-id finalize scripts have tests in `src/disk_prepare.rs` and `src/provision.rs`. Real disk images are `#[ignore]` and need `VIBE_TEST_DEBIAN_NEW_RAW`, `VIBE_TEST_DEBIAN_OLD_RAW`, and `VIBE_TEST_ALPINE_RAW`. Booting VMs is integration-heavy and requires a Mac host; `tests/vm_devices.rs` (`VIBE_VM_CHECK=read-only-disk|headless-timeout`), `tests/alpine_maintenance.rs` (`VIBE_VM_CHECK=maintenance-all`), and `tests/disk_prepare.rs` (`VIBE_VM_CHECK=prepare`) are `#[ignore]` and spawn the `vibe` binary so the check runs on its main thread. The maintenance checks use e2fsprogs from `VIBE_E2FS_SBIN` or `PATH`. The ext4 and probe tests run `tests/fixtures/make-ext4.sh` when a required `tests/fixtures/*.img.gz` file is missing; that script looks up `mkfs.ext4`, `debugfs`, and `e2fsck` via `MKFS_EXT4`, `/tmp/e2fs-install`, Homebrew `e2fsprogs`, or `PATH`).

## Development Conventions

- **Minimal Dependencies:** Maintain the "small binary" philosophy. Avoid adding heavy crates. Current dependencies are primarily `objc2`, `lexopt`, `libc`, and `regex`.
- **Senior Engineering Standards:** Code should be clean, idiomatic Rust, and follow existing patterns (e.g., surgical `objc2` usage).
- **No Emoji:** The CLI and documentation avoid emoji to maintain a professional, minimalist aesthetic.
- **Human-Centric:** Documentation (like the README) is written by humans for humans.
- **macOS Interop:** Be mindful of macOS-specific behaviors, especially entitlements and security boundaries.

## Common Tasks

- **Adding a Default Mount:** Modify `main.rs` in the `directory_shares` section of `main()`.
- **Updating Guest Provisioning:** Edit `provisioning/base.sh`. Note that this only affects the *first* run when `default.raw` is created.
- **Adjusting Networking:** Look into `src/networking.rs` and the `NetworkMode` enum.
- **Fixing Entitlement Issues:** `main.rs` contains an `ensure_signed()` function that handles self-signing and re-execution.
