#!/bin/sh
# Truncates /etc/machine-id after systemd has committed it.
# Runs as root inside the guest. A mount over /etc/machine-id means the
# commit has not happened yet, and truncating it would only clear the overlay.

set -eu
overlay=$(findmnt -n /etc/machine-id || true)
if [ -n "$overlay" ]; then
  echo "refusing to publish: /etc/machine-id is still a mount overlay" >&2
  exit 1
fi
if [ -L /etc/machine-id ] || [ ! -f /etc/machine-id ]; then
  echo "refusing to publish: /etc/machine-id is not a regular file" >&2
  exit 1
fi
: >/etc/machine-id
if [ -s /etc/machine-id ]; then
  echo "refusing to publish: /etc/machine-id is not empty" >&2
  exit 1
fi
db=/var/lib/dbus/machine-id
if [ -L "$db" ]; then
  target=$(readlink "$db") || exit 1
  case $target in
  /etc/machine-id | ../../../etc/machine-id) ;;
  *)
    echo "refusing to publish: dbus machine-id symlink is not /etc/machine-id" >&2
    exit 1
    ;;
  esac
elif [ -f "$db" ]; then
  : >"$db"
elif [ -e "$db" ]; then
  echo "refusing to publish: dbus machine-id has an unexpected file type" >&2
  exit 1
fi
sync
