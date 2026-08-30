import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LogicalSize } from "@tauri-apps/api/dpi";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import { InitialTerminalBanner } from "./terminal-banner";
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
import {
  contextDisplayLabel,
  contextKey,
  parseWorkContext,
  sessionInContext as sessionInContextPure,
  sessionReadiness,
  sessionRoomId as sessionRoomIdPure,
  visibleSessionOrder,
  type WorkContext,
} from "./work-context";
import "./styles.css";
import { SgrColourFilter } from "./sgr-filter";
import type {
  AddRoomMemberRequest,
  BriefRoomMemberRequest,
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
  StartSessionRequest,
} from "./types";

type SessionFormMode =
  | { kind: "create" }
  | { kind: "edit"; sessionId: string };

const app = document.querySelector("#app");
if (!(app instanceof HTMLDivElement)) {
  throw new Error("Missing #app root element");
}

app.innerHTML = `
  <div class="app-shell" data-view="sessions" data-log="closed">
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
    <!-- The bar IS the window title bar (decorations off): drag region, tabs, window controls. -->
    <header class="topbar panel" data-tauri-drag-region>
      <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
      <span class="hud-antenna" aria-hidden="true"></span>
      <div class="topbar-brand" data-tauri-drag-region>
        <div class="brand-logo-frame" aria-hidden="true" data-tauri-drag-region>
          <img class="brand-logo" src="/textures/prim1-logo.png" alt="" data-tauri-drag-region />
          <svg class="brand-logo-bracket" viewBox="0 0 60 60" aria-hidden="true" data-tauri-drag-region>
            <path d="M 0 14 L 0 0 L 14 0" style="stroke: var(--bronze)" stroke-width="2" fill="none" stroke-linecap="square" />
            <path d="M 46 0 L 60 0 L 60 14" style="stroke: var(--bronze)" stroke-width="2" fill="none" stroke-linecap="square" />
            <path d="M 60 46 L 60 60 L 46 60" style="stroke: var(--bronze)" stroke-width="2" fill="none" stroke-linecap="square" />
            <path d="M 14 60 L 0 60 L 0 46" style="stroke: var(--bronze)" stroke-width="2" fill="none" stroke-linecap="square" />
            <path d="M 0 22 L 0 38" style="stroke: var(--copper-hot)" stroke-width="1.5" fill="none" opacity="0.7" />
            <path d="M 60 22 L 60 38" style="stroke: var(--copper-hot)" stroke-width="1.5" fill="none" opacity="0.7" />
          </svg>
        </div>
        <h1 data-tauri-drag-region>PRIM-1</h1>
        <span class="topbar-active-tag" id="brand-active-tag" data-tauri-drag-region>Sessions</span>
      </div>
      <nav class="session-tabs-shell" aria-label="Terminal sessions">
        <button type="button" class="context-chip" id="context-rooms-chip" title="All rooms — pick a room or the lobby">&#8962; Rooms</button>
        <button type="button" class="context-chip context-chip-name" id="context-name-chip"></button>
        <div class="session-tabs" id="session-tabs" role="tablist" aria-label="Sessions" data-tauri-drag-region></div>
        <button type="button" class="new-session-button" id="new-session-button" aria-haspopup="menu" aria-expanded="false" aria-label="Attach a harness" title="Attach a harness — Ctrl+Shift+T">+</button>
      </nav>
      <div class="topbar-status" data-tauri-drag-region>
        <span class="state-pill" data-session-state="global" hidden>ready</span>
        <span class="activity-pill" hidden>idle</span>
      </div>
      <span class="mono" id="control-endpoint" hidden>starting...</span>
      <span class="mono" id="audit-path" hidden>loading...</span>
      <div class="window-controls" role="group" aria-label="Window">
        <button type="button" class="window-control" data-window="minimize" aria-label="Minimize" title="Minimize">
          <svg viewBox="0 0 10 10" aria-hidden="true"><path d="M0 5h10" /></svg>
        </button>
        <button type="button" class="window-control" data-window="maximize" aria-label="Maximize" title="Maximize">
          <svg class="glyph-max" viewBox="0 0 10 10" aria-hidden="true"><rect x="0.5" y="0.5" width="9" height="9" /></svg>
          <svg class="glyph-restore" viewBox="0 0 10 10" aria-hidden="true"><path d="M2.5 2.5V0.5h7v7h-2" /><rect x="0.5" y="2.5" width="7" height="7" /></svg>
        </button>
        <button type="button" class="window-control window-control-close" data-window="close" aria-label="Close" title="Close">
          <svg viewBox="0 0 10 10" aria-hidden="true"><path d="M0.5 0.5l9 9M9.5 0.5l-9 9" /></svg>
        </button>
      </div>
    </header>

    <section class="workspace-shell">
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
          <p class="session-form-note session-profile-note" id="permission-note"></p>
          <p class="session-form-note" id="session-form-note"></p>
          <p class="session-form-error" id="session-form-error" role="alert" hidden></p>
          <div class="session-form-actions">
            <button type="submit" class="primary" id="save-session">Create session</button>
          </div>
        </form>
      </section>

      <section class="zero-session panel" id="zero-session" hidden>
        <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
        <p class="card-kicker" id="zero-kicker">No sessions</p>
        <h2>Attach a harness</h2>
        <p>Starts in <span id="zero-workspace">the selected workspace</span>.</p>
        <div class="zero-attach-choices" id="zero-attach-choices"></div>
        <button type="button" class="ghost" id="zero-new-session" data-attach="custom">Custom…</button>
      </section>

      <section class="workspace-grid" id="workspace-grid" aria-live="polite"></section>
    </section>

    <!-- Rooms view: the overview (one card per room + the lobby; click to enter)
         or the active room's panel (feed, members, composer — today's in-room UI).
         The workspace top bar shows only the entered context's sessions. -->
    <section class="rooms-view" id="rooms-view" hidden aria-label="Rooms">
      <article class="rooms-overview panel" id="rooms-overview">
        <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
        <div class="card-head">
          <div class="card-title">
            <span class="card-icon icon-room" aria-hidden="true"></span>
            <h2>Rooms</h2>
          </div>
        </div>
        <div id="rooms-overview-grid" class="rooms-overview-grid"></div>
      </article>
      <article class="room-card panel" id="room-card" hidden>
        <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
        <div class="card-head room-card-head">
          <div class="card-title">
            <span class="card-icon icon-room" aria-hidden="true"></span>
            <button type="button" class="rooms-back" id="rooms-back" title="Back to all rooms">&larr; All rooms</button>
            <h2 id="room-panel-title">Rooms</h2>
          </div>
          <div class="room-toolbar">
            <button type="button" id="new-room">New</button>
            <button type="button" class="ghost" id="rename-room">Rename</button>
            <button type="button" class="ghost" id="move-room-left" aria-label="Move room left" hidden>←</button>
            <button type="button" class="ghost" id="move-room-right" aria-label="Move room right" hidden>→</button>
            <button type="button" class="ghost danger" id="delete-room">Delete</button>
          </div>
        </div>
        <form id="room-create-form" class="room-create-form" hidden>
          <label><span>Room label <small>optional</small></span><input id="room-label" maxlength="128" autocomplete="off" /></label>
          <fieldset>
            <legend>Sessions <small>optional — an empty room is fine; add members any time</small></legend>
            <div id="room-member-choices" class="room-member-choices"></div>
          </fieldset>
          <label class="room-brief-auto" for="room-brief-auto">
            <input type="checkbox" id="room-brief-auto" checked />
            <span>Brief members automatically — on create, on join, and when a member's run first idles, the room brief below is typed into their terminal. Off, nobody is messaged.</span>
          </label>
          <label class="room-brief-edit">
            <span>Room brief</span>
            <textarea id="room-brief-template" rows="8" maxlength="8192" spellcheck="false"></textarea>
            <small>{room_label}, {members}, {your_label} and {tools} are filled in per member. Clear the box to use the standard brief.</small>
          </label>
          <p id="room-create-error" class="session-form-error" role="alert" hidden></p>
          <div class="room-form-actions">
            <button type="submit" class="primary">Create room</button>
            <button type="button" class="ghost" id="cancel-room-create">Cancel</button>
          </div>
        </form>
        <div id="room-empty" class="room-empty">
          <p>No rooms yet. Create one — empty or from existing sessions; no harness will be started or replaced.</p>
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

    <!-- Settings view (corner menu → Settings). Local preferences + runtime facts. -->
    <section class="settings-view" id="settings-view" hidden aria-label="Settings">
      <article class="panel view-card">
        <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
        <div class="card-head view-head">
          <div class="card-title">
            <h2>Settings</h2>
          </div>
          <button type="button" class="ghost view-back" data-corner="settings">Back to sessions</button>
        </div>
        <div class="view-body">
          <section class="view-section">
            <h3>Quick attach menu</h3>
            <p class="view-hint">Which harnesses the <kbd>+</kbd> menu and the empty workspace offer, and in what order. Custom… is always there.</p>
            <ul class="settings-list" id="quick-attach-settings"></ul>
          </section>
          <section class="view-section">
            <h3>Startup</h3>
            <label class="theme-option" for="auto-resume-toggle">
              <input type="checkbox" id="auto-resume-toggle" />
              <span class="theme-option-text">
                <span class="theme-option-name">Continue where I left off</span>
                <span class="theme-option-hint">On open, relaunch the sessions of the room (or lobby) you were last in — each harness resumes its previous conversation. Another room's sessions resume when you first enter that room. Launch always resumes when a conversation is stored; right-click a tab for Start fresh session.</span>
              </span>
            </label>
          </section>
          <section class="view-section">
            <h3>Runtime</h3>
            <dl class="settings-facts">
              <dt>Workspace</dt><dd class="mono" id="settings-workspace">—</dd>
              <dt>Runtime dir</dt><dd class="mono" id="settings-runtime-dir">—</dd>
              <dt>Audit log</dt><dd class="mono" id="settings-audit-path">—</dd>
              <dt>Control endpoint</dt><dd class="mono" id="settings-control-endpoint">—</dd>
            </dl>
          </section>
        </div>
      </article>
    </section>

    <!-- Themes view (corner menu → Themes): one card per theme with a real screenshot. -->
    <section class="themes-view" id="themes-view" hidden aria-label="Themes">
      <article class="panel view-card">
        <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
        <div class="card-head view-head">
          <div class="card-title">
            <h2>Themes</h2>
          </div>
          <button type="button" class="ghost view-back" data-corner="themes">Back to sessions</button>
        </div>
        <div class="view-body">
          <p class="view-hint">Pick the look. It applies immediately and is remembered. Dark and Light show harness output the way a normal terminal does; the two PRIM-1 themes are the original HUD look.</p>
          <div class="theme-gallery" id="theme-gallery"></div>
          <label class="theme-option" for="unicolor-toggle">
            <input type="checkbox" id="unicolor-toggle" />
            <span class="theme-option-text">
              <span class="theme-option-name">Unicolor</span>
              <span class="theme-option-hint">Render every harness in the theme's text colour: the harness's own colours are dropped; bold, italic and underline stay. Off, a harness paints with its own colours as in a normal terminal. Applies to new output.</span>
            </span>
          </label>
        </div>
      </article>
    </section>

    <!-- Help view (corner menu → Help, F1). Every gesture and key the UI has. -->
    <section class="help-view" id="help-view" hidden aria-label="Help">
      <article class="panel view-card">
        <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
        <div class="card-head view-head">
          <div class="card-title">
            <h2>Help</h2>
          </div>
          <button type="button" class="ghost view-back" data-corner="help">Back to sessions</button>
        </div>
        <div class="view-body">
          <section class="view-section">
            <h3>Sessions</h3>
            <dl class="help-list">
              <dt><kbd>+</kbd></dt><dd>Pick a harness — the session is created and launched in one step. <b>Custom…</b> opens the full form (label, permission profile, working directory). Settings chooses which harnesses are listed.</dd>
              <dt>Tab</dt><dd>Click to switch. <b>Drag</b> a tab to reorder. <b>Right-click</b> for Move left / Move right / Edit… / Close.</dd>
              <dt>×</dt><dd>Closes the session the way you would by hand: leaves its room, stops the harness, deletes it — one confirmation. Terminal scrollback is discarded.</dd>
              <dt>Launch</dt><dd>Resumes the session's previous conversation when one is stored (Claude, Codex, Grok). Right-click the tab → <b>Start fresh session</b> for a clean one.</dd>
              <dt>Pane strip</dt><dd>Launch / Restart / Stop / Edit for the open session; the dot in the tab is its state (copper = ready or idle, bronze = busy or starting, red = stalled or failed).</dd>
              <dt>Normal / Unsafe</dt><dd>What the profile actually passes to the harness — Claude Code: <code>--permission-mode manual</code> vs <code>--dangerously-skip-permissions</code>. Codex: <code>--ask-for-approval on-request --sandbox workspace-write</code> vs <code>--dangerously-bypass-approvals-and-sandbox</code>. Grok Build: <code>--permission-mode default</code> vs <code>--permission-mode bypassPermissions</code>. Prime and Terminal: Normal only.</dd>
            </dl>
          </section>
          <section class="view-section">
            <h3>Keyboard</h3>
            <dl class="help-list">
              <dt><kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>T</kbd></dt><dd>Attach a harness (opens the + menu)</dd>
              <dt><kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>W</kbd></dt><dd>Close the active session</dd>
              <dt><kbd>Ctrl</kbd>+<kbd>Tab</kbd> / <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Tab</kbd></dt><dd>Next / previous tab</dd>
              <dt><kbd>←</kbd> <kbd>→</kbd> <kbd>Home</kbd> <kbd>End</kbd></dt><dd>With a tab focused: select a tab</dd>
              <dt><kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>←</kbd> / <kbd>→</kbd></dt><dd>With a tab focused: move it</dd>
              <dt><kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> / <kbd>V</kbd></dt><dd>Copy the selection / paste into the focused terminal</dd>
              <dt><kbd>F11</kbd></dt><dd>Fullscreen on / off — double-clicking the bar also leaves fullscreen</dd>
              <dt><kbd>F1</kbd></dt><dd>This page</dd>
              <dt><kbd>Esc</kbd></dt><dd>Closes any open menu</dd>
            </dl>
          </section>
          <section class="view-section">
            <h3>The ⇄ button — click for the menu, drag it anywhere</h3>
            <dl class="help-list">
              <dt>Rooms</dt><dd>Teams of sessions with a shared feed. <b>Post to feed</b> is a bulletin board: nobody is interrupted, the harnesses read it. <b>Send</b> delivers the text into one member's terminal as typed input — refused while that harness sits on a prompt or its state is unknown ("framing is blocked": type into its terminal instead). Creating a room, you choose whether the room brief is typed to members automatically and can edit its text.</dd>
              <dt>System log</dt><dd>The runtime's own messages in a bottom drawer. A red dot on the button means an error landed while the drawer was hidden; a failed close opens it.</dd>
              <dt>Themes</dt><dd>The theme gallery — one screenshot per theme; Dark and Light are plain terminal looks, the two PRIM-1 themes are the HUD look. <b>Unicolor</b> (off by default) drops the harnesses' own colours so everything renders in the theme's text colour.</dd>
              <dt>Fullscreen</dt><dd>Same as F11.</dd>
              <dt>Refresh state</dt><dd>Re-reads sessions and rooms from the supervisor.</dd>
              <dt>Settings</dt><dd>Which harnesses sit in the + menu; the runtime paths; whether PRIM-1 continues where you left off on open.</dd>
            </dl>
          </section>
        </div>
      </article>
    </section>

    <!-- System log drawer: off by default (corner menu → System log). -->
    <section class="log-drawer" id="log-drawer" aria-label="System log">
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
            <button type="button" class="ghost log-close" data-corner="log" aria-label="Hide system log" title="Hide system log">Hide</button>
          </div>
        </div>
        <div class="system-terminal" id="system-terminal"></div>
      </article>
    </section>
  </div>

  <!-- Corner button + menu. Mounted beside the shell so no panel clip-path can trap it.
       Left-click opens the menu; left-drag moves the button (position persists). -->
  <button type="button" class="corner-button" id="corner-button" aria-haspopup="menu" aria-expanded="false" aria-label="PRIM-1 menu" title="Menu — drag to move">
    <span class="corner-button-particles" aria-hidden="true">
      <span></span><span></span><span></span><span></span><span></span><span></span>
    </span>
    <span class="corner-button-glyph" aria-hidden="true">&#8644;</span>
    <span class="corner-button-alert" aria-hidden="true"></span>
  </button>
  <div class="corner-menu" id="corner-menu" role="menu" hidden>
    <button type="button" class="corner-action" data-corner="rooms" role="menuitem">
      <span class="corner-action-glyph" aria-hidden="true">&#8644;</span>
      <span class="corner-action-label" data-corner-view-label="rooms">Rooms</span>
    </button>
    <button type="button" class="corner-action" data-corner="log" role="menuitemcheckbox" aria-checked="false">
      <span class="corner-action-glyph" aria-hidden="true">&#8801;</span>
      <span class="corner-action-label">System log</span>
      <span class="corner-action-state" data-corner-log-state>off</span>
    </button>
    <div class="corner-menu-sep" role="separator"></div>
    <button type="button" class="corner-action" data-corner="themes" role="menuitem">
      <span class="corner-action-glyph" aria-hidden="true">&#9680;</span>
      <span class="corner-action-label" data-corner-view-label="themes">Themes</span>
    </button>
    <button type="button" class="corner-action" data-corner="fullscreen" role="menuitem">
      <span class="corner-action-glyph" aria-hidden="true">&#9974;</span>
      <span class="corner-action-label">Fullscreen</span>
      <span class="corner-action-hint">F11</span>
    </button>
    <button type="button" class="corner-action" data-control="refresh" role="menuitem">
      <span class="corner-action-glyph" aria-hidden="true">&#8635;</span>
      <span class="corner-action-label">Refresh state</span>
    </button>
    <div class="corner-menu-sep" role="separator"></div>
    <button type="button" class="corner-action" data-corner="settings" role="menuitem">
      <span class="corner-action-glyph" aria-hidden="true">&#9881;</span>
      <span class="corner-action-label" data-corner-view-label="settings">Settings</span>
    </button>
    <button type="button" class="corner-action" data-corner="help" role="menuitem">
      <span class="corner-action-glyph" aria-hidden="true">?</span>
      <span class="corner-action-label" data-corner-view-label="help">Help</span>
      <span class="corner-action-hint">F1</span>
    </button>
  </div>

  <!-- Attach popover: the "+" in the bar (and the zero state). Picking a harness
       creates the session AND launches it; "Custom…" opens the full form. The
       harness rows are rendered on first open from the same driver table as the tabs. -->
  <div class="corner-menu attach-menu" id="attach-menu" role="menu" aria-label="Attach a harness" hidden>
    <div class="attach-choices" id="attach-choices"></div>
    <div class="corner-menu-sep" role="separator"></div>
    <button type="button" class="corner-action" data-attach="custom" role="menuitem">
      <span class="corner-action-glyph" aria-hidden="true">&#8943;</span>
      <span class="corner-action-label">Custom…</span>
      <span class="corner-action-hint">label · profile · directory</span>
    </button>
    <p class="attach-menu-foot" id="attach-menu-foot"></p>
  </div>

  <!-- Tab menu: right-click on a tab. Move and edit live here so the tab itself
       shows the full name plus ×. Same handlers as the old hover buttons. -->
  <div class="corner-menu tab-menu" id="tab-menu" role="menu" aria-label="Tab" hidden>
    <p class="tab-menu-title" data-tab-menu-title></p>
    <button type="button" class="corner-action" data-tab-menu="move" data-delta="-1" role="menuitem">
      <span class="corner-action-glyph" aria-hidden="true">&#8592;</span>
      <span class="corner-action-label">Move left</span>
    </button>
    <button type="button" class="corner-action" data-tab-menu="move" data-delta="1" role="menuitem">
      <span class="corner-action-glyph" aria-hidden="true">&#8594;</span>
      <span class="corner-action-label">Move right</span>
    </button>
    <button type="button" class="corner-action" data-tab-menu="edit" role="menuitem">
      <span class="corner-action-glyph" aria-hidden="true">&#9998;</span>
      <span class="corner-action-label">Edit…</span>
    </button>
    <button type="button" class="corner-action" data-tab-menu="fresh" role="menuitem">
      <span class="corner-action-glyph" aria-hidden="true">&#8635;</span>
      <span class="corner-action-label">Start fresh session</span>
    </button>
    <div class="corner-menu-sep" role="separator"></div>
    <button type="button" class="corner-action" data-tab-menu="close" role="menuitem">
      <span class="corner-action-glyph" aria-hidden="true">&#215;</span>
      <span class="corner-action-label">Close</span>
      <span class="corner-action-hint">Ctrl+Shift+W</span>
    </button>
  </div>
`;

