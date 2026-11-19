import { afterEach, describe, expect, it, vi } from "vitest";
import { attachableMembers, candidateStatus, installSessionInventory, inventorySummary,
  type ConversationCandidate, type InventoryActions, type InventoryMember, type SessionInventory } from "./session-inventory";

const candidate: ConversationCandidate = {
  provider: "codex", id: "fixture", label: null, workspace: "D:/work", metadata_paths: [],
  updated_at: null, kind: "root", evidence: "metadata", conflict: false,
};
const inventory: SessionInventory = {
  workspace: "D:/work", candidates: [candidate], truncated: false,
  providers: [{ provider: "codex", entries_examined: 1, truncated: false, errors: [] }],
};
describe("local discovery presentation", () => {
  it("never equates metadata with a live agent binding", () => {
    expect(candidateStatus(candidate)).toContain("not an agent binding");
    expect(candidateStatus({ ...candidate, kind: "subagent" })).toContain("not a standalone");
    expect(candidateStatus({ ...candidate, kind: "unknown" })).toContain("unverified");
    expect(candidateStatus({ ...candidate, conflict: true })).toContain("not verified");
  });
  it("keeps incomplete and unavailable providers visible", () => {
    expect(inventorySummary(inventory)).toBe("1 conversation candidate found. Scanning never attaches or starts a member.");
    expect(inventorySummary({ ...inventory, truncated: true })).toContain("partial scan");
    expect(inventorySummary({ ...inventory, providers: [{ ...inventory.providers[0], errors: ["unreadable"] }] })).toContain("partial scan");
    expect(inventorySummary({ ...inventory, candidates: [] })).toContain("0 conversation candidates");
  });
});

const stopped: InventoryMember = {
  session_id: "00000000-0000-0000-0000-000000000001", label: "Codex member", driver: "codex",
  running: false, output_draining: false, lifecycle_state: "closed", working_dir: "D:/work",
};

it("offers only stopped same-driver members for nonconflicting supported roots", () => {
  const members: InventoryMember[] = [stopped,
    { ...stopped, session_id: "other", driver: "claude" },
    { ...stopped, session_id: "live", running: true },
    { ...stopped, session_id: "draining", output_draining: true },
    { ...stopped, session_id: "starting", lifecycle_state: "starting" },
    { ...stopped, session_id: "restarting", lifecycle_state: "restarting" },
    { ...stopped, session_id: "failed", lifecycle_state: "failed" },
    { ...stopped, session_id: "stalled", lifecycle_state: "stalled" },
    { ...stopped, session_id: "ready", lifecycle_state: "ready" },
    { ...stopped, session_id: "busy", lifecycle_state: "busy" },
    { ...stopped, session_id: "idle", lifecycle_state: "idle" },
  ];
  expect(attachableMembers(candidate, members)).toEqual([stopped]);
  for (const disallowed of [{ ...candidate, conflict: true }, { ...candidate, kind: "subagent" as const },
    { ...candidate, kind: "unknown" as const }, { ...candidate, provider: "future-provider" }]) {
    expect(attachableMembers(disallowed, members)).toEqual([]);
  }
});

it("matches native Windows folder normalization without admitting a parent, child or similar folder", () => {
  for (const folder of ["d:\\WORK\\", "\\\\?\\D:\\work", "//?/D:/WORK////", "D:/Work"]) {
    const equivalent = { ...stopped, working_dir: folder };
    expect(attachableMembers(candidate, [equivalent]), folder).toEqual([equivalent]);
    expect(attachableMembers({ ...candidate, workspace: folder }, [stopped]), folder).toEqual([stopped]);
  }
  for (const folder of ["D:/", "D:/work/child", "D:/work-other", "D:/other", " D:/work", "D:/work "]) {
    expect(attachableMembers(candidate, [{ ...stopped, working_dir: folder }]), folder).toEqual([]);
  }
});

