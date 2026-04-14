# CLI-master-wrapper — Task Breakdown

**Status:** Detailed work packages
**Date:** 2026-04-14

## WP0 — Research spikes

Scope:

- PTY spike
- xterm.js spike
- Claude spike
- Codex spike
- IPC spike
- test strategy
- fallback decision

Output:

- `phase-0-notes.md`

Dependency:

- none

## WP1 — Shared types and contracts

Scope:

- session identifiers
- lifecycle enums
- message envelope structs
- audit event structs
- permission config types

Why early:

- everything else depends on shared contracts

Dependency:

- WP0 decisions locked

## WP2 — PTY host

Scope:

- PTY creation
- child spawn in PTY
- read stream
- write stream
- resize
- close

Dependency:

- WP0 PTY decision
- WP1 shared types

## WP3 — Control plane

Scope:

- named pipe / Unix socket server
- send command
- request validation
- routing into supervisor

Dependency:

- WP1 shared types

## WP4 — Supervisor core

Scope:

- session registry
- process registry
- lifecycle transitions
- restart backoff
- routing logic
- audit log write path

Dependency:

- WP1
- WP2
- WP3

## WP5 — Claude driver

Scope:

- launch
- resume
- output parsing hooks
- interrupt/close behavior
- session metadata capture

Dependency:

- WP2
- WP4

## WP6 — Codex driver

Scope:

- launch
- resume
- output parsing hooks
- interrupt/close behavior
- session metadata capture

Dependency:

- WP2
- WP4

## WP7 — UI shell

Scope:

- Tauri shell
- xterm.js panes
- system log view
- Victor input/router
- reconnect behavior

Dependency:

- WP2
- WP3
- WP4

## WP8 — Room MVP

Scope:

- Victor -> Claude
- Victor -> Codex
- Claude -> Codex
- Codex -> Claude
- room messages

Dependency:

- WP5
- WP6
- WP7

## WP9 — Safety controls

Scope:

- working-root whitelist
- sideband capability whitelist
- early cost hook
- parse-and-drop malformed events

Dependency:

- WP4

## WP10 — Generic terminal support

Scope:

- generic terminal driver
- manifest/config-driven instance creation
- multi-instance naming validation

Dependency:

- WP4
- WP7

## WP11 — Hardening

Scope:

- refined liveness
- output throttling
- replay
- stale cleanup
- deeper sandbox controls

Dependency:

- MVP complete

## Suggested build order

1. WP0
2. WP1
3. WP2
4. WP3
5. WP4
6. WP5
7. WP6
8. WP7
9. WP8
10. WP9
11. WP10
12. WP11

## Rough size estimate

For the first daily-usable version:

- runtime + drivers + PTY host: `4k-7k LOC`
- Tauri/xterm.js UI + glue: `2k-4k LOC`
- tests/scripts/config: `1k-2k LOC`

Rough total:

- **9k-14k LOC**

