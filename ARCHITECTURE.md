# PRIM-1 — Architecture

PRIM-1 is a Windows desktop host for terminal-first agents. The supervisor owns their PTYs and process lifecycles; the operator sees the terminals and chooses how sessions collaborate in rooms.

For the user flow, start with [README.md](README.md). Protocol and lifecycle details are in [RUNTIME-CONTRACTS.md](RUNTIME-CONTRACTS.md); commands are in [CONTROL-SURFACE.md](CONTROL-SURFACE.md).

## High-level shape

| Component | Responsibility |
|---|---|
| Tauri desktop | Operator commands, window lifetime, and frontend event bridge |
| xterm.js frontend | Visible terminals, rooms, feed, session controls, and settings |
| Rust supervisor | Session registry, authorization, lifecycle, room membership, routing, and audit |
| PTY host | ConPTY I/O and owned process scopes |
| Drivers | Harness launch/resume arguments, readiness, and work-state classification |
| Pane sideband | Narrow, authenticated named-pipe requests from supervised sessions |

The driver catalog is built in: Claude Code, Codex, Grok, Prime Agent, and Generic Terminal. Arbitrary user-defined drivers, simultaneous terminal layouts, and native macOS/Linux support remain future work.

## Supervisor and process ownership

The supervisor runs inside the desktop process. It creates each PTY, owns input/output and lifecycle reservations, tracks exact run identity, and publishes structured events.

Every logical session has a stable `SessionId`. Each start creates a fresh `RunId` and advances its generation. Rename changes the display label; restart changes the run, not the session. Duplicate labels are allowed. Stale requests cannot acquire authority over a replacement run.

On Windows, each run has an owned process Job. Termination is complete only when the exact owned scope is proved empty. Failed or timed-out cleanup retains ownership in a failed state and prevents a replacement from silently overlapping it. Process text is never proof of process exit.

Prime adds an Ubuntu WSL user-systemd service with a guard and payload. The supervisor qualifies the Linux directory, reconciles stale services, and checks both the Linux service and outer Windows Job during shutdown. This is a specific integration, not a cross-platform desktop runtime.

## PTY and renderer

PTY output supplies the visible terminal, not authority for control requests. Drivers inspect the unshed stream for terminal modes and work state before output reaches the renderer.

The frontend uses xterm.js for ANSI rendering, scrollback, alternate screens, and resize. It shows one terminal at a time and retains inactive buffers while the app runs. Sessions continue running when another room or tab is selected.

The desktop event queue is bounded. Control/lifecycle events use backpressure; display output can be shed under sustained load with explicit run-scoped gap warnings. A gap means the displayed transcript may be incomplete.

## Drivers

Drivers construct qualified executable launches without renderer-supplied commands or argument lists. They supply harness-specific permission flags, conversation resumption, readiness, and semantic output classification.

- Claude Code and Codex launch their native Windows CLIs.
- Grok launches its full-screen TUI, with a new session ID or the stored resume ID.
- Prime launches through direct `wsl.exe` and absolute Linux executables under its service guard.
- Generic Terminal launches a shell. If a supported harness is started inside it, the supervisor can adapt delivery framing from process-image detection; this does not add that harness's complete work-state gating.

Normal permissions are the default. Claude Code, Codex, and Grok expose an explicit **Unsafe** profile; Prime and Generic Terminal are Normal-only. PRIM-1 does not add an OS sandbox to these policies.

## Operator and pane control

The operator uses in-process Tauri commands for lifecycle, catalog changes, folder selection, raw terminal input, room membership, and delivery. Windows directories come from the native picker and are identity-checked before spawn. Prime accepts a typed absolute Linux directory qualified by the backend.

The pane sideband uses a Windows named pipe. Its fixed actions are self `ping`, `wait_quiet`, `send_input`, and `send_key`, plus membership-derived `room_read`, `room_post`, and `room_deliver`. It exposes no session creation, inventory, membership mutation, or peer lifecycle action.

Caller identity comes first from the kernel-reported client process and its live pane Job. When the caller is outside all pane Jobs, the supervisor can authenticate its inherited per-run `PRIM1_PANE_SECRET`. A secret identifies only its own run and rotates on restart. An endpoint name alone grants nothing.

Room and sender are derived from that authenticated pane. `room_deliver` accepts a member label, session ID, or `all`; it cannot select an arbitrary room or impersonate another sender. Ambiguous labels fail rather than choosing a member.

Claude Code and Codex receive a session-scoped `prim1_pane` stdio MCP child exposing `ping`, `room_read`, `room_post`, and `room_deliver`. The same executable offers `--prim1-room` commands for non-Prime panes. Both call the same authenticated named pipe. Claude's four pane tools receive a process-local allowlist; other tools retain the selected permission profile.

Prime receives neither pane-sideband credentials nor synthetic delivery. Its terminal remains usable through raw operator input.

