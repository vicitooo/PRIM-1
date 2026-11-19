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

Resource telemetry reuses those Jobs, plus the application Job the desktop process enters at startup: every process it starts inherits that Job and cannot break away, so its members are PRIM-1's whole tree, live orphans included. A run's CPU comes from its Job's cumulative user and kernel time, which keeps exited children, and its memory from the private bytes of the Job's live members; the Job's kernel-recorded peak commit bounds its memory peak. The application Job's other members (WebView2, the ConPTY console hosts, any helper) are measured per process. Every member is read through a handle that is rechecked against its Job, so a reused process ID is never charged. Sampling is pull-based: the renderer asks every 2 s while the line or page is visible and the window is known not to be minimized, and the supervisor keeps only the previous sample and each live run's peaks.

Prime adds an Ubuntu WSL user-systemd service with a guard and payload. The supervisor qualifies the Linux directory, reconciles stale services, and checks both the Linux service and outer Windows Job during shutdown. This is a specific integration, not a cross-platform desktop runtime.

## PTY and renderer

PTY output supplies the visible terminal, not authority for control requests. Drivers inspect the unshed stream for terminal modes and work state before output reaches the renderer.

The frontend uses xterm.js for ANSI rendering, scrollback, alternate screens, and resize. It shows one terminal at a time and retains inactive buffers while the app runs. Sessions continue running when another room or tab is selected.

The desktop event queue is bounded. Control/lifecycle events use backpressure; display output can be shed under sustained load with explicit run-scoped gap warnings. A gap means the displayed transcript may be incomplete.

## Drivers

Drivers construct qualified executable launches from typed operator choices for model, effort, permission profile and task brief. The renderer cannot supply an arbitrary program, argument vector or environment. Drivers supply harness-specific permission flags, conversation resumption, readiness, and semantic output classification.

- Claude Code and Codex launch their native Windows CLIs.
- Grok launches its full-screen TUI, with a new session ID or the stored resume ID.
- Prime launches through direct `wsl.exe` and absolute Linux executables under its service guard.
- Generic Terminal launches a shell. If a supported harness is started inside it, the supervisor can adapt delivery framing from process-image detection; this does not add that harness's complete work-state gating.

Normal permissions are the default. Claude Code, Codex, and Grok expose an explicit **Unsafe** profile; Prime and Generic Terminal are Normal-only. PRIM-1 does not add an OS sandbox to these policies.

## Operator and pane control

The operator uses in-process Tauri commands for lifecycle, catalog changes, folder selection, raw terminal input, room membership, and delivery. Windows directories come from the native picker and are identity-checked before spawn. Prime accepts a typed absolute Linux directory qualified by the backend.

Room commands run their supervisor work on blocking workers, including reads that can wait behind a journal sync. Their typed results and retry IDs are unchanged. General bootstrap/session command conversion is tracked separately; this is not a claim that every IPC path is nonblocking. Pane room requests also authorize inside their blocking worker, then revalidate the same run and current membership in the operation; reads derive their join floor after queueing.

Joint room/session scopes acquire `rooms` before `slots` and release `slots` before room journal or catalog I/O. Membership changes use room publication; session deletion holds that publication guard from its membership check through catalog deletion, releasing `rooms` before the session write. Room callbacks run after releasing `rooms`. `move_room` may acquire shutdown through its snapshot while holding publication, so lifecycle paths must not acquire publication while holding shutdown. Run-input and activity guards never surround room persistence.

