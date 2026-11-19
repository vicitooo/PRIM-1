import { describe, expect, it } from "vitest";

import {
  SUPERVISOR_GRANTS,
  acceptsCodexTransport,
  acceptsMemberGrants,
  appServerFact,
  appServerFollowText,
  createGrantsFromForm,
  isSupervisorGrants,
  canUseUnsafePermission,
  createSessionRequestFromForm,
  driverLabel,
  launchChoicesFromForm,
  memberGrantsNote,
  movedSessionOrder,
  newSessionFormDefaults,
  permissionProfileForDriver,
  reconcileSessionTabs,
  relativeSessionId,
  resolveFocusedTabAction,
  sameLaunchChoices,
  resolveGlobalSessionShortcut,
  sessionTabLabel,
  sessionTabDescription,
  sessionDisplayState,
  lifecycleLabel,
  shouldShowZeroSession,
  unsafePermissionWarning,
} from "./session-tabs";
import type { SessionSnapshot } from "./types";

const A = "11111111-1111-1111-1111-111111111111";
const B = "22222222-2222-2222-2222-222222222222";
const C = "33333333-3333-3333-3333-333333333333";

it("uses draining presentation for tab badges and descriptions while keeping the run owned", () => {
  for (const lifecycle_state of ["idle", "ready"] as const) {
    const draining = session(A, { lifecycle_state, running: true, output_draining: true });
    expect(sessionDisplayState(draining)).toBe("draining");
    expect(lifecycleLabel(sessionDisplayState(draining))).toBe("draining output — input closed");
    expect(sessionTabDescription(draining)).toContain("Stop remains available");
    expect(sessionTabDescription(draining)).not.toContain(` · ${lifecycle_state} · `);
    expect(draining.running).toBe(true);
  }
});

it("uses Sleeping in badges and descriptions without replacing native lifecycle or hiding holds", () => {
  const asleep = session(A, { hibernated: true, resume_available: true });
  expect(sessionDisplayState(asleep)).toBe("sleeping");
  expect(lifecycleLabel(sessionDisplayState(asleep))).toBe("Sleeping");
  expect(sessionTabDescription(asleep)).toContain("Sleeping");
  expect(asleep.lifecycle_state).toBe("closed");
  expect(sessionDisplayState({ ...asleep, output_draining: true })).toBe("draining");
  expect(sessionDisplayState({ ...asleep, running: true, lifecycle_state: "starting" })).toBe("starting");
  expect(sessionDisplayState({ ...asleep, recovery: { original_id: "id", provenance: "hold", reason: "Held" } })).toBe("closed");
  expect(sessionDisplayState({ ...asleep, last_error: "crash" })).toBe("closed");
});

