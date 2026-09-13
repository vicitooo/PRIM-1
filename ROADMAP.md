# PRIM-1 — Roadmap

**Status:** Directional roadmap after the first real Windows MVP
**Date:** 2026-04-15

## 1. What the project is today

Today the project is:

- a Windows-first supervisor-owned PTY runtime
- a Tauri desktop app with an ordered, persistent set of `SessionId`-keyed tabs
- built-in Claude Code, Codex, Grok, Prime Agent (`wsl:Ubuntu`), and Generic Terminal session drivers
- native Windows workspace/per-session directory selection, typed Ubuntu paths for Prime, and visible permission profiles
- in-process operator session/room controls, plus a pane-local Windows sideband
  limited to self ping/wait/input/key and membership-derived room read/post
- explicit one-room-per-session `RoomId` membership, a bounded memory-only feed,
  feed-only Post, and explicit one-member / Send All delivery

Today the project is **not yet**:

- a user-extensible arbitrary-program/driver product
- a freely rearrangeable multi-pane workspace manager
- production-accepted for exact final-child room delivery and two-room / 3+
  isolation (the implementation exists; the artifact matrix remains open)
- a validated Linux/macOS product

That distinction matters. The current session model is generic, but the driver
catalog, room model, and layout system remain deliberately bounded.

## 2. Near-term product goal

Finish the persistent-session and explicit-room daily-driver proof.

That means:

- reliable create/configure/start/stop/restart/delete behavior
- restart-durable ordered definitions with zero automatic process starts
- normal-by-default harness permissions and direct executable launch
- clear lifecycle state in the UI
- stable restart/resume expectations
- clear limitation list
- repeatable validation flow
- two simultaneous rooms / at least three members with exact recipient,
  isolation, ordering, partial-failure, and receiver-oracle evidence

The point is to prove the safe tabbed runtime and small explicit room model
before adding custom drivers or richer layouts.

## 3. Future development themes

### 3.1 User-extensible driver catalog

The current registry can hold repeated Claude, Codex, Grok, Prime, and Generic
Terminal sessions. A later product slice may support additional terminal-first
CLIs.

That implies:

- typed driver capability discovery
- qualified executable resolution owned by the backend
- no renderer-controlled arbitrary command or argument surface
- driver-specific overrides only where needed

### 3.2 Richer layouts

The UI can create, rename, reorder, and delete persistent session tabs. It keeps
inactive xterm buffers mounted but shows one terminal at a time. It does not yet
provide freely rearrangeable simultaneous panes.

Future layouts should support:

- one pane
- two panes
- many panes
- add/remove/rearrange simultaneous panes
- system log + explicit room feed + active session set

### 3.3 Workspace / home model

The app now remembers one workspace preference and a qualified directory per
session. A later workspace/home layer may group sessions and rooms above that
flat ordered catalog.

Future UX direction:

- a home/workspace view
- folders/projects as first-class units
- each folder can own multiple agent sessions
- quick re-entry into prior rooms

Examples:

- one workspace with one Claude + one Codex
- one workspace with three Codex workers
- one workspace with Claude + Codex + generic shell + Hermes

### 3.4 Multi-instance addressing

The current model has stable internal `SessionId` values, duplicate display
labels, repeated same-driver workers, deterministic UUID-derived pane aliases,
and explicit one-room-per-session `RoomId` membership. Labels never carry
authority. It does not yet support named workspace ownership or multi-room
membership.

Future addressing must support:

- `claude:research`
- `claude:review`
- `codex:test`
- `codex:worker-2`
- per-workspace session ownership
- stable instance IDs

### 3.5 Cross-platform portability

The architecture already points in this direction:

- Windows: named pipes
- POSIX: Unix sockets
- PTY abstraction via portable PTY layer

But product truth today:

- the desktop and native drivers are Windows-only
- Prime crosses from that Windows host into the measured Ubuntu WSL distribution
  through an explicit dual-scope lifecycle; this is not a general Linux port
- Linux/macOS are planned ports, not current product claims

Cross-platform completion requires:

- real validation on Linux
- real validation on macOS
- shell/helper parity outside PowerShell
- packaging and operator-flow validation on those OSes

### 3.6 Better terminal control

The current wrapper is strongest at:

