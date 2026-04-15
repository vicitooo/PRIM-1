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

const app = document.querySelector("#app");
if (!(app instanceof HTMLDivElement)) {
  throw new Error("Missing #app root element");
}

app.innerHTML = `
  <div class="app-shell">
    <header class="topbar">
      <div>
        <p class="eyebrow">Victor / Claude / Codex</p>
        <h1>CLI Master Wrapper</h1>
      </div>
      <div class="meta-strip">
        <div class="meta-card">
          <span class="meta-label">Control plane</span>
          <strong id="control-endpoint">starting...</strong>
        </div>
        <div class="meta-card">
          <span class="meta-label">Audit log</span>
          <strong id="audit-path">loading...</strong>
        </div>
      </div>
    </header>

    <section class="workspace-grid">
      <article class="terminal-card" data-session-card="claude">
        <div class="card-head">
          <div>
            <p class="card-kicker">Agent pane</p>
            <h2>Claude</h2>
          </div>
          <div class="session-meta">
            <span class="state-pill" data-session-state="claude">closed</span>
            <span class="activity-pill" data-session-activity="claude">idle</span>
          </div>
        </div>
        <div class="terminal-actions">
          <button data-action="start" data-session="claude">Launch</button>
          <button data-action="restart" data-session="claude">Restart</button>
          <button data-action="stop" data-session="claude">Stop</button>
        </div>
        <div class="terminal-host" data-terminal="claude"></div>
      </article>

      <article class="terminal-card" data-session-card="codex">
        <div class="card-head">
          <div>
            <p class="card-kicker">Agent pane</p>
            <h2>Codex</h2>
          </div>
          <div class="session-meta">
            <span class="state-pill" data-session-state="codex">closed</span>
            <span class="activity-pill" data-session-activity="codex">idle</span>
          </div>
        </div>
        <div class="terminal-actions">
          <button data-action="start" data-session="codex">Launch</button>
          <button data-action="restart" data-session="codex">Restart</button>
          <button data-action="stop" data-session="codex">Stop</button>
        </div>
        <div class="terminal-host" data-terminal="codex"></div>
      </article>
    </section>

    <section class="bottom-grid">
      <article class="system-card">
        <div class="card-head">
          <div>
            <p class="card-kicker">Supervisor</p>
            <h2>System log</h2>
          </div>
          <span class="mono" id="runtime-path">runtime pending</span>
        </div>
        <div class="system-terminal" id="system-terminal"></div>
      </article>

      <article class="router-card">
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

const SESSION_NAMES = ["claude", "codex"] as const;
const SESSION_LABELS: Record<(typeof SESSION_NAMES)[number], string> = {
  claude: "Claude",
  codex: "Codex",
};

class SessionTerminal {
  readonly name: (typeof SESSION_NAMES)[number];
  readonly terminal: Terminal;
  readonly fitAddon: FitAddon;
  readonly host: HTMLDivElement;
  readonly stateEl: HTMLSpanElement;
  readonly activityEl: HTMLSpanElement;
  private hooked = false;
  private snapshot: SessionSnapshot | null = null;

  constructor(name: (typeof SESSION_NAMES)[number]) {
    this.name = name;
    this.host = must<HTMLDivElement>(`[data-terminal="${name}"]`);
    this.stateEl = must<HTMLSpanElement>(`[data-session-state="${name}"]`);
    this.activityEl = must<HTMLSpanElement>(`[data-session-activity="${name}"]`);
    this.terminal = new Terminal({
      convertEol: true,
      cursorBlink: true,
      fontFamily: '"JetBrains Mono", "Cascadia Code", Consolas, monospace',
      fontSize: 13,
      theme: {
        background: "#12100d",
        foreground: "#f4ede5",
        cursor: "#f4ede5",
        selectionBackground: "#7e5c351f",
        black: "#12100d",
        brightBlack: "#5a524a",
        red: "#f07f5a",
        brightRed: "#ff9c74",
        green: "#97c35f",
        brightGreen: "#b0d970",
        yellow: "#e1b866",
        brightYellow: "#f4cc7d",
        blue: "#7aa2f7",
        brightBlue: "#93b5ff",
        magenta: "#d28fe8",
        brightMagenta: "#e8a9ff",
        cyan: "#59c0c8",
        brightCyan: "#7ee1e7",
        white: "#d8d1c7",
        brightWhite: "#fff9f2",
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
    this.terminal.writeln(`\x1b[38;5;179m${SESSION_LABELS[this.name]} pane ready.\x1b[0m`);
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
      }).catch((error) => writeSystem("error", `${SESSION_LABELS[this.name]} input failed: ${error}`));
    });
  }

  applySnapshot(snapshot: SessionSnapshot): void {
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

    this.fitAddon.fit();
    this.hookInput();
    void resizeSession(this.name, this.terminal.cols, this.terminal.rows);
  }

  write(chunk: string): void {
    this.terminal.write(chunk);
  }

  fit(): void {
    this.fitAddon.fit();
    void resizeSession(this.name, this.terminal.cols, this.terminal.rows);
  }
}

const paneMap = new Map<(typeof SESSION_NAMES)[number], SessionTerminal>();
for (const sessionName of SESSION_NAMES) {
  paneMap.set(sessionName, new SessionTerminal(sessionName));
}

const systemTerminal = new Terminal({
  convertEol: true,
  disableStdin: true,
  fontFamily: '"JetBrains Mono", "Cascadia Code", Consolas, monospace',
  fontSize: 12,
  theme: {
    background: "#1b1814",
    foreground: "#e9e0d5",
    cursor: "#e9e0d5",
    black: "#1b1814",
    brightBlack: "#5f574d",
    red: "#ff8b68",
    brightRed: "#ff9e80",
    green: "#9fcd71",
    brightGreen: "#b7e38b",
    yellow: "#edc77f",
    brightYellow: "#f5d596",
    blue: "#88aefc",
    brightBlue: "#9fc0ff",
    magenta: "#d9a7e8",
    brightMagenta: "#ebbcff",
    cyan: "#73d1d6",
    brightCyan: "#8de7ec",
    white: "#e6ded3",
    brightWhite: "#fffaf4",
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

const snapshotByName = new Map<string, SessionSnapshot>();
const controlEndpoint = must<HTMLElement>("#control-endpoint");
const auditPath = must<HTMLElement>("#audit-path");
const runtimePath = must<HTMLElement>("#runtime-path");
let activeTerminalName: (typeof SESSION_NAMES)[number] | null = null;
let activeCopySurface: CopySurface = null;

wireButtons();
wireRouter();
wireControls();
wireResize();
wireTerminalShortcuts();

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
  controlEndpoint.textContent = snapshot.control_plane?.endpoint ?? "starting...";
  auditPath.textContent = snapshot.audit_log_path;
  runtimePath.textContent = snapshot.runtime_dir;

  for (const session of snapshot.sessions) {
    snapshotByName.set(session.name, session);
    if (session.name === "claude" || session.name === "codex") {
      paneMap.get(session.name)?.applySnapshot(session);
    }
  }
}

function handleRuntimeEvent(event: RuntimeEvent): void {
  switch (event.event) {
    case "session_output":
      paneMap.get(event.session as (typeof SESSION_NAMES)[number])?.write(event.chunk);
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
        if (event.session === "claude" || event.session === "codex") {
          paneMap.get(event.session)?.applySnapshot(next);
        }
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
          if (snapshot.name === "claude" || snapshot.name === "codex") {
            paneMap.get(snapshot.name)?.applySnapshot(snapshot);
          }
        }

        if (action === "restart") {
          const snapshot = await command<SessionSnapshot>("restart_session", {
            request: { name: session },
          });
          snapshotByName.set(snapshot.name, snapshot);
          if (snapshot.name === "claude" || snapshot.name === "codex") {
            paneMap.get(snapshot.name)?.applySnapshot(snapshot);
          }
        }

        if (action === "stop") {
          const snapshot = await command<SessionSnapshot>("stop_session", {
            request: { name: session },
          });
          snapshotByName.set(snapshot.name, snapshot);
          if (snapshot.name === "claude" || snapshot.name === "codex") {
            paneMap.get(snapshot.name)?.applySnapshot(snapshot);
          }
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
          writeSystem("warn", "main menu is reserved for the next UI slice.");
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
    for (const pane of paneMap.values()) {
      pane.fit();
    }
    systemFit.fit();
  });

  observer.observe(document.body);
  window.addEventListener("resize", () => {
    for (const pane of paneMap.values()) {
      pane.fit();
    }
    systemFit.fit();
  });
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
        terminalSelections: {
          claude: paneMap.get("claude")?.terminal.getSelection() ?? null,
          codex: paneMap.get("codex")?.terminal.getSelection() ?? null,
        },
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
              : `${SESSION_LABELS[selection.session]} selection copied`,
          ),
        )
        .catch((error) =>
          writeSystem(
            "error",
            selection.kind === "dom"
              ? `copy failed for DOM selection: ${String(error)}`
              : selection.kind === "system"
                ? `copy failed for System log: ${String(error)}`
              : `copy failed for ${SESSION_LABELS[selection.session]}: ${String(error)}`,
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
    writeSystem("warn", `${SESSION_LABELS[pane.name]} is not running; paste skipped`);
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
    writeSystem("info", `${SESSION_LABELS[pane.name]} pasted ${clipboard.length} chars`);
  } catch (error) {
    writeSystem("error", `paste failed for ${SESSION_LABELS[pane.name]}: ${String(error)}`);
  }
}

function activeTerminal(): SessionTerminal | null {
  return activeTerminalName ? paneMap.get(activeTerminalName) ?? null : null;
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
