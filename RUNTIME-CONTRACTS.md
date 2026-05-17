# PRIM-1 — Runtime Contracts

**Status:** Drafted for implementation
**Date:** 2026-04-14

## 1. Purpose

This document defines the contracts the runtime must obey:

- how sessions are named
- how messages are shaped
- how routing works
- how lifecycle is represented
- what gets logged
- what permissions exist

These contracts should be implemented explicitly, not inferred from ad-hoc code paths.

## 2. Session model

Every supervised agent session must have:

- `session_name`
- `driver`
- `instance_id`
- `working_root`
- `lifecycle_state`
- `pty_id`
- `process_id`
- `resume_token` or equivalent, when supported

Examples:

- `claude`
- `codex`
- `claude:work`
- `claude:research`
- `codex:test`

The runtime must not assume singular global roles forever.

## 3. Addressing

Supported targets:

- single session
- room
- system

Examples:

- `to = "claude"`
- `to = "codex:test"`
- `to = "room"`

Room routing contract:

- `MessageScope::Room` targets running panes in the sender's pair, excluding the sender.
- `pair_of(session_name)` maps `claude` / `codex` to the protected `main` pair, maps `<prefix>-claude` / `<prefix>-codex` to `<prefix>`, and treats any other session name as a singleton pair.
- If the sender is unknown, supervisor-originated room messages keep the defensive broadcast-to-running-panes behavior.
- Setting `PRIM1_CROSS_PAIR_ROOM_BROADCAST=1` before wrapper launch restores the legacy behavior: all running panes receive Room traffic except the sender.

## 4. Message envelope

All sideband messages should be normalized to one envelope shape.

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

## 5. Message types

Minimum v1 set:

- `chat_message`
- `command_request`
- `command_result`
- `heartbeat`
- `health`
- `spawn_request`
- `restart_request`
- `close_request`

## 6. Visible injection contract

When a routed message is injected into a PTY, it must be visibly stamped.

The operator should be able to distinguish:

- normal agent output
- supervisor-routed message injection
- system notification

V1 rule:

- injections must be explicit and human-readable
- hidden control messages are not acceptable in the visible room

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
- each event carries `session`, `state`, optional `detail`, optional `previous_state`, and `timestamp`
- events are transition-based; repeated output in the same work state updates supervisor memory but does not emit another audit event
- output quiescence can transition a session back to `idle`
- repeated blocked observations with the same detail can escalate to `error_loop`

Exit-cause events:

- `session_exit` captures why a supervised process stopped separately from UI-compatible lifecycle state
- each event carries `session`, `generation`, `process_id`, `exit_code`, `signal`, `success`, `reason`, `requested`, and `timestamp`
- `clean_exit`: the process exited on its own with code `0` / success and no signal indicator
- `crash_exit`: the process exited on its own with a non-zero code, unsuccessful status, or signal indicator
- `operator_stop`: a stop request intentionally terminated the current generation
- `restart_stop`: a restart request intentionally terminated the old generation before the new start
- `pty_error`: the PTY transport failed and process state is unknown
- `process_disappeared`: liveness pruning or PTY close found no usable exit status
- `requested` is `true` only for `operator_stop` and `restart_stop`
- `session_exit` is co-emitted with the existing `session_state` event for UI compatibility; both events use the same `session` and `timestamp`

## 8. Restart contract

Agents never restart peers directly.

Restart flow:

1. agent or operator issues request
2. supervisor validates policy
3. supervisor logs request
4. supervisor performs restart
5. supervisor emits resulting lifecycle/system events

## 9. Permission contract

Each agent session must have:

- working-directory whitelist
- allowed sideband command set
- allowed targets for lifecycle actions

Default expectation:

- agent can send messages
- agent can request actions on itself
- cross-agent lifecycle requests require explicit policy

## 10. Audit log contract

Audit log is the source of truth for replay.

Format:

- JSONL
- one event per line
- rolling files by date

Minimum fields:

- timestamp
- event type
- actor
- target
- session name
- driver
- lifecycle state
- summary
- result

