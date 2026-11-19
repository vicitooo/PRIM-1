//! Durable room intent and recipient wake attempts. A Pending receipt is synced
//! before a side effect; retries compare the last durable receipt cursor.
//! A106: Claude, Codex and Grok receive the whole room message the way the
//! live PRIM-1 delivers it: one bracketed paste with the driver's measured
//! submit sequence, also while the member works. A member that is stopped,
//! starting, on a prompt or without paste mode keeps the message queued
//! (the live PRIM-1 refuses those sends) and receives it once it can.
use super::*;
use shared_types::{MessageId, RetryRoomDeliveryRequest, RoomDeliveryReceipt, RoomFeedEvent, RoomSequence};
use std::collections::VecDeque;

struct RoomWakePlan {
    fallback: Option<(RoomId, shared_types::RoomFallbackOrigin)>,
    continuation: Option<(RoomId, shared_types::RoomContinuationOrigin)>,
    target: RunWriteTarget,
    behavior: SubmitBehavior,
    payload: String,
    input_epoch: u64,
    /// v3 W3: a pointer wake counts every unread message for this recipient
    /// up to this sequence; queued wakes for those are covered by it.
    pointer_through: Option<RoomSequence>,
    /// v3 W3 (Reviewer b307957c): the requests this pointer counts, as
    /// admitted under the Pending lock; the already-read checks ask about
    /// exactly these. Empty until then.
    pointer_counted: Vec<RoomSequence>,
    /// v3 W2 #2: typed past the cap while Codex's own title still read this
    /// busy state; the Written receipt says so.
    title_busy: Option<String>,
    /// v3 W4 S2: this attempt goes to the member's Codex App Server as one
    /// turn/start, never typed; chosen here, before any send (W4-DECISIONS S2).
    app_server: bool,
}

/// v3 W4 S2: the Codex App Server answered this attempt's own request with
/// an error; nothing is inferred about a zero write and nothing is retried.
#[derive(Debug)]
struct AppServerRefused(String);

impl std::fmt::Display for AppServerRefused {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "the Codex App Server refused the room message: {}", self.0)
    }
}

impl std::error::Error for AppServerRefused {}

/// v3 W4 S2: the request was (or may have been) written and not answered.
#[derive(Debug)]
struct AppServerUnknown(String);

impl std::fmt::Display for AppServerUnknown {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "whether the Codex App Server took the room message is unknown ({}); it is not sent again", self.0)
    }
}

impl std::error::Error for AppServerUnknown {}

/// What a write boundary passed.
enum WakeWrite {
    /// Bytes and CR reached the terminal.
    Typed(usize),
    /// The response to our own turn/start named this turn.
    Accepted(String),
}

/// The write-boundary refusals that re-admit a wake, set from whichever
/// thread ran the checks (the observer's, for the App Server route).
#[derive(Default)]
struct WakeCheckFlags {
    transient: AtomicBool,
    count_changed: AtomicBool,
    already_read: AtomicBool,
    route_changed: AtomicBool,
    /// The App Server request's own checks refused it (not the route).
    check_refused: AtomicBool,
    /// v3 W4 S3: the member's App Server began asking a person.
    person_request: AtomicBool,
}

/// What the write-boundary checks need, owned, so the App Server route can
/// run them on the observer thread immediately before the socket write
/// (Reviewer 617c8f1c).
#[derive(Clone)]
struct WakeCheck {
    message: RoomFeedEvent,
    fallback: Option<(RoomId, shared_types::RoomFallbackOrigin)>,
    continuation: Option<(RoomId, shared_types::RoomContinuationOrigin)>,
    pointer: bool,
    pointer_counted: Vec<RoomSequence>,
    input_epoch: u64,
    session_id: SessionId,
    run_id: Uuid,
    generation: SessionGeneration,
    input_gate: Arc<RunInputGate>,
    /// v3 W4 S3: the run's App Server observer, whichever route this wake takes.
    app_server: Option<codex_app_server::Dispatcher>,
}

/// v3 W2 #2 (Claude PRIM-1, d9613ce walk): a busy title holds a wake at most
/// this long. A title is a belief, and Codex queues input typed mid-turn.
pub(super) const ROOM_WAKE_TITLE_BUSY_CAP: Duration = Duration::from_secs(120);

/// v3 W3: what confirming one written wake's submission needs.
struct SubmitConfirm {
    driver: DriverKind,
    harness_id: Option<String>,
    identity_unresolved: bool,
    working_dir: String,
    since: chrono::DateTime<Utc>,
    /// The exact typed pointer, with the full newest message id.
    payload: String,
    room_id: RoomId,
    recipient_id: SessionId,
    /// The head first, then the requests its pointer covered.
    messages: Vec<RoomFeedEvent>,
    /// Each request's own Written receipt of this attempt: a result is
    /// recorded only while that receipt is still the latest (Reviewer a5d87af3).
    attempts: HashMap<MessageId, shared_types::RoomFeedCursor>,
    /// The attempt token: the head's Pending cursor.
    attempt_token: Option<shared_types::RoomFeedCursor>,
    window: Duration,
    /// v3 W4 S2: accepted by the Codex App Server, not typed.
    via_app_server: bool,
}

/// v3 W3b: one late-reconciliation item, resolved outside every lock.
struct ReconcileItem {
    room_id: RoomId,
    event: RoomFeedEvent,
    recipient_id: SessionId,
    attempt: shared_types::RoomFeedCursor,
    written: shared_types::RoomFeedCursor,
    typed: String,
    driver: DriverKind,
    path: std::path::PathBuf,
    since: chrono::DateTime<Utc>,
    source: &'static str,
}

/// v3 W3b: the reconciler's period and batch bound.
const SUBMIT_RECONCILE_EVERY: Duration = Duration::from_secs(15);
const SUBMIT_RECONCILE_BATCH: usize = 16;

/// v3 W3: how often a written wake's submission is looked for.
const SUBMIT_CONFIRM_POLL: Duration = Duration::from_millis(1000);
/// v3 W3: how long a written wake's submission is looked for in production.
pub(super) const SUBMIT_CONFIRM_WINDOW: Duration = Duration::from_secs(30);

/// v3 W3 (D4): the content-neutral wake typed for an ordinary message: fixed
/// text, a count and an opaque id, no labels or body, so nothing the room
/// says can reach the recipient's screen through PRIM-1's own paste.
/// Whether a wake of `message` types the content-neutral pointer (which
/// counts every unread message for the recipient) instead of its body.
/// v3 W3 (D4): an ACK-tracked message does; legacy history keeps the
/// full-body wake it always had. v3 W6: so does an ACK-tracked reminder; a
/// legacy reminder keeps its full body. v3 W3 (D6 wake): so does an
/// addressed PRIM-1 notice. v3 (Claude PRIM-1, 15a49bdf): so do PRIM-1's
/// ACK-tracked upgrade and recovery continuations; a legacy recovery
/// continuation keeps its full body.
pub(super) fn wakes_by_pointer(message: &RoomFeedEvent) -> bool {
    matches!(&message.item, RoomFeedItem::Message { ack_requested: true, .. }
        | RoomFeedItem::Message { sender: RoomMessageSender::Prim1 {}, .. })
}

pub(super) fn room_pointer_text(count: usize, room_id: RoomId, newest: MessageId, attempt: Option<shared_types::RoomFeedCursor>) -> String {
    // The full room id is the read/ACK context (room_read room_id); a pane
    // that has left that room is refused rather than reading another. The
    // full newest id makes the typed pointer an exact token to confirm in the
    // member's own conversation record (Reviewer e02495a5).
    // The attempt token (the head's Pending cursor, epoch and sequence) makes
    // each attempt's typed text unique, so a record of another attempt never
    // matches it (Reviewer ab3f8bf3).
    let attempt = attempt.map(|cursor| format!(", attempt {}:{}", cursor.epoch, cursor.sequence)).unwrap_or_default();
    format!("[PRIM-1] {count} new room message{} for you in room {room_id} (newest {newest}{attempt}). Run room_read with that room_id, then room_ack each.",
        if count == 1 { "" } else { "s" })
}

/// v3 W3 (D3): the code a deferred wake is recorded under, from the reason
/// the planner gave. Refusal tails carry their own code in brackets.
pub(super) fn wake_reason_code(reason: &str) -> String {
    if let Some(start) = reason.find("work state is ") {
        let tail = &reason[start..];
        if let (Some(open), Some(close)) = (tail.find('('), tail.find(')')) {
            if open < close { return tail[open + 1..close].to_string(); }
        }
    }
    let code = if reason.contains("left this room") { "left_room" }
        else if reason.contains("not running") { "stopped" }
        else if reason.contains("still starting") { "starting" }
        else if reason.contains("no output since it started") { "stalled" }
        else if reason.contains("first prompt") || reason.contains("has not seen its prompt") { "not_observed" }
        else if reason.contains("unsent text") { "draft_open" }
        else if reason.contains("operator typed") || reason.contains("operator typing") { "operator_typing" }
        else if reason.contains("another room message is being typed") { "wake_in_progress" }
        else if reason.contains("earlier room message for this member") { "behind_earlier" }
        else if reason.contains("input or another wake is active") { "input_busy" }
        else if reason.contains("bracketed paste") || reason.contains("paste mode") { "paste_mode_unknown" }
        else if reason.contains("screen is still changing") { "output_settling" }
        else if reason.contains("stopped being current") { "count_changed" }
        else if reason.contains("App Server route changed") { "route_changed" }
        else if reason.contains("own title reads") { "title_busy" }
        else if reason.contains("question") || reason.contains("asking") { "modal" }
        else { "not_ready" };
    code.into()
}

/// v3 W3 (Reviewer 79ab53d8): the refusal recorded when a reminder or
/// continuation a pointer counts is no longer current at the write boundary,
/// and the reason its group waits under until it is planned again.
/// v3 W4 S2 (Reviewer 8caec584): so is a counted request whose guest sender
/// was revoked, or that the member's current join no longer covers.
const ROOM_WAKE_COUNT_CHANGED_REFUSAL: &str = "a message this wake counted stopped being current (a reminder or \
    continuation ended, its guest sender was revoked, or it predates the member's current join) just before it \
    was typed; nothing was typed";
const ROOM_WAKE_COUNT_CHANGED: &str = "a message this wake counted stopped being current; \
    the wake is planned again without it";

/// v3 W4 S2 (Reviewer 8caec584): the recipient left the room, or rejoined
/// after this message, between enqueue and the write.
const ROOM_WAKE_MEMBERSHIP_CHANGED_REFUSAL: &str = "the member was removed from this room, or rejoined after this \
    message, before it was sent; nothing was sent";

/// v3 W4 S2: the submission record of an App Server request sent (or
/// possibly sent) and never answered.
const APP_SERVER_SUBMIT_UNKNOWN: &str = "The room message was sent to the Codex App Server and not answered; whether \
    it was taken is unknown. It is not sent again: the member reads it with its next room_read, or prompt it.";

/// v3 W4 S2: the refusal recorded when the App Server route planned for a
/// wake is gone before anything was sent; it is planned again from the start.
const ROOM_WAKE_ROUTE_CHANGED_REFUSAL: &str = "the member's Codex App Server route changed before the room message was sent; nothing was sent";
const ROOM_WAKE_ROUTE_CHANGED: &str = "the member's Codex App Server route changed; the wake is planned again";

/// v3 W4 S3: a person request (approval, question, form) opened on the
/// member's App Server between planning and the write. The terminal answers
/// it; PRIM-1 never does, and sends nothing until it is resolved.
const ROOM_WAKE_PERSON_REQUEST_REFUSAL: &str = "the member's Codex App Server began asking a person in its terminal just before the room message; nothing was sent";
const ROOM_WAKE_PERSON_REQUEST: &str = "the member is asking a person in its terminal; the wake is planned again once that is answered there";

/// v3 W4 S3: the hold while the member's App Server waits on a person (also
/// read by the room directory, cb415335).
pub(super) fn person_request_hold(slot: &SessionSlot) -> Option<String> {
    let (request, followed) = slot.running.as_ref()?.app_server.as_ref()?.open_request()?;
    Some(person_request_wait(&request, followed))
}

/// v3 W4 S2: whether a pointer wake for this member goes to its App Server:
/// no conversation hold, and the run's observer follows exactly its bound
/// thread (also read by the room directory, cb415335).
pub(super) fn app_server_route(slot: &SessionSlot) -> bool {
    slot.recovery.is_none()
        && slot.running.as_ref().and_then(|running| running.app_server.as_ref())
            .zip(slot.harness_session_id.as_deref())
            .is_some_and(|(dispatcher, thread)| dispatcher.serves(thread))
}

/// v3 W4 S3: why a wake waits while a person request is open, or was seen
/// open before PRIM-1 lost its connection to the owner (then it is
/// uncertain, S4: until its own resolution or replay, or the run's end).
pub(crate) fn person_request_wait(request: &codex_app_server::OpenRequest, followed: bool) -> String {
    if followed {
        format!("the member's Codex App Server is asking a person in its terminal ({}, request {}); PRIM-1 never answers it, and the message waits until it is answered there", request.method, request.id)
    } else {
        format!("the member's Codex App Server was asking a person in its terminal ({}, request {}) when PRIM-1 lost its connection to it, so whether it was answered is unknown; PRIM-1 never answers it, and the message waits until PRIM-1 sees it resolved or this run of the member ends: restarting the member clears it", request.method, request.id)
    }
}

/// v3 W3: the refusal recorded when fresh output arrives before the first byte.
const ROOM_WAKE_OUTPUT_CHANGED_REFUSAL: &str = "the member's screen changed just before the room message; it was not typed";
/// v3 W3: a pointer refused before any byte because the member had already
/// read or disposed of everything it counts.
const ROOM_WAKE_ALREADY_READ: &str = "the member had already read, disposed of, or been spared (withdrawn) every message this wake points to; nothing was typed";
/// v3 W3: a transient refusal whose re-admission budget is spent.
const ROOM_WAKE_READMIT_SPENT: &str = "the member's screen kept changing before the room message was typed (3 automatic attempts); it was not typed. Use Retry wake";

fn set_readmit(item: &mut RoomFeedItem, value: bool) {
    if let RoomFeedItem::Delivery { readmit, .. } = item { *readmit = value; }
}


/// v3 W3: the Waiting reason after a confirmed-zero refusal for fresh output.
const ROOM_WAKE_OUTPUT_CHANGED: &str = "the member's screen is still changing; the message waits until it settles";

pub(super) const ROOM_WAKE_PENDING: &str = "another room message is being typed into this member; this one waits its turn";

