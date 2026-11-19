"""Offline safety/accounting checks; never connects to a desktop."""
import copy
import json
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch
from websocket import WebSocketTimeoutException

sys.path.insert(0, str(Path(__file__).resolve().parent))
from lifecycle_walk import OBSERVER, LifecycleWalk, stop_overlap, validate_inventory, validate_observation


class LifecycleSafetyTests(unittest.TestCase):
    def setUp(self):
        self.rooms = {'room': 'Owned'}
        self.panes = {'pane': {'room_id': 'room', 'label': 'Owned pane', 'run_id': 'run', 'generation': 1}}
        self.state = {'runtime_dir': 'D:/scratch/runtime', 'workspace_preference': 'D:/scratch/work',
                      'rooms': [{'room_id': 'room', 'label': 'Owned', 'member_ids': ['pane']}],
                      'sessions': [{'session_id': 'pane', 'label': 'Owned pane', 'driver': 'generic_terminal',
                                    'permission_profile': 'normal', 'working_dir': 'D:/scratch/work',
                                    'run_id': 'run', 'generation': 1, 'running': True}]}

    def check_state(self, state=None, **options):
        return validate_inventory(state or self.state, 'D:/scratch/runtime', 'D:/scratch/work',
                                  options.get('rooms', self.rooms), options.get('panes', self.panes), options.get('closing'))

    def test_initial_nonempty_and_unrelated_inventory_refused(self):
        with self.assertRaisesRegex(RuntimeError, 'initial scratch not empty'):
            self.check_state(rooms={}, panes={})
        self.state['sessions'].append({**self.state['sessions'][0], 'session_id': 'unrelated'})
        with self.assertRaises(RuntimeError):
            self.check_state()

    def test_wrong_scope_or_successor_run_refused(self):
        for field, value in [('driver', 'claude_code'), ('permission_profile', 'unsafe'),
                             ('working_dir', 'D:/live'), ('run_id', 'successor'), ('generation', 2)]:
            state = copy.deepcopy(self.state)
            state['sessions'][0][field] = value
            with self.subTest(field=field), self.assertRaises(RuntimeError):
                self.check_state(state)
        state = copy.deepcopy(self.state)
        state['runtime_dir'] = 'D:/live/runtime'
        with self.assertRaises(RuntimeError):
            self.check_state(state)

    def test_only_explicit_close_target_may_disappear(self):
        self.state['sessions'] = []
        self.state['rooms'][0]['member_ids'] = []
        with self.assertRaises(RuntimeError):
            self.check_state()
        self.check_state(closing='pane')

    def test_only_requested_stop_generation_and_cleared_run_are_allowed(self):
        self.state['sessions'][0].update(generation=2, run_id=None)
        with self.assertRaises(RuntimeError):
            self.check_state()
        self.panes['pane']['stop_generation'] = 2
        self.check_state()  # Admission may retain running=true during cleanup.
        self.state['sessions'][0]['running'] = False
        self.check_state()
        self.state['sessions'][0]['run_id'] = 'successor'
        with self.assertRaises(RuntimeError):
            self.check_state()

    def test_fast_stop_does_not_claim_slow_responsiveness(self):
        stop = {'requested_ms': 10, 'returned_ms': 11}
        result = stop_overlap(stop, 12, [{'name': 'DOM input', 'start_ms': 13, 'end_ms': 15}])
        self.assertEqual(result['slow_cleanup_responsiveness'], 'UNMEASURED')
        self.assertEqual(result['command_pending_responsiveness'], 'UNMEASURED')
        result = stop_overlap(stop, 30, [{'name': 'DOM input', 'start_ms': 13, 'end_ms': 15}])
        self.assertEqual(result['slow_cleanup_responsiveness'], 'UNMEASURED')
        self.assertTrue(result['actions_overlap_cleanup'])
        self.assertEqual(result['command_pending_responsiveness'], 'UNMEASURED')
        result = stop_overlap(stop, 2010, [{'name': 'DOM input', 'start_ms': 13, 'end_ms': 15}])
        self.assertEqual(result['slow_cleanup_responsiveness'], 'MEASURED')
        with self.assertRaises(RuntimeError):
            stop_overlap(stop, None, [])

    def test_cleanup_awaits_admitted_stop_without_second_command(self):
        self.panes['pane']['stop_generation'] = 2
        admitted = copy.deepcopy(self.state)
        admitted['sessions'][0].update(generation=2, run_id=None, lifecycle_state='busy', output_draining=False)
        closed = copy.deepcopy(admitted)
        closed['sessions'][0].update(running=False, lifecycle_state='closed')
        states = iter([admitted, closed])
        walk = object.__new__(LifecycleWalk)
        walk.panes, walk.rooms, walk.result = self.panes, self.rooms, {}
        walk.closing, walk.observer_installed = None, False
        walk.state = lambda: next(states)
        walk.invoke = lambda *_: self.fail('cleanup must not submit another Stop')
        walk.record = lambda *_: None
        class Client:
            def close(self):
                pass
        walk.client = Client()
        walk.cleanup()
        self.assertNotIn('cleanup_errors', walk.result)

    def test_observer_chunk_boundaries_and_anomalies(self):
        bindings = {'pane': {**self.panes['pane'], 'prefix': 'LC_0123456789_'}}
        script = """let callback;global.window={__TAURI__:{event:{listen:async(_,cb)=>{callback=cb;return ()=>{}}}}};
        (async()=>{await __OBSERVER__;
        const emit=(seq,chunk,extra={})=>callback({payload:{event:'session_output',identity:{session_id:'pane',run_id:'run',generation:1,sequence:seq},chunk,...extra}});
        emit(1,'LC_0123456789_ST');emit(2,'REAM_001!');emit(3,' unrelated');
        window.__lifecycleWalk.panes.pane.stop_generation=2;
        callback({payload:{event:'session_state',state:'closed',identity:{session_id:'pane',run_id:'run',generation:2,sequence:4}}});
        callback({payload:{event:'session_exit',requested:true,identity:{session_id:'pane',run_id:'run',generation:2,sequence:5}}});
        const good=window.__lifecycleWalk.snapshot();
        const frozen=JSON.parse(JSON.stringify(good));
        emit(2,'regression');
        callback({payload:{event:'ui_output_gap',identity:{session_id:'pane',run_id:'run',generation:1,sequence:4}}});
        emit(5,'bad run',{identity:{session_id:'pane',run_id:'other',generation:1,sequence:5}});
        for(let n=6;n<220;n++)emit(n,'x');
        console.log(JSON.stringify({good:frozen,bad:window.__lifecycleWalk.snapshot()}));})();"""
        script = script.replace('__OBSERVER__', OBSERVER.replace('__BINDINGS__', json.dumps(bindings)))
        result = subprocess.run(['node', '-e', script], capture_output=True, text=True, timeout=10, check=True)
        data = json.loads(result.stdout)
        validate_observation(data['good'])
        self.assertEqual(data['good']['panes']['pane']['markers'], {'STREAM_001': 1})
        self.assertTrue(data['good']['panes']['pane']['exit_requested'])
        self.assertIsNotNone(data['good']['panes']['pane']['closed_ms'])
        self.assertEqual(len(data['bad']['trace']), 128)
        self.assertGreater(data['bad']['omitted'], 0)
        self.assertNotIn('carry', data['bad']['panes']['pane'])
        with self.assertRaisesRegex(RuntimeError, 'identity/order/gap'):
            validate_observation(data['bad'])

    def close_walk(self, messages):
        class Socket:
            def __init__(self):
                self.sent, self.messages = [], list(messages)
            def settimeout(self, value):
                pass
            def send(self, value):
                self.sent.append(json.loads(value))
            def recv(self):
                if not self.messages:
                    raise WebSocketTimeoutException('fixture receive timeout')
                return json.dumps(self.messages.pop(0))
        class Client:
            _next_id = 1
            ws = Socket()
        walk = object.__new__(LifecycleWalk)
        walk.state = lambda: self.state
        walk.observer_installed = False
        walk.panes = self.panes
        walk.client = Client()
        walk.receipts = []
        walk.record = lambda name, value: walk.receipts.append((name, value))
        closed_state = {'sessions': [], 'rooms': [{'member_ids': []}]}
        pane = {'closed_ms': 20, 'exit_ms': 21, 'deleted_ms': 22, 'exit_requested': True}
        walk.state = lambda: closed_state
        walk.observation = lambda: {'panes': {'pane': pane}}
        walk.js = lambda expression: True if expression.startswith('String(window.confirm)') else {'requested_ms': 10, 'tab_present': False}
        return walk, pane

    def test_close_handles_real_dialog_without_confirm_override(self):
        walk, _ = self.close_walk([
            {'id': 1, 'result': {'result': {'value': True}}},
            {'method': 'Page.javascriptDialogOpening', 'params': {'type': 'confirm', 'message': 'Close "Owned pane" (abc)? This stops it.'}},
            {'id': 2, 'result': {}},
        ])
        walk.close_ui('pane')
        sent = walk.client.ws.sent
        self.assertIn('setTimeout', sent[0]['params']['expression'])
        self.assertNotIn('window.confirm', sent[0]['params']['expression'])
        self.assertEqual(sent[1]['method'], 'Page.handleJavaScriptDialog')
        self.assertEqual(sent[1]['params'], {'accept': True})
        self.assertEqual(walk.receipts[-1][1]['dialog_observation'], 'MEASURED')

    def test_missing_dialog_accepts_only_real_close_postconditions(self):
        walk, pane = self.close_walk([{'id': 1, 'result': {'result': {'value': True}}}])
        completed = walk.close_ui('pane')
        self.assertTrue(completed['tab_removed'] and completed['inventory_removed'])
        receipt = walk.receipts[-1][1]
        self.assertEqual(receipt['dialog_observation'], 'UNMEASURED')
        self.assertFalse(receipt['protocol_complete'])
        self.assertIn('fixture receive timeout', receipt['receive_timeout'])
        self.assertEqual(len(receipt['protocol']), 2)
        self.assertEqual(len(walk.client.ws.sent), 1)  # Never manufactures an acceptance.
        pane['deleted_ms'] = None
        self.assertIsNone(walk.close_completed('pane'))
        pane['deleted_ms'] = 22
        walk.js = lambda _: {'requested_ms': 10, 'tab_present': True}
        self.assertIsNone(walk.close_completed('pane'))

    def test_missing_dialog_timeout_failure_and_protocol_error_are_preserved(self):
        walk, pane = self.close_walk([{'id': 1, 'result': {'result': {'value': True}}}])
        pane['deleted_ms'] = None
        with patch('lifecycle_walk.wait', side_effect=RuntimeError('fixture postcondition timeout')):
            with self.assertRaisesRegex(RuntimeError, 'postcondition timeout'):
                walk.close_ui('pane')
        receipt = walk.receipts[-1][1]
        self.assertIn('receive_timeout', receipt)
        self.assertIn('postcondition timeout', receipt['error'])
        walk, _ = self.close_walk([{'id': 1, 'error': {'code': -1, 'message': 'fixture refusal'}}])
        with self.assertRaisesRegex(RuntimeError, 'Close CDP action failed'):
            walk.close_ui('pane')
        receipt = walk.receipts[-1][1]
        self.assertIn('fixture refusal', receipt['protocol'][-1]['json'])
        self.assertIn('Close CDP action failed', receipt['error'])
        walk, _ = self.close_walk([
            {'id': 1, 'result': {'result': {'value': True}}},
            {'method': 'Page.javascriptDialogOpening', 'params': {'type': 'confirm', 'message': 'Close "Owned pane" (abc)? This stops it.'}},
        ])
        with self.assertRaisesRegex(RuntimeError, 'confirmation reply not observed'):
            walk.close_ui('pane')

    def test_missing_click_reply_and_other_fixture_change_refused(self):
        walk, _ = self.close_walk([])
        with self.assertRaisesRegex(RuntimeError, 'scheduling reply not observed'):
            walk.close_ui('pane')
        self.assertIn('receive_timeout', walk.receipts[-1][1])
        walk, _ = self.close_walk([{'id': 1, 'result': {'result': {'value': True}}}])
        walk.close_ui('pane')
        walk.close_other_fixtures = {'other': {'run_id': 'bound', 'generation': 1, 'running': True, 'output_draining': False}}
        with self.assertRaisesRegex(RuntimeError, 'other fixture'):
            walk.close_completed('pane')


if __name__ == '__main__':
    unittest.main()
