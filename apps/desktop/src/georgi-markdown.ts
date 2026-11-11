/* Georgi themes: agent messages read like a chat app, not a log. A small,
   safe Markdown subset — paragraphs, **bold**, *italic*, `code`, fenced code,
   headings, bullet and numbered lists (one nested level), quotes and pipe
   tables. Parsing is pure (testable); rendering builds DOM nodes with
   textContent only, so message text can never inject markup. Links show as
   text: a click must never navigate PRIM-1's window. */

export type Inline =
  | { kind: "text"; text: string }
  | { kind: "bold"; children: Inline[] }
  | { kind: "italic"; children: Inline[] }
  | { kind: "code"; text: string }
  | { kind: "link"; text: string; href: string };

export type ListItem = { content: Inline[]; children: Block | null };

export type Block =
  | { kind: "paragraph"; lines: Inline[][] }
  | { kind: "heading"; level: number; content: Inline[] }
  | { kind: "code"; text: string; lang: string }
  | { kind: "list"; ordered: boolean; start: number; items: ListItem[] }
  | { kind: "quote"; blocks: Block[] }
  | { kind: "table"; header: Inline[][]; rows: Inline[][][] }
  | { kind: "rule" };

/* ── Inline ── */

const INLINE = /(`+)([\s\S]*?[^`])\1(?!`)|\*\*([\s\S]+?)\*\*|__([\s\S]+?)__|\*([^*\s][^*]*?)\*|(?<![\w])_([^_\s][^_]*?)_(?![\w])|\[([^\]]+)\]\(([^)\s]+)\)|(https?:\/\/[^\s<>()]+[^\s<>().,;:!?'"])/g;

export function parseInline(source: string): Inline[] {
  const out: Inline[] = [];
  let last = 0;
  INLINE.lastIndex = 0;
  for (let match = INLINE.exec(source); match; match = INLINE.exec(source)) {
    if (match.index > last) out.push({ kind: "text", text: source.slice(last, match.index) });
    const [whole, , code, bold1, bold2, italic1, italic2, linkText, linkHref, url] = match;
    if (code !== undefined) out.push({ kind: "code", text: code.replace(/^ (.*) $/, "$1") });
    else if (bold1 !== undefined || bold2 !== undefined) out.push({ kind: "bold", children: parseInline(bold1 ?? bold2) });
    else if (italic1 !== undefined || italic2 !== undefined) out.push({ kind: "italic", children: parseInline(italic1 ?? italic2) });
    else if (linkText !== undefined) out.push({ kind: "link", text: linkText, href: linkHref });
    else if (url !== undefined) out.push({ kind: "link", text: url, href: url });
    else out.push({ kind: "text", text: whole });
    last = match.index + whole.length;
    INLINE.lastIndex = last;
  }
  if (last < source.length) out.push({ kind: "text", text: source.slice(last) });
  return out;
}

/* ── Blocks ── */

const FENCE = /^\s*(```|~~~)\s*([\w+-]*)\s*$/;
const HEADING = /^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$/;
const BULLET = /^(\s*)([-*+•])\s+(.*)$/;
const NUMBER = /^(\s*)(\d{1,9})[.)]\s+(.*)$/;
const QUOTE = /^\s{0,3}>\s?(.*)$/;
const RULE = /^\s{0,3}([-*_])(\s*\1){2,}\s*$/;
const TABLE_ROW = /^\s*\|.*\|\s*$/;
const TABLE_SEPARATOR = /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/;

function cells(line: string): Inline[][] {
  return line.trim().replace(/^\|/, "").replace(/\|$/, "").split("|").map((cell) => parseInline(cell.trim()));
}

function indentOf(text: string): number {
  return text.replace(/\t/g, "    ").length;
}

export function parseMarkdown(source: string): Block[] {
  const lines = source.replace(/\r\n?/g, "\n").split("\n");
  const blocks: Block[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) {
      i += 1;
      continue;
    }
    const fence = line.match(FENCE);
    if (fence) {
      const body: string[] = [];
      i += 1;
      while (i < lines.length && !lines[i].trim().startsWith(fence[1])) body.push(lines[i++]);
      i += 1;
      blocks.push({ kind: "code", text: body.join("\n"), lang: fence[2] });
      continue;
    }
    const heading = line.match(HEADING);
    if (heading) {
      blocks.push({ kind: "heading", level: heading[1].length, content: parseInline(heading[2]) });
      i += 1;
      continue;
    }
    if (RULE.test(line) && !BULLET.test(line.replace(/\s+$/, "") + " x")) {
      blocks.push({ kind: "rule" });
      i += 1;
      continue;
    }
    if (TABLE_ROW.test(line) && i + 1 < lines.length && TABLE_SEPARATOR.test(lines[i + 1])) {
      const header = cells(line);
      i += 2;
      const rows: Inline[][][] = [];
      while (i < lines.length && TABLE_ROW.test(lines[i])) rows.push(cells(lines[i++]));
      blocks.push({ kind: "table", header, rows });
      continue;
    }
    if (QUOTE.test(line)) {
      const inner: string[] = [];
      while (i < lines.length && QUOTE.test(lines[i])) inner.push(lines[i++].match(QUOTE)![1]);
      blocks.push({ kind: "quote", blocks: parseMarkdown(inner.join("\n")) });
      continue;
    }
    const listStart = line.match(BULLET) ?? line.match(NUMBER);
    if (listStart) {
      const list = parseList(lines, i);
      blocks.push(list.block);
      i = list.next;
      continue;
    }
    const paragraph: Inline[][] = [];
    while (
      i < lines.length && lines[i].trim() && !FENCE.test(lines[i]) && !HEADING.test(lines[i])
      && !QUOTE.test(lines[i]) && !BULLET.test(lines[i]) && !NUMBER.test(lines[i])
      && !(TABLE_ROW.test(lines[i]) && i + 1 < lines.length && TABLE_SEPARATOR.test(lines[i + 1]))
    ) {
      paragraph.push(parseInline(lines[i].trim()));
      i += 1;
    }
    blocks.push({ kind: "paragraph", lines: paragraph });
  }
  return blocks;
}

