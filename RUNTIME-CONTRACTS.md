# PRIM-1 — Runtime Contracts

The current runtime behavior and its boundaries. See [ARCHITECTURE.md](ARCHITECTURE.md) for the component overview and [CONTROL-SURFACE.md](CONTROL-SURFACE.md) for commands.

## 1. Purpose

This document defines the contracts the runtime must obey:

- how sessions are identified and labelled
- how messages are shaped
- how routing works
- how lifecycle is represented
- what gets logged
- what permissions exist

These contracts should be implemented explicitly, not inferred from ad-hoc code paths.

## 2. Session model

Every supervised logical session has a stable opaque `SessionId`, one mutable
human-readable label, an immutable UUID-derived internal alias, a driver,
qualified working directory, permission profile, lifecycle state, generation,
and optional live process identity. Each launched run receives a fresh opaque
`RunId`; restart preserves the `SessionId` and advances the run lineage. Delete
followed by same-label recreation produces a new identity, so stale IDs fail
closed.

Operator lifecycle, input, key, resize, and route requests use `SessionId`.
Labels are never operator authority. The ordered registry is keyed by
`SessionId`; duplicate labels are permitted. The derived alias is used only for
the narrow self-pane sideband and internal terminal provenance.

The private version-1 catalog atomically persists one workspace preference plus
ordered session intent: `SessionId`, label, driver, qualified cwd identity, and
permission profile, requested model/effort/brief choices, stored conversation
references, full recovery history, and whether a session was running at shutdown.
Custom brief text is stored privately for editing. Provisional Codex identity
evidence includes its reported thread ID, attested run/generation, rollout path
and report time. The last successful launch record retains its run/generation,
resolved program and shim prefix, typed choices, and PRIM's injected MCP metadata.
These are historical inputs, not live-run authority or observed effective settings.
Launch records exclude the task prompt and environment values; the editable
custom brief is a separate choice field. The catalog does not retain live PIDs,
terminal output or room-message bodies. A fresh catalog contains zero sessions.
Corrupt or unknown versions fail visibly without default replacement.

The renderer may request typed model, effort, permission and brief choices;
backend validation and driver code construct the launch. It cannot supply an
arbitrary program, argument vector or environment. Native working-directory
paths still enter through the backend picker.

Qualified cwd identity is namespaced. Native sessions persist a Windows final
path identity selected through the native chooser. Prime persists a canonical
path plus device/inode identity in the typed `wsl:Ubuntu` namespace. A catalog
whose driver and cwd namespace disagree fails startup without rewrite.

## 3. Addressing

The production operator routing boundary accepts exactly one `recipient_id` and
content. The desktop derives `from = "operator"`; callers cannot supply
provenance, scope, names, or a recipient list. The visible room composer uses a
separate typed `RoomId` request with either one member `SessionId` or explicit
`all`; there is no unknown-target, name-derived, implicit, or all-session
broadcast fallback.

The pane sideband exposes `room_read`, `room_post`, and `room_deliver`. Requests
carry neither a `RoomId` nor sender: the supervisor derives both from the
authenticated live run and its current membership. Delivery accepts a member
label, session ID, or `all` (every other member); ambiguous labels fail closed.
Identity comes from native Job membership or, for callers outside all pane Jobs,
the per-run pane secret. A supplied recipient never grants cross-room authority.
Cross-room authority comes only from the operator's per-session `roam` grant
(A109), checked on every request and again at its commit: the separate
`room_deliver_to`, `rooms_list`, `room_peek`, `room_join` and `room_leave`
requests carry a `RoomId` and are refused without it; the sender is still the
authenticated pane; a `RoomId` needs roam even when it names the caller's own
room. The one exception is an answer to a roaming sender: a `room_deliver`
label or session ID (without `RoomId`) that matches no member of the caller's
room, and is neither ambiguous there nor the caller, reaches a member that
delivered to the caller across rooms within 24 hours (in-memory record; a
restart ends it), in its current room, while it still exists and roams; these
conditions are rechecked at commit. A `room_join` that leaves a room
commits the leave and the join as one room-catalog write. An
`upgrade_request` (A108) needs the caller's `maintain` grant; it names only the
build and the caller's task checkpoint, and the supervisor captures the running
set, conversations and rooms itself.
A `member` request (A110) needs the operator's `manage` grant, and `roam`
as well for a room other than the caller's; it names its target by session ID or
a label unique in the caller's room, never the caller, carries a caller UUID
`request_id` and no permission field, and is rechecked where its effect commits.

