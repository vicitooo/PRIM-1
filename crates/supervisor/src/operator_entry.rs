//! A16 operator entry: a local process of the operator's Windows user that is
//! not a pane (for example a MAINPC supervisor's script on PC3 over the SSH
//! mesh) creates rooms and members, starts and stops them, and invites outside
//! agents with the operator's authority, so a PC needs no hands at its screen
//! for that. Same named pipe, no network listener: the pipe's ACL admits only
//! this Windows user, and the connection handler accepts the entry only from
//! a process proven outside every pane Job (a pane keeps to its own grants).
//! Every effect is attributed in the feed to `LocalProcess`, never to the
//! person at the screen or to a pane. Same supervisor methods, rechecks and
//! request_id replays as the member and room tools.
use super::*;
use member_lifecycle::{Actor, LocalOperator};

/// A pane that opens the operator entry is refused: it acts through its own
/// grants, which the operator sets.
pub(super) const OPERATOR_FROM_PANE_REFUSAL: &str = "a PRIM-1 pane cannot use the operator entry; use your member tools, which your grants allow";
/// What the entry takes: the member and room actions, the lists, ping, and
/// (A16) a room's fallback reminders.
const OPERATOR_ENTRY_SCOPE: &str = "the operator entry takes ping, rooms, members, member, room (guests), settings, conversations, fallbacks, deliver --room, dispose --room and cancel --room requests";
/// A shown actor label is bounded like a session label.
const OPERATOR_ACTOR_MAX_CHARS: usize = 80;

impl LocalOperator {
    /// "<actor> (local process <pid>)", or "local process <pid>". The actor
    /// is the caller's own words, display only, cut to one bounded line.
    pub(super) fn new(actor: Option<&str>, pid: u32) -> Self {
        let actor: String = actor.unwrap_or_default().chars().filter(|character| !character.is_control())
            .take(OPERATOR_ACTOR_MAX_CHARS).collect();
        let actor = actor.trim();
        Self { label: if actor.is_empty() { format!("local process {pid}") } else { format!("{actor} (local process {pid})") } }
    }
}

impl SupervisorHandle {
    pub(super) async fn apply_operator_request_async(&self, local: LocalOperator, request: SidebandRequest) -> SidebandResponse {
        let handle = self.clone();
        match tokio::task::spawn_blocking(move || handle.apply_operator_request(&local, request)).await {
            Ok(response) => response,
            Err(error) => Self::rejected_sideband_response(format!("operator worker failed: {error}")),
        }
    }

