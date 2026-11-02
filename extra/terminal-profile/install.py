#!/usr/bin/env python3
"""Install the complete Merino terminal profile; preserve previous user files."""

import argparse
import datetime
import json
import os
from pathlib import Path
import platform
import shlex
import shutil
import subprocess
import tempfile

import theme

BEGIN = '# >>> Alacritty Merino profile >>>'
END = '# <<< Alacritty Merino profile <<<'
HOOK_COMMAND = '"$HOME/.local/bin/codex-program-status" "$PPID"'
EVENTS = ('SessionStart', 'SessionEnd', 'UserPromptSubmit', 'PermissionRequest',
          'PreToolUse', 'PostToolUse', 'Stop', 'Interrupt')


class Writer:
    def __init__(self, home):
        self.backup = home / '.alacritty-backups' / datetime.datetime.now().strftime('%Y%m%d-%H%M%S-%f')

    def write(self, path, data, mode=0o644):
        if isinstance(data, str):
            data = data.encode()
        if path.is_file() and not path.is_symlink() and path.read_bytes() == data:
            path.chmod(mode)
            return
        path.parent.mkdir(parents=True, exist_ok=True)
        if path.exists() or path.is_symlink():
            # Preserve symlinks themselves, without modifying external dotfiles.
            self.backup.mkdir(parents=True, exist_ok=True, mode=0o700)
            destination = self.backup / str(path).lstrip('/')
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.move(str(path), str(destination))
        fd, temporary = tempfile.mkstemp(prefix='.alacritty-', dir=path.parent)
        try:
            with os.fdopen(fd, 'wb') as stream:
                stream.write(data)
                os.fchmod(stream.fileno(), mode)
            os.replace(temporary, path)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)


def prompt_rc(original, profile):
    if original.count(BEGIN) != original.count(END) or original.count(BEGIN) > 1:
        raise ValueError('Malformed Alacritty block in .zshrc; refusing to overwrite it')
    if BEGIN in original:
        before, tail = original.split(BEGIN, 1)
        _, after = tail.split(END, 1)
        original = before.rstrip('\n') + after
    return original.rstrip('\n') + '\n\n' + BEGIN + '\n' + (
        'if [[ ${ALACRITTY_TERMINAL_PROFILE:-} == merino ]]; then\n'
        f'  source {shlex.quote(str(profile / "prompt.zsh"))}\n'
        'fi\n' + END + '\n')


def merge_hooks(document):
    hooks = document.setdefault('hooks', {})
    for event in EVENTS:
        groups = hooks.setdefault(event, [])
        if any(h.get('command') == HOOK_COMMAND for g in groups for h in g.get('hooks', [])):
            continue
        group = {'hooks': [{'type': 'command', 'command': HOOK_COMMAND, 'timeout': 1}]}
        if event == 'SessionStart':
            group['matcher'] = '^(startup|resume|clear)$'
        if event == 'PreToolUse':
            group['matcher'] = r'(^|[._])request_user_input$'
        groups.append(group)
    return json.dumps(document, indent=2) + '\n'


