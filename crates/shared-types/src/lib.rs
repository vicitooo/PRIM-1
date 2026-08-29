use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

pub type SessionGeneration = u64;
pub type SessionId = Uuid;
pub type RunId = Uuid;
pub type RoomId = Uuid;
pub type MessageId = Uuid;
pub type RoomRevision = u64;
pub type RoomSequence = u64;
pub const SESSION_EVENT_SCHEMA_VERSION: u32 = 1;
pub const ROOM_EVENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct RunEventIdentity {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub generation: SessionGeneration,
    pub sequence: u64,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DriverKind {
    Claude,
    Codex,
    Grok,
    Prime,
    GenericTerminal,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionProfile {
    #[default]
    Normal,
    Unsafe,
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
    Exited,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchSpecError {
    ProgramNotQualified {
        program: String,
    },
    ShellMediatedProgram {
        program: String,
    },
    UnsupportedPermissionProfile {
        driver: DriverKind,
        profile: PermissionProfile,
    },
}

impl std::fmt::Display for LaunchSpecError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProgramNotQualified { program } => write!(
                formatter,
                "launch program must be a non-empty absolute path, got '{program}'"
            ),
            Self::ShellMediatedProgram { program } => write!(
                formatter,
                "shell-mediated launch program is not allowed: '{program}'"
            ),
            Self::UnsupportedPermissionProfile { driver, profile } => write!(
                formatter,
                "permission profile {profile:?} is not supported by driver {driver:?}"
            ),
        }
    }
}

impl std::error::Error for LaunchSpecError {}

