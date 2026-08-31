/**
 * Tests for runtime-events.ts.
 *
 * Layer 1 — focused unit tests for runtime event-contract exhaustiveness.
 */

import { describe, expect, it, vi } from "vitest";

import {
  completeInitialRunEventReconciliation,
  createRunEventGateState,
  flushRunEventGateWarnings,
  forgetRunEventSession,
  handleRuntimeEvent,
  MAX_PENDING_BYTES_PER_SESSION,
  MAX_RETIRED_RUN_CURSORS_PER_SESSION,
  reconcileSessionSnapshot,
  registerRuntimeEventsBeforeBootstrap,
  retireRendererSession,
  type PendingBuffer,
  type RuntimeEventContext,
} from "./runtime-events";
import type {
  RunEventIdentity,
  RuntimeEvent,
  SessionSnapshot,
} from "./types";

const SESSION_A = "11111111-1111-1111-1111-111111111111";
const SESSION_B = "22222222-2222-2222-2222-222222222222";
const RUN_A = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const RUN_B = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
const RUN_C = "cccccccc-cccc-cccc-cccc-cccccccccccc";

function runIdentity(
  sequence: number,
  overrides: Partial<RunEventIdentity> = {},
): RunEventIdentity {
  return {
    session_id: SESSION_A,
    run_id: RUN_A,
    generation: 7,
    sequence,
    ...overrides,
  };
}

function sessionSnapshot(
  overrides: Partial<SessionSnapshot> = {},
): SessionSnapshot {
  return {
    session_id: SESSION_A,
    alias: "codex",
    label: "Codex",
    driver: "codex",
    permission_profile: "normal",
    lifecycle_state: "ready",
    working_dir: "C:\\work",
    generation: 7,
    run_id: RUN_A,
    run_event_sequence: 1,
    process_id: 42,
    running: true,
    resume_available: false,
    was_running_at_shutdown: false,
    last_activity_at: "2026-08-10T00:00:00Z",
    last_error: null,
    last_error_kind: null,
    ...overrides,
  };
}

function sessionOutput(
  sequence: number,
  chunk: string,
  identityOverrides: Partial<RunEventIdentity> = {},
): Extract<RuntimeEvent, { event: "session_output" }> {
  return {
    event: "session_output",
    identity: runIdentity(sequence, identityOverrides),
    session: "codex",
    chunk,
    synthetic: false,
    timestamp: "2026-08-10T00:00:00Z",
  };
}

function makeContext(runEventsInitialized = true): RuntimeEventContext & {
  writeSystem: ReturnType<typeof vi.fn>;
  refreshSnapshotFromEvent: ReturnType<typeof vi.fn>;
  writeToPane: ReturnType<typeof vi.fn>;
  applyPaneSnapshot: ReturnType<typeof vi.fn>;
  setControlEndpoint: ReturnType<typeof vi.fn>;
} {
  const writeSystem = vi.fn();
  const refreshSnapshotFromEvent = vi.fn();
  const writeToPane = vi.fn(() => false);
  const applyPaneSnapshot = vi.fn();
  const setControlEndpoint = vi.fn();
  const snapshotById = new Map<string, SessionSnapshot>();
  const pendingOutput = new Map<string, PendingBuffer>();
  const runEventGate = createRunEventGateState();
  runEventGate.initialized = runEventsInitialized;
  return {
    writeSystem,
    refreshSnapshotFromEvent,
    snapshotById,
    pendingOutput,
    runEventGate,
    writeToPane,
    applyPaneSnapshot,
    setControlEndpoint,
  };
}

describe("runtime-events exhaustiveness default arm", () => {
  it("forwards room feed events to the dedicated ID-scoped room gate", () => {
    const ctx = makeContext();
    const handleRoomEvent = vi.fn();
    ctx.handleRoomEvent = handleRoomEvent;
    const event: RuntimeEvent = {
      event: "room_feed_event",
      feed_event: {
        schema_version: 1,
        room_id: "33333333-3333-3333-3333-333333333333",
        cursor: {
          epoch: "44444444-4444-4444-4444-444444444444",
          sequence: 1,
        },
        item: {
          kind: "message",
          message_id: "55555555-5555-5555-5555-555555555555",
          sender: { kind: "operator" },
          content: "room content",
          recipient_ids: [],
          membership_revision: 1,
        },
        timestamp: "2026-08-11T00:00:00Z",
      },
    };

    handleRuntimeEvent(event, ctx);

    expect(handleRoomEvent).toHaveBeenCalledOnce();
    expect(handleRoomEvent).toHaveBeenCalledWith(event);
    expect(ctx.writeSystem).not.toHaveBeenCalled();
  });

  it("every supervisor telemetry variant is handled explicitly", () => {
    const timestamp = "2026-04-22T12:00:00Z";
    const events: RuntimeEvent[] = [
      {
        event: "session_work_state",
        identity: runIdentity(2, {
          session_id: SESSION_A,
          run_id: RUN_A,
        }),
        session: "claude",
        state: "thinking",
        detail: null,
        previous_state: "idle",
        timestamp,
      },
      {
        event: "supervisor_heartbeat",
        wrapper_pid: 42,
        uptime_secs: 10,
        sessions: [],
        timestamp,
      },
      {
        event: "supervisor_alert",
        alert_type: "operator_attention",
        request_id: null,
        session: null,
        action: null,
        last_work_state: null,
        last_session_state: null,
        message: "attention",
        severity: "warn",
        timestamp,
      },
      {
        event: "route_delivery",
        request_id: "req-1",
        route_id: "route-1",
        from: "claude",
        logical_to: "codex",
        scope: "direct",
        recipient: "codex",
        recipient_index: 0,
        recipient_count: 1,
        payload_part_count: 1,
        phase: "written",
        bytes_written: 5,
        error: null,
        timestamp,
      },
      {
        event: "dispatch_attempt",
        request_id: "req-1",
        action: "route_message",
        from: "claude",
        target_session: "codex",
        target_lifecycle_state_before: "ready",
        target_work_state_before: "idle",
        target_last_activity_at: timestamp,
        last_route_from_target_at: null,
        overlap: false,
        reason: null,
        timestamp,
      },
    ];

    for (const event of events) {
      const ctx = makeContext();
      if (event.event === "session_work_state") {
        ctx.snapshotById.set(
          event.identity.session_id,
          sessionSnapshot({ alias: event.session }),
        );
      }
      handleRuntimeEvent(event, ctx);
      const messages = ctx.writeSystem.mock.calls.map((call) => call[1] as string);
      expect(messages.some((message) => message.includes("unhandled runtime event variant"))).toBe(false);
    }
  });

  it("unknown variant logs warn via default arm", () => {
    const ctx = makeContext();
    const fakeEvent = {
      event: "made_up_variant",
      payload: "whatever",
    } as unknown as RuntimeEvent;
    handleRuntimeEvent(fakeEvent, ctx);
    expect(ctx.writeSystem).toHaveBeenCalledOnce();
    const [level, msg] = ctx.writeSystem.mock.calls[0];
    expect(level).toBe("warn");
    expect(msg).toContain("unhandled runtime event variant");
    expect(msg).toContain("made_up_variant");
  });

  it("known variant does not trip default arm (negative control)", () => {
    const ctx = makeContext();
    handleRuntimeEvent(
      {
        event: "system_log",
        level: "info",
        message: "hello",
        timestamp: "2026-04-22T12:00:00Z",
      },
      ctx,
    );
    expect(ctx.writeSystem).toHaveBeenCalledOnce();
    const calls = ctx.writeSystem.mock.calls.map((c) => c[1] as string);
    expect(calls.some((m) => m.includes("unhandled runtime event variant"))).toBe(
      false,
    );
  });
});

