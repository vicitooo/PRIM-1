import { describe, expect, it } from "vitest";

import {
  READABLE_COLLAPSE_ABOVE_LINES,
  READABLE_LINE_CHARS,
  READABLE_PREVIEW_LINES,
  READABLE_REASON_CHARS,
  parseRoomFeedView,
  readablePreview,
  readableProblemText,
  readableRecipients,
  readableRoomFeed,
  readableSender,
  readableStatesText,
  readableTime,
  type ReadableFeedItem,
  type ReadableProblem,
} from "./readable-feed";
import { retryableRoomDeliveryEvents, roomFallbackSenderLabel, roomRecoverySenderLabel } from "./room-feed";
import { restartNotice } from "./work-context";
import type { RoomDeliveryStatus, RoomFeedEvent, RoomFeedItem } from "./types";

const labels: Record<string, string> = { a: "Claude PRIM-1", b: "Codex PRIM-1", c: "Grok PRIM-1" };
const labelFor = (sessionId: string) => labels[sessionId] ?? `Member ${sessionId}`;

let sequence = 0;
function feedEvent(item: RoomFeedItem, timestamp = "2026-10-14T09:05:00Z"): RoomFeedEvent {
  sequence += 1;
  return { schema_version: 1, room_id: "room-a", cursor: { epoch: "epoch-a", sequence }, item, timestamp };
}

function message(
  messageId: string,
  overrides: Partial<Extract<RoomFeedItem, { kind: "message" }>> = {},
): RoomFeedEvent {
  return feedEvent({
    kind: "message",
    message_id: messageId,
    request_id: `request-${messageId}`,
    sender: { kind: "operator" },
    content: `body of ${messageId}`,
    recipient_ids: ["a", "b"],
    membership_revision: 1,
    ...overrides,
  });
}

function receipt(
  messageId: string,
  recipientId: string,
  status: RoomDeliveryStatus,
  overrides: Partial<Extract<RoomFeedItem, { kind: "delivery" }>> = {},
): RoomFeedEvent {
  return feedEvent({
    kind: "delivery",
    message_id: messageId,
    recipient_id: recipientId,
    status,
    bytes_written: 0,
    error: null,
    run_id: null,
    generation: null,
    ...overrides,
  });
}

function onlyMessage(items: ReadableFeedItem[]): Extract<ReadableFeedItem, { kind: "message" }> {
  expect(items).toHaveLength(1);
  const [item] = items;
  if (item.kind !== "message") throw new Error(`expected a message, got ${item.kind}`);
  return item;
}

