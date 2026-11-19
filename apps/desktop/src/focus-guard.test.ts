import { describe, expect, it } from "vitest";

import { focusWithinActiveWindow } from "./focus-guard";
import mainSource from "./main.ts?raw";

describe("focusWithinActiveWindow", () => {
  it("never focuses anything while the window is in the background or minimized", () => {
    let focused = 0;
    const target = { focus: () => { focused += 1; } };
    expect(focusWithinActiveWindow(target, { hasFocus: () => false })).toBe(false);
    expect(focused).toBe(0);
  });

  it("focuses inside the window the user is in, and tolerates a missing target", () => {
    let focused = 0;
    const target = { focus: () => { focused += 1; } };
    expect(focusWithinActiveWindow(target, { hasFocus: () => true })).toBe(true);
    expect(focused).toBe(1);
    expect(focusWithinActiveWindow(null, { hasFocus: () => true })).toBe(false);
  });
});

describe("every focus move in main.ts", () => {
  // Claude PRIM-1 26a7f92 walk, finding 2: focus moves that can run after an
  // await or from a runtime event must not reach a background window. All of
  // them go through the guard, which changes nothing in the active window.
  it("goes through focusWithinActiveWindow", () => {
    const bare = mainSource.split("\n").filter((line) => /\.focus\(/.test(line));
    expect(bare).toEqual([]);
  });
});
