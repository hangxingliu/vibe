#!/bin/sh
# Installs bindfs during provision when the image does not have it.
# Runs as root inside the guest.

# region INJECT_INSTALL_BINDFS_CONFIG
APT_PROXY_URL=""
# endregion INJECT_INSTALL_BINDFS_CONFIG

set -eu
command -v bindfs >/dev/null && exit 0
export DEBIAN_FRONTEND=noninteractive
if [ -n "$APT_PROXY_URL" ]; then
  set -- -o "Acquire::http::Proxy=$APT_PROXY_URL" -o "Acquire::https::Proxy=$APT_PROXY_URL"
else
  set --
fi
apt-get "$@" install --yes bindfs </dev/null && exit 0
apt-get "$@" update </dev/null
apt-get "$@" install --yes bindfs </dev/null
