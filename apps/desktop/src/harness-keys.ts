/**
 * What each harness receives for a bound action (D10).
 *
 * keymap.ts holds the ONE chord the user binds per action; this table says
 * what PRIM-1 writes into each harness's terminal when that chord is pressed
 * in its pane, so one key does the same thing in every harness. Every byte
 * sequence below was verified read-only, never by running a harness:
 *
 * - Claude Code: the keybinding table and key parser in the installed
 *   claude.exe 2.1.296 (%USERPROFILE%\.local\bin, the binary PRIM-1 resolves).
 * - Codex: codex.exe 0.162.1 (%LOCALAPPDATA%\Programs\OpenAI\Codex\bin), read
 *   against openai/codex tag rust-v0.162.1, codex-rs/tui/src.
 * - Grok Build: the keyboard reference embedded in the installed grok.exe
 *   (%USERPROFILE%\.grok\bin, 13 Oct build, version string 1.0.52).
 * - Windows input path: the bytes reach console harnesses through ConPTY,
 *   whose input engine (microsoft/terminal src/terminal/parser/
 *   InputStateMachineEngine.cpp) turns CR into Enter, ESC + CR into
 *   Alt+Enter, ESC + letter into Alt+letter and ^C into Ctrl+C.
 *
 * Shift+Enter cannot be sent as such: xterm and ConPTY carry it as a bare CR,
 * the same as Enter. New line therefore sends each harness's documented
 * alternative, Alt+Enter (ESC CR).
 *
 * Where nothing is verified the entry says so, and the dispatcher leaves the
 * keystroke alone: xterm encodes it exactly as it did before this table.
 * Only the Windows column exists (PRIM-1 ships on Windows only); elsewhere
 * every byte translation reads as unverified.
 */

import type { BindableAction, Platform } from "./keymap";
import type { DriverKind } from "./types";

export type HarnessKeyTranslation =
  /** PRIM-1 writes `bytes` to the harness: its own key for the action. */
  | {
      kind: "keys";
      bytes: string;
      /** The harness's name for that key: "Alt+Enter", "Ctrl+V". */
      harnessKey: string;
      source: string;
    }
  /** PRIM-1 itself does it the same way for every harness (copy, paste text). */
  | { kind: "terminal"; label: string; source: string }
  /** The harness has no such function; the keystroke passes through. */
  | { kind: "unsupported"; reason: string }
  /** Not verified; the keystroke passes through unchanged. */
  | { kind: "unverified"; note: string };

/** Settings → Keys column order. */
export const KEY_TABLE_DRIVERS: readonly DriverKind[] = [
  "claude",
  "codex",
  "grok",
  "generic_terminal",
  "prime",
];

const ESC = "\x1b";
const CR = "\r";

const CLAUDE = "claude.exe 2.1.296";
const CODEX = "openai/codex rust-v0.162.1";
const GROK = "grok.exe keyboard reference";
const CONHOST = "ConPTY input engine";

const PRIME_UNVERIFIED: HarnessKeyTranslation = {
  kind: "unverified",
  note: "Prime is parked in v2 and its agent runs inside WSL, which was not read.",
};

const COPY: HarnessKeyTranslation = {
  kind: "terminal",
  label: "Copies the selection",
  source: "PRIM-1 writes the selection to the clipboard; the harness receives nothing.",
};

const PASTE_TEXT: HarnessKeyTranslation = {
  kind: "terminal",
  label: "Bracketed paste",
  source:
    "The webview's own paste: control characters removed (paste.ts), CRLF turned into CR and the text bracketed when the harness enabled bracketed paste (xterm 6.0.0 Clipboard.ts paste()).",
};

function keys(bytes: string, harnessKey: string, source: string): HarnessKeyTranslation {
  return { kind: "keys", bytes, harnessKey, source };
}

/** The Windows column: per action, per harness. */
export const HARNESS_KEYS: Readonly<
  Record<BindableAction, Readonly<Record<DriverKind, HarnessKeyTranslation>>>
