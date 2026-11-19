// Included in the supervisor test module to reuse its isolated PTY fixtures.
// A108 W2: every harness home is a temp fixture; no harness runs, no real
// ~/.claude, ~/.codex or ~/.grok is read, and processes come from a fake probe.

/// Process starts by pid, and codex.exe children by parent pid; shared so a
/// test can put another process behind a pid.
#[derive(Clone, Default)]
struct W2Probe(Arc<Mutex<HashMap<u32, u64>>>, Arc<Mutex<HashMap<u32, Vec<(u32, u64)>>>>);

impl W2Probe {
    fn of(starts: impl IntoIterator<Item = (u32, u64)>) -> Self {
        Self(Arc::new(Mutex::new(starts.into_iter().collect())), Arc::default())
    }
}

impl identity_tracker::ProcessProbe for W2Probe {
    fn harness_start(&self, _pty: &dyn PtySession, pid: u32, _driver: DriverKind) -> Result<u64> {
        self.0.lock().get(&pid).copied().ok_or_else(|| anyhow!("process {pid} is outside its pane's Job"))
    }

    fn codex_children(&self, _pty: &dyn PtySession, pid: u32, _started: u64) -> Vec<(u32, u64)> {
        self.1.lock().get(&pid).cloned().unwrap_or_default()
    }
}

/// Lock holders by thread, counting every query.
#[derive(Clone, Default)]
struct W2Holders(Arc<Mutex<HashMap<String, Vec<(u32, u64)>>>>, Arc<AtomicUsize>);

impl W2Holders {
    fn asked(&self) -> usize {
        self.1.swap(0, Ordering::SeqCst)
    }
}

impl identity_tracker::LockHolders for W2Holders {
    fn holders(&self, path: &Path) -> Result<Vec<(u32, u64)>> {
        self.1.fetch_add(1, Ordering::SeqCst);
        let name = path.file_name().unwrap().to_str().unwrap().trim_end_matches(".lock").to_owned();
        Ok(self.0.lock().get(&name).cloned().unwrap_or_default())
    }
}

/// A process start (FILETIME) one minute ago.
fn w2_start() -> u64 {
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap() - Duration::from_secs(60);
    116_444_736_000_000_000 + (unix.as_nanos() / 100) as u64
}

fn w2_homes(root: &Path) -> identity_tracker::HarnessHomes {
    identity_tracker::HarnessHomes {
        claude: Some(root.join("claude")),
        codex: Some(root.join("codex")),
        grok: Some(root.join("grok")),
        pid_domain: "win32:testpc".into(),
    }
}

fn w2_write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn w2_slot<T>(supervisor: &SupervisorHandle, id: SessionId, read: impl FnOnce(&mut SessionSlot) -> T) -> T {
    read(supervisor.inner.slots.lock().get_by_id_mut(id).unwrap())
}

fn w2_catalog_row(supervisor: &SupervisorHandle, id: SessionId) -> PersistedSessionV1 {
    read_session_catalog(&session_catalog_path(supervisor.runtime_dir())).unwrap()
        .sessions.into_iter().find(|row| row.session_id == id).unwrap()
}

fn w2_running(supervisor: &SupervisorHandle, label: &str, driver: DriverKind, pid: u32) -> (SessionId, Uuid, SessionGeneration) {
    let (pty, _) = recording_pty_session(pid);
    install_mock_running_session_with_process_id(supervisor, label, driver, Some(pid), pty);
    let id = test_session_id(supervisor, label);
    w2_slot(supervisor, id, |slot| (id, slot.run_id.unwrap(), slot.generation))
}

