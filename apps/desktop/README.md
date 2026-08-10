# Desktop App

This app is the first real UI shell for `PRIM-1`.

## Stack

- Tauri v2
- Vanilla TypeScript
- xterm.js

## What it currently does

- renders a backend-ordered, `SessionId`-keyed tab set for Claude Code, Codex,
  and Generic Terminal sessions
- retains inactive xterm buffers while showing one active terminal
- renders a system log pane
- exposes native workspace/cwd pickers, visible permission profiles, and
  create/rename/reorder/closed-only-delete controls
- exposes start / restart / stop controls for each session
- intentionally exposes no room or message composer before `RoomId` membership
- listens to the supervisor event bus in real time
- writes process diagnostics to `<runtime-dir>/desktop-events.jsonl`; this file is not a terminal transcript or message-content record

## Run

```powershell
cd apps/desktop
npm install
npm run tauri dev
```

## Build

```powershell
cd apps/desktop
npm run tauri build
```