describe("room feed view choice", () => {
  it("labels scheduled origin consistently without attributing it to the operator or inferring it from text", () => {
    const scheduled = message("fallback", { content: "literal <message>\nkeep this", fallback_origin: {
      revision: 2, session_id: "a", run_id: "run", generation: 1, occurrence_id: "occurrence", scheduled_at: "2026-10-19T12:30:00Z",
    } });
    const projected = onlyMessage(readableRoomFeed([scheduled], labelFor).items);
    expect(projected.author).toBe("prim1");
    expect(projected.body).toBe("literal <message>\nkeep this");
    expect(projected.sender).toMatch(/^PRIM-1 fallback · scheduled /);
    if (scheduled.item.kind !== "message") throw new Error("message fixture");
    expect(projected.sender).toBe(roomFallbackSenderLabel(scheduled.item));
    const ordinary = message("ordinary", { content: "[PRIM-1 fallback, scheduled 12:30] edited text" });
    const human = onlyMessage(readableRoomFeed([ordinary], labelFor).items);
    expect(human.author).toBe("you");
    expect(human.sender).toBe("You");
  });
  it("attributes typed continuations to PRIM-1 recovery in both views without trusting text or request IDs", () => {
    const content = "[PRIM-1 recovery continuation]\nKeep this literal body.";
    const typed = message("recovery", { content, continuation_origin: { session_id: "a", continuation_id: "interruption" } });
    const card = onlyMessage(readableRoomFeed([typed], labelFor).items);
    expect(card.author).toBe("prim1");
    expect(card.sender).toBe("PRIM-1 recovery");
    expect(card.body).toBe(content);
    if (typed.item.kind !== "message") throw new Error("message fixture");
    expect(roomRecoverySenderLabel(typed.item)).toBe(card.sender);
    for (const continuation_origin of [undefined, null]) {
      const ordinary = message("recovery", { content, continuation_origin });
      const ordinaryCard = onlyMessage(readableRoomFeed([ordinary], labelFor).items);
      expect(ordinaryCard.author).toBe("you");
      expect(ordinaryCard.sender).toBe("You");
    }
    const prefixed = message("request-only", { content, request_id: "prim1-recovery-continuation" });
    if (prefixed.item.kind !== "message") throw new Error("message fixture");
    expect(roomRecoverySenderLabel(prefixed.item)).toBeNull();
    expect(onlyMessage(readableRoomFeed([prefixed], labelFor).items).sender).not.toBe("PRIM-1 recovery");
    const bothOrigins = message("both-origins", { content, request_id: "prim1-post:legacy", continuation_origin: {
      session_id: "a", continuation_id: "interruption",
    }, fallback_origin: {
      revision: 2, session_id: "a", run_id: "run", generation: 1,
      occurrence_id: "occurrence", scheduled_at: "2026-10-19T12:30:00Z",
    } });
    const combined = onlyMessage(readableRoomFeed([bothOrigins], labelFor).items);
    expect(combined.sender).toBe("PRIM-1 recovery");
    expect(combined.author).toBe("prim1");
    expect(combined.body).toBe(content);
  });
  it("opens Readable unless Raw was chosen", () => {
    expect(parseRoomFeedView(null)).toBe("readable");
    expect(parseRoomFeedView("readable")).toBe("readable");
    expect(parseRoomFeedView("raw")).toBe("raw");
    expect(parseRoomFeedView("RAW")).toBe("readable");
    expect(parseRoomFeedView("{}")).toBe("readable");
  });
});

