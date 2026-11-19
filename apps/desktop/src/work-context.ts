import type { RoomSnapshot, SessionSnapshot, StartSessionRequest } from "./types";
import { isSleeping } from "./hibernation";
import { appServerFact } from "./session-tabs";

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
    words the operator can act on. v3 W4 S5: a running App Server member's
    connection fact follows the reason; it never changes the dot's kind or
    replaces a hold, failure or recovery reason. */
export function sessionReadiness(
  session: SessionSnapshot | undefined,
  wasRunningAtShutdown: boolean,
): SessionReadiness {
  const readiness = memberReadiness(session, wasRunningAtShutdown);
  const appServer = session ? appServerFact(session) : null;
  return appServer ? { ...readiness, reason: readiness.reason + " · " + appServer } : readiness;
}

function memberReadiness(
  session: SessionSnapshot | undefined,
  wasRunningAtShutdown: boolean,
): SessionReadiness {
  if (!session) {
    return { kind: "off", reason: "Unknown session" };
  }
  if (session.output_draining) {
    return { kind: "blocked", reason: "Draining output — input closed. Stop remains available." };
  }
  if (session.lifecycle_state === "failed") {
    return {
      kind: "blocked",
      reason: session.last_error ? "Failed: " + session.last_error : "Failed",
    };
  }
  if (!session.running) {
    if (session.recovery) return { kind: "off", reason: asleepReason(session) };
    if (session.hibernated && session.last_error) return { kind: "blocked", reason: `Stopped: ${session.last_error}` };
    if (isSleeping(session)) return { kind: "off", reason: "Sleeping — wakes for new room messages. Stop keeps it stopped." };
    if (wasRunningAtShutdown) {
      return { kind: "off", reason: "Asleep — was running when PRIM-1 closed" };
    }
    return { kind: "off", reason: asleepReason(session) };
  }
  // A question on its screen (trust, approval) leaves the lifecycle at
  // ready or idle: the member waits on the operator, not ready for input.
  if (session.blocked_reason) {
    return { kind: "blocked", reason: "Blocked: " + session.blocked_reason };
  }
  switch (session.lifecycle_state) {
    case "busy":
      return { kind: "working", reason: "Working" };
    case "ready":
    case "idle":
      // The lifecycle stays ready while a member works; its observed work
      // state tells working from waiting.
      if (session.work_state === "thinking" || session.work_state === "tool_call") {
        // v3 W2: say what is known when only the member's own record shows it busy.
        return { kind: "working", reason: session.work_state_from_record
          ? "Working (turn open in its own record)" : "Working" };
      }
      // A16 D5 (PC3, 20 Oct: a member started with no brief sat silent and
      // was stopped after 6 s as if it had not started): say it waits.
      return { kind: "ready", reason: "Ready — waiting for a task" };
    case "starting":
    case "restarting":
      return { kind: "starting", reason: "Starting" };
    case "stalled":
      return { kind: "blocked", reason: "Stalled — no output for a while" };
    case "closed":
      return { kind: "off", reason: "Closed" };
    default:
      return {
        kind: "off",
        reason: String(session.lifecycle_state).replace(/_/g, " "),
      };
  }
}

/** What waking an asleep member does: resume its conversation, show its
    hold (the reason follows), or start fresh. Loaded rooms (A92) rely on it. */
function asleepReason(session: SessionSnapshot): string {
  if (session.recovery) {
    const kind = session.recovery.provenance === "room_import_history_missing" ? "history needed" : "held";
    return `Asleep — ${kind}: ${session.recovery.reason}`;
  }
  return session.resume_available ? "Asleep — resumes its conversation" : "Asleep — starts fresh";
}

/** After a room inventory refresh: the standing room context wins (a gone
    room drops it to the lobby); otherwise a live selection stays, else the
    first room. Loading a room must therefore enter it as the context. */
export function roomSelectionAfterSync(
  context: WorkContext,
  active: string | null,
  liveIds: ReadonlySet<string>,
  first: string | null,
): { context: WorkContext; active: string | null; contextLost: boolean } {
  const contextLost = context.kind === "room" && !liveIds.has(context.roomId);
  const next: WorkContext = contextLost ? { kind: "lobby" } : context;
  if (next.kind === "room") return { context: next, active: next.roomId, contextLost };
  return { context: next, active: active !== null && liveIds.has(active) ? active : first, contextLost };
}

