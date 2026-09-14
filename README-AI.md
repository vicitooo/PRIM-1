# PRIM-1 — README for agents

Protocol-level notes. Humans start at [README.md](README.md). Tools that auto-load `AGENTS.md` are pointed here from that file.

PRIM-1 hosts CLI agents inside supervised pseudo-terminal sessions in a Tauri desktop app, with stable session identity, a pane-local sideband control plane, and an append-only metadata audit of lifecycle and delivery activity. **Windows only** in this tree.

## What it does

- **Supervisor-owned PTYs** — the Rust supervisor owns each managed agent's stdin / stdout / lifecycle. The pane-local sideband exposes no peer lifecycle action; this protocol boundary is not hostile same-user OS isolation.
- **Persistent generic sessions** — a versioned atomic catalog stores the ordered `SessionId`, label, driver, selected working directory, and visible permission profile for each session. A fresh install starts with zero sessions and zero harness processes.
- **Flat terminal tabs** — the desktop renders one active xterm.js terminal at a time while retaining inactive terminal buffers. Tabs are backend-ordered and keyed only by stable `SessionId`; duplicate labels are allowed and visibly disambiguated.
- **Qualified working directories** — Windows sessions use native folder pickers whose selections are qualified before persistence and revalidated before spawn. Prime uses an explicit Ubuntu path; the backend qualifies its canonical path and device/inode identity, and the create form prefills the qualified Ubuntu home.
- **Explicit permission posture** — Claude Code, Codex, and Grok default to their normal approval/sandbox modes. Their measured bypass modes are available only through a visibly selected `Unsafe` profile; Generic Terminal and Prime support `Normal` only.
- **Reliable Grok minimal mode** — every Grok run launches with `--minimal` and a fresh native session ID, trading the suppressible full-screen response repaint for finalized answer blocks that remain visible in the PTY. The run stays `Starting` until its bounded current-screen projection first observes `Starting session…`, then a later completed trusted frame with Grok's exact `minimal · /help` chrome, the composer, no launcher, and bracketed paste enabled. Routed multiline input is one FIFO-held fused buffer: one bracketed frame per LF-separated source line with Grok's measured Alt+Enter sequence between frames, then one paced Enter. Blank lines and a trailing LF remain exact; carriage returns, bodies above 13 KiB, and more than 256 source lines fail preflight rather than being normalized or attempted beyond Grok Build 1.0.0's receiver-proven envelope. No timer grants readiness, partial writes never trigger Enter or retry, replacement runs cannot inherit progress, and raw terminal input remains available throughout startup.
- **Fail-closed Codex prompts** — one shared, bounded driver-internal terminal viewport consumes raw Codex output across arbitrary chunks, including output emitted before PTY installation. Codex 0.147.0 may compose its prompt across several synchronized and ordinary cursor-hide/show transactions, so Idle requires freshly painted prompt and footer rows—not a single DEC-2026 frame—to show a visible column-3 cursor on an input row exactly `›` or beginning `› `, followed by an indented footer row whose final ` · ` separates a model from a path-like cwd. The legacy `▌` and `esc to interrupt` text never authorize Idle. Braille decoration is treated as spaces when recognizing prompt structure. Modal blockers remain latched until a clean prompt replaces them; transient notices clear on a later clean prompt or activity. Unfamiliar, malformed, unsupported, or over-limit state stays Unknown; after resizing, the prompt and footer rows must be repainted before Idle is accepted. Synthetic delivery rejects Unknown/Blocked while raw terminal input remains available.
- **Direct launch** — drivers launch qualified executables without `cmd.exe`, `.cmd` shims, renderer arguments, or source-checkout access. Labels and metacharacters are never interpreted as shell syntax.
- **Explicit rooms** — a separate atomic catalog stores ordered `RoomId` definitions, labels, membership, and membership revisions. Room content stays in a bounded 512-event / 16 MiB in-memory feed with explicit cursor gaps; it is never restored after process restart.
- **Deliberate room traffic** — **Post** appends to the shared feed without prompting a harness. **Send** targets one member or explicitly all members through the existing exact-run framing path, with whole-recipient preflight and truthful per-recipient pending/written/failed receipts. Generic Terminal receives the operator's validated printable single-line command without an injected text prefix — unless a harness is running inside it (`codex`, `claude`, `grok` started by hand), in which case delivery follows that harness's contract, header included. Provenance remains visible in PRIM's feed, route receipts, and audit. Prime remains raw-terminal-only.
- **Pane-local sideband** — an authorized supervised pane gets a narrow named-pipe surface for `ping`, `wait_quiet`, raw `input`, PTY `key`, membership-derived room `read` / `post`, and `room_deliver` (the operator's Send with the calling pane as sender: same gate, same framing, same receipts; recipient = member label, session id, or `all` = every other member). The pane cannot supply a `RoomId`, sender, peer target, or lifecycle action.
- **Two proofs of pane identity** — kernel Job membership first; when the calling process is outside every pane Job (Grok's tool processes, anything without a Job), the per-run `PRIM1_PANE_SECRET` from the pane's own environment, sent as the connection's first line. The secret identifies its own run only, rotates on every start/restart, and never appears in audit or diagnostics.
- **Every harness can speak** — Claude Code and Codex receive a PRIM-owned `prim1_pane` stdio MCP child (`ping`, `room_read`, `room_post`, `room_deliver`); every other non-Prime pane uses the same executable as a CLI (`"$env:PRIM1_CLI" --prim1-room …`, JSON on stdout) or `scripts/control-plane.ps1`. The supervisor delivers the canonical room brief (`crates/supervisor/src/room_brief.txt`) into a member's terminal on join and on its run's first idle; `room_read` pages list the members with labels.
- **Append-only metadata audit** at `<runtime-dir>/audit/YYYY-MM-DD.jsonl` for lifecycle, authorization, dispatch, delivery, session-definition, and room receipts. Terminal output is not persisted; routed and room-message content is stored as `[content omitted]`.

## Prerequisites and build

See [README.md](README.md). Rust **1.95.0** (tested). Lockfile crates declare compile floor 1.88; 1.88–1.94 are unverified. Frontend `apps/desktop` `npm ci && npm run build` **before** workspace cargo. Production exe only via `npm run tauri build`.

## In-pane control

```powershell
./scripts/control-plane.ps1 -Action ping -Quiet
./scripts/control-plane.ps1 -Action input -Session $env:PRIM1_PANE_IDENTITY -Content 'status'
./scripts/control-plane.ps1 -Action key -Session $env:PRIM1_PANE_IDENTITY -Key enter
./scripts/control-plane.ps1 -Action room_read -Quiet -PassThruJson
./scripts/control-plane.ps1 -Action room_post -Content 'Status from this pane'
```

Full surface: [CONTROL-SURFACE.md](CONTROL-SURFACE.md). Room members: `PRIM1_CLI --prim1-room ping | read | post | deliver`.

### Keyboard

Bindings follow the native terminal of the platform the app runs on — Windows Terminal on Windows, GNOME Terminal on Linux, Terminal.app / iTerm2 on macOS. Every binding lives in `apps/desktop/src/keymap.ts`. Anything not listed reaches the focused terminal as keystrokes.

| Action | Windows | Linux | macOS (untested) |
|---|---|---|---|
| Copy the selection | Ctrl+C (with a selection), Ctrl+Shift+C, Ctrl+Insert | Ctrl+Shift+C, Ctrl+Insert | Cmd+C (with a selection) |
| Paste into the focused terminal | Ctrl+V, Ctrl+Shift+V, Shift+Insert | Ctrl+Shift+V, Shift+Insert | Cmd+V |
| Attach a harness | Ctrl+Shift+T | Ctrl+Shift+T | Cmd+T |
| Close the active session | Ctrl+Shift+W | Ctrl+Shift+W | Cmd+W |
| Next / previous tab | Ctrl+Tab / Ctrl+Shift+Tab | Ctrl+Tab / Ctrl+Shift+Tab, Ctrl+PageDown / Ctrl+PageUp | Ctrl+Tab / Ctrl+Shift+Tab, Cmd+Shift+] / Cmd+Shift+[ |
| With a tab focused: select / move it | ← → Home End / Ctrl+Shift+← → | same | same |
| Fullscreen | F11 | F11 | Ctrl+Cmd+F |
| Help | F1 | F1 | F1 |
| Close any open menu | Esc | Esc | Esc |

## Architecture

1. **Visibility layer** — real PTYs owned by the supervisor; operators see live session output through xterm.js.
2. **Control layer** — in-process desktop commands own operator actions; a narrow pane-local named pipe handles only self-pane input/key and quiet-state probes.
3. **Policy layer** — the supervisor decides which caller owns a pane and what actions are allowed. External scripts do not gain authority from bearer files.

Deeper: [ARCHITECTURE.md](ARCHITECTURE.md), [RUNTIME-CONTRACTS.md](RUNTIME-CONTRACTS.md).

## Repo layout

```
apps/desktop/             Tauri desktop shell + xterm.js frontend
crates/supervisor/        Rust runtime — registry, lifecycle, routing, audit
crates/pty-host/          Portable-PTY ownership and process attachment
crates/control-plane/     Named-pipe / Unix-socket transport
crates/driver-*           Per-harness launch + parse behavior
crates/shared-types/      Cross-crate Rust types
scripts/                  PowerShell + Python operator helpers
docs/                     Design documents
```

## Runtime configuration

Set variables in the shell, in `PRIM1_ENV_FILE`, or in a gitignored `.env` next to the process startup directory. Restart after changes.

| Variable | Purpose |
|---|---|
| `PRIM1_RUNTIME_DIR` | Product runtime directory. Relative values resolve against the desktop startup directory. |
| `PRIM1_CONTROL_PLANE_ENDPOINT` | Supervisor-injected pane-local named-pipe endpoint. Not an operator credential. |
| `PRIM1_ENV_FILE` | Explicit dotenv file. Missing/malformed explicit file fails startup. |
| `PRIM1_AGENT_WORKING_ROOT` | Initial workspace only when the session catalog does not yet exist. |
| `PRIM1_START_MINIMIZED` | `1` / `true` starts minimized. Invalid values fail startup. |

Windows default runtime: `%LOCALAPPDATA%\io.prim1.runtime\runtime`. Session catalog `session-catalog-v1.json` stores ordered session intent only — not PIDs, output, or message content. Room catalog `room-catalog-v1.json` stores membership only; the feed is in-memory and dies with the process.

## Design rules

1. Spawn, kill, and restart go through the supervisor. `Closed` is reported only after the exact run's job is empty.
2. The pane-local sideband cannot request peer lifecycle actions.
3. PTY is visibility. The sideband is control.
4. Explicit API/script calls are the primary command channel. stdout parsing is secondary.
5. Idle and stuck are different states.
6. The runtime is generic across terminal-first CLIs.
7. Room membership is explicit and `RoomId`-keyed. Feed posts never imply PTY delivery.

## Security

The desktop app runs on the operator's machine; attached CLIs still use their own providers and accounts. Pane authority is one of two paths: (1) kernel Job membership of the named-pipe caller, bound to the live PTY job and generation; (2) the per-run `PRIM1_PANE_SECRET` as the connection's first line, for callers outside every pane Job. There is no operator-issued bearer file. Prime/WSL sessions do not receive the sideband. Treat the app-local runtime directory as private machine-local state.

The packaged renderer loads only bundled assets under CSP, no global Tauri API, no in-app devtools. `PRIM1_CDP_PORT` is QA-only, loopback WebView2 debugging.

## Tests worth knowing

`cargo test --workspace` has failed once on `pty-host::windows_real_process_tests::production_spawn_contains_immediate_descendants_at_process_creation` under load while isolation reruns passed. Cause is not established. See [docs/KNOWN_ISSUES.md](docs/KNOWN_ISSUES.md).
