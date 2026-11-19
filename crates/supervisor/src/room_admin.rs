//! A16 room actions: a member the operator lets manage agents creates a room,
//! reads and changes its settings (label, Resume on restart, brief, position),
//! deletes it, and invites, lists and revokes outside agents, through the
//! operator's own supervisor methods. The caller's run and grants are checked
//! at admission and again where each effect commits (A110's `ManageCheck`);
//! another room also needs roam. Each effect is attributed in a room feed; a
//! retry with the same request_id returns the first result.
use super::*;
use member_lifecycle::{Actor, ManageCheck, PartialEffect, ReceiptClaim, ReceiptGuard};
use shared_types::{InviteRoomGuestRequest, ListRoomGuestsRequest, MessageId, RetryRoomDeliveryRequest, RevokeRoomGuestRequest,
    RoomAction, RoomActionResult, RoomFeedCursor};

pub(super) fn is_room_request(request: &SidebandRequest) -> bool {
    matches!(request, SidebandRequest::Room { .. })
}

fn room_action_fingerprint(action: &RoomAction) -> String {
    use sha2::{Digest, Sha256};
    let canonical = serde_json::to_string(action).expect("a room action always serializes");
    format!("{:x}", Sha256::digest(canonical.as_bytes()))
}

impl SupervisorHandle {
    /// Runs on a blocking worker: catalog and journal writes.
    pub(super) fn apply_room_request(&self, caller: &PaneCaller, request: SidebandRequest) -> SidebandResponse {
        if let Err(error) = self.authorize_sideband_request(caller, &request) {
            return Self::rejected_sideband_response(error.to_string());
        }
        let SidebandRequest::Room { request_id, action } = request else {
            return Self::rejected_sideband_response("not a room request");
        };
        let outcome = self.room_action(Actor::Pane(caller), request_id, action);
        match outcome {
            Ok(result) => SidebandResponse {
                ok: true,
                message: format!("room {} \"{}\": {}{}", result.op, result.room.label,
                    if result.replayed { "replayed the first result" } else { "done" },
                    if result.warnings.is_empty() { String::new() } else { format!("; {}", result.warnings.join("; ")) }),
                timed_out: false,
                payload: Some(SidebandResponsePayload::Room { result }),
                request_id: None,
            },
            Err(error) => Self::rejected_sideband_response(format!("{error:#}")),
        }
    }

