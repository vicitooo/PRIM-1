import { describe, expect, it } from "vitest";
import { abandonUpgradeConfirmText, upgradeBannerView, upgradeResumeMembers } from "./upgrade-banner";
import type { UpgradeStage, UpgradeStatus } from "./types";

function status(stage: UpgradeStage, overrides: Partial<UpgradeStatus> = {}): UpgradeStatus {
  return {
    attempt_id: "0b6b2b9e-7d55-4b1e-9b8a-1d2c3e4f5a6b",
    stage,
    candidate_sha256: "c0ffee12".padEnd(64, "0"),
    requester_label: "Claude",
    notice_until: "2026-10-19T12:00:00Z",
    expires_at: "2026-10-19T12:15:00Z",
    message: "Every running member stops, then resumes its own conversation.",
    can_cancel: stage === "notice",
    can_abandon: stage === "held",
    member_session_ids: ["a", "b"],
    ...overrides,
  };
}

const NOTICE_END = Date.parse("2026-10-19T12:00:00Z");

describe("upgrade banner", () => {
  it("counts down the notice and offers Cancel only then", () => {
    const view = upgradeBannerView(status("notice"), NOTICE_END - 41_500);
    expect(view).toMatchObject({ visible: true, countdown: "Restarts in 42 s", canCancel: true, canAbandon: false });
    expect(view.title).toBe("Upgrade to build c0ffee12 requested by Claude");
    expect(view.body).toContain("resumes its own conversation");
    expect(upgradeBannerView(status("notice"), NOTICE_END + 5_000).countdown).toBe("Restarts when ready");
    expect(upgradeBannerView(status("closing", { can_cancel: true }), NOTICE_END)).toMatchObject({ canCancel: false, countdown: null });
  });

  it("offers Abandon only for a held attempt and Dismiss only for a final message", () => {
    expect(upgradeBannerView(status("held"), 0)).toMatchObject({ visible: true, canAbandon: true, canCancel: false, canDismiss: false });
    expect(upgradeBannerView(status("resumed"), 0)).toMatchObject({ canAbandon: false, canDismiss: true });
    expect(upgradeBannerView(status("failed"), 0)).toMatchObject({ canAbandon: false, canDismiss: true });
    expect(upgradeBannerView(status("resuming"), 0)).toMatchObject({ visible: true, canDismiss: false });
    expect(upgradeBannerView(status("cancelled"), 0).visible).toBe(false);
    expect(upgradeBannerView(null, 0).visible).toBe(false);
  });

  it("leaves only the upgrade's own members out of the restart resume", () => {
    expect([...upgradeResumeMembers(status("resuming"))]).toEqual(["a", "b"]);
    expect([...upgradeResumeMembers(status("resumed"))]).toEqual(["a", "b"]);
    for (const stage of ["held", "failed", "notice", "cancelled"] as const) {
      expect(upgradeResumeMembers(status(stage)).size, stage).toBe(0);
    }
    expect(upgradeResumeMembers(null).size).toBe(0);
  });

  it("asks before abandoning and says what abandoning does not prove", () => {
    const text = abandonUpgradeConfirmText(status("held"));
    expect(text).toContain("0b6b2b9e");
    expect(text).toContain("does not show that its helper or the new build stopped");
  });
});
