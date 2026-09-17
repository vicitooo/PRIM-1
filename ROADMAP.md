# PRIM-1 — Roadmap

This is product direction, not a release schedule. Current limitations are in [docs/KNOWN_ISSUES.md](docs/KNOWN_ISSUES.md).

## What the project is today

- A Windows desktop app with supervisor-owned PTYs and built-in Claude Code, Codex, Grok, Prime Agent (Ubuntu WSL), and Generic Terminal drivers.
- Persistent, ordered sessions grouped into rooms or an unassigned lobby, with one visible terminal at a time.
- Normal-by-default permission profiles, qualified working directories, and stored conversation references for supported harnesses.
- Explicit room membership, a bounded in-memory feed, feed-only posts, and addressed delivery from the operator or a room member.
- A pane-local control surface; Prime currently supports raw terminal interaction only.

## Development themes

### User-extensible drivers

The current catalog is built in. Supporting additional CLIs should preserve backend-owned executable resolution, typed capabilities, and driver-specific readiness and submission behavior.

### Richer layouts

Inactive terminals retain their buffers, but only one terminal is visible at a time. Simultaneous panes and more flexible arrangements would make larger rooms easier to follow.

### Workspaces and projects

Rooms and the lobby already provide grouping and quick re-entry. The app remembers a workspace preference and each session's directory. A richer project model could make folder ownership and defaults clearer across several rooms.

### Multi-instance addressing

Stable session IDs, repeated same-driver sessions, and duplicate display labels already work. Further work should improve how people distinguish and address larger teams without making labels an authority boundary. Each session currently belongs to at most one room.

### Cross-platform support

The desktop and native drivers are Windows-only. Prime's Ubuntu WSL integration is a specific driver boundary, not a Linux desktop port.

Linux and macOS support need native PTY/process ownership, local IPC, packaging, and complete user-flow verification on each platform.

Linux is the next platform priority. There is no release date yet.

### Terminal control and recovery

Improve compatibility as upstream CLIs change their terminal interfaces, including prompt detection, submission, and conversation resumption. Keep raw terminal input available when automated delivery cannot safely proceed.

### Outage handling

The runtime already tracks lifecycle, work state, and failures, with optional stalled-session restarts. Further work should distinguish provider outages, child-process failures, and transport faults more clearly, with useful guidance and bounded recovery.

## Product priorities

Keep everyday room creation, terminal interaction, delivery, and resumption understandable and reliable. Expand layouts, drivers, and platform support as those needs become concrete.

Remote operation, multi-machine coordination, usage billing, and a background service are outside the current desktop product.
