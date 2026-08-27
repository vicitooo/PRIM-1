# Desktop App

This app is the first real UI shell for `PRIM-1`.

## Stack

- Tauri v2
- Vanilla TypeScript
- xterm.js

## What it currently does

- renders a backend-ordered, `SessionId`-keyed tab set for Claude Code, Codex,
  Grok, Prime Agent (Ubuntu WSL), and Generic Terminal sessions
- retains inactive xterm buffers while showing one active terminal
- renders a system log pane
- exposes native Windows workspace/cwd pickers, a backend-qualified Ubuntu cwd
  field for Prime, visible permission profiles, and create/rename/reorder/closed-only-delete controls
- exposes start / restart / stop controls for each session
- exposes ordered `RoomId` create/rename/reorder/membership/delete controls, one
  bounded active-room feed, feed-only Post, and explicit one-member / Send All
  delivery
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
