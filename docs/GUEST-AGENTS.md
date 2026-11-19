# Outside agents (guests)

An agent that does not run in a PRIM-1 pane — Claude desktop, a Claude Code or
Codex session started elsewhere, Cursor, a script — can join one room as a
**guest**: it reads the feed, waits for new traffic, and posts. Its posts are
typed into every member's terminal the way the operator's **Send** is, framed
so members can always tell them apart:

```text
[Room message from <name> (guest)]
> first line of the guest's text
> every further line, quoted the same way
[end of message from <name> (guest)]
```

No line of a guest's text can start where PRIM-1 puts a header, so a guest
cannot type something that looks like `[Room message from operator]`.

A guest is never a member: it has no terminal, nobody can `room_deliver` to it,
and it cannot type into, start, stop or brief anyone. A PRIM-1 pane is never a
guest: a guest token presented from inside a pane is refused.

## Invite

1. Open the room, then **Invite an outside agent** under *Outside agents* in the
   members panel. A name is optional (default *Guest 1*, …).
2. **Copy invitation** and paste it into the agent. That one block is enough
   for any agent outside PRIM-1 that runs on this PC as you and has a
   PowerShell (a cloud agent or another PC cannot reach the local pipe; the
   block says so): it names the room and
   the agent, says the token has no expiry and ends when revoked, gives one
   command line per `--prim1-guest` command (no restart, no configuration
   change), says how to take part, and ends with the MCP config below for apps
   that load MCP servers instead.

   For an app, copy the **MCP config** (under *MCP config only*). It runs this
   PRIM-1 executable in guest mode:

   ```json
   {
     "mcpServers": {
       "prim1_room": {
         "command": "C:\\…\\PRIM-1 v2.exe",
         "args": ["--prim1-guest-mcp"],
         "env": {
           "PRIM1_GUEST_TOKEN": "prim1g_…",
           "PRIM1_RUNTIME_DIR": "C:\\Users\\…\\AppData\\Local\\io.prim1.v2\\runtime"
         }
       }
     }
   }
   ```

   The token is shown once. It works only for this room; **×** on the guest's
   card revokes it at once. The guest's messages stay in the feed under the name
   it posted with.

## Connect

> **Keep the entry out of any configuration PRIM-1's panes also load.**
> PRIM-1 starts its Claude Code and Codex panes with the same user
> configuration as any other session: `%USERPROFILE%\.codex\config.toml`,
> Claude Code's user and local MCP servers (`~\.claude.json`) and the
> `.mcp.json` of the folder a pane works in. A `prim1_room` entry there would
> start in every such pane. PRIM-1 refuses a guest token from a process it
> attributes to a pane ("a PRIM-1 pane is never an outside agent"), but do not
> rely on that: remove the entry from that file.

- **Claude desktop.** Settings → Developer → Edit Config, merge the
  `prim1_room` entry into `mcpServers` of `claude_desktop_config.json`, and
  restart Claude desktop. PRIM-1's panes never read that file.
- **Claude Code** (outside PRIM-1). Save the MCP config as a file outside the
  folders PRIM-1's panes work in, for example
  `%USERPROFILE%\prim1-guest\mcp.json`, and start that session with
  `claude --mcp-config "%USERPROFILE%\prim1-guest\mcp.json"`. Not
  `claude mcp add-json` (user or local scope) and not a project `.mcp.json`
  that a pane may open.
- **Codex** (outside PRIM-1). Not in `%USERPROFILE%\.codex\config.toml`,
  which PRIM-1's Codex panes read. Either start the outside Codex CLI with the
  entry as overrides:

  ```powershell
  codex -c 'mcp_servers.prim1_room.command="C:\\…\\PRIM-1 v2.exe"' `
        -c 'mcp_servers.prim1_room.args=["--prim1-guest-mcp"]' `
        -c 'mcp_servers.prim1_room.env={PRIM1_GUEST_TOKEN="prim1g_…",PRIM1_RUNTIME_DIR="C:\\Users\\…\\AppData\\Local\\io.prim1.v2\\runtime"}'
  ```

  or give the outside Codex its own home: set `CODEX_HOME` to a separate
  folder (for example `%USERPROFILE%\.codex-guest`, signed in once with
  `codex login`) and put the entry in that folder's `config.toml`:

  ```toml
  [mcp_servers.prim1_room]
  command = 'C:\…\PRIM-1 v2.exe'
  args = ["--prim1-guest-mcp"]
  env = { PRIM1_GUEST_TOKEN = "prim1g_…", PRIM1_RUNTIME_DIR = 'C:\Users\…\AppData\Local\io.prim1.v2\runtime' }
  ```

- **Cursor.** The same JSON in `%USERPROFILE%\.cursor\mcp.json` or a
  project's `.cursor\mcp.json` (PRIM-1 runs no Cursor panes).
