import { describe, expect, it, vi } from "vitest";
import { completeDesktopStartup, desktopCommandAllowed, loadTrialStatus, parseTrialStatus, TRIAL_STATUS_UNAVAILABLE_TEXT, type TrialStatus } from "./trial-startup";

const held: TrialStatus = {
  admission: "held", backend_initialized: true,
  challenge: { attempt_id: "11111111-1111-4111-8111-111111111111", boot_nonce: "22222222-2222-4222-8222-222222222222", candidate_sha256: "a".repeat(64) },
};

describe("held desktop startup", () => {
  it("requires an explicit normal response and rejects malformed held responses", () => {
    expect(parseTrialStatus(null)).toBeNull();
    expect(parseTrialStatus(held)).toEqual(held);
    for (const value of [undefined, {}, { ...held, admission: "normal" }, { ...held, backend_initialized: false }, { ...held, challenge: { ...held.challenge, boot_nonce: "stale" } }]) {
      expect(() => parseTrialStatus(value)).toThrow();
    }
  });

  it("does not retry a valid initial status", async () => {
    for (const status of [null, held]) {
      const read = vi.fn().mockResolvedValue(status);
      const delay = vi.fn();
      expect(await loadTrialStatus(read, delay)).toEqual(status);
      expect(read).toHaveBeenCalledTimes(1);
      expect(delay).not.toHaveBeenCalled();
    }
  });

  it("retries a transient status failure once before initializing exactly once", async () => {
    for (const status of [null, held]) {
      const order: string[] = [];
      const read = vi.fn()
        .mockImplementationOnce(async () => { order.push("read failed"); throw new Error("transport"); })
        .mockImplementationOnce(async () => { order.push("read recovered"); return status; });
      const recovered = await loadTrialStatus(read, async () => { order.push("delay"); });
      const initialize = vi.fn(async () => { order.push("initialize"); });
      const acknowledge = vi.fn();
      const resume = vi.fn();
      await completeDesktopStartup(recovered, initialize, async () => {}, acknowledge, resume);
      expect(order).toEqual(["read failed", "delay", "read recovered", "initialize"]);
      expect(read).toHaveBeenCalledTimes(2);
      expect(initialize).toHaveBeenCalledTimes(1);
      expect(resume).toHaveBeenCalledTimes(status === null ? 1 : 0);
      expect(acknowledge).toHaveBeenCalledTimes(status === null ? 0 : 1);
    }
  });

  it("stays blocked after two rejected or malformed reads without starting initialization", async () => {
    for (const read of [vi.fn().mockRejectedValue(new Error("transport")), vi.fn().mockResolvedValue({})]) {
      const initialize = vi.fn();
      const delay = vi.fn();
      const resume = vi.fn();
      let status: TrialStatus | null | undefined;
      const startup = async () => {
        status = await loadTrialStatus(read, delay);
        await completeDesktopStartup(status, initialize, vi.fn(), vi.fn(), resume);
      };
      await expect(startup()).rejects.toThrow();
      expect(read).toHaveBeenCalledTimes(2);
      expect(delay).toHaveBeenCalledTimes(1);
      expect(initialize).not.toHaveBeenCalled();
      expect(resume).not.toHaveBeenCalled();
      expect(desktopCommandAllowed(status, "start_session")).toBe(false);
    }
    expect(TRIAL_STATUS_UNAVAILABLE_TEXT).toContain("Reload the window to retry");
  });

  it("blocks every mutation family and future commands while status is unknown or held", () => {
    for (const status of [undefined, held]) {
      for (const command of ["start_session", "resume_session_after_restart", "restart_session", "stop_session", "send_input", "resize_session", "set_room_resume_on_restart", "create_room", "post_room_message", "deliver_room_message", "retry_room_delivery", "brief_room_member", "choose_workspace_directory", "set_task_folder", "choose_task_folder", "set_personal_repo", "choose_personal_repo", "set_idle_hibernation", "choose_room_save_path", "choose_room_source", "export_room", "inspect_room_source", "import_room", "invite_room_guest", "revoke_room_guest", "future_mutation"]) {
        expect(desktopCommandAllowed(status, command), command).toBe(false);
      }
      expect(desktopCommandAllowed(status, "read_room_feed")).toBe(true);
      expect(desktopCommandAllowed(status, "list_room_guests")).toBe(true);
      expect(desktopCommandAllowed(status, "trial_status")).toBe(true);
      // Reads only; the backend reports that a held trial never opens the folder.
      expect(desktopCommandAllowed(status, "get_task_folder")).toBe(true);
      expect(desktopCommandAllowed(status, "tasks_query")).toBe(true);
      expect(desktopCommandAllowed(status, "discover_local_sessions")).toBe(true);
      expect(desktopCommandAllowed(status, "get_personal_repo")).toBe(true);
      expect(desktopCommandAllowed(status, "get_room_fallbacks")).toBe(true);
      expect(desktopCommandAllowed(status, "set_room_fallbacks")).toBe(false);
    }
    expect(desktopCommandAllowed(null, "start_session")).toBe(true);
  });

  it("acknowledges the exact held challenge only after reconciliation and a frame, never resuming", async () => {
    const order: string[] = [];
    const resume = vi.fn();
    const ack = vi.fn(async (challenge) => { expect(challenge).toEqual(held.challenge); order.push("ack"); });
    await completeDesktopStartup(held, async () => { order.push("bootstrap"); }, async () => { order.push("frame"); }, ack, resume);
    expect(order).toEqual(["bootstrap", "frame", "ack"]);
    expect(resume).not.toHaveBeenCalled();
    expect(desktopCommandAllowed(held, "start_session")).toBe(false);
  });

  it("initialization, frame and receipt failures never resume a held trial", async () => {
    for (const failingStage of ["bootstrap", "frame", "ack"]) {
      const resume = vi.fn();
      const stage = (name: string) => async () => { if (name === failingStage) throw new Error(name); };
      await expect(completeDesktopStartup(held, stage("bootstrap"), stage("frame"), stage("ack"), resume)).rejects.toThrow(failingStage);
      expect(resume).not.toHaveBeenCalled();
    }
  });

  it("preserves ordinary resume after initialization without a trial receipt", async () => {
    const order: string[] = [];
    const ack = vi.fn();
    await completeDesktopStartup(null, async () => { order.push("bootstrap"); }, async () => { order.push("frame"); }, ack, () => { order.push("resume"); });
    expect(order).toEqual(["bootstrap", "resume"]);
    expect(ack).not.toHaveBeenCalled();
  });
});