/** Who comes back after a restart (Victor, 13 Oct; PLAN §5-3, E8): members of
    rooms marked Resume on restart that were running when PRIM-1 closed, the
    foreground member first. Everyone else that was running stays asleep —
    the last-visited room and busy agents get no special treatment. */
export function restartResumePlan(
  rooms: readonly RoomSnapshot[],
  sessions: ReadonlyMap<string, SessionSnapshot>,
  foregroundId: string | null,
): { resume: string[]; leftAsleep: string[] } {
  const wasRunning = (sessionId: string): boolean => {
    const session = sessions.get(sessionId);
    return Boolean(session && session.was_running_at_shutdown && !session.running && !session.hibernated);
  };
  const resume: string[] = [];
  for (const room of rooms) {
    if (!room.resume_on_restart) {
      continue;
    }
    for (const sessionId of room.member_ids) {
      if (wasRunning(sessionId) && !resume.includes(sessionId)) {
        resume.push(sessionId);
      }
    }
  }
  if (foregroundId && resume.includes(foregroundId)) {
    resume.splice(resume.indexOf(foregroundId), 1);
    resume.unshift(foregroundId);
  }
  const leftAsleep = Array.from(sessions.keys()).filter(
    (sessionId) => wasRunning(sessionId) && !resume.includes(sessionId),
  );
  return { resume, leftAsleep };
}

export type BackgroundResumeMode = "boot" | "manual";
export interface BackgroundResumeEntry { sessionId: string; mode: BackgroundResumeMode }

/** Keep the existing queue order and deduplication. A manual request never inherits boot authority. */
export function enqueueBackgroundResumes(
  queue: BackgroundResumeEntry[], inFlight: ReadonlySet<string>, sessionIds: readonly string[],
  mode: BackgroundResumeMode = "manual",
): void {
  for (const sessionId of sessionIds) {
    if (inFlight.has(sessionId)) continue;
    const queued = queue.find((entry) => entry.sessionId === sessionId);
    if (queued) {
      if (mode === "manual") queued.mode = "manual";
    } else queue.push({ sessionId, mode });
  }
}

/** Only C/C/G have an exact conversation contract; native rechecks boot eligibility.
    Failed boot starts retain only an ordinary Start request for retry. */
export function resumeLaunchCommand(request: StartSessionRequest, mode: BackgroundResumeMode = "manual", driver?: string):
  { name: "resume_session_after_restart"; args: { sessionId: string } }
  | { name: "start_session"; args: { request: StartSessionRequest } } {
  const exactDriver = driver === "claude" || driver === "codex" || driver === "grok";
  if (mode === "boot" && exactDriver && !request.fresh && !request.search && !request.launch_choices) {
    return { name: "resume_session_after_restart", args: { sessionId: request.session_id } };
  }
  return { name: "start_session", args: { request } };
}

/** PRIM-1's own room posts (A31) go out as operator posts. Their request ids
    carry this prefix — the operator's posts carry random UUIDs — so the
    Readable feed can name PRIM-1, not "You", as their sender. */
export const PRIM1_POST_REQUEST_PREFIX = "prim1-";
export const PRIM1_NOTICE_PREFIX = "[PRIM-1 automatic notice]";

/** A31: after a restart, one notice per resumed room tells its agents who is
    back online and names anyone who did not come back. Stored in the feed, not
    typed into a terminal; agents read it when they next read the room. */
export function restartNotice(
  roomLabel: string,
  backOnline: readonly string[],
  notBack: readonly string[],
): string {
  const parts = [
    `${PRIM1_NOTICE_PREFIX} PRIM-1 restarted. Back online in ${roomLabel}: ${
      backOnline.length > 0 ? backOnline.join(", ") : "nobody"
    }.`,
  ];
  if (notBack.length > 0) {
    parts.push(`Did not come back: ${notBack.join(", ")} (see each pane).`);
  }
  parts.push("Read the room before you continue.");
  return parts.join(" ");
}

