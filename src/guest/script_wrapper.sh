#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2016 # inner sh -c and bash -c strings expand their own $0 and $1
# Runs one Script or UserScript action. Evaluated in the current shell, so
# it reactivates mise there and prints the status line on its terminal.
# Do not enable set -e and do not exit: both would change the login shell.
#
# VIBE_VM_USER, VIBE_VM_UID, VIBE_VM_GID, and VIBE_VM_HOME are set by the
# root shell before a vm-user action. Rust does not replace
# __vibe_script_dir_base; host tests do.

# region INJECT_SCRIPT_WRAPPER_CONFIG
__vibe_script_id=""
__vibe_script_src=""
__vibe_script_b64=""
__vibe_as_vm_user=0
__vibe_script_dir_base="/tmp/vibe-scripts"
# endregion INJECT_SCRIPT_WRAPPER_CONFIG

# Prints the script: from the session share, or from the inline base64.
__vibe_script_text() {
  if [ -n "$__vibe_script_src" ]; then
    cat -- "$__vibe_script_src"
  else
    printf '%s' "$__vibe_script_b64" | base64 --decode
  fi
}

if [ "$(id -u)" = 0 ]; then
  free -h
  echo 3 >/proc/sys/vm/drop_caches
  echo 1 >/proc/sys/vm/compact_memory
fi
if [ "$__vibe_as_vm_user" = 1 ]; then
  __vibe_script_dir="$__vibe_script_dir_base-$VIBE_VM_UID"
  __vibe_script="$__vibe_script_dir/$__vibe_script_id.sh"
  # root makes the directory; the user writes the script into it.
  if {
    { mkdir -m 700 "$__vibe_script_dir" 2>/dev/null && chown "$VIBE_VM_UID:$VIBE_VM_GID" "$__vibe_script_dir"; } ||
      { [ -d "$__vibe_script_dir" ] && [ ! -L "$__vibe_script_dir" ] && [ "$(stat -c %u "$__vibe_script_dir")" = "$VIBE_VM_UID" ]; }
  } && __vibe_script_text | runuser -u "$VIBE_VM_USER" -- sh -c 'cat >"$1" && chmod +x "$1"' sh "$__vibe_script"; then
    (cd "$VIBE_VM_HOME" && runuser -u "$VIBE_VM_USER" -- env -u MISE_SHELL -u __MISE_DIFF -u __MISE_ORIG_PATH -u __MISE_SESSION HOME="$VIBE_VM_HOME" USER="$VIBE_VM_USER" LOGNAME="$VIBE_VM_USER" SHELL=/bin/bash bash --login -c 'exec "$0"' "$__vibe_script") </dev/null
    __vibe_status=$?
  else
    echo "Vibe: cannot write the script into $__vibe_script_dir" >&2
    __vibe_status=1
  fi
else
  __vibe_script_dir="$__vibe_script_dir_base-$(id -u)"
  __vibe_script="$__vibe_script_dir/$__vibe_script_id.sh"
  if {
    mkdir -m 700 "$__vibe_script_dir" 2>/dev/null ||
      { [ -d "$__vibe_script_dir" ] && [ ! -L "$__vibe_script_dir" ] && [ -O "$__vibe_script_dir" ]; }
  } && __vibe_script_text >"$__vibe_script" && chmod +x "$__vibe_script"; then
    "$__vibe_script" </dev/null
    __vibe_status=$?
  else
    echo "Vibe: cannot write the script into $__vibe_script_dir" >&2
    __vibe_status=1
  fi
fi
if [ "$__vibe_status" -eq 0 ] && [ -x "$HOME/.local/bin/mise" ]; then
  eval "$("$HOME/.local/bin/mise" activate bash)"
fi
printf '\n%s%s\n%s\n' 'VIBE_SCRIPT_STATUS_' "$__vibe_script_id" "$__vibe_status"
unset -f __vibe_script_text
unset __vibe_script_id __vibe_script_src __vibe_script_b64 __vibe_as_vm_user __vibe_script_dir_base __vibe_script_dir __vibe_script __vibe_status