/// Operator keystrokes into the member's terminal this recent may be a draft.
const ROOM_WAKE_TYPING_GRACE: Duration = Duration::from_secs(5);
/// v3 W2 (M1 re-run on 4250194): a Codex mid-turn repaint can show a clean
/// prompt for ~0.3 s while the turn continues; while working, 0.164 repaints
/// its status about every 45 ms. A wake waits for this much output quiet.
pub(super) const ROOM_WAKE_CODEX_OUTPUT_SETTLE: Duration = Duration::from_millis(1200);
/// A queued message waits this long for its member before it is held.
const ROOM_WAKE_MAX_WAIT: Duration = Duration::from_secs(12 * 60 * 60);
const ROOM_WAKE_POLL: Duration = Duration::from_millis(300);
/// A message typed this long after it was stored says so in its header.
const QUEUED_NOTE_AFTER: Duration = Duration::from_secs(5 * 60);
/// Queue bounds; a message beyond them is Held at once (Retry wake later).
const ROOM_WAKE_QUEUE_MAX: usize = 512;
const ROOM_WAKE_QUEUE_MAX_PER_MEMBER: usize = 64;

/// A16 D3 (A10, 20 Oct: a member started hours late acted on its whole
/// Stored backlog as live): "<sender>, queued since 11:45 UTC, 5 h 29 min
/// ago" for a message typed `after` or longer after it was stored.
pub(super) fn queued_sender_label(sender_label: &str, stored_at: &str, after: Duration, now: DateTime<Utc>) -> String {
    let Some(stored) = DateTime::parse_from_rfc3339(stored_at).ok().map(|at| at.with_timezone(&Utc)) else {
        return sender_label.into();
    };
    let Some(age) = (now - stored).to_std().ok().filter(|age| *age >= after) else {
        return sender_label.into();
    };
    let minutes = age.as_secs() / 60;
    let ago = match minutes {
        0 => "under a minute ago".to_string(),
        1..=59 => format!("{minutes} min ago"),
        _ => format!("{} h {} min ago", minutes / 60, minutes % 60),
    };
    format!("{sender_label}, queued since {} UTC, {ago}", stored.format("%H:%M"))
}

pub(super) fn unsupported_room_wake(driver: DriverKind) -> Option<&'static str> {
    match driver {
        DriverKind::Claude | DriverKind::Codex | DriverKind::Grok => None,
        DriverKind::Prime | DriverKind::GenericTerminal => Some("this pane has no verified room-wake adapter in this build. Read its message in the PRIM-1 feed and provide context manually; Retry wake is unavailable"),
    }
}

/// The member cannot take typed input yet; the message stays queued.
#[derive(Debug)]
pub(super) struct RoomWakeNotReady(pub String);

impl std::fmt::Display for RoomWakeNotReady {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RoomWakeNotReady {}

fn not_ready(reason: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(RoomWakeNotReady(reason.into()))
}

/// Messages waiting for their member, oldest first. One dispatcher thread
/// runs while any entry exists; each member receives its messages in order.
#[derive(Default)]
pub(super) struct RoomWakeQueue {
    entries: VecDeque<QueuedRoomWake>,
    running: bool,
    /// v3 W3: recipients whose head is being attempted on its own thread.
    in_flight: HashSet<SessionId>,
    /// Tests shorten the 12 h wait to reach the hold deterministically.
    #[cfg(test)]
    max_wait: Option<Duration>,
    /// Tests shorten the 5 min after which a typed message names its age.
    #[cfg(test)]
    queued_note_after: Option<Duration>,
}

#[derive(Clone)]
struct QueuedRoomWake {
    message: RoomFeedEvent,
    recipient_id: SessionId,
    sender_label: String,
    queued_at: Instant,
    /// A16 (T-30): the reason last written to the audit log for this wait.
    last_reason: Option<String>,
    /// v3 W6: a changed deferral code not yet recorded, and since when it holds.
    pending_code: Option<(String, Instant)>,
}

/// v3 W6 (0b708ae: output_settling and not_observed alternated, 4 records in
/// 6 s after a resume): a wait's first code is recorded at once, so no queue is
/// silent; a later change is recorded once the new code has held this long.
const WAITING_CODE_SETTLE: Duration = Duration::from_secs(2);

/// v3 W6: whether `code` should be recorded now, given the code already
/// recorded for this wait and a pending change (updated in place).
pub(super) fn settle_waiting_code(recorded: Option<&str>, pending: &mut Option<(String, Instant)>, code: &str,
    now: Instant, settle: Duration) -> bool {
    match recorded {
        None => { *pending = None; true }
        Some(recorded) if recorded == code => { *pending = None; false }
        Some(_) => match pending {
            Some((pending_code, since)) if pending_code == code => now.saturating_duration_since(*since) >= settle,
            _ => { *pending = Some((code.to_owned(), now)); false }
        },
    }
}

/// Like the live PRIM-1, a member that is thinking or running a tool is typed
/// into: Claude, Codex and Grok queue a submitted message behind their turn.
/// Only a member that cannot take a paste at all keeps the message waiting.
pub(super) fn room_wake_busy(slot: &SessionSlot) -> Option<std::borrow::Cow<'static, str>> {
    if slot.running.is_none() || slot.run_id.is_none() {
        // v3 (e20ec145, Claude PRIM-1 00b17bb1): a member that failed, at
        // launch or after it ran (Codex PRIM-1: Failed + last_error does not
        // say which), gives its recorded reason, not only that it is not running.
        if slot.state == LifecycleState::Failed && let Some(error) = slot.last_error.as_deref() {
            return Some(failed_member_wait(error, slot.termination_uncertain).into());
        }
        return Some("the member is not running; the message waits until it starts".into());
    }
    if slot.state == LifecycleState::Starting {
        return Some("the member is still starting".into());
    }
    if slot.state == LifecycleState::Stalled {
        return Some("the member has shown no output since it started; stop it and start it again".into());
    }
    // Claude buffers keys typed before its first screen; a trust question
    // drawn next would consume them. Wait for its first observed state.
    if slot.definition.driver == DriverKind::Claude && !slot.work_state_observed {
        return Some("waiting for the member's first prompt".into());
    }
    slot.room_composer.open_question().map(Into::into)
}

/// The longest launch error quoted in a wait reason, in characters.
const LAUNCH_ERROR_QUOTED: usize = 300;

/// Why a wake waits for a failed member: neutral about whether it failed to
/// start or after it ran, and with no advice to start it while PRIM-1 has not
/// yet confirmed that its process ended.
fn failed_member_wait(error: &str, termination_uncertain: bool) -> String {
    let quoted: String = error.chars().take(LAUNCH_ERROR_QUOTED).collect();
    let more = if error.chars().count() > LAUNCH_ERROR_QUOTED { "…" } else { "" };
    if termination_uncertain {
        format!("the member is not running; it failed ({quoted}{more}) and PRIM-1 has not yet confirmed that its process ended, so the message waits")
    } else {
        format!("the member is not running; it failed ({quoted}{more}); the message waits until it is started again")
    }
}

pub(super) fn room_wake_error(error: &anyhow::Error) -> String {
    if let Some(refusal) = error.downcast_ref::<RoutedInputRefusal>() {
        return bounded_room_delivery_error(&refusal.room_reason);
    }
    bounded_room_delivery_error(&error.to_string())
}

fn room_error_after_progress(error: anyhow::Error, prior_bytes: usize) -> anyhow::Error {
    let bytes = error.downcast_ref::<PtyWriteError>().map(PtyWriteError::bytes_written).unwrap_or(0);
    PtyWriteError::new(prior_bytes.saturating_add(bytes), room_wake_error(&error)).into()
}

/// No payload or backlog: the run already owns this gate. Fetched and bounded
/// receipt retention cannot release a transport attempt still in progress.
struct RoomWakeClaim {
    gate: Arc<RunInputGate>,
    pending: bool,
}

impl RoomWakeClaim {
    fn try_acquire(gate: &Arc<RunInputGate>) -> Option<Self> {
        gate.room_wake_pending.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok().map(|_| Self { gate: gate.clone(), pending: false })
    }
}

impl Drop for RoomWakeClaim {
    fn drop(&mut self) {
        // Once transport can have begun, only a durable outcome permits the
        // next wake. The explicit pre-transport claim failure releases below;
        // a panic or uncertain final commit stays held without any retry.
        if !self.pending {
            *self.gate.room_wake_claim.lock() = None;
            self.gate.room_wake_pending.store(false, Ordering::SeqCst);
        }
    }
}

impl SupervisorHandle {
    pub(super) fn deliver_room_message_as(
        &self, spec: RoomDeliverySpec, caller: Option<&PaneCaller>,
    ) -> Result<RoomDeliveryResult> {
        self.deliver_room_message_unmarked(spec, caller).map(|result| self.mark_revoked_guest_receipts(result))
    }

    /// v3 W5 (decision 9): a revoked guest's row that is still open reads as
    /// held with why, never as plainly stored; its disposition, once made,
    /// stands as it is. The room store cannot see revocations, so it is marked here.
    pub(super) fn mark_revoked_guest_receipts(&self, mut result: RoomDeliveryResult) -> RoomDeliveryResult {
        if result.guest_receipts.is_empty() { return result; }
        let registry = self.inner.guests.lock();
        for row in &mut result.guest_receipts {
            if row.status == "stored" && !registry.is_invited(row.guest_id) {
                row.status = "held".into();
                row.note = Some("the guest's invitation was revoked; the message stays until the operator disposes of it".into());
            }
        }
        result
    }

    fn deliver_room_message_unmarked(
        &self, spec: RoomDeliverySpec, caller: Option<&PaneCaller>,
    ) -> Result<RoomDeliveryResult> {
        let RoomDeliverySpec { room_id, recipient_ids, membership_revision,
            sender, sender_label, content, request_id, admission, roam_required, fallback_origin, ack_requested,
            guest_recipient_ids } = spec;
        let continuation_origin = match admission {
            RoomDeliveryAdmission::Continuation { session_id, continuation_id, .. } =>
                Some(shared_types::RoomContinuationOrigin { session_id, continuation_id }),
            _ => None,
        };
        anyhow::ensure!(fallback_origin.is_none() || continuation_origin.is_none(),
            "a message cannot be both a scheduled fallback and a recovery continuation");
        // S7: a held preview trial admits no room mutation.
        self.ensure_mutations_admitted()?;
        validate_message_body(&content)?;
        let request_id = rooms::request_id(request_id)?;
        #[cfg(test)]
        if let Some(hook) = self.inner.room_delivery_after_preflight.lock().take() { hook(); }
        let (message_event, _lease) = {
            let _publish = self.inner.room_event_publish.lock();
            self.recover_room_writer(room_id)?;
            let mut rooms = self.inner.rooms.lock();
            if let Some(origin) = &fallback_origin {
                let slots = self.inner.slots.lock();
                self.fallback_still_valid(&rooms, &slots, room_id, origin)?;
                if recipient_ids != [origin.session_id] || caller.is_some() || sender != (RoomMessageSender::Operator {}) {
                    bail!("fallback origin must name the scheduler's single hosted recipient");
                }
            }
            if let RoomDeliveryAdmission::Continuation { session_id, generation, run_id, continuation_id } = admission {
                let slots = self.inner.slots.lock();
                self.admit_recovery_post(&slots, session_id, generation, run_id, continuation_id)?;
            }
            if let Some(caller) = caller {
                let slots = self.inner.slots.lock();
                // A109: an explicit room_id needs roam, even for the
                // caller's own room; the grant may have been withdrawn since.
                if roam_required || admission == RoomDeliveryAdmission::Roaming {
                    Self::pane_roam_locked(caller, &slots)?;
                }
                match admission {
                    RoomDeliveryAdmission::Member => {
                        let (current_room, _) = Self::pane_room_binding_locked(caller, &slots, &rooms)?;
                        if current_room != room_id {
                            return Err(anyhow!("sideband caller changed rooms before delivery commit"));
                        }
                    }
                    // A visitor: roam was checked above.
                    RoomDeliveryAdmission::Roaming => {}
                    // The grant of the visitor being answered may have been withdrawn.
                    RoomDeliveryAdmission::Reply { visitor } =>
                        Self::pane_reply_locked(caller, &slots, &rooms, visitor)?,
                    // A110: a managing pane's brief; its grant may have been
                    // withdrawn or the member moved since admission.
                    RoomDeliveryAdmission::Managing => {
                        let check = member_lifecycle::ManageCheck::new(caller, room_id);
                        for recipient in &recipient_ids {
                            check.check_target(&rooms, &slots, *recipient)?;
                        }
                    }
                    RoomDeliveryAdmission::Continuation { .. } => bail!("continuation is not a pane delivery admission"),
                }
                // Admission is now bound to the caller's current run. Never
                // retain the registry guard through journal I/O.
            }
            // A109: who may answer a roaming sender without roam itself.
            let cross_room_sender = match (&sender, admission) {
                (RoomMessageSender::Session { session_id }, RoomDeliveryAdmission::Roaming) => Some(*session_id),
                // A110: so may a managing member of another room.
                (RoomMessageSender::Session { session_id }, RoomDeliveryAdmission::Managing)
                    if rooms.room_by_session.get(session_id) != Some(&room_id) => Some(*session_id),
                _ => None,
            };
            let room = rooms.get_mut(room_id).ok_or_else(|| anyhow!("room '{room_id}' was deleted before delivery"))?;
            if room.definition.membership_revision != membership_revision {
                return Err(anyhow!("room '{room_id}' membership changed before commit; retry"));
            }
            if let RoomMessageSender::Session { session_id } = &sender {
                if admission == RoomDeliveryAdmission::Member && !room.definition.member_ids.contains(session_id) {
                    return Err(anyhow!("sender is no longer a room member"));
                }
            }
            // A revoke can race the append; the guest registry is a leaf lock.
            if let RoomMessageSender::Guest { guest_id, .. } = &sender {
                self.ensure_guest_in_room(*guest_id, room_id)?;
            }
            if recipient_ids.iter().any(|id| !room.definition.member_ids.contains(id)) {
                return Err(anyhow!("recipient is no longer a room member"));
            }
            if room.in_flight_deliveries == usize::MAX { return Err(anyhow!("room delivery counter exhausted")); }
            if let Some(event) = room.replayed_post(&request_id, &sender, &content, &recipient_ids)? {
                if let RoomFeedItem::Message { fallback_origin: prior, .. } = &event.item {
                    if prior != &fallback_origin { bail!("delivery origin differs from stored intent"); }
                }
                if recovery_automation::message_origin(&event) != continuation_origin.as_ref() {
                    bail!("delivery origin differs from stored continuation intent");
                }
                return Ok(room.delivery_result(&event, true));
            }
            room.ensure_receipt_evidence_capacity(recipient_ids.len())?;
            room.ensure_sequence_capacity(1 + 2 * recipient_ids.len())?;
            let posted = if ack_requested && fallback_origin.is_none() && continuation_origin.is_none() {
                // v3 W5: each guest's label now, stored with its id for history.
                let invited = self.room_guests(room_id);
                let guests = guest_recipient_ids.iter().map(|id| (*id, invited.iter().find(|guest| guest.guest_id == *id)
                    .map(|guest| guest.label.clone()).unwrap_or_default())).collect();
                room.post_requesting_acks_with_guests(request_id.clone(), sender, content.clone(), recipient_ids.clone(),
                    guests, membership_revision)
            } else if ack_requested && let Some(origin) = fallback_origin.clone() && continuation_origin.is_none() {
                room.post_reminder_requesting_acks(request_id.clone(), sender, content.clone(), recipient_ids.clone(),
                    membership_revision, origin)
            } else if ack_requested && let Some(origin) = continuation_origin.clone() && fallback_origin.is_none() {
                room.post_continuation_requesting_acks(request_id.clone(), sender, content.clone(), recipient_ids.clone(),
                    membership_revision, origin)
            } else {
                room.post_with_origins(request_id.clone(), sender, content.clone(),
                    recipient_ids.clone(), membership_revision, fallback_origin, continuation_origin)
            };
            let (event, replayed) = match posted {
                Ok(post) => post,
                Err(error) => {
                    drop(rooms);
                    self.notify_room_history_transition(room_id);
                    return Err(error);
                }
            };
            if replayed { return Ok(room.delivery_result(&event, true)); }
            room.in_flight_deliveries += 1;
            room.receipt_reservations += recipient_ids.len();
            if let Some(sender) = cross_room_sender {
                Self::note_cross_room_delivery_locked(&mut rooms, sender, &recipient_ids);
            }
            drop(rooms);
            let lease = RoomDeliveryLease { inner: self.inner.clone(), room_id, receipt_reservations: recipient_ids.len() };
            #[cfg(test)]
            self.run_room_event_after_append_hook_for_tests();
            self.emit(RuntimeEvent::RoomFeedEvent { feed_event: event.clone() });
            (event, lease)
        };
        self.refresh_session_liveness();
        let mut receipts = Vec::with_capacity(recipient_ids.len());
        for recipient_id in &recipient_ids {
            let mut planned = self.plan_room_wake(*recipient_id, &message_event, &sender_label);
            if let Some(not_ready) = planned.as_ref().err().and_then(|error| error.downcast_ref::<RoomWakeNotReady>()) {
                // Not able to take input yet: queue it until it can, and say why
                // (T-30: "Stored, run_id null" alone named no reason).
                let reason = not_ready.0.clone();
                if self.enqueue_room_wake(&message_event, *recipient_id, &sender_label, Instant::now()) {
                    self.note_room_wake_waiting(&message_event, *recipient_id, &reason);
                    let mut queued = self.inner.rooms.lock().get(room_id)
                        .expect("in-flight room cannot be deleted").delivery_result(&message_event, false)
                        .receipts.into_iter().find(|r| r.recipient_id == *recipient_id).expect("original recipient");
                    if queued.status == RoomDeliveryStatus::Stored {
                        queued.queued_because = Some(reason);
                    }
                    receipts.push(queued);
                    continue;
                }
                planned = Err(anyhow!("too many room messages are waiting for this member; use Retry wake once it is ready"));
            }
            let mut receipt = self.perform_room_wake(&message_event, *recipient_id, planned, None, None)?
                .unwrap_or_else(|| self.inner.rooms.lock().get(room_id)
                    .expect("in-flight room cannot be deleted").delivery_result(&message_event, false)
                    .receipts.into_iter().find(|r| r.recipient_id == *recipient_id).expect("original recipient"));
            // The first result says what a replay of the stored state says.
            if receipt.recorded && receipt.status == RoomDeliveryStatus::Held && receipt.hold.is_none() {
                receipt.hold = Some(receipt.error.clone().unwrap_or_else(|| "held".into()));
            }
            self.queue_readmitted_wake(&message_event, *recipient_id, &sender_label, &mut receipt);
            receipts.push(receipt);
        }
        let RoomFeedItem::Message { message_id, guest_recipient_ids, .. } = &message_event.item else { unreachable!() };
        let written_count = receipts.iter().filter(|r| matches!(r.status,
            RoomDeliveryStatus::Written | RoomDeliveryStatus::Woken)).count();
        let failures = room_delivery_failures(&receipts);
        // v3 W5: addressed guests are stored, never woken; they read by poll.
        let guest_receipts = guest_recipient_ids.iter().map(|guest_id| shared_types::RoomGuestReceipt {
            guest_id: *guest_id, status: "stored".into(), note: Some("the guest reads by poll; it is never woken".into()) }).collect();
        Ok(RoomDeliveryResult { room_id, message_id: *message_id, cursor: message_event.cursor,
            request_id, replayed: false, receipts, recipient_count: recipient_ids.len(), written_count, failures, guest_receipts })
    }

