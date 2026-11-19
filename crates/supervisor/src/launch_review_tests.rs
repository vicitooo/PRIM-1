// Included in the existing supervisor test module to reuse its isolated PTY
// fixtures. No installed harness, desktop, configuration or pipe is accessed.

#[test]
fn s5_review_graceful_shutdown_checkpoints_latest_sizes_without_resize_writes() {
    let supervisor = test_supervisor();
    let id = test_session_id(&supervisor, "claude");
    let (pty, _) = recording_pty_session(std::process::id());
    let (spawner, _) = CapturingPtySpawner::new(vec![pty]);
    supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
    supervisor.start_session_by_id(id).unwrap();
    let before = fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap();
    supervisor.resize_session_by_id(id, 97, 41).unwrap();
    assert_eq!(
        fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap(),
        before
    );
    supervisor.fail_next_catalog_write_for_tests();
    supervisor
        .shutdown()
        .expect("size metadata failure must not block proved process shutdown");
    assert!(
        fs::read_to_string(supervisor.audit_log_path())
            .unwrap()
            .contains("previous dimensions retained")
    );
    assert_eq!(
        fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap(),
        before
    );
    supervisor.shutdown().unwrap();
    let row = read_session_catalog(&session_catalog_path(supervisor.runtime_dir()))
        .unwrap()
        .sessions
        .into_iter()
        .find(|row| row.session_id == id)
        .unwrap();
    assert_eq!((row.last_pty_cols, row.last_pty_rows), (Some(97), Some(41)));
}

#[test]
fn s5_review_identify_dispatch_uses_root_fixture_and_audits_only_accepted_metadata() {
    let root = tempfile::tempdir().unwrap();
    let supervisor = test_supervisor_with_root(root.path().join("fixture"));
    let (pty, _) = recording_pty_session(102);
    install_mock_running_session_with_process_id(
        &supervisor,
        "codex",
        DriverKind::Codex,
        Some(102),
        pty,
    );
    let caller = supervisor
        .resolve_pane_caller(TestPaneProcess::new(7001, [102]))
        .unwrap();
    let cwd = supervisor
        .inner
        .slots
        .lock()
        .get_by_id(caller.session_id)
        .unwrap()
        .definition
        .working_dir
        .clone();
    let sessions = root.path().join("isolated-codex-sessions");
    let thread_id = Uuid::new_v4().to_string();
    session::write_codex_fixture(&sessions, &thread_id, &cwd);
    *supervisor.inner.codex_identity_root_for_tests.lock() = Some(sessions);
    let response = supervisor.apply_sideband_request(
        &caller,
        SidebandRequest::IdentifyCodex {
            thread_id: thread_id.clone(),
        },
    );
    assert!(response.ok, "{}", response.message);
    let audit = fs::read_to_string(supervisor.audit_log_path()).unwrap();
    assert!(audit.contains("Codex identity report stored"));
    assert!(audit.contains(&caller.run_id.to_string()));
    assert!(!audit.contains(&thread_id));
    assert!(!audit.contains("isolated-codex-sessions"));
    assert!(
        supervisor
            .codex_native_wake_identity(caller.session_id, caller.run_id, caller.generation)
            .is_err()
    );
    let disk_before = fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap();
    let audit_before = fs::read(supervisor.audit_log_path()).unwrap();
    supervisor.fail_next_catalog_write_for_tests();
    let response =
        supervisor.apply_sideband_request(&caller, SidebandRequest::IdentifyCodex { thread_id });
    assert!(!response.ok);
    assert_eq!(
        fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap(),
        disk_before
    );
    assert_eq!(fs::read(supervisor.audit_log_path()).unwrap(), audit_before);
}

#[test]
fn s5_review_restart_policy_event_follows_successful_persistence_only() {
    let supervisor = test_supervisor();
    let room = supervisor
        .create_room(CreateRoomRequest {
            label: None,
            member_ids: Vec::new(),
            brief_on_join: false,
            brief_template: None,
        })
        .unwrap();
    supervisor
        .set_room_resume_on_restart(SetRoomResumeRequest {
            room_id: room.room_id,
            resume_on_restart: true,
        })
        .unwrap();
    let audit = fs::read_to_string(supervisor.audit_log_path()).unwrap();
    assert!(audit.contains("Room restart selection changed"));
    assert!(audit.contains("resume_on_restart=true"));
    let before = fs::read(room_catalog_path(supervisor.runtime_dir())).unwrap();
    supervisor
        .inner
        .fail_next_room_catalog_write
        .store(true, Ordering::SeqCst);
    assert!(
        supervisor
            .set_room_resume_on_restart(SetRoomResumeRequest {
                room_id: room.room_id,
                resume_on_restart: false
            })
            .is_err()
    );
    assert_eq!(
        fs::read(room_catalog_path(supervisor.runtime_dir())).unwrap(),
        before
    );
    assert_eq!(
        fs::read_to_string(supervisor.audit_log_path()).unwrap(),
        audit
    );
}

