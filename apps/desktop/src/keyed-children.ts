/** Keyed child reconciliation for the Readable feed. A child that is still
    wanted and unchanged keeps its node — and with it the reader's text
    selection, focus and scroll anchor. A changed child is updated in place
    when it can be, otherwise rebuilt where it stands. A kept node is never
    moved: moving detaches it, and a detached node loses selection and focus. */

export interface KeyedChild<N> {
  /** Unique among its siblings. */
  key: string;
  /** Everything the child shows; an equal signature means nothing to redraw. */
  signature: string;
  build(): N;
  /** Bring an existing node up to date in place; false to rebuild it. */
  update?(node: N): boolean;
}

export interface KeyedParent<N> {
  children(): readonly N[];
  /** Undefined for a node this reconciler did not build: it is removed. */
  keyOf(node: N): string | undefined;
  signatureOf(node: N): string | undefined;
  setSignature(node: N, signature: string): void;
  insertBefore(node: N, before: N | null): void;
  remove(node: N): void;
}

/** Automatic updates may keep a node the reader is using even when a new
 * projection omits it. New siblings still reconcile immediately. The caller
 * reruns the current projection on release; this retains no event backlog.
 * Existing wanted children must keep relative order (as room projections do).
 * Explicit navigation bypasses this helper. */
export function retainEngagedChildren<N>(parent: KeyedParent<N>, wanted: readonly KeyedChild<N>[],
  engaged: (node: N) => boolean): { wanted: KeyedChild<N>[]; pending: boolean } {
  const current = parent.children();
  const order = new Map(current.map((node, index) => [parent.keyOf(node), index]));
  const result = [...wanted];
  let pending = false;
  for (const node of current) {
    const key = parent.keyOf(node);
    if (key === undefined || !engaged(node)) continue;
    const at = result.findIndex((child) => child.key === key);
    const oldSignature = parent.signatureOf(node) ?? "";
    if (at >= 0) {
      // In-place updaters preserve body/control nodes and can still publish
      // receipt warnings. Only a destructive replacement is deferred here.
      if (result[at].signature !== oldSignature && !result[at].update) {
        result[at] = { key, signature: oldSignature, build: () => node };
        pending = true;
      }
      continue;
    }
    const oldIndex = order.get(key)!;
    const next = result.findIndex((child) => (order.get(child.key) ?? -1) > oldIndex);
    result.splice(next < 0 ? result.length : next, 0, { key, signature: oldSignature, build: () => node });
    pending = true;
  }
  return { wanted: result, pending };
}

export function reconcileKeyedChildren<N>(
  parent: KeyedParent<N>,
  wanted: readonly KeyedChild<N>[],
): void {
  const current = parent.children();
  const existing = new Map<string, { node: N; index: number }>();
  current.forEach((node, index) => {
    const key = parent.keyOf(node);
    if (key !== undefined && !existing.has(key)) {
      existing.set(key, { node, index });
    }
  });

  // Keep, in their current order, the nodes still wanted in that order.
  const kept = new Map<string, N>();
  let lastIndex = -1;
  for (const child of wanted) {
    const found = existing.get(child.key);
    // Keeping a node out of order would mean moving it; rebuild it instead.
    if (!found || found.index < lastIndex) continue;
    if (parent.signatureOf(found.node) !== child.signature) {
      if (!child.update?.(found.node)) continue;
      parent.setSignature(found.node, child.signature);
    }
    kept.set(child.key, found.node);
    lastIndex = found.index;
  }

  for (const node of current) {
    const key = parent.keyOf(node);
    if (key === undefined || kept.get(key) !== node) {
      parent.remove(node);
    }
  }

  // Insert from the back, each new node before its wanted successor: the kept
  // nodes are already in order, so none of them moves.
  let following: N | null = null;
  for (let index = wanted.length - 1; index >= 0; index -= 1) {
    const child = wanted[index];
    let node = kept.get(child.key);
    if (node === undefined) {
      node = child.build();
      parent.setSignature(node, child.signature);
      parent.insertBefore(node, following);
    }
    following = node;
  }
}
