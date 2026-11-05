#!/usr/bin/env python3
"""Notification policy, rollout adapter, wire protocol and routing regressions."""
import importlib.util
import io
import json
import os
from pathlib import Path
import socket
import struct
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent / 'terminal-profile'))
import codex_notifications as n


def record(kind, **fields):
    return {'type': 'event_msg', 'payload': {'type': kind, **fields}}


def item(kind, **fields):
    return {'type': 'response_item', 'payload': {'type': kind, **fields}}


class Policy(unittest.TestCase):
    def setUp(self):
        self.s = n.State()
        self.s.set_goal(None)
        self.s.rollout(record('task_started', turn_id='turn1'), 0)

    def question(self, call='q1', async_=False, count=1):
        self.s.rollout(item('function_call', name='request_user_input' + ('_async' if async_ else ''),
                            call_id=call, arguments=json.dumps({'questions': [{}] * count})), 1)

    def test_normal_completion_waits_for_settle(self):
        self.s.rollout(record('task_complete', turn_id='turn1'), 1)
        self.assertIsNone(self.s.desired(2))
        self.assertEqual(self.s.desired(4), ('done', 'turn:turn1'))

    def test_goal_never_notifies_between_turns(self):
        self.s.set_goal({'status': 'active', 'createdAt': 42})
        for i in range(30):
            self.s.rollout(record('task_started', turn_id=str(i)), i * 10)
            self.s.hook({'event': 'Stop', 'turn_id': str(i)}, i * 10 + 1)
            self.assertIsNone(self.s.desired(i * 10 + 9))
        self.s.set_goal({'status': 'complete', 'createdAt': 42})
        self.assertEqual(self.s.desired(400), ('goal', 'goal:42'))

    def test_goal_complete_waits_for_final_reply(self):
        self.s.set_goal({'status': 'complete', 'createdAt': 42})
        self.assertIsNone(self.s.desired(100))
        self.s.rollout(record('task_complete', turn_id='turn1'), 101)
        self.assertEqual(self.s.desired(104)[0], 'goal')

    def test_new_task_after_completed_goal_notifies_normally(self):
        self.s.rollout(record('task_started', turn_id='new-task', started_at=200), 0)
        self.s.set_goal({'status': 'complete', 'createdAt': 10, 'updatedAt': 100})
        self.s.rollout(record('task_complete', turn_id='new-task'), 1)
        self.assertEqual(self.s.desired(4), ('done', 'turn:new-task'))

    def test_continuation_cancels_completion_candidate(self):
        self.s.hook({'event': 'Stop', 'turn_id': 'turn1'}, 1)
        self.s.rollout(record('task_started', turn_id='turn2'), 2)
        self.assertIsNone(self.s.desired(30))

    def test_repeated_stop_does_not_withdraw_delivered_completion(self):
        self.s.hook({'event': 'Stop', 'turn_id': 'turn1'}, 1)
        expected = self.s.desired(4)
        self.s.hook({'event': 'Stop', 'turn_id': 'turn1'}, 5)
        self.assertEqual(self.s.desired(5), expected)

    def test_unknown_goal_does_not_claim_success(self):
        self.s.goal = 'unknown'
        self.s.hook({'event': 'Stop'}, 1)
        self.assertIsNone(self.s.desired(30))

    def test_terminal_goal_states(self):
        self.s.hook({'event': 'Stop'}, 1)
        for status, expected in [('blocked', 'blocked'), ('budgetLimited', 'limited'),
                                 ('usageLimited', 'limited'), ('paused', None)]:
            self.s.set_goal({'status': status, 'createdAt': 42})
            result = self.s.desired(10)
            self.assertEqual(result[0] if result else None, expected)

    def test_system_usage_stop_without_final_reply(self):
        self.s.rollout(record('thread_goal_updated', goal={'status': 'usageLimited', 'createdAt': 42}), 1)
        self.assertEqual(self.s.desired(4)[0], 'limited')

    def test_sync_question_clears_on_answer(self):
        self.question()
        self.assertEqual(self.s.desired(2)[0], 'question')
        self.s.rollout(item('function_call_output', call_id='q1', output='{}'), 3)
        self.assertIsNone(self.s.desired(4))

    def test_async_accepted_does_not_mean_answered(self):
        self.question(async_=True, count=2)
        self.s.rollout(item('function_call_output', call_id='q1', output='{"accepted":true}'), 2)
        self.s.set_goal({'status': 'active', 'createdAt': 42})
        self.s.hook({'event': 'Stop'}, 3)
        self.s.rollout(record('task_started', turn_id='auto-next'), 4)
        self.assertEqual(self.s.desired(5), ('question', 'question:q1'))
        for i in range(2):
            text = '<send_user_message_question_reply>\n' + json.dumps([{
                'questionItemId': json.dumps(['request_user_input_async', 'q1', i]), 'answer': 'yes'}]) + '\n</send_user_message_question_reply>'
            self.s.rollout(item('message', role='user', content=[{'text': text}]), 6 + i)
            self.assertEqual(len(self.s.questions), 1 - i)
        self.assertIsNone(self.s.desired(9))

    def test_auto_context_does_not_dismiss_question(self):
        self.question(async_=True)
        self.s.rollout(item('message', role='user', content=[{'text': '<codex_internal_context source="goal">continue</codex_internal_context>'}],
                            internal_chat_message_metadata_passthrough={'content_item_metadata': [{'provenance': {'type': 'harness'}}]}), 2)
        self.assertEqual(self.s.desired(3)[0], 'question')

    def test_new_user_task_supersedes_question(self):
        self.question(async_=True)
        self.s.rollout(item('message', role='user', content=[{'text': 'new task'}],
                            internal_chat_message_metadata_passthrough={'content_item_metadata': [{'provenance': {'type': 'user'}}]}), 2)
        self.assertIsNone(self.s.desired(3))

    def test_automatic_approval_has_no_toast(self):
        self.s.hook({'event': 'PermissionRequest', 'key': 'p1'}, 1)
        self.assertIsNone(self.s.desired(1.5))
        self.s.hook({'event': 'PostToolUse', 'key': 'p1'}, 1.6)
        self.assertIsNone(self.s.desired(4))

    def test_pending_permission_and_interrupt(self):
        self.s.hook({'event': 'PermissionRequest', 'key': 'p1'}, 1)
        self.assertEqual(self.s.desired(3)[0], 'permission')
        self.s.hook({'event': 'Interrupt'}, 4)
        self.assertIsNone(self.s.desired(5))

    def test_slow_automatic_review_never_notifies(self):
        self.s.rollout({'type': 'turn_context', 'payload': {'approvals_reviewer': 'auto_review'}}, 0)
        self.s.hook({'event': 'PermissionRequest', 'key': 'p1'}, 1)
        self.assertIsNone(self.s.desired(100))

    def test_sessions_do_not_share_state(self):
        self.question()
        other = n.State()
        self.assertIsNone(other.desired(10))