/** Exercises the installed event handlers and retained nodes; no browser paint claim. */
class TestElement {
  children: TestElement[] = [];
  dataset: Record<string, string> = {};
  events = new Map<string, () => void | Promise<void>>();
  attributes = new Map<string, string>();
  textContent = "";
  className = "";
  value = "";
  title = "";
  type = "";
  disabled = false;
  open = false;
  constructor(public tag: string) {}
  set innerHTML(_value: string) { throw new Error("Metadata must never be inserted as HTML"); }
  append(...nodes: TestElement[]) { this.children.push(...nodes); }
  replaceChildren(...nodes: TestElement[]) { this.children = nodes; }
  setAttribute(name: string, value: string) { this.attributes.set(name, value); }
  addEventListener(name: string, callback: () => void | Promise<void>) { this.events.set(name, callback); }
  async fire(name: string) { await this.events.get(name)?.(); }
  all(tag: string): TestElement[] { return [...(this.tag === tag ? [this] : []), ...this.children.flatMap((node) => node.all(tag))]; }
  text(): string { return this.textContent + this.children.map((node) => node.text()).join("\n"); }
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

function fixture(scanResult = inventory) {
  vi.stubGlobal("document", { createElement: (tag: string) => new TestElement(tag) });
  const scan = new TestElement("button");
  const status = new TestElement("p");
  const results = new TestElement("div");
  let members: InventoryMember[] = [stopped];
  let allowed = true;
  const actions = {
    members: () => members,
    canAttach: () => allowed,
    confirm: vi.fn(async (_message: string) => true),
    attach: vi.fn<InventoryActions["attach"]>(async () => {}),
    refresh: vi.fn(async () => {}),
  };
  const discover = vi.fn(async () => scanResult);
  installSessionInventory(scan as unknown as HTMLButtonElement, status as unknown as HTMLElement,
    results as unknown as HTMLElement, discover, actions);
  return { scan, status, results, discover, actions,
    setMembers: (value: InventoryMember[]) => { members = value; },
    setAllowed: (value: boolean) => { allowed = value; },
    select: () => results.all("select")[0], button: () => results.all("button")[0],
    async openCandidates() {
      for (const detail of results.all("details")) { detail.open = true; await detail.fire("toggle"); }
    },
    async load() { await scan.fire("click"); await this.openCandidates(); },
    async choose(id = stopped.session_id) { this.select().value = id; await this.select().fire("change"); },
  };
}

afterEach(() => vi.unstubAllGlobals());

describe("operator attachment disclosure", () => {
  it("omits wrong-folder members before selection and refuses a forced stale choice before confirmation", async () => {
    const f = fixture();
    const wrongFolder = { ...stopped, session_id: "other-folder", working_dir: "D:/another-repo" };
    f.setMembers([stopped, wrongFolder]);
    await f.load();
    expect(f.select().all("option").map((option) => option.value)).toEqual(["", stopped.session_id]);
    await f.choose(wrongFolder.session_id); await f.button().fire("click");
    expect(f.actions.confirm).not.toHaveBeenCalled();
    expect(f.actions.attach).not.toHaveBeenCalled();
    expect(f.button().disabled).toBe(true);
    expect(f.results.text()).toContain("same harness and folder");
  });

  it("keeps a native unsupported candidate visible with its literal reason and no attachment controls", async () => {
    const reason = "Archived in Codex; unarchive first. <img src=x onerror=alert(1)>";
    const unavailable = { ...candidate, attach_unavailable: reason };
    const f = fixture({ ...inventory, candidates: [unavailable] });
    await f.load();
    expect(candidateStatus(unavailable)).toContain(reason);
    expect(f.results.text()).toContain(candidate.id);
    expect(f.results.text()).toContain(reason);
    expect(f.results.all("img")).toHaveLength(0);
    expect(f.results.all("select")).toHaveLength(0);
    expect(f.results.all("button")).toHaveLength(0);
    expect(attachableMembers(unavailable, [stopped])).toEqual([]);
    expect(f.actions.attach).not.toHaveBeenCalled();
  });

  it("keeps metadata literal, shows the native personal anchor and requires an explicit member choice", async () => {
    const unsafeText = "<img src=x onerror=alert(1)>";
    const f = fixture({ ...inventory, workspace: `D:/personal/${unsafeText}`,
      candidates: [{ ...candidate, id: unsafeText, label: unsafeText, evidence: unsafeText, metadata_paths: [unsafeText] },
        { ...candidate, kind: "subagent" }, { ...candidate, kind: "unknown" }, { ...candidate, conflict: true }] });
    await f.scan.fire("click");
    expect(f.results.all("select")).toHaveLength(0); // Collapsed candidates do not build member lists.
    await f.openCandidates();
    expect(f.results.text()).toContain(`Personal repository: D:/personal/${unsafeText}`);
    expect(f.results.text()).toContain(unsafeText);
    expect(f.results.all("img")).toHaveLength(0);
    expect(f.results.all("select")).toHaveLength(1);
    expect(f.select().value).toBe("");
    expect(f.button().disabled).toBe(true);
    await f.button().fire("click"); // Even a synthetic click cannot commit an unselected destination.
    expect(f.actions.confirm).not.toHaveBeenCalled();
    expect(f.actions.attach).not.toHaveBeenCalled();
  });

  it("cancels without mutation and requires literal true from confirmation", async () => {
    const f = fixture();
    await f.load(); await f.choose();
    f.actions.confirm.mockResolvedValueOnce(false).mockResolvedValueOnce({} as boolean);
    await f.button().fire("click"); await f.button().fire("click");
    expect(f.actions.attach).not.toHaveBeenCalled();
    expect(f.results.text()).toContain("Cancelled");
    expect(f.select().value).toBe(stopped.session_id);
    const confirmation = f.actions.confirm.mock.calls[0][0];
    expect(confirmation).toContain(candidate.id);
    expect(confirmation).toContain(stopped.session_id);
    expect(confirmation).toContain("existing conversation binding may change");
    expect(confirmation).toContain("Nothing starts");
  });

  it("blocks duplicate submission and rescan during confirmation/attach, then refreshes without starting", async () => {
    const f = fixture();
    const confirmation = deferred<boolean>(); const attaching = deferred<void>();
    f.actions.confirm.mockReturnValue(confirmation.promise);
    f.actions.attach.mockReturnValue(attaching.promise);
    await f.load(); await f.choose();
    const first = f.button().fire("click");
    expect(f.button().disabled).toBe(true); expect(f.select().disabled).toBe(true); expect(f.scan.disabled).toBe(true);
    await f.button().fire("click"); await f.scan.fire("click");
    expect(f.actions.confirm).toHaveBeenCalledTimes(1); expect(f.discover).toHaveBeenCalledTimes(1);
    expect(f.actions.attach).not.toHaveBeenCalled();
    confirmation.resolve(true); await Promise.resolve();
    expect(f.actions.attach).toHaveBeenCalledExactlyOnceWith({ session_id: stopped.session_id, driver: "codex", conversation_id: candidate.id });
    await f.button().fire("click");
    expect(f.actions.attach).toHaveBeenCalledTimes(1);
    attaching.resolve(); await first;
    expect(f.actions.refresh).toHaveBeenCalledTimes(1);
    expect(f.results.text()).toContain("Attached to Codex member for recovery. Nothing started");
    expect(f.button().disabled).toBe(true); // Same confirmed binding is not offered as another submit.
    expect(f.scan.disabled).toBe(false);
  });

  it("preserves disclosure, list and selection on a native refusal or failed rescan, then permits an explicit retry", async () => {
    const f = fixture();
    await f.load(); await f.choose();
    const list = f.results.children;
    const detail = f.results.all("details")[1]; detail.open = true;
    f.actions.attach.mockRejectedValueOnce(new Error("Conversation is held by another member"));
    await f.button().fire("click");
    expect(f.results.children).toBe(list); expect(detail.open).toBe(true);
    expect(f.select().value).toBe(stopped.session_id); expect(f.button().disabled).toBe(false);
    expect(f.results.text()).toContain("Conversation is held by another member");
    f.discover.mockRejectedValueOnce(new Error("Folder unavailable"));
    await f.scan.fire("click");
    expect(f.results.children).toBe(list); expect(f.select().value).toBe(stopped.session_id);
    expect(f.status.text()).toContain("Folder unavailable");
    await f.button().fire("click"); expect(f.actions.attach).toHaveBeenCalledTimes(2);
  });

  it("rechecks destination liveness, folder and holds while confirmation is open", async () => {
    for (const stateChange of ["running", "held", "folder"] as const) {
      const f = fixture(); const confirmation = deferred<boolean>();
      f.actions.confirm.mockReturnValue(confirmation.promise);
      await f.load(); await f.choose();
      const pending = f.button().fire("click");
      if (stateChange === "running") f.setMembers([{ ...stopped, running: true }]);
      else if (stateChange === "folder") f.setMembers([{ ...stopped, working_dir: "D:/changed-repo" }]);
      else f.setAllowed(false);
      confirmation.resolve(true); await pending;
      expect(f.actions.attach).not.toHaveBeenCalled();
      expect(f.results.text()).toContain("no longer available or changes are disabled");
      expect(f.select().value).toBe(stopped.session_id); expect(f.button().disabled).toBe(true);
    }
  });

  it("keeps held-preview attachment disabled, including direct handler invocation", async () => {
    const f = fixture(); f.setAllowed(false);
    await f.load(); await f.choose(); await f.button().fire("click");
    expect(f.select().disabled).toBe(true); expect(f.button().disabled).toBe(true);
    expect(f.button().title).toContain("preview is held");
    expect(f.actions.confirm).not.toHaveBeenCalled(); expect(f.actions.attach).not.toHaveBeenCalled();
    expect(f.select().dataset).toHaveProperty("trialMutation");
  });

  it("distinguishes a committed attachment from a failed state refresh and prevents retrying it", async () => {
    const f = fixture(); f.actions.refresh.mockRejectedValueOnce(new Error("Refresh offline"));
    await f.load(); await f.choose(); await f.button().fire("click");
    expect(f.results.text()).toContain("Attached to Codex member for recovery");
    expect(f.results.text()).toContain("State refresh failed: Error: Refresh offline");
    expect(f.results.text()).toContain("do not repeat the attachment");
    expect(f.button().disabled).toBe(true);
    await f.button().fire("click"); expect(f.actions.attach).toHaveBeenCalledTimes(1);
  });
});
