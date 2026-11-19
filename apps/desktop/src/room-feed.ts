import type {
  RoomDeliveryResult,
  RoomDeliveryStatus,
  RoomFeedCursor,
  RoomFeedEvent,
  RoomFeedItem,
  RoomSnapshot,
  DriverKind,
  SessionSnapshot,
} from "./types";

export const MAX_RENDERED_ROOM_FEED_EVENTS = 512;
export const MAX_RENDERED_ROOM_FEED_BYTES = 16 * 1024 * 1024;
export const MAX_PENDING_ROOM_INTENTS = 64;
export const MAX_PENDING_ROOM_INTENT_BYTES = 1024 * 1024;

/** Native origin, never editable message text, identifies scheduled messages. */
export function roomFallbackSenderLabel(item: Extract<RoomFeedItem, { kind: "message" }>): string | null {
  if (!item.fallback_origin) return null;
  const date = new Date(item.fallback_origin.scheduled_at);
  const time = Number.isNaN(date.getTime()) ? item.fallback_origin.scheduled_at
    : date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  return `PRIM-1 fallback · scheduled ${time}`;
}

export function roomRecoverySenderLabel(item: Extract<RoomFeedItem, { kind: "message" }>): string | null {
  return item.continuation_origin ? "PRIM-1 recovery" : null;
}

/** v3 (Claude PRIM-1 ef3870d walk): a notice PRIM-1 itself wrote is PRIM-1's,
    never shown as the operator's own post. */
export function roomNoticeSenderLabel(item: Extract<RoomFeedItem, { kind: "message" }>): string | null {
  return item.sender.kind === "prim1" ? "PRIM-1" : null;
}

/** v3 W5: a guest by the name history kept for it; `invited` is the room's
    current guest ids when known, so one no longer invited reads as revoked. */
export function roomGuestLabel(guestId: string, label: string | null | undefined,
  invited: ReadonlySet<string> | null): string {
  const name = label?.trim() ? label.trim() : `guest ${guestId.slice(0, 8)}`;
  return invited && !invited.has(guestId) ? `${name} (guest, revoked)` : `${name} (guest)`;
}

/** v3 W3 (D6 wake): the PRIM-1 notice a withdrawal sent (request id
    `withdrawal-<its request id>`): whom it woke, and who has read it. */
export function roomWithdrawalNotice(events: readonly RoomFeedEvent[], cancelRequestId: string):
  { recipients: string[]; seen: string[] } | null {
  const notice = events.find((event) => event.item.kind === "message" && event.item.sender.kind === "prim1"
    && event.item.request_id === `withdrawal-${cancelRequestId}`);
  if (!notice || notice.item.kind !== "message") return null;
  const messageId = notice.item.message_id;
  const fetched = new Set(events.flatMap((event) => event.item.kind === "delivery"
    && event.item.message_id === messageId && event.item.status === "fetched" ? [event.item.recipient_id] : []));
  return { recipients: notice.item.recipient_ids, seen: notice.item.recipient_ids.filter((id) => fetched.has(id)) };
}

/** v3 W3 (D6): what a withdrawal did per recipient, in words; never claims
    to undo a delivery (Claude PRIM-1 after 25e7ecf). Since a41d8d4 the
    members it reached are woken by a PRIM-1 notice; `notice` says whom it
    woke and who has read it (Claude PRIM-1, ac9f860 walk). */
export function roomWithdrawalText(item: { not_typed?: string[]; reached?: string[] },
  memberLabel: (sessionId: string) => string,
  notice: { recipients: string[]; seen: string[] } | null = null): string {
  const parts: string[] = [];
  if (item.not_typed?.length) parts.push(`Not typed: ${item.not_typed.map(memberLabel).join(", ")}.`);
  if (item.reached?.length) {
    const told = notice?.recipients.length
      ? `PRIM-1 notified ${notice.recipients.map(memberLabel).join(", ")}${notice.seen.length
        ? ` (seen by ${notice.seen.map(memberLabel).join(", ")})` : ""}; `
      : "";
    parts.push(`Already delivered to ${item.reached.map(memberLabel).join(", ")}; ${told}it does not undo what they did.`);
  }
  return parts.join(" ");
}

