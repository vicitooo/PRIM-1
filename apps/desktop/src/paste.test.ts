import { describe, expect, it } from "vitest";

import { planPaste, sanitizePaste } from "./paste";

describe("sanitizePaste", () => {
  it("pasted text can never end the bracket early", () => {
    expect(sanitizePaste("a\x1b[201~b")).toEqual({ text: "a[201~b", removed: 1 });
    expect(sanitizePaste("a\x1b[200~b")).toEqual({ text: "a[200~b", removed: 1 });
    // The 8-bit CSI (C1) form of the same sequence.
    expect(sanitizePaste("a\u009b201~b")).toEqual({ text: "a201~b", removed: 1 });
    // Removing characters cannot assemble a new ESC.
    const nested = sanitizePaste("x\x1b[20\x1b[201~1~\x1b\x1b[201~");
    expect(nested.text).not.toMatch(/[\u001b\u009b]/);
    expect(nested.removed).toBe(4);
  });

  it("removes the controls that would act as keys: ^C, ^V, NUL, DEL, C1", () => {
    expect(sanitizePaste("stop\x03here")).toEqual({ text: "stophere", removed: 1 });
    expect(sanitizePaste("\x00\x16\x7f\u0085")).toEqual({ text: "", removed: 4 });
    expect(sanitizePaste("\x1b[A up")).toEqual({ text: "[A up", removed: 1 });
  });

  it("keeps tab, line breaks and every printable character", () => {
    const text = "first\tline\r\nsecond line\nthird\rможе ли 🙂 ñ";
    expect(sanitizePaste(text)).toEqual({ text, removed: 0 });
    expect(sanitizePaste("")).toEqual({ text: "", removed: 0 });
  });
});

describe("planPaste", () => {
  it("text wins, sanitized; text of nothing but controls pastes nothing", () => {
    expect(planPaste("hi\x1b[201~", true, "claude", "windows")).toEqual({
      kind: "text",
      text: "hi[201~",
      removed: 1,
    });
    expect(planPaste("\x1b\x03", false, "codex", "windows")).toEqual({
      kind: "nothing",
      reason: "only-controls",
    });
    expect(planPaste("", false, "grok", "windows")).toEqual({ kind: "nothing", reason: "empty" });
  });

  it("an image alone sends the harness its own image key, where it has one", () => {
    expect(planPaste("", true, "claude", "windows")).toEqual({ kind: "image", bytes: "\x1bv" });
    expect(planPaste("", true, "codex", "windows")).toEqual({ kind: "image", bytes: "\x16" });
    expect(planPaste("", true, "grok", "windows")).toEqual({ kind: "image", bytes: "\x1bv" });
    expect(planPaste("", true, "generic_terminal", "windows")).toEqual({
      kind: "nothing",
      reason: "no-image-key",
    });
    expect(planPaste("", true, "prime", "windows")).toEqual({ kind: "nothing", reason: "no-image-key" });
    expect(planPaste("", true, "claude", "linux")).toEqual({ kind: "nothing", reason: "no-image-key" });
  });
});
