// Included in the supervisor test module. A108 upgrade wiring: mock PTYs, a
// mock desktop host and temporary runtimes only. No helper, scheduler, real
// process identity, shutdown of a real app or harness runs.

struct MockUpgradeHost {
    identity: upgrade::UpgradeSource,
    registered: Mutex<usize>,
    register_error: Option<String>,
    alive: AtomicBool,
    supervisor: Mutex<Weak<SupervisorInner>>,
    exits: AtomicUsize,
    consumed: Mutex<Vec<Uuid>>,
}

impl MockUpgradeHost {
    fn new(identity: upgrade::UpgradeSource) -> Arc<Self> {
        Arc::new(Self {
            identity,
            registered: Mutex::new(0),
            register_error: None,
            alive: AtomicBool::new(true),
            supervisor: Mutex::new(Weak::new()),
            exits: AtomicUsize::new(0),
            consumed: Mutex::new(Vec::new()),
        })
    }
}

impl upgrade_flow::UpgradeHost for MockUpgradeHost {
    fn process_identity(&self) -> Result<upgrade::UpgradeSource> {
        Ok(self.identity.clone())
    }
    fn register_helper(&self, helper: &Path, request: &Path) -> Result<()> {
        assert!(helper.ends_with("continue-upgrade.ps1") && request.ends_with("request.json"));
        *self.registered.lock() += 1;
        match &self.register_error {
            Some(error) => Err(anyhow!(error.clone())),
            None => Ok(()),
        }
    }
    fn process_alive(&self, _pid: u32, _creation_filetime: &str) -> Result<bool> {
        Ok(self.alive.load(Ordering::SeqCst))
    }
    fn strict_shutdown(&self) -> Result<()> {
        let inner = self.supervisor.lock().upgrade().ok_or_else(|| anyhow!("supervisor dropped"))?;
        SupervisorHandle { inner }.shutdown_for_upgrade()
    }
    fn exit_for_upgrade(&self) {
        self.exits.fetch_add(1, Ordering::SeqCst);
    }
    fn resume_consumed(&self, record: &upgrade::UpgradeRecord) {
        self.consumed.lock().push(record.request.attempt_id);
    }
}

const UPGRADE_SOURCE_PID: u32 = 9701;
const UPGRADE_CONVERSATION: &str = "claude-conversation-1";

struct UpgradeFixture {
    root: PathBuf,
    supervisor: SupervisorHandle,
    host: Arc<MockUpgradeHost>,
    caller: PaneCaller,
    claude: SessionId,
    room: RoomId,
    candidate: PathBuf,
    sha: String,
}

fn upgrade_fixture() -> UpgradeFixture {
    upgrade_fixture_with(None)
}

fn upgrade_fixture_with(register_error: Option<&str>) -> UpgradeFixture {
    let root = std::env::temp_dir().join(format!("prim1-upgrade-flow-{}", Uuid::new_v4()));
    let supervisor = test_supervisor_with_root(root.clone());
    let claude = test_session_id(&supervisor, "claude");
    let tasks = tasks::TaskStore::open(&root.join("tasks")).unwrap();
    let task = tasks.create("Victor", tasks::NewTask {
        title: "Ship A108".into(), request: "wire the upgrade".into(), source: "test".into(), ..Default::default()
    }, None).unwrap();
    assert_eq!(task.id, "T-1");
    supervisor.set_task_store(Some(Arc::new(tasks)));
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Alpha".into()), member_ids: vec![claude],
        brief_on_join: false, brief_template: None }).unwrap().room_id;
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "claude", DriverKind::Claude,
        recording_pty_session(UPGRADE_SOURCE_PID).0);
    supervisor.inner.slots.lock().get_by_id_mut(claude).unwrap().harness_session_id = Some(UPGRADE_CONVERSATION.into());
    supervisor.persist_slot_harness_state(claude);
    let caller = supervisor.resolve_pane_caller(TestPaneProcess::new(UPGRADE_SOURCE_PID + 10_000, [UPGRADE_SOURCE_PID])).unwrap();
    supervisor.set_member_permissions(claude, MemberPermissions { roam: false, maintain: true, manage: false }).unwrap();
    let mut host = MockUpgradeHost::new(upgrade::UpgradeSource {
        pid: 123,
        creation_filetime: "134000000000000001".into(),
        exe_path: root.join("bin").join("prim1-experimental.exe"),
        exe_sha256: "a".repeat(64),
    });
    Arc::get_mut(&mut host).unwrap().register_error = register_error.map(str::to_owned);
    *host.supervisor.lock() = Arc::downgrade(&supervisor.inner);
    supervisor.install_upgrade(upgrade_flow::UpgradeEnvironment {
        profile_dir: root.join("profile"),
        personal_repo_override: Some("off".into()),
        startup: Ok(None),
    }, host.clone()).unwrap();
    supervisor.report_operator_room_drafts(false);
    // The notice already elapsed; the transaction's own clock checks stay real.
    supervisor.set_upgrade_backdate_for_tests(chrono::Duration::seconds(upgrade::NOTICE_SECONDS + 1));
    let candidate = root.join("build").join("prim1.exe");
    fs::create_dir_all(candidate.parent().unwrap()).unwrap();
    let bytes = b"MZ upgrade candidate fixture; never executed";
    fs::write(&candidate, bytes).unwrap();
    let sha = {
        use sha2::Digest as _;
        format!("{:x}", sha2::Sha256::digest(bytes))
    };
    UpgradeFixture { root, supervisor, host, caller, claude, room, candidate, sha }
}

/// A request whose visible notice has already run, like the fixture's backdate.
fn upgrade_request(f: &UpgradeFixture) -> SidebandResponse {
    let response = upgrade_request_shown_now(f);
    if response.ok {
        f.supervisor.age_upgrade_notice_for_tests(chrono::Duration::seconds(upgrade::NOTICE_SECONDS + 1));
    }
    response
}

fn upgrade_request_shown_now(f: &UpgradeFixture) -> SidebandResponse {
    f.supervisor.apply_sideband_request(&f.caller, SidebandRequest::UpgradeRequest {
        candidate_exe: f.candidate.display().to_string(),
        sha256: f.sha.to_uppercase(),
        task_id: "T-1".into(),
        task_revision: 1,
    })
}

fn upgrade_admitted(response: SidebandResponse) -> shared_types::UpgradeAdmission {
    assert!(response.ok, "{}", response.message);
    match response.payload {
        Some(SidebandResponsePayload::Upgrade { admission }) => admission,
        other => panic!("expected an upgrade admission, got {other:?}"),
    }
}

fn upgrade_helper_ready(f: &UpgradeFixture, attempt: Uuid) {
    let record = f.supervisor.upgrade_record_for_tests(attempt);
    rebuild::persist_bounded_json(record.request_path.parent().unwrap(), "helper-ready.json", &upgrade::HelperReady {
        schema_version: upgrade::SCHEMA_VERSION,
        attempt_id: attempt,
        request_sha256: record.state.request_sha256.clone(),
        helper_pid: 4555,
        helper_creation_filetime: "134000000000000002".into(),
        at_utc: Utc::now(),
    }, 16 * 1024).unwrap();
}

