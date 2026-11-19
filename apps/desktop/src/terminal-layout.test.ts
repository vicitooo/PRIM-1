import { describe, expect, it, vi } from "vitest";
import {
  initialTerminalGrid, InitialReplayFence, repaintTerminalGrid, RunGridRecovery,
  terminalGridErrorAllowsCompletion, terminalHostIsVisible,
  TerminalLayoutScheduler, TerminalResizeState,
} from "./terminal-layout";

function hostFixture() {
  const rect = { left: 0, top: 0, right: 800, bottom: 500, width: 800, height: 500 };
  const style = { visibility: "visible" };
  const doc = {
    visibilityState: "visible",
    defaultView: { innerWidth: 1280, innerHeight: 800, getComputedStyle: () => style },
  };
  const host = {
    ownerDocument: doc,
    isConnected: true,
    closest: vi.fn((): unknown => null),
    getBoundingClientRect: vi.fn(() => rect),
  };
  return { host: host as unknown as HTMLElement, raw: host, doc, rect, style };
}

describe("terminal layout visibility", () => {
  it("refuses hidden documents, disconnected hosts and hidden/inert ancestors before measuring", () => {
    const fixture = hostFixture();
    fixture.doc.visibilityState = "hidden";
    expect(terminalHostIsVisible(fixture.host)).toBe(false);
    fixture.doc.visibilityState = "visible";
    fixture.raw.isConnected = false;
    expect(terminalHostIsVisible(fixture.host)).toBe(false);
    fixture.raw.isConnected = true;
    fixture.raw.closest.mockReturnValue({ hidden: true });
    expect(terminalHostIsVisible(fixture.host)).toBe(false);
    expect(fixture.raw.getBoundingClientRect).not.toHaveBeenCalled();
  });

  it("refuses CSS-hidden, zero-size and offscreen hosts, then permits a restored host", () => {
    const { host, rect, style } = hostFixture();
    style.visibility = "hidden";
    expect(terminalHostIsVisible(host)).toBe(false);
    style.visibility = "visible";
    rect.width = 0; // display:none ancestor, including Rooms/Settings shell views
    expect(terminalHostIsVisible(host)).toBe(false);
    rect.width = 800;
    rect.left = 1280;
    expect(terminalHostIsVisible(host)).toBe(false);
    rect.left = 0;
    expect(terminalHostIsVisible(host)).toBe(true);
  });
});

function schedulerFixture() {
  let visible = true;
  let next = 0;
  const frames = new Map<number, () => void>();
  const layout = vi.fn();
  const scheduler = new TerminalLayoutScheduler({
    visible: () => visible,
    requestFrame: (callback) => { frames.set(++next, callback); return next; },
    cancelFrame: (id) => { frames.delete(id); },
    layout,
  });
  return {
    scheduler, layout, frames,
    visibility: (value: boolean) => { visible = value; },
    frame: () => {
      const pending = [...frames.values()];
      frames.clear();
      for (const callback of pending) callback();
    },
  };
}

