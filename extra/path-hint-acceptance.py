#!/usr/bin/env python3
"""Test the Rust path resolver against disposable real tmux clients on macOS.

Usage: path-hint-acceptance.py TARGET/debug/deps/alacritty-HASH /path/to/tmux
Run immediately after cargo test, before cleaning its isolated target directory.
"""

import os
from pathlib import Path
import pty
import signal
import subprocess
import sys
import tempfile
import time
import uuid


def check(test_binary, tmux, server, root):
    clients = []
    env = {**os.environ, "TERM": "xterm-256color"}
    env.pop("TMUX", None)
    env.pop("TMUX_PANE", None)

    def command(*args):
        return subprocess.check_output(
            [tmux, *server, *args], env=env, text=True, stderr=subprocess.DEVNULL
        )

    def verify(pid, cwd):
        subprocess.run(
            [test_binary, "macos::path_hint::tests::attached_tmux_client_directory",
             "--ignored", "--exact"],
            env={**env, "ALACRITTY_TEST_TMUX_PID": str(pid),
                 "ALACRITTY_TEST_TMUX_CWD": str(cwd)},
            check=True,
        )

    try:
        for index in range(2):
            cwd = root / f"project {index} 架构图"
            cwd.mkdir(exist_ok=True)
            pid, fd = pty.fork()
            if pid == 0:
                os.execve(tmux, [tmux, *server, "-f", "/dev/null", "new-session",
                                "-s", f"test{index}", "-c", str(cwd)], env)
            clients.append((pid, fd))
            deadline = time.monotonic() + 5
            while True:
                try:
                    if str(pid) in command("list-clients", "-F", "#{client_pid}").splitlines():
                        break
                except subprocess.CalledProcessError:
                    pass
                if time.monotonic() > deadline:
                    raise RuntimeError("disposable tmux client failed to attach")
                time.sleep(0.05)
            verify(pid, cwd)

        # The first client's path must not change when a second client attaches.
        verify(clients[0][0], root / "project 0 架构图")
        pane_dir = root / "new pane"
        pane_dir.mkdir(exist_ok=True)
        command("split-window", "-t", "test0:", "-c", str(pane_dir))
        verify(clients[0][0], pane_dir)
        verify(clients[1][0], root / "project 1 架构图")
    finally:
        subprocess.run([tmux, *server, "kill-server"], env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for pid, fd in clients:
            os.close(fd)
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            os.waitpid(pid, 0)


if __name__ == "__main__":
    binary, tmux = map(os.path.abspath, sys.argv[1:])
    with tempfile.TemporaryDirectory(prefix="alacritty-path-hint-") as temp:
        root = Path(temp)
        check(binary, tmux, ["-S", str(root / "tmux.sock")], root)
        check(binary, tmux, ["-L", "alacritty-path-hint-" + uuid.uuid4().hex], root)
    print("PASS: tmux -S/-L, multiple clients, active pane, spaces and Unicode")
