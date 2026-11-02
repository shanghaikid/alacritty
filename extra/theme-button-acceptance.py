#!/usr/bin/env python3
"""Click the real native theme button in a disposable, instrumented macOS app."""

import importlib.util
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
import time

EXTRA = Path(__file__).resolve().parent
sys.path.insert(0, str(EXTRA / 'terminal-profile'))
spec = importlib.util.spec_from_file_location('terminal_install', EXTRA / 'terminal-profile/install.py')
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)

PROBE = r'''
#import <AppKit/AppKit.h>
#include <stdlib.h>
static NSButton *findButton(NSView *view) {
    if ([view isKindOfClass:[NSButton class]] &&
        [view.toolTip hasPrefix:@"Switch to Merino "]) return (NSButton *)view;
    for (NSView *child in view.subviews) {
        NSButton *found = findButton(child);
        if (found) return found;
    }
    return nil;
}
__attribute__((constructor)) static void start(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        NSString *root = [NSString stringWithUTF8String:getenv("MERINO_ACCEPTANCE")];
        [NSTimer scheduledTimerWithTimeInterval:0.03 repeats:YES block:^(NSTimer *timer) {
            NSString *path = [root stringByAppendingPathComponent:@"command.json"];
            NSData *data = [NSData dataWithContentsOfFile:path];
            if (!data) return;
            [[NSFileManager defaultManager] removeItemAtPath:path error:NULL];
            NSDictionary *request = [NSJSONSerialization JSONObjectWithData:data options:0 error:NULL];
            NSWindow *window = NSApp.mainWindow ?: NSApp.windows.firstObject;
            NSButton *button = nil;
            NSUInteger count = 0;
            for (NSTitlebarAccessoryViewController *accessory in window.titlebarAccessoryViewControllers) {
                NSButton *found = findButton(accessory.view);
                if (found) { button = found; count++; }
            }
            if ([request[@"action"] isEqualToString:@"click"]) [button performClick:nil];
            if ([request[@"action"] isEqualToString:@"resize"])
                [window setContentSize:NSMakeSize(760, 420)];
            if ([request[@"action"] isEqualToString:@"snapshot"]) {
                NSView *frameView = window.contentView.superview;
                NSBitmapImageRep *image = [frameView bitmapImageRepForCachingDisplayInRect:frameView.bounds];
                [frameView cacheDisplayInRect:frameView.bounds toBitmapImageRep:image];
                [[image representationUsingType:NSBitmapImageFileTypePNG properties:@{}]
                    writeToFile:[root stringByAppendingPathComponent:@"snapshot.png"] atomically:YES];
            }
            NSRect frame = [button convertRect:button.bounds toView:nil];
            NSDictionary *state = @{
                @"request": request[@"request"], @"buttons": @(count),
                @"tooltip": button.toolTip ?: @"", @"label": button.accessibilityLabel ?: @"",
                @"x": @(frame.origin.x), @"y": @(frame.origin.y),
                @"buttonWidth": @(frame.size.width), @"windowWidth": @(window.frame.size.width),
                @"contentWidth": @(window.contentView.bounds.size.width),
                @"contentHeight": @(window.contentView.bounds.size.height)
            };
            [[NSJSONSerialization dataWithJSONObject:state options:0 error:NULL]
                writeToFile:[root stringByAppendingPathComponent:@"state.json"] atomically:YES];
        }];
    });
}
'''


