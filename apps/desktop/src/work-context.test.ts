import { describe, expect, it } from "vitest";

import type { RoomSnapshot, SessionSnapshot } from "./types";
import {
  contextDisplayLabel,
  contextKey,
  parseWorkContext,
  sessionInContext,
  sessionReadiness,
  sessionRoomId,
  visibleSessionOrder,
} from "./work-context";

function room(roomId: string, label: string, memberIds: string[]): RoomSnapshot {
  return {
    room_id: roomId,
    label,
    member_ids: memberIds,
    membership_revision: 1,
    feed_epoch: "e",
    feed_oldest_sequence: 1,
    feed_next_sequence: 1,
  };
}

function session(overrides: Partial<SessionSnapshot>): SessionSnapshot {
  return {
    session_id: "s1",
    alias: "session-s1",
    label: "Claude",
    driver: "claude",
    permission_profile: "normal",
    lifecycle_state: "ready",
    working_dir: "C:\\work",
    generation: 1,
    run_id: null,
    run_event_sequence: 0,
    process_id: null,
    running: true,
    resume_available: false,
    was_running_at_shutdown: false,
    last_activity_at: null,
    last_error: null,
    ...overrides,
  };
}

const rooms = [room("r1", "Website", ["a", "b"]), room("r2", "Songs", ["c"])];

describe("parseWorkContext", () => {
  it("restores a room context and falls back to the lobby on anything malformed", () => {
    expect(parseWorkContext(JSON.stringify({ kind: "room", roomId: "r1" }))).toEqual({
      kind: "room",
      roomId: "r1",
    });
    for (const raw of [
      null,
      "",
      "not json",
      JSON.stringify({ kind: "room" }),
      JSON.stringify({ kind: "room", roomId: 7 }),
      JSON.stringify({ kind: "room", roomId: "" }),
      JSON.stringify({ kind: "elsewhere", roomId: "r1" }),
      JSON.stringify({ kind: "lobby" }),
    ]) {
      expect(parseWorkContext(raw).kind).toBe("lobby");
    }
  });
});

describe("context membership", () => {
  it("maps sessions to their one room or the lobby", () => {
    expect(sessionRoomId(rooms, "a")).toBe("r1");
    expect(sessionRoomId(rooms, "c")).toBe("r2");
    expect(sessionRoomId(rooms, "loner")).toBeNull();

    expect(sessionInContext(rooms, "a", { kind: "room", roomId: "r1" })).toBe(true);
    expect(sessionInContext(rooms, "a", { kind: "room", roomId: "r2" })).toBe(false);
    expect(sessionInContext(rooms, "a", { kind: "lobby" })).toBe(false);
    expect(sessionInContext(rooms, "loner", { kind: "lobby" })).toBe(true);
  });

  it("filters the tab order to the standing context, preserving order", () => {
    const order = ["loner", "a", "c", "b"];
    expect(visibleSessionOrder(order, rooms, { kind: "room", roomId: "r1" })).toEqual([
      "a",
      "b",
    ]);
    expect(visibleSessionOrder(order, rooms, { kind: "lobby" })).toEqual(["loner"]);
    expect(visibleSessionOrder(order, rooms, { kind: "room", roomId: "gone" })).toEqual([]);
  });

  it("labels contexts by room label, with honest fallbacks", () => {
    expect(contextDisplayLabel(rooms, { kind: "lobby" })).toBe("Lobby");
    expect(contextDisplayLabel(rooms, { kind: "room", roomId: "r2" })).toBe("Songs");
    expect(contextDisplayLabel(rooms, { kind: "room", roomId: "gone" })).toBe("Room");
    expect(contextKey({ kind: "room", roomId: "r2" })).toBe("room:r2");
    expect(contextKey({ kind: "lobby" })).toBe("lobby");
  });
});

describe("sessionReadiness", () => {
  it("maps lifecycle to a dot with a plain reason", () => {
    expect(sessionReadiness(session({ lifecycle_state: "busy" }), false)).toEqual({
      kind: "working",
      reason: "Working",
    });
    expect(sessionReadiness(session({ lifecycle_state: "idle" }), false).kind).toBe("ready");
    expect(sessionReadiness(session({ lifecycle_state: "starting" }), false).kind).toBe(
      "starting",
    );
    expect(sessionReadiness(session({ lifecycle_state: "stalled" }), false)).toEqual({
      kind: "blocked",
      reason: "Stalled — no output for a while",
    });
  });

  it("names the failure when a session failed, and the owed resume when stopped", () => {
    expect(
      sessionReadiness(
        session({ lifecycle_state: "failed", last_error: "exit code 1" }),
        false,
      ),
    ).toEqual({ kind: "blocked", reason: "Failed: exit code 1" });
    expect(sessionReadiness(session({ running: false }), true)).toEqual({
      kind: "off",
      reason: "Not running — resumes when you enter its room",
    });
    expect(sessionReadiness(session({ running: false }), false).reason).toBe("Not running");
    expect(sessionReadiness(undefined, false).kind).toBe("off");
  });
});
