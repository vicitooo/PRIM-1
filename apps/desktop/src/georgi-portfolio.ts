/* Georgi themes: Portfolio → Project → Room in the sidebar.

   PRIM-1 itself only knows rooms. Portfolio, project and room type are
   labels the operator puts on rooms here, kept in this window's storage;
   they change how the sidebar is grouped and nothing else. A portfolio has
   no agents. Each project can mark one room as its Lead room. */

import { askText, avatar, showContextMenu, STATUS_LABEL, type GeorgiMember, type MenuItem } from "./georgi-ui";

export type RoomType = "lead" | "dev" | "seo" | "mkt" | "legal" | "ops" | "custom";

export const ROOM_TYPES: Record<RoomType, { name: string; icon: string }> = {
  lead: { name: "Lead", icon: "◆" },
  dev: { name: "Development", icon: "</>" },
  seo: { name: "SEO", icon: "↗" },
  mkt: { name: "Marketing", icon: "◎" },
  legal: { name: "Legal", icon: "§" },
  ops: { name: "Ops", icon: "≡" },
  custom: { name: "Custom", icon: "#" },
};

const TYPE_ORDER: RoomType[] = ["lead", "dev", "seo", "mkt", "legal", "ops", "custom"];

export type RoomFilter = "all" | "dev" | "seo" | "mkt" | "legal";

export const FILTERS: [RoomFilter, string][] = [
  ["all", "All"],
  ["dev", "Dev"],
  ["seo", "SEO"],
  ["mkt", "Marketing"],
  ["legal", "Legal"],
];

export interface RoomPlacement {
  portfolio: string;
  project: string;
  type: RoomType;
}

export interface Grouping {
  version: 1;
  rooms: Record<string, RoomPlacement>;
  /** Room ids the operator chose to leave ungrouped (never seeded again). */
  loose: string[];
  collapsed: string[];
  portfolio: string;
  filter: RoomFilter;
  /** "room:<id>" and "project:<portfolio>/<project>" stay on top. */
  pinned: string[];
  /** The sidebar folded into a 60px rail. */
  rail: boolean;
  /** Portfolios created before any project was moved in (kept while empty). */
  portfolios: string[];
  /** "recent": latest activity first, like a chat. "manual": your drag order. */
  sort: "recent" | "manual";
  /** Keys in your chosen order: "portfolio:<name>", "project:<pf>/<p>", "room:<id>". */
  order: string[];
}

export const portfolioKey = (name: string) => `portfolio:${name}`;

/** Put `moving` just above `before` among `siblings` (keys in on-screen
    order), switch to your own order, and keep the rest as it was. */
export function placeBefore(grouping: Grouping, siblings: string[], moving: string, before: string): void {
  if (moving === before) return;
  const list = siblings.filter((key) => key !== moving);
  const at = list.indexOf(before);
  list.splice(at < 0 ? list.length : at, 0, moving);
  const set = new Set(list);
  grouping.order = [...grouping.order.filter((key) => !set.has(key)), ...list];
  grouping.sort = "manual";
}

/** Every portfolio name: those holding rooms plus empty ones you created. */
export function portfolioNames(grouping: Grouping): string[] {
  return [...new Set([...Object.values(grouping.rooms).map((r) => r.portfolio), ...grouping.portfolios])]
    .sort((a, b) => a.localeCompare(b, undefined, { sensitivity: "base" }));
}

export function addPortfolio(grouping: Grouping, name: string): void {
  if (!portfolioNames(grouping).includes(name)) grouping.portfolios = [...grouping.portfolios, name];
}

/** Move a whole project (every room in it) into another portfolio. */
export function moveProject(grouping: Grouping, from: string, project: string, to: string): void {
  if (from === to) return;
  for (const place of Object.values(grouping.rooms)) {
    if (place.portfolio === from && place.project === project) place.portfolio = to;
  }
  const rekey = (key: string) => (key === projectPin(from, project) ? projectPin(to, project) : key);
  grouping.pinned = grouping.pinned.map(rekey);
  grouping.collapsed = grouping.collapsed.map((key) => (key === `${from}/${project}` ? `${to}/${project}` : key));
  grouping.portfolios = grouping.portfolios.filter((name) => name !== to);
  if (!Object.values(grouping.rooms).some((r) => r.portfolio === from) && !grouping.portfolios.includes(from)) {
    grouping.portfolios = [...grouping.portfolios, from];
  }
}

export function renamePortfolio(grouping: Grouping, from: string, to: string): void {
  if (from === to) return;
  for (const place of Object.values(grouping.rooms)) if (place.portfolio === from) place.portfolio = to;
  grouping.portfolios = [...new Set(grouping.portfolios.map((name) => (name === from ? to : name)))];
  grouping.pinned = grouping.pinned.map((key) => (key.startsWith(`project:${from}/`) ? `project:${to}/${key.slice(`project:${from}/`.length)}` : key));
  grouping.collapsed = grouping.collapsed.map((key) => (key === `portfolio:${from}` ? `portfolio:${to}`
    : key.startsWith(`${from}/`) ? `${to}/${key.slice(from.length + 1)}` : key));
  if (grouping.portfolio === from) grouping.portfolio = to;
}

export function renameProject(grouping: Grouping, portfolio: string, from: string, to: string): void {
  if (from === to) return;
  for (const place of Object.values(grouping.rooms)) {
    if (place.portfolio === portfolio && place.project === from) place.project = to;
  }
  grouping.pinned = grouping.pinned.map((key) => (key === projectPin(portfolio, from) ? projectPin(portfolio, to) : key));
  grouping.collapsed = grouping.collapsed.map((key) => (key === `${portfolio}/${from}` ? `${portfolio}/${to}` : key));
}

/** Drop an empty portfolio you no longer want (one with rooms stays). */
export function removeEmptyPortfolio(grouping: Grouping, name: string): void {
  grouping.portfolios = grouping.portfolios.filter((p) => p !== name);
  if (grouping.portfolio === name) grouping.portfolio = ALL_PORTFOLIOS;
}

