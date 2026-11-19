// A108 register through its production call sites: the minted ID at launch
// and Start fresh, Resume here, and the Codex identify report. Included in
// the supervisor test module for its PTY fixtures. The register writes to the
// fixture's own workspace; no harness, Git repository or user folder is used.

/// Turns the register on for supervisors built on this thread until dropped.
struct RegisterOn;

impl RegisterOn {
    fn set() -> Self {
        session_ledger::set_personal_repo_env_for_tests(None);
        Self
    }
}

impl Drop for RegisterOn {
    fn drop(&mut self) {
        session_ledger::set_personal_repo_env_for_tests(Some(std::ffi::OsStr::new("off")));
    }
}

fn register_record(supervisor: &SupervisorHandle, id: SessionId) -> session_ledger::RegisterRecordV1 {
    let report = supervisor.flush_session_ledger(false, true, Duration::from_secs(60));
    assert!(report.is_some_and(|report| report.unwritten_members == 0), "the register writer did not write everything: {report:?}");
    let repo = supervisor.personal_repo_folder().expect("register on");
    session_ledger::read_register_file(&session_ledger::register_path(&repo, id)).unwrap().expect("register file")
}

fn register_restart(supervisor: SupervisorHandle, fixture: &Path) -> SupervisorHandle {
    supervisor.shutdown().unwrap();
    drop(supervisor);
    let supervisor = SupervisorHandle::new(test_supervisor_config(fixture)).unwrap();
    supervisor.set_executable_resolver_for_tests(Arc::new(TestExecutableResolver));
    supervisor
}

fn launch_with_mock_pty(supervisor: &SupervisorHandle, id: SessionId, fresh: bool) -> (Uuid, String) {
    let (pty, _) = recording_pty_session(std::process::id());
    let (spawner, _) = CapturingPtySpawner::new(vec![pty]);
    supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
    supervisor.start_session_by_id_with_options(id, fresh, false).unwrap();
    let slots = supervisor.inner.slots.lock();
    let slot = slots.get_by_id(id).unwrap();
    (slot.run_id.unwrap(), slot.harness_session_id.clone().unwrap())
}

#[test]
fn a108_launch_and_start_fresh_record_the_minted_conversation() {
    let _register = RegisterOn::set();
    let root = tempfile::tempdir().unwrap();
    let fixture = root.path().join("fixture");
    let supervisor = test_supervisor_with_root(fixture.clone());
    let id = test_session_id(&supervisor, "claude");

    let (run_id, minted) = launch_with_mock_pty(&supervisor, id, false);
    let launched = register_record(&supervisor, id);
    let current = launched.conversation.clone().unwrap();
    assert_eq!((current.id.as_str(), current.via.as_str()), (minted.as_str(), "minted"));
    let entry = &launched.history[0];
    assert_eq!((entry.reason.as_str(), entry.via.as_str(), entry.run_id), ("launch", "minted", Some(run_id)));
    assert_eq!((launched.holder.state.as_str(), launched.holder.run_id), ("running", Some(run_id)));

    // Something else rewrites the register while PRIM-1 is closed: the next
    // start holds the member.
    let path = session_ledger::register_path(&supervisor.personal_repo_folder().unwrap(), id);
    supervisor.shutdown().unwrap();
    drop(supervisor);
    let mut changed = session_ledger::read_register_file(&path).unwrap().unwrap();
    assert_eq!(changed.holder.state, "stopped");
    changed.conversation.as_mut().unwrap().id = "9e9e9e9e-0000-4000-8000-00000000009e".into();
    changed.revision += 1;
    fs::write(&path, serde_json::to_vec_pretty(&changed).unwrap()).unwrap();
    let supervisor = SupervisorHandle::new(test_supervisor_config(&fixture)).unwrap();
    supervisor.set_executable_resolver_for_tests(Arc::new(TestExecutableResolver));
    let hold = supervisor.inner.slots.lock().get_by_id(id).unwrap().recovery.clone().expect("held");
    assert_eq!(hold.provenance, session_ledger::REGISTER_DISAGREES);
    assert!(supervisor.start_session_by_id(id).unwrap_err().to_string().contains("Start fresh"));

    // Start fresh: a new minted conversation, the hold is gone, the register
    // agrees, and the next start holds nothing.
    let (fresh_run, fresh) = launch_with_mock_pty(&supervisor, id, true);
    assert_ne!(fresh, minted);
    assert_eq!(supervisor.inner.slots.lock().get_by_id(id).unwrap().recovery, None);
    let restarted = register_record(&supervisor, id);
    assert_eq!(restarted.conversation.as_ref().unwrap().id, fresh);
    let last = restarted.history.last().unwrap();
    assert_eq!((last.reason.as_str(), last.via.as_str(), last.run_id), ("start_fresh", "minted", Some(fresh_run)));
    let supervisor = register_restart(supervisor, &fixture);
    assert_eq!(supervisor.inner.slots.lock().get_by_id(id).unwrap().recovery, None);
}

