# Vibe: Agent Instructions

Vibe is a single Rust binary that boots Debian Linux VMs on ARM Macs to sandbox LLM agents. It uses Apple's Virtualization framework through `objc2`. The terminal attaches to the VM serial console. Host directories reach the guest through VirtioFS.

## Commands

- Build: `cargo build --locked` (CI release: `cargo build --release --locked`).
- Test: `cargo test`.
- Lint: `cargo fmt --check`, `cargo clippy`, `shellcheck` and `shfmt -d` on changed shell scripts. Tool versions live in `mise.toml`.
- Help text: after changing it, copy `vibe --help` output into the help block of `USAGE.md`. Omit the dynamic "Cache directory" and "Image directories" tail.

## Tests

- After you finish the code work of a plan in `docs/ai-plans/`, delete tests that only checked a refactor or an intermediate step.
- Keep tests that guard core behavior: parsing, state machines, disk reads, locks, downloads, proxy rules, and the invariants below.
- Do not test constants, fixed strings, or simple functions that their comment already explains.
- Do not test test-only helpers.

## Module map

Read the module before you change it. Each line names the entry symbols.

- `src/main.rs`: constants, built-in script table, `run_main` (builds `CommandContext` and dispatches).
- `src/cli.rs`: CLI parsing (`lexopt`), `CliArgs`, `CliCommand`, `parse_cli_from`. Tests in `src/cli/tests.rs`.
- `src/help.rs`: `help_text`, `REPO_URL`, the section renderer (`render_section`, 90-column limit), `provisioning_scripts_banner`.
- `src/run_command.rs`, `src/provision_command.rs`: `run_run` and `run_provision`, the two command bodies of `run_main`. `run_run` writes `src/guest/install_ssh_key.sh` and, for a non-root user with the apt share, `src/guest/mount_apt_cache.sh`.
- `src/shares.rs`: cache shares (`CacheShare`, `cache_share_specs`, `legacy_cache_notice`), project masks (`project_masks_and_shares`), `DirectoryShare`, `describe_vm_files`.
- `src/signing.rs`: `ensure_signed`.
- `src/vm_user.rs`: `VmUser`, `GuestPath`, guest checks `src/guest/check_vm_user.sh` and `src/guest/ensure_vm_user.sh` (exit codes 10-15 map to `VmUserProblem`).
- `src/guest_template.rs`: `replace_region`, `replace_region_if_present`, `shell_assign`, `sh_command`.
- `src/session_dir.rs`: `SessionDir` (`stage_scripts` writes `scripts/NNN-<name>.sh`), `GuestFile`. Directory `<cache>/sessions/<pid>-<nonce>/` (mode 0700), shared read-only and not bind-mounted.
- `src/session_plan.rs`: `SessionPlan` phases and `expand_session`. Share staging, bind mounts (not for a staging-only share), masks, `src/guest/mount_apt_cache.sh`, `src/guest/install_bindfs.sh`, switch to the vm-user.
- `src/session.rs`: `LoginAction`, `legacy_login_prefix`, re-exports of `src/session/*`: `password.rs` (`RootPassword`), `failure.rs` (`LoginFailure`), `shell.rs` (`script_command` renders `src/guest/script_wrapper.sh`), `login.rs` (`legacy_login`, `switch_user`), `actions.rs` (login thread).
- `src/vm_run.rs`, `src/vm_lifecycle.rs`, `src/vm_config.rs`, `src/vm_io.rs`: boot and drive a VM, headless runs, VM configuration, terminal I/O and input gate.
- `src/provision.rs`: `ensure_default_image`, `provision_image`, `build_provision_script`, `split_run_as_user`. `build_provision_plan` writes `src/guest/install_bindfs.sh`, `src/guest/provision-env.sh`, and `scripts/include.sh` (source `src/guest/provision_include.sh`). Guest scripts `src/guest/check_vm_user_home.sh`, `src/guest/clear_machine_id.sh`, `src/guest/repl_finalize.sh`.
- `src/disk_prepare.rs`, `src/maintenance.rs`, `src/alpine.rs`, `src/seed.rs`, `src/guest/maint.sh`, `src/control.rs`: Alpine maintenance VM that prepares new Debian bases.
- `src/image_probe.rs`, `src/gpt.rs`, `src/ext4ro.rs` (with `src/ext4ro/`), `src/cache.rs`: read-only disk inspection, file locks, clones.
- `src/images.rs`, `src/image_dirs.rs`: image download, names, lookup across image directories.
- `src/networking.rs`, `src/proxy.rs`: network modes, `vibe-usernet` arguments, proxy resolution.
- `src/vm_checks/`: VM checks (maintenance checks in `src/vm_checks/maintenance/`) for the `#[ignore]` integration tests in `tests/`. Not part of the CLI.
- `provisioning/*.sh`: built-in scripts, embedded by `build.rs`. `base.sh` always runs.
- `helpers/vibe-usernet/`: Go network helper, built by `build.rs` on top of the `helpers/gvisor-tap-vsock` submodule.