#[test]
fn s5_review_initial_brief_never_reappears_on_resume_or_restart() {
    for name in ["claude", "grok"] {
        let supervisor = test_supervisor();
        if name == "grok" {
            create_test_session(
                &supervisor,
                "grok",
                DriverKind::Grok,
                shared_types::PermissionProfile::Normal,
            );
        }
        let id = test_session_id(&supervisor, name);
        let ptys = (0..4)
            .map(|_| recording_pty_session(std::process::id()).0)
            .collect();
        let (spawner, specs) = CapturingPtySpawner::new(ptys);
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
        let choices = SessionLaunchChoices {
            brief: SessionBriefChoice::Custom {
                text: "update".into(),
            },
            ..SessionLaunchChoices::default()
        };
        supervisor
            .start_session_with_launch_choices(id, false, false, Some(choices))
            .unwrap();
        supervisor.stop_session_by_id(id).unwrap();
        supervisor.start_session_by_id(id).unwrap();
        supervisor.restart_session_by_id(id).unwrap();
        supervisor.stop_session_by_id(id).unwrap();
        supervisor
            .start_session_by_id_with_options(id, true, false)
            .unwrap();
        let specs = specs.lock();
        assert_eq!(specs.len(), 4);
        for index in [0, 3] {
            let expected = if name == "claude" {
                "PRIM-1 initial brief:\n\nupdate"
            } else {
                "update"
            };
            assert_eq!(specs[index].args.last().map(String::as_str), Some(expected));
            assert_eq!(
                specs[index]
                    .args
                    .iter()
                    .filter(|arg| arg.as_str() == "--")
                    .count(),
                1
            );
        }
        for index in [1, 2] {
            assert!(
                !specs[index].args.iter().any(|arg| arg == "--"
                    || arg.contains("update")
                    || arg.contains("initial brief"))
            );
        }
        assert_eq!(
            specs[1].args, specs[2].args,
            "restart must retain exactly the resume argv"
        );
    }
}

#[test]
fn s5_review_actual_spawn_admission_replaces_stale_default_with_one_enveloped_operand() {
    let supervisor = test_supervisor();
    let id = test_session_id(&supervisor, "claude");
    let room = supervisor
        .create_room(CreateRoomRequest {
            label: None,
            member_ids: vec![id],
            brief_on_join: true,
            brief_template: Some("stale task".into()),
        })
        .unwrap();
    let (definition, directory, expected) = {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id_mut(id).unwrap();
        slot.generation += 1;
        slot.state = LifecycleState::Starting;
        (
            slot.definition.clone(),
            slot.qualified_working_directory.clone().unwrap(),
            slot.generation,
        )
    };
    let plan = supervisor
        .prepare_launch_spec_for_spawn(
            &definition,
            &directory,
            true,
            false,
            &SessionLaunchChoices::default(),
        )
        .unwrap();
    assert!(plan.spec.args.last().unwrap().contains("stale task"));
    supervisor
        .update_room_brief(shared_types::UpdateRoomBriefRequest {
            room_id: room.room_id,
            brief_on_join: true,
            brief_template: Some("doctor".into()),
        })
        .unwrap();
    let (pty, input) = recording_pty_session(std::process::id());
    let (spawner, specs) = CapturingPtySpawner::new(vec![pty]);
    supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
    let lease = supervisor
        .revalidate_session_working_directory(id, definition.driver, &directory)
        .unwrap();
    supervisor
        .start_session_at(id, expected, lease, plan, recovery_automation::StartReason::Manual)
        .unwrap();
    let specs = specs.lock();
    assert_eq!(specs.len(), 1);
    assert_eq!(
        specs[0].args.last().map(String::as_str),
        Some("PRIM-1 initial brief:\n\ndoctor")
    );
    assert_eq!(
        specs[0]
            .args
            .iter()
            .filter(|arg| arg.as_str() == "--")
            .count(),
        1
    );
    assert!(!specs[0].args.iter().any(|arg| arg.contains("stale task")));
    assert!(input.lock().is_empty());
}

