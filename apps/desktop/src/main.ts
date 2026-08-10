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
  completeInitialRunEventReconciliation,
  createRunEventGateState,
  flushRunEventGateWarnings,
  forgetRunEventSession,
  handleRuntimeEvent,
  reconcileSessionSnapshot,
  registerRuntimeEventsBeforeBootstrap,
  retireRendererSession,
  type PendingBuffer,
  type RuntimeEventContext,
} from "./runtime-events";
import {
  canUseUnsafePermission,
  driverLabel,
  lifecycleLabel,
  movedSessionOrder,
  newSessionFormDefaults,
  permissionProfileForDriver,
  reconcileSessionTabs,
  relativeSessionId,
  resolveFocusedTabAction,
  resolveGlobalSessionShortcut,
  sessionTabDescription,
  shouldShowZeroSession,
  shortSessionId,
  unsafePermissionWarning,
  type SessionTabsState,
} from "./session-tabs";
import "./styles.css";
import type {
  ChooseSessionWorkingDirectoryRequest,
  CreateSessionRequest,
  DeleteSessionRequest,
  DriverKind,
  MoveSessionRequest,
  PermissionProfile,
  RenameSessionRequest,
  RuntimeEvent,
  RuntimeSnapshot,
  SendInputRequest,
  SetSessionPermissionRequest,
  SessionSnapshot,
} from "./types";

type SessionFormMode =
  | { kind: "create" }
  | { kind: "edit"; sessionId: string };

const app = document.querySelector("#app");
if (!(app instanceof HTMLDivElement)) {
  throw new Error("Missing #app root element");
}

app.innerHTML = `
  <div class="app-shell">
    <div class="prim1-ticker" aria-hidden="true">
      <div class="prim1-ticker-track">
        <span class="prim1-ticker-cell">PRIM-1 <span class="prim1-ticker-glyph">&#8756;</span> LOCAL MULTI-HARNESS RUNTIME</span>
        <span class="prim1-ticker-sep">&#9670;</span>
        <span class="prim1-ticker-cell">SUPERVISOR-OWNED PTYS</span>
        <span class="prim1-ticker-sep">&#9670;</span>
        <span class="prim1-ticker-cell">VISIBLE TERMINAL SESSIONS</span>
        <span class="prim1-ticker-sep">&#9670;</span>
        <span class="prim1-ticker-cell">SESSIONID-KEYED TABS</span>
        <span class="prim1-ticker-sep">&#9670;</span>
        <span class="prim1-ticker-cell">METADATA-ONLY AUDIT</span>
        <span class="prim1-ticker-sep">&#9670;</span>
        <span class="prim1-ticker-cell">PRIM-1 <span class="prim1-ticker-glyph">&#8756;</span> LOCAL MULTI-HARNESS RUNTIME</span>
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
        <h1>PRIM-1</h1>
        <span class="topbar-active-tag">Sessions</span>
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
      <nav class="session-tabs-shell" aria-label="Terminal sessions">
        <div class="session-tabs" id="session-tabs" role="tablist" aria-label="Sessions"></div>
        <button type="button" class="new-session-button" id="new-session-button">+ New session</button>
      </nav>

      <section class="session-editor panel" id="session-editor" hidden aria-labelledby="session-editor-title">
        <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
        <form id="session-form" class="session-form">
          <div class="session-editor-heading">
            <div>
              <p class="card-kicker">Session definition</p>
              <h2 id="session-editor-title">New session</h2>
            </div>
            <button type="button" class="ghost" id="cancel-session-form">Cancel</button>
          </div>
          <label>
            <span>Label <small>optional for new sessions</small></span>
            <input id="session-label" type="text" maxlength="80" autocomplete="off" />
          </label>
          <label>
            <span>Harness</span>
            <select id="session-driver">
              <option value="claude">Claude Code</option>
              <option value="codex">Codex</option>
              <option value="grok">Grok Build</option>
              <option value="generic_terminal">Generic terminal</option>
            </select>
          </label>
          <label>
            <span>Permission profile</span>
            <select id="session-permission">
              <option value="normal">Normal</option>
              <option value="unsafe">Unsafe — bypass approval prompts</option>
            </select>
          </label>
          <div class="session-path-field">
            <span>Working directory</span>
            <output id="session-working-directory" class="session-path">No workspace selected</output>
            <button type="button" id="browse-session-directory">Browse…</button>
          </div>
          <p class="permission-warning" id="permission-warning" role="alert" hidden></p>
          <p class="session-form-note" id="session-form-note"></p>
          <p class="session-form-error" id="session-form-error" role="alert" hidden></p>
          <div class="session-form-actions">
            <button type="submit" class="primary" id="save-session">Create session</button>
          </div>
        </form>
      </section>

      <section class="zero-session panel" id="zero-session" hidden>
        <p class="card-kicker">Workspace ready</p>
        <h2>No terminal sessions</h2>
        <p>Create a harness session in <span id="zero-workspace">the selected workspace</span>.</p>
        <button type="button" class="primary" id="zero-new-session">New session</button>
      </section>

      <section class="workspace-grid" id="workspace-grid" aria-live="polite"></section>
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
            <span class="mono" id="runtime-path" hidden>runtime pending</span>
            <span class="log-toggle" aria-hidden="true">&lt; Log &gt;</span>
          </div>
        </div>
        <div class="system-terminal" id="system-terminal"></div>
        <span class="card-ctrl-chip" aria-hidden="true">CTRL</span>
      </article>

    </section>

    <div class="control-flyout">
      <button class="control-toggle" aria-label="Open controls">CTRL</button>
      <div class="control-menu">
        <button data-control="refresh">Refresh snapshot</button>
      </div>
    </div>
  </div>
`;

