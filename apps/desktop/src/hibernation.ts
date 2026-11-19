import type { SessionSnapshot } from "./types";

export type SleepSnapshot = Pick<SessionSnapshot,
  "hibernated" | "running" | "output_draining" | "lifecycle_state" | "recovery" | "last_error">;

/** Native owns eligibility. A stopped/error/held member is never inferred asleep. */
export function isSleeping(session: SleepSnapshot): boolean {
  return session.hibernated === true && !session.running && !session.output_draining
    && session.lifecycle_state === "closed" && !session.recovery && !session.last_error;
}

/** Stop also revokes native sleep/wake eligibility or cancels a pending crash restart. */
export function canStopSession(session: Pick<SessionSnapshot, "running" | "hibernated" | "crash_respawn_waiting">): boolean {
  return session.running || session.hibernated === true || session.crash_respawn_waiting === true;
}

/** Undefined means unsupported/unavailable, not a confirmed off setting. */
export function readIdleHibernateMinutes(value: unknown): number | null | undefined {
  if (value === null) return null;
  return typeof value === "number" && Number.isInteger(value) && value >= 1 && value <= 1440
    ? value : undefined;
}

/** Prepared controls stay absent until native reports the capability; null is supported/off. */
export function renderIdleHibernationCapability(section: Pick<HTMLElement, "hidden">, value: unknown): number | null | undefined {
  const minutes = readIdleHibernateMinutes(value);
  section.hidden = minutes === undefined;
  return minutes;
}

export function idleHibernateMinutesFromInput(enabled: boolean, text: string): number | null {
  if (!enabled) return null;
  const minutes = readIdleHibernateMinutes(text.trim() ? Number(text) : undefined);
  if (minutes === undefined || minutes === null) throw new Error("Choose a whole number of idle minutes from 1 to 1440.");
  return minutes;
}
