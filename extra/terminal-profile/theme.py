#!/usr/bin/env python3
"""Persist a Merino palette and synchronize only tmux servers using this profile."""

import argparse
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile

THEMES = ('light', 'dark')
MARKER = '# Merino selection: '


@contextmanager
def locked(directory):
    directory.mkdir(parents=True, exist_ok=True)
    with (directory / '.theme.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield


def atomic_write(path, text):
    fd, name = tempfile.mkstemp(prefix='.theme-', dir=path.parent)
    try:
        with os.fdopen(fd, 'w') as stream:
            stream.write(text)
        os.replace(name, path)
    finally:
        if os.path.exists(name):
            os.unlink(name)


def selected(directory):
    path = directory / 'active-theme.toml'
    if path.exists():
        first = next(iter(path.read_text().splitlines()), '')
        if first.startswith(MARKER) and first[len(MARKER):] in THEMES:
            return first[len(MARKER):]
        raise ValueError('Invalid Merino selection in ' + str(path))
    return 'light'


def palette(directory, name):
    return MARKER + name + '\n' + (directory / f'merino-{name}.toml').read_text()


def install(writer, source, directory, tmux):
    directory = directory.resolve()
    # Use the same lock as the button helper so an upgrade cannot replace a
    # just-selected theme or expose a mixture of old/new palette files.
    with locked(directory):
        return install_locked(writer, source, directory, tmux)


def install_locked(writer, source, directory, tmux):
    name = selected(directory)
    for file in ('theme.py', 'tmux-scroll.conf', 'merino-light.toml', 'merino-dark.toml',
                 'merino-light-tmux.conf', 'merino-dark-tmux.conf'):
        writer.write(directory / file, (source / file).read_bytes())
    writer.write(directory / 'active-theme.toml', palette(directory, name))
    writer.write(directory / 'active-tmux.conf', (directory / f'merino-{name}-tmux.conf').read_bytes())
    python = shutil.which('python3')
    if python is None:
        raise RuntimeError('python3 is required for theme switching')
    register = shlex.join([python, str(directory / 'theme.py'), 'register', '--tmux', str(tmux),
                          '--socket']) + ' #{q:socket_path}'
    bootstrap = 'set -s @merino_theme_directory ' + shlex.quote(str(directory)) + '\n'
    # Both full installs and dotfiles upgrades load this bootstrap. Keep the
    # scroll fix here so it survives restarts without editing shared dotfiles.
    bootstrap += 'source-file ' + shlex.quote(str(directory / 'tmux-scroll.conf')) + '\n'
    bootstrap += 'source-file ' + shlex.quote(str(directory / 'active-tmux.conf')) + '\n'
    bootstrap += 'run-shell -b ' + shlex.quote(register) + '\n'
    writer.write(directory / 'theme.conf', bootstrap)
    return {'program': python, 'args': [str(directory / 'theme.py'), 'toggle']}


def registered(directory):
    path = directory / 'servers.json'
    return json.loads(path.read_text()) if path.exists() else []


def register(directory, tmux, socket):
    command = [tmux, '-S', socket, '-N']
    result = subprocess.run(command + ['show-options', '-sv', '@merino_theme_directory'],
                            capture_output=True, text=True, check=True, timeout=3)
    if not result.stdout.strip() or Path(result.stdout.strip()).resolve() != directory.resolve():
        raise ValueError('tmux server does not use this Merino profile')
    # Startup may have sourced the previous palette immediately before another
    # process toggled. Catch up under the lock before joining future updates.
    subprocess.run(command + ['source-file', str(directory / 'active-tmux.conf')],
                   capture_output=True, check=True, timeout=5)
    entries = [item for item in registered(directory) if item['socket'] != socket]
    entries.append({'tmux': tmux, 'socket': socket})
    atomic_write(directory / 'servers.json', json.dumps(entries[-128:]) + '\n')


def switch(directory, requested):
    old = selected(directory)
    name = ('dark' if old == 'light' else 'light') if requested == 'toggle' else requested
    terminal = palette(directory, name)
    tmux_config = directory / 'active-tmux.conf'
    previous_tmux = tmux_config.read_text()
    servers = []
    for entry in registered(directory):
        command = [entry['tmux'], '-S', entry['socket'], '-N']
        try:
            result = subprocess.run(command + ['show-options', '-sv', '@merino_theme_directory'],
                                    capture_output=True, text=True, timeout=3)
        except (OSError, subprocess.TimeoutExpired):
            continue
        # A socket may have been reused by an unrelated server since registration.
        if result.returncode == 0 and result.stdout.strip() and Path(result.stdout.strip()).resolve() == directory.resolve():
            servers.append(command)
    atomic_write(tmux_config, (directory / f'merino-{name}-tmux.conf').read_text())
    try:
        for command in servers:
            subprocess.run(command + ['source-file', str(tmux_config)], check=True,
                           capture_output=True, timeout=5)
        # Commit selection last; Alacritty's watcher reloads this atomic replacement.
        atomic_write(directory / 'active-theme.toml', terminal)
    except Exception:
        atomic_write(tmux_config, previous_tmux)
        for command in servers:
            try:
                subprocess.run(command + ['source-file', str(tmux_config)],
                               capture_output=True, timeout=5)
            except (OSError, subprocess.TimeoutExpired):
                pass
        raise
    return name


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('toggle', 'light', 'dark', 'status', 'register'))
    parser.add_argument('--directory', type=Path, default=Path(__file__).resolve().parent)
    parser.add_argument('--tmux')
    parser.add_argument('--socket')
    args = parser.parse_args()
    directory = args.directory.resolve()
    with locked(directory):
        if args.action == 'register':
            if not args.tmux or not args.socket:
                parser.error('register requires --tmux and --socket')
            register(directory, args.tmux, args.socket)
        elif args.action == 'status':
            print(selected(directory))
        else:
            print(switch(directory, args.action))


if __name__ == '__main__':
    main()
