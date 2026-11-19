/** xterm's existing effective bound, now explicit for every pane. */
export const TERMINAL_SCROLLBACK_LINES = 1000;

/** An exhausted, actionable error may be shown/focused at the retained grid. */
export function terminalGridErrorAllowsCompletion(
  errorRun: string | null,
  currentRun: string | null,
  visible: boolean,
  disposed: boolean,
): boolean {
  return !disposed && visible && errorRun !== null && errorRun === currentRun;
}

/** No measured/default-width guess may precede a restored PTY's first output. */
export function initialTerminalGrid(size: readonly number[] | null | undefined): {
  cols: number;
  rows: number;
} {
  if (!size || size.length !== 2
    || !Number.isInteger(size[0]) || size[0] < 2 || size[0] > 65535
    || !Number.isInteger(size[1]) || size[1] < 1 || size[1] > 65535) {
    throw new Error("Terminal grid is unavailable in the backend snapshot; reload with matching desktop assets.");
  }
  return { cols: size[0], rows: size[1] };
}

/** Hidden ancestors include shell views hidden by CSS, not just card.hidden. */
export function terminalHostIsVisible(host: HTMLElement): boolean {
  const doc = host.ownerDocument;
  const view = doc.defaultView;
  if (doc.visibilityState !== "visible" || !host.isConnected || !view
    || host.closest("[hidden], [inert]")) {
    return false;
  }
  const style = view.getComputedStyle(host);
  if (style.visibility === "hidden" || style.visibility === "collapse") {
    return false;
  }
  const rect = host.getBoundingClientRect();
  return rect.width > 0 && rect.height > 0
    && rect.bottom > 0 && rect.right > 0
    && rect.top < view.innerHeight && rect.left < view.innerWidth;
}

/** Coalesce snapshot, observer and activation requests; recheck visibility at flush. */
export class TerminalLayoutScheduler {
  private frame: number | null = null;
  private completion: { target: string; shown?: () => void; focus?: () => void } | undefined;

  constructor(private readonly effects: {
    visible: () => boolean;
    requestFrame: (callback: () => void) => number;
    cancelFrame: (frame: number) => void;
    layout: () => boolean | void;
  }) {}

  request(completion?: { target: string; shown?: () => void; focus?: () => void }): void {
    if (completion) {
      this.completion = this.completion?.target === completion.target
        ? { ...this.completion, ...completion }
        : completion;
    }
    if (!this.effects.visible() || this.frame !== null) return;
    this.frame = this.effects.requestFrame(() => {
      this.frame = null;
      if (!this.effects.visible()) return;
      // A parser fence schedules the next attempt when ready; do not spin RAFs.
      if (this.effects.layout() === false) return;
      const complete = this.completion;
      this.completion = undefined;
      complete?.focus?.();
      complete?.shown?.();
    });
  }

  /** Failure settles the visible target at its confirmed grid, without fitting again. */
  settleWithoutLayout(target: string, currentAndVisible: () => boolean): void {
    if (!this.effects.visible() || !currentAndVisible()
      || (this.completion && this.completion.target !== target)) return;
    if (this.frame !== null) {
      this.effects.cancelFrame(this.frame);
      this.frame = null;
    }
    const complete = this.completion;
    this.completion = undefined;
    try { complete?.focus?.(); } finally { complete?.shown?.(); }
  }

  visibilityChanged(): void {
    if (this.effects.visible()) {
      this.request();
    } else if (this.frame !== null) {
      this.effects.cancelFrame(this.frame);
      this.frame = null;
    }
  }
}

/** Two automatic attempts per adopted run; only explicit action renews the budget. */
export class RunGridRecovery {
  private key: string | null = null;
  private revision = 0;
  private attempted = false;
  private active = false;
  private disposed = false;

  adopt(key: string): void {
    if (this.key === key) return;
    this.key = key;
    this.revision++;
    this.attempted = false;
    this.active = false;
  }

