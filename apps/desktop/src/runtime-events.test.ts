/**
 * Tests for runtime-events.ts.
 *
 * Layer 1 — 9 unit tests (sideband phases + exhaustiveness).
 * Locked via plan-v3.md verifier block: `unit_tests: 9 across 1 file
 * matching apps/desktop/src/runtime-events.test.ts`.
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
