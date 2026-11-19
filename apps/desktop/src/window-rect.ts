/** What the window-rect settle may do for the window as it is now. */
export type WindowRectAction = "none" | "show" | "resize";

export interface WindowRectState {
  fullscreen: boolean;
  maximized: boolean;
  minimized: boolean;
  visible: boolean;
  /** Outer size and position in physical px. */
  width: number;
  height: number;
  x: number;
  y: number;
  /** Screen work area in logical (CSS) px, and the device pixel ratio. */
  availWidth: number;
  availHeight: number;
  dpr: number;
}

/**
 * v3 W6 (E): a minimized window is left exactly as it is. Windows reports a
 * minimized window at about -32000,-32000 with a tiny size, which looks like
 * the degenerate rect this settle repairs; resizing or centring it, or
 * showing it, restores the window and takes the user's focus (Victor stopped
 * a minimized test candidate for this, 25 Oct). Fullscreen and maximized are
 * left alone too.
 */
export function windowRectAction(state: WindowRectState): WindowRectAction {
  if (state.minimized || state.fullscreen || state.maximized) {
    return "none";
  }
  if (!state.visible) {
    return "show";
  }
  const degenerate = state.width < 640 || state.height < 400 || state.x < -4000 || state.y < -4000;
  // Tolerate the invisible resize border (about 7 px logical per side).
  const oversized = state.width > (state.availWidth + 24) * state.dpr
    || state.height > (state.availHeight + 24) * state.dpr;
  return degenerate || oversized ? "resize" : "none";
}