The pane sideband uses a Windows named pipe. Its actions are self `ping`, `wait_quiet`, `send_input`, and `send_key`, membership-derived `room_read`, `room_post`, and `room_deliver`, runtime-wide `tasks` (GraphQL over the task folder; no room membership, changes attributed to the calling run), plus the Codex-only `identify_codex` report. It exposes no session creation, inventory, membership mutation, or peer lifecycle action, except A109 room mobility for a member the operator lets roam: `rooms_list`, `room_peek` (any feed, no receipt), `room_join`/`room_leave` (only the caller's own membership, through the operator's add/remove steps, a move committed as one room-catalog write, with a notice in each room) and `room_deliver_to` (a member of another room), A108 `upgrade_request` for a member granted `maintain` (one same-runtime restart on a staged build after a cancellable notice; the supervisor captures the running set itself), and A110 `member` for a member granted `manage` (below). Each rechecks the caller's run and grant after its waits, under the locks its effect commits under. The grants (`roam`, `maintain`, `manage`) are stored with the session, changed only by the operator, and refused for Prime and Terminal panes. A110: a member with `manage` sends one `member` request per lifecycle action on another member of its room (another room also needs `roam`): create, start, stop, restart, brief or remove. Each calls the supervisor method behind the operator's desktop command, with the caller's run, generation and grants rechecked inside the critical section where that method commits; it is keyed by a caller UUID whose retry returns the first result (in memory, plus the creating request stored on the new session) and posts a room notice. It grants nothing and never acts on the caller. While PRIM-1 is closing for an upgrade, create, start and restart are refused on this path exactly as on the operator's.

Caller identity comes first from the kernel-reported client process and its live pane Job. When the caller is outside all pane Jobs, the supervisor can authenticate its inherited per-run `PRIM1_PANE_SECRET`. A secret identifies only its own run and rotates on restart. An endpoint name alone grants nothing.

The `--prim1-identify` shell helper takes no arguments and reads its own `CODEX_THREAD_ID`. Its `identify_codex` request requires native Job/run attestation; secret-only callers are refused. The supervisor checks the exact root rollout and directory, persists provisional identity evidence, and retains full superseded recovery records. A report alone does not prove current-conversation freshness or permit resume/native wake; the conversation follower (below) releases resume only when the reported thread is also a root writer lock held by the member's own `codex.exe`. MCP exposes no identify tool accepting an ID.

Room and sender are derived from that authenticated pane. `room_deliver` accepts a member label, session ID, or `all`; it cannot select an arbitrary room or impersonate another sender. Ambiguous labels fail rather than choosing a member. Only the separate roam-gated `room_deliver_to` names a room; its sender is still the calling pane, headed `<label> (visiting from <room>)`. A `room_id` needs roam even when it names the caller's own room. The one other cross-room path is an answer: a `room_deliver` label that matches no member of the caller's room (an ambiguous label or the caller itself stays refused) reaches a roaming member that delivered to the caller across rooms within 24 hours (kept in memory; a restart ends it), in that member's current room, while it still roams; the caller needs no roam, and every condition is rechecked at commit.

Claude Code and Codex receive a session-scoped `prim1_pane` stdio MCP child exposing `ping`, `room_read`, `room_post`, `post_card`, `room_deliver`, and `tasks`, plus the four roaming tools, and instructions describing them, when the supervisor confirms the roam grant at initialize, and likewise the six `member_*` tools for the manage grant; a grant changed while the member runs is rechecked after its calls and announced with `notifications/tools/list_changed`. The same executable offers `--prim1-room` commands for non-Prime panes. Both call the same authenticated named pipe. Claude's `ping`, `room_read`, `room_post`, `room_deliver`, `tasks`, roaming and member tools receive a process-local allowlist (`post_card` does not); other tools retain the selected permission profile.

Prime receives neither pane-sideband credentials nor synthetic delivery. Its terminal remains usable through raw operator input.

## Rooms and messaging

Sessions belong to at most one room. A room may be empty; unassigned sessions are shown in the lobby. Stable room IDs and membership revisions separate identity from editable labels.

**Post to feed** appends a message without writing any terminal. **Send** selects one member or all room members. A pane's `room_deliver` uses the same delivery path with that pane as sender; `all` excludes the sender.

Every room member can read the shared feed, including messages addressed to another member. Addressing controls terminal delivery, not private visibility within the room. New members read only from their join floor onward.

Room delivery commits the body and authorized recipient snapshot first, then independently attempts a short wake. Each recipient has its own Stored/Pending/Written/Held/Failed/Fetched receipt; a refusal does not suppress others. Eligible zero-write wakes can be retried explicitly by original message ID and receipt cursor. Partial/unknown effects are never automatically replayed.

### Room wake pointer and submission

