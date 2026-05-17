# PRIM-1

A local multi-agent runtime for terminal-first AI tools. PRIM-1 hosts CLI agents (currently Claude Code and Codex CLI) inside supervised pseudo-terminal panes in a Tauri desktop app, with structured peer-to-peer routing, a sideband control plane, and an append-only audit log of every routed message and lifecycle event.

**License:** Apache-2.0
**Status:** Working runtime, in active development. Open-source, single-author personal project.

---

## What it does

- **Supervisor-owned PTYs** — the Rust supervisor owns each agent's stdin / stdout / lifecycle. Agents cannot kill each other; only the supervisor restarts.
- **Visible terminal UI** — every pane renders live via xterm.js. The operator sees agent output exactly as it happens.
- **Structured routing** — agents can send each other direct messages (`-To codex -Scope direct`) or post to a shared room (`-To room -Scope room`). The supervisor stamps provenance (`[Direct message from claude]`) and logs each routed message.
- **Sideband control plane** — a local named pipe (Windows) / Unix socket exposes operator actions: `ping`, `list`, `start`, `stop`, `restart`, `input`, `key`, `route`, `deliver`, `events_since`, `wait_quiet`.
- **Append-only audit log** at `.runtime/audit/YYYY-MM-DD.jsonl` capturing every event (session_state, session_output, routed_message, sideband_request_lifecycle, system_log, pair_created/renamed/deleted).
- **Dynamic pair management** — beyond the protected `main` pair (`claude` + `codex`), additional pairs can be created, renamed, and deleted at runtime. Pair-scoped room broadcasts by default.

## Prerequisites

- **Rust** (1.80+ recommended) with `cargo`
- **Node.js** (20+) with `npm` for the Tauri frontend
- **Tauri 2** CLI: `cargo install tauri-cli --version "^2"`
- **Claude Code CLI** (`claude`) — for the Claude pane driver
- **Codex CLI** (`codex`) — for the Codex pane driver
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

The release binary lands in `apps/desktop/src-tauri/target/release/`.

## Run

```bash
# From the repo root after building:
./apps/desktop/src-tauri/target/release/prim1-desktop.exe
```

The wrapper boots with the protected `main` pair (`claude` + `codex` panes) and a system-log pane. Additional pairs can be created from the sidebar.

For headless / scripted operation, use the PowerShell control plane:

```powershell
# Start a session
./scripts/control-plane.ps1 -Action start -Session claude

# Send a routed message
./scripts/agent-route.ps1 -From claude -To codex -Scope direct -Content 'hello from claude'

# Inspect state
./scripts/control-plane.ps1 -Action list

# Tail recent audit events
./scripts/control-plane.ps1 -Action events_since -CursorFile .runtime/cursors/me.json
```

Full operator surface is documented in [CONTROL-SURFACE.md](CONTROL-SURFACE.md).

## Architecture

PRIM-1 separates three concerns:

1. **Visibility layer** — real PTYs owned by the supervisor; operators see live agent output verbatim.
2. **Control layer** — a local named-pipe API for structured actions (send, spawn, restart, stop, route, ping). Not a stdout-parsing bridge.
3. **Policy layer** — the supervisor decides what actions are allowed, when processes are restarted, and what sandbox / security boundaries apply (e.g., peer slash commands are disabled by default; pane-bound credentials only authorize self-action).

The full architecture, including PTY ownership, drivers, the event bus, lifecycle states, and the audit schema, lives in [ARCHITECTURE.md](ARCHITECTURE.md) and [RUNTIME-CONTRACTS.md](RUNTIME-CONTRACTS.md).

## Repo layout

