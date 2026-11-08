# PRIM-1

A **Windows** desktop app that runs terminal-first AI agents (Claude Code, Codex, Grok, Prime, or a generic shell) as real sessions you can see, start, stop, and put in a room together.

The app itself runs on your machine. It is not a hosted agent platform and does not run models locally — the CLIs you attach still use their own providers and accounts.

**Status:** Alpha, build from source · **License:** Apache-2.0 · **OS:** Windows 10/11; an experimental macOS port is described in [docs/MACOS.md](docs/MACOS.md). Linux is the next platform priority.

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

Use Claude Code, Codex or Grok sessions to exercise room delivery. Sessions use your existing accounts and provider usage.

1. Open PRIM-1, click **Rooms**, then **New room**. Name it `First room` and click **Create room**. You can create the room before adding any sessions. An enabled room default brief supplies the initial task prompt for new conversations that select it.
2. In the room, click **+** (**Attach a harness**) → **Custom…**. Choose your first harness, give it a label, keep **Normal** permissions, and use **Browse…** to choose a project folder. Click **Create session** to launch it. Repeat for the second harness.
3. Open each session tab and resolve any CLI trust, sign-in, or approval prompts. Wait for the harness to be ready. A selected brief is passed at launch only for a new conversation; an actual resume receives no task prompt. Joining a room or becoming idle never types a brief. **Brief now** is an explicit new feed message with its own wake result.
4. Click the room-name chip to open its feed. Enter the following message, select **Send to all 2 members**, and click **Send**:

    > Each of you: introduce yourself in one sentence, post it to the room feed, then wait. Do not change files.

5. Check each recipient's receipt. **Send** stores the full message in the feed and submits it into each supported recipient's terminal, also while it works. Questions and other readiness guards keep it queued. **Post to feed** only stores the message. The feed opens **Readable**: one card per message, with delivery receipts left out; **Raw**, one toggle away, shows every stored event. If a wake is Held or unconfirmed, inspect that member's terminal and ask it to read the stored message. Written and Fetched receipts do not prove model acceptance or completion.

Send stores the message once, then checks each recipient independently. A held member does not suppress the others; its held wake stays visible on the message. For a supported Grok recipient with a temporary refusal, resolve its prompt or draft and use **Retry wake** on the message in Readable, or on the eligible recipient receipt in Raw; do not repost to all. Retry cannot enable an unsupported harness. Partial or unknown attempts cannot retry; inspect the recipient's terminal and ask it to read the stored message through `room_read`.

Room definitions, membership and the durable shared feed survive app restarts; terminal buffers do not. Switching rooms leaves sessions running. The backend stores an explicit per-room **Resume on restart** choice, default false; room visits and historical running flags do not select it. Supported conversations resume in a new PTY. Codex resume remains held until root identity and conversation freshness are proven; **Start fresh session** preserves prior recovery references. Frontend startup-policy integration and installed-harness acceptance remain tracked in the S5 plan. Closing the app stops its managed processes. Press **F1** for controls and shortcuts, or see [known limitations](docs/KNOWN_ISSUES.md).

### Periodic fallback messages

In the room's right-side panel, open **Fallbacks**. Choose 15, 30 or 60 minutes,
all hosted agents or selected members, and a message. A member can use its own
interval/message and optional brief, or opt out. Save enables the schedule;
Pause stops future unattempted reminders. Members and Fallbacks collapse
independently. Outside agents and stopped/sleeping members are not woken.
PRIM-1 must remain running; this does not replace an application watchdog.

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
