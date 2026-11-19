// Included in the supervisor test module. A16 native agent parity: the rest of
// the operator's member actions (grants at create, the Edit dialog, Start
// fresh, Resume here, add a lobby session, Close session, the session list)
// through the operator's own supervisor methods, with their rechecks, notices
// and replays. Mock PTYs, test pane processes and temporary catalogs only.

const SUPERVISOR: MemberPermissions = MemberPermissions { roam: true, maintain: true, manage: true };

fn update_action(member: &str, label: Option<&str>, profile: Option<shared_types::PermissionProfile>,
    grants: Option<MemberPermissions>) -> shared_types::MemberAction {
    shared_types::MemberAction::Update { member: member.into(), label: label.map(Into::into), permission_profile: profile, grants,
        working_directory: None, position: None }
}

fn start_mode_action(member: &str, mode: shared_types::StartMode) -> shared_types::MemberAction {
    shared_types::MemberAction::Start { member: member.into(), mode, launch_choices: None, search: false }
}

fn add_action(member: &str, room_id: Option<RoomId>) -> shared_types::MemberAction {
    shared_types::MemberAction::Add { member: member.into(), room_id }
}

fn delete_action(member: &str) -> shared_types::MemberAction {
    shared_types::MemberAction::Delete { member: member.into() }
}

fn members_listed(supervisor: &SupervisorHandle, caller: &PaneCaller) -> Vec<(SessionId, Option<RoomId>)> {
    let response = supervisor.apply_sideband_request(caller, SidebandRequest::MembersList {});
    assert!(response.ok, "{}", response.message);
    match response.payload {
        Some(SidebandResponsePayload::Members { members }) =>
            members.into_iter().map(|entry| (entry.session.session_id, entry.room_id)).collect(),
        other => panic!("expected the member list, got {other:?}"),
    }
}

fn fresh_id() -> String {
    Uuid::new_v4().to_string()
}

#[test]
fn a16_member_create_saves_grants_with_the_session_and_names_them() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let caller = mobility_pane(&supervisor, "claude", 9601);
    let room = mobility_room(&supervisor, "Alpha", vec![claude]);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();

    let lead = shared_types::MemberAction::Create { codex_transport: Default::default(), grants: Some(SUPERVISOR), driver: DriverKind::Codex,
        label: "Lead".into(), permission_profile: shared_types::PermissionProfile::Unsafe, brief: false,
        brief_text: None, start: false, room_id: None };
    let created = member_done(member_request(&supervisor, &caller, &fresh_id(), lead));
    // No ceiling: a member holding only manage gives all three.
    assert_eq!(created.member.permissions, SUPERVISOR);
    assert_eq!(member_permissions(&supervisor, created.member.session_id), SUPERVISOR);
    let worker = member_done(member_request(&supervisor, &caller, &fresh_id(), create_action(DriverKind::Grok, "Worker")));
    assert_eq!(worker.member.permissions, MemberPermissions::default(), "a worker gets none unless asked");
    assert_eq!(mobility_messages(&supervisor, room.room_id), [
        (Some(claude), "claude added Lead (Codex, bypass permissions, grants roam, maintain, manage, not started)".to_string()),
        (Some(claude), "claude added Worker (Grok, normal permissions, not started)".to_string()),
    ]);

    // The operator's create takes grants too; Terminal and Prime refuse any.
    let operator = supervisor.create_session(shared_types::CreateSessionRequest { codex_transport: Default::default(), permissions: Some(SUPERVISOR),
        launch_choices: None, room_id: None, label: Some("Supervisor".into()), driver: DriverKind::Claude,
        permission_profile: shared_types::PermissionProfile::Normal, linux_working_directory: None }).unwrap();
    assert_eq!(operator.permissions, SUPERVISOR);
    let refused = supervisor.create_session(shared_types::CreateSessionRequest { codex_transport: Default::default(), permissions: Some(ROAM),
        launch_choices: None, room_id: None, label: Some("shell".into()), driver: DriverKind::GenericTerminal,
        permission_profile: shared_types::PermissionProfile::Normal, linux_working_directory: None }).unwrap_err();
    assert!(refused.to_string().contains("do nothing for them"), "{refused}");
    assert!(supervisor.snapshot().sessions.iter().all(|session| session.label != "shell"), "nothing was created");
}

