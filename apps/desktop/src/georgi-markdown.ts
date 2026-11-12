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
  /** A message meant to be copied and sent on: a paragraph written in
      quotation marks ("Sam, top priority…"), shown in a box with Copy. */
  | { kind: "copy"; text: string }
  | { kind: "list"; ordered: boolean; start: number; items: ListItem[] }
  | { kind: "quote"; blocks: Block[] }
  | { kind: "table"; header: Inline[][]; rows: Inline[][][] }
  | { kind: "rule" };

/* ── Inline ── */

/* The last alternative is a bare file path (`evidence/run/summary.json`,
   `C:\work\a.ts`, `src/main.ts:42`): agents often write plain text, and a
   path reads best as a code chip. It needs a folder separator and an
   extension that starts with a letter, so "1.54/0.76" or "and/or" stay text. */
const INLINE = /(`+)([\s\S]*?[^`])\1(?!`)|\*\*([\s\S]+?)\*\*|__([\s\S]+?)__|\*([^*\s][^*]*?)\*|(?<![\w])_([^_\s][^_]*?)_(?![\w])|\[([^\]]+)\]\(([^)\s]+)\)|(https?:\/\/[^\s<>()]+[^\s<>().,;:!?'"])|(?<![\w\/\\.:-])((?:[A-Za-z]:[\\/]|~?\.{0,2}\/)?(?:[\w@.-]+[\\/])+[\w@-][\w@.-]*\.[A-Za-z][A-Za-z0-9]{0,7}(?::\d+(?::\d+)?)?)(?![\w\/\\])/g;

export function parseInline(source: string): Inline[] {
  const out: Inline[] = [];
  let last = 0;
  INLINE.lastIndex = 0;
  for (let match = INLINE.exec(source); match; match = INLINE.exec(source)) {
    if (match.index > last) out.push({ kind: "text", text: source.slice(last, match.index) });
    const [whole, , code, bold1, bold2, italic1, italic2, linkText, linkHref, url, path] = match;
    if (code !== undefined) out.push({ kind: "code", text: code.replace(/^ (.*) $/, "$1") });
    else if (bold1 !== undefined || bold2 !== undefined) out.push({ kind: "bold", children: parseInline(bold1 ?? bold2) });
    else if (italic1 !== undefined || italic2 !== undefined) out.push({ kind: "italic", children: parseInline(italic1 ?? italic2) });
    else if (linkText !== undefined) out.push({ kind: "link", text: linkText, href: linkHref });
    else if (url !== undefined) out.push({ kind: "link", text: url, href: url });
    else if (path !== undefined) out.push({ kind: "code", text: path });
    else out.push({ kind: "text", text: whole });
    last = match.index + whole.length;
    INLINE.lastIndex = last;
  }
  if (last < source.length) out.push({ kind: "text", text: source.slice(last) });
  return out;
}

/* A line that opens with a short label ("Guest: …", "Signed in: …",
   "Evidence: …") gets the label in bold, the way a person would format a
   status report. At most four words, starting with a capital; never a URL
   scheme, a time ("12:30") or a line that already starts with markup. */
const LABEL = /^([A-Z][A-Za-z0-9()&+'\/ -]{0,30}?):(\s+)(?=\S)/;

export function parseLine(source: string): Inline[] {
  const label = source.match(LABEL);
  if (!label || label[1].trim().split(/\s+/).length > 4 || /^(https?|file|ftp)$/i.test(label[1])) {
    return parseInline(source);
  }
  const rest = source.slice(label[0].length);
  return [{ kind: "bold", children: [{ kind: "text", text: `${label[1]}:` }] }, { kind: "text", text: label[2] }, ...parseInline(rest)];
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
    const quoted = quotedMessage(lines, i);
    if (quoted) {
      blocks.push({ kind: "copy", text: quoted.text });
      i = quoted.next;
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
      && !(paragraph.length > 0 && quotedMessage(lines, i))
    ) {
      paragraph.push(parseLine(lines[i].trim()));
      i += 1;
    }
    blocks.push({ kind: "paragraph", lines: paragraph });
  }
  return blocks;
}

const OPEN_QUOTE = /^["“]/;
const CLOSE_QUOTE = /["”]$/;

/* A message written out for the operator to send on: it opens with a
   quotation mark at the start of a line and closes with one at the end of
   a line (the same line or up to 40 lines later, blank lines allowed), and
   is long enough to be a message rather than a quoted phrase. */
function quotedMessage(lines: string[], from: number): { text: string; next: number } | null {
  const first = lines[from].trim();
  if (!OPEN_QUOTE.test(first)) return null;
  for (let j = from; j < lines.length && j < from + 40; j += 1) {
    const current = lines[j].trim();
    if (j > from && FENCE.test(current)) return null;
    const closes = CLOSE_QUOTE.test(current) && (j > from || current.length > 1);
    if (!closes) continue;
    const text = lines.slice(from, j + 1).join("\n").trim().slice(1, -1).trim();
    // A closing quote right after an opening one mid-text ("a" and "b") is
    // a phrase, not a message.
    if (text.length < 60 || (j === from && /["“”]/.test(text))) return null;
    return { text, next: j + 1 };
  }
  return null;
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
      items.push({ content: parseLine(match[3]), children: null });
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
      // Each written line keeps its own row with a little air between rows,
      // so a plain-text report doesn't read as one dense block.
      const p = document.createElement("p");
      for (const line of block.lines) {
        const row = document.createElement("span");
        row.className = "g-md-line";
        renderInline(row, line);
        p.append(row);
      }
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
      return copyBox(pre, block.text, "code");
    }
    case "copy": {
      const text = document.createElement("div");
      text.className = "g-md-copy-text";
      text.textContent = block.text;
      return copyBox(text, block.text, "message");
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

/** A box with a Copy button in its corner, like an editor's chat panel. */
function copyBox(content: HTMLElement, text: string, kind: "code" | "message"): HTMLElement {
  const box = document.createElement("div");
  box.className = "g-md-copy";
  box.dataset.kind = kind;
  const button = document.createElement("button");
  button.type = "button";
  button.className = "g-md-copy-btn";
  button.textContent = "Copy";
  button.title = kind === "code" ? "Copy code" : "Copy message";
  button.addEventListener("click", () => {
    void navigator.clipboard.writeText(text).then(() => {
      button.textContent = "Copied";
      button.dataset.done = "true";
      setTimeout(() => {
        button.textContent = "Copy";
        delete button.dataset.done;
      }, 1400);
    });
  });
  box.append(content, button);
  return box;
}

/** Append one line of inline Markdown (bold, code, paths) to `target`. */
export function appendInline(target: HTMLElement, source: string): void {
  renderInline(target, parseInline(source));
}

/** Replace `target`'s content with `source` rendered as Markdown. */
export function renderMarkdownInto(target: HTMLElement, source: string): void {
  target.replaceChildren(...parseMarkdown(source).map(renderBlock));
  target.classList.add("g-md");
}