    /// Operator-only: no new message, body or expanded recipient list is accepted.
    pub fn retry_room_delivery(&self, request: RetryRoomDeliveryRequest) -> Result<RoomDeliveryResult> {
        self.retry_room_delivery_checked(request, None)
    }

    /// `check`: a managing pane's room_retry (A16), rechecked under the rooms
    /// lock where the retry is reserved.
    pub(super) fn retry_room_delivery_checked(&self, request: RetryRoomDeliveryRequest,
        check: Option<&member_lifecycle::ManageCheck<'_>>) -> Result<RoomDeliveryResult> {
        self.ensure_active()?;
        self.refresh_session_liveness();
        let (message_event, previous, _lease) = {
            let _publish = self.inner.room_event_publish.lock();
            self.recover_room_writer(request.room_id)?;
            let mut rooms = self.inner.rooms.lock();
            if let Some(check) = check {
                check.check(&rooms, &self.inner.slots.lock())?;
            }
            let room = rooms.get_mut(request.room_id).ok_or_else(|| anyhow!("unknown room id '{}'", request.room_id))?;
            let (event, previous) = room.retry_snapshot(request.message_id, request.recipient_id, request.expected_receipt)?;
            let Some(previous) = previous else { return Ok(room.delivery_result(&event, true)); };
            room.ensure_receipt_evidence_capacity(1)?;
            room.ensure_sequence_capacity(2)?;
            if room.in_flight_deliveries == usize::MAX { return Err(anyhow!("room delivery counter exhausted")); }
            room.in_flight_deliveries += 1;
            room.receipt_reservations += 1;
            (event, previous, RoomDeliveryLease { inner: self.inner.clone(), room_id: request.room_id, receipt_reservations: 1 })
        };
        // A failed/held attempt bound to an older run cannot silently reach its
        // replacement, even if that replacement currently appears ready.
        if let Some(run_id) = previous.run_id {
            let slots = self.inner.slots.lock();
            let slot = slots.get_by_id(request.recipient_id)
                .ok_or_else(|| anyhow!("retry recipient no longer exists"))?;
            if slot.run_id != Some(run_id) || Some(slot.generation) != previous.generation {
                return Err(anyhow!("recipient restarted since this wake; the stored message remains readable via room_read; this old wake cannot target the replacement run"));
            }
        }
        let RoomFeedItem::Message { sender, .. } = &message_event.item else { unreachable!() };
        let sender_label = match sender {
            RoomMessageSender::Operator {} => "operator".into(),
            RoomMessageSender::Session { session_id } => self.session_sender_label(request.room_id, *session_id),
            RoomMessageSender::Guest { label, .. } => guest_sender_label(label),
            RoomMessageSender::LocalProcess { label } => label.clone(),
            RoomMessageSender::Prim1 {} => "PRIM-1".into(),
        };
        {
            let slots = self.inner.slots.lock();
            let slot = slots.get_by_id(request.recipient_id).ok_or_else(|| anyhow!("retry recipient no longer exists"))?;
            if let Some(reason) = unsupported_room_wake(slot.definition.driver) { return Err(anyhow!(reason)); }
        }
        let planned = self.plan_room_wake(request.recipient_id, &message_event, &sender_label);
        if let Some(not_ready) = planned.as_ref().err().and_then(|error| error.downcast_ref::<RoomWakeNotReady>()) {
            // An explicit retry is never queued behind its own Held receipt.
            return Err(anyhow!("{not_ready}. The message stays stored; use Retry wake once the member is ready."));
        }
        let mut outcome = self.perform_room_wake(&message_event, request.recipient_id, planned,
            Some(request.expected_receipt), Some(&previous))?;
        if let Some(receipt) = outcome.as_mut() {
            self.queue_readmitted_wake(&message_event, request.recipient_id, &sender_label, receipt);
        }
        let mut result = self.inner.rooms.lock().get(request.room_id)
            .expect("in-flight room cannot be deleted").delivery_result(&message_event, outcome.is_none());
        if let Some(receipt) = outcome.filter(|r| !r.recorded) {
            // The committed message remains Stored even if its latest wake
            // outcome could not be synced. Do not hide a known partial write.
            if matches!(receipt.status, RoomDeliveryStatus::Written | RoomDeliveryStatus::Woken) {
                result.written_count += 1;
            }
            if let Some(current) = result.receipts.iter_mut().find(|r| r.recipient_id == receipt.recipient_id) {
                *current = receipt;
            }
            result.failures = room_delivery_failures(&result.receipts);
        }
        Ok(result)
    }

    /// Ready now, or a [`RoomWakeNotReady`] that keeps the message queued.
    /// Any other error is final for this attempt and is recorded as Held.
    fn plan_room_wake(&self, recipient_id: SessionId, message: &RoomFeedEvent,
        sender_label: &str) -> Result<RoomWakePlan> {
        self.check_fallback_message(message)?;
        self.check_recovery_message(message)?;
        let (title_busy, app_server) = {
            let slots = self.inner.slots.lock();
            let slot = slots.get_by_id(recipient_id).ok_or_else(|| anyhow!("room wake recipient disappeared"))?;
            if let Some(reason) = unsupported_room_wake(slot.definition.driver) { return Err(anyhow!(reason)); }
            if let Some(reason) = room_wake_busy(slot) { return Err(not_ready(reason)); }
            // v3 W4 S3: a real block. While the member's App Server waits on a
            // person (approval, question, form), no wake goes either way: the
            // person's prompt is the terminal's to answer, and nothing PRIM-1
            // sends may compete with it. It clears on that request's own
            // serverRequest/resolved.
            if let Some(reason) = person_request_hold(slot) { return Err(not_ready(reason)); }
            // v3 W4 S2: a pointer for an App Server member whose observer has
            // applied exactly its bound thread goes as one turn/start. The
            // screen-quiet wait and the title hold below protect a typed
            // paste; turn/start adds input to an active turn instead, so a
            // busy member is not held (9828cc3b point 5).
            let app_server = wakes_by_pointer(message) && app_server_route(slot);
            // v3 W2: an Idle read during a mid-turn repaint is not a settled
            // prompt (M1 re-run: a 0.3 s Idle mid-turn admitted a wake).
            if !app_server && slot.definition.driver == DriverKind::Codex
                && !slot.room_composer.output_quiet_for(*self.inner.room_wake_output_settle.lock()) {
                return Err(not_ready("the member's screen is still changing; the message waits until it settles"));
            }
            // v3 W2 #2: Codex's own title says this run is busy. A hint: it
            // can only make a wake wait, never admit one, and only for the cap
            // (Claude PRIM-1, d9613ce walk); past it the wake is typed, as A106
            // types to a working member, and its receipt says so.
            let cap = *self.inner.room_wake_title_busy_cap.lock();
            let title_busy = match (slot.definition.driver == DriverKind::Codex && !app_server)
                .then(|| slot.codex_work_state.title_busy()).flatten() {
                Some((state, busy_for)) if busy_for < cap =>
                    return Err(not_ready(format!("Codex's own title reads {}; the message waits until that changes \
                        (at most {} s)", state.label(), cap.as_secs()))),
                Some((state, _)) => Some(state.label().to_string()),
                None => None,
            };
            (title_busy, app_server)
        };
        // A message never overtakes one already waiting for this member; the
        // dispatcher's own head has nothing for this member ahead of it.
        if self.inner.room_wake_queue.lock().entries.iter()
            .take_while(|entry| !(entry.recipient_id == recipient_id
                && entry.message.room_id == message.room_id && entry.message.cursor == message.cursor))
            .any(|entry| entry.recipient_id == recipient_id) {
            return Err(not_ready("an earlier room message for this member is still waiting"));
        }
        // Blocked on a prompt, startup, unknown paste mode or a changed run:
        // every refusal here clears by itself or after the operator acts.
        let (target, behavior) = self.delivery_target_for_session_id(recipient_id)
            .map_err(|error| not_ready(room_wake_error(&error)))?;
        if target.input_gate.room_wake_pending.load(Ordering::SeqCst) { return Err(not_ready(ROOM_WAKE_PENDING)); }
        if target.input_gate.queued_operator_input_blocks_wake() {
            return Err(not_ready("operator typing for this member is still being written"));
        }
        let activity = target.input_gate.room_input_activity.try_lock()
            .ok_or_else(|| not_ready("input or another wake is active"))?;
        if !activity.quiet_for(ROOM_WAKE_TYPING_GRACE) {
            return Err(not_ready("the operator typed into this member a moment ago"));
        }
        if activity.draft_open() {
            return Err(not_ready("unsent text is in this member's input; the message waits until it is sent or cleared (Enter or Ctrl+C)"));
        }
        let input_epoch = activity.epoch;
        drop(activity);
        let RoomFeedItem::Message { content, sender, .. } = &message.item else { unreachable!() };
        // A guest's lines are quoted, so none can pass for a PRIM-1 header.
        let content = match sender {
            RoomMessageSender::Guest { .. } => guest_typed_content(content, sender_label),
            _ => content.clone(),
        };
        let route = RouteMessageRequest {
            from: queued_sender_label(sender_label, &message.timestamp, self.queued_note_after(), Utc::now()),
            to: target.session.clone(), scope: MessageScope::Room, content };
        validate_message_framing(&route.content, behavior)?;
        let ack_tracked = wakes_by_pointer(message);
        let unread = (ack_tracked || (room_fallbacks::message_origin(message).is_none()
            && recovery_automation::message_origin(message).is_none()))
            .then(|| self.inner.rooms.lock().get(message.room_id).map(|room| room.unread_for(recipient_id)))
            .flatten()
            // Eligible wake work only: a revoked guest's request is held
            // history, inspectable with its reason, never counted as new.
            .map(|unread| unread.into_iter().filter(|event| self.guest_sender_revoked(event).is_none()).collect::<Vec<_>>())
            // The message being woken always counts itself when it asked for an
            // ACK, also on an explicit Retry of its Held wake.
            .map(|mut unread| {
                if matches!(message.item, RoomFeedItem::Message { ack_requested: true, .. })
                    && !unread.iter().any(|event| event.cursor == message.cursor) {
                    unread.push(message.clone());
                    unread.sort_by_key(|event| event.cursor.sequence);
                }
                unread
            })
            .filter(|unread| unread.iter().any(|event| event.cursor == message.cursor));
        let pointer_through = unread.as_ref().and_then(|unread| unread.last()).map(|newest| newest.cursor.sequence);
        let payload = if let Some(origin) = room_fallbacks::message_origin(message).filter(|_| !ack_tracked) {
            let scheduled = DateTime::parse_from_rfc3339(&origin.scheduled_at)?;
            format!("[PRIM-1 fallback, scheduled {} UTC]\nScheduled reminder from room settings, not a new instruction from Victor. Act per the literal text under existing task and permission boundaries.\n{}", scheduled.format("%H:%M"), route.content)
        } else if recovery_automation::message_origin(message).is_some() && !ack_tracked {
            format!("[PRIM-1 recovery continuation]\n{}", route.content)
        } else if let Some(newest) = unread.as_ref().and_then(|unread| unread.last()) {
            let RoomFeedItem::Message { message_id: newest_id, .. } = &newest.item else { unreachable!() };
            room_pointer_text(unread.as_ref().map_or(1, Vec::len), message.room_id, *newest_id, None)
        } else { routed_message_payload(&route, behavior) };
        Ok(RoomWakePlan { target, behavior, payload, input_epoch, pointer_through, pointer_counted: Vec::new(), title_busy,
            app_server: app_server && pointer_through.is_some(),
            fallback: room_fallbacks::message_origin(message).cloned().map(|origin| (message.room_id, origin)),
            continuation: recovery_automation::message_origin(message).cloned().map(|origin| (message.room_id, origin)) })
    }