/** v3 W3 (D6 wake): the withdraw form, before the operator confirms. */
export function roomWithdrawPreview(reached: readonly string[]): string {
  return reached.length
    ? `Already delivered to ${reached.join(", ")}; PRIM-1 notifies them now; it does not undo what they did.`
    : "Not delivered to anyone yet.";
}

/** v3 W3 (D6): who a message already reached (typed, woken, read, being
    typed or disposed of), for the withdraw form before the operator confirms. */
export function roomMessageReached(events: readonly RoomFeedEvent[], messageId: string): string[] {
  const latest = new Map<string, Extract<RoomFeedItem, { kind: "delivery" }>>();
  const disposed = new Set<string>();
  for (const event of events) {
    const item = event.item;
    if (item.kind === "delivery" && item.message_id === messageId) {
      if (latest.get(item.recipient_id)?.status !== "fetched") latest.set(item.recipient_id, item);
    }
    if (item.kind === "ack" && item.message_id === messageId) disposed.add(item.recipient_id);
  }
  const reached = new Set(disposed);
  for (const [recipient, item] of latest) {
    if (item.bytes_written > 0 || ["pending", "written", "woken", "fetched"].includes(item.status)) reached.add(recipient);
  }
  return [...reached];
}

/** v3 W3 (b): who still owes an ACK-tracked message, by the name the
    operator recognises, keyed by the id a disposition names (a member's
    session id, a guest's id). A withdrawn message owes nothing. */
export function roomOpenObligations(events: readonly RoomFeedEvent[], memberLabel: (sessionId: string) => string,
  invited: ReadonlySet<string> | null): Map<string, { key: string; label: string }[]> {
  const disposed = new Set<string>();
  const withdrawn = new Set<string>();
  for (const event of events) {
    const item = event.item;
    if (item.kind === "ack") disposed.add(`${item.message_id}:${item.recipient_id}`);
    if (item.kind === "guest_ack") disposed.add(`${item.message_id}:${item.guest_id}`);
    if (item.kind === "cancel") withdrawn.add(item.message_id);
  }
  const open = new Map<string, { key: string; label: string }[]>();
  for (const event of events) {
    const item = event.item;
    if (item.kind !== "message" || !item.ack_requested || withdrawn.has(item.message_id)) continue;
    const owed = [
      ...item.recipient_ids.map((id) => ({ key: id, label: memberLabel(id) })),
      ...(item.guest_recipient_ids ?? []).map((id, index) =>
        ({ key: id, label: roomGuestLabel(id, item.guest_recipient_labels?.[index], invited) })),
    ].filter((recipient) => !disposed.has(`${item.message_id}:${recipient.key}`));
    if (owed.length) open.set(item.message_id, owed);
  }
  return open;
}

/** Members and guests a message was addressed to; "feed only" only when it
    addressed nobody (Claude PRIM-1 ef3870d walk: a message to a guest is not). */
export function roomMessageRecipientsLabel(item: Extract<RoomFeedItem, { kind: "message" }>,
  memberLabel: (sessionId: string) => string, invited: ReadonlySet<string> | null): string {
  const guests = (item.guest_recipient_ids ?? []).map((id, index) =>
    roomGuestLabel(id, item.guest_recipient_labels?.[index], invited));
  const names = [...item.recipient_ids.map(memberLabel), ...guests];
  return names.length ? names.join(", ") : "feed only";
}

const encoder = new TextEncoder();
const measuredBytes = new WeakMap<RoomFeedEvent, number>();

function eventBytes(event: RoomFeedEvent): number {
  const cached = measuredBytes.get(event);
  if (cached !== undefined) {
    return cached;
  }
  const bytes = encoder.encode(JSON.stringify(event)).byteLength;
  measuredBytes.set(event, bytes);
  return bytes;
}

export function retainRoomFeedWindow(
  events: readonly RoomFeedEvent[],
): RoomFeedEvent[] {
  let retainedBytes = 0;
  let start = events.length;
  while (start > 0 && events.length - start < MAX_RENDERED_ROOM_FEED_EVENTS) {
    const candidate = events[start - 1];
    const candidateBytes = eventBytes(candidate);
    if (retainedBytes + candidateBytes > MAX_RENDERED_ROOM_FEED_BYTES) {
      break;
    }
    retainedBytes += candidateBytes;
    start -= 1;
  }
  return events.slice(start);
}

