#!/usr/bin/env python3
"""Run inside an isolated terminal: terminal-io-bench.py OUTPUT.json.

Measure PTY write + terminal parsing + cursor-report round trips. These are
not physical keyboard latency, GPU frame times, or a substitute for a visual
scrolling check. No shell integration or existing terminal session is used.
"""
import json
import os
from pathlib import Path
import select
import statistics
import sys
import termios
import time
import tty


def write_all(data):
    while data:
        data = data[os.write(1, data):]


def cursor_report():
    write_all(b"\x1b[6n")
    response = b""
    deadline = time.monotonic() + 15
    while not response.endswith(b"R"):
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([0], [], [], remaining)[0]:
            raise TimeoutError(f"cursor report timeout: {response!r}")
        response += os.read(0, 256)
    if not response.startswith(b"\x1b["):
        raise ValueError(f"unexpected cursor report: {response!r}")
    return response.decode()


def main(destination):
    original = termios.tcgetattr(0)
    result = {}
    try:
        tty.setraw(0)
        # Let the new window finish mapping and building its initial glyph cache.
        time.sleep(1)
        result["terminal"] = os.environ.get("TERM_PROGRAM", os.environ.get("TERM"))
        result["dimensions"] = list(os.get_terminal_size())
        cursor_report()
        latencies = []
        for _ in range(100):
            start = time.perf_counter()
            write_all(b"x")
            cursor_report()
            latencies.append((time.perf_counter() - start) * 1000)
            time.sleep(.005)
        result["roundtrip_ms"] = {
            "median": statistics.median(latencies),
            "p95": sorted(latencies)[94],
            "max": max(latencies),
        }
        workloads = {
            "ascii": ("0123456789 abcdefghijklmnopqrstuvwxyz " * 3 + "\r\n").encode(),
            "ansi_cjk": ("\x1b[32m中文终端测试\x1b[0m "
                         "\x1b[1;34mrender throughput\x1b[0m " * 3 + "\r\n").encode(),
        }
        result["throughput"] = {}
        for name, line in workloads.items():
            payload = line * (2 * 1024 * 1024 // len(line))
            samples = []
            for _ in range(3):
                start = time.perf_counter()
                write_all(payload)
                cursor_report()
                samples.append(time.perf_counter() - start)
                time.sleep(.1)
            result["throughput"][name] = {
                "bytes": len(payload), "seconds": samples,
                "median_mib_per_s": len(payload) / 1024**2 / statistics.median(samples),
            }
        # Probe how much grid width each representative sequence consumes.
        result["unicode_cursor_reports"] = {}
        for value in ["中文", "e\u0301", "😀", "🚀", "👩‍💻", "🇨🇳", "👍🏽", "❤️"]:
            write_all(b"\r\x1b[2K" + value.encode())
            result["unicode_cursor_reports"][value] = cursor_report()
    except Exception as error:
        result["error"] = repr(error)
        raise
    finally:
        termios.tcsetattr(0, termios.TCSANOW, original)
        Path(destination).write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main(sys.argv[1])
