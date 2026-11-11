import { describe, expect, it } from "vitest";
import { parseInline, parseMarkdown } from "./georgi-markdown";

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

  it("reads quotes and pipe tables", () => {
    const [quote, table] = parseMarkdown("> Operator: hi\n\n| A | B |\n|---|---|\n| 1 | 2 |");
    expect(quote).toMatchObject({ kind: "quote", blocks: [{ kind: "paragraph" }] });
    expect(table).toMatchObject({ kind: "table", header: [[{ text: "A" }], [{ text: "B" }]], rows: [[[{ text: "1" }], [{ text: "2" }]]] });
  });
});