describe("terminal layout scheduling", () => {
  it("coalesces observer/snapshot requests without replacing the target's pending shown callback", () => {
    const { scheduler, layout, frames, frame } = schedulerFixture();
    const shown = vi.fn(() => expect(layout).toHaveBeenCalledTimes(1));
    scheduler.request({ target: "a", shown });
    scheduler.request();
    scheduler.request();
    expect(frames.size).toBe(1);
    frame();
    expect(layout).toHaveBeenCalledTimes(1);
    expect(shown).toHaveBeenCalledTimes(1);
  });

  it("fits the latest activation once and never completes an older switch", () => {
    const { scheduler, layout, frame } = schedulerFixture();
    const oldShown = vi.fn();
    const latestShown = vi.fn();
    scheduler.request({ target: "a", shown: oldShown });
    scheduler.request({ target: "b", shown: latestShown });
    frame();
    expect(layout).toHaveBeenCalledTimes(1);
    expect(oldShown).not.toHaveBeenCalled();
    expect(latestShown).toHaveBeenCalledTimes(1);
  });

  it("cancels hidden-window work and fits once after restore, even with queued observers", () => {
    const { scheduler, layout, frames, frame, visibility } = schedulerFixture();
    scheduler.request();
    visibility(false);
    scheduler.visibilityChanged();
    scheduler.request();
    expect(frames.size).toBe(0);
    frame();
    expect(layout).not.toHaveBeenCalled();
    visibility(true);
    scheduler.visibilityChanged();
    scheduler.request();
    frame();
    expect(layout).toHaveBeenCalledTimes(1);
  });

  it("rechecks a late hidden transition at flush without reporting a pane shown", () => {
    const { scheduler, layout, frame, visibility } = schedulerFixture();
    const shown = vi.fn();
    scheduler.request({ target: "a", shown });
    visibility(false);
    frame();
    expect(layout).not.toHaveBeenCalled();
    expect(shown).not.toHaveBeenCalled();
    visibility(true);
    scheduler.visibilityChanged();
    frame();
    expect(shown).toHaveBeenCalledTimes(1);
  });

  it("same-target focus preserves shown; a different target replaces both bounded slots", () => {
    const { scheduler, frame } = schedulerFixture();
    const shown = vi.fn();
    const firstFocus = vi.fn();
    const finalFocus = vi.fn();
    scheduler.request({ target: "a", shown });
    scheduler.request({ target: "a", focus: firstFocus });
    scheduler.request({ target: "a", focus: finalFocus });
    frame();
    expect(shown).toHaveBeenCalledTimes(1);
    expect(firstFocus).not.toHaveBeenCalled();
    expect(finalFocus).toHaveBeenCalledTimes(1);
    scheduler.request({ target: "a", shown, focus: firstFocus });
    scheduler.request({ target: "b" });
    frame();
    expect(shown).toHaveBeenCalledTimes(1);
    expect(firstFocus).not.toHaveBeenCalled();
  });

  it.each(["ordinary resize", "repaint"])("settles focus/shown after failed %s without retrying layout", () => {
    const { scheduler, layout, frame, frames } = schedulerFixture();
    const focus = vi.fn();
    const shown = vi.fn();
    layout.mockReturnValue(false); // outstanding operation made the RAF defer
    scheduler.request({ target: "a", focus, shown });
    frame();
    expect(focus).not.toHaveBeenCalled();
    scheduler.request(); // a queued observer must not turn failure settlement into a retry
    scheduler.settleWithoutLayout("a", () => true);
    expect(frames.size).toBe(0);
    frame();
    expect(layout).toHaveBeenCalledTimes(1);
    expect(focus).toHaveBeenCalledTimes(1);
    expect(shown).toHaveBeenCalledTimes(1);
    scheduler.settleWithoutLayout("a", () => true);
    expect(shown).toHaveBeenCalledTimes(1);
    // An independent later user layout request may try recovery again.
    scheduler.request();
    frame();
    expect(layout).toHaveBeenCalledTimes(2);
  });

  it("failure settlement refuses stale runs, hidden panes and superseding targets", () => {
    const { scheduler, layout, frame, visibility } = schedulerFixture();
    const oldShown = vi.fn();
    const latestShown = vi.fn();
    layout.mockReturnValue(false);
    scheduler.request({ target: "a", shown: oldShown });
    frame();
    scheduler.settleWithoutLayout("a", () => false); // stale run or hidden host
    visibility(false);
    scheduler.settleWithoutLayout("a", () => true); // hidden document
    expect(oldShown).not.toHaveBeenCalled();
    visibility(true);
    scheduler.request({ target: "b", shown: latestShown });
    scheduler.settleWithoutLayout("a", () => true);
    expect(latestShown).not.toHaveBeenCalled();
    scheduler.settleWithoutLayout("b", () => true);
    expect(latestShown).toHaveBeenCalledTimes(1);
    expect(oldShown).not.toHaveBeenCalled();
    expect(layout).toHaveBeenCalledTimes(1);
  });
});

