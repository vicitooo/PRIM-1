/** Opt-in measurement probes for the S3 before/after fixture (Codex's
    performance program; PLAN D9 budgets). They switch on only in automation
    mode — the loopback debug port is set — and the same code ships in the
    S0-public-base and S3 builds, so both sides measure identically. Every
    series is bounded; results are read over CDP from window.__PRIM1_PERF__ and
    never sent anywhere.

    Measurement window: everything records only while armed. reset(), arm()
    and disarm() each start a new generation, and every queued callback checks
    the generation it was scheduled in, so nothing from one window lands in
    another (Codex reviews of 942ecfd and cdcd0ad).

    Input echo is proven from the rendered terminal, not from raw output. At a
    line's first keystroke the rendered text before the cursor — the prompt,
    already on screen — becomes the line's anchor. A keystroke counts as echoed
    only when the rendered text before the cursor equals that anchor plus the
    line typed so far. xterm's parser has already applied split escape
    sequences, and unrelated output would have to reproduce the prompt and the
    typed text exactly. This suits the fixture's line-editing shells; a TUI that
    draws its input elsewhere is not measured by it.

    v5 (Codex review of v4): any escape input other than a cursor-position
    reply (Esc, arrows, Alt+key) can edit or clear the line, so it restarts
    correlation instead of being skipped; v4 kept the old prediction after an
    Esc cleared the shell, so every later burst on that line expired. A switch
    keeps the FIRST shown frame of its target; v4 let a later refresh of the
    same pane overwrite it (a 16 ms show became 732 ms). Admitted, matched and
    pending counts are exposed so a run's coverage is visible.

    v6 binds each UI show callback to its switch, retains first-frame estimates
    before supersession, and counts parsed-but-unpainted input plus disarm
    losses. Resetting a window cannot credit its old frame callbacks. */

export const PERF_SAMPLE_CAP = 4096;
const PENDING_CAP = 256;
const LINE_CAP = 512;
const PENDING_MAX_AGE_MS = 10_000;
const SWITCH_RENDER_TIMEOUT_MS = 1_000;
/** One or more cursor-position replies (ESC[row;colR): the terminal answering
    the shell, never a key. */