class SessionTerminal {
  readonly sessionId: string;
  alias: string;
  label: string;
  readonly terminal: Terminal;
  readonly fitAddon: FitAddon;
  readonly host: HTMLDivElement;
  readonly stateEl: HTMLSpanElement;
  readonly activityEl: HTMLSpanElement;
  private hooked = false;
  private snapshot: SessionSnapshot | null = null;

  constructor(sessionId: string, alias: string, label: string) {
    this.sessionId = sessionId;
    this.alias = alias;
    this.label = label;
    this.host = must<HTMLDivElement>(`[data-terminal="${sessionId}"]`);
    this.stateEl = must<HTMLSpanElement>(`[data-session-state="${sessionId}"]`);
    this.activityEl = must<HTMLSpanElement>(`[data-session-activity="${sessionId}"]`);
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
      tabState.activeId = this.sessionId;
      activeCopySurface = this.sessionId;
    });
    this.host.addEventListener("mousedown", () => {
      tabState.activeId = this.sessionId;
      activeCopySurface = this.sessionId;
    });
    this.writeInitialBanner();
  }

  writeInitialBanner(): void {
    this.terminal.writeln(`\x1b[38;2;${brandBannerAnsi()}m${this.label} pane ready.\x1b[0m`);
    this.terminal.writeln("Launch the session from the header to begin.");
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
          session_id: this.sessionId,
          input,
        } satisfies SendInputRequest,
      }).catch((error) => writeSystem("error", `${this.label} input failed: ${error}`));
    });
  }

  applySnapshot(snapshot: SessionSnapshot): void {
    this.alias = snapshot.alias;
    this.label = snapshot.label;
    this.snapshot = snapshot;
    this.stateEl.textContent = snapshot.lifecycle_state;
    this.stateEl.dataset.state = snapshot.lifecycle_state;
    this.activityEl.textContent = snapshot.last_activity_at
      ? new Date(snapshot.last_activity_at).toLocaleTimeString()
      : snapshot.running
        ? "running"
        : "idle";
    this.activityEl.dataset.running = String(snapshot.running);

    if (isPaneVisible(this.sessionId)) {
      this.fitAddon.fit();
      void resizeSession(this.sessionId, this.terminal.cols, this.terminal.rows);
    }
    this.hookInput();
  }

  write(chunk: string): void {
    this.terminal.write(chunk);
  }

  fit(): void {
    if (!isPaneVisible(this.sessionId)) {
      return;
    }
    this.fitAddon.fit();
    void resizeSession(this.sessionId, this.terminal.cols, this.terminal.rows);
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
const sessionTabs = must<HTMLDivElement>("#session-tabs");
const newSessionButton = must<HTMLButtonElement>("#new-session-button");
const sessionEditor = must<HTMLElement>("#session-editor");
const sessionEditorTitle = must<HTMLHeadingElement>("#session-editor-title");
const sessionForm = must<HTMLFormElement>("#session-form");
const sessionLabelInput = must<HTMLInputElement>("#session-label");
const sessionDriverSelect = must<HTMLSelectElement>("#session-driver");
const sessionPermissionSelect = must<HTMLSelectElement>("#session-permission");
const sessionWorkingDirectory = must<HTMLOutputElement>("#session-working-directory");
const browseSessionDirectory = must<HTMLButtonElement>("#browse-session-directory");
const permissionWarning = must<HTMLElement>("#permission-warning");
const sessionFormNote = must<HTMLElement>("#session-form-note");
const sessionFormError = must<HTMLElement>("#session-form-error");
const saveSessionButton = must<HTMLButtonElement>("#save-session");
const zeroSession = must<HTMLElement>("#zero-session");
const zeroWorkspace = must<HTMLElement>("#zero-workspace");
const zeroNewSession = must<HTMLButtonElement>("#zero-new-session");
const snapshotById = new Map<string, SessionSnapshot>();
let latestSnapshotRequest = 0;

const paneMap = new Map<string, SessionTerminal>();
const pendingOutput = new Map<string, PendingBuffer>();
const controlEndpoint = must<HTMLElement>("#control-endpoint");
const auditPath = must<HTMLElement>("#audit-path");
const runtimePath = must<HTMLElement>("#runtime-path");
let tabState: SessionTabsState = { order: [], activeId: null };
let activeCopySurface: CopySurface = null;
let workspacePreference = "";
let sessionFormMode: SessionFormMode | null = null;
let sessionFormPending = false;
let zeroStateWasVisible = false;
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

wireSessionUi();
wireControls();
wireResize();
wireTerminalShortcuts();
wireThemeToggle();

const runtimeEventContext: RuntimeEventContext = {
  writeSystem,
  refreshSnapshotFromEvent,
  snapshotById,
  pendingOutput,
  runEventGate: createRunEventGateState(),
  writeToPane(sessionId, chunk) {
    const pane = paneMap.get(sessionId);
    if (pane) {
      pane.write(chunk);
      return true;
    }
    return false;
  },
  applyPaneSnapshot(sessionId, snapshot) {
    if (snapshot.session_id === sessionId) {
      applySessionSnapshotToUi(snapshot);
    }
  },
  setControlEndpoint(endpoint) {
    controlEndpoint.textContent = endpoint;
  },
};

void registerRuntimeEventsBeforeBootstrap(
  () =>
    listen<RuntimeEvent>("runtime://event", ({ payload }) => {
      handleRuntimeEvent(payload, runtimeEventContext);
    }),
  bootstrap,
).catch((error) => {
  writeSystem("error", `UI initialization failed: ${String(error)}`);
});

async function bootstrap(): Promise<void> {
  await refreshSnapshot();
  writeSystem("info", "UI attached to supervisor.");
}

async function refreshSnapshot(preferredSessionId?: string): Promise<RuntimeSnapshot> {
  const request = ++latestSnapshotRequest;
  const snapshot = await command<RuntimeSnapshot>("bootstrap");
  if (request === latestSnapshotRequest) {
    applySnapshot(snapshot, preferredSessionId);
  }
  return snapshot;
}

function applySnapshot(snapshot: RuntimeSnapshot, preferredSessionId?: string): void {
  const acceptedSessions = new Set<string>();
  for (const session of snapshot.sessions) {
    const reconciliation = reconcileSessionSnapshot(
      session,
      snapshotById,
      runtimeEventContext.runEventGate,
    );
    if (reconciliation.accepted) {
      acceptedSessions.add(session.session_id);
    }
  }
  flushRunEventGateWarnings(runtimeEventContext);

  workspacePreference = snapshot.workspace_preference;
  syncPaneInventory(snapshot.sessions, preferredSessionId);
  syncSessionForm();
  controlEndpoint.textContent = snapshot.control_plane?.endpoint ?? "starting...";
  auditPath.textContent = snapshot.audit_log_path;
  runtimePath.textContent = snapshot.runtime_dir;

  for (const session of snapshot.sessions) {
    if (acceptedSessions.has(session.session_id)) {
      paneMap.get(session.session_id)?.applySnapshot(session);
    }
  }
  completeInitialRunEventReconciliation(runtimeEventContext);
}

function applyCommandSessionSnapshot(snapshot: SessionSnapshot): void {
  const reconciliation = reconcileSessionSnapshot(
    snapshot,
    snapshotById,
    runtimeEventContext.runEventGate,
  );
  flushRunEventGateWarnings(runtimeEventContext);
  if (!reconciliation.accepted) {
    return;
  }
  applySessionSnapshotToUi(snapshot);
}

function applySessionSnapshotToUi(snapshot: SessionSnapshot): void {
  paneMap.get(snapshot.session_id)?.applySnapshot(snapshot);
  const card = document.querySelector<HTMLElement>(
    '[data-session-card="' + snapshot.session_id + '"]',
  );
  if (card) {
    updateSessionCard(card, snapshot);
  }
  updateSessionTab(snapshot);
  setActiveSession(tabState.activeId, false);
  syncSessionForm();
}

function refreshSnapshotFromEvent(preferredSessionId?: string): void {
  void refreshSnapshot(preferredSessionId).catch((error) => {
    writeSystem(
      "error",
      "snapshot refresh failed after runtime event: " + String(error),
    );
  });
}

function syncPaneInventory(
  sessions: SessionSnapshot[],
  preferredSessionId?: string,
): void {
  const orderedIds = sessions.map((session) => session.session_id);
  const transition = reconcileSessionTabs(
    tabState,
    orderedIds,
    preferredSessionId,
  );
  const nextIds = new Set(orderedIds);

  for (const buffered of Array.from(pendingOutput.keys())) {
    if (nextIds.has(buffered)) {
      continue;
    }
    const entry = pendingOutput.get(buffered);
    pendingOutput.delete(buffered);
    if (entry && (entry.chunks.length > 0 || entry.dropped > 0)) {
      writeSystem(
        "info",
        "pendingOutput dropped for removed session "
          + shortSessionId(buffered)
          + " ("
          + entry.chunks.length
          + " queued + "
          + entry.dropped
          + " previously shed)",
      );
    }
  }

  for (const [sessionId, removedSnapshot] of Array.from(snapshotById.entries())) {
    if (nextIds.has(sessionId)) {
      continue;
    }
    retireRendererSession({
      clearPending: () => pendingOutput.delete(sessionId),
      disposePane: () => {
        paneMap.get(sessionId)?.dispose();
        paneMap.delete(sessionId);
      },
      removeCard: () => {
        document
          .querySelector<HTMLElement>(
            '[data-session-card="' + sessionId + '"]',
          )
          ?.remove();
      },
      forgetRunEventState: () =>
        forgetRunEventSession(
          removedSnapshot.session_id,
          runtimeEventContext.runEventGate,
        ),
      removeSnapshot: () => snapshotById.delete(sessionId),
      clearFocus: () => {
        if (activeCopySurface === sessionId) {
          activeCopySurface = null;
        }
      },
    });
  }

  const effectiveSessions = sessions.map(
    (session) => snapshotById.get(session.session_id) ?? session,
  );
  const fragment = document.createDocumentFragment();
  for (const session of effectiveSessions) {
    const existingCard = document.querySelector<HTMLElement>(
      '[data-session-card="' + session.session_id + '"]',
    );
    const card = existingCard ?? buildSessionCard(session);
    updateSessionCard(card, session);
    fragment.appendChild(card);
  }
  workspaceGrid.replaceChildren(fragment);

  for (const session of effectiveSessions) {
    if (paneMap.has(session.session_id)) {
      continue;
    }
    const pane = new SessionTerminal(
      session.session_id,
      session.alias,
      session.label,
    );
    paneMap.set(session.session_id, pane);
    const pending = pendingOutput.get(session.session_id);
    if (!pending) {
      continue;
    }
    for (const chunk of pending.chunks) {
      pane.write(chunk);
    }
    pendingOutput.delete(session.session_id);
    if (pending.chunks.length > 0 || pending.dropped > 0) {
      const suffix =
        pending.dropped > 0
          ? " (" + pending.dropped + " older chunks dropped due to cap)"
          : "";
      writeSystem(
        "info",
        "session_output flushed: "
          + pending.chunks.length
          + " chunks into "
          + session.label
          + suffix,
      );
    }
  }

  tabState = transition.state;
  renderSessionTabs(effectiveSessions);
  updateZeroSessionState();
  setActiveSession(tabState.activeId, false);
  applyTheme(currentThemeName());
  wireButtons();
}

function buildSessionCard(session: SessionSnapshot): HTMLElement {
  const article = document.createElement("article");
  article.className = "terminal-card panel";
  article.dataset.sessionCard = session.session_id;
  article.id = "session-panel-" + session.session_id;
  article.setAttribute("role", "tabpanel");
  article.setAttribute("aria-labelledby", "session-tab-" + session.session_id);
  article.innerHTML = [
    '<i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>',
    '<span class="hud-antenna" aria-hidden="true"></span>',
    '<span class="hud-side-stripe" aria-hidden="true"></span>',
    '<div class="card-head terminal-head">',
    '<div class="terminal-identity">',
    "<h2></h2>",
    '<div class="terminal-facts">',
    '<span data-session-driver></span>',
    '<span data-session-permission></span>',
    '<span data-session-cwd></span>',
    "</div>",
    "</div>",
    '<div class="terminal-actions">',
    '<button type="button" data-action="start" data-session="' + session.session_id + '">Launch</button>',
    '<button type="button" data-action="restart" data-session="' + session.session_id + '">Restart</button>',
    '<button type="button" data-action="stop" data-session="' + session.session_id + '">Stop</button>',
    '<button type="button" class="ghost" data-edit-session="' + session.session_id + '">Edit</button>',
    "</div>",
    '<div class="session-meta">',
    '<span class="state-pill" data-session-state="' + session.session_id + '">closed</span>',
    '<span class="activity-pill" data-session-activity="' + session.session_id + '">idle</span>',
    "</div>",
    "</div>",
    '<div class="terminal-host" data-terminal="' + session.session_id + '"></div>',
  ].join("");
  return article;
}

function updateSessionCard(
  card: HTMLElement,
  session: SessionSnapshot,
): void {
  const heading = card.querySelector("h2");
  if (heading) {
    heading.textContent = session.label;
  }
  const driver = card.querySelector<HTMLElement>("[data-session-driver]");
  const permission = card.querySelector<HTMLElement>("[data-session-permission]");
  const cwd = card.querySelector<HTMLElement>("[data-session-cwd]");
  if (driver) {
    driver.textContent = driverLabel(session.driver);
  }
  if (permission) {
    permission.textContent =
      session.permission_profile === "unsafe" ? "Unsafe" : "Normal";
    permission.dataset.permission = session.permission_profile;
  }
  if (cwd) {
    cwd.textContent = session.working_dir;
    cwd.title = session.working_dir;
  }
  card.title = sessionTabDescription(session);

  const launch = card.querySelector<HTMLButtonElement>('[data-action="start"]');
  const restart = card.querySelector<HTMLButtonElement>('[data-action="restart"]');
  const stop = card.querySelector<HTMLButtonElement>('[data-action="stop"]');
  if (launch) {
    launch.disabled = session.running;
  }
  if (restart) {
    restart.disabled = false;
  }
  if (stop) {
    stop.disabled = !session.running;
  }
}

function renderSessionTabs(sessions: SessionSnapshot[]): void {
  const byId = new Map(
    sessions.map((session) => [session.session_id, session] as const),
  );
  const fragment = document.createDocumentFragment();
  for (const [index, sessionId] of tabState.order.entries()) {
    const session = byId.get(sessionId);
    if (!session) {
      continue;
    }
    const shell = document.createElement("div");
    shell.className = "session-tab-shell";
    shell.dataset.sessionTabShell = sessionId;

    const tab = document.createElement("button");
    tab.type = "button";
    tab.className = "session-tab";
    tab.id = "session-tab-" + sessionId;
    tab.dataset.sessionTab = sessionId;
    tab.setAttribute("role", "tab");
    tab.setAttribute(
      "aria-selected",
      String(tabState.activeId === sessionId),
    );
    tab.setAttribute("aria-controls", "session-panel-" + sessionId);
    tab.tabIndex = tabState.activeId === sessionId ? 0 : -1;
    tab.title = sessionTabDescription(session);

    const label = document.createElement("span");
    label.className = "session-tab-label";
    label.dataset.sessionTabLabel = sessionId;
    label.textContent = session.label;
    const detail = document.createElement("span");
    detail.className = "session-tab-detail";
    detail.dataset.sessionTabDetail = sessionId;
    detail.textContent =
      driverLabel(session.driver)
      + " · "
      + session.working_dir
      + " · "
      + shortSessionId(sessionId);
    const badges = document.createElement("span");
    badges.className = "session-tab-badges";
    badges.dataset.sessionTabBadges = sessionId;
    badges.append(
      tabBadge(
        session.permission_profile === "unsafe" ? "Unsafe" : "Normal",
        "permission",
        session.permission_profile,
      ),
      tabBadge(
        lifecycleLabel(session.lifecycle_state),
        "lifecycle",
        session.lifecycle_state,
      ),
    );
    tab.append(label, detail, badges);
    tab.addEventListener("click", () => setActiveSession(sessionId, false));
    tab.addEventListener("keydown", (event) =>
      handleFocusedTabKeydown(event, sessionId),
    );

    shell.append(
      tab,
      tabActionButton("←", "Move " + session.label + " left", "move", sessionId, -1, index === 0),
      tabActionButton("→", "Move " + session.label + " right", "move", sessionId, 1, index === tabState.order.length - 1),
      tabActionButton("Edit", "Edit " + session.label, "edit", sessionId),
      tabActionButton(
        "Close",
        session.running
          ? "Stop " + session.label + " before closing"
          : "Close " + session.label,
        "close",
        sessionId,
        undefined,
        session.running,
      ),
    );
    fragment.appendChild(shell);
  }
  sessionTabs.replaceChildren(fragment);
}

function updateSessionTab(session: SessionSnapshot): void {
  const shell = sessionTabs.querySelector<HTMLElement>(
    '[data-session-tab-shell="' + session.session_id + '"]',
  );
  if (!shell) {
    return;
  }
  const tab = shell.querySelector<HTMLButtonElement>("[data-session-tab]");
  const label = shell.querySelector<HTMLElement>("[data-session-tab-label]");
  const detail = shell.querySelector<HTMLElement>("[data-session-tab-detail]");
  const badges = shell.querySelector<HTMLElement>("[data-session-tab-badges]");
  if (tab) {
    tab.title = sessionTabDescription(session);
  }
  if (label) {
    label.textContent = session.label;
  }
  if (detail) {
    detail.textContent =
      driverLabel(session.driver)
      + " · "
      + session.working_dir
      + " · "
      + shortSessionId(session.session_id);
  }
  if (badges) {
    badges.replaceChildren(
      tabBadge(
        session.permission_profile === "unsafe" ? "Unsafe" : "Normal",
        "permission",
        session.permission_profile,
      ),
      tabBadge(
        lifecycleLabel(session.lifecycle_state),
        "lifecycle",
        session.lifecycle_state,
      ),
    );
  }
  const close = shell.querySelector<HTMLButtonElement>(
    '[data-tab-action="close"]',
  );
  if (close) {
    close.disabled = session.running;
    const closeLabel = session.running
      ? "Stop " + session.label + " before closing"
      : "Close " + session.label;
    close.title = closeLabel;
    close.setAttribute("aria-label", closeLabel);
  }
}

function tabBadge(
  text: string,
  kind: string,
  value: string,
): HTMLSpanElement {
  const badge = document.createElement("span");
  badge.className = "session-tab-badge";
  badge.dataset.badgeKind = kind;
  badge.dataset.badgeValue = value;
  badge.textContent = text;
  return badge;
}

function tabActionButton(
  text: string,
  label: string,
  action: "move" | "edit" | "close",
  sessionId: string,
  delta?: -1 | 1,
  disabled = false,
): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "session-tab-action";
  button.textContent = text;
  button.setAttribute("aria-label", label);
  button.title = label;
  button.dataset.tabAction = action;
  button.dataset.session = sessionId;
  if (delta) {
    button.dataset.delta = String(delta);
  }
  button.disabled = disabled;
  return button;
}

