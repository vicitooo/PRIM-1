/** Telemetry (PLAN E7): formatting and the polling rule for the top-bar line
    and the Telemetry page. The supervisor measures; this module presents.
    Every process is in exactly one group, so the rows add up to the total. */

import { driverLabel } from "./session-tabs";
import type { TelemetrySnapshot, TelemetryUsage } from "./types";

/** Poll cadence while the line or the page is on screen. */
export const TELEMETRY_POLL_MS = 2000;

const BYTE_UNITS = ["B", "KB", "MB", "GB", "TB"] as const;

/** Binary units as Task Manager writes them: "0 B", "6.6 MB", "245 MB",
    "1.2 GB". One decimal below 10, whole numbers above, at most three
    digits before the unit. */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) {
    return "0 B";
  }
  let value = bytes;
  let unit = 0;
  while (value >= 999.5 && unit < BYTE_UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  if (unit === 0) {
    return `${Math.round(value)} B`;
  }
  const text = value < 9.95 ? value.toFixed(1) : String(Math.round(value));
  return `${text} ${BYTE_UNITS[unit]}`;
}

/** A share of the whole PC: "—" before the first measurement, "0%" when
    idle, one decimal below 10 % ("7.2%"), whole percent above ("23%"). */
export function formatPercent(percent: number | null): string {
  if (percent === null || !Number.isFinite(percent)) {
    return "—";
  }
  const clamped = Math.min(Math.max(percent, 0), 100);
  if (clamped < 0.05) {
    return "0%";
  }
  return clamped < 9.95 ? `${clamped.toFixed(1)}%` : `${Math.round(clamped)}%`;
}

/** PRIM-1's own footprint: its process, its WebView2 and its console hosts. */
export function appPrivateBytes(snapshot: TelemetrySnapshot): number {
  return (
    snapshot.core.private_bytes
    + snapshot.webview.private_bytes
    + snapshot.helpers.private_bytes
  );
}

/** Every live agent's whole process tree. */
export function agentsPrivateBytes(snapshot: TelemetrySnapshot): number {
  return snapshot.sessions.reduce((sum, session) => sum + session.private_bytes, 0);
}

export interface TelemetryLinePart {
  label: string;
  value: string;
}

/** The top-bar line's three figures; dashes before the first sample. A
    narrow bar drops the labels and keeps the figures. */
export function telemetryLineParts(snapshot: TelemetrySnapshot | null): TelemetryLinePart[] {
  return [
    { label: "PRIM-1", value: snapshot ? formatBytes(appPrivateBytes(snapshot)) : "—" },
    { label: "agents", value: snapshot ? formatBytes(agentsPrivateBytes(snapshot)) : "—" },
    { label: "CPU", value: snapshot ? formatPercent(snapshot.total.cpu_percent) : "—" },
  ];
}

/** The top-bar line in full: "PRIM-1 245 MB · agents 1.2 GB · CPU 7.2%". */
export function telemetryLine(snapshot: TelemetrySnapshot | null): string {
  return telemetryLineParts(snapshot)
    .map((part) => `${part.label} ${part.value}`)
    .join(" · ");
}

export const TELEMETRY_LINE_TITLE =
  "PRIM-1's own processes · every agent's process tree · CPU share of this PC. Click for the breakdown.";

/** Some reads failed; run Job CPU can still include those processes. */
function unreadableNote(count: number): string | null {
  if (count <= 0) {
    return null;
  }
  const processes = count === 1 ? "1 process" : `${count} processes`;
  return `${processes} could not be fully read; memory and process counts may be incomplete`;
}

/** The line's tooltip, warning when some processes could not be read. */
export function telemetryLineTitle(snapshot: TelemetrySnapshot | null): string {
  const note = snapshot ? unreadableNote(snapshot.unreadable_processes) : null;
  return note ? `${TELEMETRY_LINE_TITLE} ${note}.` : TELEMETRY_LINE_TITLE;
}

/** The page's footer, `time` formatting the sample's timestamp. */
export function telemetryFootnote(
  snapshot: TelemetrySnapshot,
  time: (isoTimestamp: string) => string,
): string {
  const parts = [
    `Sampled ${time(snapshot.sampled_at)}`,
    `${snapshot.logical_cpus} logical CPUs`,
    `this sample took ${snapshot.sampler_cost_ms.toFixed(1)} ms`,
  ];
  const note = unreadableNote(snapshot.unreadable_processes);
  if (note) {
    parts.push(note);
  }
  return parts.join(" · ");
}

/** One table row of the Telemetry page, every cell already formatted.
    Peaks are tracked per agent run only; PRIM-1's own rows leave them blank. */
export interface TelemetryRow {
  kind: "app" | "agent" | "total";
  name: string;
  harness: string;
  cpuNow: string;
  cpuPeak: string;
  memoryNow: string;
  memoryPeak: string;
  processes: string;
}

function usageRow(
  kind: TelemetryRow["kind"],
  name: string,
  usage: TelemetryUsage,
): TelemetryRow {
  return {
    kind,
    name,
    harness: "",
    cpuNow: formatPercent(usage.cpu_percent),
    cpuPeak: "",
    memoryNow: formatBytes(usage.private_bytes),
    memoryPeak: "",
    processes: String(usage.processes),
  };
}

/** The webview row. macOS: WebKit's processes are launchd's children and
    are not attributed to PRIM-1, so the row says not measured rather than 0. */
