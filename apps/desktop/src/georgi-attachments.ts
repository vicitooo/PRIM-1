/* Georgi themes: images in the room composer and feed. An image is saved by
   the backend under <runtime>/attachments/<room>/ as soon as it is added, and
   shared with members as its file path at the end of the message text, so
   every harness can open it and nothing about room delivery changes. */

export const MAX_IMAGE_BYTES = 10 * 1024 * 1024;
const IMAGE_TYPES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);
const EXTENSION: Record<string, string> = {
  "image/png": "png",
  "image/jpeg": "jpg",
  "image/gif": "gif",
  "image/webp": "webp",
};

/** The marker line that starts the shared paths at the end of a message. */
export const ATTACHMENT_HEADER = "[Attached images — open these files]";

export interface SavedAttachment {
  path: string;
  name: string;
  bytes: number;
}

type Save = (roomId: string, fileName: string, dataBase64: string) => Promise<SavedAttachment>;
type Read = (path: string) => Promise<string>;

interface Pending {
  id: number;
  file: File;
  name: string;
  size: number;
  preview: string | null;
  state: "saving" | "ready" | "error";
  message: string;
  path?: string;
}

export function formatBytes(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${bytes} B`;
}

function readAsDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}

/** Split a message into its text and the attachment paths at its end. */
export function splitAttachments(body: string): { text: string; paths: string[] } {
  const at = body.lastIndexOf(ATTACHMENT_HEADER);
  if (at < 0) return { text: body, paths: [] };
  const paths = body
    .slice(at + ATTACHMENT_HEADER.length)
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => /\.(png|jpe?g|gif|webp)$/i.test(line));
  return { text: body.slice(0, at).trimEnd(), paths };
}

export function joinAttachments(text: string, paths: string[]): string {
  if (paths.length === 0) return text;
  return `${text.trimEnd()}\n\n${ATTACHMENT_HEADER}\n${paths.join("\n")}`;
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, className?: string, text?: string): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

/* ── Lightbox ── */

let lightbox: HTMLDialogElement | null = null;

export function openLightbox(src: string, name: string, path: string): void {
  if (!lightbox) {
    lightbox = el("dialog", "g-lightbox g-only");
    lightbox.addEventListener("click", (event) => {
      if (event.target === lightbox) lightbox?.close();
    });
    document.body.append(lightbox);
  }
  const image = el("img", "g-lightbox-img");
  image.src = src;
  image.alt = name;
  const bar = el("div", "g-lightbox-bar");
  const where = el("span", "g-lightbox-path", path ? `Shared as file: ${path}` : name);
  const copy = el("button", "g-btn g-btn-secondary", "Copy path");
  copy.type = "button";
  copy.hidden = !path;
  copy.addEventListener("click", () => {
    void navigator.clipboard.writeText(path).then(() => {
      copy.textContent = "Copied";
    });
  });
  const close = el("button", "g-btn g-btn-ghost", "Close");
  close.type = "button";
  close.addEventListener("click", () => lightbox?.close());
  bar.append(where, copy, close);
  lightbox.replaceChildren(image, bar);
  lightbox.showModal();
}

/* ── Feed thumbnails ── */

const thumbCache = new Map<string, Promise<string>>();

export function attachmentGrid(paths: string[], read: Read): HTMLElement {
  const grid = el("div", "g-att-grid g-only");
  for (const path of paths) {
    const name = path.split(/[\\/]/).pop() ?? path;
    const tile = el("button", "g-att-tile");
    tile.type = "button";
    tile.title = path;
    const image = el("img", "g-att-img");
    image.alt = name;
    image.decoding = "async";
    const label = el("span", "g-att-name", name.replace(/^\d+-/, ""));
    tile.append(image, label);
    let cached = thumbCache.get(path);
    if (!cached) {
      cached = read(path);
      thumbCache.set(path, cached);
      cached.catch(() => thumbCache.delete(path));
    }
    cached
      .then((src) => {
        image.src = src;
        tile.addEventListener("click", () => openLightbox(src, name, path));
      })
      .catch((error) => {
        tile.dataset.broken = "true";
        label.textContent = `${name} · ${String(error)}`;
      });
    grid.append(tile);
  }
  if (paths.length) {
    const hint = el("span", "g-att-hint", "Shared with agents as file paths");
    const wrap = el("div", "g-att-wrap g-only");
    wrap.append(grid, hint);
    return wrap;
  }
  return grid;
}

/* ── Composer ── */

export class ComposerAttachments {
  private items: Pending[] = [];
  private next = 1;

  constructor(
    private readonly strip: HTMLElement,
    private readonly roomId: () => string | null,
    private readonly save: Save,
    private readonly changed: () => void,
  ) {}

  get count(): number {
    return this.items.length;
  }

  get blocking(): string | null {
    if (this.items.some((item) => item.state === "error")) return "Remove the images marked in red before sending.";
    if (this.items.some((item) => item.state === "saving")) return "Wait for the images to finish saving.";
    return null;
  }

  get paths(): string[] {
    return this.items.filter((item) => item.state === "ready" && item.path).map((item) => item.path!);
  }

  clear(): void {
    this.items = [];
    this.render();
  }

  /** Files from a paste, a drop or the picker; non-images are ignored. */
  add(files: Iterable<File>): number {
    let added = 0;
    for (const file of files) {
      if (!file.type.startsWith("image/") && !/\.(png|jpe?g|gif|webp|heic|bmp|tiff?)$/i.test(file.name)) continue;
      added += 1;
      const extension = EXTENSION[file.type] ?? (file.name.split(".").pop() ?? "png").toLowerCase();
      const name = file.name && file.name !== "image.png" ? file.name : `pasted-${new Date().toISOString().slice(11, 19).replace(/:/g, "")}.${extension}`;
      const item: Pending = { id: this.next++, file, name, size: file.size, preview: null, state: "saving", message: "Saving…" };
      if (!IMAGE_TYPES.has(file.type)) {
        item.state = "error";
        item.message = `${extension.toUpperCase()} isn't supported · use PNG, JPG, GIF, WebP`;
      } else if (file.size > MAX_IMAGE_BYTES) {
        item.state = "error";
        item.message = `${formatBytes(file.size)} · over the 10 MB limit`;
      }
      this.items.push(item);
      void this.load(item);
    }
    this.render();
    return added;
  }

  private async load(item: Pending): Promise<void> {
    try {
      const url = await readAsDataUrl(item.file);
      item.preview = url;
      this.render();
      if (item.state === "error") return;
      const roomId = this.roomId();
      if (!roomId) throw new Error("open a room first");
      const saved = await this.save(roomId, item.name, url.slice(url.indexOf(",") + 1));
      item.path = saved.path;
      item.state = "ready";
      item.message = formatBytes(item.size);
    } catch (error) {
      item.state = "error";
      item.message = `Not saved: ${error instanceof Error ? error.message : String(error)}`;
    }
    if (this.items.includes(item)) this.render();
  }

  private remove(id: number): void {
    this.items = this.items.filter((item) => item.id !== id);
    this.render();
  }

  private render(): void {
    this.strip.hidden = this.items.length === 0;
    const chips = this.items.map((item) => {
      const chip = el("div", "g-att-chip");
      chip.dataset.state = item.state;
      const thumb = el("button", "g-att-thumb");
      thumb.type = "button";
      thumb.title = item.state === "error" ? item.message : "Preview";
      if (item.preview) {
        const image = el("img");
        image.src = item.preview;
        image.alt = item.name;
        thumb.append(image);
        thumb.addEventListener("click", () => openLightbox(item.preview!, item.name, item.path ?? ""));
      }
      if (item.state === "error") thumb.append(el("span", "g-att-bang", "!"));
      const text = el("span", "g-att-text");
      text.append(el("span", "g-att-chip-name", item.name), el("span", "g-att-chip-meta", item.message));
      if (item.state === "saving") text.append(el("span", "g-att-progress"));
      const remove = el("button", "g-att-remove", "×");
      remove.type = "button";
      remove.title = `Remove ${item.name}`;
      remove.addEventListener("click", () => this.remove(item.id));
      chip.append(thumb, text, remove);
      return chip;
    });
    this.strip.replaceChildren(...chips);
    this.changed();
  }
}