function setActiveSession(
  requestedId: string | null,
  focusTerminal: boolean,
): void {
  const activeId =
    requestedId && tabState.order.includes(requestedId)
      ? requestedId
      : tabState.order[0] ?? null;
  tabState = { ...tabState, activeId };

  for (const tab of sessionTabs.querySelectorAll<HTMLButtonElement>(
    "[data-session-tab]",
  )) {
    const selected = tab.dataset.sessionTab === activeId;
    tab.setAttribute("aria-selected", String(selected));
    tab.tabIndex = selected ? 0 : -1;
  }
  for (const card of workspaceGrid.querySelectorAll<HTMLElement>(
    "[data-session-card]",
  )) {
    const selected = card.dataset.sessionCard === activeId;
    card.hidden = !selected;
    card.inert = !selected;
    card.setAttribute("aria-hidden", String(!selected));
  }
  if (activeId) {
    requestAnimationFrame(() => {
      const pane = paneMap.get(activeId);
      pane?.fit();
      if (focusTerminal) {
        pane?.terminal.focus();
      }
    });
  }
}

function handleFocusedTabKeydown(
  event: KeyboardEvent,
  sessionId: string,
): void {
  const currentIndex = tabState.order.indexOf(sessionId);
  const action = resolveFocusedTabAction(
    shortcutInput(event),
    currentIndex,
    tabState.order.length,
  );
  if (!action) {
    return;
  }
  event.preventDefault();
  if (action.kind === "move") {
    void moveSession(sessionId, action.delta);
    return;
  }
  const targetId = tabState.order[action.index];
  if (!targetId) {
    return;
  }
  setActiveSession(targetId, false);
  sessionTabs
    .querySelector<HTMLButtonElement>(
      '[data-session-tab="' + targetId + '"]',
    )
    ?.focus();
}

