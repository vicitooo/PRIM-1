// Confirmation gates. The dialog plugin replaces window.confirm with an async
// invoke whose Promise is always truthy, so a sync `if (window.confirm(...))`
// never stops anything. Every gate awaits the native `confirm_action` dialog
// and acts only on an explicit true.

/** Run `act` only after the awaited answer is exactly `true`. A no, a closed
    dialog, a failed dialog (the ask rejects) or any other value do nothing. */
export async function afterConfirm(ask: () => Promise<boolean>, act: () => Promise<void>): Promise<boolean> {
  let answer: unknown = false;
  try {
    answer = await ask();
  } catch {
    answer = false;
  }
  if (answer !== true) return false;
  await act();
  return true;
}

export function startFreshConfirmText(label: string): string {
  return `Stop ${label}, then start a fresh conversation? The current conversation reference stays in its history.`;
}

/** `consequences` such as "stops it" or `removes it from room "X"`. */
export function closeSessionConfirmText(label: string, shortId: string, consequences: readonly string[]): string {
  return 'Close "' + label + '" (' + shortId + ")?"
    + (consequences.length > 0 ? " This " + consequences.join(" and ") + "." : "")
    + " Its terminal scrollback will be discarded.";
}

/** Copied or discovered conversations may still be open in another app. */
export function resumeHereConfirmText(label: string): string {
  return `Resume ${label}'s conversation here? Close this conversation in every other app or terminal first: two apps typing into one conversation will mix it up.`;
}

export function deleteRoomConfirmText(label: string): string {
  return `Delete room "${label}"? Its stored room history will be erased. Sessions will remain available.`;
}
