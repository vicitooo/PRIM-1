/**
 * The one place a key binding lives.
 *
 * Every chord the app consumes is declared here, per platform, and everything
 * else reads this table: the keydown dispatcher in `main.ts`, the tab
 * resolvers in `session-tabs.ts`, the F1 help page, Settings → Keys, and the
 * README table. A chord that is not declared here is not consumed by the app
 * and reaches the focused terminal as bytes (xterm's own mapping).
 *
 * One keymap for every harness (D10, 13 Oct, Victor: "If I want Ctrl+V to be
 * pasted, I can do this in all systems. If I want Ctrl+Shift+V to be insert
 * image, I should be able to do so in all systems"): the six harness actions
 * — copy, paste text, paste image, submit, new line, interrupt — carry ONE
 * chord each, and Settings → Keys rebinds it. What a harness then receives
 * for the action is `harness-keys.ts`'s job.
 *
 * A chord belongs to, in this order: the action the user explicitly bound to
 * it; else the action whose platform default it is; else the action whose
 * platform convention it is (Windows Terminal's Ctrl+Shift+C, Ctrl+Insert,
 * Shift+Insert, …). So a binding takes a convention chord over, and a
 * convention nobody claims keeps working. Only the explicit bindings are
 * stored. Everything else here (tabs, attach, close, fullscreen, help, menus)
 * is fixed and cannot be taken.
 *
 * Letters and digits match the physical key (`KeyboardEvent.code`) when the
 * typed character is not ASCII, so Ctrl+C and Ctrl+V work on a Bulgarian
 * (Cyrillic) layout. An ASCII character keeps its own meaning, so Dvorak and
 * AZERTY users keep their letters.
 *
 * Platform conventions (2026-09-08 decision, Victor): Windows follows Windows
 * Terminal, Linux follows GNOME Terminal, macOS follows Terminal.app / iTerm2.
 * PRIM-1 ships on Windows only; the Linux and macOS columns are written from
 * convention and unit-tested, and have not been verified on hardware.
 */

export type Platform = "windows" | "linux" | "macos";

export const PLATFORMS: readonly Platform[] = ["windows", "linux", "macos"];

/**
 * The actions bound once in Settings → Keys and translated per harness.
 * Declaration order is the Keys page order and the resolution order: copy
 * comes first because its chord applies only with a selection, and the
 * interrupt may share it (Windows Ctrl+C).
 */
export const BINDABLE_ACTIONS = [
  "copy",
  "paste-text",
  "paste-image",
  "submit",
  "newline",
  "interrupt",
] as const;

export type BindableAction = (typeof BINDABLE_ACTIONS)[number];

export type FixedAction =
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

export type KeyAction = BindableAction | FixedAction;

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
  /**
   * `KeyboardEvent.key` as `eventKey` reads it: one character lowercased, a
   * letter or digit typed on a non-Latin layout as its Latin key, named keys
   * ("Enter", "F11") as they are.
   */
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
  /** The action's name: Settings → Keys rows and clash messages. */
  label: string;
  /** One line for the help page; the chords are rendered in front of it. */
  help: string;
  chords: Record<Platform, readonly Chord[]>;
}

/** One platform's bindings with the user's chords applied. */
export interface PlatformBinding {
  action: KeyAction;
  scope: KeyScope;
  label: string;
  help: string;
  chords: readonly Chord[];
}

/** The user's explicit chords — only for the actions they rebound. */
export type UserBindings = Readonly<Partial<Record<BindableAction, Chord>>>;

/** No explicit binding: every action on its platform default. */
export const DEFAULT_BINDINGS: UserBindings = Object.freeze({});

const CTRL_SHIFT = { ctrl: true, shift: true } as const;

function everywhere<T>(value: T): Record<Platform, T> {
  return { windows: value, linux: value, macos: value };
}

interface BindableSpec {
  action: BindableAction;
  scope: KeyScope;
  label: string;
  help: string;
  /** The chord until the user picks another in Settings → Keys. */
  defaults: Record<Platform, Chord>;
  /**
   * Platform conventions that also run the action while no binding or
   * default claims them (see ownChords).
   */
  conventions: Record<Platform, readonly Chord[]>;
}

