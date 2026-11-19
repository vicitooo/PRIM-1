use super::*;
use shared_types::{CreateSessionRequest, PermissionProfile};

fn config(root: &Path, runtime: PathBuf) -> SupervisorConfig {
    SupervisorConfig {
        working_root: root.join("work"),
        runtime_dir: runtime,
        pane_mcp_executable: None,
        heartbeat_interval: Some(Duration::from_millis(10)),
        auto_restart_on_stall_sessions: Some(Vec::new()),
        auto_restart_stall_threshold: None,
        room_brief_on_join: false,
        task_folder: None,
    }
}

fn fixture() -> (tempfile::TempDir, TrialStartup, SessionId, RoomId) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir(root.join("work")).unwrap();
    let source = root.join("source-runtime");
    let seed = SupervisorHandle::new(config(root, source.clone())).unwrap();
    let session = seed
        .create_session(CreateSessionRequest { codex_transport: Default::default(), permissions: None,
            label: None,
            driver: DriverKind::Claude,
            permission_profile: PermissionProfile::Normal,
            linux_working_directory: None,
            room_id: None,
            launch_choices: None,
        })
        .unwrap()
        .session_id;
    let room = seed
        .create_room(CreateRoomRequest {
            label: None,
            member_ids: vec![session],
            brief_on_join: false,
            brief_template: None,
        })
        .unwrap()
        .room_id;
    seed.set_room_resume_on_restart(SetRoomResumeRequest {
        room_id: room,
        resume_on_restart: true,
    })
    .unwrap();
    {
        let mut catalog = seed.inner.catalog.lock();
        catalog.sessions[0].running_at_shutdown = true;
        persist_session_catalog(&source, &catalog).unwrap();
    }
    drop(seed);
    let attempt = Uuid::new_v4();
    let trial_root = root.join(attempt.to_string());
    fs::create_dir_all(trial_root.join("runtime")).unwrap();
    fs::create_dir(trial_root.join("profile")).unwrap();
    fs::write(
        trial_root.join("profile/settings-fixture"),
        b"copied profile fixture",
    )
    .unwrap();
    for name in [SESSION_CATALOG_FILE_NAME, ROOM_CATALOG_FILE_NAME] {
        fs::copy(source.join(name), trial_root.join("runtime").join(name)).unwrap();
    }
    let executable = root.join("inert-exe-fixture");
    fs::write(&executable, b"inert hashed fixture, never launched").unwrap();
    let trial = TrialStartup::prepare_with_executable(
        attempt,
        &trial_root,
        Duration::from_secs(30),
        &executable,
    )
    .unwrap();
    (temp, trial, session, room)
}

fn held<T: std::fmt::Debug>(result: Result<T>) {
    assert_eq!(result.unwrap_err().to_string(), TRIAL_ADMISSION_HELD);
}

#[test]
fn output_credit_trial_shutdown_refusal_leaves_renderer_admission_unchanged() {
    let (temp, trial, id, _) = fixture();
    let supervisor = SupervisorHandle::new_with_startup_admission(
        config(temp.path(), trial.runtime.clone()), trial.admission(),
    ).unwrap();
    let ledger = supervisor.output_credit();
    ledger.enable();
    let reset = ledger.begin_renderer();
    ledger.renderer_ready(reset.epoch, Duration::ZERO).unwrap();
    // Simulate an impossible ownership marker, without installing or opening
    // any process. The trial must refuse and leave its pre-call state intact.
    supervisor.inner.slots.lock().get_by_id_mut(id).unwrap().termination_uncertain = true;
    assert!(supervisor.shutdown().unwrap_err().to_string().contains("unexpectedly contains active members"));
    assert!(!supervisor.inner.shutdown_started.load(Ordering::Acquire));
    assert!(supervisor.inner.slots.lock().get_by_id(id).unwrap().termination_uncertain);
    ledger.renderer_ready(reset.epoch, Duration::ZERO)
        .expect("refused held-trial shutdown must not permanently stop the ledger");
    assert!(supervisor.trial_admission_held());
    held(supervisor.start_session_by_id(id));
    supervisor.inner.slots.lock().get_by_id_mut(id).unwrap().termination_uncertain = false;
    supervisor.shutdown().unwrap();
    assert!(supervisor.inner.shutdown_started.load(Ordering::Acquire));
}

