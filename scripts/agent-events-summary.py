"""Summarize PRIM-1 durable metadata events for operators.

This reads the local JSONL audit directly and groups lifecycle, health, and
delivery receipts into one-line summaries. The durable audit intentionally
contains no terminal or routed-message content.
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import time
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Iterable


SUMMARY_EVENTS = {
    "session_exit",
    "session_work_state",
    "supervisor_heartbeat",
    "supervisor_alert",
    "route_delivery",
    "dispatch_attempt",
}

FAIL_ON_VALUES = {"alert", "blocked", "failed", "timeout"}


@dataclass
class SummaryLine:
    timestamp: str
    sequence: int
    line: str
    conditions: set[str] = field(default_factory=set)


@dataclass
class RouteSummary:
    route_id: str
    request_id: str = ""
    from_: str = ""
    logical_to: str = ""
    scope: str = ""
    recipient_count: int = 0
    timestamp: str = ""
    written: set[str] = field(default_factory=set)
    failed: dict[str, str] = field(default_factory=dict)
    bytes_written: int = 0
    last_sequence: int = 0

    def update(self, event: dict, sequence: int) -> None:
        self.request_id = event.get("request_id") or self.request_id
        self.from_ = event.get("from") or self.from_
        self.logical_to = event.get("logical_to") or self.logical_to
        self.scope = event.get("scope") or self.scope
        self.recipient_count = int(event.get("recipient_count") or self.recipient_count or 0)
        self.timestamp = event.get("timestamp") or self.timestamp
        self.last_sequence = sequence

        phase = event.get("phase")
        recipient = event.get("recipient")
        recipient_name = str(recipient) if recipient else "<none>"
        if phase == "written":
            self.written.add(recipient_name)
            self.bytes_written += int(event.get("bytes_written") or 0)
        elif phase == "failed":
            self.failed[recipient_name] = str(event.get("error") or "unknown error")

    def is_complete(self) -> bool:
        if self.recipient_count == 0:
            return True
        return len(self.written) + len(self.failed) >= self.recipient_count

    def to_line(self) -> SummaryLine:
        total = self.recipient_count
        written_count = len(self.written)
        failed_count = len(self.failed)
        route = short_id(self.route_id)
        target = self.logical_to or "?"
        failures = ""
        conditions: set[str] = set()
        if failed_count:
            conditions.add("failed")
            failed_names = ", ".join(sorted(self.failed))
            failures = f": {failed_names}"
        line = (
            f"{format_time(self.timestamp)} route_delivery   route={route}  "
            f"from={self.from_ or '?'} -> {target} "
            f"({written_count}/{total} written, {failed_count} failed{failures}) "
            f"bytes={self.bytes_written}"
        )
        return SummaryLine(self.timestamp, self.last_sequence, line, conditions)


def default_runtime_dir() -> Path:
    override = os.environ.get("PRIM1_RUNTIME_DIR")
    if override:
        return Path(override).expanduser()
    if os.name == "nt":
        local_app_data = os.environ.get("LOCALAPPDATA")
        if not local_app_data:
            raise ValueError("LOCALAPPDATA is required when PRIM1_RUNTIME_DIR is unset")
        return Path(local_app_data) / "io.prim1.runtime" / "runtime"
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Application Support" / "io.prim1.runtime" / "runtime"
    data_home = Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local" / "share")
    return data_home / "io.prim1.runtime" / "runtime"


def default_audit_path() -> Path:
    audit_dir = Path(os.environ.get("PRIM1_AUDIT_DIR") or default_runtime_dir() / "audit")
    today = audit_dir / f"{datetime.now(timezone.utc).date().isoformat()}.jsonl"
    if today.exists():
        return today
    files = sorted(audit_dir.glob("*.jsonl"), key=lambda p: p.stat().st_mtime, reverse=True)
    if files:
        return files[0]
    return today


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    ap = argparse.ArgumentParser(description="Summarize PRIM-1 durable metadata events.")
    ap.add_argument("--audit-log", help="Read this JSONL audit log instead of the runtime default.")
    ap.add_argument("--request-id", help="Restrict to events with this request_id.")
    ap.add_argument("--since-minutes", type=float, help="Only include events newer than N minutes.")
    ap.add_argument("--watch", action="store_true", help="Poll the audit log every 2s and emit new summaries.")
    ap.add_argument(
        "--fail-on",
        default="",
        help="Comma-list of conditions that should return nonzero: alert,blocked,failed,timeout.",
    )
    return ap.parse_args(argv)


def parse_fail_on(raw: str) -> set[str]:
    values = {part.strip().lower() for part in raw.split(",") if part.strip()}
    unknown = values - FAIL_ON_VALUES
    if unknown:
        raise ValueError(f"unknown --fail-on value(s): {', '.join(sorted(unknown))}")
    return values


def parse_timestamp(raw: str | None) -> datetime | None:
    if not raw:
        return None
    value = raw.replace("Z", "+00:00")
    if "." in value:
        prefix, suffix = value.split(".", 1)
        tz_start = max(suffix.find("+"), suffix.find("-"))
        if tz_start >= 0:
            frac = suffix[:tz_start]
            tz = suffix[tz_start:]
        else:
            frac = suffix
            tz = ""
        value = f"{prefix}.{frac[:6]}{tz}"
    try:
        parsed = datetime.fromisoformat(value)
    except ValueError:
        return None
    if parsed.tzinfo is None:
        return parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def format_time(raw: str | None) -> str:
    parsed = parse_timestamp(raw)
    if parsed:
        return f"[{parsed:%H:%M:%S}]"
    return "[??:??:??]"


def short_id(value: str | None) -> str:
    return str(value or "?")[:8]


def quote_summary(value: str | None) -> str:
    text = str(value or "")
    text = text.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{text}"'


def iter_jsonl_events(path: Path) -> Iterable[dict]:
    with path.open("r", encoding="utf-8", errors="replace") as handle:
        for line in handle:
            if not line.strip():
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(event, dict):
                yield event


def event_matches(event: dict, args: argparse.Namespace, since_cutoff: datetime | None) -> bool:
    kind = event.get("event")
    if kind not in SUMMARY_EVENTS:
        return False
    if args.request_id and event.get("request_id") != args.request_id:
        return False
    if since_cutoff:
        timestamp = parse_timestamp(event.get("timestamp"))
        if not timestamp or timestamp < since_cutoff:
            return False
    return True


def event_to_line(event: dict, sequence: int) -> SummaryLine | None:
    kind = event.get("event")
    timestamp = event.get("timestamp")
    if kind == "session_work_state":
        state = str(event.get("state") or "?")
        conditions = {"blocked"} if state in {"blocked", "error_loop"} else set()
        detail = f"  detail={quote_summary(event.get('detail'))}" if event.get("detail") else ""
        previous = event.get("previous_state") or "?"
        line = (
            f"{format_time(timestamp)} work_state       session={event.get('session') or '?'}  "
            f"{previous}->{state}{detail}"
        )
        return SummaryLine(timestamp, sequence, line, conditions)

    if kind == "session_exit":
        reason = str(event.get("reason") or "?")
        conditions = {"failed"} if reason in {"crash_exit", "pty_error", "process_disappeared"} else set()
        line = (
            f"{format_time(timestamp)} session_exit    session={event.get('session') or '?'}  "
            f"reason={reason}  code={event.get('exit_code')}  "
            f"signal={event.get('signal')}  requested={str(bool(event.get('requested'))).lower()}"
        )
        return SummaryLine(timestamp, sequence, line, conditions)

    if kind == "supervisor_heartbeat":
        sessions = event.get("sessions") if isinstance(event.get("sessions"), list) else []
        active = [
            item for item in sessions
            if isinstance(item, dict) and item.get("lifecycle_state") not in {None, "closed"}
        ]
        line = (
            f"{format_time(timestamp)} heartbeat       pid={event.get('wrapper_pid') or '?'}  "
            f"uptime={event.get('uptime_secs') or 0}s  "
            f"sessions={len(sessions)} active={len(active)}"
        )
        return SummaryLine(timestamp, sequence, line)

    if kind == "supervisor_alert":
        severity = str(event.get("severity") or "?")
        alert_type = str(event.get("alert_type") or "?")
        conditions = {"alert"} if severity == "critical" else set()
        line = (
            f"{format_time(timestamp)} supervisor_alert severity={severity}  "
            f"type={alert_type}  session={event.get('session') or '?'}  "
            f"action={event.get('action') or '?'}  message={quote_summary(event.get('message'))}"
        )
        return SummaryLine(timestamp, sequence, line, conditions)

    if kind == "dispatch_attempt":
        if not event.get("overlap"):
            return None
        line = (
            f"{format_time(timestamp)} dispatch_overlap req={short_id(event.get('request_id'))}  "
            f"from={event.get('from') or '?'}  target={event.get('target_session') or '?'}  "
            f"action={event.get('action') or '?'}  reason={event.get('reason') or 'overlap'}"
        )
        return SummaryLine(timestamp, sequence, line)

    return None


def summarize_events(events: Iterable[dict], args: argparse.Namespace) -> tuple[list[SummaryLine], set[str]]:
    since_cutoff = None
    if args.since_minutes is not None:
        since_cutoff = datetime.now(timezone.utc) - timedelta(minutes=args.since_minutes)

    lines: list[SummaryLine] = []
    routes: dict[str, RouteSummary] = {}
    conditions: set[str] = set()

    for sequence, event in enumerate(events):
        if not event_matches(event, args, since_cutoff):
            continue

        if event.get("event") == "route_delivery":
            route_id = str(event.get("route_id") or "")
            if not route_id:
                continue
            summary = routes.setdefault(route_id, RouteSummary(route_id=route_id))
            summary.update(event, sequence)
            if event.get("phase") == "failed":
                conditions.add("failed")
            continue

        line = event_to_line(event, sequence)
        if line:
            lines.append(line)
            conditions.update(line.conditions)

    for route in routes.values():
        route_line = route.to_line()
        lines.append(route_line)
        conditions.update(route_line.conditions)

    lines.sort(key=lambda item: (parse_timestamp(item.timestamp) or datetime.min.replace(tzinfo=timezone.utc), item.sequence))
    return lines, conditions


def print_lines(lines: Iterable[SummaryLine]) -> None:
    for line in lines:
        print(line.line, flush=True)


def watch_audit(path: Path, args: argparse.Namespace, fail_on: set[str]) -> int:
    if not path.exists():
        print(f"audit log not found: {path}", file=sys.stderr)
        return 1

    position = path.stat().st_size
    routes: dict[str, RouteSummary] = {}
    emitted_routes: set[str] = set()
    sequence = 0

    while True:
        if not path.exists():
            time.sleep(2)
            continue
        with path.open("rb") as handle:
            size = path.stat().st_size
            if size < position:
                position = 0
            handle.seek(position)
            raw_lines = handle.readlines()
            position = handle.tell()

        batch_conditions: set[str] = set()
        since_cutoff = None
        if args.since_minutes is not None:
            since_cutoff = datetime.now(timezone.utc) - timedelta(minutes=args.since_minutes)

        for raw_line in raw_lines:
            sequence += 1
            try:
                event = json.loads(raw_line.decode("utf-8", errors="replace"))
            except json.JSONDecodeError:
                continue
            if not isinstance(event, dict) or not event_matches(event, args, since_cutoff):
                continue
            if event.get("event") == "route_delivery":
                route_id = str(event.get("route_id") or "")
                if not route_id:
                    continue
                summary = routes.setdefault(route_id, RouteSummary(route_id=route_id))
                summary.update(event, sequence)
                if event.get("phase") == "failed":
                    batch_conditions.add("failed")
                if route_id not in emitted_routes and summary.is_complete():
                    line = summary.to_line()
                    print(line.line, flush=True)
                    batch_conditions.update(line.conditions)
                    emitted_routes.add(route_id)
                continue

            line = event_to_line(event, sequence)
            if line:
                print(line.line, flush=True)
                batch_conditions.update(line.conditions)

        if fail_on & batch_conditions:
            return 1
        time.sleep(2)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        fail_on = parse_fail_on(args.fail_on)
    except ValueError as exc:
        print(str(exc), file=sys.stderr)
        return 2

    try:
        audit_path = Path(args.audit_log) if args.audit_log else default_audit_path()
    except ValueError as exc:
        print(str(exc), file=sys.stderr)
        return 2
    if args.watch:
        return watch_audit(audit_path, args, fail_on)
    if not audit_path.exists():
        print(f"audit log not found: {audit_path}", file=sys.stderr)
        return 1
    events = list(iter_jsonl_events(audit_path))

    lines, conditions = summarize_events(events, args)
    print_lines(lines)
    return 1 if fail_on & conditions else 0


if __name__ == "__main__":
    sys.exit(main())
