#!/bin/bash
# Guest side of the Q4/Q11 "mount -o uid=,gid=" probe for
# docs/ai-plans/20261112-new-vm-user-related-opts.zh-Hans.md.
# Run as root through `vibe --script` by 20261114-vm-user-virtiofs-uid-probe-run.sh.
# Needs /srv/rw (read-write --mount) and /srv/ro (read-only --mount). Other UIDs
# are entered with setpriv. Always exits 0 so the VM powers off normally.

OUT=/tmp/uidprobe-result.txt
: >"$OUT"
r() { printf '%s\n' "$*" | tee -a "$OUT"; }

# try LABEL UID COMMAND (uid 0 = root)
try() {
    local label=$1 uid=$2 cmd=$3 s
    if [ "$uid" = 0 ]; then
        sh -c "$cmd" >/tmp/uidprobe.out 2>&1
    else
        setpriv --reuid="$uid" --regid="$uid" --clear-groups -- sh -c "$cmd" >/tmp/uidprobe.out 2>&1
    fi
    s=$?
    r "P $label uid=$uid status=$s out=[$(tr '\n' ' ' </tmp/uidprobe.out | cut -c1-200)]"
}
drop() { sync; echo 3 >/proc/sys/vm/drop_caches; }

r "P-BEGIN kernel=$(uname -r) $(mount --version | head -n1)"
grep -E 'virtiofs' /proc/self/mountinfo | sed 's/^/P   /' | tee -a "$OUT"
TAG=$(findmnt -n -t virtiofs -o SOURCE,FSROOT | awk '$2 == "/" {print $1; exit}')
r "P tag=$TAG"

# 1. Mount options. A second mount of the same tag reuses the superblock, but
# options are parsed first, so unknown options fail before that.
mkdir -p /mnt/p
for o in "uid=1000,gid=1000" "uid=1000" "gid=1000" "user_id=1000,group_id=1000" \
    "default_permissions" "allow_other" "rootmode=040000" "dax" ""; do
    if [ -n "$o" ]; then
        try "mount-o[$o]" 0 "mount -t virtiofs -o '$o' $TAG /mnt/p && findmnt -n -o OPTIONS /mnt/p && stat -c '%u:%g' /mnt/p/rw_*/f644 2>&1 | head -n1"
    else
        try "mount-o[none]" 0 "mount -t virtiofs $TAG /mnt/p && findmnt -n -o OPTIONS /mnt/p"
    fi
    umount /mnt/p 2>/dev/null
    r "P   dmesg: $(dmesg | tail -n 1)"
done
# Remount of the existing staging mount with uid=/gid=.
STAGE=$(findmnt -n -t virtiofs -o TARGET,FSROOT | awk '$2 == "/" {print $1; exit}')
try "remount-uid" 0 "mount -o remount,uid=1000,gid=1000 $STAGE"
try "bind-uid" 0 "mkdir -p /mnt/b && mount --bind -o uid=1000,gid=1000 /srv/rw /mnt/b && findmnt -n -o OPTIONS /mnt/b && stat -c '%u:%g' /mnt/b/f644"
umount /mnt/b 2>/dev/null

# 2. Which owner does the guest see, depending on who asked first.
# fresh-* files have never been touched by the guest.
drop
try "stat-first-1000" 1000 "stat -c '%n %u:%g %a' /srv/rw/fresh-a /srv/rw/d700 /srv/ro/ro600"
try "stat-then-root" 0 "stat -c '%n %u:%g %a' /srv/rw/fresh-a /srv/rw/d700 /srv/ro/ro600"
try "stat-then-1001" 1001 "stat -c '%n %u:%g %a' /srv/rw/fresh-a /srv/rw/d700 /srv/ro/ro600"
drop
try "stat-first-root" 0 "stat -c '%n %u:%g %a' /srv/rw/fresh-b"
try "stat-then-1000" 1000 "stat -c '%n %u:%g %a' /srv/rw/fresh-b"
sleep 3
try "stat-1000-after-3s" 1000 "stat -c '%n %u:%g %a' /srv/rw/fresh-b"
try "stat-root-after-1000" 0 "stat -c '%n %u:%g %a' /srv/rw/fresh-b"

# 3. Permission checks by the guest kernel (default_permissions): a 0600 host
# file in the read-only share, and a 0700 host directory.
drop
try "read-ro600" 1000 "cat /srv/ro/ro600"
try "read-d700" 1000 "cat /srv/rw/d700/inner"
try "create-d755" 1000 "echo x > /srv/rw/d755/n1000 && stat -c '%u:%g' /srv/rw/d755/n1000"

# 4. git owner check for 1000 with and without root touching the repo first.
mkdir -p /tmp/h1000 && chown 1000:1000 /tmp/h1000
drop
try "git-status-1000-fresh" 1000 "HOME=/tmp/h1000 git -C /srv/rw/repo status --short; echo rc=\$?"
drop
try "git-status-root" 0 "git -C /srv/rw/repo status --short; echo rc=\$?"
try "git-status-1000-after-root" 1000 "HOME=/tmp/h1000 git -C /srv/rw/repo status --short; echo rc=\$?"

# 5. How long the owner seen by root stays cached for UID 1000.
try "owner-cache-window" 0 "stat -c %u /srv/rw/fresh-a >/dev/null; s=\$(date +%s%N); i=0; while [ \$i -lt 100 ]; do o=\$(setpriv --reuid=1000 --regid=1000 --clear-groups -- stat -c %u /srv/rw/fresh-a); [ \"\$o\" = 1000 ] && break; sleep 0.05; i=\$((i+1)); done; echo owner=\$o after_ms=\$(( (\$(date +%s%N) - s) / 1000000 ))"
try "owner-cache-window-2" 0 "stat -c %u /srv/rw/fresh-a >/dev/null; s=\$(date +%s%N); i=0; while [ \$i -lt 100 ]; do o=\$(setpriv --reuid=1000 --regid=1000 --clear-groups -- stat -c %u /srv/rw/fresh-a); [ \"\$o\" = 1000 ] && break; sleep 0.05; i=\$((i+1)); done; echo owner=\$o after_ms=\$(( (\$(date +%s%N) - s) / 1000000 ))"

r "P-END"
cp "$OUT" /srv/rw/uidprobe-result.txt
exit 0
