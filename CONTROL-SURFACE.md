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
- ordered `RoomId` create, rename, reorder, membership, and closed-room delete
- a bounded visible room feed whose **Post** action writes no PTY, plus explicit
  one-member / **Send All** delivery with per-recipient status
- visible runtime diagnostics

These operations use in-process Tauri commands. They are not exposed through a
master bearer, an info file, or an unaffiliated operator pipe.

The operator schemas reject unknown fields. Lifecycle/input requests carry one
`session_id`; room actions carry opaque `room_id` / `session_id` values and
typed recipient selection.
The Rust boundary derives operator provenance. Mutable labels are never
authority. Create accepts only a driver, optional label, and typed permission
profile. Prime create may additionally carry one typed absolute Ubuntu working
directory; the backend canonicalizes and identity-binds it, and the UI prefills
the qualified Ubuntu home. Launch accepts no renderer-controlled command,
arguments, environment, or native working-directory path. Windows folder paths
enter through the native Rust picker. Room definitions and membership persist;
room content remains bounded process-memory state and does not survive restart.

Visible terminal shortcuts:

- `Ctrl+Tab` / `Ctrl+Shift+Tab` — select the next / previous session tab
- `Ctrl+Shift+T` — open New Session
- `Ctrl+Shift+W` — close the active fully stopped session after confirmation
- `Ctrl+Shift+Left` / `Ctrl+Shift+Right` — move the focused tab
- `Ctrl+Shift+C` — copy the DOM selection, or the last active terminal selection
- `Ctrl+Shift+V` — paste into the focused running pane
- `Ctrl+C` — pass through to the terminal session
- `F11` — toggle fullscreen

## Pane-local PowerShell helper

`scripts/control-plane.ps1` supports exactly six actions:

| Action | Required fields | Wire kind |
|---|---|---|
| `ping` | none | `ping` |
| `wait_quiet` | `-Session`, `-QuietSec`, `-TimeoutSec` | `wait_quiet` |
| `input` | `-Session`, `-Content` or `-ContentFile` | `send_input` |
| `key` | `-Session`, `-Key` | `send_key` |
| `room_read` | optional paired `-CursorEpoch`, `-CursorSequence` | `room_read` |
| `room_post` | `-Content` or `-ContentFile` | `room_post` |

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

Examples from an authorized supervised pane:

```powershell
.\scripts\control-plane.ps1 -Action ping -Quiet
.\scripts\control-plane.ps1 -Action input -Session $env:PRIM1_PANE_IDENTITY -Content 'status'
.\scripts\control-plane.ps1 -Action input -Session $env:PRIM1_PANE_IDENTITY -ContentFile 'D:\tmp\prompt.txt'
.\scripts\control-plane.ps1 -Action key -Session $env:PRIM1_PANE_IDENTITY -Key enter
.\scripts\control-plane.ps1 -Action wait_quiet -Session $env:PRIM1_PANE_IDENTITY -QuietSec 2 -TimeoutSec 10
.\scripts\control-plane.ps1 -Action room_read -Quiet -PassThruJson
.\scripts\control-plane.ps1 -Action room_post -Content 'Status from this pane'
```

Behavior:

- named-pipe-only; connection and response failures fail closed
- request JSON contains no token, info path, source identity, or compatibility idle flag
- `input` writes raw text and does not press Enter
- `Content` and `ContentFile` are mutually exclusive
- `ContentFile` is strict UTF-8 (BOM optional); malformed UTF-8 fails closed, and CR/LF/trailing whitespace are preserved
- `room_read` and `room_post` derive the exact room and sender from the
  kernel-bound calling pane; they accept no `-Session`, `RoomId`, sender, peer,
  recipient, or delivery authority
- a newly added member reads only its join event and later room traffic; an
  evicted or restarted feed returns an explicit cursor gap
- `room_post` changes only the bounded room feed and writes no PTY
- `wait_quiet` is an output-silence hint, not model completion
- `wait_quiet` requires `QuietSec` 1–60, `TimeoutSec` 1–300, and `QuietSec <= TimeoutSec`; the supervisor enforces the same limits
- `PassThruJson` appends the minimal response JSON in quiet mode
- `OutRequestIdFile` atomically stores a returned `request_id`
- `timed_out: true` prints `TIMED OUT: <message>` and exits `124`
- ordinary failures exit `1`

The helper does not accept list, lifecycle, recipient delivery, arbitrary route,
signal, or session/room-management actions. It never reads `control-plane.json`,
`PRIM1_PANE_CREDENTIALS`, or any bearer token.

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
`<runtime-dir>/audit/YYYY-MM-DD.jsonl`. It contains no terminal output or routed
message content. A delivery or write receipt is not proof that a model understood
or completed work.

`agent-events-summary.py` reads that JSONL directly:

```powershell
. .\scripts\runtime-paths.ps1
$auditLog = Join-Path (Resolve-Prim1RuntimeDirectory) 'audit\2026-05-17.jsonl'
python .\scripts\agent-events-summary.py --audit-log $auditLog
```

There is no durable `events_since` wrapper. The pane-local `room_read` cursor
addresses only the caller's authorized bounded in-memory room feed and reports
gaps explicitly.

## Git Bash / MSYS

Leading-slash arguments can be rewritten by MSYS. Wrap the PowerShell invocation
as one command when sending slash input:

```bash
powershell -Command "& '.\scripts\agent-slash.ps1' -Session claude -Slash compact"
```

## Authority boundary

An endpoint name is routing metadata, not authority. The supervisor must derive
the native caller from the kernel and bind it to one live pane process job and
generation when a mutating request is handled. Bearer files are not a fallback.
Prime/WSL sideband access is deliberately disabled. The native named-pipe policy
cannot derive a Linux task identity from Windows Job membership, and there is no
bearer or compatibility fallback. Prime uses raw desktop terminal input; routed
delivery is also rejected until a measured Prime submit protocol exists.
