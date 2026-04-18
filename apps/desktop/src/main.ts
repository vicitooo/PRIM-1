import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";

import {
  resolveCopySelection,
  type CopySurface,
} from "./copy-selection";
import "./styles.css";
import type {
  RouteMessageRequest,
  RuntimeEvent,
  RuntimeSnapshot,
  SendInputRequest,
  SessionSnapshot,
} from "./types";

interface PaneGroup {
  name: string;
  paneNames: string[];
}

const app = document.querySelector("#app");
if (!(app instanceof HTMLDivElement)) {
  throw new Error("Missing #app root element");
}

app.innerHTML = `
  <div class="app-shell">
    <header class="topbar panel">
      <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
      <div class="topbar-brand">
        <p class="eyebrow">Victor / Claude / Codex</p>
        <h1>PRIM-001</h1>
      </div>
      <div class="topbar-active">
        <p class="eyebrow">Active pair</p>
        <span class="active-group-label" id="active-group-label">main</span>
      </div>
      <div class="topbar-status">
        <span class="state-pill" data-session-state="global">ready</span>
        <span class="activity-pill">idle</span>
      </div>
      <button class="theme-toggle" id="theme-toggle" aria-label="Toggle theme" title="Toggle theme"></button>
      <span class="mono" id="control-endpoint" hidden>starting...</span>
      <span class="mono" id="audit-path" hidden>loading...</span>
    </header>

    <section class="workspace-shell">
      <aside class="group-picker panel">
        <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
        <div class="card-head">
          <div>
            <p class="card-kicker">Pair picker</p>
            <h2>Active pair</h2>
          </div>
          <span class="mono" id="group-count">0 groups</span>
        </div>
        <ul class="group-list" id="group-list"></ul>
      </aside>

      <section class="workspace-grid" id="workspace-grid"></section>
    </section>

    <section class="bottom-grid">
      <article class="system-card panel">
        <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
        <div class="card-head">
          <div>
            <p class="card-kicker">Supervisor</p>
            <h2>System log</h2>
          </div>
          <span class="mono" id="runtime-path">runtime pending</span>
        </div>
        <div class="system-terminal" id="system-terminal"></div>
      </article>

      <article class="router-card panel">
        <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
        <div class="card-head">
          <div>
            <p class="card-kicker">Sideband</p>
            <h2>Route a message</h2>
          </div>
          <span class="mono">Visible + logged</span>
        </div>
        <form id="router-form" class="router-form">
          <label>
            <span>From</span>
            <select id="route-from">
              <option value="victor">Victor</option>
              <option value="claude">Claude</option>
              <option value="codex">Codex</option>
            </select>
          </label>
          <label>
            <span>To</span>
            <select id="route-to">
              <option value="claude">Claude</option>
              <option value="codex">Codex</option>
              <option value="room">Room</option>
            </select>
          </label>
          <label class="message-field">
            <span>Content</span>
            <textarea id="route-content" rows="6" placeholder="Tell Claude to review the Codex output, or send a room-wide coordination note."></textarea>
          </label>
          <div class="router-actions">
            <button type="submit" class="primary">Send routed message</button>
            <button type="button" id="clear-router">Clear</button>
          </div>
        </form>
      </article>
    </section>

    <div class="control-flyout">
      <button class="control-toggle" aria-label="Open controls">CTRL</button>
      <div class="control-menu">
        <button data-control="refresh">Refresh snapshot</button>
        <button data-control="mark-main-menu">Main menu</button>
        <button data-control="show-control-file">Show control file</button>
      </div>
    </div>
  </div>
`;

class SessionTerminal {
  readonly name: string;
  title: string;
  readonly terminal: Terminal;
  readonly fitAddon: FitAddon;
  readonly host: HTMLDivElement;
  readonly stateEl: HTMLSpanElement;
  readonly activityEl: HTMLSpanElement;
  private hooked = false;
  private snapshot: SessionSnapshot | null = null;

