if (( _alacritty_user_zdotdir_set )); then
    builtin export ZDOTDIR=$_alacritty_user_zdotdir
else
    builtin unset ZDOTDIR
fi
[[ -r ${ZDOTDIR-$HOME}/.zshrc ]] && builtin source "${ZDOTDIR-$HOME}/.zshrc"
builtin source "$_alacritty_integration_dir/integration.zsh"
builtin unset _alacritty_integration_dir _alacritty_user_zdotdir _alacritty_user_zdotdir_set
