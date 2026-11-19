# Known issues and limitations

Current user-facing limitations. For setup and the first-room walkthrough, see [README.md](../README.md). Future direction is in [ROADMAP.md](../ROADMAP.md).

## Platform and CLI compatibility

PRIM-1 is **Windows-only**. Prime Agent runs in Ubuntu WSL under user systemd; that integration is not a Linux desktop port. Native Linux and macOS support remain planned.

External CLIs change their terminal output, permission flags, and resume behavior. A new upstream version can break readiness or delivery without a PRIM-1 code change. When reporting a regression, include the PRIM-1 commit and affected CLI versions. Driver fixtures record known output shapes; they are not a guarantee for every newer CLI release.

Grok uses its full-screen TUI. An unfamiliar startup or prompt layout can leave it in **Starting**. Codex can refuse synthetic delivery when its current prompt cannot be recognized. Raw terminal interaction remains available in both cases.

## Rooms and delivery

- Room definitions, membership, messages and receipts persist. The display window retains at most 512 events / 16 MiB; reads report eviction gaps. A new member only reads from its join boundary onward.
- Addressed messages remain shared above each member's join boundary. Recipients select wake targets, not private visibility.
- **Post to feed** prompts nobody. **Send** stores once, then independently attempts a short wake for each selected member. A held recipient does not suppress others.
- Current positive composer evidence covers the recorded Grok fullscreen layout only. Claude, native Codex, Prime, Generic Terminal and unknown layouts remain Held. Raw terminal input remains available.
- Use **Retry wake** on an eligible supported recipient's receipt, identified by its parent message ID, after resolving a temporary prompt or draft. The room feed shows it on the receipt. It rechecks readiness and may remain Held; it cannot enable an unsupported harness. A partial or unknown attempt never offers Retry; check that recipient's terminal for leftover input. Successful recipients are untouched; partial/unknown attempts and old-run bindings cannot retry. Ask a held or unconfirmed recipient through its terminal to run `room_read`. There is no automatic retry loop; do not repost to all as a substitute.
- Written confirms PTY handling; Fetched confirms a returned addressed message page. Neither means model acceptance or completion. Pending with no outcome is shown as unconfirmed, including after restart.
- Complete log corruption isolates that room and preserves its bytes; startup-corrupt or replaced histories still need separate recovery. A write/sync failure holds the current writer until the next authorized write replays and syncs its still-locked history in process. Active deliveries delay that replay. Retry an uncertain Post or Send with the same request ID, exact content and recipients: complete uncertain records are preserved, matching requests return the original message, and unknown wake attempts are never repeated.
- Join boundaries are bound to a log epoch. A missing/replaced log can establish a new boundary without exposing its existing prefix to members; stale cursors report EpochReset. Same-epoch truncation remains held. Legacy catalogs with no epoch and an already shortened nonempty log cannot prove a replacement and remain held. Failure to save a new boundary holds affected rooms while healthy rooms stay available.
- Delete erases that room's log and valid recovery fragments after removing membership. A cleanup failure explicitly reports that the room was deleted but some history remains. Directories, reparse entries and unrelated files are never erased.
- Each log refuses writes at 256 MiB. Disk rollover, old orphan files and interruption between catalog deletion and file cleanup remain open; no bounded total disk-retention completion is claimed.

The backend accepts a room default, custom, or no brief. A selected brief becomes one literal initial prompt argument only for a new conversation (explicit Fresh or no stored resume reference); an actual resume gets no task prompt. Claude adds the ordinary user-prompt envelope `PRIM-1 initial brief:\n\n{text}` so a brief such as `update` cannot become a subcommand. The source-text limit is 8192 bytes with no NUL, plus that fixed prefix. Room joins and idle transitions never type a brief into a PTY. Terminal and Prime reject all briefing. Explicit **Brief now** is a separate delivery with its own outcome. Frontend controls and installed-CLI acceptance remain tracked in the S5 plan.

None removes the task prompt. MCP initialization still supplies room protocol where configured (Claude/Codex); Codex's perception is not verified here. Grok has the room CLI but receives no automatic protocol text when None is selected.

## Generic Terminal and raw input

A bare shell is held for room wakes. Legacy direct routing accepts printable single-line shell commands. The supervisor detects supported harnesses started inside a Terminal pane and adapts their delivery framing, but it does not provide their full driver-specific work-state gating. Use a dedicated Claude Code, Codex, or Grok session when prompt/approval detection matters. Process-image detection includes a best-effort bare-Node heuristic and can misidentify other Node programs.

Pane `input` writes raw text and does not press Enter. Clipboard paste in a pane is the webview's own paste: control characters are removed and xterm brackets the text when the harness enabled bracketed paste, as a terminal does. Room **Send** stores the full body in the feed and attempts only a short wake pointer, currently for the measured Grok fullscreen layout. The recipient reads the body with `room_read`; raw input has separate semantics.