describe("SessionId inventory lifecycle events", () => {
  it("refreshes a created session by opaque ID, never by label", () => {
    const ctx = makeContext();
    handleRuntimeEvent(
      {
        event: "session_created",
        schema_version: 1,
        session: sessionSnapshot({
          session_id: SESSION_B,
          alias: "review",
          label: "Review",
          generation: 0,
          run_id: null,
          run_event_sequence: 0,
          process_id: null,
          running: false,
          resume_available: false,
          was_running_at_shutdown: false,
          lifecycle_state: "closed",
        }),
        timestamp: "2026-08-10T00:00:00Z",
      },
      ctx,
    );
    expect(ctx.refreshSnapshotFromEvent).toHaveBeenCalledWith(SESSION_B);
    expect(ctx.writeSystem).toHaveBeenCalledWith(
      "info",
      "session created: Review",
    );
  });

  it("refreshes every catalog mutation without replacing selection by label", () => {
    const ctx = makeContext();
    const events: RuntimeEvent[] = [
      {
        event: "session_renamed",
        schema_version: 1,
        session_id: SESSION_A,
        old_label: "Review",
        new_label: "Same label is valid",
        timestamp: "2026-08-10T00:00:00Z",
      },
      {
        event: "session_moved",
        schema_version: 1,
        session_id: SESSION_A,
        old_index: 0,
        new_index: 1,
        timestamp: "2026-08-10T00:00:01Z",
      },
      {
        event: "session_permission_changed",
        schema_version: 1,
        session_id: SESSION_A,
        old_profile: "normal",
        new_profile: "unsafe",
        timestamp: "2026-08-10T00:00:02Z",
      },
      {
        event: "session_working_directory_changed",
        schema_version: 1,
        session_id: SESSION_A,
        old_working_dir: "C:\\work",
        new_working_dir: "C:\\other",
        timestamp: "2026-08-10T00:00:03Z",
      },
      {
        event: "session_deleted",
        schema_version: 1,
        session_id: SESSION_A,
        label: "Same label is valid",
        timestamp: "2026-08-10T00:00:04Z",
      },
    ];
    for (const event of events) {
      handleRuntimeEvent(event, ctx);
    }
    expect(ctx.refreshSnapshotFromEvent).toHaveBeenCalledTimes(5);
    for (const call of ctx.refreshSnapshotFromEvent.mock.calls) {
      expect(call).toEqual([]);
    }
    const messages = ctx.writeSystem.mock.calls.map((call) => call[1]);
    expect(messages).toContain("session renamed: Review -> Same label is valid");
    expect(messages).toContain("session moved: 11111111 (1 -> 2)");
    expect(messages).toContain(
      "session permission changed: 11111111 (normal -> unsafe)",
    );
    expect(messages).toContain(
      "session working directory changed: 11111111",
    );
    expect(messages).toContain(
      "session deleted: Same label is valid (11111111)",
    );
  });

  it("fails closed on an unknown catalog event schema", () => {
    const ctx = makeContext();
    handleRuntimeEvent(
      {
        event: "session_renamed",
        schema_version: 99,
        session_id: SESSION_A,
        old_label: "Do not trust",
        new_label: "Unknown schema",
        timestamp: "2026-08-10T00:00:00Z",
      },
      ctx,
    );

    expect(ctx.refreshSnapshotFromEvent).toHaveBeenCalledOnce();
    expect(ctx.refreshSnapshotFromEvent).toHaveBeenCalledWith();
    expect(ctx.writeSystem).toHaveBeenCalledOnce();
    expect(ctx.writeSystem).toHaveBeenCalledWith(
      "warn",
      "unsupported session catalog event schema 99; refreshing the authoritative snapshot",
    );
  });
});

