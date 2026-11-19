import { describe, expect, it, vi } from "vitest";
import { RendererOutputCredit } from "./output-credit";
import { createRunEventGateState, handleRuntimeEvent, completeInitialRunEventReconciliation,
  type RuntimeEventContext } from "./runtime-events";
import { InitialTerminalBanner } from "./terminal-banner";
import { SgrColourFilter } from "./sgr-filter";
import { sessionReadiness } from "./work-context";
import type { OutputCredit, RuntimeEvent, SessionSnapshot } from "./types";

const credit: OutputCredit = { epoch: "epoch", run: { session_id: "session", run_id: "run", generation: 1 }, tickets: ["ticket"] };
const output = (): Extract<RuntimeEvent, { event: "session_output" }> => ({
  event: "session_output", identity: { ...credit.run, sequence: 1 }, session: "Terminal",
  chunk: "tail", synthetic: false, timestamp: "now", output_credit: credit,
});
function context(initialized = true) {
  const finish = vi.fn();
  const consumer = new RendererOutputCredit(finish);
  consumer.epoch = "epoch";
  const snapshot: SessionSnapshot = {
    session_id: "session", run_id: "run", generation: 1, run_event_sequence: 0,
    alias: "terminal", label: "Terminal", driver: "generic_terminal", permission_profile: "normal",
    launch_choices: { model: null, effort: null, brief: { mode: "none" } },
    recovery: null, recovery_history: [], lifecycle_state: "ready", working_dir: "C:\\fixture",
    process_id: 42, running: true, output_draining: false, resume_available: false,
    was_running_at_shutdown: false, last_activity_at: null, last_error: null, last_error_kind: null,
  };
  const gate = createRunEventGateState(); gate.initialized = initialized;
  const ctx: RuntimeEventContext = {
    snapshotById: new Map([[snapshot.session_id, snapshot]]), pendingOutput: new Map(), runEventGate: gate,
    writeSystem: vi.fn(), refreshSnapshotFromEvent: vi.fn(), applyPaneSnapshot: vi.fn(),
    setControlEndpoint: vi.fn(), writeToPane: () => false,
    admitOutput: (credit) => consumer.admit(credit),
    settleOutput: (credit, disposition) => consumer.settle(credit, disposition),
  };
  return { finish, consumer, ctx };
}

describe("output parser credit", () => {
  it("holds admission through receipt and duplicate event until actual parser callback", () => {
    const { ctx, finish } = context();
    let parsed: (() => void) | undefined;
    const write = vi.fn((_session: string, _chunk: string, done?: () => void) => { parsed = done; return true; });
    ctx.writeToPane = write;
    handleRuntimeEvent(output(), ctx);
    handleRuntimeEvent(output(), ctx); // duplicate wire event, not internal bootstrap replay
    expect(write).toHaveBeenCalledTimes(1);
    expect(finish).not.toHaveBeenCalled();
    parsed!(); parsed!();
    expect(finish).toHaveBeenCalledTimes(1);
    expect(finish).toHaveBeenCalledWith(credit, "parsed");
  });

  it("retains credit through bootstrap then unattached buffering", () => {
    const { ctx, finish } = context(false);
    handleRuntimeEvent(output(), ctx);
    expect(finish).not.toHaveBeenCalled();
    completeInitialRunEventReconciliation(ctx);
    const pending = ctx.pendingOutput.get("session")!;
    expect(pending.chunks).toEqual(["tail"]);
    expect(pending.credits).toEqual([credit]);
    expect(finish).not.toHaveBeenCalled();
    ctx.settleOutput!(pending.credits![0]!, "parsed");
    expect(finish).toHaveBeenCalledTimes(1);
  });

  it("retains credited output while the existing bootstrap control window overflows", () => {
    const { ctx, finish } = context(false);
    handleRuntimeEvent(output(), ctx);
    for (let sequence = 2; sequence < 530; sequence++) {
      handleRuntimeEvent({ event: "session_work_state", identity: { ...credit.run, sequence },
        session: "Terminal", state: "thinking", detail: null, previous_state: null, timestamp: "now" }, ctx);
    }
    expect(ctx.runEventGate.queued).toHaveLength(512);
    expect(ctx.runEventGate.queued[0]).toEqual(output());
    expect(finish).not.toHaveBeenCalled();
  });

  it("counts disposal separately and ignores a late parser callback", () => {
    const { ctx, finish, consumer } = context();
    let parsed: (() => void) | undefined;
    ctx.writeToPane = (_session, _chunk, done) => { parsed = done; return true; };
    handleRuntimeEvent(output(), ctx);
    consumer.disposeSession("session");
    parsed!();
    expect(finish.mock.calls).toEqual([[credit, "pane_disposed"]]);
  });

  it("rejects stale epoch and records synchronous write failure without parsed ACK", () => {
    const { ctx, finish, consumer } = context();
    consumer.epoch = "replacement";
    handleRuntimeEvent(output(), ctx);
    expect(finish).toHaveBeenLastCalledWith(credit, "stale_epoch");
    consumer.epoch = "epoch";
    ctx.writeToPane = () => { throw new Error("write failed"); };
    handleRuntimeEvent(output(), ctx);
    expect(finish).toHaveBeenLastCalledWith(credit, "write_failed");
    expect(finish.mock.calls.some((call) => call[1] === "parsed")).toBe(false);
  });

  it("empty unicolor output still completes through the public terminal write callback", () => {
    const parsed = vi.fn();
    let terminalCallback: (() => void) | undefined;
    const writer = vi.fn((_chunk: string, callback?: () => void) => { terminalCallback = callback; });
    const banner = new InitialTerminalBanner(vi.fn(), writer);
    const filter = new SgrColourFilter();
    banner.forwardOutput(filter.apply("\x1b[31m"), parsed);
    expect(writer).toHaveBeenCalledWith("", parsed);
    expect(parsed).not.toHaveBeenCalled();
    terminalCallback!();
    expect(parsed).toHaveBeenCalledOnce();
    // A split sequence retains at most the filter's existing bounded carry;
    // it does not require holding a whole producer ticket for the next chunk.
    banner.forwardOutput(filter.apply("\x1b["), parsed);
    terminalCallback!();
    expect(filter.apply("31mtext")).toBe("text");
  });

  it("presents draining without closing the identity needed by late output", () => {
    const { ctx } = context();
    const idle = { ...ctx.snapshotById.get("session")!, lifecycle_state: "idle" as const };
    ctx.snapshotById.set("session", idle);
    expect(sessionReadiness(idle, false).kind).toBe("ready");
    handleRuntimeEvent({ event: "session_output_draining", identity: { ...credit.run, sequence: 1 },
      session: "Terminal", timestamp: "now" }, ctx);
    expect(ctx.snapshotById.get("session")?.output_draining).toBe(true);
    expect(ctx.snapshotById.get("session")?.run_id).toBe("run");
    expect(sessionReadiness(ctx.snapshotById.get("session"), false).kind).toBe("blocked");
    expect(ctx.applyPaneSnapshot).toHaveBeenCalledWith("session", expect.objectContaining({
      lifecycle_state: "idle", output_draining: true, running: true,
    }));
    handleRuntimeEvent({ ...output(), identity: { ...credit.run, sequence: 2 } }, ctx);
    expect(ctx.pendingOutput.get("session")?.chunks).toEqual(["tail"]);
  });
});