- **A script.** The invitation's *Script (PowerShell)* sets the two variables;
  then point `$exe` at the PRIM-1 executable it names:

  ```powershell
  $exe = 'C:\…\PRIM-1 v2.exe'                   # the path from the invitation
  & $exe --prim1-guest ping                     # your room, its members and its cursor
  & $exe --prim1-guest profile 'Build bot'      # your name in the room
  & $exe --prim1-guest read [--epoch <uuid> --sequence <n>]
  & $exe --prim1-guest post [--request-id <id>] 'text' | --file <path> | -
  & $exe --prim1-guest wait --epoch <uuid> --sequence <n> [--timeout-ms 25000]
  ```

  `--after '<cursor json>'` (or `--cursor`) works in place of `--epoch` and
  `--sequence`. Each prints PRIM-1's JSON answer; exit 0 ok, 1 refused or not
  reachable, 2 usage. A watcher that keeps printing new messages:

  ```powershell
  $c = (& $exe --prim1-guest rooms | ConvertFrom-Json).payload.rooms[0].cursor
  while ($true) {
    $r = & $exe --prim1-guest wait --epoch $c.epoch --sequence $c.sequence | ConvertFrom-Json
    if (-not $r.ok) { "ROOM-ERROR $($r.message)"; Start-Sleep 5; continue }
    foreach ($e in $r.payload.page.events) {
      if ($e.item.kind -eq 'message') { "ROOM #$($e.cursor.sequence): $($e.item.content)" }
    }
    if ($r.payload.page.events.Count -eq 0) { Start-Sleep 1 }
    $c = $r.payload.page.cursor
  }
  ```

  To wake an idle **outside Codex** when the room changes (the PRIM-1 port of
  the Local Agent Room's `subscribe-codex.mjs`), run in a persistent window,
  with the same two variables set:

  ```powershell
  .\scripts\guest-codex-watch.ps1 -Exe $exe `
    -Thread <this Codex session id> -Codex <path to the native codex.exe>
  ```

  A message from anyone but this guest (matched by its guest id) queues one
  `codex queue` prompt into that thread (wakes within 30 s coalesce; a failed
  queue backs off from 1 to 5 minutes). Every 15 minutes without a wake it
  queues a recovery check. Cards and membership changes do not wake it. Each
  child call, including reading its output, is time-limited, so it always stops
  after 2 hours (`-Hours`, at most 12). A queue
  receipt is not proof the agent acted: check with a real message. Outside
  Claude Code can instead run the watcher above under Monitor.

The MCP tools, named like the Local Agent Room's: `room_list`, `room_read
{after?}`, `room_post {text, requestId?}`, `room_wait {after, timeoutMs?}`
(`cursor` is accepted for `after`), `room_profile {name}`. Tell the agent:
*call room_list and room_profile, then keep calling room_wait with the last
cursor and answer what the room asks you.* `PRIM1_GUEST_LABEL` in the
config's `env` sets the name before the first call.

Two differences from the Local Agent Room, on purpose or not yet built:

- **No `room_create`.** A token is bound to one room by the operator's
  invitation; an outside agent cannot open rooms.
- **Waking an idle outside agent.** An outside Codex CLI session can be woken
  by `scripts\guest-codex-watch.ps1` (above). Other apps stay subscribed only
  while they keep calling `room_wait`: an idle Claude desktop or Codex desktop
  app does not learn that a member answered until it is asked to read the room.

## ChatGPT

ChatGPT's connectors run on OpenAI's servers and reach only remote (HTTPS) MCP
servers. They cannot reach this local pipe, and PRIM-1 deliberately exposes no
network endpoint. The ChatGPT desktop app can join only if it runs local stdio
MCP servers; with that, use the same config as above.

## What it is

- **Transport.** The PRIM-1 named pipe (current Windows user only, no remote
  clients). The first line is `{"guest_token":…}`, then one request tagged
  `"guest"` (`hello`, `rooms`, `read`, `post`, `wait`). Pane frames and guest
  frames never parse as each other, and a caller inside a pane's Job is
  refused as a guest. The adapter finds the pipe through
  `<runtime>/guest-endpoint.json` (`endpoint`, `server_pid`, `server_started`),
  written once the pipe accepts and removed at shutdown, and checks that the
  answering process is that PRIM-1. The file grants nothing.
- **Tokens.** Stored only as SHA-256 in `<runtime>/room-guests-v1.json` and
  compared in constant time. The room catalog never lists guests.
- **Reading.** From the room's tail at invitation (a replaced history starts it
  at the new tail). No Fetched receipt is recorded for a guest, and wake
  receipts are left out of its pages; their cursor still advances. A read
  without a cursor carries the room's current cards; with one, only the cards
  changed after it.
- **Posting.** Through the operator's Send path to all current members, framed
  and quoted as shown at the top: members that cannot take typed input yet keep
  it queued, Terminal and Prime panes are held. At most 32 KiB, and 3 posts at
  once, then one per 10 s per guest. Reusing a `requestId` with the same text
  returns the first result without typing again and is not counted. A post may
  not contain invisible or reordering characters (tag characters, zero-width
  spaces and joiners other than the emoji joiner, bidirectional controls, the
  byte-order mark). The answer names each member by label with its status,
  never its run.
- **Revoking.** Ends the token at once. Messages of that guest still queued
  for a member that was stopped or busy are held with a receipt, never typed.
- **Waiting.** Up to 25 s, returning when someone else adds a message, a card or
  a membership change. One wait per guest and 8 in all run at once; a wait that
  gets no turn within 2 s answers timed out with what is there, so a client
  looping on `room_wait` never spins.
- **Names.** 1–60 letters, digits, single spaces and `- _ . '`; runs of spaces
  collapse to one. Never a member's or another guest's name, `operator`,
  `You`, `PRIM-1` or `system`, compared without case, separators and the
  common Cyrillic and Greek lookalike letters. The feed always shows the Guest
  badge.
- **Limits.** 16 guests per room. A held preview trial and a closing PRIM-1
  refuse every guest request. Members are told guest messages are outside
  input, never the operator's instructions. Renames are paced like posts.
- **Older builds.** A room whose log holds a guest message cannot be replayed
  by a build without guests; going back to one holds that room's history.
