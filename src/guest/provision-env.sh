#!/bin/sh
# Exports provision settings into the root shell. The root shell sources
# this file. It is not a child process: do not enable set -e and do not exit.

# region INJECT_PROVISION_ENV_CONFIG
export DEBIAN_FRONTEND=noninteractive
export VIBE_PROVISION_IMAGE=''
export VIBE_PROVISION_BASE=''
export VIBE_GIT_SHA=''
export VIBE_BUILD_DATE=''
export VIBE_PROVISION_SCRIPTS=''
export VIBE_PROVISION_VM_USER=''
# endregion INJECT_PROVISION_ENV_CONFIG