#[test]
fn w2_apply_tracked_conversation_revalidates_the_run_and_persists_before_the_slot() {
    let supervisor = test_supervisor();
    let events = capture_runtime_events(&supervisor);
    let (id, run, generation) = w2_running(&supervisor, "claude", DriverKind::Claude, 101);
    let first = Uuid::new_v4().to_string();
    assert!(supervisor.apply_tracked_conversation(id, run, generation, &first, "clear", "pid_file", 101, None).unwrap());
    assert_eq!(w2_slot(&supervisor, id, |slot| slot.harness_session_id.clone()), Some(first.clone()));
    assert_eq!(w2_catalog_row(&supervisor, id).harness_session_id, Some(first.clone()));
    assert!(!supervisor.apply_tracked_conversation(id, run, generation, &first.to_uppercase(), "clear", "pid_file", 101, None).unwrap());
    // A stale run, generation or process is never bound.
    let second = Uuid::new_v4().to_string();
    for (run, generation, pid) in [(Uuid::new_v4(), generation, 101), (run, generation + 1, 101), (run, generation, 999)] {
        let error = supervisor.apply_tracked_conversation(id, run, generation, &second, "clear", "pid_file", pid, None).unwrap_err();
        assert!(error.is::<identity_tracker::TrackedRunGone>());
    }
    assert!(supervisor.apply_tracked_conversation(id, run, generation, "../escape", "clear", "pid_file", 101, None).is_err());
    // A failed catalog write changes nothing in memory.
    supervisor.fail_next_catalog_write_for_tests();
    assert!(supervisor.apply_tracked_conversation(id, run, generation, &second, "clear", "pid_file", 101, None).is_err());
    assert_eq!(w2_slot(&supervisor, id, |slot| slot.harness_session_id.clone()), Some(first.clone()));
    // A rename is a title: same ID only, never a change.
    assert!(supervisor.apply_tracked_conversation(id, run, generation, &first, "rename", "pid_file", 101, None).unwrap());
    assert!(!supervisor.apply_tracked_conversation(id, run, generation, &second, "rename", "pid_file", 101, None).unwrap());
    assert_eq!(w2_slot(&supervisor, id, |slot| slot.harness_session_id.clone()), Some(first.clone()));
    // Another member already using the conversation: never bound twice.
    create_test_session(&supervisor, "claude-two", DriverKind::Claude, shared_types::PermissionProfile::Normal);
    let (other, other_run, other_generation) = w2_running(&supervisor, "claude-two", DriverKind::Claude, 103);
    let error = supervisor.apply_tracked_conversation(other, other_run, other_generation, &first, "resume", "pid_file", 103, None).unwrap_err();
    assert!(error.to_string().contains("already uses this conversation"), "{error:#}");
    assert_eq!(w2_slot(&supervisor, other, |slot| slot.harness_session_id.clone()), None);
    let logs: Vec<String> = events.lock().iter().filter_map(|event| match event {
        RuntimeEvent::SystemLog { message, .. } if message.contains("conversation") => Some(message.clone()),
        _ => None,
    }).collect();
    assert!(logs.iter().any(|message| message.contains("followed (clear, proved by pid_file)")), "{logs:?}");
    assert!(logs.iter().any(|message| message.contains("renamed")), "{logs:?}");
    let audit = fs::read_to_string(supervisor.audit_log_path()).unwrap();
    assert!(!audit.contains(&first), "conversation IDs stay out of the audit log");
}

#[test]
fn w2_apply_releases_only_codex_launch_and_report_holds() {
    let supervisor = test_supervisor();
    let (id, run, generation) = w2_running(&supervisor, "codex", DriverKind::Codex, 102);
    let thread = Uuid::new_v4().to_string();
    let launch_hold = RecoveryReference::unresolved_codex(None, false);
    w2_slot(&supervisor, id, |slot| slot.recovery = Some(launch_hold.clone()));
    assert!(supervisor.apply_tracked_conversation(id, run, generation, &thread, "launch", "writer_lock", 102, None).unwrap());
    let (current, recovery, history) = w2_slot(&supervisor, id, |slot|
        (slot.harness_session_id.clone(), slot.recovery.clone(), slot.recovery_history.clone()));
    assert_eq!((current, recovery), (Some(thread.clone()), None));
    assert!(history.is_empty(), "a plain launch hold is superseded by the proof, not archived: {history:?}");
    let row = w2_catalog_row(&supervisor, id);
    assert_eq!((row.harness_session_id, row.recovery), (Some(thread.clone()), None));
    assert!(supervisor.snapshot().sessions.iter().any(|session| session.session_id == id && session.resume_available));
    // The same proof on an unchanged ID still releases a new launch hold.
    w2_slot(&supervisor, id, |slot| slot.recovery = Some(launch_hold.clone()));
    assert!(supervisor.apply_tracked_conversation(id, run, generation, &thread, "launch", "writer_lock", 102, None).unwrap());
    assert_eq!(w2_slot(&supervisor, id, |slot| slot.recovery.clone()), None);
    // This run's identify report: the proof must have been read with it.
    let reported = Uuid::new_v4().to_string();
    let report_hold = RecoveryReference {
        original_id: Some(thread.clone()), provenance: "job_attested_codex_shell_report".into(), reason: "reported".into(),
        reported_codex: Some(session::CodexIdentityEvidence { thread_id: reported.clone(), run_id: run,
            generation, rollout_path: PathBuf::from("rollout.jsonl"), reported_at: now_rfc3339() }),
    };
    w2_slot(&supervisor, id, |slot| { slot.recovery = Some(report_hold.clone()); slot.harness_session_id = Some(reported.clone()); });
    // Read before the report landed: retried, nothing released or overwritten.
    let error = supervisor.apply_tracked_conversation(id, run, generation, &thread, "new", "writer_lock", 102, None).unwrap_err();
    assert!(error.is::<identity_tracker::TrackedRunGone>(), "{error:#}");
    let error = supervisor.apply_tracked_conversation(id, run, generation, &thread, "new", "codex_identify", 102, Some(&reported)).unwrap_err();
    assert!(error.to_string().contains("reported thread"), "{error:#}");
    assert_eq!(w2_slot(&supervisor, id, |slot| (slot.harness_session_id.clone(), slot.recovery.clone())),
        (Some(reported.clone()), Some(report_hold.clone())));
    // Read with it: released, and the report (evidence) is archived.
    assert!(supervisor.apply_tracked_conversation(id, run, generation, &reported.to_uppercase(), "new", "codex_identify", 102,
        Some(&reported.to_uppercase())).unwrap());
    assert_eq!(w2_slot(&supervisor, id, |slot| (slot.recovery.clone(), slot.recovery_history.contains(&report_hold))), (None, true));
    // Import, duplicate and refusal holds are never touched.
    for provenance in ["room_import_in_use", "duplicate_catalog_identity", "harness_resume_refusal", "legacy_catalog"] {
        let hold = RecoveryReference { original_id: Some(thread.clone()), provenance: provenance.into(),
            reason: "held".into(), reported_codex: None };
        w2_slot(&supervisor, id, |slot| slot.recovery = Some(hold.clone()));
        let next = Uuid::new_v4().to_string();
        assert!(supervisor.apply_tracked_conversation(id, run, generation, &next, "new", "writer_lock", 102, None).is_err(), "{provenance}");
        assert_eq!(w2_slot(&supervisor, id, |slot| (slot.harness_session_id.clone(), slot.recovery.clone())),
            (Some(reported.clone()), Some(hold)));
    }
    // A Claude proof never releases a Codex-style hold.
    let (claude, claude_run, claude_generation) = w2_running(&supervisor, "claude", DriverKind::Claude, 101);
    w2_slot(&supervisor, claude, |slot| slot.recovery = Some(launch_hold.clone()));
    assert!(supervisor.apply_tracked_conversation(claude, claude_run, claude_generation,
        &Uuid::new_v4().to_string(), "clear", "pid_file", 101, None).is_err());
}

