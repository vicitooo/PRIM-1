import { describe, expect, it } from "vitest";

import {
  MAX_RENDERED_ROOM_FEED_BYTES,
  MAX_RENDERED_ROOM_FEED_EVENTS,
  MAX_PENDING_ROOM_INTENTS,
  MAX_PENDING_ROOM_INTENT_BYTES,
  appendRoomFeedEvent,
  retainRoomFeedWindow,
  retryableRoomDeliveryEvents,
  roomDeliveryFeedback,
  roomMessageIntent,
  submitRoomIntent,
  roomRequestFailureFeedback,
  roomDeliveryRetryUnavailable,
  roomRetryFeedback,
  roomWakeUnavailable,
  roomHistoryReplayMessage,
  roomWriterRecoveryMessage,
  roomDeliveryStatusLabel,
  roomDeliveryReceiptLabel,
  deliveryNotNeeded,
  roomNoticeSenderLabel,
  roomGuestLabel,
  roomMessageRecipientsLabel,
  roomOpenObligations,
  roomWithdrawalText,
  roomWithdrawalNotice,
  roomWithdrawPreview,
  roomMessageReached,
} from "./room-feed";
import type { RoomDeliveryReceipt, RoomDeliveryResult, RoomFeedCursor, RoomFeedEvent, RoomFeedItem } from "./types";

function event(
  roomId: string,
  epoch: string,
  sequence: number,
  content = `content-${sequence}`,
): RoomFeedEvent {
  return {
    schema_version: 1,
    room_id: roomId,
    cursor: { epoch, sequence },
    item: {
      kind: "message",
      message_id: `message-${sequence}`,
      request_id: `request-${sequence}`,
      sender: { kind: "operator" },
      content,
      recipient_ids: [],
      membership_revision: 1,
    },
    timestamp: "2026-08-11T00:00:00Z",
  };
}

describe("room message intent retries", () => {
  it("evicts the oldest intent at the count limit without extending it on retry", () => {
    const pending = new Map<string, string>();
    const first = roomMessageIntent(pending, "room-a", "feed", "first");
    for (let index = 1; index < MAX_PENDING_ROOM_INTENTS; index += 1) {
      roomMessageIntent(pending, "room-a", "feed", `message-${index}`);
    }
    expect(roomMessageIntent(pending, "room-a", "feed", "first")).toEqual(first);
    roomMessageIntent(pending, "room-a", "feed", "newest");
    expect(pending.size).toBe(MAX_PENDING_ROOM_INTENTS);
    expect(pending.has(first.signature)).toBe(false);
    expect(roomMessageIntent(pending, "room-a", "feed", "first").requestId).not.toBe(first.requestId);
  });

  it("accepts the exact byte limit and evicts by serialized UTF-8 bytes, not character count", () => {
    const pending = new Map<string, string>();
    const encoder = new TextEncoder();
    const overhead = encoder.encode(JSON.stringify(["room-a", "feed", ""])).byteLength;
    const roomForContent = MAX_PENDING_ROOM_INTENT_BYTES - overhead;
    const content = "λ".repeat(Math.floor(roomForContent / 2)) + "x".repeat(roomForContent % 2);
    const full = roomMessageIntent(pending, "room-a", "feed", content);
    expect(encoder.encode(full.signature).byteLength).toBe(MAX_PENDING_ROOM_INTENT_BYTES);
    expect(pending.has(full.signature)).toBe(true);
    const newest = roomMessageIntent(pending, "room-a", "feed", "newest");
    expect([...pending.keys()]).toEqual([newest.signature]);
  });

  it("rejects an oversized intent without forgetting existing retry keys", () => {
    const pending = new Map<string, string>();
    const first = roomMessageIntent(pending, "room-a", "feed", "first");
    expect(() => roomMessageIntent(pending, "room-a", "feed", "x".repeat(MAX_PENDING_ROOM_INTENT_BYTES)))
      .toThrow("too large to retain a retry key");
    expect([...pending.entries()]).toEqual([[first.signature, first.requestId]]);
  });

  it("reuses an uncertain request only for the same room, target and exact content", () => {
    const pending = new Map<string, string>();
    const first = roomMessageIntent(pending, "room-a", "send:all", "hello λ\n");
    const changed = [
      roomMessageIntent(pending, "room-b", "send:all", "hello λ\n"),
      roomMessageIntent(pending, "room-a", "send:one", "hello λ\n"),
      roomMessageIntent(pending, "room-a", "feed", "hello λ\n"),
      roomMessageIntent(pending, "room-a", "send:all", "hello λ"),
    ];
    expect(new Set([first, ...changed].map((intent) => intent.requestId)).size).toBe(5);
    expect(roomMessageIntent(pending, "room-a", "send:all", "hello λ\n")).toEqual(first);

    // An acknowledged post ends this intent; an intentional repeat is a new post.
    pending.delete(first.signature);
    expect(roomMessageIntent(pending, "room-a", "send:all", "hello λ\n").requestId)
      .not.toBe(first.requestId);
  });
});

