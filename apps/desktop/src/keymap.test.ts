import { describe, expect, it } from "vitest";

import {
  KEY_BINDINGS,
  PLATFORMS,
  describeChord,
  helpRows,
  platformFromHints,
  resolveKeyAction,
  resolveKeyMatch,
  type KeyContext,
  type KeyInput,
  type Platform,
} from "./keymap";
import {
  resolveFocusedTabAction,
  resolveGlobalSessionShortcut,
} from "./session-tabs";

function key(
  key: string,
  modifiers: Partial<Omit<KeyInput, "key">> = {},
): KeyInput {
  return {
    key,
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    metaKey: false,
    ...modifiers,
  };
}

function context(overrides: Partial<KeyContext> = {}): KeyContext {
  return {
    editable: false,
    terminalFocused: true,
    tabFocused: false,
    hasSelection: () => false,
    ...overrides,
  };
}

const withSelection = context({ hasSelection: () => true });

describe("platform conventions", () => {
  it("Windows: Ctrl+C copies with a selection and is the interrupt without one", () => {
    expect(resolveKeyAction(key("c", { ctrlKey: true }), "windows", withSelection)).toBe("copy");
    expect(resolveKeyAction(key("c", { ctrlKey: true }), "windows", context())).toBeNull();
    // The bare chord is the one that clears the selection afterwards.
    expect(
      resolveKeyMatch(key("c", { ctrlKey: true }), "windows", withSelection)?.chord.requiresSelection,
    ).toBe(true);
    expect(
      resolveKeyMatch(key("C", { ctrlKey: true, shiftKey: true }), "windows", withSelection)?.chord
        .requiresSelection,
    ).toBeUndefined();
  });

  it("Windows: Ctrl+V, Ctrl+Shift+V and Shift+Insert paste; Ctrl+Insert copies", () => {
    expect(resolveKeyAction(key("v", { ctrlKey: true }), "windows", context())).toBe("paste");
    expect(resolveKeyAction(key("V", { ctrlKey: true, shiftKey: true }), "windows", context())).toBe(
      "paste",
    );
    expect(resolveKeyAction(key("Insert", { shiftKey: true }), "windows", context())).toBe("paste");
    expect(resolveKeyAction(key("Insert", { ctrlKey: true }), "windows", withSelection)).toBe("copy");
  });

  it("Linux: Ctrl+C and Ctrl+V are never consumed; Ctrl+Shift+C/V are", () => {
    expect(resolveKeyAction(key("c", { ctrlKey: true }), "linux", withSelection)).toBeNull();
    expect(resolveKeyAction(key("v", { ctrlKey: true }), "linux", context())).toBeNull();
    expect(resolveKeyAction(key("C", { ctrlKey: true, shiftKey: true }), "linux", withSelection)).toBe(
      "copy",
    );
    expect(resolveKeyAction(key("V", { ctrlKey: true, shiftKey: true }), "linux", context())).toBe(
      "paste",
    );
    expect(resolveKeyAction(key("PageDown", { ctrlKey: true }), "linux", context())).toBe("next-tab");
    expect(resolveKeyAction(key("PageUp", { ctrlKey: true }), "linux", context())).toBe(
      "previous-tab",
    );
    expect(resolveKeyAction(key("PageDown", { ctrlKey: true }), "windows", context())).toBeNull();
  });

  it("macOS: Cmd chords resolve and the Ctrl+Shift chords do not", () => {
    expect(resolveKeyAction(key("c", { metaKey: true }), "macos", withSelection)).toBe("copy");
    expect(resolveKeyAction(key("c", { metaKey: true }), "macos", context())).toBeNull();
    expect(resolveKeyAction(key("v", { metaKey: true }), "macos", context())).toBe("paste");
    expect(resolveKeyAction(key("t", { metaKey: true }), "macos", context())).toBe("new-session");
    expect(resolveKeyAction(key("w", { metaKey: true }), "macos", context())).toBe("close-session");
    expect(resolveKeyAction(key("}", { metaKey: true, shiftKey: true }), "macos", context())).toBe(
      "next-tab",
    );
    expect(resolveKeyAction(key("f", { ctrlKey: true, metaKey: true }), "macos", context())).toBe(
      "fullscreen",
    );
    expect(resolveKeyAction(key("C", { ctrlKey: true, shiftKey: true }), "macos", withSelection)).toBeNull();
    expect(resolveKeyAction(key("F11"), "macos", context())).toBeNull();
    // A Cmd chord means nothing on the other platforms.
    expect(resolveKeyAction(key("v", { metaKey: true }), "windows", context())).toBeNull();
    expect(resolveKeyAction(key("v", { metaKey: true }), "linux", context())).toBeNull();
  });

  it("shared chords: tabs, fullscreen, help", () => {
    for (const platform of ["windows", "linux"] as const) {
      expect(resolveKeyAction(key("Tab", { ctrlKey: true }), platform, context())).toBe("next-tab");
      expect(resolveKeyAction(key("Tab", { ctrlKey: true, shiftKey: true }), platform, context())).toBe(
        "previous-tab",
      );
      expect(resolveKeyAction(key("T", { ctrlKey: true, shiftKey: true }), platform, context())).toBe(
        "new-session",
      );
      expect(resolveKeyAction(key("w", { ctrlKey: true, shiftKey: true }), platform, context())).toBe(
        "close-session",
      );
      expect(resolveKeyAction(key("F11"), platform, context())).toBe("fullscreen");
    }
    for (const platform of PLATFORMS) {
      expect(resolveKeyAction(key("F1"), platform, context())).toBe("help");
      expect(resolveKeyAction(key("Escape"), platform, context())).toBe("close-menu");
    }
  });
});

