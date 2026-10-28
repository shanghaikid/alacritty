#!/usr/bin/env python3
"""Measure isolated macOS scrolling frames and local key-event-to-GPU response.

Usage: frame-pacing-acceptance.py APP_BINARY CONFIG OUTPUT_DIRECTORY
The interposer is loaded only into the test child. It never captures other apps
or posts global events. Latency stops at GPU readback, not physical display scanout.
"""
import json
import os
from pathlib import Path
import select
import statistics
import subprocess
import sys
import termios
import time
import tty

PROBE = r'''
#import <AppKit/AppKit.h>
#include <OpenGL/OpenGL.h>
#include <OpenGL/gl3.h>
#include <time.h>
#include <stdio.h>
#include <stdlib.h>
static int sent, seen;
static double sentAt;
static FILE *output;
static double seconds(void) {
    struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}
static int phase(void) {
    const char *path = getenv("ALACRITTY_FRAME_PHASE");
    FILE *f = path ? fopen(path, "r") : NULL;
    if (!f) return 0;
    int p = fgetc(f); fclose(f); return p;
}
static void record(const char *kind, double value) {
    if (!output) output = fopen(getenv("ALACRITTY_FRAME_RESULTS"), "w");
    if (output) { fprintf(output, "%s,%.9f\n", kind, value); fflush(output); }
}
static CGLError flush(CGLContextObj context) {
    int p = phase();
    if (p == 'i' && sent > seen && CGLGetCurrentContext() == context) {
        GLint v[4], alignment, buffer;
        glGetIntegerv(GL_VIEWPORT, v);
        glGetIntegerv(GL_PACK_ALIGNMENT, &alignment);
        glGetIntegerv(GL_READ_BUFFER, &buffer);
        unsigned char rgb[4] = {0};
        glPixelStorei(GL_PACK_ALIGNMENT, 1);
        glReadBuffer(GL_BACK);
        glReadPixels(v[0] + 1, v[1] + v[3] - 2, 1, 1, GL_RGB, GL_UNSIGNED_BYTE, rgb);
        glReadBuffer(buffer);
        glPixelStorei(GL_PACK_ALIGNMENT, alignment);
        if (rgb[0] == sent && rgb[1] == 20 && rgb[2] == 30) {
            seen = sent;
            record("input_ms", (seconds() - sentAt) * 1000);
        }
    }
    CGLError result = CGLFlushDrawable(context);
    if (p == 's') record("scroll_frame", seconds());
    return result;
}
__attribute__((used)) static struct { const void *replacement; const void *original; }
interpose __attribute__((section("__DATA,__interpose"))) = {(const void *)flush, (const void *)CGLFlushDrawable};
__attribute__((constructor)) static void install(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        [NSTimer scheduledTimerWithTimeInterval:0.05 repeats:YES block:^(NSTimer *timer) {
            if (phase() != 'i' || sent != seen) return;
            // Deliver to the isolated test window even when this CLI-launched
            // process is not the foreground app. No global event or focus change.
            NSWindow *window = NSApp.keyWindow ?: NSApp.windows.firstObject;
            if (!window) return;
            NSString *text = sent == 100 ? @"q" : @"x";
            if (sent == 100) [timer invalidate]; else { ++sent; sentAt = seconds(); }
            NSEvent *event = [NSEvent keyEventWithType:NSEventTypeKeyDown
                location:NSZeroPoint modifierFlags:0 timestamp:NSProcessInfo.processInfo.systemUptime
                windowNumber:window.windowNumber context:nil characters:text
                charactersIgnoringModifiers:text isARepeat:NO keyCode:7];
            [window sendEvent:event];
        }];
    });
}
'''


