/* Georgi themes (Georgi Black / Georgi White): the calmer, status-first shell
   from the Claude Design handoff. Everything here only builds DOM; main.ts
   owns state and passes data plus callbacks. Other themes never show it:
   every element carries .g-only, which georgi.css hides outside these themes. */

import type { DriverKind, RoomSnapshot, SessionSnapshot } from "./types";
import type { SessionReadiness } from "./work-context";

export const GEORGI_THEMES = ["georgi-black", "georgi-white"] as const;

export function isGeorgiTheme(name: string | undefined): boolean {
  return name === "georgi-black" || name === "georgi-white";
}

export type GeorgiStatus = "ready" | "busy" | "needs" | "error" | "stopped" | "starting";

export const STATUS_LABEL: Record<GeorgiStatus, string> = {
  ready: "Ready",
  busy: "Busy",
  needs: "Needs you",
  error: "Error",
  stopped: "Stopped",
  starting: "Starting",
};

/** Readiness kinds onto the design's six states. "blocked" is Needs you,
    except a failed launch, which is an Error the operator cannot answer. */
export function georgiStatus(readiness: SessionReadiness, session: SessionSnapshot | undefined): GeorgiStatus {
  switch (readiness.kind) {
    case "working":
      return "busy";
    case "ready":
      return "ready";
    case "starting":
      return "starting";
    case "blocked":
      return session?.lifecycle_state === "failed" || session?.output_draining ? "error" : "needs";
    default:
      return "stopped";
  }
}

const HARNESS: Record<DriverKind, { mono: string; name: string }> = {
  claude: { mono: "CC", name: "Claude Code" },
  codex: { mono: "CX", name: "Codex" },
  grok: { mono: "GK", name: "Grok" },
  prime: { mono: "P1", name: "Prime" },
  generic_terminal: { mono: ">_", name: "Terminal" },
};

export function harnessMono(driver: DriverKind | undefined): string {
  return driver ? HARNESS[driver]?.mono ?? "??" : "··";
}

export function harnessName(driver: DriverKind | undefined): string {
  return driver ? HARNESS[driver]?.name ?? driver : "Agent";
}

export function roomInitials(label: string): string {
  const words = label.trim().split(/[\s_-]+/).filter(Boolean);
  if (words.length === 0) return "#";
  if (words.length === 1) return words[0].slice(0, 2).toUpperCase();
  return (words[0][0] + words[1][0]).toUpperCase();
}

/** A stable hue slot 1–8 per room id, so a room keeps its colour. */
export function roomHueSlot(roomId: string): number {
  let hash = 0;
  for (const char of roomId) hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  return (hash % 8) + 1;
}

export interface GeorgiMember {
  id: string;
  label: string;
  driver: DriverKind | undefined;
  status: GeorgiStatus;
  reason: string;
  /** Designated Supervisor (all three grants): receives Post to feed. */
  supervisor?: boolean;
}

export interface GeorgiRoom {
  id: string;
  label: string;
  members: GeorgiMember[];
}

export interface GeorgiShellData {
  rooms: GeorgiRoom[];
  loose: GeorgiMember[];
  /** The session whose terminal is on screen, when the terminal view shows. */
  activeSessionId: string | null;
  /** The room whose feed is on screen, when the room view shows. */
  activeRoomId: string | null;
}

export interface GeorgiShellHandlers {
  openRoom(roomId: string): void;
  openSession(sessionId: string): void;
  newRoom(): void;
  newSession(anchor: HTMLElement): void;
  showRooms(): void;
  footer(kind: string): void;
  /** Right-click on an agent: launch, stop, restart, remove… */
  agentMenu?(event: MouseEvent, sessionId: string): void;
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, className?: string, text?: string): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

export interface MenuItem {
  label: string;
  run(): void;
  danger?: boolean;
  disabled?: boolean;
  hint?: string;
}

let openMenu: { node: HTMLElement; close(): void } | null = null;

/** A small right-click menu at the pointer; "-" draws a separator. Closes on
    a click elsewhere, Escape, scroll or window blur. */