  constructor(name: string, title: string) {
    this.name = name;
    this.title = title;
    this.host = must<HTMLDivElement>(`[data-terminal="${name}"]`);
    this.stateEl = must<HTMLSpanElement>(`[data-session-state="${name}"]`);
    this.activityEl = must<HTMLSpanElement>(`[data-session-activity="${name}"]`);
    this.terminal = new Terminal({
      convertEol: true,
      cursorBlink: true,
      fontFamily: '"JetBrains Mono", "Cascadia Code", Consolas, monospace',
      fontSize: 13,
      theme: {
        background: "#100f0e",
        foreground: "#dedede",
        cursor: "#D4736A",
        cursorAccent: "#100f0e",
        selectionBackground: "rgba(212, 115, 106, 0.18)",
        black: "#100f0e",
        brightBlack: "#555350",
        red: "#D4736A",
        brightRed: "#E8A598",
        green: "#8cc265",
        brightGreen: "#a5d97a",
        yellow: "#e6b44f",
        brightYellow: "#f2cc72",
        blue: "#7aa2f7",
        brightBlue: "#93b5ff",
        magenta: "#c97eb8",
        brightMagenta: "#dfa0d2",
        cyan: "#59c0c8",
        brightCyan: "#7ee1e7",
        white: "#c8c5c0",
        brightWhite: "#dedede",
      },
    });
    this.fitAddon = new FitAddon();
    this.terminal.loadAddon(this.fitAddon);
    this.terminal.open(this.host);
    this.fitAddon.fit();
    this.host.addEventListener("focusin", () => {
      activeTerminalName = this.name;
      activeCopySurface = this.name;
    });
    this.host.addEventListener("mousedown", () => {
      activeTerminalName = this.name;
      activeCopySurface = this.name;
    });
    this.banner();
  }

  banner(): void {
    this.terminal.reset();
    this.terminal.writeln(`\x1b[38;5;137m${this.title} pane ready.\x1b[0m`);
    this.terminal.writeln("Launch the session from the header or send routed messages below.");
    this.terminal.writeln("");
  }

  hookInput(): void {
    if (this.hooked) {
      return;
    }

    this.hooked = true;
    this.terminal.onData((input) => {
      if (!this.snapshot?.running) {
        return;
      }

      void command<SessionSnapshot>("send_input", {
        request: {
          name: this.name,
          input,
        } satisfies SendInputRequest,
      }).catch((error) => writeSystem("error", `${this.title} input failed: ${error}`));
    });
  }

  applySnapshot(snapshot: SessionSnapshot): void {
    this.title = snapshot.title;
    this.snapshot = snapshot;
    this.stateEl.textContent = snapshot.lifecycle_state;
    this.stateEl.dataset.state = snapshot.lifecycle_state;
    this.activityEl.textContent = snapshot.last_activity_at
      ? new Date(snapshot.last_activity_at).toLocaleTimeString()
      : snapshot.running
        ? "running"
        : "idle";
    this.activityEl.dataset.running = String(snapshot.running);

    if (!snapshot.running && snapshot.lifecycle_state === "closed") {
      this.banner();
    }

    if (isPaneVisible(this.name)) {
      this.fitAddon.fit();
      void resizeSession(this.name, this.terminal.cols, this.terminal.rows);
    }
    this.hookInput();
  }

  write(chunk: string): void {
    this.terminal.write(chunk);
  }

  fit(): void {
    if (!isPaneVisible(this.name)) {
      return;
    }
    this.fitAddon.fit();
    void resizeSession(this.name, this.terminal.cols, this.terminal.rows);
  }

  dispose(): void {
    this.terminal.dispose();
  }
}

