import type { RoomDeliveryStatus, RoomFeedEvent, RoomFeedItem, RoomMessageSender } from "./types";
import { PRIM1_NOTICE_PREFIX, PRIM1_POST_REQUEST_PREFIX } from "./work-context";
import { roomFallbackSenderLabel, roomNoticeSenderLabel, roomRecoverySenderLabel, roomWithdrawalNotice,
  roomWithdrawalText } from "./room-feed";

type DeliveryItem = Extract<RoomFeedItem, { kind: "delivery" }>;

/** The room feed's two views (A52, A53). Readable, the default, shows each
    message as a card in a reading font: delivery receipts are folded away and
    only a held or failed wake stays visible, on its message. Raw is the stored
    event stream exactly as before — the developer view. */
export type RoomFeedView = "readable" | "raw";

export const ROOM_FEED_VIEW_KEY = "prim1-room-feed-view";

/** A stored choice; anything missing or unknown opens Readable. */
export function parseRoomFeedView(raw: string | null): RoomFeedView {
  return raw === "raw" ? "raw" : "readable";
}

/** Altitude first (Law XIV): a message longer than 16 lines shows its first
    12, with Show all one click away. Lines are estimated, not measured — each
    source line takes ceil(length / 120) rows, at least one — so the cut is the
    same in every window size and theme, and testable. */
export const READABLE_PREVIEW_LINES = 12;
export const READABLE_COLLAPSE_ABOVE_LINES = 16;
export const READABLE_LINE_CHARS = 120;
/** A long backend reason is cut to this on a warning line; the full reason is the line's tooltip. */
export const READABLE_REASON_CHARS = 160;

export interface ReadablePreview {
  text: string;
  collapsed: boolean;
}

export interface ReadableDelivery {
  recipientId: string;
  /** Fetched is final (the recipient read the message), as for Retry wake;
      otherwise the newest receipt decides. No receipt yet means Stored. */
  status: RoomDeliveryStatus;
  /** The deciding receipt. For Held and Failed it is the newest receipt —
      the event Raw offers Retry wake on. */
  receipt: RoomFeedEvent | null;
}

/** What a held or failed wake left in the recipient's terminal: nothing,
    some bytes, or possibly something (no zero-write proof). */
export type ReadableTerminalInput = "none" | "partial" | "unknown";

export interface ReadableProblem {
  /** Unique within its message: the recipient, and whether the line is about
      terminal input ("input") or a standing held or failed wake ("wake"). */
  key: string;
  recipientId: string;
  recipient: string;
  status: "held" | "failed";
  bytesWritten: number;
  error: string | null;
  input: ReadableTerminalInput;
  /** The recipient has read the message, so nothing about it is retried. */
  fetched: boolean;
  /** The receipt this line describes; Retry wake is offered only if the
      existing rules make it eligible. */
  receipt: RoomFeedEvent;
}

/** v3 W6: where one recipient stands with one message, in plain words. */
export interface ReadableRecipientState {
  recipientId: string;
  recipient: string;
  state: "acked" | "declined" | "read" | "waiting" | "written" | "woken" | "being woken" | "stored" | "held" | "failed"
    | "withdrawn";
  /** One short line: the ACK note, the waiting code, a hold reason that a
      later read did not clear. */
  detail: string;
  /** The full text behind `detail`, for the tooltip. */
  title: string;
}

export type ReadableFeedItem =
  | {
      kind: "message";
      /** The message id: stable across feed reloads. */
      key: string;
      /** "prim1" only for PRIM-1's own operator posts — never for a member.
          "guest" is an invited outside agent: its name is shown with a badge. */
      author: "you" | "member" | "prim1" | "guest";
      sender: string;
      /** The member who wrote it, when a member did (Georgi themes' avatar). */
      senderId?: string;
      recipients: string[];
      body: string;
      preview: ReadablePreview;
      timestamp: string;
      deliveries: ReadableDelivery[];
      problems: ReadableProblem[];
      /** v3 W6: one entry per recipient, in recipient order. */
      states: ReadableRecipientState[];
    }
  | { kind: "note"; key: string; text: string; timestamp: string }
  /** A problem whose message left the bounded window, shown where its receipt is. */
  | { kind: "problem"; key: string; problem: ReadableProblem; timestamp: string };

