/** The read-only Tasks panel, minus the DOM: queries, parsing, grouping and
    wording. Agents keep tasks through their pane `tasks` tool; the desktop
    reads through `tasks_query`, whose backend schema has no mutation type. */

export type TaskState = "open" | "doing" | "blocked" | "review" | "done" | "dropped";

/** Work in motion first, finished work last. */
export const TASK_STATE_ORDER: readonly TaskState[] = ["doing", "review", "blocked", "open", "done", "dropped"];

export const TASK_STATE_LABELS: Readonly<Record<TaskState, string>> = {
  doing: "Doing",
  review: "In review",
  blocked: "Blocked",
  open: "Open",
  done: "Done",
  dropped: "Dropped",
};

/** Finished groups start closed; their counts stay visible. */
export const TASK_STATES_START_CLOSED: ReadonlySet<TaskState> = new Set<TaskState>(["done", "dropped"]);

/** The store's largest page; each state group pages on its own. */
export const TASK_PAGE_SIZE = 200;

export const TASKS_READ_ONLY_NOTE = "Agents keep this list. Ask them to change it.";

export interface TaskSummary {
  id: string;
  title: string;
  owner: string | null;
  state: TaskState;
  updatedAt: string;
}

export interface TaskLogEntry {
  at: string;
  by: string;
  what: string;
  note: string | null;
}

export interface TaskDetail {
  id: string;
  title: string;
  request: string;
  source: string;
  outcome: string;
  state: TaskState;
  owner: string | null;
  reviewer: string | null;
  parent: string | null;
  evidence: string[];
  createdAt: string;
  updatedAt: string;
  /** Oldest first, as stored: the newest entry is last. */
  log: TaskLogEntry[];
}

export interface UnreadableTaskFile {
  file: string;
  error: string;
}

/** `get_task_folder` / `set_task_folder`. */
export interface TaskFolderStatus {
  folder: string | null;
  source: "setting" | "default";
  status: "open" | "error";
  error?: string;
}

/** One state's page. */
export interface TaskGroupPage {
  state: TaskState;
  items: TaskSummary[];
  /** Tasks in this state, before paging. */
  total: number;
  offset: number;
  /** Set when this state's request failed, in the backend's words; the
      page then has no items and the other states still show. */
  error?: string;
}

export const TASK_LIST_QUERY = `query TaskList($state: TaskState!, $offset: Int!) {
  tasks(state: $state, offset: $offset, limit: ${TASK_PAGE_SIZE}) {
    total
    items { id title owner state updatedAt }
    unreadable { file error }
  }
}`;

export const TASK_DETAIL_QUERY = `query TaskDetail($id: String!) {
  task(id: $id) {
    id title request source outcome state owner reviewer parent evidence createdAt updatedAt
    log { at by what note }
  }
}`;

interface GraphQlResult {
  data?: Record<string, unknown> | null;
  errors?: { message?: unknown }[];
}

/** A rejected command carries the backend's plain string; a thrown Error its message. */
export function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** The first GraphQL error message, or null when there is none. */
export function graphQlError(result: unknown): string | null {
  const errors = (result as GraphQlResult | null)?.errors;
  if (!Array.isArray(errors) || errors.length === 0) return null;
  const message = errors[0]?.message;
  return typeof message === "string" && message ? message : "The task request failed.";
}

function isState(value: unknown): value is TaskState {
  return typeof value === "string" && (TASK_STATE_ORDER as readonly string[]).includes(value);
}

/** One state's page and the folder's unreadable files. Throws with the
    backend's own words when the request failed. */
export function parseTaskPage(
  state: TaskState,
  offset: number,
  result: unknown,
): { page: TaskGroupPage; unreadable: UnreadableTaskFile[] } {
  const error = graphQlError(result);
  if (error !== null) throw new Error(error);
  const tasks = (result as GraphQlResult).data?.tasks as
    | { total?: unknown; items?: unknown; unreadable?: unknown }
    | null
    | undefined;
  if (!tasks || typeof tasks.total !== "number" || !Array.isArray(tasks.items)) {
    throw new Error("The task list came back in an unexpected shape.");
  }
  const items = (tasks.items as Partial<TaskSummary>[]).map((item) => {
    if (typeof item?.id !== "string" || typeof item.title !== "string" || !isState(item.state)) {
      throw new Error("The task list came back in an unexpected shape.");
    }
    return {
      id: item.id,
      title: item.title,
      owner: typeof item.owner === "string" ? item.owner : null,
      state: item.state,
      updatedAt: typeof item.updatedAt === "string" ? item.updatedAt : "",
    };
  });
  const unreadable = Array.isArray(tasks.unreadable)
    ? (tasks.unreadable as Partial<UnreadableTaskFile>[]).map((file) => ({
        file: String(file?.file ?? ""),
        error: String(file?.error ?? ""),
      }))
    : [];
  return { page: { state, items, total: tasks.total, offset }, unreadable };
}

