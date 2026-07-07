#!/bin/bash
# Install Google's Gemini (antigravity).
set -euxo pipefail

# Set this environment variable to prevent the Gemini CLI from failing to identify the sandbox command
# echo "export GEMINI_SANDBOX=false" >> .bashrc

# tool='    "npm:@google/gemini-cli" = "latest"'
# echo "$tool" >> .config/mise/config.toml

# mise install

retry_error=
retryable() {
  if [ -z "$retry_error" ]; then retry_error=0; return 0; fi
  if [ "$1" == "0" ]; then retry_error=; return 1; fi

  retry_error="$((retry_error+1))";
  if [[ "$retry_error" -le 3 ]]; then
    echo "Warn:  waiting 10s and try again (error=${retry_error}) ..." >&2;
    sleep 10;
    return 0;
  fi
  echo "Error: too many errors and retry attempts, exiting ..." >&2;
  exit 1;
}
exec_retry() {
  while retryable "$?"; do "${@}"; done
}

while retryable "$?"; do
curl -fsSL https://antigravity.google/cli/install.sh | bash
done
