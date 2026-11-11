# Alpine maintenance image

`src/alpine.rs` pins, downloads, and checks one official Alpine cloud disk. That disk is a helper. Vibe never publishes it, never boots it as a user VM, and never edits the cached copy.

The helper exists so `vibe provision` can make a current Debian nocloud image log in the way older images did: an empty `/etc/machine-id` and an empty root password field. The interactive path (`vibe`, `vibe path/to/disk.raw`, `.vibe/instance.raw`, `vibe --image`) does not download this image and does not open it.

`seed.iso` (how it is built, the `cidata` volume name, and the three-disk attachment) is already specified in [How a VM boots](how-vm-boot.md) under "Maintenance VM", and in `src/seed.rs`. This page only states the contract the Alpine guest relies on.

## Why this image

Debian nocloud images from late 2026, including the pinned `debian-13-nocloud-arm64-20261001-2618`, ship a locked root password field and stop in an interactive firstboot wizard. The serial console stays silent, so the legacy login (wait for `login:`, send `root`, expect no password) never reaches a shell.

Vibe does not drive that wizard, does not `chroot` the target, and does not run any program from the target disk. It needs a second Linux that:

- boots from its own EFI firmware with no password prompt
- already contains `e2fsck`, `sgdisk`, `mount`, and `findfs` (busybox), so the run needs no network and no `apk add`
- starts a script with no getty, no PAM, and no VirtioFS share

The official aarch64 **tiny** cloud image does that. It is Alpine plus tiny-cloud and the `linux-virt` kernel. The same file is the NoCloud image for every cloud: tiny-cloud looks for a volume labeled `cidata` and, in its final stage, runs `user-data` when the file starts with `#!`. That file is the embedded `src/guest/maint.sh`.

The other published variants are the wrong tool. `cloudinit` pulls in Python cloud-init and is larger and slower. `tiny-metal` and `cloudinit-metal` use `linux-lts` and hardware firmware. `.qcow2` and `.vhd` are not what Virtualization.framework attaches. Vibe does not build or ship its own kernel, initramfs, or maintenance image.

The pin (surveyed 2026-10-14, still the constants in `src/alpine.rs`):

| Constant | Value |
| --- | --- |
| `ALPINE_IMAGE_NAME` | `alpine-3.24.2-aarch64-tiny-r1` |
| `ALPINE_ARCHIVE_URL` | `https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/cloud/alpine-3.24.2-aarch64-tiny-r1.raw.tar.gz` |
| `ALPINE_ARCHIVE_SHA512` | `c66badf3648147e0e06f710d600698ec35804af70b00667742b4dce9f28df5cb516190ea90d63792c3cf970a6541aa675ffe1ddddaed13df1abde1fe6f43698b` |
| Archive size | 66,222,607 bytes. One member, `disk.raw`, 1,073,741,824 bytes (1 GiB, sparse). |
| Firmware | limine (`EFI/BOOT/BOOTAA64.EFI`). From 3.24 the filename no longer contains `uefi`. |
| Kernel | `6.18.54-0-virt`. `limine.conf` has `timeout: 10` and `root=LABEL=/`. Serial input does not skip the menu. Kernel log lines do not appear on hvc0. |
| tiny-cloud | 3.4.1-r2. `user-data` runs in the final stage, which does not wait for a network. |

`expand_root` grows Alpine's own root on every boot. `run_maintenance` therefore clones the cached raw (`cache::clone_file`: APFS clone, copy as fallback) into `maint/<pid>-<nonce>/alpine.raw` and boots the clone. `maint/` is created next to the Alpine raw in use, so the clone stays on the same file system; when that directory is not writable, `maint/` goes to the preferred image directory instead. The cached raw stays a read-only input. The maintenance checks require its SHA-256 to be unchanged after a run.

## When it starts

`ensure_bootable` (`src/disk_prepare.rs`) is the only provision-path caller. `provision_image` calls it on the private work copy `<name>.tmp.<pid>`, at the copy's original size, before `set_len` grows the file to 12 GiB and before `run_vm`. Growing first would make the maintenance manifest's `disk_bytes` disagree with the partition table.

