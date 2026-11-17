#!/bin/bash
# Install Grok Build
# vibe:run-as-user
set -euxo pipefail

# shellcheck source=/dev/null
source "${VIBE_PROVISION_INCLUDE:?run this script with vibe provision}"

exec_retry bash -o pipefail -c 'curl -fsSL https://x.ai/cli/install.sh | bash'
