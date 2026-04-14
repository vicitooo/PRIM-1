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

Example:

```powershell
.\scripts\control-plane.ps1 -Action route -From victor -To claude -Scope direct -Content "Review the Codex pane."
```
