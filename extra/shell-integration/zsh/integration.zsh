# OSC 133 shell integration. May also be sourced manually from an interactive zsh.
# Hooks never evaluate command text and are not exported to child shells.
[[ -o interactive && -t 1 && -z ${_alacritty_shell_loaded-} ]] || return 0
# Multiplexers own their pane coordinates; do not label their whole terminal as one shell.
[[ -z ${TMUX-} && -z ${STY-} ]] || return 0
builtin typeset -g _alacritty_shell_loaded=1
builtin typeset -gi _alacritty_shell_running=0

_alacritty_shell_preexec() {
    builtin emulate -L zsh
    builtin print -rn -- $'\e]133;C\a'
    _alacritty_shell_running=1
}

_alacritty_shell_precmd() {
    builtin local command_status=$?
    builtin emulate -L zsh
    if (( _alacritty_shell_running )); then
        builtin print -rn -- $'\e]133;D;'"$command_status"$'\a'
        _alacritty_shell_running=0
    fi
    # Bracket the prompt in non-printing escapes, preserving its display width.
    # Restore our previous wrapper only if a theme has not replaced PS1 since then.
    if [[ ${_alacritty_marked_prompt-} == "$PS1" ]]; then
        PS1=$_alacritty_plain_prompt
    fi
    _alacritty_plain_prompt=$PS1
    if [[ -o promptpercent ]]; then
        # A trailing bare percent must not consume the opening brace of our marker.
        [[ $PS1 == *[^%]% || $PS1 == % ]] && PS1=$PS1%
        PS1=$'%{\e]133;A\a%}'"$PS1"$'%{\e]133;B\a%}'
        _alacritty_marked_prompt=$PS1
    else
        builtin print -rn -- $'\e]133;A\a'
    fi
    return $command_status
}

builtin typeset -ga preexec_functions precmd_functions
preexec_functions+=(_alacritty_shell_preexec)
precmd_functions+=(_alacritty_shell_precmd)