const systemTerminal = new Terminal({
  convertEol: true,
  disableStdin: true,
  fontFamily: '"JetBrains Mono", "Cascadia Code", Consolas, monospace',
  fontSize: 12,
  theme: {
    background: "#1a1917",
    foreground: "#dedede",
    cursor: "#E8A598",
    cursorAccent: "#1a1917",
    selectionBackground: "rgba(212, 115, 106, 0.15)",
    black: "#1a1917",
    brightBlack: "#555350",
    red: "#D4736A",
    brightRed: "#E8A598",
    green: "#8cc265",
    brightGreen: "#a5d97a",
    yellow: "#e6b44f",
    brightYellow: "#f2cc72",
    blue: "#88aefc",
    brightBlue: "#9fc0ff",
    magenta: "#c97eb8",
    brightMagenta: "#dfa0d2",
    cyan: "#73d1d6",
    brightCyan: "#8de7ec",
    white: "#c8c5c0",
    brightWhite: "#dedede",
  },
});
const systemFit = new FitAddon();
systemTerminal.loadAddon(systemFit);
const systemTerminalHost = must<HTMLDivElement>("#system-terminal");
systemTerminal.open(systemTerminalHost);
systemFit.fit();
systemTerminalHost.addEventListener("focusin", () => {
  activeCopySurface = "system";
});
systemTerminalHost.addEventListener("mousedown", () => {
  activeCopySurface = "system";
});

const workspaceGrid = must<HTMLDivElement>("#workspace-grid");
const groupList = must<HTMLUListElement>("#group-list");
const groupCount = must<HTMLElement>("#group-count");
const snapshotByName = new Map<string, SessionSnapshot>();
const paneMap = new Map<string, SessionTerminal>();
const controlEndpoint = must<HTMLElement>("#control-endpoint");
const auditPath = must<HTMLElement>("#audit-path");
const runtimePath = must<HTMLElement>("#runtime-path");
const activeGroupLabel = must<HTMLElement>("#active-group-label");
let renderedSessionSignature: string | null = null;
let activeTerminalName: string | null = null;
let activeCopySurface: CopySurface = null;
let currentGroups: PaneGroup[] = [];

/* ── Theme system ── */

const TERMINAL_THEMES = {
  dark: {
    session: {
      background: "#151311",
      foreground: "#b5aea5",
      cursor: "#a0655e",
      cursorAccent: "#151311",
      selectionBackground: "rgba(160, 100, 60, 0.18)",
      black: "#151311", brightBlack: "#5e5850",
      red: "#8a4a42", brightRed: "#a0655e",
      green: "#6a8a50", brightGreen: "#7ea062",
      yellow: "#9a8040", brightYellow: "#b09555",
      blue: "#5a7aaa", brightBlue: "#7090c0",
      magenta: "#7a5a80", brightMagenta: "#907098",
      cyan: "#4a7a7a", brightCyan: "#608e8e",
      white: "#8a8278", brightWhite: "#b5aea5",
    },
    system: {
      background: "#1a1715",
      foreground: "#b5aea5",
      cursor: "#8a6e55",
      cursorAccent: "#1a1715",
      selectionBackground: "rgba(138, 110, 85, 0.15)",
      black: "#1a1715", brightBlack: "#5e5850",
      red: "#8a4a42", brightRed: "#a0655e",
      green: "#6a8a50", brightGreen: "#7ea062",
      yellow: "#9a8040", brightYellow: "#b09555",
      blue: "#5a7aaa", brightBlue: "#7090c0",
      magenta: "#7a5a80", brightMagenta: "#907098",
      cyan: "#4a7a7a", brightCyan: "#608e8e",
      white: "#8a8278", brightWhite: "#b5aea5",
    },
  },
  light: {
    session: {
      background: "#e8e4df",
      foreground: "#2a2520",
      cursor: "#8a5550",
      cursorAccent: "#e8e4df",
      selectionBackground: "rgba(138, 85, 80, 0.18)",
      black: "#2a2520", brightBlack: "#6a6458",
      red: "#7a3a32", brightRed: "#8a4a42",
      green: "#3a6a28", brightGreen: "#4a7a38",
      yellow: "#7a6020", brightYellow: "#8a7030",
      blue: "#3a5a8a", brightBlue: "#4a6a9a",
      magenta: "#6a4a70", brightMagenta: "#7a5a80",
      cyan: "#2a5a5a", brightCyan: "#3a6a6a",
      white: "#b5aea5", brightWhite: "#e8e4df",
    },
    system: {
      background: "#ddd9d4",
      foreground: "#2a2520",
      cursor: "#7a6048",
      cursorAccent: "#ddd9d4",
      selectionBackground: "rgba(122, 96, 72, 0.15)",
      black: "#2a2520", brightBlack: "#6a6458",
      red: "#7a3a32", brightRed: "#8a4a42",
      green: "#3a6a28", brightGreen: "#4a7a38",
      yellow: "#7a6020", brightYellow: "#8a7030",
      blue: "#3a5a8a", brightBlue: "#4a6a9a",
      magenta: "#6a4a70", brightMagenta: "#7a5a80",
      cyan: "#2a5a5a", brightCyan: "#3a6a6a",
      white: "#b5aea5", brightWhite: "#ddd9d4",
    },
  },
} as const;

