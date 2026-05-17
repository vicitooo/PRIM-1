import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";

import {
  resolveCopySelection,
  type CopySurface,
} from "./copy-selection";
import {
  handleRuntimeEvent,
  type PendingBuffer,
  type RuntimeEventContext,
} from "./runtime-events";
import "./styles.css";
import type {
  CreatePairRequest,
  DeletePairRequest,
  RenamePairRequest,
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

type PairFocusTarget = "create" | "delete" | `rename:${string}` | null;

const RESERVED_PAIR_NAMES = new Set(["main", "claude", "codex", "room", "operator"]);
const PAIR_NAME_PATTERN = /^[a-zA-Z0-9_-]+$/;
const PAIR_NAME_MAX_LEN = 48;

function paneDisplayTitle(_name: string, original: string): string {
  return original;
}

const app = document.querySelector("#app");
if (!(app instanceof HTMLDivElement)) {
  throw new Error("Missing #app root element");
}

app.innerHTML = `
  <div class="app-shell">
    <div class="prim1-ticker" aria-hidden="true">
      <div class="prim1-ticker-track">
        <span class="prim1-ticker-cell">INTEGRITY <span class="prim1-ticker-glyph">&#9635;</span> 98.6%</span>
        <span class="prim1-ticker-sep">&#9670;</span>
        <span class="prim1-ticker-cell">PRIM-1 <span class="prim1-ticker-glyph">&#8756;</span> STABLE</span>
        <span class="prim1-ticker-sep">&#9670;</span>
        <span class="prim1-ticker-cell">CHANNELS: CLAUDE // CODEX</span>
        <span class="prim1-ticker-sep">&#9670;</span>
        <span class="prim1-ticker-cell">LATENCY: 12.4ms</span>
        <span class="prim1-ticker-sep">&#9670;</span>
        <span class="prim1-ticker-cell">BREACH ATTEMPTS: 0</span>
        <span class="prim1-ticker-sep">&#9670;</span>
        <span class="prim1-ticker-cell">UNHANDLED EVENTS: 0</span>
        <span class="prim1-ticker-sep">&#9670;</span>
        <span class="prim1-ticker-cell">FW VERSION: 2.4.0</span>
        <span class="prim1-ticker-sep">&#9670;</span>
        <span class="prim1-ticker-cell">NET STATUS: GREEN</span>
        <span class="prim1-ticker-sep">&#9670;</span>
        <span class="prim1-ticker-cell">ACCESS CHECKS: PASS</span>
        <span class="prim1-ticker-sep">&#9670;</span>
        <span class="prim1-ticker-cell">RECOVERY: STANDBY</span>
        <span class="prim1-ticker-sep">&#9670;</span>
        <span class="prim1-ticker-cell">PRIM-1 <span class="prim1-ticker-glyph">&#8756;</span> STABLE</span>
        <span class="prim1-ticker-sep">&#9670;</span>
        <span class="prim1-ticker-cell">UPLINK: ENCRYPTED</span>
        <span class="prim1-ticker-sep">&#9670;</span>
      </div>
    </div>
    <header class="topbar panel">
      <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
      <span class="hud-antenna" aria-hidden="true"></span>
      <svg class="topbar-hud-strip" viewBox="0 0 2400 30" preserveAspectRatio="none" aria-hidden="true">
        <path d="M 600 6 L 920 6 M 960 6 L 1280 6 M 1340 6 L 1620 6 M 1680 6 L 2000 6 M 2060 6 L 2320 6"
              style="stroke: var(--bronze)" stroke-width="1.5" fill="none" stroke-linecap="square" />
        <path d="M 940 0 L 940 14 M 1310 0 L 1310 14 M 1650 0 L 1650 14 M 2030 0 L 2030 14"
              style="stroke: var(--bronze)" stroke-width="1.4" fill="none" stroke-linecap="square" />
        <rect x="930" y="2" width="14" height="6" style="fill: var(--bronze)" opacity="0.55" />
        <rect x="1300" y="2" width="14" height="6" style="fill: var(--bronze)" opacity="0.55" />
        <rect x="1640" y="2" width="14" height="6" style="fill: var(--bronze)" opacity="0.55" />
        <rect x="2020" y="2" width="14" height="6" style="fill: var(--bronze)" opacity="0.55" />
        <path d="M 1000 16 L 1260 16 M 1700 16 L 1980 16"
              style="stroke: var(--bronze)" stroke-width="1" fill="none" opacity="0.5" stroke-linecap="square" />
        <path d="M 600 22 L 700 22 M 1380 22 L 1580 22"
              style="stroke: var(--copper-hot)" stroke-width="1" fill="none" opacity="0.55" stroke-linecap="square" />
        <rect x="592" y="20" width="4" height="4" style="fill: var(--copper-hot)" opacity="0.7" />
        <rect x="1372" y="20" width="4" height="4" style="fill: var(--copper-hot)" opacity="0.7" />
      </svg>
      <div class="topbar-brand">
        <div class="brand-logo-frame" aria-hidden="true">
          <span class="brand-logo" aria-hidden="true">P1</span>
          <svg class="brand-logo-bracket" viewBox="0 0 60 60" aria-hidden="true">
            <path d="M 0 14 L 0 0 L 14 0" style="stroke: var(--bronze)" stroke-width="2" fill="none" stroke-linecap="square" />
            <path d="M 46 0 L 60 0 L 60 14" style="stroke: var(--bronze)" stroke-width="2" fill="none" stroke-linecap="square" />
            <path d="M 60 46 L 60 60 L 46 60" style="stroke: var(--bronze)" stroke-width="2" fill="none" stroke-linecap="square" />
            <path d="M 14 60 L 0 60 L 0 46" style="stroke: var(--bronze)" stroke-width="2" fill="none" stroke-linecap="square" />
            <path d="M 0 22 L 0 38" style="stroke: var(--copper-hot)" stroke-width="1.5" fill="none" opacity="0.7" />
            <path d="M 60 22 L 60 38" style="stroke: var(--copper-hot)" stroke-width="1.5" fill="none" opacity="0.7" />
          </svg>
        </div>
        <h1>PRIM-1 0.4</h1>
        <span class="topbar-active-tag">Active pair</span>
      </div>
      <div class="topbar-status">
        <span class="state-pill" data-session-state="global" hidden>ready</span>
        <span class="activity-pill" hidden>idle</span>
      </div>
      <button class="theme-toggle" id="theme-toggle" aria-label="Toggle theme" title="Toggle theme"></button>
      <span class="mono" id="control-endpoint" hidden>starting...</span>
      <span class="mono" id="audit-path" hidden>loading...</span>
    </header>

    <section class="workspace-shell">
      <aside class="group-picker panel">
        <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
        <span class="sidebar-accent-stripe" aria-hidden="true"></span>
        <svg class="sidebar-inner-frame" viewBox="0 0 200 600" preserveAspectRatio="none" aria-hidden="true">
          <path d="M 14 0 L 14 24 L 4 34 L 4 540 L 14 550 L 14 564 L 28 564 L 38 554 L 162 554 L 172 564 L 186 564 L 186 550 L 196 540 L 196 34 L 186 24 L 186 0"
                style="stroke: var(--copper-hot)" stroke-width="2.2" fill="none" vector-effect="non-scaling-stroke" stroke-linejoin="miter" stroke-linecap="square"/>
          <path d="M 4 540 L 14 550 L 14 564 L 28 564 L 38 554 L 162 554 L 172 564 L 186 564 L 186 550 L 196 540"
                style="stroke: var(--bronze)" stroke-width="2" fill="none" vector-effect="non-scaling-stroke" stroke-linejoin="miter" stroke-linecap="square"/>
          <rect x="2" y="544" width="14" height="10" style="fill: var(--copper-hot); fill-opacity: 0.22" stroke="none" />
          <rect x="184" y="544" width="14" height="10" style="fill: var(--copper-hot); fill-opacity: 0.22" stroke="none" />
        </svg>
        <div class="card-head">
          <div class="card-title-block">
            <div class="kicker-row">
              <p class="card-kicker">Link</p>
              <span class="online-pill"><span class="online-dot"></span>Online</span>
            </div>
            <h2>Active pair</h2>
          </div>
          <div class="group-picker-tools">
            <span class="mono" id="group-count" hidden>0 groups</span>
            <div id="pair-create-slot"></div>
          </div>
        </div>
        <ul class="group-list" id="group-list"></ul>
        <div class="sidebar-grid" aria-hidden="true"></div>
        <div class="sidebar-footer" aria-hidden="true">
          <span class="sidebar-version">iSYS v.2.0</span>
          <div class="sidebar-progress"><span></span></div>
        </div>
        <div id="pair-dialog-slot"></div>
        <span class="active-group-label" id="active-group-label" hidden>main</span>
      </aside>

      <section class="workspace-grid" id="workspace-grid"></section>
    </section>

    <section class="bottom-grid">
      <article class="system-card panel">
        <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
        <span class="hud-antenna" aria-hidden="true"></span>
        <div class="card-head">
          <div class="card-title">
            <span class="card-icon icon-log" aria-hidden="true"></span>
            <h2>System log</h2>
          </div>
          <svg class="card-head-hud" viewBox="0 0 800 12" preserveAspectRatio="none" aria-hidden="true">
            <path d="M 0 6 L 180 6 M 220 6 L 420 6 M 460 6 L 700 6 M 740 6 L 800 6"
                  style="stroke: var(--bronze)" stroke-width="1.2" fill="none" stroke-linecap="square" />
            <path d="M 200 0 L 200 12 M 440 0 L 440 12 M 720 0 L 720 12"
                  style="stroke: var(--bronze)" stroke-width="1" fill="none" stroke-linecap="square" />
            <rect x="195" y="3" width="10" height="6" style="fill: var(--bronze)" opacity="0.55" />
            <rect x="435" y="3" width="10" height="6" style="fill: var(--copper-hot)" opacity="0.75" />
            <rect x="715" y="3" width="10" height="6" style="fill: var(--bronze)" opacity="0.55" />
          </svg>
          <div class="card-head-meta">
            <span class="live-pill" hidden><span class="live-dot"></span>Live</span>
            <span class="mono" id="runtime-path" hidden>runtime pending</span>
            <span class="log-toggle" aria-hidden="true">&lt; Log &gt;</span>
          </div>
        </div>
        <div class="system-terminal" id="system-terminal"></div>
        <span class="card-ctrl-chip" aria-hidden="true">CTRL</span>
      </article>

      <article class="router-card panel">
        <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
        <span class="hud-antenna" aria-hidden="true"></span>
        <div class="card-head">
          <div class="card-title">
            <span class="card-icon icon-send" aria-hidden="true"></span>
            <h2>Route a message</h2>
          </div>
          <svg class="card-head-hud" viewBox="0 0 800 12" preserveAspectRatio="none" aria-hidden="true">
            <path d="M 0 6 L 220 6 M 260 6 L 460 6 M 500 6 L 800 6"
                  style="stroke: var(--bronze)" stroke-width="1.2" fill="none" stroke-linecap="square" />
            <path d="M 240 0 L 240 12 M 480 0 L 480 12"
                  style="stroke: var(--bronze)" stroke-width="1" fill="none" stroke-linecap="square" />
            <rect x="235" y="3" width="10" height="6" style="fill: var(--copper-hot)" opacity="0.75" />
            <rect x="475" y="3" width="10" height="6" style="fill: var(--bronze)" opacity="0.55" />
          </svg>
          <span class="mono" hidden>Visible + logged</span>
        </div>
        <form id="router-form" class="router-form">
          <label>
            <span>From</span>
            <div class="combobox-mount" data-combobox="route-from"></div>
          </label>
          <label>
            <span>To</span>
            <div class="combobox-mount" data-combobox="route-to"></div>
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
        <span class="card-ctrl-chip" aria-hidden="true">CTRL</span>
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

interface ComboboxOption { value: string; label: string; }

class Combobox {
  readonly el: HTMLDivElement;
  private readonly trigger: HTMLButtonElement;
  private readonly triggerLabel: HTMLSpanElement;
  private readonly listbox: HTMLDivElement;
  private options: ComboboxOption[] = [];
  private _value = "";
  private isOpen = false;

  constructor(mount: HTMLElement, public readonly id: string) {
    this.el = document.createElement("div");
    this.el.className = "combobox";
    this.el.dataset.comboboxId = id;

    this.trigger = document.createElement("button");
    this.trigger.type = "button";
    this.trigger.className = "combobox-trigger";
    this.trigger.setAttribute("aria-haspopup", "listbox");
    this.trigger.setAttribute("aria-expanded", "false");

    this.triggerLabel = document.createElement("span");
    this.triggerLabel.className = "combobox-label";
    this.trigger.appendChild(this.triggerLabel);

    const chevron = document.createElement("span");
    chevron.className = "combobox-chevron";
    chevron.setAttribute("aria-hidden", "true");
    this.trigger.appendChild(chevron);

    this.listbox = document.createElement("div");
    this.listbox.className = "combobox-listbox";
    this.listbox.setAttribute("role", "listbox");
    this.listbox.hidden = true;

    this.el.append(this.trigger, this.listbox);
    mount.replaceChildren(this.el);

    this.trigger.addEventListener("click", () => this.toggle());
    this.trigger.addEventListener("keydown", (event) => this.handleKeydown(event));
    this.listbox.addEventListener("keydown", (event) => this.handleKeydown(event));
    document.addEventListener("click", (event) => {
      if (!this.isOpen) return;
      if (event.target instanceof Node && !this.el.contains(event.target)) {
        this.close();
      }
    });
  }

  get value(): string { return this._value; }
  set value(v: string) {
    if (this.options.some((opt) => opt.value === v)) {
      this._value = v;
      this.updateLabel();
      this.renderListbox();
    }
  }

  setOptions(options: ComboboxOption[]): void {
    this.options = options.slice();
    if (!this.options.find((opt) => opt.value === this._value)) {
      this._value = this.options[0]?.value ?? "";
    }
    this.updateLabel();
    this.renderListbox();
  }

  private updateLabel(): void {
    const opt = this.options.find((o) => o.value === this._value);
    this.triggerLabel.textContent = opt?.label ?? "";
  }

  private renderListbox(): void {
    this.listbox.replaceChildren();
    for (const opt of this.options) {
      const item = document.createElement("button");
      item.type = "button";
      item.className = "combobox-option";
      item.setAttribute("role", "option");
      item.dataset.value = opt.value;
      item.textContent = opt.label;
      if (opt.value === this._value) {
        item.dataset.selected = "true";
        item.setAttribute("aria-selected", "true");
      }
      item.addEventListener("click", () => {
        this.value = opt.value;
        this.el.dispatchEvent(new Event("change", { bubbles: true }));
        this.close();
        this.trigger.focus();
      });
      this.listbox.appendChild(item);
    }
  }

  toggle(): void { this.isOpen ? this.close() : this.open(); }

  open(): void {
    if (this.isOpen) return;
    this.isOpen = true;
    this.listbox.hidden = false;
    this.trigger.setAttribute("aria-expanded", "true");
    const selected = this.listbox.querySelector<HTMLButtonElement>('[data-selected="true"]');
    (selected ?? this.listbox.querySelector<HTMLButtonElement>(".combobox-option"))?.focus();
  }

  close(): void {
    if (!this.isOpen) return;
    this.isOpen = false;
    this.listbox.hidden = true;
    this.trigger.setAttribute("aria-expanded", "false");
  }

  private handleKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape" && this.isOpen) {
      event.preventDefault();
      this.close();
      this.trigger.focus();
      return;
    }
    if ((event.key === "ArrowDown" || event.key === "Enter" || event.key === " ") && !this.isOpen && event.target === this.trigger) {
      event.preventDefault();
      this.open();
      return;
    }
    if (this.isOpen && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
      event.preventDefault();
      const items = Array.from(this.listbox.querySelectorAll<HTMLButtonElement>(".combobox-option"));
      const current = document.activeElement;
      const idx = current instanceof HTMLButtonElement ? items.indexOf(current) : -1;
      const next = event.key === "ArrowDown"
        ? items[(idx + 1) % items.length]
        : items[(idx - 1 + items.length) % items.length];
      next?.focus();
    }
  }
}

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
    this.title = paneDisplayTitle(name, title);
    this.host = must<HTMLDivElement>(`[data-terminal="${name}"]`);
    this.stateEl = must<HTMLSpanElement>(`[data-session-state="${name}"]`);
    this.activityEl = must<HTMLSpanElement>(`[data-session-activity="${name}"]`);
    this.terminal = new Terminal({
      convertEol: true,
      cursorBlink: true,
      fontFamily: '"JetBrains Mono", "Cascadia Code", Consolas, monospace',
      fontSize: 13,
      theme: {
        background: "#03060a",
        foreground: "#ff2d8c",
        cursor: "#5dbabe",
        cursorAccent: "#03060a",
        selectionBackground: "rgba(93, 186, 190, 0.22)",
        black: "#03060a",
        brightBlack: "#9a2858",
        red: "#ff2d6f",
        brightRed: "#ff5a8d",
        green: "#00ff99",
        brightGreen: "#5affb8",
        yellow: "#d8e02a",
        brightYellow: "#ecf055",
        blue: "#00aaff",
        brightBlue: "#5acdff",
        magenta: "#ff2dd6",
        brightMagenta: "#ff70e8",
        cyan: "#5dbabe",
        brightCyan: "#7dd0d4",
        white: "#b8e8ff",
        brightWhite: "#e0f7ff",
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
    this.terminal.writeln(`\x1b[38;2;${brandBannerAnsi()}m${this.title} pane ready.\x1b[0m`);
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
    this.title = paneDisplayTitle(snapshot.name, snapshot.title);
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
    background: "#03060a",
    foreground: "#e0f7ff",
    cursor: "#d8e02a",
    cursorAccent: "#03060a",
    selectionBackground: "rgba(216, 224, 42, 0.22)",
    black: "#03060a",
    brightBlack: "#2a3a55",
    red: "#ff2d6f",
    brightRed: "#ff5a8d",
    green: "#00ff99",
    brightGreen: "#5affb8",
    yellow: "#d8e02a",
    brightYellow: "#ecf055",
    blue: "#00aaff",
    brightBlue: "#5acdff",
    magenta: "#ff2dd6",
    brightMagenta: "#ff70e8",
    cyan: "#5dbabe",
    brightCyan: "#7dd0d4",
    white: "#b8e8ff",
    brightWhite: "#e0f7ff",
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
const pairCreateSlot = must<HTMLDivElement>("#pair-create-slot");
const pairDialogSlot = must<HTMLDivElement>("#pair-dialog-slot");
const snapshotByName = new Map<string, SessionSnapshot>();

const paneMap = new Map<string, SessionTerminal>();
const pendingOutput = new Map<string, PendingBuffer>();
const controlEndpoint = must<HTMLElement>("#control-endpoint");
const auditPath = must<HTMLElement>("#audit-path");
const runtimePath = must<HTMLElement>("#runtime-path");
const activeGroupLabel = must<HTMLElement>("#active-group-label");

const routeFromCombobox = new Combobox(
  must<HTMLDivElement>('[data-combobox="route-from"]'),
  "route-from",
);
const routeToCombobox = new Combobox(
  must<HTMLDivElement>('[data-combobox="route-to"]'),
  "route-to",
);
routeFromCombobox.setOptions([
  { value: "operator", label: "Operator" },
  { value: "claude", label: paneDisplayTitle("claude", "Claude") },
  { value: "codex", label: paneDisplayTitle("codex", "Codex") },
]);
routeToCombobox.setOptions([
  { value: "claude", label: paneDisplayTitle("claude", "Claude") },
  { value: "codex", label: paneDisplayTitle("codex", "Codex") },
  { value: "room", label: "Room" },
]);
let renderedSessionSignature: string | null = null;
let activeTerminalName: string | null = null;
let activeCopySurface: CopySurface = null;
let currentGroups: PaneGroup[] = [];
let createPairMode = false;
let createPairDraft = "";
let createPairError: string | null = null;
let renamePairTarget: string | null = null;
let renamePairDraft = "";
let renamePairError: string | null = null;
let openPairMenu: string | null = null;
let deletePairTarget: string | null = null;
let pairCrudPending = false;
let pendingPairFocus: PairFocusTarget = null;
let pairPickerDismissWired = false;
let pairPickerActionsWired = false;
let paneButtonsWired = false;

/* ── Theme system ──
   Themes are sets of CSS custom properties (defined in styles.css)
   plus matching xterm.js color objects (defined here). To add a theme:
   1) Add a [data-theme="<name>"] block in styles.css with palette overrides
   2) Add a session+system entry in TERMINAL_THEMES below
   3) Add the name to THEME_CYCLE if you want it in the picker rotation */

const TERMINAL_THEMES = {
  prim1: {
    session: {
      background: "#03060a",
      foreground: "#ff2d8c",
      cursor: "#5dbabe",
      cursorAccent: "#03060a",
      selectionBackground: "rgba(93, 186, 190, 0.22)",
      black: "#03060a", brightBlack: "#9a2858",
      red: "#ff2d6f", brightRed: "#ff5a8d",
      green: "#00ff99", brightGreen: "#5affb8",
      yellow: "#d8e02a", brightYellow: "#ecf055",
      blue: "#00aaff", brightBlue: "#5acdff",
      magenta: "#ff2dd6", brightMagenta: "#ff70e8",
      cyan: "#5dbabe", brightCyan: "#7dd0d4",
      white: "#b8e8ff", brightWhite: "#e0f7ff",
    },
    system: {
      background: "#03060a",
      foreground: "#e0f7ff",
      cursor: "#d8e02a",
      cursorAccent: "#03060a",
      selectionBackground: "rgba(216, 224, 42, 0.22)",
      black: "#03060a", brightBlack: "#2a3a55",
      red: "#ff2d6f", brightRed: "#ff5a8d",
      green: "#00ff99", brightGreen: "#5affb8",
      yellow: "#d8e02a", brightYellow: "#ecf055",
      blue: "#00aaff", brightBlue: "#5acdff",
      magenta: "#ff2dd6", brightMagenta: "#ff70e8",
      cyan: "#5dbabe", brightCyan: "#7dd0d4",
      white: "#b8e8ff", brightWhite: "#e0f7ff",
    },
  },
  "prim1-deep": {
    session: {
      background: "#000000",
      foreground: "#ff5cb8",
      cursor: "#ff70c0",
      cursorAccent: "#000000",
      selectionBackground: "rgba(255, 112, 192, 0.28)",
      black: "#000000", brightBlack: "#6a3850",
      red: "#e0244a", brightRed: "#ff5a78",
      green: "#ff70c0", brightGreen: "#ff9ad8",
      yellow: "#d8389a", brightYellow: "#ff5cb8",
      blue: "#c8388a", brightBlue: "#e060a8",
      magenta: "#ff2d8c", brightMagenta: "#ff5cb8",
      cyan: "#ff70c0", brightCyan: "#ff9ad8",
      white: "#f0d4e8", brightWhite: "#ffeaf5",
    },
    system: {
      background: "#000000",
      foreground: "#f0d4e8",
      cursor: "#d8389a",
      cursorAccent: "#000000",
      selectionBackground: "rgba(216, 56, 154, 0.22)",
      black: "#000000", brightBlack: "#3a2238",
      red: "#e0244a", brightRed: "#ff5a78",
      green: "#ff70c0", brightGreen: "#ff9ad8",
      yellow: "#d8389a", brightYellow: "#ff5cb8",
      blue: "#c8388a", brightBlue: "#e060a8",
      magenta: "#ff2d8c", brightMagenta: "#ff5cb8",
      cyan: "#ff70c0", brightCyan: "#ff9ad8",
      white: "#f0d4e8", brightWhite: "#ffeaf5",
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

/** Themes the picker cycles through. `light` is intentionally excluded \u2014 kept
    in TERMINAL_THEMES + styles.css for future repair but not user-exposed. */
const THEME_CYCLE: ThemeName[] = ["prim1", "prim1-deep"];

const THEME_LABELS: Record<ThemeName, { glyph: string; label: string }> = {
  prim1:        { glyph: "\u25C7", label: "PRIM-1" },       // \u25C7 open diamond
  "prim1-deep": { glyph: "\u25C6", label: "PRIM-1 DEEP" },  // \u25C6 filled diamond
  light:            { glyph: "\u25C8", label: "LIGHT" },
};

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
    const meta = THEME_LABELS[name];
    toggleEl.textContent = meta.glyph;
    const nextIdx = (THEME_CYCLE.indexOf(name) + 1) % THEME_CYCLE.length;
    const next = THEME_CYCLE[nextIdx] ?? THEME_CYCLE[0];
    toggleEl.title = `${meta.label} \u2014 click for ${THEME_LABELS[next].label}`;
    toggleEl.dataset.activeTheme = name;
  }
}

function currentThemeName(): ThemeName {
  const stored = document.documentElement.dataset.theme;
  if (stored && stored in TERMINAL_THEMES) return stored as ThemeName;
  return "prim1";
}

function wireThemeToggle(): void {
  const toggleEl = must<HTMLButtonElement>("#theme-toggle");
  toggleEl.addEventListener("click", () => {
    const current = currentThemeName();
    const cycleIdx = THEME_CYCLE.indexOf(current);
    // If currently on a non-cycled theme (e.g. legacy light), jump to first cycled theme
    const nextIdx = cycleIdx === -1 ? 0 : (cycleIdx + 1) % THEME_CYCLE.length;
    applyTheme(THEME_CYCLE[nextIdx]);
  });
}

/** Read the active theme's brand color from CSS and return as ANSI truecolor
    "R;G;B" so banner text stays in sync with the current theme. */
function brandBannerAnsi(): string {
  const raw = getComputedStyle(document.documentElement)
    .getPropertyValue("--prim1")
    .trim();
  const m = raw.match(/^#?([a-f\d]{2})([a-f\d]{2})([a-f\d]{2})$/i);
  if (!m) return "255;45;140";
  return `${parseInt(m[1], 16)};${parseInt(m[2], 16)};${parseInt(m[3], 16)}`;
}

/** Active theme's secondary color as ANSI truecolor — used for system log info
    entries so they shift with the theme (cyan in default, pink in deep). */
function secondaryAnsiRgb(): string {
  const raw = getComputedStyle(document.documentElement)
    .getPropertyValue("--copper-hot")
    .trim();
  const m = raw.match(/^#?([a-f\d]{2})([a-f\d]{2})([a-f\d]{2})$/i);
  if (!m) return "93;186;190";
  return `${parseInt(m[1], 16)};${parseInt(m[2], 16)};${parseInt(m[3], 16)}`;
}

/** Load saved theme from localStorage, migrating legacy "dark" \u2192 "prim1" */
function loadSavedTheme(): ThemeName {
  const raw = localStorage.getItem("prim1-theme");
  if (raw === "dark" || raw === null) return "prim1";
  if (raw in TERMINAL_THEMES) return raw as ThemeName;
  return "prim1";
}

applyTheme(loadSavedTheme());

wireRouter();
wireControls();
wireResize();
wireTerminalShortcuts();
wireThemeToggle();

const runtimeEventContext: RuntimeEventContext = {
  writeSystem,
  refreshSnapshotFromEvent,
  snapshotByName,
  pendingOutput,
  writeToPane(session, chunk) {
    const pane = paneMap.get(session);
    if (pane) {
      pane.write(chunk);
      return true;
    }
    return false;
  },
  applyPaneSnapshot(name, snapshot) {
    paneMap.get(name)?.applySnapshot(snapshot);
  },
  setControlEndpoint(endpoint) {
    controlEndpoint.textContent = endpoint;
  },
};

void listen<RuntimeEvent>("runtime://event", ({ payload }) => {
  handleRuntimeEvent(payload, runtimeEventContext);
});

void bootstrap();

async function bootstrap(): Promise<void> {
  await refreshSnapshot();
  writeSystem("info", "UI attached to supervisor.");
}

async function refreshSnapshot(preferredActiveGroup?: string): Promise<RuntimeSnapshot> {
  const snapshot = await command<RuntimeSnapshot>("bootstrap");
  applySnapshot(snapshot, preferredActiveGroup);
  return snapshot;
}

function applySnapshot(snapshot: RuntimeSnapshot, preferredActiveGroup?: string): void {
  syncPaneInventory(snapshot.sessions, preferredActiveGroup);
  populateRouterOptions(snapshot.sessions);
  controlEndpoint.textContent = snapshot.control_plane?.endpoint ?? "starting...";
  auditPath.textContent = snapshot.audit_log_path;
  runtimePath.textContent = snapshot.runtime_dir;

  for (const session of snapshot.sessions) {
    snapshotByName.set(session.name, session);
    paneMap.get(session.name)?.applySnapshot(session);
  }
}

function currentActiveGroupName(): string {
  return activeGroupLabel.textContent?.trim() || resolveInitialGroup(currentGroups);
}

function resolveActiveGroupPreference(
  groups: PaneGroup[],
  preferredGroup?: string,
): string {
  if (preferredGroup && groups.some((group) => group.name === preferredGroup)) {
    return preferredGroup;
  }

  const current = activeGroupLabel.textContent?.trim();
  if (current && groups.some((group) => group.name === current)) {
    return current;
  }

  return resolveInitialGroup(groups);
}

function canManagePairGroup(name: string): boolean {
  return name !== "main" && name !== "other";
}

function isPairRunning(name: string): boolean {
  const group = currentGroups.find((candidate) => candidate.name === name);
  return group?.paneNames.some((paneName) => snapshotByName.get(paneName)?.running) ?? false;
}

function validatePairName(name: string, ignoreName?: string): string | null {
  const trimmed = name.trim();
  if (!trimmed) {
    return "Pair name cannot be empty.";
  }
  if (trimmed.length > PAIR_NAME_MAX_LEN) {
    return `Pair name cannot exceed ${PAIR_NAME_MAX_LEN} characters.`;
  }
  if (RESERVED_PAIR_NAMES.has(trimmed)) {
    return `Pair name "${trimmed}" is reserved.`;
  }
  if (!PAIR_NAME_PATTERN.test(trimmed)) {
    return "Use only letters, numbers, hyphens, and underscores.";
  }
  if (currentGroups.some((group) => group.name === trimmed && group.name !== ignoreName)) {
    return `Pair "${trimmed}" already exists.`;
  }
  return null;
}

function syncPairCrudStateToGroups(): void {
  const groupNames = new Set(currentGroups.map((group) => group.name));
  if (renamePairTarget && !groupNames.has(renamePairTarget)) {
    renamePairTarget = null;
    renamePairDraft = "";
    renamePairError = null;
  }
  if (openPairMenu && !groupNames.has(openPairMenu)) {
    openPairMenu = null;
  }
  if (deletePairTarget && !groupNames.has(deletePairTarget)) {
    deletePairTarget = null;
  }
}

function focusPendingPairControl(): void {
  const target = pendingPairFocus;
  if (!target) {
    return;
  }

  pendingPairFocus = null;
  requestAnimationFrame(() => {
    if (target === "create") {
      document.querySelector<HTMLInputElement>("[data-pair-create-input]")?.focus();
      return;
    }
    if (target === "delete") {
      document.querySelector<HTMLButtonElement>("[data-pair-delete-confirm]")?.focus();
      return;
    }
    if (target.startsWith("rename:")) {
      const name = target.slice("rename:".length);
      document
        .querySelector<HTMLInputElement>(`[data-pair-rename-input="${name}"]`)
        ?.focus();
    }
  });
}

function refreshPairPicker(activeGroup = currentActiveGroupName()): void {
  renderGroupPicker(currentGroups);
  wirePicker();
  setActiveGroup(activeGroup);
  focusPendingPairControl();
}

function startCreatePair(): void {
  openPairMenu = null;
  deletePairTarget = null;
  renamePairTarget = null;
  renamePairDraft = "";
  renamePairError = null;
  createPairMode = true;
  createPairDraft = "";
  createPairError = null;
  pendingPairFocus = "create";
  refreshPairPicker();
}

function cancelCreatePair(): void {
  if (pairCrudPending || !createPairMode) {
    return;
  }
  createPairMode = false;
  createPairDraft = "";
  createPairError = null;
  refreshPairPicker();
}

function startRenamePair(name: string): void {
  openPairMenu = null;
  createPairMode = false;
  createPairDraft = "";
  createPairError = null;
  deletePairTarget = null;
  renamePairTarget = name;
  renamePairDraft = name;
  renamePairError = null;
  pendingPairFocus = `rename:${name}`;
  refreshPairPicker();
}

function cancelRenamePair(): void {
  if (pairCrudPending || !renamePairTarget) {
    return;
  }
  renamePairTarget = null;
  renamePairDraft = "";
  renamePairError = null;
  refreshPairPicker();
}

function openDeletePairDialog(name: string): void {
  openPairMenu = null;
  deletePairTarget = name;
  pendingPairFocus = "delete";
  refreshPairPicker();
}

function closeDeletePairDialog(): void {
  if (pairCrudPending) {
    return;
  }
  deletePairTarget = null;
  refreshPairPicker();
}

function refreshSnapshotFromEvent(preferredGroup?: string): void {
  void refreshSnapshot(preferredGroup).catch((error) => {
    writeSystem(
      "error",
      `snapshot refresh failed after runtime event: ${String(error)}`,
    );
  });
}

function syncPaneInventory(
  sessions: SessionSnapshot[],
  preferredActiveGroup?: string,
): void {
  const nextNames = new Set(sessions.map((session) => session.name));
  for (const buffered of Array.from(pendingOutput.keys())) {
    if (!nextNames.has(buffered)) {
      const entry = pendingOutput.get(buffered);
      pendingOutput.delete(buffered);
      if (entry && (entry.chunks.length > 0 || entry.dropped > 0)) {
        writeSystem(
          "info",
          `pendingOutput dropped: ${buffered} (${entry.chunks.length} queued + ${entry.dropped} previously shed, session no longer in snapshot)`,
        );
      }
    }
  }

  const signature = sessions
    .map((session) => `${session.name}:${session.title}`)
    .join("|");
  if (signature === renderedSessionSignature) {
    return;
  }

  renderedSessionSignature = signature;
  const fragment = document.createDocumentFragment();

  for (const session of sessions) {
    const existingCard = document.querySelector<HTMLElement>(
      `[data-session-card="${session.name}"]`,
    );
    const card = existingCard ?? buildSessionCard(session);
    card.dataset.group = groupNameForSession(session.name);
    const title = card.querySelector("h2");
    if (title instanceof HTMLHeadingElement) {
      title.textContent = paneDisplayTitle(session.name, session.title);
    }
    fragment.appendChild(card);
  }

  workspaceGrid.replaceChildren(fragment);

  for (const [name, pane] of Array.from(paneMap.entries())) {
    if (!nextNames.has(name)) {
      pane.dispose();
      paneMap.delete(name);
      snapshotByName.delete(name);
    }
  }

  for (const session of sessions) {
    if (!paneMap.has(session.name)) {
      const pane = new SessionTerminal(session.name, session.title);
      paneMap.set(session.name, pane);
      const pending = pendingOutput.get(session.name);
      if (pending) {
        for (const chunk of pending.chunks) {
          pane.write(chunk);
        }
        pendingOutput.delete(session.name);
        if (pending.chunks.length > 0 || pending.dropped > 0) {
          const suffix =
            pending.dropped > 0
              ? ` (${pending.dropped} older chunks dropped due to cap)`
              : "";
          writeSystem(
            "info",
            `session_output flushed: ${pending.chunks.length} chunks into ${session.name}${suffix}`,
          );
        }
      }
    }
  }

  currentGroups = groupSessions(sessions);
  syncPairCrudStateToGroups();
  refreshPairPicker(resolveActiveGroupPreference(currentGroups, preferredActiveGroup));

  if (activeTerminalName && !paneMap.has(activeTerminalName)) {
    activeTerminalName = null;
  }
  if (activeCopySurface && activeCopySurface !== "system" && !paneMap.has(activeCopySurface)) {
    activeCopySurface = null;
  }

  applyTheme(currentThemeName());
  wireButtons();
}

function buildSessionCard(session: SessionSnapshot): HTMLElement {
  const article = document.createElement("article");
  article.className = "terminal-card panel";
  article.dataset.sessionCard = session.name;
  article.dataset.group = groupNameForSession(session.name);
  article.dataset.groupActive = "true";

  const isClaude = session.name === "claude" || session.name.endsWith("-claude");
  const trailingButton = isClaude
    ? `<button type="button" class="ghost" data-decor="clear">Clear</button>`
    : `<button type="button" class="ghost" data-decor="close">Close</button>`;

  article.innerHTML = `
    <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
    <span class="hud-antenna" aria-hidden="true"></span>
    <span class="hud-side-stripe" aria-hidden="true"></span>
    <div class="card-head terminal-head">
      <h2></h2>
      <div class="terminal-actions">
        <button type="button" data-action="start" data-session="${session.name}">Launch</button>
        <button type="button" data-action="restart" data-session="${session.name}">Restart</button>
        <button type="button" data-action="stop" data-session="${session.name}">Stop</button>
      </div>
      <div class="terminal-actions terminal-actions-right">
        ${trailingButton}
        <button type="button" class="ghost" data-decor="tile">Tile</button>
      </div>
      <div class="session-meta" hidden>
        <span class="state-pill" data-session-state="${session.name}">closed</span>
        <span class="activity-pill" data-session-activity="${session.name}">idle</span>
      </div>
    </div>
    <div class="terminal-host" data-terminal="${session.name}"></div>
  `;

  const title = article.querySelector("h2");
  if (!(title instanceof HTMLHeadingElement)) {
    throw new Error(`Missing title heading for ${session.name}`);
  }
  title.textContent = paneDisplayTitle(session.name, session.title);

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
  renderPairCreateControl();

  const fragment = document.createDocumentFragment();
  const activeGroup = currentActiveGroupName();
  for (const group of groups) {
    const item = document.createElement("li");
    const shell = document.createElement("div");
    shell.className = "group-chip-shell";
    shell.dataset.groupRoot = group.name;
    shell.dataset.active = String(group.name === activeGroup);

    if (renamePairTarget === group.name) {
      const input = document.createElement("input");
      input.type = "text";
      input.className = "new-pair-input";
      input.value = renamePairDraft;
      input.placeholder = "pair name";
      input.dataset.pairRenameInput = group.name;
      input.disabled = pairCrudPending;
      shell.appendChild(input);

      if (renamePairError) {
        const error = document.createElement("span");
        error.className = "pair-error";
        error.textContent = renamePairError;
        shell.appendChild(error);
      }
    } else {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "group-chip";
      button.dataset.group = group.name;
      button.dataset.active = String(group.name === activeGroup);
      button.textContent = group.name;
      shell.appendChild(button);
    }

    if (canManagePairGroup(group.name) && renamePairTarget !== group.name) {
      const menuRoot = document.createElement("div");
      menuRoot.className = "chip-menu-root";
      menuRoot.dataset.chipMenuRoot = group.name;

      const menuButton = document.createElement("button");
      menuButton.type = "button";
      menuButton.className = "chip-menu-button";
      menuButton.dataset.groupMenuToggle = group.name;
      menuButton.setAttribute("aria-label", `Manage ${group.name}`);
      menuButton.textContent = "⋯";
      menuRoot.appendChild(menuButton);

      if (openPairMenu === group.name) {
        const menu = document.createElement("div");
        menu.className = "chip-menu";
        const pairRunning = isPairRunning(group.name);
        const actions: Array<["rename" | "delete", string]> = [
          ["rename", "Rename"],
          ["delete", "Delete"],
        ];

        for (const [action, label] of actions) {
          const actionButton = document.createElement("button");
          actionButton.type = "button";
          actionButton.className = "chip-menu-action";
          actionButton.textContent = label;
          actionButton.dataset.pairMenuAction = action;
          actionButton.dataset.group = group.name;
          actionButton.disabled = pairRunning || pairCrudPending;
          actionButton.title = pairRunning
            ? `Stop both panes to ${action}`
            : "";
          menu.appendChild(actionButton);
        }

        menuRoot.appendChild(menu);
      }

      shell.appendChild(menuRoot);
    }

    item.appendChild(shell);
    fragment.appendChild(item);
  }
  groupList.replaceChildren(fragment);
  renderDeletePairDialog();
}

function renderPairCreateControl(): void {
  pairCreateSlot.replaceChildren();

  if (!createPairMode) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "new-pair-button";
    button.id = "new-pair-button";
    button.textContent = "+ New pair";
    button.disabled = pairCrudPending;
    pairCreateSlot.appendChild(button);
    return;
  }

  const wrapper = document.createElement("div");
  wrapper.className = "pair-inline-editor";
  const input = document.createElement("input");
  input.type = "text";
  input.className = "new-pair-input";
  input.value = createPairDraft;
  input.placeholder = "pair name";
  input.dataset.pairCreateInput = "true";
  input.disabled = pairCrudPending;
  wrapper.appendChild(input);

  if (createPairError) {
    const error = document.createElement("span");
    error.className = "pair-error";
    error.textContent = createPairError;
    wrapper.appendChild(error);
  }

  pairCreateSlot.appendChild(wrapper);
}

function renderDeletePairDialog(): void {
  pairDialogSlot.replaceChildren();
  // Remove any existing dialog from anywhere it might be living
  document.querySelector(".pair-dialog-overlay")?.remove();
  document.querySelectorAll(".pair-dialog").forEach((el) => el.remove());
  if (!deletePairTarget) {
    return;
  }

  // Render inline below the targeted chip-shell so the dialog feels anchored
  // to the pair it's about to delete.
  const targetShell = document.querySelector<HTMLElement>(
    `[data-group-root="${deletePairTarget}"]`,
  );
  if (!targetShell) {
    return;
  }

  const dialog = document.createElement("div");
  dialog.className = "pair-dialog";
  dialog.innerHTML = `
    <p class="card-kicker">Delete pair</p>
    <h3>Delete "${deletePairTarget}"?</h3>
    <p>Both panes stop and are removed. Audit log preserved.</p>
  `;

  const actions = document.createElement("div");
  actions.className = "pair-dialog-actions";

  const cancelButton = document.createElement("button");
  cancelButton.type = "button";
  cancelButton.textContent = "Cancel";
  cancelButton.dataset.pairDeleteCancel = deletePairTarget;
  cancelButton.disabled = pairCrudPending;

  const deleteButton = document.createElement("button");
  deleteButton.type = "button";
  deleteButton.className = "danger";
  deleteButton.textContent = "Delete";
  deleteButton.disabled = pairCrudPending;
  deleteButton.setAttribute("data-pair-delete-confirm", deletePairTarget);

  actions.append(cancelButton, deleteButton);
  dialog.appendChild(actions);
  targetShell.appendChild(dialog);
}

async function submitCreatePair(): Promise<void> {
  if (pairCrudPending) {
    return;
  }

  const name = createPairDraft.trim();
  const validationError = validatePairName(name);
  if (validationError) {
    createPairError = validationError;
    pendingPairFocus = "create";
    refreshPairPicker();
    return;
  }

  pairCrudPending = true;
  createPairError = null;
  try {
    await command<SessionSnapshot[]>("create_pair", {
      request: { name } satisfies CreatePairRequest,
    });
    createPairMode = false;
    createPairDraft = "";
    pairCrudPending = false;
    await refreshSnapshot(name);
  } catch (error) {
    pairCrudPending = false;
    createPairError = String(error);
    pendingPairFocus = "create";
    refreshPairPicker();
  }
}

async function submitRenamePair(): Promise<void> {
  if (pairCrudPending || !renamePairTarget) {
    return;
  }

  const oldName = renamePairTarget;
  const newName = renamePairDraft.trim();
  const validationError = validatePairName(newName, oldName);
  if (validationError) {
    renamePairError = validationError;
    pendingPairFocus = `rename:${oldName}`;
    refreshPairPicker();
    return;
  }

  const wasActive = currentActiveGroupName() === oldName;
  pairCrudPending = true;
  renamePairError = null;
  try {
    await command<SessionSnapshot[]>("rename_pair", {
      request: { oldName, newName } satisfies RenamePairRequest,
    });
    renamePairTarget = null;
    renamePairDraft = "";
    pairCrudPending = false;
    await refreshSnapshot(wasActive ? newName : undefined);
  } catch (error) {
    pairCrudPending = false;
    renamePairError = String(error);
    pendingPairFocus = `rename:${oldName}`;
    refreshPairPicker();
  }
}

async function confirmDeletePair(): Promise<void> {
  if (pairCrudPending || !deletePairTarget) {
    return;
  }

  const name = deletePairTarget;
  const wasActive = currentActiveGroupName() === name;
  pairCrudPending = true;
  try {
    await command<void>("delete_pair", {
      request: { name } satisfies DeletePairRequest,
    });
    deletePairTarget = null;
    pairCrudPending = false;
    await refreshSnapshot(wasActive ? "main" : undefined);
  } catch (error) {
    pairCrudPending = false;
    writeSystem("error", `delete ${name} failed: ${String(error)}`);
    refreshPairPicker();
  }
}

function wirePairPickerDismiss(): void {
  if (pairPickerDismissWired) {
    return;
  }

  const closePairMenu = (): void => {
    if (!openPairMenu) {
      return;
    }
    openPairMenu = null;
    refreshPairPicker();
  };

  pairPickerDismissWired = true;
  document.addEventListener("click", (event) => {
    if (!(event.target instanceof Element)) {
      return;
    }
    if (openPairMenu && !event.target.closest("[data-chip-menu-root]")) {
      closePairMenu();
    }
  });
  document.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") {
      return;
    }
    if (deletePairTarget) {
      event.preventDefault();
      closeDeletePairDialog();
      return;
    }
    if (renamePairTarget) {
      event.preventDefault();
      cancelRenamePair();
      return;
    }
    if (createPairMode) {
      event.preventDefault();
      cancelCreatePair();
      return;
    }
    if (openPairMenu) {
      event.preventDefault();
      closePairMenu();
    }
  });
}

function wirePairPickerActions(): void {
  if (pairPickerActionsWired) {
    return;
  }
  pairPickerActionsWired = true;

  document.addEventListener("click", (event) => {
    if (!(event.target instanceof Element)) {
      return;
    }

    const actionButton = event.target.closest<HTMLButtonElement>("[data-pair-menu-action]");
    if (actionButton) {
      if (actionButton.disabled) {
        return;
      }
      const name = actionButton.dataset.group;
      const pairAction = actionButton.dataset.pairMenuAction;
      if (!name || !pairAction) {
        return;
      }
      if (pairAction === "rename") {
        startRenamePair(name);
      } else if (pairAction === "delete") {
        openDeletePairDialog(name);
      }
      return;
    }

    const toggle = event.target.closest<HTMLButtonElement>("[data-group-menu-toggle]");
    if (toggle) {
      const name = toggle.dataset.groupMenuToggle;
      if (!name) {
        return;
      }
      openPairMenu = openPairMenu === name ? null : name;
      refreshPairPicker();
      return;
    }

    const chip = event.target.closest<HTMLButtonElement>(".group-chip");
    if (chip) {
      const name = chip.dataset.group;
      if (!name) {
        return;
      }
      openPairMenu = null;
      refreshPairPicker(name);
      return;
    }

    const newPair = event.target.closest<HTMLButtonElement>("#new-pair-button");
    if (newPair) {
      startCreatePair();
      return;
    }
  });
}

function wirePicker(): void {
  wirePairPickerActions();
  wirePairPickerDismiss();

  document
    .querySelector<HTMLInputElement>("[data-pair-create-input]")
    ?.addEventListener("input", (event) => {
      createPairDraft = (event.currentTarget as HTMLInputElement).value;
      createPairError = null;
    });
  document
    .querySelector<HTMLInputElement>("[data-pair-create-input]")
    ?.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        void submitCreatePair();
      }
      if (event.key === "Escape") {
        event.preventDefault();
        cancelCreatePair();
      }
    });
  document
    .querySelector<HTMLInputElement>("[data-pair-create-input]")
    ?.addEventListener("blur", () => {
      cancelCreatePair();
    });

  for (const input of document.querySelectorAll<HTMLInputElement>("[data-pair-rename-input]")) {
    input.addEventListener("input", (event) => {
      renamePairDraft = (event.currentTarget as HTMLInputElement).value;
      renamePairError = null;
    });
    input.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        void submitRenamePair();
      }
      if (event.key === "Escape") {
        event.preventDefault();
        cancelRenamePair();
      }
    });
    input.addEventListener("blur", () => {
      cancelRenamePair();
    });
  }

  document
    .querySelector<HTMLButtonElement>("[data-pair-delete-cancel]")
    ?.addEventListener("click", () => {
      closeDeletePairDialog();
    });
  document
    .querySelector<HTMLButtonElement>("[data-pair-delete-confirm]")
    ?.addEventListener("click", () => {
      void confirmDeletePair();
    });
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
  const previousFrom = routeFromCombobox.value;
  const previousTo = routeToCombobox.value;
  const sessionNames = sessions.map((session) => session.name);

  const fromOptions: ComboboxOption[] = [
    { value: "operator", label: "Operator" },
    ...sessions.map((s) => ({ value: s.name, label: paneDisplayTitle(s.name, s.title) })),
  ];
  routeFromCombobox.setOptions(fromOptions);
  routeFromCombobox.value = previousFrom === "operator" || sessionNames.includes(previousFrom)
    ? previousFrom
    : "operator";

  const toOptions: ComboboxOption[] = [
    ...sessions.map((s) => ({ value: s.name, label: paneDisplayTitle(s.name, s.title) })),
    { value: "room", label: "Room" },
  ];
  routeToCombobox.setOptions(toOptions);
  routeToCombobox.value = previousTo === "room" || sessionNames.includes(previousTo)
    ? previousTo
    : "room";
}

function wireButtons(): void {
  if (paneButtonsWired) {
    return;
  }
  paneButtonsWired = true;

  document.addEventListener("click", async (event) => {
    if (!(event.target instanceof Element)) {
      return;
    }
    const button = event.target.closest<HTMLButtonElement>("[data-action]");
    if (!button) {
      return;
    }
    const action = button.dataset.action;
    const session = button.dataset.session;
    if (!action || !session) {
      return;
    }

    if (action === "start" && snapshotByName.get(session)?.running) {
      writeSystem("info", `${paneLabel(session)} is already running`);
      return;
    }

    try {
      if (action === "start") {
        const snapshot = await command<SessionSnapshot>("start_session", {
          request: { name: session },
        });
        snapshotByName.set(snapshot.name, snapshot);
        paneMap.get(snapshot.name)?.applySnapshot(snapshot);
      } else if (action === "restart") {
        const snapshot = await command<SessionSnapshot>("restart_session", {
          request: { name: session },
        });
        snapshotByName.set(snapshot.name, snapshot);
        paneMap.get(snapshot.name)?.applySnapshot(snapshot);
      } else if (action === "stop") {
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

function wireRouter(): void {
  const form = must<HTMLFormElement>("#router-form");
  const content = must<HTMLTextAreaElement>("#route-content");
  const clearButton = must<HTMLButtonElement>("#clear-router");

  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const trimmed = content.value.trim();
    if (!trimmed) {
      return;
    }

    const request: RouteMessageRequest = {
      from: routeFromCombobox.value,
      to: routeToCombobox.value,
      scope: routeToCombobox.value === "room" ? "room" : "direct",
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
          await refreshSnapshot();
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
        : `\x1b[38;2;${secondaryAnsiRgb()}m`;
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
