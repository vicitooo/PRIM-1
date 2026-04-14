# CLI-master-wrapper — Implementation Plan

**Status:** Canonical execution plan
**Date:** 2026-04-14

## Primary objective

Prove a stable local real-time room with:

- Victor
- Claude
- Codex

All visible.
All supervised.
All routable in real time.

Tech direction for this implementation:

- **Rust** for the supervisor/runtime
- **Tauri** for the desktop shell
- **xterm.js** for PTY rendering
- **named pipes / Unix sockets** for the control plane

## Success criteria for v1

The architecture is considered proven when the following work end to end:

1. supervisor launches Claude in a supervisor-owned PTY
2. supervisor launches Codex in a supervisor-owned PTY
3. UI renders both live streams
4. Victor can send a message to either agent
5. one agent can send a sideband direct message to the other
6. room broadcast is visible to all participants
7. supervisor can restart either agent
8. all actions are logged

If those eight work reliably, the foundation is real.

## Phase 0 — Research and validation

Purpose:
confirm the substrate before building the full runtime.

Tasks:

- verify Windows ConPTY crate/library choice in Rust
- verify Tauri + xterm.js integration path
- verify Claude launch, resume, and stable streamed output
- verify Codex launch, resume, and stable streamed output
- verify PTY input injection works cleanly for both
- verify local named-pipe / socket routing works
- verify visible output capture is good enough for rendering and logging
- validate Claude resume semantics across restart, not just launch
- lock the v1 heartbeat strategy
- lock the audit log format and location
- lock the test strategy
- document the fallback if native ConPTY is not good enough
- document any CLI-specific quirks early

Deliverables:

- `phase-0-notes.md`
- minimal PTY proof for Claude
- minimal PTY proof for Codex
- one local send-path proof
- explicit renderer/UI choice recorded
- explicit fallback path recorded
- explicit test strategy recorded

Rule:

- **Phase 0 code is exploratory and disposable.** Do not graft prototypes directly into Phase 1.

Rough budget:

- **2-3 focused days**

## Phase 1 — Supervisor core

Purpose:
get lifecycle, routing, and process ownership right before UI complexity.

Tasks:

- build process registry
- build session registry
- define the driver abstraction now, not later
- implement `claude_cli` and `codex_cli` as the first two real drivers
- implement base lifecycle state machine
- implement restart backoff
- add JSONL audit log with fixed event schema
- expose local-only control API over named pipe / Unix socket
- add per-agent working-directory whitelist
- add per-agent sideband capability whitelist
- add per-session token/cost tracking hook
- define supervisor service wrapper plan (`nssm` / `systemd`)

Deliverables:

- supervisor can start Claude
- supervisor can start Codex
- supervisor can stop/restart either
- every action is logged
- early safety controls are enforced

Rough budget:

- **3-5 focused days**

## Phase 2 — Room MVP

Purpose:
ship the first usable operator experience.

Tasks:

- render Claude PTY via xterm.js
- render Codex PTY via xterm.js
- render system log
- build Victor input/router
- support direct messaging
- support room broadcast
- support UI reconnect to live supervisor state
- reserve space for the future retractable control surface

Deliverables:

- Victor -> Claude
- Victor -> Codex
- Claude -> Codex
- Codex -> Claude
- room messages visible to all

Rough budget:

- **3-7 focused days**, depending on PTY rendering friction

## Phase 3 — Generic wrapper model

Purpose:
validate that the Phase 1 abstraction is actually generic.

Tasks:

- build manifest/config for terminal apps
- add capability metadata per driver
- build generic terminal fallback driver
- make health/shutdown strategies pluggable
- validate multi-instance naming and routing (`claude:work`, `claude:research`, etc.)

Deliverable:

- adding a new terminal app does not require core supervisor changes

Rough budget:

- **2-3 focused days**

## Phase 4 — Minimal control surface

Purpose:
add operator controls without bloating the room.

Tasks:

- retractable control surface
- reconnect
- restart
- close
- health status
- main menu entry point

Deliverable:

- low-noise operator controls layered on top of the room

Rough budget:

- **1-2 focused days**

## Phase 5 — Hardening

Purpose:
make the runtime safe and durable enough for sustained use.

Tasks:

- refine heartbeat/liveness strategy beyond passive output checks
- better stalled-session heuristics
- output throttling
- crash recovery
- transcript persistence
- transcript replay
- stale session cleanup
- deeper permissions and sandbox policy integration
- external sandbox path for long-running Claude if needed

Rough budget:

- **about 1 week**

## Phase 6 — External connectors

Purpose:
add outside-world communication after local runtime is stable.

Tasks:

- Telegram
- webhooks
- remote operator mode
- scheduled jobs

These are intentionally deferred.

## Immediate implementation order

Build in this order:

1. ConPTY proof
2. Tauri + xterm.js proof
3. Claude driver
4. Codex driver
5. supervisor core
6. local send API/script
7. room UI
8. restart and health
9. generic app support
10. polish

## Technical debt to carry forward from the existing bridge

The current bridge already exposed several failure modes that should be addressed during the refactor:

- non-zero exit should fail loudly, not silently retry-loop
- lifecycle should be inactivity/stall driven, not kill-on-DONE
- session ownership should be explicit
- transport should not depend only on file inboxes
- Claude/Codex-specific logic should move behind drivers

These should be treated as migration requirements, not optional cleanup.

## Testing strategy

This cannot be left implicit.

V1 testing approach:

- unit tests for lifecycle, routing, registry, and policy logic
- integration tests using simple terminal subjects instead of real Claude/Codex where possible
- opt-in manual or gated tests for real Claude/Codex sessions because they consume budget
- explicit stall/restart tests using synthetic child processes, not long real waits

The test strategy must be locked during Phase 0 before Phase 1 code starts to accumulate.

## First real use case

The first real target after the room MVP is:

- **live three-way coding**

Meaning:

- Victor talks to Claude and Codex in one room
- Claude and Codex can route messages to each other through the supervisor
- Victor sees the interaction in real time

This is the primary shaping use case for the first build.

## Migration plan from the existing bridge

V1 is parallel, not a forced cutover.

- existing bridge stays usable during development
- no current workflow is broken just to start this project
- migration decisions happen only after the room MVP is proven
- compatibility shims are out of scope unless a real migration need appears

## Open questions

1. Which ConPTY binding is the most stable fit?
2. Does Claude Code itself expose any usable external event-stream hook?
3. What budget ceiling should the runtime respect for continuous operation?
4. Does Victor want internal-only tooling first, or should product-track constraints shape naming and structure now?

## Recommended file layout

```text
CLI-master-wrapper/
  README.md
  ARCHITECTURE.md
  IMPLEMENTATION-PLAN.md
  phase-0-notes.md
  supervisor/
  drivers/
  pty/
  api/
  ui/
  scripts/
```

## What not to do

- do not build on top of random terminal windows you do not own
- do not use PID as the main control primitive
- do not make direct peer restart authority symmetrical
- do not rely on stdout regex for authoritative routing
- do not start with external connectors before the local room works
- do not optimize UI polish before PTY ownership and routing are stable
- do not start Phase 1 until the renderer choice, resume semantics, and test strategy are locked in Phase 0