#[test]
fn a16_member_update_applies_the_edit_dialog_with_one_notice_and_may_edit_the_caller() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let helper = create_test_session(&supervisor, "helper", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9611);
    let room = mobility_room(&supervisor, "Alpha", vec![claude, codex, helper]);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();

    let id = fresh_id();
    let edit = update_action("helper", Some("Helper Two"), Some(shared_types::PermissionProfile::Unsafe), Some(SUPERVISOR));
    let updated = member_done(member_request(&supervisor, &caller, &id, edit.clone()));
    assert_eq!((updated.op.as_str(), updated.member.label.as_str()), ("update", "Helper Two"));
    assert_eq!(updated.member.permission_profile, shared_types::PermissionProfile::Unsafe);
    assert_eq!(updated.member.permissions, SUPERVISOR);
    assert!(member_done(member_request(&supervisor, &caller, &id, edit)).replayed);
    // Already as asked: nothing changes and no notice.
    let same = member_done(member_request(&supervisor, &caller, &fresh_id(),
        update_action("Helper Two", Some("Helper Two"), None, None)));
    assert!(same.warnings.iter().any(|warning| warning.contains("nothing changed")), "{same:?}");
    // Labels address members: a rename keeps them unique in the room.
    member_refused(member_request(&supervisor, &caller, &fresh_id(), update_action("Helper Two", Some("CODEX"), None, None)),
        "already has a member labelled 'CODEX'");
    member_refused(member_request(&supervisor, &caller, &fresh_id(), update_action("Helper Two", None, None, None)),
        "needs label, permission_profile, grants, working_directory or position");
    // The caller may edit itself: no lifecycle constraint applies to an edit.
    let me = member_done(member_request(&supervisor, &caller, &fresh_id(), update_action("claude", None, None, Some(SUPERVISOR))));
    assert_eq!(me.member.session_id, claude);
    assert_eq!(member_permissions(&supervisor, claude), SUPERVISOR);
    // The profile changes only while stopped, as in the dialog.
    let running = member_refused_message(member_request(&supervisor, &caller, &fresh_id(),
        update_action("claude", None, Some(shared_types::PermissionProfile::Unsafe), None)));
    assert!(running.contains("stop") || running.contains("closed"), "{running}");

    assert_eq!(mobility_messages(&supervisor, room.room_id), [
        (Some(claude), "claude updated helper: renamed to Helper Two; bypass permissions; grants roam, maintain, manage".to_string()),
        (Some(claude), "claude updated claude: grants roam, maintain, manage".to_string()),
    ]);
}

fn member_refused_message(response: SidebandResponse) -> String {
    assert!(!response.ok, "expected a refusal: {response:?}");
    assert!(response.payload.is_none());
    response.message
}

#[test]
fn a16_member_tools_without_manage_or_across_rooms_without_roam_change_nothing() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let lobby = create_test_session(&supervisor, "Loner", DriverKind::Codex, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9621);
    mobility_room(&supervisor, "Alpha", vec![claude]);
    let beta = mobility_room(&supervisor, "Beta", vec![codex]);
    supervisor.set_member_permissions(claude, MemberPermissions { roam: true, maintain: true, manage: false }).unwrap();
    // Every session but the caller, whose grants this test changes itself.
    let others = |supervisor: &SupervisorHandle| supervisor.snapshot().sessions.into_iter()
        .filter(|session| session.session_id != claude)
        .map(|session| (session.session_id, session.label, session.permission_profile, session.permissions))
        .collect::<Vec<_>>();
    let catalog_before = others(&supervisor);

    // Roam and maintain are not manage.
    for action in [update_action(&codex.to_string(), Some("x"), None, Some(SUPERVISOR)), delete_action(&codex.to_string()),
        add_action(&lobby.to_string(), None), start_mode_action(&codex.to_string(), shared_types::StartMode::Fresh)] {
        member_refused(member_request(&supervisor, &caller, &fresh_id(), action), "Can manage agents");
    }
    let listed = supervisor.apply_sideband_request(&caller, SidebandRequest::MembersList {});
    assert!(!listed.ok && listed.message.contains("Can manage agents"), "{}", listed.message);

    // Manage alone stays in its room: another room and the lobby need roam.
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    member_refused(member_request(&supervisor, &caller, &fresh_id(), update_action(&codex.to_string(), None, None, Some(SUPERVISOR))),
        "also needs \"Can roam\"");
    member_refused(member_request(&supervisor, &caller, &fresh_id(), delete_action(&codex.to_string())), "also needs \"Can roam\"");
    member_refused(member_request(&supervisor, &caller, &fresh_id(), delete_action(&lobby.to_string())), "lobby is outside your room");
    member_refused(member_request(&supervisor, &caller, &fresh_id(), add_action(&lobby.to_string(), None)), "lobby is outside your room");
    assert_eq!(others(&supervisor), catalog_before, "nothing changed");
    assert_eq!(room_of(&supervisor, lobby), None);
    assert_eq!(room_now(&supervisor, beta.room_id).member_ids, [codex]);
}

