//! A16 runtime actions: the operator's Settings page for agents. A member
//! granted maintain reads and changes the task folder, the personal
//! repository and the default workspace; a member granted manage scans the
//! personal repository's local conversations and attaches one to a closed
//! member for recovery (another room or the lobby also needs roam). The
//! operator entry has the operator's authority. Same setters, same rules,
//! the grant rechecked where each change commits, a request_id for every
//! change, attribution in a room feed (and the system log for settings).
//! A member granted manage also saves, inspects and loads room packages
//! (A92's `.prim1room` files and copied runtimes).
use super::*;
use member_lifecycle::{Actor, ActionReceipts, ManageCheck, ReceiptClaim, ReceiptGuard};
use shared_types::{RuntimeAction, RuntimeActionResult};

pub(super) fn is_runtime_request(request: &SidebandRequest) -> bool {
    matches!(request, SidebandRequest::Runtime { .. })
}

pub(super) type RuntimeReceipts = ActionReceipts<RuntimeActionResult>;

fn runtime_action_fingerprint(action: &RuntimeAction) -> String {
    use sha2::{Digest, Sha256};
    let canonical = serde_json::to_string(action).expect("a runtime action always serializes");
    format!("{:x}", Sha256::digest(canonical.as_bytes()))
}

impl SupervisorHandle {
    /// Runs on a blocking worker: folder I/O, scans and catalog writes.
    pub(super) fn apply_runtime_request(&self, caller: &PaneCaller, request: SidebandRequest) -> SidebandResponse {
        if let Err(error) = self.authorize_sideband_request(caller, &request) {
            return Self::rejected_sideband_response(error.to_string());
        }
        let SidebandRequest::Runtime { request_id, action } = request else {
            return Self::rejected_sideband_response("not a runtime request");
        };
        Self::runtime_response(self.runtime_action(Actor::Pane(caller), request_id, action))
    }

    pub(super) fn runtime_response(outcome: Result<RuntimeActionResult>) -> SidebandResponse {
        match outcome {
            Ok(result) => SidebandResponse { ok: true,
                message: format!("{}: {}{}", result.op, if result.replayed { "replayed the first result" } else { "done" },
                    if result.warnings.is_empty() { String::new() } else { format!("; {}", result.warnings.join("; ")) }),
                timed_out: false, payload: Some(SidebandResponsePayload::Runtime { result }), request_id: None },
            Err(error) => Self::rejected_sideband_response(format!("{error:#}")),
        }
    }

