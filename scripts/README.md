# scripts

Helper scripts for operators and agents.

Expected responsibilities:

- send message helpers
- spawn/restart/close helpers
- local smoke-test helpers
- developer utilities during bring-up

Scripts should wrap the control plane, not bypass the supervisor.

## Current scripts

- `control-plane.ps1`
  Windows helper for the first MVP. Resolves `<runtime-dir>/control-plane.json`, connects to the supervisor named pipe, and issues:
  - `ping`
  - `list`
  - `start`
  - `stop`
  - `restart`
  - `input`
    - inline `-Content`
    - file-based `-ContentFile`
  - `deliver`
    - inline `-Content`
    - file-based `-ContentFile`
  - `wait_quiet`
  - `events_since`
  - `key`
  - `route`
  - optional `-Quiet` mode for agent-friendly success/error output
  - optional `-PassThruJson` for full response JSON alongside quiet output
  - optional `-OutRequestIdFile <path>` for request-id correlation

Example:

```powershell
.\scripts\control-plane.ps1 -Action route -From operator -To claude -Scope direct -Content "Review the Codex pane."
.\scripts\control-plane.ps1 -Action start -Session claude -ExtraArgs '--resume', '00000000-0000-0000-0000-000000000000'
.\scripts\control-plane.ps1 -Action input -Session claude -ContentFile ".runtime\compact-prompts\compact-full.txt"
.\scripts\control-plane.ps1 -Action deliver -Session claude -Content "Multi-line`nmessage body"
.\scripts\control-plane.ps1 -Action route -From operator -To codex -Content "status ping" -RequireIdle
.\scripts\control-plane.ps1 -Action wait_quiet -Session claude -QuietSec 2 -TimeoutSec 10
.\scripts\agent-events.ps1 -Consumer outside-supervisor -MaxEvents 200 -MaxWaitSeconds 15
.\scripts\control-plane.ps1 -Action route -From operator -To claude -Content "status ping" -Quiet -PassThruJson -OutRequestIdFile ".runtime\last-request-id.txt"
```

Notes:

- transport is named-pipe-only and fails closed when the pipe cannot be reached
- `start` accepts `-ExtraArgs <string[]>` for per-launch CLI arguments such as `--resume <session-id>`; the supervisor appends them to the cloned launch definition and writes them to the `start_session` audit lifecycle entry
- `wait_quiet` honors `-TimeoutSec` and the client keeps a longer matching named-pipe read budget
- `events_since` always emits structured JSON and never routes through `-Quiet`
- `events_since` extends the named-pipe read budget beyond `MaxWaitSeconds`
- `-PassThruJson` keeps quiet message output and appends the full sideband response JSON
- `-OutRequestIdFile` writes the raw `request_id` string when the response carries one
- `-RequireIdle` and `-AllowBusy` are supported for `input`, `key`, `deliver`, and `route`; default mode matches `-AllowBusy`, while `-RequireIdle` aborts before PTY write when `dispatch_attempt.overlap` is true
- timeout responses now print `TIMED OUT: <message>` and exit `124`
- ordinary non-timeout failures still exit `1`

- `agent-route.ps1`
  Minimal agent-facing wrapper over `control-plane.ps1` for direct or room messages without the verbose JSON snapshot payload. Supports `-RequireIdle`, `-AllowBusy`, `-PassThruJson`, and `-OutRequestIdFile` passthrough for dispatch/ACK correlation.

Example:

```powershell
.\scripts\agent-route.ps1 -From codex -To claude -Content "Reply with exactly: CLAUDE ACK"
```

- `agent-key.ps1`
  Minimal helper for PTY control keys routed through the supervisor. Supports:
  - `enter`
  - `up`
  - `down`
  - `left`
  - `right`
  - `tab`
  - `esc`
  - `ctrl_c`

Example:

```powershell
.\scripts\agent-key.ps1 -Session claude -Key enter
.\scripts\agent-key.ps1 -Session codex -Key down
```

- `agent-slash.ps1`
  Minimal helper for slash commands that may take their arguments from a file, without shell-quoting the payload.

Example:

```powershell
.\scripts\agent-slash.ps1 -Session claude -Slash compact -ArgsFile ".runtime\compact-prompts\compact-args.txt"
.\scripts\agent-key.ps1 -Session claude -Key enter
```

- `agent-events.ps1`
  Outside-supervisor convenience wrapper over `control-plane.ps1 -Action events_since`. Uses `<runtime-dir>/cursors/<consumer>.json`, starts from `null` on first run, writes `next_cursor` back atomically, and prints one compressed JSON line per event. Its defaults cover delivery receipts, dispatch attempts, session lifecycle/work state, supervisor health, system logs, sideband lifecycle, and request ACK metadata. It does not provide terminal output or routed-message content.

Example:

```powershell
.\scripts\agent-events.ps1 -Consumer outside-supervisor
```

- `agent-events-summary.py`
  Human-readable summary helper for durable metadata emitted by the control plane. Reads a JSONL audit log directly, or `events_since` JSON from stdin. Groups `route_delivery` phases into one logical route line and summarizes overlap-only `dispatch_attempt`, session lifecycle/work state, `supervisor_heartbeat`, `supervisor_alert`, `request_ack`, `request_ack_timeout`, `dispatch_no_reaction`, and failed/timed-out sideband lifecycle events.

Examples:

```powershell
. .\scripts\runtime-paths.ps1
$auditLog = Join-Path (Resolve-Prim1RuntimeDirectory) "audit\2026-05-17.jsonl"
python .\scripts\agent-events-summary.py --audit-log $auditLog
python .\scripts\agent-events-summary.py --audit-log $auditLog --fail-on alert,blocked,failed,timeout
.\scripts\control-plane.ps1 -Action events_since -MaxEvents 200 | python .\scripts\agent-events-summary.py --events-since-stdin
```

Runtime env vars used by the wrapper defaults:

- `PRIM1_RUNTIME_DIR`: override the product runtime directory used by scripts
- `PRIM1_HEARTBEAT_INTERVAL_SECS`: supervisor heartbeat interval in seconds; default `1800`
- `PRIM1_AUTO_RESTART_ON_STALL`: comma-separated session allowlist for stall auto-restart, for example `claude,codex`; default empty/off
- `PRIM1_AUTO_RESTART_STALL_THRESHOLD_SECS`: blocked/error-loop threshold before auto-restart; default `600`

- `runtime-paths.ps1`
  Canonical dot-sourced resolver for PowerShell product scripts. Explicit `-InfoFile` values win; `PRIM1_RUNTIME_DIR` overrides the platform default. Windows defaults to `%LOCALAPPDATA%\io.prim1.runtime\runtime`; macOS resolves to `~/Library/Application Support/io.prim1.runtime/runtime`; Linux resolves below `${XDG_DATA_HOME:-~/.local/share}/io.prim1.runtime/runtime`. The latter two mappings describe path behavior, not validated release support.
