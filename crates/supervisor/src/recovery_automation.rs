//! A107 recovery. Bounded crash attempts and one unresolved continuation per
//! existing member; retirement, launch and delivery retain their existing owners.
use super::*;
use anyhow::ensure;

const MAX_CONVERSATION_BYTES: usize = 256;
const CRASH_WINDOW_SECONDS: i64 = 15 * 60;
const CRASH_BACKOFF_SECONDS: [u64; 3] = [5, 30, 120];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum StartReason<'a> { Manual, Boot, Upgrade, Crash(&'a CrashRespawn) }

/// Captured before retirement clears question/work state. This is an in-process
/// launch fence, not a persisted restart request or another lifecycle owner.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct CrashRespawn {
    pub(super) run_id: Uuid,
    generation: SessionGeneration,
    room_id: RoomId,
    conversation_id: String,
    definition: SessionDefinition,
    directory: QualifiedWorkingDirectory,
    choices: SessionLaunchChoices,
    permissions: MemberPermissions,
    pub(super) not_before: DateTime<Utc>,
    delay: Duration,
    crash_diagnosis: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct RecoveryAutomation {
    observation: Option<Observation>,
    pending: Option<Continuation>,
    #[serde(default)]
    upgrade: Option<UpgradeContinuation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    crash_attempts: Vec<DateTime<Utc>>,
    // The retired run is already unique. Clearing this cancels its waiting
    // worker even when Stop of an already-closed member does not advance gen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    crash_waiting: Option<Uuid>,
}

fn recent_crash_attempts(attempts: &[DateTime<Utc>], now: DateTime<Utc>) -> Vec<DateTime<Utc>> {
    // Future timestamps remain charged if the wall clock moves backwards.
    attempts.iter().copied().filter(|at| *at > now - chrono::Duration::seconds(CRASH_WINDOW_SECONDS)).collect()
}