fn upgrade_helper_result(record: &upgrade::UpgradeRecord, outcome: upgrade::HelperOutcome, process: Option<upgrade::UpgradeSource>) {
    rebuild::persist_bounded_json(record.request_path.parent().unwrap(), "helper-result.json", &upgrade::HelperResult {
        schema_version: upgrade::SCHEMA_VERSION,
        attempt_id: record.request.attempt_id,
        request_sha256: record.state.request_sha256.clone(),
        outcome,
        candidate_process: process,
        at_utc: Utc::now(),
        message: None,
    }, 16 * 1024).unwrap();
}

/// Every stored message of the room: (request id, content).
fn upgrade_room_messages(supervisor: &SupervisorHandle, room_id: RoomId) -> Vec<(String, String)> {
    supervisor.read_room_feed(ReadRoomFeedRequest { room_id, cursor: None }).unwrap().events.into_iter()
        .filter_map(|event| match event.item {
            RoomFeedItem::Message { request_id, content, .. } => Some((request_id, content)),
            _ => None,
        }).collect()
}

fn upgrade_room_senders(supervisor: &SupervisorHandle, room_id: RoomId) -> Vec<(shared_types::RoomMessageSender, String)> {
    supervisor.read_room_feed(ReadRoomFeedRequest { room_id, cursor: None }).unwrap().events.into_iter()
        .filter_map(|event| match event.item {
            RoomFeedItem::Message { sender, content, .. } => Some((sender, content)),
            _ => None,
        }).collect()
}

fn candidate_identity(record: &upgrade::UpgradeRecord, pid: u32) -> upgrade::UpgradeSource {
    upgrade::UpgradeSource {
        pid,
        creation_filetime: format!("1340000000000{pid:05}"),
        exe_path: record.request.candidate.exe_path.clone(),
        exe_sha256: record.request.candidate.exe_sha256.clone(),
    }
}

/// The new build on the same runtime, started with the helper's variables.
fn upgrade_candidate(root: &Path, record: &upgrade::UpgradeRecord, identity: upgrade::UpgradeSource)
    -> (SupervisorHandle, Arc<MockUpgradeHost>, Arc<Mutex<Vec<Vec<String>>>>) {
    let supervisor = SupervisorHandle::new(test_supervisor_config(root)).unwrap();
    supervisor.set_executable_resolver_for_tests(Arc::new(TestExecutableResolver));
    let args = Arc::new(Mutex::new(Vec::new()));
    let sessions = (0..4).map(|index| recording_pty_session(9810 + index).0).collect();
    supervisor.set_pty_spawner_for_tests(Arc::new(ArgRecordingPtySpawner { args: args.clone(), sessions: Mutex::new(sessions) }));
    let host = MockUpgradeHost::new(identity);
    supervisor.install_upgrade(upgrade_flow::UpgradeEnvironment {
        profile_dir: root.join("profile"),
        personal_repo_override: Some("off".into()),
        startup: Ok(Some(upgrade_flow::UpgradeStartup {
            attempt_id: record.request.attempt_id,
            request_file: record.request_path.clone(),
        })),
    }, host.clone()).unwrap();
    (supervisor, host, args)
}

/// Request, helper ready, close: the source process records ShutdownComplete.
fn upgrade_through_close(f: UpgradeFixture) -> (PathBuf, upgrade::UpgradeRecord, Arc<MockUpgradeHost>) {
    let admission = upgrade_admitted(upgrade_request(&f));
    upgrade_helper_ready(&f, admission.attempt_id);
    assert!(!f.supervisor.upgrade_close_step(admission.attempt_id));
    assert_eq!(f.host.exits.load(Ordering::SeqCst), 1, "{:?}", f.supervisor.upgrade_status());
    let record = f.supervisor.upgrade_record_for_tests(admission.attempt_id);
    assert_eq!(record.state.phase, upgrade::UpgradePhase::ShutdownComplete);
    let UpgradeFixture { root, supervisor, host, .. } = f;
    drop(supervisor);
    (root, record, host)
}

#[test]
fn upgrade_startup_variables_are_both_or_neither_and_well_formed() {
    use std::ffi::OsString;
    let id = Uuid::new_v4().to_string();
    let file = std::env::temp_dir().join("runtime").join("request.json");
    let parse = |attempt: Option<&str>, path: Option<&Path>| upgrade_flow::parse_upgrade_startup(
        attempt.map(OsString::from), path.map(|path| path.as_os_str().to_owned()));
    assert_eq!(parse(None, None), Ok(None));
    assert_eq!(upgrade_flow::parse_upgrade_startup(Some(OsString::new()), Some(OsString::new())), Ok(None));
    assert_eq!(parse(Some(&id), Some(&file)), Ok(Some(upgrade_flow::UpgradeStartup {
        attempt_id: id.parse().unwrap(), request_file: file.clone() })));
    for (attempt, path) in [
        (Some(id.as_str()), None),
        (None, Some(file.as_path())),
        (Some("not-a-uuid"), Some(file.as_path())),
        (Some("00000000-0000-0000-0000-000000000000"), Some(file.as_path())),
        (Some(&*id.to_uppercase()), Some(file.as_path())),
        (Some(id.as_str()), Some(Path::new("relative\\request.json"))),
    ] {
        assert!(parse(attempt, path).is_err(), "{attempt:?} {path:?}");
    }
}

#[test]
fn upgrade_request_is_refused_without_the_maintain_grant() {
    let f = upgrade_fixture();
    f.supervisor.set_member_permissions(f.claude, MemberPermissions::default()).unwrap();
    let refused = upgrade_request(&f);
    assert!(!refused.ok && refused.message.contains("Can upgrade PRIM-1")
        && refused.message.contains("propose the upgrade as a task"), "{}", refused.message);
    assert_eq!(f.supervisor.current_upgrade_for_tests(), None);
    assert_eq!(*f.host.registered.lock(), 0);
    // A stale run is refused before any grant is consulted.
    f.supervisor.set_member_permissions(f.claude, MemberPermissions { roam: false, maintain: true, manage: false }).unwrap();
    install_mock_running_session_with_bracketed_paste_enabled(&f.supervisor, "claude", DriverKind::Claude,
        recording_pty_session(UPGRADE_SOURCE_PID).0);
    let stale = upgrade_request(&f);
    assert!(!stale.ok && stale.message.contains("stale"), "{}", stale.message);
}

