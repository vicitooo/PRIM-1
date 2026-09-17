# PRIM-1 — README for agents

Protocol-level notes. Humans start at [README.md](README.md). Tools that auto-load [AGENTS.md](AGENTS.md) are pointed here.

PRIM-1 hosts terminal-first CLI agents in supervised PTYs inside a Tauri desktop app. **Windows only** in this tree.

## What it does

- **Supervisor-owned PTYs.** The supervisor owns launch, input, output, stop, and restart. The pane-local protocol grants no peer lifecycle authority; this is not hostile same-user OS isolation.
- **Persistent sessions and rooms.** Stable session IDs, ordered definitions, working directories, permission profiles, conversation references, and room membership survive restart. The room feed and terminal buffers do not.
- **Rooms and lobby.** Each session belongs to at most one room; unassigned sessions live in the lobby. One terminal is visible at a time, with inactive buffers retained while the app runs. Changing context does not stop sessions.
- **Launch and resume.** A fresh install has no sessions. Creating a session launches it. The default **Continue where I left off** setting relaunches previously running sessions when their room or lobby is entered. A stored conversation reference is reused where supported; **Start fresh session** clears it.
- **Qualified directories.** Windows folders enter through the native picker and are revalidated before spawn. Prime uses a backend-qualified absolute Ubuntu path.
- **Explicit permissions.** Claude Code, Codex, and Grok default to normal approval/sandbox settings. Bypass modes require the visible **Unsafe** profile. Prime and Generic Terminal support **Normal** only.
- **Grok full-screen sessions.** Grok launches with `--fullscreen`, a working directory, the selected permission mode, and a new or resumed conversation ID. Readiness depends on recognized terminal output and bracketed-paste support, not a timer. Routed input uses one bracketed-paste payload followed by a paced Enter.
- **Fail-closed prompt detection.** Driver observations distinguish working, idle, blocked, and unknown states. Codex prompt/footer recognition and modal handling use a bounded terminal viewport. Unknown or blocked states refuse synthetic delivery; raw terminal input remains available.
- **Deliberate room traffic.** **Post to feed** writes no PTY. **Send** targets one member or all selected members, with whole-recipient preflight followed by per-recipient receipts. A failure after writing begins can leave partial delivery. A write receipt does not prove model understanding.
- **Pane-local control.** `ping`, `wait_quiet`, raw `input`, PTY `key`, `room_read`, `room_post`, and `room_deliver`. Room and sender identity come from the authenticated pane. A delivery recipient is a member label, session ID, or `all` (every other member).
- **Two identity paths.** Native Windows Job membership identifies the caller first. A caller outside all pane Jobs can use the per-run `PRIM1_PANE_SECRET` inherited from its pane. It rotates on restart and grants authority only for that run.
- **Model-facing tools.** Claude Code and Codex receive the `prim1_pane` MCP tools `ping`, `room_read`, `room_post`, and `room_deliver`. Other non-Prime panes can use the executable's room CLI. Prime has no pane sideband or routed delivery.
- **Room briefing.** Optional automatic briefing is once per membership, delivered when the member can accept it. Restarting a session does not repeat a completed brief. The operator can edit the template or request **Brief now**.
- **Metadata audit.** Lifecycle, authorization, dispatch, and delivery metadata goes to `<runtime-dir>/audit/YYYY-MM-DD.jsonl`. Raw terminal output events are omitted; room and routed-message bodies are replaced with `[content omitted]`.

## Prerequisites and build

Follow [README.md](README.md). Rust **1.95.0** is the tested toolchain. Build the frontend before workspace Cargo commands. Use `npm run tauri build` for the production desktop executable.

## In-pane control

From a supervised non-Prime pane, with the checkout as the working directory:

```powershell
.\scripts\control-plane.ps1 -Action ping -Quiet
.\scripts\control-plane.ps1 -Action input -Session $env:PRIM1_PANE_IDENTITY -Content 'status'
.\scripts\control-plane.ps1 -Action key -Session $env:PRIM1_PANE_IDENTITY -Key enter
.\scripts\control-plane.ps1 -Action room_read -Quiet -PassThruJson
.\scripts\control-plane.ps1 -Action room_post -Content 'Status from this pane'
.\scripts\control-plane.ps1 -Action room_deliver -Recipient 'Reviewer' -Content 'Please review the change.'
```

