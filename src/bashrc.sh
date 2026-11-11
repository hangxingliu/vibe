#!/usr/bin/env bash
# shellcheck disable=SC2163

#
# region Colors and Aliases
#
export LS_OPTIONS='--color=auto'
eval "$(dircolors)"
alias ls='ls $LS_OPTIONS'
alias ll='ls $LS_OPTIONS -lA'
alias l='ls $LS_OPTIONS -lA'

# Enable true color support in the terminal
export COLORTERM=truecolor

# Hide commands beginning with space from the history
export HISTCONTROL=ignorespace

# Unlimited bash history
export HISTFILESIZE=
export HISTSIZE=
# endregion Colors and Aliases
#

#
# region PATH
#
export PATH="${HOME}/.cargo/bin:${HOME}/.bun/bin:${HOME}/.local/bin:${PATH}"
[ -f "${HOME}/.local/bin/mise" ] &&
    eval "$("${HOME}/.local/bin/mise" activate bash)"
#
# endregion PATH
#

#
# region Configure Agents
#
export DO_NOT_TRACK=true

#
# [Claude Code]
# `IS_SANDBOX` is an environment variable used to indicate or signal that Claude Code is
# running inside a sandboxed environment.
export IS_SANDBOX=1
# Disable telemetry services
# https://code.claude.com/docs/en/data-usage
export CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1
export CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1
export DISABLE_TELEMETRY=1
export DISABLE_ERROR_REPORTING=1
export DISABLE_FEEDBACK_COMMAND=1


# [Gemini][Antigravity]
# Disable Sandboxing in Gemini cli
export GEMINI_SANDBOX=false
export GEMINI_TELEMETRY_ENABLED=false
export FIREBASE_CLI_TELEMETRY=0

# [Github Copilot]
export COPILOT_ALLOW_ALL=true
export GH_TELEMETRY=false
# copilot --disable-client-telemetry

# [Grok Build]
# https://docs.x.ai/build/settings/reference
export GROK_TELEMETRY_ENABLED=0
export GROK_WEB_FETCH=1
export GROK_SUBAGENTS=1
# endregion Configure Agents
#


# Disable flow control only in interactive sessions
#   Preventing Terminal "Freezes" caused by `Ctrl + S`
#   The Fix: Normally, you have to press `Ctrl + Q` (which sends an `XON` signal)
[[ $- == *i* ]] && stty -ixon
true
