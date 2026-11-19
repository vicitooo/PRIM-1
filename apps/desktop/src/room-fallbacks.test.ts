import { afterEach, describe, expect, it, vi } from "vitest";
import { fallbackMembers, installRoomFallbacks, RoomFallbackDrafts, unavailableFallbackTargets, untilFromInput, untilInputValue,
  type FallbackMember } from "./room-fallbacks";
import type { RoomFallbackSnapshot, SetRoomFallbacksRequest } from "./types";

const members: FallbackMember[] = [
  { session_id: "c", label: "Claude", driver: "claude" },
  { session_id: "x", label: "Codex", driver: "codex" },
  { session_id: "g", label: "Grok", driver: "grok" },
  { session_id: "t", label: "Terminal", driver: "generic_terminal" },
  { session_id: "p", label: "Prime", driver: "prime" },
];
const room = (room_id = "A", member_ids = members.map((member) => member.session_id), membership_revision = 1) => ({ room_id, member_ids, membership_revision });
function snapshot(room_id = "A", revision = 1): RoomFallbackSnapshot {
  return { room_id, revision, settings: { enabled: false, interval_minutes: 30, message: "Keep working", recipient_mode: "all", recipient_ids: [], overrides: [] }, members: [] };
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}
const settle = async () => { await Promise.resolve(); await Promise.resolve(); };

function fixture() {
  let allowed = true;
  const get = vi.fn(async (id: string) => snapshot(id));
  const set = vi.fn(async (request: SetRoomFallbacksRequest) => ({ ...snapshot(request.room_id, request.revision + 1), settings: request.settings }));
  const api = { get, set, canEdit: () => allowed };
  const drafts = new RoomFallbackDrafts(api);
  return { drafts, api, hold: () => { allowed = false; } };
}

