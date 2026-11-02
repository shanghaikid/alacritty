#!/usr/bin/env python3
"""Add native theme switching to an existing dotfiles-based Alacritty setup."""

import argparse
import importlib.util
import json
from pathlib import Path
import shutil
import sys

EXTRA = Path(__file__).resolve().parent
sys.path.insert(0, str(EXTRA / 'terminal-profile'))
import theme

spec = importlib.util.spec_from_file_location('terminal_install', EXTRA / 'terminal-profile/install.py')
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)
MARKER = '# Alacritty Merino theme overlay'


def install(home, tmux):
    config = home / '.config/alacritty'
    main = config / 'alacritty.toml'
    base = config / 'alacritty-before-theme.toml'
    directory = config / 'terminal-theme'
    original = main.read_text()
    managed = original.startswith(MARKER)
    if not managed and (base.exists() or base.is_symlink()):
        raise ValueError('Existing ' + str(base) + '; refusing to overwrite the saved configuration')
    writer = installer.Writer(home)
    # Prepare the theme source beside its module without changing the checkout.
    import tempfile
    with tempfile.TemporaryDirectory(prefix='merino-theme-assets-') as temporary:
        source = Path(temporary)
        for file in ('theme.py', 'tmux-scroll.conf'):
            shutil.copy2(EXTRA / 'terminal-profile' / file, source / file)
        for name in ('light', 'dark'):
            for suffix in ('.toml', '-tmux.conf'):
                file = f'merino-{name}{suffix}'
                shutil.copy2(EXTRA / file, source / file)
        toggle = theme.install(writer, source, directory, tmux)
    if not managed:
        if main.is_symlink():
            # Keep the original dotfiles link live through the wrapper import.
            base.symlink_to(main.resolve())
        else:
            writer.write(base, original)
        wrapper = MARKER + '\n# Original settings and dotfiles imports remain in the first file.\n'
        wrapper += '[general]\nimport = ' + json.dumps([str(base), str(directory / 'active-theme.toml')]) + '\n'
        wrapper += '\n[window.theme_toggle_command]\nprogram = ' + json.dumps(toggle['program']) + '\n'
        wrapper += 'args = ' + json.dumps(toggle['args']) + '\n'
        writer.write(main, wrapper)
    writer.write(config / 'theme.conf', 'source-file ' + theme.shlex.quote(str(directory / 'theme.conf')) + '\n')
    tmux_config = config / 'tmux.conf'
    content = tmux_config.read_text() if tmux_config.exists() else 'source-file ~/.tmux.conf\n'
    source_line = 'source-file ' + theme.shlex.quote(str(config / 'theme.conf'))
    if source_line not in content.splitlines():
        writer.write(tmux_config, content.rstrip() + '\n\n# Keep the selected Merino theme after shared colors.\n' + source_line + '\n')
    print('Theme overlay installed:', directory)
    print('Original Alacritty settings:', base)
    print('zsh and dotfiles symlinks are unchanged.')
    if writer.backup.exists():
        print('Backup:', writer.backup)
    return directory


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--home', type=Path, default=Path.home())
    parser.add_argument('--tmux', type=Path, default=Path.home() / '.local/bin/tmux')
    args = parser.parse_args()
    install(args.home.expanduser().resolve(), args.tmux.expanduser().resolve())
