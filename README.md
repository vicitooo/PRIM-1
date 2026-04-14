# CLI-master-wrapper — Plan

**Date:** 2026-04-14 (late evening)
**Status:** DESIGN DOC — implementation not started. Phase 0 research is the first action for the next session.
**Authors:** Claude Code session (design + integration of the inputs), Codex via Mode A review (architecture validation + v1 spec insights across 4 rounds), Victor (vision + naming + peer-supervisor insight + "real fucking time" framing for the three-way chat).

---

## The vision

A local multi-agent runtime where Victor + Claude + Codex (and future CLIs) communicate in real time as peers. Victor is always in the visibility layer, not post-hoc. Agents can ping, restart, message, and request commands from each other — but all peer actions are **supervisor-mediated** for safety.

This collapses Victor's current two-terminal workflow (one for Claude, one for Codex) into a **single real-time three-way chat room**, with the supervisor exposing every byte of agent activity in the live UI.

The same infrastructure enables the broader **24/7 autonomous-workforce vision**: the supervisor daemon that hosts the live chat is what runs nightly maintenance tasks, autonomous bug triage, test marathons, and other supervised agent work when Victor isn't watching.

## Target architecture — 5 modules (per Codex's review)

1. **agent-supervisor** — the long-lived local daemon. Owns all agent processes. The only entity allowed to spawn, kill, restart, or reattach. Enforces policy. Routes messages. Persists audit log.

2. **drivers/claude_cli.py** — first-class Claude CLI driver. Uses `claude --input-format stream-json --output-format stream-json` for real-time event streaming. Handles session resume, permission modes, stream parsing. Codex confirmed this flag exists in local `claude --help` — the substrate is real and already streaming.

3. **drivers/codex_cli.py** — Codex CLI driver. Uses `codex exec --json` with session resume. Native sandbox modes (`--sandbox read-only|workspace-write|danger-full-access`) exposed as a driver parameter. Starting point: `ask_codex()` in `tools/agent-bridge/agent_bridge.py` — ~200 lines already exist.

4. **policies/security.py** — per-agent sandbox + capability ACLs. Codex gets native sandbox modes. **Claude needs external wrapping** (restricted worktree, separate user, ACLs, or container) because the local Claude CLI has permission modes (`acceptEdits`, `auto`, `bypassPermissions`, `dontAsk`, `plan`) but NO OS-enforced read-only sandbox. Security honesty: Claude's permission modes are in-process, not OS-level.

5. **policies/lifecycle.py** — session state machine (starting / ready / busy / idle / stalled / restarting / failed / closed) + idle TTL + stall timeout + absolute ceiling + restart backoff + crash recovery. Replaces the current "kill-on-DONE-ping" behavior.

## Terminal host (Windows) — use ConPTY

Per Codex's final v1 answer: **don't scrape terminal output, own the PTY.**

- On Windows, use ConPTY (via `pywinpty` or equivalent Python binding).
- Spawn Claude and Codex each inside their own PTY.
- Capture every byte of stdout/stderr.
- Inject stdin yourself.
- This gives the exact live terminal visibility Victor wants, in a custom UI.

The PTY is the **visibility layer** — what you read.
The event bus (sideband to the PTYs) is the **control layer** — structured messages.

Both layers work together: the PTY shows you what the agent is literally doing; the event bus routes structured actions between peers and the supervisor.

## Event bus message types

Structured channel under the PTYs, not just terminal text scraping:

- `chat_message` — free-text message from sender to target (or broadcast)
- `command_request` — "please run X" from one agent to another (supervisor-mediated)
- `command_result` — response to a command_request
- `heartbeat` — periodic liveness signal per agent
- `health` — state transitions (idle → busy, busy → stalled, etc.)
- `restart_request` — agent asks supervisor to restart another agent
- `spawn_request` — agent asks supervisor to spawn a new agent
- `close_request` — agent asks supervisor to close itself or another

## The three-way chat UI — four panes

