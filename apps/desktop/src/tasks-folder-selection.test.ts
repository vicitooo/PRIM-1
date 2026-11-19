/// <reference types="vite/client" />
/* The Tasks panel's folder-selection boundary, driven through the real
   functions in main.ts. main.ts builds the whole app when it is imported, so
   the functions are lifted out with TypeScript's AST (as the phase C review
   runner does), rendering is recorded, and every command answers through a
   promise the test settles in the order it chooses. */
import ts from "typescript";
import { describe, expect, it } from "vitest";
import mainSource from "./main.ts?raw";
import {
  TASK_DETAIL_QUERY,
  TASK_LIST_QUERY,
  TASK_STATE_ORDER,
  TASK_STATES_START_CLOSED,
  errorText,
  mergeUnreadable,
  parseTaskDetail,
  parseTaskPage,
  taskFolderProblem,
  type TaskDetail,
  type TaskFolderStatus,
  type TaskGroupPage,
  type TaskState,
} from "./tasks-panel";

const LIFTED_FUNCTIONS = [
  "readTasks", "resetTasksPanel", "loadTasks", "selectTask", "loadTaskDetail",
  "loadTaskFolderSetting", "useTaskFolder", "syncTaskFolderControls",
];
const LIFTED_STATE = [
  "taskGroupPages", "taskGroupOffsets", "taskGroupsOpen", "selectedTaskId",
  "tasksLoadSeq", "taskDetailSeq", "taskFolderPending", "taskFolderGeneration",
];

