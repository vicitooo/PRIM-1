import { describe, expect, it } from "vitest";

import type { RoomSnapshot, SessionSnapshot } from "./types";
import {
  contextDisplayLabel,
  contextKey,
  parseWorkContext,
  sessionInContext,
  restartNotice,
  restartResumePlan,
  enqueueBackgroundResumes,
  resumeLaunchCommand,
  type BackgroundResumeEntry,
  sessionReadiness,
  sessionRoomId,
  visibleSessionOrder,
  roomSelectionAfterSync,
} from "./work-context";

function room(
  roomId: string,
  label: string,
  memberIds: string[],
  resumeOnRestart = false,
): RoomSnapshot {
  return {
    room_id: roomId,
    label,
    resume_on_restart: resumeOnRestart,
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
    launch_choices: { model: null, effort: null, brief: { mode: "room_default" } },
    recovery: null,
    recovery_history: [],
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
    last_error_kind: null,
    ...overrides,
  };
}

const rooms = [room("r1", "Website", ["a", "b"]), room("r2", "Songs", ["c"])];

describe("background resume command origin", () => {
  it("keeps mixed boot/manual queue order and never gives a manual request boot continuation authority", () => {
    const queue: BackgroundResumeEntry[] = [];
    const inFlight = new Set(["starting"]);
    enqueueBackgroundResumes(queue, inFlight, ["a", "b", "a", "starting"], "boot");
    enqueueBackgroundResumes(queue, inFlight, ["b", "c", "starting"]);
    // A later duplicate boot batch cannot promote an explicitly manual entry.
    enqueueBackgroundResumes(queue, inFlight, ["b", "c"], "boot");
    expect(queue.map((entry) => [entry.sessionId, resumeLaunchCommand({ session_id: entry.sessionId }, entry.mode, "claude")]))
      .toEqual([
        ["a", { name: "resume_session_after_restart", args: { sessionId: "a" } }],
        ["b", { name: "start_session", args: { request: { session_id: "b" } } }],
        ["c", { name: "start_session", args: { request: { session_id: "c" } } }],
      ]);
  });

  it("keeps ordinary Start and explicit Fresh/Search retries on start_session", () => {
    for (const request of [{ session_id: "a" }, { session_id: "a", fresh: true }, { session_id: "a", search: true }]) {
      expect(resumeLaunchCommand(request)).toEqual({ name: "start_session", args: { request } });
    }
    for (const request of [{ session_id: "a", fresh: true }, { session_id: "a", search: true }]) {
      expect(resumeLaunchCommand(request, "boot", "claude").name).toBe("start_session");
    }
  });

  it("uses exact boot resume only for Claude/Codex/Grok and preserves ordinary boot launch for other or unknown drivers", () => {
    const request = { session_id: "a" };
    for (const driver of ["claude", "codex", "grok"]) {
      expect(resumeLaunchCommand(request, "boot", driver)).toEqual({ name: "resume_session_after_restart", args: { sessionId: "a" } });
      expect(resumeLaunchCommand(request, "manual", driver)).toEqual({ name: "start_session", args: { request } });
    }
    for (const driver of ["generic_terminal", "prime", "future_driver", "", undefined]) {
      expect(resumeLaunchCommand(request, "boot", driver)).toEqual({ name: "start_session", args: { request } });
    }
  });
});

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
  it("adds the App Server fact after the reason, never replacing a hold (v3 W4 S5)", () => {
    const followed = session({ lifecycle_state: "idle", running: true, codex_transport: "app_server", app_server: "followed" });
    expect(sessionReadiness(followed, false)).toEqual({
      kind: "ready", reason: "Ready — waiting for a task · App Server: PRIM-1 follows this member's thread",
    });
    const held = { ...followed, blocked_reason: "approval waits in its terminal" };
    expect(sessionReadiness(held, false).kind).toBe("blocked");
    expect(sessionReadiness(held, false).reason).toMatch(/^Blocked: approval waits in its terminal · App Server: /);
    const lost = { ...followed, app_server: "not_followed" as const };
    expect(sessionReadiness(lost, false).kind).toBe("ready");
    expect(sessionReadiness(lost, false).reason).toContain("not followed");
    expect(sessionReadiness({ ...followed, running: false, lifecycle_state: "closed" }, false).reason).not.toContain("App Server");
  });

  it("distinguishes native sleeping from an ordinary stopped member and preserves holds/errors", () => {
    const asleep = session({ running: false, lifecycle_state: "closed", hibernated: true, resume_available: true });
    expect(sessionReadiness(asleep, true).reason).toContain("Sleeping — wakes for new room messages");
    expect(sessionReadiness({ ...asleep, hibernated: false }, false).reason).toBe("Asleep — resumes its conversation");
    expect(sessionReadiness({ ...asleep, recovery: { original_id: "id", provenance: "hold", reason: "Held elsewhere" } }, true).reason).toContain("held: Held elsewhere");
    expect(sessionReadiness({ ...asleep, last_error: "resume failed" }, true)).toEqual({ kind: "blocked", reason: "Stopped: resume failed" });
  });

  it("overrides previously idle readiness while output drains, without losing Stop ownership", () => {
    const idle = session({ lifecycle_state: "idle", running: true });
    expect(sessionReadiness(idle, false).kind).toBe("ready");
    const draining = { ...idle, output_draining: true };
    expect(sessionReadiness(draining, false)).toEqual({
      kind: "blocked", reason: "Draining output — input closed. Stop remains available.",
    });
    expect(draining.running).toBe(true);
    expect(draining.lifecycle_state).toBe("idle");
  });

  it("names a question on screen instead of ready, whatever the lifecycle says (19 Oct walk)", () => {
    const line = "it is asking whether to trust the folder — answer that question in its terminal, then send again";
    for (const lifecycle_state of ["ready", "idle", "starting"] as const) {
      expect(sessionReadiness(session({ lifecycle_state, running: true, blocked_reason: line }), false)).toEqual({
        kind: "blocked",
        reason: "Blocked: " + line,
      });
    }
    expect(sessionReadiness(session({ lifecycle_state: "idle", running: true, blocked_reason: null }), false)).toEqual({
      kind: "ready",
      reason: "Ready — waiting for a task",
    });
    // A16 D5 (Reviewer, 20 Oct): the lifecycle stays ready while a member works.
    for (const lifecycle_state of ["ready", "idle"] as const) {
      for (const work_state of ["thinking", "tool_call"] as const) {
        expect(sessionReadiness(session({ lifecycle_state, running: true, work_state }), false))
          .toEqual({ kind: "working", reason: "Working" });
      }
      for (const work_state of ["idle", null, undefined] as const) {
        expect(sessionReadiness(session({ lifecycle_state, running: true, work_state }), false))
          .toEqual({ kind: "ready", reason: "Ready — waiting for a task" });
      }
      // v3 W2: busy only from the member's own record says so.
      expect(sessionReadiness(session({ lifecycle_state, running: true, work_state: "thinking",
        work_state_from_record: true }), false))
        .toEqual({ kind: "working", reason: "Working (turn open in its own record)" });
    }
    expect(sessionReadiness(session({ running: false, lifecycle_state: "closed", blocked_reason: line }), false).kind)
      .toBe("off");
  });

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

  it("names the failure when a session failed, and asleep when stopped", () => {
    expect(
      sessionReadiness(
        session({
          running: false,
          lifecycle_state: "failed",
          last_error: "Can't find claude.exe on PATH.",
        }),
        false,
      ),
    ).toEqual({
      kind: "blocked",
      reason: "Failed: Can't find claude.exe on PATH.",
    });
    expect(
      sessionReadiness(
        session({ lifecycle_state: "failed", last_error: "exit code 1" }),
        false,
      ),
    ).toEqual({ kind: "blocked", reason: "Failed: exit code 1" });
    expect(sessionReadiness(session({ running: false }), true)).toEqual({
      kind: "off",
      reason: "Asleep — was running when PRIM-1 closed",
    });
    expect(sessionReadiness(session({ running: false }), false).reason).toBe("Asleep — starts fresh");
    expect(sessionReadiness(session({ running: false, resume_available: true }), false).reason)
      .toBe("Asleep — resumes its conversation");
    const hold = (provenance: string) => ({ original_id: "c1", provenance, reason: "Its conversation is not here." });
    expect(sessionReadiness(session({ running: false, recovery: hold("room_import_history_missing") }), false).reason)
      .toBe("Asleep — history needed: Its conversation is not here.");
    expect(sessionReadiness(session({ running: false, recovery: hold("room_import_in_use") }), false))
      .toEqual({ kind: "off", reason: "Asleep — held: Its conversation is not here." });
    expect(sessionReadiness(undefined, false).kind).toBe("off");
  });
});