describe("readable feed: receipts", () => {
  it("hides successful and in-flight receipts and counts them", () => {
    const events = [
      message("m1"),
      receipt("m1", "a", "pending"),
      receipt("m1", "a", "written", { bytes_written: 40 }),
      receipt("m1", "b", "stored"),
      receipt("m1", "b", "woken"),
      receipt("m1", "b", "fetched"),
    ];
    const feed = readableRoomFeed(events, labelFor);
    const card = onlyMessage(feed.items);
    expect(feed.hidden).toBe(5);
    expect(card.problems).toEqual([]);
    expect(card.deliveries.map((delivery) => [delivery.recipientId, delivery.status]))
      .toEqual([["a", "written"], ["b", "fetched"]]);
  });

  it("surfaces a failed wake on its own message, on the receipt Raw retries", () => {
    const failed = receipt("m1", "a", "failed", { zero_write_confirmed: true, error: "pipe closed" });
    const events = [message("m1"), receipt("m1", "a", "pending"), failed, receipt("m1", "b", "written"), message("m2")];
    const feed = readableRoomFeed(events, labelFor);
    expect(feed.items.map((item) => item.key)).toEqual(["m1", "m2"]);
    const [first, second] = feed.items;
    if (first.kind !== "message" || second.kind !== "message") throw new Error("expected messages");
    expect(first.problems).toEqual([{
      key: "a:wake", recipientId: "a", recipient: "Claude PRIM-1", status: "failed", bytesWritten: 0,
      error: "pipe closed", input: "none", fetched: false, receipt: failed,
    }]);
    expect(retryableRoomDeliveryEvents("room-a", events).has(first.problems[0].receipt)).toBe(true);
    expect(second.problems).toEqual([]);
  });

  it("keeps a standing Held refusal and a partial failure visible", () => {
    const held = receipt("m1", "a", "held", { error: "room wake is held while input is active" });
    const partial = receipt("m1", "b", "failed", { bytes_written: 23, error: "write interrupted" });
    const card = onlyMessage(readableRoomFeed([message("m1"), held, partial], labelFor).items);
    expect(card.problems.map((problem) => [problem.key, problem.status, problem.input, problem.receipt]))
      .toEqual([["a:wake", "held", "none", held], ["b:input", "failed", "partial", partial]]);
  });

  it("drops a problem once a newer receipt succeeds, and a no-input one once Fetched", () => {
    const heldThenWritten = [message("m1"), receipt("m1", "a", "held"), receipt("m1", "a", "written")];
    expect(onlyMessage(readableRoomFeed(heldThenWritten, labelFor).items).problems).toEqual([]);

    const heldThenFetched = [message("m1"), receipt("m1", "a", "held"), receipt("m1", "a", "fetched")];
    expect(onlyMessage(readableRoomFeed(heldThenFetched, labelFor).items).problems).toEqual([]);

    const fetchedThenZeroFailed = [
      message("m1"), receipt("m1", "a", "fetched"), receipt("m1", "a", "failed", { zero_write_confirmed: true }),
      receipt("m1", "a", "held"),
    ];
    const card = onlyMessage(readableRoomFeed(fetchedThenZeroFailed, labelFor).items);
    expect(card.problems).toEqual([]);
    expect(card.deliveries[0].status).toBe("fetched");
  });

  it("reports a recipient with no receipt yet as Stored, not as a problem", () => {
    const card = onlyMessage(readableRoomFeed([message("m1", { recipient_ids: ["c"] })], labelFor).items);
    expect(card.deliveries).toEqual([{ recipientId: "c", status: "stored", receipt: null }]);
    expect(card.problems).toEqual([]);
  });

  it("keeps a standing failure whose message left the bounded window", () => {
    const orphan = receipt("gone", "b", "held", { error: "composer unknown" });
    const settled = receipt("gone", "a", "failed", { zero_write_confirmed: true });
    const events = [orphan, receipt("gone", "a", "pending"), settled, receipt("gone", "a", "fetched"), message("m2")];
    const feed = readableRoomFeed(events, labelFor);
    expect(feed.items.map((item) => item.kind)).toEqual(["problem", "message"]);
    const [first] = feed.items;
    if (first.kind !== "problem") throw new Error("expected a problem line");
    expect(first.problem.receipt).toBe(orphan);
    expect(first.key).toBe("problem:gone:b:wake");
    expect(feed.hidden).toBe(3);
  });
});

describe("readable feed: terminal input survives Fetched", () => {
  const partial = (messageId: string) =>
    receipt(messageId, "a", "failed", { bytes_written: 23, error: "write interrupted" });

  it.each([
    ["Fetched, then the partial write failed", (failed: RoomFeedEvent) => [
      message("m1"), receipt("m1", "a", "pending"), receipt("m1", "a", "fetched"), failed,
    ]],
    ["the partial write failed, then Fetched", (failed: RoomFeedEvent) => [
      message("m1"), receipt("m1", "a", "pending"), failed, receipt("m1", "a", "fetched"),
    ]],
  ] as const)("%s: shows the input, with no Retry", (_label, build) => {
    const failed = partial("m1");
    const events = build(failed);
    const card = onlyMessage(readableRoomFeed(events, labelFor).items);
    expect(card.deliveries[0].status).toBe("fetched");
    expect(card.problems).toHaveLength(1);
    const [problem] = card.problems;
    expect([problem.key, problem.input, problem.fetched, problem.receipt]).toEqual(["a:input", "partial", true, failed]);
    expect(retryableRoomDeliveryEvents("room-a", events).has(problem.receipt)).toBe(false);
    expect(readableProblemText(problem))
      .toBe("Claude PRIM-1 has read the message, but a wake left 23 bytes of input in its terminal: write interrupted");
  });

  it.each([
    ["Fetched, then the partial write failed", (failed: RoomFeedEvent) => [
      receipt("gone", "a", "fetched"), failed, message("m2", { recipient_ids: [] }),
    ]],
    ["the partial write failed, then Fetched", (failed: RoomFeedEvent) => [
      failed, receipt("gone", "a", "fetched"), message("m2", { recipient_ids: [] }),
    ]],
  ] as const)("outside the window too: %s", (_label, build) => {
    const failed = partial("gone");
    const events = build(failed);
    const feed = readableRoomFeed(events, labelFor);
    expect(feed.items.map((item) => item.kind)).toEqual(["problem", "message"]);
    const [line] = feed.items;
    if (line.kind !== "problem") throw new Error("expected a problem line");
    expect([line.key, line.problem.input, line.problem.fetched, line.problem.receipt])
      .toEqual(["problem:gone:a:input", "partial", true, failed]);
    expect(retryableRoomDeliveryEvents("room-a", events).has(failed)).toBe(false);
    expect(readableProblemText(line.problem, true)).toMatch(/^Earlier message: Claude PRIM-1 has read the message, but a wake left 23 bytes/);
  });

  it("treats a 0-byte failure without zero-write proof as possible input, before or after Fetched", () => {
    const unknown = receipt("m1", "a", "failed", { error: "pipe closed" });
    const before = onlyMessage(readableRoomFeed([message("m1"), unknown], labelFor).items).problems;
    expect(before.map((problem) => [problem.key, problem.input, problem.fetched])).toEqual([["a:input", "unknown", false]]);
    expect(readableProblemText(before[0])).toBe("A wake to Claude PRIM-1 failed and may have left input in its terminal: pipe closed");

    const after = onlyMessage(readableRoomFeed([message("m1"), receipt("m1", "a", "fetched"), unknown], labelFor).items).problems;
    expect(after.map((problem) => [problem.input, problem.fetched])).toEqual([["unknown", true]]);
  });
});

