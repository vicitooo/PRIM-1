// Included in the supervisor test module. A110 member lifecycle: the manage
// grant, the six member actions through the operator's own supervisor
// methods, their commit-time rechecks, notices and request_id replays. Mock
// PTYs, test pane processes and temporary catalogs only; no harness runs.

fn manage_grant(roam: bool) -> MemberPermissions {
    MemberPermissions { roam, maintain: false, manage: true }
}

fn member_request(supervisor: &SupervisorHandle, caller: &PaneCaller, request_id: &str, action: shared_types::MemberAction)
    -> SidebandResponse {
    supervisor.apply_sideband_request(caller, SidebandRequest::Member { request_id: request_id.into(), action })
}

fn member_done(response: SidebandResponse) -> shared_types::MemberActionResult {
    assert!(response.ok, "{}", response.message);
    match response.payload {
        Some(SidebandResponsePayload::Member { result }) => result,
        other => panic!("expected a member result, got {other:?}"),
    }
}

fn member_refused(response: SidebandResponse, wanted: &str) {
    assert!(!response.ok && response.message.contains(wanted), "wanted '{wanted}': {}", response.message);
    assert!(response.payload.is_none());
}

fn create_action(driver: DriverKind, label: &str) -> shared_types::MemberAction {
    shared_types::MemberAction::Create { codex_transport: Default::default(), grants: None, driver, label: label.into(), permission_profile: shared_types::PermissionProfile::Normal,
        brief: false, brief_text: None, start: false, room_id: None }
}

fn start_action(member: &str) -> shared_types::MemberAction { shared_types::MemberAction::Start { member: member.into(), mode: Default::default(), launch_choices: None, search: false } }
fn stop_action(member: &str) -> shared_types::MemberAction { shared_types::MemberAction::Stop { member: member.into() } }
fn restart_action(member: &str) -> shared_types::MemberAction { shared_types::MemberAction::Restart { member: member.into() } }
fn remove_action(member: &str) -> shared_types::MemberAction { shared_types::MemberAction::Remove { member: member.into() } }
fn brief_action(member: &str, content: Option<&str>) -> shared_types::MemberAction {
    shared_types::MemberAction::Brief { member: member.into(), content: content.map(Into::into) }
}

fn session_now(supervisor: &SupervisorHandle, session_id: SessionId) -> SessionSnapshot {
    supervisor.snapshot().sessions.into_iter().find(|session| session.session_id == session_id).unwrap()
}

fn room_of(supervisor: &SupervisorHandle, session_id: SessionId) -> Option<RoomId> {
    supervisor.inner.rooms.lock().room_by_session.get(&session_id).copied()
}

/// Mock PTYs the next starts spawn. Each reports this test process, which is
/// alive and outside every caller's pane, and stops at once when killed.
fn queue_mock_starts(supervisor: &SupervisorHandle, count: usize) {
    let sessions = (0..count).map(|_| mock_pty_session(Some(std::process::id()), MockKillBehavior::Immediate).0).collect();
    supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(sessions)));
}

#[test]
fn every_member_action_is_refused_without_manage_and_changes_nothing() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let caller = mobility_pane(&supervisor, "claude", 9401);
    let room = mobility_room(&supervisor, "Alpha", vec![claude, codex]);
    // Roam alone is not manage.
    supervisor.set_member_permissions(claude, MemberPermissions { roam: true, maintain: true, manage: false }).unwrap();
    queue_mock_starts(&supervisor, 1);
    let before = (supervisor.snapshot().sessions.len(), room_now(&supervisor, room.room_id));

    for action in [create_action(DriverKind::Grok, "QA Grok"), start_action("codex"), stop_action("codex"),
        restart_action("codex"), brief_action("codex", Some("x")), remove_action("codex")] {
        member_refused(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), action), "Can manage agents");
    }
    assert_eq!((supervisor.snapshot().sessions.len(), room_now(&supervisor, room.room_id)), before);
    assert!(!session_now(&supervisor, codex).running);
    assert!(mobility_messages(&supervisor, room.room_id).is_empty());

    // The grant is read on the next request; a stale run is refused first.
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    let granted = supervisor.apply_sideband_request(&caller, SidebandRequest::MemberPermissions {});
    assert_eq!(granted.payload, Some(SidebandResponsePayload::MemberPermissions { permissions: manage_grant(false) }));
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "claude", DriverKind::Claude,
        recording_pty_session(9402).0);
    let stale = member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), start_action("codex"));
    assert!(!stale.ok && stale.message.contains("stale"), "{}", stale.message);
}