export function showContextMenu(x: number, y: number, items: (MenuItem | "-")[], title?: string): void {
  openMenu?.close();
  const menu = el("div", "g-ctx");
  menu.setAttribute("role", "menu");
  if (title) menu.append(el("div", "g-ctx-title", title));
  for (const item of items) {
    if (item === "-") {
      menu.append(el("div", "g-ctx-sep"));
      continue;
    }
    const row = el("button", `g-ctx-item${item.danger ? " g-ctx-danger" : ""}`);
    row.type = "button";
    row.setAttribute("role", "menuitem");
    row.disabled = Boolean(item.disabled);
    row.append(el("span", "", item.label));
    if (item.hint) row.append(el("span", "g-ctx-hint", item.hint));
    row.addEventListener("click", () => {
      close();
      item.run();
    });
    menu.append(row);
  }
  document.body.append(menu);
  const box = menu.getBoundingClientRect();
  menu.style.left = `${Math.max(4, Math.min(x, window.innerWidth - box.width - 4))}px`;
  menu.style.top = `${Math.max(4, Math.min(y, window.innerHeight - box.height - 4))}px`;
  const outside = (event: Event) => {
    if (!menu.contains(event.target as Node)) close();
  };
  const key = (event: KeyboardEvent) => {
    if (event.key === "Escape") close();
  };
  function close(): void {
    menu.remove();
    document.removeEventListener("mousedown", outside, true);
    document.removeEventListener("keydown", key, true);
    window.removeEventListener("blur", close);
    window.removeEventListener("wheel", close, true);
    if (openMenu?.node === menu) openMenu = null;
  }
  setTimeout(() => {
    document.addEventListener("mousedown", outside, true);
    document.addEventListener("keydown", key, true);
    window.addEventListener("blur", close);
    window.addEventListener("wheel", close, true);
  });
  openMenu = { node: menu, close };
  (menu.querySelector("button:not(:disabled)") as HTMLButtonElement | null)?.focus({ preventScroll: true });
}

/** A small dialog asking for one name; resolves null on Cancel or Escape. */
export function askText(title: string, options: { initial?: string; placeholder?: string; action?: string; note?: string } = {}): Promise<string | null> {
  return new Promise((resolve) => {
    document.querySelector(".g-modal-back")?.remove();
    const back = el("div", "g-modal-back");
    const form = el("form", "g-modal g-ask");
    const input = el("input", "g-org-input");
    input.value = options.initial ?? "";
    input.placeholder = options.placeholder ?? "";
    const done = (value: string | null) => {
      back.remove();
      resolve(value);
    };
    const cancel = el("button", "g-btn g-btn-ghost", "Cancel");
    cancel.type = "button";
    cancel.addEventListener("click", () => done(null));
    const ok = el("button", "g-btn g-btn-primary", options.action ?? "Save");
    ok.type = "submit";
    const actions = el("div", "g-org-actions");
    actions.append(el("span", "g-flex"), cancel, ok);
    const head = el("div", "g-modal-head");
    head.append(el("h2", "", title));
    form.append(head, input);
    if (options.note) form.append(el("p", "g-org-note", options.note));
    form.append(actions);
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      const value = input.value.trim();
      if (value) done(value);
      else input.focus({ preventScroll: true });
    });
    form.addEventListener("keydown", (event) => {
      if (event.key === "Escape") done(null);
    });
    back.addEventListener("mousedown", (event) => {
      if (event.target === back) done(null);
    });
    back.append(form);
    document.body.append(back);
    input.focus({ preventScroll: true });
    input.select();
  });
}

export function avatar(member: Pick<GeorgiMember, "driver" | "status">, size: "sm" | "md" | "lg", dot = true): HTMLElement {
  const box = el("span", `g-avatar g-avatar-${size}`, harnessMono(member.driver));
  box.dataset.driver = member.driver ?? "unknown";
  if (dot) {
    const status = el("span", "g-dot");
    status.dataset.status = member.status;
    box.append(status);
  }
  return box;
}

export function statusPill(status: GeorgiStatus): HTMLElement {
  const pill = el("span", "g-pill");
  pill.dataset.status = status;
  pill.append(el("span", "g-pill-dot"), document.createTextNode(STATUS_LABEL[status]));
  return pill;
}

function roomTile(room: { id: string; label: string }, size: "md" | "lg"): HTMLElement {
  const tile = el("span", `g-room-tile g-room-tile-${size}`, roomInitials(room.label));
  tile.dataset.hue = String(roomHueSlot(room.id));
  return tile;
}

const EXPANDED_KEY = "prim1-georgi-collapsed-rooms";

function collapsedRooms(): Set<string> {
  try {
    return new Set(JSON.parse(localStorage.getItem(EXPANDED_KEY) ?? "[]") as string[]);
  } catch {
    return new Set();
  }
}

function setCollapsed(roomId: string, collapsed: boolean): void {
  const set = collapsedRooms();
  if (collapsed) set.add(roomId);
  else set.delete(roomId);
  try {
    localStorage.setItem(EXPANDED_KEY, JSON.stringify([...set]));
  } catch {
    /* preference only */
  }
}

