/* Files dropped on the window become text: quoted paths, the way a terminal
   writes a drag. Claude Code and Codex attach an image from such a path. */

/** Backslash-escape everything a shell or harness would split a path on. */
export function quoteDroppedPath(path: string): string {
  return path.replace(/([^A-Za-z0-9_\-.,:+@%/\u0080-￿])/g, "\\$1");
}

/** The text a drop inserts, or "" when nothing usable was dropped. */
export function droppedPathsText(paths: readonly string[]): string {
  const usable = paths.filter((path) => path.length > 0 && !/[\r\n]/.test(path));
  return usable.length > 0 ? `${usable.map(quoteDroppedPath).join(" ")} ` : "";
}
