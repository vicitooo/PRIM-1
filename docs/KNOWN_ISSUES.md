# Known issues and limitations

A short, public-facing reference for current limitations, known bugs, and version compatibility notes. Bugs file as GitHub issues; this document captures the structural notes that are stable enough to belong in the repo.

## Platform support

PRIM-1 is currently **Windows-first**. The wrapper uses Windows named pipes for the control plane and assumes ConPTY/PTY semantics that have been validated on Windows 10/11. Linux/macOS work is on the roadmap (Unix sockets for the control plane, portable-pty already abstracts the PTY layer) but is not yet validated. See `ROADMAP.md`.

## Pinned CLI versions known to work

The wrapper drives external CLI tools whose UIs evolve. Known-good versions (validated against the wrapper's drivers):

| CLI | Version |
|---|---|
| Claude Code | 2.1.226 |
| Codex CLI | 0.147.0 |
| Grok Build | 1.0.0 (`3cd0d0cbce`) |

If you upgrade any CLI and observe regressions (PTY behavior, startup timing, prompt rendering, routing behavior after injected stdin, paste-threshold behavior), record the wrapper commit and every affected CLI version together. Upstream CLI changes can break wrapper assumptions independent of wrapper code.

Grok Build 1.0.0 periodically repaints its full-screen TUI even while waiting at
the prompt, and the measured cadence varies enough that silence cannot identify
idleness. A live Grok run therefore remains lifecycle `Ready`; its measured
semantic markers independently report `Idle`, `Thinking`, or `ToolCall`.

## Behavioral notes the wrapper does not yet abstract

These are documented runtime behaviors that callers should know. Each is on the roadmap to be hidden behind a wrapper abstraction; until then, callers compensate.

### Pane sideband requires an empirically verified native-caller boundary

The pane-local Windows sideband carries no bearer token and scripts do not discover authority from runtime files. Its production boundary therefore depends on the supervisor deriving the named-pipe caller from the kernel and binding that caller to one live PTY process job and generation at mutation time. An endpoint name alone is not authority.

Release confidence remains blocked until that binding and the stable per-user desktop singleton have adversarial production-build receipts. Prime/WSL sideband access remains unclaimed until an empirical Windows/WSL boundary test proves a secure channel or a supervisor-owned proxy is added. External operators use the desktop UI.

### Multi-line content submission

`control-plane.ps1 -Action input` with multi-line content (`-Content` containing `\n`, or `-ContentFile <path>` for files with newlines) does not reliably submit on Claude Code's TUI. The text renders in the input buffer but the TUI may not accept it as a complete turn.

**Workaround:** Use the desktop terminal for interactive multi-line submission. From a pane script, send a single-line pointer ("Read <filepath> and follow instructions.") plus `-Action key -Key enter` as a separate call.

### Paste-threshold no-submit on Codex CLI

Codex CLI's TUI has a content-length threshold above which pasted content is staged as `[Pasted Content N chars]` and does not auto-submit on Enter. The threshold is around 1000 characters in observed cases.

**Workaround:** Keep pane-script `input` content under the threshold. Use the visible desktop terminal for larger interactive transfers.

### Durable audit is metadata-only

The durable audit intentionally excludes terminal output and routed-message content. Use the live desktop panes to inspect conversation content; use audit events for lifecycle, authorization, dispatch, and delivery receipts only. A receipt proves that the supervisor wrote to a pane, not that the model understood or completed the request.

## Roadmap items tracked publicly

The following are not bugs but in-progress structural improvements. They affect what consumers can rely on:

- **Reattach-after-relaunch is unsupported.** The supervisor lives in the Tauri process, so quitting the desktop shuts down its owned pane process trees. Relaunch starts new runs; it does not reattach to an earlier PTY.
