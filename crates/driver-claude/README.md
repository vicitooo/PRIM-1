# crates/driver-claude

Claude CLI driver.

Expected responsibilities:

- launch Claude
- resume Claude sessions
- normalize Claude session metadata
- define Claude-specific output parsing hooks
- expose Claude-specific interrupt/close behavior

This crate should not own general lifecycle logic; that belongs to the supervisor.

