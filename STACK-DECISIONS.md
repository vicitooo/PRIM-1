# PRIM-1 — Stack Decisions

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
- an ordered set of explicit terminal sessions
- one active terminal surface with retained inactive buffers
- zero or more explicit rooms, with at most one active room membership per session
- one supervisor in charge

## 5. V1 scope lock

V1 includes:

- repeated supervised Claude, Codex, Grok, Prime, or Generic Terminal sessions
- stable `SessionId` / `RunId` / `RoomId` authority
- feed-only room posts
- explicit one-member or Send All room delivery
- supervisor-controlled restart/close
- metadata-only audit log
- basic health/liveness

V1 excludes:

- external notification
- remote operator mode
- multi-machine federation
- agent trees / sub-agent orchestration
- production-grade external sandboxing for Claude
- support for every future CLI on day one

## 6. First use case lock

The first real use case is:

- **Victor's local multi-harness daily-driver environment**

That means:

- the operator works in one or more faithful native harness terminals
- selected sessions join explicit rooms without exposing private terminal history
- feed posts and recipient delivery are distinct visible actions
- all room traffic and delivery state is visible in real time

This is the first product shape to optimize for.

## 7. Security decisions for v1

Mandatory in early implementation:

- backend-qualified, identity-revalidated working-directory selection
- per-agent capability whitelist
- IPC access control
- supervisor-only lifecycle authority

Deferred:

- strong external sandboxing for long-running Claude
- remote auth
- multi-user permissions

## 8. External collaboration boundary

Agent Bus remains an independent advisory/collaboration tool. It is not embedded
as PRIM-1 transport and cannot substitute for native room, PTY, or receiver
evidence. PRIM-1 sessions use only the supervisor-owned runtime paths.

## 9. Open decisions that remain

These remain later product decisions rather than implementation defaults:

1. supported logical resume semantics across desktop restart
2. cost budget ceiling and surfaced telemetry
3. user-extensible driver packaging and trust model
4. richer simultaneous-pane layouts
5. native Linux/macOS product support

## 10. Decision guardrail

Do not change the locked decisions above casually.

A decision should only be changed if:

- Phase 0 proves it unworkable, or
- the replacement clearly reduces total project risk and rewrite cost