const BINDABLE_SPECS: readonly BindableSpec[] = [
  {
    action: "copy",
    scope: "global",
    label: "Copy",
    help: "Copy the selection (a pane, the system log, or page text)",
    defaults: {
      windows: { key: "c", ctrl: true },
      linux: { key: "c", ...CTRL_SHIFT },
      macos: { key: "c", meta: true },
    },
    conventions: {
      windows: [
        { key: "c", ...CTRL_SHIFT },
        { key: "Insert", ctrl: true },
      ],
      linux: [{ key: "Insert", ctrl: true }],
      macos: [],
    },
  },
  {
    action: "paste-text",
    scope: "terminal",
    label: "Paste text",
    help: "Paste the clipboard into the focused terminal (bracketed; control characters removed)",
    defaults: {
      windows: { key: "v", ctrl: true },
      linux: { key: "v", ...CTRL_SHIFT },
      macos: { key: "v", meta: true },
    },
    conventions: {
      // Windows Terminal's Ctrl+Shift+V pastes text only while nothing else
      // claims it; by default Paste image does (Victor's mapping).
      windows: [
        { key: "v", ...CTRL_SHIFT },
        { key: "Insert", shift: true },
      ],
      linux: [{ key: "Insert", shift: true }],
      macos: [],
    },
  },
  {
    action: "paste-image",
    scope: "terminal",
    label: "Paste image",
    help: "Attach the clipboard image, in harnesses that take one",
    // Windows: Victor's mapping (Ctrl+V pastes, Ctrl+Shift+V inserts an image).
    // Linux and macOS: Ctrl+V, the harnesses' own image key there, which
    // GNOME Terminal and Terminal.app leave to the application.
    defaults: {
      windows: { key: "v", ...CTRL_SHIFT },
      linux: { key: "v", ctrl: true },
      macos: { key: "v", ctrl: true },
    },
    conventions: everywhere([]),
  },
  {
    action: "submit",
    scope: "terminal",
    label: "Submit",
    help: "Send the prompt",
    defaults: everywhere({ key: "Enter" }),
    conventions: everywhere([]),
  },
  {
    action: "newline",
    scope: "terminal",
    label: "New line",
    help: "Start a new line in the prompt without sending it",
    defaults: everywhere({ key: "Enter", shift: true }),
    conventions: everywhere([]),
  },
  {
    action: "interrupt",
    scope: "terminal",
    label: "Interrupt",
    help: "Interrupt the harness (with nothing selected)",
    defaults: everywhere({ key: "c", ctrl: true }),
    conventions: everywhere([]),
  },
];

/**
 * The chords the webview itself pastes with. Paste text must use one of them:
 * PRIM-1 lets the webview's own paste reach the pane (dictation tools such as
 * Wispr Flow wait for exactly that clipboard read) instead of reading the
 * clipboard itself. Windows is the shipped column; WebKitGTK's and WKWebView's
 * paste chords are unverified until those ports exist.
 */
const NATIVE_PASTE_CHORDS: Record<Platform, readonly Chord[]> = {
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
};

/** Declaration order is help-page order, after the harness actions. */
const FIXED_BINDINGS: readonly KeyBinding[] = [
  {
    action: "new-session",
    scope: "global",
    label: "Attach a harness",
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
    label: "Close session",
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
    label: "Next tab",
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
    label: "Previous tab",
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
    label: "Select the previous tab",
    help: "With a tab focused: select the previous tab",
    chords: everywhere([{ key: "ArrowLeft" }]),
  },
  {
    action: "tab-select-next",
    scope: "tab",
    label: "Select the next tab",
    help: "With a tab focused: select the next tab",
    chords: everywhere([{ key: "ArrowRight" }]),
  },
  {
    action: "tab-select-first",
    scope: "tab",
    label: "Select the first tab",
    help: "With a tab focused: select the first tab",
    chords: everywhere([{ key: "Home" }]),
  },
  {
    action: "tab-select-last",
    scope: "tab",
    label: "Select the last tab",
    help: "With a tab focused: select the last tab",
    chords: everywhere([{ key: "End" }]),
  },
  {
    action: "tab-move-left",
    scope: "tab",
    label: "Move the tab left",
    help: "With a tab focused: move it left",
    chords: everywhere([{ key: "ArrowLeft", ...CTRL_SHIFT }]),
  },
  {
    action: "tab-move-right",
    scope: "tab",
    label: "Move the tab right",
    help: "With a tab focused: move it right",
    chords: everywhere([{ key: "ArrowRight", ...CTRL_SHIFT }]),
  },
  {
    action: "fullscreen",
    scope: "always",
    label: "Fullscreen",
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
    label: "Help",
    help: "This page",
    chords: everywhere([{ key: "F1" }]),
  },
  {
    // Handled by the menu-scoped listeners in main.ts; declared here so the
    // help page and the README describe it from the same table.
    action: "close-menu",
    scope: "always",
    label: "Close menu",
    help: "Closes any open menu",
    chords: everywhere([{ key: "Escape" }]),
  },
];