#[test]
fn held_trial_resolves_but_never_opens_or_changes_the_task_folder() {
    let (temp, trial, _, _) = fixture();
    let supervisor = SupervisorHandle::new_with_startup_admission(
        config(temp.path(), trial.runtime.clone()),
        trial.admission(),
    )
    .unwrap();
    let status = supervisor.open_task_folder_from_setting();
    assert_eq!((status.source, status.status), (TaskFolderSource::Default, TaskFolderHealth::Error));
    assert!(status.error.as_deref().unwrap().contains("preview is held"), "{status:?}");
    assert!(supervisor.task_store().is_none());
    assert!(!temp.path().join("work").join("tasks").exists());
    let chosen = temp.path().join("chosen");
    assert_eq!(supervisor.select_task_folder(&chosen.display().to_string()).unwrap_err(), TRIAL_ADMISSION_HELD);
    assert!(!chosen.exists());
    assert!(!trial.runtime.join(crate::task_sideband::TASK_FOLDER_SETTING_FILE_NAME).exists());
    let read = supervisor.read_tasks("{ tasks { total } }", None, None);
    assert_eq!(read["errors"][0]["extensions"]["code"], "NO_TASK_FOLDER");
    assert!(read["errors"][0]["message"].as_str().unwrap().contains("preview is held"));
}

#[test]
fn trial_restored_selection_stays_held_across_all_mutation_families() {
    let (temp, trial, id, room) = fixture();
    let supervisor = SupervisorHandle::new_with_startup_admission(
        config(temp.path(), trial.runtime.clone()),
        trial.admission(),
    )
    .unwrap();
    let session_before = fs::read(trial.runtime.join(SESSION_CATALOG_FILE_NAME)).unwrap();
    let room_before = fs::read(trial.runtime.join(ROOM_CATALOG_FILE_NAME)).unwrap();
    let audit_before = fs::read(supervisor.audit_log_path()).unwrap_or_default();
    assert!(supervisor.snapshot().rooms[0].resume_on_restart);
    assert!(supervisor.snapshot().sessions[0].was_running_at_shutdown);
    assert!(
        supervisor
            .snapshot()
            .sessions
            .iter()
            .all(|session| !session.running)
    );
    held(supervisor.start_session_by_id(id));
    held(supervisor.stop_session_by_id(id));
    held(supervisor.restart_session_by_id(id));
    held(supervisor.set_workspace_preference(&temp.path().join("missing")));
    held(supervisor.create_session(CreateSessionRequest { codex_transport: Default::default(), permissions: None,
        label: None,
        driver: DriverKind::Claude,
        permission_profile: PermissionProfile::Normal,
        linux_working_directory: None,
        room_id: None,
        launch_choices: None,
    }));
    held(supervisor.rename_session(id, "changed"));
    held(supervisor.set_session_working_directory(id, &temp.path().join("missing")));
    held(supervisor.set_permission_profile(id, PermissionProfile::Normal));
    held(supervisor.move_session(id, 0));
    held(supervisor.delete_session(id));
    held(supervisor.create_room(CreateRoomRequest {
        label: None,
        member_ids: vec![],
        brief_on_join: false,
        brief_template: None,
    }));
    held(supervisor.rename_room(RenameRoomRequest {
        room_id: room,
        label: "changed".into(),
    }));
    held(supervisor.set_room_resume_on_restart(SetRoomResumeRequest {
        room_id: room,
        resume_on_restart: false,
    }));
    held(
        supervisor.update_room_brief(shared_types::UpdateRoomBriefRequest {
            room_id: room,
            brief_on_join: true,
            brief_template: None,
        }),
    );
    held(supervisor.move_room(MoveRoomRequest {
        room_id: room,
        new_index: 0,
    }));
    held(supervisor.add_room_member(AddRoomMemberRequest {
        room_id: room,
        session_id: id,
    }));
    held(supervisor.remove_room_member(RemoveRoomMemberRequest {
        room_id: room,
        session_id: id,
    }));
    held(supervisor.delete_room(DeleteRoomRequest { room_id: room }));
    held(supervisor.post_room_message(PostRoomMessageRequest {
        room_id: room,
        content: "held".into(),
        request_id: None,
    }));
    held(supervisor.deliver_room_message(DeliverRoomMessageRequest {
        request_id: None,
        room_id: room,
        recipients: RoomRecipientSelection::All {},
        content: "held".into(),
    }));
    held(supervisor.brief_room_member(BriefRoomMemberRequest {
        room_id: room,
        session_id: id,
    }));
    held(supervisor.send_input(SendInputRequest {
        session_id: id,
        input: "held".into(),
    }));
    held(supervisor.send_control_key_by_id(id, ControlKey::Enter));
    held(
        supervisor.route_operator_message(OperatorRouteMessageRequest {
            recipient_id: id,
            content: "held".into(),
        }),
    );
    held(supervisor.resize_session_by_id(id, 80, 24));
    held(supervisor.start_control_plane());
    held(supervisor.report_issue("held metadata mutation".into()));
    let caller = PaneCaller {
        session: session_alias(id),
        session_id: id,
        generation: 0,
        run_id: Uuid::new_v4(),
        identity: PaneCallerIdentity::Secret("not authenticated".into()),
    };
    for request in [
        SidebandRequest::SendInput {
            name: caller.session.clone(),
            input: "held".into(),
        },
        SidebandRequest::RoomPost {
            content: "held".into(),
            request_id: None,
        },
        SidebandRequest::RoomDeliver {
            recipient: "all".into(),
            content: "held".into(),
            request_id: None,
        },
        SidebandRequest::IdentifyCodex {
            thread_id: Uuid::new_v4().to_string(),
        },
        SidebandRequest::UpgradeRequest {
            candidate_exe: temp.path().join("missing.exe").display().to_string(),
            sha256: "a".repeat(64),
            task_id: "T-1".into(),
            task_revision: 1,
        },
    ] {
        let response = supervisor.apply_sideband_request(&caller, request);
        assert!(!response.ok);
        assert_eq!(response.message, TRIAL_ADMISSION_HELD);
    }
    assert!(
        supervisor
            .read_room_feed(ReadRoomFeedRequest {
                room_id: room,
                cursor: None
            })
            .is_ok()
    );
    assert!(
        supervisor
            .read_room_brief(shared_types::ReadRoomBriefRequest { room_id: room })
            .is_ok()
    );
    supervisor.emit_supervisor_heartbeat();
    assert_eq!(supervisor.doctor().unwrap().active_or_uncertain_members, 0);
    supervisor.shutdown().unwrap();
    assert_eq!(
        fs::read(trial.runtime.join(SESSION_CATALOG_FILE_NAME)).unwrap(),
        session_before
    );
    assert_eq!(
        fs::read(trial.runtime.join(ROOM_CATALOG_FILE_NAME)).unwrap(),
        room_before
    );
    assert_eq!(
        fs::read(supervisor.audit_log_path()).unwrap_or_default(),
        audit_before
    );
    assert!(!trial.runtime.join("maintenance").exists());
    assert!(supervisor.inner.auto_restart_history.lock().is_empty());
}

