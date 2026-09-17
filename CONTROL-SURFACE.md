# PRIM-1 — Control Surface

PRIM-1 is desktop-first. The desktop UI is the operator authority surface;
pane-local scripts are deliberately narrower and cannot substitute for it.

## Desktop UI

The desktop owns:

- `SessionId`-addressed create, rename, reorder, delete, launch, stop, restart,
  terminal input, and resize
- an ordered, atomically persisted session inventory and snapshots
- native Windows workspace and per-session working-directory selection, plus a
  backend-qualified absolute Ubuntu path for Prime
- visible `Normal` / `Unsafe` permission profiles, with `Normal` as the default
- ordered `RoomId` create, rename, reorder, membership, and delete
- a bounded visible room feed whose **Post** action writes no PTY, plus explicit
  one-member / **Send All** delivery with per-recipient status
- visible runtime diagnostics

These operations use in-process Tauri commands. They are not exposed through a
master bearer, an info file, or an unaffiliated operator pipe.

The operator schemas reject unknown fields. Lifecycle/input requests carry one
`session_id`; room actions carry opaque `room_id` / `session_id` values and
typed recipient selection.
The Rust boundary derives operator provenance. Mutable labels are never
authority. Create accepts a driver, optional label, typed permission profile,
and an optional room ID for initial membership. Prime create may additionally
carry one typed absolute Ubuntu working
directory; the backend canonicalizes and identity-binds it, and the UI prefills
the qualified Ubuntu home. Launch accepts no renderer-controlled command,
arguments, environment, or native working-directory path. A launch miss on PATH
is shown on the pane; `search: true` asks the backend to walk the usual install
locations (the renderer still cannot supply a program path). Windows folder paths
enter through the native Rust picker. Room definitions and membership persist;
room content remains bounded process-memory state and does not survive restart.
Deleting a room is refused during an in-flight delivery; otherwise it removes
the room and feed while leaving its sessions in the lobby.

Visible terminal shortcuts:

- `Ctrl+Tab` / `Ctrl+Shift+Tab` — select the next / previous session tab
- `Ctrl+Shift+T` — open Attach a harness
- `Ctrl+Shift+W` — close the active fully stopped session after confirmation
- `Ctrl+Shift+Left` / `Ctrl+Shift+Right` — move the focused tab
- `Ctrl+Shift+C` — copy the DOM selection, or the last active terminal selection
- `Ctrl+V` / `Ctrl+Shift+V` / `Shift+Insert` — paste into the focused running pane
- `Ctrl+C` — copy a selection; otherwise pass through to the terminal session
- `F1` — open help
- `F11` — toggle fullscreen

## Pane-local PowerShell helper

`scripts/control-plane.ps1` supports seven actions:

| Action | Required fields | Wire kind |
|---|---|---|
| `ping` | none | `ping` |
| `wait_quiet` | `-Session`, `-QuietSec`, `-TimeoutSec` | `wait_quiet` |
| `input` | `-Session`, `-Content` or `-ContentFile` | `send_input` |
| `key` | `-Session`, `-Key` | `send_key` |
| `room_read` | optional paired `-CursorEpoch`, `-CursorSequence` | `room_read` |
| `room_post` | `-Content` or `-ContentFile` | `room_post` |
| `room_deliver` | `-Recipient` (member label, session id, or `all`), `-Content` or `-ContentFile` | `room_deliver` |

The endpoint comes from a nonblank `PRIM1_CONTROL_PLANE_ENDPOINT`. An explicit
`-Endpoint` may be supplied only by isolated tests using a fake named-pipe
responder. There is no runtime-directory fallback.

The exact self-pane alias comes from the supervisor-injected
`PRIM1_PANE_IDENTITY`. Use that value for `-Session`; display labels such as
“Claude” or “review” are not authority.

When no endpoint is available, the stable failure is:

```text
PRIM1_CONTROL_PLANE_ENDPOINT is not set. External operator control is unavailable; use the PRIM-1 desktop UI.
```

Examples from a supervised pane with its injected environment, from the checkout root:

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

Behavior:

- named-pipe-only; connection and response failures fail closed
- request JSON contains no token, info path, source identity, or compatibility idle flag
- `input` writes raw text and does not press Enter
- `Content` and `ContentFile` are mutually exclusive
- `ContentFile` is strict UTF-8 (BOM optional); malformed UTF-8 fails closed, and CR/LF/trailing whitespace are preserved
- `room_read`, `room_post`, and `room_deliver` derive the exact room and sender
  from the calling pane (kernel Job, or the pane secret when the shell runs
  outside the Job); they accept no `-Session`, `RoomId`, sender, or peer
  authority — `room_deliver`'s `-Recipient` names a member of the caller's own
  room and is gated exactly like the operator's Send
- when `PRIM1_PANE_SECRET` is set in the shell, the script sends it as the
  connection's first line; the supervisor still prefers kernel identity
- a newly added member reads only its join event and later room traffic; an
  evicted or restarted feed returns an explicit cursor gap