## 4. Room/feed envelope

The private room catalog atomically persists ordered `RoomId`, label, member
`SessionId` values, a nonzero membership revision, explicit restart selection, briefing preferences
and per-member join floors. Legacy completed-brief membership marks remain
readable for catalog compatibility; automatic PTY briefing is removed.
A separate single-writer JSONL per room persists the feed epoch, monotonic
sequence, messages and delivery receipts. Each complete record is synced before
success; replay restores the bounded feed and idempotency horizon. Corrupt
complete records or unknown schemas isolate the affected room without rewriting
its log or blocking healthy rooms. `RoomSnapshot.recovery_error` exposes the
reason; that room refuses feed reads and writes/wakes until recovery and reopen.
Its validated header epoch is retained; an unreadable header has a nil (unknown)
epoch, never a generated replacement. Member terminals remain available. Only
an unterminated final record is quarantined and truncated before replay continues.

Room wake attempts sync a Pending receipt before any input write. An operator
retry names the retained message, one original recipient and the latest receipt
cursor; that cursor is compared under the room publication lock before replacing
it with Pending. Repeated/stale cursors return the current result. Only Held with
zero bytes or Failed with zero bytes plus `zero_write_confirmed: true` can retry;
legacy zero-byte counts alone are not proof. Current membership/join floor and
the previous run binding are rechecked. Room deletion and member removal refuse
while an admitted delivery holds its lease. Receipt persistence failure never
turns an uncertain attempt into a safe automatic retry.

A per-run gate coalesces concurrent room wake attempts. It is claimed under the
room publication lock before Pending is synced. A refused or unsynced claim
releases the gate before any transport is attempted, while a complete uncertain
Pending remains non-repeatable for its original intent. Once transport may have
begun, only a durable final outcome releases the gate; an unknown attempt keeps
the run held. Another message still
commits its body and original targets, then records Held for that recipient.
Other recipients proceed independently. The gate holds no payload and is
independent of Fetched, feed eviction and the dedup horizon. Replacing a run
creates a fresh gate without retargeting or replaying any old unknown attempt.

Each room owns a persistent random log epoch and monotonic sequence. Its in-memory feed
retains at most 512 events and 16 MiB; reads return at most 64 events and 2 MiB
per page. A stale epoch or evicted range returns an explicit gap. The desktop
retains only the active room's same-sized bounded display window and reloads an
inactive room from the authoritative supervisor feed when selected.

Example:

```json
{
  "schema_version": 1,
  "room_id": "uuid",
  "cursor": { "epoch": "uuid", "sequence": 14 },
  "item": {
    "kind": "message",
    "request_id": "caller-intent-id",
    "message_id": "uuid",
    "sender": { "kind": "session", "session_id": "uuid" },
    "content": "Please review the last change.",
    "recipient_ids": [],
    "membership_revision": 3
  },
  "timestamp": "2026-08-11T02:00:00Z"
}
```

Feed events and pages carry `schema_version: 1`; an unsupported version fails
closed rather than being interpreted as a known shape. `recipient_ids: []` is
feed-only. An addressed message records the exact
recipient snapshot before any readiness check or wake. Each target then receives
an independent `written`, `held` or `failed` outcome. `fetched` records inclusion
of the addressed message in a recipient's read response, never cursor advancement
alone or model acceptance. UI emission is never a model receipt.