The full message stays in the durable feed. A wake contains a control-free ASCII message-ID pointer, sender abbreviation and bounded preview that fits one measured composer row. Only the measured Grok fullscreen layout currently admits a wake; Claude, native Codex, Prime, Generic Terminal and unrecognized layouts remain Held. The recipient reads the body through `room_read`.

The room adapter requires a continuously observed empty composer, trusted surrounding rows, a visible cursor and completed synchronized redraw. It rechecks the exact run, paste mode and raw-input epoch before paste and before exactly one Enter. Submission additionally requires the exact pointer echoed in a newer completed frame. Missing or changed echo leaves a partial, non-repeatable attempt; silence never causes another Enter. These rules apply to room Send, Brief and Retry.

### Legacy direct routing

The legacy direct routing path below is separate from room wakes; its full-body framing and repeated submit gestures do not apply to room Send, Brief or Retry.

Claude Code, Codex, and Grok receive a visible provenance header and one bracketed-paste frame. The body limit is 1 MiB; unsafe control characters fail validation. A bare shell receives only a validated printable single-line command because a natural-language header would change shell syntax.

A per-run FIFO input permit prevents concurrent writers from interleaving. The supervisor revalidates run identity, PTY, input gate, paste mode, and driver-observed work state before writing. Unknown Codex state and observed unsafe states refuse synthetic delivery while leaving raw terminal input available.

After a complete paste, submission waits for a size-based minimum and a bounded output-quiet window. Codex uses a Right-arrow fence before Enter; Claude and Grok use Enter. The supervisor can repeat the submit gesture when no subsequent output is observed, up to three attempts, without replaying the body. A partial initial write does not trigger submission or replay.

These are writer and output-observation receipts. They do not prove byte-exact interpretation, model understanding, or task completion. See the runtime contract for timing and failure details.

### Room briefing

Sessions choose the room default, a custom task brief, or None. Only a new conversation (explicit Fresh or no stored resume reference) receives a brief, as one final literal argv operand after `--`. An actual resume receives no task prompt. Claude prefixes the operand with `PRIM-1 initial brief:\n\n`; the source text remains intact. Source text is limited to 8192 bytes with no NUL, plus that fixed Claude prefix. Room membership and idle transitions never inject a brief into a PTY, and old membership-completion fields have no delivery effect. **Brief now** stores a new brief and attempts a wake through the room Send path, returning its real result. Terminal and Prime reject all briefing. The default text is [room_brief.txt](crates/supervisor/src/room_brief.txt).

None means no task brief. MCP initialization supplies room protocol where MCP is configured for Claude/Codex; this source contract does not verify Codex's perception. Grok's room CLI supplies no automatic protocol text with None.

## Persistence and desktop lifetime

Atomic private catalogs store ordered session definitions, qualified directories, permission profiles, requested launch choices including custom brief text, conversation references, full recovery history and startup metadata. They also retain provisional Codex run bindings and known inputs from the last successful launch: resolved program/shim arguments, typed choices and PRIM's MCP override metadata, without prompt or environment values in those launch records. Persisted run IDs are historical evidence, not authority over a live run. Room catalogs store definitions, membership, restart selection and briefing preferences; completed-brief state is legacy compatibility data. Invalid or unsupported catalogs fail visibly instead of being silently replaced.

Room messages and receipt cursors persist in synced per-room JSONL. The replayed display window holds up to 512 events / 16 MiB and reports explicit eviction gaps; restart alone preserves its epoch. Live PTYs and terminal buffers remain process-memory state. Corrupt room history is isolated without rewriting the original log.

Recovery of an owned poisoned writer retains its original lock and acknowledged prefix while a cloned handle replays and syncs a private candidate outside the shared room registry. Publication serializes definition changes and final installation. Transient I/O failures retain a bounded retryable cause; structural history failures latch a writer-only hold without repeated scans. Previously acknowledged reads remain available in that case. Startup corruption still refuses reads because no validated prior prefix exists. Snapshot invalidation occurs on state/cause changes, not on every repeated refusal.

A fresh install has no sessions. Creating a session launches its CLI. The backend persists explicit per-room **Resume on restart**, default false; room visits and historical running flags do not select it. Frontend startup-policy integration remains tracked in S5. Supported drivers resume stored harness conversations in a new PTY; they do not reattach an old process. Codex resume remains held until root identity and current-conversation freshness are proven; Fresh retains prior recovery references.