describe("restartResumePlan (E8: only the rooms you mark come back)", () => {
  const wasRunning = (sessionId: string, overrides: Partial<SessionSnapshot> = {}) =>
    session({ session_id: sessionId, running: false, was_running_at_shutdown: true, ...overrides });
  const sessions = (...list: SessionSnapshot[]) =>
    new Map(list.map((entry) => [entry.session_id, entry] as const));

  it("does not wake hibernated members through ordinary restart resume or its room prompt", () => {
    const plan = restartResumePlan(
      [room("r1", "Marked", ["sleeping", "interrupted"], true), room("r2", "Unmarked", ["also-sleeping"])],
      sessions(wasRunning("sleeping", { hibernated: true }), wasRunning("interrupted"),
        wasRunning("also-sleeping", { hibernated: true })),
      "sleeping",
    );
    expect(plan.resume).toEqual(["interrupted"]);
    expect(plan.leftAsleep).toEqual([]);
  });

  it("resumes only marked rooms' members that were running, foreground first", () => {
    const plan = restartResumePlan(
      [room("r1", "Marked", ["a", "b"], true), room("r2", "Unmarked", ["c"]), room("r3", "Marked too", ["d"], true)],
      sessions(wasRunning("a"), wasRunning("b"), wasRunning("c"), wasRunning("d")),
      "d",
    );
    expect(plan.resume).toEqual(["d", "a", "b"]);
    expect(plan.leftAsleep).toEqual(["c"]);
  });

  it("an empty selection resumes nobody, whatever was last visited or busy", () => {
    const plan = restartResumePlan(
      [room("r1", "Last visited", ["a"]), room("r2", "Busy", ["b"])],
      sessions(wasRunning("a"), wasRunning("b", { lifecycle_state: "busy" })),
      "a",
    );
    expect(plan.resume).toEqual([]);
    expect(plan.leftAsleep).toEqual(["a", "b"]);
  });

  it("skips members already running, stopped before shutdown, or outside every room", () => {
    const plan = restartResumePlan(
      [room("r1", "Marked", ["a", "b", "c"], true)],
      sessions(
        wasRunning("a", { running: true }),
        session({ session_id: "b", running: false, was_running_at_shutdown: false }),
        wasRunning("c"),
        wasRunning("lobby"),
      ),
      null,
    );
    expect(plan.resume).toEqual(["c"]);
    expect(plan.leftAsleep).toEqual(["lobby"]);
  });
});

