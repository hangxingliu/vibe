#!/bin/sh
# Maintenance script for the Alpine maintenance VM (POSIX sh / busybox ash).
# shellcheck -s sh src/guest/maint.sh
# shellcheck is not a build dependency.
#
# The manifest nonce is inside the seed, so the seed is mounted and parsed
# before PREP-READY. Frames after that still follow
# PREP-READY, PREP-STEP seed, self, target, fsck, inspect, apply, verify,
# unmount, PREP-RESULT. A seed that cannot be mounted sends no frame.
# The script never prints shadow bytes and never enables xtrace.

NONCE=
READY=0
SENT_RESULT=0
CODE=1
STEP=ready

stat_ids() {
    if stat -c '%a %u %g' "$1" >/dev/null 2>&1; then
        stat -c '%a %u %g' "$1"
    else
        stat -f '%OLp %u %g' "$1"
    fi
}

ends_with_nl() {
    if [ ! -s "$1" ]; then
        return 0
    fi
    b=$(tail -c 1 "$1" | od -An -tu1 | tr -d ' \n')
    [ "$b" = 10 ]
}

# Write the cleared-root form of $1 to $2. Trailing newline matches $1.
render_cleared() {
    src=$1
    dst=$2
    rm -f "$dst"
    awk -F: 'BEGIN{OFS=":"} $1=="root"{$2=""} {print}' "$src" >"$dst" || return 1
    if ! ends_with_nl "$src" && ends_with_nl "$dst"; then
        sz=$(wc -c <"$dst" | tr -d ' ')
        if [ "$sz" -gt 0 ]; then
            dd if="$dst" of="$dst.cut" bs=1 count=$((sz - 1)) 2>/dev/null || return 1
            mv "$dst.cut" "$dst" || return 1
        fi
    fi
    return 0
}

apply_shadow() {
    src=$1
    dir=$(dirname "$src")
    dst="$dir/.vibe-shadow.$NONCE"
    saved=$(stat_ids "$src") || return 1
    render_cleared "$src" "$dst" || {
        rm -f "$dst" "$dst.cut"
        return 1
    }
    # shellcheck disable=SC2086
    set -- $saved
    chmod "$1" "$dst" || return 1
    chown "$2:$3" "$dst" || return 1
    sync
    mv "$dst" "$src" || return 1
    return 0
}

root_field_empty() {
    awk -F: '
        $1 == "root" { n++; if ($2 != "") bad = 1 }
        END { if (n != 1 || bad) exit 1 }
    ' "$1"
}

shadow_selftest() {
    NONCE=selftest
    tmp=${TMPDIR:-/tmp}/vibe-shadow-selftest.$$
    mkdir -p "$tmp" || exit 1
    cleanup() { rm -rf "$tmp"; }
    trap cleanup EXIT
    fail_case() {
        printf 'SHADOW_SELFTEST_FAIL %s\n' "$1"
        exit 1
    }
    run_case() {
        name=$1
        src=$2
        cp "$src" "$tmp/work" || fail_case "$name"
        meta=$(stat_ids "$tmp/work") || fail_case "$name"
        render_cleared "$src" "$tmp/expect" || fail_case "$name"
        apply_shadow "$tmp/work" || fail_case "$name"
        after=$(stat_ids "$tmp/work") || fail_case "$name"
        if [ "$meta" != "$after" ]; then
            fail_case "$name"
        fi
        if ! cmp -s "$tmp/work" "$tmp/expect"; then
            fail_case "$name"
        fi
        if ! root_field_empty "$tmp/work"; then
            fail_case "$name"
        fi
    }

    printf 'root:!unprovisioned:19999:0:99999:7:::\ndaemon:*:19999:0:99999:7:::\n' >"$tmp/locked"
    printf 'root:*:19999:0:99999:7:::\ndaemon:*:19999:0:99999:7:::\n' >"$tmp/star"
    printf 'bin:*:1:0:99999:7:::\nroot:$y$abc:19999:0:99999:7:::\n' >"$tmp/hash"
    printf 'root::19999:0:99999:7:::\ndaemon:*:19999:0:99999:7:::\n' >"$tmp/empty"
    printf 'root:!unprovisioned:19999:0:99999:7:::' >"$tmp/nonewline"
    chmod 640 "$tmp/locked" "$tmp/star" "$tmp/hash" "$tmp/empty" "$tmp/nonewline"

    run_case locked "$tmp/locked"
    run_case star "$tmp/star"
    run_case hash "$tmp/hash"
    run_case empty "$tmp/empty"
    run_case nonewline "$tmp/nonewline"
    printf 'SHADOW_SELFTEST_OK\n'
    exit 0
}

if [ "${1:-}" = "shadow-selftest" ]; then
    shadow_selftest
    exit $?
fi

exec >/dev/hvc0 2>&1
# virtio consoles default to ONLCR, which turns each newline into CRLF.
stty -F /dev/hvc0 -onlcr 2>/dev/null || true
stty -F /dev/hvc1 -onlcr 2>/dev/null || true

