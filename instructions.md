# CLI Master Wrapper — Inter-Agent Handshake Instructions

Single source of truth for the first live Claude ↔ Codex handshake test under the supervisor.

Both panes load this file when Victor says "run the handshake test." Claude is the initiator. Codex is the responder. Victor watches the room.

## 0. Mental model

You (Claude or Codex) are running inside a supervised terminal pane. A Rust supervisor owns your PTY and exposes a local control plane over a Windows named pipe. When you want to send a message to the other pane, you shell out to a small PowerShell helper; the supervisor injects the message into the target pane's input stream and logs the event.

- **Your identity** in this pane is either `claude` or `codex`. Pass it as `-From`.
- **Peer identity** is the other one. Pass it as `-To`.
- **Shared room** fans out to both panes plus the system log. Use `-To room -Scope room` whenever you want Victor to see an update. Treat the room as Victor's status feed.

### What incoming routed messages look like

- **Claude receives** a routed message as multiline input of the shape `\n[Direct message from codex]\n<content>\n` followed by an automatic Enter ~200ms later. The `[Direct message from <peer>]` prefix (or `[Room message from <peer>]` for room scope) is how you identify a routed message vs. a direct user input from Victor. Long routed messages may arrive as multiple part-labeled messages such as `[Room message from codex | part 1/3]`.
- **Codex receives** a routed message as a single-line flattened payload followed by an automatic Enter ~500ms later. Incoming routed text is whitespace-collapsed, but it now keeps an explicit provenance marker: `[Direct message from claude] <content>` or `[Room message from claude] <content>`. Long routed messages may arrive as multiple part-labeled messages.

Treat incoming routed messages as if Victor had typed them into your pane. Respond normally, following the role spec below.

## 1. Sending a routed message

The pane's working directory is the personal repo root: `<workspace>/`.

From your shell (Bash tool for Claude, shell exec for Codex), use the wrapped form below as the canonical pattern:

```bash
powershell -Command "& './scripts/agent-route.ps1' -From <you> -To <peer|room> -Scope <direct|room> -Content '<single-quoted message>'"
```

Examples:

```bash
# Claude -> Codex direct
powershell -Command "& './scripts/agent-route.ps1' -From claude -To codex -Scope direct -Content 'hello from claude'"

# Codex -> Claude direct
powershell -Command "& './scripts/agent-route.ps1' -From codex -To claude -Scope direct -Content 'hello from codex'"

# Either -> shared room (Victor's status feed)
powershell -Command "& './scripts/agent-route.ps1' -From claude -To room -Scope room -Content 'status update for Victor'"
```

Rules:

- **Always single-quote `-Content`** so `!`, `@`, `$`, special characters survive PowerShell parsing.
- **Treat the wrapped `powershell -Command "& '...\script.ps1' ..."` form as canonical on Windows.** It works from both PowerShell and Git Bash / MSYS shells, while bare `-File` calls can have their args silently mangled by MSYS before PowerShell sees them.
- **Inside the panes, use the wrapper script's absolute path.** The panes now start from `<workspace>/`, not from the wrapper root, so relative `.\scripts\...` paths are no longer reliable there.
- **Exit code 0** = supervisor accepted the route. Non-zero = supervisor rejected (peer not running, parsing error, pipe unreachable). Always check the exit code; do not assume success.
- **Do not embed newlines** in `-Content`. Codex will flatten whitespace anyway; Claude tolerates newlines but it's noisier. Keep messages single-line.
- **Do not call `control-plane.ps1` directly** for routing. Use `agent-route.ps1` — it's the hardened wrapper that the supervisor expects.

### Slash-command payloads with quotes, dollars, or Windows paths

If you need to inject a slash command whose payload is awkward to quote safely, write the payload body to a file first and let the control-plane helper read it directly.

Examples:

```bash
# Full command already assembled in the file
powershell -Command "& './scripts/control-plane.ps1' -Action input -Session claude -ContentFile './.runtime/compact-prompts/compact-full.txt'"
powershell -Command "& './scripts/agent-key.ps1' -Session claude -Key enter"

# Only the slash-command arguments live in the file
powershell -Command "& './scripts/agent-slash.ps1' -Session claude -Slash compact -ArgsFile './.runtime/compact-prompts/compact-args.txt'"
powershell -Command "& './scripts/agent-key.ps1' -Session claude -Key enter"
```

Rules:

- `-ContentFile` is only for `control-plane.ps1 -Action input`
- `-Content` and `-ContentFile` are mutually exclusive
- `agent-slash.ps1` only injects the slash command; it does **not** submit it for you
- write the content files as UTF-8 with no BOM when you control the writer

## 2. Claude's role (initiator)

When Victor says "run the handshake test":

1. **Generate a run token and timestamped target path.**

   Use the helper so the run always gets:
   - 8 uppercase hex characters
   - a UTC timestamp in the file name
   - a unique absolute and relative path pair

   ```bash
   powershell -Command "& './scripts/new-smoke-token.ps1'"
   ```

   It returns JSON with:
   - `token` — example: `SMOKE-1A2B3C4D`
   - `absolute_path`
   - `relative_path`
   - `timestamp_utc`

2. **Announce to Victor via the canonical helper:**

   ```bash
   powershell -Command "& './scripts/handshake-route.ps1' -Actor claude -Action start -Token '<token>' -Path '<absolute_path>'"
   ```

3. **Dispatch Codex** with the canonical direct message builder. Do not hand-write the preamble.

   ```bash
   powershell -Command "& './scripts/handshake-route.ps1' -Actor claude -Action dispatch -Token '<token>' -Path '<absolute_path>'"
   ```

