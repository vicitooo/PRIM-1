import { describe, expect, it } from "vitest";
import { vi } from "vitest";
import { loadRoomFlow, pickRoomSource, roomLoadSummary, roomSaveSummary, roomSourceConfirmText, roomSourceListText, type RoomLoadSteps } from "./room-package";
import { roomSelectionAfterSync, type WorkContext } from "./work-context";
import type { RoomImportReport, RoomSourceSummary } from "./types";

function report(overrides: Partial<RoomImportReport> = {}): RoomImportReport {
  return {
    room: {
      room_id: "r1", label: "Walk room", resume_on_restart: false, member_ids: ["a", "b", "c", "d"],
      membership_revision: 3, feed_epoch: "e", feed_oldest_sequence: 1, feed_next_sequence: 9,
    },
    members: [
      { session_id: "a", label: "Claude", driver: "claude", permission_profile: "unsafe", state: "resumes", note: null },
      { session_id: "b", label: "Grok", driver: "grok", permission_profile: "unsafe", state: "starts_fresh", note: null },
      { session_id: "c", label: "Terminal", driver: "generic_terminal", permission_profile: "normal", state: "starts_fresh", note: null },
      { session_id: "d", label: "Codex", driver: "codex", permission_profile: "normal", state: "held", note: "Codex resume is held." },
    ],
    warnings: [],
    ...overrides,
  };
}

describe("roomLoadSummary", () => {
  it("counts each honest state, names unsafe profiles and says nothing started", () => {
    const result = roomLoadSummary(report());
    expect(result.summary).toBe(
      'Loaded "Walk room": 1 resumes its conversation, 1 held, 2 start fresh. Nothing was started. 2 members use the unsafe profile, as saved.',
    );
    expect(result.level).toBe("warn");
    expect(result.details.split("\n")).toEqual([
      "Claude: resumes its conversation",
      "Grok: starts fresh",
      "Terminal: starts fresh",
      "Codex: held (Codex resume is held.)",
    ]);
  });

  it("shows the run-elsewhere warning in the visible line and the member's note in the details", () => {
    const warning = "1 member resumes a conversation that the PRIM-1 this room came from may still be running. Close it there before you wake it here, so no agent runs in two PRIM-1s.";
    const result = roomLoadSummary(report({
      members: [{ session_id: "a", label: "Claude", driver: "claude", permission_profile: "normal", state: "resumes",
        note: "close it in the PRIM-1 it came from first" }],
      warnings: [warning],
    }));
    expect(result.summary).toBe(`Loaded "Walk room": 1 resumes its conversation. Nothing was started. ${warning}`);
    expect(result.level).toBe("warn");
    expect(result.details).toBe("Claude: resumes its conversation (close it in the PRIM-1 it came from first)");
  });

  it("keeps every warning in the visible line, such as a visibility change", () => {
    const warning = "Loaded members see only messages posted after this load. You still see the whole feed.";
    const result = roomLoadSummary(report({
      members: [{ session_id: "a", label: "Claude", driver: "claude", permission_profile: "normal", state: "resumes", note: null }],
      warnings: [warning],
    }));
    expect(result.summary).toBe(`Loaded "Walk room": 1 resumes its conversation. Nothing was started. ${warning}`);
    expect(result.level).toBe("warn");
    expect(roomLoadSummary(report({ members: [], warnings: [] })).summary)
      .toBe('Loaded "Walk room": no members. Nothing was started.');
    expect(roomLoadSummary(report({ members: [], warnings: [] })).level).toBe("info");
  });
});

describe("roomSaveSummary", () => {
  it("states the file, counts and only a noticeable pause", () => {
    const saved = { path: "D:\\rooms\\walk.prim1room", label: "Walk room", members: 1, records: 42, bytes: 9000, paused_ms: 3, warnings: [] };
    expect(roomSaveSummary(saved)).toEqual({
      summary: 'Saved "Walk room" to D:\\rooms\\walk.prim1room: 1 member, 42 feed records.', level: "info", details: "",
    });
    const slow = roomSaveSummary({ ...saved, paused_ms: 400, warnings: ["Only each member's current hold is saved."] });
    expect(slow.summary).toContain("Room traffic waited 400 ms while the feed was copied. Only each member's current hold is saved.");
    expect(slow.level).toBe("warn");
  });
});