#[test]
fn the_manage_grant_persists_is_refused_for_terminal_and_prime_and_old_catalogs_load_without_it() {
    let root = tempfile::tempdir().unwrap();
    let mut supervisor = test_supervisor_with_root(root.path().to_path_buf());
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let terminal = create_test_session(&supervisor, "shell", DriverKind::GenericTerminal,
        shared_types::PermissionProfile::Normal).session_id;
    supervisor.set_member_permissions(claude, manage_grant(true)).unwrap();
    supervisor.set_member_permissions(codex, MemberPermissions { roam: true, maintain: false, manage: false }).unwrap();
    let refused = supervisor.set_member_permissions(terminal, manage_grant(false)).unwrap_err();
    assert!(refused.to_string().contains("\"Can manage agents\" do nothing"), "{refused}");
    assert_eq!(member_permissions(&supervisor, terminal), MemberPermissions::default());

    // manage is written only while on, so a row without it reads the same to A109.
    let path = session_catalog_path(supervisor.runtime_dir());
    let catalog: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let row = |id: SessionId| catalog["sessions"].as_array().unwrap().iter()
        .find(|row| row["session_id"] == serde_json::json!(id)).unwrap().clone();
    assert_eq!(row(claude)["permissions"], serde_json::json!({ "roam": true, "maintain": false, "manage": true }));
    assert_eq!(row(codex)["permissions"], serde_json::json!({ "roam": true, "maintain": false }));

    drop(supervisor);
    supervisor = SupervisorHandle::new(test_supervisor_config(root.path())).unwrap();
    assert_eq!(member_permissions(&supervisor, claude), manage_grant(true), "the grant survives a restart");

    // An A109 catalog (roam and maintain, no manage) loads with manage off.
    drop(supervisor);
    let mut catalog: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    for row in catalog["sessions"].as_array_mut().unwrap() {
        if row["session_id"] == serde_json::json!(claude) {
            row["permissions"] = serde_json::json!({ "roam": true, "maintain": true });
        }
    }
    fs::write(&path, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();
    supervisor = SupervisorHandle::new(test_supervisor_config(root.path())).unwrap();
    assert_eq!(member_permissions(&supervisor, claude), MemberPermissions { roam: true, maintain: true, manage: false });
}

#[test]
fn a_managing_member_starts_stops_and_restarts_a_member_with_a_notice_each_and_replays_retries() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    // A Claude member: its launch mints the conversation a restart resumes.
    let helper = create_test_session(&supervisor, "helper", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9411);
    let room = mobility_room(&supervisor, "Alpha", vec![claude, helper]);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    queue_mock_starts(&supervisor, 2);

    let start_id = Uuid::new_v4().to_string();
    let started = member_done(member_request(&supervisor, &caller, &start_id, start_action("helper")));
    assert_eq!((started.op.as_str(), started.room_id, started.member.session_id), ("start", room.room_id, helper));
    assert!(started.member.running && !started.replayed && started.warnings.is_empty(), "{started:?}");
    // The same request_id returns the first result and starts nothing again.
    let replay = member_done(member_request(&supervisor, &caller, &start_id, start_action("helper")));
    assert!(replay.replayed);
    assert_eq!(replay.member, started.member);
    member_refused(member_request(&supervisor, &caller, &start_id, stop_action("helper")), "was already used for a different request");
    member_refused(member_request(&supervisor, &caller, "not-a-uuid", stop_action("helper")), "must be a UUID");
    // Already running: nothing changes and no notice is posted.
    let again = member_done(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), start_action(&helper.to_string())));
    assert!(again.warnings.iter().any(|warning| warning.contains("already running")), "{again:?}");

    let generation = session_now(&supervisor, helper).generation;
    let restarted = member_done(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), restart_action("HELPER")));
    assert!(restarted.member.running && restarted.member.generation > generation, "{restarted:?}");

    let stop_id = Uuid::new_v4().to_string();
    let stopped = member_done(member_request(&supervisor, &caller, &stop_id, stop_action("helper")));
    assert!(!stopped.member.running);
    assert_eq!(stopped.member.lifecycle_state, LifecycleState::Closed);
    assert!(member_done(member_request(&supervisor, &caller, &stop_id, stop_action("helper"))).replayed);
    let idle = member_done(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), stop_action("helper")));
    assert!(idle.warnings.iter().any(|warning| warning.contains("was not running")), "{idle:?}");

    assert_eq!(mobility_messages(&supervisor, room.room_id), [
        (Some(claude), "claude started helper".to_string()),
        (Some(claude), "claude restarted helper".to_string()),
        (Some(claude), "claude stopped helper".to_string()),
    ], "one notice per effect, none for replays or no-ops");
    assert_eq!(room_now(&supervisor, room.room_id).member_ids, [claude, helper]);
}

