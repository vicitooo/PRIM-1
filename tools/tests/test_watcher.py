from __future__ import annotations

import sys
import unittest
from datetime import datetime, timedelta
from pathlib import Path


TOOLS_DIR = Path(__file__).resolve().parents[1]
if str(TOOLS_DIR) not in sys.path:
    sys.path.insert(0, str(TOOLS_DIR))

from prim1_command_watcher import (  # noqa: E402
    AlertAction,
    DispatchAction,
    WatcherConfig,
    WatcherEngine,
    load_config,
)


def build_config() -> WatcherConfig:
    runtime_dir = TOOLS_DIR.parent / ".runtime-test"
    return load_config(
        script_path=TOOLS_DIR / "prim1_command_watcher.py",
        runtime_dir=runtime_dir,
        config_path=None,
        control_plane_info=runtime_dir / "control-plane.json",
    )


class WatcherEngineTests(unittest.TestCase):
    def setUp(self) -> None:
        self.config = build_config()
        self.engine = WatcherEngine(self.config)
        self.base = datetime(2026, 4, 15, 10, 0, 0)

    def test_arms_on_recent_slash_echo(self) -> None:
        self.engine.observe_event(
            {"event": "session_state", "session": "claude", "state": "busy", "reason": "input forwarded"},
            self.base,
        )
        actions = self.engine.observe_event(
            {"event": "session_output", "session": "claude", "chunk": "/help"},
            self.base + timedelta(seconds=1),
        )
        self.assertEqual(actions, [])
        state = self.engine.session_state("claude")
        self.assertEqual(state.mode, "armed")
        self.assertEqual(state.command, "help")

    def test_completion_detection_dispatches_continue(self) -> None:
        self.engine.observe_event(
            {"event": "session_state", "session": "claude", "state": "busy", "reason": "input forwarded"},
            self.base,
        )
        self.engine.observe_event(
            {"event": "session_output", "session": "claude", "chunk": "/help"},
            self.base + timedelta(seconds=1),
        )
        actions = self.engine.observe_event(
            {"event": "session_output", "session": "claude", "chunk": "For more help: https://code.claude.com/docs"},
            self.base + timedelta(seconds=2),
        )
        self.assertEqual(len(actions), 1)
        self.assertIsInstance(actions[0], DispatchAction)
        self.assertEqual(actions[0].session, "claude")
        self.assertEqual(actions[0].command, "help")
        self.assertIn("Command complete", actions[0].message)

    def test_codex_fast_completion_dispatches_continue(self) -> None:
        self.engine.observe_event(
            {"event": "session_state", "session": "codex", "state": "busy", "reason": "input forwarded"},
            self.base,
        )
        self.engine.observe_event(
            {"event": "session_output", "session": "codex", "chunk": "/fast"},
            self.base + timedelta(seconds=1),
        )
        actions = self.engine.observe_event(
            {"event": "session_output", "session": "codex", "chunk": "Fast mode set to off"},
            self.base + timedelta(seconds=2),
        )
        self.assertEqual(len(actions), 1)
        self.assertIsInstance(actions[0], DispatchAction)
        self.assertEqual(actions[0].session, "codex")
        self.assertEqual(actions[0].command, "fast")
        self.assertIn("Command complete", actions[0].message)

    def test_arms_on_prompt_prefixed_slash_echo(self) -> None:
        self.engine.observe_event(
            {"event": "session_state", "session": "codex", "state": "busy", "reason": "input forwarded"},
            self.base,
        )
        actions = self.engine.observe_event(
            {"event": "session_output", "session": "codex", "chunk": "› /fast"},
            self.base + timedelta(seconds=1),
        )
        self.assertEqual(actions, [])
        state = self.engine.session_state("codex")
        self.assertEqual(state.mode, "armed")
        self.assertEqual(state.command, "fast")

    def test_does_not_arm_on_codex_banner_slash_hint(self) -> None:
        self.engine.observe_event(
            {"event": "session_state", "session": "codex", "state": "busy", "reason": "input forwarded"},
            self.base,
        )
        actions = self.engine.observe_event(
            {
                "event": "session_output",
                "session": "codex",
                "chunk": "model: gpt-5.4 xhigh fast /model to change",
            },
            self.base + timedelta(seconds=1),
        )
        self.assertEqual(actions, [])
        state = self.engine.session_state("codex")
        self.assertEqual(state.mode, "idle")
        self.assertIsNone(state.command)

    def test_context_continue_message_includes_captured_text(self) -> None:
        self.engine.observe_event(
            {"event": "session_state", "session": "claude", "state": "busy", "reason": "input forwarded"},
            self.base,
        )
        self.engine.observe_event(
            {"event": "session_output", "session": "claude", "chunk": "/context"},
            self.base + timedelta(seconds=1),
        )
        actions = self.engine.observe_event(
            {
                "event": "session_output",
                "session": "claude",
                "chunk": "Context Usage 55% tokens Autocompact buffer: 33k tokens (3.3%)",
            },
            self.base + timedelta(seconds=2),
        )
        self.assertEqual(len(actions), 1)
        self.assertIn("Autocompact buffer", actions[0].message)

    def test_context_does_not_arm_completion_on_slash_menu_description(self) -> None:
        """Regression for Finding 17: the /context menu description contains
        'current context usage' as a substring. The old 'Context Usage' marker
        was case-insensitive substring-matched and falsely fired on the menu
        text rendered while the user was typing /context before Enter."""
        self.engine.observe_event(
            {"event": "session_state", "session": "claude", "state": "busy", "reason": "input forwarded"},
            self.base,
        )
        self.engine.observe_event(
            {"event": "session_output", "session": "claude", "chunk": "/context"},
            self.base + timedelta(seconds=1),
        )
        menu_chunk = (
            "/context /context Visualize current context usage as a colored grid "
            "/clear Start fresh /compact Clear conversation history but keep a summary in context "
            "/model Set the AI model for Claude Code"
        )
        actions = self.engine.observe_event(
            {"event": "session_output", "session": "claude", "chunk": menu_chunk},
            self.base + timedelta(seconds=2),
        )
        self.assertEqual(
            actions,
            [],
            "Menu description containing 'current context usage' must not fire the watcher",
        )
        self.assertEqual(self.engine.session_state("claude").mode, "armed")

    def test_captured_text_tail_truncation_preserves_result_not_head_noise(self) -> None:
        """Regression for Finding 17 tail-truncation fix: when captured_chunks
        accumulate long head noise (e.g. slash-command menu render) followed
        by the real command result (tail, containing the completion marker),
        tail-truncation must include the marker in the captured text. Under
        the old head-truncation, the first 400 chars were pure noise and the
        marker phrase never made it into the dispatch message."""
        limit = self.config.captured_text_limit_chars
        head_noise = "/context " + ("menu-echo-noise " * ((limit // 16) + 5))
        tail_result = "Context Usage 44k/1m tokens (4%) Autocompact buffer: 33k"
        # Head noise alone exceeds the truncation window, so head-truncation
        # would NEVER include 'Autocompact buffer' in its captured_text.
        self.assertGreater(len(head_noise), limit)

        self.engine.observe_event(
            {"event": "session_state", "session": "claude", "state": "busy", "reason": "input forwarded"},
            self.base,
        )
        self.engine.observe_event(
            {"event": "session_output", "session": "claude", "chunk": "/context"},
            self.base + timedelta(seconds=1),
        )
        self.engine.observe_event(
            {"event": "session_output", "session": "claude", "chunk": head_noise},
            self.base + timedelta(seconds=2),
        )
        actions = self.engine.observe_event(
            {"event": "session_output", "session": "claude", "chunk": tail_result},
            self.base + timedelta(seconds=3),
        )
        self.assertEqual(len(actions), 1)
        # The decisive assertion: tail-truncation MUST include the marker
        # phrase. Head-truncation would NOT.
        self.assertIn("Autocompact buffer", actions[0].message)

    def test_rate_limit_suppresses_rapid_repeat_dispatches(self) -> None:
        state = self.engine.session_state("claude")
        state.dispatch_history = [self.base]
        state.last_dispatch_at = self.base

        self.engine.observe_event(
            {"event": "session_state", "session": "claude", "state": "busy", "reason": "input forwarded"},
            self.base + timedelta(seconds=1),
        )
        self.engine.observe_event(
            {"event": "session_output", "session": "claude", "chunk": "/help"},
            self.base + timedelta(seconds=2),
        )
        actions = self.engine.observe_event(
            {"event": "session_output", "session": "claude", "chunk": "For more help: https://code.claude.com/docs"},
            self.base + timedelta(seconds=3),
        )
        self.assertEqual(len(actions), 1)
        self.assertIsInstance(actions[0], AlertAction)
        self.assertEqual(actions[0].kind, "dispatch_rate_limit")

    def test_watchdog_times_out_without_assistant_turn(self) -> None:
        self.engine.observe_event(
            {"event": "session_state", "session": "claude", "state": "busy", "reason": "input forwarded"},
            self.base,
        )
        self.engine.observe_event(
            {"event": "session_output", "session": "claude", "chunk": "/help"},
            self.base + timedelta(seconds=1),
        )
        actions = self.engine.observe_event(
            {"event": "session_output", "session": "claude", "chunk": "For more help: https://code.claude.com/docs"},
            self.base + timedelta(seconds=2),
        )
        self.assertEqual(len(actions), 1)
        self.engine.mark_dispatch_success("claude", self.base + timedelta(seconds=2))

        alerts = self.engine.tick(
            self.base + timedelta(seconds=2 + self.config.assistant_wait_seconds + 1)
        )
        self.assertEqual(len(alerts), 1)
        self.assertEqual(alerts[0].kind, "assistant_turn_timeout")

    def test_assistant_turn_resets_session(self) -> None:
        self.engine.observe_event(
            {"event": "session_state", "session": "claude", "state": "busy", "reason": "input forwarded"},
            self.base,
        )
        self.engine.observe_event(
            {"event": "session_output", "session": "claude", "chunk": "/help"},
            self.base + timedelta(seconds=1),
        )
        actions = self.engine.observe_event(
            {"event": "session_output", "session": "claude", "chunk": "For more help: https://code.claude.com/docs"},
            self.base + timedelta(seconds=2),
        )
        self.assertEqual(len(actions), 1)
        self.engine.mark_dispatch_success("claude", self.base + timedelta(seconds=2))
        self.engine.observe_event(
            {"event": "session_output", "session": "claude", "chunk": "✻ Hyperspacing… thinking with high effort"},
            self.base + timedelta(seconds=3),
        )
        self.assertEqual(self.engine.session_state("claude").mode, "idle")


if __name__ == "__main__":
    unittest.main()