    /// Runs on a blocking worker, as the member and room requests do.
    pub(super) fn apply_operator_request(&self, local: &LocalOperator, request: SidebandRequest) -> SidebandResponse {
        if let Err(error) = self.ensure_mutations_admitted() {
            return Self::rejected_sideband_response(error.to_string());
        }
        let actor = Actor::Local(local);
        let outcome = match request {
            SidebandRequest::Ping {} => Ok((format!("pong from PRIM-1 for {}", local.label), None)),
            SidebandRequest::RoomsList {} => self.list_rooms(None).map(|(current_room_id, rooms)|
                (format!("{} rooms", rooms.len()), Some(SidebandResponsePayload::Rooms { current_room_id, rooms }))),
            SidebandRequest::MembersList {} => self.members_list(actor).map(|members|
                (format!("{} sessions", members.len()), Some(SidebandResponsePayload::Members { members }))),
            SidebandRequest::Member { request_id, action } => match Uuid::parse_str(request_id.trim()) {
                Ok(request_id) => self.claim_member_action(actor, &request_id.to_string(), action).map(|result| {
                    (format!("{} {}: {}", result.op, result.member.label,
                        if result.replayed { "replayed the first result" } else { "done" }),
                        Some(SidebandResponsePayload::Member { result }))
                }),
                Err(_) => Err(anyhow!("request_id must be a UUID: a fresh one per action, the same one only to retry it")),
            },
            SidebandRequest::Room { request_id, action } => self.room_action(actor, request_id, action).map(|result| {
                (format!("room {} \"{}\": {}", result.op, result.room.label,
                    if result.replayed { "replayed the first result" } else { "done" }),
                    Some(SidebandResponsePayload::Room { result }))
            }),
            SidebandRequest::Runtime { request_id, action } => self.runtime_action(actor, request_id, action).map(|result| {
                (format!("{}: {}", result.op, if result.replayed { "replayed the first result" } else { "done" }),
                    Some(SidebandResponsePayload::Runtime { result }))
            }),
            request @ SidebandRequest::RoomFallbacks { .. } => self.room_fallbacks_action(actor, request).map(|snapshot|
                ("room fallback settings".into(), Some(SidebandResponsePayload::RoomFallbacks { snapshot }))),
            SidebandRequest::RoomDispose { room_id, message_id, recipient, request_id, next_action, decline } =>
                self.dispose_room_message(room_id, message_id, &recipient,
                    shared_types::RoomAckActor::LocalProcess { label: local.label.clone() },
                    next_action, decline, request_id).map(|result| (format!("disposition {} for message {}",
                        if result.replayed { "replayed" } else { "stored" }, result.message_id),
                        Some(SidebandResponsePayload::RoomAck { result }))),
            // v3 W3 (D6): withdraw a stored message, recorded as this local process.
            SidebandRequest::RoomCancel { room_id, message_id, reason, request_id } =>
                self.cancel_room_message(room_id, message_id,
                    shared_types::RoomAckActor::LocalProcess { label: local.label.clone() }, reason, request_id)
                    .map(|result| (format!("message {} {}; {} recipient(s) held with the reason; {} already reached ({}; it does not undo what they did)",
                        result.message_id, if result.replayed { "withdrawal replayed" } else { "withdrawn" }, result.held.len(),
                        result.reached.len(), match result.notice_message_id {
                            Some(notice) => format!("PRIM-1 notified them with notice {notice}"),
                            None => "PRIM-1 sent them no notice".into(),
                        }),
                        Some(SidebandResponsePayload::RoomCancel { result }))),
            // v3 W5 (CLI parity): send to a named room's members, recorded as
            // this local process (never as the person at the screen).
            SidebandRequest::RoomDeliverTo { room_id, recipient, content, request_id } =>
                self.deliver_room_message_from_operator_entry(local, room_id, &recipient, content, request_id).map(|result|
                    (format!("room message stored; wake written for {} of {} recipients (not model acceptance)",
                        result.written_count, result.recipient_count), Some(SidebandResponsePayload::RoomDelivery { result }))),
            _ => Err(anyhow!(OPERATOR_ENTRY_SCOPE)),
        };
        match outcome {
            Ok((message, payload)) => SidebandResponse { ok: true, message, timed_out: false, payload, request_id: None },
            Err(error) => Self::rejected_sideband_response(format!("{error:#}")),
        }
    }

    /// v3 W3: the operator's disposition of `message_id` for one recipient it
    /// addressed (label or session id, members or former members), as `actor`.
    pub(super) fn dispose_room_message(&self, room_id: RoomId, message_id: shared_types::MessageId, recipient: &str,
        actor: shared_types::RoomAckActor, next_action: Option<String>, decline: Option<String>,
        request_id: Option<String>) -> Result<shared_types::RoomAckResult> {
        anyhow::ensure!(!matches!(actor, shared_types::RoomAckActor::Session { .. }),
            "a member acknowledges for itself with room_ack");
        anyhow::ensure!(!matches!(actor, shared_types::RoomAckActor::Prim1 {}),
            "only PRIM-1 itself disposes as PRIM-1");
        let (disposition, note) = room_ack_fields(next_action.as_deref(), decline.as_deref(), None)?;
        let request_id = rooms::request_id(request_id)?;
        let _publish = self.inner.room_event_publish.lock();
        self.recover_room_writer(room_id)?;
        let (cursor, replayed) = {
            let mut rooms = self.inner.rooms.lock();
            let slots = self.inner.slots.lock();
            let room = rooms.get(room_id).ok_or_else(|| anyhow!("unknown room id '{room_id}'"))?;
            let member = room.addressed_recipient_named(message_id, recipient.trim(), |id| {
                slots.get_by_id(id).map(|slot| slot.definition.label.clone())
            });
            drop(slots); // Never hold the session registry across disk sync.
            match member {
                Ok(recipient_id) => rooms.get_mut(room_id).expect("room is locked").record_ack(request_id.clone(),
                    message_id, recipient_id, actor, disposition, note, None),
                // v3 W5 (decision 9): a guest the message addressed, by its
                // label while invited or by its guest id (also once revoked).
                Err(member_error) => match self.addressed_guest_named(room, message_id, recipient.trim()) {
                    Some(guest_id) => {
                        let actor = match actor {
                            shared_types::RoomAckActor::Operator {} => shared_types::RoomGuestAckActor::Operator {},
                            shared_types::RoomAckActor::LocalProcess { label } =>
                                shared_types::RoomGuestAckActor::LocalProcess { label },
                            shared_types::RoomAckActor::Session { .. } | shared_types::RoomAckActor::Prim1 {} =>
                                unreachable!("refused above"),
                        };
                        rooms.get_mut(room_id).expect("room is locked").record_guest_ack(request_id.clone(),
                            message_id, guest_id, actor, disposition, note, None)
                    }
                    None => Err(member_error),
                },
            }
        }.map_err(|error| { self.notify_room_history_transition(room_id); error })?;
        if !replayed {
            let feed_event = self.inner.rooms.lock().get(room_id).and_then(|room| room.event_at(cursor));
            if let Some(feed_event) = feed_event { self.emit(RuntimeEvent::RoomFeedEvent { feed_event }); }
        }
        Ok(shared_types::RoomAckResult { request_id, replayed, room_id, message_id, cursor })
    }

