#!/bin/sh
# Mounts the shared apt cache so that only _apt (root's apt) can change it.
# Runs as root inside the guest. Prints the notice word on stdout when it
# falls back to a writable bind mount.

# region INJECT_MOUNT_APT_CACHE_CONFIG
APT_CACHE_DIR="/var/cache/apt"
STAGING_DIR=""
NO_BINDFS_NOTICE="apt-no-bindfs"
# endregion INJECT_MOUNT_APT_CACHE_CONFIG

set -eu
: "${STAGING_DIR:?STAGING_DIR is empty}"
mkdir -p "$APT_CACHE_DIR"
if command -v bindfs >/dev/null && apt_uid=$(id -u _apt 2>/dev/null); then
  # bindfs stays in the background. Keep its stdout off the pipe the host reads.
  bindfs -o nonempty -u "$apt_uid" -g 0 -p 0755,af-x "$STAGING_DIR" "$APT_CACHE_DIR/" >&2
  exit 0
fi
echo 'Warning: bindfs is not available; the shared apt cache is writable by every user in this VM.' >&2
mount --bind "$STAGING_DIR" "$APT_CACHE_DIR"
echo "$NO_BINDFS_NOTICE"
