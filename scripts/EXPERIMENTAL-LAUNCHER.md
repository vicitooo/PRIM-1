# Launch the separate Experimental build

`launch-experimental.ps1` stages one SHA-256-pinned executable and starts it using
a dedicated interactive, unelevated, per-user scheduled task. It never stops or
replaces the installed PRIM-1, removes files/tasks/shortcuts, or starts an AI
harness itself. Root reviews source before bounded local QA; Claude checks the
outcome before delivery, satisfying the cross-family review before completion.

Supply the production artifact and its independently recorded build hash:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\launch-experimental.ps1 -SourceExe 'D:\explicit\production.exe' -Sha256 '<64 hexadecimal characters>' -Root 'D:\prim1-v2-experimental-20261019' -Check
# After review, repeat without -Check to stage/register/request start.
# Add -CreateShortcut explicitly to create the separately named desktop shortcut.
# Use -PrepareOnly instead of -Check to stage/register without requesting start.
# -PrepareOnly -CreateShortcut also creates the shortcut without opening the app.
```

`-Check` reads paths, hashes, persistent environment, task configuration and
process metadata only. It creates nothing and takes precedence when combined with
`-PrepareOnly` or `-CreateShortcut`. `-PrepareOnly` performs the same immutable
staging, task creation/reuse and optional shortcut creation as the normal invocation,
then returns before `Start-ScheduledTask`. It makes no start request and leaves an
already-running app alone; it does not certify that the app is stopped. Without
either switch, the existing stage/register/start behavior is unchanged.
The normal invocation can reuse a
root/runtime prepared by `prepare-experimental-runtime.py`; it does not copy or
reinterpret catalogs. A mismatching existing executable or `.env` refuses rather
than overwrites. Interrupted staging is retained for inspection. Use a new QA
root for a different build; replacing the dated shipping build is deliberately
outside this script.

The only allowed roots are `D:\prim1-v2-experimental-20261019`, MAINPC's clean
main root `D:\prim1-v2-20261021` (Victor, room 3606, 21 Oct 2026), and
`D:\prim1-qa-20261019-<letters-digits-hyphens>`. The root contains `bin/`,
`runtime/`, and an independent `profile/`. Agent working directory defaults to
`D:\All Repos\victor` and must already exist. A QA `root/work` directory is allowed;
it must remain disjoint from `runtime`, `bin` and `profile`. The new window starts visible.
No automatic session start is added; copied sessions still require the migration
and conversation-ownership review described in EXPERIMENTAL-MIGRATION.md.

QA alone can specify `-DebugCdpPort 9347` (1024..65535, excluding 9222).
The application's existing CDP implementation binds loopback. Shipping writes no
CDP setting. A listening QA port is refused before a new start request.

The task name is `PRIM1-` plus the root's basename. An existing task must match the
root/build marker, direct executable, working directory, current user's SID from
the exported task XML, interactive limited principal, no triggers, unlimited
runtime, and IgnoreNew policy. The XML SID avoids a false refusal when the task
API abbreviates the account to its local name. The task is reused
without re-registration. An already-running staged executable or task receives no
second start request. Task Scheduler launches the executable directly, outside the
calling PRIM-1 job. A successful request is not a claim that app startup succeeded.

The optional shipping shortcut is named exactly `PRIM-1 Experimental`, or `PRIM-1`
for the main root `D:\prim1-v2-20261021` (move any existing `PRIM-1.lnk` aside
first: the script never changes an existing shortcut); QA shortcuts
retain their distinct dated root names. It invokes that task through an immutable `bin/launch.vbs`
using `wscript.exe //B //NoLogo`. The helper runs `schtasks /Run` with window style 0,
so there is no console flash; the Experimental app itself opens visibly. Existing
shortcuts are never modified; repeat without `-CreateShortcut` after creation.
The shortcut does not repeat the launcher's hash checks: do not manually replace
the staged executable or edit its task/configuration.
Creating that shortcut with `-PrepareOnly` does not invoke it. After preparation,
the operator may use the shortcut or repeat the launcher without `-PrepareOnly`
and, if already created, without `-CreateShortcut`.

Decision audit: fixed dated roots prevent accidental targeting; exact hashes and
exclusive new-file writes prevent ordinary overwrite; ancestor reparse refusal
and known live-path exclusions protect isolation. These checks are operator
safeguards, not protection against a hostile same-user filesystem race. They do
not inventory every descendant of a supplied runtime, prove existing catalog
compatibility, or prove every copied conversation is safe to resume.
Preparation is a delivery operation, not an additional approval gate: the separate
switch permits file/task/shortcut preparation while desktop interaction is stopped.

Startup loads `.env` before WebView creation, but dotenv preserves inherited
values. The launcher therefore refuses **all** persistent user/machine `PRIM1_*`
and `WEBVIEW2_*` variables, including `PRIM1_ENABLE_PRIME`, rather than changing OS
settings. Task Scheduler does not inherit the calling pane's process environment;
its cached environment cannot be certified here. Before accepting a real launch,
verify the effective runtime, private profile, visible window, absence of shipping
CDP, and Scheduler ancestry. A fresh profile does not carry the old localStorage
settings/keymap; migration of those preferences is not claimed.

Validation for this change is source-only: PowerShell parser, successful read-only
check, and refusal cases for a foreign root/task, hash mismatch, shipping/live-port
CDP and an existing reparse ancestor. No artifact was staged, task registered, app started, or shortcut created
while developing it. The implementation commit is supplied in the review handoff.

Follow-up verification: read-only `-Check` accepts the existing
`PRIM1-prim1-qa-20261019-a106` task with pinned SHA-256
`53CF253FF55654BE2CC19BC2E2D7B85D438BCA9ED0B2192692ED51C7B4B7E700`,
using its already staged executable as the read-only hash source. An in-memory
export fixture with a foreign SID is refused. Direct helper fixtures refuse
`PRIM1_ENABLE_PRIME`, unknown future prefixed settings, and WebView2 overrides,
including lowercase names, while unrelated environment names remain allowed.

`-PrepareOnly` validation used the Windows PowerShell 5.1 parser and six extracted
control-flow cases with an in-memory staging marker and mocked start command:
Check alone/combined, preparation with/without an observed running app, and the
unchanged normal-start/already-running paths. The launcher itself was not invoked;
no staging, scheduled-task action, shortcut creation or desktop interaction ran.
