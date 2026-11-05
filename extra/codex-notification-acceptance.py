#!/usr/bin/env python3
"""End-to-end macOS notification tests in an isolated app and tmux server.

Uses real hooks, observer subprocesses, local IPC and Notification Center. A
small in-process native fixture invokes the installed notification delegate
(the same callback macOS invokes on click), and hides only the test app. No
physical input is synthesized and existing windows/panes are never touched.
"""
import json
import os
from pathlib import Path
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent / 'terminal-profile'))
import codex_notifications as n

NATIVE = r'''
#import <AppKit/AppKit.h>
#import <UserNotifications/UserNotifications.h>
@interface TestNotification : NSObject
@property UNNotificationRequest *request;
@end
@implementation TestNotification
@end
@interface TestResponse : NSObject
@property TestNotification *notification;
@property NSString *actionIdentifier;
@end
@implementation TestResponse
@end
static NSArray<NSButton *> *notificationButtons(void) {
  NSMutableArray *buttons = [NSMutableArray array];
  for (NSWindow *window in NSApp.windows) {
    if (!(window.styleMask & NSWindowStyleMaskTitled)) continue;
    for (NSTitlebarAccessoryViewController *accessory in window.titlebarAccessoryViewControllers) {
      for (NSView *view in accessory.view.subviews) {
        if ([view isKindOfClass:NSButton.class] && [((NSButton *)view).accessibilityLabel hasPrefix:@"Codex 通知"])
          [buttons addObject:view];
      }
    }
  }
  return buttons;
}
__attribute__((constructor)) static void install_fixture(void) {
  // Isolate test launch behavior from AppKit's crash/session restoration UI.
  [NSUserDefaults.standardUserDefaults setVolatileDomain:@{@"ApplePersistenceIgnoreState":@YES,
      @"NSQuitAlwaysKeepsWindows":@NO} forName:NSArgumentDomain];
  dispatch_async(dispatch_get_main_queue(), ^{
    NSString *root = [NSString stringWithUTF8String:getenv("ALACRITTY_NOTIFICATION_ACCEPTANCE")];
    id originalPreference = [NSUserDefaults.standardUserDefaults objectForKey:@"CodexNotificationsEnabled"];
    [NSTimer scheduledTimerWithTimeInterval:0.05 repeats:YES block:^(NSTimer *timer) {
      NSString *path = [root stringByAppendingPathComponent:@"command.json"];
      NSData *data = [NSData dataWithContentsOfFile:path];
      if (!data) return;
      [[NSFileManager defaultManager] removeItemAtPath:path error:NULL];
      NSDictionary *request = [NSJSONSerialization JSONObjectWithData:data options:0 error:NULL];
      NSString *action = request[@"action"];
      UNUserNotificationCenter *center = UNUserNotificationCenter.currentNotificationCenter;
      if ([action isEqualToString:@"hide"]) [NSApp hide:nil];
      if ([action isEqualToString:@"click"]) {
        TestNotification *notification = [TestNotification new];
        notification.request = [UNNotificationRequest requestWithIdentifier:request[@"id"]
            content:[UNMutableNotificationContent new] trigger:nil];
        TestResponse *response = [TestResponse new];
        response.notification = notification;
        response.actionIdentifier = UNNotificationDefaultActionIdentifier;
        [center.delegate userNotificationCenter:center didReceiveNotificationResponse:(id)response
            withCompletionHandler:^{}];
      }
      if ([action isEqualToString:@"dismiss"]) {
        [center removeDeliveredNotificationsWithIdentifiers:@[request[@"id"]]];
      }
      if ([action isEqualToString:@"toggle"]) [notificationButtons().firstObject performClick:nil];
      if ([action isEqualToString:@"restore"]) {
        if (originalPreference) [NSUserDefaults.standardUserDefaults setObject:originalPreference forKey:@"CodexNotificationsEnabled"];
        else [NSUserDefaults.standardUserDefaults removeObjectForKey:@"CodexNotificationsEnabled"];
      }
      dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 250 * NSEC_PER_MSEC), dispatch_get_main_queue(), ^{
        NSMutableArray *buttons = [NSMutableArray array];
        for (NSButton *button in notificationButtons()) {
          [buttons addObject:@{@"label":button.accessibilityLabel, @"frame":NSStringFromRect(button.frame),
              @"hasImage":@(button.image != nil), @"template":@(button.image.template)}];
        }
        [[NSJSONSerialization dataWithJSONObject:buttons options:0 error:NULL]
            writeToFile:[root stringByAppendingPathComponent:@"buttons.json"] atomically:YES];
        [@"ok" writeToFile:[root stringByAppendingPathComponent:@"ack"] atomically:YES
            encoding:NSUTF8StringEncoding error:NULL];
      });
    }];
  });
}
'''


