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

### Grok panes run the fullscreen TUI (2026-08-29 evening)

Grok panes launch `--fullscreen`: the pane renders exactly what a normal
terminal shows (message cards with timestamps, collapsed hook chips, the
bordered composer). Measured live: alt-screen + bracketed paste at startup,
composer frame with no splash (the tracker admits the composer from either
phase), bracketed-paste delivery with embedded LF + CR submit answered by the
model, full repaint on resize (no column pinning needed). Grok deliveries use
the standard BracketedPaste framing and the general 1 MiB body cap; the
GrokMinimal framing (ESC-CR line breaks, measured 13 KiB / 256-line envelope)
remains in the codebase, tested directly, for a potential minimal-mode return.
The minimal-mode notes below are historical.

2026-08-29 late addendum — the fullscreen startup frame is ONE-SHOT and was
still loseable live (a resize invalidation or projection taint wedged a
resumed pane in `Starting` while the work-state classifier read the same
output fine — Victor's second Send refusal). The tracker now ALSO admits from
the raw stream: the composer+footer burst (❯ + Shift+Tab + Ctrl+x) in a 1 KiB
rolling window, paste-aware from the window itself, deferring while a
starting splash or the launcher menu is visible. Grok repaints that burst on
every activity, so a lost first frame self-heals. Verified over two open
cycles: readiness in ~12 s each.

2026-08-29 final addendum — the remaining launcher split: an icon-launched app
carries no TERM, and grok paints ASCII fallbacks (">" for the composer glyph)
— every glyph-anchored predicate was dead on icon launches while shell
launches (TERM set) passed. Two fixes: the pane environment is now pinned
(TERM=xterm-256color, COLORTERM=truecolor pushed; NO_COLOR / CLAUDECODE /
CLAUDE_CODE_CHILD_SESSION stripped in pty-host — panes render identically
from any launcher), and the fullscreen predicates are glyph-free (the
Shift+Tab/Ctrl+x footer is the signature; the launcher screen shows it too
and stays excluded by its own guard). Fixture: the captured no-TERM startup.
Verified: two Explorer-launched cycles, repaint completed in ~1 s each.

2026-08-29 delivery addendum — large deliveries pasted but did not submit on
Codex/Grok (an 8.4 KiB Send: all three recipients `written`, CR included, only
Claude Code acted). Measured: a TUI still ingesting a large paste swallows a
CR arriving at the fixed 1 s delay and submits cleanly once the echo settles
(reproduced in a raw ConPTY with the exact payload). The submit now waits for
a 400 ms quiet window on the pane's output after the payload (bounded at
10 s), replacing the fixed-delay-only contract. Small messages are unchanged;
the fixed delay remains the floor.

