//! A111 room schedules. One weak timer and the existing A106 delivery queue.
use super::*;
use anyhow::ensure;
use shared_types::{RoomFallbackAction, RoomFallbackMemberStatus, RoomFallbackOrigin,
    RoomFallbackOverrideMode, RoomFallbackRecipientMode, RoomFallbackSettings,
    RoomFallbackSnapshot, SetRoomFallbacksRequest};

const TICK: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RoomFallbackRecord {
    pub revision: u64,
    pub settings: RoomFallbackSettings,
    pub members: BTreeMap<SessionId, MemberRecord>,
}

impl RoomFallbackRecord {
    pub(crate) fn is_unused(&self) -> bool { self == &Self::default() }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemberRecord {
    next_due_at: Option<DateTime<Utc>>,
    occurrence: Option<RoomFallbackOrigin>,
    last_message_id: Option<Uuid>,
    last_status: Option<String>,
    note: Option<String>,
    unresolved: bool,
}

#[derive(Default)]
pub(super) struct FallbackClock {
    deadlines: HashMap<(RoomId, SessionId), (u64, Instant)>,
    /// v3 W6 (ruling b133623a): missed scheduled times waiting for their one
    /// occurrence while the member starts: revision, count, the first one.
    /// In memory only: a restart recounts them from the saved due time.
    missed: HashMap<(RoomId, SessionId), (u64, u64, DateTime<Utc>)>,
}

fn hosted(driver: DriverKind) -> bool { matches!(driver, DriverKind::Claude | DriverKind::Codex | DriverKind::Grok) }
fn interval(minutes: u32) -> Result<Duration> {
    ensure!(matches!(minutes, 15 | 30 | 60), "fallback interval must be 15, 30 or 60 minutes");
    Ok(Duration::from_secs(u64::from(minutes) * 60))
}
fn future(now: DateTime<Utc>, duration: Duration) -> DateTime<Utc> {
    now + chrono::Duration::from_std(duration).expect("bounded fallback interval")
}

// Keep the saved phase and cap a backwards-clock wait to one interval. A due
// time already passed is handled by the tick as missed (v3 W6, b133623a).
fn restored_wait(due: Option<DateTime<Utc>>, now: DateTime<Utc>, duration: Duration) -> Duration {
    match due {
        Some(due) if due > now => (due - now).to_std().unwrap_or(duration).min(duration),
        Some(due) => next_boundary((now - due).to_std().unwrap_or_default(), duration),
        None => duration,
    }
}

/// Start and end of the status note of an occurrence that covers missed times.
const COALESCED_NOTE: &str = "One reminder for ";
const COALESCED_NOTE_END: &str = "the missed ones are not repeated.";

/// An outcome's note: its error, after the missed-times note of a coalesced
/// occurrence, which stays visible also when it did not fit in the body
/// (Reviewer 5e979b6b). Repeating it with the same error changes nothing.
fn set_outcome_note(row: &mut MemberRecord, error: Option<String>) {
    row.note = row.note.as_deref().filter(|note| note.starts_with(COALESCED_NOTE))
        .and_then(|note| note.find(COALESCED_NOTE_END).map(|end| note[..end + COALESCED_NOTE_END.len()].to_string()));
    if let Some(error) = error {
        append_occurrence_note(row, &error);
    }
}

fn scheduled_times(count: u64) -> String {
    if count == 1 { "1 scheduled time".into() } else { format!("{count} scheduled times") }
}

fn next_boundary(elapsed: Duration, duration: Duration) -> Duration {
    Duration::from_nanos((duration.as_nanos() - elapsed.as_nanos() % duration.as_nanos()) as u64)
}

fn physical_outcome_uncertain(row: &MemberRecord) -> bool {
    row.unresolved && matches!(row.last_status.as_deref(), Some("pending" | "failed" | "uncertain"))
}

fn hold_occurrence(row: &mut MemberRecord, reason: &str) {
    row.unresolved = false;
    row.last_status = Some("held".into());
    row.note = Some(reason.into());
}

fn history_ready(room: &RoomRuntime) -> bool {
    let history = room.snapshot();
    history.recovery_error.is_none() && !history.history_replay_pending
}

fn append_occurrence_note(row: &mut MemberRecord, explanation: &str) {
    let prior = row.note.as_deref().unwrap_or_default();
    let prefix_chars = room_log::MAX_RECEIPT_ERROR_CHARS.saturating_sub(explanation.chars().count() + 2);
    let prefix: String = prior.chars().take(prefix_chars).collect();
    row.note = Some(if prefix.is_empty() { explanation.into() } else { format!("{prefix}; {explanation}") });
}

fn reconcile_occurrence(row: &mut MemberRecord, room: &RoomRuntime, member: SessionId, history_ready: bool) {
    if !row.unresolved || !history_ready { return; }
    let Some(origin) = &row.occurrence else { return; };
    if let Some((result, terminal)) = room.fallback_delivery(&request_id(origin), member) {
        row.last_message_id = Some(result.message_id);
        if let Some(receipt) = result.receipts.first() {
            row.last_status = Some(format!("{:?}", receipt.status).to_ascii_lowercase());
            set_outcome_note(row, receipt.error.as_ref().map(|error| bounded_room_delivery_error(error)));
        }
        row.unresolved = !terminal;
    } else if !physical_outcome_uncertain(row) && !room.writer_needs_replay()
        && matches!(room.replayed_post(&request_id(origin), &RoomMessageSender::Operator {}, "", &[member]), Ok(None)) {
        // A saved intent can predate its post (or outlive retained history).
        // Never repost that intent. Known Pending/partial/unknown effects stay held.
        hold_occurrence(row, "Previous fallback is absent from healthy retained history; that occurrence was suppressed");
    } else {
        row.note.get_or_insert_with(|| "Previous fallback outcome is unresolved; no replacement was sent".into());
    }
}

/// v3 W6 (J10): when the reminders' mandate ends, if it has an end.
fn mandate_end(settings: &RoomFallbackSettings) -> Option<DateTime<Utc>> {
    settings.until.as_deref().and_then(|until| DateTime::parse_from_rfc3339(until).ok()).map(|at| at.with_timezone(&Utc))
}

fn mandate_ended(settings: &RoomFallbackSettings, now: DateTime<Utc>) -> bool {
    mandate_end(settings).is_some_and(|end| end <= now)
}

fn effective(settings: &RoomFallbackSettings, member: SessionId, now: DateTime<Utc>) -> Result<Option<(Duration, String)>> {
    if !settings.enabled || mandate_ended(settings, now) || (settings.recipient_mode == RoomFallbackRecipientMode::Selected
        && !settings.recipient_ids.contains(&member)) { return Ok(None); }
    let member_override = settings.overrides.iter().find(|item| item.session_id == member);
    if member_override.is_some_and(|item| item.mode == RoomFallbackOverrideMode::Off) { return Ok(None); }
    let (minutes, message) = match member_override.filter(|item| item.mode == RoomFallbackOverrideMode::Custom) {
        Some(item) => (item.interval_minutes.ok_or_else(|| anyhow!("custom fallback needs an interval"))?,
            item.message.as_deref().ok_or_else(|| anyhow!("custom fallback needs a message"))?),
        None => (settings.interval_minutes, settings.message.as_str()),
    };
    ensure!(!message.trim().is_empty(), "an enabled fallback needs a message");
    let mut body = message.to_owned();
    if let Some(brief) = member_override.and_then(|item| item.brief.as_deref()).filter(|brief| !brief.is_empty()) {
        body.push_str("\n\n"); body.push_str(brief);
    }
    validate_message_body(&body)?;
    Ok(Some((interval(minutes)?, body)))
}

pub(crate) fn validate_record(record: &RoomFallbackRecord) -> Result<()> {
    interval(record.settings.interval_minutes)?;
    ensure!(record.settings.until.as_deref().is_none_or(|until| DateTime::parse_from_rfc3339(until).is_ok()),
        "until must be an RFC 3339 time, for example 2026-10-25T18:00:00Z");
    ensure!(!record.settings.enabled || record.settings.recipient_mode != RoomFallbackRecipientMode::All
        || !record.settings.message.trim().is_empty(), "all hosted agents needs a room message, including for members added later");
    ensure!(record.settings.recipient_ids.len() <= rooms::ROOM_MEMBER_MAX_COUNT
        && record.settings.overrides.len() <= rooms::ROOM_MEMBER_MAX_COUNT
        && record.members.len() <= rooms::ROOM_MEMBER_MAX_COUNT, "too many fallback members");
    let mut ids = HashSet::new();
    ensure!(record.settings.recipient_ids.iter().all(|id| ids.insert(*id)), "duplicate fallback recipient");
    ids.clear();
    let mut text_bytes = record.settings.message.len();
    validate_message_body(&record.settings.message)?;
    for item in &record.settings.overrides {
        ensure!(ids.insert(item.session_id), "duplicate fallback override");
        if let Some(minutes) = item.interval_minutes { interval(minutes)?; }
        for text in item.message.iter().chain(item.brief.iter()) {
            validate_message_body(text)?;
            text_bytes = text_bytes.saturating_add(text.len());
        }
        if item.mode == RoomFallbackOverrideMode::Custom {
            ensure!(item.interval_minutes.is_some() && item.message.as_ref().is_some_and(|text| !text.trim().is_empty()),
                "custom fallback needs its interval and message");
        }
    }
    ensure!(text_bytes <= MESSAGE_BODY_MAX_BYTES, "room fallback settings exceed the message text budget");
    for (member, row) in &record.members {
        if let Some(origin) = &row.occurrence {
            ensure!(*member == origin.session_id && !origin.occurrence_id.is_nil() && !origin.run_id.is_nil()
                && DateTime::parse_from_rfc3339(&origin.scheduled_at).is_ok(), "invalid fallback occurrence");
        }
        ensure!(row.note.as_ref().is_none_or(|note| note.chars().count() <= room_log::MAX_RECEIPT_ERROR_CHARS),
            "fallback note exceeds its bound");
    }
    Ok(())
}

pub(super) fn message_origin(message: &shared_types::RoomFeedEvent) -> Option<&RoomFallbackOrigin> {
    match &message.item { RoomFeedItem::Message { fallback_origin, .. } => fallback_origin.as_ref(), _ => None }
}

pub(super) fn request_id(origin: &RoomFallbackOrigin) -> String { format!("fallback-{}", origin.occurrence_id) }

/// The one eligibility predicate at intent, Pending and write admission. A107
/// hibernated runs are stopped; its future wake path must ignore this origin.
impl SupervisorHandle {
    pub(super) fn fallback_still_valid(&self, rooms: &RoomState, slots: &SessionRegistry,
        room_id: RoomId, origin: &RoomFallbackOrigin) -> Result<()> {
        self.ensure_active()?;
        ensure!(!self.inner.upgrade_closing.load(Ordering::Acquire), "fallback suppressed: PRIM-1 is upgrading");
        let room = rooms.get(room_id).ok_or_else(|| anyhow!("fallback suppressed: room removed"))?;
        let record = &room.definition.fallbacks;
        ensure!(record.revision == origin.revision && record.settings.enabled,
            "fallback suppressed: room settings changed or paused");
        ensure!(rooms.room_by_session.get(&origin.session_id) == Some(&room_id)
            && !mandate_ended(&record.settings, Utc::now()), "fallback suppressed: the reminders' mandate ended");
        ensure!(effective(&record.settings, origin.session_id, Utc::now())?.is_some(), "fallback suppressed: recipient changed");
        ensure!(record.members.get(&origin.session_id).and_then(|row| row.occurrence.as_ref()) == Some(origin),
            "fallback suppressed: occurrence is no longer current");
        if let Some(stored) = room.retained_delivery(&RoomMessageSender::Operator {}, &request_id(origin)) {
            ensure!(stored.cursor.epoch == room.feed_epoch
                && stored.cursor.sequence > room.join_floor_by_session.get(&origin.session_id).copied().unwrap_or(0),
                "fallback suppressed: recipient left and rejoined the room");
        }
        let slot = slots.get_by_id(origin.session_id).ok_or_else(|| anyhow!("fallback suppressed: member removed"))?;
        ensure!(hosted(slot.definition.driver) && slot.running.is_some() && slot.run_id == Some(origin.run_id)
            && slot.generation == origin.generation && slot.state != LifecycleState::Starting
            && slot.spawn_in_flight.is_none() && slot.lifecycle_operation.is_none()
            && slot.stop_intent.is_none() && !slot.termination_uncertain && !slot.output_draining,
            "fallback suppressed: member stopped, starting, sleeping or changed run");
        Ok(())
    }

