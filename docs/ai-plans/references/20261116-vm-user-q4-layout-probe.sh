#!/bin/bash
# Guest side of the Q4 re-check for
# docs/ai-plans/20261112-new-vm-user-related-opts.zh-Hans.md (2026-11-16).
# Run as root through `vibe --script` by 20261116-vm-user-q4-layout-probe-run.sh.
#
# Rebuilds the layout the user proposed (staging under a root-only 0700 dir,
# project bind mount into the user's HOME, mktemp/tmpfs/read-only masks,
# bindfs for the apt cache) and checks, as a normal user:
#   R1/R2: does the guest UID matter for writes through that layout;
#   R3:    are idmapped bind mounts usable on virtiofs;
#   whether the user can reach the staging or get around the masks;
#   whether bindfs keeps the apt cache read-only for the user and usable for apt.
# Always exits 0 so the VM powers off normally.

OUT=/tmp/q4probe-result.txt
: >"$OUT"
r() { printf '%s\n' "$*" | tee -a "$OUT"; }

# try LABEL UID COMMAND (uid 0 = root)
try() {
    local label=$1 uid=$2 cmd=$3 s
    if [ "$uid" = 0 ]; then
        sh -c "$cmd" >/tmp/q4probe.out 2>&1
    else
        setpriv --reuid="$uid" --regid="$uid" --clear-groups -- sh -c "$cmd" >/tmp/q4probe.out 2>&1
    fi
    s=$?
    r "P $label uid=$uid status=$s out=[$(tr '\n' ' ' </tmp/q4probe.out | cut -c1-220)]"
}
drop() { sync; echo 3 >/proc/sys/vm/drop_caches; }

r "P-BEGIN kernel=$(uname -r) $(mount --version | head -n1)"

# --- Layout ---------------------------------------------------------------
# Find the vibe staging (current code mounts the tag at /mnt/shared).
OLD=$(findmnt -n -t virtiofs -o TARGET,FSROOT | awk '$2 == "/" {print $1; exit}')
TAG=$(findmnt -n -t virtiofs -o SOURCE,FSROOT | awk '$2 == "/" {print $1; exit}')
r "P old-staging=$OLD tag=$TAG"

# User's proposal: staging under /mnt/root (0700, root only).
mkdir -p /mnt/root/virtiofs-shared && chmod -R 700 /mnt/root
S=/mnt/root/virtiofs-shared
mount -t virtiofs "$TAG" "$S"
r "P staging-mount status=$? perms=$(stat -c '%a %u:%g' /mnt/root) share-root=$(stat -c '%a' "$S")"

