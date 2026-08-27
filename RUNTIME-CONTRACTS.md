# PRIM-1 — Runtime Contracts

**Status:** Active implementation contract; release gates remain open
**Date:** 2026-04-14

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
permission profile. It never persists `RunId`, process/lifecycle state, commands,
arguments, environment variables, terminal output, or message content. A fresh
catalog contains zero sessions. Corrupt or unknown versions fail visibly without
default replacement.

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

The tokenless pane sideband exposes no recipient-delivery operation. Its
`room_read` and `room_post` requests carry neither `RoomId` nor sender: the
supervisor derives the exact live caller, current room membership, join floor,
and sender `SessionId` from the connected process job. A caller-supplied name is
never room authority.

## 4. Room/feed envelope

The private room catalog atomically persists ordered `RoomId`, label, member
`SessionId` values, and a nonzero membership revision. Feed epoch, sequence,
messages, delivery results, and cursor state are process-memory only and reset
on restart. Corrupt, structurally invalid, or unknown catalog versions fail
startup without rewrite.

Each room owns a random process-lifetime epoch and monotonic sequence. Its feed
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
recipient snapshot plus separate `pending` then `written` / `failed` delivery
items. UI emission is never a model receipt.

Room membership belongs to the durable session definition, not to one live
run: closed members remain members, while delivery requires every selected
recipient to have a compatible live run at whole-message preflight. A dormant
zero- or one-member room remains visible until explicitly deleted. Every room
member can read an addressed message from the shared feed even when only one
member was selected for PTY delivery. The operator is not a room member, so
**Send All** means every member in the pinned revision; pane sideband callers
can read/post only their current room and cannot request delivery.

## 5. Current typed surfaces

- In-process operator actions own `SessionId`-addressed create, rename, reorder,
  closed-only delete, lifecycle, input, inventory, resize, native cwd selection,
  typed Prime Linux cwd selection, and typed permission changes.
- In-process room actions own room catalog/membership mutations, operator feed
  posts, and explicit one-member / Send All delivery for admitted native drivers.
- The pane-local sideband owns self `ping`, `wait_quiet`, `send_input`,
  `send_key`, plus membership-derived `room_read` and feed-only `room_post`.
- Runtime telemetry owns structured session/room catalog, room feed, lifecycle,
  delivery, heartbeat, alert, and metadata-only dispatch events.

Spawn, restart, close, and arbitrary command execution are not pane messages.
Room feed item kinds are `message`, `membership`, and `delivery`. Any additional
kind requires a new explicit contract and authority model.

## 6. Visible injection contract

When a routed message is injected into a Claude, Codex, or Grok PTY, it must be
visibly stamped inside its logical bracketed submission. Claude/Codex use one
frame; Grok minimal mode uses one fused buffer of per-line frames. Generic Terminal is the
deliberate exception: the PTY receives only the validated printable single-line
source command, while PRIM displays provenance in its feed and route receipts
and retains provenance metadata in the audit. Prefixing a generic shell command
with human display text would change or invalidate the command.

The operator should be able to distinguish:

- normal agent output
- supervisor-routed message injection
- system notification

V1 rule:

- injections must be explicit and human-readable
- hidden control messages are not acceptable in the visible room
- one logical message holds one run-scoped FIFO input permit until its harness submit is complete; semantic chunking is forbidden, but a driver may use multiple serialized PTY calls when its measured TUI protocol requires a boundary
- for Claude and Codex, the Rust supervisor requires enabled paste mode before writing one complete bracketed-paste frame; Grok always launches in measured `--minimal` mode and instead receives one complete fused buffer containing one bracketed frame per LF-separated logical line with `ESC CR` (Alt+Enter) between frames, preserving empty and trailing lines without per-line host waits
- Grok minimal delivery rejects any source carriage return, body above 13 KiB, or more than 256 source lines during whole-route preflight; these bounds stay below the Grok Build 1.0.0 receiver-proven 261-line / 14,333-byte GREEN while a preserved 4,095-line / 114,780-byte neighbor remains RED, and a partial/error/short first write reports exact progress but sends no Enter or retry
- after the complete first input, Claude, Codex, and Grok wait from that successful write boundary for a one-second base interval plus a proportional 500 milliseconds per MiB of framed input, revalidate the exact run and safe observed work state, then write one Enter under the same permit; the exact final-child oracle observed approximately 10/19/174 milliseconds of parser lag at 1 KiB/64 KiB/1 MiB, while 500 milliseconds per MiB is the deliberately conservative stability margin
- PTY output is not treated as paste acknowledgement because a multiline paste may remain intentionally invisible until Enter; another operator, pane-sideband, or routed input cannot interleave during the interval, and an uncertain outcome is never followed by an automatic Enter or message retry
- bracketed delivery is admitted only after the exact active run's incrementally decoded, unshed PTY stream has enabled DEC private mode 2004 and its last driver-observed work state is not `blocked` or `error_loop`; Codex also requires an observed work state, and its bounded per-run classifier begins before PTY installation, recognizes measured modal prompts across arbitrary output chunks, latches a block until a later trusted blocker-free clean screen, and applies a pre-install block before Ready/routability; the shared bounded viewport establishes Idle from Unknown only when Codex 0.147.0's trusted current screen has a visible column-3 cursor on an input row that is exactly `›` or begins `› `, followed by an indented non-empty footer row whose final ` · ` separates a non-empty model from a path-like cwd; those facts may be composed across multiple synchronized or ordinary paints, never from incomplete evidence and never while a blocker remains on the screen; well-formed terminal strings are consumed without exposing printable payload as text or cursor evidence, while ESC and C1 global transitions follow the shipped xterm parser and malformed or unterminated controls fail closed; a future repaint shape therefore remains Unknown rather than being generalized into a second terminal emulator; UTF-8 code points split across PTY reads are carried intact so the scanners and renderer receive the same character stream, the bounded trackers reset for every `RunId`, the terminal-mode scanner follows the shipped terminal's relevant control transitions and returns to unknown on parser overflow, and preflight refuses unknown/disabled mode, unobserved Codex state, or a known unsafe work state before route metadata or PTY writes
- the supervisor revalidates `SessionId`, `RunId`, generation, PTY, input gate, enabled paste mode, and safe observed work state together before the paste frame; before Enter it revalidates the same exact run/gate/PTY and safe work state but does not require paste mode to remain enabled because Codex legitimately emits DECRST after consuming a completed frame; this proves the state observed at those writer boundaries, not the child's interpretation during or after them, and raw operator or pane-sideband typing remains available when addressed delivery is refused
- within that proven Rust boundary, supervisor framing does not trim, flatten, or silently truncate source-content UTF-8 bytes; Grok accepts LF-only source and fails closed on CR rather than normalizing it
- final-child byte fidelity and receiver receipt remain Gate 3 RED; `PtySession::send_input` success and `route_delivery.phase = "written"` prove only the PTY-writer outcome
- clipboard-to-WebView textarea CRLF fidelity before the backend request boundary remains unclaimed until a packaged WebView receipt proves it
- Claude/Codex logical message bodies up to and including 1 MiB pass size validation, subject to exact-run mode preflight; Grok uses the smaller measured bound above, and larger bodies fail before PTY-writer admission
- C0, C1, and DEL control characters other than tab and source line endings fail before PTY-writer admission because they cannot be injected as ordinary terminal text safely
- generic-terminal delivery is the unprefixed source command, raw and single-line only, until a concrete driver proves a safe provenance/framing strategy; route/feed/audit metadata remains provenance-authoritative
- Prime routed delivery is not admitted in this release. Prime accepts raw
  operator terminal input only; the rejection occurs before route-family events,
  audit records, or PTY writes.
- pane-sideband `send_input` and `send_key` have a 20-second server-side write budget. On expiry the supervisor first cancels only that PTY input operation for up to 5 seconds while a per-run barrier prevents a late cancellation from touching its successor. The live conversation is preserved when the writer stops. If isolated cancellation fails or does not stop the writer, the supervisor invalidates and terminates only that exact run (up to 5 seconds), then waits up to 5 more seconds for the writer. Every over-budget response sets `timed_out: true`; `ok: true` is possible only when the full write completed after the deadline and warns the caller not to retry. The PowerShell client allows 45 seconds so it can receive this truthful terminal response.
- pane-sideband `wait_quiet` accepts a quiet window of 1–60 seconds and a total timeout of 1–300 seconds, with the quiet window no greater than the timeout. The server validates these bounds before dispatch metadata or connection-long waiting.

## 7. Lifecycle contract

Allowed states:

- `starting`
- `ready`
- `busy`
- `idle`
- `stalled`
- `restarting`
- `failed`
- `closed`

V1 liveness rule:

- passive output-based liveness per driver

Meaning:

- output implies liveness
- lack of output while busy implies stall risk

Work-state events:

- `session_work_state` captures semantic pane activity separately from lifecycle state
- allowed work states: `idle`, `thinking`, `tool_call`, `blocked`, `error_loop`
- each event carries a nested `RunEventIdentity` (`SessionId`, `RunId`, generation, monotonic sequence), the current session label, state, optional detail, optional previous state, and timestamp
- the first explicit observation is emitted even when it is `idle`, with no previous state; later repeated output in the same observed work state updates supervisor memory but does not emit another audit event
- output quiescence can transition Claude and Generic Terminal sessions back to
  lifecycle/work-state `idle`; for Codex it changes lifecycle only and never
  establishes semantic work state or routed-input authority, which require a
  trusted current-screen observation
- each Grok launch receives native `--minimal`, a fresh native `--session-id`, and remains lifecycle
  `starting` while the exact run is on the launcher or still producing its
  startup repaint; those states reject synthetic delivery while raw terminal
  input remains available
- Grok startup admission is structural, never temporal: the shared bounded
  driver-internal fixed-grid viewport follows only the measured cursor, erase,
  scroll, terminal-string, mode, and decoded-text subset across completed
  cursor-hide/show frames, with Unicode cell widths derived from Unicode
  Standard Annex #11; it is not a general terminal emulator, and it
  first requires a trusted current screen containing `Starting session…`, then
  a later trusted current screen with the launcher absent, the interactive
  composer and exact `minimal · /help` chrome present, and DEC private mode
  2004 enabled; minimal mode can leave the earlier startup row visible, so the
  ordered exact minimal marker—not absence of that stale row—establishes readiness
- replacement runs start with a fresh tracker; resize, malformed, unsupported,
  or over-limit control state fails closed until known output reconstructs the
  screen; no timeout grants readiness, and readiness is not a model-turn receipt
- a repeated resize request with dimensions already applied to the exact run is
  a no-op; only a changed size reaches the PTY and invalidates the tracked screen
- Codex 0.147.0 prompt admission uses the same bounded viewport with Codex-only
  policy: Idle requires one trusted current screen with a visible column-3 cursor
  on an input row exactly `›` or beginning `› `, followed by an indented non-empty
  footer row whose final ` · ` separates a non-empty model from a path-like cwd,
  even when those facts were composed across several synchronized and ordinary
  paints; `▌` and `esc to interrupt` never grant Idle
- a measured Codex blocker remains latched until a later trusted current screen
  contains the clean prompt and no measured blocker; unfamiliar layouts and
  incomplete, malformed, unsupported, resized-but-unreconstructed, or over-limit
  state remain Unknown, while raw input remains available for modal resolution
- after that one-shot admission, minimal mode publishes finalized response blocks
  instead of relying on Grok's suppressible full-screen response repaint; the
  completing startup frame makes initial `idle` pending exactly once; event
  admission publishes it unless a later admitted output carries a newer
  semantic marker, which supersedes it. Every later Grok output returns to the semantic
  classifier rather than being coerced to `idle`; measured Grok 1.0.3
  `Thinking`/`Responding`, tool, and `Worked for` markers report subsequent
  `idle`, `thinking`, or `tool_call`
- repeated blocked observations with the same detail can escalate to `error_loop`
- process exit is never inferred from terminal text; PTY closure/error, OS process state, and job membership own process lifecycle truth
- `closed` means the exact per-run owned process job was proved empty; PTY EOF,
  process disappearance, or handle drop alone is not sufficient
- start, stop, restart, natural EOF/error retirement, liveness retirement, and
  delete serialize through one per-session lifecycle reservation
- a failed or bounded-out termination proof retains the exact run ownership in
  `failed`/termination-uncertain state and blocks start, restart, and delete; only a
  later reserved termination attempt that proves the job empty may transition it
  to `closed`
- shutdown has bounded per-run cleanup and returns failure if any owned scope cannot
  be proved terminated; process-exit job cleanup is a backstop, not an in-process
  termination receipt

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

1. agent or operator issues request
2. supervisor validates policy
3. supervisor logs request
4. supervisor performs restart
5. supervisor emits resulting lifecycle/system events

## 9. Permission contract

Current pane authority is deliberately narrower than operator room authority:

- the supervisor derives the caller from live process-job membership and run generation
- the pane-local sideband action set is fixed to self `ping`, `wait_quiet`,
  `send_input`, `send_key`, membership-derived `room_read`, and feed-only
  `room_post`
- pane callers cannot supply `RoomId`, sender, peer, recipient, delivery,
  inventory, membership, or lifecycle authority
