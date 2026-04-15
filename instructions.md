# CLI Master Wrapper — Inter-Agent Handshake Instructions

Single source of truth for the first live Claude ↔ Codex handshake test under the supervisor.

Both panes load this file when Victor says "run the handshake test." Claude is the initiator. Codex is the responder. Victor watches the room.

## 0. Mental model

You (Claude or Codex) are running inside a supervised terminal pane. A Rust supervisor owns your PTY and exposes a local control plane over a Windows named pipe. When you want to send a message to the other pane, you shell out to a small PowerShell helper; the supervisor injects the message into the target pane's input stream and logs the event.

- **Your identity** in this pane is either `claude` or `codex`. Pass it as `-From`.
- **Peer identity** is the other one. Pass it as `-To`.
- **Shared room** fans out to both panes plus the system log. Use `-To room -Scope room` whenever you want Victor to see an update. Treat the room as Victor's status feed.

### What incoming routed messages look like

- **Claude receives** a routed message as multiline input of the shape `\n[Direct message from codex]\n<content>\n` followed by an automatic Enter ~200ms later. The `[Direct message from <peer>]` prefix (or `[Room message from <peer>]` for room scope) is how you identify a routed message vs. a direct user input from Victor.
- **Codex receives** a routed message as a single-line flattened payload followed by an automatic Enter ~500ms later. No prefix. Incoming routed text is whitespace-collapsed.

Treat incoming routed messages as if Victor had typed them into your pane. Respond normally, following the role spec below.

## 1. Sending a routed message

The pane's working directory is the project root: `./`.

From your shell (Bash tool for Claude, shell exec for Codex), use the wrapped form below as the canonical pattern:

```bash
powershell -Command "& '.\scripts\agent-route.ps1' -From <you> -To <peer|room> -Scope <direct|room> -Content '<single-quoted message>'"
```

Examples:

```bash
# Claude -> Codex direct
powershell -Command "& '.\scripts\agent-route.ps1' -From claude -To codex -Scope direct -Content 'hello from claude'"

# Codex -> Claude direct
powershell -Command "& '.\scripts\agent-route.ps1' -From codex -To claude -Scope direct -Content 'hello from codex'"

# Either -> shared room (Victor's status feed)
powershell -Command "& '.\scripts\agent-route.ps1' -From claude -To room -Scope room -Content 'status update for Victor'"
```

Rules:

- **Always single-quote `-Content`** so `!`, `@`, `$`, special characters survive PowerShell parsing.
- **Treat the wrapped `powershell -Command "& '...\script.ps1' ..."` form as canonical on Windows.** It works from both PowerShell and Git Bash / MSYS shells, while bare `-File` calls can have their args silently mangled by MSYS before PowerShell sees them.
- **Exit code 0** = supervisor accepted the route. Non-zero = supervisor rejected (peer not running, parsing error, pipe unreachable). Always check the exit code; do not assume success.
- **Do not embed newlines** in `-Content`. Codex will flatten whitespace anyway; Claude tolerates newlines but it's noisier. Keep messages single-line.
- **Do not call `control-plane.ps1` directly** for routing. Use `agent-route.ps1` — it's the hardened wrapper that the supervisor expects.

## 2. Claude's role (initiator)

When Victor says "run the handshake test":

1. **Generate a run token.** Pick 5 random uppercase hex characters. Form `SMOKE-<chars>` (example: `SMOKE-A3F1B`). Store it for the rest of the run; do not change it.

2. **Pick the target path:**
   `./.runtime/smoke/handshake-<token>.txt`

3. **Announce to Victor via room:**

   ```bash
   powershell -Command "& '.\scripts\agent-route.ps1' -From claude -To room -Scope room -Content 'HANDSHAKE START token=<token> target=.runtime/smoke/handshake-<token>.txt dispatching codex'"
   ```

4. **Dispatch Codex** with one direct routed message containing the path, the token, and the exact reply format you expect back:

   ```bash
   powershell -Command "& '.\scripts\agent-route.ps1' -From claude -To codex -Scope direct -Content 'Handshake test from Claude. Create file at ./.runtime/smoke/handshake-<token>.txt with its entire contents being exactly the token SMOKE-<chars> (no newline, no quotes, no surrounding whitespace). When the file is written, reply to me with exactly: agent-route.ps1 -From codex -To claude -Scope direct -Content FILE_READY <token>. Do not do anything else. Do not touch any other file. Stop after replying.'"
   ```

5. **End your turn and wait.** Do not loop, poll, or spawn anything. The supervisor will deliver Codex's reply into your pane automatically. When the next input arrives, continue at step 6.

6. **Recognize the reply.** When you see input of the shape `[Direct message from codex] FILE_READY <token>` (token must match what you generated), continue. If you see anything else from Codex — an error message, a question, a mismatched token — skip to step 8 and report `HANDSHAKE FAIL`.

7. **Verify the file** using the Read tool on the exact path. The file's entire contents must equal the token string exactly.

8. **Report the result via room:**

   - **Pass** — file exists and contents match:
     ```bash
     powershell -Command "& '.\scripts\agent-route.ps1' -From claude -To room -Scope room -Content 'HANDSHAKE PASS token=<token> file verified at .runtime/smoke/handshake-<token>.txt'"
     ```
   - **Fail** — any mismatch, missing file, timeout, or error:
     ```bash
     powershell -Command "& '.\scripts\agent-route.ps1' -From claude -To room -Scope room -Content 'HANDSHAKE FAIL token=<token> reason=<one-line reason>'"
     ```

9. **Stop.** Do not retry. Do not clean up the file. Wait for Victor's next instruction.

## 3. Codex's role (responder)

When a direct routed message arrives in your pane from `claude` containing the substring `Handshake test from Claude`:

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
   powershell -Command "& '.\scripts\agent-route.ps1' -From codex -To claude -Scope direct -Content 'FILE_READY <token>'"
   ```

5. **Announce to Victor via room:**

   ```bash
   powershell -Command "& '.\scripts\agent-route.ps1' -From codex -To room -Scope room -Content 'Handshake file written at .runtime/smoke/handshake-<token>.txt token=<token> replied to claude'"
   ```

6. **Stop.** Do not touch any other file. Do not retry. Wait for the next instruction.

If the message is malformed or the write fails, report it once via room, then stop:

```bash
powershell -Command "& '.\scripts\agent-route.ps1' -From codex -To room -Scope room -Content 'HANDSHAKE FAIL (codex side) reason=<one-line reason>'"
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

Plus: the file exists at `.runtime/smoke/handshake-<token>.txt` with exact token contents, no app restart, no pane reset, no timeouts.

If any of those four signals is missing, the test failed regardless of what the agents report.

## 6. Failure mode discipline

- **Do not retry.** Every failure reports to room once and stops. The point of the first test is to surface bugs, not to paper over them.
- **Do not clean up.** Leave the temp file and partial state in place so Victor can inspect it.
- **Do not invent a fix.** If Claude thinks Codex misbehaved (wrong token, wrong path, timeout), Claude reports `HANDSHAKE FAIL` and stops. Victor debugs.
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
