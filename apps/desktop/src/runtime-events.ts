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

/** Pane bytes buffered while the pane is not attached yet (the boot window
    between event subscription and terminal construction). Byte-bounded, not
    chunk-bounded: a fullscreen TUI's resume replay is a differential paint
    stream, and shedding its OLDEST chunks deletes the full-repaint head — the
    surviving tail then paints sparse fragments onto a blank grid (the Grok
    scatter, 2026-08-29). Overflow therefore sheds the ENTIRE buffer and marks
    the stream for a full-repaint resync after attach; a corrupt tail is never
    written. */
export const MAX_PENDING_BYTES_PER_SESSION = 16 * 1024 * 1024;
export const MAX_BOOTSTRAP_RUN_EVENTS = 512;
// Retains exact retired lineages across invoke/event-channel reordering. Any
// lossy eviction is surfaced to the operator by flushRunEventGateWarnings.
export const MAX_RETIRED_RUN_CURSORS_PER_SESSION = 64;
const SESSION_CATALOG_EVENT_SCHEMA_VERSION = 1;

export interface PendingBuffer {
  chunks: string[];
  bytes: number;
  /** Times the entire buffer was shed on byte-cap overflow. Non-zero means
      the accumulated stream is missing its head: the flush must discard it
      and force a full repaint instead of writing a corrupt tail. */
  shed: number;
}

export interface RunEventGateState {
  initialized: boolean;
  queued: RunDerivedRuntimeEvent[];
  dropped: number;
  cursorBySessionId: Map<string, RunEventCursor>;
  retiredCursorsBySessionId: Map<string, RunEventCursor[]>;
  retiredGenerationFloorBySessionId: Map<string, number>;
  retiredCursorEvictions: number;
  reportedRetiredCursorEvictions: number;
}

interface RunEventCursor {
  runId: string;
  generation: number;
  // Snapshots do not contain output/work detail, so allocation high-water must
  // never initialize this consumption cursor.
  lastContentSequence: number;
  // Snapshots do represent lifecycle state and may safely advance this cursor.
  lastLifecycleSequence: number;
  terminal: boolean;
  contentHighWater: number | null;
}

export interface RuntimeEventContext {
  writeSystem: (level: "info" | "warn" | "error", message: string) => void;
  refreshSnapshotFromEvent: (preferredSessionId?: string) => void;
  snapshotById: Map<string, SessionSnapshot>;
  pendingOutput: Map<string, PendingBuffer>;
  runEventGate: RunEventGateState;
  /** Returns true if a pane exists for `sessionId` and the chunk was written. */
  writeToPane: (sessionId: string, chunk: string) => boolean;
  applyPaneSnapshot: (sessionId: string, snapshot: SessionSnapshot) => void;
  setControlEndpoint: (endpoint: string) => void;
  handleRoomEvent?: (event: RoomRuntimeEvent) => void;
}

export type RoomRuntimeEvent = Extract<
  RuntimeEvent,
  {
    event:
      | "room_created"
      | "room_renamed"
      | "room_moved"
      | "room_member_added"
      | "room_member_removed"
      | "room_deleted"
      | "room_feed_event";
  }
>;

export type RunDerivedRuntimeEvent = Extract<
  RuntimeEvent,
  {
    event:
      | "session_state"
      | "session_output"
      | "session_exit"
      | "session_work_state";
  }
>;

export function createRunEventGateState(): RunEventGateState {
  return {
    initialized: false,
    queued: [],
    dropped: 0,
    cursorBySessionId: new Map(),
    retiredCursorsBySessionId: new Map(),
    retiredGenerationFloorBySessionId: new Map(),
    retiredCursorEvictions: 0,
    reportedRetiredCursorEvictions: 0,
  };
}

export function forgetRunEventSession(
  sessionId: string,
  gate: RunEventGateState,
): void {
  gate.cursorBySessionId.delete(sessionId);
  gate.retiredCursorsBySessionId.delete(sessionId);
  gate.retiredGenerationFloorBySessionId.delete(sessionId);
  gate.queued = gate.queued.filter(
    (event) => event.identity.session_id !== sessionId,
  );
}

export interface RendererSessionRetirement {
  clearPending: () => void;
  disposePane: () => void;
  removeCard: () => void;
  forgetRunEventState: () => void;
  removeSnapshot: () => void;
  clearFocus: () => void;
}

