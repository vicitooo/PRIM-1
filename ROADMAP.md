# CLI-master-wrapper — Roadmap

**Status:** Directional roadmap after the first real Windows MVP
**Date:** 2026-04-15

## 1. What the project is today

Today the project is:

- a Windows-first supervisor-owned PTY runtime
- a Tauri desktop app with two hardcoded agent panes
- a working Claude/Codex collaboration shell
- a sideband control plane for start/stop/restart/input/route

Today the project is **not yet**:

- a fully generic terminal product
- a dynamic multi-pane workspace manager
- a validated Linux/macOS product

That distinction matters. The runtime direction is generic. The current product surface is still Claude/Codex-shaped.

## 2. Near-term product goal

Finish the Claude/Codex collaboration polish first.

That means:

- reliable routed messaging
- clear lifecycle state in the UI
- stable restart/resume expectations
- clear limitation list
- repeatable validation flow

The point is to finish the proof of concept before widening the surface area.

## 3. Future development themes

### 3.1 Generic terminal wrapper

The long-term product should support arbitrary terminal-first CLIs, not only Claude and Codex.

That implies:

- generic session definitions
- generic shell panes
- launch-time program/args/working-dir configuration
- driver-specific overrides only where needed

### 3.2 Dynamic panes and layouts

The UI should not stay fixed at two agent panes forever.

Future layouts should support:

- one pane
- two panes
- many panes
- add/remove/rearrange panes
- system log + router + active session set

### 3.3 Workspace / home model

The app should grow beyond “open the wrapper and immediately see Claude/Codex.”

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

The current model assumes one `claude` and one `codex`.

That is not enough for the real product.

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

- only Windows has been exercised seriously
- Linux/macOS are planned ports, not current product claims

Cross-platform completion requires:

- real validation on Linux
- real validation on macOS
- shell/helper parity outside PowerShell
- packaging and operator-flow validation on those OSes

### 3.6 Better terminal control

The current wrapper is strongest at:

- text injection
- Enter/submit
- start/stop/restart
- sideband routing

Future control improvements:

- arrow keys
- tab/esc
- ctrl+c as explicit sideband action
- better selection/copy behavior
- stronger resume behavior after interactive CLI exits

### 3.7 Intelligent outage handling

This does **not** belong to the current Claude/Codex room polish.

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

It is acceptable for the MVP proof to support only Claude/Codex as long as:

- the architecture stays modular
- the session model stays generic
- driver-specific behavior is isolated
- the UI is not painted into a corner

## 5. Sequencing rule

Build in this order:

1. finish Claude/Codex collaboration polish
2. produce the architecture review
3. tighten the modularity story where needed
4. expose generic terminal support in the UI
5. add dynamic panes/workspace model
6. validate Linux/macOS
7. add intelligent outage handling before treating the system as an unattended operations runtime

That keeps the project honest and prevents scope from outrunning the proof.