Optional caller `request_id` keys deduplicate the last 1024 posts or 16 MiB of
retained post records, whichever is reached first. Each sender has its own key
namespace within the room. The same sender and key with changed content or
recipients conflicts; matching retries return the original message
without another wake. Ordinary log growth stops at 252 MiB, leaving 4 MiB within
the 256 MiB hard cap for admitted wake completions and private recovery receipts.
Fetched and new claims cannot spend that reserve. Capacity refusal performs no
new wake; an older log whose required repair cannot fit stays held without
publishing the incomplete repair or silently deleting history. See README-AI.md for failure and retry
boundaries, including uncertain wake receipts and renderer-local retry keys.

Room membership belongs to the durable session definition, not to one live
run: closed members remain members. The authorized message is stored first;
each wake independently requires a compatible live run. One held recipient does
not block another. Zero- and
one-member rooms are ordinary workspaces: operator
delivery needs at least one member — an empty room refuses with the plain
reason — and a pane's `all` needs at least one other member. Every room
member can read an addressed message from the shared feed even when only one
member was selected for PTY delivery. The operator is not a room member, so
**Send All** means every member in the pinned revision; a pane's `room_deliver`
`all` means every *other* member. Pane sideband callers read, post, and deliver
only within their current room, always as themselves.

## 5. Current typed surfaces

- In-process operator actions own `SessionId`-addressed create, rename, reorder,
  closed-only delete, lifecycle, input, inventory, resize, native cwd selection,
  typed Prime Linux cwd selection, and typed permission changes.
- In-process room actions own room catalog/membership mutations, operator feed
  posts, and explicit one-member / Send All delivery for supported recipients.
- The pane-local sideband owns self `ping`, `wait_quiet`, `send_input`,
  `send_key`, plus membership-derived `room_read`, feed-only `room_post`, and
  `room_deliver` through the same delivery path as the operator. Codex's
  `identify_codex` request records provisional identity for the attested caller;
  it grants no lifecycle or peer authority.
- Runtime telemetry owns structured session/room catalog, room feed, lifecycle,
  delivery, heartbeat, alert, and metadata-only dispatch events.
- Resource telemetry (`telemetry_snapshot`) is a read-only, pull-based sample
  of the application Job, which holds every process PRIM-1 started. Each member
  belongs to exactly one group: the PRIM-1 process, one live run's Job,
  WebView2, or the remaining helpers. Members are read through handles
  rechecked against their Job. Listed process IDs with a failed read in either
  inventory are counted in `unreadable_processes`; memory/process counts may
  be incomplete, while run Job CPU still includes unreadable members. Such
  IDs are conservatively excluded from helper attribution, never treated as
  proof of ownership. CPU is a share
  of all logical CPUs since the previous sample and is null without a baseline
  0.5 to 10 s old; memory is private bytes. Peaks belong to a run and end with
  it.

Spawn, restart, close, and arbitrary command execution are not pane messages.
Room feed item kinds are `message`, `membership`, and `delivery`. Any additional
kind requires a new explicit contract and authority model.

## 6. Visible injection contract

Room notification is a separate adapter from the legacy direct routing below.
The complete body stays in the durable room feed. A room wake contains an ASCII
message-ID pointer, sender abbreviation and bounded preview fitting one input
row. Only the measured Grok fullscreen composer currently has positive admission
evidence. Claude, native Codex, Prime, Generic Terminal and unrecognized layouts
remain held; there is no blind PTY fallback.

The continuously maintained composer projection requires trusted geometry,
an empty draft, visible cursor and completed synchronized redraw. Observed busy
work stays held until explicit completion output, independent of quiescence
timers. Raw input increments its per-run epoch before waiting for a writer and
invalidates the pre-input projection. The room writer checks that epoch before
paste and again before exactly one Enter, also revalidating run/paste mode and
requiring an exact echo in a newer complete frame. Missing/changed echo or input
after paste never causes a second Enter and is recorded as a non-repeatable
partial write. A test mock echo is not real harness acceptance evidence.

