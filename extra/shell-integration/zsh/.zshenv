# Restore the user's startup directory before reading their configuration.
builtin typeset -g _alacritty_integration_dir=$ZDOTDIR
if [[ ${ALACRITTY_ZDOTDIR_SET-} == 1 ]]; then
    builtin export ZDOTDIR=$ALACRITTY_ZDOTDIR
else
    builtin unset ZDOTDIR
fi
builtin unset ALACRITTY_ZDOTDIR ALACRITTY_ZDOTDIR_SET
[[ -r ${ZDOTDIR-$HOME}/.zshenv ]] && builtin source "${ZDOTDIR-$HOME}/.zshenv"
if [[ -o interactive && -o rcs ]]; then
    builtin typeset -g _alacritty_user_zdotdir=${ZDOTDIR-$HOME}
    builtin typeset -g _alacritty_user_zdotdir_set=${+ZDOTDIR}
    builtin export ZDOTDIR=$_alacritty_integration_dir
fi
