# Development and tests

Build a checkout, run the tests, and replace the pinned disk images. Day-to-day use of the binary is in [readme.md](../readme.md). CLI flags are in [USAGE.md](../USAGE.md).

## Clone and initialize

Clone this fork with the network submodule. `cargo build` on an Apple Silicon Mac compiles that submodule.

```bash
git clone --recurse-submodules git@github.com:hangxingliu/vibe.git
cd vibe

# (Optional) If you have already installed the latest Rust, the latest Go, shfmt and shellcheck
mise install --locked
```

If the clone has no submodule yet, initialize it:

```bash
git submodule update --init helpers/gvisor-tap-vsock
```

## Build

```bash
cargo build --release --locked
```

The binary is `target/release/vibe`. `./scripts/vibe_dev` runs `cargo run --release --`. A debug build is `cargo build --locked`.

`build.rs` builds `helpers/vibe-usernet` only for host `apple-darwin` and target `aarch64-apple-darwin`. That build sets `CGO_ENABLED=1`, `GOOS=darwin`, and `GOARCH=arm64`, then embeds the binary. Any other host embeds an empty stub. `cargo check` still works there. The release build needs `go` on `PATH`. Crate and module downloads use the shell proxy variables.

## Test and lint

Run the repo check from the repo root. mise must be active.

```bash
./scripts/test.sh          # rust, then go, then shell
./scripts/test.sh rust
./scripts/test.sh go
./scripts/test.sh shell
```

The Rust check is:

```bash
cargo fmt --check
cargo clippy --all-targets -- --deny warnings
cargo test
```

`cargo test` runs the in-process tests and compiles the integration tests. Ignored tests stay off until `--ignored`.

Format the same three languages:

```bash
./scripts/format.sh
cargo fmt
```

The Go section of `./scripts/test.sh` runs `gofmt -l` and `go vet ./...` in `helpers/vibe-usernet`. Run `go test` from the Go section below. The shell section runs `shfmt --indent 2 --diff` and `shellcheck` on `scripts/` only. It allows ShellCheck `SC2164`.

## Rust tests that `cargo test` skips

