import { afterEach, describe, expect, it, vi } from "vitest";
import { RoomCardIndex, buildTypedCard, cardKey, cardReply, disposeTypedCard, selectionTouchesNode, sparklinePoints, updateTypedCard, type CardEvent } from "./room-cards";
import { readableRoomFeed } from "./readable-feed";
import type { RoomFeedEvent } from "./types";

function card(sequence: number, key = `topic${sequence}`, pinned = false, author = "a"): CardEvent {
  return { schema_version: 1, room_id: "room", cursor: { epoch: "epoch", sequence }, timestamp: "2026-10-14T07:00:00Z",
    item: { kind: "card", card_id: `id${sequence}`, request_id: `request${sequence}`, author,
      card: { key, kind: pinned ? "decision" : "report", title: `Title ${sequence}`, summary: `Summary ${sequence}`,
        body: `Body ${sequence}`, state: "open", data: {} } } };
}

describe("room card projection", () => {
  it("recognizes a selected middle card even when the range endpoints are in other cards", () => {
    const middle = { contains: () => false } as unknown as Node;
    const selection = { isCollapsed: false, anchorNode: {} as Node, focusNode: {} as Node,
      rangeCount: 1, getRangeAt: () => ({ intersectsNode: (node: Node) => node === middle }) } as unknown as Selection;
    expect(selectionTouchesNode(middle, selection)).toBe(true);
  });
  it("retains state outside raw events, prevents stale snapshot resurrection and keeps update position", () => {
    const index = new RoomCardIndex();
    const first = card(2, "topic"); const second = card(3, "topic", false, "b"); const update = card(20, "topic");
    index.snapshot([update, second], { epoch: "epoch", sequence: 22 });
    index.accept(first); index.accept(card(21, "evicted"));
    expect(index.cards).toEqual([update, second]);
    index.accept(card(23, "topic"));
    expect(index.cards.map(cardKey)).toEqual([cardKey(first), cardKey(second)]);
    expect(index.cards[0].item.card_id).toBe("id23");
    expect(index.cursor?.sequence).toBe(23);
  });

  it("caps keys, pins open decisions and makes every retained key reachable in 16-card pages", () => {
    const index = new RoomCardIndex();
    index.accept(card(1, "pinned", true));
    for (let n = 2; n <= 70; n++) index.accept(card(n));
    expect(index.cards).toHaveLength(64);
    expect(index.cards[0].item.card.key).toBe("pinned");
    expect(index.page(0).cards).toHaveLength(16);
    expect(index.page(0).cards[0].item.card.key).toBe("pinned");
    const all = [0, 1, 2, 3].flatMap((n) => index.page(n).cards.map(cardKey));
    expect(new Set(all).size).toBe(64);
    expect(index.page(999).page).toBe(3);
  });

  it("refuses all-pinned overflow and bounds serialized bytes", () => {
    const pinned = new RoomCardIndex();
    for (let n = 1; n <= 64; n++) pinned.accept(card(n, `p${n}`, true));
    expect(() => pinned.accept(card(65))).toThrow("capacity");
    expect(pinned.cards).toHaveLength(64);
    const index = new RoomCardIndex();
    for (let n = 1; n <= 50; n++) {
      const image = card(n); image.item.card.kind = "image";
      image.item.card.data = { png_base64: "A".repeat(100_000), alt: "Bounded data" };
      index.accept(image);
    }
    expect(index.cards.length).toBeLessThan(22);
    expect(new TextEncoder().encode(JSON.stringify(index.cards)).length).toBeLessThan(2 * 1024 * 1024);
  });

  it("prepares an explicit reply target without changing card state", () => {
    const event = card(1, "question");
    expect(cardReply(event)).toEqual({ target: "a", text: "Re: Title 1 [card:id1]\n\n" });
    expect(event.item.card.state).toBe("open");
  });

  it("emits only finite numeric sparkline coordinates", () => {
    expect(sparklinePoints([10, 20, 15])).toBe("0.00,36.00 80.00,4.00 160.00,20.00");
    expect(sparklinePoints([10])).toBe("0.00,20.00");
    expect(sparklinePoints([NaN])).toBe("");
    expect(sparklinePoints([Infinity])).toBe("");
    expect(sparklinePoints(Array(61).fill(1))).toBe("");
  });

  it("hides member chatter but keeps operator posts and partial/unknown input warnings after Fetched", () => {
    const message: RoomFeedEvent = { ...card(1), item: { kind: "message", message_id: "message", request_id: "post",
      sender: { kind: "session", session_id: "a" }, content: "member chatter", recipient_ids: ["b"], membership_revision: 1 } };
    const partial: RoomFeedEvent = { ...card(2), item: { kind: "delivery", message_id: "message", recipient_id: "b",
      status: "failed", bytes_written: 30, error: "partial input", run_id: "run", generation: 1 } };
    const fetched: RoomFeedEvent = { ...partial, cursor: { epoch: "epoch", sequence: 3 }, item: { ...partial.item as Extract<RoomFeedEvent["item"], { kind: "delivery" }>, status: "fetched", bytes_written: 0, error: null } };
    const operator: RoomFeedEvent = { ...card(4), item: { ...message.item as Extract<RoomFeedEvent["item"], { kind: "message" }>, message_id: "operator", sender: { kind: "operator" }, content: "Operator direction" } };
    const view = readableRoomFeed([message, partial, fetched, operator], (id) => id, true);
    expect(view.items).toHaveLength(2);
    expect(view.items[0].kind).toBe("problem");
    if (view.items[0].kind === "problem") expect(view.items[0].problem.input).toBe("partial");
    expect(view.items[1].kind).toBe("message");
    const unknown: RoomFeedEvent = { ...partial, item: { ...partial.item as Extract<RoomFeedEvent["item"], { kind: "delivery" }>, bytes_written: 0 } };
    const item = readableRoomFeed([message, unknown, fetched], (id) => id, true).items[0];
    expect(item.kind === "problem" && item.problem.input).toBe("unknown");
    expect(readableRoomFeed([message], (id) => id, false).items[0].kind).toBe("message");
  });
});