    pub(super) fn check_fallback_message(&self, message: &shared_types::RoomFeedEvent) -> Result<()> {
        if let Some(origin) = message_origin(message) {
            let rooms = self.inner.rooms.lock(); let slots = self.inner.slots.lock();
            self.fallback_still_valid(&rooms, &slots, message.room_id, origin)?;
        }
        Ok(())
    }

    fn fallback_snapshot_locked(room: &RoomRuntime, slots: &SessionRegistry) -> RoomFallbackSnapshot {
        let record = &room.definition.fallbacks;
        RoomFallbackSnapshot { room_id: room.definition.room_id, revision: record.revision,
            settings: record.settings.clone(), notice_warning: None, members: room.definition.member_ids.iter().filter_map(|id| {
                let slot = slots.get_by_id(*id)?;
                if !hosted(slot.definition.driver) { return None; }
                let row = record.members.get(id).cloned().unwrap_or_default();
                Some(RoomFallbackMemberStatus { session_id: *id, next_due_at: row.next_due_at.map(|at| at.to_rfc3339()),
                    last_message_id: row.last_message_id, last_status: row.last_status, note: row.note })
            }).collect() }
    }

    pub fn get_room_fallbacks(&self, room_id: RoomId) -> Result<RoomFallbackSnapshot> {
        self.get_room_fallbacks_as(room_id, None)
    }
    fn get_room_fallbacks_as(&self, room_id: RoomId, caller: Option<member_lifecycle::Actor<'_>>) -> Result<RoomFallbackSnapshot> {
        self.ensure_not_shutdown()?;
        let rooms = self.inner.rooms.lock(); let slots = self.inner.slots.lock();
        if let Some(caller) = caller { member_lifecycle::ManageCheck::of(caller, room_id).check(&rooms, &slots)?; }
        let room = rooms.get(room_id).ok_or_else(|| anyhow!("unknown room '{room_id}'"))?;
        Ok(Self::fallback_snapshot_locked(room, &slots))
    }

