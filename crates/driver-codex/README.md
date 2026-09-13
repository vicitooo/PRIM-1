# crates/driver-codex

Codex CLI driver.

Expected responsibilities:

- launch Codex
- resume Codex sessions
- normalize Codex session metadata
- define Codex-specific output parsing hooks
- expose Codex-specific interrupt/close behavior

This crate should not own general lifecycle logic; that belongs to the supervisor.

Prompt detection accepts freshly repainted prompt and footer rows after a resize
and treats Codex's Braille decoration as spaces. Modal blockers still require
trusted screen evidence; transient notices clear on a later clean prompt or
activity. Regression tests cover these behaviors together so merging the viewport
fixes does not restore the old transcript-based blocking behavior.