#[test]
fn trial_receipts_bind_current_boot_main_renderer_and_never_promote() {
    let (temp, trial, id, _) = fixture();
    let supervisor = SupervisorHandle::new_with_startup_admission(
        config(temp.path(), trial.runtime.clone()),
        trial.admission(),
    )
    .unwrap();
    let challenge = trial.challenge();
    assert!(
        trial
            .record_renderer_acknowledged(&supervisor, "main", &challenge)
            .is_err()
    );
    let backend = trial.record_backend_initialized(&supervisor).unwrap();
    assert!(!backend.readiness_verified);
    assert!(trial.record_backend_initialized(&supervisor).is_err());
    let mut wrong = challenge.clone();
    wrong.boot_nonce = Uuid::new_v4();
    assert!(
        trial
            .record_renderer_acknowledged(&supervisor, "main", &wrong)
            .is_err()
    );
    assert!(
        trial
            .record_renderer_acknowledged(&supervisor, "other", &challenge)
            .is_err()
    );
    assert!(!trial.root.join("renderer-v1.json").exists());
    let renderer = trial
        .record_renderer_acknowledged(&supervisor, "main", &challenge)
        .unwrap();
    assert_eq!(renderer.admission, "held");
    assert!(!renderer.readiness_verified);
    assert!(!renderer.hold_reasons.is_empty());
    assert!(supervisor.trial_admission_held());
    held(supervisor.start_session_by_id(id));
    assert!(
        trial
            .record_renderer_acknowledged(&supervisor, "main", &challenge)
            .is_err()
    );
    assert_eq!(
        fs::read(trial.profile.join("settings-fixture")).unwrap(),
        b"copied profile fixture"
    );
    assert!(
        TrialStartup::prepare_with_executable(
            challenge.attempt_id,
            &trial.root,
            Duration::from_secs(30),
            &temp.path().join("inert-exe-fixture")
        )
        .is_err()
    );
}

