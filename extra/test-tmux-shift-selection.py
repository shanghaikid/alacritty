#!/usr/bin/env python3
"""Exercise tmux drag, scroll and Shift-click through a disposable client's PTY."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import shlex
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

TMUX = str(Path.home() / '.local/bin/tmux')
FIELDS = ['selection_start_x', 'selection_start_y', 'selection_end_x', 'selection_end_y',
          'scroll_position', 'history_size', 'pane_in_mode', 'selection_present', 'selection_active']


def exercise(keys, line_numbers):
    with tempfile.TemporaryDirectory(prefix='alacritty-tmux-selection-') as directory:
        root = Path(directory)
        socket = root / 'server.sock'
        clipboard = root / 'clipboard'
        copier = root / 'copy'
        copier.write_text('#!/bin/sh\ncat > ' + shlex.quote(str(clipboard)) + '\n')
        copier.chmod(0o700)
        config = root / 'tmux.conf'
        bindings = (Path(__file__).parent / 'tmux-shift-selection.conf').read_text().replace('pbcopy', str(copier))
        config.write_text('set -g mouse on\nset -g status off\nset -g set-clipboard off\n'
                          f'set -g mode-keys {keys}\nset -g copy-mode-line-numbers {line_numbers}\n' + bindings)
        command = [TMUX, '-S', str(socket)]
        def tmux(*args):
            return subprocess.check_output(command + list(args), text=True, stderr=subprocess.PIPE, timeout=5).strip()
        payload = "import time; print(''.join('L%03d abcdefghijklmnopqrstuvwxyz\\n' % i for i in range(100)),end='',flush=True); time.sleep(120)"
        child_command = shlex.join([sys.executable, '-c', payload])
        tmux('-f', str(config), 'new-session', '-d', '-x', '60', '-y', '12', '-s', 'test', child_command)
        pid, master = pty.fork()
        if pid == 0:
            env = dict(os.environ, TERM='xterm-256color')
            env.pop('TMUX', None)
            os.execve(TMUX, command + ['attach-session', '-t', 'test'], env)
        fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack('HHHH', 12, 60, 0, 0))
        def tick(seconds=.15):
            until = time.monotonic() + seconds
            while time.monotonic() < until:
                if select.select([master], [], [], .02)[0]:
                    os.read(master, 65536)
        def snapshot():
            values = tmux('display-message', '-p', '-t', 'test', '|'.join('#{' + k + '}' for k in FIELDS)).split('|')
            return dict(zip(FIELDS, [int(v or 0) for v in values]))
        def mouse(code, x, y, release=False):
            os.write(master, f'\x1b[<{code};{x};{y}{"m" if release else "M"}'.encode())
            tick()
        try:
            tick(.7)
            tmux('copy-mode', '-t', 'test')
            tmux('send-keys', '-t', 'test', '-X', 'history-top')
            tick()
            # Line numbers consume a gutter; these tests select within text in both modes.
            mouse(0, 12, 2)
            mouse(32, 17, 3)
            mouse(0, 17, 3, True)
            original = snapshot()
            assert original['pane_in_mode'] and original['selection_present'], original
            assert not original['selection_active'], original
            os.write(master, b'\x1b[<69;20;6M' * 30)
            tick(.3)
            before = snapshot()
            assert before['scroll_position'] == original['scroll_position'] - 30, (original, before)
            assert before['selection_start_y'] == original['selection_start_y']
            assert before['selection_end_y'] == original['selection_end_y']
            mouse(4, 22, 7)
            mouse(4, 22, 7, True)
            after = snapshot()
            assert after['selection_start_x'] == original['selection_start_x'], (original, after)
            assert after['selection_start_y'] == original['selection_start_y'], (original, after)
            assert after['scroll_position'] == before['scroll_position'], (before, after)
            assert after['selection_end_x'] == 21 - (11 - original['selection_start_x']), (original, after)
            assert after['selection_end_y'] == before['history_size'] - before['scroll_position'] + 6, (before, after)
            assert not after['selection_active'], after
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                if clipboard.exists() and 'L020 ' in clipboard.read_text():
                    break
                tick(.05)
            else:
                raise AssertionError('Clipboard job did not complete: ' + tmux('show-messages'))
            # Repeated Shift-click changes the endpoint without losing the original anchor.
            mouse(4, 18, 5)
            mouse(4, 18, 5, True)
            repeated = snapshot()
            assert repeated['selection_start_y'] == original['selection_start_y'], repeated
            assert repeated['selection_end_y'] == before['history_size'] - before['scroll_position'] + 4, repeated
            assert repeated['scroll_position'] == before['scroll_position'], repeated
            # Rapid Shift-clicks may be classified as double/triple clicks by tmux.
            for y in [6, 7, 8]:
                os.write(master, f'\x1b[<4;18;{y}M\x1b[<4;18;{y}m'.encode())
                tick(.05)
            tick(.2)
            rapid = snapshot()
            assert rapid['selection_start_y'] == original['selection_start_y'], rapid
            assert rapid['selection_end_y'] == before['history_size'] - before['scroll_position'] + 7, rapid
            copied_lines = len(clipboard.read_text().splitlines())
            # Double/triple clicks can start in the live pane, outside copy mode.
            tmux('send-keys', '-t', 'test', '-X', 'cancel')
            tick(.4)
            for _ in range(3):
                os.write(master, b'\x1b[<0;16;4M\x1b[<0;16;4m')
                tick(.05)
            tick(.4)
            word_or_line = snapshot()
            assert word_or_line['pane_in_mode'] and word_or_line['selection_present'], word_or_line
            assert not word_or_line['selection_active'], word_or_line
            print(json.dumps({'mode': keys, 'line_numbers': line_numbers, 'drag_retained': True,
                              'scroll_preserved_selection': True, 'shift_click_extended': True,
                              'repeated_shift_click': True, 'copied_lines': copied_lines, 'word_line_selection_retained': True}))
        finally:
            subprocess.run(command + ['kill-server'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                os.kill(pid, signal.SIGHUP)
            except ProcessLookupError:
                pass
            os.waitpid(pid, 0)
            os.close(master)


if __name__ == '__main__':
    for keys in ['vi', 'emacs']:
        for numbers in ['off', 'absolute']:
            exercise(keys, numbers)