export type RoomFeedAppendResult =
  | {
      status: "accepted";
      events: RoomFeedEvent[];
      cursor: RoomFeedCursor;
    }
  | { status: "duplicate" }
  | { status: "gap" | "epoch_reset" };

export function appendRoomFeedEvent(
  roomId: string,
  events: readonly RoomFeedEvent[],
  cursor: RoomFeedCursor | undefined,
  event: RoomFeedEvent,
): RoomFeedAppendResult {
  if (event.room_id !== roomId) {
    return { status: "gap" };
  }
  if (cursor) {
    if (cursor.epoch !== event.cursor.epoch) {
      return { status: "epoch_reset" };
    }
    if (event.cursor.sequence <= cursor.sequence) {
      return { status: "duplicate" };
    }
    if (event.cursor.sequence !== cursor.sequence + 1) {
      return { status: "gap" };
    }
  }
  const retained = retainRoomFeedWindow([...events, event]);
  return {
    status: "accepted",
    events: retained,
    cursor: event.cursor,
  };
}

/**
 * Retry horizon: the key survives until a confirmed stored result, renderer
 * reload, or FIFO eviction beyond 64 intents / 1 MiB of UTF-8 serialized
 * signatures. Reusing a key does not extend its place in that queue.
 * Reject a single oversized intent before IPC rather than send an unretained key.
 */
export function roomMessageIntent(
  pending: Map<string, string>,
  roomId: string,
  target: string,
  content: string,
): { signature: string; requestId: string } {
  const signature = JSON.stringify([roomId, target, content]);
  const signatureBytes = encoder.encode(signature).byteLength;
  if (signatureBytes > MAX_PENDING_ROOM_INTENT_BYTES) {
    throw new Error("Message is too large to retain a retry key (maximum 1 MiB including room and recipient details).");
  }
  let requestId = pending.get(signature);
  if (!requestId) {
    let retainedBytes = signatureBytes;
    for (const retained of pending.keys()) {
      retainedBytes += encoder.encode(retained).byteLength;
    }
    for (const oldest of pending.keys()) {
      if (pending.size < MAX_PENDING_ROOM_INTENTS && retainedBytes <= MAX_PENDING_ROOM_INTENT_BYTES) {
        break;
      }
      retainedBytes -= encoder.encode(oldest).byteLength;
      pending.delete(oldest);
    }
    requestId = crypto.randomUUID();
    pending.set(signature, requestId);
  }
  return { signature, requestId };
}

export function roomRequestFailure(error: unknown): { kind: "rejected" | "commit_uncertain" | "worker_failed" | "recovery_required" | "history_unavailable" | "request_conflict"; message: string } {
  if (typeof error === "object" && error !== null && "kind" in error && "message" in error &&
      (["rejected", "commit_uncertain", "worker_failed", "recovery_required", "history_unavailable", "request_conflict"].includes(String(error.kind))) && typeof error.message === "string") {
    return { kind: error.kind as ReturnType<typeof roomRequestFailure>["kind"], message: error.message };
  }
  return { kind: "worker_failed", message: error instanceof Error ? error.message : String(error) };
}

export function roomRequestFailureFeedback(error: unknown, retainsIntent = true): string {
  const failure = roomRequestFailure(error);
  if (failure.kind === "rejected") return `Not stored: ${failure.message}`;
  if (failure.kind === "request_conflict") return `An earlier request with this key is already stored with different content or recipients. Nothing new was stored. Check the feed before creating another message. ${failure.message}`;
  if (failure.kind === "history_unavailable") return `No new write attempted: ${failure.message}. Room writes are held until the preserved history is recovered. The earlier acknowledged feed and member terminals remain available.`;
  if (failure.kind === "recovery_required") return `No new write attempted: ${failure.message}. Resolve the storage error and wait for any active wake to settle; ${retainsIntent ? "retry the unchanged message to recover history with its original key" : "then Brief now can retry recovery before storing a new brief"}.`;
  if (!retainsIntent) return `Brief outcome unknown: ${failure.message}. Check the room feed before using Brief now again; another click creates a new brief.`;
  if (failure.kind === "commit_uncertain") return `Storage outcome unknown: ${failure.message}. Retry the unchanged message and recipients; history recovery runs before any new write.`;
  return `Request outcome unknown: ${failure.message}. Retry the unchanged message and recipients in this window to reuse the same request key.`;
}