export const roomPin = (roomId: string) => `room:${roomId}`;
export const projectPin = (portfolio: string, project: string) => `project:${portfolio}/${project}`;

export function togglePin(grouping: Grouping, key: string): void {
  grouping.pinned = grouping.pinned.includes(key) ? grouping.pinned.filter((k) => k !== key) : [...grouping.pinned, key];
}

export const ALL_PORTFOLIOS = "*";
const KEY = "prim1-georgi-grouping";

export function loadGrouping(): Grouping {
  try {
    const raw = JSON.parse(localStorage.getItem(KEY) ?? "null") as Partial<Grouping> | null;
    if (raw && raw.version === 1) {
      return {
        version: 1,
        rooms: raw.rooms ?? {},
        loose: raw.loose ?? [],
        collapsed: raw.collapsed ?? [],
        portfolio: raw.portfolio ?? ALL_PORTFOLIOS,
        filter: raw.filter ?? "all",
        pinned: raw.pinned ?? [],
        rail: raw.rail ?? false,
        portfolios: raw.portfolios ?? [],
        sort: raw.sort ?? "recent",
        order: raw.order ?? [],
      };
    }
  } catch {
    /* fall through to a fresh grouping */
  }
  return { version: 1, rooms: {}, loose: [], collapsed: [], portfolio: ALL_PORTFOLIOS, filter: "all", pinned: [], rail: false, portfolios: [], sort: "recent", order: [] };
}

export function saveGrouping(grouping: Grouping): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(grouping));
  } catch {
    /* preference only */
  }
}

/** A first guess for a room nobody has placed yet, from its label. */
export function seedPlacement(label: string): RoomPlacement | null {
  const rules: [RegExp, RoomPlacement][] = [
    [/notes/i, { portfolio: "Apps", project: "Notes", type: "lead" }],
    [/note\s*taker/i, { portfolio: "Apps", project: "Notes", type: "dev" }],
    [/dream/i, { portfolio: "Apps", project: "Dreams", type: "dev" }],
    [/reel/i, { portfolio: "Apps", project: "ReelStudio", type: "dev" }],
    [/spice/i, { portfolio: "Apps", project: "SpiceRack", type: "dev" }],
    [/fable/i, { portfolio: "Apps", project: "Fables", type: "dev" }],
    [/seaside/i, { portfolio: "Seaside", project: "Seaside", type: "dev" }],
    [/colum/i, { portfolio: "Business", project: "Columns", type: "dev" }],
    [/funnel/i, { portfolio: "Business", project: "Funnels", type: "dev" }],
    [/parcel/i, { portfolio: "Business", project: "Parcel", type: "dev" }],
  ];
  return rules.find(([pattern]) => pattern.test(label))?.[1] ?? null;
}

/** Place every room not yet placed (or deliberately left loose) by its
    label; returns whether anything changed. */
export function seedGrouping(grouping: Grouping, rooms: readonly { id: string; label: string }[]): boolean {
  let changed = false;
  for (const room of rooms) {
    if (grouping.rooms[room.id] || grouping.loose.includes(room.id)) continue;
    const seed = seedPlacement(room.label);
    if (seed) {
      grouping.rooms[room.id] = { ...seed };
      changed = true;
    }
  }
  return changed;
}

/** The room's name inside its project: "Seaside-Backend" in Seaside reads
    "Backend"; a label that is only the project name reads as its type. */
export function roomNameInProject(label: string, project: string, type: RoomType): string {
  const words = project.trim().toLowerCase();
  const lower = label.toLowerCase();
  if (words && lower.startsWith(words)) {
    const rest = label.slice(project.trim().length).replace(/^[\s\-–—·:/_]+/, "").trim();
    return rest || ROOM_TYPES[type].name;
  }
  return label;
}

export interface TreeRoom {
  id: string;
  label: string;
  name: string;
  type: RoomType;
  project: string;
  portfolio: string;
  members: GeorgiMember[];
  needs: number;
  /** ISO time of the room's latest feed event ("" when unknown). */
  lastActivity: string;
  pinned: boolean;
}

export interface TreeProject {
  name: string;
  portfolio: string;
  rooms: TreeRoom[];
  needs: number;
  agents: number;
  running: number;
  working: number;
  lastActivity: string;
  pinned: boolean;
}

export interface TreePortfolio {
  name: string;
  /** The portfolio's supervisor room(s): a Lead room with no project of its
      own, shown on top of the portfolio. */
  leads: TreeRoom[];
  projects: TreeProject[];
  needs: number;
  agents: number;
  running: number;
  lastActivity: string;
}

/** Like a chat list: pinned first, then the most recent activity, then name. */
function byRecency<T extends { pinned?: boolean; lastActivity: string }>(tie: (a: T, b: T) => number,
  rank?: (item: T) => number) {
  return (a: T, b: T) => Number(Boolean(b.pinned)) - Number(Boolean(a.pinned))
    || (rank ? rank(a) - rank(b) : 0)
    || b.lastActivity.localeCompare(a.lastActivity)
    || tie(a, b);
}

const latest = (times: string[]) => times.reduce((max, t) => (t > max ? t : max), "");

export interface Tree {
  portfolios: TreePortfolio[];
  ungrouped: TreeRoom[];
}

const running = (m: GeorgiMember) => m.status !== "stopped";

