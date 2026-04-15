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
- `key`
- `route`

Canonical examples:

```bash
powershell -Command "& '.\scripts\control-plane.ps1' -Action list"
powershell -Command "& '.\scripts\control-plane.ps1' -Action start -Session claude"
powershell -Command "& '.\scripts\control-plane.ps1' -Action input -Session codex -Content 'hi'"
powershell -Command "& '.\scripts\control-plane.ps1' -Action key -Session claude -Key enter"
powershell -Command "& '.\scripts\control-plane.ps1' -Action route -From victor -To room -Scope room -Content 'status ping'"
```

Behavior:

- named pipe first
- mailbox fallback second
- `-Quiet` prints only the success/error message for agent-friendly use
- `-Action input` injects raw text only; it does **not** press Enter for you
- if you need a typed prompt to execute, follow `-Action input` with `-Action key -Key enter`

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

## 6. What agent-side terminal sessions can control today

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

## 7. Autonomous validation loop

Current rule for proving behavior:

- use runtime evidence from `.runtime/audit/` or `.runtime/desktop-events.jsonl`
- use UI evidence from the live window or a screenshot capture
- only mark the interaction as working when both sources agree

For desktop-only visuals, the current screenshot capture path is:

- `<workspace>/tools/screenshot/screenshot.py`

## 8. Current limits

- no OS-dialog control outside the PTY
- no external connector yet for Telegram or remote clients