#[test]
fn a108_resume_here_records_the_copied_conversation() {
    let _register = RegisterOn::set();
    let root = tempfile::tempdir().unwrap();
    let fixture = root.path().join("fixture");
    let supervisor = test_supervisor_with_root(fixture.clone());
    let id = test_session_id(&supervisor, "claude");
    supervisor.shutdown().unwrap();
    drop(supervisor);
    let copied = "8f8f8f8f-0000-4000-8000-00000000008f";
    let catalog_path = session_catalog_path(&fixture.join("runtime"));
    let mut catalog = read_session_catalog(&catalog_path).unwrap();
    let row = catalog.sessions.iter_mut().find(|row| row.session_id == id).unwrap();
    row.recovery = Some(RecoveryReference {
        original_id: Some(copied.into()),
        provenance: "room_import_in_use".into(),
        reason: "Copied from a running PRIM-1.".into(),
        reported_codex: None,
    });
    persist_session_catalog(&fixture.join("runtime"), &catalog).unwrap();

    let supervisor = SupervisorHandle::new(test_supervisor_config(&fixture)).unwrap();
    supervisor.resume_held_conversation(id).unwrap();
    let resumed = register_record(&supervisor, id);
    assert_eq!(resumed.conversation.as_ref().unwrap().id, copied);
    let last = resumed.history.last().unwrap();
    assert_eq!((last.reason.as_str(), last.via.as_str()), ("resume_here", "resume_here"));
    let supervisor = register_restart(supervisor, &fixture);
    assert_eq!(supervisor.inner.slots.lock().get_by_id(id).unwrap().recovery, None);
}

#[test]
fn a108_codex_identify_report_records_the_thread() {
    let _register = RegisterOn::set();
    let root = tempfile::tempdir().unwrap();
    let supervisor = test_supervisor_with_root(root.path().join("fixture"));
    let (pty, _) = recording_pty_session(102);
    install_mock_running_session_with_process_id(&supervisor, "codex", DriverKind::Codex, Some(102), pty);
    let caller = supervisor.resolve_pane_caller(TestPaneProcess::new(7001, [102])).unwrap();
    let cwd = supervisor.inner.slots.lock().get_by_id(caller.session_id).unwrap().definition.working_dir.clone();
    let sessions = root.path().join("sessions");
    let thread = Uuid::new_v4().to_string();
    session::write_codex_fixture(&sessions, &thread, &cwd);
    supervisor.identify_codex_from_pane(&caller, &thread, &sessions).unwrap();

    let reported = register_record(&supervisor, caller.session_id);
    let current = reported.conversation.clone().unwrap();
    assert_eq!((current.id.as_str(), current.via.as_str()), (thread.as_str(), "codex_identify"));
    let last = reported.history.last().unwrap();
    assert_eq!((last.reason.as_str(), last.via.as_str(), last.run_id), ("observed", "codex_identify", Some(caller.run_id)));
    let hint = reported.transcript_hint.expect("rollout hint");
    assert_eq!(hint.home, "~/.codex");
    assert_eq!(hint.relative, format!("sessions/2026/10/13/rollout-2026-10-13T12-00-00-{thread}.jsonl"));
}

