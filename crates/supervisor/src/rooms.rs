use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use shared_types::{
    RoomFeedCursor, RoomFeedEvent, RoomFeedGap, RoomFeedGapReason, RoomFeedItem, RoomFeedPage,
    RoomId, RoomRevision, RoomSequence, RoomSnapshot, SessionId, now_rfc3339,
    RoomMessageSender, RoomDeliveryReceipt, RoomDeliveryResult, RoomDeliveryFailure,
    RoomDeliveryStatus,
};
use uuid::Uuid;
use crate::room_log::structural_history;

pub(crate) const ROOM_CATALOG_SCHEMA_VERSION: u32 = 1;
pub(crate) const ROOM_CATALOG_FILE_NAME: &str = "room-catalog-v1.json";
pub(crate) const ROOM_MAX_COUNT: usize = 64;
pub(crate) const ROOM_MEMBER_MAX_COUNT: usize = 64;
pub(crate) const ROOM_LABEL_MAX_CHARS: usize = 128;
const ROOM_FEED_MAX_EVENTS: usize = 512;
const ROOM_FEED_MAX_BYTES: usize = 16 * 1024 * 1024;
const ROOM_FEED_PAGE_MAX_EVENTS: usize = 64;
const ROOM_FEED_PAGE_MAX_BYTES: usize = 2 * 1024 * 1024;
const ROOM_DEDUP_MAX_POSTS: usize = 1024;
const ROOM_DEDUP_MAX_BYTES: usize = 16 * 1024 * 1024;
/// v3 (Claude PRIM-1, 15a49bdf): PRIM-1 sends its continuations (upgrade and
/// recovery) as itself; an earlier build stored them under the operator.
pub(crate) const CONTINUATION_SENDERS: [RoomMessageSender; 2] = [RoomMessageSender::Prim1 {}, RoomMessageSender::Operator {}];

fn default_brief_on_join() -> bool {
    true
}

fn room_notice_is_false(value: &bool) -> bool { !*value }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedRoomV1 {
    #[serde(default, skip_serializing_if = "crate::room_fallbacks::RoomFallbackRecord::is_unused")]
    pub(crate) fallbacks: crate::room_fallbacks::RoomFallbackRecord,
    /// Authorization boundary survives restart, independently of feed retention.
    #[serde(default)]
    pub(crate) join_floors: Option<HashMap<SessionId, RoomSequence>>,
    /// Sequence boundaries are meaningful only inside this log epoch.
    /// Absent on legacy catalogs; bound after a successful replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) join_floor_epoch: Option<Uuid>,
    /// Earlier history was replaced or an empty legacy boundary was recovered.
    /// Persist the notice together with the new membership boundary.
    #[serde(default, skip_serializing_if = "room_notice_is_false")]
    pub(crate) history_restarted: bool,
    pub(crate) room_id: RoomId,
    pub(crate) label: String,
    #[serde(default)]
    pub(crate) resume_on_restart: bool,
    pub(crate) member_ids: Vec<SessionId>,
    pub(crate) membership_revision: RoomRevision,
    /// Legacy name: enables this room default at each selected agent launch.
    /// Joining a room never types a brief. Missing legacy values remain true.
    #[serde(default = "default_brief_on_join")]
    pub(crate) brief_on_join: bool,
    /// Operator-edited brief template; `None` means the canonical brief.
    #[serde(default)]
    pub(crate) brief_template: Option<String>,
    /// Legacy membership-brief history retained for catalog compatibility.
    /// Launch choices no longer consult or update these delivery marks.
    #[serde(default)]
    pub(crate) briefed_member_ids: Option<Vec<SessionId>>,
}

impl PersistedRoomV1 {
    pub(crate) fn member_briefed(&self, session_id: SessionId) -> bool {
        self.briefed_member_ids
            .as_deref()
            .is_some_and(|ids| ids.contains(&session_id))
    }

    pub(crate) fn mark_member_briefed(&mut self, session_id: SessionId) {
        let ids = self.briefed_member_ids.get_or_insert_with(Vec::new);
        if !ids.contains(&session_id) {
            ids.push(session_id);
        }
    }

    pub(crate) fn clear_member_briefed(&mut self, session_id: SessionId) {
        if let Some(ids) = self.briefed_member_ids.as_mut() {
            ids.retain(|id| *id != session_id);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RoomCatalogV1 {
    pub(crate) schema_version: u32,
    pub(crate) rooms: Vec<PersistedRoomV1>,
}

impl RoomCatalogV1 {
    pub(crate) fn empty() -> Self {
        Self {
            schema_version: ROOM_CATALOG_SCHEMA_VERSION,
            rooms: Vec::new(),
        }
    }
}

impl RoomRuntime {
    /// Retained messages with recipients that have no wake receipt at all:
    /// the durable "stored, not yet typed" state the A106 queue resumes from.
    pub(crate) fn unreceipted_messages(&self) -> Vec<(RoomFeedEvent, Vec<SessionId>)> {
        let mut found: Vec<_> = self.posts.values().filter_map(|post| {
            let RoomFeedItem::Message { recipient_ids, .. } = &post.event.item else { return None };
            let missing: Vec<_> = recipient_ids.iter()
                .filter(|id| !post.receipts.contains_key(id) || post.readmit_pending(**id)).copied().collect();
            (!missing.is_empty()).then(|| (post.event.clone(), missing))
        }).collect();
        found.sort_by_key(|(event, _)| event.cursor.sequence);
        found
    }

    /// A message under 12 h old with a current member that has no receipt yet.
    fn awaits_wake(&self, post: &StoredPost) -> bool {
        let RoomFeedItem::Message { recipient_ids, .. } = &post.event.item else { return false };
        let fresh = chrono::DateTime::parse_from_rfc3339(&post.event.timestamp).ok()
            .is_some_and(|at| chrono::Utc::now().signed_duration_since(at) < chrono::Duration::hours(12));
        fresh && post.cancel.is_none() && recipient_ids.iter().any(|id| self.definition.member_ids.contains(id)
            && (!post.receipts.contains_key(id) || post.readmit_pending(*id)))
    }

    /// v3 W5: per member, the feed positions of its latest retained Fetched
    /// receipt and its latest own ACK or decline (event facts, not health).
    pub(crate) fn member_confirmations(&self) -> HashMap<SessionId, (Option<RoomFeedCursor>, Option<RoomFeedCursor>)> {
        let later = |current: Option<RoomFeedCursor>, cursor: RoomFeedCursor|
            Some(current.filter(|seen| seen.sequence >= cursor.sequence).unwrap_or(cursor));
        let mut out: HashMap<SessionId, (Option<RoomFeedCursor>, Option<RoomFeedCursor>)> = HashMap::new();
        for post in self.posts.values() {
            for (recipient, stored) in &post.receipts {
                if stored.receipt.status == RoomDeliveryStatus::Fetched {
                    let entry = out.entry(*recipient).or_default();
                    entry.0 = later(entry.0, stored.cursor);
                }
            }
            for (recipient, ack) in &post.acks {
                if matches!(ack.actor, shared_types::RoomAckActor::Session { session_id } if session_id == *recipient) {
                    let entry = out.entry(*recipient).or_default();
                    entry.1 = later(entry.1, ack.cursor);
                }
            }
        }
        out
    }

    /// Wake attempts whose outcome is not recorded: one still in progress, or
    /// a retained Pending receipt with no later outcome (A108 blocker).
    pub(crate) fn unresolved_deliveries(&self) -> usize {
        self.in_flight_deliveries + self.posts.values()
            .flat_map(|post| post.receipts.values())
            .filter(|stored| stored.receipt.status == RoomDeliveryStatus::Pending)
            .count()
    }

    /// True when `recipient` has any wake receipt (or Fetched) for this message.
    /// A Waiting record is no receipt (v3 W3): it never ends wake eligibility.
    pub(crate) fn has_receipt(&self, event: &RoomFeedEvent, recipient: SessionId) -> bool {
        let RoomFeedItem::Message { request_id, sender, .. } = &event.item else { return false };
        // A re-admittable transient refusal is not an outcome that ends the wake.
        self.posts.get(&post_key(sender, request_id)).is_some_and(|post| post.receipts.contains_key(&recipient)
            && !post.readmit_pending(recipient))
    }

    /// v3 W3: the one recipient `message_id` addressed that `name` names (its
    /// session id, or its current label via `label_of`), whether or not it is
    /// still a member. Ambiguous or unaddressed names are refused.
    pub(crate) fn addressed_recipient_named(&self, message_id: Uuid, name: &str,
        label_of: impl Fn(SessionId) -> Option<String>) -> Result<SessionId> {
        let post = self.posts.values().find(|post| matches!(&post.event.item,
            RoomFeedItem::Message { message_id: id, .. } if *id == message_id))
            .ok_or_else(|| anyhow!("message {message_id} is not in this room's retained history"))?;
        let RoomFeedItem::Message { recipient_ids, .. } = &post.event.item else { unreachable!() };
        let matches: Vec<_> = recipient_ids.iter().copied().filter(|id| id.to_string() == name
            || label_of(*id).is_some_and(|label| label.trim().eq_ignore_ascii_case(name))).collect();
        match matches.as_slice() {
            [one] => Ok(*one),
            [] => Err(anyhow!("message {message_id} was not addressed to '{name}'")),
            _ => Err(anyhow!("'{name}' names more than one recipient of message {message_id}; use a session id")),
        }
    }

    /// v3 W3: where the recipient's latest receipt for this message is.
    pub(crate) fn latest_receipt_cursor(&self, event: &RoomFeedEvent, recipient: SessionId) -> Option<RoomFeedCursor> {
        let RoomFeedItem::Message { request_id, sender, .. } = &event.item else { return None };
        self.posts.get(&post_key(sender, request_id)).and_then(|post| post.receipts.get(&recipient)).map(|stored| stored.cursor)
    }

    /// v3 W3b: attempts whose submission is still unobserved, from their
    /// durable recipes — also when the journal ends before any Waiting
    /// (Reviewer 02655ed3 case 3). Each comes with the latest receipt it was
    /// computed against; oldest message first.
    pub(crate) fn open_submissions(&self) -> Vec<(RoomFeedEvent, SessionId, SubmitRecipe, RoomFeedCursor)> {
        let mut open: Vec<_> = self.posts.values().flat_map(|post| post.recipes.iter()
            .filter_map(|(recipient, recipe)| post.submission_open(*recipient, recipe)
                .map(|written| (post.event.clone(), *recipient, recipe.clone(), written)))).collect();
        open.sort_by_key(|(event, recipient, _, _)| (event.cursor.sequence, *recipient));
        open
    }

    /// v3 W3b (Claude PRIM-1 f0899a06): attempts a previous PRIM-1 process
    /// wrote but never settled: an open recipe whose latest receipt is
    /// Written, with no observation and no `submit_*` waiting code. Called once
    /// at startup, before anything of this process is typed.
    pub(crate) fn unsettled_written_attempts(&self) -> Vec<(shared_types::MessageId, SessionId, RoomFeedCursor)> {
        let mut out: Vec<_> = self.posts.values().flat_map(|post| post.recipes.iter()
            .filter(|(recipient, recipe)| post.submission_open(**recipient, recipe).is_some()
                && post.receipts.get(*recipient).is_some_and(|stored| stored.receipt.status == RoomDeliveryStatus::Written)
                && !post.waiting.get(*recipient).is_some_and(|code| code.starts_with("submit_")))
            .filter_map(|(recipient, recipe)| match &post.event.item {
                RoomFeedItem::Message { message_id, .. } => Some((*message_id, *recipient, recipe.attempt)),
                _ => None,
            })).collect();
        out.sort_by_key(|(_, recipient, attempt)| (attempt.sequence, *recipient));
        out
    }

    /// v3 W3b (Claude PRIM-1 GREEN-1 walk): this attempt's input is already
    /// recorded as observed for this recipient; neither path appends another.
    pub(crate) fn input_observed_for(&self, event: &RoomFeedEvent, recipient: SessionId, attempt: RoomFeedCursor) -> bool {
        let RoomFeedItem::Message { request_id, sender, .. } = &event.item else { return false };
        self.posts.get(&post_key(sender, request_id)).is_some_and(|post| post.observed.get(&recipient) == Some(&attempt))
    }

    /// v3 W3b: still unobserved, for this attempt, with `written` still the
    /// recipient's latest receipt (no newer attempt, run or read since).
    pub(crate) fn submission_open(&self, event: &RoomFeedEvent, recipient: SessionId, attempt: RoomFeedCursor,
        written: RoomFeedCursor) -> bool {
        let RoomFeedItem::Message { request_id, sender, .. } = &event.item else { return false };
        self.posts.get(&post_key(sender, request_id)).and_then(|post| post.recipes.get(&recipient)
            .filter(|recipe| recipe.attempt == attempt).and_then(|recipe| post.submission_open(recipient, recipe)))
            == Some(written)
    }

    /// v3 W3: the recipient read (Fetched) or disposed of this message.
    pub(crate) fn read_or_acked(&self, event: &RoomFeedEvent, recipient: SessionId) -> bool {
        let RoomFeedItem::Message { request_id, sender, .. } = &event.item else { return false };
        self.posts.get(&post_key(sender, request_id)).is_some_and(|post| post.acks.contains_key(&recipient)
            || post.receipts.get(&recipient).is_some_and(|stored| stored.receipt.status == RoomDeliveryStatus::Fetched))
    }


    /// Confirmed-zero transient refusals already re-admitted for this wake.
    pub(crate) fn readmit_attempts(&self, message_id: Uuid, recipient: SessionId) -> u8 {
        self.posts.values().find(|post| matches!(&post.event.item,
            RoomFeedItem::Message { message_id: id, .. } if *id == message_id))
            .and_then(|post| post.readmit.get(&recipient)).map_or(0, |(_, attempts)| *attempts)
    }

    pub(crate) fn readmit_pending_for(&self, event: &RoomFeedEvent, recipient: SessionId) -> bool {
        let RoomFeedItem::Message { request_id, sender, .. } = &event.item else { return false };
        self.posts.get(&post_key(sender, request_id)).is_some_and(|post| post.readmit_pending(recipient))
    }

    /// v3 W3: the message asked its recipients for an ACK. Legacy history
    /// and system messages (fallbacks, continuations, raw posts) did not: their
    /// Written/Fetched receipts stay an unknown disposition, never an invented
    /// ACK, and they are never pinned or pointer-counted for one.
    fn ack_tracked(&self, event: &RoomFeedEvent) -> bool {
        matches!(event.item, RoomFeedItem::Message { ack_requested: true, .. })
    }

    /// v3 W3 (D6 wake): what a pointer counts. ACK-tracked messages, and a
    /// PRIM-1 notice addressed to members (a withdrawal): it is woken by the
    /// same content-neutral pointer and read like any message, but never owed,
    /// and asks no ACK; it is kept until each addressed member read it
    /// (`notice_unread_by`, Reviewer 5571e300).
    fn pointer_tracked(&self, event: &RoomFeedEvent) -> bool {
        self.ack_tracked(event) || matches!(&event.item,
            RoomFeedItem::Message { sender: RoomMessageSender::Prim1 {}, recipient_ids, .. } if !recipient_ids.is_empty())
    }

    /// v3 W3 (Reviewer 5571e300): an addressed no-ACK PRIM-1 notice (a
    /// withdrawal's) that `reader`, a current member, has not read yet: no
    /// Fetched receipt of its own. It is kept in the post index and returned
    /// in the reader's retained page until then, also after the feed window
    /// and the 12 h wake bound let it go; its Fetched ends that. Not an
    /// obligation: it is never owed and asks no ACK.
    fn notice_unread_by(&self, post: &StoredPost, reader: SessionId) -> bool {
        let RoomFeedItem::Message { recipient_ids, .. } = &post.event.item else { return false };
        !self.ack_tracked(&post.event) && self.pointer_tracked(&post.event)
            && recipient_ids.contains(&reader) && self.definition.member_ids.contains(&reader)
            && post.event.cursor.sequence > self.join_floor_by_session.get(&reader).copied().unwrap_or(0)
            && !post.receipts.get(&reader).is_some_and(|stored| stored.receipt.status == RoomDeliveryStatus::Fetched)
    }

    fn awaits_fetch(&self, post: &StoredPost) -> bool {
        let RoomFeedItem::Message { recipient_ids, .. } = &post.event.item else { return false };
        recipient_ids.iter().any(|id| self.notice_unread_by(post, *id))
    }

    /// v3 W3: an ACK-tracked message that a recipient has neither
    /// acknowledged nor declined. No age limit and no membership condition: a
    /// recipient that left keeps its obligation open (visible) until an
    /// attributed disposition; a full index refuses new posts instead.
    fn awaits_ack(&self, post: &StoredPost) -> bool {
        let RoomFeedItem::Message { recipient_ids, guest_recipient_ids, .. } = &post.event.item else { return false };
        // v3 W5: an addressed guest's obligation pins the message the same way,
        // also after a revocation, until an attributed disposition.
        // v3 W3 (D6): a withdrawn message is owed by nobody.
        self.ack_tracked(&post.event) && post.cancel.is_none() && (recipient_ids.iter().any(|id| !post.acks.contains_key(id))
            || guest_recipient_ids.iter().any(|id| !post.guest_acks.contains_key(id)))
    }

    /// v3 W3 (D6): the stored withdrawal of `message_id`, if any.
    pub(crate) fn cancel_for(&self, message_id: Uuid) -> Option<StoredCancel> {
        self.post_by_message_id(message_id).and_then(|post| post.cancel.clone())
    }

    /// v3 W3 (D6): withdraw `message_id`, attributed to `actor`, with `reason`.
    /// Every addressed member that has no wake attempt yet (or one waiting to
    /// be re-admitted) gets a Held receipt with the reason in the same locked
    /// block; nothing is deleted. The same request replays; a different second
    /// withdrawal is refused. Returns the appended events and the held members.
    pub(crate) fn record_cancel(&mut self, request_id: String, message_id: Uuid, actor: shared_types::RoomAckActor,
        reason: String) -> Result<(RoomFeedCursor, bool, Vec<RoomFeedEvent>, Vec<SessionId>, Vec<SessionId>, Vec<SessionId>)> {
        let post = self.post_by_message_id(message_id)
            .ok_or_else(|| anyhow!("message {message_id} is not in this room's retained history"))?;
        if let Some(existing) = &post.cancel {
            if existing.request_id == request_id && same_ack_actor(&existing.actor, &actor) && existing.reason == reason {
                // v3 W3 (Reviewer bde87a63): the same outcome as recorded, never
                // re-derived from the current state: held are the never-reached
                // recipients this withdrawal held.
                let held = existing.not_typed.iter().copied().filter(|id| post.receipts.get(id).is_some_and(|stored|
                    stored.receipt.status == RoomDeliveryStatus::Held && stored.cursor.sequence > existing.cursor.sequence
                        && stored.receipt.error.as_deref().is_some_and(|error| error.starts_with("withdrawn: ")))).collect();
                return Ok((existing.cursor, true, Vec::new(), held, existing.reached.clone(), existing.notified.clone()));
            }
            return Err(anyhow!("message {message_id} was already withdrawn (sequence {})", existing.cursor.sequence));
        }
        let RoomFeedItem::Message { recipient_ids, .. } = &post.event.item else { unreachable!() };
        let held: Vec<SessionId> = recipient_ids.iter().copied()
            .filter(|id| !post.acks.contains_key(id) && (!post.receipts.contains_key(id) || post.readmit_pending(*id)))
            .collect();
        // Reached: typed, woken, read or being typed (or partly typed), or
        // already disposed of. Withdrawing never undoes that, and says so.
        let reached: Vec<SessionId> = recipient_ids.iter().copied().filter(|id| !held.contains(id)
            && (post.acks.contains_key(id) || post.receipts.get(id).is_some_and(|stored| stored.receipt.bytes_written > 0
                || matches!(stored.receipt.status, RoomDeliveryStatus::Pending | RoomDeliveryStatus::Written
                    | RoomDeliveryStatus::Woken | RoomDeliveryStatus::Fetched))))
            .collect();
        let not_typed: Vec<SessionId> = recipient_ids.iter().copied().filter(|id| !reached.contains(id)).collect();
        // Woken with a withdrawal notice: those it reached past an in-flight
        // attempt. One still Pending either sees the Cancel when its pointer
        // lands or was never typed to.
        let notify: Vec<SessionId> = reached.iter().copied().filter(|id| post.acks.contains_key(id)
            || post.receipts.get(id).is_some_and(|stored| stored.receipt.bytes_written > 0
                || stored.receipt.status != RoomDeliveryStatus::Pending)).collect();
        let reused = self.posts.values().any(|other| other.cancel.as_ref().is_some_and(|cancel|
            cancel.request_id == request_id && same_ack_actor(&cancel.actor, &actor)));
        if reused {
            return Err(anyhow!("request_id {request_id} already names another withdrawal; use a fresh one"));
        }
        self.ensure_sequence_capacity(1 + held.len())?;
        self.ensure_receipt_evidence_capacity(held.len())?;
        let who = match &actor {
            shared_types::RoomAckActor::Operator {} => "the operator".to_string(),
            shared_types::RoomAckActor::LocalProcess { label } => label.clone(),
            shared_types::RoomAckActor::Session { session_id } => format!("member {session_id}"),
            shared_types::RoomAckActor::Prim1 {} => "PRIM-1".to_string(),
        };
        let note = format!("withdrawn: {who} cancelled this message ({reason}); it is not typed");
        let cancel = self.append(RoomFeedItem::Cancel { request_id, message_id, actor, reason, not_typed,
            reached: reached.clone(), notified: notify.clone() })?;
        let mut events = vec![cancel.clone()];
        for recipient_id in &held {
            events.push(self.append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, message_id, recipient_id: *recipient_id,
                status: RoomDeliveryStatus::Held, bytes_written: 0, zero_write_confirmed: false,
                error: Some(note.clone()), run_id: None, generation: None, pointer: None, readmit: false,
                already_read: false })?);
        }
        Ok((cancel.cursor, false, events, held, reached, notify))
    }

    /// v3 W5: the stored disposition of `message_id` for the guest `guest_id`.
    pub(crate) fn guest_ack_for(&self, message_id: Uuid, guest_id: Uuid) -> Option<StoredGuestAck> {
        self.post_by_message_id(message_id).and_then(|post| post.guest_acks.get(&guest_id).cloned())
    }

    /// v3 W5: ACK-tracked messages addressed to `guest_id` after its floor
    /// (`floor_epoch`, `floor`) that it has neither acknowledged nor declined,
    /// oldest first.
    pub(crate) fn guest_owed(&self, guest_id: Uuid, floor_epoch: Uuid, floor: RoomSequence) -> Vec<RoomFeedEvent> {
        let mut owed: Vec<_> = self.posts.values().filter(|post| {
            let RoomFeedItem::Message { guest_recipient_ids, .. } = &post.event.item else { return false };
            self.ack_tracked(&post.event) && post.cancel.is_none() && guest_recipient_ids.contains(&guest_id)
                && post.event.cursor.epoch == floor_epoch && post.event.cursor.sequence > floor
                && !post.guest_acks.contains_key(&guest_id)
        }).map(|post| post.event.clone()).collect();
        owed.sort_by_key(|event| event.cursor.sequence);
        owed
    }

    /// v3 W5 (Reviewer 3b9fa7b2): a bounded page of `guest_owed`, bodies
    /// included, out of the event window or in it, as `retained_for` is for
    /// members. Starts after `after` (an `owed_after` from this room's feed
    /// epoch, never ahead of it), oldest first, under the same page bounds;
    /// the second value is where to continue when more remain. No receipts:
    /// a guest is never a delivery recipient.
    pub(crate) fn guest_retained_for(&self, guest_id: Uuid, floor_epoch: Uuid, floor: RoomSequence,
        after: Option<RoomFeedCursor>) -> Result<(Vec<RoomFeedEvent>, Option<RoomFeedCursor>)> {
        if let Some(after) = after {
            if after.epoch != self.feed_epoch {
                return Err(anyhow!("owed_after belongs to another history of this room; read again without it"));
            }
            if after.sequence >= self.next_sequence {
                return Err(anyhow!("owed_after is ahead of this room's feed; read again without it"));
            }
        }
        let (mut retained, mut bytes) = (Vec::new(), 0usize);
        for event in self.guest_owed(guest_id, floor_epoch, floor).into_iter()
            .filter(|event| after.is_none_or(|after| event.cursor.sequence > after.sequence)) {
            let size = serde_json::to_vec(&event).map_err(|error| anyhow!("failed to measure a retained message: {error}"))?.len();
            if !retained.is_empty() && (retained.len() >= ROOM_FEED_PAGE_MAX_EVENTS
                || bytes.saturating_add(size) > ROOM_FEED_PAGE_MAX_BYTES) {
                let last = retained.last().map(|event: &RoomFeedEvent| event.cursor);
                return Ok((retained, last));
            }
            bytes += size;
            retained.push(event);
        }
        Ok((retained, None))
    }

    /// v3 W5: store a disposition of `message_id` for the guest `guest_id`,
    /// by the guest itself, the operator or a local process. The message
    /// must be retained and addressed to that guest; the caller checks the
    /// guest's room, floor and revocation. The same request again returns the
    /// stored one; a different second disposition is refused.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_guest_ack(&mut self, request_id: String, message_id: Uuid, guest_id: Uuid,
        actor: shared_types::RoomGuestAckActor, disposition: shared_types::RoomAckDisposition,
        note: String, outcome_ref: Option<String>) -> Result<(RoomFeedCursor, bool)> {
        let post = self.post_by_message_id(message_id)
            .ok_or_else(|| anyhow!("message {message_id} is not in this room's retained history"))?;
        let RoomFeedItem::Message { guest_recipient_ids, guest_recipient_labels, .. } = &post.event.item else { unreachable!() };
        let Some(index) = guest_recipient_ids.iter().position(|id| *id == guest_id) else {
            return Err(anyhow!("message {message_id} was not addressed to this guest"));
        };
        let guest_label = guest_recipient_labels.get(index).cloned().unwrap_or_default();
        if let Some(existing) = post.guest_acks.get(&guest_id) {
            if existing.request_id == request_id && same_guest_ack_actor(&existing.actor, &actor)
                && existing.disposition == disposition && existing.note == note && existing.outcome_ref == outcome_ref {
                return Ok((existing.cursor, true));
            }
            return Err(anyhow!("message {message_id} already has a disposition for this guest \
                (stored at sequence {}); a different second one is refused", existing.cursor.sequence));
        }
        let reused = self.posts.values().any(|post| post.guest_acks.iter().any(|(target, ack)|
            ack.request_id == request_id && same_guest_ack_actor(&ack.actor, &actor)
            && (*target != guest_id || !matches!(&post.event.item,
                RoomFeedItem::Message { message_id: id, .. } if *id == message_id))));
        if reused {
            return Err(anyhow!("request_id {request_id} already names another disposition; use a fresh one"));
        }
        let event = self.append(RoomFeedItem::GuestAck { request_id, message_id, guest_id, guest_label,
            actor, disposition, note, outcome_ref })?;
        Ok((event.cursor, false))
    }