    /// False when the queue is full; the caller then records a Held receipt.
    fn enqueue_room_wake(&self, message: &RoomFeedEvent, recipient_id: SessionId, sender_label: &str,
        queued_at: Instant) -> bool {
        let start = {
            let mut queue = self.inner.room_wake_queue.lock();
            if !queue.entries.iter().any(|entry| entry.recipient_id == recipient_id
                && entry.message.room_id == message.room_id && entry.message.cursor == message.cursor) {
                if queue.entries.len() >= ROOM_WAKE_QUEUE_MAX || queue.entries.iter()
                    .filter(|entry| entry.recipient_id == recipient_id).count() >= ROOM_WAKE_QUEUE_MAX_PER_MEMBER {
                    return false;
                }
                queue.entries.push_back(QueuedRoomWake { pending_code: None, message: message.clone(), recipient_id,
                    sender_label: sender_label.into(), queued_at, last_reason: None });
            }
            !std::mem::replace(&mut queue.running, true)
        };
        if start {
            // Like the other background workers, hold the supervisor only
            // during a pass: a dropped supervisor ends the dispatcher instead
            // of keeping its room writers and PTYs alive for up to 12 hours.
            let weak = Arc::downgrade(&self.inner);
            if thread::Builder::new().name("prim1-room-wake-queue".into())
                .spawn(move || Self::run_room_wake_queue(weak)).is_err() {
                self.inner.room_wake_queue.lock().running = false;
            }
        }
        true
    }

    /// After a restart: every retained message whose recipient never got a
    /// wake receipt (it was queued) is queued again, oldest first. Messages
    /// Older ordinary messages stay stored for room_read. Typed recovery gets
    /// a fresh waiting budget; its exact native eligibility still gates writing.
    pub fn resume_room_wake_queue(&self) -> usize {
        let pending: Vec<(RoomFeedEvent, Vec<SessionId>)> = self.inner.rooms.lock().by_id.values()
            .flat_map(|room| room.unreceipted_messages()).collect();
        let mut queued = 0;
        for (message, recipients) in pending {
            let age = chrono::DateTime::parse_from_rfc3339(&message.timestamp).ok()
                .and_then(|at| (chrono::Utc::now() - at.with_timezone(&chrono::Utc)).to_std().ok())
                .unwrap_or(ROOM_WAKE_MAX_WAIT);
            let queued_at = if recovery_automation::message_origin(&message).is_some() {
                // Its interruption/UUID survives a long shutdown. Keep the
                // recorded timestamp; only this process's readiness wait resets.
                Instant::now()
            } else if room_fallbacks::message_origin(&message).is_some() {
                // Old scheduled intent reaches its exact-run/revision predicate
                // and records Held after restart instead of wedging the schedule.
                Instant::now().checked_sub(age.min(ROOM_WAKE_MAX_WAIT)).unwrap_or_else(Instant::now)
            } else {
                let Some(at) = Instant::now().checked_sub(age).filter(|_| age < ROOM_WAKE_MAX_WAIT) else { continue; };
                at
            };
            let RoomFeedItem::Message { sender, .. } = &message.item else { continue; };
            let sender_label = match sender {
                RoomMessageSender::Operator {} => "operator".into(),
                RoomMessageSender::Session { session_id } => self.session_sender_label(message.room_id, *session_id),
                RoomMessageSender::Guest { label, .. } => guest_sender_label(label),
                RoomMessageSender::LocalProcess { label } => label.clone(),
                RoomMessageSender::Prim1 {} => "PRIM-1".into(),
            };
            for recipient_id in recipients {
                queued += usize::from(self.enqueue_room_wake(&message, recipient_id, &sender_label, queued_at));
            }
        }
        queued
    }

    fn run_room_wake_queue(weak: Weak<SupervisorInner>) {
        loop {
            thread::sleep(ROOM_WAKE_POLL);
            let Some(inner) = weak.upgrade() else { return; };
            let handle = SupervisorHandle { inner };
            let heads = {
                let mut queue = handle.inner.room_wake_queue.lock();
                if queue.entries.is_empty() || handle.ensure_active().is_err() {
                    queue.entries.clear();
                    queue.running = false;
                    return;
                }
                let mut seen = HashSet::new();
                let heads: Vec<_> = queue.entries.iter().filter(|entry| seen.insert(entry.recipient_id))
                    .filter(|entry| !queue.in_flight.contains(&entry.recipient_id)).cloned().collect();
                for head in &heads { queue.in_flight.insert(head.recipient_id); }
                heads
            };
            // v3 W3 (Reviewer 80495230 a): each recipient's head is attempted on
            // its own thread, one at a time per recipient (order kept within
            // it), so a transport blocked on one member never stalls another's.
            for head in heads {
                let recipient_id = head.recipient_id;
                let worker = handle.clone();
                let spawned = thread::Builder::new().name("prim1-room-wake-recipient".into()).spawn(move || {
                    if worker.try_queued_room_wake(&head) {
                        worker.inner.room_wake_queue.lock().entries.retain(|entry| !(entry.recipient_id == head.recipient_id
                            && entry.message.room_id == head.message.room_id && entry.message.cursor == head.message.cursor));
                    }
                    worker.inner.room_wake_queue.lock().in_flight.remove(&head.recipient_id);
                });
                if let Err(error) = spawned {
                    // No thread: release the slot; the next pass retries it.
                    handle.emit(RuntimeEvent::SystemLog { level: LogLevel::Warn,
                        message: format!("room wake worker could not start: {error}"), timestamp: now_rfc3339() });
                    handle.inner.room_wake_queue.lock().in_flight.remove(&recipient_id);
                }
            }
        }
    }

    /// The room is gone, the member left, the message predates the member's
    /// current join (or feed epoch), or any receipt or fetch already exists.
    fn queued_room_wake_moot(rooms: &RoomState, entry: &QueuedRoomWake) -> bool {
        let message = &entry.message;
        let Some(room) = rooms.get(message.room_id) else { return true; };
        if message.cursor.epoch != room.feed_epoch || room.has_receipt(message, entry.recipient_id) { return true; }
        // v3 W3: a disposed-of message needs no wake.
        if let RoomFeedItem::Message { message_id, .. } = &message.item
            && room.ack_for(*message_id, entry.recipient_id).is_some() { return true; }
        // Typed automatic intent must record why it was suppressed. Its own
        // predicate refuses removed/rejoined members before any input is typed.
        if room_fallbacks::message_origin(message).is_some()
            || recovery_automation::message_origin(message).is_some() {
            return false;
        }
        !room.definition.member_ids.contains(&entry.recipient_id)
            || message.cursor.sequence <= room.join_floor_by_session.get(&entry.recipient_id).copied().unwrap_or(0)
    }

    /// True once this entry is finished: typed, held, read by the member, or moot.
    fn try_queued_room_wake(&self, entry: &QueuedRoomWake) -> bool {
        let message = &entry.message;
        // v3 W3: a member that left the room makes its wake moot, but never
        // silently: the message's waiting record says so and it stays stored.
        let left = self.inner.rooms.lock().get(message.room_id).is_some_and(|room|
            !room.definition.member_ids.contains(&entry.recipient_id)
            && matches!(message.item, RoomFeedItem::Message { ack_requested: true, .. }));
        if left {
            self.record_room_wake_waiting(message, entry.recipient_id,
                "the member left this room; the message stays stored here for the operator");
        }
        if Self::queued_room_wake_moot(&self.inner.rooms.lock(), entry) { return true; }
        // A revoked guest's message is held at once, even for a stopped member.
        let planned = match self.guest_sender_revoked(message) {
            Some(reason) => Err(anyhow!(reason)),
            None => match self.plan_room_wake(entry.recipient_id, message, &entry.sender_label) {
                Err(error) if error.is::<RoomWakeNotReady>() => {
                    if entry.queued_at.elapsed() < self.room_wake_max_wait() {
                        self.note_room_wake_waiting(message, entry.recipient_id, &error.to_string());
                        return false;
                    }
                    Err(anyhow!("The member was not ready for 12 hours ({error}). The message is stored: use Retry wake or ask it to run room_read."))
                }
                other => other,
            },
        };
        let lease = {
            let _publish = self.inner.room_event_publish.lock();
            if self.recover_room_writer(message.room_id).is_err() { return false; }
            let mut rooms = self.inner.rooms.lock();
            // Re-checked under the publish lock: a Retry wake or a read that
            // landed since planning ends this entry; it is never typed twice.
            if Self::queued_room_wake_moot(&rooms, entry) { return true; }
            let Some(room) = rooms.get_mut(message.room_id) else { return true; };
            if room.ensure_receipt_evidence_capacity(1).is_err() || room.ensure_sequence_capacity(2).is_err()
                || room.in_flight_deliveries == usize::MAX {
                return false;
            }
            room.in_flight_deliveries += 1;
            room.receipt_reservations += 1;
            RoomDeliveryLease { inner: self.inner.clone(), room_id: message.room_id, receipt_reservations: 1 }
        };
        let outcome = self.perform_room_wake(message, entry.recipient_id, planned, None, None);
        drop(lease);
        if let Err(error) = outcome {
            self.emit(RuntimeEvent::SystemLog { level: LogLevel::Error,
                message: format!("Queued room message for {} was not typed: {error}", entry.recipient_id),
                timestamp: now_rfc3339() });
        }
        // v3 W3: a re-admittable transient refusal keeps its queue turn.
        !self.inner.rooms.lock().get(message.room_id)
            .is_some_and(|room| room.readmit_pending_for(message, entry.recipient_id))
    }

    /// A16 (T-30): the audit log keeps why a queued wake waits, once when it
    /// is queued and again only when the reason changes (an unchanged
    /// recheck writes nothing). Identifiers only, never the message's text.
    fn note_room_wake_waiting(&self, message: &RoomFeedEvent, recipient_id: SessionId, reason: &str) {
        // Read before the queue lock: the two are never held together here.
        let recorded = self.inner.rooms.lock().get(message.room_id).and_then(|room| room.waiting_code(message, recipient_id));
        let (unchanged, changed, settled) = {
            let mut queue = self.inner.room_wake_queue.lock();
            let Some(entry) = queue.entries.iter_mut().find(|entry| entry.recipient_id == recipient_id
                && entry.message.room_id == message.room_id && entry.message.cursor == message.cursor) else { return };
            let settled = settle_waiting_code(recorded.as_deref(), &mut entry.pending_code,
                &wake_reason_code(reason), Instant::now(), WAITING_CODE_SETTLE);
            let unchanged = entry.last_reason.as_deref() == Some(reason);
            let changed = !unchanged && entry.last_reason.replace(reason.to_string()).is_some();
            (unchanged, changed, settled)
        };
        if unchanged {
            // The audit line is written once per reason; a held change still lands.
            if settled { self.record_room_wake_waiting(message, recipient_id, reason); }
            return;
        }
        let RoomFeedItem::Message { message_id, .. } = &message.item else { return };
        let (label, run) = self.inner.slots.lock().get_by_id(recipient_id)
            .map(|slot| (slot.definition.label.clone(), slot.run_id)).unwrap_or_default();
        let room = self.inner.rooms.lock().get(message.room_id).map(|room| room.definition.label.clone()).unwrap_or_default();
        self.emit(RuntimeEvent::SystemLog { level: LogLevel::Info,
            message: format!("room wake {}: message {message_id} for {label} ({recipient_id}, run {}) in room \"{room}\": {reason}",
                if changed { "still waiting, reason changed" } else { "queued" },
                run.map_or_else(|| "none".to_string(), |run| run.to_string())),
            timestamp: now_rfc3339() });
        if settled { self.record_room_wake_waiting(message, recipient_id, reason); }
    }

    fn queued_note_after(&self) -> Duration {
        #[cfg(test)]
        if let Some(after) = self.inner.room_wake_queue.lock().queued_note_after {
            return after;
        }
        QUEUED_NOTE_AFTER
    }

    #[cfg(test)]
    pub(super) fn set_queued_note_after_for_tests(&self, after: Duration) {
        self.inner.room_wake_queue.lock().queued_note_after = Some(after);
    }

    fn room_wake_max_wait(&self) -> Duration {
        #[cfg(test)]
        if let Some(wait) = self.inner.room_wake_queue.lock().max_wait {
            return wait;
        }
        ROOM_WAKE_MAX_WAIT
    }

    #[cfg(test)]
    pub(super) fn set_room_wake_max_wait_for_tests(&self, wait: Duration) {
        self.inner.room_wake_queue.lock().max_wait = Some(wait);
    }

    /// v3 W3 (D6): a withdrawn message's queued wakes are never attempted.
    pub(super) fn drop_queued_room_wakes_for(&self, room_id: RoomId, message_id: MessageId) {
        self.inner.room_wake_queue.lock().entries.retain(|entry| !(entry.message.room_id == room_id
            && matches!(entry.message.item, RoomFeedItem::Message { message_id: id, .. } if id == message_id)));
    }

