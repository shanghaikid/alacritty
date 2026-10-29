#!/usr/bin/env python3
"""Test installed Codex mouse selection against synthetic history, without a model.

Uses an isolated tmux socket/config and CODEX_HOME; no user sessions or credentials.
The provider points to a closed loopback port and no prompt is submitted. Automatic
clipboard copying is disabled. Requires Codex with its fullscreen transcript view.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shlex
import signal
import struct
import subprocess
import tempfile
import termios
import time
import uuid


def selected_text(capture):
    """Read inverse-video text from tmux's ANSI capture, not from a mock selection."""
    selected = []
    inverse = False
    for part in re.split(r'(\x1b\[[0-9;]*m)', capture):
        if part.startswith('\x1b['):
            for code in part[2:-1].split(';'):
                if code in ('', '0', '27'):
                    inverse = False
                elif code == '7':
                    inverse = True
        elif inverse:
            selected.append(part)
    return ''.join(selected)


def stop_fixture_daemons(config_dir):
    """Codex can leave a detached daemon; only stop binaries inside this fixture."""
    prefix = str(config_dir.resolve()) + os.sep

    def owned_pids():
        processes = subprocess.check_output(['ps', '-axo', 'pid=,comm='], text=True)
        result = []
        for line in processes.splitlines():
            pid, executable = line.strip().split(None, 1)
            if str(Path(executable).resolve()).startswith(prefix):
                result.append(int(pid))
        return result

    for attempt in range(50):
        remaining = owned_pids()
        if not remaining:
            return
        for pid in remaining:
            try:
                os.kill(pid, signal.SIGTERM if attempt < 30 else signal.SIGKILL)
            except ProcessLookupError:
                pass
        time.sleep(.1)
    raise RuntimeError(f'Fixture processes did not exit: {owned_pids()}')