describe("scopes", () => {
  it("editable targets never trigger copy, paste or tab chords", () => {
    const editable = context({ editable: true, hasSelection: () => true });
    expect(resolveKeyAction(key("c", { ctrlKey: true }), "windows", editable)).toBeNull();
    expect(resolveKeyAction(key("v", { ctrlKey: true }), "windows", editable)).toBeNull();
    expect(resolveKeyAction(key("Tab", { ctrlKey: true }), "windows", editable)).toBeNull();
    // Function keys and Escape stay live inside editable fields.
    expect(resolveKeyAction(key("F1"), "windows", editable)).toBe("help");
    expect(resolveKeyAction(key("F11"), "linux", editable)).toBe("fullscreen");
    expect(resolveKeyAction(key("Escape"), "macos", editable)).toBe("close-menu");
  });

  it("paste needs a focused terminal; copy does not (page text and the system log count)", () => {
    const unfocused = context({ terminalFocused: false });
    expect(resolveKeyAction(key("v", { ctrlKey: true }), "windows", unfocused)).toBeNull();
    expect(
      resolveKeyAction(key("c", { ctrlKey: true }), "windows", {
        ...unfocused,
        hasSelection: () => true,
      }),
    ).toBe("copy");
  });

  it("tab-scoped chords are live only from a focused tab", () => {
    expect(resolveKeyAction(key("ArrowRight"), "windows", context())).toBeNull();
    expect(resolveKeyAction(key("ArrowRight"), "windows", context({ tabFocused: true }))).toBe(
      "tab-select-next",
    );
    expect(
      resolveKeyAction(key("ArrowLeft", { ctrlKey: true, shiftKey: true }), "linux", context({ tabFocused: true })),
    ).toBe("tab-move-left");
    expect(resolveKeyAction(key("End"), "macos", context({ tabFocused: true }))).toBe("tab-select-last");
  });

  it("the selection check is lazy: only requiresSelection chords ask", () => {
    let asked = 0;
    const counting = context({
      hasSelection: () => {
        asked += 1;
        return true;
      },
    });
    resolveKeyAction(key("v", { ctrlKey: true }), "windows", counting);
    resolveKeyAction(key("Tab", { ctrlKey: true }), "windows", counting);
    expect(asked).toBe(0);
    resolveKeyAction(key("c", { ctrlKey: true }), "windows", counting);
    expect(asked).toBe(1);
  });
});

