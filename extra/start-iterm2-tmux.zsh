#!/bin/zsh
# Reattach immediately when the session already exists. Configuration changes
# can be applied with prefix-r; reconnecting must not rebuild all key tables.
# Use this executable as terminal.shell (a string) so imported args cannot append.
set -eu
export PATH="$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
tmux_bin="$HOME/.local/bin/tmux"
config_file="$HOME/.config/alacritty/tmux.conf"

if "$tmux_bin" -L default -N has-session -t '=alacritty' 2>/dev/null; then
  exec "$tmux_bin" -L default attach-session -t '=alacritty'
fi

# -f loads the configuration when starting a server, but is ignored by an
# already running server. Only that latter case needs an explicit source-file.
if "$tmux_bin" -L default -N list-sessions >/dev/null 2>&1; then
  "$tmux_bin" -L default source-file "$config_file"
fi
"$tmux_bin" -L default -f "$config_file" new-session -d -s alacritty || \
  "$tmux_bin" -L default -N has-session -t '=alacritty'
exec "$tmux_bin" -L default attach-session -t '=alacritty'