- `room_post` changes only the bounded room feed and writes no PTY
- `wait_quiet` is an output-silence hint, not model completion
- `wait_quiet` requires `QuietSec` 1–60, `TimeoutSec` 1–300, and `QuietSec <= TimeoutSec`; the supervisor enforces the same limits
- `PassThruJson` appends the minimal response JSON in quiet mode
- `OutRequestIdFile` atomically stores a returned `request_id`
- `timed_out: true` prints `TIMED OUT: <message>` and exits `124`
- ordinary failures exit `1`

The helper does not accept list, lifecycle, arbitrary cross-room route,
signal, or session/room-management actions. It never reads `control-plane.json`,
`PRIM1_PANE_CREDENTIALS`, or a bearer-token file.

Model shell tools are not assumed to preserve Job membership. Processes outside
all pane Jobs can authenticate with their inherited per-run pane secret. An
unaffiliated process with only the endpoint name is rejected.

## Model-facing MCP bridge

Claude Code and Codex are launched with one session-scoped `prim1_pane` stdio
MCP server. The server is the PRIM-1 executable in a dedicated no-UI mode and
is created by the harness. It exposes `ping`, `room_read`, `room_post`, and
`room_deliver`. The delivery tool accepts a member label, session ID, or `all`
(every other member); none accepts a caller-supplied `RoomId`, sender, or
lifecycle authority. Each tool call opens the existing
named pipe, pins and verifies the expected desktop server process, and then
uses the same caller/run/membership checks as the PowerShell
helper. MCP frames and control-plane frames are both bounded and fail closed.
Codex forwards the required injected transport, desktop-server identity, and
per-run pane-secret variables to this child. The supervisor still derives room
and sender from the authenticated live run.
Claude receives an ephemeral, process-local allowlist for exactly these four
fully qualified MCP tools. That makes the safe pane tools autonomous without a
per-call approval or persistent harness setting; every other Claude tool keeps
the selected Normal/Unsafe permission policy, and newly added MCP tools remain
unapproved by default.

Grok receives no MCP configuration from PRIM-1. It and other non-Prime panes can
use the same executable's room CLI through a shell tool, without a source checkout:

```powershell
& "$env:PRIM1_CLI" --prim1-room ping
& "$env:PRIM1_CLI" --prim1-room read
& "$env:PRIM1_CLI" --prim1-room post 'Status from this pane'
& "$env:PRIM1_CLI" --prim1-room deliver 'Reviewer' 'Please review the change.'
```

The CLI uses the pane's injected environment and the same authorization checks.
Prime has neither this sideband nor synthetic delivery; use its raw terminal.

## Thin pane wrappers

`agent-key.ps1` forwards one supported PTY key. `agent-slash.ps1` constructs a
slash command as raw input; its file-based arguments are strict UTF-8 and retain
line endings and trailing whitespace. Use a separate key call to submit it.
Both use the same endpoint rule.

```powershell
.\scripts\agent-slash.ps1 -Session $env:PRIM1_PANE_IDENTITY -Slash compact -ArgsFile 'D:\tmp\compact-args.txt'
.\scripts\agent-key.ps1 -Session $env:PRIM1_PANE_IDENTITY -Key enter
```

## Metadata audit

The durable audit remains a direct, read-only metadata surface at
`<runtime-dir>/audit/YYYY-MM-DD.jsonl`. Raw terminal-output events are omitted and
room/routed-message bodies are redacted. Failure diagnostics may contain harness
error text; inspect logs before sharing them. A delivery or write receipt is not
proof that a model understood or completed work.

`agent-events-summary.py` reads that JSONL directly:

```powershell
. .\scripts\runtime-paths.ps1
$auditName = (Get-Date -Format 'yyyy-MM-dd') + '.jsonl'
$auditLog = Join-Path (Join-Path (Resolve-Prim1RuntimeDirectory) 'audit') $auditName
python .\scripts\agent-events-summary.py --audit-log $auditLog
```

There is no durable `events_since` wrapper. The pane-local `room_read` cursor
addresses only the caller's authorized bounded in-memory room feed and reports
gaps explicitly.

## Git Bash / MSYS

Leading-slash arguments can be rewritten by MSYS. Wrap the PowerShell invocation
as one command when sending slash input:

```bash
powershell -NoProfile -Command '& ".\scripts\agent-slash.ps1" -Session $env:PRIM1_PANE_IDENTITY -Slash compact'
```

## Authority boundary

An endpoint name is routing metadata, not authority. The supervisor binds the
caller to one live run through kernel Job membership, or through its per-run
pane secret when the caller is outside all pane Jobs. A secret for another run
cannot override a kernel-attributed pane. Bearer files are not a fallback.
Prime/WSL sideband access is deliberately disabled. The native named-pipe policy
cannot derive a Linux task identity from Windows Job membership, and there is no
bearer or compatibility fallback. Prime uses raw desktop terminal input; routed
delivery is also rejected until a measured Prime submit protocol exists.