describe("room invoke intent lifecycle", () => {
  it.each([new Error("lost reply"), { kind: "commit_uncertain", message: "sync failed" }, { kind: "recovery_required", message: "replay refused" }, { kind: "request_conflict", message: "prior intent stored" }])(
    "keeps an uncertain key through a retry and releases it only after a result", async (failure) => {
      const pending = new Map<string, string>();
      const original = roomMessageIntent(pending, "room-a", "send:all", "exact body");
      const ids: string[] = [];
      await expect(submitRoomIntent(pending, original, async (id) => { ids.push(id); throw failure; })).rejects.toBe(failure);
      const retry = roomMessageIntent(pending, "room-a", "send:all", "exact body");
      await expect(submitRoomIntent(pending, retry, async (id) => { ids.push(id); return "stored"; })).resolves.toBe("stored");
      expect(ids).toEqual([original.requestId, original.requestId]);
      expect(pending.size).toBe(0);
      expect(roomMessageIntent(pending, "room-a", "send:all", "exact body").requestId).not.toBe(original.requestId);
    },
  );
  it("retires a definitely rejected intent without telling the user to retry it unchanged", async () => {
    const pending = new Map<string, string>();
    const intent = roomMessageIntent(pending, "room-a", "feed", "invalid body");
    const rejected = { kind: "rejected", message: "disallowed control character" };
    await expect(submitRoomIntent(pending, intent, async () => { throw rejected; })).rejects.toBe(rejected);
    expect(pending.size).toBe(0);
    expect(roomRequestFailureFeedback(rejected)).toBe("Not stored: disallowed control character");
    expect(roomRequestFailureFeedback({ kind: "commit_uncertain", message: "sync failed" })).toContain("history recovery");
    expect(roomRequestFailureFeedback(new Error("IPC lost"))).toContain("same request key");
  });
});

function receipt(
  recipientId: string,
  status: RoomDeliveryReceipt["status"],
  overrides: Partial<RoomDeliveryReceipt> = {},
): RoomDeliveryReceipt {
  return {
    recipient_id: recipientId,
    status,
    bytes_written: 0,
    error: null,
    run_id: null,
    generation: null,
    recorded: true,
    ...overrides,
  };
}

function deliveryResult(receipts: RoomDeliveryReceipt[]): RoomDeliveryResult {
  return {
    room_id: "room-a",
    message_id: "message-a",
    request_id: "request-a",
    replayed: false,
    cursor: { epoch: "epoch-a", sequence: 1 },
    recipient_count: receipts.length,
    written_count: receipts.filter((item) => item.status === "written").length,
    failures: [],
    receipts,
  };
}

type DeliveryItem = Extract<RoomFeedEvent["item"], { kind: "delivery" }>;

function deliveryEvent(
  sequence: number,
  status: RoomDeliveryReceipt["status"],
  overrides: Partial<DeliveryItem> = {},
): RoomFeedEvent {
  return {
    ...event("room-a", "epoch-a", sequence),
    item: {
      kind: "delivery",
      message_id: "message-a",
      recipient_id: "a",
      status,
      bytes_written: 0,
      error: null,
      run_id: null,
      generation: null,
      ...overrides,
    },
  };
}

