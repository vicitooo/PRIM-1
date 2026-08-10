import json
import os
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "agent-events-summary.py"


def write_fixture(path: Path) -> None:
    events = [
        {
            "event": "session_work_state",
            "session": "codex",
            "state": "thinking",
            "detail": None,
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
            "event": "dispatch_attempt",
            "request_id": "req-dispatch-idle",
            "action": "send_input",
            "from": "operator",
            "target_session": "claude",
            "target_lifecycle_state_before": "ready",
            "target_work_state_before": "idle",
            "target_last_activity_at": "2026-05-17T20:00:08+00:00",
            "last_route_from_target_at": None,
            "overlap": False,
            "reason": None,
            "timestamp": "2026-05-17T20:00:09.200000+00:00",
        },
        {
            "event": "dispatch_attempt",
            "request_id": "req-dispatch-overlap",
            "action": "send_input",
            "from": "operator",
            "target_session": "codex",
            "target_lifecycle_state_before": "ready",
            "target_work_state_before": "thinking",
            "target_last_activity_at": "2026-05-17T20:00:08+00:00",
            "last_route_from_target_at": None,
            "overlap": True,
            "reason": "target_thinking",
            "timestamp": "2026-05-17T20:00:09.500000+00:00",
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
            "event": "dispatch_no_reaction",
            "request_id": "req-no-reaction-123456",
            "session": "codex",
            "action": "deliver_message",
            "timestamp": "2026-05-17T20:00:10.100000+00:00",
        },
        {
            "event": "supervisor_heartbeat",
            "wrapper_pid": 4242,
            "uptime_secs": 1800,
            "sessions": [
                {
                    "name": "codex",
                    "lifecycle_state": "ready",
                    "work_state": "thinking",
                    "process_id": 1234,
                    "last_activity_at": "2026-05-17T20:00:09+00:00",
                },
                {
                    "name": "claude",
                    "lifecycle_state": "closed",
                    "work_state": None,
                    "process_id": None,
                    "last_activity_at": None,
                },
            ],
            "timestamp": "2026-05-17T20:00:10.200000+00:00",
        },
        {
            "event": "supervisor_alert",
            "alert_type": "ack_timeout",
            "request_id": "req-timeout-123456",
            "session": "codex",
            "action": "deliver_message",
            "last_work_state": "blocked",
            "last_session_state": "ready",
            "message": "Dispatch deliver_message to codex didn't ACK in 60s; last work_state=blocked",
            "severity": "warn",
            "timestamp": "2026-05-17T20:00:10.300000+00:00",
        },
        {
            "event": "supervisor_alert",
            "alert_type": "session_stall_detected",
            "request_id": None,
            "session": "codex",
            "action": "restart_session",
            "last_work_state": "error_loop",
            "last_session_state": "ready",
            "message": "Session codex stayed in error_loop for 600s; issuing auto-restart",
            "severity": "critical",
            "timestamp": "2026-05-17T20:00:10.400000+00:00",
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


def run_summary(*args, input_text=None, env=None):
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args],
        input=input_text,
        text=True,
        capture_output=True,
        cwd=ROOT,
        env=env,
    )


def test_fixture_summary_and_fail_on():
    with tempfile.TemporaryDirectory() as tmp:
        fixture = Path(tmp) / "audit.jsonl"
        write_fixture(fixture)

        result = run_summary("--audit-log", str(fixture))
        assert result.returncode == 0, result.stderr
        out = result.stdout
        assert "work_state" in out
        assert "idle->thinking" in out
        assert "session_exit" in out
        assert "reason=crash_exit" in out
        assert "code=1" in out
        assert "route_delivery" in out
        assert "2/2 written, 0 failed" in out
        assert "2/3 written, 1 failed: claude" in out
        assert "request_ack" in out
        assert "dispatch_overlap" in out
        assert "target=codex" in out
        assert "req-disp" in out
        assert "req-dispatch-idle" not in out
        assert "ack_timeout" in out
        assert "no_reaction" in out
        assert "heartbeat" in out
        assert "sessions=2 active=1" in out
        assert "supervisor_alert" in out
        assert "severity=critical" in out
        assert "lifecycle_failed" in out

        failed = run_summary("--audit-log", str(fixture), "--fail-on", "blocked,failed,timeout")
        assert failed.returncode == 1
        alert_failed = run_summary("--audit-log", str(fixture), "--fail-on", "alert")
        assert alert_failed.returncode == 1


def test_filters_and_events_since_stdin():
    with tempfile.TemporaryDirectory() as tmp:
        fixture = Path(tmp) / "audit.jsonl"
        write_fixture(fixture)

        request_filtered = run_summary("--audit-log", str(fixture), "--request-id", "req-ack-123456")
        assert request_filtered.returncode == 0
        assert "request_ack" in request_filtered.stdout
        assert "ack_timeout" not in request_filtered.stdout

        events = {"events": [json.loads(line) for line in fixture.read_text(encoding="utf-8").splitlines()]}
        stdin_result = run_summary("--events-since-stdin", input_text=json.dumps(events))
        assert stdin_result.returncode == 0
        assert "session_exit" in stdin_result.stdout
        assert "route_delivery" in stdin_result.stdout
        assert "supervisor_alert" in stdin_result.stdout


def test_error_loop_work_state_counts_as_blocked():
    events = {
        "events": [
            {
                "event": "session_work_state",
                "session": "codex",
                "state": "error_loop",
                "detail": None,
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


def test_default_runtime_override_with_spaces():
    with tempfile.TemporaryDirectory(prefix="prim1 runtime ") as tmp:
        audit_dir = Path(tmp) / "audit"
        audit_dir.mkdir(parents=True)
        fixture = audit_dir / f"{datetime.now(timezone.utc).date().isoformat()}.jsonl"
        write_fixture(fixture)
        env = os.environ.copy()
        env["PRIM1_RUNTIME_DIR"] = tmp
        env.pop("PRIM1_AUDIT_DIR", None)

        result = run_summary(env=env)

        assert result.returncode == 0, result.stderr
        assert "route_delivery" in result.stdout


if __name__ == "__main__":
    test_fixture_summary_and_fail_on()
    test_filters_and_events_since_stdin()
    test_error_loop_work_state_counts_as_blocked()
    test_default_runtime_override_with_spaces()
