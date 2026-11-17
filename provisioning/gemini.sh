#!/bin/bash
# Install Google's Gemini (antigravity).
# vibe:run-as-user
set -euxo pipefail

# Set this environment variable to prevent the Gemini CLI from failing to identify the sandbox command
# echo "export GEMINI_SANDBOX=false" >> .bashrc

# tool='    "npm:@google/gemini-cli" = "latest"'
# echo "$tool" >> .config/mise/config.toml

# mise install

# shellcheck source=/dev/null
source "${VIBE_PROVISION_INCLUDE:?run this script with vibe provision}"

exec_retry bash -o pipefail -c 'curl -fsSL https://antigravity.google/cli/install.sh | bash'