describe("room sources", () => {
  const walk: RoomSourceSummary = { source_room_id: "r1", label: "Walk room", members: 4, records: 1205, unavailable: null };
  const solo: RoomSourceSummary = { source_room_id: "r2", label: "Solo", members: 1, records: 1, unavailable: null };

  it("confirms with the exact counts before anything is written", () => {
    expect(roomSourceConfirmText(walk)).toBe('Load "Walk room": 4 members, 1,205 feed records. A new room is created; nothing starts.');
    expect(roomSourceConfirmText(solo)).toBe('Load "Solo": 1 member, 1 feed record. A new room is created; nothing starts.');
  });

  it("lists a runtime copy's rooms and picks only a valid number", () => {
    expect(roomSourceListText([walk, solo]).split("\n")).toEqual([
      "This runtime copy has 2 rooms. Type the number of the room to load:",
      "1. Walk room (4 members, 1,205 feed records)",
      "2. Solo (1 member, 1 feed record)",
    ]);
    expect(pickRoomSource([walk], null)).toBe(walk);
    expect(pickRoomSource([walk, solo], " 2 ")).toBe(solo);
    for (const answer of [null, "", "0", "3", "1.5", "two"]) {
      expect(pickRoomSource([walk, solo], answer)).toBeNull();
    }
  });

  it("shows an unreadable room as unavailable and never lets it be picked", () => {
    const broken: RoomSourceSummary = { ...solo, records: 0, unavailable: 'Room "Solo" has no feed in the copy' };
    expect(roomSourceListText([walk, broken]).split("\n")[2]).toBe('2. Solo (unavailable: Room "Solo" has no feed in the copy)');
    expect(pickRoomSource([walk, broken], "2")).toBeNull();
    expect(pickRoomSource([walk, broken], "1")).toBe(walk);
    expect(pickRoomSource([broken], null)).toBeNull();
  });
});