/** Unicolor (Themes page): drop the harnesses' own colours on the way into
    xterm so everything renders in the theme's text colour. Off by default —
    a normal terminal shows the harness's colours. */
let unicolor = localStorage.getItem("prim1-unicolor") === "1";

/** Diagnostic byte tap (localStorage `prim1-byte-tap` = "1"): ring-buffers the
    exact chunks each pane writes into its terminal, so a paint corruption can
    be replayed byte-for-byte. Off unless explicitly armed; ~4 MiB cap/pane. */
const paneByteTapEnabled = localStorage.getItem("prim1-byte-tap") === "1";
interface PaneTapEntry {
  chunk: string;
  cols: number;
  rows: number;
}
const paneTapRings = new Map<string, { entries: PaneTapEntry[]; bytes: number }>();
const PANE_TAP_MAX_BYTES = 4 * 1024 * 1024;

function recordPaneTap(sessionId: string, chunk: string, cols: number, rows: number): void {
  let ring = paneTapRings.get(sessionId);
  if (!ring) {
    ring = { entries: [], bytes: 0 };
    paneTapRings.set(sessionId, ring);
    (window as unknown as { __paneTap?: unknown }).__paneTap = paneTapRings;
  }
  ring.entries.push({ chunk, cols, rows });
  ring.bytes += chunk.length;
  while (ring.bytes > PANE_TAP_MAX_BYTES && ring.entries.length > 1) {
    ring.bytes -= ring.entries.shift()!.chunk.length;
  }
}