#[test]
fn w2_poll_follows_a_claude_clear_from_its_pid_file() {
    let supervisor = test_supervisor();
    let root = tempfile::tempdir().unwrap();
    let start = w2_start();
    let (id, _, _) = w2_running(&supervisor, "claude", DriverKind::Claude, 101);
    let minted = Uuid::new_v4().to_string();
    let cwd = w2_slot(&supervisor, id, |slot| {
        slot.harness_session_id = Some(minted.clone());
        slot.definition.working_dir.clone()
    });
    let claude = root.path().join("claude");
    let pid_file = |session_id: &str, start: u64| w2_write(&claude.join("sessions").join("101.json"), &serde_json::json!({
        "pid": 101, "sessionId": session_id, "cwd": cwd, "procStart": start.to_string(),
        "kind": "interactive", "pidDomain": "win32:testpc", "name": "session-x"}).to_string());
    let mut tracker = identity_tracker::IdentityTracker::new(w2_homes(root.path()),
        Box::new(W2Holders::default()), Box::new(W2Probe::of([(101, start)])));
    tracker.poll(&supervisor);
    pid_file(&minted, start);
    tracker.poll(&supervisor);
    let cleared = Uuid::new_v4().to_string();
    pid_file(&cleared, start);
    let projects = claude.join("projects").join(
        cwd.chars().map(|character| if character.is_ascii_alphanumeric() { character } else { '-' }).collect::<String>());
    tracker.poll(&supervisor);
    assert_eq!(w2_slot(&supervisor, id, |slot| slot.harness_session_id.clone()), Some(minted.clone()),
        "pending until the transcript exists");
    w2_write(&projects.join(format!("{cleared}.jsonl")), "{}\n");
    tracker.poll(&supervisor);
    assert_eq!(w2_slot(&supervisor, id, |slot| slot.harness_session_id.clone()), Some(cleared.clone()));
    assert_eq!(w2_catalog_row(&supervisor, id).harness_session_id, Some(cleared.clone()));
    // PID reuse: the file of an earlier process with this pid is ignored.
    let stale = Uuid::new_v4().to_string();
    w2_write(&projects.join(format!("{stale}.jsonl")), "{}\n");
    pid_file(&stale, start - 1);
    tracker.poll(&supervisor);
    assert_eq!(w2_slot(&supervisor, id, |slot| slot.harness_session_id.clone()), Some(cleared));
}