#[test]
fn a16_a_grant_withdrawn_before_the_effect_leaves_nothing_done() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let helper = create_test_session(&supervisor, "helper", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let lobby = create_test_session(&supervisor, "Loner", DriverKind::Codex, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9631);
    let room = mobility_room(&supervisor, "Alpha", vec![claude, helper]);
    for action in [update_action("helper", Some("renamed"), Some(shared_types::PermissionProfile::Unsafe), Some(SUPERVISOR)),
        delete_action("helper"), add_action(&lobby.to_string(), None),
        start_mode_action("helper", shared_types::StartMode::Fresh)] {
        supervisor.set_member_permissions(claude, SUPERVISOR).unwrap();
        let withdraw = supervisor.clone();
        *supervisor.inner.member_before_effect.lock() = Some(Box::new(move || {
            withdraw.set_member_permissions(claude, MemberPermissions::default()).unwrap();
        }));
        member_refused(member_request(&supervisor, &caller, &fresh_id(), action.clone()), "Can manage agents");
        assert!(supervisor.inner.member_before_effect.lock().is_none(), "{action:?} reached its effect");
    }
    let after = session_now(&supervisor, helper);
    assert_eq!((after.label.as_str(), after.permission_profile, after.permissions),
        ("helper", shared_types::PermissionProfile::Normal, MemberPermissions::default()));
    assert_eq!(room_now(&supervisor, room.room_id).member_ids, [claude, helper]);
    assert_eq!(room_of(&supervisor, lobby), None);
    assert!(mobility_messages(&supervisor, room.room_id).is_empty());
}

#[test]
fn a16_member_start_fresh_stops_a_running_member_first_and_resume_here_needs_a_copied_conversation() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let helper = create_test_session(&supervisor, "helper", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9641);
    let room = mobility_room(&supervisor, "Alpha", vec![claude, helper]);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    queue_mock_starts(&supervisor, 3);

    let first = member_done(member_request(&supervisor, &caller, &fresh_id(), start_action("helper")));
    assert!(first.member.running);
    let fresh = member_done(member_request(&supervisor, &caller, &fresh_id(),
        start_mode_action("helper", shared_types::StartMode::Fresh)));
    assert!(fresh.member.running && fresh.member.generation > first.member.generation, "{fresh:?}");
    // Launch choices change only for a stopped member, unless fresh.
    let choices = shared_types::MemberAction::Start { member: "helper".into(), mode: shared_types::StartMode::Resume,
        launch_choices: Some(SessionLaunchChoices { effort: Some("high".into()), ..Default::default() }), search: false };
    member_refused(member_request(&supervisor, &caller, &fresh_id(), choices), "launch choices change only for a stopped member");
    // Resume here releases only a copied conversation's hold.
    member_done(member_request(&supervisor, &caller, &fresh_id(), stop_action("helper")));
    member_refused(member_request(&supervisor, &caller, &fresh_id(), start_mode_action("helper", shared_types::StartMode::ResumeHere)),
        "no copied conversation waiting to resume");
    assert!(!session_now(&supervisor, helper).running);

    assert_eq!(mobility_messages(&supervisor, room.room_id), [
        (Some(claude), "claude started helper".to_string()),
        (Some(claude), "claude stopped and started fresh helper".to_string()),
        (Some(claude), "claude stopped helper".to_string()),
    ]);
}