- Claude Code and Codex receive a session-scoped `prim1_pane` stdio MCP child
  exposing only `ping`, `room_read`, and `room_post`; the MCP layer contributes
  no authority and every call is re-authorized through the named pipe
- Claude's three fully qualified pane-MCP tool names are allowed only for that
  process; no wildcard or persistent user/workspace permission is installed,
  and all non-pane tools retain the selected harness permission policy
- Grok Build 1.0.0 receives no model-facing pane tool: its TUI lacks a
  privacy-safe session-scoped plugin/config seam and its shell-tool children do
  not satisfy Job membership. Operator sends/raw input remain available, with
  no bearer, ancestry, global-config, or redirected-history fallback
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
- `desktop-events.jsonl` contains desktop-process diagnostics, not terminal or conversation content
- desktop diagnostic appends are serialized across command and event threads so each JSONL record remains an independent parseable line
- a PTY-write or route-delivery receipt proves runtime handling only; model understanding and task completion require an independent live or artifact-based oracle

The pane-local sideband supports `ping`, `wait_quiet`, `send_input`, `send_key`,
`room_read`, and `room_post`. Its endpoint and a successful connection are
transport facts, not credentials. On Windows, caller authority is derived by
pinning the kernel-reported named-pipe client process and verifying its live
pane job and generation, never from a bearer or info file. Room read/post then
derive membership and sender under the same lock; lifecycle, inventory,
membership, and recipient delivery remain in-process desktop actions.

`dispatch_attempt` records the pre-write target state for pane input/key requests and in-process operator delivery/routing that passed whole-request validation and recipient preflight. It is diagnostic metadata, not an idle gate, model-reaction ACK, or completion signal.

Unknown or disabled bracketed-paste mode, an unobserved Codex work state, or a driver-observed `blocked`/`error_loop` work state returns the command error and produces no route/message `dispatch_attempt`, `routed_message`, or `route_delivery` event or durable record, and no PTY write. Independent liveness reconciliation may still mutate and record a stale run's lifecycle before the rejection; desktop diagnostics may also record the visible command failure. After successful preflight, a mode, work-state, or partial write failure uses the existing failed/partial delivery receipt. An unrecognized modal appearing after an explicit safe observation remains a measured residual until its driver exposes a proved structured signal.

Dispatch overlap rules:

The `blocked` and `error_loop` overlap labels remain applicable to raw pane input/key diagnostics. Synthetic bracketed operator delivery rejects those observed states during preflight instead of emitting a route dispatch attempt.

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

Room delivery first resolves one membership revision and preflights every
selected exact run before appending the message or writing any PTY. The feed
then records one immutable message, `pending` for every selected recipient, and
one `written` or `failed` result per recipient. Membership changes after that
commit affect the next message, not the pinned delivery plan. Delivery attempts
are serialized through each exact run's FIFO input gate; room-feed append and
publication are serialized so cursor order cannot differ from renderer/audit
order. A room cannot be deleted while any of its deliveries is in flight.

Auto-restart-on-stall rules:

- disabled by default; the optional environment allowlist accepts stable
  comma-separated `SessionId` UUIDs, never labels
- threshold is `PRIM1_AUTO_RESTART_STALL_THRESHOLD_SECS`, default `600`
- when enabled, a transition into `blocked` or `error_loop` arms a generation-bound timer for that session
- any transition out of `blocked`/`error_loop` before the threshold cancels the timer
- if the timer fires and the same generation is still `ready`, running, and in the same stalled work state, the wrapper emits `supervisor_alert` (`alert_type: "session_stall_detected"`, `severity: "critical"`) and internally calls the existing restart flow
- the restart flow emits `session_exit` with `reason: "restart_stop"` for the old process and `session_state: ready` for the new process when start succeeds
- the cap is 3 auto-restarts per session per 30-minute wrapper-lifetime window; the 4th eligible stall emits a critical `supervisor_alert`, disables further auto-restarts for that session until wrapper restart, and leaves the pane for manual intervention

## 11. Cost telemetry contract

Supervisor must have a hook for:

- token usage capture
- cost accumulation per session
- surfaced cost in system log or status output

Budget enforcement may come later, but the capture hook must exist early.

## 12. Failure contract

The runtime must never treat malformed input as a fatal crash by default.

Rules:

- malformed sideband message -> log and drop
- UI crash -> supervisor keeps running
- reconnect -> UI reattaches to supervisor state
- child crash -> lifecycle transition, restart policy applies