1. **Victor's pane** — readline input, echoes his own typed messages for context
2. **Claude's pane** — live PTY output from the wrapped Claude CLI (colorized)
3. **Codex's pane** — live PTY output from the wrapped Codex CLI (colorized)
4. **System log pane** — spawns, restarts, command executions, health transitions, supervisor decisions, audit events

Every injected message (from Victor to Claude, from Codex to Claude via supervisor, etc.) is **visibly stamped** in the target pane, not hidden. "Codex said X" appears in Claude's PTY as a visible injection, marked as supervisor-routed. Victor sees the full provenance of every message.

Victor can interrupt either agent at any time via his input pane, or via commands routed through the supervisor (`/kill claude`, `/restart codex`, `/ping all`, `/history`, `/save chat-<timestamp>.md`, `/load <bundle>`).

## Peer-supervisor pattern (NOT direct-peer restart)

Critical design decision: **agents do NOT directly kill, restart, or spawn each other.** That creates restart loops, confused-deputy attacks, mutual-destruction bugs. Instead:

- Agents can `send_message` to each other (direct, via the bus)
- Agents can `request_action` from the supervisor: spawn, restart, ping, health-check, run-command
- The supervisor is the only entity allowed to actually kill/restart/spawn processes
- Every action logged with: actor, reason, result, cooldown/backoff
- Rate limits prevent DOS (A can request restart of B at most 1x per hour)

This gives the peer-coordination behavior Victor wants without the chaos.

## Stuck vs Idle (separate closure rules)

**Idle** (benign, safe to keep alive or close per policy):
- No new messages in the session
- No output stream activity
- No work explicitly requested

**Stuck** (pathological, must close):
- No output for too long while marked busy
- Repeated failed tool calls
- CPU flat + no PTY output + no file activity
- CLI process alive but non-responsive to heartbeat probe

**Close policy** — NOT just "kill on DONE ping":

1. Explicit close (manual command or supervisor-commanded)
2. Idle TTL (default ~30 min of no activity)
3. Stall timeout (default ~10 min of no stdout while busy)
4. Repeated heartbeat failure (N consecutive missed pings)
5. Absolute ceiling (default ~4 hours total session)
6. Restart backoff exhaustion (if keeps crashing, stop trying)

## 24/7 framing — service-level, not process-level

The "24/7 Claude" is NOT one immortal fragile process. It is:

- A 24/7 supervisor daemon (always running on the host)
- A persisted Claude session identity (session ID stored by supervisor, resumable via `--resume`)
- A managed child process (spawned on demand, killed on idle, restarted on crash)
- Health detection + closure rules (above)
- Audit logs + security boundaries

To Victor, it looks continuous: "there's always a Claude I can talk to." Under the hood, it's on-demand with persistent identity. Claude processes come and go; session continuity lives in the supervisor's state.

## The honest caveat about THIS session

This current Claude Code session (the one writing this plan) is NOT one of the supervised headless agents. Claude Code is an interactive runtime; it takes turns from Victor, responds, doesn't subscribe to external event streams mid-turn. For the chat to include a "Claude," the chat's Claude will be a spawned headless `claude --input-format stream-json` instance — a different runtime instance, same model and behavior, different context budget.

There's a world where Claude Code itself gains a feature to participate in external event streams (MCP push notifications, custom slash commands polling a bus). Doesn't exist today as far as confirmed. The headless Claude is functionally indistinguishable from this session; continuity lives in CLAUDE.md + `memory/` + the bundle loader.

**Phase 0 research includes confirming whether Claude Code has any hook to join external event streams.** If yes, the interactive session can participate directly. If no, the headless path is the answer.

## MVP build path

### Phase 0 — Research (1 session, tomorrow first thing)

