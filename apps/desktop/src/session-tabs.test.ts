import { describe, expect, it } from "vitest";

import {
  canUseUnsafePermission,
  movedSessionOrder,
  newSessionFormDefaults,
  permissionProfileForDriver,
  reconcileSessionTabs,
  relativeSessionId,
  resolveFocusedTabAction,
  resolveGlobalSessionShortcut,
  sessionTabLabel,
  shouldShowZeroSession,
  unsafePermissionWarning,
} from "./session-tabs";
import type { SessionSnapshot } from "./types";

const A = "11111111-1111-1111-1111-111111111111";
const B = "22222222-2222-2222-2222-222222222222";
const C = "33333333-3333-3333-3333-333333333333";

function shortcut(overrides: Partial<Parameters<typeof resolveGlobalSessionShortcut>[0]> = {}) {
  return {
    key: "",
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    metaKey: false,
    editable: false,
    ...overrides,
  };
}

function session(
  sessionId: string,
  overrides: Partial<SessionSnapshot> = {},
): SessionSnapshot {
  return {
    session_id: sessionId,
    alias: `session-${sessionId.slice(0, 4)}`,
    label: "Review",
    driver: "claude",
    permission_profile: "normal",
    lifecycle_state: "closed",
    working_dir: "C:\\work\\project",
    generation: 0,
    run_id: null,
    run_event_sequence: 0,
    process_id: null,
    running: false,
    last_activity_at: null,
    last_error: null,
    ...overrides,
  };
}

describe("SessionId tab reconciliation", () => {
  it("uses backend order while preserving exact-ID selection", () => {
    const transition = reconcileSessionTabs(
      { order: [A, B, C], activeId: B },
      [C, B, A],
    );
    expect(transition.state).toEqual({ order: [C, B, A], activeId: B });
    expect(transition.addedIds).toEqual([]);
    expect(transition.removedIds).toEqual([]);
  });

  it("selects the nearest neighbor when the active ID is removed", () => {
    const transition = reconcileSessionTabs(
      { order: [A, B, C], activeId: B },
      [A, C],
    );
    expect(transition.state.activeId).toBe(C);
    expect(transition.removedIds).toEqual([B]);
  });

  it("treats a same-label replacement as a fresh terminal lineage", () => {
    const transition = reconcileSessionTabs(
      { order: [A], activeId: A },
      [B],
    );
    expect(transition.addedIds).toEqual([B]);
    expect(transition.removedIds).toEqual([A]);
    expect(transition.state.activeId).toBe(B);
  });

  it("produces a focused zero-session state", () => {
    expect(
      reconcileSessionTabs({ order: [A], activeId: A }, []).state,
    ).toEqual({ order: [], activeId: null });
    expect(shouldShowZeroSession([], false)).toBe(true);
    expect(shouldShowZeroSession([], true)).toBe(false);
    expect(shouldShowZeroSession([A], false)).toBe(false);
  });

  it("does not report disposal when only selection or lifecycle changes", () => {
    const switched = reconcileSessionTabs(
      { order: [A, B], activeId: A },
      [A, B],
      B,
    );
    expect(switched.state.activeId).toBe(B);
    expect(switched.removedIds).toEqual([]);
  });

  it("keeps duplicate labels visibly disambiguated by driver, cwd, and short ID", () => {
    const first = session(A, { driver: "claude", working_dir: "C:\\one" });
    const second = session(B, { driver: "codex", working_dir: "C:\\two" });
    expect(sessionTabLabel(first)).toBe("Review · Claude Code · one · 11111111");
    expect(sessionTabLabel(second)).toBe("Review · Codex · two · 22222222");
  });
});

describe("tab movement and keyboard commands", () => {
  it("moves exact IDs one position without drag state", () => {
    expect(movedSessionOrder([A, B, C], B, -1)).toEqual([B, A, C]);
    expect(movedSessionOrder([A, B, C], B, 1)).toEqual([A, C, B]);
    expect(movedSessionOrder([A, B, C], A, -1)).toEqual([A, B, C]);
  });

  it("cycles selection", () => {
    expect(relativeSessionId([A, B, C], C, 1)).toBe(A);
    expect(relativeSessionId([A, B, C], A, -1)).toBe(C);
  });

  it("maps only the locked global shortcuts", () => {
    expect(resolveGlobalSessionShortcut(shortcut({ key: "Tab", ctrlKey: true }))).toEqual({
      kind: "select-relative",
      delta: 1,
    });
    expect(resolveGlobalSessionShortcut(shortcut({ key: "Tab", ctrlKey: true, shiftKey: true }))).toEqual({
      kind: "select-relative",
      delta: -1,
    });
    expect(resolveGlobalSessionShortcut(shortcut({ key: "T", ctrlKey: true, shiftKey: true }))).toEqual({
      kind: "new-session",
    });
    expect(resolveGlobalSessionShortcut(shortcut({ key: "w", ctrlKey: true, shiftKey: true }))).toEqual({
      kind: "close-session",
    });
    expect(resolveGlobalSessionShortcut(shortcut({ key: "w", ctrlKey: true }))).toBeNull();
    expect(resolveGlobalSessionShortcut(shortcut({ key: "n", ctrlKey: true }))).toBeNull();
    expect(resolveGlobalSessionShortcut(shortcut({ key: "Tab", ctrlKey: true, editable: true }))).toBeNull();
  });

  it("supports roving focus and keyboard reordering only from a focused tab", () => {
    expect(resolveFocusedTabAction(shortcut({ key: "ArrowRight" }), 1, 3)).toEqual({
      kind: "select-index",
      index: 2,
    });
    expect(resolveFocusedTabAction(shortcut({ key: "Home" }), 2, 3)).toEqual({
      kind: "select-index",
      index: 0,
    });
    expect(resolveFocusedTabAction(shortcut({ key: "ArrowLeft", ctrlKey: true, shiftKey: true }), 1, 3)).toEqual({
      kind: "move",
      delta: -1,
    });
  });
});

describe("permission profiles", () => {
  it("defaults and clamps Generic Terminal to Normal", () => {
    expect(newSessionFormDefaults("C:\\work")).toEqual({
      driver: "claude",
      permissionProfile: "normal",
      workingDirectory: "C:\\work",
    });
    expect(permissionProfileForDriver("generic_terminal", "unsafe")).toBe("normal");
    expect(canUseUnsafePermission("generic_terminal")).toBe(false);
  });

  it("makes Unsafe explicit", () => {
    expect(unsafePermissionWarning("codex", "unsafe")).toContain("permission-bypass");
    expect(unsafePermissionWarning("codex", "normal")).toBeNull();
  });
});
