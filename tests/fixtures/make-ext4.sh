#!/bin/sh
# Rebuild the small ext4 fixtures used by ext4ro and image_probe tests.
#
# The generated images are committed. Rerun this script only when a fixture
# needs to change. It uses the same mkfs.ext4 / debugfs / e2fsck commands as
# the Alpine maintenance image.
# Tool lookup order: $MKFS_EXT4 (mkfs.ext4 only), /tmp/e2fs-install, Homebrew, PATH.
#
# Usage: tests/fixtures/make-ext4.sh
# Outputs gzip-compressed images next to this script.

set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")" && pwd)

find_tool() {
    name=$1
    if [ -n "${MKFS_EXT4:-}" ] && [ "$name" = "mkfs.ext4" ] && [ -x "$MKFS_EXT4" ]; then
        printf '%s\n' "$MKFS_EXT4"
        return 0
    fi
    if [ -x "/tmp/e2fs-install/sbin/$name" ]; then
        printf '%s\n' "/tmp/e2fs-install/sbin/$name"
        return 0
    fi
    if command -v brew >/dev/null 2>&1; then
        prefix=$(brew --prefix e2fsprogs 2>/dev/null || true)
        if [ -n "$prefix" ] && [ -x "$prefix/sbin/$name" ]; then
            printf '%s\n' "$prefix/sbin/$name"
            return 0
        fi
    fi
    if command -v "$name" >/dev/null 2>&1; then
        command -v "$name"
        return 0
    fi
    echo "cannot find $name" >&2
    exit 1
}

MKFS=$(find_tool mkfs.ext4)
DEBUGFS=$(find_tool debugfs)
E2FSCK=$(find_tool e2fsck)

printf "mkfs: %s\n" "$MKFS";
printf "debugfs: %s\n" "$DEBUGFS";
printf "e2fsck: %s\n" "$E2FSCK";

WORKDIR=$(mktemp -d "${TMPDIR:-/tmp}/vibe-ext4.XXXXXX")
cleanup() { rm -rf "$WORKDIR"; }
trap cleanup EXIT

EMPTY="$WORKDIR/empty"
: >"$EMPTY"

cat >"$WORKDIR/os-release" <<'EOF'
NAME="Debian GNU/Linux"
ID=debian
VERSION_ID="13"
EOF

printf '13.7\n' >"$WORKDIR/debian-new"
printf '13.3\n' >"$WORKDIR/debian-old"
: >"$WORKDIR/machine-id-empty"
printf '0123456789abcdef0123456789abcdef\n' >"$WORKDIR/machine-id-valid"
printf 'root::19999:0:99999:7:::\ndaemon:*:19999:0:99999:7:::\n' >"$WORKDIR/shadow-empty"
printf 'root:!unprovisioned:19999:0:99999:7:::\ndaemon:*:19999:0:99999:7:::\n' >"$WORKDIR/shadow-locked"
printf 'root::19999:0:99999:7:::\nroot::19999:0:99999:7:::\n' >"$WORKDIR/shadow-two"
printf 'daemon:*:19999:0:99999:7:::\n' >"$WORKDIR/shadow-none"
printf 'root:x:0:0:root:/root:/bin/bash\n' >"$WORKDIR/passwd"
LONG=$(printf 'L%.0s' $(seq 1 70))

run_debugfs() {
    image=$1
    cmds=$2
    "$DEBUGFS" -w -f "$cmds" "$image"
}

base_tree() {
    image=$1
    debian=$2
    machine=$3
    shadow=$4
    dbus_target=$5
    {
        echo "mkdir etc"
        echo "mkdir usr"
        echo "mkdir usr/lib"
        echo "mkdir var"
        echo "mkdir var/lib"
        echo "mkdir var/lib/dbus"
        echo "write $WORKDIR/os-release usr/lib/os-release"
        echo "symlink etc/os-release ../usr/lib/os-release"
        echo "write $debian etc/debian_version"
        echo "write $machine etc/machine-id"
        echo "write $shadow etc/shadow"
        echo "write $WORKDIR/passwd etc/passwd"
        echo "symlink etc/short fast-target"
        echo "symlink etc/long $LONG"
        echo "symlink var/lib/dbus/machine-id $dbus_target"
        echo "mkdir many"
    } >"$WORKDIR/base.cmds"
    # debugfs symlink is "symlink NAME TARGET".
    run_debugfs "$image" "$WORKDIR/base.cmds"
}

