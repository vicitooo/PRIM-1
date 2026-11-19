use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

mod maintenance;
pub use maintenance::*;
mod room_card;
pub use room_card::*;

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
/// Initial PTY columns/rows when no saved size exists. Shared with spawn and snapshots.
pub const DEFAULT_PTY_SIZE: (u16, u16) = (120, 40);

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

/// v3 W4 (opt-in, per member): how PRIM-1 connects a Codex member. `Pty` is
/// the terminal alone, as before. `AppServer` runs one private Codex App
/// Server owner per run, attaches the visible terminal to it (`--remote`) and
/// follows the member's thread from protocol facts. Omitted while `Pty`, so
/// rows written without it read the same.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum CodexTransport {
    #[default]
    Pty,
    AppServer,
}

impl CodexTransport {
    pub fn is_pty(&self) -> bool {
        *self == Self::Pty
    }
}

/// v3 W4 S5: whether PRIM-1 follows a running App Server member's thread,
/// and so whether its room messages go to the App Server or are typed.
/// Fixed for the run's own owner; restarting the member starts a new one.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppServerFollow {
    /// The owner runs; no thread is bound yet, so messages are typed.
    WaitingForThread,
    /// Bound and observed: messages go to the App Server.
    Followed,
    /// The connection was lost; one bounded reconnect to the same owner runs.
    Reconnecting,
    /// Observation ended for this run: messages are typed until a restart.
    NotFollowed,
}

/// What the operator lets one member do beyond its own room (A109). All off
/// by default: such a member keeps to the one room it was put in.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct MemberPermissions {
    /// List every room, peek at any feed, join and leave rooms, and deliver
    /// to a member of another room.
    #[serde(default)]
    pub roam: bool,
    /// Request a PRIM-1 upgrade (`upgrade_request`): PRIM-1 restarts on a
    /// tested build after a notice the operator can cancel.
    #[serde(default)]
    pub maintain: bool,
    /// Create, start, stop, restart, brief and remove members of its own room
    /// (A110); another room also needs `roam`. Never for Prime or Terminal.
    /// Omitted while off, so a row without it reads the same to A109 builds.
    #[serde(default, skip_serializing_if = "is_false")]
    pub manage: bool,
}

impl MemberPermissions {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// A16: designated Supervisor means all three grants. The operator's
    /// Post wakes such members.
    pub fn is_supervisor(&self) -> bool {
        self.roam && self.maintain && self.manage
    }
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

/// Why the last launch/run failed, when the UI can offer a specific recovery.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SessionErrorKind {
    /// Host binary was not on PATH (and no cmd shim resolved). The pane may
    /// offer a bounded search of usual install locations.
    ExecutableNotFound,
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
    /// v3 W4: Codex only; any other driver keeps `Pty`.
    #[serde(default, skip_serializing_if = "CodexTransport::is_pty")]
    pub codex_transport: CodexTransport,
}

/// Requested launch inputs, not claims about the harness's effective settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionLaunchChoices {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub brief: SessionBriefChoice,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionBriefChoice {
    RoomDefault {},
    Custom { text: String },
    None {},
}

impl Default for SessionBriefChoice {
    fn default() -> Self {
        Self::RoomDefault {}
    }
}