describe("session-tabs resolvers read the keymap", () => {
  const input = (key: string, modifiers: Partial<Omit<KeyInput, "key">> = {}, editable = false) => ({
    key,
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    metaKey: false,
    editable,
    ...modifiers,
  });

  it("global shortcuts follow the platform column", () => {
    expect(resolveGlobalSessionShortcut(input("t", { metaKey: true }), "macos")).toEqual({
      kind: "new-session",
    });
    expect(resolveGlobalSessionShortcut(input("t", { metaKey: true }), "windows")).toBeNull();
    expect(resolveGlobalSessionShortcut(input("PageDown", { ctrlKey: true }), "linux")).toEqual({
      kind: "select-relative",
      delta: 1,
    });
    expect(resolveGlobalSessionShortcut(input("Tab", { ctrlKey: true }, true), "windows")).toBeNull();
  });

  it("focused-tab actions are identical on every platform", () => {
    for (const platform of PLATFORMS) {
      expect(resolveFocusedTabAction(input("End"), 0, 3, platform)).toEqual({
        kind: "select-index",
        index: 2,
      });
      expect(
        resolveFocusedTabAction(input("ArrowRight", { ctrlKey: true, shiftKey: true }), 0, 3, platform),
      ).toEqual({ kind: "move", delta: 1 });
      expect(resolveFocusedTabAction(input("ArrowLeft"), 0, 0, platform)).toBeNull();
    }
  });
});

describe("table integrity and rendering", () => {
  it("every action has at least one chord on every platform", () => {
    for (const binding of KEY_BINDINGS) {
      for (const platform of PLATFORMS) {
        expect(binding.chords[platform].length, `${binding.action} on ${platform}`).toBeGreaterThan(0);
      }
    }
  });

  it("no two actions claim the same chord under the same scope on one platform", () => {
    for (const platform of PLATFORMS) {
      const seen = new Map<string, string>();
      for (const binding of KEY_BINDINGS) {
        for (const chord of binding.chords[platform]) {
          const signature = [
            binding.scope,
            chord.key.toLowerCase(),
            chord.ctrl ? "ctrl" : "",
            chord.shift ? "shift" : "",
            chord.alt ? "alt" : "",
            chord.meta ? "meta" : "",
            chord.requiresSelection ? "sel" : "",
          ].join("|");
          const owner = seen.get(signature);
          expect(owner, `${signature} on ${platform}: ${owner} vs ${binding.action}`).toBeUndefined();
          seen.set(signature, binding.action);
        }
      }
    }
  });

  it("renders chords the way each platform reads them", () => {
    expect(describeChord({ key: "c", ctrl: true, shift: true }, "windows")).toBe("Ctrl+Shift+C");
    expect(describeChord({ key: "c", meta: true }, "macos")).toBe("Cmd+C");
    expect(describeChord({ key: "f", ctrl: true, meta: true }, "macos")).toBe("Ctrl+Cmd+F");
    expect(describeChord({ key: "F11" }, "linux")).toBe("F11");
    expect(describeChord({ key: "ArrowLeft", ctrl: true, shift: true }, "windows")).toBe("Ctrl+Shift+←");
    expect(describeChord({ key: "Escape" }, "windows")).toBe("Esc");
    expect(describeChord({ key: "x", alt: true }, "macos")).toBe("Option+X");
  });

  it("help rows carry every chord with its condition, in declaration order", () => {
    const rows = helpRows("windows");
    expect(rows[0]?.action).toBe("copy");
    expect(rows[0]?.chords).toEqual([
      { label: "Ctrl+C", note: "with a selection" },
      { label: "Ctrl+Shift+C", note: null },
      { label: "Ctrl+Insert", note: null },
    ]);
    expect(helpRows("macos").find((row) => row.action === "paste")?.chords).toEqual([
      { label: "Cmd+V", note: null },
    ]);
    expect(rows.map((row) => row.action)).toEqual(KEY_BINDINGS.map((binding) => binding.action));
  });
});

describe("platform detection", () => {
  const cases: Array<[string, string, Platform]> = [
    ["Win32", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/128 Edg/128", "windows"],
    ["Linux x86_64", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15", "linux"],
    ["MacIntel", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15", "macos"],
    ["", "Mozilla/5.0 (Windows NT 10.0)", "windows"],
    ["darwin", "", "macos"],
    ["", "", "linux"],
  ];
  for (const [platform, userAgent, expected] of cases) {
    it(`${platform || userAgent || "no hints"} → ${expected}`, () => {
      expect(platformFromHints({ platform, userAgent })).toBe(expected);
    });
  }
});
