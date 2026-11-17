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
#
# When include.sh sits beside a staged script, this wrapper copies it into
# the same private directory. It sets VIBE_PROVISION_INCLUDE on the script
# process only. An inline script has no source path, so the variable stays
# unset. A failed copy does not run the script.

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

# Copies include.sh beside the script copy when the session directory has one.
# Sets __vibe_include to that copy, or leaves it empty. Returns 1 on failure.
__vibe_copy_include() {
  __vibe_include=""
  if [ -z "$__vibe_script_src" ]; then
    return 0
  fi
  local __vibe_include_src="${__vibe_script_src%/*}/include.sh"
  if [ ! -f "$__vibe_include_src" ]; then
    return 0
  fi
  if [ "$__vibe_as_vm_user" = 1 ]; then
    # Root reads the session file. The vm-user writes the copy: the directory
    # is mode 0700 and owned by that user.
    cat -- "$__vibe_include_src" | runuser -u "$VIBE_VM_USER" -- sh -c 'cat >"$1"' sh "$__vibe_script_dir/include.sh" || return 1
  else
    cat -- "$__vibe_include_src" >"$__vibe_script_dir/include.sh" || return 1
  fi
  __vibe_include="$__vibe_script_dir/include.sh"
}

# Runs the copied script. The include variable is set on that process only.
__vibe_run_script() {
  if [ "$__vibe_as_vm_user" = 1 ]; then
    if [ -n "$__vibe_include" ]; then
      (cd "$VIBE_VM_HOME" && runuser -u "$VIBE_VM_USER" -- env -u MISE_SHELL -u __MISE_DIFF -u __MISE_ORIG_PATH -u __MISE_SESSION VIBE_PROVISION_INCLUDE="$__vibe_include" HOME="$VIBE_VM_HOME" USER="$VIBE_VM_USER" LOGNAME="$VIBE_VM_USER" SHELL=/bin/bash bash --login -c 'exec "$0"' "$__vibe_script") </dev/null
    else
      (cd "$VIBE_VM_HOME" && runuser -u "$VIBE_VM_USER" -- env -u MISE_SHELL -u __MISE_DIFF -u __MISE_ORIG_PATH -u __MISE_SESSION HOME="$VIBE_VM_HOME" USER="$VIBE_VM_USER" LOGNAME="$VIBE_VM_USER" SHELL=/bin/bash bash --login -c 'exec "$0"' "$__vibe_script") </dev/null
    fi
  elif [ -n "$__vibe_include" ]; then
    VIBE_PROVISION_INCLUDE="$__vibe_include" "$__vibe_script" </dev/null
  else
    "$__vibe_script" </dev/null
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
    if __vibe_copy_include; then
      __vibe_run_script
      __vibe_status=$?
    else
      echo "Vibe: cannot write include.sh into $__vibe_script_dir" >&2
      __vibe_status=1
    fi
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
    if __vibe_copy_include; then
      __vibe_run_script
      __vibe_status=$?
    else
      echo "Vibe: cannot write include.sh into $__vibe_script_dir" >&2
      __vibe_status=1
    fi
  else
    echo "Vibe: cannot write the script into $__vibe_script_dir" >&2
    __vibe_status=1
  fi
fi
if [ "$__vibe_status" -eq 0 ] && [ -x "$HOME/.local/bin/mise" ]; then
  eval "$("$HOME/.local/bin/mise" activate bash)"
fi
printf '\n%s%s\n%s\n' 'VIBE_SCRIPT_STATUS_' "$__vibe_script_id" "$__vibe_status"
unset -f __vibe_script_text __vibe_copy_include __vibe_run_script
unset __vibe_script_id __vibe_script_src __vibe_script_b64 __vibe_as_vm_user __vibe_script_dir_base __vibe_script_dir __vibe_script __vibe_status __vibe_include