### Legacy direct routing

The framing and writer boundaries below describe legacy direct routing. Its
full-body framing and repeated submit gestures do not apply to room Send,
Brief or Retry, which use the room pointer adapter above.

Claude Code, Codex, and Grok receive a visible provenance header inside one
bracketed-paste payload. Grok's active driver launches full-screen; historical
minimal-mode framing remains an internal tested path, not the current launch
or delivery contract.

A bare Generic Terminal receives only the source command, raw and single-line.
A natural-language header would change executable shell syntax. When a supported
harness is detected inside that terminal, the supervisor selects its framing
and header instead. This process-image heuristic does not provide the typed
driver's complete work-state tracking. Prime accepts raw terminal input only
and is rejected before synthetic route events or writes.

Validation and writer boundaries:

- Message bodies have a 1 MiB limit. C0, C1, and DEL controls other than tab and
  source line endings fail validation. Bare-shell delivery further requires a
  printable single-line command.
- The supervisor does not trim, flatten, or silently truncate source bytes
  received at its request boundary. This does not establish clipboard-to-WebView
  fidelity or final-child interpretation.
- Whole-message preflight resolves every selected recipient before any route
  message is committed or any PTY is written.
- One logical message holds one exact run's FIFO input permit through paste
  and submission. Other input cannot interleave inside that operation.
- Before the paste, the supervisor revalidates session, run, generation, PTY,
  input gate, enabled bracketed-paste mode, and observed work state. Known
  blocked/error-loop states and unobserved Codex state refuse delivery.
- Driver trackers consume the unshed output stream. Their state is reset for
  each run; a replacement cannot inherit readiness or paste-mode authority.
- After a complete paste, submission waits at least one second plus 500 ms per
  MiB of framed input, then for an output-quiet window of 400 ms, bounded at
  ten seconds. Quiet is a pacing hint, not paste acknowledgement.
- Before submission, exact run/PTY/gate and safe work state are checked again.
  Paste mode need not still be enabled: a harness may disable it after consuming
  the frame.
- Claude and Grok submit with Enter. Codex uses Right-arrow followed by Enter
  to cross its pasted-content guard. If no subsequent output is observed within
  three seconds, the submit gesture can be repeated, up to three attempts total.
  Only the gesture is repeated, never the message body. Output advancement is
  not proof of semantic acceptance.
- A partial or failed initial payload write records the accepted prefix and
  sends no submit gesture or automatic body replay. Later failures produce
  per-recipient results; successful deliveries cannot be rolled back.
- A `written` receipt proves the PTY writer's outcome, not model understanding,
  task completion, or byte-exact final-child receipt. Inspect the response or
  resulting artifact for those outcomes.

### Raw input and quiet waits

Raw operator and pane-sideband input remain available when synthetic delivery
is refused, so a person can resolve trust, authentication, or approval prompts.

Pane-sideband `send_input` and `send_key` have a 20-second server write budget.
On expiry, isolated PTY-input cancellation gets up to five seconds under a
per-run barrier. If the writer cannot be stopped in isolation, only that exact
run is invalidated and terminated (up to five seconds), followed by up to five
seconds to join the writer. Every over-budget response sets `timed_out: true`.
`ok: true` is possible if the complete write finished after the deadline and
warns against retry. The PowerShell client allows 45 seconds for the response.

`wait_quiet` accepts a quiet window of 1–60 seconds and a timeout of 1–300 seconds,
with the quiet window no greater than the timeout. Silence is not completion.

## 7. Lifecycle contract

Lifecycle states are `starting`, `ready`, `busy`, `idle`, `stalled`, `restarting`,
`failed`, and `closed`.

Work-state observations are separate: `idle`, `thinking`, `tool_call`, `blocked`,
and `error_loop`. Unknown state is not a positive idle observation.

- `session_work_state` carries a nested `RunEventIdentity` (session, run,
  generation, monotonic sequence), label, current/previous state, optional
  detail, and timestamp. The first explicit observation emits an event;
  repeated identical observations update memory without another audit event.