#[test]
fn upgrade_request_names_each_blocker_and_stages_nothing() {
    let f = upgrade_fixture();
    let codex = test_session_id(&f.supervisor, "codex");
    type Change = Box<dyn Fn(&UpgradeFixture, bool)>;
    let slot_change = |change: fn(&mut SessionSlot, bool)| -> Change {
        Box::new(move |f: &UpgradeFixture, on: bool| change(f.supervisor.inner.slots.lock().get_by_id_mut(f.claude).unwrap(), on))
    };
    let cases: Vec<(&str, Change)> = vec![
        ("conversation ID is not known", slot_change(|slot, on| {
            slot.harness_session_id = (!on).then(|| UPGRADE_CONVERSATION.to_string());
        })),
        ("question or approval open", slot_change(|slot, on| {
            slot.room_composer = room_composer::RoomComposer::default();
            if on {
                slot.room_composer.resize(60, 8);
                slot.room_composer.observe("\x1b[2J\x1b[HDo you want to proceed?\r\n\u{276f} 1. Yes\r\n  2. No\r\nEsc to cancel");
            }
        })),
        // Blocked with a screen not yet known to be clear keeps the latch.
        ("question or approval open", slot_change(|slot, on| {
            slot.room_composer = room_composer::RoomComposer::default();
            slot.work_state = if on { WorkState::Blocked } else { WorkState::Idle };
            if on {
                slot.room_composer.resize(60, 8);
                slot.room_composer.observe("\x1b[2J\x1b[H\u{276f} 1. Yes\r\n  2. No\r\nEsc to cancel");
                slot.room_composer.resize(70, 8);
            }
        })),
        ("unsent text typed into its terminal", slot_change(|slot, on| {
            slot.running.as_ref().unwrap().input_gate.room_input_activity.lock()
                .record_input(if on { "half typed" } else { "\r" });
        })),
        ("has no recorded outcome yet", slot_change(|slot, on| {
            slot.running.as_ref().unwrap().input_gate.room_wake_pending.store(on, Ordering::SeqCst);
        })),
        ("starting or stopping", slot_change(|slot, on| {
            slot.lifecycle_operation = on.then_some(LifecycleOperation { generation: slot.generation, kind: StopIntentKind::Operator });
        })),
        ("unsent message in a room composer", Box::new(|f: &UpgradeFixture, on: bool| f.supervisor.report_operator_room_drafts(on))),
        ("delivery outcome is not recorded", Box::new(|f: &UpgradeFixture, on: bool| {
            let mut rooms = f.supervisor.inner.rooms.lock();
            let room = rooms.get_mut(f.room).unwrap();
            if on { room.in_flight_deliveries += 1 } else { room.in_flight_deliveries -= 1 }
        })),
        ("(Codex) has not identified its conversation", Box::new(move |f: &UpgradeFixture, on: bool| {
            if on {
                f.supervisor.add_room_member(AddRoomMemberRequest { room_id: f.room, session_id: codex }).unwrap();
                install_mock_running_session_with_bracketed_paste_enabled(&f.supervisor, "codex", DriverKind::Codex,
                    recording_pty_session(9702).0);
                // A launch leaves Codex under its launch hold until it is proved.
                f.supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().recovery =
                    Some(RecoveryReference::unresolved_codex(None, false));
            } else {
                f.supervisor.remove_room_member(RemoveRoomMemberRequest { room_id: f.room, session_id: codex }).unwrap();
                let mut slots = f.supervisor.inner.slots.lock();
                let slot = slots.get_by_id_mut(codex).unwrap();
                slot.running = None;
                slot.run_id = None;
                slot.state = LifecycleState::Closed;
            }
        })),
    ];
    for (expected, change) in &cases {
        change(&f, true);
        let refused = upgrade_request(&f);
        assert!(!refused.ok && refused.message.contains(expected) && refused.message.contains("Nothing was stopped"),
            "{expected}: {}", refused.message);
        change(&f, false);
    }
    assert_eq!(f.supervisor.current_upgrade_for_tests(), None);
    assert_eq!(*f.host.registered.lock(), 0);
    // A window that has not reported its composers yet is unknown, not empty.
    f.supervisor.forget_room_drafts_for_tests();
    let unknown = upgrade_request(&f);
    assert!(!unknown.ok && unknown.message.contains("has not reported"), "{}", unknown.message);
    f.supervisor.report_operator_room_drafts(false);
    // The checkpoint must exist in the task store.
    let missing = f.supervisor.apply_sideband_request(&f.caller, SidebandRequest::UpgradeRequest {
        candidate_exe: f.candidate.display().to_string(), sha256: f.sha.clone(), task_id: "T-1".into(), task_revision: 2 });
    assert!(!missing.ok && missing.message.contains("not one of its checkpoints"), "{}", missing.message);
    // Everything resolved: admitted, and a second request waits for the first.
    let admission = upgrade_admitted(upgrade_request(&f));
    let again = upgrade_request(&f);
    assert!(!again.ok && again.message.contains("already prepared"), "{}", again.message);
    assert_eq!(f.supervisor.current_upgrade_for_tests(), Some(admission.attempt_id));
}

#[test]
fn a_codex_followed_by_w2_is_captured_exact_and_resumed_while_a_held_one_blocks() {
    let f = upgrade_fixture();
    let codex = test_session_id(&f.supervisor, "codex");
    f.supervisor.add_room_member(AddRoomMemberRequest { room_id: f.room, session_id: codex }).unwrap();
    install_mock_running_session_with_process_id_and_bracketed_paste_enabled(&f.supervisor, "codex", DriverKind::Codex,
        Some(9702), recording_pty_session(9702).0);
    let thread = Uuid::new_v4().to_string();
    let (run, generation) = {
        let slots = f.supervisor.inner.slots.lock();
        let slot = slots.get_by_id(codex).unwrap();
        (slot.run_id.unwrap(), slot.generation)
    };
    // Held with no report from this run (none, an earlier run's, or another
    // hold over a stored ID): a blocker, nothing staged.
    let earlier_report = RecoveryReference {
        original_id: None, provenance: "job_attested_codex_shell_report".into(), reason: "reported".into(),
        reported_codex: Some(session::CodexIdentityEvidence { thread_id: thread.clone(), run_id: Uuid::new_v4(), generation,
            rollout_path: PathBuf::from("rollout.jsonl"), reported_at: now_rfc3339() }),
    };
    for (hold, stored) in [
        (RecoveryReference::unresolved_codex(None, false), None),
        (earlier_report, Some(thread.clone())),
        (RecoveryReference::resume_refused(Some(thread.clone()), "refused".into()), Some(thread.clone())),
    ] {
        {
            let mut slots = f.supervisor.inner.slots.lock();
            let slot = slots.get_by_id_mut(codex).unwrap();
            slot.recovery = Some(hold);
            slot.harness_session_id = stored;
        }
        let refused = upgrade_request(&f);
        assert!(!refused.ok && refused.message.contains("codex (Codex) has not identified its conversation in this run"),
            "{}", refused.message);
    }
    assert_eq!(f.supervisor.current_upgrade_for_tests(), None);
    // W2's writer-lock proof lifts the launch hold: exactly known, as for Claude.
    {
        let mut slots = f.supervisor.inner.slots.lock();
        let slot = slots.get_by_id_mut(codex).unwrap();
        slot.recovery = Some(RecoveryReference::unresolved_codex(None, false));
        slot.harness_session_id = None;
    }
    assert!(f.supervisor.apply_tracked_conversation(codex, run, generation, &thread, "launch", "writer_lock", 9702, None).unwrap());
    let (root, record, _source_host) = upgrade_through_close(f);
    let member = record.request.members.iter().find(|member| member.session_id == codex).unwrap();
    assert_eq!((member.resume_mode, member.conversation_id.as_deref()), (upgrade::UpgradeResumeMode::Exact, Some(thread.as_str())));

    let identity = candidate_identity(&record, 4242);
    upgrade_helper_result(&record, upgrade::HelperOutcome::Launched, Some(identity.clone()));
    let (candidate, _host, args) = upgrade_candidate(&root, &record, identity);
    {
        // The catalog reloads Codex under its identity hold, which resumes
        // only a rollout-verified ID; once lifted (by a proof or Resume
        // here), the stored ID is the one Launch resumes.
        let mut slots = candidate.inner.slots.lock();
        let slot = slots.get_by_id_mut(codex).unwrap();
        let hold = slot.recovery.take().unwrap();
        assert_eq!((hold.provenance.as_str(), hold.original_id.as_deref()), ("legacy_catalog", Some(thread.as_str())));
        assert_eq!(slot.harness_session_id.as_deref(), Some(thread.as_str()));
    }
    candidate.begin_upgrade_resume();
    let spawned = args.lock().clone();
    assert!(spawned.iter().any(|spawn| spawn.windows(2).any(|pair| pair[0] == "resume" && pair[1] == thread)), "{spawned:?}");
    let status = candidate.upgrade_status().unwrap();
    assert_eq!(status.stage, shared_types::UpgradeStage::Resumed);
    assert!(status.message.contains("2 member(s) resumed") && !status.message.contains("Not resumed"), "{}", status.message);
}

