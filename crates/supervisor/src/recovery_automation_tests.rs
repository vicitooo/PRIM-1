// A107: real catalog/start/delivery paths with temporary rooms and mock PTYs.
// No provider history, actual harness, scheduler or desktop is used.
// Recording PTYs use the test process's live PID: delivery runs the real
// liveness sweep. Their kill method is a no-op, so Stop stays entirely mocked.

// Crash fixtures use the same real retirement/start/delivery paths below. Only
// the production timer is not scheduled in cfg(test); callbacks get explicit
// times, and every spawned PTY is a mock with a no-op kill method.
fn crash_retire_fixture(supervisor: &SupervisorHandle, member: SessionId,
    exit_code: Option<u32>, kill: MockKillBehavior) -> (recovery_automation::CrashRespawn, Arc<AtomicUsize>) {
    let attempt = {
        let rooms = supervisor.inner.rooms.lock(); let slots = supervisor.inner.slots.lock();
        supervisor.capture_crash_respawn(&rooms, slots.get_by_id(member).unwrap()).unwrap()
    };
    let (pty, _, kills) = mock_pty_session_with_exit_status(None,
        exit_code.map(|code| pty_exit_status(code, None, code == 0)), kill);
    let (generation, run) = {
        let mut slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id_mut(member).unwrap();
        slot.running.as_mut().unwrap().pty = Some(Arc::from(pty));
        slot.process_id = None;
        (slot.generation, slot.run_id.unwrap())
    };
    supervisor.retire_session_run(member, generation, run, if exit_code.is_some() {
        RunRetirementCause::OutputClosed
    } else { RunRetirementCause::PtyError("fixture PTY read failed".into()) });
    (attempt, kills)
}

#[test]
fn crash_respawn_requires_released_owner_then_exactly_resumes_and_continues_working_once() {
    for exit in [Some(3), None] {
        let (_, supervisor, member, room, run) = continuation_fixture(Some(WorkState::Thinking));
        let conversation = supervisor.inner.slots.lock().get_by_id(member).unwrap().harness_session_id.clone().unwrap();
        let (attempt, kills) = crash_retire_fixture(&supervisor, member, exit, MockKillBehavior::Immediate);
        assert_eq!(kills.load(Ordering::SeqCst), 1);
        assert!(supervisor.inner.slots.lock().get_by_id(member).unwrap().running.is_none());
        assert_eq!(continuation_catalog_entry(&supervisor, member)["crash_waiting"], run.to_string());
        let now = Utc::now();
        let attempt = supervisor.arm_crash_respawn(attempt, now).unwrap();
        assert_eq!((attempt.not_before - now).num_seconds(), 5);
        let (pty, inputs) = recording_pty_session(std::process::id());
        let (spawner, specs) = CapturingPtySpawner::new(vec![pty]);
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
        let resumed = supervisor.run_crash_respawn(attempt.clone(), attempt.not_before).unwrap();
        assert!(resumed.running);
        assert_ne!(resumed.run_id, Some(run));
        assert!(specs.lock()[0].args.contains(&format!("--resume={conversation}")));
        assert_eq!(supervisor.inner.slots.lock().get_by_id(member).unwrap().harness_session_id.as_deref(), Some(conversation.as_str()));
        assert_eq!(continuation_catalog_entry(&supervisor, member)["crash_attempts"].as_array().unwrap().len(), 1);
        let posts = continuation_posts(&supervisor, room);
        assert_eq!(posts.len(), 1);
        assert!(posts[0].1.contains("after a crash"));
        assert!(supervisor.run_crash_respawn(attempt.clone(), attempt.not_before).is_err());
        assert_eq!(specs.lock().len(), 1, "the same retired run cannot spawn twice");
        observe_current_run_paste_mode(&supervisor, member, "\x1b[?2004h");
        recovery_transition(&supervisor, member, WorkState::Idle);
        wait_for_no_queued_room_wake(&supervisor, member);
        assert_eq!(inputs.lock().concat().matches("new room message").count(), 1);
        supervisor.shutdown().unwrap();
    }
}

#[test]
fn crash_respawn_excludes_clean_disappeared_requested_and_uncertain_retirement() {
    for case in ["clean", "disappeared", "requested", "uncertain"] {
        let (_, supervisor, member, _, _) = continuation_fixture(Some(WorkState::Idle));
        let captured = {
            let rooms = supervisor.inner.rooms.lock(); let slots = supervisor.inner.slots.lock();
            supervisor.capture_crash_respawn(&rooms, slots.get_by_id(member).unwrap()).unwrap()
        };
        if case == "requested" {
            supervisor.stop_session_by_id(member).unwrap();
        } else if case == "disappeared" {
            let (generation, run) = {
                let slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id(member).unwrap();
                (slot.generation, slot.run_id.unwrap())
            };
            supervisor.retire_session_run(member, generation, run, RunRetirementCause::Liveness {
                final_state: LifecycleState::Closed, fallback_reason: SessionExitReason::ProcessDisappeared,
                detail: "fixture process disappeared without exit status".into(), inferred_at_sequence: None,
            });
        } else {
            crash_retire_fixture(&supervisor, member, Some(if case == "clean" { 0 } else { 3 }),
                if case == "uncertain" { MockKillBehavior::Error("fixture ownership retained") } else { MockKillBehavior::Immediate });
        }
        let (spawner, specs) = CapturingPtySpawner::new(Vec::new());
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
        assert!(supervisor.run_crash_respawn(captured, Utc::now() + chrono::Duration::minutes(1)).is_err(), "{case}");
        assert!(specs.lock().is_empty(), "{case}");
        if case == "uncertain" {
            let mut slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id_mut(member).unwrap();
            assert!(slot.termination_uncertain && slot.running.is_some());
            slot.running.as_mut().unwrap().pty = Some(Arc::from(recording_pty_session(std::process::id()).0));
        }
        supervisor.shutdown().unwrap();
    }
}

#[test]
fn crash_respawn_captures_question_blocked_and_unknown_before_retirement_resets_them() {
    for case in ["blocked", "blocked_then_exited", "unknown", "question", "error_loop_question", "held", "no_id", "terminal", "prime", "roomless"] {
        let (_, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Thinking));
        if case == "roomless" { supervisor.remove_room_member(RemoveRoomMemberRequest { room_id: room, session_id: member }).unwrap(); }
        if case == "blocked_then_exited" {
            let mut slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id_mut(member).unwrap();
            let run = slot.run_id.unwrap(); let alias = slot.definition.alias.clone();
            // Their delayed callbacks have not persisted either transition:
            // the old Thinking observation cannot authorize a human prompt.
            transition_work_state_locked(&alias, slot, run, WorkState::Blocked, None).unwrap();
            transition_work_state_locked(&alias, slot, run, WorkState::Exited, None).unwrap();
            drop(slots);
            assert_eq!(continuation_catalog_entry(&supervisor, member)["observation"]["state"], "thinking");
        }
        {
            let mut slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id_mut(member).unwrap();
            match case {
                "blocked" => { slot.work_state = WorkState::Blocked; }
                "unknown" => { slot.work_state_observed = false; }
                "error_loop_question" => { slot.work_state = WorkState::ErrorLoop; slot.work_detail = Some("workspace_trust".into()); }
                "question" => { slot.room_composer.resize(100, 20);
                    slot.room_composer.observe("\x1b[2J\x1b[HAccessing workspace:\r\nQuick safety check: Is this a project you trust?\r\n\u{276f} No, exit\r\n  Yes, I trust this folder\r\nEnter to confirm · Esc to cancel");
                    assert!(slot.room_composer.open_question().is_some()); }
                "held" => { slot.recovery = Some(RecoveryReference::resume_refused(slot.harness_session_id.clone(), "fixture hold".into())); }
                "no_id" => { slot.harness_session_id = None; }
                "terminal" => { slot.definition.driver = DriverKind::GenericTerminal; }
                "prime" => { slot.definition.driver = DriverKind::Prime; }
                _ => {}
            }
        }
        let rooms = supervisor.inner.rooms.lock(); let slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id(member).unwrap();
        assert!(supervisor.capture_crash_respawn(&rooms, slot).is_none(), "{case}");
        let (generation, run) = (slot.generation, slot.run_id.unwrap());
        drop(slots); drop(rooms);
        supervisor.retire_session_run(member, generation, run, RunRetirementCause::PtyError("fixture terminal failed".into()));
        assert!(continuation_catalog_entry(&supervisor, member)["crash_waiting"].is_null(), "{case}");
        supervisor.shutdown().unwrap();
    }
}

#[test]
fn crash_respawn_backoffs_and_budget_survive_stop_and_reload_without_replaying_timer() {
    let (root, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Idle));
    for delay in [5, 30, 120] {
        let (attempt, _) = crash_retire_fixture(&supervisor, member, Some(3), MockKillBehavior::Immediate);
        let now = Utc::now(); let attempt = supervisor.arm_crash_respawn(attempt, now).unwrap();
        assert_eq!((attempt.not_before - now).num_seconds(), delay);
        supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![recording_pty_session(std::process::id()).0])));
        supervisor.run_crash_respawn(attempt.clone(), attempt.not_before).unwrap();
        recovery_transition(&supervisor, member, WorkState::Idle);
    }
    let (held, _) = crash_retire_fixture(&supervisor, member, Some(3), MockKillBehavior::Immediate);
    assert!(supervisor.inner.slots.lock().get_by_id(member).unwrap().last_error.as_deref().unwrap().contains("3 attempts in 15 minutes"));
    assert!(session_now(&supervisor, member).last_error.unwrap().contains(
        &session_exit_last_error(SessionExitReason::CrashExit, Some(3), None, None).unwrap()));
    assert!(supervisor.inner.slots.lock().get_by_id(member).unwrap().recovery.is_none());
    assert!(continuation_posts(&supervisor, room).is_empty(), "Idle never receives a continuation");
    supervisor.stop_session_by_id(member).unwrap();
    assert_eq!(continuation_catalog_entry(&supervisor, member)["crash_attempts"].as_array().unwrap().len(), 3);
    supervisor.shutdown().unwrap(); drop(supervisor);
    let supervisor = SupervisorHandle::new(test_supervisor_config(&root)).unwrap();
    supervisor.set_exit_watch_enabled_for_tests(false);
    assert_eq!(continuation_catalog_entry(&supervisor, member)["crash_attempts"].as_array().unwrap().len(), 3);
    assert!(supervisor.run_crash_respawn(held, Utc::now() + chrono::Duration::minutes(1)).is_err());
    assert!(!supervisor.snapshot().sessions.iter().find(|s| s.session_id == member).unwrap().running);
    supervisor.set_executable_resolver_for_tests(Arc::new(TestExecutableResolver));
    supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![recording_pty_session(std::process::id()).0])));
    assert!(supervisor.start_session_by_id(member).unwrap().running, "budget does not block manual Start");
    supervisor.shutdown().unwrap();
}

#[test]
fn crash_respawn_backoff_is_revoked_by_stop_manual_start_room_identity_and_grant_changes() {
    for case in ["stop", "fresh", "restart", "room", "identity", "grant", "hold"] {
        let (_, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Idle));
        let (attempt, _) = crash_retire_fixture(&supervisor, member, Some(3), MockKillBehavior::Immediate);
        let (spawner, specs) = CapturingPtySpawner::new(vec![recording_pty_session(std::process::id()).0]);
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
        match case {
            "stop" => { supervisor.stop_session_by_id(member).unwrap(); }
            "fresh" => { supervisor.start_session_by_id_with_mode(member, true).unwrap(); }
            "restart" => { supervisor.restart_session_by_id(member).unwrap(); }
            "room" => { supervisor.remove_room_member(RemoveRoomMemberRequest { room_id: room, session_id: member }).unwrap();
                supervisor.add_room_member(AddRoomMemberRequest { room_id: room, session_id: member }).unwrap(); }
            "identity" => { supervisor.inner.slots.lock().get_by_id_mut(member).unwrap().harness_session_id = Some(Uuid::new_v4().to_string()); }
            "grant" => { supervisor.set_member_permissions(member, MemberPermissions { roam: true, ..Default::default() }).unwrap(); }
            "hold" => { let mut slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id_mut(member).unwrap();
                slot.recovery = Some(RecoveryReference::resume_refused(slot.harness_session_id.clone(), "fixture import hold".into())); }
            _ => unreachable!(),
        }
        let before = specs.lock().len();
        assert!(supervisor.run_crash_respawn(attempt, Utc::now() + chrono::Duration::minutes(3)).is_err(), "{case}");
        assert_eq!(specs.lock().len(), before, "{case}: waiting worker never launches a replacement");
        supervisor.shutdown().unwrap();
    }
}

