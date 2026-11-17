#!/bin/bash
# Install Anthropic's Claude.
# vibe:run-as-user
set -euxo pipefail

# shellcheck source=/dev/null
source "${VIBE_PROVISION_INCLUDE:?run this script with vibe provision}"

# Set this env var so claude doesn't complain about running as root.
echo "export IS_SANDBOX=1" >>.bashrc

tool='    claude = "latest"'
echo "$tool" >>.config/mise/config.toml

exec_retry mise install