export function retireRendererSession(
  retirement: RendererSessionRetirement,
): void {
  retirement.clearPending();
  retirement.disposePane();
  retirement.removeCard();
  retirement.forgetRunEventState();
  retirement.removeSnapshot();
  retirement.clearFocus();
}

export async function registerRuntimeEventsBeforeBootstrap(
  register: () => Promise<unknown>,
  bootstrap: () => Promise<void>,
): Promise<void> {
  await register();
  await bootstrap();
}

export interface SessionSnapshotReconciliation {
  accepted: boolean;
}

export function reconcileSessionSnapshot(
  incoming: SessionSnapshot,
  snapshots: Map<string, SessionSnapshot>,
  gate: RunEventGateState,
): SessionSnapshotReconciliation {
  const current = snapshots.get(incoming.session_id);
  if (!current) {
    snapshots.set(incoming.session_id, incoming);
    resetRunCursorsForSnapshot(incoming, gate);
    return { accepted: true };
  }

  if (incoming.generation < current.generation) {
    return { accepted: false };
  }

  if (incoming.generation > current.generation) {
    rotateRunCursorToRetired(current, incoming, gate);
    snapshots.set(incoming.session_id, incoming);
    seedActiveRunCursor(incoming, gate);
    return { accepted: true };
  }

  if (incoming.generation === current.generation) {
    if (incoming.run_event_sequence < current.run_event_sequence) {
      return { accepted: false };
    }
    if (incoming.run_id !== current.run_id) {
      // A terminal snapshot is the same-generation tombstone for the run that
      // produced the current cursor. Keep its sequence as the high-water mark.
      const terminalizesCurrentRun =
        current.run_id !== null &&
        incoming.run_id === null &&
        (incoming.lifecycle_state === "closed" ||
          incoming.lifecycle_state === "failed");
      if (!terminalizesCurrentRun) {
        return { accepted: false };
      }
      const cursor = cursorForActiveSnapshot(current, gate);
      cursor.terminal = true;
      cursor.lastLifecycleSequence = Math.max(
        cursor.lastLifecycleSequence,
        incoming.run_event_sequence,
      );
      cursor.contentHighWater = Math.max(
        cursor.contentHighWater ?? 0,
        incoming.run_event_sequence,
      );
    } else if (incoming.run_id === null) {
      const cursor = gate.cursorBySessionId.get(incoming.session_id);
      if (cursor?.generation === incoming.generation && cursor.terminal) {
        cursor.lastLifecycleSequence = Math.max(
          cursor.lastLifecycleSequence,
          incoming.run_event_sequence,
        );
        cursor.contentHighWater = Math.max(
          cursor.contentHighWater ?? 0,
          incoming.run_event_sequence,
        );
      }
    } else {
      const cursor = cursorForActiveSnapshot(incoming, gate);
      cursor.lastLifecycleSequence = Math.max(
        cursor.lastLifecycleSequence,
        incoming.run_event_sequence,
      );
    }
  }

  snapshots.set(incoming.session_id, incoming);
  return { accepted: true };
}

function resetRunCursorsForSnapshot(
  snapshot: SessionSnapshot,
  gate: RunEventGateState,
): void {
  gate.cursorBySessionId.delete(snapshot.session_id);
  gate.retiredCursorsBySessionId.delete(snapshot.session_id);
  gate.retiredGenerationFloorBySessionId.delete(snapshot.session_id);
  seedActiveRunCursor(snapshot, gate);
}

function seedActiveRunCursor(
  snapshot: SessionSnapshot,
  gate: RunEventGateState,
): void {
  gate.cursorBySessionId.delete(snapshot.session_id);
  if (snapshot.run_id !== null) {
    gate.cursorBySessionId.set(snapshot.session_id, {
      runId: snapshot.run_id,
      generation: snapshot.generation,
      lastContentSequence: 0,
      lastLifecycleSequence: snapshot.run_event_sequence,
      terminal: false,
      contentHighWater: null,
    });
  }
}

