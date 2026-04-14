# CLI-master-wrapper

**Status:** First MVP implemented
**Date:** 2026-04-14

## Current MVP

The project is no longer docs-only. The first version now exists and includes:

- a Rust workspace with separate supervisor, PTY, driver, control-plane, and shared-types crates
- a Tauri desktop shell in `apps/desktop`
- xterm.js panes for Claude, Codex, and the system log
- a named-pipe control plane on Windows
- JSONL audit logging under `.runtime/audit/`
- a PowerShell helper at `scripts/control-plane.ps1`
- an explicit control-surface reference at `CONTROL-SURFACE.md`
- release outputs under `target/release/`

Current verified commands:

- `cargo check`
- `cargo test`
- `cd apps/desktop && npm run build`
- `cd apps/desktop && npm run tauri build`

Current verified operator shortcuts:

- `Ctrl+Shift+C` copies the current terminal selection in the focused pane
- `Ctrl+Shift+V` pastes clipboard text into the focused running pane
- `Alt+V` remains reserved for screenshots

Known limitation in this shell-hosted environment:

- the release GUI binary created the control-plane file but did not stay alive in the non-interactive smoke check, so full visual validation still needs a normal desktop session

## What this is

`CLI-master-wrapper` is a local multi-agent runtime for terminal-first AI tools.

The target experience is:

- Victor always in the visibility layer
- Claude running as a supervised CLI process
- Codex running as a supervised CLI process
- future agents such as Hermes, OpenClaw, and other terminal apps
- all parties able to communicate in real time

This is not a file-bridge patch.
This is a proper runtime with:

- supervisor-owned PTYs
- a visible terminal UI
- a minimal sideband control plane
- explicit lifecycle and restart policy
- support for both direct messages and a shared room

## Chosen stack

The implementation direction is now locked:

- **Rust** for the supervisor, PTY ownership, lifecycle, routing, and security-sensitive runtime code
- **Tauri** for the desktop shell
- **xterm.js** for terminal rendering inside the UI
- **named pipes on Windows / Unix sockets on POSIX** for the local control plane

This avoids building a terminal renderer from scratch and gives the project a production-grade desktop shape from the start.

## The core idea

Three different concerns need to stay separate:

1. **Visibility layer**
   Real PTYs owned by the supervisor. Victor sees live terminal output exactly as it is happening.

2. **Control layer**
   A local-only API or script interface for structured actions:
   send message, spawn, restart, close, ping, health-check.

3. **Policy layer**
   The supervisor decides what actions are allowed, when processes are restarted, when sessions are closed, and what sandbox/security rules apply.

## Why this exists

Current limitations:

- Claude and Codex can each talk to Victor in separate terminals, but not participate in one shared real-time room cleanly.
- Current bridge tooling is useful, but it is not a generic runtime and it does not own terminal sessions end to end.
- PID-only control is not enough. The system needs ownership of stdin/stdout/PTY and explicit lifecycle rules.
- This current UI session cannot be woken externally; local supervised CLI processes can.

## Target experience

The first user-facing version should give Victor:

- left pane: Claude
- right pane: Codex
- bottom pane: system log
- Victor input/router
- a small retractable control surface for session actions

Supported communication modes:

- Victor -> Claude
- Victor -> Codex
- Claude -> Codex
- Codex -> Claude
- Victor -> room
- Claude -> room
- Codex -> room

All routed messages should be visibly stamped in the target terminal and logged by the supervisor.

## First real use case

The first target is not an overnight automation system.

It is a **live three-way coding room** where:

- Victor speaks to Claude
- Victor speaks to Codex
- Claude speaks to Codex
- Codex speaks to Claude
- room messages are visible to everyone

The later 24/7 orchestration use cases are built on top of that, not before it.

## Design rules

1. The supervisor is the only entity allowed to spawn, kill, restart, and reattach agents.
2. Agents do not directly restart or kill each other.
3. PTY is the visibility layer. Sideband API is the control layer.
4. Explicit API/script calls are the primary command channel. stdout parsing is secondary.
5. Idle and stuck are different states and must be handled differently.
6. The runtime must be generic across terminal-first CLIs, not hardcoded to Claude/Codex.
7. The first milestone is the real-time room, not autonomous orchestration.

## Canonical docs

- [ARCHITECTURE.md](ARCHITECTURE.md)
  Full system shape: supervisor, PTY host, drivers, bus, UI, lifecycle, security.

- [IMPLEMENTATION-PLAN.md](IMPLEMENTATION-PLAN.md)
  Build phases, milestones, first slices, and open questions.

- [STACK-DECISIONS.md](STACK-DECISIONS.md)
  Locked decisions, v1 scope cuts, unresolved items, and decision guardrails.

- [RUNTIME-CONTRACTS.md](RUNTIME-CONTRACTS.md)
  Message envelopes, routing rules, lifecycle semantics, permissions, audit schema.

- [PHASE-0-RESEARCH.md](PHASE-0-RESEARCH.md)
  The exact research/prototype checklist before implementation starts.

- [TASK-BREAKDOWN.md](TASK-BREAKDOWN.md)
  Work packages, dependencies, phase budgets, and what gets built in what order.

- [PROJECT-STRUCTURE.md](PROJECT-STRUCTURE.md)
  File/folder layout and the role of each crate/app/module.

## Relationship to existing bridge code

The existing `tools/agent-bridge/agent_bridge.py` is the main starting point, not throwaway code.

What it already gives:

- subprocess spawning
- session registry
- watcher pattern
- Codex driver logic
- partial Claude driver logic
- archive and dispatch patterns

What this project changes:

- replaces file-only bridging with a supervisor/runtime model
- replaces kill-on-DONE with lifecycle policy
- replaces hardcoded Claude/Codex branching with generic drivers
- adds owned PTYs and a visible room

## Migration stance

The existing bridge is not being ripped out immediately.

For v1:

- the bridge remains usable for current workflows
- `CLI-master-wrapper` is built in parallel
- migration decisions happen only after the room MVP is proven

## Non-goals for v1

- Telegram integration
- remote operator mode
- cloud multi-machine orchestration
- polished animation-heavy UI
- full autonomous overnight workforce

Those can come later. The first proof is local, visible, stable three-way communication.

## Workspace

- `apps/desktop/`
- `crates/supervisor/`
- `crates/pty-host/`
- `crates/driver-claude/`
- `crates/driver-codex/`
- `crates/driver-generic-terminal/`
- `crates/control-plane/`
- `crates/shared-types/`
- `scripts/`
- `tests/`

These are now real project directories, not placeholders.