> = {
  copy: {
    claude: COPY,
    codex: COPY,
    grok: COPY,
    generic_terminal: COPY,
    prime: COPY,
  },
  "paste-text": {
    claude: PASTE_TEXT,
    codex: PASTE_TEXT,
    grok: PASTE_TEXT,
    generic_terminal: PASTE_TEXT,
    prime: PASTE_TEXT,
  },
  "paste-image": {
    claude: keys(
      ESC + "v",
      "Alt+V",
      `${CLAUDE}: Windows binds "alt+v" to chat:imagePaste (Me = windows||wsl ? "alt+v" : "ctrl+v"); the parser reads ESC + letter as meta+letter.`,
    ),
    codex: keys(
      "\x16",
      "Ctrl+V",
      `${CODEX}: keymap.rs:2474 fixed.paste_image = ctrl('v'); the binary's own tip: "Paste an image with Ctrl+V to attach it to your next message."`,
    ),
    grok: keys(
      ESC + "v",
      "Alt+V",
      `${GROK}: "Paste image: Alt+V (Windows only — for screenshots / Copy Image)".`,
    ),
    generic_terminal: { kind: "unsupported", reason: "A shell has no image paste." },
    prime: PRIME_UNVERIFIED,
  },
  submit: {
    claude: keys(
      CR,
      "Enter",
      `${CLAUDE}: Chat binds enter to chat:submit; the parser reads CR as return, bound as "enter".`,
    ),
    codex: keys(
      CR,
      "Enter",
      `${CODEX}: keymap.rs:1683 composer.submit = plain(Enter); chat_composer.rs:3498. A typed Enter needs no Right-arrow fence; that is only for programmatic room delivery.`,
    ),
    grok: keys(
      CR,
      "Enter",
      `${GROK}: "Send: Enter"; PRIM-1 room delivery also submits Grok with CR (supervisor routed_message_submit_behavior).`,
    ),
    generic_terminal: keys(
      CR,
      "Enter",
      `${CONHOST}: CR is the Enter key; PRIM-1 room delivery submits a Terminal pane with CR.`,
    ),
    prime: PRIME_UNVERIFIED,
  },
  newline: {
    claude: keys(
      ESC + CR,
      "Alt+Enter",
      `${CLAUDE}: the parser reads ESC CR as return+meta and the prompt inserts a new line for return with shift or meta; /terminal-setup binds Shift+Enter to ESC CR.`,
    ),
    codex: keys(
      ESC + CR,
      "Alt+Enter",
      `${CODEX}: keymap.rs:1693-1698 editor.insert_newline includes alt(Enter); ${CONHOST}: ESC CR arrives as Alt+Enter (ActionExecuteFromEscape).`,
    ),
    grok: keys(
      ESC + CR,
      "Alt+Enter",
      `${GROK}: "Newline: Shift+Enter or Alt+Enter"; grok doctor: "xterm.js cannot distinguish Shift+Enter".`,
    ),
    generic_terminal: {
      kind: "unverified",
      note: "PowerShell starts a new line with Shift+Enter; carrying that key through ConPTY is not verified.",
    },
    prime: PRIME_UNVERIFIED,
  },
  interrupt: {
    claude: keys(
      "\x03",
      "Ctrl+C",
      `${CLAUDE}: Global binds "ctrl+c" to app:interrupt; the parser reads a C0 byte as ctrl+letter.`,
    ),
    codex: keys(
      "\x03",
      "Ctrl+C",
      `${CODEX}: keymap.rs:2470 fixed.interrupt_or_quit = ctrl('c'); chatwidget/interaction.rs:140 on_ctrl_c.`,
    ),
    grok: keys(
      "\x03",
      "Ctrl+C",
      `${GROK}: "Cancel (running): Ctrl+C (empty prompt; non-empty draft clears first)".`,
    ),
    generic_terminal: keys(
      "\x03",
      "Ctrl+C",
      `${CONHOST}: ^C becomes the console's Ctrl+C (_DoControlCharacter), the shell's interrupt.`,
    ),
    prime: PRIME_UNVERIFIED,
  },
};

/** The harness actions a pane's chord can trigger (copy is PRIM-1's own). */
export type PaneAction = Exclude<BindableAction, "copy">;

/** What the keydown dispatcher does with a pane action's chord. */
export interface KeyPlan {
  /** Cancel the webview's default action — its own paste among them. */
  preventDefault: boolean;
  /** Keep xterm from also encoding the chord. */
  stopPropagation: boolean;
  /** Written to the pane as typed input; null writes nothing. */
  send: string | null;
}

/**
 * Paste text lets the webview's own paste run (SessionTerminal.paste takes it
 * from there). A verified translation is sent instead of the keystroke, with
 * the webview's default cancelled, so Ctrl+Shift+V bound to Paste image
 * attaches an image and pastes no text. Anything unverified or unsupported
 * passes through untouched: xterm encodes the keystroke as before, and a
 * paste chord (Ctrl+Shift+V in a Terminal pane) still pastes text.
 */
export function harnessKeyPlan(
  action: PaneAction,
  driver: DriverKind | undefined,
  platform: Platform,
): KeyPlan {
  if (action === "paste-text") {
    return { preventDefault: false, stopPropagation: true, send: null };
  }
  const translation = driver ? harnessKeyTranslation(action, driver, platform) : null;
  if (translation?.kind !== "keys") {
    return { preventDefault: false, stopPropagation: false, send: null };
  }
  return { preventDefault: true, stopPropagation: true, send: translation.bytes };
}

/** What `driver` receives for `action` on `platform`. */
export function harnessKeyTranslation(
  action: BindableAction,
  driver: DriverKind,
  platform: Platform,
): HarnessKeyTranslation {
  const translation = HARNESS_KEYS[action][driver];
  if (platform !== "windows" && translation.kind === "keys") {
    return {
      kind: "unverified",
      note: "Verified on Windows only; PRIM-1 has no port to this platform yet.",
    };
  }
  return translation;
}

const BYTE_NAMES: Record<string, string> = {
  [ESC]: "ESC",
  [CR]: "CR",
  "\n": "LF",
  "\t": "TAB",
};

/** "ESC CR", "^V", "CR" — the bytes as a terminal person reads them. */
export function describeKeyBytes(bytes: string): string {
  return Array.from(bytes)
    .map((char) => {
      const named = BYTE_NAMES[char];
      if (named) {
        return named;
      }
      const code = char.charCodeAt(0);
      if (code >= 0x01 && code <= 0x1a) {
        return "^" + String.fromCharCode(code + 0x40);
      }
      if (code < 0x20 || code === 0x7f) {
        return "0x" + code.toString(16).padStart(2, "0");
      }
      return char;
    })
    .join(" ");
}