function setUnicolor(enabled: boolean): void {
  if (unicolor === enabled) {
    return;
  }
  unicolor = enabled;
  localStorage.setItem("prim1-unicolor", enabled ? "1" : "0");
  if (!enabled) {
    for (const pane of paneMap.values()) {
      pane.releaseColourFilter();
    }
  }
}

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
  private readonly initialBanner: InitialTerminalBanner;
  private readonly initialBannerLabel: HTMLElement;
  private readonly initialBannerElement: HTMLDivElement;
  private snapshot: SessionSnapshot | null = null;
  private pendingRepaintResync = false;
  private readonly colourFilter = new SgrColourFilter();

  constructor(sessionId: string, alias: string, label: string) {
    this.sessionId = sessionId;
    this.alias = alias;
    this.label = label;
    this.host = must<HTMLDivElement>(`[data-terminal="${sessionId}"]`);
    this.stateEl = must<HTMLSpanElement>(`[data-session-state="${sessionId}"]`);
    this.activityEl = must<HTMLSpanElement>(`[data-session-activity="${sessionId}"]`);
    this.terminal = new Terminal({
      // NEVER convertEol on a ConPTY-fed pane. A bare LF in a VT stream means
      // "down one row, SAME column", and the real ConPTY emits them (with CUF
      // and ECH — the legacy winpty dialect doesn't, which is why offline
      // captures couldn't reproduce). Rewriting LF to CR+LF throws the cursor
      // to column 0, the next run of text paints at the left edge, and
      // ConPTY's later diffs — addressed to where IT put the text — repaint
      // columns 5+ and can never heal columns 0-4: the Grok left-strip
      // ghosts (2026-08-30, tap-replayed byte-for-byte: convertEol true →
      // 8 ghost rows, false → 0).
      convertEol: false,
      cursorBlink: true,
      fontFamily: '"JetBrains Mono", "Cascadia Code", Consolas, monospace',
      fontSize: 13,
      // The pane is fed by ConPTY, which re-emits a re-wrapped repaint of its
      // own buffer on every resize. Without declaring the backend, xterm ALSO
      // reflows its buffer — the double transformation tears positioned
      // painters (Grok's pinned prompt region scattered on every resize).
      windowsPty: { backend: "conpty" },
      // New panes start in the active theme; applyTheme() keeps them in sync.
      theme: TERMINAL_THEMES[currentThemeName()].session,
    });
    this.fitAddon = new FitAddon();
    this.terminal.loadAddon(this.fitAddon);
    this.terminal.open(this.host);
    this.fitAddon.fit();
    this.initialBannerElement = document.createElement("div");
    this.initialBannerElement.className = "terminal-prelaunch";
    this.initialBannerElement.setAttribute("role", "status");
    this.initialBannerLabel = document.createElement("strong");
    this.initialBannerLabel.textContent = `${this.label} pane ready.`;
    const instruction = document.createElement("span");
    instruction.textContent = "Launch the session from the header to begin.";
    this.initialBannerElement.append(this.initialBannerLabel, instruction);
    this.host.appendChild(this.initialBannerElement);
    this.initialBanner = new InitialTerminalBanner(
      () => this.initialBannerElement.remove(),
      (chunk) => this.terminal.write(chunk),
    );
    this.host.addEventListener("focusin", () => {
      tabState.activeId = this.sessionId;
      activeCopySurface = this.sessionId;
    });
    this.host.addEventListener("mousedown", () => {
      tabState.activeId = this.sessionId;
      activeCopySurface = this.sessionId;
    });
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
    this.initialBannerLabel.textContent = `${this.label} pane ready.`;
    this.snapshot = snapshot;
    this.stateEl.textContent = snapshot.lifecycle_state;
    this.stateEl.dataset.state = snapshot.lifecycle_state;
    this.activityEl.textContent = snapshot.last_activity_at
      ? new Date(snapshot.last_activity_at).toLocaleTimeString()
      : snapshot.running
        ? "running"
        : "idle";
    this.activityEl.dataset.running = String(snapshot.running);

    this.initialBanner.observeRunning(snapshot.running);
    this.hookInput();
  }

  write(chunk: string): void {
    if (paneByteTapEnabled) {
      recordPaneTap(this.sessionId, chunk, this.terminal.cols, this.terminal.rows);
    }
    this.initialBanner.forwardOutput(unicolor ? this.colourFilter.apply(chunk) : chunk);
  }

  /** Unicolor switched off mid-stream: hand any held partial sequence on. */
  releaseColourFilter(): void {
    const carried = this.colourFilter.flush();
    if (carried) {
      this.initialBanner.forwardOutput(carried);
    }
  }

  /** Measure the pane area — only a visible host can be measured. */
  measure(): { cols: number; rows: number } | null {
    if (!isPaneVisible(this.sessionId)) {
      return null;
    }
    const proposed = this.fitAddon.proposeDimensions();
    if (!proposed || !Number.isFinite(proposed.cols) || !Number.isFinite(proposed.rows)) {
      return null;
    }
    return { cols: Math.max(2, proposed.cols), rows: Math.max(1, proposed.rows) };
  }

  /** Every pane keeps the same grid, visible or not, so a background PTY
      (sized from the persisted pane size at spawn) never writes into a
      default-width grid. Grok runs its fullscreen TUI, which fully repaints
      on resize like the other harnesses — no per-driver pinning needed. */
  applyGrid(cols: number, rows: number): void {
    if (this.terminal.cols !== cols || this.terminal.rows !== rows) {
      const before = `${this.terminal.cols}×${this.terminal.rows}`;
      this.terminal.resize(cols, rows);
      if (this.snapshot?.running) {
        // Mid-run size changes are when paint-divergence bugs surface; keep
        // a receipt of what resized and when.
        writeSystem(
          "info",
          `pane grid resized ${before} → ${cols}×${rows} (${this.label})`,
        );
      }
    }
    if (this.snapshot?.running) {
      if (this.pendingRepaintResync && rows > 1) {
        // The pane discarded a headless buffered stream; a same-size PTY
        // resize is a no-op, so wiggle rows to force ConPTY and the harness
        // to repaint the full screen at the real grid.
        this.pendingRepaintResync = false;
        void resizeSession(this.sessionId, cols, rows - 1).then(() =>
          resizeSession(this.sessionId, cols, rows),
        );
        return;
      }
      void resizeSession(this.sessionId, cols, rows);
    }
  }

  /** Called when buffered output for this pane was discarded as unusable:
      the next applyGrid with a live session forces a full harness repaint. */
  requestFullRepaintResync(): void {
    this.pendingRepaintResync = true;
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
  // The Dark system palette (TERMINAL_THEMES is declared further down, so the
  // literal is repeated here); applyTheme() replaces it at startup.
  theme: {
    background: "#000000",
    foreground: "#cccccc",
    cursor: "#ffffff",
    cursorAccent: "#000000",
    selectionBackground: "rgba(255, 255, 255, 0.18)",
    black: "#0c0c0c", brightBlack: "#767676",
    red: "#c50f1f", brightRed: "#e74856",
    green: "#13a10e", brightGreen: "#16c60c",
    yellow: "#c19c00", brightYellow: "#f9f1a5",
    blue: "#0037da", brightBlue: "#3b78ff",
    magenta: "#881798", brightMagenta: "#b4009e",
    cyan: "#3a96dd", brightCyan: "#61d6d6",
    white: "#cccccc", brightWhite: "#f2f2f2",
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
const permissionNote = must<HTMLElement>("#permission-note");
const sessionFormNote = must<HTMLElement>("#session-form-note");
const sessionFormError = must<HTMLElement>("#session-form-error");
const saveSessionButton = must<HTMLButtonElement>("#save-session");
const zeroSession = must<HTMLElement>("#zero-session");
const appShell = must<HTMLElement>(".app-shell");
const roomsView = must<HTMLElement>("#rooms-view");
const brandActiveTag = must<HTMLElement>("#brand-active-tag");
const cornerButton = must<HTMLButtonElement>("#corner-button");
const cornerMenu = must<HTMLDivElement>("#corner-menu");
const attachMenu = must<HTMLDivElement>("#attach-menu");
const attachChoices = must<HTMLDivElement>("#attach-choices");
const attachMenuFoot = must<HTMLElement>("#attach-menu-foot");
const tabMenu = must<HTMLDivElement>("#tab-menu");
const zeroAttachChoices = must<HTMLDivElement>("#zero-attach-choices");
const settingsView = must<HTMLElement>("#settings-view");
const helpView = must<HTMLElement>("#help-view");
const themesView = must<HTMLElement>("#themes-view");
const themeGallery = must<HTMLDivElement>("#theme-gallery");
const unicolorToggle = must<HTMLInputElement>("#unicolor-toggle");
const quickAttachSettings = must<HTMLUListElement>("#quick-attach-settings");
const autoResumeToggle = must<HTMLInputElement>("#auto-resume-toggle");
autoResumeToggle.checked = localStorage.getItem("prim1-auto-resume") !== "0";
autoResumeToggle.addEventListener("change", () => {
  localStorage.setItem("prim1-auto-resume", autoResumeToggle.checked ? "1" : "0");
});
const settingsWorkspace = must<HTMLElement>("#settings-workspace");
const settingsRuntimeDir = must<HTMLElement>("#settings-runtime-dir");
const settingsAuditPath = must<HTMLElement>("#settings-audit-path");
const settingsControlEndpoint = must<HTMLElement>("#settings-control-endpoint");
type ShellView = "sessions" | "rooms" | "settings" | "help" | "themes";
const VIEW_NAMES: Record<ShellView, string> = {
  sessions: "Sessions",
  rooms: "Rooms",
  settings: "Settings",
  help: "Help",
  themes: "Themes",
};
let shellView: ShellView = "sessions";
let cornerMenuOpen = false;
let attachMenuOpen = false;
let attachMenuAnchor: HTMLElement | null = null;
let attachPending = false;
let tabMenuSessionId: string | null = null;
let suppressTabClickUntil = 0;

/** The harnesses the "+" popover can offer, in tab-monogram order. */
const ATTACH_DRIVERS: readonly DriverKind[] = [
  "claude",
  "codex",
  "grok",
  "prime",
  "generic_terminal",
];

/** Settings → Quick attach menu: order of all five harnesses + the hidden ones.
    Local preference (localStorage), validated on load; unknown entries dropped,
    missing ones appended, so a stale value can never lose a harness. */
interface QuickAttachPrefs {
  order: DriverKind[];
  hidden: DriverKind[];
}
const QUICK_ATTACH_KEY = "prim1-quick-attach";
let quickAttachPrefs: QuickAttachPrefs = loadQuickAttachPrefs();

function loadQuickAttachPrefs(): QuickAttachPrefs {
  const isDriver = (value: unknown): value is DriverKind =>
    typeof value === "string" && (ATTACH_DRIVERS as readonly string[]).includes(value);
  let order: DriverKind[] = [];
  let hidden: DriverKind[] = [];
  try {
    const raw = localStorage.getItem(QUICK_ATTACH_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as { order?: unknown; hidden?: unknown };
      if (Array.isArray(parsed.order)) {
        order = parsed.order.filter(isDriver);
      }
      if (Array.isArray(parsed.hidden)) {
        hidden = parsed.hidden.filter(isDriver);
      }
    }
  } catch {
    /* corrupt or unavailable — defaults */
  }
  order = Array.from(new Set(order));
  for (const driver of ATTACH_DRIVERS) {
    if (!order.includes(driver)) {
      order.push(driver);
    }
  }
  return { order, hidden: Array.from(new Set(hidden)) };
}

function saveQuickAttachPrefs(): void {
  try {
    localStorage.setItem(QUICK_ATTACH_KEY, JSON.stringify(quickAttachPrefs));
  } catch {
    /* storage unavailable — the choice just won't persist */
  }
}

function quickAttachDrivers(): DriverKind[] {
  return quickAttachPrefs.order.filter((driver) => !quickAttachPrefs.hidden.includes(driver));
}
const zeroWorkspace = must<HTMLElement>("#zero-workspace");
const zeroKicker = must<HTMLElement>("#zero-kicker");
const contextRoomsChip = must<HTMLButtonElement>("#context-rooms-chip");
const contextNameChip = must<HTMLButtonElement>("#context-name-chip");
const roomsOverviewGrid = must<HTMLDivElement>("#rooms-overview-grid");
const roomsOverviewCard = must<HTMLElement>("#rooms-overview");
const roomsBackButton = must<HTMLButtonElement>("#rooms-back");
const roomPanelTitle = must<HTMLElement>("#room-panel-title");
const roomCard = must<HTMLElement>("#room-card");
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
const roomBriefAuto = must<HTMLInputElement>("#room-brief-auto");
const roomBriefTemplate = must<HTMLTextAreaElement>("#room-brief-template");
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

/* ── Work contexts (Rooms B1) ──
   The operator stands in the lobby or in one room; the top bar, grid,
   zero-state and keyboard order show only that context's sessions. Panes of
   every other context stay mounted and their harnesses keep running — leaving
   a room never stops its agents. On boot only the restored context's sessions
   relaunch; each other room spends its owed resumes on first entry. */
const WORK_CONTEXT_KEY = "prim1-work-context";
const storedWorkContextRaw = localStorage.getItem(WORK_CONTEXT_KEY);
let workContext: WorkContext = parseWorkContext(storedWorkContextRaw);
/** False only until a context has ever been chosen on this origin — gates the
    one-shot first-boot default below. */
let workContextWasStored = storedWorkContextRaw !== null;
let roomsViewMode: "overview" | "panel" = "overview";
/** Remembered active tab per context, so re-entering lands where you left. */
const lastActiveByContext = new Map<string, string>();
/** Sessions still owed their boot resume; spent on first context entry. */
const resumeOwed = new Set<string>();

function persistWorkContext(): void {
  localStorage.setItem(WORK_CONTEXT_KEY, JSON.stringify(workContext));
}

function sessionInContext(sessionId: string): boolean {
  return sessionInContextPure(roomSnapshots, sessionId, workContext);
}

function visibleTabOrder(): string[] {
  return visibleSessionOrder(tabState.order, roomSnapshots, workContext);
}

function contextLabel(): string {
  return contextDisplayLabel(roomSnapshots, workContext);
}

function currentContextSessions(): SessionSnapshot[] {
  return tabState.order
    .map((sessionId) => snapshotById.get(sessionId))
    .filter((session): session is SessionSnapshot => session !== undefined);
}
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
  /* A plain terminal: black field, white text, the Windows Terminal
     "Campbell" ANSI palette — harness output looks exactly as it does in a
     normal console. The default for anyone who never chose a theme. */
  dark: {
    session: {
      background: "#000000",
      foreground: "#f2f2f2",
      cursor: "#ffffff",
      cursorAccent: "#000000",
      selectionBackground: "rgba(255, 255, 255, 0.22)",
      black: "#0c0c0c", brightBlack: "#767676",
      red: "#c50f1f", brightRed: "#e74856",
      green: "#13a10e", brightGreen: "#16c60c",
      yellow: "#c19c00", brightYellow: "#f9f1a5",
      blue: "#0037da", brightBlue: "#3b78ff",
      magenta: "#881798", brightMagenta: "#b4009e",
      cyan: "#3a96dd", brightCyan: "#61d6d6",
      white: "#cccccc", brightWhite: "#f2f2f2",
    },
    system: {
      background: "#000000",
      foreground: "#cccccc",
      cursor: "#ffffff",
      cursorAccent: "#000000",
      selectionBackground: "rgba(255, 255, 255, 0.18)",
      black: "#0c0c0c", brightBlack: "#767676",
      red: "#c50f1f", brightRed: "#e74856",
      green: "#13a10e", brightGreen: "#16c60c",
      yellow: "#c19c00", brightYellow: "#f9f1a5",
      blue: "#0037da", brightBlue: "#3b78ff",
      magenta: "#881798", brightMagenta: "#b4009e",
      cyan: "#3a96dd", brightCyan: "#61d6d6",
      white: "#cccccc", brightWhite: "#f2f2f2",
    },
  },
  /* The same idea on white: near-black text, a light ANSI palette. */
  light: {
    session: {
      background: "#ffffff",
      foreground: "#1e1e1e",
      cursor: "#1e1e1e",
      cursorAccent: "#ffffff",
      selectionBackground: "rgba(0, 0, 0, 0.16)",
      black: "#383a42", brightBlack: "#4f525d",
      red: "#e45649", brightRed: "#df6c75",
      green: "#50a14f", brightGreen: "#98c379",
      yellow: "#c18401", brightYellow: "#e4c07a",
      blue: "#0184bc", brightBlue: "#61afef",
      magenta: "#a626a4", brightMagenta: "#c577dd",
      cyan: "#0997b3", brightCyan: "#56b5c1",
      white: "#fafafa", brightWhite: "#ffffff",
    },
    system: {
      background: "#f6f6f6",
      foreground: "#1e1e1e",
      cursor: "#1e1e1e",
      cursorAccent: "#f6f6f6",
      selectionBackground: "rgba(0, 0, 0, 0.12)",
      black: "#383a42", brightBlack: "#4f525d",
      red: "#e45649", brightRed: "#df6c75",
      green: "#50a14f", brightGreen: "#98c379",
      yellow: "#c18401", brightYellow: "#e4c07a",
      blue: "#0184bc", brightBlue: "#61afef",
      magenta: "#a626a4", brightMagenta: "#c577dd",
      cyan: "#0997b3", brightCyan: "#56b5c1",
      white: "#fafafa", brightWhite: "#ffffff",
    },
  },
} as const;

type ThemeName = keyof typeof TERMINAL_THEMES;

/** The Themes page lists these, in this order, each with a real screenshot of
    the app in that theme (public/textures/themes/<name>.png). */
const THEME_ORDER: ThemeName[] = ["dark", "light", "prim1", "prim1-deep"];
const DEFAULT_THEME: ThemeName = "dark";
const THEME_META: Record<ThemeName, { label: string; blurb: string }> = {
  dark: {
    label: "Dark",
    blurb: "Black field, white text, standard terminal colours. Harness output looks exactly as in a normal console.",
  },
  light: {
    label: "Light",
    blurb: "White field, near-black text, a light terminal palette. Same plain chrome as Dark.",
  },
  prim1: {
    label: "PRIM-1",
    blurb: "The original HUD look: yellow frames, magenta text, scanlines and glow.",
  },
  "prim1-deep": {
    label: "PRIM-1 Deep",
    blurb: "The HUD look with everything in magenta, plus the dissolve effects.",
  },
};

/** `chosen` marks an explicit pick on the Themes page; only those survive as
    a preference. Values written by the old cycle button were never a choice,
    so a user who never picked lands on the default. */
function applyTheme(name: ThemeName, chosen = false): void {
  document.documentElement.dataset.theme = name;
  localStorage.setItem("prim1-theme", name);
  if (chosen) {
    localStorage.setItem("prim1-theme-chosen", "1");
  }

  const themes = TERMINAL_THEMES[name];
  for (const pane of paneMap.values()) {
    pane.terminal.options.theme = themes.session;
  }
  systemTerminal.options.theme = themes.system;
  markCurrentThemeCard(name);
}

function currentThemeName(): ThemeName {
  const stored = document.documentElement.dataset.theme;
  if (stored && stored in TERMINAL_THEMES) return stored as ThemeName;
  return DEFAULT_THEME;
}

/** Themes page: one card per theme — screenshot, name, blurb, Use. */
function wireThemesPage(): void {
  themeGallery.replaceChildren(
    ...THEME_ORDER.map((name) => {
      const meta = THEME_META[name];
      const card = document.createElement("button");
      card.type = "button";
      card.className = "theme-card";
      card.dataset.themePick = name;
      card.setAttribute("aria-pressed", "false");
      const shot = document.createElement("img");
      shot.className = "theme-card-shot";
      shot.src = `/textures/themes/${name}.png`;
      shot.alt = `${meta.label} theme`;
      shot.loading = "lazy";
      const title = document.createElement("span");
      title.className = "theme-card-title";
      const label = document.createElement("span");
      label.className = "theme-card-name";
      label.textContent = meta.label;
      const state = document.createElement("span");
      state.className = "theme-card-state";
      state.textContent = "current";
      title.append(label, state);
      const blurb = document.createElement("span");
      blurb.className = "theme-card-blurb";
      blurb.textContent = meta.blurb;
      const use = document.createElement("span");
      use.className = "theme-card-use";
      use.textContent = "Use this theme";
      card.append(shot, title, blurb, use);
      return card;
    }),
  );
  unicolorToggle.checked = unicolor;
  unicolorToggle.addEventListener("change", () => {
    setUnicolor(unicolorToggle.checked);
  });
  themeGallery.addEventListener("click", (event) => {
    if (!(event.target instanceof Element)) {
      return;
    }
    const card = event.target.closest<HTMLButtonElement>("[data-theme-pick]");
    const name = card?.dataset.themePick;
    if (name && name in TERMINAL_THEMES) {
      applyTheme(name as ThemeName, true);
    }
  });
}

function markCurrentThemeCard(name: ThemeName): void {
  for (const card of themeGallery.querySelectorAll<HTMLButtonElement>("[data-theme-pick]")) {
    const current = card.dataset.themePick === name;
    card.setAttribute("aria-pressed", String(current));
  }
}

/** Shell views + drawers. Only presentation: the room and system-log DOM and
    every handler in them are untouched; the view swap hides/shows containers
    and refits xterm on the next frame (the body ResizeObserver cannot see
    internal grid changes). */
function setShellView(view: ShellView): void {
  shellView = view;
  appShell.dataset.view = view;
  roomsView.hidden = view !== "rooms";
  if (view === "rooms") {
    roomsOverviewCard.hidden = roomsViewMode !== "overview";
    roomCard.hidden = roomsViewMode !== "panel";
  }
  settingsView.hidden = view !== "settings";
  helpView.hidden = view !== "help";
  themesView.hidden = view !== "themes";
  brandActiveTag.textContent = VIEW_NAMES[view];
  for (const label of cornerMenu.querySelectorAll<HTMLElement>("[data-corner-view-label]")) {
    const target = label.dataset.cornerViewLabel as ShellView;
    label.textContent = view === target ? "Back to sessions" : VIEW_NAMES[target];
  }
  if (view === "settings") {
    renderQuickAttachSettings();
  }
  requestAnimationFrame(() => {
    fitVisiblePanes();
    systemFit.fit();
  });
}

function setLogDrawer(open: boolean): void {
  appShell.dataset.log = open ? "open" : "closed";
  const item = cornerMenu.querySelector<HTMLButtonElement>('[data-corner="log"]');
  item?.setAttribute("aria-checked", String(open));
  const state = cornerMenu.querySelector<HTMLElement>("[data-corner-log-state]");
  if (state) {
    state.textContent = open ? "on" : "off";
  }
  if (open) {
    delete cornerButton.dataset.alert;
  }
  requestAnimationFrame(() => {
    fitVisiblePanes();
    systemFit.fit();
  });
}

const CORNER_POSITION_KEY = "prim1-corner-button";

/** Corner button: click opens the menu; left-drag moves it (clamped to the
    viewport, persisted). Drag vs click is decided by a 5 px threshold. */
function wireCornerButton(): void {
  try {
    const raw = localStorage.getItem(CORNER_POSITION_KEY);
    if (raw) {
      const saved = JSON.parse(raw) as { x?: unknown; y?: unknown };
      if (typeof saved.x === "number" && typeof saved.y === "number") {
        placeCornerButton(saved.x, saved.y);
      }
    }
  } catch {
    /* ignore a corrupt saved position */
  }

  let drag: {
    pointerId: number;
    startX: number;
    startY: number;
    originX: number;
    originY: number;
    moved: boolean;
  } | null = null;

  cornerButton.addEventListener("pointerdown", (event) => {
    if (event.button !== 0) {
      return;
    }
    const rect = cornerButton.getBoundingClientRect();
    drag = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      originX: rect.left,
      originY: rect.top,
      moved: false,
    };
    cornerButton.setPointerCapture(event.pointerId);
  });

  cornerButton.addEventListener("pointermove", (event) => {
    if (!drag || event.pointerId !== drag.pointerId) {
      return;
    }
    const dx = event.clientX - drag.startX;
    const dy = event.clientY - drag.startY;
    if (!drag.moved && Math.hypot(dx, dy) < 5) {
      return;
    }
    drag.moved = true;
    cornerButton.dataset.dragging = "true";
    placeCornerButton(drag.originX + dx, drag.originY + dy);
  });

  const endDrag = (event: PointerEvent): void => {
    if (!drag || event.pointerId !== drag.pointerId) {
      return;
    }
    const wasDrag = drag.moved;
    drag = null;
    delete cornerButton.dataset.dragging;
    try {
      cornerButton.releasePointerCapture(event.pointerId);
    } catch {
      /* already released */
    }
    if (wasDrag) {
      const rect = cornerButton.getBoundingClientRect();
      try {
        localStorage.setItem(
          CORNER_POSITION_KEY,
          JSON.stringify({ x: rect.left, y: rect.top }),
        );
      } catch {
        /* storage unavailable — position just won't persist */
      }
      if (cornerMenuOpen) {
        placeCornerMenu();
      }
      return;
    }
    if (event.type === "pointerup") {
      setCornerMenuOpen(!cornerMenuOpen);
    }
  };
  cornerButton.addEventListener("pointerup", endDrag);
  cornerButton.addEventListener("pointercancel", endDrag);

  window.addEventListener("resize", () => {
    const rect = cornerButton.getBoundingClientRect();
    if (cornerButton.style.left) {
      placeCornerButton(rect.left, rect.top);
    }
    if (cornerMenuOpen) {
      placeCornerMenu();
    }
  });

  cornerMenu.addEventListener("click", (event) => {
    if (!(event.target instanceof Element)) {
      return;
    }
    const action = event.target.closest<HTMLElement>("[data-corner]");
    if (!action) {
      return;
    }
    runCornerAction(action.dataset.corner ?? "");
  });
  // The system-log card's own Hide button shares the same action.
  for (const button of document.querySelectorAll<HTMLButtonElement>(
    ".log-close[data-corner]",
  )) {
    button.addEventListener("click", () => runCornerAction("log"));
  }
  // "Back to sessions" inside the Settings / Help views.
  for (const button of document.querySelectorAll<HTMLButtonElement>(
    ".view-back[data-corner]",
  )) {
    button.addEventListener("click", () => runCornerAction(button.dataset.corner ?? ""));
  }

  document.addEventListener("click", (event) => {
    if (!cornerMenuOpen || !(event.target instanceof Node)) {
      return;
    }
    if (!cornerButton.contains(event.target) && !cornerMenu.contains(event.target)) {
      setCornerMenuOpen(false);
    }
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && cornerMenuOpen) {
      setCornerMenuOpen(false);
    }
  });
}

