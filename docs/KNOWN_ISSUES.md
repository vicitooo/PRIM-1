# Known issues and limitations

A short, public-facing reference for current limitations, known bugs, and version compatibility notes. Bugs file as GitHub issues; this document captures the structural notes that are stable enough to belong in the repo.

## Platform support

PRIM-1 is currently **Windows-first**. The wrapper uses Windows named pipes for the control plane and assumes ConPTY/PTY semantics that have been validated on Windows 10/11. Linux/macOS work is on the roadmap (Unix sockets for the control plane, portable-pty already abstracts the PTY layer) but is not yet validated. See `ROADMAP.md`.

## Pinned CLI versions known to work

The wrapper drives external CLI tools whose UIs evolve. Known-good versions (validated against the wrapper's drivers):

| CLI | Version |
|---|---|
| Claude Code | 2.1.109 |
| Codex CLI | 0.120.0 |

If you upgrade either CLI and observe regressions (PTY behavior, startup timing, prompt rendering, routing behavior after injected stdin, paste-threshold behavior), record the wrapper commit + Claude version + Codex version together. Upstream CLI changes can break wrapper assumptions independent of wrapper code.

## Behavioral notes the wrapper does not yet abstract

These are documented runtime behaviors that callers should know. Each is on the roadmap to be hidden behind a wrapper abstraction; until then, callers compensate.

### Control-plane pane scoping is not yet a same-user security boundary

The current Windows control plane uses bearer files under the current user's app-local runtime directory. Current-user ACLs exclude other OS users, but Claude, Codex, and other native panes run as the same Windows user and can read the master or peer bearer files. Target checks therefore prevent accidental misuse; they do not yet isolate a hostile same-user pane. A restart can also replace a pane generation after authorization but before a queued write, and two concurrent desktop starts can race while publishing randomized endpoints.

The production boundary remains blocked until bearer authority is removed, named-pipe callers are derived from the kernel and bound to one live PTY process job plus generation at mutation time, and a stable per-user desktop singleton is held. Prime/WSL sideband access remains unclaimed until an empirical Windows/WSL boundary test proves a secure channel or a supervisor-owned proxy is added. Do not treat current DACLs, bearer redaction, or endpoint randomization as peer isolation.

### Multi-line content submission

`control-plane.ps1 -Action input` with multi-line content (`-Content` containing `\n`, or `-ContentFile <path>` for files with newlines) does not reliably submit on Claude Code's TUI. The text renders in the input buffer but the TUI may not accept it as a complete turn.

**Workaround:** Use the `-Action deliver` action for multi-line content — it is driver-aware and handles submission per driver. For `input`, send a single-line pointer ("Read <filepath> and follow instructions.") plus `-Action key -Key enter` as a separate call.

### Paste-threshold no-submit on Codex CLI

Codex CLI's TUI has a content-length threshold above which pasted content is staged as `[Pasted Content N chars]` and does not auto-submit on Enter. The threshold is around 1000 characters in observed cases.

**Workaround:** Keep `input` content under the threshold, OR use `deliver` which chunks below the threshold.

### Restart action does not always reach `Ready` on Codex pane

`control-plane.ps1 -Action restart -Session codex` returns success but the pane lifecycle sometimes stays `Closed` rather than transitioning to `Ready`. An explicit follow-up `-Action start -Session codex` brings it back.

**Workaround:** After restart on Codex, check `lifecycle_state` via `-Action list`; re-issue `start` if state is `Closed`.

### Durable audit is metadata-only

The durable audit intentionally excludes terminal output and routed-message content. Use the live desktop panes to inspect conversation content; use audit events for lifecycle, authorization, dispatch, and delivery receipts only. A receipt proves that the supervisor wrote to a pane, not that the model understood or completed the request.

## Roadmap items tracked publicly

The following are not bugs but in-progress structural improvements. They affect what consumers can rely on:

- **Reattach-after-relaunch.** Today the supervisor lives in the Tauri process; closing the desktop kills the supervisor and orphans all panes. Service-extraction is on the roadmap.
- **Configurable wrapper root for the Claude pane's --add-dir.** The Claude driver computes the wrapper source-tree path so the `claude` pane gets `--add-dir <wrapper_root>` alongside `--add-dir <working_dir>`. Behavior: (1) `PRIM1_WRAPPER_ROOT` env var is the canonical override — set this to an absolute path to point Claude at any wrapper source tree. (2) If unset, the driver falls back to `<working_dir>/<name>`, where `<name>` defaults to `PRIM-1` and can be overridden via `PRIM1_WRAPPER_DIRNAME`. (3) When the working_dir basename is itself `PRIM-1` (or, for backward compatibility, `CLI-master-wrapper`), the driver treats working_dir AS the wrapper root.
- **Request ACK protocol.** `request_ack` now means the target pane reacted after the write (output, work-state transition, or routed message), not that the model accepted or completed the task. If a pane accepts bytes but does not react within `PRIM1_REACTION_WINDOW_SECS`, the supervisor emits `dispatch_no_reaction` plus a Critical alert. Task-level acceptance still requires a later pane signal or expected artifact.