#[test]
fn w2_poll_releases_the_codex_launch_hold_and_holds_resume_while_two_roots_are_held() {
    let supervisor = test_supervisor();
    let events = capture_runtime_events(&supervisor);
    let root = tempfile::tempdir().unwrap();
    let start = w2_start();
    let (id, _, _) = w2_running(&supervisor, "codex", DriverKind::Codex, 102);
    let cwd = w2_slot(&supervisor, id, |slot| {
        slot.recovery = Some(RecoveryReference::unresolved_codex(None, false));
        slot.definition.working_dir.clone()
    });
    let codex = root.path().join("codex");
    let holders = W2Holders::default();
    let open = |thread: &str| {
        w2_write(&codex.join("thread-writer-locks").join(format!("{thread}.lock")), "");
        session::write_codex_fixture(&codex.join("sessions"), thread, &cwd);
        holders.0.lock().insert(thread.into(), vec![(102, start)]);
    };
    let mut tracker = identity_tracker::IdentityTracker::new(w2_homes(root.path()),
        Box::new(holders.clone()), Box::new(W2Probe::of([(102, start)])));
    let first = Uuid::new_v4().to_string();
    open(&first);
    tracker.poll(&supervisor);
    assert_eq!(w2_slot(&supervisor, id, |slot| (slot.harness_session_id.clone(), slot.recovery.is_none())),
        (Some(first.clone()), true));
    assert_eq!(holders.asked(), 1);
    // /new: the old thread stays loaded; ambiguous, logged once, the ID
    // unchanged, and Resume held so neither is resumed as exact (a failed
    // catalog write is retried next pass). Only the new lock is asked; an
    // unchanged lock folder asks nothing.
    let second = Uuid::new_v4().to_string();
    open(&second);
    supervisor.fail_next_catalog_write_for_tests();
    tracker.poll(&supervisor);
    assert_eq!(holders.asked(), 1);
    assert_eq!(w2_slot(&supervisor, id, |slot| slot.recovery.clone()), None, "a failed write changes nothing");
    tracker.poll(&supervisor);
    assert_eq!(holders.asked(), 0);
    let ambiguous = events.lock().iter().filter(|event| matches!(event,
        RuntimeEvent::SystemLog { message, .. } if message.contains("ambiguous and Resume is held"))).count();
    assert_eq!(ambiguous, 1);
    assert_eq!(w2_slot(&supervisor, id, |slot| slot.harness_session_id.clone()), Some(first.clone()));
    let hold = w2_slot(&supervisor, id, |slot| slot.recovery.clone()).expect("held");
    assert_eq!((hold.provenance.as_str(), hold.original_id.as_deref()), (identity_tracker::CODEX_AMBIGUOUS, Some(first.as_str())));
    assert_eq!(w2_catalog_row(&supervisor, id).recovery, Some(hold));
    assert!(supervisor.snapshot().sessions.iter().any(|session| session.session_id == id && !session.resume_available));
    let definition = w2_slot(&supervisor, id, |slot| slot.definition.clone());
    assert!(supervisor.resolve_harness_launch_session(&definition, false).is_err(), "Launch must not resume a possibly stale ID");
    tracker.poll(&supervisor);
    assert_eq!(w2_slot(&supervisor, id, |slot| slot.recovery.as_ref().map(|hold| hold.provenance.clone())),
        Some(identity_tracker::CODEX_AMBIGUOUS.to_owned()));
    // The old thread is released (Codex deletes its lock): the remaining root
    // is exact.
    holders.0.lock().remove(&first);
    fs::remove_file(codex.join("thread-writer-locks").join(format!("{first}.lock"))).unwrap();
    tracker.poll(&supervisor);
    assert_eq!(w2_slot(&supervisor, id, |slot| (slot.harness_session_id.clone(), slot.recovery.clone())), (Some(second.clone()), None));
    assert!(w2_slot(&supervisor, id, |slot| slot.recovery_history.iter().all(|hold| hold.provenance != identity_tracker::CODEX_AMBIGUOUS)),
        "the ambiguity hold is superseded by the proof, not archived");
    let row = w2_catalog_row(&supervisor, id);
    assert_eq!((row.harness_session_id, row.recovery), (Some(second.clone()), None));
    assert_eq!(supervisor.resolve_harness_launch_session(&definition, false).unwrap(),
        shared_types::HarnessLaunchSession::Resume { session_id: second });
    assert_eq!(holders.asked(), 0);
}

