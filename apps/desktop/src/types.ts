export type DriverKind = "claude" | "codex" | "generic_terminal";
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
  | "ack_timeout"
  | "dispatch_no_reaction"
  | "session_stall_detected"
  | "operator_attention";
export type RouteDeliveryPhase = "resolved" | "written" | "failed";
export type PaneSignalType =
  | "done"
  | "blocked"
  | "yellow"
  | "heartbeat"
  | "progress";
export type SidebandPhase =
  | "started"
  | "slow_warning"
  | "timed_out"
  | "completed"
  | "failed";
export type SessionExitReason =
  | "clean_exit"
  | "crash_exit"
  | "operator_stop"
  | "restart_stop"
  | "pty_error"
  | "process_disappeared";

export interface SessionSnapshot {
  name: string;
  title: string;
  driver: DriverKind;
  lifecycle_state: LifecycleState;
  working_dir: string;
  process_id: number | null;
  running: boolean;
  last_activity_at: string | null;
  last_error: string | null;
}

export interface ControlPlaneSnapshot {
  transport: string;
  endpoint: string;
}

export interface RuntimeSnapshot {
  sessions: SessionSnapshot[];
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
  name: string;
  extra_args?: string[];
}

export interface StopSessionRequest {
  name: string;
}

export interface RestartSessionRequest {
  name: string;
}

export interface CreatePairRequest {
  name: string;
}

export interface RenamePairRequest {
  oldName: string;
  newName: string;
}

export interface DeletePairRequest {
  name: string;
}

export interface SendInputRequest {
  name: string;
  input: string;
}

export interface RouteMessageRequest {
  from: string;
  to: string;
  scope: MessageScope;
  content: string;
}

export type RuntimeEvent =
  | {
      event: "session_output";
      session: string;
      chunk: string;
      synthetic: boolean;
      timestamp: string;
    }
  | {
      event: "session_state";
      session: string;
      state: LifecycleState;
      reason: string;
      timestamp: string;
    }
  | {
      event: "session_exit";
      session: string;
      generation: number;
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
      event: "dispatch_template_warning";
      request_id: string;
      session: string;
      detected_patterns: string[];
      missing_patterns: string[];
      severity: AlertSeverity;
      timestamp: string;
    }
  | {
      event: "pair_created";
      name: string;
      timestamp: string;
    }
  | {
      event: "pair_renamed";
      old_name: string;
      new_name: string;
      timestamp: string;
    }
  | {
      event: "pair_deleted";
      name: string;
      timestamp: string;
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
      event: "pane_signal";
      request_id: string;
      session: string;
      task_id: string;
      signal_type: PaneSignalType;
      summary: string;
      artifact_paths: string[];
      commit_sha: string | null;
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
    }
  | {
      event: "sideband_request_lifecycle";
      request_id: string;
      action: string;
      session: string | null;
      extra_args?: string[];
      phase: SidebandPhase;
      error?: string;
      elapsed_ms: number;
      timestamp: string;
    }
  | {
      event: "request_ack";
      request_id: string;
      session: string;
      action: string;
      bytes_written: number;
      timestamp: string;
    }
  | {
      event: "request_ack_timeout";
      request_id: string;
      session: string;
      action: string;
      elapsed_ms: number;
      timestamp: string;
    }
  | {
      event: "dispatch_no_reaction";
      request_id: string;
      session: string;
      action: string;
      timestamp: string;
    };
