#!/usr/bin/env python3
"""Verify native PTY exit draining, including a descendant holding the slave.

Uses isolated --hold/--ref-test windows and compares every recorded byte. No
installed application, user configuration or existing window is modified.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import tty


def payload():
    return ("last output 中文 e\u0301 👩‍💻\r\n" * 80000 + "DRAIN-COMPLETE\r\n").encode()


def child(inherited):
    tty.setraw(0)
    if inherited and os.fork() == 0:
        time.sleep(2)
        os._exit(0)
    data = memoryview(payload())
    while data:
        data = data[os.write(1, data):]
    os._exit(0)


def main(binary, config):
    results = []
    expected = payload()
    for inherited in [False, True]:
        with tempfile.TemporaryDirectory(prefix="alacritty-pty-", dir="/private/tmp") as directory:
            root = Path(directory)
            # --ref-test preserves cwd, including on macOS. Keep every
            # recording/snapshot in this fresh test-owned directory.
            recording = root / "alacritty.recording"
            if recording.exists():
                raise FileExistsError(recording)
            with (root / "terminal.log").open("w") as log:
                command = [str(Path(binary).resolve()), "--config-file", str(Path(config).resolve()),
                           "--hold", "--ref-test", "-e", sys.executable,
                           str(Path(__file__).resolve()), "--child"]
                if inherited:
                    command.append("--inherited-slave")
                process = subprocess.Popen(command, cwd=root, stdout=log, stderr=log)
                try:
                    deadline = time.monotonic() + 15
                    while time.monotonic() < deadline:
                        if process.poll() is not None:
                            raise RuntimeError((root / "terminal.log").read_text())
                        if recording.exists() and recording.stat().st_size >= len(expected):
                            break
                        time.sleep(.02)
                    if not recording.exists():
                        raise RuntimeError((root / "terminal.log").read_text())
                    assert recording.read_bytes() == expected, "lost or reordered final output"
                    # Allow the descendant to exit before cleaning up the held window.
                    if inherited:
                        time.sleep(2.1)
                    results.append({"inherited_slave": inherited, "verified_bytes": len(expected)})
                finally:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
                    if recording.exists():
                        # This process created the previously absent recording.
                        recording.unlink()
    print(json.dumps(results))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--child", action="store_true")
    parser.add_argument("--inherited-slave", action="store_true")
    parser.add_argument("--alacritty")
    parser.add_argument("--config")
    args = parser.parse_args()
    if args.child:
        child(args.inherited_slave)
    elif args.alacritty and args.config:
        main(args.alacritty, args.config)
    else:
        parser.error("--alacritty and --config are required")
