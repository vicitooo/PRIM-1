# PRIM-1 — Architecture

**Status:** Canonical architecture spec
**Date:** 2026-04-14

## 1. Objective

Build a generic local runtime for terminal-first AI tools where:

- the operator remains in the live loop
- supervised agents communicate in real time
- all terminal activity is visible
- lifecycle is controlled by a single supervisor
- new CLIs can be added without changing the core runtime

## 2. High-level shape

The system consists of five main parts:

1. **Supervisor runtime**
2. **PTY host**
3. **Driver layer**
4. **Sideband control plane**
5. **UI**

These are distinct on purpose. Mixing them is what makes terminal automation brittle.

### 2.1 Current product surface vs target architecture

This distinction must stay explicit.

Current product surface:

- an atomically persisted, backend-ordered set of Claude Code, Codex, Grok,
  Prime Agent (Ubuntu WSL), and Generic Terminal session definitions
- stable opaque `SessionId` authority across desktop lifecycle, input, resize,
  direct routing, renderer state, and run-event lineage; mutable labels are
  display-only
- a flat tab UI showing one retained xterm buffer at a time
- native Windows working-directory selection, backend-qualified Ubuntu paths
  for Prime, and visible `Normal`/`Unsafe` permission profiles
- Windows-first validation
- explicit, atomically persisted `RoomId` definitions and one-room-per-session
  membership
- a bounded in-memory room feed with a visible composer, feed-only Post, and
  explicit one-member / Send All delivery
- a tokenless, pane-local Windows sideband limited to self
  `ping`/`wait_quiet`/`send_input`/`send_key` and membership-derived room
  read/feed-only post

Target architecture:

- generic terminal-first runtime
- dynamic pane/layout model
- user-extensible driver catalog
- portable to Linux/macOS

The current implementation is a **generic persistent-session proof with a
bounded built-in driver catalog and explicit room/feed slice**, not an
arbitrary-layout or user-extensible-driver product. Production room fidelity,
two-room isolation, and multi-member acceptance remain verification gates.

## 3. Supervisor runtime

The supervisor is the authority and currently lives inside the desktop process; it is not a separate daemon.

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

Prime is a deliberate current use of this boundary rather than an implicit
fallback. The Windows supervisor owns the outer `wsl.exe` ConPTY process job;
inside Ubuntu, a uniquely named transient user-systemd service owns the Prime
guard and payload control group. A run is `Ready` only after the exact service
is active with both guard and payload present. Stop/close succeeds only after
both the Linux service and outer Windows job are proved empty.

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

### 5.2 Built-in drivers

- `claude_cli`
- `codex_cli`
- `grok_cli`
- `prime_cli` (`wsl:Ubuntu`)
- `generic_terminal`

### 5.3 Future drivers

- `hermes_cli`
- `openclaw_cli`

The generic terminal driver is important. It keeps the runtime from becoming product-specific.

### 5.4 Reality check on current implementation

The desktop exposes a deliberately bounded built-in driver catalog rather than
renderer-controlled programs or arguments.

That means:

- the session registry and lifecycle remain driver-agnostic
- Claude Code, Codex, Grok, Prime Agent, and Generic Terminal are explicit typed choices
- user-extensible drivers remain a separate product step

## 6. Sideband control plane

The sideband plane is a narrow, pane-local control channel. It is not the
operator API, recipient-delivery transport, lifecycle API, inventory API, or
room-management API.

The current Windows surface is deliberately limited to:

- `ping`
- `wait_quiet` for the calling pane
- `send_input` to the calling pane
- `send_key` to the calling pane
- `room_read` for the calling pane's current authorized room and join floor
- feed-only `room_post` with sender derived from that caller

A named-pipe connection provides transport, not authority. On Windows the
supervisor obtains and pins the kernel-reported client process, requires it to
belong to exactly one live PTY job, binds the derived pane identity to that run
generation, and revalidates both affiliation and generation at mutation time.
There is no bearer token, credential file, caller-supplied identity, peer target,
or disk-mailbox fallback. Room requests accept no caller-supplied `RoomId`,
sender, peer, or recipient. Operator lifecycle, membership, and recipient
delivery remain direct in-process Tauri commands.

Prime sessions are intentionally excluded from this native sideband. Their
Linux descendants cannot be authenticated through the Windows Job membership
proof used by the named pipe, and no bearer or proxy fallback is introduced.

Room reads/posts use the same kernel-bound caller proof, derive current
`RoomId` membership and sender `SessionId` under the supervisor locks, and are
revoked immediately when the run or membership changes. Prime remains excluded.

### 6.1 Model-facing bridge

