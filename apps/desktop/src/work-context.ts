import type { RoomSnapshot, SessionSnapshot } from "./types";

/** Where the operator is standing: the lobby (sessions in no room) or one
    room. The top bar, grid, zero-state and keyboard order show only the
    current context's sessions. Every other context's panes stay mounted and
    its harnesses keep running — leaving a room never stops its agents
    (Rooms B1 invariant 1). */
export type WorkContext = { kind: "lobby" } | { kind: "room"; roomId: string };

export function contextKey(context: WorkContext): string {
  return context.kind === "lobby" ? "lobby" : "room:" + context.roomId;
}

/** Restore a persisted context; anything malformed falls back to the lobby. */
export function parseWorkContext(raw: string | null): WorkContext {
  if (!raw) {
    return { kind: "lobby" };
  }
  try {
    const parsed = JSON.parse(raw) as { kind?: unknown; roomId?: unknown };
    if (
      parsed
      && parsed.kind === "room"
      && typeof parsed.roomId === "string"
      && parsed.roomId.length > 0
    ) {
      return { kind: "room", roomId: parsed.roomId };
    }
  } catch {
    // malformed storage falls back to the lobby
  }
  return { kind: "lobby" };
}

export function sessionRoomId(
  rooms: readonly RoomSnapshot[],
  sessionId: string,
): string | null {
  for (const room of rooms) {
    if (room.member_ids.includes(sessionId)) {
      return room.room_id;
    }
  }
  return null;
}

export function sessionInContext(
  rooms: readonly RoomSnapshot[],
  sessionId: string,
  context: WorkContext,
): boolean {
  const roomId = sessionRoomId(rooms, sessionId);
  return context.kind === "lobby" ? roomId === null : roomId === context.roomId;
}

export function visibleSessionOrder(
  order: readonly string[],
  rooms: readonly RoomSnapshot[],
  context: WorkContext,
): string[] {
  return order.filter((sessionId) => sessionInContext(rooms, sessionId, context));
}

export function contextDisplayLabel(
  rooms: readonly RoomSnapshot[],
  context: WorkContext,
): string {
  if (context.kind === "lobby") {
    return "Lobby";
  }
  return rooms.find((room) => room.room_id === context.roomId)?.label ?? "Room";
}

export interface SessionReadiness {
  kind: "working" | "ready" | "starting" | "blocked" | "off";
  reason: string;
}

/** One dot per member: what it is doing — and when blocked, why, in plain
    words the operator can act on. */
export function sessionReadiness(
  session: SessionSnapshot | undefined,
  resumeOwed: boolean,
): SessionReadiness {
  if (!session) {
    return { kind: "off", reason: "Unknown session" };
  }
  if (!session.running) {
    if (resumeOwed) {
      return { kind: "off", reason: "Not running — resumes when you enter its room" };
    }
    return { kind: "off", reason: "Not running" };
  }
  switch (session.lifecycle_state) {
    case "busy":
      return { kind: "working", reason: "Working" };
    case "ready":
    case "idle":
      return { kind: "ready", reason: "Idle — ready for input" };
    case "starting":
    case "restarting":
      return { kind: "starting", reason: "Starting" };
    case "stalled":
      return { kind: "blocked", reason: "Stalled — no output for a while" };
    case "failed":
      return {
        kind: "blocked",
        reason: session.last_error ? "Failed: " + session.last_error : "Failed",
      };
    case "closed":
      return { kind: "off", reason: "Closed" };
    default:
      return {
        kind: "off",
        reason: String(session.lifecycle_state).replace(/_/g, " "),
      };
  }
}
