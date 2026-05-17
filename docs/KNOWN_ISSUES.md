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

### Multi-line content submission

`control-plane.ps1 -Action input` with multi-line content (`-Content` containing `\n`, or `-ContentFile <path>` for files with newlines) does not reliably submit on Claude Code's TUI. The text renders in the input buffer but the TUI may not accept it as a complete turn.

**Workaround:** Use the `-Action deliver` action for multi-line content — it is driver-aware and handles submission per driver. For `input`, send a single-line pointer ("Read <filepath> and follow instructions.") plus `-Action key -Key enter` as a separate call.

### Paste-threshold no-submit on Codex CLI

Codex CLI's TUI has a content-length threshold above which pasted content is staged as `[Pasted Content N chars]` and does not auto-submit on Enter. The threshold is around 1000 characters in observed cases.

**Workaround:** Keep `input` content under the threshold, OR use `deliver` which chunks below the threshold.

### Restart action does not always reach `Ready` on Codex pane

`control-plane.ps1 -Action restart -Session codex` returns success but the pane lifecycle sometimes stays `Closed` rather than transitioning to `Ready`. An explicit follow-up `-Action start -Session codex` brings it back.

**Workaround:** After restart on Codex, check `lifecycle_state` via `-Action list`; re-issue `start` if state is `Closed`.

### Audit-event count is not a health signal

The audit log emits a `session_output` event per output chunk from the PTY. TUI animation (cursor blink, spinner frames, status bar redraws) produces these continuously regardless of whether the underlying CLI is doing real work. Counting `session_output` events as a "is the agent active?" proxy will report idle panes as busy.

**Workaround:** Use `lifecycle_state` from `-Action list`, the existence of expected output artifacts on disk, and `routed_message` events from the pane (which only fire when the agent actually sends a routed message) as the canonical signals.

### MSYS pipe buffering on `tail | awk` (Windows Git Bash)

On Windows Git Bash, the pipeline `tail -F audit.jsonl | awk '/pattern/ {...}'` buffers output for minutes before lines propagate. Each side of the pipe must be wrapped in `stdbuf -oL` to enforce line-buffered stdout:

```bash
stdbuf -oL tail -F -n 0 .runtime/audit/$(date +%F).jsonl | stdbuf -oL awk '...'
```

PowerShell `Get-Content -Wait` does not have this issue.

## Roadmap items tracked publicly

The following are not bugs but in-progress structural improvements. They affect what consumers can rely on:

- **Reattach-after-relaunch.** Today the supervisor lives in the Tauri process; closing the desktop kills the supervisor and orphans all panes. Service-extraction is on the roadmap.
- **Configurable wrapper root for the Claude pane's --add-dir.** The Claude driver computes the wrapper source-tree path so the `claude` pane gets `--add-dir <wrapper_root>` alongside `--add-dir <working_dir>`. Behavior: (1) `PRIM1_WRAPPER_ROOT` env var is the canonical override — set this to an absolute path to point Claude at any wrapper source tree. (2) If unset, the driver falls back to `<working_dir>/<name>`, where `<name>` defaults to `PRIM-1` and can be overridden via `PRIM1_WRAPPER_DIRNAME`. (3) When the working_dir basename is itself `PRIM-1` (or, for backward compatibility, `CLI-master-wrapper`), the driver treats working_dir AS the wrapper root.
- **Request ACK protocol.** Today `deliver` and `route` return success on byte-write, not on model-acceptance. An ACK echo from the pane driver is on the roadmap; until it lands, callers verify dispatch landed by polling `lifecycle_state` + checking for expected artifacts.
