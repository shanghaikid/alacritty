# Applied after the user's startup configuration, only in managed Alacritty panes.
[[ -o interactive && ${ALACRITTY_TERMINAL_PROFILE:-} == merino ]] || return 0
autoload -Uz vcs_info add-zsh-hook
zstyle ':vcs_info:git:*' formats ' %F{yellow}⎇ %b%f'
_alacritty_merino_prompt() { vcs_info }
add-zsh-hook -d precmd _alacritty_merino_prompt
add-zsh-hook precmd _alacritty_merino_prompt
setopt prompt_subst
PROMPT='%F{blue}%~%f${vcs_info_msg_0_} %(?.%F{green}❯.%F{red}❯)%f '
RPROMPT='%F{8}%*%f'
source "${${(%):-%N}:A:h}/mouse-reset.zsh"