describe("bounded authoritative run-grid recovery", () => {
  it("a hidden failure can activate and complete once without fit, repaint or another recovery RPC", async () => {
    const recovery = new RunGridRecovery();
    let errorRun: string | null = null;
    let visible = false;
    const currentRun = "2:run-a";
    const load = vi.fn().mockRejectedValue(new Error("offline"));
    recovery.adopt(currentRun);
    await recovery.request(currentRun, load, () => { errorRun = currentRun; });
    expect(load).toHaveBeenCalledTimes(2);
    const frames: (() => void)[] = [];
    const measureAndFit = vi.fn();
    const shown = vi.fn();
    const focus = vi.fn();
    const scheduler = new TerminalLayoutScheduler({
      visible: () => true,
      requestFrame: (callback) => { frames.push(callback); return frames.length; },
      cancelFrame: () => {},
      layout: () => {
        if (terminalGridErrorAllowsCompletion(errorRun, currentRun, visible, false)) return true;
        measureAndFit();
        return false;
      },
    });
    expect(terminalGridErrorAllowsCompletion(errorRun, currentRun, visible, false)).toBe(false);
    visible = true;
    scheduler.request({ target: "a", shown, focus });
    frames.shift()!();
    scheduler.request();
    frames.shift()!();
    expect(focus).toHaveBeenCalledTimes(1);
    expect(shown).toHaveBeenCalledTimes(1);
    expect(measureAndFit).not.toHaveBeenCalled();
    expect(load).toHaveBeenCalledTimes(2);
    expect(terminalGridErrorAllowsCompletion(null, currentRun, true, false)).toBe(false);
    expect(terminalGridErrorAllowsCompletion(errorRun, "3:run-b", true, false)).toBe(false);
    expect(terminalGridErrorAllowsCompletion(errorRun, currentRun, true, true)).toBe(false);
  });

  it("fails twice, ignores later noisy output and allows an explicit retry for the same run", async () => {
    const recovery = new RunGridRecovery();
    const failed = vi.fn();
    const loaded = vi.fn();
    const load = vi.fn()
      .mockRejectedValueOnce(new Error("bootstrap unavailable"))
      .mockRejectedValueOnce(new Error("retry unavailable"))
      .mockImplementationOnce(async (current: () => boolean) => { if (current()) loaded(); });
    recovery.adopt("2:run-a"); // content/lifecycle adoption, not a grid observation
    await recovery.request("2:run-a", load, failed);
    expect(load).toHaveBeenCalledTimes(2);
    expect(failed).toHaveBeenCalledTimes(1);
    expect(loaded).not.toHaveBeenCalled();
    for (let output = 0; output < 100; output++) {
      recovery.adopt("2:run-a");
      await recovery.request("2:run-a", load, failed);
    }
    expect(load).toHaveBeenCalledTimes(2);
    await recovery.request("2:run-a", load, failed, true);
    expect(load).toHaveBeenCalledTimes(3);
    expect(loaded).toHaveBeenCalledTimes(1);
    expect(failed).toHaveBeenCalledTimes(1);
  });

  it("recovers on its single automatic retry and does not duplicate concurrent requests", async () => {
    const recovery = new RunGridRecovery();
    const failed = vi.fn();
    let rejectFirst!: (reason: unknown) => void;
    const load = vi.fn()
      .mockImplementationOnce(() => new Promise<void>((_, reject) => { rejectFirst = reject; }))
      .mockResolvedValueOnce(undefined);
    recovery.adopt("2:run-a");
    const pending = recovery.request("2:run-a", load, failed);
    await recovery.request("2:run-a", load, failed, true);
    expect(load).toHaveBeenCalledTimes(1);
    rejectFirst(new Error("first request failed"));
    await pending;
    expect(load).toHaveBeenCalledTimes(2);
    expect(failed).not.toHaveBeenCalled();
  });

  it.each(["replacement", "disposed", "external recovery"])("invalidates pending replies and retries after %s", async (reason) => {
    const recovery = new RunGridRecovery();
    const failed = vi.fn();
    let reject!: (reason: unknown) => void;
    let requestIsCurrent!: () => boolean;
    const load = vi.fn((current: () => boolean) => {
      requestIsCurrent = current;
      return new Promise<void>((_, rejectRequest) => { reject = rejectRequest; });
    });
    recovery.adopt("2:run-a");
    const pending = recovery.request("2:run-a", load, failed);
    if (reason === "replacement") recovery.adopt("3:run-b");
    else if (reason === "disposed") recovery.dispose();
    else recovery.resolved("2:run-a");
    expect(requestIsCurrent()).toBe(false);
    reject(new Error("old request failed"));
    await pending;
    expect(load).toHaveBeenCalledTimes(1);
    expect(failed).not.toHaveBeenCalled();
    const fresh = vi.fn(async () => {});
    await recovery.request("2:run-a", fresh, failed);
    expect(fresh).not.toHaveBeenCalled();
  });
});