#[test]
fn a_managing_member_never_acts_on_itself() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let caller = mobility_pane(&supervisor, "claude", 9421);
    let room = mobility_room(&supervisor, "Alpha", vec![claude, codex]);
    supervisor.set_member_permissions(claude, manage_grant(true)).unwrap();
    let run = session_now(&supervisor, claude);

    for member in ["claude", " Claude ", &claude.to_string()] {
        for action in [stop_action(member), restart_action(member), remove_action(member), brief_action(member, None)] {
            member_refused(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), action), "itself");
        }
        // A16: a plain start of the running caller is a no-op, not a refusal.
        let me = member_done(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), start_action(member)));
        assert!(me.warnings.iter().any(|warning| warning.contains("you are running")), "{me:?}");
    }
    let after = session_now(&supervisor, claude);
    assert!(after.running);
    assert_eq!((after.generation, after.run_id), (run.generation, run.run_id));
    assert_eq!(room_of(&supervisor, claude), Some(room.room_id));
    assert!(mobility_messages(&supervisor, room.room_id).is_empty());
}

#[test]
fn unknown_and_ambiguous_member_names_are_refused_with_the_room_members_listed() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let first = create_test_session(&supervisor, "Twin", DriverKind::Grok, shared_types::PermissionProfile::Normal).session_id;
    let second = create_test_session(&supervisor, "twin", DriverKind::Codex, shared_types::PermissionProfile::Normal).session_id;
    let lobby = create_test_session(&supervisor, "Loner", DriverKind::Codex, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9431);
    mobility_room(&supervisor, "Alpha", vec![claude, codex, first, second]);
    supervisor.set_member_permissions(claude, manage_grant(true)).unwrap();
    let ask = |member: &str| member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), stop_action(member));

    let ambiguous = ask("TWIN");
    member_refused(ambiguous.clone(), "names more than one member of room \"Alpha\"; use a session id");
    assert!(ambiguous.message.contains(&format!("Twin ({first})")) && ambiguous.message.contains(&format!("twin ({second})")),
        "{}", ambiguous.message);
    let unknown = ask("nobody");
    member_refused(unknown.clone(), "no member of room \"Alpha\" is labelled 'nobody'");
    assert!(unknown.message.contains(&format!("codex ({codex})")), "{}", unknown.message);
    member_refused(ask(&Uuid::new_v4().to_string()), "no member has session id");
    member_refused(ask(""), "label or session id");
    // A16: a session in the lobby is reached by its session id, with roam (it
    // was not running, so nothing stopped); a label names room members only.
    let loner = member_done(ask(&lobby.to_string()));
    assert!(loner.member.session_id == lobby && !loner.member.running);
    member_refused(ask("Loner"), "no member of room \"Alpha\" is labelled 'Loner'");
    // The session id picks one of the twins.
    let stopped = member_done(ask(&second.to_string()));
    assert_eq!(stopped.member.session_id, second);

    // A new member's label must be unique in the room, whatever its case.
    member_refused(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), create_action(DriverKind::Grok, "CODEX")),
        "already has a member labelled 'CODEX'");
}

#[test]
fn another_room_needs_roam_as_well_as_manage() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let caller = mobility_pane(&supervisor, "claude", 9441);
    let alpha = mobility_room(&supervisor, "Alpha", vec![claude]);
    let beta = mobility_room(&supervisor, "Beta", vec![codex]);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    queue_mock_starts(&supervisor, 1);
    let sessions = supervisor.snapshot().sessions.len();
    let in_beta = shared_types::MemberAction::Create { codex_transport: Default::default(), grants: None, driver: DriverKind::Codex, label: "Scout".into(),
        permission_profile: shared_types::PermissionProfile::Normal, brief: false, brief_text: None, start: false,
        room_id: Some(beta.room_id) };

    member_refused(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), in_beta.clone()), "Can roam");
    for action in [start_action(&codex.to_string()), stop_action(&codex.to_string()), brief_action(&codex.to_string(), Some("x")),
        remove_action(&codex.to_string())] {
        member_refused(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), action), "Can roam");
    }
    // A label resolves only in the caller's own room.
    member_refused(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), start_action("codex")),
        "no member of room \"Alpha\" is labelled 'codex'");
    assert_eq!(supervisor.snapshot().sessions.len(), sessions);
    assert!(!session_now(&supervisor, codex).running);
    assert!(mobility_messages(&supervisor, beta.room_id).is_empty());

    supervisor.set_member_permissions(claude, manage_grant(true)).unwrap();
    let created = member_done(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), in_beta));
    assert_eq!(created.room_id, beta.room_id);
    assert_eq!(room_of(&supervisor, created.member.session_id), Some(beta.room_id));
    let started = member_done(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), start_action(&codex.to_string())));
    assert!(started.member.running);
    assert_eq!(mobility_messages(&supervisor, beta.room_id), [
        (Some(claude), "claude added Scout (Codex, normal permissions, not started)".to_string()),
        (Some(claude), "claude started codex".to_string()),
    ], "the notices land in the room acted in");
    assert!(mobility_messages(&supervisor, alpha.room_id).is_empty());
}

