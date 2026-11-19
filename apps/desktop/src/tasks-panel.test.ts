import { describe, expect, it } from "vitest";
import {
  TASK_DETAIL_QUERY,
  TASK_LIST_QUERY,
  TASK_PAGE_SIZE,
  TASK_STATE_ORDER,
  errorText,
  formatTaskTime,
  graphQlError,
  groupHeading,
  mergeUnreadable,
  nextOffset,
  orderedGroups,
  pageRangeLabel,
  parseTaskDetail,
  parseTaskPage,
  previousOffset,
  taskFolderProblem,
  taskFolderStateText,
  unreadableSummary,
  type TaskGroupPage,
  type TaskState,
  type TaskSummary,
} from "./tasks-panel";

const task = (id: string, state: TaskState): TaskSummary => ({
  id, title: `title ${id}`, owner: null, state, updatedAt: "2026-10-14T20:15:00Z",
});
const page = (state: TaskState, total: number, offset = 0): TaskGroupPage => ({
  state,
  total,
  offset,
  items: Array.from({ length: Math.max(0, Math.min(TASK_PAGE_SIZE, total - offset)) }, (_, i) => task(`T-${offset + i + 1}`, state)),
});

describe("tasks panel", () => {
  it("orders groups doing, review, blocked, open, done, dropped and drops empty ones", () => {
    expect(TASK_STATE_ORDER).toEqual(["doing", "review", "blocked", "open", "done", "dropped"]);
    const shuffled = [page("dropped", 1), page("open", 2), page("done", 0), page("doing", 3), page("blocked", 1), page("review", 4)];
    expect(orderedGroups(shuffled).map((group) => group.state)).toEqual(["doing", "review", "blocked", "open", "dropped"]);
    expect(orderedGroups([])).toEqual([]);
    expect(groupHeading(page("review", 4))).toBe("In review · 4");
  });

  it("keeps a group that could not be read, with its own error and no pager", () => {
    const failed: TaskGroupPage = { state: "blocked", total: 0, offset: 200, items: [], error: "task query worker failed" };
    expect(orderedGroups([page("doing", 2), failed, page("open", 0)]).map((group) => group.state)).toEqual(["doing", "blocked"]);
    expect(groupHeading(failed)).toBe("Blocked · could not be read");
    expect(pageRangeLabel(failed)).toBeNull();
  });

  it("pages each group by 200 and never hides the rest silently", () => {
    expect(pageRangeLabel(page("open", 200))).toBeNull();
    const first = page("open", 340);
    expect(pageRangeLabel(first)).toBe("Showing 1–200 of 340");
    expect([previousOffset(first), nextOffset(first)]).toEqual([null, 200]);
    const second = page("open", 340, 200);
    expect(pageRangeLabel(second)).toBe("Showing 201–340 of 340");
    expect([previousOffset(second), nextOffset(second)]).toEqual([0, null]);
    // A page past the end after tasks moved away still says so.
    expect(pageRangeLabel({ state: "open", total: 150, offset: 200, items: [] })).toBe("None shown of 150");
    expect(TASK_LIST_QUERY).toContain(`limit: ${TASK_PAGE_SIZE}`);
  });

  it("parses a page and its unreadable files, and surfaces backend errors in their own words", () => {
    const result = {
      data: {
        tasks: {
          total: 1,
          items: [{ id: "T-1", title: "first", owner: "Claude", state: "doing", updatedAt: "2026-10-14T20:15:00Z" }],
          unreadable: [{ file: "T-9.json", error: "not JSON" }],
        },
      },
    };
    expect(parseTaskPage("doing", 0, result)).toEqual({
      page: { state: "doing", offset: 0, total: 1, items: [{ id: "T-1", title: "first", owner: "Claude", state: "doing", updatedAt: "2026-10-14T20:15:00Z" }] },
      unreadable: [{ file: "T-9.json", error: "not JSON" }],
    });
    const failed = { errors: [{ message: "No task folder is set. Choose one in PRIM-1 Settings → Task folder.", extensions: { code: "NO_TASK_FOLDER" } }] };
    expect(graphQlError(failed)).toContain("No task folder is set");
    expect(() => parseTaskPage("open", 0, failed)).toThrow("No task folder is set");
    expect(() => parseTaskPage("open", 0, { data: { tasks: null } })).toThrow("unexpected shape");
    expect(graphQlError({ data: {} })).toBeNull();
    expect(errorText(new Error("plain words"))).toBe("plain words");
    const inUse = "That folder is still in use (by another PRIM-1, or by a request still finishing here). Try again in a moment.";
    expect(errorText(inUse)).toBe(inUse);
  });

  it("keeps the request verbatim, line breaks and spacing included", () => {
    const request = "  First line\r\n\tsecond line\n\n  trailing  ";
    const detail = parseTaskDetail({
      data: {
        task: {
          id: "T-2", title: "t", request, source: "room 444", outcome: "", state: "open", owner: null, reviewer: null,
          parent: "T-1", evidence: ["commit abc"], createdAt: "x", updatedAt: "y",
          log: [{ at: "a", by: "Claude", what: "created", note: null }, { at: "b", by: "Codex", what: "state open -> doing", note: "mine" }],
        },
      },
    });
    expect(detail.request).toBe(request);
    expect(detail.log.map((entry) => entry.at)).toEqual(["a", "b"]);
    expect(() => parseTaskDetail({ errors: [{ message: "task T-404 does not exist" }], data: { task: null } })).toThrow("T-404");
    expect(TASK_DETAIL_QUERY).toContain("request");
    expect(TASK_DETAIL_QUERY).not.toMatch(/mutation/i);
  });

  it("summarizes unreadable files plainly, each file once", () => {
    expect(unreadableSummary([])).toBeNull();
    expect(unreadableSummary([{ file: "T-3.json", error: "e" }])).toBe("1 task file can't be read: T-3.json");
    const merged = mergeUnreadable([
      [{ file: "T-9.json", error: "a" }, { file: "T-3.json", error: "b" }],
      [{ file: "T-3.json", error: "b" }],
    ]);
    expect(merged.map((file) => file.file)).toEqual(["T-3.json", "T-9.json"]);
    expect(unreadableSummary(merged)).toBe("2 task files can't be read: T-3.json, T-9.json");
    const many = Array.from({ length: 7 }, (_, i) => ({ file: `T-${i + 1}.json`, error: "e" }));
    expect(unreadableSummary(many)).toBe("7 task files can't be read: T-1.json, T-2.json, T-3.json, T-4.json, T-5.json, and 2 more");
    expect(mergeUnreadable([[{ file: "T-10.json", error: "" }, { file: "T-2.json", error: "" }]]).map((file) => file.file)).toEqual(["T-2.json", "T-10.json"]);
  });

  it("formats times in local time and shows anything else as stored", () => {
    const local = new Date(2026, 9, 14, 20, 5, 30);
    expect(formatTaskTime(local.toISOString())).toBe("2026-10-14 20:05");
    expect(formatTaskTime("not a time")).toBe("not a time");
    expect(formatTaskTime("")).toBe("");
  });

  it("describes the folder for Settings and the panel", () => {
    expect(taskFolderStateText({ folder: "D:\\v\\tasks", source: "default", status: "open" })).toContain("default");
    expect(taskFolderStateText({ folder: "D:\\t", source: "setting", status: "open" })).toBe("Open. Chosen in Settings.");
    const locked = { folder: "D:\\t", source: "setting" as const, status: "error" as const, error: "already in use by another PRIM-1" };
    expect(taskFolderStateText(locked)).toBe(locked.error);
    expect(taskFolderProblem(locked)).toBe(locked.error);
    expect(taskFolderProblem({ folder: "D:\\t", source: "setting", status: "open" })).toBeNull();
    expect(taskFolderProblem({ folder: null, source: "setting", status: "error" })).toBe("The task folder is not open.");
  });
});
