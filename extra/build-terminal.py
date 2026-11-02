#!/usr/bin/env python3
"""Build the complete Apple Silicon terminal bundle, cleaning temporary artifacts."""

import argparse
import gzip
import json
import os
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
TMUX_URL = 'https://github.com/shanghaikid/tmux.git'
TMUX_REV = 'b60f804c97adee1aaa7bd9f005af90bdfc0b949d'
TARGET = 'aarch64-apple-darwin'


def run(*args, cwd=ROOT, env=None, **kwargs):
    print('+', ' '.join(map(str, args)), flush=True)
    return subprocess.run(list(map(str, args)), cwd=cwd, env=env, check=True, **kwargs)


def build(output, test=False):
    if platform.system() != 'Darwin' or platform.machine() != 'arm64':
        raise RuntimeError('Use native Apple Silicon macOS (not a Rosetta shell)')
    required = ('cargo', 'git', 'brew', 'make', 'cc', 'autoconf', 'automake',
                'pkg-config', 'scdoc', 'tic', 'codesign', 'ditto', 'yacc')
    missing = [name for name in required if not shutil.which(name)]
    if missing:
        raise RuntimeError('Missing build tools: ' + ', '.join(missing) + '; see TERMINAL-SETUP.md')
    brew = subprocess.check_output(['brew', '--prefix'], text=True).strip()
    for dependency in ('libevent', 'ncurses', 'utf8proc', 'jemalloc'):
        if not (Path(brew) / 'opt' / dependency).exists():
            raise RuntimeError('Missing dependency: brew install ' + dependency)
    # This directory owns every generated Rust/C artifact, including on failure.
    with tempfile.TemporaryDirectory(prefix='alacritty-terminal-build-') as temporary:
        work = Path(temporary)
        target = work / 'target'
        # Homebrew's Rust standard library/runtime dependencies target current
        # macOS releases. Build for this Mac instead of claiming 11.0 support.
        deployment = '.'.join(platform.mac_ver()[0].split('.')[:2])
        env = dict(os.environ, CARGO_TARGET_DIR=str(target), MACOSX_DEPLOYMENT_TARGET=deployment)
        tmux = work / 'tmux-source'
        run('git', 'init', tmux)
        run('git', '-C', tmux, 'fetch', '--depth', '1', TMUX_URL, TMUX_REV)
        run('git', '-C', tmux, 'checkout', '--detach', 'FETCH_HEAD')
        revision = subprocess.check_output(['git', '-C', str(tmux), 'rev-parse', 'HEAD'], text=True).strip()
        if revision != TMUX_REV:
            raise RuntimeError('Unexpected tmux revision')
        run('cargo', 'build', '--locked', '--release', '--target', TARGET,
            '--target-dir', target, '-p', 'alacritty', '-p', 'alacritty_status', env=env)
        release = target / TARGET / 'release'
        if test:
            for test_filter in ('config::', 'polling::ipc::tests'):
                run('cargo', 'test', '--locked', '--release', '--target', TARGET,
                    '--target-dir', target, '-p', 'alacritty', '--bin', 'alacritty', test_filter, env=env)
        tmux_env = dict(env,
                        CPPFLAGS=f'-I{brew}/opt/libevent/include -I{brew}/opt/ncurses/include -I{brew}/opt/utf8proc/include',
                        LDFLAGS=f'-L{brew}/opt/libevent/lib -L{brew}/opt/ncurses/lib -L{brew}/opt/utf8proc/lib',
                        PKG_CONFIG_PATH=f'{brew}/opt/libevent/lib/pkgconfig:{brew}/opt/ncurses/lib/pkgconfig:{brew}/opt/utf8proc/lib/pkgconfig')
        run('sh', 'autogen.sh', cwd=tmux, env=tmux_env)
        run('./configure', '--enable-utf8proc', '--enable-jemalloc',
            f'LIBS={release}/libalacritty_status.a -framework Security -framework SystemConfiguration -liconv',
            cwd=tmux, env=tmux_env)
        run('make', '-j', str(os.cpu_count() or 2), cwd=tmux, env=tmux_env)
        app = work / 'Alacritty.app'
        run('ditto', ROOT / 'extra/osx/Alacritty.app', app)
        plist = app / 'Contents/Info.plist'
        info = plistlib.loads(plist.read_bytes())
        info['LSMinimumSystemVersion'] = deployment
        plist.write_bytes(plistlib.dumps(info))
        binaries = app / 'Contents/MacOS'
        resources = app / 'Contents/Resources'
        binaries.mkdir(parents=True, exist_ok=True)
        shutil.copy2(release / 'alacritty', binaries / 'alacritty')
        shutil.copy2(tmux / 'tmux', binaries / 'tmux')
        profile = resources / 'terminal-profile'
        shutil.copytree(ROOT / 'extra/terminal-profile', profile,
                        ignore=shutil.ignore_patterns('__pycache__', '*.pyc'))
        for name in ('merino-light.toml', 'merino-dark.toml', 'rex-appearance.toml',
                     'merino-light-tmux.conf', 'merino-dark-tmux.conf',
                     'tmux-shift-selection.conf', 'tmux-mouse-anchor-input.py'):
            shutil.copy2(ROOT / 'extra' / name, profile / name)
        shutil.copy2(tmux / 'COPYING', resources / 'LICENSE-tmux')
        for name in ('LICENSE-APACHE', 'LICENSE-MIT'):
            shutil.copy2(ROOT / name, resources / name)
        for name in ('alacritty.1', 'alacritty-msg.1', 'alacritty.5', 'alacritty-bindings.5'):
            with (ROOT / 'extra/man' / (name + '.scd')).open('rb') as source:
                manpage = subprocess.check_output(['scdoc'], stdin=source)
            (resources / (name + '.gz')).write_bytes(gzip.compress(manpage, mtime=0))
        completions = resources / 'completions'
        completions.mkdir(exist_ok=True)
        for name in ('_alacritty', 'alacritty.bash', 'alacritty.fish'):
            shutil.copy2(ROOT / 'extra/completions' / name, completions / name)
        run('tic', '-xe', 'alacritty,alacritty-direct', '-o', resources, ROOT / 'extra/alacritty.info')
        (resources / 'terminal-build.json').write_text(json.dumps({
            'alacritty_revision': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
            'alacritty_dirty': bool(subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT)),
            'tmux_revision': TMUX_REV, 'target': TARGET, 'macos_deployment_target': deployment,
        }, indent=2) + '\n')
        run('codesign', '--force', '--deep', '--sign', '-', app)
        run('codesign', '--verify', '--deep', '--strict', app)
        # Publish only a completed bundle; preserve an older output on failure.
        output.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='.terminal-output-', dir=output.parent) as staging:
            staged = Path(staging) / 'Alacritty.app'
            run('ditto', app, staged)
            if output.exists():
                output.rename(Path(staging) / 'previous.app')
            staged.rename(output)
    print('Removed temporary Rust and tmux build artifacts:', temporary)
    print('Built:', output)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=ROOT / 'dist/Alacritty.app')
    parser.add_argument('--test', action='store_true', help='Run config and IPC regression tests before cleanup')
    args = parser.parse_args()
    build(args.output.resolve(), args.test)