`probe_disk` classifies the copy on the host. `prepare_decision` then:

| Class | What happens |
| --- | --- |
| `LegacyCompatible` | Print `Disk is already legacy compatible; skipping Alpine preparation.` No download. This is an older nocloud image, or any image Vibe already published: Debian, machine-id missing, empty, or a valid 32-hex id, one `root:` shadow line, empty password field. |
| `Unsupported` | Print `[probe] Warning: ...` and continue on the legacy login path. No Alpine VM. Typical causes: no GPT root, or an ext4 root whose `os-release` is not Debian. |
| `NeedsPreparation` | machine-id is `uninitialized` or some other content, or the root password field is not empty. Download Alpine if needed, then `process_disk`. The notice says this takes about 20 seconds. |
| `Unknown` | The bounded reader could not decide (`needs recovery`, a machine-id symlink, an unexpected root line, and similar). Alpine checks the image itself. |

`EMPTY_ROOT_NOTICE` is printed before a preparation: the published image, and every VM created from it, will have an empty root password.

The original base file and a `--base` path are only read. `provision --repl` on an image that already exists calls `run_vm` and does not probe. A failed maintenance run does not publish `<name>.raw`.

### Cache

`ensure_alpine_image` takes the `ImageDirs` (see `VIBE_EXTRA_IMAGE_DIR` and `VIBE_PREFERRED_IMAGE_DIR` in `vibe --help`):

- When the preferred directory holds `{name}.raw`, the steps below run there.
- Otherwise the other existing image directories are checked in lookup order. A `{name}.raw` there is used only if `alpine_root_uuid_matches` accepts it. It is read without a lock and is never deleted, repaired, or re-downloaded; a mismatch or read error prints a warning naming the file and moves on.
- When no directory qualifies, the steps below run in the preferred directory, which downloads the image there.

Each run prints `Using Alpine maintenance image <path>`, and the maintenance log header records the same path.

In the preferred directory `ensure_alpine_image` holds `alpine.lock`, then `ensure_locked`:

1. A cached archive whose SHA-512 does not match `ALPINE_ARCHIVE_SHA512` is deleted and fetched again (`images::fetch_verified`: download to `.part`, check the digest, rename).
2. A missing `{name}.raw` is extracted with `extract_disk_raw` (the archive member is `disk.raw`).
3. `alpine_root_uuid_matches` requires partition `ALPINE_ROOT_PART` to be ext4 with UUID `ALPINE_ROOT_FS_UUID`. A mismatch deletes the raw and returns `[download] verify: ...`.

A valid raw plus a valid archive does not touch the network. A good raw is kept even when a corrupt archive is replaced. Every `AlpineError` displays with the `[download]` prefix and, for a fetch failure, names the URL, the expected SHA-512, and the path where the archive can be placed by hand.

## What the guest does

`process_disk` (`src/maintenance.rs`) refuses the work copy first when any filesystem label is `/` or `cidata` (any case), or when any ext4 UUID equals `ALPINE_ROOT_FS_UUID`. Alpine's limine line is `root=LABEL=/`, and tiny-cloud finds the seed with `findfs LABEL=cidata`. A target that reused either name could be mounted as the helper's own root or as the seed. The guest `self` step also stops before any write if the UUID check fails. The host check keeps that disk from being booted.

The headless VM has 2 CPUs and 1 GiB of RAM, no network device, no shared folder, and no keyboard serial port. Disks, in guest order:

| Guest device | File | Mode |
| --- | --- | --- |
| `vda` | clone of the Alpine raw | writable |
| `vdb` | Debian work copy | writable |
| `vdc` | `seed.iso` | read-only |

