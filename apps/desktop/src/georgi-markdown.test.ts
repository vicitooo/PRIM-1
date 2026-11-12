import { describe, expect, it } from "vitest";
import { parseInline, parseLine, parseMarkdown } from "./georgi-markdown";

describe("georgi markdown", () => {
  it("parses bold, italic, code and links inline", () => {
    expect(parseInline("a **b** *c* `d` [e](http://x) f")).toEqual([
      { kind: "text", text: "a " },
      { kind: "bold", children: [{ kind: "text", text: "b" }] },
      { kind: "text", text: " " },
      { kind: "italic", children: [{ kind: "text", text: "c" }] },
      { kind: "text", text: " " },
      { kind: "code", text: "d" },
      { kind: "text", text: " " },
      { kind: "link", text: "e", href: "http://x" },
      { kind: "text", text: " f" },
    ]);
  });

  it("leaves snake_case words and lone asterisks alone", () => {
    expect(parseInline("run_room_read and 2 * 3")).toEqual([{ kind: "text", text: "run_room_read and 2 * 3" }]);
  });

  it("keeps markup inside code spans literal", () => {
    expect(parseInline("`**not bold**`")).toEqual([{ kind: "code", text: "**not bold**" }]);
  });

  it("splits paragraphs, headings and fenced code", () => {
    const blocks = parseMarkdown("## Status\nline one\nline two\n\n```ts\nconst a = 1;\n```");
    expect(blocks.map((b) => b.kind)).toEqual(["heading", "paragraph", "code"]);
    expect(blocks[1]).toMatchObject({ kind: "paragraph", lines: [[{ text: "line one" }], [{ text: "line two" }]] });
    expect(blocks[2]).toEqual({ kind: "code", text: "const a = 1;", lang: "ts" });
  });

  it("builds bullet lists with one nested level and numbered lists", () => {
    const [bullets, numbers] = parseMarkdown("- **Added:** tests\n  - nested\n- second\n\n1. one\n2. two");
    expect(bullets).toMatchObject({ kind: "list", ordered: false });
    if (bullets.kind !== "list") throw new Error("not a list");
    expect(bullets.items).toHaveLength(2);
    expect(bullets.items[0].children).toMatchObject({ kind: "list", items: [{ content: [{ text: "nested" }] }] });
    expect(numbers).toMatchObject({ kind: "list", ordered: true, start: 1 });
  });

  it("bolds a short label that opens a plain-text line", () => {
    const [para] = parseMarkdown("Guest: first load 1.2 s\nSigned in: 2.4 s\nThus the slow part persists warm: yes");
    if (para.kind !== "paragraph") throw new Error("not a paragraph");
    expect(para.lines[0].slice(0, 2)).toEqual([{ kind: "bold", children: [{ kind: "text", text: "Guest:" }] }, { kind: "text", text: " " }]);
    expect(para.lines[1][0]).toEqual({ kind: "bold", children: [{ kind: "text", text: "Signed in:" }] });
    expect(para.lines[2]).toEqual([{ kind: "text", text: "Thus the slow part persists warm: yes" }]);
    expect(parseLine("https://x.io: down")[0]).toMatchObject({ kind: "link" });
    expect(parseLine("12:30 start")).toEqual([{ kind: "text", text: "12:30 start" }]);
  });

  it("shows file paths as code but leaves numbers and and/or alone", () => {
    expect(parseInline("see evidence/2026-01-15/sample-run/summary.json now")).toEqual([
      { kind: "text", text: "see " },
      { kind: "code", text: "evidence/2026-01-15/sample-run/summary.json" },
      { kind: "text", text: " now" },
    ]);
    expect(parseInline("open C:\\work\\app\\main.ts:42.")).toContainEqual({ kind: "code", text: "C:\\work\\app\\main.ts:42" });
    expect(parseInline("medians 1.5/0.75 s and/or 2.2/2.0")).toEqual([{ kind: "text", text: "medians 1.5/0.75 s and/or 2.2/2.0" }]);
  });

  it("turns a message written in quotation marks into a copy box", () => {
    const blocks = parseMarkdown(
      "Message for you to send Sam:\n\n\"Sam, top priority from me today: the release notes. Two sections are still empty.\n\nPlease finish them first. Thanks!\"\n\n**Dreams:** next",
    );
    expect(blocks.map((b) => b.kind)).toEqual(["paragraph", "copy", "paragraph"]);
    expect(blocks[1]).toEqual({ kind: "copy",
      text: "Sam, top priority from me today: the release notes. Two sections are still empty.\n\nPlease finish them first. Thanks!" });
    expect(parseMarkdown("Intro line\n“Short and sweet message that is long enough to count as one.”").map((b) => b.kind))
      .toEqual(["paragraph", "copy"]);
  });

  it("leaves short quoted phrases and two quoted words as text", () => {
    expect(parseMarkdown("\"ok\"").map((b) => b.kind)).toEqual(["paragraph"]);
    expect(parseMarkdown("\"run_room_read\" is the tool and \"room_post\" posts a message to everyone").map((b) => b.kind))
      .toEqual(["paragraph"]);
  });

  it("reads quotes and pipe tables", () => {
    const [quote, table] = parseMarkdown("> Operator: hi\n\n| A | B |\n|---|---|\n| 1 | 2 |");
    expect(quote).toMatchObject({ kind: "quote", blocks: [{ kind: "paragraph" }] });
    expect(table).toMatchObject({ kind: "table", header: [[{ text: "A" }], [{ text: "B" }]], rows: [[[{ text: "1" }], [{ text: "2" }]]] });
  });
});
