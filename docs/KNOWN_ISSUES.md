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