    /// v3 W3: the stored disposition of `message_id` for `recipient`.
    pub(crate) fn ack_for(&self, message_id: Uuid, recipient: SessionId) -> Option<StoredAck> {
        self.post_by_message_id(message_id).and_then(|post| post.acks.get(&recipient).cloned())
    }

    /// v3 W5: the guests `message_id` addressed, if the index retains it.
    pub(crate) fn guest_recipients_of(&self, message_id: Uuid) -> Option<Vec<Uuid>> {
        self.post_by_message_id(message_id).and_then(|post| match &post.event.item {
            RoomFeedItem::Message { guest_recipient_ids, .. } => Some(guest_recipient_ids.clone()),
            _ => None,
        })
    }

    /// v3 W5: where `message_id` sits in the feed, if the index retains it.
    pub(crate) fn message_cursor(&self, message_id: Uuid) -> Option<RoomFeedCursor> {
        self.post_by_message_id(message_id).map(|post| post.event.cursor)
    }

    /// Whether this room's retained index holds `message_id`.
    pub(crate) fn contains_message(&self, message_id: Uuid) -> bool {
        self.post_by_message_id(message_id).is_some()
    }

    fn post_by_message_id(&self, message_id: Uuid) -> Option<&StoredPost> {
        self.posts.values().find(|post| matches!(&post.event.item,
            RoomFeedItem::Message { message_id: id, .. } if *id == message_id))
    }

    /// v3 W3: the addressed message `message_id` as `recipient` may act on
    /// it: retained, addressed to it, inside its current membership.
    pub(crate) fn addressed_message(&self, message_id: Uuid, recipient: SessionId) -> Result<RoomFeedEvent> {
        self.addressed_message_within(message_id, recipient, true)
    }

    /// `member_epoch`: the message must also follow the recipient's current
    /// join floor (its own read or ACK). An operator disposition validates the
    /// original addressing and retention only (Reviewer 5f475977).
    fn addressed_message_within(&self, message_id: Uuid, recipient: SessionId, member_epoch: bool) -> Result<RoomFeedEvent> {
        self.ensure_available()?;
        let post = self.post_by_message_id(message_id)
            .ok_or_else(|| anyhow!("message {message_id} is not retained in this room"))?;
        let RoomFeedItem::Message { recipient_ids, .. } = &post.event.item else { unreachable!() };
        if !recipient_ids.contains(&recipient) {
            return Err(anyhow!("message {message_id} was not addressed to this member"));
        }
        if member_epoch && post.event.cursor.sequence <= self.join_floor_by_session.get(&recipient).copied().unwrap_or(0) {
            return Err(anyhow!("message {message_id} predates this member's current room membership"));
        }
        Ok(post.event.clone())
    }

    /// v3 W3: messages addressed to `recipient` in this room that are eligible
    /// wake work: not read (no Fetched), not disposed of, and not Held or
    /// Failed (held history stays inspectable with its reason, but is never
    /// counted as new work), oldest first, ACK-tracked only: what a pointer
    /// wake counts.
    pub(crate) fn unread_for(&self, recipient: SessionId) -> Vec<RoomFeedEvent> {
        let floor = self.join_floor_by_session.get(&recipient).copied().unwrap_or(0);
        let mut found: Vec<_> = self.posts.values().filter(|post| {
            let RoomFeedItem::Message { recipient_ids, .. } = &post.event.item else { return false };
            self.pointer_tracked(&post.event) && post.event.cursor.sequence > floor && post.cancel.is_none()
                && recipient_ids.contains(&recipient) && !post.acks.contains_key(&recipient)
                && (post.readmit_pending(recipient)
                    || !post.receipts.get(&recipient).is_some_and(|stored| matches!(stored.receipt.status,
                        RoomDeliveryStatus::Fetched | RoomDeliveryStatus::Held | RoomDeliveryStatus::Failed)))
        }).map(|post| post.event.clone()).collect();
        found.sort_by_key(|event| event.cursor.sequence);
        found
    }

    /// v3 W3: what `reader` still owes in this room — ACK-tracked messages
    /// addressed to it, not disposed of, inside its membership — whether or
    /// not it has read them or their wake is held.
    fn owed_by(&self, reader: SessionId) -> Vec<&StoredPost> {
        let floor = self.join_floor_by_session.get(&reader).copied().unwrap_or(0);
        let mut owed: Vec<_> = self.posts.values().filter(|post| {
            let RoomFeedItem::Message { recipient_ids, .. } = &post.event.item else { return false };
            self.ack_tracked(&post.event) && post.event.cursor.sequence > floor && post.cancel.is_none()
                && recipient_ids.contains(&reader) && !post.acks.contains_key(&reader)
        }).collect();
        owed.sort_by_key(|post| post.event.cursor.sequence);
        owed
    }

    /// v3 W3: store `recipient`'s disposition of `message_id`. The same
    /// request again returns the stored one; any different second
    /// disposition for that recipient is a conflict, never a replacement.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_ack(&mut self, request_id: String, message_id: Uuid, recipient: SessionId,
        actor: shared_types::RoomAckActor, disposition: shared_types::RoomAckDisposition,
        note: String, outcome_ref: Option<String>) -> Result<(RoomFeedCursor, bool)> {
        let own = matches!(actor, shared_types::RoomAckActor::Session { .. });
        self.addressed_message_within(message_id, recipient, own)?;
        // A member acknowledges only while it is in the room; the operator
        // (UI or entry) may dispose for a recipient that has left — the only
        // way that open obligation is ever cleared.
        if matches!(actor, shared_types::RoomAckActor::Session { .. })
            && !self.definition.member_ids.contains(&recipient) {
            return Err(anyhow!("this member is no longer in the room"));
        }
        if let Some(existing) = self.ack_for(message_id, recipient) {
            if existing.request_id == request_id && same_ack_actor(&existing.actor, &actor) && existing.disposition == disposition
                && existing.note == note && existing.outcome_ref == outcome_ref {
                return Ok((existing.cursor, true));
            }
            return Err(anyhow!("message {message_id} already has a disposition for this member \
                (stored at sequence {}); a different second one is refused", existing.cursor.sequence));
        }
        // A request_id names one disposition by one actor: reusing it for
        // another message or recipient is a conflict, never a second ACK.
        let reused = self.posts.values().any(|post| post.acks.iter().any(|(target, ack)|
            ack.request_id == request_id && same_ack_actor(&ack.actor, &actor)
            && (*target != recipient || !matches!(&post.event.item,
                RoomFeedItem::Message { message_id: id, .. } if *id == message_id))));
        if reused {
            return Err(anyhow!("request_id {request_id} already names another disposition; use a fresh one"));
        }
        let event = self.append(RoomFeedItem::Ack { request_id, message_id, recipient_id: recipient,
            actor, disposition, note, outcome_ref })?;
        Ok((event.cursor, false))
    }

    /// v3 (Claude PRIM-1, ac9f860 walk): PRIM-1 sent `member` the newer
    /// continuation `newer`; each older PRIM-1 continuation (upgrade or
    /// recovery) that member still owes is disposed by PRIM-1, "superseded by
    /// <newer>", so only the newest stays open. Returns the new Ack events.
    pub(crate) fn supersede_continuations(&mut self, member: SessionId, newer: Uuid) -> Result<Vec<RoomFeedEvent>> {
        let Some(newest) = self.post_by_message_id(newer).map(|post| post.event.cursor.sequence) else { return Ok(Vec::new()) };
        let mut older: Vec<(RoomSequence, Uuid)> = self.posts.values().filter_map(|post| match &post.event.item {
            RoomFeedItem::Message { message_id, request_id, sender: RoomMessageSender::Prim1 {}, recipient_ids, .. }
                if post.event.cursor.sequence < newest && recipient_ids.contains(&member)
                    && (request_id.starts_with("continuation-") || request_id.starts_with("upgrade-"))
                    && self.awaits_ack(post) && !post.acks.contains_key(&member) =>
                Some((post.event.cursor.sequence, *message_id)),
            _ => None,
        }).collect();
        older.sort_unstable();
        let mut events = Vec::new();
        for (_, message_id) in older {
            let (cursor, replayed) = self.record_ack(format!("supersede-{message_id}-{member}"), message_id, member,
                shared_types::RoomAckActor::Prim1 {}, shared_types::RoomAckDisposition::Declined,
                format!("superseded by {newer}"), None)?;
            if !replayed { events.extend(self.event_at(cursor)); }
        }
        Ok(events)
    }

    /// The retained feed event at `cursor`, if the feed still holds it.
    pub(crate) fn event_at(&self, cursor: RoomFeedCursor) -> Option<RoomFeedEvent> {
        self.feed.iter().rev().find(|(event, _)| event.cursor == cursor).map(|(event, _)| event.clone())
    }

    /// v3 W3: the latest deferral code recorded for this recipient, if no
    /// transport outcome superseded it.
    pub(crate) fn waiting_code(&self, event: &RoomFeedEvent, recipient: SessionId) -> Option<String> {
        let RoomFeedItem::Message { request_id, sender, .. } = &event.item else { return None };
        self.posts.get(&post_key(sender, request_id)).and_then(|post| post.waiting.get(&recipient).cloned())
    }
}

pub(crate) struct RoomRuntime {
    log: Option<crate::room_log::RoomLog>,
    recovery_error: Option<String>,
    startup_sync_failure: Option<crate::room_log::StartupSyncFailed>,
    history_replay_notified: bool,
    writer_recovery: Option<shared_types::RoomWriterRecovery>,
    writer_recovery_notified: Option<shared_types::RoomWriterRecovery>,
    posts: HashMap<PostKey, StoredPost>,
    post_order: VecDeque<PostKey>,
    post_bytes: usize,
    cards: CardIndex,
    pub(crate) definition: PersistedRoomV1,
    pub(crate) feed_epoch: Uuid,
    feed: VecDeque<(RoomFeedEvent, usize)>,
    feed_bytes: usize,
    pub(crate) next_sequence: RoomSequence,
    pub(crate) join_floor_by_session: HashMap<SessionId, RoomSequence>,
    pub(crate) in_flight_deliveries: usize,
    pub(crate) receipt_reservations: usize,
    receipt_evidence: Vec<ReceiptEvidence>,
    /// Tests: fail the covered-request Pending receipt after this many more
    /// succeed (then clear).
    #[cfg(test)]
    pub(crate) fail_cover_appends_after_for_tests: Option<usize>,
    /// v3 W3b test seam: the next SubmitAttempt append fails.
    #[cfg(test)]
    pub(crate) fail_submit_attempt_for_tests: bool,
}

include!("room_receipt_evidence.rs");
include!("room_cards.rs");

struct StoredPost {
    event: RoomFeedEvent,
    bytes: usize,
    receipts: HashMap<SessionId, StoredReceipt>,
    written: HashSet<SessionId>,
    /// v3 W3: each recipient's first stored disposition; never replaced.
    acks: HashMap<SessionId, StoredAck>,
    /// v3 W5: each addressed guest's first stored disposition; never replaced.
    guest_acks: HashMap<Uuid, StoredGuestAck>,
    /// v3 W3 (D6): the operator's withdrawal; the first one stands.
    cancel: Option<StoredCancel>,
    /// v3 W3: the latest deferral code per recipient until a transport
    /// outcome supersedes it.
    waiting: HashMap<SessionId, String>,
    /// v3 W3: the latest hold reason per recipient until a write supersedes it.
    held: HashMap<SessionId, String>,
    /// v3 W3: a confirmed-zero transient refusal (its completion's cursor and
    /// the attempts so far): while that Failed is the latest receipt, the wake
    /// is re-admitted by the queue. Rebuilt from the completions on replay.
    readmit: HashMap<SessionId, (RoomFeedCursor, u8)>,
    /// v3 W3b: the latest pointer attempt recipe per recipient (head and
    /// covered requests alike), and the attempts observed as submitted.
    recipes: HashMap<SessionId, SubmitRecipe>,
    observed: HashMap<SessionId, RoomFeedCursor>,
}

impl StoredPost {
    /// The latest receipt, while it belongs to this recipe's attempt (at or
    /// after its token) and leaves the submission open: Pending (also a
    /// crash after the bytes and before any outcome, Supervisor dbb04667),
    /// Written or Failed. Held (nothing typed), Woken and Fetched settle it;
    /// so do an ACK or an earlier observation.
    fn submission_open(&self, recipient: SessionId, recipe: &SubmitRecipe) -> Option<RoomFeedCursor> {
        if self.acks.contains_key(&recipient) || self.observed.get(&recipient) == Some(&recipe.attempt) {
            return None;
        }
        self.receipts.get(&recipient).filter(|stored| matches!(stored.receipt.status,
                RoomDeliveryStatus::Pending | RoomDeliveryStatus::Written | RoomDeliveryStatus::Failed)
            && stored.cursor.epoch == recipe.attempt.epoch && stored.cursor.sequence >= recipe.attempt.sequence)
            .map(|stored| stored.cursor)
    }
}

/// v3 W3: the request identity of an ACK. A local process's label names
/// its process id, which differs per CLI invocation: two operator-entry
/// processes sending the same request are the same actor for replay
/// (Reviewer 460f1b3f); the stored record keeps the first one's label.
fn same_ack_actor(a: &shared_types::RoomAckActor, b: &shared_types::RoomAckActor) -> bool {
    use shared_types::RoomAckActor::LocalProcess;
    matches!((a, b), (LocalProcess { .. }, LocalProcess { .. })) || a == b
}

/// v3 W5: as `same_ack_actor`, for a guest's disposition.
fn same_guest_ack_actor(a: &shared_types::RoomGuestAckActor, b: &shared_types::RoomGuestAckActor) -> bool {
    use shared_types::RoomGuestAckActor::LocalProcess;
    matches!((a, b), (LocalProcess { .. }, LocalProcess { .. })) || a == b
}

/// v3 W3b: a submission recipe names a member's conversation and folder, and
/// an observation's locator names its record file. Outside the operator's own
/// read, a recipe reaches only its member and a locator is blanked for anyone
/// else; the observed fact itself reaches every reader, so a sender sees its
/// earlier uncertainty end (Supervisor dbb04667). A guest's `reader` is None.
pub(crate) fn redact_submission_records(page: &mut shared_types::RoomFeedPage, reader: Option<SessionId>) {
    page.events.retain(|event| !matches!(&event.item,
        RoomFeedItem::SubmitAttempt { recipient_id, .. } if Some(*recipient_id) != reader));
    for event in &mut page.events {
        if let RoomFeedItem::InputObserved { recipient_id, locator, .. } = &mut event.item
            && Some(*recipient_id) != reader {
            locator.clear();
        }
    }
}

/// v3 W3b: one pointer attempt, as its SubmitAttempt record states it.
#[derive(Clone, Debug)]
pub(crate) struct SubmitRecipe {
    pub(crate) attempt: RoomFeedCursor,
    pub(crate) typed: String,
    pub(crate) driver: shared_types::DriverKind,
    pub(crate) conversation_id: String,
    pub(crate) working_dir: String,
    /// When the attempt began (the recipe's own timestamp, RFC 3339).
    pub(crate) started_at: String,
}

/// v3 W3: confirmed-zero transient refusals re-admitted per message and recipient.
pub(crate) const ROOM_WAKE_READMIT_MAX: u8 = 3;
/// The waiting code shown while a re-admitted wake waits for quiet.
pub(crate) const ROOM_WAKE_READMIT_CODE: &str = "output_settling";

impl StoredPost {
    fn readmit_pending(&self, recipient: SessionId) -> bool {
        self.readmit.get(&recipient).is_some_and(|(cursor, attempts)| *attempts <= ROOM_WAKE_READMIT_MAX
            && self.receipts.get(&recipient).is_some_and(|stored| stored.cursor == *cursor))
    }
}

/// v3 W3 (D6): a withdrawal as stored.
#[derive(Clone)]
pub(crate) struct StoredCancel {
    pub(crate) request_id: String,
    pub(crate) actor: shared_types::RoomAckActor,
    pub(crate) reason: String,
    pub(crate) cursor: RoomFeedCursor,
    pub(crate) not_typed: Vec<SessionId>,
    pub(crate) reached: Vec<SessionId>,
    pub(crate) notified: Vec<SessionId>,
}

/// v3 W5: a guest's disposition as stored.
#[derive(Clone)]
pub(crate) struct StoredGuestAck {
    pub(crate) request_id: String,
    pub(crate) actor: shared_types::RoomGuestAckActor,
    pub(crate) disposition: shared_types::RoomAckDisposition,
    pub(crate) note: String,
    pub(crate) outcome_ref: Option<String>,
    pub(crate) cursor: RoomFeedCursor,
}

#[derive(Clone)]
pub(crate) struct StoredAck {
    pub(crate) request_id: String,
    pub(crate) actor: shared_types::RoomAckActor,
    pub(crate) disposition: shared_types::RoomAckDisposition,
    pub(crate) note: String,
    pub(crate) outcome_ref: Option<String>,
    pub(crate) cursor: RoomFeedCursor,
}

struct StoredReceipt {
    receipt: RoomDeliveryReceipt,
    cursor: RoomFeedCursor,
    zero_write_confirmed: bool,
}

/// Full-replay proof for one failed Message append. Receipt absence in the
/// bounded feed/dedup maps is insufficient: observe from the header onward.
struct UncertainMessageProof {
    message_id: Uuid,
    cursor: RoomFeedCursor,
    recipients: Option<Vec<SessionId>>,
    receipted: HashSet<SessionId>,
}

impl UncertainMessageProof {
    fn observe(&mut self, event: &RoomFeedEvent, proven_receipt_repair: bool) -> Result<()> {
        if event.cursor == self.cursor {
            anyhow::ensure!(matches!(&event.item, RoomFeedItem::Message { message_id, .. }
                if *message_id == self.message_id) || proven_receipt_repair,
                "uncertain Message identity changed during replay");
            // Only a torn Message leaves its cursor reusable. A prior private
            // repair may occupy it after that repair's sync failed. Require an
            // exact retained receipt witness, never just any Delivery shape.
        }
        match &event.item {
            RoomFeedItem::Message { message_id, recipient_ids, .. } if *message_id == self.message_id => {
                anyhow::ensure!(event.cursor == self.cursor && self.recipients.is_none(),
                    "uncertain Message occurs at an unexpected replay boundary");
                anyhow::ensure!(recipient_ids.len() <= ROOM_MEMBER_MAX_COUNT
                    && recipient_ids.iter().collect::<HashSet<_>>().len() == recipient_ids.len(),
                    "uncertain Message targets are invalid");
                self.recipients = Some(recipient_ids.clone());
            }
            RoomFeedItem::Delivery { message_id, recipient_id, .. } if *message_id == self.message_id => {
                self.receipted.insert(*recipient_id);
                anyhow::ensure!(self.receipted.len() <= ROOM_MEMBER_MAX_COUNT,
                    "uncertain Message receipt proof exceeds its bound");
            }
            _ => {}
        }
        Ok(())
    }
}

/// Owned private replay state. The original locked writer and acknowledged
/// prefix stay in the room registry until this candidate is durable and valid.
pub(crate) struct RoomRecoveryWork {
    candidate: RoomRuntime,
    log: crate::room_log::RoomLog,
    path: PathBuf,
    original_epoch: Uuid,
    original_next_sequence: RoomSequence,
    current_runs: HashMap<SessionId, (Uuid, shared_types::SessionGeneration)>,
    receipt_evidence: Vec<ReceiptEvidence>,
}

pub(crate) struct PreparedRoomRecovery {
    candidate: RoomRuntime,
    log: crate::room_log::RoomLog,
    original_epoch: Uuid,
    original_next_sequence: RoomSequence,
    recovered: Vec<RoomFeedEvent>,
    receipt_evidence: Vec<ReceiptEvidence>,
}

/// Retry only the fence on the original still-locked startup writer. Parsing
/// and normalized membership remain held until this proof and catalog commit.
pub(crate) struct RoomStartupSyncWork {
    pub(crate) definition: PersistedRoomV1,
    epoch: Uuid,
    next_sequence: RoomSequence,
    sync: crate::room_log::StartupSyncWork,
    confirmed: bool,
}

impl RoomStartupSyncWork {
    pub(crate) fn run(&mut self) -> Result<()> {
        self.sync.run()?;
        self.confirmed = true;
        Ok(())
    }
}

