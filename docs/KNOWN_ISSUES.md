# Known issues and limitations

Current user-facing limitations. For setup and the first-room walkthrough, see [README.md](../README.md). Future direction is in [ROADMAP.md](../ROADMAP.md).

## Platform and CLI compatibility

PRIM-1 is **Windows-only**. Prime Agent runs in Ubuntu WSL under user systemd; that integration is not a Linux desktop port. Native Linux and macOS support remain planned.

External CLIs change their terminal output, permission flags, and resume behavior. A new upstream version can break readiness or delivery without a PRIM-1 code change. When reporting a regression, include the PRIM-1 commit and affected CLI versions. Driver fixtures record known output shapes; they are not a guarantee for every newer CLI release.

Grok uses its full-screen TUI. An unfamiliar startup or prompt layout can leave it in **Starting**. Codex can refuse synthetic delivery when its current prompt cannot be recognized. Raw terminal interaction remains available in both cases.

## Rooms and delivery

- Room definitions and membership persist; the shared feed is memory-only and resets when the app exits.
- Feeds retain at most 512 events / 16 MiB. Reads report gaps after eviction or a restart. A new member can read from its join event onward.
- Addressed messages are visible to all room members in the shared feed. Selecting a recipient controls which terminal receives the prompt, not who can read the post.
- **Post to feed** does not interrupt or prompt a harness. Use **Send** when a member needs to act.
- Send preflights all selected members. A stopped, starting, blocked, or incompatible member can refuse the entire send before any terminal is written. Resolve its terminal prompt or send to other members individually.
- Once writing begins, a process or transport failure can leave some recipients written and others failed. Check the per-recipient receipts before retrying; resending to everyone can duplicate work.
- A **written** receipt confirms PTY handling, not model understanding or completion. Check the response or resulting work.
- Prime is raw-terminal-only and cannot receive room delivery or use pane room tools.

Automatic room briefing is optional and happens once per membership when the member can accept it. It is not resent on every relaunch. Use **Brief now** to repeat it deliberately.

## Generic Terminal and raw input

A bare shell accepts only printable single-line room commands. The supervisor detects supported harnesses started inside a Terminal pane and adapts their delivery framing, but it does not provide their full driver-specific work-state gating. Use a dedicated Claude Code, Codex, or Grok session when prompt/approval detection matters. Process-image detection includes a best-effort bare-Node heuristic and can misidentify other Node programs.

Pane `input` writes raw text and does not press Enter. Clipboard paste follows terminal behavior. Use room **Send** for framed multiline prompts rather than assuming raw input has the same delivery semantics.

## Restart and conversation resumption

Closing PRIM-1 stops its managed process trees. Relaunch creates new PTYs; it does not reattach old ones. **Settings → Continue where I left off** is enabled by default and relaunches previously running sessions as their room or lobby is entered. Turn it off for manual launch.

Stored conversation references resume supported harness conversations. They do not restore the room feed or PRIM-1 terminal buffers. Capture of a fresh Codex conversation reference is best-effort and depends on the CLI's session metadata.

If Codex reports an invalid or unresumable stored conversation, PRIM-1 drops the failed reference and explains the next action. Use **Launch**, or right-click a stopped session and choose **Start fresh session**.

Grok can update itself at startup and exit. When the pane reports this, press **Launch** to run the updated executable; automatic relaunch after a self-update is not implemented.

## Output, diagnostics, and isolation

Only one terminal is visible at a time. Background sessions keep running, and their buffers remain available while the app runs. Under sustained renderer overload, display output may be dropped with a visible gap warning; there is no durable full transcript to recover it from.

The audit omits raw terminal-output events and redacts room/routed-message bodies. Startup/failure diagnostics may include harness error text. Inspect logs before sharing them in an issue.

PRIM-1's pane permissions are protocol boundaries, not an OS sandbox between programs running under the same user. Harness provider usage and selected approval/sandbox policies still apply.

## Test-environment note

A full workspace test run has failed in `pty-host::windows_real_process_tests::production_spawn_contains_immediate_descendants_at_process_creation` while isolated reruns passed. The cause is not established; do not assume it is harmless. See [tests/README.md](../tests/README.md) for test entry points.