export interface ReadableFeed {
  items: ReadableFeedItem[];
  /** Events with no line of their own: receipts, folded into their message. */
  hidden: number;
}

const displayBodies = new WeakMap<RoomFeedEvent, { body: string; preview: ReadablePreview }>();

/** One recipient of one message, as its receipts leave it. */
interface RecipientReceipts {
  /** Fetched is final, as for Retry wake; otherwise the newest receipt. */
  deciding: RoomFeedEvent;
  /** The newest held or failed receipt that left input, or may have, in the
      recipient's terminal. Fetched does not clear it: a fetch can race an
      admitted write, and reading the message does not undo partial input. */
  input: RoomFeedEvent | null;
}

/** What a held or failed wake left in the terminal. A 0-byte Failed without
    zero-write proof (legacy, unknown progress, or proof withheld after a
    fetch) may have left input. */
export function terminalInput(item: DeliveryItem): ReadableTerminalInput {
  if (item.status !== "held" && item.status !== "failed") return "none";
  if (item.bytes_written > 0) return "partial";
  // v3 W3: an already-read pointer was refused before any byte.
  if (item.already_read === true) return "none";
  return item.status === "failed" && item.zero_write_confirmed !== true ? "unknown" : "none";
}

/** Turn the bounded feed window into what a person reads: messages with their
    standing wake problems and any terminal input a wake left, membership
    notes, and nothing else. */