function updateZeroSessionState(): void {
  const visible = shouldShowZeroSession(
    tabState.order,
    sessionFormMode !== null,
  );
  zeroSession.hidden = !visible;
  workspaceGrid.hidden = tabState.order.length === 0;
  zeroWorkspace.textContent = workspacePreference || "the selected workspace";
  if (visible && !zeroStateWasVisible) {
    requestAnimationFrame(() => zeroNewSession.focus());
  }
  zeroStateWasVisible = visible;
}

function openCreateSessionForm(): void {
  const defaults = newSessionFormDefaults(workspacePreference);
  sessionFormMode = { kind: "create" };
  sessionFormPending = false;
  sessionLabelInput.value = "";
  sessionDriverSelect.value = defaults.driver;
  sessionPermissionSelect.value = defaults.permissionProfile;
  setSessionFormError(null);
  syncSessionForm();
  requestAnimationFrame(() => sessionLabelInput.focus());
}

function openEditSessionForm(sessionId: string): void {
  const session = snapshotById.get(sessionId);
  if (!session) {
    return;
  }
  sessionFormMode = { kind: "edit", sessionId };
  sessionFormPending = false;
  sessionLabelInput.value = session.label;
  sessionDriverSelect.value = session.driver;
  sessionPermissionSelect.value = session.permission_profile;
  setSessionFormError(null);
  syncSessionForm();
  requestAnimationFrame(() => sessionLabelInput.focus());
}