## Invariants

- Keep the run path read-only on disks. Only provision probes or modifies a disk (`ensure_bootable`). Running a disk never calls `gpt`, `ext4ro`, or `image_probe`.
- Run Virtualization.framework code on the process main thread. `cargo test` threads are not; integration tests spawn the `vibe` binary with `VIBE_VM_CHECK`.
- Never print the root password. `RootPassword` prints `***`. Never put it on a guest command line; type it with `SecretSend`. Action labels never include `--send` text.
- Prefix errors with their stage: `[boot]`, `[login]`, `[action N: label]`, `[shutdown]`, `[prepare]`, `[verify]`, `[download]`, `[image-dirs]`, `[probe]`.
- Login as root first, mount shares, then switch to a non-root `--vm-user`. Relative `GuestPath`s resolve below that user's HOME.
- Keep `apt` a `CacheShare::global`. Other caches are `CacheShare::user` (`<cache>/user-<vm-user>/<name>`).
- Mark the start of user-level work in built-in scripts with `# vibe:run-as-user`. Custom scripts are never split and run as root.
- Never write secrets into the session directory. It holds scripts and the SSH public key only.
- Keep dependencies small: `objc2` crates, `lexopt`, `libc`, `regex`, `base64`. Ask before adding a crate.
- Use no emoji in CLI output or docs. The readme is written by humans; keep edits short and in its voice.

## Integration tests

These tests are `#[ignore]` and need a Mac host. Run them with `cargo test -- --ignored <name>`.

- `tests/vm_devices.rs`: no extra input.
- `tests/alpine_maintenance.rs`: e2fsprogs from `VIBE_E2FS_SBIN` or `PATH`.
- `tests/disk_prepare.rs`: `VIBE_VM_CHECK_DISK`, `VIBE_VM_CHECK_CACHE`.
- `tests/vm_user.rs`: `VIBE_VM_CHECK_DISK` (root without password); proxy and DNS options in `VIBE_VM_CHECK_ARGS`.
- Real-image unit tests: `VIBE_TEST_DEBIAN_NEW_RAW`, `VIBE_TEST_DEBIAN_OLD_RAW`, `VIBE_TEST_ALPINE_RAW`.
- `tests/fixtures/make-ext4.sh` rebuilds the gitignored ext4 fixtures. Tests run it when a fixture is missing.

## Common tasks

- Add a default mount: add a `CacheShare` to `SYS_CACHE_SHARES` or `AGENT_CONF_SHARES` in `src/shares.rs`. Use a relative guest path for per-user data.
- Change guest provisioning: edit `provisioning/base.sh`. Existing `default.raw` images do not change.
- Change proxy rules: edit `src/proxy.rs` and the helper side in `helpers/gvisor-tap-vsock/pkg/services/{noproxy,netproxy,dns,forwarder}` and `pkg/virtualnetwork/proxy.go`. Update `docs/network-proxy.md`.
- Change shares or masks: update `docs/file-shares-between-vm.md`.
- Change a guest script that Rust fills in: edit `src/guest/*.sh`. The header names the `# region INJECT_<NAME>_CONFIG` block that Rust replaces.
- Change boot, login, or provision flow: update `docs/how-vm-boot.md`.
- Keep each `*.zh-Hans.md` file in sync with its English file.

## Changelog

`CHANGELOG.md` follows [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/), in English. The readme links to it and does not list fork differences.

- Add every user-visible change to `## [Unreleased]` under `Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, or `Security`. User-visible means CLI options, defaults, guest behavior, provisioning scripts, user-set environment variables, or supported images.
- Prefix breaking changes with `**Breaking:**` and name the replacement.
- Skip refactors, tests, module layout, tooling, and internal or test-only environment variables.
- On release: bump `version` in `Cargo.toml`, rename `[Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD`, and add an empty `## [Unreleased]` above it.
