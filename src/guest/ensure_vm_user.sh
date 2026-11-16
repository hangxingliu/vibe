#!/bin/sh
# Creates the Vibe --vm-user account during provision when it does not exist.
# Runs as root inside the guest. Usage: ensure_vm_user.sh <name>
#
# The account gets the Debian default UID, its own group, bash, and
# /home/<name>; it is not added to sudo or any other group. An existing
# account is left unchanged; Vibe checks it with check_vm_user.sh afterwards.
set -u

name=$1
if getent passwd "$name" >/dev/null; then
    exit 0
fi
# Keep useradd output off stdout: stdout carries the notice word of the check.
useradd --create-home --home-dir "/home/$name" --shell /bin/bash --user-group "$name" >&2