describe("initial replay parser fence", () => {
  it("activation before deferred replay drain cannot resize; later live output cannot extend the cutoff", () => {
    const fence = new InitialReplayFence();
    const parser: (() => void)[] = [];
    const parsed: string[] = [];
    const frames: (() => void)[] = [];
    const resize = vi.fn();
    const shown = vi.fn();
    const scheduler = new TerminalLayoutScheduler({
      visible: () => true,
      requestFrame: (callback) => { frames.push(callback); return frames.length; },
      cancelFrame: () => {},
      layout: () => { if (!fence.ready) return false; resize(); return true; },
    });
    // Synchronous pending-buffer and bootstrap event replay precede the seal.
    parser.push(() => parsed.push("pending"), () => parsed.push("bootstrap"));
    fence.seal((done) => parser.push(done), () => scheduler.request());
    parser.push(() => parsed.push("live-after-cutoff"));
    scheduler.request({ target: "a", shown });
    frames.shift()!();
    expect(resize).not.toHaveBeenCalled();
    expect(shown).not.toHaveBeenCalled();
    parser.shift()!();
    parser.shift()!();
    expect(fence.ready).toBe(false);
    parser.shift()!(); // initial replay's parser marker
    frames.shift()!();
    expect(resize).toHaveBeenCalledTimes(1);
    expect(shown).toHaveBeenCalledTimes(1);
    expect(parsed).toEqual(["pending", "bootstrap"]);
    expect(parser).toHaveLength(1); // ongoing live parsing does not own the fence
  });

  it("no-output/filtered-empty replay releases once, including synchronous parser callbacks", () => {
    const fence = new InitialReplayFence();
    const released = vi.fn();
    const barrier = vi.fn((done: () => void) => done());
    fence.seal(barrier, released);
    fence.seal(barrier, released);
    expect(fence.ready).toBe(true);
    expect(barrier).toHaveBeenCalledTimes(1);
    expect(released).toHaveBeenCalledTimes(1);
  });

  it("stale run/disposal callbacks cannot release a replacement hold or schedule work", () => {
    const fence = new InitialReplayFence();
    const callbacks: (() => void)[] = [];
    const released = vi.fn();
    fence.seal((done) => callbacks.push(done), released);
    fence.reset();
    fence.seal((done) => callbacks.push(done), released);
    callbacks[0]();
    expect(fence.ready).toBe(false);
    expect(released).not.toHaveBeenCalled();
    callbacks[1]();
    expect(fence.ready).toBe(true);
    fence.reset();
    fence.seal((done) => callbacks.push(done), released);
    fence.dispose();
    callbacks[2]();
    expect(released).toHaveBeenCalledTimes(1);
    expect(fence.ready).toBe(false);
  });

  it("submission or completion errors cannot strand the hold or abort xterm's queue", () => {
    const fence = new InitialReplayFence();
    const error = new Error("write rejected");
    const released = vi.fn(() => { throw new Error("probe failed"); });
    expect(() => fence.seal(() => { throw error; }, released)).not.toThrow();
    expect(fence.ready).toBe(true);
    expect(released).toHaveBeenCalledWith(error);
    fence.reset();
    expect(() => fence.seal((done) => done(), released)).not.toThrow();
    expect(fence.ready).toBe(true);
  });
});

