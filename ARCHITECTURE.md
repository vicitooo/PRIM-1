# CLI-master-wrapper — Architecture

**Status:** Canonical architecture spec
**Date:** 2026-04-14

## 1. Objective

Build a generic local runtime for terminal-first AI tools where:

- Victor remains in the live loop
- supervised agents communicate in real time
- all terminal activity is visible
- lifecycle is controlled by a single supervisor
- new CLIs can be added without changing the core runtime

## 2. High-level shape

The system consists of five main parts:

1. **Supervisor daemon**
2. **PTY host**
3. **Driver layer**
4. **Sideband control plane**
5. **UI**

These are distinct on purpose. Mixing them is what makes terminal automation brittle.

### 2.1 Current product surface vs target architecture

This distinction must stay explicit.

Current product surface:

- two hardcoded panes: Claude and Codex
- Windows-first validation
- collaboration through supervisor-mediated routing

Target architecture:

- generic terminal-first runtime
- dynamic session set
- dynamic pane/layout model
- portable to Linux/macOS

The current implementation is a **Claude/Codex proof built on a generic-shaped runtime**, not a finished generic terminal product.

## 3. Supervisor daemon

The supervisor is the authority.

Responsibilities:

- spawn agent processes
- track process trees
- store session state
- route messages
- enforce policy
- run health checks
- apply restart rules
- track usage and cost telemetry
- persist audit logs
- expose control API

The supervisor is the only component allowed to:

- start a new agent
- stop an agent
- restart an agent
- reattach to a session
- decide whether an action request is permitted

Agents may request actions. They do not execute lifecycle actions on peers directly.

## 4. PTY host

The PTY host is the visibility layer.

Windows target:

- use ConPTY

Responsibilities:

- create a PTY per agent
- launch the target process inside that PTY
- capture stdout/stderr byte stream
- inject stdin into the same PTY
- surface output to the UI

Why PTY ownership matters:

- PID alone is not enough
- scraping random existing terminals is brittle
- terminal apps redraw, echo commands, and mix prompts with content
- if the runtime does not own the PTY, it does not truly own the interaction

### 4.1 Renderer and UI stack

The renderer choice is no longer open.

Use:

- **Rust + Tauri** for the desktop app shell
- **xterm.js** for terminal rendering in the UI

Why:

- xterm.js already solves ANSI handling, cursor movement, scrollback, colors, alternate screen behavior, and terminal resizing
- Tauri gives a clean desktop shell without forcing the runtime into JavaScript
- Rust remains the right place for PTY ownership and supervisor logic

This avoids the hidden cost of building a terminal renderer from scratch.

### 4.2 ConPTY fallback

If native Windows ConPTY proves unstable for a required CLI flow, the fallback path is:

1. keep the Rust supervisor architecture unchanged
2. route the affected agent through a POSIX PTY environment in WSL2
3. keep the same UI and control plane above it

The fallback is architectural, not product-level. The room model should not change if the PTY backend changes.

## 5. Driver layer

Drivers translate generic supervisor actions into CLI-specific behavior.

### 5.1 Driver contract

Each driver should define:

- executable path
- launch arguments
- resume arguments
- environment bootstrap
- supported sandbox/security modes
- health probe strategy
- interrupt strategy
- clean shutdown strategy
- hard kill strategy
- optional output parser hooks

### 5.2 Initial drivers

- `claude_cli`
- `codex_cli`

### 5.3 Future drivers

- `hermes_cli`
- `openclaw_cli`
- `generic_terminal`

The generic terminal driver is important. It keeps the runtime from becoming product-specific.

### 5.4 Reality check on current implementation

The runtime already has a generic-terminal driver concept, but the current app does not yet expose it as a first-class product feature.

That means:

- the architecture supports generic sessions
- the current UI still hardcodes Claude and Codex
- turning the app into a true universal wrapper is a product step still ahead of us

