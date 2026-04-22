"""Print last session-event timestamp per pane from audit log.

Useful before assuming a pane is stalled — compare last-activity time to
wall clock to decide whether to probe vs wait.

Usage:
    python supervisor-last-pane-activity.py [audit-path]

If audit-path is omitted, defaults to the most recent file in
CLI-master-wrapper/.runtime/audit/.

Output format (one line per pane):
    <pane>  last_event=<iso-timestamp>  age=<N>s  <hint>

Hints:
    fresh       — last event <60s ago (active right now)
    recent      — <10min
    quiet       — <45min (normal during BUILD thinking or test runs)
    long-quiet  — 45min-3h (flag for probe)
    stalled     — >3h (probe or restart)

Rule reference: memory/feedback_codex_stream_recovery_silent.md
"""
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path


def find_audit_path() -> Path:
    audit_dir = Path("./.runtime/audit")
    if not audit_dir.exists():
        print(f"audit dir not found: {audit_dir}", file=sys.stderr)
        sys.exit(1)
    files = sorted(audit_dir.glob("*.jsonl"), key=lambda p: p.stat().st_mtime, reverse=True)
    if not files:
        print(f"no audit files in {audit_dir}", file=sys.stderr)
        sys.exit(1)
    return files[0]


def tail_bytes(path: Path, n_bytes: int = 20 * 1024 * 1024) -> str:
    size = path.stat().st_size
    with path.open("rb") as f:
        f.seek(max(0, size - n_bytes))
        return f.read().decode("utf-8", errors="replace")


def classify(age_seconds: int) -> str:
    if age_seconds < 60:
        return "fresh"
    if age_seconds < 600:
        return "recent"
    if age_seconds < 2700:
        return "quiet"
    if age_seconds < 10800:
        return "long-quiet  [flag: consider probe]"
    return "stalled    [flag: probe or restart]"


def main() -> int:
    if len(sys.argv) > 1:
        audit_path = Path(sys.argv[1])
        if not audit_path.exists():
            print(f"audit file not found: {audit_path}", file=sys.stderr)
            return 1
    else:
        audit_path = find_audit_path()

    tail = tail_bytes(audit_path)
    last_per_pane: dict[str, str] = {}
    for line in tail.splitlines():
        if not line:
            continue
        try:
            e = json.loads(line)
        except json.JSONDecodeError:
            continue
        pane = e.get("session")
        ts = e.get("timestamp")
        if pane and ts:
            last_per_pane[pane] = ts

    now = datetime.now(timezone.utc)
    print(f"# audit file: {audit_path}")
    print(f"# wall clock (UTC): {now.isoformat(timespec='seconds')}")
    if not last_per_pane:
        print("# no session events in tail window")
        return 0
    for pane in sorted(last_per_pane):
        ts_raw = last_per_pane[pane]
        try:
            ts = datetime.fromisoformat(ts_raw.replace("Z", "+00:00"))
            age = int((now - ts).total_seconds())
            hint = classify(age)
            ts_short = ts_raw[:19].replace("T", " ") + "Z"
            print(f"{pane:10} last_event={ts_short}  age={age}s  {hint}")
        except (ValueError, TypeError):
            print(f"{pane:10} last_event={ts_raw}  age=?  parse-fail")
    return 0


if __name__ == "__main__":
    sys.exit(main())