function memberRow(member: GeorgiMember, active: boolean, nested: boolean, handlers: GeorgiShellHandlers): HTMLElement {
  const row = el("button", `g-side-member${nested ? " g-nested" : ""}`);
  row.type = "button";
  row.title = member.reason;
  row.dataset.status = member.status;
  if (active) row.setAttribute("aria-current", "true");
  row.append(avatar(member, "sm"), el("span", "g-side-label", member.label), el("span", "g-side-state", STATUS_LABEL[member.status]));
  row.addEventListener("click", () => handlers.openSession(member.id));
  row.addEventListener("contextmenu", (event) => {
    if (!handlers.agentMenu) return;
    event.preventDefault();
    handlers.agentMenu(event, member.id);
  });
  return row;
}

function roomSummary(room: GeorgiRoom): string {
  if (room.members.length === 0) return "no agents yet";
  const busy = room.members.filter((m) => m.status === "busy").length;
  const running = room.members.filter((m) => m.status !== "stopped").length;
  const agents = `${room.members.length} agent${room.members.length === 1 ? "" : "s"}`;
  if (running === 0) return `${agents} · all stopped`;
  if (busy > 0) return `${agents} · ${busy} working`;
  return agents;
}

export function renderSidebar(
  container: HTMLElement,
  data: GeorgiShellData,
  handlers: GeorgiShellHandlers,
  /** The portfolio tree, when grouping is on; replaces the flat room list. */
  roomsSection?: HTMLElement,
): void {
  const scroll = container.querySelector<HTMLElement>(".g-side-scroll");
  const keepScroll = scroll?.scrollTop ?? 0;
  const collapsed = collapsedRooms();
  const body = el("div", "g-side-scroll");

  const roomsGroup = roomsSection ?? el("section", "g-side-group");
  if (!roomsSection) {
  const roomsHead = el("div", "g-side-head");
  const roomsTitle = el("button", "g-side-title", "Rooms");
  roomsTitle.type = "button";
  roomsTitle.title = "All rooms";
  roomsTitle.addEventListener("click", handlers.showRooms);
  const addRoom = el("button", "g-icon-btn", "+");
  addRoom.type = "button";
  addRoom.title = "New room";
  addRoom.addEventListener("click", handlers.newRoom);
  roomsHead.append(roomsTitle, addRoom);
  roomsGroup.append(roomsHead);
  if (data.rooms.length === 0) {
    const empty = el("button", "g-side-empty", "No rooms yet. Create one to run agents as a team.");
    empty.type = "button";
    empty.addEventListener("click", handlers.newRoom);
    roomsGroup.append(empty);
  }
  for (const room of data.rooms) {
    const isCollapsed = collapsed.has(room.id);
    const wrap = el("div", "g-side-room");
    const head = el("div", "g-side-room-head");
    if (data.activeRoomId === room.id) head.setAttribute("aria-current", "true");
    const open = el("button", "g-side-room-open");
    open.type = "button";
    const text = el("span", "g-side-room-text");
    const sub = el("span", "g-side-room-sub");
    const dots = el("span", "g-side-dots");
    for (const member of room.members) {
      const dot = el("span", "g-mini-dot");
      dot.dataset.status = member.status;
      dots.append(dot);
    }
    sub.append(dots, el("span", "g-ellipsis", roomSummary(room)));
    text.append(el("span", "g-side-room-name", room.label), sub);
    open.append(roomTile(room, "md"), text);
    const needs = room.members.filter((m) => m.status === "needs").length;
    if (needs > 0) open.append(el("span", "g-count", String(needs)));
    open.addEventListener("click", () => handlers.openRoom(room.id));
    const toggle = el("button", "g-chevron", isCollapsed ? "▸" : "▾");
    toggle.type = "button";
    toggle.title = isCollapsed ? "Show members" : "Hide members";
    toggle.addEventListener("click", () => {
      setCollapsed(room.id, !isCollapsed);
      renderSidebar(container, data, handlers);
    });
    head.append(open, toggle);
    wrap.append(head);
    if (!isCollapsed && room.members.length > 0) {
      const list = el("div", "g-side-members");
      for (const member of room.members) {
        list.append(memberRow(member, data.activeSessionId === member.id, true, handlers));
      }
      wrap.append(list);
    }
    roomsGroup.append(wrap);
  }
  }

  const looseGroup = el("section", "g-side-group");
  const looseHead = el("div", "g-side-head");
  const looseTitle = el("span", "g-side-title", "Single sessions");
  const addSession = el("button", "g-icon-btn", "+");
  addSession.type = "button";
  addSession.title = "New session";
  addSession.addEventListener("click", () => handlers.newSession(addSession));
  looseHead.append(looseTitle, addSession);
  looseGroup.append(looseHead);
  if (data.loose.length === 0) {
    looseGroup.append(el("span", "g-side-hint", "Sessions outside a room appear here."));
  }
  for (const member of data.loose) {
    looseGroup.append(memberRow(member, data.activeSessionId === member.id, false, handlers));
  }
  body.append(roomsGroup, looseGroup);

  const foot = el("nav", "g-side-foot");
  for (const [kind, label, hint] of [
    ["tasks", "Tasks", ""],
    ["telemetry", "Telemetry", ""],
    ["themes", "Themes", ""],
    ["settings", "Settings", ""],
    ["help", "Help", "F1"],
    ["log", "System log", ""],
  ] as const) {
    const item = el("button", "g-side-foot-item");
    item.type = "button";
    item.dataset.gFooter = kind;
    item.append(el("span", "", label));
    if (hint) item.append(el("span", "g-kbd-hint", hint));
    item.addEventListener("click", () => handlers.footer(kind));
    foot.append(item);
  }
  container.replaceChildren(body, foot);
  body.scrollTop = keepScroll;
}