describe("room-owned fallback drafts and revision admission", () => {
  it("offers only hosted Claude/Codex/Grok stable IDs, independent of lifecycle and label collisions", () => {
    const inputs = [...members, { session_id: "outside", label: "Guest", driver: "claude" as const }];
    expect(fallbackMembers(room("A", ["c", "c", "x", "g", "t", "p", "unknown"]), inputs).map((entry) => entry.session_id)).toEqual(["c", "x", "g"]);
    expect(fallbackMembers(room("A", ["c", "x"]), inputs.map((entry) => ({ ...entry, label: "Same" })))).toHaveLength(2);
  });

  it("retains exact room drafts through A→B→A and ordinary member updates", async () => {
    const { drafts } = fixture();
    drafts.bind(room(), members); await settle();
    drafts.edit("A", (value) => { value.message = "  literal\n<instructions>  "; });
    const state = drafts.rooms.get("A")!;
    const formVersion = state.formVersion;
    drafts.bind(room("B"), members); await settle();
    drafts.edit("B", (value) => { value.message = "For B"; });
    drafts.bind(room(), members);
    expect(state.settings?.message).toBe("  literal\n<instructions>  ");
    expect(state.formVersion).toBe(formVersion);
    expect(drafts.rooms.get("B")?.settings?.message).toBe("For B");
  });

  it("keeps edits made during a refresh or discard read and ignores older read responses", async () => {
    const { drafts, api } = fixture();
    drafts.bind(room(), members); await settle();
    const old = deferred<RoomFallbackSnapshot>();
    const latest = deferred<RoomFallbackSnapshot>();
    api.get.mockImplementationOnce(() => old.promise).mockImplementationOnce(() => latest.promise);
    const first = drafts.load("A"); const second = drafts.load("A", true);
    drafts.edit("A", (value) => { value.message = "Typed while reading"; });
    latest.resolve(snapshot("A", 3)); await second;
    old.resolve(snapshot("A", 2)); await first;
    const state = drafts.rooms.get("A")!;
    expect(state.settings?.message).toBe("Typed while reading");
    expect(state.revision).toBe(1);
    expect(state.snapshot?.revision).toBe(3);
    expect(state.dirty).toBe(true);
  });

  it("saves the loaded revision once and never replaces another room with a late save", async () => {
    const { drafts, api } = fixture();
    drafts.bind(room(), members); await settle();
    drafts.edit("A", (value) => { value.enabled = true; });
    const pending = deferred<RoomFallbackSnapshot>(); api.set.mockImplementationOnce(() => pending.promise);
    const save = drafts.save("A"); await drafts.save("A");
    drafts.bind(room("B"), members); await settle();
    drafts.edit("B", (value) => { value.message = "B draft"; });
    pending.resolve({ ...snapshot("A", 2), settings: { ...snapshot().settings, enabled: true } }); await save;
    expect(api.set).toHaveBeenCalledTimes(1);
    expect(api.set.mock.calls[0][0].revision).toBe(1);
    expect(drafts.activeRoomId).toBe("B");
    expect(drafts.rooms.get("B")?.settings?.message).toBe("B draft");
    expect(drafts.rooms.get("A")?.dirty).toBe(false);
  });

  it("keeps a revision conflict visible without rebasing or retrying an unsaved draft", async () => {
    const { drafts, api } = fixture();
    drafts.bind(room(), members); await settle();
    drafts.edit("A", (value) => { value.message = "My text"; });
    api.set.mockRejectedValueOnce(new Error("Settings changed; reload the latest revision"));
    await drafts.save("A");
    expect(drafts.rooms.get("A")?.error).toContain("Settings changed");
    expect(drafts.rooms.get("A")?.settings?.message).toBe("My text");
    expect(drafts.rooms.get("A")?.revision).toBe(1);
    expect(api.set).toHaveBeenCalledTimes(1);
    api.get.mockResolvedValueOnce(snapshot("A", 4)); await drafts.load("A", true);
    expect(drafts.rooms.get("A")?.revision).toBe(4);
    expect(drafts.rooms.get("A")?.dirty).toBe(false);
  });

  it("refreshes membership without auto-selecting new members or silently forgetting removed targets", async () => {
    const { drafts, api } = fixture();
    drafts.bind(room("A", ["c"]), members); await settle();
    drafts.edit("A", (value) => { value.recipient_mode = "selected"; value.recipient_ids = ["c"]; });
    drafts.bind(room("A", ["c", "x", "g"], 2), members); await settle();
    expect(drafts.rooms.get("A")?.settings?.recipient_ids).toEqual(["c"]);
    drafts.bind(room("A", ["x", "g"], 3), members); await settle();
    expect(drafts.rooms.get("A")?.settings?.recipient_ids).toEqual(["c"]);
    expect(unavailableFallbackTargets(drafts.rooms.get("A")!.settings!, drafts.rooms.get("A")!.members)).toEqual(["c"]);
    await drafts.save("A");
    expect(api.set).not.toHaveBeenCalled();
    expect(drafts.rooms.get("A")?.error).toContain("no longer eligible");
    drafts.edit("A", (value) => { value.recipient_ids = ["x"]; });
    await drafts.save("A");
    expect(api.set.mock.calls[0][0].settings.recipient_ids).toEqual(["x"]);
  });

  it("does not resurrect deleted room drafts from pending reads or saves", async () => {
    const { drafts, api } = fixture();
    const pending = deferred<RoomFallbackSnapshot>(); api.get.mockImplementationOnce(() => pending.promise);
    drafts.bind(room(), members); drafts.retire(new Set());
    pending.resolve(snapshot()); await settle();
    expect(drafts.rooms.has("A")).toBe(false);
    drafts.bind(room(), members); await settle();
    drafts.edit("A", (value) => { value.message = "Deleted room"; });
    const saving = deferred<RoomFallbackSnapshot>(); api.set.mockImplementationOnce(() => saving.promise);
    const result = drafts.save("A"); drafts.retire(new Set());
    saving.resolve(snapshot("A", 2)); await result;
    expect(drafts.rooms.has("A")).toBe(false);
  });

  it("rejects a wrong-room response without replacing the loaded draft", async () => {
    const { drafts, api } = fixture();
    drafts.bind(room(), members); await settle();
    drafts.edit("A", (value) => { value.message = "Only A"; });
    api.get.mockResolvedValueOnce(snapshot("B", 5)); await drafts.load("A", true);
    expect(drafts.rooms.get("A")?.settings?.message).toBe("Only A");
    expect(drafts.rooms.get("A")?.error).toContain("another room");
    api.set.mockResolvedValueOnce(snapshot("B", 6)); await drafts.save("A");
    expect(drafts.rooms.get("A")?.settings?.message).toBe("Only A");
    expect(drafts.rooms.get("A")?.dirty).toBe(true);
    expect(drafts.rooms.has("B")).toBe(false);
  });

  it("holds mutation even through direct calls while allowing settings reads", async () => {
    const { drafts, api, hold } = fixture();
    drafts.bind(room(), members); await settle();
    drafts.edit("A", (value) => { value.enabled = true; }); hold();
    drafts.edit("A", (value) => { value.message = "must not change"; });
    await drafts.save("A");
    expect(drafts.rooms.get("A")?.settings?.message).toBe("Keep working");
    expect(api.set).not.toHaveBeenCalled();
    await drafts.load("A"); expect(api.get).toHaveBeenCalledTimes(2);
  });

  it("coalesces native invalidations by known room and refreshes outcomes without replacing a dirty draft", async () => {
    const { drafts, api } = fixture();
    drafts.bind(room(), members); await settle();
    drafts.edit("A", (value) => { value.message = "Unfinished draft"; });
    const first = deferred<RoomFallbackSnapshot>(); const latest = deferred<RoomFallbackSnapshot>();
    api.get.mockImplementationOnce(() => first.promise).mockImplementationOnce(() => latest.promise);
    drafts.invalidate("A");
    for (let index = 0; index < 20; index += 1) { drafts.invalidate("A"); drafts.invalidate("unvisited"); }
    expect(api.get).toHaveBeenCalledTimes(2);
    first.resolve(snapshot("A", 2)); await settle();
    expect(api.get).toHaveBeenCalledTimes(3);
    latest.resolve({ ...snapshot("A", 3), members: [{ session_id: "c", next_due_at: "2026-10-19T12:00:00Z", last_message_id: "occurrence", last_status: "written", note: null }] }); await settle();
    const state = drafts.rooms.get("A")!;
    expect(state.snapshot?.members[0].last_status).toBe("written");
    expect(state.snapshot?.revision).toBe(3);
    expect(state.settings?.message).toBe("Unfinished draft");
    expect(state.revision).toBe(1);
    expect(state.dirty).toBe(true);
  });

  it("defers invalidation behind save and retains the committed notice warning through that read", async () => {
    const { drafts, api } = fixture();
    drafts.bind(room(), members); await settle();
    drafts.edit("A", (value) => { value.enabled = true; });
    const pending = deferred<RoomFallbackSnapshot>(); api.set.mockImplementationOnce(() => pending.promise);
    const save = drafts.save("A");
    drafts.invalidate("A"); drafts.invalidate("A");
    expect(api.get).toHaveBeenCalledTimes(1);
    const committed = { ...snapshot("A", 2), settings: { ...snapshot().settings, enabled: true } };
    api.get.mockResolvedValueOnce(committed);
    pending.resolve({ ...committed, notice_warning: "Feed notice failed" }); await save; await settle();
    expect(api.get).toHaveBeenCalledTimes(2);
    expect(drafts.rooms.get("A")?.snapshot?.notice_warning).toBe("Feed notice failed");
    expect(drafts.rooms.get("A")?.dirty).toBe(false);
    expect(api.set).toHaveBeenCalledTimes(1);
  });
});

