#!/bin/sh
# Fails when a file below the vm-user's HOME, other than a mount point, is
# owned by root, so user-level tools installed by root are not published.
# Runs as root inside the guest. VIBE_VM_HOME is set by the root shell.

set -eu
root_owned=$(
  find "$VIBE_VM_HOME" -xdev -user 0 2>/dev/null | while IFS= read -r path; do
    mountpoint -q "$path" || printf '%s\n' "$path"
  done
)
if [ -n "$root_owned" ]; then
  echo "refusing to publish: files below $VIBE_VM_HOME are owned by root:" >&2
  printf '%s\n' "$root_owned" | head -n 20 >&2
  exit 1
fi