def wait(check, label, timeout=10):
    end = time.monotonic() + timeout
    last = None
    while time.monotonic() < end:
        try:
            last = check()
            if last:
                return last
        except (OSError, ValueError, KeyError) as error:
            last = type(error).__name__ + ': ' + str(error)
        time.sleep(.1)
    raise AssertionError(label + ': ' + repr(last))


def main(bundle):
    app = Path(bundle).resolve()
    binary = app / 'Contents/MacOS/alacritty'
    tmux = app / 'Contents/MacOS/tmux'
    results = []
    with tempfile.TemporaryDirectory(prefix='codex-notify-accept-', dir='/private/tmp') as temporary:
        root = Path(temporary)
        process = None
        workers = []
        socket = root / 'app.sock'
        tmux_socket = root / 'tmux.sock'
        home = root / 'codex'; home.mkdir()
        state = root / 'state'
        (root / 'fixture.m').write_text(NATIVE)
        subprocess.run(['cc', '-dynamiclib', '-fobjc-arc', '-framework', 'AppKit', '-framework', 'UserNotifications',
                        str(root / 'fixture.m'), '-o', str(root / 'fixture.dylib')], check=True, capture_output=True)
        db = sqlite3.connect(home / 'goals_1.sqlite')
        db.execute('CREATE TABLE thread_goals(thread_id TEXT PRIMARY KEY, status TEXT, created_at_ms INTEGER, updated_at_ms INTEGER)')
        db.commit()
        def tm(*args):
            return subprocess.check_output([str(tmux), '-S', str(tmux_socket), *args], text=True).strip()
        def windows():
            return json.loads(n.ipc(socket, {'ProgramStatus': {'window_id': -1}})['ProgramStatus'])
        def delivered():
            return json.loads(n.ipc(socket, 'AgentNotifications')['ProgramStatus'])
        def native(action, session=''):
            ack = root / 'ack'
            ack.unlink(missing_ok=True)
            (root / 'command.tmp').write_text(json.dumps({'action': action, 'id': 'alacritty-codex-' + session}))
            (root / 'command.tmp').rename(root / 'command.json')
            wait(ack.exists, 'native callback acknowledgement')
        def pass_(name):
            results.append(name)
            print('PASS:', name, flush=True)
        try:
            tm('-f', '/dev/null', 'new-session', '-d', '-s', 'notify-test', '-n', 'first', '/bin/sleep 1200')
            pane1 = tm('display-message', '-p', '-t', 'notify-test:0', '#{pane_id}')
            pane2 = tm('new-window', '-P', '-F', '#{pane_id}', '-t', 'notify-test', '-n', 'second', '/bin/sleep 1200')
            env = dict(os.environ, DYLD_INSERT_LIBRARIES=str(root / 'fixture.dylib'), ALACRITTY_NOTIFICATION_ACCEPTANCE=str(root))
            with (root / 'app.log').open('w') as log:
                process = subprocess.Popen([str(binary), '-vv', '--config-file', '/dev/null', '--socket', str(socket),
                    '--title', 'Codex notification acceptance', '-o', 'keyboard.global_hotkey.enabled=false',
                    '-o', 'window.startup_mode="Windowed"',
                    '-o', 'window.dimensions.columns=72', '-o', 'window.dimensions.lines=12',
                    '-e', str(tmux), '-S', str(tmux_socket), 'attach-session', '-t', 'notify-test'],
                    env=env, stdout=log, stderr=log)
            window = wait(lambda: windows()[0], 'test window', timeout=20)
            if not window.get('codex_notifications_enabled', True): native('toggle')
            native('hide')
            wait(lambda: not windows()[0]['focused'], 'test window hidden')
            sessions = {}
            def setup(session, pane):
                transcript = root / (session + '.jsonl'); transcript.write_text('')
                sessions[session] = (transcript, pane)
                hook(session, 'UserPromptSubmit', 'turn0')
                status = state / 'codex-notifications' / session / 'status.json'
                value = wait(lambda: json.loads(status.read_text()), 'observer startup')
                workers.append(value['pid'])
            def hook(session, event, turn='', **fields):
                transcript, pane = sessions[session]
                with patch.dict(os.environ, {'ALACRITTY_SOCKET': str(socket), 'ALACRITTY_WINDOW_ID': str(window['window_id']),
                        'TMUX': str(tmux_socket) + ',0,0', 'TMUX_PANE': pane,
                        'XDG_STATE_HOME': str(state), 'CODEX_HOME': str(home)}):
                    n.hook({'hook_event_name': event, 'session_id': session, 'transcript_path': str(transcript),
                            'cwd': '/notification-acceptance', 'turn_id': turn, **fields}, os.getpid(), '/dev/ttys000')
            def append(session, type_, payload):
                with sessions[session][0].open('a') as f:
                    f.write(json.dumps({'type': type_, 'payload': payload}) + '\n')
            def event(session, type_, **fields):
                append(session, 'event_msg', {'type': type_, **fields})
            def item(session, type_, **fields):
                append(session, 'response_item', {'type': type_, **fields})
            def goal(session, status):
                db.execute('INSERT OR REPLACE INTO thread_goals VALUES(?,?,?,?)', (session, status, 1000, int(time.time() * 1000)))
                db.commit()
            def present(session):
                return 'alacritty-codex-' + session in delivered()
            def question(session, call):
                item(session, 'function_call', name='request_user_input_async', call_id=call,
                     arguments=json.dumps({'questions': [{'title': 'test question'}]}))
                item(session, 'function_call_output', call_id=call, output='{"accepted":true}')
            def answer(session, call):
                text = '<send_user_message_question_reply>\n' + json.dumps([{'questionItemId': json.dumps(['request_user_input_async', call, 0]), 'answer': 'yes'}]) + '\n</send_user_message_question_reply>'
                item(session, 'message', role='user', content=[{'text': text}])
            s = 'acceptance-observer-' + str(os.getpid())
            setup(s, pane1)
            event(s, 'task_started', turn_id='normal')
            event(s, 'task_complete', turn_id='normal')
            wait(lambda: present(s), 'normal completion notification')
            pass_('hook → observer → live tmux routing → native completion notification')
            for _ in range(4): hook(s, 'Stop', 'normal')
            time.sleep(2.5)
            actual = delivered()
            assert actual.count('alacritty-codex-' + s) == 1, actual
            pass_('repeated Stop events do not accumulate notifications')
            native('click', s)
            wait(lambda: windows()[0]['focused'], 'native notification click focuses window')
            wait(lambda: tm('display-message', '-p', '-t', 'notify-test', '#{pane_id}') == pane1, 'native click selects exact tmux pane')
            wait(lambda: not present(s), 'native click clears notification')
            pass_('real notification delegate focuses the correct window/pane and removes notification')
            event(s, 'task_started', turn_id='visible')
            event(s, 'task_complete', turn_id='visible')
            time.sleep(3)
            assert not present(s), {'windows': windows(), 'panes': tm('list-panes', '-a', '-F', '#{pane_id} #{window_active} #{pane_active}')}
            native('hide')
            time.sleep(1)
            assert not present(s)
            pass_('viewed completions are suppressed and do not appear after leaving')
            goal(s, 'active')
            for i in range(3):
                event(s, 'task_started', turn_id='goal-' + str(i))
                event(s, 'task_complete', turn_id='goal-' + str(i))
                time.sleep(2.5)
                assert not present(s)
            event(s, 'task_started', turn_id='goal-final')
            goal(s, 'complete')
            time.sleep(2.5)
            assert not present(s)
            event(s, 'task_complete', turn_id='goal-final')
            wait(lambda: present(s), 'final goal completion')
            pass_('goal intermediate turns stay silent; completion waits for final reply')
            native('click', s)
            wait(lambda: not present(s), 'goal notification cleared')
            # Same app is focused, but the source pane is in a hidden window.
            tm('select-window', '-t', pane2)
            goal(s, 'active')
            event(s, 'task_started', turn_id='async')
            question(s, 'q1')
            wait(lambda: present(s), 'background pane question while app is focused')
            event(s, 'task_complete', turn_id='async')
            event(s, 'task_started', turn_id='auto-continue')
            time.sleep(.7)
            assert present(s)
            answer(s, 'q1')
            wait(lambda: not present(s), 'async answer removes notification')
            pass_('async questions survive accepted/goal continuation and clear only after answer')
            question(s, 'q2')
            wait(lambda: present(s), 'new question notification')
            native('dismiss', s)
            time.sleep(1.5)
            assert not present(s)
            pass_('manual dismissal does not re-alert the same pending question')
            answer(s, 'q2')
            native('hide')
            second = s + '-second'; setup(second, pane2)
            goal(second, 'active')
            event(second, 'task_started', turn_id='independent')
            question(s, 'q3'); question(second, 'q4')
            wait(lambda: present(s) and present(second), 'independent sessions notify')
            answer(s, 'q3')
            wait(lambda: not present(s) and present(second), 'answer only clears matching session')
            pass_('two sessions have independent notification and dismissal state')
            # The title bar switch uses the actual NSButton target/action.
            subprocess.run([str(binary), 'msg', '--socket', str(socket), 'create-window',
                            '-e', '/bin/sleep', '1200'], check=True)
            wait(lambda: len(windows()) == 2, 'second native window')
            native('toggle')
            wait(lambda: all(not w['codex_notifications_enabled'] for w in windows()), 'title bar switch off')
            wait(lambda: not present(second), 'switch off withdraws pending notification')
            buttons = json.loads((root / 'buttons.json').read_text())
            assert len(buttons) == 2 and all('已关闭' in b['label'] and b['hasImage'] and b['template'] for b in buttons), buttons
            frames = [b['frame'] for b in buttons]
            probe_socket = root / 'probe.sock'
            probe_root = root / 'probe-native'; probe_root.mkdir()
            probe_env = dict(env, ALACRITTY_NOTIFICATION_ACCEPTANCE=str(probe_root))
            probe = subprocess.Popen([str(binary), '--config-file', '/dev/null', '--socket', str(probe_socket),
                '-o', 'keyboard.global_hotkey.enabled=false', '-o', 'window.startup_mode="Minimized"',
                '-e', '/bin/sleep', '60'], env=probe_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                snapshot = wait(lambda: json.loads(n.ipc(probe_socket, {'ProgramStatus': {'window_id': -1}})['ProgramStatus'])[0], 'new app preference read')
                assert snapshot['codex_notifications_enabled'] is False
            finally:
                probe.terminate()
                try: probe.wait(timeout=5)
                except subprocess.TimeoutExpired: probe.kill(); probe.wait()
            question(s, 'muted-question')
            time.sleep(1)
            assert not present(s)
            native('toggle')
            wait(lambda: all(w['codex_notifications_enabled'] for w in windows()), 'title bar switch on')
            buttons = json.loads((root / 'buttons.json').read_text())
            assert [b['frame'] for b in buttons] == frames
            assert all('已开启' in b['label'] for b in buttons)
            native('hide')
            time.sleep(1)
            assert not present(s)
            answer(s, 'muted-question'); question(s, 'after-enable')
            wait(lambda: present(s), 'new event after enabling notifications')
            answer(s, 'after-enable')
            wait(lambda: not present(s), 'answered new question cleared')
            pass_('title bar bell toggles, clears, synchronizes windows, persists across processes and keeps layout stable')
            hook(second, 'SessionEnd')
            wait(lambda: not present(second), 'session end removes notification')
            pass_('session exit withdraws pending notification')
            hook(s, 'SessionEnd')
            print(json.dumps({'result': 'PASS', 'checks': results}, ensure_ascii=False, indent=2), flush=True)
        except Exception:
            if (root / 'app.log').exists(): print((root / 'app.log').read_text(), file=sys.stderr)
            for p in state.rglob('status.json'): print(p.name, p.read_text(), file=sys.stderr)
            for p in state.rglob('error.txt'): print(p.name, p.read_text(), file=sys.stderr)
            raise
        finally:
            for pid in workers:
                try: os.kill(pid, signal.SIGTERM)
                except ProcessLookupError: pass
            # Remove only this test's session notifications before closing its app.
            if process and process.poll() is None:
                try: native('restore')
                except (AssertionError, OSError): pass
                for session in locals().get('sessions', {}):
                    try: n.ipc(socket, {'AgentNotify': {'session': session, 'kind': 'clear', 'window_id': window['window_id']}})
                    except (OSError, ValueError): pass
                time.sleep(.2)
                process.terminate()
                try: process.wait(timeout=5)
                except subprocess.TimeoutExpired: process.kill(); process.wait()
            subprocess.run([str(tmux), '-S', str(tmux_socket), 'kill-server'], capture_output=True)
            db.close()


if __name__ == '__main__':
    main(sys.argv[1])
