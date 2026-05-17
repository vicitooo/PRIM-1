"""Detect OpenAI (or similar) stream-disconnect errors in pane audit stream.

Grep for stream-error signatures + check whether the affected pane has had
subsequent activity. Reports one of three states per pane:

    clean        — no error signatures in window
    recovered    — error signature + subsequent activity
    possibly-stuck — error signature + no subsequent activity within silence threshold

Usage:
    python supervisor-detect-stream-errors.py [--window-minutes N] [audit-path]

If audit-path is omitted, defaults to most recent *.jsonl in
PRIM1_AUDIT_DIR, or <repo-root>/.runtime/audit. Window defaults to 90 minutes.

Related supervisor script: supervisor-last-pane-activity.py
"""
import json
import os
import re
import sys
from datetime import datetime, timezone
from pathlib import Path


ERROR_PATTERNS = [
    r"stream disconnected before completion",
    r"An error occurred while processing your request",
    r"retry your request",
    r"network error",
    r"timed? out",
    r"ECONNRESET",
    r"socket hang up",
]

ERROR_RE = re.compile("|".join(ERROR_PATTERNS), re.IGNORECASE)


def find_audit_path() -> Path:
    audit_dir = Path(
        os.environ.get("PRIM1_AUDIT_DIR")
        or Path(__file__).resolve().parent.parent / ".runtime" / "audit"
    )
    files = sorted(audit_dir.glob("*.jsonl"), key=lambda p: p.stat().st_mtime, reverse=True)
    if not files:
        print(f"no audit files in {audit_dir}", file=sys.stderr)
        sys.exit(1)
    return files[0]


def tail_bytes(path: Path, n_bytes: int = 30 * 1024 * 1024) -> str:
    size = path.stat().st_size
    with path.open("rb") as f:
        f.seek(max(0, size - n_bytes))
        return f.read().decode("utf-8", errors="replace")


def parse_args():
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("--window-minutes", type=int, default=90,
                    help="How far back to look for stream errors (default 90min)")
    ap.add_argument("--silence-minutes", type=int, default=10,
                    help="If error found + pane silent for N min after, flag as possibly-stuck (default 10min)")
    ap.add_argument("audit", nargs="?", default=None, help="Path to audit log (default: most recent in runtime/audit/)")
    return ap.parse_args()


def main() -> int:
    args = parse_args()
    audit_path = Path(args.audit) if args.audit else find_audit_path()
    if not audit_path.exists():
        print(f"audit file not found: {audit_path}", file=sys.stderr)
        return 1

    tail = tail_bytes(audit_path)
    now = datetime.now(timezone.utc)
    window_seconds = args.window_minutes * 60

    # Per-pane: list of (timestamp, is_error) events
    per_pane: dict[str, list[tuple[datetime, bool]]] = {}
    for line in tail.splitlines():
        if not line:
            continue
        try:
            e = json.loads(line)
        except json.JSONDecodeError:
            continue
        pane = e.get("session")
        ts_raw = e.get("timestamp")
        if not (pane and ts_raw):
            continue
        try:
            ts = datetime.fromisoformat(ts_raw.replace("Z", "+00:00"))
        except (ValueError, TypeError):
            continue
        age = (now - ts).total_seconds()
        if age > window_seconds:
            continue
        chunk = e.get("chunk", "")
        is_error = bool(ERROR_RE.search(chunk))
        per_pane.setdefault(pane, []).append((ts, is_error))

    print(f"# audit file: {audit_path}")
    print(f"# wall clock (UTC): {now.isoformat(timespec='seconds')}")
    print(f"# window: last {args.window_minutes} min; silence threshold: {args.silence_minutes} min")
    if not per_pane:
        print("# no session events in window")
        return 0

    for pane in sorted(per_pane):
        events = per_pane[pane]
        errors = [(ts, ce) for ts, ce in events if ce]
        if not errors:
            print(f"{pane:10} clean")
            continue
        last_error_ts = max(ts for ts, _ in errors)
        activity_after_error = [ts for ts, _ in events if ts > last_error_ts]
        if activity_after_error:
            latest = max(activity_after_error)
            delta_since_error = int((latest - last_error_ts).total_seconds())
            age_of_last = int((now - latest).total_seconds())
            print(f"{pane:10} recovered   last_error={last_error_ts.strftime('%H:%M:%S')}Z  "
                  f"activity_after=+{delta_since_error}s  last_event_age={age_of_last}s")
        else:
            age_since_error = int((now - last_error_ts).total_seconds())
            if age_since_error > args.silence_minutes * 60:
                print(f"{pane:10} possibly-stuck  last_error={last_error_ts.strftime('%H:%M:%S')}Z  "
                      f"silent_for={age_since_error}s  [PROBE RECOMMENDED]")
            else:
                print(f"{pane:10} error-recent   last_error={last_error_ts.strftime('%H:%M:%S')}Z  "
                      f"silent_for={age_since_error}s  [wait, may recover]")
    return 0


if __name__ == "__main__":
    sys.exit(main())
