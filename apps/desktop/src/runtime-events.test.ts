/**
 * Tests for runtime-events.ts.
 *
 * Layer 1 — focused unit tests for sideband phases and event-contract exhaustiveness.
 */

import { describe, expect, it, vi } from "vitest";

import {
  handleRuntimeEvent,
  type PendingBuffer,
  type RuntimeEventContext,
} from "./runtime-events";
import type { RuntimeEvent, SessionSnapshot } from "./types";

function makeContext(): RuntimeEventContext & {
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
  const snapshotByName = new Map<string, SessionSnapshot>();
  const pendingOutput = new Map<string, PendingBuffer>();
  return {
    writeSystem,
    refreshSnapshotFromEvent,
    snapshotByName,
    pendingOutput,
    writeToPane,
    applyPaneSnapshot,
    setControlEndpoint,
  };
}

function sidebandEvent(overrides: Partial<Extract<RuntimeEvent, { event: "sideband_request_lifecycle" }>>): RuntimeEvent {
  return {
    event: "sideband_request_lifecycle",
    request_id: "abcdef12-3456-7890-abcd-ef1234567890",
    action: "wait_quiet",
    session: "claude",
    phase: "completed",
    elapsed_ms: 1234,
    timestamp: "2026-04-22T12:00:00Z",
    ...overrides,
  };
}

describe("runtime-events sideband handler", () => {
  it("sideband slow_warning emits warn", () => {
    const ctx = makeContext();
    handleRuntimeEvent(sidebandEvent({ phase: "slow_warning" }), ctx);
    expect(ctx.writeSystem).toHaveBeenCalledOnce();
    const [level, msg] = ctx.writeSystem.mock.calls[0];
    expect(level).toBe("warn");
    expect(msg).toContain("slow_warning");
  });

  it("sideband timed_out emits error", () => {
    const ctx = makeContext();
    handleRuntimeEvent(sidebandEvent({ phase: "timed_out" }), ctx);
    expect(ctx.writeSystem).toHaveBeenCalledOnce();
    const [level] = ctx.writeSystem.mock.calls[0];
    expect(level).toBe("error");
  });

  it("sideband failed emits error", () => {
    const ctx = makeContext();
    handleRuntimeEvent(sidebandEvent({ phase: "failed" }), ctx);
    expect(ctx.writeSystem).toHaveBeenCalledOnce();
    const [level] = ctx.writeSystem.mock.calls[0];
    expect(level).toBe("error");
  });

  it("sideband started emits nothing", () => {
    const ctx = makeContext();
    handleRuntimeEvent(sidebandEvent({ phase: "started" }), ctx);
    expect(ctx.writeSystem).not.toHaveBeenCalled();
  });

  it("sideband completed emits nothing", () => {
    const ctx = makeContext();
    handleRuntimeEvent(sidebandEvent({ phase: "completed" }), ctx);
    expect(ctx.writeSystem).not.toHaveBeenCalled();
  });

  it("sideband null session rendering omits empty parens", () => {
    const ctx = makeContext();
    handleRuntimeEvent(
      sidebandEvent({ phase: "failed", session: null }),
      ctx,
    );
    const [, msg] = ctx.writeSystem.mock.calls[0];
    expect(msg).not.toContain("()");
    expect(msg).not.toMatch(/\(\s*\)/);
  });

  it("sideband request_id is truncated to 8 chars", () => {
    const ctx = makeContext();
    handleRuntimeEvent(
      sidebandEvent({
        phase: "failed",
        request_id: "abcdef1234567890",
      }),
      ctx,
    );
    const [, msg] = ctx.writeSystem.mock.calls[0];
    expect(msg).toContain("[req=abcdef12]");
    expect(msg).not.toContain("[req=abcdef1234");
  });
});

describe("runtime-events exhaustiveness default arm", () => {
  it("every supervisor telemetry variant is handled explicitly", () => {
    const timestamp = "2026-04-22T12:00:00Z";
    const events: RuntimeEvent[] = [
      {
        event: "session_work_state",
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
        event: "dispatch_template_warning",
        request_id: "req-1",
        session: "claude",
        detected_patterns: [],
        missing_patterns: ["completion signal"],
        severity: "info",
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
      {
        event: "pane_signal",
        request_id: "req-1",
        session: "claude",
        task_id: "legacy",
        signal_type: "done",
        summary: "complete",
        artifact_paths: [],
        commit_sha: null,
        timestamp,
      },
      {
        event: "request_ack",
        request_id: "req-1",
        session: "codex",
        action: "send_input",
        bytes_written: 5,
        timestamp,
      },
      {
        event: "request_ack_timeout",
        request_id: "req-1",
        session: "codex",
        action: "send_input",
        elapsed_ms: 1_000,
        timestamp,
      },
      {
        event: "dispatch_no_reaction",
        request_id: "req-1",
        session: "codex",
        action: "send_input",
        timestamp,
      },
    ];

    for (const event of events) {
      const ctx = makeContext();
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
