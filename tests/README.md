# Tests

From the repository root, create a project-local environment and install the
pinned Python test dependency with:

```powershell
python -m venv .runtime\test-venv
.\.runtime\test-venv\Scripts\python.exe -m pip install -r requirements-test.txt
```

`tests/run-all.ps1` covers the deterministic control-script suites below. It
does not run the Rust workspace, frontend tests, or real harness interaction.

From the repository root:

```powershell
.\tests\run-all.ps1
```

Frontend tests live in `apps/desktop` and run with `npm test`. Rust tests live
beside their modules and run with `cargo test --release --workspace --features tauri/custom-protocol`
after building the frontend as described in
[README.md](../README.md). Real CLI interaction requires installed, authenticated
harnesses and separate desktop verification; deterministic tests do not establish
provider compatibility.

The known Windows descendant-containment test failure is recorded in
[known issues](../docs/KNOWN_ISSUES.md#test-environment-note). Its cause is not
established.

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
  - checks that actions without content support reject `-ContentFile`
  - captures the named-pipe request payload and asserts strict UTF-8 Unicode, embedded CRLF, a trailing newline, and shell-hostile characters survive exactly
  - verifies the request contains no token or legacy idle field

- `tests/control-plane-request-id.ps1`
  - validates `scripts/control-plane.ps1 -PassThruJson`
  - validates `scripts/control-plane.ps1 -OutRequestIdFile`
  - validates environment endpoint resolution, explicit test endpoints, `agent-key.ps1`, and legacy-action rejection

- `tests/control-plane-server-identity.ps1`
  - proves the client authenticates the exact named-pipe server PID and creation FILETIME before sending request bytes
  - verifies PID and creation-time mismatch each send zero bytes

- `tests/control-plane-wait.ps1`
  - validates `scripts/control-plane.ps1 -Action wait_quiet`
  - verifies named-pipe transport preserves the minimal tokenless request/response payload

- `tests/control-plane-timeouts.ps1`
  - validates `wait_quiet` timeout surfacing and the 45-second input/key client budget against a response delayed beyond the former 35-second boundary
  - validates the client-side 60-second quiet-window and 300-second total-wait caps; matching server-side bounds are covered by supervisor tests
  - checks `timed_out: true` returns exit code `124`
  - checks the `TIMED OUT: <message>` banner in both normal and `-Quiet` modes

- `tests/control-plane-room.ps1`
  - validates `room_read` / `room_post` / `room_deliver` request shapes against a real named pipe
  - proves the script carries no caller-supplied `RoomId`, sender, or peer authority, and that `room_deliver` carries exactly kind, recipient, content
  - proves the pane-secret preamble is the connection's first line when `PRIM1_PANE_SECRET` is set, and never rides inside a request
  - checks paired cursor validation and strict UTF-8 Unicode, embedded CRLF, and trailing-newline fidelity for feed posts

- `tests/run-all-regression.ps1`
  - proves the eight-suite aggregate runner reports one failing suite without hiding seven passes
  - proves a missing native exit code fails all eight suites closed