log() {
    printf 'maint: %s\n' "$*"
}

emit() {
    printf 'VIBE1 %s %s\n' "$NONCE" "$*" >/dev/hvc1
}

fail() {
    CODE=$1
    STEP=$2
    log "failed $STEP"
    exit 1
}

on_exit() {
    if [ "$SENT_RESULT" -eq 0 ] && [ -n "$NONCE" ] && [ "$READY" -eq 1 ]; then
        printf 'VIBE1 %s PREP-RESULT fail %s %s\n' "$NONCE" "$CODE" "$STEP" >/dev/hvc1
        SENT_RESULT=1
    fi
    rm -rf /run/vibe-maint
    sync
    poweroff
}
trap on_exit EXIT

valid_token() {
    case $1 in
        *[!A-Za-z0-9_.-]*) return 1 ;;
        '') return 1 ;;
    esac
    return 0
}

digits() {
    case $1 in
        *[!0-9]*) return 1 ;;
        '') return 1 ;;
    esac
    return 0
}

mount_seed() {
    mkdir -p /run/vibe-seed /run/vibe-maint /mnt/target
    cidata=$(findfs LABEL=cidata) || return 1
    [ "$cidata" = /dev/vdc ] || return 1
    mount -t iso9660 -o ro,nodev,nosuid,noexec /dev/vdc /run/vibe-seed || return 1
    return 0
}

parse_manifest() {
    [ -f /run/vibe-seed/manifest ] || return 1
    FORMAT=
    ALPINE_UUID=
    DISK_BYTES=
    ROOT_PART=
    ROOT_PARTUUID=
    ROOT_TYPE=
    ESP_PART=
    POLICY=
    while IFS= read -r line || [ -n "$line" ]; do
        case $line in
            '' | '#'*) continue ;;
        esac
        key=${line%%=*}
        val=${line#*=}
        valid_token "$key" || return 1
        valid_token "$val" || return 1
        case $key in
            format) FORMAT=$val ;;
            nonce) NONCE=$val ;;
            alpine_root_uuid) ALPINE_UUID=$val ;;
            disk_bytes) DISK_BYTES=$val ;;
            root_part) ROOT_PART=$val ;;
            root_partuuid) ROOT_PARTUUID=$val ;;
            root_type) ROOT_TYPE=$val ;;
            esp_part) ESP_PART=$val ;;
            policy) POLICY=$val ;;
            *) return 1 ;;
        esac
    done </run/vibe-seed/manifest
    echo "$NONCE" | grep -Eq '^[0-9a-f]{32}$' || return 1
    emit PREP-READY
    READY=1
    [ "$FORMAT" = 1 ] || return 1
    [ "$POLICY" = minimal ] || return 1
    valid_token "$ALPINE_UUID" || return 1
    digits "$DISK_BYTES" || return 1
    digits "$ROOT_PART" || return 1
    digits "$ESP_PART" || return 1
    valid_token "$ROOT_PARTUUID" || return 1
    valid_token "$ROOT_TYPE" || return 1
    return 0
}

lower() {
    printf '%s' "$1" | tr 'A-Z' 'a-z'
}

step_self() {
    rootdev=$(awk '$2 == "/" { print $1; exit }' /proc/mounts)
    [ "$rootdev" = /dev/vda2 ] || return 1
    found=$(findfs "UUID=$ALPINE_UUID") || return 1
    [ "$found" = /dev/vda2 ] || return 1
    return 0
}

sgdisk_field() {
    label=$1
    sgdisk -i "$ROOT_PART" /dev/vdb | sed -n "s/^${label}: \\([0-9A-Fa-f-]*\\).*/\\1/p" | head -n 1
}

step_target() {
    blocks=$(cat /sys/class/block/vdb/size) || return 1
    bytes=$((blocks * 512))
    [ "$bytes" = "$DISK_BYTES" ] || return 1
    [ -b "/dev/vdb$ROOT_PART" ] || return 1
    code=$(sgdisk_field "Partition GUID code") || return 1
    uniq=$(sgdisk_field "Partition unique GUID") || return 1
    [ -n "$code" ] && [ -n "$uniq" ] || return 1
    [ "$(lower "$code")" = "$(lower "$ROOT_TYPE")" ] || return 1
    [ "$(lower "$uniq")" = "$(lower "$ROOT_PARTUUID")" ] || return 1
    return 0
}

step_fsck() {
    e2fsck -p "/dev/vdb$ROOT_PART"
    rc=$?
    [ "$rc" -eq 0 ] || [ "$rc" -eq 1 ]
}