## Rooms and messaging

Sessions belong to at most one room. A room may be empty; unassigned sessions are shown in the lobby. Stable room IDs and membership revisions separate identity from editable labels.

**Post to feed** appends a message without writing any terminal. **Send** selects one member or all room members. A pane's `room_deliver` uses the same delivery path with that pane as sender; `all` excludes the sender.

Every room member can read the shared feed, including messages addressed to another member. Addressing controls terminal delivery, not private visibility within the room. New members read only from their join floor onward.

Room delivery pins a membership revision and preflights every selected recipient before committing the message or writing a PTY. If any recipient is refused at preflight, none receives it. Once writing starts, each recipient gets a separate pending/written/failed outcome; failures can leave partial delivery. There is no rollback or automatic replay of the message body.

### Framing and submission

Claude Code, Codex, and Grok receive a visible provenance header and one bracketed-paste frame. The body limit is 1 MiB; unsafe control characters fail validation. A bare shell receives only a validated printable single-line command because a natural-language header would change shell syntax.

A per-run FIFO input permit prevents concurrent writers from interleaving. The supervisor revalidates run identity, PTY, input gate, paste mode, and driver-observed work state before writing. Unknown Codex state and observed unsafe states refuse synthetic delivery while leaving raw terminal input available.

After a complete paste, submission waits for a size-based minimum and a bounded output-quiet window. Codex uses a Right-arrow fence before Enter; Claude and Grok use Enter. The supervisor can repeat the submit gesture when no subsequent output is observed, up to three attempts, without replaying the body. A partial initial write does not trigger submission or replay.

These are writer and output-observation receipts. They do not prove byte-exact interpretation, model understanding, or task completion. See the runtime contract for timing and failure details.

### Room briefing

Each room can enable automatic briefing and customize its text. An owed brief waits until a member can accept it; a completed automatic brief is remembered per membership, not repeated on every run. The operator can explicitly request **Brief now**. The default text is [room_brief.txt](crates/supervisor/src/room_brief.txt).

## Persistence and desktop lifetime

Atomic versioned catalogs store ordered session definitions, qualified directories, permission profiles, stored conversation references and startup metadata, plus room definitions, membership, and briefing preferences/state. Invalid or unsupported catalogs fail visibly instead of being silently replaced.

Room feeds, cursor epochs, live PTYs, and terminal buffers are process-memory state. The bounded room feed holds up to 512 events / 16 MiB and reports cursor gaps after eviction or restart.

A fresh install has no sessions. Creating a session launches its CLI. **Continue where I left off** is enabled by default: previously running sessions relaunch as their room or lobby is entered. Supported drivers resume stored harness conversations in a new PTY; they do not reattach an old process.

Renderer reload can recover the supervisor's live state while the desktop process remains alive. Closing the desktop shuts down managed process scopes. There is no detached background supervisor service.

## Lifecycle and recovery

Lifecycle states are `starting`, `ready`, `busy`, `idle`, `stalled`, `restarting`, `failed`, and `closed`. Driver work state is tracked separately and is not inferred from silence alone for every harness.

Grok readiness comes from recognized full-screen composer/footer output and enabled bracketed paste, excluding launcher/startup states. Codex uses a bounded viewport and prompt/footer recognition, with modal blocks latched until a clean prompt and transient notices cleared by later prompt/activity. Unknown layouts fail closed; no timer alone makes them safe to receive input.

Operator restart terminates the exact old run before replacing it. Optional stalled-session restarts are disabled by default and have generation-bound timers and a restart cap. They do not establish unattended-operation guarantees or provider-outage recovery.

## Security and metadata

The packaged renderer loads bundled assets under CSP with a restricted Tauri capability surface. It has no global Tauri API or in-app devtools. The opt-in `PRIM1_CDP_PORT` is a loopback QA debugging surface.

The runtime directory is private machine-local state and must not overlap a session workspace in either direction. Pane secrets stay out of diagnostics and audit. These controls narrow protocol authority; they do not isolate mutually hostile programs running as the same OS user.

The append-only JSONL audit records lifecycle, authorization, membership, dispatch, and delivery metadata. Raw output events are omitted and room/routed-message bodies are redacted. Startup and failure diagnostics can include harness error text, so inspect logs before sharing them.

## Source layout and design history

The desktop lives in `apps/desktop`; runtime components live under `crates/`; pane scripts and the audit reader live under `scripts/`. Per-directory READMEs describe their responsibilities.

[Architecture decision records](docs/architecture-decisions/) preserve earlier design rationale. Current behavior is described here and in the runtime contract; an old proposal is not a current feature claim.

## Product boundaries

PRIM-1 is a local, Windows desktop product. It does not provide hosted models, remote operation, multi-machine coordination, a task scheduler, usage billing, or a service-manager deployment. See [ROADMAP.md](ROADMAP.md) for future direction.