Second half of the same defect (same day, Victor's retry): Codex collapses a
large paste into a "[Pasted Content N chars]" chip and ignores Enter for a
guard window even after the echo settles — its chip renders instantly, so the
settle wait alone does not clear the guard. The submit is now
verify-and-retry: after each Enter the supervisor watches the pane for
response output (3 s window); a pane that has not started responding gets the
Enter again, three attempts total. An extra Enter into an already-submitted
empty composer is a no-op on every measured harness. Test builds default the
settle/verify timings to zero so mock deliveries keep their historical shape;
`a_swallowed_submit_is_retried_until_the_pane_responds_or_attempts_end` pins
the retry contract.

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

2026-08-29 second addendum — Grok's pane scattering had TWO renderer-side
roots, found by capturing the raw ConPTY resize flow: (a) only the VISIBLE
pane's xterm grid was ever sized, so a background pane's PTY (spawned at the
persisted real size) wrote into a default-width grid — a resumed replay was
pre-wrapped garbage before the tab was ever opened, and the reveal-refit
reflowed it into scatter (broken at full screen with zero resizes); (b) grok
pads its pinned-region lines with hard spaces to the full terminal width, so
ANY later width change makes xterm reflow those lines into scatter. Fixes:
one measured grid propagated to every pane, hidden included; a running Grok's
columns stay pinned for the life of the run (width changes apply at its next
run; rows track freely); `windowsPty: conpty` declared; resize refits
debounced to one per settle. Capture harness: `grok_resize_capture.py`
pattern — grok repaints only `CUP + ED(J) + width-padded statusline` on
resize.

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

### driver-codex classifier: numeric triggers were substring bombs (fixed 2026-08-29)

Bare `"401"`, `"403"`, `"429"` substrings classified ANY digit run as an
auth/rate blocker — a resumed replay carried 131 such runs and latched
`blocked (auth_refresh)`, refusing room deliveries to a perfectly healthy
Codex (and feeding the repeated-blocked → `error_loop` escalation seen
2026-08-28). Numeric codes now require their HTTP phrasing ("401
unauthorized", "http 403", "429 too many", …); sentence triggers unchanged.
Related and still open: the tracker clears a blocker only via a TRUSTED
clean-prompt screen, which a scrollback-style resume replay may never
produce — a false phrase-level latch during replay would still stick.
Watchlist: the day's FIRST resumed run of a harness has died silently ~30 s
in twice (Grok 10:34, Codex 12:09, both 2026-08-29); relaunch recovered both.

### driver-codex tracker: transcript text latched a phantom block (fixed 2026-09-08)

The "still open" half of the entry above happened, for four hours, on a
healthy pane. `WorkStateTracker` treated every `Blocked` detail alike and
cleared one only via a trusted clean-prompt screen **with no blocker phrase
anywhere on the visible screen**. Codex's final answer ended in
"…screenshot capture timed out."; bare `timed out` was a
`stream_disconnected` trigger; the answer stayed in the transcript above a
perfectly clean prompt, so every rescan (each keystroke echo, `/new` +
`/resume`) re-latched it and three re-latches in a minute escalated to
`error_loop`. Every room send to all members was refused before delivery —
to anyone — with a message that named nothing an operator could act on.

Now (see `CONTRACT-BLOCKED-LATCH.md`):

- **Blocker classes.** `Modal` (`workspace_trust`, `approval_prompt`,
  `plan_mode_prompt`) latches on the trusted screen exactly as before.
  `Notice` (`stream_disconnected`, `rate_limit`, `usage_limit`,
  `auth_refresh`) is detected only from fresh output, never from a screen
  rescan, and is cleared by the next clean prompt or any later activity
  (`Working…`, a tool call). A notice's text staying in the transcript is
  not a block. A modal on the same screen as stale notice text still wins.
- **Phrase precision.** Codex's `stream_disconnected` triggers are its real
  banners (`stream disconnected - retrying sampling request (`,
  `Reconnecting...`, `Reconnect failed`, `Network request disconnected
  after`, `request timed out`). Bare `timed out` / `network error` / `retry
  your request` are gone from the Codex list; Claude and Grok keep their
  lists but `timed out` became `request timed out` there too.
- **Operator-legible refusals.** Preflight errors read
  `Codex (f3f34814) can't take a routed message right now: its connection to
  the model dropped (it printed "stream disconnected") — wait for it to
  reconnect and show its prompt, or restart it from its tab.
  [session-…: work state is blocked (stream_disconnected); …]`. A room send
  says up front `Nothing was delivered to any of the 3 recipients (a room
  send is all-or-nothing).` The bracketed tail keeps the raw state.

Also open: best-effort multi-recipient delivery with a per-recipient report
instead of all-or-nothing.

### Launch failures said "process exited with code 1" and nothing else (fixed 2026-09-09)

Two panes died the same morning with the same four words and different causes:

- **Codex** — the resume-id capture (`spawn_codex_session_capture`, fresh
  launches only) took the newest `~/.codex/sessions` rollout in the cwd
  modified after spawn. An unrelated multi-agent *child* thread was appending
  to its rollout in the same cwd every few seconds, so 7 s after the 2026-09-08
  launch the child's id was persisted for the room's Codex pane. The next
  morning `codex resume <child>` printed `thread/resume failed: cannot resume
  an unloaded multi-agent v2 sub-agent through its parent` and exited 1 — and
  the pane showed only the exit code, with Restart re-running the same dead
  command.
- **Grok** — Grok 1.0.13 self-updated at startup: downloaded 1.0.24, replaced
  `~/.grok/bin/grok.exe` (old kept as `grok.exe.old`) and exited 1 to be
  restarted, ten seconds after PRIM-1 marked it idle. PRIM-1 called it a crash.
  The next Launch ran 1.0.24 fine.

Now (`CONTRACT-LAUNCH-DIAGNOSIS.md`):

- the supervisor keeps the last 2 KiB of control-stripped output per run and an
  unrequested exit's pane message quotes the harness's last real lines;
- a resumed Codex run whose tail carries `failed to resume` / `thread/resume
  failed` / `cannot resume` is reported as *Codex could not resume conversation
  <id>: "<its sentence>"*, the dead id is dropped from the catalog, and the
  message names the next click (Launch, or right-click → Start fresh session);
- a Grok run whose `bin/grok.exe` is newer than the run's start is reported as
  *Grok updated itself (now <version>) and exited to be restarted — press
  Launch*;
- the capture skips any rollout whose `session_meta` has a `parent_thread_id`
  and prefers `thread_source: "user"` when present.

Not done: auto-relaunch after a Grok self-update (one click today); capture
by the pane's own first paint instead of newest-file-in-cwd.

### Room delivery follows the harness running inside a Terminal pane (2026-09-09)

A Terminal pane in which the operator started `codex` by hand received room
messages under the generic-terminal contract — raw keystrokes, no provenance
header, then Enter — because delivery behaviour was chosen from the pane's
driver label. Codex's paste-burst detector swallowed the Enter as a pasted
newline; messages from the Claude pane stacked in the composer unsubmitted
while Codex kept replying through the feed as if nothing were pending.

Now (`CONTRACT-TERMINAL-HARNESS-DELIVERY.md`): before each delivery to a
Terminal pane the supervisor reads the pane's live process images. `grok.exe`
→ Grok, `codex*` → Codex, `claude*` → Claude Code, a bare `node` → Codex
(whose bracketed paste + Right-Arrow fence + Enter is also right for Claude
Code); shells and console hosts never count. The detected harness's delivery
contract applies — framing, fence, submit delay, the `[… message from …]`
header, and the bracketed-paste-mode requirement — and the first detection in
a run is announced in the system log. A Terminal pane running a bare shell
keeps the raw single-line contract. **Not covered:** work-state gating for a
hand-started harness (a delivery can land in a Codex modal; Codex's own
Tab-to-queue while a turn runs is not driven). Start a real Codex/Claude/Grok
pane when those matter.

## Roadmap items tracked publicly

The following are not bugs but in-progress structural improvements. They affect what consumers can rely on:

- **Reattach-after-relaunch is unsupported.** The supervisor lives in the Tauri process, so quitting the desktop shuts down its owned pane process trees. Relaunch starts new runs; it does not reattach to an earlier PTY.
