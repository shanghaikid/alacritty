#!/bin/zsh
# Keep the existing Alacritty session on the default socket, including its panes.
# Use this executable as terminal.shell (a string) so imported args cannot append.
set -eu
export PATH="$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
tmux_bin="$HOME/.local/bin/tmux"
config_file="$HOME/.config/alacritty/tmux.conf"

if ! "$tmux_bin" -L default has-session -t alacritty 2>/dev/null; then
  "$tmux_bin" -L default -f "$config_file" new-session -d -s alacritty || \
    "$tmux_bin" -L default has-session -t alacritty
fi
"$tmux_bin" -L default source-file "$config_file"
exec "$tmux_bin" -L default attach-session -t alacritty