export function readableRoomFeed(
  events: readonly RoomFeedEvent[],
  labelFor: (sessionId: string) => string,
  typedCardsPresent = false,
): ReadableFeed {
  const messageIds = new Set<string>();
  const receipts = new Map<string, Map<string, RecipientReceipts>>();
  // v3 W6: the latest Waiting since the last receipt, the stored disposition,
  // and the newest hold reason no later write superseded (a read never does).
  const waiting = new Map<string, { code: string; reason: string }>();
  const acks = new Map<string, Extract<RoomFeedItem, { kind: "ack" }>>();
  const holds = new Map<string, string>();
  const observed = new Set<string>();
  // v3 W3 (D6): withdrawn messages and who withdrew them, and why.
  const withdrawn = new Map<string, Extract<RoomFeedItem, { kind: "cancel" }>>();
  for (const event of events) {
    const item = event.item;
    if (item.kind === "cancel") withdrawn.set(item.message_id, item);
    if (item.kind === "input_observed") {
      const key = `${item.message_id}:${item.recipient_id}`;
      observed.add(key);
      if (waiting.get(key)?.code.startsWith("submit_")) waiting.delete(key);
    }
    if (item.kind === "waiting") {
      waiting.set(`${item.message_id}:${item.recipient_id}`, { code: item.reason_code, reason: item.reason });
    } else if (item.kind === "ack") {
      acks.set(`${item.message_id}:${item.recipient_id}`, item);
    }
    if (item.kind === "delivery") {
      const key = `${item.message_id}:${item.recipient_id}`;
      if (item.status !== "fetched") { waiting.delete(key); observed.delete(key); }
      if (item.status === "held") holds.set(key, item.error ?? "held");
      if (item.status === "written" || item.status === "woken") holds.delete(key);
    }
    if (item.kind === "message") {
      messageIds.add(item.message_id);
    } else if (item.kind === "delivery") {
      let byRecipient = receipts.get(item.message_id);
      if (!byRecipient) {
        byRecipient = new Map();
        receipts.set(item.message_id, byRecipient);
      }
      const current = byRecipient.get(item.recipient_id);
      byRecipient.set(item.recipient_id, {
        deciding: current && statusOf(current.deciding) === "fetched" ? current.deciding : event,
        input: terminalInput(item) === "none" ? current?.input ?? null : event,
      });
    }
  }

  // Problems of messages outside the window show where their receipt is.
  const orphans = new Map<RoomFeedEvent, ReadableProblem[]>();
  for (const [messageId, byRecipient] of receipts) {
    if (messageIds.has(messageId)) continue;
    for (const [recipientId, entry] of byRecipient) {
      for (const problem of problemsFor(recipientId, entry, labelFor)) {
        orphans.set(problem.receipt, [...(orphans.get(problem.receipt) ?? []), problem]);
      }
    }
  }

  const items: ReadableFeedItem[] = [];
  for (const event of events) {
    const item = event.item;
    if (item.kind === "message") {
      const nativeSender = roomNoticeSenderLabel(item) ?? roomRecoverySenderLabel(item) ?? roomFallbackSenderLabel(item);
      const automatic = nativeSender === null && item.sender.kind === "operator"
        && typeof item.request_id === "string"
        && item.request_id.startsWith(PRIM1_POST_REQUEST_PREFIX);
      const byRecipient = receipts.get(item.message_id);
      const deliveries = deliveriesFor(item.recipient_ids, byRecipient);
      const cancel = withdrawn.get(item.message_id);
      // A withdrawn message raises no wake problem; it says who withdrew it.
      const problems = cancel ? [] : deliveries.flatMap((delivery) =>
        problemsFor(delivery.recipientId, byRecipient?.get(delivery.recipientId), labelFor));
      if (typedCardsPresent && item.sender.kind === "session") {
        for (const problem of problems) items.push({ kind: "problem",
          key: `problem:${item.message_id}:${problem.key}`, problem, timestamp: event.timestamp });
        // v3 W6 (Reviewer ad24c525): a result card never hides an open
        // obligation; an addressed request keeps one compact status line.
        const states = deliveries.map((delivery) => recipientState(item.message_id, delivery, labelFor, waiting, acks, holds, observed,
          withdrawn.has(item.message_id)));
        if (item.ack_requested && states.some((state) => state.state !== "acked" && state.state !== "declined"
          && state.state !== "withdrawn")) {
          items.push({ kind: "note", key: `status:${item.message_id}`, timestamp: event.timestamp,
            text: `${readableSender(item.sender, labelFor)} asked ${readableRecipients(item.recipient_ids.map(labelFor)).replace(/^to /, "")}: ${readableStatesText(states)}` });
        }
        continue;
      }
      items.push({
        kind: "message",
        key: item.message_id,
        author: nativeSender !== null || automatic ? "prim1" : readableAuthor(item.sender),
        sender: nativeSender ?? (automatic ? "PRIM-1" : readableSender(item.sender, labelFor)),
        ...(item.sender.kind === "session" && nativeSender === null && !automatic ? { senderId: item.sender.session_id } : {}),
        recipients: item.recipient_ids.map(labelFor),
        ...displayBody(event, item.content, automatic),
        timestamp: event.timestamp,
        deliveries,
        problems,
        states: deliveries.map((delivery) => recipientState(item.message_id, delivery, labelFor, waiting, acks, holds, observed,
          cancel !== undefined)),
      });
      if (cancel) {
        const by = cancel.actor.kind === "operator" ? "You" : cancel.actor.kind === "local_process"
          ? cancel.actor.label : cancel.actor.kind === "prim1" ? "PRIM-1" : labelFor(cancel.actor.session_id);
        const outcome = roomWithdrawalText(cancel, labelFor, roomWithdrawalNotice(events, cancel.request_id));
        items.push({ kind: "note", key: `withdrawn:${item.message_id}`, timestamp: event.timestamp,
          text: `${by} withdrew this message: ${cancel.reason}${outcome ? ` ${outcome}` : ""}` });
      }
    } else if (item.kind === "membership") {
      items.push({
        kind: "note",
        key: `${event.cursor.epoch}:${event.cursor.sequence}`,
        text: `${labelFor(item.session_id)} ${item.action === "joined" ? "joined the room" : "was removed from the room"}`,
        timestamp: event.timestamp,
      });
    } else if (item.kind === "delivery") {
      for (const problem of orphans.get(event) ?? []) {
        items.push({
          kind: "problem",
          key: `problem:${item.message_id}:${problem.key}`,
          problem,
          timestamp: event.timestamp,
        });
      }
    }
  }
  return { items, hidden: events.length - items.length };
}

