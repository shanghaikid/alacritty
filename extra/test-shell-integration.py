#!/usr/bin/env python3
"""Exercise the shipped zsh startup trampoline and hooks in an isolated real PTY."""
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import tempfile
import time

SOURCE = Path(__file__).resolve().parent / "shell-integration" / "zsh"


def exercise(login):
    with tempfile.TemporaryDirectory(prefix="alacritty-shell-test-") as root:
        root = Path(root)
        bootstrap, user = root / "bootstrap", root / "user"
        shutil.copytree(SOURCE, bootstrap)
        user.mkdir()
        (user / ".zshenv").write_text("print -r -- ENV-LOADED\n")
        (user / ".zprofile").write_text("print -r -- PROFILE-LOADED\n")
        (user / ".zlogin").write_text("print -r -- LOGIN-LOADED\n")
        (user / ".zshrc").write_text(
            "PS1='SHELL-TEST> '; RPROMPT=''\n"
            "precmd() { return 0 }\n"
            "preexec() { print -r -- EXISTING-PREEXEC }\n"
            "setopt no_prompt_cr no_prompt_sp\n"
        )
        pid, fd = pty.fork()
        if pid == 0:
            env = os.environ.copy()
            env.update(ZDOTDIR=str(bootstrap), ALACRITTY_ZDOTDIR=str(user),
                       ALACRITTY_ZDOTDIR_SET="1", TERM="xterm-256color")
            env.pop("TMUX", None)
            env.pop("STY", None)
            os.execve("/bin/zsh", ["zsh", "-il" if login else "-i"], env)
        data = bytearray()

        def read_until(needle):
            start = len(data)
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                if select.select([fd], [], [], .1)[0]:
                    data.extend(os.read(fd, 65536))
                if needle in data[start:]:
                    return bytes(data[start:])
            raise AssertionError(f"missing {needle!r}: {bytes(data)!r}")

        try:
            initial = read_until(b"\x1b]133;B\x07")
            assert initial.count(b"ENV-LOADED") == 1
            assert (b"PROFILE-LOADED" in initial) == login
            assert (b"LOGIN-LOADED" in initial) == login
            os.write(fd, b"false\n")
            failed = read_until(b"\x1b]133;B\x07")
            assert b"\x1b]133;C\x07" in failed
            assert b"\x1b]133;D;1\x07" in failed, failed
            assert b"EXISTING-PREEXEC" in failed
            os.write(fd, b"print -r -- $ZDOTDIR; print OK\n")
            success = read_until(b"\x1b]133;B\x07")
            assert str(user).encode() in success
            assert b"\x1b]133;D;0\x07" in success
            os.write(fd, b"print REDIRECTED >/dev/null\n")
            redirected = read_until(b"\x1b]133;B\x07")
            assert b"\x1b]133;C\x07" in redirected
            assert b"\x1b]133;D;0\x07" in redirected
            os.write(fd, b"PS1='CHANGED> '\n")
            changed = read_until(b"\x1b]133;B\x07")
            assert b"CHANGED> " in changed
            for _ in range(3):
                os.write(fd, b"true\n")
                frame = read_until(b"\x1b]133;B\x07")
                assert frame.count(b"\x1b]133;A\x07") == 1
                assert frame.count(b"\x1b]133;B\x07") == 1
            os.write(fd, b"exit\n")
        finally:
            os.kill(pid, signal.SIGHUP)
            os.waitpid(pid, 0)
            os.close(fd)
    print(f"PASS: zsh {'login' if login else 'interactive'} hooks, exit status, startup order, ZDOTDIR, redirection, prompt replacement")


if __name__ == "__main__":
    exercise(False)
    exercise(True)