describe("readable feed: notes and senders", () => {
  it("keeps membership changes as notes, in feed order", () => {
    const events = [
      feedEvent({ kind: "membership", action: "joined", session_id: "c", membership_revision: 2 }),
      message("m1"),
      feedEvent({ kind: "membership", action: "removed", session_id: "zz", membership_revision: 3 }),
    ];
    const feed = readableRoomFeed(events, labelFor);
    expect(feed.items.map((item) => (item.kind === "note" ? item.text : item.key)))
      .toEqual(["Grok PRIM-1 joined the room", "m1", "Member zz was removed from the room"]);
    expect(feed.items[0].key).toBe(`epoch-a:${events[0].cursor.sequence}`);
    expect(feed.hidden).toBe(0);
  });

  it("labels the operator You and a member by its label, with its recipients", () => {
    const events = [
      message("m1", { recipient_ids: [] }),
      message("m2", { sender: { kind: "session", session_id: "b" }, recipient_ids: ["a", "c"] }),
    ];
    const [mine, theirs] = readableRoomFeed(events, labelFor).items;
    if (mine.kind !== "message" || theirs.kind !== "message") throw new Error("expected messages");
    expect([mine.author, mine.sender, mine.recipients]).toEqual(["you", "You", []]);
    expect([theirs.author, theirs.sender, theirs.recipients])
      .toEqual(["member", "Codex PRIM-1", ["Claude PRIM-1", "Grok PRIM-1"]]);
  });

  it("names PRIM-1 as the sender of its own restart notice, by request id only", () => {
    const notice = restartNotice("Upgrade", ["Claude"], ["Grok"]);
    const events = [
      message("auto", { request_id: "prim1-restart-abc-room-a", content: notice, recipient_ids: [] }),
      message("typed", { request_id: "6f1c2a8e-0000-4000-8000-000000000000", content: notice, recipient_ids: [] }),
    ];
    const [auto, typed] = readableRoomFeed(events, labelFor).items;
    if (auto.kind !== "message" || typed.kind !== "message") throw new Error("expected messages");
    expect([auto.author, auto.sender]).toEqual(["prim1", "PRIM-1"]);
    expect(auto.body).toBe("PRIM-1 restarted. Back online in Upgrade: Claude. Did not come back: Grok (see each pane). Read the room before you continue.");
    expect([typed.author, typed.sender, typed.body]).toEqual(["you", "You", notice]);
  });

  it("never treats a member as PRIM-1, whatever its label or request id", () => {
    const impostor = (sessionId: string) => (sessionId === "p" ? "PRIM-1" : labelFor(sessionId));
    const events = [
      message("m1", { sender: { kind: "session", session_id: "p" }, content: "PRIM-1 restarted.", recipient_ids: [] }),
      message("m2", { sender: { kind: "session", session_id: "p" }, request_id: "prim1-restart-x", recipient_ids: [] }),
    ];
    const cards = readableRoomFeed(events, impostor).items;
    expect(cards.map((card) => (card.kind === "message" ? [card.author, card.sender] : card.kind)))
      .toEqual([["member", "PRIM-1"], ["member", "PRIM-1"]]);
  });

  it("shows an outside agent under the name it posted with, as a guest, never as PRIM-1 or a member", () => {
    const events = [
      message("g1", { sender: { kind: "guest", guest_id: "g", label: "ChatGPT" }, recipient_ids: ["a"] }),
      message("g2", { sender: { kind: "guest", guest_id: "g", label: "PRIM-1" }, request_id: "prim1-restart-x", recipient_ids: [] }),
    ];
    const [first, second] = readableRoomFeed(events, labelFor, true).items;
    if (first.kind !== "message" || second.kind !== "message") throw new Error("expected messages");
    expect([first.author, first.sender, first.recipients]).toEqual(["guest", "ChatGPT", ["Claude PRIM-1"]]);
    expect([second.author, second.sender]).toEqual(["guest", "PRIM-1"]);
  });

  it("shows line breaks the way they were written, whatever the line endings", () => {
    const card = onlyMessage(readableRoomFeed([message("m1", { content: "one\r\ntwo\rthree\n\nfour" })], labelFor).items);
    expect(card.body).toBe("one\ntwo\nthree\n\nfour");
    expect(card.preview).toEqual({ text: card.body, collapsed: false });
  });
});

