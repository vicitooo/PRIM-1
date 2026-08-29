# Known issues and limitations

A short, public-facing reference for current limitations, known bugs, and version compatibility notes. Bugs file as GitHub issues; this document captures the structural notes that are stable enough to belong in the repo.

## Platform support

PRIM-1 is currently **Windows-first**. The wrapper uses Windows named pipes for the control plane and assumes ConPTY/PTY semantics that have been validated on Windows 10/11. Prime is the one explicit cross-boundary driver: it runs inside the Ubuntu WSL distribution under a user-systemd transient service while the Windows supervisor retains the outer ConPTY job. This is not a general Linux desktop port. Native Linux/macOS work remains on the roadmap. See `ROADMAP.md`.

## Pinned CLI versions known to work

The wrapper drives external CLI tools whose UIs evolve. Known-good versions (validated against the wrapper's drivers):

| CLI | Version |
|---|---|
| Claude Code | 2.1.226 |
| Codex CLI | 0.147.0 |
| Grok Build | 1.0.0 (`3cd0d0cbce`) |
| Prime Agent (Ubuntu WSL) | 0.7.0 |

If you upgrade any CLI and observe regressions (PTY behavior, startup timing, prompt rendering, routing behavior after injected stdin, paste-threshold behavior), record the wrapper commit and every affected CLI version together. Upstream CLI changes can break wrapper assumptions independent of wrapper code.

Grok Build 1.0.0 periodically repaints its full-screen TUI even while waiting at
the prompt, and its independent MCP spinner can repaint continuously, so
ordinary silence cannot identify readiness or idleness. PRIM launches each run
with a fresh native `--session-id` and holds it in lifecycle `Starting` until a
bounded exact-run tracker observes the measured ordered cursor-hide/show startup
frames: `Starting session…`, then a later full-screen Home repaint containing
the interactive composer and both shortcut labels. Partial and spinner-only
frames do not admit, replacement runs cannot inherit progress, and no timeout
grants readiness. The optional telemetry banner is non-modal in the pinned build
and is ignored. After admission, only measured semantic markers report `Idle`,
`Thinking`, or `ToolCall`; no generic quiet timer runs. A Grok UI/copy change can
therefore fail closed in `Starting` until the pinned markers are re-measured.
The fresh native session ID intentionally creates a separate Grok conversation
record for each PRIM run. PRIM never reuses or deletes Grok-owned history, so
long-lived installations should manage that history through Grok's own tools.

## Behavioral notes the wrapper does not yet abstract

### Grok startup detection is measured, and Grok's paint changes server-side

2026-08-28 late: with an unchanged grok.exe (1.0.5, Aug 20), Grok's startup
paint changed under it — splash text became "Signing in… starting your
session.", the ❯ composer glyph became ">", and the whole startup collapsed
into one repaint frame with the `minimal · /help` statusline written outside
any cursor hide/show cycle. The tracker's old two-ordered-frames measure wedged
every run in `Starting` (brief refused, deliveries refused, operator typing
refused). Re-measured the same night: the statusline is now the ready signal,
evaluated on completed frames AND on the settled screen at chunk end; the new
splash phrasing is recognized; the fullscreen path keeps its ordered measure.
If Grok's UI changes again, the failure mode is the same fail-closed wedge —
compare a raw ConPTY capture against `StartupTracker`'s predicates (fixture:
`crates/driver-grok/src/fixtures/grok-startup-minimal-single-frame-v105.json`).

2026-08-29 addendum — a RESUMED grok (`--resume=<id>`) replays its transcript
scrollback-style: no clear, no cursor hide/show frames, so the viewport
projection never becomes trusted and the old tracker wedged in `Starting` with
a fully interactive pane. Third measured acceptance path: the raw-stream
`minimal · /help` statusline (bracketed paste on, no launcher text,
chunk-spanning tail). Two grok-side oddities observed once each and not yet
explained: (a) the day's first resumed grok exited silently ~30 s after
"session loaded" (no log line, no crash artifact); (b) one subsequent launch
hung BEFORE grok's logging initialised (zero unified.jsonl lines, ~5 s CPU) —
remedy: taskkill the grok.exe, Stop the pane, Launch again. Watch for
recurrence; grok's own `active_sessions.json` can also hold a stale dead-pid
entry, which did NOT block resumes in testing.

