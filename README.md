# PRIM-1

A local multi-agent runtime for terminal-first AI tools. PRIM-1 hosts CLI agents inside supervised pseudo-terminal sessions in a Tauri desktop app, with stable session identity, a pane-local sideband control plane, and an append-only metadata audit of lifecycle and delivery activity.

**License:** Apache-2.0
**Platform:** Windows 10/11. macOS and Linux ports are planned; they are not in this tree.
**Status:** Working runtime, in active development.

---

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

## Prerequisites

- **Windows 10/11** — this tree is Windows-only. macOS and Linux ports are planned (see [ROADMAP.md](ROADMAP.md)).
- **Rust 1.85+** with `cargo` (the workspace uses edition 2024). Last verified with rustc 1.95.0.
- **MSVC C++ Build Tools** (the Visual Studio "Desktop development with C++" workload, or Build Tools for Visual Studio) — required to compile native crates on Windows.
- **WebView2 Runtime** — required by the Tauri 2 desktop shell (preinstalled on current Windows 11; install the Evergreen runtime on Windows 10 if missing).
- **Node.js 20+** with `npm` for the desktop frontend
- **Tauri 2** CLI: `cargo install tauri-cli --version "^2"`
- **Claude Code CLI** (`claude`) — for the Claude pane driver
- **Codex CLI** (`codex`) — for the Codex pane driver
- **Grok Build CLI** (`grok`) — for the Grok pane driver
- **Prime Agent** (`prime-agent`) in the Ubuntu WSL distribution — for the Prime pane driver; WSL must have a working user `systemd` manager

## Build

The Tauri crate embeds `apps/desktop/dist/index.html`. Build that frontend **before** any workspace `cargo check` / `cargo test` / `cargo build`.

```bash
cd apps/desktop
npm ci
npm run build
cd ../..

cargo test --workspace
cd apps/desktop
npm run tauri build
```

The workspace release binary lands in `target/release/` at the repository root.

**Build the desktop exe only through the Tauri CLI** (`npm run tauri build`, add
`-- --no-bundle` to skip installers). A plain `cargo build --release` of the
desktop crate produces a *dev-mode* exe that loads the Vite dev server at
`localhost:1420` instead of the embedded frontend — it starts, then shows
"localhost refused to connect". The tell is size: the production exe is ~14 MB,
the dev-mode one ~12 MB (2026-09-09).

## Run

```bash
# From the repo root after building:
./target/release/cli-master-wrapper-desktop.exe
```

On a fresh runtime, the wrapper opens with a system log and a focused **New session** action. It restores persisted definitions as closed tabs on later launches and starts no harness until the operator explicitly selects **Start**.

External operators use the desktop UI. Inside an authorized supervised pane,
the narrow PowerShell helper can address that pane through its injected endpoint:

```powershell
# Check the pane-local pipe
./scripts/control-plane.ps1 -Action ping -Quiet

# Write raw input, then submit it
./scripts/control-plane.ps1 -Action input -Session $env:PRIM1_PANE_IDENTITY -Content 'status'
./scripts/control-plane.ps1 -Action key -Session $env:PRIM1_PANE_IDENTITY -Key enter

# Read or post the calling pane's current room (RoomId and sender are derived)
./scripts/control-plane.ps1 -Action room_read -Quiet -PassThruJson
./scripts/control-plane.ps1 -Action room_post -Content 'Status from this pane'
```

Full operator surface is documented in [CONTROL-SURFACE.md](CONTROL-SURFACE.md).

### Keyboard

Bindings follow the native terminal of the platform the app runs on — Windows
Terminal on Windows, GNOME Terminal on Linux, Terminal.app / iTerm2 on macOS.
Every binding lives in one table, `apps/desktop/src/keymap.ts`; the F1 page
and this list are rendered from it. Anything not listed reaches the focused
terminal as keystrokes, so Ctrl+C with nothing selected is still the interrupt
everywhere, and on Linux Ctrl+V still reaches the harness as `^V`.

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