/** Own the invoke lifecycle: only a result or a definite refusal retires a key. */
export async function submitRoomIntent<T>(pending: Map<string, string>, intent: ReturnType<typeof roomMessageIntent>,
  submit: (requestId: string) => Promise<T>): Promise<T> {
  try {
    const result = await submit(intent.requestId);
    pending.delete(intent.signature);
    return result;
  } catch (error) {
    if (roomRequestFailure(error).kind === "rejected") pending.delete(intent.signature);
    throw error;
  }
}

/** Claude, Codex and Grok get the whole message typed in (A106), as the live PRIM-1 does. */
export function roomWakeUnavailable(driver: DriverKind | undefined): string | null {
  if (driver === "prime" || driver === "generic_terminal") return "No verified wake for this pane in this build. Read the message in the PRIM-1 feed and provide context manually; Retry wake is unavailable.";
  return null;
}

export function roomWriterRecoveryMessage(room: Pick<RoomSnapshot, "writer_recovery"> | null | undefined): string | null {
  if (room?.writer_recovery?.kind !== "structural") return null;
  return `Room writes are held: ${room.writer_recovery.reason}. The preserved history needs recovery. Earlier acknowledged messages and member terminals remain available.`;
}

export function roomHistoryReplayMessage(room: Pick<RoomSnapshot, "history_replay_pending" | "history_restarted" | "writer_recovery"> | null | undefined): string | null {
  const notices = [
    room?.history_restarted ? "Earlier room history is unavailable. A new history was linked; room members and settings were preserved." : "",
    roomWriterRecoveryMessage(room) ?? (room?.history_replay_pending
      ? `Room history needs replay after an uncertain write.${room.writer_recovery?.reason ? ` Storage error: ${room.writer_recovery.reason}.` : ""} Resolve the storage error, then retry the unchanged message; Post, Send or Retry recovers history before any new write. Earlier wake outcomes are never repeated automatically.` : ""),
  ].filter(Boolean);
  return notices.length ? notices.join(" ") : null;
}

export function roomDeliveryRetryUnavailable(event: RoomFeedEvent, members: readonly string[],
  sessions: ReadonlyMap<string, Pick<SessionSnapshot, "run_id" | "generation" | "driver">>): string | null {
  if (event.item.kind !== "delivery") return null;
  const receipt = event.item;
  if (!members.includes(receipt.recipient_id)) return "Recipient is no longer in this room; this stored wake cannot be retried.";
  const current = sessions.get(receipt.recipient_id);
  if (current) {
    const unavailable = roomWakeUnavailable(current.driver);
    if (unavailable) return unavailable;
  }
  if (receipt.run_id && (!current || receipt.run_id !== current.run_id || receipt.generation !== current.generation)) {
    return "Recipient restarted since this wake; the message remains readable via room_read.";
  }
  return null;
}

export function roomRetryFeedback(result: RoomDeliveryResult, recipientId: string,
  recipientLabel: (id: string) => string, wakeUnavailable?: (id: string) => string | null): ReturnType<typeof roomDeliveryFeedback> {
  const receipts = result.receipts.filter((receipt) => receipt.recipient_id === recipientId);
  return roomDeliveryFeedback({ ...result, receipts, recipient_count: 1 }, recipientLabel, wakeUnavailable);
}

export function roomDeliveryStatusLabel(status: RoomDeliveryStatus): string {
  switch (status) {
    case "stored": return "Stored; typed in when the member is ready";
    // Pending proves a durable attempt, not that bytes are being written
    // (Reviewer a55d8454).
    case "pending": return "Wake attempt recorded; transport outcome pending";
    case "written": return "Wake written to terminal";
    case "woken": return "Wake notification confirmed";
    case "fetched": return "Fetched (page returned)";
    case "held": return "Held";
    case "failed": return "Failed";
  }
}

/** v3 W3 (Claude PRIM-1, 26a7f92 walk): a pointer the member no longer
    needed reads neutrally, never as a failure. */
