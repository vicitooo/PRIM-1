"""List routed_message events from PRIM-1 audit log.

Useful for catching up on room state mid-campaign, verifying protocol
adherence, and counting events by tag prefix.

Usage:
    python supervisor-routed-events.py [--tag PREFIX] [--last N] [--from FROM] [--to TO] [audit-path]

Examples:
    # Last 10 room events
    python supervisor-routed-events.py --last 10

    # All events with tag prefix CRM-python supervisor-routed-events.py --tag CRM-# Count + category breakdown for a campaign
    python supervisor-routed-events.py --tag CRM--summary

    # All events where codex routed to room
    python supervisor-routed-events.py --from codex --to room --last 20

Rule reference: memory/feedback_prim1_room_protocol_discipline.md
"""
import argparse
import json
import re
import sys
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path


def find_audit_path() -> Path:
    audit_dir = Path("./.runtime/audit")
    files = sorted(audit_dir.glob("*.jsonl"), key=lambda p: p.stat().st_mtime, reverse=True)
    if not files:
        print(f"no audit files in {audit_dir}", file=sys.stderr)
        sys.exit(1)
    return files[0]


def iter_lines(path: Path):
    """Stream lines from audit file. For multi-hundred-MB audit files,
    streaming beats tail-slicing because routed_message events can span
    hours of campaign activity that wouldn't fit in a 50MB tail."""
    with path.open("r", encoding="utf-8", errors="replace") as f:
        for line in f:
            yield line


def parse_args():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tag", help="Filter to events whose content starts with this tag prefix (e.g. CRM-)")
    ap.add_argument("--from", dest="from_", help="Filter by from pane (claude / codex / outside-claude)")
    ap.add_argument("--to", help="Filter by to pane (claude / codex / room)")
    ap.add_argument("--last", type=int, default=None, help="Show only the last N matching events")
    ap.add_argument("--summary", action="store_true", help="Print category counts instead of full events")
    ap.add_argument("audit", nargs="?", default=None, help="Path to audit log (default: most recent)")
    return ap.parse_args()


EVENT_CATEGORY_PATTERNS = [
    (r"\bPLAN_V\d+_READY\b", "plan-ready (meta)"),
    (r"\bPLAN_[A-Z]+_V\d+_READY\b", "plan-ready (sub-scope)"),
    (r"\bREVIEW_V\d+_READY\b", "review-ready (meta)"),
    (r"\bREVIEW_[A-Z]+_V\d+_READY\b", "review-ready (sub-scope)"),
    (r"\bREVIEW_.*_SUBAGENT_READY\b", "subagent-review-ready"),
    (r"\bREVIEW_.*_PASS\b", "review-pass"),
    (r"\bBUILD_START\b", "build-start"),
    (r"\bBUILD_.*_START\b", "build-start"),
    (r"\bBUILD_.*_STATUS\b", "build-status"),
    (r"\bBUILD_.*_READY\b", "build-ready"),
    (r"\bBUILD_.*_BLOCKED\b", "build-blocked"),
    (r"\bBUILD_.*_PATCH", "build-patch"),
    (r"\bTAG_CORRECTION_NEEDED\b", "tag-correction"),
    (r"\bTEMPLATE_COMPLETE\b", "template-complete"),
    (r"\bACK\b|_ACK_PROCEED\b", "ack"),
    (r"\bBLOCKED\b", "blocked"),
    (r"\bSTATUS\b", "status"),
]


def categorize(content: str) -> str:
    for pattern, label in EVENT_CATEGORY_PATTERNS:
        if re.search(pattern, content):
            return label
    return "other"


def main() -> int:
    args = parse_args()
    audit_path = Path(args.audit) if args.audit else find_audit_path()
    if not audit_path.exists():
        print(f"audit file not found: {audit_path}", file=sys.stderr)
        return 1

    matches = []
    for line in iter_lines(audit_path):
        if not line.strip():
            continue
        try:
            e = json.loads(line)
        except json.JSONDecodeError:
            continue
        if e.get("event") != "routed_message":
            continue
        content = e.get("content", "")
        if args.tag and not content.startswith(args.tag):
            continue
        if args.from_ and e.get("from") != args.from_:
            continue
        if args.to and e.get("to") != args.to:
            continue
        matches.append(e)

    if args.last:
        matches = matches[-args.last:]

    print(f"# audit file: {audit_path}")
    filters = []
    if args.tag:
        filters.append(f"tag={args.tag}")
    if args.from_:
        filters.append(f"from={args.from_}")
    if args.to:
        filters.append(f"to={args.to}")
    print(f"# filters: {' '.join(filters) if filters else 'none'}")
    print(f"# matches: {len(matches)}")

    if not matches:
        return 0

    if args.summary:
        cats = Counter(categorize(e.get("content", "")) for e in matches)
        print()
        for cat, n in sorted(cats.items(), key=lambda x: -x[1]):
            print(f"  {n:4}  {cat}")
        return 0

    print()
    for e in matches:
        ts = e.get("timestamp", "")[:19].replace("T", " ")
        print(f"{ts}Z  {e.get('from',''):18} → {e.get('to',''):10}  {e.get('content','')[:140]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
