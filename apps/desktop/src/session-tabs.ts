import { detectPlatform, resolveKeyAction, type Platform } from "./keymap";
import { isSleeping, type SleepSnapshot } from "./hibernation";
import type {
  AppServerFollow,
  CodexTransport,
  CreateSessionRequest,
  DriverKind,
  LifecycleState,
  MemberPermissions,
  PermissionProfile,
  SessionBriefChoice,
  SessionLaunchChoices,
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
  /** `KeyboardEvent.code` — letter chords fall back to it off a Latin layout. */
  code?: string;
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

/**
 * Session-wide chords (tab switching, attach, close), read from the keymap.
 * `platform` defaults to the running platform; tests pass it explicitly.
 */
export function resolveGlobalSessionShortcut(
  input: ShortcutInput,
  platform: Platform = detectPlatform(),
): GlobalSessionShortcut | null {
  const action = resolveKeyAction(input, platform, {
    editable: input.editable,
    terminalFocused: false,
    tabFocused: false,
    hasSelection: () => false,
  });
  switch (action) {
    case "next-tab":
      return { kind: "select-relative", delta: 1 };
    case "previous-tab":
      return { kind: "select-relative", delta: -1 };
    case "new-session":
      return { kind: "new-session" };
    case "close-session":
      return { kind: "close-session" };
    default:
      return null;
  }
}

/** Chords that are live only while a tab button owns focus, from the keymap. */
export function resolveFocusedTabAction(
  input: ShortcutInput,
  currentIndex: number,
  tabCount: number,
  platform: Platform = detectPlatform(),
): FocusedTabAction | null {
  if (tabCount <= 0) {
    return null;
  }
  const action = resolveKeyAction(input, platform, {
    editable: input.editable,
    terminalFocused: false,
    tabFocused: true,
    hasSelection: () => false,
  });
  switch (action) {
    case "tab-move-left":
      return { kind: "move", delta: -1 };
    case "tab-move-right":
      return { kind: "move", delta: 1 };
    case "tab-select-previous":
      return {
        kind: "select-index",
        index: (currentIndex - 1 + tabCount) % tabCount,
      };
    case "tab-select-next":
      return { kind: "select-index", index: (currentIndex + 1) % tabCount };
    case "tab-select-first":
      return { kind: "select-index", index: 0 };
    case "tab-select-last":
      return { kind: "select-index", index: tabCount - 1 };
    default:
      return null;
  }
}

export function permissionProfileForDriver(
  driver: DriverKind,
  requested: PermissionProfile,
): PermissionProfile {
  return driver === "generic_terminal" || driver === "prime" ? "normal" : requested;
}

export function canUseUnsafePermission(driver: DriverKind): boolean {
  return driver !== "generic_terminal" && driver !== "prime";
}

export function createSessionRequestFromForm(
  label: string,
  driver: DriverKind,
  requestedPermissionProfile: PermissionProfile,
  linuxWorkingDirectory: string,
  codexTransport: CodexTransport = "pty",
): CreateSessionRequest {
  const request: CreateSessionRequest = {
    label: label.trim() || null,
    driver,
    permission_profile: permissionProfileForDriver(
      driver,
      requestedPermissionProfile,
    ),
  };
  if (driver === "prime") {
    request.linux_working_directory = linuxWorkingDirectory || null;
  }
  // v3 W4 S5: the App Server opt-in is a Codex member's only, and omitted
  // while it is the terminal, so the request reads as it always did.
  if (driver === "codex" && codexTransport === "app_server") {
    request.codex_transport = "app_server";
  }
  return request;
}

/** v3 W4 S5: the "Codex connection" choice is shown for Codex only. */
export function acceptsCodexTransport(driver: DriverKind): boolean {
  return driver === "codex";
}

/** v3 W4 S5 (Codex PRIM-1): the App Server fact names PRIM-1's connection
    to the member's thread, never whether a message will be delivered: a
    hold or recovery reason shown beside it still decides that. */
export function appServerFollowText(follow: AppServerFollow): string {
  switch (follow) {
    case "waiting_for_thread":
      return "App Server: waiting to see this member's thread; room messages take the terminal route meanwhile";
    case "followed":
      return "App Server: PRIM-1 follows this member's thread";
    case "reconnecting":
      return "App Server: connection lost; PRIM-1 is reconnecting to the same owner";
    case "not_followed":
      return "App Server: not followed; room messages take the terminal route until the member is restarted";
  }
}

/** The member's App Server fact while it runs on that transport, else null. */
export function appServerFact(
  session: Pick<SessionSnapshot, "running" | "app_server">,
): string | null {
  return session.running && session.app_server ? appServerFollowText(session.app_server) : null;
}

/** Only the agent harnesses take a model, effort or starting brief (S5);
    Terminal and Prime launch exactly as configured. */
export function acceptsLaunchChoices(driver: DriverKind): boolean {
  return driver === "claude" || driver === "codex" || driver === "grok";
}

/** "Can roam", "Can upgrade PRIM-1" (A109) and "Can manage agents" (A110)
    unlock room tools; Prime and Terminal panes have none, and the backend
    refuses those grants for them. */
export function acceptsMemberGrants(driver: DriverKind): boolean {
  return driver === "claude" || driver === "codex" || driver === "grok";
}

/** A16 (Victor, 20 Oct: "for the supervisors it should be allowed under
    Beyond"): a supervisor holds all three grants. The create form's
    Supervisor box ticks them; each stays editable, and a worker gets none. */
export const SUPERVISOR_GRANTS: Readonly<Required<MemberPermissions>> = { roam: true, maintain: true, manage: true };

export function isSupervisorGrants(grants: MemberPermissions): boolean {
  return grants.roam && grants.maintain && Boolean(grants.manage);
}

/** What a create sends: the ticked grants for an agent, nothing when none
    is ticked or the driver has no room tools. */
export function createGrantsFromForm(driver: DriverKind, grants: Required<MemberPermissions>): MemberPermissions | null {
  if (!acceptsMemberGrants(driver) || !(grants.roam || grants.maintain || grants.manage)) {
    return null;
  }
  return { roam: grants.roam, maintain: grants.maintain, manage: grants.manage };
}

/** The line under the grants in the Edit dialog. A running agent's pane MCP
    rechecks its grants after a tool call and announces a change, which a
    harness that ignores tool-list updates never acts on. */
export function memberGrantsNote(driver: DriverKind): string {
  return acceptsMemberGrants(driver)
    ? "A running agent sees new tools after its next tool call; if its harness ignores tool-list updates, use Restart (it resumes the same conversation)."
    : "Prime and Terminal panes have no room tools, so these grants do nothing for them.";
}

export type BriefMode = SessionBriefChoice["mode"];

/** The session form's launch choices. A blank model or effort means the
    harness default (null); requested values are sent as typed, never
    filtered here — the backend validates them. */
export function launchChoicesFromForm(
  driver: DriverKind,
  model: string,
  effort: string,
  briefMode: BriefMode,
  customBrief: string,
): SessionLaunchChoices | null {
  if (!acceptsLaunchChoices(driver)) {
    return null;
  }
  const brief: SessionBriefChoice =
    briefMode === "custom"
      ? { mode: "custom", text: customBrief }
      : briefMode === "none"
        ? { mode: "none" }
        : { mode: "room_default" };
  return { model: model.trim() || null, effort: effort.trim() || null, brief };
}

export function sameLaunchChoices(
  left: SessionLaunchChoices,
  right: SessionLaunchChoices,
): boolean {
  if (left.model !== right.model || left.effort !== right.effort) {
    return false;
  }
  if (left.brief.mode !== right.brief.mode) {
    return false;
  }
  return left.brief.mode !== "custom"
    || (right.brief.mode === "custom" && left.brief.text === right.brief.text);
}

export function unsafePermissionWarning(
  driver: DriverKind,
  permissionProfile: PermissionProfile,
): string | null {
  if (permissionProfile !== "unsafe") {
    return null;
  }
  if (!canUseUnsafePermission(driver)) {
    return driver === "prime"
      ? "Prime Agent in Ubuntu supports Normal permission only."
      : "Generic terminals support Normal permission only.";
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
    | "output_draining"
    | "hibernated"
    | "running"
    | "recovery"
    | "last_error"
    | "blocked_reason"
    | "app_server"
  >,
): string {
  const appServer = appServerFact(session);
  return [
    sessionTabLabel(session),
    session.permission_profile === "unsafe" ? "Unsafe" : "Normal",
    session.output_draining
      ? "Draining output — input closed. Stop remains available."
      : lifecycleLabel(sessionDisplayState(session)),
    session.working_dir,
    ...(appServer ? [appServer] : []),
  ].join(" · ");
}

export function driverLabel(driver: DriverKind): string {
  switch (driver) {
    case "claude":
      return "Claude Code";
    case "codex":
      return "Codex";
    case "grok":
      return "Grok Build";
    case "prime":
      return "Prime Agent (Ubuntu)";
    case "generic_terminal":
      return "Terminal";
  }
}

/** Display override only: the backend lifecycle/run identity remains intact.
    A running member that waits on a person (a question on its screen) shows
    blocked: its lifecycle alone would read ready or idle. */
export function sessionDisplayState(
  session: SleepSnapshot & Pick<SessionSnapshot, "blocked_reason">,
): LifecycleState | "draining" | "blocked" | "sleeping" {
  if (session.output_draining) return "draining";
  if (session.running && session.blocked_reason) return "blocked";
  return isSleeping(session) ? "sleeping" : session.lifecycle_state;
}

export function lifecycleLabel(state: LifecycleState | "draining" | "blocked" | "sleeping"): string {
  if (state === "draining") return "draining output — input closed";
  if (state === "sleeping") return "Sleeping";
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
