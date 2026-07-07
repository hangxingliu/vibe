#!/bin/bash
# Install Rust with sccache.
# shellcheck disable=SC1091
set -euxo pipefail

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
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --component "rustfmt,clippy"
done

source "$HOME/.cargo/env"
exec_retry \
apt install -y --no-install-recommends sccache

# shellcheck disable=SC2016
# echo 'source "$HOME/.cargo/env"' >> .bashrc
# echo 'export RUSTC_WRAPPER=sccache' >> .bashrc
true;
