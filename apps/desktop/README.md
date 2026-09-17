# Desktop App

The Windows Tauri desktop for PRIM-1. Start with the [first-room quickstart](../../README.md#your-first-room) for normal use.

## Stack

- Tauri v2
- Vanilla TypeScript
- xterm.js

## What it currently does

- renders a backend-ordered, `SessionId`-keyed tab set for Claude Code, Codex,
  Grok, Prime Agent (Ubuntu WSL), and Generic Terminal sessions
- retains inactive xterm buffers while showing one active terminal
- groups sessions into rooms and an unassigned lobby; changing context leaves sessions running
- renders a system log pane
- exposes native Windows workspace/cwd pickers, a backend-qualified Ubuntu cwd
  field for Prime, visible permission profiles, and create/rename/reorder/closed-only-delete controls
- exposes start / restart / stop controls for each session
- exposes ordered `RoomId` create/rename/reorder/membership/delete controls, one
  bounded active-room feed, feed-only Post, and explicit one-member / Send All
  delivery
- listens to the supervisor event bus in real time
- optionally relaunches previously running sessions on context entry and resumes stored harness conversations
- writes process diagnostics to `<runtime-dir>/desktop-events.jsonl`; errors may include harness text, so inspect before sharing

## Development

```powershell
cd apps/desktop
npm ci
npm run tauri dev
```

## Production build

```powershell
cd apps/desktop
npm ci
npm run build
npm run tauri build
```

The executable is `target/release/cli-master-wrapper-desktop.exe` relative to
the repository root. Plain Cargo builds without Tauri's production protocol can
still point at the development server; use the command above for normal use.
