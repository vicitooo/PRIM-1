//! A109 room mobility: a member the operator lets roam lists every room,
//! peeks at any feed without joining it, moves itself between rooms with a
//! notice in each, and delivers to members of other rooms. A move is one
//! room-catalog write (the operator's add/remove membership steps together),
//! and every effect rechecks the caller's run and grant after its waits. A
//! member that receives such a delivery may answer it without roam for a
//! bounded time. "maintain" is stored here; the A108 upgrade request
//! (`upgrade_flow.rs`) checks it.
use super::*;
use shared_types::{RoomDirectoryEntry, RoomFeedCursor, RoomMembershipChange, RoomMoveResult};

const ROAM_REFUSAL: &str = "the operator has not let this member roam: rooms_list, room_peek, room_join, room_leave and room_deliver with room_id need \"Can roam\" in its Edit dialog";

/// How a message from outside the recipient's room is headed: "<label>
/// (visiting from <room>)".
const VISITING_FROM: &str = " (visiting from ";

/// A member that received a cross-room delivery may answer its sender this
/// long after it, with plain room_deliver and without roam.
const CROSS_ROOM_REPLY_HOURS: i64 = 24;

/// A pane's recipient that names no member of its room at all: not
/// ambiguous, not the caller. Only this refusal may become an answer to a
/// visitor.
#[derive(Debug)]
pub(super) struct NoRoomMemberNamed(pub(super) String);

impl std::fmt::Display for NoRoomMemberNamed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for NoRoomMemberNamed {}

pub(super) fn is_mobility_request(request: &SidebandRequest) -> bool {
    matches!(request, SidebandRequest::MemberPermissions {} | SidebandRequest::RoomsList {}
        | SidebandRequest::RoomPeek { .. } | SidebandRequest::RoomJoin { .. } | SidebandRequest::RoomLeave {})
}

/// Prime and Terminal panes have no room tools for a grant to unlock.
pub(super) fn grants_apply_to(driver: DriverKind) -> bool {
    matches!(driver, DriverKind::Claude | DriverKind::Codex | DriverKind::Grok)
}

impl SupervisorHandle {
    /// The operator's Edit dialog, and (A16) a managing member's
    /// member_update. Takes effect for the next sideband request; a member
    /// keeps the room it is in when roam is withdrawn.
    pub fn set_member_permissions(&self, session_id: SessionId, permissions: MemberPermissions) -> Result<SessionSnapshot> {
        self.set_member_permissions_checked(session_id, permissions, None)
    }

    /// `check`: a managing pane's member_update (A16), rechecked where the
    /// grants commit. No ceiling (Victor, 20 Oct): a managing member may give
    /// any of the three, also to itself.
    pub(super) fn set_member_permissions_checked(
        &self, session_id: SessionId, permissions: MemberPermissions, check: Option<&member_lifecycle::ManageCheck<'_>>,
    ) -> Result<SessionSnapshot> {
        self.ensure_active()?;
        let (snapshot, old_permissions) = {
            let rooms = check.map(|_| self.inner.rooms.lock());
            let mut slots = self.inner.slots.lock();
            let mut catalog = self.inner.catalog.lock();
            if let (Some(check), Some(rooms)) = (check, rooms.as_deref()) {
                check.check_editable(rooms, &slots, session_id)?;
            }
            let slot = slots.get_by_id(session_id).ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
            let old_permissions = slot.permissions;
            if old_permissions == permissions {
                return Ok(slot.snapshot());
            }
            // Clearing stays allowed, so a grant stored before this check can be removed.
            if !permissions.is_default() && !grants_apply_to(slot.definition.driver) {
                bail!("{} panes have no room tools, so \"Can roam\", \"Can upgrade PRIM-1\" and \"Can manage agents\" do nothing for them",
                    driver_display_name(slot.definition.driver));
            }
            let mut candidate = catalog.clone();
            candidate.sessions.iter_mut().find(|session| session.session_id == session_id)
                .ok_or_else(|| anyhow!("session '{session_id}' is missing from the catalog"))?
                .permissions = permissions;
            self.persist_catalog_candidate(&candidate)?;
            let slot = slots.get_by_id_mut(session_id).expect("validated session disappeared during permission update");
            slot.permissions = permissions;
            let snapshot = slot.snapshot();
            *catalog = candidate;
            (snapshot, old_permissions)
        };
        self.emit(RuntimeEvent::SessionMemberPermissionsChanged {
            schema_version: shared_types::SESSION_EVENT_SCHEMA_VERSION,
            session_id,
            old_permissions,
            new_permissions: permissions,
            timestamp: now_rfc3339(),
        });
        Ok(snapshot)
    }