function rotateRunCursorToRetired(
  current: SessionSnapshot,
  incoming: SessionSnapshot,
  gate: RunEventGateState,
): void {
  let cursor = gate.cursorBySessionId.get(current.session_id);
  if (!cursor && current.run_id !== null) {
    cursor = cursorForActiveSnapshot(current, gate);
  }
  if (cursor) {
    cursor.terminal = true;
    cursor.lastLifecycleSequence = Math.max(
      cursor.lastLifecycleSequence,
      incoming.run_event_sequence,
    );
    cursor.contentHighWater = Math.max(
      cursor.contentHighWater ?? 0,
      incoming.run_event_sequence,
    );
    retireRunCursor(current.session_id, cursor, gate);
  }
  gate.cursorBySessionId.delete(current.session_id);
}

function retireRunCursor(
  sessionId: string,
  cursor: RunEventCursor,
  gate: RunEventGateState,
): void {
  let retired = gate.retiredCursorsBySessionId.get(sessionId);
  if (!retired) {
    retired = [];
    gate.retiredCursorsBySessionId.set(sessionId, retired);
  }
  const existing = retired.findIndex(
    (candidate) =>
      candidate.runId === cursor.runId &&
      candidate.generation === cursor.generation,
  );
  if (existing >= 0) {
    retired.splice(existing, 1);
  }
  retired.push(cursor);
  if (retired.length > MAX_RETIRED_RUN_CURSORS_PER_SESSION) {
    const evicted = retired.shift();
    if (evicted) {
      const priorFloor =
        gate.retiredGenerationFloorBySessionId.get(sessionId) ?? 0;
      gate.retiredGenerationFloorBySessionId.set(
        sessionId,
        Math.max(priorFloor, evicted.generation),
      );
    }
    gate.retiredCursorEvictions += 1;
  }
}

function cursorForActiveSnapshot(
  snapshot: SessionSnapshot,
  gate: RunEventGateState,
): RunEventCursor {
  if (snapshot.run_id === null) {
    throw new Error("cannot establish an active-run cursor without a run_id");
  }
  const existing = gate.cursorBySessionId.get(snapshot.session_id);
  if (
    existing?.runId === snapshot.run_id &&
    existing.generation === snapshot.generation
  ) {
    return existing;
  }
  const cursor: RunEventCursor = {
    runId: snapshot.run_id,
    generation: snapshot.generation,
    lastContentSequence: 0,
    lastLifecycleSequence: snapshot.run_event_sequence,
    terminal: false,
    contentHighWater: null,
  };
  gate.cursorBySessionId.set(snapshot.session_id, cursor);
  return cursor;
}

function isRunDerivedRuntimeEvent(
  event: RuntimeEvent,
): event is RunDerivedRuntimeEvent {
  return (
    event.event === "session_state" ||
    event.event === "session_output" ||
    event.event === "session_exit" ||
    event.event === "session_work_state"
  );
}