function bindableSpec(action: BindableAction): BindableSpec {
  const spec = BINDABLE_SPECS.find((candidate) => candidate.action === action);
  if (!spec) {
    throw new Error(`unknown bindable action ${action}`);
  }
  return spec;
}

/** The chord `action` has on `platform` until the user rebinds it. */
export function defaultChord(action: BindableAction, platform: Platform): Chord {
  return bindableSpec(action).defaults[platform];
}

/** Copy applies only with a selection, so it may share its chord with the interrupt (Windows Ctrl+C). */
function mayShare(a: BindableAction, b: BindableAction): boolean {
  return (a === "copy" && b === "interrupt") || (a === "interrupt" && b === "copy");
}

/**
 * Each harness action's own chord: the user's explicit binding; else the
 * platform default, unless another action's explicit binding has taken that
 * chord — then the action has none (only a store written under other
 * defaults can do that: Settings → Keys refuses it).
 */
export function ownChords(
  platform: Platform,
  user: UserBindings = DEFAULT_BINDINGS,
): Record<BindableAction, Chord | null> {
  const own = {} as Record<BindableAction, Chord | null>;
  for (const spec of BINDABLE_SPECS) {
    const explicit = user[spec.action];
    if (explicit) {
      own[spec.action] = explicit;
      continue;
    }
    const fallback = spec.defaults[platform];
    const taken = BINDABLE_ACTIONS.some((other) => {
      const theirs = user[other];
      return other !== spec.action && !mayShare(spec.action, other) && theirs !== undefined && sameChord(theirs, fallback);
    });
    own[spec.action] = taken ? null : fallback;
  }
  return own;
}

/**
 * The action's own chord, then its conventions that no action's own chord
 * has claimed. Copy's own chord always waits for a selection: without one
 * the keys belong to whatever comes next (the interrupt, or the terminal).
 */
function bindableChords(
  spec: BindableSpec,
  platform: Platform,
  own: Record<BindableAction, Chord | null>,
): Chord[] {
  const claimed = Object.values(own).filter((chord): chord is Chord => chord !== null);
  const chords: Chord[] = [];
  const mine = own[spec.action];
  if (mine) {
    chords.push(spec.action === "copy" ? { ...mine, requiresSelection: true } : mine);
  }
  for (const convention of spec.conventions[platform]) {
    if (!claimed.some((chord) => sameChord(chord, convention))) {
      chords.push(convention);
    }
  }
  return chords;
}

/**
 * Every binding on one platform with the user's chords applied, in
 * resolution order. No two actions end up holding the same chord, so the
 * order matters only for copy's selection-only chord, which comes first.
 */
export function platformBindings(
  platform: Platform,
  user: UserBindings = DEFAULT_BINDINGS,
): PlatformBinding[] {
  const own = ownChords(platform, user);
  return [
    ...BINDABLE_SPECS.map((spec) => ({
      action: spec.action,
      scope: spec.scope,
      label: spec.label,
      help: spec.help,
      chords: bindableChords(spec, platform, own),
    })),
    ...FIXED_BINDINGS.map((binding) => ({
      action: binding.action,
      scope: binding.scope,
      label: binding.label,
      help: binding.help,
      chords: binding.chords[platform],
    })),
  ];
}