    pub(super) fn member_may_roam_locked(slots: &SessionRegistry, session_id: SessionId) -> bool {
        Self::member_grants_locked(slots, session_id).roam
    }

    pub(super) fn member_grants_locked(slots: &SessionRegistry, session_id: SessionId) -> MemberPermissions {
        slots.get_by_id(session_id).map(|slot| slot.permissions).unwrap_or_default()
    }

    /// The caller's current run, and its roam grant.
    pub(super) fn pane_roam_locked(caller: &PaneCaller, slots: &SessionRegistry) -> Result<()> {
        Self::validate_pane_caller_locked(caller, slots)?;
        if Self::member_may_roam_locked(slots, caller.session_id) { Ok(()) } else { Err(anyhow!(ROAM_REFUSAL)) }
    }

    /// `room_deliver` with room_id needs roam. True when the room is not the
    /// caller's own, so the delivery admits a sender from outside it.
    pub(super) fn pane_delivery_room_locked(
        caller: &PaneCaller, slots: &SessionRegistry, rooms: &RoomState, target: RoomId,
    ) -> Result<bool> {
        Self::pane_roam_locked(caller, slots)?;
        if rooms.room_by_session.get(&caller.session_id) == Some(&target) {
            Self::pane_room_binding_locked(caller, slots, rooms)?;
            return Ok(false);
        }
        rooms.get(target).ok_or_else(|| anyhow!("unknown room id '{target}'; rooms_list names every room"))?;
        Ok(true)
    }

    /// "<label> (visiting from <room>)", or "... from the lobby)"; None for an
    /// unknown session.
    pub(super) fn visiting_label_locked(rooms: &RoomState, slots: &SessionRegistry, session_id: SessionId) -> Option<String> {
        let label = &slots.get_by_id(session_id)?.definition.label;
        let from = rooms.room_by_session.get(&session_id).and_then(|room_id| rooms.get(*room_id))
            .map_or_else(|| "the lobby".to_string(), |room| room.definition.label.clone());
        Some(format!("{label}{VISITING_FROM}{from})"))
    }

    /// The label a stored session message is typed under when it is woken
    /// again (Retry wake, the restart queue): a sender outside the message's
    /// room is shown as visiting, as at its first delivery.
    pub(super) fn session_sender_label(&self, room_id: RoomId, session_id: SessionId) -> String {
        let rooms = self.inner.rooms.lock();
        let slots = self.inner.slots.lock();
        let member = rooms.get(room_id).is_some_and(|room| room.definition.member_ids.contains(&session_id));
        let label = if member {
            slots.get_by_id(session_id).map(|slot| slot.definition.label.clone())
        } else {
            Self::visiting_label_locked(&rooms, &slots, session_id)
        };
        label.unwrap_or_else(|| session_id.to_string())
    }

    fn reply_window_open(at: chrono::DateTime<chrono::Utc>) -> bool {
        chrono::Utc::now().signed_duration_since(at) < chrono::Duration::hours(CROSS_ROOM_REPLY_HOURS)
    }

    /// Called with the delivery committed: each recipient may answer `sender`.
    pub(super) fn note_cross_room_delivery_locked(rooms: &mut RoomState, sender: SessionId, recipients: &[SessionId]) {
        let now = chrono::Utc::now();
        rooms.cross_room_senders.retain(|_, at| Self::reply_window_open(*at));
        for recipient in recipients {
            rooms.cross_room_senders.insert((*recipient, sender), now);
        }
    }