#[test]
fn crash_respawn_catalog_failure_prevents_auto_spawn_but_does_not_block_manual_start() {
    let (_, supervisor, member, _, _) = continuation_fixture(Some(WorkState::Idle));
    let (attempt, _) = crash_retire_fixture(&supervisor, member, Some(3), MockKillBehavior::Immediate);
    let (spawner, specs) = CapturingPtySpawner::new(vec![recording_pty_session(std::process::id()).0]);
    supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
    supervisor.fail_next_catalog_write_for_tests();
    assert!(supervisor.run_crash_respawn(attempt, Utc::now() + chrono::Duration::minutes(1)).unwrap_err()
        .to_string().contains("injected session catalog persistence failure"));
    assert!(specs.lock().is_empty());
    assert!(supervisor.start_session_by_id(member).unwrap().running);
    supervisor.shutdown().unwrap();
}

#[test]
fn crash_respawn_stop_between_admission_and_reservation_keeps_charged_attempt_and_spawns_nothing() {
    let (_, supervisor, member, _, _) = continuation_fixture(Some(WorkState::Idle));
    let (attempt, _) = crash_retire_fixture(&supervisor, member, Some(3), MockKillBehavior::Immediate);
    let (spawner, specs) = CapturingPtySpawner::new(Vec::new());
    supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
    let stop = supervisor.clone();
    *supervisor.inner.recovery_start_boundary.lock() = Some(Box::new(move |boundary| {
        if boundary == "admitted" { stop.stop_session_by_id(member).unwrap(); }
    }));
    assert!(supervisor.run_crash_respawn(attempt, Utc::now() + chrono::Duration::minutes(1)).is_err());
    supervisor.inner.recovery_start_boundary.lock().take();
    assert!(specs.lock().is_empty());
    assert_eq!(continuation_catalog_entry(&supervisor, member)["crash_attempts"].as_array().unwrap().len(), 1);
    supervisor.shutdown().unwrap();
}

#[test]
fn crash_respawn_room_change_during_spawn_rejects_and_terminates_only_the_new_mock_owner() {
    let (_, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Idle));
    let (attempt, _) = crash_retire_fixture(&supervisor, member, Some(3), MockKillBehavior::Immediate);
    let (pty, _, killed) = mock_pty_session(Some(std::process::id()), MockKillBehavior::Immediate);
    let (spawner, entered, release) = gated_pty_spawner(pty);
    supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
    let handle = supervisor.clone();
    let task = thread::spawn(move || handle.run_crash_respawn(attempt, Utc::now() + chrono::Duration::minutes(1)));
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    supervisor.remove_room_member(RemoveRoomMemberRequest { room_id: room, session_id: member }).unwrap();
    release.send(()).unwrap();
    assert!(task.join().unwrap().is_err());
    assert_eq!(killed.load(Ordering::SeqCst), 1);
    assert!(!supervisor.inner.slots.lock().get_by_id(member).unwrap().termination_uncertain);
    assert!(supervisor.inner.slots.lock().get_by_id(member).unwrap().running.is_none());
    supervisor.shutdown().unwrap();
}

#[test]
fn crash_respawn_upgrade_owned_run_is_not_restarted_or_given_a_second_continuation() {
    let (_, supervisor, member, room, run) = continuation_fixture(Some(WorkState::Thinking));
    {
        let mut catalog = supervisor.inner.catalog.lock();
        let mut entry = serde_json::to_value(&catalog.recovery_automation[&member]).unwrap();
        entry["upgrade"] = serde_json::json!({ "interrupted_run": Uuid::new_v4(), "resumed_run": run,
            "room_id": room, "continuation_id": Uuid::new_v4(),
            "conversation_id": entry["observation"]["conversation_id"], "resolved": false });
        catalog.recovery_automation.insert(member, serde_json::from_value(entry).unwrap());
        supervisor.persist_catalog_candidate(&catalog).unwrap();
    }
    let (attempt, _) = crash_retire_fixture(&supervisor, member, Some(3), MockKillBehavior::Immediate);
    let (spawner, specs) = CapturingPtySpawner::new(Vec::new());
    supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
    assert!(supervisor.run_crash_respawn(attempt, Utc::now() + chrono::Duration::minutes(1)).is_err());
    assert!(specs.lock().is_empty());
    assert!(continuation_posts(&supervisor, room).is_empty());
    supervisor.shutdown().unwrap();
}

#[test]
fn crash_respawn_budget_window_expires_but_future_timestamps_remain_charged() {
    let (_, supervisor, member, _, _) = continuation_fixture(Some(WorkState::Idle));
    let (attempt, _) = crash_retire_fixture(&supervisor, member, Some(3), MockKillBehavior::Immediate);
    let now = Utc::now();
    for (at, allowed) in [(now - chrono::Duration::minutes(15), true), (now + chrono::Duration::seconds(1), false)] {
        let mut catalog = supervisor.inner.catalog.lock();
        let mut entry = serde_json::to_value(&catalog.recovery_automation[&member]).unwrap();
        entry["crash_attempts"] = serde_json::json!([at, at, at]);
        catalog.recovery_automation.insert(member, serde_json::from_value(entry).unwrap());
        supervisor.persist_catalog_candidate(&catalog).unwrap(); drop(catalog);
        let armed = supervisor.arm_crash_respawn(attempt.clone(), now);
        assert_eq!(armed.is_ok(), allowed);
        if let Ok(armed) = armed { assert_eq!((armed.not_before - now).num_seconds(), 5); }
    }
    supervisor.shutdown().unwrap();
}

#[test]
fn crash_respawn_closed_stop_after_retirement_claim_cannot_be_rearmed_and_disk_failure_does_not_block_stop() {
    let (_, supervisor, member, _, _) = continuation_fixture(Some(WorkState::Idle));
    assert!(!session_now(&supervisor, member).crash_respawn_waiting);
    let waiting_event_saw_projection = Arc::new(AtomicBool::new(false));
    let observed = waiting_event_saw_projection.clone(); let handle = supervisor.clone();
    supervisor.set_event_sink(move |event| {
        if matches!(event, RuntimeEvent::SessionState { ref reason, .. } if reason.contains("Crash recovery will resume")) {
            observed.store(handle.inner.slots.lock().get_by_id(member).unwrap().crash_respawn_waiting, Ordering::Release);
        }
    });
    let (attempt, _) = crash_retire_fixture(&supervisor, member, Some(3), MockKillBehavior::Immediate);
    assert!(session_now(&supervisor, member).crash_respawn_waiting);
    assert!(waiting_event_saw_projection.load(Ordering::Acquire), "refresh-driving event follows waiting projection");
    supervisor.set_event_sink(|_| {});
    let generation = supervisor.inner.slots.lock().get_by_id(member).unwrap().generation;
    supervisor.fail_next_catalog_write_for_tests();
    assert!(!supervisor.stop_session_by_id(member).unwrap().running);
    assert!(!session_now(&supervisor, member).crash_respawn_waiting);
    assert_eq!(supervisor.inner.slots.lock().get_by_id(member).unwrap().generation, generation);
    assert!(supervisor.arm_crash_respawn(attempt.clone(), Utc::now()).is_err(), "Stop revoked the pre-proof claim, not just its timer");
    assert!(supervisor.run_crash_respawn(attempt, Utc::now() + chrono::Duration::minutes(1)).is_err());
    supervisor.shutdown().unwrap();
}

#[test]
fn crash_respawn_stop_during_retirement_proof_revokes_only_an_authorized_exact_claim() {
    for case in ["operator", "manager", "unrelated_claim"] {
        let (_, supervisor, member, room, run) = continuation_fixture(Some(WorkState::Idle));
        let manager = test_session_id(&supervisor, "codex");
        let caller = mobility_pane(&supervisor, "codex", 19401);
        supervisor.add_room_member(AddRoomMemberRequest { room_id: room, session_id: manager }).unwrap();
        let check = member_lifecycle::ManageCheck::new(&caller, room);
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let (attempt, generation, owner) = {
            let rooms = supervisor.inner.rooms.lock(); let mut slots = supervisor.inner.slots.lock();
            let slot = slots.get_by_id_mut(member).unwrap();
            let attempt = supervisor.capture_crash_respawn(&rooms, slot).unwrap();
            let owner: Arc<dyn PtySessionTrait> = Arc::new(GatedKillPtySession {
                process_id: std::process::id(), kill_entered: Mutex::new(Some(entered_tx)),
                kill_release: Mutex::new(release_rx),
            });
            slot.running.as_mut().unwrap().pty = Some(owner.clone());
            (attempt, slot.generation, owner)
        };
        supervisor.set_stop_kill_timeout_for_tests(Duration::from_secs(30));
        let (spawner, specs) = CapturingPtySpawner::new(Vec::new());
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
        let handle = supervisor.clone();
        let retirement = thread::spawn(move || handle.retire_session_run(member, generation, run,
            RunRetirementCause::PtyError("fixture retirement proof".into())));
        entered_rx.recv_timeout(Duration::from_secs(2)).expect("retirement did not enter mock kill");

        // Authority and expected-run failures must not cancel the claim.
        let before = serde_json::to_value(&supervisor.inner.catalog.lock().recovery_automation[&member]).unwrap();
        let wrong_run = supervisor.declare_stop_operation(member, Some((generation, Some(Uuid::new_v4()))), StopIntentKind::Operator);
        let denied = supervisor.begin_stop_session_checked(member, Some(&check)).err();
        supervisor.set_member_permissions(manager, manage_grant(false)).unwrap();
        let mut stale = caller.clone(); stale.run_id = Uuid::new_v4();
        let stale_check = member_lifecycle::ManageCheck::new(&stale, room);
        let stale_stop = supervisor.begin_stop_session_checked(member, Some(&stale_check)).err();
        let invalid_preserved = serde_json::to_value(&supervisor.inner.catalog.lock().recovery_automation[&member]).unwrap() == before;
        let foreign_run = Uuid::new_v4();
        if case == "unrelated_claim" {
            let mut catalog = supervisor.inner.catalog.lock();
            let mut entry = serde_json::to_value(&catalog.recovery_automation[&member]).unwrap();
            entry["crash_waiting"] = serde_json::json!(foreign_run);
            catalog.recovery_automation.insert(member, serde_json::from_value(entry).unwrap());
        }
        let busy = if case == "manager" { supervisor.begin_stop_session_checked(member, Some(&check)).err() }
            else { supervisor.begin_stop_session(member).err() };
        let after = serde_json::to_value(&supervisor.inner.catalog.lock().recovery_automation[&member]).unwrap();
        let owner_unchanged = {
            let slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id(member).unwrap();
            slot.generation == generation && slot.run_id.is_none() && slot.last_run_id == Some(run)
                && slot.termination_uncertain && slot.lifecycle_operation == Some(LifecycleOperation {
                    generation, kind: StopIntentKind::Operator,
                }) && slot.running.as_ref().and_then(|r| r.pty.as_ref()).is_some_and(|pty| Arc::ptr_eq(pty, &owner))
        };
        // Always release the mock proof before assertions; no real process is killed.
        release_tx.send(()).unwrap(); retirement.join().unwrap();
        assert_eq!(before["crash_waiting"], serde_json::json!(run), "retirement claimed this run before proof: {case}");
        assert!(wrong_run.unwrap_err().to_string().contains("run changed"), "{case}");
        assert!(denied.unwrap().to_string().contains("Can manage agents"), "{case}");
        assert!(stale_stop.unwrap().to_string().contains("stale"), "{case}");
        assert!(invalid_preserved && owner_unchanged, "{case}");
        assert!(busy.unwrap().to_string().contains("lifecycle operation in progress"), "{case}");
        let mut expected = before;
        if case == "unrelated_claim" { expected["crash_waiting"] = serde_json::json!(foreign_run); }
        else { expected.as_object_mut().unwrap().remove("crash_waiting"); }
        assert_eq!(after, expected, "only the exact retiring claim may change: {case}");
        assert_eq!(continuation_catalog_entry(&supervisor, member)["crash_waiting"], expected["crash_waiting"], "retirement persists cancellation: {case}");
        assert!(supervisor.arm_crash_respawn(attempt.clone(), Utc::now()).is_err(), "{case}");
        assert!(supervisor.run_crash_respawn(attempt, Utc::now() + chrono::Duration::minutes(1)).is_err(), "{case}");
        assert!(specs.lock().is_empty(), "no replacement after Stop during proof: {case}");
        let slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id(member).unwrap();
        assert!(slot.running.is_none() && slot.lifecycle_operation.is_none() && !slot.termination_uncertain, "original retirement finished: {case}");
        drop(slots);
        supervisor.shutdown().unwrap();
    }
}

