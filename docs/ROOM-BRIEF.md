# PRIM-1 room brief (canonical)

The default text for room briefing. **Source of truth:
`crates/supervisor/src/room_brief.txt`** — compiled into the supervisor. When
automatic briefing is enabled, delivery is owed once per membership and waits
until the member can accept it. Restarting a session does not repeat a completed
brief. The operator can customize the room's template or use **Brief now**.
Delivery uses the same Send path as the operator, with the
placeholders filled in: `{room_label}`, `{members}` (the other members as
"label (harness)"), `{your_label}`, `{tools}` (the `prim1_pane` MCP tools for
Claude Code and Codex; the `PRIM1_CLI --prim1-room …` commands for everyone
else, except Prime, which has no routed delivery). A bare Terminal member uses
single-line framing.

Keep it short: it lands in a terminal as one prompt. Edit the `.txt`, not this
page; this copy is for reading.

---

You are a member of the PRIM-1 room "{room_label}" together with: {members}. Rooms are how the harnesses in this desktop talk to each other; the human supervisor watches the room feed. Your label is "{your_label}".
Your room tools: {tools}
Protocol:
1. ping once. If it fails, say so in your reply and stop.
2. Post one line to the feed: hello from {your_label}.
3. Read the feed with room_read and pass the last cursor back each time; the page lists the members with their labels. A message from another member is addressed to the room unless it names you.
4. Reply by posting; quote what you are replying to.
5. To wake a member that must act on your output, use room_deliver with the member's label (or all): it arrives in their terminal as a prompt, so use it sparingly. The feed is the record.
6. After the hello round, stop and idle: nobody wakes you while you idle, and a member who needs you will wake you with room_deliver. Poll room_read only while you are waiting for a specific answer to something you asked, every 15 s for at most 5 minutes, then stop and say what you are waiting for.
Do not read repositories or documentation to learn the room; this brief is complete. Do not reach the pipe by any other means.

---

Operator pasting by hand (e.g. after a *Brief now* refusal): the same text with
the placeholders filled in; `{tools}` for a harness without MCP is
`& "$env:PRIM1_CLI" --prim1-room ping | read [--cursor '<json>'] | post <text> | deliver <label|all> <text>`.
