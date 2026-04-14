# CLI-master-wrapper — Phase 0 Research

**Status:** Not started
**Purpose:** Remove the major unknowns before implementation begins

## Rule

Phase 0 code is exploratory.

- spikes are allowed
- hacks are allowed
- shortcuts are allowed
- **none of it should be treated as production code automatically**

Do not graft Phase 0 spikes into the real runtime without re-implementing them cleanly in Phase 1.

## Objectives

Phase 0 must answer these questions:

1. Can we own and drive PTYs cleanly enough on Windows?
2. Can we render them cleanly in Tauri via xterm.js?
3. Can Claude and Codex be resumed/restarted under supervision in a useful way?
4. Can a local sideband control path route messages reliably?
5. What are the highest-risk failure modes before building the real runtime?

## Deliverables

Phase 0 is done only when these exist:

- `phase-0-notes.md`
- one Claude PTY spike
- one Codex PTY spike
- one xterm.js render proof
- one local IPC send-path proof
- one documented renderer/backend choice
- one documented fallback path
- one documented test strategy
- one documented Claude resume finding

## Work items

### A. PTY backend evaluation

Find and validate the most viable Windows-first Rust path for:

- creating a PTY
- spawning a child inside it
- reading terminal output continuously
- writing input into it
- handling resize
- handling clean close

Success criteria:

- child CLI visibly responds to injected input
- output stream is stable enough for rendering
- lifecycle control is viable

### B. Tauri + xterm.js proof

Build a minimal Tauri shell that:

- opens a window
- renders xterm.js
- receives a PTY byte stream
- displays output correctly
- sends input back to the runtime

Success criteria:

- terminal output is readable
- input path works
- resizing is not obviously broken

### C. Claude spike

Validate:

- launch under supervisor-owned PTY
- stable output capture
- interrupt/close behavior
- resume semantics after restart

Key question:

- after restart, is Claude meaningfully resumable for our use case, or does it require re-briefing?

### D. Codex spike

Validate:

- launch under supervisor-owned PTY
- stable output capture
- interrupt/close behavior
- resume semantics after restart

Key question:

- what state is preserved usefully enough for the room and future orchestration?

### E. IPC proof

Build a minimal local send path:

- sender uses local IPC
- runtime receives message
- runtime routes it to a target session
- target PTY shows a visible stamped injection

Success criteria:

- no reliance on stdout regex
- local-only path works
- logging can see the same event

### F. Audit and telemetry decision

Lock:

- audit log format
- audit log location
- minimum event schema
- token/cost capture hooks

Success criteria:

- enough structure to support replay later

### G. Test strategy decision

Lock the v1 testing model:

- unit tests for runtime logic
- synthetic child-process integration tests
- real Claude/Codex tests as opt-in or manual

Success criteria:

- Phase 1 can be implemented without inventing the test approach on the fly

## Recommended output file

`phase-0-notes.md` should contain:

1. PTY backend result
2. xterm.js result
3. Claude resume result
4. Codex resume result
5. IPC result
6. audit log decision
7. test strategy
8. fallback decision
9. unresolved blockers
10. go/no-go recommendation

## Go / no-go threshold

Do **not** start Phase 1 unless:

- PTY backend is chosen
- xterm.js integration is proven enough
- Claude and Codex can both be run under owned PTYs
- IPC path is proven
- test strategy is written down
- fallback path is clear

If one of those is missing, Phase 1 will accumulate rework.

