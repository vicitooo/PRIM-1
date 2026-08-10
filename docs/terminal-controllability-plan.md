# Terminal Controllability Plan

Created: 2026-04-15
Status: Plan only. No implementation code is included in this task.

## Purpose

This document maps the gap between the current wrapper and the target operator model:

- interrupt a running pane intentionally
- restart a pane without losing operator-defined launch settings
- launch arbitrary or curated terminal programs under supervision
- choose flags, cwd, and env per pane
- transfer an existing Claude session into a pane with `claude --resume <session_id>`
- add and remove panes dynamically
- save and restore named workspaces

The goal is not to redesign the wrapper from scratch. The goal is to describe the smallest set of architectural changes that moves the current fixed Claude/Codex MVP toward a real terminal runtime without breaking today's known-good workflow.

## Locked Invariants

These constraints should remain true while this work is designed and later implemented:

- PTY ownership stays in the supervisor.
- `send_input` remains the raw-bytes path.
- `route_message` remains the wrapped sideband-visible path.
- The embedded supervisor inside the Tauri desktop app remains the current runtime model.
- The control plane uses a native named pipe on Windows and Unix socket on supported Unix targets; transport failure is explicit and there is no plaintext disk fallback.
- Today's fixed Claude and Codex panes must continue to work without requiring any operator migration.

## Current State Inventory

### Supervisor lifecycle and session registry

Current runtime state lives in `crates/supervisor/src/lib.rs`.

- `SupervisorHandle::new` creates exactly two slots at startup:
  - `claude` from `crates/driver-claude`
  - `codex` from `crates/driver-codex`
- No public API exists to add, remove, or mutate session definitions at runtime.
- The session registry is therefore compile-time seeded, not operator-defined.

### Session lifecycle controls that exist today

The supervisor already exposes these lifecycle actions:

- `start_session`
  - idempotent if the session is already running
  - builds a launch spec from the session definition
  - spawns a fresh PTY child
- `stop_session`
  - marks the slot closed
  - kills the PTY child
  - does not preserve process state
- `restart_session`
  - emits `Restarting`
  - calls `stop_session`
  - then calls `start_session`
  - always produces a fresh process with a fresh PTY

What is not preserved today:

- process memory
- interactive shell history inside the child
- TUI state
- launch overrides made after app startup
- pane-specific cwd overrides
- pane-specific env overrides
- pane-specific custom flags

### Send paths that exist today

Three distinct operator-to-pane paths already exist:

- `send_input`
  - raw bytes to PTY stdin
  - this is the PRIM-1 load-bearing path
- `send_control_key`
  - implemented inside the supervisor
  - maps keys to byte sequences:
    - `enter` -> `\r`
    - `up` -> `\x1b[A`
    - `down` -> `\x1b[B`
    - `left` -> `\x1b[D`
    - `right` -> `\x1b[C`
    - `tab` -> `\t`
    - `esc` -> `\x1b`
    - `ctrl_c` -> `\x03`
- `route_message`
  - wraps the content with visible sender/scope metadata
  - driver-specific submit behavior is applied after injection

Important limitation:

- `send_control_key` exists in the supervisor and the PowerShell control-plane helper, but it is not exposed as a first-class desktop UI control yet.

### PTY host behavior

`crates/pty-host/src/lib.rs` currently does the following:

- opens a PTY at 120 columns x 40 rows
- spawns the child process with the resolved launch spec
- writes stdin bytes directly
- reads stdout/stderr-like PTY output in a background thread
- forwards output chunks to the supervisor
- supports resize
- supports kill
- can poll child exit with `try_wait`

Important limitations:

- there is no replay buffer at the PTY layer
- there is no preserved scrollback after a restart
- there is no session checkpointing
- output is decoded with `String::from_utf8_lossy`, which is pragmatic but not a byte-perfect transcript model

### Launch spec shape

The shared launch contract already exists in `crates/shared-types/src/lib.rs`:

- `LaunchSpec`
  - `program`
  - `args`
  - `working_dir`
  - `env`
  - `display_name`
- `SessionDefinition`
  - `name`
  - `title`
  - `driver`
  - `working_dir`
  - `command`
  - `args`
  - `env`
  - `auto_start`