#[test]
fn crash_respawn_closed_stop_while_resolving_executable_cancels_without_generation_change() {
    struct BlockingResolver { entered: mpsc::SyncSender<()>, release: Mutex<mpsc::Receiver<()>> }
    impl DriverExecutableResolver for BlockingResolver {
        fn resolve(&self, driver: DriverKind) -> Result<ResolvedLaunchProgram> {
            self.entered.send(()).unwrap();
            self.release.lock().recv_timeout(Duration::from_secs(10)).unwrap();
            TestExecutableResolver.resolve(driver)
        }
    }
    let (_, supervisor, member, _, _) = continuation_fixture(Some(WorkState::Idle));
    let (attempt, _) = crash_retire_fixture(&supervisor, member, Some(3), MockKillBehavior::Immediate);
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    supervisor.set_executable_resolver_for_tests(Arc::new(BlockingResolver { entered: entered_tx, release: Mutex::new(release_rx) }));
    let (spawner, specs) = CapturingPtySpawner::new(Vec::new());
    supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
    let handle = supervisor.clone();
    let task = thread::spawn(move || handle.run_crash_respawn(attempt, Utc::now() + chrono::Duration::minutes(1)));
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let generation = supervisor.inner.slots.lock().get_by_id(member).unwrap().generation;
    assert!(!supervisor.stop_session_by_id(member).unwrap().running);
    assert_eq!(supervisor.inner.slots.lock().get_by_id(member).unwrap().generation, generation);
    release_tx.send(()).unwrap();
    assert!(task.join().unwrap().is_err());
    assert!(specs.lock().is_empty());
    supervisor.shutdown().unwrap();
}

#[test]
fn crash_respawn_failed_spawn_preserves_crash_diagnosis_in_status_and_notice() {
    let (_, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Idle));
    let (attempt, _) = crash_retire_fixture(&supervisor, member, Some(3), MockKillBehavior::Immediate);
    let original = session_now(&supervisor, member).last_error.unwrap();
    let attempt = supervisor.arm_crash_respawn(attempt, Utc::now()).unwrap();
    supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(Vec::new())));
    let events = capture_runtime_events(&supervisor);
    assert!(supervisor.run_crash_respawn(attempt.clone(), attempt.not_before).is_err());
    let snapshot = session_now(&supervisor, member);
    assert!(!snapshot.crash_respawn_waiting);
    let detail = snapshot.last_error.unwrap();
    assert!(detail.contains(&original) && detail.contains("no queued PTY sessions"), "{detail}");
    assert!(mobility_messages(&supervisor, room).iter().any(|(_, text)| text.contains(&original)
        && text.contains("Automatic crash resume did not complete")));
    assert!(events.lock().iter().any(|event| matches!(event, RuntimeEvent::SessionState { reason, .. }
        if reason.contains(&original) && reason.contains("Automatic crash resume did not complete"))));
    supervisor.shutdown().unwrap();
}

#[test]
fn crash_respawn_closing_callback_keeps_token_and_performs_no_effects() {
    struct ClosingResolver { supervisor: SupervisorHandle, upgrade: bool }
    impl DriverExecutableResolver for ClosingResolver {
        fn resolve(&self, driver: DriverKind) -> Result<ResolvedLaunchProgram> {
            if self.upgrade { self.supervisor.inner.upgrade_closing.store(true, Ordering::Release); }
            else { self.supervisor.inner.shutdown_started.store(true, Ordering::Release); }
            TestExecutableResolver.resolve(driver)
        }
    }
    for upgrade in [false, true] { for boundary in ["before", "prepare", "admitted"] {
        let (_, supervisor, member, _, _) = continuation_fixture(Some(WorkState::Thinking));
        let (attempt, _) = crash_retire_fixture(&supervisor, member, Some(3), MockKillBehavior::Immediate);
        let events = capture_runtime_events(&supervisor);
        let before = supervisor.inner.catalog.lock().clone();
        let snapshot = serde_json::to_value(supervisor.inner.slots.lock().get_by_id(member).unwrap().snapshot()).unwrap();
        let baseline = Arc::new(Mutex::new((
            before,
            fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap(),
            snapshot,
            events.lock().len(),
        )));
        let admitted = Arc::new(AtomicBool::new(false));
        let (spawner, specs) = CapturingPtySpawner::new(Vec::new());
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
        if boundary == "admitted" {
            let handle = supervisor.clone(); let baseline = baseline.clone();
            let events = events.clone(); let admitted = admitted.clone();
            *supervisor.inner.recovery_start_boundary.lock() = Some(Box::new(move |boundary| {
                assert_eq!(boundary, "admitted");
                // Budget reservation and Starting are legitimate before close.
                // Capture their exact state; no later cleanup may rewrite it.
                let before = handle.inner.catalog.lock().clone();
                let snapshot = serde_json::to_value(handle.inner.slots.lock().get_by_id(member).unwrap().snapshot()).unwrap();
                *baseline.lock() = (
                    before,
                    fs::read(session_catalog_path(handle.runtime_dir())).unwrap(),
                    snapshot,
                    events.lock().len(),
                );
                admitted.store(true, Ordering::Release);
                if upgrade { handle.inner.upgrade_closing.store(true, Ordering::Release); }
                else { handle.inner.shutdown_started.store(true, Ordering::Release); }
                handle.fail_next_catalog_write_for_tests();
            }));
        } else if boundary == "prepare" {
            supervisor.set_executable_resolver_for_tests(Arc::new(ClosingResolver { supervisor: supervisor.clone(), upgrade }));
        } else if upgrade { supervisor.inner.upgrade_closing.store(true, Ordering::Release); }
        else { supervisor.inner.shutdown_started.store(true, Ordering::Release); }
        if boundary != "admitted" { supervisor.fail_next_catalog_write_for_tests(); }
        assert!(supervisor.run_crash_respawn(attempt, Utc::now() + chrono::Duration::minutes(1)).is_err());
        assert_eq!(admitted.load(Ordering::Acquire), boundary == "admitted");
        let baseline = baseline.lock();
        assert_eq!(*supervisor.inner.catalog.lock(), baseline.0, "{boundary}, upgrade={upgrade}");
        assert_eq!(fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap(), baseline.1);
        assert_eq!(serde_json::to_value(supervisor.inner.slots.lock().get_by_id(member).unwrap().snapshot()).unwrap(), baseline.2);
        assert!(supervisor.inner.fail_next_catalog_write.load(Ordering::Acquire), "no persistence attempted");
        assert_eq!(events.lock().len(), baseline.3, "closing emits no recovery status or notices");
        assert!(specs.lock().is_empty());
        drop(baseline);
        supervisor.inner.recovery_start_boundary.lock().take();
        supervisor.inner.upgrade_closing.store(false, Ordering::Release);
        supervisor.inner.shutdown_started.store(false, Ordering::Release);
        supervisor.inner.fail_next_catalog_write.store(false, Ordering::Release);
        supervisor.set_executable_resolver_for_tests(Arc::new(TestExecutableResolver));
        supervisor.shutdown().unwrap();
    }}
}

#[test]
fn crash_respawn_upgrade_cancel_releases_stranded_start_without_retry_or_budget_refund() {
    for fail_clear in [false, true] {
        let fixture = upgrade_fixture(); let supervisor = &fixture.supervisor; let member = fixture.claude;
        supervisor.set_exit_watch_enabled_for_tests(false);
        let admission = upgrade_admitted(upgrade_request(&fixture));
        recovery_transition(supervisor, member, WorkState::Thinking);
        let (attempt, _) = crash_retire_fixture(supervisor, member, Some(3), MockKillBehavior::Immediate);
        let original = session_now(supervisor, member).last_error.unwrap();
        let (spawner, specs) = CapturingPtySpawner::new(vec![recording_pty_session(std::process::id()).0]);
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
        let closing = supervisor.clone();
        *supervisor.inner.recovery_start_boundary.lock() = Some(Box::new(move |boundary| {
            assert_eq!(boundary, "admitted");
            closing.inner.upgrade_closing.store(true, Ordering::Release);
        }));
        assert!(supervisor.run_crash_respawn(attempt, Utc::now() + chrono::Duration::minutes(1)).is_err());
        supervisor.inner.recovery_start_boundary.lock().take();
        let before = session_now(supervisor, member);
        assert_eq!(before.lifecycle_state, LifecycleState::Starting);
        assert!(specs.lock().is_empty());
        let events = capture_runtime_events(supervisor);
        if fail_clear { supervisor.fail_next_catalog_write_for_tests(); }
        supervisor.cancel_upgrade(admission.attempt_id).unwrap();
        let after = session_now(supervisor, member);
        assert_eq!(after.generation, before.generation);
        assert_eq!(after.lifecycle_state, LifecycleState::Failed);
        assert!(!after.running && !after.crash_respawn_waiting);
        let detail = after.last_error.unwrap();
        assert!(detail.contains(&original) && detail.contains("use Start or Restart"), "{detail}");
        let entry = serde_json::to_value(&supervisor.inner.catalog.lock().recovery_automation[&member]).unwrap();
        assert_eq!(entry["crash_attempts"].as_array().unwrap().len(), 1);
        assert!(entry.get("crash_waiting").is_none() && entry["pending"].is_null());
        assert!(events.lock().iter().any(|event| matches!(event, RuntimeEvent::SessionState { state: LifecycleState::Failed, reason, .. }
            if reason.contains("upgrade closing") && reason.contains(&original))));
        if fail_clear {
            assert!(events.lock().iter().any(|event| matches!(event, RuntimeEvent::SystemLog { message, .. }
                if message.contains("injected session catalog persistence failure"))));
        }
        assert!(specs.lock().is_empty(), "cancel does not start a replacement");
        assert!(supervisor.start_session_by_id(member).unwrap().running);
        assert_eq!(specs.lock().len(), 1, "existing manual Start remains usable");
        assert!(continuation_posts(supervisor, fixture.room).is_empty());
        supervisor.shutdown().unwrap();
    }
}

#[test]
fn crash_respawn_upgrade_reopen_does_not_mark_manual_changed_or_owned_starts() {
    for case in ["manual", "changed_generation", "replacement_run", "spawn", "lifecycle", "shutdown"] {
        let (_, supervisor, member, _, _) = continuation_fixture(Some(WorkState::Idle));
        let (attempt, _) = crash_retire_fixture(&supervisor, member, Some(3), MockKillBehavior::Immediate);
        let closing = supervisor.clone();
        *supervisor.inner.recovery_start_boundary.lock() = Some(Box::new(move |boundary| {
            assert_eq!(boundary, "admitted");
            closing.inner.upgrade_closing.store(true, Ordering::Release);
        }));
        assert!(supervisor.run_crash_respawn(attempt, Utc::now() + chrono::Duration::minutes(1)).is_err());
        supervisor.inner.recovery_start_boundary.lock().take();
        if case == "manual" {
            supervisor.inner.upgrade_closing.store(false, Ordering::Release);
            supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![recording_pty_session(std::process::id()).0])));
            assert!(supervisor.start_session_by_id(member).unwrap().running);
            supervisor.inner.upgrade_closing.store(true, Ordering::Release);
        } else {
            let mut slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id_mut(member).unwrap();
            match case {
                "changed_generation" => slot.generation += 1,
                "replacement_run" => slot.run_id = Some(Uuid::new_v4()),
                "spawn" => slot.spawn_in_flight = Some(SpawnReservation { generation: slot.generation, run_id: Uuid::new_v4() }),
                "lifecycle" => slot.lifecycle_operation = Some(LifecycleOperation { generation: slot.generation, kind: StopIntentKind::Restart }),
                "shutdown" => supervisor.inner.shutdown_started.store(true, Ordering::Release),
                _ => unreachable!(),
            }
        }
        let before = serde_json::to_value(supervisor.inner.slots.lock().get_by_id(member).unwrap().snapshot()).unwrap();
        let catalog = supervisor.inner.catalog.lock().clone();
        let bytes = fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap();
        let events = capture_runtime_events(&supervisor);
        supervisor.fail_next_catalog_write_for_tests();
        supervisor.reopen_after_upgrade();
        assert_eq!(serde_json::to_value(supervisor.inner.slots.lock().get_by_id(member).unwrap().snapshot()).unwrap(), before, "{case}");
        assert_eq!(*supervisor.inner.catalog.lock(), catalog, "{case}");
        assert_eq!(fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap(), bytes, "{case}");
        assert!(events.lock().is_empty(), "{case}");
        assert!(supervisor.inner.fail_next_catalog_write.load(Ordering::Acquire), "{case}");
        supervisor.inner.fail_next_catalog_write.store(false, Ordering::Release);
        supervisor.inner.shutdown_started.store(false, Ordering::Release);
        {
            let mut slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id_mut(member).unwrap();
            slot.spawn_in_flight = None; slot.lifecycle_operation = None;
            if slot.running.is_none() { slot.run_id = None; }
        }
        supervisor.shutdown().unwrap();
    }
}

