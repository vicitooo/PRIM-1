# PRIM-1 — Control Surface

This file lists the current controls that the operator, Claude, Codex, and future terminal-launched agents can use to operate the wrapper.

## 1. Desktop UI

### Agent panes

- `Launch`
- `Restart`
- `Stop`
- direct terminal typing into Claude or Codex
- direct terminal paste into the focused pane

### Routed message form

- `From`: `operator`, `Claude`, `Codex`
- `To`: `Claude`, `Codex`, `Room`
- `Content`: freeform message body
- `Send routed message`
- `Clear`

### Floating control

- `Refresh snapshot`
- `Main menu` placeholder
- `Show control file`

## 2. Keyboard shortcuts

These shortcuts cover the visible copy/paste surfaces in the room.

- `Ctrl+Shift+C`
  - copies the current selection from:
    - Claude
    - Codex
    - System log
    - normal `DOM` text such as the control-plane path or audit-log path
  - precedence is explicit:
    - `DOM` selection first
    - otherwise the last active terminal surface
- `Ctrl+Shift+V`
  - pastes clipboard text into the focused running pane
- `F11`
  - toggles the desktop shell between fullscreen and windowed mode
  - startup still defaults to fullscreen
  - the key is intercepted at the window layer before xterm.js sees it
- `Alt+V`
  - preserved as the screenshot shortcut

Notes:

- `Ctrl+C` is still passed through to the terminal session itself
- `Ctrl+Shift+C` is verified on the real desktop build across Claude, Codex, System log, and `DOM` text surfaces
- `Ctrl+Shift+V` still applies only to the focused running agent pane
- `F11` is verified both with the window generally focused and after clicking into a pane

## 3. Control-plane helper

Primary helper:

- `scripts/control-plane.ps1`

Supported actions:

- `ping`
- `list`
- `start`
- `stop`
- `restart`
- `input`
- `deliver`
- `wait_quiet`
- `events_since`
- `key`
- `route`

The task-shaped `signal` action remains accepted for backward compatibility, but it is not part of the supported operator workflow or durable task state.

Canonical examples:

```bash
powershell -Command "& '.\scripts\control-plane.ps1' -Action list"
powershell -Command "& '.\scripts\control-plane.ps1' -Action start -Session claude"
powershell -Command "& '.\scripts\control-plane.ps1' -Action start -Session claude -ExtraArgs '--resume','00000000-0000-0000-0000-000000000000'"
powershell -Command "& '.\scripts\control-plane.ps1' -Action input -Session codex -Content 'hi'"
powershell -Command "& '.\scripts\control-plane.ps1' -Action input -Session codex -Content 'hi' -RequireIdle"
powershell -Command "& '.\scripts\control-plane.ps1' -Action input -Session claude -ContentFile 'D:\tmp\compact.txt'"
powershell -Command "& '.\scripts\control-plane.ps1' -Action deliver -Session claude -Content 'hello from operator'"
powershell -Command "& '.\scripts\control-plane.ps1' -Action wait_quiet -Session claude -QuietSec 2 -TimeoutSec 10"
powershell -Command "& '.\scripts\agent-events.ps1' -Consumer outside-supervisor -MaxEvents 200 -MaxWaitSeconds 15"
powershell -Command "& '.\scripts\control-plane.ps1' -Action key -Session claude -Key enter"
powershell -Command "& '.\scripts\control-plane.ps1' -Action route -From operator -To room -Scope room -Content 'status ping'"
```

Behavior:

- named pipe only; transport failures fail closed
- default credential resolution is now layered:
  - explicit `-InfoFile` wins
  - otherwise `PRIM1_PANE_CREDENTIALS` is used when present inside a supervised pane
  - otherwise `PRIM1_RUNTIME_DIR` overrides the platform runtime default
  - on Windows the default master file is `%LOCALAPPDATA%\io.prim1.runtime\runtime\control-plane.json`
- `-Quiet` prints only the success/error message for agent-friendly use
- `-PassThruJson` additionally prints the full sideband response JSON, including `request_id`, even when `-Quiet` is set
- `-OutRequestIdFile <path>` atomically writes the response `request_id` when one is present
- `-Action start` accepts `-ExtraArgs <string[]>`; values are appended to the spawned driver's launch args for that invocation only and are recorded on the `start_session` sideband audit event
- `-Action input` injects raw text only; it does **not** press Enter for you
- `-Action input` now accepts either:
  - `-Content '<inline text>'`
  - `-ContentFile '<path to UTF-8 text file>'`