- Output quiescence can return Claude and Generic Terminal to idle. For Codex,
  it cannot establish semantic idle or routed-input authority. Grok uses
  recognized semantic markers rather than ordinary silence.
- Grok launches with `--fullscreen` and a new or resumed conversation ID. It
  stays starting through launcher/startup output. Readiness requires recognized
  composer/footer output and bracketed-paste support, from the bounded viewport
  or the raw-stream readiness tracker. No timeout grants readiness.
- Codex's bounded viewport recognizes a trusted prompt and model/cwd footer
  across output chunks and repaint transactions. Modal blocks remain latched
  until a clean prompt; transient notices are detected from fresh output and
  clear on a later clean prompt or activity. Stale transcript text is not
  repeatedly reclassified as a new notice.
- Incomplete or unfamiliar terminal layouts can remain unknown. Resizing
  invalidates tracked screen evidence until output reconstructs it; a resize
  to dimensions already applied is a no-op.
- Repeated blocked observations with the same detail can escalate to an error
  loop. Work-state hints do not establish process exit or task completion.
- PTY status, OS process state, and owned process scopes determine lifecycle.
  `closed` requires proof that the exact run's Job is empty; EOF, disappearance,
  or handle drop alone is insufficient.
- Start, stop, restart, retirement, and delete serialize through a per-session
  lifecycle reservation. Failed or timed-out termination retains exact ownership
  in a failed/termination-uncertain state and blocks start, restart, and delete.
  A later termination attempt must prove the scope empty before closure.
- Shutdown has bounded per-run cleanup and returns failure when any owned scope
  cannot be proved terminated. Exit-time Job cleanup is a backstop, not an
  in-process termination receipt.

Prime lifecycle adds a second ownership proof:

- Prime is supported only through the Ubuntu WSL distribution and `Normal`
  permission profile; launch uses direct `wsl.exe` plus absolute Linux
  `systemd-run`, Python, and `prime-agent` paths without a shell
- the create form obtains the qualified Ubuntu home from the backend by default;
  an explicit Linux path is canonicalized and bound to device/inode identity,
  then revalidated before launch and once more by the immutable guard immediately
  before it spawns Prime
- startup enumerates and terminates every exact-prefix stale Prime transient
  service before Prime create/start is admitted; a reconciliation failure blocks
  Prime while leaving native sessions available
- provisional PTY output remains visible while launch is being checked but cannot
  mark the run `ready` or update semantic work state; `ready` requires the exact
  transient service to be active with both guard and payload tasks
- `closed` for Prime requires proof that the Linux service has no tasks and that
  the outer Windows ConPTY job is empty; any failed proof retains exact ownership
  in `failed`/termination-uncertain state
- Prime receives neither native pane-sideband authority nor synthetic routed
  delivery because no verified Windows-job-to-Linux-task caller bridge or Prime
  submit protocol exists

Supervisor default events:

- `supervisor_heartbeat` is emitted automatically every `PRIM1_HEARTBEAT_INTERVAL_SECS` seconds; default is `1800`
- heartbeat payload fields are `wrapper_pid`, `uptime_secs`, `sessions`, and `timestamp`
- each heartbeat session summary carries `name`, `lifecycle_state`, optional `work_state`, optional `process_id`, and optional `last_activity_at`
- `supervisor_alert` is the wrapper-owned operator attention channel; fields are `alert_type`, optional `request_id`, optional `session`, optional `action`, optional `last_work_state`, optional `last_session_state`, `message`, `severity`, and `timestamp`
- alert types are `session_stall_detected` and `operator_attention`
- alert severities are `info`, `warn`, and `critical`

Exit-cause events:

- `session_exit` captures why a supervised process stopped separately from UI-compatible lifecycle state
- each event carries a nested `RunEventIdentity`, the current session label, process ID, exit code, signal, success, reason, requested flag, and timestamp
- `clean_exit`: the process exited on its own with code `0` / success and no signal indicator
- `crash_exit`: the process exited on its own with a non-zero code, unsuccessful status, or signal indicator
- `operator_stop`: a stop request intentionally terminated the current generation
- `restart_stop`: a restart request intentionally terminated the old generation before the new start
- `pty_error`: the PTY transport failed and process state is unknown
- `process_disappeared`: liveness pruning or PTY close found no usable exit status
- `requested` is `true` only for `operator_stop` and `restart_stop`
- `session_exit` is co-emitted with the existing `session_state` event for UI compatibility; both events use the same `session` and `timestamp`

## 8. Restart contract

The pane-local sideband exposes no peer restart action. Operator restart requests
remain in-process desktop actions executed by the supervisor; this contract does
not claim hostile same-user OS isolation.

Restart flow:

1. operator issues a request, or an enabled bounded auto-restart policy triggers
2. supervisor validates policy
3. supervisor logs request
4. supervisor performs restart
5. supervisor emits resulting lifecycle/system events

Creating a session launches it. The backend persists explicit per-room **Resume
on restart**, default false; room visits and historical running flags do not
select it. Frontend startup-policy integration remains tracked in S5. A new PTY
uses a stored harness conversation reference where supported; it never reattaches
a previous process. Codex resume and Restart remain held until root identity and
current-conversation freshness are proven. While a member runs, the conversation
follower records its proven current conversation (Claude pid file, Codex writer
lock held by the member's own `codex.exe` plus a root rollout, Grok create/load
log line plus `summary.json`, each bound to the launched pid and start time) in
the catalog; a Codex proof releases only the launch and identify-report holds.
Two held Codex roots are ambiguous and change nothing; a lone root waits while
the process also holds this run's reported thread or an unwritten one. **Start fresh session** preserves
superseded recovery references and their provenance in history. The durable
room feed replays separately from conversation resumption; terminal buffers
are not restored.

A task brief is optional and belongs only to a new conversation: explicit
`fresh=true`, or a launch with no stored resume reference. Actual resume passes
no task prompt. The brief is one literal final argv operand after `--` and all
driver/MCP flags; it is never shell code. Claude uses the ordinary user-prompt
envelope `PRIM-1 initial brief:\n\n{text}` to avoid subcommand dispatch while
preserving the supplied text. Brief source text is limited to 8192 bytes with
no NUL; the fixed Claude prefix is additional.

Room joins and idle transitions never inject a brief into a PTY. The legacy
`brief_on_join` setting enables the RoomDefault choice for a new conversation;
completed-membership fields do not control it. **Brief now** remains a separate
explicit Send-path request with an actual delivery result. Terminal and Prime
reject both initial and explicit briefing. None means no task brief; MCP
initialization supplies room protocol where configured (Claude/Codex), without
claiming verified Codex perception. Grok's room CLI adds no protocol text when
None is selected.

## 9. Permission contract

Current pane authority is deliberately narrower than operator room authority:

- the supervisor derives the caller from live process-job membership and run
  generation, or — only when the client process is in no pane Job — from the
  per-run pane secret (`PRIM1_PANE_SECRET`) presented as the connection preamble
- the pane-local sideband action set includes self `ping`, `wait_quiet`,
  `send_input`, `send_key`, membership-derived `room_read`, feed-only
  `room_post`, and `room_deliver` (recipient = member label / session id /
  `all` = every other member; the sender is always the caller), plus the
  Codex-only `identify_codex` identity report
- `PRIM1_CLI --prim1-identify` accepts no arguments and reads its own
  `CODEX_THREAD_ID`; there is no MCP identify tool or supported ID override.
  The supervisor requires native Windows Job/run attestation and checks the
  exact root rollout and directory. A pane secret alone is insufficient.
  Accepted reports persist provisional run-bound evidence and full superseded
  recovery history; a report alone never clears the freshness hold or enables
  native wake, and is not proof that `/new` has not changed the active
  conversation. The conversation follower releases resume when the reported
  thread is also a root writer lock held by the member's own `codex.exe` and
  the report postdates every other thread that process holds
