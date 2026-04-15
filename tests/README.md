# tests

Test strategy scaffold.

Expected v1 test mix:

- unit tests for lifecycle, routing, registry, and policy
- integration tests using synthetic child processes
- opt-in real Claude/Codex tests
- explicit stall/restart tests using controlled fixtures

Current script-level coverage:

- `tests/control-plane-content-file.ps1`
  - validates `scripts/control-plane.ps1 -ContentFile`
  - checks mutual exclusion with `-Content`
  - checks missing-file failure
  - checks non-`input` action rejection
  - captures the mailbox fallback payload and asserts the exact string survives shell-hostile characters

The detailed strategy should be locked during Phase 0.
