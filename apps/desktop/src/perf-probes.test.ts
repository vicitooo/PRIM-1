import { describe, expect, it } from "vitest";

import { PerfProbes, exposePerfProbes, percentileStats } from "./perf-probes";

function harness() {
  let now = 0;
  let frames: Array<() => void> = [];
  const timers: Array<{ at: number; callback: () => void }> = [];
  const probes = new PerfProbes(
    () => now,
    (callback) => frames.push(callback),
    (callback, ms) => timers.push({ at: now + ms, callback }),
  );
  return {
    probes,
    advance: (ms: number) => {
      now += ms;
      for (const timer of timers.filter((entry) => entry.at <= now)) {
        timers.splice(timers.indexOf(timer), 1);
        timer.callback();
      }
    },
    flushFrame: () => {
      const due = frames;
      frames = [];
      for (const callback of due) {
        callback();
      }
    },
    pendingFrames: () => frames.length,
  };
}

function armedHarness() {
  const h = harness();
  h.probes.arm();
  return h;
}

const PROMPT = "PS D:\\work> ";

describe("perf probes", () => {
  it("computes nearest-rank percentiles", () => {
    expect(percentileStats([])).toEqual({ count: 0, p50: 0, p95: 0, p99: 0, max: 0 });
    const values = Array.from({ length: 100 }, (_, index) => index + 1);
    expect(percentileStats(values)).toEqual({ count: 100, p50: 50, p95: 95, p99: 99, max: 100 });
  });

  it("records nothing while disarmed", () => {
    const { probes, flushFrame } = harness();
    probes.inputAdmitted("s1", "f", () => PROMPT);
    probes.outputParsed("s1", () => PROMPT + "f");
    flushFrame();
    expect(probes.snapshot().input_echo_ms.count).toBe(0);
  });

  it("times admission to the frame after the rendered line equals prompt plus typed text", () => {
    const { probes, advance, flushFrame } = armedHarness();
    probes.inputAdmitted("s1", "f", () => PROMPT);
    advance(30);
    probes.outputParsed("s1", () => PROMPT + "f");
    advance(16);
    flushFrame();
    expect(probes.snapshot().input_echo_ms).toMatchObject({ count: 1, max: 46 });
  });

  it("does not count unrelated output that holds or ends with the typed text (Codex P1)", () => {
    const { probes, flushFrame } = armedHarness();
    probes.inputAdmitted("s1", "s", () => PROMPT);
    probes.outputParsed("s1", () => "background status");
    probes.outputParsed("s1", () => "PS D:\\other> s");
    flushFrame();
    expect(probes.snapshot().input_echo_ms.count).toBe(0);
  });

  it("does not count a split escape sequence as an echo (Codex P1)", () => {
    const { probes, flushFrame } = armedHarness();
    probes.inputAdmitted("s1", "1", () => PROMPT);
    probes.outputParsed("s1", () => PROMPT);
    flushFrame();
    expect(probes.snapshot().input_echo_ms.count).toBe(0);
  });

  it("credits every earlier keystroke when one render shows several", () => {
    const { probes, advance, flushFrame } = armedHarness();
    probes.inputAdmitted("s1", "f", () => PROMPT);
    advance(10);
    probes.inputAdmitted("s1", "i", () => "ignored after the anchor");
    advance(10);
    probes.outputParsed("s1", () => PROMPT + "fi");
    flushFrame();
    expect(probes.snapshot().input_echo_ms).toMatchObject({ count: 2, max: 20, p50: 10 });
  });

  it("ignores the terminal's own cursor-position replies in the input path", () => {
    const { probes, flushFrame } = armedHarness();
    probes.inputAdmitted("s1", "f", () => PROMPT);
    probes.inputAdmitted("s1", "\x1b[5;12R", () => PROMPT);
    probes.inputAdmitted("s1", "i", () => PROMPT);
    probes.outputParsed("s1", () => PROMPT + "fi");
    flushFrame();
    expect(probes.snapshot().input_echo_ms.count).toBe(2);
    expect(probes.snapshot().dropped.input_line_reset).toBe(0);
  });

  it("stops correlating after Enter and counts what it dropped", () => {
    const { probes, flushFrame } = armedHarness();
    probes.inputAdmitted("s1", "ab\r", () => PROMPT);
    probes.outputParsed("s1", () => PROMPT + "ab");
    flushFrame();
    expect(probes.snapshot().input_echo_ms.count).toBe(0);
    expect(probes.snapshot().dropped.input_line_reset).toBe(2);
  });

  it("caps an unbounded typed line (Codex)", () => {
    const { probes } = armedHarness();
    probes.inputAdmitted("s1", "x".repeat(600), () => PROMPT);
    expect(probes.snapshot().dropped.input_line_cap).toBeGreaterThan(0);
  });

  it("drops callbacks queued before a reset (Codex P2)", () => {
    const { probes, flushFrame } = armedHarness();
    probes.inputAdmitted("s1", "x", () => PROMPT);
    probes.outputParsed("s1", () => PROMPT + "x");
    probes.reset();
    flushFrame();
    expect(probes.snapshot().input_echo_ms.count).toBe(0);
  });

  it("drops an echo queued before disarm when re-armed (Codex)", () => {
    const { probes, flushFrame } = armedHarness();
    probes.inputAdmitted("s1", "x", () => PROMPT);
    probes.outputParsed("s1", () => PROMPT + "x");
    probes.disarm();
    probes.arm();
    flushFrame();
    expect(probes.snapshot().input_echo_ms.count).toBe(0);
  });

  it("restarts the frame origin on an armed reset (Codex)", () => {
    const { probes, advance, flushFrame } = armedHarness();
    flushFrame();
    advance(500);
    probes.reset();
    flushFrame();
    advance(16);
    flushFrame();
    const frames = probes.snapshot().frame_ms;
    expect(frames.count).toBe(1);
    expect(frames.max).toBe(16);
  });

  it("re-arming never runs two frame loops (Codex P2)", () => {
    const { probes, advance, flushFrame, pendingFrames } = harness();
    probes.arm();
    probes.disarm();
    probes.arm();
    flushFrame();
    for (let index = 0; index < 4; index += 1) {
      expect(pendingFrames()).toBe(1);
      advance(16);
      flushFrame();
    }
    probes.disarm();
    const frames = probes.snapshot().frame_ms;
    expect(frames.p50).toBe(16);
    expect(frames.count).toBe(4);
  });

  it("times a switch to the frame after the target terminal renders", () => {
    const { probes, advance, flushFrame } = armedHarness();
    flushFrame();
    let render: (() => void) | null = null;
    let cleanups = 0;
    probes.switchStarted();
    advance(40);
    probes.switchStarted(); // the inner setActiveSession call does not restart it
    probes.switchTarget((rendered) => {
      render = rendered;
      return () => { cleanups += 1; };
    });
    advance(30);
    render!();
    advance(16);
    flushFrame();
    expect(probes.snapshot().switch_paint_ms).toMatchObject({ count: 1, max: 86 });
    expect(probes.snapshot().switch_provenance).toEqual({ render_tied: 1, frame_estimate: 0 });
    expect(cleanups).toBe(1);
    advance(1_000);
    probes.disarm();
    expect(cleanups).toBe(1);
  });

  it("falls back to the frame after the pane is shown when its terminal never renders", () => {
    const { probes, advance, flushFrame } = armedHarness();
    flushFrame();
    let cleanups = 0;
    probes.switchStarted();
    const shown = probes.switchTarget(() => () => { cleanups += 1; })!;
    advance(20);
    shown();
    advance(16);
    flushFrame();
    advance(1_000);
    expect(probes.snapshot().switch_paint_ms).toMatchObject({ count: 1, max: 36 });
    expect(probes.snapshot().switch_provenance).toEqual({ render_tied: 0, frame_estimate: 1 });
    expect(cleanups).toBe(1);
    probes.reset();
    expect(cleanups).toBe(1);
  });

  it("counts a switch that neither rendered nor was shown instead of guessing", () => {
    const { probes, advance } = armedHarness();
    probes.switchStarted();
    probes.switchTarget(() => {});
    advance(1_000);
    expect(probes.snapshot().switch_paint_ms.count).toBe(0);
    expect(probes.snapshot().dropped.switch_no_render).toBe(1);
  });

  it("restarts correlation after Esc so the next burst on the cleared line is measured (Codex v4 review)", () => {
    const { probes, advance, flushFrame } = armedHarness();
    probes.inputAdmitted("s1", "abcdefghijkl", () => PROMPT);
    advance(50);
    probes.outputParsed("s1", () => PROMPT + "abcdefghijkl");
    advance(6);
    flushFrame();
    expect(probes.snapshot().input_echo_ms.count).toBe(12);
    probes.inputAdmitted("s1", "\x1b", () => PROMPT + "abcdefghijkl");
    // PowerShell cleared the line; the next burst starts at the bare prompt.
    probes.inputAdmitted("s1", "abcdefghijkl", () => PROMPT);
    advance(50);
    probes.outputParsed("s1", () => PROMPT + "abcdefghijkl");
    advance(6);
    flushFrame();
    const snapshot = probes.snapshot();
    expect(snapshot.input_echo_ms.count).toBe(24);
    expect(snapshot.dropped.input_unmatched).toBe(0);
    expect(snapshot.input_counts).toEqual({ admitted: 24, matched: 24, pending: 0, awaiting_frame: 0 });
  });

  it("treats arrow keys as a line edit but a cursor-position reply as no key", () => {
    const { probes } = armedHarness();
    probes.inputAdmitted("s1", "ab", () => PROMPT);
    probes.inputAdmitted("s1", "\x1b[3;14R\x1b[3;14R", () => PROMPT);
    expect(probes.snapshot().input_counts.pending).toBe(2);
    probes.inputAdmitted("s1", "\x1b[D", () => PROMPT);
    expect(probes.snapshot().input_counts.pending).toBe(0);
    expect(probes.snapshot().dropped.input_line_reset).toBe(2);
  });

  it("keeps the first shown frame when the same target is refreshed later (Codex v4 review)", () => {
    const { probes, advance, flushFrame } = armedHarness();
    flushFrame();
    probes.switchStarted();
    const shown = probes.switchTarget(() => {})!;
    shown();
    advance(16);
    flushFrame();
    advance(700);
    shown();
    advance(16);
    flushFrame();
    advance(300);
    const snapshot = probes.snapshot();
    expect(snapshot.switch_paint_ms).toMatchObject({ count: 1, max: 16 });
    expect(snapshot.switch_provenance).toEqual({ render_tied: 0, frame_estimate: 1 });
    expect(snapshot.dropped.switch_repeat_show).toBe(1);
  });

  it("reports version 6 and clears coverage counts on reset", () => {
    const { probes, flushFrame } = armedHarness();
    probes.inputAdmitted("s1", "x", () => PROMPT);
    probes.outputParsed("s1", () => PROMPT + "x");
    flushFrame();
    expect(probes.snapshot().version).toBe(6);
    expect(probes.snapshot().input_counts.matched).toBe(1);
    probes.reset();
    expect(probes.snapshot().input_counts).toEqual({ admitted: 0, matched: 0, pending: 0, awaiting_frame: 0 });
  });

  it("ignores an older target's delayed show and render while a newer target is pending", () => {
    const { probes, advance, flushFrame } = armedHarness();
    let renderA = () => {};
    probes.switchStarted();
    const shownA = probes.switchTarget(render => { renderA = render; })!;
    advance(100);
    probes.switchStarted();
    const shownB = probes.switchTarget(() => {})!;
    shownA(); renderA();
    advance(16); flushFrame();
    expect(probes.snapshot().switch_paint_ms.count).toBe(0);
    advance(84); shownB();
    advance(16); flushFrame();
    advance(1000);
    expect(probes.snapshot().switch_paint_ms).toMatchObject({ count: 1, max: 116 });
    expect(probes.snapshot().dropped.switch_cancelled).toBe(1);
  });

  it("drops canceled render frames and keeps an unchanged refresh bound to its original target", () => {
    const { probes, advance, flushFrame } = armedHarness();
    let render = () => {};
    probes.switchStarted();
    probes.switchTarget(callback => { render = callback; });
    render();
    probes.switchCancelled();
    advance(16); flushFrame();
    expect(probes.snapshot().switch_paint_ms.count).toBe(0);
    probes.switchStarted();
    const shown = probes.switchTarget(() => {})!;
    probes.switchCancelled(true); // unchanged active pane
    shown(); advance(16); flushFrame(); advance(1000);
    expect(probes.snapshot().switch_paint_ms).toMatchObject({ count: 1, max: 16 });
  });

  it("accounts for both unmatched and parsed-but-unpainted keys when disarming", () => {
    const { probes, flushFrame } = armedHarness();
    probes.inputAdmitted("s1", "ab", () => PROMPT);
    probes.outputParsed("s1", () => PROMPT + "a");
    expect(probes.snapshot().input_counts).toEqual({ admitted: 2, matched: 0, pending: 1, awaiting_frame: 1 });
    probes.disarm();
    let snapshot = probes.snapshot();
    expect(snapshot.input_counts).toEqual({ admitted: 2, matched: 0, pending: 0, awaiting_frame: 0 });
    expect(snapshot.dropped.input_disarmed).toBe(2);
    flushFrame();
    expect(probes.snapshot()).toEqual(snapshot);
    probes.arm();
    probes.inputAdmitted("s2", "c", () => PROMPT);
    probes.outputParsed("s2", () => PROMPT + "c");
    flushFrame();
    snapshot = probes.snapshot();
    expect(snapshot.input_counts.matched + snapshot.dropped.input_disarmed).toBe(snapshot.input_counts.admitted);
  });

  it("reset rejects old parsed-frame credit without changing the new window's accounting", () => {
    const { probes, flushFrame } = armedHarness();
    let cleanups = 0;
    probes.switchStarted();
    const shown = probes.switchTarget(() => () => { cleanups += 1; })!;
    probes.inputAdmitted("s1", "a", () => PROMPT);
    probes.outputParsed("s1", () => PROMPT + "a");
    probes.reset();
    expect(cleanups).toBe(1);
    shown();
    probes.inputAdmitted("s1", "b", () => PROMPT);
    probes.outputParsed("s1", () => PROMPT + "b");
    flushFrame();
    expect(probes.snapshot().input_counts).toEqual({ admitted: 1, matched: 1, pending: 0, awaiting_frame: 0 });
    expect(probes.snapshot().switch_paint_ms.count).toBe(0);
    expect(cleanups).toBe(1);
  });

  it("keeps a known first-frame estimate on an early next switch and releases render listeners", () => {
    const { probes, advance, flushFrame } = armedHarness();
    let cleanups = 0;
    probes.switchStarted();
    const shown = probes.switchTarget(() => () => { cleanups += 1; })!;
    shown(); advance(16); flushFrame();
    advance(600);
    probes.switchStarted();
    probes.switchTarget(() => () => { cleanups += 1; });
    expect(probes.snapshot().switch_paint_ms).toMatchObject({ count: 1, max: 16 });
    expect(cleanups).toBe(1);
    probes.disarm();
    expect(cleanups).toBe(2);
    advance(1000); flushFrame();
    expect(cleanups).toBe(2);
    expect(probes.snapshot().dropped.switch_cancelled).toBe(1);
  });

  it("never reanchors capped input to a stale prefix and accounts for every admission", () => {
    const { probes, flushFrame } = armedHarness();
    probes.inputAdmitted("s1", "a".repeat(514), () => PROMPT);
    probes.outputParsed("s1", () => PROMPT + "a");
    flushFrame();
    let snapshot = probes.snapshot();
    expect(snapshot.input_counts).toEqual({ admitted: 514, matched: 0, pending: 0, awaiting_frame: 0 });
    expect(snapshot.dropped.input_overflow + snapshot.dropped.input_line_cap).toBe(514);
    probes.inputAdmitted("s1", "\x1b", () => PROMPT);
    probes.inputAdmitted("s1", "b", () => PROMPT);
    probes.outputParsed("s1", () => PROMPT + "b");
    flushFrame();
    snapshot = probes.snapshot();
    expect(snapshot.input_counts.matched).toBe(1);
    expect(snapshot.input_counts.admitted).toBe(515);
  });

  it("is exposed only when enabled", () => {
    const target: Record<string, unknown> = {};
    expect(exposePerfProbes(target, false, new PerfProbes())).toBe(false);
    expect("__PRIM1_PERF__" in target).toBe(false);
    expect(exposePerfProbes(target, true, new PerfProbes())).toBe(true);
    expect(typeof (target.__PRIM1_PERF__ as { snapshot: unknown }).snapshot).toBe("function");
  });
});