This is a good foundation. The missing piece is not the type shape. The missing piece is operator control over these fields at runtime.

### Driver behavior today

Current drivers:

- `driver-claude`
  - default program: `claude`
  - default flags include:
    - `-n <session_name>`
    - `--dangerously-skip-permissions`
    - `--add-dir <working_dir>`
    - `--add-dir <wrapper_root>`
  - appends any extra `SessionDefinition.args`
- `driver-codex`
  - Windows path uses `cmd.exe /d /c codex.cmd`
  - default flags include:
    - `--yolo`
    - `--no-alt-screen`
    - `-C <working_dir>`
  - appends any extra `SessionDefinition.args`
- `driver-generic-terminal`
  - exists already
  - defaults to `powershell -NoLogo` on Windows or `bash -l` on Unix
  - honors `definition.command`, `definition.args`, `definition.env`, and `definition.working_dir`

Important limitation:

- the generic driver exists at the code level but is not wired into a dynamic pane/session creation flow.

### Desktop UI state today

`apps/desktop/src/main.ts` is still hardcoded around two panes:

- `SESSION_NAMES = ["claude", "codex"]`
- pane HTML is inlined for those two sessions only
- launch/restart/stop buttons exist only for those panes
- router dropdowns are fixed to `operator`, `claude`, `codex`, and `room`

This means the current UI is a fixed two-pane dashboard, not a general session workspace.

### Permission model today

There are two different concepts that should not be confused:

- `working_root`
  - used to seed the default session working directory
  - not a general security boundary
  - not re-checked on every action
- pane-bound control-plane credentials
  - enforced by `validate_session_action_token`
  - reject peer slash-command injection without a global bypass
  - master control-plane credentials still bypass pane binding

Conclusion:

- the wrapper currently has a bearer-based compatibility policy, not a hostile same-user authority boundary
- it does not yet have a strong launch-policy model for arbitrary binaries

## Gap Analysis

### A. Ctrl+C to a running pane

Current state:

- `send_control_key("ctrl_c")` sends literal `\x03`
- the control-plane script supports `-Action key -Key ctrl_c`
- the desktop UI does not expose an interrupt button or interrupt shortcut path

What is missing:

- explicit operator-facing interrupt control
- clear semantics for when copy should win versus when interrupt should win
- empirical proof that `\x03` behaves acceptably for Claude and Codex during long-running tool calls

Minimal fix direction:

- add a per-pane `Interrupt` button in the desktop UI
- expose a desktop command for `send_key`
- keep `Ctrl+Shift+C` reserved for copy
- do not overload copy and interrupt onto the same shortcut
- add a later optional active-pane shortcut only after the explicit button path is proven stable

Scope: hours for the basic UI/control-plane surfacing, days if shortcut policy and edge-case handling are included.

### B. Restart with state preservation

Current state:

- restart is a hard stop plus a fresh start
- no launch overrides are preserved beyond the static session definition

What is missing:

- persistent "effective launch profile" per pane
- any distinction between:
  - restart the same executable with the same config
  - resume the previous application-level session

Minimal fix direction:

- introduce a runtime `SessionLaunchProfile` derived from `SessionDefinition`
- store the last effective launch config in the supervisor slot
- make restart use the stored launch profile, not just the original compile-time defaults
- treat process continuity and app-level continuity separately
- driver-specific resume strategies should be opt-in, not assumed

Scope: days.

### C. Operator-chosen launch binary

Current state:

- `driver-generic-terminal` already knows how to spawn arbitrary commands from a `SessionDefinition`
- there is no control-plane API to create a new session definition dynamically
- there is no UI for choosing a binary

What is missing:

- session creation API
- validation policy
- runtime registry updates
- UI for new-pane creation

Minimal fix direction:

- add a `create_session` control-plane command that accepts a sanitized session definition or launch profile
- support two modes:
  - curated launch templates in v1
  - arbitrary binary mode later if the operator wants full raw power
- use `driver-generic-terminal` as the initial implementation target

Scope: days for curated mode, weeks for fully general arbitrary-binary support with safe UX and validation.