/// How a launch relates to the harness-side conversation: pin a fresh id
/// where the CLI accepts one, resume a stored conversation, or neither
/// (drivers without either capability).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessLaunchSession {
    Fresh,
    New { session_id: String },
    Resume { session_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionDefinition {
    pub session_id: SessionId,
    pub alias: String,
    pub label: String,
    pub driver: DriverKind,
    pub working_dir: String,
    pub permission_profile: PermissionProfile,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionSnapshot {
    pub session_id: SessionId,
    pub alias: String,
    pub label: String,
    pub driver: DriverKind,
    pub permission_profile: PermissionProfile,
    pub lifecycle_state: LifecycleState,
    pub working_dir: String,
    pub generation: SessionGeneration,
    pub run_id: Option<RunId>,
    pub run_event_sequence: u64,
    pub process_id: Option<u32>,
    pub running: bool,
    /// A harness conversation id is stored: the next Launch resumes it.
    pub resume_available: bool,
    /// The session had a live run when the app last went down; the UI's
    /// "continue where I left off" relaunches these on open.
    pub was_running_at_shutdown: bool,
    pub last_activity_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct RoomFeedCursor {
    pub epoch: Uuid,
    pub sequence: RoomSequence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomSnapshot {
    pub room_id: RoomId,
    pub label: String,
    pub member_ids: Vec<SessionId>,
    pub membership_revision: RoomRevision,
    pub feed_epoch: Uuid,
    pub feed_oldest_sequence: RoomSequence,
    pub feed_next_sequence: RoomSequence,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RoomMembershipAction {
    Joined,
    Removed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RoomDeliveryStatus {
    Pending,
    Written,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoomMessageSender {
    Operator {},
    Session { session_id: SessionId },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoomFeedItem {
    Message {
        message_id: MessageId,
        sender: RoomMessageSender,
        content: String,
        recipient_ids: Vec<SessionId>,
        membership_revision: RoomRevision,
    },
    Membership {
        action: RoomMembershipAction,
        session_id: SessionId,
        membership_revision: RoomRevision,
    },
    Delivery {
        message_id: MessageId,
        recipient_id: SessionId,
        status: RoomDeliveryStatus,
        bytes_written: usize,
        error: Option<String>,
        run_id: Option<RunId>,
        generation: Option<SessionGeneration>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomFeedEvent {
    pub schema_version: u32,
    pub room_id: RoomId,
    pub cursor: RoomFeedCursor,
    pub item: RoomFeedItem,
    pub timestamp: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RoomFeedGapReason {
    Evicted,
    EpochReset,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomFeedGap {
    pub reason: RoomFeedGapReason,
    pub from_sequence: Option<RoomSequence>,
    pub through_sequence: Option<RoomSequence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomFeedPage {
    pub schema_version: u32,
    pub room_id: RoomId,
    pub cursor: RoomFeedCursor,
    pub gap: Option<RoomFeedGap>,
    pub events: Vec<RoomFeedEvent>,
    pub has_more: bool,
    /// Current members with their labels, so a reader can address a session
    /// by name and map feed sender ids to names. Empty only on legacy pages.
    #[serde(default)]
    pub members: Vec<RoomMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomMember {
    pub session_id: SessionId,
    pub label: String,
    pub driver: DriverKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneStatus {
    pub transport: String,
    pub endpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlPlaneSnapshot {
    pub transport: String,
    pub endpoint: String,
}

impl From<&ControlPlaneStatus> for ControlPlaneSnapshot {
    fn from(status: &ControlPlaneStatus) -> Self {
        Self {
            transport: status.transport.clone(),
            endpoint: status.endpoint.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeSnapshot {
    pub sessions: Vec<SessionSnapshot>,
    #[serde(default)]
    pub rooms: Vec<RoomSnapshot>,
    pub workspace_preference: String,
    pub control_plane: Option<ControlPlaneSnapshot>,
    pub runtime_dir: String,
    pub audit_log_path: String,
    pub generated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OperatorRouteMessageRequest {
    pub recipient_id: SessionId,
    pub content: String,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CreateRoomRequest {
    #[serde(default)]
    pub label: Option<String>,
    pub member_ids: Vec<SessionId>,
    /// Deliver the room brief into every member's terminal automatically —
    /// on creation, when a member joins, and on a run's first idle. Off, no
    /// member is messaged; the operator can still brief one by hand.
    #[serde(default = "default_true")]
    pub brief_on_join: bool,
    /// Operator-edited brief for this room; `None` means the canonical brief.
    /// `{room_label}`, `{members}`, `{your_label}` and `{tools}` are filled
    /// in per member.
    #[serde(default)]
    pub brief_template: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RenameRoomRequest {
    pub room_id: RoomId,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MoveRoomRequest {
    pub room_id: RoomId,
    pub new_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeleteRoomRequest {
    pub room_id: RoomId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AddRoomMemberRequest {
    pub room_id: RoomId,
    pub session_id: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoveRoomMemberRequest {
    pub room_id: RoomId,
    pub session_id: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PostRoomMessageRequest {
    pub room_id: RoomId,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoomRecipientSelection {
    One { session_id: SessionId },
    All {},
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeliverRoomMessageRequest {
    pub room_id: RoomId,
    pub recipients: RoomRecipientSelection,
    pub content: String,
}

/// Deliver the canonical room brief into one member's terminal (the same
/// path as the operator's Send). The supervisor does this on join and on the
/// run's first idle; the UI's "Brief now" retries it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BriefRoomMemberRequest {
    pub room_id: RoomId,
    pub session_id: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReadRoomFeedRequest {
    pub room_id: RoomId,
    #[serde(default)]
    pub cursor: Option<RoomFeedCursor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomPostResult {
    pub room_id: RoomId,
    pub message_id: MessageId,
    pub cursor: RoomFeedCursor,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomDeliveryFailure {
    pub recipient_id: SessionId,
    pub bytes_written: usize,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomDeliveryResult {
    pub room_id: RoomId,
    pub message_id: MessageId,
    pub cursor: RoomFeedCursor,
    pub recipient_count: usize,
    pub written_count: usize,
    pub failures: Vec<RoomDeliveryFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SendInputRequest {
    pub session_id: SessionId,
    pub input: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StartSessionRequest {
    pub session_id: SessionId,
    /// Start a NEW harness conversation instead of resuming the stored one.
    /// Default false: Launch continues where the session left off.
    #[serde(default)]
    pub fresh: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StopSessionRequest {
    pub session_id: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestartSessionRequest {
    pub session_id: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CreateSessionRequest {
    #[serde(default)]
    pub label: Option<String>,
    pub driver: DriverKind,
    #[serde(default)]
    pub permission_profile: PermissionProfile,
    #[serde(default)]
    pub linux_working_directory: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RenameSessionRequest {
    pub session_id: SessionId,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SetSessionPermissionRequest {
    pub session_id: SessionId,
    pub permission_profile: PermissionProfile,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChooseSessionWorkingDirectoryRequest {
    pub session_id: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SetSessionLinuxWorkingDirectoryRequest {
    pub session_id: SessionId,
    pub linux_working_directory: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MoveSessionRequest {
    pub session_id: SessionId,
    pub new_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeleteSessionRequest {
    pub session_id: SessionId,
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
pub struct HeartbeatSessionSummary {
    pub name: String,
    pub lifecycle_state: LifecycleState,
    pub work_state: Option<WorkState>,
    pub process_id: Option<u32>,
    pub last_activity_at: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorAlertType {
    SessionStallDetected,
    OperatorAttention,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AlertSeverity {
    Info,
    Warn,
    Critical,
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
        identity: RunEventIdentity,
        session: String,
        chunk: String,
        synthetic: bool,
        timestamp: String,
    },
    SessionState {
        identity: RunEventIdentity,
        session: String,
        state: LifecycleState,
        reason: String,
        timestamp: String,
    },
    SessionExit {
        identity: RunEventIdentity,
        session: String,
        process_id: Option<u32>,
        exit_code: Option<i32>,
        signal: Option<i32>,
        success: bool,
        reason: SessionExitReason,
        requested: bool,
        timestamp: String,
    },
    SessionWorkState {
        identity: RunEventIdentity,
        session: String,
        state: WorkState,
        detail: Option<String>,
        previous_state: Option<WorkState>,
        timestamp: String,
    },
    SupervisorHeartbeat {
        wrapper_pid: u32,
        uptime_secs: u64,
        sessions: Vec<HeartbeatSessionSummary>,
        timestamp: String,
    },
    SupervisorAlert {
        alert_type: SupervisorAlertType,
        request_id: Option<String>,
        session: Option<String>,
        action: Option<String>,
        last_work_state: Option<WorkState>,
        last_session_state: Option<LifecycleState>,
        message: String,
        severity: AlertSeverity,
        timestamp: String,
    },
    SessionCreated {
        schema_version: u32,
        session: SessionSnapshot,
        timestamp: String,
    },
    SessionRenamed {
        schema_version: u32,
        session_id: SessionId,
        old_label: String,
        new_label: String,
        timestamp: String,
    },
    SessionMoved {
        schema_version: u32,
        session_id: SessionId,
        old_index: usize,
        new_index: usize,
        timestamp: String,
    },
    SessionPermissionChanged {
        schema_version: u32,
        session_id: SessionId,
        old_profile: PermissionProfile,
        new_profile: PermissionProfile,
        timestamp: String,
    },
    SessionWorkingDirectoryChanged {
        schema_version: u32,
        session_id: SessionId,
        old_working_dir: String,
        new_working_dir: String,
        timestamp: String,
    },
    SessionDeleted {
        schema_version: u32,
        session_id: SessionId,
        label: String,
        timestamp: String,
    },
    RoomCreated {
        schema_version: u32,
        room: RoomSnapshot,
        timestamp: String,
    },
    RoomRenamed {
        schema_version: u32,
        room_id: RoomId,
        old_label: String,
        new_label: String,
        timestamp: String,
    },
    RoomMoved {
        schema_version: u32,
        room_id: RoomId,
        old_index: usize,
        new_index: usize,
        timestamp: String,
    },
    RoomMemberAdded {
        schema_version: u32,
        room_id: RoomId,
        session_id: SessionId,
        membership_revision: RoomRevision,
        timestamp: String,
    },
    RoomMemberRemoved {
        schema_version: u32,
        room_id: RoomId,
        session_id: SessionId,
        membership_revision: RoomRevision,
        timestamp: String,
    },
    RoomDeleted {
        schema_version: u32,
        room_id: RoomId,
        label: String,
        timestamp: String,
    },
    RoomFeedEvent {
        feed_event: RoomFeedEvent,
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
    SystemLog {
        level: LogLevel,
        message: String,
        timestamp: String,
    },
    ControlPlaneReady {
        endpoint: String,
        transport: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SidebandRequest {
    Ping {},
    WaitQuiet {
        name: String,
        quiet_seconds: u32,
        timeout_seconds: u32,
    },
    SendInput {
        name: String,
        input: String,
    },
    SendKey {
        name: String,
        key: ControlKey,
    },
    RoomRead {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<RoomFeedCursor>,
    },
    RoomPost {
        content: String,
    },
    /// Deliver into one member's terminal (or every other member with
    /// `"all"`) exactly like the operator's Send: same gate, same framing,
    /// same receipts. `recipient` is a member session id, a member label, or
    /// `"all"`; the sender is always the calling pane.
    RoomDeliver {
        recipient: String,
        content: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SidebandResponsePayload {
    WaitQuiet { quiet_duration_ms: u64 },
    WaitQuietTimeout { last_output_age_ms: u64 },
    RoomFeed { page: RoomFeedPage },
    RoomPost { result: RoomPostResult },
    RoomDelivery { result: RoomDeliveryResult },
}

/// Optional first line of a sideband connection: the per-run pane secret the
/// supervisor minted into the pane's environment (`PRIM1_PANE_SECRET`). It
/// identifies callers the kernel cannot attribute to a pane Job (harnesses
/// whose tool processes run outside it). It is proof of *own* identity only —
/// never authority over another pane, a room, or a sender.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SidebandPreamble {
    pub secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SidebandResponse {
    pub ok: bool,
    pub message: String,
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

    fn run_event_identity(sequence: u64) -> RunEventIdentity {
        RunEventIdentity {
            session_id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            run_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            generation: 7,
            sequence,
        }
    }

    fn session_snapshot() -> SessionSnapshot {
        SessionSnapshot {
            session_id: run_event_identity(0).session_id,
            alias: "session-11111111-1111-1111-1111-111111111111".into(),
            label: "Codex".into(),
            driver: DriverKind::Codex,
            permission_profile: PermissionProfile::Normal,
            lifecycle_state: LifecycleState::Ready,
            working_dir: r"C:\work".into(),
            generation: 7,
            run_id: Some(run_event_identity(0).run_id),
            run_event_sequence: 4,
            process_id: Some(1234),
            running: true,
            last_activity_at: Some("2026-08-10T00:00:00Z".into()),
            last_error: None,
        }
    }

    #[test]
    fn session_snapshot_round_trips_run_provenance() {
        let snapshot = session_snapshot();

        let value = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(value["session_id"], json!(snapshot.session_id));
        assert_eq!(value["generation"], json!(7));
        assert_eq!(value["run_id"], json!(snapshot.run_id));
        assert_eq!(value["run_event_sequence"], json!(4));
        assert_eq!(
            serde_json::from_value::<SessionSnapshot>(value).unwrap(),
            snapshot
        );
    }

    #[test]
    fn session_output_and_state_embed_run_event_identity() {
        let events = [
            RuntimeEvent::SessionOutput {
                identity: run_event_identity(1),
                session: "codex".into(),
                chunk: "hello".into(),
                synthetic: false,
                timestamp: "2026-08-10T00:00:00Z".into(),
            },
            RuntimeEvent::SessionState {
                identity: run_event_identity(2),
                session: "codex".into(),
                state: LifecycleState::Ready,
                reason: "session ready".into(),
                timestamp: "2026-08-10T00:00:01Z".into(),
            },
        ];

        for event in events {
            let value = serde_json::to_value(&event).unwrap();
            assert_eq!(
                value["identity"]["session_id"],
                json!("11111111-1111-1111-1111-111111111111")
            );
            assert_eq!(
                value["identity"]["run_id"],
                json!("22222222-2222-2222-2222-222222222222")
            );
            assert_eq!(value["identity"]["generation"], json!(7));
            let roundtrip: RuntimeEvent = serde_json::from_value(value).unwrap();
            assert_eq!(roundtrip, event);
        }
    }

    #[test]
    fn session_catalog_mutation_events_are_versioned_and_id_bearing() {
        let session_id = run_event_identity(0).session_id;
        let events = [
            RuntimeEvent::SessionCreated {
                schema_version: SESSION_EVENT_SCHEMA_VERSION,
                session: session_snapshot(),
                timestamp: "2026-08-10T00:00:00Z".into(),
            },
            RuntimeEvent::SessionRenamed {
                schema_version: SESSION_EVENT_SCHEMA_VERSION,
                session_id,
                old_label: "Codex".into(),
                new_label: "Research".into(),
                timestamp: "2026-08-10T00:00:01Z".into(),
            },
            RuntimeEvent::SessionMoved {
                schema_version: SESSION_EVENT_SCHEMA_VERSION,
                session_id,
                old_index: 0,
                new_index: 2,
                timestamp: "2026-08-10T00:00:02Z".into(),
            },
            RuntimeEvent::SessionPermissionChanged {
                schema_version: SESSION_EVENT_SCHEMA_VERSION,
                session_id,
                old_profile: PermissionProfile::Normal,
                new_profile: PermissionProfile::Unsafe,
                timestamp: "2026-08-10T00:00:03Z".into(),
            },
            RuntimeEvent::SessionWorkingDirectoryChanged {
                schema_version: SESSION_EVENT_SCHEMA_VERSION,
                session_id,
                old_working_dir: r"C:\work".into(),
                new_working_dir: r"C:\other".into(),
                timestamp: "2026-08-10T00:00:04Z".into(),
            },
            RuntimeEvent::SessionDeleted {
                schema_version: SESSION_EVENT_SCHEMA_VERSION,
                session_id,
                label: "Research".into(),
                timestamp: "2026-08-10T00:00:05Z".into(),
            },
        ];

        for event in events {
            let value = serde_json::to_value(&event).unwrap();
            assert_eq!(value["schema_version"], json!(SESSION_EVENT_SCHEMA_VERSION));
            let encoded_session_id = value.get("session_id").or_else(|| {
                value
                    .get("session")
                    .and_then(|session| session.get("session_id"))
            });
            assert_eq!(encoded_session_id, Some(&json!(session_id)));
            assert_eq!(
                serde_json::from_value::<RuntimeEvent>(value).unwrap(),
                event
            );
        }
    }

    #[test]
    fn control_plane_status_is_endpoint_only() {
        let status = ControlPlaneStatus {
            transport: "named_pipe".into(),
            endpoint: r"\\.\pipe\prim1-pane-42".into(),
        };

        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            json!({
                "transport": "named_pipe",
                "endpoint": r"\\.\pipe\prim1-pane-42",
            })
        );
        assert!(
            serde_json::from_value::<ControlPlaneStatus>(json!({
                "transport": "named_pipe",
                "endpoint": r"\\.\pipe\prim1-pane-42",
                "token": "legacy-secret",
            }))
            .is_err()
        );
    }

    #[test]
    fn tokenless_sideband_requests_round_trip_with_minimal_shapes() {
        let cases = [
            (SidebandRequest::Ping {}, json!({ "kind": "ping" })),
            (
                SidebandRequest::WaitQuiet {
                    name: "claude".into(),
                    quiet_seconds: 3,
                    timeout_seconds: 30,
                },
                json!({
                    "kind": "wait_quiet",
                    "name": "claude",
                    "quiet_seconds": 3,
                    "timeout_seconds": 30,
                }),
            ),
            (
                SidebandRequest::SendInput {
                    name: "codex".into(),
                    input: "status".into(),
                },
                json!({
                    "kind": "send_input",
                    "name": "codex",
                    "input": "status",
                }),
            ),
            (
                SidebandRequest::SendKey {
                    name: "codex".into(),
                    key: ControlKey::Enter,
                },
                json!({
                    "kind": "send_key",
                    "name": "codex",
                    "key": "enter",
                }),
            ),
            (
                SidebandRequest::RoomRead { cursor: None },
                json!({ "kind": "room_read" }),
            ),
            (
                SidebandRequest::RoomPost {
                    content: "future only".into(),
                },
                json!({
                    "kind": "room_post",
                    "content": "future only",
                }),
            ),
        ];

        for (request, expected) in cases {
            let value = serde_json::to_value(&request).unwrap();
            assert_eq!(value, expected);
            assert!(value.get("token").is_none());

            let roundtrip: SidebandRequest = serde_json::from_value(value).unwrap();
            assert_eq!(roundtrip, request);
        }
    }

    #[test]
    fn sideband_request_rejects_legacy_authority_and_idle_fields() {
        let legacy_requests = [
            json!({ "kind": "ping", "token": "secret" }),
            json!({
                "kind": "wait_quiet",
                "token": "secret",
                "name": "claude",
                "quiet_seconds": 3,
                "timeout_seconds": 30,
            }),
            json!({
                "kind": "send_input",
                "name": "codex",
                "input": "status",
                "require_idle": true,
            }),
            json!({
                "kind": "send_key",
                "token": "secret",
                "name": "codex",
                "key": "enter",
            }),
        ];

        for request in legacy_requests {
            assert!(
                serde_json::from_value::<SidebandRequest>(request.clone()).is_err(),
                "legacy request unexpectedly decoded: {request}"
            );
        }
    }

    #[test]
    fn sideband_request_rejects_removed_variants() {
        let removed_kinds = [
            "list_sessions",
            "events_since",
            "route_message",
            "create_pair",
            "start_session",
            "stop_session",
            "restart_session",
            "deliver_message",
            "pane_signal",
        ];

        for kind in removed_kinds {
            assert!(
                serde_json::from_value::<SidebandRequest>(json!({ "kind": kind })).is_err(),
                "legacy variant {kind} unexpectedly decoded"
            );
        }
    }

    #[test]
    fn pane_room_sideband_rejects_room_and_sender_authority_fields() {
        let room_id = RoomId::new_v4();
        let session_id = SessionId::new_v4();
        for request in [
            json!({
                "kind": "room_read",
                "room_id": room_id,
            }),
            json!({
                "kind": "room_post",
                "room_id": room_id,
                "content": "hello",
            }),
            json!({
                "kind": "room_post",
                "sender": { "kind": "session", "session_id": session_id },
                "content": "hello",
            }),
        ] {
            assert!(serde_json::from_value::<SidebandRequest>(request).is_err());
        }
    }

    #[test]
    fn room_deliver_and_preamble_carry_no_room_or_sender_authority() {
        let room_id = RoomId::new_v4();
        let ok = serde_json::from_value::<SidebandRequest>(json!({
            "kind": "room_deliver",
            "recipient": "all",
            "content": "wake up",
        }))
        .unwrap();
        assert_eq!(
            ok,
            SidebandRequest::RoomDeliver {
                recipient: "all".into(),
                content: "wake up".into(),
            }
        );
        for request in [
            json!({ "kind": "room_deliver", "recipient": "all", "content": "x", "room_id": room_id }),
            json!({ "kind": "room_deliver", "recipient": "all", "content": "x", "sender": "operator" }),
            json!({ "kind": "room_deliver", "recipient": "all", "content": "x", "secret": "s" }),
            json!({ "kind": "room_deliver", "content": "x" }),
        ] {
            assert!(serde_json::from_value::<SidebandRequest>(request).is_err());
        }

        let preamble =
            serde_json::from_value::<SidebandPreamble>(json!({ "secret": "abc" })).unwrap();
        assert_eq!(preamble.secret, "abc");
        for preamble in [
            json!({ "secret": "abc", "session_id": SessionId::new_v4() }),
            json!({ "secret": "abc", "kind": "ping" }),
            json!({ "kind": "ping" }),
        ] {
            assert!(serde_json::from_value::<SidebandPreamble>(preamble).is_err());
        }
        // A legacy page without members still decodes.
        let page = serde_json::from_value::<RoomFeedPage>(json!({
            "schema_version": ROOM_EVENT_SCHEMA_VERSION,
            "room_id": room_id,
            "cursor": { "epoch": Uuid::new_v4(), "sequence": 0 },
            "gap": null,
            "events": [],
            "has_more": false,
        }))
        .unwrap();
        assert!(page.members.is_empty());
    }

    #[test]
    fn room_requests_and_events_are_strict_id_bearing_shapes() {
        let room_id = RoomId::new_v4();
        let session_id = SessionId::new_v4();
        let create: CreateRoomRequest = serde_json::from_value(json!({
            "label": "Research",
            "member_ids": [session_id, SessionId::new_v4()],
        }))
        .unwrap();
        assert_eq!(create.label.as_deref(), Some("Research"));
        assert!(
            serde_json::from_value::<CreateRoomRequest>(json!({
                "label": "Research",
                "member_ids": [session_id, SessionId::new_v4()],
                "member_names": ["claude", "codex"]
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<DeliverRoomMessageRequest>(json!({
                "room_id": room_id,
                "recipients": { "kind": "all" },
                "content": "hello",
                "sender": "operator"
            }))
            .is_err()
        );

        let feed_event = RoomFeedEvent {
            schema_version: ROOM_EVENT_SCHEMA_VERSION,
            room_id,
            cursor: RoomFeedCursor {
                epoch: Uuid::new_v4(),
                sequence: 4,
            },
            item: RoomFeedItem::Message {
                message_id: MessageId::new_v4(),
                sender: RoomMessageSender::Operator {},
                content: "hello".into(),
                recipient_ids: vec![session_id],
                membership_revision: 2,
            },
            timestamp: "2026-08-11T00:00:00Z".into(),
        };
        let event = RuntimeEvent::RoomFeedEvent {
            feed_event: feed_event.clone(),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["event"], "room_feed_event");
        assert_eq!(
            value["feed_event"]["schema_version"],
            ROOM_EVENT_SCHEMA_VERSION
        );
        assert_eq!(value["feed_event"]["room_id"], json!(room_id));
        assert_eq!(
            serde_json::from_value::<RuntimeEvent>(value).unwrap(),
            event
        );
    }

    #[test]
    fn normal_permission_profile_is_the_default() {
        assert_eq!(PermissionProfile::default(), PermissionProfile::Normal);
        assert_eq!(
            serde_json::from_value::<PermissionProfile>(json!("normal")).unwrap(),
            PermissionProfile::Normal
        );
        assert_eq!(
            serde_json::to_value(PermissionProfile::Unsafe).unwrap(),
            json!("unsafe")
        );
    }

    #[test]
    fn start_session_request_rejects_renderer_launch_arguments() {
        let session_id = SessionId::new_v4();
        let request: StartSessionRequest = serde_json::from_value(json!({
            "session_id": session_id
        }))
        .unwrap();

        assert_eq!(request.session_id, session_id);
        assert!(
            serde_json::from_value::<StartSessionRequest>(json!({
                "session_id": session_id,
                "extra_args": ["--renderer-controlled"]
            }))
            .is_err()
        );
    }

    #[test]
    fn session_crud_requests_are_strict_and_permission_defaults_to_normal() {
        let session_id = SessionId::new_v4();
        let create: CreateSessionRequest = serde_json::from_value(json!({
            "driver": "claude"
        }))
        .unwrap();
        assert_eq!(create.label, None);
        assert_eq!(create.permission_profile, PermissionProfile::Normal);
        assert_eq!(create.linux_working_directory, None);

        let prime: CreateSessionRequest = serde_json::from_value(json!({
            "driver": "prime",
            "permission_profile": "normal",
            "linux_working_directory": "/home/alice"
        }))
        .unwrap();
        assert_eq!(prime.driver, DriverKind::Prime);
        assert_eq!(
            prime.linux_working_directory.as_deref(),
            Some("/home/alice")
        );

        assert!(
            serde_json::from_value::<CreateSessionRequest>(json!({
                "driver": "codex",
                "permission_profile": "normal",
                "extra_args": ["--renderer-controlled"]
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<RenameSessionRequest>(json!({
                "session_id": session_id,
                "label": "Research",
                "name": "legacy-authority"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<SetSessionPermissionRequest>(json!({
                "session_id": session_id,
                "permission_profile": "unsafe",
                "unsafe": true
            }))
            .is_err()
        );
        assert_eq!(
            serde_json::from_value::<SetSessionLinuxWorkingDirectoryRequest>(json!({
                "session_id": session_id,
                "linux_working_directory": "/home/alice/project"
            }))
            .unwrap()
            .linux_working_directory,
            "/home/alice/project"
        );
        assert!(
            serde_json::from_value::<SetSessionLinuxWorkingDirectoryRequest>(json!({
                "session_id": session_id,
                "linux_working_directory": "/home/alice/project",
                "working_directory": "renderer-authority"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ChooseSessionWorkingDirectoryRequest>(json!({
                "session_id": session_id,
                "working_dir": "C:\\renderer-controlled"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<MoveSessionRequest>(json!({
                "session_id": session_id,
                "new_index": 2,
                "label": "Research"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<DeleteSessionRequest>(json!({
                "session_id": session_id,
                "name": "Research"
            }))
            .is_err()
        );
    }

    #[test]
    fn operator_requests_reject_name_based_authority_fields() {
        let session_id = SessionId::new_v4();

        assert!(
            serde_json::from_value::<StartSessionRequest>(json!({
                "session_id": session_id,
                "name": "codex"
            }))
            .is_err()
        );
        assert!(serde_json::from_value::<StopSessionRequest>(json!({ "name": "codex" })).is_err());
        assert!(
            serde_json::from_value::<SendInputRequest>(json!({
                "session_id": session_id,
                "name": "codex",
                "input": "hello"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<OperatorRouteMessageRequest>(json!({
                "recipient_id": session_id,
                "from": "operator",
                "to": "codex",
                "content": "hello"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<OperatorRouteMessageRequest>(json!({
                "recipient_ids": [session_id],
                "scope": "direct",
                "content": "hello"
            }))
            .is_err()
        );
    }

    #[test]
    fn sideband_response_serializes_without_payload_field_when_none() {
        let value = serde_json::to_value(SidebandResponse {
            ok: true,
            message: "pong".into(),
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
            })
        );
    }

    #[test]
    fn sideband_response_rejects_legacy_snapshot_projection() {
        let response = serde_json::from_value::<SidebandResponse>(json!({
            "ok": true,
            "message": "pong",
            "snapshot": null,
        }));

        assert!(response.is_err());
    }

    #[test]
    fn sideband_response_payload_rejects_removed_variants() {
        for kind in [
            "reserved",
            "events_since",
            "events_since_error",
            "pane_signal",
        ] {
            assert!(
                serde_json::from_value::<SidebandResponsePayload>(json!({ "kind": kind })).is_err(),
                "legacy payload {kind} unexpectedly decoded"
            );
        }
    }

    #[test]
    fn sideband_response_round_trips_minimal_wait_payloads() {
        let responses = [
            SidebandResponse {
                ok: true,
                message: "quiet".into(),
                timed_out: false,
                payload: Some(SidebandResponsePayload::WaitQuiet {
                    quiet_duration_ms: 3_000,
                }),
                request_id: Some("req-1".into()),
            },
            SidebandResponse {
                ok: false,
                message: "timed out".into(),
                timed_out: true,
                payload: Some(SidebandResponsePayload::WaitQuietTimeout {
                    last_output_age_ms: 250,
                }),
                request_id: Some("req-2".into()),
            },
        ];

        for response in responses {
            let value = serde_json::to_value(&response).unwrap();
            assert!(value.get("snapshot").is_none());
            let roundtrip: SidebandResponse = serde_json::from_value(value).unwrap();
            assert_eq!(roundtrip, response);
        }
    }

    #[test]
    fn session_work_state_event_round_trips_via_json() {
        let event = RuntimeEvent::SessionWorkState {
            identity: run_event_identity(3),
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
                "identity": {
                    "session_id": "11111111-1111-1111-1111-111111111111",
                    "run_id": "22222222-2222-2222-2222-222222222222",
                    "generation": 7,
                    "sequence": 3,
                },
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
    fn supervisor_heartbeat_event_round_trips_via_json() {
        let event = RuntimeEvent::SupervisorHeartbeat {
            wrapper_pid: 4242,
            uptime_secs: 1800,
            sessions: vec![HeartbeatSessionSummary {
                name: "codex".into(),
                lifecycle_state: LifecycleState::Ready,
                work_state: Some(WorkState::Thinking),
                process_id: Some(1234),
                last_activity_at: Some("2026-05-18T00:00:00Z".into()),
            }],
            timestamp: "2026-05-18T00:30:00Z".into(),
        };

        let value = serde_json::to_value(event.clone()).unwrap();
        assert_eq!(
            value,
            json!({
                "event": "supervisor_heartbeat",
                "wrapper_pid": 4242,
                "uptime_secs": 1800,
                "sessions": [{
                    "name": "codex",
                    "lifecycle_state": "ready",
                    "work_state": "thinking",
                    "process_id": 1234,
                    "last_activity_at": "2026-05-18T00:00:00Z",
                }],
                "timestamp": "2026-05-18T00:30:00Z",
            })
        );

        let roundtrip: RuntimeEvent = serde_json::from_value(value).unwrap();
        assert_eq!(roundtrip, event);
    }

    #[test]
    fn supervisor_alert_event_round_trips_via_json() {
        let event = RuntimeEvent::SupervisorAlert {
            alert_type: SupervisorAlertType::SessionStallDetected,
            request_id: None,
            session: Some("codex".into()),
            action: Some("restart_session".into()),
            last_work_state: Some(WorkState::ErrorLoop),
            last_session_state: Some(LifecycleState::Ready),
            message: "Session codex stayed in error_loop for 600s; issuing auto-restart".into(),
            severity: AlertSeverity::Critical,
            timestamp: "2026-05-18T00:01:00Z".into(),
        };

        let value = serde_json::to_value(event.clone()).unwrap();
        assert_eq!(
            value,
            json!({
                "event": "supervisor_alert",
                "alert_type": "session_stall_detected",
                "request_id": null,
                "session": "codex",
                "action": "restart_session",
                "last_work_state": "error_loop",
                "last_session_state": "ready",
                "message": "Session codex stayed in error_loop for 600s; issuing auto-restart",
                "severity": "critical",
                "timestamp": "2026-05-18T00:01:00Z",
            })
        );

        let roundtrip: RuntimeEvent = serde_json::from_value(value).unwrap();
        assert_eq!(roundtrip, event);
    }

    #[test]
    fn session_exit_event_round_trips_via_json() {
        let event = RuntimeEvent::SessionExit {
            identity: run_event_identity(4),
            session: "codex".into(),
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
                "identity": {
                    "session_id": "11111111-1111-1111-1111-111111111111",
                    "run_id": "22222222-2222-2222-2222-222222222222",
                    "generation": 7,
                    "sequence": 4,
                },
                "session": "codex",
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
            "target_exited",
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
    fn sideband_response_round_trips_with_request_id() {
        let response = SidebandResponse {
            ok: true,
            message: "input sent".into(),
            timed_out: false,
            payload: None,
            request_id: Some("req-1".into()),
        };

        let roundtrip: SidebandResponse =
            serde_json::from_value(serde_json::to_value(response.clone()).unwrap()).unwrap();

        assert_eq!(roundtrip, response);
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
        assert_eq!(
            serde_json::to_value(WorkState::Exited).unwrap(),
            json!("exited")
        );
    }
}
