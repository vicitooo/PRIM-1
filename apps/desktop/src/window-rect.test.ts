import { describe, expect, it } from "vitest";

import { windowRectAction, type WindowRectState } from "./window-rect";

const normal: WindowRectState = {
  fullscreen: false, maximized: false, minimized: false, visible: true,
  width: 1500, height: 950, x: 40, y: 30, availWidth: 1920, availHeight: 1040, dpr: 1,
};

describe("windowRectAction", () => {
  it("leaves a minimized window alone even though Windows reports it far off-screen and tiny", () => {
    // What Windows reports for a minimized window: about -32000,-32000, 160x28.
    expect(windowRectAction({ ...normal, minimized: true, width: 160, height: 28, x: -32000, y: -32000 })).toBe("none");
    expect(windowRectAction({ ...normal, minimized: true, visible: false })).toBe("none");
  });

  it("leaves fullscreen and maximized windows alone", () => {
    expect(windowRectAction({ ...normal, fullscreen: true, width: 10 })).toBe("none");
    expect(windowRectAction({ ...normal, maximized: true, x: -32000 })).toBe("none");
  });

  it("shows a window that came back hidden, and repairs a degenerate or oversized restored rect", () => {
    expect(windowRectAction({ ...normal, visible: false })).toBe("show");
    expect(windowRectAction({ ...normal, width: 159, height: 27, x: -25600, y: -25600 })).toBe("resize");
    expect(windowRectAction({ ...normal, width: 4000 })).toBe("resize");
    expect(windowRectAction(normal)).toBe("none");
  });
});
