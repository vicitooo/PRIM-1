import type { RoomCardContent, RoomFeedCursor, RoomFeedEvent } from "./types";

export type CardEvent = RoomFeedEvent & { item: Extract<RoomFeedEvent["item"], { kind: "card" }> };
export const ROOM_BOARD_PAGE_SIZE = 16;
const MAX_KEYS = 64;
const MAX_BYTES = 2 * 1024 * 1024;
const encoder = new TextEncoder();
const bytesCache = new WeakMap<CardEvent, number>();
const signatureCache = new WeakMap<CardEvent, { label: string; signature: string }>();
export const isCardEvent = (event: RoomFeedEvent): event is CardEvent => event.item.kind === "card";
export const cardKey = (event: CardEvent): string => `typed:${JSON.stringify([event.item.author, event.item.card.key])}`;
/** A card as a person reads it: title, summary, then the body. */
export function cardText(card: Pick<RoomCardContent, "title" | "summary" | "body">): string {
  return [card.title, card.summary, card.body].filter((part) => part?.trim()).join("\n\n");
}

export const pinnedCard = (event: CardEvent): boolean => event.item.card.kind === "decision" && event.item.card.state === "open";

export function selectionTouchesNode(node: Node, selection: Selection | null): boolean {
  if (!selection || selection.isCollapsed) return false;
  if ((selection.anchorNode && node.contains(selection.anchorNode)) || (selection.focusNode && node.contains(selection.focusNode))) return true;
  // A range spanning several cards also selects the middle cards even though
  // neither endpoint is inside them.
  for (let index = 0; index < selection.rangeCount; index++) {
    if (selection.getRangeAt(index).intersectsNode(node)) return true;
  }
  return false;
}

export function typedCardSignature(event: CardEvent, label: string): string {
  const cached = signatureCache.get(event);
  if (cached?.label === label) return cached.signature;
  const signature = JSON.stringify([event, label]);
  signatureCache.set(event, { label, signature });
  return signature;
}

function cardBytes(event: CardEvent): number {
  let bytes = bytesCache.get(event);
  if (bytes === undefined) { bytes = encoder.encode(JSON.stringify(event)).length; bytesCache.set(event, bytes); }
  return bytes;
}

/** Mirror the bounded native projection. A snapshot watermark is separate
 * from the paginated feed cursor, so old queued events cannot resurrect an
 * evicted key or replace a newer version delivered in the snapshot. */
export class RoomCardIndex {
  cards: CardEvent[] = [];
  cursor: RoomFeedCursor | null = null;

  snapshot(events: readonly RoomFeedEvent[], cursor: RoomFeedCursor | null): void {
    this.cards = [];
    this.cursor = null;
    for (const event of events) if (isCardEvent(event)) this.insert(event);
    this.cursor = cursor;
  }

  accept(event: RoomFeedEvent): void {
    if (this.cursor?.epoch === event.cursor.epoch && event.cursor.sequence <= this.cursor.sequence) return;
    if (this.cursor && this.cursor.epoch !== event.cursor.epoch) this.cards = [];
    if (isCardEvent(event)) this.insert(event);
    this.cursor = event.cursor;
  }

  private insert(event: CardEvent): void {
    const key = cardKey(event);
    const existing = this.cards.findIndex((old) => cardKey(old) === key);
    const next = [...this.cards];
    if (existing === -1) next.push(event); else next[existing] = event;
    const sizes = next.map(cardBytes);
    let bytes = sizes.reduce((sum, size) => sum + size, 0);
    for (let index = 0; index < next.length && (next.length > MAX_KEYS || bytes > MAX_BYTES);) {
      if (cardKey(next[index]) === key || pinnedCard(next[index])) { index += 1; continue; }
      bytes -= sizes[index]; sizes.splice(index, 1); next.splice(index, 1);
    }
    // A corrupt/mismatched source cannot grow the renderer without bound.
    if (next.length > MAX_KEYS || bytes > MAX_BYTES) throw new Error("Room card snapshot exceeds its capacity");
    this.cards = next;
  }

  page(index: number): { cards: CardEvent[]; page: number; pages: number; total: number } {
    // Keep surviving cards in insertion order. Pinned decisions stay in the
    // first page's selected set without moving whenever their text updates.
    const pinned = this.cards.filter(pinnedCard);
    const ordinary = this.cards.filter((card) => !pinnedCard(card));
    const pages = Math.max(1, Math.ceil(this.cards.length / ROOM_BOARD_PAGE_SIZE));
    const page = Math.min(Math.max(0, index), pages - 1);
    const selected = new Set([...pinned, ...ordinary]
      .slice(page * ROOM_BOARD_PAGE_SIZE, (page + 1) * ROOM_BOARD_PAGE_SIZE).map(cardKey));
    return { cards: this.cards.filter((card) => selected.has(cardKey(card))), page, pages, total: this.cards.length };
  }
}

export function cardReply(event: CardEvent): { target: string; text: string } {
  return { target: event.item.author, text: `Re: ${event.item.card.title} [card:${event.item.card_id}]\n\n` };
}

/** Only numeric coordinates derived here enter the SVG; no card markup does. */
export function sparklinePoints(series: readonly number[]): string {
  if (!series.length || series.length > 60 || series.some((n) => !Number.isFinite(n) || Math.abs(n) > 1e100)) return "";
  const low = Math.min(...series); const high = Math.max(...series);
  return series.map((value, index) => `${(index * 160 / Math.max(1, series.length - 1)).toFixed(2)},${(high === low ? 20 : 36 - (value - low) / (high - low) * 32).toFixed(2)}`).join(" ");
}