function webviewRow(snapshot: TelemetrySnapshot): TelemetryRow {
  if (snapshot.webview.processes > 0) {
    return usageRow("app", "WebView2 (UI)", snapshot.webview);
  }
  return {
    kind: "app",
    name: "Webview (UI), not measured on this platform",
    harness: "",
    cpuNow: "—",
    cpuPeak: "",
    memoryNow: "—",
    memoryPeak: "",
    processes: "—",
  };
}

/** PRIM-1's three groups, then one row per live agent run. */
export function telemetryRows(snapshot: TelemetrySnapshot): TelemetryRow[] {
  const helpers = snapshot.helpers.processes > 0 ? [usageRow("app", "Console hosts & helpers", snapshot.helpers)] : [];
  return [
    usageRow("app", "PRIM-1 core", snapshot.core),
    webviewRow(snapshot),
    ...helpers,
    ...snapshot.sessions.map((session): TelemetryRow => ({
      kind: "agent",
      name: session.label,
      harness: driverLabel(session.driver),
      cpuNow: formatPercent(session.cpu_percent),
      cpuPeak: formatPercent(session.peak_cpu_percent),
      memoryNow: formatBytes(session.private_bytes),
      memoryPeak: formatBytes(session.peak_private_bytes),
      processes: String(session.processes),
    })),
  ];
}

export function telemetryTotalRow(snapshot: TelemetrySnapshot): TelemetryRow {
  return usageRow("total", "Total", snapshot.total);
}

export interface TelemetryVisibility {
  documentVisible: boolean;
  windowMinimized: boolean;
  /** The top-bar line is laid out; narrow windows hide it. */
  lineShown: boolean;
  pageOpen: boolean;
}

/** Sample only while a number is on screen: nothing polls when hidden. */
export function shouldPollTelemetry(visibility: TelemetryVisibility): boolean {
  return (
    visibility.documentVisible
    && !visibility.windowMinimized
    && (visibility.lineShown || visibility.pageOpen)
  );
}

/** Milliseconds until the next poll keeps at least one full interval since
    the last one, so reopening the page or the window never samples early. */
export function telemetryPollDelay(lastPollAt: number | null, now: number): number {
  if (lastPollAt === null) {
    return 0;
  }
  return Math.min(TELEMETRY_POLL_MS, Math.max(0, lastPollAt + TELEMETRY_POLL_MS - now));
}

/** What the poller needs from the page, injected so that its scheduling can
    be tested without a window. */
export interface TelemetryPollerHost {
  documentVisible(): boolean;
  /** The top-bar line is laid out. */
  lineShown(): boolean;
  pageOpen(): boolean;
  /** Asks the window whether it is minimized; may reject. */
  isMinimized(): Promise<boolean>;
  sample(): Promise<TelemetrySnapshot>;
  onSample(snapshot: TelemetrySnapshot): void;
  onError(error: unknown): void;
  now(): number;
  setTimer(callback: () => void, delayMs: number): number;
  clearTimer(timer: number): void;
}

/** One poll every 2 s while the line or the page is on screen; none while the
    document is hidden, the window is minimized, or its state is unknown.
    WebView2 stays "visible" while its window is minimized, so the window is
    asked. Polls never overlap: the supervisor measures CPU against the
    previous sample. */
export class TelemetryPoller {
  private readonly host: TelemetryPollerHost;
  /** Unknown until the window answers, and again after a failed query:
      either way nothing is sampled. */
  private minimized: boolean | "unknown" = "unknown";
  private windowQuery = 0;
  private timer: number | null = null;
  private inFlight = false;
  private lastPollAt: number | null = null;

  constructor(host: TelemetryPollerHost) {
    this.host = host;
  }

  /** Re-reads the minimized state. Only the newest query may set it, so an
      older answer arriving late can never undo a newer one. */
  async refreshWindowState(): Promise<void> {
    const query = ++this.windowQuery;
    // A resize can mean minimization. The previous answer is no longer
    // sufficient while the newest query is pending, even if it never settles.
    this.minimized = "unknown";
    this.sync();
    let minimized: boolean | "unknown";
    try {
      minimized = await this.host.isMinimized();
    } catch {
      minimized = "unknown";
    }
    if (query !== this.windowQuery) {
      return;
    }
    this.minimized = minimized;
    this.sync();
  }

  /** Starts or stops polling to match what is on screen. */
  sync(): void {
    if (!this.onScreen()) {
      if (this.timer !== null) {
        this.host.clearTimer(this.timer);
        this.timer = null;
      }
      return;
    }
    if (this.timer !== null || this.inFlight) {
      return;
    }
    this.timer = this.host.setTimer(
      () => void this.poll(),
      telemetryPollDelay(this.lastPollAt, this.host.now()),
    );
  }

  private onScreen(): boolean {
    return shouldPollTelemetry({
      documentVisible: this.host.documentVisible(),
      windowMinimized: this.minimized !== false,
      lineShown: this.host.lineShown(),
      pageOpen: this.host.pageOpen(),
    });
  }

  private async poll(): Promise<void> {
    this.timer = null;
    if (!this.onScreen()) {
      return;
    }
    this.inFlight = true;
    this.lastPollAt = this.host.now();
    try {
      this.host.onSample(await this.host.sample());
    } catch (error) {
      this.host.onError(error);
    } finally {
      this.inFlight = false;
    }
    this.sync();
  }
}