A conversation follower thread (A108 W2, about every 3 s) proves each running Claude, Codex and Grok member's current conversation from harness evidence bound to the launched process's pid and start time, inside the pane's Job: Claude's `sessions\<pid>.json` (this machine, the member's folder, `interactive`); the Codex writer locks the member's `codex.exe` holds (the file system's list of processes with each lock open, asked only when the lock folder changes and at least once a minute; unlike a Restart Manager session it writes no event-log entries) plus a root CLI rollout for the folder (a missing or half-written rollout waits); Grok's `session created`/`session loaded` log lines for that pid, confirmed by the session's `summary.json` (never a subagent). A proven change is persisted to the catalog, so Launch resumes it; a Codex proof releases only the launch and identify-report holds. With more than one held Codex root (after `/new` the old thread stays loaded), the member is ambiguous and nothing changes until `--prim1-identify` names one of them (a report made after every other held thread appeared) or the older one is released. A lone root never replaces this run's report while the process still holds the reported thread, nor while it holds a thread not yet written. A run whose process changes behind its pid, or started after the launch, is no longer followed. Harness files are read with no supervisor lock held.

Renderer reload can recover the supervisor's live state while the desktop process remains alive. Closing the desktop shuts down managed process scopes. There is no detached background supervisor service.

## Lifecycle and recovery

Lifecycle states are `starting`, `ready`, `busy`, `idle`, `stalled`, `restarting`, `failed`, and `closed`. Driver work state is tracked separately and is not inferred from silence alone for every harness. A question on a member's screen (a startup trust question, an approval) leaves its lifecycle at `ready` or `idle`; while its work state is Blocked or an error loop, the session snapshot's `blocked_reason` carries the plain operator line, and the member card and pane header show Blocked with it instead of ready. For Claude a question is also read off the composed screen with the driver's own classifier: before its first state, at the run's install (a screen painted before the spawn was confirmed), and when its output goes quiet (mid-run only under a selection menu with its hint, so quoted transcript text never makes one), so a dialog split across output chunks is still a question.

Grok readiness comes from recognized full-screen composer/footer output and enabled bracketed paste, excluding launcher/startup states. Codex uses a bounded viewport and prompt/footer recognition, with modal blocks latched until a clean prompt and transient notices cleared by later prompt/activity. Unknown layouts fail closed; no timer alone makes them safe to receive input.

Operator restart terminates the exact old run before replacing it. Optional stalled-session restarts are disabled by default and have generation-bound timers and a restart cap. They do not establish unattended-operation guarantees or provider-outage recovery.

## Security and metadata

The packaged renderer loads bundled assets under CSP with a restricted Tauri capability surface. It has no global Tauri API or in-app devtools. WebView2 remote debugging is off by default. The opt-in `PRIM1_DEBUG_CDP_PORT` is a temporary loopback QA debugging surface. The desktop process removes an inherited `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` when it is not set, and panes never inherit that switch or either port variable.

The runtime directory is private machine-local state and must not overlap a session workspace in either direction. Pane secrets stay out of diagnostics and audit. These controls narrow protocol authority; they do not isolate mutually hostile programs running as the same OS user.

The append-only JSONL audit records lifecycle, authorization, membership, dispatch, and delivery metadata. Raw output events are omitted and room/routed-message bodies are redacted. Startup and failure diagnostics can include harness error text, so inspect logs before sharing them.

## Source layout and design history

The desktop lives in `apps/desktop`; runtime components live under `crates/`; pane scripts and the audit reader live under `scripts/`. Per-directory READMEs describe their responsibilities.

[Architecture decision records](docs/architecture-decisions/) preserve earlier design rationale. Current behavior is described here and in the runtime contract; an old proposal is not a current feature claim.

## Product boundaries

PRIM-1 is a local, Windows desktop product. It does not provide hosted models, remote operation, multi-machine coordination, a task scheduler, usage billing, or a service-manager deployment. See [ROADMAP.md](ROADMAP.md) for future direction.