describe("readable feed: collapse", () => {
  const lines = (count: number) => Array.from({ length: count }, (_, index) => `line ${index + 1}`).join("\n");

  it("collapses only past the threshold, to the first 12 lines", () => {
    expect(readablePreview(lines(READABLE_COLLAPSE_ABOVE_LINES))).toEqual({
      text: lines(READABLE_COLLAPSE_ABOVE_LINES), collapsed: false,
    });
    expect(readablePreview(lines(READABLE_COLLAPSE_ABOVE_LINES + 1))).toEqual({
      text: lines(READABLE_PREVIEW_LINES), collapsed: true,
    });
  });

  it("counts a long line as the rows it wraps to", () => {
    const fits = "x".repeat(READABLE_COLLAPSE_ABOVE_LINES * READABLE_LINE_CHARS);
    expect(readablePreview(fits).collapsed).toBe(false);
    expect(readablePreview(`${fits}x`).collapsed).toBe(true);
    expect(readablePreview(`${"y".repeat(15 * READABLE_LINE_CHARS)}\n${lines(2)}`).collapsed).toBe(true);
  });

  it("cuts one long paragraph on a word boundary and marks the cut", () => {
    const paragraph = Array.from({ length: 600 }, (_, index) => `word${index}`).join(" ");
    const preview = readablePreview(paragraph);
    expect(preview.collapsed).toBe(true);
    expect(preview.text.endsWith(" …")).toBe(true);
    const kept = preview.text.slice(0, -2);
    expect(kept.length).toBeLessThanOrEqual(READABLE_PREVIEW_LINES * READABLE_LINE_CHARS);
    expect(paragraph.startsWith(`${kept} `)).toBe(true);
  });

  it("fills the rows a short head leaves, and never splits a surrogate pair", () => {
    const head = lines(10);
    const preview = readablePreview(`${head}\nx${"😀".repeat(2000)}`);
    // Two rows remain; 240 UTF-16 units would end inside an emoji, so the cut steps back one.
    expect(preview).toEqual({ text: `${head}\nx${"😀".repeat(119)} …`, collapsed: true });
  });
});

