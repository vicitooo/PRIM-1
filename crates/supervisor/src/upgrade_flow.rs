//! A108 desktop wiring of the same-runtime upgrade transaction (`upgrade.rs`):
//! the maintain-gated pane request, the running-set capture and its blockers,
//! the cancellable notice, close admission, and the new build's exact resume
//! with one durable continuation per member. Delivery, resume and shutdown are
//! the existing paths; the desktop supplies only the strict close, process
//! exit, the helper start and the stable launcher (`UpgradeHost`).
use super::*;
use crate::upgrade::{
    CLOSE_REMAINING_SECONDS, HelperIdentity, HelperOutcome, NOTICE_SECONDS, TaskRef, UpgradeCandidate, UpgradeMember,
    UpgradePhase, UpgradeRecord, UpgradeRequest, UpgradeResumeMode, UpgradeSource, UpgradeStore,
};
use shared_types::{UpgradeAdmission, UpgradeAttemptSummary, UpgradeStage, UpgradeStatus};
use std::ffi::OsString;

pub const ATTEMPT_ENV: &str = "PRIM1_UPGRADE_ATTEMPT_ID";
pub const REQUEST_FILE_ENV: &str = "PRIM1_UPGRADE_REQUEST_FILE";
const MAINTAIN_REFUSAL: &str = "the operator has not let this member upgrade PRIM-1: ask Victor to grant \"Can upgrade PRIM-1\" (maintain) in its Edit dialog, or propose the upgrade as a task";
pub(crate) const UPGRADE_CLOSING_REFUSAL: &str = "PRIM-1 is restarting for an upgrade";
/// Like the desktop's boot resume: at most this many members start at once.
const RESUME_CONCURRENCY: usize = 2;
/// A member still starting after this long keeps no slot from the next one.
#[cfg(not(test))]
const RESUME_SETTLE_CEILING: Duration = Duration::from_secs(120);
#[cfg(test)]
const RESUME_SETTLE_CEILING: Duration = Duration::ZERO;
/// The window normally asks for the resume once it is shown; this starts it
/// anyway if the window never does.
const RESUME_TRIGGER_FALLBACK: Duration = Duration::from_secs(60);
const CLOSE_POLL: Duration = Duration::from_millis(500);
/// Close admission gives up this long before the transaction's own floor.
const CLOSE_ADMISSION_MARGIN_SECONDS: i64 = 15;
const HELPER_REGISTER_TIMEOUT: Duration = Duration::from_secs(120);

/// What the desktop does for the upgrade. Process identity, the helper start
/// and the liveness probe default to the real OS paths; tests replace them.
pub trait UpgradeHost: Send + Sync + 'static {
    fn process_identity(&self) -> Result<UpgradeSource> {
        current_process_identity()
    }
    /// Start the staged helper hidden with `-Register`; returns when it exits.
    fn register_helper(&self, helper: &Path, request: &Path) -> Result<()> {
        register_helper_hidden(helper, request)
    }
    /// True when `pid` is alive with exactly this creation FILETIME.
    fn process_alive(&self, pid: u32, creation_filetime: &str) -> Result<bool> {
        process_alive(pid, creation_filetime)
    }
    /// The desktop's own shutdown, strict about the final catalog write.
    fn strict_shutdown(&self) -> Result<()>;
    /// Exit promptly; called only after `shutdown_complete` was recorded.
    fn exit_for_upgrade(&self);
    /// The new build consumed its attempt after its window was shown.
    fn resume_consumed(&self, record: &UpgradeRecord);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeStartup {
    pub attempt_id: Uuid,
    pub request_file: PathBuf,
}

pub struct UpgradeEnvironment {
    /// The effective WebView2 user data folder.
    pub profile_dir: PathBuf,
    /// `PRIM1_PERSONAL_REPO` exactly as set, including QA's `off`.
    pub personal_repo_override: Option<String>,
    /// The candidate's startup variables; an Err is shown and ignored.
    pub startup: std::result::Result<Option<UpgradeStartup>, String>,
}

/// `PRIM1_UPGRADE_ATTEMPT_ID` and `PRIM1_UPGRADE_REQUEST_FILE`: both or
/// neither, a canonical non-nil UUID and an absolute file path.
pub fn parse_upgrade_startup(
    attempt: Option<OsString>,
    request_file: Option<OsString>,
) -> std::result::Result<Option<UpgradeStartup>, String> {
    let set = |value: Option<OsString>| value.filter(|value| !value.is_empty());
    match (set(attempt), set(request_file)) {
        (None, None) => Ok(None),
        (Some(_), None) | (None, Some(_)) => Err(format!("{ATTEMPT_ENV} and {REQUEST_FILE_ENV} must be set together")),
        (Some(attempt), Some(file)) => {
            let text = attempt.to_str().ok_or_else(|| format!("{ATTEMPT_ENV} is not a UUID"))?;
            let attempt_id = Uuid::parse_str(text).map_err(|_| format!("{ATTEMPT_ENV} is not a UUID"))?;
            if attempt_id.is_nil() || attempt_id.to_string() != text {
                return Err(format!("{ATTEMPT_ENV} must be a canonical, non-nil UUID"));
            }
            let request_file = PathBuf::from(file);
            if !request_file.is_absolute() {
                return Err(format!("{REQUEST_FILE_ENV} must be an absolute path"));
            }
            Ok(Some(UpgradeStartup { attempt_id, request_file }))
        }
    }
}

pub struct UpgradeFlow {
    store: UpgradeStore,
    profile_dir: PathBuf,
    personal_repo_override: Option<String>,
    host: Arc<dyn UpgradeHost>,
    /// One request admission at a time.
    requests: Mutex<()>,
    state: Mutex<FlowState>,
    #[cfg(test)]
    backdate: Mutex<Option<chrono::Duration>>,
    #[cfg(test)]
    stop_before_complete_resume: AtomicBool,
}

#[derive(Default)]
struct FlowState {
    identity: Option<UpgradeSource>,
    summary: Option<UpgradeAttemptSummary>,
    status: Option<UpgradeStatus>,
    /// The window's report: does any room composer hold unsent text?
    room_drafts_open: Option<bool>,
    /// The attempt whose notice the banner and room show, and when that
    /// visible notice ends: a full notice after it was published.
    notice: Option<(Uuid, DateTime<Utc>)>,
    pending_resume: Option<UpgradeRecord>,
    resume_requested: bool,
}

impl UpgradeFlow {
    fn identity(&self) -> Result<UpgradeSource> {
        let mut state = self.state.lock();
        if let Some(identity) = &state.identity {
            return Ok(identity.clone());
        }
        let identity = self.host.process_identity().context("this PRIM-1 process could not identify itself")?;
        state.identity = Some(identity.clone());
        Ok(identity)
    }

    fn note(&self, record: &UpgradeRecord) {
        self.state.lock().summary = Some(UpgradeAttemptSummary {
            attempt_id: record.request.attempt_id,
            phase: phase_name(record.state.phase).into(),
        });
    }

    #[cfg(test)]
    fn apply_backdate(&self, request: &mut UpgradeRequest) {
        if let Some(shift) = *self.backdate.lock() {
            request.prepared_at -= shift;
            request.notice_until -= shift;
            request.expires_at -= shift;
        }
    }
}

#[derive(Default)]
struct Capture {
    members: Vec<UpgradeMember>,
    labels: HashMap<SessionId, String>,
    blockers: Vec<String>,
}

