"""Offline fixture checks only: no CDP connection or product runtime."""
import copy
import hashlib
import io
import json
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import output_load_walk as fixture


class FixtureTests(unittest.TestCase):
    def test_staged_and_frozen_paths_may_differ_but_both_hashes_must_match(self):
        with tempfile.TemporaryDirectory() as folder:
            frozen = Path(folder) / "immutable-candidate.exe"
            staged = Path(folder) / "prim1-v2.exe"
            payload = b"offline frozen image fixture"
            digest = hashlib.sha256(payload).hexdigest()
            frozen.write_bytes(payload)
            staged.write_bytes(payload)
            receipt = fixture.verify_build_images(frozen, staged, digest)
            self.assertEqual(receipt["frozen_exe"], str(frozen))
            self.assertEqual(receipt["staged_exe"], str(staged))
            self.assertNotEqual(receipt["frozen_exe"], receipt["staged_exe"])
            fixture.verify_build_images(frozen, frozen, digest)
            staged.write_bytes(b"changed staged image")
            with self.assertRaisesRegex(RuntimeError, "staged executable hash differs"):
                fixture.verify_build_images(frozen, staged, digest)
            staged.write_bytes(payload)
            frozen.write_bytes(b"changed frozen image")
            with self.assertRaisesRegex(RuntimeError, "frozen executable hash differs"):
                fixture.verify_build_images(frozen, staged, digest)

    def metrics(self):
        return {"epoch": "epoch-1", "parser_ack_tickets": 8, "max_ms": 5001,
                "latency_bucket_upper_bounds_ms": fixture.BUCKETS,
                "latency_histogram": [1] * 8, "over_250_ms": 4,
                "over_2000_ms": 2, "over_5000_ms": 1, "observer_entries": 1,
                "observer_max_quiet_ms": 2001, "current_observing_runs": 1,
                "current_bytes": 10, "peak_bytes": 11, "current_items": 1,
                "peak_items": 2}

    def test_metric_delta_and_reset_refusal(self):
        before = fixture.validate_metrics(self.metrics())
        after = copy.deepcopy(before)
        after["latency_histogram"][0] += 7
        after["parser_ack_tickets"] += 7
        fixture.validate_metrics(after)
        self.assertEqual(fixture.metrics_delta(before, after)["parser_ack_tickets"], 7)
        after["epoch"] = "reset"
        with self.assertRaisesRegex(RuntimeError, "epoch changed"):
            fixture.metrics_delta(before, after)
        after = self.metrics()
        after["over_2000_ms"] = 1
        with self.assertRaisesRegex(RuntimeError, "threshold mismatch"):
            fixture.validate_metrics(after)

    def test_empty_scratch_and_exact_membership_are_required(self):
        args = SimpleNamespace(expected_runtime="D:/scratch/runtime", expected_work="D:/scratch/work")
        walk = fixture.Walk(args, None, {"steps": []}, io.StringIO())
        snapshot = {"runtime_dir": args.expected_runtime, "workspace_preference": args.expected_work, "sessions": [], "rooms": []}
        walk.invoke = lambda *_: snapshot
        walk.state()
        snapshot["rooms"] = [{"room_id": "room", "member_ids": []}]
        with self.assertRaisesRegex(RuntimeError, "initial scratch not empty"):
            walk.state()
        walk.rooms["room"] = "Fixture"
        walk.panes["pane"] = {"room_id": "room", "run_id": "run", "generation": 1}
        snapshot["sessions"] = [{"session_id": "pane", "driver": "generic_terminal", "permission_profile": "normal", "working_dir": args.expected_work, "running": True, "run_id": "run", "generation": 1}]
        with self.assertRaisesRegex(RuntimeError, "membership changed"):
            walk.state()
        snapshot["rooms"][0]["member_ids"] = ["pane"]
        walk.state()
        snapshot["sessions"][0]["driver"] = "codex"
        with self.assertRaisesRegex(RuntimeError, "Generic Terminal refused"):
            walk.state()

    def test_output_script_size_bounds_and_echo_do_not_forge_end(self):
        self.assertEqual(fixture.LINES * fixture.LINE_BYTES, 2 * 1024 * 1024)
        script = fixture.output_script("123456789a", 9, 15000, True)
        self.assertNotIn("OL_123456789a_P9_", script)
        self.assertIn("$olN -le 4096", script)
        self.assertIn("PadRight(512,'x')", script)
        with self.assertRaisesRegex(RuntimeError, "delay exceeds"):
            fixture.output_script("123456789a", 1, 15001, True)
        with self.assertRaisesRegex(RuntimeError, "ambiguous path"):
            fixture.normalized("D:/scratch/../live")

    def test_observer_handles_split_markers_and_does_not_hide_gaps(self):
        bindings = {"pane": {"prefix": "OL_123456789a_P1_", "run_id": "run", "generation": 1}}
        script = """const assert=require('node:assert/strict');let listener;
          global.window={__TAURI__:{event:{listen:async(name,fn)=>{listener=fn;return()=>{}}}}};
          (async()=>{await OBSERVER;
          const prefix='OL_123456789a_P1_';let sequence=0;
          function emit(chunk,extra={}){listener({payload:{event:'session_output',identity:{session_id:'pane',run_id:'run',generation:1,sequence:++sequence},chunk,...extra}})}
          emit(ECHO);assert.equal(window.__outputLoadWalk.snapshot().panes.pane.end,false);
          for(let n=1;n<=4096;n++){
            const line=prefix+String(n).padStart(4,'0')+'|'+('x'.repeat(480))+'\\r\\n';
            emit(line.slice(0,10));emit(line.slice(10,22));emit(line.slice(22));
          }
          emit(prefix+'EN');emit('D\\r\\n');
          let s=window.__outputLoadWalk.snapshot().panes.pane;
          assert.equal(s.markers,4096);assert.equal(s.end,true);assert.equal(s.maxMarker,4096);
          assert.equal(s.gaps,0);assert.equal('carry' in s,false);assert.equal('seen' in s,false);
          emit('wrong-run',{identity:{session_id:'pane',run_id:'other',generation:1,sequence:90000}});
          emit('',{event:'ui_output_gap',dropped_events:12});
          s=window.__outputLoadWalk.snapshot().panes.pane;
          assert.equal(s.wrongRun,1);assert.equal(s.gaps,1);assert.equal(s.dropped,12);
          emit('other',{identity:{session_id:'stranger',run_id:'run',generation:1,sequence:1}});
          assert.equal(window.__outputLoadWalk.snapshot().unknownOutput,1);
          console.log('bounded observer checks passed');
          })().catch(e=>{console.error(e);process.exitCode=1});
        """.replace("OBSERVER", fixture.observer_script(bindings)).replace("ECHO", json.dumps(fixture.output_script("123456789a", 1, 0, True)))
        result = subprocess.run(["node", "-e", script], capture_output=True, text=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_stall_attempts_resume_even_when_pause_response_fails(self):
        with tempfile.TemporaryDirectory() as folder:
            args = SimpleNamespace(expected_work=folder, stall_mode="debugger")
            walk = fixture.Walk(args, Path(folder), {"steps": []}, io.StringIO())
            walk.create = lambda *_: walk.panes.update({"pane": {}})
            walk.metrics = self.metrics
            walk.input = lambda *_: None
            walk.ws_url = "unused-offline"
            calls = []

            def command(_client, method, **_kwargs):
                calls.append(method)
                if method == "Debugger.pause":
                    raise RuntimeError("pause acknowledgement lost")
                return True

            with patch.object(fixture, "CdpClient"), patch.object(fixture, "debugger_command", command):
                with self.assertRaisesRegex(RuntimeError, "pause acknowledgement lost"):
                    walk.stall()
            self.assertEqual(calls, ["Debugger.enable", "Debugger.pause", "Debugger.resume", "Debugger.disable"])
            self.assertEqual(walk.result["steps"][-1]["name"], "pause_resume")

    def test_busy_loop_timing_rejects_completion_outside_block_and_clock_jump(self):
        receipt = {"request_epoch_ms": 990, "reply_epoch_ms": 7010, "receipt_read_epoch_ms": 7005, "seconds": 6.02,
                   "browser": {"start_epoch_ms": 1000, "end_epoch_ms": 7000, "duration_ms": 6000},
                   "completion": {"mtime_epoch_ms": 3100, "observed_epoch_ms": 3150}}
        fixture.validate_busy_loop_timing(receipt)
        for completion in ({"mtime_epoch_ms": 900, "observed_epoch_ms": 3150},
                           {"mtime_epoch_ms": 3100, "observed_epoch_ms": 7010}, None):
            changed = copy.deepcopy(receipt)
            changed["completion"] = completion
            with self.assertRaisesRegex(RuntimeError, "native completion"):
                fixture.validate_busy_loop_timing(changed)
        changed = copy.deepcopy(receipt)
        changed["browser"]["end_epoch_ms"] += 500
        with self.assertRaisesRegex(RuntimeError, "wall clock changed"):
            fixture.validate_busy_loop_timing(changed)
        changed = copy.deepcopy(receipt)
        changed["receipt_read_epoch_ms"] = 6900
        with self.assertRaisesRegex(RuntimeError, "before ordinary task completed"):
            fixture.validate_busy_loop_timing(changed)

    def test_busy_loop_schedules_then_waits_before_one_receipt_read_without_debugger(self):
        sent, clock = [], [0.0]
        browser = {"start_epoch_ms": 1000, "end_epoch_ms": 7000, "duration_ms": 6000}
        replies = iter([{"id": 1, "result": {"result": {"value": {"scheduled_epoch_ms": 1000}}}},
                        {"id": 2, "result": {"result": {"value": browser}}}])
        socket = SimpleNamespace(settimeout=lambda _: None, send=lambda s: sent.append((clock[0], json.loads(s))),
                                 recv=lambda: json.dumps(next(replies)))
        marker = SimpleNamespace(exists=lambda: clock[0] >= 2,
                                 stat=lambda: SimpleNamespace(st_size=10, st_mtime_ns=3_000_000_000),
                                 read_text=lambda **_: "123456789a")
        receipt = {}
        with patch.object(fixture.time, "monotonic", side_effect=lambda: clock[0]), \
             patch.object(fixture.time, "time_ns", side_effect=lambda: int((1 + clock[0]) * 1_000_000_000)), \
             patch.object(fixture.time, "sleep", side_effect=lambda seconds: clock.__setitem__(0, clock[0] + seconds)):
            fixture.busy_loop_command(SimpleNamespace(_next_id=1, ws=socket), marker, "123456789a", receipt)
        self.assertEqual([s["method"] for _, s in sent], ["Runtime.evaluate", "Runtime.evaluate"])
        self.assertEqual(sent[0][1]["params"]["expression"], fixture.BUSY_LOOP_SCRIPT.replace("__TOKEN__", '"123456789a"'))
        self.assertGreaterEqual(sent[1][0], 6.2)
        self.assertNotIn("while(", sent[1][1]["params"]["expression"])
        self.assertTrue(receipt["native_completed_while_blocked"])
        self.assertGreaterEqual(receipt["completion"]["observed_epoch_ms"], 3000)

    def test_debugger_pause_requires_both_response_and_actual_paused_event(self):
        class Socket:
            def __init__(self, messages):
                self.messages = iter(messages)
            def settimeout(self, _):
                pass
            def send(self, _):
                pass
            def recv(self):
                try:
                    return json.dumps(next(self.messages))
                except StopIteration:
                    raise fixture.WebSocketTimeoutException()
        for messages in ([{"id": 1}, {"method": "Debugger.paused"}], [{"method": "Debugger.paused"}, {"id": 1}]):
            self.assertTrue(fixture.debugger_command(SimpleNamespace(_next_id=1, ws=Socket(messages)), "Debugger.pause"))
        with self.assertRaisesRegex(RuntimeError, "timed out"):
            fixture.debugger_command(SimpleNamespace(_next_id=1, ws=Socket([{"id": 1}])), "Debugger.pause")


if __name__ == "__main__":
    unittest.main()