/** The top bar's live summary: "1 needs you · 2 busy · 3 ready". */
export function renderSummary(
  container: HTMLElement,
  members: GeorgiMember[],
  jumpTo: (sessionId: string) => void,
  /** Open @Georgi asks and question/decision cards across rooms. */
  asks = 0,
  openInbox?: () => void,
): void {
  const parts: HTMLElement[] = [];
  if (asks > 0 && openInbox) {
    const chip = el("button", "g-summary-chip g-summary-asks");
    chip.type = "button";
    chip.dataset.status = "needs";
    chip.title = "Agents are waiting on you — open Needs you";
    chip.append(el("span", "g-pill-dot"), document.createTextNode(`${asks} for you`));
    chip.addEventListener("click", openInbox);
    parts.push(chip);
  }
  const groups: [GeorgiStatus, string][] = [
    ["needs", "needs you"],
    ["error", "error"],
    ["busy", "busy"],
    ["ready", "ready"],
  ];
  for (const [status, words] of groups) {
    const matching = members.filter((m) => m.status === status);
    if (matching.length === 0) continue;
    const chip = el("button", "g-summary-chip");
    chip.type = "button";
    chip.dataset.status = status;
    chip.title = matching.map((m) => m.label).join(", ");
    chip.append(el("span", "g-pill-dot"), document.createTextNode(`${matching.length} ${words}`));
    chip.addEventListener("click", () => jumpTo(matching[0].id));
    parts.push(chip);
  }
  if (parts.length === 0) {
    parts.push(el("span", "g-summary-empty", members.length === 0 ? "No sessions yet" : "No sessions running"));
  }
  container.replaceChildren(...parts);
}

/** The room header's member chips; a click opens that member's terminal. */
export function renderMemberChips(
  container: HTMLElement,
  members: GeorgiMember[],
  open: (sessionId: string) => void,
  menu?: (event: MouseEvent, sessionId: string) => void,
): void {
  const chips = members.map((member) => {
    const chip = el("button", "g-chip");
    chip.type = "button";
    chip.dataset.status = member.status;
    chip.title = `${member.reason} — open terminal`;
    chip.append(avatar(member, "sm", false), el("span", "g-chip-label", member.label));
    if (member.supervisor) {
      const crown = el("span", "g-crown", "♛");
      crown.title = "Supervisor: receives Post to feed";
      chip.append(crown);
    }
    chip.append(statusPill(member.status));
    chip.addEventListener("click", () => open(member.id));
    if (menu) {
      chip.title = `${member.reason} — click: terminal · right-click: launch, stop, remove…`;
      chip.addEventListener("contextmenu", (event) => {
        event.preventDefault();
        menu(event, member.id);
      });
    }
    return chip;
  });
  container.replaceChildren(...chips);
}

export interface JumpTarget {
  kind: "room" | "session";
  id: string;
  label: string;
  detail: string;
}