    /// A read runs as is; a change runs once under its request_id. Shared by
    /// panes and the operator entry.
    pub(super) fn room_action(&self, caller: Actor<'_>, request_id: Option<String>, action: RoomAction)
        -> Result<RoomActionResult> {
        if action.is_read() {
            return match request_id {
                Some(_) => Err(anyhow!("{} is a read: send it without request_id", action.name())),
                None => self.run_room_action(caller, None, action),
            };
        }
        self.claim_room_action(caller, request_id, action)
    }

    fn claim_room_action(&self, caller: Actor<'_>, request_id: Option<String>, action: RoomAction)
        -> Result<RoomActionResult> {
        let request_id = request_id.as_deref().map(str::trim).and_then(|id| Uuid::parse_str(id).ok())
            .map(|id| id.to_string())
            .ok_or_else(|| anyhow!("{} needs request_id, a UUID: a fresh one per action, the same one only to retry it",
                action.name()))?;
        let fingerprint = room_action_fingerprint(&action);
        let claim = self.inner.room_receipts.lock().claim(caller.receipt_key(), &request_id, &fingerprint)?;
        match claim {
            ReceiptClaim::Replay(result) => Ok(RoomActionResult { replayed: true, ..result }),
            ReceiptClaim::Fresh => {
                let mut guard = ReceiptGuard::new(&self.inner.room_receipts, caller.receipt_key(), &request_id);
                let outcome = self.run_room_action(caller, Some(&request_id), action);
                // A guest token is shown once: a replay returns the guest
                // without it.
                guard.settle(&outcome.as_ref().map(|result| RoomActionResult { invitation: None, ..result.clone() })
                    .map_err(|error| match error.downcast_ref::<PartialEffect>() {
                        Some(partial) => PartialEffect(partial.0.clone()).into(),
                        None => anyhow!("{error:#}"),
                    }));
                outcome
            }
        }
    }

    fn run_room_action(&self, caller: Actor<'_>, request_id: Option<&str>, action: RoomAction)
        -> Result<RoomActionResult> {
        match action {
            RoomAction::Create { label, brief_on_join, brief_template, resume_on_restart } =>
                self.room_create(caller, request_id.expect("a mutation has a request_id"), label, brief_on_join,
                    brief_template, resume_on_restart),
            RoomAction::Get { room_id } => self.room_get(caller, room_id),
            RoomAction::Update { room_id, label, resume_on_restart, brief_on_join, brief_template, position } =>
                self.room_update(caller, request_id.expect("a mutation has a request_id"), room_id,
                    RoomEdit { label, resume_on_restart, brief_on_join, brief_template, position }),
            RoomAction::Delete { room_id } => self.room_delete(caller, room_id),
            RoomAction::Start { room_id } => self.room_run(caller, request_id.expect("a mutation has a request_id"), room_id, true),
            RoomAction::Stop { room_id } => self.room_run(caller, request_id.expect("a mutation has a request_id"), room_id, false),
            RoomAction::GuestInvite { room_id, label } =>
                self.room_guest_invite(caller, request_id.expect("a mutation has a request_id"), room_id, label),
            RoomAction::GuestRevoke { guest_id } =>
                self.room_guest_revoke(caller, request_id.expect("a mutation has a request_id"), guest_id),
            RoomAction::GuestList { room_id } => self.room_guest_list(caller, room_id),
            RoomAction::Retry { room_id, message_id, recipient, receipt } =>
                self.room_retry(caller, request_id.expect("a mutation has a request_id"), room_id, message_id, &recipient, receipt),
        }
    }

    /// The operator's Retry wake (A16): one recipient's held or failed wake
    /// of a stored message, attempted again; never a second post, never a
    /// recipient it already reached. Manage, and roam for another room; the
    /// grant is checked again where the retry is reserved.
    fn room_retry(&self, caller: Actor<'_>, request_id: &str, room_id: Option<RoomId>, message_id: MessageId,
        recipient: &str, receipt: RoomFeedCursor) -> Result<RoomActionResult> {
        let room_id = self.room_target(caller, room_id)?;
        let (_, caller_label) = self.admit_room(caller, room_id)?;
        let (recipient_id, recipient_label) = {
            let rooms = self.inner.rooms.lock();
            let slots = self.inner.slots.lock();
            let room = rooms.get(room_id).ok_or_else(|| anyhow!("unknown room id '{room_id}'"))?;
            let wanted = recipient.trim();
            let members: Vec<SessionId> = room.definition.member_ids.iter().copied()
                .filter(|id| Uuid::parse_str(wanted).ok() == Some(*id)
                    || slots.get_by_id(*id).is_some_and(|slot| slot.definition.label.trim().eq_ignore_ascii_case(wanted)))
                .collect();
            let [recipient_id] = members.as_slice() else {
                bail!("'{wanted}' names {} members of room \"{}\"; name one by label or session id",
                    if members.is_empty() { "no" } else { "more than one" }, room.definition.label);
            };
            (*recipient_id, slots.get_by_id(*recipient_id).map(|slot| slot.definition.label.clone()).unwrap_or_default())
        };
        let check = ManageCheck::of(caller, room_id);
        let delivery = self.retry_room_delivery_checked(RetryRoomDeliveryRequest { room_id, message_id, recipient_id,
            expected_receipt: receipt }, Some(&check))?;
        let mut warnings = Vec::new();
        if !delivery.replayed {
            self.post_room_notice(room_id, caller, format!("{request_id}.notice"),
                format!("{caller_label} retried the wake of message {message_id} for {recipient_label}"), &mut warnings);
        }
        let mut result = self.room_result(Some(request_id), "retry", room_id, warnings)?;
        result.delivery = Some(delivery);
        Ok(result)
    }

    /// The caller's room when `room_id` is omitted.
    fn room_target(&self, caller: Actor<'_>, room_id: Option<RoomId>) -> Result<RoomId> {
        match room_id {
            Some(room_id) => Ok(room_id),
            None => caller.own_room(&self.inner.rooms.lock()).ok_or_else(|| anyhow!(
                "you are in the lobby: name the room by room_id (rooms_list names every room)")),
        }
    }

    /// Admission: manage, and roam for a room that is not the caller's.
    fn admit_room(&self, caller: Actor<'_>, room_id: RoomId) -> Result<(String, String)> {
        let rooms = self.inner.rooms.lock();
        let slots = self.inner.slots.lock();
        ManageCheck::of(caller, room_id).check(&rooms, &slots)?;
        let room = rooms.get(room_id).ok_or_else(|| anyhow!("unknown room id '{room_id}'"))?;
        Ok((room.definition.label.clone(), caller.label_locked(&slots)))
    }

    fn room_result(&self, request_id: Option<&str>, op: &str, room_id: RoomId, warnings: Vec<String>)
        -> Result<RoomActionResult> {
        let room = self.inner.rooms.lock().get(room_id).map(RoomRuntime::snapshot)
            .ok_or_else(|| anyhow!("room '{room_id}' no longer exists"))?;
        Ok(RoomActionResult { request_id: request_id.map(Into::into), replayed: false, op: op.into(),
            room: self.fill_room_guests(room), brief: None, default_brief: None, position: None, guests: Vec::new(),
            members: Vec::new(), invitation: None, delivery: None, warnings })
    }

    /// A stored notice from the caller that wakes no one, keyed by the
    /// request so a retry never repeats it.
    fn post_room_notice(&self, room_id: RoomId, caller: Actor<'_>, key: String, content: String,
        warnings: &mut Vec<String>) {
        let sender = caller.sender();
        if let Err(error) = self.post_room_message_as(room_id, sender, content.clone(), Some(key)) {
            warnings.push(format!("notice \"{content}\" was not stored: {error}"));
        }
    }

    /// A new, empty room; the caller stays where it is. The new room and the
    /// caller's own room both record who created it.
    fn room_create(&self, caller: Actor<'_>, request_id: &str, label: String, brief_on_join: Option<bool>,
        brief_template: Option<String>, resume_on_restart: Option<bool>) -> Result<RoomActionResult> {
        validate_room_label(&label)?;
        let caller_label = {
            let slots = self.inner.slots.lock();
            caller.manage_locked(&slots)?;
            caller.label_locked(&slots)
        };
        let created = self.create_room_checked(CreateRoomRequest { label: Some(label.clone()), member_ids: Vec::new(),
            brief_on_join: brief_on_join.unwrap_or(true), brief_template }, Some(caller), resume_on_restart.unwrap_or(false))?;
        let mut warnings = Vec::new();
        self.post_room_notice(created.room_id, caller, format!("{request_id}.notice"),
            format!("{caller_label} created this room"), &mut warnings);
        // Bound first: posting takes the rooms lock again.
        let own_room = caller.own_room(&self.inner.rooms.lock());
        if let Some(own) = own_room {
            self.post_room_notice(own, caller, format!("{request_id}.notice-own"),
                format!("{caller_label} created room \"{label}\""), &mut warnings);
        }
        self.room_result(Some(request_id), "create", created.room_id, warnings)
    }

    fn room_get(&self, caller: Actor<'_>, room_id: Option<RoomId>) -> Result<RoomActionResult> {
        self.ensure_not_shutdown()?;
        let room_id = self.room_target(caller, room_id)?;
        self.admit_room(caller, room_id)?;
        let brief = self.read_room_brief(shared_types::ReadRoomBriefRequest { room_id })?;
        let position = self.inner.rooms.lock().order.iter().position(|id| *id == room_id);
        let result = self.room_result(None, "get", room_id, Vec::new())?;
        Ok(RoomActionResult { brief: Some(brief), default_brief: Some(default_room_brief_template()), position, ..result })
    }

    /// Each given field through the operator's own setter, each rechecked
    /// where it commits; a later failure keeps the earlier changes and says
    /// so, as member_update does. One notice for what changed.
    fn room_update(&self, caller: Actor<'_>, request_id: &str, room_id: Option<RoomId>, edit: RoomEdit)
        -> Result<RoomActionResult> {
        let RoomEdit { label, resume_on_restart, brief_on_join, brief_template, position } = edit;
        if label.is_none() && resume_on_restart.is_none() && brief_on_join.is_none() && brief_template.is_none()
            && position.is_none() {
            bail!("room update needs label, resume_on_restart, brief_on_join, brief_template or position");
        }
        if let Some(label) = &label {
            validate_room_label(label)?;
        }
        let room_id = self.room_target(caller, room_id)?;
        let (old_label, caller_label) = self.admit_room(caller, room_id)?;
        let check = ManageCheck::of(caller, room_id);
        let mut changes = Vec::new();
        let outcome = (|| -> Result<()> {
            if let Some(label) = label.filter(|label| *label != old_label) {
                self.rename_room_checked(RenameRoomRequest { room_id, label: label.clone() }, Some(&check))?;
                changes.push(format!("renamed to \"{label}\""));
            }
            if let Some(resume) = resume_on_restart {
                let before = self.inner.rooms.lock().get(room_id).map(|room| room.definition.resume_on_restart);
                if before != Some(resume) {
                    self.set_room_resume_on_restart_checked(SetRoomResumeRequest { room_id, resume_on_restart: resume },
                        Some(&check))?;
                    changes.push(format!("Resume on restart {}", if resume { "on" } else { "off" }));
                }
            }
            if brief_on_join.is_some() || brief_template.is_some() {
                let current = self.read_room_brief(shared_types::ReadRoomBriefRequest { room_id })?;
                // "" restores the built-in brief, as clearing the editor does.
                let template = match brief_template {
                    Some(text) if text.trim().is_empty() => None,
                    Some(text) => Some(text),
                    None => current.brief_template.clone(),
                };
                let on_join = brief_on_join.unwrap_or(current.brief_on_join);
                if on_join != current.brief_on_join || template != current.brief_template {
                    self.update_room_brief_checked(shared_types::UpdateRoomBriefRequest { room_id,
                        brief_on_join: on_join, brief_template: template.clone() }, Some(&check))?;
                    if on_join != current.brief_on_join {
                        changes.push(format!("brief at launch {}", if on_join { "on" } else { "off" }));
                    }
                    if template != current.brief_template {
                        changes.push(if template.is_some() { "brief changed".into() } else { "built-in brief restored".into() });
                    }
                }
            }
            if let Some(position) = position {
                let before = self.inner.rooms.lock().order.iter().position(|id| *id == room_id);
                if before != Some(position) {
                    self.move_room_checked(MoveRoomRequest { room_id, new_index: position }, Some(&check))?;
                    changes.push(format!("moved to position {position}"));
                }
            }
            Ok(())
        })();
        let mut warnings = Vec::new();
        if !changes.is_empty() {
            self.post_room_notice(room_id, caller, format!("{request_id}.notice"),
                format!("{caller_label} updated the room: {}", changes.join("; ")), &mut warnings);
        }
        if let Err(error) = outcome {
            if changes.is_empty() {
                return Err(error);
            }
            return Err(PartialEffect(format!("room \"{old_label}\" was partly updated ({}), then: {error:#}", changes.join("; "))).into());
        }
        if changes.is_empty() {
            warnings.push("the room already has those settings; nothing changed".into());
        }
        let brief = self.read_room_brief(shared_types::ReadRoomBriefRequest { room_id })?;
        let position = self.inner.rooms.lock().order.iter().position(|id| *id == room_id);
        let result = self.room_result(Some(request_id), "update", room_id, warnings)?;
        Ok(RoomActionResult { brief: Some(brief), position, ..result })
    }

    /// The operator's Delete room: its sessions go to the lobby and its
    /// history is erased. The caller's own room (when it is another) records
    /// the deletion; the system log always does.
    fn room_delete(&self, caller: Actor<'_>, room_id: RoomId) -> Result<RoomActionResult> {
        let (label, caller_label) = self.admit_room(caller, room_id)?;
        let before = self.room_result(None, "delete", room_id, Vec::new())?;
        self.delete_room_checked(DeleteRoomRequest { room_id }, Some(caller))?;
        let mut warnings = Vec::new();
        let notice = format!("{caller_label} deleted room \"{label}\"; its members are in the lobby");
        self.emit(RuntimeEvent::SystemLog { level: LogLevel::Info, message: notice.clone(), timestamp: now_rfc3339() });
        // Bound first: posting takes the rooms lock again.
        let own_room = caller.own_room(&self.inner.rooms.lock());
        if let Some(own) = own_room {
            self.post_room_notice(own, caller, format!("room-delete-{room_id}"), notice, &mut warnings);
        }
        Ok(RoomActionResult { warnings, ..before })
    }

    /// Every other member started (Launch, resuming its conversation) or
    /// stopped, each through the operator's own method rechecked where it
    /// commits. One member's failure does not stop the others; it is named
    /// in the warnings. The caller is never one of them: stopping itself
    /// would end the run that asked.
    fn room_run(&self, caller: Actor<'_>, request_id: &str, room_id: Option<RoomId>, start: bool)
        -> Result<RoomActionResult> {
        let room_id = self.room_target(caller, room_id)?;
        let (_, caller_label) = self.admit_room(caller, room_id)?;
        let targets: Vec<SessionId> = {
            let rooms = self.inner.rooms.lock();
            let slots = self.inner.slots.lock();
            let room = rooms.get(room_id).ok_or_else(|| anyhow!("unknown room id '{room_id}'"))?;
            room.definition.member_ids.iter().copied()
                .filter(|id| !caller.is(*id))
                .filter(|id| slots.get_by_id(*id).is_some_and(|slot| slot.running.is_some() != start))
                .collect()
        };
        let check = ManageCheck::of(caller, room_id);
        let mut done = Vec::new();
        let mut warnings = Vec::new();
        for session_id in targets {
            let label = self.inner.slots.lock().get_by_id(session_id).map(|slot| slot.definition.label.clone())
                .unwrap_or_default();
            let outcome = if start {
                self.start_session_checked(session_id, false, false, None, Some(&check)).map(|_| ())
            } else {
                // A stop returns Ok also when termination was not proved
                // (Failed, process retained): that is no stopped member.
                self.begin_stop_session_checked(session_id, Some(&check))
                    .and_then(|(ticket, _)| self.finish_stop_session(ticket))
                    .and_then(|after| if after.running || after.lifecycle_state != LifecycleState::Closed {
                        Err(anyhow!("the stop was not proved; it reads {:?}{}", after.lifecycle_state,
                            if after.running { " and still runs" } else { "" }))
                    } else { Ok(()) })
            };
            match outcome {
                Ok(()) => done.push((session_id, label)),
                Err(error) => warnings.push(format!("{label}: {error:#}")),
            }
        }
        if !done.is_empty() {
            let names = done.iter().map(|(_, label)| label.as_str()).collect::<Vec<_>>().join(", ");
            self.post_room_notice(room_id, caller, format!("{request_id}.notice"),
                format!("{caller_label} {} the room: {names}", if start { "started" } else { "stopped" }), &mut warnings);
        } else if warnings.is_empty() {
            warnings.push(format!("no member was {}; nothing changed", if start { "stopped" } else { "running" }));
        }
        let members = {
            let slots = self.inner.slots.lock();
            done.iter().filter_map(|(id, _)| slots.get_by_id(*id).map(|slot| slot.snapshot())).collect()
        };
        let result = self.room_result(Some(request_id), if start { "start" } else { "stop" }, room_id, warnings)?;
        Ok(RoomActionResult { members, ..result })
    }

    /// An outside agent bound to the room; the token goes to the caller only.
    fn room_guest_invite(&self, caller: Actor<'_>, request_id: &str, room_id: Option<RoomId>, label: Option<String>)
        -> Result<RoomActionResult> {
        let room_id = self.room_target(caller, room_id)?;
        let (_, caller_label) = self.admit_room(caller, room_id)?;
        let check = ManageCheck::of(caller, room_id);
        let invitation = self.invite_room_guest_checked(InviteRoomGuestRequest { room_id, label }, Some(&check))?;
        let mut warnings = Vec::new();
        self.post_room_notice(room_id, caller, format!("{request_id}.notice"),
            format!("{caller_label} invited outside agent {}", invitation.guest.label), &mut warnings);
        let output = self.guest_invitation_output(&invitation)?;
        let result = self.room_result(Some(request_id), "guest_invite", room_id, warnings)?;
        Ok(RoomActionResult { guests: vec![invitation.guest], invitation: Some(output), ..result })
    }

    fn room_guest_revoke(&self, caller: Actor<'_>, request_id: &str, guest_id: Uuid) -> Result<RoomActionResult> {
        let (room_id, guest_label) = self.room_guest_room(guest_id)?;
        let (_, caller_label) = self.admit_room(caller, room_id)?;
        let check = ManageCheck::of(caller, room_id);
        self.revoke_room_guest_checked(RevokeRoomGuestRequest { guest_id }, Some(&check))?;
        let mut warnings = Vec::new();
        self.post_room_notice(room_id, caller, format!("{request_id}.notice"),
            format!("{caller_label} revoked outside agent {guest_label}"), &mut warnings);
        self.room_result(Some(request_id), "guest_revoke", room_id, warnings)
    }

    fn room_guest_list(&self, caller: Actor<'_>, room_id: Option<RoomId>) -> Result<RoomActionResult> {
        let room_id = self.room_target(caller, room_id)?;
        self.admit_room(caller, room_id)?;
        let guests = self.list_room_guests(ListRoomGuestsRequest { room_id })?;
        let result = self.room_result(None, "guest_list", room_id, Vec::new())?;
        Ok(RoomActionResult { guests, ..result })
    }
}

/// room_update's fields.
struct RoomEdit {
    label: Option<String>,
    resume_on_restart: Option<bool>,
    brief_on_join: Option<bool>,
    brief_template: Option<String>,
    position: Option<usize>,
}