impl RoomRecoveryWork {
    /// Run inside the existing blocking worker, outside the rooms registry.
    pub(crate) fn run(self) -> Result<PreparedRoomRecovery> {
        let Self { mut candidate, mut log, path, original_epoch, original_next_sequence, current_runs, receipt_evidence } = self;
        let mut proof = log.uncertain_message().map(|(message_id, cursor)| UncertainMessageProof {
            message_id, cursor, recipients: None, receipted: HashSet::new(),
        });
        let mut receipt_proofs: Vec<_> = receipt_evidence.iter().cloned().map(|evidence| ReceiptEvidenceProof {
            evidence, message_seen: false, pending_seen: false, fetched: false, later_receipt: false, terminal_seen: false,
        }).collect();
        // Keep the entire newly recovered delta independently of feed eviction.
        // One failed append/batch plus <=128 repair receipts fits these bounds;
        // an unexpected suffix stays held instead of allocating without limit.
        let mut recovered = Vec::new();
        let mut recovered_bytes = 0usize;
        log.replay_poisoned(&path, candidate.definition.room_id, |record| {
            if let crate::room_log::ReplayRecord::Event(event) = &record {
                for proof in &mut receipt_proofs { proof.observe(event).map_err(structural_history)?; }
                if let Some(proof) = &mut proof {
                    let repaired = receipt_proofs.iter().any(|receipt| receipt.matches_repair(event));
                    proof.observe(event, repaired).map_err(structural_history)?;
                }
                if event.cursor.sequence >= original_next_sequence {
                    recovered_bytes = recovered_bytes.saturating_add(serde_json::to_vec(event)?.len());
                    if recovered.len() >= 3 * ROOM_MEMBER_MAX_COUNT + 1
                        || recovered_bytes > 2 * ROOM_FEED_MAX_BYTES {
                        return Err(structural_history("uncertain recovery suffix exceeds its bound"));
                    }
                    recovered.push(event.clone());
                }
            }
            candidate.apply_replay_record(record).map_err(structural_history)
        })?;
        if candidate.feed_epoch != original_epoch
            || candidate.definition.join_floor_epoch != Some(candidate.feed_epoch)
            || candidate.next_sequence < original_next_sequence
            || candidate.join_floor_by_session.values().any(|floor| *floor >= candidate.next_sequence) {
            return Err(structural_history("room history recovery cannot change its committed epoch or membership boundary"));
        }
        let mut repairs = Vec::new();
        if let Some(proof) = proof {
            if let Some(recipients) = proof.recipients {
                if proof.cursor.epoch != original_epoch || proof.cursor.sequence < original_next_sequence {
                    return Err(structural_history("uncertain Message is outside the unpublished recovery boundary"));
                }
                for recipient in recipients {
                    if proof.receipted.contains(&recipient)
                        || !candidate.definition.member_ids.contains(&recipient)
                        || proof.cursor.sequence <= candidate.join_floor_by_session.get(&recipient).copied().unwrap_or(0) {
                        continue;
                    }
                    let binding = current_runs.get(&recipient).copied();
                    candidate.ensure_sequence_capacity(repairs.len() + 1)?;
                    repairs.push(candidate.prepare_append_at(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, readmit: false, pointer: None,
                        message_id: proof.message_id, recipient_id: recipient,
                        status: RoomDeliveryStatus::Held, bytes_written: 0, zero_write_confirmed: true,
                        error: Some("Wake not attempted; message recovered after an uncertain commit. Retry wake explicitly where supported, or use room_read.".into()),
                        run_id: binding.map(|(run, _)| run), generation: binding.map(|(_, generation)| generation),
                    }, candidate.next_sequence + repairs.len() as RoomSequence)?);
                }
            }
        }
        for proof in receipt_proofs {
            if let Some(item) = proof.repair(&candidate).map_err(structural_history)? {
                candidate.ensure_sequence_capacity(repairs.len() + 1)?;
                repairs.push(candidate.prepare_append_at(item, candidate.next_sequence + repairs.len() as RoomSequence)?);
            }
        }
        candidate.ensure_sequence_capacity(repairs.len())?;
        let events: Vec<_> = repairs.iter().map(|prepared| prepared.event.clone()).collect();
        log.append_recovery_batch(&events).map_err(|error| crate::room_log::RecoveryRequired(
            format!("room history recovery receipts remain held: {error:#}")))?;
        for repair in repairs { recovered.push(candidate.apply_append(repair)); }
        // No installation (including dedup publication or hint clearing) until
        // both replay and all repair receipts have passed their durability fence.
        Ok(PreparedRoomRecovery { candidate, log, original_epoch,
            original_next_sequence, recovered, receipt_evidence })
    }
}

pub(crate) struct PreparedRoomFeedEvent {
    event: RoomFeedEvent,
    bytes: usize,
}

#[derive(Debug)]
pub(crate) struct RequestConflict;
impl std::fmt::Display for RequestConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("request_id conflict: this sender already stored this key with different content or recipients")
    }
}
impl std::error::Error for RequestConflict {}

/// Whose request_id namespace a post or card belongs to. A guest's key is its
/// id, never its label, so a renamed guest's retry still replays.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum SenderKey {
    Operator,
    Session(SessionId),
    Guest(Uuid),
    LocalProcess,
    Prim1,
}

type PostKey = (SenderKey, String);

fn post_key(sender: &RoomMessageSender, request_id: &str) -> PostKey {
    let sender = match sender {
        RoomMessageSender::Operator {} => SenderKey::Operator,
        RoomMessageSender::Session { session_id } => SenderKey::Session(*session_id),
        RoomMessageSender::Guest { guest_id, .. } => SenderKey::Guest(*guest_id),
        RoomMessageSender::LocalProcess { .. } => SenderKey::LocalProcess,
        RoomMessageSender::Prim1 {} => SenderKey::Prim1,
    };
    (sender, request_id.to_owned())
}

impl RoomRuntime {
    pub(crate) fn from_persisted(definition: PersistedRoomV1) -> Self {
        let join_floor_by_session = definition.join_floors.clone().unwrap_or_else(|| definition
            .member_ids
            .iter()
            .copied()
            .map(|session_id| (session_id, 0))
            .collect());
        Self {
            log: None,
            recovery_error: None,
            startup_sync_failure: None,
            history_replay_notified: false,
            writer_recovery: None,
            writer_recovery_notified: None,
            posts: HashMap::new(),
            post_order: VecDeque::new(),
            post_bytes: 0,
            cards: CardIndex::default(),
            definition,
            feed_epoch: Uuid::new_v4(),
            feed: VecDeque::new(),
            feed_bytes: 0,
            next_sequence: 1,
            join_floor_by_session,
            in_flight_deliveries: 0,
            receipt_reservations: 0,
            receipt_evidence: Vec::with_capacity(ROOM_RECEIPT_EVIDENCE_MAX),
            #[cfg(test)]
            fail_cover_appends_after_for_tests: None,
            #[cfg(test)]
            fail_submit_attempt_for_tests: false,
        }
    }

    pub(crate) fn open_log(&mut self, runtime_dir: &Path) -> Result<()> {
        self.open_log_with_sync(runtime_dir, |file| file.sync_all())
    }

    pub(crate) fn open_log_with_sync(&mut self, runtime_dir: &Path,
        final_sync: impl FnMut(&std::fs::File) -> std::io::Result<()>,
    ) -> Result<()> {
        let path = runtime_dir.join("rooms").join(format!("{}.jsonl", self.definition.room_id));
        let result = self.replay_log(runtime_dir, final_sync)
            .with_context(|| format!("room {} history {}", self.definition.room_id, path.display()));
        if let Err(error) = &result {
            // Do not turn a partially replayed prefix into a replacement feed.
            // Keep the original file and any validated header epoch, but refuse
            // reads and mutations held. A typed final-sync failure alone keeps
            // the validated, still-locked writer for a lazy fence retry.
            if !error.is::<crate::room_log::StartupSyncFailed>() {
                self.log = None;
                self.startup_sync_failure = None;
            }
            let reason = if self.startup_sync_pending() {
                "Room startup durability fence is held; retry room access"
            } else { "Room history needs recovery" };
            self.recovery_error = Some(format!("{reason}: {error:#}")
                .chars().take(1024).collect());
        }
        result
    }

    pub(crate) fn startup_sync_pending(&self) -> bool { self.startup_sync_failure.is_some() }

    pub(crate) fn prepare_startup_sync(&mut self) -> Result<Option<RoomStartupSyncWork>> {
        if !self.startup_sync_pending() { return Ok(None); }
        let log = self.log.as_mut().expect("startup fence hold retains its locked writer");
        anyhow::ensure!(!log.needs_replay(), "startup fence cannot cover a changed writer");
        Ok(Some(RoomStartupSyncWork { definition: self.definition.clone(), epoch: self.feed_epoch,
            next_sequence: self.next_sequence, sync: log.prepare_startup_sync()?, confirmed: false }))
    }

    pub(crate) fn validate_startup_sync(&self, work: &RoomStartupSyncWork) -> Result<()> {
        anyhow::ensure!(work.confirmed && self.startup_sync_pending()
            && self.definition == work.definition && self.feed_epoch == work.epoch
            && self.next_sequence == work.next_sequence
            && self.definition.join_floor_epoch == Some(work.epoch)
            && self.definition.join_floors.as_ref() == Some(&self.join_floor_by_session)
            && self.log.as_ref().is_some_and(|log| log.epoch == work.epoch && !log.needs_replay()),
            "startup history boundary changed while its durability fence was pending");
        Ok(())
    }

    /// The caller has durably persisted this exact normalized definition.
    pub(crate) fn finish_startup_sync(&mut self, work: RoomStartupSyncWork) -> Result<()> {
        self.validate_startup_sync(&work)?;
        self.startup_sync_failure = None;
        self.recovery_error = None;
        Ok(())
    }

    pub(crate) fn note_startup_sync_failure(&mut self, error: &anyhow::Error) {
        if self.startup_sync_pending() {
            self.recovery_error = Some(format!("Room startup durability fence remains held; retry room access: {error:#}")
                .chars().take(1024).collect());
        }
    }