#[test]
fn a_grant_withdrawn_after_admission_stops_every_action_where_it_commits() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let caller = mobility_pane(&supervisor, "claude", 9451);
    let (codex_pty, codex_inputs) = recording_pty_session(9452);
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, codex_pty);
    let room = mobility_room(&supervisor, "Alpha", vec![claude, codex]);
    queue_mock_starts(&supervisor, 2);
    let sessions = supervisor.snapshot().sessions.len();
    let codex_run = session_now(&supervisor, codex);

    for action in [create_action(DriverKind::Codex, "QA"), stop_action("codex"), restart_action("codex"),
        brief_action("codex", Some("withdrawn")), remove_action("codex")] {
        supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
        // Every pre-check passed; the operator untticks the grant just before the effect.
        let withdraw = supervisor.clone();
        *supervisor.inner.member_before_effect.lock() = Some(Box::new(move || {
            withdraw.set_member_permissions(claude, MemberPermissions::default()).unwrap();
        }));
        member_refused(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), action.clone()), "Can manage agents");
        assert!(supervisor.inner.member_before_effect.lock().is_none(), "{action:?} reached its effect");
    }
    let after = session_now(&supervisor, codex);
    assert!(after.running);
    assert_eq!((after.generation, after.run_id), (codex_run.generation, codex_run.run_id));
    assert_eq!(supervisor.snapshot().sessions.len(), sessions, "nothing was created");
    assert_eq!(room_now(&supervisor, room.room_id).member_ids, [claude, codex]);
    assert!(codex_inputs.lock().is_empty(), "no brief was typed");
    assert!(mobility_messages(&supervisor, room.room_id).is_empty(), "no brief and no notice was stored");

    // A stopped target: start is stopped at its commit too.
    supervisor.stop_session_by_id(codex).unwrap();
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    let withdraw = supervisor.clone();
    *supervisor.inner.member_before_effect.lock() = Some(Box::new(move || {
        withdraw.set_member_permissions(claude, MemberPermissions::default()).unwrap();
    }));
    member_refused(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), start_action("codex")), "Can manage agents");
    assert!(!session_now(&supervisor, codex).running);
}

#[test]
fn roam_withdrawn_just_before_a_create_in_another_room_commits_creates_and_publishes_no_session() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let caller = mobility_pane(&supervisor, "claude", 9501);
    mobility_room(&supervisor, "Alpha", vec![claude]);
    let beta = mobility_room(&supervisor, "Beta", vec![codex]);
    supervisor.set_member_permissions(claude, manage_grant(true)).unwrap();
    let sessions = supervisor.snapshot().sessions.len();
    let published = Arc::new(Mutex::new(Vec::new()));
    let sink = published.clone();
    supervisor.set_event_sink(move |event| match event {
        RuntimeEvent::SessionCreated { session, .. } => sink.lock().push(format!("created {}", session.label)),
        RuntimeEvent::SessionDeleted { label, .. } => sink.lock().push(format!("deleted {label}")),
        _ => {}
    });
    // Manage stays; only roam is withdrawn, after every pre-check passed.
    let withdraw = supervisor.clone();
    *supervisor.inner.member_before_effect.lock() = Some(Box::new(move || {
        withdraw.set_member_permissions(claude, manage_grant(false)).unwrap();
    }));
    let in_beta = shared_types::MemberAction::Create { codex_transport: Default::default(), grants: None, driver: DriverKind::Codex, label: "Scout".into(),
        permission_profile: shared_types::PermissionProfile::Normal, brief: false, brief_text: None, start: false,
        room_id: Some(beta.room_id) };

    member_refused(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), in_beta), "Can roam");
    assert!(supervisor.inner.member_before_effect.lock().is_none(), "the create reached its effect");
    assert!(published.lock().is_empty(), "no session was committed or published: {:?}", published.lock());
    assert_eq!(supervisor.snapshot().sessions.len(), sessions);
    assert_eq!(supervisor.inner.catalog.lock().sessions.len(), sessions);
    assert_eq!(room_now(&supervisor, beta.room_id).member_ids, [codex]);
    assert!(mobility_messages(&supervisor, beta.room_id).is_empty());
}

