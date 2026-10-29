#!/usr/bin/env python3
"""Exercise cold startup and reconnect through a disposable tmux client's PTY."""
import json
import os
from pathlib import Path
import pty
import select
import shlex
import signal
import subprocess
import tempfile
import time

TMUX = str(Path.home() / '.local/bin/tmux')
EXTRA = Path(__file__).resolve().parent


def exercise(existing_server=False):
    with tempfile.TemporaryDirectory(prefix='alacritty-startup-test-') as directory:
        root = Path(directory)
        command = [TMUX, '-S', str(root / 'socket')]

        def tmux(*args):
            return subprocess.check_output(command + list(args), text=True,
                                           stderr=subprocess.PIPE, timeout=10).strip()

        fragment = root / 'selection.conf'
        fragment.write_text((EXTRA / 'tmux-shift-selection.conf').read_text().replace(
            '~/.config/alacritty/tmux-mouse-anchor-input.py',
            shlex.quote(str(EXTRA / 'tmux-mouse-anchor-input.py'))))
        config = root / 'tmux.conf'
        config.write_text(
            'set -g mouse on\nset -g status off\nset -g default-shell /bin/sh\n'
            'set -g default-command "printf STARTUP_READY; exec sleep 120"\n'
            'set -gF @startup_loads "#{e|+:#{?@startup_loads,#{@startup_loads},0},1}"\n'
            'source-file ' + shlex.quote(str(fragment)) + '\n')
        script = root / 'start.zsh'
        source = (EXTRA / 'start-iterm2-tmux.zsh').read_text()
        source = source.replace('-L default', '-S ' + shlex.quote(str(root / 'socket')))
        source = source.replace('config_file="$HOME/.config/alacritty/tmux.conf"',
                                'config_file=' + shlex.quote(str(config)))
        script.write_text(source)

        def attach():
            start = time.monotonic()
            pid, master = pty.fork()
            if pid == 0:
                env = dict(os.environ, TERM='alacritty')
                env.pop('TMUX', None)
                os.execve('/bin/zsh', ['/bin/zsh', str(script)], env)
            output = b''
            try:
                deadline = start + 15
                while time.monotonic() < deadline and b'STARTUP_READY' not in output:
                    if select.select([master], [], [], .1)[0]:
                        output += os.read(master, 65536)
                assert b'STARTUP_READY' in output, output[-1000:]
                elapsed = time.monotonic() - start
                os.write(master, b'\x02d')
                # Observe detachment without indefinitely waiting on a broken launcher.
                deadline = time.monotonic() + 5
                while os.waitpid(pid, os.WNOHANG)[0] == 0:
                    assert time.monotonic() < deadline, 'Client did not detach'
                    time.sleep(.01)
                pid = None
                return round(elapsed, 4)
            finally:
                if pid is not None:
                    try:
                        os.kill(pid, signal.SIGHUP)
                    except ProcessLookupError:
                        pass
                    os.waitpid(pid, 0)
                os.close(master)

        try:
            if existing_server:
                tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'unrelated', 'sleep 120')
            initial = attach()
            assert tmux('show-options', '-gv', '@startup_loads') == '1'
            first = tmux('display-message', '-p', '-t', '=alacritty', '#{window_id}')
            tmux('new-window', '-d', '-t', 'alacritty', 'sleep 120')
            tmux('select-window', '-t', first)
            windows_before = tmux('list-windows', '-t', '=alacritty', '-F', '#{window_id}')
            bindings_before = tmux('list-keys')
            reconnect = [attach() for _ in range(3)]
            assert tmux('show-options', '-gv', '@startup_loads') == '1', 'Reconnect reloaded config'
            assert tmux('list-windows', '-t', '=alacritty', '-F', '#{window_id}') == windows_before
            assert tmux('display-message', '-p', '-t', '=alacritty', '#{window_id}') == first
            assert tmux('list-keys') == bindings_before, 'Reconnect rewrote bindings'
            if existing_server:
                tmux('has-session', '-t', '=unrelated')
            # Explicit configuration reload continues to work.
            tmux('source-file', str(config))
            assert tmux('show-options', '-gv', '@startup_loads') == '2'
            print(json.dumps({'existing_server': existing_server, 'initial_seconds': initial,
                              'reconnect_seconds': reconnect, 'configuration_loaded_once': True,
                              'tabs_and_selection_preserved': True,
                              'bindings_unchanged_on_reconnect': True}), flush=True)
        finally:
            subprocess.run(command + ['kill-server'], stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL)


if __name__ == '__main__':
    exercise()
    exercise(existing_server=True)
