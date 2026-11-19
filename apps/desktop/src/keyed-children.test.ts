import { describe, expect, it } from "vitest";

import { reconcileKeyedChildren, retainEngagedChildren, type KeyedChild, type KeyedParent } from "./keyed-children";

interface FakeNode {
  key: string | undefined;
  signature?: string;
  text: string;
  /** Times the node left its parent. A DOM node that leaves loses selection and focus. */
  detached: number;
}

function node(key: string | undefined, signature: string, text = key ?? "foreign"): FakeNode {
  return { key, signature, text, detached: 0 };
}

function fakeParent(initial: FakeNode[]) {
  const nodes = [...initial];
  const built: FakeNode[] = [];
  const parent: KeyedParent<FakeNode> = {
    children: () => [...nodes],
    keyOf: (child) => child.key,
    signatureOf: (child) => child.signature,
    setSignature: (child, signature) => {
      child.signature = signature;
    },
    insertBefore: (child, before) => {
      const at = nodes.indexOf(child);
      if (at !== -1) {
        // Inserting a node that is already a child moves it, as in the DOM.
        nodes.splice(at, 1);
        child.detached += 1;
      }
      nodes.splice(before === null ? nodes.length : nodes.indexOf(before), 0, child);
    },
    remove: (child) => {
      nodes.splice(nodes.indexOf(child), 1);
      child.detached += 1;
    },
  };
  const want = (key: string, signature: string, update?: (child: FakeNode) => boolean): KeyedChild<FakeNode> => ({
    key,
    signature,
    build: () => {
      const fresh = node(key, "", `${key}*`);
      built.push(fresh);
      return fresh;
    },
    update,
  });
  return { parent, nodes, built, want };
}