#[test]
fn member_create_briefs_only_when_asked_once_when_ready_and_a_retry_creates_nothing() {
    let root = tempfile::tempdir().unwrap();
    let mut supervisor = test_supervisor_with_root(root.path().to_path_buf());
    let claude = test_session_id(&supervisor, "claude");
    let mut caller = mobility_pane(&supervisor, "claude", 9461);
    let room = mobility_room(&supervisor, "Alpha", vec![claude]);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();

    // brief=false: no brief now and none at launch.
    let quiet = member_done(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(),
        create_action(DriverKind::Codex, "Quiet Codex")));
    assert!(quiet.brief.is_none());
    assert_eq!(quiet.member.launch_choices.brief, SessionBriefChoice::None {});
    assert_eq!(quiet.member.permissions, MemberPermissions::default(), "a new member has no grants");
    assert_eq!(supervisor.queued_room_wake_count_for_tests(quiet.member.session_id), 0);

    // brief=true with custom text, not started: stored and queued for the member.
    let request_id = Uuid::new_v4().to_string();
    let text = "Check the release build and report in the room.";
    let briefed_action = shared_types::MemberAction::Create { codex_transport: Default::default(), grants: None, driver: DriverKind::Codex, label: "QA Codex".into(),
        permission_profile: shared_types::PermissionProfile::Normal, brief: true, brief_text: Some(text.into()),
        start: false, room_id: None };
    let first = member_done(member_request(&supervisor, &caller, &request_id, briefed_action.clone()));
    let qa = first.member.session_id;
    let brief = first.brief.clone().expect("brief delivery");
    assert_eq!(brief.receipts[0].status, RoomDeliveryStatus::Stored);
    assert_eq!(first.member.launch_choices.brief, SessionBriefChoice::None {}, "never a second brief at launch");
    assert_eq!(supervisor.queued_room_wake_count_for_tests(qa), 1);
    assert_eq!(room_now(&supervisor, room.room_id).member_ids, [claude, quiet.member.session_id, qa]);

    // A retry creates nothing and briefs nothing.
    let sessions = supervisor.snapshot().sessions.len();
    let replay = member_done(member_request(&supervisor, &caller, &request_id, briefed_action.clone()));
    assert!(replay.replayed);
    assert_eq!((replay.member.session_id, replay.brief.map(|brief| brief.message_id)), (qa, Some(brief.message_id)));
    assert_eq!(supervisor.snapshot().sessions.len(), sessions);
    assert_eq!(supervisor.queued_room_wake_count_for_tests(qa), 1);

    // The member becomes ready: the brief is typed once, from the caller.
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_running_session_by_id_with_mode(&supervisor, qa, DriverKind::Codex, None, pty, BracketedPasteMode::Enabled);
    wait_for_room_receipt(&supervisor, room.room_id, brief.message_id, qa, RoomDeliveryStatus::Written);
    wait_for_no_queued_room_wake(&supervisor, qa);
    assert_eq!(*inputs.lock(), vec![typed_room_message("claude", text), "\x1b[C\r".to_string()]);
    let stored: Vec<_> = mobility_messages(&supervisor, room.room_id);
    assert_eq!(stored, [
        (Some(claude), "claude added Quiet Codex (Codex, normal permissions, not started)".to_string()),
        (Some(claude), text.to_string()),
        (Some(claude), "claude added QA Codex (Codex, normal permissions, not started, briefed)".to_string()),
    ]);

    // After a PRIM-1 restart the receipts are gone; the session row still
    // names the request, so the retry finds the member instead of a second.
    drop(caller);
    drop(supervisor);
    supervisor = SupervisorHandle::new(test_supervisor_config(root.path())).unwrap();
    supervisor.set_executable_resolver_for_tests(Arc::new(TestExecutableResolver));
    caller = mobility_pane(&supervisor, "claude", 9462);
    let sessions = supervisor.snapshot().sessions.len();
    // The row also keeps the fingerprint of the create's arguments: a retry
    // that changes any of them is refused, never given the first success.
    let changed = |driver: DriverKind, label: &str, permission_profile: shared_types::PermissionProfile, brief: bool,
        brief_text: Option<&str>, start: bool, room_id: Option<RoomId>| shared_types::MemberAction::Create { codex_transport: Default::default(), grants: None,
            driver, label: label.into(), permission_profile, brief, brief_text: brief_text.map(Into::into), start, room_id };
    use shared_types::PermissionProfile::{Normal, Unsafe};
    for action in [
        changed(DriverKind::Grok, "QA Codex", Normal, true, Some(text), false, None),
        changed(DriverKind::Codex, "QA Codex 2", Normal, true, Some(text), false, None),
        changed(DriverKind::Codex, "QA Codex", Unsafe, true, Some(text), false, None),
        changed(DriverKind::Codex, "QA Codex", Normal, true, Some("Something else."), false, None),
        changed(DriverKind::Codex, "QA Codex", Normal, true, None, false, None),
        changed(DriverKind::Codex, "QA Codex", Normal, false, None, false, None),
        changed(DriverKind::Codex, "QA Codex", Normal, true, Some(text), true, None),
        changed(DriverKind::Codex, "QA Codex", Normal, true, Some(text), false, Some(Uuid::new_v4())),
    ] {
        member_refused(member_request(&supervisor, &caller, &request_id, action), "was used for a different create");
    }
    assert_eq!(supervisor.snapshot().sessions.len(), sessions);
    assert_eq!(mobility_messages(&supervisor, room.room_id).len(), 3);
    let after_restart = member_done(member_request(&supervisor, &caller, &request_id, briefed_action));
    assert!(after_restart.replayed);
    assert!(after_restart.incomplete.is_empty() && after_restart.warnings.is_empty(), "{after_restart:?}");
    assert_eq!(after_restart.member.session_id, qa);
    assert_eq!(after_restart.brief.map(|brief| brief.message_id), Some(brief.message_id));
    assert_eq!(supervisor.snapshot().sessions.len(), sessions);
    assert_eq!(mobility_messages(&supervisor, room.room_id).len(), 3);

    // brief_text alone, an empty text and non-agent drivers are refused.
    for action in [
        shared_types::MemberAction::Create { codex_transport: Default::default(), grants: None, driver: DriverKind::Codex, label: "X".into(), permission_profile: Default::default(),
            brief: false, brief_text: Some("x".into()), start: false, room_id: None },
        shared_types::MemberAction::Create { codex_transport: Default::default(), grants: None, driver: DriverKind::Codex, label: "X".into(), permission_profile: Default::default(),
            brief: true, brief_text: Some(" ".into()), start: false, room_id: None },
        create_action(DriverKind::GenericTerminal, "X"),
        create_action(DriverKind::Prime, "X"),
    ] {
        assert!(!member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), action).ok);
    }
    assert_eq!(supervisor.snapshot().sessions.len(), sessions);
}

