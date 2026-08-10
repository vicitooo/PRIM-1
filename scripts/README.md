# Scripts

PRIM-1 is desktop-first. Lifecycle, ordered session inventory, native directory
selection, and session-definition management are available only through the
desktop UI and its in-process Tauri commands. The current tab UI exposes no room
or message composer. There is no master credential file or external operator
backdoor.

## Pane-local control helper

`control-plane.ps1` is a narrow Windows named-pipe client for an already
authorized supervised pane. Its complete action set is:

- `ping`
- `wait_quiet`
- `input`
- `key`

The endpoint must be a nonblank `PRIM1_CONTROL_PLANE_ENDPOINT`. The explicit
`-Endpoint` parameter exists only so isolated named-pipe tests can supply a fake
endpoint. If neither is present, the script fails with a stable direction to use
the desktop UI.

Use the supervisor-injected `PRIM1_PANE_IDENTITY` as `-Session`. User-facing
labels are not pane authority.

```powershell
$env:PRIM1_CONTROL_PLANE_ENDPOINT = '\\.\pipe\<pane-endpoint>'
.\scripts\control-plane.ps1 -Action ping -Quiet
.\scripts\control-plane.ps1 -Action input -Session $env:PRIM1_PANE_IDENTITY -Content 'status'
.\scripts\control-plane.ps1 -Action input -Session $env:PRIM1_PANE_IDENTITY -ContentFile 'D:\tmp\prompt.txt'
.\scripts\control-plane.ps1 -Action key -Session $env:PRIM1_PANE_IDENTITY -Key enter
.\scripts\control-plane.ps1 -Action wait_quiet -Session $env:PRIM1_PANE_IDENTITY -QuietSec 2 -TimeoutSec 10
```

`input` writes raw text and does not press Enter. Use a following `key` action
when submission is intended. `wait_quiet` reports an output-silence hint, not
model completion. Timeout responses exit `124`; ordinary failures exit `1`.
`-PassThruJson` and `-OutRequestIdFile` preserve the minimal response and request
correlation without exposing a runtime snapshot.

The helper never reads `control-plane.json`, `PRIM1_PANE_CREDENTIALS`, or a
bearer token. It does not implement list, lifecycle, delivery, events, signals,
routing, or session-definition management.

`-ContentFile` is decoded as strict UTF-8 and preserves source line endings and
trailing whitespace. `wait_quiet` accepts `QuietSec` 1–60 and `TimeoutSec`
1–300, with `QuietSec` no greater than `TimeoutSec`; the supervisor enforces the
same bounds.

## Thin pane wrappers

- `agent-key.ps1` sends one supported PTY control key.
- `agent-slash.ps1` prepares a slash command as raw pane input; `-ArgsFile` is
  decoded as strict UTF-8 and preserves line endings and trailing whitespace.
  Send Enter separately with `agent-key.ps1`.

Both consume the same endpoint environment variable and accept `-Endpoint` only
for isolated tests.

## Metadata audit summary

`agent-events-summary.py` reads the metadata-only JSONL audit directly. It does
not call the control plane and does not read terminal output or message content.

```powershell
. .\scripts\runtime-paths.ps1
$auditLog = Join-Path (Resolve-Prim1RuntimeDirectory) 'audit\2026-05-17.jsonl'
python .\scripts\agent-events-summary.py --audit-log $auditLog
python .\scripts\agent-events-summary.py --audit-log $auditLog --fail-on alert,blocked,failed,timeout
```

`runtime-paths.ps1` remains the canonical runtime-directory resolver for direct
audit reads. `PRIM1_RUNTIME_DIR` overrides the platform default; paths containing
spaces are supported. It also resolves the pane endpoint from
`PRIM1_CONTROL_PLANE_ENDPOINT` without consulting disk.
