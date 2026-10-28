#!/usr/bin/env python3
"""Paired, real-PTY benchmarks against Ghostty; run on an otherwise idle desktop.

Each payload is followed by a cursor-position report: timings include draining
and parsing, rather than just writes into the kernel. This does not measure GPU
presentation or physical input latency. Both apps use identical grid dimensions,
font, history limits and grapheme handling. Keep raw samples, not just ratios.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import select
import statistics
import subprocess
import sys
import termios
import time
import tty


def save(path, value):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")
    temporary.replace(path)


def binary_hash(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def launch_environment():
    env = dict(os.environ)
    library = env.get("TERMINAL_BENCH_BACKGROUND_LIBRARY")
    if library:
        env["DYLD_INSERT_LIBRARIES"] = str(Path(library).resolve(strict=True))
    return env


def launch_metadata():
    library = os.environ.get("TERMINAL_BENCH_BACKGROUND_LIBRARY")
    return ({"mode": "background", "library_sha256": binary_hash(library)} if library
            else {"mode": "foreground"})


def write_all(data):
    data = memoryview(data)
    while data:
        data = data[os.write(1, data):]


def report():
    write_all(b"\x1b[6n")
    data = b""
    deadline = time.monotonic() + 30
    while not data.endswith(b"R"):
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([0], [], [], remaining)[0]:
            raise TimeoutError(f"Missing terminal acknowledgment: {data!r}")
        data += os.read(0, 256)
    if not data.startswith(b"\x1b["):
        raise ValueError(f"Unexpected terminal response: {data!r}")
    return data.decode("ascii")


def stats(values):
    ordered = sorted(values)
    return {"median": statistics.median(values),
            "p95": ordered[int((len(ordered) - 1) * .95)], "samples": values}


def varied_unicode():
    """A deterministic working set much larger than the short-cluster cache."""
    randomizer = random.Random(728315)
    lines = []
    for _ in range(2048):
        words = []
        for _ in range(16):
            kind = randomizer.randrange(5)
            if kind == 0:
                words.append(chr(0x4e00 + randomizer.randrange(0x4fff)))
            elif kind == 1:
                words.append(chr(0xac00 + randomizer.randrange(11172)))
            elif kind == 2:
                words.append(chr(0x61 + randomizer.randrange(26)) + chr(0x300 + randomizer.randrange(16)))
            elif kind == 3:
                words.append(chr(0x1f1e6 + randomizer.randrange(26)) + chr(0x1f1e6 + randomizer.randrange(26)))
            else:
                words.append(randomizer.choice(["👩‍💻", "🧑‍🚀", "👨‍🔬", "👩‍⚕️", "👍🏽", "❤️"]))
        lines.append(" ".join(words) + "\r\n")
    return "".join(lines).encode()


def interaction_probes():
    """PTY acknowledgments under bursts and TUI updates, not presentation time."""
    result = {}
    for size in [1024, 64 * 1024, 1024 * 1024]:
        payload = (b"interleaved output 0123456789\r\n" * (size // 30 + 1))[:size]
        timings = []
        for _ in range(20):
            start = time.perf_counter()
            write_all(payload)
            report()
            timings.append((time.perf_counter() - start) * 1000)
        result[f"burst_{size}_bytes_ms"] = stats(timings)
    write_all(b"\x1b[?1049h\x1b[2J\x1b[?25l")
    timings = []
    for frame in range(100):
        rows = [f"\x1b[{row};1H\x1b[{31 + row % 6}m{frame:04} "
                + "状态 e\u0301 👩‍💻 CPU: 42% " * 3 + "\x1b[K"
                for row in range(1, 41)]
        payload = b"\x1b[?2026h" + "".join(rows).encode() + b"\x1b[?2026l"
        start = time.perf_counter()
        write_all(payload)
        report()
        timings.append((time.perf_counter() - start) * 1000)
    result["tui_frame_ack_ms"] = stats(timings)
    write_all(b"\x1b[?1049l\x1b[0m")
    # A first input arriving while idle history maintenance may be running.
    timings = []
    for delay in [.20, .22, .25, .3, .4, .6]:
        write_all(b"\x1b[2J\x1b[3J\x1b[H")
        write_all((b"0123456789 " * 10 + b"\r\n") * 10040)
        report()
        time.sleep(delay)
        start = time.perf_counter()
        write_all(b"x")
        report()
        timings.append((time.perf_counter() - start) * 1000)
    result["first_input_during_idle_maintenance_ms"] = stats(timings)
    return result


def child(destination, mib, samples, workload=None, interaction=False):
    destination = Path(destination)
    result = {"dimensions": list(os.get_terminal_size()), "workloads": {}}
    original = termios.tcgetattr(0)
    try:
        tty.setraw(0)
        # Explicitly enable Unicode grapheme mode where supported.
        write_all(b"\x1b[?2027h\x1b[?25l")
        time.sleep(1)
        report()
        assert result["dimensions"] == [120, 40], result["dimensions"]
        latency = []
        for _ in range(100):
            start = time.perf_counter()
            write_all(b"x")
            report()
            latency.append((time.perf_counter() - start) * 1000)
            time.sleep(.005)
        result["roundtrip_ms"] = stats(latency)
        corpora = {
            "ascii": ("0123456789 abcdefghijklmnopqrstuvwxyz " * 3 + "\r\n").encode(),
            "ascii_wrapped": b"0123456789 abcdefghijklmnopqrstuvwxyz ",
            "cjk": ("中文终端性能测试，日本語と한글。" * 3 + "\r\n").encode(),
            "ansi_cjk": ("\x1b[32m中文终端测试\x1b[0m "
                         "\x1b[1;34mrender throughput\x1b[0m " * 3 + "\r\n").encode(),
            "graphemes": ("e\u0301 👩‍💻 🇨🇳 👍🏽 ❤️ hello " * 3 + "\r\n").encode(),
            "osc_titles": b"\x1b]2;benchmark title 0123456789\x1b\\",
            "hyperlinks": b"\x1b]8;;https://example.com/benchmark\x1b\\linked text\x1b]8;;\x1b\\\r\n",
            "unicode_varied": varied_unicode(),
        }
        for name, line in corpora.items():
            if workload and name != workload:
                continue
            payload = line * max(1, int(mib * 1024**2) // len(line))
            durations = []
            for _ in range(samples):
                write_all(b"\x1b[0m\x1b[2J\x1b[3J\x1b[H")
                report()
                start = time.perf_counter()
                write_all(payload)
                report()
                durations.append(time.perf_counter() - start)
                time.sleep(.15)
            result["workloads"][name] = {
                "bytes": len(payload), "seconds": durations,
                "mib_per_s": len(payload) / 1024**2 / statistics.median(durations),
            }
            save(destination, result)
        result["unicode_positions"] = {}
        for value in ["中文", "e\u0301", "😀", "👩‍💻", "🇨🇳", "👍🏽", "❤️"]:
            write_all(b"\x1b[H\x1b[2K" + value.encode())
            result["unicode_positions"][value] = report()
        if interaction:
            result["interaction"] = interaction_probes()
        result["complete"] = True
    except Exception as error:
        result["error"] = repr(error)
        raise
    finally:
        termios.tcsetattr(0, termios.TCSANOW, original)
        save(destination, result)


def main(args):
    root = Path(args.output).resolve()
    root.mkdir(parents=True, exist_ok=True)
    alacritty_config = root / "alacritty.toml"
    alacritty_config.write_text('''[general]
live_config_reload = false
[window]
startup_mode = "Windowed"
dimensions = { columns = 120, lines = 40 }
padding = { x = 0, y = 0 }
dynamic_padding = false
[font]
normal = { family = "Menlo", style = "Regular" }
size = 11
[scrolling]
history = 10000
[terminal]
grapheme_clusters = true
[keyboard.global_hotkey]
enabled = false
''')
    ghostty_config = root / "ghostty.conf"
    ghostty_config.write_text('''font-family = Menlo
font-size = 11
window-width = 120
window-height = 40
window-padding-x = 0
window-padding-y = 0
scrollback-limit-lines = 10000
scrollback-limit-bytes = unlimited
shell-integration = none
window-save-state = never
quit-after-last-window-closed = true
confirm-close-surface = false
''')
    reference = "baseline" if args.baseline else "ghostty"
    binaries = {"alacritty": str(Path(args.alacritty).resolve()),
                reference: str(Path(args.baseline or args.ghostty).resolve())}
    summary = {"platform": platform.platform(), "machine": platform.machine(),
               "binaries": binaries, "pairs": [], "parameters": vars(args)}
    summary["binary_sha256"] = {name: binary_hash(path) for name, path in binaries.items()}
    summary["python"] = sys.version
    summary["launch"] = launch_metadata()
    for pair in range(args.pairs):
        runs = {}
        order = ["alacritty", reference] if pair % 2 == 0 else [reference, "alacritty"]
        for name in order:
            result_file = root / f"{pair}-{name}.json"
            if result_file.exists():
                raise FileExistsError(result_file)
            command = [binaries[name]]
            if name != "ghostty":
                command += ["--config-file", str(alacritty_config)]
            else:
                command += ["--config-default-files=false", f"--config-file={ghostty_config}"]
            command += ["-e", sys.executable, str(Path(__file__).resolve()), "--child",
                        str(result_file), "--mib", str(args.mib), "--samples", str(args.samples)]
            if args.workload:
                command += ["--workload", args.workload]
            if args.interaction:
                command += ["--interaction"]
            with (root / f"{pair}-{name}.log").open("w") as log:
                process = subprocess.Popen(command, stdout=log, stderr=log, env=launch_environment())
                result_file.with_suffix(".pid").write_text(str(process.pid))
                try:
                    process.wait(timeout=180)
                except BaseException:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
                    raise
            result = json.loads(result_file.read_text())
            assert result.get("complete"), result
            assert process.returncode == 0, process.returncode
            runs[name] = result
            print(json.dumps({"pair": pair, "terminal": name,
                              "mib_per_s": {k: round(v["mib_per_s"], 2)
                                            for k, v in result["workloads"].items()},
                              "roundtrip_ms": result["roundtrip_ms"]["median"]}), flush=True)
            time.sleep(1)
        assert runs["alacritty"]["unicode_positions"] == runs[reference]["unicode_positions"]
        summary["pairs"].append(runs)
        save(root / "summary.json", summary)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--child")
    parser.add_argument("--alacritty")
    reference = parser.add_mutually_exclusive_group()
    reference.add_argument("--ghostty")
    reference.add_argument("--baseline", help="Compare with a previous Alacritty binary")
    parser.add_argument("--output")
    parser.add_argument("--pairs", type=int, default=5)
    parser.add_argument("--mib", type=float, default=8)
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--interaction", action="store_true")
    parser.add_argument("--workload", choices=["ascii", "ascii_wrapped", "cjk", "ansi_cjk",
                                              "graphemes", "osc_titles", "hyperlinks", "unicode_varied"])
    args = parser.parse_args()
    if args.child:
        child(args.child, args.mib, args.samples, args.workload, args.interaction)
    else:
        if not all([args.alacritty, args.baseline or args.ghostty, args.output]):
            parser.error("--alacritty, --ghostty/--baseline and --output are required")
        main(args)