- pane callers cannot supply `RoomId`, sender, peer, inventory, membership, or
  lifecycle authority; a presented secret is proof of the caller's own pane only
- Claude Code and Codex receive a session-scoped `prim1_pane` stdio MCP child
  exposing `ping`, `room_read`, `room_post`, `room_deliver`; every other
  non-Prime pane has the same surface through `PRIM1_CLI --prim1-room …`; the
  MCP/CLI layer contributes no authority and every call is re-authorized
  through the named pipe
- Claude's four fully qualified pane-MCP tool names are allowed only for that
  process; no wildcard or persistent user/workspace permission is installed,
  and all non-pane tools retain the selected harness permission policy
- Grok receives no MCP configuration. Its shell tools can use the executable's
  room CLI and inherited per-run secret when outside all pane Jobs. No global
  plugin configuration or redirected harness-history directory is required.

- Prime/WSL callers have no pane-sideband surface; the native Job-derived caller
  proof cannot identify Linux tasks and no bearer fallback exists
- operator lifecycle, input, resize, and routing authority remains inside the desktop process and targets stable `SessionId` values

## 10. Metadata audit contract

The audit log is the source of truth for durable operational metadata. It is not a terminal transcript, message archive, or task-completion oracle.

Format:

- JSONL
- one event per line
- rolling files by date

Every record carries an event type and timestamp. Event-specific metadata may add session/actor/target identity, request or route IDs, lifecycle/work state, action/phase/result, error classification, and delivery counts.

`session_work_state`, `session_exit`, `supervisor_heartbeat`, and `supervisor_alert` events are part of the metadata event stream. Work-state events are derived from driver classifiers, not from explicit pane requests. Exit events are derived from PTY exit status, requested-stop intent, PTY transport errors, and liveness pruning. Supervisor default events are emitted by wrapper policy so operators do not need a separate heartbeat monitor.

Content-retention rules:

- `session_output` is live-only and is never appended to the durable audit
- the desktop ingress queue holds exactly 512 events; lifecycle/control events are lossless and apply bounded backpressure, while only sanitized `session_output` display events may be shed
- every shed output event is counted in a visible, run-scoped gap warning emitted before the next surviving event or when the queue drains; terminal-control parser state is advanced before shedding
- terminal output under sustained renderer saturation may be incomplete; PRIM-1 does not claim byte fidelity across an output-gap warning
- `routed_message` retains addressing, scope, identity, and timestamp metadata; its durable `content` value is `[content omitted]`
- room catalog and membership events retain IDs, labels, order/revision, and
  timestamps; room-feed message content is replaced with `[content omitted]`
  while membership and per-recipient delivery status remain metadata
- `session_work_state.detail` is omitted from the durable projection
- `desktop-events.jsonl` contains desktop-process diagnostics rather than a terminal transcript; failure diagnostics may quote harness error text and should be inspected before sharing
- desktop diagnostic appends are serialized across command and event threads so each JSONL record remains an independent parseable line
- a PTY-write or route-delivery receipt proves runtime handling only; model understanding and task completion require an independent live or artifact-based oracle

The pane-local sideband uses the authorization paths in section 9. Its endpoint
and a successful connection are transport facts, not credentials. Room read,
post, and delivery derive membership and sender from the authenticated live run.
Lifecycle, inventory, and membership mutations remain desktop actions.

`dispatch_attempt` records the pre-write target state for pane input/key requests and legacy in-process direct routing that passed whole-request validation and recipient preflight. Room Send, Brief and Retry use room-feed delivery receipts instead. The event is diagnostic metadata, not an idle gate, model-reaction ACK, or completion signal.