The bare Windows Ctrl+C copies and then drops the selection, so the next Ctrl+C
is the interrupt again (the Windows Terminal rule). Ctrl+V on Windows pastes
text; the harnesses' image paste stays on Alt+V. To change a binding, edit the
table and its tests (`keymap.test.ts`); nothing else knows a key.

## Architecture

PRIM-1 separates three concerns:

1. **Visibility layer** — real PTYs owned by the supervisor; operators see live session output through xterm.js.
2. **Control layer** — in-process desktop commands own operator actions; a narrow pane-local named pipe handles only self-pane input/key and quiet-state probes. Neither is a stdout-parsing bridge.
3. **Policy layer** — the supervisor decides which caller owns a pane and what actions are allowed. External scripts do not gain authority from bearer files; see [Known issues](docs/KNOWN_ISSUES.md).

The full architecture, including PTY ownership, drivers, the event bus, lifecycle states, and the audit schema, lives in [ARCHITECTURE.md](ARCHITECTURE.md) and [RUNTIME-CONTRACTS.md](RUNTIME-CONTRACTS.md).

## Repo layout

```
apps/desktop/             Tauri desktop shell + xterm.js frontend
crates/supervisor/        Rust runtime brain — registry, lifecycle, routing, audit
crates/pty-host/          Portable-PTY ownership and process attachment
crates/control-plane/     Named-pipe / Unix-socket transport
crates/driver-claude/     Claude Code launch + parse behavior
crates/driver-codex/      Codex CLI launch + parse behavior
crates/driver-grok/       Grok Build launch + parse behavior
crates/driver-prime/      Prime Agent launch behavior for Ubuntu WSL
crates/driver-generic-terminal/  Fallback driver for any terminal program
crates/shared-types/      Cross-crate Rust types
scripts/                  PowerShell + Python operator helpers
tests/                    Integration tests
docs/                     Design + planning documents
```

## Runtime configuration

Environment variables modify runtime behavior. Set them in your shell, in the explicit file named by `PRIM1_ENV_FILE`, or in a gitignored `.env` in the process startup directory (the executable directory is the final fallback). Values containing spaces must be quoted in `.env`:

```
PRIM1_AGENT_WORKING_ROOT="C:/path/to/your working repo"
```

| Variable | Purpose |
|---|---|
| `PRIM1_RUNTIME_DIR` | Explicit product runtime directory used by the desktop and direct runtime readers. Relative values resolve against the desktop startup directory. Overrides the platform default without moving the source checkout. |
| `PRIM1_CONTROL_PLANE_ENDPOINT` | Supervisor-injected pane-local named-pipe endpoint used by the narrow PowerShell helper. It is not an operator credential or a runtime-file discovery mechanism. |
| `PRIM1_ENV_FILE` | Explicit dotenv file. Relative paths resolve against the process startup directory; an explicit missing or malformed file fails startup. |
| `PRIM1_AGENT_WORKING_ROOT` | Initial workspace preference used only when the versioned session catalog does not yet exist. When unset, the captured process startup directory is used exactly. Later workspace changes use the native picker and persist atomically. |
| `PRIM1_START_MINIMIZED` | `1` / `true` starts the desktop minimized without activation; fullscreen is deferred until the operator intentionally restores it. `0` / `false` or unset preserves the normal visible fullscreen startup. Invalid values fail startup. |

Restart the wrapper after changing `.env` or environment variables — they are read at startup only.

Product runtime state is outside the source checkout. On Windows, the desktop and product scripts default to `%LOCALAPPDATA%\io.prim1.runtime\runtime`. The shared script resolver also maps macOS to `~/Library/Application Support/io.prim1.runtime/runtime` and Linux to `${XDG_DATA_HOME:-~/.local/share}/io.prim1.runtime/runtime`; those mappings do not constitute a macOS or Linux release-support claim.

The private session catalog is `<runtime-dir>/session-catalog-v1.json`. It stores
only ordered session intent and the workspace preference—not process IDs,
`RunId`s, command arguments, environment variables, terminal output, or message
content. A corrupt or unknown catalog version fails startup visibly and is never
silently replaced with defaults.

The private room catalog is `<runtime-dir>/room-catalog-v1.json`. It stores only
ordered `RoomId`, label, member `SessionId` values, and membership revision.
Feed messages, delivery errors, and cursors are process-memory state only. A
corrupt or unknown room catalog also fails startup without rewrite.

