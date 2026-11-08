//! Operator-selected metadata binding. A discovered root is identity evidence,
//! never evidence that a conversation is free in another process or install.
use super::*;
use anyhow::{Result, anyhow, bail, ensure};
use serde::Deserialize;
use shared_types::{DriverKind, SessionId, RuntimeEvent, LogLevel, now_rfc3339};
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachLocalSessionRequest {
    pub session_id: SessionId,
    pub driver: DriverKind,
    pub conversation_id: String,
}

impl AttachLocalSessionRequest {
    fn provider(&self) -> Result<&'static str> {
        let id = Uuid::parse_str(&self.conversation_id)
            .map_err(|_| anyhow!("The conversation ID must be a canonical non-nil UUID"))?;
        ensure!(!id.is_nil() && id.to_string() == self.conversation_id,
            "The conversation ID must be a canonical non-nil UUID");
        match self.driver {
            DriverKind::Claude => Ok("claude"),
            DriverKind::Codex => Ok("codex"),
            DriverKind::Grok => Ok("grok"),
            _ => bail!("Only Claude, Codex and Grok conversations can be attached"),
        }
    }
}

impl crate::SupervisorHandle {
    /// Operator Attach for recovery. This saves a binding, not ownership or
    /// permission to resume: the existing Resume here confirmation is required.
    /// Scanning and metadata reads run outside the session/catalog locks.
    pub fn attach_local_session(&self, request: AttachLocalSessionRequest) -> Result<()> {
        self.attach_local_session_scanning(request, discover)
    }

    /// `check`: a managing member's conversation_attach (A16), rechecked
    /// where the binding commits.
    pub(crate) fn attach_local_session_checked(
        &self, request: AttachLocalSessionRequest, check: Option<&crate::member_lifecycle::ManageCheck<'_>>,
    ) -> Result<()> {
        self.attach_local_session_scanning_checked(request, discover, check)
    }

    fn attach_local_session_scanning(
        &self, request: AttachLocalSessionRequest, scan: impl FnOnce(&str) -> Inventory,
    ) -> Result<()> {
        self.attach_local_session_scanning_checked(request, scan, None)
    }

