#!/usr/bin/env python3
"""Check blink phases through read-only IPC while a TUI repeats steady styles.

Uses a disposable application window and child, without sending keyboard or
mouse events or changing the user's configuration. Requires a desktop session.
"""
import json
import os
from pathlib import Path
import plistlib
import shutil
import signal
import subprocess
import sys
import tempfile
import time


def exercise(binary):
    binary = str(Path(binary).resolve())
    with tempfile.TemporaryDirectory(prefix='alacritty-blink-') as directory:
        root = Path(directory)
        ipc = root / 'app.sock'
        request = root / 'request'
        active = root / 'active'
        config = root / 'config.toml'
        config.write_text('''[window]
startup_mode = "Windowed"
dimensions = { columns = 50, lines = 8 }
title = "Cursor blink regression"
[keyboard.global_hotkey]
enabled = false
[cursor]
style = { shape = "Block", blinking = "Always" }
blink_interval = 100
blink_timeout = 0
''')
        child = root / 'child.py'
        child.write_text(fr'''
import os
from pathlib import Path
import time
request = Path({str(request)!r})
active = Path({str(active)!r})
os.write(1, b'\x1b[2J\x1b[Hprompt> \x1b[?25h\x1b[2 q')
while True:
    if request.exists():
        mode = request.read_text()
        # Codex and other TUIs can repeat cursor style on every redraw.
        if mode == 'hidden_refresh':
            os.write(1, b'\x1b[?25l\x1b[2 q')
            time.sleep(.03)
            os.write(1, b'\x1b[?25h')
        else:
            os.write(1, b'\x1b[2 q')
        active.write_text(mode)
    time.sleep(.02)
''')
        env = dict(os.environ)
        for name in ('TMUX', 'TMUX_PANE', 'ALACRITTY_SOCKET', 'ALACRITTY_WINDOW_ID'):
            env.pop(name, None)

        def snapshot():
            result = subprocess.run([binary, 'msg', '--socket', str(ipc), 'program-status',
                                     '--window-id=-1'], capture_output=True, text=True,
                                    timeout=3, env=env)
            if result.returncode:
                return None
            windows = json.loads(result.stdout)
            return windows[0] if windows else None

        def observe(description):
            states = []
            deadline = time.monotonic() + 1.5
            while time.monotonic() < deadline:
                state = snapshot()
                assert state and state['focused'], 'Test window must remain focused'
                if not state['cursor_visible']:
                    time.sleep(.02)
                    continue
                hidden = state['cursor_hidden']
                if not states or states[-1] != hidden:
                    states.append(hidden)
                time.sleep(.02)
            assert len(states) >= 5, (description, 'blink phase stalled', states)
            return len(states) - 1

        app = root / 'Alacritty.app'
        shutil.copytree(Path(__file__).parent / 'osx/Alacritty.app', app)
        plist = app / 'Contents/Info.plist'
        info = plistlib.loads(plist.read_bytes())
        info['CFBundleIdentifier'] = f'org.alacritty.blink-test.{os.getpid()}'
        info['CFBundleName'] = 'Alacritty Blink Test'
        plist.write_bytes(plistlib.dumps(info))
        executable = app / 'Contents/MacOS/alacritty'
        executable.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(binary, executable)
        subprocess.run(['codesign', '--force', '--deep', '--sign', '-', str(app)],
                       check=True, capture_output=True)
        with (root / 'open.log').open('w') as log:
            # LaunchServices registers and activates this isolated app, unlike directly
            # spawning an executable alongside an already-running Alacritty instance.
            process = subprocess.Popen(['open', '-n', '-W', '-a', str(app),
                                        '--stdout', str(root / 'app.log'),
                                        '--stderr', str(root / 'app.log'),
                                        '--args', '--config-file', str(config), '--socket', str(ipc),
                                        '-e', sys.executable, str(child)],
                                       env=env, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    if process.poll() is not None:
                        raise AssertionError((process.returncode, (root / 'app.log').read_text()))
                    if ipc.exists():
                        state = snapshot()
                        if state and state['focused']:
                            break
                    time.sleep(.05)
                else:
                    raise AssertionError('Disposable test window did not become focused')
                idle = observe('idle terminal')
                request.write_text('refresh')
                deadline = time.monotonic() + 3
                while not active.exists():
                    assert time.monotonic() < deadline, 'TUI refresh fixture did not start'
                    time.sleep(.02)
                refreshed = observe('repeated steady cursor styles')
                request.write_text('hidden_refresh')
                deadline = time.monotonic() + 3
                while active.read_text() != 'hidden_refresh':
                    assert time.monotonic() < deadline, 'Hidden-cursor refresh fixture did not start'
                    time.sleep(.02)
                hidden_refresh = observe('steady styles while the cursor is temporarily hidden')
                print(json.dumps({'idle_blink_transitions': idle,
                                  'tui_refresh_blink_transitions': refreshed,
                                  'hidden_refresh_blink_transitions': hidden_refresh}), flush=True)
            finally:
                processes = subprocess.check_output(['ps', '-axo', 'pid=,comm='], text=True)
                for line in processes.splitlines():
                    pid, command = line.strip().split(None, 1)
                    if command == str(executable):
                        try:
                            os.kill(int(pid), signal.SIGTERM)
                        except ProcessLookupError:
                            pass
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)


if __name__ == '__main__':
    exercise(sys.argv[1])