#[test]
fn trial_receipts_reject_wrong_attempt_wrong_runtime_and_normal_admission_without_files() {
    for mismatch in ["attempt", "runtime", "normal"] {
        let (temp, mut trial, _, _) = fixture();
        let admission = if mismatch == "normal" {
            StartupAdmission::Normal
        } else {
            trial.admission()
        };
        let supervisor = SupervisorHandle::new_with_startup_admission(
            config(temp.path(), trial.runtime.clone()),
            admission,
        )
        .unwrap();
        // Construct a valid supervisor first, then change only the test-side
        // receipt binding. Constructor validation remains independently intact.
        match mismatch {
            "attempt" => trial.challenge.attempt_id = Uuid::new_v4(),
            "runtime" => {
                let runtime = temp.path().join("other-copied-runtime");
                fs::create_dir(&runtime).unwrap();
                for name in [SESSION_CATALOG_FILE_NAME, ROOM_CATALOG_FILE_NAME] {
                    fs::copy(trial.runtime.join(name), runtime.join(name)).unwrap();
                }
                trial.runtime = fs::canonicalize(runtime).unwrap();
                assert_eq!(
                    supervisor.inner.startup_admission.attempt_id(),
                    Some(trial.challenge.attempt_id),
                    "runtime mismatch must keep the same attempt",
                );
            }
            "normal" => {}
            _ => unreachable!(),
        }
        assert!(
            trial
                .record_backend_initialized(&supervisor)
                .unwrap_err()
                .to_string()
                .contains("does not match the held trial supervisor"),
            "{mismatch}"
        );
        assert!(!*trial.backend_recorded.lock());
        // Reach the renderer's backend-binding check independently of its
        // separate backend-stage/nonce check, without writing any receipt.
        *trial.backend_recorded.lock() = true;
        assert!(
            trial
                .record_renderer_acknowledged(&supervisor, "main", &trial.challenge())
                .unwrap_err()
                .to_string()
                .contains("does not match the held trial supervisor"),
            "{mismatch}"
        );
        for name in [
            "backend-v1.json",
            ".backend-v1.json.pending",
            "renderer-v1.json",
            ".renderer-v1.json.pending",
        ] {
            assert!(!trial.root.join(name).exists(), "{mismatch}: {name}");
        }
    }
}

#[cfg(windows)]
#[test]
fn trial_receipt_rechecks_replaced_root_before_changing_target_permissions() {
    let (temp, trial, _, _) = fixture();
    let original = temp.path().join("original-attempt-preserved");
    fs::rename(&trial.root, &original).unwrap();
    let target = temp.path().join("junction-target");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("sentinel"), b"untouched").unwrap();
    let before = crate::tests::windows_path_sddl(&target);
    crate::tests::create_windows_junction(&trial.root, &target);
    let outcome = trial.write_receipt("backend-v1.json", "backend_initialized_held");
    // Unlink only the isolated fixture's junction before assertions/cleanup.
    fs::remove_dir(&trial.root).unwrap();
    assert!(outcome.unwrap_err().to_string().contains("reparse point"));
    assert_eq!(crate::tests::windows_path_sddl(&target), before);
    assert_eq!(fs::read(target.join("sentinel")).unwrap(), b"untouched");
    assert_eq!(fs::read_dir(&target).unwrap().count(), 1);
    assert!(!original.join("backend-v1.json").exists());
}

#[test]
fn trial_refuses_wrong_runtime_incomplete_paths_and_expired_health() {
    let (temp, mut trial, _, _) = fixture();
    assert!(
        SupervisorHandle::new_with_startup_admission(
            config(temp.path(), temp.path().join("source-runtime")),
            trial.admission()
        )
        .is_err()
    );
    let supervisor = SupervisorHandle::new_with_startup_admission(
        config(temp.path(), trial.runtime.clone()),
        trial.admission(),
    )
    .unwrap();
    trial.started = Instant::now() - Duration::from_secs(31);
    assert!(
        trial
            .record_backend_initialized(&supervisor)
            .unwrap_err()
            .to_string()
            .contains("expired")
    );
    assert!(!trial.root.join("backend-v1.json").exists());
    trial.started = Instant::now();
    trial.record_backend_initialized(&supervisor).unwrap();
    supervisor.shutdown().unwrap();
    assert!(
        trial
            .record_renderer_acknowledged(&supervisor, "main", &trial.challenge())
            .is_err()
    );
    assert!(!trial.root.join("renderer-v1.json").exists());
    let executable = temp.path().join("inert-exe-fixture");
    assert!(
        TrialStartup::prepare_with_executable(
            Uuid::new_v4(),
            &trial.root,
            Duration::from_secs(30),
            &executable
        )
        .is_err()
    );
    assert!(
        TrialStartup::prepare_with_executable(
            trial.challenge.attempt_id,
            &trial.root,
            Duration::ZERO,
            &executable
        )
        .is_err()
    );
    let fresh_id = Uuid::new_v4();
    let incomplete = temp.path().join(fresh_id.to_string());
    assert!(
        TrialStartup::prepare_with_executable(
            fresh_id,
            &incomplete,
            Duration::from_secs(30),
            &executable
        )
        .is_err()
    );
    assert!(!incomplete.exists());
}

