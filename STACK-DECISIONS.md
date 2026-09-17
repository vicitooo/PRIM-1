# PRIM-1 — Stack Decisions

The implemented Windows stack and its rationale. Future product work is in [ROADMAP.md](ROADMAP.md).

## Core stack

- **Rust** owns the supervisor, PTYs, process lifecycle, authorization, routing, and audit.
- **Tauri 2** provides the desktop shell, window controls, packaging, and the bridge to Rust.
- **xterm.js** renders terminal output, scrollback, cursor movement, and alternate screens.
- **Windows named pipes** carry the pane-local protocol. Unix sockets remain a possible transport for future native POSIX ports.

## Why this stack

Rust keeps process ownership and the desktop backend in one runtime. Tauri integrates that backend with a webview UI. xterm.js supplies an established terminal renderer instead of a custom emulator. Local IPC keeps the pane control surface off a network listener.

## Platform strategy

The desktop is Windows-only and uses ConPTY through the PTY layer. Prime Agent is a specific Ubuntu WSL integration with separate Windows and Linux process scopes; it is not a general fallback PTY backend or a Linux desktop port.

## Product shape

A local desktop app with an ordered set of sessions, rooms and an unassigned lobby, one visible terminal at a time, and one supervisor. The app itself is local; attached CLIs use their own model providers.

## Current scope

- Repeated Claude Code, Codex, Grok, Prime, and Generic Terminal sessions.
- Stable session/run/room identity and persistent definitions.
- Stored conversation references and configurable session relaunch on app startup.
- Feed-only posts and explicit recipient delivery from operators or room members.
- Supervisor-owned restart and shutdown, lifecycle/work-state tracking, and metadata audit.

Remote operation, multi-machine federation, usage billing, and a background service are outside the current product.

## Primary use case

Local work with several CLI agents: visible terminals, explicit teams, and shared messages without automatically sharing private terminal history.

## Security decisions

Working directories are qualified by the backend. The desktop owns lifecycle authority. Pane actions are allowlisted and bound to a live run through native Job membership or the per-run pane secret. Room access is derived from membership.

Normal permissions are the default. PRIM-1 does not supply hostile same-user isolation or an additional OS sandbox.

## Open product decisions

User-extensible driver packaging, richer simultaneous-pane layouts, usage telemetry, and native Linux/macOS support remain future work. Conversation resumption is already implemented; its upstream CLI compatibility remains an ongoing maintenance concern.
