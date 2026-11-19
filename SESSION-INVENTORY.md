# Local conversation discovery

Settings → **Local conversations** scans Claude, Codex and Grok metadata for the
effective personal repository (environment override, Settings, then workspace).
The scan never launches a harness, changes a provider store, binds a conversation
or clears a hold. Its result names the inspected folder. Disabled or invalid
configuration does not silently fall back to a different folder.

The separate operator action **Attach for recovery** binds an exact root to a
fully stopped, same-driver member in that folder. Every binding remains held;
nothing starts. Close the conversation in every other app or terminal before
explicitly choosing **Resume here**. Discovery does not prove outside ownership
is free. Neither discovery nor Attach is a pane MCP tool.

The scan runs on a blocking worker, not the UI thread, with one scan admitted at a
time. Each provider has a 5,000-entry, 2,000-candidate, depth-four and cooperative
ten-second limit. Ordinary metadata reads are capped at 64 KiB. Claude's identity
search streams at most 1 MiB or 16 records, retaining at most 64 KiB per record;
oversized records are skipped, not buffered whole. Its optional title-only tail
read is separately capped at 64 KiB. Filesystem calls
can exceed the cooperative deadline; this is not a hard I/O timeout. Results are
partial whenever limits or errors intervene; an omitted conversation is not proof
that it does not exist. No background polling is added.

## Evidence and limits

- Codex: first rollout `session_meta` record, with optional metadata name index.
  Plain and known `<primary UUID>_<secondary UUID>` rollout names must match the
  header's primary UUID. Bounded directory batches prefer newer date/name entries;
  reaching a limit can still omit any entries outside the admitted batch.
  Archived, suffixed or non-CLI roots remain visible but cannot attach, with a
  reason: the existing native resume verifier does not support those forms.
- Claude: exact `cwd`/`projectPath` from session metadata, an index or the first
  complete bounded JSONL record containing both `sessionId` and workspace. A
  preceding title is carried only for the same session ID. The bounded tail
  supplies the last matching custom-title, else ai-title, for display only;
  tail records never establish identity or ownership.
  Historical files without this evidence are omitted visibly;
  ambiguous encoded folder names are not treated as exact workspace identities.
  No Claude folder-name prefilter is used.
- Grok: direct workspace/UUID directory and `summary.json`; missing summaries
  yield an explicitly unknown type rather than an inferred root conversation.
  Reversible encoded workspace folders are filtered before inspecting children.
- Subagent/unknown/conflicting evidence remains labeled. Different workspaces for
  the same provider ID in observed metadata are conflicts; the bounded, filtered
  scan does not establish store-wide uniqueness. Title changes do not invalidate
  identity. Codex's name index uses its last recorded name; Claude's bounded title
  metadata can replace an old displayed title. Timestamps never establish ownership.
- Paths and provider labels are rendered as text, not HTML. Symlink/reparse entries
  are skipped. Codex parses only its first JSONL record; Claude parses complete
  records in the bounded prefix until exact identity/workspace metadata appears.
  Those records and buffered bytes can contain message fields: discovery does not
  search or return those fields, credentials or conversation text. It does not
  continue searching for identity after finding the metadata; Claude's separate
  bounded tail pass reads title metadata only.

Attach re-reads metadata and target state under the existing admission boundary.
It saves the exact ID, import hold and prior references durably, then increments
the member generation so a Start prepared against the old binding cannot commit.
Failed writes or exhausted generation leave the existing member unchanged. The
UI filters destination folders identically to native admission and rechecks after
confirmation; native checks remain authoritative.

## Decision audit / verification

Read-only inspection is deliberately separate from attach/resume: a plausible name
is not sufficient evidence to bind an existing PRIM member. SQLite indexing and
automatic ownership guesses were rejected to avoid dependencies and false joins.
Existing Settings/details styling keeps the feature out of the terminal workspace.
The discovery correction expands the finite provider budget after real-PC scans
truncated legitimate results. It retains the existing worker boundary, entry/read
caps and link refusal. No new index, background service or provider writes are added.

Current correction results and decision choices are recorded in
the discovery review (a development note that is not part of this repository). Release fixtures
cover metadata bounds, mutable titles, Codex forms, target races, persistent holds
and write failure. They do not establish full real-store coverage.

Source checkpoint only until the production desktop IPC, partial/error states and
dark/light rendering are walked. The operator stopped desktop control on 19 Oct;
unit/build results do not replace that pending walk or claim full session recovery.