#[test]
fn an_interrupted_member_create_replays_naming_the_start_and_brief_it_cannot_confirm() {
    let root = tempfile::tempdir().unwrap();
    let mut supervisor = test_supervisor_with_root(root.path().to_path_buf());
    let claude = test_session_id(&supervisor, "claude");
    let mut caller = mobility_pane(&supervisor, "claude", 9521);
    let room = mobility_room(&supervisor, "Alpha", vec![claude]);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    // No PTY is queued: a start that is attempted fails, and says so.
    queue_mock_starts(&supervisor, 0);
    let request_id = Uuid::new_v4().to_string();
    let action = shared_types::MemberAction::Create { codex_transport: Default::default(), grants: None, driver: DriverKind::Codex, label: "QA Codex".into(),
        permission_profile: shared_types::PermissionProfile::Normal, brief: true, brief_text: Some("Check the build.".into()),
        start: true, room_id: None };

    // The worker dies once the new member has joined: before its start and brief.
    supervisor.set_room_event_after_append_for_tests(|| panic!("interrupted after the join"));
    let interrupted = member_request(&supervisor, &caller, &request_id, action.clone());
    assert!(!interrupted.ok && interrupted.payload.is_none(), "{}", interrupted.message);
    let qa = *room_now(&supervisor, room.room_id).member_ids.iter().find(|id| **id != claude).expect("the member joined");
    let sessions = supervisor.snapshot().sessions.len();

    let unconfirmed = |result: &shared_types::MemberActionResult| {
        assert!(result.replayed && result.member.session_id == qa && result.brief.is_none(), "{result:?}");
        assert_eq!(result.incomplete, [shared_types::MemberFollowUp::Start, shared_types::MemberFollowUp::Brief]);
        assert_eq!(result.warnings, ["the member was created, but its start and brief may not have happened; check it and use member_start / member_brief"]);
    };
    let response = member_request(&supervisor, &caller, &request_id, action.clone());
    assert!(response.message.contains("may not have happened"), "{}", response.message);
    unconfirmed(&member_done(response));
    assert_eq!(supervisor.snapshot().sessions.len(), sessions, "the retry created nothing");
    assert!(!session_now(&supervisor, qa).running, "the retry started nothing");
    assert!(mobility_messages(&supervisor, room.room_id).is_empty(), "the retry briefed and noticed nothing");

    // After a PRIM-1 restart the retry says the same.
    drop(caller);
    drop(supervisor);
    supervisor = SupervisorHandle::new(test_supervisor_config(root.path())).unwrap();
    supervisor.set_executable_resolver_for_tests(Arc::new(TestExecutableResolver));
    caller = mobility_pane(&supervisor, "claude", 9522);
    unconfirmed(&member_done(member_request(&supervisor, &caller, &request_id, action)));

    // A first run whose start fails names it too.
    queue_mock_starts(&supervisor, 0);
    let failed = member_done(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(),
        shared_types::MemberAction::Create { codex_transport: Default::default(), grants: None, driver: DriverKind::Grok, label: "QA Grok".into(),
            permission_profile: shared_types::PermissionProfile::Normal, brief: false, brief_text: None, start: true,
            room_id: None }));
    assert_eq!(failed.incomplete, [shared_types::MemberFollowUp::Start]);
    assert!(failed.warnings.iter().any(|warning| warning.starts_with("created but not started")), "{failed:?}");
}

