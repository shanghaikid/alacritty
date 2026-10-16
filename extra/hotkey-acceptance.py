#!/usr/bin/env python3
"""Exercise native Carbon hotkey events in an isolated macOS app process.

Usage: python3 extra/hotkey-acceptance.py /path/to/Alacritty.app

This verifies registration, callback dispatch, window toggling, repeat suppression,
reload, conflict rollback, disabling, and daemon startup. It sends Carbon events
to the registered target rather than synthesizing physical keyboard input. The
temporary interposer is never loaded into an existing application or installed.
"""
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import time


INTERPOSER = r'''
#import <AppKit/AppKit.h>
#import <Carbon/Carbon.h>
#include <stdio.h>
#include <stdlib.h>
static EventHotKeyID current_id, previous_id;
static EventTargetRef current_target;
static unsigned registrations, removals;
static OSStatus last_status;
static OSStatus register_hotkey(UInt32 key, UInt32 mods, EventHotKeyID id,
                               EventTargetRef target, OptionBits options, EventHotKeyRef *ref) {
    OSStatus result = RegisterEventHotKey(key, mods, id, target, options, ref);
    last_status = result;
    if (!result) {
        previous_id = current_id;
        current_id = id;
        current_target = target;
        registrations++;
    }
    return result;
}
static OSStatus unregister_hotkey(EventHotKeyRef ref) {
    removals++;
    return UnregisterEventHotKey(ref);
}
__attribute__((used)) static struct { const void *replacement; const void *original; }
interposers[] __attribute__((section("__DATA,__interpose"))) = {
    { (const void *)register_hotkey, (const void *)RegisterEventHotKey },
    { (const void *)unregister_hotkey, (const void *)UnregisterEventHotKey }
};
static void send_hotkey(UInt32 kind, EventHotKeyID id) {
    EventRef event = NULL;
    OSStatus status = CreateEvent(NULL, kEventClassKeyboard, kind, GetCurrentEventTime(), 0, &event);
    if (!status) {
        status = SetEventParameter(event, kEventParamDirectObject, typeEventHotKeyID, sizeof(id), &id);
        if (!status) SendEventToEventTarget(event, current_target);
        ReleaseEvent(event);
    }
}
__attribute__((constructor)) static void acceptance_start(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        NSString *root = [NSString stringWithUTF8String:getenv("ALACRITTY_HOTKEY_ACCEPTANCE")];
        [NSTimer scheduledTimerWithTimeInterval:0.03 repeats:YES block:^(NSTimer *timer) {
            NSString *command_path = [root stringByAppendingPathComponent:@"command.json"];
            NSData *data = [NSData dataWithContentsOfFile:command_path];
            if (!data) return;
            [[NSFileManager defaultManager] removeItemAtPath:command_path error:NULL];
            NSDictionary *request = [NSJSONSerialization JSONObjectWithData:data options:0 error:NULL];
            NSString *action = request[@"action"];
            if ([action isEqualToString:@"press"] || [action isEqualToString:@"tap"])
                send_hotkey(kEventHotKeyPressed, current_id);
            if ([action isEqualToString:@"release"] || [action isEqualToString:@"tap"])
                send_hotkey(kEventHotKeyReleased, current_id);
            if ([action isEqualToString:@"stale"])
                send_hotkey(kEventHotKeyPressed, previous_id);
            if ([action isEqualToString:@"minimize"])
                [NSApp.keyWindow miniaturize:nil];
            // Observe after winit processes the proxy notification queued by the callback.
            dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 100 * NSEC_PER_MSEC),
                           dispatch_get_main_queue(), ^{
                NSWindow *window = NSApp.mainWindow;
                if (!window) window = NSApp.windows.firstObject;
                NSDictionary *state = @{
                    @"request": request[@"request"],
                    @"hidden": @(NSApp.hidden), @"active": @(NSApp.active),
                    @"windows": @(NSApp.windows.count),
                    @"visible": @(window && window.visible),
                    @"minimized": @(window && window.miniaturized),
                    @"registrations": @(registrations), @"removals": @(removals),
                    @"last_status": @(last_status)
                };
                NSData *result = [NSJSONSerialization dataWithJSONObject:state options:0 error:NULL];
                [result writeToFile:[root stringByAppendingPathComponent:@"state.json"] atomically:YES];
            });
        }];
    });
}
'''