describe("run-event provenance gate", () => {
  it("registers the runtime listener before bootstrap can invoke commands", async () => {
    let releaseRegistration: (() => void) | undefined;
    const register = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          releaseRegistration = resolve;
        }),
    );
    const bootstrap = vi.fn(async () => undefined);

    const start = registerRuntimeEventsBeforeBootstrap(register, bootstrap);
    await Promise.resolve();
    expect(register).toHaveBeenCalledOnce();
    expect(bootstrap).not.toHaveBeenCalled();

    releaseRegistration?.();
    await start;
    expect(bootstrap).toHaveBeenCalledOnce();
  });

  it("retires every renderer surface before a removed session id can be replaced", () => {
    const order: string[] = [];
    const oldPane = {
      dispose: vi.fn(() => order.push("dispose-pane")),
    };
    const oldCard = {
      remove: vi.fn(() => order.push("remove-card")),
    };
    const pending = new Map([[SESSION_A, ["private old output"]]]);
    let activeTerminal: string | null = SESSION_A;
    let activeCopySurface: string | null = SESSION_A;
    let runEventStatePresent = true;
    let snapshotPresent = true;

    retireRendererSession({
      clearPending: () => {
        pending.delete(SESSION_A);
        order.push("clear-pending");
      },
      disposePane: () => oldPane.dispose(),
      removeCard: () => oldCard.remove(),
      forgetRunEventState: () => {
        runEventStatePresent = false;
        order.push("forget-run-event-state");
      },
      removeSnapshot: () => {
        snapshotPresent = false;
        order.push("remove-snapshot");
      },
      clearFocus: () => {
        activeTerminal = null;
        activeCopySurface = null;
        order.push("clear-focus");
      },
    });

    expect(order).toEqual([
      "clear-pending",
      "dispose-pane",
      "remove-card",
      "forget-run-event-state",
      "remove-snapshot",
      "clear-focus",
    ]);
    expect(oldPane.dispose).toHaveBeenCalledOnce();
    expect(oldCard.remove).toHaveBeenCalledOnce();
    expect(pending.has(SESSION_A)).toBe(false);
    expect(activeTerminal).toBeNull();
    expect(activeCopySurface).toBeNull();
    expect(runEventStatePresent).toBe(false);
    expect(snapshotPresent).toBe(false);
  });

  it("replays a pre-snapshot event once when it is newer than the snapshot", () => {
    const ctx = makeContext(false);
    ctx.writeToPane.mockReturnValue(true);

    handleRuntimeEvent(sessionOutput(2, "queued"), ctx);
    expect(ctx.writeToPane).not.toHaveBeenCalled();
    expect(ctx.runEventGate.queued).toHaveLength(1);

    reconcileSessionSnapshot(
      sessionSnapshot(),
      ctx.snapshotById,
      ctx.runEventGate,
    );
    completeInitialRunEventReconciliation(ctx);
    completeInitialRunEventReconciliation(ctx);

    expect(ctx.writeToPane).toHaveBeenCalledOnce();
    expect(ctx.writeToPane).toHaveBeenCalledWith(SESSION_A, "queued");
    expect(ctx.runEventGate.queued).toHaveLength(0);
    expect(ctx.snapshotById.get(SESSION_A)?.run_event_sequence).toBe(2);
  });

  it("drops a queued lifecycle event already represented by the initial snapshot", () => {
    const ctx = makeContext(false);

    handleRuntimeEvent(
      {
        event: "session_state",
        identity: runIdentity(2),
        session: "codex",
        state: "starting",
        reason: "stale bootstrap state",
        timestamp: "2026-08-10T00:00:02Z",
      },
      ctx,
    );
    reconcileSessionSnapshot(
      sessionSnapshot({ run_event_sequence: 3 }),
      ctx.snapshotById,
      ctx.runEventGate,
    );
    completeInitialRunEventReconciliation(ctx);

    expect(ctx.writeSystem).not.toHaveBeenCalled();
    expect(ctx.snapshotById.get(SESSION_A)?.lifecycle_state).toBe("ready");
    expect(ctx.runEventGate.queued).toHaveLength(0);
  });

  it("renders a delayed output at the active snapshot allocation high-water once", () => {
    const ctx = makeContext();
    ctx.writeToPane.mockReturnValue(true);
    reconcileSessionSnapshot(
      sessionSnapshot({ run_event_sequence: 2 }),
      ctx.snapshotById,
      ctx.runEventGate,
    );

    handleRuntimeEvent(sessionOutput(2, "delayed output"), ctx);
    handleRuntimeEvent(sessionOutput(2, "duplicate output"), ctx);

    expect(ctx.writeToPane).toHaveBeenCalledOnce();
    expect(ctx.writeToPane).toHaveBeenCalledWith(SESSION_A, "delayed output");
    expect(ctx.snapshotById.get(SESSION_A)?.run_event_sequence).toBe(2);
  });

  it("applies a delayed work-state event at the active snapshot allocation high-water once", () => {
    const ctx = makeContext();
    reconcileSessionSnapshot(
      sessionSnapshot({ run_event_sequence: 2 }),
      ctx.snapshotById,
      ctx.runEventGate,
    );
    const event: Extract<RuntimeEvent, { event: "session_work_state" }> = {
      event: "session_work_state",
      identity: runIdentity(2),
      session: "codex",
      state: "blocked",
      detail: "approval required",
      previous_state: "thinking",
      timestamp: "2026-08-10T00:00:02Z",
    };

    handleRuntimeEvent(event, ctx);
    handleRuntimeEvent(event, ctx);

    expect(ctx.writeSystem).toHaveBeenCalledOnce();
    expect(ctx.writeSystem).toHaveBeenCalledWith(
      "warn",
      "codex work state: blocked (approval required)",
    );
  });

  it("replays queued output equal to the initial snapshot allocation high-water", () => {
    const ctx = makeContext(false);
    ctx.writeToPane.mockReturnValue(true);

    handleRuntimeEvent(sessionOutput(2, "queued equal"), ctx);
    reconcileSessionSnapshot(
      sessionSnapshot({ run_event_sequence: 2 }),
      ctx.snapshotById,
      ctx.runEventGate,
    );
    completeInitialRunEventReconciliation(ctx);
    handleRuntimeEvent(sessionOutput(2, "duplicate equal"), ctx);

    expect(ctx.writeToPane).toHaveBeenCalledOnce();
    expect(ctx.writeToPane).toHaveBeenCalledWith(SESSION_A, "queued equal");
  });

  it("replays queued output and work state when the initial snapshot is terminal", () => {
    const ctx = makeContext(false);
    ctx.writeToPane.mockReturnValue(true);
    const workState: Extract<
      RuntimeEvent,
      { event: "session_work_state" }
    > = {
      event: "session_work_state",
      identity: runIdentity(3),
      session: "codex",
      state: "blocked",
      detail: "approval required",
      previous_state: "thinking",
      timestamp: "2026-08-10T00:00:03Z",
    };

    handleRuntimeEvent(sessionOutput(2, "terminal bootstrap output"), ctx);
    handleRuntimeEvent(workState, ctx);
    reconcileSessionSnapshot(
      sessionSnapshot({
        lifecycle_state: "closed",
        run_id: null,
        run_event_sequence: 4,
        process_id: null,
        running: false,
      }),
      ctx.snapshotById,
      ctx.runEventGate,
    );
    completeInitialRunEventReconciliation(ctx);

    expect(ctx.writeToPane).toHaveBeenCalledOnce();
    expect(ctx.writeToPane).toHaveBeenCalledWith(
      SESSION_A,
      "terminal bootstrap output",
    );
    expect(ctx.writeSystem).toHaveBeenCalledOnce();
    expect(ctx.writeSystem).toHaveBeenCalledWith(
      "warn",
      "codex work state: blocked (approval required)",
    );
    expect(ctx.snapshotById.get(SESSION_A)).toMatchObject({
      lifecycle_state: "closed",
      run_id: null,
      run_event_sequence: 4,
    });

    handleRuntimeEvent(sessionOutput(5, "past terminal high-water"), ctx);
    expect(ctx.writeToPane).toHaveBeenCalledOnce();
  });

  it("replays queued run events in FIFO order", () => {
    const ctx = makeContext(false);
    ctx.writeToPane.mockReturnValue(true);

    handleRuntimeEvent(sessionOutput(2, "two"), ctx);
    handleRuntimeEvent(sessionOutput(3, "three"), ctx);
    reconcileSessionSnapshot(
      sessionSnapshot(),
      ctx.snapshotById,
      ctx.runEventGate,
    );
    completeInitialRunEventReconciliation(ctx);

    expect(ctx.writeToPane.mock.calls).toEqual([
      [SESSION_A, "two"],
      [SESSION_A, "three"],
    ]);
  });

  it("rejects a stale run_id without buffering or rendering output", () => {
    const ctx = makeContext();
    ctx.writeToPane.mockReturnValue(true);
    ctx.snapshotById.set(SESSION_A, sessionSnapshot());

    handleRuntimeEvent(
      sessionOutput(2, "stale", { run_id: RUN_B }),
      ctx,
    );

    expect(ctx.writeToPane).not.toHaveBeenCalled();
    expect(ctx.pendingOutput.size).toBe(0);
    expect(ctx.snapshotById.get(SESSION_A)?.run_event_sequence).toBe(1);
  });

  it("keeps same-label sessions isolated by session_id", () => {
    const ctx = makeContext();
    ctx.writeToPane.mockReturnValue(true);
    ctx.snapshotById.set(SESSION_A, sessionSnapshot());
    const replacement = sessionSnapshot({
      session_id: SESSION_B,
      run_id: RUN_B,
      generation: 1,
      run_event_sequence: 0,
    });

    expect(
      reconcileSessionSnapshot(
        replacement,
        ctx.snapshotById,
        ctx.runEventGate,
      ),
    ).toEqual({ accepted: true });
    handleRuntimeEvent(sessionOutput(99, "old lineage"), ctx);

    expect(ctx.writeToPane).toHaveBeenCalledOnce();
    expect(ctx.writeToPane).toHaveBeenCalledWith(SESSION_A, "old lineage");
    expect(ctx.pendingOutput.size).toBe(0);
    expect(ctx.snapshotById.get(SESSION_A)?.session_id).toBe(SESSION_A);
    expect(ctx.snapshotById.get(SESSION_B)?.session_id).toBe(SESSION_B);
  });

  it("forgets all gate state when a session leaves renderer inventory", () => {
    const ctx = makeContext(false);
    reconcileSessionSnapshot(
      sessionSnapshot({ generation: 1, run_event_sequence: 1 }),
      ctx.snapshotById,
      ctx.runEventGate,
    );
    reconcileSessionSnapshot(
      sessionSnapshot({
        generation: 2,
        run_id: RUN_B,
        run_event_sequence: 2,
      }),
      ctx.snapshotById,
      ctx.runEventGate,
    );
    ctx.runEventGate.retiredGenerationFloorBySessionId.set(SESSION_A, 1);
    handleRuntimeEvent(
      sessionOutput(1, "queued old lineage", { generation: 1 }),
      ctx,
    );

    forgetRunEventSession(SESSION_A, ctx.runEventGate);
    ctx.snapshotById.delete(SESSION_A);

    expect(ctx.runEventGate.cursorBySessionId.has(SESSION_A)).toBe(false);
    expect(ctx.runEventGate.retiredCursorsBySessionId.has(SESSION_A)).toBe(
      false,
    );
    expect(
      ctx.runEventGate.retiredGenerationFloorBySessionId.has(SESSION_A),
    ).toBe(false);
    expect(ctx.runEventGate.queued).toHaveLength(0);

    reconcileSessionSnapshot(
      sessionSnapshot({
        session_id: SESSION_B,
        run_id: RUN_B,
        generation: 1,
        run_event_sequence: 0,
      }),
      ctx.snapshotById,
      ctx.runEventGate,
    );
    ctx.runEventGate.initialized = true;
    ctx.writeToPane.mockReturnValue(true);
    handleRuntimeEvent(sessionOutput(2, "old session output"), ctx);
    expect(ctx.writeToPane).not.toHaveBeenCalled();
  });

  it("accepts sequence 3 then rejects out-of-order sequence 2", () => {
    const ctx = makeContext();
    ctx.writeToPane.mockReturnValue(true);
    ctx.snapshotById.set(SESSION_A, sessionSnapshot());

    handleRuntimeEvent(sessionOutput(3, "three"), ctx);
    handleRuntimeEvent(sessionOutput(2, "two"), ctx);

    expect(ctx.writeToPane).toHaveBeenCalledOnce();
    expect(ctx.writeToPane).toHaveBeenCalledWith(SESSION_A, "three");
    expect(ctx.snapshotById.get(SESSION_A)?.run_event_sequence).toBe(3);
  });

  it("adopts a higher-generation run from content, and its late state still lands", () => {
    // Contract change 2026-08-30: a newer run's output arriving before its
    // `starting` state is ADOPTED, never swallowed — the pty reader can win
    // the emit race against the spawner, and what it carries is a resumed
    // harness's transcript replay (the head of the conversation on screen).
    const ctx = makeContext();
    ctx.writeToPane.mockReturnValue(true);
    ctx.snapshotById.set(SESSION_A, sessionSnapshot());

    handleRuntimeEvent(
      sessionOutput(2, "future output", {
        run_id: RUN_B,
        generation: 8,
      }),
      ctx,
    );
    expect(ctx.writeToPane).toHaveBeenCalledWith(SESSION_A, "future output");

    handleRuntimeEvent(
      {
        event: "session_state",
        identity: runIdentity(1, { run_id: RUN_B, generation: 8 }),
        session: "codex",
        state: "starting",
        reason: "restart requested",
        timestamp: "2026-08-10T00:00:01Z",
      },
      ctx,
    );
    expect(ctx.snapshotById.get(SESSION_A)).toMatchObject({
      lifecycle_state: "starting",
      generation: 8,
      run_id: RUN_B,
    });

    // A bare session_exit from a yet-unseen even-newer run still waits for
    // its state: exits carry no renderable content to lose.
    handleRuntimeEvent(
      {
        event: "session_exit",
        identity: runIdentity(1, { run_id: RUN_C, generation: 9 }),
        session: "codex",
        exit_code: 0,
        reason: "clean_exit",
        timestamp: "2026-08-10T00:00:02Z",
      } as unknown as RuntimeEvent,
      ctx,
    );
    expect(ctx.snapshotById.get(SESSION_A)?.generation).toBe(8);
  });

  it("rejects a snapshot that would move the current run cursor backward", () => {
    const snapshots = new Map<string, SessionSnapshot>();
    const gate = createRunEventGateState();
    snapshots.set(SESSION_A, sessionSnapshot({ run_event_sequence: 3 }));

    expect(
      reconcileSessionSnapshot(
        sessionSnapshot({ run_event_sequence: 2 }),
        snapshots,
        gate,
      ),
    ).toEqual({ accepted: false });
    expect(snapshots.get(SESSION_A)?.run_event_sequence).toBe(3);
  });

  it("replays unrepresented content through a same-generation terminal high-water", () => {
    const ctx = makeContext();
    ctx.writeToPane.mockReturnValue(true);
    ctx.snapshotById.set(
      "codex",
      sessionSnapshot({ run_event_sequence: 2 }),
    );

    expect(
      reconcileSessionSnapshot(
        sessionSnapshot({
          lifecycle_state: "closed",
          run_id: null,
          run_event_sequence: 4,
          process_id: null,
          running: false,
        }),
        ctx.snapshotById,
        ctx.runEventGate,
      ),
    ).toEqual({ accepted: true });

    handleRuntimeEvent(sessionOutput(3, "late three"), ctx);
    handleRuntimeEvent(sessionOutput(4, "late four"), ctx);
    handleRuntimeEvent(sessionOutput(4, "duplicate four"), ctx);
    handleRuntimeEvent(sessionOutput(5, "past high-water"), ctx);

    expect(ctx.writeToPane.mock.calls).toEqual([
      [SESSION_A, "late three"],
      [SESSION_A, "late four"],
    ]);
    expect(ctx.snapshotById.get(SESSION_A)).toMatchObject({
      lifecycle_state: "closed",
      run_id: null,
      run_event_sequence: 4,
      running: false,
    });
  });

  it("replays immediate-predecessor output after a newer terminal snapshot", () => {
    const ctx = makeContext();
    ctx.writeToPane.mockReturnValue(true);
    reconcileSessionSnapshot(
      sessionSnapshot({ generation: 1, run_event_sequence: 2 }),
      ctx.snapshotById,
      ctx.runEventGate,
    );
    reconcileSessionSnapshot(
      sessionSnapshot({
        generation: 2,
        lifecycle_state: "closed",
        run_id: null,
        run_event_sequence: 4,
        process_id: null,
        running: false,
      }),
      ctx.snapshotById,
      ctx.runEventGate,
    );

    handleRuntimeEvent(
      sessionOutput(3, "predecessor output", { generation: 1 }),
      ctx,
    );
    handleRuntimeEvent(
      sessionOutput(3, "predecessor duplicate", { generation: 1 }),
      ctx,
    );
    handleRuntimeEvent(
      sessionOutput(5, "past predecessor high-water", { generation: 1 }),
      ctx,
    );
    handleRuntimeEvent(
      {
        event: "session_state",
        identity: runIdentity(4, { generation: 1 }),
        session: "codex",
        state: "ready",
        reason: "stale predecessor reopen",
        timestamp: "2026-08-10T00:00:04Z",
      },
      ctx,
    );

    expect(ctx.writeToPane).toHaveBeenCalledOnce();
    expect(ctx.writeToPane).toHaveBeenCalledWith(
      SESSION_A,
      "predecessor output",
    );
    expect(ctx.snapshotById.get(SESSION_A)).toMatchObject({
      generation: 2,
      lifecycle_state: "closed",
      run_id: null,
      run_event_sequence: 4,
    });
  });

  it("replays a queued predecessor output against an initial newer terminal snapshot", () => {
    const ctx = makeContext(false);
    ctx.writeToPane.mockReturnValue(true);

    handleRuntimeEvent(
      sessionOutput(3, "queued predecessor", { generation: 1 }),
      ctx,
    );
    reconcileSessionSnapshot(
      sessionSnapshot({
        generation: 2,
        lifecycle_state: "closed",
        run_id: null,
        run_event_sequence: 4,
        process_id: null,
        running: false,
      }),
      ctx.snapshotById,
      ctx.runEventGate,
    );
    completeInitialRunEventReconciliation(ctx);
    handleRuntimeEvent(
      sessionOutput(3, "queued predecessor duplicate", { generation: 1 }),
      ctx,
    );

    expect(ctx.writeToPane).toHaveBeenCalledOnce();
    expect(ctx.writeToPane).toHaveBeenCalledWith(
      SESSION_A,
      "queued predecessor",
    );
    expect(ctx.snapshotById.get(SESSION_A)).toMatchObject({
      generation: 2,
      lifecycle_state: "closed",
      run_id: null,
      run_event_sequence: 4,
    });
  });

  it("retains exact retired runs across two snapshot-generation jumps", () => {
    const ctx = makeContext();
    ctx.writeToPane.mockReturnValue(true);
    reconcileSessionSnapshot(
      sessionSnapshot({ generation: 1, run_event_sequence: 1 }),
      ctx.snapshotById,
      ctx.runEventGate,
    );
    reconcileSessionSnapshot(
      sessionSnapshot({
        generation: 2,
        run_id: RUN_B,
        run_event_sequence: 2,
      }),
      ctx.snapshotById,
      ctx.runEventGate,
    );
    reconcileSessionSnapshot(
      sessionSnapshot({
        generation: 3,
        run_id: RUN_C,
        run_event_sequence: 3,
      }),
      ctx.snapshotById,
      ctx.runEventGate,
    );

    handleRuntimeEvent(
      sessionOutput(1, "run A delayed across B and C", { generation: 1 }),
      ctx,
    );
    handleRuntimeEvent(
      sessionOutput(1, "run A duplicate", { generation: 1 }),
      ctx,
    );

    expect(ctx.writeToPane).toHaveBeenCalledOnce();
    expect(ctx.writeToPane).toHaveBeenCalledWith(
      SESSION_A,
      "run A delayed across B and C",
    );
    expect(ctx.snapshotById.get(SESSION_A)).toMatchObject({
      generation: 3,
      run_id: RUN_C,
      run_event_sequence: 3,
    });
  });

  it("bounds retired-run history and reports any lossy eviction", () => {
    const ctx = makeContext();
    ctx.writeToPane.mockReturnValue(true);
    const numberedRunId = (value: number) =>
      `00000000-0000-4000-8000-${value.toString().padStart(12, "0")}`;
    reconcileSessionSnapshot(
      sessionSnapshot({
        generation: 1,
        run_id: numberedRunId(1),
        run_event_sequence: 1,
      }),
      ctx.snapshotById,
      ctx.runEventGate,
    );
    handleRuntimeEvent(
      sessionOutput(1, "oldest run content", {
        run_id: numberedRunId(1),
        generation: 1,
      }),
      ctx,
    );

    for (
      let generation = 2;
      generation <= MAX_RETIRED_RUN_CURSORS_PER_SESSION + 2;
      generation += 1
    ) {
      reconcileSessionSnapshot(
        sessionSnapshot({
          generation,
          run_id: numberedRunId(generation),
          run_event_sequence: generation,
        }),
        ctx.snapshotById,
        ctx.runEventGate,
      );
    }

    expect(
      ctx.runEventGate.retiredCursorsBySessionId.get(SESSION_A),
    ).toHaveLength(MAX_RETIRED_RUN_CURSORS_PER_SESSION);
    expect(ctx.runEventGate.retiredCursorEvictions).toBe(1);
    handleRuntimeEvent(
      sessionOutput(1, "evicted duplicate", {
        run_id: numberedRunId(1),
        generation: 1,
      }),
      ctx,
    );
    expect(ctx.writeToPane).toHaveBeenCalledOnce();
    expect(
      ctx.runEventGate.retiredCursorsBySessionId.get(SESSION_A),
    ).toHaveLength(MAX_RETIRED_RUN_CURSORS_PER_SESSION);
    expect(ctx.runEventGate.retiredCursorEvictions).toBe(1);
    flushRunEventGateWarnings(ctx);
    flushRunEventGateWarnings(ctx);
    expect(ctx.writeSystem).toHaveBeenCalledOnce();
    expect(ctx.writeSystem.mock.calls[0][1]).toContain(
      "1 oldest retired run cursor evicted",
    );
  });

  it("applies a stop state and its later exit before terminalizing the run", () => {
    const ctx = makeContext();
    ctx.snapshotById.set(
      SESSION_A,
      sessionSnapshot({ run_event_sequence: 2 }),
    );

    handleRuntimeEvent(
      {
        event: "session_state",
        identity: runIdentity(3),
        session: "codex",
        state: "closed",
        reason: "operator stop requested",
        timestamp: "2026-08-10T00:00:03Z",
      },
      ctx,
    );

    expect(ctx.snapshotById.get(SESSION_A)).toMatchObject({
      lifecycle_state: "closed",
      run_id: RUN_A,
      run_event_sequence: 3,
      running: false,
    });

    handleRuntimeEvent(
      {
        event: "session_exit",
        identity: runIdentity(4),
        session: "codex",
        process_id: 42,
        exit_code: 0,
        signal: null,
        success: true,
        reason: "operator_stop",
        requested: true,
        timestamp: "2026-08-10T00:00:04Z",
      },
      ctx,
    );

    expect(ctx.applyPaneSnapshot).toHaveBeenCalledTimes(2);
    expect(ctx.snapshotById.get(SESSION_A)).toMatchObject({
      lifecycle_state: "closed",
      run_id: null,
      run_event_sequence: 4,
      process_id: null,
      running: false,
    });
  });

  it("applies a crash exit followed by its monotonic failed state", () => {
    const ctx = makeContext();
    ctx.snapshotById.set(
      SESSION_A,
      sessionSnapshot({ run_event_sequence: 2 }),
    );

    handleRuntimeEvent(
      {
        event: "session_exit",
        identity: runIdentity(3),
        session: "codex",
        process_id: 42,
        exit_code: 1,
        signal: null,
        success: false,
        reason: "crash_exit",
        requested: false,
        timestamp: "2026-08-10T00:00:03Z",
      },
      ctx,
    );
    handleRuntimeEvent(
      {
        event: "session_state",
        identity: runIdentity(4),
        session: "codex",
        state: "failed",
        reason: "process exited with code 1",
        timestamp: "2026-08-10T00:00:04Z",
      },
      ctx,
    );

    expect(ctx.applyPaneSnapshot).toHaveBeenCalledTimes(2);
    expect(ctx.snapshotById.get(SESSION_A)).toMatchObject({
      lifecycle_state: "failed",
      run_id: null,
      run_event_sequence: 4,
      process_id: null,
      running: false,
      last_error: "process exited with code 1",
    });

    handleRuntimeEvent(
      {
        event: "session_state",
        identity: runIdentity(5),
        session: "codex",
        state: "ready",
        reason: "stale reopen",
        timestamp: "2026-08-10T00:00:05Z",
      },
      ctx,
    );
    handleRuntimeEvent(
      {
        event: "session_state",
        identity: runIdentity(6),
        session: "codex",
        state: "closed",
        reason: "stale terminal regression",
        timestamp: "2026-08-10T00:00:06Z",
      },
      ctx,
    );
    expect(ctx.applyPaneSnapshot).toHaveBeenCalledTimes(2);
    expect(ctx.snapshotById.get(SESSION_A)?.lifecycle_state).toBe("failed");
  });

  it("accepts a newer terminal state after an initial terminal snapshot", () => {
    const ctx = makeContext();
    reconcileSessionSnapshot(
      sessionSnapshot({
        lifecycle_state: "closed",
        run_id: null,
        run_event_sequence: 3,
        process_id: null,
        running: false,
      }),
      ctx.snapshotById,
      ctx.runEventGate,
    );

    handleRuntimeEvent(
      {
        event: "session_state",
        identity: runIdentity(4),
        session: "codex",
        state: "failed",
        reason: "late failure classification",
        timestamp: "2026-08-10T00:00:04Z",
      },
      ctx,
    );

    expect(ctx.applyPaneSnapshot).toHaveBeenCalledOnce();
    expect(ctx.snapshotById.get(SESSION_A)).toMatchObject({
      lifecycle_state: "failed",
      run_id: null,
      run_event_sequence: 4,
      running: false,
    });
  });

  it("keeps a terminal cursor closed against a stale active snapshot and event", () => {
    const ctx = makeContext();
    ctx.writeToPane.mockReturnValue(true);
    ctx.snapshotById.set(
      SESSION_A,
      sessionSnapshot({
        lifecycle_state: "closed",
        run_id: null,
        run_event_sequence: 4,
        process_id: null,
        running: false,
      }),
    );

    expect(
      reconcileSessionSnapshot(
        sessionSnapshot({ run_event_sequence: 5 }),
        ctx.snapshotById,
        ctx.runEventGate,
      ),
    ).toEqual({ accepted: false });
    handleRuntimeEvent(sessionOutput(5, "stale active output"), ctx);

    expect(ctx.writeToPane).not.toHaveBeenCalled();
    expect(ctx.snapshotById.get(SESSION_A)).toMatchObject({
      lifecycle_state: "closed",
      run_id: null,
      run_event_sequence: 4,
      process_id: null,
      running: false,
    });
  });
});

