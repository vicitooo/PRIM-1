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
  Windows helper for the first MVP. Reads `.runtime/control-plane.json`, connects to the supervisor named pipe, and issues:
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
.\scripts\control-plane.ps1 -Action wait_quiet -Session claude -QuietSec 2 -TimeoutSec 10
.\scripts\control-plane.ps1 -Action events_since -CursorFile ".runtime\cursors\outside-supervisor.json" -MaxEvents 200 -MaxWaitSeconds 15 -IncludeKinds "routed_message","session_state","system_log","sideband_request_lifecycle" -OutCursorFile ".runtime\cursors\outside-supervisor.json"
.\scripts\control-plane.ps1 -Action route -From operator -To claude -Content "status ping" -Quiet -PassThruJson -OutRequestIdFile ".runtime\last-request-id.txt"
```

Notes:

- mailbox fallback keeps a 10-second response window by default
- `start` accepts `-ExtraArgs <string[]>` for per-launch CLI arguments such as `--resume <session-id>`; the supervisor appends them to the cloned launch definition and writes them to the `start_session` audit lifecycle entry
- `deliver` and `wait_quiet` honor `-TimeoutSec` for longer mailbox-backed waits when needed
- `events_since` always emits structured JSON and never routes through `-Quiet`
- `events_since` extends pipe/mailbox waits to `MaxWaitSeconds + 5`
- `-PassThruJson` keeps quiet message output and appends the full sideband response JSON
- `-OutRequestIdFile` writes the raw `request_id` string when the response carries one
- timeout responses now print `TIMED OUT: <message>` and exit `124`
- ordinary non-timeout failures still exit `1`

- `agent-route.ps1`
  Minimal agent-facing wrapper over `control-plane.ps1` for direct or room messages without the verbose JSON snapshot payload. Supports `-PassThruJson` and `-OutRequestIdFile` passthrough for ACK correlation.

Example:

```powershell
.\scripts\agent-route.ps1 -From codex -To claude -Content "Reply with exactly: CLAUDE ACK"
```

- `agent-ping.ps1`
  Token-based smoke-test helper for agents. Builds the exact reply prompt for the target and routes it through the supervisor.

Example:

```powershell
.\scripts\agent-ping.ps1 -From codex -To claude -Token CLAUDE_ACK
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
  Outside-supervisor convenience wrapper over `control-plane.ps1 -Action events_since`. Uses `.runtime/cursors/<consumer>.json`, starts from `null` on first run, writes `next_cursor` back atomically, and prints one compressed JSON line per event. Default signal events include route receipts, pane signals, session lifecycle, session work-state, system logs, sideband lifecycle, and request ACK events.

Example:

```powershell
.\scripts\agent-events.ps1 -Consumer outside-supervisor
```

- `agent-events-summary.py`
  Human-readable summary helper for the receipt/signal event families emitted by the control plane. Reads a JSONL audit log directly, or `events_since` JSON from stdin. Groups `route_delivery` phases into one logical route line and summarizes `pane_signal`, `session_work_state`, `request_ack`, `request_ack_timeout`, and failed/timed-out sideband lifecycle events.

Examples:

```powershell
python .\scripts\agent-events-summary.py --audit-log ".runtime\audit\2026-05-17.jsonl"
python .\scripts\agent-events-summary.py --audit-log ".runtime\audit\2026-05-17.jsonl" --task-id smoke-task-418
python .\scripts\agent-events-summary.py --audit-log ".runtime\audit\2026-05-17.jsonl" --fail-on blocked,failed,timeout
.\scripts\control-plane.ps1 -Action events_since -MaxEvents 200 | python .\scripts\agent-events-summary.py --events-since-stdin
```

- `new-smoke-token.ps1`
  Generates a unique `SMOKE-XXXXXXXX` token plus a UTC-timestamped handshake file path.

Example:

```powershell
.\scripts\new-smoke-token.ps1
```

- `handshake-route.ps1`
  Canonical builder/router for handshake messages so agents stop hand-writing fragile preambles and status lines.

Example:

```powershell
.\scripts\handshake-route.ps1 -Actor claude -Action start -Token "SMOKE-1A2B3C4D" -Path "<repo-root>\.runtime\smoke\handshake-20260415T120000Z-SMOKE-1A2B3C4D.txt"
.\scripts\handshake-route.ps1 -Actor codex -Action ready -Token "SMOKE-1A2B3C4D"
```

- `handshake-watchdog.ps1`
  External handshake timeout watcher. Polls the audit log and routes a canonical timeout FAIL if the run stalls without a terminal marker.

Example:

```powershell
.\scripts\handshake-watchdog.ps1 -Token "SMOKE-1A2B3C4D"
```