Virtualization.framework runs on the process main thread. A `cargo test` worker is a background thread. Each ignored VM test starts the `vibe` binary with `VIBE_VM_CHECK`. `main` runs that check before CLI parsing. The check table is in [How a VM boots](how-vm-boot.md#tests-that-boot-or-classify-disks).

`cargo test -- --ignored` runs only ignored tests. `cargo test -- --include-ignored` runs both sets. (However, please do not run ignored tests directly like this; read below for details)

### Real disk images

Three unit tests are ignored. Each one returns without failure when a required variable is unset. Set the variables, then pass `--ignored`. A skip line means the test did not read the disk.

| Variable | Disk |
| --- | --- |
| `VIBE_TEST_DEBIAN_NEW_RAW` | Current nocloud raw. Class `NeedsPreparation`. Debian 13.7. `machine-id` is `uninitialized`. The root password field is not empty. |
| `VIBE_TEST_DEBIAN_OLD_RAW` | Older nocloud raw. Class `LegacyCompatible`. Debian 13.3. Empty `machine-id`. Empty root password field. |
| `VIBE_TEST_ALPINE_RAW` | Pinned Alpine tiny raw. |
| `VIBE_TEST_DEBIAN_LOCKED_ROOT_RAW` | Optional. Empty `machine-id` and a locked root. Still `NeedsPreparation`. |

The default image directory is `$XDG_CACHE_HOME/vibe`, or `~/.cache/vibe`. The current pins are:

- `debian-13-nocloud-arm64-20261001-2618.raw` for `VIBE_TEST_DEBIAN_NEW_RAW`
- `debian-13-nocloud-arm64-20260112-2355.raw` for `VIBE_TEST_DEBIAN_OLD_RAW`
- `alpine-3.24.2-aarch64-tiny-r1.raw` for `VIBE_TEST_ALPINE_RAW`

Tests need the `.raw` file. The archive member is `disk.raw`. Extract a local archive when the raw is missing. Do not download the archive again when `shasum -a 512` already matches.

```bash
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/vibe"
tar -xOf "$CACHE/debian-13-nocloud-arm64-20260112-2355.tar.xz" disk.raw > "$CACHE/debian-13-nocloud-arm64-20260112-2355.raw"

export VIBE_TEST_DEBIAN_NEW_RAW="$CACHE/debian-13-nocloud-arm64-20261001-2618.raw"
export VIBE_TEST_DEBIAN_OLD_RAW="$CACHE/debian-13-nocloud-arm64-20260112-2355.raw"
export VIBE_TEST_ALPINE_RAW="$CACHE/alpine-3.24.2-aarch64-tiny-r1.raw"

cargo test --lib real_disk_layouts -- --ignored --nocapture
cargo test --lib real_debian_root_matches_recorded_files -- --ignored --nocapture
cargo test --lib real_images_match_recorded_classes -- --ignored --nocapture
```

Those tests live in `src/gpt.rs`, `src/ext4ro/tests.rs`, and `src/image_probe.rs`.

`tests/fixtures/make-ext4.sh` rebuilds the small gzip ext4 images. The in-process tests run that script when a fixture is missing. The script looks for `mkfs.ext4` in `MKFS_EXT4`, `/tmp/e2fs-install`, Homebrew `e2fsprogs`, then `PATH`.

### Ignored VM tests

These tests panic when a required variable is unset. Run them on a Mac.

| Cargo test | Needs |
| --- | --- |
| `--test vm_devices` `second_disk_is_read_only_in_the_guest` | `VIBE_TEST_DEBIAN_OLD_RAW` |
| `--test vm_devices` `headless_sleep_times_out_within_the_budget` | `VIBE_TEST_ALPINE_RAW` |
| `--test disk_prepare` `old_base_skips_alpine_and_new_base_is_processed` | All three `VIBE_TEST_*_RAW` paths. |
| `--test alpine_maintenance` `alpine_maintenance_prepares_and_rejects_disks` | All three raws. `e2fsprogs` from `VIBE_E2FS_SBIN` or `PATH`. |
| `--test vm_user` `vm_user_session_and_shares` and `root_password_login` | `VIBE_VM_CHECK_DISK`. Optional `VIBE_VM_CHECK_ARGS`. |

`disk_prepare` copies `<alpine-raw>.tar.gz` when that sibling exists. Point `VIBE_TEST_ALPINE_RAW` at `alpine-3.24.2-aarch64-tiny-r1.raw` so the sibling is the real archive. The old-image half leaves an empty cache.

`alpine_maintenance` always sets `VIBE_VM_CHECK=maintenance-all`. The `download` step fetches the pinned Alpine archive into a temporary cache. It does that even when `~/.cache/vibe` already holds the archive. Run one check on the binary when that fetch is not wanted:

```bash
VIBE_VM_CHECK=maintenance-prepare-new \
VIBE_TEST_ALPINE_RAW="$CACHE/alpine-3.24.2-aarch64-tiny-r1.raw" \
VIBE_TEST_DEBIAN_NEW_RAW="$CACHE/debian-13-nocloud-arm64-20261001-2618.raw" \
  ./target/release/vibe
```

`maintenance-<name>` accepts `download`, `shadow`, `prepare-new`, `prepare-old`, `prepare-again`, `negative`, `needs-recovery`, `ssh`, `logs`, and `all`. `VIBE_MAINT_TEST_PARTUUID` forces a target mismatch. `VIBE_KEEP_FAILED_WORK=1` keeps a failed work copy.

`VIBE_VM_CHECK_DISK` is a prepared image, or `default.raw`, whose root has no password. The check clones it. `VIBE_VM_CHECK_ARGS` is extra CLI text on every inner `vibe` call. Provisioning installs `bindfs`, so the guest needs a proxy:

```bash
export VIBE_VM_CHECK_DISK="$CACHE/default.raw"
export VIBE_VM_CHECK_ARGS="--proxy socks5://<COMPANY_PROXY_SERVER_IP>:3003 --proxy-udp --dns 1.1.1.1 --dns 8.8.8.8"
cargo test --test vm_user -- --ignored
```

Send guest traffic through `socks5://<COMPANY_PROXY_SERVER_IP>:3003`. Pass `--proxy` and `--proxy-udp`. Also pass `--dns 1.1.1.1`. A link-local host resolver is on the bypass list, so the guest cannot use it. Host `cargo`, `go`, and image `curl` read `https_proxy`, `HTTPS_PROXY`, `http_proxy`, and `HTTP_PROXY`. Rules are in [Networking and Proxy](network-proxy.md).

```bash
cargo test --test vm_devices -- --ignored
cargo test --test disk_prepare -- --ignored
```

## Build and test the Go helpers

NAT uses two Go modules. Both declare `go 1.26.0`. Initialize the submodule before either build. A submodule rebase is a separate procedure: [how to rebase gvisor-tap-vsock](ai-tasks/how-to-rebase-onto-gvisor-tap-vsock-latest-code.md).

### helpers/gvisor-tap-vsock

This directory is the forked [gvisor-tap-vsock](https://github.com/containers/gvisor-tap-vsock) submodule. It is the user-mode network stack. Dependencies are vendored. From that directory:

```bash
cd helpers/gvisor-tap-vsock

# Unit tests for the packages this fork changes.
go test -count=1 -timeout 180s -skip TestSuite \
  ./pkg/services/noproxy/ \
  ./pkg/services/netproxy/ \
  ./pkg/services/dns/ \
  ./pkg/services/forwarder/

# Upstream gvproxy CLI. vibe embeds helpers/vibe-usernet.
go build -o bin/gvproxy ./cmd/gvproxy
```

`-skip TestSuite` leaves out the port-forward specs in `pkg/services/forwarder`. Those specs listen on a host address and dial it. Run them on purpose:

```bash
go test -count=1 -timeout 180s -run TestSuite ./pkg/services/forwarder/
```

`go test ./...` also compiles `test-qemu/` and `test-vfkit/`. Those suites need a hypervisor and a guest image. `make test` builds `gvproxy` and runs every package. `./scripts/test-gvisor.sh` tests the four packages above, including `TestSuite`, and builds `bin/gvproxy`.

The DNS suite looks up `redhat.com` through the host resolver. Run it with outbound DNS.

### helpers/vibe-usernet

This module is the helper that `vibe` embeds. It has no `vendor/` directory. `go test` and `go build` use the module cache. The `replace` line takes `gvisor-tap-vsock` from the submodule.

```bash
cd helpers/vibe-usernet
go test -count=1 ./...
go build -o bin/vibe-usernet .
```

The binary above is the standalone helper. On an Apple Silicon Mac, `cargo build` compiles this package again and embeds that result.

## Update the Debian and Alpine images

The pin is the constant in source. A newer cache file leaves that constant unchanged. Edit the constants, then verify. Download behavior is in [How a VM boots](how-vm-boot.md#debian-base) and [Alpine maintenance image](alpine-os-purpose.md).

The image directory is `$XDG_CACHE_HOME/vibe`, or `~/.cache/vibe`. `VIBE_EXTRA_IMAGE_DIR` and `VIBE_PREFERRED_IMAGE_DIR` add directories. `vibe --help` prints the lookup order.

Host downloads use the shell proxy variables. Do not use a third-party mirror. Guest network checks use `--proxy socks5://<COMPANY_PROXY_SERVER_IP>:3003 --proxy-udp`.

If `shasum -a 512` matches the pin, keep the cached archive. Download again only when the digest does not match.

### Debian

The pin is `DEBIAN_COMPRESSED_DISK_URL`, `DEBIAN_COMPRESSED_SHA`, and `DEBIAN_COMPRESSED_SIZE_BYTES` in `src/images.rs`. The current file is `debian-13-nocloud-arm64-20261001-2618.tar.xz` from <https://cdimage.debian.org/cdimage/cloud/trixie/>.

1. Open <https://cdimage.debian.org/cdimage/cloud/trixie/>. Take the newest `debian-13-nocloud-arm64-*.tar.xz`. Leave genericcloud and qcow2 alone.
2. Read `SHA512SUMS` in that directory. Copy that digest into `DEBIAN_COMPRESSED_SHA`.
3. Set `DEBIAN_COMPRESSED_SIZE_BYTES` below the archive size. A file at least that large with a bad digest is left in place.
4. `ensure_base_image` returns when the `.raw` already exists. Delete that `.raw` before a fresh extract. Delete a short `.tar.xz` before a fresh download.
5. Keep `debian-13-nocloud-arm64-20260112-2355.raw` as `VIBE_TEST_DEBIAN_OLD_RAW`. Point `VIBE_TEST_DEBIAN_NEW_RAW` at the new raw.
6. If `/etc/debian_version`, `machine-id`, or the root password field changed, update `real_debian_root_matches_recorded_files` and `real_images_match_recorded_classes`. Update the stamp in [how-vm-boot.md](how-vm-boot.md) and [how-vm-boot.zh-Hans.md](how-vm-boot.zh-Hans.md).

Extract with the member name `disk.raw`:

```bash
tar -xOf "$CACHE/debian-13-nocloud-arm64-STAMP.tar.xz" disk.raw \
  > "$CACHE/debian-13-nocloud-arm64-STAMP.raw"
shasum -a 512 "$CACHE/debian-13-nocloud-arm64-STAMP.tar.xz"
```

### Alpine

Follow [Replacing the pin](alpine-os-purpose.md#replacing-the-pin). Short form:

1. From <https://alpinelinux.org/cloud/>, take the current aarch64 tiny `.raw.tar.gz`. Leave `.qcow2`, `.vhd`, `cloudinit`, and `metal` alone.
2. Check SHA-512 against the published `.sha512` file and the image yaml.
3. Update `ALPINE_IMAGE_NAME`, `ALPINE_ARCHIVE_URL`, `ALPINE_ARCHIVE_SHA512`, `ALPINE_ROOT_FS_UUID`, and `ALPINE_ROOT_PART` in `src/alpine.rs`.
4. Update the other copies named in that section. A raw that still has the old UUID stays in use.
5. Delete the old `<name>.raw` and `<name>.raw.tar.gz` from every image directory.

A valid raw plus a valid archive stays offline.

### Verify

1. `shasum -a 512` on each archive matches the constant in the source.
2. Run the three real-disk unit tests. They must assert. A skip line means the test ignored the disk.
3. Run `cargo test --test disk_prepare -- --ignored`. Old Debian prints `kind=AlreadyCompatible` and leaves the cache empty. New Debian prints `kind=Processed`.
4. Run `VIBE_VM_CHECK=maintenance-prepare-new` as shown above. The maintenance log shows a successful `PREP-RESULT`. The work copy has an empty `machine-id` and an empty root password field. The cached Alpine raw keeps its SHA-256. The full contract is in [alpine-os-purpose.md](alpine-os-purpose.md).
5. Skip `maintenance-all` when step 4 is enough. `maintenance-all` includes the Alpine download into a temporary cache.
6. Provision a new name. Do not replace `default.raw` until this boot works.

```bash
./target/release/vibe provision --image dev-check \
  --proxy socks5://<COMPANY_PROXY_SERVER_IP>:3003 --proxy-udp \
  --dns 1.1.1.1 --dns 8.8.8.8 \
  --repl
```

In the guest, read `/etc/debian_version` and `/etc/os-release`. `/etc/machine-id` is empty. `exit` returns to the host. Delete `dev-check.raw` from the image directory after that check.

A direct boot of the new base raw stops in the firstboot wizard. `vibe provision` runs the Alpine helper first. See [How a VM boots](how-vm-boot.md).
