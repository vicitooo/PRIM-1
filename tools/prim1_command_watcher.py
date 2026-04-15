from __future__ import annotations

import argparse
import json
import os
import re
import signal
import subprocess
import sys
import time
import traceback
from dataclasses import dataclass, field
from datetime import date, datetime
from pathlib import Path
from typing import Any, Optional


OSC_RE = re.compile(r"\x1b\][^\x07]*(?:\x07|\x1b\\)")
CURSOR_RIGHT_RE = re.compile(r"\x1b\[(\d+)C")
CSI_RE = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]")
WHITESPACE_RE = re.compile(r"\s+")
ARMABLE_SLASH_COMMAND_RE = re.compile(r"^(?:[›❯↳>]+\s*)?/([A-Za-z0-9:_-]+)\b")


DEFAULT_CONFIG = {
    "poll_interval_seconds": 0.2,
    "arm_window_seconds": 3.0,
    "assistant_wait_seconds": 60.0,
    "min_dispatch_interval_seconds": 5.0,
    "daily_dispatch_limit": 50,
    "captured_text_limit_chars": 400,
    "commands": {
        "compact": {
            "completion_markers": ["Conversation compacted", "Compacted"],
            "continue_template": "Compaction complete. Continue with the task as previously instructed.",
        },
        "context": {
            "completion_markers": ["Context Usage"],
            "continue_template": "Your context usage: {captured_text}. Continue with the task.",
        },
        "help": {
            "completion_markers": ["For more help:"],
            "continue_template": "Command complete. Continue with the task.",
        },
        "fast": {
            "completion_markers": ["Fast mode set to"],
            "continue_template": "Command complete. Continue with the task.",
        },
        "model": {
            "completion_markers": ["to change"],
            "continue_template": "Command complete. Continue with the task.",
        },
        "permissions": {
            "completion_markers": ["Permissions", "bypass permissions"],
            "continue_template": "Command complete. Continue with the task.",
        },
        "skills": {
            "completion_markers": ["Use /skills", "Skills"],
            "continue_template": "Command complete. Continue with the task.",
        },
    },
    "assistant_turn_markers": {
        "claude": ["Hyperspacing", "thinking with high effort"],
        "codex": ["Working", "thinking"],
    },
}


def deep_merge(base: dict[str, Any], override: dict[str, Any]) -> dict[str, Any]:
    merged = dict(base)
    for key, value in override.items():
        if (
            key in merged
            and isinstance(merged[key], dict)
            and isinstance(value, dict)
        ):
            merged[key] = deep_merge(merged[key], value)
        else:
            merged[key] = value
    return merged


def strip_terminal_control_sequences(text: str) -> str:
    text = OSC_RE.sub("", text)

    def cursor_right_replacement(match: re.Match[str]) -> str:
        return " " * int(match.group(1))

    text = CURSOR_RIGHT_RE.sub(cursor_right_replacement, text)
    text = CSI_RE.sub("", text)
    return text.replace("\r", "")


def collapse_whitespace(text: str) -> str:
    return WHITESPACE_RE.sub(" ", text).strip()


def now_utc() -> datetime:
    return datetime.utcnow()


@dataclass
class CommandConfig:
    completion_markers: list[str]
    continue_template: str


@dataclass
class WatcherConfig:
    runtime_dir: Path
    audit_dir: Path
    control_plane_info: Path
    control_plane_script: Path
    watcher_log_path: Path
    watcher_alert_dir: Path
    config_path: Optional[Path]
    poll_interval_seconds: float
    arm_window_seconds: float
    assistant_wait_seconds: float
    min_dispatch_interval_seconds: float
    daily_dispatch_limit: int
    captured_text_limit_chars: int
    commands: dict[str, CommandConfig]
    assistant_turn_markers: dict[str, list[str]]


@dataclass
class DispatchAction:
    session: str
    command: str
    message: str


@dataclass
class AlertAction:
    session: str
    kind: str
    message: str
    details: dict[str, Any] = field(default_factory=dict)


@dataclass
class SessionState:
    mode: str = "idle"
    command: Optional[str] = None
    armed_at: Optional[datetime] = None
    last_input_forwarded_at: Optional[datetime] = None
    continue_sent_at: Optional[datetime] = None
    captured_chunks: list[str] = field(default_factory=list)
    dispatch_history: list[datetime] = field(default_factory=list)
    last_dispatch_at: Optional[datetime] = None

    def reset(self) -> None:
        self.mode = "idle"
        self.command = None
        self.armed_at = None
        self.continue_sent_at = None
        self.captured_chunks = []


