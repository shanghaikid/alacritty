#!/usr/bin/env python3
"""Verify native macOS selection, pointer feedback, and disposable tmux tabs.

Usage: mouse-selection-acceptance.py APP_BINARY TMUX_BINARY
Events go only to the test child's NSView. Clipboard writes use a private
pasteboard, and the isolated tmux server is removed when the test ends.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time
PROBE = r'''
#import <AppKit/AppKit.h>
#import <objc/runtime.h>
static NSPasteboard *board;
static NSPasteboard *privateBoard(id self,SEL cmd) { if (!board) board=[NSPasteboard pasteboardWithUniqueName]; return board; }
static NSString *shape=@"unknown";
static void (*originalRect)(id,SEL,NSRect,NSCursor *);
static void cursorRect(id self,SEL cmd,NSRect rect,NSCursor *cursor) {
 if (cursor==NSCursor.IBeamCursor) shape=@"text";
 else if (cursor==NSCursor.crosshairCursor) shape=@"crosshair";
 else if (cursor==NSCursor.pointingHandCursor) shape=@"pointer";
 else if (cursor==NSCursor.arrowCursor) shape=@"arrow";
 originalRect(self,cmd,rect,cursor);
}
__attribute__((constructor)) static void start(void) {
 method_setImplementation(class_getClassMethod(NSPasteboard.class,@selector(generalPasteboard)),(IMP)privateBoard);
 dispatch_async(dispatch_get_main_queue(), ^{
  (void)privateBoard(nil,NULL);
  Method m=class_getInstanceMethod(NSView.class,@selector(addCursorRect:cursor:));
  originalRect=(void *)method_getImplementation(m); method_setImplementation(m,(IMP)cursorRect);
  NSString *root=@(getenv("ALACRITTY_MOUSE_PROBE"));
  [NSTimer scheduledTimerWithTimeInterval:.005 repeats:YES block:^(NSTimer *timer) {
   NSString *path=[root stringByAppendingPathComponent:@"command.json"];
   NSData *data=[NSData dataWithContentsOfFile:path]; if (!data) return;
   [[NSFileManager defaultManager] removeItemAtPath:path error:NULL];
   NSDictionary *r=[NSJSONSerialization JSONObjectWithData:data options:0 error:NULL];
   NSWindow *w=NSApp.mainWindow ?: NSApp.windows.firstObject;
   NSView *v=w.contentView; if (!v) return;
   NSString *action=r[@"action"]; NSUInteger mods=[r[@"mods"] unsignedIntegerValue];
   NSPoint pos=NSMakePoint([r[@"x"] doubleValue],(v.isFlipped ? [r[@"y"] doubleValue] : v.bounds.size.height-[r[@"y"] doubleValue]));
   pos=[v convertPoint:pos toView:nil];
   NSEventType type=NSEventTypeMouseMoved;
   if ([action isEqual:@"down"]) type=NSEventTypeLeftMouseDown;
   if ([action isEqual:@"up"]) type=NSEventTypeLeftMouseUp;
   if ([action isEqual:@"drag"]) type=NSEventTypeLeftMouseDragged;
   if ([action isEqual:@"mods"]) {
    NSEvent *e=[NSEvent keyEventWithType:NSEventTypeFlagsChanged location:pos modifierFlags:mods timestamp:NSProcessInfo.processInfo.systemUptime windowNumber:w.windowNumber context:nil characters:@"" charactersIgnoringModifiers:@"" isARepeat:NO keyCode:58];
    [v flagsChanged:e];
   } else if (![action isEqual:@"state"]) {
    NSEvent *e=[NSEvent mouseEventWithType:type location:pos modifierFlags:mods timestamp:NSProcessInfo.processInfo.systemUptime windowNumber:w.windowNumber context:nil eventNumber:0 clickCount:1 pressure:0];
    if (type==NSEventTypeLeftMouseDown) [v mouseDown:e];
    else if (type==NSEventTypeLeftMouseUp) [v mouseUp:e];
    else if (type==NSEventTypeLeftMouseDragged) [v mouseDragged:e];
    else [v mouseMoved:e];
   }
   dispatch_after(dispatch_time(DISPATCH_TIME_NOW,20*NSEC_PER_MSEC),dispatch_get_main_queue(),^{
    [v resetCursorRects];
    NSDictionary *s=@{@"request":r[@"request"],@"cursor":shape,@"text":[board stringForType:NSPasteboardTypeString] ?: @"",@"scale":@(w.backingScaleFactor)};
    [[NSJSONSerialization dataWithJSONObject:s options:0 error:NULL] writeToFile:[root stringByAppendingPathComponent:@"state.json"] atomically:YES];
   });
  }];
 });
}
'''

CHILD = r'''import os
from pathlib import Path
import time

os.write(1, b"\x1b[2J\x1b[Hhello world ABCDEFGHIJKLMNOPQRSTUVWXYZ\r\nsecond line 0123456789\r\n\x1b]8;;https://example.com\x1b\\link text\x1b]8;;\x1b\\")
flag = Path(@ATTACH@)
while not flag.exists():
    time.sleep(.01)
os.execv(@TMUX@, ["tmux", "-S", @SOCKET@, "attach", "-t", "native"])
'''


def main(binary, tmux):
    with tempfile.TemporaryDirectory(prefix='alacritty-mouse-native-') as temp:
        root = Path(temp)
        (root / 'probe.m').write_text(PROBE)
        subprocess.run(['clang', '-dynamiclib', '-fobjc-arc', '-framework', 'AppKit', str(root / 'probe.m'), '-o', str(root / 'probe.dylib')], check=True)
        subprocess.run(['codesign', '--force', '--sign', '-', str(root / 'probe.dylib')], check=True, capture_output=True)
        config = root / 'config.toml'
        config.write_text('''[window]
startup_mode = "Windowed"
decorations = "None"
dynamic_padding = false
dimensions = { columns = 60, lines = 15 }
padding = { x = 10, y = 10 }
[keyboard.global_hotkey]
enabled = false
[mouse]
hide_when_typing = false
shift_passthrough = true
[selection]
save_to_clipboard = true
''')
        child = root / 'child.py'
        child.write_text(CHILD.replace('@ATTACH@', repr(str(root / 'attach')))
                         .replace('@TMUX@', repr(str(tmux)))
                         .replace('@SOCKET@', repr(str(root / 'tmux.sock'))))
        env = dict(os.environ, DYLD_INSERT_LIBRARIES=str(root / 'probe.dylib'),
                   ALACRITTY_MOUSE_PROBE=temp)
        env.pop('TMUX', None)
        env.pop('TMUX_PANE', None)
        seq = 0

        def request(action='state', x=0, y=0, mods=0):
            nonlocal seq
            seq += 1
            p = root / 'command.tmp'
            p.write_text(json.dumps(dict(request=seq, action=action, x=x, y=y, mods=mods)))
            p.replace(root / 'command.json')
            until = time.monotonic() + 5
            while time.monotonic() < until:
                try:
                    state = json.loads((root / 'state.json').read_text())
                    if state['request'] == seq:
                        return state
                except (FileNotFoundError, json.JSONDecodeError):
                    pass
                time.sleep(0.005)
            raise RuntimeError('Native test did not respond')
        with (root / 'app.log').open('w') as log:
            proc = subprocess.Popen([str(binary), '--config-file', str(config), '-vv', '-e', '/usr/bin/env', '-u', 'DYLD_INSERT_LIBRARIES', sys.executable, str(child)], env=env, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 15
                geometry = None
                while time.monotonic() < deadline:
                    logs = (root / 'app.log').read_text()
                    geometry = re.search('Cell size: (\\d+) x (\\d+)', logs)
                    if geometry:
                        break
                    if proc.poll() is not None:
                        raise RuntimeError('App exited: ' + str(proc.returncode) + ' ' + logs)
                    time.sleep(0.05)
                if not geometry:
                    raise RuntimeError('No geometry after 15s: ' + logs)
                time.sleep(0.7)
                state = request()
                scale = state['scale']
                cw, ch = (int(x) / scale for x in geometry.groups())
                px, py = (10, 10)

                def at(action, col, row=0, mods=0):
                    return request(action, px + col * cw, py + (row + 0.5) * ch, mods)
                print('GEOMETRY', cw, ch, scale, flush=True)
                state = at('move', 1)
                assert state['cursor'] == 'text', state
                at('down', 0.2)
                at('drag', 4.8)
                state = at('up', 4.8)
                assert state['text'] == 'hello', state
                time.sleep(0.55)
                at('move', 1)
                at('down', 1)
                at('up', 1)
                at('down', 1)
                state = at('up', 1)
                assert state['text'] == 'hello', state
                at('down', 12.2)
                at('drag', 14.8)
                state = at('up', 14.8)
                assert state['text'] == 'ABC', state
                request('mods', mods=1 << 19)
                state = at('move', 1, 0, 1 << 19)
                assert state['cursor'] == 'crosshair', state
                at('down', 0.2, 0, 1 << 19)
                at('drag', 2.8, 1, 1 << 19)
                state = at('up', 2.8, 1, 1 << 19)
                assert state['text'] == 'hel\nsec', state
                request('mods', mods=1 << 17)
                at('down', 4.8, 1, 1 << 17)
                at('drag', 4.8, 2, 1 << 17)
                state = at('up', 4.8, 2, 1 << 17)
                assert state['text'] == 'hello\nsecon\nlink', state
                request('mods', mods=0)
                time.sleep(0.55)
                at('down', 35, 1)
                at('up', 35, 1)
                state = at('move', 2, 2)
                assert state['cursor'] == 'pointer', state
                at('down', 0.2, 2)
                state = at('drag', 3.8, 2)
                assert state['cursor'] == 'text', state
                state = at('up', 3.8, 2)
                assert state['text'] == 'link', state
                tmux = str(tmux)
                tmux_config = root / 'tmux.conf'
                tmux_config.write_text("set -g mouse on\nset -g status-position top\n"
                                       "set -g status-left ''\nset -g status-right ''\n"
                                       "setw -g window-status-format ' #I:#W '\n"
                                       "setw -g window-status-current-format ' #I:#W '\n")

                env_without_probe = {k: v for k, v in env.items()
                                     if k not in ('DYLD_INSERT_LIBRARIES', 'ALACRITTY_MOUSE_PROBE')}

                def mux(*args):
                    return subprocess.check_output(
                        [tmux, '-S', str(root / 'tmux.sock'), *args], env=env_without_probe, text=True)
                mux('-f', str(tmux_config), 'new-session', '-d', '-s', 'native', '-n', 'FIRST', 'sleep 120')
                mux('new-window', '-t', 'native', '-n', 'SECOND', 'sleep 120')
                mux('select-window', '-t', 'native:0')
                (root / 'attach').touch()
                time.sleep(0.6)
                state = at('move', 13, 0)
                assert state['cursor'] == 'arrow', state
                at('down', 13, 0)
                at('up', 13, 0)
                assert mux('display-message', '-p', '-t', 'native', '#{window_index}').strip() == '1'
                state = at('move', 1, 2)
                assert state['cursor'] == 'arrow', state
                at('down', 1, 2)
                state = at('drag', 7, 2)
                assert state['cursor'] == 'text', state
                state = at('up', 7, 2)
                assert state['cursor'] == 'arrow', state
                request('mods', mods=1 << 17)
                state = at('move', 1, 2, 1 << 17)
                assert state['cursor'] == 'text', state
                at('down', 1, 2, 1 << 17)
                state = at('up', 1, 2, 1 << 17)
                assert state['cursor'] == 'text', state
                state = request('mods', mods=0)
                assert state['cursor'] == 'arrow', state
                request('mods', mods=1 << 20)
                state = at('move', 13, 0, 1 << 20)
                assert state['cursor'] == 'text', state
                request('mods', mods=1 << 20 | 1 << 19)
                state = at('move', 13, 0, 1 << 20 | 1 << 19)
                assert state['cursor'] == 'crosshair', state
                request('mods', mods=0)
                state = at('move', 13, 0)
                assert state['cursor'] == 'arrow', state
                print('PASS: tmux tab arrow, click switches window, drag/Shift I-beam, release arrow, Cmd overrides', flush=True)
                print('PASS: native text cursor, drag selection, word double-click, distant click reset, Option rectangle, link cursor reset', flush=True)
            except BaseException:
                print((root / 'app.log').read_text()[-6000:])
                raise
            finally:
                tmux = Path(tmux)
                subprocess.run([str(tmux), '-S', str(root / 'tmux.sock'), 'kill-server'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                proc.terminate()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait()


if __name__ == '__main__':
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    main(Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve())
