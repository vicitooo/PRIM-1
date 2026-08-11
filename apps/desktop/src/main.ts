import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";

import { exposeAutomationBridge } from "./automation-bridge";

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
  type RoomRuntimeEvent,
  type PendingBuffer,
  type RuntimeEventContext,
} from "./runtime-events";
import {
  canUseUnsafePermission,
  createSessionRequestFromForm,
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
import {
  appendRoomFeedEvent as reconcileRoomFeedEvent,
  retainRoomFeedWindow,
} from "./room-feed";
import "./styles.css";
import type {
  AddRoomMemberRequest,
  ChooseSessionWorkingDirectoryRequest,
  CreateRoomRequest,
  DeleteRoomRequest,
  DeleteSessionRequest,
  DeliverRoomMessageRequest,
  DriverKind,
  MoveRoomRequest,
  MoveSessionRequest,
  PermissionProfile,
  PostRoomMessageRequest,
  ReadRoomFeedRequest,
  RemoveRoomMemberRequest,
  RenameRoomRequest,
  RoomDeliveryResult,
  RoomFeedCursor,
  RoomFeedEvent,
  RoomFeedPage,
  RoomSnapshot,
  RenameSessionRequest,
  RuntimeEvent,
  RuntimeSnapshot,
  SendInputRequest,
  SetSessionLinuxWorkingDirectoryRequest,
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
              <option value="prime">Prime Agent (Ubuntu)</option>
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
          <div class="session-path-field" id="session-windows-directory-field">
            <span>Working directory</span>
            <output id="session-working-directory" class="session-path">No workspace selected</output>
            <button type="button" id="browse-session-directory">Browse…</button>
          </div>
          <label class="session-linux-directory" id="session-linux-directory-field" hidden>
            <span>Linux working directory <small>Ubuntu namespace</small></span>
            <input id="session-linux-working-directory" type="text" autocomplete="off" spellcheck="false" placeholder="Ubuntu home when blank" />
          </label>
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

      <article class="room-card panel" id="room-card">
        <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
        <div class="card-head room-card-head">
          <div class="card-title">
            <span class="card-icon icon-room" aria-hidden="true"></span>
            <h2>Room feed</h2>
          </div>
          <div class="room-toolbar">
            <select id="room-select" aria-label="Active room"></select>
            <button type="button" id="new-room">New</button>
            <button type="button" class="ghost" id="rename-room">Rename</button>
            <button type="button" class="ghost" id="move-room-left" aria-label="Move room left">←</button>
            <button type="button" class="ghost" id="move-room-right" aria-label="Move room right">→</button>
            <button type="button" class="ghost danger" id="delete-room">Delete</button>
          </div>
        </div>
        <form id="room-create-form" class="room-create-form" hidden>
          <label><span>Room label <small>optional</small></span><input id="room-label" maxlength="128" autocomplete="off" /></label>
          <fieldset>
            <legend>Select at least two sessions</legend>
            <div id="room-member-choices" class="room-member-choices"></div>
          </fieldset>
          <p id="room-create-error" class="session-form-error" role="alert" hidden></p>
          <div class="room-form-actions">
            <button type="submit" class="primary">Create room</button>
            <button type="button" class="ghost" id="cancel-room-create">Cancel</button>
          </div>
        </form>
        <div id="room-empty" class="room-empty">
          <p>No rooms yet. Create one from existing sessions; no harness will be started or replaced.</p>
        </div>
        <div id="room-content" class="room-content" hidden>
          <div class="room-members-row">
            <div id="room-members" class="room-members" aria-label="Room members"></div>
            <select id="room-add-member-select" aria-label="Session to add"></select>
            <button type="button" id="room-add-member">Add member</button>
          </div>
          <div id="room-feed" class="room-feed" role="log" aria-live="polite"></div>
          <label class="room-composer-label" for="room-message">Message</label>
          <textarea id="room-message" rows="3" maxlength="1048576" placeholder="Post to the shared feed or deliver explicitly…"></textarea>
          <div class="room-composer-actions">
            <button type="button" id="room-post">Post to feed</button>
            <select id="room-recipient" aria-label="Room delivery recipients"></select>
            <button type="button" class="primary" id="room-send">Send</button>
          </div>
          <p id="room-status" class="room-status" aria-live="polite"></p>
        </div>
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
const sessionWindowsDirectoryField = must<HTMLElement>("#session-windows-directory-field");
const sessionLinuxDirectoryField = must<HTMLElement>("#session-linux-directory-field");
const sessionLinuxWorkingDirectory = must<HTMLInputElement>("#session-linux-working-directory");
const browseSessionDirectory = must<HTMLButtonElement>("#browse-session-directory");
const permissionWarning = must<HTMLElement>("#permission-warning");
const sessionFormNote = must<HTMLElement>("#session-form-note");
const sessionFormError = must<HTMLElement>("#session-form-error");
const saveSessionButton = must<HTMLButtonElement>("#save-session");
const zeroSession = must<HTMLElement>("#zero-session");
const zeroWorkspace = must<HTMLElement>("#zero-workspace");
const zeroNewSession = must<HTMLButtonElement>("#zero-new-session");
const roomSelect = must<HTMLSelectElement>("#room-select");
const newRoomButton = must<HTMLButtonElement>("#new-room");
const renameRoomButton = must<HTMLButtonElement>("#rename-room");
const moveRoomLeftButton = must<HTMLButtonElement>("#move-room-left");
const moveRoomRightButton = must<HTMLButtonElement>("#move-room-right");
const deleteRoomButton = must<HTMLButtonElement>("#delete-room");
const roomCreateForm = must<HTMLFormElement>("#room-create-form");
const roomLabelInput = must<HTMLInputElement>("#room-label");
const roomMemberChoices = must<HTMLDivElement>("#room-member-choices");
const roomCreateError = must<HTMLElement>("#room-create-error");
const cancelRoomCreate = must<HTMLButtonElement>("#cancel-room-create");
const roomEmpty = must<HTMLElement>("#room-empty");
const roomContent = must<HTMLElement>("#room-content");
const roomMembers = must<HTMLDivElement>("#room-members");
const roomAddMemberSelect = must<HTMLSelectElement>("#room-add-member-select");
const roomAddMemberButton = must<HTMLButtonElement>("#room-add-member");
const roomFeed = must<HTMLDivElement>("#room-feed");
const roomMessage = must<HTMLTextAreaElement>("#room-message");
const roomPostButton = must<HTMLButtonElement>("#room-post");
const roomRecipient = must<HTMLSelectElement>("#room-recipient");
const roomSendButton = must<HTMLButtonElement>("#room-send");
const roomStatus = must<HTMLElement>("#room-status");
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
let roomSnapshots: RoomSnapshot[] = [];
let activeRoomId: string | null = null;
const roomFeedEvents = new Map<string, RoomFeedEvent[]>();
const roomFeedCursors = new Map<string, RoomFeedCursor>();
const queuedRoomFeedEvents = new Map<string, RoomFeedEvent[]>();
const initializedRoomFeeds = new Set<string>();
const loadingRoomFeeds = new Set<string>();
const roomFeedReloadRequired = new Set<string>();
let sessionFormMode: SessionFormMode | null = null;
let sessionFormPending = false;
let primeDefaultRequest = 0;
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
wireRoomUi();
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
  handleRoomEvent(event) {
    handleRoomRuntimeEvent(event);
  },
};

void initializeUi().catch((error) => {
  writeSystem("error", `UI initialization failed: ${String(error)}`);
});

async function initializeUi(): Promise<void> {
  const automationMode = await command<boolean>("automation_mode");
  exposeAutomationBridge(window, automationMode, {
    core: { invoke },
    event: { listen },
    window: { getCurrentWindow },
  });
  await registerRuntimeEventsBeforeBootstrap(
    () =>
      listen<RuntimeEvent>("runtime://event", ({ payload }) => {
        handleRuntimeEvent(payload, runtimeEventContext);
      }),
    bootstrap,
  );
}

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
  syncRoomInventory(snapshot.rooms);
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

function handleRoomRuntimeEvent(event: RoomRuntimeEvent): void {
  if (event.event === "room_feed_event") {
    acceptLiveRoomFeedEvent(event.feed_event);
    return;
  }
  if (event.schema_version !== 1) {
    writeSystem(
      "warn",
      `unsupported room event schema ${event.schema_version}; refreshing authoritative room state`,
    );
  }
  void refreshSnapshot(tabState.activeId ?? undefined).catch((error) =>
    writeSystem("error", `room inventory refresh failed: ${String(error)}`),
  );
}

function syncRoomInventory(incoming: RoomSnapshot[]): void {
  roomSnapshots = incoming;
  const liveIds = new Set(incoming.map((room) => room.room_id));
  for (const roomId of Array.from(roomFeedEvents.keys())) {
    if (!liveIds.has(roomId)) {
      roomFeedEvents.delete(roomId);
      roomFeedCursors.delete(roomId);
      queuedRoomFeedEvents.delete(roomId);
      initializedRoomFeeds.delete(roomId);
      loadingRoomFeeds.delete(roomId);
      roomFeedReloadRequired.delete(roomId);
    }
  }
  if (!activeRoomId || !liveIds.has(activeRoomId)) {
    activeRoomId = incoming[0]?.room_id ?? null;
  }
  discardInactiveRoomFeedState();
  renderRoomUi();
  if (activeRoomId) {
    void initializeRoomFeed(activeRoomId);
  }
}

function activeRoom(): RoomSnapshot | null {
  return roomSnapshots.find((room) => room.room_id === activeRoomId) ?? null;
}

function discardInactiveRoomFeedState(): void {
  for (const roomId of new Set([
    ...roomFeedEvents.keys(),
    ...roomFeedCursors.keys(),
    ...queuedRoomFeedEvents.keys(),
    ...initializedRoomFeeds,
    ...roomFeedReloadRequired,
  ])) {
    if (roomId === activeRoomId) {
      continue;
    }
    roomFeedEvents.delete(roomId);
    roomFeedCursors.delete(roomId);
    queuedRoomFeedEvents.delete(roomId);
    initializedRoomFeeds.delete(roomId);
    roomFeedReloadRequired.delete(roomId);
  }
}

function acceptLiveRoomFeedEvent(event: RoomFeedEvent): void {
  if (event.schema_version !== 1) {
    writeSystem(
      "warn",
      `unsupported room feed schema ${event.schema_version}; event ignored`,
    );
    return;
  }
  if (event.room_id !== activeRoomId) {
    return;
  }
  if (!initializedRoomFeeds.has(event.room_id)) {
    const queued = queuedRoomFeedEvents.get(event.room_id) ?? [];
    const bounded = retainRoomFeedWindow([...queued, event]);
    if (bounded.length < queued.length + 1) {
      const firstDrop = !roomFeedReloadRequired.has(event.room_id);
      roomFeedReloadRequired.add(event.room_id);
      if (firstDrop) {
        writeSystem(
          "warn",
          `room ${shortSessionId(event.room_id)} bootstrap queue reached its bounded window; reloading the authoritative feed`,
        );
      }
    }
    queuedRoomFeedEvents.set(event.room_id, bounded);
    if (bounded.length === 0) {
      writeSystem(
        "warn",
        `room ${shortSessionId(event.room_id)} emitted an event too large for the renderer feed window`,
      );
    }
    return;
  }
  if (!appendRoomFeedEvent(event)) {
    const queued = queuedRoomFeedEvents.get(event.room_id) ?? [];
    queued.push(event);
    queuedRoomFeedEvents.set(event.room_id, queued);
    initializedRoomFeeds.delete(event.room_id);
    void initializeRoomFeed(event.room_id, true);
  }
}

function appendRoomFeedEvent(event: RoomFeedEvent): boolean {
  const cursor = roomFeedCursors.get(event.room_id);
  const events = roomFeedEvents.get(event.room_id) ?? [];
  const result = reconcileRoomFeedEvent(event.room_id, events, cursor, event);
  if (result.status === "duplicate") {
    return true;
  }
  if (result.status !== "accepted") {
    return false;
  }
  roomFeedEvents.set(event.room_id, result.events);
  roomFeedCursors.set(event.room_id, result.cursor);
  if (activeRoomId === event.room_id) {
    renderRoomFeed(result.events);
  }
  return true;
}

async function initializeRoomFeed(roomId: string, force = false): Promise<void> {
  if ((!force && initializedRoomFeeds.has(roomId)) || loadingRoomFeeds.has(roomId)) {
    return;
  }
  loadingRoomFeeds.add(roomId);
  try {
    let cursor: RoomFeedCursor | null = null;
    const events: RoomFeedEvent[] = [];
    for (;;) {
      const page: RoomFeedPage = await command<RoomFeedPage>("read_room_feed", {
        request: {
          room_id: roomId,
          cursor,
        } satisfies ReadRoomFeedRequest,
      });
      if (page.schema_version !== 1) {
        throw new Error(`unsupported room feed schema ${page.schema_version}`);
      }
      if (page.gap) {
        const range =
          page.gap.from_sequence === null
            ? "a prior feed epoch"
            : `events ${page.gap.from_sequence}–${page.gap.through_sequence}`;
        writeSystem(
          "warn",
          `room ${shortSessionId(roomId)} feed gap: ${range} unavailable (${page.gap.reason})`,
        );
        if (activeRoomId === roomId) {
          roomStatus.textContent = `Feed gap: ${range} unavailable.`;
          roomStatus.dataset.level = "warn";
        }
      }
      events.push(...page.events);
      cursor = page.cursor;
      if (!page.has_more) {
        break;
      }
    }
    if (activeRoomId !== roomId) {
      return;
    }
    roomFeedEvents.set(roomId, retainRoomFeedWindow(events));
    if (cursor) {
      roomFeedCursors.set(roomId, cursor);
    }
    initializedRoomFeeds.add(roomId);
    const queued = (queuedRoomFeedEvents.get(roomId) ?? []).sort(
      (left, right) => left.cursor.sequence - right.cursor.sequence,
    );
    queuedRoomFeedEvents.delete(roomId);
    let needsReload = roomFeedReloadRequired.delete(roomId);
    for (const event of queued) {
      if (!appendRoomFeedEvent(event)) {
        needsReload = true;
        break;
      }
    }
    if (activeRoomId === roomId) {
      renderRoomFeed(roomFeedEvents.get(roomId) ?? []);
    }
    if (needsReload) {
      initializedRoomFeeds.delete(roomId);
      queueMicrotask(() => void initializeRoomFeed(roomId, true));
    }
  } catch (error) {
    if (activeRoomId === roomId) {
      roomStatus.textContent = `Room feed unavailable: ${String(error)}`;
      roomStatus.dataset.level = "error";
    }
    writeSystem("error", `room feed read failed: ${String(error)}`);
  } finally {
    loadingRoomFeeds.delete(roomId);
  }
}

function renderRoomUi(): void {
  const selected = activeRoom();
  roomSelect.replaceChildren();
  if (roomSnapshots.length === 0) {
    const option = document.createElement("option");
    option.textContent = "No rooms";
    option.value = "";
    roomSelect.append(option);
  } else {
    for (const room of roomSnapshots) {
      const option = document.createElement("option");
      option.value = room.room_id;
      option.textContent = `${room.label} · ${shortSessionId(room.room_id)}`;
      roomSelect.append(option);
    }
    roomSelect.value = selected?.room_id ?? roomSnapshots[0].room_id;
  }
  roomEmpty.hidden = selected !== null || !roomCreateForm.hidden;
  roomContent.hidden = selected === null || !roomCreateForm.hidden;
  const roomIndex = selected
    ? roomSnapshots.findIndex((room) => room.room_id === selected.room_id)
    : -1;
  roomSelect.disabled = roomSnapshots.length === 0;
  renameRoomButton.disabled = selected === null;
  deleteRoomButton.disabled = selected === null;
  moveRoomLeftButton.disabled = roomIndex <= 0;
  moveRoomRightButton.disabled = roomIndex < 0 || roomIndex >= roomSnapshots.length - 1;
  if (!selected) {
    roomMembers.replaceChildren();
    roomFeed.replaceChildren();
    return;
  }
  renderRoomMembers(selected);
  renderRoomRecipientOptions(selected);
  renderRoomFeed(roomFeedEvents.get(selected.room_id) ?? []);
  roomSendButton.disabled = selected.member_ids.length < 2;
}

function renderRoomMembers(room: RoomSnapshot): void {
  roomMembers.replaceChildren();
  for (const sessionId of room.member_ids) {
    const chip = document.createElement("span");
    chip.className = "room-member-chip";
    const label = document.createElement("span");
    label.textContent = `${paneLabel(sessionId)} · ${shortSessionId(sessionId)}`;
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "room-member-remove";
    remove.textContent = "×";
    remove.title = `Remove ${paneLabel(sessionId)} from ${room.label}`;
    remove.addEventListener("click", () => void removeRoomMember(room.room_id, sessionId));
    chip.append(label, remove);
    roomMembers.append(chip);
  }

  const occupied = new Set(roomSnapshots.flatMap((candidate) => candidate.member_ids));
  const eligible = Array.from(snapshotById.values()).filter(
    (session) => !occupied.has(session.session_id),
  );
  roomAddMemberSelect.replaceChildren();
  const placeholder = document.createElement("option");
  placeholder.value = "";
  placeholder.textContent = eligible.length === 0 ? "No eligible sessions" : "Add session…";
  roomAddMemberSelect.append(placeholder);
  for (const session of eligible) {
    const option = document.createElement("option");
    option.value = session.session_id;
    option.textContent = `${session.label} · ${shortSessionId(session.session_id)}`;
    roomAddMemberSelect.append(option);
  }
  roomAddMemberButton.disabled = eligible.length === 0;
}

function renderRoomRecipientOptions(room: RoomSnapshot): void {
  const previous = roomRecipient.value;
  roomRecipient.replaceChildren();
  const all = document.createElement("option");
  all.value = "all";
  all.textContent = `Send to all ${room.member_ids.length} members`;
  roomRecipient.append(all);
  for (const sessionId of room.member_ids) {
    const option = document.createElement("option");
    option.value = sessionId;
    option.textContent = `Send to ${paneLabel(sessionId)}`;
    roomRecipient.append(option);
  }
  if (Array.from(roomRecipient.options).some((option) => option.value === previous)) {
    roomRecipient.value = previous;
  }
}

function renderRoomFeed(events: RoomFeedEvent[]): void {
  roomFeed.replaceChildren();
  if (events.length === 0) {
    const empty = document.createElement("p");
    empty.className = "room-feed-empty";
    empty.textContent = "No room traffic yet. Posts appear here without prompting any harness.";
    roomFeed.append(empty);
    return;
  }
  if (events[0].cursor.sequence > 1) {
    const bounded = document.createElement("p");
    bounded.className = "room-feed-empty";
    bounded.textContent = `Earlier room traffic is outside the bounded live window (first visible #${events[0].cursor.sequence}).`;
    roomFeed.append(bounded);
  }
  for (const event of events) {
    roomFeed.append(roomFeedEntry(event));
  }
  roomFeed.scrollTop = roomFeed.scrollHeight;
}

function roomFeedEntry(event: RoomFeedEvent): HTMLElement {
  const entry = document.createElement("article");
  entry.className = `room-feed-entry room-feed-${event.item.kind}`;
  entry.dataset.sequence = String(event.cursor.sequence);
  const meta = document.createElement("div");
  meta.className = "room-feed-meta";
  const sequence = `#${event.cursor.sequence}`;
  if (event.item.kind === "message") {
    const sender =
      event.item.sender.kind === "operator"
        ? "Operator"
        : paneLabel(event.item.sender.session_id);
    const recipients = event.item.recipient_ids.length === 0
      ? "feed only"
      : event.item.recipient_ids.map(paneLabel).join(", ");
    meta.textContent = `${sequence} ${sender} · ${recipients} · ${event.item.message_id.slice(0, 8)}`;
    const body = document.createElement("pre");
    body.textContent = event.item.content;
    entry.append(meta, body);
  } else if (event.item.kind === "membership") {
    meta.textContent = `${sequence} ${paneLabel(event.item.session_id)} ${event.item.action} · revision ${event.item.membership_revision}`;
    entry.append(meta);
  } else {
    const partial = event.item.bytes_written > 0 ? ` · ${event.item.bytes_written} bytes` : "";
    meta.textContent = `${sequence} ${paneLabel(event.item.recipient_id)} · ${event.item.status}${partial}`;
    entry.append(meta);
    if (event.item.error) {
      const error = document.createElement("p");
      error.className = "room-feed-error";
      error.textContent = event.item.error;
      entry.append(error);
    }
  }
  return entry;
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
  primeDefaultRequest += 1;
  const defaults = newSessionFormDefaults(workspacePreference);
  sessionFormMode = { kind: "create" };
  sessionFormPending = false;
  sessionLabelInput.value = "";
  sessionDriverSelect.value = defaults.driver;
  sessionPermissionSelect.value = defaults.permissionProfile;
  sessionLinuxWorkingDirectory.value = "";
  setSessionFormError(null);
  syncSessionForm();
  requestAnimationFrame(() => sessionLabelInput.focus());
}

function openEditSessionForm(sessionId: string): void {
  const session = snapshotById.get(sessionId);
  if (!session) {
    return;
  }
  primeDefaultRequest += 1;
  sessionFormMode = { kind: "edit", sessionId };
  sessionFormPending = false;
  sessionLabelInput.value = session.label;
  sessionDriverSelect.value = session.driver;
  sessionPermissionSelect.value = session.permission_profile;
  sessionLinuxWorkingDirectory.value =
    session.driver === "prime" ? session.working_dir : "";
  setSessionFormError(null);
  syncSessionForm();
  requestAnimationFrame(() => sessionLabelInput.focus());
}

function closeSessionForm(): void {
  primeDefaultRequest += 1;
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

async function selectSessionDriver(): Promise<void> {
  syncSessionForm();
  if (
    sessionFormMode?.kind !== "create"
    || sessionDriverSelect.value !== "prime"
    || sessionLinuxWorkingDirectory.value
  ) {
    return;
  }

  const request = ++primeDefaultRequest;
  sessionFormPending = true;
  setSessionFormError(null);
  syncSessionForm();
  try {
    const qualified = await command<string>("prime_default_working_directory");
    if (
      request === primeDefaultRequest
      && sessionFormMode?.kind === "create"
      && sessionDriverSelect.value === "prime"
    ) {
      sessionLinuxWorkingDirectory.value = qualified;
      sessionLinuxWorkingDirectory.title = qualified;
    }
  } catch (error) {
    if (request === primeDefaultRequest && sessionFormMode?.kind === "create") {
      setSessionFormError(
        "Prime working-directory discovery failed; enter a qualified absolute Ubuntu path or retry. "
          + String(error),
      );
    }
  } finally {
    if (request === primeDefaultRequest && sessionFormMode?.kind === "create") {
      sessionFormPending = false;
      syncSessionForm();
    }
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
  const prime = sessionDriverSelect.value === "prime";
  sessionWindowsDirectoryField.hidden = prime;
  sessionLinuxDirectoryField.hidden = !prime;
  if (sessionFormMode.kind === "create") {
    sessionEditorTitle.textContent = "New session";
    sessionDriverSelect.disabled = sessionFormPending;
    sessionWorkingDirectory.value =
      workspacePreference || "No workspace selected";
    sessionWorkingDirectory.title = sessionWorkingDirectory.value;
    browseSessionDirectory.textContent = "Browse…";
    browseSessionDirectory.disabled = sessionFormPending || prime;
    sessionLinuxWorkingDirectory.disabled = sessionFormPending || !prime;
    sessionPermissionSelect.disabled = sessionFormPending;
    sessionLabelInput.disabled = sessionFormPending;
    saveSessionButton.textContent = "Create session";
    sessionFormNote.textContent = prime
      ? "Prime runs directly in Ubuntu WSL. Enter an absolute Linux path, or leave blank to use the qualified Ubuntu home. The session is created stopped."
      : workspacePreference
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
    browseSessionDirectory.disabled = sessionFormPending || !stopped || prime;
    sessionLinuxWorkingDirectory.disabled = sessionFormPending || !stopped || !prime;
    sessionPermissionSelect.disabled = sessionFormPending || !stopped;
    sessionLabelInput.disabled = sessionFormPending;
    saveSessionButton.textContent = "Save changes";
    sessionFormNote.textContent = stopped
      ? prime
        ? "Driver identity is fixed. Saving requalifies the exact Ubuntu path and applies any label change."
        : "Driver identity is fixed. Browse applies the working-directory change immediately; permission changes apply when saved."
      : "Stop this run before changing its working directory or permission profile.";
  }
  saveSessionButton.disabled =
    sessionFormPending
    || (sessionFormMode.kind === "create" && !prime && !workspacePreference);
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
  const linuxWorkingDirectory = sessionLinuxWorkingDirectory.value;
  if (mode.kind === "edit" && !label) {
    setSessionFormError("A saved session label cannot be empty.");
    return;
  }
  if (mode.kind === "edit" && driver === "prime" && !linuxWorkingDirectory.trim()) {
    setSessionFormError("Prime requires an absolute Ubuntu working directory.");
    return;
  }
  sessionFormPending = true;
  setSessionFormError(null);
  syncSessionForm();

  if (mode.kind === "create") {
    let created: SessionSnapshot;
    try {
      created = await command<SessionSnapshot>("create_session", {
        request: createSessionRequestFromForm(
          label,
          driver,
          permissionProfile,
          linuxWorkingDirectory,
        ),
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
    if (
      !current.running
      && current.driver === "prime"
      && linuxWorkingDirectory !== current.working_dir
    ) {
      await command<SessionSnapshot>("set_session_linux_working_directory", {
        request: {
          session_id: current.session_id,
          linux_working_directory: linuxWorkingDirectory,
        } satisfies SetSessionLinuxWorkingDirectoryRequest,
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
  if (sessionDriverSelect.value === "prime") {
    setSessionFormError("Prime uses an absolute path inside Ubuntu; enter it in the Linux field.");
    return;
  }
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

function openRoomCreateForm(): void {
  roomCreateForm.hidden = false;
  roomLabelInput.value = "";
  roomCreateError.hidden = true;
  roomCreateError.textContent = "";
  roomMemberChoices.replaceChildren();
  for (const session of snapshotById.values()) {
    const occupied = roomSnapshots.some((room) => room.member_ids.includes(session.session_id));
    const label = document.createElement("label");
    label.className = "room-member-choice";
    const checkbox = document.createElement("input");
    checkbox.type = "checkbox";
    checkbox.value = session.session_id;
    checkbox.disabled = occupied;
    const text = document.createElement("span");
    text.textContent = `${session.label} · ${driverLabel(session.driver)} · ${shortSessionId(session.session_id)}${occupied ? " · already in a room" : ""}`;
    label.append(checkbox, text);
    roomMemberChoices.append(label);
  }
  renderRoomUi();
  roomLabelInput.focus();
}

function closeRoomCreateForm(): void {
  roomCreateForm.hidden = true;
  roomCreateError.hidden = true;
  renderRoomUi();
}

async function createRoomFromForm(): Promise<void> {
  const memberIds = Array.from(
    roomMemberChoices.querySelectorAll<HTMLInputElement>('input[type="checkbox"]:checked'),
  ).map((checkbox) => checkbox.value);
  if (memberIds.length < 2) {
    roomCreateError.textContent = "Select at least two sessions.";
    roomCreateError.hidden = false;
    return;
  }
  const label = roomLabelInput.value.trim();
  try {
    const room = await command<RoomSnapshot>("create_room", {
      request: {
        label: label || null,
        member_ids: memberIds,
      } satisfies CreateRoomRequest,
    });
    activeRoomId = room.room_id;
    closeRoomCreateForm();
    await refreshSnapshot(tabState.activeId ?? undefined);
  } catch (error) {
    roomCreateError.textContent = `Create room failed: ${String(error)}`;
    roomCreateError.hidden = false;
  }
}

async function renameActiveRoom(): Promise<void> {
  const room = activeRoom();
  if (!room) {
    return;
  }
  const label = window.prompt("Room label", room.label);
  if (label === null || label === room.label) {
    return;
  }
  try {
    await command<RoomSnapshot>("rename_room", {
      request: { room_id: room.room_id, label } satisfies RenameRoomRequest,
    });
    await refreshSnapshot(tabState.activeId ?? undefined);
  } catch (error) {
    setRoomStatus(`Rename failed: ${String(error)}`, "error");
  }
}

async function moveActiveRoom(delta: -1 | 1): Promise<void> {
  const room = activeRoom();
  if (!room) {
    return;
  }
  const index = roomSnapshots.findIndex((candidate) => candidate.room_id === room.room_id);
  const newIndex = index + delta;
  if (newIndex < 0 || newIndex >= roomSnapshots.length) {
    return;
  }
  try {
    const snapshot = await command<RuntimeSnapshot>("move_room", {
      request: { room_id: room.room_id, new_index: newIndex } satisfies MoveRoomRequest,
    });
    applySnapshot(snapshot, tabState.activeId ?? undefined);
  } catch (error) {
    setRoomStatus(`Move failed: ${String(error)}`, "error");
  }
}

async function deleteActiveRoom(): Promise<void> {
  const room = activeRoom();
  if (!room) {
    return;
  }
  if (!window.confirm(`Delete room "${room.label}"? Its bounded in-memory feed will be discarded.`)) {
    return;
  }
  try {
    await command<void>("delete_room", {
      request: { room_id: room.room_id } satisfies DeleteRoomRequest,
    });
    await refreshSnapshot(tabState.activeId ?? undefined);
  } catch (error) {
    setRoomStatus(`Delete failed: ${String(error)}`, "error");
  }
}

async function addSelectedRoomMember(): Promise<void> {
  const room = activeRoom();
  const sessionId = roomAddMemberSelect.value;
  if (!room || !sessionId) {
    return;
  }
  try {
    await command<RoomSnapshot>("add_room_member", {
      request: {
        room_id: room.room_id,
        session_id: sessionId,
      } satisfies AddRoomMemberRequest,
    });
    await refreshSnapshot(tabState.activeId ?? undefined);
  } catch (error) {
    setRoomStatus(`Add member failed: ${String(error)}`, "error");
  }
}

async function removeRoomMember(roomId: string, sessionId: string): Promise<void> {
  try {
    await command<RoomSnapshot>("remove_room_member", {
      request: { room_id: roomId, session_id: sessionId } satisfies RemoveRoomMemberRequest,
    });
    await refreshSnapshot(tabState.activeId ?? undefined);
  } catch (error) {
    setRoomStatus(`Remove member failed: ${String(error)}`, "error");
  }
}

async function postActiveRoomMessage(): Promise<void> {
  const room = activeRoom();
  const content = roomMessage.value;
  if (!room || !content.trim()) {
    setRoomStatus("Enter a message before posting.", "warn");
    return;
  }
  roomPostButton.disabled = true;
  roomSendButton.disabled = true;
  try {
    await command("post_room_message", {
      request: { room_id: room.room_id, content } satisfies PostRoomMessageRequest,
    });
    if (activeRoomId === room.room_id) {
      if (roomMessage.value === content) {
        roomMessage.value = "";
      }
      setRoomStatus("Posted to the room feed; no harness was prompted.", "info");
    } else {
      writeSystem("info", `Posted to room ${room.label}; no harness was prompted.`);
    }
  } catch (error) {
    if (activeRoomId === room.room_id) {
      setRoomStatus(`Post failed: ${String(error)}`, "error");
    } else {
      writeSystem("error", `Post to room ${room.label} failed: ${String(error)}`);
    }
  } finally {
    roomPostButton.disabled = false;
    roomSendButton.disabled = (activeRoom()?.member_ids.length ?? 0) < 2;
  }
}

async function deliverActiveRoomMessage(): Promise<void> {
  const room = activeRoom();
  const content = roomMessage.value;
  if (!room || !content.trim()) {
    setRoomStatus("Enter a message before sending.", "warn");
    return;
  }
  if (room.member_ids.length < 2) {
    setRoomStatus("Room delivery requires at least two members.", "warn");
    return;
  }
  const recipients = roomRecipient.value === "all"
    ? ({ kind: "all" } as const)
    : ({ kind: "one", session_id: roomRecipient.value } as const);
  roomPostButton.disabled = true;
  roomSendButton.disabled = true;
  try {
    const result = await command<RoomDeliveryResult>("deliver_room_message", {
      request: {
        room_id: room.room_id,
        recipients,
        content,
      } satisfies DeliverRoomMessageRequest,
    });
    const resultMessage = result.failures.length === 0
      ? `PTY write completed for ${result.written_count}/${result.recipient_count}; model receipt remains unconfirmed.`
      : `${result.written_count}/${result.recipient_count} PTY writes completed; ${result.failures.length} failed. See feed details.`;
    if (activeRoomId === room.room_id) {
      if (roomMessage.value === content) {
        roomMessage.value = "";
      }
      setRoomStatus(resultMessage, result.failures.length === 0 ? "info" : "error");
    } else {
      writeSystem(
        result.failures.length === 0 ? "info" : "error",
        `Room ${room.label}: ${resultMessage}`,
      );
    }
  } catch (error) {
    if (activeRoomId === room.room_id) {
      setRoomStatus(`Send failed before delivery: ${String(error)}`, "error");
    } else {
      writeSystem(
        "error",
        `Send to room ${room.label} failed before delivery: ${String(error)}`,
      );
    }
  } finally {
    roomPostButton.disabled = false;
    roomSendButton.disabled = (activeRoom()?.member_ids.length ?? 0) < 2;
  }
}

function setRoomStatus(message: string, level: "info" | "warn" | "error"): void {
  roomStatus.textContent = message;
  roomStatus.dataset.level = level;
}

function wireRoomUi(): void {
  newRoomButton.addEventListener("click", openRoomCreateForm);
  cancelRoomCreate.addEventListener("click", closeRoomCreateForm);
  roomCreateForm.addEventListener("submit", (event) => {
    event.preventDefault();
    void createRoomFromForm();
  });
  roomSelect.addEventListener("change", () => {
    activeRoomId = roomSelect.value || null;
    discardInactiveRoomFeedState();
    roomStatus.textContent = "";
    roomStatus.dataset.level = "info";
    renderRoomUi();
    if (activeRoomId) {
      void initializeRoomFeed(activeRoomId);
    }
  });
  renameRoomButton.addEventListener("click", () => void renameActiveRoom());
  moveRoomLeftButton.addEventListener("click", () => void moveActiveRoom(-1));
  moveRoomRightButton.addEventListener("click", () => void moveActiveRoom(1));
  deleteRoomButton.addEventListener("click", () => void deleteActiveRoom());
  roomAddMemberButton.addEventListener("click", () => void addSelectedRoomMember());
  roomPostButton.addEventListener("click", () => void postActiveRoomMessage());
  roomSendButton.addEventListener("click", () => void deliverActiveRoomMessage());
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
  sessionDriverSelect.addEventListener("change", () => {
    void selectSessionDriver();
  });
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