/// A run is installed or withdrawn without any conversation, label or room
/// change (19 Oct real walk: Grok resumed at boot and again after a
/// self-upgrade, its register file still `stopped`): the holder follows it.
#[test]
fn a108_holder_follows_runs_that_change_no_conversation() {
    let _register = RegisterOn::set();
    let root = tempfile::tempdir().unwrap();
    let fixture = root.path().join("fixture");
    let supervisor = test_supervisor_with_root(fixture.clone());
    let id = create_test_session(&supervisor, "grok", DriverKind::Grok, shared_types::PermissionProfile::Normal).session_id;
    let (_, conversation) = launch_with_mock_pty(&supervisor, id, false);

    // A forced kill or an upgrade: the catalog keeps running_at_shutdown, and
    // the next start's first pass finds nothing running.
    let supervisor = register_restart(supervisor, &fixture);
    let booted = register_record(&supervisor, id);
    assert_eq!((booted.holder.state.as_str(), booted.holder.run_id), ("stopped", None));
    let history = booted.history.len();

    let process_id = std::process::id();
    let ptys = (0..3).map(|_| recording_pty_session(process_id).0).collect();
    let (spawner, _) = CapturingPtySpawner::new(ptys);
    supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
    let run = |supervisor: &SupervisorHandle| {
        let slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id(id).unwrap();
        assert_eq!(slot.harness_session_id.as_deref(), Some(conversation.as_str()), "the conversation must not change");
        slot.run_id.expect("running")
    };
    let assert_running = |record: &session_ledger::RegisterRecordV1, run_id: Uuid| {
        assert_eq!((record.holder.state.as_str(), record.holder.run_id, record.holder.pid), ("running", Some(run_id), Some(process_id)));
        assert!(record.holder.proc_start.is_some(), "the holder names its process start: {:?}", record.holder);
        assert_eq!(record.conversation.as_ref().unwrap().id, conversation);
        assert_eq!(record.history.len(), history, "no conversation change is invented");
    };

    // Resume: the same conversation, nothing in the catalog row changes.
    supervisor.start_session_by_id(id).unwrap();
    let resumed = run(&supervisor);
    assert_running(&register_record(&supervisor, id), resumed);

    // Restart: a new run of the same conversation.
    supervisor.restart_session_by_id(id).unwrap();
    let restarted = run(&supervisor);
    assert_ne!(restarted, resumed);
    assert_running(&register_record(&supervisor, id), restarted);

    // Stop.
    supervisor.stop_session_by_id(id).unwrap();
    let stopped = register_record(&supervisor, id);
    assert_eq!(
        (stopped.holder.state.as_str(), stopped.holder.run_id, stopped.holder.pid, stopped.holder.proc_start.as_deref()),
        ("stopped", None, None, None)
    );

    // Started again: the file shows the new run.
    supervisor.start_session_by_id(id).unwrap();
    let again = run(&supervisor);
    assert!(again != resumed && again != restarted);
    assert_running(&register_record(&supervisor, id), again);
}

/// A run that ends by itself (the process exits) stops the holder, also when
/// nothing in the catalog row changes.
#[test]
fn a108_holder_stops_when_the_run_exits() {
    let _register = RegisterOn::set();
    let root = tempfile::tempdir().unwrap();
    let supervisor = test_supervisor_with_root(root.path().join("fixture"));
    let id = test_session_id(&supervisor, "claude");
    let (run_id, _) = launch_with_mock_pty(&supervisor, id, false);
    let running = register_record(&supervisor, id);
    assert_eq!((running.holder.state.as_str(), running.holder.run_id), ("running", Some(run_id)));

    // Exit with running_at_shutdown already false: only the run changes.
    supervisor.inner.slots.lock().get_by_id_mut(id).unwrap().running_at_shutdown = false;
    supervisor.persist_slot_harness_state(id);
    assert_eq!(register_record(&supervisor, id).holder.state, "running");
    let generation = supervisor.inner.slots.lock().get_by_id(id).unwrap().generation;
    supervisor.handle_pty_event_by_id(id, generation, run_id, PtyEvent::Closed);
    assert!(supervisor.inner.slots.lock().get_by_id(id).unwrap().running.is_none(), "the run was retired");
    let exited = register_record(&supervisor, id);
    assert_eq!((exited.holder.state.as_str(), exited.holder.run_id, exited.holder.pid), ("stopped", None, None));
}