function recipientState(
  messageId: string,
  delivery: ReadableDelivery,
  labelFor: (sessionId: string) => string,
  waiting: ReadonlyMap<string, { code: string; reason: string }>,
  acks: ReadonlyMap<string, Extract<RoomFeedItem, { kind: "ack" }>>,
  holds: ReadonlyMap<string, string>,
  observed: ReadonlySet<string> = new Set(),
  withdrawn = false,
): ReadableRecipientState {
  const key = `${messageId}:${delivery.recipientId}`;
  const recipient = labelFor(delivery.recipientId);
  const base = { recipientId: delivery.recipientId, recipient };
  // v3 W3 (D6): a withdrawn message owes nothing; a disposition made before
  // the withdrawal still shows as it was.
  const ack = acks.get(key);
  if (ack) {
    const by = ack.actor.kind === "operator" ? " (by you)"
      : ack.actor.kind === "local_process" ? ` (by ${ack.actor.label})`
      : ack.actor.kind === "prim1" ? " (by PRIM-1)" : "";
    return { ...base, state: ack.disposition === "acked" ? "acked" : "declined",
      detail: `${ack.note}${by}`, title: ack.outcome_ref ? `${ack.note} · ${ack.outcome_ref}` : ack.note };
  }
  if (withdrawn) return { ...base, state: "withdrawn", detail: "withdrawn; nothing owed", title: "" };
  const hold = holds.get(key);
  if (delivery.status === "fetched") {
    // Reading a message never clears why its wake was held (W6, b5deac90).
    return hold
      ? { ...base, state: "read", detail: `read; its wake was held: ${readableReason(hold)}`, title: hold }
      : { ...base, state: "read", detail: "read, not yet acknowledged", title: "" };
  }
  const wait = waiting.get(key);
  // A current wait outranks the transport status it follows, also after a
  // confirmed-zero Failed that is waiting to be re-admitted (Reviewer 585e0423).
  if (wait && delivery.status !== "held") {
    return { ...base, state: "waiting", detail: `waiting: ${wait.code}`, title: wait.reason };
  }
  switch (delivery.status) {
    case "held": return { ...base, state: "held", detail: hold ? readableReason(hold) : "held", title: hold ?? "" };
    // An unneeded pointer (everything it counted was read or withdrawn) is no failure.
    case "failed": return delivery.receipt?.item.kind === "delivery" && delivery.receipt.item.already_read
      ? { ...base, state: "stored", detail: "not typed: nothing open to read", title: "" }
      : { ...base, state: "failed", detail: "wake failed", title: "" };
    // Written is bytes and CR in the terminal, never submission (W3 #9f).
    case "written": {
      // v3 W2 #2 (Claude PRIM-1, d9613ce walk): typed past the busy-title cap.
      const busy = delivery.receipt?.item.kind === "delivery" && delivery.receipt.item.title_busy
        ? `; typed while its title still read ${delivery.receipt.item.title_busy}` : "";
      // v3 W4 S2: accepted by the Codex App Server, not typed into a terminal.
      const turn = delivery.receipt?.item.kind === "delivery" ? delivery.receipt.item.app_server_turn : undefined;
      if (turn) {
        return observed.has(key)
          ? { ...base, state: "written", detail: "accepted by the Codex App Server (input observed), not yet read",
            title: `The Codex App Server answered this request with turn ${turn}, and the member's input was observed; that is not an acknowledgement.` }
          : { ...base, state: "written", detail: "accepted by the Codex App Server, not yet read",
            title: `The Codex App Server answered this request with turn ${turn}; that is not model acceptance or an acknowledgement.` };
      }
      return observed.has(key)
        ? { ...base, state: "written", detail: `submitted (input observed), not yet read${busy}`,
          title: "The member's own record shows this pointer as a prompt; that is not an acknowledgement." }
        : { ...base, state: "written", detail: `written; submission unconfirmed, not yet read${busy}`,
        title: "The pointer reached the member's terminal; PRIM-1 has not confirmed that the member submitted it." };
    }
    case "woken": return { ...base, state: "woken", detail: "woken, not yet read", title: "" };
    case "pending": return { ...base, state: "being woken", detail: "being woken", title: "" };
    default: return { ...base, state: "stored", detail: "stored", title: "" };
  }
}

/** "Codex J1: read · Claude J1: ACK — repro received". */
export function readableStatesText(states: readonly ReadableRecipientState[]): string {
  return states.map((state) => {
    const word = state.state === "acked" ? "ACK" : state.state === "declined" ? "declined" : "";
    return word ? `${state.recipient}: ${word} — ${state.detail}` : `${state.recipient}: ${state.detail}`;
  }).join(" · ");
}

