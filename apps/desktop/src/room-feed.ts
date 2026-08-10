import type { RoomFeedCursor, RoomFeedEvent } from "./types";

export const MAX_RENDERED_ROOM_FEED_EVENTS = 512;
export const MAX_RENDERED_ROOM_FEED_BYTES = 16 * 1024 * 1024;

const encoder = new TextEncoder();
const measuredBytes = new WeakMap<RoomFeedEvent, number>();

function eventBytes(event: RoomFeedEvent): number {
  const cached = measuredBytes.get(event);
  if (cached !== undefined) {
    return cached;
  }
  const bytes = encoder.encode(JSON.stringify(event)).byteLength;
  measuredBytes.set(event, bytes);
  return bytes;
}

export function retainRoomFeedWindow(
  events: readonly RoomFeedEvent[],
): RoomFeedEvent[] {
  let retainedBytes = 0;
  let start = events.length;
  while (start > 0 && events.length - start < MAX_RENDERED_ROOM_FEED_EVENTS) {
    const candidate = events[start - 1];
    const candidateBytes = eventBytes(candidate);
    if (retainedBytes + candidateBytes > MAX_RENDERED_ROOM_FEED_BYTES) {
      break;
    }
    retainedBytes += candidateBytes;
    start -= 1;
  }
  return events.slice(start);
}

export type RoomFeedAppendResult =
  | {
      status: "accepted";
      events: RoomFeedEvent[];
      cursor: RoomFeedCursor;
    }
  | { status: "duplicate" }
  | { status: "gap" | "epoch_reset" };

export function appendRoomFeedEvent(
  roomId: string,
  events: readonly RoomFeedEvent[],
  cursor: RoomFeedCursor | undefined,
  event: RoomFeedEvent,
): RoomFeedAppendResult {
  if (event.room_id !== roomId) {
    return { status: "gap" };
  }
  if (cursor) {
    if (cursor.epoch !== event.cursor.epoch) {
      return { status: "epoch_reset" };
    }
    if (event.cursor.sequence <= cursor.sequence) {
      return { status: "duplicate" };
    }
    if (event.cursor.sequence !== cursor.sequence + 1) {
      return { status: "gap" };
    }
  }
  const retained = retainRoomFeedWindow([...events, event]);
  return {
    status: "accepted",
    events: retained,
    cursor: event.cursor,
  };
}