describe("room wake retry eligibility", () => {
  it("offers Held with no bytes and Failed only with explicit zero-write proof", () => {
    const held = deliveryEvent(1, "held");
    const failed = deliveryEvent(2, "failed", { recipient_id: "b", zero_write_confirmed: true });
    expect(retryableRoomDeliveryEvents("room-a", [held, failed])).toEqual(new Set([held, failed]));
  });

  it.each([
    ["legacy Failed", "failed", {}],
    ["unconfirmed Failed", "failed", { zero_write_confirmed: false }],
    ["partial Failed", "failed", { bytes_written: 1, zero_write_confirmed: true }],
    ["partial Held", "held", { bytes_written: 1 }],
    ["unknown outcome", "stored", { zero_write_confirmed: true }],
    ["pending wake", "pending", { zero_write_confirmed: true }],
    ["terminal write", "written", { zero_write_confirmed: true }],
    ["adapter wake", "woken", { zero_write_confirmed: true }],
    ["fetched message", "fetched", { zero_write_confirmed: true }],
  ] as const)("excludes %s", (_label, status, overrides) => {
    expect(retryableRoomDeliveryEvents("room-a", [deliveryEvent(1, status, overrides)]).size).toBe(0);
  });

  it.each(["stored", "pending", "written", "woken", "fetched", "failed"] as const)(
    "does not reveal old Held when the latest receipt is %s",
    (status) => {
      const held = deliveryEvent(1, "held");
      expect(retryableRoomDeliveryEvents("room-a", [held, deliveryEvent(2, status)]).size).toBe(0);
    },
  );

  it("uses only the newest eligible receipt and its original cursor", () => {
    const old = deliveryEvent(1, "held");
    const latest = deliveryEvent(2, "failed", { zero_write_confirmed: true });
    const eligible = retryableRoomDeliveryEvents("room-a", [old, latest]);
    expect(eligible.has(old)).toBe(false);
    expect([...eligible]).toEqual([latest]);
    expect([...eligible][0].cursor).toEqual({ epoch: "epoch-a", sequence: 2 });
  });

  it.each(["held", "failed"] as const)("keeps Fetched final when a later wake receipt is %s", (status) => {
    const fetched = deliveryEvent(1, "fetched");
    const later = deliveryEvent(2, status, { zero_write_confirmed: true });
    expect(retryableRoomDeliveryEvents("room-a", [fetched, later]).size).toBe(0);
  });

  it.each([
    ["the member's own ACK", { kind: "session", session_id: "a" }],
    ["an operator disposition", { kind: "operator" }],
    ["a local-process disposition", { kind: "local_process", label: "MAINPC supervisor (local process 4242)" }],
  ] as const)("offers no Retry once %s is stored (Reviewer 998e10ed)", (_label, actor) => {
    const held = deliveryEvent(1, "held");
    const ack: RoomFeedEvent = { ...event("room-a", "epoch-a", 2), item: { kind: "ack", request_id: "r1",
      message_id: "message-a", recipient_id: "a", actor, disposition: "declined", note: "handled elsewhere" } };
    expect(retryableRoomDeliveryEvents("room-a", [held, ack]).size).toBe(0);
  });

  it("tracks recipients and messages separately and ignores other rooms", () => {
    const superseded = deliveryEvent(1, "held");
    const otherRecipient = deliveryEvent(2, "held", { recipient_id: "b" });
    const otherMessage = deliveryEvent(3, "held", { message_id: "message-b" });
    const fetched = deliveryEvent(4, "fetched");
    const otherRoom = { ...deliveryEvent(5, "fetched", { recipient_id: "b" }), room_id: "room-b" };
    expect(retryableRoomDeliveryEvents("room-a", [
      superseded, otherRecipient, otherMessage, fetched, otherRoom, event("room-a", "epoch-a", 6),
    ])).toEqual(new Set([otherRecipient, otherMessage]));
  });
});

describe("current recipient and retry feedback", () => {
  it("rejects old run bindings and removed members while allowing an unbound offline Held", () => {
    const sessions = new Map([["a", { run_id: "run-current", generation: 4, driver: "grok" as const }]]);
    const bound = deliveryEvent(1, "held", { run_id: "run-current", generation: 4 });
    expect(roomDeliveryRetryUnavailable(bound, ["a"], sessions)).toBeNull();
    expect(roomDeliveryRetryUnavailable(bound, [], sessions)).toContain("no longer");
    expect(roomDeliveryRetryUnavailable(bound, ["a"], new Map())).toContain("restarted");
    expect(roomDeliveryRetryUnavailable(deliveryEvent(1, "held", { run_id: "old", generation: 4 }), ["a"], sessions)).toContain("restarted");
    expect(roomDeliveryRetryUnavailable(deliveryEvent(1, "held", { run_id: "run-current", generation: 3 }), ["a"], sessions)).toContain("restarted");
    expect(roomDeliveryRetryUnavailable(deliveryEvent(1, "held"), ["a"], new Map())).toBeNull();
  });
  it("does not report an unrelated old failure as a failed recipient retry", () => {
    const result = deliveryResult([receipt("a", "failed", { bytes_written: 12 }), receipt("b", "written")]);
    const feedback = roomRetryFeedback(result, "b", (id) => id);
    expect(feedback.level).toBe("info");
    expect(feedback.message).not.toContain("a: Failed");
    expect(feedback.message).toContain("b: Wake written");
    expect(roomDeliveryStatusLabel("pending")).toBe("Wake attempt recorded; transport outcome pending");
  });
});

