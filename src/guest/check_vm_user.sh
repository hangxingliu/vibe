#!/bin/sh
# Checks that an existing account can be used as the Vibe --vm-user.
# Runs as root inside the guest. Usage: check_vm_user.sh <name>
#
# Exit codes:
#   10  the account does not exist
#   11  the account has UID 0
#   12  the login shell is not bash
#   13  HOME is not an absolute path to an existing directory
#   14  HOME is not owned by the account
#   15  HOME is /root
#
# On success, prints one word on stdout when the account is in a group that
# usually grants root access, for example "groups=sudo,adm". Vibe reports it
# on the host as a warning.
set -u

name=$1
entry=$(getent passwd "$name") || exit 10
[ -n "$entry" ] || exit 10

uid=$(printf '%s\n' "$entry" | cut -d: -f3)
home=$(printf '%s\n' "$entry" | cut -d: -f6)
shell=$(printf '%s\n' "$entry" | cut -d: -f7)

[ "$uid" != 0 ] || exit 11

case $shell in
    */bash) ;;
    *) exit 12 ;;
esac

case $home in
    /*) ;;
    *) exit 13 ;;
esac
case $home in
    /root | /root/) exit 15 ;;
esac
[ -d "$home" ] || exit 13

owner=$(stat -c %u "$home") || exit 14
[ "$owner" = "$uid" ] || exit 14

privileged=
for group in $(id -nG "$name" 2>/dev/null); do
    case $group in
        root | sudo | adm | wheel | admin)
            privileged=${privileged:+$privileged,}$group
            ;;
    esac
done
if [ -n "$privileged" ]; then
    printf 'groups=%s\n' "$privileged"
fi
exit 0