describe("keyed children", () => {
  it("keeps the original selected cohort bounded while new traffic crosses its range", () => {
    const start = node("start", "1"); const end = node("end", "1");
    const { parent, nodes, want } = fakeParent([start, end]);
    const cohort = new Set(["start", "end"]);
    for (let index = 0; index < 100; index++) {
      const desired = [want(`new-${index}`, "1"), want("warning", `${index}`)];
      // The browser range may now intersect every child. Only original nodes
      // are eligible for continued selection retention during this deferral.
      const held = retainEngagedChildren(parent, desired, (child) => cohort.has(child.key ?? ""));
      reconcileKeyedChildren(parent, held.wanted);
      expect(nodes.length).toBe(4);
      expect(start.detached).toBe(0); expect(end.detached).toBe(0);
    }
  });
  it("retains selected chatter during the first-card transition while showing operator posts and warnings", () => {
    const chatter = node("chatter", "1"); const operator = node("operator", "1");
    const { parent, nodes, want } = fakeParent([chatter, operator]);
    const projection = [want("typed-card", "1"), want("operator", "1"), want("critical-warning", "1"), want("new-operator", "1")];
    const held = retainEngagedChildren(parent, projection, (child) => child === chatter);
    expect(held.pending).toBe(true);
    reconcileKeyedChildren(parent, held.wanted);
    expect(nodes.map((child) => child.key)).toEqual(["typed-card", "chatter", "operator", "critical-warning", "new-operator"]);
    expect(chatter.detached).toBe(0);
    const released = retainEngagedChildren(parent, projection, () => false);
    expect(released.pending).toBe(false);
    reconcileKeyedChildren(parent, released.wanted);
    expect(nodes.map((child) => child.key)).toEqual(["typed-card", "operator", "critical-warning", "new-operator"]);
  });

  it("keeps a focused card excluded by a newly pinned topic until focus release, without blocking its siblings", () => {
    const focus = node("focused-card", "old"); const other = node("other", "1");
    const { parent, nodes, want } = fakeParent([focus, other]);
    const desired = [want("new-decision", "1"), want("other", "2", (child) => { child.text = "new"; return true; }), want("warning", "1")];
    const held = retainEngagedChildren(parent, desired, (child) => child === focus);
    reconcileKeyedChildren(parent, held.wanted);
    expect(focus.detached).toBe(0); expect(other.text).toBe("new");
    expect(nodes.some((child) => child.key === "warning")).toBe(true);
    // Explicit navigation calls the reconciler with the projection directly.
    reconcileKeyedChildren(parent, desired);
    expect(focus.detached).toBe(1);
  });

  it("defers only destructive replacement, while in-place updates publish normally", () => {
    const note = node("note", "1"); const message = node("message", "1");
    const { parent, nodes, want } = fakeParent([note, message]);
    const desired = [want("note", "2"), want("message", "2", (child) => { child.text = "warning updated"; return true; })];
    const held = retainEngagedChildren(parent, desired, () => true);
    reconcileKeyedChildren(parent, held.wanted);
    expect(held.pending).toBe(true); expect(nodes[0]).toBe(note); expect(note.signature).toBe("1");
    expect(nodes[1]).toBe(message); expect(message.text).toBe("warning updated");
  });
  it("keeps unchanged nodes in place and appends new ones", () => {
    const a = node("a", "1");
    const b = node("b", "1");
    const { parent, nodes, built, want } = fakeParent([a, b]);
    reconcileKeyedChildren(parent, [want("a", "1"), want("b", "1"), want("c", "1")]);
    expect(nodes.map((child) => child.text)).toEqual(["a", "b", "c*"]);
    expect([a.detached, b.detached]).toEqual([0, 0]);
    expect(built).toHaveLength(1);
  });

  it("updates a changed node in place when it can", () => {
    const a = node("a", "1");
    const b = node("b", "1");
    const { parent, nodes, built, want } = fakeParent([a, b]);
    reconcileKeyedChildren(parent, [
      want("a", "1"),
      want("b", "2", (child) => {
        child.text = "b updated";
        return true;
      }),
    ]);
    expect(nodes[0]).toBe(a);
    expect(nodes[1]).toBe(b);
    expect([b.text, b.signature, a.detached, b.detached]).toEqual(["b updated", "2", 0, 0]);
    expect(built).toEqual([]);
  });

  it("rebuilds only a changed node it cannot update, where it stands", () => {
    const a = node("a", "1");
    const b = node("b", "1");
    const c = node("c", "1");
    const { parent, nodes, want } = fakeParent([a, b, c]);
    reconcileKeyedChildren(parent, [want("a", "1"), want("b", "2", () => false), want("c", "1")]);
    expect(nodes.map((child) => child.text)).toEqual(["a", "b*", "c"]);
    expect([a.detached, c.detached, b.detached]).toEqual([0, 0, 1]);
    expect(nodes[1].signature).toBe("2");
  });

  it("drops nodes that left the window and keeps the rest without moving them", () => {
    const a = node("a", "1");
    const b = node("b", "1");
    const c = node("c", "1");
    const { parent, nodes, want } = fakeParent([a, b, c]);
    reconcileKeyedChildren(parent, [want("b", "1"), want("c", "1"), want("d", "1")]);
    expect(nodes.map((child) => child.text)).toEqual(["b", "c", "d*"]);
    expect([a.detached, b.detached, c.detached]).toEqual([1, 0, 0]);
  });

  it("rebuilds a node that would have to move rather than moving a kept one", () => {
    const p = node("p", "1");
    const a = node("a", "1");
    const b = node("b", "1");
    const { parent, nodes, want } = fakeParent([p, a, b]);
    reconcileKeyedChildren(parent, [want("a", "1"), want("b", "1"), want("p", "1")]);
    expect(nodes.map((child) => child.text)).toEqual(["a", "b", "p*"]);
    expect([a.detached, b.detached]).toEqual([0, 0]);
  });

  it("removes nodes it did not build, and does nothing when nothing changed", () => {
    const foreign = node(undefined, "1");
    const { parent, nodes, built, want } = fakeParent([foreign]);
    const wanted = [want("a", "1"), want("b", "1")];
    reconcileKeyedChildren(parent, wanted);
    expect(nodes.map((child) => child.text)).toEqual(["a*", "b*"]);
    expect(foreign.detached).toBe(1);
    const before = [...nodes];
    reconcileKeyedChildren(parent, wanted);
    expect(nodes.map((child, index) => child === before[index])).toEqual([true, true]);
    expect(nodes.map((child) => child.detached)).toEqual([0, 0]);
    expect(built).toHaveLength(2);
  });
});