- operator-controlled start/stop/restart
- persistent ordered session and room definitions
- explicit feed-only post and one-member / Send All room delivery
- pane-local self input/key plus membership-derived room read/post

Future control improvements:

- richer driver-specific key handling where empirically safe
- better selection/copy behavior
- stronger resume behavior after interactive CLI exits

### 3.7 Intelligent outage handling

This does **not** belong to the current persistent-tab proof.

It belongs to a later supervision/orchestration layer that can distinguish:

- upstream API/service degradation while the child process is still alive
- child-process crashes
- PTY/control-plane transport failures
- wrapper-level failures

Future capabilities should include:

- explicit degraded/outage state in the UI
- error classification instead of treating all failures as generic session trouble
- guarded restart/backoff policy for upstream-error storms
- clearer operator guidance about when to restart, wait, or leave the session alone

This becomes important once the wrapper is expected to run longer autonomous sessions, not just the current live room proof.

## 4. Guiding product rule

Do not confuse:

- **runtime generality**
- **product completeness**

It is acceptable for the current proof to keep a small built-in driver catalog as long as:

- the architecture stays modular
- the session model stays generic
- driver-specific behavior is isolated
- the UI is not painted into a corner

## 5. Sequencing rule

Build and admit in this order:

1. finish and dogfood persistent flat session tabs
2. prove normal/unsafe launch behavior with real harnesses
3. prove explicit `RoomId` membership/feed and addressed delivery on the exact
   production artifact
4. add richer simultaneous-pane/workspace layouts only when the flat model proves insufficient
5. validate Linux/macOS
6. add intelligent outage handling before treating the system as an unattended operations runtime

That keeps the project honest and prevents scope from outrunning the proof.

## 6. Durable CC, Grok, and Codex collaboration protocol

PRIM-1 development uses two deliberately separate collaboration surfaces.

### 6.1 Agent Bus now: advisory review about PRIM-1

Codex keeps persistent Agent Bus conversations with CC and Grok for architecture, implementation, evidence, deletion, performance, and UX review. Every handoff names:

- the exact source commit, or explicitly labels a dirty checkpoint as non-admitted
- the exact production artifact hash
- immutable evidence paths and the narrow RED/GREEN scope they prove
- open contract gates and known counterevidence

Codex is the sole integrator. CC owns architecture, correctness, Windows runtime, and security pressure-testing; Grok owns simplicity, performance, and UX pressure-testing. A review finding changes code only after it is reproduced locally as a failing test or append-only RED receipt. Agent Bus text is advisory: it carries no operator authority and can never prove PRIM-1 transport, PTY delivery, receiver behavior, or gate completion.

### 6.2 Inside PRIM-1: admitted first-class sessions

CC and Grok may collaborate as PRIM-1 sessions only through the same first-class session/driver path as every other harness. There is no bearer, special bypass, Agent Bus fallback, or hidden compatibility transport. Each harness must first pass, on an exact production hash:

1. driver-native launch, intended cwd, readiness, raw input/output, stop, restart, and zero-orphan shutdown; Prime additionally proves both its Ubuntu service and outer Windows job
2. applicable kernel ownership and stale-run/replacement rejection without inventing a cross-kernel caller identity
3. driver-specific framing, measured payload ceiling and submit gesture, positive turn-start observation, and independent receiver oracle
4. fail-closed handling of trust/authentication/limit/modal states while raw recovery input remains usable
5. direct and explicit `SessionId`/`RoomId` delivery, then multi-member room delivery, with Agent Bus stopped
6. privacy, ungraceful termination, clean-install, and production-build performance checks against the packaged desktop artifact

Until the independent receiver oracle exists, a response token proves only that a routed turn was reached and answered; it does not prove byte-exact final-child delivery. Agent Bus evidence never substitutes for any native-session gate.

### 6.3 Simplicity and deletion rule

Keep one implementation path behind small driver-specific behavior. Once a replacement passes the full relevant matrix, delete the superseded path and its compatibility surface. Do not add an Agent Bus transport inside the supervisor, a task engine, cloud coordination, a second control transport, multi-tenancy, or parallel speculative abstractions. Prove the existing generic-terminal path, use that seam to admit Grok and the typed Prime/WSL boundary, and widen the UI only as those receipts require.