def child(phase):
    phase = Path(phase)
    original = termios.tcgetattr(0)
    try:
        tty.setraw(0)
        os.write(1, b"\x1b[?25l\x1b[2J\x1b[H")
        row = ("\x1b[32m中文滚动\x1b[0m SF Mono 0123456789 👩‍💻 🇨🇳 👍🏽 ❤️\r\n").encode()
        os.write(1, row * 200)
        time.sleep(1)
        phase.write_text("s")
        start = time.monotonic()
        for i in range(240):
            os.write(1, row * 4)
            time.sleep(max(0, start + (i + 1) / 60 - time.monotonic()))
        phase.write_text("-")
        time.sleep(.25)
        os.write(1, b"\x1b[2J\x1b[H")
        phase.write_text("i")
        count = 0
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if not select.select([0], [], [], .5)[0]:
                continue
            for key in os.read(0, 128):
                if key == ord("q"):
                    assert count == 100
                    return
                if key == ord("x"):
                    count += 1
                    os.write(1, f"\x1b[H\x1b[48;2;{count};20;30m \x1b[0m".encode())
        raise TimeoutError(f"Only {count}/100 local key events received")
    finally:
        phase.write_text("-")
        termios.tcsetattr(0, termios.TCSANOW, original)


def main(binary, config, destination):
    root = Path(destination).resolve()
    root.mkdir(parents=True, exist_ok=True)
    source, library = root / "frames.m", root / "frames.dylib"
    source.write_text(PROBE)
    subprocess.run(["xcrun", "clang", "-O2", "-dynamiclib", "-fobjc-arc",
                    "-Wno-deprecated-declarations", str(source), "-framework", "AppKit",
                    "-framework", "OpenGL", "-o", str(library)], check=True)
    subprocess.run(["codesign", "--force", "--sign", "-", str(library)], check=True,
                   capture_output=True)
    phase, samples = root / "phase", root / "samples.csv"
    phase.unlink(missing_ok=True)
    samples.unlink(missing_ok=True)
    env = dict(os.environ, DYLD_INSERT_LIBRARIES=str(library),
               ALACRITTY_FRAME_PHASE=str(phase), ALACRITTY_FRAME_RESULTS=str(samples))
    with (root / "app.log").open("w") as log:
        subprocess.run([str(Path(binary).resolve()), "--config-file", str(Path(config).resolve()),
                        "-vv", "--title", "Scrolling and input verification",
                        "-o", "keyboard.global_hotkey.enabled=false",
                        "-o", 'window.startup_mode="Windowed"',
                        "-o", "window.dimensions={columns=140,lines=35}",
                        "-e", sys.executable, str(Path(__file__).resolve()), "--child", str(phase)],
                       env=env, stdout=log, stderr=log, check=True, timeout=30)
    values = {"scroll_frame": [], "input_ms": []}
    for row in samples.read_text().splitlines():
        kind, value = row.split(",")
        values[kind].append(float(value))
    frames = values["scroll_frame"]
    assert len(values["input_ms"]) == 100, "Missing local key/GPU acknowledgments"
    assert len(frames) > 1, "Missing scrolling frames"
    intervals = [(b - a) * 1000 for a, b in zip(frames, frames[1:])]
    def stats(data):
        ordered = sorted(data)
        return {"median": statistics.median(data), "p95": ordered[int((len(data)-1)*.95)],
                "max": max(data)}
    result = {"scroll_frames": len(frames), "scroll_fps": (len(frames)-1)/(frames[-1]-frames[0]),
              "scroll_interval_ms": stats(intervals), "local_key_samples": len(values["input_ms"]),
              "local_key_to_gpu_ms": stats(values["input_ms"])}
    (root / "results.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
    assert len(values["input_ms"]) == 100, "Missing local key/GPU acknowledgments"
    assert result["scroll_fps"] >= 50, "Unexpected scrolling frame-rate reduction"
    assert result["scroll_interval_ms"]["p95"] < 35, "Uneven scrolling frame pacing"
    assert result["local_key_to_gpu_ms"]["p95"] < 50, "Slow local key/GPU response"


if __name__ == "__main__":
    if sys.argv[1:2] == ["--child"]:
        child(sys.argv[2])
    else:
        main(*sys.argv[1:])
