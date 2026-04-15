# CLI-master-wrapper — Architecture Review

**Status:** Post-MVP review after the first real Windows collaboration runs
**Date:** 2026-04-15

## 1. Executive summary

The current system is **good enough to prove the core idea**:

- a supervisor-owned PTY runtime
- a visible desktop UI
- a sideband control plane
- Claude/Codex collaboration through routed terminal messages

The current system is **not yet** a finished universal terminal wrapper.

That is not a failure. It is the correct reading of the current build.

The architecture direction is strong.
The current product surface is still narrow.

## 2. What is strong

### 2.1 The system owns the PTY

This is the most important architectural win.

The wrapper does not depend on random existing windows or PID-only tricks.
It launches the target process inside a PTY it owns and controls.

That gives:

- real visibility
- real input injection
- lifecycle ownership
- reliable logging boundary

### 2.2 The core is separated into useful layers

The current runtime already separates:

- PTY host
- supervisor
- drivers
- control plane
- UI

That is the correct decomposition.
It gives the project a real path toward genericity.

### 2.3 The control plane is explicit

The sideband path is a real architectural asset.

Instead of relying only on terminal scraping, the system has:

- structured requests
- structured responses
- named-pipe / Unix-socket direction
- helper scripts that agents can call

That is the right control model.

### 2.4 Claude/Codex collaboration is real

The current build is not theoretical.

It already proved:

- Victor -> Claude
- Victor -> Codex
- Claude -> Codex
- Codex -> Claude
- room-style broadcasts

That matters because it validates the core room concept.

### 2.5 The hardening trend is correct

Recent work improved the system in the right way:

- crash diagnostics
- mailbox fallback
- BOM-tolerant decoder
- stale-session pruning
- regression coverage for known-good paths

This is exactly the kind of hardening the project needs.

## 3. What is weak

### 3.1 The product surface is still hardcoded

The current UI and default session registry are still shaped around:

- one Claude
- one Codex

That means the runtime is more modular than the product surface.

This is the biggest current mismatch.

### 3.2 Generic terminal support exists architecturally, not product-wise

There is already a generic terminal driver.
But the product does not yet expose it as a real first-class feature.

Today you cannot honestly say:

- “this is a universal terminal wrapper”

You can honestly say:

- “this is a Claude/Codex collaboration wrapper built on a generic-shaped runtime”

### 3.3 Driver-specific behavior still leaks into the supervisor

The current routed-submit logic still contains driver-aware behavior in the supervisor:

- Claude timing/payload behavior
- Codex timing/payload behavior

This works, but it is an abstraction leak.

Long-term, driver-specific routing behavior should move further behind the driver boundary.

### 3.4 Session identity is still too name-based

Right now the system is still close to singular names like:

- `claude`
- `codex`

That is not enough for:

- multiple Claudes
- multiple Codexes
- workspace ownership
- reattachment across larger rooms

The system needs durable internal IDs plus human-facing labels.

### 3.5 Collaboration readiness is still heuristic

The current model is much better than before, but still not final.

The system can route text into a session that is:

- technically running
- not truly settled
- reconnecting internally
- still warming up tools/MCPs

That means readiness is still partly inferred rather than explicitly modeled.

### 3.6 Terminal control is incomplete

The wrapper is strong at:

- text input
- Enter/submit
- route/start/stop/restart

It is weaker at:

- arrow-key approval menus
- tab/esc
- ctrl+c as a first-class sideband action
- exact terminal UX parity

### 3.7 Cross-platform support is architectural, not validated

The code already points toward POSIX support.
That is good.

But product truth today is still:

- Windows-proven
- Linux/macOS unproven

That distinction must stay explicit.

## 4. Modularity assessment

## Verdict

**Mostly yes at the backend, not yet enough at the product boundary.**

### Backend modularity is good enough

The backend already has the right structure for growth:

- driver enum
- generic launch specs
- transport abstraction direction
- PTY host crate
- supervisor crate
- shared types

That is a real foundation.

### Product modularity is not finished

The current app still assumes:

- fixed sessions
- fixed panes
- fixed labels

That means the architecture is not painted into a corner, but the UI/product layer still is.

## Conclusion

We did build it modularly enough to continue.
We did **not** yet finish the last mile that makes that modularity visible to users.

## 5. Portability assessment

### Windows

This is the only platform that can be treated as current product reality.

What is true on Windows today:

- real PTY ownership
- real desktop UI
- real control plane
- real agent collaboration
- real logs and tests

### Linux

Likely feasible without changing the core architecture.

What still needs work:

- validation of PTY behavior there
- helper parity beyond PowerShell
- packaging and workflow validation

### macOS

Same answer as Linux, but likely with its own desktop/runtime quirks.

Again:

- architecture says yes
- current product proof says not yet

## Portability conclusion

The system is **portable by design**, but **not yet portable in practice**.

## 6. Can this become a true generic terminal wrapper?

Yes.

But it still needs these concrete product steps:

1. dynamic session definitions
2. dynamic pane rendering
3. generic shell/session creation in the UI
4. per-driver behavior pushed further behind driver interfaces
5. durable session IDs and workspace ownership
6. POSIX helper/tooling parity

Without those, it remains a strong specialized wrapper rather than a universal one.

## 7. Most important limitations right now

These are the limitations that matter most today:

1. fixed Claude/Codex pane model
2. no first-class generic shell pane in the UI
3. no multi-instance workspace model yet
4. readiness is still partly heuristic
5. key-control/approval-menu support is incomplete
6. Linux/macOS are not validated product targets yet
7. session continuity/resume is not yet a product-level feature

## 8. Recommended next steps

The order should be:

1. finish Claude/Codex collaboration polish and live validation
2. keep documenting the current limitations honestly
3. move session identity from simple names to durable IDs
4. expose generic terminal sessions in the UI
5. generalize the pane/workspace model
6. validate Linux/macOS after the generic session model exists

## 9. Final judgment

The project is bigger than the first intuition, but the current architecture did **not** make that growth impossible.

That is the important outcome.

The system we have now is:

- a valid proof of the room model
- a strong foundation for a generic terminal supervisor
- not yet the final generic terminal product

That is a good place to be.