#[test]
fn crash_respawn_boot_clears_waiting_without_replay_or_resetting_budget() {
    let (root, supervisor, member, _, _) = continuation_fixture(Some(WorkState::Idle));
    let (_, _) = crash_retire_fixture(&supervisor, member, Some(3), MockKillBehavior::Immediate);
    let at = Utc::now();
    {
        let mut catalog = supervisor.inner.catalog.lock();
        let mut entry = serde_json::to_value(&catalog.recovery_automation[&member]).unwrap();
        entry["crash_attempts"] = serde_json::json!([at]);
        catalog.recovery_automation.insert(member, serde_json::from_value(entry).unwrap());
        supervisor.persist_catalog_candidate(&catalog).unwrap();
    }
    assert!(session_now(&supervisor, member).crash_respawn_waiting);
    supervisor.shutdown().unwrap(); drop(supervisor);
    let supervisor = SupervisorHandle::new(test_supervisor_config(&root)).unwrap();
    let snapshot = session_now(&supervisor, member);
    assert!(!snapshot.running && !snapshot.crash_respawn_waiting && !snapshot.was_running_at_shutdown);
    assert!(snapshot.last_error.unwrap().contains("was waiting when PRIM-1 closed; use Restart"));
    let entry = continuation_catalog_entry(&supervisor, member);
    assert!(entry.get("crash_waiting").is_none());
    assert_eq!(entry["crash_attempts"], serde_json::json!([at]));
    supervisor.shutdown().unwrap();
}

#[test]
fn crash_respawn_elapsed_timer_tolerates_wall_clock_rollback() {
    let (_, supervisor, member, _, _) = continuation_fixture(Some(WorkState::Idle));
    let (attempt, _) = crash_retire_fixture(&supervisor, member, Some(3), MockKillBehavior::Immediate);
    let attempt = supervisor.arm_crash_respawn(attempt, Utc::now()).unwrap();
    let (spawner, specs) = CapturingPtySpawner::new(vec![recording_pty_session(std::process::id()).0]);
    supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
    let rolled_back = attempt.not_before - chrono::Duration::hours(2);
    assert!(supervisor.run_elapsed_crash_respawn(attempt, rolled_back).unwrap().running);
    assert_eq!(specs.lock().len(), 1);
    supervisor.shutdown().unwrap();
}

#[test]
fn crash_respawn_failed_crash_mark_is_visible_held_without_arming() {
    let (_, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Thinking));
    // Make the ordinary retired-row write a no-op, so this failure targets the
    // following crash observation sync, not unrelated identity persistence.
    {
        let mut catalog = supervisor.inner.catalog.lock();
        catalog.sessions.iter_mut().find(|row| row.session_id == member).unwrap().running_at_shutdown = false;
        supervisor.persist_catalog_candidate(&catalog).unwrap();
    }
    supervisor.fail_next_catalog_write_for_tests();
    let (attempt, _) = crash_retire_fixture(&supervisor, member, Some(3), MockKillBehavior::Immediate);
    let snapshot = session_now(&supervisor, member);
    let detail = snapshot.last_error.unwrap();
    assert!(detail.contains("Automatic crash recovery held") && detail.contains("persistence failure"), "{detail}");
    assert!(detail.contains(&session_exit_last_error(SessionExitReason::CrashExit, Some(3), None, None).unwrap()),
        "original crash exit remains visible: {detail}");
    assert!(!snapshot.crash_respawn_waiting);
    assert!(supervisor.arm_crash_respawn(attempt, Utc::now()).is_err());
    assert!(mobility_messages(&supervisor, room).iter().any(|(_, text)| text.contains("Automatic crash recovery held")
        && text.contains("persistence failure")));
    supervisor.shutdown().unwrap();
}

#[test]
fn crash_respawn_omits_only_empty_new_catalog_fields() {
    let empty = serde_json::to_value(recovery_automation::RecoveryAutomation::default()).unwrap();
    assert!(empty.get("crash_attempts").is_none() && empty.get("crash_waiting").is_none());
    let mut populated = empty.clone();
    populated["crash_waiting"] = serde_json::json!(Uuid::new_v4());
    populated["crash_attempts"] = serde_json::json!([Utc::now()]);
    let entry: recovery_automation::RecoveryAutomation = serde_json::from_value(populated.clone()).unwrap();
    assert_eq!(serde_json::to_value(entry).unwrap(), populated, "nonempty new state remains explicitly version-sensitive");
    populated["unknown_future_field"] = true.into();
    assert!(serde_json::from_value::<recovery_automation::RecoveryAutomation>(populated).is_err());
}

fn recovery_transition(supervisor: &SupervisorHandle, member: SessionId, state: WorkState) -> RuntimeEvent {
    let (event, generation, run_id) = {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id_mut(member).unwrap();
        let run_id = slot.run_id.unwrap();
        let alias = slot.definition.alias.clone();
        let event = transition_work_state_locked(&alias, slot, run_id, state, None).unwrap();
        (event, slot.generation, run_id)
    };
    supervisor.emit_run_work_state(event.clone(), generation, run_id);
    event
}

fn continuation_fixture(state: Option<WorkState>) -> (PathBuf, SupervisorHandle, SessionId, RoomId, Uuid) {
    let root = std::env::temp_dir().join(format!("prim1-continuation-{}", Uuid::new_v4()));
    let supervisor = test_supervisor_with_root(root.clone());
    supervisor.set_exit_watch_enabled_for_tests(false);
    let member = test_session_id(&supervisor, "claude");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Recovery".into()),
        member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap().room_id;
    supervisor.set_room_resume_on_restart(SetRoomResumeRequest { room_id: room, resume_on_restart: true }).unwrap();
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "claude", DriverKind::Claude,
        recording_pty_session(std::process::id()).0);
    let run = {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id_mut(member).unwrap();
        slot.harness_session_id = Some(Uuid::new_v4().to_string());
        slot.running_at_shutdown = true;
        slot.work_state_observed = false;
        slot.run_id.unwrap()
    };
    supervisor.persist_slot_harness_state(member);
    if let Some(state) = state { recovery_transition(&supervisor, member, state); }
    (root, supervisor, member, room, run)
}

fn reopen_continuation_fixture(root: &Path, supervisor: SupervisorHandle) -> SupervisorHandle {
    supervisor.shutdown().unwrap();
    drop(supervisor);
    let reopened = SupervisorHandle::new(test_supervisor_config(root)).unwrap();
    reopened.set_exit_watch_enabled_for_tests(false);
    reopened.set_executable_resolver_for_tests(Arc::new(TestExecutableResolver));
    reopened.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![recording_pty_session(std::process::id()).0])));
    reopened.resume_room_wake_queue(); // Production replays queued intent before the renderer starts members.
    reopened
}

fn continuation_posts(supervisor: &SupervisorHandle, room: RoomId) -> Vec<(String, String)> {
    // Every page of the retained window: a full window spans several pages.
    let mut found = Vec::new();
    let mut cursor = None;
    loop {
        let page = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor }).unwrap();
        found.extend(page.events.iter().filter_map(|event| match &event.item {
            RoomFeedItem::Message { request_id, content, .. } if request_id.starts_with("continuation-") =>
                Some((request_id.clone(), content.clone())),
            _ => None,
        }));
        if !page.has_more { return found; }
        cursor = Some(page.cursor);
    }
}

fn continuation_catalog_entry(supervisor: &SupervisorHandle, member: SessionId) -> serde_json::Value {
    let catalog = read_session_catalog(&session_catalog_path(supervisor.runtime_dir())).unwrap();
    catalog.recovery_automation.get(&member).map(|entry| serde_json::to_value(entry).unwrap())
        .unwrap_or(serde_json::Value::Null)
}

#[test]
fn continuation_working_survives_two_restarts_with_one_original_queued_intent() {
    let (root, supervisor, member, room, run) = continuation_fixture(Some(WorkState::Thinking));
    let before = continuation_catalog_entry(&supervisor, member);
    let original_id = before["observation"]["continuation_id"].as_str().unwrap().to_owned();
    let supervisor = reopen_continuation_fixture(&root, supervisor);
    supervisor.resume_session_after_restart(member).unwrap();
    assert_eq!(continuation_posts(&supervisor, room).len(), 1);
    let queued = continuation_catalog_entry(&supervisor, member);
    assert_eq!(queued["pending"]["interrupted"]["run_id"], run.to_string());
    assert_eq!(queued["pending"]["interrupted"]["continuation_id"], original_id);
    assert_eq!(queued["pending"]["phase"], "queued");
    let supervisor = reopen_continuation_fixture(&root, supervisor);
    supervisor.resume_session_after_restart(member).unwrap();
    assert_eq!(continuation_posts(&supervisor, room), vec![(format!("continuation-{original_id}"),
        "PRIM-1 restarted your session after a PRIM-1 restart. Continue the task you were working on; its state is in the task folder. This is an automatic recovery reminder, not a new instruction from Victor.".into())]);
    assert_eq!(continuation_catalog_entry(&supervisor, member)["pending"]["interrupted"]["run_id"], run.to_string());
    supervisor.shutdown().unwrap();
}

#[test]
fn continuation_idle_blocked_error_and_unknown_do_not_prompt_and_fresh_does_not_inherit() {
    for state in [Some(WorkState::Idle), Some(WorkState::Blocked), Some(WorkState::ErrorLoop), None] {
        let (root, supervisor, member, room, _) = continuation_fixture(state);
        let supervisor = reopen_continuation_fixture(&root, supervisor);
        supervisor.resume_session_after_restart(member).unwrap();
        assert!(continuation_posts(&supervisor, room).is_empty(), "{state:?}");
        supervisor.shutdown().unwrap();
    }
    let (root, supervisor, member, room, _) = continuation_fixture(Some(WorkState::ToolCall));
    let supervisor = reopen_continuation_fixture(&root, supervisor);
    supervisor.start_session_by_id_with_options(member, true, false).unwrap();
    assert!(continuation_posts(&supervisor, room).is_empty());
    supervisor.shutdown().unwrap();
}

#[test]
fn continuation_stop_while_closed_invalidates_a_later_manual_resume() {
    let (root, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Thinking));
    let supervisor = reopen_continuation_fixture(&root, supervisor);
    supervisor.stop_session_by_id(member).unwrap();
    assert!(continuation_catalog_entry(&supervisor, member).is_null());
    supervisor.start_session_by_id_with_options(member, false, false).unwrap();
    assert!(continuation_posts(&supervisor, room).is_empty());
    supervisor.shutdown().unwrap();
}

