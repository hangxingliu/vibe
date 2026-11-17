#!/usr/bin/env bash
# shellcheck shell=bash
# Sourced by a provision script. Do not enable set -e here and do not exit
# at the top level: this file only defines functions.
#
# exec_retry runs an external command, then retries up to 3 times after
# 10 seconds, then exits 1. The `||` around the command disables errexit
# inside a shell function it calls. Pass external commands only. For a
# pipeline, call `exec_retry bash -o pipefail -c '...'`.

exec_retry() {
  local attempt=0 status
  while :; do
    status=0
    "$@" || status=$?
    [ "$status" -eq 0 ] && return 0
    attempt=$((attempt + 1))
    if [ "$attempt" -gt 3 ]; then
      echo "Error: too many errors and retry attempts, exiting ..." >&2
      exit 1
    fi
    echo "Warn:  waiting 10s and try again (error=${attempt}) ..." >&2
    sleep 10
  done
}
