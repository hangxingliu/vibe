#!/usr/bin/env bash
history -w # Write bash history. Otherwise bash would be killed by poweroff without having written history
set +o nounset;

DO_SHUTDOWN=true;
if [ -n "$SSH_CLIENT" ] || [ -n "$SSH_TTY" ] || [ -n "$TMUX" ]; then DO_SHUTDOWN=false;
elif tmux list-sessions &> /dev/null; then DO_SHUTDOWN=false;
fi

if "$DO_SHUTDOWN"; then
  systemctl poweroff
  sleep 100 # sleep here so that we don not see the login screen flash up before the shutdown.
fi
