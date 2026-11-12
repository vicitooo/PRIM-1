/* Georgi themes: what in the rooms is waiting on the operator.

   Agents talk to each other all day; a message for the human has to stand
   out. An agent flags one by starting it with @Georgi (the room brief asks
   for that), and a question or decision card counts the same way. This
   module finds those asks in a room's events and decides which are still
   open. Pure: no DOM, no Tauri, so it is tested on its own. */

import type { RoomFeedEvent } from "./types";

export const OPERATOR_HANDLE = "Georgi";

/** "@Georgi", "**@Georgi**", "@operator" at the start of the message, after
    any "> " reply quote; a colon, comma or dash after it is dropped. */
const MENTION = /^\**@(georgi|operator)\b\**\s*[:,—-]?\s*/i;

/** The message without its leading "> " reply-quote lines. */
export function withoutReplyQuote(content: string): string {
  const lines = content.replace(/\r\n?/g, "\n").split("\n");
  let i = 0;
  while (i < lines.length && (lines[i].trim().startsWith(">") || (i > 0 && !lines[i].trim() && lines[i - 1].trim().startsWith(">")))) i += 1;
  while (i < lines.length && !lines[i].trim()) i += 1;
  return lines.slice(i).join("\n");
}

/** The request when a message is addressed to the operator, else null:
    the first line after the mention (or the next line when the mention
    stands alone), trimmed to one readable line. */
export function operatorRequest(content: string): string | null {
  const body = withoutReplyQuote(content);
  const match = body.match(MENTION);
  if (!match) return null;
  const rest = body.slice(match[0].length).split("\n").map((line) => line.trim()).filter(Boolean);
  const first = (rest[0] ?? "").replace(/^\*\*(.+?)\*\*$/, "$1");
  return (first || "needs you").slice(0, 240);
}

/** The message with its @Georgi mention removed, for display. */
export function stripOperatorMention(content: string): string {
  const text = content.replace(/\r\n?/g, "\n");
  const body = withoutReplyQuote(text);
  return text.slice(0, text.length - body.length) + body.replace(MENTION, "");
}

export type AskKind = "mention" | "question" | "decision";

export interface Ask {
  /** Stable across reloads: message id, or card author + key. */
  key: string;
  roomId: string;
  kind: AskKind;
  /** Session id of the asking member, when a member asked. */
  senderId: string | null;
  sender: string;
  text: string;
  timestamp: string;
  messageId: string | null;
}

/** Open asks in one room, oldest first.
    - A member message starting with @Georgi opens one.
    - An open question or decision card opens one; the same card marked done
      closes it.
    - Your reply closes a mention ask: a message sent to the asker (alone or
      to everyone) after the ask. A plain post to the feed does not.
    - Anything in `dismissed` (you clicked Done) stays closed. */
export function openAsks(
  roomId: string,
  events: readonly RoomFeedEvent[],
  labelFor: (sessionId: string) => string,
  dismissed: ReadonlySet<string>,
): Ask[] {
  const open = new Map<string, Ask>();
  for (const event of events) {
    const item = event.item;
    if (item.kind === "message") {
      const sender = item.sender;
      if (sender.kind === "operator") {
        // A message sent to the asker (alone or with everyone) answers it.
        // A plain post to the feed does not: it may be about anything.
        for (const [key, ask] of open) {
          if (ask.kind !== "mention") continue;
          if (ask.senderId !== null && item.recipient_ids.includes(ask.senderId)) open.delete(key);
        }
        continue;
      }
      if (sender.kind !== "session" && sender.kind !== "guest") continue;
      const request = operatorRequest(item.content);
      if (request === null) continue;
      open.set(item.message_id, {
        key: item.message_id,
        roomId,
        kind: "mention",
        senderId: sender.kind === "session" ? sender.session_id : null,
        sender: sender.kind === "session" ? labelFor(sender.session_id) : sender.label,
        text: request,
        timestamp: event.timestamp,
        messageId: item.message_id,
      });
    } else if (item.kind === "card") {
      const card = item.card;
      if (card.kind !== "question" && card.kind !== "decision") continue;
      const key = `card:${item.author}:${card.key}`;
      if (card.state === "done") {
        open.delete(key);
        continue;
      }
      open.set(key, {
        key,
        roomId,
        kind: card.kind,
        senderId: item.author,
        sender: labelFor(item.author),
        text: card.title || card.summary,
        timestamp: event.timestamp,
        messageId: null,
      });
    }
  }
  return [...open.values()].filter((ask) => !dismissed.has(ask.key));
}