    /// v3 W3 (b): the person at the screen disposes of a message for one of
    /// its recipients, from the desktop; recorded as the operator's, never as
    /// the recipient's own ACK.
    pub fn dispose_room_message_as_operator(&self, request: shared_types::DisposeRoomMessageRequest)
        -> Result<shared_types::RoomAckResult> {
        self.ensure_active()?;
        self.dispose_room_message(request.room_id, request.message_id, &request.recipient,
            shared_types::RoomAckActor::Operator {}, request.next_action, request.decline, request.request_id)
    }

    /// v3 W3 (D6): the person at the screen withdraws a message, from the desktop.
    pub fn cancel_room_message_as_operator(&self, request: shared_types::CancelRoomMessageRequest)
        -> Result<shared_types::RoomCancelResult> {
        self.ensure_active()?;
        self.cancel_room_message(request.room_id, request.message_id, shared_types::RoomAckActor::Operator {},
            request.reason, request.request_id)
    }

    /// v3 W3 (D6): the operator (UI or a local process with its authority)
    /// withdraws `message_id`: durable and attributed. Recipients not yet
    /// attempted are Held with the reason and their queued wakes dropped; an
    /// attempt in flight is refused before its first byte once nothing it
    /// counts is open; a Retry is refused. Nothing is deleted.
    pub(super) fn cancel_room_message(&self, room_id: RoomId, message_id: shared_types::MessageId,
        actor: shared_types::RoomAckActor, reason: String, request_id: Option<String>)
        -> Result<shared_types::RoomCancelResult> {
        anyhow::ensure!(!matches!(actor, shared_types::RoomAckActor::Session { .. } | shared_types::RoomAckActor::Prim1 {}),
            "withdrawing a message is an operator action");
        let reason = reason.trim().to_string();
        anyhow::ensure!(!reason.is_empty() && reason.len() <= 2048, "the reason must be 1 to 2048 bytes");
        anyhow::ensure!(!reason.chars().any(|ch| ch.is_control() && ch != '\n'), "the reason may not contain control characters");
        let request_id = rooms::request_id(request_id)?;
        let (cursor, replayed, held, reached, notify) = {
            let _publish = self.inner.room_event_publish.lock();
            self.recover_room_writer(room_id)?;
            let (cursor, replayed, events, held, reached, notify) = {
                let mut rooms = self.inner.rooms.lock();
                let room = rooms.get_mut(room_id).ok_or_else(|| anyhow!("unknown room id '{room_id}'"))?;
                room.record_cancel(request_id.clone(), message_id, actor, reason.clone())
            }.map_err(|error| { self.notify_room_history_transition(room_id); error })?;
            for feed_event in events { self.emit(RuntimeEvent::RoomFeedEvent { feed_event }); }
            (cursor, replayed, held, reached, notify)
        };
        // Propagated: no queued wake of it is ever attempted.
        self.drop_queued_room_wakes_for(room_id, message_id);
        // Claude PRIM-1: the members it reached are woken with a pointer to a
        // PRIM-1 notice (no ACK asked), so an agent mid-task learns of it now.
        // v3 W3 (Reviewer bde87a63): a replay re-derives nothing. It finds the
        // first notice by its request id; if the notice was never stored (a
        // crash or a failure after the durable Cancel), it posts it now for the
        // recipients the Cancel recorded, once.
        let existing = replayed.then(|| self.inner.rooms.lock().get(room_id).and_then(|room|
            room.retained_delivery(&RoomMessageSender::Prim1 {}, &withdrawal_notice_request_id(&request_id))))
            .flatten().map(|notice| notice.message_id);
        let notice_message_id = existing
            .or_else(|| self.post_withdrawal_notice(room_id, message_id, &reason, &request_id, &notify));
        Ok(shared_types::RoomCancelResult { request_id, replayed, room_id, message_id, cursor, held, reached,
            notice_message_id })
    }

