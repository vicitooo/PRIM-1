import { describe, expect, it } from "vitest";

import {
  TELEMETRY_LINE_TITLE,
  TELEMETRY_POLL_MS,
  TelemetryPoller,
  agentsPrivateBytes,
  appPrivateBytes,
  formatBytes,
  formatPercent,
  shouldPollTelemetry,
  telemetryFootnote,
  telemetryLine,
  telemetryLineParts,
  telemetryLineTitle,
  telemetryPollDelay,
  telemetryRows,
  telemetryTotalRow,
  type TelemetryPollerHost,
} from "./telemetry";
import type { TelemetrySnapshot } from "./types";

const MB = 1024 * 1024;
const GB = 1024 * MB;

function snapshot(): TelemetrySnapshot {
  return {
    sampled_at: "2026-10-14T00:00:00Z",
    logical_cpus: 12,
    core: { cpu_percent: 0.4, private_bytes: 8 * MB, processes: 1 },
    webview: { cpu_percent: 4.1, private_bytes: 221 * MB, processes: 6 },
    helpers: { cpu_percent: 0, private_bytes: 16 * MB, processes: 7 },
    sessions: [
      {
        session_id: "11111111-1111-1111-1111-111111111111",
        label: "Reviewer",
        driver: "codex",
        cpu_percent: 2.5,
        private_bytes: 716 * MB,
        processes: 9,
        peak_cpu_percent: 31.2,
        peak_private_bytes: 1.1 * GB,
      },
      {
        session_id: "22222222-2222-2222-2222-222222222222",
        label: "Builder",
        driver: "claude",
        cpu_percent: null,
        private_bytes: 512 * MB,
        processes: 4,
        peak_cpu_percent: null,
        peak_private_bytes: 512 * MB,
      },
    ],
    total: { cpu_percent: 7.04, private_bytes: 1473 * MB, processes: 27 },
    unreadable_processes: 0,
    sampler_cost_ms: 3.2,
  };
}

describe("formatBytes", () => {
  it("writes binary units with at most three digits", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(6.6 * MB)).toBe("6.6 MB");
    expect(formatBytes(245 * MB)).toBe("245 MB");
    expect(formatBytes(1.2 * GB)).toBe("1.2 GB");
    expect(formatBytes(12.4 * GB)).toBe("12 GB");
  });

  it("moves to the next unit instead of printing four digits", () => {
    expect(formatBytes(999 * MB)).toBe("999 MB");
    expect(formatBytes(1000 * MB)).toBe("1.0 GB");
    expect(formatBytes(1023)).toBe("1.0 KB");
  });

  it("rounds 9.95 up to a whole number, not to 10.0", () => {
    expect(formatBytes(9.94 * MB)).toBe("9.9 MB");
    expect(formatBytes(9.96 * MB)).toBe("10 MB");
  });

  it("reads anything unusable as nothing", () => {
    expect(formatBytes(-1)).toBe("0 B");
    expect(formatBytes(Number.NaN)).toBe("0 B");
  });
});

describe("formatPercent", () => {
  it("shows a dash until there is a measurement", () => {
    expect(formatPercent(null)).toBe("—");
    expect(formatPercent(Number.NaN)).toBe("—");
  });

  it("uses one decimal below ten and whole percent above", () => {
    expect(formatPercent(0)).toBe("0%");
    expect(formatPercent(0.04)).toBe("0%");
    expect(formatPercent(0.1)).toBe("0.1%");
    expect(formatPercent(7.04)).toBe("7.0%");
    expect(formatPercent(9.96)).toBe("10%");
    expect(formatPercent(23.4)).toBe("23%");
  });

  it("never leaves 0 to 100", () => {
    expect(formatPercent(-3)).toBe("0%");
    expect(formatPercent(140)).toBe("100%");
  });
});