export function buildTree(
  rooms: readonly { id: string; label: string; members: GeorgiMember[] }[],
  grouping: Grouping,
  needsByRoom: ReadonlyMap<string, number>,
  activity: ReadonlyMap<string, string> = new Map(),
): Tree {
  const portfolios = new Map<string, Map<string, TreeRoom[]>>();
  const leadsOf = new Map<string, TreeRoom[]>();
  const ungrouped: TreeRoom[] = [];
  const pins = new Set(grouping.pinned);
  // Your own order (after a drag): its position; otherwise latest activity.
  const position = new Map(grouping.order.map((key, index) => [key, index]));
  const manual = grouping.sort === "manual";
  const rankOf = (key: string) => (manual ? position.get(key) ?? Number.MAX_SAFE_INTEGER : 0);
  for (const room of rooms) {
    const place = grouping.rooms[room.id];
    const needs = (needsByRoom.get(room.id) ?? 0) + room.members.filter((m) => m.status === "needs").length;
    const lastActivity = activity.get(room.id) ?? "";
    const pinned = pins.has(roomPin(room.id));
    if (!place) {
      ungrouped.push({ id: room.id, label: room.label, name: room.label, type: "custom", project: "", portfolio: "", members: room.members, needs, lastActivity, pinned });
      continue;
    }
    if (place.type === "lead" && (!place.project || place.project === place.portfolio)) {
      leadsOf.set(place.portfolio, [...(leadsOf.get(place.portfolio) ?? []), {
        id: room.id, label: room.label, name: room.label, type: "lead", project: "", portfolio: place.portfolio,
        members: room.members, needs, lastActivity, pinned,
      }]);
      if (!portfolios.has(place.portfolio)) portfolios.set(place.portfolio, new Map());
      continue;
    }
    const projects = portfolios.get(place.portfolio) ?? new Map<string, TreeRoom[]>();
    portfolios.set(place.portfolio, projects);
    const list = projects.get(place.project) ?? [];
    projects.set(place.project, list);
    list.push({
      id: room.id,
      label: room.label,
      name: roomNameInProject(room.label, place.project, place.type),
      type: place.type,
      project: place.project,
      portfolio: place.portfolio,
      members: room.members,
      needs,
      lastActivity,
      pinned,
    });
  }
  const byName = (a: string, b: string) => a.localeCompare(b, undefined, { sensitivity: "base" });
  const tree: TreePortfolio[] = [...portfolios.entries()].map(([name, projects]) => {
    const list: TreeProject[] = [...projects.entries()].map(([project, roomList]) => {
      roomList.sort(byRecency<TreeRoom>((a, b) => TYPE_ORDER.indexOf(a.type) - TYPE_ORDER.indexOf(b.type) || byName(a.name, b.name),
        (r) => rankOf(roomPin(r.id))));
      const members = roomList.flatMap((r) => r.members);
      return {
        name: project,
        portfolio: name,
        rooms: roomList,
        needs: roomList.reduce((sum, r) => sum + r.needs, 0),
        agents: members.length,
        running: members.filter(running).length,
        working: members.filter((m) => m.status === "busy").length,
        lastActivity: latest(roomList.map((r) => r.lastActivity)),
        pinned: pins.has(projectPin(name, project)),
      };
    });
    list.sort(byRecency<TreeProject>((a, b) => byName(a.name, b.name), (p) => rankOf(projectPin(p.portfolio, p.name))));
    const leads = leadsOf.get(name) ?? [];
    const leadMembers = leads.flatMap((r) => r.members);
    return {
      name,
      leads,
      projects: list,
      needs: list.reduce((sum, p) => sum + p.needs, 0) + leads.reduce((sum, r) => sum + r.needs, 0),
      agents: list.reduce((sum, p) => sum + p.agents, 0) + leadMembers.length,
      running: list.reduce((sum, p) => sum + p.running, 0) + leadMembers.filter(running).length,
      lastActivity: latest([...list.map((p) => p.lastActivity), ...leads.map((r) => r.lastActivity)]),
    };
  });
  for (const name of grouping.portfolios) {
    if (!tree.some((p) => p.name === name)) tree.push({ name, leads: [], projects: [], needs: 0, agents: 0, running: 0, lastActivity: "" });
  }
  tree.sort(byRecency<TreePortfolio>((a, b) => byName(a.name, b.name), (p) => rankOf(portfolioKey(p.name))));
  ungrouped.sort(byRecency<TreeRoom>((a, b) => byName(a.name, b.name), (r) => rankOf(roomPin(r.id))));
  return { portfolios: tree, ungrouped };
}

/** "5 agents · 2 running" — never alarming; "all stopped" is a normal state. */
export function projectSummary(project: Pick<TreeProject, "agents" | "running" | "working">): string {
  if (project.agents === 0) return "no agents yet";
  const agents = `${project.agents} agent${project.agents === 1 ? "" : "s"}`;
  if (project.working > 0) return `${agents} · ${project.working} working`;
  if (project.running > 0) return `${agents} · ${project.running} running`;
  return `${agents} · all stopped`;
}

/* ── Sidebar DOM ── */

export interface PortfolioHandlers {
  openRoom(roomId: string): void;
  newRoom(project: { portfolio: string; project: string } | null): void;
  openInbox(): void;
  openProject(portfolio: string, project: string): void;
  openPortfolio(name: string): void;
  /** Ask for a project name, then create its first room from a template. */
  newProject(portfolio: string): void;
  openSession(sessionId: string): void;
  agentMenu(event: MouseEvent, sessionId: string): void;
  changed(): void;
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, className?: string, text?: string): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function button(className: string, text?: string): HTMLButtonElement {
  const node = el("button", className, text);
  node.type = "button";
  return node;
}

function initials(name: string): string {
  const words = name.trim().split(/[\s_\-–]+/).filter(Boolean);
  if (words.length === 0) return "#";
  if (words.length === 1) return words[0].slice(0, 2).toUpperCase();
  return (words[0][0] + words[1][0]).toUpperCase();
}

function hueSlot(name: string): number {
  let hash = 0;
  for (const char of name) hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  return (hash % 8) + 1;
}

function tile(name: string, size: "sm" | "md"): HTMLElement {
  const node = el("span", `g-room-tile g-pf-tile g-pf-tile-${size}`, initials(name));
  node.dataset.hue = String(hueSlot(name));
  return node;
}

function badge(count: number, className = "g-count"): HTMLElement | null {
  return count > 0 ? el("span", className, String(count)) : null;
}

function dots(members: GeorgiMember[]): HTMLElement {
  const box = el("span", "g-side-dots");
  for (const member of members) {
    const dot = el("span", "g-mini-dot");
    dot.dataset.status = member.status;
    dot.title = `${member.label}: ${member.reason}`;
    box.append(dot);
  }
  return box;
}

let dragRoom: string | null = null;
let dragPortfolio: string | null = null;