- [ ] Verify `claude --input-format stream-json --output-format stream-json` works end-to-end. Capture sample event streams to `phase-0-research.md`.
- [ ] Verify Codex session resume via `codex exec --json --session <id>` and its output format.
- [ ] Locate Claude Code's session file format / path (where `cra` reads sessions from).
- [ ] Research Windows ConPTY Python bindings (`pywinpty`, `winpty`, `prompt_toolkit.pty`, or similar). Pick one.
- [ ] Check whether Claude Code supports MCP servers that can push events mid-session (would let THIS Claude participate in chat directly).
- [ ] Estimate API cost per session at our size (both Claude and Codex).
- [ ] Document all findings in `CLI-master-wrapper/phase-0-research.md`.

### Phase 1 — Supervisor scaffold + drivers (2-3 sessions)

- [ ] Extract `tools/agent-bridge/agent_bridge.py`'s `ask_codex` + half-done `ask_claude` into `CLI-master-wrapper/drivers/{codex_cli,claude_cli}.py`
- [ ] Create `CLI-master-wrapper/supervisor.py` main loop
- [ ] Session state machine in `CLI-master-wrapper/policies/lifecycle.py`
- [ ] Security policy stubs in `CLI-master-wrapper/policies/security.py`
- [ ] **Ship Improvement 5 first** (from `projects/bridge-improvements.md`): exit-code fail-loud, archive failed dispatches, no retry loops. Biggest live footgun, land before anything else.
- [ ] **Ship Improvement 1 second**: stall timeout (reset on stdout activity, absolute safety ceiling)
- [ ] **Ship Improvement 2 third**: keep-alive mode for continuous workers

### Phase 2 — Chat MVP (2-3 sessions)

- [ ] ConPTY host in `CLI-master-wrapper/pty_host.py` (Windows) — spawn agent inside PTY, capture stdout/stderr byte stream, inject stdin
- [ ] Event bus in `CLI-master-wrapper/bus.py` — message types from section above
- [ ] Chat UI: Python script with 4 panes (Victor / Claude / Codex / System log), readline input, colorized output, `/command` support
- [ ] **First smoke test:** Victor says "hi" → Claude responds → Codex says "hello from Codex" → Claude responds to Codex. Three-way end-to-end, every byte visible.

### Phase 3 — Peer actions (1-2 sessions)

- [ ] Agents can request actions from the supervisor: ping, health-check, restart, spawn
- [ ] Capability ACLs: who can request what from whom, enforced in `policies/security.py`
- [ ] Rate limits + restart backoff

### Phase 4 — Persistence + replay (1 session)

- [ ] Transcript persistence in `.bridge/audit/chat-<timestamp>.jsonl`
- [ ] Session resume across supervisor restarts (reload agent state from last known checkpoint)
- [ ] Replay mode: step through a past conversation

### Phase 5 — Sandbox hardening for Claude (as needed)

- [ ] External sandbox wrapper for Claude: restricted worktree, low-privilege user, ACLs, or container
- [ ] Only needed when Claude runs unsupervised with file-write permissions

### Phase 6+ — The 24/7 orchestrator on top

- [ ] Task queue + scheduler
- [ ] Cross-agent workflows (Claude plans, Codex executes, Claude reviews)
- [ ] Budget management per agent per session
- [ ] Telegram integration for escalations
- [ ] Nightly maintenance runner (health checks, cost digest, drift detection, urgent ping via Telegram)
- [ ] Autonomous bug triage pipeline (read Jira R/O → classify → generate Codex prompts → approval queue)

## Where the existing code fits

`tools/agent-bridge/agent_bridge.py` (~500 lines) is the **starting point, not a throwaway**. It already has:

- File-based queue + archive
- Codex subprocess spawning via `ask_codex()`
- **Partial Claude subprocess support via `ask_claude()`** — Codex flagged this as half-implemented; we extend it
- Session registry
- Correlation via `--in-reply-to <dispatch_id>` early-kill
- Watcher loop

What needs replacement:

- Kill-on-DONE → stall/idle-based closure
- Hardcoded Codex/Claude branches → driver abstraction
- Inbox/outbox file-only → event bus + audit log + PTY streams
- Implicit single-agent lifecycle → session state machine

