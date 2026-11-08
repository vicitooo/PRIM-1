import { RoomCardIndex, cardKey, cardReply, typedCardSignature, selectionTouchesNode, buildTypedCard, updateTypedCard, disposeTypedCard, type CardEvent } from "./room-cards";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LogicalSize } from "@tauri-apps/api/dpi";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import { InitialTerminalBanner } from "./terminal-banner";
import { windowRectAction, type WindowRectState } from "./window-rect";
import { focusWithinActiveWindow } from "./focus-guard";
import { installSessionInventory, type SessionInventory } from "./session-inventory";
import {
  initialTerminalGrid,
  InitialReplayFence,
  repaintTerminalGrid,
  RunGridRecovery,
  terminalGridErrorAllowsCompletion,
  terminalHostIsVisible,
  TerminalLayoutScheduler,
  TerminalResizeState,
  TERMINAL_SCROLLBACK_LINES,
} from "./terminal-layout";
import { RendererOutputCredit } from "./output-credit";
import { completeDesktopStartup, desktopCommandAllowed, loadTrialStatus, TRIAL_HOLD_TEXT, TRIAL_STATUS_UNAVAILABLE_TEXT, type TrialStatus } from "./trial-startup";
import { personalRepoLocked, personalRepoStateText, type PersonalRepoStatus } from "./personal-repo";
import { canStopSession, idleHibernateMinutesFromInput, isSleeping, readIdleHibernateMinutes, renderIdleHibernationCapability } from "./hibernation";
import {
  TASK_DETAIL_QUERY,
  TASK_LIST_QUERY,
  TASK_STATE_LABELS,
  TASK_STATE_ORDER,
  TASK_STATES_START_CLOSED,
  TASKS_READ_ONLY_NOTE,
  UNREADABLE_ADVICE,
  errorText,
  formatTaskTime,
  groupHeading,
  mergeUnreadable,
  nextOffset,
  orderedGroups,
  pageRangeLabel,
  parseTaskDetail,
  parseTaskPage,
  previousOffset,
  taskFolderProblem,
  taskFolderStateText,
  unreadableSummary,
  type TaskDetail,
  type TaskFolderStatus,
  type TaskGroupPage,
  type TaskState,
  type UnreadableTaskFile,
} from "./tasks-panel";
import "@xterm/xterm/css/xterm.css";

import { exposeAutomationBridge } from "./automation-bridge";
import { PerfProbes, exposePerfProbes } from "./perf-probes";
import {
  BINDABLE_ACTIONS,
  DEFAULT_BINDINGS,
  bindableAction,
  captureChord,
  chordsFor,
  describeChord,
  detectPlatform,
  helpRows,
  ownChords,
  parseUserBindings,
  resolveKeyMatch,
  sameChord,
  serializeUserBindings,
  type BindableAction,
  type Platform,
  type UserBindings,
} from "./keymap";
import {
  KEY_TABLE_DRIVERS,
  describeKeyBytes,
  harnessKeyPlan,
  harnessKeyTranslation,
  type HarnessKeyTranslation,
} from "./harness-keys";
import { planPaste } from "./paste";