- `input`, `key`, `deliver`, and `route` accept `-RequireIdle` or `-AllowBusy`
  - default mode emits `dispatch_attempt` and proceeds even when overlap is detected
  - `-AllowBusy` is explicit default-mode behavior
  - `-RequireIdle` aborts before any PTY write when overlap is detected and returns `ok=false`
- `-Action deliver` delivers a conversational message with driver-aware preprocessing and Enter submission
- `-Action deliver` accepts either:
  - `-Content '<inline text>'`
  - `-ContentFile '<path to UTF-8 text file>'`
- large Codex-targeted `deliver` payloads are flattened and chunked below the paste-staging threshold before each Enter submit
- `-Action wait_quiet` waits for no real session content for `-QuietSec` seconds, up to `-TimeoutSec`
- `wait_quiet` is **not** turn-completion detection; long-think phases may go quiet while the assistant is still in flight
- `events_since` returns structured JSON only; it does **not** honor `-Quiet`
- `events_since` reads from a caller-managed cursor file/object and returns:
  - `events`
  - `next_cursor`
  - `gap_detected`
  - `as_of`
- raw `events_since` calls with no kind filter use the server's compatibility metadata set:
  - `session_state`
  - `session_exit`
  - `session_work_state`
  - `supervisor_heartbeat`
  - `supervisor_alert`
  - `dispatch_template_warning`
  - `pair_created`
  - `pair_renamed`
  - `pair_deleted`
  - `routed_message`
  - `route_delivery`
  - `dispatch_attempt`
  - `pane_signal`
  - `system_log`
  - `control_plane_ready`
  - `sideband_request_lifecycle`
  - `request_ack`
  - `request_ack_timeout`
  - `dispatch_no_reaction`
- `routed_message` content is `[content omitted]`; legacy `pane_signal` task fields are blank in durable events
- prefer `agent-events.ps1` for the narrower metadata-only operator defaults
- `events_since` accepts:
  - `-Cursor` or `-CursorFile`
  - `-MaxEvents`
  - `-MaxWaitSeconds`
  - `-IncludeKinds`
  - `-IncludeSessions`
  - `-IncludeScopes`
  - `-OutCursorFile`
- named-pipe read budgets are action-specific; `wait_quiet` and `events_since` extend the client budget beyond their requested supervisor wait
- `-Content` and `-ContentFile` are mutually exclusive
- `-ContentFile` is only supported for `-Action input` or `-Action deliver`
- if you need a typed prompt to execute, follow `-Action input` with `-Action key -Key enter`
- pane-bound credentials now live at:
  - `<runtime-dir>/control-plane-claude.json`
  - `<runtime-dir>/control-plane-codex.json`
- the master credentials file `<runtime-dir>/control-plane.json` remains the operator/debug backdoor

### 3a. Sideband timeout contract

Per-action supervisor budgets:

- `ping` / `list`: 2 s
- `pane_signal`: 5 s
- `create_pair` / `stop`: 10 s
- `send_input` / `key` / `deliver`: 20 s
- `route`: 30 s
- `wait_quiet`: requested timeout + 5 s supervisor budget
- `events_since`: requested `max_wait_seconds` + 5 s supervisor budget
- `start`: 60 s
- `restart`: 70 s

What callers see:

- every sideband response includes `request_id` when the request decoded successfully
- pane-bound `send_input`, `key`, `deliver`, and `route` requests emit `request_ack` after PTY writes complete; if the write path remains incomplete past `PRIM1_REQUEST_ACK_TIMEOUT_SECS` (default 60), they emit `request_ack_timeout` plus `supervisor_alert` (`alert_type=ack_timeout`, `severity=warn`)
- pane-bound `send_input`, `key`, `deliver`, and `route` requests emit `dispatch_attempt` before any PTY write, carrying the target lifecycle/work-state snapshot, recent outbound-route timestamp, overlap boolean, and reason
- `-RequireIdle` aborts overlapping dispatches before PTY write; aborted `route` requests emit per-recipient `dispatch_attempt` events but no `route_delivery` or `request_ack`
- `route` requests emit `route_delivery` with one `resolved` event for the resolved pane fan-out and one `written` or `failed` event per pane recipient; each event carries `request_id`, `route_id`, `logical_to`, `recipient_count`, `payload_part_count`, `bytes_written`, and optional `error`
- process exits emit `session_exit` alongside the existing `session_state`, carrying `generation`, `process_id`, optional `exit_code` / `signal`, `success`, `reason`, and `requested`
- pane output can emit `session_work_state` on semantic work-state transitions only; events carry `session`, `state`, optional `detail`, `previous_state`, and `timestamp`
- the wrapper emits `supervisor_heartbeat` every `PRIM1_HEARTBEAT_INTERVAL_SECS` (default 1800) with wrapper PID, uptime, and per-session lifecycle/work summaries
- if `PRIM1_AUTO_RESTART_ON_STALL` includes a session name, `blocked`/`error_loop` work state lasting longer than `PRIM1_AUTO_RESTART_STALL_THRESHOLD_SECS` (default 600) emits critical `supervisor_alert` and internally restarts that session
- auto-restart is capped at 3 restarts per session per 30-minute wrapper lifetime window; the 4th eligible stall emits a critical `supervisor_alert` and leaves the pane for manual intervention
- failed or timed-out `sideband_request_lifecycle` audit events include an optional `error` field with the response message
- `timed_out: true` now returns exit code `124`
- non-`-Quiet` mode prints `TIMED OUT: <message>` instead of JSON for timeout responses
- `-Quiet` mode also surfaces the timeout banner and exits `124`

Lane-aware retry rule:

- lifecycle ops (`start` / `stop` / `restart`) are retryable after checking `list` first because retries advance the session generation and stale workers self-abort
- side-effecting ops (`deliver` / `send_input` / `key` / `route`) are **not** automatically retry-safe on timeout; retry only if you have independent proof nothing landed
- read-only ops (`ping` / `list` / `wait_quiet`) can be retried normally

## 4. Calling from Git Bash / MSYS shells

On Windows, callers running under Git Bash / MSYS should treat the wrapped `powershell -Command "& '...\script.ps1' ..."` pattern as canonical.

Why:

- MSYS rewrites leading-slash arguments before PowerShell sees them
- that silently corrupts wrapper commands such as `/fast`, `/review`, or any other leading-slash content
- wrapping the whole PowerShell invocation as one `-Command` string prevents MSYS from splitting and rewriting individual script args

Canonical patterns:

```bash
# control-plane.ps1
powershell -Command "& '.\scripts\control-plane.ps1' -Action list"

# agent-route.ps1
powershell -Command "& '.\scripts\agent-route.ps1' -From claude -To codex -Scope direct -Content 'hello from claude'"

# agent-key.ps1
powershell -Command "& '.\scripts\agent-key.ps1' -Session codex -Key enter"
```

PowerShell-native callers can still use the bare `-File` form, but the wrapped form is the safest cross-shell default on the operator's Windows setup.

## 5. Agent-facing scripts

### `scripts/agent-route.ps1`

Minimal wrapper around `control-plane.ps1` for routed messages.

```bash
powershell -Command "& '.\scripts\agent-route.ps1' -From codex -To claude -Content 'Reply with exactly: CLAUDE ACK'"
```

### `scripts/agent-key.ps1`

Minimal helper for PTY control keys such as Enter, arrows, Tab, Esc, and `Ctrl+C`.

```bash
powershell -Command "& '.\scripts\agent-key.ps1' -Session claude -Key enter"
powershell -Command "& '.\scripts\agent-key.ps1' -Session codex -Key down"
```

### `scripts/agent-slash.ps1`

Helper for slash commands where the command name is stable but the arguments may come from a file.

```bash
powershell -Command "& '.\scripts\agent-slash.ps1' -Session claude -Slash compact -ArgsFile 'D:\tmp\compact-args.txt'"
powershell -Command "& '.\scripts\agent-key.ps1' -Session claude -Key enter"
```

### `scripts/agent-events.ps1`

Outside-supervisor convenience wrapper over `control-plane.ps1 -Action events_since`.

Defaults:

