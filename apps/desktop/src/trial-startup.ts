export interface TrialChallenge {
  attempt_id: string;
  boot_nonce: string;
  candidate_sha256: string;
}

export interface TrialStatus {
  challenge: TrialChallenge;
  admission: "held";
  backend_initialized: true;
}

export const TRIAL_HOLD_TEXT = "Preview held — agents stay stopped. Changes are disabled.";
export const TRIAL_STATUS_UNAVAILABLE_TEXT = "Startup paused — admission check unavailable. Reload the window to retry. Agents were not resumed.";

/** Status failure must never silently select ordinary startup. */
export function parseTrialStatus(value: unknown): TrialStatus | null {
  if (value === null) return null;
  if (!value || typeof value !== "object") throw new Error("Trial status is unavailable");
  const status = value as Partial<TrialStatus>;
  const challenge = status.challenge;
  const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
  if (status.admission !== "held" || status.backend_initialized !== true || !challenge
    || !uuid.test(challenge.attempt_id) || !uuid.test(challenge.boot_nonce)
    || !/^[0-9a-f]{64}$/.test(challenge.candidate_sha256)) {
    throw new Error("Trial status does not describe an initialized held backend");
  }
  return status as TrialStatus;
}

/** Retry only the read, before listeners or bootstrap can have side effects. */
export async function loadTrialStatus(
  read: () => Promise<unknown>,
  retryDelay: () => Promise<void> = () => new Promise((resolve) => setTimeout(resolve, 250)),
): Promise<TrialStatus | null> {
  try {
    return parseTrialStatus(await read());
  } catch {
    await retryDelay();
    return parseTrialStatus(await read());
  }
}

// These commands inspect the app, maintain the renderer transport, or operate
// its window. Backend admission remains authoritative, including raw IPC.
const HELD_COMMANDS = new Set([
  "trial_status", "trial_renderer_acknowledged", "automation_mode", "bootstrap",
  "prime_enabled", "prime_default_working_directory", "default_room_brief",
  "read_room_brief", "read_room_feed", "list_room_guests", "get_room_fallbacks", "telemetry_snapshot", "toggle_fullscreen",
  // Read-only: a held trial never opens the task folder or writes the
  // session register, so these report why.
  "get_task_folder", "tasks_query", "discover_local_sessions", "get_personal_repo",
  "begin_output_renderer", "output_renderer_ready", "acknowledge_output", "discard_output",
]);

export function desktopCommandAllowed(status: TrialStatus | null | undefined, name: string): boolean {
  return status === null || HELD_COMMANDS.has(name);
}

/** Called after status is known, with listener registration + reconciliation
 * as one operation. A renderer acknowledgement never changes admission. */
export async function completeDesktopStartup(
  status: TrialStatus | null,
  initialize: () => Promise<void>,
  renderingFrame: () => Promise<void>,
  acknowledge: (challenge: TrialChallenge) => Promise<unknown>,
  resume: () => void,
): Promise<void> {
  await initialize();
  if (status === null) {
    resume();
    return;
  }
  await renderingFrame();
  await acknowledge(status.challenge);
}