Claude Code and Codex receive a session-scoped stdio MCP child named
`prim1_pane`. The child is the signed/packaged PRIM-1 executable in an early
no-UI mode, configured so the harness creates it inside the same pane Job. Its
only tools are `ping`, membership-derived `room_read`, and feed-only
`room_post`. The MCP
request carries neither caller nor room authority; the helper verifies the
desktop named-pipe server PID and creation time, and the supervisor derives the
exact caller/run from the kernel again on every request.

This bridge is deliberately driver-scoped. Grok Build 1.0.0 supports
session-scoped plugins only through its non-interactive agent host, while its
TUI has no equivalent flag and its shell-tool subprocesses are empirically not
members of the pane Job. Redirecting `GROK_HOME` would redirect Grok's sessions
and logs into PRIM runtime storage. PRIM therefore leaves Grok, Prime, and
Generic Terminal unchanged rather than adding a bearer, ancestry check,
global plugin, workspace mutation, or history-redirection fallback.

### 6.2 Transport choice

The control-plane transport model is:

- **Windows (implemented and under production verification):** current-user-local named pipe
- **POSIX (planned, not a product claim):** Unix domain socket

Not localhost TCP by default.

Reason:

- machine-local transport surface
- kernel-reported peer-process attribution on Windows
- rejection of unaffiliated clients before their request frame is accepted

Local HTTP is not a fallback. Any future remote-control layer needs its own
explicit trust boundary and product contract.

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

But not for authority, routing, or lifecycle control.

## 7. Messaging model

### 7.1 Message scopes

- `direct`
  One sender, one recipient

- `room`
  Shared feed provenance plus an explicit member-recipient snapshot; posting
  alone never broadcasts prompts

- `system`
  Supervisor-generated events

- `private`
  Reserved for future local-only notes or hidden operator metadata

### 7.2 Current message types

- one-recipient operator-authored direct messages at the backend boundary
- room messages from the operator or a kernel-derived member
- room membership events
- per-recipient room delivery state (`pending`, `written`, `failed`)
- supervisor-generated lifecycle, delivery, heartbeat, and alert events

Command execution, spawn, restart, and close are not chat message types. They
remain typed in-process operator actions; the pane-local sideband cannot invoke
them.

### 7.3 Routing behavior

On every operator route, the supervisor must:

1. derive operator provenance at the Tauri boundary
2. validate the source request and whole message body
3. resolve the explicit recipient `SessionId` to one exact run
4. preflight its framing before any write, including exact-run evidence that DEC private mode 2004 is enabled and that the driver has not observed a blocked/error-loop state for bracketed-paste drivers
5. record pending metadata without retaining message content
6. hold the recipient run-input permit, revalidate identity/generation/PTY/gate/mode/work-state, write the complete paste frame, then from that successful write boundary wait a one-second base interval plus a conservative proportional 500 milliseconds per MiB of framed input (chosen over the roughly 10/19/174 milliseconds of parser lag observed at 1 KiB/64 KiB/1 MiB), revalidate identity/generation/PTY/gate/work-state, and write one Enter; the 1 MiB body cap keeps compensation to roughly half a second, paste mode may legitimately disable after the completed frame, and raw single-line drivers remain one PTY input containing only the validated source command because a textual provenance prefix would be executable shell input
7. emit a written or failed receipt without claiming final-child or model receipt

Claude, Codex, and Grok receive the visible provenance envelope inside their
bracketed-paste frame. Generic Terminal carries provenance in PRIM's feed,
route receipts, and audit instead; its raw command line is not prefixed with
display text that a shell could parse as code.

The desktop command runs this blocking supervisor operation on Tauri's blocking
pool, so the driver settle interval does not stall the UI or serialize unrelated
desktop IPC behind the route.

The mode-2004 scanner consumes the exact run's raw PTY output before desktop
coalescing or output shedding and resets on every `RunId`. Unknown or disabled
state, or a driver-observed blocked/error-loop state, blocks addressed Claude/Codex
delivery without blocking raw operator or pane-local input. The paste and Enter
boundary is FIFO-serialized and rechecked twice. Output is not used as an
acknowledgement because multiline input may stay
invisible until Enter; each measured interval is version-sensitive compatibility
behavior measured against Claude Code 2.1.226, Codex 0.147.0, and Grok Build
1.0.0, with no automatic retry after an uncertain outcome. The packaged
real-harness byte oracle remains
responsible for final-child fidelity and receiver receipt.

Room **Post** validates and appends one provenance-stamped feed message without
touching any PTY. Room **Send** resolves one membership revision and one or all
explicit member IDs, preflights every exact run and framing before any
message-specific mutation, then appends the immutable message plus one pending
delivery item per recipient. Each recipient uses the same exact-run FIFO writer
and receives a written or failed feed item with truthful prefix progress. A
membership change after the commit affects the next message; an in-flight room
cannot be deleted. Room feed append and publication share one ordering gate so
cursor order cannot race renderer/audit order.

### 7.4 Addressing model