/** Minimal DOM double checks actual renderer mutations/listener lifecycle.
 * This is not WebView paint/selection evidence; root still owns that walk. */
class TestElement {
  children: TestElement[] = [];
  dataset: Record<string, string> = {};
  attributes = new Map<string, string>();
  events = new Map<string, () => void>();
  textContent = "";
  className = "";
  hidden = false;
  open = false;
  namespaceURI = "http://www.w3.org/2000/svg";
  constructor(public tag: string) {}
  append(...children: TestElement[]) { this.children.push(...children); }
  replaceChildren(...children: TestElement[]) { this.children = children; }
  setAttribute(name: string, value: string) { this.attributes.set(name, value); }
  addEventListener(name: string, callback: () => void) { this.events.set(name, callback); }
  contains(node: TestElement): boolean { return node === this || this.children.some((child) => child.contains(node)); }
}

afterEach(() => vi.unstubAllGlobals());

it("keeps focused controls and selected text until release, then applies only the latest card", () => {
  let selection: { isCollapsed: boolean; anchorNode: TestElement | null; focusNode: TestElement | null } = { isCollapsed: true, anchorNode: null, focusNode: null };
  const listeners = new Set<() => void>();
  vi.stubGlobal("document", { createElement: (tag: string) => new TestElement(tag),
    getSelection: () => selection,
    addEventListener: (_name: string, fn: () => void) => listeners.add(fn),
    removeEventListener: (_name: string, fn: () => void) => listeners.delete(fn) });
  const replies: CardEvent[] = [];
  const node = buildTypedCard(card(1, "topic"), (id) => id, (event) => replies.push(event));
  const element = node as unknown as TestElement;
  const title = element.children[1]; const details = element.children[4]; const reply = element.children[5];
  details.open = true;
  selection = { isCollapsed: false, anchorNode: title, focusNode: title };
  expect(updateTypedCard(node, card(2, "topic"), (id) => id)).toBe(true);
  expect(updateTypedCard(node, card(3, "topic"), (id) => id)).toBe(true);
  expect(title.textContent).toBe("Title 1");
  reply.events.get("click")?.();
  expect(replies[0].item.card_id).toBe("id1"); // reply names the version actually shown
  selection.isCollapsed = true;
  for (const flush of listeners) flush();
  expect(title.textContent).toBe("Title 3");
  expect(element.children[4]).toBe(details); expect(details.open).toBe(true);
  expect(element.children[5]).toBe(reply);
  reply.events.get("click")?.(); expect(replies[1].item.card_id).toBe("id3");
  disposeTypedCard(node); expect(listeners.size).toBe(0);
});