### D. Operator-chosen flags per launch

Current state:

- `SessionDefinition.args` already exists
- drivers append extra args after their defaults
- the operator cannot edit args from the desktop UI

What is missing:

- editable launch profile UI
- a policy for append versus override
- persistence of operator-selected flags across restart

Minimal fix direction:

- define per-driver launch settings:
  - default args
  - extra args
  - overridden args when explicitly allowed
- start with "append-only" in v1
- make full override an advanced mode for curated drivers only after validation

Scope: hours for append-only on fixed panes, days if combined with a generic session editor.

### E. Operator-chosen working directory per pane

Current state:

- `working_dir` already exists in both `SessionDefinition` and `LaunchSpec`
- the app currently seeds both panes from one resolved root and keeps that fixed

What is missing:

- UI/editor control for per-pane cwd
- persistence across restart and workspace restore

Minimal fix direction:

- make cwd part of the launch profile editor
- require explicit path existence validation before spawn
- keep current defaults for Claude and Codex when no override is set

Scope: hours once launch profiles exist.

### F. Operator-chosen environment variables per pane

Current state:

- `env` already exists in `SessionDefinition` and `LaunchSpec`
- the supervisor already injects pane credentials during spawn
- no UI or persistence exists for operator-provided env vars

What is missing:

- env editor UI
- secure handling for secrets versus visible plain values
- persistence rules

Minimal fix direction:

- start with plain visible env pairs for local power-user workflows
- store them in the session launch profile
- defer secret masking or OS-keychain integration to a later iteration

Scope: hours for plain env support once launch profile editing exists, days if secret handling is required.

### G. Session transfer via `claude --resume <session_id>`

Current state:

- technically possible in principle because the Claude driver already supports extra args
- not usable as a product flow because:
  - there is no UI for entering or selecting a session id
  - restart does not preserve a resume configuration
  - there is no integration with Claude session discovery

What is missing:

- a first-class resume launch mode
- a source of candidate session ids
- UX for manual and assisted selection

Minimal fix direction:

- make "resume Claude session" a dedicated launch preset, not just raw extra args
- support two levels:
  - minimal: operator pastes a session id manually
  - enriched: wrapper queries `<workspace-root>/tools/cra.py` to list resumable Claude sessions
- store the selected session id inside the pane's launch profile so restart reuses it

Important caution:

- the current Claude driver injects default args including `-n <session_name>`
- the resume flow needs an explicit driver policy for how `-n` interacts with `--resume`
- this should be tested empirically before implementation assumptions are locked

Scope: days for manual resume mode, days to a week for integrated listing and polished UX.

### H. Dynamically add and remove panes at runtime

Current state:

- supervisor slots are seeded at startup only
- the frontend hardcodes two panes and fixed router participants

What is missing:

- dynamic session registry in the supervisor
- dynamic pane rendering in the frontend
- stable identifiers for panes beyond `claude` and `codex`
- removal semantics for a running pane

Minimal fix direction:

- introduce runtime-created session ids
- treat pane layout as data, not hardcoded DOM
- render panes from the runtime snapshot instead of `SESSION_NAMES`
- add `create_session` and `remove_session` control-plane commands
- require stop-before-remove in the first iteration to keep semantics simple

Scope: days for supervisor support plus basic dynamic rendering, longer if freeform layout management is included.

### I. Save and restore workspaces

Current state:

- no persisted workspace format exists
- runtime state is ephemeral apart from logs and control-plane files

What is missing:

- workspace schema
- save/load commands
- versioning strategy
- UI for selecting, naming, and restoring workspaces

Minimal fix direction:

- define a simple JSON workspace format containing:
  - workspace name
  - pane order
  - pane launch profiles
  - route participants derived from pane ids
- store under `.runtime/workspaces/<name>.json` first
- add a `schema_version` field from day one
- keep layout simple in v1: grid order, not freeform coordinates

Scope: days for save/load without migrations, weeks if a durable product-grade workspace system with migration and cross-version guarantees is required.

## Proposed Architecture Changes

### 1. Introduce a first-class launch profile model

Primary crates:

- `crates/shared-types`
- `crates/supervisor`
- `apps/desktop/src-tauri`
- `apps/desktop/src`

Minimal data model addition:

- `SessionLaunchProfile`
  - session id
  - title
  - driver kind
  - program override
  - args
  - working_dir
  - env
  - launch preset metadata
  - optional resume metadata

Why this is the key move:

- almost every missing operator feature depends on a mutable, persistable launch profile

### 2. Split fixed defaults from operator overrides

The drivers should continue to own safe defaults, but the operator layer should own overrides.

Recommended rule:

- driver default spec
- plus launch profile overrides
- plus supervisor-injected pane credentials

That preserves today's behavior while opening a safe path to richer launch control.

### 3. Add runtime session registry commands

Needed commands:

- `create_session`
- `remove_session`
- `update_session_profile`
- `send_key` desktop exposure

Backward compatibility:

- the existing `claude` and `codex` sessions still exist on boot
- fixed-pane startup can coexist with future dynamic panes

### 4. Make the frontend render panes from session data

Primary frontend change:

- replace hardcoded `SESSION_NAMES` rendering with a snapshot-driven pane model

This is the foundation for:

- dynamic panes
- workspace restore
- generic terminals
- pane-specific controls

### 5. Treat resume as a driver capability, not a generic assumption

Some resume behaviors are driver-specific.

Recommended shape:

- generic launch profile carries optional resume metadata
- each driver decides how to map that metadata into actual CLI flags
- unsupported drivers simply ignore resume metadata

This keeps `claude --resume` first-class without pretending all CLIs work the same way.

## Recommended Rollout Order

### Iteration 1: Controlled launch profiles on the existing two panes

Ship first:

- desktop exposure of `send_key`
- explicit `Interrupt` button
- launch profile persistence for args, cwd, and env on Claude/Codex
- restart uses the last effective launch profile

Why first:

- high value
- low architectural risk
- improves the current wrapper immediately without requiring dynamic panes

### Iteration 2: Claude resume flow

Ship next:

- manual `--resume <session_id>` mode
- stored resume metadata in launch profiles
- restart semantics for resumed Claude panes

Why second:

- this is the operator's highest-value controllability case
- it depends on launch profile persistence but not yet on dynamic panes

### Iteration 3: Dynamic session registry plus generic terminal pane

Ship next:

- create/remove session commands
- snapshot-driven pane rendering
- generic terminal launch path

Why third:

- this is the real transition from fixed dashboard to terminal runtime
- it is broader and riskier than launch-profile work

### Iteration 4: Workspace save and restore

Ship after dynamic panes:

- save workspace
- restore workspace
- load a named saved operator layout

Why fourth:

- workspaces become much more valuable once panes are dynamic

## Rough Scope Summary

| Area | Rough scope |
|---|---|
| UI interrupt button plus desktop `send_key` exposure | hours |
| Launch profile persistence on fixed panes | days |
| Per-pane args/cwd/env editing | days |
| Manual Claude resume mode | days |
| `cra.py`-assisted Claude session picking | days to a week |
| Dynamic pane registry and rendering | days to weeks |
| Generic arbitrary-binary support | weeks if fully general |
| Workspace save/restore | days for simple JSON, weeks for product-grade durability |

## Open Questions

- Should v1 controllability support only curated binaries, or truly arbitrary binaries?
- Should full argument override be allowed, or only additive flags on curated drivers?
- Is plain-text env persistence acceptable for local operator use, or do secrets need separate handling immediately?
- Should `cra.py` integration be first-class in the first resume iteration, or should manual session-id entry ship first?
- Should pane removal hard-stop a running process automatically, or should stop-before-remove remain mandatory?
- Is workspace layout in v1 just pane order and launch profiles, or does the operator want saved visual geometry immediately?

## Recommendation

The right next move is not "generic terminal everything" in one jump. The right next move is:

1. launch-profile persistence on the existing Claude/Codex panes
2. explicit interrupt control
3. Claude `--resume` as the first-class transfer use case
4. dynamic pane registry after that foundation is stable

That sequence preserves today's working wrapper, lands operator value early, and avoids turning the MVP into a large uncontrolled refactor.
