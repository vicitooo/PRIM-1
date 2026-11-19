import { describe, expect, it } from "vitest";

import {
  HARNESS_KEYS,
  KEY_TABLE_DRIVERS,
  describeKeyBytes,
  harnessKeyPlan,
  harnessKeyTranslation,
  type HarnessKeyTranslation,
  type KeyPlan,
} from "./harness-keys";
import {
  BINDABLE_ACTIONS,
  DEFAULT_BINDINGS,
  PLATFORMS,
  captureChord,
  chordFromInput,
  isNativePasteChord,
  resolveKeyMatch,
  type BindableAction,
  type KeyInput,
  type UserBindings,
} from "./keymap";
import type { DriverKind } from "./types";

function bytes(action: BindableAction, driver: DriverKind): string | null {
  const translation = harnessKeyTranslation(action, driver, "windows");
  return translation.kind === "keys" ? translation.bytes : null;
}

function press(
  key: string,
  code: string,
  modifiers: Partial<Pick<KeyInput, "ctrlKey" | "shiftKey" | "altKey" | "metaKey">> = {},
): KeyInput {
  return { key, code, ctrlKey: false, shiftKey: false, altKey: false, metaKey: false, ...modifiers };
}

/**
 * A test double of what xterm 6 sends for the chords pressed here
 * (src/common/input/Keyboard.ts): Ctrl+letter → its C0 byte, Alt+letter →
 * ESC + the letter (from the key code, so on any layout), Enter → CR (ESC CR
 * with Alt). Ctrl+Shift+letter and Shift+Insert have no encoding, so the
 * webview's default runs — for the paste chords, its own paste.
 */
function xtermSends(input: KeyInput): string | null {
  const letter = /^Key([A-Z])$/.exec(input.code ?? "")?.[1];
  if (input.key === "Enter") {
    return input.altKey ? "\x1b\r" : "\r";
  }
  if (letter && input.ctrlKey && !input.shiftKey && !input.altKey && !input.metaKey) {
    return String.fromCharCode(letter.charCodeAt(0) - 64);
  }
  if (letter && input.altKey && !input.ctrlKey && !input.metaKey) {
    return "\x1b" + (input.shiftKey ? letter : letter.toLowerCase());
  }
  return null;
}

interface Outcome {
  /** Every byte sequence the harness receives, in order. */
  received: string[];
  /** The webview ran its own paste (text into the pane). */
  textPasted: boolean;
}

/**
 * One keydown in a focused `driver` pane, as main.ts's dispatcher handles it:
 * resolve, plan, then whatever the plan leaves to xterm and the webview.
 */
function keydown(input: KeyInput, driver: DriverKind, user: UserBindings = DEFAULT_BINDINGS): Outcome {
  const match = resolveKeyMatch(
    input,
    "windows",
    { editable: false, terminalFocused: true, tabFocused: false, hasSelection: () => false },
    user,
  );
  const plan: KeyPlan =
    match && match.action !== "copy" && (BINDABLE_ACTIONS as readonly string[]).includes(match.action)
      ? harnessKeyPlan(match.action as Exclude<BindableAction, "copy">, driver, "windows")
      : { preventDefault: false, stopPropagation: false, send: null };
  const received: string[] = plan.send === null ? [] : [plan.send];
  let defaultPrevented = plan.preventDefault;
  if (!plan.stopPropagation) {
    const typed = xtermSends(input);
    if (typed !== null) {
      // xterm encodes the chord and cancels the event.
      received.push(typed);
      defaultPrevented = true;
    }
  }
  const chord = chordFromInput(input);
  return {
    received,
    textPasted: !defaultPrevented && chord !== null && isNativePasteChord(chord, "windows"),
  };
}