describe("stored room message feedback", () => {
  const label = (id: string) => `Member ${id}`;

  it("names mixed outcomes without confusing storage, refusal and partial writes", () => {
    const feedback = roomDeliveryFeedback(deliveryResult([
      receipt("a", "written", { bytes_written: 100 }),
      receipt("b", "held", { error: "composer unknown" }),
      receipt("c", "failed", { bytes_written: 23, error: "write interrupted" }),
    ]), label);
    expect(feedback.level).toBe("error");
    expect(feedback.message).toContain("Stored in the room feed.");
    expect(feedback.message).toContain("Member a: Wake written to terminal, 100 bytes");
    expect(feedback.message).toContain("Member b: Held (composer unknown)");
    expect(feedback.message).toContain("Member c: Failed, 23 bytes (write interrupted)");
    expect(feedback.message).toContain("Model acceptance is unconfirmed; the feed shows each recipient's current state.");
    expect(feedback.message).toContain("No automatic retry. Use Retry wake on an eligible recipient receipt; do not repost to all.");
    expect(feedback.summary).toContain("Use Retry wake on eligible held receipts.");
  });

  it("reports a held brief as a warning even when the legacy failures list is empty", () => {
    const feedback = roomDeliveryFeedback(deliveryResult([
      receipt("a", "held", { error: "approval open" }),
    ]), label);
    expect(feedback.level).toBe("warn");
    expect(feedback.message).toContain("Member a: Held (approval open)");
    expect(feedback.message).not.toContain("sent");
  });

  it("keeps repeated long held reasons in details while the composer gets counts", () => {
    const reason = "No verified wake adapter. ".repeat(40);
    const feedback = roomDeliveryFeedback(deliveryResult([
      receipt("a", "held", { error: reason }),
      receipt("b", "held", { error: reason }),
    ]), label, () => "No verified wake adapter");
    expect(feedback.summary).toBe("Stored in the room feed. At send: 2 held. Prompt held members manually.");
    expect(feedback.level).toBe("warn");
    expect(feedback.message).toContain(`Member a: Held (${reason})`);
    expect(feedback.message).toContain(`Member b: Held (${reason})`);
  });

  it("keeps partial input and missing durability visible in a compact result", () => {
    const feedback = roomDeliveryFeedback(deliveryResult([
      receipt("a", "written", { bytes_written: 100 }),
      receipt("b", "failed", { bytes_written: 12, recorded: false, error: "sync failed after partial input" }),
    ]), label);
    expect(feedback.summary).toContain("1 wake written");
    expect(feedback.summary).toContain("1 failed");
    expect(feedback.summary).toContain("1 partial write");
    expect(feedback.summary).toContain("1 receipt not saved");
    expect(feedback.summary).toContain("Check storage and history; do not repost to all.");
    expect(feedback.level).toBe("error");
    expect(feedback.message).toContain("12 bytes (sync failed after partial input)");
  });

  it("separates a saved message from an unsaved wake receipt", () => {
    const feedback = roomDeliveryFeedback(deliveryResult([
      receipt("a", "written", { bytes_written: 12, recorded: false }),
    ]), label);
    expect(feedback.level).toBe("warn");
    expect(feedback.message).toContain("Stored in the room feed.");
    expect(feedback.message).toContain("[receipt not saved]");
  });

  it("makes replay and unfinished wake outcomes explicit", () => {
    const result = deliveryResult([receipt("a", "stored")]);
    result.replayed = true;
    const feedback = roomDeliveryFeedback(result, label);
    expect(feedback.level).toBe("warn");
    expect(feedback.message).toContain("previous request; no new wake attempted");
    expect(feedback.message).toContain("Stored; typed in when the member is ready");
    expect(feedback.summary).toContain("Previously stored; no new wake attempted.");
    expect(feedback.summary).toContain("1 queued until ready");

    const missing = deliveryResult([]);
    missing.recipient_count = 1;
    expect(roomDeliveryFeedback(missing, label).level).toBe("warn");
    expect(roomDeliveryFeedback(missing, label).summary).toContain("1 wake outcome unavailable");
  });

  it("keeps notification and fetch outcomes distinct from model acceptance", () => {
    const feedback = roomDeliveryFeedback(deliveryResult([
      receipt("a", "woken"),
      receipt("b", "fetched"),
    ]), label);
    expect(feedback.level).toBe("info");
    expect(feedback.message).toContain("Member a: Wake notification confirmed");
    expect(feedback.message).toContain("Member b: Fetched (page returned)");
    expect(feedback.message).toContain("Model acceptance is unconfirmed; the feed shows each recipient's current state.");
  });
});

