#!/usr/bin/env python3
"""Exercise cutover with fake Codex frontends sharing one history-owning backend."""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import runpy
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time

binary = os.environ.get('TEST_TMUX', str(Path.home() / '.local/bin/tmux'))
source = Path(__file__).with_name('tmux-upgrade.py')
use_launchd = '--launchd' in sys.argv
with tempfile.TemporaryDirectory(prefix='tmux-cutover-test-') as directory:
    root = Path(directory).resolve()
    (root / 'home').mkdir()
    env = dict(os.environ, HOME=str(root / 'home'))
    env.pop('TMUX', None)
    env.pop('TMUX_PANE', None)
    old = [binary, '-S', str(root / 'old.sock')]
    new = [binary, '-S', str(root / 'new.sock')]

    def call(command, *args):
        return subprocess.check_output(command + list(args), text=True, stderr=subprocess.PIPE,
                                       env=env, timeout=10).strip()

    config = root / 'config'
    config.write_text('set -g status on\nset -g default-shell /bin/zsh\n'
                      'set -g base-index 1\nset -g pane-base-index 1\n')
    shutil.copy2(source, root / 'upgrade.py')
    (root / 'fake.c').write_text(r'''
#include <stdio.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
    if (argc != 5 || strcmp(argv[1], "resume") || strcmp(argv[3], "-C")) return 3;
    if (!strcmp(argv[2], "00000000-0000-0000-0000-000000000001")) {
        const char *ids[] = {
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000002",
            "00000000-0000-0000-0000-000000000004"
        };
        for (int i = 0; i < 3; i++) {
            char path[4096];
            snprintf(path, sizeof path, "%s/rollout-%s.jsonl", argv[4], ids[i]);
            if (fopen(path, "r") == NULL) return 5;
        }
    }
    printf("RESUMED %s\n", argv[2]);
    fflush(stdout);
    for (;;) pause();
}
''')
    subprocess.run(['cc', str(root / 'fake.c'), '-o', str(root / 'codex')], check=True)
    client = master = None
    launchd_job = None
    output = bytearray()
    stopped = threading.Event()

    def drain():
        while not stopped.is_set():
            try:
                if select.select([master], [], [], .02)[0]:
                    data = os.read(master, 65536)
                    if not data:
                        return
                    output.extend(data)
            except OSError:
                return

    try:
        call(old, '-f', str(config), 'new-session', '-d', '-s', 'fixture', '-x', '80', '-y', '20',
             '-n', 'one', 'sleep 300')
        call(old, 'new-window', '-d', '-t', 'fixture:2', '-n', 'two', 'sleep 300')
        call(old, 'split-window', '-h', '-d', '-t', 'fixture:2', 'sleep 300')
        call(old, 'select-pane', '-t', 'fixture:2.2')
        call(old, 'new-window', '-d', '-t', 'fixture:3', '-n', 'three', 'sleep 300')
        call(old, 'select-window', '-t', 'fixture:3')
        client, master = pty.fork()
        if client == 0:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack('HHHH', 20, 80, 0, 0))
            os.execve(binary, old + ['attach-session', '-t', '=fixture'],
                      dict(env, TERM='xterm-256color'))
        reader = threading.Thread(target=drain, daemon=True)
        reader.start()
        time.sleep(.5)
        windows = []
        ordinal = 0
        for line in call(old, 'list-windows', '-t', 'fixture', '-F',
                         '#{window_index}|#{window_name}|#{window_layout}|#{window_width}|#{window_height}').splitlines():
            index, name, layout, width, height = line.split('|')
            panes = []
            for row in call(old, 'list-panes', '-t', 'fixture:' + index, '-F',
                            '#{pane_id}|#{pane_pid}|#{pane_active}|#{pane_left},#{pane_top},#{pane_width},#{pane_height}').splitlines():
                pid, process, active, geometry = row.split('|')
                ordinal += 1
                spec = dict(id=pid, pid=int(process), cwd=str(root), active=active == '1', geometry=geometry)
                if ordinal != 3:
                    spec['session_id'] = '00000000-0000-0000-0000-' + str(ordinal).zfill(12)
                    spec['rollout'] = str(root / ('rollout-' + spec['session_id'] + '.jsonl'))
                    Path(spec['rollout']).write_text('{}\n')
                panes.append(spec)
                (root / ('history-' + pid + '.txt')).write_text('SAVED TERMINAL HISTORY\n')
            windows.append(dict(index=int(index), name=name, layout=layout, width=int(width),
                                height=int(height), panes=panes))
        launcher = root / 'start.zsh'
        launcher.write_text('#!/bin/zsh\nexec tmux -L test-old attach\n')
        shutil.copy2(launcher, root / 'start-tmux.zsh')
        watch = root / 'thread.jsonl'
        watch.write_text(json.dumps(dict(timestamp='now', type='event_msg',
                                         payload=dict(type='task_complete'))) + '\n')
        goal = root / 'active-goal.jsonl'
        goal.write_text(json.dumps(dict(timestamp='now', type='event_msg',
                                       payload=dict(type='task_started'))) + '\n')
        plan = dict(tmux=binary, python=sys.executable, codex=str(root / 'codex'),
                    old_socket=str(root / 'old.sock'), new_socket=str(root / 'new.sock'),
                    old_pid=int(call(old, 'display-message', '-p', '#{pid}')),
                    old_name='test-old', new_name='test-new', session='fixture', active_window=3,
                    config=str(config), launcher=str(launcher), windows=windows,
                    watch=[str(watch), str(goal)], wait_for=[str(watch)],
                    interruption_authorized=True, status_lines=1,
                    inventory=call(old, 'list-panes', '-s', '-t', 'fixture', '-F',
                                   '#{pane_id}|#{pane_pid}|#{pane_current_path}'))
        path = root / 'plan.json'
        path.write_text(json.dumps(plan))
        module = runpy.run_path(str(source))
        agent = module['launch_agent_config'](dict(plan, root=str(root)), str(path), 'test-cutover')
        assert agent['RunAtLoad'] is True and agent['KeepAlive'] is False
        assert agent['ProgramArguments'][-2:] == ['run', str(path)]
        assert agent['StandardOutPath'] == str(root / 'upgrade.log')
        lifecycle_file = root / 'long-history.jsonl'
        lifecycle_file.write_text(json.dumps(dict(timestamp='start', type='event_msg',
                                                 payload=dict(type='task_started'))) + '\n' +
                                  json.dumps(dict(type='padding', payload='x' * 1500000)) + '\n')
        assert module['lifecycle'](str(lifecycle_file)) == ('start', 'task_started')
        with lifecycle_file.open('a') as stream:
            stream.write(json.dumps(dict(timestamp='end', type='event_msg',
                                         payload=dict(type='task_complete'))))
        assert module['lifecycle'](str(lifecycle_file)) == ('start', 'task_started')
        with lifecycle_file.open('a') as stream:
            stream.write('\n')
        assert module['lifecycle'](str(lifecycle_file)) == ('end', 'task_complete')
        try:
            module['wait_paths'](dict(watch=['a', 'b'], wait_for=['a']))
        except RuntimeError:
            pass
        else:
            raise AssertionError('Skipping active work needs explicit authorization')
        subprocess.run([sys.executable, str(root / 'upgrade.py'), 'stage', str(path)],
                       env=env, check=True)
        if use_launchd:
            subprocess.run([sys.executable, str(root / 'upgrade.py'), 'arm', str(path)],
                           env=env, check=True)
            launchd_job = 'gui/' + str(os.getuid()) + '/' + (root / 'job-label.txt').read_text().strip()
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                state = json.loads((root / 'status.json').read_text())['state']
                if state in ('complete', 'failed'):
                    break
                time.sleep(.05)
            time.sleep(.5)
            job_info = subprocess.check_output(['launchctl', 'print', launchd_job], text=True)
            assert re.search(r'\bruns = 1\b', job_info), job_info
        else:
            subprocess.run([sys.executable, str(root / 'upgrade.py'), 'run', str(path)],
                           env=env, check=True)
        result = json.loads((root / 'status.json').read_text())
        assert result['state'] == 'complete' and result['restored_codex'] == 3, result
        assert 'test-new' in launcher.read_text()
        assert call(new, 'display-message', '-p', '#{window_index}') == '3'
        assert len(call(new, 'list-panes', '-s', '-t', 'fixture', '-F', '#{pane_id}').splitlines()) == 4
        mapping = json.loads((root / 'mapping.json').read_text())
        status_before = (root / 'status.json').read_bytes()
        pids_before = call(new, 'list-panes', '-s', '-t', 'fixture', '-F', '#{pane_id}|#{pane_pid}')
        subprocess.run([sys.executable, str(root / 'upgrade.py'), 'run', str(path)],
                       env=env, check=True)
        assert (root / 'status.json').read_bytes() == status_before
        assert call(new, 'list-panes', '-s', '-t', 'fixture', '-F', '#{pane_id}|#{pane_pid}') == pids_before
        failure = json.dumps(dict(state='failed', error='fixture terminal failure')).encode()
        (root / 'status.json').write_bytes(failure)
        subprocess.run([sys.executable, str(root / 'upgrade.py'), 'run', str(path)],
                       env=env, check=True)
        assert (root / 'status.json').read_bytes() == failure
        (root / 'status.json').write_bytes(status_before)
        subprocess.run([sys.executable, str(root / 'upgrade.py'), 'verify', str(path)],
                       env=env, check=True)
        for window in windows:
            for spec in window['panes']:
                actual = call(new, 'display-message', '-p', '-t', mapping[spec['id']],
                              '#{pane_left},#{pane_top},#{pane_width},#{pane_height}')
                assert actual == spec['geometry'], (spec, actual)
                assert call(new, 'display-message', '-p', '-t', mapping[spec['id']], '#{pane_active}') == str(int(spec['active']))
                if spec.get('session_id'):
                    deadline = time.monotonic() + 3
                    while time.monotonic() < deadline:
                        captured = call(new, 'capture-pane', '-p', '-J', '-t', mapping[spec['id']])
                        if spec['session_id'] in captured:
                            break
                        time.sleep(.02)
                    else:
                        raise AssertionError(captured)
        print('PASS: shared backend, idempotent cutover, one-shot launch config, lifecycle parsing, '
              'client re-exec, three session IDs/four panes/split/active tab/launcher.')
    except BaseException:
        print('CLIENT OUTPUT:', repr(output[-500:]))
        raise
    finally:
        if launchd_job:
            subprocess.run(['launchctl', 'bootout', launchd_job],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for command in (old, new):
            subprocess.run(command + ['kill-server'], stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL, env=env)
        stopped.set()
        if master is not None:
            os.close(master)
        if client:
            until = time.monotonic() + 3
            while time.monotonic() < until:
                if os.waitpid(client, os.WNOHANG)[0]:
                    break
                time.sleep(.02)
            else:
                os.kill(client, signal.SIGTERM)
                os.waitpid(client, 0)
