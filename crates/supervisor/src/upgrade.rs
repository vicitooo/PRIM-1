//! Same-runtime executable handoff records. This module neither stops nor starts
//! processes, inspects conversation ownership, opens task/profile contents, nor
//! rolls back state. The desktop supplies and rechecks the live running set and
//! calls `shutdown_complete` only after its strict shutdown succeeds.
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use shared_types::DriverKind;
use uuid::Uuid;

use crate::{
    canonical_destination_without_reparse_points, ensure_existing_path_chain_has_no_reparse_points,
    rebuild::{maintenance_directory, read_bounded_json},
    restrict_path_to_current_user,
};

pub const SCHEMA_VERSION: u32 = 1;
pub const NOTICE_SECONDS: i64 = 60;
pub const EXPIRY_SECONDS: i64 = 15 * 60;
pub const CLOSE_REMAINING_SECONDS: i64 = 5 * 60;
const REQUEST_MAX_BYTES: usize = 2 * 1024 * 1024;
const RECORD_MAX_BYTES: usize = 16 * 1024;
const MAX_MEMBERS: usize = 4096;
const CANDIDATE_MAX_BYTES: u64 = 512 * 1024 * 1024;
const HELPER: &[u8] = include_bytes!("../../../scripts/continue-upgrade.ps1");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpgradeSource {
    pub pid: u32,
    /// Decimal Windows FILETIME, a string so JavaScript cannot round its u64.
    pub creation_filetime: String,
    pub exe_path: PathBuf,
    pub exe_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpgradeCandidate {
    pub exe_path: PathBuf,
    pub exe_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskRef {
    pub task_id: String,
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpgradeMember {
    pub session_id: Uuid,
    pub room_id: Option<Uuid>,
    pub driver: DriverKind,
    pub run_id: Uuid,
    pub generation: u64,
    #[serde(default)]
    pub resume_mode: UpgradeResumeMode,
    pub conversation_id: Option<String>,
    pub task_ref: Option<TaskRef>,
    /// Generated once when constructing the request; reuse for durable delivery.
    pub continuation_id: Uuid,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpgradeResumeMode {
    #[default]
    Exact,
    InterruptedWithoutResume,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpgradeRequest {
    pub schema_version: u32,
    pub attempt_id: Uuid,
    pub prepared_at: DateTime<Utc>,
    pub notice_until: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub source: UpgradeSource,
    pub candidate: UpgradeCandidate,
    pub runtime_dir: PathBuf,
    pub profile_dir: PathBuf,
    pub task_folder: PathBuf,
    /// Preserve a startup override (including QA's `off`) through the helper's
    /// clean environment. None means the normal desktop discovery policy.
    #[serde(default)]
    pub personal_repo_override: Option<String>,
    pub requester_session_id: Uuid,
    pub members: Vec<UpgradeMember>,
}

impl UpgradeRequest {
    /// Allocate the attempt and continuation IDs once, before calling prepare.
    /// Keep this request for retries; read(attempt_id) recovers its committed copy.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source: UpgradeSource,
        candidate: UpgradeCandidate,
        runtime_dir: PathBuf,
        profile_dir: PathBuf,
        task_folder: PathBuf,
        personal_repo_override: Option<String>,
        requester_session_id: Uuid,
        mut members: Vec<UpgradeMember>,
    ) -> Self {
        let prepared_at = Utc::now();
        for member in &mut members {
            member.continuation_id = Uuid::new_v4();
            if member.room_id.is_none()
                || matches!(
                    member.driver,
                    DriverKind::GenericTerminal | DriverKind::Prime
                )
            {
                member.resume_mode = UpgradeResumeMode::InterruptedWithoutResume;
                member.conversation_id = None;
            }
        }
        Self {
            schema_version: SCHEMA_VERSION,
            attempt_id: Uuid::new_v4(),
            prepared_at,
            notice_until: prepared_at + Duration::seconds(NOTICE_SECONDS),
            expires_at: prepared_at + Duration::seconds(EXPIRY_SECONDS),
            source,
            candidate,
            runtime_dir,
            profile_dir,
            task_folder,
            personal_repo_override,
            requester_session_id,
            members,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpgradePhase {
    Prepared,
    Cancelled,
    Closing,
    ShutdownComplete,
    Resuming,
    Consumed,
    Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpgradeState {
    pub schema_version: u32,
    pub attempt_id: Uuid,
    pub request_sha256: String,
    pub phase: UpgradePhase,
    pub changed_at: DateTime<Utc>,
    #[serde(default)]
    pub candidate_identity: Option<UpgradeSource>,
}

/// The desktop verifies this exact helper process is still alive before close.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelperIdentity {
    pub pid: u32,
    pub creation_filetime: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperReady {
    pub schema_version: u32,
    pub attempt_id: Uuid,
    pub request_sha256: String,
    pub helper_pid: u32,
    pub helper_creation_filetime: String,
    pub at_utc: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HelperOutcome {
    Launching,
    Launched,
    Failed,
    Cancelled,
    Expired,
    Uncertain,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperResult {
    pub schema_version: u32,
    pub attempt_id: Uuid,
    pub request_sha256: String,
    pub outcome: HelperOutcome,
    pub candidate_process: Option<UpgradeSource>,
    pub at_utc: DateTime<Utc>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CurrentAttempt {
    schema_version: u32,
    attempt_id: Uuid,
    request_sha256: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpgradeRecord {
    pub request: UpgradeRequest,
    pub state: UpgradeState,
    pub request_path: PathBuf,
    pub helper_path: PathBuf,
}

/// Keep one store per owning desktop. The OS lock serializes separate instances;
/// the mutex serializes callers of this instance. Neither lock is held by helper.
pub struct UpgradeStore {
    runtime: PathBuf,
    directory: PathBuf,
    _lock: File,
    mutations: Mutex<()>,
}

impl UpgradeStore {
    pub fn open(runtime: &Path) -> Result<Self> {
        ensure!(
            runtime.is_absolute() && runtime.is_dir(),
            "upgrade runtime must already exist"
        );
        let runtime = canonical_destination_without_reparse_points(runtime)?;
        let local = std::env::var_os("LOCALAPPDATA")
            .context("LOCALAPPDATA is required for upgrade isolation")?;
        reject_installed_v2_overlap(&runtime, Path::new(&local))?;
        let directory = maintenance_directory(&runtime)?.join("upgrades");
        ensure_existing_path_chain_has_no_reparse_points(&directory)?;
        fs::create_dir_all(&directory)?;
        restrict_path_to_current_user(&directory, true)?;
        let path = directory.join("transaction.lock");
        ensure_existing_path_chain_has_no_reparse_points(&path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        lock.try_lock()
            .context("another upgrade transaction store owns this runtime")?;
        restrict_path_to_current_user(&path, false)?;
        Ok(Self {
            runtime,
            directory,
            _lock: lock,
            mutations: Mutex::new(()),
        })
    }

    pub fn request_path(&self, attempt_id: Uuid) -> PathBuf {
        self.attempt_dir(attempt_id).join("request.json")
    }
    pub fn helper_path(&self, attempt_id: Uuid) -> PathBuf {
        self.attempt_dir(attempt_id).join("continue-upgrade.ps1")
    }

    /// `candidate.exe_path` is an input file. The immutable committed request
    /// names its hash-checked private copy instead. A retry may use either path.
    /// Caller must own the current desktop/runtime; this API does not certify it.
    pub fn prepare(&self, request: UpgradeRequest) -> Result<UpgradeRecord> {
        self.prepare_at(request, Utc::now())
    }

    fn prepare_at(&self, mut request: UpgradeRequest, now: DateTime<Utc>) -> Result<UpgradeRecord> {
        let _guard = self.mutations.lock();
        validate_request(&request)?;
        ensure!(
            canonical_destination_without_reparse_points(&request.runtime_dir)? == self.runtime,
            "upgrade request names a different runtime"
        );
        let candidate_input = request.candidate.exe_path.clone();
        request.candidate.exe_path = self
            .attempt_dir(request.attempt_id)
            .join("candidate/prim1.exe");
        let bytes = json_bytes(&request, REQUEST_MAX_BYTES)?;
        let hash = digest(&bytes);
        let pointer = CurrentAttempt {
            schema_version: SCHEMA_VERSION,
            attempt_id: request.attempt_id,
            request_sha256: hash.clone(),
        };
        if let Some(current) = self.current()? {
            if current.attempt_id == request.attempt_id {
                ensure!(
                    current.request_sha256 == hash,
                    "attempt request is immutable"
                );
                return self.read_inner(request.attempt_id);
            } else {
                self.predecessor_admits(&current, &request.source)?;
            }
        }
        ensure!(
            now >= request.prepared_at && now < request.expires_at,
            "upgrade request is future-dated or expired"
        );
        let attempt = self.attempt_dir(request.attempt_id);
        ensure_existing_path_chain_has_no_reparse_points(&attempt)?;
        // Fill missing pre-launch records only before any helper activity.
        self.no_helper_activity(&attempt)?;
        fs::create_dir_all(&attempt)?;
        restrict_path_to_current_user(&attempt, true)?;
        stage_candidate(&candidate_input, &request.candidate)?;
        stage_prelaunch_bytes(&self.helper_path(request.attempt_id), HELPER)?;
        stage_prelaunch_bytes(&self.request_path(request.attempt_id), &bytes)?;
        let initial = UpgradeState {
            schema_version: SCHEMA_VERSION,
            attempt_id: request.attempt_id,
            request_sha256: hash,
            phase: UpgradePhase::Prepared,
            changed_at: request.prepared_at,
            candidate_identity: None,
        };
        let state_path = attempt.join("state.json");
        if state_path.try_exists()? {
            let state: UpgradeState = read_bounded_json(&state_path, RECORD_MAX_BYTES)?;
            ensure!(
                state == initial,
                "foreign or non-Prepared upgrade state retained"
            );
        } else {
            persist_record(&attempt, "state.json", &initial)?;
        }
        // Publish data before pointer. Helper registration occurs only after
        // prepare returns. Leftover temporaries never authorize any action.
        persist_record(&self.directory, "current.json", &pointer)?;
        self.read_inner(request.attempt_id)
    }

    pub fn read(&self, attempt_id: Uuid) -> Result<UpgradeRecord> {
        let _guard = self.mutations.lock();
        self.read_inner(attempt_id)
    }

    /// Observe only the committed pointer. Uncommitted temporary files are not
    /// another transaction and are never replayed or automatically repaired.
    pub fn current_attempt_id(&self) -> Result<Option<Uuid>> {
        let _guard = self.mutations.lock();
        Ok(self.current()?.map(|v| v.attempt_id))
    }

    pub fn helper_result(&self, attempt_id: Uuid) -> Result<Option<HelperResult>> {
        let _guard = self.mutations.lock();
        self.helper_result_inner(&self.read_inner(attempt_id)?)
    }
    pub fn helper_ready(&self, attempt_id: Uuid) -> Result<Option<HelperReady>> {
        let _guard = self.mutations.lock();
        self.helper_ready_inner(&self.read_inner(attempt_id)?)
    }

    pub fn cancel(&self, attempt_id: Uuid, source: &UpgradeSource) -> Result<UpgradeRecord> {
        self.transition(
            attempt_id,
            source,
            UpgradePhase::Cancelled,
            Utc::now(),
            None,
        )
    }
    /// A restarted desktop that owns this runtime may cancel an old Prepared
    /// attempt. Readiness never authorized shutdown; late helpers see Cancelled.
    /// No helper is stopped or restarted. Caller establishes desktop ownership.
    pub fn cancel_prelaunch(
        &self,
        attempt_id: Uuid,
        request_sha256: &str,
    ) -> Result<UpgradeRecord> {
        let _guard = self.mutations.lock();
        let current = self.current()?.context("no current upgrade")?;
        ensure!(
            current.attempt_id == attempt_id && current.request_sha256 == request_sha256,
            "current upgrade/request binding differs"
        );
        let mut record = self.read_inner(attempt_id)?;
        ensure!(
            record.state.request_sha256 == request_sha256,
            "upgrade request binding differs"
        );
        if record.state.phase == UpgradePhase::Cancelled {
            return Ok(record);
        }
        ensure!(
            record.state.phase == UpgradePhase::Prepared,
            "only a Prepared upgrade can be abandoned after restart"
        );
        record.state.phase = UpgradePhase::Cancelled;
        record.state.changed_at = Utc::now();
        persist_record(&self.attempt_dir(attempt_id), "state.json", &record.state)?;
        Ok(record)
    }
    pub fn begin_close(
        &self,
        attempt_id: Uuid,
        source: &UpgradeSource,
        verified_helper: &HelperIdentity,
    ) -> Result<UpgradeRecord> {
        self.transition(
            attempt_id,
            source,
            UpgradePhase::Closing,
            Utc::now(),
            Some(verified_helper),
        )
    }
    /// This records the desktop's successful strict shutdown. It does not
    /// certify process exit, lock release, or WebView descendant termination.
    pub fn shutdown_complete(
        &self,
        attempt_id: Uuid,
        source: &UpgradeSource,
    ) -> Result<UpgradeRecord> {
        self.transition(
            attempt_id,
            source,
            UpgradePhase::ShutdownComplete,
            Utc::now(),
            None,
        )
    }

    /// Claim the continuation once. The desktop verifies its current process
    /// identity/hash and runtime ownership before calling. A later process must
    /// not take over another owner's partially completed Resuming phase.
    pub fn begin_resume(
        &self,
        attempt_id: Uuid,
        candidate: &UpgradeSource,
    ) -> Result<UpgradeRecord> {
        let _guard = self.mutations.lock();
        validate_source(candidate)?;
        let mut record = self.read_inner(attempt_id)?;
        ensure!(
            candidate.exe_sha256 == record.request.candidate.exe_sha256,
            "resume candidate build differs"
        );
        if record.state.phase == UpgradePhase::Consumed {
            return Ok(record);
        }
        self.require_current(&record)?;
        if record.state.phase == UpgradePhase::Resuming {
            ensure!(
                record.state.candidate_identity.as_ref() == Some(candidate),
                "another candidate process owns this resume; manual recovery required"
            );
            return Ok(record);
        }
        ensure!(
            record.state.phase == UpgradePhase::ShutdownComplete
                && same_image(candidate, &record.request.candidate),
            "resume requires the exact staged candidate after successful shutdown"
        );
        let result = self
            .helper_result_inner(&record)?
            .context("helper launch outcome is not yet available")?;
        ensure!(
            matches!(
                result.outcome,
                HelperOutcome::Launching | HelperOutcome::Launched
            ),
            "helper has not admitted candidate launch"
        );
        if result.outcome == HelperOutcome::Launched {
            let launched = result
                .candidate_process
                .context("helper launch has no candidate identity")?;
            ensure!(
                launched.pid == candidate.pid
                    && launched.creation_filetime == candidate.creation_filetime
                    && same_image(&launched, &record.request.candidate),
                "helper launched another candidate process"
            );
        }
        record.state.phase = UpgradePhase::Resuming;
        record.state.candidate_identity = Some(candidate.clone());
        record.state.changed_at = Utc::now();
        persist_record(&self.attempt_dir(attempt_id), "state.json", &record.state)?;
        Ok(record)
    }

    /// Called after the desktop's resume/continuation loop finishes. Stable
    /// continuation IDs remain in the immutable request for delivery dedup.
    pub fn complete_resume(
        &self,
        attempt_id: Uuid,
        candidate: &UpgradeSource,
    ) -> Result<UpgradeRecord> {
        let _guard = self.mutations.lock();
        let mut record = self.read_inner(attempt_id)?;
        self.require_current(&record)?;
        ensure!(
            record.state.candidate_identity.as_ref() == Some(candidate),
            "candidate does not own this resume"
        );
        if record.state.phase == UpgradePhase::Consumed {
            return Ok(record);
        }
        ensure!(
            record.state.phase == UpgradePhase::Resuming,
            "upgrade is not resuming"
        );
        record.state.phase = UpgradePhase::Consumed;
        record.state.changed_at = Utc::now();
        persist_record(&self.attempt_dir(attempt_id), "state.json", &record.state)?;
        Ok(record)
    }

    /// Explicit operator-confirmed terminal note. It neither proves that a
    /// helper/candidate stopped nor grants automatic retry after unknown launch.
    pub fn abandon_held(
        &self,
        attempt_id: Uuid,
        request_sha256: &str,
        operator_confirmed: bool,
    ) -> Result<UpgradeRecord> {
        ensure!(
            operator_confirmed,
            "operator confirmation is required to abandon a held upgrade"
        );
        let _guard = self.mutations.lock();
        let mut record = self.read_inner(attempt_id)?;
        self.require_current(&record)?;
        ensure!(
            record.state.request_sha256 == request_sha256,
            "upgrade request binding differs"
        );
        ensure!(
            !matches!(
                record.state.phase,
                UpgradePhase::Consumed | UpgradePhase::Cancelled
            ),
            "upgrade is already complete or cancelled"
        );
        if record.state.phase == UpgradePhase::Abandoned {
            return Ok(record);
        }
        record.state.phase = UpgradePhase::Abandoned;
        record.state.changed_at = Utc::now();
        persist_record(&self.attempt_dir(attempt_id), "state.json", &record.state)?;
        Ok(record)
    }

    fn require_current(&self, record: &UpgradeRecord) -> Result<()> {
        let current = self.current()?.context("no current upgrade")?;
        ensure!(
            current.attempt_id == record.request.attempt_id
                && current.request_sha256 == record.state.request_sha256,
            "upgrade attempt is no longer current or binding differs"
        );
        Ok(())
    }

    fn transition(
        &self,
        attempt_id: Uuid,
        source: &UpgradeSource,
        target: UpgradePhase,
        now: DateTime<Utc>,
        verified_helper: Option<&HelperIdentity>,
    ) -> Result<UpgradeRecord> {
        let _guard = self.mutations.lock();
        let current = self.current()?.context("no current upgrade")?;
        ensure!(
            current.attempt_id == attempt_id,
            "upgrade attempt is no longer current"
        );
        let mut record = self.read_inner(attempt_id)?;
        ensure!(
            &record.request.source == source,
            "upgrade source process identity changed"
        );
        ensure!(
            current.request_sha256 == record.state.request_sha256,
            "current upgrade binding differs"
        );
        if record.state.phase == target {
            return Ok(record);
        }
        match (record.state.phase, target) {
            (UpgradePhase::Prepared, UpgradePhase::Cancelled) => {}
            (UpgradePhase::Prepared, UpgradePhase::Closing) => {
                ensure!(
                    now >= record.request.notice_until
                        && now + Duration::seconds(CLOSE_REMAINING_SECONDS)
                            <= record.request.expires_at,
                    "upgrade notice is still open or less than five minutes remain"
                );
                let ready = self
                    .helper_ready_inner(&record)?
                    .context("upgrade helper is not ready")?;
                let helper = verified_helper
                    .context("desktop must verify the exact helper process before closing")?;
                ensure!(
                    helper.pid == ready.helper_pid
                        && helper.creation_filetime == ready.helper_creation_filetime,
                    "verified helper identity differs from readiness record"
                );
                ensure!(
                    ready.at_utc >= record.request.prepared_at && ready.at_utc <= now,
                    "helper ready timestamp is outside this attempt"
                );
                ensure!(
                    self.helper_result_inner(&record)?.is_none(),
                    "upgrade helper already reported an outcome"
                );
                ensure!(
                    read_bytes(&record.helper_path, HELPER.len())? == HELPER,
                    "staged upgrade helper changed"
                );
            }
            (UpgradePhase::Closing, UpgradePhase::ShutdownComplete) => {}
            _ => bail!("upgrade phase does not permit this transition"),
        }
        record.state.phase = target;
        record.state.changed_at = now;
        persist_record(&self.attempt_dir(attempt_id), "state.json", &record.state)?;
        Ok(record)
    }

    fn attempt_dir(&self, id: Uuid) -> PathBuf {
        self.directory.join(id.to_string())
    }
    fn predecessor_admits(
        &self,
        current: &CurrentAttempt,
        next_source: &UpgradeSource,
    ) -> Result<()> {
        let previous = self.read_inner(current.attempt_id)?;
        ensure!(
            current.request_sha256 == previous.state.request_sha256,
            "current upgrade binding differs"
        );
        match previous.state.phase {
            UpgradePhase::Cancelled => Ok(()),
            UpgradePhase::Consumed => {
                ensure!(
                    next_source.exe_sha256 == previous.request.candidate.exe_sha256,
                    "new source is not the consumed candidate build"
                );
                Ok(())
            }
            UpgradePhase::ShutdownComplete | UpgradePhase::Abandoned => {
                let result = self
                    .helper_result_inner(&previous)?
                    .context("previous upgrade outcome is unknown")?;
                // The bundled helper sets these outcomes only before the sole
                // candidate Start. After attempting Start, failures are Uncertain.
                // This admits a new owner running the original image; it does
                // not launch that image, reset old attempts, or roll back state.
                if matches!(
                    result.outcome,
                    HelperOutcome::Failed | HelperOutcome::Cancelled | HelperOutcome::Expired
                ) {
                    let original = UpgradeCandidate {
                        exe_path: previous.request.source.exe_path.clone(),
                        exe_sha256: previous.request.source.exe_sha256.clone(),
                    };
                    ensure!(
                        result.candidate_process.is_none() && same_image(next_source, &original),
                        "definite pre-start failure requires the original source image on the new owning desktop"
                    );
                    return Ok(());
                }
                bail!(
                    "previous upgrade is not consumed and has no definite pre-start failure; manual recovery required"
                )
            }
            _ => bail!("another upgrade is prepared or closing; cancel it before preparing again"),
        }
    }
    fn no_helper_activity(&self, attempt: &Path) -> Result<()> {
        for name in [
            "helper-claim.json",
            "helper-ready.json",
            "helper-result.json",
        ] {
            for path in [attempt.join(name), attempt.join(format!("{name}.tmp"))] {
                ensure_existing_path_chain_has_no_reparse_points(&path)?;
                ensure!(
                    !path.try_exists()?,
                    "helper activity exists; incomplete attempt is not rewritten"
                );
            }
        }
        Ok(())
    }
    fn current(&self) -> Result<Option<CurrentAttempt>> {
        let path = self.directory.join("current.json");
        if !path.try_exists()? {
            return Ok(None);
        }
        let current: CurrentAttempt = read_bounded_json(&path, RECORD_MAX_BYTES)?;
        ensure!(
            current.schema_version == SCHEMA_VERSION && !current.attempt_id.is_nil(),
            "unsupported current upgrade record"
        );
        validate_hash(&current.request_sha256)?;
        Ok(Some(current))
    }
    fn read_inner(&self, attempt_id: Uuid) -> Result<UpgradeRecord> {
        let request_path = self.request_path(attempt_id);
        let bytes = read_bytes(&request_path, REQUEST_MAX_BYTES)?;
        let request: UpgradeRequest =
            serde_json::from_slice(&bytes).context("incomplete or malformed upgrade request")?;
        validate_request(&request)?;
        ensure!(
            request.attempt_id == attempt_id
                && canonical_destination_without_reparse_points(&request.runtime_dir)?
                    == self.runtime,
            "upgrade request path/runtime binding differs"
        );
        let state: UpgradeState = read_bounded_json(
            &self.attempt_dir(attempt_id).join("state.json"),
            RECORD_MAX_BYTES,
        )
        .context("upgrade preparation or transition is incomplete")?;
        ensure!(
            state.schema_version == SCHEMA_VERSION
                && state.attempt_id == attempt_id
                && state.request_sha256 == digest(&bytes),
            "upgrade state/request binding differs"
        );
        if matches!(state.phase, UpgradePhase::Resuming | UpgradePhase::Consumed) {
            let candidate = state
                .candidate_identity
                .as_ref()
                .context("resume state has no candidate owner")?;
            validate_source(candidate)?;
            ensure!(
                candidate.exe_sha256 == request.candidate.exe_sha256,
                "resume state candidate build differs"
            );
        }
        Ok(UpgradeRecord {
            request,
            state,
            request_path,
            helper_path: self.helper_path(attempt_id),
        })
    }
    fn helper_result_inner(&self, record: &UpgradeRecord) -> Result<Option<HelperResult>> {
        let path = self
            .attempt_dir(record.request.attempt_id)
            .join("helper-result.json");
        if !path.try_exists()? {
            return Ok(None);
        }
        let result: HelperResult = read_bounded_json(&path, RECORD_MAX_BYTES)?;
        binding(
            result.schema_version,
            result.attempt_id,
            &result.request_sha256,
            record,
        )?;
        if let Some(process) = &result.candidate_process {
            validate_source(process)?;
        }
        Ok(Some(result))
    }
    fn helper_ready_inner(&self, record: &UpgradeRecord) -> Result<Option<HelperReady>> {
        let path = self
            .attempt_dir(record.request.attempt_id)
            .join("helper-ready.json");
        if !path.try_exists()? {
            return Ok(None);
        }
        let ready: HelperReady = read_bounded_json(&path, RECORD_MAX_BYTES)?;
        binding(
            ready.schema_version,
            ready.attempt_id,
            &ready.request_sha256,
            record,
        )?;
        validate_process_fields(ready.helper_pid, &ready.helper_creation_filetime)?;
        Ok(Some(ready))
    }
}

fn binding(schema: u32, id: Uuid, hash: &str, record: &UpgradeRecord) -> Result<()> {
    ensure!(
        schema == SCHEMA_VERSION
            && id == record.request.attempt_id
            && hash == record.state.request_sha256,
        "upgrade helper/request binding differs"
    );
    Ok(())
}
fn validate_process_fields(pid: u32, creation: &str) -> Result<()> {
    ensure!(
        pid != 0
            && !creation.is_empty()
            && creation.len() <= 20
            && creation.bytes().all(|b| b.is_ascii_digit())
            && creation.parse::<u64>().is_ok_and(|n| n > 0),
        "upgrade process identity requires PID and exact creation FILETIME"
    );
    Ok(())
}
fn validate_hash(hash: &str) -> Result<()> {
    ensure!(
        hash.len() == 64
            && hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "upgrade executable/request SHA256 must be lowercase hex"
    );
    Ok(())
}
fn validate_path(path: &Path) -> Result<()> {
    let text = path.to_str().context("upgrade path is not valid Unicode")?;
    ensure!(
        path.is_absolute()
            && text.len() <= 32 * 1024
            && !text.contains(['\0', '\r', '\n'])
            && !path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "upgrade path must be absolute, bounded, and contain no parent traversal"
    );
    Ok(())
}
fn validate_source(source: &UpgradeSource) -> Result<()> {
    validate_process_fields(source.pid, &source.creation_filetime)?;
    validate_path(&source.exe_path)?;
    validate_hash(&source.exe_sha256)
}
fn same_image(source: &UpgradeSource, candidate: &UpgradeCandidate) -> bool {
    fn key(path: &Path) -> String {
        let text = path.to_string_lossy();
        #[cfg(windows)]
        {
            text.strip_prefix("\\\\?\\")
                .unwrap_or(&text)
                .replace('/', "\\")
                .to_ascii_lowercase()
        }
        #[cfg(not(windows))]
        {
            text.into_owned()
        }
    }
    source.exe_sha256 == candidate.exe_sha256 && key(&source.exe_path) == key(&candidate.exe_path)
}
fn validate_request(request: &UpgradeRequest) -> Result<()> {
    ensure!(
        request.schema_version == SCHEMA_VERSION && !request.attempt_id.is_nil(),
        "unsupported upgrade request"
    );
    validate_source(&request.source)?;
    validate_path(&request.candidate.exe_path)?;
    validate_hash(&request.candidate.exe_sha256)?;
    for path in [
        &request.runtime_dir,
        &request.profile_dir,
        &request.task_folder,
    ] {
        validate_path(path)?;
    }
    if let Some(value) = &request.personal_repo_override {
        if value != "off" {
            validate_path(Path::new(value))?;
        }
    }
    ensure!(
        request.notice_until >= request.prepared_at + Duration::seconds(NOTICE_SECONDS)
            && request.notice_until < request.expires_at
            && request.expires_at <= request.prepared_at + Duration::seconds(EXPIRY_SECONDS),
        "invalid upgrade notice/expiry window"
    );
    ensure!(
        !request.members.is_empty() && request.members.len() <= MAX_MEMBERS,
        "upgrade running set exceeds bounds or is empty"
    );
    let mut sessions = HashSet::new();
    let mut continuations = HashSet::new();
    for member in &request.members {
        ensure!(
            !member.session_id.is_nil()
                && !member.run_id.is_nil()
                && member.room_id.is_none_or(|id| !id.is_nil())
                && !member.continuation_id.is_nil()
                && sessions.insert(member.session_id)
                && continuations.insert(member.continuation_id),
            "upgrade member/continuation identity is missing or duplicated"
        );
        if member.resume_mode == UpgradeResumeMode::Exact {
            let conversation = member
                .conversation_id
                .as_deref()
                .context("exact resume member has no conversation ID")?;
            ensure!(
                member.room_id.is_some()
                    && !matches!(
                        member.driver,
                        DriverKind::GenericTerminal | DriverKind::Prime
                    ),
                "terminal, Prime and roomless members must be marked interrupted_without_resume"
            );
            ensure!(
                !conversation.trim().is_empty()
                    && conversation.len() <= 4096
                    && !conversation.chars().any(char::is_control),
                "upgrade member has no bounded exact conversation ID"
            );
        } else {
            ensure!(
                member.conversation_id.is_none(),
                "interrupted member must not invent a conversation ID"
            );
        }
        if let Some(task) = &member.task_ref {
            ensure!(
                !task.task_id.trim().is_empty()
                    && task.task_id.len() <= 256
                    && task.revision > 0
                    && !task.task_id.chars().any(char::is_control),
                "invalid upgrade task reference"
            );
        }
    }
    ensure!(
        request
            .members
            .iter()
            .any(|m| m.session_id == request.requester_session_id
                && m.task_ref.is_some()
                && m.resume_mode == UpgradeResumeMode::Exact),
        "requesting member requires exact resume and its task checkpoint reference"
    );
    Ok(())
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn reject_installed_v2_overlap(runtime: &Path, local: &Path) -> Result<()> {
    let local = canonical_destination_without_reparse_points(local)?;
    ensure!(
        !crate::paths_overlap(runtime, &local.join("io.prim1.v2")),
        "upgrade runtime overlaps the installed v2 storage"
    );
    Ok(())
}

/// Atomic committed JSON is authoritative. A failed temporary write is retained
/// but never blocks a later retry, gets replayed, or changes execution authority.
fn persist_record(directory: &Path, name: &str, value: &impl Serialize) -> Result<()> {
    let destination = directory.join(name);
    ensure_existing_path_chain_has_no_reparse_points(&destination)?;
    let temporary = directory.join(format!(".{name}.{}.partial", Uuid::new_v4()));
    let bytes = json_bytes(value, RECORD_MAX_BYTES)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    restrict_path_to_current_user(&temporary, false)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    crate::replace_catalog_file(&temporary, &destination, "upgrade record")
}
fn json_bytes(value: &impl Serialize, max: usize) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    ensure!(
        bytes.len() <= max,
        "upgrade record exceeds its storage bound"
    );
    Ok(bytes)
}
fn read_bytes(path: &Path, max: usize) -> Result<Vec<u8>> {
    ensure_existing_path_chain_has_no_reparse_points(path)?;
    let file = File::open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "upgrade record is not a regular file"
    );
    let mut bytes = Vec::new();
    file.take(max as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= max,
        "upgrade record exceeds its storage bound"
    );
    Ok(bytes)
}
// These two staging helpers run only before any immutable request is published.
// Their unvalidated .partial files may be rewritten; completed files never are.
fn stage_prelaunch_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    ensure_existing_path_chain_has_no_reparse_points(path)?;
    if path.try_exists()? {
        ensure!(
            read_bytes(path, bytes.len())? == bytes,
            "completed staged helper differs; preserved"
        );
        return Ok(());
    }
    let partial = path.with_extension("partial");
    ensure_existing_path_chain_has_no_reparse_points(&partial)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&partial)?;
    restrict_path_to_current_user(&partial, false)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&partial, path)?;
    Ok(())
}

fn stage_candidate(input_path: &Path, candidate: &UpgradeCandidate) -> Result<()> {
    ensure_existing_path_chain_has_no_reparse_points(input_path)?;
    ensure_existing_path_chain_has_no_reparse_points(&candidate.exe_path)?;
    // A completed staged copy is reusable only when its bytes still match the
    // exact request. Do not require the original build output to still exist.
    let existing = candidate.exe_path.try_exists()?;
    let source = if existing {
        &candidate.exe_path
    } else {
        input_path
    };
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ);
    }
    let mut input = options
        .open(source)
        .context("cannot retain upgrade candidate for reading")?;
    let metadata = input.metadata()?;
    ensure!(
        metadata.is_file() && (2..=CANDIDATE_MAX_BYTES).contains(&metadata.len()),
        "upgrade candidate must be a regular file of at most 512 MiB"
    );
    let directory = candidate
        .exe_path
        .parent()
        .context("candidate has no parent")?;
    fs::create_dir_all(directory)?;
    restrict_path_to_current_user(directory, true)?;
    let partial = candidate.exe_path.with_extension("partial");
    ensure_existing_path_chain_has_no_reparse_points(&partial)?;
    let mut output = if existing {
        None
    } else {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&partial)?;
        restrict_path_to_current_user(&partial, false)?;
        Some(file)
    };
    let mut signature = [0; 2];
    input.read_exact(&mut signature)?;
    ensure!(
        signature == *b"MZ",
        "upgrade candidate lacks Windows executable header"
    );
    let mut hash = Sha256::new();
    hash.update(signature);
    if let Some(file) = &mut output {
        file.write_all(&signature)?;
    }
    let mut count = 2u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        let length = input.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        count += length as u64;
        ensure!(
            count <= CANDIDATE_MAX_BYTES,
            "upgrade candidate grew beyond its bound"
        );
        hash.update(&buffer[..length]);
        if let Some(file) = &mut output {
            file.write_all(&buffer[..length])?;
        }
    }
    ensure!(
        count == metadata.len() && format!("{:x}", hash.finalize()) == candidate.exe_sha256,
        "upgrade candidate hash/size differs; unvalidated partial preserved for retry"
    );
    if let Some(file) = output {
        file.sync_all()?;
        drop(file);
        fs::rename(&partial, &candidate.exe_path)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "upgrade_tests.rs"]
mod tests;