describe("room feed event gate", () => {
  it("accepts only contiguous exact-room events and deduplicates repeats", () => {
    const first = event("room-a", "epoch-a", 4);
    const accepted = appendRoomFeedEvent("room-a", [], undefined, first);
    expect(accepted.status).toBe("accepted");
    if (accepted.status !== "accepted") throw new Error("expected acceptance");

    expect(
      appendRoomFeedEvent("room-a", accepted.events, accepted.cursor, first),
    ).toEqual({ status: "duplicate" });
    expect(
      appendRoomFeedEvent(
        "room-a",
        accepted.events,
        accepted.cursor,
        event("room-a", "epoch-a", 6),
      ),
    ).toEqual({ status: "gap" });
    expect(
      appendRoomFeedEvent(
        "room-a",
        accepted.events,
        accepted.cursor,
        event("room-b", "epoch-a", 5),
      ),
    ).toEqual({ status: "gap" });
  });

  it("forces a reload across feed epochs", () => {
    expect(
      appendRoomFeedEvent(
        "room-a",
        [event("room-a", "old", 8)],
        { epoch: "old", sequence: 8 },
        event("room-a", "new", 1),
      ),
    ).toEqual({ status: "epoch_reset" });
  });

  it("retains the exact newest bounded window", () => {
    let events: RoomFeedEvent[] = [];
    let cursor: RoomFeedCursor | undefined;
    for (let sequence = 1; sequence <= MAX_RENDERED_ROOM_FEED_EVENTS + 9; sequence += 1) {
      const result = appendRoomFeedEvent(
        "room-a",
        events,
        cursor,
        event("room-a", "epoch-a", sequence),
      );
      if (result.status !== "accepted") throw new Error("unexpected rejection");
      events = result.events;
      cursor = result.cursor;
    }
    expect(events).toHaveLength(MAX_RENDERED_ROOM_FEED_EVENTS);
    expect(events[0].cursor.sequence).toBe(10);
    expect(events[events.length - 1]?.cursor.sequence).toBe(
      MAX_RENDERED_ROOM_FEED_EVENTS + 9,
    );
  });

  it("bounds the rendered feed by UTF-8 bytes as well as event count", () => {
    const source = Array.from({ length: 20 }, (_, index) =>
      event("room-a", "epoch-a", index + 1, "x".repeat(1024 * 1024)),
    );

    const retained = retainRoomFeedWindow(source);
    const retainedBytes = retained.reduce(
      (total, item) => total + new TextEncoder().encode(JSON.stringify(item)).byteLength,
      0,
    );
    expect(retained.length).toBeLessThan(source.length);
    expect(retainedBytes).toBeLessThanOrEqual(MAX_RENDERED_ROOM_FEED_BYTES);
    expect(retained[retained.length - 1]?.cursor.sequence).toBe(20);
  });
});