fn phase_name(phase: UpgradePhase) -> &'static str {
    match phase {
        UpgradePhase::Prepared => "prepared",
        UpgradePhase::Cancelled => "cancelled",
        UpgradePhase::Closing => "closing",
        UpgradePhase::ShutdownComplete => "shutdown_complete",
        UpgradePhase::Resuming => "resuming",
        UpgradePhase::Consumed => "consumed",
        UpgradePhase::Abandoned => "abandoned",
    }
}

fn short(value: impl std::fmt::Display) -> String {
    value.to_string().chars().take(8).collect()
}

fn same_path_text(left: &Path, right: &Path) -> bool {
    fn key(path: &Path) -> String {
        let text = path.to_string_lossy();
        let text = text.strip_prefix(r"\\?\").unwrap_or(&text).replace('/', "\\");
        let text = text.trim_end_matches('\\');
        if cfg!(windows) { text.to_ascii_lowercase() } else { text.to_owned() }
    }
    key(left) == key(right)
}

/// Why a recorded attempt is held for the operator, if it is: it neither
/// finished nor definitely failed before the new build started.
fn held_reason(record: &UpgradeRecord, outcome: Option<HelperOutcome>, identity: Option<&UpgradeSource>) -> Option<String> {
    let attempt = short(record.request.attempt_id);
    let prestart_failure = matches!(outcome, Some(HelperOutcome::Failed | HelperOutcome::Cancelled | HelperOutcome::Expired));
    let described = match outcome {
        None => "no outcome recorded".to_string(),
        Some(outcome) => format!("helper outcome {outcome:?}").to_lowercase(),
    };
    match record.state.phase {
        // A close that never completed stays recorded as closing; only the
        // operator's note lets another upgrade be prepared.
        UpgradePhase::Closing if prestart_failure => Some(format!(
            "Upgrade {attempt} did not start the new build ({described}) and is still recorded as closing. Abandon it to allow another upgrade.")),
        UpgradePhase::ShutdownComplete if prestart_failure => None,
        UpgradePhase::Closing | UpgradePhase::ShutdownComplete => Some(format!(
            "Upgrade {attempt} is held: PRIM-1 closed for it, but the new build's start is not confirmed ({described}). Nothing is started again automatically.")),
        UpgradePhase::Resuming if record.state.candidate_identity.as_ref() != identity => Some(format!(
            "Upgrade {attempt} is held: the new build stopped while it resumed its members; they are not resumed again."
        )),
        _ => None,
    }
}

fn running_set_change(recorded: &[UpgradeMember], now: &[UpgradeMember], label: impl Fn(&SessionId) -> String) -> Option<String> {
    let key = |member: &UpgradeMember| (member.session_id, member.run_id, member.generation);
    let before: HashSet<_> = recorded.iter().map(key).collect();
    let after: HashSet<_> = now.iter().map(key).collect();
    if before == after {
        return None;
    }
    let mut changes: Vec<String> = recorded.iter().filter(|member| !after.contains(&key(member)))
        .map(|member| format!("{} stopped or restarted", label(&member.session_id))).collect();
    changes.extend(now.iter().filter(|member| !before.contains(&key(member)))
        .map(|member| format!("{} started", label(&member.session_id))));
    Some(changes.join(", "))
}

/// A member whose exact resume changed since admission (W2 followed a /clear
/// or /new, or it changed rooms): the recorded request is stale. A
/// conversation not known now is a capture blocker, so closing waits for it.
fn resume_change(recorded: &[UpgradeMember], now: &[UpgradeMember], label: impl Fn(&SessionId) -> String) -> Option<String> {
    let changes: Vec<String> = recorded.iter().filter_map(|before| {
        let after = now.iter().find(|member| member.session_id == before.session_id)?;
        if (after.room_id, after.resume_mode) != (before.room_id, before.resume_mode) {
            Some(format!("{} changed room during the notice", label(&before.session_id)))
        } else if after.conversation_id.is_some() && after.conversation_id != before.conversation_id {
            Some(format!("{} changed conversation during the notice", label(&before.session_id)))
        } else {
            None
        }
    }).collect();
    (!changes.is_empty()).then(|| format!("{}; ask again", changes.join(", ")))
}

fn continuation_text(record: &UpgradeRecord, member: &UpgradeMember) -> String {
    match &member.task_ref {
        Some(task) if member.session_id == record.request.requester_session_id => format!(
            "PRIM-1 restarted on build {} (upgrade {}). Continue task {} (revision {}) from its current task state.",
            short(&record.request.candidate.exe_sha256), short(record.request.attempt_id), task.task_id, task.revision
        ),
        _ => "PRIM-1 restarted on a new build. Continue your current task where you left off.".into(),
    }
}

impl SupervisorHandle {
    /// Desktop startup (not a held trial): open this runtime's upgrade store
    /// and settle what an earlier attempt left. An Err means self-upgrade is
    /// unavailable here; PRIM-1 still starts normally.
    pub fn install_upgrade(&self, environment: UpgradeEnvironment, host: Arc<dyn UpgradeHost>) -> Result<()> {
        self.ensure_active()?;
        let startup = environment.startup;
        let store = match UpgradeStore::open(&self.inner.runtime_dir) {
            Ok(store) => store,
            Err(error) => {
                if !matches!(startup, Ok(None)) {
                    self.upgrade_log(LogLevel::Error, format!(
                        "This build was started for an upgrade, but the runtime's upgrade store is unavailable ({error:#}). PRIM-1 started normally; no member was resumed by the upgrade."));
                }
                return Err(error.context("self-upgrade is unavailable for this runtime"));
            }
        };
        let flow = Arc::new(UpgradeFlow {
            store,
            profile_dir: environment.profile_dir,
            personal_repo_override: environment.personal_repo_override,
            host,
            requests: Mutex::new(()),
            state: Mutex::new(FlowState::default()),
            #[cfg(test)]
            backdate: Mutex::new(None),
            #[cfg(test)]
            stop_before_complete_resume: AtomicBool::new(false),
        });
        *self.inner.upgrade.write() = Some(flow.clone());
        match startup {
            Ok(Some(startup)) => self.begin_candidate_resume(&flow, startup),
            Ok(None) => self.settle_attempt_at_startup(&flow),
            Err(reason) => {
                self.upgrade_log(LogLevel::Error, format!(
                    "Upgrade startup settings were ignored: {reason}. PRIM-1 started normally; nothing was resumed by an upgrade."));
                self.settle_attempt_at_startup(&flow);
            }
        }
        Ok(())
    }

    pub(crate) fn upgrade_flow(&self) -> Option<Arc<UpgradeFlow>> {
        self.inner.upgrade.read().clone()
    }

    /// Ordinary recovery must not send a second continuation for the exact
    /// run captured by a committed upgrade. A notice or cancellation owns none.
    pub(crate) fn upgrade_owns_interrupted_run(&self, session_id: SessionId, run_id: Uuid) -> Result<bool> {
        if self.recorded_upgrade_owns_run(session_id, run_id) { return Ok(true); }
        let Some(flow) = self.upgrade_flow() else { return Ok(false) };
        let Some(attempt) = flow.store.current_attempt_id()? else { return Ok(false) };
        let record = flow.store.read(attempt)?;
        let Some(member) = record.request.members.iter().find(|member| member.session_id == session_id
            && member.run_id == run_id && member.resume_mode == UpgradeResumeMode::Exact) else { return Ok(false); };
        if matches!(record.state.phase, UpgradePhase::Closing | UpgradePhase::ShutdownComplete | UpgradePhase::Resuming) {
            return Ok(true);
        }
        // Consumed describes the attempt, not success for every captured member.
        // A failed member or refused append must not permanently claim its run.
        if record.state.phase != UpgradePhase::Consumed { return Ok(false); }
        let rooms = self.inner.rooms.lock();
        let Some(room) = member.room_id.and_then(|room_id| rooms.get(room_id)) else { return Ok(false); };
        // A poisoned journal cannot prove absence. The caller suppresses the
        // prompt on this error while still permitting an eligible exact start.
        Ok(room.replayed_continuation(&format!("upgrade-{}", member.continuation_id),
            &continuation_text(&record, member), &[member.session_id])?.is_some())
    }

    pub(crate) fn upgrade_reserves_boot_resume(&self, session_id: SessionId) -> Result<bool> {
        let Some(flow) = self.upgrade_flow() else { return Ok(false) };
        let Some(attempt) = flow.store.current_attempt_id()? else { return Ok(false) };
        let record = flow.store.read(attempt)?;
        Ok(matches!(record.state.phase, UpgradePhase::Closing | UpgradePhase::ShutdownComplete | UpgradePhase::Resuming)
            && record.request.members.iter().any(|member| member.session_id == session_id
                && member.resume_mode == UpgradeResumeMode::Exact))
    }

    pub fn upgrade_status(&self) -> Option<UpgradeStatus> {
        self.upgrade_flow().and_then(|flow| flow.state.lock().status.clone())
    }

    pub(crate) fn upgrade_attempt_summary(&self) -> Option<UpgradeAttemptSummary> {
        self.upgrade_flow().and_then(|flow| flow.state.lock().summary.clone())
    }

    pub(crate) fn ensure_not_upgrade_closing(&self) -> Result<()> {
        if self.inner.upgrade_closing.load(Ordering::Acquire) {
            bail!(UPGRADE_CLOSING_REFUSAL);
        }
        Ok(())
    }

    /// The window reports whether any room composer holds unsent text. Until
    /// it has, an upgrade treats the drafts as unknown and does not close.
    pub fn report_operator_room_drafts(&self, open: bool) {
        if let Some(flow) = self.upgrade_flow() {
            flow.state.lock().room_drafts_open = Some(open);
        }
    }

    pub(super) fn pane_maintain_locked(caller: &PaneCaller, slots: &SessionRegistry) -> Result<()> {
        Self::validate_pane_caller_locked(caller, slots)?;
        if slots.get_by_id(caller.session_id).is_some_and(|slot| slot.permissions.maintain) {
            Ok(())
        } else {
            Err(anyhow!(MAINTAIN_REFUSAL))
        }
    }

    /// Runs on a blocking worker: it reads tasks and stages the candidate.
    pub(super) fn apply_upgrade_request(&self, caller: &PaneCaller, request: SidebandRequest) -> SidebandResponse {
        if let Err(error) = self.authorize_sideband_request(caller, &request) {
            return Self::rejected_sideband_response(error.to_string());
        }
        #[cfg(test)]
        self.run_sideband_after_initial_authorization_for_tests();
        let SidebandRequest::UpgradeRequest { candidate_exe, sha256, task_id, task_revision } = request else {
            return Self::rejected_sideband_response("not an upgrade request");
        };
        match self.request_upgrade(caller, &candidate_exe, &sha256, &task_id, task_revision) {
            Ok(admission) => SidebandResponse {
                ok: true,
                message: format!(
                    "upgrade {} admitted: PRIM-1 closes after the notice ends at {} unless Victor cancels; this conversation then resumes on the new build and receives one continuation",
                    short(admission.attempt_id), admission.notice_until
                ),
                timed_out: false,
                payload: Some(SidebandResponsePayload::Upgrade { admission }),
                request_id: None,
            },
            Err(error) => Self::rejected_sideband_response(format!("{error:#}")),
        }
    }

    fn request_upgrade(
        &self, caller: &PaneCaller, candidate_exe: &str, sha256: &str, task_id: &str, task_revision: u64,
    ) -> Result<UpgradeAdmission> {
        self.ensure_active()?;
        self.ensure_not_upgrade_closing()?;
        let flow = self.upgrade_flow().ok_or_else(|| anyhow!(
            "self-upgrade is unavailable in this PRIM-1 (its runtime has no upgrade store); propose the upgrade as a task"))?;
        let candidate = PathBuf::from(candidate_exe);
        ensure_upgrade(candidate.is_absolute(), "candidate_exe must be the absolute path of the tested build")?;
        ensure_upgrade(candidate.is_file(), &format!("candidate_exe {} is not a file", candidate.display()))?;
        let sha256 = sha256.to_ascii_lowercase();
        ensure_upgrade(sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit()),
            "sha256 must be the build's 64 hex digit SHA-256")?;
        ensure_upgrade(task_revision >= 1, "task_revision must be 1 or more")?;
        let tasks = self.task_store().ok_or_else(|| anyhow!("the task folder is not open, so the task checkpoint cannot be checked"))?;
        let task = tasks.get(task_id).map_err(|error| anyhow!("task {task_id} cannot be read: {error}"))?;
        ensure_upgrade(task_revision <= task.revision, &format!(
            "task {task_id} is at revision {}; revision {task_revision} is not one of its checkpoints", task.revision))?;
        let task_folder = tasks.dir().to_path_buf();
        drop(tasks);

        let _one = flow.requests.lock();
        if let Some(current) = flow.store.current_attempt_id()? {
            let record = flow.store.read(current)?;
            if matches!(record.state.phase, UpgradePhase::Prepared | UpgradePhase::Closing) {
                bail!("upgrade {} is already {}; wait for it or ask Victor to cancel it", short(current), phase_name(record.state.phase));
            }
        }
        let task_ref = TaskRef { task_id: task_id.to_owned(), revision: task_revision };
        let capture = self.capture_upgrade_members(&flow, Some((caller, task_ref)))?;
        if !capture.blockers.is_empty() {
            bail!("PRIM-1 cannot upgrade now: {}. Nothing was stopped; ask again once these are resolved", capture.blockers.join("; "));
        }
        let source = flow.identity()?;
        #[cfg_attr(not(test), allow(unused_mut))]
        let mut request = UpgradeRequest::new(
            source.clone(),
            UpgradeCandidate { exe_path: candidate, exe_sha256: sha256 },
            self.inner.runtime_dir.clone(),
            flow.profile_dir.clone(),
            task_folder,
            flow.personal_repo_override.clone(),
            caller.session_id,
            capture.members,
        );
        #[cfg(test)]
        flow.apply_backdate(&mut request);
        let record = flow.store.prepare(request).context("the upgrade could not be prepared; nothing was stopped")?;
        flow.note(&record);
        let attempt = record.request.attempt_id;
        if let Err(error) = flow.host.register_helper(&record.helper_path, &record.request_path) {
            if let Ok(cancelled) = flow.store.cancel(attempt, &source) {
                flow.note(&cancelled);
            }
            bail!("the upgrade helper could not be started ({error:#}); the attempt was cancelled and nothing was stopped");
        }
        let label = |id: &SessionId| capture.labels.get(id).cloned().unwrap_or_else(|| short(id));
        let (resumed, interrupted): (Vec<_>, Vec<_>) = record.request.members.iter()
            .partition(|member| member.resume_mode == UpgradeResumeMode::Exact);
        let resumed_members: Vec<String> = resumed.iter().map(|member| label(&member.session_id)).collect();
        let interrupted_members: Vec<String> = interrupted.iter().map(|member| label(&member.session_id)).collect();
        let requester = label(&caller.session_id);
        let not_resumed = if interrupted_members.is_empty() { String::new() } else {
            format!(" Not restarted: {} (terminal, Prime or outside a room).", interrupted_members.join(", "))
        };
        let notice = format!(
            "PRIM-1 restarts in {} s to upgrade to build {} for {requester} (upgrade {}). Every running member stops, then resumes its own conversation on the new build.{not_resumed} Victor can cancel from the banner until then.",
            NOTICE_SECONDS, short(&record.request.candidate.exe_sha256), short(attempt));
        self.post_upgrade_notice(&record, &format!("upgrade-notice-{attempt}"), &notice);
        // The notice Victor sees runs in full from now, however long the
        // helper took to register; the banner shows the same end.
        let notice_until = record.request.notice_until.max(Utc::now() + chrono::Duration::seconds(NOTICE_SECONDS));
        flow.state.lock().notice = Some((attempt, notice_until));
        let message = format!(
            "{requester} asked to restart PRIM-1 on build {}. Every running member stops, then resumes its own conversation.{not_resumed}",
            short(&record.request.candidate.exe_sha256));
        self.set_upgrade_status(&flow, Some(self.status_for(&record, UpgradeStage::Notice, message)));
        self.upgrade_log(LogLevel::Info, format!("Upgrade {} admitted for {requester}: notice until {}.", short(attempt), notice_until.to_rfc3339()));
        self.spawn_close_admission(attempt);
        Ok(UpgradeAdmission {
            attempt_id: attempt,
            notice_until: notice_until.to_rfc3339(),
            expires_at: record.request.expires_at.to_rfc3339(),
            resumed_members,
            interrupted_members,
        })
    }

    /// Every active member with the exact conversation the Resume path would
    /// use, plus everything that must not be guessed away before closing.
    fn capture_upgrade_members(&self, flow: &UpgradeFlow, requester: Option<(&PaneCaller, TaskRef)>) -> Result<Capture> {
        let mut capture = Capture::default();
        let mut codex_checks = Vec::new();
        {
            let rooms = self.inner.rooms.lock();
            let slots = self.inner.slots.lock();
            if let Some((caller, _)) = &requester {
                Self::pane_maintain_locked(caller, &slots)?;
            }
            for slot in slots.ordered_slots() {
                let label = slot.definition.label.clone();
                if slot.spawn_in_flight.is_some() || slot.lifecycle_operation.is_some() || slot.termination_uncertain
                    || (slot.running.is_some() && matches!(slot.state, LifecycleState::Starting | LifecycleState::Restarting)) {
                    capture.blockers.push(format!("{label} is starting or stopping"));
                }
                let Some(running) = slot.running.as_ref() else { continue };
                let Some(run_id) = slot.run_id else {
                    capture.blockers.push(format!("{label}'s run is changing"));
                    continue;
                };
                capture.labels.insert(slot.session_id, label.clone());
                let room_id = rooms.room_by_session.get(&slot.session_id).copied();
                let driver = slot.definition.driver;
                let interrupted = room_id.is_none() || matches!(driver, DriverKind::GenericTerminal | DriverKind::Prime);
                let mut conversation_id = None;
                if !interrupted {
                    match (&slot.recovery, &slot.harness_session_id) {
                        // No hold: the stored ID is the one Launch resumes. For
                        // Codex, a proof (W2) or Resume here lifted the hold.
                        (None, Some(id)) => conversation_id = Some(id.clone()),
                        (Some(recovery), _) if driver == DriverKind::Codex => {
                            // Still held: only the ID this run reported and the Resume path verifies.
                            let reported = recovery.reported_codex.as_ref()
                                .filter(|evidence| evidence.run_id == run_id && evidence.generation == slot.generation)
                                .map(|evidence| evidence.thread_id.clone());
                            match reported {
                                Some(thread) => {
                                    codex_checks.push((label.clone(), slot.definition.working_dir.clone(), recovery.clone(), thread.clone()));
                                    conversation_id = Some(thread);
                                }
                                None => capture.blockers.push(format!(
                                    "{label} (Codex) has not identified its conversation in this run and PRIM-1 has not proved it; ask it to run & \"$env:PRIM1_CLI\" --prim1-identify | Out-String from an unsandboxed shell (a Normal-profile Codex shell cannot reach PRIM-1)")),
                            }
                        }
                        _ => capture.blockers.push(format!("{label}'s current conversation ID is not known, so it cannot be resumed exactly")),
                    }
                }
                if slot.room_composer.open_question().is_some()
                    || (slot.work_state_observed && slot.work_state == WorkState::Blocked && slot.room_composer.question_may_be_open()) {
                    capture.blockers.push(format!("{label} has a question or approval open on its screen"));
                }
                let gate = &running.input_gate;
                match gate.room_input_activity.try_lock() {
                    Some(activity) if activity.draft_open() => capture.blockers.push(format!("{label} has unsent text typed into its terminal")),
                    Some(_) => {}
                    None => capture.blockers.push(format!("text is being typed into {label}")),
                }
                if gate.queued_operator_input_blocks_wake() {
                    capture.blockers.push(format!("operator input for {label} is still being written"));
                }
                if gate.room_wake_pending.load(Ordering::SeqCst) {
                    capture.blockers.push(format!("a room message typed into {label} has no recorded outcome yet"));
                }
                let task_ref = requester.as_ref().filter(|(caller, _)| caller.session_id == slot.session_id)
                    .map(|(_, task)| task.clone());
                capture.members.push(UpgradeMember {
                    session_id: slot.session_id,
                    room_id,
                    driver,
                    run_id,
                    generation: slot.generation,
                    resume_mode: if interrupted { UpgradeResumeMode::InterruptedWithoutResume } else { UpgradeResumeMode::Exact },
                    conversation_id: if interrupted { None } else { conversation_id },
                    task_ref,
                    continuation_id: Uuid::nil(),
                });
            }
            for room in rooms.order.iter().filter_map(|room_id| rooms.get(*room_id)) {
                let snapshot = room.snapshot();
                if snapshot.recovery_error.is_some() || snapshot.history_replay_pending {
                    capture.blockers.push(format!("room \"{}\" needs its history recovered", snapshot.label));
                }
                if room.unresolved_deliveries() > 0 {
                    capture.blockers.push(format!("room \"{}\" has a message whose delivery outcome is not recorded yet", snapshot.label));
                }
            }
        }
        match flow.state.lock().room_drafts_open {
            Some(false) => {}
            Some(true) => capture.blockers.push("Victor has an unsent message in a room composer".into()),
            None => capture.blockers.push("the window has not reported whether a room composer holds an unsent message".into()),
        }
        // As on the Resume path: the rollout check runs outside the registry lock.
        for (label, working_dir, recovery, thread) in codex_checks {
            if Self::verified_codex_resume_id(DriverKind::Codex, &working_dir, &recovery).as_deref() != Some(thread.as_str()) {
                capture.blockers.push(format!("{label}'s Codex conversation was not found for its folder"));
            }
        }
        if let Some((caller, _)) = &requester {
            let exact = capture.members.iter().any(|member| member.session_id == caller.session_id
                && member.resume_mode == UpgradeResumeMode::Exact);
            ensure_upgrade(exact, "the requesting member must be in a room and resume its own conversation (Claude, Codex or Grok)")?;
        }
        capture.blockers.sort();
        capture.blockers.dedup();
        Ok(capture)
    }

    fn spawn_close_admission(&self, attempt: Uuid) {
        if cfg!(test) {
            return; // Tests step close admission explicitly.
        }
        let weak = Arc::downgrade(&self.inner);
        let spawned = thread::Builder::new().name("prim1-upgrade-close".into()).spawn(move || loop {
            thread::sleep(CLOSE_POLL);
            let Some(inner) = weak.upgrade() else { return };
            if !(SupervisorHandle { inner }).upgrade_close_step(attempt) {
                return;
            }
        });
        if let Err(error) = spawned
            && let Some(flow) = self.upgrade_flow()
        {
            let _ = self.cancel_upgrade_attempt(&flow, attempt, format!("its close timer could not start ({error})"), None);
        }
    }

    /// One pass of close admission; true while it must be polled again.
    pub(crate) fn upgrade_close_step(&self, attempt: Uuid) -> bool {
        let Some(flow) = self.upgrade_flow() else { return false };
        let record = match flow.store.read(attempt) {
            Ok(record) => record,
            Err(error) => {
                self.upgrade_log(LogLevel::Error, format!("Upgrade {} could not be read before closing: {error:#}", short(attempt)));
                return false;
            }
        };
        if record.state.phase != UpgradePhase::Prepared {
            return false;
        }
        let now = Utc::now();
        if now < record.request.notice_until {
            return true;
        }
        let cancel = |reason: String| {
            let _ = self.cancel_upgrade_attempt(&flow, attempt, reason, None);
            false
        };
        // The module's notice and the visible one: a full notice after the
        // banner and room notices were published.
        let visible_until = flow.state.lock().notice.filter(|(shown, _)| *shown == attempt).map(|(_, until)| until);
        match visible_until {
            None => return cancel("its notice was not shown by this PRIM-1".into()),
            Some(until) if now < until => return true,
            Some(_) => {}
        }
        if now + chrono::Duration::seconds(CLOSE_REMAINING_SECONDS) > record.request.expires_at {
            return cancel("less than five minutes would remain before it expires; ask again".into());
        }
        let deadline = record.request.expires_at
            - chrono::Duration::seconds(CLOSE_REMAINING_SECONDS + CLOSE_ADMISSION_MARGIN_SECONDS);
        match flow.store.helper_result(attempt) {
            Ok(None) => {}
            Ok(Some(result)) => return cancel(format!("its helper stopped before it was ready ({})",
                result.message.unwrap_or_else(|| format!("{:?}", result.outcome).to_lowercase()))),
            Err(error) => return cancel(format!("its helper result is unreadable ({error:#})")),
        }
        let ready = match flow.store.helper_ready(attempt) {
            Ok(Some(ready)) => ready,
            Ok(None) if now < deadline => {
                self.update_notice_message(&flow, &record, "Waiting for the upgrade helper to report ready.");
                return true;
            }
            Ok(None) => return cancel("its helper did not report ready in time".into()),
            Err(error) => return cancel(format!("its helper readiness is unreadable ({error:#})")),
        };
        match flow.host.process_alive(ready.helper_pid, &ready.helper_creation_filetime) {
            Ok(true) => {}
            Ok(false) => return cancel("its helper is no longer running".into()),
            Err(error) => return cancel(format!("its helper could not be checked ({error:#})")),
        }
        let source = match flow.identity() {
            Ok(source) => source,
            Err(error) => return cancel(format!("{error:#}")),
        };
        // Refuse new starts first, then recheck: nothing can join the set now.
        self.inner.upgrade_closing.store(true, Ordering::Release);
        let reopen = || self.reopen_after_upgrade();
        let capture = match self.capture_upgrade_members(&flow, None) {
            Ok(capture) => capture,
            Err(error) => {
                reopen();
                return cancel(format!("{error:#}"));
            }
        };
        if let Some(change) = running_set_change(&record.request.members, &capture.members, |id| self.upgrade_label(*id)) {
            reopen();
            return cancel(format!("the running members changed during the notice ({change})"));
        }
        if let Some(change) = resume_change(&record.request.members, &capture.members, |id| self.upgrade_label(*id)) {
            reopen();
            return cancel(change);
        }
        let requester_may_maintain = self.inner.slots.lock().get_by_id(record.request.requester_session_id)
            .is_some_and(|slot| slot.permissions.maintain);
        if !requester_may_maintain {
            reopen();
            return cancel("the requesting member no longer has the \"Can upgrade PRIM-1\" grant".into());
        }
        if !capture.blockers.is_empty() {
            reopen();
            let blockers = capture.blockers.join("; ");
            if now < deadline {
                self.update_notice_message(&flow, &record, &format!("Closing waits: {blockers}."));
                return true;
            }
            return cancel(format!("it could not close in time: {blockers}"));
        }
        let helper = HelperIdentity { pid: ready.helper_pid, creation_filetime: ready.helper_creation_filetime };
        let closing = match flow.store.begin_close(attempt, &source, &helper) {
            Ok(closing) => closing,
            Err(error) => {
                reopen();
                return cancel(format!("{error:#}"));
            }
        };
        flow.note(&closing);
        self.set_upgrade_status(&flow, Some(self.status_for(&closing, UpgradeStage::Closing, format!(
            "PRIM-1 is closing to start build {}; every member resumes afterwards.", short(&closing.request.candidate.exe_sha256)))));
        if let Err(error) = flow.host.strict_shutdown() {
            self.set_upgrade_status(&flow, Some(self.status_for(&closing, UpgradeStage::Held, format!(
                "PRIM-1 could not close cleanly ({error:#}), so the new build was not started; its helper gives up at {}. Members may already be stopped: restart PRIM-1 to continue on this build.",
                closing.request.expires_at.to_rfc3339()))));
            return false;
        }
        match flow.store.shutdown_complete(attempt, &source) {
            Ok(done) => {
                flow.note(&done);
                flow.host.exit_for_upgrade();
            }
            Err(error) => self.set_upgrade_status(&flow, Some(self.status_for(&closing, UpgradeStage::Held, format!(
                "PRIM-1 closed but could not record it ({error:#}), so the new build will not start. Restart PRIM-1 to continue on this build.")))),
        }
        false
    }

    /// Operator Cancel during the notice.
    pub fn cancel_upgrade(&self, attempt_id: Uuid) -> Result<()> {
        self.cancel_upgrade_checked(attempt_id, "Victor", None)
    }

    /// A16: the same Cancel by `who` (a member granted maintain, named in the
    /// notice), never the operator's name. `check` (the caller's run and
    /// grant) holds the slots lock from its pass through the store's commit,
    /// as the settings setters do (`authorize_selection`).
    pub(crate) fn cancel_upgrade_checked(&self, attempt_id: Uuid, who: &str,
        check: Option<&dyn Fn(&SessionRegistry) -> Result<()>>) -> Result<()> {
        let flow = self.upgrade_flow().ok_or_else(|| anyhow!("no upgrade is in progress"))?;
        let record = flow.store.read(attempt_id)?;
        if record.state.phase != UpgradePhase::Prepared {
            bail!("upgrade {} can no longer be cancelled: it is {}", short(attempt_id), phase_name(record.state.phase));
        }
        self.cancel_upgrade_attempt(&flow, attempt_id, format!("{who} cancelled it"), check)
    }

    fn cancel_upgrade_attempt(&self, flow: &UpgradeFlow, attempt: Uuid, reason: String,
        check: Option<&dyn Fn(&SessionRegistry) -> Result<()>>) -> Result<()> {
        let source = flow.identity()?;
        // The store never takes a supervisor lock, so slots -> store is safe.
        let authority = task_sideband::authorize_selection(&self.inner.slots, check).map_err(anyhow::Error::msg)?;
        let cancelled = flow.store.cancel(attempt, &source);
        drop(authority);
        let record = match cancelled {
            Ok(record) => record,
            Err(error) => {
                self.upgrade_log(LogLevel::Error, format!("Upgrade {} could not be cancelled ({reason}): {error:#}", short(attempt)));
                return Err(error);
            }
        };
        self.reopen_after_upgrade();
        flow.note(&record);
        let message = format!("Upgrade {} to build {} was cancelled: {reason}. PRIM-1 keeps running on this build.",
            short(attempt), short(&record.request.candidate.exe_sha256));
        self.post_upgrade_notice(&record, &format!("upgrade-cancelled-{attempt}"), &message);
        let status = self.status_for(&record, UpgradeStage::Cancelled, message.clone());
        self.emit(RuntimeEvent::UpgradeStatusChanged { status: Some(status), timestamp: now_rfc3339() });
        flow.state.lock().status = None;
        self.upgrade_log(LogLevel::Info, message);
        Ok(())
    }

    /// Operator-confirmed note on a held attempt. It proves nothing stopped.
    pub fn abandon_held_upgrade(&self, attempt_id: Uuid) -> Result<()> {
        self.abandon_held_upgrade_checked(attempt_id, "the operator", None)
    }

    /// A16: the same Abandon by `who`, named in the log; `check` as for
    /// Cancel, held through the store's commit.
    pub(crate) fn abandon_held_upgrade_checked(&self, attempt_id: Uuid, who: &str,
        check: Option<&dyn Fn(&SessionRegistry) -> Result<()>>) -> Result<()> {
        let flow = self.upgrade_flow().ok_or_else(|| anyhow!("no upgrade is held"))?;
        let record = flow.store.read(attempt_id)?;
        let outcome = flow.store.helper_result(attempt_id)?.map(|result| result.outcome);
        let identity = flow.state.lock().identity.clone();
        if held_reason(&record, outcome, identity.as_ref()).is_none() {
            bail!("upgrade {} is not held", short(attempt_id));
        }
        let authority = task_sideband::authorize_selection(&self.inner.slots, check).map_err(anyhow::Error::msg)?;
        let abandoned = flow.store.abandon_held(attempt_id, &record.state.request_sha256, true);
        drop(authority);
        let abandoned = abandoned?;
        flow.note(&abandoned);
        self.set_upgrade_status(&flow, None);
        self.upgrade_log(LogLevel::Warn, format!(
            "Upgrade {} was abandoned by {who}. This does not show that its helper or new build stopped; check that only one PRIM-1 runs for this runtime.",
            short(attempt_id)));
        Ok(())
    }

    /// Ordinary startup: a notice interrupted by a restart is cancelled (this
    /// desktop owns the runtime now); an unfinished attempt is shown as held.
    fn settle_attempt_at_startup(&self, flow: &UpgradeFlow) {
        let settled = (|| -> Result<()> {
            let Some(attempt) = flow.store.current_attempt_id()? else { return Ok(()) };
            let record = flow.store.read(attempt)?;
            flow.note(&record);
            if record.state.phase == UpgradePhase::Prepared {
                let cancelled = flow.store.cancel_prelaunch(attempt, &record.state.request_sha256)?;
                flow.note(&cancelled);
                self.upgrade_log(LogLevel::Warn, format!(
                    "Upgrade {} was still in its notice when PRIM-1 restarted; it was cancelled.", short(attempt)));
                return Ok(());
            }
            let result = flow.store.helper_result(attempt)?;
            match held_reason(&record, result.as_ref().map(|result| result.outcome), None) {
                Some(reason) => self.set_upgrade_status(flow, Some(self.status_for(&record, UpgradeStage::Held, format!(
                    "{reason} Abandon it once you have checked that no second PRIM-1 runs for this runtime.")))),
                None if matches!(record.state.phase, UpgradePhase::Closing | UpgradePhase::ShutdownComplete) => {
                    self.upgrade_log(LogLevel::Warn, format!("Upgrade {} did not start the new build: {}.", short(attempt),
                        result.and_then(|result| result.message).unwrap_or_else(|| "no detail recorded".into())));
                }
                None => {}
            }
            Ok(())
        })();
        if let Err(error) = settled {
            self.upgrade_log(LogLevel::Error, format!("The upgrade record could not be read at startup: {error:#}"));
        }
    }

    /// The new build: claim the recorded resume once. Consumed means it was
    /// already done; nothing is replayed.
    fn begin_candidate_resume(&self, flow: &UpgradeFlow, startup: UpgradeStartup) {
        let attempt = startup.attempt_id;
        if !same_path_text(&flow.store.request_path(attempt), &startup.request_file) {
            self.upgrade_log(LogLevel::Error, format!(
                "This build was started for upgrade {} with a request file that is not this runtime's record; PRIM-1 started normally and resumed nothing.",
                short(attempt)));
            return self.settle_attempt_at_startup(flow);
        }
        let claimed = flow.identity().and_then(|identity| flow.store.begin_resume(attempt, &identity));
        match claimed {
            Ok(record) if record.state.phase == UpgradePhase::Consumed => {
                flow.note(&record);
                self.upgrade_log(LogLevel::Info, format!("Upgrade {} was already completed; ordinary start, nothing replayed.", short(attempt)));
            }
            Ok(record) => {
                flow.note(&record);
                let message = format!("PRIM-1 restarted on build {}: resuming the members that were running.",
                    short(&record.request.candidate.exe_sha256));
                self.set_upgrade_status(flow, Some(self.status_for(&record, UpgradeStage::Resuming, message)));
                flow.state.lock().pending_resume = Some(record);
                self.spawn_resume_fallback();
            }
            Err(error) => {
                let message = format!("This build was started for upgrade {} but cannot continue it: {error:#}. PRIM-1 started normally; no member was resumed by the upgrade.", short(attempt));
                let record = flow.store.read(attempt).ok();
                let outcome = flow.store.helper_result(attempt).ok().flatten().map(|result| result.outcome);
                let identity = flow.state.lock().identity.clone();
                match record {
                    Some(record) => {
                        flow.note(&record);
                        let stage = if held_reason(&record, outcome, identity.as_ref()).is_some() { UpgradeStage::Held } else { UpgradeStage::Failed };
                        self.set_upgrade_status(flow, Some(self.status_for(&record, stage, message)));
                    }
                    None => self.upgrade_log(LogLevel::Error, message),
                }
            }
        }
    }

    fn spawn_resume_fallback(&self) {
        if cfg!(test) {
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        let _ = thread::Builder::new().name("prim1-upgrade-resume-fallback".into()).spawn(move || {
            thread::sleep(RESUME_TRIGGER_FALLBACK);
            if let Some(inner) = weak.upgrade() {
                SupervisorHandle { inner }.begin_upgrade_resume();
            }
        });
    }

    /// The window is shown: resume the recorded members once (the fallback
    /// timer calls this too). Returns the status naming the members, which
    /// the window's ordinary restart resume leaves alone.
    pub fn begin_upgrade_resume(&self) -> Option<UpgradeStatus> {
        let flow = self.upgrade_flow()?;
        let record = {
            let mut state = flow.state.lock();
            if state.resume_requested {
                return state.status.clone();
            }
            state.resume_requested = true;
            state.pending_resume.take()
        };
        if let Some(record) = record {
            let status = flow.state.lock().status.clone();
            if cfg!(test) {
                self.run_upgrade_resume(&flow, record);
                return flow.state.lock().status.clone();
            }
            let handle = self.clone();
            let worker_flow = flow.clone();
            if let Err(error) = thread::Builder::new().name("prim1-upgrade-resume".into())
                .spawn(move || handle.run_upgrade_resume(&worker_flow, record)) {
                self.upgrade_log(LogLevel::Error, format!("The upgrade resume could not start: {error}"));
            }
            return status;
        }
        flow.state.lock().status.clone()
    }

    fn run_upgrade_resume(&self, flow: &UpgradeFlow, record: UpgradeRecord) {
        let attempt = record.request.attempt_id;
        let mut queue: VecDeque<&UpgradeMember> = record.request.members.iter()
            .filter(|member| member.resume_mode == UpgradeResumeMode::Exact).collect();
        let mut starting: Vec<(SessionId, Instant)> = Vec::new();
        let mut resumed = Vec::new();
        let mut not_resumed: Vec<(&UpgradeMember, String)> = Vec::new();
        let mut undelivered = Vec::new();
        loop {
            starting.retain(|(session_id, started)| started.elapsed() < RESUME_SETTLE_CEILING && self.upgrade_member_starting(*session_id));
            while starting.len() < RESUME_CONCURRENCY {
                let Some(member) = queue.pop_front() else { break };
                match self.resume_upgrade_member(member) {
                    Ok(()) => {
                        starting.push((member.session_id, Instant::now()));
                        resumed.push(member);
                        if let Err(error) = self.enqueue_upgrade_continuation(&record, member) {
                            undelivered.push(format!("{} ({error:#})", self.upgrade_label(member.session_id)));
                        }
                    }
                    Err(error) => not_resumed.push((member, format!("{error:#}"))),
                }
            }
            if queue.is_empty() && starting.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(250));
        }
        for member in record.request.members.iter().filter(|member| member.resume_mode == UpgradeResumeMode::InterruptedWithoutResume) {
            let why = if member.room_id.is_none() { "it is not in a room" } else { "a terminal or Prime pane has no conversation to resume" };
            self.post_upgrade_member_notice(member, "stopped", &format!(
                "{} was stopped by the upgrade and was not restarted: {why}.", self.upgrade_label(member.session_id)));
        }
        for (member, reason) in &not_resumed {
            self.post_upgrade_member_notice(member, "not-resumed", &format!(
                "{} was not resumed after the upgrade: {reason}. Resume it from its tab.", self.upgrade_label(member.session_id)));
        }
        #[cfg(test)]
        if flow.stop_before_complete_resume.load(Ordering::Acquire) {
            return;
        }
        let identity = match flow.identity() {
            Ok(identity) => identity,
            Err(error) => return self.upgrade_log(LogLevel::Error, format!("{error:#}")),
        };
        let mut summary = format!("PRIM-1 restarted on build {}: {} member(s) resumed with one continuation each.",
            short(&record.request.candidate.exe_sha256), resumed.len());
        if !not_resumed.is_empty() {
            let labels: Vec<_> = not_resumed.iter().map(|(member, _)| self.upgrade_label(member.session_id)).collect();
            summary.push_str(&format!(" Not resumed: {}.", labels.join(", ")));
        }
        if !undelivered.is_empty() {
            summary.push_str(&format!(" Continuation not stored for: {}.", undelivered.join(", ")));
        }
        match flow.store.complete_resume(attempt, &identity) {
            Ok(done) => {
                flow.note(&done);
                self.set_upgrade_status(flow, Some(self.status_for(&done, UpgradeStage::Resumed, summary.clone())));
                self.upgrade_log(LogLevel::Info, summary);
                flow.host.resume_consumed(&done);
            }
            Err(error) => self.set_upgrade_status(flow, Some(self.status_for(&record, UpgradeStage::Failed, format!(
                "{summary} The upgrade could not record that it finished ({error:#}); restarting this build does not replay it.")))),
        }
    }

    fn upgrade_member_starting(&self, session_id: SessionId) -> bool {
        self.inner.slots.lock().get_by_id(session_id).is_some_and(|slot| slot.spawn_in_flight.is_some()
            || (slot.running.is_some() && matches!(slot.state, LifecycleState::Starting | LifecycleState::Restarting)))
    }

    /// The existing Resume path for one recorded member, only when the
    /// conversation it would resume is still exactly the recorded one.
    fn resume_upgrade_member(&self, member: &UpgradeMember) -> Result<()> {
        let expected = member.conversation_id.as_deref().ok_or_else(|| anyhow!("no conversation was recorded"))?;
        let (driver, working_dir, recovery, stored, running, run_resume) = {
            let slots = self.inner.slots.lock();
            let slot = slots.get_by_id(member.session_id).ok_or_else(|| anyhow!("it no longer exists"))?;
            (slot.definition.driver, slot.definition.working_dir.clone(), slot.recovery.clone(),
                slot.harness_session_id.clone(), slot.running.is_some(), slot.run_resume_id.clone())
        };
        ensure_upgrade(driver == member.driver, "its harness changed")?;
        if running {
            ensure_upgrade(run_resume.as_deref() == Some(expected), "it is already running another conversation")?;
            return Ok(());
        }
        // As resolve_harness_launch_session: no hold resumes the stored ID; a
        // Codex hold resumes only its verified ID.
        let current = match (driver, recovery) {
            (DriverKind::Claude | DriverKind::Grok | DriverKind::Codex, None) => stored,
            (DriverKind::Codex, Some(recovery)) => Self::verified_codex_resume_id(driver, &working_dir, &recovery),
            _ => None,
        };
        ensure_upgrade(current.as_deref() == Some(expected), "the conversation it would resume is no longer the recorded one")?;
        self.start_session_checked_for_reason(member.session_id, false, false, None, None,
            recovery_automation::StartReason::Upgrade)?;
        let resumed = self.inner.slots.lock().get_by_id(member.session_id)
            .is_some_and(|slot| slot.run_resume_id.as_deref() == Some(expected));
        ensure_upgrade(resumed, "it started without resuming its recorded conversation")?;
        Ok(())
    }

    /// One continuation through the A106 queue. Its request id is the
    /// attempt's stable continuation id, so a rerun replays the stored post.
    fn enqueue_upgrade_continuation(&self, record: &UpgradeRecord, member: &UpgradeMember) -> Result<()> {
        let room_id = member.room_id.ok_or_else(|| anyhow!("it is not in a room"))?;
        let resumed_run = self.inner.slots.lock().get_by_id(member.session_id).and_then(|slot| slot.run_id)
            .ok_or_else(|| anyhow!("the resumed run is no longer running"))?;
        let membership_revision = {
            let rooms = self.inner.rooms.lock();
            let room = rooms.get(room_id).ok_or_else(|| anyhow!("its room no longer exists"))?;
            ensure_upgrade(room.definition.member_ids.contains(&member.session_id), "it left its room")?;
            room.definition.membership_revision
        };
        let delivered = self.deliver_room_message_as(RoomDeliverySpec { guest_recipient_ids: Vec::new(),
            fallback_origin: None,
            room_id,
            recipient_ids: vec![member.session_id],
            membership_revision,
            // v3 (Claude PRIM-1, 15a49bdf): PRIM-1's own, ACK-tracked and
            // pointer-woken, so a resumed member that never takes it stays open.
            sender: RoomMessageSender::Prim1 {},
            sender_label: "PRIM-1".into(),
            content: continuation_text(record, member),
            request_id: Some(format!("upgrade-{}", member.continuation_id)),
            admission: RoomDeliveryAdmission::Member,
            roam_required: false,
            ack_requested: true,
        }, None)?;
        if !delivered.replayed { self.supersede_older_continuations(room_id, member.session_id, delivered.message_id); }
        if let Err(error) = self.record_upgrade_continuation(member, resumed_run) {
            self.recovery_warning(member.session_id, error);
        }
        Ok(())
    }

    fn post_upgrade_member_notice(&self, member: &UpgradeMember, kind: &str, text: &str) {
        let posted = member.room_id.filter(|room_id| self.inner.rooms.lock().get(*room_id).is_some())
            .map(|room_id| self.post_prim1_notice(room_id, text.to_owned(),
                Some(format!("upgrade-{kind}-{}", member.continuation_id))));
        match posted {
            Some(Ok(_)) => {}
            Some(Err(error)) => self.upgrade_log(LogLevel::Warn, format!("{text} (room notice not stored: {error})")),
            None => self.upgrade_log(LogLevel::Info, text.to_owned()),
        }
    }

    /// A stored notice in every room with a recorded member; it wakes no one.
    fn post_upgrade_notice(&self, record: &UpgradeRecord, request_id: &str, text: &str) {
        let mut rooms: Vec<RoomId> = record.request.members.iter().filter_map(|member| member.room_id).collect();
        rooms.sort();
        rooms.dedup();
        for room_id in rooms {
            if self.inner.rooms.lock().get(room_id).is_none() {
                continue;
            }
            if let Err(error) = self.post_prim1_notice(room_id, text.to_owned(), Some(request_id.to_owned())) {
                self.upgrade_log(LogLevel::Warn, format!("Upgrade notice not stored in room {room_id}: {error}"));
            }
        }
    }

    fn update_notice_message(&self, flow: &UpgradeFlow, record: &UpgradeRecord, message: &str) {
        let changed = flow.state.lock().status.as_ref()
            .is_some_and(|status| status.attempt_id == record.request.attempt_id && status.message != message);
        if changed {
            self.set_upgrade_status(flow, Some(self.status_for(record, UpgradeStage::Notice, message.to_owned())));
        }
    }

    fn status_for(&self, record: &UpgradeRecord, stage: UpgradeStage, message: String) -> UpgradeStatus {
        let attempt = record.request.attempt_id;
        // The banner counts down the visible notice.
        let notice_until = self.upgrade_flow().and_then(|flow| flow.state.lock().notice)
            .filter(|(shown, _)| *shown == attempt).map_or(record.request.notice_until, |(_, until)| until);
        UpgradeStatus {
            attempt_id: attempt,
            stage,
            candidate_sha256: record.request.candidate.exe_sha256.clone(),
            requester_label: self.upgrade_label(record.request.requester_session_id),
            notice_until: notice_until.to_rfc3339(),
            expires_at: record.request.expires_at.to_rfc3339(),
            message,
            can_cancel: stage == UpgradeStage::Notice,
            can_abandon: stage == UpgradeStage::Held,
            member_session_ids: record.request.members.iter().map(|member| member.session_id).collect(),
        }
    }

    fn set_upgrade_status(&self, flow: &UpgradeFlow, status: Option<UpgradeStatus>) {
        flow.state.lock().status = status.clone();
        self.emit(RuntimeEvent::UpgradeStatusChanged { status, timestamp: now_rfc3339() });
    }

    fn upgrade_label(&self, session_id: SessionId) -> String {
        self.inner.slots.lock().get_by_id(session_id).map(|slot| slot.definition.label.clone())
            .unwrap_or_else(|| short(session_id))
    }

    fn upgrade_log(&self, level: LogLevel, message: String) {
        self.emit(RuntimeEvent::SystemLog { level, message, timestamp: now_rfc3339() });
    }

}