function readableAuthor(sender: RoomMessageSender): "you" | "member" | "guest" {
  return sender.kind === "operator" ? "you" : sender.kind === "guest" ? "guest" : "member";
}

/** A guest keeps the name it posted under, even after it was revoked. */
export function readableSender(sender: RoomMessageSender, labelFor: (sessionId: string) => string): string {
  switch (sender.kind) {
    case "operator":
      return "You";
    case "guest":
    case "local_process":
      return sender.label;
    case "session":
      return labelFor(sender.session_id);
    case "prim1":
      return "PRIM-1";
  }
}

function statusOf(event: RoomFeedEvent): RoomDeliveryStatus | null {
  return event.item.kind === "delivery" ? event.item.status : null;
}

function deliveriesFor(
  recipientIds: readonly string[],
  receipts: ReadonlyMap<string, RecipientReceipts> | undefined,
): ReadableDelivery[] {
  const ids = [...recipientIds];
  for (const recipientId of receipts?.keys() ?? []) {
    if (!ids.includes(recipientId)) ids.push(recipientId);
  }
  return ids.map((recipientId) => {
    const receipt = receipts?.get(recipientId)?.deciding ?? null;
    return { recipientId, status: (receipt && statusOf(receipt)) ?? "stored", receipt };
  });
}

/** At most a terminal-input line (which survives Fetched, never with Retry)
    and a standing held or failed wake (only before Fetched). They are one
    line whenever the same receipt is both. */
function problemsFor(
  recipientId: string,
  entry: RecipientReceipts | undefined,
  labelFor: (sessionId: string) => string,
): ReadableProblem[] {
  if (!entry) return [];
  const fetched = statusOf(entry.deciding) === "fetched";
  const problems: ReadableProblem[] = [];
  if (entry.input) {
    problems.push(problemOf(recipientId, "input", entry.input, fetched, labelFor));
  }
  const standing = statusOf(entry.deciding);
  // v3 W3: an already-read pointer that was not typed is no wake problem.
  const notNeeded = entry.deciding.item.kind === "delivery" && entry.deciding.item.already_read === true;
  if (!fetched && !notNeeded && (standing === "held" || standing === "failed") && entry.deciding !== entry.input) {
    problems.push(problemOf(recipientId, "wake", entry.deciding, false, labelFor));
  }
  return problems;
}

function problemOf(
  recipientId: string,
  kind: "input" | "wake",
  receipt: RoomFeedEvent,
  fetched: boolean,
  labelFor: (sessionId: string) => string,
): ReadableProblem {
  const item = receipt.item as DeliveryItem;
  return {
    key: `${recipientId}:${kind}`,
    recipientId,
    recipient: labelFor(recipientId),
    status: item.status === "failed" ? "failed" : "held",
    bytesWritten: item.bytes_written,
    error: item.error,
    input: terminalInput(item),
    fetched,
    receipt,
  };
}

function displayBody(
  event: RoomFeedEvent,
  content: string,
  automatic: boolean,
): { body: string; preview: ReadablePreview } {
  const cached = displayBodies.get(event);
  if (cached) return cached;
  let body = content.replace(/\r\n?/g, "\n");
  // The card already names PRIM-1; the agents keep the marker in Raw.
  if (automatic && body.startsWith(PRIM1_NOTICE_PREFIX)) {
    body = body.slice(PRIM1_NOTICE_PREFIX.length).trimStart();
  }
  const display = { body, preview: readablePreview(body) };
  displayBodies.set(event, display);
  return display;
}

function rowsFor(length: number): number {
  return Math.max(1, Math.ceil(length / READABLE_LINE_CHARS));
}

/** Estimated rows, counted only until they exceed `stopAbove`. */
function estimatedRows(content: string, stopAbove: number): number {
  let rows = 0;
  let start = 0;
  for (;;) {
    const newline = content.indexOf("\n", start);
    rows += rowsFor((newline === -1 ? content.length : newline) - start);
    if (newline === -1 || rows > stopAbove) return rows;
    start = newline + 1;
  }
}