`hvc0` is the console log. `hvc1` carries control frames `VIBE1 <nonce> ...`. EFI variables live in the maintenance directory and are thrown away with it. `HeadlessDeadlines::maintenance` waits 90 seconds for `PREP-READY` (a measured boot is about 17 seconds, of which limine's menu is about 10) and 300 seconds for `PREP-RESULT`.

tiny-cloud's final stage runs `maint.sh`. The script redirects its own output to `hvc0`, turns off `ONLCR` on both consoles, and never prints shadow bytes or enables xtrace. `mount_seed` requires `findfs LABEL=cidata` to be `/dev/vdc` and mounts that ISO at `/run/vibe-seed` (`ro,nodev,nosuid,noexec`). A seed that cannot be mounted emits no frame. The manifest on that ISO supplies the nonce, `alpine_root_uuid` (the constant below), the work-copy size, and the target's root and ESP identity. `policy` is `minimal`.

Steps run in this order. `observe_frame` rejects any other order. A failed step emits `PREP-RESULT fail <code> <step>` and powers off. The exit trap does the same if the script dies after `PREP-READY`.

| Step | Code | Work |
| --- | --- | --- |
| `seed` | 2 | Manifest format is 1, policy is `minimal`, fields are single tokens. |
| `self` | 3 | The mounted root is `/dev/vda2`, and `findfs UUID=<alpine_root_uuid>` is the same device. This is the "we booted the helper, not the Debian disk" check. |
| `target` | 4 | `/sys/class/block/vdb/size` matches `disk_bytes`. `sgdisk -i` on the manifest's root partition matches the type GUID and PARTUUID. Busybox `blkid` does not print PARTUUID. |
| `fsck` | 5 | `e2fsck -p` on the target root returns 0 or 1. |
| `inspect` | 6 | Mount the root read-only. `ID=debian` in `os-release`. `/etc` and `/etc/machine-id` and `/etc/shadow` are real files, not symlinks. Exactly one well-formed `root` line in `shadow`. dbus machine-id is absent, a regular file, or a symlink to `/etc/machine-id`. No `PermitEmptyPasswords yes` in `sshd_config` or `sshd_config.d/*.conf`. `/etc/passwd` has one `root` line whose password field is `x`. Then remount read-write. |
| `apply` | 7 | Truncate `/etc/machine-id` and a regular dbus machine-id. `apply_shadow` rewrites only the root password field to empty and keeps mode and owner. |
| `verify` | 8 | machine-id is an empty regular file. Shadow bytes match the cleared rendering. Shadow mode and owner are unchanged. |
| `unmount` | 9 | `sync`, unmount, confirm nothing on `/dev/vdb` stays mounted, `e2fsck -n`. |

`PREP-RESULT ok 0 done` follows `unmount`, then `poweroff`. On the host, `verify_processed` re-reads the work copy: same GPT layout, empty `/etc/machine-id`, empty root password field, dbus machine-id missing, empty, or a symlink to `/etc/machine-id`, and `classify` == `LegacyCompatible`. A mismatch is `[verify] host-verify`. Package installation is a later step, inside the Debian guest, via `provisioning/base.sh`. The helper does not install packages and does not edit GRUB, the ESP, PAM, sshd, or the timezone.

On an M2 Mac the whole helper run is about 19 seconds. The user-facing notice says about 20 seconds, which also covers the first download on a fast link.

## `ALPINE_ROOT_PART` and `ALPINE_ROOT_FS_UUID`

```rust
pub(crate) const ALPINE_ROOT_FS_UUID: &str = "d51a6213-c765-4b85-9105-1b438c523993";
pub(crate) const ALPINE_ROOT_PART: u32 = 2;
```

Both values were read from the extracted `disk.raw` of `alpine-3.24.2-aarch64-tiny-r1`, not chosen. `gpt::tests::real_disk_layouts` (ignored unless `VIBE_TEST_ALPINE_RAW` is set) asserts them on a real file: root index 2, ext4 label `/`, UUID equal to the constant.

### Partition number

`ALPINE_ROOT_PART` is the 1-based slot in the GPT partition entry array. Empty slots still consume a number, which is why a Debian ESP in slot 15 is index 15 (`Partition::index` in `src/gpt.rs`).

On this raw the primary GPT is:

| Index | Role | Type | Filesystem | Range |
| --- | --- | --- | --- | --- |
| 1 | ESP | EFI System Partition | vfat, label `EFI` | LBA 2048–380927 |
| 2 | root | Linux filesystem `0fc63daf-8483-4772-8e79-3d69d8477de4` | ext4, label `/` | LBA 380928–917470 |

The two partitions occupy 448 MiB. `disk.raw` is padded with zeros to 1 GiB. `select_layout` reports partition 2 as the single Linux root because it is the only `LINUX_FILESYSTEM_GUID` entry (this image has no ARM64-root type GUID).

Virtio numbers the device node from that same index. The clone is the first disk, so partition 2 appears as `/dev/vda2`. `step_self` in `src/guest/maint.sh` compares against that literal path. The guest script has its own copy of the number. The host constant is the unpack check in `alpine_root_uuid_matches`. A new slot index has to be written in both places.

### Filesystem UUID

`ALPINE_ROOT_FS_UUID` is the ext4 filesystem UUID (`s_uuid`) of partition 2, in the order `blkid` prints as `UUID`. Copy that field. `sgdisk -i` prints the partition unique GUID, which the manifest already carries separately as the target's `root_partuuid`. GPT type and unique GUIDs are mixed-endian; ext4 stores `s_uuid` in display order (`format_ext4_uuid` in `src/gpt.rs`).

`read_ext4_label` reads it like this:

1. Partition byte offset is `first_lba * 512`. For this image, `380928 * 512`.
2. The superblock starts 1024 bytes later.
3. Magic `0xEF53` is the little-endian `u16` at superblock offset `0x38`.
4. `s_uuid` is the 16 bytes at superblock offset `0x68`.
5. The volume label is the NUL-terminated string at superblock offset `0x78` (16 bytes). On this image it is `/`.

Those 16 bytes are `d5 1a 62 13 c7 65 4b 85 91 05 1b 43 8c 52 39 93`, which `format_ext4_uuid` renders as `d51a6213-c765-4b85-9105-1b438c523993`. `synthetic_raw` in `src/alpine.rs` writes the same bytes so the cache tests can run without the real image.

The UUID is used in three places:

- After extract, `alpine_root_uuid_matches` rejects a raw whose partition 2 is not this ext4. That catches a truncated extract or a swapped `disk.raw` even when the archive digest passed.
- `process_disk` copies it into the seed manifest as `alpine_root_uuid`. `step_self` requires `findfs UUID=<that value>` to be `/dev/vda2` before it touches `vdb`.
- `label_block_reason` refuses a work copy that already carries this ext4 UUID, so the guest cannot mistake the Debian root for its own.

Alpine itself finds the root by the label `/` (`root=LABEL=/` in `limine.conf`). The UUID is Vibe's check that the booted root is this pinned image.

### Reading the two fields off a new raw

On a Mac, the repository parser is the authoritative reader. `gpt::tests::real_disk_layouts` opens all three paths, then checks the Alpine raw for index 2, label `/`, and this UUID:

```bash
VIBE_TEST_DEBIAN_NEW_RAW=/path/to/debian-new.raw \
VIBE_TEST_DEBIAN_OLD_RAW=/path/to/debian-old.raw \
VIBE_TEST_ALPINE_RAW=/path/to/alpine.raw \
  cargo test --lib gpt::tests::real_disk_layouts -- --ignored --nocapture
```

A new Alpine image fails that assertion until the test and the constants move together. To print the UUID before editing the test, read the same superblock `scan_labels` reads. Partition 2 of the current image:

```bash
# superblock = first_lba * 512 + 1024; s_uuid starts 0x68 bytes into it
dd if=disk.raw bs=1 skip=$((380928 * 512 + 1024 + 0x68)) count=16 2>/dev/null | xxd
```

Recompute `first_lba` from the GPT when the layout moves. `sgdisk -p disk.raw` lists the entry numbers; `sgdisk -i 2` prints the partition unique GUID, which must not be copied into `ALPINE_ROOT_FS_UUID`. On Linux, `blkid -o value -s UUID` on the second partition and `blkid -o value -s LABEL` are the same two strings (`/` and the UUID).

SHA-512 of the `.raw.tar.gz` is checked against the official `.sha512` next to the file and against `outputs.raw.tar.gz.sha512` in the release's `.yaml`. The code stores that digest and does not call GPG.

## Replacing the pin

Re-measure the disk in the same change as the URL. Alpine 3.24 replaced GRUB with limine and dropped `uefi` from the filename. A later release can move the root partition, mint a new filesystem UUID, change the label, change tiny-cloud's NoCloud probe, or change the limine timeout.

1. From <https://alpinelinux.org/cloud/>, take the current aarch64 **tiny** `.raw.tar.gz`. Leave `.qcow2`, `.vhd`, `cloudinit`, and `metal` alone.
2. Check SHA-512 against the published `.sha512` and the image yaml. Record the archive size.
3. Extract `disk.raw` (`tar -xOf archive.raw.tar.gz disk.raw`) and re-read the partition index, the ext4 label, and `s_uuid` as above. The label Vibe depends on is `/`.
4. Confirm the image still contains `e2fsprogs`, `sgdisk`, and busybox `findfs` / `mount`, that tiny-cloud still runs a `#!` `user-data` in the final stage with no network, that getty is not bound to `hvc0` or `hvc1`, and that `expand_root` still rewrites the booted disk (the cache file must stay a clone source).
5. Update `ALPINE_IMAGE_NAME`, `ALPINE_ARCHIVE_URL`, `ALPINE_ARCHIVE_SHA512`, `ALPINE_ROOT_FS_UUID`, and `ALPINE_ROOT_PART` in `src/alpine.rs`.
6. If the root slot is no longer 2, change the `/dev/vda2` literals in `step_self`. The device node is `vda` plus the GPT index only while the Alpine clone remains the first disk in `run_maintenance`.
7. Keep the fixtures on the new UUID and the new index:
   - `synthetic_raw` in `src/alpine.rs` (the 16 bytes at superblock `0x68`, and `TestPart.index`)
   - the sample `alpine_root_uuid` in `src/seed.rs` tests
   - `gpt::tests::real_disk_layouts` (index, label `/`, UUID)
   - the cache stem `alpine-3.24.2-aarch64-tiny-r1` in `tests/disk_prepare.rs` when the file name changes
   - the image name in `docs/how-vm-boot.md` and `docs/how-vm-boot.zh-Hans.md`
8. Remove the previous `<name>.raw` and `<name>.raw.tar.gz` from the cache (`$XDG_CACHE_HOME/vibe`, or `~/.cache/vibe`) and from any extra image directory (`VIBE_EXTRA_IMAGE_DIR`). A raw that still matches the old UUID stays in use and is never re-downloaded. `ensure_alpine_image` keys files on `ALPINE_IMAGE_NAME`, so a renamed pin is a new pair of files; delete the old pair in the same change.
9. `cargo test`. On a Mac, run `VIBE_VM_CHECK=maintenance-all` (`tests/alpine_maintenance.rs`) and `VIBE_VM_CHECK=prepare` (`tests/disk_prepare.rs`) with `VIBE_TEST_ALPINE_RAW` pointing at the new raw and the archive beside it as `<raw>.tar.gz`. Boot the helper once against a new Debian work copy and confirm `PREP-RESULT ok 0 done`, an empty machine-id, an empty root password field, and an unchanged SHA-256 on the cached raw.

If limine's `timeout` grows, compare a measured boot with `MAINT_FIRST_OUTPUT_TIMEOUT` (90 seconds). The current menu costs about 10 seconds and fits that budget. Direct kernel boot (`VZLinuxBootLoader`, about 8.5 seconds in the original survey) is not implemented; the maintenance VM still uses EFI so the host does not have to unpack the FAT ESP or the zboot kernel.