#[test]
fn s5_review_rendered_template_bounds_are_checked_before_saving_effective_defaults() {
    let supervisor = test_supervisor();
    let id = test_session_id(&supervisor, "claude");
    let oversized = "{members}".repeat(900); // source < 8 KiB, expansion > 8 KiB.
    assert!(oversized.len() <= 8192);
    let request = CreateRoomRequest {
        label: None,
        member_ids: vec![id],
        brief_on_join: true,
        brief_template: Some(oversized.clone()),
    };
    let error = supervisor.create_room(request.clone()).unwrap_err();
    assert!(format!("{error:#}").contains("maximum 8192"));
    let room = supervisor
        .create_room(CreateRoomRequest {
            brief_template: Some("short".into()),
            ..request
        })
        .unwrap();
    let before = fs::read(room_catalog_path(supervisor.runtime_dir())).unwrap();
    let error = supervisor
        .update_room_brief(shared_types::UpdateRoomBriefRequest {
            room_id: room.room_id,
            brief_on_join: true,
            brief_template: Some(oversized.clone()),
        })
        .unwrap_err();
    assert!(format!("{error:#}").contains("maximum 8192"));
    assert_eq!(
        fs::read(room_catalog_path(supervisor.runtime_dir())).unwrap(),
        before
    );
    supervisor
        .update_room_brief(shared_types::UpdateRoomBriefRequest {
            room_id: room.room_id,
            brief_on_join: false,
            brief_template: Some(oversized.clone()),
        })
        .unwrap();
    supervisor
        .inner
        .slots
        .lock()
        .get_by_id_mut(id)
        .unwrap()
        .launch_choices
        .brief = SessionBriefChoice::None {};
    supervisor
        .update_room_brief(shared_types::UpdateRoomBriefRequest {
            room_id: room.room_id,
            brief_on_join: true,
            brief_template: Some(oversized),
        })
        .unwrap();
}

#[test]
fn s5_review_identify_preserves_three_bindings_and_legacy_evidence_atomically() {
    let root = tempfile::tempdir().unwrap();
    let supervisor = test_supervisor_with_root(root.path().join("fixture"));
    let (pty, _) = recording_pty_session(102);
    install_mock_running_session_with_process_id(
        &supervisor,
        "codex",
        DriverKind::Codex,
        Some(102),
        pty,
    );
    let caller = supervisor
        .resolve_pane_caller(TestPaneProcess::new(7001, [102]))
        .unwrap();
    let legacy = RecoveryReference {
        original_id: Some("original-imported-id".into()),
        provenance: "legacy_catalog".into(),
        reason: "original import failure".into(),
        reported_codex: None,
    };
    let cwd = {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id_mut(caller.session_id).unwrap();
        slot.recovery = Some(legacy.clone());
        slot.definition.working_dir.clone()
    };
    supervisor.persist_slot_harness_state(caller.session_id);
    let sessions = root.path().join("codex-fixtures");
    let ids = (0..4)
        .map(|_| Uuid::new_v4().to_string())
        .collect::<Vec<_>>();
    for id in &ids {
        session::write_codex_fixture(&sessions, id, &cwd);
    }
    for id in &ids[..3] {
        supervisor
            .identify_codex_from_pane(&caller, id, &sessions)
            .unwrap();
    }
    let read_row = || {
        read_session_catalog(&session_catalog_path(supervisor.runtime_dir()))
            .unwrap()
            .sessions
            .into_iter()
            .find(|row| row.session_id == caller.session_id)
            .unwrap()
    };
    let before = read_row();
    assert_eq!(before.harness_session_id.as_deref(), Some(ids[2].as_str()));
    assert!(before.recovery_history.contains(&legacy));
    for id in &ids[..2] {
        assert!(before.recovery_history.iter().any(|entry| {
            entry.reported_codex.as_ref().is_some_and(|evidence| {
                evidence.thread_id == *id
                    && evidence.run_id == caller.run_id
                    && evidence.generation == caller.generation
            }) && entry.provenance == "job_attested_codex_shell_report"
        }));
    }
    supervisor
        .identify_codex_from_pane(&caller, &ids[2], &sessions)
        .unwrap();
    assert_eq!(read_row().recovery_history, before.recovery_history);
    let disk_before = fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap();
    supervisor.fail_next_catalog_write_for_tests();
    assert!(
        supervisor
            .identify_codex_from_pane(&caller, &ids[3], &sessions)
            .is_err()
    );
    assert_eq!(
        fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap(),
        disk_before
    );
    let slots = supervisor.inner.slots.lock();
    let slot = slots.get_by_id(caller.session_id).unwrap();
    assert_eq!(slot.harness_session_id.as_deref(), Some(ids[2].as_str()));
    assert_eq!(slot.recovery_history, before.recovery_history);
}

#[test]
fn s5_review_resize_updates_memory_without_catalog_io_and_stop_persists_size() {
    let supervisor = test_supervisor();
    let id = test_session_id(&supervisor, "claude");
    let (pty, _) = recording_pty_session(std::process::id());
    let (spawner, _) = CapturingPtySpawner::new(vec![pty]);
    supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
    supervisor.start_session_by_id(id).unwrap();
    let before = fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap();
    supervisor.resize_session_by_id(id, 91, 37).unwrap();
    assert_eq!(
        fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap(),
        before
    );
    assert_eq!(
        supervisor
            .inner
            .slots
            .lock()
            .get_by_id(id)
            .unwrap()
            .last_pty_size,
        Some((91, 37))
    );
    supervisor.stop_session_by_id(id).unwrap();
    let row = read_session_catalog(&session_catalog_path(supervisor.runtime_dir()))
        .unwrap()
        .sessions
        .into_iter()
        .find(|row| row.session_id == id)
        .unwrap();
    assert_eq!((row.last_pty_cols, row.last_pty_rows), (Some(91), Some(37)));
}