    /// A read runs as is; a change runs once under its request_id. Shared by
    /// panes and the operator entry.
    pub(super) fn runtime_action(&self, caller: Actor<'_>, request_id: Option<String>, action: RuntimeAction)
        -> Result<RuntimeActionResult> {
        {
            let slots = self.inner.slots.lock();
            if action.needs_maintain() { caller.maintain_locked(&slots)? } else { caller.manage_locked(&slots)? }
        }
        if action.is_read() {
            return match request_id {
                Some(_) => Err(anyhow!("{} is a read: send it without request_id", action.name())),
                None => self.run_runtime_action(caller, None, action),
            };
        }
        let request_id = request_id.as_deref().map(str::trim).and_then(|id| Uuid::parse_str(id).ok())
            .map(|id| id.to_string())
            .ok_or_else(|| anyhow!("{} needs request_id, a UUID: a fresh one per action, the same one only to retry it",
                action.name()))?;
        let fingerprint = runtime_action_fingerprint(&action);
        let claim = self.inner.runtime_receipts.lock().claim(caller.receipt_key(), &request_id, &fingerprint)?;
        match claim {
            ReceiptClaim::Replay(result) => Ok(RuntimeActionResult { replayed: true, ..result }),
            ReceiptClaim::Fresh => {
                let mut guard = ReceiptGuard::new(&self.inner.runtime_receipts, caller.receipt_key(), &request_id);
                let outcome = self.run_runtime_action(caller, Some(&request_id), action);
                guard.settle(&outcome);
                outcome
            }
        }
    }

    fn run_runtime_action(&self, caller: Actor<'_>, request_id: Option<&str>, action: RuntimeAction)
        -> Result<RuntimeActionResult> {
        match action {
            RuntimeAction::SettingsGet {} => Ok(RuntimeActionResult { op: "settings_get".into(),
                settings: Some(self.runtime_settings_json()), ..Default::default() }),
            RuntimeAction::SettingsSet { task_folder, personal_repo, workspace } =>
                self.runtime_settings_set(caller, request_id.expect("a change has a request_id"), task_folder,
                    personal_repo, workspace),
            RuntimeAction::ConversationsScan {} => {
                let inventory = session_inventory::discover_personal_repo(&self.personal_repo_status())
                    .map_err(anyhow::Error::msg)?;
                Ok(RuntimeActionResult { op: "conversations_scan".into(),
                    conversations: Some(serde_json::to_value(inventory)?), ..Default::default() })
            }
            RuntimeAction::ConversationAttach { member, conversation_id } =>
                self.runtime_conversation_attach(caller, request_id.expect("a change has a request_id"), &member,
                    conversation_id),
            RuntimeAction::UpgradeStatus {} => Ok(self.upgrade_result(None, "upgrade_status")),
            RuntimeAction::UpgradeCancel { attempt_id } | RuntimeAction::UpgradeAbandon { attempt_id } => {
                let cancel = matches!(action, RuntimeAction::UpgradeCancel { .. });
                let request_id = request_id.expect("a change has a request_id");
                let who = caller.label_locked(&self.inner.slots.lock());
                #[cfg(test)]
                self.run_sideband_after_initial_authorization_for_tests();
                // The caller's run and grant hold through the attempt's store
                // commit (the slots lock), released before any notice.
                let check = |slots: &SessionRegistry| caller.maintain_locked(slots);
                if cancel {
                    self.cancel_upgrade_checked(attempt_id, &who, Some(&check))?;
                } else {
                    self.abandon_held_upgrade_checked(attempt_id, &who, Some(&check))?;
                }
                // The upgrade's own notices are unchanged; the caller's room
                // records its act under its name, once per request.
                let notice = if cancel {
                    format!("{who} cancelled upgrade {attempt_id}; PRIM-1 keeps running on this build")
                } else {
                    format!("{who} abandoned held upgrade {attempt_id}; this does not show that its helper or new build stopped")
                };
                let mut result = self.upgrade_result(Some(request_id), if cancel { "upgrade_cancel" } else { "upgrade_abandon" });
                let own_room = caller.own_room(&self.inner.rooms.lock());
                if let Some(room_id) = own_room {
                    if let Err(error) = self.post_room_message_as(room_id, caller.sender(), notice.clone(),
                        Some(format!("{request_id}.notice"))) {
                        result.warnings.push(format!("notice \"{notice}\" was not stored: {error}"));
                    }
                }
                Ok(result)
            }
            RuntimeAction::Telemetry {} => Ok(RuntimeActionResult { op: "telemetry".into(),
                telemetry: Some(serde_json::to_value(self.telemetry_snapshot()?)?), ..Default::default() }),
            RuntimeAction::RoomSave { room_id, path } =>
                self.runtime_room_save(caller, request_id.expect("a change has a request_id"), room_id, &path),
            RuntimeAction::RoomInspect { path } => Ok(RuntimeActionResult { op: "room_inspect".into(),
                room_sources: Some(self.inspect_room_source(Path::new(&path))?), ..Default::default() }),
            RuntimeAction::RoomLoad { path, source_room_id } =>
                self.runtime_room_load(caller, request_id.expect("a change has a request_id"), &path, source_room_id),
        }
    }

    /// Rooms → Save room…: the operator's save (A92). Manage, and roam for a
    /// room not the caller's, checked under the rooms lock the feed is copied
    /// with. An existing file is never replaced: the operator's dialog asks
    /// before replacing, and an agent picks a new name instead. The saved
    /// room records it.
    fn runtime_room_save(&self, caller: Actor<'_>, request_id: &str, room_id: Option<RoomId>, path: &str)
        -> Result<RuntimeActionResult> {
        let room_id = match room_id {
            Some(room_id) => room_id,
            None => caller.own_room(&self.inner.rooms.lock()).ok_or_else(|| anyhow!(
                "you are in the lobby: name the room by room_id (rooms_list names every room)"))?,
        };
        let file = Path::new(path);
        if file.exists() {
            bail!("{} already exists; choose a new file name (an agent never replaces a room file)", file.display());
        }
        #[cfg(test)]
        self.run_sideband_after_initial_authorization_for_tests();
        let scope = ManageCheck::of(caller, room_id);
        let check = |rooms: &RoomState, slots: &SessionRegistry| scope.check(rooms, slots);
        let report = self.export_room_checked(room_id, file, Some(&check))?;
        let caller_label = caller.label_locked(&self.inner.slots.lock());
        let mut warnings = Vec::new();
        let notice = format!("{caller_label} saved this room to {} ({} records)", report.path, report.records);
        if let Err(error) = self.post_room_message_as(room_id, caller.sender(), notice.clone(), Some(format!("{request_id}.notice"))) {
            warnings.push(format!("notice \"{notice}\" was not stored: {error}"));
        }
        Ok(RuntimeActionResult { request_id: Some(request_id.into()), op: "room_save".into(), room_saved: Some(report),
            warnings, ..Default::default() })
    }

    /// Rooms → Load room…: the operator's load (A92), a new room with new
    /// members, nothing started and no grants; manage, checked under the
    /// locks the first member copy is saved with. The new room and the
    /// caller's own room record it.
    fn runtime_room_load(&self, caller: Actor<'_>, request_id: &str, path: &str, source_room_id: Option<RoomId>)
        -> Result<RuntimeActionResult> {
        #[cfg(test)]
        self.run_sideband_after_initial_authorization_for_tests();
        let check = |_: &RoomState, slots: &SessionRegistry| caller.manage_locked(slots);
        let loaded = self.import_room_checked(Path::new(path), source_room_id, Some(&check));
        let caller_label = caller.label_locked(&self.inner.slots.lock());
        let report = match loaded {
            Ok(report) => report,
            // Listed member copies remain: the caller's room records the
            // attempt once (a retry replays this outcome and loads nothing).
            Err(error) if error.is::<member_lifecycle::PartialEffect>() => {
                let notice = format!("{caller_label} loaded {path} only in part: {error:#}");
                let own_room = caller.own_room(&self.inner.rooms.lock());
                let stored = own_room.map(|room_id| self.post_room_message_as(room_id, caller.sender(), notice.clone(),
                    Some(format!("{request_id}.partial"))));
                if !matches!(stored, Some(Ok(_))) {
                    self.emit(RuntimeEvent::SystemLog { level: LogLevel::Warn, message: notice, timestamp: now_rfc3339() });
                }
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        let (room_id, label) = (report.room.room_id, report.room.label.clone());
        let mut warnings = Vec::new();
        let mut notify = |room_id: RoomId, key: &str, notice: String| if let Err(error) =
            self.post_room_message_as(room_id, caller.sender(), notice.clone(), Some(format!("{request_id}.{key}"))) {
            warnings.push(format!("notice \"{notice}\" was not stored: {error}"));
        };
        notify(room_id, "notice", format!("{caller_label} loaded this room from {path} ({} members, none started)",
            report.members.len()));
        let own_room = caller.own_room(&self.inner.rooms.lock());
        if let Some(own) = own_room {
            notify(own, "notice-own", format!("{caller_label} loaded room \"{label}\" from {path}"));
        }
        Ok(RuntimeActionResult { request_id: Some(request_id.into()), op: "room_load".into(), room_loaded: Some(report),
            warnings, ..Default::default() })
    }

    fn upgrade_result(&self, request_id: Option<&str>, op: &str) -> RuntimeActionResult {
        RuntimeActionResult { request_id: request_id.map(Into::into), op: op.into(), upgrade: self.upgrade_status(),
            ..Default::default() }
    }

    fn runtime_settings_json(&self) -> serde_json::Value {
        serde_json::json!({
            "task_folder": self.task_folder_status(),
            "personal_repo": self.personal_repo_status(),
            "workspace": self.snapshot().workspace_preference,
        })
    }

    /// Each given folder through the operator's own setter, each with the
    /// maintain grant rechecked at its commit; a later failure keeps the
    /// earlier changes (a partial effect, replayed to a retry).
    fn runtime_settings_set(&self, caller: Actor<'_>, request_id: &str, task_folder: Option<String>,
        personal_repo: Option<String>, workspace: Option<String>) -> Result<RuntimeActionResult> {
        if task_folder.is_none() && personal_repo.is_none() && workspace.is_none() {
            bail!("settings set needs task_folder, personal_repo or workspace");
        }
        let check = |slots: &SessionRegistry| caller.maintain_locked(slots);
        let mut changes = Vec::new();
        let outcome = (|| -> Result<()> {
            // Each setter's own result, compared with the effective value
            // before it: a choice already in effect changes nothing and is
            // not reported.
            if let Some(folder) = &personal_repo {
                let before = self.personal_repo_status();
                let after = self.select_personal_repo_checked(folder, Some(&check)).map_err(anyhow::Error::msg)?;
                if (after.folder.as_deref(), after.source) != (before.folder.as_deref(), before.source) {
                    changes.push(format!("personal repository {}", after.folder.as_deref().unwrap_or(folder)));
                }
            }
            if let Some(folder) = &task_folder {
                let before = self.task_folder_status();
                let after = self.select_task_folder_checked(folder, Some(&check)).map_err(anyhow::Error::msg)?;
                if (after.folder.as_deref(), after.source) != (before.folder.as_deref(), before.source) {
                    changes.push(format!("task folder {}", after.folder.as_deref().unwrap_or(folder)));
                }
            }
            if let Some(folder) = &workspace {
                let before = self.snapshot().workspace_preference;
                let qualified = self.set_workspace_preference_checked(Path::new(folder), Some(&check))?;
                if qualified != before {
                    changes.push(format!("default workspace {qualified}"));
                }
            }
            Ok(())
        })();
        let label = caller.label_locked(&self.inner.slots.lock());
        let mut warnings = Vec::new();
        if !changes.is_empty() {
            let notice = format!("{label} changed the runtime settings: {}", changes.join("; "));
            self.emit(RuntimeEvent::SystemLog { level: LogLevel::Info, message: notice.clone(), timestamp: now_rfc3339() });
            let own_room = caller.own_room(&self.inner.rooms.lock());
            if let Some(room_id) = own_room {
                if let Err(error) = self.post_room_message_as(room_id, caller.sender(), notice.clone(),
                    Some(format!("{request_id}.notice"))) {
                    warnings.push(format!("notice \"{notice}\" was not stored: {error}"));
                }
            }
        }
        if let Err(error) = outcome {
            if changes.is_empty() {
                return Err(error);
            }
            return Err(member_lifecycle::PartialEffect(format!("settings were partly changed ({}), then: {error:#}",
                changes.join("; "))).into());
        }
        if changes.is_empty() {
            warnings.push("those settings were already in effect; nothing changed".into());
        }
        Ok(RuntimeActionResult { request_id: Some(request_id.into()), op: "settings_set".into(),
            settings: Some(self.runtime_settings_json()), warnings, ..Default::default() })
    }

    /// Attach for recovery: the operator's binding, with the caller's scope
    /// rechecked where it commits. Nothing starts; `member_start` with mode
    /// resume_here releases the hold once the conversation is closed
    /// everywhere else.
    fn runtime_conversation_attach(&self, caller: Actor<'_>, request_id: &str, member: &str, conversation_id: String)
        -> Result<RuntimeActionResult> {
        let target = self.resolve_any_member(caller, member)?;
        let (session_id, room_id, label, caller_label) = (target.session_id, target.room_id, target.label, target.caller_label);
        let driver = self.inner.slots.lock().get_by_id(session_id).map(|slot| slot.definition.driver)
            .ok_or_else(|| anyhow!("member {session_id} no longer exists"))?;
        let check = ManageCheck::for_room(caller, room_id);
        self.attach_local_session_checked(session_inventory::AttachLocalSessionRequest {
            session_id, driver, conversation_id: conversation_id.clone() }, Some(&check))?;
        let mut warnings = Vec::new();
        let notice = format!("{caller_label} attached conversation {conversation_id} to {label} (held until Resume here)");
        let notice_room = room_id.or_else(|| caller.own_room(&self.inner.rooms.lock()));
        match notice_room {
            Some(room_id) => if let Err(error) = self.post_room_message_as(room_id, caller.sender(), notice.clone(),
                Some(format!("{request_id}.notice"))) {
                warnings.push(format!("notice \"{notice}\" was not stored: {error}"));
            },
            None => self.emit(RuntimeEvent::SystemLog { level: LogLevel::Info, message: notice, timestamp: now_rfc3339() }),
        }
        let member = self.inner.slots.lock().get_by_id(session_id).map(|slot| slot.snapshot());
        Ok(RuntimeActionResult { request_id: Some(request_id.into()), op: "conversation_attach".into(), member, warnings,
            ..Default::default() })
    }
}