function acceptRunDerivedEvent(
  event: RunDerivedRuntimeEvent,
  snapshots: Map<string, SessionSnapshot>,
  gate: RunEventGateState,
): boolean {
  const current = snapshots.get(event.identity.session_id);
  if (!current) {
    return false;
  }

  const sameGeneration = event.identity.generation === current.generation;
  if (event.identity.generation < current.generation) {
    return acceptRetiredRunContent(event, current, gate);
  }
  if (sameGeneration) {
    if (current.run_id === null) {
      let cursor = gate.cursorBySessionId.get(current.session_id);
      if (
        !cursor &&
        event.event === "session_state" &&
        (event.state === "closed" || event.state === "failed") &&
        event.identity.sequence > current.run_event_sequence
      ) {
        cursor = terminalCursorFromEvent(event, event.identity.sequence);
        cursor.lastLifecycleSequence = current.run_event_sequence;
        gate.cursorBySessionId.set(current.session_id, cursor);
      }
      if (
        !cursor &&
        isContentEvent(event) &&
        event.identity.sequence <= current.run_event_sequence
      ) {
        cursor = terminalCursorFromEvent(event, current.run_event_sequence);
        gate.cursorBySessionId.set(current.session_id, cursor);
      }
      if (cursor && acceptTerminalContent(event, cursor)) {
        return true;
      }
      const mayAdvanceTerminalLifecycle =
        event.event === "session_state" &&
        (event.state === "closed" || event.state === "failed") &&
        (current.lifecycle_state !== "failed" || event.state === "failed") &&
        cursor?.terminal === true &&
        cursor.runId === event.identity.run_id &&
        cursor.generation === event.identity.generation &&
        event.identity.sequence > cursor.lastLifecycleSequence;
      if (!mayAdvanceTerminalLifecycle || !cursor) {
        return false;
      }
      cursor.lastLifecycleSequence = event.identity.sequence;
      cursor.contentHighWater = Math.max(
        cursor.contentHighWater ?? 0,
        event.identity.sequence,
      );
      snapshots.set(event.identity.session_id, {
        ...current,
        run_event_sequence: Math.max(
          current.run_event_sequence,
          event.identity.sequence,
        ),
      });
      return true;
    }
    if (current.run_id !== event.identity.run_id) {
      return false;
    }
    const activeCursor = cursorForActiveSnapshot(current, gate);
    if (activeCursor.terminal) {
      return false;
    }
    if (isContentEvent(event)) {
      if (event.identity.sequence <= activeCursor.lastContentSequence) {
        return false;
      }
      activeCursor.lastContentSequence = event.identity.sequence;
    } else {
      if (event.identity.sequence <= activeCursor.lastLifecycleSequence) {
        return false;
      }
      activeCursor.lastLifecycleSequence = event.identity.sequence;
      if (event.event === "session_exit") {
        activeCursor.terminal = true;
        activeCursor.contentHighWater = event.identity.sequence;
      }
    }
  } else if (event.event !== "session_state") {
    return false;
  } else {
    const nextSnapshot: SessionSnapshot = {
      ...current,
      generation: event.identity.generation,
      run_id: event.identity.run_id,
      run_event_sequence: event.identity.sequence,
    };
    rotateRunCursorToRetired(current, nextSnapshot, gate);
    gate.cursorBySessionId.set(current.session_id, {
      runId: event.identity.run_id,
      generation: event.identity.generation,
      lastContentSequence: 0,
      lastLifecycleSequence: event.identity.sequence,
      terminal: false,
      contentHighWater: null,
    });
  }

  snapshots.set(event.identity.session_id, {
    ...current,
    generation: event.identity.generation,
    run_id: event.identity.run_id,
    run_event_sequence: sameGeneration
      ? Math.max(current.run_event_sequence, event.identity.sequence)
      : event.identity.sequence,
  });
  return true;
}

function isContentEvent(
  event: RunDerivedRuntimeEvent,
): event is Extract<
  RunDerivedRuntimeEvent,
  { event: "session_output" | "session_work_state" }
> {
  return event.event === "session_output" || event.event === "session_work_state";
}

function terminalCursorFromEvent(
  event: RunDerivedRuntimeEvent,
  contentHighWater: number,
): RunEventCursor {
  return {
    runId: event.identity.run_id,
    generation: event.identity.generation,
    lastContentSequence: 0,
    lastLifecycleSequence: contentHighWater,
    terminal: true,
    contentHighWater,
  };
}

function acceptTerminalContent(
  event: RunDerivedRuntimeEvent,
  cursor: RunEventCursor,
): boolean {
  if (
    !isContentEvent(event) ||
    !cursor.terminal ||
    cursor.runId !== event.identity.run_id ||
    cursor.generation !== event.identity.generation ||
    cursor.contentHighWater === null ||
    event.identity.sequence > cursor.contentHighWater ||
    event.identity.sequence <= cursor.lastContentSequence
  ) {
    return false;
  }
  cursor.lastContentSequence = event.identity.sequence;
  return true;
}

function acceptRetiredRunContent(
  event: RunDerivedRuntimeEvent,
  current: SessionSnapshot,
  gate: RunEventGateState,
): boolean {
  if (!isContentEvent(event)) {
    return false;
  }
  const retired = gate.retiredCursorsBySessionId.get(current.session_id) ?? [];
  let cursor = retired.find(
    (candidate) =>
      candidate.runId === event.identity.run_id &&
      candidate.generation === event.identity.generation,
  );
  const retiredGenerationFloor =
    gate.retiredGenerationFloorBySessionId.get(current.session_id);
  if (
    !cursor &&
    (retiredGenerationFloor === undefined ||
      event.identity.generation > retiredGenerationFloor) &&
    event.identity.sequence <= current.run_event_sequence
  ) {
    cursor = terminalCursorFromEvent(event, current.run_event_sequence);
    retireRunCursor(current.session_id, cursor, gate);
  }
  return cursor ? acceptTerminalContent(event, cursor) : false;
}