describe("room recovery and unsupported-wake guidance", () => {
  it("keeps structural writer holds separate from retryable storage failures", async () => {
    const structural = { writer_recovery: { kind: "structural" as const, reason: "invalid complete record" },
      history_replay_pending: true };
    expect(roomWriterRecoveryMessage(structural)).toContain("Earlier acknowledged messages");
    expect(roomHistoryReplayMessage(structural)).toContain("invalid complete record");
    expect(roomHistoryReplayMessage(structural)).not.toContain("retry the unchanged message");
    const transient = { writer_recovery: { kind: "transient" as const, reason: "disk unavailable" },
      history_replay_pending: true };
    expect(roomWriterRecoveryMessage(transient)).toBeNull();
    expect(roomHistoryReplayMessage(transient)).toContain("disk unavailable");
    expect(roomHistoryReplayMessage(transient)).toContain("retry the unchanged message");
    const failure = { kind: "history_unavailable", message: "invalid complete record" };
    expect(roomRequestFailureFeedback(failure)).toContain("No new write attempted");
    expect(roomRequestFailureFeedback(failure)).not.toContain("Retry the unchanged");
    const pending = new Map<string, string>();
    const intent = roomMessageIntent(pending, "room-a", "all", "retain uncertain original");
    await expect(submitRoomIntent(pending, intent, async () => { throw failure; })).rejects.toEqual(failure);
    expect(roomMessageIntent(pending, "room-a", "all", "retain uncertain original").requestId).toBe(intent.requestId);
  });

  it("distinguishes no new write, earlier stored intent and uncertain new append", () => {
    expect(roomRequestFailureFeedback({ kind: "recovery_required", message: "disk unavailable" })).toContain("No new write attempted");
    expect(roomRequestFailureFeedback({ kind: "request_conflict", message: "target changed" })).toContain("earlier request with this key is already stored");
    expect(roomRequestFailureFeedback({ kind: "rejected", message: "quota" }, false)).toBe("Not stored: quota");
    const brief = roomRequestFailureFeedback({ kind: "commit_uncertain", message: "sync failed" }, false);
    expect(brief).toContain("another click creates a new brief");
    expect(brief).not.toContain("Retry the unchanged");
    expect(roomRequestFailureFeedback({ kind: "recovery_required", message: "replay failed" }, false)).not.toContain("original key");
    expect(roomHistoryReplayMessage({ history_replay_pending: true })).toContain("retry the unchanged message");
    expect(roomHistoryReplayMessage({ history_replay_pending: false })).toBeNull();
    const restarted = roomHistoryReplayMessage({ history_restarted: true });
    expect(restarted).toBe("Earlier room history is unavailable. A new history was linked; room members and settings were preserved.");
    const both = roomHistoryReplayMessage({ history_restarted: true, history_replay_pending: true });
    expect(both).toContain(restarted);
    expect(both).toContain("retry the unchanged message");
  });

  it.each(["claude", "codex", "grok"] as const)("types room messages into %s members (A106)", (driver) => {
    expect(roomWakeUnavailable(driver)).toBeNull();
  });

  it.each(["prime", "generic_terminal"] as const)("does not offer impossible %s retries", (driver) => {
    const held = deliveryEvent(1, "held");
    const sessions = new Map([["a", { run_id: null, generation: 0, driver }]]);
    expect(roomDeliveryRetryUnavailable(held, ["a"], sessions)).toContain("Retry wake is unavailable");
    const feedback = roomDeliveryFeedback(deliveryResult([receipt("a", "held")]), (id) => id, () => roomWakeUnavailable(driver));
    expect(feedback.message).toContain("prompt agent members to run room_read, or provide context manually");
    expect(feedback.message).not.toContain("Use Retry wake");
  });

  it("does not direct an unsaved Held receipt to a nonexistent retry row", () => {
    const feedback = roomDeliveryFeedback(deliveryResult([receipt("a", "held", { recorded: false, error: "claim sync failed" })]), (id) => id);
    expect(feedback.message).toContain("Resolve the storage error");
    expect(feedback.message).not.toContain("Use Retry wake");
  });
});

describe("a wake typed past the busy-title cap (v3 W2 #2, Claude PRIM-1 d9613ce walk)", () => {
  it("says the title still read busy, only on a written receipt", () => {
    expect(roomDeliveryReceiptLabel({ status: "written", title_busy: "Working" }))
      .toBe(`${roomDeliveryReceiptLabel({ status: "written" })}, typed while its title still read Working`);
    expect(roomDeliveryReceiptLabel({ status: "failed", title_busy: "Working" })).toBe("Failed");
  });
});

describe("a wake accepted by the Codex App Server (v3 W4 S2, Claude PRIM-1 c01d84f8, 1214431f)", () => {
  it("names the turn, never the terminal or a byte count, and only on a written receipt", () => {
    const accepted = roomDeliveryReceiptLabel({ status: "written", app_server_turn: "019a-turn" });
    expect(accepted).toBe("Accepted by the Codex App Server · turn 019a-turn");
    expect(accepted).not.toContain("terminal");
    expect(accepted).not.toContain("bytes");
    expect(roomDeliveryReceiptLabel({ status: "written", app_server_turn: "019a-turn", already_read: true }))
      .toBe("Accepted by the Codex App Server · turn 019a-turn; all counted requests read or disposed of by receipt time");
    // A refusal, a timeout or a pending attempt never reads as accepted.
    expect(roomDeliveryReceiptLabel({ status: "failed", app_server_turn: "019a-turn" })).toBe("Failed");
    expect(roomDeliveryReceiptLabel({ status: "pending" })).toBe("Wake attempt recorded; transport outcome pending");
    expect(roomDeliveryReceiptLabel({ status: "written" })).toBe("Wake written to terminal");
  });
});

