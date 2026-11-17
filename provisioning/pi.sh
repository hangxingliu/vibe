#!/bin/bash
# Install Earendil's Pi.
# vibe:run-as-user
set -euxo pipefail

# shellcheck source=/dev/null
source "${VIBE_PROVISION_INCLUDE:?run this script with vibe provision}"

exec_retry mise use -g "npm:@earendil-works/pi-coding-agent@latest"