export function readablePreview(content: string): ReadablePreview {
  if (estimatedRows(content, READABLE_COLLAPSE_ABOVE_LINES) <= READABLE_COLLAPSE_ABOVE_LINES) {
    return { text: content, collapsed: false };
  }
  let used = 0;
  let start = 0;
  let cut = 0;
  let midLine = false;
  while (used < READABLE_PREVIEW_LINES) {
    const newline = content.indexOf("\n", start);
    const end = newline === -1 ? content.length : newline;
    const rows = rowsFor(end - start);
    if (used + rows <= READABLE_PREVIEW_LINES) {
      used += rows;
      cut = end;
      if (newline === -1) break;
      start = newline + 1;
      continue;
    }
    // The next line only partly fits: fill the remaining rows, ending on a word.
    const room = (READABLE_PREVIEW_LINES - used) * READABLE_LINE_CHARS;
    cut = start + room;
    const space = content.lastIndexOf(" ", cut);
    if (space > start + room / 2) {
      cut = space;
    } else if (isHighSurrogate(content.charCodeAt(cut - 1))) {
      cut -= 1;
    }
    midLine = true;
    break;
  }
  return { text: content.slice(0, cut).trimEnd() + (midLine ? " …" : ""), collapsed: true };
}

function isHighSurrogate(code: number): boolean {
  return code >= 0xd800 && code <= 0xdbff;
}

/** "to A, B, C"; past three names, "to A, B and 2 more". Empty for a feed-only post. */
export function readableRecipients(labels: readonly string[]): string {
  if (labels.length === 0) return "";
  if (labels.length <= 3) return `to ${labels.join(", ")}`;
  return `to ${labels.slice(0, 2).join(", ")} and ${labels.length - 2} more`;
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** Clock time, local and 24-hour: "09:05" today, "13 Oct, 21:04" this year,
    "31 Dec 2025, 23:59" before. A clock time never goes stale between renders
    the way "5 min ago" would. Empty for an unreadable timestamp. */
export function readableTime(timestamp: string, now: Date): string {
  const at = new Date(timestamp);
  if (Number.isNaN(at.getTime())) return "";
  const clock = `${String(at.getHours()).padStart(2, "0")}:${String(at.getMinutes()).padStart(2, "0")}`;
  if (at.getFullYear() === now.getFullYear() && at.getMonth() === now.getMonth() && at.getDate() === now.getDate()) {
    return clock;
  }
  const day = `${at.getDate()} ${MONTHS[at.getMonth()]}`;
  return at.getFullYear() === now.getFullYear() ? `${day}, ${clock}` : `${day} ${at.getFullYear()}, ${clock}`;
}

/** The warning line. Terminal input comes first — it stays true after the
    recipient reads the message. A held wake is a safe refusal: the message is
    stored, and its reason is the line's tooltip. */
export function readableProblemText(problem: ReadableProblem, orphan = false): string {
  const lead = orphan ? "Earlier message: " : "";
  const reason = problem.error ? `: ${readableReason(problem.error)}` : ".";
  const who = problem.recipient;
  if (problem.input === "partial") {
    const bytes = `${problem.bytesWritten} byte${problem.bytesWritten === 1 ? "" : "s"}`;
    const verb = problem.status === "failed" ? "failed" : "was held";
    return problem.fetched
      ? `${lead}${who} has read the message, but a wake left ${bytes} of input in its terminal${reason}`
      : `${lead}A wake to ${who} ${verb} partway; ${bytes} of input reached its terminal${reason}`;
  }
  if (problem.input === "unknown") {
    return problem.fetched
      ? `${lead}${who} has read the message, but a failed wake may have left input in its terminal${reason}`
      : `${lead}A wake to ${who} failed and may have left input in its terminal${reason}`;
  }
  if (problem.status === "held") {
    return `${lead}${who} was not woken; prompt manually or use an available Retry wake.`;
  }
  return `${lead}Wake to ${who} failed${reason}`;
}

export function readableReason(error: string): string {
  const flat = error.replace(/\s+/g, " ").trim();
  return flat.length > READABLE_REASON_CHARS
    ? `${flat.slice(0, READABLE_REASON_CHARS - 1).trimEnd()}…`
    : flat;
}