    pub fn set_room_fallbacks(&self, request: SetRoomFallbacksRequest) -> Result<RoomFallbackSnapshot> {
        self.set_room_fallbacks_as(request, None, Utc::now())
    }
    fn set_room_fallbacks_as(&self, request: SetRoomFallbacksRequest, caller: Option<member_lifecycle::Actor<'_>>,
        now: DateTime<Utc>) -> Result<RoomFallbackSnapshot> {
        self.ensure_active()?;
        let (mut snapshot, sender, actor) = {
            let _publish = self.inner.room_event_publish.lock();
            let mut rooms = self.inner.rooms.lock(); let slots = self.inner.slots.lock();
            if let Some(caller) = caller { member_lifecycle::ManageCheck::of(caller, request.room_id).check(&rooms, &slots)?; }
            let room = rooms.get(request.room_id).ok_or_else(|| anyhow!("unknown room '{}'", request.room_id))?;
            ensure!(room.definition.fallbacks.revision == request.revision, "fallback settings changed; reload before saving");
            let mut record = room.definition.fallbacks.clone();
            record.revision = record.revision.checked_add(1).ok_or_else(|| anyhow!("fallback revision exhausted"))?;
            record.settings = request.settings;
            validate_record(&record)?;
            // A mandate that has already ended cannot be saved: nothing would revive.
            ensure!(mandate_end(&record.settings).is_none_or(|end| end > now),
                "until is in the past; give a future end, or none");
            for id in record.settings.recipient_ids.iter().chain(record.settings.overrides.iter().map(|item| &item.session_id)) {
                ensure!(room.definition.member_ids.contains(id)
                    && slots.get_by_id(*id).is_some_and(|slot| hosted(slot.definition.driver)),
                    "fallback target {id} was removed or is not a hosted Claude, Codex or Grok member");
            }
            record.members.retain(|id, _| room.definition.member_ids.contains(id));
            let history_ready = history_ready(room);
            for id in &room.definition.member_ids {
                let Some(slot) = slots.get_by_id(*id).filter(|slot| hosted(slot.definition.driver)) else { continue };
                let schedule = effective(&record.settings, *id, now)?;
                if let Some((_, body)) = &schedule { validate_message_framing(body, routed_message_submit_behavior(slot.definition.driver))?; }
                if schedule.is_none() && !record.members.contains_key(id) { continue; }
                let row = record.members.entry(*id).or_default();
                let absent = history_ready && row.unresolved && row.occurrence.as_ref().is_some_and(|origin|
                    matches!(room.replayed_post(&request_id(origin), &RoomMessageSender::Operator {}, "", &[*id]), Ok(None)));
                if absent {
                    // Explicit Save rearms the schedule, not the old write.
                    // Bounded-history absence does not prove it never happened.
                    row.unresolved = false;
                    append_occurrence_note(row, "Settings saved: the old occurrence is absent from retained history and will not be repeated; its prior outcome remains unconfirmed");
                } else {
                    reconcile_occurrence(row, room, *id, history_ready);
                }
                if row.unresolved && history_ready {
                    let recorded_terminal = row.occurrence.as_ref().and_then(|origin|
                        room.fallback_delivery(&request_id(origin), *id)).is_some_and(|(result, _)|
                        result.receipts.iter().any(|receipt| receipt.recipient_id == *id && receipt.recorded
                            && matches!(receipt.status, RoomDeliveryStatus::Failed | RoomDeliveryStatus::Held
                                | RoomDeliveryStatus::Written | RoomDeliveryStatus::Woken | RoomDeliveryStatus::Fetched)));
                    if recorded_terminal {
                        // Explicit settings Save may begin a new schedule after
                        // a recorded partial failure. Never repeat the old intent
                        // or call its already-written bytes unattempted.
                        row.unresolved = false;
                        append_occurrence_note(row, "Settings changed after a recorded outcome; the old occurrence will not be repeated");
                    } else if !physical_outcome_uncertain(row) {
                        hold_occurrence(row, "Unattempted fallback suppressed by changed settings");
                    }
                }
                row.next_due_at = schedule.map(|(duration, _)| future(now, duration));
            }
            let mut snapshot = Self::fallback_snapshot_locked(room, &slots);
            snapshot.revision = record.revision;
            snapshot.settings = record.settings.clone();
            for member in &mut snapshot.members {
                let row = record.members.get(&member.session_id).cloned().unwrap_or_default();
                member.next_due_at = row.next_due_at.map(|at| at.to_rfc3339());
                member.last_message_id = row.last_message_id; member.last_status = row.last_status; member.note = row.note;
            }
            let (sender, actor) = match caller {
                Some(caller) => (caller.sender(), caller.label_locked(&slots)),
                None => (RoomMessageSender::Operator {}, "Operator".into()),
            };
            // A16 (Reviewer, 6490d57): an agent's grant and run are rechecked
            // where its save commits, so its slots guard stays held through the
            // catalog write and a revocation waits for it. The operator's own
            // save has no grant to keep and releases slots before the fsync.
            let held = caller.is_some().then_some(slots);
            self.commit_fallback_record(&mut rooms, request.room_id, record)?;
            drop(held);
            (snapshot, sender, actor)
        };
        self.emit(RuntimeEvent::RoomFallbacksChanged { room_id: request.room_id });
        let notice = format!("{actor} {} room fallbacks (settings revision {}).",
            if snapshot.settings.enabled { "updated and enabled" } else { "paused" }, snapshot.revision);
        if let Err(error) = self.post_room_message_as(request.room_id, sender, notice,
            Some(format!("fallback-settings-{}", Uuid::new_v4()))) {
            snapshot.notice_warning = Some(format!("Settings saved, but the attributed room notice could not be stored: {error:#}"));
            self.emit(RuntimeEvent::SystemLog { level: LogLevel::Warn,
                message: format!("Fallback settings saved, but its room notice failed: {error:#}"), timestamp: now_rfc3339() });
        }
        Ok(snapshot)
    }