describe("an already-read pointer (v3 W3, Claude PRIM-1 26a7f92 walk)", () => {
  it("reads neutrally, is never retryable and never counts as failed", () => {
    const notTyped = { already_read: true, zero_write_confirmed: true,
      error: "the member had already read every message this wake points to; nothing was typed" };
    expect(roomDeliveryReceiptLabel({ status: "failed", already_read: true })).toBe("Not typed — nothing open to read");
    expect(roomDeliveryReceiptLabel({ status: "written", already_read: true }))
      .toBe("Wake written to terminal; all counted requests read or disposed of by receipt time");
    expect(roomDeliveryReceiptLabel({ status: "failed" })).toBe("Failed");
    expect(deliveryNotNeeded({ status: "failed", already_read: true })).toBe(true);
    expect(deliveryNotNeeded({ status: "written", already_read: true })).toBe(false);
    expect(retryableRoomDeliveryEvents("room-a", [deliveryEvent(1, "failed", notTyped)]).size).toBe(0);
    const feedback = roomDeliveryFeedback(deliveryResult([receipt("a", "failed", notTyped)]), (id) => id);
    expect(feedback.level).toBe("info");
    expect(feedback.summary).toBe("Stored in the room feed. At send: 1 not typed, already read.");
    expect(feedback.summary).not.toContain("failed");
    expect(feedback.message).toContain("a: Not typed — nothing open to read");
  });
});

describe("history names who wrote and who was addressed (Claude PRIM-1 ef3870d walk)", () => {
  const message = (overrides: Partial<Extract<RoomFeedEvent["item"], { kind: "message" }>>) => ({
    kind: "message" as const, message_id: "m", request_id: "r", sender: { kind: "operator" as const },
    content: "x", recipient_ids: [] as string[], membership_revision: 1, ...overrides,
  });
  const member = (id: string) => (id === "s-1" ? "Claude J" : id);

  it("attributes a PRIM-1 notice to PRIM-1, never to the operator", () => {
    expect(roomNoticeSenderLabel(message({ sender: { kind: "prim1" } }))).toBe("PRIM-1");
    expect(roomNoticeSenderLabel(message({}))).toBeNull();
  });

  it("names guest recipients, and says feed only only when nobody was addressed", () => {
    const invited = new Set(["g-1"]);
    expect(roomMessageRecipientsLabel(message({ guest_recipient_ids: ["g-1"], guest_recipient_labels: ["Driver"] }),
      member, invited)).toBe("Driver (guest)");
    expect(roomMessageRecipientsLabel(message({ recipient_ids: ["s-1"], guest_recipient_ids: ["g-2"],
      guest_recipient_labels: ["Scout"] }), member, invited)).toBe("Claude J, Scout (guest, revoked)");
    expect(roomMessageRecipientsLabel(message({}), member, invited)).toBe("feed only");
  });

  it("keeps a guest's name in history and marks one no longer invited as revoked", () => {
    expect(roomGuestLabel("a9c237af-0000", "Driver", new Set())).toBe("Driver (guest, revoked)");
    expect(roomGuestLabel("a9c237af-0000", "Driver", new Set(["a9c237af-0000"]))).toBe("Driver (guest)");
    expect(roomGuestLabel("a9c237af-0000", "", null)).toBe("guest a9c237af (guest)");
  });
});

describe("a withdrawn message (v3 W3 D6)", () => {
  it("offers no Retry for any recipient once the operator withdrew it", () => {
    const held = deliveryEvent(1, "held", { error: "withdrawn: the operator cancelled this message (wrong room); it is not typed" });
    const cancel: RoomFeedEvent = { ...deliveryEvent(2, "held"), item: { kind: "cancel", request_id: "c-1",
      message_id: "message-a", actor: { kind: "operator" }, reason: "wrong room" } };
    expect(retryableRoomDeliveryEvents("room-a", [held]).size).toBe(1);
    expect(retryableRoomDeliveryEvents("room-a", [held, cancel]).size).toBe(0);
  });
});