/** Ctrl K: a small filter over rooms and sessions. */
export function wireJump(input: HTMLInputElement, list: HTMLElement, targets: () => JumpTarget[], go: (target: JumpTarget) => void): void {
  let matches: JumpTarget[] = [];
  let index = 0;
  const close = () => {
    list.hidden = true;
  };
  const paint = () => {
    const query = input.value.trim().toLowerCase();
    matches = targets().filter((t) => !query || t.label.toLowerCase().includes(query) || t.detail.toLowerCase().includes(query)).slice(0, 10);
    index = Math.min(index, Math.max(0, matches.length - 1));
    list.replaceChildren(
      ...(matches.length
        ? matches.map((target, i) => {
            const row = el("button", "g-jump-row");
            row.type = "button";
            if (i === index) row.setAttribute("aria-selected", "true");
            row.append(el("span", "g-jump-kind", target.kind === "room" ? "#" : ">_"), el("span", "g-jump-label", target.label), el("span", "g-jump-detail", target.detail));
            row.addEventListener("mousedown", (event) => {
              event.preventDefault();
              pick(target);
            });
            return row;
          })
        : [el("span", "g-jump-none", "Nothing matches")]),
    );
    list.hidden = false;
  };
  const pick = (target: JumpTarget) => {
    input.value = "";
    close();
    input.blur();
    go(target);
  };
  input.addEventListener("focus", () => {
    index = 0;
    paint();
  });
  input.addEventListener("input", () => {
    index = 0;
    paint();
  });
  input.addEventListener("blur", () => setTimeout(close, 120));
  input.addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (matches.length) index = (index + (event.key === "ArrowDown" ? 1 : matches.length - 1)) % matches.length;
      paint();
    } else if (event.key === "Enter") {
      event.preventDefault();
      if (matches[index]) pick(matches[index]);
    } else if (event.key === "Escape") {
      input.value = "";
      close();
      input.blur();
    }
  });
}

export function georgiMember(
  sessionId: string,
  session: SessionSnapshot | undefined,
  readiness: SessionReadiness,
  label: string,
): GeorgiMember {
  const grants = session?.permissions;
  const supervisor = Boolean(grants?.roam && grants?.maintain && grants?.manage);
  return { id: sessionId, label, driver: session?.driver, status: georgiStatus(readiness, session), reason: readiness.reason,
    ...(supervisor ? { supervisor } : {}) };
}

export function roomFolder(room: RoomSnapshot, sessions: Map<string, SessionSnapshot>): string {
  const first = room.member_ids.map((id) => sessions.get(id)?.working_dir).find(Boolean);
  return first ?? "";
}

/* ── Rooms overview cards ── */

export interface GeorgiRoomCardData {
  id: string;
  label: string;
  members: GeorgiMember[];
  last: string;
  folder: string;
  current: boolean;
}

function roomCounts(members: GeorgiMember[]): string {
  if (members.length === 0) return "No agents yet";
  const order: GeorgiStatus[] = ["busy", "needs", "error", "starting", "ready", "stopped"];
  const words: Record<GeorgiStatus, string> = {
    busy: "busy", needs: "needs you", error: "error", starting: "starting", ready: "ready", stopped: "stopped",
  };
  return order
    .map((status) => [status, members.filter((m) => m.status === status).length] as const)
    .filter(([, n]) => n > 0)
    .map(([status, n]) => `${n} ${words[status]}`)
    .join(" · ");
}

export function georgiRoomCard(room: GeorgiRoomCardData, open: () => void): HTMLElement {
  const card = el("button", "g-room-card g-only");
  card.type = "button";
  const needs = room.members.filter((m) => m.status === "needs").length;
  if (needs > 0) card.dataset.needs = "true";
  if (room.current) card.dataset.current = "true";
  const head = el("div", "g-room-card-head");
  const tile = el("span", "g-room-tile g-room-tile-md", roomInitials(room.label));
  tile.dataset.hue = String(roomHueSlot(room.id));
  head.append(tile, el("span", "g-room-card-name", room.label));
  if (needs > 0) head.append(el("span", "g-room-card-badge", `${needs} needs you`));
  const faces = el("div", "g-room-card-faces");
  for (const member of room.members) {
    const face = avatar(member, "md");
    face.title = `${member.label} · ${STATUS_LABEL[member.status]}`;
    faces.append(face);
  }
  const allStopped = room.members.length > 0 && room.members.every((m) => m.status === "stopped");
  const last = el("p", "g-room-card-last", room.last || (allStopped ? "All members stopped" : room.members.length ? "No messages yet" : "Add agents to start working together"));
  const foot = el("div", "g-room-card-foot");
  foot.append(el("span", "", roomCounts(room.members)), el("span", "g-mono g-room-card-folder", room.folder));
  card.append(head, faces, last, foot);
  card.addEventListener("click", open);
  return card;
}
