// Room save and load (A92): plain-English results for the room status line.
import type { RoomExportReport, RoomImportMemberState, RoomImportReport, RoomSourceSummary } from "./types";

export interface RoomPackageSummary {
  summary: string;
  level: "info" | "warn";
  details: string;
}

const STATES: [RoomImportMemberState, string, string][] = [
  ["resumes", "resumes its conversation", "resume their conversations"],
  ["history_needed", "history needed", "history needed"],
  ["held", "held", "held"],
  ["starts_fresh", "starts fresh", "start fresh"],
];

const STATE_LABEL: Record<RoomImportMemberState, string> = {
  resumes: "resumes its conversation",
  history_needed: "history needed",
  held: "held",
  starts_fresh: "starts fresh",
};

function plural(count: number, one: string, many: string): string {
  return `${count.toLocaleString("en-US")} ${count === 1 ? one : many}`;
}

function sourceCounts(source: RoomSourceSummary): string {
  return `${plural(source.members, "member", "members")}, ${plural(source.records, "feed record", "feed records")}`;
}

/** The confirm shown before anything is written. */
export function roomSourceConfirmText(source: RoomSourceSummary): string {
  return `Load "${source.label}": ${sourceCounts(source)}. A new room is created; nothing starts.`;
}

/** A runtime copy with several rooms: a short numbered list to choose from. */
export function roomSourceListText(sources: readonly RoomSourceSummary[]): string {
  return [`This runtime copy has ${sources.length} rooms. Type the number of the room to load:`,
    ...sources.map((source, index) => `${index + 1}. ${source.label} (${source.unavailable
      ? `unavailable: ${source.unavailable}` : sourceCounts(source)})`)].join("\n");
}

/** The source an answer names: a lone source needs none. */
function namedRoomSource(sources: readonly RoomSourceSummary[], answer: string | null): RoomSourceSummary | null {
  if (sources.length === 1) return sources[0];
  const index = Number((answer ?? "").trim());
  return Number.isInteger(index) && index >= 1 && index <= sources.length ? sources[index - 1] : null;
}

/** One source loads directly; otherwise the typed number picks it. An
    unavailable room can never be picked. */
export function pickRoomSource(sources: readonly RoomSourceSummary[], answer: string | null): RoomSourceSummary | null {
  const named = namedRoomSource(sources, answer);
  return named && !named.unavailable ? named : null;
}

/** `Loaded "Walk room": 2 resume their conversations, 1 starts fresh. …`
    Unsafe profiles and every warning (such as a history-visibility change)
    stay in the visible line; one line per member goes to the details. */
export function roomLoadSummary(report: RoomImportReport): RoomPackageSummary {
  const counts = STATES.map(([state, one, many]) => {
    const count = report.members.filter((member) => member.state === state).length;
    return count === 0 ? null : plural(count, one, many);
  }).filter((part): part is string => part !== null);
  const unsafe = report.members.filter((member) => member.permission_profile === "unsafe").length;
  const sentences = [
    `Loaded "${report.room.label}": ${counts.length > 0 ? counts.join(", ") : "no members"}. Nothing was started.`,
    ...(unsafe > 0 ? [`${plural(unsafe, "member uses", "members use")} the unsafe profile, as saved.`] : []),
    ...report.warnings,
  ];
  const attention = report.warnings.length > 0
    || report.members.some((member) => member.state === "held" || member.state === "history_needed");
  return {
    summary: sentences.join(" "),
    level: attention ? "warn" : "info",
    details: report.members
      .map((member) => `${member.label}: ${STATE_LABEL[member.state]}${member.note ? ` (${member.note})` : ""}`)
      .join("\n"),
  };
}

/** `Saved "Walk room" to D:\rooms\walk.prim1room: 3 members, 42 feed records.` */
export function roomSaveSummary(report: RoomExportReport): RoomPackageSummary {
  const paused = report.paused_ms >= 250
    ? [`Room traffic waited ${report.paused_ms} ms while the feed was copied.`] : [];
  return {
    summary: [
      `Saved "${report.label}" to ${report.path}: ${plural(report.members, "member", "members")}, ${plural(report.records, "feed record", "feed records")}.`,
      ...paused,
      ...report.warnings,
    ].join(" "),
    level: report.warnings.length > 0 ? "warn" : "info",
    details: "",
  };
}

/** The steps `Load room…` takes; main.ts supplies the commands and the UI. */
export interface RoomLoadSteps {
  choose(): Promise<string | null>;
  inspect(path: string): Promise<RoomSourceSummary[]>;
  ask(list: string): string | null;
  /** The native dialog; only an awaited, exact `true` lets the load run. */
  confirm(text: string): Promise<boolean>;
  load(path: string, sourceRoomId: string): Promise<RoomImportReport>;
  refresh(): Promise<void>;
  /** Enter the loaded room as the work context, so later refreshes keep it. */
  enter(roomId: string): void;
  loaded(result: RoomPackageSummary): void;
  problem(message: string): void;
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** Pick, inspect, confirm, then exactly one import. A display refresh that
    fails after a committed load is reported as that, never as a failed load,
    and the load is not retried. */
export async function loadRoomFlow(steps: RoomLoadSteps): Promise<"loaded" | "cancelled" | "failed"> {
  let chosen: { path: string; source: RoomSourceSummary };
  try {
    const path = await steps.choose();
    if (!path) return "cancelled";
    const sources = await steps.inspect(path);
    if (sources.length === 0) {
      steps.problem("Room not loaded: this runtime copy has no rooms.");
      return "failed";
    }
    const answer = sources.length > 1 ? steps.ask(roomSourceListText(sources)) : null;
    const unavailable = namedRoomSource(sources, answer)?.unavailable;
    if (unavailable) {
      steps.problem(`Room not loaded: ${unavailable}`);
      return "failed";
    }
    const source = pickRoomSource(sources, answer);
    // A rejected confirm lands in the catch below: reported, nothing loaded.
    if (!source || (await steps.confirm(roomSourceConfirmText(source))) !== true) return "cancelled";
    chosen = { path, source };
  } catch (error) {
    steps.problem(`Room not loaded: ${messageOf(error)}`);
    return "failed";
  }
  let report: RoomImportReport;
  try {
    report = await steps.load(chosen.path, chosen.source.source_room_id);
  } catch (error) {
    steps.problem(`Room not loaded: ${messageOf(error)}`);
    // A failed load can leave held lobby copies (its message says so): show them.
    await steps.refresh().catch(() => undefined);
    return "failed";
  }
  const result = roomLoadSummary(report);
  try {
    await steps.refresh();
  } catch (error) {
    steps.problem(`Room loaded; the display did not refresh: ${messageOf(error)}. ${result.summary}`);
    return "loaded";
  }
  steps.enter(report.room.room_id);
  steps.loaded(result);
  return "loaded";
}
