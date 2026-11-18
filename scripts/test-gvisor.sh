#!/usr/bin/env bash

TEST_DIRS=(
  ./pkg/services/noproxy/
  ./pkg/services/netproxy/
  ./pkg/services/dns/
  ./pkg/services/forwarder/
)

throw() { printf "fatal: %s\n" "$1" >&2; exit 1; }
print_cmd() { printf "\$ %s\n" "$*"; }
execute() { print_cmd "$@"; "$@" || throw "Failed to execute '$1'"; }
execute_silently() { print_cmd "$@"; "$@" >/dev/null || throw "Failed to execute '$1'"; }
get_stdout() { print_cmd "$@"; get_stdout_result="$("$@")"; }

pushd "$( dirname -- "${BASH_SOURCE[0]}" )" >/dev/null || exit 1;

execute cd ../helpers/gvisor-tap-vsock
execute go test -count=1  "${TEST_DIRS[@]}"
execute go build -o bin/gvproxy ./cmd/gvproxy