#[test]
fn an_unstartable_helper_cancels_the_attempt_before_anything_stops() {
    let f = upgrade_fixture_with(Some("Task Scheduler refused"));
    let refused = upgrade_request(&f);
    assert!(!refused.ok && refused.message.contains("Task Scheduler refused") && refused.message.contains("nothing was stopped"),
        "{}", refused.message);
    let attempt = f.supervisor.current_upgrade_for_tests().unwrap();
    assert_eq!(f.supervisor.upgrade_record_for_tests(attempt).state.phase, upgrade::UpgradePhase::Cancelled);
    assert!(f.supervisor.inner.slots.lock().get_by_id(f.claude).unwrap().running.is_some());
}

#[test]
fn admission_records_the_exact_running_set_and_announces_a_cancellable_notice() {
    let f = upgrade_fixture();
    let term = create_test_session(&f.supervisor, "term", DriverKind::GenericTerminal, shared_types::PermissionProfile::Normal).session_id;
    f.supervisor.add_room_member(AddRoomMemberRequest { room_id: f.room, session_id: term }).unwrap();
    install_mock_running_session_with_bracketed_paste_enabled(&f.supervisor, "term", DriverKind::GenericTerminal,
        recording_pty_session(9703).0);
    let events = capture_runtime_events(&f.supervisor);
    let admission = upgrade_admitted(upgrade_request(&f));
    assert_eq!((admission.resumed_members.clone(), admission.interrupted_members.clone()), (vec!["claude".to_string()], vec!["term".to_string()]));
    let record = f.supervisor.upgrade_record_for_tests(admission.attempt_id);
    assert_eq!(record.request.candidate.exe_sha256, f.sha, "the hash is stored lowercase");
    assert_eq!(record.request.personal_repo_override.as_deref(), Some("off"));
    assert_eq!(record.request.requester_session_id, f.claude);
    let claude = record.request.members.iter().find(|member| member.session_id == f.claude).unwrap();
    assert_eq!((claude.resume_mode, claude.conversation_id.as_deref()), (upgrade::UpgradeResumeMode::Exact, Some(UPGRADE_CONVERSATION)));
    assert_eq!(claude.task_ref, Some(upgrade::TaskRef { task_id: "T-1".into(), revision: 1 }));
    let terminal = record.request.members.iter().find(|member| member.session_id == term).unwrap();
    assert_eq!((terminal.resume_mode, terminal.conversation_id.clone(), terminal.task_ref.clone()),
        (upgrade::UpgradeResumeMode::InterruptedWithoutResume, None, None));
    assert_eq!(*f.host.registered.lock(), 1);
    let notices = upgrade_room_messages(&f.supervisor, f.room);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].0, format!("upgrade-notice-{}", admission.attempt_id));
    assert!(notices[0].1.contains("Not restarted: term") && notices[0].1.contains("Victor can cancel"), "{}", notices[0].1);
    let status = f.supervisor.upgrade_status().unwrap();
    assert_eq!((status.stage, status.can_cancel, status.can_abandon), (shared_types::UpgradeStage::Notice, true, false));
    assert!(events.lock().iter().any(|event| matches!(event, RuntimeEvent::UpgradeStatusChanged { status: Some(status), .. }
        if status.attempt_id == admission.attempt_id)));

    // Operator Cancel: a notice, nothing stopped, and the timer stops.
    f.supervisor.cancel_upgrade(admission.attempt_id).unwrap();
    assert_eq!(f.supervisor.upgrade_record_for_tests(admission.attempt_id).state.phase, upgrade::UpgradePhase::Cancelled);
    assert_eq!(f.supervisor.upgrade_status(), None);
    let notices = upgrade_room_messages(&f.supervisor, f.room);
    assert!(notices.last().unwrap().1.contains("was cancelled: Victor cancelled it"), "{notices:?}");
    upgrade_helper_ready(&f, admission.attempt_id);
    assert!(!f.supervisor.upgrade_close_step(admission.attempt_id));
    assert_eq!(f.host.exits.load(Ordering::SeqCst), 0);
    assert!(f.supervisor.inner.slots.lock().get_by_id(f.claude).unwrap().running.is_some());
    assert!(f.supervisor.cancel_upgrade(admission.attempt_id).is_err());
}

#[test]
fn close_waits_for_the_helper_and_blockers_and_cancels_when_the_running_set_changes() {
    let f = upgrade_fixture();
    let admission = upgrade_admitted(upgrade_request(&f));
    // No helper readiness yet: keep waiting, nothing closes.
    assert!(f.supervisor.upgrade_close_step(admission.attempt_id));
    upgrade_helper_ready(&f, admission.attempt_id);
    // A blocker postpones closing and new starts stay admitted meanwhile.
    f.supervisor.report_operator_room_drafts(true);
    assert!(f.supervisor.upgrade_close_step(admission.attempt_id));
    assert!(f.supervisor.upgrade_status().unwrap().message.contains("Closing waits"));
    assert!(f.supervisor.ensure_not_upgrade_closing().is_ok());
    f.supervisor.report_operator_room_drafts(false);
    // The member restarted during the notice: the recorded set is stale.
    {
        let mut slots = f.supervisor.inner.slots.lock();
        let slot = slots.get_by_id_mut(f.claude).unwrap();
        slot.generation += 1;
        slot.run_id = Some(Uuid::new_v4());
    }
    assert!(!f.supervisor.upgrade_close_step(admission.attempt_id));
    assert_eq!(f.supervisor.upgrade_record_for_tests(admission.attempt_id).state.phase, upgrade::UpgradePhase::Cancelled);
    assert!(upgrade_room_messages(&f.supervisor, f.room).last().unwrap().1.contains("claude stopped or restarted"));
    assert_eq!(f.host.exits.load(Ordering::SeqCst), 0);
    assert!(f.supervisor.ensure_not_upgrade_closing().is_ok());
}