    #[cfg(test)]
    pub(super) fn queued_room_wake_count_for_tests(&self, recipient_id: SessionId) -> usize {
        self.inner.room_wake_queue.lock().entries.iter()
            .filter(|entry| entry.recipient_id == recipient_id).count()
    }

    #[cfg(test)]
    pub(super) fn queued_room_wake_pass_for_tests(&self, recipient_id: SessionId) -> bool {
        let entry = self.inner.room_wake_queue.lock().entries.iter()
            .find(|entry| entry.recipient_id == recipient_id).cloned().expect("queued member intent");
        self.try_queued_room_wake(&entry)
    }

    /// Plans a wake, runs `between` (a revoke, a read), then performs the
    /// stale plan: the window the admission lock must close.
    #[cfg(test)]
    pub(super) fn wake_with_stale_plan_for_tests(&self, message: &RoomFeedEvent, recipient_id: SessionId,
        between: impl FnOnce()) -> Result<Option<RoomDeliveryReceipt>> {
        let planned = self.plan_room_wake(recipient_id, message, "test sender");
        between();
        self.perform_room_wake(message, recipient_id, planned, None, None)
    }

    #[cfg(test)]
    pub(super) fn hold_room_wake_dispatch_for_tests(&self, recipient_id: SessionId) {
        self.inner.room_wake_queue.lock().in_flight.insert(recipient_id);
    }

    /// Lets the running dispatcher take the member's queue again.
    #[cfg(test)]
    pub(super) fn release_room_wake_dispatch_for_tests(&self, recipient_id: SessionId) {
        self.inner.room_wake_queue.lock().in_flight.remove(&recipient_id);
    }

    /// v3 W3 (Reviewer b307957c): a reminder or continuation another request's
    /// pointer would count is admitted only where its own head admission
    /// would be; an ordinary request needs nothing more. Under the caller's
    /// rooms and slots locks.
    fn pointer_origin_admitted(&self, rooms: &RoomState, slots: &SessionRegistry, room_id: RoomId,
        event: &RoomFeedEvent) -> bool {
        if let Some(origin) = room_fallbacks::message_origin(event) {
            return matches!(event.item, RoomFeedItem::Message { ack_requested: true, .. })
                && self.fallback_still_valid(rooms, slots, room_id, origin).is_ok();
        }
        if let Some(origin) = recovery_automation::message_origin(event) {
            return self.continuation_still_valid(rooms, slots, room_id, origin).is_ok();
        }
        true
    }

    fn perform_room_wake(&self, message: &RoomFeedEvent, recipient_id: SessionId,
        mut planned: Result<RoomWakePlan>, expected: Option<shared_types::RoomFeedCursor>,
        previous: Option<&RoomDeliveryReceipt>) -> Result<Option<RoomDeliveryReceipt>> {
        let room_id = message.room_id;
        let RoomFeedItem::Message { message_id, .. } = &message.item else { unreachable!() };
        if planned.is_ok() && let Some(reason) = self.guest_sender_revoked(message) {
            planned = Err(anyhow!(reason));
        }
        let mut receipt = RoomDeliveryReceipt { app_server_turn: None, title_busy: None, already_read: false, recipient_id,
            status: RoomDeliveryStatus::Held, bytes_written: 0, error: None,
            run_id: previous.and_then(|r| r.run_id), generation: previous.and_then(|r| r.generation), recorded: true,
            queued_because: None, waiting: None, ack: None, hold: None };
        match &planned {
            Err(error) => {
                receipt.error = Some(room_wake_error(error));
                if receipt.run_id.is_none() {
                    let slots = self.inner.slots.lock();
                    if let Some(slot) = slots.get_by_id(recipient_id) {
                        if slot.running.is_some() && slot.run_id.is_some() {
                            receipt.run_id = slot.run_id;
                            receipt.generation = Some(slot.generation);
                        }
                    }
                }
            }
            Ok(delivery) => {
                if previous.is_some_and(|r| r.run_id.is_some()
                    && (r.run_id != Some(delivery.target.run_id) || r.generation != Some(delivery.target.generation))) {
                    return Err(anyhow!("recipient run changed before retry admission"));
                }
                receipt.status = RoomDeliveryStatus::Pending;
                receipt.run_id = Some(delivery.target.run_id);
                receipt.generation = Some(delivery.target.generation);
            }
        }
        let mut wake_claim = None;
        let attempt_sequence;
        let mut pending_cursor = None;
        let recipe_slot = self.submit_recipe_slot(recipient_id);
        let mut recipe_stored = false;
        let mut covered: Vec<MessageId> = Vec::new();
        let mut cover_incomplete = false;
        let mut cover_error: Option<String> = None;
        let pointer_admitted: Vec<RoomFeedEvent>;
        {
            let _publish = self.inner.room_event_publish.lock();
            let mut rooms = self.inner.rooms.lock();
            if planned.is_ok() && let Some(origin) = room_fallbacks::message_origin(message) {
                let slots = self.inner.slots.lock();
                if let Err(error) = self.fallback_still_valid(&rooms, &slots, room_id, origin) {
                    receipt.status = RoomDeliveryStatus::Held;
                    receipt.error = Some(bounded_room_delivery_error(&error.to_string()));
                    planned = Err(error);
                }
            }
            if planned.is_ok() && let Some(origin) = recovery_automation::message_origin(message) {
                let slots = self.inner.slots.lock();
                if let Err(error) = self.continuation_still_valid(&rooms, &slots, room_id, origin) {
                    receipt.status = RoomDeliveryStatus::Held;
                    receipt.error = Some(bounded_room_delivery_error(&error.to_string()));
                    planned = Err(error);
                }
            }
            // v3 W3 (D4; Reviewer b307957c): the one set this pointer counts
            // and covers, read under this lock: this recipient's unread
            // requests through the planned newest, each admitted on its own
            // terms as its own head would be. A revoked guest's request, or a
            // reminder or continuation that is no longer current, is neither
            // counted nor covered; it keeps its own turn to record why.
            pointer_admitted = match &planned {
                Ok(RoomWakePlan { pointer_through: Some(through), .. }) => {
                    let unread: Vec<_> = rooms.get(room_id).expect("in-flight room cannot be deleted")
                        .unread_for(recipient_id).into_iter()
                        .filter(|event| event.cursor.epoch == message.cursor.epoch && event.cursor.sequence <= *through
                            && event.cursor != message.cursor && self.guest_sender_revoked(event).is_none())
                        .collect();
                    let slots = self.inner.slots.lock();
                    let mut admitted: Vec<_> = unread.into_iter()
                        .filter(|event| self.pointer_origin_admitted(&rooms, &slots, room_id, event)).collect();
                    admitted.push(message.clone());
                    admitted.sort_by_key(|event| event.cursor.sequence);
                    admitted
                }
                _ => Vec::new(),
            };
            let room = rooms.get_mut(room_id).expect("in-flight room cannot be deleted");
            if let Some(expected) = expected {
                if room.retry_snapshot(*message_id, recipient_id, expected)?.1.is_none() {
                    return Ok(None);
                }
            } else if room.ack_for(*message_id, recipient_id).is_some() {
                // v3 W3 (Reviewer 998e10ed): disposed of after planning and
                // before Pending — nothing is admitted for it.
                return Ok(None);
            } else if room.has_receipt(message, recipient_id) {
                // Checked in the same locked block as the Pending append: a
                // Retry, read or covering pointer that committed after
                // planning wins, on the queued and the direct path alike
                // (Reviewer 4e12572e). A Retry is judged by its expected
                // cursor above instead.
                return Ok(None);
            } else if room.delivery_result(message, false).receipts.iter()
                .any(|r| r.recipient_id == recipient_id && r.status == RoomDeliveryStatus::Fetched) {
                return Ok(None); // Already fetched while this wake was being planned.
            }
            // A revoke commits under this same publish lock (publish, rooms,
            // then guests), so a guest wake planned before it is held here.
            if planned.is_ok() && let Some(reason) = self.guest_sender_revoked(message) {
                receipt.status = RoomDeliveryStatus::Held;
                receipt.error = Some(bounded_room_delivery_error(&reason));
                planned = Err(anyhow!(reason));
            }
            if let Ok(delivery) = &planned {
                wake_claim = RoomWakeClaim::try_acquire(&delivery.target.input_gate);
                if wake_claim.is_none() {
                    receipt.status = RoomDeliveryStatus::Held;
                    receipt.error = Some(ROOM_WAKE_PENDING.into());
                    planned = Err(anyhow!(ROOM_WAKE_PENDING));
                }
            }
            // Claim before sync/publication; two separately planned messages
            // cannot both become Pending and queue behind the same PTY writer.
            if let Some(claim) = wake_claim.as_mut() { claim.pending = true; }
            attempt_sequence = room.next_sequence;
            let outcome = room.append(room_receipt_item(*message_id, &receipt, planned.is_err()));
            let mut cover_events = Vec::new();
            if let Ok(event) = &outcome {
                if let Some(claim) = &wake_claim {
                    pending_cursor = Some(event.cursor);
                    *claim.gate.room_wake_claim.lock() = Some((*message_id, event.cursor));
                }
                // v3 W3 (D4): record the covering set BEFORE anything is typed:
                // each covered request gets its own Pending naming this pointer.
                // It is the admitted set the pointer counts, not only the queue
                // entries: a request posted before planning whose queue entry is
                // not in yet is counted and so must be covered, or its own entry
                // would type a second pointer (PC3 full-suite double type, 48a0e1c).
                if planned.is_ok() {
                    for candidate in pointer_admitted.iter().filter(|event| event.cursor != message.cursor) {
                        let RoomFeedItem::Message { message_id: covered_id, .. } = &candidate.item else { continue };
                        if room.has_receipt(candidate, recipient_id) || room.ack_for(*covered_id, recipient_id).is_some() {
                            continue;
                        }
                        // A request outside the recipient's current membership is
                        // never covered and keeps its own queue turn to record why
                        // (Reviewer 5abde321); revoked guests and stale origins are
                        // already out of the admitted set.
                        if !room.definition.member_ids.contains(&recipient_id)
                            || candidate.cursor.sequence
                                <= room.join_floor_by_session.get(&recipient_id).copied().unwrap_or(0) {
                            continue;
                        }
                        match room.append(room_cover_receipt_item(*covered_id, &receipt, *message_id, false)) {
                            Ok(event) => { covered.push(*covered_id); cover_events.push(event); }
                            Err(error) => {
                                cover_incomplete = true;
                                cover_error = Some(format!("Pointer coverage for {recipient_id} was not fully recorded: {error}; nothing is typed"));
                                break;
                            }
                        }
                    }
                }
                // The count and newest id were planned before this lock: derive
                // them again from the set admitted here, so a request revoked or
                // no longer current in between is not counted (Reviewers
                // 371647cb, b307957c).
                if let Ok(delivery) = planned.as_mut() && delivery.pointer_through.is_some() {
                    delivery.pointer_counted = pointer_admitted.iter().map(|event| event.cursor.sequence).collect();
                    if let Some(RoomFeedItem::Message { message_id: newest, .. }) = pointer_admitted.last().map(|event| &event.item) {
                        // The attempt token is typed only where a recipe is
                        // recorded for it (a bound Claude or Codex member).
                        delivery.payload = room_pointer_text(pointer_admitted.len(), room_id, *newest,
                            recipe_slot.as_ref().and(pending_cursor));
                    }
                }
                // v3 W3b (Reviewers 49bfe72d, 777b2bcc): the durable recipe of
                // this attempt, before any byte: the exact typed text and the
                // conversation it goes into. If it cannot be stored, nothing is
                // typed (Reviewer 7f071b01), as for an incomplete covering set.
                if !cover_incomplete && let Ok(delivery) = &planned && delivery.pointer_through.is_some()
                    && let (Some(attempt), Some((driver, conversation_id, working_dir))) = (pending_cursor, &recipe_slot) {
                    match room.append(RoomFeedItem::SubmitAttempt { message_id: *message_id, recipient_id, attempt,
                        typed: delivery.payload.clone(), driver: *driver, conversation_id: conversation_id.clone(),
                        working_dir: working_dir.clone(), run_id: Some(delivery.target.run_id), covered: covered.clone() }) {
                        Ok(event) => { recipe_stored = true; cover_events.push(event); }
                        Err(error) => {
                            cover_incomplete = true;
                            cover_error = Some(format!(
                                "The submission recipe for {recipient_id} was not stored: {error}; nothing is typed"));
                        }
                    }
                }
            } else {
                // Transport has not started, including when our own Pending
                // write is torn. Full replay suppresses this Held repair if a
                // complete Pending (or any later receipt) survived the failure.
                let mut refused = receipt.clone();
                refused.status = RoomDeliveryStatus::Held;
                refused.error = Some("Wake not attempted because its receipt could not be stored. Retry explicitly after history recovery where supported.".into());
                room.retain_receipt_evidence(message, attempt_sequence, None,
                    room_receipt_item(*message_id, &refused, true));
            }
            drop(rooms);
            self.publish_room_receipt(room_id, outcome, &mut receipt);
            for feed_event in cover_events { self.emit(RuntimeEvent::RoomFeedEvent { feed_event }); }
            if let Some(message) = cover_error.take() {
                self.emit(RuntimeEvent::SystemLog { level: LogLevel::Error, message, timestamp: now_rfc3339() });
            }
        }
        let Ok(delivery) = planned else { return Ok(Some(receipt)); };
        if !receipt.recorded {
            // Nothing crossed the transport boundary. Release this run gate;
            // any complete uncertain Pending still forbids retry of that intent.
            wake_claim.as_mut().expect("planned wake owns its run claim").pending = false;
            // The caller knows no transport was attempted; a complete uncertain
            // Pending on disk still stays non-repeatable after replay.
            receipt.status = RoomDeliveryStatus::Held;
            receipt.error = Some(format!("Wake not attempted: its claim was not confirmed. {}",
                receipt.error.as_deref().unwrap_or("The original message remains stored.")));
            return Ok(Some(receipt));
        }
        let mut known_terminal = true;
        // v3 W3: what confirming this attempt's submission needs, taken before
        // anything is typed (Supervisor 1086baa2).
        let recipe = recipe_slot.as_ref().filter(|_| recipe_stored);
        let confirm = (!cover_incomplete).then(|| self.submit_confirm_start(recipient_id, message, &delivery, recipe)).flatten();
        let flags = Arc::new(WakeCheckFlags::default());
        // v3 W4 S2: the App Server route needs this attempt's stored recipe:
        // its thread and the attempt token the request is bound to.
        let route = recipe.zip(pending_cursor).map(|((_, conversation_id, _), attempt)| (conversation_id.clone(), attempt));
        let mut app_server_unknown = false;
        let mut readmit_events: Vec<RoomFeedEvent> = Vec::new();
        let mut readmit_error: Option<String> = None;
        let zero_write_confirmed = if cover_incomplete {
            // v3 W3: never type a pointer over a partially recorded covering
            // set. Nothing was written: a Failed known zero (the completion
            // form of a refused wake), retryable.
            receipt.status = RoomDeliveryStatus::Failed;
            receipt.error = Some("pointer not typed: its covering set or submission recipe could not be \
                fully recorded; the messages stay stored".into());
            true
        } else { match self.write_room_wake(message, &delivery, route, &flags) {
            Ok(WakeWrite::Typed(bytes_written)) => {
                receipt.status = RoomDeliveryStatus::Written;
                receipt.bytes_written = bytes_written;
                // v3 W2 #2: typed past the cap while the title still read busy.
                receipt.title_busy = delivery.title_busy.clone();
                false
            }
            // v3 W4 S2: the internal status stays Written (c01d84f8); the turn
            // id is what Raw shows, and nothing was typed into the pane.
            Ok(WakeWrite::Accepted(turn_id)) => {
                receipt.status = RoomDeliveryStatus::Written;
                receipt.app_server_turn = Some(turn_id);
                false
            }
            Err(error) => {
                receipt.status = RoomDeliveryStatus::Failed;
                let progress = error.downcast_ref::<PtyWriteError>();
                // A refusal by the App Server is a known outcome with no
                // zero-write inference (Reviewer 617c8f1c); no answer is not.
                known_terminal = progress.is_some() || error.is::<AppServerRefused>();
                app_server_unknown = error.is::<AppServerUnknown>();
                receipt.bytes_written = progress.map(PtyWriteError::bytes_written).unwrap_or(0);
                receipt.error = Some(room_wake_error(&error));
                progress.is_some_and(|progress| progress.bytes_written() == 0)
            }
        } };
        let transient = std::cell::Cell::new(flags.transient.load(Ordering::Acquire));
        let already_read = std::cell::Cell::new(flags.already_read.load(Ordering::Acquire));
        let count_changed = std::cell::Cell::new(flags.count_changed.load(Ordering::Acquire));
        let route_changed = flags.route_changed.load(Ordering::Acquire);
        let person_request = flags.person_request.load(Ordering::Acquire);
        let _publish = self.inner.room_event_publish.lock();
        let (outcome, cover_outcomes, attempt_cursors) = {
            let mut rooms = self.inner.rooms.lock();
            let room = rooms.get_mut(room_id).expect("in-flight room cannot be deleted");
            // v3 W3 (Claude PRIM-1, 26a7f92 walk): a pointer refused because
            // everything it counts was read is marked so; a typed one whose
            // counted messages were all read before its submit says so too.
            receipt.already_read = already_read.get() || (receipt.status == RoomDeliveryStatus::Written
                && delivery.pointer_through.is_some() && !room.unread_for(recipient_id).iter()
                    .any(|event| event.cursor.epoch == message.cursor.epoch
                        && delivery.pointer_counted.contains(&event.cursor.sequence)));
            // A fetch can race the admitted write. Do not advertise another
            // retry proof after it, even when the older Fetched event later
            // falls outside the renderer's bounded feed window.
            let retry_proof = zero_write_confirmed && !receipt.already_read
                && room.needs_fetched_receipt(*message_id, recipient_id);
            // v3 W3: a confirmed-zero transient refusal is re-admitted while its
            // budget lasts; the flag is on the completion itself (Reviewer
            // b34f15c2). Once spent, the reason says so and nothing waits for
            // a quiet that will not flush it (Reviewer 75ffe61f).
            let transient_zero = transient.get() && zero_write_confirmed && receipt.bytes_written == 0;
            let readmit_head = transient_zero
                && room.readmit_attempts(*message_id, recipient_id) < crate::rooms::ROOM_WAKE_READMIT_MAX;
            if transient_zero && !readmit_head {
                receipt.error = Some(ROOM_WAKE_READMIT_SPENT.into());
            }
            let mut item = room_receipt_item(*message_id, &receipt, retry_proof);
            set_readmit(&mut item, readmit_head);
            let outcome = room.append_wake_completion(item.clone());
            if outcome.is_err() && known_terminal {
                room.retain_receipt_evidence(message, attempt_sequence, pending_cursor, item);
            }
            // v3 W3 (D4): each covered request's own outcome of the same write.
            // Its bytes stay 0: the head's receipt carries what was typed.
            let mut readmit_group = vec![(*message_id, readmit_head && outcome.is_ok())];
            let cover_outcomes: Vec<_> = covered.iter().map(|covered_id| {
                let mut cover = receipt.clone();
                cover.bytes_written = 0;
                let eligible = transient_zero
                    && room.readmit_attempts(*covered_id, recipient_id) < crate::rooms::ROOM_WAKE_READMIT_MAX;
                if transient_zero {
                    let refusal = if count_changed.get() { ROOM_WAKE_COUNT_CHANGED_REFUSAL }
                        else if person_request { ROOM_WAKE_PERSON_REQUEST_REFUSAL }
                        else if route_changed { ROOM_WAKE_ROUTE_CHANGED_REFUSAL } else { ROOM_WAKE_OUTPUT_CHANGED_REFUSAL };
                    cover.error = Some(if eligible { refusal.into() } else { ROOM_WAKE_READMIT_SPENT.into() });
                }
                let mut item = room_cover_receipt_item(*covered_id, &cover, *message_id, zero_write_confirmed);
                set_readmit(&mut item, eligible);
                let appended = room.append_wake_completion(item);
                readmit_group.push((*covered_id, eligible && appended.is_ok()));
                appended
            }).collect();
            // v3 W3 (Supervisor 43891c0e, 9aaf9e8b; Reviewer 4712da1b): a
            // transient refusal with a confirmed zero write is re-admitted, the
            // whole group: a durable Waiting{output_settling} right after each
            // Failed (head and every covered request) is what re-admits it, on
            // replay too. Partial, ambiguous or restricted outcomes never are.
            // Visibility only: the completion's flag is what re-admits it.
            for (id, readmitted) in readmit_group {
                if !readmitted { continue; }
                let waiting = if count_changed.get() { ROOM_WAKE_COUNT_CHANGED }
                    else if person_request { ROOM_WAKE_PERSON_REQUEST }
                    else if route_changed { ROOM_WAKE_ROUTE_CHANGED } else { ROOM_WAKE_OUTPUT_CHANGED };
                match room.append(RoomFeedItem::Waiting { message_id: id, recipient_id,
                    reason_code: wake_reason_code(waiting), reason: waiting.into(), attempt: None }) {
                    Ok(event) => readmit_events.push(event),
                    Err(error) => readmit_error = Some(format!(
                        "waiting record for {recipient_id} was not stored: {error}; the wake is still re-admitted")),
                }
            }
            // v3 W3: this attempt's own receipts, which its confirmation is bound to.
            let attempt_cursors: HashMap<MessageId, shared_types::RoomFeedCursor> = std::iter::once((*message_id, outcome.as_ref().ok().map(|event| event.cursor)))
                .chain(covered.iter().copied().zip(cover_outcomes.iter().map(|outcome| outcome.as_ref().ok().map(|event| event.cursor))))
                .filter_map(|(id, cursor)| cursor.map(|cursor| (id, cursor))).collect();
            // v3 W4 S2: a turn/start sent (or possibly sent) and never
            // answered is unknown for the head and each covered request,
            // bound to this attempt, and never sent again (9828cc3b point 3).
            // v3 W4 S2 (Reviewer be2267e4): with record_submit_waiting's
            // guards, under this same append lock: a request the member read
            // or ACKed, whose attempt input was observed, or whose latest
            // receipt is no longer this attempt's completion is settled, and
            // no uncertainty is recorded over it. Its RPC outcome stays unknown
            // on the completion itself; the member's disposition is not RPC
            // admission.
            if app_server_unknown && outcome.is_ok() {
                let requests = std::iter::once(message).chain(pointer_admitted.iter().filter(|candidate|
                    matches!(&candidate.item, RoomFeedItem::Message { message_id, .. } if covered.contains(message_id))));
                for event in requests {
                    let RoomFeedItem::Message { message_id: id, .. } = &event.item else { continue };
                    if room.read_or_acked(event, recipient_id)
                        || pending_cursor.is_some_and(|attempt| room.input_observed_for(event, recipient_id, attempt))
                        || attempt_cursors.get(id).is_none_or(|completion|
                            room.latest_receipt_cursor(event, recipient_id) != Some(*completion)) {
                        continue;
                    }
                    match room.append(RoomFeedItem::Waiting { message_id: *id, recipient_id,
                        reason_code: "submit_unknown".into(), reason: APP_SERVER_SUBMIT_UNKNOWN.into(),
                        attempt: pending_cursor }) {
                        Ok(event) => readmit_events.push(event),
                        Err(error) => readmit_error = Some(format!(
                            "submission record for {recipient_id} was not stored: {error}; nothing is sent again")),
                    }
                }
            }
            (outcome, cover_outcomes, attempt_cursors)
        };
        if outcome.is_ok() {
            wake_claim.as_mut().expect("an admitted wake owns its current run claim").pending = false;
        }
        self.publish_room_receipt(room_id, outcome, &mut receipt);
        for cover_outcome in cover_outcomes {
            let mut cover = receipt.clone();
            self.publish_room_receipt(room_id, cover_outcome, &mut cover);
        }
        for feed_event in readmit_events { self.emit(RuntimeEvent::RoomFeedEvent { feed_event }); }
        if let Some(message) = readmit_error {
            self.emit(RuntimeEvent::SystemLog { level: LogLevel::Error, message, timestamp: now_rfc3339() });
        }
        // v3 W3: Written means the bytes and CR reached the terminal, never
        // that the harness submitted them; that is confirmed separately.
        if let Some(mut confirm) = confirm.filter(|_| receipt.status == RoomDeliveryStatus::Written && receipt.recorded) {
            confirm.messages.extend(pointer_admitted.iter().filter(|candidate| matches!(&candidate.item,
                RoomFeedItem::Message { message_id, .. } if covered.contains(message_id))).cloned());
            confirm.attempts = attempt_cursors;
            confirm.attempt_token = pending_cursor;
            confirm.via_app_server = receipt.app_server_turn.is_some();
            self.spawn_submit_confirmation(confirm);
        }
        Ok(Some(receipt))
    }

