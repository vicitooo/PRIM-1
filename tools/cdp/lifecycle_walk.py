"""One bounded Generic PowerShell lifecycle walk in an empty owned preview.

Root attests the running process/frozen artifact. This helper checks the public
runtime/work paths and inventory before creating three normal Generic panes.
No live 9222, OS clipboard, AI harness, private product hook, app restart or build.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import sys
import time
import traceback
from urllib.parse import urlparse

sys.path.insert(0, str(Path(__file__).resolve().parent))
from cdp import CdpClient, list_targets
from output_load_walk import Walk as OutputWalk, normalized, require, wait
from websocket import WebSocketTimeoutException


STREAM_LINES = 200  # 51,200 printable bytes, paced over about twenty seconds.
OBSERVER = r"""(async()=>{
  if(window.__lifecycleWalk)throw Error('lifecycle fixture already exists');
  const bindings=__BINDINGS__;
  const w=window.__lifecycleWalk={panes:{},trace:[],omitted:0,unknown:0,stop:null};
  for(const [sid,b] of Object.entries(bindings))w.panes[sid]={...b,events:0,chars:0,
    lastSequence:0,regressions:0,wrongRun:0,gaps:0,markers:{},carry:'',
    closed_ms:null,exit_ms:null,deleted_ms:null,exit_requested:null};
  w.unlisten=await window.__TAURI__.event.listen('runtime://event',({payload:e})=>{
    if(!['session_output','session_state','session_exit','session_deleted','ui_output_gap'].includes(e.event))return;
    const sid=e.identity?.session_id??e.session_id,p=w.panes[sid],ms=performance.now();
    if(!p){w.unknown++;return;}
    const terminal=e.event==='session_exit'||(e.event==='session_state'&&e.state==='closed');
    const generations=(terminal&&p.stop_generation!==undefined)?[p.stop_generation]:
      (e.event==='session_state'?[p.generation,p.stop_generation??p.generation]:[p.generation]);
    if(e.event!=='session_deleted'&&(e.identity?.run_id!==p.run_id||!generations.includes(e.identity?.generation))){
      p.wrongRun++;return;
    }
    const receipt={event:e.event,session_id:sid,sequence:e.identity?.sequence??null,ms};
    if(e.event==='session_state')receipt.state=e.state;
    if(w.trace.length<128)w.trace.push(receipt);else w.omitted++;
    if(e.event==='ui_output_gap'){p.gaps++;return;}
    if(e.event==='session_state'&&e.state==='closed'&&p.closed_ms===null)p.closed_ms=ms;
    if(e.event==='session_exit'){p.exit_ms??=ms;p.exit_requested=e.requested;}
    if(e.event==='session_deleted'){p.deleted_ms??=ms;return;}
    if(e.event!=='session_output')return;
    if(!Number.isSafeInteger(e.identity.sequence)||e.identity.sequence<=p.lastSequence)p.regressions++;
    p.lastSequence=e.identity.sequence;p.events++;
    const text=p.carry+e.chunk,offset=p.chars-p.carry.length;
    const pattern=new RegExp(p.prefix+'([A-Z_]+[0-9]*(?:=[a-z |]+)?)!','g');
    for(const m of text.matchAll(pattern))if(offset+m.index+m[0].length>p.chars){
      if(Object.keys(p.markers).length>=210&&!Object.hasOwn(p.markers,m[1])){p.regressions++;continue;}
      p.markers[m[1]]=(p.markers[m[1]]??0)+1;
    }
    p.chars+=e.chunk.length;p.carry=text.slice(-256);
  });
  w.snapshot=()=>({panes:Object.fromEntries(Object.entries(w.panes).map(([sid,p])=>{
    const {carry,...rest}=p;return [sid,rest];})),trace:w.trace,omitted:w.omitted,unknown:w.unknown,stop:w.stop});
  return true;
})()"""


def validate_inventory(state, runtime, work, rooms, panes, closing=None):
    require(normalized(state['runtime_dir']) == normalized(runtime), 'wrong scratch runtime')
    require(normalized(state['workspace_preference']) == normalized(work), 'wrong scratch work root')
    sessions = {s['session_id']: s for s in state['sessions']}
    require(set(sessions) <= set(panes) and set(panes) - set(sessions) <= {closing},
            'unexpected/missing session or initial scratch not empty')
    require({r['room_id'] for r in state['rooms']} == set(rooms), 'unexpected/missing room')
    for room in state['rooms']:
        require(room['label'] == rooms[room['room_id']], 'room identity changed')
        expected = {sid for sid, p in panes.items() if p['room_id'] == room['room_id']}
        actual = set(room['member_ids'])
        require(actual <= expected and expected - actual <= {closing}, 'room membership changed')
    for sid, s in sessions.items():
        p = panes[sid]
        require(s['driver'] == 'generic_terminal' and s['permission_profile'] == 'normal', 'non-normal Generic refused')
        require(s['label'] == p['label'] and normalized(s['working_dir']) == normalized(work), 'fixture identity/work changed')
        if 'run_id' in p:
            original = s['generation'] == p['generation'] and s['run_id'] in (None, p['run_id'])
            admitted = 'stop_generation' in p and s['generation'] == p['stop_generation'] and s['run_id'] is None
            require(original or admitted, 'fixture run replaced')
            require(not s['running'] or admitted or s['run_id'] == p['run_id'], 'running fixture has no expected run')
        else:
            require(not s['running'], 'unbound fixture run refused')
    return state


def validate_observation(data):
    require(not data['unknown'], 'unexpected session event')
    for p in data['panes'].values():
        require(not (p['wrongRun'] or p['regressions'] or p['gaps']), 'output identity/order/gap anomaly')
        require(all(count == 1 for count in p['markers'].values()), 'duplicate output marker')
    if data['stop']:
        require(not data['stop'].get('error'), 'Stop command rejected: ' + str(data['stop'].get('error')))
        if data['stop'].get('returned_ms') is not None:
            pane = data['panes'][data['stop']['session_id']]
            snapshot = data['stop']['snapshot']
            require(snapshot['generation'] == pane.get('stop_generation') and snapshot['run_id'] is None,
                    'Stop returned an unexpected admission identity')
    return data


def stop_overlap(stop, closed_ms, actions):
    require(stop.get('returned_ms') is not None and closed_ms is not None, 'missing Stop return or terminal closure')
    require(stop['returned_ms'] >= stop['requested_ms'] and closed_ms >= stop['requested_ms'], 'invalid Stop timings')
    for action in actions:
        require(action['end_ms'] >= action['start_ms'] >= stop['requested_ms'], 'invalid action interval')
        action['command_pending_through_action'] = action['end_ms'] < stop['returned_ms']
        action['terminal_open_through_action'] = action['end_ms'] < closed_ms
    command_pending = bool(actions) and all(a['command_pending_through_action'] for a in actions)
    cleanup_pending = bool(actions) and all(a['terminal_open_through_action'] for a in actions)
    slow_cleanup = cleanup_pending and closed_ms - stop['requested_ms'] >= 2000
    return {'command_return_ms': stop['returned_ms'] - stop['requested_ms'],
            'terminal_closed_ms': closed_ms - stop['requested_ms'], 'actions': actions,
            'command_pending_responsiveness': 'MEASURED' if command_pending else 'UNMEASURED',
            'actions_overlap_cleanup': cleanup_pending,
            'slow_cleanup_minimum_ms': 2000,
            'slow_cleanup_responsiveness': 'MEASURED' if slow_cleanup else 'UNMEASURED',
            'limit': 'Closure is the renderer observation of session_state=closed, not native completion time. Fast Stop/cleanup does not measure the slow case; Ctrl-C ignore does not ensure slow force-kill.'}


class LifecycleWalk(OutputWalk):
    def __init__(self, *args):
        super().__init__(*args)
        self.closing = None
        self.observer_installed = False

    def state(self):
        return validate_inventory(self.invoke('bootstrap'), self.args.expected_runtime,
                                  self.args.expected_work, self.rooms, self.panes, self.closing)

    def connect(self):
        require(1024 <= self.args.port <= 65535 and self.args.port != 9222, 'non-live explicit preview port required')
        runtime, work = normalized(self.args.expected_runtime), normalized(self.args.expected_work)
        require(not (runtime == work or runtime.startswith(work+'\\') or work.startswith(runtime+'\\')), 'runtime/work overlap')
        require(Path(self.args.expected_runtime).is_dir() and Path(self.args.expected_work).is_dir(), 'scratch directories missing')
        targets = [t for t in list_targets(self.args.port) if t.get('type') == 'page']
        require(len(targets) == 1, 'ambiguous preview page')
        url = urlparse(targets[0]['webSocketDebuggerUrl'])
        require(url.scheme == 'ws' and url.hostname in ('localhost', '127.0.0.1', '::1') and url.port == self.args.port,
                'wrong preview websocket endpoint')
        self.client = CdpClient(targets[0]['webSocketDebuggerUrl'])
        require(self.invoke('trial_status') is None, 'trial mode refused')
        self.state()  # Empty owned sets intentionally refuse all prior fixtures.
        self.record('empty_scratch', {'runtime': self.args.expected_runtime, 'work': self.args.expected_work, 'port': self.args.port})
        require('error' not in self.client.send('Page.enable'), 'cannot observe Close confirmation')

    def create(self):
        label = 'Lifecycle ' + self.token
        rid = self.invoke('create_room', {'request': {'label': label, 'member_ids': [], 'brief_on_join': False}})['room_id']
        self.rooms[rid] = label
        self.record('created_room', {'room_id': rid, 'label': label})
        for role in ('stream', 'stop', 'close'):
            self.state()
            label = 'Lifecycle ' + self.token + ' ' + role
            s = self.invoke('create_session', {'request': {'label': label, 'driver': 'generic_terminal', 'permission_profile': 'normal', 'room_id': rid}})
            sid = s['session_id']
            self.panes[sid] = {'room_id': rid, 'label': label, 'role': role, 'prefix': 'LC_'+self.token+'_'}
            self.record('created_session', {'session_id': sid, **self.panes[sid]})
            self.select(sid)
            s = self.invoke('start_session', {'request': {'session_id': sid}})
            require(s['running'] and s['run_id'], 'Generic PowerShell failed to start')
            self.panes[sid].update(run_id=s['run_id'], generation=s['generation'])
            self.record('started_session', {'session_id': sid, **self.panes[sid]})
        self.state()
        self.js(OBSERVER.replace('__BINDINGS__', json.dumps(self.panes)))
        self.observer_installed = True
        for sid in self.panes:
            self.input(sid, self.prefix() + "[Console]::WriteLine($lc+'READY!');")
        wait('three PowerShell readiness markers', lambda: all(p['markers'].get('READY') == 1 for p in self.observation()['panes'].values()))
        return list(self.panes)

    def prefix(self):
        # Full marker never occurs in echoed input, so echoes cannot pass output checks.
        return "$lc='LC_'+'" + self.token + "'+'_';"

    def observation(self):
        return validate_observation(self.js('window.__lifecycleWalk.snapshot()'))

    def marker(self, sid, marker):
        return self.observation()['panes'][sid]['markers'].get(marker) == 1

    def arm_stop(self, sid):
        # Admission advances generation and clears run_id; output already queued
        # remains on the original run. Bind the one requested transition only.
        p = self.panes[sid]
        p['stop_generation'] = p['generation'] + 1
        if self.observer_installed:
            self.js('window.__lifecycleWalk.panes['+json.dumps(sid)+'].stop_generation='+str(p['stop_generation']))

    def click(self, selector):
        self.js("(()=>{const e=document.querySelector("+json.dumps(selector)+");if(!e||e.disabled||!e.getClientRects().length)throw Error('DOM action unavailable');e.click()})()")

    def type_text(self, selector, text):
        self.js("(()=>{const e=document.querySelector("+json.dumps(selector)+");if(!e||e.disabled||!e.getClientRects().length)throw Error('input unavailable');e.focus();if(document.activeElement!==e)throw Error('input focus failed')})()")
        require('error' not in self.client.send('Input.insertText', {'text': text}), 'DOM text input failed')

    def enter(self):
        for kind in ('keyDown', 'keyUp'):
            require('error' not in self.client.send('Input.dispatchKeyEvent', {'type': kind, 'key': 'Enter', 'code': 'Enter', 'windowsVirtualKeyCode': 13, 'text': '\r' if kind == 'keyDown' else ''}), 'Enter failed')

    def paste_case(self, sid):
        self.select(sid)
        selector = '[data-terminal="'+sid+'"] .xterm-helper-textarea'
        script = "& {\r\n" + self.prefix() + "\r\n$lcA='alpha beta';\r\n$lcB='gamma';\r\n[Console]::WriteLine($lc+'PASTE='+$lcA+'|'+$lcB+'!');\r\n}"
        self.js("(()=>{const e=document.querySelector("+json.dumps(selector)+");if(!e)throw Error('terminal input missing');e.focus();const d=new DataTransfer();d.setData('text/plain',"+json.dumps(script)+");const p=new ClipboardEvent('paste',{bubbles:true,cancelable:true,clipboardData:d});e.dispatchEvent(p);if(!p.defaultPrevented)throw Error('product paste handler did not consume event')})()")
        self.enter()
        wait('exact multiline paste result', lambda: self.marker(sid, 'PASTE=alpha beta|gamma'))
        self.record('generic_multiline_paste', {'session_id': sid, 'input': script, 'expected_result': self.panes[sid]['prefix']+'PASTE=alpha beta|gamma!', 'method': 'synthetic ClipboardEvent -> product paste handler; DOM Enter', 'coverage': 'Generic text mechanics only; no OS clipboard, Ctrl+V, Wispr or installed-harness claim', 'screenshot': self.screenshot('generic-paste')})

    def close_ui(self, sid):
        """Real tab Close; dialog telemetry is optional, product postconditions are not."""
        before = self.state()
        self.close_other_fixtures = {s['session_id']: {k: s[k] for k in ('run_id', 'generation', 'running', 'output_draining')}
                                     for s in before['sessions'] if s['session_id'] != sid}
        self.closing = sid
        self.arm_stop(sid)
        label = self.panes[sid]['label']
        selector = '[data-tab-action="close"][data-session="'+sid+'"]'
        expression = "(()=>{const e=document.querySelector("+json.dumps(selector)+");if(!e||!e.getClientRects().length)throw Error('Close unavailable');setTimeout(()=>{window.__lifecycleWalk.close_requested_ms=performance.now();e.click()},50);return true})()"
        client = self.client
        click_id = client._next_id
        client._next_id += 1
        receipt = {'session_id': sid, 'protocol': [], 'omitted_messages': 0,
                   'dialog_observation': 'UNMEASURED', 'reply_ids': [],
                   'confirm_looks_native_before_click': self.js("String(window.confirm).includes('[native code]')"),
                   'other_fixtures_before': self.close_other_fixtures}
        started = time.monotonic()

        def protocol(direction, message):
            raw = json.dumps(message)
            if len(receipt['protocol']) < 64:
                receipt['protocol'].append({'direction': direction, 'ms': (time.monotonic()-started)*1000,
                                            'method': message.get('method'), 'id': message.get('id'),
                                            'json': raw[:2048], 'truncated_chars': max(0, len(raw)-2048)})
            else:
                receipt['omitted_messages'] += 1

        command = {'id': click_id, 'method': 'Runtime.evaluate', 'params': {'expression': expression, 'returnByValue': True}}
        deadline, replies, confirm_id, dialog = started+2, set(), None, None
        try:
            require(receipt['confirm_looks_native_before_click'], 'Close confirmation function is not native')
            protocol('send', command)
            client.ws.send(json.dumps(command))
            while time.monotonic() < deadline:
                client.ws.settimeout(max(.001, deadline-time.monotonic()))
                try:
                    message = json.loads(client.ws.recv())
                except WebSocketTimeoutException as error:
                    receipt['receive_timeout'] = repr(error)
                    break
                protocol('receive', message)
                if message.get('method') == 'Page.javascriptDialogOpening':
                    params = message['params']
                    matches = dialog is None and params.get('type') == 'confirm' and params.get('message', '').startswith('Close "'+label+'" (') and 'stops it' in params['message']
                    confirm_id = client._next_id
                    client._next_id += 1
                    command = {'id': confirm_id, 'method': 'Page.handleJavaScriptDialog', 'params': {'accept': matches}}
                    protocol('send', command)
                    client.ws.send(json.dumps(command))
                    require(matches, 'unexpected Close confirmation (rejected)')
                    require(dialog is None, 'duplicate Close confirmation')
                    dialog = {'message': params['message'], 'observed_utc': datetime.now(timezone.utc).isoformat()}
                    receipt.update(dialog=dialog, dialog_observation='MEASURED')
                if message.get('id') in (click_id, confirm_id) and 'id' in message:
                    require('error' not in message and 'exceptionDetails' not in message.get('result', {}), 'Close CDP action failed')
                    replies.add(message['id'])
                    receipt['reply_ids'] = sorted(replies)
                if dialog and {click_id, confirm_id} <= replies:
                    break
            receipt['receive_seconds'] = time.monotonic()-started
            receipt['protocol_complete'] = bool(dialog and {click_id, confirm_id} <= replies)
            require(click_id in replies, 'Close click scheduling reply not observed')
            require(dialog is None or confirm_id in replies, 'Close confirmation reply not observed')
            # WebView can complete the actual UI action without publishing the
            # dialog event to this CDP client. Silence is never an acceptance.
            client.ws.settimeout(10)
            receipt['postconditions'] = wait('exact UI Close postconditions', lambda: self.close_completed(sid), 15)
            return receipt['postconditions']
        except Exception as error:
            receipt['error'] = repr(error)
            raise
        finally:
            client.ws.settimeout(10)
            self.record('ui_close_protocol', receipt)

    def close_completed(self, sid):
        state = self.state()
        others = {s['session_id']: {k: s[k] for k in ('run_id', 'generation', 'running', 'output_draining')}
                  for s in state['sessions'] if s['session_id'] != sid}
        require(others == self.close_other_fixtures, 'other fixture identity/running state changed during Close')
        pane = self.observation()['panes'][sid]
        dom = self.js("({requested_ms:window.__lifecycleWalk.close_requested_ms??null,tab_present:Boolean(document.querySelector('[data-session-tab=\""+sid+"\"]'))})")
        if (dom['requested_ms'] is None or pane['closed_ms'] is None or pane['exit_ms'] is None
                or pane['deleted_ms'] is None or pane['exit_requested'] is not True
                or dom['tab_present'] or any(s['session_id'] == sid for s in state['sessions'])
                or any(sid in r['member_ids'] for r in state['rooms'])):
            return None
        require(pane['deleted_ms'] >= pane['exit_ms'] >= pane['closed_ms'] >= dom['requested_ms'], 'Close lifecycle order invalid')
        return {'session_id': sid, 'requested_ms': dom['requested_ms'], 'closed_ms': pane['closed_ms'],
                'exit_ms': pane['exit_ms'], 'deleted_ms': pane['deleted_ms'], 'exit_requested': True,
                'inventory_removed': True, 'membership_removed': True, 'tab_removed': True, 'other_fixtures_unchanged': True}

    def run(self):
        stream, stop, close = self.create()
        self.paste_case(close)
        # No background child process or guaranteed slow kill: finite PowerShell work.
        self.input(stop, self.prefix()+"[Console]::TreatControlCAsInput=$true;[Console]::WriteLine($lc+'STOP_READY!');Start-Sleep -Seconds 30;")
        wait('Stop target busy marker', lambda: self.marker(stop, 'STOP_READY'))
        self.input(stream, self.prefix()+"for($lcN=1;$lcN -le 200;$lcN++){[Console]::WriteLine(($lc+'STREAM_'+('{0:D3}' -f $lcN)+'!').PadRight(256,'x'));Start-Sleep -Milliseconds 100};[Console]::WriteLine($lc+'STREAM_END!');")
        wait('stream begins', lambda: self.marker(stream, 'STREAM_001'))
        self.select(stop)
        self.state()
        self.arm_stop(stop)
        self.js("(()=>{const w=window.__lifecycleWalk;w.stop={session_id:"+json.dumps(stop)+",requested_ms:performance.now()};window.__TAURI__.core.invoke('stop_session',{request:{session_id:"+json.dumps(stop)+"}}).then(s=>Object.assign(w.stop,{returned_ms:performance.now(),snapshot:{running:s.running,lifecycle_state:s.lifecycle_state,output_draining:s.output_draining,run_id:s.run_id,generation:s.generation}}),e=>Object.assign(w.stop,{returned_ms:performance.now(),error:String(e)}));return true})()")
        actions = []

        def action(name, fn):
            start = self.js('performance.now()')
            value = fn()
            actions.append({'name': name, 'start_ms': start, 'end_ms': self.js('performance.now()'), 'value': value})

        def switch():
            self.click('[data-session-tab="'+close+'"]')
            value = self.visible()
            require(value['visible_terminals'][0]['session_id'] == close, 'DOM tab switch failed')
            return value

        action('actual_tab_switch', switch)

        def terminal_input():
            self.type_text('[data-terminal="'+close+'"] .xterm-helper-textarea', self.prefix()+"[Console]::WriteLine($lc+'INPUT=typed!');")
            self.enter()
            wait('DOM typed command result', lambda: self.marker(close, 'INPUT=typed'))
            return 'exact split-literal INPUT=typed marker observed'

        action('DOM_typing_and_output', terminal_input)
        draft = 'Lifecycle draft '+self.token+' remains unsent'

        def draft_check():
            self.click('#context-name-chip')
            require(self.js("document.querySelector('#room-message').value") == '', 'fixture composer unexpectedly nonempty')
            self.type_text('#room-message', draft)
            self.click('[data-session-tab="'+stream+'"]')
            self.click('#context-name-chip')
            require(self.js("document.querySelector('#room-message').value") == draft, 'draft lost across DOM navigation')
            return {'text': draft, 'unsent': True}

        action('DOM_draft_and_navigation', draft_check)
        wait('Stop return and terminal closed/exit events', lambda: (o if o['stop'].get('returned_ms') is not None and o['panes'][stop]['closed_ms'] is not None and o['panes'][stop]['exit_requested'] is True else None) if (o := self.observation()) else None, 20)
        o = self.observation()
        self.record('stop_timing', stop_overlap(o['stop'], o['panes'][stop]['closed_ms'], actions))
        require(o['panes'][stop]['exit_requested'] is True, 'Stop did not produce requested exit')
        self.click('[data-session-tab="'+close+'"]')
        before = self.observation()['panes'][stream]
        require(not before['markers'].get('STREAM_END'), 'stream ended before UI Close; overlap unmeasured')
        completed = self.close_ui(close)
        self.record('ui_close', {**completed, 'command_return': 'UNMEASURED: UI owns Stop/Delete promises; no invoke monkeypatch', 'stream_events_before': before['events'], 'stream_events_after': self.observation()['panes'][stream]['events']})
        wait('finite stream END', lambda: self.marker(stream, 'STREAM_END'), 35)
        o = self.observation()
        require(all(o['panes'][stream]['markers'].get('STREAM_'+str(n).zfill(3)) == 1 for n in range(1, STREAM_LINES+1)), 'missing numbered stream output')
        require(o['panes'][stream]['closed_ms'] is None, 'stream pane closed unexpectedly')
        self.click('#context-name-chip')
        require(self.js("document.querySelector('#room-message').value") == draft, 'draft lost after Close')
        self.record('final_draft', {'text': draft, 'screenshot': self.screenshot('draft-after-close')})
        self.click('[data-session-tab="'+stream+'"]')
        self.record('stream_complete', {'observation': o, 'screenshot': self.screenshot('stream-after-close'), **self.visible()})
        self.result['assertions_passed'] = True

    def cleanup(self):
        self.result['owned_fixtures'] = {'rooms': self.rooms, 'panes': self.panes, 'ui_close_target': self.closing}
        if not self.client:
            return
        # Stop every owned remaining run even if an earlier postcondition failed.
        # Never stop a replacement or unrelated run; retain errors in the result.
        for sid in self.panes:
            try:
                state = self.state()
                session = next((s for s in state['sessions'] if s['session_id'] == sid), None)
                p = self.panes[sid]
                if session and session['running'] and session['generation'] == p['generation'] and session['run_id'] == p['run_id']:
                    self.arm_stop(sid)
                    self.invoke('stop_session', {'request': {'session_id': sid}})
            except Exception as error:
                self.result.setdefault('cleanup_errors', []).append({'session_id': sid, 'error': repr(error)})
        try:
            final = wait('all owned fixtures fully closed', lambda: (s if all(not p['running'] and not p['output_draining'] and p['lifecycle_state'] == 'closed' for p in s['sessions']) else None) if (s := self.state()) else None, 20)
            self.record('cleanup_sessions', [{'session_id': s['session_id'], 'running': s['running'], 'output_draining': s['output_draining'], 'lifecycle_state': s['lifecycle_state']} for s in final['sessions']])
            if self.observer_installed:
                self.record('final_observation', self.observation())
        except Exception as error:
            self.result.setdefault('cleanup_errors', []).append({'action': 'verify_closed', 'error': repr(error)})
        finally:
            if self.observer_installed:
                try:
                    self.js('(async()=>{await window.__lifecycleWalk.unlisten();delete window.__lifecycleWalk;return true})()')
                except Exception as error:
                    self.result.setdefault('cleanup_errors', []).append({'action': 'unlisten', 'error': repr(error)})
            self.client.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, required=True)
    parser.add_argument('--expected-runtime', required=True)
    parser.add_argument('--expected-work', required=True)
    parser.add_argument('--out-parent', type=Path, required=True)
    args = parser.parse_args()
    out = args.out_parent / ('lifecycle-'+datetime.now(timezone.utc).strftime('%Y%m%d-%H%M%S%fZ'))
    out.mkdir()
    result = {'steps': [], 'assertions_passed': False, 'acceptance_claim': 'none',
              'driver_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              'limits': 'Three Generic PowerShell panes, one visible at a time. Public event receipt is not paint/native latency. Synthetic DOM paste is not OS clipboard, Ctrl+V, Wispr or installed-harness coverage. Root attests frozen process ownership and walks screenshots.'}
    with (out/'steps.jsonl').open('x', encoding='utf-8') as journal:
        walk = LifecycleWalk(args, out, result, journal)
        try:
            walk.connect()
            walk.run()
        except Exception:
            result['error'] = traceback.format_exc()
        finally:
            walk.cleanup()
            result['finished_utc'] = datetime.now(timezone.utc).isoformat()
            with (out/'result.json').open('x', encoding='utf-8') as stream:
                json.dump(result, stream, indent=2)
            print(out)
    return 0 if result['assertions_passed'] and not result.get('cleanup_errors') else 1


if __name__ == '__main__':
    raise SystemExit(main())
