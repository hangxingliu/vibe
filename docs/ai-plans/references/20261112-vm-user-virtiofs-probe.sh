#!/bin/bash
# Guest side of the Q4/T11 VirtioFS permission probe for
# docs/ai-plans/20261112-new-vm-user-related-opts.zh-Hans.md.
# Run as root through `vibe --script` by 20261112-vm-user-virtiofs-probe-run.sh.
# Needs HOST_UID and HOST_GID (`--env`), /srv/rw (read-write --mount) and
# /srv/ro (read-only --mount). Creates no accounts: other UIDs are entered with
# setpriv. Always exits 0 so the VM powers off normally.

H=${HOST_UID:?}
HG=${HOST_GID:?}
OUT=/tmp/t11-result.txt
: >"$OUT"

r() { printf '%s\n' "$*" | tee -a "$OUT"; }

# try LABEL UID COMMAND: run COMMAND with sh as UID (0 = root) and record the
# exit status and the first part of its output.
try() {
    local label=$1 uid=$2 cmd=$3 s
    if [ "$uid" = 0 ]; then
        sh -c "$cmd" >/tmp/t11-cmd.out 2>&1
    else
        setpriv --reuid="$uid" --regid="$uid" --clear-groups -- sh -c "$cmd" >/tmp/t11-cmd.out 2>&1
    fi
    s=$?
    r "T11 $label uid=$uid status=$s out=[$(tr '\n' ' ' </tmp/t11-cmd.out | cut -c1-160)]"
}

r "T11-BEGIN"
r "T11 kernel=$(uname -r) util-linux=$(mount --version | head -n1)"
r "T11 host_uid=$H host_gid=$HG"
r "T11 virtiofs-mounts:"
grep -E 'virtiofs' /proc/self/mountinfo | sed 's/^/T11   /' | tee -a "$OUT"
r "T11 stat (guest view, name uid:gid mode):"
for p in /srv/rw /srv/rw/f644 /srv/rw/f666 /srv/rw/d755 /srv/rw/d777 /srv/rw/d700 /srv/rw/d555 /srv/rw/.env /srv/rw/.vibe /srv/ro /srv/ro/ro-file; do
    r "T11   $(stat -c '%n %u:%g %a' "$p" 2>&1)"
done

# 1-5: create, modify, delete, mkdir, read for root, a Debian-style UID 1000,
# the host UID, and an unrelated UID 1001 in the host group.
for u in 0 1000 "$H"; do
    try "create-root-dir" "$u" "echo x > /srv/rw/new-$u"
    try "create-d755" "$u" "echo x > /srv/rw/d755/new-$u"
    try "create-d777" "$u" "echo x > /srv/rw/d777/new-$u"
    try "create-d700" "$u" "echo x > /srv/rw/d700/new-$u"
    try "create-d555" "$u" "echo x > /srv/rw/d555/new-$u"
    try "append-f644" "$u" "echo $u >> /srv/rw/f644"
    try "append-f666" "$u" "echo $u >> /srv/rw/f666"
    try "read-d700-inner" "$u" "cat /srv/rw/d700/inner"
    try "mkdir" "$u" "mkdir /srv/rw/dir-$u"
    try "delete-host-file" "$u" "rm /srv/rw/del-$u"
    try "write-ro" "$u" "echo x > /srv/ro/new-$u"
done
try "create-d777-gid" 1001 "echo x > /srv/rw/d777/new-1001"
r "T11 owners of files created above (guest view):"
for f in /srv/rw/new-* /srv/rw/d777/new-* /srv/rw/dir-*; do
    r "T11   $(stat -c '%n %u:%g %a' "$f" 2>&1)"
done

# 6: chmod/chown from the guest (the host runner checks the host side).
try "chmod-600" 0 "chmod 600 /srv/rw/chmod-target && stat -c '%a' /srv/rw/chmod-target"
try "chown-1000" 0 "chown 1000:1000 /srv/rw/chown-target && stat -c '%u:%g' /srv/rw/chown-target"

