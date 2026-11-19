/// <reference types="vite/client" />
/* T-35 (Victor, room 2844: "if I click on the actual scroll bar, it minimizes
   the app"). Tauri's drag.js starts a window drag, and a double click toggles
   maximize, on a mousedown whose target is a bare data-tauri-drag-region. A
   click on a scroll container's own scrollbar targets the container itself,
   so no scrolling element of the shell may be a drag region. */
import { describe, expect, it } from "vitest";
import mainSource from "./main.ts?raw";

/** The shell's scroll containers (styles.css: overflow auto). Vitest blanks
    CSS imports, even with ?raw, so the list is named here. */
const SCROLLING_CLASSES = ["session-tabs"];

describe("window drag regions", () => {
  it("never include an element that scrolls", () => {
    const dragTags = [...mainSource.matchAll(/<[a-z][a-z0-9]*\b[^>]*\bdata-tauri-drag-region\b[^>]*>/gi)].map((tag) => tag[0]);
    expect(dragTags.length, "the top bar still drags the window").toBeGreaterThan(0);
    for (const tag of dragTags) {
      const classes = tag.match(/\bclass="([^"]*)"/)?.[1].split(/\s+/) ?? [];
      expect(classes.filter((name) => SCROLLING_CLASSES.includes(name)), tag).toEqual([]);
    }
    const strip = mainSource.match(/<div[^>]*\bid="session-tabs"[^>]*>/)?.[0] ?? "";
    expect(strip, "the member tab strip is in the shell").toContain('class="session-tabs"');
    expect(strip).not.toContain("data-tauri-drag-region");
  });
});