describe("route status truth", () => {
  const timestamp = "2026-08-10T00:00:00Z";

  it("labels routed_message as pending rather than delivered", () => {
    const ctx = makeContext();
    handleRuntimeEvent(
      {
        event: "routed_message",
        id: "route-message-1",
        from: "operator",
        to: "codex",
        scope: "direct",
        content: "review this",
        timestamp,
      },
      ctx,
    );

    expect(ctx.writeSystem).toHaveBeenCalledWith(
      "info",
      "route pending: operator -> codex (direct); PTY completion not yet confirmed",
    );
    expect(ctx.writeSystem.mock.calls[0][1]).not.toContain("delivered");
  });

  it("keeps resolved routes pending until a PTY-write receipt", () => {
    const ctx = makeContext();
    handleRuntimeEvent(
      {
        event: "route_delivery",
        request_id: "req-1",
        route_id: "route-123456",
        from: "operator",
        logical_to: "codex",
        scope: "direct",
        recipient: null,
        recipient_index: 0,
        recipient_count: 1,
        payload_part_count: 0,
        phase: "resolved",
        bytes_written: 0,
        error: null,
        timestamp,
      },
      ctx,
    );

    expect(ctx.writeSystem).toHaveBeenCalledWith(
      "info",
      "route route-12 pending: 1 recipient resolved; awaiting PTY write",
    );
  });

  it("does not regress when resolved precedes routed_message in production order", () => {
    const ctx = makeContext();
    handleRuntimeEvent(
      {
        event: "route_delivery",
        request_id: "req-1",
        route_id: "route-123456",
        from: "operator",
        logical_to: "codex",
        scope: "direct",
        recipient: null,
        recipient_index: 0,
        recipient_count: 1,
        payload_part_count: 0,
        phase: "resolved",
        bytes_written: 0,
        error: null,
        timestamp,
      },
      ctx,
    );
    handleRuntimeEvent(
      {
        event: "routed_message",
        id: "route-message-1",
        from: "operator",
        to: "codex",
        scope: "direct",
        content: "review this",
        timestamp,
      },
      ctx,
    );

    expect(ctx.writeSystem.mock.calls.map((call) => call[1])).toEqual([
      "route route-12 pending: 1 recipient resolved; awaiting PTY write",
      "route pending: operator -> codex (direct); PTY completion not yet confirmed",
    ]);
    expect(ctx.writeSystem.mock.calls[1][1]).not.toContain("recipient resolution");
  });

  it("describes written as a PTY outcome without claiming model receipt", () => {
    const ctx = makeContext();
    handleRuntimeEvent(
      {
        event: "route_delivery",
        request_id: "req-1",
        route_id: "route-123456",
        from: "operator",
        logical_to: "codex",
        scope: "direct",
        recipient: "codex",
        recipient_index: 0,
        recipient_count: 1,
        payload_part_count: 1,
        phase: "written",
        bytes_written: 42,
        error: null,
        timestamp,
      },
      ctx,
    );

    expect(ctx.writeSystem).toHaveBeenCalledWith(
      "info",
      "route route-12 PTY write completed for codex (42 bytes); model receipt not confirmed",
    );
    expect(ctx.writeSystem.mock.calls[0][1]).not.toContain("delivered");
  });

  it("surfaces the per-recipient PTY-write failure", () => {
    const ctx = makeContext();
    handleRuntimeEvent(
      {
        event: "route_delivery",
        request_id: "req-1",
        route_id: "route-123456",
        from: "operator",
        logical_to: "codex",
        scope: "direct",
        recipient: "codex",
        recipient_index: 0,
        recipient_count: 1,
        payload_part_count: 1,
        phase: "failed",
        bytes_written: 0,
        error: "pipe closed",
        timestamp,
      },
      ctx,
    );

    expect(ctx.writeSystem).toHaveBeenCalledWith(
      "error",
      "route route-12 PTY write failed for codex: pipe closed",
    );
  });

  it("warns when a failed PTY write committed a visible prefix", () => {
    const ctx = makeContext();
    handleRuntimeEvent(
      {
        event: "route_delivery",
        request_id: "req-1",
        route_id: "route-123456",
        from: "operator",
        logical_to: "codex",
        scope: "direct",
        recipient: "codex",
        recipient_index: 0,
        recipient_count: 1,
        payload_part_count: 1,
        phase: "failed",
        bytes_written: 17,
        error: "pipe closed",
        timestamp,
      },
      ctx,
    );

    expect(ctx.writeSystem).toHaveBeenCalledWith(
      "error",
      "route route-12 PTY write failed for codex after 17 bytes were accepted by the PTY; content may be partial and model receipt is unconfirmed: pipe closed",
    );
  });
});

