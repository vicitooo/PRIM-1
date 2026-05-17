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

- `tests/control-plane-request-id.ps1`
  - validates `scripts/control-plane.ps1 -PassThruJson`
  - validates `scripts/control-plane.ps1 -OutRequestIdFile`
  - validates `scripts/agent-route.ps1` forwards both request-id helper switches

- `tests/control-plane-deliver-wait.ps1`
  - validates `scripts/control-plane.ps1 -Action deliver`
  - validates `scripts/control-plane.ps1 -Action wait_quiet`
  - verifies mailbox fallback preserves request payloads and extended wait budgets

- `tests/control-plane-timeouts.ps1`
  - validates `scripts/control-plane.ps1` timeout surfacing
  - checks `timed_out: true` returns exit code `124`
  - checks the `TIMED OUT: <message>` banner in both normal and `-Quiet` modes

- `tests/handshake-helpers.ps1`
  - validates `scripts/new-smoke-token.ps1`
  - validates canonical `scripts/handshake-route.ps1` payload generation in `-DryRun` mode
  - validates `scripts/handshake-watchdog.ps1` success detection and timeout dry-run behavior against synthetic audit files

The detailed strategy should be locked during Phase 0.
