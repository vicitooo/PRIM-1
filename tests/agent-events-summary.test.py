import json
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "agent-events-summary.py"


def write_fixture(path: Path) -> None:
    events = [
        {
            "event": "pane_signal",
            "request_id": "req-signal-done",
            "session": "codex",
            "task_id": "smoke",
            "signal_type": "done",
            "summary": "finished",
            "artifact_paths": [],
            "commit_sha": None,
            "timestamp": "2026-05-17T20:00:00.123456700+00:00",
        },
        {
            "event": "pane_signal",
            "request_id": "req-signal-blocked",
            "session": "claude",
            "task_id": "smoke",
            "signal_type": "blocked",
            "summary": "needs operator",
            "artifact_paths": [],
            "commit_sha": None,
            "timestamp": "2026-05-17T20:00:01+00:00",
        },
        {
            "event": "session_work_state",
            "session": "codex",
            "state": "thinking",
            "detail": "Working 12s",
            "previous_state": "idle",
            "timestamp": "2026-05-17T20:00:01.500000+00:00",
        },
        {
            "event": "session_exit",
            "session": "codex",
            "generation": 4,
            "process_id": 1234,
            "exit_code": 1,
            "signal": None,
            "success": False,
            "reason": "crash_exit",
            "requested": False,
            "timestamp": "2026-05-17T20:00:01.700000+00:00",
        },
        {
            "event": "route_delivery",
            "request_id": "req-route-ok",
            "route_id": "route-ok-123456",
            "from": "supervisor",
            "logical_to": "room",
            "scope": "room",
            "recipient": None,
            "recipient_index": 0,
            "recipient_count": 2,
            "payload_part_count": 0,
            "phase": "resolved",
            "bytes_written": 0,
            "error": None,
            "timestamp": "2026-05-17T20:00:02+00:00",
        },
        {
            "event": "route_delivery",
            "request_id": "req-route-ok",
            "route_id": "route-ok-123456",
            "from": "supervisor",
            "logical_to": "room",
            "scope": "room",
            "recipient": "codex",
            "recipient_index": 0,
            "recipient_count": 2,
            "payload_part_count": 1,
            "phase": "written",
            "bytes_written": 40,
            "error": None,
            "timestamp": "2026-05-17T20:00:03+00:00",
        },
        {
            "event": "route_delivery",
            "request_id": "req-route-ok",
            "route_id": "route-ok-123456",
            "from": "supervisor",
            "logical_to": "room",
            "scope": "room",
            "recipient": "claude",
            "recipient_index": 1,
            "recipient_count": 2,
            "payload_part_count": 1,
            "phase": "written",
            "bytes_written": 41,
            "error": None,
            "timestamp": "2026-05-17T20:00:04+00:00",
        },
        {
            "event": "route_delivery",
            "request_id": "req-route-partial",
            "route_id": "route-partial-123456",
            "from": "supervisor",
            "logical_to": "room",
            "scope": "room",
            "recipient": None,
            "recipient_index": 0,
            "recipient_count": 3,
            "payload_part_count": 0,
            "phase": "resolved",
            "bytes_written": 0,
            "error": None,
            "timestamp": "2026-05-17T20:00:05+00:00",
        },
        {
            "event": "route_delivery",
            "request_id": "req-route-partial",
            "route_id": "route-partial-123456",
            "from": "supervisor",
            "logical_to": "room",
            "scope": "room",
            "recipient": "codex",
            "recipient_index": 0,
            "recipient_count": 3,
            "payload_part_count": 1,
            "phase": "written",
            "bytes_written": 40,
            "error": None,
            "timestamp": "2026-05-17T20:00:06+00:00",
        },
        {
            "event": "route_delivery",
            "request_id": "req-route-partial",
            "route_id": "route-partial-123456",
            "from": "supervisor",
            "logical_to": "room",
            "scope": "room",
            "recipient": "claude",
            "recipient_index": 1,
            "recipient_count": 3,
            "payload_part_count": 1,
            "phase": "failed",
            "bytes_written": 0,
            "error": "pipe closed",
            "timestamp": "2026-05-17T20:00:07+00:00",
        },
        {
            "event": "route_delivery",
            "request_id": "req-route-partial",
            "route_id": "route-partial-123456",
            "from": "supervisor",
            "logical_to": "room",
            "scope": "room",
            "recipient": "qa-codex",
            "recipient_index": 2,
            "recipient_count": 3,
            "payload_part_count": 1,
            "phase": "written",
            "bytes_written": 44,
            "error": None,
            "timestamp": "2026-05-17T20:00:08+00:00",
        },
        {
            "event": "request_ack",
            "request_id": "req-ack-123456",
            "session": "codex",
            "action": "send_input",
            "bytes_written": 19,
            "timestamp": "2026-05-17T20:00:09+00:00",
        },
        {
            "event": "request_ack_timeout",
            "request_id": "req-timeout-123456",
            "session": "codex",
            "action": "deliver_message",
            "elapsed_ms": 60000,
            "timestamp": "2026-05-17T20:00:10+00:00",
        },
        {
            "event": "sideband_request_lifecycle",
            "request_id": "req-failed-123456",
            "action": "send_input",
            "session": "no-such-pane",
            "phase": "failed",
            "error": "unknown session 'no-such-pane'",
            "elapsed_ms": 0,
            "timestamp": "2026-05-17T20:00:11+00:00",
        },
    ]
    path.write_text("\n".join(json.dumps(event) for event in events) + "\n", encoding="utf-8")