4. **Launch the external timeout watchdog** before ending your turn. Claude cannot self-tick while idle in the PTY, so timeout failure reporting must come from a separate process.

   ```bash
   powershell -Command "Start-Process powershell -WindowStyle Hidden -ArgumentList '-ExecutionPolicy','Bypass','-File','./scripts/handshake-watchdog.ps1','-Token','<token>'"
   ```

5. **End your turn and wait.** Do not loop or retry. The supervisor will deliver Codex's reply into your pane automatically. When the next input arrives, continue at step 6.

6. **Recognize the reply.** When you see input of the shape `[Direct message from codex] FILE_READY <token>` (token must match what you generated), continue. If you see anything else from Codex — an error message, a question, a mismatched token — skip to step 8 and report `HANDSHAKE FAIL`.

7. **Verify the file** using the Read tool on the exact path. The file's entire contents must equal the token string exactly.

8. **Report the result via room:**

   - **Pass** — file exists and contents match:
     ```bash
     powershell -Command "& './scripts/handshake-route.ps1' -Actor claude -Action pass -Token '<token>' -Path '<absolute_path>'"
     ```
   - **Fail** — any mismatch, missing file, timeout, or error:
     ```bash
     powershell -Command "& './scripts/handshake-route.ps1' -Actor claude -Action fail -Token '<token>' -Reason '<one-line reason>'"
     ```

9. **Stop.** Do not retry. Do not clean up the file. Wait for Victor's next instruction.

## 3. Codex's role (responder)

When a direct routed message arrives in your pane from `claude` that begins with the exact canonical preamble `Handshake test from Claude.`:

1. **Parse** the path and the token from the message body. Both are given explicitly as literal strings.

2. **Ensure the parent directory exists:**

   ```bash
   powershell -Command "& { New-Item -ItemType Directory -Force -Path (Split-Path -Parent '<path>') | Out-Null }"
   ```

3. **Write the file with the token as its exact contents, no trailing newline, no BOM:**

   ```bash
   powershell -Command "& { [System.IO.File]::WriteAllText('<path>', '<token>', [System.Text.UTF8Encoding]::new([bool]0)) }"
   ```

4. **Reply to Claude** with the exact acknowledgement string Claude asked for:

   ```bash
   powershell -Command "& './scripts/handshake-route.ps1' -Actor codex -Action ready -Token '<token>'"
   ```

5. **Announce to Victor via room:**

   ```bash
   powershell -Command "& './scripts/handshake-route.ps1' -Actor codex -Action status -Token '<token>' -Path '<path>'"
   ```

6. **Stop.** Do not touch any other file. Do not retry. Wait for the next instruction.

If the message is malformed, missing the canonical preamble, or the write fails, report it once via room, then stop:

```bash
powershell -Command "& './scripts/handshake-route.ps1' -Actor codex -Action fail -Token '<token-if-known-or-UNKNOWN>' -Reason '<one-line reason>'"
```

## 4. Trigger prompts (for Victor to paste)

**Paste into Claude's pane first:**

> Read `./instructions.md`. Execute your role from section 2 (Claude's role — initiator). Begin now.

**Then paste into Codex's pane:**

> Read `./instructions.md`. Section 3 is your role (Codex — responder). Stand by for Claude's routed message and respond as instructed.

Order matters only weakly (Codex can be primed before or after Claude starts). What matters is that Codex has read its role before Claude's dispatch arrives, otherwise Codex will try to interpret the incoming message without context.

Important:

- when you shell out, prefer the exact wrapped commands from this file instead of rewriting them
- do not substitute `False` for `[bool]0` in the file-write command; the wrapped form is written that way specifically so Git Bash / MSYS and nested PowerShell both preserve the no-BOM intent

## 5. What success looks like (Victor's view)

In order, the room feed should show:

1. `HANDSHAKE START token=<token> target=... dispatching codex` — from claude
2. `Handshake file written at ... token=<token> replied to claude` — from codex
3. `HANDSHAKE PASS token=<token> file verified at ...` — from claude

Plus: the file exists at the timestamped path returned by `new-smoke-token.ps1`, with exact token contents, no app restart, no pane reset, and no timeout FAIL.

If any of those four signals is missing, the test failed regardless of what the agents report.

## 6. Failure mode discipline

- **Do not retry.** Every failure reports to room once and stops. The point of the first test is to surface bugs, not to paper over them.
- **Do not clean up.** Leave the temp file and partial state in place so Victor can inspect it.
- **Do not invent a fix.** If Claude thinks Codex misbehaved (wrong token, wrong path, timeout), Claude reports `HANDSHAKE FAIL` and stops. Victor debugs.
- **Do not hand-write the canonical route strings.** Use `new-smoke-token.ps1`, `handshake-route.ps1`, and `handshake-watchdog.ps1` so the timestamped path, canonical preamble, and timeout semantics stay aligned.
- **Do not talk to each other outside the protocol.** The only messages during this test are the ones specified above. No chitchat, no status probes, no "are you there" pings.

## 7. Scope of this document

This is the atomic first test — one round, one file, one verification. It exercises:

- Victor → Claude direct (the trigger)
- Claude → room (status announcement)
- Claude → Codex direct (the dispatch — the path Codex just hardened)
- Codex file-system side effect (ground truth)
- Codex → Claude direct (the reply — the path that previously crash-restarted the app)
- Codex → room (status announcement)
- Claude → room (verification result)

If this round passes cleanly, the next test expands to continuous multi-round chat. Until then, one round at a time.
