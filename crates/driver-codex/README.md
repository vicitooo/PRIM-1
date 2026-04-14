# crates/driver-codex

Codex CLI driver.

Expected responsibilities:

- launch Codex
- resume Codex sessions
- normalize Codex session metadata
- define Codex-specific output parsing hooks
- expose Codex-specific interrupt/close behavior

This crate should not own general lifecycle logic; that belongs to the supervisor.

