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
    pub const DEFAULT_INCLUDE_KINDS: [&'static str; 8] = [
        "session_state",
        "pair_created",
        "pair_renamed",
        "pair_deleted",
        "routed_message",
        "system_log",
        "control_plane_ready",
        "sideband_request_lifecycle",
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
        phase: SidebandPhase,
        elapsed_ms: u64,
        timestamp: String,
    },
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
    },
    SendKey {
        token: String,
        name: String,
        key: ControlKey,
    },
    RouteMessage {
        token: String,
        request: RouteMessageRequest,
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
            | Self::RouteMessage { token, .. } => token,
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
        };

        assert_eq!(request.token(), "secret");
    }

    #[test]
    fn sideband_response_serializes_without_payload_field_when_none() {
        let value = serde_json::to_value(SidebandResponse {
            ok: true,
            message: "pong".into(),
            snapshot: None,
            timed_out: false,
            payload: None,
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
    }

    #[test]
    fn sideband_response_roundtrips_timed_out_flag() {
        let response = SidebandResponse {
            ok: false,
            message: "timed out".into(),
            snapshot: None,
            timed_out: true,
            payload: None,
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