def main(app_source):
    with tempfile.TemporaryDirectory(prefix='alacritty-hotkey-native-') as directory:
        root = Path(directory)
        app = root/'HotkeyAcceptance.app'
        shutil.copytree(app_source, app)
        plist_path = app/'Contents/Info.plist'
        plist = plistlib.loads(plist_path.read_bytes())
        plist['CFBundleIdentifier'] = 'org.alacritty.hotkey-acceptance'
        plist_path.write_bytes(plistlib.dumps(plist))
        subprocess.run(['codesign', '--force', '--deep', '--sign', '-', str(app)], check=True)
        source = root/'acceptance.m'
        dylib = root/'acceptance.dylib'
        source.write_text(INTERPOSER)
        subprocess.run(['clang', '-dynamiclib', '-fobjc-arc', '-framework', 'AppKit',
                        '-framework', 'Carbon', str(source), '-o', str(dylib)], check=True)
        binary = str(app/'Contents/MacOS/alacritty')
        config = root/'alacritty.toml'
        base = '''[window]
startup_mode = "Windowed"
title = "Native hotkey acceptance"
[terminal.shell]
program = "/bin/sleep"
args = ["300"]
[keyboard.global_hotkey]
enabled = true
key = "KeyY"
mods = "Command|Control|Alt|Shift"
'''
        config.write_text(base)
        env = dict(os.environ, DYLD_INSERT_LIBRARIES=str(dylib),
                   ALACRITTY_HOTKEY_ACCEPTANCE=str(root))
        sequence = 0

        def request(action='state'):
            nonlocal sequence
            sequence += 1
            temporary = root/'command.tmp'
            temporary.write_text(json.dumps({'request': sequence, 'action': action}))
            temporary.replace(root/'command.json')
            deadline = time.monotonic()+5
            while time.monotonic() < deadline:
                try:
                    state = json.loads((root/'state.json').read_text())
                    if state['request'] == sequence:
                        return state
                except (FileNotFoundError, json.JSONDecodeError):
                    pass
                time.sleep(.03)
            raise AssertionError('Native event loop did not respond')

        def wait_for(predicate):
            deadline = time.monotonic()+5
            while time.monotonic() < deadline:
                state = request()
                if predicate(state):
                    return state
            raise AssertionError(state)

        processes = []
        logs = []
        try:
            log = (root/'runtime.log').open('w')
            logs.append(log)
            process = subprocess.Popen([binary, '--config-file', str(config), '-v'],
                                       env=env, stdout=log, stderr=log)
            processes.append(process)
            wait_for(lambda s: s['registrations'] == 1 and s['active'] and s['visible'])
            assert request('tap')['hidden'], 'Foreground hotkey did not hide the application'
            shown = request('tap')
            assert not shown['hidden'] and shown['active'] and shown['visible'], shown
            assert request('press')['hidden']
            assert request('press')['hidden'], 'Held shortcut toggled more than once'
            request('release')
            assert not request('tap')['hidden']
            print('PASS: native registration, foreground hide, hidden summon, held-key suppression')

            request('minimize')
            wait_for(lambda s: s['minimized'])
            restored = request('tap')
            assert restored['active'] and restored['visible'] and not restored['minimized'], restored
            print('PASS: a minimized window is restored and focused by the native hotkey')

            config.write_text(base.replace('KeyY', 'KeyU'))
            rebound = wait_for(lambda s: s['registrations'] == 2 and s['removals'] == 1)
            assert not request('stale')['hidden'], 'Stale registration event toggled the window'
            assert request('tap')['hidden']
            request('tap')
            print('PASS: hot reload replaces/releases registration and ignores stale events')

            # A second isolated process owns a different combination exclusively.
            conflict_config = root/'conflict.toml'
            conflict_config.write_text(base.replace('KeyY', 'KeyI'))
            conflict_log = (root/'conflict.log').open('w')
            logs.append(conflict_log)
            other = subprocess.Popen([binary, '--config-file', str(conflict_config), '--daemon', '-v'],
                                     stdout=conflict_log, stderr=conflict_log)
            processes.append(other)
            deadline = time.monotonic()+5
            while 'Registered native global hotkey' not in (root/'conflict.log').read_text():
                assert time.monotonic() < deadline, (root/'conflict.log').read_text()
                time.sleep(.03)
            config.write_text(base.replace('KeyY', 'KeyI'))
            failed = wait_for(lambda s: s['last_status'] != 0)
            assert failed['registrations'] == rebound['registrations'], failed
            assert failed['removals'] == rebound['removals'], failed
            assert request('tap')['hidden'], 'Registration conflict lost the previous shortcut'
            request('tap')
            print('PASS: conflicting replacement keeps the previous native shortcut active')

            config.write_text(base.replace('enabled = true', 'enabled = false'))
            wait_for(lambda s: s['removals'] == 2)
            assert not request('tap')['hidden'], 'Disabled shortcut still toggled the application'
            config.write_text(base)
            wait_for(lambda s: s['registrations'] == 3)
            assert request('tap')['hidden']
            print('PASS: disabling releases the shortcut; reenabling registers it again')

            process.terminate()
            process.wait(timeout=5)
            other.terminate()
            other.wait(timeout=5)
            config.write_text(base)
            process = subprocess.Popen([binary, '--config-file', str(config), '--daemon', '-v'],
                                       env=env, stdout=log, stderr=log)
            processes.append(process)
            wait_for(lambda s: s['registrations'] == 1 and s['windows'] == 0)
            created = request('tap')
            assert created['windows'] > 0 and created['visible'] and created['active'], created
            print('PASS: daemon with no window creates and focuses a terminal on native hotkey')
        except Exception:
            for path in (root/'runtime.log', root/'conflict.log'):
                if path.exists():
                    print(path.read_text(), file=sys.stderr)
            raise
        finally:
            for process in processes:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
            for log in logs:
                log.close()
    print('Temporary acceptance apps, helper library, and logs removed.')


if __name__ == '__main__':
    main(sys.argv[1])