describe("what each harness receives (Windows)", () => {
  it("every action has an entry for every harness", () => {
    for (const action of BINDABLE_ACTIONS) {
      for (const driver of KEY_TABLE_DRIVERS) {
        expect(HARNESS_KEYS[action][driver], `${action} × ${driver}`).toBeDefined();
      }
    }
    expect([...KEY_TABLE_DRIVERS].sort()).toEqual(
      ["claude", "codex", "generic_terminal", "grok", "prime"],
    );
  });

  it("submit is Enter (CR) wherever it is verified", () => {
    for (const driver of ["claude", "codex", "grok", "generic_terminal"] as const) {
      expect(bytes("submit", driver), driver).toBe("\r");
    }
  });

  it("new line is Alt+Enter (ESC CR): Shift+Enter cannot cross xterm and ConPTY", () => {
    for (const driver of ["claude", "codex", "grok"] as const) {
      expect(bytes("newline", driver), driver).toBe("\x1b\r");
    }
    // LF would arrive through ConPTY as Ctrl+Enter, which Grok reads as
    // interject and Codex does not bind to a new line.
    for (const action of BINDABLE_ACTIONS) {
      for (const driver of KEY_TABLE_DRIVERS) {
        expect(bytes(action, driver) ?? "", `${action} × ${driver}`).not.toContain("\n");
      }
    }
  });

  it("paste image is each harness's own key: Alt+V for Claude and Grok, Ctrl+V for Codex", () => {
    expect(bytes("paste-image", "claude")).toBe("\x1bv");
    expect(bytes("paste-image", "grok")).toBe("\x1bv");
    expect(bytes("paste-image", "codex")).toBe("\x16");
    expect(harnessKeyTranslation("paste-image", "generic_terminal", "windows").kind).toBe("unsupported");
  });

  it("interrupt is Ctrl+C (^C) wherever it is verified", () => {
    for (const driver of ["claude", "codex", "grok", "generic_terminal"] as const) {
      expect(bytes("interrupt", driver), driver).toBe("\x03");
    }
  });

  it("copy and paste text are PRIM-1's own, the same for every harness", () => {
    for (const driver of KEY_TABLE_DRIVERS) {
      expect(harnessKeyTranslation("copy", driver, "windows").kind).toBe("terminal");
      expect(harnessKeyTranslation("paste-text", driver, "windows").kind).toBe("terminal");
    }
  });

  it("what was not verified says so, and is never sent", () => {
    const unverified: Array<[BindableAction, DriverKind]> = [
      ["newline", "generic_terminal"],
      ["paste-image", "prime"],
      ["submit", "prime"],
      ["newline", "prime"],
      ["interrupt", "prime"],
    ];
    for (const [action, driver] of unverified) {
      const translation = harnessKeyTranslation(action, driver, "windows");
      expect(translation.kind, `${action} × ${driver}`).toBe("unverified");
      expect(bytes(action, driver)).toBeNull();
    }
  });

  it("every verified translation names its source and the harness's key", () => {
    for (const action of BINDABLE_ACTIONS) {
      for (const driver of KEY_TABLE_DRIVERS) {
        const translation: HarnessKeyTranslation = HARNESS_KEYS[action][driver];
        if (translation.kind === "keys") {
          expect(translation.source.length, `${action} × ${driver}`).toBeGreaterThan(20);
          expect(translation.harnessKey).toMatch(/^(?:Enter|Alt\+Enter|Alt\+V|Ctrl\+V|Ctrl\+C)$/);
        }
      }
    }
  });
});