function closeSessionForm(): void {
  sessionFormMode = null;
  sessionFormPending = false;
  sessionEditor.hidden = true;
  setSessionFormError(null);
  updateZeroSessionState();
  if (tabState.activeId) {
    sessionTabs
      .querySelector<HTMLButtonElement>(
        '[data-session-tab="' + tabState.activeId + '"]',
      )
      ?.focus();
  }
}

function syncSessionForm(): void {
  zeroWorkspace.textContent = workspacePreference || "the selected workspace";
  if (!sessionFormMode) {
    sessionEditor.hidden = true;
    updateZeroSessionState();
    return;
  }

  sessionEditor.hidden = false;
  updateZeroSessionState();
  if (sessionFormMode.kind === "create") {
    sessionEditorTitle.textContent = "New session";
    sessionDriverSelect.disabled = sessionFormPending;
    sessionWorkingDirectory.value =
      workspacePreference || "No workspace selected";
    sessionWorkingDirectory.title = sessionWorkingDirectory.value;
    browseSessionDirectory.textContent = "Browse…";
    browseSessionDirectory.disabled = sessionFormPending;
    sessionPermissionSelect.disabled = sessionFormPending;
    sessionLabelInput.disabled = sessionFormPending;
    saveSessionButton.textContent = "Create session";
    sessionFormNote.textContent = workspacePreference
      ? "Browse changes the workspace default for this and future new sessions. The session is created stopped."
      : "Choose a workspace before creating the session. Browse saves it as the default for future new sessions.";
  } else {
    const session = snapshotById.get(sessionFormMode.sessionId);
    if (!session) {
      closeSessionForm();
      return;
    }
    const stopped = !session.running;
    sessionEditorTitle.textContent = "Edit " + session.label;
    sessionDriverSelect.value = session.driver;
    sessionDriverSelect.disabled = true;
    sessionWorkingDirectory.value = session.working_dir;
    sessionWorkingDirectory.title = session.working_dir;
    browseSessionDirectory.textContent = "Change…";
    browseSessionDirectory.disabled = sessionFormPending || !stopped;
    sessionPermissionSelect.disabled = sessionFormPending || !stopped;
    sessionLabelInput.disabled = sessionFormPending;
    saveSessionButton.textContent = "Save changes";
    sessionFormNote.textContent = stopped
      ? "Driver identity is fixed. Browse applies the working-directory change immediately; permission changes apply when saved."
      : "Stop this run before changing its working directory or permission profile.";
  }
  saveSessionButton.disabled =
    sessionFormPending
    || (sessionFormMode.kind === "create" && !workspacePreference);
  syncPermissionControls();
}