class WatcherEngine:
    def __init__(self, config: WatcherConfig):
        self.config = config
        self.sessions: dict[str, SessionState] = {}

    def session_state(self, session: str) -> SessionState:
        return self.sessions.setdefault(session, SessionState())

    def observe_event(self, event: dict[str, Any], now: datetime) -> list[Any]:
        session = event.get("session")
        if not session:
            return []

        state = self.session_state(session)
        event_name = event.get("event")
        actions: list[Any] = []

        if event_name == "session_state":
            reason = (event.get("reason") or "").lower()
            if event.get("state") == "busy" and reason == "input forwarded":
                state.last_input_forwarded_at = now
            if event.get("state") in {"closed", "restarting"}:
                state.reset()
            return actions

        if event_name != "session_output":
            return actions

        sanitized = strip_terminal_control_sequences(event.get("chunk", ""))
        collapsed = collapse_whitespace(sanitized)
        if not collapsed:
            return actions

        if state.mode == "idle":
            command = self.detect_command(collapsed, state, now)
            if command:
                state.mode = "armed"
                state.command = command
                state.armed_at = now
                state.captured_chunks = [collapsed]
            return actions

        if state.mode == "armed":
            state.captured_chunks.append(collapsed)
            if self.is_completion_marker(collapsed, state.command):
                throttle_alert = self.enforce_dispatch_limits(session, state, now)
                if throttle_alert:
                    state.reset()
                    actions.append(throttle_alert)
                    return actions

                message = self.build_continue_message(state.command or "", state.captured_chunks)
                state.mode = "awaiting_assistant_turn"
                state.continue_sent_at = now
                actions.append(DispatchAction(session=session, command=state.command or "", message=message))
            return actions

        if state.mode == "awaiting_assistant_turn":
            if self.is_assistant_turn(session, collapsed):
                state.reset()
            return actions

        return actions

    def tick(self, now: datetime) -> list[AlertAction]:
        actions: list[AlertAction] = []
        for session, state in self.sessions.items():
            if state.mode != "awaiting_assistant_turn" or state.continue_sent_at is None:
                continue

            elapsed = (now - state.continue_sent_at).total_seconds()
            if elapsed >= self.config.assistant_wait_seconds:
                actions.append(
                    AlertAction(
                        session=session,
                        kind="assistant_turn_timeout",
                        message="No assistant turn observed after watcher continue dispatch.",
                        details={
                            "command": state.command,
                            "assistant_wait_seconds": self.config.assistant_wait_seconds,
                        },
                    )
                )
                state.reset()
        return actions

    def mark_dispatch_success(self, session: str, when: datetime) -> None:
        state = self.session_state(session)
        state.last_dispatch_at = when
        state.dispatch_history.append(when)

    def reset_session(self, session: str) -> None:
        self.session_state(session).reset()

    def detect_command(self, text: str, state: SessionState, now: datetime) -> Optional[str]:
        if state.last_input_forwarded_at is None:
            return None

        age = (now - state.last_input_forwarded_at).total_seconds()
        if age > self.config.arm_window_seconds:
            return None

        match = ARMABLE_SLASH_COMMAND_RE.match(text)
        if not match:
            return None

        command = match.group(1).lower()
        if command in self.config.commands:
            return command
        return None

    def is_completion_marker(self, text: str, command: Optional[str]) -> bool:
        if not command:
            return False

        config = self.config.commands.get(command)
        if not config:
            return False

        return any(marker.lower() in text.lower() for marker in config.completion_markers)

    def is_assistant_turn(self, session: str, text: str) -> bool:
        markers = self.config.assistant_turn_markers.get(session, [])
        lowered = text.lower()
        return any(marker.lower() in lowered for marker in markers)

    def build_continue_message(self, command: str, captured_chunks: list[str]) -> str:
        config = self.config.commands[command]
        captured_text = collapse_whitespace(" ".join(captured_chunks))
        captured_text = captured_text[: self.config.captured_text_limit_chars].strip()

        if "{captured_text}" in config.continue_template:
            return config.continue_template.format(captured_text=captured_text)
        return config.continue_template

    def enforce_dispatch_limits(
        self, session: str, state: SessionState, now: datetime
    ) -> Optional[AlertAction]:
        if state.last_dispatch_at is not None:
            since_last = (now - state.last_dispatch_at).total_seconds()
            if since_last < self.config.min_dispatch_interval_seconds:
                return AlertAction(
                    session=session,
                    kind="dispatch_rate_limit",
                    message="Watcher dispatch suppressed by the per-session rate limit.",
                    details={
                        "seconds_since_last_dispatch": since_last,
                        "minimum_seconds": self.config.min_dispatch_interval_seconds,
                    },
                )

        today = now.date()
        state.dispatch_history = [
            entry for entry in state.dispatch_history if entry.date() == today
        ]
        if len(state.dispatch_history) >= self.config.daily_dispatch_limit:
            return AlertAction(
                session=session,
                kind="daily_dispatch_limit",
                message="Watcher dispatch suppressed by the daily per-session throttle.",
                details={
                    "daily_dispatch_limit": self.config.daily_dispatch_limit,
                    "dispatches_today": len(state.dispatch_history),
                },
            )
        return None