describe("readable feed: meta and warning text", () => {
  it("lists up to three recipients, then counts the rest", () => {
    expect(readableRecipients([])).toBe("");
    expect(readableRecipients(["A"])).toBe("to A");
    expect(readableRecipients(["A", "B", "C"])).toBe("to A, B, C");
    expect(readableRecipients(["A", "B", "C", "D", "E"])).toBe("to A, B and 3 more");
  });

  it("shows clock time today and the date before", () => {
    const now = new Date(2026, 9, 14, 12, 0);
    expect(readableTime(new Date(2026, 9, 14, 9, 5).toISOString(), now)).toBe("09:05");
    expect(readableTime(new Date(2026, 9, 13, 21, 4).toISOString(), now)).toBe("13 Oct, 21:04");
    expect(readableTime(new Date(2025, 11, 31, 23, 59).toISOString(), now)).toBe("31 Dec 2025, 23:59");
    expect(readableTime("not a time", now)).toBe("");
  });

  it("words held, failed, partial and out-of-window problems plainly", () => {
    const base: ReadableProblem = {
      key: "a:wake", recipientId: "a", recipient: "Claude PRIM-1", status: "held", bytesWritten: 0,
      error: "room wake is held while input is active", input: "none", fetched: false,
      receipt: receipt("m1", "a", "held"),
    };
    expect(readableProblemText(base)).toBe("Claude PRIM-1 was not woken; prompt manually or use an available Retry wake.");
    expect(readableProblemText(base, true))
      .toBe("Earlier message: Claude PRIM-1 was not woken; prompt manually or use an available Retry wake.");
    expect(readableProblemText({ ...base, status: "failed", error: "pipe\nclosed" }))
      .toBe("Wake to Claude PRIM-1 failed: pipe closed");
    expect(readableProblemText({ ...base, status: "failed", input: "partial", bytesWritten: 23, error: null }))
      .toBe("A wake to Claude PRIM-1 failed partway; 23 bytes of input reached its terminal.");
    expect(readableProblemText({ ...base, status: "failed", input: "partial", bytesWritten: 1, fetched: true, error: null }))
      .toBe("Claude PRIM-1 has read the message, but a wake left 1 byte of input in its terminal.");
    const long = readableProblemText({ ...base, status: "failed", error: "e".repeat(4096) });
    expect(long.endsWith("…")).toBe(true);
    expect(long.length).toBe("Wake to Claude PRIM-1 failed: ".length + READABLE_REASON_CHARS);
  });
});

describe("operator entry sender (A16)", () => {
  it("names a local process by its own label, never as the person at the screen", () => {
    const label = "MAINPC supervisor over SSH (local process 4242)";
    expect(readableSender({ kind: "local_process", label }, () => "member")).toBe(label);
    expect(readableSender({ kind: "operator" }, () => "member")).toBe("You");
  });
});

