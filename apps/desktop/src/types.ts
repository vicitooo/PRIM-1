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

export interface ControlPlaneStatus {
  transport: string;
  endpoint: string;
  token: string;
  info_path: string;
}

export interface RuntimeSnapshot {
  sessions: SessionSnapshot[];
  control_plane: ControlPlaneStatus | null;
  runtime_dir: string;
  audit_log_path: string;
  generated_at: string;
}

export interface StartSessionRequest {
  name: string;
}

export interface StopSessionRequest {
  name: string;
}

export interface RestartSessionRequest {
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
      event: "routed_message";
      id: string;
      from: string;
      to: string;
      scope: MessageScope;
      content: string;
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
      info_path: string;
      timestamp: string;
    };
