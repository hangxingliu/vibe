#!/bin/bash
# Install Rust with sccache.
# shellcheck disable=SC1091
set -euxo pipefail

# shellcheck source=/dev/null
source "${VIBE_PROVISION_INCLUDE:?run this script with vibe provision}"

exec_retry \
  apt install -y --no-install-recommends sccache

# vibe:run-as-user
# rustup installs Rust for the vm-user (root by default).
set -euxo pipefail

# shellcheck source=/dev/null
source "${VIBE_PROVISION_INCLUDE:?run this script with vibe provision}"

exec_retry bash -o pipefail -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --component "rustfmt,clippy"'

source "$HOME/.cargo/env"

# shellcheck disable=SC2016
# echo 'source "$HOME/.cargo/env"' >> .bashrc
# echo 'export RUSTC_WRAPPER=sccache' >> .bashrc
true
