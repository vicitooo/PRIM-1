import type { UpgradeStatus } from "./types";

/** A108: the self-upgrade banner, derived from the supervisor's status. */
export interface UpgradeBannerView {
  visible: boolean;
  title: string;
  body: string;
  /** "Restarts in 42 s" during the notice, otherwise null. */
  countdown: string | null;
  canCancel: boolean;
  canAbandon: boolean;
  canDismiss: boolean;
}

const HIDDEN: UpgradeBannerView = {
  visible: false, title: "", body: "", countdown: null, canCancel: false, canAbandon: false, canDismiss: false,
};

export function upgradeBannerView(status: UpgradeStatus | null, nowMs: number): UpgradeBannerView {
  if (!status || status.stage === "cancelled") return HIDDEN;
  const build = status.candidate_sha256.slice(0, 8);
  const base = { ...HIDDEN, visible: true, body: status.message, canCancel: status.can_cancel, canAbandon: status.can_abandon };
  switch (status.stage) {
    case "notice": {
      const seconds = Math.ceil((Date.parse(status.notice_until) - nowMs) / 1000);
      return {
        ...base,
        title: `Upgrade to build ${build} requested by ${status.requester_label}`,
        countdown: Number.isFinite(seconds) && seconds > 0 ? `Restarts in ${seconds} s` : "Restarts when ready",
      };
    }
    case "closing":
      return { ...base, title: "PRIM-1 is restarting for an upgrade", canCancel: false };
    case "held":
      return { ...base, title: `Upgrade ${status.attempt_id.slice(0, 8)} is held`, canCancel: false };
    case "resuming":
      return { ...base, title: `Restarted on build ${build}`, canCancel: false };
    case "resumed":
      return { ...base, title: `Upgrade to build ${build} finished`, canCancel: false, canDismiss: true };
    case "failed":
      return { ...base, title: `Upgrade ${status.attempt_id.slice(0, 8)} did not continue`, canCancel: false, canDismiss: true };
    default: {
      const _exhaustive: never = status.stage;
      void _exhaustive;
      return HIDDEN;
    }
  }
}

/** Members the upgrade resumes itself; the ordinary restart resume skips them. */
export function upgradeResumeMembers(status: UpgradeStatus | null): Set<string> {
  return new Set(status && (status.stage === "resuming" || status.stage === "resumed") ? status.member_session_ids : []);
}

export function abandonUpgradeConfirmText(status: UpgradeStatus): string {
  return `Abandon held upgrade ${status.attempt_id.slice(0, 8)}? This only records your decision: it does not show that its helper or the new build stopped. Check first that no second PRIM-1 runs for this runtime.`;
}