    fn attach_local_session_scanning_checked(
        &self, request: AttachLocalSessionRequest, scan: impl FnOnce(&str) -> Inventory,
        check: Option<&crate::member_lifecycle::ManageCheck<'_>>,
    ) -> Result<()> {
        self.ensure_active()?;
        request.provider()?;
        let workspace = discovery_scope(&self.personal_repo_status()).map_err(anyhow::Error::msg)?;
        let (definition, qualified, generation, previous_id, previous_hold) = {
            let slots = self.inner.slots.lock();
            let slot = slots.get_by_id(request.session_id).ok_or_else(|| anyhow!("The selected member no longer exists"))?;
            check_target(slot, &request, &workspace)?;
            (slot.definition.clone(), slot.qualified_working_directory.clone()
                .ok_or_else(|| anyhow!("The selected member has no qualified folder"))?,
                slot.generation, slot.harness_session_id.clone(), slot.recovery.clone())
        };
        let _lease = crate::revalidate_qualified_directory("member folder", &qualified, &self.inner.runtime_dir)?;
        let inventory = scan(&workspace);
        let codex_root = (request.driver == DriverKind::Codex)
            .then(|| self.codex_identity_sessions_root()).transpose()?;
        verify_candidate(&inventory, &request, &workspace, codex_root.as_deref())?;

        let _shutdown = self.inner.shutdown_lifecycle.read();
        self.ensure_active()?;
        let rooms = check.map(|_| self.inner.rooms.lock());
        let mut slots = self.inner.slots.lock();
        if let (Some(check), Some(rooms)) = (check, rooms.as_deref()) {
            check.check_target(rooms, &slots, request.session_id)?;
        }
        let mut catalog = self.inner.catalog.lock();
        ensure!(self.personal_repo_folder().is_some_and(|folder| normalize(&folder.display().to_string()) == normalize(&workspace)),
            "The personal repository changed during discovery; scan again");
        let slot = slots.get_by_id(request.session_id).ok_or_else(|| anyhow!("The selected member no longer exists"))?;
        check_target(slot, &request, &workspace)?;
        ensure!(slot.definition == definition && slot.generation == generation
            && slot.harness_session_id == previous_id && slot.recovery == previous_hold,
            "The selected member changed while its conversation was checked; try again");
        // Start can still be preparing outside these locks while the member
        // looks closed. Fence that prepared binding only after this commit.
        let next_generation = slot.generation.checked_add(1)
            .ok_or_else(|| anyhow!("session generation exhausted for '{}'", request.session_id))?;
        let same = |id: &str| Uuid::parse_str(id).is_ok_and(|id| id.to_string() == request.conversation_id);
        if let Some(owner) = slots.ordered_slots().into_iter().find(|other|
            other.session_id != request.session_id && other.definition.driver == request.driver
            && (other.harness_session_id.as_deref().is_some_and(same)
                || other.recovery.as_ref().and_then(|held| held.original_id.as_deref()).is_some_and(same)
                || other.recovery.as_ref().and_then(|held| held.reported_codex.as_ref())
                    .is_some_and(|evidence| same(&evidence.thread_id)))) {
            bail!("\"{}\" in this PRIM-1 already uses or holds this conversation", owner.definition.label);
        }
        let hold = crate::RecoveryReference {
            original_id: Some(request.conversation_id.clone()),
            provenance: "room_import_in_use".into(),
            reason: "Attached for recovery from local metadata. This conversation may still be running outside this PRIM-1. Stop it there, then explicitly choose Resume here; attachment alone never starts or resumes it.".into(),
            reported_codex: None,
        };
        let mut history = slot.recovery_history.clone();
        if let Some(previous) = previous_hold.filter(|previous| previous != &hold)
            && !history.contains(&previous) {
            history.push(previous);
        }
        if let Some(previous) = previous_id.filter(|previous| !same(previous))
            && !history.iter().any(|reference| reference.original_id.as_deref() == Some(previous.as_str())) {
            history.push(crate::RecoveryReference {
                original_id: Some(previous), provenance: "local_attach_replaced".into(),
                reason: "This member's previous conversation was replaced by an operator attachment; its reference is retained here.".into(),
                reported_codex: None,
            });
        }
        // An attached conversation is never eligible for a silent new start (D2).
        self.inner.identity_tracker.revoke_minted_claude(request.session_id);
        let mut candidate = catalog.clone();
        let row = candidate.sessions.iter_mut().find(|row| row.session_id == request.session_id)
            .ok_or_else(|| anyhow!("The selected member is missing from the catalog"))?;
        row.harness_session_id = Some(request.conversation_id.clone());
        row.recovery = Some(hold.clone());
        row.recovery_history = history.clone();
        self.persist_catalog_candidate(&candidate)?;
        let slot = slots.get_by_id_mut(request.session_id).expect("validated member is locked");
        slot.generation = next_generation;
        slot.harness_session_id = Some(request.conversation_id.clone());
        slot.recovery = Some(hold);
        slot.recovery_history = history;
        *catalog = candidate;
        // Queue after the durable catalog update, while later attachments are
        // still excluded. This call performs no file or Git work on this thread.
        self.note_conversation_change(crate::session_ledger::ConversationChange {
            session_id: request.session_id, harness: request.driver,
            conversation_id: request.conversation_id, reason: "import", via: "import",
            run_id: None, pid: None,
        });
        drop(catalog);
        drop(slots);
        self.emit(RuntimeEvent::SystemLog { level: LogLevel::Info,
            message: format!("{}: local conversation attached for recovery; Resume here remains required", definition.label),
            timestamp: now_rfc3339() });
        Ok(())
    }
}

fn check_target(slot: &crate::SessionSlot, request: &AttachLocalSessionRequest, workspace: &str) -> Result<()> {
    crate::ensure_closed_for_definition_edit(slot)?;
    ensure!(!slot.output_draining, "The selected member is still draining output; stop it before attachment");
    ensure!(slot.definition.driver == request.driver, "The selected member uses a different harness");
    ensure!(normalize(&slot.definition.working_dir) == normalize(workspace),
        "The selected member must use the conversation's personal-repository folder");
    Ok(())
}