function liftedSource(): string {
  const ast = ts.createSourceFile("main.ts", mainSource, ts.ScriptTarget.Latest, true);
  const lifted: string[] = [];
  for (const statement of ast.statements) {
    if (ts.isFunctionDeclaration(statement) && LIFTED_FUNCTIONS.includes(statement.name?.text ?? "")) {
      lifted.push(statement.getText(ast));
    } else if (
      ts.isVariableStatement(statement)
      && statement.declarationList.declarations.some((declaration) => LIFTED_STATE.includes(declaration.name.getText(ast)))
    ) {
      lifted.push(statement.getText(ast));
    }
  }
  return ts.transpileModule(lifted.join("\n"), { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
}

interface Call {
  name: string;
  args?: { query?: string; variables?: { state?: TaskState; id?: string } };
  answered: boolean;
  resolve(value: unknown): void;
  reject(reason: unknown): void;
}

interface Lifted {
  loadTasks(): Promise<void>;
  selectTask(id: string): void;
  loadTaskDetail(id: string): Promise<void>;
  loadTaskFolderSetting(): Promise<void>;
  useTaskFolder(path: string): Promise<void>;
  pages(): Map<TaskState, TaskGroupPage>;
  pending(): boolean;
}

const folder = (name: string): TaskFolderStatus => ({ folder: name, source: "setting", status: "open" });
const listResult = (state: TaskState, title: string) => ({
  data: { tasks: { total: 1, items: [{ id: "T-1", title, owner: null, state, updatedAt: "" }], unreadable: [] } },
});
const detailResult = (title: string) => ({
  data: {
    task: {
      id: "T-1", title, request: "", source: "", outcome: "", state: "open", owner: null, reviewer: null,
      parent: null, evidence: [], createdAt: "", updatedAt: "", log: [],
    },
  },
});
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

function panel(shellView: "tasks" | "settings") {
  const calls: Call[] = [];
  const command = (name: string, args?: Call["args"]) =>
    new Promise((resolve, reject) => {
      calls.push({ name, args, answered: false, resolve, reject });
    });
  const element = () => ({
    textContent: "", value: "", title: "", hidden: false, disabled: false,
    dataset: {} as Record<string, string | undefined>,
    replaceChildren() {},
  });
  const tasksStatus = element();
  const tasksDetail = element();
  const settingsTaskFolderState = element();
  const shown = { setting: null as string | null, detail: "", result: "" };
  const env: Record<string, unknown> = {
    command, shellView, trialStatus: null,
    TASK_DETAIL_QUERY, TASK_LIST_QUERY, TASK_STATE_ORDER, TASK_STATES_START_CLOSED,
    errorText, mergeUnreadable, parseTaskDetail, parseTaskPage, taskFolderProblem,
    tasksStatus, tasksDetail, settingsTaskFolderState,
    tasksRefresh: element(), tasksFolder: element(), tasksProblem: element(), tasksProblemText: element(),
    taskFolderInput: element(), taskFolderUse: element(), taskFolderBrowse: element(),
    tasksList: { querySelectorAll: () => [] },
    renderTaskGroups() {},
    renderUnreadableTasks() {},
    renderTaskDetailMessage(message: string) {
      delete tasksDetail.dataset.taskId;
      shown.detail = message;
    },
    renderTaskDetail(task: TaskDetail) {
      tasksDetail.dataset.taskId = task.id;
      shown.detail = `${task.id}: ${task.title}`;
    },
    renderTaskFolderSetting(status: TaskFolderStatus) {
      shown.setting = status.folder;
    },
    showTaskFolderResult(message: string) {
      shown.result = message;
    },
  };
  const names = Object.keys(env);
  const build = new Function(
    ...names,
    `${liftedSource()}
return { loadTasks, selectTask, loadTaskDetail, loadTaskFolderSetting, useTaskFolder,
  pages: () => taskGroupPages, pending: () => taskFolderPending };`,
  ) as (...values: unknown[]) => Lifted;
  const api = build(...names.map((name) => env[name]));

  const open = (name: string, matches: (call: Call) => boolean = () => true) =>
    calls.filter((call) => !call.answered && call.name === name && matches(call));
  const answer = (call: Call, value: unknown, fail = false) => {
    call.answered = true;
    if (fail) call.reject(value);
    else call.resolve(value);
  };
  const listReads = () => open("tasks_query", (call) => call.args?.query === TASK_LIST_QUERY);
  const detailReads = () => open("tasks_query", (call) => call.args?.query === TASK_DETAIL_QUERY);
  const rows = () => [...api.pages().values()].flatMap((page) => page.items.map((item) => item.title));
  return { api, calls, open, answer, listReads, detailReads, rows, shown, tasksStatus, settingsTaskFolderState };
}

describe("tasks panel folder-selection boundary", () => {
  it("drops list reads that started on the old folder, whatever order they finish in", async () => {
    const p = panel("tasks");
    void p.api.loadTasks();
    p.answer(p.open("get_task_folder")[0], folder("A"));
    await settle();
    const oldReads = p.listReads();
    expect(oldReads).toHaveLength(TASK_STATE_ORDER.length);

    const selection = p.api.useTaskFolder("B");
    p.answer(p.open("set_task_folder")[0], folder("B"));
    await selection;
    await settle();
    // Settled: the status and the visible list are read again.
    for (const call of p.open("get_task_folder")) p.answer(call, folder("B"));
    await settle();
    for (const call of p.listReads().filter((call) => !oldReads.includes(call))) {
      p.answer(call, listResult(call.args!.variables!.state!, "New folder row"));
    }
    await settle();
    for (const call of oldReads) p.answer(call, listResult(call.args!.variables!.state!, "Old folder row"));
    await settle();
    expect(p.rows()).toEqual(TASK_STATE_ORDER.map(() => "New folder row"));
    expect(p.tasksStatus.textContent).toMatch(/^Read at /);
  });

  it("never lets a late folder status, success or failure, overwrite a finished selection", async () => {
    for (const late of [{ value: folder("A"), fail: false }, { value: "task folder worker failed", fail: true }]) {
      const p = panel("settings");
      void p.api.loadTaskFolderSetting();
      const oldStatus = p.open("get_task_folder")[0];
      const selection = p.api.useTaskFolder("B");
      p.answer(p.open("set_task_folder")[0], folder("B"));
      await selection;
      expect(p.shown.setting).toBe("B");
      const fresh = p.open("get_task_folder").filter((call) => call !== oldStatus);
      expect(fresh).toHaveLength(1);
      p.answer(fresh[0], folder("B"));
      await settle();
      p.answer(oldStatus, late.value, late.fail);
      await settle();
      expect(p.shown.setting).toBe("B");
      expect(p.settingsTaskFolderState.textContent).toBe("");
      // The Tasks view is not on show, so nothing reads tasks.
      expect(p.calls.filter((call) => call.name === "tasks_query")).toEqual([]);
    }
  });

  it("starts no task read while a selection is pending, and reads again after it fails", async () => {
    const p = panel("tasks");
    const selection = p.api.useTaskFolder("B");
    expect(p.api.pending()).toBe(true);
    p.api.selectTask("T-1");
    void p.api.loadTasks();
    void p.api.loadTaskFolderSetting();
    await settle();
    expect(p.calls.map((call) => call.name)).toEqual(["set_task_folder"]);
    expect(p.tasksStatus.textContent).toBe("Waiting for the task folder to change…");
    expect(p.shown.detail).toBe("Waiting for the task folder to change…");

    const refused = "That folder is still in use (by another PRIM-1, or by a request still finishing here). Try again in a moment.";
    p.answer(p.open("set_task_folder")[0], refused, true);
    await selection;
    expect(p.api.pending()).toBe(false);
    expect(p.shown.result).toBe(refused);
    // The previous folder is still selected: its status, list and the
    // task chosen during the selection are read again.
    const statuses = p.open("get_task_folder");
    expect(statuses).toHaveLength(2);
    for (const call of statuses) p.answer(call, folder("A"));
    await settle();
    expect(p.shown.setting).toBe("A");
    for (const call of p.listReads()) p.answer(call, listResult(call.args!.variables!.state!, "Folder A row"));
    await settle();
    const detail = p.detailReads();
    expect(detail.map((call) => call.args!.variables!.id)).toEqual(["T-1"]);
    p.answer(detail[0], detailResult("Folder A task"));
    await settle();
    expect(p.shown.detail).toBe("T-1: Folder A task");
  });

  it("drops a task detail read that started before the selection", async () => {
    const p = panel("settings");
    p.api.selectTask("T-1");
    const oldDetail = p.detailReads();
    expect(oldDetail).toHaveLength(1);
    const selection = p.api.useTaskFolder("B");
    p.answer(p.open("set_task_folder")[0], folder("B"));
    await selection;
    expect(p.shown.detail).toBe("Choose a task to read it.");
    p.answer(oldDetail[0], detailResult("Folder A task"));
    await settle();
    expect(p.shown.detail).toBe("Choose a task to read it.");
  });

  it("shows a failed state group's own error while the other groups render", async () => {
    const p = panel("tasks");
    void p.api.loadTasks();
    p.answer(p.open("get_task_folder")[0], folder("A"));
    await settle();
    for (const call of p.listReads()) {
      const state = call.args!.variables!.state!;
      if (state === "blocked") p.answer(call, "task query worker failed", true);
      else p.answer(call, listResult(state, `${state} row`));
    }
    await settle();
    expect(p.api.pages().get("blocked")).toMatchObject({ items: [], error: "task query worker failed" });
    expect(p.rows()).toEqual(TASK_STATE_ORDER.filter((state) => state !== "blocked").map((state) => `${state} row`));
    expect(p.tasksStatus.dataset.tone).toBe("error");
    expect(p.tasksStatus.textContent).toMatch(/^Read at .*\. 1 group could not be read\.$/);

    void p.api.loadTasks();
    p.answer(p.open("get_task_folder")[0], folder("A"));
    await settle();
    for (const call of p.listReads()) p.answer(call, { errors: [{ message: "The task store is closed." }] });
    await settle();
    expect([...p.api.pages().values()].every((page) => page.error === "The task store is closed.")).toBe(true);
    expect(p.tasksStatus.textContent).toBe("Tasks could not be read: The task store is closed.");
  });
});
