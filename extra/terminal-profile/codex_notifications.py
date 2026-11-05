#!/usr/bin/env python3
"""Local Codex 0.166 notification adapter. No prompts/tool output are persisted.

Hooks start one observer per terminal session. Its only Codex API operation is
thread/goal/get; standalone TUI sessions are not subscribed in the shared daemon.
The versioned rollout adapter reads lifecycle/structured question events only.
"""
import base64
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import socket
import sqlite3
import stat
import struct
import subprocess
import sys
import time

QUESTION = re.compile(r'(^|[._])request_user_input(_async)?$')
SAFE_ID = re.compile(r'^[a-zA-Z0-9_.:-]{1,160}$')
DELAY = 2.0
MAX_LINE = 8 * 1024 * 1024


class GoalClient:
    """Minimal bounded RFC6455 client for Codex's local Unix socket."""
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.settimeout(1.0)
        self.counter = 0
        try:
            self.sock.connect(str(path))
            key = base64.b64encode(os.urandom(16)).decode()
            self.sock.sendall(('GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n'
                              'Connection: Upgrade\r\nSec-WebSocket-Key: ' + key +
                              '\r\nSec-WebSocket-Version: 13\r\n\r\n').encode())
            header = bytearray()
            while not header.endswith(b'\r\n\r\n') and len(header) < 16384:
                header.extend(self.read(1))
            accept = base64.b64encode(hashlib.sha1((key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest())
            if not header.startswith(b'HTTP/1.1 101 ') or accept.lower() not in header.lower():
                raise ValueError('Invalid WebSocket handshake')
            self.call('initialize', {'clientInfo': {'name': 'alacritty_notifications', 'version': '1'},
                                     'capabilities': {'experimentalApi': True}})
            self.send({'method': 'initialized'})
        except Exception:
            self.close()
            raise

    def close(self):
        self.sock.close()

    def read(self, n):
        data = bytearray()
        while len(data) < n:
            part = self.sock.recv(n - len(data))
            if not part:
                raise EOFError('Codex disconnected')
            data.extend(part)
        return bytes(data)

    def frame(self, data, opcode=1):
        mask = os.urandom(4)
        n = len(data)
        header = bytes([128 | opcode, 128 | n]) if n < 126 else bytes([128 | opcode, 254]) + struct.pack('!H', n)
        self.sock.sendall(header + mask + bytes(v ^ mask[i % 4] for i, v in enumerate(data)))

    def send(self, value):
        self.frame(json.dumps(value, separators=(',', ':')).encode())

    def receive(self):
        data = bytearray()
        while True:
            a, b = self.read(2)
            n = b & 127
            if n == 126:
                n = struct.unpack('!H', self.read(2))[0]
            elif n == 127:
                n = struct.unpack('!Q', self.read(8))[0]
            if b & 128 or n + len(data) > MAX_LINE:
                raise ValueError('Invalid WebSocket frame')
            part = self.read(n)
            opcode = a & 15
            if opcode == 8:
                raise EOFError('Codex disconnected')
            if opcode == 9:
                self.frame(part, 10)
                continue
            if opcode == 10:
                continue
            if opcode not in (0, 1):
                raise ValueError('Expected text frame')
            data.extend(part)
            if a & 128:
                return json.loads(data)

    def call(self, method, params):
        self.counter += 1
        self.send({'id': self.counter, 'method': method, 'params': params})
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            msg = self.receive()
            if msg.get('id') == self.counter:
                if 'error' in msg:
                    raise ValueError('Codex API error: ' + str(msg['error'].get('code')))
                return msg['result']
        raise TimeoutError('Codex query timeout')


class State:
    """Pure lifecycle policy, with independent pending questions and turn state."""
    def __init__(self):
        self.turn = ''
        self.stopped = None
        self.goal = 'unknown'
        self.goal_id = ''
        self.goal_updated = 0
        self.turn_started = 0
        self.automatic_approval = False
        self.questions = {}
        self.permissions = {}
        self.closed = False
        self.cancelled = False

    def hook(self, event, now):
        name = event.get('event')
        key = event.get('key', '')
        if name == 'PermissionRequest':
            self.permissions[key] = now
        elif name == 'PostToolUse':
            self.permissions.pop(key, None)
        elif name in ('Interrupt', 'SessionEnd'):
            self.questions.clear()
            self.permissions.clear()
            self.stopped = None
            self.cancelled = True
            self.closed = name == 'SessionEnd'
        elif name == 'UserPromptSubmit':
            self.stopped = None
            self.cancelled = False
            self.turn = event.get('turn_id') or self.turn
        elif name == 'Stop':
            turn = event.get('turn_id') or self.turn
            self.permissions.clear()
            # Stop is a candidate only. A later task_started cancels it.
            if self.stopped is None or turn != self.turn:
                self.stopped = now
            self.turn = turn

    def rollout(self, record, now):
        p = record.get('payload', {})
        kind = p.get('type')
        if record.get('type') == 'turn_context':
            self.automatic_approval = p.get('approvals_reviewer') in ('auto_review', 'guardian_subagent')
            return
        if record.get('type') == 'event_msg':
            if kind == 'task_started':
                self.turn = p.get('turn_id', '')
                self.turn_started = p.get('started_at', 0)
                self.stopped = None
                self.cancelled = False
                self.permissions.clear()
            elif kind == 'task_complete':
                self.hook({'event': 'Stop', 'turn_id': p.get('turn_id')}, now)
            elif kind in ('turn_aborted', 'task_interrupted'):
                self.hook({'event': 'Interrupt'}, now)
            elif kind == 'thread_goal_updated':
                self.set_goal(p.get('goal'))
                # System budget/usage stops can happen without a final model
                # reply or Stop hook. These are never reported as success.
                if self.goal in ('budgetLimited', 'usageLimited'):
                    self.stopped = now
            return
        if record.get('type') != 'response_item':
            return
        call_id = p.get('call_id', '')
        if kind in ('function_call', 'custom_tool_call') and QUESTION.search(p.get('name', '')):
            try:
                args = json.loads(p.get('arguments') or p.get('input') or '{}')
                count = len(args.get('questions', []))
            except (TypeError, ValueError):
                count = 1
            self.questions[call_id] = {'async': p['name'].endswith('_async'),
                                       'remaining': set(range(max(1, count))), 'at': now}
        elif kind in ('function_call_output', 'custom_tool_call_output') and call_id in self.questions:
            question = self.questions[call_id]
            try:
                output = p.get('output', '{}')
                output = json.loads(output) if isinstance(output, str) else output
            except ValueError:
                output = {}
            if not question['async'] or (isinstance(output, dict) and output.get('accepted') is False):
                self.questions.pop(call_id, None)
        elif kind == 'message' and p.get('role') == 'user':
            text = ''.join(c.get('text', '') for c in p.get('content', []) if isinstance(c, dict))
            match = re.fullmatch(r'\s*<send_user_message_question_reply>\s*(.*?)\s*</send_user_message_question_reply>\s*', text, re.S)
            if match:
                try:
                    for answer in json.loads(match[1]):
                        name, call, index = json.loads(answer['questionItemId'])
                        if name == 'request_user_input_async' and call in self.questions:
                            self.questions[call]['remaining'].discard(index)
                            if not self.questions[call]['remaining']:
                                del self.questions[call]
                except (ValueError, KeyError, TypeError):
                    pass
            else:
                meta = p.get('internal_chat_message_metadata_passthrough') or {}
                provenance = [x.get('provenance', {}).get('type') for x in meta.get('content_item_metadata', [])]
                # A real new user task supersedes old questions. Automatic goal
                # continuation/context injection must never do so.
                if 'user' in provenance and not text.lstrip().startswith('<codex_internal_context'):
                    self.questions.clear()
                    self.permissions.clear()
                    self.stopped = None

    def set_goal(self, goal):
        self.goal = goal['status'] if isinstance(goal, dict) else None
        self.goal_id = str(goal.get('createdAt', '')) if isinstance(goal, dict) else ''
        self.goal_updated = goal.get('updatedAt', 0) if isinstance(goal, dict) else 0

    def desired(self, now):
        if self.cancelled:
            return None
        if self.questions:
            # One notification for the set; replying to one of several questions
            # does not issue another alert for the remaining questions.
            key = max(self.questions, key=lambda k: self.questions[k]['at'])
            return 'question', 'question:' + key
        if self.permissions and not self.automatic_approval:
            key = max(self.permissions, key=self.permissions.get)
            if now - self.permissions[key] >= 1.0:
                return 'permission', 'permission:' + key
            return None
        if self.stopped is None or now - self.stopped < DELAY:
            return None
        if self.goal in ('active', 'unknown'):
            return None
        if self.goal in ('paused',):
            return None
        if self.goal == 'complete':
            if self.goal_updated and self.goal_updated < self.turn_started:
                return 'done', 'turn:' + self.turn
            return 'goal', 'goal:' + self.goal_id
        if self.goal in ('blocked', 'budgetLimited', 'usageLimited'):
            kind = 'blocked' if self.goal == 'blocked' else 'limited'
            return kind, 'goal:' + self.goal_id + ':' + self.goal
        return 'done', 'turn:' + self.turn


def persisted_goal(home, session):
    """Read-only 0.166 fallback when the optional shared daemon is unavailable.

    Validate the schema and fail closed on upgrades; never interpret a database
    error as 'no goal'. Do not read objectives or any conversation text.
    """
    path = Path(home) / 'goals_1.sqlite'
    with sqlite3.connect(path.as_uri() + '?mode=ro', uri=True, timeout=0.2) as db:
        columns = {row[1] for row in db.execute('PRAGMA table_info(thread_goals)')}
        if not {'thread_id', 'status', 'created_at_ms', 'updated_at_ms'} <= columns:
            raise ValueError('Unsupported Codex goal schema')
        row = db.execute('SELECT status, created_at_ms, updated_at_ms FROM thread_goals WHERE thread_id = ?', (session,)).fetchone()
    if row is None:
        return None
    status = {'usage_limited': 'usageLimited', 'budget_limited': 'budgetLimited'}.get(row[0], row[0])
    if status not in ('active', 'paused', 'complete', 'blocked', 'usageLimited', 'budgetLimited'):
        raise ValueError('Unknown goal status')
    return {'status': status, 'createdAt': row[1] // 1000, 'updatedAt': row[2] // 1000}


def ipc(path, message):
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(0.5)
        s.connect(str(path))
        s.sendall(json.dumps(message, separators=(',', ':')).encode() + b'\n')
        s.shutdown(socket.SHUT_WR)
        data = bytearray()
        while len(data) <= 1024 * 1024:
            part = s.recv(65536)
            if not part:
                break
            data.extend(part)
        return json.loads(data) if data else None


def command(args):
    return subprocess.run(args, capture_output=True, text=True, timeout=0.6, check=True).stdout.strip()


def routes(meta):
    """Resolve live tmux clients, not stale env inherited by a long-lived server."""
    sockets = {meta['socket']}
    sockets.update(str(p) for p in Path(meta['socket']).parent.glob('Alacritty-*.sock'))
    windows = []
    for path in sockets:
        try:
            reply = ipc(path, {'ProgramStatus': {'window_id': -1}})
            for w in json.loads(reply['ProgramStatus']):
                if 'codex_notifications_enabled' not in w:
                    continue  # An older running Alacritty cannot deliver this IPC.
                w['socket'] = path
                windows.append(w)
        except (OSError, ValueError, KeyError, TypeError):
            continue
    if not meta.get('pane') or not meta.get('tmux_socket'):
        return [(w, '', bool(w.get('focused'))) for w in windows if w['window_id'] == meta['window_id'] and w['socket'] == meta['socket']]
    prefix = [meta['tmux_bin'], '-S', meta['tmux_socket']]
    try:
        panes = command(prefix + ['list-panes', '-a', '-F', '#{pane_id}\t#{session_id}\t#{window_active}\t#{pane_active}']).splitlines()
        target_sessions = {}
        for line in panes:
            pane, session, active_window, active_pane = line.split('\t')
            if pane == meta['pane']:
                target_sessions[session] = active_window == active_pane == '1'
        clients = command(prefix + ['list-clients', '-F', '#{client_tty}\t#{session_id}']).splitlines()
        ttys = {}
        pids = [str(w['shell_pid']) for w in windows if w.get('shell_pid')]
        if pids:
            for line in command(['/bin/ps', '-p', ','.join(pids), '-o', 'pid=,tty=']).splitlines():
                pid, tty = line.split()
                ttys[int(pid)] = '/dev/' + tty
        result = []
        for w in windows:
            for line in clients:
                tty, session = line.split('\t')
                if ttys.get(w.get('shell_pid')) == tty and session in target_sessions:
                    result.append((w, tty, bool(w.get('focused')) and target_sessions[session]))
        return result
    except (OSError, ValueError, subprocess.SubprocessError):
        return []


def private_dir(path):
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise OSError('Notification state directory must be private and owned by this user')


def hook(payload, codex_pid, tty):
    """Fast advisory hook; never query services or wait for notifications here."""
    sock = os.environ.get('ALACRITTY_SOCKET')
    session = payload.get('session_id', '')
    if payload.get('agent_id') and payload['agent_id'] != session:
        return
    transcript = payload.get('transcript_path')
    if not sock or not SAFE_ID.fullmatch(session) or not transcript:
        return
    codex_home = Path(os.environ.get('CODEX_HOME', Path.home() / '.codex'))
    root = Path(os.environ.get('XDG_STATE_HOME', Path.home() / '.local/state')) / 'codex-notifications'
    private_dir(root)
    directory = root / session
    private_dir(directory)
    arguments = payload.get('tool_input')
    if isinstance(arguments, dict):
        arguments = {k: v for k, v in arguments.items() if k != 'description'}
    key = hashlib.sha256(json.dumps([payload.get('tool_name'), arguments], sort_keys=True).encode()).hexdigest()
    event = {'event': payload.get('hook_event_name'), 'key': key, 'turn_id': payload.get('turn_id', '')}
    fd = os.open(directory / 'events.jsonl', os.O_WRONLY | os.O_APPEND | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        event_offset = os.lseek(fd, 0, os.SEEK_END)
        os.write(fd, (json.dumps(event) + '\n').encode())
    finally:
        os.close(fd)
    lock = os.open(directory / 'worker.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return
        # One worker holds this inherited lock until the Codex process exits.
        tmux = os.environ.get('TMUX', '').split(',')[0]
        app = Path('/Applications/Alacritty.app/Contents/MacOS')
        meta = {'pid': codex_pid, 'tty': tty, 'session': session, 'transcript': transcript,
                'event_offset': event_offset,
                'offset': Path(transcript).stat().st_size, 'socket': sock,
                'window_id': int(os.environ.get('ALACRITTY_WINDOW_ID', '0')),
                'tmux_socket': tmux, 'pane': os.environ.get('TMUX_PANE', ''),
                'tmux_bin': str(app / 'tmux'), 'codex_home': str(codex_home),
                'project': Path(payload.get('cwd', '')).name[:80]}
        (directory / 'meta.json').write_text(json.dumps(meta))
        subprocess.Popen([sys.executable, str(Path(__file__).resolve()), '--watch', str(directory), str(lock)],
                         pass_fds=(lock,), stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                         stderr=subprocess.DEVNULL, start_new_session=True)
    finally:
        os.close(lock)


def lines(stream):
    """Never consume a partially written record; bound individual records."""
    while True:
        start = stream.tell()
        line = stream.readline(MAX_LINE + 1)
        if not line:
            return
        if len(line) > MAX_LINE:
            while line and not line.endswith(b'\n'):
                line = stream.readline(MAX_LINE + 1)
            continue
        if not line.endswith(b'\n'):
            stream.seek(start)
            return
        try:
            yield json.loads(line)
        except (ValueError, UnicodeDecodeError):
            continue


def watch(directory, lock_fd):
    meta = json.loads((directory / 'meta.json').read_text())
    state = State()
    client = None
    last_query = -10
    queried_stop = None
    announced = set()
    active = None
    active_route = None
    previous_snapshot = None
    last_routes = -10
    cached_routes = []
    previous_desired = None
    delivery_seen = False
    with (directory / 'events.jsonl').open('rb') as events, Path(meta['transcript']).open('rb') as transcript:
        events.seek(meta.get('event_offset', 0))
        # Bootstrap only recent structured state. A PreToolUse hook may run just
        # after its function_call was appended, before this observer existed.
        start = max(0, meta['offset'] - MAX_LINE)
        transcript.seek(start)
        if start:
            transcript.readline(MAX_LINE + 1)
        for record in lines(transcript):
            state.rollout(record, time.monotonic())
        # Never replay a historical completion merely on resume/startup.
        state.stopped = None
        try:
            while True:
                now = time.monotonic()
                try:
                    os.kill(meta['pid'], 0)
                except ProcessLookupError:
                    break
                for e in lines(events):
                    state.hook(e, now)
                for record in lines(transcript):
                    state.rollout(record, now)
                if state.closed:
                    break
                # Formal persisted goal API works for standalone TUI threads too.
                # Only poll at completion boundaries; errors never imply no goal.
                if state.stopped is not None and now - last_query >= 2 and (state.goal in ('active', 'unknown') or queried_stop != state.stopped):
                    last_query = now
                    queried_stop = state.stopped
                    try:
                        if client is None:
                            client = GoalClient(Path(meta['codex_home']) / 'app-server-control/app-server-control.sock')
                        state.set_goal(client.call('thread/goal/get', {'threadId': meta['session']})['goal'])
                    except (OSError, EOFError, ValueError, KeyError):
                        if client:
                            client.close()
                        client = None
                        try:
                            state.set_goal(persisted_goal(meta['codex_home'], meta['session']))
                        except (OSError, sqlite3.Error, ValueError):
                            state.goal = 'unknown'
                desired = state.desired(now)
                # Route checks are necessary only while something needs attention.
                needs_routes = active or (desired and desired not in announced)
                if needs_routes and (desired != previous_desired or now - last_routes >= 1):
                    cached_routes = routes(meta)
                    last_routes = now
                    if active_route:
                        try:
                            ids = json.loads(ipc(active_route[0]['socket'], 'AgentNotifications')['ProgramStatus'])
                            exists = 'alacritty-codex-' + meta['session'] in ids
                            if exists:
                                delivery_seen = True
                            elif delivery_seen:
                                # A manual dismissal/read is an acknowledgement,
                                # not a reason to send the same event again.
                                active = active_route = None
                        except (OSError, ValueError, KeyError, TypeError):
                            pass
                current_routes = cached_routes if needs_routes else []
                previous_desired = desired
                visible = any(r[2] for r in current_routes)
                muted = bool(current_routes) and not any(r[0].get('codex_notifications_enabled', True) for r in current_routes)
                if active and (desired != active or visible or muted):
                    clear(meta, active_route)
                    active = active_route = None
                if desired and desired not in announced and current_routes:
                    if visible or muted:
                        announced.add(desired)
                    else:
                        window, tty, _ = current_routes[0]
                        message = notification(meta, window, tty, *desired)
                        try:
                            ipc(window['socket'], {'AgentNotify': message})
                            active, active_route = desired, (window, tty)
                            delivery_seen = False
                            announced.add(desired)
                        except (OSError, ValueError):
                            pass
                # Small local diagnostics, deliberately excluding conversation content.
                snapshot = {'pid': os.getpid(), 'session': meta['session'], 'turn': state.turn,
                            'goal': state.goal, 'questions': len(state.questions),
                            'permissions': len(state.permissions), 'active': active,
                            'announced': len(announced)}
                if snapshot != previous_snapshot:
                    (directory / 'status.json').write_text(json.dumps(snapshot))
                    previous_snapshot = snapshot
                time.sleep(0.5 if desired or active else 0.25)
        finally:
            if active_route:
                clear(meta, active_route)
            if client:
                client.close()
            os.close(lock_fd)


def notification(meta, window, tty, kind, event):
    # Hash long tool/call identifiers into bounded routing keys.
    return {'session': meta['session'], 'event': hashlib.sha256(event.encode()).hexdigest(),
            'kind': kind, 'project': ''.join(c for c in meta['project'] if c.isprintable()),
            'window_id': window['window_id'], 'tmux_socket': meta.get('tmux_socket', ''),
            'pane': meta.get('pane', ''), 'client_tty': tty}


def clear(meta, route):
    if not route:
        return
    window, tty = route
    try:
        ipc(window['socket'], {'AgentNotify': notification(meta, window, tty, 'clear', '')})
    except (OSError, ValueError):
        pass


if __name__ == '__main__' and len(sys.argv) == 4 and sys.argv[1] == '--watch':
    try:
        watch(Path(sys.argv[2]), int(sys.argv[3]))
    except Exception as error:
        # No transcript/tool text or tracebacks in diagnostics.
        (Path(sys.argv[2]) / 'error.txt').write_text(type(error).__name__ + '\n')
