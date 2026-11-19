export class InitialTerminalBanner {
  private visible = true;
  /** The last observed run state; null until the first snapshot. */
  private running: boolean | null = null;

  constructor(
    private readonly removeOverlay: () => void,
    private readonly writeTerminal: (chunk: string, onParsed?: () => void) => void,
  ) {}

  observeRunning(running: boolean): void {
    this.running = running;
    if (running) {
      this.dismiss();
    } else {
      // The prelaunch view may show the same overlay again for a recovery;
      // the next run must hide it once more (Reviewer d393719c).
      this.visible = true;
    }
  }

  forwardOutput(chunk: string, onParsed?: () => void): void {
    // Late output of a run already seen stopped must not hide the recovery
    // overlay shown for it.
    if (this.running !== false) {
      this.dismiss();
    }
    if (onParsed) this.writeTerminal(chunk, onParsed);
    else this.writeTerminal(chunk);
  }

  private dismiss(): void {
    if (!this.visible) {
      return;
    }
    this.visible = false;
    this.removeOverlay();
  }
}
