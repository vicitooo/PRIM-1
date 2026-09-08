/**
 * The one place a key binding lives.
 *
 * Every chord the app consumes is declared here, per platform, and everything
 * else reads this table: the keydown dispatcher in `main.ts`, the tab
 * resolvers in `session-tabs.ts`, the F1 help page, and the README table.
 * A chord that is not declared here is not consumed by the app and reaches the
 * focused terminal as bytes (xterm's own mapping) — that is how Ctrl+C stays
 * the interrupt on every platform when nothing is selected.
 *
 * Platform conventions (2026-09-08 decision, Victor): Windows follows Windows
 * Terminal, Linux follows GNOME Terminal, macOS follows Terminal.app / iTerm2.
 * The macOS column is written from convention and unit-tested; it has not been
 * verified on hardware.
 */

export type Platform = "windows" | "linux" | "macos";

export const PLATFORMS: readonly Platform[] = ["windows", "linux", "macos"];

export type KeyAction =
  | "copy"
  | "paste"
  | "new-session"
  | "close-session"
  | "next-tab"
  | "previous-tab"
  | "fullscreen"
  | "help"
  | "close-menu"
  | "tab-select-previous"
  | "tab-select-next"
  | "tab-select-first"
  | "tab-select-last"
  | "tab-move-left"
  | "tab-move-right";

/** Where a chord is live. */
export type KeyScope =
  /** Everywhere, editable fields included (function keys, Escape). */
  | "always"
  /** Anywhere except an editable field (inputs, textareas, contenteditable). */
  | "global"
  /** Only while a session terminal owns focus. */
  | "terminal"
  /** Only while a session tab button owns focus. */
  | "tab";

export interface Chord {
  /** `KeyboardEvent.key`; letters are matched case-insensitively. */
  key: string;
  ctrl?: boolean;
  shift?: boolean;
  alt?: boolean;
  /** Command on macOS, Windows key elsewhere. */
  meta?: boolean;
  /**
   * The chord is consumed only when something is selected (Windows Ctrl+C:
   * copy with a selection, interrupt without one). Never consumed otherwise.
   */
  requiresSelection?: boolean;
}

export interface KeyBinding {
  action: KeyAction;
  scope: KeyScope;
  /** One line for the help page; the chords are rendered in front of it. */
  help: string;
  chords: Record<Platform, readonly Chord[]>;
}

const CTRL_SHIFT = { ctrl: true, shift: true } as const;

function everywhere(chords: readonly Chord[]): Record<Platform, readonly Chord[]> {
  return { windows: chords, linux: chords, macos: chords };
}

/**
 * Declaration order is help-page order. Within one platform, the first
 * matching chord wins, so a `requiresSelection` chord must be declared before
 * an unconditional chord with the same keys if both exist (they do not today).
 */