- consumer cursor file: `<runtime-dir>/cursors/outside-supervisor.json`
- kinds: `route_delivery`, `dispatch_attempt`, `session_state`, `session_exit`, `session_work_state`, `supervisor_heartbeat`, `supervisor_alert`, `system_log`, `sideband_request_lifecycle`, `request_ack`, `request_ack_timeout`, `dispatch_no_reaction`
- `-MaxWaitSeconds 15`
- `-MaxEvents 200`

Behavior:

- creates the cursor file with JSON `null` on first run so the first poll starts from "now"
- writes the returned `next_cursor` back to the same file atomically
- prints each returned event as one compressed JSON line
- intentionally excludes terminal output, routed-message content, and legacy task-shaped events

```bash
powershell -Command "& '.\scripts\agent-events.ps1' -Consumer outside-supervisor"
```

## 6. What agent-side terminal sessions can control today

Supervised `claude` and `codex` panes now start from `<workspace-root>/`.

That means:

- repo-root context discovery happens from the configured workspace root
- inside-pane script calls should use absolute wrapper paths such as `<repo-root>/scripts/...` unless the caller first changes directory into the wrapper root

From inside Claude/Codex, an agent can call the helper scripts to:

- list sessions
- start a session
- stop a session
- restart a session
- send raw input to a session
- send raw input from a content file without shell-quoting the payload
- send PTY control keys to a session
- route a direct message
- route a room message

Routed-message delivery shape:

- Claude receives multiline routed input with `[Direct|Room message from <sender>]` headers
- Codex receives flattened single-line routed input, keeps the same provenance header instead of dropping it, and chunks long routed payloads into part-labeled submits to avoid `[Pasted Content N chars]` staging
- long routed messages destined for Claude are split into part-labeled routed inputs to avoid the queued-message truncation found in exploration
- direct-scope routed messages are allowed across panes regardless of pair; room-scope routed messages are pair-scoped by default, with the full contract in `RUNTIME-CONTRACTS.md`

Pane-target policy:

- `send_input` and `send_key` reject a pane-bound credential that targets a different pane
- current behavior is fixed rather than controlled by a global escape hatch:
  - a pane-bound token can control its own pane
  - a pane-bound token cannot inject slash commands or PTY keys into a different pane
  - the rejection message is `session action: pane-bound token cannot target other sessions`
- routed messages remain the explicit pane-to-pane path
- master credentials remain an operator/debug compatibility path
- bearer files are readable by other processes running as the same Windows user, so this target check is not yet a hostile same-user isolation boundary; see `docs/KNOWN_ISSUES.md`

## 7. Verification evidence

- use the live desktop panes for conversation content and visible interaction
- use `<runtime-dir>/audit/` for lifecycle, authorization, dispatch, and delivery metadata
- use `<runtime-dir>/desktop-events.jsonl` only for desktop-process diagnostics
- do not treat `route_delivery`, `request_ack`, `wait_quiet`, or an idle classifier as proof that a model understood or completed work
- content-fidelity tests need an explicit receiver-side oracle, such as a controlled child process or expected artifact

## 8. Current limits

- no OS-dialog control outside the PTY
- no external connector yet for external notification or remote clients

## 9. Pull-based metadata events

`events_since` is the wrapper's pull-based API over the metadata audit. Use it for durable cross-restart consumption and cursor-managed polling, not for transcripts or task completion.

Recommended outside-supervisor pattern:

- raw API: `scripts/control-plane.ps1 -Action events_since ...`
- human/operator wrapper: `scripts/agent-events.ps1`
- cursor file: `<runtime-dir>/cursors/<consumer>.json`

`wait_quiet` remains a synchronous "no real output for N seconds" helper. It and `session_state: idle` are runtime-state hints only; long-think phases may go quiet and idle classification is not a completion signal.

`session_work_state` supplements lifecycle state with semantic pane activity. The supervisor emits it only when a driver's classifier changes state, including quiesce-driven `idle` transitions, so consumers should not expect one event per output chunk. Durable records omit classifier detail.

`dispatch_attempt` is the pre-write audit event for pane-bound dispatches. It lets consumers distinguish safe idle sends from overlap sends before looking for `request_ack` or `route_delivery`.

`dispatch_no_reaction` records that a pane accepted a write but produced no live reaction during the observation window. It is an attention signal, not a content or completion record.

`supervisor_heartbeat` and `supervisor_alert` are wrapper-enforced metadata events. Heartbeats are always on; alerts surface ACK timeouts and optional auto-restart stall decisions.
