#!/bin/zsh
set -eu
profile_dir=${0:A:h}
export PATH="/opt/homebrew/bin:/usr/local/bin:$HOME/.local/bin:$PATH"
source "$profile_dir/paths.zsh"
export PATH="${tmux_bin:h}:$PATH"
export ALACRITTY_TERMINAL_PROFILE=merino
# A private server does not inherit another terminal's theme or alter its panes.
if "$tmux_bin" -L alacritty-merino -N has-session -t '=alacritty' 2>/dev/null; then
  exec "$tmux_bin" -L alacritty-merino attach-session -t '=alacritty'
fi
if "$tmux_bin" -L alacritty-merino -N list-sessions >/dev/null 2>&1; then
  "$tmux_bin" -L alacritty-merino source-file "$profile_dir/tmux.conf"
fi
"$tmux_bin" -L alacritty-merino -f "$profile_dir/tmux.conf" new-session -d -s alacritty || \
  "$tmux_bin" -L alacritty-merino -N has-session -t '=alacritty'
exec "$tmux_bin" -L alacritty-merino attach-session -t '=alacritty'