class IO(unittest.TestCase):
    def test_readonly_goal_fallback_and_schema_guard(self):
        import sqlite3
        with tempfile.TemporaryDirectory() as d:
            db = sqlite3.connect(Path(d) / 'goals_1.sqlite')
            db.execute('CREATE TABLE thread_goals(thread_id TEXT, status TEXT, created_at_ms INTEGER, updated_at_ms INTEGER)')
            db.execute("INSERT INTO thread_goals VALUES('s', 'budget_limited', 1000, 2000)")
            db.commit()
            self.assertEqual(n.persisted_goal(d, 's'), {'status': 'budgetLimited', 'createdAt': 1, 'updatedAt': 2})
            self.assertIsNone(n.persisted_goal(d, 'missing'))
            db.execute('DROP TABLE thread_goals'); db.commit()
            with self.assertRaises(ValueError): n.persisted_goal(d, 's')
            db.close()

    def test_partial_records_wait_for_newline(self):
        f = io.BytesIO(b'{"x":1}\n{"y":2}')
        self.assertEqual(list(n.lines(f)), [{'x': 1}])
        self.assertEqual(f.tell(), 8)
        f.seek(0, 2); f.write(b'\n'); f.seek(8)
        self.assertEqual(list(n.lines(f)), [{'y': 2}])

    def test_only_exact_active_pane_is_visible(self):
        meta = {'socket': '/tmp/test-alacritty.sock', 'pane': '%2', 'tmux_socket': '/tmp/tmux-test', 'tmux_bin': '/app/tmux'}
        windows = [{'window_id': 9, 'shell_pid': 11, 'focused': True, 'codex_notifications_enabled': True}]
        def cmd(args):
            if 'list-panes' in args:
                return '%1\t$0\t1\t1\n%2\t$0\t0\t1'
            if 'list-clients' in args:
                return '/dev/ttys004\t$0'
            return '11 ttys004'
        with patch.object(n, 'ipc', return_value={'ProgramStatus': json.dumps(windows)}), patch.object(n, 'command', side_effect=cmd), patch.object(Path, 'glob', return_value=[]):
            result = n.routes(meta)
        self.assertEqual(len(result), 1)
        self.assertFalse(result[0][2])
        self.assertEqual(result[0][1], '/dev/ttys004')

    def test_websocket_masking_and_fragmentation(self):
        a, b = socket.socketpair()
        client = object.__new__(n.GoalClient); client.sock = a
        try:
            client.send({'hello': 'world'})
            data = b.recv(1024)
            self.assertTrue(data[1] & 128)
            mask, payload = data[2:6], data[6:]
            self.assertEqual(json.loads(bytes(v ^ mask[i % 4] for i, v in enumerate(payload))), {'hello': 'world'})
            b.sendall(b'\x01\x05{"x":' + b'\x80\x021}')
            self.assertEqual(client.receive(), {'x': 1})
        finally:
            a.close(); b.close()

    def test_private_state_rejects_symlink(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / 'link'; p.symlink_to(d)
            with self.assertRaises(OSError):
                n.private_dir(p)


if __name__ == '__main__':
    unittest.main()
