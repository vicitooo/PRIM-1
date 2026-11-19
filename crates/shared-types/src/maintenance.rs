//! Bounded maintenance receipts. No terminal text, environment, paths or peer
//! conversation references are included in these pane-visible projections.
use crate::{DriverKind, LifecycleState, RunId, SessionGeneration, SessionId, WorkState};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DoctorCaller {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub generation: SessionGeneration,
    pub driver: DriverKind,
    pub lifecycle: LifecycleState,
    pub work_state: WorkState,
    pub recovery_present: bool,
    pub conversation_reference_present: bool,
    pub launch_record_matches_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DoctorReport {
    pub schema_version: u32,
    pub generated_at: String,
    pub supervisor_version: String,
    /// Absent unless supplied by the build; package version is not a source SHA.
    pub build_revision: Option<String>,
    pub os: String,
    pub uptime_seconds: u64,
    pub shutdown_started: bool,
    pub control_plane_registered: bool,
    pub sessions: usize,
    pub active_or_uncertain_members: usize,
    pub recovery_items: usize,
    pub rooms: usize,
    pub selected_rooms: usize,
    pub caller: Option<DoctorCaller>,
    /// Stored observations only; no process, disk, harness or renderer probes.
    pub observation_scope: String,
    /// This runtime's current self-upgrade attempt, when there is one.
    #[serde(default)]
    pub upgrade: Option<UpgradeAttemptSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IssueReceipt {
    pub incident_id: Uuid,
    pub stored_at: String,
    pub retained_incidents: usize,
}

/// The committed phase of one self-upgrade attempt (A108), as recorded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpgradeAttemptSummary {
    pub attempt_id: Uuid,
    /// `prepared`, `cancelled`, `closing`, `shutdown_complete`, `resuming`,
    /// `consumed` or `abandoned`.
    pub phase: String,
}

/// What a member's `upgrade_request` was granted: one attempt with a notice
/// the operator can cancel before PRIM-1 closes. Not a restart yet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpgradeAdmission {
    pub attempt_id: Uuid,
    /// RFC 3339; PRIM-1 does not close before this.
    pub notice_until: String,
    /// RFC 3339; the outside helper gives up after this.
    pub expires_at: String,
    /// Running members that will be resumed in their exact conversation.
    pub resumed_members: Vec<String>,
    /// Running members that stop and are not restarted (terminal, Prime, or
    /// outside a room).
    pub interrupted_members: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpgradeStage {
    /// The 60-second notice, then waiting for the helper; Cancel works.
    Notice,
    /// PRIM-1 is stopping its members and closing for the new build.
    Closing,
    /// An attempt that neither finished nor definitely failed before the
    /// new build started. The operator may abandon it.
    Held,
    /// The new build is resuming the recorded members.
    Resuming,
    /// The new build resumed the members and queued their continuations.
    Resumed,
    Cancelled,
    /// A failure the operator should read; nothing is retried.
    Failed,
}

/// The desktop banner's view of the current attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpgradeStatus {
    pub attempt_id: Uuid,
    pub stage: UpgradeStage,
    /// Lowercase hex of the new build.
    pub candidate_sha256: String,
    pub requester_label: String,
    pub notice_until: String,
    pub expires_at: String,
    /// Plain words for the banner.
    pub message: String,
    pub can_cancel: bool,
    pub can_abandon: bool,
    /// Members this attempt owns at startup; the ordinary restart resume
    /// leaves them to the upgrade.
    pub member_session_ids: Vec<SessionId>,
}
