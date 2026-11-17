#!/bin/sh
# Installs a transient unit that clears machine-id at poweroff. The unit and
# its script live under /run and are not part of the published image.
# ExecStop runs on both exit (.bash_logout powers off) and systemctl poweroff.
# /run is mounted noexec, so the script is run through /bin/sh.
# Runs as root inside the guest.

set -eu
overlay=$(findmnt -n /etc/machine-id || true)
if [ -n "$overlay" ]; then
  echo "refusing to publish: /etc/machine-id is still a mount overlay" >&2
  exit 1
fi
mkdir -p /run/vibe-finalize
cat >/run/vibe-finalize/clear-machine-id.sh <<'EOF'
#!/bin/sh
set -eu
if [ -L /etc/machine-id ] || [ ! -f /etc/machine-id ]; then
    echo "vibe-finalize-template: /etc/machine-id is not a regular file" >&2
    exit 1
fi
: > /etc/machine-id
db=/var/lib/dbus/machine-id
if [ -L "$db" ]; then
    target=$(readlink "$db") || exit 1
    case $target in
        /etc/machine-id|../../../etc/machine-id) ;;
        *)
            echo "vibe-finalize-template: dbus machine-id symlink is not /etc/machine-id" >&2
            exit 1
            ;;
    esac
elif [ -f "$db" ]; then
    : > "$db"
elif [ -e "$db" ]; then
    echo "vibe-finalize-template: dbus machine-id has an unexpected file type" >&2
    exit 1
fi
sync
EOF
systemd-run \
  --unit=vibe-finalize-template \
  -p Type=oneshot \
  -p RemainAfterExit=yes \
  -p 'ExecStop=/bin/sh /run/vibe-finalize/clear-machine-id.sh' \
  /bin/true
systemctl is-active --quiet vibe-finalize-template.service