function queueBootstrapRunEvent(
  event: RunDerivedRuntimeEvent,
  ctx: RuntimeEventContext,
): void {
  const gate = ctx.runEventGate;
  if (gate.queued.length === MAX_BOOTSTRAP_RUN_EVENTS) {
    gate.queued.shift();
    gate.dropped += 1;
    if (gate.dropped === 1 || gate.dropped % 64 === 0) {
      ctx.writeSystem(
        "warn",
        `run-event bootstrap queue pressure: ${gate.dropped} oldest events dropped`,
      );
    }
  }
  gate.queued.push(event);
}

export function completeInitialRunEventReconciliation(
  ctx: RuntimeEventContext,
): void {
  if (ctx.runEventGate.initialized) {
    return;
  }

  ctx.runEventGate.initialized = true;
  const queued = ctx.runEventGate.queued.splice(0);
  for (const event of queued) {
    handleRuntimeEvent(event, ctx);
  }
}

export function flushRunEventGateWarnings(ctx: RuntimeEventContext): void {
  const gate = ctx.runEventGate;
  const unreported =
    gate.retiredCursorEvictions - gate.reportedRetiredCursorEvictions;
  if (unreported <= 0) {
    return;
  }
  gate.reportedRetiredCursorEvictions = gate.retiredCursorEvictions;
  ctx.writeSystem(
    "warn",
    `run-event history pressure: ${unreported} oldest retired run cursor${unreported === 1 ? "" : "s"} evicted; exceptionally delayed terminal output may be omitted`,
  );
}