The current operator boundary identifies each logical session by opaque
`SessionId`; restart preserves that ID while creating a new `RunId`, and rename
changes only its display label. Direct routing accepts one recipient ID. Room
delivery accepts one member ID or explicit `all` resolved from one `RoomId`
membership revision. Unknown or stale IDs fail closed, including
delete/recreate with the same label. There is no name, label, implicit
multi-recipient, or all-session fallback.

The registry is keyed by `SessionId` with a separate persisted order and a
deterministic immutable alias table used only by the narrow self-pane sideband.
The private version-1 catalog stores the workspace preference and ordered
session intent. A separate private version-1 room catalog stores ordered room
identity, label, member IDs, and membership revision. Neither persists `RunId`,
process state, launch arguments, environment variables, terminal output, room
feed events, or message content.

## 8. UI model

The current useful UI has four surfaces:

- backend-ordered session tabs
- one active terminal with inactive terminal buffers retained
- the system log
- the active room's member list, bounded feed, and Post / explicit-delivery
  composer

There should also be a small retractable control surface for:

- main menu
- restart
- reconnect
- close
- health
- refresh and room actions

The control surface should stay minimal and subordinate to the core room UX.

### 8.2 Current tab model

The current UI creates, renames, reorders, configures, and deletes persistent
session tabs. Every tab, snapshot, pending-output buffer, and terminal reference
is keyed by `SessionId`. Duplicate labels are disambiguated by driver, cwd, and
short ID. A tab switch hides rather than disposes the inactive xterm; only exact
`SessionDeleted` lineage retirement discards it.

Future layout requirements:

- layout presets for one, two, or many sessions
- workspace-specific session groupings
- simultaneous terminal/room arrangements beyond the current active-room card

### 8.3 Workspace / home model

The future UI should include a workspace/home layer above the live room.

That layer should support:

- a list of folders/projects
- multiple agent sessions per folder
- reopening prior rooms
- identifying which Claude/Codex instance belongs to which workspace

The room remains the core interaction surface, but it should no longer be the only surface.

### 8.1 Desktop and renderer lifetime

The native supervisor is the source of runtime truth while the desktop process is
alive. A renderer reload may reconcile from its current snapshot without replacing
the supervised runs. Closing or crashing the desktop process is different: the
current product deliberately ends every owned process job. A later launch restores
the ordered persisted session definitions as closed tabs and starts no harness
until the operator explicitly starts one. Reattaching live runs across desktop
processes would require a separately hosted supervisor service and is not claimed.

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

Process exit is derived only from PTY/OS process evidence. Terminal text, including
normal CLI version banners or strings that resemble shell/exit output, cannot close
or fail a run.

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

`closed` is a termination-proof claim, not a display convenience. Every explicit
stop and every natural PTY EOF/error or liveness retirement serializes through the
same per-session lifecycle reservation and must prove that the exact run's owned
process job is empty. A failed or bounded-out proof retains the exact run ownership
and reports `failed` with termination uncertainty; start, restart, and delete remain
blocked until a later reserved termination attempt proves the scope empty. Dropping
a PTY/job handle or observing terminal EOF is never accepted as that proof.

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

The desktop renderer is bundled-only and runs under an explicit CSP: scripts
come from the application bundle, objects/frames/forms are denied, and the one
inline-style allowance exists for xterm and the current static HUD markup. The
normal renderer has no `window.__TAURI__` global and the release binary omits
the in-app devtools feature. Its capability manifest grants only runtime-event
listen/unlisten plus read-only focused/fullscreen/minimized queries used by the
test seam; filesystem, shell, menu, image, tray, and devtools-toggle commands
are not granted.

Production-artifact automation is an explicit local diagnostic mode rather
than a second build. A strictly parsed `PRIM1_CDP_PORT` binds WebView2 debugging
to `127.0.0.1` without wildcard origins. Only in that mode does trusted bundled
frontend code install a frozen compatibility bridge containing `invoke`,
`listen`, and `getCurrentWindow`, allowing the same exact artifact to run the
verification matrix and built-in `Page.captureScreenshot` receipts. Without
the opt-in port, the bridge and debugging endpoint are absent.

Per-agent policy should include:

- qualified working-directory selection
- network policy
- sandbox mode
- allowed sideband commands
- launch identity / environment

### 10.1 Early mandatory protections

These are not deferred hardening tasks. They are mandatory from the first working supervisor:

1. **native working-directory qualification**
   The renderer cannot submit a raw path. A native chooser selects an existing
   directory; the supervisor resolves its final path and stable identity,
   rejects runtime overlap, persists that qualified selection, and revalidates
   it immediately before spawn. The workspace preference is a default, not an
   allow-root or OS sandbox.

   Prime uses a separate typed `wsl:Ubuntu` namespace. The backend resolves the
   Ubuntu path and persists its canonical path plus device/inode identity,
   revalidates that identity before every start, and passes it to an immutable
   launch guard that checks it again immediately before spawning Prime. Windows
   paths and `/mnt/c/...` are never silently equated.