fn capture_crash_respawn(rooms: &RoomState, slot: &SessionSlot, last_state: Option<WorkState>) -> Option<CrashRespawn> {
    if !supported(slot.definition.driver) || slot.stop_intent.is_some() || slot.recovery.is_some()
        || slot.termination_uncertain || !matches!(last_state, Some(WorkState::Idle | WorkState::Thinking | WorkState::ToolCall))
        || slot.work_detail.as_deref().is_some_and(is_open_question_detail)
        || slot.room_composer.open_question().is_some() || screen_question(slot).is_some() { return None; }
    Some(CrashRespawn {
        run_id: slot.run_id?, generation: slot.generation,
        room_id: *rooms.room_by_session.get(&slot.session_id)?,
        conversation_id: slot.harness_session_id.as_ref()
            .filter(|id| !id.is_empty() && id.len() <= MAX_CONVERSATION_BYTES)?.clone(),
        definition: slot.definition.clone(), directory: slot.qualified_working_directory.clone()?,
        choices: slot.launch_choices.clone(), permissions: slot.permissions, not_before: Utc::now(),
        delay: Duration::ZERO, crash_diagnosis: None,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct UpgradeContinuation {
    interrupted_run: Uuid,
    resumed_run: Uuid,
    room_id: RoomId,
    continuation_id: Uuid,
    conversation_id: String,
    resolved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Observation {
    run_id: Uuid,
    generation: SessionGeneration,
    sequence: u64,
    state: WorkState,
    room_id: RoomId,
    conversation_id: Option<String>,
    // Minted once, then synced before this run can become an interruption.
    continuation_id: Uuid,
    #[serde(default)]
    crashed: bool,
}

impl Observation {
    fn same_eligibility(&self, other: &Self) -> bool {
        self.run_id == other.run_id && self.generation == other.generation && self.room_id == other.room_id
            && self.conversation_id == other.conversation_id && working(self.state) == working(other.state)
            && self.crashed == other.crashed
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ContinuationPhase { Ready, Posting, Queued, Written }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Continuation {
    interrupted: Observation,
    phase: ContinuationPhase,
    #[serde(default)]
    admitted_generation: Option<SessionGeneration>,
    #[serde(default)]
    resumed_run: Option<Uuid>,
}

impl Continuation {
    fn request_id(&self) -> String { format!("continuation-{}", self.interrupted.continuation_id) }
    fn text(&self) -> String {
        format!("PRIM-1 restarted your session after {}. Continue the task you were working on; its state is in the task folder. This is an automatic recovery reminder, not a new instruction from Victor.",
            if self.interrupted.crashed { "a crash" } else { "a PRIM-1 restart" })
    }
}

fn supported(driver: DriverKind) -> bool {
    matches!(driver, DriverKind::Claude | DriverKind::Codex | DriverKind::Grok)
}
fn working(state: WorkState) -> bool { matches!(state, WorkState::Thinking | WorkState::ToolCall) }
pub(super) fn message_origin(event: &shared_types::RoomFeedEvent) -> Option<&shared_types::RoomContinuationOrigin> {
    match &event.item { RoomFeedItem::Message { continuation_origin, .. } => continuation_origin.as_ref(), _ => None }
}

pub(super) fn validate_catalog(catalog: &SessionCatalogV1) -> Result<()> {
    for (session, entry) in &catalog.recovery_automation {
        ensure!(catalog.sessions.iter().any(|row| row.session_id == *session),
            "continuation state refers to a deleted member {session}");
        ensure!(entry.crash_attempts.len() <= CRASH_BACKOFF_SECONDS.len()
            && entry.crash_waiting.is_none_or(|run| !run.is_nil()), "invalid crash recovery budget for {session}");
        if let Some(owner) = &entry.upgrade {
            ensure!(!owner.interrupted_run.is_nil() && !owner.resumed_run.is_nil()
                && !owner.room_id.is_nil() && !owner.continuation_id.is_nil()
                && !owner.conversation_id.is_empty() && owner.conversation_id.len() <= MAX_CONVERSATION_BYTES,
                "invalid upgrade continuation ownership for {session}");
        }
        for observation in entry.observation.iter().chain(entry.pending.iter().map(|p| &p.interrupted)) {
            ensure!(!observation.run_id.is_nil() && !observation.continuation_id.is_nil()
                && !observation.room_id.is_nil() && observation.conversation_id.as_ref()
                    .is_none_or(|id| !id.is_empty() && id.len() <= MAX_CONVERSATION_BYTES),
                "invalid continuation observation for {session}");
        }
    }
    Ok(())
}

/// W2 already holds slots and the catalog candidate when it proves an identity.
/// Bind only that observed run; no locks, I/O or delayed callback here.
pub(super) fn bind_verified_identity(catalog: &mut SessionCatalogV1, session_id: SessionId,
    run_id: Uuid, generation: SessionGeneration, conversation_id: &str) {
    if conversation_id.is_empty() || conversation_id.len() > MAX_CONVERSATION_BYTES { return; }
    if let Some(observation) = catalog.recovery_automation.get_mut(&session_id)
        .and_then(|entry| entry.observation.as_mut())
        .filter(|o| o.run_id == run_id && o.generation == generation)
        && observation.conversation_id.as_deref() != Some(conversation_id) {
        if observation.conversation_id.is_some() { observation.continuation_id = Uuid::new_v4(); }
        observation.conversation_id = Some(conversation_id.into());
    }
}

impl SupervisorHandle {
    /// Persisted tokens explain an interrupted backoff; they never replay it.
    /// Failure to write keeps the clear in memory and leaves manual Start usable.
    pub(super) fn clear_crash_backoffs_on_boot(&self) {
        let write_error = {
            let mut slots = self.inner.slots.lock(); let mut catalog = self.inner.catalog.lock();
            let mut cleared = Vec::new();
            for (id, entry) in &mut catalog.recovery_automation {
                if entry.crash_waiting.take().is_none() { continue; }
                cleared.push(*id);
                if let Some(slot) = slots.get_by_id_mut(*id) {
                    slot.crash_respawn_waiting = false;
                    slot.running_at_shutdown = false;
                    slot.boot_resume_id = None;
                    let detail = "Crash recovery was waiting when PRIM-1 closed; use Restart to resume this conversation.";
                    slot.last_error = Some(match slot.last_error.take() {
                        Some(original) => format!("{original}\n{detail}"), None => detail.into(),
                    });
                }
            }
            for row in &mut catalog.sessions {
                if cleared.contains(&row.session_id) { row.running_at_shutdown = false; }
            }
            (!cleared.is_empty()).then(|| self.persist_catalog_candidate(&catalog).err()).flatten()
        };
        if let Some(error) = write_error {
            self.emit(RuntimeEvent::SystemLog { level: LogLevel::Warn,
                message: format!("Stopped crash backoffs could not be persisted: {error:#}"), timestamp: now_rfc3339() });
        }
    }

    pub(super) fn crash_effects_admitted(&self) -> Result<()> {
        self.ensure_active()?;
        self.ensure_not_upgrade_closing()
    }

    /// Reopening upgrade admission must release only crash starts that it
    /// interrupted before a process was owned. Never retry or refund a budget.
    pub(super) fn reopen_after_upgrade(&self) {
        let mut events = Vec::new(); let mut errors = Vec::new();
        {
            let mut slots = self.inner.slots.lock();
            self.inner.upgrade_closing.store(false, Ordering::Release);
            for slot in slots.by_id.values_mut() {
                if self.crash_effects_admitted().is_err() { break; }
                let Some((run, generation)) = slot.crash_start_admission.take() else { continue; };
                if slot.generation != generation || slot.last_run_id != Some(run)
                    || slot.state != LifecycleState::Starting || slot.run_id.is_some()
                    || slot.running.is_some() || slot.spawn_in_flight.is_some()
                    || slot.lifecycle_operation.is_some() || slot.stop_intent.is_some()
                    || slot.termination_uncertain { continue; }
                if !self.inner.catalog.lock().recovery_automation.get(&slot.session_id)
                    .is_some_and(|entry| entry.crash_waiting == Some(run)) { continue; }
                if self.crash_effects_admitted().is_err() { break; }
                slot.state = LifecycleState::Failed;
                slot.crash_respawn_waiting = false;
                let detail = "Automatic crash resume was interrupted by upgrade closing; use Start or Restart to resume this conversation.";
                let detail = match slot.last_error.take() {
                    Some(original) => format!("{original}\n{detail}"), None => detail.into(),
                };
                slot.last_error = Some(detail.clone());
                if let Err(error) = self.clear_recovery_eligibility(slot.session_id) {
                    errors.push((slot.session_id, error));
                }
                if ensure_run_event_capacity(slot, 1).is_ok() {
                    events.push(RuntimeEvent::SessionState { identity: next_run_event_identity(slot, run),
                        session: slot.definition.alias.clone(), state: slot.state,
                        reason: detail, timestamp: now_rfc3339() });
                }
            }
        }
        for event in events {
            if self.crash_effects_admitted().is_err() { return; }
            self.emit_run_event(event);
        }
        for (session, error) in errors {
            if self.crash_effects_admitted().is_err() { return; }
            self.recovery_warning(session, error);
        }
    }

    fn capture_crash_diagnosis(&self, attempt: &mut CrashRespawn) {
        if attempt.crash_diagnosis.is_none() {
            attempt.crash_diagnosis = self.inner.slots.lock().get_by_id(attempt.definition.session_id)
                .filter(|slot| slot.generation == attempt.generation && slot.last_run_id == Some(attempt.run_id))
                .and_then(|slot| slot.last_error.clone());
        }
    }

    /// Caller holds rooms then slots, before retirement resets either state.
    pub(super) fn capture_crash_respawn(&self, rooms: &RoomState, slot: &SessionSlot) -> Option<CrashRespawn> {
        let state = if slot.work_state_observed && slot.work_state == WorkState::Exited {
            slot.work_state_before_exit
        } else { slot.work_state_observed.then_some(slot.work_state) };
        capture_crash_respawn(rooms, slot, state)
    }

    pub(super) fn claim_crash_respawn(&self, rooms: &RoomState, slot: &SessionSlot) -> Option<CrashRespawn> {
        let attempt = self.capture_crash_respawn(rooms, slot)?;
        // Capture under the retirement admission lock. A closed Stop between
        // proof and scheduling must be able to revoke this exact claim.
        self.inner.catalog.lock().recovery_automation.entry(slot.session_id).or_default().crash_waiting = Some(attempt.run_id);
        Some(attempt)
    }

    pub(super) fn discard_crash_claim(&self, attempt: &CrashRespawn) -> bool {
        if self.crash_effects_admitted().is_err() { return false; }
        let mut slots = self.inner.slots.lock();
        if let Some(entry) = self.inner.catalog.lock().recovery_automation.get_mut(&attempt.definition.session_id)
            .filter(|entry| entry.crash_waiting == Some(attempt.run_id)) {
            if self.crash_effects_admitted().is_err() { return false; }
            entry.crash_waiting = None;
            if let Some(slot) = slots.get_by_id_mut(attempt.definition.session_id) { slot.crash_respawn_waiting = false; }
            true
        }
        else { false }
    }

    /// The Stop caller holds slots and has checked its caller/run authority.
    /// Revoke only the retiring run's in-process claim; leave termination and
    /// all other recovery state to their existing owners. Subsequent catalog
    /// writes retain the clear; no crash timer is replayed across application boot.
    pub(super) fn cancel_retiring_crash_claim(&self, slot: &mut SessionSlot) {
        if slot.lifecycle_operation != Some(LifecycleOperation {
            generation: slot.generation, kind: StopIntentKind::Operator,
        }) || !slot.termination_uncertain || slot.running.is_none()
            || slot.run_id.is_some() || slot.spawn_in_flight.is_some() { return; }
        let Some(run_id) = slot.last_run_id else { return; };
        if let Some(entry) = self.inner.catalog.lock().recovery_automation.get_mut(&slot.session_id)
            .filter(|entry| entry.crash_waiting == Some(run_id)) {
            entry.crash_waiting = None;
            slot.crash_respawn_waiting = false;
        }
    }

    fn crash_binding_matches(&self, rooms: &RoomState, slot: &SessionSlot, attempt: &CrashRespawn) -> bool {
        rooms.room_by_session.get(&slot.session_id) == Some(&attempt.room_id)
            && slot.definition == attempt.definition && slot.launch_choices == attempt.choices
            && slot.qualified_working_directory.as_ref() == Some(&attempt.directory)
            && slot.permissions == attempt.permissions && slot.recovery.is_none()
            && slot.harness_session_id.as_deref() == Some(attempt.conversation_id.as_str())
            && slot.stop_intent.is_none() && slot.lifecycle_operation.is_none() && !slot.termination_uncertain
    }

    pub(super) fn check_crash_upgrade_owner(&self, session_id: SessionId, attempt: &CrashRespawn) -> Result<()> {
        ensure!(!self.upgrade_owns_interrupted_run(session_id, attempt.run_id)?, "this interruption belongs to the upgrade continuation");
        Ok(())
    }

    /// Rechecked at launch admission, spawn reservation and installation. An
    /// already-started process that loses this fence uses normal rejected-spawn
    /// cleanup, never an identity substitution or a second owner.
    pub(super) fn check_crash_binding(&self, rooms: &RoomState, slot: &SessionSlot, attempt: &CrashRespawn) -> Result<()> {
        self.ensure_active()?;
        self.ensure_not_upgrade_closing()?;
        ensure!(self.crash_binding_matches(rooms, slot, attempt)
            && self.inner.catalog.lock().recovery_automation.get(&slot.session_id)
                .is_some_and(|entry| entry.crash_waiting == Some(attempt.run_id)),
            "automatic crash resume cancelled: member, conversation, room, permissions or recovery eligibility changed");
        Ok(())
    }

    pub(super) fn check_crash_start(&self, rooms: &RoomState, slot: &SessionSlot,
        attempt: &CrashRespawn, plan: Option<&PreparedLaunch>) -> Result<()> {
        self.check_crash_binding(rooms, slot, attempt)?;
        ensure!(slot.generation == attempt.generation && slot.last_run_id == Some(attempt.run_id)
            && slot.run_id.is_none() && slot.running.is_none() && slot.spawn_in_flight.is_none()
            && plan.is_none_or(|plan| plan.resumes_conversation
                && plan.harness_session_id.as_deref() == Some(attempt.conversation_id.as_str())),
            "automatic crash resume no longer names the retired exact run");
        Ok(())
    }

    /// Caller retains slots after checking the binding and dropping rooms.
    pub(super) fn reserve_crash_attempt(&self, session_id: SessionId, now: DateTime<Utc>) -> Result<()> {
        let mut catalog = self.inner.catalog.lock();
        let mut candidate = catalog.clone();
        let entry = candidate.recovery_automation.get_mut(&session_id).unwrap();
        entry.crash_attempts = recent_crash_attempts(&entry.crash_attempts, now);
        ensure!(entry.crash_attempts.len() < CRASH_BACKOFF_SECONDS.len(), "automatic crash recovery reached 3 attempts in 15 minutes; use Restart when ready");
        entry.crash_attempts.push(now);
        // This write, not the timer, reserves an attempt. Failure permits no
        // automatic spawn; manual Start and Stop never depend on this budget.
        self.persist_catalog_candidate(&candidate)?;
        *catalog = candidate;
        Ok(())
    }

    pub(super) fn arm_crash_respawn(&self, mut attempt: CrashRespawn, now: DateTime<Utc>) -> Result<CrashRespawn> {
        self.crash_effects_admitted()?;
        let session_id = attempt.definition.session_id;
        self.check_crash_upgrade_owner(session_id, &attempt)?;
        let rooms = self.inner.rooms.lock(); let mut slots = self.inner.slots.lock();
        let slot = slots.get_by_id_mut(session_id).ok_or_else(|| anyhow!("crash member was removed"))?;
        ensure!(self.crash_binding_matches(&rooms, slot, &attempt)
            && slot.generation == attempt.generation && slot.last_run_id == Some(attempt.run_id)
            && slot.running.is_none() && slot.run_id.is_none() && slot.spawn_in_flight.is_none(),
            "crash retirement is no longer current or released");
        self.ensure_active()?; self.ensure_not_upgrade_closing()?;
        let mut catalog = self.inner.catalog.lock(); let mut candidate = catalog.clone();
        let entry = candidate.recovery_automation.entry(session_id).or_default();
        ensure!(entry.crash_waiting == Some(attempt.run_id), "automatic crash recovery was cancelled after retirement");
        entry.crash_attempts = recent_crash_attempts(&entry.crash_attempts, now);
        let delay = *CRASH_BACKOFF_SECONDS.get(entry.crash_attempts.len())
            .ok_or_else(|| anyhow!("automatic crash recovery reached 3 attempts in 15 minutes; use Restart when ready"))?;
        entry.crash_waiting = Some(attempt.run_id);
        attempt.not_before = now + chrono::Duration::seconds(delay as i64);
        attempt.delay = Duration::from_secs(delay);
        if attempt.crash_diagnosis.is_none() { attempt.crash_diagnosis = slot.last_error.clone(); }
        drop(rooms);
        self.persist_catalog_candidate(&candidate)?; *catalog = candidate;
        slot.crash_respawn_waiting = true;
        Ok(attempt)
    }

    /// Called only after the original retirement proved release. Late cleanup
    /// reproof never calls this and ProcessDisappeared is deliberately excluded.
    pub(super) fn schedule_crash_respawn(&self, attempt: Option<CrashRespawn>, classification: &SessionExitClassification) {
        if self.crash_effects_admitted().is_err() { return; }
        let Some(mut attempt) = attempt.filter(|_| !classification.requested
            && matches!(classification.reason, SessionExitReason::CrashExit | SessionExitReason::PtyError)) else { return; };
        self.capture_crash_diagnosis(&mut attempt);
        if attempt.crash_diagnosis.is_none() { attempt.crash_diagnosis = classification.last_error.clone(); }
        let attempt = match self.arm_crash_respawn(attempt.clone(), Utc::now()) {
            Ok(attempt) => attempt,
            Err(error) => {
                if self.discard_crash_claim(&attempt) {
                    self.crash_respawn_notice(&attempt, "held", &format!("Automatic crash recovery held: {error:#}"), true);
                }
                return;
            }
        };
        let delay = attempt.delay;
        self.crash_respawn_notice(&attempt, "waiting", &format!("Crash recovery will resume the same conversation in {} seconds. Stop cancels it.", delay.as_secs()), false);
        // Fixtures drive this exact callback with injected time; they never
        // shorten the production backoffs or launch a native harness.
        #[cfg(test)]
        { return; }
        #[cfg(not(test))]
        {
            let weak = Arc::downgrade(&self.inner);
            self.inner.background_runtime.spawn(async move {
                tokio::time::sleep(delay).await;
                if let Some(inner) = weak.upgrade() {
                    let _ = tokio::task::spawn_blocking(move || SupervisorHandle { inner }.run_elapsed_crash_respawn(attempt, Utc::now())).await;
                }
            });
        }
    }

    pub(super) fn hold_crash_respawn(&self, mut attempt: CrashRespawn, error: anyhow::Error) {
        if self.crash_effects_admitted().is_err() { return; }
        self.capture_crash_diagnosis(&mut attempt);
        if self.discard_crash_claim(&attempt) {
            self.crash_respawn_notice(&attempt, "held", &format!("Automatic crash recovery held: {error:#}. Use Restart when ready."), true);
        }
    }

    /// Called after Tokio's monotonic sleep. A wall-clock rollback cannot undo
    /// that elapsed backoff; wall time still conservatively charges the budget.
    pub(super) fn run_elapsed_crash_respawn(&self, attempt: CrashRespawn, wall_now: DateTime<Utc>) -> Result<SessionSnapshot> {
        let now = wall_now.max(attempt.not_before);
        self.run_crash_respawn(attempt, now)
    }

    pub(super) fn run_crash_respawn(&self, mut attempt: CrashRespawn, now: DateTime<Utc>) -> Result<SessionSnapshot> {
        self.crash_effects_admitted()?;
        self.capture_crash_diagnosis(&mut attempt);
        let session_id = attempt.definition.session_id;
        let result = (|| {
            ensure!(now >= attempt.not_before, "crash backoff has not elapsed");
            self.check_crash_upgrade_owner(session_id, &attempt)?;
            self.start_session_checked_for_reason(session_id, false, false, None, None, StartReason::Crash(&attempt))
        })();
        if self.crash_effects_admitted().is_err() { return result; }
        // Cancel only this worker's token. A Stop or a newer run may have
        // already removed/replaced it; neither gets a stale error painted on it.
        let (owned, write_error) = {
            let mut slots = self.inner.slots.lock();
            let mut catalog = self.inner.catalog.lock();
            if self.crash_effects_admitted().is_err() { return result; }
            if let Some(entry) = catalog.recovery_automation.get_mut(&session_id)
                .filter(|entry| entry.crash_waiting == Some(attempt.run_id)) {
                entry.crash_waiting = None;
                if let Some(slot) = slots.get_by_id_mut(session_id) {
                    slot.crash_respawn_waiting = false;
                    // A failed admitted start advanced generation. Keep the
                    // exact completed attempt's generation for notice painting.
                    attempt.generation = slot.generation;
                }
                (true, self.persist_catalog_candidate(&catalog).err())
            } else { (false, None) }
        };
        if let Some(error) = write_error { self.recovery_warning(session_id, error); }
        if owned {
            match &result {
                Ok(_) => self.crash_respawn_notice(&attempt, "resumed", "Resumed the same conversation after a crash.", false),
                Err(error) => self.crash_respawn_notice(&attempt, "failed", &format!("Automatic crash resume did not complete: {error:#}. Use Start or Restart when ready."), true),
            }
        }
        result
    }

    fn crash_respawn_notice(&self, attempt: &CrashRespawn, kind: &str, detail: &str, failed: bool) {
        if self.crash_effects_admitted().is_err() { return; }
        let session_id = attempt.definition.session_id;
        let detail = if failed { match attempt.crash_diagnosis.as_deref() {
            Some(original) if !detail.contains(original) => format!("{original}\n{detail}"),
            _ => detail.to_owned(),
        }} else { detail.to_owned() };
        let state_event = if failed || kind == "waiting" {
            let mut slots = self.inner.slots.lock();
            if let Some(slot) = slots.get_by_id_mut(session_id).filter(|slot| slot.running.is_none()
                && slot.generation == attempt.generation
                && (failed || (slot.last_run_id == Some(attempt.run_id) && slot.crash_respawn_waiting))) {
                if failed { slot.last_error = Some(detail.to_owned()); }
                slot.last_run_id.filter(|_| ensure_run_event_capacity(slot, 1).is_ok()).map(|run| {
                    let identity = next_run_event_identity(slot, run);
                    RuntimeEvent::SessionState { identity, session: slot.definition.alias.clone(),
                        state: slot.state, reason: detail.clone(), timestamp: now_rfc3339() }
                })
            } else { None }
        } else { None };
        if let Some(event) = state_event { self.emit_run_event(event); }
        let message = format!("[PRIM-1 recovery] {}: {detail}", attempt.definition.label);
        self.emit_supervisor_alert(SupervisorAlertEvent { alert_type: SupervisorAlertType::OperatorAttention,
            request_id: None, session: Some(attempt.definition.alias.clone()), action: Some(format!("crash_{kind}")),
            last_work_state: None, last_session_state: None, message: message.clone(),
            severity: if failed { AlertSeverity::Warn } else { AlertSeverity::Info } });
        // A notice only; it must not wake another member or be a task prompt.
        // v3: attributed to PRIM-1, never shown as the operator's own post.
        if let Err(error) = self.post_prim1_notice(attempt.room_id, message,
            Some(format!("crash-{kind}-{}", attempt.run_id))) { self.recovery_warning(session_id, error); }
    }

    /// The upgrade already resumed this exact member and committed its message.
    /// One bounded marker keeps that ownership after the journal window moves on.
    pub(super) fn record_upgrade_continuation(&self, member: &upgrade::UpgradeMember, resumed_run: Uuid) -> Result<()> {
        let Some(room_id) = member.room_id else { return Ok(()); };
        let Some(conversation_id) = member.conversation_id.as_ref().filter(|id| !id.is_empty() && id.len() <= MAX_CONVERSATION_BYTES)
            else { return Ok(()); };
        let rooms = self.inner.rooms.lock(); let slots = self.inner.slots.lock();
        if rooms.room_by_session.get(&member.session_id) != Some(&room_id) { return Ok(()); }
        let Some(slot) = slots.get_by_id(member.session_id).filter(|slot| slot.run_id == Some(resumed_run)
            && slot.running.is_some() && slot.stop_intent.is_none() && slot.lifecycle_operation.is_none()
            && slot.harness_session_id == member.conversation_id && slot.run_resume_id == member.conversation_id)
            else { return Ok(()); };
        let _ = slot;
        let Some((_, resolved)) = rooms.get(room_id).and_then(|room|
            room.continuation_delivery(&format!("upgrade-{}", member.continuation_id), member.session_id)) else { return Ok(()); };
        let mut catalog = self.inner.catalog.lock();
        let entry = catalog.recovery_automation.entry(member.session_id).or_default();
        entry.upgrade = Some(UpgradeContinuation { interrupted_run: member.run_id, resumed_run,
            room_id, continuation_id: member.continuation_id, conversation_id: conversation_id.clone(), resolved });
        drop(rooms);
        self.persist_catalog_candidate(&catalog)
    }

    /// A terminal receipt ends ownership of the resumed run, never the original
    /// interruption. Unavailable/evicted unresolved evidence is not success.
    pub(super) fn refresh_upgrade_continuation(&self, session_id: SessionId) {
        let result = (|| -> Result<()> {
            let rooms = self.inner.rooms.lock(); let mut catalog = self.inner.catalog.lock();
            let Some(owner) = catalog.recovery_automation.get_mut(&session_id).and_then(|entry| entry.upgrade.as_mut())
                .filter(|owner| !owner.resolved) else { return Ok(()); };
            if rooms.get(owner.room_id).and_then(|room| room.continuation_delivery(
                &format!("upgrade-{}", owner.continuation_id), session_id)).is_some_and(|(_, terminal)| terminal) {
                owner.resolved = true;
                drop(rooms);
                self.persist_catalog_candidate(&catalog)?;
            }
            Ok(())
        })();
        if let Err(error) = result { self.recovery_warning(session_id, error); }
    }

    pub(super) fn resolve_upgrade_continuation_receipt(&self, event: &shared_types::RoomFeedEvent) {
        if let RoomFeedItem::Delivery { recipient_id, status, .. } = event.item
            && !matches!(status, RoomDeliveryStatus::Stored | RoomDeliveryStatus::Pending) {
            self.refresh_upgrade_continuation(recipient_id);
        }
    }

    pub(super) fn recorded_upgrade_owns_run(&self, session_id: SessionId, run_id: Uuid) -> bool {
        self.refresh_upgrade_continuation(session_id);
        self.inner.catalog.lock().recovery_automation.get(&session_id).is_some_and(|entry| {
            entry.upgrade.as_ref().is_some_and(|owner| owner.interrupted_run == run_id
                || (!owner.resolved && owner.resumed_run == run_id
                    && entry.observation.iter().chain(entry.pending.iter().map(|pending| &pending.interrupted))
                        .filter(|observation| observation.run_id == run_id)
                        .all(|observation| observation.conversation_id.as_deref() == Some(owner.conversation_id.as_str()))))
        })
    }

    /// Boot may start Idle/Blocked/Unknown too; only prompt promotion tests work state.
    pub(super) fn check_boot_recovery_start(&self, rooms: &RoomState, slots: &SessionRegistry,
        session_id: SessionId, plan: &PreparedLaunch) -> Result<()> {
        let slot = slots.get_by_id(session_id).ok_or_else(|| anyhow!("boot member disappeared"))?;
        let marked = rooms.room_by_session.get(&session_id).and_then(|id| rooms.get(*id))
            .is_some_and(|room| room.definition.resume_on_restart);
        ensure!(marked && slot.running_at_shutdown && slot.boot_resume_id.is_some()
            && supported(plan.driver) && plan.resumes_conversation && plan.harness_session_id == slot.boot_resume_id,
            "this member is no longer eligible for exact resume from this PRIM-1 restart; use its normal Start action");
        Ok(())
    }

    pub(super) fn invalidate_manual_recovery_start(&self, session_id: SessionId,
        check: Option<&member_lifecycle::ManageCheck<'_>>) -> Result<()> {
        let rooms = check.map(|_| self.inner.rooms.lock());
        let mut slots = self.inner.slots.lock();
        if let (Some(check), Some(rooms)) = (check, rooms.as_deref()) { check.check_target(rooms, &slots, session_id)?; }
        let slot = slots.get_by_id_mut(session_id).ok_or_else(|| anyhow!("unknown member"))?;
        if slot.running.is_some() || slot.spawn_in_flight.is_some() || slot.lifecycle_operation.is_some() { return Ok(()); }
        slot.boot_resume_id = None;
        slot.crash_respawn_waiting = false;
        let error = self.clear_recovery_eligibility(session_id).err();
        drop(slots); drop(rooms);
        if let Some(error) = error { self.recovery_warning(session_id, error); }
        Ok(())
    }

    pub(super) fn recovery_warning(&self, session_id: SessionId, error: anyhow::Error) {
        self.emit(RuntimeEvent::SystemLog { level: LogLevel::Warn,
            message: format!("{session_id}: automatic continuation: {error:#}"), timestamp: now_rfc3339() });
    }

    /// v3 (Claude PRIM-1, ac9f860 walk): PRIM-1 just sent `member` the newer
    /// continuation `newer`; the older ones it still owes are disposed by
    /// PRIM-1 as superseded, so only the newest stays open.
    pub(super) fn supersede_older_continuations(&self, room_id: RoomId, member: SessionId, newer: Uuid) {
        let failed = {
            let _publish = self.inner.room_event_publish.lock();
            let result = self.recover_room_writer(room_id).and_then(|()| {
                let mut rooms = self.inner.rooms.lock();
                rooms.get_mut(room_id).ok_or_else(|| anyhow!("its room no longer exists"))?
                    .supersede_continuations(member, newer)
            });
            match result {
                Ok(events) => { for feed_event in events { self.emit(RuntimeEvent::RoomFeedEvent { feed_event }); } None }
                Err(error) => Some(error),
            }
        };
        if let Some(error) = failed {
            self.recovery_warning(member, error.context("an older continuation could not be marked superseded"));
        }
    }

    /// Called only for a work-state transition, never for an output chunk.
    pub(super) fn observe_recovery_work_state(&self, session_id: SessionId,
        generation: SessionGeneration, run_id: Uuid, sequence: u64, state: WorkState) {
        // Exited is the interruption, not the work that was interrupted. A
        // proven clean/intentional retirement invalidates eligibility separately.
        if state == WorkState::Exited { return; }
        let result = (|| -> Result<()> {
            let rooms = self.inner.rooms.lock();
            let slots = self.inner.slots.lock();
            let Some(slot) = slots.get_by_id(session_id).filter(|slot|
                slot.generation == generation && slot.run_id == Some(run_id)
                && slot.last_work_state_sequence == Some(sequence)
                && slot.work_state_observed && slot.work_state == state
                && supported(slot.definition.driver)) else { return Ok(()) };
            let Some(room_id) = rooms.room_by_session.get(&session_id).copied() else { return Ok(()) };
            let conversation_id = slot.harness_session_id.as_deref().filter(|id|
                !id.is_empty() && id.len() <= MAX_CONVERSATION_BYTES);
            let mut catalog = self.inner.catalog.lock();
            let mut candidate = catalog.clone();
            let entry = candidate.recovery_automation.entry(session_id).or_default();
            let continuation_id = entry.observation.as_ref().filter(|old|
                old.run_id == run_id && old.conversation_id.as_deref() == conversation_id)
                .map(|old| old.continuation_id).unwrap_or_else(Uuid::new_v4);
            let previous = entry.observation.clone();
            let resolved = entry.pending.as_ref().is_some_and(|pending| rooms.get(pending.interrupted.room_id)
                .and_then(|room| room.continuation_delivery(&pending.request_id(), session_id))
                .is_some_and(|(_, terminal)| terminal));
            if resolved {
                entry.pending = None;
            }
            entry.observation = Some(Observation { run_id, generation, sequence, state, room_id,
                conversation_id: conversation_id.map(str::to_owned), continuation_id, crashed: false });
            let changed = resolved || previous.as_ref().is_none_or(|old|
                !old.same_eligibility(entry.observation.as_ref().unwrap()));
            // The slot retains exact state and sequence. Thinking/ToolCall and
            // other changes inside the same eligibility class need no fsync.
            drop(rooms);
            let persisted = if changed { self.persist_catalog_candidate(&candidate) } else { Ok(()) };
            *catalog = candidate;
            persisted?;
            Ok(())
        })();
        if let Err(error) = result { self.recovery_warning(session_id, error); }
    }

    /// Called with slots held at boot admission. Transfer the existing queued
    /// intent to this exact attempt before consuming the boot-only eligibility.
    pub(super) fn admit_boot_recovery_start(&self, session_id: SessionId,
        generation: SessionGeneration, plan: &PreparedLaunch) {
        let mut catalog = self.inner.catalog.lock();
        if let Some(pending) = catalog.recovery_automation.get_mut(&session_id).and_then(|entry| entry.pending.as_mut())
            .filter(|pending| matches!(pending.phase, ContinuationPhase::Posting | ContinuationPhase::Queued)
                && pending.interrupted.conversation_id.is_some() && plan.resumes_conversation
                && plan.harness_session_id == pending.interrupted.conversation_id) {
            // Waiting is not permission to write. Preparation persists this
            // binding; successful finish persists the new run before delivery.
            pending.admitted_generation = Some(generation);
            pending.resumed_run = None;
        }
    }

    /// Promote before spawning. A later startup's Idle paint must not erase the
    /// original interrupted Working run or replace its durable request UUID.
    pub(super) fn prepare_recovery_start(&self, session_id: SessionId,
        generation: SessionGeneration, plan: &PreparedLaunch, reason: StartReason<'_>) -> Result<Option<Uuid>> {
        if !matches!(reason, StartReason::Boot | StartReason::Crash(_)) { return Ok(None); }
        if matches!(reason, StartReason::Crash(_)) { self.crash_effects_admitted()?; }
        let prior = self.inner.catalog.lock().recovery_automation.get(&session_id).cloned();
        let Some(prior) = prior else { return Ok(None); };
        // Check the pending original interruption as well as the latest observed
        // run. Unreadable upgrade metadata suppresses prompting, not the launch.
        let mut owned = HashSet::new();
        for run in prior.observation.iter().map(|o| o.run_id)
            .chain(prior.pending.iter().map(|p| p.interrupted.run_id)) {
            if self.upgrade_owns_interrupted_run(session_id, run)? { owned.insert(run); }
        }
        let rooms = self.inner.rooms.lock();
        let slots = self.inner.slots.lock();
        let slot = slots.get_by_id(session_id).ok_or_else(|| anyhow!("member disappeared before continuation preparation"))?;
        if let StartReason::Crash(attempt) = reason { self.check_crash_binding(&rooms, slot, attempt)?; }
        ensure!(slot.generation == generation, "member changed before continuation preparation");
        let mut catalog = self.inner.catalog.lock();
        if matches!(reason, StartReason::Crash(_)) { self.crash_effects_admitted()?; }
        let mut candidate = catalog.clone();
        let Some(entry) = candidate.recovery_automation.get_mut(&session_id) else { return Ok(None) };
        let room_id = rooms.room_by_session.get(&session_id).copied();
        let exact = |observation: &Observation| plan.resumes_conversation && supported(plan.driver)
            && observation.conversation_id.is_some() && plan.harness_session_id == observation.conversation_id
            && room_id == Some(observation.room_id);
        if entry.pending.as_ref().is_some_and(|pending| !exact(&pending.interrupted)
            || pending.phase == ContinuationPhase::Ready || owned.contains(&pending.interrupted.run_id)) {
            // A Ready record belongs only to the start that created it. A
            // different start never inherits an unconsumed preparation.
            entry.pending = None;
        }
        if let Some(pending) = &entry.pending
            && (pending.phase == ContinuationPhase::Written || rooms.get(pending.interrupted.room_id)
                .and_then(|room| room.continuation_delivery(&pending.request_id(), session_id))
                .is_some_and(|(_, terminal)| terminal)) {
            entry.pending = None;
        }
        if let Some(observation) = entry.observation.as_ref()
            && exact(observation) && working(observation.state)
            && !owned.contains(&observation.run_id)
            && entry.pending.as_ref().is_none_or(|pending| pending.interrupted.run_id != observation.run_id) {
            // A later exact Working run is a new interruption, not a retry of
            // the unknown old delivery. Its own UUID replaces only eligibility;
            // the old journal/receipt remains and its typed write is invalidated.
            entry.pending = Some(Continuation { interrupted: observation.clone(), phase: ContinuationPhase::Ready,
                admitted_generation: None, resumed_run: None });
        }
        entry.observation = None;
        let permit = entry.pending.as_mut().map(|pending| {
            pending.admitted_generation = Some(generation); pending.resumed_run = None;
            pending.interrupted.continuation_id
        });
        drop(rooms);
        if matches!(reason, StartReason::Crash(_)) { self.crash_effects_admitted()?; }
        let persisted = self.persist_catalog_candidate(&candidate);
        if persisted.is_err() {
            // No prompt effect is permitted without its durable preparation.
            if let Some(entry) = candidate.recovery_automation.get_mut(&session_id) {
                if entry.pending.as_ref().is_some_and(|pending| pending.phase == ContinuationPhase::Ready) {
                    entry.pending = None;
                } else if let Some(pending) = entry.pending.as_mut() {
                    pending.admitted_generation = None; pending.resumed_run = None;
                }
            }
        }
        *catalog = candidate;
        persisted?;
        Ok(permit)
    }

    /// Stop callers already hold slots (and rooms for a managing pane). The
    /// clear precedes Stop admission, including its closed early return. Disk
    /// failure never prevents Stop; subsequent catalog writes retain the clear.
    pub(super) fn clear_recovery_eligibility(&self, session_id: SessionId) -> Result<()> {
        let mut catalog = self.inner.catalog.lock();
        if !catalog.recovery_automation.contains_key(&session_id) { return Ok(()) }
        let mut candidate = catalog.clone();
        // A human action clears eligibility, never the persisted crash budget.
        let attempts = candidate.recovery_automation.get(&session_id).unwrap().crash_attempts.clone();
        if attempts.is_empty() { candidate.recovery_automation.remove(&session_id); }
        else { candidate.recovery_automation.insert(session_id, RecoveryAutomation { crash_attempts: attempts, ..Default::default() }); }
        let persisted = self.persist_catalog_candidate(&candidate);
        *catalog = candidate;
        persisted
    }

    /// A failed or no-longer-eligible start cannot leave Ready for a later Start.
    pub(super) fn clear_unconsumed_recovery_start(&self, session_id: SessionId,
        generation: SessionGeneration, reason: StartReason<'_>) -> Result<()> {
        let slots = self.inner.slots.lock();
        if slots.get_by_id(session_id).is_none_or(|slot| slot.generation != generation) { return Ok(()); }
        let mut catalog = self.inner.catalog.lock();
        if matches!(reason, StartReason::Crash(_)) && self.crash_effects_admitted().is_err() { return Ok(()); }
        let Some(entry) = catalog.recovery_automation.get_mut(&session_id) else { return Ok(()); };
        if entry.pending.as_ref().is_some_and(|pending| pending.resumed_run.is_none()
            && pending.admitted_generation == Some(generation)) {
            if entry.pending.as_ref().unwrap().phase == ContinuationPhase::Ready { entry.pending = None; }
            else { entry.pending.as_mut().unwrap().admitted_generation = None; }
            self.persist_catalog_candidate(&catalog)?;
        }
        Ok(())
    }

    pub(super) fn recovery_run_retired(&self, session_id: SessionId, generation: SessionGeneration,
        run_id: Uuid, classification: &SessionExitClassification) -> Result<()> {
        let mut slots = self.inner.slots.lock();
        let Some(slot) = slots.get_by_id_mut(session_id).filter(|s| s.generation == generation
            && s.last_run_id == Some(run_id) && s.run_id.is_none() && !s.termination_uncertain)
            else { return Ok(()) };
        if classification.requested || !matches!(classification.reason, SessionExitReason::CrashExit | SessionExitReason::PtyError) {
            slot.crash_respawn_waiting = false;
            return self.clear_recovery_eligibility(session_id);
        }
        let mut catalog = self.inner.catalog.lock();
        let mut candidate = catalog.clone();
        if let Some(observation) = candidate.recovery_automation.get_mut(&session_id)
            .and_then(|entry| entry.observation.as_mut()).filter(|o| o.run_id == run_id) {
            observation.crashed = true;
        }
        if candidate != *catalog { self.persist_catalog_candidate(&candidate)?; *catalog = candidate; }
        Ok(())
    }

    /// Successful *exact resume* only. Posting is synced before the room call;
    /// after an uncertain call a missing retained post is not permission to retry.
    pub(super) fn finish_recovery_start(&self, session_id: SessionId, generation: SessionGeneration,
        run_id: Uuid, plan: &PreparedLaunch, permit: Option<Uuid>) {
        let Some(permit) = permit.filter(|_| plan.resumes_conversation) else { return; };
        let result = (|| -> Result<()> {
            let prepared = {
                let rooms = self.inner.rooms.lock();
                let slots = self.inner.slots.lock();
                let Some(slot) = slots.get_by_id(session_id).filter(|s| s.generation == generation
                    && s.run_id == Some(run_id) && s.running.is_some() && s.stop_intent.is_none()
                    && s.lifecycle_operation.is_none() && !s.termination_uncertain
                    && s.run_resume_id == plan.harness_session_id) else { return Ok(()) };
                let _ = slot;
                let mut catalog = self.inner.catalog.lock();
                let mut candidate = catalog.clone();
                let Some(pending) = candidate.recovery_automation.get_mut(&session_id)
                    .and_then(|entry| entry.pending.as_mut()).filter(|pending|
                        pending.interrupted.continuation_id == permit && pending.admitted_generation == Some(generation))
                    else { return Ok(()) };
                ensure!(pending.interrupted.conversation_id.is_some() && plan.harness_session_id == pending.interrupted.conversation_id,
                    "the resumed conversation differs from the interruption");
                let room = rooms.get(pending.interrupted.room_id).ok_or_else(|| anyhow!("interrupted room no longer exists"))?;
                ensure!(rooms.room_by_session.get(&session_id) == Some(&pending.interrupted.room_id), "member changed rooms");
                let post = pending.phase == ContinuationPhase::Ready;
                if !post {
                    let retained = room.replayed_continuation(&pending.request_id(), &pending.text(), &[session_id])?;
                    ensure!(retained.is_some(), "previous continuation outcome is uncertain; its retained room post is unavailable. It was not posted again");
                    if room.continuation_delivery(&pending.request_id(), session_id).is_some_and(|(_, terminal)| terminal) {
                        candidate.recovery_automation.get_mut(&session_id).unwrap().pending = None;
                        drop(rooms);
                        self.persist_catalog_candidate(&candidate)?; *catalog = candidate;
                        return Ok(());
                    }
                }
                if post { pending.phase = ContinuationPhase::Posting; }
                pending.resumed_run = Some(run_id);
                let prepared = (pending.clone(), room.definition.membership_revision, post);
                drop(rooms);
                self.persist_catalog_candidate(&candidate)?;
                *catalog = candidate;
                prepared
            };
            let (pending, membership_revision, post) = prepared;
            if !post { return Ok(()); } // existing A106 queue owns the retained intent
            let result = self.deliver_room_message_as(RoomDeliverySpec { guest_recipient_ids: Vec::new(),
                fallback_origin: None,
                room_id: pending.interrupted.room_id, recipient_ids: vec![session_id], membership_revision,
                // v3 (Claude PRIM-1, 15a49bdf): PRIM-1's own, ACK-tracked and
                // pointer-woken, so a member that never takes it stays open.
                sender: RoomMessageSender::Prim1 {}, sender_label: "PRIM-1".into(), content: pending.text(),
                request_id: Some(pending.request_id()), admission: RoomDeliveryAdmission::Continuation {
                    session_id, generation, run_id, continuation_id: pending.interrupted.continuation_id }, roam_required: false,
                ack_requested: true,
            }, None);
            if let Ok(delivered) = &result && !delivered.replayed {
                self.supersede_older_continuations(pending.interrupted.room_id, session_id, delivered.message_id);
            }
            let rooms = self.inner.rooms.lock();
            let room = rooms.get(pending.interrupted.room_id);
            let resolved = room.and_then(|room| room.continuation_delivery(&pending.request_id(), session_id))
                .is_some_and(|(_, terminal)| terminal);
            let definite_noncommit = result.as_ref().err().is_some_and(|error| !room_commit_is_uncertain(error)
                && room.is_some_and(|room| matches!(room.replayed_continuation(&pending.request_id(),
                    &pending.text(), &[session_id]), Ok(None))));
            let mut catalog = self.inner.catalog.lock();
            let mut candidate = catalog.clone();
            if let Some(entry) = candidate.recovery_automation.get_mut(&session_id)
                .filter(|entry| entry.pending.as_ref().is_some_and(|p| p.interrupted.continuation_id == permit)) {
                if resolved || definite_noncommit { entry.pending = None; }
                else if result.is_ok() { entry.pending.as_mut().unwrap().phase = ContinuationPhase::Queued; }
                drop(rooms);
                self.persist_catalog_candidate(&candidate)?;
                *catalog = candidate;
            }
            result?;
            Ok(())
        })();
        if let Err(error) = result { self.recovery_warning(session_id, error); }
    }

    pub(super) fn admit_recovery_post(&self, slots: &SessionRegistry, session_id: SessionId,
        generation: SessionGeneration, run_id: Uuid, continuation_id: Uuid) -> Result<()> {
        let slot = slots.get_by_id(session_id).ok_or_else(|| anyhow!("continuation member disappeared"))?;
        ensure!(slot.generation == generation && slot.run_id == Some(run_id) && slot.running.is_some()
            && slot.stop_intent.is_none() && slot.lifecycle_operation.is_none() && !slot.termination_uncertain,
            "continuation cancelled because the resumed run changed or stopped");
        let catalog = self.inner.catalog.lock();
        let pending = catalog.recovery_automation.get(&session_id).and_then(|entry| entry.pending.as_ref())
            .ok_or_else(|| anyhow!("continuation eligibility was cleared"))?;
        ensure!(pending.interrupted.continuation_id == continuation_id && pending.phase == ContinuationPhase::Posting
            && pending.admitted_generation == Some(generation) && pending.resumed_run == Some(run_id)
            && pending.interrupted.conversation_id.is_some() && slot.harness_session_id == pending.interrupted.conversation_id,
            "continuation no longer names the same interrupted conversation");
        Ok(())
    }

    /// Reused by planning, queued replay, Pending admission and the PTY write.
    pub(super) fn continuation_still_valid(&self, rooms: &RoomState, slots: &SessionRegistry,
        room_id: RoomId, origin: &shared_types::RoomContinuationOrigin) -> Result<()> {
        self.ensure_active()?;
        let room = rooms.get(room_id).ok_or_else(|| anyhow!("continuation room was removed"))?;
        ensure!(rooms.room_by_session.get(&origin.session_id) == Some(&room_id), "continuation member left its room");
        let slot = slots.get_by_id(origin.session_id).ok_or_else(|| anyhow!("continuation member was removed"))?;
        let catalog = self.inner.catalog.lock();
        let pending = catalog.recovery_automation.get(&origin.session_id).and_then(|entry| entry.pending.as_ref())
            .filter(|pending| pending.interrupted.continuation_id == origin.continuation_id
                && pending.interrupted.room_id == room_id && matches!(pending.phase, ContinuationPhase::Posting | ContinuationPhase::Queued))
            .ok_or_else(|| anyhow!("continuation cancelled by a manual action or changed interruption"))?;
        ensure!(slot.stop_intent.is_none() && slot.lifecycle_operation.is_none() && !slot.termination_uncertain,
            "continuation cancelled because the member stopped or is changing run");
        ensure!(pending.interrupted.conversation_id.is_some()
            && slot.harness_session_id == pending.interrupted.conversation_id,
            "continuation no longer names the member's current conversation");
        if let Some(stored) = room.retained_continuation(&pending.request_id()) {
            ensure!(stored.cursor.epoch == room.feed_epoch
                && stored.cursor.sequence > room.join_floor_by_session.get(&origin.session_id).copied().unwrap_or(0),
                "continuation belongs to an earlier room membership");
        }
        let awaiting_boot = slot.boot_resume_id == pending.interrupted.conversation_id
            && slot.boot_resume_id.is_some() && room.definition.resume_on_restart;
        let awaiting_start = pending.admitted_generation == Some(slot.generation)
            && pending.resumed_run.is_none() && (slot.state == LifecycleState::Starting
                || (slot.running.is_some() && slot.run_resume_id == pending.interrupted.conversation_id));
        if awaiting_boot || awaiting_start {
            return Err(room_delivery::RoomWakeNotReady("continuation waits for its qualifying exact resume".into()).into());
        }
        ensure!(slot.running.is_some() && slot.run_id == pending.resumed_run && slot.run_id.is_some()
            && pending.admitted_generation == Some(slot.generation)
            && slot.run_resume_id == pending.interrupted.conversation_id && slot.run_resume_id.is_some(),
            "continuation no longer belongs to the resumed conversation/run");
        Ok(())
    }

    pub(super) fn check_recovery_message(&self, message: &shared_types::RoomFeedEvent) -> Result<()> {
        if let Some(origin) = message_origin(message) {
            let rooms = self.inner.rooms.lock(); let slots = self.inner.slots.lock();
            self.continuation_still_valid(&rooms, &slots, message.room_id, origin)?;
        }
        Ok(())
    }
}