describe("one keydown, end to end (dispatcher plan + xterm + the webview's paste)", () => {
  const ctrlV = press("v", "KeyV", { ctrlKey: true });
  const ctrlShiftV = press("V", "KeyV", { ctrlKey: true, shiftKey: true });
  const cyrillicCtrlV = press("ж", "KeyV", { ctrlKey: true });
  const cyrillicCtrlShiftV = press("Ж", "KeyV", { ctrlKey: true, shiftKey: true });
  const imageKey: Record<"claude" | "codex" | "grok", string> = {
    claude: "\x1bv",
    codex: "\x16",
    grok: "\x1bv",
  };

  it("Ctrl+Shift+V in a Claude, Codex or Grok pane sends exactly one image key and pastes no text", () => {
    for (const driver of ["claude", "codex", "grok"] as const) {
      for (const input of [ctrlShiftV, cyrillicCtrlShiftV]) {
        expect(keydown(input, driver), `${driver} ${input.key}`).toEqual({
          received: [imageKey[driver]],
          textPasted: false,
        });
      }
    }
  });

  it("Ctrl+Shift+V in a Terminal or Prime pane passes through: the webview pastes the text", () => {
    for (const driver of ["generic_terminal", "prime"] as const) {
      expect(keydown(ctrlShiftV, driver), driver).toEqual({ received: [], textPasted: true });
      expect(keydown(cyrillicCtrlShiftV, driver), driver).toEqual({ received: [], textPasted: true });
    }
  });

  it("Ctrl+V pastes text in every pane, on either layout, and never sends ^V", () => {
    for (const driver of KEY_TABLE_DRIVERS) {
      expect(keydown(ctrlV, driver), driver).toEqual({ received: [], textPasted: true });
      expect(keydown(cyrillicCtrlV, driver), driver).toEqual({ received: [], textPasted: true });
    }
  });

  it("Alt+V is no PRIM-1 chord: xterm sends ESC v, Claude's and Grok's own image key", () => {
    for (const driver of ["claude", "grok"] as const) {
      expect(keydown(press("v", "KeyV", { altKey: true }), driver)).toEqual({
        received: ["\x1bv"],
        textPasted: false,
      });
      expect(keydown(press("ж", "KeyV", { altKey: true }), driver)).toEqual({
        received: ["\x1bv"],
        textPasted: false,
      });
    }
  });

  it("Victor's mapping set through the capture path behaves the same as the default", () => {
    let user = DEFAULT_BINDINGS;
    for (const [action, input] of [
      ["paste-image", press("v", "KeyV", { altKey: true })],
      ["paste-image", cyrillicCtrlShiftV],
      ["paste-text", cyrillicCtrlV],
    ] as const) {
      const result = captureChord(action, input, "windows", user);
      expect(result.kind).toBe("bound");
      if (result.kind === "bound") {
        user = result.bindings;
      }
    }
    expect(keydown(ctrlShiftV, "codex", user)).toEqual({ received: ["\x16"], textPasted: false });
    expect(keydown(ctrlV, "codex", user)).toEqual({ received: [], textPasted: true });
    expect(keydown(ctrlShiftV, "generic_terminal", user)).toEqual({ received: [], textPasted: true });
  });

  it("submit, new line and interrupt send the harness's own keys", () => {
    expect(keydown(press("Enter", "Enter"), "codex").received).toEqual(["\r"]);
    expect(keydown(press("Enter", "Enter", { shiftKey: true }), "claude").received).toEqual(["\x1b\r"]);
    expect(keydown(press("Enter", "Enter", { shiftKey: true }), "grok").received).toEqual(["\x1b\r"]);
    expect(keydown(press("c", "KeyC", { ctrlKey: true }), "codex").received).toEqual(["\x03"]);
    // Unverified for a Terminal pane: xterm's own CR, as before.
    expect(keydown(press("Enter", "Enter", { shiftKey: true }), "generic_terminal").received).toEqual(["\r"]);
  });

  it("the plan itself: paste text lets the webview paste; a translation cancels it", () => {
    expect(harnessKeyPlan("paste-text", "claude", "windows")).toEqual({
      preventDefault: false,
      stopPropagation: true,
      send: null,
    });
    expect(harnessKeyPlan("paste-image", "claude", "windows")).toEqual({
      preventDefault: true,
      stopPropagation: true,
      send: "\x1bv",
    });
    expect(harnessKeyPlan("paste-image", "generic_terminal", "windows")).toEqual({
      preventDefault: false,
      stopPropagation: false,
      send: null,
    });
    expect(harnessKeyPlan("submit", undefined, "windows")).toEqual({
      preventDefault: false,
      stopPropagation: false,
      send: null,
    });
  });
});

describe("other platforms", () => {
  it("byte translations are unverified off Windows; PRIM-1's own actions are not", () => {
    for (const platform of PLATFORMS.filter((candidate) => candidate !== "windows")) {
      expect(harnessKeyTranslation("submit", "claude", platform).kind).toBe("unverified");
      expect(harnessKeyTranslation("paste-image", "codex", platform).kind).toBe("unverified");
      expect(harnessKeyTranslation("copy", "claude", platform).kind).toBe("terminal");
      expect(harnessKeyTranslation("paste-text", "grok", platform).kind).toBe("terminal");
      expect(harnessKeyTranslation("paste-image", "generic_terminal", platform).kind).toBe("unsupported");
    }
  });
});

describe("byte names", () => {
  it("reads the bytes the way a terminal person does", () => {
    expect(describeKeyBytes("\r")).toBe("CR");
    expect(describeKeyBytes("\x1b\r")).toBe("ESC CR");
    expect(describeKeyBytes("\x1bv")).toBe("ESC v");
    expect(describeKeyBytes("\x16")).toBe("^V");
    expect(describeKeyBytes("\x03")).toBe("^C");
    expect(describeKeyBytes("\n\t")).toBe("LF TAB");
    expect(describeKeyBytes("\x00\x7f")).toBe("0x00 0x7f");
  });
});