#[test]
fn member_create_uses_bypass_permissions_only_when_asked_and_can_start_and_send_the_room_brief() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let caller = mobility_pane(&supervisor, "claude", 9471);
    let room = mobility_room(&supervisor, "Alpha", vec![claude]);
    supervisor.set_member_permissions(claude, manage_grant(true)).unwrap();
    queue_mock_starts(&supervisor, 1);

    let safe = member_done(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(),
        create_action(DriverKind::Grok, "Safe Grok")));
    assert_eq!(safe.member.permission_profile, shared_types::PermissionProfile::Normal);

    let bypass = member_done(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(),
        shared_types::MemberAction::Create { codex_transport: Default::default(), grants: None, driver: DriverKind::Grok, label: "QA Grok".into(),
            permission_profile: shared_types::PermissionProfile::Unsafe, brief: true, brief_text: None, start: true, room_id: None }));
    assert_eq!(bypass.member.permission_profile, shared_types::PermissionProfile::Unsafe);
    assert!(bypass.member.running, "{bypass:?}");
    assert_eq!(bypass.member.permissions, MemberPermissions::default(), "a managing member grants nothing");
    // The room brief, rendered for the new member, waits in the room queue.
    let brief = bypass.brief.unwrap_or_else(|| panic!("room brief delivery: {:?}", bypass.warnings));
    assert_eq!(brief.recipient_count, 1);
    let body = last_stored_room_body(&supervisor, bypass.member.session_id);
    assert!(body.contains("Your label is \"QA Grok\"") && body.contains("room \"Alpha\""), "{body}");
    assert!(!body.contains("member_create"), "the new member has no manage grant: {body}");
    assert_eq!(mobility_messages(&supervisor, room.room_id).last(),
        Some(&(Some(claude), "claude added QA Grok (Grok, bypass permissions, briefed)".to_string())));
}