2. **sideband capability whitelist**
   After kernel-bound affiliation succeeds, the pane-local schema permits only
   ping, wait, input, a supported key, membership-derived room read, or
   feed-only room post for that caller's live run. Peer targeting, room IDs,
   recipient delivery, lifecycle, inventory, and membership actions are absent.
   This is a protocol boundary, not hostile same-user OS isolation.

3. **named-pipe / socket access control**
   A successful transport connection grants no authority. On Windows, requests proceed only after the kernel-reported peer process is pinned and verified against exactly one live pane job and generation. A future POSIX transport must establish an equivalent peer-identity boundary before becoming a product claim.

### Important CLI reality

- Claude `Normal` launches directly with `--permission-mode manual`; `Unsafe`
  adds only `--dangerously-skip-permissions`.
- Codex `Normal` launches directly with
  `--ask-for-approval on-request --sandbox workspace-write`; `Unsafe` adds only
  `--dangerously-bypass-approvals-and-sandbox`.
- Grok `Normal` launches directly with `--permission-mode default`; `Unsafe`
  changes only that value to `bypassPermissions`.
- Prime launches only with `Normal`. The Windows command is direct `wsl.exe`;
  the Linux side uses absolute `systemd-run`, Python, and `prime-agent` paths,
  with no shell evaluation or renderer-controlled arguments.
- Generic Terminal accepts `Normal` only.

These are visible harness permission profiles, not hostile same-user OS
isolation. Stronger containment would require a separately measured restricted
user, ACL, container, VM, or equivalent boundary.

## 11. 24/7 target model

The current supervisor lifetime is the desktop-process lifetime. A future 24/7 property would belong to a separately hosted supervisor service, not to one immortal child process.

What remains continuous:

- supervisor runtime
- session identity and state
- metadata audit history
- routing and health model

What may come and go:

- actual child CLI processes

To the operator, the agent appears persistent. Under the hood, the process can be restarted, resumed, or recreated by policy.

### 11.1 Supervisor-of-supervisor

For true 24/7 operation, the supervisor itself must be restartable by the host.

Recommended first implementation:

- **Windows:** run under a service wrapper such as `nssm`
- **Linux:** run under `systemd`

This is separate from child-agent lifecycle and should be treated as part of the runtime envelope.

## 12. Metadata audit and live state

The audit log is a durable operational ledger, not a transcript or room-replay store.

Format:

- JSONL, one structured event per line
- rolling daily files

Every record carries an event type and timestamp. Event-specific metadata may add session/actor/target identity, request or route IDs, lifecycle state, action/phase/result, and delivery counts.

Terminal output and message content remain available only through the live
runtime and desktop event path. `session_output` is not written to audit;
routed and room-message content is replaced with `[content omitted]`. Room
definitions/membership persist separately, while every room feed is bounded to
512 events / 16 MiB in memory and every page to 64 events / 2 MiB, with epoch
reset and eviction gaps explicit. The renderer retains only the active room's
same-sized feed window and reloads inactive rooms from the supervisor.

The PTY host carries split UTF-8 code points across reads, then the exact-run
mode-2004 scanner consumes that same incrementally decoded stream before
renderer sanitization, coalescing, or shedding. The desktop bridge holds at
most 512 queued events. Lifecycle and control events use lossless bounded
backpressure; only already-sanitized `session_output` display events may be
shed under saturation. Every such loss produces a visible, run-scoped gap
notice before the next surviving event (or at drain), and renderer
terminal-control parsing remains synchronized. Live terminal history can
therefore be incomplete under sustained renderer pressure; full output
fidelity under saturation is not claimed.

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
- fail-closed behavior for malformed or unaffiliated sideband requests
- reconnecting the UI without dropping supervised sessions
- serializing room feed append/publication so cursor order is identical in the
  renderer and metadata audit even when multiple agents post concurrently
- refusing room deletion while delivery is in flight and recording truthful
  partial per-recipient outcomes without retry

## 14. Reuse of current bridge code

The current bridge is useful source material.

Reusable parts:

- Codex subprocess logic
- partial Claude subprocess logic
- archive and dispatch patterns
- session registry concepts

To be replaced:

- hardcoded dual-agent branching
- file-only transport as the main runtime
- kill-on-DONE behavior
- implicit lifecycle

## 15. Non-goals for architecture v1

- external notification or external notification channels
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
- the same narrow, identity-derived sideband policy after each OS boundary is empirically verified
- different OS-specific validation and packaging work

So the correct claim is:

- **portable by design**
- **Windows-proven**
- **Linux/macOS not yet proven**