    fn commit_fallback_record(&self, rooms: &mut RoomState, room_id: RoomId, record: RoomFallbackRecord) -> Result<()> {
        let mut candidate = rooms.catalog.clone();
        candidate.rooms.iter_mut().find(|room| room.room_id == room_id)
            .ok_or_else(|| anyhow!("room disappeared"))?.fallbacks = record.clone();
        self.persist_room_catalog_candidate(&candidate)?;
        rooms.get_mut(room_id).unwrap().definition.fallbacks = record;
        rooms.catalog = candidate;
        Ok(())
    }

    pub(super) fn start_room_fallback_timer(&self) {
        let weak = Arc::downgrade(&self.inner);
        self.inner.background_runtime.spawn(async move {
            let mut clock = FallbackClock::default();
            loop {
                tokio::time::sleep(TICK).await;
                let Some(inner) = weak.upgrade() else { break };
                if inner.shutdown_started.load(Ordering::Acquire) { break; }
                let result = tokio::task::spawn_blocking(move || {
                    let handle = SupervisorHandle { inner };
                    handle.tick_room_fallbacks(&mut clock, Utc::now(), Instant::now()); clock
                }).await;
                let Some(inner) = weak.upgrade() else { break };
                let Some(returned) = (SupervisorHandle { inner }).resume_fallback_clock(result) else { break };
                clock = returned;
            }
        });
    }