/// Require a fresh metadata match and a real root metadata anchor; a cached
/// display row, title index or PID record alone cannot change a member.
fn verify_candidate(inventory: &Inventory, request: &AttachLocalSessionRequest, workspace: &str, codex_root: Option<&Path>) -> Result<()> {
    let provider = request.provider()?;
    ensure!(normalize(&inventory.workspace) == normalize(workspace), "The discovery scope changed; scan again");
    let mut matching = inventory.candidates.iter().filter(|candidate|
        candidate.provider == provider && candidate.id == request.conversation_id);
    let candidate = matching.next().ok_or_else(|| anyhow!("That exact conversation is no longer present in this personal repository; scan again"))?;
    ensure!(matching.next().is_none() && !candidate.conflict, "Conflicting metadata for this conversation prevents attachment");
    ensure!(candidate.kind == ConversationKind::Root, "Only a verified root conversation can be attached; subagents and unknown roots cannot");
    ensure!(normalize(&candidate.workspace) == normalize(workspace), "The conversation belongs to another folder");
    if let Some(reason) = &candidate.attach_unavailable { bail!("{reason}"); }
    if request.driver == DriverKind::Codex {
        let root = codex_root.ok_or_else(|| anyhow!("The Codex sessions directory is unavailable"))?;
        crate::session::verify_codex_rollout(root, &request.conversation_id, workspace)?;
        return Ok(());
    }
    for source in &candidate.metadata_paths {
        let path = Path::new(source);
        if !plain(path).is_some_and(|meta| meta.is_file()) { continue; }
        let id = request.conversation_id.as_str();
        let proven = match request.driver {
            DriverKind::Claude if path.extension().is_some_and(|ext| ext == "jsonl")
                && path.file_stem().and_then(|stem| stem.to_str()) == Some(id) => {
                let value = jsonl_metadata(path, true).map_err(anyhow::Error::msg)?;
                value["sessionId"] == id && value["isSidechain"] == false
                    && value["cwd"].as_str().or_else(|| value["projectPath"].as_str())
                        .is_some_and(|cwd| normalize(cwd) == normalize(workspace))
            }
            DriverKind::Grok if path.file_name().is_some_and(|name| name == "summary.json")
                && path.parent().and_then(Path::file_name).and_then(|name| name.to_str()) == Some(id) => {
                let value = metadata_json(path, false).map_err(anyhow::Error::msg)?;
                value["info"]["id"] == id && value["info"]["cwd"].as_str()
                    .is_some_and(|cwd| normalize(cwd) == normalize(workspace))
            }
            _ => false,
        };
        if proven { return Ok(()); }
    }
    bail!("The exact root conversation metadata is unavailable or changed; an index or PID record alone cannot attach it")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(driver: DriverKind) -> (tempfile::TempDir, Inventory, AttachLocalSessionRequest) {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        fs::create_dir_all(&work).unwrap();
        // The personal-repo scope is canonical; a macOS temp path goes through
        // the /var link, so the recorded cwd must be canonical too.
        #[cfg(target_os = "macos")]
        let work = fs::canonicalize(&work).unwrap();
        let roots = [dir.path().join("codex"), dir.path().join("claude"), dir.path().join("grok")];
        for root in &roots { fs::create_dir_all(root).unwrap(); }
        let id = Uuid::new_v4().to_string();
        let cwd = work.display().to_string();
        let (path, value) = match driver {
            DriverKind::Codex => (roots[0].join(format!("sessions/day/rollout-test-{id}.jsonl")),
                serde_json::json!({"type":"session_meta","payload":{"id":id,"cwd":cwd,"source":"cli","thread_source":"user"}})),
            DriverKind::Claude => (roots[1].join(format!("projects/work/{id}.jsonl")),
                serde_json::json!({"sessionId":id,"cwd":cwd,"isSidechain":false})),
            DriverKind::Grok => {
                let encoded: String = cwd.bytes().map(|byte| format!("%{byte:02X}")).collect();
                (roots[2].join(format!("sessions/{encoded}/{id}/summary.json")),
                    serde_json::json!({"info":{"id":id,"cwd":cwd}}))
            }
            _ => unreachable!(),
        };
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("{value}\n")).unwrap();
        if driver != DriverKind::Grok {
            use std::io::Write;
            fs::OpenOptions::new().append(true).open(&path).unwrap()
                .write_all(b"private transcript beyond metadata; deliberately invalid JSON\n").unwrap();
        }
        let inventory = discover_at(&cwd, &roots[0], &roots[1], &roots[2]);
        assert_eq!(inventory.candidates.len(), 1, "{inventory:?}");
        let request = AttachLocalSessionRequest { session_id: Uuid::new_v4(), driver, conversation_id: id };
        (dir, inventory, request)
    }

    #[test]
    fn exact_external_root_metadata_rechecks_each_provider_without_reading_messages() {
        for driver in [DriverKind::Claude, DriverKind::Codex, DriverKind::Grok] {
            let (dir, inventory, request) = fixture(driver);
            let codex_root = dir.path().join("codex/sessions");
            verify_candidate(&inventory, &request, &inventory.workspace, Some(&codex_root)).unwrap();
            let path = Path::new(&inventory.candidates[0].metadata_paths[0]);
            fs::write(path, "{\"wrong\":\"changed after discovery\"}\n").unwrap();
            assert!(verify_candidate(&inventory, &request, &inventory.workspace, Some(&codex_root)).is_err());
        }
    }

    #[test]
    fn conflicts_subagents_wrong_driver_and_index_only_do_not_attach() {
        let (dir, mut inventory, request) = fixture(DriverKind::Claude);
        inventory.candidates[0].conflict = true;
        assert!(verify_candidate(&inventory, &request, &inventory.workspace, None).unwrap_err().to_string().contains("Conflicting"));
        inventory.candidates[0].conflict = false;
        for kind in [ConversationKind::Subagent, ConversationKind::Unknown] {
            inventory.candidates[0].kind = kind;
            assert!(verify_candidate(&inventory, &request, &inventory.workspace, None).is_err());
        }
        inventory.candidates[0].kind = ConversationKind::Root;
        let mut wrong = request.clone();
        wrong.driver = DriverKind::Codex;
        assert!(verify_candidate(&inventory, &wrong, &inventory.workspace, None).is_err());
        let index = dir.path().join("sessions-index.json");
        fs::write(&index, "{}").unwrap();
        inventory.candidates[0].metadata_paths = vec![index.display().to_string()];
        assert!(verify_candidate(&inventory, &request, &inventory.workspace, None).unwrap_err().to_string().contains("index or PID"));
        wrong.driver = DriverKind::Claude;
        wrong.conversation_id = Uuid::nil().to_string();
        assert!(wrong.provider().is_err());
        wrong.conversation_id = "AAAAAAAA-0000-4000-8000-000000000001".into();
        assert!(wrong.provider().is_err());
    }

    #[test]
    fn codex_archive_and_suffix_are_visible_but_refused_and_canonical_ambiguity_uses_resume_verifier() {
        let (dir, inventory, request) = fixture(DriverKind::Codex);
        let codex = dir.path().join("codex");
        let canonical = PathBuf::from(&inventory.candidates[0].metadata_paths[0]);
        let original = metadata_json(&canonical, true).unwrap();
        let archived_id = Uuid::new_v4().to_string();
        let suffixed_id = Uuid::new_v4().to_string();
        for (id, path) in [
            (&archived_id, codex.join(format!("archived_sessions/rollout-test-{archived_id}.jsonl"))),
            (&suffixed_id, codex.join(format!("sessions/day/rollout-test-{suffixed_id}_{}.jsonl", Uuid::new_v4()))),
        ] {
            let mut header = original.clone();
            header["payload"]["id"] = Value::String(id.clone());
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, format!("{header}\n")).unwrap();
        }
        let scan = || discover_at(&inventory.workspace, &codex, &dir.path().join("claude"), &dir.path().join("grok"));
        let found = scan();
        assert_eq!(found.candidates.len(), 3);
        assert!(found.candidates.iter().find(|row| row.id == request.conversation_id).unwrap().attach_unavailable.is_none());
        for (id, expected) in [
            (archived_id, "Archived in Codex; unarchive it in Codex first"),
            (suffixed_id, "This Codex conversation file is not resumable by PRIM-1 yet"),
        ] {
            assert_eq!(found.candidates.iter().find(|row| row.id == id).unwrap().attach_unavailable.as_deref(), Some(expected));
            let unsupported = AttachLocalSessionRequest { conversation_id: id, ..request.clone() };
            assert_eq!(verify_candidate(&found, &unsupported, &inventory.workspace, Some(&codex.join("sessions"))).unwrap_err().to_string(), expected);
        }
        verify_candidate(&found, &request, &inventory.workspace, Some(&codex.join("sessions"))).unwrap();
        let duplicate = codex.join(format!("sessions/another/rollout-other-{}.jsonl", request.conversation_id));
        fs::create_dir_all(duplicate.parent().unwrap()).unwrap();
        fs::copy(&canonical, duplicate).unwrap();
        assert!(verify_candidate(&scan(), &request, &inventory.workspace, Some(&codex.join("sessions")))
            .unwrap_err().to_string().contains("multiple Codex rollouts"));
    }

    struct NoHarness;
    impl crate::PtySpawner for NoHarness {
        fn spawn(&self, _plan: &crate::PreparedLaunch, _handler: crate::PtyEventHandler)
            -> Result<Box<dyn crate::PtySession>> {
            panic!("Attach and its recovery hold must never launch a harness")
        }
    }

    fn supervisor_at(root: &Path, workspace: &str) -> crate::SupervisorHandle {
        crate::session_ledger::set_personal_repo_env_for_tests(Some(std::ffi::OsStr::new(workspace)));
        let result = crate::SupervisorHandle::new(crate::SupervisorConfig {
            working_root: PathBuf::from(workspace), runtime_dir: root.join("runtime"),
            pane_mcp_executable: None, heartbeat_interval: Some(Duration::from_secs(3600)),
            auto_restart_on_stall_sessions: Some(Vec::new()), auto_restart_stall_threshold: None,
            room_brief_on_join: false, task_folder: None,
        });
        crate::session_ledger::set_personal_repo_env_for_tests(Some(std::ffi::OsStr::new("off")));
        let supervisor = result.unwrap();
        supervisor.set_pty_spawner_for_tests(std::sync::Arc::new(NoHarness));
        *supervisor.inner.codex_identity_root_for_tests.lock() = Some(root.join("codex/sessions"));
        supervisor
    }

    fn member(supervisor: &crate::SupervisorHandle, driver: DriverKind) -> SessionId {
        supervisor.create_session(shared_types::CreateSessionRequest { codex_transport: Default::default(), permissions: None,
            label: Some("Attach fixture".into()), driver,
            permission_profile: shared_types::PermissionProfile::Normal,
            linux_working_directory: None, room_id: None, launch_choices: None,
        }).unwrap().session_id
    }

    #[test]
    fn attach_is_durable_held_through_launch_and_reload_and_records_import() {
        for driver in [DriverKind::Claude, DriverKind::Codex, DriverKind::Grok] {
            let (dir, inventory, mut request) = fixture(driver);
            let workspace = inventory.workspace.clone();
            let supervisor = supervisor_at(dir.path(), &workspace);
            request.session_id = member(&supervisor, driver);
            let id = request.session_id;
            let conversation = request.conversation_id.clone();
            supervisor.attach_local_session_scanning(request, |_| inventory).unwrap();
            let snapshot = supervisor.snapshot().sessions.into_iter().find(|row| row.session_id == id).unwrap();
            assert_eq!(supervisor.inner.slots.lock().get_by_id(id).unwrap().harness_session_id.as_deref(), Some(conversation.as_str()));
            assert_eq!(snapshot.recovery.as_ref().unwrap().provenance, "room_import_in_use");
            assert!(supervisor.start_session_by_id(id).unwrap_err().is::<crate::session::RecoveryHold>());
            assert!(supervisor.flush_session_ledger(false, false, Duration::from_secs(5)).is_some_and(|report| report.unwritten_members == 0));
            let record = crate::session_ledger::read_register_file(&crate::session_ledger::register_path(Path::new(&workspace), id))
                .unwrap().unwrap();
            assert_eq!(record.conversation.as_ref().unwrap().id, conversation);
            assert_eq!(record.conversation.as_ref().unwrap().via, "import");
            supervisor.shutdown().unwrap();
            drop(supervisor);
            let reloaded = supervisor_at(dir.path(), &workspace);
            let snapshot = reloaded.snapshot().sessions.into_iter().find(|row| row.session_id == id).unwrap();
            assert_eq!(snapshot.recovery.as_ref().unwrap().provenance, "room_import_in_use",
                "a held binding must not become resumable or register_disagrees on restart");
            assert!(reloaded.start_session_by_id(id).unwrap_err().is::<crate::session::RecoveryHold>());
            reloaded.shutdown().unwrap();
        }
    }

    #[test]
    fn stale_register_preserves_attach_hold_and_retains_disagreement_as_history() {
        let (dir, inventory, mut request) = fixture(DriverKind::Claude);
        let workspace = inventory.workspace.clone();
        let supervisor = supervisor_at(dir.path(), &workspace);
        request.session_id = member(&supervisor, request.driver);
        let id = request.session_id;
        let selected = request.conversation_id.clone();
        supervisor.attach_local_session_scanning(request, |_| inventory).unwrap();
        assert!(supervisor.flush_session_ledger(false, false, Duration::from_secs(5)).is_some_and(|report| report.unwritten_members == 0));
        supervisor.shutdown().unwrap();
        drop(supervisor);
        // Model a crash after the catalog commit but before the asynchronous
        // register update: a valid same-install register still names A, while
        // the durable catalog explicitly selected B under the import hold.
        let path = crate::session_ledger::register_path(Path::new(&workspace), id);
        let mut stale = crate::session_ledger::read_register_file(&path).unwrap().unwrap();
        let old = Uuid::new_v4().to_string();
        stale.conversation.as_mut().unwrap().id = old.clone();
        for history in &mut stale.history {
            if history.id == selected { history.id = old.clone(); }
        }
        fs::write(&path, serde_json::to_vec_pretty(&stale).unwrap()).unwrap();
        let reloaded = supervisor_at(dir.path(), &workspace);
        let snapshot = reloaded.snapshot().sessions.into_iter().find(|row| row.session_id == id).unwrap();
        reloaded.shutdown().unwrap();
        assert_eq!(snapshot.recovery.as_ref().unwrap().provenance, "room_import_in_use",
            "an asynchronous register failure must not remove the operator's Resume here path");
        assert_eq!(snapshot.recovery.as_ref().unwrap().original_id.as_deref(), Some(selected.as_str()));
        assert!(snapshot.recovery_history.iter().any(|held| held.provenance == "register_disagrees"
            && held.reason.contains(&old) && held.reason.contains(&selected)));
    }

    #[test]
    fn catalog_failure_preserves_previous_binding_and_success_retains_history() {
        let (dir, inventory, mut request) = fixture(DriverKind::Claude);
        let supervisor = supervisor_at(dir.path(), &inventory.workspace);
        request.session_id = member(&supervisor, request.driver);
        let previous_id = Uuid::new_v4().to_string();
        let previous_hold = crate::RecoveryReference {
            original_id: Some(Uuid::new_v4().to_string()), provenance: "fixture_previous_hold".into(),
            reason: "Previous recovery evidence".into(), reported_codex: None,
        };
        let generation = {
            let mut slots = supervisor.inner.slots.lock();
            let slot = slots.get_by_id_mut(request.session_id).unwrap();
            slot.harness_session_id = Some(previous_id.clone());
            slot.recovery = Some(previous_hold.clone());
            slot.generation
        };
        let before = fs::read(dir.path().join("runtime/session-catalog-v1.json")).unwrap();
        supervisor.fail_next_catalog_write_for_tests();
        assert!(supervisor.attach_local_session_scanning(request.clone(), |_| Inventory {
            workspace: inventory.workspace.clone(), candidates: inventory.candidates.clone(),
            providers: Vec::new(), truncated: false,
        }).unwrap_err().to_string().contains("persistence failure"));
        assert_eq!(fs::read(dir.path().join("runtime/session-catalog-v1.json")).unwrap(), before);
        {
            let slots = supervisor.inner.slots.lock();
            let slot = slots.get_by_id(request.session_id).unwrap();
            assert_eq!(slot.generation, generation);
            assert_eq!(slot.harness_session_id.as_deref(), Some(previous_id.as_str()));
            assert_eq!(slot.recovery.as_ref(), Some(&previous_hold));
        }
        supervisor.attach_local_session_scanning(request.clone(), |_| inventory).unwrap();
        {
            let slots = supervisor.inner.slots.lock();
            let slot = slots.get_by_id(request.session_id).unwrap();
            assert_eq!(slot.generation, generation + 1);
            assert!(slot.recovery_history.contains(&previous_hold));
            assert!(slot.recovery_history.iter().any(|held| held.original_id.as_deref() == Some(previous_id.as_str())));
        }
        supervisor.shutdown().unwrap();
    }

    #[test]
    fn attach_fences_a_start_preparing_the_previous_conversation() {
        use std::sync::{Arc, mpsc};

        struct BlockingResolver {
            entered: mpsc::SyncSender<()>,
            release: parking_lot::Mutex<mpsc::Receiver<()>>,
        }
        impl crate::DriverExecutableResolver for BlockingResolver {
            fn resolve(&self, _: DriverKind) -> Result<crate::ResolvedLaunchProgram> {
                self.entered.send(()).unwrap();
                self.release.lock().recv_timeout(Duration::from_secs(10)).unwrap();
                Ok(crate::ResolvedLaunchProgram {
                    program: fs::canonicalize(std::env::current_exe()?)?.to_string_lossy().into_owned(),
                    prefix_args: Vec::new(),
                })
            }
        }

        let (dir, inventory, mut request) = fixture(DriverKind::Claude);
        let supervisor = supervisor_at(dir.path(), &inventory.workspace);
        request.session_id = member(&supervisor, request.driver);
        let id = request.session_id;
        let previous = Uuid::new_v4().to_string();
        let selected = request.conversation_id.clone();
        let generation = {
            let mut slots = supervisor.inner.slots.lock();
            let slot = slots.get_by_id_mut(id).unwrap();
            slot.harness_session_id = Some(previous.clone());
            slot.generation
        };
        supervisor.persist_slot_harness_state(id);
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        supervisor.set_executable_resolver_for_tests(Arc::new(BlockingResolver {
            entered: entered_tx, release: parking_lot::Mutex::new(release_rx),
        }));
        let starting = supervisor.clone();
        let worker = std::thread::spawn(move || starting.start_session_by_id(id));
        entered_rx.recv_timeout(Duration::from_secs(5)).expect("Start did not reach executable resolution");
        {
            let slots = supervisor.inner.slots.lock();
            let slot = slots.get_by_id(id).unwrap();
            assert_eq!(slot.state, shared_types::LifecycleState::Closed);
            assert!(slot.spawn_in_flight.is_none() && slot.running.is_none());
        }
        let attached = supervisor.attach_local_session_scanning(request, |_| inventory);
        release_tx.send(()).unwrap();
        let started = worker.join().expect("NoHarness must never be reached by the stale Start");
        attached.unwrap();
        assert!(started.unwrap_err().to_string().contains("definition changed while launch was being prepared"));
        {
            let slots = supervisor.inner.slots.lock();
            let slot = slots.get_by_id(id).unwrap();
            assert_eq!(slot.generation, generation + 1);
            assert_eq!(slot.state, shared_types::LifecycleState::Closed);
            assert!(slot.running.is_none() && slot.spawn_in_flight.is_none());
            assert_eq!(slot.harness_session_id.as_deref(), Some(selected.as_str()));
            assert_eq!(slot.recovery.as_ref().unwrap().provenance, "room_import_in_use");
            assert_eq!(slot.recovery.as_ref().unwrap().original_id.as_deref(), Some(selected.as_str()));
            assert!(slot.recovery_history.iter().any(|held| held.original_id.as_deref() == Some(previous.as_str())
                && held.provenance == "local_attach_replaced"));
            let catalog = supervisor.inner.catalog.lock();
            let row = catalog.sessions.iter().find(|row| row.session_id == id).unwrap();
            assert_eq!(row.harness_session_id, slot.harness_session_id);
            assert_eq!(row.recovery, slot.recovery);
            assert_eq!(row.recovery_history, slot.recovery_history);
        }
        assert!(supervisor.start_session_by_id(id).unwrap_err().is::<crate::session::RecoveryHold>());
        supervisor.shutdown().unwrap();
    }

    #[test]
    fn exhausted_generation_refuses_attach_without_changing_catalog_or_binding() {
        let (dir, inventory, mut request) = fixture(DriverKind::Claude);
        let supervisor = supervisor_at(dir.path(), &inventory.workspace);
        request.session_id = member(&supervisor, request.driver);
        let id = request.session_id;
        let previous = Uuid::new_v4().to_string();
        {
            let mut slots = supervisor.inner.slots.lock();
            let slot = slots.get_by_id_mut(id).unwrap();
            slot.generation = shared_types::SessionGeneration::MAX;
            slot.harness_session_id = Some(previous.clone());
        }
        supervisor.persist_slot_harness_state(id);
        let path = dir.path().join("runtime/session-catalog-v1.json");
        let before = fs::read(&path).unwrap();
        assert!(supervisor.attach_local_session_scanning(request, |_| inventory)
            .unwrap_err().to_string().contains("generation exhausted"));
        assert_eq!(fs::read(&path).unwrap(), before);
        {
            let slots = supervisor.inner.slots.lock();
            let slot = slots.get_by_id(id).unwrap();
            assert_eq!(slot.generation, shared_types::SessionGeneration::MAX);
            assert_eq!(slot.harness_session_id.as_deref(), Some(previous.as_str()));
            assert!(slot.recovery.is_none() && slot.recovery_history.is_empty());
        }
        supervisor.shutdown().unwrap();
    }

    #[test]
    fn attach_refuses_busy_wrong_driver_duplicate_and_changed_target() {
        let (dir, inventory, mut request) = fixture(DriverKind::Claude);
        let supervisor = supervisor_at(dir.path(), &inventory.workspace);
        request.session_id = member(&supervisor, request.driver);
        let scan = || Inventory { workspace: inventory.workspace.clone(), candidates: inventory.candidates.clone(),
            providers: Vec::new(), truncated: false };
        let mut wrong = request.clone();
        wrong.driver = DriverKind::Codex;
        assert!(supervisor.attach_local_session_scanning(wrong, |_| panic!("wrong driver must refuse before reading providers")).is_err());
        for state in [shared_types::LifecycleState::Starting, shared_types::LifecycleState::Busy] {
            supervisor.inner.slots.lock().get_by_id_mut(request.session_id).unwrap().state = state;
            assert!(supervisor.attach_local_session_scanning(request.clone(), |_| panic!("running member must refuse before scan")).is_err());
        }
        supervisor.inner.slots.lock().get_by_id_mut(request.session_id).unwrap().state = shared_types::LifecycleState::Closed;
        supervisor.inner.slots.lock().get_by_id_mut(request.session_id).unwrap().output_draining = true;
        assert!(supervisor.attach_local_session_scanning(request.clone(), |_| panic!("draining member must refuse before scan")).is_err());
        supervisor.inner.slots.lock().get_by_id_mut(request.session_id).unwrap().output_draining = false;
        assert!(supervisor.attach_local_session_scanning(request.clone(), |_| {
            supervisor.inner.slots.lock().get_by_id_mut(request.session_id).unwrap().generation += 1;
            scan()
        }).unwrap_err().to_string().contains("changed"));
        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        assert!(supervisor.attach_local_session_scanning(request.clone(), |_| {
            supervisor.set_session_working_directory(request.session_id, &elsewhere).unwrap();
            scan()
        }).unwrap_err().to_string().contains("folder"));
        supervisor.set_session_working_directory(request.session_id, Path::new(&inventory.workspace)).unwrap();
        let other = member(&supervisor, request.driver);
        supervisor.attach_local_session_scanning(AttachLocalSessionRequest {session_id: other, ..request.clone()}, |_| scan()).unwrap();
        assert!(supervisor.attach_local_session_scanning(request, |_| scan()).unwrap_err().to_string().contains("already uses or holds"));
        supervisor.shutdown().unwrap();
    }

    #[test]
    fn held_original_and_reported_only_references_also_prevent_duplicate_attachment() {
        let (dir, inventory, mut request) = fixture(DriverKind::Codex);
        let supervisor = supervisor_at(dir.path(), &inventory.workspace);
        request.session_id = member(&supervisor, request.driver);
        let other = member(&supervisor, request.driver);
        for reported_only in [false, true] {
            supervisor.inner.slots.lock().get_by_id_mut(other).unwrap().recovery = Some(crate::RecoveryReference {
                original_id: (!reported_only).then(|| request.conversation_id.to_uppercase()),
                provenance: "fixture_held".into(), reason: "Identity is held".into(),
                reported_codex: reported_only.then(|| crate::session::CodexIdentityEvidence {
                    thread_id: request.conversation_id.clone(), run_id: Uuid::new_v4(), generation: 1,
                    rollout_path: PathBuf::from(&inventory.candidates[0].metadata_paths[0]), reported_at: now_rfc3339(),
                }),
            });
            assert!(supervisor.attach_local_session_scanning(request.clone(), |_| Inventory {
                workspace: inventory.workspace.clone(), candidates: inventory.candidates.clone(),
                providers: Vec::new(), truncated: false,
            }).unwrap_err().to_string().contains("already uses or holds"));
        }
        supervisor.shutdown().unwrap();
    }
}