export function roomDeliveryReceiptLabel(receipt: { status: RoomDeliveryStatus; already_read?: boolean;
  title_busy?: string | null; app_server_turn?: string | null }): string {
  // v3 W4 S2 (Claude PRIM-1 c01d84f8, 1214431f): the Codex App Server's success
  // response to this attempt's own request named a turn. Nothing was typed, so
  // no byte count and no "terminal"; never model acceptance, Fetched or ACK.
  if (receipt.app_server_turn && receipt.status === "written") {
    const accepted = `Accepted by the Codex App Server · turn ${receipt.app_server_turn}`;
    return receipt.already_read ? `${accepted}; all counted requests read or disposed of by receipt time` : accepted;
  }
  if (receipt.already_read && receipt.status === "failed") return "Not typed — nothing open to read";
  if (receipt.already_read && receipt.status === "written") return "Wake written to terminal; all counted requests read or disposed of by receipt time";
  // v3 W2 #2 (Claude PRIM-1, d9613ce walk): typed past the busy-title cap.
  if (receipt.title_busy && receipt.status === "written") {
    return `${roomDeliveryStatusLabel(receipt.status)}, typed while its title still read ${receipt.title_busy}`;
  }
  return roomDeliveryStatusLabel(receipt.status);
}

/** An already-read pointer that was not typed: neither failed nor retryable. */
export function deliveryNotNeeded(receipt: { status: RoomDeliveryStatus; already_read?: boolean }): boolean {
  return receipt.status === "failed" && receipt.already_read === true;
}

export function roomDeliveryRetryKey(roomId: string, messageId: string, recipientId: string): string {
  return JSON.stringify([roomId, messageId, recipientId]);
}

/** Only the latest receipt in the reconciled feed can authorize a retry. */
export function retryableRoomDeliveryEvents(
  roomId: string,
  events: readonly RoomFeedEvent[],
): Set<RoomFeedEvent> {
  const seen = new Set<string>();
  const eligible = new Set<RoomFeedEvent>();
  // Fetch evidence is retained even when a later in-flight wake writes a receipt.
  // v3 W3: a stored ACK or disposition ends the wake too; no Retry is offered.
  // v3 W3 (D6): a withdrawn message is never retried, for any recipient.
  const withdrawn = new Set<string>();
  for (const event of events) {
    if (event.room_id !== roomId) continue;
    if ((event.item.kind === "delivery" && event.item.status === "fetched") || event.item.kind === "ack") {
      seen.add(roomDeliveryRetryKey(roomId, event.item.message_id, event.item.recipient_id));
    }
    if (event.item.kind === "cancel") withdrawn.add(event.item.message_id);
  }
  for (let index = events.length - 1; index >= 0; index -= 1) {
    const event = events[index];
    const item = event.item;
    if (event.room_id !== roomId || item.kind !== "delivery") continue;
    const key = roomDeliveryRetryKey(roomId, item.message_id, item.recipient_id);
    if (seen.has(key) || withdrawn.has(item.message_id)) continue;
    // Every newer status supersedes the old receipt, including Fetched.
    seen.add(key);
    if (item.bytes_written === 0 && !deliveryNotNeeded(item) && (
      item.status === "held" || (item.status === "failed" && item.zero_write_confirmed === true)
    )) {
      eligible.add(event);
    }
  }
  return eligible;
}