export const KEY_BINDINGS: readonly KeyBinding[] = [
  {
    action: "copy",
    scope: "global",
    help: "Copy the selection (a pane, the system log, or page text)",
    chords: {
      windows: [
        { key: "c", ctrl: true, requiresSelection: true },
        { key: "c", ...CTRL_SHIFT },
        { key: "Insert", ctrl: true },
      ],
      linux: [
        { key: "c", ...CTRL_SHIFT },
        { key: "Insert", ctrl: true },
      ],
      macos: [{ key: "c", meta: true, requiresSelection: true }],
    },
  },
  {
    action: "paste",
    scope: "terminal",
    help: "Paste the clipboard into the focused terminal",
    chords: {
      windows: [
        { key: "v", ctrl: true },
        { key: "v", ...CTRL_SHIFT },
        { key: "Insert", shift: true },
      ],
      linux: [
        { key: "v", ...CTRL_SHIFT },
        { key: "Insert", shift: true },
      ],
      macos: [{ key: "v", meta: true }],
    },
  },
  {
    action: "new-session",
    scope: "global",
    help: "Attach a harness (opens the + menu)",
    chords: {
      windows: [{ key: "t", ...CTRL_SHIFT }],
      linux: [{ key: "t", ...CTRL_SHIFT }],
      macos: [{ key: "t", meta: true }],
    },
  },
  {
    action: "close-session",
    scope: "global",
    help: "Close the active session",
    chords: {
      windows: [{ key: "w", ...CTRL_SHIFT }],
      linux: [{ key: "w", ...CTRL_SHIFT }],
      macos: [{ key: "w", meta: true }],
    },
  },
  {
    action: "next-tab",
    scope: "global",
    help: "Next tab",
    chords: {
      windows: [{ key: "Tab", ctrl: true }],
      linux: [
        { key: "Tab", ctrl: true },
        { key: "PageDown", ctrl: true },
      ],
      macos: [
        { key: "Tab", ctrl: true },
        { key: "}", meta: true, shift: true },
      ],
    },
  },
  {
    action: "previous-tab",
    scope: "global",
    help: "Previous tab",
    chords: {
      windows: [{ key: "Tab", ...CTRL_SHIFT }],
      linux: [
        { key: "Tab", ...CTRL_SHIFT },
        { key: "PageUp", ctrl: true },
      ],
      macos: [
        { key: "Tab", ...CTRL_SHIFT },
        { key: "{", meta: true, shift: true },
      ],
    },
  },
  {
    action: "tab-select-previous",
    scope: "tab",
    help: "With a tab focused: select the previous tab",
    chords: everywhere([{ key: "ArrowLeft" }]),
  },
  {
    action: "tab-select-next",
    scope: "tab",
    help: "With a tab focused: select the next tab",
    chords: everywhere([{ key: "ArrowRight" }]),
  },
  {
    action: "tab-select-first",
    scope: "tab",
    help: "With a tab focused: select the first tab",
    chords: everywhere([{ key: "Home" }]),
  },
  {
    action: "tab-select-last",
    scope: "tab",
    help: "With a tab focused: select the last tab",
    chords: everywhere([{ key: "End" }]),
  },
  {
    action: "tab-move-left",
    scope: "tab",
    help: "With a tab focused: move it left",
    chords: everywhere([{ key: "ArrowLeft", ...CTRL_SHIFT }]),
  },
  {
    action: "tab-move-right",
    scope: "tab",
    help: "With a tab focused: move it right",
    chords: everywhere([{ key: "ArrowRight", ...CTRL_SHIFT }]),
  },
  {
    action: "fullscreen",
    scope: "always",
    help: "Fullscreen on / off — double-clicking the bar also leaves fullscreen",
    chords: {
      windows: [{ key: "F11" }],
      linux: [{ key: "F11" }],
      macos: [{ key: "f", ctrl: true, meta: true }],
    },
  },
  {
    action: "help",
    scope: "always",
    help: "This page",
    chords: everywhere([{ key: "F1" }]),
  },
  {
    // Handled by the menu-scoped listeners in main.ts; declared here so the
    // help page and the README describe it from the same table.
    action: "close-menu",
    scope: "always",
    help: "Closes any open menu",
    chords: everywhere([{ key: "Escape" }]),
  },
];

export interface KeyInput {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}

export interface KeyContext {
  /** The event target is an input, textarea, select or contenteditable. */
  editable: boolean;
  /** A session terminal owns focus. */
  terminalFocused: boolean;
  /** A session tab button owns focus. */
  tabFocused: boolean;
  /** Evaluated lazily and only for `requiresSelection` chords. */
  hasSelection: () => boolean;
}

function normalizeKey(key: string): string {
  return key.length === 1 ? key.toLowerCase() : key;
}

export function chordMatches(chord: Chord, input: KeyInput): boolean {
  return (
    normalizeKey(chord.key) === normalizeKey(input.key) &&
    Boolean(chord.ctrl) === input.ctrlKey &&
    Boolean(chord.shift) === input.shiftKey &&
    Boolean(chord.alt) === input.altKey &&
    Boolean(chord.meta) === input.metaKey
  );
}

function scopeAdmits(scope: KeyScope, context: KeyContext): boolean {
  switch (scope) {
    case "always":
      return true;
    case "global":
      return !context.editable;
    case "terminal":
      return context.terminalFocused && !context.editable;
    case "tab":
      return context.tabFocused && !context.editable;
  }
}

