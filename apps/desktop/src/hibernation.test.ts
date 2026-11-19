import { describe, expect, it } from "vitest";
import { canStopSession, idleHibernateMinutesFromInput, isSleeping, readIdleHibernateMinutes, renderIdleHibernationCapability, type SleepSnapshot } from "./hibernation";

const sleeping: SleepSnapshot = {
  hibernated: true, running: false, output_draining: false, lifecycle_state: "closed",
  recovery: null, last_error: null,
};

describe("native hibernation projection", () => {
  it("never infers sleep from closed state and keeps active/error/held states visible", () => {
    expect(isSleeping(sleeping)).toBe(true);
    for (const changed of [
      { hibernated: undefined }, { hibernated: false }, { running: true }, { output_draining: true },
      { lifecycle_state: "failed" as const }, { lifecycle_state: "starting" as const },
      { last_error: "resume failed" }, { recovery: { original_id: "id", provenance: "hold", reason: "Held" } },
    ]) expect(isSleeping({ ...sleeping, ...changed })).toBe(false);
  });

  it("allows explicit Stop to clear sleep even without a process, then disables it on native clear", () => {
    expect(canStopSession(sleeping)).toBe(true);
    expect(canStopSession({ ...sleeping, hibernated: false })).toBe(false);
    expect(canStopSession({ running: false })).toBe(false);
    expect(canStopSession({ running: true })).toBe(true);
  });

  it("keeps Stop available during native crash backoff and disables it after cancellation", () => {
    const crashed = { ...sleeping, hibernated: false, lifecycle_state: "failed" as const, last_error: "process exited with code 3" };
    expect(canStopSession(crashed)).toBe(false);
    expect(canStopSession({ ...crashed, crash_respawn_waiting: true })).toBe(true);
    expect(canStopSession({ ...crashed, crash_respawn_waiting: false })).toBe(false);
    expect(canStopSession({ ...crashed, crash_respawn_waiting: undefined })).toBe(false);
    expect(canStopSession({ ...crashed, running: true, crash_respawn_waiting: false })).toBe(true);
    expect(isSleeping(crashed)).toBe(false);
  });
});

describe("idle hibernation setting", () => {
  it("keeps the whole prepared control block hidden for legacy snapshots, but shows supported off and enabled settings", () => {
    const controls = { hidden: true };
    const legacy: { idle_hibernate_minutes?: number | null } = {};
    expect(renderIdleHibernationCapability(controls, legacy.idle_hibernate_minutes)).toBeUndefined();
    expect(controls.hidden).toBe(true);
    expect(renderIdleHibernationCapability(controls, null)).toBeNull();
    expect(controls.hidden).toBe(false);
    expect(renderIdleHibernationCapability(controls, 60)).toBe(60);
    expect(controls.hidden).toBe(false);
    renderIdleHibernationCapability(controls, legacy.idle_hibernate_minutes);
    expect(controls.hidden).toBe(true);
  });
  it("distinguishes confirmed off from an absent, malformed or out-of-range backend setting", () => {
    expect(readIdleHibernateMinutes(null)).toBeNull();
    for (const value of [undefined, "60", false, {}, 0, -1, 1441, 1.5, NaN, Infinity]) {
      expect(readIdleHibernateMinutes(value)).toBeUndefined();
    }
    for (const value of [1, 60, 1440]) expect(readIdleHibernateMinutes(value)).toBe(value);
  });

  it("sends off explicitly and accepts only whole minutes within the native range", () => {
    expect(idleHibernateMinutesFromInput(false, "")).toBeNull();
    expect(idleHibernateMinutesFromInput(false, "invalid unused input")).toBeNull();
    for (const text of ["1", "60", " 1440 "]) expect(idleHibernateMinutesFromInput(true, text)).toBe(Number(text));
    for (const text of ["", " ", "0", "-1", "1441", "1.5", "NaN", "Infinity", "one"]) {
      expect(() => idleHibernateMinutesFromInput(true, text)).toThrow("1 to 1440");
    }
  });
});
