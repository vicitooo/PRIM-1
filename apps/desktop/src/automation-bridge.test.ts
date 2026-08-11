import { describe, expect, it, vi } from "vitest";

import {
  exposeAutomationBridge,
  type AutomationBridgeTarget,
  type Prim1AutomationBridge,
} from "./automation-bridge";

function bridge(): Prim1AutomationBridge {
  return {
    core: { invoke: vi.fn() as Prim1AutomationBridge["core"]["invoke"] },
    event: { listen: vi.fn() as Prim1AutomationBridge["event"]["listen"] },
    window: {
      getCurrentWindow:
        vi.fn() as Prim1AutomationBridge["window"]["getCurrentWindow"],
    },
  };
}

describe("automation bridge", () => {
  it("keeps the global bridge absent outside explicit automation mode", () => {
    const target: AutomationBridgeTarget = {};
    expect(exposeAutomationBridge(target, false, bridge())).toBe(false);
    expect("__TAURI__" in target).toBe(false);
  });

  it("exposes only the required frozen API in explicit automation mode", () => {
    const target: AutomationBridgeTarget = {};
    const api = bridge();

    expect(exposeAutomationBridge(target, true, api)).toBe(true);
    expect(target.__TAURI__).toEqual(api);
    expect(Object.isFrozen(target.__TAURI__)).toBe(true);
    expect(Object.isFrozen(target.__TAURI__?.core)).toBe(true);
    expect(Object.isFrozen(target.__TAURI__?.event)).toBe(true);
    expect(Object.isFrozen(target.__TAURI__?.window)).toBe(true);
    expect(Object.keys(target)).not.toContain("__TAURI__");
    expect(Object.getOwnPropertyDescriptor(target, "__TAURI__")).toMatchObject({
      configurable: false,
      enumerable: false,
      writable: false,
    });
  });

  it("fails closed instead of replacing any pre-existing bridge", () => {
    const target: AutomationBridgeTarget = { __TAURI__: bridge() };
    expect(() => exposeAutomationBridge(target, false, bridge())).toThrow(
      "global Tauri bridge already exists",
    );
    expect(() => exposeAutomationBridge(target, true, bridge())).toThrow(
      "global Tauri bridge already exists",
    );
  });
});