## 6. Sideband control plane

The sideband plane is the reliable control channel.

Examples:

- `send --to claude --content "..."`
- `send --to room --content "..."`

Primary use cases:

- direct messages
- room broadcasts
- restart requests
- spawn requests
- close requests
- ping/health probes
- command execution requests

### 6.1 Transport choice

The default control plane transport should be:

- **Windows:** named pipe
- **POSIX:** Unix domain socket

Not localhost TCP by default.

Reason:

- lower attack surface
- local file-permission model
- avoids accidental browser or unrelated local-process access

Local HTTP can exist later as a compatibility or remote-control layer, but it should not be the default local control path.

### Why explicit sideband is required

stdout intent detection is too weak to be the primary transport.

Problems:

- ANSI redraw noise
- partial output
- normal chat text can look like commands
- approvals and prompts can be mixed into the stream
- command echos can create false positives

stdout parsing may still be used for:

- UX markers
- progress hints
- telemetry
- fallback stalled-session heuristics

But not for authoritative routing.

## 7. Messaging model

### 7.1 Message scopes

- `direct`
  One sender, one recipient

- `room`
  Broadcast to all visible participants

- `system`
  Supervisor-generated events

- `private`
  Reserved for future local-only notes or hidden operator metadata

### 7.2 Message types

- `chat_message`
- `command_request`
- `command_result`
- `heartbeat`
- `health`
- `spawn_request`
- `restart_request`
- `close_request`

### 7.3 Routing behavior

On every sideband message, the supervisor should:

1. validate sender permissions
2. log the event
3. route it to the target scope
4. visibly stamp the injection in the target PTY or room
5. emit any resulting system log entries

### 7.4 Addressing model

The supervisor must not assume one Claude and one Codex forever.

Addressing should support:

- `claude`
- `codex`
- `claude:work`
- `claude:research`
- `codex:test`
- `room`

The routing model should treat agent instances as named sessions, not hardcoded singular roles.

## 8. UI model

The first useful UI is a four-surface room:

- Claude pane
- Codex pane
- System log pane
- Victor input/router

There should also be a small retractable control surface for:

- main menu
- restart
- reconnect
- close
- health
- room actions

The control surface should stay minimal and subordinate to the core room UX.

### 8.2 Future pane model

The current fixed two-pane layout is acceptable for the first proof, but it is not the final UI model.

Future UI requirements:

- add/remove panes
- dynamic pane count
- agent instance labels and IDs
- layout presets for one, two, or many sessions
- workspace-specific session groupings

### 8.3 Workspace / home model

The future UI should include a workspace/home layer above the live room.

That layer should support:

- a list of folders/projects
- multiple agent sessions per folder
- reopening prior rooms
- identifying which Claude/Codex instance belongs to which workspace

The room remains the core interaction surface, but it should no longer be the only surface.

### 8.1 Reconnect behavior

The UI is not the source of truth. The supervisor is.

If the UI crashes or is closed:

- supervised agents may continue running
- the supervisor continues logging and routing
- reopening the UI should reattach to live sessions and replay recent log state

This prevents the room from depending on one fragile window process.

## 9. Lifecycle model

### 9.1 States

- `starting`
- `ready`
- `busy`
- `idle`
- `stalled`
- `restarting`
- `failed`
- `closed`

### 9.2 Definitions

**Idle**

- no pending work
- no messages
- no output

**Stalled**

- work is expected
- output has stopped unexpectedly
- heartbeat is failing or absent
- optional supporting signals show no meaningful activity

### 9.3 Closure and restart policy

Do not kill on DONE.

An agent should only be closed or restarted because of:

1. explicit supervisor action
2. idle TTL
3. stall timeout while busy
4. heartbeat failure
5. crash
6. absolute runtime ceiling
7. restart backoff exhaustion

This separates normal completion from process teardown.

### 9.4 Heartbeat strategy for v1