    /// A reply to `visitor` is admitted while it delivered to `recipient_id`
    /// within the window, still exists, still roams and is in a room. Returns
    /// that room: the reply is stored and typed there.
    fn reply_admitted_locked(rooms: &RoomState, slots: &SessionRegistry, recipient_id: SessionId,
        visitor: SessionId) -> Result<RoomId> {
        if !rooms.cross_room_senders.get(&(recipient_id, visitor)).is_some_and(|at| Self::reply_window_open(*at)) {
            bail!("no reply window: that member of another room last messaged you before PRIM-1 restarted or over {CROSS_ROOM_REPLY_HOURS} hours ago; post in your own room (room_post)");
        }
        let slot = slots.get_by_id(visitor)
            .ok_or_else(|| anyhow!("the member of another room that messaged you no longer exists; post in your own room (room_post)"))?;
        let label = &slot.definition.label;
        if !slot.permissions.roam {
            bail!("\"{label}\" messaged you from another room, but the operator no longer lets it roam, so it cannot be answered there; post in your own room (room_post)");
        }
        rooms.room_by_session.get(&visitor).copied().ok_or_else(|| anyhow!(
            "\"{label}\" is in the lobby now; post your answer in your own room (room_post), where it can read it with room_peek"))
    }

    /// The roaming member `wanted` names (its label, its label as typed in a
    /// message header, or its session id) that delivered to `recipient_id`
    /// from another room within the window, and the room it is in now.
    /// None when no such visitor has that name.
    pub(super) fn reply_visitor_locked(rooms: &RoomState, slots: &SessionRegistry, recipient_id: SessionId,
        wanted: &str) -> Result<Option<(SessionId, RoomId)>> {
        let wanted = wanted.trim();
        let wanted = wanted.split_once(VISITING_FROM).map_or(wanted, |(label, _)| label.trim_end());
        if wanted.is_empty() || wanted.eq_ignore_ascii_case("all") {
            return Ok(None);
        }
        let by_id = Uuid::parse_str(wanted).ok();
        let visitors: Vec<SessionId> = rooms.cross_room_senders.iter()
            .filter(|((recipient, _), at)| *recipient == recipient_id && Self::reply_window_open(**at))
            .map(|((_, visitor), _)| *visitor)
            .filter(|visitor| by_id == Some(*visitor) || slots.get_by_id(*visitor)
                .is_some_and(|slot| slot.definition.label.trim().eq_ignore_ascii_case(wanted)))
            .collect();
        let visitor = match visitors.as_slice() {
            [] => return Ok(None),
            [visitor] => *visitor,
            _ => bail!("'{wanted}' names more than one member of another room that messaged you; use its session id"),
        };
        Self::reply_admitted_locked(rooms, slots, recipient_id, visitor).map(|room_id| Some((visitor, room_id)))
    }

    /// When a name that matched no one in the caller's room is a session in
    /// another room or the lobby: why it cannot be answered. The reply window
    /// is kept in memory, so a restart closes it as surely as 24 hours do.
    pub(super) fn closed_reply_window_locked(rooms: &RoomState, slots: &SessionRegistry, caller_id: SessionId,
        wanted: &str) -> Option<String> {
        let wanted = wanted.trim();
        let wanted = wanted.split_once(VISITING_FROM).map_or(wanted, |(label, _)| label.trim_end());
        let by_id = Uuid::parse_str(wanted).ok();
        let own_room = rooms.room_by_session.get(&caller_id);
        let elsewhere = slots.ordered_slots().into_iter().find(|slot| slot.session_id != caller_id
            && rooms.room_by_session.get(&slot.session_id) != own_room
            && (by_id == Some(slot.session_id) || slot.definition.label.trim().eq_ignore_ascii_case(wanted)))?;
        Some(format!("no reply window to \"{}\" in another room: its last message to you was before PRIM-1 restarted or over {CROSS_ROOM_REPLY_HOURS} hours ago, or it never messaged you; post in your own room (room_post)",
            elsewhere.definition.label))
    }

    /// Commit-time check of a reply: the caller's current run, and the
    /// visitor's standing.
    pub(super) fn pane_reply_locked(caller: &PaneCaller, slots: &SessionRegistry, rooms: &RoomState,
        visitor: SessionId) -> Result<()> {
        Self::validate_pane_caller_locked(caller, slots)?;
        Self::reply_admitted_locked(rooms, slots, caller.session_id, visitor).map(|_| ())
    }

    pub(super) fn room_delivery_response(outcome: Result<RoomDeliveryResult>) -> SidebandResponse {
        match outcome {
            Ok(result) => SidebandResponse {
                ok: true,
                message: format!(
                    "room message stored; wake written for {} of {} recipients (not model acceptance)",
                    result.written_count, result.recipient_count
                ),
                timed_out: false,
                payload: Some(SidebandResponsePayload::RoomDelivery { result }),
                request_id: None,
            },
            Err(error) => Self::rejected_sideband_response(error.to_string()),
        }
    }

