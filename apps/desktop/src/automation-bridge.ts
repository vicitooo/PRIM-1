export type Prim1AutomationBridge = {
  core: {
    invoke: typeof import("@tauri-apps/api/core").invoke;
  };
  event: {
    listen: typeof import("@tauri-apps/api/event").listen;
  };
  window: {
    getCurrentWindow: typeof import("@tauri-apps/api/window").getCurrentWindow;
  };
};

export type AutomationBridgeTarget = {
  __TAURI__?: Prim1AutomationBridge;
};

export function exposeAutomationBridge(
  target: object,
  enabled: boolean,
  bridge: Prim1AutomationBridge,
): boolean {
  if ("__TAURI__" in target) {
    throw new Error("global Tauri bridge already exists");
  }
  if (!enabled) {
    return false;
  }

  const frozenBridge = Object.freeze({
    core: Object.freeze({ invoke: bridge.core.invoke }),
    event: Object.freeze({ listen: bridge.event.listen }),
    window: Object.freeze({ getCurrentWindow: bridge.window.getCurrentWindow }),
  });
  Object.defineProperty(target, "__TAURI__", {
    value: frozenBridge,
    configurable: false,
    enumerable: false,
    writable: false,
  });
  return true;
}