/** The default table across every platform (what a fresh install binds). */
export const KEY_BINDINGS: readonly KeyBinding[] = (() => {
  const windows = platformBindings("windows");
  const linux = platformBindings("linux");
  const macos = platformBindings("macos");
  return windows.map((binding, index) => ({
    action: binding.action,
    scope: binding.scope,
    label: binding.label,
    help: binding.help,
    chords: {
      windows: binding.chords,
      linux: linux[index]?.chords ?? [],
      macos: macos[index]?.chords ?? [],
    },
  }));
})();

export interface KeyInput {
  key: string;
  /** `KeyboardEvent.code`; letters and digits fall back to it off a Latin layout. */
  code?: string;
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

const PHYSICAL_KEY = /^(?:Key([A-Z])|Digit([0-9]))$/;

/**
 * The key an event compares as. One typed character compares lowercased; a
 * character outside ASCII typed on a letter or digit key compares as that
 * key's Latin letter or digit (`code` "KeyV" → "v"), so Ctrl+V on the
 * Bulgarian layout (`key` "ж") is Ctrl+V. An ASCII character always keeps its
 * own meaning.
 */
export function eventKey(input: Pick<KeyInput, "key" | "code">): string {
  const { key, code } = input;
  if (key.length === 1 && key.charCodeAt(0) > 0x7f && code) {
    const physical = PHYSICAL_KEY.exec(code);
    if (physical) {
      return (physical[1] ?? physical[2] ?? key).toLowerCase();
    }
  }
  return normalizeKey(key);
}

export function chordMatches(chord: Chord, input: KeyInput): boolean {
  return (
    normalizeKey(chord.key) === eventKey(input) &&
    Boolean(chord.ctrl) === input.ctrlKey &&
    Boolean(chord.shift) === input.shiftKey &&
    Boolean(chord.alt) === input.altKey &&
    Boolean(chord.meta) === input.metaKey
  );
}

/** Same keys and modifiers; `requiresSelection` is the action's, not the keys'. */
export function sameChord(a: Chord, b: Chord): boolean {
  return (
    normalizeKey(a.key) === normalizeKey(b.key) &&
    Boolean(a.ctrl) === Boolean(b.ctrl) &&
    Boolean(a.shift) === Boolean(b.shift) &&
    Boolean(a.alt) === Boolean(b.alt) &&
    Boolean(a.meta) === Boolean(b.meta)
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
 * in a textarea) or to xterm (bytes to the pty). A harness action resolves
 * even in a harness with no verified translation for it; the dispatcher then
 * lets the keystroke through itself (see harness-keys.ts).
 */
export function resolveKeyMatch(
  input: KeyInput,
  platform: Platform,
  context: KeyContext,
  user: UserBindings = DEFAULT_BINDINGS,
): KeyMatch | null {
  for (const binding of platformBindings(platform, user)) {
    if (!scopeAdmits(binding.scope, context)) {
      continue;
    }
    for (const chord of binding.chords) {
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
  user?: UserBindings,
): KeyAction | null {
  return resolveKeyMatch(input, platform, context, user)?.action ?? null;
}

export function chordsFor(
  action: KeyAction,
  platform: Platform,
  user?: UserBindings,
): readonly Chord[] {
  return platformBindings(platform, user).find((binding) => binding.action === action)?.chords ?? [];
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
export function helpRows(platform: Platform, user?: UserBindings): HelpRow[] {
  return platformBindings(platform, user).map((binding) => ({
    action: binding.action,
    chords: binding.chords.map((chord) => ({
      label: describeChord(chord, platform),
      note: chord.requiresSelection ? "with a selection" : null,
    })),
    help: binding.help,
  }));
}

/* ── Settings → Keys: capturing, checking and storing the user's chords ── */

/** Keys that are never a chord on their own: modifiers, dead keys, IME. */
const NOT_A_CHORD = new Set([
  "",
  "Shift",
  "Control",
  "Alt",
  "AltGraph",
  "Meta",
  "OS",
  "Super",
  "Hyper",
  "Fn",
  "FnLock",
  "CapsLock",
  "NumLock",
  "ScrollLock",
  "Symbol",
  "SymbolLock",
  "Dead",
  "Compose",
  "Process",
  "Unidentified",
]);

/** The chord a key press binds, or null while only a modifier (or a dead or IME key) is down. */
export function chordFromInput(input: KeyInput): Chord | null {
  if (NOT_A_CHORD.has(input.key)) {
    return null;
  }
  const chord: Chord = { key: eventKey(input) };
  if (input.ctrlKey) {
    chord.ctrl = true;
  }
  if (input.shiftKey) {
    chord.shift = true;
  }
  if (input.altKey) {
    chord.alt = true;
  }
  if (input.metaKey) {
    chord.meta = true;
  }
  return chord;
}

/** Keys that type or edit text when pressed without Ctrl, Alt or Cmd. */
const TEXT_KEYS = new Set(["Tab", "Backspace", "Delete"]);

function typesText(chord: Chord): boolean {
  return (
    !chord.ctrl &&
    !chord.alt &&
    !chord.meta &&
    (chord.key.length === 1 || TEXT_KEYS.has(chord.key))
  );
}

/** A chord the webview itself pastes with (Paste text must be one). */
export function isNativePasteChord(chord: Chord, platform: Platform): boolean {
  return NATIVE_PASTE_CHORDS[platform].some((native) => sameChord(native, chord));
}

/**
 * Why `chord` cannot be `action`'s key on `platform`, whatever the other
 * harness actions are bound to — or null when it can. A convention chord is
 * no problem: binding it takes it over.
 */
function chordProblem(action: BindableAction, chord: Chord, platform: Platform): string | null {
  const label = describeChord(chord, platform);
  if (chord.key === "Escape") {
    return "Esc is reserved: it cancels.";
  }
  if (typesText(chord)) {
    // Option+letter types a character on macOS, so Cmd is the second choice there.
    return `${label} types text — add Ctrl or ${platform === "macos" ? "Cmd" : "Alt"}.`;
  }
  if (chord.meta && platform !== "macos") {
    return platform === "windows"
      ? "The Windows key belongs to Windows."
      : "The Super key belongs to the desktop.";
  }
  if (action === "paste-text" && !isNativePasteChord(chord, platform)) {
    const natives = NATIVE_PASTE_CHORDS[platform].map((native) => describeChord(native, platform));
    return `Paste text uses the system paste: ${natives.join(", ")}.`;
  }
  for (const binding of FIXED_BINDINGS) {
    if (binding.chords[platform].some((fixed) => sameChord(fixed, chord))) {
      return `${label} is ${binding.label}.`;
    }
  }
  return null;
}

/**
 * The other action whose own chord (explicit or default) is `chord`, if any.
 * Copy and interrupt may share one.
 */
function chordOwner(
  action: BindableAction,
  chord: Chord,
  platform: Platform,
  user: UserBindings,
): BindableAction | null {
  const own = ownChords(platform, user);
  for (const other of BINDABLE_ACTIONS) {
    const theirs = own[other];
    if (other !== action && !mayShare(action, other) && theirs && sameChord(theirs, chord)) {
      return other;
    }
  }
  return null;
}

/**
 * Why `chord` cannot become `action`'s key, or null when it can. Another
 * action's own chord must be rebound first, so no action is left without a
 * key by accident; a convention chord is simply taken over. Settings → Keys
 * shows the message as is.
 */
export function checkUserChord(
  action: BindableAction,
  chord: Chord,
  platform: Platform,
  user: UserBindings,
): string | null {
  const problem = chordProblem(action, chord, platform);
  if (problem) {
    return problem;
  }
  const owner = chordOwner(action, chord, platform, user);
  return owner
    ? `${describeChord(chord, platform)} is ${bindableSpec(owner).label} — rebind that first.`
    : null;
}

/**
 * `user` with `action` bound to `chord`. A chord equal to the action's
 * platform default is no explicit binding: the action just follows its default.
 */
export function bindChord(
  action: BindableAction,
  chord: Chord,
  platform: Platform,
  user: UserBindings,
): UserBindings {
  const next: Partial<Record<BindableAction, Chord>> = { ...user };
  delete next[action];
  if (!sameChord(chord, defaultChord(action, platform))) {
    next[action] = keysOnly(chord);
  }
  return next;
}

function keysOnly(chord: Chord): Chord {
  const keys: Chord = { key: chord.key };
  for (const modifier of ["ctrl", "shift", "alt", "meta"] as const) {
    if (chord[modifier]) {
      keys[modifier] = true;
    }
  }
  return keys;
}

export type CaptureResult =
  /** Esc: the binding stays as it was. */
  | { kind: "cancelled" }
  /** A modifier (or a dead or IME key) on its own: keep listening. */
  | { kind: "waiting" }
  /** Settings → Keys shows `message` and keeps listening. */
  | { kind: "refused"; message: string }
  | { kind: "bound"; chord: Chord; bindings: UserBindings };

/** One key press while Settings → Keys listens for `action`'s new chord. */
export function captureChord(
  action: BindableAction,
  input: KeyInput,
  platform: Platform,
  user: UserBindings,
): CaptureResult {
  if (input.key === "Escape") {
    return { kind: "cancelled" };
  }
  const chord = chordFromInput(input);
  if (!chord) {
    return { kind: "waiting" };
  }
  const refusal = checkUserChord(action, chord, platform, user);
  if (refusal) {
    return { kind: "refused", message: refusal };
  }
  return { kind: "bound", chord, bindings: bindChord(action, chord, platform, user) };
}

function parseStoredChord(value: unknown): Chord | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return null;
  }
  const stored = value as Record<string, unknown>;
  if (typeof stored.key !== "string" || stored.key.length === 0 || stored.key.length > 32) {
    return null;
  }
  const chord: Chord = { key: normalizeKey(stored.key) };
  for (const modifier of ["ctrl", "shift", "alt", "meta"] as const) {
    const flag = stored[modifier];
    if (flag === undefined) {
      continue;
    }
    if (typeof flag !== "boolean") {
      return null;
    }
    if (flag) {
      chord[modifier] = true;
    }
  }
  return chord;
}

/**
 * The stored explicit bindings. An unknown action, an invalid chord or a chord
 * equal to the default is dropped (that action follows its default); two
 * bindings on one chord (only a damaged store holds them) drop everything.
 * A binding that took a convention chord, or another action's default under
 * an older table, is kept: explicit wins (ownChords).
 */
export function parseUserBindings(raw: string | null, platform: Platform): UserBindings {
  if (!raw) {
    return DEFAULT_BINDINGS;
  }
  let stored: unknown;
  try {
    stored = JSON.parse(raw);
  } catch {
    return DEFAULT_BINDINGS;
  }
  if (typeof stored !== "object" || stored === null || Array.isArray(stored)) {
    return DEFAULT_BINDINGS;
  }
  const entries = stored as Record<string, unknown>;
  let bindings: UserBindings = DEFAULT_BINDINGS;
  for (const action of BINDABLE_ACTIONS) {
    const chord = parseStoredChord(entries[action]);
    if (chord && chordProblem(action, chord, platform) === null) {
      bindings = bindChord(action, chord, platform, bindings);
    }
  }
  const explicit = BINDABLE_ACTIONS.filter((action) => bindings[action] !== undefined);
  const doubled = explicit.some((action) =>
    explicit.some((other) => {
      const mine = bindings[action];
      const theirs = bindings[other];
      return other !== action && !mayShare(action, other) && mine && theirs && sameChord(mine, theirs);
    }),
  );
  return doubled ? DEFAULT_BINDINGS : bindings;
}

/** What Settings → Keys stores: the explicit bindings only. Null when there are none (the store is removed). */
export function serializeUserBindings(bindings: UserBindings, platform: Platform): string | null {
  const stored: Partial<Record<BindableAction, Chord>> = {};
  for (const action of BINDABLE_ACTIONS) {
    const chord = bindings[action];
    if (chord && !sameChord(chord, defaultChord(action, platform))) {
      stored[action] = keysOnly(chord);
    }
  }
  return Object.keys(stored).length > 0 ? JSON.stringify(stored) : null;
}

/** The Settings → Keys row text for a harness action. */
export function bindableAction(action: BindableAction): { label: string; help: string } {
  const spec = bindableSpec(action);
  return { label: spec.label, help: spec.help };
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