    /// Runs on a blocking worker: a move writes two room journals.
    pub(super) fn apply_mobility_request(&self, caller: &PaneCaller, request: SidebandRequest) -> SidebandResponse {
        if let Err(error) = self.authorize_sideband_request(caller, &request) {
            return Self::rejected_sideband_response(error.to_string());
        }
        #[cfg(test)]
        self.run_sideband_after_initial_authorization_for_tests();
        let outcome = match request {
            SidebandRequest::MemberPermissions {} => self.pane_permissions(caller).map(|permissions| (
                "this member's operator grants".to_string(), SidebandResponsePayload::MemberPermissions { permissions })),
            SidebandRequest::RoomsList {} => self.list_rooms_for_pane(caller).map(|(current_room_id, rooms)| {
                // The text states where the caller is; a fixed lobby phrase read as "null" (19 Oct walk).
                let current = current_room_id.and_then(|id| rooms.iter().find(|room| room.room_id == id));
                let message = match current {
                    Some(room) => format!("{} rooms; you are in \"{}\" (current_room_id {})", rooms.len(), room.label, room.room_id),
                    None => format!("{} rooms; you are in the lobby (current_room_id is null)", rooms.len()),
                };
                (message, SidebandResponsePayload::Rooms { current_room_id, rooms })
            }),
            SidebandRequest::RoomPeek { room_id, cursor } => self.read_room_feed_as(ReadRoomFeedRequest { room_id, cursor },
                Some(caller)).map(|page| (
                "room feed read without joining; no receipt recorded".to_string(), SidebandResponsePayload::RoomFeed { page })),
            SidebandRequest::RoomJoin { room_id } => self.join_room_from_pane(caller, room_id).map(|result| {
                let joined = result.joined.as_ref().map(|room| room.label.as_str()).unwrap_or_default();
                (format!("joined room \"{joined}\"; room_read now reads it"), SidebandResponsePayload::RoomMove { result })
            }),
            SidebandRequest::RoomLeave {} => self.leave_room_from_pane(caller).map(|result|
                ("left for the lobby".to_string(), SidebandResponsePayload::RoomMove { result })),
            _ => return Self::rejected_sideband_response("not a room mobility request"),
        };
        match outcome {
            Ok((message, payload)) => SidebandResponse { ok: true, message, timed_out: false, payload: Some(payload), request_id: None },
            Err(error) => Self::rejected_sideband_response(error.to_string()),
        }
    }

    fn pane_permissions(&self, caller: &PaneCaller) -> Result<MemberPermissions> {
        let slots = self.inner.slots.lock();
        Self::validate_pane_caller_locked(caller, &slots)?;
        Ok(slots.get_by_id(caller.session_id).map(|slot| slot.permissions).unwrap_or_default())
    }

    fn list_rooms_for_pane(&self, caller: &PaneCaller) -> Result<(Option<RoomId>, Vec<RoomDirectoryEntry>)> {
        self.list_rooms(Some(caller))
    }

    /// Every room. `caller`: a pane, which needs roam; None is the operator
    /// entry (A16), which is in no room.
    pub(super) fn list_rooms(&self, caller: Option<&PaneCaller>) -> Result<(Option<RoomId>, Vec<RoomDirectoryEntry>)> {
        let (current, mut entries) = {
            let rooms = self.inner.rooms.lock();
            let slots = self.inner.slots.lock();
            if let Some(caller) = caller {
                Self::pane_roam_locked(caller, &slots)?;
            }
            let entries: Vec<RoomDirectoryEntry> = rooms.order.iter().filter_map(|room_id| rooms.get(*room_id))
                .map(|room| RoomDirectoryEntry {
                    room_id: room.definition.room_id,
                    label: room.definition.label.clone(),
                    members: Self::room_members_locked(room, &slots),
                    guest_count: 0,
                    cursor: RoomFeedCursor { epoch: room.feed_epoch, sequence: room.next_sequence.saturating_sub(1) },
                })
                .collect();
            (caller.and_then(|caller| rooms.room_by_session.get(&caller.session_id).copied()), entries)
        };
        // The guest registry is a leaf lock, taken after the rooms are released.
        for entry in &mut entries {
            entry.guest_count = self.room_guests(entry.room_id).len();
        }
        Ok((current, entries))
    }