    pub(super) fn resume_fallback_clock(&self, result: std::result::Result<FallbackClock, tokio::task::JoinError>) -> Option<FallbackClock> {
        match result {
            Ok(clock) => Some(clock),
            Err(error) => {
                self.emit(RuntimeEvent::SystemLog { level: LogLevel::Error,
                    message: format!("Room fallback timer worker failed: {error}; {}",
                        if error.is_cancelled() { "timer cancelled" } else { "clock reset; scheduling will continue" }),
                    timestamp: now_rfc3339() });
                (!error.is_cancelled()).then(FallbackClock::default)
            }
        }
    }

    pub(super) fn tick_room_fallbacks(&self, clock: &mut FallbackClock, now: DateTime<Utc>, mono: Instant) {
        if self.ensure_active().is_err() { return; }
        let ids: Vec<_> = self.inner.rooms.lock().catalog.rooms.iter().map(|room| room.room_id).collect();
        clock.deadlines.retain(|(room, _), _| ids.contains(room));
        clock.missed.retain(|(room, _), _| ids.contains(room));
        for room_id in ids {
            if let Err(error) = self.tick_room_fallback(room_id, clock, now, mono) {
                clock.deadlines.retain(|(room, _), _| *room != room_id);
                clock.missed.retain(|(room, _), _| *room != room_id);
                self.emit(RuntimeEvent::SystemLog { level: LogLevel::Warn,
                    message: format!("Room {room_id} fallback not sent: {error:#}"), timestamp: now_rfc3339() });
            }
        }
    }