#[cfg(test)]
impl SupervisorHandle {
    pub(crate) fn set_upgrade_backdate_for_tests(&self, shift: chrono::Duration) {
        *self.upgrade_flow().expect("installed upgrade").backdate.lock() = Some(shift);
    }

    /// Moves the end of the visible notice `shift` earlier, as if it had been
    /// shown that long ago.
    pub(crate) fn age_upgrade_notice_for_tests(&self, shift: chrono::Duration) {
        if let Some((_, until)) = self.upgrade_flow().expect("installed upgrade").state.lock().notice.as_mut() {
            *until -= shift;
        }
    }

    pub(crate) fn stop_upgrade_before_complete_for_tests(&self) {
        self.upgrade_flow().expect("installed upgrade").stop_before_complete_resume.store(true, Ordering::Release);
    }

    pub(crate) fn upgrade_record_for_tests(&self, attempt: Uuid) -> UpgradeRecord {
        self.upgrade_flow().expect("installed upgrade").store.read(attempt).expect("readable upgrade record")
    }

    pub(crate) fn forget_room_drafts_for_tests(&self) {
        self.upgrade_flow().expect("installed upgrade").state.lock().room_drafts_open = None;
    }

    pub(crate) fn current_upgrade_for_tests(&self) -> Option<Uuid> {
        self.upgrade_flow().expect("installed upgrade").store.current_attempt_id().expect("readable pointer")
    }
}