def install_profile(app, home, config, zsh_dir, codex_dir):
    source = app / 'Contents/Resources/terminal-profile'
    profile = config / 'terminal-profile'
    tmux = app / 'Contents/MacOS/tmux'
    if not tmux.is_file():
        raise ValueError('Bundle has no tmux; build it with make terminal first')
    # Validate user-owned input before making any configuration changes.
    rc = zsh_dir / '.zshrc'
    new_rc = prompt_rc(rc.read_text() if rc.exists() else '', profile)
    hooks = codex_dir / 'hooks.json'
    new_hooks = merge_hooks(json.loads(hooks.read_text()) if hooks.exists() else {})
    writer = Writer(home)
    toggle = theme.install(writer, source, profile, tmux)
    writer.write(config / 'theme.conf', 'source-file ' + shlex.quote(str(profile / 'theme.conf')) + '\n')
    for name in ('prompt.zsh', 'mouse-reset.zsh', 'window-name', 'start.zsh',
                 'merino-light.toml', 'rex-appearance.toml', 'merino-light-tmux.conf',
                 'tmux-mouse-anchor-input.py'):
        writer.write(profile / name, (source / name).read_bytes(),
                     0o755 if name in ('window-name', 'start.zsh') else 0o644)
    writer.write(profile / 'paths.zsh', f'tmux_bin={shlex.quote(str(tmux))}\n')
    selection = (source / 'tmux-shift-selection.conf').read_text()
    selection = selection[:selection.rindex('run-shell ')]
    command = ' '.join(shlex.quote(str(x)) for x in (
        shutil.which('python3'), profile / 'tmux-mouse-anchor-input.py', tmux))
    selection += 'run-shell ' + json.dumps(command + ' "#{socket_path}"') + '\n'
    writer.write(profile / 'tmux-shift-selection.conf', selection)
    label = '#(' + shlex.quote(str(profile / 'window-name')) + ' #{q:pane_current_path})'
    base = (source / 'tmux-base.conf').read_text().replace(
        '@TAB_LABEL@', 'setw -g @tab-label ' + shlex.quote(label))
    base += 'set-environment -g ALACRITTY_TERMINAL_PROFILE merino\n'
    for name in ('theme.conf', 'tmux-shift-selection.conf'):
        base += 'source-file ' + shlex.quote(str(profile / name)) + '\n'
    base += 'bind r source-file ' + shlex.quote(str(profile / 'tmux.conf')) + '\n'
    writer.write(profile / 'tmux.conf', base)
    main = '[general]\nimport = ' + json.dumps([
        str(profile / 'rex-appearance.toml'), str(profile / 'active-theme.toml')]) + '\n'
    main += '\n[window.theme_toggle_command]\nprogram = ' + json.dumps(toggle['program']) + '\n'
    main += 'args = ' + json.dumps(toggle['args']) + '\n'
    main += '\n[terminal]\nshell = ' + json.dumps(str(profile / 'start.zsh')) + '\n'
    main += '\n[env]\nALACRITTY_TERMINAL_PROFILE = "merino"\n'
    if zsh_dir != home:
        main += 'ZDOTDIR = ' + json.dumps(str(zsh_dir)) + '\n'
    main += '\n[mouse]\nshift_passthrough = true\n\n'
    bindings = (source / 'bindings.toml').read_text()
    additional = '  { key = "N", mods = "Command|Shift", command = { program = '
    additional += json.dumps(str(app / 'Contents/MacOS/alacritty'))
    additional += ', args = ["msg", "create-window", "-e", "/bin/zsh", "-l"] } },\n'
    before, after = bindings.rsplit('\n]', 1)
    main += before + '\n' + additional + ']' + after
    writer.write(config / 'alacritty.toml', main)
    writer.write(rc, new_rc, 0o600)
    writer.write(home / '.local/bin/codex-program-status',
                 (source / 'codex-program-status').read_bytes(), 0o755)
    writer.write(hooks, new_hooks, 0o600)
    return writer


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bundle', type=Path, default=Path(__file__).resolve().parents[3])
    parser.add_argument('--app-dir', type=Path, default=Path('/Applications'))
    parser.add_argument('--home', type=Path, default=Path.home(), help='Alternate home for staging tests')
    parser.add_argument('--skip-preferences', action='store_true')
    args = parser.parse_args()
    if platform.system() != 'Darwin':
        parser.error('This installer requires macOS')
    home = args.home.expanduser().resolve()
    real_home = home == Path.home().resolve()
    config = Path(os.environ.get('XDG_CONFIG_HOME', home / '.config')) if real_home else home / '.config'
    zsh_dir = Path(os.environ.get('ZDOTDIR', home)) if real_home else home
    codex_dir = Path(os.environ.get('CODEX_HOME', home / '.codex')) if real_home else home / '.codex'
    bundle = args.bundle.resolve()
    app = args.app_dir.expanduser().resolve() / 'Alacritty.app'
    subprocess.run(['codesign', '--verify', '--deep', '--strict', str(bundle)], check=True)
    if app != bundle:
        args.app_dir.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='.alacritty-install-', dir=app.parent) as tmp:
            staged = Path(tmp) / app.name
            subprocess.run(['ditto', str(bundle), str(staged)], check=True)
            subprocess.run(['codesign', '--verify', '--deep', '--strict', str(staged)], check=True)
            if app.exists():
                backup = Writer(home).backup / app.name
                backup.parent.mkdir(parents=True, exist_ok=True)
                shutil.move(str(app), str(backup))
                print('Previous app:', backup)
            shutil.move(str(staged), str(app))
    writer = install_profile(app, home, config / 'alacritty', zsh_dir, codex_dir)
    if real_home and not args.skip_preferences:
        previous = subprocess.run(['defaults', 'export', 'org.alacritty', '-'],
                                  capture_output=True)
        writer.backup.mkdir(parents=True, exist_ok=True, mode=0o700)
        writer.write(writer.backup / 'org.alacritty-before.plist', previous.stdout, 0o600)
        subprocess.run(['defaults', 'write', 'org.alacritty', 'AppleFontSmoothing', '-int', '0'], check=True)
        subprocess.run(['defaults', 'write', 'org.alacritty', 'NSUserKeyEquivalents', '-dict-add',
                        'Hide Alacritty', '@^~$h', 'Hide alacritty', '@^~$h'], check=True)
    if writer.backup.exists():
        print('Previous configuration:', writer.backup)
    print('Installed:', app)
    print('Quit and reopen Alacritty. Existing tmux servers are unchanged.')
    print('For Codex status badges, use /hooks to trust codex-program-status once on this Mac.')


if __name__ == '__main__':
    main()