function runCornerAction(kind: string): void {
  switch (kind) {
    case "rooms": {
      if (shellView === "rooms") {
        setShellView("sessions");
      } else {
        openRoomsOverview();
      }
      setCornerMenuOpen(false);
      break;
    }
    case "settings":
    case "help":
    case "themes": {
      const view = kind as ShellView;
      setShellView(shellView === view ? "sessions" : view);
      setCornerMenuOpen(false);
      break;
    }
    case "log":
      setLogDrawer(appShell.dataset.log !== "open");
      break;
    case "fullscreen":
      setCornerMenuOpen(false);
      void toggleFullscreen();
      break;
    default:
      break;
  }
}

function placeCornerButton(x: number, y: number): void {
  const margin = 8;
  const width = cornerButton.offsetWidth || 64;
  const height = cornerButton.offsetHeight || 64;
  const left = Math.min(Math.max(x, margin), Math.max(margin, window.innerWidth - width - margin));
  const top = Math.min(Math.max(y, margin), Math.max(margin, window.innerHeight - height - margin));
  cornerButton.style.left = `${Math.round(left)}px`;
  cornerButton.style.top = `${Math.round(top)}px`;
  cornerButton.style.right = "auto";
  cornerButton.style.bottom = "auto";
}

function placeCornerMenu(): void {
  const button = cornerButton.getBoundingClientRect();
  const menu = cornerMenu.getBoundingClientRect();
  const gap = 10;
  const margin = 8;
  let top = button.top - menu.height - gap;
  if (top < margin) {
    top = button.bottom + gap;
  }
  let left = button.right - menu.width;
  if (left < margin) {
    left = margin;
  }
  if (left + menu.width > window.innerWidth - margin) {
    left = Math.max(margin, window.innerWidth - margin - menu.width);
  }
  cornerMenu.style.top = `${Math.round(top)}px`;
  cornerMenu.style.left = `${Math.round(left)}px`;
}