#[test]
fn a16_member_add_puts_a_lobby_session_in_a_room_and_member_delete_closes_room_and_lobby_sessions() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let helper = create_test_session(&supervisor, "helper", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let spare = create_test_session(&supervisor, "spare", DriverKind::Codex, shared_types::PermissionProfile::Normal).session_id;
    let stale = create_test_session(&supervisor, "stale", DriverKind::Codex, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9651);
    let room = mobility_room(&supervisor, "Alpha", vec![claude, helper]);
    supervisor.set_member_permissions(claude, SUPERVISOR).unwrap();
    queue_mock_starts(&supervisor, 1);

    let added = member_done(member_request(&supervisor, &caller, &fresh_id(), add_action(&spare.to_string(), None)));
    assert_eq!((added.op.as_str(), added.room_id), ("add", room.room_id));
    assert_eq!(room_of(&supervisor, spare), Some(room.room_id));
    member_refused(member_request(&supervisor, &caller, &fresh_id(), add_action("spare", None)), "already in room");

    // A running room member: stopped, out of the room, closed.
    member_done(member_request(&supervisor, &caller, &fresh_id(), start_action("helper")));
    let id = fresh_id();
    let closed = member_done(member_request(&supervisor, &caller, &id, delete_action("helper")));
    assert_eq!((closed.op.as_str(), closed.member.session_id, closed.member.running), ("delete", helper, false));
    assert!(supervisor.snapshot().sessions.iter().all(|session| session.session_id != helper));
    assert!(member_done(member_request(&supervisor, &caller, &id, delete_action("helper"))).replayed,
        "a retry returns the first result and closes nothing else");
    // A lobby session, by its id: the caller's room records it.
    member_done(member_request(&supervisor, &caller, &fresh_id(), delete_action(&stale.to_string())));
    assert!(supervisor.snapshot().sessions.iter().all(|session| session.session_id != stale));
    // Never the caller itself: closing it would end the run that asked.
    member_refused(member_request(&supervisor, &caller, &fresh_id(), delete_action("claude")), "itself");

    assert_eq!(mobility_messages(&supervisor, room.room_id), [
        (Some(claude), "claude added spare to the room".to_string()),
        (Some(claude), "claude started helper".to_string()),
        (Some(claude), "claude stopped, removed and closed helper".to_string()),
        (Some(claude), "claude closed stale".to_string()),
    ]);
    assert_eq!(room_now(&supervisor, room.room_id).member_ids, [claude, spare]);
}

#[test]
fn a16_members_list_shows_the_callers_room_and_with_roam_every_session() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let helper = create_test_session(&supervisor, "helper", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let lobby = create_test_session(&supervisor, "Loner", DriverKind::Codex, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9661);
    let alpha = mobility_room(&supervisor, "Alpha", vec![claude, helper]);
    let beta = mobility_room(&supervisor, "Beta", vec![codex]);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();

    assert_eq!(members_listed(&supervisor, &caller), [(claude, Some(alpha.room_id)), (helper, Some(alpha.room_id))]);
    supervisor.set_member_permissions(claude, manage_grant(true)).unwrap();
    let everywhere = members_listed(&supervisor, &caller);
    for wanted in [(claude, Some(alpha.room_id)), (codex, Some(beta.room_id)), (helper, Some(alpha.room_id)), (lobby, None)] {
        assert!(everywhere.contains(&wanted), "{wanted:?} missing from {everywhere:?}");
    }
    assert_eq!(everywhere.len(), supervisor.snapshot().sessions.len());
}

