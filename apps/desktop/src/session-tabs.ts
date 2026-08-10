import type {
  DriverKind,
  LifecycleState,
  PermissionProfile,
  SessionSnapshot,
} from "./types";

export interface SessionTabsState {
  order: string[];
  activeId: string | null;
}

export interface SessionTabsTransition {
  state: SessionTabsState;
  addedIds: string[];
  removedIds: string[];
}

export type GlobalSessionShortcut =
  | { kind: "select-relative"; delta: -1 | 1 }
  | { kind: "new-session" }
  | { kind: "close-session" };

export type FocusedTabAction =
  | { kind: "select-index"; index: number }
  | { kind: "move"; delta: -1 | 1 };

export interface ShortcutInput {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
  editable: boolean;
}

export interface NewSessionFormDefaults {
  driver: DriverKind;
  permissionProfile: PermissionProfile;
  workingDirectory: string;
}

export function newSessionFormDefaults(
  workspacePreference: string,
): NewSessionFormDefaults {
  return {
    driver: "claude",
    permissionProfile: "normal",
    workingDirectory: workspacePreference,
  };
}

export function shouldShowZeroSession(
  sessionIds: readonly string[],
  editorOpen: boolean,
): boolean {
  return sessionIds.length === 0 && !editorOpen;
}

export function reconcileSessionTabs(
  previous: SessionTabsState,
  orderedSessionIds: readonly string[],
  preferredId?: string | null,
): SessionTabsTransition {
  const nextOrder = Array.from(new Set(orderedSessionIds));
  const previousIds = new Set(previous.order);
  const nextIds = new Set(nextOrder);
  const removedIds = previous.order.filter((sessionId) => !nextIds.has(sessionId));
  const addedIds = nextOrder.filter((sessionId) => !previousIds.has(sessionId));

  let activeId: string | null = null;
  if (preferredId && nextIds.has(preferredId)) {
    activeId = preferredId;
  } else if (previous.activeId && nextIds.has(previous.activeId)) {
    activeId = previous.activeId;
  } else if (nextOrder.length > 0) {
    const previousIndex = previous.activeId
      ? previous.order.indexOf(previous.activeId)
      : 0;
    activeId = nextOrder[Math.min(Math.max(previousIndex, 0), nextOrder.length - 1)];
  }

  return {
    state: { order: nextOrder, activeId },
    addedIds,
    removedIds,
  };
}

export function relativeSessionId(
  order: readonly string[],
  activeId: string | null,
  delta: -1 | 1,
): string | null {
  if (order.length === 0) {
    return null;
  }
  const currentIndex = activeId ? order.indexOf(activeId) : -1;
  const origin = currentIndex >= 0 ? currentIndex : delta > 0 ? -1 : 0;
  return order[(origin + delta + order.length) % order.length] ?? null;
}

export function movedSessionOrder(
  order: readonly string[],
  sessionId: string,
  delta: -1 | 1,
): string[] {
  const currentIndex = order.indexOf(sessionId);
  const targetIndex = currentIndex + delta;
  if (currentIndex < 0 || targetIndex < 0 || targetIndex >= order.length) {
    return [...order];
  }
  const next = [...order];
  [next[currentIndex], next[targetIndex]] = [next[targetIndex], next[currentIndex]];
  return next;
}

export function resolveGlobalSessionShortcut(
  input: ShortcutInput,
): GlobalSessionShortcut | null {
  if (input.editable || input.metaKey || input.altKey || !input.ctrlKey) {
    return null;
  }
  if (input.key === "Tab") {
    return { kind: "select-relative", delta: input.shiftKey ? -1 : 1 };
  }
  if (!input.shiftKey) {
    return null;
  }
  switch (input.key.toLowerCase()) {
    case "t":
      return { kind: "new-session" };
    case "w":
      return { kind: "close-session" };
    default:
      return null;
  }
}

export function resolveFocusedTabAction(
  input: ShortcutInput,
  currentIndex: number,
  tabCount: number,
): FocusedTabAction | null {
  if (input.editable || input.metaKey || input.altKey || tabCount <= 0) {
    return null;
  }
  if (input.ctrlKey && input.shiftKey) {
    if (input.key === "ArrowLeft") {
      return { kind: "move", delta: -1 };
    }
    if (input.key === "ArrowRight") {
      return { kind: "move", delta: 1 };
    }
    return null;
  }
  if (input.ctrlKey || input.shiftKey) {
    return null;
  }
  switch (input.key) {
    case "ArrowLeft":
      return {
        kind: "select-index",
        index: (currentIndex - 1 + tabCount) % tabCount,
      };
    case "ArrowRight":
      return { kind: "select-index", index: (currentIndex + 1) % tabCount };
    case "Home":
      return { kind: "select-index", index: 0 };
    case "End":
      return { kind: "select-index", index: tabCount - 1 };
    default:
      return null;
  }
}

export function permissionProfileForDriver(
  driver: DriverKind,
  requested: PermissionProfile,
): PermissionProfile {
  return driver === "generic_terminal" ? "normal" : requested;
}

export function canUseUnsafePermission(driver: DriverKind): boolean {
  return driver !== "generic_terminal";
}

export function unsafePermissionWarning(
  driver: DriverKind,
  permissionProfile: PermissionProfile,
): string | null {
  if (permissionProfile !== "unsafe") {
    return null;
  }
  if (!canUseUnsafePermission(driver)) {
    return "Generic terminals support Normal permission only.";
  }
  return "Unsafe starts this harness with its permission-bypass mode. It may act without normal approval prompts.";
}

export function sessionTabLabel(
  session: Pick<
    SessionSnapshot,
    | "session_id"
    | "label"
    | "driver"
    | "working_dir"
    | "permission_profile"
    | "lifecycle_state"
  >,
): string {
  return `${session.label} · ${driverLabel(session.driver)} · ${cwdLeaf(session.working_dir)} · ${shortSessionId(session.session_id)}`;
}

export function sessionTabDescription(
  session: Pick<
    SessionSnapshot,
    | "session_id"
    | "label"
    | "driver"
    | "working_dir"
    | "permission_profile"
    | "lifecycle_state"
  >,
): string {
  return [
    sessionTabLabel(session),
    session.permission_profile === "unsafe" ? "Unsafe" : "Normal",
    lifecycleLabel(session.lifecycle_state),
    session.working_dir,
  ].join(" · ");
}

export function driverLabel(driver: DriverKind): string {
  switch (driver) {
    case "claude":
      return "Claude Code";
    case "codex":
      return "Codex";
    case "generic_terminal":
      return "Terminal";
  }
}

export function lifecycleLabel(state: LifecycleState): string {
  return state.replace(/_/g, " ");
}

export function shortSessionId(sessionId: string): string {
  return sessionId.slice(0, 8);
}

function cwdLeaf(workingDir: string): string {
  const trimmed = workingDir.replace(/[\\/]+$/, "");
  const parts = trimmed.split(/[\\/]/);
  return parts[parts.length - 1] || workingDir;
}