/** The task in a detail response. Throws with the backend's own words. */
export function parseTaskDetail(result: unknown): TaskDetail {
  const error = graphQlError(result);
  if (error !== null) throw new Error(error);
  const task = (result as GraphQlResult).data?.task as TaskDetail | null | undefined;
  if (!task || typeof task.id !== "string" || typeof task.request !== "string" || !Array.isArray(task.log)) {
    throw new Error("The task came back in an unexpected shape.");
  }
  return task;
}

/** Pages in display order; states with no tasks are left out, and a state
    that could not be read stays in with its error. */
export function orderedGroups(pages: Iterable<TaskGroupPage>): TaskGroupPage[] {
  const byState = new Map<TaskState, TaskGroupPage>();
  for (const page of pages) byState.set(page.state, page);
  return TASK_STATE_ORDER.flatMap((state) => {
    const page = byState.get(state);
    return page && (page.total > 0 || page.error !== undefined) ? [page] : [];
  });
}

/** The same file reported by several pages is listed once. */
export function mergeUnreadable(lists: Iterable<readonly UnreadableTaskFile[]>): UnreadableTaskFile[] {
  const byFile = new Map<string, UnreadableTaskFile>();
  for (const list of lists) {
    for (const file of list) if (!byFile.has(file.file)) byFile.set(file.file, file);
  }
  return Array.from(byFile.values()).sort((a, b) => a.file.localeCompare(b.file, undefined, { numeric: true }));
}

/** "Doing · 3", or "Doing · could not be read". */
export function groupHeading(page: TaskGroupPage): string {
  return `${TASK_STATE_LABELS[page.state]} · ${page.error !== undefined ? "could not be read" : page.total}`;
}

/** "Showing 201–340 of 340", or null when one page holds the whole group
    or the group could not be read. */
export function pageRangeLabel(page: TaskGroupPage): string | null {
  if (page.error !== undefined) return null;
  if (page.offset === 0 && page.total <= TASK_PAGE_SIZE) return null;
  if (page.items.length === 0) return `None shown of ${page.total}`;
  return `Showing ${page.offset + 1}–${page.offset + page.items.length} of ${page.total}`;
}

export function previousOffset(page: TaskGroupPage): number | null {
  return page.offset > 0 ? Math.max(0, page.offset - TASK_PAGE_SIZE) : null;
}

export function nextOffset(page: TaskGroupPage): number | null {
  const next = page.offset + TASK_PAGE_SIZE;
  return next < page.total ? next : null;
}

const SUMMARY_FILES = 5;

/** "2 task files can't be read: T-3.json, T-9.json", or null for none. */
export function unreadableSummary(files: readonly UnreadableTaskFile[]): string | null {
  if (files.length === 0) return null;
  const names = files.slice(0, SUMMARY_FILES).map((file) => file.file).join(", ");
  const more = files.length > SUMMARY_FILES ? `, and ${files.length - SUMMARY_FILES} more` : "";
  const subject = files.length === 1 ? "1 task file can't be read" : `${files.length} task files can't be read`;
  return `${subject}: ${names}${more}`;
}

/** Shown under the unreadable files: the store never rewrites them. */
export const UNREADABLE_ADVICE =
  "PRIM-1 leaves these files as they are. Ask an agent to repair them, or restore them from git.";

const pad = (value: number): string => String(value).padStart(2, "0");

/** Local "2026-10-14 20:15"; a value that is not a time is shown as stored. */
export function formatTaskTime(iso: string): string {
  const time = Date.parse(iso);
  if (!iso || Number.isNaN(time)) return iso;
  const date = new Date(time);
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** Settings → Task folder: where the folder came from, or why it is closed. */
export function taskFolderStateText(status: TaskFolderStatus): string {
  if (status.status === "error") return status.error || "The task folder is not open.";
  return status.source === "default"
    ? "Open. This is the default: no folder has been chosen yet."
    : "Open. Chosen in Settings.";
}

/** Tasks panel: what to say when tasks cannot be read at all. */
export function taskFolderProblem(status: TaskFolderStatus): string | null {
  if (status.status === "open") return null;
  return status.error || "The task folder is not open.";
}