#[test]
fn a_conversation_followed_during_the_notice_cancels_instead_of_closing() {
    let f = upgrade_fixture();
    let admission = upgrade_admitted(upgrade_request(&f));
    upgrade_helper_ready(&f, admission.attempt_id);
    // W2 follows a /clear: same session, run and generation, new conversation.
    let (run, generation) = {
        let mut slots = f.supervisor.inner.slots.lock();
        let slot = slots.get_by_id_mut(f.claude).unwrap();
        slot.process_id = Some(UPGRADE_SOURCE_PID);
        (slot.run_id.unwrap(), slot.generation)
    };
    let cleared = Uuid::new_v4().to_string();
    assert!(f.supervisor.apply_tracked_conversation(f.claude, run, generation, &cleared, "clear", "pid_file",
        UPGRADE_SOURCE_PID, None).unwrap());
    assert!(!f.supervisor.upgrade_close_step(admission.attempt_id));
    assert_eq!(f.supervisor.upgrade_record_for_tests(admission.attempt_id).state.phase, upgrade::UpgradePhase::Cancelled);
    assert_eq!(f.host.exits.load(Ordering::SeqCst), 0);
    assert!(upgrade_room_messages(&f.supervisor, f.room).last().unwrap().1
        .contains("cancelled: claude changed conversation during the notice; ask again"));
    let slots = f.supervisor.inner.slots.lock();
    let claude = slots.get_by_id(f.claude).unwrap();
    assert!(claude.running.is_some() && claude.run_id == Some(run), "nothing was shut down");
    assert_eq!(claude.harness_session_id.as_deref(), Some(cleared.as_str()));
    drop(slots);
    assert!(f.supervisor.ensure_not_upgrade_closing().is_ok());

    // Leaving its room during the notice is stale the same way.
    let f = upgrade_fixture();
    let admission = upgrade_admitted(upgrade_request(&f));
    upgrade_helper_ready(&f, admission.attempt_id);
    f.supervisor.remove_room_member(RemoveRoomMemberRequest { room_id: f.room, session_id: f.claude }).unwrap();
    let events = capture_runtime_events(&f.supervisor);
    assert!(!f.supervisor.upgrade_close_step(admission.attempt_id));
    let record = f.supervisor.upgrade_record_for_tests(admission.attempt_id);
    assert_eq!(record.state.phase, upgrade::UpgradePhase::Cancelled);
    assert!(events.lock().iter().any(|event| matches!(event, RuntimeEvent::SystemLog { message, .. }
        if message.contains("cancelled: claude changed room during the notice; ask again"))));
    assert_eq!(f.host.exits.load(Ordering::SeqCst), 0);
    assert!(f.supervisor.inner.slots.lock().get_by_id(f.claude).unwrap().running.is_some());
}

#[test]
fn closing_waits_a_full_notice_after_a_slow_helper_registration() {
    let f = upgrade_fixture();
    // The helper took 65 s to register: the module's notice ended before
    // the banner and room notices were published.
    f.supervisor.set_upgrade_backdate_for_tests(chrono::Duration::seconds(65));
    let admission = upgrade_admitted(upgrade_request_shown_now(&f));
    let record = f.supervisor.upgrade_record_for_tests(admission.attempt_id);
    assert!(record.request.notice_until < Utc::now());
    let shown_until = DateTime::parse_from_rfc3339(&admission.notice_until).unwrap().with_timezone(&Utc);
    assert!(shown_until > Utc::now() + chrono::Duration::seconds(upgrade::NOTICE_SECONDS - 10), "{}", admission.notice_until);
    assert_eq!(f.supervisor.upgrade_status().unwrap().notice_until, admission.notice_until, "the banner counts down the same end");
    upgrade_helper_ready(&f, admission.attempt_id);
    assert!(f.supervisor.upgrade_close_step(admission.attempt_id), "the visible notice is still running");
    assert_eq!(f.supervisor.upgrade_record_for_tests(admission.attempt_id).state.phase, upgrade::UpgradePhase::Prepared);
    assert_eq!(f.host.exits.load(Ordering::SeqCst), 0);
    assert!(f.supervisor.upgrade_status().unwrap().can_cancel);
    // A full notice after it was published: it closes.
    f.supervisor.age_upgrade_notice_for_tests(chrono::Duration::seconds(upgrade::NOTICE_SECONDS + 1));
    assert!(!f.supervisor.upgrade_close_step(admission.attempt_id));
    assert_eq!(f.host.exits.load(Ordering::SeqCst), 1);
    assert_eq!(f.supervisor.upgrade_record_for_tests(admission.attempt_id).state.phase, upgrade::UpgradePhase::ShutdownComplete);

    // Less than five minutes would remain once the visible notice ends: cancelled.
    let f = upgrade_fixture();
    f.supervisor.set_upgrade_backdate_for_tests(chrono::Duration::seconds(
        upgrade::EXPIRY_SECONDS - upgrade::CLOSE_REMAINING_SECONDS + 30));
    let admission = upgrade_admitted(upgrade_request(&f));
    upgrade_helper_ready(&f, admission.attempt_id);
    assert!(!f.supervisor.upgrade_close_step(admission.attempt_id));
    assert_eq!(f.supervisor.upgrade_record_for_tests(admission.attempt_id).state.phase, upgrade::UpgradePhase::Cancelled);
    assert_eq!(f.host.exits.load(Ordering::SeqCst), 0);
    let notices = upgrade_room_messages(&f.supervisor, f.room);
    assert!(notices.last().unwrap().1.contains("cancelled: less than five minutes would remain before it expires; ask again"),
        "{notices:?}");
}

#[test]
fn a_dead_helper_cancels_instead_of_closing() {
    let f = upgrade_fixture();
    let admission = upgrade_admitted(upgrade_request(&f));
    upgrade_helper_ready(&f, admission.attempt_id);
    f.host.alive.store(false, Ordering::SeqCst);
    assert!(!f.supervisor.upgrade_close_step(admission.attempt_id));
    assert_eq!(f.supervisor.upgrade_record_for_tests(admission.attempt_id).state.phase, upgrade::UpgradePhase::Cancelled);
    assert_eq!(f.host.exits.load(Ordering::SeqCst), 0);
}

#[test]
fn starts_restarts_and_new_sessions_are_refused_while_closing() {
    let f = upgrade_fixture();
    let codex = test_session_id(&f.supervisor, "codex");
    f.supervisor.inner.upgrade_closing.store(true, Ordering::SeqCst);
    for refused in [
        f.supervisor.start_session_by_id(codex).map(drop),
        f.supervisor.restart_session_by_id(f.claude).map(drop),
        f.supervisor.create_session(shared_types::CreateSessionRequest { codex_transport: Default::default(), permissions: None, label: Some("late".into()), driver: DriverKind::Claude,
            permission_profile: shared_types::PermissionProfile::Normal, linux_working_directory: None, room_id: None,
            launch_choices: None }).map(drop),
    ] {
        assert_eq!(refused.unwrap_err().to_string(), upgrade_flow::UPGRADE_CLOSING_REFUSAL);
    }
}

#[test]
fn a_managing_member_cannot_create_start_or_restart_while_closing() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let helper = create_test_session(&supervisor, "helper", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let spare = create_test_session(&supervisor, "spare", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9481);
    let room = mobility_room(&supervisor, "Alpha", vec![claude, helper, spare]);
    supervisor.set_member_permissions(claude, MemberPermissions { roam: false, maintain: false, manage: true }).unwrap();
    queue_mock_starts(&supervisor, 2);
    let running = member_done(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), start_action("helper"))).member;
    let sessions = supervisor.snapshot().sessions.len();
    let notices = mobility_messages(&supervisor, room.room_id).len();

    supervisor.inner.upgrade_closing.store(true, Ordering::SeqCst);
    let create_id = Uuid::new_v4().to_string();
    for (request_id, action) in [
        (create_id.clone(), create_action(DriverKind::Grok, "QA Grok")),
        (Uuid::new_v4().to_string(), start_action("spare")),
        (Uuid::new_v4().to_string(), restart_action("helper")),
    ] {
        member_refused(member_request(&supervisor, &caller, &request_id, action), upgrade_flow::UPGRADE_CLOSING_REFUSAL);
    }
    assert_eq!(supervisor.snapshot().sessions.len(), sessions, "no member was created");
    assert!(!session_now(&supervisor, spare).running);
    let helper_now = session_now(&supervisor, helper);
    assert!(helper_now.running && helper_now.generation == running.generation, "{helper_now:?}");
    assert_eq!(mobility_messages(&supervisor, room.room_id).len(), notices, "a refusal posts no notice");

    // A refusal records no result: the same request_id acts once PRIM-1 is open again.
    supervisor.inner.upgrade_closing.store(false, Ordering::SeqCst);
    let created = member_done(member_request(&supervisor, &caller, &create_id, create_action(DriverKind::Grok, "QA Grok")));
    assert!(!created.replayed, "{created:?}");
    assert_eq!(supervisor.snapshot().sessions.len(), sessions + 1);
}

