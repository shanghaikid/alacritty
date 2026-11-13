#!/usr/bin/env python3
"""One-shot tmux replacement, after saved Codex turns finish."""
import fcntl
import json
import os
from pathlib import Path
import plistlib
import shlex
import shutil
import subprocess
import sys
import time

_lifecycle_cache = {}


def read_plan(path):
    plan = json.loads(Path(path).read_text())
    plan['root'] = str(Path(path).resolve().parent)
    return plan


def call(plan, server, *args, check=True):
    result = subprocess.run([plan['tmux'], '-S', plan[server + '_socket'], *args],
                            text=True, capture_output=True, timeout=15)
    if check and result.returncode:
        raise RuntimeError(shlex.join(args) + ': ' + result.stderr.strip())
    return result.stdout.strip()


def status(plan, state, **fields):
    root = Path(plan['root'])
    data = dict(state=state, time=time.time(), **fields)
    temporary = root / 'status.tmp'
    temporary.write_text(json.dumps(data, indent=2, ensure_ascii=False) + '\n')
    temporary.replace(root / 'status.json')
    print(json.dumps(data, ensure_ascii=False), flush=True)


def pane(plan, pane_id):
    root = Path(plan['root'])
    spec = next(p for w in plan['windows'] for p in w['panes'] if p['id'] == pane_id)
    os.chdir(spec['cwd'])
    history = root / ('history-' + pane_id + '.txt')
    if history.exists():
        sys.stdout.buffer.write(history.read_bytes())
        sys.stdout.flush()
    (root / ('ready-' + pane_id)).write_text(str(os.getpid()))
    while not (root / 'release').exists():
        if (root / 'abort').exists():
            os.execv('/bin/zsh', ['-zsh'])
        time.sleep(.05)
    if spec.get('session_id'):
        command = shlex.join([plan['codex'], 'resume', spec['session_id'], '-C', spec['cwd']])
        os.execv('/bin/zsh', ['zsh', '-lic', 'exec ' + command])
    os.execv('/bin/zsh', ['-zsh'])


def stage(plan, path):
    root = Path(plan['root'])
    if Path(plan['new_socket']).exists():
        raise RuntimeError('Destination socket already exists')
    script = root / 'upgrade.py'
    if script.resolve() != Path(__file__).resolve():
        shutil.copy2(__file__, script)
        script.chmod(0o700)
    mapping = {}
    for index, window in enumerate(plan['windows']):
        first = window['panes'][0]
        command = shlex.join([plan['python'], str(root / 'upgrade.py'), 'pane', str(path), first['id']])
        target = plan['session'] + ':' + str(window['index'])
        if index == 0:
            new = call(plan, 'new', '-f', plan['config'], 'new-session', '-d', '-P', '-F', '#{pane_id}',
                       '-s', plan['session'], '-n', window['name'], '-x', str(window['width']),
                       '-y', str(window['height'] + plan.get('status_lines', 0)),
                       '-c', first['cwd'], command)
            actual_index = call(plan, 'new', 'display-message', '-p', '-t', new, '#{window_index}')
            if int(actual_index) != window['index']:
                call(plan, 'new', 'move-window', '-s', plan['session'] + ':' + actual_index, '-t', target)
        else:
            new = call(plan, 'new', 'new-window', '-d', '-P', '-F', '#{pane_id}', '-t', target,
                       '-n', window['name'], '-c', first['cwd'], command)
        mapping[first['id']] = new
        for spec in window['panes'][1:]:
            command = shlex.join([plan['python'], str(root / 'upgrade.py'), 'pane', str(path), spec['id']])
            new = call(plan, 'new', 'split-window', '-d', '-P', '-F', '#{pane_id}', '-t', target,
                       '-c', spec['cwd'], command)
            mapping[spec['id']] = new
        call(plan, 'new', 'select-layout', '-t', target, window['layout'])
        active = next(p for p in window['panes'] if p['active'])
        call(plan, 'new', 'select-pane', '-t', mapping[active['id']])
    call(plan, 'new', 'select-window', '-t', plan['session'] + ':' + str(plan['active_window']))
    (root / 'mapping.json').write_text(json.dumps(mapping, indent=2) + '\n')
    deadline = time.monotonic() + 15
    while not all((root / ('ready-' + key)).exists() for key in mapping):
        if time.monotonic() > deadline:
            raise RuntimeError('Staged pane did not become ready')
        time.sleep(.05)
    observed = call(plan, 'new', 'list-panes', '-s', '-t', plan['session'], '-F', '#{pane_id}').splitlines()
    if set(observed) != set(mapping.values()):
        raise RuntimeError('Staged pane inventory differs')
    status(plan, 'staged', new_pid=call(plan, 'new', 'display-message', '-p', '#{pid}'),
           panes=len(mapping), waiting_for=plan.get('watch', []))


