#!/bin/bash
# Install OpenAI's Codex.
# vibe:run-as-user
set -euxo pipefail

# shellcheck source=/dev/null
source "${VIBE_PROVISION_INCLUDE:?run this script with vibe provision}"

exec_retry mise use -g "npm:@openai/codex@latest"
