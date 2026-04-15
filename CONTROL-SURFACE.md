# CLI Master Wrapper — Control Surface

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

These shortcuts are for the focused terminal pane.

- `Ctrl+Shift+C`
  - copies the current xterm selection
  - implementation is in place, but live desktop re-validation is still pending
- `Ctrl+Shift+V`
  - pastes clipboard text into the focused running pane
- `Alt+V`
  - preserved as the screenshot shortcut

Notes:

- `Ctrl+C` is still passed through to the terminal session itself
- the wrapper only intercepts `Ctrl+Shift+C` / `Ctrl+Shift+V` when a terminal pane owns focus

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
- `key`
- `route`

Examples:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\control-plane.ps1 -Action list
powershell -ExecutionPolicy Bypass -File .\scripts\control-plane.ps1 -Action start -Session claude
powershell -ExecutionPolicy Bypass -File .\scripts\control-plane.ps1 -Action input -Session codex -Content "hi"
powershell -ExecutionPolicy Bypass -File .\scripts\control-plane.ps1 -Action key -Session claude -Key enter
powershell -ExecutionPolicy Bypass -File .\scripts\control-plane.ps1 -Action route -From victor -To room -Scope room -Content "status ping"
```

Behavior:

- named pipe first
- mailbox fallback second
- `-Quiet` prints only the success/error message for agent-friendly use

## 4. Agent-facing scripts

### `scripts/agent-route.ps1`

Minimal wrapper around `control-plane.ps1` for routed messages.

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\agent-route.ps1 -From codex -To claude -Content "Reply with exactly: CLAUDE ACK"
```

### `scripts/agent-ping.ps1`

Token-based smoke-test helper.

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\agent-ping.ps1 -From claude -To codex -Token CODEX_ACK
```

### `scripts/agent-key.ps1`

Minimal helper for PTY control keys such as Enter, arrows, Tab, Esc, and `Ctrl+C`.

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\agent-key.ps1 -Session claude -Key enter
powershell -ExecutionPolicy Bypass -File .\scripts\agent-key.ps1 -Session codex -Key down
```

## 5. What agent-side terminal sessions can control today

From inside Claude/Codex, an agent can call the helper scripts to:

- list sessions
- start a session
- stop a session
- restart a session
- send raw input to a session
- send PTY control keys to a session
- route a direct message
- route a room message
- ping another agent with a tokenized smoke test

## 6. Autonomous validation loop

Current rule for proving behavior:

- use runtime evidence from `.runtime/audit/` or `.runtime/desktop-events.jsonl`
- use UI evidence from the live window or a screenshot capture
- only mark the interaction as working when both sources agree

For desktop-only visuals, the current screenshot capture path is:

- `<workspace>/tools/screenshot/screenshot.py`

## 7. Current limits

- no OS-dialog control outside the PTY
- no external connector yet for Telegram or remote clients