#[test]
fn member_remove_stops_a_running_member_then_removes_it_and_keeps_the_session() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let grok = create_test_session(&supervisor, "grok", DriverKind::Grok, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9481);
    let (codex_pty, _, kills) = mock_pty_session(Some(std::process::id()), MockKillBehavior::Immediate);
    install_mock_running_session(&supervisor, "codex", DriverKind::Codex, codex_pty);
    let room = mobility_room(&supervisor, "Alpha", vec![claude, codex, grok]);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();

    let removed = member_done(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), remove_action("codex")));
    assert_eq!(kills.load(Ordering::SeqCst), 1, "stopped first");
    assert!(!removed.member.running);
    assert_eq!(room_of(&supervisor, codex), None);
    assert_eq!(room_now(&supervisor, room.room_id).member_ids, [claude, grok]);
    assert!(supervisor.snapshot().sessions.iter().any(|session| session.session_id == codex), "deleting stays the operator's");
    assert_eq!(membership_items(&supervisor, room.room_id, codex).last().map(|item| item.0), Some(RoomMembershipAction::Removed));

    // A stopped member is only removed.
    member_done(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), remove_action("grok")));
    assert_eq!(room_now(&supervisor, room.room_id).member_ids, [claude]);
    assert_eq!(mobility_messages(&supervisor, room.room_id), [
        (Some(claude), "claude stopped and removed codex".to_string()),
        (Some(claude), "claude removed grok".to_string()),
    ]);
    member_refused(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), remove_action("grok")),
        "no member of room \"Alpha\" is labelled 'grok'");
}

#[test]
fn member_brief_sends_the_room_brief_or_given_text_from_the_caller_with_a_notice() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let caller = mobility_pane(&supervisor, "claude", 9491);
    let (codex_pty, codex_inputs) = recording_pty_session(9492);
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, codex_pty);
    let room = mobility_room(&supervisor, "Alpha", vec![claude, codex]);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();

    let request_id = Uuid::new_v4().to_string();
    let custom = member_done(member_request(&supervisor, &caller, &request_id, brief_action("codex", Some("Review PR 12."))));
    assert_eq!(custom.brief.as_ref().unwrap().written_count, 1);
    assert_eq!(*codex_inputs.lock(), vec![typed_room_message("claude", "Review PR 12."), "\x1b[C\r".to_string()]);
    let replay = member_done(member_request(&supervisor, &caller, &request_id, brief_action("codex", Some("Review PR 12."))));
    assert!(replay.replayed);
    assert_eq!(codex_inputs.lock().len(), 2, "a retry is not a second brief");

    let room_brief = member_done(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), brief_action("codex", None)));
    // Typed at once, or queued behind the first wake and typed once ready.
    wait_for_room_receipt(&supervisor, room.room_id, room_brief.brief.as_ref().unwrap().message_id, codex,
        RoomDeliveryStatus::Written);
    assert_eq!(codex_inputs.lock().len(), 4);
    let typed = codex_inputs.lock()[2].clone();
    assert!(typed.contains("[Room message from claude]") && typed.contains("Your label is \"codex\""), "{typed}");
    let notices: Vec<_> = mobility_messages(&supervisor, room.room_id).into_iter()
        .filter(|(_, content)| content.starts_with("claude briefed")).collect();
    assert_eq!(notices, [(Some(claude), "claude briefed codex".to_string()), (Some(claude), "claude briefed codex".to_string())]);
}

#[test]
fn the_room_brief_names_the_member_tools_only_to_managing_members() {
    let claude = RoomMember { session_id: Uuid::new_v4(), label: "CC".into(), driver: DriverKind::Claude };
    let grok = RoomMember { session_id: Uuid::new_v4(), label: "Grok".into(), driver: DriverKind::Grok };
    let members = vec![claude.clone(), grok.clone()];
    let plain = render_room_brief("Team A", &members, &claude, MemberPermissions::default(), None, None);
    assert!(!plain.contains("member_") && !plain.contains("{managing}"), "{plain}");
    let managing = render_room_brief("Team A", &members, &claude, manage_grant(false), None, None);
    for tool in ["member_create", "member_start", "member_stop", "member_restart", "member_brief", "member_remove",
        "members_list", "member_update", "member_delete", "resume_here", "request_id", "permission_profile \"unsafe\""] {
        assert!(managing.contains(tool), "{tool}: {managing}");
    }
    assert!(!managing.contains("other rooms"), "{managing}");
    assert!(render_room_brief("Team A", &members, &claude, manage_grant(true), None, None).contains("other rooms"));
    let grok_managing = render_room_brief("Team A", &members, &grok, manage_grant(false), None, None);
    assert!(grok_managing.contains("--prim1-room member create") && grok_managing.contains("--unsafe"), "{grok_managing}");
    for form in ["members lists them", "--fresh | --resume-here", "member update", "--grants"] {
        assert!(grok_managing.contains(form), "{form}: {grok_managing}");
    }
    assert!(validate_message_framing(&grok_managing, routed_message_submit_behavior(DriverKind::Grok)).is_ok());
    // An edited brief from before the placeholder still tells a managing member.
    assert!(render_room_brief("Team A", &members, &claude, manage_grant(false), None, Some("Tools: {tools}")).contains("member_create"));
}