#[test]
fn a16_member_input_types_once_for_a_manager_and_the_room_sees_metadata_unless_marked_safe() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let stopped = create_test_session(&supervisor, "Idle", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9671);
    let (_target, typed) = mobility_wake_pane(&supervisor, "codex", 9672);
    let room = mobility_room(&supervisor, "Alpha", vec![claude, codex, stopped]);
    let input = |member: &str, text: Option<&str>, keys: Vec<ControlKey>, show_text: bool| shared_types::MemberAction::Input {
        member: member.into(), text: text.map(Into::into), keys, show_text };

    // Without manage nothing is typed.
    member_refused(member_request(&supervisor, &caller, &fresh_id(), input("codex", Some("y"), vec![ControlKey::Enter], false)),
        "Can manage agents");
    assert!(typed.lock().is_empty());
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();

    // By default the room sees the length and keys, never the text.
    let secretish = "hunter2-token";
    let id = fresh_id();
    let action = input("codex", Some(secretish), vec![ControlKey::Enter], false);
    let done = member_done(member_request(&supervisor, &caller, &id, action.clone()));
    assert_eq!(done.op, "input");
    assert_eq!(typed.lock().concat(), format!("{secretish}\r"));
    assert!(!serde_json::to_string(&done).unwrap().contains(secretish), "the result carries no text");
    // A retry repeats nothing.
    assert!(member_done(member_request(&supervisor, &caller, &id, action)).replayed);
    assert_eq!(typed.lock().concat(), format!("{secretish}\r"));
    // Marked not sensitive, the text is shown; keys alone are named.
    let safe = fresh_id();
    member_done(member_request(&supervisor, &caller, &safe, input("codex", Some("y"), vec![ControlKey::Enter], true)));
    let keys = fresh_id();
    member_done(member_request(&supervisor, &caller, &keys, input("codex", None, vec![ControlKey::Down, ControlKey::Enter], false)));
    assert_eq!(typed.lock().concat(), format!("{secretish}\ry\r\x1b[B\r"));

    // Refusals type nothing.
    member_refused(member_request(&supervisor, &caller, &fresh_id(), input("codex", Some("two\nlines"), vec![], false)),
        "no control characters");
    member_refused(member_request(&supervisor, &caller, &fresh_id(), input("codex", None, vec![], false)), "give text, keys or both");
    member_refused(member_request(&supervisor, &caller, &fresh_id(), input("claude", Some("x"), vec![], false)),
        "never act on the calling member itself");
    member_refused(member_request(&supervisor, &caller, &fresh_id(), input("Idle", Some("x"), vec![], false)), "not running");
    assert_eq!(typed.lock().concat(), format!("{secretish}\ry\r\x1b[B\r"));

    let messages = mobility_messages(&supervisor, room.room_id);
    assert!(messages.iter().all(|(_, text)| !text.contains(secretish)), "{messages:?}");
    assert_eq!(messages, [
        (Some(claude), format!("claude typed into codex: 13 characters, then Enter (request {id})")),
        (Some(claude), format!("claude typed into codex: \"y\", then Enter (request {safe})")),
        (Some(claude), format!("claude typed into codex: pressed Down, Enter (request {keys})")),
    ]);
}

#[test]
fn a16_member_update_moves_a_stopped_members_folder_and_its_place_among_the_listed_sessions() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let helper = create_test_session(&supervisor, "helper", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9681);
    let room = mobility_room(&supervisor, "Alpha", vec![claude, helper]);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    let edit = |member: &str, working_directory: Option<String>, position: Option<u32>| shared_types::MemberAction::Update {
        member: member.into(), label: None, permission_profile: None, grants: None, working_directory, position };
    let folder = tempfile::tempdir().unwrap();
    let folder_name = folder.path().file_name().unwrap().to_string_lossy().into_owned();

    // A stopped member's folder, as the Edit dialog sets it.
    let moved = member_done(member_request(&supervisor, &caller, &fresh_id(),
        edit("helper", Some(folder.path().display().to_string()), None)));
    assert!(moved.member.working_dir.ends_with(&folder_name), "{}", moved.member.working_dir);
    // Only while stopped: the running caller's own folder is refused.
    let running = member_refused_message(member_request(&supervisor, &caller, &fresh_id(),
        edit("claude", Some(folder.path().display().to_string()), None)));
    assert!(running.contains("stop") || running.contains("closed"), "{running}");

    // Position counts among the sessions this manage-only caller lists: its room.
    let listed = |supervisor: &SupervisorHandle| members_listed(supervisor, &caller).into_iter().map(|(id, _)| id).collect::<Vec<_>>();
    assert_eq!(listed(&supervisor), [claude, helper]);
    member_done(member_request(&supervisor, &caller, &fresh_id(), edit("helper", None, Some(0))));
    assert_eq!(listed(&supervisor), [helper, claude]);
    let same = member_done(member_request(&supervisor, &caller, &fresh_id(), edit("helper", None, Some(0))));
    assert!(same.warnings.iter().any(|warning| warning.contains("nothing changed")), "{same:?}");
    member_refused(member_request(&supervisor, &caller, &fresh_id(), edit("helper", None, Some(2))), "beyond the 2 sessions");

    let messages = mobility_messages(&supervisor, room.room_id);
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(messages[0].1.starts_with("claude updated helper: working directory ") && messages[0].1.ends_with(&folder_name),
        "{messages:?}");
    assert_eq!(messages[1], (Some(claude), "claude updated helper: moved to position 0".to_string()));
}