describe("pending pane-output buffer", () => {
  it("buffers unattached pane bytes without shedding below the byte cap", () => {
    const ctx = makeContext();
    reconcileSessionSnapshot(
      sessionSnapshot({ run_event_sequence: 3 }),
      ctx.snapshotById,
      ctx.runEventGate,
    );
    handleRuntimeEvent(sessionOutput(1, "\x1b[2J\x1b[H full repaint"), ctx);
    handleRuntimeEvent(sessionOutput(2, "diff one"), ctx);
    handleRuntimeEvent(sessionOutput(3, "diff two"), ctx);

    const entry = [...ctx.pendingOutput.values()][0];
    expect(entry.chunks).toEqual([
      "\x1b[2J\x1b[H full repaint",
      "diff one",
      "diff two",
    ]);
    expect(entry.bytes).toBe(
      "\x1b[2J\x1b[H full repaint".length + "diff one".length + "diff two".length,
    );
    expect(entry.shed).toBe(0);
  });

  it("sheds the ENTIRE buffer on byte-cap overflow — a TUI stream must never survive as a headless tail", () => {
    const ctx = makeContext();
    reconcileSessionSnapshot(
      sessionSnapshot({ run_event_sequence: 3 }),
      ctx.snapshotById,
      ctx.runEventGate,
    );
    handleRuntimeEvent(sessionOutput(1, "the full-repaint head"), ctx);
    handleRuntimeEvent(
      sessionOutput(2, "x".repeat(MAX_PENDING_BYTES_PER_SESSION)),
      ctx,
    );

    const entry = [...ctx.pendingOutput.values()][0];
    expect(entry.chunks).toEqual([]);
    expect(entry.bytes).toBe(0);
    expect(entry.shed).toBe(1);
    expect(ctx.writeSystem).toHaveBeenCalledWith(
      "warn",
      expect.stringContaining("shed the entire buffered stream"),
    );

    handleRuntimeEvent(sessionOutput(3, "fresh tail after shed"), ctx);
    const after = [...ctx.pendingOutput.values()][0];
    expect(after.chunks).toEqual(["fresh tail after shed"]);
    expect(after.shed).toBe(1);
  });
});