    #[cfg(test)]
    pub(crate) fn fail_next_commit_for_tests(&mut self, after_write: bool) {
        self.log.as_mut().expect("room log must be open").fail_commit_for_tests(after_write);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_replay_sync_for_tests(&mut self) {
        self.log.as_mut().expect("room log must be open").fail_replay_sync_for_tests();
    }

    #[cfg(test)]
    pub(crate) fn before_replay_io_for_tests(&mut self, hook: impl FnOnce() + Send + 'static) {
        self.log.as_mut().expect("room log must be open").before_replay_io_for_tests(hook);
    }

    #[cfg(test)]
    pub(crate) fn append_invalid_complete_record_for_tests(&mut self) {
        self.log.as_mut().expect("room log must be open").append_invalid_complete_record_for_tests();
    }

    #[cfg(test)]
    pub(crate) fn read_owned_log_for_tests(&mut self) -> Vec<u8> {
        self.log.as_mut().expect("room log must be open").read_owned_for_tests()
    }

    #[cfg(test)]
    pub(crate) fn exhaust_log_quota_for_tests(&mut self) {
        self.log.as_mut().expect("room log must be open").exhaust_quota_for_tests();
    }

    #[cfg(test)]
    pub(crate) fn set_log_quotas_for_tests(&mut self, ordinary: u64, hard: u64) {
        self.log.as_mut().expect("room log must be open").set_quota_for_tests(ordinary, hard);
    }

    /// Room save (A92): the acknowledged log prefix through the owned writer.
    /// A failed position restore leaves the writer held for replay.
    pub(crate) fn read_committed_log(&mut self) -> Result<Vec<u8>> {
        self.ensure_available()?;
        self.ensure_writer_available()?;
        self.log.as_mut().ok_or_else(|| anyhow!("room history is not open"))?.read_committed_prefix()
    }

    #[cfg(test)]
    pub(crate) fn fail_next_log_copy_for_tests(&mut self, restore_fails: bool) {
        self.log.as_mut().expect("room log must be open").fail_prefix_read_for_tests(restore_fails);
    }

    pub(crate) fn writer_needs_replay(&self) -> bool {
        self.log.as_ref().is_some_and(|log| log.needs_replay())
    }

    /// Recover only an uncertain writer we still own, never a startup-corrupt
    /// or replaced history. A fresh candidate prevents partial replay from
    /// changing returned cursors, dedup state, receipts or authorization floors.
    #[cfg(test)]
    pub(crate) fn recover_poisoned(&mut self, runtime_dir: &Path) -> Result<Option<Vec<RoomFeedEvent>>> {
        self.recover_poisoned_with_runs(runtime_dir, &HashMap::new())
    }

    #[cfg(test)]
    pub(crate) fn recover_poisoned_with_runs(&mut self, runtime_dir: &Path,
        current_runs: &HashMap<SessionId, (Uuid, shared_types::SessionGeneration)>,
    ) -> Result<Option<Vec<RoomFeedEvent>>> {
        let result = (|| {
            let Some(work) = self.prepare_recovery(runtime_dir, current_runs.clone())? else { return Ok(None); };
            self.finish_recovery(work.run()?).map(Some)
        })();
        if let Err(error) = &result { self.note_recovery_failure(error); }
        result
    }

    pub(crate) fn prepare_recovery(&mut self, runtime_dir: &Path,
        current_runs: HashMap<SessionId, (Uuid, shared_types::SessionGeneration)>,
    ) -> Result<Option<RoomRecoveryWork>> {
        if !self.writer_needs_replay() { return Ok(None); }
        self.ensure_writer_available()?;
        if self.in_flight_deliveries != 0 {
            return Err(crate::room_log::RecoveryRequired(
                "room history recovery waits for the current delivery to settle".into()).into());
        }
        let mut candidate = Self::from_persisted(self.definition.clone());
        candidate.feed_epoch = Uuid::nil();
        let log = self.log.as_mut().expect("poisoned room owns its writer").clone_for_recovery()?;
        Ok(Some(RoomRecoveryWork { candidate, log,
            path: runtime_dir.join("rooms").join(format!("{}.jsonl", self.definition.room_id)),
            original_epoch: self.feed_epoch, original_next_sequence: self.next_sequence, current_runs, receipt_evidence: self.receipt_evidence.clone() }))
    }

    /// Caller holds publication and rooms. The original committed boundary must
    /// still match; this final swap does no filesystem work and repeats no wake.
    pub(crate) fn finish_recovery(&mut self, prepared: PreparedRoomRecovery) -> Result<Vec<RoomFeedEvent>> {
        let PreparedRoomRecovery { mut candidate, log, original_epoch,
            original_next_sequence, recovered, receipt_evidence } = prepared;
        if !self.writer_needs_replay() || self.in_flight_deliveries != 0
            || self.feed_epoch != original_epoch || self.next_sequence != original_next_sequence
            || self.receipt_reservations != 0 || self.receipt_evidence != receipt_evidence
            || self.definition != candidate.definition
            || self.join_floor_by_session != candidate.join_floor_by_session
            || !self.log.as_ref().is_some_and(|original| original.epoch == log.epoch
                && original.uncertain_message() == log.uncertain_message()) {
            return Err(crate::room_log::RecoveryRequired(
                "room history recovery cannot replace a changed committed boundary".into()).into());
        }
        candidate.history_replay_notified = self.history_replay_notified;
        candidate.writer_recovery_notified = self.writer_recovery_notified.clone();
        let mut original = self.log.take().expect("poisoned room retains its original writer");
        original.accept_recovered_writer(log);
        candidate.log = Some(original);
        *self = candidate;
        Ok(recovered)
    }

    /// An in-process hold applies only to writes. Startup corruption remains a
    /// separate stronger hold; an acknowledged prefix must stay readable here.
    pub(crate) fn note_recovery_failure(&mut self, error: &anyhow::Error) {
        if !self.writer_needs_replay() || self.writer_recovery.as_ref().is_some_and(|hold|
            hold.kind == shared_types::RoomWriterRecoveryKind::Structural) { return; }
        self.writer_recovery = Some(shared_types::RoomWriterRecovery {
            kind: if error.is::<crate::room_log::StructuralHistory>() {
                shared_types::RoomWriterRecoveryKind::Structural
            } else { shared_types::RoomWriterRecoveryKind::Transient },
            reason: format!("{error:#}").chars().take(1024).collect(),
        });
    }

    fn ensure_writer_available(&self) -> Result<()> {
        if let Some(hold) = &self.writer_recovery {
            if hold.kind == shared_types::RoomWriterRecoveryKind::Structural {
                return Err(structural_history(&hold.reason));
            }
        }
        Ok(())
    }

    pub(crate) fn apply_replay_record(&mut self, record: crate::room_log::ReplayRecord) -> Result<()> {
        match record {
            crate::room_log::ReplayRecord::Header(epoch) => self.feed_epoch = epoch,
            crate::room_log::ReplayRecord::Event(event) => {
                self.validate_card_event(&event)?;
                let bytes = serde_json::to_vec(&event)?.len();
                if bytes > ROOM_FEED_MAX_BYTES {
                    return Err(anyhow!("room replay event exceeds the {ROOM_FEED_MAX_BYTES} byte feed bound"));
                }
                self.ensure_sequence_capacity(1)?;
                self.apply_append(PreparedRoomFeedEvent { event, bytes });
            }
        }
        Ok(())
    }

    fn replay_log(&mut self, runtime_dir: &Path,
        final_sync: impl FnMut(&std::fs::File) -> std::io::Result<()>,
    ) -> Result<()> {
        let path = runtime_dir.join("rooms").join(format!("{}.jsonl", self.definition.room_id));
        // An unreadable header has no known epoch. Nil is explicit unknown,
        // not a generated replacement identity for the damaged history.
        self.feed_epoch = Uuid::nil();
        let (log, sync_failure) = crate::room_log::RoomLog::open_with_header_sync(&path, self.definition.room_id,
            |record| self.apply_replay_record(record), final_sync)?;
        self.feed_epoch = log.epoch;
        let replaced_epoch = self.definition.join_floor_epoch
            .is_some_and(|epoch| epoch != log.epoch);
        // A failed first migration may leave a valid header but no bound
        // catalog epoch. With zero records there is no prefix to expose. Keep
        // nonempty shortened logs and already-bound truncations held below.
        let empty_legacy_restart = self.definition.join_floor_epoch.is_none()
            && self.next_sequence == 1
            && self.join_floor_by_session.values().any(|floor| *floor > 0);
        if replaced_epoch || empty_legacy_restart {
            self.definition.history_restarted = true;
        }
        if replaced_epoch || log.created_epoch || empty_legacy_restart {
            // A replacement may contain an imported prefix. Existing members
            // receive only traffic after that prefix; old boundaries cannot
            // authorize history in a different epoch.
            let floor = self.next_sequence.saturating_sub(1);
            self.join_floor_by_session = self.definition.member_ids.iter()
                .map(|id| (*id, floor)).collect();
        }
        if self.join_floor_by_session.values().any(|floor| *floor >= self.next_sequence) {
            return Err(anyhow!("room log is shorter than its persisted membership boundary"));
        }
        self.definition.join_floor_epoch = Some(log.epoch);
        self.definition.join_floors = Some(self.join_floor_by_session.clone());
        self.log = Some(log);
        self.startup_sync_failure = sync_failure;
        if let Some(failure) = &self.startup_sync_failure { return Err(failure.clone().into()); }
        Ok(())
    }

    fn ensure_available(&self) -> Result<()> {
        if self.startup_sync_pending() {
            return Err(crate::room_log::StartupSyncFailed(format!(
                "{}; retry room access to confirm its startup durability fence",
                self.recovery_error.as_deref().unwrap_or("Room startup history remains held"))).into());
        }
        if let Some(error) = &self.recovery_error {
            return Err(anyhow!("{error}; room traffic is held"));
        }
        Ok(())
    }

    /// The key and complete authorized intent are compared before new I/O.
    /// Retrying an acknowledged/lost-reply post never performs another wake.
    pub(crate) fn replayed_post(&self, request_id: &str, sender: &RoomMessageSender,
        content: &str, recipient_ids: &[SessionId]) -> Result<Option<RoomFeedEvent>> {
        self.ensure_available()?;
        if let Some(stored) = self.posts.get(&post_key(sender, request_id)) {
            if let RoomFeedItem::Message { content: old_content,
                recipient_ids: old_recipients, .. } = &stored.event.item {
                let mut old_ids = old_recipients.clone(); old_ids.sort_unstable();
                let mut new_ids = recipient_ids.to_vec(); new_ids.sort_unstable();
                if old_content != content || old_ids != new_ids {
                    return Err(RequestConflict.into());
                }
            } else {
                return Err(RequestConflict.into());
            }
            return Ok(Some(stored.event.clone()));
        }
        Ok(None)
    }

    pub(crate) fn post(
        &mut self, request_id: String, sender: RoomMessageSender, content: String,
        recipient_ids: Vec<SessionId>, membership_revision: RoomRevision,
    ) -> Result<(RoomFeedEvent, bool)> {
        self.post_with_origins(request_id, sender, content, recipient_ids, membership_revision, None, None)
    }

    pub(crate) fn post_with_fallback(&mut self, request_id: String, sender: RoomMessageSender, content: String,
        recipient_ids: Vec<SessionId>, membership_revision: RoomRevision,
        fallback_origin: Option<shared_types::RoomFallbackOrigin>) -> Result<(RoomFeedEvent, bool)> {
        self.post_with_origins(request_id, sender, content, recipient_ids, membership_revision, fallback_origin, None)
    }

    /// v3 (Claude PRIM-1, 15a49bdf): PRIM-1's recovery continuation asks its
    /// member for an ACK, keeping its continuation origin; it is owed and
    /// woken by a pointer like any ACK-tracked request.
    pub(crate) fn post_continuation_requesting_acks(&mut self, request_id: String, sender: RoomMessageSender,
        content: String, recipient_ids: Vec<SessionId>, membership_revision: RoomRevision,
        continuation_origin: shared_types::RoomContinuationOrigin) -> Result<(RoomFeedEvent, bool)> {
        let ack_requested = !recipient_ids.is_empty();
        self.post_message(request_id, sender, content, recipient_ids, membership_revision, None, Some(continuation_origin),
            ack_requested, Vec::new())
    }

    pub(crate) fn post_with_origins(&mut self, request_id: String, sender: RoomMessageSender,
        content: String, recipient_ids: Vec<SessionId>, membership_revision: RoomRevision,
        fallback_origin: Option<shared_types::RoomFallbackOrigin>,
        continuation_origin: Option<shared_types::RoomContinuationOrigin>) -> Result<(RoomFeedEvent, bool)> {
        self.post_message(request_id, sender, content, recipient_ids, membership_revision,
            fallback_origin, continuation_origin, false, Vec::new())
    }

    /// v3 W6: a scheduled reminder occurrence that asks its recipient for an
    /// ACK, keeping its fallback origin; it is woken and covered by a pointer
    /// like any ACK-tracked request (Reviewer 9ba4c476).
    pub(crate) fn post_reminder_requesting_acks(&mut self, request_id: String, sender: RoomMessageSender,
        content: String, recipient_ids: Vec<SessionId>, membership_revision: RoomRevision,
        fallback_origin: shared_types::RoomFallbackOrigin) -> Result<(RoomFeedEvent, bool)> {
        let ack_requested = !recipient_ids.is_empty();
        self.post_message(request_id, sender, content, recipient_ids, membership_revision, Some(fallback_origin), None,
            ack_requested, Vec::new())
    }

    /// v3 W6: the time of the earliest ACK-tracked reminder `member` still
    /// owes here (neither acknowledged nor declined), if any.
    pub(crate) fn owed_reminder_since(&self, member: SessionId) -> Option<String> {
        self.owed_by(member).into_iter()
            .filter(|post| matches!(&post.event.item, RoomFeedItem::Message { fallback_origin: Some(_), .. }))
            .map(|post| post.event.timestamp.clone())
            .min()
    }

    /// v3 W3: an ordinary addressed delivery that asks each recipient for an
    /// ACK (no fallback or continuation origin).
    pub(crate) fn post_requesting_acks(&mut self, request_id: String, sender: RoomMessageSender,
        content: String, recipient_ids: Vec<SessionId>, membership_revision: RoomRevision) -> Result<(RoomFeedEvent, bool)> {
        self.post_requesting_acks_with_guests(request_id, sender, content, recipient_ids, Vec::new(), membership_revision)
    }

    /// v3 W5: the same, also addressed to guests (stored ids, never re-resolved).
    /// `guest_recipients`: each guest's id and its label at admission.
    pub(crate) fn post_requesting_acks_with_guests(&mut self, request_id: String, sender: RoomMessageSender,
        content: String, recipient_ids: Vec<SessionId>, guest_recipients: Vec<(Uuid, String)>, membership_revision: RoomRevision)
        -> Result<(RoomFeedEvent, bool)> {
        let ack_requested = !recipient_ids.is_empty() || !guest_recipients.is_empty();
        self.post_message(request_id, sender, content, recipient_ids, membership_revision, None, None, ack_requested,
            guest_recipients)
    }

    /// v3 W5: the guests a stored post under this request id was addressed
    /// to, so a retry replays them rather than resolving a label again.
    pub(crate) fn stored_guest_recipients(&self, sender: &RoomMessageSender, request_id: &str) -> Option<Vec<Uuid>> {
        match &self.posts.get(&post_key(sender, request_id))?.event.item {
            RoomFeedItem::Message { guest_recipient_ids, .. } => Some(guest_recipient_ids.clone()),
            _ => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn post_message(&mut self, request_id: String, sender: RoomMessageSender,
        content: String, recipient_ids: Vec<SessionId>, membership_revision: RoomRevision,
        fallback_origin: Option<shared_types::RoomFallbackOrigin>,
        continuation_origin: Option<shared_types::RoomContinuationOrigin>,
        ack_requested: bool, guest_recipients: Vec<(Uuid, String)>) -> Result<(RoomFeedEvent, bool)> {
        let (guest_recipient_ids, guest_recipient_labels): (Vec<Uuid>, Vec<String>) = guest_recipients.into_iter().unzip();
        anyhow::ensure!(fallback_origin.is_none() || continuation_origin.is_none(),
            "a message cannot have both fallback and continuation origins");
        if let Some(event) = self.replayed_post(&request_id, &sender, &content, &recipient_ids)? {
            if let RoomFeedItem::Message { fallback_origin: prior_fallback, continuation_origin: prior_continuation,
                guest_recipient_ids: prior_guests, .. } = &event.item {
                anyhow::ensure!(*prior_fallback == fallback_origin && *prior_continuation == continuation_origin,
                    "delivery origin differs from the stored intent");
                // v3 W5: the same request with other guests is a conflict, never a redirect.
                let (mut old, mut new) = (prior_guests.clone(), guest_recipient_ids.clone());
                old.sort_unstable(); new.sort_unstable();
                if old != new { return Err(RequestConflict.into()); }
            }
            return Ok((event, true));
        }
        let event = self.append(RoomFeedItem::Message { guest_recipient_ids, guest_recipient_labels,
            fallback_origin, continuation_origin, request_id,
            message_id: Uuid::new_v4(), sender, content, recipient_ids, membership_revision, ack_requested })?;
        Ok((event, false))
    }

    /// Existing receipt evidence decides whether a schedule may replace an
    /// occurrence. Unknown/partial failure and Pending remain unresolved.
    pub(crate) fn fallback_delivery(&self, request_id: &str, member: SessionId) -> Option<(RoomDeliveryResult, bool)> {
        let post = self.posts.get(&post_key(&RoomMessageSender::Operator {}, request_id))?;
        let terminal = post.receipts.get(&member).is_some_and(|stored| {
            matches!(stored.receipt.status, RoomDeliveryStatus::Written | RoomDeliveryStatus::Woken | RoomDeliveryStatus::Fetched)
                || (stored.receipt.bytes_written == 0 && (stored.receipt.status == RoomDeliveryStatus::Held
                    || (stored.receipt.status == RoomDeliveryStatus::Failed && stored.zero_write_confirmed)))
        });
        Some((self.delivery_result(&post.event, true), terminal))
    }

    /// A16 (T-31): the operator's stored post under `request_id`, whatever it
    /// was addressed to. Whom a Post reaches depends on who is Supervisor when
    /// it is first stored, so its retry replays that event even if the grants
    /// changed since; different content is a conflict.
    pub(crate) fn replayed_operator_post(&self, request_id: &str, content: &str) -> Result<Option<RoomFeedEvent>> {
        self.ensure_available()?;
        let Some(stored) = self.posts.get(&post_key(&RoomMessageSender::Operator {}, request_id)) else {
            return Ok(None);
        };
        match &stored.event.item {
            RoomFeedItem::Message { content: old, fallback_origin: None, continuation_origin: None, .. } if old == content =>
                Ok(Some(stored.event.clone())),
            _ => Err(RequestConflict.into()),
        }
    }

    /// A retained delivery by its sender and request_id, as a replay (A110).
    pub(crate) fn retained_delivery(&self, sender: &RoomMessageSender, request_id: &str) -> Option<RoomDeliveryResult> {
        let stored = self.posts.get(&post_key(sender, request_id))?;
        matches!(stored.event.item, RoomFeedItem::Message { .. }).then(|| self.delivery_result(&stored.event, true))
    }

    /// v3 (Claude PRIM-1, 15a49bdf): a continuation stored under `request_id`,
    /// by PRIM-1 or, written by an earlier build, by the operator; different
    /// content is a conflict.
    pub(crate) fn replayed_continuation(&self, request_id: &str, content: &str, recipient_ids: &[SessionId])
        -> Result<Option<RoomFeedEvent>> {
        for sender in &CONTINUATION_SENDERS {
            if let Some(event) = self.replayed_post(request_id, sender, content, recipient_ids)? { return Ok(Some(event)); }
        }
        Ok(None)
    }

    pub(crate) fn retained_continuation(&self, request_id: &str) -> Option<RoomDeliveryResult> {
        CONTINUATION_SENDERS.iter().find_map(|sender| self.retained_delivery(sender, request_id))
    }

    pub(crate) fn continuation_delivery(&self, request_id: &str, member: SessionId) -> Option<(RoomDeliveryResult, bool)> {
        let stored = CONTINUATION_SENDERS.iter().find_map(|sender| self.posts.get(&post_key(sender, request_id)))?;
        let terminal = stored.receipts.get(&member).is_some_and(|receipt|
            matches!(receipt.receipt.status, RoomDeliveryStatus::Written | RoomDeliveryStatus::Woken | RoomDeliveryStatus::Fetched)
            || (receipt.receipt.bytes_written == 0 && (receipt.receipt.status == RoomDeliveryStatus::Held
                || (receipt.receipt.status == RoomDeliveryStatus::Failed && receipt.zero_write_confirmed))));
        Some((self.delivery_result(&stored.event, true), terminal))
    }

    pub(crate) fn delivery_result(&self, event: &RoomFeedEvent, replayed: bool) -> RoomDeliveryResult {
        let RoomFeedItem::Message { request_id, message_id, recipient_ids, sender, .. } = &event.item else {
            unreachable!("delivery result requires a message");
        };
        let stored = self.posts.get(&post_key(sender, request_id));
        let receipts: Vec<_> = recipient_ids.iter().map(|id| {
            let mut receipt = stored.and_then(|p| p.receipts.get(id)).map(|r| r.receipt.clone()).unwrap_or(RoomDeliveryReceipt { app_server_turn: None, title_busy: None, already_read: false,
                recipient_id: *id, status: RoomDeliveryStatus::Stored, bytes_written: 0,
                error: None, run_id: None, generation: None, recorded: true, queued_because: None,
                waiting: None, ack: None, hold: None,
            });
            // v3 W3: separate facts beside the transport receipt.
            receipt.waiting = stored.and_then(|p| p.waiting.get(id).cloned());
            receipt.hold = stored.and_then(|p| p.held.get(id).cloned());
            receipt.ack = stored.and_then(|p| p.acks.get(id)).map(|ack| shared_types::RoomAckSummary {
                actor: ack.actor.clone(), disposition: ack.disposition, note: ack.note.clone(),
                outcome_ref: ack.outcome_ref.clone(), cursor: ack.cursor,
            });
            receipt
        }).collect();
        let written_count = stored.map(|p| p.written.len()).unwrap_or(0);
        let failures = receipts.iter().filter(|r| matches!(r.status,
            RoomDeliveryStatus::Failed | RoomDeliveryStatus::Held) && !r.already_read).map(|r| RoomDeliveryFailure {
                recipient_id: r.recipient_id, bytes_written: r.bytes_written,
                error: r.error.clone().unwrap_or_else(|| "wake not completed".into()),
            }).collect();
        let guest_receipts = match &event.item {
            RoomFeedItem::Message { guest_recipient_ids, .. } => guest_recipient_ids.iter()
                .map(|guest_id| match stored.and_then(|post| post.guest_acks.get(guest_id)) {
                    // v3 W5: its disposition, by whoever made it (the guest, or the operator for it).
                    Some(ack) => shared_types::RoomGuestReceipt { guest_id: *guest_id,
                        status: match ack.disposition {
                            shared_types::RoomAckDisposition::Acked => "acked",
                            shared_types::RoomAckDisposition::Declined => "declined",
                        }.into(),
                        note: Some(match &ack.actor {
                            shared_types::RoomGuestAckActor::Guest { .. } => ack.note.clone(),
                            shared_types::RoomGuestAckActor::Operator {} => format!("by the operator: {}", ack.note),
                            shared_types::RoomGuestAckActor::LocalProcess { label } => format!("by {label}: {}", ack.note),
                        }) },
                    None => shared_types::RoomGuestReceipt { guest_id: *guest_id, status: "stored".into(),
                        note: Some("the guest reads by poll; it is never woken".into()) },
                }).collect(),
            _ => Vec::new(),
        };
        RoomDeliveryResult { request_id: request_id.clone(), replayed, receipts,
            room_id: event.room_id, message_id: *message_id, cursor: event.cursor,
            recipient_count: recipient_ids.len(), written_count, failures, guest_receipts }
    }

    /// A stale cursor is an idempotent read of the current result. Only the
    /// latest eligible durable receipt can claim a new wake attempt.
    pub(crate) fn retry_snapshot(
        &self, message_id: Uuid, recipient: SessionId, expected: RoomFeedCursor,
    ) -> Result<(RoomFeedEvent, Option<RoomDeliveryReceipt>)> {
        self.ensure_available()?;
        if !self.definition.member_ids.contains(&recipient) {
            return Err(anyhow!("retry recipient is no longer a room member"));
        }
        let post = self.posts.values().find(|p| matches!(&p.event.item,
            RoomFeedItem::Message { message_id: id, .. } if *id == message_id))
            .ok_or_else(|| anyhow!("stored message is outside the retained retry horizon"))?;
        let RoomFeedItem::Message { recipient_ids, .. } = &post.event.item else { unreachable!() };
        if !recipient_ids.contains(&recipient) {
            return Err(anyhow!("recipient was not in the original authorized wake targets"));
        }
        if post.event.cursor.sequence <= self.join_floor_by_session.get(&recipient).copied().unwrap_or(0) {
            return Err(anyhow!("stored message predates the recipient's current room membership"));
        }
        // v3 W3 (Reviewer 998e10ed): a committed disposition ends the wake's
        // obligation; nothing is retried for it.
        if post.acks.contains_key(&recipient) {
            return Err(anyhow!("message {message_id} already has a disposition for this recipient; it is not woken again"));
        }
        if post.cancel.is_some() {
            return Err(anyhow!("message {message_id} was withdrawn; nothing is retried"));
        }
        let latest = post.receipts.get(&recipient)
            .ok_or_else(|| anyhow!("wake outcome is unknown; there is no safe retry receipt"))?;
        if latest.cursor != expected {
            return Ok((post.event.clone(), None));
        }
        // v3 W3: a pointer not typed because everything it counted was read
        // is not a failure to retry (Claude PRIM-1, 26a7f92 walk).
        if latest.receipt.already_read {
            return Err(anyhow!("the member had already read every message this wake pointed to; nothing is retried"));
        }
        if !(latest.receipt.status == RoomDeliveryStatus::Held && latest.receipt.bytes_written == 0)
            && !(latest.receipt.status == RoomDeliveryStatus::Failed
                && latest.receipt.bytes_written == 0 && latest.zero_write_confirmed) {
            return Err(anyhow!("wake cannot be safely retried: successful, partial or unknown outcome"));
        }
        Ok((post.event.clone(), Some(latest.receipt.clone())))
    }

    pub(crate) fn needs_fetched_receipt(&self, message_id: Uuid, recipient: SessionId) -> bool {
        self.posts.values().find(|p| matches!(&p.event.item,
            RoomFeedItem::Message { message_id: id, recipient_ids, .. }
                if *id == message_id && recipient_ids.contains(&recipient)))
            .is_some_and(|p| p.receipts.get(&recipient).is_none_or(|r| r.receipt.status != RoomDeliveryStatus::Fetched))
    }

    /// Record only addressed messages included in this already-authorized page.
    /// One sync covers the page; no receipt becomes visible before it succeeds.
    pub(crate) fn record_fetched_page(
        &mut self, page: &RoomFeedPage, recipient: SessionId,
        run_id: Uuid, generation: shared_types::SessionGeneration,
    ) -> Result<Vec<RoomFeedEvent>> {
        debug_assert_eq!(page.room_id, self.definition.room_id);
        debug_assert_eq!(page.cursor.epoch, self.feed_epoch);
        // v3 W3: retained bodies returned beside the window are read too; a
        // message on the page and in retained is one read, one Fetched.
        let mut seen = HashSet::new();
        let message_ids: Vec<_> = page.events.iter().chain(page.retained.iter()).filter_map(|event| match &event.item {
            RoomFeedItem::Message { message_id, .. }
                if seen.insert(*message_id) && self.needs_fetched_receipt(*message_id, recipient) => Some(*message_id),
            _ => None,
        }).collect();
        self.ensure_sequence_capacity(message_ids.len())?;
        let prepared: Vec<_> = message_ids.into_iter().enumerate().map(|(index, message_id)| {
            self.prepare_append_at(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, readmit: false, message_id,
                recipient_id: recipient, status: RoomDeliveryStatus::Fetched,
                bytes_written: 0, zero_write_confirmed: false, error: None,
                run_id: Some(run_id), generation: Some(generation), pointer: None,
            }, self.next_sequence + index as RoomSequence)
        }).collect::<Result<_>>()?;
        if let Some(log) = self.log.as_mut() {
            let events: Vec<_> = prepared.iter().map(|entry| entry.event.clone()).collect();
            log.append_batch(&events)?;
        }
        Ok(prepared.into_iter().map(|entry| self.apply_append(entry)).collect())
    }

    pub(crate) fn take_history_transition(&mut self) -> bool {
        let pending = self.startup_sync_pending() || self.writer_needs_replay();
        if pending == self.history_replay_notified && self.writer_recovery == self.writer_recovery_notified { return false; }
        self.history_replay_notified = pending;
        self.writer_recovery_notified = self.writer_recovery.clone();
        true
    }

    pub(crate) fn snapshot(&self) -> RoomSnapshot {
        RoomSnapshot {
            room_id: self.definition.room_id,
            label: self.definition.label.clone(),
            resume_on_restart: self.definition.resume_on_restart,
            member_ids: self.definition.member_ids.clone(),
            membership_revision: self.definition.membership_revision,
            feed_epoch: self.feed_epoch,
            feed_oldest_sequence: self
                .feed
                .front()
                .map(|(event, _bytes)| event.cursor.sequence)
                .unwrap_or(self.next_sequence),
            feed_next_sequence: self.next_sequence,
            recovery_error: self.recovery_error.clone(),
            history_replay_pending: self.startup_sync_pending() || self.writer_needs_replay(),
            writer_recovery: self.writer_recovery.clone(),
            history_restarted: self.definition.history_restarted,
            // Filled by the supervisor, which owns the guest registry.
            guests: Vec::new(),
        }
    }

    pub(crate) fn ensure_sequence_capacity(&self, additional: usize) -> Result<()> {
        self.ensure_available()?;
        self.ensure_writer_available()?;
        let additional = RoomSequence::try_from(additional)
            .map_err(|_| anyhow!("room feed reservation is too large"))?;
        self.next_sequence
            .checked_add(additional)
            .ok_or_else(|| anyhow!("room feed sequence exhausted"))?;
        Ok(())
    }

    pub(crate) fn prepare_append(&self, item: RoomFeedItem) -> Result<PreparedRoomFeedEvent> {
        self.prepare_append_at(item, self.next_sequence)
    }

    fn prepare_append_at(&self, item: RoomFeedItem, sequence: RoomSequence) -> Result<PreparedRoomFeedEvent> {
        self.prepare_append_with_quota(item, sequence, false)
    }

    fn prepare_append_with_quota(&self, item: RoomFeedItem, sequence: RoomSequence,
        completion: bool) -> Result<PreparedRoomFeedEvent> {
        self.ensure_available()?;
        self.ensure_writer_available()?;
        sequence
            .checked_add(1)
            .ok_or_else(|| anyhow!("room feed sequence exhausted"))?;
        let event = RoomFeedEvent {
            schema_version: shared_types::ROOM_EVENT_SCHEMA_VERSION,
            room_id: self.definition.room_id,
            cursor: RoomFeedCursor {
                epoch: self.feed_epoch,
                sequence,
            },
            item,
            timestamp: now_rfc3339(),
        };
        let event_bytes = serde_json::to_vec(&event)
            .map_err(|error| anyhow!("failed to measure room feed event: {error}"))?
            .len();
        self.validate_card_event(&event)?;
        // A106: accepted messages still waiting for a member are never
        // evicted from the post index; an item that would need their space
        // is refused here, before any durable append, at its exact size.
        if matches!(event.item, RoomFeedItem::Message { .. } | RoomFeedItem::Card { .. }) {
            // v3 W3: an unacknowledged tracked message is pinned the same way,
            // so a full index refuses new work before it is Stored.
            let (waiting, waiting_bytes) = self.posts.values()
                .filter(|post| self.awaits_wake(post) || self.awaits_ack(post) || self.awaits_fetch(post))
                .fold((0usize, 0usize), |(count, bytes), post| (count + 1, bytes + post.bytes));
            if waiting + 1 > ROOM_DEDUP_MAX_POSTS || waiting_bytes + event_bytes > ROOM_DEDUP_MAX_BYTES {
                return Err(anyhow!("too many room messages are still waiting for their members; \
                    nothing was stored. Send again once members have received the waiting ones."));
            }
        }
        if event_bytes > ROOM_FEED_MAX_BYTES {
            return Err(anyhow!(
                "room feed event requires {event_bytes} bytes, exceeding the {} byte feed bound",
                ROOM_FEED_MAX_BYTES
            ));
        }
        if let Some(log) = &self.log {
            if completion { log.validate_completion(&event)?; }
            else { log.validate_append(&event)?; }
        }
        Ok(PreparedRoomFeedEvent {
            event,
            bytes: event_bytes,
        })
    }

    pub(crate) fn commit_append(&mut self, prepared: PreparedRoomFeedEvent) -> Result<RoomFeedEvent> {
        self.ensure_available()?;
        self.ensure_writer_available()?;
        if let Some(log) = self.log.as_mut() { log.append(&prepared.event)?; }
        Ok(self.apply_append(prepared))
    }

    /// Only the caller that already stored Pending uses the completion reserve.
    /// Held, Fetched, membership and new Messages stay on the ordinary budget.
    pub(crate) fn append_wake_completion(&mut self, item: RoomFeedItem) -> Result<RoomFeedEvent> {
        let prepared = self.prepare_append_with_quota(item, self.next_sequence, true)?;
        if let Some(log) = self.log.as_mut() { log.append_completion(&prepared.event)?; }
        Ok(self.apply_append(prepared))
    }

    fn apply_append(&mut self, prepared: PreparedRoomFeedEvent) -> RoomFeedEvent {
        debug_assert_eq!(prepared.event.room_id, self.definition.room_id);
        debug_assert_eq!(prepared.event.cursor.epoch, self.feed_epoch);
        debug_assert_eq!(prepared.event.cursor.sequence, self.next_sequence);
        let next_sequence = self
            .next_sequence
            .checked_add(1)
            .expect("prepared room feed event reserved sequence capacity");
        while self.feed.len() >= ROOM_FEED_MAX_EVENTS
            || self.feed_bytes.saturating_add(prepared.bytes) > ROOM_FEED_MAX_BYTES
        {
            let (_evicted, evicted_bytes) = self
                .feed
                .pop_front()
                .expect("a room feed bound required eviction from a non-empty feed");
            self.feed_bytes -= evicted_bytes;
        }
        self.cards.apply(&prepared.event);
        match &prepared.event.item {
            RoomFeedItem::Message { .. } | RoomFeedItem::Card { .. } => {
                let key = match &prepared.event.item {
                    RoomFeedItem::Message { request_id, sender, .. } => post_key(sender, request_id),
                    RoomFeedItem::Card { request_id, author, .. } => (SenderKey::Session(*author), request_id.clone()),
                    _ => unreachable!(),
                };
                // A106: a message still waiting for a member is the queue's
                // durable intent; it is skipped, never evicted. `post` refuses
                // new messages before they would need that space.
                let mut pinned = Vec::new();
                while self.post_order.len() + pinned.len() >= ROOM_DEDUP_MAX_POSTS
                    || self.post_bytes + prepared.bytes > ROOM_DEDUP_MAX_BYTES {
                    let Some(key) = self.post_order.pop_front() else { break; };
                    if self.posts.get(&key).is_some_and(|post| self.awaits_wake(post) || self.awaits_ack(post)
                        || self.awaits_fetch(post)) {
                        pinned.push(key);
                        continue;
                    }
                    if let Some(post) = self.posts.remove(&key) { self.post_bytes -= post.bytes; }
                }
                for key in pinned.into_iter().rev() { self.post_order.push_front(key); }
                self.post_order.push_back(key.clone());
                self.post_bytes += prepared.bytes;
                self.posts.insert(key, StoredPost {
                    event: prepared.event.clone(), bytes: prepared.bytes, receipts: HashMap::new(),
                    written: HashSet::new(), acks: HashMap::new(), guest_acks: HashMap::new(), cancel: None, waiting: HashMap::new(), held: HashMap::new(), readmit: HashMap::new(), recipes: HashMap::new(), observed: HashMap::new(),
                });
            }
            RoomFeedItem::Waiting { message_id, recipient_id, reason_code, attempt, .. } => {
                if let Some(post) = self.posts.values_mut().find(|p| matches!(&p.event.item,
                    RoomFeedItem::Message { message_id: id, .. } if id == message_id)) {
                    // v3 W3b (Reviewer 68568692): a submission uncertainty of
                    // an attempt already observed is history, never current
                    // state (a journal written before the append-lock recheck).
                    let observed = reason_code.starts_with("submit_")
                        && attempt.as_ref().is_some_and(|attempt| post.observed.get(recipient_id) == Some(attempt));
                    if !observed {
                        post.waiting.insert(*recipient_id, reason_code.clone());
                    }

                }
            }
            RoomFeedItem::SubmitAttempt { message_id, recipient_id, attempt, typed, driver, conversation_id,
                working_dir, covered, .. } => {
                let recipe = SubmitRecipe { attempt: *attempt, typed: typed.clone(), driver: *driver,
                    conversation_id: conversation_id.clone(), working_dir: working_dir.clone(),
                    started_at: prepared.event.timestamp.clone() };
                for id in std::iter::once(message_id).chain(covered.iter()) {
                    if let Some(post) = self.posts.values_mut().find(|p| matches!(&p.event.item,
                        RoomFeedItem::Message { message_id: own, .. } if own == id)) {
                        post.recipes.insert(*recipient_id, recipe.clone());
                    }
                }
            }
            RoomFeedItem::InputObserved { message_id, recipient_id, attempt, .. } => {
                // Clears only this attempt's uncertainty (Reviewer 2efa5499).
                if let Some(post) = self.posts.values_mut().find(|p| matches!(&p.event.item,
                    RoomFeedItem::Message { message_id: id, .. } if id == message_id)) {
                    post.observed.insert(*recipient_id, *attempt);
                    if post.recipes.get(recipient_id).is_some_and(|recipe| recipe.attempt == *attempt)
                        && post.waiting.get(recipient_id).is_some_and(|code| code.starts_with("submit_")) {
                        post.waiting.remove(recipient_id);
                    }
                }
            }
            RoomFeedItem::Cancel { request_id, message_id, actor, reason, not_typed, reached, notified } => {
                if let Some(post) = self.posts.values_mut().find(|p| matches!(&p.event.item,
                    RoomFeedItem::Message { message_id: id, .. } if id == message_id)) {
                    post.cancel.get_or_insert_with(|| StoredCancel { request_id: request_id.clone(), actor: actor.clone(),
                        reason: reason.clone(), cursor: prepared.event.cursor, not_typed: not_typed.clone(),
                        reached: reached.clone(), notified: notified.clone() });
                }
            }
            RoomFeedItem::GuestAck { guest_label: _, request_id, message_id, guest_id, actor, disposition, note, outcome_ref } => {
                if let Some(post) = self.posts.values_mut().find(|p| matches!(&p.event.item,
                    RoomFeedItem::Message { message_id: id, .. } if id == message_id)) {
                    // The first disposition stands; a conflicting one is refused before append.
                    post.guest_acks.entry(*guest_id).or_insert_with(|| StoredGuestAck {
                        request_id: request_id.clone(), actor: actor.clone(), disposition: *disposition,
                        note: note.clone(), outcome_ref: outcome_ref.clone(), cursor: prepared.event.cursor,
                    });
                }
            }
            RoomFeedItem::Ack { request_id, message_id, recipient_id, actor, disposition, note, outcome_ref } => {
                if let Some(post) = self.posts.values_mut().find(|p| matches!(&p.event.item,
                    RoomFeedItem::Message { message_id: id, .. } if id == message_id)) {
                    // The first disposition stands; a conflicting one is refused before append.
                    post.acks.entry(*recipient_id).or_insert_with(|| StoredAck {
                        request_id: request_id.clone(), actor: actor.clone(), disposition: *disposition,
                        note: note.clone(), outcome_ref: outcome_ref.clone(), cursor: prepared.event.cursor,
                    });
                }
            }
            RoomFeedItem::Delivery { pointer: _, message_id, recipient_id, status, bytes_written,
                zero_write_confirmed, error, run_id, generation, readmit, already_read, title_busy, app_server_turn } => {
                if let Some(post) = self.posts.values_mut().find(|p| matches!(&p.event.item,
                    RoomFeedItem::Message { message_id: id, .. } if id == message_id)) {
                    // A transport outcome supersedes a recorded deferral.
                    post.waiting.remove(recipient_id);
                    // v3 W3b (Reviewer ed940924): a new attempt begins with its
                    // Pending, and its own recipe follows it; an older recipe
                    // never stands for a later attempt that recorded none.
                    if *status == RoomDeliveryStatus::Pending {
                        post.recipes.remove(recipient_id);
                    }
                    if matches!(status, RoomDeliveryStatus::Written | RoomDeliveryStatus::Woken) {
                        post.written.insert(*recipient_id);
                        post.held.remove(recipient_id);
                    }
                    // v3 W3 (Supervisor 43891c0e; Reviewer 4712da1b, b34f15c2): a
                    // re-admittable transient refusal is marked on its completion,
                    // so replay rebuilds eligibility and the attempt count.
                    if *readmit && *status == RoomDeliveryStatus::Failed && *zero_write_confirmed {
                        let attempts = post.readmit.get(recipient_id).map_or(0, |(_, attempts)| *attempts).saturating_add(1);
                        post.readmit.insert(*recipient_id, (prepared.event.cursor, attempts));
                    }
                    // v3 W3: a hold reason survives a later read (Reviewer 371647cb).
                    if *status == RoomDeliveryStatus::Held {
                        post.held.insert(*recipient_id, error.clone().unwrap_or_else(|| "held".into()));
                    }
                    // Fetch is independent evidence, never downgrade it for a later wake receipt.
                    if !post.receipts.get(recipient_id).is_some_and(|r| r.receipt.status == RoomDeliveryStatus::Fetched) {
                        post.receipts.insert(*recipient_id, StoredReceipt {
                            cursor: prepared.event.cursor, zero_write_confirmed: *zero_write_confirmed,
                            receipt: RoomDeliveryReceipt {
                            recipient_id: *recipient_id, status: *status, bytes_written: *bytes_written,
                            error: error.clone(), run_id: *run_id, generation: *generation, recorded: true,
                            queued_because: None, waiting: None, ack: None, hold: None, already_read: *already_read,
                            title_busy: title_busy.clone(), app_server_turn: app_server_turn.clone(),
                        }});
                    }
                }
            }
            _ => {}
        }
        self.feed
            .push_back((prepared.event.clone(), prepared.bytes));
        self.feed_bytes += prepared.bytes;
        self.next_sequence = next_sequence;
        prepared.event
    }

    pub(crate) fn append(&mut self, item: RoomFeedItem) -> Result<RoomFeedEvent> {
        #[cfg(test)]
        if let (Some(remaining), RoomFeedItem::Delivery { pointer: Some(_), status: RoomDeliveryStatus::Pending, .. })
            = (self.fail_cover_appends_after_for_tests, &item) {
            if remaining == 0 {
                self.fail_cover_appends_after_for_tests = None;
                return Err(anyhow!("injected room append failure"));
            }
            self.fail_cover_appends_after_for_tests = Some(remaining - 1);
        }
        #[cfg(test)]
        if self.fail_submit_attempt_for_tests && matches!(item, RoomFeedItem::SubmitAttempt { .. }) {
            self.fail_submit_attempt_for_tests = false;
            return Err(anyhow!("injected recipe append failure"));
        }
        let prepared = self.prepare_append(item)?;
        self.commit_append(prepared)
    }

    pub(crate) fn read(
        &self,
        cursor: Option<RoomFeedCursor>,
        floor: RoomSequence,
    ) -> Result<RoomFeedPage> {
        self.ensure_available()?;
        let mut gap = None;
        let requested_after = match cursor {
            Some(cursor) if cursor.epoch == self.feed_epoch => {
                if cursor.sequence >= self.next_sequence {
                    return Err(anyhow!(
                        "room feed cursor sequence {} is ahead of the next sequence {}",
                        cursor.sequence,
                        self.next_sequence
                    ));
                }
                cursor.sequence.max(floor)
            }
            Some(_) => {
                gap = Some(RoomFeedGap {
                    reason: RoomFeedGapReason::EpochReset,
                    from_sequence: None,
                    through_sequence: None,
                });
                floor
            }
            None => floor,
        };
        let oldest = self
            .feed
            .front()
            .map(|(event, _bytes)| event.cursor.sequence)
            .unwrap_or(self.next_sequence);
        if gap.is_none() && requested_after.saturating_add(1) < oldest {
            gap = Some(RoomFeedGap {
                reason: RoomFeedGapReason::Evicted,
                from_sequence: Some(requested_after.saturating_add(1)),
                through_sequence: Some(oldest - 1),
            });
        }

        let effective_after = requested_after.max(oldest.saturating_sub(1));
        let mut events = Vec::new();
        let mut page_bytes = 0usize;
        for event in self
            .feed
            .iter()
            .map(|(event, _bytes)| event)
            .filter(|event| event.cursor.sequence > effective_after)
        {
            let event_bytes = serde_json::to_vec(event)
                .map_err(|error| anyhow!("failed to measure room feed page event: {error}"))?
                .len();
            if !events.is_empty()
                && (events.len() >= ROOM_FEED_PAGE_MAX_EVENTS
                    || page_bytes.saturating_add(event_bytes) > ROOM_FEED_PAGE_MAX_BYTES)
            {
                break;
            }
            page_bytes += event_bytes;
            events.push(event.clone());
        }
        let last_sequence = events
            .last()
            .map(|event| event.cursor.sequence)
            .unwrap_or(effective_after);
        let has_more = self
            .feed
            .back()
            .is_some_and(|(event, _bytes)| event.cursor.sequence > last_sequence);
        Ok(RoomFeedPage { retained_next: None,
            schema_version: shared_types::ROOM_EVENT_SCHEMA_VERSION,
            room_id: self.definition.room_id,
            cursor: RoomFeedCursor {
                epoch: self.feed_epoch,
                sequence: last_sequence,
            },
            gap,
            events,
            cards: self.cards.snapshot(floor),
            cards_cursor: Some(RoomFeedCursor { epoch: self.feed_epoch, sequence: self.next_sequence - 1 }),
            has_more,
            // Filled by the supervisor, which owns session labels and guests.
            members: Vec::new(),
            guests: Vec::new(),
            // Filled for a member's own read (`retained_for`).
            retained: Vec::new(),
            retained_receipts: Vec::new(),
            retained_has_more: false,
        })
    }

    /// v3 W3: a bounded page of `reader`'s owed messages (ACK-tracked,
    /// addressed to it, undisposed, inside its join floor), and of its unread
    /// addressed PRIM-1 notices (Reviewer 5571e300) — out of the event
    /// window or in it, also when the returned page carries the body too, so
    /// each comes with its current receipt (a hold reason on the next page
    /// is never lost; Reviewer d68ddeea) and every own read reaches what the
    /// member owes without paging unrelated history (Reviewer ff1dd411). Starts after `after` (an `owed_after` from this room's feed
    /// epoch, never ahead of it), oldest first; the message bodies and their
    /// receipts share the page byte bound. The third value is where to
    /// continue when more remain. Each comes with the reader's own receipt.
    pub(crate) fn retained_for(&self, reader: SessionId, after: Option<RoomFeedCursor>)
        -> Result<(Vec<RoomFeedEvent>, Vec<RoomDeliveryReceipt>, Option<RoomFeedCursor>)> {
        if let Some(after) = after {
            if after.epoch != self.feed_epoch {
                return Err(anyhow!("owed_after belongs to another history of this room; read again without it"));
            }
            if after.sequence >= self.next_sequence {
                return Err(anyhow!("owed_after is ahead of this room's feed; read again without it"));
            }
        }
        let (mut retained, mut receipts) = (Vec::new(), Vec::new());
        let mut bytes = 0usize;
        // v3 W3 (Reviewer 5571e300): an unread addressed PRIM-1 notice comes
        // back with what the reader owes, in feed order, until it is read.
        let mut candidates: Vec<_> = self.owed_by(reader).into_iter()
            .chain(self.posts.values().filter(|post| self.notice_unread_by(post, reader)))
            .filter(|post| after.is_none_or(|after| post.event.cursor.sequence > after.sequence)).collect();
        candidates.sort_by_key(|post| post.event.cursor.sequence);
        for post in &candidates {
            let own: Vec<_> = self.delivery_result(&post.event, false).receipts.into_iter()
                .filter(|receipt| receipt.recipient_id == reader).collect();
            let size = serde_json::to_vec(&post.event).map_err(|error| anyhow!("failed to measure a retained message: {error}"))?.len()
                + serde_json::to_vec(&own).map_err(|error| anyhow!("failed to measure a retained receipt: {error}"))?.len();
            if !retained.is_empty() && (retained.len() >= ROOM_FEED_PAGE_MAX_EVENTS
                || bytes.saturating_add(size) > ROOM_FEED_PAGE_MAX_BYTES) {
                let last = retained.last().map(|event: &RoomFeedEvent| event.cursor);
                return Ok((retained, receipts, last));
            }
            bytes += size;
            retained.push(post.event.clone());
            receipts.extend(own);
        }
        Ok((retained, receipts, None))
    }
}

pub(crate) fn request_id(value: Option<String>) -> Result<String> {
    let value = value.unwrap_or_else(|| Uuid::new_v4().to_string());
    if value.is_empty() || value.len() > 128
        || !value.bytes().all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b)) {
        return Err(anyhow!("request_id must be 1–128 ASCII letters, digits, '.', '_', ':' or '-'"));
    }
    Ok(value)
}

