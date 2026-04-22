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
 *
 * Rule references:
 *   memory/feedback_maintainability_bar.md (non-author navigability)
 *   CLI-master-wrapper/STATE-SYNC-AUDIT-2026-04-22.md (Gap 1, Gap 2)
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
