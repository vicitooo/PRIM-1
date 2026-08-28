export type DriverKind = "claude" | "codex" | "grok" | "prime" | "generic_terminal";
export type PermissionProfile = "normal" | "unsafe";
export type LifecycleState =
  | "starting"
  | "ready"
  | "busy"
  | "idle"
  | "stalled"
  | "restarting"
  | "failed"
  | "closed";
export type MessageScope = "direct" | "room" | "system" | "private";
export type LogLevel = "info" | "warn" | "error";
export type WorkState =
  | "idle"
  | "thinking"
  | "tool_call"
  | "blocked"
  | "error_loop"
  | "exited";
export type AlertSeverity = "info" | "warn" | "critical";
export type SupervisorAlertType =
  | "session_stall_detected"
  | "operator_attention";
export type RouteDeliveryPhase = "resolved" | "written" | "failed";
export type SessionExitReason =
  | "clean_exit"
  | "crash_exit"
  | "operator_stop"
  | "restart_stop"
  | "pty_error"
  | "process_disappeared";

export interface RunEventIdentity {
  session_id: string;
  run_id: string;
  generation: number;
  sequence: number;
}

export interface SessionSnapshot {
  session_id: string;
  alias: string;
  label: string;
  driver: DriverKind;
  permission_profile: PermissionProfile;
  lifecycle_state: LifecycleState;
  working_dir: string;
  generation: number;
  run_id: string | null;
  run_event_sequence: number;
  process_id: number | null;
  running: boolean;
  last_activity_at: string | null;
  last_error: string | null;
}

export interface RoomFeedCursor {
  epoch: string;
  sequence: number;
}

export interface RoomSnapshot {
  room_id: string;
  label: string;
  member_ids: string[];
  membership_revision: number;
  feed_epoch: string;
  feed_oldest_sequence: number;
  feed_next_sequence: number;
}

export type RoomMessageSender =
  | { kind: "operator" }
  | { kind: "session"; session_id: string };

export type RoomFeedItem =
  | {
      kind: "message";
      message_id: string;
      sender: RoomMessageSender;
      content: string;
      recipient_ids: string[];
      membership_revision: number;
    }
  | {
      kind: "membership";
      action: "joined" | "removed";
      session_id: string;
      membership_revision: number;
    }
  | {
      kind: "delivery";
      message_id: string;
      recipient_id: string;
      status: "pending" | "written" | "failed";
      bytes_written: number;
      error: string | null;
      run_id: string | null;
      generation: number | null;
    };

export interface RoomFeedEvent {
  schema_version: number;
  room_id: string;
  cursor: RoomFeedCursor;
  item: RoomFeedItem;
  timestamp: string;
}

export interface RoomFeedPage {
  schema_version: number;
  room_id: string;
  cursor: RoomFeedCursor;
  gap: {
    reason: "evicted" | "epoch_reset";
    from_sequence: number | null;
    through_sequence: number | null;
  } | null;
  events: RoomFeedEvent[];
  has_more: boolean;
}

export interface ControlPlaneSnapshot {
  transport: string;
  endpoint: string;
}

export interface RuntimeSnapshot {
  sessions: SessionSnapshot[];
  rooms: RoomSnapshot[];
  workspace_preference: string;
  control_plane: ControlPlaneSnapshot | null;
  runtime_dir: string;
  audit_log_path: string;
  generated_at: string;
}

export interface HeartbeatSessionSummary {
  name: string;
  lifecycle_state: LifecycleState;
  work_state: WorkState | null;
  process_id: number | null;
  last_activity_at: string | null;
}

export interface StartSessionRequest {
  session_id: string;
}

export interface StopSessionRequest {
  session_id: string;
}

export interface RestartSessionRequest {
  session_id: string;
}

export interface CreateSessionRequest {
  label?: string | null;
  driver: DriverKind;
  permission_profile: PermissionProfile;
  linux_working_directory?: string | null;
}

export interface RenameSessionRequest {
  session_id: string;
  label: string;
}

export interface ChooseSessionWorkingDirectoryRequest {
  session_id: string;
}

export interface SetSessionLinuxWorkingDirectoryRequest {
  session_id: string;
  linux_working_directory: string;
}

export interface SetSessionPermissionRequest {
  session_id: string;
  permission_profile: PermissionProfile;
}

export interface MoveSessionRequest {
  session_id: string;
  new_index: number;
}

export interface DeleteSessionRequest {
  session_id: string;
}

export interface CreateRoomRequest {
  label?: string | null;
  member_ids: string[];
  /** Deliver the room brief automatically on create / join / first idle. */
  brief_on_join: boolean;
  /** Edited brief for this room; null means the canonical brief. */
  brief_template?: string | null;
}

export interface RenameRoomRequest {
  room_id: string;
  label: string;
}

export interface MoveRoomRequest {
  room_id: string;
  new_index: number;
}

export interface DeleteRoomRequest {
  room_id: string;
}

export interface AddRoomMemberRequest {
  room_id: string;
  session_id: string;
}

export interface RemoveRoomMemberRequest {
  room_id: string;
  session_id: string;
}