type ThemeName = keyof typeof TERMINAL_THEMES;

function applyTheme(name: ThemeName): void {
  document.documentElement.dataset.theme = name;
  localStorage.setItem("prim1-theme", name);

  const themes = TERMINAL_THEMES[name];
  for (const pane of paneMap.values()) {
    pane.terminal.options.theme = themes.session;
  }
  systemTerminal.options.theme = themes.system;

  const toggleEl = document.getElementById("theme-toggle");
  if (toggleEl) {
    toggleEl.textContent = name === "dark" ? "\u2600" : "\u263E";
    toggleEl.title = name === "dark" ? "Switch to light" : "Switch to dark";
  }
}

function currentThemeName(): ThemeName {
  return (document.documentElement.dataset.theme || "dark") as ThemeName;
}

function wireThemeToggle(): void {
  const toggleEl = must<HTMLButtonElement>("#theme-toggle");
  toggleEl.addEventListener("click", () => {
    const current = (document.documentElement.dataset.theme || "dark") as ThemeName;
    applyTheme(current === "dark" ? "light" : "dark");
  });
}

const savedTheme = (localStorage.getItem("prim1-theme") || "dark") as ThemeName;
applyTheme(savedTheme);

wireRouter();
wireControls();
wireResize();
wireTerminalShortcuts();
wireThemeToggle();

void listen<RuntimeEvent>("runtime://event", ({ payload }) => {
  handleRuntimeEvent(payload);
});

void bootstrap();

async function bootstrap(): Promise<void> {
  const snapshot = await command<RuntimeSnapshot>("bootstrap");
  applySnapshot(snapshot);
  writeSystem("info", "UI attached to supervisor.");
}

function applySnapshot(snapshot: RuntimeSnapshot): void {
  syncPaneInventory(snapshot.sessions);
  populateRouterOptions(snapshot.sessions);
  controlEndpoint.textContent = snapshot.control_plane?.endpoint ?? "starting...";
  auditPath.textContent = snapshot.audit_log_path;
  runtimePath.textContent = snapshot.runtime_dir;

  for (const session of snapshot.sessions) {
    snapshotByName.set(session.name, session);
    paneMap.get(session.name)?.applySnapshot(session);
  }
}

function handleRuntimeEvent(event: RuntimeEvent): void {
  switch (event.event) {
    case "session_output":
      paneMap.get(event.session)?.write(event.chunk);
      break;
    case "session_state": {
      const previous = snapshotByName.get(event.session);
      if (previous) {
        const next: SessionSnapshot = {
          ...previous,
          lifecycle_state: event.state,
          running: event.state !== "closed" && event.state !== "failed",
          last_activity_at: event.timestamp,
          last_error:
            event.state === "failed" ? event.reason : previous.last_error,
        };
        snapshotByName.set(event.session, next);
        paneMap.get(event.session)?.applySnapshot(next);
      }
      writeSystem("info", `${event.session} -> ${event.state} (${event.reason})`);
      break;
    }
    case "system_log":
      writeSystem(event.level, event.message);
      break;
    case "routed_message":
      writeSystem(
        "info",
        `route ${event.from} -> ${event.to} (${event.scope}): ${event.content}`,
      );
      break;
    case "control_plane_ready":
      controlEndpoint.textContent = event.endpoint;
      writeSystem("info", `control plane ready: ${event.endpoint}`);
      break;
  }
}

