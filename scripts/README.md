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
  - `route`
  - optional `-Quiet` mode for agent-friendly success/error output

Example:

```powershell
.\scripts\control-plane.ps1 -Action route -From victor -To claude -Scope direct -Content "Review the Codex pane."
```

- `agent-route.ps1`
  Minimal agent-facing wrapper over `control-plane.ps1` for direct or room messages without the verbose JSON snapshot payload.

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