# 7: idmapped bind mounts. A tmpfs control shows whether the kernel and
# util-linux support X-mount.idmap at all; then both mapping orientations on
# the virtiofs share.
mkdir -p /tmp/idctl-src /tmp/idctl-dst /srv/idm-a /srv/idm-b
mount -t tmpfs -o mode=0755 tmpfs /tmp/idctl-src && chown "$H:$HG" /tmp/idctl-src
try "idmap-tmpfs-control" 0 "mount --bind -o X-mount.idmap='u:1000:$H:1 g:1000:$HG:1' /tmp/idctl-src /tmp/idctl-dst && stat -c '%u:%g %a' /tmp/idctl-dst"
try "idmap-tmpfs-control-write" 1000 "echo x > /tmp/idctl-dst/new && stat -c '%u:%g' /tmp/idctl-dst/new"
try "idmap-virtiofs-a(u:1000:H)" 0 "mount --bind -o X-mount.idmap='u:1000:$H:1 g:1000:$HG:1' /srv/rw /srv/idm-a && stat -c '%u:%g %a' /srv/idm-a"
try "idmap-virtiofs-a-write" 1000 "echo x > /srv/idm-a/idmap-a-1000 && stat -c '%u:%g' /srv/idm-a/idmap-a-1000"
try "idmap-virtiofs-b(u:H:1000)" 0 "mount --bind -o X-mount.idmap='u:$H:1000:1 g:$HG:1000:1' /srv/rw /srv/idm-b && stat -c '%u:%g %a' /srv/idm-b"
try "idmap-virtiofs-b-write" 1000 "echo x > /srv/idm-b/idmap-b-1000 && stat -c '%u:%g' /srv/idm-b/idmap-b-1000"
STAGING_ROOT=$(findmnt -n -t virtiofs -o TARGET,FSROOT | awk '$2 == "/" {print $1; exit}')
try "idmap-virtiofs-staging-root" 0 "mkdir -p /srv/idm-c && mount --bind -o X-mount.idmap='u:1000:$H:1 g:1000:$HG:1' $STAGING_ROOT /srv/idm-c && ls /srv/idm-c"
r "T11 dmesg tail:"
dmesg 2>/dev/null | tail -n 5 | sed 's/^/T11   /' | tee -a "$OUT"
umount /srv/idm-a /srv/idm-b /srv/idm-c /tmp/idctl-dst /tmp/idctl-src 2>/dev/null

# 8: staging directory access for a normal user.
STAGED_RW=$(ls -d "$STAGING_ROOT"/*/t11-marker 2>/dev/null | head -n1)
STAGED_RW=${STAGED_RW%/t11-marker}
r "T11 staging_root=$STAGING_ROOT staged_rw=$STAGED_RW mode=$(stat -c '%u:%g %a' "$STAGING_ROOT")"
try "staging-list" 1000 "ls $STAGING_ROOT"
try "staging-read-env" 1000 "cat $STAGED_RW/.env"
try "staging-read-env" "$H" "cat $STAGED_RW/.env"

# 9-10: unmount and masks.
try "umount-share" "$H" "umount /srv/rw"
try "userns-umount-env" "$H" "unshare -Urm sh -c 'umount /srv/rw/.env; cat /srv/rw/.env'"
try "userns-umount-vibe" "$H" "unshare -Urm sh -c 'umount /srv/rw/.vibe; ls -A /srv/rw/.vibe'"
try "env-mask-read" "$H" "wc -c < /srv/rw/.env"
try "env-mask-write" "$H" "echo leak >> /srv/rw/.env"
try "env-mask-write" 1000 "echo leak >> /srv/rw/.env"
try "vibe-mask-list" 1000 "ls -A /srv/rw/.vibe"
try "vibe-mask-write" 1000 "touch /srv/rw/.vibe/x"
r "T11   $(stat -c '%n %u:%g %a' /srv/rw/.env /srv/rw/.vibe | tr '\n' ' ')"
try "staging-vibe-list" 1000 "ls -A $STAGED_RW/.vibe"
try "staging-git-write" 1000 "touch $STAGED_RW/.git/x"
try "git-write" 1000 "touch /srv/rw/.git/x"

# Files show up as 0:0, so git's safe.directory owner check matters for a
# non-root user working in a shared repository.
mkdir -p /tmp/h1000 && chown 1000:1000 /tmp/h1000
try "git-status" 0 "git -C /srv/rw/repo status --short"
try "git-status" 1000 "HOME=/tmp/h1000 git -C /srv/rw/repo status --short"
try "git-status-safe-dir" 1000 "HOME=/tmp/h1000 git -c safe.directory='*' -C /srv/rw/repo status --short"
try "git-commit-safe-dir" 1000 "HOME=/tmp/h1000 git -c safe.directory='*' -c user.name=t -c user.email=t@t -C /srv/rw/repo commit -q --allow-empty -m probe && echo committed"

# 11-12: facts for Q1-Q3 and the host UID.
r "T11 sudo=$(command -v sudo || echo missing) su=$(su --version 2>&1 | head -n1)"
r "T11 su-pty=$(su --help 2>&1 | grep -c -- '--pty')"
r "T11 pam-su-common-auth=$(grep -c common-auth /etc/pam.d/su) nullok=[$(grep -h pam_unix /etc/pam.d/common-auth | tr -s ' ')]"
r "T11 root-passwd-status=[$(passwd -S root)]"
r "T11 legacy_tiocsti=$(sysctl -n dev.tty.legacy_tiocsti 2>&1)"
r "T11 unprivileged_userns_clone=$(sysctl -n kernel.unprivileged_userns_clone 2>&1)"
r "T11 getent-passwd-$H=[$(getent passwd "$H")] getent-group-$HG=[$(getent group "$HG")] getent-passwd-1000=[$(getent passwd 1000)]"
r "T11 useradd-uid-min=$(awk '$1 == "UID_MIN" {print $2}' /etc/login.defs)"
r "T11-END"
cp "$OUT" /srv/rw/t11-result.txt
exit 0