export interface ReadRoomFeedRequest {
  room_id: string;
  cursor?: RoomFeedCursor | null;
}

export interface PostRoomMessageRequest {
  room_id: string;
  content: string;
}

export type RoomRecipientSelection =
  | { kind: "one"; session_id: string }
  | { kind: "all" };

export interface DeliverRoomMessageRequest {
  room_id: string;
  recipients: RoomRecipientSelection;
  content: string;
}

export interface RoomPostResult {
  room_id: string;
  message_id: string;
  cursor: RoomFeedCursor;
}

export interface RoomDeliveryFailure {
  recipient_id: string;
  bytes_written: number;
  error: string;
}

export interface RoomDeliveryResult {
  room_id: string;
  message_id: string;
  cursor: RoomFeedCursor;
  recipient_count: number;
  written_count: number;
  failures: RoomDeliveryFailure[];
}

export interface SendInputRequest {
  session_id: string;
  input: string;
}

export type RuntimeEvent =
  | {
      event: "session_output";
      identity: RunEventIdentity;
      session: string;
      chunk: string;
      synthetic: boolean;
      timestamp: string;
    }
  | {
      event: "session_state";
      identity: RunEventIdentity;
      session: string;
      state: LifecycleState;
      reason: string;
      timestamp: string;
    }
  | {
      event: "session_exit";
      identity: RunEventIdentity;
      session: string;
      process_id: number | null;
      exit_code: number | null;
      signal: number | null;
      success: boolean;
      reason: SessionExitReason;
      requested: boolean;
      timestamp: string;
    }
  | {
      event: "session_work_state";
      identity: RunEventIdentity;
      session: string;
      state: WorkState;
      detail: string | null;
      previous_state: WorkState | null;
      timestamp: string;
    }
  | {
      event: "supervisor_heartbeat";
      wrapper_pid: number;
      uptime_secs: number;
      sessions: HeartbeatSessionSummary[];
      timestamp: string;
    }
  | {
      event: "supervisor_alert";
      alert_type: SupervisorAlertType;
      request_id: string | null;
      session: string | null;
      action: string | null;
      last_work_state: WorkState | null;
      last_session_state: LifecycleState | null;
      message: string;
      severity: AlertSeverity;
      timestamp: string;
    }
  | {
      event: "session_created";
      schema_version: number;
      session: SessionSnapshot;
      timestamp: string;
    }
  | {
      event: "session_renamed";
      schema_version: number;
      session_id: string;
      old_label: string;
      new_label: string;
      timestamp: string;
    }
  | {
      event: "session_moved";
      schema_version: number;
      session_id: string;
      old_index: number;
      new_index: number;
      timestamp: string;
    }
  | {
      event: "session_permission_changed";
      schema_version: number;
      session_id: string;
      old_profile: PermissionProfile;
      new_profile: PermissionProfile;
      timestamp: string;
    }
  | {
      event: "session_working_directory_changed";
      schema_version: number;
      session_id: string;
      old_working_dir: string;
      new_working_dir: string;
      timestamp: string;
    }
  | {
      event: "session_deleted";
      schema_version: number;
      session_id: string;
      label: string;
      timestamp: string;
    }
  | {
      event: "room_created";
      schema_version: number;
      room: RoomSnapshot;
      timestamp: string;
    }
  | {
      event: "room_renamed";
      schema_version: number;
      room_id: string;
      old_label: string;
      new_label: string;
      timestamp: string;
    }
  | {
      event: "room_moved";
      schema_version: number;
      room_id: string;
      old_index: number;
      new_index: number;
      timestamp: string;
    }
  | {
      event: "room_member_added";
      schema_version: number;
      room_id: string;
      session_id: string;
      membership_revision: number;
      timestamp: string;
    }
  | {
      event: "room_member_removed";
      schema_version: number;
      room_id: string;
      session_id: string;
      membership_revision: number;
      timestamp: string;
    }
  | {
      event: "room_deleted";
      schema_version: number;
      room_id: string;
      label: string;
      timestamp: string;
    }
  | {
      event: "room_feed_event";
      feed_event: RoomFeedEvent;
    }
  | {
      event: "routed_message";
      id: string;
      from: string;
      to: string;
      scope: MessageScope;
      content: string;
      timestamp: string;
    }
  | {
      event: "route_delivery";
      request_id: string;
      route_id: string;
      from: string;
      logical_to: string;
      scope: MessageScope;
      recipient: string | null;
      recipient_index: number;
      recipient_count: number;
      payload_part_count: number;
      phase: RouteDeliveryPhase;
      bytes_written: number;
      error: string | null;
      timestamp: string;
    }
  | {
      event: "dispatch_attempt";
      request_id: string;
      action: string;
      from: string;
      target_session: string;
      target_lifecycle_state_before: LifecycleState;
      target_work_state_before: WorkState | null;
      target_last_activity_at: string | null;
      last_route_from_target_at: string | null;
      overlap: boolean;
      reason: string | null;
      timestamp: string;
    }
  | {
      event: "system_log";
      level: LogLevel;
      message: string;
      timestamp: string;
    }
  | {
      event: "control_plane_ready";
      endpoint: string;
      transport: string;
      timestamp: string;
    };
