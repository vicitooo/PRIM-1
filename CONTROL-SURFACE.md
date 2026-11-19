# PRIM-1 — Control Surface

PRIM-1 is desktop-first. The desktop UI is the operator authority surface;
pane-local scripts are deliberately narrower and cannot substitute for it.

## Desktop UI

The desktop owns:

- `SessionId`-addressed create, rename, reorder, delete, launch, stop, restart,
  terminal input, and resize
- an ordered, atomically persisted session inventory and snapshots
- native Windows workspace and per-session working-directory selection, plus a
  backend-qualified absolute Ubuntu path for Prime
- visible `Normal` / `Unsafe` permission profiles, with `Normal` as the default
- ordered `RoomId` create, rename, reorder, membership, delete, explicit restart
  selection, and editable room-default brief settings
- a bounded visible room feed whose **Post** action writes no PTY except to the
  members designated Supervisor (all three grants, A16), plus explicit
  one-member / **Send All** delivery with per-recipient status (every receipt in
  the room feed; the composer status names the next action, full text in Details)
- visible runtime diagnostics
- a read-only resource sample (`telemetry_snapshot`): PRIM-1's own process,
  WebView2 and console hosts, then each live run's Job, every process counted
  once; the renderer requests it only while telemetry is on screen

These operations use in-process Tauri commands. They are not exposed through a
master bearer, an info file, or an unaffiliated operator pipe. The one file
naming the pipe, `<runtime>/guest-endpoint.json`, grants nothing: an outside
agent also needs the room's invitation token ([Outside agents](#outside-agents-guests)).

Every confirmation (Start fresh on a running session, Close session, Delete
room, Load room) awaits the native `confirm_action(message, ok_label)` dialog
and acts only on an explicit yes; Cancel, a closed dialog or a failed dialog
does nothing. `window.confirm` never gates an action: the dialog plugin
replaces it with an async invoke whose result is always truthy.

The operator schemas reject unknown fields. Lifecycle/input requests carry one
`session_id`; room actions carry opaque `room_id` / `session_id` values and
typed recipient selection.
The Rust boundary derives operator provenance. Mutable labels are never
authority. Create accepts a driver, optional label, typed permission profile,
optional `launch_choices` (model, effort and room-default/custom/None brief),
and an optional room ID for initial membership. Start may replace saved launch
choices for a stopped session; omitted choices preserve them. A supplied
`launch_choices` is a complete replacement, not a patch: omitted fields inside
it use their defaults, so clients retain and send the full object when editing.
Prime create may additionally
carry one typed absolute Ubuntu working
directory; the backend canonicalizes and identity-binds it, and the UI prefills
the qualified Ubuntu home. The backend validates typed choices and constructs
their driver arguments. Launch accepts no arbitrary renderer-controlled program,
argument vector, environment, or native working-directory path. A launch miss on PATH
is shown on the pane; `search: true` asks the backend to walk the usual install
locations (the renderer still cannot supply a program path). Windows folder paths
enter through the native Rust picker. Room definitions and membership persist;
room content persists in synced JSONL with a bounded replayed display window.
Deleting a room is refused during an in-flight delivery; otherwise it removes
the room from the catalog and leaves its sessions in the lobby. Its canonical
room log and valid room-owned torn-tail files are then erased nonrecursively.
If cleanup fails, RoomDeleted still reflects the committed catalog change and
the response explicitly says history cleanup is incomplete; it does not invite
another delete. Unrelated files, directories and reparse entries are untouched.

A chosen task brief is sent only for a new conversation (explicit Fresh or no
stored resume reference), as one literal final argv operand after `--`; actual
resume receives no command-line task prompt. A107 may separately queue a durable
continuation for interrupted Working members (see below). Claude uses the ordinary user-prompt envelope
`PRIM-1 initial brief:\n\n{text}`. Source text is limited to 8192 bytes with no
NUL; the fixed Claude prefix is additional. Joining a room or becoming idle
never types a brief. Explicit **Brief now** uses the Send path and reports its
result. Terminal and Prime reject briefing.

Visible terminal shortcuts:

- `Ctrl+Tab` / `Ctrl+Shift+Tab` — select the next / previous session tab
- `Ctrl+Shift+T` — open Attach a harness
- `Ctrl+Shift+W` — close the active fully stopped session after confirmation
- `Ctrl+Shift+Left` / `Ctrl+Shift+Right` — move the focused tab
- `Ctrl+Shift+C` — copy the DOM selection, or the last active terminal selection
- `Ctrl+V` / `Shift+Insert` — the webview's own paste into the focused running pane: control characters removed, CRLF turned into CR, bracketed when the harness enabled bracketed paste; a clipboard holding only an image sends the harness its paste-image key
- `Ctrl+C` — copy a selection; otherwise the interrupt
- `Ctrl+Shift+V` / `Enter` / `Shift+Enter` — paste image / submit / new line, sent as each harness's own key (`apps/desktop/src/harness-keys.ts`); in a Terminal pane Ctrl+Shift+V pastes text, since a shell has no image paste

Settings → Keys rebinds copy, paste text, paste image, submit, new line and interrupt once for every harness. A chord belongs to the user's binding, else to an action's default, else to its convention (Ctrl+Insert, Shift+Insert, Ctrl+Shift+C/V), so a binding takes a convention chord over and an unclaimed convention keeps working. An action not verified for a harness reaches it as typed.
- `F1` — open help
- `F11` — toggle fullscreen

## Room save and load (A92)

Normal rooms only. Tauri commands follow the task-folder pattern: the pickers
`choose_room_save_path(label)` and `choose_room_source()` only open the native
dialog and return a path (or null); `export_room(room_id, path)`,
`inspect_room_source(path)` and `import_room(path, source_room_id?)` act. A
held trial refuses all of them, in the renderer and in the supervisor.

- **File.** `<room>.prim1room`, UTF-8 JSON Lines. Line 1 is the manifest
  (`"record":"prim1_room_package","version":1`): the room (`room_id`, `label`,
  `member_ids`, `membership_revision`, `brief_on_join`, `brief_template`,
  `join_floor_epoch`, `join_floors`, `history_restarted`) and each member
  (`session_id`, `label`, `driver`, `working_directory` path,
  `permission_profile`, `launch_choices`, `harness_session_id`, and the current
  hold as `recovery` with its `original_id`), plus `feed` (`records`,
  `last_sequence`, `sha256` of the exact log bytes). The catalogs' field
  names; no session-registry dependency. Lines 2..n are the room log exactly
  as written. Never included: conversation files, `last_launch`, run state,
  folder identity, earlier recovery history (the save says so when a member
  had any). A hold reason travels unchanged up to 16,384 characters; Save
  refuses, naming the member, rather than cut a longer one.
- **Save.** The committed log prefix is read through the room writer's own
  handle under the rooms lock only (sessions are captured first and the
  session registry released); the append position is restored on every path,
  and if that restore fails the writer is held for replay. Building, checking
  (the file must pass the load parser) and the synced temp-file write happen
  after the lock. `paused_ms` reports how long room traffic waited. A room that
  needs history recovery is refused.
- **Sources.** `choose_room_source` offers "PRIM-1 room (*.prim1room)" and
  "PRIM-1 runtime copy (room-catalog-v1.json)". `inspect_room_source` returns
  `{source_room_id, label, members, records, unavailable}`: one entry for a
  room file, one per room for a runtime copy. A runtime copy is read only: its
  room catalog, session catalog and `rooms/<id>.jsonl`, each opened read-only
  under a shared lock and never modified. It is refused when it is this app's
  own runtime (canonical path compare), when a log is locked (its PRIM-1 is
  running), and when it lists a room or session ID twice (the message names
  it). Any other unreadable log marks only its room `unavailable`, with the
  reason; that room cannot be chosen and its load refuses. A torn last record
  in a copy is left out and reported. The chosen room becomes
  the same in-memory package and goes through the same checks and import.
- **Load.** Load room… picks a source, inspects it, lists a runtime copy's
  rooms by number when it has several, then confirms: `Load "<label>": N
  members, M feed records. A new room is created; nothing starts.` Input is
  bounded and untrusted: at most 1 MiB of manifest plus 256 MiB of log; the
  manifest is strict; the feed must match its count, last sequence and digest
  (a file cut at a record boundary is incomplete); IDs must be canonical UUIDs
  before any store path is built; the log is replayed with the writer's own
  rules (header, room/epoch identity, sequence, schema, cards) before anything
  is written, and again when the new log is opened. A refusal ends with
  "Nothing was changed." and leaves the runtime byte-identical. The loaded room
  becomes the work context. A display refresh that fails after a committed
  load says so ("Room loaded; the display did not refresh: …") and does not
  invite a second load.