pub(crate) struct RoomState {
    pub(crate) catalog: RoomCatalogV1,
    pub(crate) by_id: HashMap<RoomId, RoomRuntime>,
    pub(crate) order: Vec<RoomId>,
    pub(crate) room_by_session: HashMap<SessionId, RoomId>,
    /// A109: (recipient, roaming sender) -> when that sender last delivered
    /// to the recipient from outside the recipient's room. It lets the
    /// recipient answer for a bounded time without roam; memory only.
    pub(crate) cross_room_senders: HashMap<(SessionId, SessionId), chrono::DateTime<chrono::Utc>>,
}

impl RoomState {
    /// Bootstrap each room independently, then durably bind any legacy or
    /// replaced log epoch before the supervisor exposes its room traffic.
    pub(crate) fn open_logs(
        &mut self,
        runtime_dir: &Path,
        persist: impl FnOnce(&RoomCatalogV1) -> Result<()>,
    ) {
        let mut candidate = self.catalog.clone();
        let mut changed = Vec::new();
        for definition in &mut candidate.rooms {
            let room = self.by_id.get_mut(&definition.room_id).expect("catalog room exists");
            if room.open_log(runtime_dir).is_ok() && room.definition != *definition {
                changed.push(definition.room_id);
                *definition = room.definition.clone();
            }
        }
        if changed.is_empty() { return; }
        match persist(&candidate) {
            Ok(()) => self.catalog = candidate,
            Err(error) => {
                for id in changed {
                    let room = self.by_id.get_mut(&id).expect("changed room exists");
                    room.log = None;
                    room.recovery_error = Some(format!(
                        "Room {id} history needs recovery: could not persist epoch boundary in {}: {error:#}; room traffic is held",
                        runtime_dir.join(ROOM_CATALOG_FILE_NAME).display(),
                    ).chars().take(1024).collect());
                    room.definition = self.catalog.rooms.iter()
                        .find(|definition| definition.room_id == id).expect("catalog room exists").clone();
                }
            }
        }
    }

    pub(crate) fn from_catalog(
        mut catalog: RoomCatalogV1,
        known_sessions: &HashSet<SessionId>,
    ) -> Result<Self> {
        // Grandfather rooms persisted before the briefed list existed: their
        // members were briefed under the old per-run contract, so treat every
        // current member as briefed rather than re-briefing on upgrade.
        for room in &mut catalog.rooms {
            if room.briefed_member_ids.is_none() {
                room.briefed_member_ids = Some(room.member_ids.clone());
            }
        }
        if catalog.schema_version != ROOM_CATALOG_SCHEMA_VERSION {
            return Err(anyhow!(
                "unsupported room catalog schema version {} (expected {})",
                catalog.schema_version,
                ROOM_CATALOG_SCHEMA_VERSION
            ));
        }
        if catalog.rooms.len() > ROOM_MAX_COUNT {
            return Err(anyhow!(
                "room catalog contains {} rooms, exceeding the limit of {ROOM_MAX_COUNT}",
                catalog.rooms.len()
            ));
        }
        let mut by_id = HashMap::new();
        let mut order = Vec::with_capacity(catalog.rooms.len());
        let mut room_by_session = HashMap::new();
        for definition in &catalog.rooms {
            crate::room_fallbacks::validate_record(&definition.fallbacks)?;
            validate_room_label(&definition.label)?;
            if definition.membership_revision == 0 {
                return Err(anyhow!(
                    "room '{}' has invalid zero membership revision",
                    definition.room_id
                ));
            }
            if definition.member_ids.len() > ROOM_MEMBER_MAX_COUNT {
                return Err(anyhow!(
                    "room '{}' has {} members, exceeding the limit of {ROOM_MEMBER_MAX_COUNT}",
                    definition.room_id,
                    definition.member_ids.len()
                ));
            }
            let mut unique_members = HashSet::new();
            for session_id in &definition.member_ids {
                if !known_sessions.contains(session_id) {
                    return Err(anyhow!(
                        "room '{}' references unknown session '{}'",
                        definition.room_id,
                        session_id
                    ));
                }
                if !unique_members.insert(*session_id) {
                    return Err(anyhow!(
                        "room '{}' contains duplicate session '{}'",
                        definition.room_id,
                        session_id
                    ));
                }
                if let Some(existing_room) = room_by_session.insert(*session_id, definition.room_id)
                {
                    return Err(anyhow!(
                        "session '{}' belongs to both room '{}' and room '{}'",
                        session_id,
                        existing_room,
                        definition.room_id
                    ));
                }
            }
            if by_id
                .insert(
                    definition.room_id,
                    RoomRuntime::from_persisted(definition.clone()),
                )
                .is_some()
            {
                return Err(anyhow!(
                    "room catalog contains duplicate room id '{}'",
                    definition.room_id
                ));
            }
            order.push(definition.room_id);
        }
        Ok(Self {
            catalog,
            by_id,
            order,
            room_by_session,
            cross_room_senders: HashMap::new(),
        })
    }

    pub(crate) fn snapshots(&self) -> Vec<RoomSnapshot> {
        self.order
            .iter()
            .filter_map(|room_id| self.by_id.get(room_id).map(RoomRuntime::snapshot))
            .collect()
    }

    pub(crate) fn get(&self, room_id: RoomId) -> Option<&RoomRuntime> {
        self.by_id.get(&room_id)
    }

    pub(crate) fn get_mut(&mut self, room_id: RoomId) -> Option<&mut RoomRuntime> {
        self.by_id.get_mut(&room_id)
    }
}

pub(crate) fn validate_room_label(label: &str) -> Result<()> {
    if label.trim() != label || label.is_empty() {
        return Err(anyhow!(
            "room label must be non-empty and cannot start or end with whitespace"
        ));
    }
    if label.chars().count() > ROOM_LABEL_MAX_CHARS {
        return Err(anyhow!(
            "room label exceeds the {ROOM_LABEL_MAX_CHARS} character limit"
        ));
    }
    if label.chars().any(char::is_control) {
        return Err(anyhow!("room label contains a control character"));
    }
    Ok(())
}

