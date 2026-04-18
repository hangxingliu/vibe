#!/usr/bin/env bash
# shellcheck disable=SC2163

export LS_OPTIONS='--color=auto'
eval "$(dircolors)"
alias ls='ls $LS_OPTIONS'
alias ll='ls $LS_OPTIONS -lA'
alias l='ls $LS_OPTIONS -lA'

# Set this env var so claude doesn't complain about running as root.'
export IS_SANDBOX=1

# Set this environment variable to prevent the Gemini CLI from failing to identify the sandbox command
export GEMINI_SANDBOX=false

export COPILOT_ALLOW_ALL=true

# Enable true color support in the terminal
export COLORTERM=truecolor

# Hide commands beginning with space from the history
export HISTCONTROL=ignorespace

# Unlimited bash history
export HISTFILESIZE=
export HISTSIZE=

export PATH="${HOME}/.cargo/bin:${HOME}/.local/bin:${PATH}";
[ -f "${HOME}/.local/bin/mise" ] &&
    eval "$("${HOME}/.local/bin/mise" activate bash)";

# stty -ixon

function http-proxy-export() {
    local cmds cmd url="$1" noproxy='127.0.0.1,localhost,192.168.*';
    if [ -z "$url" ]; then
        cmds=( "http_proxy" "https_proxy" "no_proxy""HTTP_PROXY" "HTTPS_PROXY" "NO_PROXY" );
        for cmd in "${cmds[@]}"; do
            printf "\$ unset %s\n" "${cmd}";
            unset "${cmd}";
        done
    else
        url="http://${url#*://}"
        cmds=(
            "http_proxy=${url}" "https_proxy=${url}" "no_proxy=${noproxy}"
            "HTTP_PROXY=${url}" "HTTPS_PROXY=${url}" "NO_PROXY=${noproxy}"
        );
        for cmd in "${cmds[@]}"; do
            printf "\$ export %s\n" "${cmd}";
            export "${cmd}";
        done
    fi
}