describe("telemetry line", () => {
  it("splits PRIM-1's own footprint from the agents' and shows total CPU", () => {
    const sample = snapshot();
    expect(appPrivateBytes(sample)).toBe(245 * MB);
    expect(agentsPrivateBytes(sample)).toBe(1228 * MB);
    expect(telemetryLine(sample)).toBe("PRIM-1 245 MB · agents 1.2 GB · CPU 7.0%");
  });

  it("shows a dash for CPU on the first sample", () => {
    const sample = snapshot();
    sample.total.cpu_percent = null;
    sample.sessions = [];
    expect(telemetryLine(sample)).toBe("PRIM-1 245 MB · agents 0 B · CPU —");
  });

  it("shows dashes before any sample and keeps labels apart from figures", () => {
    expect(telemetryLine(null)).toBe("PRIM-1 — · agents — · CPU —");
    expect(telemetryLineParts(snapshot())).toEqual([
      { label: "PRIM-1", value: "245 MB" },
      { label: "agents", value: "1.2 GB" },
      { label: "CPU", value: "7.0%" },
    ]);
  });
});

describe("telemetry rows", () => {
  it("lists PRIM-1's groups first, then every live agent with its peaks", () => {
    expect(telemetryRows(snapshot())).toEqual([
      {
        kind: "app",
        name: "PRIM-1 core",
        harness: "",
        cpuNow: "0.4%",
        cpuPeak: "",
        memoryNow: "8.0 MB",
        memoryPeak: "",
        processes: "1",
      },
      {
        kind: "app",
        name: "WebView2 (UI)",
        harness: "",
        cpuNow: "4.1%",
        cpuPeak: "",
        memoryNow: "221 MB",
        memoryPeak: "",
        processes: "6",
      },
      {
        kind: "app",
        name: "Console hosts & helpers",
        harness: "",
        cpuNow: "0%",
        cpuPeak: "",
        memoryNow: "16 MB",
        memoryPeak: "",
        processes: "7",
      },
      {
        kind: "agent",
        name: "Reviewer",
        harness: "Codex",
        cpuNow: "2.5%",
        cpuPeak: "31%",
        memoryNow: "716 MB",
        memoryPeak: "1.1 GB",
        processes: "9",
      },
      {
        kind: "agent",
        name: "Builder",
        harness: "Claude Code",
        cpuNow: "—",
        cpuPeak: "—",
        memoryNow: "512 MB",
        memoryPeak: "512 MB",
        processes: "4",
      },
    ]);
    expect(telemetryTotalRow(snapshot())).toEqual({
      kind: "total",
      name: "Total",
      harness: "",
      cpuNow: "7.0%",
      cpuPeak: "",
      memoryNow: "1.4 GB",
      memoryPeak: "",
      processes: "27",
    });
  });
});

describe("telemetry polling", () => {
  const visible = {
    documentVisible: true,
    windowMinimized: false,
    lineShown: true,
    pageOpen: false,
  };

  it("polls only while the line or the page is on screen", () => {
    expect(shouldPollTelemetry(visible)).toBe(true);
    expect(shouldPollTelemetry({ ...visible, lineShown: false, pageOpen: true })).toBe(true);
    expect(shouldPollTelemetry({ ...visible, lineShown: false })).toBe(false);
    expect(shouldPollTelemetry({ ...visible, documentVisible: false, pageOpen: true })).toBe(false);
    expect(shouldPollTelemetry({ ...visible, windowMinimized: true, pageOpen: true })).toBe(false);
  });

  it("never polls sooner than one interval after the last poll", () => {
    expect(telemetryPollDelay(null, 5_000)).toBe(0);
    expect(telemetryPollDelay(5_000, 5_300)).toBe(TELEMETRY_POLL_MS - 300);
    expect(telemetryPollDelay(5_000, 9_000)).toBe(0);
    // A clock that went backwards still waits at most one interval.
    expect(telemetryPollDelay(9_000, 5_000)).toBe(TELEMETRY_POLL_MS);
  });
});