#[test]
fn strict_upgrade_shutdown_fails_when_the_final_catalog_write_fails() {
    for strict in [true, false] {
        let supervisor = test_supervisor();
        let claude = test_session_id(&supervisor, "claude");
        supervisor.inner.slots.lock().get_by_id_mut(claude).unwrap().last_pty_size = Some((99, 33));
        supervisor.inner.fail_next_catalog_write.store(true, Ordering::SeqCst);
        let outcome = if strict { supervisor.shutdown_for_upgrade() } else { supervisor.shutdown() };
        if strict {
            let error = outcome.unwrap_err().to_string();
            assert!(error.contains("final session catalog write failed"), "{error}");
        } else {
            outcome.expect("an ordinary close keeps the size checkpoint warning-only");
        }
    }
}

#[test]
fn a_failed_strict_close_holds_the_attempt_without_recording_shutdown() {
    let f = upgrade_fixture();
    let admission = upgrade_admitted(upgrade_request(&f));
    upgrade_helper_ready(&f, admission.attempt_id);
    f.supervisor.inner.slots.lock().get_by_id_mut(f.claude).unwrap().last_pty_size = Some((99, 33));
    f.supervisor.inner.fail_next_catalog_write.store(true, Ordering::SeqCst);
    assert!(!f.supervisor.upgrade_close_step(admission.attempt_id));
    assert_eq!(f.host.exits.load(Ordering::SeqCst), 0);
    assert_eq!(f.supervisor.upgrade_record_for_tests(admission.attempt_id).state.phase, upgrade::UpgradePhase::Closing);
    let status = f.supervisor.upgrade_status().unwrap();
    assert_eq!((status.stage, status.can_abandon), (shared_types::UpgradeStage::Held, true));
    assert!(status.message.contains("could not close cleanly"), "{}", status.message);
}

#[test]
fn an_expired_close_stays_held_until_the_operator_abandons_it() {
    let f = upgrade_fixture();
    let admission = upgrade_admitted(upgrade_request(&f));
    upgrade_helper_ready(&f, admission.attempt_id);
    f.supervisor.inner.slots.lock().get_by_id_mut(f.claude).unwrap().last_pty_size = Some((99, 33));
    f.supervisor.inner.fail_next_catalog_write.store(true, Ordering::SeqCst);
    assert!(!f.supervisor.upgrade_close_step(admission.attempt_id));
    let record = f.supervisor.upgrade_record_for_tests(admission.attempt_id);
    let UpgradeFixture { root, supervisor, host, .. } = f;
    drop(supervisor);
    // The helper gave up: a definite pre-start failure, still recorded as closing.
    upgrade_helper_result(&record, upgrade::HelperOutcome::Expired, None);
    let restarted = SupervisorHandle::new(test_supervisor_config(&root)).unwrap();
    restarted.install_upgrade(upgrade_flow::UpgradeEnvironment { profile_dir: root.join("profile"),
        personal_repo_override: None, startup: Ok(None) }, host).unwrap();
    let status = restarted.upgrade_status().unwrap();
    assert_eq!((status.stage, status.can_abandon), (shared_types::UpgradeStage::Held, true));
    assert!(status.message.contains("Abandon it to allow another upgrade"), "{}", status.message);
    restarted.abandon_held_upgrade(admission.attempt_id).unwrap();
    assert_eq!(restarted.upgrade_record_for_tests(admission.attempt_id).state.phase, upgrade::UpgradePhase::Abandoned);
    assert_eq!(restarted.upgrade_status(), None);
}

#[test]
fn the_new_build_resumes_exact_conversations_once_and_consumes_the_attempt() {
    let f = upgrade_fixture();
    let term = create_test_session(&f.supervisor, "term", DriverKind::GenericTerminal, shared_types::PermissionProfile::Normal).session_id;
    f.supervisor.add_room_member(AddRoomMemberRequest { room_id: f.room, session_id: term }).unwrap();
    install_mock_running_session_with_bracketed_paste_enabled(&f.supervisor, "term", DriverKind::GenericTerminal,
        recording_pty_session(9703).0);
    let room = f.room;
    let sha = f.sha.clone();
    let (root, record, _source_host) = upgrade_through_close(f);
    let attempt = record.request.attempt_id;
    let identity = candidate_identity(&record, 4242);
    upgrade_helper_result(&record, upgrade::HelperOutcome::Launched, Some(identity.clone()));
    let claude_member = record.request.members.iter().find(|member| member.task_ref.is_some()).unwrap().clone();
    let continuation_id = format!("upgrade-{}", claude_member.continuation_id);

    // First run of the new build stops after the continuations (a crash
    // before complete_resume).
    let (candidate, host, args) = upgrade_candidate(&root, &record, identity.clone());
    let status = candidate.upgrade_status().unwrap();
    assert_eq!(status.stage, shared_types::UpgradeStage::Resuming);
    assert_eq!(status.member_session_ids.len(), 2, "the window's restart resume leaves both members alone");
    candidate.stop_upgrade_before_complete_for_tests();
    candidate.begin_upgrade_resume();
    assert_eq!(args.lock().len(), 1, "only the exact member starts; the terminal stays stopped");
    assert!(args.lock()[0].iter().any(|arg| *arg == format!("--resume={UPGRADE_CONVERSATION}")), "{:?}", args.lock());
    assert!(candidate.inner.slots.lock().get_by_id(term).unwrap().running.is_none());
    let messages = upgrade_room_messages(&candidate, room);
    let continuations: Vec<_> = messages.iter().filter(|(id, _)| *id == continuation_id).collect();
    assert_eq!(continuations.len(), 1);
    assert_eq!(continuations[0].1, format!(
        "PRIM-1 restarted on build {} (upgrade {}). Continue task T-1 (revision 1) from its current task state.",
        &sha[..8], &attempt.to_string()[..8]));
    assert!(messages.iter().any(|(_, text)| text.starts_with("term was stopped by the upgrade and was not restarted")), "{messages:?}");
    // v3 (Claude PRIM-1, 15a49bdf): the continuation is PRIM-1's own, asks
    // for an ACK, stays open until it is given and is woken by the pointer,
    // never typed in full; the upgrade's notices are PRIM-1's, feed only.
    let events = candidate.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor: None }).unwrap().events;
    let continuation = events.iter().find(|event| matches!(&event.item,
        RoomFeedItem::Message { request_id, .. } if *request_id == continuation_id)).unwrap().clone();
    let RoomFeedItem::Message { message_id, sender, ack_requested, recipient_ids, .. } = &continuation.item else { unreachable!() };
    assert_eq!((sender, *ack_requested, recipient_ids.as_slice()),
        (&shared_types::RoomMessageSender::Prim1 {}, true, [claude_member.session_id].as_slice()));
    assert!(candidate.inner.rooms.lock().get(room).unwrap().unread_for(claude_member.session_id).iter()
        .any(|event| event.cursor == continuation.cursor), "open until ACKed");
    // The planner's own decision; the recovery path's test types it end to end.
    assert!(crate::room_delivery::wakes_by_pointer(&continuation), "{message_id} wakes by pointer");
    let notices: Vec<_> = events.iter().filter_map(|event| match &event.item {
        RoomFeedItem::Message { sender, recipient_ids, content, .. } if content.starts_with("term was stopped") =>
            Some((sender.clone(), recipient_ids.is_empty())),
        _ => None }).collect();
    assert_eq!(notices, vec![(shared_types::RoomMessageSender::Prim1 {}, true)]);
    assert_eq!(candidate.upgrade_record_for_tests(attempt).state.phase, upgrade::UpgradePhase::Resuming);
    assert!(host.consumed.lock().is_empty());
    drop(candidate);

    // The same process identity continues: the exact conversation resumes
    // again, and the continuation is the stored one, not a second delivery.
    let (candidate, host, args) = upgrade_candidate(&root, &record, identity.clone());
    candidate.begin_upgrade_resume();
    assert_eq!(args.lock().len(), 1);
    assert!(args.lock()[0].iter().any(|arg| *arg == format!("--resume={UPGRADE_CONVERSATION}")));
    let messages = upgrade_room_messages(&candidate, room);
    assert_eq!(messages.iter().filter(|(id, _)| *id == continuation_id).count(), 1, "{messages:?}");
    assert_eq!(messages.iter().filter(|(_, text)| text.starts_with("term was stopped")).count(), 1);
    assert_eq!(candidate.upgrade_record_for_tests(attempt).state.phase, upgrade::UpgradePhase::Consumed);
    assert_eq!(*host.consumed.lock(), vec![attempt]);
    let status = candidate.upgrade_status().unwrap();
    assert_eq!(status.stage, shared_types::UpgradeStage::Resumed);
    assert!(status.message.contains("1 member(s) resumed"), "{}", status.message);
    assert!(candidate.begin_upgrade_resume().is_some(), "a second trigger only reports the status");
    assert_eq!(args.lock().len(), 1);
    drop(candidate);

    // Consumed: an ordinary later start with the same variables replays nothing.
    let (candidate, host, args) = upgrade_candidate(&root, &record, identity);
    assert_eq!(candidate.upgrade_status(), None);
    assert_eq!(candidate.begin_upgrade_resume(), None);
    assert!(args.lock().is_empty());
    assert!(host.consumed.lock().is_empty());
    assert_eq!(upgrade_room_messages(&candidate, room).iter().filter(|(id, _)| *id == continuation_id).count(), 1);
    assert_eq!(candidate.upgrade_attempt_summary().unwrap().phase, "consumed");
}

