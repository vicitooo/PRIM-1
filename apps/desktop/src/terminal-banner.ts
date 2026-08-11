export class InitialTerminalBanner {
  private visible = true;

  constructor(
    private readonly removeOverlay: () => void,
    private readonly writeTerminal: (chunk: string) => void,
  ) {}

  observeRunning(running: boolean): void {
    if (running) {
      this.dismiss();
    }
  }

  forwardOutput(chunk: string): void {
    this.dismiss();
    this.writeTerminal(chunk);
  }

  private dismiss(): void {
    if (!this.visible) {
      return;
    }
    this.visible = false;
    this.removeOverlay();
  }
}