### Room briefs are owed once per membership (2026-08-29; retires the relaunch-miss issue)

The brief is delivered once per MEMBERSHIP (`briefed_member_ids` in the room
catalog): delivery marks, removal clears, re-adding re-briefs, relaunches are
quiet, manual Brief now is unconditional. The 2026-08-28 "owed brief missed on
relaunch" observation is retired by this contract change — per-run re-briefing
was the wrong behaviour (it re-briefed a finished team on the next morning's
open); the one unexplained non-delivery was never reproduced under test.

These are documented runtime behaviors that callers should know. Each is on the roadmap to be hidden behind a wrapper abstraction; until then, callers compensate.

### Pane sideband requires an empirically verified native-caller boundary

The pane-local Windows sideband carries no bearer token and scripts do not discover authority from runtime files. Its production boundary therefore depends on the supervisor deriving the named-pipe caller from the kernel and binding that caller to one live PTY process job and generation at mutation time. An endpoint name alone is not authority.

The native Windows boundary and stable per-user desktop singleton still require
exact-artifact adversarial receipts for each release candidate. Prime/WSL is
intentionally ineligible for the named-pipe sideband because Windows Job
membership cannot identify Linux tasks. No bearer fallback exists; external
operators use the desktop UI.

Claude Code and Codex receive a PRIM-owned session-scoped stdio MCP child for
model-facing `ping`, `room_read`, `room_post`, and `room_deliver`. Grok Build
1.0.0 still gets no MCP child (its TUI has no session-scoped plugin/config flag
and `GROK_HOME` also owns session/log storage), but since 2026-08-28 every
non-Prime pane carries `PRIM1_PANE_SECRET` and `PRIM1_CLI`: a Grok shell tool
runs `& "$env:PRIM1_CLI" --prim1-room ping|read|post|deliver …` and is
identified by the per-run secret when its process is outside the pane Job.
Grok therefore reads, posts, and delivers like any other member; only the
tool-discovery step differs (the room brief tells it which path it has).
Prime remains the open case: its WSL launch forwards no environment, so the
CLI-through-interop line comes after the first live retest.

### Prime is raw-input-only in the current release

Prime supports `Normal` permission only, one qualified absolute path inside the
Ubuntu distribution, and raw operator terminal input. Synthetic routed delivery
and pane-sideband actions are rejected before write. Prime startup also requires
an operational Ubuntu user-systemd manager and an absolute executable
`prime-agent`; a stale-service cleanup failure blocks only Prime create/start.

### Multi-line content submission

`control-plane.ps1 -Action input` with multi-line content (`-Content` containing `\n`, or `-ContentFile <path>` for files with newlines) does not reliably submit on Claude Code's TUI. The text renders in the input buffer but the TUI may not accept it as a complete turn.

**Workaround:** Use the desktop terminal for interactive multi-line submission. From a pane script, send a single-line pointer ("Read <filepath> and follow instructions.") plus `-Action key -Key enter` as a separate call.

### Paste-threshold no-submit on Codex CLI

Codex CLI's TUI has a content-length threshold above which pasted content is staged as `[Pasted Content N chars]` and does not auto-submit on Enter. The threshold is around 1000 characters in observed cases.

**Workaround:** Keep pane-script `input` content under the threshold. Use the visible desktop terminal for larger interactive transfers.

### Room history is bounded and process-memory-only

Room definitions and membership survive restart, but room messages and delivery
details do not. Each room retains at most 512 events / 16 MiB while the desktop
process is alive. A reader that falls behind receives an explicit eviction or
epoch-reset gap; PRIM-1 does not silently reconstruct missing conversation from
the metadata audit.

### Durable audit is metadata-only

The durable audit intentionally excludes terminal output and routed/room-message
content. Use the live desktop panes and room feed to inspect conversation
content; use audit events for lifecycle, authorization, membership, dispatch,
and delivery receipts only. A receipt proves that the supervisor wrote to a
pane, not that the model understood or completed the request.

## Roadmap items tracked publicly

The following are not bugs but in-progress structural improvements. They affect what consumers can rely on:

- **Reattach-after-relaunch is unsupported.** The supervisor lives in the Tauri process, so quitting the desktop shuts down its owned pane process trees. Relaunch starts new runs; it does not reattach to an earlier PTY.