#[test]
fn a16_room_retry_attempts_one_held_wake_again_for_a_manager_and_never_reposts() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let caller = mobility_pane(&supervisor, "claude", 9691);
    let (codex_pty, codex_inputs) = recording_pty_session(9692);
    install_mock_room_wake_recipient(&supervisor, "codex", codex_pty);
    let alpha = mobility_room(&supervisor, "Alpha", vec![claude, codex]);
    // The wake's Pending receipt is torn: no transport, Held after recovery.
    let inner = Arc::downgrade(&supervisor.inner);
    let alpha_id = alpha.room_id;
    supervisor.set_room_event_after_append_for_tests(move || {
        inner.upgrade().unwrap().rooms.lock().get_mut(alpha_id).unwrap().fail_next_commit_for_tests(false);
    });
    let deliver = || supervisor.apply_sideband_request(&caller, SidebandRequest::RoomDeliver {
        request_id: Some("held-once".into()), recipient: "codex".into(), content: "held at first".into() });
    let first = deliver();
    let Some(SidebandResponsePayload::RoomDelivery { result: first }) = first.payload else { panic!("{}", first.message) };
    assert!(deliver().ok, "the replay records the Held receipt");
    assert!(codex_inputs.lock().is_empty());
    let receipt = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: alpha.room_id, cursor: None }).unwrap()
        .events.last().unwrap().cursor;
    let retry = |request_id: &str| supervisor.apply_sideband_request(&caller, SidebandRequest::Room {
        request_id: Some(request_id.into()), action: shared_types::RoomAction::Retry { room_id: None,
            message_id: first.message_id, recipient: "codex".into(), receipt } });

    // Without manage: refused, nothing typed.
    let refused = retry(&fresh_id());
    assert!(!refused.ok && refused.message.contains("Can manage agents"), "{}", refused.message);
    assert!(codex_inputs.lock().is_empty());

    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    let id = fresh_id();
    let done = retry(&id);
    let Some(SidebandResponsePayload::Room { result }) = done.payload else { panic!("{}", done.message) };
    let delivery = result.delivery.expect("the attempt's delivery result");
    assert_eq!((result.op.as_str(), delivery.written_count), ("retry", 1));
    assert_eq!(delivery.message_id, first.message_id, "the same stored message, never a second post");
    let typed = codex_inputs.lock().clone();
    assert!(!typed.is_empty());
    // A retry of the same request repeats nothing.
    let again = retry(&id);
    assert!(again.ok && again.message.contains("replayed"), "{}", again.message);
    assert_eq!(*codex_inputs.lock(), typed);

    let messages = mobility_messages(&supervisor, alpha.room_id);
    assert_eq!(messages.iter().filter(|(_, text)| text == "held at first").count(), 1, "{messages:?}");
    assert_eq!(messages.last().unwrap(),
        &(Some(claude), format!("claude retried the wake of message {} for codex", first.message_id)));
}

#[test]
fn a16_member_input_rechecks_the_grant_at_the_input_boundary_and_a_partial_write_is_never_retyped() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let caller = mobility_pane(&supervisor, "claude", 9701);
    let (_target, typed) = mobility_wake_pane(&supervisor, "codex", 9702);
    let room = mobility_room(&supervisor, "Alpha", vec![claude, codex]);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    let input = |text: &str| shared_types::MemberAction::Input { member: "codex".into(), text: Some(text.into()),
        keys: vec![ControlKey::Enter], show_text: false };

    // Manage withdrawn while the write waits for the run's writer: nothing typed.
    let revoker = supervisor.clone();
    supervisor.set_run_input_before_commit_for_tests(move || {
        revoker.set_member_permissions(claude, MemberPermissions::default()).unwrap();
    });
    member_refused(member_request(&supervisor, &caller, &fresh_id(), input("approve")), "Can manage agents");
    assert!(typed.lock().is_empty(), "the grant is checked at the input boundary");
    assert!(mobility_messages(&supervisor, room.room_id).is_empty(), "no notice for input that never happened");

    // A short write: a partial effect with a metadata notice, and a retry never types again.
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex,
        Box::new(A16ShortWritePty { process_id: 9702, accepted: 2 }));
    let id = fresh_id();
    let first = member_refused_message(member_request(&supervisor, &caller, &id, input("hello")));
    assert!(first.contains("may have received part of that input"), "{first}");
    assert!(!first.contains("hello"), "the outcome holds no text: {first}");
    let again = member_refused_message(member_request(&supervisor, &caller, &id, input("hello")));
    assert!(again.starts_with(&first) && again.contains("replayed"), "the same request_id replays the partial outcome: {again}");
    let messages = mobility_messages(&supervisor, room.room_id);
    assert_eq!(messages, [(Some(claude),
        format!("claude typed into codex: 5 characters, then Enter, but the write did not complete (request {id})"))]);
}