class AuditTailer:
    def __init__(self, audit_dir: Path):
        self.audit_dir = audit_dir
        self._current_date: Optional[date] = None
        self._handle = None
        self._path: Optional[Path] = None

    def current_path(self, now: datetime) -> Path:
        return self.audit_dir / f"{now.date().isoformat()}.jsonl"

    def ensure_open(self, now: datetime) -> None:
        path = self.current_path(now)
        if self._path == path and self._handle is not None:
            return

        if self._handle is not None:
            self._handle.close()
            self._handle = None

        self._path = path
        self._current_date = now.date()
        if not path.exists():
            return

        self._handle = path.open("r", encoding="utf-8", errors="replace")
        self._handle.seek(0, os.SEEK_END)

    def read_events(self, now: datetime) -> list[dict[str, Any]]:
        self.ensure_open(now)
        if self._handle is None:
            return []

        events: list[dict[str, Any]] = []
        while True:
            line = self._handle.readline()
            if not line:
                break
            line = line.strip()
            if not line:
                continue
            try:
                events.append(json.loads(line))
            except json.JSONDecodeError:
                continue
        return events


class PowerShellControlPlaneClient:
    def __init__(self, script_path: Path, info_path: Path):
        self.script_path = script_path
        self.info_path = info_path

    def send_continue(self, session: str, message: str) -> None:
        self._run(
            [
                "-Action",
                "input",
                "-Session",
                session,
                "-Content",
                message,
                "-InfoFile",
                str(self.info_path),
                "-Quiet",
            ]
        )
        self._run(
            [
                "-Action",
                "key",
                "-Session",
                session,
                "-Key",
                "enter",
                "-InfoFile",
                str(self.info_path),
                "-Quiet",
            ]
        )

    def _run(self, arguments: list[str]) -> None:
        process = subprocess.run(
            [
                "powershell",
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                str(self.script_path),
                *arguments,
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        if process.returncode != 0:
            details = (process.stderr or process.stdout or "").strip()
            raise RuntimeError(details or f"control-plane call failed with exit code {process.returncode}")


class WatcherRuntime:
    def __init__(
        self,
        config: WatcherConfig,
        engine: WatcherEngine,
        control_plane: PowerShellControlPlaneClient,
    ):
        self.config = config
        self.engine = engine
        self.control_plane = control_plane
        self.tailer = AuditTailer(config.audit_dir)
        self._should_stop = False

    def stop(self, *_args: Any) -> None:
        self._should_stop = True

    def run(self) -> int:
        self.config.watcher_alert_dir.mkdir(parents=True, exist_ok=True)
        self.config.watcher_log_path.parent.mkdir(parents=True, exist_ok=True)
        self.log("watcher_start", "info", "Watcher started.")

        signal.signal(signal.SIGINT, self.stop)
        signal.signal(signal.SIGTERM, self.stop)

        try:
            while not self._should_stop:
                now = now_utc()
                for event in self.tailer.read_events(now):
                    actions = self.engine.observe_event(event, now)
                    self.handle_actions(actions, now)

                self.handle_actions(self.engine.tick(now), now)
                time.sleep(self.config.poll_interval_seconds)
        except Exception as exc:  # pragma: no cover - handled by live runtime
            self.log(
                "watcher_crash",
                "error",
                f"Watcher crashed: {exc}",
                traceback=traceback.format_exc(),
            )
            return 1

        self.log("watcher_stop", "info", "Watcher stopped cleanly.")
        return 0

    def handle_actions(self, actions: list[Any], now: datetime) -> None:
        for action in actions:
            if isinstance(action, DispatchAction):
                self.dispatch_continue(action, now)
            elif isinstance(action, AlertAction):
                self.write_alert(action, now)
                self.log(
                    "watcher_alert",
                    "warning",
                    action.message,
                    session=action.session,
                    kind=action.kind,
                    details=action.details,
                )

    def dispatch_continue(self, action: DispatchAction, now: datetime) -> None:
        try:
            self.control_plane.send_continue(action.session, action.message)
            self.engine.mark_dispatch_success(action.session, now)
            self.log(
                "continue_dispatched",
                "info",
                f"Continue signal dispatched for /{action.command}.",
                session=action.session,
                command=action.command,
                continue_message=action.message,
            )
        except Exception as exc:
            self.engine.reset_session(action.session)
            alert = AlertAction(
                session=action.session,
                kind="dispatch_failed",
                message=f"Failed to dispatch continue signal: {exc}",
                details={
                    "command": action.command,
                    "continue_message": action.message,
                },
            )
            self.write_alert(alert, now)
            self.log(
                "watcher_alert",
                "error",
                alert.message,
                session=alert.session,
                kind=alert.kind,
                details=alert.details,
            )

    def log(self, event: str, level: str, message: str, **fields: Any) -> None:
        record = {
            "timestamp": now_utc().isoformat(timespec="seconds") + "Z",
            "event": event,
            "level": level,
            "message": message,
        }
        record.update(fields)
        with self.config.watcher_log_path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(record, ensure_ascii=False) + "\n")

    def write_alert(self, alert: AlertAction, now: datetime) -> None:
        timestamp = now.strftime("%Y%m%dT%H%M%SZ")
        alert_path = self.config.watcher_alert_dir / f"{timestamp}-{alert.session}-{alert.kind}.json"
        payload = {
            "timestamp": now.isoformat(timespec="seconds") + "Z",
            "session": alert.session,
            "kind": alert.kind,
            "message": alert.message,
            "details": alert.details,
        }
        alert_path.write_text(json.dumps(payload, indent=2), encoding="utf-8")


def load_config(script_path: Path, runtime_dir: Optional[Path], config_path: Optional[Path], control_plane_info: Optional[Path]) -> WatcherConfig:
    repo_root = script_path.parent.parent
    runtime_root = runtime_dir or (repo_root / ".runtime")
    config_file = config_path or (script_path.parent / "prim1-command-watcher.config.json")
    merged = DEFAULT_CONFIG
    if config_file.exists():
        merged = deep_merge(DEFAULT_CONFIG, json.loads(config_file.read_text(encoding="utf-8")))

    commands = {
        name: CommandConfig(
            completion_markers=list(value["completion_markers"]),
            continue_template=value["continue_template"],
        )
        for name, value in merged["commands"].items()
    }

    return WatcherConfig(
        runtime_dir=runtime_root,
        audit_dir=runtime_root / "audit",
        control_plane_info=control_plane_info or (runtime_root / "control-plane.json"),
        control_plane_script=repo_root / "scripts" / "control-plane.ps1",
        watcher_log_path=runtime_root / "watcher.log",
        watcher_alert_dir=runtime_root / "watcher-alerts",
        config_path=config_file if config_file.exists() else None,
        poll_interval_seconds=float(merged["poll_interval_seconds"]),
        arm_window_seconds=float(merged["arm_window_seconds"]),
        assistant_wait_seconds=float(merged["assistant_wait_seconds"]),
        min_dispatch_interval_seconds=float(merged["min_dispatch_interval_seconds"]),
        daily_dispatch_limit=int(merged["daily_dispatch_limit"]),
        captured_text_limit_chars=int(merged["captured_text_limit_chars"]),
        commands=commands,
        assistant_turn_markers={
            session: list(markers)
            for session, markers in merged["assistant_turn_markers"].items()
        },
    )


def build_argument_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="PRIM-001 slash-command watcher")
    parser.add_argument("--runtime-dir", type=Path, default=None)
    parser.add_argument("--config", type=Path, default=None)
    parser.add_argument("--control-plane-info", type=Path, default=None)
    return parser


def main(argv: Optional[list[str]] = None) -> int:
    parser = build_argument_parser()
    args = parser.parse_args(argv)
    script_path = Path(__file__).resolve()
    config = load_config(script_path, args.runtime_dir, args.config, args.control_plane_info)
    engine = WatcherEngine(config)
    control_plane = PowerShellControlPlaneClient(
        script_path=config.control_plane_script,
        info_path=config.control_plane_info,
    )
    runtime = WatcherRuntime(config, engine, control_plane)
    return runtime.run()


if __name__ == "__main__":  # pragma: no cover - CLI entrypoint
    raise SystemExit(main())