function setCornerMenuOpen(open: boolean): void {
  cornerMenuOpen = open;
  cornerMenu.hidden = !open;
  cornerButton.setAttribute("aria-expanded", String(open));
  if (open) {
    placeCornerMenu();
  }
}

/** Window controls for the undecorated window. Close goes through the same
    close-requested path as the native button (supervisor shutdown in lib.rs). */
function wireWindowControls(): void {
  const win = getCurrentWindow();
  const refresh = async (): Promise<void> => {
    try {
      const [maximized, fullscreen] = await Promise.all([
        win.isMaximized(),
        win.isFullscreen(),
      ]);
      appShell.dataset.maximized = String(maximized);
      if (appShell.dataset.fullscreen !== String(fullscreen)) {
        scheduleTerminalRefit();
      }
      appShell.dataset.fullscreen = String(fullscreen);
      const maxButton = document.querySelector<HTMLButtonElement>('[data-window="maximize"]');
      if (maxButton) {
        const label = fullscreen
          ? "Exit fullscreen"
          : maximized
            ? "Restore"
            : "Maximize";
        maxButton.title = label;
        maxButton.setAttribute("aria-label", label);
      }
      if (!fullscreen) {
        // Anything that un-fullscreens the window can hand it a broken rect;
        // settle it whenever the window is not fullscreen.
        void settleWindowRect();
      }
    } catch {
      /* window state unavailable — leave the flags as they are */
    }
  };
  for (const button of document.querySelectorAll<HTMLButtonElement>("[data-window]")) {
    button.addEventListener("click", async () => {
      const kind = button.dataset.window;
      try {
        if (kind === "minimize") {
          await win.minimize();
        } else if (kind === "maximize") {
          if (appShell.dataset.fullscreen === "true") {
            await toggleFullscreen();
          } else {
            await win.toggleMaximize();
            await refresh();
          }
        } else if (kind === "close") {
          await win.close();
        }
      } catch (error) {
        writeSystem("error", `window ${kind ?? "control"} failed: ${String(error)}`);
      }
    });
  }
  void win.onResized(() => {
    void refresh();
  });
  void refresh();

  // Double-click on the drag region: Tauri's built-in handler toggles
  // *maximize*, which on a fullscreen window leaves it with a broken rect
  // (looked like a crash). In fullscreen, double-click exits fullscreen
  // instead; windowed, Tauri's maximize toggle stays.
  window.addEventListener(
    "mousedown",
    (event) => {
      if (event.button !== 0 || event.detail < 2) {
        return;
      }
      if (!(event.target instanceof Element) || !event.target.hasAttribute("data-tauri-drag-region")) {
        return;
      }
      if (appShell.dataset.fullscreen !== "true") {
        return;
      }
      event.preventDefault();
      event.stopImmediatePropagation();
      void toggleFullscreen();
    },
    { capture: true },
  );
}

async function toggleFullscreen(): Promise<void> {
  try {
    await command<void>("toggle_fullscreen");
  } catch (error) {
    writeSystem("error", `fullscreen toggle failed: ${String(error)}`);
    return;
  }
  await settleWindow();
  scheduleTerminalRefit();
}

/** xterm sizes its canvas from a layout the fullscreen swap has not finished —
    the text garbled until the next tab switch. Refit a few times while the
    transition settles. */
function scheduleTerminalRefit(): void {
  for (const delay of [60, 250, 700]) {
    setTimeout(() => {
      fitVisiblePanes();
      systemFit.fit();
    }, delay);
  }
}

/** After leaving fullscreen the window can come back with a degenerate rect
    (observed: -25600,-25600 159×27 — it was created hidden and fullscreened
    before it ever had a restore size). Put it back to the configured size,
    centred. No-op while fullscreen or when the rect is sane. */
async function settleWindow(): Promise<void> {
  const win = getCurrentWindow();
  try {
    await new Promise((resolve) => setTimeout(resolve, 150));
    const fullscreen = await win.isFullscreen();
    appShell.dataset.fullscreen = String(fullscreen);
    if (fullscreen) {
      return;
    }
  } catch (error) {
    writeSystem("warn", `window settle failed: ${String(error)}`);
    return;
  }
  await settleWindowRect();
}

let settlingRect = false;

async function settleWindowRect(): Promise<void> {
  if (settlingRect) {
    return;
  }
  settlingRect = true;
  const win = getCurrentWindow();
  try {
    if ((await win.isFullscreen()) || (await win.isMaximized())) {
      return;
    }
    if (!(await win.isVisible())) {
      await win.show();
      writeSystem("warn", "window came back hidden after a state change — shown again");
    }
    const [size, position] = await Promise.all([win.outerSize(), win.outerPosition()]);
    // outerSize/outerPosition are physical px; screen.avail* are logical (CSS) px.
    const dpr = window.devicePixelRatio || 1;
    const availWidth = window.screen.availWidth;
    const availHeight = window.screen.availHeight;
    const degenerate =
      size.width < 640
      || size.height < 400
      || position.x < -4000
      || position.y < -4000;
    // Tolerate the invisible resize border (≈7 px logical per side).
    const oversized =
      size.width > (availWidth + 24) * dpr
      || size.height > (availHeight + 24) * dpr;
    if (!degenerate && !oversized) {
      return;
    }
    const width = Math.min(1560, Math.max(960, availWidth - 48));
    const height = Math.min(980, Math.max(600, availHeight - 48));
    await win.setSize(new LogicalSize(width, height));
    await win.center();
    writeSystem(
      "info",
      `window restored: was ${size.width}x${size.height} @${position.x},${position.y} (${degenerate ? "invalid" : "larger than the screen"}); now ${width}x${height} centred`,
    );
  } catch (error) {
    writeSystem("warn", `window settle failed: ${String(error)}`);
  } finally {
    settlingRect = false;
  }
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

/** The stored theme counts only if it was picked on the Themes page; values
    left behind by the old cycle button fall back to the default. */
function loadSavedTheme(): ThemeName {
  const raw = localStorage.getItem("prim1-theme");
  const chosen = localStorage.getItem("prim1-theme-chosen") === "1";
  if (chosen && raw !== null && raw in TERMINAL_THEMES) return raw as ThemeName;
  return DEFAULT_THEME;
}

applyTheme(loadSavedTheme());

wireSessionUi();
wireRoomUi();
wireControls();
wireResize();
wireTerminalShortcuts();
wireThemesPage();
markCurrentThemeCard(currentThemeName());
wireWindowControls();
wireCornerButton();
wireAttachMenu();
wireTabMenu();
void settleWindow();

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
  maybeResumeWorkspace();
}

/** "Continue where I left off" (Settings, on by default): on app open, every
    session that had a live run when the app last went down becomes an owed
    resume — but only the restored context's sessions launch NOW. Another
    room's sessions launch when the operator first enters that room; starting
    every room at boot is explicitly rejected (Rooms B1 invariant 2). The debt
    survives restarts on its own: the supervisor sets running_at_shutdown when
    a run spawns and clears it only on an operator stop. */
const AUTO_RESUME_KEY = "prim1-auto-resume";
let workspaceResumeAttempted = false;

function maybeResumeWorkspace(): void {
  if (workspaceResumeAttempted) {
    return;
  }
  workspaceResumeAttempted = true;
  if (localStorage.getItem(AUTO_RESUME_KEY) === "0") {
    return;
  }
  for (const session of snapshotById.values()) {
    if (session.was_running_at_shutdown && !session.running) {
      resumeOwed.add(session.session_id);
    }
  }
  if (
    !workContextWasStored
    && visibleTabOrder().length === 0
    && roomSnapshots.length > 0
  ) {
    // First boot on the rooms shell with every session already in a room:
    // land in the first room, not an empty lobby. enterWorkContext persists
    // the choice and spends that room's owed resumes.
    workContextWasStored = true;
    enterWorkContext({ kind: "room", roomId: roomSnapshots[0].room_id });
    return;
  }
  spendOwedResumes();
}

/** Tab menu → Start fresh session: a NEW harness conversation, abandoning the
    stored one. Only for a stopped session — a running one is its own truth. */