#[test]
fn continuation_catalog_failure_does_not_prevent_stop_or_revive_current_process_eligibility() {
    let (_, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Thinking));
    let events = capture_runtime_events(&supervisor);
    supervisor.inner.fail_next_catalog_write.store(true, Ordering::Release);
    let stopped = supervisor.stop_session_by_id(member).unwrap();
    assert!(!stopped.running);
    assert!(!supervisor.inner.catalog.lock().recovery_automation.contains_key(&member));
    assert!(events.lock().iter().any(|event| matches!(event,
        RuntimeEvent::SystemLog { message, .. } if message.contains("automatic continuation")
            && message.contains("injected session catalog persistence failure"))));
    supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![recording_pty_session(std::process::id()).0])));
    supervisor.start_session_by_id(member).unwrap();
    assert!(continuation_posts(&supervisor, room).is_empty());
    supervisor.shutdown().unwrap();
}

#[test]
fn continuation_clean_exit_clears_eligibility_but_crash_keeps_last_working_run() {
    for exit_code in [0, 3] {
        let (_, supervisor, member, _, _) = continuation_fixture(None);
        let conversation = supervisor.inner.slots.lock().get_by_id(member).unwrap().harness_session_id.clone();
        let (pty, _, _) = mock_pty_session_with_exit_status(None,
            Some(pty_exit_status(exit_code, None, exit_code == 0)), MockKillBehavior::Immediate);
        install_mock_running_session_with_process_id(&supervisor, "claude", DriverKind::Claude, None, pty);
        supervisor.inner.slots.lock().get_by_id_mut(member).unwrap().harness_session_id = conversation;
        recovery_transition(&supervisor, member, WorkState::Thinking);
        handle_current_pty_event(&supervisor, "claude", 0, PtyEvent::Closed);
        let entry = continuation_catalog_entry(&supervisor, member);
        if exit_code == 0 { assert!(entry.is_null()); }
        else {
            assert_eq!(entry["observation"]["state"], "thinking");
            assert_eq!(entry["observation"]["crashed"], true);
        }
        supervisor.shutdown().unwrap();
    }
}

#[test]
fn continuation_stop_between_preparation_and_append_prevents_new_intent() {
    let (root, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Thinking));
    let supervisor = reopen_continuation_fixture(&root, supervisor);
    let stop = supervisor.clone();
    supervisor.set_room_delivery_after_preflight_for_tests(move || {
        stop.stop_session_by_id(member).unwrap();
    });
    supervisor.resume_session_after_restart(member).unwrap();
    assert!(continuation_posts(&supervisor, room).is_empty());
    assert!(continuation_catalog_entry(&supervisor, member).is_null());
    supervisor.shutdown().unwrap();
}

#[test]
fn continuation_room_leave_clears_a_definite_preappend_refusal() {
    let (root, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Thinking));
    let supervisor = reopen_continuation_fixture(&root, supervisor);
    let mutate = supervisor.clone();
    supervisor.set_room_delivery_after_preflight_for_tests(move || {
        // A known pre-append removal is not an uncertain transport effect.
        mutate.remove_room_member(RemoveRoomMemberRequest { room_id: room, session_id: member }).unwrap();
    });
    supervisor.resume_session_after_restart(member).unwrap();
    assert!(continuation_catalog_entry(&supervisor, member).is_null());
    supervisor.add_room_member(AddRoomMemberRequest { room_id: room, session_id: member }).unwrap();
    let supervisor = reopen_continuation_fixture(&root, supervisor);
    supervisor.resume_session_after_restart(member).unwrap();
    assert!(continuation_posts(&supervisor, room).is_empty());
    assert!(continuation_catalog_entry(&supervisor, member)["pending"].is_null());
    supervisor.shutdown().unwrap();
}

#[test]
fn continuation_rejects_delayed_state_and_keeps_actual_eligibility_when_persistence_fails() {
    let (_, supervisor, member, _, run) = continuation_fixture(Some(WorkState::Thinking));
    let thinking = {
        let slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id(member).unwrap();
        (slot.generation, slot.last_work_state_sequence.unwrap())
    };
    recovery_transition(&supervisor, member, WorkState::Blocked);
    supervisor.observe_recovery_work_state(member, thinking.0, run, thinking.1, WorkState::Thinking);
    assert_eq!(continuation_catalog_entry(&supervisor, member)["observation"]["state"], "blocked");
    let before = supervisor.inner.catalog.lock().clone();
    supervisor.inner.fail_next_catalog_write.store(true, Ordering::Release);
    recovery_transition(&supervisor, member, WorkState::ToolCall);
    assert_eq!(serde_json::to_value(&supervisor.inner.catalog.lock().recovery_automation[&member]).unwrap()["observation"]["state"], "tool_call");
    assert_eq!(read_session_catalog(&session_catalog_path(supervisor.runtime_dir())).unwrap(), before);
    supervisor.shutdown().unwrap();
}

#[test]
fn continuation_unknown_codex_identity_is_bound_by_same_run_w2_without_another_transition() {
    let supervisor = test_supervisor();
    let (member, run, generation) = w2_running(&supervisor, "codex", DriverKind::Codex, 9911);
    supervisor.create_room(CreateRoomRequest { label: Some("Late identity".into()),
        member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap();
    supervisor.inner.slots.lock().get_by_id_mut(member).unwrap().harness_session_id = None;
    recovery_transition(&supervisor, member, WorkState::Thinking);
    let before = continuation_catalog_entry(&supervisor, member);
    assert!(before["observation"]["conversation_id"].is_null());
    let id = Uuid::new_v4().to_string();
    assert!(supervisor.apply_tracked_conversation(member, run, generation, &id,
        "new", "writer_lock", 9911, None).unwrap());
    let after = continuation_catalog_entry(&supervisor, member);
    assert_eq!(after["observation"]["conversation_id"], id);
    assert_eq!(after["observation"]["continuation_id"], before["observation"]["continuation_id"]);
    assert_eq!(after["observation"]["state"], "thinking");
    assert!(supervisor.apply_tracked_conversation(member, Uuid::new_v4(), generation, &Uuid::new_v4().to_string(),
        "new", "writer_lock", 9911, None).is_err());
    assert_eq!(continuation_catalog_entry(&supervisor, member), after);
    supervisor.shutdown().unwrap();
}

#[test]
fn continuation_upgrade_only_owns_committed_exact_runs() {
    let fixture = upgrade_fixture();
    let run = current_run_id(&fixture.supervisor, "claude");
    let admission = upgrade_admitted(upgrade_request(&fixture));
    assert!(!fixture.supervisor.upgrade_owns_interrupted_run(fixture.claude, run).unwrap());
    fixture.supervisor.cancel_upgrade(admission.attempt_id).unwrap();
    assert!(!fixture.supervisor.upgrade_owns_interrupted_run(fixture.claude, run).unwrap());
    fixture.supervisor.shutdown().unwrap();
    let fixture = upgrade_fixture();
    let member = fixture.claude;
    let run = current_run_id(&fixture.supervisor, "claude");
    let (root, record, _) = upgrade_through_close(fixture);
    let (candidate, _, _) = upgrade_candidate(&root, &record, candidate_identity(&record, 9914));
    assert!(candidate.upgrade_owns_interrupted_run(member, run).unwrap());
    assert!(!candidate.upgrade_owns_interrupted_run(member, Uuid::new_v4()).unwrap());
    candidate.shutdown().unwrap();
}

#[test]
fn continuation_map_is_defaulted_for_old_catalogs_and_rejects_deleted_member_keys() {
    let (_, supervisor, member, _, _) = continuation_fixture(Some(WorkState::Thinking));
    let mut json = serde_json::to_value(&*supervisor.inner.catalog.lock()).unwrap();
    json.as_object_mut().unwrap().remove("recovery_automation");
    let legacy: SessionCatalogV1 = serde_json::from_value(json).unwrap();
    assert!(legacy.recovery_automation.is_empty());
    let mut catalog = supervisor.inner.catalog.lock().clone();
    catalog.sessions.retain(|row| row.session_id != member);
    assert!(recovery_automation::validate_catalog(&catalog).is_err());
    supervisor.shutdown().unwrap();
}

#[test]
fn continuation_working_sequence_changes_do_not_fsync_the_catalog() {
    let (_, supervisor, member, _, _) = continuation_fixture(Some(WorkState::Thinking));
    let before = fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap();
    supervisor.fail_next_catalog_write_for_tests();
    recovery_transition(&supervisor, member, WorkState::ToolCall);
    recovery_transition(&supervisor, member, WorkState::Thinking);
    assert!(supervisor.inner.fail_next_catalog_write.load(Ordering::Acquire), "same eligibility does not attempt persistence");
    assert_eq!(fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap(), before);
    let slots = supervisor.inner.slots.lock();
    let latest = slots.get_by_id(member).unwrap().last_work_state_sequence.unwrap();
    drop(slots);
    let memory = serde_json::to_value(&supervisor.inner.catalog.lock().recovery_automation[&member]).unwrap();
    assert_eq!(memory["observation"]["sequence"], latest);
    recovery_transition(&supervisor, member, WorkState::Blocked);
    assert!(!supervisor.inner.fail_next_catalog_write.load(Ordering::Acquire), "eligibility change does attempt persistence");
    supervisor.shutdown().unwrap();
}

#[test]
fn continuation_manual_start_and_restart_are_not_boot_promotions() {
    for restart in [false, true] {
        let (root, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Thinking));
        let supervisor = reopen_continuation_fixture(&root, supervisor);
        if restart { supervisor.restart_session_by_id(member).unwrap(); }
        else { supervisor.start_session_by_id(member).unwrap(); }
        assert!(continuation_posts(&supervisor, room).is_empty());
        assert!(supervisor.inner.slots.lock().get_by_id(member).unwrap().boot_resume_id.is_none());
        assert!(continuation_catalog_entry(&supervisor, member)["pending"].is_null());
        supervisor.shutdown().unwrap();
    }
}

#[test]
fn continuation_failed_qualifying_start_cannot_leave_ready_for_another_start() {
    let (root, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Thinking));
    let supervisor = reopen_continuation_fixture(&root, supervisor);
    supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(Vec::new())));
    assert!(supervisor.resume_session_after_restart(member).is_err());
    assert!(continuation_catalog_entry(&supervisor, member)["pending"].is_null());
    supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![recording_pty_session(std::process::id()).0])));
    supervisor.start_session_by_id(member).unwrap();
    assert!(continuation_posts(&supervisor, room).is_empty());
    supervisor.shutdown().unwrap();
}

#[test]
fn continuation_preparation_metadata_failure_warns_but_starts_exactly_without_prompt() {
    let (root, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Thinking));
    let supervisor = reopen_continuation_fixture(&root, supervisor);
    let events = capture_runtime_events(&supervisor);
    supervisor.fail_next_catalog_write_for_tests();
    assert!(supervisor.resume_session_after_restart(member).unwrap().running);
    assert!(continuation_posts(&supervisor, room).is_empty());
    assert!(continuation_catalog_entry(&supervisor, member)["pending"].is_null());
    assert!(events.lock().iter().any(|event| matches!(event, RuntimeEvent::SystemLog { message, .. }
        if message.contains("automatic continuation") && message.contains("injected session catalog persistence failure"))));
    supervisor.shutdown().unwrap();
}

#[test]
fn continuation_stop_or_fresh_after_queue_blocks_the_old_intent_at_replay() {
    for fresh in [false, true] {
        let (root, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Thinking));
        let supervisor = reopen_continuation_fixture(&root, supervisor);
        supervisor.resume_session_after_restart(member).unwrap();
        let message = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor: None }).unwrap().events.into_iter()
            .find_map(|event| match event.item { RoomFeedItem::Message { message_id, continuation_origin: Some(_), .. } => Some(message_id), _ => None }).unwrap();
        supervisor.stop_session_by_id(member).unwrap();
        let (next, inputs) = recording_pty_session(std::process::id());
        if fresh {
            supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![next])));
            supervisor.start_session_by_id_with_mode(member, true).unwrap();
        }
        supervisor.resume_room_wake_queue();
        wait_for_room_receipt(&supervisor, room, message, member, RoomDeliveryStatus::Held);
        assert!(inputs.lock().is_empty());
        assert_eq!(continuation_posts(&supervisor, room).len(), 1);
        supervisor.shutdown().unwrap();
    }
}