Product scripts never discover control authority through the runtime directory. The
control helper requires `PRIM1_CONTROL_PLANE_ENDPOINT`; an explicit `-Endpoint`
override exists only for isolated named-pipe tests. Direct audit readers continue
to use the runtime-directory resolver, including roots containing spaces.

## Design rules

1. PRIM-1 performs managed-agent spawn, kill, and restart through the supervisor; the desktop UI is the operator entry point. `Closed` is reported only after the exact run's owned process job is proved empty. Reopening the desktop restores closed definitions, not live runs.
2. The pane-local sideband cannot request peer lifecycle actions. Arbitrary same-user processes remain outside this guarantee.
3. PTY is the visibility layer. The sideband API is the control layer.
4. Explicit API/script calls are the primary command channel. stdout parsing is secondary.
5. Idle and stuck are different states and must be handled differently.
6. The runtime is generic across terminal-first CLIs, not hardcoded to Claude/Codex.
7. Room membership is explicit and `RoomId`-keyed. Feed posts never imply PTY delivery; one-member and Send All delivery are separate visible actions. Autonomous orchestration remains a later layer.

## Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md) — full system shape: supervisor, PTY host, drivers, bus, UI, lifecycle, security
- [RUNTIME-CONTRACTS.md](RUNTIME-CONTRACTS.md) — message envelopes, routing semantics, audit schema
- [CONTROL-SURFACE.md](CONTROL-SURFACE.md) — operator + agent control surface (scripts, shortcuts, command reference)
- [STACK-DECISIONS.md](STACK-DECISIONS.md) — locked technical decisions and scope boundaries
- [ROADMAP.md](ROADMAP.md) — forward direction: generic terminals, dynamic panes, cross-platform
- [docs/KNOWN_ISSUES.md](docs/KNOWN_ISSUES.md) — current limitations and known bugs

## Security note

PRIM-1 runs locally. The pane sideband carries no bearer token and no master or
per-pane credential file is an operator backdoor. Native pane authority must be
derived by the supervisor from the named-pipe caller and bound to the live PTY
process job and generation at mutation time, or — for a caller outside every
pane Job — from the per-run pane secret it presents. Prime/WSL sessions still
do not receive the sideband or routed-message surface: their launch forwards no
environment, so neither the Job proof nor the secret reaches them yet. Use raw
input through the desktop UI for Prime. Grok's TUI receives no MCP child but
reaches the same surface through the CLI (`PRIM1_CLI`) with the pane secret.

Treat the app-local runtime directory as private machine-local state. The repo-local `.runtime/` directory remains gitignored for test environments and developer scratch files; it is no longer the product runtime default.

The packaged renderer loads only bundled assets under an explicit Content
Security Policy. It exposes no global Tauri API during normal use, ships without
the in-app devtools feature, and grants the renderer only event-listener plus
read-only window-state capabilities. Exact-artifact QA may opt into a numeric
`PRIM1_CDP_PORT`; that mode binds WebView2 debugging to loopback, never enables
wildcard origins, and exposes a frozen automation bridge only for the lifetime
of that explicitly instrumented process. Production-artifact screenshots use
the WebView's built-in `Page.captureScreenshot` path.

## Contributors

- **Victor Valtchev** — runtime, supervisor, drivers, product
- **Alexander Valtchev** — desktop UI and bugfixes

## Acknowledgements

An earlier private macOS experiment by Martin Tomov is not part of this repository. A first-class macOS (and Linux) port is planned against the current Windows tree.

## Contributing

Bug reports and well-scoped pull requests are welcome via GitHub Issues / PRs against `main`.

## License

Apache-2.0 — see [LICENSE](LICENSE). Software is provided AS IS, without warranty.

Some compiled dependencies (the `cssparser` / `selectors` family) are **MPL-2.0**. That is file-level copyleft: Apache-2.0 is compatible, but distributing binaries that include those files still requires MPL source availability and notices for the MPL-covered files. See [NOTICE](NOTICE).

Copyright 2026 Victor Valtchev.