function reorderSlot(node: HTMLElement, accepts: () => boolean, drop: () => void): void {
  node.addEventListener("dragover", (event) => {
    if (!accepts()) return;
    event.preventDefault();
    event.stopPropagation();
    node.classList.add("g-drop-before");
  });
  node.addEventListener("dragleave", () => node.classList.remove("g-drop-before"));
  node.addEventListener("drop", (event) => {
    node.classList.remove("g-drop-before");
    if (!accepts()) return;
    event.preventDefault();
    event.stopPropagation();
    drop();
  });
}
let dragProject: { portfolio: string; project: string } | null = null;

/** A portfolio accepts a project dropped on it: the whole project moves. */
function projectDropTarget(node: HTMLElement, portfolio: string, onMove: () => void, grouping: Grouping): void {
  node.addEventListener("dragover", (event) => {
    if (!dragProject || dragProject.portfolio === portfolio) return;
    event.preventDefault();
    node.classList.add("g-drop-on");
  });
  node.addEventListener("dragleave", (event) => {
    if (!node.contains(event.relatedTarget as Node | null)) node.classList.remove("g-drop-on");
  });
  node.addEventListener("drop", (event) => {
    node.classList.remove("g-drop-on");
    const moving = dragProject;
    dragProject = null;
    if (!moving || moving.portfolio === portfolio) return;
    event.preventDefault();
    event.stopPropagation();
    moveProject(grouping, moving.portfolio, moving.project, portfolio);
    onMove();
  });
}

/** Ask for a new portfolio name; create it (empty until a project moves in). */
export async function promptNewPortfolio(grouping: Grouping): Promise<string | null> {
  const name = await askText("New portfolio", {
    placeholder: "e.g. Parcel, Seaside, Clients",
    action: "Create",
    note: "A portfolio groups projects in the sidebar. Move projects into it with right-click → Move to, or drag a project onto it.",
  });
  if (!name) return null;
  addPortfolio(grouping, name);
  return name;
}
let pinChanged: (key: string) => void = () => undefined;

/** The open room's agents under it: click for the terminal, right-click
    for launch, stop, restart or remove. */
function memberRows(members: GeorgiMember[], activeSessionId: string | null, handlers: PortfolioHandlers): HTMLElement {
  const list = el("div", "g-pf-members");
  for (const member of members) {
    const row = button("g-side-member g-nested g-pf-member");
    row.title = `${member.reason} — right-click: launch, stop, restart, remove`;
    row.dataset.status = member.status;
    if (activeSessionId === member.id) row.setAttribute("aria-current", "true");
    row.append(avatar(member, "sm"), el("span", "g-side-label", member.label));
    if (member.supervisor) row.append(el("span", "g-crown", "♛"));
    row.append(el("span", "g-side-state", STATUS_LABEL[member.status]));
    row.addEventListener("click", () => handlers.openSession(member.id));
    row.addEventListener("contextmenu", (event) => {
      event.preventDefault();
      handlers.agentMenu(event, member.id);
    });
    list.append(row);
  }
  return list;
}

function roomRow(room: TreeRoom, activeRoomId: string | null, handlers: PortfolioHandlers, flat: boolean): HTMLElement {
  const row = button(`g-pf-room${flat ? " g-pf-room-flat" : ""}`);
  row.draggable = true;
  row.dataset.roomId = room.id;
  row.title = room.label;
  if (activeRoomId === room.id) row.setAttribute("aria-current", "true");
  const icon = el("span", "g-pf-icon", ROOM_TYPES[room.type].icon);
  icon.title = ROOM_TYPES[room.type].name;
  const name = el("span", "g-pf-room-name");
  if (flat) {
    name.append(el("span", "g-pf-flat-project", room.project || room.label));
    if (room.project) name.append(el("span", "g-pf-flat-room", room.name));
  } else {
    name.textContent = room.name;
  }
  row.append(icon, name);
  if (room.pinned) row.append(el("span", "g-pin", "📌"));
  if (room.type === "lead") row.append(el("span", "g-lead-badge", "LEAD"));
  row.append(dots(room.members));
  const count = badge(room.needs);
  if (count) row.append(count);
  row.addEventListener("click", () => handlers.openRoom(room.id));
  row.addEventListener("contextmenu", (event) => {
    event.preventDefault();
    showContextMenu(event.clientX, event.clientY, [
      { label: "Open room", run: () => handlers.openRoom(room.id) },
      { label: room.pinned ? "Unpin" : "Pin to top", run: () => pinChanged(roomPin(room.id)) },
      ...(room.project ? [{ label: `Open ${room.project} overview`, run: () => handlers.openProject(room.portfolio, room.project) }] : []),
    ], room.label);
  });
  row.addEventListener("dragstart", (event) => {
    dragRoom = room.id;
    event.dataTransfer?.setData("text/plain", room.id);
    if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
    row.classList.add("g-dragging");
  });
  row.addEventListener("dragend", () => {
    dragRoom = null;
    row.classList.remove("g-dragging");
  });
  return row;
}

function dropTarget(node: HTMLElement, onDrop: (roomId: string) => void): void {
  node.addEventListener("dragover", (event) => {
    if (!dragRoom) return;
    event.preventDefault();
    node.classList.add("g-drop-on");
  });
  node.addEventListener("dragleave", (event) => {
    if (!node.contains(event.relatedTarget as Node | null)) node.classList.remove("g-drop-on");
  });
  node.addEventListener("drop", (event) => {
    event.preventDefault();
    node.classList.remove("g-drop-on");
    const id = dragRoom ?? event.dataTransfer?.getData("text/plain");
    dragRoom = null;
    if (id) onDrop(id);
  });
}

/** Move a room into a project (keeping its type), or out of every project. */
export function placeRoom(grouping: Grouping, roomId: string, target: { portfolio: string; project: string } | null): void {
  if (!target) {
    delete grouping.rooms[roomId];
    if (!grouping.loose.includes(roomId)) grouping.loose.push(roomId);
    return;
  }
  const type = grouping.rooms[roomId]?.type ?? "dev";
  grouping.rooms[roomId] = { portfolio: target.portfolio, project: target.project, type };
  grouping.loose = grouping.loose.filter((id) => id !== roomId);
}

