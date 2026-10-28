if (( _alacritty_user_zdotdir_set )); then
    builtin export ZDOTDIR=$_alacritty_user_zdotdir
else
    builtin unset ZDOTDIR
fi
[[ -r ${ZDOTDIR-$HOME}/.zprofile ]] && builtin source "${ZDOTDIR-$HOME}/.zprofile"
builtin typeset -g _alacritty_user_zdotdir=${ZDOTDIR-$HOME}
builtin typeset -g _alacritty_user_zdotdir_set=${+ZDOTDIR}
[[ -o rcs ]] && builtin export ZDOTDIR=$_alacritty_integration_dir