async function startFreshSession(sessionId: string): Promise<void> {
  const session = snapshotById.get(sessionId);
  if (session?.running) {
    writeSystem(
      "warn",
      "Stop the session first — Start fresh session begins a new conversation.",
    );
    return;
  }
  try {
    await command<SessionSnapshot>("start_session", {
      request: { session_id: sessionId, fresh: true } satisfies StartSessionRequest,
    });
    await refreshSnapshot(sessionId);
  } catch (error) {
    writeSystem("error", `Start fresh failed: ${String(error)}`);
  }
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
  // Rooms first: the tab bar, grid and zero-state filter by room membership,
  // so the context must be reconciled before the panes render.
  syncRoomInventory(snapshot.rooms);
  syncPaneInventory(snapshot.sessions, preferredSessionId);
  syncSessionForm();
  controlEndpoint.textContent = snapshot.control_plane?.endpoint ?? "starting...";
  auditPath.textContent = snapshot.audit_log_path;
  runtimePath.textContent = snapshot.runtime_dir;
  settingsWorkspace.textContent = snapshot.workspace_preference || "—";
  settingsRuntimeDir.textContent = snapshot.runtime_dir;
  settingsAuditPath.textContent = snapshot.audit_log_path;
  settingsControlEndpoint.textContent = snapshot.control_plane?.endpoint ?? "starting...";

  for (const session of snapshot.sessions) {
    if (acceptedSessions.has(session.session_id)) {
      paneMap.get(session.session_id)?.applySnapshot(session);
    }
  }
  fitVisiblePanes();
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
  if (workContext.kind === "room" && !liveIds.has(workContext.roomId)) {
    workContext = { kind: "lobby" };
    persistWorkContext();
    writeSystem("warn", "The room this workspace was in is gone — back to the lobby.");
  }
  if (workContext.kind === "room") {
    activeRoomId = workContext.roomId;
  } else if (!activeRoomId || !liveIds.has(activeRoomId)) {
    activeRoomId = incoming[0]?.room_id ?? null;
  }
  discardInactiveRoomFeedState();
  renderWorkContextChips();
  renderRoomsOverview();
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

function renderWorkContextChips(): void {
  contextNameChip.textContent = contextLabel();
  contextNameChip.dataset.kind = workContext.kind;
  if (workContext.kind === "room") {
    contextNameChip.disabled = false;
    contextNameChip.title = "Open this room's feed and members";
  } else {
    contextNameChip.disabled = true;
    contextNameChip.title = "Sessions in no room";
  }
}

function openRoomsOverview(): void {
  roomsViewMode = "overview";
  renderRoomsOverview();
  setShellView("rooms");
}

function openRoomPanel(roomId: string): void {
  activeRoomId = roomId;
  roomsViewMode = "panel";
  renderRoomUi();
  void initializeRoomFeed(roomId);
  setShellView("rooms");
}

/** Enter the lobby or a room: the top bar swaps to that context's sessions,
    the remembered tab (or the first) becomes active, and any resumes the
    context is still owed from the last shutdown are spent now. */
function enterWorkContext(context: WorkContext): void {
  const changed = contextKey(context) !== contextKey(workContext);
  if (changed && tabState.activeId) {
    lastActiveByContext.set(contextKey(workContext), tabState.activeId);
  }
  workContext = context;
  persistWorkContext();
  if (context.kind === "room") {
    activeRoomId = context.roomId;
  }
  renderWorkContextChips();
  renderSessionTabs(currentContextSessions());
  const visible = visibleTabOrder();
  const remembered = lastActiveByContext.get(contextKey(context));
  setActiveSession(
    remembered && visible.includes(remembered) ? remembered : visible[0] ?? null,
    false,
  );
  updateZeroSessionState();
  setShellView("sessions");
  if (context.kind === "room") {
    void initializeRoomFeed(context.roomId);
  }
  spendOwedResumes();
  renderRoomsOverview();
}

/** Launch the current context's sessions that still owe their boot resume.
    Other contexts' debts stay owed until the operator enters them —
    starting every room at boot is explicitly rejected (Rooms B1). */
function spendOwedResumes(): void {
  if (resumeOwed.size === 0) {
    return;
  }
  const targets: SessionSnapshot[] = [];
  for (const sessionId of Array.from(resumeOwed)) {
    const session = snapshotById.get(sessionId);
    if (!session) {
      resumeOwed.delete(sessionId);
      continue;
    }
    if (!sessionInContext(sessionId)) {
      continue;
    }
    resumeOwed.delete(sessionId);
    if (!session.running) {
      targets.push(session);
    }
  }
  if (targets.length === 0) {
    return;
  }
  writeSystem(
    "info",
    `Continuing where you left off in ${contextLabel()}: launching ${targets
      .map((session) => session.label)
      .join(", ")}.`,
  );
  for (const session of targets) {
    void command<SessionSnapshot>("start_session", {
      request: { session_id: session.session_id } satisfies StartSessionRequest,
    }).catch((error) =>
      writeSystem("error", `Auto-launch ${session.label} failed: ${String(error)}`),
    );
  }
  window.setTimeout(() => {
    void refreshSnapshot(tabState.activeId ?? undefined).catch(() => {});
  }, 1800);
}

function renderRoomsOverview(): void {
  const fragment = document.createDocumentFragment();
  fragment.appendChild(buildContextCard(null));
  for (const room of roomSnapshots) {
    fragment.appendChild(buildContextCard(room));
  }
  const create = document.createElement("button");
  create.type = "button";
  create.className = "rooms-overview-card rooms-overview-new";
  const glyph = document.createElement("span");
  glyph.className = "rooms-overview-new-glyph";
  glyph.textContent = "+";
  const text = document.createElement("span");
  text.textContent = "New room";
  create.append(glyph, text);
  create.addEventListener("click", () => {
    roomsViewMode = "panel";
    setShellView("rooms");
    openRoomCreateForm();
  });
  fragment.appendChild(create);
  roomsOverviewGrid.replaceChildren(fragment);
}

function buildContextCard(room: RoomSnapshot | null): HTMLElement {
  const card = document.createElement("button");
  card.type = "button";
  card.className = "rooms-overview-card";
  const standing =
    room === null
      ? workContext.kind === "lobby"
      : workContext.kind === "room" && workContext.roomId === room.room_id;
  if (standing) {
    card.dataset.current = "true";
  }
  const name = document.createElement("h3");
  name.textContent = room === null ? "Lobby" : room.label;
  const members = document.createElement("div");
  members.className = "rooms-overview-members";
  const memberIds =
    room === null
      ? tabState.order.filter(
          (sessionId) => sessionRoomIdPure(roomSnapshots, sessionId) === null,
        )
      : room.member_ids;
  if (memberIds.length === 0) {
    const empty = document.createElement("span");
    empty.className = "rooms-overview-empty";
    empty.textContent =
      room === null ? "No sessions outside rooms" : "No members yet";
    members.append(empty);
  } else {
    for (const sessionId of memberIds) {
      members.append(buildMemberChip(sessionId));
    }
  }
  card.append(name, members);
  card.addEventListener("click", () =>
    enterWorkContext(
      room === null ? { kind: "lobby" } : { kind: "room", roomId: room.room_id },
    ),
  );
  return card;
}

function buildMemberChip(sessionId: string): HTMLElement {
  const chip = document.createElement("span");
  chip.className = "rooms-member-chip";
  const session = snapshotById.get(sessionId);
  const readiness = sessionReadiness(session, resumeOwed.has(sessionId));
  const dot = document.createElement("span");
  dot.className = "readiness-dot";
  dot.dataset.state = readiness.kind;
  const tag = document.createElement("span");
  tag.className = "rooms-member-driver";
  tag.textContent = session ? driverTag(session.driver) : "";
  const label = document.createElement("span");
  label.textContent = session ? session.label : shortSessionId(sessionId);
  chip.append(dot, tag, label);
  chip.title = readiness.reason;
  return chip;
}

function renderRoomUi(): void {
  const selected = activeRoom();
  roomPanelTitle.textContent = selected ? selected.label : "Rooms";
  roomEmpty.hidden = selected !== null || !roomCreateForm.hidden;
  roomContent.hidden = selected === null || !roomCreateForm.hidden;
  const roomIndex = selected
    ? roomSnapshots.findIndex((room) => room.room_id === selected.room_id)
    : -1;
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
  roomSendButton.disabled = selected.member_ids.length < 1;
  roomSendButton.title = roomSendButton.disabled
    ? "Add a member before sending"
    : "Type the message into the selected member terminals";
}

function renderRoomMembers(room: RoomSnapshot): void {
  roomMembers.replaceChildren();
  for (const sessionId of room.member_ids) {
    const chip = document.createElement("span");
    chip.className = "room-member-chip";
    const readiness = sessionReadiness(
      snapshotById.get(sessionId),
      resumeOwed.has(sessionId),
    );
    const dot = document.createElement("span");
    dot.className = "readiness-dot room-member-dot";
    dot.dataset.state = readiness.kind;
    const label = document.createElement("span");
    label.textContent = `${paneLabel(sessionId)} · ${shortSessionId(sessionId)}`;
    label.title = readiness.reason;
    const brief = document.createElement("button");
    brief.type = "button";
    brief.className = "room-member-brief";
    brief.textContent = "Brief";
    brief.title = `Type the room brief into ${paneLabel(sessionId)}'s terminal now`;
    brief.addEventListener("click", () => void briefRoomMemberNow(room.room_id, sessionId));
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "room-member-remove";
    remove.textContent = "×";
    remove.title = `Remove ${paneLabel(sessionId)} from ${room.label}`;
    remove.addEventListener("click", () => void removeRoomMember(room.room_id, sessionId));
    chip.append(dot, label, brief, remove);
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
  all.textContent = room.member_ids.length === 1
    ? "Send to the only member"
    : `Send to all ${room.member_ids.length} members`;
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
  fitVisiblePanes();
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
    if (entry && (entry.chunks.length > 0 || entry.shed > 0)) {
      writeSystem(
        "info",
        "pendingOutput dropped for removed session "
          + shortSessionId(buffered)
          + " ("
          + entry.chunks.length
          + " queued + "
          + entry.shed
          + " overflow sheds)",
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
    pendingOutput.delete(session.session_id);
    if (pending.shed > 0) {
      // The buffered stream lost its head to the byte cap. A TUI paint
      // stream's tail painted onto a blank grid is scatter, not content —
      // discard it and force the harness to repaint the whole screen once
      // the real grid has been applied.
      pane.requestFullRepaintResync();
      writeSystem(
        "warn",
        "session_output resync: "
          + session.label
          + " overflowed the pending buffer; discarded "
          + pending.chunks.length
          + " tail chunks and scheduled a full repaint",
      );
      continue;
    }
    for (const chunk of pending.chunks) {
      pane.write(chunk);
    }
    if (pending.chunks.length > 0) {
      writeSystem(
        "info",
        "session_output flushed: "
          + pending.chunks.length
          + " chunks into "
          + session.label,
      );
    }
  }

  tabState = transition.state;
  renderSessionTabs(effectiveSessions);
  updateZeroSessionState();
  setActiveSession(tabState.activeId, false);
  // Rooms sync ran before the panes existed in tabState; re-render the
  // overview so its member chips reflect this same snapshot.
  renderRoomsOverview();
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
  for (const sessionId of tabState.order) {
    const session = byId.get(sessionId);
    if (!session) {
      continue;
    }
    if (!sessionInContext(sessionId)) {
      continue; // another context's session: running, mounted, just not shown here
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
    tab.dataset.driverTag = driverTag(session.driver);

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
    tab.addEventListener("click", () => {
      if (performance.now() < suppressTabClickUntil) {
        return; // the pointerup that ended a drag
      }
      if (shellView !== "sessions") {
        setShellView("sessions");
      }
      setActiveSession(sessionId, false);
    });
    tab.addEventListener("keydown", (event) =>
      handleFocusedTabKeydown(event, sessionId),
    );
    wireTabDrag(tab, shell, sessionId);

    shell.append(
      tab,
      tabActionButton("×", "Close " + session.label, "close", sessionId),
    );
    shell.addEventListener("contextmenu", (event) => {
      event.preventDefault();
      openTabMenu(sessionId, event.clientX, event.clientY);
    });
    fragment.appendChild(shell);
  }
  sessionTabs.replaceChildren(fragment);
}

/** Drag a tab to reorder: pointer-based, 6 px threshold, the drop slot comes
    from the other tabs' midpoints, and the drop calls the same move_session
    command as the right-click menu. A press without movement is still a click. */
function wireTabDrag(
  tab: HTMLButtonElement,
  shell: HTMLElement,
  sessionId: string,
): void {
  let drag: {
    pointerId: number;
    startX: number;
    startY: number;
    active: boolean;
    slot: number;
  } | null = null;

  tab.addEventListener("pointerdown", (event) => {
    if (event.button !== 0) {
      return;
    }
    drag = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      active: false,
      slot: -1,
    };
  });
  tab.addEventListener("pointermove", (event) => {
    if (!drag || event.pointerId !== drag.pointerId) {
      return;
    }
    if (!drag.active) {
      if (Math.hypot(event.clientX - drag.startX, event.clientY - drag.startY) < 6) {
        return;
      }
      drag.active = true;
      tab.setPointerCapture(event.pointerId);
      shell.dataset.dragging = "true";
      sessionTabs.dataset.dragging = "true";
      closeTabMenu();
    }
    drag.slot = markTabDropSlot(sessionId, event.clientX);
  });
  const finish = (event: PointerEvent): void => {
    if (!drag || event.pointerId !== drag.pointerId) {
      return;
    }
    const { active, slot } = drag;
    drag = null;
    if (!active) {
      return;
    }
    delete shell.dataset.dragging;
    delete sessionTabs.dataset.dragging;
    clearTabDropSlot();
    try {
      tab.releasePointerCapture(event.pointerId);
    } catch {
      /* already released */
    }
    suppressTabClickUntil = performance.now() + 400;
    if (event.type === "pointerup" && slot >= 0) {
      void moveSessionToIndex(sessionId, slot);
    }
  };
  tab.addEventListener("pointerup", finish);
  tab.addEventListener("pointercancel", finish);
}

/** The slot among the other tabs the pointer is over (= the dragged tab's final
    index); marks the drop line on the neighbour it would land before / after. */
function markTabDropSlot(draggedId: string, clientX: number): number {
  const others = Array.from(
    sessionTabs.querySelectorAll<HTMLElement>("[data-session-tab-shell]"),
  ).filter((shell) => shell.dataset.sessionTabShell !== draggedId);
  let slot = 0;
  for (const shell of others) {
    const rect = shell.getBoundingClientRect();
    if (clientX > rect.left + rect.width / 2) {
      slot += 1;
    }
    delete shell.dataset.drop;
  }
  if (slot < others.length) {
    others[slot].dataset.drop = "before";
  } else if (others.length > 0) {
    others[others.length - 1].dataset.drop = "after";
  }
  return slot;
}

function clearTabDropSlot(): void {
  for (const shell of sessionTabs.querySelectorAll<HTMLElement>("[data-drop]")) {
    delete shell.dataset.drop;
  }
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
    tab.dataset.driverTag = driverTag(session.driver);
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
    const closeLabel = "Close " + session.label;
    close.title = closeLabel;
    close.setAttribute("aria-label", closeLabel);
  }
}

function wireTabMenu(): void {
  tabMenu.addEventListener("click", (event) => {
    if (!(event.target instanceof Element)) {
      return;
    }
    const item = event.target.closest<HTMLButtonElement>("[data-tab-menu]");
    const sessionId = tabMenuSessionId;
    if (!item || !sessionId) {
      return;
    }
    closeTabMenu();
    switch (item.dataset.tabMenu) {
      case "move":
        void moveSession(sessionId, item.dataset.delta === "-1" ? -1 : 1);
        break;
      case "edit":
        openEditSessionForm(sessionId);
        break;
      case "fresh":
        void startFreshSession(sessionId);
        break;
      case "close":
        void deleteSession(sessionId);
        break;
    }
  });
  document.addEventListener("click", (event) => {
    if (
      tabMenuSessionId
      && event.target instanceof Node
      && !tabMenu.contains(event.target)
    ) {
      closeTabMenu();
    }
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && tabMenuSessionId) {
      closeTabMenu();
    }
  });
  window.addEventListener("resize", closeTabMenu);
  sessionTabs.addEventListener("scroll", closeTabMenu);
}

function openTabMenu(sessionId: string, x: number, y: number): void {
  const session = snapshotById.get(sessionId);
  if (!session) {
    return;
  }
  setAttachMenuOpen(false);
  setCornerMenuOpen(false);
  tabMenuSessionId = sessionId;
  const index = tabState.order.indexOf(sessionId);
  const left = tabMenu.querySelector<HTMLButtonElement>(
    '[data-tab-menu="move"][data-delta="-1"]',
  );
  const right = tabMenu.querySelector<HTMLButtonElement>(
    '[data-tab-menu="move"][data-delta="1"]',
  );
  if (left) {
    left.disabled = index <= 0;
  }
  if (right) {
    right.disabled = index < 0 || index >= tabState.order.length - 1;
  }
  const title = tabMenu.querySelector<HTMLElement>("[data-tab-menu-title]");
  if (title) {
    title.textContent = session.label;
    title.title = sessionTabDescription(session);
  }
  tabMenu.hidden = false;
  const menu = tabMenu.getBoundingClientRect();
  const margin = 8;
  const top = Math.max(margin, Math.min(y, window.innerHeight - margin - menu.height));
  const clampedLeft = Math.max(margin, Math.min(x, window.innerWidth - margin - menu.width));
  tabMenu.style.top = `${Math.round(top)}px`;
  tabMenu.style.left = `${Math.round(clampedLeft)}px`;
  tabMenu.querySelector<HTMLButtonElement>("[data-tab-menu]:not(:disabled)")?.focus();
}

function closeTabMenu(): void {
  tabMenuSessionId = null;
  tabMenu.hidden = true;
}

const DRIVER_TAGS: Record<DriverKind, string> = {
  claude: "CL",
  codex: "CX",
  grok: "GK",
  prime: "PR",
  generic_terminal: "SH",
};

function driverTag(driver: DriverKind): string {
  return DRIVER_TAGS[driver] ?? "??";
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
  const visible = visibleTabOrder();
  const activeId =
    requestedId && visible.includes(requestedId)
      ? requestedId
      : visible[0] ?? null;
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
      fitVisiblePanes();
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
  const visible = visibleTabOrder();
  const currentIndex = visible.indexOf(sessionId);
  const action = resolveFocusedTabAction(
    shortcutInput(event),
    currentIndex,
    visible.length,
  );
  if (!action) {
    return;
  }
  event.preventDefault();
  if (action.kind === "move") {
    void moveSession(sessionId, action.delta);
    return;
  }
  const targetId = visible[action.index];
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
  const contextIds = visibleTabOrder();
  const visible = shouldShowZeroSession(contextIds, sessionFormMode !== null);
  zeroSession.hidden = !visible;
  zeroKicker.textContent =
    workContext.kind === "room"
      ? "No sessions in " + contextLabel()
      : "No sessions";
  // The form owns the whole area while it is open — never a form over a live terminal.
  const gridHidden = contextIds.length === 0 || sessionFormMode !== null;
  if (workspaceGrid.hidden && !gridHidden) {
    requestAnimationFrame(fitVisiblePanes);
  }
  workspaceGrid.hidden = gridHidden;
  zeroWorkspace.textContent = workspacePreference || "the workspace you choose under Custom…";
  if (visible && !zeroStateWasVisible) {
    renderAttachChoices();
    requestAnimationFrame(() => {
      zeroAttachChoices.querySelector<HTMLButtonElement>("button")?.focus();
    });
  }
  zeroStateWasVisible = visible;
}

function wireAttachMenu(): void {
  attachMenu.addEventListener("click", (event) => {
    if (!(event.target instanceof Element)) {
      return;
    }
    const action = event.target.closest<HTMLButtonElement>("[data-attach]");
    if (!action) {
      return;
    }
    void runAttachAction(action.dataset.attach ?? "");
  });
  document.addEventListener("click", (event) => {
    if (!attachMenuOpen || !(event.target instanceof Node)) {
      return;
    }
    if (
      !attachMenu.contains(event.target)
      && !(attachMenuAnchor?.contains(event.target) ?? false)
    ) {
      setAttachMenuOpen(false);
    }
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && attachMenuOpen) {
      const anchor = attachMenuAnchor;
      setAttachMenuOpen(false);
      anchor?.focus();
    }
  });
  window.addEventListener("resize", () => {
    if (attachMenuOpen) {
      placeAttachMenu();
    }
  });
}