export interface PortfolioView {
  tree: Tree;
  grouping: Grouping;
  activeRoomId: string | null;
  activeSessionId: string | null;
  totalNeeds: number;
}

/** The sidebar's rooms area: portfolio switcher, Needs you, filter chips and
    the project tree (or a flat list while a filter is on). */
export function renderPortfolioRooms(view: PortfolioView, handlers: PortfolioHandlers): HTMLElement {
  const { tree, grouping } = view;
  pinChanged = (key) => {
    togglePin(grouping, key);
    handlers.changed();
  };
  if (grouping.rail) return renderRail(view, handlers);
  const section = el("section", "g-pf");
  const known = new Set(tree.portfolios.map((p) => p.name));
  if (grouping.portfolio !== ALL_PORTFOLIOS && !known.has(grouping.portfolio)) grouping.portfolio = ALL_PORTFOLIOS;
  const shown = grouping.portfolio === ALL_PORTFOLIOS ? tree.portfolios : tree.portfolios.filter((p) => p.name === grouping.portfolio);

  // Portfolio switcher.
  const head = el("div", "g-pf-switch");
  const current = button("g-pf-current");
  const all = grouping.portfolio === ALL_PORTFOLIOS;
  const projects = shown.reduce((n, p) => n + p.projects.length, 0);
  const roomCount = shown.reduce((n, p) => n + p.projects.reduce((m, q) => m + q.rooms.length, 0), 0);
  current.append(
    all ? el("span", "g-room-tile g-pf-tile g-pf-tile-md g-pf-all", "All") : tile(grouping.portfolio, "md"),
    (() => {
      const text = el("span", "g-pf-current-text");
      text.append(
        el("span", "g-pf-current-name", all ? "All portfolios" : grouping.portfolio),
        el("span", "g-pf-current-sub", all
          ? `${tree.portfolios.length} portfolio${tree.portfolios.length === 1 ? "" : "s"} · ${projects} project${projects === 1 ? "" : "s"}`
          : `${projects} project${projects === 1 ? "" : "s"} · ${roomCount} room${roomCount === 1 ? "" : "s"}`),
      );
      return text;
    })(),
    el("span", "g-pf-caret", "⌄"),
  );
  const menu = el("div", "g-pf-menu");
  menu.hidden = true;
  const choices: [string, string][] = [[ALL_PORTFOLIOS, "All portfolios"], ...tree.portfolios.map((p) => [p.name, p.name] as [string, string])];
  for (const [value, label] of choices) {
    const item = button("g-pf-menu-item", label);
    if (value === grouping.portfolio) item.setAttribute("aria-current", "true");
    item.addEventListener("click", () => {
      grouping.portfolio = value;
      handlers.changed();
    });
    menu.append(item);
  }
  const create = button("g-pf-menu-item g-pf-menu-new", "+ New portfolio…");
  create.addEventListener("click", () => {
    menu.hidden = true;
    void promptNewPortfolio(grouping).then((name) => {
      if (!name) return;
      grouping.portfolio = ALL_PORTFOLIOS;
      handlers.changed();
    });
  });
  menu.append(create);
  menu.append(el("div", "g-pf-menu-sep"));
  for (const [value, text] of [["recent", "Sort: latest activity first"], ["manual", "Sort: my order (drag to arrange)"]] as const) {
    const item = button("g-pf-menu-item", `${grouping.sort === value ? "✓ " : "   "}${text}`);
    item.addEventListener("click", () => {
      grouping.sort = value;
      handlers.changed();
    });
    menu.append(item);
  }
  current.addEventListener("click", () => {
    menu.hidden = !menu.hidden;
  });
  const fold = button("g-pf-fold", "«");
  fold.title = "Fold the sidebar into a rail";
  fold.addEventListener("click", () => {
    grouping.rail = true;
    handlers.changed();
  });
  head.append(current, fold, menu);

  // Needs you.
  const inbox = button("g-pf-inbox");
  inbox.append(el("span", "g-pf-inbox-mark", "!"), el("span", "g-pf-inbox-label", "Needs you"));
  const total = badge(view.totalNeeds, "g-count g-count-lg");
  if (total) inbox.append(total);
  else inbox.append(el("span", "g-pf-inbox-none", "nothing"));
  inbox.dataset.empty = String(view.totalNeeds === 0);
  inbox.addEventListener("click", handlers.openInbox);

  // Filter chips.
  const chips = el("div", "g-pf-chips");
  for (const [value, label] of FILTERS) {
    const chip = button("g-pf-chip", label);
    chip.setAttribute("aria-pressed", String(grouping.filter === value));
    chip.addEventListener("click", () => {
      grouping.filter = value;
      handlers.changed();
    });
    chips.append(chip);
  }

  const top = el("div", "g-pf-top");
  top.append(head, inbox, chips);
  section.append(top);

  const list = el("div", "g-pf-list");
  if (grouping.filter !== "all") {
    const matching = shown.flatMap((p) => p.projects.flatMap((q) => q.rooms)).filter((r) => r.type === grouping.filter);
    const banner = el("div", "g-pf-filter");
    banner.append(el("span", "g-pf-icon", ROOM_TYPES[grouping.filter].icon),
      el("span", "g-flex", `${matching.length} ${ROOM_TYPES[grouping.filter].name} room${matching.length === 1 ? "" : "s"}`));
    const clear = button("g-pf-clear", "Clear");
    clear.addEventListener("click", () => {
      grouping.filter = "all";
      handlers.changed();
    });
    banner.append(clear);
    list.append(banner);
    if (matching.length === 0) list.append(el("span", "g-side-hint", "No rooms of this type yet. Set a room's type from its header."));
    for (const room of matching) list.append(roomRow(room, view.activeRoomId, handlers, true));
    section.append(list);
    return section;
  }

  if (tree.portfolios.length === 0 && tree.ungrouped.length === 0) {
    const empty = button("g-side-empty", "No rooms yet. Create one to run agents as a team.");
    empty.addEventListener("click", () => handlers.newRoom(null));
    list.append(empty);
  }
  const collapsed = new Set(grouping.collapsed);
  for (const portfolio of shown) {
    const group = el("div", "g-pf-group");
    const label = button("g-pf-label g-pf-label-btn");
    label.title = `Open the ${portfolio.name} overview`;
    label.addEventListener("click", () => handlers.openPortfolio(portfolio.name));
    label.append(el("span", "g-pf-diamond"), el("span", "g-pf-label-name", portfolio.name));
    label.append(el("span", `g-pf-label-count${portfolio.needs ? " g-needs" : ""}`, portfolio.needs
      ? `${portfolio.needs} need${portfolio.needs === 1 ? "s" : ""} you`
      : `${portfolio.running} running`));
    label.querySelector(".g-pf-diamond")?.setAttribute("data-hue", String(hueSlot(portfolio.name)));
    label.addEventListener("contextmenu", (event) => {
      event.preventDefault();
      showContextMenu(event.clientX, event.clientY, [
        { label: "Open portfolio overview", run: () => handlers.openPortfolio(portfolio.name) },
        { label: "+ Add project…", run: () => handlers.newProject(portfolio.name) },
        { label: "Rename…", run: () => void askText("Rename portfolio", { initial: portfolio.name }).then((name) => {
          if (!name) return;
          renamePortfolio(grouping, portfolio.name, name);
          handlers.changed();
        }) },
        ...(portfolio.projects.length === 0 ? ["-" as const, { label: "Remove empty portfolio", danger: true, run: () => {
          removeEmptyPortfolio(grouping, portfolio.name);
          handlers.changed();
        } }] : []),
      ], portfolio.name);
    });
    projectDropTarget(group, portfolio.name, handlers.changed, grouping);
    // ▾ folds the whole portfolio (supervisor and projects) to one line.
    const foldKey = `portfolio:${portfolio.name}`;
    const folded = grouping.collapsed.includes(foldKey);
    const labelRow = el("div", "g-pf-label-row");
    const fold = button("g-chevron g-pf-fold-pf", folded ? "▸" : "▾");
    fold.title = folded ? `Show ${portfolio.name}` : `Hide everything in ${portfolio.name}`;
    fold.addEventListener("click", () => {
      grouping.collapsed = folded ? grouping.collapsed.filter((k) => k !== foldKey) : [...grouping.collapsed, foldKey];
      handlers.changed();
    });
    labelRow.append(label, fold);
    labelRow.draggable = true;
    labelRow.addEventListener("dragstart", (event) => {
      dragPortfolio = portfolio.name;
      event.dataTransfer?.setData("text/plain", portfolio.name);
      if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
    });
    labelRow.addEventListener("dragend", () => {
      dragPortfolio = null;
    });
    reorderSlot(labelRow, () => dragPortfolio !== null && dragPortfolio !== portfolio.name, () => {
      const moving = dragPortfolio!;
      dragPortfolio = null;
      placeBefore(grouping, shown.map((p) => portfolioKey(p.name)), portfolioKey(moving), portfolioKey(portfolio.name));
      handlers.changed();
    });
    group.append(labelRow);
    if (folded) {
      group.dataset.folded = "true";
      const projectCount = portfolio.projects.length;
      label.querySelector(".g-pf-label-count")!.textContent = portfolio.needs
        ? `${portfolio.needs} need${portfolio.needs === 1 ? "s" : ""} you`
        : `${projectCount} project${projectCount === 1 ? "" : "s"} · ${portfolio.running} running`;
      list.append(group);
      continue;
    }
    for (const lead of portfolio.leads) {
      const row = button("g-pf-lead");
      row.dataset.roomId = lead.id;
      if (view.activeRoomId === lead.id) row.setAttribute("aria-current", "true");
      const supervisor = lead.members.find((m) => m.supervisor) ?? lead.members[0];
      const face = el("span", "g-pf-lead-tile", "◆");
      const dot = el("span", "g-dot");
      dot.dataset.status = supervisor?.status ?? "stopped";
      face.append(dot);
      const text = el("span", "g-pf-project-text");
      const title = el("span", "g-pf-project-name");
      title.append(document.createTextNode("Supervisor "), el("span", "g-pf-lead-who", `· ${supervisor?.label ?? lead.label}`));
      text.append(title, el("span", "g-pf-project-sub", `${projectSummary({ agents: lead.members.length, running: lead.members.filter(running).length,
        working: lead.members.filter((m) => m.status === "busy").length })} · sees all of ${portfolio.name}`));
      row.append(face, text);
      const count = badge(lead.needs);
      if (count) row.append(count);
      row.title = lead.label;
      row.addEventListener("click", () => handlers.openRoom(lead.id));
      row.addEventListener("contextmenu", (event) => {
        event.preventDefault();
        showContextMenu(event.clientX, event.clientY, [
          { label: "Open supervisor room", run: () => handlers.openRoom(lead.id) },
          { label: "Open portfolio overview", run: () => handlers.openPortfolio(portfolio.name) },
        ], lead.label);
      });
      group.append(row);
      if (lead.id === view.activeRoomId && lead.members.length) group.append(memberRows(lead.members, view.activeSessionId, handlers));
    }
    if (portfolio.projects.length === 0) {
      const empty = el("div", "g-pf-empty");
      if (portfolio.leads.length) empty.dataset.lead = "true";
      empty.append(el("span", "", "No projects yet. Drag a project here, or"));
      const add = button("g-pf-clear", "add one");
      add.addEventListener("click", () => handlers.newProject(portfolio.name));
      empty.append(add);
      group.append(empty);
    }
    for (const project of portfolio.projects) {
      const key = `${portfolio.name}/${project.name}`;
      const isCollapsed = collapsed.has(key);
      const block = el("div", "g-pf-project");
      const row = el("div", "g-pf-project-head");
      const open = button("g-pf-project-open");
      const text = el("span", "g-pf-project-text");
      text.append(el("span", "g-pf-project-name", project.name));
      if (project.pinned) open.append(el("span", "g-pin", "📌"));
      if (isCollapsed) text.append(el("span", "g-pf-project-sub", projectSummary(project)));
      open.append(tile(project.name, "sm"), text);
      const needs = badge(project.needs);
      if (needs) open.append(needs);
      open.title = "Open the project overview · right-click to pin";
      open.addEventListener("click", () => handlers.openProject(portfolio.name, project.name));
      row.draggable = true;
      row.title = "Drag onto another portfolio to move the whole project";
      row.addEventListener("dragstart", (event) => {
        dragProject = { portfolio: portfolio.name, project: project.name };
        event.dataTransfer?.setData("text/plain", project.name);
        if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
        row.classList.add("g-dragging");
      });
      row.addEventListener("dragend", () => {
        dragProject = null;
        row.classList.remove("g-dragging");
      });
      row.addEventListener("contextmenu", (event) => {
        event.preventDefault();
        const moveTo: MenuItem[] = portfolioNames(grouping).filter((name) => name !== portfolio.name).map((name) => ({
          label: `Move to ${name}`,
          run: () => {
            moveProject(grouping, portfolio.name, project.name, name);
            handlers.changed();
          },
        }));
        showContextMenu(event.clientX, event.clientY, [
          { label: "Open project overview", run: () => handlers.openProject(portfolio.name, project.name) },
          { label: project.pinned ? "Unpin" : "Pin to top", run: () => pinChanged(projectPin(portfolio.name, project.name)) },
          { label: "New room…", run: () => handlers.newRoom({ portfolio: portfolio.name, project: project.name }) },
          { label: "Rename…", run: () => void askText("Rename project", { initial: project.name }).then((name) => {
            if (!name) return;
            renameProject(grouping, portfolio.name, project.name, name);
            handlers.changed();
          }) },
          "-",
          ...moveTo,
          { label: "Move to a new portfolio…", run: () => void promptNewPortfolio(grouping).then((name) => {
            if (!name) return;
            moveProject(grouping, portfolio.name, project.name, name);
            handlers.changed();
          }) },
          "-",
          { label: isCollapsed ? "Show rooms" : "Hide rooms", run: () => toggle() },
        ], project.name);
      });
      const chevron = button("g-chevron", isCollapsed ? "▸" : "▾");
      chevron.title = isCollapsed ? "Show rooms" : "Hide rooms";
      const toggle = () => {
        grouping.collapsed = isCollapsed ? grouping.collapsed.filter((k) => k !== key) : [...grouping.collapsed, key];
        handlers.changed();
      };
      chevron.addEventListener("click", toggle);
      row.append(open, chevron);
      const projectKeys = portfolio.projects.map((q) => projectPin(portfolio.name, q.name));
      reorderSlot(row, () => dragProject !== null && !(dragProject.portfolio === portfolio.name && dragProject.project === project.name), () => {
        const moving = dragProject!;
        dragProject = null;
        if (moving.portfolio !== portfolio.name) moveProject(grouping, moving.portfolio, moving.project, portfolio.name);
        placeBefore(grouping, projectKeys, projectPin(portfolio.name, moving.project), projectPin(portfolio.name, project.name));
        handlers.changed();
      });
      block.append(row);
      dropTarget(block, (roomId) => {
        placeRoom(grouping, roomId, { portfolio: portfolio.name, project: project.name });
        handlers.changed();
      });
      if (!isCollapsed) {
        const rooms = el("div", "g-pf-rooms");
        const roomKeys = project.rooms.map((r) => roomPin(r.id));
        for (const room of project.rooms) {
          const roomNode = roomRow(room, view.activeRoomId, handlers, false);
          reorderSlot(roomNode, () => dragRoom !== null && dragRoom !== room.id, () => {
            const moving = dragRoom!;
            dragRoom = null;
            if (grouping.rooms[moving]?.portfolio !== portfolio.name || grouping.rooms[moving]?.project !== project.name) {
              placeRoom(grouping, moving, { portfolio: portfolio.name, project: project.name });
            }
            placeBefore(grouping, roomKeys, roomPin(moving), roomPin(room.id));
            handlers.changed();
          });
          rooms.append(roomNode);
          if (room.id === view.activeRoomId && room.members.length) rooms.append(memberRows(room.members, view.activeSessionId, handlers));
        }
        const add = button("g-pf-new-room");
        add.append(el("span", "g-pf-plus", "+"), document.createTextNode("New room"));
        add.addEventListener("click", () => handlers.newRoom({ portfolio: portfolio.name, project: project.name }));
        rooms.append(add);
        block.append(rooms);
      }
      group.append(block);
    }
    list.append(group);
  }
  if (tree.ungrouped.length > 0 && grouping.portfolio === ALL_PORTFOLIOS) {
    const group = el("div", "g-pf-group g-pf-ungrouped");
    const label = el("div", "g-pf-label");
    label.append(el("span", "g-pf-label-name", "Ungrouped"), el("span", "g-pf-label-count", "drag onto a project"));
    group.append(label);
    for (const room of tree.ungrouped) {
      group.append(roomRow(room, view.activeRoomId, handlers, false));
      if (room.id === view.activeRoomId && room.members.length) group.append(memberRows(room.members, view.activeSessionId, handlers));
    }
    dropTarget(group, (roomId) => {
      placeRoom(grouping, roomId, null);
      handlers.changed();
    });
    list.append(group);
  }
  const newProject = button("g-pf-new-project");
  newProject.append(el("span", "g-pf-plus", "+"), document.createTextNode("New room"));
  newProject.title = "Create a room; set its project from the room header";
  newProject.addEventListener("click", () => handlers.newRoom(null));
  list.append(newProject);
  section.append(list);
  return section;
}