describe("open obligations for the operator's disposition (v3 W3 b)", () => {
  const at = (sequence: number, item: RoomFeedEvent["item"]): RoomFeedEvent =>
    ({ ...deliveryEvent(sequence, "held"), item });
  const asked = (id: string, extra: Record<string, unknown> = {}) => at(1, { kind: "message", message_id: id, request_id: `r-${id}`,
    sender: { kind: "operator" }, content: "x", recipient_ids: ["s-1", "s-2"], membership_revision: 1, ack_requested: true, ...extra });
  const label = (id: string) => ({ "s-1": "Claude J", "s-2": "Codex J" } as Record<string, string>)[id] ?? id;

  it("lists who still owes a message, members and guests, and nothing for a withdrawn one", () => {
    const events = [
      asked("m-1", { guest_recipient_ids: ["g-1"], guest_recipient_labels: ["Driver"] }),
      at(2, { kind: "ack", request_id: "a-1", message_id: "m-1", recipient_id: "s-1", actor: { kind: "session", session_id: "s-1" },
        disposition: "acked", note: "on it" }),
      asked("m-2"),
      at(4, { kind: "cancel", request_id: "c-1", message_id: "m-2", actor: { kind: "operator" }, reason: "wrong room" }),
      at(5, { kind: "message", message_id: "m-3", request_id: "r-3", sender: { kind: "operator" }, content: "x",
        recipient_ids: ["s-1"], membership_revision: 1 }),
    ];
    const open = roomOpenObligations(events, label, new Set(["g-1"]));
    expect([...open.keys()]).toEqual(["m-1"]);
    expect(open.get("m-1")).toEqual([{ key: "s-2", label: "Codex J" }, { key: "g-1", label: "Driver (guest)" }]);
  });
});

describe("a withdrawal says who it already reached (v3 W3 D6)", () => {
  const label = (id: string) => ({ "s-1": "Claude J", "s-2": "Codex J" } as Record<string, string>)[id] ?? id;
  it("never claims to undo a delivery", () => {
    expect(roomWithdrawalText({ not_typed: ["s-2"], reached: ["s-1"] }, label))
      .toBe("Not typed: Codex J. Already delivered to Claude J; it does not undo what they did.");
    expect(roomWithdrawalText({}, label)).toBe("");
  });

  // Claude PRIM-1, ac9f860 walk: since a41d8d4 the reached members are woken
  // by a PRIM-1 notice; the form and the row say so, never "no one is woken".
  it("says PRIM-1 notifies the members it reached, and who has seen the notice", () => {
    expect(roomWithdrawPreview(["Claude J"]))
      .toBe("Already delivered to Claude J; PRIM-1 notifies them now; it does not undo what they did.");
    expect(roomWithdrawPreview([])).toBe("Not delivered to anyone yet.");
    const at = (sequence: number, item: RoomFeedItem): RoomFeedEvent =>
      ({ schema_version: 1, room_id: "r", cursor: { epoch: "e", sequence }, timestamp: "t", item });
    const notice = at(2, { kind: "message", message_id: "n-1", request_id: "withdrawal-w-1", sender: { kind: "prim1" },
      content: "[PRIM-1] The operator withdrew message m-1", recipient_ids: ["s-1", "s-2"], membership_revision: 1 } as RoomFeedItem);
    const fetched = at(3, { kind: "delivery", message_id: "n-1", recipient_id: "s-1", status: "fetched",
      bytes_written: 0, error: null, zero_write_confirmed: false } as RoomFeedItem);
    expect(roomWithdrawalNotice([notice], "w-1")).toEqual({ recipients: ["s-1", "s-2"], seen: [] });
    expect(roomWithdrawalNotice([notice, fetched], "w-1")).toEqual({ recipients: ["s-1", "s-2"], seen: ["s-1"] });
    expect(roomWithdrawalNotice([notice, fetched], "w-2")).toBeNull();
    expect(roomWithdrawalText({ reached: ["s-1", "s-2"] }, label, roomWithdrawalNotice([notice, fetched], "w-1")))
      .toBe("Already delivered to Claude J, Codex J; PRIM-1 notified Claude J, Codex J (seen by Claude J); it does not undo what they did.");
    const operatorNamed = at(2, { ...notice.item, sender: { kind: "operator" } } as RoomFeedItem);
    expect(roomWithdrawalNotice([operatorNamed], "w-1")).toBeNull();
  });

  it("lists who already has a message before the operator withdraws it", () => {
    const written = deliveryEvent(1, "written", { recipient_id: "s-1", bytes_written: 120 });
    const held = deliveryEvent(2, "held", { recipient_id: "s-2" });
    expect(roomMessageReached([written, held], "message-a")).toEqual(["s-1"]);
  });
});