/** Installed handler/node assertions only; this is not browser layout evidence. */
class TestElement {
  children: TestElement[] = [];
  parent: TestElement | null = null;
  dataset: Record<string, string> = {};
  events = new Map<string, () => void>();
  textContent = ""; className = ""; value = ""; title = ""; type = "";
  disabled = false; open = false; hidden = false; checked = false; rows = 0;
  selectionStart = 0; selectionEnd = 0;
  constructor(public tag: string) {}
  set innerHTML(_text: string) { throw new Error("Never insert metadata as HTML"); }
  append(...nodes: TestElement[]) { for (const node of nodes) { node.parent = this; this.children.push(node); } }
  replaceChildren(...nodes: TestElement[]) { this.children = []; this.append(...nodes); }
  remove() { if (this.parent) this.parent.children = this.parent.children.filter((node) => node !== this); }
  contains(target: TestElement): boolean { return this === target || this.children.some((node) => node.contains(target)); }
  setAttribute(_name: string, _value: string) {}
  addEventListener(name: string, handler: () => void) { this.events.set(name, handler); }
  fire(name: string) { this.events.get(name)?.(); }
  all(tag: string): TestElement[] { return [...(this.tag === tag ? [this] : []), ...this.children.flatMap((node) => node.all(tag))]; }
  text(): string { return this.textContent + this.children.map((node) => node.text()).join("\n"); }
}
afterEach(() => vi.unstubAllGlobals());

