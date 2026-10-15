#!/usr/bin/env python3
"""Compare isolated macOS OpenGL frames without screen-recording permission.

Usage: python3 extra/render-acceptance.py BEFORE_BINARY AFTER_BINARY OUTPUT_DIR
A temporary interposer reads only the framebuffer of each test child process.
It is never loaded into an existing application or the installed app by default.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


INTERPOSER = r'''
#include <OpenGL/OpenGL.h>
#include <OpenGL/gl3.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
static CGLError capture_flush(CGLContextObj context) {
    const char *ready = getenv("ALACRITTY_ACCEPTANCE_READY");
    const char *path = getenv("ALACRITTY_ACCEPTANCE_FRAME");
    if (ready && path && access(ready, F_OK) == 0 && CGLGetCurrentContext() == context) {
        GLint viewport[4], alignment, read_buffer;
        glGetIntegerv(GL_VIEWPORT, viewport);
        glGetIntegerv(GL_PACK_ALIGNMENT, &alignment);
        glGetIntegerv(GL_READ_BUFFER, &read_buffer);
        int width = viewport[2], height = viewport[3];
        if (width > 0 && height > 0 && width < 16384 && height < 16384) {
            unsigned char *pixels = malloc((size_t)width * height * 3);
            if (pixels) {
                glPixelStorei(GL_PACK_ALIGNMENT, 1);
                glReadBuffer(GL_BACK);
                glReadPixels(viewport[0], viewport[1], width, height, GL_RGB, GL_UNSIGNED_BYTE, pixels);
                glPixelStorei(GL_PACK_ALIGNMENT, alignment);
                glReadBuffer(read_buffer);
                FILE *file = fopen(path, "wb");
                if (file) {
                    fprintf(file, "P6\n%d %d\n255\n", width, height);
                    for (int row = height - 1; row >= 0; row--)
                        fwrite(pixels + (size_t)row * width * 3, 3, width, file);
                    fclose(file);
                }
                free(pixels);
            }
        }
    }
    return CGLFlushDrawable(context);
}
__attribute__((used)) static struct { const void *replacement; const void *original; }
interpose __attribute__((section("__DATA,__interpose"))) = {
    (const void *)capture_flush, (const void *)CGLFlushDrawable
};
'''


def child(ready):
    lines = [
        "Alacritty render acceptance: ASCII 0123456789 !@#$%^&*()",
        "中文宽字符、标点；日本語と한글。",
        "Combining marks: e\u0301 a\u0308 n\u0303 e\u0301\u0302",
        "Emoji: 😀 🚀 ✅ 🧪",
        "\x1b]8;id=example;https://example.com\x1b\\OSC 8 hyperlink with combining e\u0301\x1b]8;;\x1b\\",
        "\x1b[4;58;5;2mColored underline: abc 中文 e\u0301\x1b[0m",
        "\x1b[1mBold\x1b[0m \x1b[3mItalic\x1b[0m \x1b[1;3mBold italic\x1b[0m",
        "\x1b[9mStrikeout\x1b[0m \x1b[7mInverse\x1b[0m \x1b[8mHidden e\u0301\x1b[0m visible",
        "┌──────────────┬───────────┐",
        "│ tmux / TUI   │ alignment │",
        "└──────────────┴───────────┘",
        "https://example.com/regular-url",
    ]
    os.write(1, ("\x1b[?25l\x1b[2J\x1b[H" + "\r\n".join(lines)).encode())
    Path(ready).touch()
    time.sleep(60)


def main(before, after, destination):
    destination = Path(destination).resolve()
    destination.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="alacritty-render-acceptance-") as directory:
        root = Path(directory)
        source, library = root / "capture.c", root / "capture.dylib"
        source.write_text(INTERPOSER)
        subprocess.run(["xcrun", "clang", "-O2", "-dynamiclib", "-Wno-deprecated-declarations",
                        str(source), "-framework", "OpenGL", "-o", str(library)], check=True)
        subprocess.run(["codesign", "--force", "--sign", "-", str(library)], check=True,
                       capture_output=True)
        for label, binary in [("before", before), ("after", after)]:
            ready = root / (label + ".ready")
            frame = destination / (label + ".ppm")
            frame.unlink(missing_ok=True)
            env = dict(os.environ, DYLD_INSERT_LIBRARIES=str(library),
                       ALACRITTY_ACCEPTANCE_READY=str(ready), ALACRITTY_ACCEPTANCE_FRAME=str(frame))
            with (destination / (label + ".log")).open("w") as log:
                process = subprocess.Popen([
                    str(Path(binary).resolve()), "--config-file", "/dev/null",
                    "--title", "Render acceptance", "-o", 'window.startup_mode="Windowed"',
                    "-o", 'window.decorations="None"', "-o", 'window.position={x=80,y=80}',
                    "-o", 'window.dimensions={columns=80,lines=24}',
                    "-o", 'window.padding={x=0,y=0}', "-o", 'window.dynamic_padding=false',
                    "-e", sys.executable, str(Path(__file__).resolve()), "--child", str(ready),
                ], stdout=log, stderr=log, env=env)
                try:
                    deadline = time.monotonic() + 15
                    while not frame.exists():
                        if process.poll() is not None or time.monotonic() > deadline:
                            raise RuntimeError((destination / (label + ".log")).read_text())
                        time.sleep(.05)
                    time.sleep(1)
                finally:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
            subprocess.run(["sips", "-s", "format", "png", str(frame), "--out",
                            str(destination / (label + ".png"))], check=True, capture_output=True)
        before_parts = (destination / "before.ppm").read_bytes().split(b"\n", 3)
        after_parts = (destination / "after.ppm").read_bytes().split(b"\n", 3)
        assert before_parts[:3] == after_parts[:3], "frame dimensions differ"
        a, b = before_parts[3], after_parts[3]
        width, height = map(int, before_parts[1].split())
        assert len(a) == len(b) == width * height * 3
        # Reject an accidentally captured blank startup frame.
        assert len(set(a[i:i+3] for i in range(0, len(a), 3))) > 32
        differences = sum(a[i:i+3] != b[i:i+3] for i in range(0, len(a), 3))
        result = dict(width=width, height=height, different_pixels=differences)
        (destination / "comparison.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result))
        assert differences == 0, "rendered pixels changed"


if __name__ == "__main__":
    if sys.argv[1:2] == ["--child"]:
        child(sys.argv[2])
    else:
        main(*sys.argv[1:])