  resolved(key: string): void {
    if (this.key !== key) return;
    this.revision++;
    this.attempted = true;
    this.active = false;
  }

  async request(
    key: string,
    load: (current: () => boolean) => Promise<void>,
    failed: (error: unknown) => void,
    explicit = false,
  ): Promise<void> {
    if (this.disposed || key !== this.key || this.active || (this.attempted && !explicit)) return;
    this.attempted = true;
    this.active = true;
    const revision = ++this.revision;
    const current = (): boolean => !this.disposed && this.key === key && this.revision === revision;
    for (let attempt = 0; attempt < 2; attempt++) {
      try {
        await load(current);
        if (current()) this.active = false;
        return;
      } catch (error) {
        if (!current()) return;
        if (attempt === 0) continue;
        this.active = false;
        failed(error);
      }
    }
  }

  dispose(): void { this.disposed = true; this.revision++; this.active = false; }
}

/** One empty xterm write marks a fixed backlog, never a moving live-output tail. */
export class InitialReplayFence {
  private revision = 0;
  private waiting = true;
  private sealed = false;
  private disposed = false;

  get ready(): boolean { return !this.disposed && !this.waiting; }

  reset(): void {
    this.revision++;
    this.waiting = true;
    this.sealed = false;
  }

  seal(writeBarrier: (parsed: () => void) => void, released: (error?: unknown) => void): void {
    if (this.disposed || this.sealed) return;
    this.sealed = true;
    const revision = this.revision;
    const finish = (error?: unknown): void => {
      if (this.disposed || revision !== this.revision || !this.waiting) return;
      this.waiting = false;
      // Never throw into xterm's WriteBuffer callback: that aborts its drain.
      try { released(error); } catch { /* hold is already released */ }
    };
    try { writeBarrier(() => finish()); } catch (error) { finish(error); }
  }

  dispose(): void { this.disposed = true; this.revision++; }
}

export interface TerminalGrid { cols: number; rows: number }

/** One in-flight resize per pane/run; stale async results cannot alter a new run. */
export class TerminalResizeState {
  private revision = 0;
  private active: number | null = null;
  private disposed = false;

  constructor(private run: string, private grid: TerminalGrid) {}

  get confirmed(): TerminalGrid { return { ...this.grid }; }
  get busy(): boolean { return this.active !== null; }

  adopt(run: string, grid: TerminalGrid, authoritative = false): boolean {
    if (run === this.run && !authoritative) return false;
    this.run = run;
    this.grid = { ...grid };
    this.active = null;
    this.revision++;
    return true;
  }

  begin(): number | null {
    if (this.disposed || this.busy) return null;
    this.active = ++this.revision;
    return this.active;
  }

  current(ticket: number): boolean { return !this.disposed && this.active === ticket; }

  confirm(ticket: number, grid: TerminalGrid): void {
    if (this.current(ticket)) this.grid = { ...grid };
  }

  finish(ticket: number): TerminalGrid | null {
    if (!this.current(ticket)) return null;
    this.active = null;
    return this.confirmed;
  }

  dispose(): void { this.disposed = true; this.active = null; this.revision++; }
}

/** Restore an admitted shrink even after hide; only two successful legs heal loss. */
export async function repaintTerminalGrid(
  grid: TerminalGrid,
  resize: (grid: TerminalGrid) => Promise<boolean>,
  current: () => boolean,
  confirmed: (grid: TerminalGrid) => void,
): Promise<boolean> {
  const attempt = async (target: TerminalGrid): Promise<boolean> => {
    if (!current()) return false;
    let accepted = false;
    try { accepted = await resize(target); } catch { /* still try the restore */ }
    if (!current()) return false;
    if (accepted) confirmed(target);
    return accepted;
  };
  const shrunk = await attempt({ cols: grid.cols, rows: grid.rows - 1 });
  const restored = await attempt(grid);
  return shrunk && restored;
}
