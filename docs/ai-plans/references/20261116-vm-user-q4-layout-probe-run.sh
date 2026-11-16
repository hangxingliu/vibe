#!/bin/bash
# Host side of the Q4 re-check for
# docs/ai-plans/20261112-new-vm-user-related-opts.zh-Hans.md (2026-11-16).
# Boots an APFS clone of an existing image (default ~/.cache/vibe/default.raw;
# no image is downloaded, the original is not modified) and runs
# 20261116-vm-user-q4-layout-probe.sh as root. VM traffic goes through
# VIBE_PROBE_PROXY when it is set. The guest installs bindfs with apt when it
# is missing; the host apt cache is not mounted (a fake one is used instead).
#
# Usage: docs/ai-plans/references/20261116-vm-user-q4-layout-probe-run.sh [vibe-binary] [image]
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
VIBE=${1:-target/release/vibe}
IMAGE=${2:-$HOME/.cache/vibe/default.raw}
PROXY=${VIBE_PROBE_PROXY:-}
# Pass --proxy only when VIBE_PROBE_PROXY is set; otherwise the VM uses the default network.
PROXY_ARGS=()
[ -n "$PROXY" ] && PROXY_ARGS=(--proxy "$PROXY")
# Upstream for the proxied gateway DNS (the host resolver may be link-local,
# which is in the bypass list and unreachable from the helper).
DNS=${VIBE_PROBE_DNS:-1.1.1.1}

WORK=$(mktemp -d /tmp/vibe-q4probe.XXXX)
trap 'rm -rf "$WORK"' EXIT
cp -c "$IMAGE" "$WORK/p.raw"

PJ=$WORK/proj
mkdir -p "$PJ"/{d755,d700,d555,.vibe,node_modules} "$WORK"/{apt/archives/partial,ro,out}
git init -q "$PJ" && git -C "$PJ" -c user.name=p -c user.email=p@p commit -q --allow-empty -m init
echo secret >"$PJ/.env" && chmod 600 "$PJ/.env"
echo host-vibe >"$PJ/.vibe/state"
echo host-nm >"$PJ/node_modules/m"
echo inner >"$PJ/d700/inner" && chmod 700 "$PJ/d700"
chmod 555 "$PJ/d555"
echo f644 >"$PJ/f644" && chmod 644 "$PJ/f644"
echo ro600 >"$WORK/ro/ro600" && chmod 600 "$WORK/ro/ro600"
echo fake >"$WORK/apt/archives/fake.deb"
for n in proj apt ro out; do
    d=$WORK/$n
    [ "$n" = proj ] && d=$PJ
    echo "$n" >"$d/probe-id"
done

"$VIBE" \
    ${PROXY_ARGS[@]+"${PROXY_ARGS[@]}"} --dns "$DNS" \
    --no-mount-cwd --no-mount-mise --no-mount-sys-cache \
    --mount "$PJ:/srv/proj:read-write" \
    --mount "$WORK/apt:/srv/apt:read-write" \
    --mount "$WORK/ro:/srv/ro:read-only" \
    --mount "$WORK/out:/srv/out:read-write" \
    --script "$HERE/20261116-vm-user-q4-layout-probe.sh" \
    --send 'systemctl poweroff' \
    "$WORK/p.raw" </dev/null >"$WORK/console.log" 2>&1 || echo "vibe exited with $?"

if [ -f "$WORK/out/q4probe-result.txt" ]; then
    cat "$WORK/out/q4probe-result.txt"
else
    tr -d '\r' <"$WORK/console.log" | grep -a '^P' || tail -n 80 "$WORK/console.log"
fi
echo "== host view"
echo "fs: $(diskutil info "$(df "$WORK" | awk 'NR==2 {print $1}')" | grep -E 'Personality|Case' | tr -s ' ' | tr '\n' ' ')"
echo ".env: $(cat "$PJ/.env")"
echo ".git entries: $(ls "$PJ/.git" | tr '\n' ' ')"
echo "proj: $(ls -a "$PJ" | tr '\n' ' ')"
(cd "$PJ" && stat -f '%N %Su:%Sg %Lp' d755/* d700/* f644 2>/dev/null) || true
echo ".git/config tail: $(tail -n 2 "$PJ/.git/config" | tr '\n' ' ')"
echo "guest_node_modules: $(ls ~/.cache/vibe/guest_node_modules 2>/dev/null | tr '\n' ' ')"
echo "apt: $(cd "$WORK/apt" && find . -type f | tr '\n' ' ')"
echo "apt fake.deb: $(cat "$WORK/apt/archives/fake.deb")"