function syncPaneInventory(sessions: SessionSnapshot[]): void {
  const signature = sessions
    .map((session) => `${session.name}:${session.title}`)
    .join("|");
  if (signature === renderedSessionSignature) {
    return;
  }

  renderedSessionSignature = signature;
  snapshotByName.clear();
  for (const pane of paneMap.values()) {
    pane.dispose();
  }
  paneMap.clear();
  renderSessionCards(sessions);

  for (const session of sessions) {
    paneMap.set(session.name, new SessionTerminal(session.name, session.title));
  }

  currentGroups = groupSessions(sessions);
  renderGroupPicker(currentGroups);

  if (activeTerminalName && !paneMap.has(activeTerminalName)) {
    activeTerminalName = null;
  }
  if (activeCopySurface && activeCopySurface !== "system" && !paneMap.has(activeCopySurface)) {
    activeCopySurface = null;
  }

  applyTheme(currentThemeName());
  wireButtons();
  wirePicker();
  setActiveGroup(resolveInitialGroup(currentGroups));
}

function renderSessionCards(sessions: SessionSnapshot[]): void {
  const fragment = document.createDocumentFragment();
  for (const session of sessions) {
    fragment.appendChild(buildSessionCard(session));
  }
  workspaceGrid.replaceChildren(fragment);
}

function buildSessionCard(session: SessionSnapshot): HTMLElement {
  const article = document.createElement("article");
  article.className = "terminal-card panel";
  article.dataset.sessionCard = session.name;
  article.dataset.group = groupNameForSession(session.name);
  article.dataset.groupActive = "true";

  article.innerHTML = `
    <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
    <div class="card-head">
      <div>
        <p class="card-kicker">Agent pane</p>
        <h2></h2>
      </div>
      <div class="session-meta">
        <span class="state-pill" data-session-state="${session.name}">closed</span>
        <span class="activity-pill" data-session-activity="${session.name}">idle</span>
      </div>
    </div>
    <div class="terminal-actions">
      <button data-action="start" data-session="${session.name}">Launch</button>
      <button data-action="restart" data-session="${session.name}">Restart</button>
      <button data-action="stop" data-session="${session.name}">Stop</button>
    </div>
    <div class="terminal-host" data-terminal="${session.name}"></div>
  `;

  const title = article.querySelector("h2");
  if (!(title instanceof HTMLHeadingElement)) {
    throw new Error(`Missing title heading for ${session.name}`);
  }
  title.textContent = session.title;

  return article;
}

function groupSessions(sessions: SessionSnapshot[]): PaneGroup[] {
  const groups = new Map<string, string[]>();
  for (const session of sessions) {
    const groupName = groupNameForSession(session.name);
    const paneNames = groups.get(groupName) ?? [];
    paneNames.push(session.name);
    groups.set(groupName, paneNames);
  }

  return Array.from(groups.entries())
    .sort(([left], [right]) => compareGroupNames(left, right))
    .map(([name, paneNames]) => ({
      name,
      paneNames: paneNames.sort(comparePaneNamesWithinGroup),
    }));
}

function groupNameForSession(name: string): string {
  if (name === "claude" || name === "codex") {
    return "main";
  }

  const match = name.match(/^(.+)-(claude|codex)$/);
  return match ? match[1] : "other";
}

function compareGroupNames(left: string, right: string): number {
  if (left === right) {
    return 0;
  }
  if (left === "main") {
    return -1;
  }
  if (right === "main") {
    return 1;
  }
  if (left === "other") {
    return 1;
  }
  if (right === "other") {
    return -1;
  }
  return left.localeCompare(right);
}

function comparePaneNamesWithinGroup(left: string, right: string): number {
  const leftRank = paneRoleRank(left);
  const rightRank = paneRoleRank(right);
  if (leftRank !== rightRank) {
    return leftRank - rightRank;
  }
  return left.localeCompare(right);
}

function paneRoleRank(name: string): number {
  if (name === "claude" || name.endsWith("-claude")) {
    return 0;
  }
  if (name === "codex" || name.endsWith("-codex")) {
    return 1;
  }
  return 2;
}