describe("newer-run content adoption (resume replay must never be swallowed)", () => {
  it("adopts a newer generation from output arriving before its starting state", () => {
    const ctx = makeContext();
    // Boot snapshot: not running, generation 0, no run — the catalog shape.
    ctx.snapshotById.set(
      SESSION_A,
      sessionSnapshot({
        generation: 0,
        run_id: null,
        run_event_sequence: 0,
        running: false,
        lifecycle_state: "closed",
      }),
    );
    ctx.writeToPane.mockReturnValue(true);

    // The resume replay wins the emit race: output first, state second.
    handleRuntimeEvent(
      sessionOutput(2, "REPLAYED TRANSCRIPT", { generation: 1, run_id: RUN_B }),
      ctx,
    );
    expect(ctx.writeToPane).toHaveBeenCalledWith(SESSION_A, "REPLAYED TRANSCRIPT");
    expect(
      ctx.writeSystem.mock.calls.some(
        (call: unknown[]) => String(call[1]).includes("adopted from terminal output"),
      ),
    ).toBe(true);

    // The late starting state still lands and advances lifecycle.
    handleRuntimeEvent(
      {
        event: "session_state",
        identity: runIdentity(1, { generation: 1, run_id: RUN_B }),
        session: "codex",
        state: "starting",
        reason: "launch requested",
        timestamp: "2026-08-10T00:00:01Z",
      } as RuntimeEvent,
      ctx,
    );
    expect(ctx.snapshotById.get(SESSION_A)?.lifecycle_state).toBe("starting");

    // Later output of the adopted run flows normally.
    handleRuntimeEvent(
      sessionOutput(3, "MORE", { generation: 1, run_id: RUN_B }),
      ctx,
    );
    expect(ctx.writeToPane).toHaveBeenCalledWith(SESSION_A, "MORE");
  });

  it("still rejects stale output from an older generation after adoption", () => {
    const ctx = makeContext();
    ctx.snapshotById.set(
      SESSION_A,
      sessionSnapshot({
        generation: 0,
        run_id: null,
        run_event_sequence: 0,
        running: false,
        lifecycle_state: "closed",
      }),
    );
    ctx.writeToPane.mockReturnValue(true);
    handleRuntimeEvent(
      sessionOutput(2, "NEW RUN", { generation: 2, run_id: RUN_C }),
      ctx,
    );
    ctx.writeToPane.mockClear();

    // A straggler from a lower generation with no retired cursor: dropped.
    handleRuntimeEvent(
      sessionOutput(9, "STALE", { generation: 1, run_id: RUN_B }),
      ctx,
    );
    expect(ctx.writeToPane).not.toHaveBeenCalledWith(SESSION_A, "STALE");
  });
});

describe("ui_output_gap heal", () => {
  it("forces a repaint resync for the shed session", () => {
    const ctx = makeContext();
    const requestRepaintResync = vi.fn();
    ctx.requestRepaintResync = requestRepaintResync;

    handleRuntimeEvent(
      {
        event: "ui_output_gap",
        identity: runIdentity(5),
        session: "codex",
        dropped_events: 3,
        timestamp: "2026-08-10T00:00:00Z",
      } as RuntimeEvent,
      ctx,
    );
    expect(requestRepaintResync).toHaveBeenCalledWith(SESSION_A);
  });
});
