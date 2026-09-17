# crates/driver-claude

Claude CLI driver.

Responsibilities:

- launch Claude
- resume Claude sessions
- select permission flags and validate direct executable launch
- classify Claude-specific output and work-state markers

General lifecycle and routed-input policy belong to the supervisor.