    fn tick_room_fallback(&self, room_id: RoomId, clock: &mut FallbackClock, now: DateTime<Utc>, mono: Instant) -> Result<()> {
        let (outgoing, changed) = {
            let _publish = self.inner.room_event_publish.lock();
            let mut rooms = self.inner.rooms.lock(); let slots = self.inner.slots.lock();
            let Some(room) = rooms.get(room_id) else { return Ok(()) };
            let history_ready = history_ready(room);
            let original = room.definition.fallbacks.clone();
            let mut record = original.clone();
            record.members.retain(|member, _| room.definition.member_ids.contains(member));
            clock.deadlines.retain(|(room_key, member), _| *room_key != room_id || room.definition.member_ids.contains(member));
            clock.missed.retain(|(room_key, member), _| *room_key != room_id || room.definition.member_ids.contains(member));
            let mut outgoing = Vec::new();
            for member in &room.definition.member_ids {
                let Some(slot) = slots.get_by_id(*member).filter(|slot| hosted(slot.definition.driver)) else { continue };
                let schedule = effective(&record.settings, *member, now)?;
                if schedule.is_none() && !record.members.contains_key(member) {
                    clock.deadlines.remove(&(room_id, *member)); clock.missed.remove(&(room_id, *member)); continue;
                }
                let row = record.members.entry(*member).or_default();
                reconcile_occurrence(row, room, *member, history_ready);
                let Some((duration, body)) = schedule else {
                    row.next_due_at = None; clock.deadlines.remove(&(room_id, *member)); clock.missed.remove(&(room_id, *member));
                    // v3 W6 (J10): said once, then quiet; nothing revives without a new save.
                    if let Some(end) = mandate_end(&record.settings).filter(|end| *end <= now) {
                        let note = format!("The reminders' mandate ended at {} UTC; none are scheduled. Save new settings to start again.",
                            end.format("%Y-%m-%d %H:%M"));
                        if row.note.as_deref() != Some(note.as_str()) { row.note = Some(note); }
                    }
                    continue;
                };
                let key = (room_id, *member);
                let mut cold_due = None;
                if clock.deadlines.get(&key).is_none_or(|deadline| deadline.0 != record.revision) {
                    clock.missed.remove(&key);
                    // v3 W6 (ruling b133623a): a saved due time that passed
                    // while no clock ran (a PRIM-1 restart) is due now, as missed.
                    if let Some(due) = row.next_due_at.filter(|due| *due <= now) {
                        cold_due = Some(due);
                        clock.deadlines.insert(key, (record.revision, mono));
                    } else {
                        let wait = restored_wait(row.next_due_at, now, duration);
                        clock.deadlines.insert(key, (record.revision, mono + wait));
                        row.next_due_at = Some(future(now, wait));
                    }
                }
                let deadline = clock.deadlines.get_mut(&key).unwrap();
                let pending = clock.missed.remove(&key);
                let mut scheduled_at = now;
                // How many scheduled times this occurrence answers for when it
                // is not simply on time, and the first of them; counted before
                // the phase advances (Reviewer 496079e4).
                let missed = if mono >= deadline.1 {
                    let elapsed = match cold_due {
                        Some(due) => (now - due).to_std().unwrap_or_default(),
                        None => mono.duration_since(deadline.1),
                    };
                    let boundaries = 1 + u64::try_from(elapsed.as_nanos() / duration.as_nanos()).unwrap_or(u64::MAX);
                    let wait = next_boundary(elapsed, duration);
                    scheduled_at = cold_due.or(row.next_due_at).unwrap_or(now);
                    *deadline = (record.revision, mono + wait);
                    row.next_due_at = Some(future(now, wait));
                    // A group still waiting folds into this boundary: the
                    // same one occurrence, now for one more time.
                    match pending {
                        Some((_, carried, first)) => Some((carried.saturating_add(boundaries), first)),
                        None if cold_due.is_some() || elapsed >= duration => Some((boundaries, scheduled_at)),
                        None => None,
                    }
                } else {
                    match pending {
                        Some((_, carried, first)) => Some((carried, first)),
                        None => continue,
                    }
                };
                row.next_due_at = Some(future(now, deadline.1.saturating_duration_since(mono)));
                // Kept for a later tick only while the member is expected to
                // run (starting, or being resumed after a PRIM-1 restart);
                // pause, an ended mandate, a stop, a new save, an owed reminder
                // or an unresolved earlier occurrence end it. While kept, the
                // saved due time stays at the first missed one, so a restart
                // counts them all again (Reviewer 43addeb5).
                let mut keep = |row: &mut MemberRecord, why: &str| {
                    let Some((count, first)) = missed else { return };
                    if slot.stop_intent.is_none() && (slot.running.is_some() || slot.running_at_shutdown) {
                        clock.missed.insert(key, (record.revision, count, first));
                        row.next_due_at = Some(first);
                        row.note = Some(format!("{} since {} UTC missed; one reminder follows once {why}",
                            scheduled_times(count), first.format("%H:%M")));
                    }
                };
                if row.unresolved { continue; }
                if !history_ready {
                    row.last_status = Some("skipped".into()); row.note = Some("Room history is recovering or unavailable; this scheduled occurrence was skipped".into());
                    keep(row, "room history has recovered"); continue;
                }
                if slot.running.is_none() || slot.run_id.is_none() || slot.state == LifecycleState::Starting
                    || slot.spawn_in_flight.is_some() || slot.lifecycle_operation.is_some() || slot.stop_intent.is_some()
                    || slot.termination_uncertain || slot.output_draining || self.inner.upgrade_closing.load(Ordering::Acquire) {
                    row.last_status = Some("skipped".into()); row.note = Some("Member is stopped, starting, sleeping or changing run".into());
                    keep(row, "the member is running"); continue;
                }
                // v3 W6: one owed reminder at a time. While the member has not
                // acknowledged or declined an earlier one, a tick posts nothing.
                if let Some(since) = room.owed_reminder_since(*member) {
                    let since = DateTime::parse_from_rfc3339(&since).map(|at| at.format("%H:%M").to_string())
                        .unwrap_or(since);
                    row.last_status = Some("skipped".into());
                    row.note = Some(format!("An earlier reminder ({since} UTC) is still owed; no new reminder was posted"));
                    continue;
                }
                let (body, note, scheduled_at) = match missed {
                    Some((count, first)) => {
                        let note = format!("{COALESCED_NOTE}{} since {} UTC that PRIM-1 could not send on time; \
                            {COALESCED_NOTE_END}", scheduled_times(count), first.format("%H:%M"));
                        let noted = format!("{body}\n\n[{note}]");
                        (if validate_message_body(&noted).is_ok() { noted } else { body }, Some(note), first)
                    }
                    None => (body, None, scheduled_at),
                };
                let origin = RoomFallbackOrigin { revision: record.revision, session_id: *member,
                    run_id: slot.run_id.unwrap(), generation: slot.generation, occurrence_id: Uuid::new_v4(),
                    scheduled_at: scheduled_at.to_rfc3339() };
                row.occurrence = Some(origin.clone()); row.unresolved = true; row.last_message_id = None;
                row.last_status = Some("posting".into()); row.note = note;
                outgoing.push((origin, body, room.definition.membership_revision));
            }
            let changed = record != original;
            drop(slots);
            if changed { self.commit_fallback_record(&mut rooms, room_id, record)?; }
            (outgoing, changed)
        };
        if changed { self.emit(RuntimeEvent::RoomFallbacksChanged { room_id }); }
        for (origin, content, membership_revision) in outgoing {
            let result = self.deliver_room_message_as(RoomDeliverySpec { guest_recipient_ids: Vec::new(), room_id,
                recipient_ids: vec![origin.session_id], membership_revision, sender: RoomMessageSender::Operator {},
                sender_label: "PRIM-1 fallback".into(), content, request_id: Some(request_id(&origin)),
                admission: RoomDeliveryAdmission::Member, roam_required: false, fallback_origin: Some(origin.clone()), ack_requested: true }, None);
            if let Err(error) = self.record_fallback_outcome(room_id, &origin, &result) {
                self.emit(RuntimeEvent::SystemLog { level: LogLevel::Error,
                    message: format!("Fallback {} outcome could not be saved: {error:#}; other recipients will still be processed", origin.occurrence_id),
                    timestamp: now_rfc3339() });
            }
            if let Err(error) = result {
                self.emit(RuntimeEvent::SystemLog { level: LogLevel::Warn,
                    message: format!("Fallback {} was not confirmed: {error:#}", origin.occurrence_id), timestamp: now_rfc3339() });
            }
        }
        Ok(())
    }