function renderGroupPicker(groups: PaneGroup[]): void {
  groupCount.textContent = `${groups.length} ${groups.length === 1 ? "group" : "groups"}`;
  const fragment = document.createDocumentFragment();
  for (const group of groups) {
    const item = document.createElement("li");
    const button = document.createElement("button");
    button.className = "group-chip";
    button.dataset.group = group.name;
    button.dataset.active = "false";
    button.textContent = group.name;
    item.appendChild(button);
    fragment.appendChild(item);
  }
  groupList.replaceChildren(fragment);
}

function wirePicker(): void {
  for (const chip of document.querySelectorAll<HTMLButtonElement>(".group-chip")) {
    chip.addEventListener("click", () => {
      const name = chip.dataset.group;
      if (!name) {
        return;
      }
      setActiveGroup(name);
    });
  }
}

function resolveInitialGroup(groups: PaneGroup[]): string {
  const saved = localStorage.getItem("prim1-active-group");
  if (saved && groups.some((group) => group.name === saved)) {
    return saved;
  }
  if (groups.some((group) => group.name === "main")) {
    return "main";
  }
  return groups[0]?.name ?? "main";
}

function setActiveGroup(name: string): void {
  const resolved =
    currentGroups.find((group) => group.name === name)?.name
    ?? resolveInitialGroup(currentGroups);

  localStorage.setItem("prim1-active-group", resolved);
  activeGroupLabel.textContent = resolved;

  for (const group of currentGroups) {
    const isActive = group.name === resolved;
    for (const paneName of group.paneNames) {
      const card = document.querySelector<HTMLElement>(`[data-session-card="${paneName}"]`);
      if (!card) {
        continue;
      }
      card.dataset.groupActive = String(isActive);
    }
  }

  for (const chip of document.querySelectorAll<HTMLButtonElement>(".group-chip")) {
    chip.dataset.active = String(chip.dataset.group === resolved);
  }

  requestAnimationFrame(() => {
    for (const paneName of currentGroups.find((group) => group.name === resolved)?.paneNames ?? []) {
      paneMap.get(paneName)?.fit();
    }
  });
}

function isPaneVisible(name: string): boolean {
  const card = document.querySelector<HTMLElement>(`[data-session-card="${name}"]`);
  return !card || card.dataset.groupActive !== "false";
}

function populateRouterOptions(sessions: SessionSnapshot[]): void {
  const from = must<HTMLSelectElement>("#route-from");
  const to = must<HTMLSelectElement>("#route-to");
  const previousFrom = from.value;
  const previousTo = to.value;
  const sessionNames = sessions.map((session) => session.name);

  from.replaceChildren(optionElement("victor", "Victor"));
  for (const sessionName of sessionNames) {
    from.appendChild(optionElement(sessionName, sessionName));
  }
  from.value = previousFrom === "victor" || sessionNames.includes(previousFrom)
    ? previousFrom
    : "victor";

  to.replaceChildren();
  for (const sessionName of sessionNames) {
    to.appendChild(optionElement(sessionName, sessionName));
  }
  to.appendChild(optionElement("room", "Room"));
  to.value = previousTo === "room" || sessionNames.includes(previousTo)
    ? previousTo
    : "room";
}

function optionElement(value: string, label: string): HTMLOptionElement {
  const option = document.createElement("option");
  option.value = value;
  option.textContent = label;
  return option;
}

function wireButtons(): void {
  for (const button of document.querySelectorAll<HTMLButtonElement>("[data-action]")) {
    button.addEventListener("click", async () => {
      const action = button.dataset.action;
      const session = button.dataset.session;
      if (!action || !session) {
        return;
      }

      try {
        if (action === "start") {
          const snapshot = await command<SessionSnapshot>("start_session", {
            request: { name: session },
          });
          snapshotByName.set(snapshot.name, snapshot);
          paneMap.get(snapshot.name)?.applySnapshot(snapshot);
        }

        if (action === "restart") {
          const snapshot = await command<SessionSnapshot>("restart_session", {
            request: { name: session },
          });
          snapshotByName.set(snapshot.name, snapshot);
          paneMap.get(snapshot.name)?.applySnapshot(snapshot);
        }

        if (action === "stop") {
          const snapshot = await command<SessionSnapshot>("stop_session", {
            request: { name: session },
          });
          snapshotByName.set(snapshot.name, snapshot);
          paneMap.get(snapshot.name)?.applySnapshot(snapshot);
        }
      } catch (error) {
        writeSystem("error", `${action} ${session} failed: ${String(error)}`);
      }
    });
  }
}