#[test]
fn w2_poll_releases_the_codex_ambiguity_hold_on_this_runs_identify_report() {
    let supervisor = test_supervisor();
    let root = tempfile::tempdir().unwrap();
    let start = w2_start();
    let (id, _, _) = w2_running(&supervisor, "codex", DriverKind::Codex, 102);
    let cwd = w2_slot(&supervisor, id, |slot| {
        slot.recovery = Some(RecoveryReference::unresolved_codex(None, false));
        slot.definition.working_dir.clone()
    });
    let codex = root.path().join("codex");
    let sessions = codex.join("sessions");
    let holders = W2Holders::default();
    let open = |thread: &str| {
        w2_write(&codex.join("thread-writer-locks").join(format!("{thread}.lock")), "");
        session::write_codex_fixture(&sessions, thread, &cwd);
        holders.0.lock().insert(thread.into(), vec![(102, start)]);
    };
    let mut tracker = identity_tracker::IdentityTracker::new(w2_homes(root.path()),
        Box::new(holders.clone()), Box::new(W2Probe::of([(102, start)])));
    let (a, b) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
    open(&a);
    tracker.poll(&supervisor);
    open(&b);
    tracker.poll(&supervisor);
    assert_eq!(w2_slot(&supervisor, id, |slot| (slot.harness_session_id.clone(),
        slot.recovery.as_ref().map(|hold| hold.provenance.clone()))), (Some(a.clone()), Some(identity_tracker::CODEX_AMBIGUOUS.to_owned())));
    // The agent runs --prim1-identify for B, made (as the tracker requires)
    // more than 2 s after it first saw A held.
    let caller = supervisor.resolve_pane_caller(TestPaneProcess::new(7001, [102])).unwrap();
    supervisor.identify_codex_from_pane(&caller, &b, &sessions).unwrap();
    w2_slot(&supervisor, id, |slot| slot.recovery.as_mut().unwrap().reported_codex.as_mut().unwrap().reported_at =
        (chrono::Utc::now() + chrono::Duration::seconds(5)).to_rfc3339());
    tracker.poll(&supervisor);
    assert_eq!(w2_slot(&supervisor, id, |slot| (slot.harness_session_id.clone(), slot.recovery.clone())), (Some(b.clone()), None));
    let row = w2_catalog_row(&supervisor, id);
    assert_eq!((row.harness_session_id, row.recovery), (Some(b.clone()), None));
    let definition = w2_slot(&supervisor, id, |slot| slot.definition.clone());
    assert_eq!(supervisor.resolve_harness_launch_session(&definition, false).unwrap(),
        shared_types::HarnessLaunchSession::Resume { session_id: b });
}

/// Through the npm shim the member's process is node running codex.js; its
/// vendored codex.exe child is the one holding the writer lock.
#[test]
fn w2_poll_follows_codex_whose_lock_is_held_by_the_members_own_child() {
    let supervisor = test_supervisor();
    let root = tempfile::tempdir().unwrap();
    let start = w2_start();
    let (id, _, _) = w2_running(&supervisor, "codex", DriverKind::Codex, 102);
    let cwd = w2_slot(&supervisor, id, |slot| {
        slot.recovery = Some(RecoveryReference::unresolved_codex(None, false));
        slot.definition.working_dir.clone()
    });
    let codex = root.path().join("codex");
    let thread = Uuid::new_v4().to_string();
    w2_write(&codex.join("thread-writer-locks").join(format!("{thread}.lock")), "");
    session::write_codex_fixture(&codex.join("sessions"), &thread, &cwd);
    let child = (202, start + 5_000_000);
    let holders = W2Holders::default();
    holders.0.lock().insert(thread.clone(), vec![child]);
    let probe = W2Probe::of([(102, start)]);
    let mut tracker = identity_tracker::IdentityTracker::new(w2_homes(root.path()),
        Box::new(holders), Box::new(probe.clone()));
    // A holder that is neither the member's process nor its child: not its.
    tracker.poll(&supervisor);
    assert_eq!(w2_slot(&supervisor, id, |slot| slot.harness_session_id.clone()), None);
    probe.1.lock().insert(102, vec![child]);
    tracker.poll(&supervisor);
    assert_eq!(w2_slot(&supervisor, id, |slot| (slot.harness_session_id.clone(), slot.recovery.is_none())),
        (Some(thread.clone()), true));
    assert_eq!(w2_catalog_row(&supervisor, id).harness_session_id, Some(thread));
}

