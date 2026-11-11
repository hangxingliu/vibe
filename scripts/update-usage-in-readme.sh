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


# Run without the image directory variables so the maintainer's own setup
# neither changes the output nor makes `--help` fail.
get_stdout env -u VIBE_EXTRA_IMAGE_DIR -u VIBE_PREFERRED_IMAGE_DIR "$BIN" --help ||
  throw "Failed to run '$BIN --help'"
# Everything from `Cache directory:` on is host specific (cache directory and
# image directory lookup order) and must not be written into the README.
help_output="$("$awk_bin" '/^Cache directory:/ {exit} {print}' <<< "$get_stdout_result")"

# Guard against host paths moving above the `Cache directory:` marker.
host_paths=("/Volumes/")
[ -n "$HOME" ] && [ "$HOME" != "/" ] && host_paths+=("$HOME")
host_paths+=("${XDG_CACHE_HOME:-$HOME/.cache}/vibe")
IFS=: read -r -a extra_dirs <<< "${VIBE_EXTRA_IMAGE_DIR:-}"
for dir in "${extra_dirs[@]}" "${VIBE_PREFERRED_IMAGE_DIR:-}"; do
  [ -n "${dir//[[:space:]]/}" ] && host_paths+=("$dir")
done
for path in "${host_paths[@]}"; do
  [[ "$help_output" == *"$path"* ]] && throw "help text above 'Cache directory:' contains the host path '$path'"
done

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