/** The folded sidebar: Needs you, then one tile per project with its
    needs badge and a dot while an agent works. Hover shows the name. */
function renderRail(view: PortfolioView, handlers: PortfolioHandlers): HTMLElement {
  const { tree, grouping } = view;
  const rail = el("section", "g-pf g-rail");
  const unfold = button("g-rail-btn g-rail-unfold", "»");
  unfold.title = "Unfold the sidebar";
  unfold.addEventListener("click", () => {
    grouping.rail = false;
    handlers.changed();
  });
  const inbox = button("g-rail-btn g-rail-inbox", "!");
  inbox.title = view.totalNeeds ? `Needs you · ${view.totalNeeds}` : "Needs you · nothing waiting";
  inbox.dataset.empty = String(view.totalNeeds === 0);
  const inboxCount = badge(view.totalNeeds, "g-count g-rail-badge");
  if (inboxCount) inbox.append(inboxCount);
  inbox.addEventListener("click", handlers.openInbox);
  rail.append(unfold, inbox, el("div", "g-rail-sep"));
  const shown = grouping.portfolio === ALL_PORTFOLIOS ? tree.portfolios : tree.portfolios.filter((p) => p.name === grouping.portfolio);
  for (const portfolio of shown) {
    for (const project of portfolio.projects) {
      const item = button("g-rail-btn g-rail-project");
      item.title = `${project.name} · ${projectSummary(project)}${project.needs ? ` · ${project.needs} need you` : ""}`;
      item.append(tile(project.name, "md"));
      const count = badge(project.needs, "g-count g-rail-badge");
      if (count) item.append(count);
      if (project.working > 0) item.append(el("span", "g-rail-working"));
      if (project.rooms.some((r) => r.id === view.activeRoomId)) item.setAttribute("aria-current", "true");
      item.addEventListener("click", () => handlers.openProject(portfolio.name, project.name));
      rail.append(item);
    }
  }
  for (const room of tree.ungrouped) {
    const item = button("g-rail-btn g-rail-project");
    item.title = room.label;
    item.append(tile(room.label, "md"));
    item.addEventListener("click", () => handlers.openRoom(room.id));
    rail.append(item);
  }
  return rail;
}