const CPR_REPLIES = /^(?:\x1b\[\d+;\d+R)+$/;

export interface PerfStats {
  count: number;
  p50: number;
  p95: number;
  p99: number;
  max: number;
}

export function percentileStats(values: readonly number[]): PerfStats {
  if (values.length === 0) {
    return { count: 0, p50: 0, p95: 0, p99: 0, max: 0 };
  }
  const sorted = [...values].sort((left, right) => left - right);
  const pick = (quantile: number): number =>
    sorted[Math.min(sorted.length - 1, Math.max(0, Math.ceil(quantile * sorted.length) - 1))];
  return {
    count: sorted.length,
    p50: pick(0.5),
    p95: pick(0.95),
    p99: pick(0.99),
    max: sorted[sorted.length - 1],
  };
}

class BoundedSeries {
  private values: number[] = [];

  constructor(private readonly cap: number) {}

  push(value: number): void {
    this.values.push(value);
    if (this.values.length > this.cap) {
      this.values.shift();
    }
  }

  all(): number[] {
    return [...this.values];
  }

  clear(): void {
    this.values = [];
  }
}

export interface PerfSnapshot {
  version: 6;
  /** Admission (xterm onData) to the frame after the rendered cursor line
      first equals the line's prompt anchor plus the typed text. */
  input_echo_ms: PerfStats;
  /** Switch start to the target pane's paint: the frame after the target
      terminal's own render when it renders within 1 s (render-tied), else the
      frame after the pane was shown, for a pane whose content was already
      painted (frame estimate). The provenance counts say which is which. */
  switch_paint_ms: PerfStats;
  switch_provenance: { render_tied: number; frame_estimate: number };
  /** Coverage of the echo series: printable keys admitted while armed, keys
      credited with an echo, and keys still waiting for one. */
  input_counts: { admitted: number; matched: number; pending: number; awaiting_frame: number };
  frame_ms: PerfStats & { over_50ms: number };
  longtask_ms: PerfStats & { total: number };
  dropped: {
    input_unmatched: number;
    input_overflow: number;
    input_line_reset: number;
    input_line_cap: number;
    input_disarmed: number;
    switch_no_render: number;
    switch_cancelled: number;
    /** Later shows of an already-shown switch target, ignored (v5). */
    switch_repeat_show: number;
  };
  armed: boolean;
}

type Scheduler = (callback: () => void) => void;
type Timer = (callback: () => void, ms: number) => void;
type LineReader = () => string;

interface TypingState {
  anchor: string;
  line: string;
  pending: { expected: string; t0: number }[];
}

function emptyDropped(): PerfSnapshot["dropped"] {
  return {
    input_unmatched: 0,
    input_overflow: 0,
    input_line_reset: 0,
    input_line_cap: 0,
    input_disarmed: 0,
    switch_no_render: 0,
    switch_cancelled: 0,
    switch_repeat_show: 0,
  };
}

export class PerfProbes {
  private readonly inputEcho = new BoundedSeries(PERF_SAMPLE_CAP);
  private readonly switchPaint = new BoundedSeries(PERF_SAMPLE_CAP);
  private switchProvenance = { render_tied: 0, frame_estimate: 0 };
  /** The switch whose target was shown most recently, until it settles. */
  private shownSwitch: {
    generation: number;
    startedAt: number;
    shownAt: number | null;
    showRequested: boolean;
    renderSeen: boolean;
    finished: boolean;
    cleanup: () => void;
  } | null = null;
  private inputCounts = { admitted: 0, matched: 0 };
  private awaitingFrame = 0;
  private readonly frames = new BoundedSeries(PERF_SAMPLE_CAP * 4);
  private readonly longTasks = new BoundedSeries(PERF_SAMPLE_CAP);
  private readonly typing = new Map<string, TypingState>();
  private switchT0: number | null = null;
  private dropped = emptyDropped();
  private armed = false;
  private generation = 0;
  private observer: PerformanceObserver | null = null;

  constructor(
    private readonly now: () => number = () => performance.now(),
    private readonly nextFrame: Scheduler = (callback) => {
      requestAnimationFrame(() => callback());
    },
    private readonly setTimer: Timer = (callback, ms) => {
      setTimeout(callback, ms);
    },
  ) {}

  /** UI admission: the pane accepted these keystrokes for its session.
      `textBeforeCursor` reads the rendered cursor line up to the cursor.
      Escape sequences are skipped whole: xterm answers a shell's cursor
      position queries (ESC[r;cR) through the same path, and cursor keys never
      add a character. Skipping can only lose a match, never invent one. */
  inputAdmitted(sessionId: string, data: string, textBeforeCursor: LineReader): void {
    if (!this.armed) {
      return;
    }
    if (data.includes("\x1b")) {
      if (!CPR_REPLIES.test(data)) {
        this.restartTyping(sessionId);
      }
      return;
    }
    let state = this.typing.get(sessionId);
    const t0 = this.now();
    for (const char of data) {
      if (char < " " || char > "~") {
        // Enter or an editing key: the typed line no longer predicts the
        // rendered one; correlation restarts at the next line's first key.
        if (state) {
          this.dropped.input_line_reset += state.pending.length;
          this.typing.delete(sessionId);
          state = undefined;
        }
        continue;
      }
      this.inputCounts.admitted += 1;
      if (!state) {
        state = { anchor: textBeforeCursor(), line: "", pending: [] };
        this.typing.set(sessionId, state);
      }
      if (state.line.length >= LINE_CAP) {
        this.dropped.input_line_cap += state.pending.length + 1;
        // Keep the capped prediction until an editing key resets it. Starting
        // again from the old rendered anchor could credit a later key early.
        state.pending.length = 0;
        continue;
      }
      state.line += char;
      state.pending.push({ expected: state.anchor + state.line, t0 });
      while (state.pending.length > PENDING_CAP) {
        state.pending.shift();
        this.dropped.input_overflow += 1;
      }
    }
  }

  /** xterm finished parsing output for this session. */
  outputParsed(sessionId: string, textBeforeCursor: LineReader): void {
    if (!this.armed) {
      return;
    }
    const state = this.typing.get(sessionId);
    if (!state || state.pending.length === 0) {
      return;
    }
    const parsedAt = this.now();
    while (state.pending.length > 0 && parsedAt - state.pending[0].t0 > PENDING_MAX_AGE_MS) {
      state.pending.shift();
      this.dropped.input_unmatched += 1;
    }
    if (state.pending.length === 0) {
      return;
    }
    const text = textBeforeCursor();
    // Each expected line extends the previous one, so the latest exact match
    // covers every earlier keystroke too.
    let echoed = -1;
    for (let index = state.pending.length - 1; index >= 0; index -= 1) {
      if (text === state.pending[index].expected) {
        echoed = index;
        break;
      }
    }
    if (echoed < 0) {
      return;
    }
    const matched = state.pending.splice(0, echoed + 1).map((entry) => entry.t0);
    this.awaitingFrame += matched.length;
    const generation = this.generation;
    this.nextFrame(() => {
      if (generation !== this.generation) {
        return;
      }
      const visibleAt = this.now();
      this.awaitingFrame -= matched.length;
      for (const t0 of matched) {
        this.inputEcho.push(visibleAt - t0);
      }
      this.inputCounts.matched += matched.length;
    });
  }

  /** A room or tab switch began; the first call of one switch wins. */
  switchStarted(): void {
    if (this.armed && this.switchT0 === null) {
      this.switchT0 = this.now();
    }
  }

  /** The switch ended with no pane to show: nothing to time. */
  switchCancelled(preserveCurrentTarget = false): void {
    this.switchT0 = null;
    if (!preserveCurrentTarget) {
      this.cancelShownSwitch();
    }
  }

  /** The switch's target is known and not yet shown. `onTargetRender`
      subscribes once to the target terminal's own render, before it is
      unhidden so the first render cannot be missed; the paint is the frame
      after that render. */
  switchTarget(onTargetRender: (rendered: () => void) => void | (() => void)): (() => void) | null {
    const t0 = this.switchT0;
    if (t0 === null) {
      return null;
    }
    this.switchT0 = null;
    const generation = this.generation;
    this.cancelShownSwitch();
    const shown = {
      generation,
      startedAt: t0,
      shownAt: null as number | null,
      showRequested: false,
      renderSeen: false,
      finished: false,
      cleanup: () => {},
    };
    this.shownSwitch = shown;
    const current = () => this.armed && generation === this.generation
      && this.shownSwitch === shown && !shown.finished;
    const cleanup = onTargetRender(() => {
      if (!current() || shown.renderSeen) {
        return;
      }
      shown.renderSeen = true;
      this.nextFrame(() => {
        if (current()) {
          shown.finished = true;
          shown.cleanup();
          this.switchPaint.push(this.now() - t0);
          this.switchProvenance.render_tied += 1;
        }
      });
    });
    let remainingCleanup = cleanup;
    shown.cleanup = () => {
      const dispose = remainingCleanup;
      remainingCleanup = undefined;
      dispose?.();
    };
    if (shown.finished) shown.cleanup();
    this.setTimer(() => {
      if (!current() || shown.renderSeen) {
        return;
      }
      shown.finished = true;
      shown.cleanup();
      if (shown.shownAt !== null) {
        this.switchPaint.push(shown.shownAt - t0);
        this.switchProvenance.frame_estimate += 1;
      } else {
        this.dropped.switch_no_render += 1;
      }
    }, SWITCH_RENDER_TIMEOUT_MS);
    // This closure belongs only to this target; never look up a newer switch
    // when an older UI requestAnimationFrame finally runs.
    return () => {
      if (!current()) return;
      if (shown.showRequested) {
        this.dropped.switch_repeat_show += 1;
        return;
      }
      shown.showRequested = true;
      this.nextFrame(() => {
        if (current() && shown.shownAt === null) shown.shownAt = this.now();
      });
    };
  }

  private cancelShownSwitch(): void {
    const shown = this.shownSwitch;
    if (shown && !shown.finished) {
      shown.finished = true;
      shown.cleanup();
      if (shown.shownAt !== null && !shown.renderSeen) {
        // The first shown frame was already observed. Keep that estimate even
        // when the next switch comes before the no-render timeout.
        this.switchPaint.push(shown.shownAt - shown.startedAt);
        this.switchProvenance.frame_estimate += 1;
      } else {
        this.dropped.switch_cancelled += 1;
      }
    }
    this.shownSwitch = null;
  }

  /** Opens a measurement window. Frame and long-task recording run only while
      armed, so an idle app carries no probe cost. */
  arm(): void {
    if (this.armed) {
      return;
    }
    this.armed = true;
    this.startWindow();
  }

  disarm(): void {
    if (!this.armed) {
      return;
    }
    for (const state of this.typing.values()) {
      this.dropped.input_disarmed += state.pending.length;
    }
    this.dropped.input_disarmed += this.awaitingFrame;
    this.awaitingFrame = 0;
    this.cancelShownSwitch();
    this.armed = false;
    this.generation += 1;
    this.stopObserver();
    this.typing.clear();
    this.switchT0 = null;
  }

  /** Clears every series. While armed it also restarts the frame loop, so the
      first frame after a reset never spans it. */
  reset(): void {
    this.shownSwitch?.cleanup();
    this.inputEcho.clear();
    this.switchPaint.clear();
    this.switchProvenance = { render_tied: 0, frame_estimate: 0 };
    this.frames.clear();
    this.longTasks.clear();
    this.dropped = emptyDropped();
    this.inputCounts = { admitted: 0, matched: 0 };
    this.awaitingFrame = 0;
    this.shownSwitch = null;
    if (this.armed) {
      this.startWindow();
    } else {
      this.generation += 1;
      this.typing.clear();
      this.switchT0 = null;
    }
  }

  snapshot(): PerfSnapshot {
    const frames = this.frames.all();
    const longTasks = this.longTasks.all();
    let pending = 0;
    for (const state of this.typing.values()) {
      pending += state.pending.length;
    }
    return {
      version: 6,
      input_echo_ms: percentileStats(this.inputEcho.all()),
      switch_paint_ms: percentileStats(this.switchPaint.all()),
      switch_provenance: { ...this.switchProvenance },
      input_counts: { ...this.inputCounts, pending, awaiting_frame: this.awaitingFrame },
      frame_ms: {
        ...percentileStats(frames),
        over_50ms: frames.filter((value) => value > 50).length,
      },
      longtask_ms: {
        ...percentileStats(longTasks),
        total: longTasks.reduce((sum, value) => sum + value, 0),
      },
      dropped: { ...this.dropped },
      armed: this.armed,
    };
  }

  /** The line may have changed under the prediction: drop it, count what was
      still waiting, and anchor afresh at the next key. */
  private restartTyping(sessionId: string): void {
    const state = this.typing.get(sessionId);
    if (state) {
      this.dropped.input_line_reset += state.pending.length;
      this.typing.delete(sessionId);
    }
  }

  /** A new generation with a fresh frame loop and long-task observer. */
  private startWindow(): void {
    this.shownSwitch?.cleanup();
    this.generation += 1;
    const generation = this.generation;
    this.typing.clear();
    this.switchT0 = null;
    this.shownSwitch = null;
    let lastFrame: number | null = null;
    const tick = (): void => {
      if (!this.armed || generation !== this.generation) {
        return;
      }
      const at = this.now();
      if (lastFrame !== null) {
        this.frames.push(at - lastFrame);
      }
      lastFrame = at;
      this.nextFrame(tick);
    };
    this.nextFrame(tick);
    this.stopObserver();
    if (typeof PerformanceObserver !== "undefined") {
      try {
        const observer = new PerformanceObserver((list) => {
          if (this.observer !== observer || generation !== this.generation) {
            return;
          }
          for (const entry of list.getEntries()) {
            this.longTasks.push(entry.duration);
          }
        });
        observer.observe({ entryTypes: ["longtask"] });
        this.observer = observer;
      } catch {
        this.observer = null;
      }
    }
  }

  private stopObserver(): void {
    this.observer?.disconnect();
    this.observer = null;
  }
}

/** Exposed only in automation mode; read and driven over CDP. */
export function exposePerfProbes(target: object, enabled: boolean, probes: PerfProbes): boolean {
  if (!enabled || "__PRIM1_PERF__" in target) {
    return false;
  }
  Object.defineProperty(target, "__PRIM1_PERF__", {
    value: Object.freeze({
      version: 6,
      arm: () => probes.arm(),
      disarm: () => probes.disarm(),
      reset: () => probes.reset(),
      snapshot: () => probes.snapshot(),
    }),
    configurable: false,
    enumerable: false,
    writable: false,
  });
  return true;
}
