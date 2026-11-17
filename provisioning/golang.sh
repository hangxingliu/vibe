#!/bin/bash
# Install Go language.
# vibe:run-as-user
set -euxo pipefail

# shellcheck source=/dev/null
source "${VIBE_PROVISION_INCLUDE:?run this script with vibe provision}"

exec_retry mise use -g go@latest
