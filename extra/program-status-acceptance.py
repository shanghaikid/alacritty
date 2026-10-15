#!/usr/bin/env python3
"""Isolated native-window/PTY acceptance; never touches existing windows or tmux panes."""
import base64
import json
import os
from pathlib import Path
import select
import socket
import subprocess
import sys
import tempfile
import termios
import time
import tty


def child(path):
    server = socket.socket(socket.AF_UNIX)
    server.bind(path)
    server.listen()
    while True:
        conn, _ = server.accept()
        with conn:
            request = json.loads(conn.makefile('rb').readline())
            raw = base64.b64decode(request['bytes'])
            if request.get('query'):
                settings = termios.tcgetattr(0)
                tty.setraw(0)
                try:
                    os.write(1, raw)
                    reply = b''
                    deadline = time.monotonic() + 2
                    while time.monotonic() < deadline:
                        if select.select([0], [], [], .1)[0]:
                            reply += os.read(0, 8192)
                            if reply.endswith((b'\x1b\\', b'\x07')):
                                break
                    response = base64.b64encode(reply).decode()
                finally:
                    termios.tcsetattr(0, termios.TCSANOW, settings)
            else:
                for i in range(0, len(raw), 65536):
                    os.write(1, raw[i:i+65536])
                response = 'ok'
            conn.sendall(json.dumps(response).encode() + b'\n')


def main(binary):
    with tempfile.TemporaryDirectory(prefix='osc7501-native-') as directory:
        root = Path(directory)
        ipc, control = root/'alacritty.sock', root/'child.sock'
        output = open(root/'alacritty.log', 'w')
        process = subprocess.Popen([binary, '--config-file', '/dev/null', '--socket', str(ipc),
            '--title', 'OSC7501 acceptance', '-o', 'window.startup_mode="Windowed"',
            '-e', sys.executable, __file__, '--child', str(control)], stdout=output, stderr=output)
        def snapshot():
            for _ in range(20):
                result = subprocess.run([binary, 'msg', '--socket', str(ipc), 'program-status', '-w', '-1'], capture_output=True, text=True)
                if result.returncode == 0 and result.stdout.strip():
                    windows = json.loads(result.stdout)
                    if windows:
                        return windows[0]
                time.sleep(.03)
            raise AssertionError('IPC unavailable: ' + (root/'alacritty.log').read_text())
        def send(raw, query=False):
            with socket.socket(socket.AF_UNIX) as conn:
                conn.settimeout(10)
                conn.connect(str(control))
                conn.sendall(json.dumps({'bytes': base64.b64encode(raw).decode(), 'query': query}).encode()+b'\n')
                return json.loads(conn.makefile('rb').readline())
        def wait(predicate):
            deadline = time.monotonic()+3
            while time.monotonic() < deadline:
                state = snapshot()
                if predicate(state):
                    return state
                time.sleep(.03)
            raise AssertionError(state)
        try:
            for _ in range(100):
                if control.exists(): break
                if process.poll() is not None: raise AssertionError((root/'alacritty.log').read_text())
                time.sleep(.05)
            assert control.exists()
            # A status buffered by synchronized output must arrive after timeout,
            # even if the child sends no more bytes (no explicit ESU).
            send(b'\x1b[?2026h\x1b]7501;state=working:app=sync\x07')
            wait(lambda s: '[working] sync' in s['title'])
            send(b'\x1b]7501;state=clear\x07')
            wait(lambda s: not s['records'])
            send(b'\x1b]7501;state=working:app=build:progress=42\x1b\\')
            wait(lambda s: s['title'].endswith('[working 42%] build'))
            for ending in (b'\x1b\\', b'\x07'):
                reply = base64.b64decode(send(b'\x1b]7501;?'+ending, True))
                assert reply == b'\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth'+ending, repr(reply)
            send(b'\x1b]7501;state=blocked:id=worker:kind=permission:msg=QXBwcm92ZQ==\x07')
            wait(lambda s: '[permission] build' in s['title'] and len(s['records']) == 2)
            send(b'\x1b[?1049h\x1b[?1049l\x1b[!p')
            assert len(snapshot()['records']) == 2
            send(b'\x1b]7501;state=clear:id=worker\x07')
            wait(lambda s: len(s['records']) == 1 and 'working' in s['title'])
            before = snapshot()
            send(b'\x1b]7501;state=error:msg=____\x07\x1b]7501;state=error\x18')
            time.sleep(.15)
            assert snapshot()['generation'] == before['generation']
            send(b'\x1b]7501;state=done:id=result:app=tests\x07\x1b]133;A\x07')
            wait(lambda s: len(s['records']) == 1 and s['records'][0]['state'] == 'done')
            send(b'\x1bc')
            wait(lambda s: not s['records'] and s['title'] == 'OSC7501 acceptance')
            send(b'\x1b]7501;state=working:progress=41\x07')
            wait(lambda s: '41%' in s['title'])
            before = snapshot()
            send(b'\x1b]7501;state=working:progress=41\x07'*50000)
            time.sleep(.2)
            assert snapshot()['title_updates'] == before['title_updates']
            raw = b''.join(b'\x1b]7501;state=working:progress='+str(i%101).encode()+b'\x07' for i in range(50000))
            start = time.monotonic()
            send(raw)
            after = wait(lambda s: '4%' in s['title'])  # 49999 % 101 == 4
            elapsed = time.monotonic()-start
            assert after['title_updates']-before['title_updates'] <= int(elapsed*10)+4
            send(b'\x1b]7501;state=error:x='+b'x'*4_000_000+b'\x1b\\')
            time.sleep(.15)
            assert snapshot()['generation'] == after['generation']
            report = {'result':'PASS','changing_reports':50000,'duplicate_reports':50000,
                'seconds':round(elapsed,3),'native_title_updates':after['title_updates']-before['title_updates'],
                'final':after}
            print(json.dumps(report,ensure_ascii=False,indent=2))
        finally:
            process.terminate()
            try: process.wait(timeout=5)
            except subprocess.TimeoutExpired: process.kill(); process.wait()
            output.close()

if __name__ == '__main__':
    if sys.argv[1:2] == ['--child']: child(sys.argv[2])
    else: main(os.path.abspath(sys.argv[1] if len(sys.argv)>1 else 'target/release/alacritty'))