it("keeps clean form nodes, text selection and member expansion on same-revision native outcomes and Refresh", async () => {
  const doc = { createElement: (tag: string) => new TestElement(tag), activeElement: null as TestElement | null };
  vi.stubGlobal("document", doc);
  const host = new TestElement("div"); const { api } = fixture();
  api.get.mockResolvedValueOnce(snapshot("A", 0));
  const drafts = installRoomFallbacks(new TestElement("details") as unknown as HTMLDetailsElement,
    new TestElement("span") as unknown as HTMLElement, host as unknown as HTMLElement, api);
  drafts.bind(room(), members); await settle();
  const message = host.all("textarea")[0];
  const member = host.all("details")[0];
  doc.activeElement = message; message.selectionStart = 2; message.selectionEnd = 6; member.open = true;
  const outcome: RoomFallbackSnapshot = { ...snapshot("A", 0), members: [{ session_id: "c", next_due_at: "2026-10-20T12:00:00Z",
    last_message_id: "sent", last_status: "written", note: "Native outcome" }] };
  api.get.mockResolvedValueOnce(outcome);
  drafts.invalidate("A"); await settle();
  expect(drafts.rooms.get("A")?.dirty).toBe(false);
  expect(host.all("textarea")[0]).toBe(message);
  expect(host.contains(doc.activeElement!)).toBe(true);
  expect([message.selectionStart, message.selectionEnd]).toEqual([2, 6]);
  expect(host.all("details")[0]).toBe(member);
  expect(member.open).toBe(true);
  expect(host.text()).toContain("Last: written");
  expect(host.text()).toContain("Native outcome");
  api.get.mockResolvedValueOnce(outcome);
  host.all("button").find((node) => node.textContent === "Refresh")!.fire("click"); await settle();
  expect(host.all("textarea")[0]).toBe(message);
  expect(member.open).toBe(true);
});

it("replaces clean settings on a new revision and restores saved settings on explicit same-revision discard", async () => {
  vi.stubGlobal("document", { createElement: (tag: string) => new TestElement(tag) });
  const host = new TestElement("div"); const { api } = fixture();
  const drafts = installRoomFallbacks(new TestElement("details") as unknown as HTMLDetailsElement,
    new TestElement("span") as unknown as HTMLElement, host as unknown as HTMLElement, api);
  drafts.bind(room(), members); await settle();
  const original = host.all("textarea")[0];
  const revised = { ...snapshot("A", 2), settings: { ...snapshot().settings, message: "Changed by another member" } };
  api.get.mockResolvedValueOnce(revised); drafts.invalidate("A"); await settle();
  const current = host.all("textarea")[0];
  expect(current).not.toBe(original);
  expect(current.value).toBe("Changed by another member");
  current.value = "My unsaved replacement"; current.fire("input");
  api.get.mockResolvedValueOnce(revised);
  host.all("button").find((node) => node.textContent === "Discard edits")!.fire("click"); await settle();
  expect(host.all("textarea")[0].value).toBe("Changed by another member");
  expect(drafts.rooms.get("A")?.dirty).toBe(false);
  expect(drafts.rooms.get("A")?.revision).toBe(2);
});

