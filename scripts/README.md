# Scripts

PRIM-1 is desktop-first. Lifecycle, ordered session inventory, native directory
selection, and session-definition management are available only through the
desktop UI and its in-process Tauri commands. Room creation, membership,
and the visible composer are desktop actions. Room members can also request
delivery to fellow members through the pane-local protocol. There is no
master credential file or external operator backdoor.

## Pane-local control helper

`control-plane.ps1` is a narrow Windows named-pipe client for an already
authorized supervised pane. Its complete action set is:

- `ping`
- `wait_quiet`
- `input`
- `key`
- `room_read`
- `room_post`
- `room_deliver`

The endpoint must be a nonblank `PRIM1_CONTROL_PLANE_ENDPOINT`. The explicit
`-Endpoint` parameter exists only so isolated named-pipe tests can supply a fake
endpoint. If neither is present, the script fails with a stable direction to use
the desktop UI.

Use the supervisor-injected `PRIM1_PANE_IDENTITY` as `-Session`. User-facing
labels are not pane authority. Run these examples inside a supervised non-Prime
pane from the checkout root; the endpoint and pane identity are already injected.

```powershell
.\scripts\control-plane.ps1 -Action ping -Quiet
.\scripts\control-plane.ps1 -Action input -Session $env:PRIM1_PANE_IDENTITY -Content 'status'
.\scripts\control-plane.ps1 -Action input -Session $env:PRIM1_PANE_IDENTITY -ContentFile 'D:\tmp\prompt.txt'
.\scripts\control-plane.ps1 -Action key -Session $env:PRIM1_PANE_IDENTITY -Key enter
.\scripts\control-plane.ps1 -Action wait_quiet -Session $env:PRIM1_PANE_IDENTITY -QuietSec 2 -TimeoutSec 10
.\scripts\control-plane.ps1 -Action room_read -Quiet -PassThruJson
.\scripts\control-plane.ps1 -Action room_post -Content 'Status from this pane'
.\scripts\control-plane.ps1 -Action room_deliver -Recipient 'Reviewer' -Content 'Please review the change.'
```

`input` writes raw text and does not press Enter. Use a following `key` action
when submission is intended. `wait_quiet` reports an output-silence hint, not
model completion. Timeout responses exit `124`; ordinary failures exit `1`.
`-PassThruJson` and `-OutRequestIdFile` preserve the minimal response and request
correlation without exposing a runtime snapshot.

`room_read`, `room_post`, and `room_deliver` derive the room, membership, and
sender from the calling pane — kernel Job membership, or the per-run pane
secret (`PRIM1_PANE_SECRET`, sent as the connection's first line) when the
shell runs outside the Job. They accept no `RoomId`, sender, or peer authority;
`room_deliver -Recipient` names a member of the caller's own room (label,
session id, or `all`) and is gated like the operator's Send. A room post writes
no PTY; a newly joined pane reads only its join event and later traffic, with
explicit gaps after eviction/restart. The same requests are available without
the source checkout through the executable's `--prim1-room` commands; see
[CONTROL-SURFACE.md](../CONTROL-SURFACE.md) for working examples.

The helper never reads `control-plane.json` or `PRIM1_PANE_CREDENTIALS`. It
does not implement list, lifecycle, arbitrary routing, signals, or
session/room-definition management.

`-ContentFile` for `input`, `room_post`, or `room_deliver` is decoded as strict UTF-8 and
preserves source line endings and trailing whitespace. `wait_quiet` accepts `QuietSec` 1–60 and `TimeoutSec`
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
$auditName = (Get-Date -Format 'yyyy-MM-dd') + '.jsonl'
$auditLog = Join-Path (Join-Path (Resolve-Prim1RuntimeDirectory) 'audit') $auditName
python .\scripts\agent-events-summary.py --audit-log $auditLog
python .\scripts\agent-events-summary.py --audit-log $auditLog --fail-on alert,blocked,failed,timeout
```

`runtime-paths.ps1` remains the canonical runtime-directory resolver for direct
audit reads. `PRIM1_RUNTIME_DIR` overrides the platform default; paths containing
spaces are supported. It also resolves the pane endpoint from
`PRIM1_CONTROL_PLANE_ENDPOINT` without consulting disk.