describe("v3 W6 per-recipient state on a Readable card", () => {
  const waiting = (messageId: string, recipientId: string, code: string): RoomFeedEvent =>
    feedEvent({ kind: "waiting", message_id: messageId, recipient_id: recipientId, reason_code: code, reason: `why ${code}` });
  const ack = (messageId: string, recipientId: string, actor: Extract<RoomFeedItem, { kind: "ack" }>["actor"],
    disposition: "acked" | "declined", note: string): RoomFeedEvent =>
    feedEvent({ kind: "ack", request_id: `ack-${recipientId}`, message_id: messageId, recipient_id: recipientId, actor, disposition, note });

  it("says a Codex App Server acceptance was accepted, never written to a terminal (v3 W4 S2)", () => {
    const state = onlyMessage(readableRoomFeed([message("m1"), receipt("m1", "a", "written", { app_server_turn: "t-1" })],
      labelFor).items).states[0];
    expect([state.state, state.detail]).toEqual(["written", "accepted by the Codex App Server, not yet read"]);
    expect(state.title).toContain("turn t-1");
    expect(state.title).not.toContain("terminal");
    expect(onlyMessage(readableRoomFeed([message("m1"), receipt("m1", "a", "written", { app_server_turn: "t-1" }),
      receipt("m1", "a", "fetched")], labelFor).items).states[0].detail).toBe("read, not yet acknowledged");
  });

  it("shows waiting with its code, then woken, read and ACK in that precedence", () => {
    const states = (events: RoomFeedEvent[]) => onlyMessage(readableRoomFeed(events, labelFor).items).states
      .map((state) => [state.recipient, state.state, state.detail]);
    const m = message("m1");
    expect(states([m, waiting("m1", "a", "output_settling")])).toEqual([
      ["Claude PRIM-1", "waiting", "waiting: output_settling"], ["Codex PRIM-1", "stored", "stored"]]);
    expect(states([m, waiting("m1", "a", "output_settling"), receipt("m1", "a", "written")]))
      .toEqual([["Claude PRIM-1", "written", "written; submission unconfirmed, not yet read"], ["Codex PRIM-1", "stored", "stored"]]);
    expect(states([m, receipt("m1", "a", "written"), waiting("m1", "a", "submit_unconfirmed")])[0])
      .toEqual(["Claude PRIM-1", "waiting", "waiting: submit_unconfirmed"]);
    expect(states([m, receipt("m1", "a", "written"), receipt("m1", "a", "fetched")])[0])
      .toEqual(["Claude PRIM-1", "read", "read, not yet acknowledged"]);
    expect(states([m, receipt("m1", "a", "fetched"), ack("m1", "a", { kind: "session", session_id: "a" }, "acked", "on it")])[0])
      .toEqual(["Claude PRIM-1", "acked", "on it"]);
  });

  it("keeps a hold reason after a read, and names who disposed of a message", () => {
    const m = message("m1");
    const card = onlyMessage(readableRoomFeed([m, receipt("m1", "a", "held", { error: "sender revoked" }),
      receipt("m1", "a", "fetched"),
      ack("m1", "b", { kind: "local_process", label: "MAINPC supervisor (local process 42)" }, "declined", "handled")], labelFor).items);
    expect(card.states.map((state) => [state.state, state.detail])).toEqual([
      ["read", "read; its wake was held: sender revoked"],
      ["declined", "handled (by MAINPC supervisor (local process 42))"],
    ]);
    expect(readableStatesText(card.states)).toBe(
      "Claude PRIM-1: read; its wake was held: sender revoked · Codex PRIM-1: declined — handled (by MAINPC supervisor (local process 42))");
  });
});


// Reviewer 585e0423: transport evidence and a live deferred retry are distinct.
describe("reviewer W6 truthful transport state", () => {
  it("never labels Written as Woken or submitted without stronger evidence", () => {
    const card = onlyMessage(readableRoomFeed([
      message("review-written", { recipient_ids: ["a"] }),
      receipt("review-written", "a", "written"),
    ], labelFor).items);
    const state = card.states[0];
    expect(state.state).not.toBe("woken");
    expect(state.detail.toLowerCase()).toContain("written");
    expect(state.detail.toLowerCase()).toContain("unconfirmed");
  });

  it("shows a confirmed-zero failed attempt's current waiting reason", () => {
    const card = onlyMessage(readableRoomFeed([
      message("review-wait", { recipient_ids: ["a"] }),
      receipt("review-wait", "a", "failed", { zero_write_confirmed: true }),
      feedEvent({ kind: "waiting", message_id: "review-wait", recipient_id: "a",
        reason_code: "output_settling", reason: "waiting for output to settle before the next attempt" }),
    ], labelFor).items);
    expect(card.states[0].state).toBe("waiting");
    expect(card.states[0].detail).toContain("output_settling");
    expect(card.deliveries[0].status).toBe("failed");
  });
});


// A typed result card does not erase a different addressed request's status.
it("reviewer W6 keeps addressed session-request uncertainty when typed cards exist", () => {
  const request = message("review-card-request", { sender: { kind: "session", session_id: "b" }, recipient_ids: ["a"], ack_requested: true });
  const events = [request, receipt("review-card-request", "a", "written"),
    feedEvent({ kind: "waiting", message_id: "review-card-request", recipient_id: "a",
      reason_code: "submit_unknown", reason: "input observation unavailable; request remains owed" })];
  const normal = readableRoomFeed(events, labelFor, false);
  expect(JSON.stringify(normal.items)).toContain("submit_unknown");
  const withCards = readableRoomFeed(events, labelFor, true);
  // Allow either the existing message card or a compact status projection.
  expect(JSON.stringify(withCards.items)).toContain("submit_unknown");
});