it("keeps editing nodes on telemetry/collapse and renders literal metadata with held controls", async () => {
  vi.stubGlobal("document", { createElement: (tag: string) => new TestElement(tag) });
  const details = new TestElement("details"); const summary = new TestElement("span"); const host = new TestElement("div");
  const { api, hold } = fixture();
  const drafts = installRoomFallbacks(details as unknown as HTMLDetailsElement, summary as unknown as HTMLElement, host as unknown as HTMLElement, api);
  const literal = members.map((member) => ({ ...member, label: "<img onerror=alert(1)>" }));
  drafts.bind(room(), literal); await settle();
  const message = host.all("textarea")[0]; message.value = "  keep literal\n<script>"; message.fire("input");
  drafts.bind(room(), literal); details.open = false; details.fire("toggle");
  expect(host.all("textarea")[0]).toBe(message);
  expect(host.text()).toContain("<img onerror=alert(1)>");
  details.open = true; details.fire("toggle"); await settle();
  expect(host.all("textarea")[0]).toBe(message);
  expect(message.value).toBe("  keep literal\n<script>");
  hold(); drafts.bind(room(), literal);
  expect(host.all("button").find((node) => node.textContent === "Save")?.disabled).toBe(true);
  expect(message.disabled).toBe(true);
  expect(message.dataset.trialMutation).toBe("");
});

it("projects custom replacement and appended brief literally without an immediate-send action", async () => {
  vi.stubGlobal("document", { createElement: (tag: string) => new TestElement(tag) });
  const details = new TestElement("details"); const host = new TestElement("div"); const { api } = fixture();
  const drafts = installRoomFallbacks(details as unknown as HTMLDetailsElement, new TestElement("span") as unknown as HTMLElement, host as unknown as HTMLElement, api);
  drafts.bind(room(), members); await settle();
  const member = host.all("details")[0];
  const mode = member.all("select")[0]; mode.value = "custom"; mode.fire("change");
  const cadence = member.all("select")[1]; cadence.value = "60"; cadence.fire("change");
  const custom = member.all("textarea")[0]; custom.value = "Replacement\n"; custom.fire("input");
  const brief = member.all("textarea")[1]; brief.value = "  literal brief  "; brief.fire("input");
  api.set.mockImplementationOnce(async (request) => ({ ...snapshot("A", 2), settings: request.settings, notice_warning: "Saved settings; feed notice unavailable" }));
  host.all("button").find((node) => node.textContent === "Save")!.fire("click"); await settle();
  expect(api.set.mock.calls[0][0].settings.overrides).toEqual([{ session_id: "c", mode: "custom", interval_minutes: 60, message: "Replacement\n", brief: "  literal brief  " }]);
  expect(host.all("button").some((node) => /send|start|wake/i.test(node.textContent))).toBe(false);
  expect(host.text()).toContain("Saved · Saved settings; feed notice unavailable");
  expect(drafts.rooms.get("A")?.dirty).toBe(false);
  expect(host.all("button").find((node) => node.textContent === "Save")?.disabled).toBe(true);
});

// v3 W6 (J10): the room's end time, typed in local time, saved as UTC.
describe("the fallback end time", () => {
  it("round-trips local input and the saved UTC instant, to the minute", () => {
    expect(untilInputValue(null)).toBe("");
    expect(untilInputValue(undefined)).toBe("");
    expect(untilInputValue("not a time")).toBe("");
    expect(untilFromInput("")).toBeNull();
    expect(untilFromInput("  ")).toBeNull();
    const saved = untilFromInput("2026-10-25T18:30");
    expect(saved).toBe(new Date(2026, 9, 25, 18, 30).toISOString());
    expect(untilInputValue(saved)).toBe("2026-10-25T18:30");
  });

  it("saves the end time with the settings and clears it to no end", async () => {
    const { drafts, api } = fixture();
    drafts.bind(room(), members);
    await settle();
    drafts.edit("A", (settings) => { settings.until = untilFromInput("2026-10-25T18:30"); });
    await drafts.save("A");
    expect(api.set.mock.calls[0][0].settings.until).toBe(new Date(2026, 9, 25, 18, 30).toISOString());
    drafts.edit("A", (settings) => { settings.until = untilFromInput(""); });
    await drafts.save("A");
    expect(api.set.mock.calls[1][0].settings.until).toBeNull();
  });
});