function syncPermissionControls(): void {
  const driver = sessionDriverSelect.value as DriverKind;
  const requested = sessionPermissionSelect.value as PermissionProfile;
  const normalized = permissionProfileForDriver(driver, requested);
  if (normalized !== requested) {
    sessionPermissionSelect.value = normalized;
  }
  const unsafeOption = sessionPermissionSelect.querySelector<HTMLOptionElement>(
    'option[value="unsafe"]',
  );
  if (unsafeOption) {
    unsafeOption.disabled = !canUseUnsafePermission(driver);
  }
  const warning = unsafePermissionWarning(driver, normalized);
  permissionWarning.hidden = warning === null;
  permissionWarning.textContent = warning ?? "";
}

function setSessionFormError(message: string | null): void {
  sessionFormError.hidden = message === null;
  sessionFormError.textContent = message ?? "";
}

async function submitSessionForm(): Promise<void> {
  if (!sessionFormMode || sessionFormPending) {
    return;
  }
  const mode = sessionFormMode;
  const driver = sessionDriverSelect.value as DriverKind;
  const permissionProfile = permissionProfileForDriver(
    driver,
    sessionPermissionSelect.value as PermissionProfile,
  );
  const label = sessionLabelInput.value.trim();
  if (mode.kind === "edit" && !label) {
    setSessionFormError("A saved session label cannot be empty.");
    return;
  }
  sessionFormPending = true;
  setSessionFormError(null);
  syncSessionForm();

  if (mode.kind === "create") {
    let created: SessionSnapshot;
    try {
      created = await command<SessionSnapshot>("create_session", {
        request: {
          label: label || null,
          driver,
          permission_profile: permissionProfile,
        } satisfies CreateSessionRequest,
      });
    } catch (error) {
      sessionFormPending = false;
      setSessionFormError("Create session failed: " + String(error));
      syncSessionForm();
      return;
    }
    try {
      await refreshSnapshot(created.session_id);
      closeSessionForm();
    } catch (error) {
      sessionFormPending = false;
      setSessionFormError(
        "Session "
          + shortSessionId(created.session_id)
          + " was created, but inventory refresh failed. Do not create a duplicate; use Refresh snapshot. "
          + String(error),
      );
      syncSessionForm();
    }
    return;
  }

  try {
    const current = snapshotById.get(mode.sessionId);
    if (!current) {
      throw new Error("session no longer exists");
    }
    if (label !== current.label) {
      await command<void>("rename_session", {
        request: {
          session_id: current.session_id,
          label,
        } satisfies RenameSessionRequest,
      });
    }
    if (
      !current.running
      && permissionProfile !== current.permission_profile
    ) {
      await command<void>("set_session_permission_profile", {
        request: {
          session_id: current.session_id,
          permission_profile: permissionProfile,
        } satisfies SetSessionPermissionRequest,
      });
    }
    await refreshSnapshot(current.session_id);
    closeSessionForm();
  } catch (error) {
    sessionFormPending = false;
    let refreshed = true;
    try {
      await refreshSnapshot(mode.sessionId);
    } catch {
      refreshed = false;
    }
    setSessionFormError(
      (refreshed
        ? "Session change failed; current runtime state was refreshed. "
        : "Session change may have partially applied and inventory refresh also failed. ")
        + String(error),
    );
    syncSessionForm();
  }
}

