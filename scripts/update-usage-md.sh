#!/usr/bin/env bash
# Copy plain `vibe --help` into USAGE.md and USAGE.zh-Hans.md.
# The host-specific tail starts at "Cache directory:" and stays out of the docs.

set -euo pipefail

die() {
  printf 'fatal: %s\n' "$1" >&2
  exit 1
}

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)" || die "failed to enter the repo root"
bin="${root}/target/release/vibe"

[[ -x "${bin}" ]] || die "missing ${bin}; run: cargo build --release --locked"

# Unset image-dir variables so a local setup cannot change the text or fail --help.
# Captured stdout is not a terminal, so the text has no color and no links.
raw="$(env -u VIBE_EXTRA_IMAGE_DIR -u VIBE_PREFERRED_IMAGE_DIR "${bin}" --help)" \
  || die "failed to run ${bin} --help"

help="$(
  printf '%s\n' "${raw}" | LC_ALL=C awk '
    /^Cache directory:/ { seen = 1; exit }
    { lines[++n] = $0 }
    END {
      if (!seen) {
        print "help has no \"Cache directory:\" marker" > "/dev/stderr"
        exit 2
      }
      while (n > 0 && lines[n] ~ /^[[:space:]]*$/) {
        n--
      }
      for (i = 1; i <= n; i++) {
        print lines[i]
      }
    }
  '
)" || die "failed to trim help text"

[[ -n "${help}" ]] || die "help text is empty"
[[ "${help}" != *$'\033'* ]] || die "help text contains ANSI escapes"

# Paths below belong to this machine. They must not appear above the marker.
# Guard empty arrays: bash 3.2 with `set -u` rejects "${empty[@]}".
paths=("/Volumes/" "${XDG_CACHE_HOME:-${HOME:-}/.cache}/vibe")
if [[ -n "${HOME:-}" && "${HOME}" != "/" ]]; then
  paths+=("${HOME}")
fi
extra=()
if [[ -n "${VIBE_EXTRA_IMAGE_DIR:-}" ]]; then
  IFS=':' read -r -a extra <<< "${VIBE_EXTRA_IMAGE_DIR}"
fi
if [[ ${#extra[@]} -gt 0 ]]; then
  for path in "${extra[@]}"; do
    if [[ -n "${path//[[:space:]]/}" ]]; then
      paths+=("${path}")
    fi
  done
fi
preferred="${VIBE_PREFERRED_IMAGE_DIR:-}"
if [[ -n "${preferred//[[:space:]]/}" ]]; then
  paths+=("${preferred}")
fi
for path in "${paths[@]}"; do
  if [[ "${help}" == *"${path}"* ]]; then
    die "help text above 'Cache directory:' contains the host path '${path}'"
  fi
done

# Replace the first fenced block after the heading. Both files quote the same English help.
update_file() {
  local file="$1"
  local heading="$2"
  local tmp
  tmp="$(mktemp)"
  # macOS awk treats unequal multibyte strings as equal in a UTF-8 locale
  # (`## 命令` matches `## 帮助输出`). Byte comparison avoids that.
  if ! HELP_TEXT="${help}" LC_ALL=C awk -v heading="${heading}" '
    $0 == heading { in_section = 1 }
    in_section && !replaced && $0 == "```" {
      print
      if (!in_fence) {
        printf "%s\n", ENVIRON["HELP_TEXT"]
        in_fence = 1
      } else {
        in_fence = 0
        replaced = 1
        in_section = 0
      }
      next
    }
    in_fence { next }
    { print }
    END {
      if (!replaced) {
        print "no help fence after: " heading > "/dev/stderr"
        exit 2
      }
    }
  ' "${file}" > "${tmp}"; then
    rm -f "${tmp}"
    die "failed to update ${file}"
  fi
  mv "${tmp}" "${file}"
  printf 'updated %s\n' "${file}"
}

update_file "${root}/USAGE.md" "## Help output"
update_file "${root}/USAGE.zh-Hans.md" "## 帮助输出"
