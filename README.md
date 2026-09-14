# PRIM-1

A **Windows** desktop app that runs terminal-first AI agents (Claude Code, Codex, Grok, Prime, or a generic shell) as real sessions you can see, start, stop, and put in a room together.

Local only. Not a cloud. Not a hosted agent platform.

**License:** Apache-2.0 · **OS:** Windows 10/11 · macOS/Linux are planned, not in this tree.

## Run

You need Rust **1.95.0**, MSVC C++ Build Tools, WebView2, Node.js 20+, and the Tauri 2 CLI. Then:

```bash
cd apps/desktop
npm ci
npm run build
cd ../..
cd apps/desktop
npm run tauri build
```

Build the frontend **before** any `cargo test` / `cargo build`. Use `npm run tauri build` for the exe — a plain `cargo build --release` of the desktop crate is a dev-mode binary that talks to Vite on `localhost:1420` and will look broken.

The exe lands at `target/release/cli-master-wrapper-desktop.exe`. On first launch: system log + **New session**. Nothing starts until you press Start.

## Docs

| Who | Where |
|---|---|
| Humans | this file, then [docs/KNOWN_ISSUES.md](docs/KNOWN_ISSUES.md) and [ROADMAP.md](ROADMAP.md) |
| Agents | [AGENTS.md](AGENTS.md) — contracts, sideband, rooms, env, security |
| Deeper | [ARCHITECTURE.md](ARCHITECTURE.md), [RUNTIME-CONTRACTS.md](RUNTIME-CONTRACTS.md), [CONTROL-SURFACE.md](CONTROL-SURFACE.md) |

## Contributors

- **Victor Valtchev** — runtime, supervisor, drivers
- **Alexander Valtchev** — desktop UI and bugfixes

Martin Tomov did an earlier private macOS experiment; it is not in this tree.

## License

Apache-2.0 — [LICENSE](LICENSE), [NOTICE](NOTICE). AS IS, no warranty.

Shipping a compiled exe also carries **MPL-2.0** file-copyleft obligations for the `cssparser` / `selectors` crates. Source-only GitHub publication does not.

Copyright 2026 Victor Valtchev.