sub() { # sub NAME -> staging subdir whose probe-id is NAME
    local d
    for d in "$S"/*; do
        [ "$(cat "$d/probe-id" 2>/dev/null)" = "$1" ] && { echo "$d"; return; }
    done
}
# The current code already gives a project share's .git (`--git ro`) and
# node_modules their own shares; reuse those tags.
SP=$(sub proj) SA=$(sub apt) SR=$(sub ro) SO=$(sub out)
SG=$S$(findmnt -n -o FSROOT /srv/proj/.git) SN=$S$(findmnt -n -o FSROOT /srv/proj/node_modules)
r "P subdirs proj=$SP git=$SG nm=$SN apt=$SA ro=$SR out=$SO staging=[$(ls "$S" | tr '\n' ' ')]"

# Remove the current vibe paths so only the new layout is reachable.
for m in /srv/proj /srv/apt /srv/ro /srv/out; do umount -R "$m"; done
umount -R "$OLD" && rmdir "$OLD"
r "P old-paths-removed virtiofs-mounts=[$(findmnt -n -t virtiofs -o TARGET | tr '\n' ' ')]"

useradd --create-home --shell /bin/bash --user-group dev
U=$(id -u dev)
r "P user dev uid=$U gid=$(id -g dev) _apt=$(id -u _apt)"

P=/home/dev/proj
install -d -o dev -g dev "$P"
mount --bind "$SP" "$P"
T=$(mktemp) && mount --bind "$T" "$P/.env"
mount -t tmpfs tmpfs "$P/.vibe"
mount --bind "$SG" "$P/.git"
mount --bind "$SN" "$P/node_modules"
mount --bind "$SR" /srv/ro
# Second bind of the project outside the 0700 HOME, to compare other UIDs.
mkdir -p /srv/pub && mount --bind "$SP" /srv/pub
r "P layout home=$(stat -c '%a' /home/dev) mounts=[$(findmnt -n -R -o TARGET,FSTYPE "$P" | tr -s ' ' | tr '\n' ';')]"
r "P layout tmpfile=$T $(stat -c '%a %U' "$T") .vibe=$(stat -c '%a' "$P/.vibe")"

# --- R1 / R2: writes through the bind mount, by UID ------------------------
drop
try "R-stat-first-root" 0 "stat -c '%n %u:%g %a' $P/d755 $P/d700"
try "R-create-d755-in-root-cache-window" "$U" "echo x > $P/d755/u$U && stat -c '%u:%g' $P/d755/u$U"
try "R-create-d700-in-root-cache-window" "$U" "echo x > $P/d700/u$U && cat $P/d700/inner"
try "R-home-other-uid" 502 "ls $P"
for uid in "$U" 502 1001; do
    drop
    try "R-stat" "$uid" "stat -c '%n %u:%g %a' /srv/pub/f644 /srv/pub/d700 /srv/ro/ro600"
    try "R-append-f644" "$uid" "echo $uid >> /srv/pub/f644"
    try "R-create-d700" "$uid" "echo x > /srv/pub/d700/u$uid && cat /srv/pub/d700/inner"
    try "R-mkdir-rm" "$uid" "mkdir /srv/pub/m$uid && rmdir /srv/pub/m$uid"
    try "R-create-d555" "$uid" "echo x > /srv/pub/d555/u$uid"
    try "R-read-ro600" "$uid" "cat /srv/ro/ro600"
    try "R-write-ro" "$uid" "echo x > /srv/ro/u$uid"
done
try "R-create-d555" 0 "echo x > $P/d555/root"

# --- R3: idmapped bind mounts -----------------------------------------------
mkdir -p /mnt/idm /mnt/idt /mnt/idt2
try "R3-idmap-virtiofs" 0 "mount --bind -o X-mount.idmap=b:0:$U:1 $SP /mnt/idm && stat -c '%u:%g' /mnt/idm/f644"
umount /mnt/idm 2>/dev/null
mount -t tmpfs tmpfs /mnt/idt && touch /mnt/idt/f
try "R3-idmap-tmpfs-control" 0 "mount --bind -o X-mount.idmap=b:0:$U:1 /mnt/idt /mnt/idt2 && stat -c '%u:%g' /mnt/idt2/f"
umount /mnt/idt2 /mnt/idt 2>/dev/null

# --- Staging bypass (user point 2) -----------------------------------------
try "B-ls-staging" "$U" "ls $S"
try "B-stat-staging-sub" "$U" "stat -c %n $SP/.env"
try "B-proc-1-root" "$U" "ls /proc/1/root$S"
try "B-symlink" "$U" "ln -s $SP/.env $P/lnk && cat $P/lnk; rm -f $P/lnk"
try "B-mountinfo-visible" "$U" "grep -c virtiofs /proc/self/mountinfo"
try "B-userns-ls-staging" "$U" "unshare -Urm sh -c 'ls $S'"
try "B-userns-mount-virtiofs" "$U" "mkdir -p /tmp/v && unshare -Urm sh -c 'mount -t virtiofs $TAG /tmp/v && ls /tmp/v'"
try "B-userns-umount-staging" "$U" "unshare -Urm sh -c 'umount $S; ls $S'"
try "B-dotdot" "$U" "ls -a $P/.. | tr '\n' ' '"

# --- Mask bypass (user point 4) --------------------------------------------
try "M-read-env" "$U" "cat $P/.env"
try "M-write-env" "$U" "echo y > $P/.env"
try "M-rm-env" "$U" "rm -f $P/.env"
try "M-mv-env" "$U" "mv $P/.env $P/env2"
try "M-umount-env" "$U" "umount $P/.env"
try "M-umount-l-git" "$U" "umount -l $P/.git"
try "M-fusermount-git" "$U" "fusermount3 -u $P/.git"
try "M-userns-umount-env" "$U" "unshare -Urm sh -c 'umount $P/.env; cat $P/.env'"
try "M-userns-bind-nonrec" "$U" "mkdir -p /tmp/b && unshare -Urm sh -c 'mount --bind $P /tmp/b && cat /tmp/b/.env'"
try "M-userns-rbind" "$U" "mkdir -p /tmp/b && unshare -Urm sh -c 'mount --rbind $P /tmp/b && cat /tmp/b/.env; ls /tmp/b/.vibe'"
try "M-write-git" "$U" "echo x > $P/.git/u$U"
try "M-vibe-tmpfs" "$U" "ls -a $P/.vibe | tr '\n' ' '; echo x > $P/.vibe/u$U"
try "M-node-modules-write" "$U" "echo x > $P/node_modules/u$U && cat $P/node_modules/probe-id"
try "M-rm-tmpfile" "$U" "rm -f $T"
# APFS is usually case-insensitive: other spellings may reach the masked host
# entries through the read-write project share.
try "M-case-ENV-read" "$U" "cat $P/.ENV"
try "M-case-Env-read" 0 "cat $P/.Env"
try "M-case-GIT-read" "$U" "head -c 40 $P/.GIT/HEAD"
try "M-case-GIT-write" "$U" "echo x > $P/.GIT/case-u$U && ls $P/.GIT | tr '\n' ' '"
try "M-case-Git-write" "$U" "echo x > $P/.Git/case2-u$U"
try "M-case-Git-config" "$U" "printf '[core]\\n\\tfsmonitor = /tmp/x\\n' >> $P/.Git/config && tail -n 2 $P/.git/config"
try "M-case-VIBE-read" "$U" "cat $P/.VIBE/state"
try "M-case-NODE-read" "$U" "cat $P/NODE_MODULES/m"
try "M-case-hardlink" "$U" "ln $P/.ENV $P/leak && cat $P/leak; rm -f $P/leak"
try "M-case-after-drop" 0 "sync; echo 3 > /proc/sys/vm/drop_caches; setpriv --reuid=$U --regid=$U --clear-groups -- sh -c 'cat $P/.ENV; ls $P/.GIT | head -n 3 | tr \"\\n\" \" \"; ls -a $P | tr \"\\n\" \" \"'"

# --- bindfs for the apt cache (user point 5) -------------------------------
if ! command -v bindfs >/dev/null; then
    try "A-install-bindfs" 0 "DEBIAN_FRONTEND=noninteractive apt-get install -y -q bindfs >/dev/null 2>&1 || { apt-get update -q >/dev/null 2>&1 && DEBIAN_FRONTEND=noninteractive apt-get install -y -q bindfs >/dev/null; }; bindfs --version | head -n1"
fi
# The proposed command as written, then with -o nonempty (the image's own
# /var/cache/apt is not empty; bindfs 1.14 on libfuse2 refuses that otherwise).
try "A-bindfs-mount" 0 "bindfs -u $(id -u _apt) -g 0 -p 0755,af-x $SA /var/cache/apt/"
mountpoint -q /var/cache/apt || try "A-bindfs-mount-nonempty" 0 "bindfs -o nonempty -u $(id -u _apt) -g 0 -p 0755,af-x $SA /var/cache/apt/"
try "A-mount-info" 0 "findmnt -n -o FSTYPE,OPTIONS /var/cache/apt; ps -o user=,args= -C bindfs; grep -c user_allow_other /etc/fuse.conf"
try "A-stat" "$U" "stat -c '%n %U:%G %a' /var/cache/apt /var/cache/apt/archives /var/cache/apt/archives/partial /var/cache/apt/archives/fake.deb"
try "A-user-read" "$U" "cat /var/cache/apt/archives/fake.deb"
try "A-user-write" "$U" "echo evil > /var/cache/apt/archives/fake.deb"
try "A-user-create" "$U" "echo evil > /var/cache/apt/archives/evil.deb"
try "A-user-rm" "$U" "rm -f /var/cache/apt/archives/fake.deb"
try "A-user-chmod" "$U" "chmod 666 /var/cache/apt/archives/fake.deb"
try "A-user-umount" "$U" "fusermount3 -u /var/cache/apt"
try "A-user-umount2" "$U" "umount /var/cache/apt"
try "A-user-kill-bindfs" "$U" "kill \$(pidof bindfs)"
try "A-user-symlink-swap" "$U" "ln -s /etc/shadow /var/cache/apt/archives/x.deb"
try "A-_apt-write-partial" "$(id -u _apt)" "echo x > /var/cache/apt/archives/partial/t && rm /var/cache/apt/archives/partial/t"
try "A-root-gencaches" 0 "apt-cache gencaches && ls /var/cache/apt | tr '\n' ' '"
try "A-root-download" 0 "apt-get install -y -q --download-only --reinstall bindfs >/dev/null 2>&1; echo rc=\$?; ls /var/cache/apt/archives | tr '\n' ' '"
try "A-root-reinstall-from-cache" 0 "apt-get install -y -q --reinstall --no-download bindfs >/dev/null 2>&1; echo rc=\$?"
try "A-root-exec-bit" 0 "stat -c '%a' /var/cache/apt/archives/*.deb | sort -u | tr '\n' ' '"
try "A-staging-view" 0 "stat -c '%n %a' $SA/archives/fake.deb"

r "P-END"
cp "$OUT" "$SO/q4probe-result.txt"
exit 0
