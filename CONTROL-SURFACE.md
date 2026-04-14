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
- `route`

Examples:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\control-plane.ps1 -Action list
powershell -ExecutionPolicy Bypass -File .\scripts\control-plane.ps1 -Action start -Session claude
powershell -ExecutionPolicy Bypass -File .\scripts\control-plane.ps1 -Action input -Session codex -Content "hi"
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

## 5. What agent-side terminal sessions can control today

From inside Claude/Codex, an agent can call the helper scripts to:

- list sessions
- start a session
- stop a session
- restart a session
- send raw input to a session
- route a direct message
- route a room message
- ping another agent with a tokenized smoke test

## 6. Current limits

- no dedicated sideband action yet for arrow keys / tab / esc / ctrl+c
- no OS-dialog control outside the PTY
- no external connector yet for Telegram or remote clients
