# PRIM-1

A local multi-agent runtime for terminal-first AI tools. PRIM-1 hosts CLI agents inside supervised pseudo-terminal sessions in a Tauri desktop app, with stable session identity, a pane-local sideband control plane, and an append-only metadata audit of lifecycle and delivery activity.

**License:** Apache-2.0
**Status:** Working runtime, in active development. Open-source, single-author personal project.

---

## What it does

- **Supervisor-owned PTYs** — the Rust supervisor owns each managed agent's stdin / stdout / lifecycle. The pane-local sideband exposes no peer lifecycle action; this protocol boundary is not hostile same-user OS isolation.
- **Persistent generic sessions** — a versioned atomic catalog stores the ordered `SessionId`, label, driver, selected working directory, and visible permission profile for each session. A fresh install starts with zero sessions and zero harness processes.
- **Flat terminal tabs** — the desktop renders one active xterm.js terminal at a time while retaining inactive terminal buffers. Tabs are backend-ordered and keyed only by stable `SessionId`; duplicate labels are allowed and visibly disambiguated.
- **Native working-directory selection** — the renderer never supplies an authority-bearing path string. Native folder pickers qualify the workspace default and per-session directory before persistence and revalidate it before spawn.
- **Explicit permission posture** — Claude Code, Codex, and Grok default to their normal approval/sandbox modes. Their measured bypass modes are available only through a visibly selected `Unsafe` profile; Generic Terminal supports `Normal` only.
- **Direct launch** — drivers launch qualified executables without `cmd.exe`, `.cmd` shims, renderer arguments, or source-checkout access. Labels and metacharacters are never interpreted as shell syntax.
- **Structured routing core** — the in-process supervisor retains one-recipient `SessionId` routing with exact-run framing checks, but the Gate-4 tab UI intentionally exposes no message composer or pseudo-room. Visible room messaging waits for explicit `RoomId` membership. PTY-write receipts do not prove final-child byte fidelity, model receipt, or task completion.
- **Pane-local sideband** — an authorized supervised pane gets a narrow named-pipe surface for `ping`, `wait_quiet`, raw `input`, and PTY `key` actions. Operator lifecycle, routing, rooms, and inventory remain in the desktop UI.
- **Append-only metadata audit** at `<runtime-dir>/audit/YYYY-MM-DD.jsonl` for lifecycle, authorization, dispatch, delivery, and session-definition receipts. Terminal output is not persisted; routed-message content is stored as `[content omitted]`.

## Prerequisites

- **Rust** (1.80+ recommended) with `cargo`
- **Node.js** (20+) with `npm` for the Tauri frontend
- **Tauri 2** CLI: `cargo install tauri-cli --version "^2"`
- **Claude Code CLI** (`claude`) — for the Claude pane driver
- **Codex CLI** (`codex`) — for the Codex pane driver
- **Grok Build CLI** (`grok`) — for the Grok pane driver
- **Windows 10/11** — currently Windows-first. Linux/macOS work is planned but not yet validated (see [ROADMAP.md](ROADMAP.md)).

## Build

```bash
# Check Rust workspace
cargo check
cargo test

# Build the Tauri desktop app
cd apps/desktop
npm install
npm run build
npm run tauri build
```

The workspace release binary lands in `target/release/` at the repository root.

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
```

Full operator surface is documented in [CONTROL-SURFACE.md](CONTROL-SURFACE.md).

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
7. The next messaging milestone is explicit `RoomId` membership; autonomous orchestration layers on top.

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
process job and generation at mutation time. Prime/WSL sideband access is not
claimed until that boundary has its own empirical transport test; use the desktop
UI for operator control.

Treat the app-local runtime directory as private machine-local state. The repo-local `.runtime/` directory remains gitignored for test environments and developer scratch files; it is no longer the product runtime default.

## Contributing

This is a personal open-source project. Bug reports and well-scoped pull requests are welcome via GitHub Issues / PRs against the `main` branch.

## License

Apache-2.0 — see [LICENSE](LICENSE).

Copyright 2026 Victor Valtchev.
