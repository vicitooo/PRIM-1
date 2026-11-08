# PRIM-1 on macOS

Status: experimental port of the `v3-communication` line, Apple Silicon only so far. The rest of the repo still describes the Windows app; this file is where the macOS differences live.

## Build

Requires Xcode Command Line Tools, Rust 1.95.0 (rustup), Node 20+, and the CLIs you want to run (`claude`, `codex`), already logged in.

```sh
cd apps/desktop
npm ci
npm run build
npm run tauri build        # CI=true to skip Finder scripting in the DMG step
```

The bundle is at `target/release/bundle/macos/PRIM-1 v2.app` (and `target/release/bundle/dmg/`). There is no signing or notarization: the linker ad-hoc signature is all it has, so it runs on the machine that built it. Copying it elsewhere needs Gatekeeper approval.

Build the frontend before any `cargo` command, as on Windows. Tests: `npm run test` in `apps/desktop`, `cargo test --workspace` at the root.

## What works

- **Terminals.** Claude Code, Codex, Grok and a plain shell run in real PTYs through portable-pty's Unix backend (`TERM=xterm-256color`, UTF-8 locale set, other terminals' identity variables removed).
- **Process ownership.** See [Process scopes](#process-scopes). Stop, restart and app close terminate the whole tree and prove it before reporting success. If the app is killed outright (`kill -9`, a crash), a small helper (`--prim1-orphan-guard`) terminates whatever was still running.
- **Pane sideband.** A Unix socket in a fresh `0700` directory inside the per-user temp folder, socket mode `0600`. The client verifies the server's PID, start time and user id; the server identifies the client with the kernel's peer credentials and its process scope, then the existing per-run pane secret. Same protocol and same checks as the Windows named pipe.
- **Rooms and feed**, the pane MCP server (`--prim1-pane-mcp`), the room CLI (`--prim1-room`), the guest and operator entries (`--prim1-guest`, `--prim1-operator`).
- **Harness discovery.** No `.exe` and no `.cmd` shims. A Finder launch only has `/usr/bin:/bin:/usr/sbin:/sbin` on PATH, so at startup the app puts the usual CLI folders first (`~/.local/bin`, `~/.local/node/bin`, `~/.npm-global/bin`, `~/.bun/bin`, `~/.cargo/bin`, `/opt/homebrew/bin`, `/usr/local/bin`, then `/etc/paths*`). npm script installs (like `codex`) run as `node <script>`. Native Claude installs (`…/claude/versions/<version>`) are recognized by path.
- **Conversation following.** Claude's per-process session file (`pidDomain: "darwin"`, `procStart` in `lstart` form), Codex writer locks through the kernel's open-file list, Grok's logs.
- **Telemetry.** PRIM-1's own process and each run's tree (CPU, physical footprint, peaks). PRIM-1's WebKit webview is not counted: its processes belong to launchd and can't be attributed without a private API. The UI says so instead of showing zero.
- **Paths.** Runtime and webview data live under `~/Library/Application Support/io.prim1.v2/`. The default workspace is the folder the app was started in; a Finder launch starts in `/`, so it uses `~/PRIM-1`, which it creates.

## Disabled on macOS

These fail with a clear message or are absent; none is faked.

- **Prime / WSL**: needs Windows.
- **Self-upgrade** (`upgrade_request`, the stable launcher, the helper task): built on Windows Task Scheduler and PowerShell.
- **Closed-runtime checkpoints** (`--prim1-operator` maintenance capture): need Windows file ownership semantics.
- **The Codex App Server transport** (`codex_transport = app-server`): opt-in, Windows only; Codex members use the terminal transport.
- **The PowerShell helper scripts** under `scripts/`; the `--prim1-room` CLI covers the same pane actions.

## Process scopes

macOS has no job object, so a run's tree is derived from evidence, in this order:

1. its session and process group (the PTY child is a session leader);
2. parentage (anything whose parent chain reaches a member);
3. a per-run environment marker (`PRIM1_PROCESS_SCOPE`), read back from the kernel, which finds descendants that left the session and were reparented to launchd;
4. members seen earlier, by pid and start time.

Termination sends SIGTERM, waits briefly, then SIGKILL, and only succeeds after a pass finds nobody left. If that fails, the run is held as uncertain, as on Windows.

**This is weaker than a Job.** A process that both daemonizes and scrubs its environment before PRIM-1 looks can escape. PRIM-1 was never a sandbox for hostile programs running as the same user (see [SECURITY.md](../SECURITY.md)); on macOS, keep that in mind for anything an agent starts.

## Verified on this machine

Apple Silicon, macOS 26.6: the `.app` started through LaunchServices and from a shell with a bare `PATH`; rooms, sessions and live terminals; Claude Code (2.1.319) receiving a room message and replying through the room tools; Codex (0.158.0) launching and reporting its folder-trust question; Stop, Restart, app close and `kill -9`/SIGTERM leaving no process behind. A Codex reply to a room message has not been exercised yet, because its first-run folder-trust question was left for the user to answer.

## Known gaps

- `PATH` is not read from your login shell, so a CLI installed somewhere only your shell profile knows (nvm, pyenv shims) won't be found. Set it in `PRIM1_ENV_FILE` or start the app from a terminal.
- SIGTERM (`kill <pid>`) is not a graceful close: the crash guard still clears the agents, but the app does not get to save anything first. Closing the window or Quit is graceful.
- First use of a protected folder (Documents, Desktop, Downloads) from a Finder-launched app is expected to show macOS's privacy prompt; approve it once. (Testing so far started the app from a terminal, which uses that terminal's access instead.)
- Claude and Codex show their own folder-trust prompt the first time they run in a new folder; answer it in the session's terminal.
- Windows-only tests are compiled out: the Prime paths, the Windows-path fixtures, and the upgrade launcher tests.
- Keybindings for image paste and the like were verified on Windows only (`apps/desktop/src/harness-keys.ts`).
- Not run on Intel Macs.