#[test]
fn trial_failed_health_write_retains_pending_and_does_not_advance_stage() {
    let (temp, trial, _, _) = fixture();
    let supervisor = SupervisorHandle::new_with_startup_admission(
        config(temp.path(), trial.runtime.clone()),
        trial.admission(),
    )
    .unwrap();
    let pending = trial.root.join(".backend-v1.json.pending");
    fs::write(&pending, b"uncertain prior write").unwrap();
    assert!(trial.record_backend_initialized(&supervisor).is_err());
    assert!(!*trial.backend_recorded.lock());
    assert!(
        trial
            .record_renderer_acknowledged(&supervisor, "main", &trial.challenge())
            .is_err()
    );
    assert_eq!(fs::read(pending).unwrap(), b"uncertain prior write");
    assert!(!trial.root.join("backend-v1.json").exists());
    assert!(!trial.root.join("renderer-v1.json").exists());
    assert!(supervisor.trial_admission_held());
}

#[test]
fn trial_renderer_receipt_serializes_with_shutdown_until_persisted() {
    let (temp, trial, _, _) = fixture();
    let supervisor = SupervisorHandle::new_with_startup_admission(
        config(temp.path(), trial.runtime.clone()),
        trial.admission(),
    )
    .unwrap();
    trial.record_backend_initialized(&supervisor).unwrap();
    let (entered_tx, entered_rx) = mpsc::sync_channel(0);
    let (release_tx, release_rx) = mpsc::sync_channel(0);
    *trial.before_receipt_for_tests.lock() = Some(Box::new(move || {
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    }));
    let trial = Arc::new(trial);
    let acknowledge = {
        let trial = trial.clone();
        let supervisor = supervisor.clone();
        thread::spawn(move || {
            trial.record_renderer_acknowledged(&supervisor, "main", &trial.challenge())
        })
    };
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(supervisor.inner.shutdown_lifecycle.try_write().is_none());
    assert!(!trial.root.join("renderer-v1.json").exists());
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let shutdown = {
        let supervisor = supervisor.clone();
        thread::spawn(move || {
            let result = supervisor.shutdown();
            done_tx.send(()).unwrap();
            result
        })
    };
    assert!(matches!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    release_tx.send(()).unwrap();
    assert_eq!(acknowledge.join().unwrap().unwrap().admission, "held");
    shutdown.join().unwrap().unwrap();
    assert!(trial.root.join("renderer-v1.json").exists());
    assert!(supervisor.doctor().unwrap().shutdown_started);
    assert!(
        trial
            .record_renderer_acknowledged(&supervisor, "main", &trial.challenge())
            .is_err()
    );
}

#[test]
fn held_trial_refuses_room_save_and_load_without_writing() {
    let (temp, trial, _, room) = fixture();
    let supervisor = SupervisorHandle::new_with_startup_admission(
        config(temp.path(), trial.runtime.clone()),
        trial.admission(),
    )
    .unwrap();
    let target = temp.path().join("held.prim1room");
    held(supervisor.export_room(room, &target));
    assert!(!target.exists());
    fs::write(&target, b"{\"record\":\"prim1_room_package\",\"version\":1}\n").unwrap();
    let catalogs = [SESSION_CATALOG_FILE_NAME, ROOM_CATALOG_FILE_NAME].map(|name| fs::read(trial.runtime.join(name)).unwrap());
    held(supervisor.inspect_room_source(&target));
    held(supervisor.import_room(&target, None));
    assert_eq!([SESSION_CATALOG_FILE_NAME, ROOM_CATALOG_FILE_NAME].map(|name| fs::read(trial.runtime.join(name)).unwrap()), catalogs);
}