async function chooseWorkingDirectory(): Promise<void> {
  if (!sessionFormMode || sessionFormPending) {
    return;
  }
  const mode = sessionFormMode;
  sessionFormPending = true;
  setSessionFormError(null);
  syncSessionForm();

  if (mode.kind === "create") {
    try {
      const snapshot = await command<RuntimeSnapshot | null>(
        "choose_workspace_directory",
      );
      if (snapshot) {
        applySnapshot(snapshot, tabState.activeId ?? undefined);
      }
    } catch (error) {
      setSessionFormError("Workspace selection failed: " + String(error));
    } finally {
      sessionFormPending = false;
      syncSessionForm();
    }
    return;
  }

  const sessionId = mode.sessionId;
  let snapshot: SessionSnapshot | null;
  try {
    snapshot = await command<SessionSnapshot | null>(
      "choose_session_working_directory",
      {
        request: {
          session_id: sessionId,
        } satisfies ChooseSessionWorkingDirectoryRequest,
      },
    );
  } catch (error) {
    setSessionFormError("Directory selection failed: " + String(error));
    sessionFormPending = false;
    syncSessionForm();
    return;
  }
  if (!snapshot) {
    sessionFormPending = false;
    syncSessionForm();
    return;
  }
  applyCommandSessionSnapshot(snapshot);
  try {
      await refreshSnapshot(sessionId);
  } catch (error) {
    setSessionFormError(
      "Working directory changed, but inventory refresh failed: "
        + String(error),
    );
  } finally {
    sessionFormPending = false;
    syncSessionForm();
  }
}

async function moveSession(
  sessionId: string,
  delta: -1 | 1,
): Promise<void> {
  const nextOrder = movedSessionOrder(tabState.order, sessionId, delta);
  const newIndex = nextOrder.indexOf(sessionId);
  if (newIndex === tabState.order.indexOf(sessionId)) {
    return;
  }
  try {
    const snapshot = await command<RuntimeSnapshot>("move_session", {
      request: {
        session_id: sessionId,
        new_index: newIndex,
      } satisfies MoveSessionRequest,
    });
    applySnapshot(snapshot, sessionId);
    sessionTabs
      .querySelector<HTMLButtonElement>(
        '[data-session-tab="' + sessionId + '"]',
      )
      ?.focus();
  } catch (error) {
    writeSystem(
      "error",
      "move " + paneLabel(sessionId) + " failed: " + String(error),
    );
  }
}

async function deleteSession(sessionId: string): Promise<void> {
  const session = snapshotById.get(sessionId);
  if (!session) {
    return;
  }
  if (session.running) {
    writeSystem("warn", "Stop " + session.label + " before closing it.");
    return;
  }
  const confirmed = window.confirm(
    'Close "' + session.label + '" (' + shortSessionId(sessionId) + ")? Its terminal scrollback will be discarded.",
  );
  if (!confirmed) {
    return;
  }
  try {
    await command<void>("delete_session", {
      request: { session_id: sessionId } satisfies DeleteSessionRequest,
    });
    if (
      sessionFormMode?.kind === "edit"
      && sessionFormMode.sessionId === sessionId
    ) {
      closeSessionForm();
    }
  } catch (error) {
    writeSystem(
      "error",
      "close " + session.label + " failed: " + String(error),
    );
    return;
  }
  try {
    await refreshSnapshot();
  } catch (error) {
    writeSystem(
      "error",
      "session closed, but the inventory refresh failed: " + String(error),
    );
  }
}