    /// The early check reads exactly the conversation the stored recipe names,
    /// never the slot separately (Reviewer ed940924). Without a recipe the
    /// member is unbound or unresolved, so its submission can only be Unknown.
    fn submit_confirm_start(&self, recipient_id: SessionId, message: &RoomFeedEvent, delivery: &RoomWakePlan,
        recipe: Option<&(DriverKind, String, String)>) -> Option<SubmitConfirm> {
        let window = (*self.inner.submit_confirm_window.lock())?;
        delivery.pointer_through?;
        let (driver, harness_id, working_dir) = match recipe {
            Some((driver, conversation_id, working_dir)) => (*driver, Some(conversation_id.clone()), working_dir.clone()),
            None => {
                let slots = self.inner.slots.lock();
                let slot = slots.get_by_id(recipient_id)?;
                (slot.definition.driver, None, slot.definition.working_dir.clone())
            }
        };
        if !matches!(driver, DriverKind::Claude | DriverKind::Codex) {
            return None;
        }
        Some(SubmitConfirm { driver, identity_unresolved: harness_id.is_none(), harness_id, working_dir,
            since: Utc::now(), payload: delivery.payload.clone(), room_id: message.room_id,
            recipient_id, messages: vec![message.clone()], attempts: HashMap::new(), attempt_token: None, window,
            via_app_server: false })
    }

    /// Looks for the submission off the request path until the window ends.
    /// Only an exact user record in the member's own conversation confirms
    /// the pointer; a read or ACK settles its own request only (Reviewer
    /// c49038b0). Nothing is typed again on the absence of evidence
    /// (Supervisors 4aa88095, 50fb64e7, 0d81f25d).
    fn spawn_submit_confirmation(&self, confirm: SubmitConfirm) {
        let weak = Arc::downgrade(&self.inner);
        let spawned = thread::Builder::new().name("prim1-room-submit-confirm".into()).spawn(move || {
            let deadline = Instant::now() + confirm.window;
            loop {
                {
                    let Some(inner) = weak.upgrade() else { return };
                    let handle = SupervisorHandle { inner };
                    if let submit_confirm::SubmitEvidence::Confirmed(locator) = handle.submission_evidence(&confirm) {
                        handle.record_input_observed(&confirm, &locator);
                        return;
                    }
                    if handle.unsettled_requests(&confirm).is_empty() {
                        return;
                    }
                }
                if Instant::now() < deadline {
                    thread::sleep(SUBMIT_CONFIRM_POLL);
                    continue;
                }
                let Some(inner) = weak.upgrade() else { return };
                (SupervisorHandle { inner }).settle_unconfirmed_submission(&confirm);
                return;
            }
        });
        if let Err(error) = spawned {
            self.emit(RuntimeEvent::SystemLog { level: LogLevel::Warn,
                message: format!("room wake submission check could not start: {error}"), timestamp: now_rfc3339() });
        }
    }

    /// The pointer's own submission, from the member's own bound record.
    fn submission_evidence(&self, confirm: &SubmitConfirm) -> submit_confirm::SubmitEvidence {
        let home = self.inner.identity_tracker.claude_home();
        let payload = confirm.payload.trim();
        match submit_confirm::record_path(confirm.driver, home.as_deref(), confirm.harness_id.as_deref(),
            confirm.identity_unresolved, &confirm.working_dir) {
            Ok(path) => submit_confirm::submitted_since(confirm.driver, &path, confirm.since, &|text| text == payload),
            Err(reason) => submit_confirm::SubmitEvidence::Unknown(reason),
        }
    }

    /// v3 W3b: one InputObserved per request of this attempt whose own Written
    /// receipt is still its latest — input observed, never acceptance.
    fn record_input_observed(&self, confirm: &SubmitConfirm, locator: &str) {
        let source = match confirm.driver { DriverKind::Codex => "codex_rollout", _ => "claude_transcript" };
        let Some(token) = confirm.attempt_token else { return };
        let mut events = Vec::new();
        {
            let _publish = self.inner.room_event_publish.lock();
            let mut rooms = self.inner.rooms.lock();
            let Some(room) = rooms.get_mut(confirm.room_id) else { return };
            for event in &confirm.messages {
                let RoomFeedItem::Message { message_id, .. } = &event.item else { continue };
                let Some(written) = confirm.attempts.get(message_id).copied() else { continue };
                // Once per attempt: the reconciler may have recorded it first.
                if room.latest_receipt_cursor(event, confirm.recipient_id) != Some(written)
                    || room.input_observed_for(event, confirm.recipient_id, token) {
                    continue;
                }
                if let Ok(feed_event) = room.append(RoomFeedItem::InputObserved { message_id: *message_id,
                    recipient_id: confirm.recipient_id, attempt: token, source: source.into(), locator: locator.into() }) {
                    events.push(feed_event);
                }
            }
        }
        for feed_event in events { self.emit(RuntimeEvent::RoomFeedEvent { feed_event }); }
    }

