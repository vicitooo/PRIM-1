import { describe, expect, it } from "vitest";

import {
  BINDABLE_ACTIONS,
  DEFAULT_BINDINGS,
  KEY_BINDINGS,
  PLATFORMS,
  captureChord,
  checkUserChord,
  chordFromInput,
  defaultChord,
  describeChord,
  eventKey,
  helpRows,
  ownChords,
  parseUserBindings,
  platformBindings,
  platformFromHints,
  resolveKeyAction,
  resolveKeyMatch,
  serializeUserBindings,
  type BindableAction,
  type Chord,
  type KeyContext,
  type KeyInput,
  type Platform,
  type UserBindings,
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

/** Press keys while Settings → Keys listens for `action`: the real capture path. */
function capture(
  action: BindableAction,
  input: KeyInput,
  user: UserBindings,
  platform: Platform = "windows",
): UserBindings {
  const result = captureChord(action, input, platform, user);
  if (result.kind !== "bound") {
    throw new Error(`capture of ${action} ended ${result.kind}: ${JSON.stringify(result)}`);
  }
  return result.bindings;
}

/** A reload: what Settings → Keys stored, read back. */
function reload(user: UserBindings, platform: Platform = "windows"): UserBindings {
  return parseUserBindings(serializeUserBindings(user, platform), platform);
}

const ctrlV = key("v", { code: "KeyV", ctrlKey: true });
const ctrlShiftV = key("V", { code: "KeyV", ctrlKey: true, shiftKey: true });
const cyrillicCtrlV = key("ж", { code: "KeyV", ctrlKey: true });
const cyrillicCtrlShiftV = key("Ж", { code: "KeyV", ctrlKey: true, shiftKey: true });
const shiftInsert = key("Insert", { code: "Insert", shiftKey: true });
const ctrlInsert = key("Insert", { code: "Insert", ctrlKey: true });

describe("platform conventions", () => {
  it("Windows: Ctrl+C copies with a selection and is the interrupt without one", () => {
    expect(resolveKeyAction(key("c", { ctrlKey: true }), "windows", withSelection)).toBe("copy");
    expect(resolveKeyAction(key("c", { ctrlKey: true }), "windows", context())).toBe("interrupt");
    // The bare chord is the one that clears the selection afterwards.
    expect(
      resolveKeyMatch(key("c", { ctrlKey: true }), "windows", withSelection)?.chord.requiresSelection,
    ).toBe(true);
    expect(
      resolveKeyMatch(key("C", { ctrlKey: true, shiftKey: true }), "windows", withSelection)?.chord
        .requiresSelection,
    ).toBeUndefined();
  });

  it("Windows defaults are Victor's mapping: Ctrl+V pastes text, Ctrl+Shift+V inserts an image", () => {
    expect(resolveKeyAction(ctrlV, "windows", context())).toBe("paste-text");
    expect(resolveKeyAction(ctrlShiftV, "windows", context())).toBe("paste-image");
    expect(resolveKeyAction(shiftInsert, "windows", context())).toBe("paste-text");
    expect(resolveKeyAction(ctrlInsert, "windows", withSelection)).toBe("copy");
    // Alt+V is no PRIM-1 default: it reaches the harness as typed.
    expect(resolveKeyAction(key("v", { code: "KeyV", altKey: true }), "windows", context())).toBeNull();
  });

  it("Windows: submit, new line and interrupt default to Enter, Shift+Enter, Ctrl+C", () => {
    expect(resolveKeyAction(key("Enter"), "windows", context())).toBe("submit");
    expect(resolveKeyAction(key("Enter", { shiftKey: true }), "windows", context())).toBe("newline");
    // Ctrl+Enter and Alt+Enter are not bound: they reach the harness as typed.
    expect(resolveKeyAction(key("Enter", { ctrlKey: true }), "windows", context())).toBeNull();
    expect(resolveKeyAction(key("Enter", { altKey: true }), "windows", context())).toBeNull();
  });

  it("Linux: Ctrl+C interrupts, Ctrl+V is the image key; Ctrl+Shift+C/V copy and paste", () => {
    expect(resolveKeyAction(key("c", { ctrlKey: true }), "linux", withSelection)).toBe("interrupt");
    expect(resolveKeyAction(key("v", { ctrlKey: true }), "linux", context())).toBe("paste-image");
    expect(resolveKeyAction(key("C", { ctrlKey: true, shiftKey: true }), "linux", withSelection)).toBe(
      "copy",
    );
    expect(resolveKeyAction(key("V", { ctrlKey: true, shiftKey: true }), "linux", context())).toBe(
      "paste-text",
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
    expect(resolveKeyAction(key("v", { metaKey: true }), "macos", context())).toBe("paste-text");
    expect(resolveKeyAction(key("v", { ctrlKey: true }), "macos", context())).toBe("paste-image");
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

describe("physical keys off a Latin layout", () => {
  // Bulgarian: V types "ж", C types "ц", T types "т"; `code` stays the key.
  it("a non-ASCII character on a letter key compares as that key's letter", () => {
    expect(eventKey({ key: "ж", code: "KeyV" })).toBe("v");
    expect(eventKey({ key: "Ж", code: "KeyV" })).toBe("v");
    expect(eventKey({ key: "ц", code: "KeyC" })).toBe("c");
    expect(eventKey({ key: "é", code: "Digit2" })).toBe("2");
  });

  it("an ASCII character keeps its meaning (Dvorak, AZERTY), and named keys are untouched", () => {
    // Dvorak: the key labelled V on QWERTY types "." — and the "v" key sits on Period.
    expect(eventKey({ key: "v", code: "Period" })).toBe("v");
    expect(eventKey({ key: ".", code: "KeyV" })).toBe(".");
    expect(eventKey({ key: "Enter", code: "Enter" })).toBe("Enter");
    expect(eventKey({ key: "ж" })).toBe("ж");
    expect(eventKey({ key: "ш", code: "BracketLeft" })).toBe("ш");
  });

  it("Ctrl+V, Ctrl+Shift+V, Ctrl+C and Ctrl+Shift+T work on the Bulgarian layout", () => {
    expect(resolveKeyAction(cyrillicCtrlV, "windows", context())).toBe("paste-text");
    expect(resolveKeyAction(cyrillicCtrlShiftV, "windows", context())).toBe("paste-image");
    expect(resolveKeyAction(key("ц", { code: "KeyC", ctrlKey: true }), "windows", withSelection)).toBe(
      "copy",
    );
    expect(resolveKeyAction(key("ц", { code: "KeyC", ctrlKey: true }), "windows", context())).toBe(
      "interrupt",
    );
    expect(
      resolveKeyAction(key("Т", { code: "KeyT", ctrlKey: true, shiftKey: true }), "windows", context()),
    ).toBe("new-session");
  });

  it("every letter chord resolves the same from a Latin and a Cyrillic key on every platform", () => {
    const cyrillic: Record<string, string> = { c: "ц", v: "ж", t: "т", w: "в", f: "ф" };
    for (const platform of PLATFORMS) {
      for (const binding of KEY_BINDINGS) {
        for (const chord of binding.chords[platform]) {
          const local = cyrillic[chord.key];
          if (!local) {
            continue;
          }
          const modifiers = {
            ctrlKey: Boolean(chord.ctrl),
            shiftKey: Boolean(chord.shift),
            altKey: Boolean(chord.alt),
            metaKey: Boolean(chord.meta),
          };
          const latin = resolveKeyAction(key(chord.key, modifiers), platform, withSelection);
          const physical = resolveKeyAction(
            key(local, { ...modifiers, code: `Key${chord.key.toUpperCase()}` }),
            platform,
            withSelection,
          );
          expect(physical, `${binding.action} ${describeChord(chord, platform)} on ${platform}`).toBe(latin);
        }
      }
    }
  });
});

describe("scopes", () => {
  it("editable targets never trigger copy, paste, harness or tab chords", () => {
    const editable = context({ editable: true, hasSelection: () => true });
    expect(resolveKeyAction(key("c", { ctrlKey: true }), "windows", editable)).toBeNull();
    expect(resolveKeyAction(ctrlV, "windows", editable)).toBeNull();
    expect(resolveKeyAction(ctrlShiftV, "windows", editable)).toBeNull();
    expect(resolveKeyAction(key("Enter"), "windows", editable)).toBeNull();
    expect(resolveKeyAction(key("Enter", { shiftKey: true }), "windows", editable)).toBeNull();
    expect(resolveKeyAction(key("Tab", { ctrlKey: true }), "windows", editable)).toBeNull();
    // Function keys and Escape stay live inside editable fields.
    expect(resolveKeyAction(key("F1"), "windows", editable)).toBe("help");
    expect(resolveKeyAction(key("F11"), "linux", editable)).toBe("fullscreen");
    expect(resolveKeyAction(key("Escape"), "macos", editable)).toBe("close-menu");
  });

  it("paste and the harness actions need a focused terminal; copy does not (page text and the system log count)", () => {
    const unfocused = context({ terminalFocused: false });
    expect(resolveKeyAction(ctrlV, "windows", unfocused)).toBeNull();
    expect(resolveKeyAction(ctrlShiftV, "windows", unfocused)).toBeNull();
    expect(resolveKeyAction(key("Enter"), "windows", unfocused)).toBeNull();
    expect(resolveKeyAction(key("c", { ctrlKey: true }), "windows", unfocused)).toBeNull();
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
    resolveKeyAction(ctrlV, "windows", counting);
    resolveKeyAction(key("Tab", { ctrlKey: true }), "windows", counting);
    resolveKeyAction(key("Enter"), "windows", counting);
    expect(asked).toBe(0);
    resolveKeyAction(key("c", { ctrlKey: true }), "windows", counting);
    expect(asked).toBe(1);
  });
});

describe("Victor's keymap, set through Settings → Keys", () => {
  it("from Alt+V back to Ctrl+Shift+V: the chord is taken from the text-paste convention", () => {
    // Start where Codex's review started: paste image elsewhere, so
    // Ctrl+Shift+V is only Windows Terminal's text-paste convention.
    let user = capture("paste-image", key("v", { code: "KeyV", altKey: true }), DEFAULT_BINDINGS);
    expect(user).toEqual({ "paste-image": { key: "v", alt: true } });
    expect(resolveKeyAction(ctrlShiftV, "windows", context(), user)).toBe("paste-text");
    expect(checkUserChord("paste-image", { key: "v", ctrl: true, shift: true }, "windows", user)).toBeNull();

    // Victor presses Ctrl+Shift+V on the Bulgarian layout.
    user = capture("paste-image", cyrillicCtrlShiftV, user);
    for (const bindings of [user, reload(user)]) {
      expect(resolveKeyAction(ctrlShiftV, "windows", context(), bindings)).toBe("paste-image");
      expect(resolveKeyAction(cyrillicCtrlShiftV, "windows", context(), bindings)).toBe("paste-image");
      expect(resolveKeyAction(ctrlV, "windows", context(), bindings)).toBe("paste-text");
      expect(resolveKeyAction(cyrillicCtrlV, "windows", context(), bindings)).toBe("paste-text");
      expect(resolveKeyAction(shiftInsert, "windows", context(), bindings)).toBe("paste-text");
    }
  });

  it("an explicit paste-image binding outranks the convention even when stored by an older table", () => {
    // Codex's repro: the override existed but paste text was declared first.
    const injected: UserBindings = { "paste-image": { key: "v", ctrl: true, shift: true } };
    expect(resolveKeyAction(ctrlShiftV, "windows", context(), injected)).toBe("paste-image");
    const stored = JSON.stringify(injected);
    expect(resolveKeyAction(ctrlShiftV, "windows", context(), parseUserBindings(stored, "windows"))).toBe(
      "paste-image",
    );
  });

  it("the same mapping on a non-default chord persists: binding wins over convention after reload", () => {
    // Shift+Insert is a text-paste convention; bind it to New line.
    const user = capture("newline", shiftInsert, DEFAULT_BINDINGS);
    expect(serializeUserBindings(user, "windows")).toBe('{"newline":{"key":"Insert","shift":true}}');
    for (const bindings of [user, reload(user)]) {
      expect(resolveKeyAction(shiftInsert, "windows", context(), bindings)).toBe("newline");
      expect(resolveKeyAction(ctrlV, "windows", context(), bindings)).toBe("paste-text");
      expect(
        platformBindings("windows", bindings)
          .find((binding) => binding.action === "paste-text")
          ?.chords.map((chord) => describeChord(chord, "windows")),
      ).toEqual(["Ctrl+V"]);
    }
  });

  it("Ctrl+Insert bound to interrupt takes it from copy; the other copy chords stay", () => {
    const user = capture("interrupt", ctrlInsert, DEFAULT_BINDINGS);
    expect(user).toEqual({ interrupt: { key: "Insert", ctrl: true } });
    for (const bindings of [user, reload(user)]) {
      expect(resolveKeyAction(ctrlInsert, "windows", withSelection, bindings)).toBe("interrupt");
      expect(resolveKeyAction(ctrlInsert, "windows", context(), bindings)).toBe("interrupt");
      expect(resolveKeyAction(key("C", { code: "KeyC", ctrlKey: true, shiftKey: true }), "windows", withSelection, bindings)).toBe("copy");
      expect(resolveKeyAction(key("c", { code: "KeyC", ctrlKey: true }), "windows", withSelection, bindings)).toBe("copy");
      // Ctrl+C without a selection is no PRIM-1 chord now: the terminal's own ^C.
      expect(resolveKeyAction(key("c", { code: "KeyC", ctrlKey: true }), "windows", context(), bindings)).toBeNull();
      expect(helpRows("windows", bindings)[0]?.chords).toEqual([
        { label: "Ctrl+C", note: "with a selection" },
        { label: "Ctrl+Shift+C", note: null },
      ]);
    }
  });

  it("rebinding paste image elsewhere gives Ctrl+Shift+V back to text paste", () => {
    const user = capture("paste-image", key("i", { code: "KeyI", ctrlKey: true, shiftKey: true }), DEFAULT_BINDINGS);
    expect(resolveKeyAction(ctrlShiftV, "windows", context(), user)).toBe("paste-text");
    expect(resolveKeyAction(key("I", { code: "KeyI", ctrlKey: true, shiftKey: true }), "windows", context(), user)).toBe(
      "paste-image",
    );
  });

  it("capture waits through bare modifiers, cancels on Esc, and explains a refusal", () => {
    expect(captureChord("newline", key("Shift", { shiftKey: true }), "windows", DEFAULT_BINDINGS).kind).toBe("waiting");
    expect(captureChord("newline", key("Escape"), "windows", DEFAULT_BINDINGS).kind).toBe("cancelled");
    expect(captureChord("newline", key("Enter"), "windows", DEFAULT_BINDINGS)).toEqual({
      kind: "refused",
      message: "Enter is Submit — rebind that first.",
    });
  });
});

describe("ownership", () => {
  it("one chord per action replaces the default; untouched conventions keep working", () => {
    const user: UserBindings = {
      submit: { key: "Enter", ctrl: true },
      newline: { key: "Enter" },
    };
    expect(resolveKeyAction(key("Enter"), "windows", context(), user)).toBe("newline");
    expect(resolveKeyAction(key("Enter", { ctrlKey: true }), "windows", context(), user)).toBe("submit");
    expect(resolveKeyAction(key("Enter", { shiftKey: true }), "windows", context(), user)).toBeNull();
    expect(resolveKeyAction(shiftInsert, "windows", context(), user)).toBe("paste-text");
    expect(resolveKeyAction(ctrlInsert, "windows", withSelection, user)).toBe("copy");
  });

  it("copy always waits for a selection, so it can share its chord with the interrupt", () => {
    const user: UserBindings = {
      copy: { key: "x", ctrl: true, shift: true },
      interrupt: { key: "x", ctrl: true, shift: true },
    };
    const chord = key("X", { ctrlKey: true, shiftKey: true });
    expect(resolveKeyAction(chord, "windows", withSelection, user)).toBe("copy");
    expect(resolveKeyAction(chord, "windows", context(), user)).toBe("interrupt");
    expect(platformBindings("windows", user)[0]?.chords[0]?.requiresSelection).toBe(true);
  });

  it("picking one's own convention frees the default and is not listed twice", () => {
    const user = capture("paste-text", shiftInsert, DEFAULT_BINDINGS);
    const paste = platformBindings("windows", user).find((binding) => binding.action === "paste-text");
    expect(paste?.chords.map((chord) => describeChord(chord, "windows"))).toEqual(["Shift+Insert"]);
    expect(resolveKeyAction(ctrlV, "windows", context(), user)).toBeNull();
  });

  it("a stored binding that took another action's default leaves that action unbound (explicit wins)", () => {
    // Settings → Keys refuses this; only a store written under other defaults holds it.
    const stored = parseUserBindings('{"paste-text":{"key":"v","ctrl":true,"shift":true}}', "windows");
    expect(stored).toEqual({ "paste-text": { key: "v", ctrl: true, shift: true } });
    expect(ownChords("windows", stored)["paste-image"]).toBeNull();
    expect(resolveKeyAction(ctrlShiftV, "windows", context(), stored)).toBe("paste-text");
    expect(platformBindings("windows", stored).find((binding) => binding.action === "paste-image")?.chords).toEqual([]);
  });

  it("chordFromInput waits through bare modifiers and binds the physical letter", () => {
    expect(chordFromInput(key("Shift", { shiftKey: true }))).toBeNull();
    expect(chordFromInput(key("Control", { ctrlKey: true }))).toBeNull();
    expect(chordFromInput(key("Dead"))).toBeNull();
    expect(chordFromInput(key("Process"))).toBeNull();
    expect(chordFromInput(cyrillicCtrlShiftV)).toEqual({ key: "v", ctrl: true, shift: true });
    expect(chordFromInput(key("Enter", { shiftKey: true }))).toEqual({ key: "Enter", shift: true });
  });
});

describe("rebinding rules", () => {
  const check = (action: BindableAction, chord: Chord, user: UserBindings = DEFAULT_BINDINGS) =>
    checkUserChord(action, chord, "windows", user);

  it("refuses a chord that types text", () => {
    expect(check("newline", { key: "a" })).toBe("A types text — add Ctrl or Alt.");
    expect(check("newline", { key: "a", shift: true })).toMatch(/types text/);
    expect(check("newline", { key: " " })).toMatch(/^Space types text/);
    expect(check("newline", { key: "Tab" })).toMatch(/types text/);
    expect(check("newline", { key: "Backspace", shift: true })).toMatch(/types text/);
    expect(check("newline", { key: "j", ctrl: true })).toBeNull();
    expect(check("newline", { key: "Enter", alt: true })).toBeNull();
  });

  it("refuses Esc, the Windows key, and the app's own chords", () => {
    expect(check("interrupt", { key: "Escape" })).toBe("Esc is reserved: it cancels.");
    expect(check("interrupt", { key: "q", meta: true })).toBe("The Windows key belongs to Windows.");
    expect(check("interrupt", { key: "Tab", ctrl: true })).toBe("Ctrl+Tab is Next tab.");
    expect(check("submit", { key: "F1" })).toBe("F1 is Help.");
  });

  it("takes a convention chord over", () => {
    expect(check("interrupt", { key: "c", ctrl: true, shift: true })).toBeNull();
    expect(check("interrupt", { key: "Insert", ctrl: true })).toBeNull();
    expect(check("newline", { key: "Insert", shift: true })).toBeNull();
  });

  it("refuses another action's own chord, default or chosen, except copy and interrupt sharing one", () => {
    expect(check("newline", { key: "Enter" })).toBe("Enter is Submit — rebind that first.");
    expect(check("paste-image", { key: "v", ctrl: true })).toBe("Ctrl+V is Paste text — rebind that first.");
    expect(check("paste-text", { key: "v", ctrl: true, shift: true })).toBe(
      "Ctrl+Shift+V is Paste image — rebind that first.",
    );
    const chosen: UserBindings = { newline: { key: "j", ctrl: true } };
    expect(check("submit", { key: "j", ctrl: true }, chosen)).toBe("Ctrl+J is New line — rebind that first.");
    expect(check("interrupt", { key: "c", ctrl: true })).toBeNull();
    expect(check("copy", { key: "x", ctrl: true }, { interrupt: { key: "x", ctrl: true } })).toBeNull();
    // Re-pressing an action's own chord is fine.
    expect(check("submit", { key: "Enter" })).toBeNull();
  });

  it("paste text keeps the system paste: only a chord the webview pastes with", () => {
    expect(check("paste-text", { key: "p", ctrl: true, shift: true })).toBe(
      "Paste text uses the system paste: Ctrl+V, Ctrl+Shift+V, Shift+Insert.",
    );
    expect(check("paste-text", { key: "Insert", shift: true })).toBeNull();
    const imageElsewhere: UserBindings = { "paste-image": { key: "v", alt: true } };
    expect(check("paste-text", { key: "v", ctrl: true, shift: true }, imageElsewhere)).toBeNull();
    expect(checkUserChord("paste-text", { key: "v", meta: true }, "macos", DEFAULT_BINDINGS)).toBeNull();
  });

  it("every platform's defaults pass the same check a rebind must pass", () => {
    for (const platform of PLATFORMS) {
      for (const action of BINDABLE_ACTIONS) {
        expect(
          checkUserChord(action, defaultChord(action, platform), platform, DEFAULT_BINDINGS),
          `${action} on ${platform}`,
        ).toBeNull();
      }
    }
  });
});

describe("stored bindings", () => {
  it("nothing stored, unreadable or of the wrong shape means the defaults", () => {
    expect(parseUserBindings(null, "windows")).toEqual({});
    expect(parseUserBindings("", "windows")).toEqual({});
    expect(parseUserBindings("{not json", "windows")).toEqual({});
    expect(parseUserBindings("[1,2]", "windows")).toEqual({});
    expect(parseUserBindings('"Enter"', "windows")).toEqual({});
  });

  it("an invalid, unknown or default entry is dropped; valid ones stay", () => {
    const raw = JSON.stringify({
      newline: { key: "j", ctrl: true },
      submit: { key: "Enter", ctrl: "yes" },
      interrupt: { key: "a" },
      "paste-image": { key: "" },
      "paste-text": { key: "v", ctrl: true },
      teleport: { key: "t", ctrl: true },
    });
    expect(parseUserBindings(raw, "windows")).toEqual({ newline: { key: "j", ctrl: true } });
  });

  it("two stored bindings on one chord drop everything", () => {
    const raw = JSON.stringify({ newline: { key: "j", ctrl: true }, submit: { key: "j", ctrl: true } });
    expect(parseUserBindings(raw, "windows")).toEqual({});
  });

  it("a swap survives a reload; only explicit bindings are stored", () => {
    const swapped = capture(
      "newline",
      key("Enter"),
      capture("submit", key("Enter", { ctrlKey: true }), DEFAULT_BINDINGS),
    );
    expect(JSON.parse(serializeUserBindings(swapped, "windows") ?? "{}")).toEqual({
      submit: { key: "Enter", ctrl: true },
      newline: { key: "Enter" },
    });
    expect(reload(swapped)).toEqual(swapped);
  });

  it("the defaults store nothing, and copy's selection condition is never stored", () => {
    expect(serializeUserBindings(DEFAULT_BINDINGS, "windows")).toBeNull();
    const raw = serializeUserBindings(
      { copy: { key: "x", ctrl: true, requiresSelection: true } },
      "windows",
    );
    expect(JSON.parse(raw ?? "{}")).toEqual({ copy: { key: "x", ctrl: true } });
  });

  it("letters are stored lowercased whatever the store says", () => {
    const parsed = parseUserBindings(JSON.stringify({ newline: { key: "J", ctrl: true } }), "windows");
    expect(parsed.newline).toEqual({ key: "j", ctrl: true });
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
    expect(
      resolveGlobalSessionShortcut(input("Т", { code: "KeyT", ctrlKey: true, shiftKey: true }), "windows"),
    ).toEqual({ kind: "new-session" });
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

  it("no two actions hold the same chord on one platform, except copy's selection-only chord", () => {
    for (const platform of PLATFORMS) {
      const seen = new Map<string, string>();
      for (const binding of KEY_BINDINGS) {
        for (const chord of binding.chords[platform]) {
          const signature = [
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

  it("the default table is the user table with nothing rebound", () => {
    for (const platform of PLATFORMS) {
      expect(platformBindings(platform).map((binding) => binding.chords)).toEqual(
        KEY_BINDINGS.map((binding) => binding.chords[platform]),
      );
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
    expect(describeChord({ key: "Enter", shift: true }, "windows")).toBe("Shift+Enter");
  });

  it("help rows carry every chord with its condition, in declaration order", () => {
    const rows = helpRows("windows");
    expect(rows[0]?.action).toBe("copy");
    expect(rows[0]?.chords).toEqual([
      { label: "Ctrl+C", note: "with a selection" },
      { label: "Ctrl+Shift+C", note: null },
      { label: "Ctrl+Insert", note: null },
    ]);
    expect(rows.find((row) => row.action === "paste-text")?.chords).toEqual([
      { label: "Ctrl+V", note: null },
      { label: "Shift+Insert", note: null },
    ]);
    expect(rows.find((row) => row.action === "paste-image")?.chords).toEqual([
      { label: "Ctrl+Shift+V", note: null },
    ]);
    expect(helpRows("macos").find((row) => row.action === "paste-text")?.chords).toEqual([
      { label: "Cmd+V", note: null },
    ]);
    expect(rows.map((row) => row.action)).toEqual(KEY_BINDINGS.map((binding) => binding.action));
  });

  it("help rows follow the user's chords", () => {
    const user: UserBindings = { newline: { key: "j", ctrl: true } };
    expect(helpRows("windows", user).find((row) => row.action === "newline")?.chords).toEqual([
      { label: "Ctrl+J", note: null },
    ]);
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
