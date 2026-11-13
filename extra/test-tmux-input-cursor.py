#!/usr/bin/env python3
"""Observe the cursor rendered by tmux, independently of its selection cursor."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import time

TMUX = os.environ.get('TEST_TMUX', str(Path.home() / '.local/bin/tmux'))
EXTRA = Path(__file__).resolve().parent


def exercise(keys, status, line_numbers='off', blinking=False):
    with tempfile.TemporaryDirectory(prefix='alacritty-input-cursor-') as directory:
        root = Path(directory)
        inner = [TMUX, '-S', str(root / 'inner')]
        outer = [TMUX, '-S', str(root / 'outer')]
        env = dict(os.environ)
        env.pop('TMUX', None)
        env.pop('TMUX_PANE', None)

        def call(command, *args):
            return subprocess.check_output(command + list(args), text=True,
                                           stderr=subprocess.PIPE, timeout=10, env=env).strip()

        def snapshot(command):
            return tuple(map(int, call(command, 'display-message', '-p',
                                      '#{cursor_x},#{cursor_y},#{cursor_flag}').split(',')))

        def wait_for(check, description):
            deadline = time.monotonic() + 4
            while time.monotonic() < deadline:
                if check():
                    return
                time.sleep(.03)
            raise AssertionError((description, snapshot(inner), snapshot(outer)))

        def mouse(code, x, y, release=False):
            call(outer, 'send-keys', '-l', '--', f'\x1b[<{code};{x};{y}{"m" if release else "M"}')
            time.sleep(.08)

        copier = root / 'copy'
        copier.write_text('#!/bin/sh\ncat >/dev/null\n')
        copier.chmod(0o700)
        bootstrap = shlex.join([sys.executable, str(EXTRA / 'tmux-mouse-anchor-input.py'), TMUX])
        bootstrap += ' "#{socket_path}"'
        bindings = '\n'.join(
            'run-shell ' + shlex.quote(bootstrap) if line.startswith('run-shell ') else line
            for line in (EXTRA / 'tmux-shift-selection.conf').read_text().replace(
                'pbcopy', str(copier)).splitlines()) + '\n'
        config = root / 'tmux.conf'
        config.write_text('set -g mouse on\nset -g set-clipboard off\nset -g default-shell /bin/sh\n'
                          f'set -g mode-keys {keys}\nset -g status {status}\n'
                          f'set -g copy-mode-line-numbers {line_numbers}\n'
                          'set -g status-position top\n'
                          'bind -T copy-mode WheelUpPane send -X -N 3 scroll-up\n'
                          'bind -T copy-mode-vi WheelUpPane send -X -N 3 scroll-up\n' + bindings)
        received = root / 'received'
        control = root / 'control'
        fixture = root / 'application.py'
        fixture.write_text(fr'''
import os
from pathlib import Path
import select
import sys
import tty

tty.setraw(0)
control = Path({str(control)!r})
last_command = ''

def render():
    sys.stdout.write(''.join('L%03d abcdefghijklmnopqrstuvwxyz\r\n' % i for i in range(100)))
    if {blinking!r}:
        sys.stdout.write('\x1b[?12h')
    sys.stdout.write('\x1b[6;1Hprompt> ')
    sys.stdout.flush()

render()
with open({str(received)!r}, 'ab', buffering=0) as output:
    while True:
        if select.select([0], [], [], .02)[0]:
            data = os.read(0, 4096)
            if not data:
                break
            output.write(data)
            if b'\x01' in data:
                sys.stdout.write('\x1b[?25l')
                sys.stdout.flush()
        if control.exists():
            command = control.read_text()
            if command != last_command:
                last_command = command
                if command == 'output':
                    sys.stdout.write('\x1b[10;21HLIVE')
                    sys.stdout.flush()
                elif command == 'reset':
                    render()
                elif command == 'edge':
                    sys.stdout.write('\x1b[6;59H')
                    sys.stdout.flush()
                elif command == 'wrap':
                    sys.stdout.write('\x1b[6;1H' + 'W' * 60)
                    sys.stdout.flush()
''')
        observer_config = root / 'observer.conf'
        observer_config.write_text('set -g status off\nset -g default-shell /bin/sh\n'
                                   'set -g default-terminal xterm-256color\n')
        try:
            call(inner, '-f', str(config), 'new-session', '-d', '-x', '60', '-y', '12',
                 '-s', 'test', shlex.join([sys.executable, str(fixture)]))
            # A second tmux parses the actual client output, including cursor visibility.
            call(outer, '-f', str(observer_config), 'new-session', '-d', '-x', '60', '-y', '12',
                 '-s', 'observer', shlex.join(inner + ['attach-session', '-t', 'test']))
            offset = int(status == 'on')

            def gutter_width():
                return call(outer, 'capture-pane', '-p').splitlines()[5 + offset].index('prompt>')

            expected = (8, 5 + offset, 1)
            wait_for(lambda: snapshot(outer) == expected, 'initial input cursor')
            if blinking:
                wait_for(lambda: call(outer, 'display-message', '-p', '#{cursor_blinking}') == '1',
                         'initial explicit blinking is observable')
            if line_numbers != 'off':
                call(inner, 'copy-mode')
                wait_for(lambda: gutter_width() > 0, 'copy-mode line numbers are displayed')
                gutter = gutter_width()
                expected = (8 + gutter, 5 + offset, 1)
                wait_for(lambda: snapshot(outer) == expected, 'input cursor with line numbers')

            mouse(0, 12, 3 + offset)
            mouse(0, 12, 3 + offset, True)
            assert call(inner, 'display-message', '-p', '#{pane_in_mode}|#{selection_present}') == '1|0'
            anchor = call(inner, 'display-message', '-p', '#{selection_start_x},#{selection_start_y}')
            logical = call(inner, 'display-message', '-p', '#{copy_cursor_x},#{copy_cursor_y}')
            assert logical != '8,5', logical
            wait_for(lambda: snapshot(outer) == expected, 'single click must keep the input cursor')
            if blinking:
                wait_for(lambda: call(outer, 'display-message', '-p', '#{cursor_blinking}') == '1',
                         'empty anchor must preserve explicitly requested blinking')

            mouse(4, 22, 8 + offset)
            mouse(4, 22, 8 + offset, True)
            assert call(inner, 'display-message', '-p', '#{selection_present}') == '1'
            assert call(inner, 'display-message', '-p', '#{selection_start_x},#{selection_start_y}') == anchor
            # A plain click dismisses the range; the first typed key reaches the app.
            time.sleep(.4)
            mouse(0, 14, 10 + offset)
            mouse(0, 14, 10 + offset, True)
            call(outer, 'send-keys', '-l', '--', 'hello')
            wait_for(lambda: received.read_bytes().endswith(b'hello'), 'input after selection')

            time.sleep(.4)
            mouse(0, 12, 3 + offset)
            mouse(0, 12, 3 + offset, True)
            mouse(64, 20, 6 + offset)
            wait_for(lambda: snapshot(outer) == (8, 8 + offset, 1),
                     'scrolled input cursor stays aligned with its text')
            for _ in range(3):
                mouse(64, 20, 6 + offset)
            wait_for(lambda: snapshot(outer)[2] == 0, 'hide input cursor while viewing history')
            call(inner, 'send-keys', '-X', 'cancel')

            # Explicit keyboard copy mode must keep its own navigation cursor.
            call(inner, 'copy-mode')
            call(inner, 'send-keys', '-X', 'cursor-left')
            logical = tuple(map(int, call(inner, 'display-message', '-p',
                                        '#{copy_cursor_x},#{copy_cursor_y}').split(',')))
            if line_numbers != 'off':
                wait_for(lambda: gutter_width() > 0, 'keyboard copy-mode line numbers are displayed')
            gutter = gutter_width()
            wait_for(lambda: snapshot(outer) == (logical[0] + gutter, logical[1] + offset, 1),
                     'keyboard copy-mode cursor')
            call(inner, 'send-keys', '-X', 'cancel')

            time.sleep(.4)
            mouse(0, 12, 3 + offset)
            mouse(0, 12, 3 + offset, True)
            frozen_text = call(outer, 'capture-pane', '-p')
            control.write_text('output')
            wait_for(lambda: snapshot(inner) == (24, 9, 1), 'asynchronous application output')
            assert call(outer, 'capture-pane', '-p') == frozen_text
            wait_for(lambda: snapshot(outer) == (8, 5 + offset, 1),
                     'input cursor must stay aligned with the frozen copy-mode text')
            call(inner, 'send-keys', '-X', 'cancel')
            wait_for(lambda: snapshot(outer) == (24, 9 + offset, 1), 'return to live input cursor')
            control.write_text('reset')
            wait_for(lambda: snapshot(outer) == (8, 5 + offset, 1), 'reset input fixture')

            if line_numbers != 'off':
                control.write_text('edge')
                wait_for(lambda: snapshot(inner) == (58, 5, 1), 'input near the right edge')
                call(inner, 'copy-mode')
                time.sleep(.4)
                mouse(0, 12, 3 + offset)
                mouse(0, 12, 3 + offset, True)
                wait_for(lambda: snapshot(outer)[2] == 0,
                         'line-number gutter clips the input position')
                call(inner, 'send-keys', '-X', 'cancel')
                control.write_text('reset')
                wait_for(lambda: snapshot(outer) == (8, 5 + offset, 1), 'reset clipped cursor fixture')

            control.write_text('wrap')
            wait_for(lambda: snapshot(inner) == (60, 5, 1), 'application cursor has a pending wrap')
            time.sleep(.4)
            mouse(0, 12, 3 + offset)
            mouse(0, 12, 3 + offset, True)
            wait_for(lambda: snapshot(outer) == (59, 5 + offset, 1),
                     'empty anchor keeps the last-column input cursor visible')
            call(inner, 'send-keys', '-X', 'cancel')
            control.write_text('reset')
            wait_for(lambda: snapshot(outer) == (8, 5 + offset, 1), 'reset pending wrap fixture')

            call(outer, 'send-keys', 'C-a')
            wait_for(lambda: snapshot(outer)[2] == 0, 'application-hidden cursor')
            time.sleep(.4)
            mouse(0, 12, 3 + offset)
            mouse(0, 12, 3 + offset, True)
            wait_for(lambda: snapshot(outer)[2] == 0, 'empty anchor must respect hidden input cursor')
            print(json.dumps({'keys': keys, 'status': status, 'line_numbers': line_numbers,
                              'blinking': blinking, 'input_cursor_preserved': True,
                              'shift_selection_preserved': True, 'history_cursor_hidden': True,
                              'frozen_input_cursor_preserved': True,
                              'keyboard_copy_cursor_preserved': True}), flush=True)
        finally:
            for command in (outer, inner):
                subprocess.run(command + ['kill-server'], stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL, timeout=10, env=env)


if __name__ == '__main__':
    for keys in ('vi', 'emacs'):
        for status in ('off', 'on'):
            exercise(keys, status)
        exercise(keys, 'on', line_numbers='absolute')
        exercise(keys, 'off', blinking=True)