describe("telemetry notes", () => {
  it("says when partial readings leave memory and counts incomplete", () => {
    const time = () => "02:41:07";
    expect(telemetryFootnote(snapshot(), time)).toBe(
      "Sampled 02:41:07 · 12 logical CPUs · this sample took 3.2 ms",
    );
    expect(telemetryLineTitle(snapshot())).toBe(TELEMETRY_LINE_TITLE);
    expect(telemetryLineTitle(null)).toBe(TELEMETRY_LINE_TITLE);

    const partial = { ...snapshot(), unreadable_processes: 2 };
    expect(telemetryFootnote(partial, time)).toBe(
      "Sampled 02:41:07 · 12 logical CPUs · this sample took 3.2 ms"
        + " · 2 processes could not be fully read; memory and process counts may be incomplete",
    );
    expect(telemetryLineTitle({ ...partial, unreadable_processes: 1 })).toBe(
      `${TELEMETRY_LINE_TITLE} 1 process could not be fully read; memory and process counts may be incomplete.`,
    );
  });
});

interface Deferred<T> {
  resolve(value: T): void;
  reject(error: unknown): void;
}

/** A window, a clock and a supervisor the test drives by hand. */
class FakeHost implements TelemetryPollerHost {
  visible = true;
  line = true;
  page = false;
  time = 0;
  private nextTimer = 1;
  readonly timers = new Map<number, { callback: () => void; at: number }>();
  readonly windowQueries: Deferred<boolean>[] = [];
  readonly samples: Deferred<TelemetrySnapshot>[] = [];
  readonly received: TelemetrySnapshot[] = [];
  readonly errors: unknown[] = [];

  documentVisible(): boolean {
    return this.visible;
  }
  lineShown(): boolean {
    return this.line;
  }
  pageOpen(): boolean {
    return this.page;
  }
  isMinimized(): Promise<boolean> {
    return new Promise((resolve, reject) => this.windowQueries.push({ resolve, reject }));
  }
  sample(): Promise<TelemetrySnapshot> {
    return new Promise((resolve, reject) => this.samples.push({ resolve, reject }));
  }
  onSample(sample: TelemetrySnapshot): void {
    this.received.push(sample);
  }
  onError(error: unknown): void {
    this.errors.push(error);
  }
  now(): number {
    return this.time;
  }
  setTimer(callback: () => void, delayMs: number): number {
    const timer = this.nextTimer++;
    this.timers.set(timer, { callback, at: this.time + delayMs });
    return timer;
  }
  clearTimer(timer: number): void {
    this.timers.delete(timer);
  }
  /** When the one pending poll is due, or null. */
  dueAt(): number | null {
    const [only, ...rest] = [...this.timers.values()];
    expect(rest).toEqual([]);
    return only ? only.at : null;
  }
  /** Moves the clock and runs every timer that falls due. */
  advance(ms: number): void {
    this.time += ms;
    for (const [timer, entry] of [...this.timers]) {
      if (entry.at <= this.time) {
        this.timers.delete(timer);
        entry.callback();
      }
    }
  }
}

async function settle(): Promise<void> {
  for (let turn = 0; turn < 10; turn += 1) {
    await Promise.resolve();
  }
}

/** A poller whose window has answered "not minimized" and whose first poll
    has been answered at time 0. */
async function pollingAtZero(host: FakeHost): Promise<TelemetryPoller> {
  const poller = new TelemetryPoller(host);
  const ready = poller.refreshWindowState();
  host.windowQueries[0].resolve(false);
  await ready;
  expect(host.dueAt()).toBe(0);
  host.advance(0);
  expect(host.samples).toHaveLength(1);
  host.samples[0].resolve(snapshot());
  await settle();
  expect(host.received).toHaveLength(1);
  return poller;
}