fn ensure_upgrade(condition: bool, message: &str) -> Result<()> {
    if condition { Ok(()) } else { Err(anyhow!(message.to_owned())) }
}

/// This process: PID, exact creation FILETIME, image path and its SHA-256.
#[cfg(windows)]
pub fn current_process_identity() -> Result<UpgradeSource> {
    let process = PinnedProcess::open(std::process::id())?;
    let exe_path = std::env::current_exe().context("the running executable cannot be located")?;
    let exe_sha256 = file_sha256(&exe_path)?;
    Ok(UpgradeSource {
        pid: process.pid(),
        creation_filetime: process.creation_time_filetime().to_string(),
        exe_path,
        exe_sha256,
    })
}

#[cfg(not(windows))]
pub fn current_process_identity() -> Result<UpgradeSource> {
    bail!("self-upgrade requires Windows process identity")
}

fn file_sha256(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read as _;
    let mut file = File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(windows)]
pub fn process_alive(pid: u32, creation_filetime: &str) -> Result<bool> {
    let expected: u64 = creation_filetime.parse().context("creation FILETIME is not a number")?;
    // Gone, inaccessible or reused: none of these is the recorded helper.
    let Ok(process) = PinnedProcess::open(pid) else { return Ok(false) };
    Ok(process.creation_time_filetime() == expected && process.is_alive()?)
}