What to keep (migrate into the new structure):

- Filesystem queue as one transport among others
- Archive pattern
- Dispatch ID correlation (generalize to structured event messages)
- Watcher loop pattern (generalize to multi-agent)

## Design decisions already locked

From the multi-round design conversation (Claude + Codex + Victor across 5-6 exchanges today):

1. **Supervisor daemon owns all lifecycle.** Peers do not restart each other directly.
2. **PTY = visibility layer, event bus = control layer.** Two separate channels, not one.
3. **Three-way real-time chat is the first user-facing deliverable.** Not a dashboard, not a log viewer — a live terminal chat room.
4. **Stuck detection is separate from idle detection.** Different closure rules.
5. **Claude needs external sandbox wrapping** for secure continuous mode. Codex has native sandbox.
6. **Claude CLI `--input-format stream-json --output-format stream-json` is the streaming substrate.** Confirmed in Codex's local help check.
7. **Start with Improvement 5 (exit-code handling)** — biggest live footgun, ship first even during refactor.
8. **ConPTY on Windows** for real terminal ownership, not scraped stdout.
9. **The 24/7 is service-level**, not single-process-level.
10. **THIS Claude Code session is NOT the supervised Claude in the chat** — headless instance is, unless Claude Code gains MCP push support.

## Open questions for tomorrow

1. **Does Claude Code itself have any way to participate in external event streams?** (MCP push, slash command polling, custom hooks)
2. **Budget ceiling?** Monthly API spend Victor is willing to commit to supervised 24/7 agent work?
3. **First real task types?** Nightly maintenance, autonomous bug triage, test marathons, cost monitoring, documentation drift detection? Which first?
4. **Approval flow preference?** Telegram-based? Dashboard at some local URL? Morning check-in on a report file?
5. **Internal-only or product-track?** A polished version of this architecture is commercially interesting — every company wants an "extra engineer" agent system. Worth naming upfront whether we're building for Victor's internal use only or with product eyes on it (a possible product line).

## Pointers for the next session

When tomorrow's Claude picks up this work:

1. Read `<workspace>/CLAUDE.md` (the entry point)
2. Load the Codex Mode B bridge bundle: `python "<workspace>/tools/load-context.py" codex-mode-b-bridge`
3. Read this plan: `CLI-master-wrapper/README.md` (also accessible via `load cli-master`)
4. Also worth loading for context: `projects/bridge-improvements.md` (root-cause notes + the 5 queued improvements that this project ships)
5. Start **Phase 0 research** — all research items above are discrete and parallelizable; can dispatch them via Explore sub-agents or direct Bash/Read
6. Report findings in `CLI-master-wrapper/phase-0-research.md`
7. When Phase 0 is done, move to Phase 1 with Codex Mode B dispatches for the refactor work (driver extraction, supervisor scaffold)

**Don't re-derive the architecture.** Claude and Codex already converged on it through 4+ rounds of review. This plan is the checkpoint. The next session's job is execution, not redesign. If something in the architecture feels wrong once you start building, flag it to Victor, don't silently pivot.

## Related existing docs

- `projects/bridge-improvements.md` — the 5 queued improvements + root-cause notes from 2026-04-12 retrospective. Improvements 1, 2, 5 land as part of Phase 1 of this project.
- `tools/agent-bridge/agent_bridge.py` — the code being refactored. Currently ~500 lines. Extract drivers, replace lifecycle.
- `guides/claude-codex-bridge/README.md` — current bridge operational docs. Will need updating when CLI-master-wrapper replaces the bridge.
- `guides/claude-codex-bridge/PROMPT-TEMPLATE.md` — current Codex dispatch prompt template. Still relevant for Phase 1 Codex dispatches that build the new system.
- `memory/feedback_codex_builds.md` — Codex-vs-Opus delegation policy. Still applies: Codex builds the refactor, Claude plans and reviews.
- `tools/load-context.py` — bundle loader. Phase 2b will add bundles for this new system's components once they exist.