#[test]
fn another_process_cannot_inherit_a_partial_resume_and_the_operator_may_abandon_it() {
    let f = upgrade_fixture();
    let (root, record, _source_host) = upgrade_through_close(f);
    let attempt = record.request.attempt_id;
    upgrade_helper_result(&record, upgrade::HelperOutcome::Launching, None);
    let (candidate, _host, _args) = upgrade_candidate(&root, &record, candidate_identity(&record, 4242));
    candidate.stop_upgrade_before_complete_for_tests();
    candidate.begin_upgrade_resume();
    drop(candidate);

    let (candidate, _host, args) = upgrade_candidate(&root, &record, candidate_identity(&record, 4343));
    let status = candidate.upgrade_status().unwrap();
    assert_eq!((status.stage, status.can_abandon), (shared_types::UpgradeStage::Held, true));
    assert!(status.message.contains("cannot continue it"), "{}", status.message);
    assert_eq!(candidate.begin_upgrade_resume().map(|status| status.stage), Some(shared_types::UpgradeStage::Held));
    assert!(args.lock().is_empty(), "nothing is replayed by a process that does not own the resume");
    candidate.abandon_held_upgrade(attempt).unwrap();
    assert_eq!(candidate.upgrade_status(), None);
    assert_eq!(candidate.upgrade_attempt_summary().unwrap().phase, "abandoned");
    assert!(candidate.abandon_held_upgrade(attempt).is_err(), "only a held attempt is abandoned");
}

#[test]
fn an_ordinary_restart_cancels_an_interrupted_notice_and_shows_an_unconfirmed_launch_as_held() {
    // A notice interrupted by a restart: this desktop owns the runtime now.
    let f = upgrade_fixture();
    let admission = upgrade_admitted(upgrade_request(&f));
    let UpgradeFixture { root, supervisor, .. } = f;
    drop(supervisor);
    let restarted = SupervisorHandle::new(test_supervisor_config(&root)).unwrap();
    let host = MockUpgradeHost::new(upgrade::UpgradeSource { pid: 777, creation_filetime: "134000000000000777".into(),
        exe_path: root.join("bin").join("prim1-experimental.exe"), exe_sha256: "a".repeat(64) });
    let environment = || upgrade_flow::UpgradeEnvironment { profile_dir: root.join("profile"),
        personal_repo_override: None, startup: Ok(None) };
    restarted.install_upgrade(environment(), host.clone()).unwrap();
    assert_eq!(restarted.upgrade_record_for_tests(admission.attempt_id).state.phase, upgrade::UpgradePhase::Cancelled);
    assert_eq!(restarted.upgrade_status(), None);
    drop(restarted);

    // Closed for an upgrade whose launch is not confirmed: held, never retried.
    let (root, record, _source_host) = upgrade_through_close(upgrade_fixture());
    let restarted = SupervisorHandle::new(test_supervisor_config(&root)).unwrap();
    restarted.install_upgrade(upgrade_flow::UpgradeEnvironment { profile_dir: root.join("profile"),
        personal_repo_override: None, startup: Ok(None) }, host).unwrap();
    let status = restarted.upgrade_status().unwrap();
    assert_eq!((status.stage, status.can_abandon, status.attempt_id), (shared_types::UpgradeStage::Held, true, record.request.attempt_id));
    assert!(status.message.contains("not confirmed"), "{}", status.message);
}

