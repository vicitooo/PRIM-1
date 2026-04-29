# CLI-master-wrapper — Runtime Contracts

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

