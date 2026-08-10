# Tests

From the repository root, create a project-local environment and install the
pinned Python test dependency with:

```powershell
python -m venv .runtime\test-venv
.\.runtime\test-venv\Scripts\python.exe -m pip install -r requirements-test.txt
```

The complete production gate is being consolidated separately; the existing
`tests/run-all.ps1` command covers only deterministic and opt-in live
control-script suites and deliberately identifies that narrower scope.

Expected v1 test mix:

- unit tests for lifecycle, routing, registry, and policy
- integration tests using synthetic child processes
- opt-in real Claude/Codex tests
- explicit stall/restart tests using controlled fixtures

Current script-level coverage:

- `tests/agent-events-summary.test.py`
  - validates `scripts/agent-events-summary.py` metadata summaries for grouped `route_delivery`, overlap-only `dispatch_attempt`, session lifecycle/work state, `supervisor_heartbeat`, `supervisor_alert`, `request_ack`, `request_ack_timeout`, `dispatch_no_reaction`, and failed sideband lifecycle events
  - checks `--fail-on`, `--request-id`, and `--events-since-stdin`

- `tests/runtime-paths.ps1`
  - validates the canonical product runtime resolver and explicit override precedence
  - covers runtime roots containing spaces

- `tests/control-plane-content-file.ps1`
  - validates `scripts/control-plane.ps1 -ContentFile`
  - checks mutual exclusion with `-Content`
  - checks missing-file failure
  - checks non-`input` action rejection
  - captures the named-pipe request payload and asserts the exact string survives shell-hostile characters

- `tests/control-plane-request-id.ps1`
  - validates `scripts/control-plane.ps1 -PassThruJson`
  - validates `scripts/control-plane.ps1 -OutRequestIdFile`
  - validates `scripts/agent-route.ps1` forwards both request-id helper switches

- `tests/control-plane-deliver-wait.ps1`
  - validates `scripts/control-plane.ps1 -Action deliver`
  - validates `scripts/control-plane.ps1 -Action wait_quiet`
  - verifies named-pipe transport preserves request payloads and extended wait budgets

- `tests/control-plane-timeouts.ps1`
  - validates `scripts/control-plane.ps1` timeout surfacing
  - checks `timed_out: true` returns exit code `124`
  - checks the `TIMED OUT: <message>` banner in both normal and `-Quiet` modes

- `tests/routing-stress-sequential.ps1` (live, opt-in)
  - correlates each routed request with one resolved receipt and one successful recipient write
  - validates delivery metadata only; content fidelity requires a receiver-side oracle
