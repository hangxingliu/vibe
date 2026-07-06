#!/usr/bin/env bash

BIN="target/release/vibe";
README_FILE="readme.md";

throw() { printf "fatal: %s\n" "$1" >&2; exit 1; }
print_cmd() { printf "\$ %s\n" "$*"; }
execute() { print_cmd "$@"; "$@" || throw "Failed to execute '$1'"; }
get_stdout() { print_cmd "$@"; get_stdout_result="$("$@")"; }

cd "$(dirname "${BASH_SOURCE[0]}")/.." || throw "Failed to cd"

# then update usage help in README.md
# locate `## Using Vibe` and the code block beginning sign
# then replace and save file back

awk_bin="$(command -v gawk)";
[ -z "$awk_bin" ] && awk_bin="$(command -v awk)";
[ -z "$awk_bin" ] && throw "awk is not found";


get_stdout "$BIN" --help
help_output="$("$awk_bin" '/^Cache directory:/ {exit} {print}' <<< "$get_stdout_result")"

# shellcheck disable=SC2016
awk_script='
BEGIN {
    in_usage = 0
    in_code_block = 0
    replaced = 0
}
/^## Using Vibe/ {
    print $0
    in_usage = 1
    next
}
in_usage && !replaced && /^```/ {
    if (in_code_block == 0) {
        print $0
        print help_output
        in_code_block = 1
    } else {
        print $0
        in_code_block = 0
        in_usage = 0
        replaced = 1
    }
    next
}
in_code_block {
    next
}
{
    print $0
}
';

"$awk_bin" -v help_output="$help_output" "$awk_script" "${README_FILE}" > "${README_FILE}.tmp"
execute mv "${README_FILE}.tmp" "${README_FILE}"
