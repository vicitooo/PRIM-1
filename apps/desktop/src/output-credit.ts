import type { OutputCredit } from "./types";

export type OutputDisposition = "parsed" | "stale_epoch" | "stale_run" |
  "pane_disposed" | "write_failed" | "bootstrap_overflow" | "unattached_overflow";

/** Ticket ownership, not a payload buffer. The backend caps all live tickets. */
export class RendererOutputCredit {
  epoch: string | null = null;
  private pending = new Map<string, OutputCredit>();
  private tickets = new Set<string>();

  constructor(private readonly finish: (credit: OutputCredit, disposition: OutputDisposition) => void) {}

  admit(credit: OutputCredit): boolean {
    if (credit.epoch !== this.epoch) {
      this.finish(credit, "stale_epoch");
      return false;
    }
    const keys = credit.tickets.map((ticket) => `${credit.epoch}:${ticket}`);
    // A duplicate event must not discard a ticket whose original is still
    // buffered/parsing. Internal bootstrap replay skips this admission step.
    if (keys.some((key) => this.tickets.has(key))) return false;
    for (const key of keys) this.tickets.add(key);
    this.pending.set(keys[0], credit);
    return true;
  }

  settle(credit: OutputCredit, disposition: OutputDisposition): void {
    const key = `${credit.epoch}:${credit.tickets[0]}`;
    if (!this.pending.delete(key)) return;
    for (const ticket of credit.tickets) this.tickets.delete(`${credit.epoch}:${ticket}`);
    this.finish(credit, disposition);
  }

  disposeSession(sessionId: string): void {
    for (const credit of [...this.pending.values()]) {
      if (credit.run.session_id === sessionId) this.settle(credit, "pane_disposed");
    }
  }
}
