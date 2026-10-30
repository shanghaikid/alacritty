#!/usr/bin/env python3
"""Compare isolated macOS key-to-render latency on a dense CJK/ASCII screen.

Run one terminal at a time; repeat in alternating order on an otherwise idle
machine. Both use Menlo 11, 140x35 cells, no padding, and 100 local key events.
The probe reads only the child process's own rendered pixels. No global input
or screen recording is used. OpenGL stops at GPU readback; Metal stops at the
completed IOSurface's layer submission. Neither measures display scanout.

Ghostty's official hardened signature blocks DYLD injection. Use an ad-hoc
signed temporary app copy, leaving the installed app unchanged. A missing
probe marker or incomplete sample count is a failed run, never a measurement.

Example: compare-terminal-input.py --terminal alacritty --binary /path/alacritty
    --label candidate-1 --output /tmp/input-comparison
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import select
import statistics
import subprocess
import sys
import termios
import time
import tty


ALACRITTY_CONFIG = '''[general]
live_config_reload = false
[window]
startup_mode = "Windowed"
dimensions = { columns = 140, lines = 35 }
padding = { x = 0, y = 0 }
dynamic_padding = false
[font]
normal = { family = "Menlo", style = "Regular" }
size = 11
[cursor]
style = { blinking = "Off" }
[keyboard.global_hotkey]
enabled = false
'''
GHOSTTY_CONFIG = '''font-family = Menlo
font-size = 11
window-width = 140
window-height = 35
window-padding-x = 0
window-padding-y = 0
window-padding-balance = false
shell-integration = none
window-save-state = never
quit-after-last-window-closed = true
confirm-close-surface = false
cursor-style-blink = false
'''


def child(phase, ready):
    phase, ready = Path(phase), Path(ready)
    original = termios.tcgetattr(0)
    try:
        tty.setraw(0)
        time.sleep(.75)
        dimensions = list(os.get_terminal_size())
        assert dimensions == [140, 35], dimensions
        os.write(1, b"\x1b[?25l\x1b[2J\x1b[H" + bytes(range(32, 127)))
        rows = ["".join(chr(0x4e00 + (row * 24 + col) % 600) + "ab"
                        for col in range(24)) for row in range(30)]
        os.write(1, ("\x1b[H" + "\r\n".join(rows)).encode())
        time.sleep(1)
        ready.touch()
        phase.write_text("i")
        count = 0
        deadline = time.monotonic() + 40
        while time.monotonic() < deadline:
            if not select.select([0], [], [], .5)[0]:
                continue
            for key in os.read(0, 128):
                if key == ord("q"):
                    assert count == 100, count
                    ready.with_suffix(".complete").write_text(str(count))
                    return
                if key == ord("x"):
                    count += 1
                    # Black/white binary markers survive either renderer's
                    # color-space conversion, unlike exact mid-tone RGB values.
                    cells = []
                    for bit in range(8):
                        value = 255 if count & (1 << bit) else 0
                        cells.append(f"\x1b[48;2;{value};{value};{value}m ")
                    os.write(1, ("\x1b[H" + "".join(cells) + "\x1b[0m").encode())
        raise TimeoutError(f"Only {count}/100 local key events received")
    except Exception as error:
        ready.with_suffix(".error").write_text(repr(error))
        raise
    finally:
        phase.write_text("-")
        termios.tcsetattr(0, termios.TCSANOW, original)


def main(args):
    assert (re.fullmatch(r"[a-zA-Z0-9_.-]+", args.label)
            and args.label not in {".", ".."}), "Invalid label"
    root = Path(args.output).resolve() / args.label
    root.mkdir(parents=True, exist_ok=False)
    binary = Path(args.binary).resolve(strict=True)
    try:
        run(args, root, binary)
    finally:
        # Also clean up if compilation, signing, imports, or launch fail.
        (root / "probe.dylib").unlink(missing_ok=True)


def run(args, root, binary):
    source = Path(__file__).with_name("terminal-input-probe.m")
    library = root / "probe.dylib"
    subprocess.run(["clang", "-O2", "-dynamiclib", "-fobjc-arc",
                    "-Wno-deprecated-declarations", str(source), "-framework", "AppKit",
                    "-framework", "OpenGL", "-framework", "QuartzCore", "-framework",
                    "IOSurface", "-o", str(library)], check=True)
    subprocess.run(["codesign", "--force", "--sign", "-", str(library)], check=True,
                   capture_output=True)
    config = root / ("ghostty.conf" if args.terminal == "ghostty" else "alacritty.toml")
    config.write_text(GHOSTTY_CONFIG if args.terminal == "ghostty" else ALACRITTY_CONFIG)
    options = (["--config-default-files=false", f"--config-file={config}"]
               if args.terminal == "ghostty" else ["--config-file", str(config)])

    spec = importlib.util.spec_from_file_location(
        "terminal_resources", Path(__file__).with_name("compare-terminal-resources.py"))
    resources = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(resources)
    phase, samples, ready = root / "phase", root / "samples.csv", root / "ready"
    env = dict(os.environ, DYLD_INSERT_LIBRARIES=str(library),
               PARITY_PHASE=str(phase), PARITY_SAMPLES=str(samples))
    with (root / "app.log").open("w") as log:
        process = subprocess.Popen([str(binary), *options, "-e", sys.executable,
                                    str(Path(__file__).resolve()), "--child", str(phase), str(ready)],
                                   env=env, stdout=log, stderr=log)
        initial, last, peak = None, None, 0
        try:
            deadline = time.monotonic() + 55
            while process.poll() is None:
                if time.monotonic() > deadline:
                    raise TimeoutError("Input probe exceeded 55 seconds")
                try:
                    usage = resources.usage(process.pid)
                except OSError:
                    if process.poll() is None:
                        raise
                    break
                peak = max(peak, usage["footprint_mib"])
                if ready.exists() and usage["footprint_mib"] > 0:
                    last = (time.monotonic(), usage)
                    if initial is None:
                        initial = last
                time.sleep(.02)
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()

    values = [float(line) for line in samples.read_text().splitlines()] if samples.exists() else []
    result = {"terminal": args.terminal, "label": args.label, "samples": len(values),
              "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "peak_footprint_mib": peak, "exit_code": process.returncode,
              "grid": [140, 35], "font": "Menlo 11", "key_interval_ms": 50,
              "endpoint": "GPU readback" if args.terminal == "alacritty" else "IOSurface submission"}
    if values:
        result.update(median_ms=statistics.median(values),
                      p95_ms=sorted(values)[int((len(values) - 1) * .95)], max_ms=max(values))
    if initial and last and last[0] > initial[0]:
        elapsed = last[0] - initial[0]
        result.update(cpu_percent_one_core=100 * (last[1]["cpu_seconds"] - initial[1]["cpu_seconds"]) / elapsed,
                      footprint_mib=last[1]["footprint_mib"])
    complete = ready.with_suffix(".complete")
    error = ready.with_suffix(".error")
    result["child_complete"] = complete.exists() and complete.read_text() == "100"
    result["probe_loaded"] = "input probe installed" in (root / "app.log").read_text()
    result["valid"] = (len(values) == 100 and process.returncode == 0
                       and result["child_complete"] and result["probe_loaded"]
                       and not error.exists())
    if error.exists():
        result["child_error"] = error.read_text()
    (root / "results.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result), flush=True)
    assert result["valid"], result


if __name__ == "__main__":
    if sys.argv[1:2] == ["--child"]:
        child(*sys.argv[2:])
    else:
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument("--terminal", choices=["alacritty", "ghostty"], required=True)
        parser.add_argument("--binary", required=True)
        parser.add_argument("--label", required=True)
        parser.add_argument("--output", required=True)
        main(parser.parse_args())