`session_work_state` and `session_exit` events are part of the default signal event stream. Work-state events are derived from driver classifiers, not from explicit pane requests. Exit events are derived from PTY exit status, requested-stop intent, PTY transport errors, and liveness pruning. Both are intended for supervision dashboards, summary tools, and external pollers that need semantic state without parsing raw `session_output`.

Pane-bound sideband requests (`send_input`, `send_key`, `deliver_message`, `route_message`) must expose two correlated layers:

- `sideband_request_lifecycle` records supervisor request processing, keyed by `request_id`
- `dispatch_attempt` records the pre-PTY-write target state for `send_input`, `send_key`, `deliver_message`, and each resolved `route_message` recipient
- `request_ack` records successful PTY-write completion for the target session, keyed by the same `request_id`; `request_ack_timeout` records a missing PTY-write completion after `PRIM1_REQUEST_ACK_TIMEOUT_SECS` (default 60)
- failed or timed-out `sideband_request_lifecycle` events include optional `error` text with the same message returned to the caller

Dispatch overlap rules:

- `target_work_state_before: thinking` -> `overlap: true`, `reason: "target_thinking"`
- `target_work_state_before: tool_call` -> `overlap: true`, `reason: "target_tool_call"`
- `target_work_state_before: blocked` -> `overlap: true`, `reason: "target_blocked"`
- `target_work_state_before: error_loop` -> `overlap: true`, `reason: "target_error_loop"`
- no observed work state and `target_lifecycle_state_before != ready` -> `overlap: true`, `reason: "target_not_ready"`
- `last_route_from_target_at` within 3 seconds while target work-state is `thinking` or `tool_call` -> `overlap: true`, `reason: "recent_route_from_target"`
- `recent_route_from_target` takes precedence over the generic `thinking` / `tool_call` reason when both apply
- otherwise `overlap: false`, `reason: null`

Dispatch gate modes:

- default mode and `-AllowBusy` emit `dispatch_attempt` and proceed with the PTY write
- `-RequireIdle` emits `dispatch_attempt` and aborts before the PTY write when `overlap: true`
- aborted `-RequireIdle` route requests emit one `dispatch_attempt` per resolved recipient, then abort the entire route without partial delivery, `request_ack`, or `route_delivery`

Route sideband requests expose a third delivery-truth layer:

- `route_delivery` records the resolved pane fan-out and each per-recipient write outcome, keyed by `request_id` and `route_id`
- every route emits one `phase: "resolved"` event with `recipient: null`, `recipient_count` set to the resolved pane count, and zero payload/byte counts
- every resolved pane recipient emits `phase: "written"` with `recipient`, `recipient_index`, `payload_part_count`, and `bytes_written`, or `phase: "failed"` with `error`
- partial failure is non-transactional: successful recipient writes remain delivered and audited, and the route request returns an error naming the failed recipient(s)

Pane signal sideband requests expose a first-class completion/liveness channel:

- request kind: `pane_signal`
- signal types: `done`, `blocked`, `yellow`, `heartbeat`, `progress`
- pane-bound tokens resolve `session` from the credential binding; callers cannot spoof another pane name in the request payload
- master/operator-token calls are allowed and record `session: "supervisor"`
- every accepted signal emits a `pane_signal` audit event keyed by `request_id`
- the canonical signal record is written before audit emission to `.runtime/signals/<task_id>__<signal_type>__<timestamp>.json`
- the filename timestamp is UTC and filesystem-safe (`YYYYMMDDTHHMMSS.nnnnnnnnnZ`) because raw RFC3339 colons are invalid on Windows
- `task_id` is sanitized for filenames; the original `task_id` remains in the JSON payload
- canonical JSON contains the full `pane_signal` audit event payload:

```json
{
  "event": "pane_signal",
  "request_id": "uuid",
  "session": "codex",
  "task_id": "task-418",
  "signal_type": "done",
  "summary": "completed",
  "artifact_paths": [],
  "commit_sha": null,
  "timestamp": "2026-05-17T00:00:00Z"
}
```

- a legacy empty touch-file is also written at `.runtime/dispatch-triggers/<task_id>.<signal_type>` for existing watchers
- legacy touch-file write failure emits a warning `system_log` but does not roll back or suppress the canonical JSON write path

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