    fn record_fallback_outcome(&self, room_id: RoomId, origin: &RoomFallbackOrigin,
        outcome: &Result<RoomDeliveryResult>) -> Result<()> {
        let _publish = self.inner.room_event_publish.lock();
        let mut rooms = self.inner.rooms.lock();
        let Some(room) = rooms.get(room_id) else { return Ok(()) };
        let mut record = room.definition.fallbacks.clone();
        let Some(row) = record.members.get_mut(&origin.session_id).filter(|row| row.occurrence.as_ref() == Some(origin))
            else { return Ok(()) };
        if let Some((result, terminal)) = room.fallback_delivery(&request_id(origin), origin.session_id) {
            row.last_message_id = Some(result.message_id); row.unresolved = !terminal;
            if let Some(receipt) = result.receipts.first() {
                row.last_status = Some(format!("{:?}", receipt.status).to_ascii_lowercase());
                set_outcome_note(row, receipt.error.as_ref().map(|error| bounded_room_delivery_error(error)));
            }
        } else if let Err(error) = outcome {
            // Fresh delivery returns Err before Message commit, or a typed
            // uncertain Message append. After commit, transport/receipt failures
            // return Ok with receipts. The retained-post branch above wins even
            // if history became unhealthy; never erase a known partial write.
            if !room_commit_is_uncertain(error) {
                row.unresolved = false; row.last_status = Some("held".into());
                row.note = Some(bounded_room_delivery_error(&format!("Fallback not posted: {error}")));
            } else {
                row.last_status = Some("uncertain".into());
                row.note = Some(bounded_room_delivery_error(&format!("Fallback outcome is uncertain: {error}")));
            }
        }
        let changed = record != room.definition.fallbacks;
        if changed { self.commit_fallback_record(&mut rooms, room_id, record)?; }
        drop(rooms);
        drop(_publish);
        if changed { self.emit(RuntimeEvent::RoomFallbacksChanged { room_id }); }
        Ok(())
    }