#[test]
fn continuation_uses_automatic_framing_and_rechecks_stop_before_queued_paste() {
    for stop_before_paste in [false, true] {
        let (root, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Thinking));
        let supervisor = reopen_continuation_fixture(&root, supervisor);
        let (pty, inputs) = recording_pty_session(std::process::id());
        supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![pty])));
        supervisor.resume_session_after_restart(member).unwrap();
        let message = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor: None }).unwrap().events.into_iter()
            .find_map(|event| match event.item { RoomFeedItem::Message { message_id, continuation_origin: Some(_), .. } => Some(message_id), _ => None }).unwrap();
        if stop_before_paste {
            let stop = supervisor.clone();
            supervisor.set_room_wake_before_write_for_tests(move |_| { stop.stop_session_by_id(member).unwrap(); });
        }
        observe_current_run_paste_mode(&supervisor, member, "\x1b[?2004h");
        supervisor.inner.slots.lock().get_by_id_mut(member).unwrap().state = LifecycleState::Ready;
        recovery_transition(&supervisor, member, WorkState::Idle);
        let status = if stop_before_paste { RoomDeliveryStatus::Failed } else { RoomDeliveryStatus::Written };
        wait_for_room_receipt(&supervisor, room, message, member, status);
        wait_for_no_queued_room_wake(&supervisor, member);
        if stop_before_paste {
            assert!(inputs.lock().is_empty(), "Stop after Pending must prevent the actual paste");
        } else {
            // v3 (Claude PRIM-1, 15a49bdf): PRIM-1's own, ACK-tracked, woken by
            // the content-neutral pointer, and open until the member ACKs it.
            let typed = inputs.lock().concat();
            assert!(typed.contains(&format!("new room message for you in room {room} (newest {message}")), "{typed}");
            assert!(!typed.contains("not a new instruction from Victor"), "the body is read, never typed: {typed}");
            assert!(!typed.contains("[PRIM-1 recovery continuation]") && !typed.contains("[Room message from operator]"));
            let item = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor: None }).unwrap().events.into_iter()
                .find_map(|event| match event.item {
                    RoomFeedItem::Message { message_id, sender, ack_requested, .. } if message_id == message => Some((sender, ack_requested)),
                    _ => None }).unwrap();
            assert_eq!(item, (RoomMessageSender::Prim1 {}, true));
            assert!(supervisor.inner.rooms.lock().get(room).unwrap().unread_for(member).iter().any(|event|
                matches!(&event.item, RoomFeedItem::Message { message_id, .. } if *message_id == message)), "open until ACKed");
            recovery_transition(&supervisor, member, WorkState::Thinking);
            assert!(continuation_catalog_entry(&supervisor, member)["pending"].is_null(), "a written receipt resolves the original interruption");
        }
        supervisor.shutdown().unwrap();
    }
}

#[test]
fn continuation_known_membership_race_resolves_without_uncertain_pending() {
    let (root, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Thinking));
    let supervisor = reopen_continuation_fixture(&root, supervisor);
    let codex = test_session_id(&supervisor, "codex");
    let edit = supervisor.clone();
    supervisor.set_room_delivery_after_preflight_for_tests(move || {
        edit.add_room_member(AddRoomMemberRequest { room_id: room, session_id: codex }).unwrap();
    });
    supervisor.resume_session_after_restart(member).unwrap();
    assert!(continuation_posts(&supervisor, room).is_empty());
    assert!(continuation_catalog_entry(&supervisor, member)["pending"].is_null());
    supervisor.shutdown().unwrap();
}

#[test]
fn continuation_uncertain_append_keeps_original_identity_without_reposting_after_boot() {
    let (root, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Thinking));
    let supervisor = reopen_continuation_fixture(&root, supervisor);
    let fault = supervisor.clone();
    supervisor.set_room_delivery_after_preflight_for_tests(move || {
        fault.inner.rooms.lock().get_mut(room).unwrap().fail_next_commit_for_tests(true);
    });
    supervisor.resume_session_after_restart(member).unwrap();
    let original = continuation_catalog_entry(&supervisor, member)["pending"]["interrupted"].clone();
    assert_eq!(continuation_catalog_entry(&supervisor, member)["pending"]["phase"], "posting");
    let supervisor = reopen_continuation_fixture(&root, supervisor);
    supervisor.resume_session_after_restart(member).unwrap();
    assert_eq!(continuation_posts(&supervisor, room).len(), 1);
    assert_eq!(continuation_catalog_entry(&supervisor, member)["pending"]["interrupted"], original);
    supervisor.shutdown().unwrap();
}

#[test]
fn continuation_unknown_upgrade_metadata_warns_without_blocking_boot_or_prompting() {
    let fixture = upgrade_fixture();
    let member = fixture.claude; let room = fixture.room;
    fixture.supervisor.set_room_resume_on_restart(SetRoomResumeRequest { room_id: room, resume_on_restart: true }).unwrap();
    fixture.supervisor.inner.slots.lock().get_by_id_mut(member).unwrap().running_at_shutdown = true;
    fixture.supervisor.persist_slot_harness_state(member);
    recovery_transition(&fixture.supervisor, member, WorkState::Thinking);
    let supervisor = reopen_continuation_fixture(&fixture.root, fixture.supervisor);
    supervisor.install_upgrade(upgrade_flow::UpgradeEnvironment { profile_dir: fixture.root.join("profile"),
        personal_repo_override: Some("off".into()), startup: Ok(None) }, fixture.host).unwrap();
    fs::write(supervisor.runtime_dir().join("maintenance/upgrades/current.json"), b"broken fixture pointer").unwrap();
    let events = capture_runtime_events(&supervisor);
    assert!(supervisor.resume_session_after_restart(member).unwrap().running);
    assert!(continuation_posts(&supervisor, room).is_empty());
    assert!(events.lock().iter().any(|event| matches!(event, RuntimeEvent::SystemLog { message, .. }
        if message.contains("automatic continuation"))));
    supervisor.shutdown().unwrap();
}

#[test]
fn continuation_retained_queue_waits_through_boot_admission_and_ready_installation() {
    let (root, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Thinking));
    let supervisor = reopen_continuation_fixture(&root, supervisor);
    supervisor.resume_session_after_restart(member).unwrap();
    let original = continuation_posts(&supervisor, room);
    let supervisor = reopen_continuation_fixture(&root, supervisor);
    assert!(!supervisor.queued_room_wake_pass_for_tests(member), "the real replay queue waits before boot");
    let (pty, inputs) = recording_pty_session(std::process::id());
    supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![pty])));
    let boundaries = Arc::new(Mutex::new(Vec::new()));
    let seen = boundaries.clone(); let check = supervisor.clone();
    *supervisor.inner.recovery_start_boundary.lock() = Some(Box::new(move |boundary| {
        let state = check.inner.slots.lock().get_by_id(member).unwrap().state;
        assert_eq!(state, if boundary == "admitted" { LifecycleState::Starting } else { LifecycleState::Ready });
        assert!(!check.queued_room_wake_pass_for_tests(member), "the real queue must keep waiting at {boundary}");
        assert!(room_receipt_statuses(&check, room, member).is_empty());
        seen.lock().push(boundary);
    }));
    supervisor.resume_session_after_restart(member).unwrap();
    supervisor.inner.recovery_start_boundary.lock().take();
    assert_eq!(*boundaries.lock(), vec!["admitted", "installed"]);
    observe_current_run_paste_mode(&supervisor, member, "\x1b[?2004h");
    recovery_transition(&supervisor, member, WorkState::Idle);
    wait_for_no_queued_room_wake(&supervisor, member);
    assert_eq!(room_receipt_statuses(&supervisor, room, member), vec![RoomDeliveryStatus::Pending, RoomDeliveryStatus::Written]);
    assert_eq!(inputs.lock().concat().matches("new room message").count(), 1);
    assert_eq!(continuation_posts(&supervisor, room), original);
    supervisor.shutdown().unwrap();
}

#[test]
fn continuation_same_run_w2_conversation_change_holds_old_queued_intent() {
    let (root, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Thinking));
    let supervisor = reopen_continuation_fixture(&root, supervisor);
    let (pty, inputs) = recording_pty_session(std::process::id());
    supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![pty])));
    supervisor.resume_session_after_restart(member).unwrap();
    let (run, generation, resume_id) = {
        let slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id(member).unwrap();
        (slot.run_id.unwrap(), slot.generation, slot.run_resume_id.clone())
    };
    assert!(supervisor.apply_tracked_conversation(member, run, generation, &Uuid::new_v4().to_string(),
        "new", "writer_lock", std::process::id(), None).unwrap());
    assert_eq!(supervisor.inner.slots.lock().get_by_id(member).unwrap().run_resume_id, resume_id,
        "the launch identity remains old while W2 changes the current conversation");
    observe_current_run_paste_mode(&supervisor, member, "\x1b[?2004h");
    recovery_transition(&supervisor, member, WorkState::Idle);
    wait_for_no_queued_room_wake(&supervisor, member);
    assert_eq!(room_receipt_statuses(&supervisor, room, member), vec![RoomDeliveryStatus::Held]);
    assert!(inputs.lock().is_empty());
    supervisor.shutdown().unwrap();
}

#[test]
fn continuation_upgrade_ownership_survives_eviction_but_resolved_resumed_work_can_continue() {
    let fixture = upgrade_fixture();
    let member = fixture.claude; let room = fixture.room;
    fixture.supervisor.set_room_resume_on_restart(SetRoomResumeRequest { room_id: room, resume_on_restart: true }).unwrap();
    recovery_transition(&fixture.supervisor, member, WorkState::Thinking);
    let original_run = current_run_id(&fixture.supervisor, "claude");
    let (root, record, _) = upgrade_through_close(fixture);
    let identity = candidate_identity(&record, 9925);
    upgrade_helper_result(&record, upgrade::HelperOutcome::Launched, Some(identity.clone()));
    let (candidate, _, _) = upgrade_candidate(&root, &record, identity);
    candidate.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![recording_pty_session(std::process::id()).0])));
    candidate.begin_upgrade_resume();
    let resumed_run = current_run_id(&candidate, "claude");
    assert!(candidate.upgrade_owns_interrupted_run(member, original_run).unwrap());
    assert!(candidate.upgrade_owns_interrupted_run(member, resumed_run).unwrap(), "queued upgrade continuation still owns this interruption");
    let marker = continuation_catalog_entry(&candidate, member)["upgrade"].clone();
    assert_eq!(marker["resumed_run"], resumed_run.to_string());
    assert_eq!(marker["resolved"], false);
    let pid = candidate.inner.slots.lock().get_by_id(member).unwrap().process_id.unwrap();
    let caller = candidate.resolve_pane_caller(TestPaneProcess::new(pid + 10_000, [pid])).unwrap();
    candidate.read_room_feed_from_pane(&caller, None).unwrap();
    assert_eq!(continuation_catalog_entry(&candidate, member)["upgrade"]["resolved"], true,
        "the committed Fetched receipt durably ends resumed-run ownership");
    assert!(!candidate.upgrade_owns_interrupted_run(member, resumed_run).unwrap());
    let request_id = format!("upgrade-{}", record.request.members[0].continuation_id);
    // v3 (Claude PRIM-1, 15a49bdf): an unacknowledged continuation is owed and
    // pinned; once the member ACKs it, it can leave the index like any post.
    let continuation = candidate.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor: None }).unwrap().events.into_iter()
        .find_map(|event| match event.item {
            RoomFeedItem::Message { message_id, request_id: id, .. } if id == request_id => Some(message_id),
            _ => None }).unwrap();
    let acked = candidate.apply_sideband_request(&caller, shared_types::SidebandRequest::RoomAck {
        request_id: Some("ack-upgrade-continuation".into()), message_id: continuation,
        next_action: Some("continuing".into()), decline: None, outcome_ref: None });
    assert!(acked.ok, "{}", acked.message);
    // The retained-post index holds 1024 posts (the feed window only 512):
    // post past the index bound so the continuation post is really evicted.
    for index in 0..1100 {
        candidate.post_room_message(PostRoomMessageRequest { room_id: room, request_id: None,
            content: format!("eviction fixture {index}") }).unwrap();
    }
    assert!(candidate.inner.rooms.lock().get(room).unwrap().retained_delivery(
        &RoomMessageSender::Prim1 {}, &request_id).is_none());
    assert!(candidate.upgrade_owns_interrupted_run(member, original_run).unwrap(), "eviction does not forget original ownership");
    recovery_transition(&candidate, member, WorkState::Thinking);
    let candidate = reopen_continuation_fixture(&root, candidate);
    assert!(candidate.upgrade_owns_interrupted_run(member, original_run).unwrap(), "ownership survives catalog reload without the upgrade flow");
    candidate.resume_session_after_restart(member).unwrap();
    assert_eq!(continuation_posts(&candidate, room).len(), 1, "later genuine work gets one ordinary continuation");
    candidate.shutdown().unwrap();
}

