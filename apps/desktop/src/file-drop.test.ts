import { describe, expect, it } from "vitest";
import { droppedPathsText, quoteDroppedPath } from "./file-drop";

describe("file drop", () => {
  it("escapes spaces and brackets", () => {
    expect(quoteDroppedPath("/tmp/Screenshot 2026 (1).png")).toBe("/tmp/Screenshot\\ 2026\\ \\(1\\).png");
  });
  it("keeps Cyrillic names readable", () => {
    expect(quoteDroppedPath("/tmp/Снимка.png")).toBe("/tmp/Снимка.png");
  });
  it("joins several paths and ends with a space", () => {
    expect(droppedPathsText(["/a.png", "/b.png"])).toBe("/a.png /b.png ");
  });
  it("drops empty and multi-line paths", () => {
    expect(droppedPathsText(["", "/a\nb.png"])).toBe("");
  });
});