    pub(super) fn apply_room_fallback_request(&self, caller: &PaneCaller, request: SidebandRequest) -> SidebandResponse {
        let result = (|| -> Result<RoomFallbackSnapshot> {
            self.authorize_sideband_request(caller, &request)?;
            #[cfg(test)] self.run_sideband_after_initial_authorization_for_tests();
            self.room_fallbacks_action(member_lifecycle::Actor::Pane(caller), request)
        })();
        match result {
            Ok(snapshot) => SidebandResponse { ok: true, message: "room fallback settings".into(), timed_out: false,
                payload: Some(SidebandResponsePayload::RoomFallbacks { snapshot }), request_id: None },
            Err(error) => Self::rejected_sideband_response(error.to_string()),
        }
    }

    /// A managing pane's room_fallbacks, or (A16) the operator entry's
    /// `fallbacks`: one room's settings through the same validation and
    /// revision check, the caller rechecked under the locks it commits with.
    /// The operator entry has no room of its own, so it names one.
    pub(super) fn room_fallbacks_action(&self, caller: member_lifecycle::Actor<'_>, request: SidebandRequest)
        -> Result<RoomFallbackSnapshot> {
        let SidebandRequest::RoomFallbacks { action, room_id, revision, settings } = request else { bail!("not a fallback request") };
        let room_id = room_id.or_else(|| caller.own_room(&self.inner.rooms.lock())).ok_or_else(|| anyhow!(match caller {
            member_lifecycle::Actor::Pane(_) => "the caller is not in a room; choose a room with roam permission",
            member_lifecycle::Actor::Local(_) => "the operator entry has no room of its own; name one with --room <room id>",
        }))?;
        match action {
            RoomFallbackAction::Get => {
                ensure!(revision.is_none() && settings.is_none(), "get takes neither revision nor settings");
                self.get_room_fallbacks_as(room_id, Some(caller))
            }
            RoomFallbackAction::Set => self.set_room_fallbacks_as(SetRoomFallbacksRequest { room_id,
                revision: revision.ok_or_else(|| anyhow!("set needs revision"))?,
                settings: settings.ok_or_else(|| anyhow!("set needs settings"))? }, Some(caller), Utc::now()),
        }
    }
}