export function handleRuntimeEvent(
  event: RuntimeEvent,
  ctx: RuntimeEventContext,
): void {
  if (isRunDerivedRuntimeEvent(event)) {
    if (!ctx.runEventGate.initialized) {
      queueBootstrapRunEvent(event, ctx);
      return;
    }
    const accepted = acceptRunDerivedEvent(
      event,
      ctx.snapshotById,
      ctx.runEventGate,
    );
    flushRunEventGateWarnings(ctx);
    if (!accepted) {
      return;
    }
  }

  switch (event.event) {
    case "session_output": {
      const sessionId = event.identity.session_id;
      if (ctx.writeToPane(sessionId, event.chunk)) {
        break;
      }

      let entry = ctx.pendingOutput.get(sessionId);
      if (!entry) {
        entry = { chunks: [], bytes: 0, shed: 0 };
        ctx.pendingOutput.set(sessionId, entry);
        ctx.writeSystem(
          "warn",
          `session_output buffered: pane ${event.session} not attached yet`,
        );
      }

      entry.chunks.push(event.chunk);
      entry.bytes += event.chunk.length;
      if (entry.bytes > MAX_PENDING_BYTES_PER_SESSION) {
        // Never keep a headless tail of a TUI paint stream: shed everything
        // and let the attach-time flush trigger a full-repaint resync.
        entry.chunks = [];
        entry.bytes = 0;
        entry.shed += 1;
        ctx.writeSystem(
          "warn",
          `session_output buffer overflow: ${event.session} shed the entire buffered stream (${entry.shed}×); full repaint scheduled on attach`,
        );
      }
      break;
    }
    case "session_state": {
      const sessionId = event.identity.session_id;
      const previous = ctx.snapshotById.get(sessionId);
      if (previous) {
        const next: SessionSnapshot = {
          ...previous,
          lifecycle_state: event.state,
          running: event.state !== "closed" && event.state !== "failed",
          last_activity_at: event.timestamp,
          last_error:
            event.state === "failed" ? event.reason : previous.last_error,
        };
        ctx.snapshotById.set(sessionId, next);
        ctx.applyPaneSnapshot(sessionId, next);
      }
      ctx.writeSystem(
        "info",
        `${event.session} -> ${event.state} (${event.reason})`,
      );
      break;
    }
    case "session_exit": {
      const sessionId = event.identity.session_id;
      const previous = ctx.snapshotById.get(sessionId);
      if (previous) {
        const lastError = sessionExitLastError(event);
        const next: SessionSnapshot = {
          ...previous,
          lifecycle_state:
            event.reason === "pty_error" ? "failed" : "closed",
          run_id: null,
          process_id: null,
          running: false,
          last_activity_at: event.timestamp,
          last_error: lastError,
        };
        ctx.snapshotById.set(sessionId, next);
        ctx.applyPaneSnapshot(sessionId, next);
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
    case "session_created":
      if (!acceptSessionCatalogEventVersion(event.schema_version, ctx)) {
        break;
      }
      ctx.writeSystem("info", `session created: ${event.session.label}`);
      ctx.refreshSnapshotFromEvent(event.session.session_id);
      break;
    case "session_renamed":
      if (!acceptSessionCatalogEventVersion(event.schema_version, ctx)) {
        break;
      }
      ctx.writeSystem(
        "info",
        `session renamed: ${event.old_label} -> ${event.new_label}`,
      );
      ctx.refreshSnapshotFromEvent();
      break;
    case "session_moved":
      if (!acceptSessionCatalogEventVersion(event.schema_version, ctx)) {
        break;
      }
      ctx.writeSystem(
        "info",
        `session moved: ${event.session_id.slice(0, 8)} (${event.old_index + 1} -> ${event.new_index + 1})`,
      );
      ctx.refreshSnapshotFromEvent();
      break;
    case "session_permission_changed":
      if (!acceptSessionCatalogEventVersion(event.schema_version, ctx)) {
        break;
      }
      ctx.writeSystem(
        "info",
        `session permission changed: ${event.session_id.slice(0, 8)} (${event.old_profile} -> ${event.new_profile})`,
      );
      ctx.refreshSnapshotFromEvent();
      break;
    case "session_working_directory_changed":
      if (!acceptSessionCatalogEventVersion(event.schema_version, ctx)) {
        break;
      }
      ctx.writeSystem(
        "info",
        `session working directory changed: ${event.session_id.slice(0, 8)}`,
      );
      ctx.refreshSnapshotFromEvent();
      break;
    case "session_deleted":
      if (!acceptSessionCatalogEventVersion(event.schema_version, ctx)) {
        break;
      }
      ctx.writeSystem(
        "info",
        `session deleted: ${event.label} (${event.session_id.slice(0, 8)})`,
      );
      ctx.refreshSnapshotFromEvent();
      break;
    case "room_created":
    case "room_renamed":
    case "room_moved":
    case "room_member_added":
    case "room_member_removed":
    case "room_deleted":
    case "room_feed_event":
      if (ctx.handleRoomEvent) {
        ctx.handleRoomEvent(event);
      } else {
        ctx.writeSystem("warn", `room event ignored before room UI attachment: ${event.event}`);
      }
      break;
    case "system_log":
      ctx.writeSystem(event.level, event.message);
      break;
    case "routed_message":
      ctx.writeSystem(
        "info",
        `route pending: ${event.from} -> ${event.to} (${event.scope}); PTY completion not yet confirmed`,
      );
      break;
    case "route_delivery": {
      const route = event.route_id.slice(0, 8);
      switch (event.phase) {
        case "resolved":
          ctx.writeSystem(
            "info",
            `route ${route} pending: ${event.recipient_count} recipient${event.recipient_count === 1 ? "" : "s"} resolved; awaiting PTY write`,
          );
          break;
        case "written":
          ctx.writeSystem(
            "info",
            `route ${route} PTY write completed${event.recipient ? ` for ${event.recipient}` : ""} (${event.bytes_written} bytes); model receipt not confirmed`,
          );
          break;
        case "failed":
          const partialWrite =
            event.bytes_written > 0
              ? ` after ${event.bytes_written} bytes were accepted by the PTY; content may be partial and model receipt is unconfirmed`
              : "";
          ctx.writeSystem(
            "error",
            `route ${route} PTY write failed${event.recipient ? ` for ${event.recipient}` : ""}${partialWrite}: ${event.error ?? "unknown error"}`,
          );
          break;
      }
      break;
    }
    case "dispatch_attempt":
      if (event.overlap) {
        ctx.writeSystem(
          "warn",
          `dispatch overlap for ${event.target_session}${event.reason ? ` (${event.reason})` : ""}`,
        );
      }
      break;
    case "control_plane_ready":
      ctx.setControlEndpoint(event.endpoint);
      ctx.writeSystem("info", `control plane ready: ${event.endpoint}`);
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

function acceptSessionCatalogEventVersion(
  schemaVersion: number,
  ctx: RuntimeEventContext,
): boolean {
  if (schemaVersion === SESSION_CATALOG_EVENT_SCHEMA_VERSION) {
    return true;
  }
  ctx.writeSystem(
    "warn",
    `unsupported session catalog event schema ${schemaVersion}; refreshing the authoritative snapshot`,
  );
  ctx.refreshSnapshotFromEvent();
  return false;
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