function parseList(lines: string[], from: number): { block: Block; next: number } {
  const first = (lines[from].match(BULLET) ?? lines[from].match(NUMBER))!;
  const ordered = NUMBER.test(lines[from]) && !BULLET.test(lines[from]);
  const baseIndent = indentOf(first[1]);
  const items: ListItem[] = [];
  let i = from;
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) {
      // A blank line ends the list unless the next line continues it.
      const next = lines[i + 1];
      const nextMatch = next ? next.match(ordered ? NUMBER : BULLET) : null;
      if (nextMatch && indentOf(nextMatch[1]) === baseIndent) {
        i += 1;
        continue;
      }
      break;
    }
    const match = line.match(ordered ? NUMBER : BULLET);
    if (match && indentOf(match[1]) === baseIndent) {
      items.push({ content: parseInline(match[3]), children: null });
      i += 1;
      continue;
    }
    const nested = line.match(BULLET) ?? line.match(NUMBER);
    if (nested && indentOf(nested[1]) > baseIndent && items.length) {
      const sub = parseList(lines, i);
      items[items.length - 1].children = sub.block;
      i = sub.next;
      continue;
    }
    if (indentOf(line.match(/^\s*/)![0]) > baseIndent && items.length) {
      // A wrapped continuation line of the previous item.
      const last = items[items.length - 1];
      last.content = [...last.content, { kind: "text", text: " " }, ...parseInline(line.trim())];
      i += 1;
      continue;
    }
    break;
  }
  const start = ordered ? Number(first[2]) : 1;
  return { block: { kind: "list", ordered, start, items }, next: i };
}

/* ── Rendering (DOM, textContent only) ── */

function renderInline(parent: HTMLElement, nodes: Inline[]): void {
  for (const node of nodes) {
    if (node.kind === "text") parent.append(document.createTextNode(node.text));
    else if (node.kind === "code") {
      const code = document.createElement("code");
      code.textContent = node.text;
      parent.append(code);
    } else if (node.kind === "link") {
      const link = document.createElement("span");
      link.className = "g-md-link";
      link.textContent = node.text;
      link.title = node.href;
      parent.append(link);
    } else {
      const wrap = document.createElement(node.kind === "bold" ? "strong" : "em");
      renderInline(wrap, node.children);
      parent.append(wrap);
    }
  }
}

function renderBlock(block: Block): HTMLElement {
  switch (block.kind) {
    case "paragraph": {
      const p = document.createElement("p");
      block.lines.forEach((line, index) => {
        if (index > 0) p.append(document.createElement("br"));
        renderInline(p, line);
      });
      return p;
    }
    case "heading": {
      const h = document.createElement("p");
      h.className = `g-md-h g-md-h${Math.min(block.level, 3)}`;
      renderInline(h, block.content);
      return h;
    }
    case "code": {
      const pre = document.createElement("pre");
      const code = document.createElement("code");
      code.textContent = block.text;
      pre.append(code);
      return pre;
    }
    case "list": {
      const list = document.createElement(block.ordered ? "ol" : "ul");
      if (block.ordered && block.start !== 1) (list as HTMLOListElement).start = block.start;
      for (const item of block.items) {
        const li = document.createElement("li");
        renderInline(li, item.content);
        if (item.children) li.append(renderBlock(item.children));
        list.append(li);
      }
      return list;
    }
    case "quote": {
      const quote = document.createElement("blockquote");
      for (const inner of block.blocks) quote.append(renderBlock(inner));
      return quote;
    }
    case "table": {
      const wrap = document.createElement("div");
      wrap.className = "g-md-table";
      const table = document.createElement("table");
      const head = document.createElement("thead");
      const headRow = document.createElement("tr");
      for (const cell of block.header) {
        const th = document.createElement("th");
        renderInline(th, cell);
        headRow.append(th);
      }
      head.append(headRow);
      const body = document.createElement("tbody");
      for (const row of block.rows) {
        const tr = document.createElement("tr");
        for (const cell of row) {
          const td = document.createElement("td");
          renderInline(td, cell);
          tr.append(td);
        }
        body.append(tr);
      }
      table.append(head, body);
      wrap.append(table);
      return wrap;
    }
    case "rule":
      return document.createElement("hr");
  }
}

/** Replace `target`'s content with `source` rendered as Markdown. */
export function renderMarkdownInto(target: HTMLElement, source: string): void {
  target.replaceChildren(...parseMarkdown(source).map(renderBlock));
  target.classList.add("g-md");
}