describe("restartNotice (A31)", () => {
  it("names who is back and who did not come back", () => {
    expect(restartNotice("PRIM-1 Upgrade", ["Claude", "Codex"], ["Grok"])).toBe(
      "[PRIM-1 automatic notice] PRIM-1 restarted. Back online in PRIM-1 Upgrade: Claude, Codex. " +
        "Did not come back: Grok (see each pane). Read the room before you continue.",
    );
  });

  it("says nobody when no resume succeeded", () => {
    expect(restartNotice("Video", [], ["Claude"])).toContain("Back online in Video: nobody.");
  });
});


describe("roomSelectionAfterSync", () => {
  it("keeps the context room, drops a gone one to the lobby, else keeps a live selection", () => {
    const live = new Set(["a", "b"]);
    expect(roomSelectionAfterSync({ kind: "room", roomId: "b" }, "a", live, "a"))
      .toEqual({ context: { kind: "room", roomId: "b" }, active: "b", contextLost: false });
    expect(roomSelectionAfterSync({ kind: "room", roomId: "gone" }, "gone", live, "a"))
      .toEqual({ context: { kind: "lobby" }, active: "a", contextLost: true });
    expect(roomSelectionAfterSync({ kind: "lobby" }, "b", live, "a").active).toBe("b");
    expect(roomSelectionAfterSync({ kind: "lobby" }, null, new Set(), null).active).toBeNull();
  });
});