function wireRouter(): void {
  const form = must<HTMLFormElement>("#router-form");
  const from = must<HTMLSelectElement>("#route-from");
  const to = must<HTMLSelectElement>("#route-to");
  const content = must<HTMLTextAreaElement>("#route-content");
  const clearButton = must<HTMLButtonElement>("#clear-router");

  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const trimmed = content.value.trim();
    if (!trimmed) {
      return;
    }

    const request: RouteMessageRequest = {
      from: from.value,
      to: to.value,
      scope: to.value === "room" ? "room" : "direct",
      content: trimmed,
    };

    try {
      const snapshot = await command<RuntimeSnapshot>("route_message", { request });
      applySnapshot(snapshot);
      content.value = "";
    } catch (error) {
      writeSystem("error", `route failed: ${String(error)}`);
    }
  });

  clearButton.addEventListener("click", () => {
    content.value = "";
  });
}

function wireControls(): void {
  for (const button of document.querySelectorAll<HTMLButtonElement>("[data-control]")) {
    button.addEventListener("click", async () => {
      switch (button.dataset.control) {
        case "refresh": {
          const snapshot = await command<RuntimeSnapshot>("bootstrap");
          applySnapshot(snapshot);
          writeSystem("info", "snapshot refreshed");
          break;
        }
        case "mark-main-menu":
          setActiveGroup("main");
          writeSystem("info", "active pair switched to main");
          break;
        case "show-control-file":
          writeSystem("info", `control plane info: ${controlEndpoint.textContent}`);
          break;
      }
    });
  }
}

function wireResize(): void {
  const observer = new ResizeObserver(() => {
    fitVisiblePanes();
    systemFit.fit();
  });

  observer.observe(document.body);
  window.addEventListener("resize", () => {
    fitVisiblePanes();
    systemFit.fit();
  });
}

function fitVisiblePanes(): void {
  for (const pane of paneMap.values()) {
    if (isPaneVisible(pane.name)) {
      pane.fit();
    }
  }
}

function wireTerminalShortcuts(): void {
  window.addEventListener(
    "keydown",
    (event) => {
      if (event.key !== "F11") {
        return;
      }

      event.preventDefault();
      void command<void>("toggle_fullscreen").catch((error) =>
        writeSystem("error", `fullscreen toggle failed: ${String(error)}`),
      );
    },
    { capture: true },
  );

  window.addEventListener("keydown", (event) => {
    if (event.altKey && !event.ctrlKey && !event.metaKey) {
      return;
    }

    if (!event.ctrlKey || !event.shiftKey || event.metaKey) {
      return;
    }

    const key = event.key.toLowerCase();
    if (key === "c") {
      const selection = resolveCopySelection({
        domSelection: activeNonTerminalDomSelectionText(),
        activeSurface: activeCopySurface,
        terminalSelections: Object.fromEntries(
          Array.from(paneMap.entries()).map(([name, pane]) => [
            name,
            pane.terminal.getSelection(),
          ]),
        ),
        systemSelection: systemTerminal.getSelection(),
      });
      if (!selection) {
        return;
      }

      event.preventDefault();
      void navigator.clipboard
        .writeText(selection.text)
        .then(() =>
          writeSystem(
            "info",
            selection.kind === "dom"
              ? "DOM selection copied"
              : selection.kind === "system"
                ? "System log selection copied"
                : `${paneLabel(selection.session)} selection copied`,
          ),
        )
        .catch((error) =>
          writeSystem(
            "error",
            selection.kind === "dom"
              ? `copy failed for DOM selection: ${String(error)}`
              : selection.kind === "system"
                ? `copy failed for System log: ${String(error)}`
                : `copy failed for ${paneLabel(selection.session)}: ${String(error)}`,
          ),
        );
      return;
    }

    const activePane = activeTerminal();
    if (!activePane) {
      return;
    }

    if (key === "v") {
      if (!activeTerminalOwnsFocus(activePane)) {
        return;
      }
      event.preventDefault();
      void pasteClipboardIntoTerminal(activePane);
    }
  });
}