#[test]
fn continuation_consumed_upgrade_does_not_claim_a_member_that_failed_to_resume() {
    let fixture = upgrade_fixture(); let member = fixture.claude;
    let original_run = current_run_id(&fixture.supervisor, "claude");
    let (root, record, _) = upgrade_through_close(fixture);
    let identity = candidate_identity(&record, 9926);
    upgrade_helper_result(&record, upgrade::HelperOutcome::Launched, Some(identity.clone()));
    let (candidate, _, _) = upgrade_candidate(&root, &record, identity);
    candidate.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(Vec::new())));
    candidate.begin_upgrade_resume();
    assert_eq!(candidate.upgrade_record_for_tests(record.request.attempt_id).state.phase, upgrade::UpgradePhase::Consumed);
    assert!(!candidate.upgrade_owns_interrupted_run(member, original_run).unwrap());
    assert!(continuation_catalog_entry(&candidate, member)["upgrade"].is_null());
    candidate.shutdown().unwrap();
}

#[test]
fn continuation_older_than_twelve_hours_resumes_original_intent_and_counts_the_old_ordinary_message_never_typing_it() {
    let (root, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Thinking));
    let supervisor = reopen_continuation_fixture(&root, supervisor);
    supervisor.resume_session_after_restart(member).unwrap();
    let original = continuation_posts(&supervisor, room);
    let ordinary = deliver_to(&supervisor, room, member, "old ordinary fixture message");
    assert_eq!(ordinary.receipts[0].status, RoomDeliveryStatus::Stored);
    let log = supervisor.runtime_dir().join("rooms").join(format!("{room}.jsonl"));
    supervisor.shutdown().unwrap();
    let weak = Arc::downgrade(&supervisor.inner);
    drop(supervisor);
    wait_until("closed fixture releases its journal", || weak.strong_count() == 0);
    // Only a closed disposable fixture is backdated; production clocks/limits
    // are unchanged. The journal keeps the actual original message IDs.
    let old_timestamp = (chrono::Utc::now() - chrono::Duration::hours(13)).to_rfc3339();
    let records = fs::read_to_string(&log).unwrap().lines().map(|line| {
        let mut record: serde_json::Value = serde_json::from_str(line).unwrap();
        if record["event"]["item"]["kind"] == "message" {
            record["event"]["timestamp"] = old_timestamp.clone().into();
        }
        serde_json::to_string(&record).unwrap()
    }).collect::<Vec<_>>().join("\n");
    fs::write(&log, format!("{records}\n")).unwrap();
    let supervisor = SupervisorHandle::new(test_supervisor_config(&root)).unwrap();
    supervisor.set_exit_watch_enabled_for_tests(false);
    supervisor.set_executable_resolver_for_tests(Arc::new(TestExecutableResolver));
    let (pty, inputs) = recording_pty_session(std::process::id());
    supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![pty])));
    assert_eq!(supervisor.resume_room_wake_queue(), 1, "only typed recovery bypasses the old-message cutoff");
    assert!(!supervisor.queued_room_wake_pass_for_tests(member), "wait for boot rather than expiring immediately");
    supervisor.resume_session_after_restart(member).unwrap();
    observe_current_run_paste_mode(&supervisor, member, "\x1b[?2004h");
    recovery_transition(&supervisor, member, WorkState::Idle);
    wait_for_no_queued_room_wake(&supervisor, member);
    assert_eq!(continuation_posts(&supervisor, room), original);
    let page = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor: None }).unwrap();
    assert!(page.events.iter().any(|event| event.timestamp == old_timestamp
        && matches!(&event.item, RoomFeedItem::Message { request_id, .. } if request_id == &original[0].0)));
    // v3 (Claude PRIM-1, 15a49bdf): the continuation's pointer counts every
    // unread message, so the old one is counted in that one pointer; its
    // body is never typed.
    let typed = inputs.lock().concat();
    assert_eq!(typed.matches("new room message").count(), 1, "{typed}");
    assert!(typed.contains("2 new room messages for you"), "{typed}");
    assert!(!typed.contains("old ordinary fixture message"));
    supervisor.shutdown().unwrap();
}

#[test]
fn continuation_newer_working_interruption_supersedes_unknown_old_delivery_without_retrying_it() {
    for phase in ["posting", "queued"] {
        for case in ["newer_working", "same_run", "idle", "wrong_id", "upgrade_owned"] {
            let (root, supervisor, member, room, original_run) = continuation_fixture(Some(WorkState::Thinking));
            let supervisor = reopen_continuation_fixture(&root, supervisor);
            supervisor.resume_session_after_restart(member).unwrap();
            let old = continuation_posts(&supervisor, room)[0].0.clone();
            let old_delivery = supervisor.inner.rooms.lock().get(room).unwrap()
                .retained_delivery(&RoomMessageSender::Prim1 {}, &old).unwrap();
            let (newer_run, generation) = {
                let slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id(member).unwrap();
                (slot.run_id.unwrap(), slot.generation)
            };
            // A committed Pending without a completion is unknown, not proof
            // that bytes were written or that repeating them is safe.
            supervisor.inner.rooms.lock().get_mut(room).unwrap().append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, readmit: false, pointer: None,
                message_id: old_delivery.message_id, recipient_id: member, status: RoomDeliveryStatus::Pending,
                bytes_written: 0, zero_write_confirmed: false, error: None,
                run_id: Some(newer_run), generation: Some(generation),
            }).unwrap();
            recovery_transition(&supervisor, member, if case == "idle" { WorkState::Idle } else { WorkState::Thinking });
            let newer = continuation_catalog_entry(&supervisor, member)["observation"].clone();
            {
                // Exercise persisted boundary records without executing a
                // provider: same interrupted run, mismatched ID, or known
                // upgrade ownership must not mint another continuation.
                let mut catalog = supervisor.inner.catalog.lock();
                let mut entry = serde_json::to_value(&catalog.recovery_automation[&member]).unwrap();
                entry["pending"]["phase"] = phase.into();
                match case {
                    "same_run" => entry["observation"]["run_id"] = original_run.to_string().into(),
                    "wrong_id" => entry["observation"]["conversation_id"] = Uuid::new_v4().to_string().into(),
                    "upgrade_owned" => entry["upgrade"] = serde_json::json!({
                        "interrupted_run": newer_run, "resumed_run": Uuid::new_v4(), "room_id": room,
                        "continuation_id": Uuid::new_v4(), "conversation_id": newer["conversation_id"], "resolved": true,
                    }),
                    _ => {}
                }
                catalog.recovery_automation.insert(member, serde_json::from_value(entry).unwrap());
                supervisor.persist_catalog_candidate(&catalog).unwrap();
            }
            let supervisor = reopen_continuation_fixture(&root, supervisor);
            let (pty, inputs) = recording_pty_session(std::process::id());
            supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![pty])));
            supervisor.resume_session_after_restart(member).unwrap();
            let pending = continuation_catalog_entry(&supervisor, member)["pending"].clone();
            if case == "newer_working" {
                assert_eq!(pending["interrupted"]["run_id"], newer_run.to_string());
                assert_eq!(pending["interrupted"]["continuation_id"], newer["continuation_id"]);
                assert_ne!(format!("continuation-{}", pending["interrupted"]["continuation_id"].as_str().unwrap()), old);
                assert_eq!(continuation_posts(&supervisor, room).len(), 2);
                observe_current_run_paste_mode(&supervisor, member, "\x1b[?2004h");
                recovery_transition(&supervisor, member, WorkState::Idle);
                wait_for_no_queued_room_wake(&supervisor, member);
                // Claude PRIM-1 (ac9f860 walk): PRIM-1 disposed of the older
                // continuation, "superseded by" the newer; one pointer to the
                // newer alone, never the old body again.
                let events = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor: None }).unwrap().events;
                let ids: Vec<(String, Uuid)> = events.iter().filter_map(|event| match &event.item {
                    RoomFeedItem::Message { request_id, message_id, .. } if request_id.starts_with("continuation-") =>
                        Some((request_id.clone(), *message_id)),
                    _ => None }).collect();
                let old_id = ids.iter().find(|(id, _)| *id == old).unwrap().1;
                let new_id = ids.iter().find(|(id, _)| *id != old).unwrap().1;
                let disposal = events.iter().find_map(|event| match &event.item {
                    RoomFeedItem::Ack { message_id, recipient_id, actor, disposition, note, .. } if *message_id == old_id =>
                        Some((*recipient_id, actor.clone(), *disposition, note.clone())),
                    _ => None }).expect("the older continuation is disposed");
                assert_eq!(disposal, (member, shared_types::RoomAckActor::Prim1 {}, shared_types::RoomAckDisposition::Declined,
                    format!("superseded by {new_id}")));
                let typed = inputs.lock().concat();
                assert_eq!(typed.matches("new room message").count(), 1, "{typed}");
                assert!(typed.contains(&format!("1 new room message for you in room {room} (newest {new_id}")), "{typed}");
            } else {
                assert_eq!(pending["interrupted"]["run_id"], original_run.to_string(), "{phase}/{case}");
                assert_eq!(continuation_posts(&supervisor, room).len(), 1, "{phase}/{case}");
                assert!(inputs.lock().is_empty());
            }
            let retained = supervisor.inner.rooms.lock().get(room).unwrap()
                .retained_delivery(&RoomMessageSender::Prim1 {}, &old).unwrap();
            assert_eq!(retained.message_id, old_delivery.message_id);
            assert_eq!(retained.receipts[0].status, RoomDeliveryStatus::Pending, "old uncertainty is preserved, never retried");
            supervisor.shutdown().unwrap();
        }
    }
}

// F2: the agent exits while a process it started (a tool call, a server) keeps
// the Job and the terminal alive. The mock scope never reports exited, so only
// the drain grace can retire the run; its kill is counted, never real. The
// returned liveness handle lets a test change what the exact-Job check reports.
const ROOT: u32 = u32::MAX; // a PID no live process has
fn drain_fixture(supervisor: &SupervisorHandle, member: SessionId, root_pid: Option<u32>,
    root_status: Option<pty_host::PtyExitStatus>, liveness: AgentLiveness, kill: MockKillBehavior)
    -> (Arc<AtomicUsize>, Arc<Mutex<AgentLiveness>>) {
    let kills = Arc::new(AtomicUsize::new(0));
    let liveness = Arc::new(Mutex::new(liveness));
    let pty = MockPtySession { process_id: root_pid, exit_status: root_status, send_input_count: Arc::new(AtomicUsize::new(0)),
        kill_count: kills.clone(), kill_behavior: kill, agent_liveness: liveness.clone(), on_send: None };
    {
        let mut slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id_mut(member).unwrap();
        slot.running.as_mut().unwrap().pty = Some(Arc::new(pty));
        slot.process_id = root_pid;
    }
    enable_output_credit_for_current_run(supervisor, "claude");
    (kills, liveness)
}

fn exited(last_agent_pids: Vec<u32>, live: Vec<(u32, Option<&str>)>) -> AgentLiveness {
    AgentLiveness::Exited { last_agent_pids, live_processes: live.into_iter()
        .map(|(process_id, image)| pty_host::ProcessIdentity { process_id, image_name: image.map(str::to_owned) }).collect() }
}