def main(app_source):
    with tempfile.TemporaryDirectory(prefix='merino-native-acceptance-') as temporary:
        root = Path(temporary)
        app = root / 'ThemeAcceptance.app'
        shutil.copytree(app_source, app)
        plist = app / 'Contents/Info.plist'
        info = plistlib.loads(plist.read_bytes())
        info['CFBundleIdentifier'] = 'org.alacritty.theme-acceptance'
        plist.write_bytes(plistlib.dumps(info))
        subprocess.run(['codesign', '--force', '--deep', '--sign', '-', str(app)], check=True)
        source, dylib = root / 'probe.m', root / 'probe.dylib'
        source.write_text(PROBE)
        subprocess.run(['clang', '-dynamiclib', '-fobjc-arc', '-framework', 'AppKit',
                        str(source), '-o', str(dylib)], check=True)
        home = root / 'home'
        config_dir = home / '.config/alacritty'
        installer.install_profile(app, home, config_dir, home, home / '.codex')
        profile = config_dir / 'terminal-profile'
        config = config_dir / 'alacritty.toml'
        base = config.read_text() + '''
[window]
startup_mode = "Windowed"
dimensions = { columns = 80, lines = 20 }
title = "Merino native button acceptance"
[keyboard.global_hotkey]
enabled = false
'''
        config.write_text(base)
        binary = str(app / 'Contents/MacOS/alacritty')
        tmux = [str(app / 'Contents/MacOS/tmux'), '-S', str(root / 'tmux.sock')]
        socket = root / 'alacritty.sock'
        env = dict(os.environ, HOME=str(home), ZDOTDIR=str(home),
                   DYLD_INSERT_LIBRARIES=str(dylib), MERINO_ACCEPTANCE=str(root))
        env.pop('ALACRITTY_SOCKET', None)
        env.pop('ALACRITTY_WINDOW_ID', None)
        sequence = 0

        def request(action='state'):
            nonlocal sequence
            sequence += 1
            pending = root / 'command.tmp'
            pending.write_text(json.dumps({'request': sequence, 'action': action}))
            pending.replace(root / 'command.json')
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                try:
                    state = json.loads((root / 'state.json').read_text())
                    if state['request'] == sequence:
                        return state
                except (FileNotFoundError, json.JSONDecodeError):
                    pass
                time.sleep(.03)
            raise AssertionError('Native app did not respond')

        def wait_for(predicate):
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                state = request()
                if predicate(state):
                    return state
                time.sleep(.05)
            raise AssertionError(state)

        process = None
        log_path = root / 'app.log'
        try:
            subprocess.run(tmux + ['-f', str(profile / 'theme.conf'), 'new-session', '-d',
                                   '-s', 'acceptance', '/bin/sleep 300'], check=True)
            with log_path.open('w') as log:
                process = subprocess.Popen([binary, '--config-file', str(config), '--socket', str(socket),
                                            '-e', '/bin/sleep', '300'], env=env, stdout=log, stderr=log)
                initial = wait_for(lambda s: s['buttons'] == 1)
                assert initial['x'] > initial['windowWidth'] - 100, initial
                assert initial['buttonWidth'] == 30, initial
                assert initial['tooltip'] == initial['label'] == 'Switch to Merino Dark', initial
                deadline = time.monotonic() + 5
                while not (profile / 'servers.json').exists():
                    assert time.monotonic() < deadline
                    time.sleep(.03)
                for name, background in [('Light', '#0d1117'), ('Dark', '#ffffff')]:
                    request('click')
                    state = wait_for(lambda s: s['tooltip'] == 'Switch to Merino ' + name)
                    assert state['buttons'] == 1
                    assert state['contentWidth'] == initial['contentWidth'], state
                    assert state['contentHeight'] == initial['contentHeight'], state
                    runtime = subprocess.check_output([binary, 'msg', '-s', str(socket),
                                                       'get-config', '-w', '-1'], env=env, text=True)
                    assert background in runtime, runtime[:500]
                    style = subprocess.check_output(tmux + ['show-options', '-gwv', 'window-style'], text=True)
                    assert background in style, style
                    if os.environ.get('MERINO_ACCEPTANCE_ARTIFACTS'):
                        request('snapshot')
                        artifacts = Path(os.environ['MERINO_ACCEPTANCE_ARTIFACTS'])
                        artifacts.mkdir(parents=True, exist_ok=True)
                        shutil.copy2(root / 'snapshot.png', artifacts / ('merino-' + ('dark' if name == 'Light' else 'light') + '.png'))
                request('resize')
                resized = wait_for(lambda s: s['contentWidth'] == 760)
                assert resized['x'] > resized['windowWidth'] - 100, resized
                # Disable and restore through live config; terminal geometry must stay stable.
                without = re.sub(r'\n\[window\.theme_toggle_command\]\n.*?(?=\n\[)', '', base, flags=re.S)
                config.write_text(without)
                hidden = wait_for(lambda s: s['buttons'] == 0)
                assert hidden['contentHeight'] == resized['contentHeight'], hidden
                config.write_text(base)
                wait_for(lambda s: s['buttons'] == 1)
                request('click')
                wait_for(lambda s: s['tooltip'] == 'Switch to Merino Light')
                process.terminate()
                process.wait(timeout=5)
                socket.unlink(missing_ok=True)
                process = subprocess.Popen([binary, '--config-file', str(config), '--socket', str(socket),
                                            '-e', '/bin/sleep', '300'], env=env, stdout=log, stderr=log)
                reopened = wait_for(lambda s: s['buttons'] == 1 and s['tooltip'] == 'Switch to Merino Light')
                print(json.dumps({'native_clicks': 'passed', 'tmux_sync': 'passed',
                                  'stable_geometry': 'passed', 'resize_and_reload': 'passed',
                                  'restart_persistence': 'passed', 'button': reopened}))
        except Exception:
            if log_path.exists():
                print(log_path.read_text()[-8000:], file=sys.stderr)
            raise
        finally:
            if process and process.poll() is None:
                process.terminate()
                process.wait(timeout=5)
            subprocess.run(tmux + ['kill-server'], capture_output=True)


if __name__ == '__main__':
    main(Path(sys.argv[1]).resolve())
