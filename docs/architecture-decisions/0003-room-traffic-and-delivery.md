# ADR 0003 — Shared room feed and explicit delivery

**Status:** Accepted by Victor on 2026-08-10

**Date:** 2026-08-10

## Context

Victor described a room as a visible group conversation that every member can read, not as an instruction to inject every post into every harness. Ordinary terminal conversations that predate or happen outside the room remain private. Current PRIM-1 instead maps a name-derived pair to recipient PTYs and treats a room route as prompt fan-out; unknown/supervisor senders may fall back to every running pair.

PRIM-1 already has a native local sideband for explicit harness communication. Agent Bus remains an independent collaboration tool and is not the room transport.

## Options

### A. Every room post automatically becomes a prompt for every member

This preserves current broadcast-like behavior but spends every member's context and compute, confuses visibility with instruction delivery, and makes accidental fan-out the default.

**Rejected as the default.** An explicit Send All operation may still request this behavior.

### B. Shared member-readable feed with explicit addressed delivery

- Posting appends one immutable, provenance-stamped room message.
- Each member can read new feed events through the native sideband from its own cursor.
- A post does not enter a harness PTY unless the sender explicitly selects recipients or chooses Send All.
- Ordinary terminal output is private unless the harness explicitly posts it.

**Recommended.** It matches Victor's group-chat concept while retaining deliberate model invocation and a visible operator record.

### C. UI-only transcript that harnesses cannot read

This gives the operator visibility but cannot support harness-to-harness collaboration without another hidden transport.

**Rejected.**

## Delivery semantics under Option B

- A room and its membership are identified by stable IDs; names grant no authority.
- Joining begins at the current room-feed cursor. PRIM-1 never imports or replays private terminal history or earlier room messages to the new member.
- Join and removal append visible system membership events. A newly joined harness receives only the minimal explicit notice needed to discover the room and sideband operation; the notice is not prior conversation.
- Creation accepts any member count from zero up to the member cap (amended 2026-08-30, Rooms B1: rooms are workspaces, and an empty room may be set up before its team arrives). Operator delivery requires at least one member — an empty room refuses with the plain reason — and a pane's `all` requires at least one *other* member.
- The visible provenance envelope and immutable source content are separate. The source bytes must reconstruct exactly inside any clearly delimited envelope, and the driver submits the complete envelope as one logical harness turn.
- Direct delivery names explicit sessions. Send All snapshots the room membership revision and recipient set before the first write.
- Every recipient and its measured input capability is preflighted before delivery begins. Preflight failure writes nothing.
- Once writes begin, recipient results are independently visible. A later OS or harness failure may leave an honest partial success because a delivered prompt cannot be rolled back. PRIM-1 never automatically retries a logical message.
- Deliveries to one run are FIFO and atomic with respect to operator input and other logical submissions. No global ordering across independent runs is claimed.
- Pending, written, accepted, and failed are distinct where the harness can prove them. A pre-delivery UI/event emission is never labeled delivered.
- Kernel-derived pane authority can post/read only its authorized room. The first
  sideband slice exposes no recipient delivery at all; any later pane delivery
  must target eligible members through a separate explicit grant. Operator
  authority may explicitly deliver to current room members. Unknown sender,
  unknown target, stale run, and unauthorized cross-room requests fail closed.
- Two rooms with identical labels remain isolated by `RoomId`.
- Feed retention is separate from active visibility. The accepted first slice uses a bounded, authorized in-memory feed with explicit cursor-gap behavior. With retention off, the durable audit stores metadata/status but no message content or content-derived hash; existing historical audit files remain readable legacy evidence and are not rewritten.

## Consequences

- Current automatic/defensive room broadcast and semantic chunking are characterized with negative controls, then deleted.
- Harnesses opt into collaboration through explicit sideband read/post instead
  of having ordinary assistant output scraped and relayed; recipient delivery
  is an explicit operator action in the first slice.
- The operator can see exactly which content was merely posted, which recipients were addressed, and what each delivery proved.
- Multi-member rooms do not force every message through every model.

## Decision

Victor accepted Option B: a shared native-sideband room feed readable by members, with no automatic prompt injection and with explicit recipients or Send All for harness delivery.