def run_summary(*args, input_text=None):
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args],
        input=input_text,
        text=True,
        capture_output=True,
        cwd=ROOT,
    )


def test_fixture_summary_and_fail_on():
    with tempfile.TemporaryDirectory() as tmp:
        fixture = Path(tmp) / "audit.jsonl"
        write_fixture(fixture)

        result = run_summary("--audit-log", str(fixture))
        assert result.returncode == 0, result.stderr
        out = result.stdout
        assert "pane_signal" in out
        assert "type=done" in out
        assert "type=blocked" in out
        assert "work_state" in out
        assert "idle->thinking" in out
        assert "session_exit" in out
        assert "reason=crash_exit" in out
        assert "code=1" in out
        assert "route_delivery" in out
        assert "2/2 written, 0 failed" in out
        assert "2/3 written, 1 failed: claude" in out
        assert "request_ack" in out
        assert "ack_timeout" in out
        assert "lifecycle_failed" in out

        failed = run_summary("--audit-log", str(fixture), "--fail-on", "blocked,failed,timeout")
        assert failed.returncode == 1


def test_filters_and_events_since_stdin():
    with tempfile.TemporaryDirectory() as tmp:
        fixture = Path(tmp) / "audit.jsonl"
        write_fixture(fixture)

        task_filtered = run_summary("--audit-log", str(fixture), "--task-id", "smoke")
        assert task_filtered.returncode == 0
        assert "pane_signal" in task_filtered.stdout
        assert "route_delivery" not in task_filtered.stdout

        request_filtered = run_summary("--audit-log", str(fixture), "--request-id", "req-ack-123456")
        assert request_filtered.returncode == 0
        assert "request_ack" in request_filtered.stdout
        assert "ack_timeout" not in request_filtered.stdout

        events = {"events": [json.loads(line) for line in fixture.read_text(encoding="utf-8").splitlines()]}
        stdin_result = run_summary("--events-since-stdin", input_text=json.dumps(events))
        assert stdin_result.returncode == 0
        assert "pane_signal" in stdin_result.stdout
        assert "session_exit" in stdin_result.stdout
        assert "route_delivery" in stdin_result.stdout


def test_error_loop_work_state_counts_as_blocked():
    events = {
        "events": [
            {
                "event": "session_work_state",
                "session": "codex",
                "state": "error_loop",
                "detail": "usage_limit",
                "previous_state": "blocked",
                "timestamp": "2026-05-17T20:00:01.500000+00:00",
            }
        ]
    }

    result = run_summary(
        "--events-since-stdin",
        "--fail-on",
        "blocked",
        input_text=json.dumps(events),
    )

    assert result.returncode == 1
    assert "blocked->error_loop" in result.stdout


if __name__ == "__main__":
    test_fixture_summary_and_fail_on()
    test_filters_and_events_since_stdin()
    test_error_loop_work_state_counts_as_blocked()