async function pasteClipboardIntoTerminal(pane: SessionTerminal): Promise<void> {
  if (!snapshotByName.get(pane.name)?.running) {
    writeSystem("warn", `${pane.title} is not running; paste skipped`);
    return;
  }

  try {
    const clipboard = await navigator.clipboard.readText();
    if (!clipboard) {
      return;
    }

    await command<SessionSnapshot>("send_input", {
      request: {
        name: pane.name,
        input: clipboard,
      } satisfies SendInputRequest,
    });
    writeSystem("info", `${pane.title} pasted ${clipboard.length} chars`);
  } catch (error) {
    writeSystem("error", `paste failed for ${pane.title}: ${String(error)}`);
  }
}

function activeTerminal(): SessionTerminal | null {
  return activeTerminalName ? paneMap.get(activeTerminalName) ?? null : null;
}

function paneLabel(name: string): string {
  return paneMap.get(name)?.title ?? snapshotByName.get(name)?.title ?? name;
}

function activeNonTerminalDomSelectionText(): string | null {
  return activeEditableSelectionText() ?? activeDocumentSelectionText();
}

function activeEditableSelectionText(): string | null {
  const activeElement = document.activeElement;
  if (activeElement instanceof HTMLTextAreaElement) {
    return selectedEditableText(activeElement.value, activeElement.selectionStart, activeElement.selectionEnd);
  }

  if (activeElement instanceof HTMLInputElement) {
    return selectedEditableText(activeElement.value, activeElement.selectionStart, activeElement.selectionEnd);
  }

  return null;
}

function selectedEditableText(
  value: string,
  start: number | null,
  end: number | null,
): string | null {
  if (start == null || end == null || start === end) {
    return null;
  }

  const selected = value.slice(start, end);
  return selected.length > 0 ? selected : null;
}

function activeDocumentSelectionText(): string | null {
  const selection = window.getSelection();
  if (!selection || selection.isCollapsed || selection.rangeCount === 0) {
    return null;
  }

  const range = selection.getRangeAt(0);
  if (nodeInsideTerminalSurface(range.commonAncestorContainer)) {
    return null;
  }

  const selected = selection.toString();
  return selected.length > 0 ? selected : null;
}

function nodeInsideTerminalSurface(node: Node | null): boolean {
  const element = node instanceof Element ? node : node?.parentElement;
  if (!element) {
    return false;
  }

  return Boolean(element.closest("[data-terminal], #system-terminal"));
}

function activeTerminalOwnsFocus(pane: SessionTerminal): boolean {
  const activeElement = document.activeElement;
  return activeElement instanceof Node && pane.host.contains(activeElement);
}

function writeSystem(level: "info" | "warn" | "error", message: string): void {
  const color =
    level === "error"
      ? "\x1b[38;5;203m"
      : level === "warn"
        ? "\x1b[38;5;215m"
        : "\x1b[38;5;110m";
  systemTerminal.writeln(
    `${color}[${new Date().toLocaleTimeString()}] ${message}\x1b[0m`,
  );
}

async function resizeSession(name: string, cols: number, rows: number): Promise<void> {
  if (!snapshotByName.get(name)?.running) {
    return;
  }

  try {
    await command<void>("resize_session", { name, cols, rows });
  } catch {
    // Resize is best-effort for the MVP.
  }
}

async function command<T>(
  name: string,
  payload?: Record<string, unknown>,
): Promise<T> {
  return invoke<T>(name, payload);
}

function must<T extends Element>(selector: string): T {
  const element = document.querySelector(selector);
  if (!element) {
    throw new Error(`Missing required element: ${selector}`);
  }
  return element as T;
}
