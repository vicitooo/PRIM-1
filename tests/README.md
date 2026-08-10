# Tests

From the repository root, create a project-local environment and install the
pinned Python test dependency with:

```powershell
python -m venv .runtime\test-venv
.\.runtime\test-venv\Scripts\python.exe -m pip install -r requirements-test.txt
```

The complete production gate is being consolidated separately; the existing
`tests/run-all.ps1` command covers only deterministic control-script suites and
deliberately identifies that narrower scope.

Expected v1 test mix:

- unit tests for lifecycle, routing, registry, and policy
- integration tests using synthetic child processes
- opt-in real Claude/Codex tests
- explicit stall/restart tests using controlled fixtures

Current script-level coverage:

- `tests/agent-events-summary.test.py`
  - validates `scripts/agent-events-summary.py` metadata summaries for grouped `route_delivery`, overlap-only `dispatch_attempt`, session lifecycle/work state, `supervisor_heartbeat`, and `supervisor_alert`
  - checks direct audit reads, `--fail-on`, and `--request-id`

- `tests/runtime-paths.ps1`
  - validates the canonical product runtime resolver and endpoint environment/explicit-test precedence
  - verifies the stable missing-endpoint failure directing operators to the desktop UI
  - covers runtime roots containing spaces

- `tests/control-plane-content-file.ps1`
  - validates `scripts/control-plane.ps1 -ContentFile`
  - checks mutual exclusion with `-Content`
  - checks missing-file failure
  - checks non-`input` action rejection
  - captures the named-pipe request payload and asserts strict UTF-8 Unicode, embedded CRLF, a trailing newline, and shell-hostile characters survive exactly
  - verifies the request contains no token or legacy idle field

- `tests/control-plane-request-id.ps1`
  - validates `scripts/control-plane.ps1 -PassThruJson`
  - validates `scripts/control-plane.ps1 -OutRequestIdFile`
  - validates environment endpoint resolution, explicit test endpoints, `agent-key.ps1`, and legacy-action rejection

- `tests/control-plane-wait.ps1`
  - validates `scripts/control-plane.ps1 -Action wait_quiet`
  - verifies named-pipe transport preserves the minimal tokenless request/response payload

- `tests/control-plane-timeouts.ps1`
  - validates `wait_quiet` timeout surfacing and the 45-second input/key client budget against a response delayed beyond the former 35-second boundary
  - validates the client-side 60-second quiet-window and 300-second total-wait caps; matching server-side bounds are covered by supervisor tests
  - checks `timed_out: true` returns exit code `124`
  - checks the `TIMED OUT: <message>` banner in both normal and `-Quiet` modes