/// A stop that cannot prove the process ended keeps the holder's identity and
/// says "stopping", never "stopped": the process may still run (Codex room 343).
#[test]
fn a108_holder_never_claims_stopped_while_termination_is_unproven() {
    let _register = RegisterOn::set();
    let root = tempfile::tempdir().unwrap();
    let supervisor = test_supervisor_with_root(root.path().join("fixture"));
    let id = create_test_session(&supervisor, "uncertain", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let (pty, _, _) = mock_pty_session_with_exit_status(
        Some(u32::MAX),
        Some(pty_exit_status(0, None, true)),
        MockKillBehavior::Error("injected Job termination failure"),
    );
    install_mock_running_session_by_id_with_mode(&supervisor, id, DriverKind::Claude, Some(u32::MAX), pty, BracketedPasteMode::Unknown);
    let run_id = supervisor.inner.slots.lock().get_by_id(id).unwrap().run_id.expect("installed run");
    supervisor.note_run_changed(id);
    let running = register_record(&supervisor, id);
    assert_eq!((running.holder.state.as_str(), running.holder.run_id), ("running", Some(run_id)));

    let stopped = supervisor.stop_session_by_id(id).expect("an uncertain stop still returns its Failed snapshot");
    assert!(stopped.running, "the exact process-scope owner is retained");
    let record = register_record(&supervisor, id);
    assert_eq!(
        (record.holder.state.as_str(), record.holder.run_id, record.holder.pid),
        ("stopping", Some(run_id), running.holder.pid),
        "an unproven stop keeps the holder's identity: {:?}",
        record.holder
    );
}

/// While a stop waits for termination the holder says "stopping"; once the
/// release is proved it says "stopped" (Codex room 350).
#[test]
fn a108_holder_goes_stopping_then_stopped_when_release_is_proved() {
    let _register = RegisterOn::set();
    let root = tempfile::tempdir().unwrap();
    let supervisor = test_supervisor_with_root(root.path().join("fixture"));
    let id = create_test_session(&supervisor, "slow-stop", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let (pty, _, _) = mock_pty_session_with_exit_status(
        Some(u32::MAX),
        Some(pty_exit_status(0, None, true)),
        MockKillBehavior::Sleep(Duration::from_secs(3)),
    );
    install_mock_running_session_by_id_with_mode(&supervisor, id, DriverKind::Claude, Some(u32::MAX), pty, BracketedPasteMode::Unknown);
    supervisor.set_stop_kill_timeout_for_tests(Duration::from_secs(20));
    let run_id = supervisor.inner.slots.lock().get_by_id(id).unwrap().run_id.expect("installed run");
    supervisor.note_run_changed(id);
    assert_eq!(register_record(&supervisor, id).holder.state, "running");

    let stopper = {
        let supervisor = supervisor.clone();
        std::thread::spawn(move || supervisor.stop_session_by_id(id))
    };
    let deadline = Instant::now() + Duration::from_millis(2500);
    let mut seen = None;
    while Instant::now() < deadline {
        let record = register_record(&supervisor, id);
        if record.holder.state == "stopping" {
            seen = Some(record);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let stopping = seen.expect("the holder says stopping while termination is being waited for");
    assert_eq!(stopping.holder.run_id, Some(run_id), "the prior identity is kept while stopping");

    let stopped = stopper.join().unwrap().expect("the stop completes once the kill returns");
    assert!(!stopped.running, "termination was proved");
    let record = register_record(&supervisor, id);
    assert_eq!((record.holder.state.as_str(), record.holder.run_id, record.holder.pid), ("stopped", None, None));
}