#[test]
fn a16_a_maintaining_member_reads_and_cancels_an_upgrade_under_its_own_name() {
    let f = upgrade_fixture();
    let admission = upgrade_admitted(upgrade_request(&f));
    let runtime = |request_id: Option<String>, action: shared_types::RuntimeAction| f.supervisor.apply_sideband_request(&f.caller,
        SidebandRequest::Runtime { request_id, action });

    let status = runtime(None, shared_types::RuntimeAction::UpgradeStatus {});
    let Some(SidebandResponsePayload::Runtime { result }) = status.payload else { panic!("{}", status.message) };
    let upgrade = result.upgrade.expect("an attempt in its notice");
    assert_eq!((upgrade.attempt_id, upgrade.stage, upgrade.can_cancel),
        (admission.attempt_id, shared_types::UpgradeStage::Notice, true));
    // Telemetry passes the grant; the sampler itself needs the desktop's app job.
    let telemetry = runtime(None, shared_types::RuntimeAction::Telemetry {});
    assert!(!telemetry.message.contains("Can upgrade PRIM-1"), "{}", telemetry.message);

    // Maintain withdrawn after admission, before the commit: nothing is cancelled.
    let revoker = f.supervisor.clone();
    let claude = f.claude;
    *f.supervisor.inner.sideband_after_initial_authorization.lock() = Some(Box::new(move || {
        revoker.set_member_permissions(claude, MemberPermissions::default()).unwrap();
    }));
    let before = upgrade_room_messages(&f.supervisor, f.room).len();
    let refused = runtime(Some(Uuid::new_v4().to_string()),
        shared_types::RuntimeAction::UpgradeCancel { attempt_id: admission.attempt_id });
    assert!(!refused.ok && refused.message.contains("Can upgrade PRIM-1"), "{}", refused.message);
    assert_eq!(f.supervisor.upgrade_record_for_tests(admission.attempt_id).state.phase, upgrade::UpgradePhase::Prepared);
    assert_eq!(upgrade_room_messages(&f.supervisor, f.room).len(), before, "no notice for a refused cancel");
    f.supervisor.set_member_permissions(f.claude, MemberPermissions { maintain: true, ..MemberPermissions::default() }).unwrap();

    let id = Uuid::new_v4().to_string();
    let cancelled = runtime(Some(id.clone()), shared_types::RuntimeAction::UpgradeCancel { attempt_id: admission.attempt_id });
    assert!(cancelled.ok, "{}", cancelled.message);
    assert_eq!(f.supervisor.upgrade_record_for_tests(admission.attempt_id).state.phase, upgrade::UpgradePhase::Cancelled);
    let notices = upgrade_room_messages(&f.supervisor, f.room);
    assert!(notices.iter().any(|(_, text)| text.contains("was cancelled: claude cancelled it")), "the member, never Victor: {notices:?}");
    let own = format!("claude cancelled upgrade {}; PRIM-1 keeps running on this build", admission.attempt_id);
    assert_eq!(upgrade_room_senders(&f.supervisor, f.room).into_iter().filter(|(sender, text)| *text == own
        && *sender == shared_types::RoomMessageSender::Session { session_id: f.claude }).count(), 1, "{notices:?}");
    assert_eq!(f.supervisor.upgrade_status(), None);
    // The same request_id replays the first result: no second effect or notice.
    let replay = runtime(Some(id), shared_types::RuntimeAction::UpgradeCancel { attempt_id: admission.attempt_id });
    let Some(SidebandResponsePayload::Runtime { result }) = replay.payload else { panic!("{}", replay.message) };
    assert!(result.replayed);
    assert_eq!(upgrade_room_messages(&f.supervisor, f.room).len(), notices.len());
    // An abandon of an attempt that is not held is refused, as the operator's.
    let abandon = runtime(Some(Uuid::new_v4().to_string()),
        shared_types::RuntimeAction::UpgradeAbandon { attempt_id: admission.attempt_id });
    assert!(!abandon.ok, "{}", abandon.message);

    // Without maintain every upgrade action is refused.
    f.supervisor.set_member_permissions(f.claude, MemberPermissions::default()).unwrap();
    for action in [shared_types::RuntimeAction::UpgradeStatus {}, shared_types::RuntimeAction::Telemetry {}] {
        let refused = runtime(None, action);
        assert!(!refused.ok && refused.message.contains("Can upgrade PRIM-1"), "{}", refused.message);
    }
}

#[test]
fn a16_a_maintaining_member_abandons_a_held_upgrade_under_its_own_name_with_the_grant_held_to_the_commit() {
    // As an_expired_close_stays_held_until_the_operator_abandons_it: the
    // attempt is held in the restarted PRIM-1, where a member acts.
    let f = upgrade_fixture();
    let admission = upgrade_admitted(upgrade_request(&f));
    upgrade_helper_ready(&f, admission.attempt_id);
    f.supervisor.inner.slots.lock().get_by_id_mut(f.claude).unwrap().last_pty_size = Some((99, 33));
    f.supervisor.inner.fail_next_catalog_write.store(true, Ordering::SeqCst);
    assert!(!f.supervisor.upgrade_close_step(admission.attempt_id));
    let record = f.supervisor.upgrade_record_for_tests(admission.attempt_id);
    let UpgradeFixture { root, supervisor, host, room, claude, .. } = f;
    drop(supervisor);
    upgrade_helper_result(&record, upgrade::HelperOutcome::Expired, None);
    let restarted = SupervisorHandle::new(test_supervisor_config(&root)).unwrap();
    restarted.install_upgrade(upgrade_flow::UpgradeEnvironment { profile_dir: root.join("profile"),
        personal_repo_override: None, startup: Ok(None) }, host).unwrap();
    assert_eq!(restarted.upgrade_status().map(|status| status.stage), Some(shared_types::UpgradeStage::Held));
    install_mock_running_session_with_bracketed_paste_enabled(&restarted, "claude", DriverKind::Claude,
        recording_pty_session(UPGRADE_SOURCE_PID + 1).0);
    let caller = restarted.resolve_pane_caller(TestPaneProcess::new(UPGRADE_SOURCE_PID + 10_001, [UPGRADE_SOURCE_PID + 1])).unwrap();
    let maintain = MemberPermissions { maintain: true, ..MemberPermissions::default() };
    restarted.set_member_permissions(claude, maintain).unwrap();
    let runtime = |request_id: String| restarted.apply_sideband_request(&caller,
        SidebandRequest::Runtime { request_id: Some(request_id),
            action: shared_types::RuntimeAction::UpgradeAbandon { attempt_id: admission.attempt_id } });

    // Maintain withdrawn after admission, before the commit: nothing is abandoned.
    let revoker = restarted.clone();
    *restarted.inner.sideband_after_initial_authorization.lock() = Some(Box::new(move || {
        revoker.set_member_permissions(claude, MemberPermissions::default()).unwrap();
    }));
    let refused = runtime(Uuid::new_v4().to_string());
    assert!(!refused.ok && refused.message.contains("Can upgrade PRIM-1"), "{}", refused.message);
    assert_eq!(restarted.upgrade_record_for_tests(admission.attempt_id).state.phase, upgrade::UpgradePhase::Closing);
    restarted.set_member_permissions(claude, maintain).unwrap();

    let id = Uuid::new_v4().to_string();
    let abandoned = runtime(id.clone());
    assert!(abandoned.ok, "{}", abandoned.message);
    assert_eq!(restarted.upgrade_record_for_tests(admission.attempt_id).state.phase, upgrade::UpgradePhase::Abandoned);
    assert_eq!(restarted.upgrade_status(), None);
    let own = format!("claude abandoned held upgrade {}; this does not show that its helper or new build stopped", admission.attempt_id);
    let own_notices = || upgrade_room_senders(&restarted, room).into_iter().filter(|(sender, text)| *text == own
        && *sender == shared_types::RoomMessageSender::Session { session_id: claude }).count();
    assert_eq!(own_notices(), 1);
    // The same request_id replays the first result: no second effect or notice.
    let replay = runtime(id);
    let Some(SidebandResponsePayload::Runtime { result }) = replay.payload else { panic!("{}", replay.message) };
    assert!(result.replayed);
    assert_eq!(own_notices(), 1);
    let audited = fs::read_to_string(restarted.audit_log_path()).unwrap_or_default();
    assert!(audited.contains("was abandoned by claude"), "the log names the member");
}