it("shows blocked on the pane header and tab while a question waits on the operator (19 Oct walk)", () => {
  const line = "it is asking whether to trust the folder — answer that question in its terminal, then send again";
  for (const lifecycle_state of ["idle", "ready"] as const) {
    const asked = session(A, { lifecycle_state, running: true, blocked_reason: line });
    expect(sessionDisplayState(asked)).toBe("blocked");
    expect(lifecycleLabel(sessionDisplayState(asked))).toBe("blocked");
    expect(sessionTabDescription(asked)).toContain(" · blocked · ");
    expect(sessionDisplayState({ ...asked, blocked_reason: null })).toBe(lifecycle_state);
    expect(sessionDisplayState({ ...asked, output_draining: true })).toBe("draining");
  }
  expect(sessionDisplayState(session(A, { running: false, blocked_reason: line }))).toBe("closed");
});

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
    launch_choices: { model: null, effort: null, brief: { mode: "room_default" } },
    recovery: null,
    recovery_history: [],
    lifecycle_state: "closed",
    working_dir: "C:\\work\\project",
    generation: 0,
    run_id: null,
    run_event_sequence: 0,
    process_id: null,
    running: false,
    resume_available: false,
    was_running_at_shutdown: false,
    last_activity_at: null,
    last_error: null,
    last_error_kind: null,
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
    expect(resolveGlobalSessionShortcut(shortcut({ key: "Tab", ctrlKey: true }), "windows")).toEqual({
      kind: "select-relative",
      delta: 1,
    });
    expect(resolveGlobalSessionShortcut(shortcut({ key: "Tab", ctrlKey: true, shiftKey: true }), "windows")).toEqual({
      kind: "select-relative",
      delta: -1,
    });
    expect(resolveGlobalSessionShortcut(shortcut({ key: "T", ctrlKey: true, shiftKey: true }), "windows")).toEqual({
      kind: "new-session",
    });
    expect(resolveGlobalSessionShortcut(shortcut({ key: "w", ctrlKey: true, shiftKey: true }), "windows")).toEqual({
      kind: "close-session",
    });
    expect(resolveGlobalSessionShortcut(shortcut({ key: "w", ctrlKey: true }), "windows")).toBeNull();
    expect(resolveGlobalSessionShortcut(shortcut({ key: "n", ctrlKey: true }), "windows")).toBeNull();
    expect(resolveGlobalSessionShortcut(shortcut({ key: "Tab", ctrlKey: true, editable: true }), "windows")).toBeNull();
  });

  it("supports roving focus and keyboard reordering only from a focused tab", () => {
    expect(resolveFocusedTabAction(shortcut({ key: "ArrowRight" }), 1, 3, "windows")).toEqual({
      kind: "select-index",
      index: 2,
    });
    expect(resolveFocusedTabAction(shortcut({ key: "Home" }), 2, 3, "windows")).toEqual({
      kind: "select-index",
      index: 0,
    });
    expect(resolveFocusedTabAction(shortcut({ key: "ArrowLeft", ctrlKey: true, shiftKey: true }), 1, 3, "windows")).toEqual({
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
    expect(permissionProfileForDriver("prime", "unsafe")).toBe("normal");
    expect(canUseUnsafePermission("prime")).toBe(false);
    expect(unsafePermissionWarning("prime", "unsafe")).toContain("Prime Agent");
  });

  it("builds a typed Prime request without leaking Linux paths into native sessions", () => {
    expect(
      createSessionRequestFromForm(
        "  Ubuntu review  ",
        "prime",
        "unsafe",
        "/home/alice/work & (qa)",
      ),
    ).toEqual({
      label: "Ubuntu review",
      driver: "prime",
      permission_profile: "normal",
      linux_working_directory: "/home/alice/work & (qa)",
    });
    expect(
      createSessionRequestFromForm("", "prime", "normal", ""),
    ).toEqual({
      label: null,
      driver: "prime",
      permission_profile: "normal",
      linux_working_directory: null,
    });
    expect(
      createSessionRequestFromForm("Native", "claude", "normal", "/not/native"),
    ).toEqual({
      label: "Native",
      driver: "claude",
      permission_profile: "normal",
    });
  });

  it("asks for the App Server only for a Codex member that opts in (v3 W4 S5)", () => {
    expect(createSessionRequestFromForm("", "codex", "normal", "", "app_server")).toEqual({
      label: null,
      driver: "codex",
      permission_profile: "normal",
      codex_transport: "app_server",
    });
    // The terminal is the default and stays omitted, as before the choice existed.
    expect(createSessionRequestFromForm("", "codex", "normal", "")).not.toHaveProperty("codex_transport");
    expect(createSessionRequestFromForm("", "codex", "normal", "", "pty")).not.toHaveProperty("codex_transport");
    // A stale choice left from Codex never reaches another harness.
    for (const driver of ["claude", "grok", "prime", "generic_terminal"] as const) {
      expect(createSessionRequestFromForm("", driver, "normal", "", "app_server")).not.toHaveProperty("codex_transport");
      expect(acceptsCodexTransport(driver)).toBe(false);
    }
    expect(acceptsCodexTransport("codex")).toBe(true);
  });

  it("states the App Server connection without promising delivery (v3 W4 S5)", () => {
    expect(appServerFollowText("followed")).toBe("App Server: PRIM-1 follows this member's thread");
    expect(appServerFollowText("not_followed")).toContain("until the member is restarted");
    expect(appServerFollowText("reconnecting")).toContain("reconnecting to the same owner");
    expect(appServerFollowText("waiting_for_thread")).toContain("terminal route meanwhile");
    for (const follow of ["waiting_for_thread", "followed", "reconnecting", "not_followed"] as const) {
      expect(appServerFollowText(follow)).not.toMatch(/deliver|will arrive|ready/i);
    }
    expect(appServerFact({ running: false, app_server: "followed" })).toBeNull();
    expect(appServerFact({ running: true })).toBeNull();
    const held = session(A, { running: true, lifecycle_state: "ready", blocked_reason: "approval waits", app_server: "followed" });
    expect(sessionTabDescription(held)).toContain(" · blocked · ");
    expect(sessionTabDescription(held).endsWith("App Server: PRIM-1 follows this member's thread")).toBe(true);
  });

  it("makes Unsafe explicit", () => {
    expect(unsafePermissionWarning("codex", "unsafe")).toContain("permission-bypass");
    expect(canUseUnsafePermission("grok")).toBe(true);
    expect(permissionProfileForDriver("grok", "unsafe")).toBe("unsafe");
    expect(driverLabel("grok")).toBe("Grok Build");
    expect(driverLabel("prime")).toBe("Prime Agent (Ubuntu)");
    expect(unsafePermissionWarning("codex", "normal")).toBeNull();
  });
});