function wireSessionUi(): void {
  newSessionButton.addEventListener("click", openCreateSessionForm);
  zeroNewSession.addEventListener("click", openCreateSessionForm);
  must<HTMLButtonElement>("#cancel-session-form").addEventListener(
    "click",
    closeSessionForm,
  );
  sessionForm.addEventListener("submit", (event) => {
    event.preventDefault();
    void submitSessionForm();
  });
  browseSessionDirectory.addEventListener("click", () => {
    void chooseWorkingDirectory();
  });
  sessionDriverSelect.addEventListener("change", syncPermissionControls);
  sessionPermissionSelect.addEventListener("change", syncPermissionControls);
  sessionTabs.addEventListener("click", (event) => {
    if (!(event.target instanceof Element)) {
      return;
    }
    const action = event.target.closest<HTMLButtonElement>("[data-tab-action]");
    if (!action) {
      return;
    }
    const sessionId = action.dataset.session;
    if (!sessionId) {
      return;
    }
    switch (action.dataset.tabAction) {
      case "move": {
        const delta = action.dataset.delta === "-1" ? -1 : 1;
        void moveSession(sessionId, delta);
        break;
      }
      case "edit":
        openEditSessionForm(sessionId);
        break;
      case "close":
        void deleteSession(sessionId);
        break;
    }
  });
  document.addEventListener("click", (event) => {
    if (!(event.target instanceof Element)) {
      return;
    }
    const edit = event.target.closest<HTMLButtonElement>("[data-edit-session]");
    if (edit?.dataset.editSession) {
      openEditSessionForm(edit.dataset.editSession);
    }
  });
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
    const sessionId = button.dataset.session;
    if (!action || !sessionId) {
      return;
    }

    if (action === "start" && snapshotById.get(sessionId)?.running) {
      writeSystem("info", `${paneLabel(sessionId)} is already running`);
      return;
    }

    try {
      if (action === "start") {
        const snapshot = await command<SessionSnapshot>("start_session", {
          request: { session_id: sessionId },
        });
        applyCommandSessionSnapshot(snapshot);
      } else if (action === "restart") {
        const snapshot = await command<SessionSnapshot>("restart_session", {
          request: { session_id: sessionId },
        });
        applyCommandSessionSnapshot(snapshot);
      } else if (action === "stop") {
        const snapshot = await command<SessionSnapshot>("stop_session", {
          request: { session_id: sessionId },
        });
        applyCommandSessionSnapshot(snapshot);
      }
    } catch (error) {
      writeSystem("error", `${action} ${paneLabel(sessionId)} failed: ${String(error)}`);
    }
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
    if (isPaneVisible(pane.sessionId)) {
      pane.fit();
    }
  }
}

function isPaneVisible(sessionId: string): boolean {
  return tabState.activeId === sessionId;
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

  window.addEventListener(
    "keydown",
    (event) => {
      const action = resolveGlobalSessionShortcut(shortcutInput(event));
      if (!action) {
        return;
      }
      event.preventDefault();
      if (action.kind === "new-session") {
        openCreateSessionForm();
        return;
      }
      if (action.kind === "close-session") {
        if (tabState.activeId) {
          void deleteSession(tabState.activeId);
        }
        return;
      }
      const targetId = relativeSessionId(
        tabState.order,
        tabState.activeId,
        action.delta,
      );
      if (targetId) {
        setActiveSession(targetId, true);
      }
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
          Array.from(paneMap.entries()).map(([sessionId, pane]) => [
            sessionId,
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

function shortcutInput(event: KeyboardEvent) {
  return {
    key: event.key,
    ctrlKey: event.ctrlKey,
    shiftKey: event.shiftKey,
    altKey: event.altKey,
    metaKey: event.metaKey,
    editable: isEditableShortcutTarget(event.target),
  };
}

function isEditableShortcutTarget(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) {
    return false;
  }
  if (target.closest("[data-terminal]")) {
    return false;
  }
  return Boolean(
    target.closest('input, select, textarea, [contenteditable="true"]'),
  );
}

async function pasteClipboardIntoTerminal(pane: SessionTerminal): Promise<void> {
  if (!snapshotById.get(pane.sessionId)?.running) {
    writeSystem("warn", `${pane.label} is not running; paste skipped`);
    return;
  }

  try {
    const clipboard = await navigator.clipboard.readText();
    if (!clipboard) {
      return;
    }

    await command<SessionSnapshot>("send_input", {
      request: {
        session_id: pane.sessionId,
        input: clipboard,
      } satisfies SendInputRequest,
    });
    writeSystem("info", `${pane.label} pasted ${clipboard.length} chars`);
  } catch (error) {
    writeSystem("error", `paste failed for ${pane.label}: ${String(error)}`);
  }
}

function activeTerminal(): SessionTerminal | null {
  return tabState.activeId ? paneMap.get(tabState.activeId) ?? null : null;
}

function paneLabel(sessionId: string): string {
  return paneMap.get(sessionId)?.label ?? snapshotById.get(sessionId)?.label ?? sessionId;
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

async function resizeSession(sessionId: string, cols: number, rows: number): Promise<void> {
  if (!snapshotById.get(sessionId)?.running) {
    return;
  }

  try {
    await command<void>("resize_session", { sessionId, cols, rows });
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
