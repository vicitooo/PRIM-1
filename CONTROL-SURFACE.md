# PRIM-001 — Control Surface

This file lists the current controls that Victor, Claude, Codex, and future terminal-launched agents can use to operate the wrapper.

## 1. Desktop UI

### Agent panes

- `Launch`
- `Restart`
- `Stop`
- direct terminal typing into Claude or Codex
- direct terminal paste into the focused pane

### Routed message form

- `From`: `Victor`, `Claude`, `Codex`
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
- `key`
- `route`

Canonical examples:

```bash
powershell -Command "& '.\scripts\control-plane.ps1' -Action list"
powershell -Command "& '.\scripts\control-plane.ps1' -Action start -Session claude"
powershell -Command "& '.\scripts\control-plane.ps1' -Action input -Session codex -Content 'hi'"
powershell -Command "& '.\scripts\control-plane.ps1' -Action input -Session claude -ContentFile 'D:\tmp\compact.txt'"
powershell -Command "& '.\scripts\control-plane.ps1' -Action deliver -Session claude -Content 'hello from victor'"
powershell -Command "& '.\scripts\control-plane.ps1' -Action wait_quiet -Session claude -QuietSec 2 -TimeoutSec 10"
powershell -Command "& '.\scripts\control-plane.ps1' -Action key -Session claude -Key enter"
powershell -Command "& '.\scripts\control-plane.ps1' -Action route -From victor -To room -Scope room -Content 'status ping'"
```

Behavior:

- named pipe first
- mailbox fallback second
- default credential resolution is now layered:
  - explicit `-InfoFile` wins
  - otherwise `PRIM1_PANE_CREDENTIALS` is used when present inside a supervised pane
  - otherwise the helper falls back to `.runtime/control-plane.json`
- `-Quiet` prints only the success/error message for agent-friendly use
- `-Action input` injects raw text only; it does **not** press Enter for you
- `-Action input` now accepts either:
  - `-Content '<inline text>'`
  - `-ContentFile '<path to UTF-8 text file>'`
- `-Action deliver` delivers a conversational message with driver-aware preprocessing and Enter submission
- `-Action deliver` accepts either:
  - `-Content '<inline text>'`
  - `-ContentFile '<path to UTF-8 text file>'`
- `-Action wait_quiet` waits for no real session content for `-QuietSec` seconds, up to `-TimeoutSec`
- `wait_quiet` is **not** turn-completion detection; long-think phases may go quiet while the assistant is still in flight
- `-Content` and `-ContentFile` are mutually exclusive
- `-ContentFile` is only supported for `-Action input` or `-Action deliver`
- if you need a typed prompt to execute, follow `-Action input` with `-Action key -Key enter`
- pane-bound credentials now live at:
  - `.runtime/control-plane-claude.json`
  - `.runtime/control-plane-codex.json`
- the master credentials file `.runtime/control-plane.json` remains the operator/debug backdoor

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

# agent-ping.ps1
powershell -Command "& '.\scripts\agent-ping.ps1' -From claude -To codex -Token CODEX_ACK"

# agent-key.ps1
powershell -Command "& '.\scripts\agent-key.ps1' -Session codex -Key enter"
```

PowerShell-native callers can still use the bare `-File` form, but the wrapped form is the safest cross-shell default on Victor's Windows setup.

## 5. Agent-facing scripts

### `scripts/agent-route.ps1`

Minimal wrapper around `control-plane.ps1` for routed messages.

```bash
powershell -Command "& '.\scripts\agent-route.ps1' -From codex -To claude -Content 'Reply with exactly: CLAUDE ACK'"
```

### `scripts/agent-ping.ps1`

Token-based smoke-test helper.

```bash
powershell -Command "& '.\scripts\agent-ping.ps1' -From claude -To codex -Token CODEX_ACK"
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

### `scripts/new-smoke-token.ps1`

Helper that generates a unique handshake token and timestamped smoke-file path.

```bash
powershell -Command "& '.\scripts\new-smoke-token.ps1'"
```

Returns JSON with:

- `token`
- `timestamp_utc`
- `relative_path`
- `absolute_path`

### `scripts/handshake-route.ps1`

Canonical handshake message builder/router. Use it instead of hand-writing the route strings.

