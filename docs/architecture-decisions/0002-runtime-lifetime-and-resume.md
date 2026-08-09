# ADR 0002 — Runtime lifetime and honest resume

**Status:** Proposed; requires Victor approval

**Date:** 2026-08-10

## Context

Current Tauri and supervisor state share one process; full UI/app close shuts down PTYs. Existing architecture documentation describes a daemon that can outlive the UI, but the implementation does not provide one. PTY byte-stream reattachment cannot be claimed after its owning process dies. Installed harnesses expose different logical resume/attach capabilities, and Prime runs across the WSL boundary.

## Options

### A. First slice keeps one desktop process

- PRIM-1 has no hidden tray or invisible background mode in this slice.
- Closing the last window or choosing Quit shows the number of owned running sessions and offers **Cancel** or **Quit and stop sessions**. It never silently leaves processes behind or silently kills them.
- Confirmed full backend/app exit stops every owned run and proves or reports the resulting process state.
- Persist logical session/room definitions and supported harness resume references.
- Starting PRIM-1 again loads definitions and offers capability-gated logical resume in a new owned process/PTY, not terminal reattachment.
- A separate host/service is deferred until measurement and workflow demand justify it.

**Recommended minimum.** It avoids a service rewrite while making every lifecycle promise honest.

If approved, this first-slice decision explicitly supersedes the daemon/reconnect promise in the current `ARCHITECTURE.md`. That documentation must be updated with the implementation rather than left as a contradictory promise. It does not forbid a later persistent host if measured daily-driver demand earns one.

### B. Split a persistent native session host now

- UI attaches to a separate local supervisor process.
- Sessions may survive UI close and renderer failure.
- Requires authenticated IPC, install/start/update/recovery ownership, version negotiation, bounded replay, and a Windows/WSL lifetime model.

**Valid only if UI-close survival is a first-release requirement.** It is materially larger and must be selected deliberately.

### C. Use a WSL daemon as the universal owner

This makes Windows-native Claude/Codex/Grok dependent on WSL and confuses host path/process boundaries.

**Rejected.**

## Invariants under either A or B

- `SessionId` represents logical continuity; the internal `RunId` and PTY handle do not survive process death.
- Resume always creates a fresh run/PTY unless a driver proves a distinct native attach-client capability.
- Prime resume/attach remains unavailable in product UI until Gate 3 empirically proves its advertised CLI semantics. If a native attach-client capability is later proven, it creates a new client PTY and closing that client does not imply target termination.
- Process and terminal survival, logical resume, transcript replay, and archived output are separately labeled capabilities.
- No silent restart or fallback from failed resume to a new conversation.
- UI close, app quit, backend crash, WSL shutdown, harness exit, attachment close, and target stop are distinct events.

## Approval question

Choose Option A for the first production slice—explicit quit confirmation, owned-session shutdown, and later logical resume in fresh PTYs—or require Option B and UI-close process survival before the daily-driver/room release can be considered complete?
