# CLI-master-wrapper — Phase 0 Notes

**Status:** MVP complete, regression coverage and crash hardening in progress
**Date:** 2026-04-15

## 1. PTY backend result

- Chosen backend: `portable-pty`
- Outcome: `cargo check` and `cargo test` pass on Windows with the PTY host crate in place.
- Current usage: the supervisor owns PTYs for launched sessions and streams output into the app event bus.

## 2. Tauri + xterm.js result

- Chosen stack: Tauri v2 + vanilla TypeScript + `@xterm/xterm` + `@xterm/addon-fit`
- Outcome: `npm run build` and `npm run tauri build` both pass.
- UI now renders two session panes, a system pane, routed-message form, and a minimal floating control surface.

## 3. Claude launch/resume result

- Driver shape is implemented for interactive Claude CLI launch.
- The first version launches `claude -n <session> --add-dir <working_dir>` inside a supervised PTY.
- Resume semantics are not implemented yet. This is still a next slice, not part of the MVP.

## 4. Codex launch/resume result

- Driver shape is implemented for interactive Codex CLI launch.
- The first version launches `codex --no-alt-screen -C <working_dir>` inside a supervised PTY.
- Resume semantics are not implemented yet. This is also a follow-up slice.

## 5. Local IPC send-path result

- Chosen transport: Windows named pipe with a generated endpoint written to `.runtime/control-plane.json`
- Outcome: the control-plane file is created on startup and a PowerShell helper now exists at `scripts/control-plane.ps1`
- Supported actions in the first version:
  - `ping`
  - `list`
  - `start`
  - `stop`
  - `restart`
  - `input`
  - `route`

## 6. Audit log decision

- Format: JSONL
- Location: `.runtime/audit/YYYY-MM-DD.jsonl`
- Source of truth: emitted runtime events

## 7. Test strategy decision

- Unit tests for shared types, control-plane codec, and supervisor routing/state basics
- Added regression coverage for the known-good runtime contracts:
  - Claude and Codex launch specs
  - control-plane request/response round-trips
  - control-plane persistence, idempotence, and token validation
  - supervisor refusal to route to non-running recipients
- Full workspace verification via:
  - `cargo check`
  - `cargo test`
  - `npm run build`
  - `npm run tauri build`
- Manual visual checkpoint has now been reached through `npm run tauri dev`: the desktop window opened and both Claude and Codex reached `ready`.
- The raw packaged `.exe` path still needs a dedicated release-path investigation because it has shown inconsistent startup behavior versus the dev launcher.

## 8. Fallback decision

- Architectural fallback remains:
  - Windows primary = ConPTY via `portable-pty`
  - fallback = WSL2/POSIX PTY if a required CLI flow proves unstable
- No fallback implementation was needed yet because the first compile/build cut succeeded.

## 9. Unresolved blockers

- Release-path startup is still inconsistent compared with the dev launcher.
- Resume/reattach semantics for Claude and Codex are not implemented yet.
- The current UI only covers the main room workflow, not settings/main menu depth.
- Routed-message submit is improved but not fully hardened:
  - Claude and Codex both accept routed prompts now.
  - Codex needed a driver-specific single-line payload plus delayed submit.
  - Claude can still occasionally hold a landed prompt until another Enter arrives.
- Next reliability slice should be a readiness/acknowledgement gate instead of more blind submit timing.
- New hardening priority after live testing:
  - preserve the currently working room flows with regression tests before more runtime edits
  - add explicit crash/restart diagnostics so app exits are recorded with a reason
  - fix the restart path seen when Codex attempts to send a routed message to Claude from inside its pane

## 10. Go / no-go recommendation

- **Go**
- The project is past planning. There is now a real first version worth iterating on.