```bash
powershell -Command "& '.\scripts\handshake-route.ps1' -Actor claude -Action start -Token 'SMOKE-1A2B3C4D' -Path '.\.runtime\smoke\handshake-20260415T120000Z-SMOKE-1A2B3C4D.txt'"
powershell -Command "& '.\scripts\handshake-route.ps1' -Actor claude -Action dispatch -Token 'SMOKE-1A2B3C4D' -Path '.\.runtime\smoke\handshake-20260415T120000Z-SMOKE-1A2B3C4D.txt'"
powershell -Command "& '.\scripts\handshake-route.ps1' -Actor codex -Action ready -Token 'SMOKE-1A2B3C4D'"
```

Supported actions:

- Claude:
  - `start`
  - `dispatch`
  - `pass`
  - `fail`
- Codex:
  - `ready`
  - `status`
  - `fail`

`-DryRun` prints the exact routed payload JSON without sending it.

### `scripts/handshake-watchdog.ps1`

External handshake timeout watcher. It polls the audit log for a terminal marker and routes a canonical Claude-side timeout FAIL if no `FILE_READY`, `HANDSHAKE PASS`, or `HANDSHAKE FAIL` appears before the deadline.

```bash
powershell -Command "& '.\scripts\handshake-watchdog.ps1' -Token 'SMOKE-1A2B3C4D'"
```

## 6. What agent-side terminal sessions can control today

Supervised `claude` and `codex` panes now start from `<workspace>/`.

That means:

- repo-root context discovery happens from the personal repo root
- inside-pane script calls should use absolute wrapper paths such as `./scripts/...` unless the caller first changes directory into the wrapper root

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
- ping another agent with a tokenized smoke test
- generate unique handshake tokens and timestamped smoke paths
- route canonical handshake messages without hand-written preambles
- run an external handshake timeout watchdog

Routed-message delivery shape:

- Claude receives multiline routed input with `[Direct|Room message from <sender>]` headers
- Codex receives flattened single-line routed input, but it now also keeps the same provenance header instead of dropping it
- long routed messages destined for Claude are split into part-labeled routed inputs to avoid the queued-message truncation found in exploration

Peer slash-command policy:

- `send_input` and `send_key` now honor a supervisor policy boundary when called with pane-bound credentials
- default posture is locked down:
  - a pane-bound token can control its own pane
  - a pane-bound token cannot inject slash commands or PTY keys into a different pane
  - the rejection message is `peer slash commands are disabled by this wrapper's policy (PRIM1_PEER_SLASH_COMMANDS_ALLOWED=0)`
- routed messages are still allowed across panes; the lockdown only covers `send_input` / `send_key`
- operators using the master credentials bypass the lockdown unconditionally
- the gate is reversible at wrapper startup with `PRIM1_PEER_SLASH_COMMANDS_ALLOWED=1`

## 7. Autonomous validation loop

Current rule for proving behavior:

- use runtime evidence from `.runtime/audit/` or `.runtime/desktop-events.jsonl`
- use UI evidence from the live window or a screenshot capture
- only mark the interaction as working when both sources agree

## 8. Python watcher

Watcher entrypoints:

- `tools/prim1-command-watcher.py`
- `tools/prim1_command_watcher.py`
- config: `tools/prim1-command-watcher.config.json`

Run:

```bash
python tools/prim1-command-watcher.py
```

Behavior:

- tails `.runtime/audit/YYYY-MM-DD.jsonl` from EOF
- watches each session independently
- auto-arms only on prompt-echo slash-command lines such as `/fast` or `› /fast`
- ignores static slash hints embedded in banners such as Codex's `/model to change`
- dispatches the continue signal through `scripts/control-plane.ps1`
- logs runtime events to `.runtime/watcher.log`
- writes alerts to `.runtime/watcher-alerts/`
- enforces:
  - per-session minimum dispatch interval
  - per-session daily dispatch limit
  - assistant-turn timeout after a continue dispatch

Current verified path:

- use Codex `/fast` for Codex-side verification; Codex does not expose a `/help` command
- use Claude `/help` for Claude-side slash-command verification
- Codex `/fast` completion is detected
- watcher dispatches `Command complete. Continue with the task.`
- Codex produces the follow-up assistant turn
- the watcher stays quiet during the normal Claude/Codex handshake flow

For desktop-only visuals, the current screenshot capture path is:

- `<workspace>/tools/screenshot/screenshot.py`

## 9. Current limits

- no OS-dialog control outside the PTY
- no external connector yet for Telegram or remote clients
