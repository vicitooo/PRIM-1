# Desktop App

This app is the first real UI shell for `CLI-master-wrapper`.

## Stack

- Tauri v2
- Vanilla TypeScript
- xterm.js

## What it currently does

- renders supervised Claude and Codex panes
- renders a system log pane
- lets Victor route direct or room messages
- exposes start / restart / stop controls for each session
- listens to the supervisor event bus in real time

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
