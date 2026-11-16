#!/bin/bash
# Host side of the "mount -o uid=,gid=" probe for
# docs/ai-plans/20261112-new-vm-user-related-opts.zh-Hans.md.
# Boots an APFS clone of an existing image (default ~/.cache/vibe/default.raw;
# nothing is downloaded, the original is not modified) and runs
# 20261114-vm-user-virtiofs-uid-probe.sh as root. VM traffic goes through
# VIBE_PROBE_PROXY when it is set.
#
# Usage: docs/ai-plans/references/20261114-vm-user-virtiofs-uid-probe-run.sh [vibe-binary] [image]
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
VIBE=${1:-target/release/vibe}
IMAGE=${2:-$HOME/.cache/vibe/default.raw}
PROXY=${VIBE_PROBE_PROXY:-}
# Pass --proxy only when VIBE_PROBE_PROXY is set; otherwise the VM uses the default network.
PROXY_ARGS=()
[ -n "$PROXY" ] && PROXY_ARGS=(--proxy "$PROXY")

WORK=$(mktemp -d /tmp/vibe-uidprobe.XXXX)
trap 'rm -rf "$WORK"' EXIT
cp -c "$IMAGE" "$WORK/p.raw"

RW=$WORK/rw
mkdir -p "$RW"/{d755,d700} "$WORK/ro"
chmod 755 "$RW" "$RW/d755"
echo inner >"$RW/d700/inner" && chmod 700 "$RW/d700"
echo f644 >"$RW/f644" && chmod 644 "$RW/f644"
: >"$RW/fresh-a" && : >"$RW/fresh-b"
echo ro600 >"$WORK/ro/ro600" && chmod 600 "$WORK/ro/ro600"
git init -q "$RW/repo" && echo hi >"$RW/repo/file"

"$VIBE" \
    ${PROXY_ARGS[@]+"${PROXY_ARGS[@]}"} \
    --no-mount-cwd --no-mount-mise --no-mount-sys-cache \
    --mount "$RW:/srv/rw:read-write" --mount "$WORK/ro:/srv/ro:read-only" \
    --script "$HERE/20261114-vm-user-virtiofs-uid-probe.sh" \
    --send 'systemctl poweroff' \
    "$WORK/p.raw" </dev/null >"$WORK/console.log" 2>&1 || echo "vibe exited with $?"

if [ -f "$RW/uidprobe-result.txt" ]; then
    cat "$RW/uidprobe-result.txt"
else
    tr -d '\r' <"$WORK/console.log" | grep -a '^P' || tail -n 60 "$WORK/console.log"
fi
echo "== host view"
(cd "$RW" && stat -f '%N %Su:%Sg %Lp' d755/* 2>/dev/null) || true