describe("telemetry poller", () => {
  it("samples nothing until the window says it is not minimized", async () => {
    const host = new FakeHost();
    const poller = new TelemetryPoller(host);
    poller.sync();
    expect(host.dueAt()).toBeNull();

    const answered = poller.refreshWindowState();
    host.windowQueries[0].resolve(false);
    await answered;
    expect(host.dueAt()).toBe(0);
  });

  it("keeps sampling paused when the minimized query fails", async () => {
    const host = new FakeHost();
    const poller = await pollingAtZero(host);
    expect(host.dueAt()).toBe(TELEMETRY_POLL_MS);

    const failed = poller.refreshWindowState();
    host.windowQueries[1].reject(new Error("window state unavailable"));
    await failed;
    expect(host.dueAt()).toBeNull();
    host.advance(10 * TELEMETRY_POLL_MS);
    expect(host.samples).toHaveLength(1);
  });

  it("cancels polling while the newest window query remains pending", async () => {
    const host = new FakeHost();
    const poller = await pollingAtZero(host);
    expect(host.dueAt()).toBe(TELEMETRY_POLL_MS);

    const older = poller.refreshWindowState();
    const newer = poller.refreshWindowState();
    expect(host.dueAt()).toBeNull();
    host.windowQueries[1].resolve(false);
    await older;
    poller.sync();
    host.advance(10 * TELEMETRY_POLL_MS);
    expect(host.dueAt()).toBeNull();
    expect(host.samples).toHaveLength(1);

    host.windowQueries[2].resolve(false);
    await newer;
    expect(host.dueAt()).toBe(host.time);
    host.advance(0);
    expect(host.samples).toHaveLength(2);
    host.samples[1].resolve(snapshot());
    await settle();
  });

  it("does not rearm after an in-flight sample while the window query is pending", async () => {
    const host = new FakeHost();
    const poller = await pollingAtZero(host);
    host.advance(TELEMETRY_POLL_MS);
    expect(host.samples).toHaveLength(2);
    const minimized = poller.refreshWindowState();
    host.samples[1].resolve(snapshot());
    await settle();
    host.advance(10 * TELEMETRY_POLL_MS);
    expect(host.dueAt()).toBeNull();
    expect(host.samples).toHaveLength(2);
    host.windowQueries[1].resolve(true);
    await minimized;
    expect(host.dueAt()).toBeNull();
  });

  it("lets only the newest minimized answer count", async () => {
    const host = new FakeHost();
    const poller = await pollingAtZero(host);

    // The window is minimized, then the older "visible" answer arrives late.
    const older = poller.refreshWindowState();
    const newer = poller.refreshWindowState();
    host.windowQueries[2].resolve(true);
    await newer;
    expect(host.dueAt()).toBeNull();
    host.windowQueries[1].resolve(false);
    await older;
    expect(host.dueAt()).toBeNull();
    host.advance(10 * TELEMETRY_POLL_MS);
    expect(host.samples).toHaveLength(1);
  });

  it("resumes after a restore no sooner than one interval after the last poll", async () => {
    const host = new FakeHost();
    const poller = await pollingAtZero(host);

    const minimized = poller.refreshWindowState();
    host.windowQueries[1].resolve(true);
    await minimized;
    expect(host.dueAt()).toBeNull();

    host.advance(500);
    const restored = poller.refreshWindowState();
    host.windowQueries[2].resolve(false);
    await restored;
    expect(host.dueAt()).toBe(TELEMETRY_POLL_MS);
  });

  it("never overlaps two samples", async () => {
    const host = new FakeHost();
    const poller = await pollingAtZero(host);
    host.advance(TELEMETRY_POLL_MS);
    expect(host.samples).toHaveLength(2);

    // The page opens while the second sample is still being measured.
    host.page = true;
    poller.sync();
    poller.sync();
    expect(host.dueAt()).toBeNull();

    host.samples[1].resolve(snapshot());
    await settle();
    expect(host.dueAt()).toBe(2 * TELEMETRY_POLL_MS);
  });

  it("stops when nothing is on screen and reports failed samples", async () => {
    const host = new FakeHost();
    const poller = await pollingAtZero(host);

    host.visible = false;
    poller.sync();
    expect(host.dueAt()).toBeNull();

    host.visible = true;
    host.line = false;
    poller.sync();
    expect(host.dueAt()).toBeNull();

    host.page = true;
    poller.sync();
    host.advance(TELEMETRY_POLL_MS);
    host.samples[1].reject(new Error("supervisor has shut down"));
    await settle();
    expect(host.errors).toHaveLength(1);
    expect(host.dueAt()).toBe(2 * TELEMETRY_POLL_MS);
  });
});