/// Accepts only a prefix of every write, like `ShortOkPtySession`, with a
/// process identity so the caller's pane check can walk past it.
struct A16ShortWritePty {
    process_id: u32,
    accepted: usize,
}

impl PtySessionTrait for A16ShortWritePty {
    fn send_input(&self, _input: &str) -> pty_host::PtyWriteResult {
        Ok(self.accepted)
    }

    fn resize(&self, _cols: u16, _rows: u16) -> Result<()> {
        Ok(())
    }

    fn kill(&self) -> Result<()> {
        Ok(())
    }

    fn try_wait(&self) -> Result<Option<pty_host::PtyExitStatus>> {
        Ok(None)
    }

    fn process_id(&self) -> Option<u32> {
        Some(self.process_id)
    }
}

#[test]
fn a16_lobby_sessions_start_stop_restart_and_update_with_roam_recorded_in_the_callers_room() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let caller = mobility_pane(&supervisor, "claude", 9811);
    let room = mobility_room(&supervisor, "Home", vec![claude]);
    let other = mobility_room(&supervisor, "Other", Vec::new());
    let loner = create_test_session(&supervisor, "Loner", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let id = loner.to_string();
    queue_mock_starts(&supervisor, 2);

    // Manage without roam: a session in the lobby is outside the caller's room.
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    for action in [start_action(&id), stop_action(&id), restart_action(&id), update_action(&id, Some("Solo"), None, None)] {
        member_refused(member_request(&supervisor, &caller, &fresh_id(), action), "Can roam");
    }
    supervisor.set_member_permissions(claude, manage_grant(true)).unwrap();

    let started = member_done(member_request(&supervisor, &caller, &fresh_id(), start_action(&id)));
    assert_eq!(started.room_id, room.room_id, "the caller's room records it");
    assert!(session_now(&supervisor, loner).running);
    assert_eq!(room_of(&supervisor, loner), None, "it stays in the lobby");
    member_done(member_request(&supervisor, &caller, &fresh_id(), restart_action(&id)));
    assert!(session_now(&supervisor, loner).running);
    let updated = member_done(member_request(&supervisor, &caller, &fresh_id(), update_action(&id, Some("Solo"), None, None)));
    assert_eq!(updated.member.label, "Solo");
    member_done(member_request(&supervisor, &caller, &fresh_id(), stop_action(&id)));
    assert!(!session_now(&supervisor, loner).running);
    assert_eq!(room_of(&supervisor, loner), None);
    let notices = mobility_messages(&supervisor, room.room_id).into_iter().map(|(_, text)| text).collect::<Vec<_>>();
    assert_eq!(notices, ["claude started Loner", "claude restarted Loner", "claude updated Loner: renamed to Solo", "claude stopped Solo"]);

    // Brief and input keep to room members.
    member_refused(member_request(&supervisor, &caller, &fresh_id(), brief_action(&id, Some("x"))), "in the lobby");
    // The operator entry has no room of its own to record it in.
    let local = operator_exchange(&supervisor, JobAttribution::Outside(4242), None,
        &SidebandRequest::Member { request_id: fresh_id(), action: start_action(&id) });
    assert!(!local.ok && local.message.contains("member add <session id> --room"), "{local:?}");
    assert!(!session_now(&supervisor, loner).running);

    // Put into a room after the request was resolved: the lobby recheck refuses the effect.
    let mover = supervisor.clone();
    *supervisor.inner.member_before_effect.lock() = Some(Box::new(move || {
        mover.add_room_member(AddRoomMemberRequest { room_id: other.room_id, session_id: loner }).unwrap();
    }));
    member_refused(member_request(&supervisor, &caller, &fresh_id(), start_action(&id)), "no longer in the lobby");
    assert!(!session_now(&supervisor, loner).running, "nothing started");
}