    /// The withdrawal notice for the recorded recipients still in the room
    /// (a member that left can no longer be addressed), or None.
    fn post_withdrawal_notice(&self, room_id: RoomId, message_id: shared_types::MessageId, reason: &str,
        request_id: &str, reached: &[SessionId]) -> Option<shared_types::MessageId> {
        let (recipient_ids, membership_revision) = {
            let rooms = self.inner.rooms.lock();
            let room = rooms.get(room_id)?;
            (reached.iter().copied().filter(|id| room.definition.member_ids.contains(id)).collect::<Vec<_>>(),
                room.definition.membership_revision)
        };
        if recipient_ids.is_empty() { return None; }
        let content = format!("[PRIM-1] The operator withdrew message {} in this room: {reason}. Stop acting on it; it needs no ACK. \
            Do not undo work you already did on your own; post what you had done, so the operator can decide.", message_id);
        match self.deliver_room_message_as(RoomDeliverySpec { guest_recipient_ids: Vec::new(), fallback_origin: None, room_id,
            recipient_ids, membership_revision, sender: RoomMessageSender::Prim1 {}, sender_label: "PRIM-1".into(), content,
            request_id: Some(withdrawal_notice_request_id(request_id)), admission: RoomDeliveryAdmission::Member,
            roam_required: false, ack_requested: false }, None) {
            Ok(result) => Some(result.message_id),
            Err(error) => {
                self.emit(RuntimeEvent::SystemLog { level: LogLevel::Warn,
                    message: format!("the withdrawal of {message_id} was stored, but its notice was not: {error:#}"),
                    timestamp: now_rfc3339() });
                None
            }
        }
    }

    /// v3 W5: the guest `name` (an invited guest's label, or a guest id) that
    /// `message_id` addressed in `room`.
    fn addressed_guest_named(&self, room: &crate::rooms::RoomRuntime, message_id: shared_types::MessageId, name: &str)
        -> Option<Uuid> {
        let addressed = room.guest_recipients_of(message_id)?;
        if let Ok(id) = Uuid::parse_str(name) && addressed.contains(&id) {
            return Some(id);
        }
        self.room_guests(room.definition.room_id).into_iter()
            .find(|guest| addressed.contains(&guest.guest_id) && guest.label.eq_ignore_ascii_case(name))
            .map(|guest| guest.guest_id)
    }

    /// `recipient` is `all` (every member), or one member's label or session
    /// id in that room. The message asks each recipient for an ACK.
    fn deliver_room_message_from_operator_entry(&self, local: &LocalOperator, room_id: RoomId, recipient: &str,
        content: String, request_id: Option<String>) -> Result<RoomDeliveryResult> {
        validate_message_body(&content)?;
        let (membership_revision, recipient_ids) = {
            let rooms = self.inner.rooms.lock();
            let slots = self.inner.slots.lock();
            let room = rooms.get(room_id).ok_or_else(|| anyhow!("unknown room id '{room_id}'"))?;
            // Same resolution as a pane's room_deliver; the nil sender is no
            // member, so `all` is every member.
            let ids = Self::resolve_pane_delivery_recipients(room, &slots, SessionId::nil(), recipient)?;
            (room.definition.membership_revision, ids)
        };
        self.deliver_room_message_as(RoomDeliverySpec { guest_recipient_ids: Vec::new(), fallback_origin: None, room_id, recipient_ids, membership_revision,
            sender: RoomMessageSender::LocalProcess { label: local.label.clone() }, sender_label: local.label.clone(),
            content, request_id, admission: RoomDeliveryAdmission::Member, roam_required: false, ack_requested: true }, None)
    }
}

/// v3 W3 (D6): the request id of a withdrawal's PRIM-1 notice; a replay finds it by this.
fn withdrawal_notice_request_id(cancel_request_id: &str) -> String {
    format!("withdrawal-{cancel_request_id}")
}
