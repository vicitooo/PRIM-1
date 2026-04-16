use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
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
            | Self::StartSession { token, .. }
            | Self::StopSession { token, .. }
            | Self::RestartSession { token, .. }
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SidebandResponse {
    pub ok: bool,
    pub message: String,
    pub snapshot: Option<RuntimeSnapshot>,
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
        assert_eq!(response.payload, None);
    }
}