#[cfg(not(windows))]
pub fn process_alive(_pid: u32, _creation_filetime: &str) -> Result<bool> {
    bail!("self-upgrade requires Windows process identity")
}

/// `powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass
/// -WindowStyle Hidden -File <helper> -RequestPath <request> -Register`.
pub fn helper_register_command(system_root: &Path, helper: &Path, request: &Path) -> (PathBuf, Vec<OsString>) {
    let program = system_root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let mut args: Vec<OsString> = ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"]
        .into_iter().map(OsString::from).collect();
    args.push(helper.as_os_str().to_owned());
    args.push("-RequestPath".into());
    args.push(request.as_os_str().to_owned());
    args.push("-Register".into());
    (program, args)
}

/// Runs the staged helper's `-Register` mode without a window and waits for
/// it: it registers and starts the outside Task Scheduler helper, then exits.
#[cfg(windows)]
pub fn register_helper_hidden(helper: &Path, request: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let system_root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let (program, args) = helper_register_command(&system_root, helper, request);
    let mut child = std::process::Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("the upgrade helper could not be launched")?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            let mut detail = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                use std::io::Read as _;
                let _ = (&mut stderr).take(4096).read_to_string(&mut detail);
            }
            bail!("the upgrade helper refused to register ({status}): {}", detail.trim());
        }
        if started.elapsed() >= HELPER_REGISTER_TIMEOUT {
            let _ = child.kill();
            bail!("the upgrade helper did not finish registering within {} s", HELPER_REGISTER_TIMEOUT.as_secs());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(not(windows))]
pub fn register_helper_hidden(_helper: &Path, _request: &Path) -> Result<()> {
    bail!("the upgrade helper requires Windows Task Scheduler")
}
