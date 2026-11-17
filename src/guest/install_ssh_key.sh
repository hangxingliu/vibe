#!/bin/sh
# Installs the --ssh-key public key as the only authorized key of the user
# that runs this script. $1 is that user's HOME.

# region INJECT_INSTALL_SSH_KEY_CONFIG
SSH_PUBLIC_KEY=""
# endregion INJECT_INSTALL_SSH_KEY_CONFIG

set -eu
target_home=${1:?target HOME is missing}
: "${SSH_PUBLIC_KEY:?SSH_PUBLIC_KEY is empty}"
umask 077
mkdir -p "$target_home/.ssh"
chmod 700 "$target_home/.ssh"
printf '%s\n' "$SSH_PUBLIC_KEY" >"$target_home/.ssh/authorized_keys"
chmod 600 "$target_home/.ssh/authorized_keys"