def exercise(tmux_binary, codex_binary):
    with tempfile.TemporaryDirectory(prefix='alacritty-codex-selection-') as directory:
        root = Path(directory)
        config_dir = root / 'config'
        config_dir.mkdir()
        project = root / 'project'
        project.mkdir()
        config_dir.joinpath('config.toml').write_text(f'''model = "offline-test"
model_provider = "offline"
approval_policy = "never"
sandbox_mode = "read-only"
check_for_update_on_startup = false
[model_providers.offline]
name = "Offline UI fixture"
base_url = "http://127.0.0.1:1/v1"
wire_api = "responses"
requires_openai_auth = false
[projects.{json.dumps(str(project))}]
trust_level = "trusted"
[tui]
animations = false
screen_reader_detection_done = true
copy_on_select = "never"
''')
        thread_id = str(uuid.uuid4())
        stamp = '2026-10-29T00:00:00Z'
        records = [dict(timestamp=stamp, type='session_meta', payload=dict(
            id=thread_id, session_id=thread_id, timestamp=stamp, cwd=str(project),
            originator='codex', cli_version='0.165.0', source='cli', model_provider='offline'))]
        for i in range(60):
            role = 'user' if i % 2 == 0 else 'assistant'
            text = f'FIXTURE{i:03d} abcdefghijklmnopqrstuvwxyz'
            records.append(dict(timestamp=stamp, type='response_item', payload=dict(
                type='message', role=role, content=[dict(
                    type='input_text' if role == 'user' else 'output_text', text=text)])))
            records.append(dict(timestamp=stamp, type='event_msg', payload=dict(
                type='user_message' if role == 'user' else 'agent_message', message=text)))
        folder = config_dir / 'sessions/2026/10/29'
        folder.mkdir(parents=True)
        folder.joinpath(f'rollout-2026-10-29T00-00-00-{thread_id}.jsonl').write_text(
            ''.join(json.dumps(record) + '\n' for record in records))

        source_dir = Path(__file__).resolve().parent
        copier = root / 'copy'
        copier.write_text('#!/bin/sh\ncat > ' + shlex.quote(str(root / 'clipboard')) + '\n')
        copier.chmod(0o700)
        bindings = source_dir.joinpath('tmux-shift-selection.conf').read_text().replace(
            '~/.config/alacritty/tmux-mouse-anchor-input.py',
            shlex.quote(str(source_dir / 'tmux-mouse-anchor-input.py'))).replace(
            '~/.local/bin/tmux', shlex.quote(tmux_binary)).replace('pbcopy', str(copier))
        config = root / 'tmux.conf'
        config.write_text('set -g mouse on\nset -g status off\nset -g base-index 1\n'
                          'set -g history-limit 100000\nset -g set-clipboard off\n' + bindings)
        command = [tmux_binary, '-S', str(root / 'socket')]

        def tmux(*args):
            return subprocess.check_output(command + list(args), text=True, timeout=10).rstrip('\n')

        # No user config, credentials, existing conversation, or prompt is used.
        launch = ['env', 'CODEX_HOME=' + str(config_dir), codex_binary,
                  '-C', str(project), 'resume', thread_id]
        pid = master = None
        try:
            tmux('-f', str(config), 'new-session', '-d', '-x', '100', '-y', '30',
                 '-s', 'probe', '-n', 'CODEX', shlex.join(launch))
            pid, master = pty.fork()
            if pid == 0:
                fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 100, 1000, 600))
                env = dict(os.environ, TERM='xterm-256color')
                env.pop('TMUX', None)
                os.execve(tmux_binary, command + ['attach-session', '-t', 'probe'], env)
            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 100, 1000, 600))

            def tick(seconds=.15):
                until = time.monotonic() + seconds
                while time.monotonic() < until:
                    if select.select([master], [], [], .02)[0]:
                        os.read(master, 65536)

            def mouse(code, x, y, release=False):
                os.write(master, f'\x1b[<{code};{x};{y}{"m" if release else "M"}'.encode())
                tick()

            def capture(ansi=False):
                return tmux('capture-pane', '-p', *(['-e'] if ansi else []))

            def row_for(text):
                return next(i + 1 for i, line in enumerate(capture().splitlines()) if text in line)

            def mode():
                return tmux('display-message', '-p', '#{pane_in_mode}')

            for _ in range(150):
                tick(.1)
                if 'FIXTURE059' in capture():
                    break
            else:
                raise AssertionError('Codex did not display the fixture: ' + capture())
            assert tmux('display-message', '-p', '#{alternate_on}|#{mouse_any_flag}|#{history_size}') == '1|1|0'

            # Negative control: the former unconditional click-to-copy rule freezes
            # the actual fullscreen app and prevents its transcript from scrolling.
            tmux('bind-key', '-n', 'MouseDown1Pane',
                 'select-pane -t=; copy-mode -H; send -X stop-selection; send-keys User999')
            row = row_for('FIXTURE053')
            mouse(0, 4, row)
            mouse(32, 5, row)
            mouse(0, 5, row, True)
            frozen = capture()
            mouse(64, 20, 12)
            assert mode() == '1'
            assert capture() == frozen, 'Negative control unexpectedly scrolled'
            tmux('send-keys', '-X', 'cancel')
            tmux('source-file', str(config))
            tick(.5)

            # Select exactly one character, then scroll the real internal transcript.
            row = row_for('FIXTURE053')
            mouse(0, 4, row)
            mouse(32, 5, row)
            mouse(0, 5, row, True)
            assert mode() == '0'
            assert selected_text(capture(True)) == 'I', capture(True)
            for _ in range(12):
                mouse(64, 20, 12)
            assert 'FIXTURE048' in capture() and 'FIXTURE059' not in capture()
            assert selected_text(capture(True)) == 'I', capture(True)
            assert mode() == '0'

            # Shift-click extends the same anchor across several history entries.
            row = row_for('FIXTURE049')
            mouse(4, 10, row)
            mouse(4, 10, row, True)
            selected = selected_text(capture(True))
            for marker in ('049', 'FIXTURE050', 'FIXTURE051', 'FIXTURE052'):
                assert marker in selected, selected
            assert selected.rstrip().endswith('F'), selected
            for _ in range(12):
                mouse(65, 20, 12)
            assert 'FIXTURE059' in capture()
            assert selected_text(capture(True)), 'Scrolling back cleared the selection'

            # Clicking the composer clears the selection and accepts the first key.
            row = row_for('Ask Codex to do anything')
            mouse(0, 10, row)
            mouse(0, 10, row, True)
            os.write(master, b'scroll-fixture-input')  # Deliberately no Enter / model request.
            tick(.4)
            assert 'scroll-fixture-input' in capture(), capture()
            assert selected_text(capture(True)) == ''
            assert mode() == '0'

            # Top status tabs must still route to tmux, not to the application.
            tmux('new-window', '-d', '-n', 'OTHER', 'sleep 120')
            tmux('set-option', '-g', 'status-position', 'top')
            tmux('set-option', '-g', 'status-left', '')
            tmux('set-option', '-g', 'status-right', '')
            tmux('set-option', '-gw', 'window-status-format', ' TAB#I ')
            tmux('set-option', '-gw', 'window-status-current-format', ' TAB#I ')
            tmux('set-option', '-g', 'status', 'on')
            tick(.3)
            mouse(0, 10, 1)
            mouse(0, 10, 1, True)
            assert tmux('display-message', '-p', '#{window_name}') == 'OTHER'
            mouse(0, 3, 1)
            mouse(0, 3, 1, True)
            assert tmux('display-message', '-p', '#{window_name}') == 'CODEX'
            assert 'scroll-fixture-input' in capture()
            print(json.dumps(dict(result='PASS', codex=subprocess.check_output(
                [codex_binary, '--version'], text=True).strip(), negative_control=True,
                one_character_selection=True, scroll_preserves_selection=True,
                shift_extends_selection=True, composer_first_key=True, status_tabs=True)))
        finally:
            subprocess.run(command + ['kill-server'], stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL, timeout=10)
            if master is not None:
                os.close(master)
            if pid:
                for _ in range(50):
                    if os.waitpid(pid, os.WNOHANG)[0]:
                        break
                    time.sleep(.02)
                else:
                    os.kill(pid, signal.SIGTERM)
                    os.waitpid(pid, 0)
            stop_fixture_daemons(config_dir)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tmux', default=str(Path.home() / '.local/bin/tmux'))
    parser.add_argument('--codex', default='/opt/homebrew/bin/codex')
    args = parser.parse_args()
    exercise(args.tmux, args.codex)