/* ── Room header: type chip + organise popover ── */

export interface OrganizeOptions {
  roomId: string;
  label: string;
  grouping: Grouping;
  changed(): void;
}

/** The chip in the room header ("</> Development · ReelStudio") and its
    popover: portfolio, project and type. Only relabels the room. */
export function organizeChip(options: OrganizeOptions): HTMLElement {
  const { roomId, grouping } = options;
  const wrap = el("div", "g-org");
  const place = grouping.rooms[roomId];
  const chip = button("g-org-chip");
  chip.title = "Project and room type (only changes how the sidebar is grouped)";
  if (place) {
    chip.append(el("span", "g-pf-icon", ROOM_TYPES[place.type].icon),
      document.createTextNode(`${ROOM_TYPES[place.type].name} · ${place.project}`));
  } else {
    chip.append(el("span", "g-pf-icon", "+"), document.createTextNode("Add to a project"));
  }
  chip.append(el("span", "g-org-caret", "⌄"));
  const pop = el("form", "g-org-pop");
  pop.hidden = true;
  const field = (labelText: string, input: HTMLElement) => {
    const label = el("label", "g-org-field");
    label.append(el("span", "", labelText), input);
    return label;
  };
  const listId = `g-org-${roomId}`;
  const portfolios = portfolioNames(grouping);
  const projects = [...new Set(Object.values(grouping.rooms).map((r) => r.project))].sort();
  const portfolio = el("input", "g-org-input");
  portfolio.value = place?.portfolio ?? "";
  portfolio.placeholder = "e.g. Apps";
  portfolio.setAttribute("list", `${listId}-pf`);
  const project = el("input", "g-org-input");
  project.value = place?.project ?? "";
  project.placeholder = "e.g. ReelStudio";
  project.setAttribute("list", `${listId}-pr`);
  const pfList = el("datalist");
  pfList.id = `${listId}-pf`;
  pfList.append(...portfolios.map((name) => Object.assign(document.createElement("option"), { value: name })));
  const prList = el("datalist");
  prList.id = `${listId}-pr`;
  prList.append(...projects.map((name) => Object.assign(document.createElement("option"), { value: name })));
  project.addEventListener("change", () => {
    const owner = Object.values(grouping.rooms).find((r) => r.project === project.value.trim());
    if (owner && !portfolio.value.trim()) portfolio.value = owner.portfolio;
  });
  const type = el("select", "g-org-input");
  for (const key of TYPE_ORDER) {
    const option = document.createElement("option");
    option.value = key;
    option.textContent = `${ROOM_TYPES[key].icon}  ${key === "lead" ? "Lead room (project supervisor)" : ROOM_TYPES[key].name}`;
    type.append(option);
  }
  type.value = place?.type ?? "dev";
  const actions = el("div", "g-org-actions");
  const save = button("g-btn g-btn-primary", "Save");
  save.type = "submit";
  const remove = button("g-btn g-btn-ghost", "Ungroup");
  remove.hidden = !place;
  actions.append(remove, el("span", "g-flex"), save);
  pop.append(field("Portfolio", portfolio), field("Project", project), field("Room type", type), pfList, prList, actions,
    el("p", "g-org-note", "Only changes how the sidebar groups this room. Agents, folders and the feed stay as they are."));
  pop.addEventListener("submit", (event) => {
    event.preventDefault();
    const projectName = project.value.trim();
    // A Lead room with no project supervises its whole portfolio.
    if (!projectName && type.value !== "lead") {
      project.focus();
      return;
    }
    const owner = Object.values(grouping.rooms).find((r) => r.project === projectName);
    grouping.rooms[roomId] = {
      portfolio: portfolio.value.trim() || owner?.portfolio || "Projects",
      project: projectName,
      type: type.value as RoomType,
    };
    grouping.loose = grouping.loose.filter((id) => id !== roomId);
    pop.hidden = true;
    options.changed();
  });
  remove.addEventListener("click", () => {
    placeRoom(grouping, roomId, null);
    pop.hidden = true;
    options.changed();
  });
  chip.addEventListener("click", () => {
    pop.hidden = !pop.hidden;
    if (!pop.hidden) (place ? type : project).focus();
  });
  wrap.append(chip, pop);
  return wrap;
}
