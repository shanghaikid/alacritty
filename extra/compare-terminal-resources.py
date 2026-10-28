#!/usr/bin/env python3
"""Compare macOS physical footprint and idle CPU at equal history line limits.

Use configs from compare-terminal-performance.py. Measurements cover the native
terminal process, excluding this benchmark child. Darwin rusage CPU counters are
Mach ticks, converted with mach_timebase_info, not assumed to be nanoseconds.
"""
import argparse
import ctypes
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import termios
import time
import tty

spec = importlib.util.spec_from_file_location("terminal_bench", Path(__file__).with_name("compare-terminal-performance.py"))
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


class Usage(ctypes.Structure):
    _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [(name, ctypes.c_uint64) for name in
        ["user", "system", "idle_wakes", "interrupt_wakes", "pageins", "wired",
         "resident", "footprint", "start", "exit"]]


class Timebase(ctypes.Structure):
    _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]


def usage(pid):
    value = Usage()
    libproc = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    if libproc.proc_pid_rusage(pid, 0, ctypes.byref(value)):
        raise OSError(ctypes.get_errno(), "proc_pid_rusage")
    timebase = Timebase()
    assert ctypes.CDLL("/usr/lib/libSystem.B.dylib").mach_timebase_info(ctypes.byref(timebase)) == 0
    return {"footprint_mib": value.footprint / 1024**2,
            "resident_mib": value.resident / 1024**2,
            "cpu_seconds": (value.user + value.system) * timebase.numer / timebase.denom / 1e9,
            "idle_wakes": value.idle_wakes, "interrupt_wakes": value.interrupt_wakes}


def history_rows(start, end, corpus, varied):
    for n in range(start, end):
        if corpus == "ascii":
            body = "0123456789 " * 10
        elif corpus == "cjk":
            body = "中文终端日本語한글測試" * 5
        elif corpus == "ansi":
            body = "".join(f"\x1b[{31+i}m" + chr(97+i) * 22 for i in range(5)) + "\x1b[0m"
        elif corpus == "graphemes":
            body = "e\u0301 👩‍💻 🇨🇳 " * 13 + "xxxxxx"
        else:
            body = varied[n % len(varied)]
        yield f"{n:07d} " + body + "\r\n"


def child(output, history, corpus):
    output = Path(output)
    original = termios.tcgetattr(0)
    result = {"history_limit": history, "corpus": corpus}
    varied = bench.varied_unicode().decode().splitlines() if corpus == "unicode_varied" else []
    try:
        tty.setraw(0)
        time.sleep(1)
        pid = int(output.with_suffix(".pid").read_text())
        result["terminal_pid"] = pid
        result["dimensions"] = list(os.get_terminal_size())
        assert result["dimensions"] == [120, 40]
        bench.write_all(b"\x1b[?2027h\x1b[?25l\x1b[2J\x1b[3J\x1b[H")
        bench.report()
        time.sleep(1)
        result["empty"] = usage(pid)
        # Unique line numbers; fixed corpora occupy 118 cells. The varied
        # Unicode corpus has shorter rows. No corpus wraps at 120 columns.
        fill_started = time.perf_counter()
        for start in range(0, history + 40, 1000):
            rows = history_rows(start, min(start + 1000, history + 40), corpus, varied)
            bench.write_all("".join(rows).encode())
        bench.report()
        result["history_fill_seconds"] = time.perf_counter() - fill_started
        result["history_active"] = usage(pid)
        time.sleep(3)
        result["history_idle"] = usage(pid)
        started = time.monotonic()
        time.sleep(3)
        after = usage(pid)
        elapsed = time.monotonic() - started
        before = result["history_idle"]
        result["idle"] = {"seconds": elapsed,
            "cpu_percent_one_core": 100 * (after["cpu_seconds"] - before["cpu_seconds"]) / elapsed,
            "wakeups_per_second": (after["idle_wakes"] - before["idle_wakes"]) / elapsed}
        bench.write_all(b"\x1b[2J\x1b[3J\x1b[H")
        bench.report()
        time.sleep(3)
        result["cleared_idle"] = usage(pid)
        result["complete"] = True
    except Exception as error:
        result["error"] = repr(error)
        raise
    finally:
        termios.tcsetattr(0, termios.TCSANOW, original)
        bench.save(output, result)


def main(args):
    root = Path(args.output).resolve()
    root.mkdir(parents=True, exist_ok=True)
    configs = Path(args.config_directory).resolve()
    summary = []
    reference = "baseline" if args.baseline else "ghostty"
    binaries = [("alacritty", args.alacritty), (reference, args.baseline or args.ghostty)]
    cases = []
    for pair in range(args.pairs):
        for history in ([args.history] if args.history else [10000, 100000]):
            order = binaries if pair % 2 == 0 else binaries[::-1]
            cases.extend((pair, history, name, binary) for name, binary in order)
    for pair, history, name, binary in cases:
        output = root / f"{pair}-{history}-{name}.json"
        if output.exists(): raise FileExistsError(output)
        if name != "ghostty":
            options = ["--config-file", str(configs / "alacritty.toml"), "-o", f"scrolling.history={history}"]
        else:
            # Ghostty loads recursive config files AFTER CLI options, so
            # overriding this value on the command line would be ignored.
            source = (configs / "ghostty.conf").read_text()
            assert "scrollback-limit-lines = 10000" in source
            ghostty_config = root / f"ghostty-{history}.conf"
            ghostty_config.write_text(source.replace("scrollback-limit-lines = 10000", f"scrollback-limit-lines = {history}"))
            options = ["--config-default-files=false", f"--config-file={ghostty_config}"]
        with output.with_suffix(".log").open("w") as log:
            process = subprocess.Popen([str(Path(binary).resolve()), *options, "-e", sys.executable,
                str(Path(__file__).resolve()), "--child", str(output), "--history", str(history),
                "--corpus", args.corpus], stdout=log, stderr=log, env=bench.launch_environment())
            output.with_suffix(".pid").write_text(str(process.pid))
            peak = 0
            try:
                deadline = time.monotonic() + 90
                while process.poll() is None:
                    if time.monotonic() >= deadline:
                        raise TimeoutError("Resource probe exceeded 90 seconds")
                    try:
                        peak = max(peak, usage(process.pid)["footprint_mib"])
                    except OSError:
                        if process.poll() is None:
                            raise
                    time.sleep(.01)
            finally:
                if process.poll() is None:
                    process.terminate()
                    try: process.wait(timeout=5)
                    except subprocess.TimeoutExpired: process.kill(); process.wait()
        value = json.loads(output.read_text())
        assert value.get("complete") and process.returncode == 0, value
        value["terminal"] = name
        value["pair"] = pair
        value["binary_sha256"] = bench.binary_hash(binary)
        value["launch"] = bench.launch_metadata()
        value["sampled_peak_footprint_mib"] = peak
        value["sampling_interval_seconds"] = .01
        summary.append(value)
        bench.save(root / "summary.json", summary)
        print(json.dumps(value), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--child")
    parser.add_argument("--history", type=int)
    parser.add_argument("--corpus", choices=["ascii", "cjk", "ansi", "graphemes", "unicode_varied"], default="ascii")
    parser.add_argument("--alacritty")
    reference = parser.add_mutually_exclusive_group()
    reference.add_argument("--ghostty")
    reference.add_argument("--baseline")
    parser.add_argument("--pairs", type=int, default=1)
    parser.add_argument("--config-directory")
    parser.add_argument("--output")
    args = parser.parse_args()
    if args.child: child(args.child, args.history, args.corpus)
    else: main(args)
