"""Print a single heartbeat line summarizing wrapper + pane state.

Designed for use in a Monitor loop. Reads health-check JSON from stdin.

Usage:
    powershell -File health-check.ps1 -Json | python supervisor-heartbeat.py
"""
import json
import sys
from datetime import datetime, timezone


def main() -> int:
    raw = sys.stdin.read()
    if not raw.strip():
        print(f"HB {datetime.now(timezone.utc).isoformat(timespec='seconds').replace('+00:00','Z')} health-check-empty")
        return 0
    try:
        data = json.loads(raw)
    except json.JSONDecodeError as exc:
        print(f"HB {datetime.now(timezone.utc).isoformat(timespec='seconds').replace('+00:00','Z')} json-parse-fail: {exc}")
        return 0

    ts = data.get("timestamp", "")[:19].replace("T", " ") + "Z"
    overall = data.get("overall", "?")
    panes = {p["name"]: p for p in data.get("panes", [])}
    claude = panes.get("claude", {})
    codex = panes.get("codex", {})
    claude_str = f"{claude.get('lifecycle_state','?')}({claude.get('last_activity_age_seconds','?')}s)"
    codex_str = f"{codex.get('lifecycle_state','?')}({codex.get('last_activity_age_seconds','?')}s)"
    failures = data.get("failures", [])
    warnings = data.get("warnings", [])
    f_count = len(failures)
    w_count = len(warnings)
    print(f"HB {ts} overall={overall} claude={claude_str} codex={codex_str} failures={f_count} warnings={w_count}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
