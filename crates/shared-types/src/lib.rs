use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

pub type SessionGeneration = u64;

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DriverKind {
    Claude,
    Codex,
    GenericTerminal,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    Starting,
    Ready,
    Busy,
    Idle,
    Stalled,
    Restarting,
    Failed,
    Closed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    Idle,
    Thinking,
    ToolCall,
    Blocked,
    ErrorLoop,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MessageScope {
    Direct,
    Room,
    System,
    Private,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ControlKey {
    Enter,
    Up,
    Down,
    Left,
    Right,
    Tab,
    Esc,
    CtrlC,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvVar {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchSpec {
    pub program: String,
    pub args: Vec<String>,
    pub working_dir: String,
    pub env: Vec<EnvVar>,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionDefinition {
    pub name: String,
    pub title: String,
    pub driver: DriverKind,
    pub working_dir: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<EnvVar>,
    pub auto_start: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSnapshot {
    pub name: String,
    pub title: String,
    pub driver: DriverKind,
    pub lifecycle_state: LifecycleState,
    pub working_dir: String,
    pub process_id: Option<u32>,
    pub running: bool,
    pub last_activity_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlPlaneStatus {
    pub transport: String,
    pub endpoint: String,
    pub token: String,
    pub info_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeSnapshot {
    pub sessions: Vec<SessionSnapshot>,
    pub control_plane: Option<ControlPlaneStatus>,
    pub runtime_dir: String,
    pub audit_log_path: String,
    pub generated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteMessageRequest {
    pub from: String,
    pub to: String,
    pub scope: MessageScope,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendInputRequest {
    pub name: String,
    pub input: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StartSessionRequest {
    pub name: String,
    #[serde(default)]
    pub extra_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StopSessionRequest {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RestartSessionRequest {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreatePairRequest {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RenamePairRequest {
    pub old_name: String,
    pub new_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeletePairRequest {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliverMessageRequest {
    pub name: String,
    pub content: String,
}

/// Waits until a session has emitted no "real content" for the requested
/// quiet window. This is not equivalent to "assistant turn finished":
/// assistants may go quiet during long think phases while still in flight.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WaitQuietRequest {
    pub name: String,
    pub quiet_seconds: u32,
    pub timeout_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventCursor {
    pub audit_file: String,
    pub byte_offset: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EventFilter {
    pub include_kinds: Vec<String>,
    pub include_sessions: Vec<String>,
    pub include_scopes: Vec<String>,
}

impl EventFilter {
    pub const ALL_KINDS: &'static str = "all";
    pub const DEFAULT_INCLUDE_KINDS: [&'static str; 15] = [
        "session_state",
        "session_exit",
        "session_work_state",
        "pair_created",
        "pair_renamed",
        "pair_deleted",
        "routed_message",
        "route_delivery",
        "dispatch_attempt",
        "pane_signal",
        "system_log",
        "control_plane_ready",
        "sideband_request_lifecycle",
        "request_ack",
        "request_ack_timeout",
    ];

    pub fn includes_kind(&self, kind: &str) -> bool {
        if self
            .include_kinds
            .iter()
            .any(|candidate| candidate == Self::ALL_KINDS)
        {
            return true;
        }

        if self.include_kinds.is_empty() {
            return Self::DEFAULT_INCLUDE_KINDS.contains(&kind);
        }

        self.include_kinds.iter().any(|candidate| candidate == kind)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SessionExitReason {
    CleanExit,
    CrashExit,
    OperatorStop,
    RestartStop,
    PtyError,
    ProcessDisappeared,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RuntimeEvent {
    SessionOutput {
        session: String,
        chunk: String,
        synthetic: bool,
        timestamp: String,
    },
    SessionState {
        session: String,
        state: LifecycleState,
        reason: String,
        timestamp: String,
    },
    SessionExit {
        session: String,
        generation: u64,
        process_id: Option<u32>,
        exit_code: Option<i32>,
        signal: Option<i32>,
        success: bool,
        reason: SessionExitReason,
        requested: bool,
        timestamp: String,
    },
    SessionWorkState {
        session: String,
        state: WorkState,
        detail: Option<String>,
        previous_state: Option<WorkState>,
        timestamp: String,
    },
    PairCreated {
        name: String,
        timestamp: String,
    },
    PairRenamed {
        old_name: String,
        new_name: String,
        timestamp: String,
    },
    PairDeleted {
        name: String,
        timestamp: String,
    },
    RoutedMessage {
        id: Uuid,
        from: String,
        to: String,
        scope: MessageScope,
        content: String,
        timestamp: String,
    },
    RouteDelivery {
        request_id: String,
        route_id: String,
        from: String,
        logical_to: String,
        scope: MessageScope,
        recipient: Option<String>,
        recipient_index: u32,
        recipient_count: u32,
        payload_part_count: u32,
        phase: RouteDeliveryPhase,
        bytes_written: usize,
        error: Option<String>,
        timestamp: String,
    },
    DispatchAttempt {
        request_id: String,
        action: String,
        from: String,
        target_session: String,
        target_lifecycle_state_before: LifecycleState,
        target_work_state_before: Option<WorkState>,
        target_last_activity_at: Option<String>,
        last_route_from_target_at: Option<String>,
        overlap: bool,
        reason: Option<String>,
        timestamp: String,
    },
    PaneSignal {
        request_id: String,
        session: String,
        task_id: String,
        signal_type: PaneSignalType,
        summary: String,
        #[serde(default)]
        artifact_paths: Vec<String>,
        #[serde(default)]
        commit_sha: Option<String>,
        timestamp: String,
    },
    SystemLog {
        level: LogLevel,
        message: String,
        timestamp: String,
    },
    ControlPlaneReady {
        endpoint: String,
        transport: String,
        info_path: String,
        timestamp: String,
    },
    SidebandRequestLifecycle {
        request_id: String,
        action: String,
        session: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        extra_args: Vec<String>,
        phase: SidebandPhase,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        elapsed_ms: u64,
        timestamp: String,
    },
    RequestAck {
        request_id: String,
        session: String,
        action: String,
        bytes_written: usize,
        timestamp: String,
    },
    RequestAckTimeout {
        request_id: String,
        session: String,
        action: String,
        elapsed_ms: u64,
        timestamp: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RouteDeliveryPhase {
    Resolved,
    Written,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PaneSignalType {
    Done,
    Blocked,
    Yellow,
    Heartbeat,
    Progress,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SidebandPhase {
    Started,
    SlowWarning,
    TimedOut,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SidebandRequest {
    Ping {
        token: String,
    },
    ListSessions {
        token: String,
    },
    CreatePair {
        token: String,
        name: String,
    },
    StartSession {
        token: String,
        name: String,
        #[serde(default)]
        extra_args: Vec<String>,
    },
    StopSession {
        token: String,
        name: String,
    },
    RestartSession {
        token: String,
        name: String,
    },
    DeliverMessage {
        token: String,
        name: String,
        content: String,
        #[serde(default, skip_serializing_if = "is_false")]
        require_idle: bool,
    },
    WaitQuiet {
        token: String,
        name: String,
        quiet_seconds: u32,
        timeout_seconds: u32,
    },
    EventsSince {
        token: String,
        cursor: Option<EventCursor>,
        max_events: Option<u32>,
        max_wait_seconds: Option<u32>,
        filter: Option<EventFilter>,
    },
    SendInput {
        token: String,
        name: String,
        input: String,
        #[serde(default, skip_serializing_if = "is_false")]
        require_idle: bool,
    },
    SendKey {
        token: String,
        name: String,
        key: ControlKey,
        #[serde(default, skip_serializing_if = "is_false")]
        require_idle: bool,
    },
    RouteMessage {
        token: String,
        request: RouteMessageRequest,
        #[serde(default, skip_serializing_if = "is_false")]
        require_idle: bool,
    },
    PaneSignal {
        token: String,
        task_id: String,
        signal_type: PaneSignalType,
        summary: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        artifact_paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commit_sha: Option<String>,
    },
}

impl SidebandRequest {
    pub fn token(&self) -> &str {
        match self {
            Self::Ping { token }
            | Self::ListSessions { token }
            | Self::CreatePair { token, .. }
            | Self::StartSession { token, .. }
            | Self::StopSession { token, .. }
            | Self::RestartSession { token, .. }
            | Self::DeliverMessage { token, .. }
            | Self::WaitQuiet { token, .. }
            | Self::EventsSince { token, .. }
            | Self::SendInput { token, .. }
            | Self::SendKey { token, .. }
            | Self::RouteMessage { token, .. }
            | Self::PaneSignal { token, .. } => token,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SidebandResponsePayload {
    Reserved,
    WaitQuiet {
        quiet_duration_ms: u64,
    },
    WaitQuietTimeout {
        last_output_age_ms: u64,
    },
    EventsSince {
        events: Vec<RuntimeEvent>,
        next_cursor: EventCursor,
        gap_detected: bool,
        as_of: String,
    },
    EventsSinceError {
        echoed_cursor: serde_json::Value,
    },
    PaneSignal {
        signal_path: String,
        legacy_touch_path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SidebandResponse {
    pub ok: bool,
    pub message: String,
    pub snapshot: Option<RuntimeSnapshot>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub timed_out: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<SidebandResponsePayload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sideband_token_accessor_returns_expected_value() {
        let request = SidebandRequest::SendKey {
            token: "secret".into(),
            name: "claude".into(),
            key: ControlKey::Enter,
            require_idle: false,
        };

        assert_eq!(request.token(), "secret");
    }

    #[test]
    fn start_session_request_defaults_extra_args() {
        let request: StartSessionRequest = serde_json::from_value(json!({
            "name": "claude"
        }))
        .unwrap();

        assert_eq!(request.name, "claude");
        assert!(request.extra_args.is_empty());
    }

    #[test]
    fn sideband_start_session_extra_args_roundtrip_and_legacy_default() {
        let payload = json!({
            "kind": "start_session",
            "token": "abc",
            "name": "claude",
            "extra_args": ["--resume", "abc-123"]
        });
        let decoded: SidebandRequest = serde_json::from_value(payload.clone()).unwrap();

        match &decoded {
            SidebandRequest::StartSession {
                token,
                name,
                extra_args,
            } => {
                assert_eq!(token, "abc");
                assert_eq!(name, "claude");
                assert_eq!(
                    extra_args,
                    &vec!["--resume".to_string(), "abc-123".to_string()]
                );
            }
            other => panic!("unexpected request variant: {other:?}"),
        }
        assert_eq!(serde_json::to_value(decoded).unwrap(), payload);

        let legacy: SidebandRequest = serde_json::from_value(json!({
            "kind": "start_session",
            "token": "abc",
            "name": "claude"
        }))
        .unwrap();
        match legacy {
            SidebandRequest::StartSession { extra_args, .. } => assert!(extra_args.is_empty()),
            other => panic!("unexpected request variant: {other:?}"),
        }
    }

    #[test]
    fn sideband_response_serializes_without_payload_field_when_none() {
        let value = serde_json::to_value(SidebandResponse {
            ok: true,
            message: "pong".into(),
            snapshot: None,
            timed_out: false,
            payload: None,
            request_id: None,
        })
        .unwrap();

        assert_eq!(
            value,
            json!({
                "ok": true,
                "message": "pong",
                "snapshot": null,
            })
        );
    }

    #[test]
    fn sideband_response_deserializes_legacy_format_without_payload_field() {
        let response: SidebandResponse = serde_json::from_value(json!({
            "ok": true,
            "message": "pong",
            "snapshot": null,
        }))
        .unwrap();

        assert!(response.ok);
        assert_eq!(response.message, "pong");
        assert_eq!(response.snapshot, None);
        assert!(!response.timed_out);
        assert_eq!(response.payload, None);
        assert_eq!(response.request_id, None);
    }

    #[test]
    fn sideband_response_roundtrips_timed_out_flag() {
        let response = SidebandResponse {
            ok: false,
            message: "timed out".into(),
            snapshot: None,
            timed_out: true,
            payload: None,
            request_id: None,
        };

        let roundtrip: SidebandResponse =
            serde_json::from_value(serde_json::to_value(response.clone()).unwrap()).unwrap();

        assert_eq!(roundtrip, response);
    }

    #[test]
    fn request_ack_event_round_trips_via_json() {
        let event = RuntimeEvent::RequestAck {
            request_id: "req-1".into(),
            session: "codex".into(),
            action: "send_input".into(),
            bytes_written: 7,
            timestamp: "2026-05-17T00:00:00Z".into(),
        };

        let value = serde_json::to_value(event.clone()).unwrap();
        assert_eq!(
            value,
            json!({
                "event": "request_ack",
                "request_id": "req-1",
                "session": "codex",
                "action": "send_input",
                "bytes_written": 7,
                "timestamp": "2026-05-17T00:00:00Z",
            })
        );

        let roundtrip: RuntimeEvent = serde_json::from_value(value).unwrap();
        assert_eq!(roundtrip, event);
    }

    #[test]
    fn session_work_state_event_round_trips_via_json() {
        let event = RuntimeEvent::SessionWorkState {
            session: "codex".into(),
            state: WorkState::Thinking,
            detail: Some("Working 12s".into()),
            previous_state: Some(WorkState::Idle),
            timestamp: "2026-05-17T00:00:00Z".into(),
        };

        let value = serde_json::to_value(event.clone()).unwrap();
        assert_eq!(
            value,
            json!({
                "event": "session_work_state",
                "session": "codex",
                "state": "thinking",
                "detail": "Working 12s",
                "previous_state": "idle",
                "timestamp": "2026-05-17T00:00:00Z",
            })
        );

        let roundtrip: RuntimeEvent = serde_json::from_value(value).unwrap();
        assert_eq!(roundtrip, event);
    }

    #[test]
    fn session_exit_event_round_trips_via_json() {
        let event = RuntimeEvent::SessionExit {
            session: "codex".into(),
            generation: 7,
            process_id: Some(1234),
            exit_code: Some(1),
            signal: None,
            success: false,
            reason: SessionExitReason::CrashExit,
            requested: false,
            timestamp: "2026-05-18T00:00:00Z".into(),
        };

        let value = serde_json::to_value(event.clone()).unwrap();
        assert_eq!(
            value,
            json!({
                "event": "session_exit",
                "session": "codex",
                "generation": 7,
                "process_id": 1234,
                "exit_code": 1,
                "signal": null,
                "success": false,
                "reason": "crash_exit",
                "requested": false,
                "timestamp": "2026-05-18T00:00:00Z",
            })
        );

        let roundtrip: RuntimeEvent = serde_json::from_value(value).unwrap();
        assert_eq!(roundtrip, event);
    }

    #[test]
    fn session_exit_reason_variants_use_snake_case() {
        let cases = [
            (SessionExitReason::CleanExit, "clean_exit"),
            (SessionExitReason::CrashExit, "crash_exit"),
            (SessionExitReason::OperatorStop, "operator_stop"),
            (SessionExitReason::RestartStop, "restart_stop"),
            (SessionExitReason::PtyError, "pty_error"),
            (SessionExitReason::ProcessDisappeared, "process_disappeared"),
        ];

        for (reason, expected) in cases {
            assert_eq!(serde_json::to_value(reason).unwrap(), json!(expected));
            assert_eq!(
                serde_json::from_value::<SessionExitReason>(json!(expected)).unwrap(),
                reason
            );
        }
    }

    #[test]
    fn route_delivery_event_round_trips_via_json() {
        let event = RuntimeEvent::RouteDelivery {
            request_id: "req-1".into(),
            route_id: "route-1".into(),
            from: "claude".into(),
            logical_to: "room".into(),
            scope: MessageScope::Room,
            recipient: Some("codex".into()),
            recipient_index: 1,
            recipient_count: 2,
            payload_part_count: 3,
            phase: RouteDeliveryPhase::Written,
            bytes_written: 42,
            error: None,
            timestamp: "2026-05-17T00:00:00Z".into(),
        };

        let value = serde_json::to_value(event.clone()).unwrap();
        assert_eq!(
            value,
            json!({
                "event": "route_delivery",
                "request_id": "req-1",
                "route_id": "route-1",
                "from": "claude",
                "logical_to": "room",
                "scope": "room",
                "recipient": "codex",
                "recipient_index": 1,
                "recipient_count": 2,
                "payload_part_count": 3,
                "phase": "written",
                "bytes_written": 42,
                "error": null,
                "timestamp": "2026-05-17T00:00:00Z",
            })
        );

        let roundtrip: RuntimeEvent = serde_json::from_value(value).unwrap();
        assert_eq!(roundtrip, event);
    }

    #[test]
    fn dispatch_attempt_event_round_trips_via_json() {
        let event = RuntimeEvent::DispatchAttempt {
            request_id: "req-1".into(),
            action: "send_input".into(),
            from: "operator".into(),
            target_session: "codex".into(),
            target_lifecycle_state_before: LifecycleState::Ready,
            target_work_state_before: Some(WorkState::Thinking),
            target_last_activity_at: Some("2026-05-18T00:00:00Z".into()),
            last_route_from_target_at: Some("2026-05-18T00:00:01Z".into()),
            overlap: true,
            reason: Some("target_thinking".into()),
            timestamp: "2026-05-18T00:00:02Z".into(),
        };

        let value = serde_json::to_value(event.clone()).unwrap();
        assert_eq!(
            value,
            json!({
                "event": "dispatch_attempt",
                "request_id": "req-1",
                "action": "send_input",
                "from": "operator",
                "target_session": "codex",
                "target_lifecycle_state_before": "ready",
                "target_work_state_before": "thinking",
                "target_last_activity_at": "2026-05-18T00:00:00Z",
                "last_route_from_target_at": "2026-05-18T00:00:01Z",
                "overlap": true,
                "reason": "target_thinking",
                "timestamp": "2026-05-18T00:00:02Z",
            })
        );

        let roundtrip: RuntimeEvent = serde_json::from_value(value).unwrap();
        assert_eq!(roundtrip, event);
    }

    #[test]
    fn dispatch_attempt_reason_strings_round_trip() {
        let reasons = [
            "target_thinking",
            "target_tool_call",
            "target_blocked",
            "target_error_loop",
            "target_not_ready",
            "recent_route_from_target",
        ];

        for reason in reasons {
            let event = RuntimeEvent::DispatchAttempt {
                request_id: format!("req-{reason}"),
                action: "send_input".into(),
                from: "operator".into(),
                target_session: "codex".into(),
                target_lifecycle_state_before: LifecycleState::Ready,
                target_work_state_before: Some(WorkState::Thinking),
                target_last_activity_at: None,
                last_route_from_target_at: None,
                overlap: true,
                reason: Some(reason.into()),
                timestamp: "2026-05-18T00:00:02Z".into(),
            };

            let roundtrip: RuntimeEvent =
                serde_json::from_value(serde_json::to_value(event.clone()).unwrap()).unwrap();
            assert_eq!(roundtrip, event);
        }
    }

    #[test]
    fn pane_signal_request_round_trips_with_optional_fields() {
        let request = SidebandRequest::PaneSignal {
            token: "secret".into(),
            task_id: "task-418".into(),
            signal_type: PaneSignalType::Done,
            summary: "signal complete".into(),
            artifact_paths: vec!["evidence/one.md".into()],
            commit_sha: Some("abc123".into()),
        };

        let value = serde_json::to_value(request.clone()).unwrap();
        assert_eq!(
            value,
            json!({
                "kind": "pane_signal",
                "token": "secret",
                "task_id": "task-418",
                "signal_type": "done",
                "summary": "signal complete",
                "artifact_paths": ["evidence/one.md"],
                "commit_sha": "abc123",
            })
        );

        let roundtrip: SidebandRequest = serde_json::from_value(value).unwrap();
        assert_eq!(roundtrip, request);
    }

    #[test]
    fn pane_signal_request_defaults_absent_optional_fields() {
        let request: SidebandRequest = serde_json::from_value(json!({
            "kind": "pane_signal",
            "token": "secret",
            "task_id": "task-418",
            "signal_type": "blocked",
            "summary": "blocked on credentials"
        }))
        .unwrap();

        assert_eq!(
            request,
            SidebandRequest::PaneSignal {
                token: "secret".into(),
                task_id: "task-418".into(),
                signal_type: PaneSignalType::Blocked,
                summary: "blocked on credentials".into(),
                artifact_paths: Vec::new(),
                commit_sha: None,
            }
        );
    }

    #[test]
    fn pane_signal_event_round_trips_via_json() {
        let event = RuntimeEvent::PaneSignal {
            request_id: "req-1".into(),
            session: "codex".into(),
            task_id: "task-418".into(),
            signal_type: PaneSignalType::Yellow,
            summary: "complete with caveats".into(),
            artifact_paths: Vec::new(),
            commit_sha: None,
            timestamp: "2026-05-17T00:00:00Z".into(),
        };

        let value = serde_json::to_value(event.clone()).unwrap();
        assert_eq!(
            value,
            json!({
                "event": "pane_signal",
                "request_id": "req-1",
                "session": "codex",
                "task_id": "task-418",
                "signal_type": "yellow",
                "summary": "complete with caveats",
                "artifact_paths": [],
                "commit_sha": null,
                "timestamp": "2026-05-17T00:00:00Z",
            })
        );

        let roundtrip: RuntimeEvent = serde_json::from_value(value).unwrap();
        assert_eq!(roundtrip, event);
    }

    #[test]
    fn sideband_lifecycle_event_skips_absent_error() {
        let event = RuntimeEvent::SidebandRequestLifecycle {
            request_id: "req-1".into(),
            action: "ping".into(),
            session: None,
            extra_args: Vec::new(),
            phase: SidebandPhase::Started,
            error: None,
            elapsed_ms: 0,
            timestamp: "2026-05-17T00:00:00Z".into(),
        };

        let value = serde_json::to_value(event.clone()).unwrap();
        assert_eq!(
            value,
            json!({
                "event": "sideband_request_lifecycle",
                "request_id": "req-1",
                "action": "ping",
                "session": null,
                "phase": "started",
                "elapsed_ms": 0,
                "timestamp": "2026-05-17T00:00:00Z",
            })
        );

        let roundtrip: RuntimeEvent = serde_json::from_value(value).unwrap();
        assert_eq!(roundtrip, event);
    }

    #[test]
    fn sideband_lifecycle_event_round_trips_error() {
        let event = RuntimeEvent::SidebandRequestLifecycle {
            request_id: "req-1".into(),
            action: "route_message".into(),
            session: Some("codex".into()),
            extra_args: Vec::new(),
            phase: SidebandPhase::Failed,
            error: Some("no running recipients available for 'codex'".into()),
            elapsed_ms: 12,
            timestamp: "2026-05-17T00:00:00Z".into(),
        };

        let value = serde_json::to_value(event.clone()).unwrap();
        assert_eq!(
            value,
            json!({
                "event": "sideband_request_lifecycle",
                "request_id": "req-1",
                "action": "route_message",
                "session": "codex",
                "phase": "failed",
                "error": "no running recipients available for 'codex'",
                "elapsed_ms": 12,
                "timestamp": "2026-05-17T00:00:00Z",
            })
        );

        let roundtrip: RuntimeEvent = serde_json::from_value(value).unwrap();
        assert_eq!(roundtrip, event);
    }

    #[test]
    fn request_ack_timeout_event_round_trips_via_json() {
        let event = RuntimeEvent::RequestAckTimeout {
            request_id: "req-1".into(),
            session: "codex".into(),
            action: "send_input".into(),
            elapsed_ms: 60_000,
            timestamp: "2026-05-17T00:00:00Z".into(),
        };

        let value = serde_json::to_value(event.clone()).unwrap();
        assert_eq!(
            value,
            json!({
                "event": "request_ack_timeout",
                "request_id": "req-1",
                "session": "codex",
                "action": "send_input",
                "elapsed_ms": 60000,
                "timestamp": "2026-05-17T00:00:00Z",
            })
        );

        let roundtrip: RuntimeEvent = serde_json::from_value(value).unwrap();
        assert_eq!(roundtrip, event);
    }

    #[test]
    fn sideband_response_round_trips_with_request_id() {
        let response = SidebandResponse {
            ok: true,
            message: "input sent".into(),
            snapshot: None,
            timed_out: false,
            payload: None,
            request_id: Some("req-1".into()),
        };

        let roundtrip: SidebandResponse =
            serde_json::from_value(serde_json::to_value(response.clone()).unwrap()).unwrap();

        assert_eq!(roundtrip, response);
    }

    #[test]
    fn sideband_phase_uses_snake_case() {
        assert_eq!(
            serde_json::to_value(SidebandPhase::SlowWarning).unwrap(),
            json!("slow_warning")
        );
        assert_eq!(
            serde_json::to_value(SidebandPhase::TimedOut).unwrap(),
            json!("timed_out")
        );
    }

    #[test]
    fn route_delivery_phase_uses_snake_case() {
        assert_eq!(
            serde_json::to_value(RouteDeliveryPhase::Resolved).unwrap(),
            json!("resolved")
        );
        assert_eq!(
            serde_json::to_value(RouteDeliveryPhase::Written).unwrap(),
            json!("written")
        );
        assert_eq!(
            serde_json::to_value(RouteDeliveryPhase::Failed).unwrap(),
            json!("failed")
        );
    }

    #[test]
    fn pane_signal_type_uses_snake_case_for_all_variants() {
        assert_eq!(
            serde_json::to_value(PaneSignalType::Done).unwrap(),
            json!("done")
        );
        assert_eq!(
            serde_json::to_value(PaneSignalType::Blocked).unwrap(),
            json!("blocked")
        );
        assert_eq!(
            serde_json::to_value(PaneSignalType::Yellow).unwrap(),
            json!("yellow")
        );
        assert_eq!(
            serde_json::to_value(PaneSignalType::Heartbeat).unwrap(),
            json!("heartbeat")
        );
        assert_eq!(
            serde_json::to_value(PaneSignalType::Progress).unwrap(),
            json!("progress")
        );
    }

    #[test]
    fn work_state_uses_snake_case_for_all_variants() {
        assert_eq!(
            serde_json::to_value(WorkState::Idle).unwrap(),
            json!("idle")
        );
        assert_eq!(
            serde_json::to_value(WorkState::Thinking).unwrap(),
            json!("thinking")
        );
        assert_eq!(
            serde_json::to_value(WorkState::ToolCall).unwrap(),
            json!("tool_call")
        );
        assert_eq!(
            serde_json::to_value(WorkState::Blocked).unwrap(),
            json!("blocked")
        );
        assert_eq!(
            serde_json::to_value(WorkState::ErrorLoop).unwrap(),
            json!("error_loop")
        );
    }

    #[test]
    fn pane_signal_response_payload_round_trips() {
        let payload = SidebandResponsePayload::PaneSignal {
            signal_path: ".runtime/signals/task-418__done__20260517T000000.000Z.json".into(),
            legacy_touch_path: ".runtime/dispatch-triggers/task-418.done".into(),
        };

        let roundtrip: SidebandResponsePayload =
            serde_json::from_value(serde_json::to_value(payload.clone()).unwrap()).unwrap();

        assert_eq!(roundtrip, payload);
    }

    #[test]
    fn event_cursor_roundtrips_json() {
        let cursor = EventCursor {
            audit_file: "2026-04-18.jsonl".into(),
            byte_offset: 42,
        };

        let roundtrip: EventCursor =
            serde_json::from_value(serde_json::to_value(cursor.clone()).unwrap()).unwrap();

        assert_eq!(roundtrip, cursor);
    }

    #[test]
    fn event_cursor_rejects_legacy_date_shape() {
        let error = serde_json::from_value::<EventCursor>(json!({
            "date": "2026-04-18",
            "byte_offset": 42
        }))
        .unwrap_err();

        assert!(error.to_string().contains("audit_file"));
    }

    #[test]
    fn filter_default_excludes_session_output() {
        let filter = EventFilter::default();

        assert!(!filter.includes_kind("session_output"));
    }

    #[test]
    fn filter_default_includes_sideband_request_lifecycle() {
        let filter = EventFilter::default();

        assert!(filter.includes_kind("sideband_request_lifecycle"));
    }

    #[test]
    fn filter_default_includes_request_ack_events() {
        let filter = EventFilter::default();

        assert!(filter.includes_kind("request_ack"));
        assert!(filter.includes_kind("request_ack_timeout"));
    }

    #[test]
    fn filter_default_includes_route_delivery() {
        let filter = EventFilter::default();

        assert!(filter.includes_kind("route_delivery"));
    }

    #[test]
    fn filter_default_includes_dispatch_attempt() {
        let filter = EventFilter::default();

        assert!(filter.includes_kind("dispatch_attempt"));
    }

    #[test]
    fn filter_default_includes_pane_signal() {
        let filter = EventFilter::default();

        assert!(filter.includes_kind("pane_signal"));
    }

    #[test]
    fn filter_default_includes_session_work_state() {
        let filter = EventFilter::default();

        assert!(filter.includes_kind("session_work_state"));
    }

    #[test]
    fn filter_default_includes_session_exit() {
        let filter = EventFilter::default();

        assert!(filter.includes_kind("session_exit"));
    }

    #[test]
    fn filter_all_includes_session_output() {
        let filter = EventFilter {
            include_kinds: vec!["all".into()],
            include_sessions: Vec::new(),
            include_scopes: Vec::new(),
        };

        assert!(filter.includes_kind("session_output"));
    }

    #[test]
    fn events_since_error_roundtrips_echoed_cursor_shapes() {
        let payloads = vec![
            SidebandResponsePayload::EventsSinceError {
                echoed_cursor: json!({
                    "audit_file": "2026-04-18.jsonl",
                    "byte_offset": 12
                }),
            },
            SidebandResponsePayload::EventsSinceError {
                echoed_cursor: json!("malformed"),
            },
            SidebandResponsePayload::EventsSinceError {
                echoed_cursor: serde_json::Value::Null,
            },
        ];

        for payload in payloads {
            let roundtrip: SidebandResponsePayload =
                serde_json::from_value(serde_json::to_value(payload.clone()).unwrap()).unwrap();
            assert_eq!(roundtrip, payload);
        }
    }
}
