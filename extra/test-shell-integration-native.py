#!/usr/bin/env python3
"""Verify automatic zsh integration through the packaged app's real PTY."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def main(binary):
    with tempfile.TemporaryDirectory(prefix="alacritty-shell-native-") as directory:
        root = Path(directory)
        user = root / "user"
        user.mkdir()
        (user / ".zshrc").write_text(r'''
PS1='NATIVE> '
RPROMPT=''
setopt no_prompt_sp no_prompt_cr
typeset -gi test_command_sent=0
native_line_init() {
    if (( !test_command_sent )); then
        test_command_sent=1
        BUFFER='print -r -- NATIVE-OUTPUT "$ZDOTDIR"; false'
        zle accept-line
    fi
}
zle -N zle-line-init native_line_init
''')
        recording = root / "alacritty.recording"
        env = dict(os.environ, ZDOTDIR=str(user))
        env.pop("TMUX", None)
        env.pop("STY", None)
        with (root / "app.log").open("w") as log:
            process = subprocess.Popen([
                str(Path(binary).resolve()), "--config-file", "/dev/null", "--ref-test", "--hold",
                "-o", "keyboard.global_hotkey.enabled=false",
                "-o", "terminal.notify_on_command_finish=false",
                "-e", "/bin/zsh", "-il",
            ], cwd=root, env=env, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline:
                    if process.poll() is not None:
                        raise RuntimeError((root / "app.log").read_text())
                    data = recording.read_bytes() if recording.exists() else b""
                    if b"\x1b]133;D;1\x07" in data and data.count(b"\x1b]133;A\x07") >= 2:
                        break
                    time.sleep(.05)
                else:
                    raise AssertionError(f"No completed shell command: {data!r}\n{(root / 'app.log').read_text()}")
                assert b"\x1b]133;B\x07" in data
                assert b"\x1b]133;C\x07" in data
                assert b"NATIVE-OUTPUT " + str(user).encode() + b"\r\n" in data
                print(json.dumps({"automatic_injection": True, "exit_status": 1,
                                  "zdotdir_restored": True, "recorded_bytes": len(data)}))
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


if __name__ == "__main__":
    main(sys.argv[1])