it("W3b shows an observed submission without calling it an acknowledgement", () => {
  const events = [message("obs", { recipient_ids: ["a"] }), receipt("obs", "a", "written"),
    feedEvent({ kind: "waiting", message_id: "obs", recipient_id: "a", reason_code: "submit_unconfirmed", reason: "uncertain" }),
    feedEvent({ kind: "input_observed", message_id: "obs", recipient_id: "a", attempt: { epoch: "e", sequence: 2 }, source: "claude_transcript", locator: "c.jsonl@0" })];
  const state = onlyMessage(readableRoomFeed(events, labelFor).items).states[0];
  expect([state.state, state.detail]).toEqual(["written", "submitted (input observed), not yet read"]);
});

describe("an already-read pointer (v3 W3, Claude PRIM-1 26a7f92 walk)", () => {
  it("is no wake problem and offers no Retry, even with no read receipt in the window", () => {
    const sent = message("m-read", { recipient_ids: ["a"] });
    // As the backend records it: no retry proof after the read.
    const notTyped = receipt("m-read", "a", "failed", { zero_write_confirmed: false, already_read: true,
      error: "the member had already read every message this wake points to; nothing was typed" });
    const events = [sent, receipt("m-read", "a", "pending"), notTyped];
    expect(onlyMessage(readableRoomFeed(events, labelFor).items).problems).toEqual([]);
    expect(retryableRoomDeliveryEvents("room-a", events).size).toBe(0);
  });
});

describe("a withdrawn message in the readable view (v3 W3 D6)", () => {
  it("shows who withdrew it and why, and no wake problem", () => {
    const sent = message("m-gone", { recipient_ids: ["a"], ack_requested: true });
    const held = receipt("m-gone", "a", "held", { error: "withdrawn: the operator cancelled this message (wrong room); it is not typed" });
    const cancel = feedEvent({ kind: "cancel", request_id: "c-1", message_id: "m-gone", actor: { kind: "operator" }, reason: "wrong room",
      not_typed: ["a"] });
    const items = readableRoomFeed([sent, cancel, held], labelFor).items;
    const card = items.find((item) => item.kind === "message");
    expect(card && card.kind === "message" ? card.problems : null).toEqual([]);
    expect(items.some((item) => item.kind === "note"
      && item.text === `You withdrew this message: wrong room Not typed: ${labelFor("a")}.`)).toBe(true);
  });

  it("says a withdrawn recipient owes nothing, and an unneeded pointer is no failure (J2: truthful states)", () => {
    const sent = message("m-gone2", { recipient_ids: ["a"], ack_requested: true });
    const held = receipt("m-gone2", "a", "held", { error: "withdrawn: the operator cancelled this message (dup); it is not typed" });
    const cancel = feedEvent({ kind: "cancel", request_id: "c-2", message_id: "m-gone2", actor: { kind: "operator" }, reason: "dup" });
    const card = onlyMessage(readableRoomFeed([sent, cancel, held], labelFor).items.filter((item) => item.kind === "message"));
    expect(card.states.map((state) => [state.state, state.detail])).toEqual([["withdrawn", "withdrawn; nothing owed"]]);

    const asked = message("m-moot", { recipient_ids: ["a"], ack_requested: true });
    const moot = receipt("m-moot", "a", "failed", { zero_write_confirmed: false, already_read: true });
    const mootCard = onlyMessage(readableRoomFeed([asked, moot], labelFor).items.filter((item) => item.kind === "message"));
    expect(mootCard.states.map((state) => state.detail)).toEqual(["not typed: nothing open to read"]);
  });

  it("attributes a PRIM-1 notice to PRIM-1", () => {
    const notice = message("m-notice", { sender: { kind: "prim1" }, recipient_ids: [],
      content: "[PRIM-1 recovery] Claude J: started fresh." });
    const card = onlyMessage(readableRoomFeed([notice], labelFor).items);
    expect([card.author, card.sender]).toEqual(["prim1", "PRIM-1"]);
  });
});