pub(crate) fn default_room_label(existing_count: usize) -> String {
    format!("Room {}", existing_count.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_types::{RoomMembershipAction, RoomMessageSender};

    include!("room_recovery_candidate_tests.rs");
    include!("room_cards_tests.rs");

    #[test]
    fn room_history_notice_is_omitted_when_false_and_preserved_when_true() {
        let mut definition = definition(Uuid::new_v4(), "Notice compatibility", vec![]);
        let without_notice = serde_json::to_value(&definition).unwrap();
        assert!(without_notice.get("history_restarted").is_none());
        assert!(!serde_json::from_value::<PersistedRoomV1>(without_notice).unwrap().history_restarted);
        definition.history_restarted = true;
        let with_notice = serde_json::to_value(&definition).unwrap();
        assert_eq!(with_notice["history_restarted"], true);
        assert!(serde_json::from_value::<PersistedRoomV1>(with_notice).unwrap().history_restarted);
    }

    fn definition(room_id: RoomId, label: &str, members: Vec<SessionId>) -> PersistedRoomV1 {
        PersistedRoomV1 {
            fallbacks: Default::default(),
            join_floors: None,
            join_floor_epoch: None,
            history_restarted: false,
            room_id,
            label: label.into(),
            resume_on_restart: false,
            member_ids: members,
            membership_revision: 1,
            brief_on_join: true,
            brief_template: None,
            briefed_member_ids: Some(Vec::new()),
        }
    }

    #[test]
    fn restart_selection_defaults_off_for_old_room_catalogs() {
        let room = definition(Uuid::new_v4(), "Existing room", Vec::new());
        let mut json = serde_json::to_value(&room).unwrap();
        json.as_object_mut().unwrap().remove("resume_on_restart");
        let restored: PersistedRoomV1 = serde_json::from_value(json).unwrap();
        assert!(!restored.resume_on_restart);
        assert!(!RoomRuntime::from_persisted(restored).snapshot().resume_on_restart);
    }

    #[test]
    fn briefed_members_grandfather_and_track() {
        let member = Uuid::new_v4();
        let mut room = definition(Uuid::new_v4(), "b", vec![member]);
        room.briefed_member_ids = None;
        let catalog = RoomCatalogV1 {
            schema_version: ROOM_CATALOG_SCHEMA_VERSION,
            rooms: vec![room],
        };
        let known = std::iter::once(member).collect();
        let state = RoomState::from_catalog(catalog, &known).unwrap();
        let loaded = state.by_id.values().next().unwrap();
        assert!(
            loaded.definition.member_briefed(member),
            "a legacy catalog's members are grandfathered as briefed"
        );

        let mut fresh = definition(Uuid::new_v4(), "c", vec![member]);
        assert!(!fresh.member_briefed(member));
        fresh.mark_member_briefed(member);
        fresh.mark_member_briefed(member);
        assert_eq!(fresh.briefed_member_ids.as_deref().unwrap().len(), 1);
        fresh.clear_member_briefed(member);
        assert!(!fresh.member_briefed(member));
    }

    #[test]
    fn persisted_room_defaults_brief_fields_when_absent() {
        let json = format!(
            r#"{{"room_id":"{}","label":"old","member_ids":[],"membership_revision":1}}"#,
            Uuid::new_v4()
        );
        let room: PersistedRoomV1 = serde_json::from_str(&json).unwrap();
        assert!(room.brief_on_join);
        assert!(room.brief_template.is_none());
        assert!(room.briefed_member_ids.is_none());

        let back = serde_json::to_string(&room).unwrap();
        let reparsed: PersistedRoomV1 = serde_json::from_str(&back).unwrap();
        assert_eq!(room, reparsed);
    }

    fn message(index: usize, content: String) -> RoomFeedItem {
        RoomFeedItem::Message { guest_recipient_labels: Vec::new(), guest_recipient_ids: Vec::new(), ack_requested: false,
            fallback_origin: None,
            continuation_origin: None,
            request_id: Uuid::new_v4().to_string(),
            message_id: Uuid::from_u128(index as u128 + 1),
            sender: RoomMessageSender::Operator {},
            content,
            recipient_ids: Vec::new(),
            membership_revision: 1,
        }
    }

    #[test]
    fn room_posts_preserve_both_origin_kinds_and_reject_origin_changes_on_replay() {
        let dir = tempfile::tempdir().unwrap();
        let member = Uuid::new_v4();
        let definition = definition(Uuid::new_v4(), "Typed origins", vec![member]);
        let mut room = RoomRuntime::from_persisted(definition);
        room.open_log(dir.path()).unwrap();
        let fallback = shared_types::RoomFallbackOrigin { revision: 1, session_id: member,
            run_id: Uuid::new_v4(), generation: 1, occurrence_id: Uuid::new_v4(),
            scheduled_at: "2026-10-19T12:00:00Z".into() };
        let continuation = shared_types::RoomContinuationOrigin {
            session_id: member, continuation_id: Uuid::new_v4() };
        for (key, fallback_origin, continuation_origin) in [
            ("ordinary", None, None), ("fallback", Some(fallback.clone()), None),
            ("continuation", None, Some(continuation.clone())),
        ] {
            let (event, replayed) = room.post_with_origins(key.into(), RoomMessageSender::Operator {},
                "same body".into(), vec![member], 1, fallback_origin.clone(), continuation_origin.clone()).unwrap();
            assert!(!replayed);
            assert_eq!(room.post_with_origins(key.into(), RoomMessageSender::Operator {}, "same body".into(),
                vec![member], 1, fallback_origin.clone(), continuation_origin.clone()).unwrap(), (event, true));
            let replacement = if fallback_origin.is_some() { (None, Some(continuation.clone())) }
                else { (Some(fallback.clone()), None) };
            assert!(room.post_with_origins(key.into(), RoomMessageSender::Operator {}, "same body".into(),
                vec![member], 1, replacement.0, replacement.1).is_err());
        }
        let before = room.read(None, 0).unwrap();
        assert!(room.post_with_origins("dual-origin".into(), RoomMessageSender::Operator {}, "same body".into(),
            vec![member], 1, Some(fallback), Some(continuation)).is_err());
        assert_eq!(room.read(None, 0).unwrap(), before);
        let definition = room.definition.clone();
        drop(room);
        let mut restored = RoomRuntime::from_persisted(definition);
        restored.open_log(dir.path()).unwrap();
        assert_eq!(restored.read(None, 0).unwrap(), before);
    }

    fn epoch_fixture(dir: &Path, member: SessionId) -> (PersistedRoomV1, RoomFeedCursor) {
        let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Epoch", vec![member]));
        room.open_log(dir).unwrap();
        let (event, _) = room.post("old".into(), RoomMessageSender::Operator {},
            "before this member joined".into(), vec![], 1).unwrap();
        let mut saved = room.definition.clone();
        saved.join_floors = Some(HashMap::from([(member, event.cursor.sequence)]));
        (saved, event.cursor)
    }

    fn epoch_state(definitions: Vec<PersistedRoomV1>) -> RoomState {
        let known = definitions.iter().flat_map(|room| room.member_ids.iter().copied()).collect();
        RoomState::from_catalog(RoomCatalogV1 { schema_version: 1, rooms: definitions }, &known).unwrap()
    }

    fn persist_epoch_catalog(dir: &Path, catalog: &RoomCatalogV1) -> Result<()> {
        crate::persist_room_catalog(dir, catalog)
    }

    #[test]
    fn room_epoch_missing_history_recovers_with_gap_and_survives_another_restart() {
        let dir = tempfile::tempdir().unwrap();
        let member = Uuid::new_v4();
        let (saved, cursor) = epoch_fixture(dir.path(), member);
        let id = saved.room_id;
        let path = dir.path().join("rooms").join(format!("{id}.jsonl"));
        std::fs::rename(&path, path.with_extension("retained")).unwrap();
        let mut state = epoch_state(vec![saved]);
        state.open_logs(dir.path(), |catalog| persist_epoch_catalog(dir.path(), catalog));
        let room = state.get_mut(id).unwrap();
        assert!(room.snapshot().recovery_error.is_none());
        assert_ne!(room.feed_epoch, cursor.epoch);
        assert!(room.snapshot().history_restarted, "recreated bound history needs a persistent notice");
        assert_eq!(room.join_floor_by_session[&member], 0);
        let (new, _) = room.post("new".into(), RoomMessageSender::Operator {},
            "after recovery".into(), vec![], 1).unwrap();
        let page = room.read(Some(cursor), room.join_floor_by_session[&member]).unwrap();
        assert_eq!(page.gap.unwrap().reason, RoomFeedGapReason::EpochReset);
        assert_eq!(page.events, vec![new.clone()]);
        drop(state);
        let catalog = crate::read_room_catalog(&dir.path().join(ROOM_CATALOG_FILE_NAME)).unwrap();
        let mut again = epoch_state(catalog.rooms);
        again.open_logs(dir.path(), |_| panic!("already bound epoch must not rewrite catalog"));
        let room = again.get(id).unwrap();
        assert_eq!(room.feed_epoch, new.cursor.epoch);
        assert!(room.snapshot().history_restarted, "history notice survives another startup");
        assert_eq!(room.read(None, room.join_floor_by_session[&member]).unwrap().events, vec![new]);
        assert!(path.with_extension("retained").exists());
    }

    #[test]
    fn room_epoch_replacement_prefix_is_not_new_members_history() {
        let dir = tempfile::tempdir().unwrap();
        let member = Uuid::new_v4();
        let (saved, cursor) = epoch_fixture(dir.path(), member);
        let id = saved.room_id;
        let path = dir.path().join("rooms").join(format!("{id}.jsonl"));
        std::fs::rename(&path, path.with_extension("retained")).unwrap();
        {
            let mut replacement = RoomRuntime::from_persisted(definition(id, "Replacement", vec![]));
            replacement.open_log(dir.path()).unwrap();
            replacement.post("imported".into(), RoomMessageSender::Operator {},
                "replacement prefix".into(), vec![], 1).unwrap();
        }
        let mut state = epoch_state(vec![saved]);
        state.open_logs(dir.path(), |catalog| persist_epoch_catalog(dir.path(), catalog));
        let room = state.get_mut(id).unwrap();
        assert_eq!(room.join_floor_by_session[&member], 1);
        assert!(room.snapshot().history_restarted, "replacement history needs a persistent notice");
        let (new, _) = room.post("future".into(), RoomMessageSender::Operator {},
            "future message".into(), vec![], 1).unwrap();
        let page = room.read(Some(cursor), room.join_floor_by_session[&member]).unwrap();
        assert_eq!(page.gap.unwrap().reason, RoomFeedGapReason::EpochReset);
        assert_eq!(page.events, vec![new]);
        assert_eq!(room.read(None, 0).unwrap().events.len(), 2); // Operator can inspect the prefix.
    }

    #[test]
    fn room_epoch_legacy_intact_log_keeps_join_boundary_and_binds_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let member = Uuid::new_v4();
        let (mut saved, cursor) = epoch_fixture(dir.path(), member);
        let id = saved.room_id;
        saved.join_floor_epoch = None;
        let mut state = epoch_state(vec![saved]);
        state.open_logs(dir.path(), |catalog| persist_epoch_catalog(dir.path(), catalog));
        let room = state.get(id).unwrap();
        assert_eq!(room.definition.join_floor_epoch, Some(cursor.epoch));
        assert_eq!(room.join_floor_by_session[&member], 1);
        assert!(room.read(None, room.join_floor_by_session[&member]).unwrap().events.is_empty());
        let stored = crate::read_room_catalog(&dir.path().join(ROOM_CATALOG_FILE_NAME)).unwrap();
        assert_eq!(stored.rooms[0].join_floor_epoch, Some(cursor.epoch));
    }

    #[test]
    fn room_epoch_legacy_empty_history_retries_failed_migration_and_keeps_notice() {
        let dir = tempfile::tempdir().unwrap();
        let member = Uuid::new_v4();
        let (mut legacy, _) = epoch_fixture(dir.path(), member);
        legacy.join_floor_epoch = None;
        let id = legacy.room_id;
        let path = dir.path().join("rooms").join(format!("{id}.jsonl"));
        std::fs::rename(&path, path.with_extension("retained")).unwrap();
        let mut first = epoch_state(vec![legacy]);
        persist_epoch_catalog(dir.path(), &first.catalog).unwrap();
        let catalog_before = std::fs::read(dir.path().join(ROOM_CATALOG_FILE_NAME)).unwrap();
        first.open_logs(dir.path(), |_| Err(anyhow!("injected first migration failure")));
        assert!(first.get(id).unwrap().snapshot().recovery_error.is_some());
        assert!(!first.get(id).unwrap().snapshot().history_restarted,
            "failed migration must not publish an uncommitted notice");
        let header = std::fs::read(&path).unwrap();
        assert_eq!(header.iter().filter(|byte| **byte == b'\n').count(), 1);
        assert_eq!(std::fs::read(dir.path().join(ROOM_CATALOG_FILE_NAME)).unwrap(), catalog_before);
        drop(first);

        let catalog = crate::read_room_catalog(&dir.path().join(ROOM_CATALOG_FILE_NAME)).unwrap();
        let mut second = epoch_state(catalog.rooms);
        second.open_logs(dir.path(), |catalog| persist_epoch_catalog(dir.path(), catalog));
        let room = second.get_mut(id).unwrap();
        assert!(room.snapshot().recovery_error.is_none());
        assert!(room.snapshot().history_restarted);
        assert_eq!(room.join_floor_by_session[&member], 0);
        let (new, _) = room.post("after-recovery".into(), RoomMessageSender::Operator {},
            "new readable history".into(), vec![], 1).unwrap();
        assert_eq!(room.read(None, room.join_floor_by_session[&member]).unwrap().events, vec![new.clone()]);
        drop(second);
        // The live writer deliberately retains its Windows file lock. Inspect
        // bytes only after it closes, preserving the exact original header.
        let appended = std::fs::read(&path).unwrap();
        assert_eq!(&appended[..header.len()], header.as_slice(), "keep the existing epoch/header");
        assert_eq!(appended.iter().filter(|byte| **byte == b'\n').count(), 2,
            "the retained header is followed by exactly the new post");

        let catalog = crate::read_room_catalog(&dir.path().join(ROOM_CATALOG_FILE_NAME)).unwrap();
        let mut third = epoch_state(catalog.rooms);
        third.open_logs(dir.path(), |_| panic!("bound history and notice need no rewrite"));
        let room = third.get(id).unwrap();
        assert!(room.snapshot().history_restarted);
        assert_eq!(room.read(None, room.join_floor_by_session[&member]).unwrap().events, vec![new]);
        assert!(path.with_extension("retained").exists());
    }

    #[test]
    fn room_epoch_legacy_nonempty_shortened_history_stays_held() {
        let dir = tempfile::tempdir().unwrap();
        let member = Uuid::new_v4();
        let (mut legacy, _) = epoch_fixture(dir.path(), member);
        legacy.join_floor_epoch = None;
        legacy.join_floors = Some(HashMap::from([(member, 3)]));
        let id = legacy.room_id;
        let path = dir.path().join("rooms").join(format!("{id}.jsonl"));
        let bytes = std::fs::read(&path).unwrap();
        let mut state = epoch_state(vec![legacy]);
        state.open_logs(dir.path(), |_| panic!("nonempty shortened history must not rebind"));
        let room = state.get(id).unwrap();
        assert!(room.snapshot().recovery_error.unwrap().contains("shorter than its persisted membership boundary"));
        assert!(!room.snapshot().history_restarted);
        assert!(room.read(None, 0).is_err());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn room_epoch_same_epoch_truncation_stays_held_and_names_room_path() {
        let dir = tempfile::tempdir().unwrap();
        let member = Uuid::new_v4();
        let (saved, _) = epoch_fixture(dir.path(), member);
        let id = saved.room_id;
        let path = dir.path().join("rooms").join(format!("{id}.jsonl"));
        let bytes = std::fs::read(&path).unwrap();
        let header_end = bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
        std::fs::write(&path, &bytes[..header_end]).unwrap();
        let mut state = epoch_state(vec![saved]);
        state.open_logs(dir.path(), |_| panic!("damaged log must not rebind its boundary"));
        let room = state.get(id).unwrap();
        let error = room.snapshot().recovery_error.unwrap();
        assert!(error.contains(&id.to_string()));
        assert!(error.contains(&path.display().to_string()));
        assert!(error.contains("shorter than its persisted membership boundary"));
        assert!(room.read(None, 0).is_err());
        assert_eq!(std::fs::read(path).unwrap(), bytes[..header_end]);
    }

    #[test]
    fn room_epoch_catalog_failure_holds_only_rebound_rooms_and_can_retry_on_boot() {
        let dir = tempfile::tempdir().unwrap();
        let (changed, _) = epoch_fixture(dir.path(), Uuid::new_v4());
        let (healthy, _) = epoch_fixture(dir.path(), Uuid::new_v4());
        let changed_id = changed.room_id;
        let healthy_id = healthy.room_id;
        let path = dir.path().join("rooms").join(format!("{changed_id}.jsonl"));
        std::fs::rename(&path, path.with_extension("retained")).unwrap();
        let mut state = epoch_state(vec![changed, healthy]);
        let before = state.catalog.clone();
        state.open_logs(dir.path(), |_| Err(anyhow!("injected denied catalog replace")));
        assert_eq!(state.catalog, before);
        let changed = state.get_mut(changed_id).unwrap();
        assert!(changed.snapshot().recovery_error.unwrap().contains("could not persist epoch"));
        assert!(changed.post("blocked".into(), RoomMessageSender::Operator {},
            "must not store".into(), vec![], 1).is_err());
        assert!(state.get(healthy_id).unwrap().read(None, 0).is_ok());
        drop(state);
        let mut again = epoch_state(before.rooms);
        again.open_logs(dir.path(), |catalog| persist_epoch_catalog(dir.path(), catalog));
        assert!(again.get(changed_id).unwrap().snapshot().recovery_error.is_none());
        assert!(again.get(healthy_id).unwrap().read(None, 0).is_ok());
    }

    #[test]
    fn durable_post_retry_replays_one_message_and_rejects_changed_intent() {
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let definition = definition(Uuid::new_v4(), "Durable", vec![recipient]);
        let sender = RoomMessageSender::Operator {};
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        let (committed, replayed) = room.post("intent-1".into(), sender.clone(), "exact text".into(), vec![recipient], 1).unwrap();
        assert!(!replayed);
        drop(room); // Commit succeeded, but the response was lost.
        let mut room = RoomRuntime::from_persisted(definition);
        room.open_log(dir.path()).unwrap();
        let (retried, replayed) = room.post("intent-1".into(), sender.clone(), "exact text".into(), vec![recipient], 1).unwrap();
        assert!(replayed);
        assert_eq!(committed, retried);
        assert_eq!(room.next_sequence, 2);
        assert!(room.post("intent-1".into(), sender.clone(), "different text".into(), vec![recipient], 1).unwrap_err().to_string().contains("conflict"));
        assert!(room.post("intent-1".into(), sender, "exact text".into(), vec![], 1).is_err());
        assert_eq!(room.next_sequence, 2);
    }

    #[test]
    fn request_keys_are_scoped_to_each_sender_and_survive_replay() {
        let dir = tempfile::tempdir().unwrap();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let definition = definition(Uuid::new_v4(), "Shared key", vec![first, second]);
        let senders = [RoomMessageSender::Operator {}, RoomMessageSender::Session { session_id: first },
            RoomMessageSender::Session { session_id: second }];
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        let mut originals = Vec::new();
        for (index, sender) in senders.iter().enumerate() {
            let (event, replayed) = room.post("hello".into(), sender.clone(), format!("body-{index}"), vec![first], 1).unwrap();
            assert!(!replayed);
            originals.push(event);
        }
        let RoomFeedItem::Message { message_id, .. } = originals[1].item else { unreachable!() };
        room.append(retry_receipt(message_id, first, RoomDeliveryStatus::Held, 0, true)).unwrap();
        drop(room);
        let mut room = RoomRuntime::from_persisted(definition);
        room.open_log(dir.path()).unwrap();
        for (index, sender) in senders.iter().enumerate() {
            let (event, replayed) = room.post("hello".into(), sender.clone(), format!("body-{index}"), vec![first], 1).unwrap();
            assert!(replayed);
            assert_eq!(event, originals[index]);
            let result = room.delivery_result(&event, true);
            assert_eq!(result.receipts[0].status, if index == 1 { RoomDeliveryStatus::Held } else { RoomDeliveryStatus::Stored });
            assert!(room.post("hello".into(), sender.clone(), "changed".into(), vec![first], 1).is_err());
            assert!(room.post("hello".into(), sender.clone(), format!("body-{index}"), vec![second], 1).is_err());
        }
        assert_eq!(room.next_sequence, 5);
    }

    #[test]
    fn corrupt_room_keeps_its_header_epoch_and_refuses_partial_history_or_new_posts() {
        use std::{fs, io::Write};
        for had_post in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let definition = definition(Uuid::new_v4(), "Corrupt", vec![]);
            let mut room = RoomRuntime::from_persisted(definition.clone());
            room.open_log(dir.path()).unwrap();
            let epoch = room.feed_epoch;
            if had_post {
                room.post("previous".into(), RoomMessageSender::Operator {}, "original".into(), vec![], 1).unwrap();
            }
            let path = dir.path().join("rooms").join(format!("{}.jsonl", definition.room_id));
            drop(room);
            fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"broken complete record\n").unwrap();
            let original = fs::read(&path).unwrap();
            let mut room = RoomRuntime::from_persisted(definition);
            assert!(room.open_log(dir.path()).is_err());
            assert_eq!(room.snapshot().feed_epoch, epoch);
            assert!(room.snapshot().recovery_error.as_deref().unwrap().contains("invalid complete"));
            assert!(room.read(None, 0).is_err(), "a valid prefix is not a successful history replay");
            assert!(room.post("previous".into(), RoomMessageSender::Operator {}, "original".into(), vec![], 1).is_err());
            assert!(room.post("new".into(), RoomMessageSender::Operator {}, "new body".into(), vec![], 1).is_err());
            assert_eq!(fs::read(&path).unwrap(), original);
        }
    }

    #[test]
    fn oversized_complete_replay_record_is_quarantined_without_panicking_or_rewrite() {
        use std::{fs, io::Write};
        let dir = tempfile::tempdir().unwrap();
        let definition = definition(Uuid::new_v4(), "Oversized", vec![]);
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        let epoch = room.feed_epoch;
        let mut event = RoomFeedEvent {
            schema_version: shared_types::ROOM_EVENT_SCHEMA_VERSION,
            room_id: definition.room_id, cursor: RoomFeedCursor { epoch, sequence: 1 },
            timestamp: now_rfc3339(), item: message(0, String::new()),
        };
        let empty_bytes = serde_json::to_vec(&event).unwrap().len();
        if let RoomFeedItem::Message { content, .. } = &mut event.item {
            *content = "x".repeat(ROOM_FEED_MAX_BYTES + 1 - empty_bytes);
        }
        assert_eq!(serde_json::to_vec(&event).unwrap().len(), ROOM_FEED_MAX_BYTES + 1);
        let path = dir.path().join("rooms").join(format!("{}.jsonl", definition.room_id));
        drop(room);
        let mut complete_record = serde_json::to_vec(&serde_json::json!({ "record": "event", "event": event })).unwrap();
        complete_record.push(b'\n');
        fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(&complete_record).unwrap();
        let original = fs::read(&path).unwrap();
        let mut room = RoomRuntime::from_persisted(definition);
        let error = format!("{:#}", room.open_log(dir.path()).unwrap_err());
        assert!(error.contains("feed bound"), "{error}");
        assert!(error.contains(&room.definition.room_id.to_string()), "{error}");
        assert!(error.contains(&path.display().to_string()), "{error}");
        assert_eq!(room.snapshot().feed_epoch, epoch);
        assert!(room.snapshot().recovery_error.is_some());
        assert!(room.read(None, 0).is_err());
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn poisoned_replay_publishes_fetched_only_after_sync_and_retains_written_count_and_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let member = Uuid::new_v4();
        let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Receipt replay", vec![member]));
        room.open_log(dir.path()).unwrap();
        let messages: Vec<_> = (0..3).map(|index| {
            let (message, _) = room.post(format!("receipt-replay-{index}"), RoomMessageSender::Operator {},
                "original".into(), vec![member], 1).unwrap();
            let RoomFeedItem::Message { message_id, .. } = message.item else { unreachable!() };
            room.append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, readmit: false, pointer: None, message_id, recipient_id: member,
                status: RoomDeliveryStatus::Written, bytes_written: 42, zero_write_confirmed: false,
                error: None, run_id: None, generation: None }).unwrap();
            message
        }).collect();
        let before = room.read(None, 0).unwrap();
        let run_id = Uuid::new_v4();
        let writes_before = room.log.as_ref().unwrap().write_and_sync_count_for_tests();
        room.fail_next_commit_for_tests(true);
        assert!(room.record_fetched_page(&before, member, run_id, 1).is_err());
        assert_eq!(room.log.as_ref().unwrap().write_and_sync_count_for_tests(), writes_before + 1);
        assert_eq!(room.read(None, 0).unwrap(), before, "an unsynced batch publishes no receipt or cursor");
        room.fail_next_replay_sync_for_tests();
        assert!(room.recover_poisoned(dir.path()).is_err());
        assert_eq!(room.read(None, 0).unwrap(), before, "failed recovery cannot publish a partial candidate");
        for message in &messages {
            assert_eq!(room.delivery_result(message, false).receipts[0].status, RoomDeliveryStatus::Written);
        }
        let recovered = room.recover_poisoned(dir.path()).unwrap().unwrap();
        assert_eq!(recovered.len(), 3);
        assert_eq!(recovered[0].cursor.sequence, before.cursor.sequence + 1);
        for (index, message) in messages.iter().enumerate() {
            let result = room.delivery_result(message, true);
            assert_eq!(result.written_count, 1);
            assert_eq!(result.receipts[0].status, RoomDeliveryStatus::Fetched);
            assert!(!room.needs_fetched_receipt(result.message_id, member));
            assert!(room.post(format!("receipt-replay-{index}"), RoomMessageSender::Operator {},
                "original".into(), vec![member], 1).unwrap().1);
        }
        assert!(room.post("receipt-replay-0".into(), RoomMessageSender::Operator {},
            "changed".into(), vec![member], 1).unwrap_err().to_string().contains("request_id conflict"));
        let after = room.read(None, 0).unwrap();
        assert!(room.record_fetched_page(&before, member, run_id, 1).unwrap().is_empty());
        assert_eq!(room.log.as_ref().unwrap().write_and_sync_count_for_tests(), writes_before + 1,
            "re-reading already fetched messages performs no journal I/O");
        assert_eq!(room.read(None, 0).unwrap(), after);
        assert!(room.recover_poisoned(dir.path()).unwrap().is_none());
    }

    #[test]
    fn fetched_batch_partial_prefix_replays_once_and_preserves_exact_torn_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let member = Uuid::new_v4();
        let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Batch prefix", vec![member]));
        room.open_log(dir.path()).unwrap();
        let messages: Vec<_> = (0..3).map(|index| room.post(format!("prefix-{index}"),
            RoomMessageSender::Operator {}, "addressed body".into(), vec![member], 1).unwrap().0).collect();
        let before = room.read(None, 0).unwrap();
        let original_bytes = room.log.as_mut().unwrap().read_owned_for_tests();
        let run_id = Uuid::new_v4();
        room.fail_next_commit_for_tests(false);
        assert!(room.record_fetched_page(&before, member, run_id, 1).is_err());
        assert_eq!(room.read(None, 0).unwrap(), before);
        let uncertain_bytes = room.log.as_mut().unwrap().read_owned_for_tests();
        let appended = &uncertain_bytes[original_bytes.len()..];
        let complete_count = appended.iter().filter(|byte| **byte == b'\n').count();
        assert_eq!(complete_count, 1, "half of three short receipts includes one whole receipt");
        let boundary = appended.iter().rposition(|byte| *byte == b'\n').unwrap() + 1;
        let torn = &appended[boundary..];
        assert!(!torn.is_empty());

        let recovered = room.recover_poisoned(dir.path()).unwrap().unwrap();
        assert_eq!(recovered.len(), complete_count);
        assert_eq!(room.feed_epoch, before.cursor.epoch);
        assert_eq!(room.next_sequence, before.cursor.sequence + 1 + complete_count as u64);
        let tails: Vec<_> = std::fs::read_dir(dir.path().join("rooms")).unwrap().map(|entry| entry.unwrap().path())
            .filter(|path| path.file_name().unwrap().to_string_lossy().contains(".torn-")).collect();
        assert_eq!(tails.len(), 1);
        assert_eq!(std::fs::read(&tails[0]).unwrap(), torn);
        // Append immediately after recovery, before any helper can reseek EOF.
        let writes_before = room.log.as_ref().unwrap().write_and_sync_count_for_tests();
        let remaining = room.record_fetched_page(&before, member, run_id, 1).unwrap();
        assert_eq!(remaining.len(), 3 - complete_count);
        let bytes_after_append = room.log.as_mut().unwrap().read_owned_for_tests();
        assert_eq!(&bytes_after_append[..original_bytes.len() + boundary],
            &uncertain_bytes[..original_bytes.len() + boundary]);
        assert_eq!(room.log.as_ref().unwrap().write_and_sync_count_for_tests(), writes_before + 1,
            "all remaining receipts share one write and sync");
        let after = room.read(None, 0).unwrap();
        let fetched: Vec<_> = after.events.iter().filter(|event| matches!(event.item,
            RoomFeedItem::Delivery { status: RoomDeliveryStatus::Fetched, .. })).collect();
        assert_eq!(fetched.len(), 3);
        assert!(fetched.windows(2).all(|pair| pair[1].cursor.sequence == pair[0].cursor.sequence + 1));
        assert!(room.record_fetched_page(&before, member, run_id, 1).unwrap().is_empty());
        for message in &messages {
            let result = room.delivery_result(message, true);
            assert_eq!(result.receipts[0].status, RoomDeliveryStatus::Fetched);
            assert_eq!(result.written_count, 0, "Fetched never claims a wake");
        }
        let definition = room.definition.clone();
        drop(room);
        let mut restored = RoomRuntime::from_persisted(definition);
        restored.open_log(dir.path()).unwrap();
        assert_eq!(restored.read(None, 0).unwrap(), after, "direct append preserved every replayed record");
    }

    #[test]
    fn corrupt_header_has_unknown_epoch_and_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let definition = definition(Uuid::new_v4(), "Header", vec![]);
        std::fs::create_dir(dir.path().join("rooms")).unwrap();
        let path = dir.path().join("rooms").join(format!("{}.jsonl", definition.room_id));
        let original = b"broken header\n";
        std::fs::write(&path, original).unwrap();
        let mut room = RoomRuntime::from_persisted(definition);
        assert!(room.open_log(dir.path()).is_err());
        assert_eq!(room.snapshot().feed_epoch, Uuid::nil());
        assert!(room.snapshot().recovery_error.is_some());
        assert!(room.read(None, 0).is_err());
        assert_eq!(std::fs::read(path).unwrap(), original);
    }

    #[test]
    fn uncertain_commit_returns_no_stored_result_then_recovers_the_same_intent() {
        let dir = tempfile::tempdir().unwrap();
        let definition = definition(Uuid::new_v4(), "Uncertain", vec![]);
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        room.log.as_mut().unwrap().fail_commit_for_tests(true);
        let post = || ("lost-sync".into(), RoomMessageSender::Operator {}, "payload".into());
        let (id, sender, body) = post();
        assert!(room.post(id, sender, body, vec![], 1).is_err());
        assert!(room.posts.is_empty());
        assert_eq!(room.next_sequence, 1);
        drop(room);
        let mut room = RoomRuntime::from_persisted(definition);
        room.open_log(dir.path()).unwrap();
        let (id, sender, body) = post();
        let (_, replayed) = room.post(id, sender, body, vec![], 1).unwrap();
        assert!(replayed);
        assert_eq!(room.next_sequence, 2);
    }

    #[test]
    fn dedup_horizon_is_bounded_and_rebuilt_from_committed_records() {
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let definition = definition(Uuid::new_v4(), "Horizon", vec![recipient]);
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        let mut expired = None;
        for n in 0..=ROOM_DEDUP_MAX_POSTS {
            let targets = if n == 0 { vec![recipient] } else { vec![] };
            let (event, _) = room.post(format!("id-{n}"), RoomMessageSender::Operator {}, "x".into(), targets, 1).unwrap();
            if n == 0 {
                let message_id = room.delivery_result(&event, false).message_id;
                let receipt = room.append(retry_receipt(message_id, recipient,
                    RoomDeliveryStatus::Held, 0, false)).unwrap();
                expired = Some((message_id, receipt.cursor));
            }
        }
        let (message_id, receipt_cursor) = expired.unwrap();
        assert_eq!(room.posts.len(), ROOM_DEDUP_MAX_POSTS);
        assert!(!room.posts.contains_key(&post_key(&RoomMessageSender::Operator {}, "id-0")));
        assert!(room.retry_snapshot(message_id, recipient, receipt_cursor)
            .unwrap_err().to_string().contains("retained retry horizon"));
        drop(room);
        let mut room = RoomRuntime::from_persisted(definition);
        room.open_log(dir.path()).unwrap();
        assert_eq!(room.posts.len(), ROOM_DEDUP_MAX_POSTS);
        assert!(!room.posts.contains_key(&post_key(&RoomMessageSender::Operator {}, "id-0")));
        assert!(room.retry_snapshot(message_id, recipient, receipt_cursor)
            .unwrap_err().to_string().contains("retained retry horizon"));
        let before = room.next_sequence;
        assert!(room.post("id-1".into(), RoomMessageSender::Operator {}, "x".into(), vec![], 1).unwrap().1);
        assert_eq!(room.next_sequence, before);
    }

    #[test]
    fn waiting_messages_stay_pinned_through_churn_and_reopen_within_the_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let definition = definition(Uuid::new_v4(), "Pinned", vec![recipient]);
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        for n in 0..2 {
            room.post(format!("waiting-{n}"), RoomMessageSender::Operator {}, "x".into(), vec![recipient], 1).unwrap();
        }
        for n in 0..3 * ROOM_DEDUP_MAX_POSTS {
            room.post(format!("churn-{n}"), RoomMessageSender::Operator {}, "x".into(), vec![], 1).unwrap();
            assert!(room.posts.len() <= ROOM_DEDUP_MAX_POSTS && room.post_order.len() == room.posts.len());
        }
        assert_eq!(room.unreceipted_messages().len(), 2, "both waiting intents survive the churn");
        drop(room);
        let mut room = RoomRuntime::from_persisted(definition);
        room.open_log(dir.path()).unwrap();
        assert!(room.posts.len() <= ROOM_DEDUP_MAX_POSTS);
        assert_eq!(room.unreceipted_messages().len(), 2, "and the reopened index still finds them");
    }

    /// v3 W3 (8597d39f, 52f28e93): a read but unacknowledged ACK-tracked
    /// message outlives index churn and a reopen, comes back to its reader
    /// once out of the window, and leaves only after its disposition.
    #[test]
    fn an_unacknowledged_tracked_message_stays_retained_through_churn_and_reopen_until_its_ack() {
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let definition = definition(Uuid::new_v4(), "Owed", vec![recipient]);
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        let (owed, _) = room.post_requesting_acks("owed".into(), RoomMessageSender::Operator {}, "owed body".into(),
            vec![recipient], 1).unwrap();
        let message_id = room.delivery_result(&owed, false).message_id;
        room.append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, readmit: false, message_id, recipient_id: recipient, status: RoomDeliveryStatus::Fetched,
            bytes_written: 0, zero_write_confirmed: false, error: None, run_id: None, generation: None, pointer: None }).unwrap();
        for n in 0..2 * ROOM_DEDUP_MAX_POSTS {
            room.post(format!("churn-{n}"), RoomMessageSender::Operator {}, "x".into(), vec![], 1).unwrap();
        }
        assert!(room.contains_message(message_id), "pinned through churn");
        drop(room);
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        assert!(room.contains_message(message_id), "and after a reopen");
        // Read (Fetched) messages are not re-pointed, but stay owed and readable
        // only through the reader's own retained list once out of the window.
        assert!(room.unread_for(recipient).is_empty());
        let (retained, receipts, _) = room.retained_for(recipient, None).unwrap();
        assert_eq!(retained.iter().map(|event| event.cursor).collect::<Vec<_>>(), vec![owed.cursor],
            "a read but unACKed body is still returned to its reader (Reviewer 80495230 b)");
        assert_eq!(receipts[0].status, RoomDeliveryStatus::Fetched);
        room.record_ack("ack-1".into(), message_id, recipient,
            shared_types::RoomAckActor::Session { session_id: recipient }, shared_types::RoomAckDisposition::Acked,
            "done".into(), None).unwrap();
        for n in 0..ROOM_DEDUP_MAX_POSTS {
            room.post(format!("after-{n}"), RoomMessageSender::Operator {}, "x".into(), vec![], 1).unwrap();
        }
        assert!(!room.contains_message(message_id), "after its ACK it ages out like any post");
    }

    /// Reviewer 4712da1b: re-admission after a confirmed-zero transient
    /// refusal is derived from the journal (Failed zero, then
    /// Waiting{output_settling}), so a reopen keeps it and its attempt budget;
    /// the fourth such refusal is not re-admitted; a non-zero Failed never is.
    #[test]
    fn a_transient_zero_write_readmission_survives_a_reopen_and_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let definition = definition(Uuid::new_v4(), "Readmit", vec![recipient]);
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        let (event, _) = room.post_requesting_acks("r".into(), RoomMessageSender::Operator {}, "body".into(),
            vec![recipient], 1).unwrap();
        let RoomFeedItem::Message { message_id, .. } = event.item.clone() else { unreachable!() };
        let attempt = |room: &mut RoomRuntime, bytes: usize, zero: bool| {
            room.append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, readmit: false, pointer: None, message_id, recipient_id: recipient,
                status: RoomDeliveryStatus::Pending, bytes_written: 0, zero_write_confirmed: false,
                error: None, run_id: None, generation: None }).unwrap();
            room.append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, pointer: None, message_id, recipient_id: recipient,
                status: RoomDeliveryStatus::Failed, bytes_written: bytes, zero_write_confirmed: zero,
                error: Some("screen changed".into()), run_id: None, generation: None, readmit: true }).unwrap();
        };
        attempt(&mut room, 0, true);
        assert!(room.readmit_pending_for(&event, recipient) && !room.has_receipt(&event, recipient));
        drop(room);
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        assert!(room.readmit_pending_for(&event, recipient), "rebuilt from the journal");
        assert_eq!(room.unreceipted_messages().len(), 1, "a restart queues it again");
        assert_eq!(room.unread_for(recipient).len(), 1, "still counted as owed wake work");
        attempt(&mut room, 0, true);
        attempt(&mut room, 0, true);
        assert!(room.readmit_pending_for(&event, recipient), "third refusal still re-admitted");
        drop(room);
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        attempt(&mut room, 0, true);
        assert!(!room.readmit_pending_for(&event, recipient) && room.has_receipt(&event, recipient),
            "the budget survives the reopen: the fourth is Failed for Retry wake");
        let (other, _) = room.post_requesting_acks("p".into(), RoomMessageSender::Operator {}, "partial".into(),
            vec![recipient], 1).unwrap();
        let RoomFeedItem::Message { message_id: other_id, .. } = other.item.clone() else { unreachable!() };
        room.append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, pointer: None, message_id: other_id, recipient_id: recipient,
            status: RoomDeliveryStatus::Failed, bytes_written: 5, zero_write_confirmed: false,
            error: None, run_id: None, generation: None, readmit: true }).unwrap();
        assert!(!room.readmit_pending_for(&other, recipient), "a partial write is never re-admitted");
        // A Waiting record alone re-admits nothing: only the completion's flag does.
        let (plain, _) = room.post_requesting_acks("q".into(), RoomMessageSender::Operator {}, "plain".into(),
            vec![recipient], 1).unwrap();
        let RoomFeedItem::Message { message_id: plain_id, .. } = plain.item.clone() else { unreachable!() };
        room.append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, pointer: None, message_id: plain_id, recipient_id: recipient,
            status: RoomDeliveryStatus::Failed, bytes_written: 0, zero_write_confirmed: true,
            error: None, run_id: None, generation: None, readmit: false }).unwrap();
        room.append(RoomFeedItem::Waiting { attempt: None, message_id: plain_id, recipient_id: recipient,
            reason_code: ROOM_WAKE_READMIT_CODE.into(), reason: "settling".into() }).unwrap();
        assert!(!room.readmit_pending_for(&plain, recipient));
    }

    /// Reviewer b34f15c2: a crash between the group's appends. The head's
    /// completion carries its own flag, so it is re-admitted after a reopen;
    /// a covered request whose completion never landed keeps its uncertain
    /// Pending (never re-typed for itself) and is still counted as owed by
    /// the head's next pointer.
    #[test]
    fn a_crash_between_group_completions_readmits_the_head_and_still_counts_the_follower() {
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let definition = definition(Uuid::new_v4(), "Torn group", vec![recipient]);
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        let (head, _) = room.post_requesting_acks("h".into(), RoomMessageSender::Operator {}, "head".into(),
            vec![recipient], 1).unwrap();
        let (follower, _) = room.post_requesting_acks("f".into(), RoomMessageSender::Operator {}, "follower".into(),
            vec![recipient], 1).unwrap();
        let id = |event: &RoomFeedEvent| match &event.item { RoomFeedItem::Message { message_id, .. } => *message_id, _ => unreachable!() };
        let delivery = |message_id, status, pointer, readmit| RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, pointer, message_id,
            recipient_id: recipient, status, bytes_written: 0, zero_write_confirmed: status == RoomDeliveryStatus::Failed,
            error: None, run_id: None, generation: None, readmit };
        room.append(delivery(id(&head), RoomDeliveryStatus::Pending, None, false)).unwrap();
        room.append(delivery(id(&follower), RoomDeliveryStatus::Pending, Some(id(&head)), false)).unwrap();
        room.append_wake_completion(delivery(id(&head), RoomDeliveryStatus::Failed, None, true)).unwrap();
        drop(room); // crash before the follower's completion
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        assert!(room.readmit_pending_for(&head, recipient), "the head is re-admitted from its own completion");
        assert!(!room.readmit_pending_for(&follower, recipient) && room.has_receipt(&follower, recipient),
            "the follower's outcome stays uncertain: never re-typed for itself");
        let owed: Vec<_> = room.unread_for(recipient).iter().map(|event| event.cursor).collect();
        assert_eq!(owed, vec![head.cursor, follower.cursor], "both are counted by the next pointer");
    }

    /// W3b: the recipe makes an attempt reconcilable — also when the journal
    /// ends before any Waiting (Reviewer 02655ed3 case 3) — and only that
    /// attempt's InputObserved, with its own Written still latest, clears it.
    #[test]
    fn a_submit_recipe_survives_a_reopen_and_only_its_own_attempt_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let definition = definition(Uuid::new_v4(), "Recipe", vec![recipient]);
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        let (event, _) = room.post_requesting_acks("o".into(), RoomMessageSender::Operator {}, "body".into(),
            vec![recipient], 1).unwrap();
        let RoomFeedItem::Message { message_id, .. } = event.item.clone() else { unreachable!() };
        let delivery = |status| RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, pointer: None, message_id, recipient_id: recipient, status,
            bytes_written: 0, zero_write_confirmed: false, error: None, run_id: None, generation: None, readmit: false };
        let pending = room.append(delivery(RoomDeliveryStatus::Pending)).unwrap();
        room.append(RoomFeedItem::SubmitAttempt { message_id, recipient_id: recipient, attempt: pending.cursor,
            typed: "[PRIM-1] x".into(), driver: shared_types::DriverKind::Claude, conversation_id: "c".into(),
            working_dir: "D:\\w".into(), run_id: None, covered: Vec::new() }).unwrap();
        drop(room); // a crash after the recipe (and maybe the bytes), before any outcome
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        let open = room.open_submissions();
        assert_eq!((open.len(), open[0].3), (1, pending.cursor), "the Pending boundary stays reconcilable");
        let written = room.append_wake_completion(delivery(RoomDeliveryStatus::Written)).unwrap();
        drop(room); // the journal ends before any Waiting
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        let open = room.open_submissions();
        assert_eq!(open.len(), 1, "the recipe survives the reopen");
        assert_eq!((open[0].2.attempt, open[0].3), (pending.cursor, written.cursor));
        let other = RoomFeedCursor { epoch: pending.cursor.epoch, sequence: pending.cursor.sequence + 99 };
        room.append(RoomFeedItem::InputObserved { message_id, recipient_id: recipient, attempt: other,
            source: "claude_transcript".into(), locator: "t.jsonl@0".into() }).unwrap();
        assert!(room.submission_open(&event, recipient, pending.cursor, written.cursor), "another attempt clears nothing");
        room.append(RoomFeedItem::InputObserved { message_id, recipient_id: recipient, attempt: pending.cursor,
            source: "claude_transcript".into(), locator: "t.jsonl@0".into() }).unwrap();
        assert!(!room.submission_open(&event, recipient, pending.cursor, written.cursor));
        assert!(room.open_submissions().is_empty());
        assert_eq!(room.delivery_result(&event, false).receipts[0].status, RoomDeliveryStatus::Written, "not an ACK, not a read");
    }

    /// W3b (Reviewer 68568692): a submission uncertainty appended after its
    /// own attempt's InputObserved (a journal from before the append-lock
    /// recheck) restores nothing, in this process or after a reopen; another
    /// attempt's uncertainty still stands.
    #[test]
    fn a_submit_uncertainty_after_its_own_observation_restores_nothing_also_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let definition = definition(Uuid::new_v4(), "Observed first", vec![recipient]);
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        let (event, _) = room.post_requesting_acks("o".into(), RoomMessageSender::Operator {}, "body".into(),
            vec![recipient], 1).unwrap();
        let RoomFeedItem::Message { message_id, .. } = event.item.clone() else { unreachable!() };
        let delivery = |status| RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, pointer: None, message_id, recipient_id: recipient, status,
            bytes_written: 0, zero_write_confirmed: false, error: None, run_id: None, generation: None, readmit: false };
        let pending = room.append(delivery(RoomDeliveryStatus::Pending)).unwrap();
        room.append(RoomFeedItem::SubmitAttempt { message_id, recipient_id: recipient, attempt: pending.cursor,
            typed: "[PRIM-1] x".into(), driver: shared_types::DriverKind::Claude, conversation_id: "c".into(),
            working_dir: "D:\\w".into(), run_id: None, covered: Vec::new() }).unwrap();
        room.append_wake_completion(delivery(RoomDeliveryStatus::Written)).unwrap();
        room.append(RoomFeedItem::InputObserved { message_id, recipient_id: recipient, attempt: pending.cursor,
            source: "claude_transcript".into(), locator: "t.jsonl@0".into() }).unwrap();
        let unconfirmed = |attempt| RoomFeedItem::Waiting { message_id, recipient_id: recipient,
            reason_code: "submit_unconfirmed".into(), reason: "late early worker".into(), attempt: Some(attempt) };
        room.append(unconfirmed(pending.cursor)).unwrap();
        assert_eq!(room.waiting_code(&event, recipient), None, "the observed attempt stays settled");
        drop(room);
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        assert_eq!(room.waiting_code(&event, recipient), None, "and stays settled after a reopen");
        assert!(room.open_submissions().is_empty(), "nothing is left to reconcile");
        let other = RoomFeedCursor { epoch: pending.cursor.epoch, sequence: pending.cursor.sequence + 99 };
        room.append(unconfirmed(other)).unwrap();
        assert_eq!(room.waiting_code(&event, recipient).as_deref(), Some("submit_unconfirmed"),
            "another attempt's uncertainty is not hidden by this observation");
    }

    /// W3b (Claude PRIM-1 f0899a06): a Written attempt of a previous process
    /// with no observation is listed once; after its submit_unknown (or an
    /// observation) it is not listed again.
    #[test]
    fn a_written_unsettled_attempt_is_listed_once_for_the_restart_record() {
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let definition = definition(Uuid::new_v4(), "Restart", vec![recipient]);
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        let (event, _) = room.post_requesting_acks("o".into(), RoomMessageSender::Operator {}, "body".into(),
            vec![recipient], 1).unwrap();
        let RoomFeedItem::Message { message_id, .. } = event.item.clone() else { unreachable!() };
        let delivery = |status| RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, pointer: None, message_id, recipient_id: recipient, status,
            bytes_written: 0, zero_write_confirmed: false, error: None, run_id: None, generation: None, readmit: false };
        let pending = room.append(delivery(RoomDeliveryStatus::Pending)).unwrap();
        room.append(RoomFeedItem::SubmitAttempt { message_id, recipient_id: recipient, attempt: pending.cursor,
            typed: "t".into(), driver: shared_types::DriverKind::Codex, conversation_id: "c".into(),
            working_dir: "w".into(), run_id: None, covered: Vec::new() }).unwrap();
        assert!(room.unsettled_written_attempts().is_empty(), "a Pending-only attempt is not listed");
        room.append_wake_completion(delivery(RoomDeliveryStatus::Written)).unwrap();
        drop(room);
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        assert_eq!(room.unsettled_written_attempts(), vec![(message_id, recipient, pending.cursor)]);
        room.append(RoomFeedItem::Waiting { message_id, recipient_id: recipient, reason_code: "submit_unknown".into(),
            reason: "restarted".into(), attempt: Some(pending.cursor) }).unwrap();
        drop(room);
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        assert!(room.unsettled_written_attempts().is_empty(), "idempotent across a further restart");
        assert_eq!(room.delivery_result(&event, false).receipts[0].status, RoomDeliveryStatus::Written, "no retype, no new receipt");
    }

    /// W3b (Reviewer ed940924): an older recipe never stands for a later
    /// attempt of the same message that recorded none (a full-body wake, an
    /// unbound member).
    #[test]
    fn a_later_attempt_without_a_recipe_ends_the_older_recipe() {
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Later", vec![recipient]));
        room.open_log(dir.path()).unwrap();
        let (event, _) = room.post_requesting_acks("o".into(), RoomMessageSender::Operator {}, "body".into(),
            vec![recipient], 1).unwrap();
        let RoomFeedItem::Message { message_id, .. } = event.item.clone() else { unreachable!() };
        let delivery = |status| RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, pointer: None, message_id, recipient_id: recipient, status,
            bytes_written: 0, zero_write_confirmed: false, error: None, run_id: None, generation: None, readmit: false };
        let first = room.append(delivery(RoomDeliveryStatus::Pending)).unwrap();
        room.append(RoomFeedItem::SubmitAttempt { message_id, recipient_id: recipient, attempt: first.cursor,
            typed: "t".into(), driver: shared_types::DriverKind::Claude, conversation_id: "c".into(),
            working_dir: "w".into(), run_id: None, covered: Vec::new() }).unwrap();
        room.append_wake_completion(delivery(RoomDeliveryStatus::Written)).unwrap();
        assert_eq!(room.open_submissions().len(), 1);
        room.append(delivery(RoomDeliveryStatus::Pending)).unwrap();
        assert!(room.open_submissions().is_empty(), "the later attempt has no recipe");
        room.append_wake_completion(delivery(RoomDeliveryStatus::Written)).unwrap();
        assert!(room.open_submissions().is_empty());
        assert!(!room.submission_open(&event, recipient, first.cursor, first.cursor));
    }

    #[test]
    fn submission_details_reach_only_their_member_and_the_observed_fact_reaches_all() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Redact", vec![a, b]));
        room.open_log(dir.path()).unwrap();
        let (event, _) = room.post_requesting_acks("o".into(), RoomMessageSender::Operator {}, "body".into(),
            vec![a, b], 1).unwrap();
        let RoomFeedItem::Message { message_id, .. } = event.item.clone() else { unreachable!() };
        for recipient in [a, b] {
            room.append(RoomFeedItem::SubmitAttempt { message_id, recipient_id: recipient, attempt: event.cursor,
                typed: "t".into(), driver: shared_types::DriverKind::Claude, conversation_id: "c".into(),
                working_dir: "w".into(), run_id: None, covered: Vec::new() }).unwrap();
            room.append(RoomFeedItem::InputObserved { message_id, recipient_id: recipient, attempt: event.cursor,
                source: "claude_transcript".into(), locator: "c.jsonl@0".into() }).unwrap();
        }
        // (recipes kept, observations kept, locators kept)
        let kept = |reader| {
            let mut page = room.read(None, 0).unwrap();
            redact_submission_records(&mut page, reader);
            page.events.iter().fold((0, 0, 0), |(recipes, observed, locators), event| match &event.item {
                RoomFeedItem::SubmitAttempt { .. } => (recipes + 1, observed, locators),
                RoomFeedItem::InputObserved { locator, .. } => (recipes, observed + 1, locators + usize::from(!locator.is_empty())),
                _ => (recipes, observed, locators),
            })
        };
        assert_eq!(kept(Some(a)), (1, 2, 1), "its own recipe and locator; the other's observed fact only");
        assert_eq!(kept(None), (0, 2, 0), "a guest sees that input was observed, nothing private");
    }

    /// Reviewer 998e10ed: a committed disposition ends the wake; a retry of
    /// its Held receipt is refused.
    #[test]
    fn a_disposed_message_is_never_retried() {
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Disposed", vec![recipient]));
        room.open_log(dir.path()).unwrap();
        let (event, _) = room.post_requesting_acks("d".into(), RoomMessageSender::Operator {}, "body".into(),
            vec![recipient], 1).unwrap();
        let RoomFeedItem::Message { message_id, .. } = event.item.clone() else { unreachable!() };
        let held = room.append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, pointer: None, message_id, recipient_id: recipient,
            status: RoomDeliveryStatus::Held, bytes_written: 0, zero_write_confirmed: false,
            error: Some("sender revoked".into()), run_id: None, generation: None, readmit: false }).unwrap();
        assert!(room.retry_snapshot(message_id, recipient, held.cursor).unwrap().1.is_some(), "retryable before");
        room.record_ack("op-1".into(), message_id, recipient, shared_types::RoomAckActor::Operator {},
            shared_types::RoomAckDisposition::Declined, "handled elsewhere".into(), None).unwrap();
        let refused = room.retry_snapshot(message_id, recipient, held.cursor).unwrap_err();
        assert!(refused.to_string().contains("already has a disposition"), "{refused}");
    }

    /// Reviewer 371647cb / 168b7d7e: a later read never erases why a wake was
    /// held; a later write does.
    #[test]
    fn a_hold_reason_survives_a_later_read_and_a_later_write_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Hold", vec![recipient]));
        room.open_log(dir.path()).unwrap();
        let (event, _) = room.post_requesting_acks("held".into(), RoomMessageSender::Operator {}, "body".into(),
            vec![recipient], 1).unwrap();
        let RoomFeedItem::Message { message_id, .. } = event.item.clone() else { unreachable!() };
        let delivery = |status, error: Option<&str>| RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, readmit: false, pointer: None, message_id,
            recipient_id: recipient, status, bytes_written: 0, zero_write_confirmed: false,
            error: error.map(Into::into), run_id: None, generation: None };
        let hold = |room: &RoomRuntime| room.delivery_result(&event, false).receipts[0].hold.clone();
        room.append(delivery(RoomDeliveryStatus::Held, Some("sender revoked"))).unwrap();
        room.append(delivery(RoomDeliveryStatus::Fetched, None)).unwrap();
        let receipt = room.delivery_result(&event, false).receipts[0].clone();
        assert_eq!((receipt.status, receipt.hold.as_deref()), (RoomDeliveryStatus::Fetched, Some("sender revoked")));
        let (_, retained_receipts, _) = room.retained_for(recipient, None).unwrap();
        assert!(retained_receipts.iter().all(|receipt| receipt.hold.as_deref() == Some("sender revoked")));
        room.append(delivery(RoomDeliveryStatus::Written, None)).unwrap();
        assert_eq!(hold(&room), None, "a later write supersedes the hold");
    }

    #[test]
    fn retained_for_returns_an_unread_tracked_message_that_left_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Window", vec![recipient]));
        room.open_log(dir.path()).unwrap();
        let (owed, _) = room.post_requesting_acks("owed".into(), RoomMessageSender::Operator {}, "owed body".into(),
            vec![recipient], 1).unwrap();
        assert_eq!(room.retained_for(recipient, None).unwrap().0.iter().map(|event| event.cursor).collect::<Vec<_>>(),
            vec![owed.cursor], "an owed message in the window is returned with its receipt");
        for n in 0..ROOM_FEED_MAX_EVENTS {
            room.post(format!("churn-{n}"), RoomMessageSender::Operator {}, "x".into(), vec![], 1).unwrap();
        }
        let (retained, receipts, more) = room.retained_for(recipient, None).unwrap();
        assert_eq!((retained.len(), receipts.len(), more), (1, 1, None));
        assert_eq!(receipts[0].status, RoomDeliveryStatus::Stored, "the reader's own receipt comes with it");
        assert_eq!(retained[0].cursor, owed.cursor);
        assert_eq!(room.retained_for(Uuid::new_v4(), None).unwrap().0, Vec::new(), "never another member's");
    }

    /// M1 negative journey on 0b708ae: a fresh member's first read (no cursor)
    /// got the oldest page and paged 233 events before reaching what it owed.
    /// The owed message after that page now comes back on the first read.
    #[test]
    fn a_read_without_a_cursor_returns_what_the_reader_owes_after_the_page() {
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "First read", vec![recipient]));
        room.open_log(dir.path()).unwrap();
        for n in 0..ROOM_FEED_PAGE_MAX_EVENTS * 2 {
            room.post(format!("history-{n}"), RoomMessageSender::Operator {}, "x".into(), vec![], 1).unwrap();
        }
        let (owed, _) = room.post_requesting_acks("owed".into(), RoomMessageSender::Operator {}, "owed body".into(),
            vec![recipient], 1).unwrap();
        // A fresh member's first page, and an old saved cursor far behind:
        // either way the owed message comes with the read.
        for cursor in [None, Some(RoomFeedCursor { epoch: owed.cursor.epoch, sequence: 3 })] {
            let page = room.read(cursor, 0).unwrap();
            assert!(!page.events.iter().any(|event| event.cursor == owed.cursor), "not on this page");
            let (retained, receipts, next) = room.retained_for(recipient, None).unwrap();
            assert_eq!((retained.iter().map(|event| event.cursor).collect::<Vec<_>>(), receipts.len(), next),
                (vec![owed.cursor], 1, None), "returned with the read ({cursor:?})");
        }
        assert!(room.retained_for(Uuid::new_v4(), None).unwrap().0.is_empty(), "never another member's");
        let foreign = RoomFeedCursor { epoch: Uuid::new_v4(), sequence: 1 };
        assert!(room.retained_for(recipient, Some(foreign)).is_err(), "a continuation from another history is refused");
        let ahead = RoomFeedCursor { epoch: owed.cursor.epoch, sequence: owed.cursor.sequence + 100 };
        assert!(room.retained_for(recipient, Some(ahead)).is_err(), "a continuation ahead of the feed is refused");
    }

    /// Reviewer d68ddeea: a Held message that ends a page, with its Held
    /// receipt on the next page, still comes with its current hold reason; a
    /// body on the page and in retained is recorded as read once.
    #[test]
    fn an_owed_message_on_the_page_keeps_its_hold_reason_and_one_fetched() {
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Boundary", vec![recipient]));
        room.open_log(dir.path()).unwrap();
        for n in 0..ROOM_FEED_PAGE_MAX_EVENTS - 1 {
            room.post(format!("before-{n}"), RoomMessageSender::Operator {}, "x".into(), vec![], 1).unwrap();
        }
        let (held, _) = room.post_requesting_acks("held".into(), RoomMessageSender::Operator {}, "held body".into(),
            vec![recipient], 1).unwrap();
        let RoomFeedItem::Message { message_id, .. } = held.item.clone() else { unreachable!() };
        room.append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, pointer: None, message_id, recipient_id: recipient,
            status: RoomDeliveryStatus::Held, bytes_written: 0, zero_write_confirmed: false,
            error: Some("sender revoked".into()), run_id: None, generation: None, readmit: false }).unwrap();
        let mut page = room.read(None, 0).unwrap();
        assert_eq!(page.events.last().map(|event| event.cursor), Some(held.cursor), "the message ends the page");
        assert!(!page.events.iter().any(|event| matches!(event.item, RoomFeedItem::Delivery { .. })), "its Held is on the next page");
        let (retained, receipts, _) = room.retained_for(recipient, None).unwrap();
        assert_eq!(retained.iter().map(|event| event.cursor).collect::<Vec<_>>(), vec![held.cursor]);
        assert_eq!((receipts[0].status, receipts[0].hold.as_deref()), (RoomDeliveryStatus::Held, Some("sender revoked")),
            "the current restriction travels with the body");
        (page.retained, page.retained_receipts) = (retained, receipts);
        let fetched = room.record_fetched_page(&page, recipient, Uuid::new_v4(), 1).unwrap();
        assert_eq!(fetched.len(), 1, "one read, one Fetched");
    }

    /// Supervisor 0330c9ef: a large owed set, or an early batch read but not
    /// ACKed, never starves later requests: explicit continuation.
    #[test]
    fn owed_messages_continue_explicitly_past_a_full_batch() {
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Many owed", vec![recipient]));
        room.open_log(dir.path()).unwrap();
        let owed: Vec<_> = (0..ROOM_FEED_PAGE_MAX_EVENTS + 6).map(|n| room.post_requesting_acks(format!("o-{n}"),
            RoomMessageSender::Operator {}, "owed".into(), vec![recipient], 1).unwrap().0.cursor).collect();
        let (first, _, next) = room.retained_for(recipient, None).unwrap();
        assert_eq!(first.len(), ROOM_FEED_PAGE_MAX_EVENTS);
        let next = next.expect("more remain");
        assert_eq!(Some(next), first.last().map(|event| event.cursor));
        let (rest, _, after) = room.retained_for(recipient, Some(next)).unwrap();
        assert_eq!(rest.iter().map(|event| event.cursor).collect::<Vec<_>>(), owed[ROOM_FEED_PAGE_MAX_EVENTS..].to_vec());
        assert_eq!(after, None);
    }

    #[test]
    fn pinned_bytes_never_push_the_post_index_past_its_byte_bound() {
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Bytes", vec![recipient]));
        room.open_log(dir.path()).unwrap();
        // Quotes and backslashes double when serialized: the check uses the
        // exact event size, not the body length.
        let big = "\"\\".repeat(225 * 1024);
        let mut accepted = 0;
        for n in 0..40 {
            match room.post(format!("big-{n}"), RoomMessageSender::Operator {}, big.clone(), vec![recipient], 1) {
                Ok(_) => accepted += 1,
                Err(error) => assert!(error.to_string().contains("still waiting"), "{error}"),
            }
            assert!(room.post_bytes <= ROOM_DEDUP_MAX_BYTES, "after {n}: {}", room.post_bytes);
        }
        assert!(accepted > 0 && accepted < 40, "later posts are refused before append: {accepted}");
        assert_eq!(room.unreceipted_messages().len(), accepted, "no accepted waiting message is ever lost");
    }

    fn retry_receipt(message_id: Uuid, recipient_id: SessionId, status: RoomDeliveryStatus,
        bytes_written: usize, zero_write_confirmed: bool) -> RoomFeedItem {
        RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, readmit: false, pointer: None, message_id, recipient_id, status, bytes_written,
            zero_write_confirmed, error: None, run_id: None, generation: None }
    }

    #[test]
    fn retry_snapshot_accepts_only_durable_zero_write_eligible_receipts_after_replay() {
        use RoomDeliveryStatus::*;
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let definition = definition(Uuid::new_v4(), "Retry eligibility", vec![recipient]);
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        let cases = [
            (Held, 0, false, true),
            (Held, 1, true, false),
            (Failed, 0, false, false),
            (Failed, 0, true, true),
            (Failed, 1, true, false),
            (Stored, 0, true, false),
            (Pending, 0, true, false),
            (Written, 0, true, false),
            (Woken, 0, true, false),
            (Fetched, 0, true, false),
        ];
        let mut committed = Vec::new();
        for (index, (status, bytes, confirmed, eligible)) in cases.into_iter().enumerate() {
            let (message, _) = room.post(format!("retry-{index}"), RoomMessageSender::Operator {},
                format!("original body {index}"), vec![recipient], 1).unwrap();
            let message_id = room.delivery_result(&message, false).message_id;
            let receipt = room.append(retry_receipt(message_id, recipient, status, bytes, confirmed)).unwrap();
            committed.push((message, message_id, receipt.cursor, status, eligible));
        }
        for replay in [false, true] {
            if replay {
                drop(room);
                room = RoomRuntime::from_persisted(definition.clone());
                room.open_log(dir.path()).unwrap();
            }
            let before = room.next_sequence;
            for (message, message_id, cursor, status, eligible) in &committed {
                let result = room.retry_snapshot(*message_id, recipient, *cursor);
                if *eligible {
                    let (original, receipt) = result.unwrap();
                    assert_eq!(&original, message, "stored body and targets must remain exact");
                    let receipt = receipt.expect("the current eligible receipt authorizes admission");
                    assert!(receipt.recorded);
                    assert_eq!(receipt.status, *status);
                    assert_eq!(receipt.bytes_written, 0);
                } else {
                    assert!(result.unwrap_err().to_string().contains("cannot be safely retried"),
                        "unexpected result for {status:?}, replay={replay}");
                }
            }
            assert_eq!(room.next_sequence, before);
        }
    }

    #[test]
    fn retry_snapshot_stale_claim_reads_pending_without_new_event_even_after_replay() {
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let definition = definition(Uuid::new_v4(), "Pending claim", vec![recipient]);
        let path = dir.path().join("rooms").join(format!("{}.jsonl", definition.room_id));
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        let (message, _) = room.post("retry-pending".into(), RoomMessageSender::Operator {},
            "keep this stored body".into(), vec![recipient], 1).unwrap();
        let message_id = room.delivery_result(&message, false).message_id;
        assert!(room.retry_snapshot(message_id, recipient, message.cursor)
            .unwrap_err().to_string().contains("no safe retry receipt"));
        let held = room.append(retry_receipt(message_id, recipient, RoomDeliveryStatus::Held, 0, false)).unwrap();
        assert!(room.retry_snapshot(message_id, recipient, held.cursor).unwrap().1.is_some());
        let pending = room.append(retry_receipt(message_id, recipient, RoomDeliveryStatus::Pending, 0, false)).unwrap();
        // Windows locks the journal's byte range against other handles. Read
        // its bytes only while the writer is closed, never bypass that lock.
        drop(room);
        let before_bytes = std::fs::read(&path).unwrap();
        room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        for replay in [false, true] {
            if replay {
                drop(room);
                room = RoomRuntime::from_persisted(definition.clone());
                room.open_log(dir.path()).unwrap();
            }
            let before_sequence = room.next_sequence;
            let (original, claim) = room.retry_snapshot(message_id, recipient, held.cursor).unwrap();
            assert_eq!(original, message);
            assert!(claim.is_none(), "stale held cursor must not claim a second attempt");
            let current = room.delivery_result(&original, true);
            assert_eq!(current.receipts[0].status, RoomDeliveryStatus::Pending);
            assert!(current.receipts[0].recorded);
            assert!(room.retry_snapshot(message_id, recipient, pending.cursor)
                .unwrap_err().to_string().contains("cannot be safely retried"));
            assert_eq!(room.next_sequence, before_sequence);
        }
        drop(room);
        assert_eq!(std::fs::read(&path).unwrap(), before_bytes);
    }

    #[test]
    fn retry_snapshot_requires_original_target_current_membership_and_join_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let bystander = Uuid::new_v4();
        let definition = definition(Uuid::new_v4(), "Retry authority", vec![recipient, bystander]);
        let mut room = RoomRuntime::from_persisted(definition);
        room.open_log(dir.path()).unwrap();
        let (message, _) = room.post("retry-authority".into(), RoomMessageSender::Operator {},
            "original audience only".into(), vec![recipient], 1).unwrap();
        let message_id = room.delivery_result(&message, false).message_id;
        let held = room.append(retry_receipt(message_id, recipient, RoomDeliveryStatus::Held, 0, false)).unwrap();
        assert!(room.retry_snapshot(message_id, bystander, held.cursor)
            .unwrap_err().to_string().contains("original authorized wake targets"));
        room.definition.member_ids.retain(|id| *id != recipient);
        assert!(room.retry_snapshot(message_id, recipient, held.cursor)
            .unwrap_err().to_string().contains("no longer a room member"));
        room.definition.member_ids.push(recipient);
        room.join_floor_by_session.insert(recipient, message.cursor.sequence);
        assert!(room.retry_snapshot(message_id, recipient, held.cursor)
            .unwrap_err().to_string().contains("predates"));
        room.join_floor_by_session.insert(recipient, message.cursor.sequence - 1);
        assert!(room.retry_snapshot(message_id, recipient, held.cursor).unwrap().1.is_some());
    }

    #[test]
    fn fetch_receipt_is_scoped_idempotent_and_does_not_claim_a_wake() {
        let dir = tempfile::tempdir().unwrap();
        let recipient = Uuid::new_v4();
        let definition = definition(Uuid::new_v4(), "Fetch", vec![recipient]);
        let mut room = RoomRuntime::from_persisted(definition.clone());
        room.open_log(dir.path()).unwrap();
        let (event, _) = room.post("read-1".into(), RoomMessageSender::Operator {}, "hello".into(), vec![recipient], 1).unwrap();
        let result = room.delivery_result(&event, false);
        assert_eq!(result.receipts[0].status, RoomDeliveryStatus::Stored);
        assert!(room.needs_fetched_receipt(result.message_id, recipient));
        assert!(!room.needs_fetched_receipt(result.message_id, Uuid::new_v4()));
        room.append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, readmit: false, pointer: None, message_id: result.message_id, recipient_id: recipient,
            status: RoomDeliveryStatus::Fetched, bytes_written: 0, zero_write_confirmed: false,
            error: None, run_id: None, generation: None }).unwrap();
        assert!(!room.needs_fetched_receipt(result.message_id, recipient));
        assert_eq!(room.delivery_result(&event, true).written_count, 0);
        drop(room);
        let mut room = RoomRuntime::from_persisted(definition);
        room.open_log(dir.path()).unwrap();
        assert!(!room.needs_fetched_receipt(result.message_id, recipient));
    }

    #[test]
    fn same_label_rooms_remain_isolated_by_room_id() {
        let sessions = [
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        ];
        let room_a = Uuid::new_v4();
        let room_b = Uuid::new_v4();
        let state = RoomState::from_catalog(
            RoomCatalogV1 {
                schema_version: ROOM_CATALOG_SCHEMA_VERSION,
                rooms: vec![
                    definition(room_a, "Twin", sessions[..2].to_vec()),
                    definition(room_b, "Twin", sessions[2..].to_vec()),
                ],
            },
            &sessions.into_iter().collect(),
        )
        .unwrap();

        assert_eq!(state.order, vec![room_a, room_b]);
        assert_eq!(state.room_by_session[&sessions[0]], room_a);
        assert_eq!(state.room_by_session[&sessions[3]], room_b);
        assert_ne!(
            state.get(room_a).unwrap().feed_epoch,
            state.get(room_b).unwrap().feed_epoch
        );
    }

    #[test]
    fn duplicate_or_cross_room_membership_fails_closed() {
        let member = Uuid::new_v4();
        let known = HashSet::from([member]);
        for rooms in [
            vec![definition(
                Uuid::new_v4(),
                "Duplicate",
                vec![member, member],
            )],
            vec![
                definition(Uuid::new_v4(), "First", vec![member]),
                definition(Uuid::new_v4(), "Second", vec![member]),
            ],
        ] {
            assert!(
                RoomState::from_catalog(
                    RoomCatalogV1 {
                        schema_version: ROOM_CATALOG_SCHEMA_VERSION,
                        rooms,
                    },
                    &known,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn bounded_feed_reports_exact_eviction_gap_and_pages() {
        let room_id = Uuid::new_v4();
        let mut room = RoomRuntime::from_persisted(definition(room_id, "Bounded", Vec::new()));
        for index in 0..(ROOM_FEED_MAX_EVENTS + 17) {
            room.append(message(index, format!("event-{index}")))
                .unwrap();
        }

        assert_eq!(room.feed.len(), ROOM_FEED_MAX_EVENTS);
        let first = room.read(None, 0).unwrap();
        assert_eq!(
            first.gap,
            Some(RoomFeedGap {
                reason: RoomFeedGapReason::Evicted,
                from_sequence: Some(1),
                through_sequence: Some(17),
            })
        );
        assert_eq!(first.events.len(), ROOM_FEED_PAGE_MAX_EVENTS);
        assert!(first.has_more);
        let second = room.read(Some(first.cursor), 0).unwrap();
        assert_eq!(second.events[0].cursor.sequence, first.cursor.sequence + 1);
    }

    #[test]
    fn feed_byte_bound_evicts_large_messages_without_exceeding_limit() {
        let mut room =
            RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Byte bounded", Vec::new()));
        for index in 0..24 {
            room.append(message(index, "x".repeat(1024 * 1024)))
                .unwrap();
        }
        assert!(room.feed_bytes <= ROOM_FEED_MAX_BYTES);
        assert!(room.feed.len() < 24);
        assert_eq!(
            room.read(None, 0).unwrap().gap.unwrap().reason,
            RoomFeedGapReason::Evicted
        );
    }

    #[test]
    fn join_floor_exposes_only_future_feed_traffic_including_join_event() {
        let member = Uuid::new_v4();
        let mut room =
            RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Future only", Vec::new()));
        room.append(message(0, "private-before-join".into()))
            .unwrap();
        let floor = room.next_sequence - 1;
        room.append(RoomFeedItem::Membership {
            action: RoomMembershipAction::Joined,
            session_id: member,
            membership_revision: 2,
        })
        .unwrap();
        room.append(message(1, "after-join".into())).unwrap();

        let page = room.read(None, floor).unwrap();
        assert_eq!(page.events.len(), 2);
        assert!(matches!(
            page.events[0].item,
            RoomFeedItem::Membership { .. }
        ));
        assert!(matches!(page.events[1].item, RoomFeedItem::Message { .. }));
        assert!(
            !serde_json::to_string(&page)
                .unwrap()
                .contains("private-before-join")
        );
    }

    #[test]
    fn stale_epoch_is_explicit_and_cursor_ahead_is_rejected() {
        let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Epoch", Vec::new()));
        room.append(message(0, "current".into())).unwrap();
        let stale = room
            .read(
                Some(RoomFeedCursor {
                    epoch: Uuid::new_v4(),
                    sequence: 1,
                }),
                0,
            )
            .unwrap();
        assert_eq!(stale.gap.unwrap().reason, RoomFeedGapReason::EpochReset);
        for index in 1..(ROOM_FEED_MAX_EVENTS + 8) {
            room.append(message(index, format!("evict-{index}")))
                .unwrap();
        }
        let stale_after_eviction = room
            .read(
                Some(RoomFeedCursor {
                    epoch: Uuid::new_v4(),
                    sequence: 0,
                }),
                0,
            )
            .unwrap();
        assert_eq!(
            stale_after_eviction.gap.unwrap().reason,
            RoomFeedGapReason::EpochReset,
            "an eviction in the replacement epoch must not hide the epoch reset"
        );
        assert!(
            room.read(
                Some(RoomFeedCursor {
                    epoch: room.feed_epoch,
                    sequence: room.next_sequence,
                }),
                0,
            )
            .unwrap_err()
            .to_string()
            .contains("ahead")
        );
    }
}
