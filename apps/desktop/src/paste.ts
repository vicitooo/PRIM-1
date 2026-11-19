/**
 * Clipboard contents on their way into a pane.
 *
 * The webview's own paste reaches the pane (keymap.ts lets every paste chord
 * through to it), and xterm's `paste()` turns CRLF into CR and brackets the
 * text when the harness has enabled bracketed paste. Before that, every
 * control character except tab, line feed and carriage return is removed —
 * Windows Terminal's paste rule. Pasted text therefore can never carry
 * `ESC[201~` to end the bracket early, and can never smuggle keys: ConPTY
 * turns an escape sequence inside a paste into key events for the harnesses
 * that read console input.
 */

import { harnessKeyTranslation } from "./harness-keys";
import type { Platform } from "./keymap";
import type { DriverKind } from "./types";

/** C0 except tab, LF and CR; DEL; C1 (U+0080–U+009F, which includes the 8-bit CSI). */
const PASTE_CONTROLS = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/g;

export interface SanitizedPaste {
  text: string;
  /** How many control characters were removed. */
  removed: number;
}

export function sanitizePaste(text: string): SanitizedPaste {
  let removed = 0;
  const clean = text.replace(PASTE_CONTROLS, () => {
    removed += 1;
    return "";
  });
  return { text: clean, removed };
}

export type PastePlan =
  /** Hand `text` to xterm's paste(). */
  | { kind: "text"; text: string; removed: number }
  /** Write the harness's own paste-image key; it reads the clipboard itself. */
  | { kind: "image"; bytes: string }
  | { kind: "nothing"; reason: "empty" | "only-controls" | "no-image-key" };

/**
 * What one paste event does in a `driver` pane. Text wins: a clipboard holding
 * both (a browser image copy brings its HTML along) pastes the text. A
 * clipboard holding only an image sends the harness its paste-image key, so a
 * screenshot pasted with the text chord still arrives where a harness takes
 * images.
 */
export function planPaste(
  text: string,
  hasImage: boolean,
  driver: DriverKind,
  platform: Platform,
): PastePlan {
  if (text.length > 0) {
    const sanitized = sanitizePaste(text);
    return sanitized.text.length > 0
      ? { kind: "text", ...sanitized }
      : { kind: "nothing", reason: "only-controls" };
  }
  if (!hasImage) {
    return { kind: "nothing", reason: "empty" };
  }
  const translation = harnessKeyTranslation("paste-image", driver, platform);
  return translation.kind === "keys"
    ? { kind: "image", bytes: translation.bytes }
    : { kind: "nothing", reason: "no-image-key" };
}
