# Prepare the legacy rooms for Experimental

This bounded, standard-library Python tool prepares catalogs. It never launches
PRIM-1, connects to a room, reads a feed or harness transcript, changes the live
runtime, copies executable/config/lock files, or activates a conversation.
Reversible local preparation may proceed within Victor's mandate. Cross-family
review and the production walk are required before claiming the result complete.

```powershell
python -B scripts/prepare-experimental-runtime.py --source-runtime 'C:\explicit\source-runtime' --target-root 'D:\explicit\new-preparation'
python -B scripts/test_prepare_experimental_runtime.py
```

The target's parent must exist. The target must be absent or empty. Source and
target cannot overlap; symlinks and Windows reparse points (including junctions)
are refused throughout their paths. Source catalogs are bounded at 16 MiB each.
Targets at or below `%LOCALAPPDATA%\io.prim1.runtime`,
`%LOCALAPPDATA%\io.prim1.v2`, `%LOCALAPPDATA%\Programs\PRIM-1`, or
`D:\prim1-v2-victor-walk-20261014` are refused even when empty.
Any source `rooms/*.jsonl` refuses this legacy path: use room export for durable
history. No source log is opened. There is no cleanup or overwrite mode.

Output:

- `source-snapshot/`: exact input bytes of both catalogs, never normalized.
- `runtime/`: transformed catalogs, suitable for existing catalog bootstrap.
- `receipt.json`: published last after its pending file passes fsync; source/output SHA-256, counts, unresolved
  references, added holds, timestamp and tool hash. An interrupted attempt stays
  in place without a successful receipt; inspect it and use a different target.

Both catalogs are read twice and their hashes compared before any destination
write. This detects observed changes; it is not an atomic transaction between
two files in a running app. Membership validation rejects inconsistent pairs.
Windows reads use `CreateFileW` with read, write and delete sharing, so an open
inspection handle does not prevent the live writer's `ReplaceFileW`. That handle
reads its original file if replacement occurs; the second read checks the current
path and rejects a changed pair. The handle is owned and closed by the Python stream.
Root must use a fresh snapshot just before preparing the delivered runtime and
compare its inventory against the current source. This is ordinary local
operator tooling, not protection against a hostile same-user filesystem race.

The transformations are `sessions[*].running_at_shutdown=false`,
`rooms[*].resume_on_restart=false`, `rooms[*].history_restarted=true`, plus the
conversation holds below. All UUIDs, order, labels, membership, briefs, qualified
workspaces and their identities, launch inputs, conversation references, prior
history, and lobby/Prime sessions otherwise retain their JSON values.

Every Claude, Codex and Grok row with a nonblank harness ID, active recovery
`original_id`, or `recovery.reported_codex.thread_id` receives a `room_import_in_use`
recovery hold. Its `original_id` uses the first present reference in that order,
preserving its exact value; the tool never fills in a missing `harness_session_id`.
A reported thread is guarded as a known reference, not promoted to verified identity.
The reason is:

> Copied from the live PRIM-1, which may still be running this conversation. Choose Start fresh session here, or Resume here only after closing it in the live PRIM-1.

Its existing active recovery, including any Codex report, moves intact to
`recovery_history`; earlier history remains intact. An identical import hold is
not archived again. The receipt's `conversation_holds_added` lists these guarded
identities for all three providers. Clearing startup flags alone would not prevent
an operator accidentally activating the same conversation in both installs.
All three references participate in duplicate-owner checks. If they identify
different conversations, the receipt reports `conflicting_conversation_ids` with
all three values and preserves the old recovery and report in history; choosing
the guard's reference does not resolve that conflict.

A Codex row with all three references null/absent/blank and no current hold
gets exactly `RecoveryReference::unresolved_codex(None, true)` from
`crates/supervisor/src/session.rs` at `fce0247`: original ID null, provenance
`legacy_catalog`, reason “The imported Codex session has no verified conversation
ID. No replacement was guessed.” Optional `reported_codex` is omitted as Rust
serialization omits `None`. Without this hold, clearing the running flag would
silently turn an unknown old Codex conversation into a fresh session. When all three
references are absent, an existing recovery remains authoritative. With no current
recovery, the unknown hold is added even when the old running flag is false.
Missing/invalid IDs and duplicate conversation references
are listed as unresolved; blank IDs produce one missing-ID issue rather than an
additional invalid-ID issue. No ID or owner is guessed. A missing ID in another
harness is also unresolved, not evidence that starting fresh is acceptable.

Existing startup creates new empty durable room logs and persists new epochs.
The explicit history-reset marker covers legacy empty/no-floor rooms too;
their unavailable past is never represented as copied history. Do not create
fake feed events. Keep `PRIM1_RUNTIME_DIR` (not `PRIM1_RUNTIME_ROOT`) explicitly pointed to `target/runtime`
on every candidate launch. The script does not change any launcher or `.env`.

Only the root's subsequent reviewed candidate walk can establish compatibility
and actual resume behavior. Validation here checks catalog structure, IDs and
membership; it does not replicate every Rust model/launch-policy check. The
baseline still holds Codex resume/wake, and no cross-install conversation lock
exists. Keep copied sessions stopped until Victor chooses and the corresponding
original is confirmed stopped. Copying metadata is not a completed handoff.

Implementation decisions: bootstrap rather than packages preserves IDs and
supports legacy missing feeds; a separate opaque snapshot preserves evidence;
refusal rather than copying feeds keeps this narrowly catalog-only; existing
holds are archived intact beneath a new cross-install ownership hold; fsynced exclusive writes plus a
last-written receipt expose incomplete preparations without deleting anything.
The fixture tests retain a unique temporary directory, including their junction
fixture. They never read real catalogs or start a harness.
