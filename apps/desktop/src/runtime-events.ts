/**
 * Runtime-event handler extracted from main.ts for testability.
 *
 * main.ts has no exports and runs top-level DOM initialization on import,
 * so testing handleRuntimeEvent directly requires either jsdom + awkward
 * module-surgery or extraction. This module is the extraction, following
 * the copy-selection.ts testable-module precedent.
 *
 * The handler operates on an injected RuntimeEventContext rather than
 * module-level globals. main.ts constructs a production ctx wiring the
 * live DOM + supervisor; tests construct a mock ctx with spy functions.
 */

import type { RuntimeEvent, SessionSnapshot } from "./types";

export const MAX_PENDING_CHUNKS_PER_SESSION = 256;

export interface PendingBuffer {
  chunks: string[];
  dropped: number;
}

export interface RuntimeEventContext {
  writeSystem: (level: "info" | "warn" | "error", message: string) => void;
  refreshSnapshotFromEvent: (preferredGroup?: string) => void;
  snapshotByName: Map<string, SessionSnapshot>;
  pendingOutput: Map<string, PendingBuffer>;
  /** Returns true if a pane exists for `session` and the chunk was written. */
  writeToPane: (session: string, chunk: string) => boolean;
  applyPaneSnapshot: (name: string, snapshot: SessionSnapshot) => void;
  setControlEndpoint: (endpoint: string) => void;
}

export function handleRuntimeEvent(
  event: RuntimeEvent,
  ctx: RuntimeEventContext,
): void {
  switch (event.event) {
    case "session_output": {
      if (ctx.writeToPane(event.session, event.chunk)) {
        break;
      }

      let entry = ctx.pendingOutput.get(event.session);
      if (!entry) {
        entry = { chunks: [], dropped: 0 };
        ctx.pendingOutput.set(event.session, entry);
        ctx.writeSystem(
          "warn",
          `session_output buffered: pane ${event.session} not attached yet`,
        );
      }

      entry.chunks.push(event.chunk);
      if (entry.chunks.length > MAX_PENDING_CHUNKS_PER_SESSION) {
        entry.chunks.shift();
        entry.dropped += 1;
        if (entry.dropped === 1 || entry.dropped % 64 === 0) {
          ctx.writeSystem(
            "warn",
            `session_output buffer pressure: ${event.session} shed ${entry.dropped} oldest chunks`,
          );
        }
      }
      break;
    }
    case "session_state": {
      const previous = ctx.snapshotByName.get(event.session);
      if (previous) {
        const next: SessionSnapshot = {
          ...previous,
          lifecycle_state: event.state,
          running: event.state !== "closed" && event.state !== "failed",
          last_activity_at: event.timestamp,
          last_error:
            event.state === "failed" ? event.reason : previous.last_error,
        };
        ctx.snapshotByName.set(event.session, next);
        ctx.applyPaneSnapshot(event.session, next);
      }
      ctx.writeSystem(
        "info",
        `${event.session} -> ${event.state} (${event.reason})`,
      );
      break;
    }
    case "session_exit": {
      const previous = ctx.snapshotByName.get(event.session);
      if (previous) {
        const lastError = sessionExitLastError(event);
        const next: SessionSnapshot = {
          ...previous,
          process_id: null,
          running: false,
          last_activity_at: event.timestamp,
          last_error: lastError,
        };
        ctx.snapshotByName.set(event.session, next);
        ctx.applyPaneSnapshot(event.session, next);
      }
      break;
    }
    case "session_work_state":
      if (event.state === "blocked" || event.state === "error_loop") {
        ctx.writeSystem(
          event.state === "error_loop" ? "error" : "warn",
          `${event.session} work state: ${event.state}${event.detail ? ` (${event.detail})` : ""}`,
        );
      }
      break;
    case "supervisor_heartbeat":
      break;
    case "supervisor_alert":
      ctx.writeSystem(
        event.severity === "critical" ? "error" : event.severity,
        event.message,
      );
      break;
    case "dispatch_template_warning":
      ctx.writeSystem(
        event.severity === "critical" ? "error" : event.severity,
        `dispatch template warning for ${event.session}: missing ${event.missing_patterns.join(", ")}`,
      );
      break;
    case "pair_created":
      ctx.writeSystem("info", `pair created: ${event.name}`);
      ctx.refreshSnapshotFromEvent(event.name);
      break;
    case "pair_renamed":
      ctx.writeSystem(
        "info",
        `pair renamed: ${event.old_name} -> ${event.new_name}`,
      );
      ctx.refreshSnapshotFromEvent(event.new_name);
      break;
    case "pair_deleted":
      ctx.writeSystem("info", `pair deleted: ${event.name}`);
      ctx.refreshSnapshotFromEvent();
      break;
    case "system_log":
      ctx.writeSystem(event.level, event.message);
      break;
    case "routed_message":
      ctx.writeSystem(
        "info",
        `route ${event.from} -> ${event.to} (${event.scope}): ${event.content}`,
      );
      break;
    case "route_delivery":
      if (event.phase === "failed") {
        ctx.writeSystem(
          "error",
          `route delivery failed${event.recipient ? ` for ${event.recipient}` : ""}`,
        );
      }
      break;
    case "dispatch_attempt":
      if (event.overlap) {
        ctx.writeSystem(
          "warn",
          `dispatch overlap for ${event.target_session}${event.reason ? ` (${event.reason})` : ""}`,
        );
      }
      break;
    case "pane_signal":
      ctx.writeSystem(
        event.signal_type === "blocked"
          ? "error"
          : event.signal_type === "yellow"
            ? "warn"
            : "info",
        `${event.session}: ${event.signal_type}${event.summary ? ` — ${event.summary}` : ""}`,
      );
      break;
    case "control_plane_ready":
      ctx.setControlEndpoint(event.endpoint);
      ctx.writeSystem("info", `control plane ready: ${event.endpoint}`);
      break;
    case "sideband_request_lifecycle":
      if (
        event.phase === "slow_warning" ||
        event.phase === "timed_out" ||
        event.phase === "failed"
      ) {
        ctx.writeSystem(
          event.phase === "failed" || event.phase === "timed_out"
            ? "error"
            : "warn",
          `sideband ${event.action}${event.session ? ` (${event.session})` : ""}: ${event.phase} after ${event.elapsed_ms}ms [req=${event.request_id.slice(0, 8)}]`,
        );
      }
      break;
    case "request_ack":
      break;
    case "request_ack_timeout":
      ctx.writeSystem(
        "error",
        `request acknowledgement timed out for ${event.session} (${event.action})`,
      );
      break;
    case "dispatch_no_reaction":
      ctx.writeSystem(
        "warn",
        `no reaction from ${event.session} after ${event.action}`,
      );
      break;
    default: {
      const _exhaustive: never = event;
      void _exhaustive;
      ctx.writeSystem(
        "warn",
        `unhandled runtime event variant: ${JSON.stringify(event as unknown).slice(0, 200)}`,
      );
      break;
    }
  }
}

function sessionExitLastError(
  event: Extract<RuntimeEvent, { event: "session_exit" }>,
): string | null {
  switch (event.reason) {
    case "crash_exit":
      if (event.signal !== null) {
        return `process exited after signal ${event.signal}`;
      }
      if (event.exit_code !== null) {
        return `process exited with code ${event.exit_code}`;
      }
      return "process exited unsuccessfully";
    case "pty_error":
      return "PTY error";
    case "process_disappeared":
      return "process no longer running";
    case "clean_exit":
    case "operator_stop":
    case "restart_stop":
      return null;
  }
}
