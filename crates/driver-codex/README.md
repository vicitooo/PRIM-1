# crates/driver-codex

Codex CLI driver.

Responsibilities:

- launch Codex
- resume Codex sessions
- select permission flags and validate direct executable launch
- classify Codex output with bounded prompt, modal, and work-state tracking

General lifecycle and routed-input policy belong to the supervisor.

Prompt detection accepts freshly repainted prompt and footer rows after a resize
and treats Codex's Braille decoration as spaces. Modal blockers still require
trusted screen evidence; transient notices clear on a later clean prompt or
activity. Regression tests cover these behaviors together so merging the viewport
fixes does not restore the old transcript-based blocking behavior.