describe("launch choices (S5)", () => {
  it("turns blank model and effort into the harness default", () => {
    expect(launchChoicesFromForm("claude", "  ", "", "room_default", "")).toEqual({
      model: null,
      effort: null,
      brief: { mode: "room_default" },
    });
  });

  it("keeps typed values and the custom brief text exactly", () => {
    expect(
      launchChoicesFromForm("codex", " gpt-5.5 ", "high", "custom", "  Line one\nLine two  "),
    ).toEqual({
      model: "gpt-5.5",
      effort: "high",
      brief: { mode: "custom", text: "  Line one\nLine two  " },
    });
    expect(launchChoicesFromForm("grok", "", "", "none", "ignored")?.brief).toEqual({
      mode: "none",
    });
  });

  it("gives Terminal and Prime no launch choices", () => {
    expect(launchChoicesFromForm("generic_terminal", "m", "high", "custom", "x")).toBeNull();
    expect(launchChoicesFromForm("prime", "m", "high", "none", "")).toBeNull();
  });

  it("compares choices including the custom text", () => {
    const base = { model: "m", effort: null, brief: { mode: "custom" as const, text: "a" } };
    expect(sameLaunchChoices(base, { ...base })).toBe(true);
    expect(sameLaunchChoices(base, { ...base, brief: { mode: "custom", text: "b" } })).toBe(false);
    expect(sameLaunchChoices(base, { ...base, brief: { mode: "none" } })).toBe(false);
    expect(sameLaunchChoices(base, { ...base, effort: "max" })).toBe(false);
  });
});

describe("member grants (A109)", () => {
  it("offers Can roam, Can upgrade PRIM-1 and Can manage agents only to panes with room tools", () => {
    for (const driver of ["claude", "codex", "grok"] as const) {
      expect(acceptsMemberGrants(driver)).toBe(true);
    }
    expect(acceptsMemberGrants("prime")).toBe(false);
    expect(acceptsMemberGrants("generic_terminal")).toBe(false);
  });

  it("tells the operator when a running agent sees new tools, and Restart as the fallback", () => {
    for (const driver of ["claude", "codex", "grok"] as const) {
      expect(memberGrantsNote(driver)).toBe(
        "A running agent sees new tools after its next tool call; if its harness ignores tool-list updates, use Restart (it resumes the same conversation).",
      );
    }
    for (const driver of ["prime", "generic_terminal"] as const) {
      expect(memberGrantsNote(driver)).toBe("Prime and Terminal panes have no room tools, so these grants do nothing for them.");
    }
  });
});

describe("supervisor grants at create (A16)", () => {
  const none = { roam: false, maintain: false, manage: false };

  it("treats a supervisor as all three grants, never fewer", () => {
    expect(isSupervisorGrants(SUPERVISOR_GRANTS)).toBe(true);
    expect(isSupervisorGrants({ roam: true, maintain: true, manage: false })).toBe(false);
    expect(isSupervisorGrants({ roam: true, maintain: true })).toBe(false);
    expect(isSupervisorGrants(none)).toBe(false);
  });

  it("sends the ticked grants for an agent and nothing for a worker or a pane without room tools", () => {
    for (const driver of ["claude", "codex", "grok"] as const) {
      expect(createGrantsFromForm(driver, { ...SUPERVISOR_GRANTS })).toEqual(SUPERVISOR_GRANTS);
      expect(createGrantsFromForm(driver, { roam: true, maintain: false, manage: false }))
        .toEqual({ roam: true, maintain: false, manage: false });
      expect(createGrantsFromForm(driver, none)).toBeNull();
    }
    expect(createGrantsFromForm("prime", { ...SUPERVISOR_GRANTS })).toBeNull();
    expect(createGrantsFromForm("generic_terminal", { ...SUPERVISOR_GRANTS })).toBeNull();
  });
});