/// Inputs PRIM supplied, not observations of the harness's effective settings.
/// Environment values, conversation content and the initial prompt are excluded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionLaunchInputs {
    pub driver: DriverKind,
    pub program: String,
    pub prefix_args: Vec<String>,
    pub working_dir: String,
    pub permission_profile: PermissionProfile,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Only PRIM's injected pane MCP overrides; inherited configuration is unknown.
    pub pane_mcp: Option<SessionPaneMcpInputs>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionPaneMcpInputs {
    pub server_name: String,
    pub program: String,
    pub args: Vec<String>,
    /// Names only. No inherited environment values or per-run identities.
    pub environment_names: Vec<String>,
    pub allowed_tools: Vec<String>,
    pub required: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionEffectiveSettings {
    pub harness_version: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission_mode: Option<String>,
    pub mcp_config: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionLaunchRecord {
    pub run_id: RunId,
    pub generation: SessionGeneration,
    pub started_at: String,
    pub inputs: SessionLaunchInputs,
    /// Null fields mean unobserved, never an inferred default or inherited value.
    pub effective: SessionEffectiveSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionLaunchPreview {
    pub previous: Option<SessionLaunchRecord>,
    pub proposed: SessionLaunchInputs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionSnapshot {
    pub session_id: SessionId,
    pub alias: String,
    pub label: String,
    pub driver: DriverKind,
    pub permission_profile: PermissionProfile,
    /// v3 W4: omitted while `Pty`.
    #[serde(default, skip_serializing_if = "CodexTransport::is_pty")]
    pub codex_transport: CodexTransport,
    #[serde(default)]
    pub launch_choices: SessionLaunchChoices,
    /// Last successful spawn; absent for legacy rows and launches without pins.
    #[serde(default)]
    pub last_launch: Option<SessionLaunchRecord>,
    /// Conversation hold, separate from a process/launch failure.
    #[serde(default)]
    pub recovery: Option<SessionRecovery>,
    /// Superseded recovery references retained when starting fresh.
    #[serde(default)]
    pub recovery_history: Vec<SessionRecovery>,
    pub lifecycle_state: LifecycleState,
    pub working_dir: String,
    pub generation: SessionGeneration,
    pub run_id: Option<RunId>,
    pub run_event_sequence: u64,
    /// Authoritative columns/rows for initial terminal parsing. None only in legacy snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pty_size: Option<(u16, u16)>,
    pub process_id: Option<u32>,
    pub running: bool,
    /// Process ended; retained run identity is draining its PTY reader only.
    #[serde(default)]
    pub output_draining: bool,
    /// An active in-process crash-restart backoff that the operator can cancel
    /// with Stop. Historical recovery tokens do not set this projection.
    #[serde(default)]
    pub crash_respawn_waiting: bool,
    /// A stored conversation reference exists without a known recovery hold.
    pub resume_available: bool,
    /// Historical activity only. Restart selection is explicit room policy,
    /// never inferred from visiting a room or this flag alone.
    pub was_running_at_shutdown: bool,
    pub last_activity_at: Option<String>,
    pub last_error: Option<String>,
    /// Set with `last_error` when the UI can offer a specific recovery.
    #[serde(default)]
    pub last_error_kind: Option<SessionErrorKind>,
    /// Operator grants beyond the member's own room; absent in legacy snapshots.
    #[serde(default)]
    pub permissions: MemberPermissions,
    /// Set while the running member waits on a person (work state Blocked,
    /// such as a startup trust question, or an error loop): what PRIM-1 saw
    /// and what clears it, the plain operator line. Its lifecycle state alone
    /// reads Ready or Idle meanwhile. Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    /// The running member's observed work state (A16 D5). Its lifecycle reads
    /// Ready while it thinks or runs a tool, so this tells working from
    /// waiting. Absent before the run's first observation and when stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_state: Option<WorkState>,
    /// v3 W2: the work state is busy only because the member's own record
    /// (its Codex rollout) shows a turn still open while its screen reads
    /// quiet. The UI says so instead of a plain Working. Absent otherwise.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub work_state_from_record: bool,
    /// v3 W4 S5: set only while an App Server member runs (its run's own
    /// observer); absent for every other member and when stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_server: Option<AppServerFollow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionRecovery {
    pub original_id: Option<String>,
    pub provenance: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct RoomFeedCursor {
    pub epoch: Uuid,
    pub sequence: RoomSequence,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RoomWriterRecoveryKind { Transient, Structural }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomWriterRecovery {
    pub kind: RoomWriterRecoveryKind,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomSnapshot {
    pub room_id: RoomId,
    pub label: String,
    /// Explicit operator choice; visiting a room never changes this policy.
    #[serde(default)]
    pub resume_on_restart: bool,
    pub member_ids: Vec<SessionId>,
    pub membership_revision: RoomRevision,
    pub feed_epoch: Uuid,
    pub feed_oldest_sequence: RoomSequence,
    pub feed_next_sequence: RoomSequence,
    /// This room's journal could not be opened/replayed. Room traffic is held;
    /// unrelated rooms and the member terminals remain available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_error: Option<String>,
    /// An owned writer needs replay before any new append. The next authorized
    /// Post/Send/Retry or add replays it in place; no terminal input is repeated.
    #[serde(default)]
    pub history_replay_pending: bool,
    /// Failure of an already-owned writer. Its acknowledged prefix remains
    /// readable. Structural damage refuses further automatic replay in this run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writer_recovery: Option<RoomWriterRecovery>,
    /// Earlier legacy history was unavailable and an empty history was linked.
    /// Informational: this does not disable room traffic.
    #[serde(default)]
    pub history_restarted: bool,
    /// Outside agents invited into this room. Never members: no terminal,
    /// no wake, no Send target.
    #[serde(default)]
    pub guests: Vec<RoomGuest>,
}

/// An outside agent (Claude desktop, Codex, a script…) invited into exactly
/// one room. Its token is shown once at invitation and stored only hashed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomGuest {
    pub guest_id: Uuid,
    pub label: String,
    pub room_id: RoomId,
    pub created_at: String,
    #[serde(default)]
    pub last_seen: Option<String>,
}

/// `invite_room_guest`: the token appears here and nowhere else.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomGuestInvitation {
    pub guest: RoomGuest,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InviteRoomGuestRequest {
    pub room_id: RoomId,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RevokeRoomGuestRequest {
    pub guest_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ListRoomGuestsRequest {
    pub room_id: RoomId,
}

/// `<runtime>/guest-endpoint.json`, written once the pipe accepts and removed
/// at shutdown. It grants nothing: the invitation token is the authority.
pub const GUEST_ENDPOINT_FILE_NAME: &str = "guest-endpoint.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GuestEndpoint {
    pub endpoint: String,
    pub server_pid: u32,
    /// The server process's creation time (Windows FILETIME), checked by the
    /// client so a reused process id cannot answer for PRIM-1.
    pub server_started: u64,
}

/// Room save (A92): what one `.prim1room` file holds. `paused_ms` is how long
/// room traffic waited while the feed was copied.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomExportReport {
    pub path: String,
    pub label: String,
    pub members: usize,
    pub records: u64,
    pub bytes: u64,
    pub paused_ms: u64,
    pub warnings: Vec<String>,
}

/// Room load (A92): one room a file or a copied runtime offers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomSourceSummary {
    pub source_room_id: RoomId,
    pub label: String,
    pub members: usize,
    /// 0 and meaningless when `unavailable` is set.
    pub records: u64,
    /// Why this room of a runtime copy cannot be read; it cannot be chosen.
    #[serde(default)]
    pub unavailable: Option<String>,
}

/// Room load (A92): each loaded member's honest resume state. Nothing starts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RoomImportMemberState {
    Resumes,
    HistoryNeeded,
    Held,
    StartsFresh,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomImportMember {
    pub session_id: SessionId,
    pub label: String,
    pub driver: DriverKind,
    pub permission_profile: PermissionProfile,
    pub state: RoomImportMemberState,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomImportReport {
    pub room: RoomSnapshot,
    pub members: Vec<RoomImportMember>,
    pub warnings: Vec<String>,
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
    /// The message is durable; no wake outcome has been established.
    Stored,
    Pending,
    /// PTY bytes and submit handling completed; not model acceptance.
    Written,
    /// A native wake adapter acknowledged notification, not model acceptance.
    Woken,
    /// The message was included in a room_read response to this recipient.
    Fetched,
    /// Wake was refused safely; the durable message remains available.
    Held,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoomMessageSender {
    Operator {},
    Session { session_id: SessionId },
    /// An invited outside agent. The label is copied at post time, so a
    /// revoked guest's messages still render.
    Guest { guest_id: Uuid, label: String },
    /// A16: a local process of the operator's Windows user outside every
    /// pane, through the operator entry (`--prim1-operator`), named as it
    /// introduced itself plus its process id. Never the person at the screen
    /// and never a pane.
    LocalProcess { label: String },
    /// v3: PRIM-1 itself (automatic recovery notices). Only PRIM-1 writes it;
    /// it is feed-only and never anyone's instruction, the operator's included.
    Prim1 {},
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoomFeedItem {
    Card {
        request_id: String,
        card_id: Uuid,
        author: SessionId,
        card: RoomCardContent,
    },
    Message {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        continuation_origin: Option<RoomContinuationOrigin>,
        #[serde(default)]
        request_id: String,
        message_id: MessageId,
        sender: RoomMessageSender,
        content: String,
        recipient_ids: Vec<SessionId>,
        membership_revision: RoomRevision,
        /// A111: trusted scheduler provenance, never inferred from message text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fallback_origin: Option<RoomFallbackOrigin>,
        /// v3 W3: the sender asked each recipient for an ACK (ordinary
        /// addressed deliveries). Absent on legacy history and on system
        /// messages; those create no ACK obligation and keep full-body wakes.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        ack_requested: bool,
        /// v3 W5: guests this message is addressed to, resolved once at
        /// admission and stored; never a guest id cast into `recipient_ids`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        guest_recipient_ids: Vec<Uuid>,
        /// v3 W5: the addressed guests' labels at admission, in the order of
        /// `guest_recipient_ids`; history keeps them after a rename or revoke.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        guest_recipient_labels: Vec<String>,
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
        /// Positive adapter evidence that a failed attempt wrote nothing.
        /// Legacy zero counts and unknown errors are not sufficient to retry.
        #[serde(default)]
        zero_write_confirmed: bool,
        error: Option<String>,
        run_id: Option<RunId>,
        generation: Option<SessionGeneration>,
        /// v3 W3: this receipt belongs to a pointer wake typed for another
        /// message (that message's own receipt carries the typed bytes).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pointer: Option<MessageId>,
        /// v3 W3: a Failed with a confirmed zero write whose refusal clears
        /// by itself (fresh pane output before the first byte): the queue
        /// re-admits this wake. Set on the completion itself, so it survives
        /// a crash before any later record.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        readmit: bool,
        /// v3 W3 (Claude PRIM-1, 26a7f92 walk): the member had already read or
        /// disposed of every message this pointer counts. On a Failed: nothing
        /// was typed, which is not a failure, never re-admitted and never
        /// retried. On a Written: every counted request was read or disposed
        /// of by the time the receipt was recorded; whether before or after
        /// the CR is not known (Reviewer 2b567163). One extra short turn is
        /// possible.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        already_read: bool,
        /// v3 W2 #2 (Claude PRIM-1, d9613ce walk): on a Written, the wake was
        /// typed past the cap while Codex's own title still read this busy
        /// state (Working, Thinking or Action Required).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title_busy: Option<String>,
        /// v3 W4 S2: on a Written, the matching JSON-RPC success response to
        /// this attempt's own turn/start named this turn: the Codex App Server
        /// admitted it. Never terminal input, model acceptance, Fetched or
        /// ACK (Claude PRIM-1 1214431f); nothing was typed, so bytes are 0.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        app_server_turn: Option<String>,
    },
    /// v3 W3: a wake for this recipient is deferred, and why (no silent
    /// queue). Not a transport outcome: it never ends wake eligibility and is
    /// appended again only when the reason code changes.
    Waiting {
        message_id: MessageId,
        recipient_id: SessionId,
        /// `not_observed`, `modal:<kind>`, `draft_open`, `stopped`, … (no
        /// screen words).
        reason_code: String,
        /// Plain-language reason and next action, for people.
        reason: String,
        /// v3 W3b: for `submit_unconfirmed`/`submit_unknown`, the attempt
        /// token (the head's Pending cursor) this uncertainty belongs to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt: Option<RoomFeedCursor>,
    },
    /// v3 W5: a disposition of a message addressed to a guest: the guest's own
    /// ACK or decline, or the operator's or a local process's on its behalf,
    /// shown as theirs. Separate from member ACKs.
    /// v3 W3 (D6): the operator withdrew this message, for every recipient
    /// that has not disposed of it: attributed, with its reason. Durable; the
    /// recipients not yet attempted get a Held receipt with the reason, and an
    /// attempt still in flight is refused before its first byte.
    Cancel {
        request_id: String,
        message_id: MessageId,
        actor: RoomAckActor,
        reason: String,
        /// Recipients it never reached (Held now, or held or refused before).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        not_typed: Vec<SessionId>,
        /// Recipients it already reached (typed, woken, read, being typed or
        /// disposed of): the withdrawal does not undo that.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reached: Vec<SessionId>,
        /// v3 W3 (D6; Reviewer bde87a63): the reached recipients its PRIM-1
        /// notice is for, fixed when it was recorded; a replay or a recovery
        /// after a crash finishes that notice for exactly these, once.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        notified: Vec<SessionId>,
    },
    GuestAck {
        request_id: String,
        message_id: MessageId,
        guest_id: Uuid,
        /// The guest's label as the message addressed it, kept after a revoke.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        guest_label: String,
        actor: RoomGuestAckActor,
        disposition: RoomAckDisposition,
        note: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome_ref: Option<String>,
    },
    /// v3 W3b: the durable recipe of one pointer attempt, written in the same
    /// locked block right after the head's Pending and before any byte: the
    /// exact typed text (it carries the attempt token) and the conversation it
    /// was typed into. A later match is reconciled only against this recipe.
    SubmitAttempt {
        /// The pointer's head request.
        message_id: MessageId,
        recipient_id: SessionId,
        /// The head's Pending cursor: the attempt token.
        attempt: RoomFeedCursor,
        typed: String,
        driver: DriverKind,
        conversation_id: String,
        working_dir: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        /// The requests this pointer covered besides its head.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        covered: Vec<MessageId>,
    },
    /// v3 W3b: the member's own conversation record shows this attempt's exact
    /// pointer as a user prompt. Input observed — never an ACK and never model
    /// acceptance; it clears only this attempt's submission uncertainty.
    InputObserved {
        message_id: MessageId,
        recipient_id: SessionId,
        attempt: RoomFeedCursor,
        /// `claude_transcript` or `codex_rollout`.
        source: String,
        /// The matching record: `<file name>@<byte offset>`. Empty in a view
        /// for anyone but the operator and that member.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        locator: String,
    },
    /// v3 W3: a disposition of an addressed message for one recipient: the
    /// recipient's own ACK (received, and its next action) or decline, or the
    /// operator's disposition on its behalf, shown as the operator's. No
    /// transport receipt changes or erases it.
    Ack {
        request_id: String,
        message_id: MessageId,
        recipient_id: SessionId,
        actor: RoomAckActor,
        disposition: RoomAckDisposition,
        /// The next action (acked) or the reason (declined).
        note: String,
        /// A task id or other outcome link, when the work has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome_ref: Option<String>,
    },
}

/// v3 W5: who disposed of a message for a guest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoomGuestAckActor {
    /// The addressed guest itself, by its own token.
    Guest { guest_id: Uuid },
    /// The operator, for the guest.
    Operator {},
    /// A local process acting with the operator's authority, for the guest.
    LocalProcess { label: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoomAckActor {
    /// The recipient itself, as its authenticated pane.
    Session { session_id: SessionId },
    /// The operator, disposing of the message for `recipient_id`.
    Operator {},
    /// v3 W3: a local process acting with the operator's authority through
    /// the operator entry, disposing of the message for `recipient_id`;
    /// `label` is its own words plus its process id (never the person).
    LocalProcess { label: String },
    /// v3 (Claude PRIM-1, ac9f860 walk): PRIM-1 itself, disposing of an older
    /// continuation of its own that a newer one superseded for `recipient_id`.
    Prim1 {},
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RoomAckDisposition {
    /// Received; `note` names the next action. Not "done".
    Acked,
    Declined,
}

/// Native recovery intent, distinct from an operator's editable message text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomContinuationOrigin {
    pub session_id: SessionId,
    pub continuation_id: Uuid,
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
    /// Current cards, independently bounded from the event window and filtered
    /// by the reader's join floor. This snapshot may be ahead of page.cursor;
    /// never use a card's cursor to advance the feed's pagination cursor.
    #[serde(default)]
    pub cards: Vec<RoomFeedEvent>,
    #[serde(default)]
    pub cards_cursor: Option<RoomFeedCursor>,
    pub has_more: bool,
    /// Current members with their labels, so a reader can address a session
    /// by name and map feed sender ids to names. Empty only on legacy pages.
    #[serde(default)]
    pub members: Vec<RoomMember>,
    /// Outside agents invited into this room.
    #[serde(default)]
    pub guests: Vec<RoomGuest>,
    /// v3 W3: messages addressed to the reader, neither read nor disposed
    /// of, that have left the bounded event window (they stay retained until
    /// an attributed disposition), oldest first and bounded like a page. Only
    /// a member's own read returns them; never another room's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retained: Vec<RoomFeedEvent>,
    /// The reader's own receipt for each retained message (status, hold
    /// reason, waiting code, ACK), in the same order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retained_receipts: Vec<RoomDeliveryReceipt>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retained_has_more: bool,
    /// v3 W3: more owed messages remain; read again with `owed_after` set to
    /// this position to continue them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_next: Option<RoomFeedCursor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomMember {
    pub session_id: SessionId,
    pub label: String,
    pub driver: DriverKind,
}

/// One room as a roaming member's `rooms_list` shows it (A109).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomDirectoryEntry {
    pub room_id: RoomId,
    pub label: String,
    pub members: Vec<RoomMember>,
    pub guest_count: usize,
    /// The room's tail now: `room_peek` from it reads only what follows.
    pub cursor: RoomFeedCursor,
}

/// v3 W5: one room's capability directory, scoped to the caller's own room.
/// It states only what PRIM-1 knows: no paths, conversation or run ids, and
/// no claim of current route health beyond the readiness PRIM-1 computes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomCapabilityDirectory {
    pub room_id: RoomId,
    pub label: String,
    /// The PRIM-1 build serving this room (version and, when known, revision).
    pub prim1_build: String,
    pub members: Vec<RoomMemberCapability>,
    pub guests: Vec<RoomGuestCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomMemberCapability {
    pub session_id: SessionId,
    pub label: String,
    pub driver: DriverKind,
    /// Room operations this member's pane offers (MCP tool and CLI verb
    /// alike): `read`, `wait`, `post`, `deliver`, `ack`. Empty for a pane
    /// with no room tools.
    pub operations: Vec<String>,
    /// Whether PRIM-1 can type a wake pointer into this member, and if not, why.
    pub wake_supported: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_note: Option<String>,
    /// Lifecycle as PRIM-1 shows it (`running`, `stopped`, ...).
    pub state: String,
    /// Ready for a wake now, by PRIM-1's own readiness; `not_ready` says why.
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_ready: Option<String>,
    /// The harness CLI version this run's launch program reported (v3 W2);
    /// None means unknown. Never a readiness or admission input.
    pub harness_version: Option<String>,
    /// Event facts, not current health: the feed positions of this member's
    /// latest retained Fetched receipt and latest ACK or decline in this room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fetched: Option<RoomFeedCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_acked: Option<RoomFeedCursor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomGuestCapability {
    /// Opaque and stable: address a guest by this id, not by its label.
    pub guest_id: Uuid,
    pub label: String,
    /// What the guest can do through its invitation: `read`, `wait`, `post`.
    pub operations: Vec<String>,
    /// Guests are never woken; they read by poll.
    pub wake_supported: bool,
    /// Whether a member or the operator can address a message to it.
    pub addressable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<String>,
}

/// One side of a `room_join` / `room_leave`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomMembershipChange {
    pub room_id: RoomId,
    pub label: String,
    pub membership_revision: RoomRevision,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomMoveResult {
    /// The room left; None when the member came from the lobby.
    pub left: Option<RoomMembershipChange>,
    /// The room joined; None after `room_leave` (the lobby).
    pub joined: Option<RoomMembershipChange>,
    /// Notices that could not be stored. Membership changed regardless.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
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

/// One resource sample (PLAN E7). Every process PRIM-1 started, directly or
/// not, is counted in exactly one group: the PRIM-1 process itself, one live
/// run's Job, its WebView2 processes, or its other helpers (chiefly the console
/// host each terminal needs).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TelemetrySnapshot {
    pub sampled_at: String,
    /// The logical processors every CPU percentage is a share of.
    pub logical_cpus: u32,
    pub core: TelemetryUsage,
    pub webview: TelemetryUsage,
    pub helpers: TelemetryUsage,
    /// Live runs in session order.
    pub sessions: Vec<TelemetrySessionUsage>,
    pub total: TelemetryUsage,
    /// Unique listed process IDs with a failed read in at least one inventory.
    /// Memory/counts may be incomplete; run CPU still uses Job accounting.
    /// Another inventory may have successfully measured the same process.
    pub unreadable_processes: u32,
    /// Wall time this sample took to measure, in milliseconds.
    pub sampler_cost_ms: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TelemetryUsage {
    /// Share of the whole machine since the previous sample, 0 to 100 (Task
    /// Manager's convention). `None` until a recent previous sample exists.
    pub cpu_percent: Option<f64>,
    /// Private bytes (commit charge) summed over the group's processes.
    pub private_bytes: u64,
    pub processes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TelemetrySessionUsage {
    pub session_id: SessionId,
    pub label: String,
    pub driver: DriverKind,
    pub cpu_percent: Option<f64>,
    pub private_bytes: u64,
    pub processes: u32,
    /// Highest sampled CPU since the run started. Samples exist only while
    /// telemetry is on screen.
    pub peak_cpu_percent: Option<f64>,
    /// Highest private memory since the run started: the Job's own peak
    /// commit, or a higher sample.
    pub peak_private_bytes: u64,
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
    /// Legacy field name: enables the room default as an initial prompt at
    /// each selected launch. Joining a room never types a brief into a PTY.
    #[serde(default = "default_true")]
    pub brief_on_join: bool,
    /// Operator-edited brief for this room; `None` means the canonical brief.
    /// `{room_label}`, `{members}`, `{your_label}`, `{tools}` and `{roaming}`
    /// (roaming members only) are filled in per member.
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
pub struct SetRoomResumeRequest {
    pub room_id: RoomId,
    pub resume_on_restart: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReadRoomBriefRequest {
    pub room_id: RoomId,
}

/// Whole-value room brief settings; the legacy enable flag now controls launch defaults.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomBriefSettings {
    pub room_id: RoomId,
    pub brief_on_join: bool,
    pub brief_template: Option<String>,
}

pub type UpdateRoomBriefRequest = RoomBriefSettings;

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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RoomFallbackAction { Get, Set }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoomFallbackRecipientMode { #[default] All, Selected }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RoomFallbackOverrideMode { Inherit, Custom, Off }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomFallbackMemberOverride {
    pub session_id: SessionId,
    pub mode: RoomFallbackOverrideMode,
    pub interval_minutes: Option<u32>,
    pub message: Option<String>,
    pub brief: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomFallbackSettings {
    pub enabled: bool,
    pub interval_minutes: u32,
    pub message: String,
    pub recipient_mode: RoomFallbackRecipientMode,
    pub recipient_ids: Vec<SessionId>,
    pub overrides: Vec<RoomFallbackMemberOverride>,
    /// v3 W6 (J10, end of mandate): no reminder is scheduled or delivered at
    /// or after this UTC time (RFC 3339). Future when saved; once it passes,
    /// only a new explicit save starts reminders again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
}

impl Default for RoomFallbackSettings {
    fn default() -> Self {
        Self { enabled: false, interval_minutes: 30, message: String::new(),
            recipient_mode: RoomFallbackRecipientMode::All, recipient_ids: Vec::new(), overrides: Vec::new(), until: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomFallbackMemberStatus {
    pub session_id: SessionId,
    pub next_due_at: Option<String>,
    pub last_message_id: Option<MessageId>,
    pub last_status: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomFallbackSnapshot {
    pub room_id: RoomId,
    pub revision: u64,
    pub settings: RoomFallbackSettings,
    pub members: Vec<RoomFallbackMemberStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice_warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SetRoomFallbacksRequest {
    pub room_id: RoomId,
    pub revision: u64,
    pub settings: RoomFallbackSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomFallbackOrigin {
    pub revision: u64,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub generation: SessionGeneration,
    pub occurrence_id: Uuid,
    pub scheduled_at: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub room_id: RoomId,
    pub recipients: RoomRecipientSelection,
    pub content: String,
}

/// Retry one definitely unattempted wake without storing the body again.
/// v3 W3 (b): the operator's own disposition of a message for one recipient
/// it addressed (a member's label or session id, or a guest's label or id),
/// from the desktop. Exactly one of next_action or decline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DisposeRoomMessageRequest {
    pub room_id: RoomId,
    pub message_id: MessageId,
    pub recipient: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// v3 W3 (D6): the operator withdraws a message from the desktop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CancelRoomMessageRequest {
    pub room_id: RoomId,
    pub message_id: MessageId,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// A receipt cursor is the attempt token: stale/repeated cursors never wake.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetryRoomDeliveryRequest {
    pub room_id: RoomId,
    pub message_id: MessageId,
    pub recipient_id: SessionId,
    pub expected_receipt: RoomFeedCursor,
}

/// Explicitly store a new room brief and attempt a wake for one member
/// through the operator Send path. Joining or idling never invokes it.
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
    pub request_id: String,
    pub replayed: bool,
    pub room_id: RoomId,
    pub message_id: MessageId,
    pub cursor: RoomFeedCursor,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomAckResult {
    pub request_id: String,
    pub replayed: bool,
    pub room_id: RoomId,
    pub message_id: MessageId,
    pub cursor: RoomFeedCursor,
}

/// v3 W3 (D6): a stored withdrawal, and the recipients it held before any attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomCancelResult {
    pub request_id: String,
    pub replayed: bool,
    pub room_id: RoomId,
    pub message_id: MessageId,
    pub cursor: RoomFeedCursor,
    /// Recipients that had no wake attempt yet and are now Held with the reason.
    pub held: Vec<SessionId>,
    /// Recipients it had already reached; the withdrawal does not undo that.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reached: Vec<SessionId>,
    /// The PRIM-1 notice that wakes the reached members with a pointer
    /// (Claude PRIM-1: no ACK asked; its Fetched receipts say who has seen it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice_message_id: Option<MessageId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomCardResult {
    pub request_id: String,
    pub replayed: bool,
    pub room_id: RoomId,
    pub card_id: Uuid,
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
    pub request_id: String,
    pub replayed: bool,
    pub receipts: Vec<RoomDeliveryReceipt>,
    pub room_id: RoomId,
    pub message_id: MessageId,
    pub cursor: RoomFeedCursor,
    pub recipient_count: usize,
    pub written_count: usize,
    pub failures: Vec<RoomDeliveryFailure>,
    /// v3 W5: one row per guest the message is addressed to. Guests are never
    /// woken; they read by poll.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guest_receipts: Vec<RoomGuestReceipt>,
}

/// v3 W5: what is known of one addressed guest's message: `stored` (it reads
/// by poll), or its own `acked` or `declined`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomGuestReceipt {
    pub guest_id: Uuid,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomDeliveryReceipt {
    pub recipient_id: SessionId,
    pub status: RoomDeliveryStatus,
    pub bytes_written: usize,
    pub error: Option<String>,
    pub run_id: Option<RunId>,
    pub generation: Option<SessionGeneration>,
    /// False means the message is durable but this wake receipt was not synced.
    pub recorded: bool,
    /// A16 (T-30): why a Stored wake is queued at delivery (an active wake or
    /// input lock, operator typing, an unsent draft, a stopped member). Only
    /// in the delivery's own result; the feed's receipts stay as they are.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_because: Option<String>,
    /// v3 W3: the latest reason code a deferred wake was recorded under, while
    /// no transport outcome has superseded it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<String>,
    /// v3 W3: this recipient's stored disposition, independent of transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack: Option<RoomAckSummary>,
    /// v3 W3: why this recipient's wake was last held, until a later write
    /// supersedes it. A later read (Fetched) never clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<String>,
    /// v3 W3: as on the Delivery item: the member had read or disposed of
    /// every message this wake counts (not typed on a Failed; on a Written,
    /// by the time the receipt was recorded, in no known order with the CR).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub already_read: bool,
    /// v3 W2 #2: as on the Delivery item: typed past the cap while Codex's
    /// own title still read this busy state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_busy: Option<String>,
    /// v3 W4 S2: as on the Delivery item: the Codex App Server's turn id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_server_turn: Option<String>,
}

/// v3 W3: a stored disposition as a delivery result reports it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomAckSummary {
    pub actor: RoomAckActor,
    pub disposition: RoomAckDisposition,
    pub note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_ref: Option<String>,
    pub cursor: RoomFeedCursor,
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
    /// Walk the usual install locations after PATH + cmd-shim miss.
    /// Default false: Launch only uses PATH and cmd shims.
    #[serde(default)]
    pub search: bool,
    /// Omitted means reuse the saved choices. Changes apply only to a stopped session.
    #[serde(default)]
    pub launch_choices: Option<SessionLaunchChoices>,
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
    /// Born already a member of this room ("+" inside a room). An unknown or
    /// full room fails the request before any session is created.
    #[serde(default)]
    pub room_id: Option<RoomId>,
    #[serde(default)]
    pub launch_choices: Option<SessionLaunchChoices>,
    /// A16: grants saved with the new session (omitted: none). The create
    /// form ticks all three for a Claude, Codex or Grok agent the operator
    /// attaches, which marks it a supervisor; Prime and Terminal refuse any.
    #[serde(default)]
    pub permissions: Option<MemberPermissions>,
    /// v3 W4: Codex only; omitted means `Pty`.
    #[serde(default, skip_serializing_if = "CodexTransport::is_pty")]
    pub codex_transport: CodexTransport,
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

/// Operator-only: the member's roam, maintain (A109) and manage (A110) grants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SetMemberPermissionsRequest {
    pub session_id: SessionId,
    pub roam: bool,
    pub maintain: bool,
    #[serde(default)]
    pub manage: bool,
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
    SessionOutputDraining {
        identity: RunEventIdentity,
        session: String,
        timestamp: String,
    },
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
    /// v3: what a member's snapshot presents changed without a work-state
    /// event (W2 #4 an advisory notice expired; J4 a provider error was set or
    /// cleared). The desktop refreshes that member's snapshot for this run.
    SessionPresentationChanged {
        session_id: SessionId,
        run_id: RunId,
        session: String,
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
    SessionMemberPermissionsChanged {
        schema_version: u32,
        session_id: SessionId,
        old_permissions: MemberPermissions,
        new_permissions: MemberPermissions,
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
    /// Invalidates room snapshots only when an owned writer changes between
    /// poisoned and recovered; it is not another durable feed receipt.
    RoomHistoryChanged {
        schema_version: u32,
        room_id: RoomId,
        timestamp: String,
    },
    RoomFeedEvent {
        feed_event: RoomFeedEvent,
    },
    /// Saved settings, next due time or an occurrence outcome changed.
    RoomFallbacksChanged {
        room_id: RoomId,
    },
    /// A room's outside agents changed: invited, revoked or renamed.
    RoomGuestsChanged {
        schema_version: u32,
        room_id: RoomId,
        guests: Vec<RoomGuest>,
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
    SystemLog {
        level: LogLevel,
        message: String,
        timestamp: String,
    },
    /// The bounded UI bridge shed display events for this run. The renderer
    /// uses this to force a full harness repaint — a diff-painting TUI never
    /// heals dropped bytes on its own.
    UiOutputGap {
        identity: RunEventIdentity,
        session: String,
        dropped_events: u64,
        timestamp: String,
    },
    ControlPlaneReady {
        endpoint: String,
        transport: String,
        timestamp: String,
    },
    /// The self-upgrade attempt changed (A108); None once nothing is shown.
    UpgradeStatusChanged {
        status: Option<UpgradeStatus>,
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
    RoomFallbacks {
        action: RoomFallbackAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_id: Option<RoomId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        settings: Option<RoomFallbackSettings>,
    },
    Ping {},
    Doctor {},
    ReportIssue { summary: String },
    /// Maintain only (A108): restart PRIM-1 on this tested build after a
    /// cancellable notice, resuming every running member's exact
    /// conversation. `task_id`/`task_revision` name the caller's checkpoint.
    UpgradeRequest {
        candidate_exe: String,
        sha256: String,
        task_id: String,
        task_revision: u64,
    },
    /// Used only by the shell helper reading its own CODEX_THREAD_ID.
    /// The supervisor requires native process/run attestation, not a pane secret.
    IdentifyCodex {
        thread_id: String,
    },
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
        /// v3 W3: the room a PRIM-1 pointer named. When the pane is no
        /// longer in it, the read is refused instead of reading another room.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect_room: Option<RoomId>,
        /// v3 W3: continue the reader's owed messages (`retained`) after this
        /// position, from a previous page's `retained_next`; bound to the
        /// room's feed epoch and refused when foreign or ahead. Never moves the
        /// chronological cursor.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owed_after: Option<RoomFeedCursor>,
    },
    /// A16 (Victor, 20 Oct: a subscription to the feed): `RoomRead` that
    /// waits until news from someone else (a message, card, membership change
    /// or gap) follows `cursor`, or `timeout_ms` passes (at most and by
    /// default 25 s). Same room, floor and Fetched receipts as `RoomRead`.
    RoomWait {
        cursor: RoomFeedCursor,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    RoomPost {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        content: String,
    },
    RoomPostCard {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        card: RoomCardContent,
    },
    /// v3 W3: the calling pane acknowledges a room message addressed to it
    /// (`next_action`) or declines it (`decline`); exactly one of the two.
    /// The actor and recipient are the calling pane, never request fields.
    RoomAck {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        message_id: MessageId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_action: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decline: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome_ref: Option<String>,
    },
    /// v3 W3: the operator entry disposes of a room message for one of its
    /// addressed recipients (`recipient`: label or session id), also one that
    /// has left the room; exactly one of `next_action` or `decline`. Recorded
    /// as the local process, never as the recipient.
    /// v3 W3 (D6): the operator entry withdraws a stored message.
    RoomCancel {
        room_id: RoomId,
        message_id: MessageId,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    RoomDispose {
        room_id: RoomId,
        message_id: MessageId,
        recipient: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_action: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decline: Option<String>,
    },
    /// Deliver into one member's terminal (or every other member with
    /// `"all"`) exactly like the operator's Send: same gate, same framing,
    /// same receipts. `recipient` is a member session id, a member label, or
    /// `"all"`; the sender is always the calling pane. A109: a recipient that
    /// is no member of the caller's room may be a roaming member that
    /// delivered to the caller recently; the answer reaches it in its room.
    RoomDeliver {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        recipient: String,
        content: String,
    },
    /// `RoomDeliver` into a named room, for a member the operator lets roam
    /// (A109): it reaches members of a room the caller is not in. Without
    /// roam it is refused; the sender is still the calling pane.
    RoomDeliverTo {
        room_id: RoomId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        recipient: String,
        content: String,
    },
    /// This pane's own grants; decides which tools its MCP server lists.
    MemberPermissions {},
    /// Roam only: every room with its members and guest count.
    RoomsList {},
    /// v3 W5: the caller's own room as a capability directory: how each
    /// member and guest can be reached and what is known of it now.
    RoomDirectory {},
    /// Roam only: another room's feed without joining it and without
    /// Fetched receipts.
    RoomPeek {
        room_id: RoomId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<RoomFeedCursor>,
    },
    /// Roam only: leave the current room (if any) for this one; both rooms
    /// get a notice.
    RoomJoin { room_id: RoomId },
    /// Roam only: leave the current room for the lobby, with a notice.
    RoomLeave {},
    /// Manage only (A110): one lifecycle action on a member of the caller's
    /// room (another room also needs roam). `request_id` is a caller-chosen
    /// UUID; a retry with it returns the first result and repeats nothing.
    Member {
        request_id: String,
        action: MemberAction,
    },
    /// Manage only (A16): every session with its room, as the operator's
    /// tab bar and lobby show them.
    MembersList {},
    /// Manage only (A16): one of the operator's room actions. `request_id`
    /// is a caller-chosen UUID for a mutation (a retry returns the first
    /// result); reads take none.
    Room {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        action: RoomAction,
    },
    /// A16: the runtime's settings (maintain) and local conversations
    /// (manage), as the operator's Settings page offers them. `request_id`
    /// is a caller-chosen UUID for a change; reads take none.
    Runtime {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        action: RuntimeAction,
    },
    /// One GraphQL request against the runtime's task folder. The actor
    /// recorded for any change is the calling pane, never a request field.
    /// Needs no room membership.
    Tasks {
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        variables: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operation_name: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SidebandResponsePayload {
    RoomFallbacks { snapshot: RoomFallbackSnapshot },
    Doctor { report: DoctorReport },
    Issue { receipt: IssueReceipt },
    Upgrade { admission: UpgradeAdmission },
    WaitQuiet { quiet_duration_ms: u64 },
    WaitQuietTimeout { last_output_age_ms: u64 },
    RoomFeed { page: RoomFeedPage },
    RoomPost { result: RoomPostResult },
    RoomCard { result: RoomCardResult },
    RoomAck { result: RoomAckResult },
    RoomCancel { result: RoomCancelResult },
    RoomDelivery { result: RoomDeliveryResult },
    /// The standard GraphQL response object: `data` and/or `errors`.
    Tasks { result: serde_json::Value },
    MemberPermissions { permissions: MemberPermissions },
    /// `current_room_id` is None while the caller is in the lobby.
    Rooms { current_room_id: Option<RoomId>, rooms: Vec<RoomDirectoryEntry> },
    RoomDirectory { directory: RoomCapabilityDirectory },
    RoomMove { result: RoomMoveResult },
    Member { result: MemberActionResult },
    Members { members: Vec<MemberDirectoryEntry> },
    Room { result: RoomActionResult },
    Runtime { result: RuntimeActionResult },
}

/// The operator's Settings actions for agents (A16).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeAction {
    /// Task folder, personal repository and default workspace (maintain).
    SettingsGet {},
    /// Each given folder through the operator's own setter (maintain): the
    /// task folder opens and locks first, the personal repository must
    /// exist, the workspace is qualified as the picker's choice is.
    SettingsSet {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_folder: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        personal_repo: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
    },
    /// Settings → Local conversations → Scan personal repository (manage).
    ConversationsScan {},
    /// Attach for recovery (manage): binds a found conversation to a fully
    /// closed member of the same driver, held until Resume here.
    ConversationAttach { member: String, conversation_id: String },
    /// The current self-upgrade attempt, if any (maintain).
    UpgradeStatus {},
    /// The operator's Cancel during an upgrade's notice (maintain).
    UpgradeCancel { attempt_id: Uuid },
    /// The operator's Abandon of a held attempt (maintain): a terminal note
    /// that proves nothing stopped.
    UpgradeAbandon { attempt_id: Uuid },
    /// A resource sample, as the telemetry panel shows (maintain).
    Telemetry {},
    /// Rooms → Save room… (manage; a room not the caller's also roam): one
    /// room to a new `.prim1room` file at an absolute path.
    RoomSave {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_id: Option<RoomId>,
        path: String,
    },
    /// Load room…'s confirm step (manage): the rooms a `.prim1room` file or
    /// a copied runtime's `room-catalog-v1.json` offers. Reads only.
    RoomInspect { path: String },
    /// Rooms → Load room… (manage): a new room with new members; nothing
    /// starts and no member is given a grant.
    RoomLoad {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_room_id: Option<RoomId>,
    },
}

impl RuntimeAction {
    pub fn name(&self) -> &'static str {
        match self {
            Self::SettingsGet {} => "settings_get",
            Self::SettingsSet { .. } => "settings_set",
            Self::ConversationsScan {} => "conversations_scan",
            Self::ConversationAttach { .. } => "conversation_attach",
            Self::UpgradeStatus {} => "upgrade_status",
            Self::UpgradeCancel { .. } => "upgrade_cancel",
            Self::UpgradeAbandon { .. } => "upgrade_abandon",
            Self::Telemetry {} => "telemetry",
            Self::RoomSave { .. } => "room_save",
            Self::RoomInspect { .. } => "room_inspect",
            Self::RoomLoad { .. } => "room_load",
        }
    }

    pub fn is_read(&self) -> bool {
        matches!(self, Self::SettingsGet {} | Self::ConversationsScan {} | Self::UpgradeStatus {} | Self::Telemetry {}
            | Self::RoomInspect { .. })
    }

    /// Settings, upgrade control and telemetry need maintain; conversations
    /// and room packages need manage.
    pub fn needs_maintain(&self) -> bool {
        matches!(self, Self::SettingsGet {} | Self::SettingsSet { .. } | Self::UpgradeStatus {} | Self::UpgradeCancel { .. }
            | Self::UpgradeAbandon { .. } | Self::Telemetry {})
    }
}

/// A runtime action's result (A16); a retry with the same request_id returns
/// it again with `replayed`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeActionResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub replayed: bool,
    pub op: String,
    /// settings_get / settings_set: `task_folder`, `personal_repo` (the
    /// desktop's status objects) and `workspace`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<serde_json::Value>,
    /// conversations_scan: the bounded inventory (no transcript text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversations: Option<serde_json::Value>,
    /// conversation_attach: the member as the binding left it (held).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<SessionSnapshot>,
    /// upgrade_*: the upgrade attempt as the action left it (none: no attempt).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upgrade: Option<UpgradeStatus>,
    /// telemetry: the `TelemetrySnapshot`, as JSON (it holds floats).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<serde_json::Value>,
    /// room_save: the file written, as the operator's Save room reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_saved: Option<RoomExportReport>,
    /// room_inspect: the rooms a file or a copied runtime offers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_sources: Option<Vec<RoomSourceSummary>>,
    /// room_load: the new room and each member's honest resume state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_loaded: Option<RoomImportReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// The operator's room actions for a managing member (A16). `room_id`
/// omitted means the caller's room; another room also needs roam.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoomAction {
    /// A new, empty room; the caller stays where it is.
    Create {
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        brief_on_join: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        brief_template: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume_on_restart: Option<bool>,
    },
    /// The room's settings: label, Resume on restart, brief, position.
    Get {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_id: Option<RoomId>,
    },
    /// Each field given changes with the operator's own setter.
    /// `brief_template: ""` restores the built-in brief.
    Update {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_id: Option<RoomId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume_on_restart: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        brief_on_join: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        brief_template: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<usize>,
    },
    /// The operator's Delete room: its sessions go to the lobby.
    Delete { room_id: RoomId },
    /// Launch every stopped member (each resumes its conversation, as Launch
    /// does); the caller is never one of them. After a PC restart this brings
    /// a room back whatever "Resume on restart" recorded.
    Start {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_id: Option<RoomId>,
    },
    /// Stop every running member but the caller.
    Stop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_id: Option<RoomId>,
    },
    /// Invite an outside agent; the token is returned to the caller only.
    GuestInvite {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_id: Option<RoomId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    GuestRevoke { guest_id: Uuid },
    GuestList {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_id: Option<RoomId>,
    },
    /// The operator's Retry wake: one recipient's held or failed wake of a
    /// stored message (`receipt`, its cursor in the feed), attempted again;
    /// never a second post, never a recipient it already reached.
    Retry {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_id: Option<RoomId>,
        message_id: MessageId,
        recipient: String,
        receipt: RoomFeedCursor,
    },
}

impl RoomAction {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Create { .. } => "create",
            Self::Get { .. } => "get",
            Self::Update { .. } => "update",
            Self::Delete { .. } => "delete",
            Self::Start { .. } => "start",
            Self::Stop { .. } => "stop",
            Self::GuestInvite { .. } => "guest_invite",
            Self::GuestRevoke { .. } => "guest_revoke",
            Self::GuestList { .. } => "guest_list",
            Self::Retry { .. } => "retry",
        }
    }

    /// Reads take no request_id; every other action does.
    pub fn is_read(&self) -> bool {
        matches!(self, Self::Get { .. } | Self::GuestList { .. })
    }
}

/// A room action's result (A16). A retry with the same request_id returns it
/// again with `replayed`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomActionResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub replayed: bool,
    pub op: String,
    /// The room as the action left it; for delete, as it was.
    pub room: RoomSnapshot,
    /// Get and update: the brief settings and the built-in brief.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<RoomBriefSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_brief: Option<String>,
    /// Get and update: the room's place in the room order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guests: Vec<RoomGuest>,
    /// start and stop: the members acted on, as they were left.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<SessionSnapshot>,
    /// guest_invite only: shown once, never posted in the feed, kept only as
    /// a SHA-256. A retry returns the guest without the token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invitation: Option<RoomGuestInvitationOutput>,
    /// retry only: the attempt's delivery result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<RoomDeliveryResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// What an invited outside agent needs: its token and two ready uses, and
/// (A16) the one block to paste into any agent so it joins and takes part.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomGuestInvitationOutput {
    pub guest_id: Uuid,
    pub token: String,
    pub mcp_config_json: String,
    pub cli_example: String,
    pub agent_invitation: String,
}

/// One session for `members_list` (A16): its room (None in the lobby) and
/// its snapshot, as the operator sees it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemberDirectoryEntry {
    pub room_id: Option<RoomId>,
    pub room_label: Option<String>,
    pub session: SessionSnapshot,
}

/// What a member the operator lets manage agents (A110) asks of a session.
/// `member` is a session id, or a label unique in the caller's room. A16
/// (Victor, 20 Oct): every operator action on a member, grants included; no
/// ceiling, so a managing member may grant any of the three.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemberAction {
    /// A new Claude, Codex or Grok member of the caller's room (or `room_id`).
    /// Unsafe (bypass) only when asked for. `brief` sends the room brief, or
    /// `brief_text`, through the room's queued delivery once it is ready.
    /// `grants` (A16) are saved with it; omitted means none.
    Create {
        driver: DriverKind,
        label: String,
        #[serde(default)]
        permission_profile: PermissionProfile,
        #[serde(default)]
        brief: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        brief_text: Option<String>,
        #[serde(default)]
        start: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_id: Option<RoomId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grants: Option<MemberPermissions>,
        /// v3 W4: Codex only; omitted means `Pty`.
        #[serde(default, skip_serializing_if = "CodexTransport::is_pty")]
        codex_transport: CodexTransport,
    },
    /// `mode` (A16): resume (the default, as Launch), fresh (Start fresh
    /// session: stops a running member first and clears its hold) or
    /// resume_here (releases a copied conversation's hold, then launches).
    /// `launch_choices` replace the saved ones, as the operator's Save and
    /// launch does; only for a stopped member. `search` walks the usual
    /// install locations after a launch missed the harness on PATH.
    Start {
        member: String,
        #[serde(default, skip_serializing_if = "StartMode::is_resume")]
        mode: StartMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        launch_choices: Option<SessionLaunchChoices>,
        #[serde(default, skip_serializing_if = "is_false")]
        search: bool,
    },
    Stop { member: String },
    Restart { member: String },
    /// The room brief, or `content`, through the room's queued delivery.
    Brief {
        member: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
    },
    /// A16: what the operator types into a running member's terminal: `text`
    /// typed literally, then each of `keys`, for example to answer a prompt
    /// only a person could. The room's notice holds metadata only (who, whom,
    /// length, keys, request id); the text itself only with `show_text`, the
    /// caller's explicit mark that it is not sensitive.
    Input {
        member: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keys: Vec<ControlKey>,
        #[serde(default, skip_serializing_if = "is_false")]
        show_text: bool,
    },
    /// Stops the member if it runs, then removes it from its room. The
    /// session itself stays in the lobby; `delete` closes it.
    Remove { member: String },
    /// A16: the Edit dialog. Each field given is changed with the operator's
    /// rules (the profile and the working directory only while stopped);
    /// `grants` replace all three. `position` moves its tab, counted among the
    /// sessions the caller may list (members_list), 0 first.
    Update {
        member: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_profile: Option<PermissionProfile>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grants: Option<MemberPermissions>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_directory: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<u32>,
    },
    /// A16: a lobby session joins the caller's room (or `room_id`).
    Add {
        member: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_id: Option<RoomId>,
    },
    /// A16: the operator's Close session: out of its room, stopped, deleted.
    Delete { member: String },
}

/// How `member_start` launches (A16).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StartMode {
    #[default]
    Resume,
    Fresh,
    ResumeHere,
}

impl StartMode {
    pub fn is_resume(&self) -> bool {
        *self == Self::Resume
    }
}

impl MemberAction {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Create { .. } => "create",
            Self::Start { .. } => "start",
            Self::Stop { .. } => "stop",
            Self::Restart { .. } => "restart",
            Self::Brief { .. } => "brief",
            Self::Input { .. } => "input",
            Self::Remove { .. } => "remove",
            Self::Update { .. } => "update",
            Self::Add { .. } => "add",
            Self::Delete { .. } => "delete",
        }
    }
}

/// A16 `member_input` bounds: text typed literally, then keys.
pub const MEMBER_INPUT_TEXT_MAX_BYTES: usize = 4096;
pub const MEMBER_INPUT_KEYS_MAX: usize = 16;

/// The first result of one `Member` request; a retry with the same
/// request_id returns it again with `replayed`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemberActionResult {
    pub request_id: String,
    pub replayed: bool,
    /// The action's `op`.
    pub op: String,
    pub room_id: RoomId,
    /// The member as the action left it (for remove: no longer in the room).
    pub member: SessionSnapshot,
    /// The brief's delivery: Stored while it waits for the member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<RoomDeliveryResult>,
    /// member_create's requested start or brief that did not complete, or
    /// that a retry cannot confirm because the first result is gone (a
    /// PRIM-1 restart, or the first request did not finish).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub incomplete: Vec<MemberFollowUp>,
    /// The feed notice, a later step or a follow-up that did not complete.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// A follow-up member_create performs after the member joins its room.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemberFollowUp {
    Start,
    Brief,
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

/// A16: first line of the operator entry's connection: a local process of
/// the operator's Windows user that is not a pane (for example a supervisor
/// on another PC over SSH). The pipe's ACL admits only that user; the
/// supervisor also requires the caller to be outside every pane Job. `actor`
/// is display only: how the feed names it, beside its process id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OperatorPreamble {
    pub operator: OperatorIntro,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OperatorIntro {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
}

/// First line of an outside agent's connection: its invitation token. The
/// token alone decides identity; a caller inside a pane Job is refused, since
/// a pane is never an outside agent. A guest then sends one `GuestRequest`,
/// never a `SidebandRequest`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuestPreamble {
    pub guest_token: String,
    /// v3 W5: what this connection's client can read (per connection, never
    /// per guest). Absent means a legacy reader: its pages leave out guest
    /// ACKs, guest recipients and submission records.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

/// v3 W5: a guest client that reads addressed messages and guest ACKs.
pub const GUEST_CAPABILITY_ADDRESSED: &str = "guest_addressed_v1";
/// v3 W5 (cross-PC spool): this client reads `retry_after_secs` on a refusal that
/// clears by itself (a paced post, a closing or held PRIM-1).
pub const GUEST_CAPABILITY_RETRY_AFTER: &str = "guest_retry_after_v1";
/// Every guest invitation token starts with this.
pub const GUEST_TOKEN_PREFIX: &str = "prim1g_";

/// Everything an outside agent can ask, bound to its one room. Tagged by
/// `guest`, not `kind`, so it never parses as a pane request or vice versa.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "guest", rename_all = "snake_case", deny_unknown_fields)]
pub enum GuestRequest {
    /// Set this guest's label (shown as "<label> · guest") and mark it seen.
    Hello { label: String },
    /// This guest's room: label, tail cursor, members and guests.
    Rooms {},
    Read {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<RoomFeedCursor>,
        /// v3 W5: continue the guest's owed messages after a previous
        /// `retained_next`; absent starts them from the oldest.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owed_after: Option<RoomFeedCursor>,
    },
    /// Stored and typed to every room member, framed as a guest message.
    Post {
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    /// Long-poll: returns once others add a message, card, membership change
    /// or gap after `cursor`, or with `timed_out` at the deadline.
    Wait {
        cursor: RoomFeedCursor,
        timeout_ms: u64,
    },
    /// v3 W5: acknowledge (next_action) or decline one message addressed to
    /// this guest; exactly one of the two.
    Ack {
        message_id: MessageId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_action: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decline: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    /// v3 W5: this guest's room as a capability directory.
    Directory {},
    /// v3 W5: this guest's own invitation: room, label, and what it still owes.
    Status {},
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuestRoomSummary {
    pub room_id: RoomId,
    pub label: String,
    /// The room's tail now: pass it to read or wait to start from now.
    pub cursor: RoomFeedCursor,
    pub members: Vec<RoomMember>,
    pub guests: Vec<RoomGuest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GuestResponsePayload {
    Ack { result: GuestAckResult },
    Directory { directory: RoomCapabilityDirectory },
    Status { status: GuestStatus },
    Hello { guest: RoomGuest },
    Rooms { you: RoomGuest, rooms: Vec<GuestRoomSummary> },
    Feed { page: RoomFeedPage },
    Post { result: GuestPostResult },
}

/// v3 W5: a guest's own disposition as stored (the first one on a replay).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuestAckResult {
    pub message_id: MessageId,
    pub disposition: RoomAckDisposition,
    pub replayed: bool,
}

/// v3 W5: a guest's own invitation as the guest may see it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuestStatus {
    pub guest: RoomGuest,
    pub room_label: String,
    /// Messages addressed to this guest that it has neither acknowledged nor declined.
    pub owed: Vec<MessageId>,
}

/// What a guest learns of its post: members by label and status only, never
/// their run ids, generations, byte counts or wake errors.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuestPostResult {
    pub message_id: MessageId,
    pub cursor: RoomFeedCursor,
    pub request_id: String,
    pub replayed: bool,
    pub recipient_count: usize,
    pub written_count: usize,
    pub receipts: Vec<GuestPostReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuestPostReceipt {
    pub label: String,
    pub status: RoomDeliveryStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuestResponse {
    pub ok: bool,
    pub message: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub timed_out: bool,
    /// v3 W5: only for a client that sent `guest_retry_after_v1`; the same
    /// request may succeed after this many seconds. None is not retryable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<GuestResponsePayload>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// W6 (Claude PRIM-1 744574eb): the operator's host health check reads
    /// `runtime\guest-endpoint.json` by exactly these keys; the shape stays.
    #[test]
    fn the_guest_endpoint_file_keeps_its_shape() {
        let value = serde_json::to_value(GuestEndpoint { endpoint: r"\\.\pipe\x".into(), server_pid: 7, server_started: 9 }).unwrap();
        let keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["endpoint", "server_pid", "server_started"]);
        assert_eq!(value["server_pid"], json!(7));
    }

    fn run_event_identity(sequence: u64) -> RunEventIdentity {
        RunEventIdentity {
            session_id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            run_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            generation: 7,
            sequence,
        }
    }

    fn session_snapshot() -> SessionSnapshot {
        SessionSnapshot { codex_transport: Default::default(), app_server: None,
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
            pty_size: Some(DEFAULT_PTY_SIZE),
            process_id: Some(1234),
            running: true,
            output_draining: false,
            crash_respawn_waiting: false,
            resume_available: false,
            was_running_at_shutdown: false,
            last_activity_at: Some("2026-08-10T00:00:00Z".into()),
            last_error: None,
            last_error_kind: None,
            launch_choices: SessionLaunchChoices::default(),
            last_launch: None,
            recovery: None,
            recovery_history: Vec::new(),
            permissions: MemberPermissions::default(),
            blocked_reason: None,
            work_state: None,
            work_state_from_record: false,
        }
    }

    #[test]
    fn crash_respawn_waiting_defaults_false_for_legacy_snapshots_and_round_trips() {
        let mut snapshot = session_snapshot();
        let mut legacy = serde_json::to_value(&snapshot).unwrap();
        legacy.as_object_mut().unwrap().remove("crash_respawn_waiting");
        assert!(!serde_json::from_value::<SessionSnapshot>(legacy).unwrap().crash_respawn_waiting);
        snapshot.running = false;
        snapshot.crash_respawn_waiting = true;
        let wire = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(wire["crash_respawn_waiting"], json!(true));
        assert_eq!(serde_json::from_value::<SessionSnapshot>(wire).unwrap(), snapshot);
    }

    #[test]
    fn blocked_reason_is_omitted_when_absent_and_round_trips() {
        let mut snapshot = session_snapshot();
        let wire = serde_json::to_value(&snapshot).unwrap();
        assert!(wire.get("blocked_reason").is_none(), "older readers never see it unset");
        snapshot.blocked_reason = Some("it is asking whether to trust the folder — answer that question in its terminal, then send again".into());
        let wire = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(wire["blocked_reason"], json!(snapshot.blocked_reason));
        assert_eq!(serde_json::from_value::<SessionSnapshot>(wire).unwrap(), snapshot);
    }

    #[test]
    fn member_permissions_default_off_and_legacy_snapshots_decode() {
        let mut snapshot = session_snapshot();
        let mut wire = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(wire["permissions"], json!({ "roam": false, "maintain": false }), "manage is omitted while off");
        wire.as_object_mut().unwrap().remove("permissions");
        assert_eq!(serde_json::from_value::<SessionSnapshot>(wire).unwrap().permissions, MemberPermissions::default());
        snapshot.permissions.roam = true;
        snapshot.permissions.manage = true;
        let wire = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(wire["permissions"]["manage"], json!(true));
        assert_eq!(serde_json::from_value::<SessionSnapshot>(wire).unwrap(), snapshot);
        // A109 grants written before manage existed decode with manage off.
        assert_eq!(serde_json::from_value::<MemberPermissions>(json!({ "roam": true, "maintain": true })).unwrap(),
            MemberPermissions { roam: true, maintain: true, manage: false });
        assert_eq!(serde_json::from_value::<MemberPermissions>(json!({ "maintain": true })).unwrap(),
            MemberPermissions { roam: false, maintain: true, manage: false });
        assert!(serde_json::from_value::<MemberPermissions>(json!({ "roam": true, "admin": true })).is_err());
        let session_id = SessionId::new_v4();
        assert_eq!(serde_json::from_value::<SetMemberPermissionsRequest>(json!({ "session_id": session_id,
            "roam": true, "maintain": false })).unwrap().manage, false);
    }

    #[test]
    fn member_requests_carry_one_action_and_grant_only_by_name() {
        let request_id = Uuid::new_v4().to_string();
        let room_id = RoomId::new_v4();
        assert_eq!(
            serde_json::from_value::<SidebandRequest>(json!({ "kind": "member", "request_id": request_id,
                "action": { "op": "create", "driver": "grok", "label": "QA Grok" } })).unwrap(),
            SidebandRequest::Member { request_id: request_id.clone(), action: MemberAction::Create { codex_transport: Default::default(),
                driver: DriverKind::Grok, label: "QA Grok".into(), permission_profile: PermissionProfile::Normal,
                brief: false, brief_text: None, start: false, room_id: None, grants: None } },
            "the safe profile, no brief, no start and no grants unless asked"
        );
        let all = MemberPermissions { roam: true, maintain: true, manage: true };
        let create = MemberAction::Create { codex_transport: Default::default(), driver: DriverKind::Claude, label: "QA".into(),
            permission_profile: PermissionProfile::Unsafe, brief: true, brief_text: Some("check the build".into()),
            start: true, room_id: Some(room_id), grants: Some(all) };
        assert_eq!(serde_json::from_value::<MemberAction>(serde_json::to_value(&create).unwrap()).unwrap(), create);
        // A16: an action without grants or a mode serializes as A110 wrote it.
        assert_eq!(serde_json::to_value(MemberAction::Start { member: "QA".into(), mode: StartMode::Resume,
            launch_choices: None, search: false }).unwrap(), json!({ "op": "start", "member": "QA" }));
        assert_eq!(serde_json::to_value([MemberFollowUp::Start, MemberFollowUp::Brief]).unwrap(), json!(["start", "brief"]));
        for (op, action) in [
            ("start", MemberAction::Start { member: "QA".into(), mode: StartMode::Resume, launch_choices: None, search: false }),
            ("stop", MemberAction::Stop { member: "QA".into() }),
            ("restart", MemberAction::Restart { member: "QA".into() }),
            ("brief", MemberAction::Brief { member: "QA".into(), content: None }),
            ("remove", MemberAction::Remove { member: "QA".into() }),
            ("update", MemberAction::Update { member: "QA".into(), label: None, permission_profile: None, grants: None, working_directory: None, position: None }),
            ("add", MemberAction::Add { member: "QA".into(), room_id: None }),
            ("delete", MemberAction::Delete { member: "QA".into() }),
        ] {
            assert_eq!(action.name(), op);
            assert_eq!(serde_json::from_value::<MemberAction>(json!({ "op": op, "member": "QA" })).unwrap(), action);
        }
        assert_eq!(serde_json::from_value::<MemberAction>(json!({ "op": "start", "member": "QA", "mode": "fresh" })).unwrap(),
            MemberAction::Start { member: "QA".into(), mode: StartMode::Fresh, launch_choices: None, search: false });
        assert_eq!(serde_json::from_value::<MemberAction>(json!({ "op": "update", "member": "QA",
            "grants": { "roam": true, "maintain": true, "manage": true } })).unwrap(),
            MemberAction::Update { member: "QA".into(), label: None, permission_profile: None, grants: Some(all), working_directory: None, position: None });
        for request in [
            json!({ "kind": "member", "action": { "op": "start", "member": "QA" } }),
            // Grants travel only as `grants`, with the three known names.
            json!({ "kind": "member", "request_id": request_id, "action": { "op": "create", "driver": "grok",
                "label": "QA", "permissions": { "manage": true } } }),
            json!({ "kind": "member", "request_id": request_id, "action": { "op": "update", "member": "QA",
                "grants": { "admin": true } } }),
            json!({ "kind": "member", "request_id": request_id, "action": { "op": "start", "member": "QA",
                "mode": "anywhere" } }),
            json!({ "kind": "member", "request_id": request_id, "action": { "op": "stop", "member": "QA",
                "room_id": room_id } }),
            json!({ "kind": "member", "request_id": request_id, "session_id": SessionId::new_v4(),
                "action": { "op": "stop", "member": "QA" } }),
            json!({ "kind": "members_list", "all": true }),
        ] {
            assert!(serde_json::from_value::<SidebandRequest>(request.clone()).is_err(), "{request}");
        }
        assert_eq!(serde_json::from_value::<SidebandRequest>(json!({ "kind": "members_list" })).unwrap(),
            SidebandRequest::MembersList {});
    }

    #[test]
    fn roaming_requests_name_their_room_and_nothing_else() {
        let room_id = RoomId::new_v4();
        assert_eq!(
            serde_json::from_value::<SidebandRequest>(json!({ "kind": "room_deliver_to", "room_id": room_id,
                "recipient": "Codex", "content": "x" })).unwrap(),
            SidebandRequest::RoomDeliverTo { room_id, request_id: None, recipient: "Codex".into(), content: "x".into() }
        );
        assert_eq!(serde_json::from_value::<SidebandRequest>(json!({ "kind": "room_join", "room_id": room_id })).unwrap(),
            SidebandRequest::RoomJoin { room_id });
        assert_eq!(serde_json::from_value::<SidebandRequest>(json!({ "kind": "room_peek", "room_id": room_id })).unwrap(),
            SidebandRequest::RoomPeek { room_id, cursor: None });
        for request in [
            json!({ "kind": "room_deliver_to", "recipient": "all", "content": "x" }),
            json!({ "kind": "room_deliver_to", "room_id": room_id, "recipient": "all", "content": "x", "sender": "operator" }),
            json!({ "kind": "room_join", "room_id": room_id, "session_id": SessionId::new_v4() }),
            json!({ "kind": "room_leave", "room_id": room_id }),
            json!({ "kind": "rooms_list", "all": true }),
            json!({ "kind": "member_permissions", "roam": true }),
        ] {
            assert!(serde_json::from_value::<SidebandRequest>(request.clone()).is_err(), "{request}");
        }
    }

    #[test]
    fn upgrade_requests_carry_the_candidate_and_checkpoint_only() {
        let sha = "a".repeat(64);
        assert_eq!(
            serde_json::from_value::<SidebandRequest>(json!({ "kind": "upgrade_request",
                "candidate_exe": "D:\\build\\prim1.exe", "sha256": sha, "task_id": "T-7", "task_revision": 3 })).unwrap(),
            SidebandRequest::UpgradeRequest { candidate_exe: "D:\\build\\prim1.exe".into(), sha256: sha.clone(),
                task_id: "T-7".into(), task_revision: 3 }
        );
        for request in [
            json!({ "kind": "upgrade_request", "candidate_exe": "x", "sha256": sha, "task_id": "T-7" }),
            json!({ "kind": "upgrade_request", "candidate_exe": "x", "sha256": sha, "task_id": "T-7", "task_revision": 3,
                "session_id": SessionId::new_v4() }),
            json!({ "kind": "rebuild_preflight", "interrupt_without_resume": true }),
        ] {
            assert!(serde_json::from_value::<SidebandRequest>(request.clone()).is_err(), "{request}");
        }
        let status = UpgradeStatus {
            attempt_id: Uuid::new_v4(), stage: UpgradeStage::Notice, candidate_sha256: sha,
            requester_label: "Claude".into(), notice_until: "t".into(), expires_at: "t".into(),
            message: "m".into(), can_cancel: true, can_abandon: false, member_session_ids: vec![SessionId::new_v4()],
        };
        let event = RuntimeEvent::UpgradeStatusChanged { status: Some(status), timestamp: "t".into() };
        let wire = serde_json::to_value(&event).unwrap();
        assert_eq!((wire["event"].as_str(), wire["status"]["stage"].as_str()), (Some("upgrade_status_changed"), Some("notice")));
        assert_eq!(serde_json::from_value::<RuntimeEvent>(wire).unwrap(), event);
    }

    #[test]
    fn snapshot_pty_size_preserves_exact_grid_and_legacy_absence() {
        let mut snapshot = session_snapshot();
        snapshot.pty_size = Some((196, 22));
        let mut wire = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(wire["pty_size"], json!([196, 22]));
        assert_eq!(serde_json::from_value::<SessionSnapshot>(wire.clone()).unwrap(), snapshot);
        wire.as_object_mut().unwrap().remove("pty_size");
        assert_eq!(serde_json::from_value::<SessionSnapshot>(wire).unwrap().pty_size, None);
    }

    #[test]
    fn launch_choices_wire_shape_defaults_and_rejects_unknown_brief_fields() {
        let id = Uuid::new_v4();
        let old: StartSessionRequest = serde_json::from_value(json!({"session_id": id})).unwrap();
        assert!(old.launch_choices.is_none());
        for brief in [
            json!({"mode": "room_default"}),
            json!({"mode": "none"}),
            json!({"mode": "custom", "text": "task"}),
        ] {
            let request: StartSessionRequest = serde_json::from_value(json!({
                "session_id": id, "launch_choices": {"model": "chosen", "effort": "max", "brief": brief}
            })).unwrap();
            assert_eq!(
                serde_json::to_value(request).unwrap()["launch_choices"]["brief"],
                brief
            );
        }
        for brief in [
            json!({"mode": "none", "text": "hidden"}),
            json!({"mode": "custom"}),
            json!({"mode": "guess"}),
        ] {
            assert!(
                serde_json::from_value::<StartSessionRequest>(json!({
                    "session_id": id, "launch_choices": {"brief": brief}
                }))
                .is_err()
            );
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
            RuntimeEvent::SessionMemberPermissionsChanged {
                schema_version: SESSION_EVENT_SCHEMA_VERSION,
                session_id,
                old_permissions: MemberPermissions::default(),
                new_permissions: MemberPermissions { roam: true, maintain: false, manage: true },
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
                SidebandRequest::RoomRead { owed_after: None, cursor: None, expect_room: None },
                json!({ "kind": "room_read" }),
            ),
            (
                SidebandRequest::RoomPost {
                    request_id: None,
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
            json!({
                "kind": "room_wait",
                "room_id": room_id,
                "cursor": { "epoch": room_id, "sequence": 3 },
            }),
            // A16: a wait always continues from a cursor read before.
            json!({ "kind": "room_wait", "timeout_ms": 500 }),
        ] {
            assert!(serde_json::from_value::<SidebandRequest>(request).is_err());
        }
        assert_eq!(
            serde_json::from_value::<SidebandRequest>(json!({
                "kind": "room_wait", "cursor": { "epoch": room_id, "sequence": 3 },
            })).unwrap(),
            SidebandRequest::RoomWait { cursor: RoomFeedCursor { epoch: room_id, sequence: 3 }, timeout_ms: None }
        );
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
                request_id: None,
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
            item: RoomFeedItem::Message { guest_recipient_ids: Vec::new(), guest_recipient_labels: Vec::new(), 
                fallback_origin: None,
                continuation_origin: None,
                request_id: Uuid::new_v4().to_string(),
                message_id: MessageId::new_v4(),
                sender: RoomMessageSender::Operator {},
                content: "hello".into(),
                recipient_ids: vec![session_id],
                membership_revision: 2,
                ack_requested: false,
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
        assert!(!request.fresh);
        assert!(!request.search);
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
    fn tasks_request_and_payload_round_trip_without_actor_authority() {
        let full = SidebandRequest::Tasks {
            query: "mutation($p: TaskPatchInput!) { updateTask(id: \"T-1\", expectedRevision: 1, patch: $p) { revision } }".into(),
            variables: Some(json!({ "p": { "state": "doing", "note": "λ\nline two" } })),
            operation_name: Some("Move".into()),
        };
        let minimal = SidebandRequest::Tasks {
            query: "{ tasks { total } }".into(),
            variables: None,
            operation_name: None,
        };
        for (request, expected) in [
            (
                full,
                json!({
                    "kind": "tasks",
                    "query": "mutation($p: TaskPatchInput!) { updateTask(id: \"T-1\", expectedRevision: 1, patch: $p) { revision } }",
                    "variables": { "p": { "state": "doing", "note": "λ\nline two" } },
                    "operation_name": "Move",
                }),
            ),
            (minimal, json!({ "kind": "tasks", "query": "{ tasks { total } }" })),
        ] {
            let value = serde_json::to_value(&request).unwrap();
            assert_eq!(value, expected);
            assert_eq!(serde_json::from_value::<SidebandRequest>(value).unwrap(), request);
        }
        for forged in [
            json!({ "kind": "tasks", "query": "{ tasks { total } }", "actor": "operator" }),
            json!({ "kind": "tasks", "query": "{ tasks { total } }", "session_id": SessionId::new_v4() }),
            json!({ "kind": "tasks", "variables": {} }),
        ] {
            assert!(serde_json::from_value::<SidebandRequest>(forged).is_err());
        }

        let response = SidebandResponse {
            ok: false,
            message: "task request returned 1 error".into(),
            timed_out: false,
            payload: Some(SidebandResponsePayload::Tasks {
                result: json!({
                    "data": { "updateTask": null },
                    "errors": [{ "message": "conflict", "extensions": { "code": "CONFLICT", "currentRevision": 2 } }],
                }),
            }),
            request_id: None,
        };
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["payload"]["kind"], "tasks");
        assert_eq!(value["payload"]["result"]["errors"][0]["extensions"]["currentRevision"], 2);
        assert_eq!(serde_json::from_value::<SidebandResponse>(value).unwrap(), response);
        assert!(
            serde_json::from_value::<SidebandResponsePayload>(json!({ "kind": "tasks", "result": {}, "committed": [] }))
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

    /// apps/desktop/src/types.ts mirrors this shape field for field.
    #[test]
    fn telemetry_snapshot_wire_shape_matches_the_renderer_mirror() {
        let usage = |cpu_percent, private_bytes, processes| TelemetryUsage {
            cpu_percent,
            private_bytes,
            processes,
        };
        let snapshot = TelemetrySnapshot {
            sampled_at: "2026-10-14T00:00:00Z".into(),
            logical_cpus: 12,
            core: usage(Some(0.5), 8_000_000, 1),
            webview: usage(Some(4.0), 500_000_000, 6),
            helpers: usage(Some(0.0), 16_000_000, 7),
            sessions: vec![TelemetrySessionUsage {
                session_id: run_event_identity(0).session_id,
                label: "Codex".into(),
                driver: DriverKind::Codex,
                cpu_percent: Some(2.5),
                private_bytes: 300_000_000,
                processes: 9,
                peak_cpu_percent: Some(12.0),
                peak_private_bytes: 410_000_000,
            }],
            total: usage(None, 824_000_000, 23),
            unreadable_processes: 1,
            sampler_cost_ms: 3.25,
        };

        let value = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(
            value,
            json!({
                "sampled_at": "2026-10-14T00:00:00Z",
                "logical_cpus": 12,
                "core": {"cpu_percent": 0.5, "private_bytes": 8_000_000, "processes": 1},
                "webview": {"cpu_percent": 4.0, "private_bytes": 500_000_000, "processes": 6},
                "helpers": {"cpu_percent": 0.0, "private_bytes": 16_000_000, "processes": 7},
                "sessions": [{
                    "session_id": snapshot.sessions[0].session_id,
                    "label": "Codex",
                    "driver": "codex",
                    "cpu_percent": 2.5,
                    "private_bytes": 300_000_000,
                    "processes": 9,
                    "peak_cpu_percent": 12.0,
                    "peak_private_bytes": 410_000_000,
                }],
                "total": {"cpu_percent": null, "private_bytes": 824_000_000, "processes": 23},
                "unreadable_processes": 1,
                "sampler_cost_ms": 3.25,
            })
        );
        assert_eq!(
            serde_json::from_value::<TelemetrySnapshot>(value).unwrap(),
            snapshot
        );
    }

    #[test]
    fn guest_sender_and_requests_keep_their_wire_shape() {
        let guest_id = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
        let sender = RoomMessageSender::Guest { guest_id, label: "ChatGPT".into() };
        let wire = serde_json::to_value(&sender).unwrap();
        assert_eq!(wire, json!({"kind": "guest", "guest_id": guest_id, "label": "ChatGPT"}));
        assert_eq!(serde_json::from_value::<RoomMessageSender>(wire).unwrap(), sender);
        assert!(serde_json::from_value::<RoomMessageSender>(
            json!({"kind": "guest", "guest_id": guest_id, "label": "x", "session_id": guest_id})).is_err());

        let read = GuestRequest::Read { cursor: None, owed_after: None };
        assert_eq!(serde_json::to_value(&read).unwrap(), json!({"guest": "read"}));
        let wait: GuestRequest = serde_json::from_value(json!({"guest": "wait",
            "cursor": {"epoch": guest_id, "sequence": 4}, "timeout_ms": 1000})).unwrap();
        assert_eq!(wait, GuestRequest::Wait {
            cursor: RoomFeedCursor { epoch: guest_id, sequence: 4 }, timeout_ms: 1000 });
        assert!(serde_json::from_value::<GuestRequest>(json!({"guest": "post", "content": "x",
            "room_id": guest_id})).is_err(), "a guest names no room");
        assert!(serde_json::from_value::<GuestRequest>(json!({"guest": "send_input",
            "name": "a", "input": "b"})).is_err());
    }

    #[test]
    fn room_snapshot_and_page_without_guests_still_parse() {
        let room_id = Uuid::new_v4();
        let snapshot: RoomSnapshot = serde_json::from_value(json!({
            "room_id": room_id, "label": "Room", "member_ids": [], "membership_revision": 1,
            "feed_epoch": room_id, "feed_oldest_sequence": 1, "feed_next_sequence": 1,
        })).unwrap();
        assert!(snapshot.guests.is_empty());
        let page: RoomFeedPage = serde_json::from_value(json!({
            "schema_version": 1, "room_id": room_id, "cursor": {"epoch": room_id, "sequence": 0},
            "gap": null, "events": [], "has_more": false,
        })).unwrap();
        assert!(page.guests.is_empty());
    }
}