- **New identities.** A new room ID, epoch and session IDs. Every session ID in
  the feed is remapped, former members to fresh IDs, so no old line can credit
  a session that already lives here. A taken room label gets " (loaded)".
  Prime members are left out while Prime is parked.
- **Floors.** Kept only when the saved floor epoch is the log's header epoch,
  every member has one and each is at most the tail. Otherwise (including v1
  rooms without floors) loaded members see only traffic after the load; this
  is a stated visibility change, and the operator still reads the whole feed.
- **Resume state.** Read-only exact-name checks: Claude
  `<config>\projects\<folder with non-alphanumerics as '-'>\<id>.jsonl`;
  Grok `~\.grok\sessions\<percent-encoded folder>\<id>\` (folder names are
  decoded, never built); one level, at most 5,000 folders, no links followed.
  A saved hold stays held with its original ID, provenance and reason
  unchanged ("held in the saved room" is report text only); an ID already
  used here (live or held) or twice in the file is held; Codex is held; a
  missing working folder uses the workspace and needs history. Only *resumes*
  keeps a live ID. Unsafe profiles are kept as saved and named in the load
  summary. The check sees only this runtime: a conversation still driven by
  the PRIM-1 a copy came from reads as *resumes* here, and nothing starts it.
- **Crash safety.** The log is written and replayed first; the members are then
  saved held (`room_import_pending`), the registry is released, and the room
  catalog commits. A failed room-catalog save is confirmed by reading it back:
  not saved, the held members are removed and the log erased ("Nothing was
  changed."); if that undo fails, they stay in the lobby, held, and the error
  says so. Only after the commit are holds released to each member's state.
  A crash in between leaves held copies that resume nothing.
- **Known limitation.** There is no startup reconciliation. If PRIM-1 stops
  after the log copy is written but before the room catalog commits, the
  unused `rooms/<new room id>.jsonl` stays on disk, listed nowhere, and the
  held member copies stay in the lobby until they are deleted by hand.

## Pane-local PowerShell helper

`scripts/control-plane.ps1` supports these seven actions; the separate Codex
identity helper below is an additional sideband request, not a script action:

| Action | Required fields | Wire kind |
|---|---|---|
| `ping` | none | `ping` |
| `wait_quiet` | `-Session`, `-QuietSec`, `-TimeoutSec` | `wait_quiet` |
| `input` | `-Session`, `-Content` or `-ContentFile` | `send_input` |
| `key` | `-Session`, `-Key` | `send_key` |
| `room_read` | optional paired `-CursorEpoch`, `-CursorSequence` | `room_read` |
| `room_post` | `-Content` or `-ContentFile` | `room_post` |
| `room_deliver` | `-Recipient` (member label, session id, or `all`), `-Content` or `-ContentFile` | `room_deliver` |

The endpoint comes from a nonblank `PRIM1_CONTROL_PLANE_ENDPOINT`. An explicit
`-Endpoint` may be supplied only by isolated tests using a fake named-pipe
responder. There is no runtime-directory fallback.

The exact self-pane alias comes from the supervisor-injected
`PRIM1_PANE_IDENTITY`. Use that value for `-Session`; display labels such as
“Claude” or “review” are not authority.

When no endpoint is available, the stable failure is:

```text
PRIM1_CONTROL_PLANE_ENDPOINT is not set. External operator control is unavailable; use the PRIM-1 desktop UI.
```

Examples from a supervised pane with its injected environment, from the checkout root:

```powershell
.\scripts\control-plane.ps1 -Action ping -Quiet
.\scripts\control-plane.ps1 -Action input -Session $env:PRIM1_PANE_IDENTITY -Content 'status'
.\scripts\control-plane.ps1 -Action input -Session $env:PRIM1_PANE_IDENTITY -ContentFile 'D:\tmp\prompt.txt'
.\scripts\control-plane.ps1 -Action key -Session $env:PRIM1_PANE_IDENTITY -Key enter
.\scripts\control-plane.ps1 -Action wait_quiet -Session $env:PRIM1_PANE_IDENTITY -QuietSec 2 -TimeoutSec 10
.\scripts\control-plane.ps1 -Action room_read -Quiet -PassThruJson
.\scripts\control-plane.ps1 -Action room_post -Content 'Status from this pane'
.\scripts\control-plane.ps1 -Action room_deliver -Recipient 'Reviewer' -Content 'Please review the change.'
```

Behavior:

- named-pipe-only; connection and response failures fail closed
- request JSON contains no token, info path, source identity, or compatibility idle flag
- `input` writes raw text and does not press Enter
- `Content` and `ContentFile` are mutually exclusive
- `ContentFile` is strict UTF-8 (BOM optional); malformed UTF-8 fails closed, and CR/LF/trailing whitespace are preserved
- `room_read`, `room_post`, and `room_deliver` derive the exact room and sender
  from the calling pane (kernel Job, or the pane secret when the shell runs
  outside the Job); they accept no `-Session`, `RoomId`, sender, or peer
  authority — `room_deliver`'s `-Recipient` names a member of the caller's own
  room and is gated exactly like the operator's Send
- when `PRIM1_PANE_SECRET` is set in the shell, the script sends it as the
  connection's first line; the supervisor still prefers kernel identity
- a newly added member reads only its join event and later room traffic; an
  evicted range or stale cursor epoch returns an explicit cursor gap. Join
  boundaries are persisted with their log epoch. A replacement epoch places
  current members after its existing tail (zero for an empty replacement),
  before new traffic is allowed. Same-epoch truncation or a failed boundary
  save isolates the affected room; it never guesses access to old history
- `room_post` syncs the durable room log and writes no PTY
- `wait_quiet` is an output-silence hint, not model completion
- `wait_quiet` requires `QuietSec` 1–60, `TimeoutSec` 1–300, and `QuietSec <= TimeoutSec`; the supervisor enforces the same limits
- `PassThruJson` appends the minimal response JSON in quiet mode
- `OutRequestIdFile` atomically stores a returned `request_id`
- `timed_out: true` prints `TIMED OUT: <message>` and exits `124`
- ordinary failures exit `1`

The helper does not accept list, lifecycle, arbitrary cross-room route,
signal, or session/room-management actions. It never reads `control-plane.json`,
`PRIM1_PANE_CREDENTIALS`, or a bearer-token file.

Model shell tools are not assumed to preserve Job membership. Processes outside
all pane Jobs can authenticate with their inherited per-run pane secret. An
unaffiliated process with only the endpoint name is rejected.

## Model-facing MCP bridge

`room_post` and `room_deliver` accept an optional sender-scoped `request_id` idempotency key.
The executable room CLI also accepts `post --request-id ID ...` and
`deliver RECIPIENT --request-id ID ...`. Reuse the exact key, content and targets
for a lost-reply retry; matching retained requests do not repeat wakes. Keys are
bounded to 128 ASCII letters/digits or `.`, `_`, `:`, `-`. The retry horizon is
1024 posts / 16 MiB of post records per room, whichever comes first.
Successful results mean Stored; inspect per-recipient receipts for wake outcomes. A queued message typed five minutes or more after it was stored names its age in the header, `[Room message from <sender>, queued since 11:45 UTC, 5 h 29 min ago]` (A16), so a member started late can tell its backlog from live news; fallback and continuation headers are unchanged. A wake queued at delivery names why in its receipt's `queued_because` (not running or starting, its first prompt, an open question, an earlier message waiting, an active wake or input lock, operator typing, an unsent draft), in that delivery's fresh result only: a `request_id` replay, `room_read` and history carry no reason. The runtime's audit log keeps it durably, once when queued and again when the reason changes, by message, recipient and run id, never the text.
The PowerShell helper retains its legacy unkeyed request shape.


Claude Code and Codex are launched with one session-scoped `prim1_pane` stdio
MCP server. The server is the PRIM-1 executable in a dedicated no-UI mode and
is created by the harness. It exposes `ping`, `room_read`, `room_wait`, `room_post`, `post_card`,
`room_deliver`, and `tasks` (see [Tasks](#tasks-graphql)). The delivery tool accepts a member label, session ID, or `all`
(every other member); none accepts a caller-supplied sender or lifecycle
authority, and only a roaming member's `room_deliver` names a `RoomId`
([Room mobility](#room-mobility-a109)). Each tool call opens the existing
named pipe, pins and verifies the expected desktop server process, and then
uses the same caller/run/membership checks as the PowerShell
helper. MCP frames and control-plane frames are both bounded and fail closed.
Codex forwards the required injected transport, desktop-server identity, and
per-run pane-secret variables to this child. The supervisor still derives room
and sender from the authenticated live run.
Claude receives an ephemeral, process-local allowlist for these fully qualified
MCP tools: `ping`, `room_read`, `room_wait`, `room_post`, `room_deliver`, `tasks`, the four
roaming tools, `upgrade_request`, the member tools below (A110's six and A16's
`members_list`, `member_update`, `member_delete`, `member_add`) and
`room_fallbacks` (`post_card` is not pre-approved). The roaming tools, `upgrade_request` and the member tools are
listed only to members granted them; the operator's grant is the consent for
these gated ones. That makes them
autonomous without a per-call approval or persistent harness setting; every
other Claude tool keeps the selected Normal/Unsafe permission policy, and newly
added MCP tools remain unapproved by default.

A grant changed while a member runs reaches its tool list without a restart.
The pane MCP declares `tools.listChanged: true`. After it answers a tool call
or a protocol ping it asks the supervisor for the caller's grants again (the
same `MemberPermissions` sideband request, at most once every 5 seconds per
server); when they differ from the grants behind the tools the client was last
offered, it writes one `notifications/tools/list_changed` on the same stdout,
after that answer, and the client's next `tools/list` shows the current set.
A change is announced once. A refused or failed lookup keeps the tools offered
and sends nothing. The instructions are not sent again: each gated tool's
description says what it needs. A harness that ignores the notification keeps
its startup list; **Restart** resumes the same conversation with the current
tools, and the Edit dialog says so under the grants. The listing never
authorizes: every call is checked by the supervisor.

### Room mobility (A109)

The operator grants each member two permissions in its Edit dialog, saved with
the session (`set_member_permissions`; no pane request can change them):
**Can roam** and **Can upgrade PRIM-1** (`maintain`, see
[Self-upgrade](#self-upgrade-a108)). Prime and Terminal panes have no room tools: the dialog
disables both for them and `set_member_permissions` refuses either grant (it
still clears one). Without roam a member keeps to its one room, and its tool
list and MCP instructions say nothing about roaming. A member with roam
additionally gets `rooms_list` (every room, members, guest count, tail cursor),
`room_peek {room_id, cursor?}` (the operator's read of any feed: no join, no
Fetched receipt), `room_join {room_id}` (leaves its room, which reads "<label>
left for <room>", then joins the target, which reads "<label> joined"),
`room_leave` (to the lobby, "<label> left"), and `room_deliver`'s optional
`room_id` to deliver to a member of another room through the normal room wake
path, stored in that room's feed. A `room_id` always needs roam, also when it
names the caller's own room. The pane MCP asks for the grant at
initialize: its instructions describe roaming, and its first tool list shows
these tools, only when the answer is roam; later lists ask again, and a grant
given or taken while it runs is announced after a later call (above). Every call is
checked against the caller's current run and grant at entry and again after
its waits, under the locks its effect is committed under (the join's catalog
write, the peek's read, the delivery's append), and a refusal names the
missing grant. A move is one room-catalog write holding both the leave and the
join, so a refused or failed join changes nothing and never strands the member
in the lobby; membership revisions, join floors and membership events are the
operator's add/remove ones. The room CLI offers the same as `rooms`,
`peek <room id>`, `join <room id>`, `leave` and
`deliver --room <room id> <recipient> <text>`.

A cross-room delivery reaches its recipients headed
`[Room message from <label> (visiting from <room>)]` (or "the lobby"), also
when it is retried or resumed after a restart. Its recipients may answer the
visitor without roam: a plain `room_deliver` (no `room_id`) to its label (or
that header label, or its session ID) that matches no member of the caller's
own room goes to that visitor in the room it is in now, stored in that room's
feed and headed with the caller's own room. Only a genuine no-match falls back
this way: a label naming two members of the caller's room stays refused as
ambiguous, and the caller's own label or ID stays refused. This holds for 24
hours after the visitor's last delivery to that recipient, while the visitor
still exists, still roams and is in a room; all of it is rechecked at commit.
The window is kept in memory, so a PRIM-1 restart closes it; the tool text says
so, and the refusal then reads "no reply window to "<label>" in another room:
its last message to you was before PRIM-1 restarted or over 24 hours ago". A
member that cannot answer posts in its own room (`room_post`), where a roaming
visitor reads it with `room_peek`. The pane MCP's grant lookups (at initialize,
per list and after calls) only choose which tools it lists; every call is
authorized by the supervisor.

### Self-upgrade (A108)

A member granted **Can upgrade PRIM-1** sees `upgrade_request {candidate_exe,
sha256, task_id, task_revision}` (CLI `upgrade <candidate exe> <sha256> <task
id> <task revision>`). It saves its task checkpoint with `tasks` first; the
task must exist at that revision or later. Admission rechecks the caller's run
and grant and refuses, naming each reason, while a member is starting or
stopping, a question or approval is on a screen, text is typed and unsent (a
terminal input or a room composer), a room message has no recorded delivery
outcome, or a running member's exact conversation is unknown (Claude/Grok: the
stored ID the Resume path would use; Codex: only the ID it reported with
`--prim1-identify` in its current run and the Resume path verifies). One
attempt at a time.

An admitted request stages the build and the helper (`supervisor::upgrade`),
starts the helper hidden (it registers an outside Task Scheduler task), posts a
notice in every affected room and shows a banner with a 60-second countdown and
**Cancel**. After the notice the desktop waits for the helper's readiness,
checks its exact process, refuses new starts, restarts and sessions ("PRIM-1 is
restarting for an upgrade"), and rechecks: a changed running set cancels the
attempt; a blocker postpones closing until a deadline, then cancels. Shutdown
is the ordinary one, except that a failed final catalog write fails it; only a
clean shutdown is recorded before PRIM-1 exits. A failure leaves the attempt
held (the helper then expires) and the banner says why.

The new build reads `PRIM1_UPGRADE_ATTEMPT_ID` and `PRIM1_UPGRADE_REQUEST_FILE`
(both or neither) and claims the resume once. Once its window is shown it
resumes each recorded member's exact conversation through the ordinary Resume
path (two starting at a time, never Start fresh, no task prompt replayed);
terminals, Prime and members outside a room stay stopped with a room notice.
Each resumed member gets one continuation through the durable room queue,
keyed by the attempt's continuation ID, so a restart never adds a second one.
The requester is told to continue its task from its current state; others to
continue their current task. The attempt is then consumed: a later start with
the same variables replays nothing. Finally the Desktop launcher task
`PRIM1-<root>` follows the new build when its single action is the replaced
build (a foreign action is refused). An attempt that closed without a
confirmed new build is shown as held with **Abandon held upgrade** (a confirmed
note that proves nothing stopped). The S7 `rebuild preflight` request is retired.
### Start readiness (A16, PC3 20 Oct)

A Claude or Codex run is **starting** until it shows visible text, and
**ready** from then on: the pseudoconsole's own setup sequences, which it
writes before the program runs, do not count; text drawn while the spawn is
still being confirmed does. A run with no visible output 90 s after its spawn
is **stalled**: its state says so, the log warns "<label> has shown no output
90 s after it started; stop it and start it again", and room messages wait
for it ("the member has shown no output since it started"). Visible output
later makes it ready. Restart is unchanged: for a Codex whose conversation is
not proved it still refuses and names Start fresh session. Grok keeps its
measured startup repaint; Terminal and Prime are ready when they spawn.

### Member lifecycle (A110)

A third grant in the Edit dialog, **Can manage agents** (`manage`, saved with
the session, omitted from the catalog row while off, so A109 catalogs load with
it off), lets a Claude, Codex or Grok member start and instruct other agents. It
is refused, and its checkbox hidden, for Terminal and Prime. A member with it
also gets six tools; the pane MCP lists them, and its instructions and the room
brief describe them, only after the supervisor confirms the grant, and a member
without it is refused with "ask the operator to tick Can manage agents":

| Tool | What it does (the operator's own supervisor method) |
|---|---|
| `member_create {request_id, driver, label, permission_profile?, start?, brief?, brief_text?, room_id?}` | `create_session` into the room (driver `claude`, `codex` or `grok`; label unique in the room). The safe `normal` profile unless `permission_profile: "unsafe"` (bypass) is given. `start` launches it (`start_session`); `brief` sends the room brief, or `brief_text`, through the room's queued delivery (`brief_room_member`'s path), typed once the member is ready, never also as a launch prompt. |
| `member_start {request_id, member}` | `start_session` (resumes as the operator's Launch does; a running member is left as it is) |
| `member_stop {request_id, member}` | `stop_session` (waits for the bounded stop) |
| `member_restart {request_id, member}` | `restart_session` |
| `member_brief {request_id, member, content?}` | the room brief or `content`, from the caller, through the queued room delivery |
| `member_input {request_id, member, text?, keys?, show_text?}` (`member input … [--text t] [--key k]… [--show-text]`) | types `text` (literal, no control characters, at most 4 KiB) and then `keys` (enter, up, down, left, right, tab, esc, ctrl_c; at most 16) into a running member's terminal on the operator's raw input path, for example to answer a prompt it waits on. Never to get around a refusal or to give consent that is Victor's. The notice holds metadata only ("<caller> typed into <label>: N characters, then <keys> (request <id>)"); the text appears only with `show_text`, the caller's mark that it is not sensitive. The result carries no terminal output |
| `member_remove {request_id, member}` | stops a running member, then `remove_room_member`; the session stays in the lobby (`member_delete` closes it, A16) |

A member is named by its session ID or by a label unique in the caller's room;
anything else is refused with that room's members listed. A tool never acts on
the caller itself. Scope is the caller's current room; another room (by
`room_id` or a session ID from `rooms_list`) also needs **Can roam**. The
caller's run, generation and grants are checked at admission and again inside
the critical section where the effect commits (the session and membership
commits, the stop or restart declaration, the launch admission, the brief's
message commit), so a grant withdrawn meanwhile leaves nothing done. A new
member has no grants unless `member_create` names them (A16, below). A brief from a managing member of
another room is headed "<label> (visiting from <room>)", and the briefed member
may answer it with `room_deliver` as it answers any visitor.

`request_id` is a caller-chosen UUID. A retry with it returns the first result
(`replayed: true`) and never creates a second agent or a second brief; reusing
it for a different action, or the same action with other arguments, is refused
(the check compares a SHA-256 of the arguments). Results are kept in memory for
the last 1024 requests; `member_create` also stores the request and that
fingerprint with the new session, so a retry after a PRIM-1 restart still finds
the member (a changed create is refused with "request_id was used for a
different create"), and the brief and notice posts are keyed by it in the room
journal. Such a retry has no first result to return, so a requested start or
brief counts as done only when it can be seen (the member running, the brief's
delivery retained); any other is named in `incomplete` (`["start", "brief"]`)
with the warning "the member was created, but its start and brief may not have
happened; check it and use member_start / member_brief". A first run whose start
or brief fails names it in `incomplete` too. The session commit itself rechecks
the caller's run, generation, manage grant and, for another room, roam, all
under the room and session locks, so a grant withdrawn before it leaves no
session created or published. Every effect posts a notice from the
caller, for example "Claude PRIM-1 added QA Grok (Grok, bypass permissions,
briefed)", "Claude PRIM-1 restarted QA Grok" or "… stopped and removed …";
replays and no-ops (starting a running member, stopping a stopped one) post
none. The CLI equivalents are `member create --request-id <uuid> --driver <d>
--label <l> [--unsafe] [--start] [--brief] [--brief-text <t> | --brief-file
<p>] [--room <id>]`, `member start|stop|restart|remove --request-id <uuid>
<member>` and `member brief --request-id <uuid> <member> [<text>]`.

A Codex member whose earlier run ended keeps its identity hold, as for the
operator: `member_start` and `member_restart` then refuse with the hold's
reason, and only the operator's **Start fresh session** clears it.

While PRIM-1 is closing for an upgrade (A108), `member_create`, `member_start`
and `member_restart` are refused with "PRIM-1 is restarting for an upgrade",
exactly as the operator's create, start and restart are; a create that asked
for `start` and was admitted before closing reports "created but not started".
A refused request records no result, so its `request_id` can be retried after
the restart.

Grok receives no MCP configuration from PRIM-1. It and other non-Prime panes can
use the same executable's room CLI through a shell tool, without a source checkout:

```powershell
& "$env:PRIM1_CLI" --prim1-room ping | Out-String; $LASTEXITCODE
& "$env:PRIM1_CLI" --prim1-room read | Out-String
& "$env:PRIM1_CLI" --prim1-room post 'Status from this pane' | Out-String
& "$env:PRIM1_CLI" --prim1-room deliver 'Reviewer' 'Please review the change.' | Out-String
```

The CLI uses the pane's injected environment and the same authorization checks.
**Call it piped.** The release executable is a windowed (GUI-subsystem)
program, so PowerShell's bare `& "$env:PRIM1_CLI" …` neither waits for it nor
sets `$LASTEXITCODE` (and may close its stdout first). Piped (`… | Out-String`)
or through `Start-Process -Wait -PassThru`, PowerShell waits and the exit code
is real: 0 ok, 1 refused or transport, 2 usage. Judge a result by its JSON
`ok` and `message`, not by the exit code alone. The CLI modes never panic on
a closed stdout or stderr. A console-subsystem companion that would make
every call wait is deferred (decision audit 59).
Prime has neither this sideband nor synthetic delivery; use its raw terminal.

Choosing brief None removes the task prompt. Where PRIM configures MCP
(Claude/Codex), initialization still carries room protocol; installed Codex
perception is not established by this source contract. Grok receives the CLI
but no automatic protocol text when None is selected.

### Room fallback messages (A111)

The existing right-side room panel contains independently collapsible Members
and Fallbacks sections. Fallbacks are off until saved. Choose 15, 30 or 60 minutes,
all hosted Claude/Codex/Grok members or selected session IDs, and the room message.
Each member can inherit, use a custom interval/message, or opt out. Its optional
repeated-task brief is appended to the effective message, not sent separately
and not written into the member's launch brief. All-hosted includes later eligible
members; selected-only does not. There is one effective schedule per member.

Tauri `get_room_fallbacks(room_id)` returns the current settings and revision;
`set_room_fallbacks({room_id, revision, settings})` saves a complete replacement.
Only a successful revision-checked native save changes the schedule. The pane
MCP `room_fallbacks {action: "get"|"set", room_id?, revision?, settings?}` uses the
same implementation, needs **manage**, and additionally needs **roam** for another
room. Set requires settings and revision from a get. There is no guest tool or
new permission flag. CLI equivalents are `fallbacks get [--room ID]` and
`fallbacks set <revision> <settings-json> [--room ID]`; JSON is data, not code.
Every change posts a feed notice attributed to the actual operator/member. The
operator entry (A16) takes the same two commands; it has no room of its own,
so `--room` is required, and its notice names the local process. A set is a compare-and-set on the revision, not a replayed request: after an uncertain result, read it with get and reconcile; never raise the revision and resend blindly.

The native supervisor timer keeps working when the panel is collapsed or the
room is not selected. It submits through A106's durable Send path, including
while an agent works, without bypassing questions, paste mode or operator drafts.
The prompt is labelled `[PRIM-1 fallback, scheduled HH:MM]`: a scheduled reminder
from the room's settings, **not a new instruction from Victor**. The feed uses
typed scheduler provenance for the label, never a user-editable text prefix.

Stopped, starting, hibernated and external members are not prompted or started.
No elapsed-interval burst is sent after restart. Only one unresolved scheduled
occurrence is retained per member; Written/Fetched still do not mean Answered.
Off, editing the configuration, leaving the room or changing runs invalidates
unattempted old occurrences, including those already queued. Pending/Written
effects cannot be recalled. Settings survive same-runtime upgrades, while newly
imported rooms start disabled. PRIM-1 must be running: this does not replace an
external watchdog for the PRIM-1 application itself.

### Native parity (A16)

Victor, 20 Oct: every function a person can use in PRIM-1 is also an agent's,
natively, through the pane MCP and the room CLI, never through the UI. The
member tools below call the operator's own supervisor methods with A110's
rules: **manage** for the caller's own room, also **roam** for another room
or a session in the lobby, the caller's run and grants rechecked where each
effect commits, a UUID `request_id` that replays the first result, and a feed
notice from the caller for every effect (none for replays and no-ops). There
is no ceiling: a managing member may give any grant (Victor: "No guards ...
More power"). Guests stay outside: a guest token never sends these requests.

| Tool (CLI) | The operator's action |
|---|---|
| `members_list` (`members`) | the tab bar and lobby: each session's snapshot with its room; the caller's room, with roam every session |
| `member_create {…, grants?}` (`member create … --grants roam,maintain,manage`) | create with grants saved in the same catalog write; omitted means none (a worker) |
| `member_start {…, mode?, launch_choices?, search?}` (`member start … [--fresh \| --resume-here] [--search] [--choices <json>]`) | Launch (`resume`), **Start fresh session** (`fresh`: a running member is stopped first; also clears a Codex identity hold), **Resume here** (`resume_here`), **Search for it** (`search`), Save and launch (`launch_choices`, a complete replacement, only for a stopped member or with `fresh`) |
| `member_update {request_id, member, label?, permission_profile?, grants?, working_directory?, position?}` (`member update … [--label] [--profile] [--grants] [--working-directory] [--position]`) | the Edit dialog: each field through its own setter in the dialog's order; the profile and the working directory (an existing folder) only while stopped; `position` moves its tab, counted among the sessions the caller may list (its room, or every session with roam), 0 first; labels unique in the room; the caller may edit itself; fields already as asked change nothing; a later failure keeps the earlier changes and says so. Notice "<caller> updated <label>: renamed to …; bypass permissions; grants …; working directory …; moved to position N" |
| `member_add {request_id, member, room_id?}` (`member add <session id> [--room]`) | add a lobby session to the caller's room or `room_id` (always needs roam: the lobby is outside the room) |
| `member_delete {request_id, member}` (`member delete`) | Close session: stopped if running, out of its room, deleted once its event stream drains (up to 15 s, as the desktop's Close retries). A lobby session is named by its id and needs roam; its close is recorded in the caller's room, so a caller in the lobby is refused |

`member_stop`, `member_restart`, `member_remove`, `member_delete` and
`member_start` still never name the caller: each would end or replace the run
that asked before its result could be returned.

A session in the lobby (A16 rows): `member_start`, `member_stop`,
`member_restart` and `member_update` take it by its session id (a label names
only the caller's room's members) and need roam. The caller's own room records
the act, so a caller that is itself in the lobby is refused; each effect
rechecks that the session is still in the lobby. `member_brief` and
`member_input` keep to room members: a brief is a room message to a member.

**Supervisor designation.** The create form shows the three grants and a
**Supervisor** box that ticks all three (Victor: "for the supervisors it should
be allowed under Beyond"); each stays editable. Ordinary creates and quick
attach give none. `CreateSessionRequest.permissions` carries the grants;
Terminal and Prime refuse any. The Edit dialog opens in sight from the rooms
view (it switches to the sessions view first).

**Room actions.** Same rules (manage; another room also roam), each through
the operator's own setter rechecked where it commits, a UUID `request_id` for
every change (reads take none; a read with one is refused), and a feed notice
from the caller:

| Tool (CLI) | The operator's action |
|---|---|
| `room_create {request_id, label, brief_on_join?, brief_template?, resume_on_restart?}` (`room create --request-id <uuid> <label> [--resume on\|off] [--brief-on-join on\|off] [--brief-text \| --brief-file]`) | New room (empty; the caller stays where it is). The new room and the caller's read "<caller> created …" |
| `room_settings {action: get}` (`room get [--room]`) | the room, its brief settings, the built-in brief, its position |
| `room_settings {action: set, request_id, label?, resume_on_restart?, brief_on_join?, brief_template?, position?}` (`room set …`; `--brief-default` restores the built-in brief) | rename, **Resume on restart**, brief at launch, brief text, room order; "<caller> updated the room: …" |
| `room_delete {request_id, room_id}` (`room delete <room id>`) | Delete room (refused during an in-flight delivery); members go to the lobby; the caller's own room (when another) and the system log record it |
| `room_start` / `room_stop {request_id, room_id?}` (`room start\|stop`) | Launch every stopped member (resuming its conversation) or stop every running one, never the caller; a member's failure is a warning, the rest proceed. After a PC restart `room start` brings a room back whatever Resume on restart recorded |
| `room_retry {request_id, room_id?, message_id, recipient, receipt}` (`room retry --message <id> --recipient <member> --receipt '<cursor>'`) | the operator's Retry wake: one recipient's held or failed wake of a stored message, named by that receipt's feed cursor, attempted again; never a second post or a recipient already reached; a recipient that restarted since, or is not ready, is refused and the message stays readable. The result carries the delivery outcome. Notice "<caller> retried the wake of message <id> for <label>" |
| `room_guests {action: list\|invite\|revoke}` (`guests list\|invite\|revoke`) | outside agents; the token, MCP config and CLI example are returned **once, to the caller only**, never posted; a replay returns the guest without them |

A request that changed something and then failed (`member_update` after its
first field, a fresh start after its stop, a delete after its stop or
removal, a partial `room_settings` set) keeps that error as its outcome: a
retry with its `request_id` replays it and applies nothing more, and other
arguments under it are refused. An error before any effect leaves nothing
behind.

**Runtime settings and local conversations** (A16 phase 3). `runtime_settings
{action: get|set, request_id?, task_folder?, personal_repo?, workspace?}`
(`settings get|set`) needs **maintain** (the grant behind "Can upgrade
PRIM-1"): the task folder, the personal repository and the default workspace,
each through the operator's own setter (`select_task_folder`,
`select_personal_repo`, `set_workspace_preference`) with the grant rechecked
just before the choice is saved; the caller's room and the system log record
the change. `local_conversations {action: scan|attach, request_id?, member?,
conversation_id?}` (`conversations scan|attach`) needs **manage** (another room
or the lobby also roam): the bounded metadata scan of the personal repository,
and Attach for recovery onto a fully stopped member of the same driver, held
until `member_start {mode: resume_here}`; nothing starts. Both are also open
to the operator entry.

**Upgrade control and telemetry** (A16 rows). `upgrade_control {action:
status|cancel|abandon, request_id?, attempt_id?}` (`upgrade-control status`,
`upgrade-control cancel|abandon --request-id <uuid> <attempt id>`) and
`telemetry` (`telemetry`) need **maintain**. Status returns the current
attempt as the operator's banner shows it. Cancel is the operator's Cancel
during the notice, under the member's own name ("<member> cancelled it");
Abandon is the operator's terminal note on a held attempt, logged as
"abandoned by <member>", and like the operator's it proves nothing stopped:
check that one PRIM-1 runs for this runtime first. Telemetry returns the
operator's resource sample. For a cancel or abandon, the caller's run and grant
are held from their check through the attempt's store commit; the upgrade's own
notices are unchanged, and the caller's room records its act under its name.

**Room packages** (A16 rows). `room_package {action: save|inspect|load,
request_id?, path, room_id?, source_room_id?}` (`room-package save --request-id
<uuid> [--room <id>] <path>`, `room-package inspect <path>`, `room-package load
--request-id <uuid> [--source-room <id>] <path>`) needs **manage**: the
operator's Rooms → Save room… and Load room… (A92). Save writes one room to a
new `.prim1room` file at an absolute path (the caller's room by default;
another room also roam), with the grant and scope checked under the rooms lock
the feed is copied with; an existing file is never replaced, where the
operator's dialog would ask (the finished file is published by a hard link,
which fails if the name exists, also for a file created meanwhile). Inspect lists what a file or a copied runtime's
`room-catalog-v1.json` offers, reading only. Load creates a new room with new
members, nothing started and no grants, with the grant checked under the locks
the first member copy is saved with; a failure that left listed member copies
is a partial effect, recorded once in the caller's room and never loaded again
on retry. The saved room, the loaded
room and the caller's own room record each act.

### Feed subscription (A16)

Victor, 20 Oct: *"I just post it in the feed and you are not seeing it."*

- **The operator's Post reaches the Supervisors.** A plain post (no recipients
  chosen) is addressed to the room's members designated Supervisor (all three
  grants; Claude, Codex or Grok) and typed to them through the operator's Send
  path, as "Room message from operator", with the same receipts and
  `request_id` replay. A room without one stores the post and types it to no
  one. Scheduled fallbacks keep their own path and `fallback_origin` marking
  ("[PRIM-1 fallback, scheduled HH:MM]"), never the operator's. Members' own
  `room_post` still prompts no one.
- **`room_wait {cursor, timeout_ms?}`** (MCP) and `--prim1-room wait
  (--cursor <json> | --epoch <uuid> --sequence <n>) [--timeout-ms <0-25000>]`
  (CLI): `room_read` that waits, for every member. It returns as soon as
  someone else posts or delivers, adds or updates a card, or changes the
  membership after the cursor, or when part of the history is unavailable (a
  cursor from another epoch, after a restart or from another room, answers at
  once with the gap); otherwise after `timeout_ms` (at most and by default
  25 s) with `timed_out`. Delivery receipts and the caller's own posts and cards
  move the cursor, not the wait. Every read rechecks the caller's run and room
  and records Fetched receipts as `room_read` does. It holds no lock while it
  sleeps on the room-feed watch; one open wait per pane and 16 at once (the
  guests have 8 of the pipe's 64 connections), and a wait that gets neither
  within 2 s answers at once with a note. The pane MCP serves one call at a
  time, so the member's other PRIM-1 tools wait behind it.

### Windows restart (A16)

Victor 345: after a restart, *"start prim one, start the agents, and continue
work."* tao turns WM_ENDSESSION into an exit with no close or exit request, so
the desktop handles that exit as a session end, unless an ordinary shutdown
already ran: it first stops any later run end from clearing a member's
`running_at_shutdown` and writes the session catalog with each flag and pane
size, then tells each agent's process scope to end on its own thread and
returns without waiting (the pane Jobs end with PRIM-1 in any case). The
desktop sets its process shutdown level to 0x380 with `SHUTDOWN_NORETRY`, so
Windows ends PRIM-1 before the agents (default 0x280) and never shows a retry
dialog for it. After the restart, Resume on restart and `room start` find
the members that were running as interrupted and resume their conversations.
`desktop-events.jsonl` records `session_end_started` and
`session_end_checkpoint` (or `session_end_checkpoint_failed`) before
`process_exit`.

### Operator entry (A16)

`prim1.exe --prim1-operator [--runtime <dir>] [--actor <label>]
[--token-file <path>] <command…>` takes the member, room and guest commands
above plus `ping`, `rooms`, `members` and `fallbacks get|set … --room <room
id>` (a room's fallback reminders), for a **local process of the
operator's Windows user that is not a pane**, for example a supervisor's
script on PC3 or the laptop over the SSH mesh, so a PC needs no hands at its
screen to get its first room and agents.

- **Transport.** The same named pipe; no network listener. It finds PRIM-1
  through `<runtime>/guest-endpoint.json` (`--runtime`, else
  `PRIM1_RUNTIME_DIR`, else `%LOCALAPPDATA%\io.prim1.v2\runtime`) and pins
  the exact server process as the guest adapter does. The first frame is
  `{"operator":{"actor":…}}`, then one request.
- **Who.** The pipe's ACL admits only this Windows user. The supervisor also
  requires the caller to be proven outside the panes: a pane, even one
  holding every grant, is refused ("use your member tools"), and so is a
  caller PRIM-1 cannot place. A caller that no running pane's Job claims is
  walked up its parent chain (each parent pinned and started no later than
  its child): if it reaches a pane's own process or a process in a pane's
  Job, it is that pane's descendant and refused (PC3, 20 Oct: on a PRIM-1
  started by its launcher task, a pane's tool shell was in no pane's Job).
  A chain that ends at the system, at a process this user may not query (a
  service host, an elevated parent) or at an exited parent, without reaching
  a pane, is outside; one longer than 32 is refused. Every operator-entry and
  guest admission decision is written to the audit log: the caller, how it
  was decided and the verdict. `--prim1-operator` also refuses before
  connecting when its shell is a pane of the PRIM-1 it calls. PRIM-1 (medium integrity) cannot open an
  elevated client's process, so when the Job query fails the pipe's own
  client and server session ids decide: a client in another session than
  PRIM-1 (an SSH session runs elevated in session 0) cannot be in a pane Job,
  since pane processes descend from PRIM-1 and stay in its session, and is
  accepted. An unreadable client in PRIM-1's own session stays refused (run
  it non-elevated). The same rule admits an elevated guest in another
  session. A same-user process can already read and change PRIM-1's runtime
  files, so this entry adds no authority that user lacks; it is not a
  boundary against that user. A process deliberately started outside the
  panes (a scheduled task, `ssh localhost`) or a pane-launched process whose
  parent has exited reads as outside: grants are consent and attribution,
  not containment.
- **Attribution.** Effects are posted under a new feed sender,
  `local_process`, labelled "<actor> (local process <pid>)": never the person
  at the screen, never a pane. The actor text is display only, one line,
  at most 80 characters. Journal readers need the matching build, as for
  cards.
- **Guests.** `guests invite` over the entry requires `--token-file <path>`:
  the invitation is written to that new file and stdout names only the path.
- **Not here.** Posting, delivering, tasks and upgrade stay pane actions;
  starting and closing the app stay the launcher task and the window.

An unknown `--prim1-…` first argument exits 2 with the list of modes and
never starts the desktop (over SSH it would be an invisible session-0
instance).

**Tasks without MCP.** `--prim1-room tasks [--variables <json>]
[--operation-name <name>] (<query…> | --file <path> | -)` sends the same
`tasks` request as the MCP tool, for Grok and other shells.

## Outside agents (guests)

**Invite an outside agent** in a room's members panel (Tauri
`invite_room_guest(room_id, label?)`, `revoke_room_guest(guest_id)`,
`list_room_guests(room_id)`; a held trial refuses invite and revoke) binds one
token to that room and shows it once. The dialog's **Copy invitation** copies
one block (`agent_invitation`, A16) that any agent outside PRIM-1, on this PC
as this Windows user (the pipe is local), can act on alone: the room and its own name, no expiry (it ends when revoked), one
PowerShell line carrying the token and runtime folder for each `--prim1-guest`
command (`rooms`, a first `read` with no cursor so nothing posted since the
invitation is skipped, then `read` and `wait` from the read's own cursor,
`post`, `profile`; no restart or configuration change), how to take part (read first, every post reaches every
member, answer a member by name with a quote, members' messages are not the
operator's), and the MCP config as the alternative. The MCP config alone runs
this executable as `--prim1-guest-mcp` (tools `room_list`, `room_read`,
`room_post`, `room_wait`, `room_profile`); a `--prim1-guest` script example is
also shown. A managing member's `room_guests` invite returns the same block. The token is
stored only as SHA-256 in `room-guests-v1.json`; it is the whole identity, and
a caller inside a pane Job is refused as a guest. A guest sends only
`GuestRequest`s after a `{"guest_token"}` preamble, reads from its invitation
onward without Fetched or wake receipts, and posts (at most 32 KiB, 3 at once
then one per 10 s) through the operator's Send path to every member as
`[Room message from <label> (guest)]` with every line quoted `> ` and a closing
`[end of message from <label> (guest)]`, with `request_id` dedupe. A revoke
holds that guest's still-queued wakes. `wait` long-polls up to 25 s without
locks (one per guest, 8 at once, 2 s grace). Guests are never members and
never woken. Setup per app, the ChatGPT limit and the details: [docs/GUEST-AGENTS.md](docs/GUEST-AGENTS.md).

## Codex root identity helper

A fallback (A16): the exact-ID follower proves a running Codex member's
conversation from the thread writer lock its own process holds, so PRIM-1 no
longer asks every Codex root to identify. Only when PRIM-1 reports a Codex
conversation as not proved or not identified (a room notice, or an upgrade
refused for it), from the attached Codex root's own unsandboxed shell tool:

```powershell
& "$env:PRIM1_CLI" --prim1-identify | Out-String; $LASTEXITCODE
```

A Normal-profile (sandboxed) Codex shell cannot open the pane pipe, whose DACL
admits only the operator's own user: the helper is refused there, and the pane
says so instead of asking for approval.

The helper accepts no arguments or ID override. It reads its own
`CODEX_THREAD_ID` and sends `identify_codex`; it is not an MCP tool. The
supervisor requires native Windows Job/run attestation, checks the exact root
rollout and working directory, and refuses secret-only, stale, wrong-driver or
conflicting reports. Accepted evidence is provisional and bound to that run.
Superseded recovery records retain their full provenance and identity evidence
in the private catalog. A report alone never clears the freshness hold or
enables native wake. When the reported thread is also a root writer lock held
by the member's own `codex.exe` (through the npm shim: the `codex.exe` child
of the member's node process, in its pane Job), and the report was made after
every other thread that process holds appeared, the conversation follower
treats it as exact and releases the resume hold; this is how a member with two
loaded roots after `/new` becomes exact. Until then such a member is held as
`codex_ambiguous` (persisted; Launch and restart resume neither thread), and
the hold also lifts when the older lock is released and one root is left.
While the process still holds the reported thread, the follower never replaces
it with another lone root. Re-report after changing conversations without
guessing an ID.

## Thin pane wrappers

`agent-key.ps1` forwards one supported PTY key. `agent-slash.ps1` constructs a
slash command as raw input; its file-based arguments are strict UTF-8 and retain
line endings and trailing whitespace. Use a separate key call to submit it.
Both use the same endpoint rule.

```powershell
.\scripts\agent-slash.ps1 -Session $env:PRIM1_PANE_IDENTITY -Slash compact -ArgsFile 'D:\tmp\compact-args.txt'
.\scripts\agent-key.ps1 -Session $env:PRIM1_PANE_IDENTITY -Key enter
```

## Metadata audit

The durable audit remains a direct, read-only metadata surface at
`<runtime-dir>/audit/YYYY-MM-DD.jsonl`. Raw terminal-output events are omitted and
room/routed-message bodies are redacted. Failure diagnostics may contain harness
error text; inspect logs before sharing them. A delivery or write receipt is not
proof that a model understood or completed work.

`agent-events-summary.py` reads that JSONL directly:

```powershell
. .\scripts\runtime-paths.ps1
$auditName = (Get-Date -Format 'yyyy-MM-dd') + '.jsonl'
$auditLog = Join-Path (Join-Path (Resolve-Prim1RuntimeDirectory) 'audit') $auditName
python .\scripts\agent-events-summary.py --audit-log $auditLog
```

There is no durable `events_since` wrapper. The pane-local `room_read` cursor
addresses the caller's authorized bounded view of durable room history and reports
gaps explicitly.

## Git Bash / MSYS

Leading-slash arguments can be rewritten by MSYS. Wrap the PowerShell invocation
as one command when sending slash input:

```bash
powershell -NoProfile -Command '& ".\scripts\agent-slash.ps1" -Session $env:PRIM1_PANE_IDENTITY -Slash compact'
```

## Authority boundary

An endpoint name is routing metadata, not authority. The supervisor binds the
caller to one live run through kernel Job membership, or through its per-run
pane secret when the caller is outside all pane Jobs. A secret for another run
cannot override a kernel-attributed pane. Bearer files are not a fallback. An
invited guest's token binds it to one room and only to guest requests; it is
never a pane identity.
Prime/WSL sideband access is deliberately disabled. The native named-pipe policy
cannot derive a Linux task identity from Windows Job membership, and there is no
bearer or compatibility fallback. Prime uses raw desktop terminal input; routed
delivery is also rejected until a measured Prime submit protocol exists.


## Typed cards (v0)

The MCP tool `post_card` and `--prim1-room card [--request-id ID] '<card JSON>'`
use `room_post_card`. They accept a card and optional request ID only. Room/author
are derived from current pane membership and run identity, rechecked on the
blocking worker immediately before journal append. They never write a terminal.
The successful `room_card` receipt contains `card_id`, room/cursor, request ID and
`replayed`. Message/card retries share one sender namespace and bounded horizon;
changed payload or operation kind conflicts. Invalid data, exhausted ordinary
quota and writer holds refuse the new append without changing the projection.

Cards are additive v1 journal events. Old message/membership/delivery records
replay unchanged; readers must use the matching build to understand Card events.
The separate `cards` snapshot in room-read is at most 64 keys / 2 MiB and respects
the current join floor. `cards_cursor` marks snapshot state only; it never replaces
the pagination cursor. See README-AI for the data and rendering bounds.

For successful MCP room reads, `content[0].text` contains the full JSON response.
`structuredContent` normally repeats it but is optional: an exact duplicate is
omitted only if both copies exceed the 8 MiB frame limit. Parse the text if the
structured field is absent. The writer rechecks the final encoded frame size;
unrelated or still-oversized responses retain the bounded error. No feed/card
data or cursor is truncated to fit.

## Tasks (GraphQL)

The MCP tool `tasks` sends one `tasks` sideband request, `{query, variables?,
operation_name?}`, against the runtime's task folder. Agents create and change
tasks; humans read them.

- **Folder.** One folder per runtime, often a git repository: one pretty-printed
  `T-<n>.json` per task, owned by one runtime at a time through
  `.prim1-tasks.lock`. It is the Settings → Task folder choice, saved as
  `task-folder.json` in the runtime directory. With no choice it is
  `<personal repo>\tasks` (see [Session register](#session-register-a108)); with
  the register off, `<workspace>\tasks` (the workspace new sessions start in),
  or `<runtime>\tasks` when that folder is gone. The default is not saved, so
  it follows the personal repository at the next start. The desktop opens it at start through
  `SupervisorHandle::open_task_folder_from_setting`; an embedding program may
  pass `SupervisorConfig.task_folder` instead. A saved choice that cannot be read
  is reported, never replaced by the default. With no folder the request returns
  `No task folder is set. Choose one in PRIM-1 Settings → Task folder.`; a
  folder that could not be opened at start reports why. A held trial resolves
  the folder but never opens it.
- **Choosing a folder.** Selecting creates the folder if needed, opens it under
  its canonical path and takes its lock first; only then is the choice saved
  and the runtime switched. Choosing the folder already open is a successful
  no-op that takes no second lock (choosing the default this way saves it). A
  folder still locked (by another runtime, or by a request still finishing on
  it here), a file, or a folder that cannot be created keeps the current folder
  and setting and returns a plain error. Nothing is copied or moved: the old
  folder keeps its tasks and is released once the last request running on it
  finishes.
- **Identity.** Same current-run/secret checks and held-trial refusal as other
  pane requests, but no room membership: a lobby pane may use tasks. The run is
  checked again on the blocking worker immediately before execution and
  immediately before each mutation, and once more before the result is
  returned; a run that fails that last check receives no task content, only
  its confirmed current task results (including replays) in the refusal
  message. Changes are logged as `<session label> (session <id>)`; a request
  cannot name its own actor.
- **Schema.** `task(id)`, `tasks(state, owner, parent, offset = 0, limit = 50)`
  (limit above 200 is an error), `createTask(input, mutationId)` and
  `updateTask(id, expectedRevision, patch, mutationId)`. States: `open`, `doing`,
  `blocked`, `review`, `done`, `dropped` (no delete). `request` and `source` are
  fixed at create; patches have no such fields. To clear owner, reviewer or
  parent, send `clearOwner`/`clearReviewer`/`clearParent: true`. The tool
  description carries the full summary; introspection is disabled.
- **Retries.** A stale `expectedRevision` writes nothing and returns
  `extensions.currentRevision`. Reusing a `mutationId` with the same change
  writes nothing and returns the task as it is now, which may include later
  changes (no earlier snapshot is kept); with a different change it is
  refused. Errors carry `extensions.code` (`CONFLICT`, `MUTATION_REUSED`,
  `NOT_FOUND`, `INVALID`, `BUDGET_EXCEEDED`, `OUTPUT_TOO_LARGE`, ...).
- **Bounds.** Query 16 KiB, variables 128 KiB, operation name 128 characters;
  before parsing, nesting depth 12, 32 `...` spreads, 16 fragment definitions,
  no fragment cycles, and 8192 names after fragment expansion, counting every
  definition whether used or not; an object field such as `log` at most once
  per response name in one selection set; 20 `task`/`tasks`/`createTask`/`updateTask`
  fields per request, failed ones included; 1 MiB response, with response keys
  (aliases included) charged before an object or list is built. Over the
  response cap, the result is replaced by an error whose `extensions.confirmed`
  lists every confirmed current task result (including replays): id, revision
  and mutationId.
- **Response.** `payload.result` is the standard GraphQL `{data, errors}` object
  (object keys are serialized in sorted order). `ok` is false whenever `errors`
  is non-empty; `data` may still hold partial results.

**Desktop (read-only).** Four Tauri commands serve people, not agents:

- `get_task_folder` returns `{folder, source: "setting" | "default", status:
  "open" | "error", error?}`; `folder` is null only when no folder is known
  (for example, an unreadable saved choice).
- `set_task_folder(path)` selects as above and returns the same object, or a
  plain error string. `choose_task_folder` only opens the native folder picker
  (a held trial refuses before it opens); the renderer then calls
  `set_task_folder`.
- `tasks_query(query, variables?)` returns the GraphQL `{data, errors}` from
  `task_graphql::execute_read_only`, whose schema has no mutation type: any
  mutation fails with `READ_ONLY` before anything runs. Same bounds as above.

The Tasks page (corner menu → Tasks) lists tasks grouped by state — doing,
review, blocked, open, done, dropped — 200 per state per page, with id, title,
owner and update time. A task shows its request verbatim, source, outcome,
state, owner, reviewer, parent (a link), evidence, times and history, newest
last. Unreadable files are named. Each state is its own request: a state that
cannot be read shows its own error and the others still show. Choosing a
folder drops every list, task and folder-status read still in flight, starts
none while the choice is pending, and reads again once it settles, whether it
succeeded or not. It has no edit control; people ask an agent to change a
task. No live events: **Refresh** reads again. A held trial allows
`get_task_folder` and `tasks_query` (both say the folder is not opened) and
refuses `set_task_folder` and `choose_task_folder`.

Task changes emit no runtime events yet. The CLI form is `--prim1-room tasks`
(A16). `tasks` is in Claude's pre-approved pane-tool list.

## Session register (A108)

PRIM-1 keeps each Claude, Codex and Grok member's current conversation in a
small Git-tracked register in the personal repository. The runtime catalog
keeps launch authority; the register is the durable identity record and never
grants a launch. It holds no transcript text and no secrets.

- **Personal repository.** `PRIM1_PERSONAL_REPO` decides first: `off` turns
  the register off, a full folder path overrides (QA and copied runtimes set
  one of the two). Then the Settings → Personal repository choice, saved as
  `personal-repo.json` (`{schema_version: 1, folder}`) in the runtime
  directory; then the workspace. A choice that cannot be used (a missing
  folder, an unreadable file, a relative override) turns the register off and
  says why; it never falls through to the next source. The folder must exist.
  A held trial has no register.
- **Install id.** `<runtime>\install-id` holds one UUID, created once, and
  only while the register is on (at start, or when a folder is chosen in
  Settings): a runtime with the register off keeps none. It names this install
  as a file's holder; an unreadable id is reported, never replaced.
- **Files.** `<repo>\prim1\sessions\<prim-session-uuid>.json`, schema 1, keys
  sorted, LF: `schema`, `revision`, `prim_session_id`, `harness`, `label`,
  `room{id, label}`, `conversation{id, since, via, confirmed_at}`,
  `history[{id, from, to, reason, via, run_id}]`, `titles{current, source}`
  (display only; Claude's session name, source `claude_name`, once its
  conversation is the one followed),
  `holder{machine, install_id, pid, proc_start, run_id, state, since}` (state
  `running`, `stopping`, `stopped` or `retired`; `stopping` keeps the prior
  `pid` and `run_id` while a stop has not proved that the process ended, and
  it turns `stopped` only once the release is proved), `working_dir`,
  `transcript_hint{home, relative}` and `updated_at`. History is append-only:
  an entry per conversation that became current; only the open entry's `to`
  is set, once, when its successor arrives. A deleted member's file stays,
  `retired`. `prim1\.gitignore` (`*.tmp`, `.*.tmp`, `*.lock`) and
  `prim1\.gitattributes` (`* text eol=lf`) are created when missing. Fields
  a later build adds, at any level, are read, kept and written back; only a
  breaking change raises `schema`.
- **Writes.** One background writer per runtime. A successful catalog write,
  each proven conversation change and each run start or end (a run installed,
  stopped, restarted or exited, also when no conversation, label or room
  changes) are handed to it over a channel; the holder is read from the
  member's slot when its file is written. No file or Git I/O runs under the
  slots, catalog or rooms locks. A burst is written
  after about 0.5 s of quiet: temp file then replace, `revision` one above the
  file on disk. An install writes only files it holds: a file whose
  `holder.install_id` names another install is reported in Settings and the
  system log and left as it is. A file that cannot be read or replaced (a
  sharing violation, merge markers) keeps its changes; they are written with
  the next change or on a retry timer (20 s, doubling to 10 min), reported
  until then. One writer at a time per repository, across installs and
  copied runtimes: each file is read again, its holder and revision
  rechecked, and replaced under an OS lock on `prim1\.writer.lock` (ignored
  by `*.lock`); a file another install took meanwhile is left as it is, and a
  lock held elsewhere keeps the changes like a failed write. Before it
  replaces files, the writer records in the session catalog's
  `register_acks` (per personal repository and member) the revision and
  conversation it is about to write: the watermark. Shutdown writes what it
  can, each member's stopped state included, starts no git, gives a git
  already running (also one the writer was waiting for) the rest of its
  10 s and stops the writer. PRIM-1 never kills that git; the desktop's job
  may still end one that outlasts the 10 s when the app exits;
  the system log line says how many members were not written, how many
  files are owed a commit, and whether a git was still running. The next
  start brings unwritten files up to the session catalog and commits what is
  owed. After the close the register never starts again in that process.
- **Reasons.** `launch` and `start_fresh` (minted at launch), `resume_here`,
  `import` (a member a room import created with a conversation), `observed`
  (proven, cause unknown: a Codex identify report, or the catalog without a
  proof event, `via: "catalog"`), and the followers' `clear`, `new`, `resume`,
  `fork`, `rename`.
- **Git.** About 20 s after the last write: `git -C <repo> commit --only -m
  "ADD: PRIM-1 session register - <labels>" -- <files>`, only `prim1` files
  this install wrote. Nothing is staged: a tracked file is committed from the
  working tree, and a new one first gets `git add --intent-to-add -- <files>`,
  which a commit someone else makes meanwhile does not take (a plain
  `git add` would let it). Nothing is pushed. No commit is made on a detached
  HEAD or during a rebase, merge, cherry-pick, revert or bisect: the files
  wait, the state is shown in Settings, and the commit follows once it ends.
  A held `index.lock` is retried later; other failures back off and are
  reported. Outside a Git work tree, with Git missing, or when a `.gitignore`
  matches `prim1`, files are written and not committed. Closing PRIM-1 starts
  no git (the app's job ends its children). Each start, and each change of
  personal repository, runs `git status -- prim1` once and commits every
  changed `prim1` file this install holds, a deleted member's included.
- **Startup.** Before anything can launch, each member's file held by this
  install is compared with the catalog. A file at or behind its watermark is
  this install's own and only lags (a failed write, the register off, a close
  between the catalog write and the register's): the writer brings in the
  catalog's conversation (`observed`, `via: "catalog"`) and nothing is held.
  A file ahead of its watermark, or with none, that names another
  conversation holds the member as `register_disagrees`, naming both IDs (the
  register keeps its value until a proven change); one conversation the
  register names for two members holds them as `register_duplicate`. No
  other hold is replaced or released: a member under a duplicate catalog
  identity or an in-use copy (`room_import_in_use`, so Resume here stays
  offered) keeps that hold, and a differing register conversation is only
  added to its recovery history as `register_disagrees`. Under a duplicate
  catalog identity the register keeps its own value; under an in-use copy or
  a Codex identity hold a lagging file follows the catalog, as it does live.
  A register hold is lifted at a later start once the register agrees
  again; a Codex member then returns to its identity hold, which still needs
  the thread's rollout to resume. Launch stays refused while held; Start
  fresh session works as for any hold. Choosing another personal repository,
  or one used before, brings its lagging files in line the same way.

**Desktop.** `get_personal_repo` returns `{folder, source: "environment" |
"setting" | "default", status: "on" | "off" | "error", message?}`.
`set_personal_repo(path)` saves the choice and moves later writes there (files
already written stay where they are); it is refused while
`PRIM1_PERSONAL_REPO` is set and in a held trial. `choose_personal_repo` only
opens the native folder picker. A held trial allows only `get_personal_repo`.

## Local conversations (A108-B)

Settings → Local conversations → **Scan personal repository** reads bounded
Claude, Codex and Grok metadata for the same effective personal repository.
An explicit disabled or unusable choice does not fall back to another folder.
Partial results and provider errors remain visible. No transcript text is returned.
Archived Codex conversations must first be unarchived in Codex; suffixed rollout
files are not yet supported for PRIM-1 resume. Both remain listed with a reason,
not an attachment control.

The operator can open a root candidate and **Attach for recovery** to a fully
closed same-driver member in that folder. Confirmation names both exact IDs.
`attach_local_session({request: {session_id, driver, conversation_id}})` rechecks
metadata, target state and same-runtime duplicates, saves the catalog binding and
its `room_import_in_use` hold, preserves replaced recovery references, then queues
an `import` register event. It is not a pane tool and held trials refuse it.

Nothing starts. A discovered conversation may still run in another application;
there is no global outside-owner absence proof. Close it everywhere else, then
explicitly choose **Resume here** on the member. Ordinary Launch cannot release
the import hold. A failed state refresh after a successful attachment is reported
as such: use Refresh state rather than repeat the attachment.

## Interrupted-work continuation (A107)

Native exact resume can queue one continuation for a room member last observed
Thinking or ToolCall before an interruption. Idle, Blocked, ErrorLoop and unknown
states do not trigger it. It uses the existing room journal and delivery gates;
stored, written and answered remain different outcomes. An upgrade owns its own
captured run's continuation, so ordinary recovery does not duplicate it.

Work-state transitions, the interrupted run and a once-created request UUID are
stored in the session catalog. A second restart retains that original pending
request. A missing identity observation stays ineligible until the same run's
verified identity update binds it. Fresh/different-conversation starts do not
inherit prompts. An uncertain Posting result whose retained journal evidence is
unavailable is warned about, never posted again automatically.

Explicit Stop clears continuation eligibility even for an already stopped member.
Stop proceeds despite a metadata-write failure, clearing in-memory eligibility
and warning; an abrupt crash before a later successful catalog write can retain
older on-disk state. Work-state persistence failures have the same durability
limit. Existing already-admitted room messages are not revoked by this feature.

This slice does not itself start a stopped member, schedule crash respawn or
enable hibernation. The prepared desktop sleeping controls stay unavailable
until their native setting/snapshot interface is present. Production app walks
are tracked separately from source and unit-test completion in the development plan, which is not part of this repository.