/// The review sequence: A proven; /new opens B (held, not written yet); B's
/// first line is written; the agent identifies B. The lone root A must never
/// overwrite that report or make either conversation resumable as exact.
#[test]
fn w2_poll_never_binds_the_lone_root_over_this_runs_identify_report() {
    let supervisor = test_supervisor();
    let root = tempfile::tempdir().unwrap();
    let start = w2_start();
    let (id, _, _) = w2_running(&supervisor, "codex", DriverKind::Codex, 102);
    let cwd = w2_slot(&supervisor, id, |slot| {
        slot.recovery = Some(RecoveryReference::unresolved_codex(None, false));
        slot.definition.working_dir.clone()
    });
    let codex = root.path().join("codex");
    let sessions = codex.join("sessions");
    let holders = W2Holders::default();
    let lock = |thread: &str| {
        w2_write(&codex.join("thread-writer-locks").join(format!("{thread}.lock")), "");
        holders.0.lock().insert(thread.into(), vec![(102, start)]);
    };
    let mut tracker = identity_tracker::IdentityTracker::new(w2_homes(root.path()),
        Box::new(holders.clone()), Box::new(W2Probe::of([(102, start)])));
    let (a, b) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
    lock(&a);
    session::write_codex_fixture(&sessions, &a, &cwd);
    tracker.poll(&supervisor);
    assert_eq!(w2_slot(&supervisor, id, |slot| (slot.harness_session_id.clone(), slot.recovery.is_none())),
        (Some(a.clone()), true));
    lock(&b);
    tracker.poll(&supervisor);
    // B's rollout appears, its first line not yet written; then complete.
    let rollout = session::write_codex_fixture(&sessions, &b, &cwd);
    let header = fs::read(&rollout).unwrap();
    fs::write(&rollout, "").unwrap();
    tracker.poll(&supervisor);
    fs::write(&rollout, header).unwrap();
    let caller = supervisor.resolve_pane_caller(TestPaneProcess::new(7001, [102])).unwrap();
    supervisor.identify_codex_from_pane(&caller, &b, &sessions).unwrap();
    let report_hold = w2_slot(&supervisor, id, |slot| slot.recovery.clone()).unwrap();
    // The report was made within 2 s of A first being seen, so it does not
    // prove B is in front: two roots, so Resume is held, the report kept.
    for _ in 0..3 {
        tracker.poll(&supervisor);
        let (current, hold, history) = w2_slot(&supervisor, id, |slot|
            (slot.harness_session_id.clone(), slot.recovery.clone(), slot.recovery_history.clone()));
        assert_eq!(current, Some(b.clone()), "A must not overwrite the report");
        let hold = hold.expect("held");
        assert_eq!((hold.provenance.as_str(), hold.original_id.as_deref()), (identity_tracker::CODEX_AMBIGUOUS, Some(b.as_str())));
        assert!(history.contains(&report_hold));
    }
    // A is released: B, reported and held, is exact; Launch resumes it.
    holders.0.lock().remove(&a);
    fs::remove_file(codex.join("thread-writer-locks").join(format!("{a}.lock"))).unwrap();
    tracker.poll(&supervisor);
    let (current, recovery, history) = w2_slot(&supervisor, id, |slot|
        (slot.harness_session_id.clone(), slot.recovery.clone(), slot.recovery_history.clone()));
    assert_eq!((current, recovery), (Some(b.clone()), None));
    assert!(history.contains(&report_hold), "the report's evidence is kept");
    assert_eq!(w2_catalog_row(&supervisor, id).harness_session_id, Some(b));
}

#[test]
fn w2_poll_refuses_a_process_that_is_not_the_one_launched() {
    let supervisor = test_supervisor();
    let events = capture_runtime_events(&supervisor);
    let root = tempfile::tempdir().unwrap();
    let start = w2_start();
    let (id, _, _) = w2_running(&supervisor, "claude", DriverKind::Claude, 101);
    let minted = Uuid::new_v4().to_string();
    let cwd = w2_slot(&supervisor, id, |slot| {
        slot.harness_session_id = Some(minted.clone());
        slot.definition.working_dir.clone()
    });
    let claude = root.path().join("claude");
    let projects = claude.join("projects").join(
        cwd.chars().map(|character| if character.is_ascii_alphanumeric() { character } else { '-' }).collect::<String>());
    let follow = |start: u64| {
        let next = Uuid::new_v4().to_string();
        w2_write(&projects.join(format!("{next}.jsonl")), "{}\n");
        w2_write(&claude.join("sessions").join("101.json"), &serde_json::json!({
            "pid": 101, "sessionId": next, "cwd": cwd, "procStart": start.to_string(),
            "kind": "interactive", "pidDomain": "win32:testpc"}).to_string());
        next
    };
    let probe = W2Probe::of([(101, start)]);
    let mut tracker = identity_tracker::IdentityTracker::new(w2_homes(root.path()),
        Box::new(W2Holders::default()), Box::new(probe.clone()));
    let followed = follow(start);
    tracker.poll(&supervisor);
    assert_eq!(w2_slot(&supervisor, id, |slot| slot.harness_session_id.clone()), Some(followed.clone()));
    // Another process now behind the run's pid (same Job, same image): its
    // own valid evidence is never followed, and that is logged once.
    probe.0.lock().insert(101, start + 10_000_000);
    follow(start + 10_000_000);
    tracker.poll(&supervisor);
    tracker.poll(&supervisor);
    assert_eq!(w2_slot(&supervisor, id, |slot| slot.harness_session_id.clone()), Some(followed));
    let refused = |events: &Arc<Mutex<Vec<RuntimeEvent>>>| events.lock().iter().filter(|event| matches!(event,
        RuntimeEvent::SystemLog { message, .. } if message.contains("no longer followed"))).count();
    assert_eq!(refused(&events), 1);
    // A first sighting that started after the launch is refused too.
    let (codex, _, _) = w2_running(&supervisor, "codex", DriverKind::Codex, 102);
    w2_slot(&supervisor, codex, |slot| slot.run_started_at = Some(std::time::SystemTime::now() - Duration::from_secs(600)));
    probe.0.lock().insert(102, start);
    tracker.poll(&supervisor);
    assert_eq!(refused(&events), 2);
}

