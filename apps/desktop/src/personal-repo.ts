/* Settings → Personal repository: where PRIM-1 writes its session register,
   one small Git-tracked file per Claude, Codex and Grok member. */

/** `get_personal_repo` / `set_personal_repo`. */
export interface PersonalRepoStatus {
  /** The folder named, also when it cannot be used. */
  folder: string | null;
  source: "environment" | "setting" | "default";
  status: "on" | "off" | "error";
  /** Why it is off or unusable, or the writer's latest report. */
  message?: string;
}

const ORIGIN: Record<PersonalRepoStatus["source"], string> = {
  environment: "On. Set by PRIM1_PERSONAL_REPO for this PRIM-1.",
  setting: "On. Chosen in Settings.",
  default: "On. This is the default: the workspace.",
};

/** Where the folder came from, or why there is no register. */
export function personalRepoStateText(status: PersonalRepoStatus): string {
  if (status.status === "off") return status.message || "Off: this PRIM-1 keeps no session register.";
  if (status.status === "error") return status.message || "The personal repository cannot be used; no session register is written.";
  return status.message ? `${ORIGIN[status.source]} ${status.message}` : ORIGIN[status.source];
}

/** The Settings controls are locked only while the environment decides. */
export function personalRepoLocked(status: PersonalRepoStatus): boolean {
  return status.source === "environment";
}