fill_many() {
    image=$1
    {
        i=0
        while [ "$i" -lt 600 ]; do
            printf 'write %s many/f%03d\n' "$EMPTY" "$i"
            i=$((i + 1))
        done
    } >"$WORKDIR/many.cmds"
    run_debugfs "$image" "$WORKDIR/many.cmds"
    # e2fsck exits 1 when it fixes something. Directory optimization does that.
    set +e
    "$E2FSCK" -f -D -y "$image" >/dev/null
    status=$?
    set -e
    if [ "$status" -gt 1 ]; then
        echo "e2fsck -D failed with status $status" >&2
        exit 1
    fi
    flags=$("$DEBUGFS" -R "stat many" "$image" | sed -n 's/.*Flags: \(0x[0-9a-fA-F]*\).*/\1/p' | head -n 1)
    if [ -z "$flags" ]; then
        echo "debugfs stat many did not report Flags" >&2
        exit 1
    fi
    python3 - "$flags" <<'PY'
import sys
flags = int(sys.argv[1], 16)
if flags & 0x1000 == 0:
    raise SystemExit(f"EXT4_INDEX_FL is not set on /many ({sys.argv[1]})")
PY
}

mkfs_new() {
    image=$1
    truncate -s 2M "$image"
    # 2 MiB is too small for a journal. Drop it instead of warning.
    "$MKFS" -F -q -b 4096 -I 256 -N 2048 \
        -O 64bit,extents,flex_bg,metadata_csum,metadata_csum_seed,filetype,dir_index,sparse_super,^has_journal \
        "$image"
}

mkfs_old() {
    image=$1
    truncate -s 1M "$image"
    "$MKFS" -F -q -b 1024 -I 128 -N 256 \
        -O '^64bit,^metadata_csum,^flex_bg,^metadata_csum_seed,^dir_index,^huge_file,^extra_isize,^orphan_file,extents,filetype,sparse_super,^has_journal' \
        "$image"
}

publish() {
    src=$1
    name=$2
    gzip -c -n "$src" >"$ROOT/$name"
    echo "wrote $ROOT/$name"
}

NEW="$WORKDIR/new.img"
mkfs_new "$NEW"
base_tree "$NEW" "$WORKDIR/debian-new" "$WORKDIR/machine-id-empty" "$WORKDIR/shadow-empty" "../../../etc/machine-id"
fill_many "$NEW"
publish "$NEW" "ext4-new.img.gz"

old_tree() {
    image=$1
    {
        echo "mkdir etc"
        echo "write $WORKDIR/os-release etc/os-release"
        echo "write $WORKDIR/debian-old etc/debian_version"
        echo "write $WORKDIR/machine-id-empty etc/machine-id"
        echo "write $WORKDIR/shadow-empty etc/shadow"
        echo "symlink etc/short fast-target"
        echo "symlink etc/long $LONG"
    } >"$WORKDIR/old.cmds"
    run_debugfs "$image" "$WORKDIR/old.cmds"
}

OLD="$WORKDIR/old.img"
mkfs_old "$OLD"
old_tree "$OLD"
publish "$OLD" "ext4-old.img.gz"

variant() {
    name=$1
    cmd=$2
    shift 2
    cp "$NEW" "$WORKDIR/variant.img"
    "$cmd" "$WORKDIR/variant.img" "$@"
    publish "$WORKDIR/variant.img" "$name"
}

replace_file() {
    image=$1
    dest=$2
    src=$3
    printf 'rm %s\nwrite %s %s\n' "$dest" "$src" "$dest" >"$WORKDIR/replace.cmds"
    run_debugfs "$image" "$WORKDIR/replace.cmds"
}

replace_symlink() {
    image=$1
    dest=$2
    target=$3
    printf 'rm %s\nsymlink %s %s\n' "$dest" "$dest" "$target" >"$WORKDIR/replace.cmds"
    run_debugfs "$image" "$WORKDIR/replace.cmds"
}

variant "ext4-mid-nonempty.img.gz" replace_file etc/machine-id "$WORKDIR/machine-id-valid"
variant "ext4-mid-symlink.img.gz" replace_symlink etc/machine-id /etc/hostname
variant "ext4-shadow-locked.img.gz" replace_file etc/shadow "$WORKDIR/shadow-locked"
variant "ext4-shadow-two.img.gz" replace_file etc/shadow "$WORKDIR/shadow-two"
variant "ext4-shadow-none.img.gz" replace_file etc/shadow "$WORKDIR/shadow-none"
variant "ext4-dbus-outside.img.gz" replace_symlink var/lib/dbus/machine-id /tmp/outside

echo "fixtures ready"