#[test]
fn w2_poll_follows_grok_new_retries_a_failed_write_and_ignores_its_subagents() {
    let supervisor = test_supervisor();
    let root = tempfile::tempdir().unwrap();
    let start = w2_start();
    create_test_session(&supervisor, "grok", DriverKind::Grok, shared_types::PermissionProfile::Normal);
    let (id, _, _) = w2_running(&supervisor, "grok", DriverKind::Grok, 104);
    let minted = Uuid::new_v4().to_string();
    let cwd = w2_slot(&supervisor, id, |slot| {
        slot.harness_session_id = Some(minted.clone());
        slot.definition.working_dir.clone()
    });
    let grok = root.path().join("grok");
    let log = grok.join("logs").join("unified.jsonl");
    w2_write(&log, "");
    let line = |sid: &str, msg: &str| {
        use std::io::Write as _;
        let entry = serde_json::json!({"ts": chrono::Utc::now().to_rfc3339(), "src": "shell", "pid": 104, "sid": sid, "msg": msg});
        OpenOptions::new().append(true).open(&log).unwrap().write_all(format!("{entry}\n").as_bytes()).unwrap();
    };
    let summary = |sid: &str, kind: Option<&str>| {
        let mut value = serde_json::json!({"info": {"id": sid, "cwd": cwd}});
        if let Some(kind) = kind { value["session_kind"] = serde_json::json!(kind); }
        let folder: String = cwd.bytes().map(|byte| if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            char::from(byte).to_string() } else { format!("%{byte:02X}") }).collect();
        w2_write(&grok.join("sessions").join(folder).join(sid).join("summary.json"), &value.to_string());
    };
    let mut tracker = identity_tracker::IdentityTracker::new(w2_homes(root.path()),
        Box::new(W2Holders::default()), Box::new(W2Probe::of([(104, start)])));
    tracker.prime();
    let new = Uuid::new_v4().to_string();
    let subagent = Uuid::new_v4().to_string();
    line(&new, "session created");
    summary(&new, None);
    line(&subagent, "session created");
    summary(&subagent, Some("subagent"));
    // The log line stays pending until its change is persisted.
    supervisor.fail_next_catalog_write_for_tests();
    tracker.poll(&supervisor);
    assert_eq!(w2_slot(&supervisor, id, |slot| slot.harness_session_id.clone()), Some(minted.clone()));
    tracker.poll(&supervisor);
    assert_eq!(w2_slot(&supervisor, id, |slot| slot.harness_session_id.clone()), Some(new.clone()));
    assert_eq!(w2_catalog_row(&supervisor, id).harness_session_id, Some(new));
}

#[test]
fn w2_poll_gives_the_register_the_claude_title_and_its_renames() {
    let _register = RegisterOn::set();
    let root = tempfile::tempdir().unwrap();
    let supervisor = test_supervisor_with_root(root.path().join("fixture"));
    let homes = tempfile::tempdir().unwrap();
    let start = w2_start();
    let (id, _, _) = w2_running(&supervisor, "claude", DriverKind::Claude, 101);
    let minted = Uuid::new_v4().to_string();
    let cwd = w2_slot(&supervisor, id, |slot| {
        slot.harness_session_id = Some(minted.clone());
        slot.definition.working_dir.clone()
    });
    let claude = homes.path().join("claude");
    let pid_file = |session_id: &str, name: &str| w2_write(&claude.join("sessions").join("101.json"), &serde_json::json!({
        "pid": 101, "sessionId": session_id, "cwd": cwd, "procStart": start.to_string(),
        "kind": "interactive", "pidDomain": "win32:testpc", "name": name}).to_string());
    let mut tracker = identity_tracker::IdentityTracker::new(w2_homes(homes.path()),
        Box::new(W2Holders::default()), Box::new(W2Probe::of([(101, start)])));
    let title = |supervisor: &SupervisorHandle| {
        let titles = register_record(supervisor, id).titles;
        (titles.current, titles.source)
    };
    pid_file(&minted, "session-x");
    tracker.poll(&supervisor);
    assert_eq!(title(&supervisor), (Some("session-x".into()), Some("claude_name".into())));
    pid_file(&minted, "courses");
    tracker.poll(&supervisor);
    assert_eq!(title(&supervisor).0.as_deref(), Some("courses"));
    // A /clear with a new title whose catalog write fails: the title waits
    // for its conversation, and both follow on the next pass.
    let cleared = Uuid::new_v4().to_string();
    let projects = claude.join("projects").join(
        cwd.chars().map(|character| if character.is_ascii_alphanumeric() { character } else { '-' }).collect::<String>());
    w2_write(&projects.join(format!("{cleared}.jsonl")), "{}\n");
    pid_file(&cleared, "after clear");
    supervisor.fail_next_catalog_write_for_tests();
    tracker.poll(&supervisor);
    assert_eq!(title(&supervisor).0.as_deref(), Some("courses"));
    tracker.poll(&supervisor);
    assert_eq!(w2_slot(&supervisor, id, |slot| slot.harness_session_id.clone()), Some(cleared.clone()));
    assert_eq!(title(&supervisor).0.as_deref(), Some("after clear"));
    assert_eq!(register_record(&supervisor, id).conversation.unwrap().id, cleared);
}