/** The "+" popover and the empty-workspace panel show the same rows: the
    harnesses Settings left enabled, in the chosen order. Rendered at open /
    show time (DRIVER_TAGS is declared after the wiring calls run). */
function renderAttachChoices(): void {
  const drivers = quickAttachDrivers();
  attachChoices.replaceChildren(...drivers.map(attachChoiceButton));
  zeroAttachChoices.replaceChildren(...drivers.map(attachChoiceButton));
}

function attachChoiceButton(driver: DriverKind): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "corner-action attach-choice";
  button.dataset.attach = driver;
  button.setAttribute("role", "menuitem");
  const tag = document.createElement("span");
  tag.className = "attach-tag";
  tag.setAttribute("aria-hidden", "true");
  tag.textContent = driverTag(driver);
  const label = document.createElement("span");
  label.className = "corner-action-label";
  label.textContent = driverLabel(driver);
  button.append(tag, label);
  if (driver === "prime") {
    const hint = document.createElement("span");
    hint.className = "corner-action-hint";
    hint.textContent = "Ubuntu home";
    button.append(hint);
  }
  return button;
}

/** Settings → Quick attach menu: one row per harness — on/off + ↑ ↓ order. */
function renderQuickAttachSettings(): void {
  quickAttachSettings.replaceChildren(
    ...quickAttachPrefs.order.map((driver, index) => {
      const row = document.createElement("li");
      row.className = "settings-row";
      const label = document.createElement("label");
      const box = document.createElement("input");
      box.type = "checkbox";
      box.checked = !quickAttachPrefs.hidden.includes(driver);
      box.addEventListener("change", () => {
        quickAttachPrefs.hidden = box.checked
          ? quickAttachPrefs.hidden.filter((hidden) => hidden !== driver)
          : [...quickAttachPrefs.hidden, driver];
        saveQuickAttachPrefs();
        renderAttachChoices();
      });
      const tag = document.createElement("span");
      tag.className = "attach-tag";
      tag.setAttribute("aria-hidden", "true");
      tag.textContent = driverTag(driver);
      const name = document.createElement("span");
      name.className = "settings-row-label";
      name.textContent = driverLabel(driver);
      label.append(box, tag, name);
      row.append(
        label,
        quickAttachMoveButton(driver, -1, index === 0),
        quickAttachMoveButton(driver, 1, index === quickAttachPrefs.order.length - 1),
      );
      return row;
    }),
  );
}

function quickAttachMoveButton(
  driver: DriverKind,
  delta: -1 | 1,
  disabled: boolean,
): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "ghost settings-row-move";
  button.textContent = delta < 0 ? "↑" : "↓";
  const label = (delta < 0 ? "Move " : "Move ") + driverLabel(driver) + (delta < 0 ? " up" : " down");
  button.title = label;
  button.setAttribute("aria-label", label);
  button.disabled = disabled;
  button.addEventListener("click", () => {
    const order = [...quickAttachPrefs.order];
    const from = order.indexOf(driver);
    const to = from + delta;
    if (from < 0 || to < 0 || to >= order.length) {
      return;
    }
    [order[from], order[to]] = [order[to], order[from]];
    quickAttachPrefs = { ...quickAttachPrefs, order };
    saveQuickAttachPrefs();
    renderAttachChoices();
    renderQuickAttachSettings();
    quickAttachSettings
      .querySelectorAll<HTMLButtonElement>(".settings-row-move")[to * 2 + (delta < 0 ? 0 : 1)]
      ?.focus();
  });
  return button;
}

function toggleAttachMenu(anchor: HTMLElement, focusFirst = false): void {
  if (attachMenuOpen && attachMenuAnchor === anchor) {
    setAttachMenuOpen(false);
    return;
  }
  attachMenuAnchor = anchor;
  setAttachMenuOpen(true);
  if (focusFirst) {
    attachChoices.querySelector<HTMLButtonElement>("[data-attach]")?.focus();
  }
}

function setAttachMenuOpen(open: boolean): void {
  attachMenuOpen = open;
  if (open) {
    renderAttachChoices();
    setCornerMenuOpen(false);
    closeTabMenu();
    attachMenuFoot.textContent = quickAttachDrivers().length === 0
      ? "All harnesses are hidden — enable them in Settings."
      : workspacePreference
        ? "in " + workspacePreference
        : "No workspace yet — the form asks for one.";
    attachMenuFoot.title = workspacePreference;
  }
  attachMenu.hidden = !open;
  newSessionButton.setAttribute("aria-expanded", String(open));
  if (open) {
    placeAttachMenu();
  } else {
    attachMenuAnchor = null;
  }
}

function placeAttachMenu(): void {
  const anchorEl = attachMenuAnchor ?? newSessionButton;
  const anchor = anchorEl.getBoundingClientRect();
  const menu = attachMenu.getBoundingClientRect();
  const gap = 6;
  const margin = 8;
  let top = anchor.bottom + gap;
  if (top + menu.height > window.innerHeight - margin) {
    top = Math.max(margin, anchor.top - menu.height - gap);
  }
  let left = Math.min(anchor.left, window.innerWidth - margin - menu.width);
  left = Math.max(margin, left);
  attachMenu.style.top = `${Math.round(top)}px`;
  attachMenu.style.left = `${Math.round(left)}px`;
}

async function runAttachAction(kind: string): Promise<void> {
  setAttachMenuOpen(false);
  if (kind === "custom") {
    setShellView("sessions");
    openCreateSessionForm();
    return;
  }
  const driver = ATTACH_DRIVERS.find((candidate) => candidate === kind);
  if (driver) {
    await quickAttach(driver);
  }
}

/** One click, one running harness: create with the defaults (Normal profile, the
    workspace directory — Prime resolves its Ubuntu home itself), then launch.
    Without a workspace the full form opens with this harness preselected; the
    form owns the directory choice. Errors land where session errors already
    live: in the form. */
async function quickAttach(driver: DriverKind): Promise<void> {
  if (attachPending) {
    return;
  }
  setShellView("sessions");
  if (driver !== "prime" && !workspacePreference) {
    openCreateSessionForm(driver);
    return;
  }
  if (sessionFormMode) {
    closeSessionForm();
  }
  setAttachPending(true);
  let created: SessionSnapshot;
  try {
    const request = createSessionRequestFromForm("", driver, "normal", "");
    if (workContext.kind === "room") {
      request.room_id = workContext.roomId; // born into the room you stand in
    }
    created = await command<SessionSnapshot>("create_session", { request });
  } catch (error) {
    setAttachPending(false);
    openCreateSessionForm(driver);
    setSessionFormError("Create session failed: " + String(error));
    return;
  }
  try {
    await refreshSnapshot(created.session_id);
  } catch (error) {
    setAttachPending(false);
    openCreateSessionForm(driver);
    setSessionFormError(
      "Session "
        + shortSessionId(created.session_id)
        + " was created, but inventory refresh failed. Do not create a duplicate; use Refresh state. "
        + String(error),
    );
    return;
  }
  setAttachPending(false);
  await launchSession(created.session_id);
}

function setAttachPending(pending: boolean): void {
  attachPending = pending;
  newSessionButton.disabled = pending;
  for (const button of zeroSession.querySelectorAll<HTMLButtonElement>("[data-attach]")) {
    button.disabled = pending;
  }
  if (pending) {
    newSessionButton.dataset.busy = "true";
  } else {
    delete newSessionButton.dataset.busy;
  }
}