describe("confirmed terminal resize and repaint", () => {
  it.each([
    { outcomes: [false, true], healed: false, finalRows: 40 },
    { outcomes: [true, false], healed: false, finalRows: 39 },
    { outcomes: [true, true], healed: true, finalRows: 40 },
  ])("keeps recovery unless shrink and restore both succeed: $outcomes", async ({ outcomes, healed, finalRows }) => {
    const state = new TerminalResizeState("run:1", { cols: 120, rows: 40 });
    const ticket = state.begin()!;
    const resize = vi.fn(async () => outcomes.shift()!);
    const result = await repaintTerminalGrid(
      { cols: 120, rows: 40 }, resize, () => state.current(ticket),
      (grid) => state.confirm(ticket, grid),
    );
    expect(result).toBe(healed);
    expect(resize.mock.calls).toHaveLength(2);
    expect(state.finish(ticket)).toEqual({ cols: 120, rows: finalRows });
  });

  it("a refused optimistic resize rolls back after hide; stale completion cannot roll back a replacement run", () => {
    const original = { cols: 120, rows: 40 };
    const state = new TerminalResizeState("run:1", original);
    let terminal = { cols: 180, rows: 50 }; // optimistic xterm resize
    const request = state.begin()!;
    expect(state.begin()).toBeNull(); // no overlapping resize can invalidate the rollback baseline
    // There is no visibility condition on the rejection's local rollback.
    terminal = state.finish(request)!;
    expect(terminal).toEqual(original);
    const old = state.begin()!;
    state.adopt("run:2", { cols: 196, rows: 22 });
    const current = state.begin()!;
    state.confirm(old, { cols: 180, rows: 50 });
    expect(state.finish(old)).toBeNull();
    expect(state.current(current)).toBe(true);
    expect(state.confirmed).toEqual({ cols: 196, rows: 22 });
    state.dispose();
    expect(state.finish(current)).toBeNull();
  });

  it("completes an admitted restore after the pane is hidden", async () => {
    const state = new TerminalResizeState("run:1", { cols: 120, rows: 40 });
    const ticket = state.begin()!;
    let visible = true;
    let completeShrink!: (value: boolean) => void;
    const visibilityAtResize: boolean[] = [];
    const resize = vi.fn(async () => {
      visibilityAtResize.push(visible);
      if (visibilityAtResize.length === 1) return new Promise<boolean>((resolve) => { completeShrink = resolve; });
      return true;
    });
    const repaint = repaintTerminalGrid(
      state.confirmed, resize, () => state.current(ticket), (grid) => state.confirm(ticket, grid),
    );
    visible = false;
    completeShrink(true);
    expect(await repaint).toBe(true);
    expect(visibilityAtResize).toEqual([true, false]);
    expect(state.finish(ticket)).toEqual({ cols: 120, rows: 40 });
  });

  it("never issues the old repaint's restore for a replacement run", async () => {
    const state = new TerminalResizeState("run:1", { cols: 120, rows: 40 });
    const ticket = state.begin()!;
    let completeShrink!: (value: boolean) => void;
    const resize = vi.fn(() => new Promise<boolean>((resolve) => { completeShrink = resolve; }));
    const repaint = repaintTerminalGrid(
      state.confirmed, resize, () => state.current(ticket), (grid) => state.confirm(ticket, grid),
    );
    state.adopt("run:2", { cols: 196, rows: 22 });
    completeShrink(true);
    expect(await repaint).toBe(false);
    expect(resize).toHaveBeenCalledTimes(1);
    expect(state.confirmed).toEqual({ cols: 196, rows: 22 });
  });

  it("tries restoration even when the shrink promise rejects", async () => {
    const resize = vi.fn().mockRejectedValueOnce(new Error("shrink")).mockResolvedValueOnce(true);
    const confirmed = vi.fn();
    expect(await repaintTerminalGrid({ cols: 120, rows: 40 }, resize, () => true, confirmed)).toBe(false);
    expect(resize).toHaveBeenCalledTimes(2);
    expect(confirmed).toHaveBeenCalledWith({ cols: 120, rows: 40 });
  });
});

describe("authoritative initial terminal grid", () => {
  it("uses the saved PTY grid before any visible fit or output replay", () => {
    expect(initialTerminalGrid([196, 22])).toEqual({ cols: 196, rows: 22 });
    expect(initialTerminalGrid([120, 40])).toEqual({ cols: 120, rows: 40 });
  });

  it.each([undefined, null, [], [80], [0, 24], [80, 0], [80.5, 24], [65536, 24]].map((size) => ({ size })))(
    "does not invent a grid for an unknown or malformed snapshot: %j",
    ({ size }) => expect(() => initialTerminalGrid(size)).toThrow("Terminal grid is unavailable"),
  );
});
