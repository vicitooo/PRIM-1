# PRIM-1

A **Windows** desktop app that runs terminal-first AI agents (Claude Code, Codex, Grok, Prime, or a generic shell) as real sessions you can see, start, stop, and put in a room together.

The app itself runs on your machine. It is not a hosted agent platform and does not run models locally — the CLIs you attach still use their own providers and accounts.

**Status:** Alpha, build from source · **License:** Apache-2.0 · **OS:** Windows 10/11. Linux is the next platform priority; macOS is also planned. Neither port is in this tree.

## Run

You need Git, Rust **1.95.0** with the MSVC toolchain, MSVC C++ Build Tools, WebView2, Node.js 20+, and **the CLI(s) you actually want to run**, already installed and logged in (`claude`, `codex`, `grok`, and/or `prime-agent`). Prime also needs Ubuntu WSL with a working user `systemd`. The Tauri CLI is installed by `npm ci`.

For Prime Agent, follow the [upstream quickstart](https://github.com/PrimeIntellect-ai/prime-agent/blob/main/packages/coding-agent/docs/quickstart.md) inside Ubuntu WSL. Prime is optional and currently supports raw terminal interaction only in PRIM-1; use Claude Code, Codex, or Grok for room messaging.

```powershell
git clone https://github.com/vicitooo/PRIM-1.git
cd PRIM-1/apps/desktop
npm ci
npm run build
npm run tauri build
```

Build the frontend **before** any `cargo test` / `cargo build`. Use `npm run tauri build` for the exe — a plain `cargo build --release` of the desktop crate is a dev-mode binary that talks to Vite on `localhost:1420` and will look broken.

From the repository root, the executable is `target/release/cli-master-wrapper-desktop.exe`. A fresh install has no sessions. Creating a session launches its CLI.

## Your first room

Use two agent sessions for this walkthrough, for example Claude Code and Codex, or two instances of one installed CLI. They use your existing accounts and provider usage.

1. Open PRIM-1, click **Rooms**, then **New room**. Name it `First room`, leave **Brief members automatically** checked, and click **Create room**. You can create the room before adding any sessions.
2. In the room, click **+** (**Attach a harness**) → **Custom…**. Choose your first harness, give it a label, keep **Normal** permissions, and use **Browse…** to choose a project folder. Click **Create session** to launch it. Repeat for the second harness.
3. Open each session tab and resolve any CLI trust, sign-in, or approval prompts. Wait for the harness to be ready. The automatic room brief explains how to read and post to the shared feed; it is delivered once per membership when the session can accept it.
4. Click the room-name chip to open its feed. Enter the following message, select **Send to all 2 members**, and click **Send**:

    > Each of you: introduce yourself in one sentence, post it to the room feed, then wait. Do not change files.

5. Watch the session tabs for the responses and the room feed for their posts. **Send** puts the message into the selected terminals. **Post to feed** only adds a bulletin for members to read; it does not prompt them.

If a member is still starting or waiting on a question, Send is refused before delivery begins; the error names the member. Resolve its prompt in the terminal and try again. Prime is currently raw-terminal-only, so use Claude Code, Codex, or Grok for this room walkthrough.

Room definitions and membership survive app restarts; the shared feed does not. Switching rooms leaves sessions running. **Settings → Continue where I left off** is on by default and relaunches previously running sessions as you re-enter their room or lobby, resuming stored conversations where supported. Closing the app stops its managed processes. Press **F1** for controls and shortcuts, or see [known limitations](docs/KNOWN_ISSUES.md).

## Docs

| Who | Where |
|---|---|
| Humans | this file, then [docs/KNOWN_ISSUES.md](docs/KNOWN_ISSUES.md) and [ROADMAP.md](ROADMAP.md) |
| Agents | [README-AI.md](README-AI.md) (also [AGENTS.md](AGENTS.md)) |
| Deeper | [ARCHITECTURE.md](ARCHITECTURE.md), [RUNTIME-CONTRACTS.md](RUNTIME-CONTRACTS.md), [CONTROL-SURFACE.md](CONTROL-SURFACE.md) |

## Feedback and contributions

Bug reports, questions, and feature requests are welcome in [Issues](https://github.com/vicitooo/PRIM-1/issues). For a bug, include your PRIM-1 commit, Windows and CLI versions, reproduction steps, and expected versus actual behaviour. Remove tokens, private paths, and conversation content from logs or screenshots before sharing them.

For a larger change, open an issue first to discuss the approach. Small fixes can go directly to a pull request. Report security vulnerabilities privately using [SECURITY.md](SECURITY.md).

## Contributors

- **Victor Valtchev** — runtime, supervisor, drivers
- **Alexander Valtchev** — desktop UI and bugfixes

Martin Tomov did an earlier private macOS experiment; it is not in this tree.

## License

Apache-2.0 — [LICENSE](LICENSE), [NOTICE](NOTICE). AS IS, no warranty.

Shipping a compiled exe also carries **MPL-2.0** file-copyleft obligations for the `cssparser` / `selectors` crates. Source-only GitHub publication does not.

Copyright 2026 Victor Valtchev.