def lifecycle(path):
    metadata = Path(path).stat()
    cached = _lifecycle_cache.get(path)
    if cached and cached[0] == metadata.st_ino and metadata.st_size >= cached[1]:
        _, offset, previous, pending = cached
    else:
        offset, previous, pending = 0, (None, 'unknown'), b''
    with Path(path).open('rb') as stream:
        stream.seek(offset)
        data = pending + stream.read()
        offset = stream.tell()
    lines = data.split(b'\n')
    pending = lines.pop()
    for line in lines:
        try:
            record = json.loads(line)
        except ValueError:
            continue
        payload = record.get('payload', {})
        if record.get('type') == 'event_msg' and payload.get('type') in (
                'task_started', 'task_complete', 'turn_aborted'):
            previous = record.get('timestamp'), payload['type']
    _lifecycle_cache[path] = metadata.st_ino, offset, previous, pending
    return previous


def wait_paths(plan):
    paths = plan.get('wait_for', plan.get('watch', []))
    if set(paths) != set(plan.get('watch', [])) and not plan.get('interruption_authorized'):
        raise RuntimeError('Interrupting another conversation was not authorized')
    return paths


def wait_idle(plan):
    deadline = time.monotonic() + 900
    last = None
    since = None
    while time.monotonic() < deadline:
        states = [lifecycle(path) for path in wait_paths(plan)]
        idle = all(kind in ('task_complete', 'turn_aborted') for _, kind in states)
        if idle:
            if states != last:
                last, since = states, time.monotonic()
            elif time.monotonic() - since >= 1.5:
                return
        else:
            last, since = None, None
        time.sleep(.25)
    raise RuntimeError('Tasks did not become idle; original server was left intact')


def atomic_write(path, data):
    path = Path(path)
    temporary = path.with_name('.' + path.name + '.tmux-upgrade')
    with temporary.open('wb') as output:
        os.fchmod(output.fileno(), 0o700)
        output.write(data)
    temporary.replace(path)


def restored(plan, mapping):
    """Check all requested threads across the restored CLI process trees.

    Several frontends can share one app-server, so a thread's open rollout
    need not belong to the frontend displaying that thread.
    """
    processes = set()
    expected_threads = set()
    for window in plan['windows']:
        for spec in window['panes']:
            target = mapping[spec['id']]
            command = call(plan, 'new', 'display-message', '-p', '-t', target, '#{pane_current_command}')
            expected = 'codex' if spec.get('session_id') else 'zsh'
            if command != expected:
                return False
            if spec.get('rollout'):
                expected_threads.add(spec['session_id'])
                processes.add(int(call(plan, 'new', 'display-message', '-p', '-t', target, '#{pane_pid}')))
    if not expected_threads:
        return True
    rows = subprocess.check_output(['ps', '-axo', 'pid=,ppid=,comm='], text=True)
    parsed = [line.strip().split(None, 2) for line in rows.splitlines()]
    while True:
        found = {int(row[0]) for row in parsed if len(row) == 3 and int(row[1]) in processes}
        if found <= processes:
            break
        processes |= found
    opened = subprocess.run(['lsof', '-p', ','.join(map(str, processes)), '-Fn'],
                            text=True, capture_output=True, timeout=10).stdout
    rollouts = [line[1:] for line in opened.splitlines()
                if line.startswith('n') and line.endswith('.jsonl')]
    return all(any(thread in path for path in rollouts) for thread in expected_threads)


def launch_agent_config(plan, path, label):
    root = Path(plan['root'])
    return {
        'Label': label,
        'ProgramArguments': [plan['python'], str(root / 'upgrade.py'), 'run', str(Path(path).resolve())],
        'RunAtLoad': True,
        'KeepAlive': False,
        'StandardOutPath': str(root / 'upgrade.log'),
        'StandardErrorPath': str(root / 'upgrade.log'),
        'EnvironmentVariables': {'HOME': str(Path.home()), 'PATH': os.environ['PATH']},
    }


def arm(plan, path):
    root = Path(plan['root'])
    if json.loads((root / 'status.json').read_text())['state'] != 'staged':
        raise RuntimeError('Only a staged migration can be armed')
    label = 'org.alacritty.tmux-upgrade.' + str(time.time_ns())
    agent = root / 'cutover.plist'
    agent.write_bytes(plistlib.dumps(launch_agent_config(plan, path, label)))
    agent.chmod(0o600)
    subprocess.run(['launchctl', 'bootstrap', 'gui/' + str(os.getuid()), str(agent)], check=True)
    (root / 'job-label.txt').write_text(label + '\n')
    print('Armed one-shot job: ' + label, flush=True)