    fn join_room_from_pane(&self, caller: &PaneCaller, target: RoomId) -> Result<RoomMoveResult> {
        let (label, from, target_label) = {
            let rooms = self.inner.rooms.lock();
            let slots = self.inner.slots.lock();
            Self::pane_roam_locked(caller, &slots)?;
            let room = rooms.get(target).ok_or_else(|| anyhow!("unknown room id '{target}'; rooms_list names every room"))?;
            let target_label = room.definition.label.clone();
            let from = rooms.room_by_session.get(&caller.session_id).copied();
            if from == Some(target) {
                bail!("this member is already in room \"{target_label}\"");
            }
            if room.definition.member_ids.len() >= ROOM_MEMBER_MAX_COUNT {
                bail!("room \"{target_label}\" has reached the {ROOM_MEMBER_MAX_COUNT} member limit");
            }
            let from = from.map(|room_id| (room_id, rooms.get(room_id).map(|room| room.definition.label.clone()).unwrap_or_default()));
            (Self::member_label_locked(&slots, caller), from, target_label)
        };
        // Leave and join in one catalog write: a refused join leaves the
        // member where it was, never in the lobby.
        let change = self.change_room_membership(caller.session_id, from.as_ref().map(|(room_id, _)| *room_id),
            Some(target), Some(caller), None)
            .map_err(|error| anyhow!("could not move to room \"{target_label}\": {error}; nothing changed"))?;
        let mut result = RoomMoveResult { left: None, joined: None, warnings: Vec::new() };
        if let (Some((from_id, from_label)), Some(left)) = (&from, &change.left) {
            result.left = Some(RoomMembershipChange { room_id: *from_id, label: from_label.clone(),
                membership_revision: left.revision });
        }
        if let Some(joined) = &change.joined {
            result.joined = Some(RoomMembershipChange { room_id: joined.room_id, label: target_label.clone(),
                membership_revision: joined.revision });
        }
        if let Some((from_id, _)) = &from {
            self.post_mobility_notice(*from_id, caller.session_id, format!("{label} left for {target_label}"), &mut result.warnings);
        }
        self.post_mobility_notice(target, caller.session_id, format!("{label} joined"), &mut result.warnings);
        Ok(result)
    }

    fn leave_room_from_pane(&self, caller: &PaneCaller) -> Result<RoomMoveResult> {
        let (label, from_id, from_label) = {
            let rooms = self.inner.rooms.lock();
            let slots = self.inner.slots.lock();
            Self::pane_roam_locked(caller, &slots)?;
            let from_id = *rooms.room_by_session.get(&caller.session_id)
                .ok_or_else(|| anyhow!("this member is already in the lobby; room_join enters a room"))?;
            let from_label = rooms.get(from_id).map(|room| room.definition.label.clone()).unwrap_or_default();
            (Self::member_label_locked(&slots, caller), from_id, from_label)
        };
        let change = self.change_room_membership(caller.session_id, Some(from_id), None, Some(caller), None)
            .map_err(|error| anyhow!("could not leave room \"{from_label}\": {error}"))?;
        let left = change.left.expect("a leave reports the room it left");
        let mut result = RoomMoveResult {
            left: Some(RoomMembershipChange { room_id: from_id, label: from_label, membership_revision: left.revision }),
            joined: None,
            warnings: Vec::new(),
        };
        self.post_mobility_notice(from_id, caller.session_id, format!("{label} left"), &mut result.warnings);
        Ok(result)
    }

    pub(super) fn member_label_locked(slots: &SessionRegistry, caller: &PaneCaller) -> String {
        slots.get_by_id(caller.session_id).map(|slot| slot.definition.label.clone()).unwrap_or_else(|| caller.session.clone())
    }

    /// A stored post every member reads; it wakes no one. The move already
    /// happened, so a refused notice is reported rather than undoing it.
    fn post_mobility_notice(&self, room_id: RoomId, session_id: SessionId, content: String, warnings: &mut Vec<String>) {
        if let Err(error) = self.post_room_message_as(room_id, RoomMessageSender::Session { session_id }, content.clone(), None) {
            warnings.push(format!("notice \"{content}\" was not stored: {error}"));
        }
    }
}