import {
  resolveCopySelection,
  type CopySelectionResult,
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
import { prelaunchView, searchPrompt } from "./pane-prelaunch";
import {
  canUseUnsafePermission,
  acceptsCodexTransport,
  acceptsLaunchChoices,
  acceptsMemberGrants,
  createGrantsFromForm,
  createSessionRequestFromForm,
  driverLabel,
  isSupervisorGrants,
  launchChoicesFromForm,
  memberGrantsNote,
  sameLaunchChoices,
  type BriefMode,
  lifecycleLabel,
  sessionDisplayState,
  movedSessionOrder,
  newSessionFormDefaults,
  permissionProfileForDriver,
  reconcileSessionTabs,
  relativeSessionId,
  resolveFocusedTabAction,
  sessionTabDescription,
  shouldShowZeroSession,
  shortSessionId,
  unsafePermissionWarning,
  type SessionTabsState,
} from "./session-tabs";
import {
  appendRoomFeedEvent as reconcileRoomFeedEvent,
  retainRoomFeedWindow,
  retryableRoomDeliveryEvents,
  roomDeliveryFeedback,
  roomDeliveryRetryKey,
  roomDeliveryReceiptLabel,
  deliveryNotNeeded,
  roomMessageIntent,
  submitRoomIntent,
  roomRequestFailure,
  roomRequestFailureFeedback,
  roomDeliveryRetryUnavailable,
  roomRetryFeedback,
  roomWakeUnavailable,
  roomHistoryReplayMessage,
  roomWriterRecoveryMessage,
  roomFallbackSenderLabel,
  roomRecoverySenderLabel,
  roomNoticeSenderLabel,
  roomGuestLabel,
  roomMessageRecipientsLabel,
  roomOpenObligations,
  roomWithdrawalText,
  roomWithdrawalNotice,
  roomWithdrawPreview,
  roomMessageReached,
} from "./room-feed";
import {
  readableProblemText,
  readableStatesText,
  readableRecipients,
  readableRoomFeed,
  readableTime,
  type ReadableFeedItem,
  type ReadableProblem,
  type RoomFeedView,
} from "./readable-feed";
import { reconcileKeyedChildren, retainEngagedChildren, type KeyedChild, type KeyedParent } from "./keyed-children";
import {
  PRIM1_POST_REQUEST_PREFIX,
  contextDisplayLabel,
  contextKey,
  parseWorkContext,
  restartNotice,
  restartResumePlan,
  enqueueBackgroundResumes,
  resumeLaunchCommand,
  roomSelectionAfterSync,
  sessionInContext as sessionInContextPure,
  sessionReadiness,
  sessionRoomId as sessionRoomIdPure,
  visibleSessionOrder,
  type WorkContext,
  type BackgroundResumeEntry,
  type BackgroundResumeMode,
} from "./work-context";
import { loadRoomFlow, roomSaveSummary } from "./room-package";
import { afterConfirm, closeSessionConfirmText, deleteRoomConfirmText, resumeHereConfirmText, startFreshConfirmText } from "./confirm-gate";
import "./styles.css";
import { RoomDraftStore } from "./room-drafts";
import { installRoomFallbacks } from "./room-fallbacks";
import { abandonUpgradeConfirmText, upgradeBannerView, upgradeResumeMembers } from "./upgrade-banner";
import { SgrColourFilter } from "./sgr-filter";
import {
  TELEMETRY_POLL_MS,
  TelemetryPoller,
  formatBytes,
  formatPercent,
  telemetryFootnote,
  telemetryLine,
  telemetryLineParts,
  telemetryLineTitle,
  telemetryRows,
  telemetryTotalRow,
  type TelemetryRow,
} from "./telemetry";
import type {
  AddRoomMemberRequest,
  BriefRoomMemberRequest,
  ChooseSessionWorkingDirectoryRequest,
  CreateRoomRequest,
  DeleteRoomRequest,
  DeleteSessionRequest,
  DeliverRoomMessageRequest,
  DriverKind,
  InviteRoomGuestRequest,
  MoveRoomRequest,
  MoveSessionRequest,
  PermissionProfile,
  RevokeRoomGuestRequest,
  RoomGuest,
  RoomGuestInvitationView,
  PostRoomMessageRequest,
  ReadRoomFeedRequest,
  RemoveRoomMemberRequest,
  RenameRoomRequest,
  RetryRoomDeliveryRequest,
  SetRoomResumeRequest,
  RoomBriefSettings,
  ReadRoomBriefRequest,
  UpdateRoomBriefRequest,
  RoomDeliveryResult,
  RoomExportReport,
  RoomImportReport,
  RoomSourceSummary,
  RoomFeedCursor,
  RoomFeedEvent,
  RoomFeedPage,
  RoomPostResult,
  RoomSnapshot,
  RenameSessionRequest,
  RuntimeEvent,
  RuntimeSnapshot,
  SendInputRequest,
  SetMemberPermissionsRequest,
  SetSessionLinuxWorkingDirectoryRequest,
  SetSessionPermissionRequest,
  SessionSnapshot,
  MemberPermissions,
  SessionLaunchChoices,
  StartSessionRequest,
  StopSessionRequest,
  TelemetrySnapshot,
  UpgradeStatus,
} from "./types";

type SessionFormMode =
  | { kind: "create" }
  | { kind: "edit"; sessionId: string };

// Undefined blocks mutation commands until the backend explicitly says normal.
let trialStatus: TrialStatus | null | undefined;

const app = document.querySelector("#app");
if (!(app instanceof HTMLDivElement)) {
  throw new Error("Missing #app root element");
}

/** Decided once at boot from the webview's platform hints (see keymap.ts). */
const keyPlatform: Platform = detectPlatform();
const telemetryPageHint = keyPlatform === "macos"
  ? "Each process is counted once: PRIM-1 itself, then each live agent's whole process tree. PRIM-1's WebKit webview runs in processes macOS owns, so it is not measured here. CPU is the share of the whole Mac since the previous sample. Memory is each process's physical footprint (Activity Monitor › Memory). Peaks run from each agent's start; CPU peaks are sampled only while telemetry is on screen."
  : "Each process is counted once: PRIM-1 itself, its WebView2 window, the console host beside each terminal, then each live agent's whole process tree. CPU is the share of the whole PC since the previous sample (Task Manager › Processes). Memory is private bytes (Task Manager › Details › Commit size). Peaks run from each agent's start; CPU peaks are sampled only while telemetry is on screen.";

/** Settings → Keys: the user's explicit bindings, validated on load
    (keymap.ts parseUserBindings). One localStorage key. */
const USER_BINDINGS_KEY = "prim1-keys";
let userBindings: UserBindings = loadUserBindings();
/** While set, the next key press is that action's new chord (Settings → Keys). */
let keyCapture: { action: BindableAction; button: HTMLButtonElement } | null = null;

function loadUserBindings(): UserBindings {
  try {
    return parseUserBindings(localStorage.getItem(USER_BINDINGS_KEY), keyPlatform);
  } catch {
    /* storage unavailable — defaults */
    return DEFAULT_BINDINGS;
  }
}

function saveUserBindings(): void {
  try {
    const raw = serializeUserBindings(userBindings, keyPlatform);
    if (raw === null) {
      localStorage.removeItem(USER_BINDINGS_KEY);
    } else {
      localStorage.setItem(USER_BINDINGS_KEY, raw);
    }
  } catch {
    /* storage unavailable — the binding just won't persist */
  }
}

function keyboardPlatformLabel(platform: Platform): string {
  switch (platform) {
    case "windows":
      return "Windows";
    case "linux":
      return "Linux";
    case "macos":
      return "macOS";
  }
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function kbdMarkup(label: string): string {
  return label
    .split("+")
    .map((part) => `<kbd>${escapeHtml(part)}</kbd>`)
    .join("+");
}

/** The F1 "Keyboard" rows, generated from the keymap so they cannot drift. */
function keyboardHelpMarkup(platform: Platform): string {
  return helpRows(platform, userBindings)
    .map((row) => {
      const chords = row.chords
        .map((chord) =>
          chord.note
            ? `${kbdMarkup(chord.label)} <small>(${escapeHtml(chord.note)})</small>`
            : kbdMarkup(chord.label),
        )
        .join(" / ");
      return `<dt>${chords}</dt><dd>${escapeHtml(row.help)}</dd>`;
    })
    .join("");
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
    <!-- A108: a self-upgrade's notice, progress or hold; above everything, out of the grid. -->
    <div class="upgrade-banner" id="upgrade-banner" role="status" aria-live="polite" hidden>
      <strong id="upgrade-banner-title"></strong>
      <span class="upgrade-banner-countdown" id="upgrade-banner-countdown"></span>
      <span id="upgrade-banner-body"></span>
      <div class="upgrade-banner-actions">
        <button type="button" class="ghost" id="upgrade-cancel" hidden>Cancel upgrade</button>
        <button type="button" class="ghost" id="upgrade-abandon" hidden>Abandon held upgrade</button>
        <button type="button" class="ghost" id="upgrade-dismiss" hidden>Dismiss</button>
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
        <div class="session-tabs" id="session-tabs" role="tablist" aria-label="Sessions"></div>
        <button type="button" class="new-session-button" id="new-session-button" aria-haspopup="menu" aria-expanded="false" aria-label="Attach a harness" title="Attach a harness — Ctrl+Shift+T">+</button>
      </nav>
      <div class="topbar-status" data-tauri-drag-region>
        <button type="button" class="telemetry-line" id="telemetry-line"></button>
        <span id="trial-hold" class="trial-hold" role="status" hidden></span>
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
          <label id="session-codex-transport-field" hidden>
            <span>Codex connection <small id="session-codex-transport-note"></small></span>
            <select id="session-codex-transport">
              <option value="pty">Terminal (default)</option>
              <option value="app_server">Codex App Server (opt-in)</option>
            </select>
          </label>
          <fieldset class="session-launch-choices" id="session-launch-choices">
            <legend>Launch choices <small>what this agent asks its harness for at launch; the harness decides what actually runs</small></legend>
            <label>
              <span>Model <small>blank = harness default</small></span>
              <input id="session-model" type="text" maxlength="200" autocomplete="off" spellcheck="false" />
            </label>
            <label>
              <span>Effort <small>blank = harness default</small></span>
              <input id="session-effort" type="text" maxlength="40" autocomplete="off" spellcheck="false" list="session-effort-options" />
              <datalist id="session-effort-options">
                <option value="low"></option>
                <option value="medium"></option>
                <option value="high"></option>
                <option value="xhigh"></option>
                <option value="max"></option>
              </datalist>
            </label>
            <label>
              <span>Starting brief</span>
              <select id="session-brief-mode">
                <option value="room_default">Room default — the room's brief, when the room has it on</option>
                <option value="custom">Custom text</option>
                <option value="none">None — no task prompt</option>
              </select>
            </label>
            <label id="session-brief-custom-field" hidden>
              <span>Custom brief <small>the first prompt of a fresh conversation; a resume sends none</small></span>
              <textarea id="session-brief-custom" rows="5" maxlength="8192" spellcheck="false"></textarea>
            </label>
            <p class="session-form-note" id="session-choices-note"></p>
          </fieldset>
          <fieldset class="session-launch-choices session-member-grants" id="session-member-grants" hidden>
            <legend>Beyond its room <small>saved with Save; no restart needed</small></legend>
            <label class="session-grant" for="session-grant-supervisor" id="session-grant-supervisor-field">
              <input type="checkbox" id="session-grant-supervisor" />
              <span>Supervisor <small>ticks all three below: it may roam, upgrade PRIM-1 and manage agents; a worker needs none</small></span>
            </label>
            <label class="session-grant" for="session-grant-roam">
              <input type="checkbox" id="session-grant-roam" />
              <span>Can roam <small>list every room, read any feed, join and leave rooms, message members of other rooms</small></span>
            </label>
            <label class="session-grant" for="session-grant-maintain">
              <input type="checkbox" id="session-grant-maintain" />
              <span>Can upgrade PRIM-1 <small>ask PRIM-1 to restart on a tested build after a notice you can cancel; every running member resumes its own conversation</small></span>
            </label>
            <label class="session-grant" for="session-grant-manage" id="session-grant-manage-field">
              <input type="checkbox" id="session-grant-manage" />
              <span>Can manage agents <small>everything you can do to an agent: add, start, stop, edit, grant, brief, remove and close members of its room (another room or the lobby also needs Can roam)</small></span>
            </label>
            <p class="session-form-note" id="session-grants-note"></p>
          </fieldset>
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
        <p id="rooms-load-problem" class="session-form-error" role="alert" hidden></p>
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
            <button type="button" class="ghost" id="save-room" title="Save this room, its members and its feed to a .prim1room file">Save…</button>
            <button type="button" class="ghost" id="move-room-left" aria-label="Move room left" hidden>←</button>
            <button type="button" class="ghost" id="move-room-right" aria-label="Move room right" hidden>→</button>
            <button type="button" class="ghost danger" id="delete-room">Delete</button>
            <label class="room-resume-toggle" for="room-resume-toggle" title="When PRIM-1 restarts, this room's agents that were running come back. Rooms you do not mark stay asleep until you wake them.">
              <input type="checkbox" id="room-resume-toggle" />
              <span>Resume on restart</span>
            </label>
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
            <span>Use this room brief by default when starting a fresh agent conversation. Joining, idling, and resuming a stored conversation do not send it. Individual launch choices can use a custom brief or no brief.</span>
          </label>
          <label class="room-brief-edit">
            <span>Room brief</span>
            <textarea id="room-brief-template" rows="8" maxlength="8192" spellcheck="false"></textarea>
            <small>{room_label}, {members}, {your_label}, {tools} and {roaming} (empty unless the member may roam) are filled in per member. Clear the box to use the standard brief.</small>
          </label>
          <p id="room-create-error" class="session-form-error" role="alert" hidden></p>
          <div class="room-form-actions">
            <button type="submit" class="primary">Create room</button>
            <button type="button" class="ghost" id="cancel-room-create">Cancel</button>
          </div>
        </form>
        <div id="room-empty" class="room-empty">
          <p>No rooms yet. Create one — empty or from existing sessions; no harness will be started or replaced.</p>
          <button type="button" class="ghost" data-room-load>Load room…</button>
        </div>
        <div id="room-content" class="room-content" hidden>
          <details id="room-brief-settings" class="room-brief-settings">
            <summary>Room brief</summary>
            <label class="room-brief-auto" for="room-brief-on-launch">
              <input type="checkbox" id="room-brief-on-launch" />
              <span>Use this room brief by default when a member starts a fresh conversation (its brief set to "room default"). Resuming a stored conversation sends no brief, and saving never types into a running terminal.</span>
            </label>
            <textarea id="room-brief-text" rows="8" maxlength="8192" spellcheck="false" aria-label="Room brief text"></textarea>
            <div class="room-brief-actions">
              <button type="button" id="room-brief-save">Save brief</button>
              <button type="button" class="ghost" id="room-brief-default">Use the default text</button>
              <span id="room-brief-state" class="room-brief-state"></span>
            </div>
          </details>
          <div id="room-resume-note" class="room-resume-note" hidden>
            <span id="room-resume-note-text"></span>
            <button type="button" id="room-resume-now">Resume</button>
          </div>
          <div class="room-members-row">
            <div class="room-members-head">
              <span>Room settings</span>
              <button type="button" class="ghost room-members-close" id="room-members-close" title="Close room settings" aria-label="Close room settings">&times;</button>
            </div>
            <div class="room-side-body">
              <details id="room-members-section" class="room-side-section" open>
                <summary>Members · <span id="room-members-count" class="room-members-count"></span></summary>
                <div class="room-side-section-body">
                  <div id="room-members" class="room-members" aria-label="Room members"></div>
                  <select id="room-add-member-select" aria-label="Session to add"></select>
                  <button type="button" id="room-add-member">Add member</button>
                  <div class="room-guests">
                    <div class="room-members-head">
                      <span>Outside agents <span id="room-guests-count" class="room-members-count"></span></span>
                    </div>
                    <div id="room-guest-list" class="room-guest-list" aria-label="Outside agents"></div>
                    <button type="button" class="ghost" id="room-invite-guest" title="Let an agent outside PRIM-1 (Claude desktop, Codex, Cursor, a script) read this room and post to it">Invite an outside agent</button>
                  </div>
                </div>
              </details>
              <details id="room-fallback-settings" class="room-side-section">
                <summary>Fallbacks · <span id="room-fallback-summary">Not loaded</span></summary>
                <div id="room-fallback-editor" class="room-side-section-body room-fallback-editor"></div>
              </details>
            </div>
          </div>
          <button type="button" class="ghost room-members-reopen" id="room-members-open" hidden title="Show room settings">Room settings</button>
          <div id="room-feed" class="room-feed" role="log" aria-live="polite"></div>
          <label class="room-composer-label" for="room-message">Message</label>
          <textarea id="room-message" rows="3" maxlength="1048576" placeholder="Post to the shared feed or deliver explicitly…"></textarea>
          <div class="room-composer-actions">
            <button type="button" id="room-post">Post to feed</button>
            <select id="room-recipient" aria-label="Room delivery recipients"></select>
            <button type="button" class="primary" id="room-send">Send</button>
          </div>
          <p id="room-history-replay" class="room-status" data-level="warn" aria-live="polite" hidden></p>
          <div id="room-status" class="room-status" aria-live="polite">
            <span id="room-status-summary"></span>
            <details id="room-status-details" hidden>
              <summary aria-label="Delivery result details">Details</summary>
              <p id="room-status-detail-text"></p>
            </details>
          </div>
        </div>
      </article>
    </section>

    <!-- Tasks view (corner menu → Tasks). Read-only: agents keep tasks through
         their pane tool; people read them here. No edit controls. -->
    <section class="tasks-view" id="tasks-view" hidden aria-label="Tasks">
      <article class="panel view-card">
        <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
        <div class="card-head view-head">
          <div class="card-title">
            <h2>Tasks</h2>
          </div>
          <div class="tasks-head-actions">
            <button type="button" class="ghost" id="tasks-refresh">Refresh</button>
            <button type="button" class="ghost view-back" data-corner="tasks">Back to sessions</button>
          </div>
        </div>
        <div class="view-body tasks-body">
          <div class="tasks-top">
            <p class="view-hint tasks-note">${TASKS_READ_ONLY_NOTE} <span class="tasks-folder" id="tasks-folder"></span></p>
            <div class="tasks-problem" id="tasks-problem" role="alert" hidden>
              <p id="tasks-problem-text"></p>
              <button type="button" class="ghost" id="tasks-open-settings">Open Settings</button>
            </div>
            <details class="tasks-unreadable" id="tasks-unreadable" hidden>
              <summary id="tasks-unreadable-summary"></summary>
              <ul id="tasks-unreadable-list"></ul>
              <p class="view-hint">${UNREADABLE_ADVICE}</p>
            </details>
          </div>
          <div class="tasks-layout">
            <section class="tasks-list" id="tasks-list" aria-label="Tasks by state"></section>
            <section class="tasks-detail" id="tasks-detail" aria-label="Task" aria-live="polite"></section>
          </div>
          <p class="tasks-status" id="tasks-status" role="status" aria-live="polite"></p>
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
            <h3>Keys</h3>
            <p class="view-hint">One key per action for every harness: PRIM-1 sends each harness its own keys for it. Click a key, then press the new one (<kbd>Esc</kbd> cancels); a key that also copies or pastes by convention is taken over. Unverified and unsupported actions reach the harness as typed, as does every key not listed — so in a Terminal pane the paste-image key pastes text.</p>
            <div class="keys-table-scroll">
              <table class="keys-table" id="keys-table"></table>
            </div>
            <div class="keys-foot">
              <p class="keys-status" id="keys-status" role="status" aria-live="polite"></p>
              <button type="button" class="ghost" id="keys-reset">Reset to defaults</button>
            </div>
          </section>
          <section class="view-section">
            <h3>Startup</h3>
            <p class="view-hint">Which rooms come back after a restart is set on each room: open the room and tick <b>Resume on restart</b>. Their agents that were running relaunch in the background; every other agent stays asleep until you wake it. Visiting a room never starts its agents.</p>
            <div id="idle-hibernation-controls" hidden>
            <div class="settings-row">
              <label><input id="idle-hibernation-enabled" type="checkbox" data-trial-mutation disabled /><span class="settings-row-label">Sleep idle room members</span></label>
            </div>
            <div class="task-folder-form">
              <label for="idle-hibernation-minutes">Idle minutes</label>
              <input id="idle-hibernation-minutes" type="number" min="1" max="1440" step="1" value="60" data-trial-mutation disabled />
              <button type="button" id="idle-hibernation-save" data-trial-mutation disabled>Save</button>
            </div>
            <p class="view-hint">Off until enabled. Eligible idle members keep their conversation and free process memory. A new room message or Wake resumes them; Stop keeps them stopped.</p>
            <p id="idle-hibernation-status" class="task-folder-result" role="status" aria-live="polite">Waiting for the idle hibernation setting…</p>
            </div>
          </section>
          <section class="view-section">
            <h3>Task folder</h3>
            <p class="view-hint">Where agents keep the task list, one file per task; a folder in a git repository works well. Until you choose one, PRIM-1 uses a <code>tasks</code> folder in the personal repository below (the workspace unless you choose another). Choosing another folder switches to it; nothing is copied or moved, and the old folder keeps its tasks.</p>
            <dl class="settings-facts">
              <dt>Folder</dt><dd class="mono" id="settings-task-folder">—</dd>
              <dt>State</dt><dd class="task-folder-state" id="settings-task-folder-state">—</dd>
            </dl>
            <div class="task-folder-form">
              <input id="task-folder-input" type="text" autocomplete="off" spellcheck="false" placeholder="D:\\Work\\tasks" aria-label="Task folder path" data-trial-mutation />
              <button type="button" class="ghost" id="task-folder-browse" data-trial-mutation>Browse…</button>
              <button type="button" id="task-folder-use" data-trial-mutation>Use this folder</button>
            </div>
            <p class="task-folder-result" id="task-folder-result" role="status" aria-live="polite"></p>
          </section>
          <section class="view-section">
            <h3>Personal repository</h3>
            <p class="view-hint">Where PRIM-1 keeps its session register: one small file per Claude, Codex and Grok agent under <code>prim1\\sessions</code>, naming its current conversation, never its text. In a git repository PRIM-1 commits only those files and never pushes. Until you choose one, PRIM-1 uses the workspace. Choosing another folder moves later writes there; files already written stay where they are, and a task folder you have not chosen follows from the next start.</p>
            <dl class="settings-facts">
              <dt>Folder</dt><dd class="mono" id="settings-personal-repo">—</dd>
              <dt>State</dt><dd class="task-folder-state" id="settings-personal-repo-state">—</dd>
            </dl>
            <div class="task-folder-form">
              <input id="personal-repo-input" type="text" autocomplete="off" spellcheck="false" placeholder="D:\\All Repos\\victor" aria-label="Personal repository path" data-trial-mutation />
              <button type="button" class="ghost" id="personal-repo-browse" data-trial-mutation>Browse…</button>
              <button type="button" id="personal-repo-use" data-trial-mutation>Use this folder</button>
            </div>
            <p class="task-folder-result" id="personal-repo-result" role="status" aria-live="polite"></p>
          </section>
          <section class="view-section">
            <details class="local-conversations">
              <summary>Local conversations</summary>
              <p class="view-hint">Find Claude, Codex and Grok conversations for your personal repository. Scanning never attaches or starts a member. Open a root conversation to attach it to a stopped member for recovery; uncertain identities stay visible.</p>
              <button type="button" class="ghost" id="local-conversations-scan">Scan personal repository</button>
              <p id="local-conversations-status" role="status" aria-live="polite"></p>
              <div id="local-conversations-results"></div>
            </details>
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

    <!-- Telemetry view (top-bar line or corner menu → Telemetry). -->
    <section class="telemetry-view" id="telemetry-view" hidden aria-label="Telemetry">
      <article class="panel view-card">
        <i class="c tl"></i><i class="c tr"></i><i class="c bl"></i><i class="c br"></i>
        <div class="card-head view-head">
          <div class="card-title">
            <h2>Telemetry</h2>
          </div>
          <button type="button" class="ghost view-back" data-corner="telemetry">Back to sessions</button>
        </div>
        <div class="view-body">
          <p class="view-hint">${telemetryPageHint}</p>
          <table class="telemetry-table">
            <thead>
              <tr>
                <th scope="col">Process group</th>
                <th scope="col">Harness</th>
                <th scope="col">CPU now</th>
                <th scope="col">CPU peak</th>
                <th scope="col">Memory now</th>
                <th scope="col">Memory peak</th>
                <th scope="col">Processes</th>
              </tr>
            </thead>
            <tbody id="telemetry-rows"></tbody>
            <tfoot id="telemetry-total"></tfoot>
          </table>
          <p class="view-hint telemetry-foot" id="telemetry-foot">Waiting for the first sample…</p>
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
              <dt>Launch</dt><dd>Resumes the session's previous conversation when its ID is known (Claude, Grok; Codex only once its conversation ID is verified — otherwise the pane says why). If the harness binary is missing, the pane names the error and can look in the usual places. Right-click the tab → <b>Start fresh session</b> for a clean one; a running session is stopped first, after you confirm, and earlier conversation references stay in its history.</dd>
              <dt>Pane strip</dt><dd>Launch / Restart / Stop / Edit for the open session; the dot in the tab is its state (copper = ready or idle, bronze = busy or starting, red = stalled or failed).</dd>
              <dt>Normal / Unsafe</dt><dd>What the profile actually passes to the harness — Claude Code: <code>--permission-mode manual</code> vs <code>--dangerously-skip-permissions</code>. Codex: <code>--ask-for-approval on-request --sandbox workspace-write</code> vs <code>--dangerously-bypass-approvals-and-sandbox</code>. Grok Build: <code>--permission-mode default</code> vs <code>--permission-mode bypassPermissions</code>. Prime and Terminal: Normal only.</dd>
            </dl>
          </section>
          <section class="view-section">
            <h3>Keyboard (${keyboardPlatformLabel(keyPlatform)} bindings)</h3>
            <dl class="help-list" id="help-keys">${keyboardHelpMarkup(keyPlatform)}</dl>
            <p class="help-note">Every binding above comes from one table, <code>apps/desktop/src/keymap.ts</code>; anything not listed reaches the focused terminal as keystrokes. <b>Settings → Keys</b> binds copy, paste text, paste image, submit, new line and interrupt once for every harness, and shows what each harness receives.</p>
          </section>
          <section class="view-section">
            <h3>The ⇄ button — click for the menu, drag it anywhere</h3>
            <dl class="help-list">
              <dt>Rooms</dt><dd>Teams of sessions with a shared durable feed. <b>Post to feed</b> stores a message and wakes the members designated Supervisor (all three grants), as Send does; no one else is prompted. <b>Send</b> stores it once, then types a short pointer into each selected member's terminal, also while the member works; the member reads the message with <code>room_read</code> and answers with <code>room_ack</code>. The feed shows every receipt: Stored (queued until the member is ready), Held, Failed, Wake attempt recorded, Wake written, Input observed, Fetched, and each ACK. Written and Input observed do not prove model acceptance; the member's ACK does. Use <b>Retry wake</b> on an eligible held or failed wake to recheck readiness without reposting the message or touching successful recipients. Partial and unknown attempts cannot be retried. Prompt that member in its terminal to run <code>room_read</code> and inspect the stored message. Claude, Codex and Grok receive the pointer, never the whole message. A member that is stopped, starting or waiting on a prompt keeps it queued and receives it once ready. For Prime or Terminal, read the shared feed and provide context manually. Do not resend to all as a substitute. <b>Brief now</b> stores a new brief and attempts one wake.</dd>
              <dt>Tasks</dt><dd>The task list agents keep, grouped by state: Doing, In review, Blocked, Open, Done, Dropped. Click a task to read its original request word for word, its outcome, people, evidence and history. Read-only: ask an agent to change a task. <b>Refresh</b> reads the folder again; Settings chooses the folder.</dd>
              <dt>System log</dt><dd>The runtime's own messages in a bottom drawer. A red dot on the button means an error landed while the drawer was hidden; a failed close opens it.</dd>
              <dt>Themes</dt><dd>The theme gallery — one screenshot per theme; Dark and Light are plain terminal looks, the two PRIM-1 themes are the HUD look. <b>Unicolor</b> (off by default) drops the harnesses' own colours so everything renders in the theme's text colour.</dd>
              <dt>Fullscreen</dt><dd>Same as F11.</dd>
              <dt>Refresh state</dt><dd>Re-reads sessions and rooms from the supervisor.</dd>
              <dt>Telemetry</dt><dd>CPU and memory now and at peak for PRIM-1 itself and for each agent, every process counted once. The figures in the top bar open it too. It samples every 2 s, only while it is on screen.</dd>
              <dt>Settings</dt><dd>Which harnesses sit in the + menu; one key per action (copy, paste, submit, new line, interrupt) for every harness; the task folder; the personal repository that holds the session register; the runtime paths; whether PRIM-1 continues where you left off on open.</dd>
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
            <button type="button" class="ghost" id="retry-output-ready" hidden>Retry terminal display</button>
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
    <button type="button" class="corner-action" data-corner="tasks" role="menuitem">
      <span class="corner-action-glyph" aria-hidden="true">&#10003;</span>
      <span class="corner-action-label" data-corner-view-label="tasks">Tasks</span>
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
    <button type="button" class="corner-action" data-corner="telemetry" role="menuitem">
      <span class="corner-action-glyph" aria-hidden="true">&#8767;</span>
      <span class="corner-action-label" data-corner-view-label="telemetry">Telemetry</span>
    </button>
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

  <!-- Invite an outside agent (room panel): the native modal, outside every
       view so no layout change can hide it while the token is on screen. -->
  <dialog id="guest-invite-dialog" class="guest-invite-dialog" aria-labelledby="guest-invite-title">
    <h3 id="guest-invite-title">Invite an outside agent</h3>
    <form id="guest-invite-form" class="guest-invite-form">
      <label><span>Name <small>optional — shown as "name · guest"</small></span><input id="guest-invite-label" maxlength="60" autocomplete="off" placeholder="ChatGPT" /></label>
      <p class="guest-invite-note">The agent can read this room from now on and post to it. Each post is typed into every member's terminal as a guest message; members are told it is outside input.</p>
      <p id="guest-invite-error" class="session-form-error" role="alert" hidden></p>
      <div class="room-form-actions">
        <button type="submit" class="primary">Create invitation</button>
        <button type="button" class="ghost" data-guest-invite-close>Cancel</button>
      </div>
    </form>
    <div id="guest-invite-result" class="guest-invite-result" hidden>
      <p class="guest-invite-note">Copy the invitation and paste it into any agent running on this PC as you, outside PRIM-1 (Claude, Codex, Grok or another). It joins from its own PowerShell with no setup and is told how to take part. A cloud agent or another PC cannot reach the room. The token in it is shown only now and works only for this room; Revoke ends it.</p>
      <textarea id="guest-invite-block" rows="14" readonly spellcheck="false" aria-label="Invitation for the agent"></textarea>
      <div class="room-form-actions">
        <button type="button" class="primary" id="guest-invite-copy-block">Copy invitation</button>
        <span id="guest-invite-copied" class="room-brief-state" aria-live="polite"></span>
      </div>
      <details class="guest-invite-cli">
        <summary>MCP config only</summary>
        <p class="guest-invite-note">Add this MCP server to the agent's app: Claude desktop claude_desktop_config.json, Cursor mcp.json. Never put it where PRIM-1's own panes read it: not %USERPROFILE%\\.codex\\config.toml, not Claude Code's user or local MCP servers, not a project .mcp.json. For an outside Claude Code use claude --mcp-config with this saved as a separate file; for an outside Codex use -c overrides or its own CODEX_HOME (docs/GUEST-AGENTS.md).</p>
        <textarea id="guest-invite-config" rows="12" readonly spellcheck="false" aria-label="MCP server configuration"></textarea>
        <button type="button" class="ghost" id="guest-invite-copy">Copy MCP config</button>
      </details>
      <details class="guest-invite-cli">
        <summary>Script (PowerShell)</summary>
        <textarea id="guest-invite-cli" rows="5" readonly spellcheck="false" aria-label="PowerShell example"></textarea>
        <button type="button" class="ghost" id="guest-invite-copy-cli">Copy script</button>
      </details>
      <div class="room-form-actions">
        <button type="button" class="ghost" data-guest-invite-close>Done</button>
      </div>
    </div>
  </dialog>

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
  private readonly initialBannerBody: HTMLElement;
  private readonly initialBannerActions: HTMLDivElement;
  private readonly initialBannerElement: HTMLDivElement;
  private snapshot: SessionSnapshot | null = null;
  private pendingRepaintResync = false;
  private pendingVisibleFit = true;
  private readonly initialReplay = new InitialReplayFence();
  private readonly resizeState: TerminalResizeState;
  private readonly gridRecovery = new RunGridRecovery();
  private gridRecoveryErrorKey: string | null = null;
  private gridRecoveryRetry: HTMLButtonElement | null = null;
  private authoritativeRunGrid = true;
  private disposed = false;
  private readonly colourFilter = new SgrColourFilter();

  constructor(snapshot: SessionSnapshot) {
    const { session_id: sessionId, alias, label } = snapshot;
    const grid = initialTerminalGrid(snapshot.pty_size);
    this.snapshot = snapshot;
    this.resizeState = new TerminalResizeState(this.runKey(snapshot), grid);
    this.sessionId = sessionId;
    this.alias = alias;
    this.label = label;
    this.host = must<HTMLDivElement>(`[data-terminal="${sessionId}"]`);
    if (this.host.dataset.layoutError) {
      this.host.replaceChildren();
      delete this.host.dataset.layoutError;
    }
    this.stateEl = must<HTMLSpanElement>(`[data-session-state="${sessionId}"]`);
    this.activityEl = must<HTMLSpanElement>(`[data-session-activity="${sessionId}"]`);
    this.terminal = new Terminal({
      ...grid,
      scrollback: TERMINAL_SCROLLBACK_LINES,
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
      // Keep explicit pale ANSI/RGB text readable on Light's white background.
      minimumContrastRatio: currentThemeName() === "light" ? 4.5 : 1,
    });
    this.fitAddon = new FitAddon();
    this.terminal.loadAddon(this.fitAddon);
    this.terminal.open(this.host);
    this.terminal.onRender(() => {
      // A terminal opened while hidden may acquire cell metrics only when
      // xterm's supported visibility observer resumes its renderer.
      if (this.pendingVisibleFit && isPaneVisible(this.sessionId)) terminalLayout.request();
    });
    this.initialBannerElement = document.createElement("div");
    this.initialBannerElement.className = "terminal-prelaunch";
    this.initialBannerElement.setAttribute("role", "status");
    this.initialBannerLabel = document.createElement("strong");
    this.initialBannerLabel.textContent = `${this.label} pane ready.`;
    this.initialBannerBody = document.createElement("span");
    this.initialBannerBody.textContent = "Launch the session from the header to begin.";
    this.initialBannerActions = document.createElement("div");
    this.initialBannerActions.className = "terminal-prelaunch-actions";
    this.initialBannerElement.append(
      this.initialBannerLabel,
      this.initialBannerBody,
      this.initialBannerActions,
    );
    this.host.appendChild(this.initialBannerElement);
    this.initialBanner = new InitialTerminalBanner(
      () => {
        // A live grid error has its own recovery action; normal output must
        // not dismiss it along with the initial prelaunch banner.
        if (this.gridRecoveryErrorKey === null) this.initialBannerElement.hidden = true;
      },
      (chunk, onParsed) =>
        this.terminal.write(
          chunk,
          () => {
            try {
              if (perfEnabled) perfProbes.outputParsed(this.sessionId, () => textBeforeCursor(this.terminal));
            } catch {
              // Optional measurement must not throw into xterm's parser drain.
            } finally {
              onParsed?.();
            }
          },
        ),
    );
    this.host.addEventListener("focusin", () => {
      tabState.activeId = this.sessionId;
      activeCopySurface = this.sessionId;
    });
    this.host.addEventListener("mousedown", () => {
      tabState.activeId = this.sessionId;
      activeCopySurface = this.sessionId;
    });
    // The webview's own paste (keymap.ts lets every paste chord through, and
    // Wispr Flow's Ctrl+V / Shift+Insert arrive the same way), captured before
    // xterm's listeners: they would paste a second, unsanitized copy.
    this.host.addEventListener("paste", (event) => this.paste(event), { capture: true });
    // Pending-buffer replay and bootstrap event reconciliation run in this
    // task. Seal at its end, before future live events can extend the fence.
    queueMicrotask(() => this.sealInitialReplay());
  }

  private runKey(snapshot: SessionSnapshot): string {
    return `${snapshot.generation}:${snapshot.run_id ?? "stopped"}`;
  }

  private sealInitialReplay(): void {
    this.initialReplay.seal(
      (parsed) => this.terminal.write("", parsed),
      (error) => {
        if (error !== undefined) {
          this.pendingRepaintResync = true;
          writeSystem("error", `${this.label} initial terminal replay failed: ${String(error)}`);
        }
        if (this.pendingRepaintResync) this.forceFullRepaintResyncNow();
        terminalLayout.request();
      },
    );
  }

  private async requestRunGrid(key: string, explicit = false): Promise<void> {
    await this.gridRecovery.request(key, async (current) => {
      const snapshot = await command<RuntimeSnapshot>("bootstrap");
      const active = snapshotById.get(this.sessionId);
      if (!current() || !active || this.runKey(active) !== key) return;
      const observed = snapshot.sessions.find((session) => session.session_id === this.sessionId
        && this.runKey(session) === key);
      if (!observed) throw new Error("This run is absent from the backend snapshot.");
      // A successful RPC without an actual grid is not recovery. Keep the
      // old confirmed parser size and expose the mismatch without guessing.
      initialTerminalGrid(observed.pty_size);
      this.observeRun(observed, true);
    }, (error) => this.showRunGridError(key, error), explicit);
  }

  private showRunGridError(key: string, error: unknown): void {
    const active = snapshotById.get(this.sessionId);
    if (this.disposed || !active || this.runKey(active) !== key || this.authoritativeRunGrid) return;
    this.pendingVisibleFit = false;
    this.gridRecoveryErrorKey = key;
    this.initialBannerElement.hidden = false;
    this.initialBannerElement.setAttribute("role", "alert");
    this.initialBannerElement.classList.add("is-interactive");
    this.initialBannerLabel.textContent = "Terminal size could not be confirmed.";
    this.initialBannerBody.textContent = `Retry to restore terminal sizing. If it keeps failing, reload matching desktop assets. ${String(error)}`;
    if (!this.gridRecoveryRetry) {
      this.gridRecoveryRetry = document.createElement("button");
      this.gridRecoveryRetry.type = "button";
      this.gridRecoveryRetry.className = "ghost";
      this.gridRecoveryRetry.addEventListener("click", () => {
        const retryKey = this.gridRecoveryErrorKey;
        if (!retryKey || !this.gridRecoveryRetry) return;
        this.gridRecoveryRetry.disabled = true;
        this.gridRecoveryRetry.textContent = "Retrying terminal size…";
        void this.requestRunGrid(retryKey, true);
      });
    }
    this.gridRecoveryRetry.disabled = false;
    this.gridRecoveryRetry.textContent = "Retry terminal size";
    if (this.gridRecoveryRetry.parentElement !== this.initialBannerActions) {
      this.initialBannerActions.replaceChildren(this.gridRecoveryRetry);
    }
    writeSystem("error", `${this.label} terminal size recovery failed after two attempts: ${String(error)}`);
    terminalLayout.settleWithoutLayout(this.sessionId, () => isPaneVisible(this.sessionId));
  }

  private clearRunGridError(): void {
    if (this.gridRecoveryErrorKey === null) return;
    this.gridRecoveryErrorKey = null;
    this.initialBannerElement.hidden = true;
    this.initialBannerElement.setAttribute("role", "status");
    this.initialBannerElement.classList.remove("is-interactive");
    this.initialBannerActions.replaceChildren();
  }

  /** Run changes invalidate old callbacks before any replacement-run output. */
  observeRun(snapshot: SessionSnapshot, authoritative = false): void {
    if (this.disposed) return;
    const sameRun = this.snapshot && this.runKey(snapshot) === this.runKey(this.snapshot);
    if (sameRun && (!authoritative || this.authoritativeRunGrid)) return;
    // Lifecycle/content events carry a derived snapshot, not a new grid
    // observation. Keep the confirmed grid until the command/bootstrap reply.
    const grid = authoritative && snapshot.pty_size != null
      ? initialTerminalGrid(snapshot.pty_size) : this.resizeState.confirmed;
    const previousGrid = this.resizeState.confirmed;
    if (authoritative && (grid.cols !== previousGrid.cols || grid.rows !== previousGrid.rows)) {
      // New-run output may already have arrived at the inherited grid before
      // the authoritative reply; changing dimensions alone cannot heal it.
      this.pendingRepaintResync = true;
    }
    const key = this.runKey(snapshot);
    this.gridRecovery.adopt(key);
    if (authoritative) this.gridRecovery.resolved(key);
    if (authoritative || !sameRun) this.clearRunGridError();
    this.resizeState.adopt(key, grid, authoritative);
    this.authoritativeRunGrid = authoritative;
    // A grid-only authoritative reply may have an older output cursor. It
    // must not roll lifecycle or running state back with it.
    this.snapshot = sameRun ? { ...this.snapshot!, pty_size: snapshot.pty_size } : snapshot;
    this.initialReplay.reset();
    this.pendingVisibleFit = true;
    // Finish the old parser tail at its old grid; apply the authoritative
    // replacement grid before the next run's queued output is parsed.
    try {
      this.terminal.write("", () => {
        if (this.disposed || !this.snapshot || this.runKey(this.snapshot) !== key) return;
        try { this.terminal.resize(grid.cols, grid.rows); }
        catch { this.pendingRepaintResync = true; }
      });
    } catch {
      this.pendingRepaintResync = true;
    }
    queueMicrotask(() => this.sealInitialReplay());
    if (!authoritative && snapshot.run_id) {
      queueMicrotask(() => {
        if (!this.disposed && !this.authoritativeRunGrid
          && this.snapshot && this.runKey(this.snapshot) === key) {
          void this.requestRunGrid(key);
        }
      });
    }
  }

  /**
   * paste.ts plans it: text goes through xterm's paste() — CRLF to CR,
   * bracketed when the harness enabled bracketed paste — with control
   * characters removed; a clipboard holding only an image sends the harness
   * its own paste-image key.
   */
  private paste(event: ClipboardEvent): void {
    event.preventDefault();
    event.stopPropagation();
    const data = event.clipboardData;
    const raw = data?.getData("text/plain") ?? "";
    const hasImage = Array.from(data?.items ?? []).some(
      (item) => item.kind === "file" && item.type.startsWith("image/"),
    );
    if (!raw && !hasImage) {
      return;
    }
    if (!this.snapshot?.running || this.snapshot.output_draining) {
      writeSystem("warn", `${this.label} is not accepting input; paste skipped`);
      return;
    }
    const plan = planPaste(raw, hasImage, this.snapshot.driver, keyPlatform);
    switch (plan.kind) {
      case "text":
        this.terminal.paste(plan.text);
        writeSystem(
          "info",
          `${this.label} pasted ${plan.text.length} chars`
            + (plan.removed > 0 ? ` (${plan.removed} control characters removed)` : ""),
        );
        return;
      case "image":
        this.terminal.input(plan.bytes);
        writeSystem("info", `${this.label}: clipboard image handed to the harness`);
        return;
      case "nothing":
        if (plan.reason === "no-image-key") {
          writeSystem("warn", `${this.label} cannot paste images; the clipboard holds no text`);
        } else if (plan.reason === "only-controls") {
          writeSystem("warn", `${this.label}: the clipboard held only control characters; nothing pasted`);
        }
        return;
    }
  }

  hookInput(): void {
    if (this.hooked) {
      return;
    }

    this.hooked = true;
    this.terminal.onData((input) => {
      if (!this.snapshot?.running || this.snapshot.output_draining) {
        return;
      }
      if (perfEnabled) {
        perfProbes.inputAdmitted(this.sessionId, input, () => textBeforeCursor(this.terminal));
      }

      void command<void>("send_input", {
        request: {
          session_id: this.sessionId,
          input,
        } satisfies SendInputRequest,
      }).catch((error) => writeSystem("error", `${this.label} input failed: ${error}`));
    });
  }

  applySnapshot(snapshot: SessionSnapshot, authoritative = false): void {
    this.observeRun(snapshot, authoritative);
    this.alias = snapshot.alias;
    this.label = snapshot.label;
    this.snapshot = snapshot;
    this.stateEl.textContent = lifecycleLabel(sessionDisplayState(snapshot));
    this.stateEl.dataset.state = sessionDisplayState(snapshot);
    this.stateEl.title = sessionReadiness(snapshot, false).reason;
    this.activityEl.textContent = snapshot.last_activity_at
      ? new Date(snapshot.last_activity_at).toLocaleTimeString()
      : snapshot.running
        ? "running"
        : "idle";
    this.activityEl.dataset.running = String(snapshot.running);

    this.initialBanner.observeRunning(snapshot.running);
    if (!snapshot.running) {
      this.renderPrelaunch(snapshot);
    }
    this.hookInput();
  }

  private renderPrelaunch(snapshot: SessionSnapshot): void {
    if (this.gridRecoveryErrorKey !== null) return;
    const view = prelaunchView(snapshot);
    if (view.kind === "exit" || view.kind === "sleeping") {
      // A retired run can retain an error and its final terminal output.
      // Keep the tail readable; the existing status pill carries its outcome.
      this.initialBannerElement.hidden = true;
      this.initialBannerActions.replaceChildren();
      this.initialBannerElement.classList.remove("is-interactive");
      this.stateEl.textContent = view.kind === "sleeping" ? "Sleeping" : "exited";
      this.stateEl.title = view.body;
      return;
    }
    if (view.kind === "idle" && this.initialBannerElement.hidden) {
      return;
    }
    this.initialBannerLabel.textContent = view.title;
    this.initialBannerBody.textContent = view.body;
    this.initialBannerActions.replaceChildren();
    this.initialBannerElement.hidden = false;
    if (view.canSearch) {
      const ask = document.createElement("span");
      ask.textContent = searchPrompt(snapshot);
      const yes = document.createElement("button");
      yes.type = "button";
      yes.className = "ghost";
      yes.textContent = "Yes";
      yes.addEventListener("click", (event) => {
        event.preventDefault();
        // Replay the failed request exactly, adding only search (S5): the
        // backend never infers Fresh from Search.
        const failed = lastFailedStart.get(this.sessionId);
        void launchSession(this.sessionId, {
          search: true,
          fresh: failed?.fresh,
          launchChoices: failed?.launch_choices ?? undefined,
        });
      });
      const no = document.createElement("button");
      no.type = "button";
      no.className = "ghost";
      no.textContent = "No";
      no.addEventListener("click", (event) => {
        event.preventDefault();
        this.initialBannerActions.replaceChildren();
        this.initialBannerElement.classList.remove("is-interactive");
      });
      this.initialBannerActions.append(ask, yes, no);
      this.initialBannerElement.classList.add("is-interactive");
    } else if (view.canStartFresh) {
      const fresh = document.createElement("button");
      fresh.type = "button";
      fresh.className = "ghost";
      fresh.textContent = "Start fresh session";
      fresh.addEventListener("click", (event) => {
        event.preventDefault();
        void startFreshSession(this.sessionId);
      });
      this.initialBannerActions.append(fresh);
      if (snapshotById.get(this.sessionId)?.recovery?.provenance === "room_import_in_use") {
        const resume = document.createElement("button");
        resume.type = "button";
        resume.className = "ghost";
        resume.textContent = "Resume here";
        resume.addEventListener("click", (event) => {
          event.preventDefault();
          void resumeHeldConversation(this.sessionId);
        });
        this.initialBannerActions.append(resume);
      }
      this.initialBannerElement.classList.add("is-interactive");
    } else {
      this.initialBannerElement.classList.remove("is-interactive");
    }
  }

  write(chunk: string, onParsed?: () => void): void {
    if (paneByteTapEnabled) {
      recordPaneTap(this.sessionId, chunk, this.terminal.cols, this.terminal.rows);
    }
    this.initialBanner.forwardOutput(unicolor ? this.colourFilter.apply(chunk) : chunk, onParsed);
  }

  /** Unicolor switched off mid-stream: hand any held partial sequence on. */
  releaseColourFilter(): void {
    const carried = this.colourFilter.flush();
    if (carried) {
      this.initialBanner.forwardOutput(carried);
    }
  }

  /** A visible, actionable grid error can complete activation without fitting. */
  canCompleteLayoutAtConfirmedGrid(): boolean {
    const current = snapshotById.get(this.sessionId);
    return terminalGridErrorAllowsCompletion(
      this.gridRecoveryErrorKey, current ? this.runKey(current) : null,
      isPaneVisible(this.sessionId), this.disposed,
    );
  }

  /** Measure the pane area — only a visible host can be measured. */
  measure(): { cols: number; rows: number } | null {
    if (!isPaneVisible(this.sessionId) || !this.initialReplay.ready
      || (this.snapshot?.running && !this.authoritativeRunGrid) || this.resizeState.busy) {
      return null;
    }
    const proposed = this.fitAddon.proposeDimensions();
    if (!proposed || !Number.isFinite(proposed.cols) || !Number.isFinite(proposed.rows)) {
      this.pendingVisibleFit = true;
      return null;
    }
    this.pendingVisibleFit = false;
    return { cols: Math.max(2, proposed.cols), rows: Math.max(1, proposed.rows) };
  }

  /** A hidden pane keeps its own last PTY grid and continues parsing there. */
  applyGrid(cols: number, rows: number): void {
    // Closed panes also retain the saved PTY grid: changing an empty xterm
    // here would misparse the next hidden launch's replay before activation.
    if (!isPaneVisible(this.sessionId) || !this.snapshot?.running
      || !this.initialReplay.ready || !this.authoritativeRunGrid || this.resizeState.busy) return;
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
    if (this.pendingRepaintResync && rows > 1) {
      this.forceFullRepaintResyncNow();
      return;
    }
    const confirmed = this.resizeState.confirmed;
    if (confirmed.cols === cols && confirmed.rows === rows) return;
    const ticket = this.resizeState.begin();
    if (ticket === null) return;
    const key = this.runKey(this.snapshot);
    void this.resizeForRun(ticket, key, cols, rows).then((resized) => {
      if (!this.resizeIsCurrent(ticket, key)) return;
      if (resized) this.resizeState.confirm(ticket, { cols, rows });
      const actual = this.resizeState.finish(ticket)!;
      if (!resized) {
        // Local rollback is required even after hide. Output parsed during
        // the optimistic resize may be uncertain, so retain repaint recovery.
        this.pendingVisibleFit = false;
        this.terminal.resize(actual.cols, actual.rows);
        this.pendingRepaintResync = true;
        writeSystem("warn", `${this.label} resize failed; restored the last confirmed grid; repaint pending`);
        terminalLayout.settleWithoutLayout(this.sessionId, () => this.snapshot !== null
          && this.runKey(this.snapshot) === key && isPaneVisible(this.sessionId));
      } else if (this.pendingRepaintResync) {
        this.forceFullRepaintResyncNow();
      } else {
        terminalLayout.request();
      }
    });
  }

  private resizeIsCurrent(ticket: number, key: string): boolean {
    const current = snapshotById.get(this.sessionId);
    return this.resizeState.current(ticket) && current !== undefined && this.runKey(current) === key;
  }

  private async resizeForRun(ticket: number, key: string, cols: number, rows: number): Promise<boolean> {
    const expected = snapshotById.get(this.sessionId);
    if (!this.resizeIsCurrent(ticket, key) || !expected?.running || !expected.run_id) return false;
    const resized = await resizeSession(this.sessionId, cols, rows, expected.run_id, expected.generation);
    if (resized && this.resizeIsCurrent(ticket, key)) {
      // Event-derived snapshots must inherit the latest confirmed grid.
      const current = snapshotById.get(this.sessionId)!;
      snapshotById.set(this.sessionId, { ...current, pty_size: [cols, rows] });
    }
    return resized;
  }

  /** Unattached-buffer overflow is also explicit display loss, including in a hidden pane. */
  requestFullRepaintResync(): void {
    this.forceFullRepaintResyncNow();
  }

  /** Actual display loss needs a full repaint even in the background. This is
      correctness recovery, not ordinary hidden-pane layout work. */
  forceFullRepaintResyncNow(): void {
    this.pendingRepaintResync = true;
    const { cols, rows } = this.terminal;
    if (!this.disposed && this.initialReplay.ready && this.authoritativeRunGrid
      && this.snapshot?.running && rows > 1) {
      const ticket = this.resizeState.begin();
      if (ticket === null) return;
      const key = this.runKey(this.snapshot);
      this.pendingRepaintResync = false;
      void repaintTerminalGrid(
        { cols, rows },
        (grid) => this.resizeForRun(ticket, key, grid.cols, grid.rows),
        () => this.resizeIsCurrent(ticket, key),
        (grid) => this.resizeState.confirm(ticket, grid),
      ).then((healed) => {
        if (!this.resizeIsCurrent(ticket, key)) return;
        const actual = this.resizeState.finish(ticket)!;
        // Failed restore can leave the PTY shrunk; match its confirmed grid.
        if (!healed) this.pendingVisibleFit = false;
        this.terminal.resize(actual.cols, actual.rows);
        this.pendingRepaintResync ||= !healed;
        if (!healed) {
          writeSystem("warn", `${this.label} full repaint is still pending; a resize leg failed`);
          terminalLayout.settleWithoutLayout(this.sessionId, () => this.snapshot !== null
            && this.runKey(this.snapshot) === key && isPaneVisible(this.sessionId));
        }
        if (healed && this.pendingRepaintResync) this.forceFullRepaintResyncNow();
        else if (healed) terminalLayout.request();
      });
    }
  }

  dispose(): void {
    this.disposed = true;
    this.initialReplay.dispose();
    this.resizeState.dispose();
    this.gridRecovery.dispose();
    outputCredits.disposeSession(this.sessionId);
    this.terminal.dispose();
  }
}

const systemTerminal = new Terminal({
  convertEol: true,
  disableStdin: true,
  scrollback: TERMINAL_SCROLLBACK_LINES,
  fontFamily: '"JetBrains Mono", "Cascadia Code", Consolas, monospace',
  fontSize: 12,
  minimumContrastRatio: 1,
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
const sessionCodexTransportField = must<HTMLLabelElement>("#session-codex-transport-field");
const sessionCodexTransportSelect = must<HTMLSelectElement>("#session-codex-transport");
const sessionCodexTransportNote = must<HTMLElement>("#session-codex-transport-note");
const sessionLaunchChoicesField = must<HTMLFieldSetElement>("#session-launch-choices");
const sessionModelInput = must<HTMLInputElement>("#session-model");
const sessionEffortInput = must<HTMLInputElement>("#session-effort");
const sessionBriefMode = must<HTMLSelectElement>("#session-brief-mode");
const sessionBriefCustomField = must<HTMLElement>("#session-brief-custom-field");
const sessionBriefCustom = must<HTMLTextAreaElement>("#session-brief-custom");
const sessionChoicesNote = must<HTMLElement>("#session-choices-note");
const sessionMemberGrants = must<HTMLFieldSetElement>("#session-member-grants");
const sessionGrantSupervisor = must<HTMLInputElement>("#session-grant-supervisor");
const sessionGrantSupervisorField = must<HTMLElement>("#session-grant-supervisor-field");
const sessionGrantRoam = must<HTMLInputElement>("#session-grant-roam");
const sessionGrantMaintain = must<HTMLInputElement>("#session-grant-maintain");
const sessionGrantsNote = must<HTMLElement>("#session-grants-note");
const sessionGrantManage = must<HTMLInputElement>("#session-grant-manage");
const sessionGrantManageField = must<HTMLElement>("#session-grant-manage-field");
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
installSessionInventory(
  must<HTMLButtonElement>("#local-conversations-scan"),
  must<HTMLElement>("#local-conversations-status"),
  must<HTMLElement>("#local-conversations-results"),
  () => command<SessionInventory>("discover_local_sessions"),
  {
    members: () => [...snapshotById.values()],
    canAttach: () => desktopCommandAllowed(trialStatus, "attach_local_session"),
    confirm: (message) => confirmAction(message, "Attach"),
    attach: (request) => command<void>("attach_local_session", { request }),
    refresh: () => refreshSnapshot(),
  },
);
const helpView = must<HTMLElement>("#help-view");
const themesView = must<HTMLElement>("#themes-view");
const themeGallery = must<HTMLDivElement>("#theme-gallery");
const unicolorToggle = must<HTMLInputElement>("#unicolor-toggle");
const quickAttachSettings = must<HTMLUListElement>("#quick-attach-settings");
const keysTable = must<HTMLTableElement>("#keys-table");
const keysStatus = must<HTMLElement>("#keys-status");
const keysReset = must<HTMLButtonElement>("#keys-reset");
const helpKeys = must<HTMLElement>("#help-keys");
const settingsWorkspace = must<HTMLElement>("#settings-workspace");
const settingsRuntimeDir = must<HTMLElement>("#settings-runtime-dir");
const settingsAuditPath = must<HTMLElement>("#settings-audit-path");
const settingsControlEndpoint = must<HTMLElement>("#settings-control-endpoint");
const settingsTaskFolder = must<HTMLElement>("#settings-task-folder");
const settingsTaskFolderState = must<HTMLElement>("#settings-task-folder-state");
const taskFolderInput = must<HTMLInputElement>("#task-folder-input");
const taskFolderBrowse = must<HTMLButtonElement>("#task-folder-browse");
const taskFolderUse = must<HTMLButtonElement>("#task-folder-use");
const taskFolderResult = must<HTMLElement>("#task-folder-result");
const settingsPersonalRepo = must<HTMLElement>("#settings-personal-repo");
const settingsPersonalRepoState = must<HTMLElement>("#settings-personal-repo-state");
const personalRepoInput = must<HTMLInputElement>("#personal-repo-input");
const personalRepoBrowse = must<HTMLButtonElement>("#personal-repo-browse");
const personalRepoUse = must<HTMLButtonElement>("#personal-repo-use");
const personalRepoResult = must<HTMLElement>("#personal-repo-result");
const idleHibernationControls = must<HTMLElement>("#idle-hibernation-controls");
const idleHibernationEnabled = must<HTMLInputElement>("#idle-hibernation-enabled");
const idleHibernationMinutes = must<HTMLInputElement>("#idle-hibernation-minutes");
const idleHibernationSave = must<HTMLButtonElement>("#idle-hibernation-save");
const idleHibernationStatus = must<HTMLElement>("#idle-hibernation-status");
const tasksView = must<HTMLElement>("#tasks-view");
const tasksRefresh = must<HTMLButtonElement>("#tasks-refresh");
const tasksFolder = must<HTMLElement>("#tasks-folder");
const tasksProblem = must<HTMLElement>("#tasks-problem");
const tasksProblemText = must<HTMLElement>("#tasks-problem-text");
const tasksOpenSettings = must<HTMLButtonElement>("#tasks-open-settings");
const tasksUnreadable = must<HTMLDetailsElement>("#tasks-unreadable");
const tasksUnreadableSummary = must<HTMLElement>("#tasks-unreadable-summary");
const tasksUnreadableList = must<HTMLUListElement>("#tasks-unreadable-list");
const tasksList = must<HTMLElement>("#tasks-list");
const tasksDetail = must<HTMLElement>("#tasks-detail");
const tasksStatus = must<HTMLElement>("#tasks-status");
const telemetryView = must<HTMLElement>("#telemetry-view");
const telemetryLineButton = must<HTMLButtonElement>("#telemetry-line");
const telemetryRowsBody = must<HTMLTableSectionElement>("#telemetry-rows");
const telemetryTotal = must<HTMLTableSectionElement>("#telemetry-total");
const telemetryFoot = must<HTMLElement>("#telemetry-foot");
let telemetryLast: TelemetrySnapshot | null = null;
let telemetryError: string | null = null;
/** Run ids live when the last sample was requested, and the subset still live
    when it arrived: only those runs' usage may be shown on a member card. */
let telemetryRequestRuns = new Map<string, string>();
let telemetrySampleRuns = new Map<string, string>();
/** One pending expiry per sample: if the next sample never arrives, member
    usage still turns to "usage pending" once this one is too old to show. */
let memberUsageExpiry: number | null = null;
/** Telemetry polling (PLAN E7): every 2 s while the line or the page is on
    screen, never while the window is minimized or its state is unknown. */
const telemetryPoller = new TelemetryPoller({
  documentVisible: () => document.visibilityState === "visible",
  lineShown: () => telemetryLineButton.getClientRects().length > 0,
  pageOpen: () => shellView === "telemetry",
  isMinimized: () => getCurrentWindow().isMinimized(),
  sample: () => {
    telemetryRequestRuns = liveRunIds();
    return command<TelemetrySnapshot>("telemetry_snapshot");
  },
  onSample: (snapshot) => {
    telemetryLast = snapshot;
    telemetryError = null;
    const arrived = liveRunIds();
    telemetrySampleRuns = new Map(
      Array.from(telemetryRequestRuns).filter(([sessionId, runId]) => arrived.get(sessionId) === runId),
    );
    renderTelemetry();
    updateRoomMemberUsage();
    if (memberUsageExpiry !== null) window.clearTimeout(memberUsageExpiry);
    const age = Date.now() - Date.parse(snapshot.sampled_at);
    memberUsageExpiry = window.setTimeout(() => {
      memberUsageExpiry = null;
      updateRoomMemberUsage();
    }, Math.max(0, 3 * TELEMETRY_POLL_MS - age) + 50);
  },
  onError: (error) => {
    telemetryError = String(error);
    renderTelemetry();
    updateRoomMemberUsage();
  },
  now: () => performance.now(),
  setTimer: (callback, delayMs) => window.setTimeout(callback, delayMs),
  clearTimer: (timer) => window.clearTimeout(timer),
});
type ShellView = "sessions" | "rooms" | "tasks" | "settings" | "help" | "themes" | "telemetry";
const VIEW_NAMES: Record<ShellView, string> = {
  sessions: "Sessions",
  rooms: "Rooms",
  tasks: "Tasks",
  settings: "Settings",
  help: "Help",
  themes: "Themes",
  telemetry: "Telemetry",
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
/** Settled by applyPrimeAvailability; Settings → Keys shows Prime's column only then. */
let primeAvailable = false;

/** Prime/WSL is parked in v2 unless the backend runs with PRIM1_ENABLE_PRIME=1
    (Victor, 13 Oct: hidden, kept as a concept). Parked, it is offered nowhere
    a new session starts; an existing Prime row still renders. */
async function applyPrimeAvailability(): Promise<void> {
  const enabled = await invoke<boolean>("prime_enabled").catch(() => false);
  primeAvailable = enabled;
  const primeOption = sessionDriverSelect.querySelector<HTMLOptionElement>(
    'option[value="prime"]',
  );
  if (primeOption) {
    primeOption.hidden = !enabled;
    primeOption.disabled = !enabled;
  }
  if (!enabled) {
    // In memory only: once Prime is enabled again, loading the saved
    // preference appends it back.
    quickAttachPrefs = {
      order: quickAttachPrefs.order.filter((driver) => driver !== "prime"),
      hidden: quickAttachPrefs.hidden.filter((driver) => driver !== "prime"),
    };
  }
}

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
const saveRoomButton = must<HTMLButtonElement>("#save-room");
const roomsLoadProblem = must<HTMLElement>("#rooms-load-problem");
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
const roomMembersCount = must<HTMLElement>("#room-members-count");
const roomMembersCloseButton = must<HTMLButtonElement>("#room-members-close");
const roomMembersOpenButton = must<HTMLButtonElement>("#room-members-open");
const ROOM_MEMBERS_PANEL_KEY = "prim1-room-members-panel";
const roomResumeToggle = must<HTMLInputElement>("#room-resume-toggle");
const roomBriefSettings = must<HTMLDetailsElement>("#room-brief-settings");
const roomBriefOnLaunch = must<HTMLInputElement>("#room-brief-on-launch");
const roomBriefText = must<HTMLTextAreaElement>("#room-brief-text");
const roomBriefSaveButton = must<HTMLButtonElement>("#room-brief-save");
const roomBriefDefaultButton = must<HTMLButtonElement>("#room-brief-default");
const roomBriefState = must<HTMLElement>("#room-brief-state");
const roomResumeNote = must<HTMLElement>("#room-resume-note");
const roomResumeNoteText = must<HTMLElement>("#room-resume-note-text");
const roomResumeNowButton = must<HTMLButtonElement>("#room-resume-now");
const roomAddMemberSelect = must<HTMLSelectElement>("#room-add-member-select");
const roomAddMemberButton = must<HTMLButtonElement>("#room-add-member");
const roomGuestList = must<HTMLDivElement>("#room-guest-list");
const roomGuestsCount = must<HTMLElement>("#room-guests-count");
const roomInviteGuestButton = must<HTMLButtonElement>("#room-invite-guest");
const guestInviteDialog = must<HTMLDialogElement>("#guest-invite-dialog");
const guestInviteForm = must<HTMLFormElement>("#guest-invite-form");
const guestInviteLabel = must<HTMLInputElement>("#guest-invite-label");
const guestInviteError = must<HTMLElement>("#guest-invite-error");
const guestInviteResult = must<HTMLElement>("#guest-invite-result");
const guestInviteBlock = must<HTMLTextAreaElement>("#guest-invite-block");
const guestInviteConfig = must<HTMLTextAreaElement>("#guest-invite-config");
const guestInviteCli = must<HTMLTextAreaElement>("#guest-invite-cli");
const guestInviteCopied = must<HTMLElement>("#guest-invite-copied");
/** The room an open invitation dialog belongs to; null when it is closed. */
let guestInviteRoomId: string | null = null;
const roomFeed = must<HTMLDivElement>("#room-feed");
const roomMessage = must<HTMLTextAreaElement>("#room-message");
/** Each room owns its unsent composer text; see room-drafts.ts. */
const roomDrafts = new RoomDraftStore();
const roomPostButton = must<HTMLButtonElement>("#room-post");
// A16 (Victor, 20 Oct): a post reaches the room's Supervisors natively.
const ROOM_POST_TITLE = "Append to the shared feed and type it to the members designated Supervisor; no one else is prompted";
const roomRecipient = must<HTMLSelectElement>("#room-recipient");
const roomSendButton = must<HTMLButtonElement>("#room-send");
const roomStatus = must<HTMLElement>("#room-status");
const roomHistoryReplay = must<HTMLElement>("#room-history-replay");
const snapshotById = new Map<string, SessionSnapshot>();
let latestSnapshotRequest = 0;

const paneMap = new Map<string, SessionTerminal>();
const pendingOutput = new Map<string, PendingBuffer>();
const outputCredits = new RendererOutputCredit((credit, disposition) => {
  const operation = disposition === "parsed"
    ? command<number>("acknowledge_output", { credit })
    : command<void>("discard_output", { credit, reason: disposition });
  void operation.catch((error) => writeSystem("error",
    `Output ${disposition === "parsed" ? "acknowledgement" : "disposal"} failed: ${String(error)}. Display admission may be held; reload the window to reset its display stream.`));
});
const controlEndpoint = must<HTMLElement>("#control-endpoint");
const auditPath = must<HTMLElement>("#audit-path");
const runtimePath = must<HTMLElement>("#runtime-path");
let tabState: SessionTabsState = { order: [], activeId: null };
const terminalLayout = new TerminalLayoutScheduler({
  visible: () => document.visibilityState === "visible",
  requestFrame: (callback) => requestAnimationFrame(callback),
  cancelFrame: (frame) => cancelAnimationFrame(frame),
  layout: () => {
    const fitted = fitVisiblePanes();
    if (terminalHostIsVisible(systemTerminalHost)) systemFit.fit();
    return fitted;
  },
});
let activeCopySurface: CopySurface = null;
let workspacePreference = "";
let roomSnapshots: RoomSnapshot[] = [];
let activeRoomId: string | null = null;

/* ── Work contexts (Rooms B1) ──
   The operator stands in the lobby or in one room; the top bar, grid,
   zero-state and keyboard order show only that context's sessions. Panes of
   every other context stay mounted and their harnesses keep running — leaving
   a room never stops its agents, and entering one never starts them. On boot
   only the rooms marked Resume on restart relaunch their agents. */
const WORK_CONTEXT_KEY = "prim1-work-context";
const storedWorkContextRaw = localStorage.getItem(WORK_CONTEXT_KEY);
let workContext: WorkContext = parseWorkContext(storedWorkContextRaw);
/** False only until a context has ever been chosen on this origin — gates the
    one-shot first-boot default below. */
let workContextWasStored = storedWorkContextRaw !== null;
let roomsViewMode: "overview" | "panel" = "overview";
/** Remembered active tab per context, so re-entering lands where you left. */
const lastActiveByContext = new Map<string, string>();

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
const roomCardIndexes = new Map<string, RoomCardIndex>();
let roomCardPage = 0;
const roomFeedCursors = new Map<string, RoomFeedCursor>();
const pendingRoomSubmissions = new Set<string>();
const MAX_PENDING_ROOM_WAKE_RETRIES = 16;
// Hold each recipient until its request settles; never evict an in-flight retry.
const pendingRoomWakeRetries = new Set<string>();
const queuedRoomFeedEvents = new Map<string, RoomFeedEvent[]>();
const initializedRoomFeeds = new Set<string>();
const loadingRoomFeeds = new Set<string>();
const roomFeedReloadRequired = new Set<string>();
/** Victor, room 117 (14 Oct): the room keeps PRIM-1's own feed; there is no
    Readable/Raw choice. The Readable renderer stays for the Experimental view. */
let roomFeedView: RoomFeedView = "raw";
/** Messages opened with Show all; the oldest is forgotten first. */
const expandedRoomMessages = new Set<string>();
const MAX_EXPANDED_ROOM_MESSAGES = 128;
/** Readable follows new traffic only while the reader is at the bottom, so
    reading an older message is never interrupted by a new one. */
let readableFeedFollow = true;
let readableFeedRoomId: string | null = null;
let readableReconcilePending = false;
let readableExplicitPageChange = false;
let readableHeldSelectionKeys: ReadonlySet<string> | null = null;
const READABLE_FOLLOW_SLACK_PX = 32;
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

const themeMotionPreference = window.matchMedia("(prefers-reduced-motion: reduce)");

/** Decorative effects have no work to do in a hidden window. In particular,
    unreferenced SVG filters otherwise keep their own animation timelines alive. */
function syncThemeEffects(): void {
  const active = !document.hidden && !themeMotionPreference.matches;
  document.documentElement.dataset.themeEffects = active ? "active" : "paused";
  const filters = document.querySelector<SVGSVGElement>("#theme-effect-filters");
  if (filters) {
    if (active && currentThemeName() === "prim1-deep") {
      filters.unpauseAnimations();
    } else {
      filters.pauseAnimations();
    }
  }
}

/** `chosen` marks an explicit pick on the Themes page; only those survive as
    a preference. Values written by the old cycle button were never a choice,
    so a user who never picked lands on the default. */
function applyTheme(name: ThemeName, chosen = false): void {
  document.documentElement.dataset.theme = name;
  syncThemeEffects();
  localStorage.setItem("prim1-theme", name);
  if (chosen) {
    localStorage.setItem("prim1-theme-chosen", "1");
  }

  const themes = TERMINAL_THEMES[name];
  const minimumContrastRatio = name === "light" ? 4.5 : 1;
  for (const pane of paneMap.values()) {
    pane.terminal.options.theme = themes.session;
    pane.terminal.options.minimumContrastRatio = minimumContrastRatio;
  }
  systemTerminal.options.theme = themes.system;
  systemTerminal.options.minimumContrastRatio = minimumContrastRatio;
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
  appShell.dataset.roomsMode = roomsViewMode;
  roomsView.hidden = view !== "rooms";
  if (view === "rooms") {
    roomsOverviewCard.hidden = roomsViewMode !== "overview";
    roomCard.hidden = roomsViewMode !== "panel";
  }
  tasksView.hidden = view !== "tasks";
  settingsView.hidden = view !== "settings";
  helpView.hidden = view !== "help";
  themesView.hidden = view !== "themes";
  telemetryView.hidden = view !== "telemetry";
  brandActiveTag.textContent = VIEW_NAMES[view];
  for (const label of cornerMenu.querySelectorAll<HTMLElement>("[data-corner-view-label]")) {
    const target = label.dataset.cornerViewLabel as ShellView;
    label.textContent = view === target ? "Back to sessions" : VIEW_NAMES[target];
  }
  if (view === "settings") {
    renderQuickAttachSettings();
    renderKeysSettings();
    void loadTaskFolderSetting();
    void loadPersonalRepoSetting();
    syncIdleHibernationControls();
  } else {
    cancelKeyCapture();
  }
  if (view === "tasks") {
    void loadTasks();
  }
  if (view === "telemetry") {
    renderTelemetryPage();
  }
  telemetryPoller.sync();
  syncContextChipCurrent();
  terminalLayout.request();
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
  terminalLayout.request();
}

/* ── Telemetry (PLAN E7): the top-bar line and the Telemetry page ── */

function renderTelemetry(): void {
  renderTelemetryLine();
  if (shellView === "telemetry") {
    renderTelemetryPage();
  }
}

function renderTelemetryLine(): void {
  if (telemetryError !== null) {
    telemetryLineButton.textContent = "Telemetry unavailable";
    telemetryLineButton.title = telemetryError;
    telemetryLineButton.setAttribute("aria-label", `Telemetry unavailable: ${telemetryError}`);
    telemetryLineButton.dataset.state = "error";
    return;
  }
  delete telemetryLineButton.dataset.state;
  telemetryLineButton.title = telemetryLineTitle(telemetryLast);
  // The labels are hidden on a narrow bar, so the full line names the button.
  telemetryLineButton.setAttribute("aria-label", telemetryLine(telemetryLast));
  telemetryLineButton.replaceChildren(
    ...telemetryLineParts(telemetryLast).flatMap((part, index) => {
      const label = document.createElement("span");
      label.className = "telemetry-label";
      label.textContent = `${part.label} `;
      const nodes: Node[] = index === 0 ? [] : [document.createTextNode(" · ")];
      nodes.push(label, document.createTextNode(part.value));
      return nodes;
    }),
  );
}

function telemetryRowElement(row: TelemetryRow): HTMLTableRowElement {
  const element = document.createElement("tr");
  element.dataset.kind = row.kind;
  const name = document.createElement("th");
  name.scope = "row";
  name.textContent = row.name;
  element.append(name);
  for (const value of [
    row.harness,
    row.cpuNow,
    row.cpuPeak,
    row.memoryNow,
    row.memoryPeak,
    row.processes,
  ]) {
    const cell = document.createElement("td");
    cell.textContent = value;
    element.append(cell);
  }
  return element;
}

function renderTelemetryPage(): void {
  if (telemetryLast) {
    const rows = telemetryRows(telemetryLast).map(telemetryRowElement);
    if (telemetryLast.sessions.length === 0) {
      const empty = document.createElement("tr");
      empty.dataset.kind = "agent";
      const cell = document.createElement("td");
      cell.colSpan = 7;
      cell.textContent = "No agent is running.";
      empty.append(cell);
      rows.push(empty);
    }
    telemetryRowsBody.replaceChildren(...rows);
    telemetryTotal.replaceChildren(telemetryRowElement(telemetryTotalRow(telemetryLast)));
    telemetryFoot.textContent = telemetryFootnote(telemetryLast, (timestamp) =>
      new Date(timestamp).toLocaleTimeString(),
    );
  }
  if (telemetryError !== null) {
    telemetryFoot.textContent = `Telemetry unavailable: ${telemetryError}`;
  }
}

function wireTelemetry(): void {
  renderTelemetryLine();
  telemetryLineButton.addEventListener("click", () => {
    setShellView(shellView === "telemetry" ? "sessions" : "telemetry");
  });
  document.addEventListener("visibilitychange", () => {
    telemetryPoller.sync();
    if (document.visibilityState === "visible") updateRoomMemberUsage();
  });
  // Narrow windows hide the line; widening shows it again.
  window.addEventListener("resize", () => telemetryPoller.sync());
  // Minimizing and restoring resize the window; each asks it again.
  void getCurrentWindow().onResized(() => void telemetryPoller.refreshWindowState());
  void telemetryPoller.refreshWindowState();
}

/* ── Tasks (read-only) and Settings → Task folder ──
   The panel reads through tasks_query, whose backend schema has no mutation
   type; there is no edit control anywhere. Agents change tasks through their
   pane `tasks` tool. No live events in this build: Refresh reads again. */

let taskGroupPages = new Map<TaskState, TaskGroupPage>();
const taskGroupOffsets = new Map<TaskState, number>();
const taskGroupsOpen = new Set<TaskState>(TASK_STATE_ORDER.filter((state) => !TASK_STATES_START_CLOSED.has(state)));
let selectedTaskId: string | null = null;
let tasksLoadSeq = 0;
let taskDetailSeq = 0;
/* The folder-selection boundary. Starting a selection bumps the generation;
   every task list, detail and folder-status read captures it when it starts
   and drops its result, or its failure, once it has changed. No task read
   starts while a selection is pending; when the selection settles, either
   way, it reads again what is on show. */
let taskFolderPending = false,
  taskFolderGeneration = 0;

function textSpan(className: string, text: string): HTMLSpanElement {
  const span = document.createElement("span");
  span.className = className;
  span.textContent = text;
  return span;
}

function readTasks(query: string, variables: Record<string, unknown>): Promise<unknown> {
  return command<unknown>("tasks_query", { query, variables });
}

function resetTasksPanel(): void {
  taskGroupPages = new Map();
  taskGroupOffsets.clear();
  selectedTaskId = null;
  taskDetailSeq += 1;
  renderTaskGroups();
  renderUnreadableTasks([]);
  renderTaskDetailMessage("Choose a task to read it.");
}

async function loadTasks(): Promise<void> {
  if (taskFolderPending) {
    // Never read a folder that is still being chosen; the selection reads
    // again when it settles.
    delete tasksStatus.dataset.tone;
    tasksStatus.textContent = "Waiting for the task folder to change…";
    return;
  }
  const seq = ++tasksLoadSeq;
  const generation = taskFolderGeneration;
  const current = (): boolean => seq === tasksLoadSeq && generation === taskFolderGeneration;
  tasksRefresh.disabled = true;
  delete tasksStatus.dataset.tone;
  tasksStatus.textContent = "Reading tasks…";
  try {
    const folder = await command<TaskFolderStatus>("get_task_folder");
    if (!current()) return;
    tasksFolder.textContent = folder.folder ? `Folder: ${folder.folder}` : "";
    tasksFolder.title = folder.folder ?? "";
    const problem = taskFolderProblem(folder);
    tasksProblem.hidden = problem === null;
    tasksProblemText.textContent = problem ?? "";
    if (problem !== null) {
      resetTasksPanel();
      tasksDetail.replaceChildren();
      tasksStatus.textContent = "";
      return;
    }
    // One request per state: a group that fails shows its own error and
    // the others still render.
    const requests = TASK_STATE_ORDER.map((state) => ({ state, offset: taskGroupOffsets.get(state) ?? 0 }));
    const results = await Promise.allSettled(
      requests.map(async ({ state, offset }) =>
        parseTaskPage(state, offset, await readTasks(TASK_LIST_QUERY, { state, offset })),
      ),
    );
    if (!current()) return;
    const pages = results.map((result, index): TaskGroupPage => {
      const { state, offset } = requests[index];
      return result.status === "fulfilled"
        ? result.value.page
        : { state, offset, total: 0, items: [], error: errorText(result.reason) };
    });
    taskGroupPages = new Map(pages.map((page) => [page.state, page]));
    renderUnreadableTasks(
      mergeUnreadable(results.flatMap((result) => (result.status === "fulfilled" ? [result.value.unreadable] : []))),
    );
    renderTaskGroups();
    const failed = pages.filter((page) => page.error !== undefined);
    const readAt = `Read at ${new Date().toLocaleTimeString()}.`;
    if (failed.length === 0) {
      tasksStatus.textContent = readAt;
    } else {
      tasksStatus.dataset.tone = "error";
      tasksStatus.textContent =
        failed.length === pages.length
          ? `Tasks could not be read: ${failed[0].error}`
          : `${readAt} ${failed.length === 1 ? "1 group" : `${failed.length} groups`} could not be read.`;
    }
    if (selectedTaskId !== null) {
      void loadTaskDetail(selectedTaskId);
    } else if (!tasksDetail.dataset.taskId) {
      renderTaskDetailMessage("Choose a task to read it.");
    }
  } catch (error) {
    if (!current()) return;
    tasksStatus.dataset.tone = "error";
    tasksStatus.textContent = `Tasks could not be read: ${errorText(error)}`;
  } finally {
    if (current()) tasksRefresh.disabled = false;
  }
}

function renderUnreadableTasks(files: readonly UnreadableTaskFile[]): void {
  const summary = unreadableSummary(files);
  tasksUnreadable.hidden = summary === null;
  tasksUnreadableSummary.textContent = summary ?? "";
  tasksUnreadableList.replaceChildren(
    ...files.map((file) => {
      const item = document.createElement("li");
      item.append(textSpan("tasks-unreadable-file", file.file), document.createTextNode(` — ${file.error}`));
      return item;
    }),
  );
}

function renderTaskGroups(): void {
  const groups = orderedGroups(taskGroupPages.values());
  if (groups.length === 0) {
    const empty = document.createElement("p");
    empty.className = "view-hint";
    empty.textContent = taskGroupPages.size > 0 ? "No tasks yet. Ask an agent to create one." : "";
    tasksList.replaceChildren(empty);
    return;
  }
  tasksList.replaceChildren(...groups.map(taskGroupElement));
}

function taskGroupElement(page: TaskGroupPage): HTMLDetailsElement {
  const group = document.createElement("details");
  group.className = "tasks-group";
  group.dataset.state = page.state;
  group.open = taskGroupsOpen.has(page.state);
  group.addEventListener("toggle", () => {
    if (group.open) taskGroupsOpen.add(page.state);
    else taskGroupsOpen.delete(page.state);
  });
  const summary = document.createElement("summary");
  summary.textContent = groupHeading(page);
  if (page.error !== undefined) {
    const error = document.createElement("p");
    error.className = "view-hint tasks-group-error";
    error.dataset.tone = "error";
    error.textContent = page.error;
    group.append(summary, error);
    return group;
  }
  const rows = document.createElement("ul");
  rows.className = "tasks-rows";
  for (const task of page.items) {
    const row = document.createElement("button");
    row.type = "button";
    row.className = "tasks-row";
    row.dataset.taskId = task.id;
    if (task.id === selectedTaskId) row.setAttribute("aria-current", "true");
    const meta = textSpan("tasks-row-meta", `${task.owner ?? "Unassigned"} · ${formatTaskTime(task.updatedAt)}`);
    meta.title = `Updated ${task.updatedAt}`;
    row.append(textSpan("tasks-row-id", task.id), textSpan("tasks-row-title", task.title), meta);
    const item = document.createElement("li");
    item.append(row);
    rows.append(item);
  }
  group.append(summary, rows);
  const range = pageRangeLabel(page);
  if (range !== null) {
    const pager = document.createElement("div");
    pager.className = "tasks-pager";
    pager.append(textSpan("tasks-pager-range", range));
    for (const [label, offset] of [["Previous", previousOffset(page)], ["Next", nextOffset(page)]] as const) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "ghost";
      button.textContent = label;
      button.disabled = offset === null;
      button.dataset.taskPageState = page.state;
      button.dataset.taskPageOffset = String(offset ?? 0);
      pager.append(button);
    }
    group.append(pager);
  }
  return group;
}

function selectTask(id: string): void {
  selectedTaskId = id;
  for (const row of tasksList.querySelectorAll<HTMLElement>(".tasks-row")) {
    if (row.dataset.taskId === id) row.setAttribute("aria-current", "true");
    else row.removeAttribute("aria-current");
  }
  void loadTaskDetail(id);
}

async function loadTaskDetail(id: string): Promise<void> {
  const seq = ++taskDetailSeq;
  if (taskFolderPending) {
    // The selection reads the chosen task again when it settles.
    renderTaskDetailMessage("Waiting for the task folder to change…");
    return;
  }
  const generation = taskFolderGeneration;
  const current = (): boolean => seq === taskDetailSeq && generation === taskFolderGeneration;
  // A refresh of the open task replaces it in place, without a flash.
  if (tasksDetail.dataset.taskId !== id) renderTaskDetailMessage(`Reading ${id}…`);
  try {
    const task = parseTaskDetail(await readTasks(TASK_DETAIL_QUERY, { id }));
    if (!current()) return;
    renderTaskDetail(task);
  } catch (error) {
    if (!current()) return;
    renderTaskDetailMessage(`${id} could not be read: ${errorText(error)}`, true);
  }
}

function renderTaskDetailMessage(message: string, error = false): void {
  delete tasksDetail.dataset.taskId;
  const text = document.createElement("p");
  text.className = "view-hint";
  if (error) text.dataset.tone = "error";
  text.textContent = message;
  tasksDetail.replaceChildren(text);
}

function taskFact(list: HTMLDListElement, label: string, value: Node | string, title?: string): void {
  const term = document.createElement("dt");
  term.textContent = label;
  const detail = document.createElement("dd");
  if (typeof value === "string") detail.textContent = value;
  else detail.append(value);
  if (title) detail.title = title;
  list.append(term, detail);
}

function renderTaskDetail(task: TaskDetail): void {
  tasksDetail.dataset.taskId = task.id;
  const head = document.createElement("div");
  head.className = "task-detail-head";
  const title = document.createElement("h3");
  title.textContent = task.title;
  const state = textSpan("state-pill task-state", TASK_STATE_LABELS[task.state]);
  state.dataset.state = task.state;
  head.append(textSpan("task-detail-id", task.id), title, state);

  const requestLabel = document.createElement("h4");
  requestLabel.textContent = "Original request";
  // Verbatim: every line break and space as stored, never shortened.
  const request = document.createElement("pre");
  request.className = "task-request";
  request.textContent = task.request;

  const facts = document.createElement("dl");
  facts.className = "task-facts";
  taskFact(facts, "Source", task.source);
  taskFact(facts, "Outcome", task.outcome || "Not set");
  taskFact(facts, "State", TASK_STATE_LABELS[task.state]);
  taskFact(facts, "Owner", task.owner ?? "Unassigned");
  taskFact(facts, "Reviewer", task.reviewer ?? "None");
  if (task.parent) {
    const parent = document.createElement("button");
    parent.type = "button";
    parent.className = "ghost task-parent";
    parent.dataset.taskId = task.parent;
    parent.textContent = task.parent;
    parent.title = `Open ${task.parent}`;
    taskFact(facts, "Parent", parent);
  } else {
    taskFact(facts, "Parent", "None");
  }
  if (task.evidence.length > 0) {
    const evidence = document.createElement("ul");
    evidence.className = "task-evidence";
    for (const link of task.evidence) {
      const item = document.createElement("li");
      item.textContent = link;
      evidence.append(item);
    }
    taskFact(facts, "Evidence", evidence);
  } else {
    taskFact(facts, "Evidence", "None");
  }
  taskFact(facts, "Created", formatTaskTime(task.createdAt), task.createdAt);
  taskFact(facts, "Updated", formatTaskTime(task.updatedAt), task.updatedAt);

  const logLabel = document.createElement("h4");
  logLabel.textContent = `History · ${task.log.length}`;
  const log = document.createElement("ol");
  log.className = "task-log";
  // Stored oldest first, so the newest change is last.
  for (const entry of task.log) {
    const item = document.createElement("li");
    const meta = textSpan("task-log-meta", `${formatTaskTime(entry.at)} · ${entry.by}`);
    meta.title = entry.at;
    item.append(meta, textSpan("task-log-what", entry.what));
    if (entry.note) item.append(textSpan("task-log-note", entry.note));
    log.append(item);
  }
  tasksDetail.replaceChildren(head, requestLabel, request, facts, logLabel, log);
}

async function loadTaskFolderSetting(): Promise<void> {
  // A pending selection reads the status again when it settles.
  if (taskFolderPending) return;
  const generation = taskFolderGeneration;
  try {
    const status = await command<TaskFolderStatus>("get_task_folder");
    if (generation !== taskFolderGeneration) return;
    renderTaskFolderSetting(status);
  } catch (error) {
    if (generation !== taskFolderGeneration) return;
    settingsTaskFolderState.textContent = `Unavailable: ${errorText(error)}`;
    settingsTaskFolderState.dataset.tone = "error";
  }
}

function renderTaskFolderSetting(status: TaskFolderStatus): void {
  settingsTaskFolder.textContent = status.folder ?? "—";
  settingsTaskFolder.title = status.folder ?? "";
  settingsTaskFolderState.textContent = taskFolderStateText(status);
  if (status.status === "error") settingsTaskFolderState.dataset.tone = "error";
  else delete settingsTaskFolderState.dataset.tone;
  if (!taskFolderInput.value && status.folder && document.activeElement !== taskFolderInput) {
    taskFolderInput.value = status.folder;
  }
}

function showTaskFolderResult(message: string, tone: "ok" | "error" | null): void {
  taskFolderResult.textContent = message;
  if (tone) taskFolderResult.dataset.tone = tone;
  else delete taskFolderResult.dataset.tone;
}

function syncTaskFolderControls(): void {
  // A held trial keeps them disabled (installTrialReadOnlyControls).
  if (trialStatus !== null) return;
  taskFolderUse.disabled = taskFolderPending;
  taskFolderBrowse.disabled = taskFolderPending;
}

async function useTaskFolder(path: string): Promise<void> {
  if (taskFolderPending) return;
  // The selection boundary: every task read already in flight belongs to
  // the folder before it, whichever folder this one ends on.
  taskFolderPending = true;
  taskFolderGeneration += 1;
  syncTaskFolderControls();
  showTaskFolderResult("Opening the folder…", null);
  try {
    const status = await command<TaskFolderStatus>("set_task_folder", { path });
    renderTaskFolderSetting(status);
    taskFolderInput.value = status.folder ?? path;
    showTaskFolderResult(`Tasks now come from ${status.folder ?? path}. Nothing was copied or moved.`, "ok");
    resetTasksPanel();
  } catch (error) {
    // The previous folder is still selected; say why in the backend's words.
    showTaskFolderResult(errorText(error), "error");
  } finally {
    taskFolderPending = false;
    syncTaskFolderControls();
    // Settled either way: read again what the dropped reads would have shown.
    void loadTaskFolderSetting();
    if (shellView === "tasks") void loadTasks();
  }
}

async function browseTaskFolder(): Promise<void> {
  if (taskFolderPending) return;
  try {
    const path = await command<string | null>("choose_task_folder");
    if (path) {
      taskFolderInput.value = path;
      await useTaskFolder(path);
    }
  } catch (error) {
    showTaskFolderResult(errorText(error), "error");
  }
}

/* ── Settings → Idle hibernation ──
   Native owns sleep eligibility and the saved policy. These fields only
   protect an unsaved edit and a setter response from an older bootstrap. */
let idleHibernationSetting: number | null | undefined;
let idleHibernationPending = false;
let idleHibernationDirty = false;
let idleHibernationGeneration = 0;

function syncIdleHibernationControls(): void {
  const disabled = trialStatus !== null || idleHibernationPending || idleHibernationSetting === undefined;
  idleHibernationEnabled.disabled = disabled;
  idleHibernationMinutes.disabled = disabled || !idleHibernationEnabled.checked;
  idleHibernationSave.disabled = disabled;
}

function renderIdleHibernationSetting(value: unknown): void {
  idleHibernationSetting = renderIdleHibernationCapability(idleHibernationControls, value);
  if (idleHibernationSetting === undefined) {
    idleHibernationStatus.textContent = "Idle hibernation setting unavailable in this backend.";
    idleHibernationStatus.dataset.tone = "error";
  } else if (!idleHibernationDirty && !idleHibernationPending) {
    idleHibernationEnabled.checked = idleHibernationSetting !== null;
    idleHibernationMinutes.value = String(idleHibernationSetting ?? 60);
    idleHibernationStatus.textContent = idleHibernationSetting === null
      ? "Idle hibernation is off."
      : `Eligible room members sleep after ${idleHibernationSetting} idle minute${idleHibernationSetting === 1 ? "" : "s"}.`;
    idleHibernationStatus.dataset.tone = "";
  }
  syncIdleHibernationControls();
}

async function saveIdleHibernationSetting(): Promise<void> {
  if (trialStatus !== null || idleHibernationPending || idleHibernationSetting === undefined) return;
  let minutes: number | null;
  try {
    minutes = idleHibernateMinutesFromInput(idleHibernationEnabled.checked, idleHibernationMinutes.value);
  } catch (error) {
    idleHibernationStatus.textContent = errorText(error);
    idleHibernationStatus.dataset.tone = "error";
    return;
  }
  idleHibernationPending = true;
  idleHibernationGeneration += 1;
  syncIdleHibernationControls();
  idleHibernationStatus.textContent = "Saving idle hibernation…";
  idleHibernationStatus.dataset.tone = "";
  try {
    const saved = await command<{ idle_minutes: number | null }>("set_idle_hibernation", {
      request: { idle_minutes: minutes },
    });
    if (readIdleHibernateMinutes(saved?.idle_minutes) === undefined) throw new Error("The backend did not return a valid idle hibernation setting. Refresh state to check it.");
    idleHibernationPending = false;
    idleHibernationDirty = false;
    renderIdleHibernationSetting(saved.idle_minutes);
    idleHibernationStatus.dataset.tone = "ok";
  } catch (error) {
    idleHibernationStatus.textContent = errorText(error);
    idleHibernationStatus.dataset.tone = "error";
  } finally {
    idleHibernationPending = false;
    idleHibernationGeneration += 1;
    syncIdleHibernationControls();
  }
}

function wireIdleHibernationSetting(): void {
  const edit = (): void => { idleHibernationDirty = true; syncIdleHibernationControls(); };
  idleHibernationEnabled.addEventListener("change", edit);
  idleHibernationMinutes.addEventListener("input", edit);
  idleHibernationSave.addEventListener("click", () => void saveIdleHibernationSetting());
}

/* ── Settings → Personal repository ──
   The task folder's shape: one selection at a time, and a status read that
   started before a selection is dropped once the selection settles. */
let personalRepoPending = false,
  personalRepoGeneration = 0,
  personalRepoEnvironment = false;

async function loadPersonalRepoSetting(): Promise<void> {
  if (personalRepoPending) return;
  const generation = personalRepoGeneration;
  try {
    const status = await command<PersonalRepoStatus>("get_personal_repo");
    if (generation !== personalRepoGeneration) return;
    renderPersonalRepoSetting(status);
  } catch (error) {
    if (generation !== personalRepoGeneration) return;
    settingsPersonalRepoState.textContent = `Unavailable: ${errorText(error)}`;
    settingsPersonalRepoState.dataset.tone = "error";
  }
}

function renderPersonalRepoSetting(status: PersonalRepoStatus): void {
  settingsPersonalRepo.textContent = status.folder ?? "—";
  settingsPersonalRepo.title = status.folder ?? "";
  settingsPersonalRepoState.textContent = personalRepoStateText(status);
  if (status.status === "error") settingsPersonalRepoState.dataset.tone = "error";
  else delete settingsPersonalRepoState.dataset.tone;
  if (!personalRepoInput.value && status.folder && document.activeElement !== personalRepoInput) {
    personalRepoInput.value = status.folder;
  }
  personalRepoEnvironment = personalRepoLocked(status);
  syncPersonalRepoControls();
}

function showPersonalRepoResult(message: string, tone: "ok" | "error" | null): void {
  personalRepoResult.textContent = message;
  if (tone) personalRepoResult.dataset.tone = tone;
  else delete personalRepoResult.dataset.tone;
}

function syncPersonalRepoControls(): void {
  // A held trial keeps them disabled (installTrialReadOnlyControls).
  if (trialStatus !== null) return;
  personalRepoUse.disabled = personalRepoPending || personalRepoEnvironment;
  personalRepoBrowse.disabled = personalRepoPending || personalRepoEnvironment;
  personalRepoInput.disabled = personalRepoEnvironment;
  const why = personalRepoEnvironment ? "PRIM1_PERSONAL_REPO decides the personal repository for this PRIM-1." : "";
  for (const control of [personalRepoUse, personalRepoBrowse, personalRepoInput]) control.title = why;
}

async function usePersonalRepo(path: string): Promise<void> {
  if (personalRepoPending) return;
  personalRepoPending = true;
  personalRepoGeneration += 1;
  syncPersonalRepoControls();
  showPersonalRepoResult("Checking the folder…", null);
  try {
    const status = await command<PersonalRepoStatus>("set_personal_repo", { path });
    renderPersonalRepoSetting(status);
    personalRepoInput.value = status.folder ?? path;
    showPersonalRepoResult(`The session register is now written to ${status.folder ?? path}. Nothing was moved.`, "ok");
  } catch (error) {
    // The previous folder is still in use; say why in the backend's words.
    showPersonalRepoResult(errorText(error), "error");
  } finally {
    personalRepoPending = false;
    syncPersonalRepoControls();
    void loadPersonalRepoSetting();
  }
}

async function browsePersonalRepo(): Promise<void> {
  if (personalRepoPending) return;
  try {
    const path = await command<string | null>("choose_personal_repo");
    if (path) {
      personalRepoInput.value = path;
      await usePersonalRepo(path);
    }
  } catch (error) {
    showPersonalRepoResult(errorText(error), "error");
  }
}

function wirePersonalRepo(): void {
  personalRepoUse.addEventListener("click", () => void usePersonalRepo(personalRepoInput.value));
  personalRepoInput.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      void usePersonalRepo(personalRepoInput.value);
    }
  });
  personalRepoBrowse.addEventListener("click", () => void browsePersonalRepo());
}

function wireTasks(): void {
  renderTaskDetailMessage("Choose a task to read it.");
  tasksRefresh.addEventListener("click", () => void loadTasks());
  tasksOpenSettings.addEventListener("click", () => setShellView("settings"));
  tasksList.addEventListener("click", (event) => {
    if (!(event.target instanceof Element)) return;
    const pageButton = event.target.closest<HTMLButtonElement>("[data-task-page-state]");
    if (pageButton) {
      taskGroupOffsets.set(pageButton.dataset.taskPageState as TaskState, Number(pageButton.dataset.taskPageOffset));
      void loadTasks();
      return;
    }
    const row = event.target.closest<HTMLElement>(".tasks-row");
    if (row?.dataset.taskId) selectTask(row.dataset.taskId);
  });
  tasksDetail.addEventListener("click", (event) => {
    if (!(event.target instanceof Element)) return;
    const parent = event.target.closest<HTMLElement>(".task-parent");
    if (parent?.dataset.taskId) selectTask(parent.dataset.taskId);
  });
  taskFolderUse.addEventListener("click", () => void useTaskFolder(taskFolderInput.value));
  taskFolderInput.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      void useTaskFolder(taskFolderInput.value);
    }
  });
  taskFolderBrowse.addEventListener("click", () => void browseTaskFolder());
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
    case "tasks":
    case "settings":
    case "help":
    case "themes":
    case "telemetry": {
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
      terminalLayout.request();
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
  const state = async (): Promise<WindowRectState> => {
    const [fullscreen, maximized, minimized, visible, size, position] = await Promise.all([
      win.isFullscreen(), win.isMaximized(), win.isMinimized(), win.isVisible(), win.outerSize(), win.outerPosition()]);
    // outerSize/outerPosition are physical px; screen.avail* are logical (CSS) px.
    return { fullscreen, maximized, minimized, visible, width: size.width, height: size.height, x: position.x,
      y: position.y, availWidth: window.screen.availWidth, availHeight: window.screen.availHeight,
      dpr: window.devicePixelRatio || 1 };
  };
  try {
    let current = await state();
    if (windowRectAction(current) === "show") {
      await win.show();
      writeSystem("warn", "window came back hidden after a state change — shown again");
      current = await state();
    }
    // v3 W6 (E): never resize, centre or show a minimized window (see window-rect.ts).
    if (windowRectAction(current) !== "resize") {
      return;
    }
    const width = Math.min(1560, Math.max(960, current.availWidth - 48));
    const height = Math.min(980, Math.max(600, current.availHeight - 48));
    await win.setSize(new LogicalSize(width, height));
    await win.center();
    const degenerate = current.width < 640 || current.height < 400 || current.x < -4000 || current.y < -4000;
    writeSystem(
      "info",
      `window restored: was ${current.width}x${current.height} @${current.x},${current.y} (${degenerate ? "invalid" : "larger than the screen"}); now ${width}x${height} centred`,
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
document.addEventListener("visibilitychange", syncThemeEffects);
document.addEventListener("visibilitychange", () => terminalLayout.visibilityChanged());
themeMotionPreference.addEventListener("change", syncThemeEffects);

wireSessionUi();
const roomFallbacks = installRoomFallbacks(
  must<HTMLDetailsElement>("#room-fallback-settings"),
  must<HTMLElement>("#room-fallback-summary"),
  must<HTMLElement>("#room-fallback-editor"),
  {
    get: (roomId) => command("get_room_fallbacks", { roomId }),
    set: (request) => command("set_room_fallbacks", { request }),
    canEdit: () => trialStatus === null,
  },
);
wireRoomUi();
wireControls();
wireResize();
wireTerminalShortcuts();
wireKeysSettings();
wireThemesPage();
markCurrentThemeCard(currentThemeName());
wireWindowControls();
wireCornerButton();
wireTelemetry();
wireTasks();
wirePersonalRepo();
wireIdleHibernationSetting();
wireAttachMenu();
wireTabMenu();
void settleWindow();

const runtimeEventContext: RuntimeEventContext = {
  writeSystem,
  refreshSnapshotFromEvent,
  snapshotById,
  pendingOutput,
  runEventGate: createRunEventGateState(),
  admitOutput: (credit) => outputCredits.admit(credit),
  settleOutput: (credit, disposition) => outputCredits.settle(credit, disposition),
  writeToPane(sessionId, chunk, onParsed) {
    const pane = paneMap.get(sessionId);
    if (pane) {
      const current = snapshotById.get(sessionId);
      if (current) pane.observeRun(current);
      pane.write(chunk, onParsed);
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
  showUpgradeStatus(status) {
    showUpgradeStatus(status);
  },
  requestRepaintResync(sessionId) {
    if (appShell.dataset.log !== "open") cornerButton.dataset.alert = "true";
    const pane = paneMap.get(sessionId);
    if (!pane) {
      return;
    }
    const current = snapshotById.get(sessionId);
    if (current) pane.observeRun(current);
    if (current?.running && !current.output_draining) pane.forceFullRepaintResyncNow();
    writeSystem(
      "warn",
      `${pane.label}: display output is incomplete. ${current?.running && !current.output_draining ? "A repaint was requested. " : ""}Lost history and screen positioning cannot be reconstructed.`,
    );
  },
};

void initializeUi().catch((error) => {
  if (trialStatus === undefined) showTrialHold(TRIAL_STATUS_UNAVAILABLE_TEXT);
  else if (trialStatus) showTrialHold("Preview held — renderer check failed. Reload the preview to retry. Agents stay stopped.");
  writeSystem("error", `UI initialization failed: ${String(error)}`);
});

/** Measurement probes for the S3 fixture: on only in automation mode. */
const perfProbes = new PerfProbes();
let perfEnabled = false;

/** The rendered cursor line up to the cursor: what the eye sees of the line
    being typed, after xterm's parser has applied every escape sequence. */
function textBeforeCursor(terminal: Terminal): string {
  const buffer = terminal.buffer.active;
  const line = buffer.getLine(buffer.baseY + buffer.cursorY);
  return line ? line.translateToString(false, 0, buffer.cursorX) : "";
}

async function initializeUi(): Promise<void> {
  trialStatus = await loadTrialStatus(() => command<unknown>("trial_status"));
  if (trialStatus) {
    showTrialHold(TRIAL_HOLD_TEXT);
    installTrialReadOnlyControls();
  }
  const automationMode = await command<boolean>("automation_mode");
  exposeAutomationBridge(window, automationMode, {
    core: { invoke },
    event: { listen },
    window: { getCurrentWindow },
  });
  perfEnabled = exposePerfProbes(window, automationMode, perfProbes);
  await completeDesktopStartup(
    trialStatus,
    () => registerRuntimeEventsBeforeBootstrap(
      () => listen<RuntimeEvent>("runtime://event", ({ payload }) => {
          handleRuntimeEvent(payload, runtimeEventContext);
        }),
      bootstrap,
    ),
    () => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))),
    (challenge) => command("trial_renderer_acknowledged", { challenge }),
    () => { void resumeAfterStartup(); },
  );
}

/* ── Self-upgrade (A108) ──
   The supervisor drives the upgrade; the window shows its notice with Cancel,
   a held attempt with Abandon, and tells it when room composers hold unsent
   text (an upgrade does not close over a draft). */
const upgradeBanner = must<HTMLElement>("#upgrade-banner");
const upgradeBannerTitle = must<HTMLElement>("#upgrade-banner-title");
const upgradeBannerCountdown = must<HTMLElement>("#upgrade-banner-countdown");
const upgradeBannerBody = must<HTMLElement>("#upgrade-banner-body");
const upgradeCancel = must<HTMLButtonElement>("#upgrade-cancel");
const upgradeAbandon = must<HTMLButtonElement>("#upgrade-abandon");
const upgradeDismiss = must<HTMLButtonElement>("#upgrade-dismiss");
let upgradeStatus: UpgradeStatus | null = null;
let upgradeDismissedKey: string | null = null;
let upgradeCountdownTimer: number | null = null;
/** Members the upgrade resumes itself; the restart resume leaves them alone. */
let upgradeOwnedMembers = new Set<string>();

function showUpgradeStatus(status: UpgradeStatus | null): void {
  upgradeStatus = status;
  renderUpgradeBanner();
}

function renderUpgradeBanner(): void {
  const view = upgradeBannerView(upgradeStatus, Date.now());
  const key = upgradeStatus ? `${upgradeStatus.attempt_id}:${upgradeStatus.stage}` : null;
  upgradeBanner.hidden = !view.visible || key === upgradeDismissedKey;
  upgradeBanner.dataset.stage = upgradeStatus?.stage ?? "";
  upgradeBannerTitle.textContent = view.title;
  upgradeBannerCountdown.textContent = view.countdown ?? "";
  upgradeBannerCountdown.hidden = view.countdown === null;
  upgradeBannerBody.textContent = view.body;
  upgradeCancel.hidden = !view.canCancel;
  upgradeAbandon.hidden = !view.canAbandon;
  upgradeDismiss.hidden = !view.canDismiss;
  if (view.countdown !== null && upgradeCountdownTimer === null) {
    upgradeCountdownTimer = window.setInterval(renderUpgradeBanner, 1000);
  } else if (view.countdown === null && upgradeCountdownTimer !== null) {
    window.clearInterval(upgradeCountdownTimer);
    upgradeCountdownTimer = null;
  }
}

upgradeCancel.addEventListener("click", () => {
  const status = upgradeStatus;
  if (!status) return;
  upgradeCancel.disabled = true;
  void command<void>("cancel_upgrade", { attemptId: status.attempt_id })
    .catch((error: unknown) => writeSystem("error", `The upgrade was not cancelled: ${errorText(error)}`))
    .finally(() => { upgradeCancel.disabled = false; });
});

upgradeAbandon.addEventListener("click", () => {
  const status = upgradeStatus;
  if (!status) return;
  void afterConfirm(
    () => confirmAction(abandonUpgradeConfirmText(status), "Abandon"),
    async () => {
      try {
        await command<void>("abandon_held_upgrade", { attemptId: status.attempt_id });
        showUpgradeStatus(await command<UpgradeStatus | null>("upgrade_status"));
      } catch (error) {
        writeSystem("error", `The held upgrade was not abandoned: ${errorText(error)}`);
      }
    },
  );
});

upgradeDismiss.addEventListener("click", () => {
  if (upgradeStatus) upgradeDismissedKey = `${upgradeStatus.attempt_id}:${upgradeStatus.stage}`;
  renderUpgradeBanner();
});

/** Report the room composers' unsent text whenever it changes; the window's
    first report says whether any exists at all. */
let reportedRoomDrafts: boolean | null = null;
function reportRoomDrafts(): void {
  if (trialStatus !== null) return;
  const open = roomDrafts.hasUnsentText(roomMessage.value);
  if (open === reportedRoomDrafts) return;
  reportedRoomDrafts = open;
  void command<void>("report_room_drafts", { open }).catch(() => { reportedRoomDrafts = null; });
}
roomMessage.addEventListener("input", reportRoomDrafts);
window.setInterval(reportRoomDrafts, 1000);

/** After startup: the upgrade first names the members it resumes itself,
    then the ordinary restart resume handles the rest. */
async function resumeAfterStartup(): Promise<void> {
  reportRoomDrafts();
  try {
    const status = await command<UpgradeStatus | null>("begin_upgrade_resume");
    upgradeOwnedMembers = upgradeResumeMembers(status);
    showUpgradeStatus(status);
  } catch (error) {
    writeSystem("warn", `Upgrade status is unavailable: ${errorText(error)}`);
  }
  resumeMarkedRooms();
}

function showTrialHold(message: string): void {
  const hold = must<HTMLElement>("#trial-hold");
  hold.textContent = message;
  hold.title = message;
  hold.hidden = false;
  must<HTMLElement>("#telemetry-line").hidden = true;
}

/** Navigation, feed selection and theme preferences remain usable. Disabled
 * controls explain the backend's immutable hold; command() is the key-path
 * backstop, and the supervisor still refuses raw IPC mutations. */
function installTrialReadOnlyControls(): void {
  const selectors = [
    "#new-session-button", "[data-attach]", "[data-tab-menu]", "[data-tab-action]",
    "[data-edit-session]", "[data-action][data-session]",
    "#session-form :is(input,select,textarea,button):not(#cancel-session-form)",
    "#room-create-form :is(input,select,textarea,button):not(#cancel-room-create)",
    "#room-brief-settings :is(input,textarea,button)",
    "#new-room", "#rename-room", "#save-room", "[data-room-load]", "#delete-room", "#move-room-left", "#move-room-right",
    "#room-resume-toggle", "#room-resume-now", "#room-members button",
    "#room-add-member", "#room-add-member-select", "#room-invite-guest", "#room-guest-list button",
    "#room-message", "#room-recipient",
    "#room-post", "#room-send", "[data-trial-mutation]", "#workspace-grid button",
    ".room-typed-card > button.room-readable-more",
  ].join(",");
  const disable = (): void => {
    for (const control of document.querySelectorAll<HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement | HTMLButtonElement>(selectors)) {
      if (!control.disabled) control.disabled = true;
      if (control.title !== TRIAL_HOLD_TEXT) control.title = TRIAL_HOLD_TEXT;
    }
  };
  disable();
  // The existing renderers update availability after room/tab changes. Keep
  // the hold visible without adding trial conditionals to every renderer.
  new MutationObserver(disable).observe(must("#app"), { childList: true, subtree: true, attributes: true, attributeFilter: ["disabled"] });
}

async function bootstrap(): Promise<void> {
  const reset = await command<{ epoch: string; display_reset: boolean; discarded: { bytes: number; items: number } }>("begin_output_renderer");
  outputCredits.epoch = reset.epoch;
  await applyPrimeAvailability();
  await refreshSnapshot();
  if (reset.display_reset) {
    writeSystem("warn", `Display stream reset after page replacement: ${reset.discarded.items} dispatched output item(s) (${reset.discarded.bytes} reserved bytes) were disposed, not parsed. Additional queued native output may be disposed during reset. Previous terminal scrollback is lost; a TUI repaint cannot restore that history.`);
  }
  // Readiness failure must not abort room navigation/resume. Native observation
  // continues with an explicit display gap until the same epoch becomes ready.
  const readiness = retryOutputRendererReady(reset.epoch, reset.display_reset);
  // A held trial must not issue its renderer-health receipt before this
  // transport is attached. Normal startup can continue while display retries.
  if (trialStatus && !(await readiness)) {
    throw new Error("Held trial terminal display did not attach; reload to retry the renderer check");
  }
  writeSystem("info", "UI attached to supervisor.");
  enterFirstRoomOnFirstBoot();
}

let outputReadyPending: Promise<boolean> | null = null;
const retryOutputReady = document.querySelector<HTMLButtonElement>("#retry-output-ready")!;
retryOutputReady.addEventListener("click", () => {
  if (trialStatus === null && outputCredits.epoch) void retryOutputRendererReady(outputCredits.epoch, true);
});

function retryOutputRendererReady(epoch: string, repaint: boolean): Promise<boolean> {
  if (outputReadyPending) return outputReadyPending;
  retryOutputReady.disabled = true;
  outputReadyPending = command<void>("output_renderer_ready", { epoch }).then(() => {
    if (outputCredits.epoch !== epoch) return false;
    retryOutputReady.hidden = true;
    writeSystem("info", "Terminal display attached. Any previously omitted output remains incomplete.");
    if (repaint) {
      for (const [sessionId, pane] of paneMap) {
        const snapshot = snapshotById.get(sessionId);
        if (snapshot?.running && !snapshot.output_draining) pane.forceFullRepaintResyncNow();
      }
    }
    return true;
  }).catch((error) => {
    if (outputCredits.epoch !== epoch) return false;
    const held = trialStatus !== null;
    retryOutputReady.hidden = held;
    const failure = held ? "Preview terminal display did not attach." : "Terminal display is paused; native observation continues.";
    const hint = held ? "Reload the preview to retry its renderer check." : "Use Retry terminal display in the system log.";
    writeSystem("error", `${failure} ${String(error)}. ${hint}`);
    setLogDrawer(true);
    return false;
  }).finally(() => {
    outputReadyPending = null;
    retryOutputReady.disabled = false;
  });
  return outputReadyPending;
}

/** First boot on the rooms shell with every session already in a room: land
    in the first room, not an empty lobby. Navigation only — entering a room
    never launches its agents. */
function enterFirstRoomOnFirstBoot(): void {
  if (
    !workContextWasStored
    && visibleTabOrder().length === 0
    && roomSnapshots.length > 0
  ) {
    workContextWasStored = true;
    enterWorkContext({ kind: "room", roomId: roomSnapshots[0].room_id });
  }
}

/* ── Resume on restart (Victor, 13 Oct; PLAN §5-3, E8/E9) ──
   Only rooms the operator marked come back after a restart: their members that
   were running when PRIM-1 went down relaunch in the background, the
   foreground member first, at most RESUME_CONCURRENCY at a time. Every other
   agent stays asleep with a Wake button; a room left unmarked whose agents
   were running offers Resume instead of pausing silently. */
const RESUME_CONCURRENCY = 2; // provisional until measured (E9)
/** After this long a still-starting member is reported, but keeps its slot:
    the limit is the number of members starting at once (Codex review). */
const RESUME_SLOW_START_MS = 15_000;
let bootResumeDone = false;

function wasRunningAtShutdown(session: SessionSnapshot | undefined): boolean {
  return Boolean(session && session.was_running_at_shutdown && !session.running && !session.hibernated);
}

function resumeMarkedRooms(): void {
  if (bootResumeDone || trialStatus !== null) {
    return;
  }
  bootResumeDone = true;
  const marked = roomSnapshots.filter((room) => room.resume_on_restart);
  const plan = restartResumePlan(
    roomSnapshots,
    snapshotById,
    tabState.activeId,
  );
  // A108: members an upgrade resumes in their exact conversation are its own.
  const queue = plan.resume.filter((sessionId) => !upgradeOwnedMembers.has(sessionId));
  const leftAsleep = plan.leftAsleep.filter((sessionId) => !upgradeOwnedMembers.has(sessionId));
  if (queue.length > 0) {
    writeSystem(
      "info",
      `Restarted: resuming ${queue.length} agent${queue.length === 1 ? "" : "s"} in the rooms you marked (${marked
        .map((room) => room.label)
        .join(", ")}).${leftAsleep.length > 0 ? ` ${leftAsleep.length} other agent${leftAsleep.length === 1 ? " was" : "s were"} running and ${leftAsleep.length === 1 ? "is" : "are"} asleep — each room offers Resume.` : ""}`,
    );
    resumeInBackground(queue, "boot");
    void announceRestartWhenSettled(marked, queue);
  } else if (leftAsleep.length > 0) {
    writeSystem(
      "info",
      `Restarted: no marked room had running agents, so all ${leftAsleep.length} that were running are asleep — each room offers Resume, or mark a room Resume on restart.`,
    );
  }
}

/** A31: once this boot's resumes have settled (or after two minutes), post one
    notice per marked room naming who is back online and who did not come
    back. A deterministic request id keeps a retried post from duplicating. */
async function announceRestartWhenSettled(
  rooms: readonly RoomSnapshot[],
  resumed: readonly string[],
): Promise<void> {
  const deadline = Date.now() + 120_000;
  while (
    resumed.some((sessionId) => resumeQueue.some((entry) => entry.sessionId === sessionId) || resumeInFlight.has(sessionId))
    && Date.now() < deadline
  ) {
    await new Promise((resolve) => window.setTimeout(resolve, 500));
  }
  const bootId = Date.now().toString(36);
  for (const room of rooms) {
    const members = room.member_ids.filter((sessionId) => resumed.includes(sessionId));
    if (members.length === 0) {
      continue;
    }
    const backOnline = members.filter((sessionId) => snapshotById.get(sessionId)?.running);
    const notBack = members.filter((sessionId) => !snapshotById.get(sessionId)?.running);
    try {
      await command<RoomPostResult>("post_room_message", {
        request: {
          room_id: room.room_id,
          content: restartNotice(room.label, backOnline.map(paneLabel), notBack.map(paneLabel)),
          request_id: `${PRIM1_POST_REQUEST_PREFIX}restart-${bootId}-${room.room_id}`,
        } satisfies PostRoomMessageRequest,
      });
    } catch (error) {
      writeSystem("warn", `Restart notice for ${room.label} was not stored: ${String(error)}`);
    }
  }
}

/** One resume queue shared by the boot resume and every Resume button, so at
    most RESUME_CONCURRENCY members are starting at once across all of them
    (Codex U3). Launches never take focus. A slot frees only when its member
    leaves "starting" (ready, busy, stalled, failed or stopped); a slow one is
    reported, never skipped past. */
const resumeQueue: BackgroundResumeEntry[] = [];
const resumeInFlight = new Set<string>();

function resumeInBackground(sessionIds: string[], mode: BackgroundResumeMode = "manual"): void {
  enqueueBackgroundResumes(resumeQueue, resumeInFlight, sessionIds, mode);
  pumpResumeQueue();
}

function pumpResumeQueue(): void {
  while (resumeInFlight.size < RESUME_CONCURRENCY && resumeQueue.length > 0) {
    const { sessionId: next, mode } = resumeQueue.shift()!;
    const session = snapshotById.get(next);
    if (!session || session.running) {
      continue;
    }
    resumeInFlight.add(next);
    void (async () => {
      try {
        await launchSession(next, { activate: false, resumeMode: mode });
        await waitForStartupToSettle(next, RESUME_SLOW_START_MS);
      } finally {
        resumeInFlight.delete(next);
        pumpResumeQueue();
      }
    })();
  }
}

function waitForStartupToSettle(sessionId: string, slowAfterMs: number): Promise<void> {
  const slowAt = Date.now() + slowAfterMs;
  let reportedSlow = false;
  return new Promise((resolve) => {
    const check = (): void => {
      const session = snapshotById.get(sessionId);
      const starting = session?.running
        && (session.lifecycle_state === "starting" || session.lifecycle_state === "restarting");
      if (!starting) {
        resolve();
        return;
      }
      if (!reportedSlow && Date.now() >= slowAt) {
        reportedSlow = true;
        writeSystem(
          "warn",
          `${paneLabel(sessionId)} is still starting after ${Math.round(slowAfterMs / 1000)} s; the remaining resumes wait for a free slot.`,
        );
      }
      window.setTimeout(check, 500);
    };
    check();
  });
}

async function setActiveRoomResume(resume: boolean): Promise<void> {
  const room = activeRoom();
  if (!room) {
    return;
  }
  try {
    const snapshot = await command<RoomSnapshot>("set_room_resume_on_restart", {
      request: { room_id: room.room_id, resume_on_restart: resume } satisfies SetRoomResumeRequest,
    });
    roomSnapshots = roomSnapshots.map((candidate) =>
      candidate.room_id === snapshot.room_id ? snapshot : candidate,
    );
    setRoomStatus(
      resume
        ? `${snapshot.label} will resume its running agents after a restart.`
        : `${snapshot.label} stays asleep after a restart until you wake it.`,
      "info",
    );
  } catch (error) {
    setRoomStatus(`Resume on restart was not changed: ${String(error)}`, "error");
  }
  renderRoomUi();
  renderRoomsOverview();
}

/** Tab menu or recovery banner → Start fresh session: a NEW harness
    conversation. The superseded reference stays in the session's recovery
    history (S5). A running session is stopped first, after confirmation —
    that is how a running held Codex pane gets a fresh conversation. */
async function startFreshSession(sessionId: string): Promise<void> {
  const session = snapshotById.get(sessionId);
  if (session?.running) {
    if (!(await confirmAction(startFreshConfirmText(session.label), "Stop and start fresh"))) {
      return;
    }
    try {
      const stopped = await command<SessionSnapshot>("stop_session", {
        request: { session_id: sessionId } satisfies StopSessionRequest,
      });
      applyCommandSessionSnapshot(stopped);
    } catch (error) {
      writeSystem("error", `Stop ${paneLabel(sessionId)} failed: ${String(error)}`);
      return;
    }
    if (!(await waitUntilStopped(sessionId, 15_000))) {
      writeSystem(
        "warn",
        `${paneLabel(sessionId)} is still stopping; choose Start fresh session again once it has stopped.`,
      );
      return;
    }
  }
  await launchSession(sessionId, { fresh: true });
}

/** Recovery banner → Resume here: a member copied from the live PRIM-1
    continues its own conversation once the operator confirms it is closed there. */
async function resumeHeldConversation(sessionId: string): Promise<void> {
  const session = snapshotById.get(sessionId);
  if (!session || !(await confirmAction(resumeHereConfirmText(session.label), "Resume here"))) return;
  try {
    const released = await command<SessionSnapshot>("resume_held_conversation", {
      request: { session_id: sessionId } satisfies StopSessionRequest,
    });
    applyCommandSessionSnapshot(released);
  } catch (error) {
    writeSystem("error", `Resume ${paneLabel(sessionId)} here failed: ${String(error)}`);
    return;
  }
  await launchSession(sessionId, {});
}

function waitUntilStopped(sessionId: string, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  return new Promise((resolve) => {
    const check = (): void => {
      const session = snapshotById.get(sessionId);
      if (!session || !session.running) {
        resolve(true);
        return;
      }
      if (Date.now() >= deadline) {
        resolve(false);
        return;
      }
      window.setTimeout(check, 250);
    };
    check();
  });
}

async function refreshSnapshot(preferredSessionId?: string): Promise<RuntimeSnapshot> {
  const request = ++latestSnapshotRequest;
  const hibernationGeneration = idleHibernationGeneration;
  const snapshot = await command<RuntimeSnapshot>("bootstrap");
  if (request === latestSnapshotRequest) {
    applySnapshot(snapshot, preferredSessionId, hibernationGeneration === idleHibernationGeneration);
  }
  return snapshot;
}

// Only a generation-checked bootstrap refreshes this global setting; a delayed
// unrelated move/folder command must not overwrite a newer settings response.
function applySnapshot(snapshot: RuntimeSnapshot, preferredSessionId?: string, currentHibernationSetting = false): void {
  const acceptedSessions = new Set<string>();
  for (const session of snapshot.sessions) {
    observeAuthoritativeTerminalGrid(session);
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
  if (currentHibernationSetting && !idleHibernationPending) renderIdleHibernationSetting(snapshot.idle_hibernate_minutes);

  for (const session of snapshot.sessions) {
    if (acceptedSessions.has(session.session_id)) {
      paneMap.get(session.session_id)?.applySnapshot(session, true);
    }
  }
  terminalLayout.request();
  completeInitialRunEventReconciliation(runtimeEventContext);
}

let roomSnapshotRefreshScheduled = false;

function handleRoomRuntimeEvent(event: RoomRuntimeEvent): void {
  if (event.event === "room_fallbacks_changed") {
    roomFallbacks.invalidate(event.room_id);
    return;
  }
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
  if (roomSnapshotRefreshScheduled) return;
  roomSnapshotRefreshScheduled = true;
  requestAnimationFrame(() => {
    roomSnapshotRefreshScheduled = false;
    void refreshSnapshot(tabState.activeId ?? undefined).catch((error) =>
      writeSystem("error", `room inventory refresh failed: ${String(error)}`),
    );
  });
}

/** Show the active room's own draft in the composer. Same-room renders leave
    the text, selection and focus untouched. */
function bindComposerToActiveRoom(): void {
  const next = roomDrafts.bind(activeRoomId, roomMessage.value, new Set(roomSnapshots.map((room) => room.room_id)));
  if (next !== null) {
    roomMessage.value = next;
  }
}

function syncRoomInventory(incoming: RoomSnapshot[]): void {
  roomSnapshots = incoming;
  for (const room of incoming) {
    if (room.recovery_error == null) continue;
    roomFeedEvents.delete(room.room_id);
    roomCardIndexes.delete(room.room_id);
    roomFeedCursors.delete(room.room_id);
    queuedRoomFeedEvents.delete(room.room_id);
    initializedRoomFeeds.delete(room.room_id);
    roomFeedReloadRequired.delete(room.room_id);
  }
  const liveIds = new Set(incoming.map((room) => room.room_id));
  roomDrafts.retire(liveIds);
  roomFallbacks.retire(liveIds);
  for (const roomId of Array.from(roomFeedEvents.keys())) {
    if (!liveIds.has(roomId)) {
      roomFeedEvents.delete(roomId);
    roomCardIndexes.delete(roomId);
      roomFeedCursors.delete(roomId);
      queuedRoomFeedEvents.delete(roomId);
      initializedRoomFeeds.delete(roomId);
      loadingRoomFeeds.delete(roomId);
      roomFeedReloadRequired.delete(roomId);
    }
  }
  const selection = roomSelectionAfterSync(workContext, activeRoomId, liveIds, incoming[0]?.room_id ?? null);
  if (selection.contextLost) {
    workContext = selection.context;
    persistWorkContext();
    writeSystem("warn", "The room this workspace was in is gone — back to the lobby.");
  }
  activeRoomId = selection.active;
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

function roomRecoveryMessage(room: RoomSnapshot | null | undefined): string | null {
  if (room?.recovery_error == null) return null;
  return `Room log recovery failed: ${room.recovery_error || "the stored feed is unavailable"}. Posting, sending and briefing are disabled. Member terminals remain available.`;
}

function roomWriteRecoveryMessage(room: RoomSnapshot | null | undefined): string | null {
  return roomRecoveryMessage(room) ?? roomWriterRecoveryMessage(room);
}

function roomFeedUnavailable(roomId: string): boolean {
  return roomSnapshots.some((room) => room.room_id === roomId && room.recovery_error != null);
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
    roomCardIndexes.delete(roomId);
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
  if (event.room_id !== activeRoomId || roomFeedUnavailable(event.room_id)) {
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
  roomCardIndexes.get(event.room_id)?.accept(event);
  roomFeedEvents.set(event.room_id, result.events);
  roomFeedCursors.set(event.room_id, result.cursor);
  if (activeRoomId === event.room_id) {
    renderRoomFeed(result.events);
  }
  return true;
}

async function initializeRoomFeed(roomId: string, force = false): Promise<void> {
  if (roomFeedUnavailable(roomId)) return;
  if ((!force && initializedRoomFeeds.has(roomId)) || loadingRoomFeeds.has(roomId)) {
    return;
  }
  loadingRoomFeeds.add(roomId);
  try {
    let cursor: RoomFeedCursor | null = null;
    const events: RoomFeedEvent[] = [];
    const cards = new RoomCardIndex();
    for (;;) {
      if (roomFeedUnavailable(roomId)) return;
      const page: RoomFeedPage = await command<RoomFeedPage>("read_room_feed", {
        request: {
          room_id: roomId,
          cursor,
        } satisfies ReadRoomFeedRequest,
      });
      if (roomFeedUnavailable(roomId)) return;
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
          setRoomStatus(`Feed gap: ${range} unavailable.`, "warn");
        }
      }
      cards.snapshot(page.cards ?? [], page.cards_cursor ?? null);
      events.push(...page.events);
      cursor = page.cursor;
      if (!page.has_more) {
        break;
      }
    }
    if (activeRoomId !== roomId) {
      return;
    }
    roomCardIndexes.set(roomId, cards);
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
    if (roomFeedUnavailable(roomId)) return;
    if (activeRoomId === roomId) {
      setRoomStatus(`Room feed unavailable: ${String(error)}`, "error");
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
  syncContextChipCurrent();
}

/** The top bar highlights what is on screen: the active tab in the sessions
    view, the Rooms chip on the rooms overview, the room-name chip on the
    standing room's panel. Presentation only — tab selection (aria-selected,
    keyboard order) is untouched; the CSS mutes the selected tab's active look
    outside the sessions view. */
function syncContextChipCurrent(): void {
  const inRooms = shellView === "rooms";
  const roomsChipCurrent = inRooms && roomsViewMode === "overview";
  const nameChipCurrent =
    inRooms
    && roomsViewMode === "panel"
    && workContext.kind === "room"
    && activeRoomId === workContext.roomId;
  setChipCurrent(contextRoomsChip, roomsChipCurrent);
  setChipCurrent(contextNameChip, nameChipCurrent);
}

function setChipCurrent(chip: HTMLButtonElement, current: boolean): void {
  if (current) {
    chip.setAttribute("aria-current", "true");
  } else {
    chip.removeAttribute("aria-current");
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

/** Enter the lobby or a room: the top bar swaps to that context's sessions and
    the remembered tab (or the first) becomes active. Entering never launches
    agents (PLAN §5-3). */
function enterWorkContext(context: WorkContext): void {
  if (perfEnabled) {
    perfProbes.switchStarted();
  }
  const changed = contextKey(context) !== contextKey(workContext);
  if (changed && tabState.activeId) {
    lastActiveByContext.set(contextKey(workContext), tabState.activeId);
  }
  workContext = context;
  persistWorkContext();
  if (context.kind === "room") {
    activeRoomId = context.roomId;
    bindComposerToActiveRoom();
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
  renderRoomsOverview();
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
  const load = document.createElement("button");
  load.type = "button";
  load.className = "rooms-overview-card rooms-overview-new";
  load.dataset.roomLoad = "";
  load.textContent = "Load room…";
  load.title = "Load a saved .prim1room file as a new room; nothing starts";
  load.addEventListener("click", () => void loadRoomFromFile());
  fragment.appendChild(load);
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
  const recovery = roomWriteRecoveryMessage(room);
  if (recovery) {
    const warning = document.createElement("span");
    warning.className = "room-feed-error";
    warning.textContent = room?.recovery_error != null
      ? "Room log unavailable · terminals available"
      : "Room writes held · earlier feed and terminals available";
    warning.title = recovery;
    card.append(warning);
  }
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
  chip.dataset.sessionReadiness = sessionId;
  const session = snapshotById.get(sessionId);
  const readiness = sessionReadiness(session, wasRunningAtShutdown(session));
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
  bindComposerToActiveRoom();
  const selected = activeRoom();
  roomFallbacks.bind(selected, [...snapshotById.values()]);
  const creating = !roomCreateForm.hidden;
  // While the create form is open the panel is about the room being born —
  // never the standing room. Its title must not lie, and rename/delete
  // (which target the standing room) have no business being reachable.
  roomPanelTitle.textContent = creating
    ? "Creating new room"
    : selected
      ? selected.label
      : "Rooms";
  newRoomButton.hidden = creating;
  renameRoomButton.hidden = creating;
  saveRoomButton.hidden = creating;
  deleteRoomButton.hidden = creating;
  roomResumeToggle.closest("label")!.hidden = creating || selected === null;
  roomResumeToggle.checked = selected?.resume_on_restart ?? false;
  roomEmpty.hidden = selected !== null || creating;
  roomContent.hidden = selected === null || creating;
  const roomIndex = selected
    ? roomSnapshots.findIndex((room) => room.room_id === selected.room_id)
    : -1;
  renameRoomButton.disabled = selected === null;
  saveRoomButton.disabled = selected === null;
  deleteRoomButton.disabled = selected === null;
  moveRoomLeftButton.disabled = roomIndex <= 0;
  moveRoomRightButton.disabled = roomIndex < 0 || roomIndex >= roomSnapshots.length - 1;
  renderRoomComposerAvailability();
  if (!selected) {
    roomMembers.replaceChildren();
    roomGuestList.replaceChildren();
    replaceRoomFeedChildren();
    roomResumeNote.hidden = true;
    return;
  }
  renderRoomResumeNote(selected);
  if (roomBriefSettings.open && roomBriefRequestedFor !== selected.room_id) {
    void loadRoomBriefSettings(selected.room_id);
  }
  renderRoomMembers(selected);
  renderRoomGuests(selected);
  renderRoomRecipientOptions(selected);
  renderRoomFeed(roomFeedEvents.get(selected.room_id) ?? []);
}

function renderRoomComposerAvailability(): void {
  const selected = activeRoom();
  const replayNotice = roomHistoryReplayMessage(selected);
  roomHistoryReplay.hidden = replayNotice === null;
  roomHistoryReplay.textContent = replayNotice ?? "";
  const recovery = roomWriteRecoveryMessage(selected);
  const unavailable = selected === null || recovery !== null;
  const busy = selected !== null && pendingRoomSubmissions.has(selected.room_id);
  roomMessage.disabled = unavailable;
  roomRecipient.disabled = unavailable;
  roomPostButton.disabled = unavailable || busy;
  roomSendButton.disabled = unavailable || busy || (selected?.member_ids.length ?? 0) < 1;
  roomPostButton.title = recovery ?? ROOM_POST_TITLE;
  roomSendButton.title = recovery ?? ((selected?.member_ids.length ?? 0) < 1
    ? "Add a member before sending"
    : "Store the message and type it into each member's terminal (queued while a member is stopped, starting or on a prompt)");
  if (recovery) {
    setRoomStatus(recovery, "error");
  } else if (roomStatus.dataset.recoveryRoomId !== undefined) {
    setRoomStatus("", "info");
    delete roomStatus.dataset.recoveryRoomId;
    delete roomStatus.dataset.level;
  }
}

/** A room whose agents were running when PRIM-1 closed but which did not
    resume them (unmarked, or the operator stopped the resume) says so, with
    one Resume for all of them — nothing pauses silently. */
function renderRoomResumeNote(room: RoomSnapshot): void {
  const asleep = room.member_ids.filter((sessionId) =>
    wasRunningAtShutdown(snapshotById.get(sessionId)),
  );
  roomResumeNote.hidden = asleep.length === 0;
  if (asleep.length === 0) {
    return;
  }
  roomResumeNoteText.textContent = `Was running when PRIM-1 closed: ${asleep
    .map((sessionId) => paneLabel(sessionId))
    .join(", ")} — resume?`;
  roomResumeNowButton.onclick = () => {
    roomResumeNote.hidden = true;
    resumeInBackground(asleep);
  };
}

/** The members side panel: one card per member with its live state, its
    run's CPU and memory from the telemetry sample, and its actions. */
function renderRoomMembers(room: RoomSnapshot): void {
  roomMembers.replaceChildren();
  roomMembersCount.textContent = String(room.member_ids.length);
  for (const sessionId of room.member_ids) {
    const session = snapshotById.get(sessionId);
    const readiness = sessionReadiness(session, wasRunningAtShutdown(session));
    const card = document.createElement("div");
    card.className = "room-member-card";
    card.dataset.sessionReadiness = sessionId;
    card.title = readiness.reason;

    const who = document.createElement("div");
    who.className = "room-member-who";
    const tag = document.createElement("span");
    tag.className = "room-member-tag";
    tag.textContent = session ? driverTag(session.driver) : "--";
    const name = document.createElement("span");
    name.className = "room-member-name";
    name.textContent = paneLabel(sessionId);
    name.title = readiness.reason;
    name.dataset.readinessLabel = "";
    const id = document.createElement("span");
    id.className = "room-member-id";
    id.textContent = shortSessionId(sessionId);
    who.append(tag, name, id);

    const state = document.createElement("div");
    state.className = "room-member-state";
    const dot = document.createElement("span");
    dot.className = "readiness-dot room-member-dot";
    dot.dataset.state = readiness.kind;
    const stateText = document.createElement("span");
    stateText.dataset.readinessText = "";
    stateText.textContent = readiness.reason;
    state.append(dot, stateText);

    const usage = document.createElement("p");
    usage.className = "room-member-usage";
    usage.dataset.memberUsage = sessionId;

    const actions = document.createElement("div");
    actions.className = "room-member-actions";
    const open = document.createElement("button");
    open.type = "button";
    open.className = "ghost";
    open.textContent = "Open";
    open.title = `Open ${paneLabel(sessionId)}'s terminal`;
    open.addEventListener("click", () => {
      setShellView("sessions");
      setActiveSession(sessionId, true);
    });
    actions.append(open);
    if (session && !session.running) actions.append(roomMemberWakeButton(sessionId));
    const brief = document.createElement("button");
    brief.type = "button";
    brief.className = "ghost room-member-brief";
    brief.textContent = "Brief";
    const recovery = roomWriteRecoveryMessage(room);
    brief.disabled = recovery !== null;
    brief.title = recovery ?? `Store a new room brief for ${paneLabel(sessionId)} and type it into its terminal`;
    brief.addEventListener("click", () => void briefRoomMemberNow(room.room_id, sessionId));
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "room-member-remove";
    remove.dataset.action = "stop";
    remove.textContent = "×";
    remove.title = `Remove ${paneLabel(sessionId)} from ${room.label}`;
    remove.setAttribute("aria-label", remove.title);
    remove.addEventListener("click", () => void removeRoomMember(room.room_id, sessionId));
    actions.append(brief, remove);

    card.append(who, state, usage, actions);
    roomMembers.append(card);
  }
  updateRoomMemberUsage();

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
  roomAddMemberButton.disabled = eligible.length === 0 || roomWriteRecoveryMessage(room) !== null;
}

function roomMemberWakeButton(sessionId: string): HTMLButtonElement {
  const wake = document.createElement("button");
  wake.type = "button";
  wake.className = "room-member-wake";
  wake.textContent = "Wake";
  wake.title = roomMemberWakeTitle(sessionId);
  wake.addEventListener("click", () => void launchSession(sessionId));
  return wake;
}

function roomMemberWakeTitle(sessionId: string): string {
  const session = snapshotById.get(sessionId);
  return session && isSleeping(session)
    ? `Wake ${paneLabel(sessionId)} in its existing conversation`
    : `Start ${paneLabel(sessionId)} (asleep)`;
}

/** Invited outside agents: never members, so no terminal, wake or Send.
    Revoking one ends its token; its messages stay in the feed under its name. */
function renderRoomGuests(room: RoomSnapshot): void {
  const guests = room.guests ?? [];
  roomGuestsCount.textContent = String(guests.length);
  const now = new Date();
  roomGuestList.replaceChildren(...guests.map((guest) => roomGuestCard(room, guest, now)));
  roomInviteGuestButton.disabled = roomWriteRecoveryMessage(room) !== null;
}

function roomGuestCard(room: RoomSnapshot, guest: RoomGuest, now: Date): HTMLElement {
  const card = document.createElement("div");
  card.className = "room-member-card room-guest-card";
  const who = document.createElement("div");
  who.className = "room-member-who";
  const name = document.createElement("span");
  name.className = "room-member-name";
  name.textContent = guest.label;
  who.append(name, guestBadge());
  const state = document.createElement("div");
  state.className = "room-member-state";
  const seen = guest.last_seen ? readableTime(guest.last_seen, now) : "";
  state.textContent = seen ? `last seen ${seen}` : "not connected yet";
  const actions = document.createElement("div");
  actions.className = "room-member-actions";
  const revoke = document.createElement("button");
  revoke.type = "button";
  revoke.className = "room-member-remove";
  revoke.dataset.action = "stop";
  revoke.textContent = "×";
  revoke.title = `Revoke ${guest.label}'s invitation to ${room.label}`;
  revoke.setAttribute("aria-label", revoke.title);
  revoke.addEventListener("click", () => void revokeRoomGuest(guest));
  actions.append(revoke);
  card.append(who, state, actions);
  return card;
}

/** The same cue on a guest's feed messages and on its card: never a member. */
function guestBadge(): HTMLElement {
  const badge = document.createElement("span");
  badge.className = "room-readable-system room-readable-guest";
  badge.textContent = "Guest";
  badge.title = "An outside agent invited into this room. Members are told its messages are outside input.";
  return badge;
}

async function revokeRoomGuest(guest: RoomGuest): Promise<void> {
  if (!(await confirmAction(`Revoke ${guest.label}? Its token stops working at once; its messages stay in the feed.`, "Revoke"))) {
    return;
  }
  try {
    await command<void>("revoke_room_guest", {
      request: { guest_id: guest.guest_id } satisfies RevokeRoomGuestRequest,
    });
    setRoomStatus(`${guest.label} can no longer read or post to this room.`, "info");
  } catch (error) {
    setRoomStatus(`Revoke failed: ${errorText(error)}`, "error");
  }
}

/** Each opening of the invite dialog is one generation; a late result or
    error from an earlier opening never reaches the current dialog. */
let guestInviteGeneration = 0;

function openGuestInvite(): void {
  const room = activeRoom();
  if (!room) return;
  guestInviteGeneration += 1;
  guestInviteRoomId = room.room_id;
  // A request from an earlier opening may still be pending; it no longer
  // owns this button.
  const submit = guestInviteForm.querySelector<HTMLButtonElement>("button[type=submit]");
  if (submit) submit.disabled = false;
  guestInviteForm.hidden = false;
  guestInviteResult.hidden = true;
  guestInviteError.hidden = true;
  guestInviteLabel.value = "";
  guestInviteDialog.showModal();
  focusWithinActiveWindow(guestInviteLabel);
}

/** The token lives only in the open dialog: every way of closing it forgets it. */
function forgetGuestInvite(): void {
  guestInviteGeneration += 1;
  guestInviteRoomId = null;
  guestInviteBlock.value = "";
  guestInviteConfig.value = "";
  guestInviteCli.value = "";
  guestInviteCopied.textContent = "";
}

async function createGuestInvitation(): Promise<void> {
  const roomId = guestInviteRoomId;
  const generation = guestInviteGeneration;
  const submit = guestInviteForm.querySelector<HTMLButtonElement>("button[type=submit]");
  if (!roomId || submit?.disabled) return;
  const label = guestInviteLabel.value.trim();
  guestInviteError.hidden = true;
  if (submit) submit.disabled = true;
  try {
    const invitation = await command<RoomGuestInvitationView>("invite_room_guest", {
      request: { room_id: roomId, label: label || null } satisfies InviteRoomGuestRequest,
    });
    if (generation !== guestInviteGeneration) {
      // The dialog closed meanwhile: its token is gone; the guest stays listed.
      setRoomStatus(`Invited ${invitation.guest.label}, but the dialog was closed before its token was shown. Revoke it and invite again to get a token.`, "warn");
      return;
    }
    setRoomStatus(`Invited ${invitation.guest.label} as an outside agent.`, "info");
    guestInviteBlock.value = invitation.agent_invitation;
    guestInviteConfig.value = invitation.mcp_config_json;
    guestInviteCli.value = invitation.cli_example;
    guestInviteForm.hidden = true;
    guestInviteResult.hidden = false;
    focusWithinActiveWindow(guestInviteBlock);
    guestInviteBlock.select();
  } catch (error) {
    if (generation !== guestInviteGeneration) return;
    guestInviteError.textContent = errorText(error);
    guestInviteError.hidden = false;
  } finally {
    if (submit && generation === guestInviteGeneration) submit.disabled = false;
  }
}

function copyGuestInvite(text: string, what: string): void {
  void navigator.clipboard.writeText(text).then(
    // The webview clipboard cannot opt out of Windows clipboard history.
    () => { guestInviteCopied.textContent = keyPlatform === "macos"
      ? `${what} copied; the token stays on the clipboard until something else replaces it`
      : `${what} copied; the token stays in Windows clipboard history (Win+V) until you clear it there`; },
    (error: unknown) => { guestInviteCopied.textContent = `Copy failed (${errorText(error)}); select the text and copy it`; },
  );
}

/** Partial snapshot updates keep Wake in step with the member's run without
    rebuilding the room (which would cost unsent drafts): asleep members get
    it right after Open, a launched member loses it. */
function syncRoomMemberWake(card: HTMLElement, snapshot: SessionSnapshot): void {
  const actions = card.querySelector<HTMLElement>(".room-member-actions");
  if (!actions) return;
  const wake = actions.querySelector<HTMLButtonElement>(".room-member-wake");
  if (snapshot.running && wake) {
    wake.remove();
  } else if (wake) {
    wake.title = roomMemberWakeTitle(snapshot.session_id);
  } else if (!snapshot.running && !wake) {
    const open = actions.firstElementChild;
    const button = roomMemberWakeButton(snapshot.session_id);
    if (open) open.after(button);
    else actions.prepend(button);
  }
}

function liveRunIds(): Map<string, string> {
  const runs = new Map<string, string>();
  for (const session of snapshotById.values()) {
    if (session.running && session.run_id) runs.set(session.session_id, session.run_id);
  }
  return runs;
}

/** Fills each member card's usage line. Numbers appear only for the member's
    current run, from a fresh sample bound to that run; otherwise the line says
    why there are none instead of showing an earlier run's figures. */
function updateRoomMemberUsage(): void {
  const usage = new Map((telemetryLast?.sessions ?? []).map((run) => [run.session_id, run]));
  const fresh = telemetryLast !== null
    && Date.now() - Date.parse(telemetryLast.sampled_at) <= 3 * TELEMETRY_POLL_MS;
  for (const line of roomMembers.querySelectorAll<HTMLElement>("[data-member-usage]")) {
    const sessionId = line.dataset.memberUsage ?? "";
    const session = snapshotById.get(sessionId);
    const run = usage.get(sessionId);
    let text = "";
    if (session?.running && session.run_id) {
      if (telemetryError !== null) {
        text = "usage unavailable";
      } else if (fresh && run && telemetrySampleRuns.get(sessionId) === session.run_id) {
        text = `CPU ${formatPercent(run.cpu_percent)} · ${formatBytes(run.private_bytes)}`;
      } else {
        text = "usage pending";
      }
    }
    if (line.textContent !== text) line.textContent = text;
  }
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

function replaceRoomFeedChildren(...nodes: (Node | string)[]): void {
  for (const node of Array.from(roomFeed.children)) disposeTypedCard(node);
  roomFeed.replaceChildren(...nodes);
}

function renderRoomFeed(events: RoomFeedEvent[]): void {
  roomFeed.dataset.view = roomFeedView;
  const recovery = roomRecoveryMessage(activeRoom());
  if (recovery) {
    const warning = document.createElement("p");
    warning.className = "room-feed-error";
    warning.textContent = recovery;
    replaceRoomFeedChildren(warning);
    return;
  }
  if (roomFeedView === "readable") {
    renderReadableRoomFeed(events);
    return;
  }
  readableReconcilePending = false;
  readableHeldSelectionKeys = null;
  replaceRoomFeedChildren();
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
  const retryable = retryableRoomDeliveryEvents(activeRoomId ?? "", events);
  roomObligations = roomOpenObligations(events, paneLabel, invitedGuestIds());
  for (const event of events) {
    roomFeed.append(roomFeedEntry(event, retryable.has(event)));
  }
  roomFeed.scrollTop = roomFeed.scrollHeight;
}

/** v3 W3 (b): who still owes each ACK-tracked message in the Raw view. */
let roomObligations = new Map<string, { key: string; label: string }[]>();

/** v3 W3 (b) and D6: the operator records a disposition for one recipient,
    or withdraws the message for everyone; both are the operator's, never the
    recipient's own ACK. Collapsed by default. */
function roomDispositionControl(messageId: string, open: { key: string; label: string }[]): HTMLElement {
  const box = document.createElement("details");
  box.className = "room-feed-actions";
  const summary = document.createElement("summary");
  summary.textContent = "Dispose or withdraw";
  const dispose = document.createElement("form");
  const recipient = document.createElement("select");
  recipient.setAttribute("aria-label", "Recipient");
  for (const owed of open) {
    const option = document.createElement("option");
    option.value = owed.key;
    option.textContent = owed.label;
    recipient.append(option);
  }
  const kind = document.createElement("select");
  kind.setAttribute("aria-label", "Disposition");
  for (const [value, text] of [["next", "ACK: next action"], ["decline", "Decline: reason"]] as const) {
    const option = document.createElement("option");
    option.value = value;
    option.textContent = text;
    kind.append(option);
  }
  const note = document.createElement("input");
  note.type = "text";
  note.maxLength = 2048;
  note.required = true;
  note.placeholder = "next action, or why not";
  note.setAttribute("aria-label", "Note");
  const record = document.createElement("button");
  record.type = "submit";
  record.dataset.trialMutation = "";
  record.textContent = "Record for them";
  dispose.append(recipient, kind, note, record);
  dispose.addEventListener("submit", (event) => {
    event.preventDefault();
    const text = note.value.trim();
    if (!text) return;
    void roomOperatorAction(`Disposition for ${recipient.selectedOptions[0]?.textContent ?? "the recipient"}`,
      (roomId) => command("dispose_room_message", { request: { room_id: roomId, message_id: messageId, recipient: recipient.value,
        ...(kind.value === "next" ? { next_action: text } : { decline: text }) } }));
  });
  const withdraw = document.createElement("form");
  const reason = document.createElement("input");
  reason.type = "text";
  reason.maxLength = 2048;
  reason.required = true;
  reason.placeholder = "why it is withdrawn";
  reason.setAttribute("aria-label", "Reason");
  const cancel = document.createElement("button");
  cancel.type = "submit";
  cancel.dataset.trialMutation = "";
  cancel.textContent = "Withdraw for everyone";
  // Before the operator confirms: who already has it (Claude PRIM-1 after 25e7ecf).
  const reached = roomMessageReached(roomFeedEvents.get(activeRoomId ?? "") ?? [], messageId).map(paneLabel);
  const already = document.createElement("p");
  already.className = "room-feed-note";
  already.textContent = roomWithdrawPreview(reached);
  withdraw.append(reason, cancel, already);
  withdraw.addEventListener("submit", (event) => {
    event.preventDefault();
    const text = reason.value.trim();
    if (!text) return;
    void roomOperatorAction(`Withdrawal of message ${messageId.slice(0, 8)}`,
      (roomId) => command("cancel_room_message", { request: { room_id: roomId, message_id: messageId, reason: text } }));
  });
  box.append(summary, dispose, withdraw);
  return box;
}

/** Runs one operator room action, says how it went, and reloads the feed. */
async function roomOperatorAction(what: string, action: (roomId: string) => Promise<unknown>): Promise<void> {
  const room = activeRoom();
  if (!room) return;
  try {
    await action(room.room_id);
    setRoomStatus(`${what}: stored.`, "info");
  } catch (error) {
    setRoomStatus(`${what}: not stored: ${roomRequestFailure(error).message}`, "error");
  } finally {
    if (activeRoomId === room.room_id && !roomFeedUnavailable(room.room_id)) {
      await initializeRoomFeed(room.room_id, true);
    }
  }
}

/** The active room's invited guests, or null while no room is shown. */
function invitedGuestIds(): ReadonlySet<string> | null {
  const room = activeRoom();
  return room ? new Set((room.guests ?? []).map((guest) => guest.guest_id)) : null;
}

function roomFeedEntry(event: RoomFeedEvent, canRetry: boolean): HTMLElement {
  const entry = document.createElement("article");
  entry.className = `room-feed-entry room-feed-${event.item.kind}`;
  entry.dataset.sequence = String(event.cursor.sequence);
  const meta = document.createElement("div");
  meta.className = "room-feed-meta";
  const sequence = `#${event.cursor.sequence}`;
  if (event.item.kind === "message") {
    const sender = roomNoticeSenderLabel(event.item) ?? roomRecoverySenderLabel(event.item) ?? roomFallbackSenderLabel(event.item) ?? (
      event.item.sender.kind === "operator"
        ? "Operator"
        : event.item.sender.kind === "guest"
          ? `${event.item.sender.label} · guest`
          : event.item.sender.kind === "local_process"
            ? `${event.item.sender.label} · operator entry`
            : event.item.sender.kind === "prim1"
              ? "PRIM-1"
              : paneLabel(event.item.sender.session_id));
    const recipients = roomMessageRecipientsLabel(event.item, paneLabel, invitedGuestIds());
    meta.textContent = `${sequence} ${sender} · Stored · ${recipients} · ${event.item.message_id.slice(0, 8)}`;
    const body = document.createElement("pre");
    body.textContent = event.item.content;
    entry.append(meta, body);
    const open = roomObligations.get(event.item.message_id);
    if (open?.length) entry.append(roomDispositionControl(event.item.message_id, open));
  } else if (event.item.kind === "card") {
    meta.textContent = `${sequence} ${paneLabel(event.item.author)} · ${event.item.card.kind} · ${event.item.card_id}`;
    const body = document.createElement("pre");
    body.textContent = JSON.stringify(event.item.card, null, 2);
    entry.append(meta, body);
  } else if (event.item.kind === "membership") {
    meta.textContent = `${sequence} ${paneLabel(event.item.session_id)} ${event.item.action} · revision ${event.item.membership_revision}`;
    entry.append(meta);
  } else if (event.item.kind === "waiting") {
    // v3 W3: a deferred wake is visible at once, with its reason code.
    meta.textContent = `${sequence} ${paneLabel(event.item.recipient_id)} · Waiting (${event.item.reason_code}) | message ${event.item.message_id.slice(0, 8)}`;
    const reason = document.createElement("p");
    reason.className = "room-feed-waiting";
    reason.textContent = event.item.reason;
    entry.append(meta, reason);
  } else if (event.item.kind === "submit_attempt") {
    // v3 W3b: the recipe of one pointer attempt, before any byte was typed.
    meta.textContent = `${sequence} ${paneLabel(event.item.recipient_id)} · Attempt ${event.item.attempt.sequence} recorded (${event.item.driver}) | message ${event.item.message_id.slice(0, 8)}`;
    entry.append(meta);
  } else if (event.item.kind === "input_observed") {
    // v3 W3b: submitted, as the member's own record shows; not an ACK.
    meta.textContent = `${sequence} ${paneLabel(event.item.recipient_id)} · Input observed (attempt ${event.item.attempt.sequence}) | message ${event.item.message_id.slice(0, 8)}`;
    meta.title = `${event.item.source} ${event.item.locator ?? ""}`.trim();
    entry.append(meta);
  } else if (event.item.kind === "cancel") {
    // v3 W3 (D6): who withdrew the message and why; neutral, it is no failure.
    const actor = event.item.actor;
    const by = actor.kind === "operator" ? "Operator" : actor.kind === "local_process" ? actor.label
      : actor.kind === "prim1" ? "PRIM-1" : paneLabel(actor.session_id);
    meta.textContent = `${sequence} ${by} · Withdrawn | message ${event.item.message_id.slice(0, 8)}`;
    const reason = document.createElement("p");
    reason.className = "room-feed-note";
    reason.textContent = event.item.reason;
    const outcome = document.createElement("p");
    outcome.className = "room-feed-note";
    outcome.textContent = roomWithdrawalText(event.item, paneLabel,
      roomWithdrawalNotice(roomFeedEvents.get(activeRoomId ?? "") ?? [], event.item.request_id));
    entry.append(meta, reason, ...(outcome.textContent ? [outcome] : []));
  } else if (event.item.kind === "guest_ack") {
    // v3 W5: a guest's own disposition, or one made for it, never confused.
    const item = event.item;
    const guest = roomGuestLabel(item.guest_id, item.guest_label, invitedGuestIds());
    const by = item.actor.kind === "guest" ? guest
      : item.actor.kind === "operator" ? `Operator for ${guest}`
      : `${item.actor.label} for ${guest}`;
    meta.textContent = `${sequence} ${by} · ${item.disposition === "acked" ? "ACK" : "Declined"} | message ${item.message_id.slice(0, 8)}`;
    const note = document.createElement("p");
    note.className = "room-feed-ack";
    note.textContent = item.note;
    entry.append(meta, note);
  } else if (event.item.kind === "ack") {
    // v3 W3: an agent's own ACK, or a disposition for it by the operator or a
    // local process acting with the operator's authority, never confused.
    const actor = event.item.actor;
    const by = actor.kind === "operator" ? `Operator for ${paneLabel(event.item.recipient_id)}`
      : actor.kind === "local_process" ? `${actor.label} for ${paneLabel(event.item.recipient_id)}`
      : actor.kind === "prim1" ? `PRIM-1 for ${paneLabel(event.item.recipient_id)}`
      : paneLabel(event.item.recipient_id);
    const what = event.item.disposition === "acked" ? "ACK" : "Declined";
    const outcome = event.item.outcome_ref ? ` · ${event.item.outcome_ref}` : "";
    meta.textContent = `${sequence} ${by} · ${what}${outcome} | message ${event.item.message_id.slice(0, 8)}`;
    const note = document.createElement("p");
    note.className = "room-feed-ack";
    note.textContent = event.item.note;
    entry.append(meta, note);
  } else {
    const partial = event.item.bytes_written > 0 ? ` · ${event.item.bytes_written} bytes` : "";
    meta.textContent = `${sequence} ${paneLabel(event.item.recipient_id)} · ${roomDeliveryReceiptLabel(event.item)}${partial} | message ${event.item.message_id.slice(0, 8)}`;
    entry.append(meta);
    if (event.item.status === "pending" || event.item.status === "stored") {
      meta.title = roomWakeUnavailable(snapshotById.get(event.item.recipient_id)?.driver)
        ?? "Wake outcome unknown. Prompt this member in its terminal to run room_read; this attempt cannot be retried safely.";
    }
    if (event.item.error) {
      const error = document.createElement("p");
      // v3 W3: an already-read pointer is a neutral note, not a failure.
      error.className = deliveryNotNeeded(event.item) ? "room-feed-note" : "room-feed-error";
      error.textContent = event.item.error;
      entry.append(error);
    }
    const retry = roomRetryControl(event, canRetry);
    if (retry) {
      entry.append(retry);
    }
  }
  return entry;
}

type RoomRetryState =
  | { kind: "unavailable"; reason: string }
  | { kind: "retry"; pending: boolean; disabled: boolean };

/** What Retry wake offers for a receipt: nothing, why it can no longer be
    retried, or the control and whether it is usable right now. */
function roomRetryState(event: RoomFeedEvent, canRetry: boolean): RoomRetryState | null {
  if (!canRetry || event.item.kind !== "delivery") return null;
  const unavailable = roomWriteRecoveryMessage(activeRoom())
    ?? roomDeliveryRetryUnavailable(event, activeRoom()?.member_ids ?? [], snapshotById);
  if (unavailable) return { kind: "unavailable", reason: unavailable };
  const key = roomDeliveryRetryKey(event.room_id, event.item.message_id, event.item.recipient_id);
  const pending = pendingRoomWakeRetries.has(key);
  return { kind: "retry", pending, disabled: pending || pendingRoomWakeRetries.size >= MAX_PENDING_ROOM_WAKE_RETRIES };
}

/** Retry wake for an eligible receipt, or why it can no longer be retried.
    Raw puts it on the receipt; Readable on the message's warning line. */
function roomRetryControl(event: RoomFeedEvent, canRetry: boolean): HTMLElement | null {
  const state = roomRetryState(event, canRetry);
  return state ? roomRetryElement(event, state) : null;
}

/** `receiptAtClick` lets a kept Readable control retry the receipt its line
    holds now: a feed reload brings new event objects for the same receipts. */
function roomRetryElement(
  event: RoomFeedEvent,
  state: RoomRetryState,
  receiptAtClick: () => RoomFeedEvent = () => event,
): HTMLElement | null {
  if (event.item.kind !== "delivery") return null;
  if (state.kind === "unavailable") {
    const reason = document.createElement("p");
    reason.className = "room-feed-error";
    reason.textContent = state.reason;
    return reason;
  }
  const retry = document.createElement("button");
  retry.type = "button";
  retry.dataset.trialMutation = "";
  retry.textContent = state.pending ? "Retrying…" : "Retry wake";
  retry.title = `Retry this recipient's wake for stored message ${event.item.message_id}`;
  retry.setAttribute("aria-label", `Retry wake for ${paneLabel(event.item.recipient_id)}, message ${event.item.message_id.slice(0, 8)}`);
  retry.disabled = state.disabled;
  retry.addEventListener("click", () => void retryRoomDelivery(receiptAtClick()));
  return retry;
}

/** What the Readable feed knows about a node it built: the signature it shows
    and, for cards and warning lines, the parts it updates in place. */
type ReadableNode =
  | { kind: "note" | "typed"; signature: string }
  | { kind: "card"; signature: string; meta: HTMLElement; metaSignature: string; problems: HTMLElement; states: HTMLElement }
  | { kind: "line"; signature: string; receipt: RoomFeedEvent; retry: RoomRetryState | null; control: HTMLElement | null;
      details: HTMLDetailsElement; reason: HTMLParagraphElement; unavailable: HTMLParagraphElement };
const readableNodes = new WeakMap<Element, ReadableNode>();

/** Readable (A52, A53): each message is a card in a reading font. Receipts
    fold into their message; a held or failed wake, and any input a wake left
    in a terminal, stays visible on that message, with the same Retry wake
    control Raw offers. Unchanged cards keep their nodes, so a reader's text
    selection and focus survive new traffic. */
function renderReadableRoomFeed(events: RoomFeedEvent[]): void {
  const sameRoom = readableFeedRoomId === activeRoomId;
  if (!sameRoom) roomCardPage = 0;
  const index = activeRoomId ? roomCardIndexes.get(activeRoomId) : undefined;
  const board = index?.page(roomCardPage);
  if (board) roomCardPage = board.page;
  const feed = readableRoomFeed(events, readableMemberLabel, (board?.total ?? 0) > 0);
  const retryable = retryableRoomDeliveryEvents(activeRoomId ?? "", events);
  const now = new Date();
  const selection = document.getSelection();
  const readingSelection = selectionTouchesNode(roomFeed, selection);
  const follow = !readingSelection && (readableFeedFollow || readableFeedRoomId !== activeRoomId);
  if (readingSelection) readableFeedFollow = false;
  const anchor = follow ? null : readableFeedAnchor();
  readableFeedRoomId = activeRoomId;
  const wanted: KeyedChild<Element>[] = [];
  if (events.length > 0 && events[0].cursor.sequence > 1) {
    wanted.push(readableNoteChild("window", "Earlier messages are outside the live window.", null, now));
  }
  if (feed.items.length === 0 && !board?.total) {
    wanted.push(readableNoteChild(
      "empty",
      events.length === 0 ? "No messages yet." : "No messages in the live window. Raw lists every stored event.",
      null,
      now,
    ));
  }
  for (const card of board?.cards ?? []) wanted.push(typedCardChild(card));
  if (board && board.pages > 1) wanted.push(typedCardPager(board.page, board.pages, board.total));
  for (const item of feed.items) {
    wanted.push(readableItemChild(item, now));
  }
  const parent = readableChildren(roomFeed);
  // Capture the original engaged cohort once. Browser ranges can include new
  // nodes inserted between their endpoints; those must not grow a traffic backlog.
  const selectionKeys = readableHeldSelectionKeys ?? new Set(parent.children()
    .filter((node) => selectionTouchesNode(node, selection))
    .map((node) => parent.keyOf(node)).filter((key): key is string => key !== undefined));
  const retained = sameRoom && !readableExplicitPageChange
    ? retainEngagedChildren(parent, wanted, (node) => {
        // Warning lines must publish their new partial/unknown/held/failed
        // evidence immediately; their parent message can still keep its body.
        if (readableNodes.get(node)?.kind === "line") return false;
        return node.contains(document.activeElement) ||
          (selectionKeys.has(parent.keyOf(node) ?? "") && selectionTouchesNode(node, selection));
      })
    : { wanted, pending: false };
  readableExplicitPageChange = false;
  readableReconcilePending = retained.pending;
  readableHeldSelectionKeys = retained.pending ? selectionKeys : null;
  reconcileKeyedChildren(parent, retained.wanted);
  const receipts = new Map(events.map((event) => [readableCursorKey(event), event]));
  for (const line of Array.from(roomFeed.querySelectorAll(".room-readable-problem"))) {
    syncReadableRetry(line, retryable, receipts);
  }
  if (follow) {
    readableFeedFollow = true;
    roomFeed.scrollTop = roomFeed.scrollHeight;
  } else if (anchor) {
    restoreReadableFeedAnchor(anchor);
  }
}

function typedCardChild(event: CardEvent): KeyedChild<Element> {
  return { key: cardKey(event), signature: typedCardSignature(event, readableMemberLabel(event.item.author)),
    build: () => {
      const card = buildTypedCard(event, readableMemberLabel, prepareCardReply);
      readableNodes.set(card, { kind: "typed", signature: "" });
      return card;
    },
    update: (node) => updateTypedCard(node, event, readableMemberLabel),
  };
}

function prepareCardReply(event: CardEvent): void {
  const room = activeRoom();
  if (!room || room.room_id !== event.room_id || !room.member_ids.includes(event.item.author)) {
    setRoomStatus("The card's author is no longer in this room. Nothing was sent.", "warn"); return;
  }
  const reply = cardReply(event);
  // Preserve any draft; preparing a reply never submits, records an answer or
  // changes the agent's card state. Normal Post/Send still owns its authority.
  roomMessage.value = roomMessage.value.trim() ? `${roomMessage.value}\n\n${reply.text}` : reply.text;
  roomRecipient.value = reply.target;
  focusWithinActiveWindow(roomMessage);
  roomMessage.setSelectionRange(roomMessage.value.length, roomMessage.value.length);
  setRoomStatus("Reply prepared. Edit it, then choose Post or Send.", "info");
}

function typedCardPager(page: number, pages: number, total: number): KeyedChild<Element> {
  const paint = (nav: Element) => {
    const buttons = nav.querySelectorAll("button");
    buttons[0].disabled = page === 0; buttons[1].disabled = page + 1 === pages;
    const text = nav.querySelector("span")!;
    text.textContent = `Cards ${page + 1} / ${pages} · ${total} topics`;
    return true;
  };
  return { key: "typed-card-pages", signature: `${page}:${pages}:${total}`, build: () => {
    const nav = document.createElement("nav"); nav.className = "room-card-pages"; nav.dataset.key = "typed-card-pages";
    nav.setAttribute("aria-label", "Room cards");
    for (const [delta, label] of [[-1, "Previous"], [1, "Next"]] as const) {
      const button = document.createElement("button"); button.type = "button"; button.textContent = label;
      button.addEventListener("click", () => {
        roomCardPage += delta; readableFeedFollow = false;
        readableExplicitPageChange = true;
        renderRoomFeed(roomFeedEvents.get(activeRoomId ?? "") ?? []);
        roomFeed.querySelector<HTMLElement>(".room-typed-card")?.scrollIntoView({ block: "start" });
      });
      nav.append(button);
    }
    nav.append(document.createElement("span")); paint(nav);
    readableNodes.set(nav, { kind: "note", signature: "" }); return nav;
  }, update: paint };
}

/** The Readable nodes under one parent; anything this view did not build is removed. */
function readableChildren(parent: Element): KeyedParent<Element> {
  return {
    children: () => Array.from(parent.children),
    keyOf: (node) => (readableNodes.has(node) ? node.getAttribute("data-key") ?? undefined : undefined),
    signatureOf: (node) => readableNodes.get(node)?.signature,
    setSignature: (node, signature) => {
      const state = readableNodes.get(node);
      if (state) {
        state.signature = signature;
      }
    },
    insertBefore: (node, before) => {
      parent.insertBefore(node, before);
    },
    remove: (node) => { disposeTypedCard(node); node.remove(); },
  };
}

function readableItemChild(item: ReadableFeedItem, now: Date): KeyedChild<Element> {
  if (item.kind === "note") {
    return readableNoteChild(item.key, item.text, item.timestamp, now);
  }
  if (item.kind === "problem") {
    return readableLineChild(item.key, item.problem, true);
  }
  // The body is not in the signature: a message id never changes its text.
  const metaSignature = readableMetaSignature(item, now);
  return {
    key: item.key,
    signature: JSON.stringify([item.author, metaSignature, item.problems.map((problem) => readableLineSignature(problem, false)),
      item.states]),
    build: () => buildReadableCard(item, metaSignature, now),
    update: (node) => updateReadableCard(node, item, metaSignature, now),
  };
}

function readableNoteChild(key: string, text: string, timestamp: string | null, now: Date): KeyedChild<Element> {
  return {
    key,
    signature: JSON.stringify([text, timestamp ? readableTime(timestamp, now) : ""]),
    build: () => {
      const note = readableNote(text, timestamp, now);
      note.dataset.key = key;
      readableNodes.set(note, { kind: "note", signature: "" });
      return note;
    },
  };
}

/** A warning line changes when its words or its receipt do: a newer receipt
    gets a new line, so Retry wake always acts on the latest one. */
function readableLineSignature(problem: ReadableProblem, orphan: boolean): string {
  return JSON.stringify([readableProblemText(problem, orphan), problem.error, problem.status, problem.receipt.cursor]);
}

function readableLineChild(key: string, problem: ReadableProblem, orphan: boolean): KeyedChild<Element> {
  return {
    key,
    signature: readableLineSignature(problem, orphan),
    build: () => {
      const line = document.createElement("div");
      line.className = "room-readable-problem";
      line.dataset.key = key;
      line.dataset.level = problem.status === "held" && problem.input === "none" ? "warn" : "error";
      const words = document.createElement("span");
      words.textContent = readableProblemText(problem, orphan);
      if (problem.error) {
        words.title = problem.error;
      }
      line.append(words);
      const details = document.createElement("details");
      details.className = "room-readable-details";
      const summary = document.createElement("summary");
      summary.textContent = "Details";
      summary.setAttribute("aria-label", `Wake details for ${problem.recipient}`);
      const reason = document.createElement("p");
      reason.textContent = problem.error ?? "";
      reason.hidden = !problem.error;
      const unavailable = document.createElement("p");
      unavailable.hidden = true;
      details.hidden = !problem.error;
      details.append(summary, reason, unavailable);
      line.append(details);
      readableNodes.set(line, { kind: "line", signature: "", receipt: problem.receipt, retry: null, control: null,
        details, reason, unavailable });
      return line;
    },
  };
}

function readableMetaSignature(item: Extract<ReadableFeedItem, { kind: "message" }>, now: Date): string {
  return JSON.stringify([item.author, item.sender, item.recipients, readableTime(item.timestamp, now)]);
}

function buildReadableMeta(item: Extract<ReadableFeedItem, { kind: "message" }>, now: Date): HTMLElement {
  const meta = document.createElement("div");
  meta.className = "room-readable-meta";
  const sender = document.createElement("span");
  sender.className = "room-readable-sender";
  sender.textContent = item.sender;
  meta.append(sender);
  if (item.author === "prim1") {
    // Only PRIM-1's own posts get this; a member named "PRIM-1" never does.
    const system = document.createElement("span");
    system.className = "room-readable-system";
    system.textContent = "System";
    system.title = "Posted by PRIM-1 itself, not by a member";
    meta.append(system);
  } else if (item.author === "guest") {
    meta.append(guestBadge());
  }
  const recipients = readableRecipients(item.recipients);
  if (recipients) {
    const to = document.createElement("span");
    to.textContent = recipients;
    if (item.recipients.length > 3) {
      to.title = item.recipients.join(", ");
    }
    meta.append(to);
  }
  const time = readableTimeElement(item.timestamp, now);
  if (time) {
    meta.append(time);
  }
  return meta;
}

function buildReadableCard(
  item: Extract<ReadableFeedItem, { kind: "message" }>,
  metaSignature: string,
  now: Date,
): HTMLElement {
  const card = document.createElement("article");
  card.className = "room-readable-card";
  card.dataset.key = item.key;
  card.dataset.author = item.author;
  const meta = buildReadableMeta(item, now);
  const body = document.createElement("div");
  body.className = "room-readable-body";
  card.append(meta, body);
  if (item.preview.collapsed) {
    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.className = "room-readable-more";
    const show = (expanded: boolean) => {
      body.textContent = expanded ? item.body : item.preview.text;
      toggle.textContent = expanded ? "Show less" : "Show all";
      toggle.setAttribute("aria-expanded", String(expanded));
    };
    show(expandedRoomMessages.has(item.key));
    toggle.addEventListener("click", () => {
      const expanded = !expandedRoomMessages.has(item.key);
      setRoomMessageExpanded(item.key, expanded);
      show(expanded);
      if (!expanded) {
        card.scrollIntoView({ block: "nearest" });
      }
      readableFeedFollow = readableFeedAtBottom();
    });
    card.append(toggle);
  } else {
    body.textContent = item.body;
  }
  // v3 W6: where each recipient stands, one line.
  const states = document.createElement("div");
  states.className = "room-readable-states";
  fillReadableStates(states, item);
  const problems = document.createElement("div");
  problems.className = "room-readable-problems";
  card.append(states, problems);
  readableNodes.set(card, { kind: "card", signature: "", meta, metaSignature, problems, states });
  reconcileKeyedChildren(readableChildren(problems), item.problems.map((problem) => readableLineChild(problem.key, problem, false)));
  return card;
}

/** A changed card keeps its body and Show all control; only its meta line
    and its warning lines are redrawn, and only where they changed. */
function updateReadableCard(
  node: Element,
  item: Extract<ReadableFeedItem, { kind: "message" }>,
  metaSignature: string,
  now: Date,
): boolean {
  const state = readableNodes.get(node);
  if (state?.kind !== "card" || node.getAttribute("data-author") !== item.author) return false;
  if (state.metaSignature !== metaSignature) {
    const meta = buildReadableMeta(item, now);
    state.meta.replaceWith(meta);
    state.meta = meta;
    state.metaSignature = metaSignature;
  }
  reconcileKeyedChildren(readableChildren(state.problems), item.problems.map((problem) => readableLineChild(problem.key, problem, false)));
  fillReadableStates(state.states, item);
  return true;
}

function fillReadableStates(container: HTMLElement, item: Extract<ReadableFeedItem, { kind: "message" }>): void {
  container.textContent = readableStatesText(item.states);
  container.title = item.states.filter((state) => state.title).map((state) => `${state.recipient}: ${state.title}`).join("\n");
  container.hidden = item.states.length === 0;
}

function readableCursorKey(event: RoomFeedEvent): string {
  return `${event.cursor.epoch}:${event.cursor.sequence}`;
}

/** Retry wake follows the current state in place, so keyboard focus on it
    survives every update that leaves its warning line unchanged. A kept line
    has the same receipt cursor as before, but a feed reload brings a new event
    object for it: eligibility and the click use the current one. */
function syncReadableRetry(
  line: Element,
  retryable: ReadonlySet<RoomFeedEvent>,
  receipts: ReadonlyMap<string, RoomFeedEvent>,
): void {
  const state = readableNodes.get(line);
  if (state?.kind !== "line") return;
  const lineState = state;
  lineState.receipt = receipts.get(readableCursorKey(lineState.receipt)) ?? lineState.receipt;
  const wanted = roomRetryState(lineState.receipt, retryable.has(lineState.receipt));
  const unavailable = wanted?.kind === "unavailable" ? wanted.reason : "";
  if (lineState.unavailable.textContent !== unavailable) lineState.unavailable.textContent = unavailable;
  lineState.unavailable.hidden = !unavailable || unavailable === lineState.reason.textContent;
  lineState.details.hidden = lineState.reason.hidden && lineState.unavailable.hidden;
  if (lineState.retry?.kind === "retry" && wanted?.kind === "retry" && lineState.control instanceof HTMLButtonElement) {
    const text = wanted.pending ? "Retrying…" : "Retry wake";
    if (lineState.control.textContent !== text) {
      lineState.control.textContent = text;
    }
    lineState.control.disabled = wanted.disabled;
    lineState.retry = wanted;
    return;
  }
  if (JSON.stringify(lineState.retry) === JSON.stringify(wanted)) return;
  const control = wanted?.kind === "retry" ? roomRetryElement(lineState.receipt, wanted, () => lineState.receipt) : null;
  if (lineState.control && control) {
    lineState.control.replaceWith(control);
  } else if (lineState.control) {
    lineState.control.remove();
  } else if (control) {
    line.append(control);
  }
  lineState.control = control;
  lineState.retry = wanted;
}

function readableNote(text: string, timestamp: string | null, now: Date): HTMLElement {
  const note = document.createElement("p");
  note.className = "room-readable-note";
  const words = document.createElement("span");
  words.textContent = text;
  note.append(words);
  const time = timestamp ? readableTimeElement(timestamp, now) : null;
  if (time) {
    note.append(time);
  }
  return note;
}

function readableTimeElement(timestamp: string, now: Date): HTMLElement | null {
  const label = readableTime(timestamp, now);
  if (!label) return null;
  const time = document.createElement("time");
  time.className = "room-readable-time";
  time.dateTime = timestamp;
  time.textContent = label;
  time.title = new Date(timestamp).toLocaleString();
  return time;
}

/** A member's label; one that no longer exists shows its short id, never a full UUID. */
function readableMemberLabel(sessionId: string): string {
  return paneMap.get(sessionId)?.label
    ?? snapshotById.get(sessionId)?.label
    ?? `Member ${shortSessionId(sessionId)}`;
}

function setRoomMessageExpanded(key: string, expanded: boolean): void {
  expandedRoomMessages.delete(key);
  if (!expanded) return;
  expandedRoomMessages.add(key);
  if (expandedRoomMessages.size > MAX_EXPANDED_ROOM_MESSAGES) {
    const oldest = expandedRoomMessages.values().next().value;
    if (oldest !== undefined) {
      expandedRoomMessages.delete(oldest);
    }
  }
}

function readableFeedAtBottom(): boolean {
  return roomFeed.scrollHeight - roomFeed.scrollTop - roomFeed.clientHeight <= READABLE_FOLLOW_SLACK_PX;
}

/** The first line still in view, and how far its top sits from the feed's top edge. */
function readableFeedAnchor(): { key: string; offset: number } | null {
  const top = roomFeed.getBoundingClientRect().top;
  for (const child of Array.from(roomFeed.children)) {
    if (!(child instanceof HTMLElement) || child.dataset.key === undefined) continue;
    const rect = child.getBoundingClientRect();
    if (rect.bottom > top) {
      return { key: child.dataset.key, offset: rect.top - top };
    }
  }
  return null;
}

function restoreReadableFeedAnchor(anchor: { key: string; offset: number }): void {
  for (const child of Array.from(roomFeed.children)) {
    if (child instanceof HTMLElement && child.dataset.key === anchor.key) {
      roomFeed.scrollTop += child.getBoundingClientRect().top - roomFeed.getBoundingClientRect().top - anchor.offset;
      return;
    }
  }
}

/** The members panel is a per-viewer preference: closed, it leaves a slim
    "Members" tab on the edge that reopens it. */
function loadRoomMembersPanelOpen(): boolean {
  try {
    return localStorage.getItem(ROOM_MEMBERS_PANEL_KEY) !== "closed";
  } catch {
    return true; // storage unavailable: the panel stays open
  }
}

function setRoomMembersPanelOpen(open: boolean): void {
  try {
    localStorage.setItem(ROOM_MEMBERS_PANEL_KEY, open ? "open" : "closed");
  } catch {
    /* storage unavailable: the choice just won't persist */
  }
  syncRoomMembersPanel(open);
  focusWithinActiveWindow(open ? roomMembersCloseButton : roomMembersOpenButton);
}

function syncRoomMembersPanel(open = loadRoomMembersPanelOpen()): void {
  roomContent.dataset.members = open ? "open" : "closed";
  roomMembersOpenButton.hidden = open;
}

function applyCommandSessionSnapshot(snapshot: SessionSnapshot): void {
  observeAuthoritativeTerminalGrid(snapshot);
  const reconciliation = reconcileSessionSnapshot(
    snapshot,
    snapshotById,
    runtimeEventContext.runEventGate,
  );
  flushRunEventGateWarnings(runtimeEventContext);
  if (!reconciliation.accepted) {
    return;
  }
  applySessionSnapshotToUi(snapshot, true);
}

function observeAuthoritativeTerminalGrid(snapshot: SessionSnapshot): void {
  const current = snapshotById.get(snapshot.session_id);
  // Content can advance while bootstrap is in flight. The first grid of a
  // run is still authoritative: no resize was admitted while awaiting it.
  if (current?.generation === snapshot.generation && current.run_id === snapshot.run_id) {
    paneMap.get(snapshot.session_id)?.observeRun(snapshot, true);
  }
}

function applySessionSnapshotToUi(snapshot: SessionSnapshot, authoritative = false): void {
  paneMap.get(snapshot.session_id)?.applySnapshot(snapshot, authoritative);
  terminalLayout.request();
  const card = document.querySelector<HTMLElement>(
    '[data-session-card="' + snapshot.session_id + '"]',
  );
  if (card) {
    updateSessionCard(card, snapshot);
  }
  updateSessionTab(snapshot);
  // Update only this member's existing dots; a draining event must not wait
  // for a full room rebuild to stop advertising its previous Ready state.
  const readiness = sessionReadiness(snapshot, wasRunningAtShutdown(snapshot));
  for (const chip of document.querySelectorAll<HTMLElement>(
    `[data-session-readiness="${snapshot.session_id}"]`,
  )) {
    chip.title = readiness.reason;
    const dot = chip.querySelector<HTMLElement>(".readiness-dot");
    if (dot) dot.dataset.state = readiness.kind;
    const label = chip.querySelector<HTMLElement>("[data-readiness-label]");
    if (label) label.title = readiness.reason;
    const words = chip.querySelector<HTMLElement>("[data-readiness-text]");
    if (words && words.textContent !== readiness.reason) words.textContent = readiness.reason;
    syncRoomMemberWake(chip, snapshot);
  }
  updateRoomMemberUsage();
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
    outputCredits.disposeSession(buffered);
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
      clearPending: () => {
        outputCredits.disposeSession(sessionId);
        pendingOutput.delete(sessionId);
      },
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
    const selected = session.session_id === transition.state.activeId;
    card.hidden = !selected;
    card.inert = !selected;
    card.setAttribute("aria-hidden", String(!selected));
    updateSessionCard(card, session);
    fragment.appendChild(card);
  }
  workspaceGrid.replaceChildren(fragment);

  for (const session of effectiveSessions) {
    if (paneMap.has(session.session_id)) {
      continue;
    }
    let pane: SessionTerminal;
    try {
      pane = new SessionTerminal(session);
    } catch (error) {
      const host = must<HTMLDivElement>(`[data-terminal="${session.session_id}"]`);
      const message = `${session.label}: ${String(error)}`;
      if (host.dataset.layoutError !== message) {
        host.dataset.layoutError = message;
        host.textContent = message;
        writeSystem("error", message);
      }
      continue;
    }
    paneMap.set(session.session_id, pane);
    const pending = pendingOutput.get(session.session_id);
    if (!pending) {
      continue;
    }
    pendingOutput.delete(session.session_id);
    if (pending.shed > 0) {
      for (const credit of pending.credits ?? []) {
        if (credit) outputCredits.settle(credit, "unattached_overflow");
      }
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
          + " tail chunks and requested a full repaint",
      );
      continue;
    }
    for (const [index, chunk] of pending.chunks.entries()) {
      const credit = pending.credits?.[index];
      try {
        pane.write(chunk, credit ? () => outputCredits.settle(credit, "parsed") : undefined);
      } catch (error) {
        if (credit) outputCredits.settle(credit, "write_failed");
        writeSystem("error", `Buffered output was not parsed: ${String(error)}; display is incomplete.`);
        pane.requestFullRepaintResync();
      }
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
    launch.textContent = isSleeping(session) ? "Wake" : "Launch";
  }
  if (restart) {
    restart.disabled = false;
  }
  if (stop) {
    stop.disabled = !canStopSession(session);
    stop.title = session.hibernated ? "Keep this member stopped; new messages will not wake it." : "Stop this member";
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
        lifecycleLabel(sessionDisplayState(session)),
        "lifecycle",
        sessionDisplayState(session),
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
        lifecycleLabel(sessionDisplayState(session)),
        "lifecycle",
        sessionDisplayState(session),
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
  focusWithinActiveWindow(tabMenu.querySelector<HTMLButtonElement>("[data-tab-menu]:not(:disabled)"));
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
  let reportSwitchShown: (() => void) | null = null;
  if (perfEnabled) {
    if (!activeId || activeId === tabState.activeId) {
      // An unchanged refresh must not cancel the current target's pending
      // paint measurement, nor acquire its own reporting callback.
      perfProbes.switchCancelled(activeId !== null);
    } else {
      perfProbes.switchStarted();
      const target = paneMap.get(activeId);
      reportSwitchShown = perfProbes.switchTarget((rendered) => {
        const subscription = target?.terminal.onRender(() => {
          rendered();
        });
        return () => subscription?.dispose();
      });
    }
  }
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
    terminalLayout.request({
      target: activeId,
      ...(focusTerminal ? { focus: () => {
        // v3 W6 (E): only inside a window the user is in; never an OS focus request.
        if (isPaneVisible(activeId)) focusWithinActiveWindow(paneMap.get(activeId)?.terminal);
      } } : {}),
      ...(reportSwitchShown ? { shown: () => {
        if (isPaneVisible(activeId)) reportSwitchShown();
      } } : {}),
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
  focusWithinActiveWindow(sessionTabs
    .querySelector<HTMLButtonElement>(
      '[data-session-tab="' + targetId + '"]',
    ));
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
    terminalLayout.request();
  }
  workspaceGrid.hidden = gridHidden;
  zeroWorkspace.textContent = workspacePreference || "the workspace you choose under Custom…";
  if (visible && !zeroStateWasVisible) {
    renderAttachChoices();
    requestAnimationFrame(() => {
      focusWithinActiveWindow(zeroAttachChoices.querySelector<HTMLButtonElement>("button"));
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
      focusWithinActiveWindow(anchor);
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
    focusWithinActiveWindow(quickAttachSettings
      .querySelectorAll<HTMLButtonElement>(".settings-row-move")[to * 2 + (delta < 0 ? 0 : 1)]);
  });
  return button;
}

/* ── Settings → Keys (D10): one chord per harness action; one column per
   harness showing what it receives (harness-keys.ts). ── */

function wireKeysSettings(): void {
  keysReset.addEventListener("click", () => {
    cancelKeyCapture();
    userBindings = DEFAULT_BINDINGS;
    saveUserBindings();
    renderKeysSettings();
    renderKeyboardHelp();
    setKeysStatus("Every key is back to its default.");
  });
}

/** Prime's column shows only while Prime is enabled; it is parked in v2. */
function keyTableDrivers(): DriverKind[] {
  return KEY_TABLE_DRIVERS.filter((driver) => driver !== "prime" || primeAvailable);
}

function renderKeysSettings(): void {
  const drivers = keyTableDrivers();
  const headRow = document.createElement("tr");
  for (const title of ["Action", "Your key", ...drivers.map(driverLabel)]) {
    const cell = document.createElement("th");
    cell.scope = "col";
    cell.textContent = title;
    headRow.append(cell);
  }
  const head = document.createElement("thead");
  head.append(headRow);
  const body = document.createElement("tbody");
  for (const action of BINDABLE_ACTIONS) {
    const { label, help } = bindableAction(action);
    const name = document.createElement("th");
    name.scope = "row";
    name.textContent = label;
    const hint = document.createElement("small");
    hint.textContent = help;
    name.append(hint);
    const row = document.createElement("tr");
    row.append(
      name,
      chordCell(action),
      ...drivers.map((driver) => translationCell(harnessKeyTranslation(action, driver, keyPlatform))),
    );
    body.append(row);
  }
  keysTable.replaceChildren(head, body);
}

/** The user's chord as a button (click to rebind), with its condition and the conventions that also run the action. */
function chordCell(action: BindableAction): HTMLTableCellElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "ghost key-chord";
  button.dataset.keysAction = action;
  fillChordButton(button, action);
  button.addEventListener("click", () => {
    if (keyCapture?.button === button) {
      cancelKeyCapture("Unchanged.");
    } else {
      startKeyCapture(action, button);
    }
  });
  button.addEventListener("blur", () => {
    if (keyCapture?.button === button) {
      cancelKeyCapture("Unchanged.");
    }
  });
  const cell = document.createElement("td");
  cell.append(button);
  const own = ownChords(keyPlatform, userBindings)[action];
  const chords = chordsFor(action, keyPlatform, userBindings);
  const also = own ? chords.slice(1) : chords;
  const notes: string[] = [];
  if (action === "copy" && own) {
    notes.push("with a selection");
  }
  if (also.length > 0) {
    notes.push("also " + also.map((chord) => describeChord(chord, keyPlatform)).join(", "));
  }
  if (notes.length > 0) {
    const note = document.createElement("small");
    note.textContent = notes.join("; ");
    cell.append(note);
  }
  return cell;
}

function fillChordButton(button: HTMLButtonElement, action: BindableAction): void {
  const own = ownChords(keyPlatform, userBindings)[action];
  delete button.dataset.capturing;
  if (own) {
    const label = describeChord(own, keyPlatform);
    button.replaceChildren(...kbdNodes(label));
    button.setAttribute("aria-label", `${bindableAction(action).label}: ${label}. Press to change.`);
  } else {
    // Only a store from an older table leaves an action unbound (keymap.ts ownChords).
    button.replaceChildren("Not bound");
    button.setAttribute("aria-label", `${bindableAction(action).label}: not bound. Press to bind.`);
  }
}

function translationCell(translation: HarnessKeyTranslation): HTMLTableCellElement {
  const cell = document.createElement("td");
  switch (translation.kind) {
    case "keys": {
      const bytes = document.createElement("small");
      bytes.className = "keys-bytes";
      bytes.textContent = describeKeyBytes(translation.bytes);
      cell.append(...kbdNodes(translation.harnessKey), bytes);
      cell.title = translation.source;
      break;
    }
    case "terminal":
      cell.textContent = translation.label;
      cell.title = translation.source;
      break;
    case "unsupported":
      cell.className = "keys-muted";
      cell.textContent = "not supported";
      cell.title = `${translation.reason} The key reaches the harness as typed.`;
      break;
    case "unverified":
      cell.className = "keys-muted";
      cell.textContent = "unverified";
      cell.title = `${translation.note} The key reaches the harness as typed.`;
      break;
  }
  return cell;
}

/** "Ctrl+Shift+V" as <kbd> parts; a "+" key keeps its own part. */
function kbdNodes(label: string): Node[] {
  const parts = label.endsWith("++") ? [...label.slice(0, -2).split("+"), "+"] : label.split("+");
  return parts.flatMap((part, index) => {
    const kbd = document.createElement("kbd");
    kbd.textContent = part;
    return index === 0 ? [kbd] : [document.createTextNode("+"), kbd];
  });
}

function setKeysStatus(message: string, tone: "info" | "error" = "info"): void {
  keysStatus.textContent = message;
  keysStatus.dataset.tone = tone;
}

function startKeyCapture(action: BindableAction, button: HTMLButtonElement): void {
  cancelKeyCapture();
  keyCapture = { action, button };
  button.dataset.capturing = "true";
  button.replaceChildren("Press keys…");
  setKeysStatus(`Press the new key for ${bindableAction(action).label}. Esc cancels.`);
}

function cancelKeyCapture(message?: string): void {
  const capture = keyCapture;
  if (!capture) {
    return;
  }
  keyCapture = null;
  fillChordButton(capture.button, capture.action);
  if (message !== undefined) {
    setKeysStatus(message);
  }
}

/**
 * Settings → Keys capture, ahead of every other key handler: each press
 * belongs to the capture until a chord is accepted or Esc cancels. A modifier
 * on its own keeps listening; a refused chord says why and keeps listening.
 */
function captureKey(event: KeyboardEvent): void {
  const capture = keyCapture;
  if (!capture) {
    return;
  }
  consumeKey(event);
  if (event.repeat) {
    return;
  }
  const result = captureChord(capture.action, event, keyPlatform, userBindings);
  switch (result.kind) {
    case "waiting":
      return;
    case "cancelled":
      cancelKeyCapture("Unchanged.");
      return;
    case "refused":
      setKeysStatus(`${result.message} Press another key, or Esc.`, "error");
      return;
    case "bound":
      keyCapture = null;
      userBindings = result.bindings;
      saveUserBindings();
      renderKeysSettings();
      renderKeyboardHelp();
      setKeysStatus(
        `${bindableAction(capture.action).label} is now ${describeChord(result.chord, keyPlatform)}.`,
      );
      focusWithinActiveWindow(keysTable
        .querySelector<HTMLButtonElement>(`[data-keys-action="${capture.action}"]`));
      return;
  }
}

/** F1's keyboard rows follow the user's chords. */
function renderKeyboardHelp(): void {
  helpKeys.innerHTML = keyboardHelpMarkup(keyPlatform);
}

function toggleAttachMenu(anchor: HTMLElement, focusFirst = false): void {
  if (attachMenuOpen && attachMenuAnchor === anchor) {
    setAttachMenuOpen(false);
    return;
  }
  attachMenuAnchor = anchor;
  setAttachMenuOpen(true);
  if (focusFirst) {
    focusWithinActiveWindow(attachChoices.querySelector<HTMLButtonElement>("[data-attach]"));
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
    Start button; a failure leaves the stopped session with the error on the pane. */
/** The last Start request that failed, per session, so a search retry
    replays exactly it (S5). */
const lastFailedStart = new Map<string, StartSessionRequest>();

async function launchSession(
  sessionId: string,
  options: {
    search?: boolean;
    fresh?: boolean;
    activate?: boolean;
    launchChoices?: SessionLaunchChoices;
    resumeMode?: BackgroundResumeMode;
  } = {},
): Promise<void> {
  const request: StartSessionRequest = {
    session_id: sessionId,
    search: options.search,
    fresh: options.fresh,
    launch_choices: options.launchChoices,
  };
  try {
    const selected = resumeLaunchCommand(request, options.resumeMode, snapshotById.get(sessionId)?.driver);
    const snapshot = await command<SessionSnapshot>(selected.name, selected.args);
    lastFailedStart.delete(sessionId);
    applyCommandSessionSnapshot(snapshot);
    if (options.activate !== false) {
      setActiveSession(sessionId, true);
    }
  } catch (error) {
    lastFailedStart.set(sessionId, request);
    await reportLaunchFailure(
      sessionId,
      error,
      options.fresh ? "Start fresh" : "launch",
      options.activate !== false,
    );
  }
}

async function reportLaunchFailure(
  sessionId: string,
  error: unknown,
  action: string,
  activate = true,
): Promise<void> {
  writeSystem("error", `${action} ${paneLabel(sessionId)} failed: ${String(error)}`);
  try {
    // A background launch (boot resume) must not pull focus to its failure,
    // nor pin whichever tab was active when it failed (Codex U2).
    await refreshSnapshot(activate ? sessionId : undefined);
  } catch {
    const pane = paneMap.get(sessionId);
    const previous = snapshotById.get(sessionId);
    if (pane && previous) {
      pane.applySnapshot({
        ...previous,
        running: false,
        lifecycle_state: "failed",
        last_error: String(error),
        last_error_kind: null,
      });
    }
  }
}

function openCreateSessionForm(driver?: DriverKind): void {
  if (trialStatus !== null) return;
  setShellView("sessions");
  primeDefaultRequest += 1;
  const defaults = newSessionFormDefaults(workspacePreference);
  sessionFormMode = { kind: "create" };
  sessionFormPending = false;
  sessionLabelInput.value = "";
  sessionDriverSelect.value = driver ?? defaults.driver;
  sessionPermissionSelect.value = defaults.permissionProfile;
  sessionCodexTransportSelect.value = "pty";
  sessionLinuxWorkingDirectory.value = "";
  fillLaunchChoices(null);
  setFormGrants({ roam: false, maintain: false, manage: false });
  setSessionFormError(null);
  syncSessionForm();
  if (sessionDriverSelect.value === "prime") {
    // A preselected Prime gets the same Ubuntu-home discovery a manual pick triggers.
    void selectSessionDriver();
  }
  requestAnimationFrame(() => focusWithinActiveWindow(sessionLabelInput));
}

function openEditSessionForm(sessionId: string): void {
  if (trialStatus !== null) return;
  const session = snapshotById.get(sessionId);
  if (!session) {
    return;
  }
  // The editor lives in the sessions view; Edit from a tab menu or a member
  // row in the rooms view used to fill it out of sight (Victor, 20 Oct).
  setShellView("sessions");
  primeDefaultRequest += 1;
  sessionFormMode = { kind: "edit", sessionId };
  sessionFormPending = false;
  sessionLabelInput.value = session.label;
  sessionDriverSelect.value = session.driver;
  sessionPermissionSelect.value = session.permission_profile;
  sessionCodexTransportSelect.value = session.codex_transport ?? "pty";
  sessionLinuxWorkingDirectory.value =
    session.driver === "prime" ? session.working_dir : "";
  fillLaunchChoices(session.launch_choices);
  setFormGrants({
    roam: session.permissions?.roam ?? false,
    maintain: session.permissions?.maintain ?? false,
    manage: session.permissions?.manage ?? false,
  });
  setSessionFormError(null);
  syncSessionForm();
  requestAnimationFrame(() => focusWithinActiveWindow(sessionLabelInput));
}

function formGrants(): Required<MemberPermissions> {
  return { roam: sessionGrantRoam.checked, maintain: sessionGrantMaintain.checked, manage: sessionGrantManage.checked };
}

/** The three grants, and the Supervisor box that stands for all three. */
function setFormGrants(grants: Required<MemberPermissions>): void {
  sessionGrantRoam.checked = grants.roam;
  sessionGrantMaintain.checked = grants.maintain;
  sessionGrantManage.checked = grants.manage;
  sessionGrantSupervisor.checked = isSupervisorGrants(grants);
}

function fillLaunchChoices(choices: SessionLaunchChoices | null): void {
  sessionModelInput.value = choices?.model ?? "";
  sessionEffortInput.value = choices?.effort ?? "";
  sessionBriefMode.value = choices?.brief.mode ?? "room_default";
  sessionBriefCustom.value = choices?.brief.mode === "custom" ? choices.brief.text : "";
}

function formLaunchChoices(): SessionLaunchChoices | null {
  return launchChoicesFromForm(
    sessionDriverSelect.value as DriverKind,
    sessionModelInput.value,
    sessionEffortInput.value,
    sessionBriefMode.value as BriefMode,
    sessionBriefCustom.value,
  );
}

/** Edit mode: choices are saved by the next Start (S5 has no separate setter),
    so changed choices on a stopped agent turn Save into Save and launch. */
function editedLaunchChoices(): SessionLaunchChoices | null {
  if (sessionFormMode?.kind !== "edit") {
    return null;
  }
  const session = snapshotById.get(sessionFormMode.sessionId);
  const chosen = formLaunchChoices();
  if (!session || session.running || !chosen) {
    return null;
  }
  return sameLaunchChoices(chosen, session.launch_choices) ? null : chosen;
}

function closeSessionForm(): void {
  primeDefaultRequest += 1;
  sessionFormMode = null;
  sessionFormPending = false;
  sessionEditor.hidden = true;
  setSessionFormError(null);
  updateZeroSessionState();
  if (tabState.activeId) {
    focusWithinActiveWindow(sessionTabs
      .querySelector<HTMLButtonElement>(
        '[data-session-tab="' + tabState.activeId + '"]',
      ));
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
  const choicesDriver = sessionDriverSelect.value as DriverKind;
  const editingRunning =
    sessionFormMode.kind === "edit"
    && Boolean(snapshotById.get(sessionFormMode.sessionId)?.running);
  sessionLaunchChoicesField.hidden = !acceptsLaunchChoices(choicesDriver);
  // v3 W4 S5: chosen at create only; an existing member shows its own,
  // fixed (switching means creating a new member).
  sessionCodexTransportField.hidden = !acceptsCodexTransport(choicesDriver);
  sessionCodexTransportSelect.disabled = sessionFormPending || sessionFormMode.kind === "edit";
  sessionCodexTransportNote.textContent = sessionFormMode.kind === "edit"
    ? "fixed when the member was created; to switch, create a new member"
    : "how PRIM-1 sends this member room messages; fixed once created";
  sessionBriefCustomField.hidden = sessionBriefMode.value !== "custom";
  for (const control of [sessionModelInput, sessionEffortInput, sessionBriefMode, sessionBriefCustom]) {
    control.disabled = sessionFormPending || editingRunning;
  }
  // Grants belong to an existing member and apply while it runs (A109).
  // Prime and Terminal panes have no room tools; the backend refuses them.
  const grantsApply = acceptsMemberGrants(choicesDriver);
  // A16: grants are chosen at create too; the Supervisor box ticks all three.
  sessionMemberGrants.hidden = !grantsApply && sessionFormMode.kind === "create";
  sessionGrantSupervisorField.hidden = !grantsApply;
  sessionGrantSupervisor.disabled = sessionFormPending || !grantsApply;
  sessionGrantRoam.disabled = sessionFormPending || !grantsApply;
  sessionGrantMaintain.disabled = sessionFormPending || !grantsApply;
  // A110: only an agent can manage agents; Terminal and Prime never see it.
  sessionGrantManageField.hidden = !grantsApply;
  sessionGrantManage.disabled = sessionFormPending || !grantsApply;
  sessionGrantsNote.textContent = memberGrantsNote(choicesDriver);
  if (sessionFormMode.kind === "create") {
    sessionChoicesNote.textContent = "Saved with the session and used at every launch.";
  } else if (editingRunning) {
    sessionChoicesNote.textContent = "Stop this agent to change its launch choices.";
  } else {
    sessionChoicesNote.textContent =
      "Launch choices are saved when the agent starts, so changing them launches it.";
    if (editedLaunchChoices()) {
      saveSessionButton.textContent = "Save and launch";
    }
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
  const chosenLaunchChoices = formLaunchChoices();
  if (chosenLaunchChoices?.brief.mode === "custom" && !chosenLaunchChoices.brief.text.trim()) {
    setSessionFormError("Write the custom brief, or choose Room default or None.");
    return;
  }
  const relaunchWithChoices = editedLaunchChoices();
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
        sessionCodexTransportSelect.value === "app_server" ? "app_server" : "pty",
      );
      if (workContext.kind === "room") {
        request.room_id = workContext.roomId; // born into the room you stand in
      }
      if (chosenLaunchChoices) {
        request.launch_choices = chosenLaunchChoices;
      }
      const grants = createGrantsFromForm(driver, formGrants());
      if (grants) {
        request.permissions = grants;
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
      acceptsMemberGrants(current.driver)
      && (sessionGrantRoam.checked !== (current.permissions?.roam ?? false)
        || sessionGrantMaintain.checked !== (current.permissions?.maintain ?? false)
        || sessionGrantManage.checked !== (current.permissions?.manage ?? false))
    ) {
      await command<SessionSnapshot>("set_member_permissions", {
        request: {
          session_id: current.session_id,
          roam: sessionGrantRoam.checked,
          maintain: sessionGrantMaintain.checked,
          manage: sessionGrantManage.checked,
        } satisfies SetMemberPermissionsRequest,
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
    if (relaunchWithChoices) {
      // Start persists the new choices before it spawns (S5).
      await launchSession(current.session_id, { launchChoices: relaunchWithChoices });
    }
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
  focusWithinActiveWindow(sessionTabs
    .querySelector<HTMLButtonElement>(
      '[data-session-tab="' + sessionId + '"]',
    ));
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
  if (!(await confirmAction(closeSessionConfirmText(session.label, shortSessionId(sessionId), consequences), "Close"))) {
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
  if (trialStatus !== null) return;
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
  // Only sessions that can actually join: membership is exclusive, so a
  // harness already in a room is not a choice here (Victor, 2026-08-30).
  const available = [...snapshotById.values()].filter(
    (session) => sessionRoomIdPure(roomSnapshots, session.session_id) === null,
  );
  if (available.length === 0) {
    const empty = document.createElement("p");
    empty.className = "room-member-choices-empty";
    empty.textContent =
      "No available sessions — the lobby is empty and every harness is already "
      + 'in a room. An empty room is fine: create it, and "+" spawns members '
      + "straight into it.";
    roomMemberChoices.append(empty);
  } else {
    for (const session of available) {
      const label = document.createElement("label");
      label.className = "room-member-choice";
      const checkbox = document.createElement("input");
      checkbox.type = "checkbox";
      checkbox.value = session.session_id;
      const text = document.createElement("span");
      text.textContent = `${session.label} · ${driverLabel(session.driver)} · ${shortSessionId(session.session_id)}`;
      label.append(checkbox, text);
      roomMemberChoices.append(label);
    }
  }
  renderRoomUi();
  focusWithinActiveWindow(roomLabelInput);
}

function closeRoomCreateForm(): void {
  roomCreateForm.hidden = true;
  roomCreateError.hidden = true;
  renderRoomUi();
}

/* ── Room brief settings for an existing room (S5 read/update_room_brief) ──
   Loaded when the section opens or the room changes; a null template is the
   canonical default, kept null so improvements to the default reach it. */
/** The room whose settings the editor holds, set only once they have loaded,
    and a token so a late load or save reply for another room or an older
    request never lands in the editor (Codex review). */
let roomBriefLoadedFor: string | null = null;
let roomBriefRequestedFor: string | null = null;
let roomBriefToken = 0;

function setRoomBriefBusy(busy: boolean): void {
  for (const control of [roomBriefOnLaunch, roomBriefText, roomBriefSaveButton, roomBriefDefaultButton]) {
    control.disabled = busy;
  }
}

async function loadRoomBriefSettings(roomId: string): Promise<void> {
  const token = ++roomBriefToken;
  roomBriefRequestedFor = roomId;
  roomBriefLoadedFor = null;
  setRoomBriefBusy(true);
  roomBriefState.textContent = "Loading…";
  try {
    const [settings, fallback] = await Promise.all([
      command<RoomBriefSettings>("read_room_brief", {
        request: { room_id: roomId } satisfies ReadRoomBriefRequest,
      }),
      loadDefaultRoomBrief(),
    ]);
    if (token !== roomBriefToken || settings.room_id !== roomId) {
      return;
    }
    roomBriefOnLaunch.checked = settings.brief_on_join;
    roomBriefText.value = settings.brief_template ?? fallback;
    roomBriefState.textContent = settings.brief_template === null ? "Default text" : "Custom text";
    roomBriefLoadedFor = roomId;
    setRoomBriefBusy(false);
  } catch (error) {
    if (token === roomBriefToken) {
      roomBriefState.textContent = `Could not load the brief: ${String(error)}`;
    }
  }
}

async function saveRoomBriefSettings(useDefault: boolean): Promise<void> {
  // Save only what the editor actually holds, for the room it was loaded from.
  const roomId = roomBriefLoadedFor;
  if (!roomId) {
    return;
  }
  const text = roomBriefText.value;
  const template =
    useDefault || text.trim() === "" || text === defaultRoomBrief ? null : text;
  const token = ++roomBriefToken;
  setRoomBriefBusy(true);
  roomBriefState.textContent = "Saving…";
  try {
    const settings = await command<RoomBriefSettings>("update_room_brief", {
      request: {
        room_id: roomId,
        brief_on_join: roomBriefOnLaunch.checked,
        brief_template: template,
      } satisfies UpdateRoomBriefRequest,
    });
    if (token !== roomBriefToken || settings.room_id !== roomId) {
      return;
    }
    roomBriefOnLaunch.checked = settings.brief_on_join;
    roomBriefText.value = settings.brief_template ?? (defaultRoomBrief ?? "");
    const kind = settings.brief_template === null ? "default text" : "custom text";
    // The room brief reaches a launch only while it is on (resolve_launch_brief).
    roomBriefState.textContent = settings.brief_on_join
      ? `Saved — ${kind}; members on "room default" get it in their next fresh conversation.`
      : `Saved — ${kind}; not in use while "Use this room brief by default" is off.`;
  } catch (error) {
    if (token === roomBriefToken) {
      roomBriefState.textContent = `Not saved: ${String(error)}`;
    }
  } finally {
    if (token === roomBriefToken) {
      setRoomBriefBusy(false);
    }
  }
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

/** Save… : the native dialog picks the file, then `export_room` writes it. */
async function saveActiveRoom(): Promise<void> {
  const room = activeRoom();
  if (!room) {
    return;
  }
  try {
    const path = await command<string | null>("choose_room_save_path", { label: room.label });
    if (!path) {
      return;
    }
    setRoomStatus(`Saving "${room.label}"…`, "info");
    const result = roomSaveSummary(await command<RoomExportReport>("export_room", { roomId: room.room_id, path }));
    writeSystem(result.level, result.summary);
    setRoomStatus(result.summary, result.level);
  } catch (error) {
    writeSystem("error", `Room "${room.label}" was not saved: ${errorText(error)}`);
    setRoomStatus(errorText(error), "error");
    // A failed copy can hold the room's writer for replay; show it.
    try { await refreshSnapshot(tabState.activeId ?? undefined); } catch { /* the status already says why */ }
  }
}

/** Load room… : a `.prim1room` file or a copied runtime's room catalog
    becomes a new room with new members after a confirm. Nothing starts; the
    status line states each member's resume state. */
async function loadRoomFromFile(): Promise<void> {
  roomsLoadProblem.hidden = true;
  await loadRoomFlow({
    choose: () => command<string | null>("choose_room_source"),
    inspect: (path) => command<RoomSourceSummary[]>("inspect_room_source", { path }),
    ask: (list) => window.prompt(list),
    confirm: (text) => confirmAction(text, "Load"),
    load: (path, sourceRoomId) => command<RoomImportReport>("import_room", { path, sourceRoomId }),
    refresh: async () => { await refreshSnapshot(tabState.activeId ?? undefined); },
    enter: (roomId) => {
      enterWorkContext({ kind: "room", roomId });
      openRoomPanel(roomId);
    },
    loaded: (result) => {
      // Also in the system log: a later feed notice may replace the status line.
      writeSystem(result.level, result.summary);
      setRoomStatus(result.summary, result.level, result.details);
    },
    problem: (message) => {
      writeSystem("error", message);
      openRoomsOverview();
      roomsLoadProblem.textContent = message;
      roomsLoadProblem.hidden = false;
    },
  });
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
  if (!(await confirmAction(deleteRoomConfirmText(room.label), "Delete"))) {
    return;
  }
  try {
    await command<void>("delete_room", {
      request: { room_id: room.room_id } satisfies DeleteRoomRequest,
    });
    await refreshSnapshot(tabState.activeId ?? undefined);
  } catch (error) {
    // The catalog may have committed even when erasing its history failed.
    try { await refreshSnapshot(tabState.activeId ?? undefined); }
    catch (refreshError) { writeSystem("error", `Room inventory refresh failed: ${String(refreshError)}`); }
    const message = `Room "${room.label}": ${String(error)}`;
    writeSystem("error", message);
    setRoomStatus(message, "error");
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

// Keep uncertain intent keys within the bounded, in-memory retry horizon.
// Changing the room, action, target or exact text starts a different intent.
const pendingRoomMessageIntents = new Map<string, string>();

async function postActiveRoomMessage(): Promise<void> {
  const room = activeRoom();
  const recovery = roomWriteRecoveryMessage(room);
  if (recovery) {
    setRoomStatus(recovery, "error");
    return;
  }
  if (room && roomDrafts.ownerRoomId !== room.room_id) {
    // Never submit text that belongs to another room.
    bindComposerToActiveRoom();
    setRoomStatus("The message box was showing another room's draft. It now shows this room's; check it and try again.", "warn");
    return;
  }
  const content = roomMessage.value;
  if (!room || !content.trim()) {
    setRoomStatus("Enter a message before posting.", "warn");
    return;
  }
  let intent: ReturnType<typeof roomMessageIntent>;
  try {
    intent = roomMessageIntent(pendingRoomMessageIntents, room.room_id, "feed", content);
  } catch (error) {
    setRoomStatus(String(error), "error");
    return;
  }
  pendingRoomSubmissions.add(room.room_id);
  renderRoomComposerAvailability();
  try {
    const result = await submitRoomIntent(pendingRoomMessageIntents, intent, (requestId) => command<RoomPostResult>("post_room_message", {
      request: {
        room_id: room.room_id,
        content,
        request_id: requestId,
      } satisfies PostRoomMessageRequest,
    }));
    const status = `Stored in the room feed${result.replayed ? " (previous request)" : ""}; no harness was prompted.`;
    if (roomDrafts.completeSubmission(room.room_id, content, roomMessage.value)) {
      roomMessage.value = "";
    }
    if (activeRoomId === room.room_id) {
      setRoomStatus(status, "info");
    } else {
      writeSystem("info", `Room ${room.label}: ${status}`);
    }
  } catch (error) {
    if (activeRoomId === room.room_id) {
      setRoomStatus(roomRequestFailureFeedback(error), "error");
    } else {
      writeSystem("error", `Room ${room.label}: ${roomRequestFailureFeedback(error)}`);
    }
  } finally {
    pendingRoomSubmissions.delete(room.room_id);
    try { await refreshSnapshot(tabState.activeId ?? undefined); }
    catch (error) { writeSystem("warn", `Room status refresh failed: ${String(error)}`); }
    renderRoomComposerAvailability();
  }
}

async function deliverActiveRoomMessage(): Promise<void> {
  const room = activeRoom();
  const recovery = roomWriteRecoveryMessage(room);
  if (recovery) {
    setRoomStatus(recovery, "error");
    return;
  }
  if (room && roomDrafts.ownerRoomId !== room.room_id) {
    // Never submit text that belongs to another room.
    bindComposerToActiveRoom();
    setRoomStatus("The message box was showing another room's draft. It now shows this room's; check it and try again.", "warn");
    return;
  }
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
  let intent: ReturnType<typeof roomMessageIntent>;
  try {
    intent = roomMessageIntent(pendingRoomMessageIntents, room.room_id, `send:${roomRecipient.value}`, content);
  } catch (error) {
    setRoomStatus(String(error), "error");
    return;
  }
  pendingRoomSubmissions.add(room.room_id);
  renderRoomComposerAvailability();
  try {
    const result = await submitRoomIntent(pendingRoomMessageIntents, intent, (requestId) => command<RoomDeliveryResult>("deliver_room_message", {
      request: {
        room_id: room.room_id,
        recipients,
        content,
        request_id: requestId,
      } satisfies DeliverRoomMessageRequest,
    }));
    const feedback = roomDeliveryFeedback(result, paneLabel, (id) => roomWakeUnavailable(snapshotById.get(id)?.driver));
    if (roomDrafts.completeSubmission(room.room_id, content, roomMessage.value)) {
      roomMessage.value = "";
    }
    if (activeRoomId === room.room_id) {
      setRoomStatus(feedback.summary, feedback.level, feedback.message);
    } else {
      writeSystem(
        feedback.level,
        `Room ${room.label}: ${feedback.message}`,
      );
    }
  } catch (error) {
    if (activeRoomId === room.room_id) {
      setRoomStatus(roomRequestFailureFeedback(error), "error");
    } else {
      writeSystem(
        "error",
        `Room ${room.label}: ${roomRequestFailureFeedback(error)}`,
      );
    }
  } finally {
    pendingRoomSubmissions.delete(room.room_id);
    try { await refreshSnapshot(tabState.activeId ?? undefined); }
    catch (error) { writeSystem("warn", `Room status refresh failed: ${String(error)}`); }
    renderRoomComposerAvailability();
  }
}

async function retryRoomDelivery(event: RoomFeedEvent): Promise<void> {
  const room = activeRoom();
  if (!room || room.room_id !== event.room_id || event.item.kind !== "delivery") return;
  const recovery = roomWriteRecoveryMessage(room);
  if (recovery) {
    setRoomStatus(recovery, "error");
    return;
  }
  const events = roomFeedEvents.get(room.room_id) ?? [];
  const unavailable = roomDeliveryRetryUnavailable(event, room.member_ids, snapshotById);
  if (unavailable || !retryableRoomDeliveryEvents(room.room_id, events).has(event)) {
    setRoomStatus(unavailable ?? "That wake receipt changed. Use its latest receipt.", "warn");
    renderRoomFeed(events);
    return;
  }
  const request: RetryRoomDeliveryRequest = {
    room_id: room.room_id,
    message_id: event.item.message_id,
    recipient_id: event.item.recipient_id,
    expected_receipt: event.cursor,
  };
  const key = roomDeliveryRetryKey(request.room_id, request.message_id, request.recipient_id);
  if (pendingRoomWakeRetries.has(key)) return;
  if (pendingRoomWakeRetries.size >= MAX_PENDING_ROOM_WAKE_RETRIES) {
    setRoomStatus("Wait for an active wake retry to finish.", "warn");
    return;
  }
  pendingRoomWakeRetries.add(key);
  renderRoomFeed(events);
  const recipient = paneLabel(request.recipient_id);
  setRoomStatus(`Retrying wake for ${recipient}…`, "info");
  let completion: { message: string; details?: string; level: "info" | "warn" | "error" } | undefined;
  try {
    const result = await command<RoomDeliveryResult>("retry_room_delivery", { request });
    const feedback = roomRetryFeedback(result, request.recipient_id, paneLabel, (id) => roomWakeUnavailable(snapshotById.get(id)?.driver));
    const context = `Wake retry for ${recipient}, message ${request.message_id.slice(0, 8)}: `;
    completion = { message: context + feedback.summary, details: context + feedback.message, level: feedback.level };
  } catch (error) {
    const failure = roomRequestFailure(error);
    completion = { message: `${failure.kind === "rejected" || failure.kind === "recovery_required" || failure.kind === "history_unavailable" ? "Not retried" : "Retry outcome unknown"} for ${recipient}, message ${request.message_id.slice(0, 8)}: ${failure.message}`, level: "error" };
  } finally {
    try { await refreshSnapshot(tabState.activeId ?? undefined); }
    catch (error) { writeSystem("warn", `Room status refresh failed: ${String(error)}`); }
    // Results have no receipt cursor; reload authoritative rows even after a lost reply.
    if (activeRoomId === room.room_id && !roomFeedUnavailable(room.room_id)) {
      await initializeRoomFeed(room.room_id, true);
    }
    pendingRoomWakeRetries.delete(key);
    // A retry in another room may have released the global concurrency cap.
    const selected = activeRoom();
    if (selected) renderRoomFeed(roomFeedEvents.get(selected.room_id) ?? []);
    if (completion) {
      if (activeRoomId === room.room_id) setRoomStatus(completion.message, completion.level, completion.details);
      else writeSystem(completion.level, `Room ${room.label}: ${completion.details ?? completion.message}`);
    }
  }
}

function setRoomStatus(message: string, level: "info" | "warn" | "error", details?: string): void {
  const recovery = roomWriteRecoveryMessage(activeRoom());
  const summary = roomStatus.querySelector<HTMLElement>("#room-status-summary")!;
  const disclosure = roomStatus.querySelector<HTMLDetailsElement>("#room-status-details")!;
  const detailText = roomStatus.querySelector<HTMLElement>("#room-status-detail-text")!;
  const visible = recovery ?? message;
  const full = recovery ? "" : details ?? "";
  // Keep an unchanged disclosure mounted and open across snapshot refreshes.
  if (summary.textContent !== visible) summary.textContent = visible;
  if (detailText.textContent !== full) {
    detailText.textContent = full;
    disclosure.open = false;
  }
  disclosure.hidden = !full;
  // Keep the live region mounted before its first result so it can announce it.
  roomStatus.dataset.level = recovery ? "error" : level;
  if (recovery) {
    roomStatus.dataset.recoveryRoomId = activeRoomId ?? "";
  } else {
    delete roomStatus.dataset.recoveryRoomId;
  }
}

/** Store an explicit brief intent and attempt its separately admitted wake. */
async function briefRoomMemberNow(roomId: string, sessionId: string): Promise<void> {
  const recovery = roomWriteRecoveryMessage(roomSnapshots.find((room) => room.room_id === roomId));
  if (recovery) {
    if (activeRoomId === roomId) setRoomStatus(recovery, "error");
    else writeSystem("error", recovery);
    return;
  }
  try {
    const result = await command<RoomDeliveryResult>("brief_room_member", {
      request: {
        room_id: roomId,
        session_id: sessionId,
      } satisfies BriefRoomMemberRequest,
    });
    const feedback = roomDeliveryFeedback(result, paneLabel, (id) => roomWakeUnavailable(snapshotById.get(id)?.driver));
    const message = `Room brief for ${paneLabel(sessionId)}: ${feedback.message}`;
    if (activeRoomId === roomId) {
      setRoomStatus(`Room brief for ${paneLabel(sessionId)}: ${feedback.summary}`, feedback.level, message);
    } else {
      writeSystem(feedback.level, message);
    }
  } catch (error) {
    const message = `Room brief for ${paneLabel(sessionId)}: ${roomRequestFailureFeedback(error, false)}`;
    if (activeRoomId === roomId) {
      setRoomStatus(message, "error");
    } else {
      writeSystem("error", message);
    }
  } finally {
    try { await refreshSnapshot(tabState.activeId ?? undefined); }
    catch (error) { writeSystem("warn", `Room status refresh failed: ${String(error)}`); }
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
  roomPostButton.title = ROOM_POST_TITLE;
  roomSendButton.title = "Store the message and type it into each member's terminal (queued while a member is stopped, starting or on a prompt)";
  roomsBackButton.addEventListener("click", openRoomsOverview);
  contextRoomsChip.addEventListener("click", openRoomsOverview);
  contextNameChip.addEventListener("click", () => {
    if (workContext.kind === "room") {
      openRoomPanel(workContext.roomId);
    }
  });
  renameRoomButton.addEventListener("click", () => void renameActiveRoom());
  saveRoomButton.addEventListener("click", () => void saveActiveRoom());
  roomInviteGuestButton.addEventListener("click", openGuestInvite);
  guestInviteForm.addEventListener("submit", (event) => {
    event.preventDefault();
    void createGuestInvitation();
  });
  for (const button of guestInviteDialog.querySelectorAll<HTMLButtonElement>("[data-guest-invite-close]")) {
    button.addEventListener("click", () => guestInviteDialog.close());
  }
  guestInviteDialog.addEventListener("close", forgetGuestInvite);
  must<HTMLButtonElement>("#guest-invite-copy-block").addEventListener("click", () =>
    copyGuestInvite(guestInviteBlock.value, "Invitation"));
  must<HTMLButtonElement>("#guest-invite-copy").addEventListener("click", () =>
    copyGuestInvite(guestInviteConfig.value, "MCP config"));
  must<HTMLButtonElement>("#guest-invite-copy-cli").addEventListener("click", () =>
    copyGuestInvite(guestInviteCli.value, "Script"));
  roomEmpty.querySelector<HTMLButtonElement>("[data-room-load]")!.addEventListener("click", () => void loadRoomFromFile());
  roomResumeToggle.addEventListener("change", () =>
    void setActiveRoomResume(roomResumeToggle.checked),
  );
  roomBriefSettings.addEventListener("toggle", () => {
    const room = activeRoom();
    if (roomBriefSettings.open && room) {
      void loadRoomBriefSettings(room.room_id);
    }
  });
  roomBriefSaveButton.addEventListener("click", () => void saveRoomBriefSettings(false));
  roomBriefDefaultButton.addEventListener("click", () => void saveRoomBriefSettings(true));
  moveRoomLeftButton.addEventListener("click", () => void moveActiveRoom(-1));
  moveRoomRightButton.addEventListener("click", () => void moveActiveRoom(1));
  deleteRoomButton.addEventListener("click", () => void deleteActiveRoom());
  roomAddMemberButton.addEventListener("click", () => void addSelectedRoomMember());
  roomPostButton.addEventListener("click", () => void postActiveRoomMessage());
  roomSendButton.addEventListener("click", () => void deliverActiveRoomMessage());
  roomFeed.dataset.view = roomFeedView;
  syncRoomMembersPanel();
  roomMembersCloseButton.addEventListener("click", () => setRoomMembersPanelOpen(false));
  roomMembersOpenButton.addEventListener("click", () => setRoomMembersPanelOpen(true));
  roomFeed.addEventListener(
    "scroll",
    () => {
      if (roomFeedView === "readable" && roomFeed.clientHeight > 0) {
        readableFeedFollow = readableFeedAtBottom();
      }
    },
    { passive: true },
  );
  const flushReadableEngagement = () => {
    if (!readableReconcilePending || roomFeedView !== "readable") return;
    queueMicrotask(() => {
      if (readableReconcilePending && roomFeedView === "readable") {
        renderRoomFeed(roomFeedEvents.get(activeRoomId ?? "") ?? []);
      }
    });
  };
  document.addEventListener("selectionchange", flushReadableEngagement);
  roomFeed.addEventListener("focusout", flushReadableEngagement);
  // Back from another view, or resized: a reader who was following sees the newest message.
  new ResizeObserver(() => {
    if (roomFeedView === "readable" && readableFeedFollow && roomFeed.clientHeight > 0) {
      roomFeed.scrollTop = roomFeed.scrollHeight;
    }
  }).observe(roomFeed);
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
  for (const control of [sessionModelInput, sessionEffortInput, sessionBriefCustom]) {
    control.addEventListener("input", syncSessionForm);
  }
  sessionBriefMode.addEventListener("change", syncSessionForm);
  sessionGrantSupervisor.addEventListener("change", () => {
    const all = sessionGrantSupervisor.checked;
    setFormGrants({ roam: all, maintain: all, manage: all });
  });
  for (const grant of [sessionGrantRoam, sessionGrantMaintain, sessionGrantManage]) {
    grant.addEventListener("change", () => setFormGrants(formGrants()));
  }
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
        await launchSession(sessionId);
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
    if (document.visibilityState !== "visible") {
      settle = undefined;
      return;
    }
    settle = window.setTimeout(() => {
      settle = undefined;
      terminalLayout.request();
    }, 120);
  };
  const observer = new ResizeObserver(refit);
  observer.observe(document.body);
  observer.observe(workspaceGrid);
  observer.observe(systemTerminalHost);
  window.addEventListener("resize", refit);
}

function fitVisiblePanes(): boolean {
  const active = tabState.activeId ? paneMap.get(tabState.activeId) : undefined;
  // A hidden pane may have exhausted grid recovery before this activation.
  // Show/focus its actionable error without another RPC, fit or repaint.
  if (active?.canCompleteLayoutAtConfirmedGrid()) return true;
  const measured = active?.measure() ?? null;
  if (!measured) {
    return !active || !isPaneVisible(active.sessionId);
  }
  active?.applyGrid(measured.cols, measured.rows);
  return true;
}

function isPaneVisible(sessionId: string): boolean {
  const pane = paneMap.get(sessionId);
  return shellView === "sessions" && tabState.activeId === sessionId
    && pane !== undefined && terminalHostIsVisible(pane.host);
}

/**
 * A consumed chord: no browser default, and no xterm either. xterm listens on
 * its own textarea and never checks `defaultPrevented`, so only stopping the
 * event in this capture phase keeps it from also encoding the chord (Ctrl+C
 * on a selection would copy AND interrupt; F1 would send ESC O P).
 */
function consumeKey(event: KeyboardEvent): void {
  event.preventDefault();
  event.stopPropagation();
}

/**
 * The one keyboard dispatcher. Every chord the app consumes is declared in
 * keymap.ts; this listener runs in the capture phase so a consumed chord never
 * reaches xterm, and an unconsumed one falls through untouched — to the
 * browser inside an editable field, to the pty inside a terminal.
 *
 * Pane actions follow harnessKeyPlan: paste text is stopped but not
 * prevented, so the webview's own paste fires on the pane, where
 * SessionTerminal.paste hands it to xterm; the other actions send the
 * focused harness's own keys and let the keystroke through where none is
 * verified.
 *
 * Tab-scoped chords (arrows on a focused tab) are dispatched by the tab
 * element's own handler; Escape-closes-menu by the menu-scoped listeners.
 */
function wireTerminalShortcuts(): void {
  writeSystem("info", `Keyboard: ${keyboardPlatformLabel(keyPlatform)} bindings (F1 lists them)`);
  window.addEventListener(
    "keydown",
    (event) => {
      if (keyCapture) {
        captureKey(event);
        return;
      }
      if (event.isComposing) {
        // An IME owns the keys until its composition ends (Enter commits it).
        return;
      }
      let selection: CopySelectionResult | null | undefined;
      const currentSelection = (): CopySelectionResult | null => {
        if (selection === undefined) {
          selection = resolveCopySelection({
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
        }
        return selection;
      };
      const activePane = activeTerminal();
      const match = resolveKeyMatch(
        event,
        keyPlatform,
        {
          editable: isEditableShortcutTarget(event.target),
          terminalFocused: activePane !== null && activeTerminalOwnsFocus(activePane),
          tabFocused: false,
          hasSelection: () => currentSelection() !== null,
        },
        userBindings,
      );
      if (!match) {
        return;
      }
      if (trialStatus !== null && (match.action === "new-session" || match.action === "close-session")) {
        consumeKey(event);
        return;
      }
      switch (match.action) {
        case "help":
          consumeKey(event);
          runCornerAction("help");
          return;
        case "fullscreen":
          consumeKey(event);
          void toggleFullscreen();
          return;
        case "new-session":
          consumeKey(event);
          toggleAttachMenu(newSessionButton, true);
          return;
        case "close-session":
          consumeKey(event);
          if (tabState.activeId) {
            void deleteSession(tabState.activeId);
          }
          return;
        case "next-tab":
        case "previous-tab": {
          consumeKey(event);
          const targetId = relativeSessionId(
            visibleTabOrder(),
            tabState.activeId,
            match.action === "next-tab" ? 1 : -1,
          );
          if (targetId) {
            setActiveSession(targetId, true);
          }
          return;
        }
        case "copy": {
          const selected = currentSelection();
          if (!selected) {
            return;
          }
          consumeKey(event);
          // Windows Terminal's rule for a copy chord that is also the
          // interrupt: drop the selection so the next press interrupts.
          const interrupt = ownChords(keyPlatform, userBindings).interrupt;
          copySelectionToClipboard(selected, interrupt !== null && sameChord(match.chord, interrupt));
          return;
        }
        case "paste-text":
        case "paste-image":
        case "submit":
        case "newline":
        case "interrupt": {
          if (!activePane) {
            return;
          }
          // harness-keys.ts: paste text lets the webview's own paste run; a
          // verified translation replaces the keystroke (and cancels any
          // native paste); anything else reaches xterm as typed.
          const plan = harnessKeyPlan(
            match.action,
            snapshotById.get(activePane.sessionId)?.driver,
            keyPlatform,
          );
          if (plan.preventDefault) {
            event.preventDefault();
          }
          if (plan.stopPropagation) {
            event.stopPropagation();
          }
          if (plan.send !== null) {
            activePane.terminal.input(plan.send);
          }
          return;
        }
        default:
          return;
      }
    },
    { capture: true },
  );
}

/**
 * `clearPaneSelection` is the Windows Terminal rule for a copy chord that is
 * also the interrupt (Ctrl+C): copy, then drop the selection so the next press
 * is the interrupt again. The other copy chords (Ctrl+Shift+C, Ctrl+Insert)
 * leave it in place.
 */
function copySelectionToClipboard(
  selection: CopySelectionResult,
  clearPaneSelection: boolean,
): void {
  void navigator.clipboard
    .writeText(selection.text)
    .then(() => {
      writeSystem(
        "info",
        selection.kind === "dom"
          ? "DOM selection copied"
          : selection.kind === "system"
            ? "System log selection copied"
            : `${paneLabel(selection.session)} selection copied`,
      );
      if (clearPaneSelection) {
        if (selection.kind === "session") {
          paneMap.get(selection.session)?.terminal.clearSelection();
        } else if (selection.kind === "system") {
          systemTerminal.clearSelection();
        }
      }
    })
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
}

function shortcutInput(event: KeyboardEvent) {
  return {
    key: event.key,
    code: event.code,
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

async function resizeSession(
  sessionId: string, cols: number, rows: number, expectedRunId: string, expectedGeneration: number,
): Promise<boolean> {
  try {
    await command<void>("resize_session", { sessionId, cols, rows, expectedRunId, expectedGeneration });
    return true;
  } catch {
    // Includes a stale native run identity; callers retain confirmed-grid recovery.
    return false;
  }
}

/** Every confirmation gate: the native OK/Cancel dialog (`confirm_action`).
    `window.confirm` is replaced by the dialog plugin with an async invoke
    that is always truthy, so it must never gate an action. A failed dialog
    is a no and is logged; nothing is done before an explicit yes. */
async function confirmAction(message: string, ok: string): Promise<boolean> {
  return afterConfirm(
    () => command<boolean>("confirm_action", { message, okLabel: ok }).catch((error: unknown) => {
      writeSystem("error", `The confirmation could not be shown, so nothing was done: ${errorText(error)}`);
      return false;
    }),
    async () => {},
  );
}

async function command<T>(
  name: string,
  payload?: Record<string, unknown>,
): Promise<T> {
  if (!desktopCommandAllowed(trialStatus, name)) {
    throw new Error(trialStatus === undefined ? "Startup admission is not established" : TRIAL_HOLD_TEXT);
  }
  return invoke<T>(name, payload);
}

function must<T extends Element>(selector: string): T {
  const element = document.querySelector(selector);
  if (!element) {
    throw new Error(`Missing required element: ${selector}`);
  }
  return element as T;
}