```
apps/desktop/             Tauri desktop shell + xterm.js frontend
crates/supervisor/        Rust runtime brain — registry, lifecycle, routing, audit
crates/pty-host/          Portable-PTY ownership and process attachment
crates/control-plane/     Named-pipe / Unix-socket transport
crates/driver-claude/     Claude Code launch + parse behavior
crates/driver-codex/      Codex CLI launch + parse behavior
crates/driver-generic-terminal/  Fallback driver for any terminal program
crates/shared-types/      Cross-crate Rust types
scripts/                  PowerShell + Python operator helpers
tests/                    Integration tests
docs/                     Design + planning documents
```

## Runtime configuration

Environment variables modify runtime behavior. Set them in a gitignored `.env` at the repo root (loaded automatically at startup) or in your shell. Values containing spaces must be quoted in `.env`:

```
PRIM1_AGENT_WORKING_ROOT="C:/path/to/your working repo"
```

| Variable | Purpose |
|---|---|
| `PRIM1_AGENT_WORKING_ROOT` | Absolute path used as `working_dir` for default-spawned agent panes. Set this to the directory whose `CLAUDE.md` / project context you want loaded. When unset, falls back to the parent directory of the wrapper repo (legacy heuristic; works only when the wrapper lives inside your working repo). |
| `PRIM1_WRAPPER_ROOT` | Absolute path to the wrapper source tree, used for Claude's `--add-dir` so the Claude pane can read wrapper internals. When unset, falls back to `<working_dir>/<PRIM1_WRAPPER_DIRNAME or PRIM-1>`. |
| `PRIM1_WRAPPER_DIRNAME` | Override the wrapper directory name used by the `PRIM1_WRAPPER_ROOT` fallback. Default: `PRIM-1`. |
| `PRIM1_PEER_SLASH_COMMANDS_ALLOWED` | `1` / `true` — re-enable pane-bound `send_input` / `send_key` control over peer panes. Default off (panes can only control their own session). |
| `PRIM1_CROSS_PAIR_ROOM_BROADCAST` | `1` / `true` — re-enable legacy cross-pair Room fan-out. Default off (pair-scoped). |

Restart the wrapper after changing `.env` or environment variables — they are read at startup only.

## Design rules

1. The supervisor is the only entity allowed to spawn, kill, restart, and reattach agents.
2. Agents do not directly restart or kill each other.
3. PTY is the visibility layer. The sideband API is the control layer.
4. Explicit API/script calls are the primary command channel. stdout parsing is secondary.
5. Idle and stuck are different states and must be handled differently.
6. The runtime is generic across terminal-first CLIs, not hardcoded to Claude/Codex.
7. The first milestone is the real-time room; autonomous orchestration layers on top.

## Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md) — full system shape: supervisor, PTY host, drivers, bus, UI, lifecycle, security
- [RUNTIME-CONTRACTS.md](RUNTIME-CONTRACTS.md) — message envelopes, routing semantics, audit schema
- [CONTROL-SURFACE.md](CONTROL-SURFACE.md) — operator + agent control surface (scripts, shortcuts, command reference)
- [STACK-DECISIONS.md](STACK-DECISIONS.md) — locked technical decisions and scope boundaries
- [ROADMAP.md](ROADMAP.md) — forward direction: generic terminals, dynamic panes, cross-platform
- [instructions.md](instructions.md) — inter-agent handshake and routing protocol
- [docs/KNOWN_ISSUES.md](docs/KNOWN_ISSUES.md) — current limitations and known bugs

## Security note

PRIM-1 runs locally. The named-pipe control plane is access-token-gated (token written to `.runtime/control-plane.json` on startup, readable only by the launching user). Pane-bound credentials at `.runtime/control-plane-<pane>.json` further restrict which panes a sideband caller can act on.

Treat `.runtime/` as machine-local state. It is gitignored by default. Do not commit anything under `.runtime/`.

## Contributing

This is a personal open-source project. Bug reports and well-scoped pull requests are welcome via GitHub Issues / PRs against the `main` branch.

## License

Apache-2.0 — see [LICENSE](LICENSE).

Copyright 2026 Victor Valtchev.