V1 heartbeat strategy is:

- **passive, output-based liveness** with per-driver thresholds

Meaning:

- if an agent is producing output, it is considered alive
- if an agent is marked busy and produces no output for too long, it is a stall candidate
- thresholds may differ by driver

Active heartbeats can be added later, but v1 should not block on them.

## 10. Security model

Generic wrapper does not imply generic trust.

Per-agent policy should include:

- working directory restrictions
- network policy
- sandbox mode
- allowed sideband commands
- launch identity / environment

### 10.1 Early mandatory protections

These are not deferred hardening tasks. They are mandatory from the first working supervisor:

1. **working-directory whitelist**
   The supervisor refuses to spawn an agent without an explicit allowed root.

2. **sideband capability whitelist**
   By default, an agent can act on itself and send messages. Cross-agent lifecycle requests require explicit policy.

3. **named-pipe / socket access control**
   Only authorized local clients should be able to issue control-plane requests.

### Important CLI reality

- Codex exposes native sandbox modes directly.
- Claude appears to expose permission modes and resume, but not Codex-style OS-enforced read-only sandbox.

Implication:

- Codex secure modes can start as a driver setting.
- Claude secure continuous modes will need external sandboxing later:
  restricted worktree, lower-privilege user, ACLs, container, or equivalent.

This later work does not remove the need for the early mandatory protections above.

## 11. 24/7 model

The 24/7 property belongs to the supervisor service, not to one immortal child process.

What remains continuous:

- supervisor daemon
- session identity and state
- audit history
- routing and health model

What may come and go:

- actual child CLI processes

To Victor, the agent appears persistent. Under the hood, the process can be restarted, resumed, or recreated by policy.

### 11.1 Supervisor-of-supervisor

For true 24/7 operation, the supervisor itself must be restartable by the host.

Recommended first implementation:

- **Windows:** run under a service wrapper such as `nssm`
- **Linux:** run under `systemd`

This is separate from child-agent lifecycle and should be treated as part of the runtime envelope.

## 12. Audit log and replay

The audit log is the source of truth for replay.

Format:

- JSONL, one structured event per line
- rolling daily files

Minimum fields:

- timestamp
- event type
- actor
- target
- session id
- driver
- payload summary
- result

Replay should read the audit log forward. A separate transcript system should not be invented unless the audit log proves insufficient.

## 12.1 Multi-instance identity

As the product grows past one Claude and one Codex, each session needs:

- stable runtime ID
- human-facing label
- workspace association
- driver kind
- lifecycle state

Names like `claude:research` are useful, but an internal stable ID is still required.

## 13. Failure handling for the room

The room itself needs first-class failure behavior.

At minimum the architecture must support:

- output throttling when an agent floods the pane
- parse-and-drop behavior for malformed sideband messages
- reconnecting the UI without dropping supervised sessions
- serializing room/system events in the audit log even when multiple agents emit concurrently

## 14. Reuse of current bridge code

The current bridge is useful source material.

Reusable parts:

- Codex subprocess logic
- partial Claude subprocess logic
- archive and dispatch patterns
- session registry concepts
- watcher loop structure

To be replaced:

- hardcoded dual-agent branching
- file-only transport as the main runtime
- kill-on-DONE behavior
- implicit lifecycle

## 15. Non-goals for architecture v1

- Telegram or external notification channels
- multi-machine federation
- productized SaaS concerns
- complex workflow automation
- orchestration of large agent trees

The architecture should allow those later, but they are not required to prove the runtime.

## 16. Portability stance

The architecture is intentionally portable, but the product claim must stay conservative.

Today:

- Windows is the real validated target

Planned:

- Linux
- macOS

What portability means here:

- same supervisor model
- same PTY ownership model
- same sideband control-plane model
- different OS-specific validation and packaging work

So the correct claim is:

- **portable by design**
- **Windows-proven**
- **Linux/macOS not yet proven**
