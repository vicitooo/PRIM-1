/**
 * v3 W6 (E, Claude PRIM-1 8afd010f): automatic DOM focus (a pane becoming
 * visible, a member starting) moves focus only inside a window the user is
 * already in. In a background or minimized window WebView2 can turn an
 * element's focus() into an OS focus request, which takes the user's focus.
 * Returns whether focus was moved.
 */
export function focusWithinActiveWindow(
  target: { focus(): void } | null | undefined,
  doc: Pick<Document, "hasFocus"> = document,
): boolean {
  if (!target || !doc.hasFocus()) {
    return false;
  }
  target.focus();
  return true;
}
