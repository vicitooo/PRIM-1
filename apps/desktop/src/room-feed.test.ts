import { describe, expect, it } from "vitest";

import {
  MAX_RENDERED_ROOM_FEED_BYTES,
  MAX_RENDERED_ROOM_FEED_EVENTS,
  appendRoomFeedEvent,
  retainRoomFeedWindow,
} from "./room-feed";
import type { RoomFeedCursor, RoomFeedEvent } from "./types";

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
      sender: { kind: "operator" },
      content,
      recipient_ids: [],
      membership_revision: 1,
    },
    timestamp: "2026-08-11T00:00:00Z",
  };
}

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