/** Creating is half the job — the harness has to run. Same command as the pane's
    Start button; a failure leaves the stopped session with that button as the retry. */
async function launchSession(sessionId: string): Promise<void> {
  try {
    const snapshot = await command<SessionSnapshot>("start_session", {
      request: { session_id: sessionId },
    });
    applyCommandSessionSnapshot(snapshot);
    setActiveSession(sessionId, true);
  } catch (error) {
    writeSystem("error", `launch ${paneLabel(sessionId)} failed: ${String(error)}`);
  }
}

function openCreateSessionForm(driver?: DriverKind): void {
  primeDefaultRequest += 1;
  const defaults = newSessionFormDefaults(workspacePreference);
  sessionFormMode = { kind: "create" };
  sessionFormPending = false;
  sessionLabelInput.value = "";
  sessionDriverSelect.value = driver ?? defaults.driver;
  sessionPermissionSelect.value = defaults.permissionProfile;
  sessionLinuxWorkingDirectory.value = "";
  setSessionFormError(null);
  syncSessionForm();
  if (sessionDriverSelect.value === "prime") {
    // A preselected Prime gets the same Ubuntu-home discovery a manual pick triggers.
    void selectSessionDriver();
  }
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
  zeroWorkspace.textContent = workspacePreference || "the workspace you choose under Custom…";
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
    saveSessionButton.textContent = "Create & launch";
    sessionFormNote.textContent = prime
      ? "Prime runs directly in Ubuntu WSL. Enter an absolute Linux path, or leave blank to use the qualified Ubuntu home. The session launches as soon as it is created."
      : workspacePreference
        ? "Browse changes the workspace default for this and future new sessions. The session launches as soon as it is created."
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
  const flags = PROFILE_FLAGS[driver];
  permissionNote.textContent = flags.unsafe
    ? "Normal passes " + flags.normal + " · Unsafe passes " + flags.unsafe + "."
    : driverLabel(driver) + " runs with " + flags.normal + "; Unsafe is not available.";
}

/** What each driver actually passes per profile (crates/driver-*); the form
    note and the Help page quote this table. */
const PROFILE_FLAGS: Record<DriverKind, { normal: string; unsafe: string | null }> = {
  claude: {
    normal: "--permission-mode manual",
    unsafe: "--dangerously-skip-permissions",
  },
  codex: {
    normal: "--ask-for-approval on-request --sandbox workspace-write",
    unsafe: "--dangerously-bypass-approvals-and-sandbox",
  },
  grok: {
    normal: "--permission-mode default",
    unsafe: "--permission-mode bypassPermissions",
  },
  prime: { normal: "no permission flags", unsafe: null },
  generic_terminal: { normal: "no permission flags", unsafe: null },
};

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
      const request = createSessionRequestFromForm(
        label,
        driver,
        permissionProfile,
        linuxWorkingDirectory,
      );
      if (workContext.kind === "room") {
        request.room_id = workContext.roomId; // born into the room you stand in
      }
      created = await command<SessionSnapshot>("create_session", { request });
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
          + " was created, but inventory refresh failed. Do not create a duplicate; use Refresh state. "
          + String(error),
      );
      syncSessionForm();
      return;
    }
    await launchSession(created.session_id);
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
  await moveSessionToIndex(sessionId, nextOrder.indexOf(sessionId));
  sessionTabs
    .querySelector<HTMLButtonElement>(
      '[data-session-tab="' + sessionId + '"]',
    )
    ?.focus();
}

async function moveSessionToIndex(sessionId: string, newIndex: number): Promise<void> {
  if (newIndex < 0 || newIndex === tabState.order.indexOf(sessionId)) {
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
  } catch (error) {
    writeSystem(
      "error",
      "move " + paneLabel(sessionId) + " failed: " + String(error),
    );
  }
}

/** × does what a person would do by hand: leave the room the session sits in,
    stop it, then delete it — one confirmation. The same three commands as the
    room's "remove", the pane's Stop and the old close; the supervisor's rules
    (a room member and a running session cannot be deleted) are unchanged. */
async function deleteSession(sessionId: string): Promise<void> {
  const session = snapshotById.get(sessionId);
  if (!session) {
    return;
  }
  const room = roomOfSession(sessionId);
  const consequences: string[] = [];
  if (session.running) {
    consequences.push("stops it");
  }
  if (room) {
    consequences.push('removes it from room "' + room.label + '"');
  }
  const confirmed = window.confirm(
    'Close "' + session.label + '" (' + shortSessionId(sessionId) + ")?"
      + (consequences.length > 0 ? " This " + consequences.join(" and ") + "." : "")
      + " Its terminal scrollback will be discarded.",
  );
  if (!confirmed) {
    return;
  }
  try {
    if (room) {
      await command<RoomSnapshot>("remove_room_member", {
        request: {
          room_id: room.room_id,
          session_id: sessionId,
        } satisfies RemoveRoomMemberRequest,
      });
    }
    if (session.running) {
      const stopped = await command<SessionSnapshot>("stop_session", {
        request: { session_id: sessionId },
      });
      applyCommandSessionSnapshot(stopped);
    }
    await deleteSessionOnceClosed(sessionId);
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
    // A direct action that fails has to say so where the person is looking.
    setLogDrawer(true);
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

function roomOfSession(sessionId: string): RoomSnapshot | null {
  return roomSnapshots.find((room) => room.member_ids.includes(sessionId)) ?? null;
}

/** The supervisor deletes only a session that is fully closed with its event
    stream drained; right after a stop that takes a moment ("retry deletion" is
    its own wording). Retry those two transient refusals, nothing else. */
async function deleteSessionOnceClosed(sessionId: string): Promise<void> {
  const deadline = Date.now() + 15_000;
  for (;;) {
    try {
      await command<void>("delete_session", {
        request: { session_id: sessionId } satisfies DeleteSessionRequest,
      });
      return;
    } catch (error) {
      const text = String(error);
      const transient =
        text.includes("must be fully closed")
        || text.includes("still draining");
      if (!transient || Date.now() >= deadline) {
        throw error;
      }
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
  }
}

/** The canonical room brief, fetched once — prefills the creation form. */
let defaultRoomBrief: string | null = null;

async function loadDefaultRoomBrief(): Promise<string> {
  if (defaultRoomBrief === null) {
    try {
      defaultRoomBrief = await command<string>("default_room_brief");
    } catch {
      return "";
    }
  }
  return defaultRoomBrief;
}

function openRoomCreateForm(): void {
  roomCreateForm.hidden = false;
  roomLabelInput.value = "";
  roomCreateError.hidden = true;
  roomCreateError.textContent = "";
  roomBriefAuto.checked = true;
  roomBriefTemplate.value = defaultRoomBrief ?? "";
  if (defaultRoomBrief === null) {
    void loadDefaultRoomBrief().then((text) => {
      if (!roomCreateForm.hidden && roomBriefTemplate.value === "") {
        roomBriefTemplate.value = text;
      }
    });
  }
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
  const label = roomLabelInput.value.trim();
  try {
    const briefText = roomBriefTemplate.value;
    const room = await command<RoomSnapshot>("create_room", {
      request: {
        label: label || null,
        member_ids: memberIds,
        brief_on_join: roomBriefAuto.checked,
        // Unchanged or cleared = the canonical brief, kept as null so future
        // improvements to the default text reach this room too.
        brief_template:
          briefText.trim() === "" || briefText === defaultRoomBrief ? null : briefText,
      } satisfies CreateRoomRequest,
    });
    closeRoomCreateForm();
    await refreshSnapshot(tabState.activeId ?? undefined);
    // A fresh room is where you're headed: enter it (empty is fine — "+"
    // inside spawns harnesses already in the room).
    enterWorkContext({ kind: "room", roomId: room.room_id });
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
    roomSendButton.disabled = (activeRoom()?.member_ids.length ?? 0) < 1;
  }
}

async function deliverActiveRoomMessage(): Promise<void> {
  const room = activeRoom();
  const content = roomMessage.value;
  if (!room || !content.trim()) {
    setRoomStatus("Enter a message before sending.", "warn");
    return;
  }
  if (room.member_ids.length === 0) {
    setRoomStatus("This room has no members yet — add one before sending.", "warn");
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
    // Deliver to the deliverable, report the refused — by name, with the reason.
    const refused = result.failures
      .map((failure) => `${paneLabel(failure.recipient_id)} (${failure.error.slice(0, 120)})`)
      .join("; ");
    const resultMessage = result.failures.length === 0
      ? `PTY write completed for ${result.written_count}/${result.recipient_count}; model receipt remains unconfirmed.`
      : `${result.written_count}/${result.recipient_count} PTY writes completed; refused: ${refused}. See feed details.`;
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
    roomSendButton.disabled = (activeRoom()?.member_ids.length ?? 0) < 1;
  }
}

function setRoomStatus(message: string, level: "info" | "warn" | "error"): void {
  roomStatus.textContent = message;
  roomStatus.dataset.level = level;
}

/** "Brief now": type the canonical room brief into one member's terminal on
    demand — the same text the join path delivers. */
async function briefRoomMemberNow(roomId: string, sessionId: string): Promise<void> {
  try {
    await command<RoomDeliveryResult>("brief_room_member", {
      request: {
        room_id: roomId,
        session_id: sessionId,
      } satisfies BriefRoomMemberRequest,
    });
    setRoomStatus(`Room brief sent to ${paneLabel(sessionId)}.`, "info");
  } catch (error) {
    setRoomStatus(`Brief ${paneLabel(sessionId)} failed: ${String(error)}`, "error");
  }
}

function wireRoomUi(): void {
  void loadDefaultRoomBrief();
  newRoomButton.addEventListener("click", openRoomCreateForm);
  cancelRoomCreate.addEventListener("click", closeRoomCreateForm);
  roomCreateForm.addEventListener("submit", (event) => {
    event.preventDefault();
    void createRoomFromForm();
  });
  roomPostButton.title = "Append to the shared feed — no harness is prompted";
  roomSendButton.title = "Type the message into the selected member terminals";
  roomsBackButton.addEventListener("click", openRoomsOverview);
  contextRoomsChip.addEventListener("click", openRoomsOverview);
  contextNameChip.addEventListener("click", () => {
    if (workContext.kind === "room") {
      openRoomPanel(workContext.roomId);
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
  newSessionButton.addEventListener("click", () => {
    toggleAttachMenu(newSessionButton);
  });
  zeroSession.addEventListener("click", (event) => {
    if (!(event.target instanceof Element)) {
      return;
    }
    const action = event.target.closest<HTMLButtonElement>("[data-attach]");
    if (action) {
      void runAttachAction(action.dataset.attach ?? "");
    }
  });
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
      case "fresh":
        void startFreshSession(sessionId);
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
          writeSystem("info", "state refreshed");
          break;
        }
      }
    });
  }
}

function wireResize(): void {
  // Both observers fire per frame during a window drag; every fit reflows
  // xterm AND resizes ConPTY, which re-emits its whole re-wrapped screen each
  // time. One settle-fit per drag is the contract ConPTY behaves under.
  let settle: number | undefined;
  const refit = () => {
    if (settle !== undefined) {
      window.clearTimeout(settle);
    }
    settle = window.setTimeout(() => {
      settle = undefined;
      fitVisiblePanes();
      systemFit.fit();
    }, 120);
  };
  const observer = new ResizeObserver(refit);
  observer.observe(document.body);
  window.addEventListener("resize", refit);
}

function fitVisiblePanes(): void {
  const active = tabState.activeId ? paneMap.get(tabState.activeId) : undefined;
  const measured = active?.measure() ?? null;
  if (!measured) {
    return;
  }
  for (const pane of paneMap.values()) {
    pane.applyGrid(measured.cols, measured.rows);
  }
}

function isPaneVisible(sessionId: string): boolean {
  return tabState.activeId === sessionId;
}

function wireTerminalShortcuts(): void {
  window.addEventListener(
    "keydown",
    (event) => {
      if (event.key === "F1") {
        event.preventDefault();
        runCornerAction("help");
        return;
      }
      if (event.key !== "F11") {
        return;
      }

      event.preventDefault();
      void toggleFullscreen();
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
        toggleAttachMenu(newSessionButton, true);
        return;
      }
      if (action.kind === "close-session") {
        if (tabState.activeId) {
          void deleteSession(tabState.activeId);
        }
        return;
      }
      const targetId = relativeSessionId(
        visibleTabOrder(),
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
  if (level === "error" && appShell.dataset.log !== "open") {
    cornerButton.dataset.alert = "true";
  }
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
