# ADR 0001 — Explicit sessions and rooms

**Status:** Accepted by Victor on 2026-08-10

**Date:** 2026-08-10

Historical decision record. The context and options describe the implementation
at that date. Current behavior is in [ARCHITECTURE.md](../../ARCHITECTURE.md)
and [RUNTIME-CONTRACTS.md](../../RUNTIME-CONTRACTS.md).

## Context

At the time of this decision, main derived a Claude/Codex pair from display-name suffixes. Pair creation spawned new fixed slots, an existing session could not join, a third member could not exist, and an unknown/supervisor room sender could resolve to every running pane. The frontend independently repeated grouping rules.

Victor's required workflow is to begin with excellent native terminal sessions and later select already-running sessions into a visible isolated room without importing their earlier private histories.

## Options

### A. Keep pairs and add a tab/room facade

This preserves current internals but cannot honestly support arbitrary member count, joining existing sessions, stable identity, rename, isolation, or cross-runtime harnesses. It creates a second model in the UI.

**Rejected.**

### B. Add explicit identities and membership inside the current supervisor

- Stable `SessionId`, per-process `RunId`, stable `RoomId`, explicit membership records.
- `RunId` is an internal generation key needed to reject stale process events and run-bound authority; it is not another operator-facing object.
- Display aliases remain compatibility input only.
- Rooms reference existing sessions and never spawn by implication.
- The supervisor remains the authority; the UI renders backend truth through a typed protocol.
- Current in-memory pair behavior is characterized and removed. Only named compatibility aliases with a proved consumer remain temporarily.

**Recommended.** This is the smallest model that satisfies the workflow and completes the direction already stated in `ARCHITECTURE.md`.

### C. Replace native routing with Agent Bus or a remote-room subsystem

This adds a second service/protocol and imports outside goals before local tabs/rooms work. Agent Bus is valuable for outside consultation; remote-room branches target other products.

**Rejected.**

## Consequences

- Fixed-pair routing is characterized, replaced, and deleted rather than wrapped forever.
- Session labels and harness kinds stop serving as authorization.
- The separately accepted first slice permits zero or one active room per session; the identity model does not make multi-room support impossible.
- Joining exposes only future room messages; private terminal scrollback/history stays private.
- Direct and room sends use explicit recipient identities and fail closed.
- The product can add Grok, Prime, Hermes, or a future driver without another room model.
- Compatibility aliases and their tests are temporary and removable once no named consumer remains.

## Decision

Victor accepted Option B: replace name-derived Claude/Codex pairs with stable PRIM-managed sessions plus explicit room membership, while retaining PRIM-1's native sideband—not Agent Bus—as the communication mechanism.

Victor separately accepted zero or one active room per session for the first slice; the identity model remains extensible to a future multi-room decision.