    /// v3 W3b (Reviewers 2efa5499, b234bfc6): ONE lifecycle-owned reconciler.
    /// It wakes on a capacity-one coalesced signal or every
    /// SUBMIT_RECONCILE_EVERY, holds the supervisor only weakly, reads a
    /// bounded fair batch of uncertain attempts outside every lock, and
    /// appends InputObserved only after revalidating shutdown and the attempt.
    /// It is never awaited from the identity or liveness ticks; on shutdown it
    /// stops without waiting on a read in progress.
    pub(super) fn start_submit_reconciler(&self) {
        let (signal, wake) = std::sync::mpsc::sync_channel::<()>(1);
        *self.inner.submit_reconcile_signal.lock() = Some(signal);
        let weak = Arc::downgrade(&self.inner);
        let spawned = thread::Builder::new().name("prim1-room-submit-reconcile".into()).spawn(move || {
            let mut rotation = 0usize;
            loop {
                match wake.recv_timeout(SUBMIT_RECONCILE_EVERY) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                }
                let batch = {
                    let Some(inner) = weak.upgrade() else { return };
                    if inner.shutdown_started.load(Ordering::Acquire) { return; }
                    (SupervisorHandle { inner }).submission_reconcile_batch(&mut rotation)
                };
                for item in batch {
                    // File reads with no lock and no strong supervisor held.
                    let submit_confirm::SubmitEvidence::Confirmed(locator) = submit_confirm::submitted_since(
                        item.driver, &item.path, item.since, &|text| text == item.typed.trim()) else { continue };
                    let Some(inner) = weak.upgrade() else { return };
                    if inner.shutdown_started.load(Ordering::Acquire) { return; }
                    (SupervisorHandle { inner }).append_reconciled_observation(&item, &locator);
                }
                // v3 J4 (Claude PRIM-1): each running Codex member's own rollout, read
                // here off locks; identity and liveness stay free of file I/O.
                let targets = {
                    let Some(inner) = weak.upgrade() else { return };
                    if inner.shutdown_started.load(Ordering::Acquire) { return; }
                    (SupervisorHandle { inner }).provider_scan_targets()
                };
                for (session_id, run_id, conversation, working_dir, since) in targets {
                    let Ok(path) = submit_confirm::record_path(DriverKind::Codex, None, Some(&conversation), false,
                        &working_dir) else { continue };
                    let (turn, open) = submit_confirm::scan_rollout_turns(&path, since);
                    let Some(inner) = weak.upgrade() else { return };
                    if inner.shutdown_started.load(Ordering::Acquire) { return; }
                    (SupervisorHandle { inner }).apply_rollout_scan(session_id, run_id, &conversation, turn, open);
                }
            }
        });
        if let Err(error) = spawned {
            self.emit(RuntimeEvent::SystemLog { level: LogLevel::Warn,
                message: format!("room wake submission reconciler could not start: {error}"), timestamp: now_rfc3339() });
        }
    }

    /// One reconciler pass, synchronously, for tests.
    #[cfg(test)]
    pub(super) fn reconcile_submissions_once_for_tests(&self) {
        let mut rotation = 0;
        for item in self.submission_reconcile_batch(&mut rotation) {
            if let submit_confirm::SubmitEvidence::Confirmed(locator) = submit_confirm::submitted_since(
                item.driver, &item.path, item.since, &|text| text == item.typed.trim()) {
                self.append_reconciled_observation(&item, &locator);
            }
        }
    }

    /// v3 W3b (Claude PRIM-1 f0899a06): once at startup, before any wake of
    /// this process, an attempt the previous process wrote but never settled
    /// gets one `submit_unknown` bound to its token, so Raw shows the truth
    /// rather than a bare Written. Never a retype; idempotent, since the next
    /// startup finds the code. Cleared only by that attempt's InputObserved,
    /// a read or an ACK.
    pub(super) fn record_restart_unknown_submissions(&self) {
        let mut events = Vec::new();
        {
            let _publish = self.inner.room_event_publish.lock();
            let mut rooms = self.inner.rooms.lock();
            for room in rooms.by_id.values_mut() {
                for (message_id, recipient_id, attempt) in room.unsettled_written_attempts() {
                    match room.append(RoomFeedItem::Waiting { message_id, recipient_id,
                        reason_code: "submit_unknown".into(),
                        reason: "PRIM-1 restarted before the submission could be confirmed; nothing was typed again".into(),
                        attempt: Some(attempt) }) {
                        Ok(event) => events.push(event),
                        Err(error) => eprintln!("restart submission state was not recorded: {error:#}"),
                    }
                }
            }
        }
        for feed_event in events { self.emit(RuntimeEvent::RoomFeedEvent { feed_event }); }
    }

    /// Wakes the reconciler soon; a pending wake already covers this one.
    fn signal_submit_reconcile(&self) {
        if let Some(signal) = self.inner.submit_reconcile_signal.lock().as_ref() {
            let _ = signal.try_send(());
        }
    }

    /// A bounded batch, rotating fairly across the open attempts, each resolved
    /// from its own durable recipe — never from the member's current slot
    /// (Reviewers 49bfe72d, 02655ed3). Unresolvable records are skipped.
    fn submission_reconcile_batch(&self, rotation: &mut usize) -> Vec<ReconcileItem> {
        let open: Vec<_> = self.inner.rooms.lock().by_id.iter()
            .flat_map(|(room_id, room)| room.open_submissions().into_iter().map(move |open| (*room_id, open))).collect();
        if open.is_empty() { return Vec::new(); }
        let start = *rotation % open.len();
        *rotation = rotation.wrapping_add(SUBMIT_RECONCILE_BATCH);
        let home = self.inner.identity_tracker.claude_home();
        open.iter().cycle().skip(start).take(open.len().min(SUBMIT_RECONCILE_BATCH)).cloned()
            .filter_map(|(room_id, (event, recipient, recipe, written))| {
                let path = submit_confirm::record_path(recipe.driver, home.as_deref(), Some(&recipe.conversation_id),
                    false, &recipe.working_dir).ok()?;
                let since = chrono::DateTime::parse_from_rfc3339(&recipe.started_at).ok()?.with_timezone(&Utc);
                Some(ReconcileItem { room_id, event, recipient_id: recipient, attempt: recipe.attempt, written,
                    typed: recipe.typed, driver: recipe.driver, path, since,
                    source: match recipe.driver { DriverKind::Codex => "codex_rollout", _ => "claude_transcript" } })
            }).collect()
    }

    /// Appends InputObserved only while that attempt is still open with the
    /// same latest receipt it was read against.
    fn append_reconciled_observation(&self, item: &ReconcileItem, locator: &str) {
        let RoomFeedItem::Message { message_id, .. } = &item.event.item else { return };
        let appended = {
            let _publish = self.inner.room_event_publish.lock();
            let mut rooms = self.inner.rooms.lock();
            let Some(room) = rooms.get_mut(item.room_id) else { return };
            if !room.submission_open(&item.event, item.recipient_id, item.attempt, item.written)
                || room.input_observed_for(&item.event, item.recipient_id, item.attempt) { return; }
            room.append(RoomFeedItem::InputObserved { message_id: *message_id, recipient_id: item.recipient_id,
                attempt: item.attempt, source: item.source.into(), locator: locator.into() })
        };
        if let Ok(feed_event) = appended { self.emit(RuntimeEvent::RoomFeedEvent { feed_event }); }
    }

    /// The recipe parts a pointer attempt records: only a bound Claude or
    /// Codex conversation qualifies.
    fn submit_recipe_slot(&self, recipient_id: SessionId) -> Option<(DriverKind, String, String)> {
        let slots = self.inner.slots.lock();
        let slot = slots.get_by_id(recipient_id)?;
        if !matches!(slot.definition.driver, DriverKind::Claude | DriverKind::Codex) || slot.recovery.is_some() {
            return None;
        }
        Some((slot.definition.driver, slot.harness_session_id.clone()?, slot.definition.working_dir.clone()))
    }

    /// The covered requests this recipient has neither read nor disposed of.
    fn unsettled_requests(&self, confirm: &SubmitConfirm) -> Vec<RoomFeedEvent> {
        self.inner.rooms.lock().get(confirm.room_id).map(|room| confirm.messages.iter()
            .filter(|event| !room.read_or_acked(event, confirm.recipient_id)).cloned().collect()).unwrap_or_default()
    }

    /// The window ended without positive evidence: each unsettled request
    /// keeps a visible `submit_unconfirmed` (record readable, no match) or
    /// `submit_unknown` record and is never typed again, not even Enter
    /// (Supervisors 4aa88095, 50fb64e7, 2af36863). Recovery is the member's
    /// own next room_read and the reminders.
    fn settle_unconfirmed_submission(&self, confirm: &SubmitConfirm) {
        // v3 W4 S2: an App Server attempt was accepted into a turn, not typed.
        let (done, again) = if confirm.via_app_server {
            ("The Codex App Server accepted the pointer", "sent")
        } else {
            ("The pointer was typed", "typed")
        };
        let (code, reason) = match self.submission_evidence(confirm) {
            submit_confirm::SubmitEvidence::Confirmed(_) => return,
            submit_confirm::SubmitEvidence::Unknown(why) => ("submit_unknown", format!(
                "{done}, but whether the member submitted it cannot be confirmed ({why}). \
                 It is not {again} again: the member reads it with its next room_read, or prompt it.")),
            submit_confirm::SubmitEvidence::NotFound => ("submit_unconfirmed", format!("{done}, but the \
                member's own record does not show it submitted. It is not {again} again: the member reads it with \
                its next room_read, or prompt it.")),
        };
        let unsettled = self.unsettled_requests(confirm);
        if !unsettled.is_empty() {
            self.record_submit_waiting(confirm, &unsettled, code, &reason);
        }
    }

    /// Appends one Waiting record per request.
    fn record_submit_waiting(&self, confirm: &SubmitConfirm, requests: &[RoomFeedEvent], code: &str, reason: &str) {
        #[cfg(test)]
        if let Some(hook) = self.inner.submit_waiting_before_append.lock().take() { hook(); }
        let mut events = Vec::new();
        {
            let _publish = self.inner.room_event_publish.lock();
            let mut rooms = self.inner.rooms.lock();
            let Some(room) = rooms.get_mut(confirm.room_id) else { return };
            for event in requests {
                let RoomFeedItem::Message { message_id, .. } = &event.item else { continue };
                // Rechecked under the append lock: a read or ACK that landed
                // since the list was taken settles the request (Reviewer
                // 621948fa), and a newer attempt or run makes this result
                // strictly historical (Reviewer a5d87af3).
                if room.read_or_acked(event, confirm.recipient_id) {
                    continue;
                }
                // v3 W3b (Reviewer 68568692): so does this attempt's own
                // InputObserved, which the reconciler may have recorded after
                // this worker's read; its uncertainty is never restored.
                if confirm.attempt_token.is_some_and(|attempt| room.input_observed_for(event, confirm.recipient_id, attempt)) {
                    continue;
                }
                let current = room.latest_receipt_cursor(event, confirm.recipient_id);
                if confirm.attempts.get(message_id).is_none_or(|attempt| current != Some(*attempt)) {
                    self.emit(RuntimeEvent::SystemLog { level: LogLevel::Info,
                        message: format!("room wake submission result for message {message_id} and {} not recorded: \
                            a newer attempt superseded it", confirm.recipient_id), timestamp: now_rfc3339() });
                    continue;
                }
                match room.append(RoomFeedItem::Waiting { message_id: *message_id, recipient_id: confirm.recipient_id,
                    reason_code: code.into(), reason: reason.into(), attempt: confirm.attempt_token }) {
                    Ok(feed_event) => events.push(feed_event),
                    Err(error) => self.emit(RuntimeEvent::SystemLog { level: LogLevel::Warn,
                        message: format!("room wake submission record for {} was not stored: {error}", confirm.recipient_id),
                        timestamp: now_rfc3339() }),
                }
            }
        }
        let recorded = !events.is_empty();
        for feed_event in events { self.emit(RuntimeEvent::RoomFeedEvent { feed_event }); }
        if recorded { self.signal_submit_reconcile(); }
    }


    /// v3 W3: a direct (unqueued) attempt refused before any byte for a
    /// transient reason joins the queue, so it is re-admitted like a queued one.
    fn queue_readmitted_wake(&self, message: &RoomFeedEvent, recipient_id: SessionId, sender_label: &str,
        receipt: &mut RoomDeliveryReceipt) {
        let readmit = self.inner.rooms.lock().get(message.room_id)
            .is_some_and(|room| room.readmit_pending_for(message, recipient_id));
        if readmit && self.enqueue_room_wake(message, recipient_id, sender_label, Instant::now()) {
            receipt.waiting = Some(wake_reason_code(ROOM_WAKE_OUTPUT_CHANGED));
            receipt.queued_because = Some(ROOM_WAKE_OUTPUT_CHANGED.into());
        }
    }

    /// v3 W3 (D3): a deferred wake is visible at once in the feed, and again
    /// only when its reason code changes. It is no receipt and ends nothing.
    fn record_room_wake_waiting(&self, message: &RoomFeedEvent, recipient_id: SessionId, reason: &str) {
        let RoomFeedItem::Message { message_id, .. } = &message.item else { return };
        let code = wake_reason_code(reason);
        let outcome = {
            let _publish = self.inner.room_event_publish.lock();
            let mut rooms = self.inner.rooms.lock();
            let Some(room) = rooms.get_mut(message.room_id) else { return };
            if room.has_receipt(message, recipient_id) || room.waiting_code(message, recipient_id).as_deref() == Some(code.as_str()) {
                return;
            }
            room.append(RoomFeedItem::Waiting { attempt: None, message_id: *message_id, recipient_id,
                reason_code: code, reason: bounded_room_delivery_error(reason) })
        };
        match outcome {
            Ok(feed_event) => self.emit(RuntimeEvent::RoomFeedEvent { feed_event }),
            Err(error) => {
                self.notify_room_history_transition(message.room_id);
                self.emit(RuntimeEvent::SystemLog { level: LogLevel::Warn,
                    message: format!("room wake waiting record for {recipient_id} was not stored: {error}"),
                    timestamp: now_rfc3339() });
            }
        }
    }

    /// The live PRIM-1 path: one bracketed paste of the whole message, then the
    /// driver's submit sequence once the pane's echo settles, verified by output.
    /// v3 W4 S2: or, for the App Server route chosen at planning, one
    /// turn/start through the observer (`route`: the recipe's thread and the
    /// attempt token, present only when the recipe was stored). The flags
    /// say why a refusal before anything was written re-admits the wake.
    fn write_room_wake(&self, message: &RoomFeedEvent, plan: &RoomWakePlan,
        route: Option<(String, shared_types::RoomFeedCursor)>, flags: &Arc<WakeCheckFlags>) -> Result<WakeWrite> {
        let target = &plan.target;
        let check = WakeCheck { message: message.clone(), fallback: plan.fallback.clone(),
            continuation: plan.continuation.clone(), pointer: plan.pointer_through.is_some(),
            pointer_counted: plan.pointer_counted.clone(), input_epoch: plan.input_epoch, session_id: target.session_id,
            run_id: target.run_id, generation: target.generation, input_gate: target.input_gate.clone(),
            app_server: target.app_server.clone() };
        if plan.app_server {
            return self.send_room_wake_to_app_server(plan, check, route, flags);
        }
        {
            let activity = target.input_gate.room_input_activity.lock();
            if !activity.admits(Some(plan.input_epoch)) {
                return Err(PtyWriteError::new(0, "the operator typed into this member just before the room message").into());
            }
        }
        #[cfg(test)]
        self.run_room_wake_before_write_hook_for_tests("paste");
        if target.input_gate.queued_operator_input_blocks_wake() {
            return Err(PtyWriteError::new(0, "queued operator input is waiting; the room message was not typed").into());
        }
        // Rechecked with the writer ticket held: input admitted while this
        // wake waited for the ticket is ahead of it in the pane.
        let still_safe = || self.room_wake_still_safe(&check, true, flags);
        self.write_bracketed_submission_checked(target, &plan.payload, plan.behavior, Some(&still_safe))
            .map(WakeWrite::Typed)
            .map_err(|error| room_error_after_progress(error, 0))
    }

    /// The checks every wake passes at its write boundary, before any byte.
    /// `screen_quiet` is the typed route's own output-quiet recheck; the App
    /// Server route types nothing into the pane and skips it.
    fn room_wake_still_safe(&self, check: &WakeCheck, screen_quiet: bool, flags: &WakeCheckFlags) -> Result<()> {
        let message = &check.message;
        if let Some((room_id, origin)) = &check.fallback {
            let rooms = self.inner.rooms.lock(); let slots = self.inner.slots.lock();
            self.fallback_still_valid(&rooms, &slots, *room_id, origin)
                .map_err(|error| PtyWriteError::new(0, error.to_string()))?;
        }
        if let Some((room_id, origin)) = &check.continuation {
            let rooms = self.inner.rooms.lock(); let slots = self.inner.slots.lock();
            self.continuation_still_valid(&rooms, &slots, *room_id, origin)
                .map_err(|error| PtyWriteError::new(0, error.to_string()))?;
        }
        // v3 W4 S2 (Reviewer 8caec584): the enqueue-time authority checks
        // again at the write, before any byte: the recipient is still a
        // member whose current join covers this message, and a guest sender
        // still holds its invitation. Either failing refuses the head; its
        // own receipt records why and nothing is re-admitted for it.
        let floor = {
            let rooms = self.inner.rooms.lock();
            let room = rooms.get(message.room_id);
            let floor = room.and_then(|room| room.join_floor_by_session.get(&check.session_id).copied()).unwrap_or(0);
            if !room.is_some_and(|room| room.definition.member_ids.contains(&check.session_id))
                || message.cursor.sequence <= floor {
                return Err(PtyWriteError::new(0, ROOM_WAKE_MEMBERSHIP_CHANGED_REFUSAL).into());
            }
            floor
        };
        if let Some(reason) = self.guest_sender_revoked(message) {
            return Err(PtyWriteError::new(0, reason).into());
        }
        // v3 W3 (Reviewer 79ab53d8): each other reminder or continuation
        // this pointer counts is rechecked here too, as its own head would
        // be. One no longer current refuses the fixed typed text before any
        // byte; the group is re-admitted and planned again without it, and
        // its own turn records why. v3 W4 S2 (Reviewer 8caec584): so does a
        // counted request whose guest sender was revoked, or that the
        // member's current join no longer covers (unread_for already drops
        // those, so the counted sequences are compared with the floor).
        if check.pointer {
            let rooms = self.inner.rooms.lock(); let slots = self.inner.slots.lock();
            let below_join = check.pointer_counted.iter().any(|sequence| *sequence <= floor);
            let stale = below_join || rooms.get(message.room_id).is_some_and(|room| room.unread_for(check.session_id).iter()
                .filter(|event| event.cursor != message.cursor && event.cursor.epoch == message.cursor.epoch
                    && check.pointer_counted.contains(&event.cursor.sequence))
                .any(|event| self.guest_sender_revoked(event).is_some()
                    || !self.pointer_origin_admitted(&rooms, &slots, message.room_id, event)));
            if stale {
                flags.transient.store(true, Ordering::Release);
                flags.count_changed.store(true, Ordering::Release);
                return Err(PtyWriteError::new(0, ROOM_WAKE_COUNT_CHANGED_REFUSAL).into());
            }
        }
        if !check.input_gate.room_input_activity.lock().admits(Some(check.input_epoch)) {
            return Err(PtyWriteError::new(0, "the operator typed into this member just before the room message").into());
        }
        if check.input_gate.queued_operator_input_blocks_wake() {
            return Err(PtyWriteError::new(0, "queued operator input is waiting; the room message was not typed").into());
        }
        // v3 W4 S3: a person request opened since planning holds this wake
        // again, before any byte; it is re-admitted and waits at planning.
        if check.app_server.as_ref().and_then(codex_app_server::Dispatcher::open_request).is_some() {
            flags.transient.store(true, Ordering::Release);
            flags.person_request.store(true, Ordering::Release);
            return Err(PtyWriteError::new(0, ROOM_WAKE_PERSON_REQUEST_REFUSAL).into());
        }
        // v3 W2 (Reviewer 0324b01f): the quiet sampled at planning is
        // rechecked here, once, with the writer ticket held and before
        // any byte: output since planning refuses with a zero write.
        if screen_quiet {
            let settle = *self.inner.room_wake_output_settle.lock();
            let quiet = self.inner.slots.lock().get_by_id(check.session_id)
                .filter(|slot| slot.definition.driver == DriverKind::Codex && slot.run_id == Some(check.run_id))
                .is_none_or(|slot| slot.room_composer.output_quiet_for(settle));
            if !quiet {
                flags.transient.store(true, Ordering::Release);
                return Err(PtyWriteError::new(0, ROOM_WAKE_OUTPUT_CHANGED_REFUSAL).into());
            }
        }
        // v3 W3 (Claude PRIM-1, 26a7f92 walk): a pointer whose every
        // counted request the member has read or disposed of since
        // planning has nothing left to point at; nothing is typed. The
        // stored Fetched is never downgraded by the Failed that records it.
        if check.pointer
            && self.inner.rooms.lock().get(message.room_id).is_some_and(|room| !room.unread_for(check.session_id)
                .iter().any(|event| event.cursor.epoch == message.cursor.epoch
                    && check.pointer_counted.contains(&event.cursor.sequence))) {
            flags.already_read.store(true, Ordering::Release);
            return Err(PtyWriteError::new(0, ROOM_WAKE_ALREADY_READ).into());
        }
        Ok(())
    }

    /// v3 W4 S2: one turn/start through the member's observer. Its receipt
    /// comes only from the response to that request's own id (9828cc3b,
    /// 1214431f): a turn id admits it (Written, nothing typed); an error
    /// refuses it with no zero-write inference; no answer is unknown and never
    /// sent again. A refusal before anything was written re-admits the wake,
    /// which is then planned from the start, typed route included only if
    /// planning admits it (Reviewer 617c8f1c).
    fn send_room_wake_to_app_server(&self, plan: &RoomWakePlan, check: WakeCheck,
        route: Option<(String, shared_types::RoomFeedCursor)>, flags: &Arc<WakeCheckFlags>) -> Result<WakeWrite> {
        let route_changed = || {
            flags.transient.store(true, Ordering::Release);
            flags.route_changed.store(true, Ordering::Release);
            Err(PtyWriteError::new(0, ROOM_WAKE_ROUTE_CHANGED_REFUSAL).into())
        };
        #[cfg(test)]
        self.run_room_wake_before_write_hook_for_tests("app_server");
        // Without its stored recipe an attempt has no durable record to bind
        // the request to, so it is never sent this way.
        let Some((thread_id, attempt)) = route else { return route_changed() };
        let Some(dispatcher) = plan.target.app_server.as_ref().filter(|dispatcher| dispatcher.serves(&thread_id)) else {
            return route_changed();
        };
        let weak = Arc::downgrade(&self.inner);
        let check_flags = flags.clone();
        let send_check: codex_app_server::SendCheck = Box::new(move || {
            let Some(inner) = weak.upgrade() else { return Err("PRIM-1 is shutting down; nothing was sent".into()) };
            let handle = SupervisorHandle { inner };
            handle.app_server_send_check(&check, attempt, &check_flags).map_err(|error| room_wake_error(&error))
        });
        match dispatcher.turn_start(&thread_id, &plan.payload, attempt, send_check) {
            codex_app_server::DispatchOutcome::Accepted { turn_id } => Ok(WakeWrite::Accepted(turn_id)),
            codex_app_server::DispatchOutcome::Refused(reason) => Err(AppServerRefused(reason).into()),
            codex_app_server::DispatchOutcome::Unknown(why) => Err(AppServerUnknown(why).into()),
            codex_app_server::DispatchOutcome::NotSent(why) => {
                // The request's own checks set their flags; any other
                // refusal before the write is the route going away.
                if flags.check_refused.load(Ordering::Acquire) {
                    Err(PtyWriteError::new(0, why).into())
                } else {
                    flags.transient.store(true, Ordering::Release);
                    flags.route_changed.store(true, Ordering::Release);
                    Err(PtyWriteError::new(0, format!("{ROOM_WAKE_ROUTE_CHANGED_REFUSAL} ({why})")).into())
                }
            }
        }
    }

    /// v3 W4 S2: on the observer thread, immediately before the socket write:
    /// the run is still the one planned, its wake claim is still this
    /// attempt, and the request passes the same checks a typed wake does.
    fn app_server_send_check(&self, check: &WakeCheck, attempt: shared_types::RoomFeedCursor,
        flags: &WakeCheckFlags) -> Result<()> {
        let refused = |error: anyhow::Error| { flags.check_refused.store(true, Ordering::Release); error };
        let current = self.inner.slots.lock().get_by_id(check.session_id).is_some_and(|slot|
            slot.running.is_some() && slot.run_id == Some(check.run_id) && slot.generation == check.generation);
        if !current {
            return Err(refused(anyhow!("the member's run changed before the room message was sent; nothing was sent")));
        }
        // The run's one wake claim still names exactly this attempt (its
        // head's Pending cursor). A member's own Fetched of the head is not a
        // supersession: the already-read check below judges what is left.
        let RoomFeedItem::Message { message_id, .. } = &check.message.item else { unreachable!() };
        if *check.input_gate.room_wake_claim.lock() != Some((*message_id, attempt)) {
            return Err(refused(anyhow!("this wake's attempt is no longer the member's current one; nothing was sent")));
        }
        self.room_wake_still_safe(check, false, flags).map_err(refused)
    }

    fn publish_room_receipt(&self, room_id: RoomId, outcome: Result<RoomFeedEvent>, receipt: &mut RoomDeliveryReceipt) {
        match outcome {
            Ok(feed_event) => {
                self.resolve_upgrade_continuation_receipt(&feed_event);
                #[cfg(test)]
                self.run_room_event_after_append_hook_for_tests();
                self.emit(RuntimeEvent::RoomFeedEvent { feed_event });
            }
            Err(error) => {
                self.notify_room_history_transition(room_id);
                receipt.recorded = false;
                receipt.error = Some(format!("{}wake receipt not durable: {error}; message remains stored",
                    receipt.error.as_ref().map(|s| format!("{s}; ")).unwrap_or_default()));
                self.emit(RuntimeEvent::SystemLog { level: LogLevel::Error,
                    message: format!("Room wake receipt for {} was not saved: {error}. {} Do not repeat an unknown wake.",
                        receipt.recipient_id,
                        if room_commit_is_uncertain(&error) { "Receipt commit is uncertain; check room history status before retrying." }
                        else if error.is::<room_log::StructuralHistory>() { "Room history requires recovery; no receipt was appended." }
                        else if error.is::<room_log::RecoveryRequired>() { "Room history recovery is pending; no receipt was appended." }
                        else { "Receipt storage was refused before append; resolve the storage error before another attempt." }),
                    timestamp: now_rfc3339() });
            }
        }
    }
}