sshd_allows_empty() {
    [ -f "$1" ] || return 1
    awk '
        {
            line = $0
            sub(/^[ \t]+/, "", line)
            if (line ~ /^#/ || line == "") next
            low = tolower(line)
            if (low ~ /^permitemptypasswords[ \t]+yes([ \t]|$)/) found = 1
        }
        END { exit found ? 0 : 1 }
    ' "$1"
}

step_inspect() {
    mount -t ext4 -o ro,nodev,nosuid "/dev/vdb$ROOT_PART" /mnt/target || return 1
    id=$(awk -F= '$1 == "ID" { gsub(/"/, "", $2); print $2; exit }' /mnt/target/etc/os-release)
    [ "$id" = debian ] || return 1
    if [ -L /mnt/target/etc ] || [ ! -d /mnt/target/etc ]; then
        return 1
    fi
    if [ -L /mnt/target/etc/machine-id ] || [ ! -f /mnt/target/etc/machine-id ]; then
        return 1
    fi
    if [ -L /mnt/target/etc/shadow ] || [ ! -f /mnt/target/etc/shadow ]; then
        return 1
    fi
    awk -F: '
        $1 == "root" { n++; if (NF < 9) bad = 1 }
        END { if (n != 1 || bad) exit 1 }
    ' /mnt/target/etc/shadow || return 1
    db=/mnt/target/var/lib/dbus/machine-id
    if [ -L "$db" ]; then
        target=$(readlink "$db") || return 1
        case $target in
            /etc/machine-id | ../../../etc/machine-id) ;;
            *) return 1 ;;
        esac
    elif [ -f "$db" ]; then
        :
    elif [ -e "$db" ]; then
        return 1
    fi
    if [ -f /mnt/target/etc/ssh/sshd_config ] && sshd_allows_empty /mnt/target/etc/ssh/sshd_config; then
        return 1
    fi
    for conf in /mnt/target/etc/ssh/sshd_config.d/*.conf; do
        [ -f "$conf" ] || continue
        if sshd_allows_empty "$conf"; then
            return 1
        fi
    done
    awk -F: '
        $1 == "root" { n++; if ($2 != "x") bad = 1 }
        END { if (n != 1 || bad) exit 1 }
    ' /mnt/target/etc/passwd || return 1
    mount -o remount,rw /mnt/target || return 1
    return 0
}

step_apply() {
    mkdir -p /run/vibe-maint
    # The backup copy is mode 0600 so a later failure does not leave the
    # original shadow world-readable on the Alpine tmpfs. Permission checks
    # use the saved stat line, not the copy.
    stat_ids /mnt/target/etc/shadow >/run/vibe-maint/shadow.meta || return 1
    cp /mnt/target/etc/shadow /run/vibe-maint/shadow.prev || return 1
    chmod 0600 /run/vibe-maint/shadow.prev /run/vibe-maint/shadow.meta || return 1
    : >/mnt/target/etc/machine-id || return 1
    db=/mnt/target/var/lib/dbus/machine-id
    if [ ! -L "$db" ] && [ -f "$db" ]; then
        : >"$db" || return 1
    fi
    apply_shadow /mnt/target/etc/shadow || return 1
    return 0
}

step_verify() {
    [ -f /mnt/target/etc/machine-id ] || return 1
    [ ! -s /mnt/target/etc/machine-id ] || return 1
    root_field_empty /mnt/target/etc/shadow || return 1
    render_cleared /run/vibe-maint/shadow.prev /run/vibe-maint/shadow.expect || return 1
    cmp -s /mnt/target/etc/shadow /run/vibe-maint/shadow.expect || return 1
    now=$(stat_ids /mnt/target/etc/shadow) || return 1
    prev=$(cat /run/vibe-maint/shadow.meta) || return 1
    [ "$now" = "$prev" ] || return 1
    rm -f /run/vibe-maint/shadow.prev /run/vibe-maint/shadow.expect /run/vibe-maint/shadow.meta
    return 0
}

step_unmount() {
    sync
    umount /mnt/target || return 1
    if awk '$1 ~ "^/dev/vdb" { bad = 1 } END { exit bad ? 1 : 0 }' /proc/mounts; then
        :
    else
        return 1
    fi
    e2fsck -n "/dev/vdb$ROOT_PART" || return 1
    return 0
}

if ! mount_seed; then
    log "seed mount failed"
    exit 1
fi
if ! parse_manifest; then
    fail 2 seed
fi
log "seed ok"
emit PREP-STEP seed

if ! step_self; then
    fail 3 self
fi
log "self ok"
emit PREP-STEP self

if ! step_target; then
    fail 4 target
fi
log "target ok"
emit PREP-STEP target

if ! step_fsck; then
    fail 5 fsck
fi
log "fsck ok"
emit PREP-STEP fsck

if ! step_inspect; then
    fail 6 inspect
fi
log "inspect ok"
emit PREP-STEP inspect

if ! step_apply; then
    fail 7 apply
fi
log "apply ok"
emit PREP-STEP apply

if ! step_verify; then
    fail 8 verify
fi
log "verify ok"
emit PREP-STEP verify

if ! step_unmount; then
    fail 9 unmount
fi
log "unmount ok"
emit PREP-STEP unmount

SENT_RESULT=1
emit PREP-RESULT ok 0 done
log "result ok"
exit 0
