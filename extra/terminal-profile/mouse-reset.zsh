# A TUI interrupted during teardown may leave mouse tracking enabled. Reset
# application mouse modes before returning to the shell prompt. Inside tmux,
# these sequences reset only this pane; tmux's own mouse handling stays on.
_terminal_reset_mouse_tracking() {
  [[ -o interactive && -t 1 ]] || return 0
  builtin printf '\033[?1000l\033[?1002l\033[?1003l\033[?1005l\033[?1006l\033[?1015l\033[?1016l'
}

autoload -Uz add-zsh-hook
add-zsh-hook -d precmd _terminal_reset_mouse_tracking
add-zsh-hook precmd _terminal_reset_mouse_tracking

# Mouse reports already queued during TUI shutdown can arrive after precmd.
# Consume complete SGR mouse reports in ZLE instead of inserting their numeric
# payload into the command line. Preserve ordinary text and incomplete reports.
_terminal_discard_sgr_mouse() {
  local char report=''
  integer remaining=48
  while (( remaining-- )) && read -r -k 1 -t 0.05 char; do
    case $char in
      [0-9\;]) report+=$char ;;
      M|m) return 0 ;;
      *) zle -U -- "${report}${char}"; return 0 ;;
    esac
  done
  [[ -n $report ]] && zle -U -- "$report"
  return 0
}

if [[ -o interactive ]]; then
  zle -N _terminal_discard_sgr_mouse
  bindkey -M emacs $'\e[<' _terminal_discard_sgr_mouse
  bindkey -M viins $'\e[<' _terminal_discard_sgr_mouse
  bindkey -M vicmd $'\e[<' _terminal_discard_sgr_mouse
fi
