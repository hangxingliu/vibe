#!/bin/bash
# Install Go language.
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

exec_retry mise use -g go@latest