The installed executable also provides room commands without a source checkout:

```powershell
& "$env:PRIM1_CLI" --prim1-room ping
& "$env:PRIM1_CLI" --prim1-room read
& "$env:PRIM1_CLI" --prim1-room post 'Status from this pane'
& "$env:PRIM1_CLI" --prim1-room deliver 'Reviewer' 'Please review the change.'
```

These require the pane's injected environment. They are not external operator controls. Full behavior: [CONTROL-SURFACE.md](CONTROL-SURFACE.md).

### Windows keyboard shortcuts

Bindings are defined in [keymap.ts](apps/desktop/src/keymap.ts). Unhandled keys reach the focused terminal.

| Action | Binding |
|---|---|
| Copy selection | Ctrl+C with a selection, Ctrl+Shift+C, or Ctrl+Insert |
| Paste | Ctrl+V, Ctrl+Shift+V, or Shift+Insert |
| Attach a harness | Ctrl+Shift+T |
| Close the active session | Ctrl+Shift+W |
| Next / previous tab | Ctrl+Tab / Ctrl+Shift+Tab |
| Select a focused tab | Left, Right, Home, End |
| Move a focused tab | Ctrl+Shift+Left / Ctrl+Shift+Right |
| Fullscreen | F11 |
| Help | F1 |
| Close a menu | Esc |

Ctrl+C without a selection reaches the terminal.

## Architecture

The desktop owns operator actions. The Rust supervisor owns PTYs, lifecycle, room membership, delivery, and authorization. Drivers supply launch, resume, readiness, and work-state behavior.

See [ARCHITECTURE.md](ARCHITECTURE.md) and [RUNTIME-CONTRACTS.md](RUNTIME-CONTRACTS.md).

## Repo layout

```text
apps/desktop/             Tauri shell and xterm.js frontend
crates/supervisor/        Registry, lifecycle, routing, named-pipe server, audit
crates/pty-host/          PTY and process ownership
crates/control-plane/     Bounded JSON-line protocol codec
crates/driver-*           Per-harness launch and output classification
crates/shared-types/      Cross-crate Rust types
scripts/                  Pane helpers and audit reader
docs/                     Limitations, room brief, and design records
```

## Runtime configuration

Set variables in the launching shell, in `PRIM1_ENV_FILE`, or in a gitignored `.env` in the process startup directory. Restart after changes.

| Variable | Purpose |
|---|---|
| `PRIM1_RUNTIME_DIR` | Runtime directory; relative paths resolve against the desktop startup directory. Must not overlap a session workspace in either direction. |
| `PRIM1_ENV_FILE` | Explicit dotenv file. A missing or malformed explicit file fails startup. |
| `PRIM1_AGENT_WORKING_ROOT` | Initial workspace when no session catalog exists. |
| `PRIM1_START_MINIMIZED` | `1` / `true` starts minimized. Invalid values fail startup. |
| `PRIM1_CONTROL_PLANE_ENDPOINT` | Supervisor-injected named-pipe endpoint; routing metadata, not a credential. |
| `PRIM1_PANE_IDENTITY` | Supervisor-injected alias for this pane's self-input and key requests. |
| `PRIM1_PANE_SECRET` | Per-run pane authentication secret. Keep it private; do not copy it into messages or logs. |
| `PRIM1_CLI` | Supervisor-injected path to the executable's room CLI. |

The default runtime directory is `%LOCALAPPDATA%\io.prim1.runtime\runtime`. The session catalog stores definitions and resume/startup metadata; the room catalog stores definitions, membership, and briefing preferences/state. Neither stores the room feed or terminal transcript.

## Security

PRIM-1 runs on the operator's machine. Attached CLIs use their own providers, accounts, and selected permission policies. The app does not add an OS sandbox between same-user processes.

The packaged renderer loads bundled assets under CSP, without a global Tauri API or in-app devtools. `PRIM1_CDP_PORT` enables QA-only loopback WebView2 debugging. Treat the runtime directory and injected pane secrets as private machine-local state.

## Tests

See [tests/README.md](tests/README.md) for test entry points and [docs/KNOWN_ISSUES.md](docs/KNOWN_ISSUES.md) for limitations. Documentation checks do not substitute for runtime verification.