describe("loadRoomFlow", () => {
  const sourceB: RoomSourceSummary = { source_room_id: "src-b", label: "Walk room", members: 4, records: 12, unavailable: null };
  const loadedB = (): RoomImportReport => report({ room: { ...report().room, room_id: "B" } });

  /** The renderer's room state, refreshed with the same rule as syncRoomInventory. */
  function harness(overrides: Partial<RoomLoadSteps> = {}) {
    const state: { context: WorkContext; active: string | null; rooms: string[] } =
      { context: { kind: "room", roomId: "A" }, active: "A", rooms: ["A"] };
    const refresh = async () => {
      const next = roomSelectionAfterSync(state.context, state.active, new Set(state.rooms), state.rooms[0] ?? null);
      state.context = next.context;
      state.active = next.active;
    };
    const steps: RoomLoadSteps = {
      choose: vi.fn(async () => "D:\\rooms\\walk.prim1room"),
      inspect: vi.fn(async () => [sourceB]),
      ask: vi.fn(() => null),
      confirm: vi.fn(async () => true),
      load: vi.fn(async () => { state.rooms.push("B"); return loadedB(); }),
      refresh: vi.fn(refresh),
      enter: vi.fn((roomId: string) => { state.context = { kind: "room", roomId }; state.active = roomId; }),
      loaded: vi.fn(),
      problem: vi.fn(),
      ...overrides,
    };
    return { state, steps, refresh };
  }

  it("enters the loaded room as the context, so a later refresh keeps it selected", async () => {
    const { state, steps, refresh } = harness();
    expect(await loadRoomFlow(steps)).toBe("loaded");
    expect(steps.confirm).toHaveBeenCalledWith(roomSourceConfirmText(sourceB));
    expect(steps.load).toHaveBeenCalledWith("D:\\rooms\\walk.prim1room", "src-b");
    await refresh();
    expect(state.active).toBe("B");
    expect(state.context).toEqual({ kind: "room", roomId: "B" });
    // Selecting B without entering it is what a refresh used to undo.
    expect(roomSelectionAfterSync({ kind: "room", roomId: "A" }, "B", new Set(["A", "B"]), "A").active).toBe("A");
  });

  it("keeps a committed load when only the display refresh fails, without loading again", async () => {
    const { steps } = harness({ refresh: vi.fn(async () => { throw new Error("snapshot unavailable"); }) });
    expect(await loadRoomFlow(steps)).toBe("loaded");
    expect(steps.load).toHaveBeenCalledTimes(1);
    expect(steps.problem).toHaveBeenCalledTimes(1);
    expect(vi.mocked(steps.problem).mock.calls[0][0]).toMatch(/^Room loaded; the display did not refresh: snapshot unavailable\. Loaded "Walk room":/);
    expect(steps.loaded).not.toHaveBeenCalled();
  });

  it("reports a refused load once, shows any held copies, and lets a copy's room be chosen by number", async () => {
    const refused = harness({ load: vi.fn(async () => { throw "The room file is incomplete. Nothing was changed."; }) });
    expect(await loadRoomFlow(refused.steps)).toBe("failed");
    expect(refused.steps.problem).toHaveBeenCalledWith("Room not loaded: The room file is incomplete. Nothing was changed.");
    expect(refused.steps.refresh).toHaveBeenCalledTimes(1);
    expect(refused.steps.enter).not.toHaveBeenCalled();

    const other: RoomSourceSummary = { source_room_id: "src-c", label: "Solo", members: 1, records: 2, unavailable: null };
    const several = harness({ inspect: vi.fn(async () => [sourceB, other]), ask: vi.fn(() => "2") });
    await loadRoomFlow(several.steps);
    expect(several.steps.ask).toHaveBeenCalledWith(roomSourceListText([sourceB, other]));
    expect(several.steps.load).toHaveBeenCalledWith(expect.any(String), "src-c");

    const declined = harness({ confirm: vi.fn(async () => false) });
    expect(await loadRoomFlow(declined.steps)).toBe("cancelled");
    expect(declined.steps.load).not.toHaveBeenCalled();

    const broken: RoomSourceSummary = { ...other, records: 0, unavailable: 'Room "Solo" has no feed in the copy' };
    const unavailable = harness({ inspect: vi.fn(async () => [sourceB, broken]), ask: vi.fn(() => "2") });
    expect(await loadRoomFlow(unavailable.steps)).toBe("failed");
    expect(unavailable.steps.problem).toHaveBeenCalledWith('Room not loaded: Room "Solo" has no feed in the copy');
    expect(unavailable.steps.confirm).not.toHaveBeenCalled();
    expect(unavailable.steps.load).not.toHaveBeenCalled();
  });

  it("imports only after the awaited native confirm says yes", async () => {
    const cases: [string, RoomLoadSteps["confirm"], number][] = [
      ["resolves false", async () => false, 0],
      ["resolves true", async () => true, 1],
      ["rejects", async () => { throw new Error("dialog.confirm not allowed"); }, 0],
    ];
    for (const [name, confirm, imports] of cases) {
      const { steps } = harness({ confirm: vi.fn(confirm) });
      await loadRoomFlow(steps);
      expect(steps.load, name).toHaveBeenCalledTimes(imports);
    }
    // Like the dialog plugin's window.confirm result: truthy, but not true.
    const truthyObject = harness({ confirm: vi.fn(async () => ({ then: undefined }) as unknown as boolean) });
    await loadRoomFlow(truthyObject.steps);
    expect(truthyObject.steps.load, "a truthy non-true value").not.toHaveBeenCalled();
    const rejected = harness({ confirm: vi.fn(async () => { throw new Error("dialog.confirm not allowed"); }) });
    expect(await loadRoomFlow(rejected.steps)).toBe("failed");
    expect(rejected.steps.problem).toHaveBeenCalledWith("Room not loaded: dialog.confirm not allowed");
  });
});
