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

## 3. Addressing

The production operator routing boundary accepts exactly one `recipient_id` and
content. The desktop derives `from = "operator"`; callers cannot supply
provenance, scope, names, or a recipient list. The current flat-tab UI exposes no
message composer, so this remains a one-recipient backend boundary until
explicit `RoomId` membership is implemented. There is no unknown-target, name,
multi-recipient, or broadcast fallback.

The tokenless pane sideband exposes no route operation. Future pane room traffic
must derive identity from the connected process job and explicit `RoomId`
membership; a caller-supplied name is never authority.

## 4. Future room/feed envelope

This envelope is the target shape for explicit RoomId-scoped feed entries after
`RoomId` membership exists. Stable `SessionId`/`RunId` identity is implemented;
the feed and room registry are not. This is not the current pane-local sideband
schema and its `from` field is never caller authority.

Example:

```json
{
  "id": "uuid",
  "type": "chat_message",
  "from": "claude",
  "to": "codex",
  "scope": "direct",
  "created_at": "2026-04-14T22:00:00Z",
  "content": "Please review the last change.",
  "metadata": {
    "request_id": null,
    "reply_to": null
  }
}
```

## 5. Current typed surfaces

- In-process operator actions own `SessionId`-addressed create, rename, reorder,
  closed-only delete, lifecycle, input, inventory, resize, native cwd selection,
  typed permission changes, and singular direct routing.
- The pane-local sideband owns only self `ping`, `wait_quiet`, `send_input`, and
  `send_key`.
- Runtime telemetry owns structured lifecycle, delivery, heartbeat, alert, and
  metadata-only dispatch events.

Spawn, restart, close, and arbitrary command execution are not pane messages.
Future room feed entries begin with `chat_message`; any additional type requires
a new explicit contract and authority model.

## 6. Visible injection contract

When a routed message is injected into a PTY, it must be visibly stamped.

The operator should be able to distinguish:

- normal agent output
- supervisor-routed message injection
- system notification

V1 rule:

- injections must be explicit and human-readable
- hidden control messages are not acceptable in the visible room
- one logical message holds one run-scoped FIFO input permit until its harness submit is complete; semantic chunking is forbidden, but a driver may use multiple serialized PTY calls when its measured TUI protocol requires a boundary
- for Claude, Codex, and Grok, the Rust supervisor requires enabled paste mode before writing one complete bracketed-paste frame, waits a one-second compatibility interval measured against Claude Code 2.1.226, Codex 0.147.0, and Grok Build 1.0.0, revalidates the exact run and safe observed work state, then writes one Enter under the same permit; tests cover embedded CR, LF, tabs, Unicode, blank lines, and trailing spaces from the backend request body through the bytes admitted to the PTY writer
- PTY output is not treated as paste acknowledgement because a multiline paste may remain intentionally invisible until Enter; another operator, pane-sideband, or routed input cannot interleave during the interval, and an uncertain outcome is never followed by an automatic Enter or message retry
- bracketed delivery is admitted only after the exact active run's incrementally decoded, unshed PTY stream has enabled DEC private mode 2004 and its last driver-observed work state is not `blocked` or `error_loop`; UTF-8 code points split across PTY reads are carried intact so the scanner and renderer receive the same character stream, the bounded scanner resets for every `RunId`, follows the shipped terminal's relevant control transitions, returns to unknown on parser overflow, and refuses unknown/disabled mode or a known unsafe work state before route metadata or PTY writes
- the supervisor revalidates `SessionId`, `RunId`, generation, PTY, input gate, enabled paste mode, and safe observed work state together before the paste frame; before Enter it revalidates the same exact run/gate/PTY and safe work state but does not require paste mode to remain enabled because Codex legitimately emits DECRST after consuming a completed frame; this proves the state observed at those writer boundaries, not the child's interpretation during or after them, and raw operator or pane-sideband typing remains available when addressed delivery is refused
- within that proven Rust boundary, supervisor framing does not normalize, trim, flatten, or silently truncate source-content UTF-8 bytes
- final-child byte fidelity and receiver receipt remain Gate 3 RED; `PtySession::send_input` success and `route_delivery.phase = "written"` prove only the PTY-writer outcome
- clipboard-to-WebView textarea CRLF fidelity before the backend request boundary remains unclaimed until a packaged WebView receipt proves it
- logical message bodies up to and including 1 MiB pass size validation, subject to the recipient driver's framing and exact-run mode preflight; larger bodies fail before PTY-writer admission
- C0, C1, and DEL control characters other than tab and source line endings fail before PTY-writer admission because they cannot be injected as ordinary terminal text safely
- generic-terminal delivery is raw and single-line only until a concrete driver proves a faithful multiline strategy
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
- events are transition-based; repeated output in the same work state updates supervisor memory but does not emit another audit event
- output quiescence can transition a session back to `idle`
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

Current pane authority is deliberately narrower than the future room model:

- the supervisor derives the caller from live process-job membership and run generation
- the pane-local sideband action set is fixed to self `ping`, `wait_quiet`, `send_input`, and `send_key`
- pane callers have no room, peer, inventory, or lifecycle action
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
- `session_work_state.detail` is omitted from the durable projection
- `desktop-events.jsonl` contains desktop-process diagnostics, not terminal or conversation content
- desktop diagnostic appends are serialized across command and event threads so each JSONL record remains an independent parseable line
- a PTY-write or route-delivery receipt proves runtime handling only; model understanding and task completion require an independent live or artifact-based oracle

The pane-local sideband supports only `ping`, `wait_quiet`, `send_input`, and `send_key`. Its endpoint and a successful connection are transport facts, not credentials. On Windows, caller authority is derived by pinning the kernel-reported named-pipe client process and verifying its live pane job and generation, never from a bearer or info file. Lifecycle, inventory, room, and routing operations remain in-process desktop actions.

`dispatch_attempt` records the pre-write target state for pane input/key requests and in-process operator delivery/routing that passed whole-request validation and recipient preflight. It is diagnostic metadata, not an idle gate, model-reaction ACK, or completion signal.

Unknown or disabled bracketed-paste mode, or a driver-observed `blocked`/`error_loop` work state, returns the command error and produces no route/message `dispatch_attempt`, `routed_message`, or `route_delivery` event or durable record, and no PTY write. Independent liveness reconciliation may still mutate and record a stale run's lifecycle before the rejection; desktop diagnostics may also record the visible command failure. After successful preflight, a mode, work-state, or partial write failure uses the existing failed/partial delivery receipt. Unrecognized modal prompts remain a measured residual until their driver exposes a proved readiness signal.

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