def run(plan):
    root = Path(plan['root'])
    with (root / 'upgrade.lock').open('a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print('Cutover already running; leaving it unchanged.', flush=True)
            return
        previous = json.loads((root / 'status.json').read_text())
        if previous['state'] in ('complete', 'failed'):
            print('Cutover already finished; preserving its status.', flush=True)
            return
        _run_cutover(plan)


def _run_cutover(plan):
    root = Path(plan['root'])
    changed_launcher = False
    killed_old = False
    try:
        status(plan, 'waiting_for_idle')
        wait_idle(plan)
        if call(plan, 'old', 'display-message', '-p', '#{pid}') != str(plan['old_pid']):
            raise RuntimeError('Original server PID changed')
        before = call(plan, 'old', 'list-panes', '-s', '-t', plan['session'],
                      '-F', '#{pane_id}|#{pane_pid}|#{pane_current_path}')
        if before != plan['inventory']:
            raise RuntimeError('Original panes changed; refusing stale migration')
        for window in plan['windows']:
            for spec in window['panes']:
                if not spec.get('codex_pid'):
                    continue
                parent = subprocess.check_output(
                    ['ps', '-p', str(spec['codex_pid']), '-o', 'ppid='], text=True).strip()
                if parent != str(spec['pid']):
                    raise RuntimeError('A Codex process changed after backup')
                opened = subprocess.check_output(['lsof', '-p', str(spec['codex_pid']), '-Fn'], text=True)
                rollouts = {line[1:] for line in opened.splitlines()
                            if line.startswith('n' + str(Path.home() / '.codex/sessions/'))
                            and line.endswith('.jsonl')}
                if rollouts - set(plan.get('watch', [])):
                    raise RuntimeError('A new Codex conversation opened after backup')
        if not all(lifecycle(path)[1] in ('task_complete', 'turn_aborted') for path in wait_paths(plan)):
            raise RuntimeError('A task started during the final check')
        mapping = json.loads((root / 'mapping.json').read_text())
        for window in plan['windows']:
            rows = call(plan, 'old', 'list-panes', '-t', plan['session'] + ':' + str(window['index']),
                        '-F', '#{pane_id}|#{pane_active}').splitlines()
            active = next(row.split('|')[0] for row in rows if row.endswith('|1'))
            call(plan, 'new', 'select-pane', '-t', mapping[active])
        rows = call(plan, 'old', 'list-windows', '-t', plan['session'],
                    '-F', '#{window_index}|#{window_active}').splitlines()
        active = next(row.split('|')[0] for row in rows if row.endswith('|1'))
        call(plan, 'new', 'select-window', '-t', plan['session'] + ':' + active)
        for window in plan['windows']:
            for spec in window['panes']:
                data = call(plan, 'old', 'capture-pane', '-p', '-S', '-', '-t', spec['id'])
                (root / ('final-history-' + spec['id'] + '.txt')).write_text(data + '\n')
        if plan.get('launcher'):
            launcher = Path(plan['launcher'])
            original = (root / 'start-tmux.zsh').read_bytes()
            if launcher.read_bytes() != original:
                raise RuntimeError('Launcher changed since backup')
            updated = original.replace(('-L ' + plan['old_name']).encode(),
                                       ('-L ' + plan['new_name']).encode())
            if updated == original:
                raise RuntimeError('Launcher did not contain the original server name')
            atomic_write(launcher, updated)
            subprocess.run(['/bin/zsh', '-n', str(launcher)], check=True)
            changed_launcher = True
        status(plan, 'switching_client')
        clients = call(plan, 'old', 'list-clients', '-F', '#{client_tty}').splitlines()
        if not clients:
            raise RuntimeError('No original terminal client to switch')
        command = 'exec ' + shlex.join([plan['tmux'], '-S', plan['new_socket'],
                                       'attach-session', '-t', plan['session']])
        for client in clients:
            call(plan, 'old', 'detach-client', '-t', client, '-E', command)
        deadline = time.monotonic() + 10
        while not call(plan, 'new', 'list-clients', '-F', '#{client_pid}'):
            if time.monotonic() > deadline:
                raise RuntimeError('Terminal client did not attach to the staged server')
            time.sleep(.05)
        call(plan, 'old', 'kill-server')
        killed_old = True
        (root / 'release').touch()
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if restored(plan, mapping):
                break
            time.sleep(.2)
        else:
            raise RuntimeError('A restored pane did not reach its expected command')
        new_pid = call(plan, 'new', 'display-message', '-p', '#{pid}')
        if new_pid == str(plan['old_pid']):
            raise RuntimeError('Server did not change')
        call(plan, 'new', 'display-message', 'tmux 已升级，原会话已恢复')
        status(plan, 'complete', new_pid=new_pid, socket=plan['new_socket'],
               restored_codex=sum(bool(p.get('session_id')) for w in plan['windows'] for p in w['panes']))
    except BaseException as error:
        if changed_launcher and not killed_old:
            atomic_write(plan['launcher'], (root / 'start-tmux.zsh').read_bytes())
        (root / ('release' if killed_old else 'abort')).touch()
        status(plan, 'failed', error=str(error), original_server_stopped=killed_old)
        raise


if __name__ == '__main__':
    mode, path = sys.argv[1:3]
    plan = read_plan(path)
    if mode == 'stage':
        stage(plan, path)
    elif mode == 'run':
        run(plan)
    elif mode == 'arm':
        arm(plan, path)
    elif mode == 'verify':
        mapping = json.loads((Path(plan['root']) / 'mapping.json').read_text())
        raise SystemExit(0 if restored(plan, mapping) else 1)
    elif mode == 'pane':
        pane(plan, sys.argv[3])
    else:
        raise SystemExit('Unknown mode')
