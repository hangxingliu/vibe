#!/bin/bash
# Host side of the Q4/T11 VirtioFS permission probe for
# docs/ai-plans/20261112-new-vm-user-related-opts.zh-Hans.md.
#
# Boots an APFS clone of an existing image (default: ~/.cache/vibe/default.raw;
# nothing is downloaded and the original is not modified), runs
# 20261112-vm-user-virtiofs-probe.sh as root, then reports the host view of the
# files the guest touched. VM traffic goes through VIBE_PROBE_PROXY when it is set.
#
# Usage: docs/ai-plans/references/20261112-vm-user-virtiofs-probe-run.sh [vibe-binary] [image]
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
VIBE=${1:-target/release/vibe}
IMAGE=${2:-$HOME/.cache/vibe/default.raw}
PROXY=${VIBE_PROBE_PROXY:-}
# Pass --proxy only when VIBE_PROBE_PROXY is set; otherwise the VM uses the default network.
PROXY_ARGS=()
[ -n "$PROXY" ] && PROXY_ARGS=(--proxy "$PROXY")

WORK=$(mktemp -d /tmp/vibe-t11.XXXX)
trap 'rm -rf "$WORK"' EXIT
cp -c "$IMAGE" "$WORK/t11.raw"

RW=$WORK/rw
mkdir -p "$RW"/{d755,d777,d700,d555,.vibe,.git} "$WORK/ro"
chmod 755 "$RW" "$RW/d755"
chmod 777 "$RW/d777"
echo inner >"$RW/d700/inner"
chmod 700 "$RW/d700"
chmod 555 "$RW/d555"
echo f644 >"$RW/f644" && chmod 644 "$RW/f644"
echo f666 >"$RW/f666" && chmod 666 "$RW/f666"
echo secret >"$RW/.env"
echo hidden >"$RW/.vibe/secret"
: >"$RW/t11-marker"
for u in 0 1000 "$(id -u)"; do : >"$RW/del-$u"; done
: >"$RW/chmod-target" && chmod 644 "$RW/chmod-target"
: >"$RW/chown-target"
echo ro >"$WORK/ro/ro-file"
git init -q "$RW/repo" && echo hi >"$RW/repo/file"

HOST_UID=$(id -u) HOST_GID=$(id -g) "$VIBE" \
    ${PROXY_ARGS[@]+"${PROXY_ARGS[@]}"} \
    --no-mount-cwd --no-mount-mise --no-mount-sys-cache \
    --mount "$RW:/srv/rw:read-write" --mount "$WORK/ro:/srv/ro:read-only" \
    --env HOST_UID --env HOST_GID \
    --script "$HERE/20261112-vm-user-virtiofs-probe.sh" \
    --send 'systemctl poweroff' \
    "$WORK/t11.raw" </dev/null >"$WORK/console.log" 2>&1 || echo "vibe exited with $?"

echo "== guest results"
if [ -f "$RW/t11-result.txt" ]; then
    cat "$RW/t11-result.txt"
else
    tr -d '\r' <"$WORK/console.log" | grep -a 'T11' || tail -n 60 "$WORK/console.log"
fi
echo "== host view (name owner:group mode)"
(cd "$RW" && stat -f '%N %Su:%Sg %Lp' . * d755/* d777/* d700/* d555/* 2>/dev/null) || true
echo "== host f644 / f666 contents"
cat "$RW/f644" "$RW/f666"
echo "== host .env (must still be 'secret')"
cat "$RW/.env"