Ctrl+Shift+V is **Paste image** by default (Settings → Keys), not Windows Terminal's text paste: in Claude, Codex and Grok panes it sends the harness's own image key. With only text on the clipboard, Claude and Grok paste that text themselves (their own fallback, unbracketed by PRIM-1), while Codex shows "Failed to paste image". Paste text with Ctrl+V or Shift+Insert, or rebind Paste image to give Ctrl+Shift+V back to text paste. In a Terminal pane Ctrl+Shift+V still pastes text.

## Restart and conversation resumption

Closing PRIM-1 stops its managed process trees. Relaunch creates new PTYs; it does not reattach old ones. The backend stores an explicit per-room Resume on restart choice, default false. Historical running flags and room visits do not select this setting. Frontend startup-policy integration is tracked in the S5 plan.

Stored conversation references can resume supported harness conversations; they do not restore PRIM-1 terminal buffers. Room history replays separately. Codex resume and Restart remain held until root identity and current-conversation freshness are proven. The `--prim1-identify` helper accepts no arguments, reads its own `CODEX_THREAD_ID`, and requires native Windows Job/run attestation; a pane secret alone is insufficient. Accepted reports record provisional root evidence and retain full prior recovery records. A report alone never proves freshness after `/new` or enables native wake; the conversation follower releases resume only from a root writer lock held by the member's own `codex.exe`. While two root threads stay loaded after `/new` (about 30 minutes), the member is ambiguous and keeps its last proven conversation unless `--prim1-identify` names the current one. Recovery is exposed separately from a launch/process failure.

For a held Codex pane, stop the run, then choose **Start fresh session**. Fresh launches retain superseded recovery references and their provenance in the session catalog. Held automatic restarts are skipped before consuming a restart attempt and report a warning. An executable-search retry must preserve the original Fresh mode and launch choices; it must not turn into an ordinary Launch.

Grok can update itself at startup and exit. When the pane reports this, press **Launch** to run the updated executable; automatic relaunch after a self-update is not implemented.

## Output, diagnostics, and isolation

Only one terminal is visible at a time. Background sessions keep running, and their buffers remain available while the app runs. Under sustained renderer overload, display output may be dropped with a visible gap warning; there is no durable full transcript to recover it from. After a gap or a page reload, a pane gets back its tracked input modes (cursor keys, mouse, focus, bracketed paste, alternate screen, cursor visibility) with its next output. A pane that prints nothing keeps the new page's defaults until it does; lost text and cursor positions are not reconstructed. If WebView2's page process exits, PRIM-1 reloads the page; if its browser process exits, PRIM-1 rebuilds the window (minimized again if it was minimized). Agents, rooms and their state live in the desktop process and are not touched. At most three such recoveries run in any ten minutes; after that the window stays as it is, and closing it still shuts down gracefully. A page that stops responding but is still alive is only logged. If a rebuilt window cannot be created, PRIM-1 shuts down gracefully, so the members that were running are recorded for Resume on restart.

The audit omits raw terminal-output events and redacts room/routed-message bodies. Startup/failure diagnostics may include harness error text. Inspect logs before sharing them in an issue.

The session catalog is private state, not a metadata-only diagnostic export. It includes custom brief text, recovery history, provisional identity/run bindings, and known inputs from the last successful launch. Those launch records exclude prompt and environment values and do not prove current effective harness settings. The renderer controls typed choices, not arbitrary executable paths, argv vectors or environment values.

PRIM-1's pane permissions are protocol boundaries, not an OS sandbox between programs running under the same user. Harness provider usage and selected approval/sandbox policies still apply.

## Telemetry

- Memory is private bytes, Task Manager's **Commit size** column on the Details tab. Task Manager's default Memory column (private working set) is usually lower.
- CPU is measured only while the top-bar line or the Telemetry page is on screen. The first sample after it appears shows a dash, and CPU peaks are the highest 2-second sample seen on screen. Memory peaks also include each run Job's own peak commit, which covers time off screen.
- Console hosts appear as one PRIM-1 row, not under the agent whose terminal they serve.
- A Prime row counts only its Windows-side processes; the Linux processes run inside the WSL virtual machine.
- A WebView2 or console-host process that exits between two samples loses that interval's CPU. Agent rows keep it through Job accounting.
- A process that starts or exits while a sample is being read can be missing from that sample. An agent that is still launching can appear once under console hosts and helpers.
- A process reading can fail in either Job inventory. The page reports the number of affected IDs; memory/process counts may be incomplete, but run Job CPU still includes unreadable members. An unreadable ID is excluded from helper attribution for that sample, including if the ID was reused; it never authorizes attributing a replacement process to the run.

## Test-environment note

A full workspace test run has failed in `pty-host::windows_real_process_tests::production_spawn_contains_immediate_descendants_at_process_creation` while isolated reruns passed. The cause is not established; do not assume it is harmless. See [tests/README.md](../tests/README.md) for test entry points.