#[test]
fn drain_grace_ends_processes_left_by_an_exited_agent_and_classifies_only_the_agents_own_status() {
    let tool = vec![(4242, Some("powershell.exe"))];
    let node_server = AgentLiveness::Alive(vec![pty_host::ProcessIdentity { process_id: 4243, image_name: Some("node.exe".into()) }]);
    const NATIVE: &str = r"C:\Users\me\.local\bin\claude.exe";
    const NODE_SHIM: &str = r"C:\Program Files\nodejs\node.exe";
    // (case, spawned program, root PID, root status before the kill, exact-Job report, crash recovery armed)
    for (case, program, root_pid, status, liveness, crash) in [
        // The root was the agent's native executable: its exit code is the agent's.
        ("crash", Some(NATIVE), Some(ROOT), Some(pty_exit_status(3, None, false)), exited(vec![ROOT], tool.clone()), true),
        ("clean", Some(NATIVE), Some(ROOT), Some(pty_exit_status(0, None, true)), exited(vec![ROOT], tool.clone()), false),
        // A Node launcher is recorded among the agent PIDs too, but its code is not
        // the agent's: an exited Node shim is unknown, never a crash.
        ("node_shim_recorded", Some(NODE_SHIM), Some(ROOT), Some(pty_exit_status(3, None, false)), exited(vec![ROOT], tool.clone()), false),
        // A native claude.exe crashed and left a Node dev server, which the Job
        // check counts as an agent: the native root's own exit still proves it.
        ("native_root_live_node", Some(NATIVE), Some(ROOT), Some(pty_exit_status(3, None, false)), node_server.clone(), true),
        // A dead wrapper's code is not the agent's: unknown, never a crash.
        ("dead_wrapper", None, Some(ROOT), Some(pty_exit_status(3, None, false)), exited(vec![777], tool.clone()), false),
        // A live wrapper around an exited agent: unknown, and the killed wrapper's status is ignored.
        ("live_wrapper", None, None, Some(pty_exit_status(3, None, false)), exited(vec![777], tool.clone()), false),
    ] {
        let (_, supervisor, member, _, run) = continuation_fixture(Some(WorkState::Thinking));
        let (kills, _) = drain_fixture(&supervisor, member, root_pid, status, liveness, MockKillBehavior::Immediate);
        supervisor.inner.slots.lock().get_by_id_mut(member).unwrap().run_program = program.map(PathBuf::from);
        supervisor.refresh_session_liveness();
        supervisor.refresh_session_liveness();
        {
            let slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id(member).unwrap();
            assert!(slot.output_draining && slot.running.is_some() && slot.agent_exited_at.is_some(), "{case}");
        }
        assert_eq!(kills.load(Ordering::SeqCst), 0, "{case}: nothing is ended inside the grace");
        let events = capture_runtime_events(&supervisor);
        supervisor.set_output_drain_grace_for_tests(Duration::ZERO);
        supervisor.refresh_session_liveness();
        assert_eq!(kills.load(Ordering::SeqCst), 1, "{case}: the whole scope is ended once after the grace");
        {
            let slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id(member).unwrap();
            assert!(slot.running.is_none() && !slot.output_draining && slot.agent_exited_at.is_none(), "{case}");
        }
        assert!(events.lock().iter().any(|event| matches!(event, RuntimeEvent::SystemLog { level: LogLevel::Warn, message, .. }
            if message.contains("processes it started still held the terminal"))), "{case}: the operator is told why");
        let waiting = continuation_catalog_entry(&supervisor, member)["crash_waiting"].clone();
        if crash { assert_eq!(waiting, run.to_string(), "{case}"); } else { assert!(waiting.is_null(), "{case}: {waiting}"); }
        supervisor.refresh_session_liveness();
        assert_eq!(kills.load(Ordering::SeqCst), 1, "{case}: a retired run is never ended again");
        supervisor.shutdown().unwrap();
    }
}

#[test]
fn drain_grace_never_ends_a_run_whose_agent_exit_is_unproven_or_that_is_not_a_hosted_agent() {
    for case in ["not_yet_observed", "unreadable_image", "agent_pid_alive", "terminal", "prime"] {
        let (_, supervisor, member, _, _) = continuation_fixture(Some(WorkState::Idle));
        let liveness = match case {
            "not_yet_observed" => AgentLiveness::NotYetObserved(Vec::new()),
            "unreadable_image" => exited(vec![ROOT], vec![(4242, None)]),
            "agent_pid_alive" => exited(vec![4242], vec![(4242, Some("claude.exe"))]),
            _ => exited(vec![ROOT], vec![(4242, Some("node.exe"))]),
        };
        let (kills, _) = drain_fixture(&supervisor, member, Some(ROOT), Some(pty_exit_status(3, None, false)),
            liveness, MockKillBehavior::Immediate);
        if let Some(driver) = match case { "terminal" => Some(DriverKind::GenericTerminal), "prime" => Some(DriverKind::Prime), _ => None } {
            supervisor.inner.slots.lock().get_by_id_mut(member).unwrap().definition.driver = driver;
        }
        supervisor.set_output_drain_grace_for_tests(Duration::ZERO);
        for _ in 0..3 { supervisor.refresh_session_liveness(); }
        assert_eq!(kills.load(Ordering::SeqCst), 0, "{case}: an unproven exit or a terminal's children are never ended");
        let slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id(member).unwrap();
        assert!(slot.running.is_some() && slot.output_draining && slot.agent_exited_at.is_none(),
            "{case}: the run keeps draining; Stop remains available");
        drop(slots);
        supervisor.shutdown().unwrap();
    }
}

#[test]
fn drain_grace_keeps_a_live_agent_whose_wrapper_exited_and_counts_from_its_own_later_exit() {
    let (_, supervisor, member, _, _) = continuation_fixture(Some(WorkState::Idle));
    // The npm shim (a wrapper, not the agent) exited; its claude.exe child lives on.
    let (kills, liveness) = drain_fixture(&supervisor, member, Some(ROOT), Some(pty_exit_status(0, None, true)),
        AgentLiveness::Alive(vec![pty_host::ProcessIdentity { process_id: 4242, image_name: Some("claude.exe".into()) }]),
        MockKillBehavior::Immediate);
    supervisor.inner.slots.lock().get_by_id_mut(member).unwrap().run_program =
        Some(PathBuf::from(r"C:\Users\me\AppData\Roaming\npm\claude.cmd"));
    for _ in 0..3 { supervisor.refresh_session_liveness(); }
    {
        let slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id(member).unwrap();
        assert!(slot.running.is_some() && !slot.output_draining && slot.agent_exited_at.is_none(),
            "only the wrapper exited: the agent keeps running");
    }
    assert!(run_input_gate(&supervisor, member).is_accepting(), "and keeps its input");
    // The agent exits much later; its own exit starts the grace, not the wrapper's.
    *liveness.lock() = exited(vec![4242], vec![(5151, Some("cargo.exe"))]);
    supervisor.refresh_session_liveness();
    {
        let slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id(member).unwrap();
        assert!(slot.output_draining && slot.agent_exited_at.is_some());
    }
    supervisor.refresh_session_liveness();
    assert_eq!(kills.load(Ordering::SeqCst), 0, "a full grace follows the agent's own exit");
    supervisor.set_output_drain_grace_for_tests(Duration::ZERO);
    supervisor.refresh_session_liveness();
    assert_eq!(kills.load(Ordering::SeqCst), 1);
    assert!(continuation_catalog_entry(&supervisor, member)["crash_waiting"].is_null(),
        "the wrapper's PID was never the agent's: its status is unknown, so no automatic resume");
    supervisor.shutdown().unwrap();
}

#[test]
fn drain_grace_releases_a_producer_blocked_on_output_credit_before_the_scope_kill() {
    let (_, supervisor, member, _, _) = continuation_fixture(Some(WorkState::Thinking));
    // The kill outlives the assertion window below; without the exact-run credit
    // cancellation first, the blocked producer would wait for the whole kill.
    let (kills, _) = drain_fixture(&supervisor, member, Some(ROOT), Some(pty_exit_status(3, None, false)),
        exited(vec![ROOT], vec![(4242, Some("powershell.exe"))]), MockKillBehavior::Sleep(Duration::from_secs(4)));
    supervisor.set_stop_kill_timeout_for_tests(Duration::from_secs(10));
    let run = {
        let slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id(member).unwrap();
        output_credit::OutputRun { session_id: member, run_id: slot.run_id.unwrap(), generation: slot.generation }
    };
    let ledger = supervisor.output_credit();
    let leases = (0..4).map(|_| match ledger.reserve(run, output_credit::MAX_CHUNK_BYTES - output_credit::DISPLAY_OVERHEAD) {
        output_credit::Admission::Reserved(lease) => lease,
        _ => panic!("available credit"),
    }).collect::<Vec<_>>();
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let producer = supervisor.clone();
    let reader = thread::spawn(move || {
        producer.handle_pty_event_by_id(run.session_id, run.generation, run.run_id, PtyEvent::Output("tool output".into()));
        done_tx.send(()).unwrap();
    });
    assert!(ledger.wait_for_blocked_producer(Duration::from_secs(5)));
    supervisor.refresh_session_liveness();
    supervisor.refresh_session_liveness();
    assert_eq!(kills.load(Ordering::SeqCst), 0);
    supervisor.set_output_drain_grace_for_tests(Duration::ZERO);
    let sweeper = supervisor.clone();
    let sweep = thread::spawn(move || sweeper.refresh_session_liveness());
    assert!(done_rx.recv_timeout(Duration::from_secs(2)).is_ok(), "the producer is released before the kill finishes");
    sweep.join().unwrap();
    reader.join().unwrap();
    assert_eq!(kills.load(Ordering::SeqCst), 1);
    assert!(supervisor.inner.slots.lock().get_by_id(member).unwrap().running.is_none());
    drop(leases);
    supervisor.shutdown().unwrap();
}

/// Reviewer b307957c: a current continuation that another request's pointer
/// counts is covered by that pointer too, so its own queued wake is moot and
/// never types a second pointer; the cover's Written resolves it. The request
/// heads the member's queue; its pointer counts the newer continuation as the
/// newest unread message.
#[test]
fn v3_a_current_continuation_counted_by_another_pointer_is_covered_by_it() {
    let (root, supervisor, member, room, _) = continuation_fixture(Some(WorkState::Thinking));
    let supervisor = reopen_continuation_fixture(&root, supervisor);
    let (pty, inputs) = recording_pty_session(std::process::id());
    supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![pty])));
    supervisor.hold_room_wake_dispatch_for_tests(member);
    let request = deliver_to(&supervisor, room, member, "ordinary request before the continuation");
    assert_eq!(request.receipts[0].status, RoomDeliveryStatus::Stored);
    supervisor.resume_session_after_restart(member).unwrap();
    let continuation = room_message_ids(&supervisor, room, |item| matches!(item,
        RoomFeedItem::Message { continuation_origin: Some(_), .. }));
    assert_eq!(continuation.len(), 1);
    observe_current_run_paste_mode(&supervisor, member, "\x1b[?2004h");
    supervisor.inner.slots.lock().get_by_id_mut(member).unwrap().state = LifecycleState::Ready;
    recovery_transition(&supervisor, member, WorkState::Idle);
    let head = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor: None }).unwrap().events.into_iter()
        .find(|event| matches!(event.item, RoomFeedItem::Message { message_id, .. } if message_id == request.message_id))
        .unwrap();
    let outcome = supervisor.wake_with_stale_plan_for_tests(&head, member, || {}).unwrap().expect("the request was attempted");
    assert_eq!(outcome.status, RoomDeliveryStatus::Written, "{outcome:?}");
    let covered = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor: None }).unwrap().events.iter()
        .any(|event| matches!(event.item, RoomFeedItem::Delivery { message_id, recipient_id, pointer: Some(pointer), .. }
            if message_id == continuation[0] && recipient_id == member && pointer == request.message_id));
    assert!(covered, "the counted continuation carries the pointer's cover receipt");
    supervisor.release_room_wake_dispatch_for_tests(member);
    wait_for_no_queued_room_wake(&supervisor, member);
    let typed = inputs.lock().concat();
    assert_eq!(typed.matches("new room message").count(), 1, "its own queued wake typed nothing: {typed}");
    assert!(typed.contains(&format!("2 new room messages for you in room {room} (newest {}", continuation[0])), "{typed}");
    recovery_transition(&supervisor, member, WorkState::Thinking);
    assert!(continuation_catalog_entry(&supervisor, member)["pending"].is_null(), "the cover's Written resolves it");
    supervisor.shutdown().unwrap();
}

fn room_message_ids(supervisor: &SupervisorHandle, room: RoomId, wanted: impl Fn(&RoomFeedItem) -> bool) -> Vec<Uuid> {
    supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor: None }).unwrap().events.into_iter()
        .filter(|event| wanted(&event.item))
        .filter_map(|event| match event.item { RoomFeedItem::Message { message_id, .. } => Some(message_id), _ => None })
        .collect()
}