export interface KeyMatch {
  action: KeyAction;
  /** The chord that matched — the dispatcher reads `requiresSelection` off it. */
  chord: Chord;
}

/**
 * The binding a key event resolves to on `platform`, or null when the app must
 * not consume it. A null means "let it through": to the browser (native paste
 * in a textarea) or to xterm (bytes to the pty).
 */
export function resolveKeyMatch(
  input: KeyInput,
  platform: Platform,
  context: KeyContext,
): KeyMatch | null {
  for (const binding of KEY_BINDINGS) {
    if (!scopeAdmits(binding.scope, context)) {
      continue;
    }
    for (const chord of binding.chords[platform]) {
      if (!chordMatches(chord, input)) {
        continue;
      }
      if (chord.requiresSelection && !context.hasSelection()) {
        continue;
      }
      return { action: binding.action, chord };
    }
  }
  return null;
}

export function resolveKeyAction(
  input: KeyInput,
  platform: Platform,
  context: KeyContext,
): KeyAction | null {
  return resolveKeyMatch(input, platform, context)?.action ?? null;
}

export function chordsFor(action: KeyAction, platform: Platform): readonly Chord[] {
  return KEY_BINDINGS.find((binding) => binding.action === action)?.chords[platform] ?? [];
}

const KEY_LABELS: Record<string, string> = {
  ArrowLeft: "←",
  ArrowRight: "→",
  ArrowUp: "↑",
  ArrowDown: "↓",
  Escape: "Esc",
  " ": "Space",
};

/** "Ctrl+Shift+C", "Cmd+V", "F11" — the modifier order every platform reads naturally. */
export function describeChord(chord: Chord, platform: Platform): string {
  const parts: string[] = [];
  if (chord.ctrl) {
    parts.push("Ctrl");
  }
  if (chord.alt) {
    parts.push(platform === "macos" ? "Option" : "Alt");
  }
  if (chord.shift) {
    parts.push("Shift");
  }
  if (chord.meta) {
    parts.push(platform === "macos" ? "Cmd" : "Win");
  }
  const key = KEY_LABELS[chord.key] ?? (chord.key.length === 1 ? chord.key.toUpperCase() : chord.key);
  parts.push(key);
  return parts.join("+");
}

export interface HelpChord {
  /** "Ctrl+Shift+C" */
  label: string;
  /** "with a selection", or null when the chord is unconditional. */
  note: string | null;
}

export interface HelpRow {
  action: KeyAction;
  /** Every chord for the platform, in declaration order. */
  chords: HelpChord[];
  help: string;
}

/** The help page and README rows for one platform, in declaration order. */
export function helpRows(platform: Platform): HelpRow[] {
  return KEY_BINDINGS.map((binding) => ({
    action: binding.action,
    chords: binding.chords[platform].map((chord) => ({
      label: describeChord(chord, platform),
      note: chord.requiresSelection ? "with a selection" : null,
    })),
    help: binding.help,
  }));
}

export interface PlatformHints {
  /** `navigator.userAgentData?.platform` or `navigator.platform`. */
  platform: string;
  userAgent: string;
}

/**
 * WebView2 reports "Win32" / "Windows NT"; WebKitGTK reports "Linux x86_64" /
 * "X11; Linux"; WKWebView reports "MacIntel" / "Macintosh". Anything else is
 * treated as Linux, the convention with the fewest surprises.
 */
export function platformFromHints(hints: PlatformHints): Platform {
  const haystack = `${hints.platform} ${hints.userAgent}`;
  if (/mac|darwin|iphone|ipad/i.test(haystack)) {
    return "macos";
  }
  if (/win/i.test(haystack)) {
    return "windows";
  }
  return "linux";
}

export function detectPlatform(): Platform {
  const nav = globalThis.navigator as
    | (Navigator & { userAgentData?: { platform?: string } })
    | undefined;
  return platformFromHints({
    platform: nav?.userAgentData?.platform ?? nav?.platform ?? "",
    userAgent: nav?.userAgent ?? "",
  });
}