/// Two follows race: the first is paused at its log line, after its catalog
/// write; the second is written meanwhile. The register ends where the
/// catalog does.
#[test]
fn w2_apply_tells_the_register_in_catalog_write_order() {
    let _register = RegisterOn::set();
    let root = tempfile::tempdir().unwrap();
    let supervisor = test_supervisor_with_root(root.path().join("fixture"));
    let (id, run, generation) = w2_running(&supervisor, "claude", DriverKind::Claude, 101);
    let (first, second) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
    let (paused, wait_paused) = std::sync::mpsc::channel::<()>();
    let (resume, wait_resume) = std::sync::mpsc::channel::<()>();
    let gate = Mutex::new(Some((paused, wait_resume)));
    supervisor.set_event_sink(move |event| {
        let followed = matches!(&event, RuntimeEvent::SystemLog { message, .. } if message.contains("current conversation followed"));
        let first_follow = if followed { gate.lock().take() } else { None };
        if let Some((paused, wait_resume)) = first_follow {
            paused.send(()).unwrap();
            wait_resume.recv().unwrap();
        }
    });
    let applying = {
        let (supervisor, first) = (supervisor.clone(), first.clone());
        thread::spawn(move || supervisor.apply_tracked_conversation(id, run, generation, &first, "clear", "pid_file", 101, None).unwrap())
    };
    wait_paused.recv().unwrap();
    assert!(supervisor.apply_tracked_conversation(id, run, generation, &second, "clear", "pid_file", 101, None).unwrap());
    resume.send(()).unwrap();
    assert!(applying.join().unwrap());
    assert_eq!(w2_catalog_row(&supervisor, id).harness_session_id, Some(second.clone()));
    assert_eq!(register_record(&supervisor, id).conversation.unwrap().id, second);
}

#[test]
fn w2_tracker_thread_stops_on_shutdown() {
    let supervisor = test_supervisor();
    let root = tempfile::tempdir().unwrap();
    supervisor.start_identity_tracker(identity_tracker::IdentityTracker::new(w2_homes(root.path()),
        Box::new(W2Holders::default()), Box::new(W2Probe::default())));
    let follower = supervisor.inner.identity_tracker.take_thread().expect("the follower thread started");
    supervisor.shutdown().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !follower.is_finished() {
        assert!(Instant::now() < deadline, "the follower thread outlived shutdown");
        thread::sleep(Duration::from_millis(10));
    }
    follower.join().unwrap();
}

/// v3 W4 S1 (Reviewer 8b1858f1): an App Server binding proved before its run
/// is installed is not applied (NotReady); once the run is installed it is
/// applied and releases the launch hold as a writer lock does; a binding for
/// another run is Stale and changes nothing.
#[test]
fn w4_an_app_server_binding_proved_before_install_waits_then_binds() {
    let supervisor = test_supervisor();
    let (id, run, generation) = w2_running(&supervisor, "codex", DriverKind::Codex, 103);
    let thread = Uuid::new_v4().to_string();
    let installed = w2_slot(&supervisor, id, |slot| {
        let installed = (slot.running.take(), slot.process_id.take());
        slot.spawn_in_flight = Some(SpawnReservation { generation, run_id: run });
        installed
    });
    assert_eq!(supervisor.bind_codex_owner_thread(id, run, generation, &thread, "launch").unwrap(),
        codex_app_server::BindOutcome::NotReady);
    assert_eq!(w2_slot(&supervisor, id, |slot| slot.harness_session_id.clone()), None, "nothing applied early");
    // Installed as start_session_at does it: process, run, and the launch hold.
    w2_slot(&supervisor, id, |slot| {
        slot.spawn_in_flight = None;
        (slot.running, slot.process_id) = installed;
        slot.harness_session_id = None;
        slot.recovery = Some(RecoveryReference::unresolved_codex(None, false));
    });
    assert_eq!(supervisor.bind_codex_owner_thread(id, run, generation, &thread, "launch").unwrap(),
        codex_app_server::BindOutcome::Applied);
    assert_eq!(w2_slot(&supervisor, id, |slot| (slot.harness_session_id.clone(), slot.recovery.is_none())),
        (Some(thread.clone()), true));
    assert_eq!(w2_catalog_row(&supervisor, id).harness_session_id, Some(thread.clone()));
    assert_eq!(supervisor.bind_codex_owner_thread(id, Uuid::new_v4(), generation, &Uuid::new_v4().to_string(), "new")
        .unwrap(), codex_app_server::BindOutcome::Stale);
    assert_eq!(w2_slot(&supervisor, id, |slot| slot.harness_session_id.clone()), Some(thread));
}