fn room_receipt_item(message_id: MessageId, receipt: &RoomDeliveryReceipt, zero_write_confirmed: bool) -> RoomFeedItem {
    RoomFeedItem::Delivery { readmit: false, already_read: receipt.already_read, message_id, recipient_id: receipt.recipient_id, status: receipt.status,
        bytes_written: receipt.bytes_written, zero_write_confirmed, error: receipt.error.clone(),
        run_id: receipt.run_id, generation: receipt.generation, pointer: None, title_busy: receipt.title_busy.clone(),
        app_server_turn: receipt.app_server_turn.clone() }
}

/// v3 W3 (D4): a covered request's receipt for the pointer typed for `pointer`.
fn room_cover_receipt_item(message_id: MessageId, receipt: &RoomDeliveryReceipt, pointer: MessageId,
    zero_write_confirmed: bool) -> RoomFeedItem {
    RoomFeedItem::Delivery { title_busy: None, readmit: false, already_read: receipt.already_read, message_id, recipient_id: receipt.recipient_id, status: receipt.status,
        bytes_written: 0, zero_write_confirmed, error: receipt.error.clone(),
        run_id: receipt.run_id, generation: receipt.generation, pointer: Some(pointer),
        app_server_turn: receipt.app_server_turn.clone() }
}

fn room_delivery_failures(receipts: &[RoomDeliveryReceipt]) -> Vec<RoomDeliveryFailure> {
    receipts.iter().filter(|r| matches!(r.status, RoomDeliveryStatus::Failed | RoomDeliveryStatus::Held) && !r.already_read)
        .map(|r| RoomDeliveryFailure { recipient_id: r.recipient_id, bytes_written: r.bytes_written,
            error: r.error.clone().unwrap_or_else(|| "wake not completed".into()) }).collect()
}