interface CardNodeState {
  event: CardEvent;
  author: HTMLElement;
  title: HTMLElement;
  summary: HTMLElement;
  details: HTMLDetailsElement;
  body: HTMLElement;
  visual: HTMLElement;
  visualSignature: string;
  reply: HTMLButtonElement;
  pending: CardEvent | null;
  flush: () => void;
}
const states = new WeakMap<Element, CardNodeState>();
const setText = (element: HTMLElement, text: string) => { if (element.textContent !== text) element.textContent = text; };

export function buildTypedCard(event: CardEvent, label: (id: string) => string,
  onReply: (event: CardEvent) => void): HTMLElement {
  const node = document.createElement("article");
  node.className = "room-readable-card room-typed-card";
  node.dataset.key = cardKey(event);
  const author = document.createElement("div"); author.className = "room-readable-meta";
  const title = document.createElement("h3");
  const summary = document.createElement("p"); summary.className = "room-typed-summary";
  const visual = document.createElement("div"); visual.className = "room-typed-visual";
  const details = document.createElement("details"); details.className = "room-typed-details";
  const toggle = document.createElement("summary"); toggle.textContent = "Details";
  const body = document.createElement("div"); body.className = "room-readable-body";
  details.append(toggle, body);
  const reply = document.createElement("button"); reply.type = "button"; reply.className = "room-readable-more";
  reply.textContent = "Reply";
  const state: CardNodeState = { event, author, title, summary, details, body, visual,
    visualSignature: "", reply, pending: null, flush: () => {} };
  states.set(node, state);
  reply.addEventListener("click", () => onReply(state.event));
  node.append(author, title, summary, visual, details, reply);
  const selectionInside = () => {
    return selectionTouchesNode(node, document.getSelection());
  };
  state.flush = () => {
    if (state.pending && !selectionInside()) {
      const pending = state.pending; state.pending = null; paintTypedCard(node, state, pending, label);
    }
  };
  // One bounded listener per mounted card; explicit dispose on keyed removal.
  document.addEventListener("selectionchange", state.flush);
  paintTypedCard(node, state, event, label);
  return node;
}

export function updateTypedCard(node: Element, event: CardEvent, label: (id: string) => string): boolean {
  const state = states.get(node);
  if (!state) return false;
  if (selectionTouchesNode(node, document.getSelection())) {
    state.pending = event;
    return true; // keep the selected version until the reader releases it
  }
  state.pending = null;
  paintTypedCard(node, state, event, label);
  return true;
}

export function disposeTypedCard(node: Element): void {
  const state = states.get(node);
  if (state) document.removeEventListener("selectionchange", state.flush);
  states.delete(node);
}

function paintTypedCard(node: Element, state: CardNodeState, event: CardEvent, label: (id: string) => string): void {
  const card = event.item.card;
  state.event = event;
  node.setAttribute("data-card-kind", card.kind);
  node.setAttribute("data-card-state", card.state);
  setText(state.author, `${label(event.item.author)} · ${card.kind}${pinnedCard(event) ? " · Needs a decision" : card.state === "done" ? " · Done" : ""}`);
  setText(state.title, card.title); setText(state.summary, card.summary); setText(state.body, card.body);
  state.details.hidden = !card.body;
  state.reply.setAttribute("aria-label", `Reply to ${label(event.item.author)} about ${card.title}`);
  const signature = JSON.stringify([card.kind, card.data]);
  if (state.visualSignature !== signature) {
    state.visualSignature = signature;
    state.visual.replaceChildren(...cardVisual(card));
  }
}

function cardVisual(card: RoomCardContent): Element[] {
  const data = card.data;
  if (card.kind === "progress") {
    const text = document.createElement("p"); text.textContent = `${data.done} / ${data.total}${data.unit ? ` ${data.unit}` : ""}`;
    const bar = document.createElement("progress"); bar.max = data.total ?? 1; bar.value = data.done ?? 0;
    bar.setAttribute("aria-label", card.title);
    return [text, bar];
  }
  if (card.kind === "metric") {
    const text = document.createElement("p"); text.className = "room-typed-metric";
    text.textContent = `${data.value}${data.unit ? ` ${data.unit}` : ""}`;
    const points = sparklinePoints(data.series ?? []);
    if (!points) return [text];
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("viewBox", "0 0 160 40"); svg.setAttribute("role", "img"); svg.setAttribute("aria-label", "Recent values: " + data.series?.join(", "));
    const line = document.createElementNS(svg.namespaceURI, "polyline");
    line.setAttribute("points", points); line.setAttribute("fill", "none"); line.setAttribute("stroke", "currentColor"); line.setAttribute("stroke-width", "2");
    svg.append(line); return [text, svg];
  }
  if (card.kind === "image" && data.png_base64 && data.png_base64.length <= 131072 && /^[A-Za-z0-9+/]+={0,2}$/.test(data.png_base64)) {
    const image = document.createElement("img"); image.src = `data:image/png;base64,${data.png_base64}`;
    image.alt = data.alt ?? card.title; image.loading = "lazy"; image.decoding = "async";
    image.addEventListener("error", () => { image.replaceWith(document.createTextNode("Image could not be decoded.")); }, { once: true });
    return [image];
  }
  return [];
}