For legacy direct routing only: unknown or disabled bracketed-paste mode, an unobserved Codex work state, or a driver-observed `blocked`/`error_loop` work state returns the command error and produces no route/message `dispatch_attempt`, `routed_message`, or `route_delivery` event or durable record, and no PTY write. Independent liveness reconciliation may still mutate and record a stale run's lifecycle before the rejection; desktop diagnostics may also record the visible command failure. After successful preflight, a mode, work-state, or partial write failure uses the existing failed/partial delivery receipt. An unrecognized modal appearing after an explicit safe observation remains a measured residual until its driver exposes a proved structured signal.

Dispatch overlap rules:

The `blocked` and `error_loop` overlap labels remain applicable to raw pane input/key diagnostics. Legacy direct routing rejects those observed states during preflight instead of emitting a route dispatch attempt. Room delivery stores the body first and records a Held receipt for a refused recipient.

- `target_work_state_before: thinking` -> `overlap: true`, `reason: "target_thinking"`
- `target_work_state_before: tool_call` -> `overlap: true`, `reason: "target_tool_call"`
- `target_work_state_before: blocked` -> `overlap: true`, `reason: "target_blocked"`
- `target_work_state_before: error_loop` -> `overlap: true`, `reason: "target_error_loop"`
- no observed work state and `target_lifecycle_state_before != ready` -> `overlap: true`, `reason: "target_not_ready"`
- `last_route_from_target_at` within 3 seconds while target work-state is `thinking` or `tool_call` -> `overlap: true`, `reason: "recent_route_from_target"`
- `recent_route_from_target` takes precedence over the generic `thinking` / `tool_call` reason when both apply
- otherwise `overlap: false`, `reason: null`

In-process operator route requests expose delivery metadata:

- `route_delivery` records one resolved target and its write outcome, keyed by
  `request_id` and `route_id`
- every successfully validated and preflighted route emits one
  `phase: "resolved"` event followed by one `phase: "written"` or
  `phase: "failed"` outcome
- a failed outcome records the PTY-writer-reported prefix byte count accepted
  before failure; no automatic retry is allowed after an uncertain partial write

Room delivery commits one immutable message with its authorized recipient snapshot
before independently planning each wake. Unsafe recipients get Held; admitted
attempts sync Pending before a short pointer, then record Written or Failed.
Fetched records the actual addressed message in a returned page, not acceptance.
Uncertain Pending is never repeated. Retry wake uses the latest eligible receipt
cursor for one original recipient and never targets a replacement run. Room-feed
append/publication preserves cursor order; one pending wake per run coalesces
concurrent requests. A room cannot be deleted while a delivery is in flight.

Auto-restart-on-stall rules:

- disabled by default; the optional environment allowlist accepts stable
  comma-separated `SessionId` UUIDs, never labels
- threshold is `PRIM1_AUTO_RESTART_STALL_THRESHOLD_SECS`, default `600`
- when enabled, a transition into `blocked` or `error_loop` arms a generation-bound timer for that session
- any transition out of `blocked`/`error_loop` before the threshold cancels the timer
- if the timer fires and the same generation is still `ready`, running, and in the same stalled work state, the wrapper emits `supervisor_alert` (`alert_type: "session_stall_detected"`, `severity: "critical"`) and internally calls the existing restart flow
- the restart flow emits `session_exit` with `reason: "restart_stop"` for the old process and `session_state: ready` for the new process when start succeeds
- the cap is 3 auto-restarts per session per 30-minute wrapper-lifetime window; the 4th eligible stall emits a critical `supervisor_alert`, disables further auto-restarts for that session until wrapper restart, and leaves the pane for manual intervention

## 11. Cost telemetry

Usage capture, per-session cost totals, and budget enforcement are future work.
The current runtime does not supply a cost meter; attached CLIs use their own
providers and accounts.

## 12. Failure contract

The runtime must never treat malformed input as a fatal crash by default.

Rules:

- malformed sideband message -> log and drop
- renderer reload -> UI can recover live supervisor state while the desktop process remains alive
- desktop exit -> managed process scopes are shut down; later launch creates new runs
- child crash -> lifecycle transition, restart policy applies
