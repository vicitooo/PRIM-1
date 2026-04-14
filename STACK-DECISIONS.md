# CLI-master-wrapper — Stack Decisions

**Status:** Locked unless explicitly changed
**Date:** 2026-04-14

## 1. Core stack

The implementation direction for v1 is:

- **Rust** for supervisor/runtime/process ownership
- **Tauri** for desktop packaging and app shell
- **xterm.js** for terminal rendering
- **named pipes on Windows / Unix sockets on POSIX** for the local control plane

## 2. Why this stack

### Rust

Use Rust for:

- process management
- PTY ownership
- lifecycle state machine
- restart/backoff logic
- control plane
- cost telemetry hooks
- audit logging

Reason:

- long-lived local runtime
- better fit than Python for a durable Tauri-backed desktop system
- avoids a probable rewrite after MVP

### Tauri

Use Tauri for:

- desktop shell
- window management
- local app packaging
- secure bridge between UI and Rust backend

Reason:

- lighter than Electron
- natural fit with a Rust backend
- supports the product shape directly

### xterm.js

Use xterm.js for:

- ANSI rendering
- cursor movement
- colors
- scrollback
- alternate screen behavior
- resize handling

Reason:

- avoid building a terminal renderer
- battle-tested
- fits naturally inside Tauri webview

### Named pipes / Unix sockets

Use local IPC by default, not localhost TCP.

Reason:

- lower attack surface
- local-only by default
- avoids accidental browser access
- better fit for supervisor-local communication

## 3. Platform strategy

### V1 platform

- **Windows first**

Primary PTY backend:

- **ConPTY**

Fallback if ConPTY is not good enough for a required flow:

- **WSL2 + POSIX PTY backend**, while keeping the same supervisor and UI architecture

## 4. Product shape

V1 is a **local desktop app/runtime**, not:

- a hosted SaaS
- a shell-only tool
- a browser-only dashboard
- a remote-control service

V1 should feel like:

- one local app
- multiple live terminal panes
- one shared room
- one supervisor in charge

## 5. V1 scope lock

V1 includes:

- one supervised Claude instance
- one supervised Codex instance
- Victor in the room
- direct messages
- room broadcast
- supervisor-controlled restart/close
- audit log
- basic health/liveness

V1 excludes:

- Telegram
- remote operator mode
- multi-machine federation
- agent trees / sub-agent orchestration
- production-grade external sandboxing for Claude
- support for every future CLI on day one

## 6. First use case lock

The first real use case is:

- **live three-way coding**

That means:

- Victor talks to Claude and Codex in the same app
- Claude and Codex can message each other through the supervisor
- all communication is visible in real time

This is the first product shape to optimize for.

## 7. Security decisions for v1

Mandatory in early implementation:

- per-agent working-directory whitelist
- per-agent capability whitelist
- IPC access control
- supervisor-only lifecycle authority

Deferred:

- strong external sandboxing for long-running Claude
- remote auth
- multi-user permissions

## 8. Migration decision

The existing bridge is not replaced on day one.

Plan:

- keep the existing bridge operational
- build `CLI-master-wrapper` in parallel
- only decide migration/cutover after the room MVP works

## 9. Open decisions that remain

These are still open and must be closed in Phase 0:

1. exact Rust PTY/ConPTY library choice
2. exact Rust-side named pipe / Unix socket library choice
3. exact Claude resume semantics under supervisor restart
4. cost budget ceiling
5. whether current interactive Claude Code has any usable external event-stream hook

## 10. Decision guardrail

Do not change the locked decisions above casually.

A decision should only be changed if:

- Phase 0 proves it unworkable, or
- the replacement clearly reduces total project risk and rewrite cost