export function roomDeliveryFeedback(
  result: RoomDeliveryResult,
  recipientLabel: (id: string) => string,
  wakeUnavailable: (id: string) => string | null = () => null,
): { summary: string; message: string; level: "info" | "warn" | "error" } {
  const details = result.receipts.map((receipt) => {
    const bytes = receipt.bytes_written > 0 ? `, ${receipt.bytes_written} bytes` : "";
    const reason = receipt.error ? ` (${receipt.error})` : "";
    const recording = receipt.recorded ? "" : " [receipt not saved]";
    const why = receipt.status === "stored" && (receipt.queued_because || receipt.waiting)
      ? ` (waiting: ${receipt.queued_because || receipt.waiting})` : "";
    return `${recipientLabel(receipt.recipient_id)}: ${roomDeliveryReceiptLabel(receipt)}${why}${bytes}${reason}${recording}`;
  }).join("; ");
  const replay = result.replayed ? " (previous request; no new wake attempted)" : "";
  const held = result.receipts.some((receipt) => receipt.status === "held");
  const failed = result.receipts.some((receipt) => receipt.status === "failed" && !deliveryNotNeeded(receipt));
  const incomplete = result.receipts.length !== result.recipient_count || result.receipts.some((receipt) =>
    !receipt.recorded || receipt.status === "stored" || receipt.status === "pending",
  );
  const retryable = result.receipts.some((receipt) => receipt.recorded && receipt.status === "held" && receipt.bytes_written === 0 && !wakeUnavailable(receipt.recipient_id));
  const unsupported = result.receipts.some((receipt) => receipt.status === "held" && wakeUnavailable(receipt.recipient_id));
  const unsaved = result.receipts.some((receipt) => !receipt.recorded);
  const guidance = [
    retryable ? "No automatic retry. Use Retry wake on an eligible recipient receipt; do not repost to all." : "",
    unsupported ? "Some members have no verified wake in this build. Read their messages in the PRIM-1 feed; prompt agent members to run room_read, or provide context manually." : "",
    unsaved ? "The wake receipt was not saved. Resolve the storage error and check the feed after history replay; do not repost the message to all." : "",
  ].filter(Boolean).join(" ");
  return {
    summary: [roomDeliverySummary(result),
      unsupported ? "Prompt held members manually." : "",
      retryable ? "Use Retry wake on eligible held receipts." : "",
      unsaved ? "Check storage and history; do not repost to all." : "",
    ].filter(Boolean).join(" "),
    message: `Stored in the room feed${replay}. At send: ${details || "No wake receipts available"}. Model acceptance is unconfirmed; the feed shows each recipient's current state.${guidance ? ` ${guidance}` : ""}`,
    level: failed ? "error" : held || incomplete ? "warn" : "info",
  };
}

/** Counts beside the composer. roomDeliveryFeedback adds the immediate action;
    full named receipts and reasons remain available through Details. */
function roomDeliverySummary(result: RoomDeliveryResult): string {
  const counts = new Map<RoomDeliveryStatus, number>();
  let notNeeded = 0;
  for (const receipt of result.receipts) {
    if (deliveryNotNeeded(receipt)) { notNeeded += 1; continue; }
    counts.set(receipt.status, (counts.get(receipt.status) ?? 0) + 1);
  }
  const outcomes: string[] = [];
  for (const [status, singular, plural] of [
    ["written", "wake written", "wakes written"],
    ["woken", "wake notification confirmed", "wake notifications confirmed"],
    ["fetched", "fetched", "fetched"],
    ["held", "held", "held"],
    ["failed", "failed", "failed"],
  ] as const) {
    const count = counts.get(status) ?? 0;
    if (count) outcomes.push(`${count} ${count === 1 ? singular : plural}`);
  }
  if (notNeeded) outcomes.push(`${notNeeded} not typed, already read`);
  // A queued recipient is waiting for readiness, not an unknown outcome
  // (Claude PRIM-1 046a70f4 C).
  const queued = counts.get("stored") ?? 0;
  if (queued) outcomes.push(`${queued} queued until ready`);
  const attempted = counts.get("pending") ?? 0;
  if (attempted) outcomes.push(`${attempted} wake attempt${attempted === 1 ? "" : "s"} recorded, outcome pending`);
  const missing = result.recipient_count - result.receipts.length;
  if (missing > 0) outcomes.push(`${missing} wake outcome${missing === 1 ? "" : "s"} unavailable`);
  if (missing < 0) outcomes.push("receipt count mismatch");
  const partial = result.receipts.filter((receipt) =>
    (receipt.status === "failed" || receipt.status === "held") && receipt.bytes_written > 0).length;
  if (partial) outcomes.push(`${partial} partial write${partial === 1 ? "" : "s"}`);
  const unsaved = result.receipts.filter((receipt) => !receipt.recorded).length;
  if (unsaved) outcomes.push(`${unsaved} receipt${unsaved === 1 ? "" : "s"} not saved`);
  const stored = result.replayed ? "Previously stored; no new wake attempted." : "Stored in the room feed.";
  // The counts are the outcome at send and are never updated; the feed shows
  // each recipient's current state (Claude PRIM-1 26a7f92 walk, finding 4).
  return outcomes.length ? `${stored} At send: ${outcomes.join(" · ")}.` : `${stored} No wake receipts available.`;
}
