// Included in the supervisor test module. A16 phase 2: the room actions
// (create, settings, delete, start/stop, guests) for a managing pane, the
// operator entry for a same-user local process outside every pane, and the
// Reviewer's four phase-1 repros. Mock PTYs, test panes, temporary catalogs.

fn room_request(supervisor: &SupervisorHandle, caller: &PaneCaller, request_id: Option<&str>, action: shared_types::RoomAction)
    -> SidebandResponse {
    supervisor.apply_sideband_request(caller, SidebandRequest::Room { request_id: request_id.map(Into::into), action })
}

fn room_done(response: SidebandResponse) -> shared_types::RoomActionResult {
    assert!(response.ok, "{}", response.message);
    match response.payload {
        Some(SidebandResponsePayload::Room { result }) => result,
        other => panic!("expected a room result, got {other:?}"),
    }
}

/// Every stored message with its sender as the feed shows it.
fn feed_lines(supervisor: &SupervisorHandle, room_id: RoomId) -> Vec<(String, String)> {
    supervisor.read_room_feed(ReadRoomFeedRequest { room_id, cursor: None }).unwrap().events.into_iter()
        .filter_map(|event| match event.item {
            RoomFeedItem::Message { sender, content, .. } => Some((match sender {
                RoomMessageSender::Operator {} => "operator".to_string(),
                RoomMessageSender::Session { session_id } => format!("session {session_id}"),
                RoomMessageSender::Guest { label, .. } => format!("guest {label}"),
                RoomMessageSender::LocalProcess { label } => format!("local {label}"),
                RoomMessageSender::Prim1 {} => "prim1".to_string(),
            }, content)),
            _ => None,
        }).collect()
}

fn operator_exchange(supervisor: &SupervisorHandle, attribution: impl Into<pane_ancestry::Attributed> + Send + 'static,
    actor: Option<&str>, request: &SidebandRequest)
    -> SidebandResponse {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_io().enable_time().build().unwrap();
    let preamble = control_plane::encode_operator_preamble(&shared_types::OperatorPreamble {
        operator: shared_types::OperatorIntro { actor: actor.map(Into::into) } }).unwrap();
    let line = control_plane::encode_request(request).unwrap();
    runtime.block_on(async {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let handle = supervisor.clone();
        let task = tokio::spawn(async move { handle_sideband_stream(handle, attribution, server).await });
        client.write_all(format!("{preamble}\n{line}\n").as_bytes()).await.unwrap();
        client.flush().await.unwrap();
        let mut response = String::new();
        tokio::time::timeout(Duration::from_secs(10), BufReader::new(client).read_line(&mut response)).await.unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(10), task).await.unwrap().unwrap().unwrap();
        control_plane::decode_response(response.trim()).unwrap()
    })
}

#[test]
fn a16_a_managing_member_creates_reads_changes_and_deletes_rooms_with_notices_and_replays() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let caller = mobility_pane(&supervisor, "claude", 9701);
    let home = mobility_room(&supervisor, "Home", vec![claude]);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();

    let id = Uuid::new_v4().to_string();
    let create = shared_types::RoomAction::Create { label: "PC3 Demo".into(), brief_on_join: Some(false),
        brief_template: None, resume_on_restart: Some(true) };
    let created = room_done(room_request(&supervisor, &caller, Some(&id), create.clone()));
    let new_room = created.room.room_id;
    assert_eq!((created.room.label.as_str(), created.room.resume_on_restart, created.room.member_ids.len()),
        ("PC3 Demo", true, 0));
    assert!(room_done(room_request(&supervisor, &caller, Some(&id), create)).replayed);
    let label = |content: &str| (format!("session {claude}"), content.to_string());
    assert_eq!(feed_lines(&supervisor, new_room), [label("claude created this room")]);
    assert!(feed_lines(&supervisor, home.room_id).contains(&label("claude created room \"PC3 Demo\"")));

    // Its own room: manage suffices. The new room is another room: roam.
    let get = room_done(room_request(&supervisor, &caller, None, shared_types::RoomAction::Get { room_id: None }));
    assert_eq!(get.room.room_id, home.room_id);
    assert!(get.default_brief.is_some() && get.brief.is_some() && get.position.is_some());
    let refused = room_request(&supervisor, &caller, Some(&Uuid::new_v4().to_string()),
        shared_types::RoomAction::Update { room_id: Some(new_room), label: Some("x".into()), resume_on_restart: None,
            brief_on_join: None, brief_template: None, position: None });
    assert!(!refused.ok && refused.message.contains("Can roam"), "{}", refused.message);
    member_refused(room_request(&supervisor, &caller, Some("not-a-uuid"),
        shared_types::RoomAction::Delete { room_id: home.room_id }).into_member_shape(), "needs request_id, a UUID");
    let read_with_id = room_request(&supervisor, &caller, Some(&Uuid::new_v4().to_string()),
        shared_types::RoomAction::Get { room_id: None });
    assert!(!read_with_id.ok && read_with_id.message.contains("is a read"), "{}", read_with_id.message);

    supervisor.set_member_permissions(claude, manage_grant(true)).unwrap();
    let updated = room_done(room_request(&supervisor, &caller, Some(&Uuid::new_v4().to_string()),
        shared_types::RoomAction::Update { room_id: Some(new_room), label: Some("PC3 Blender".into()),
            resume_on_restart: Some(false), brief_on_join: Some(true), brief_template: Some("Hi {your_label}".into()),
            position: Some(0) }));
    assert_eq!((updated.room.label.as_str(), updated.room.resume_on_restart, updated.position), ("PC3 Blender", false, Some(0)));
    assert_eq!(updated.brief.as_ref().unwrap().brief_template.as_deref(), Some("Hi {your_label}"));
    assert_eq!(feed_lines(&supervisor, new_room).last().unwrap(), &label(
        "claude updated the room: renamed to \"PC3 Blender\"; Resume on restart off; brief at launch on; brief changed; moved to position 0"));
    let restored = room_done(room_request(&supervisor, &caller, Some(&Uuid::new_v4().to_string()),
        shared_types::RoomAction::Update { room_id: Some(new_room), label: None, resume_on_restart: None,
            brief_on_join: None, brief_template: Some(String::new()), position: None }));
    assert_eq!(restored.brief.unwrap().brief_template, None, "\"\" restores the built-in brief");

    // Delete: its sessions go to the lobby; the caller's own room records it.
    supervisor.add_room_member(AddRoomMemberRequest { room_id: new_room, session_id: codex }).unwrap();
    room_done(room_request(&supervisor, &caller, Some(&Uuid::new_v4().to_string()),
        shared_types::RoomAction::Delete { room_id: new_room }));
    assert!(supervisor.snapshot().rooms.iter().all(|room| room.room_id != new_room));
    assert_eq!(room_of(&supervisor, codex), None);
    assert!(feed_lines(&supervisor, home.room_id).contains(&label(
        "claude deleted room \"PC3 Blender\"; its members are in the lobby")));
}

/// Lets a room refusal reuse the member helper's shape check.
trait IntoMemberShape { fn into_member_shape(self) -> SidebandResponse; }
impl IntoMemberShape for SidebandResponse { fn into_member_shape(self) -> SidebandResponse { self } }

#[test]
fn a16_room_start_and_stop_act_on_every_other_member_and_never_the_caller() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let first = create_test_session(&supervisor, "first", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let second = create_test_session(&supervisor, "second", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9711);
    let room = mobility_room(&supervisor, "Team", vec![claude, first, second]);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    queue_mock_starts(&supervisor, 2);

    let started = room_done(room_request(&supervisor, &caller, Some(&Uuid::new_v4().to_string()),
        shared_types::RoomAction::Start { room_id: None }));
    assert_eq!(started.members.iter().map(|member| member.session_id).collect::<Vec<_>>(), [first, second]);
    assert!(session_now(&supervisor, first).running && session_now(&supervisor, second).running);
    let again = room_done(room_request(&supervisor, &caller, Some(&Uuid::new_v4().to_string()),
        shared_types::RoomAction::Start { room_id: None }));
    assert!(again.members.is_empty() && again.warnings.iter().any(|warning| warning.contains("nothing changed")));
    let stopped = room_done(room_request(&supervisor, &caller, Some(&Uuid::new_v4().to_string()),
        shared_types::RoomAction::Stop { room_id: None }));
    assert_eq!(stopped.members.len(), 2);
    assert!(session_now(&supervisor, claude).running, "the caller is never stopped by its own room stop");
    assert!(!session_now(&supervisor, first).running && !session_now(&supervisor, second).running);
    let label = |content: &str| (format!("session {claude}"), content.to_string());
    assert_eq!(feed_lines(&supervisor, room.room_id), [label("claude started the room: first, second"),
        label("claude stopped the room: first, second")]);
}

#[test]
fn a16_guest_tokens_go_to_the_inviting_member_only_and_a_replay_never_repeats_them() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let caller = mobility_pane(&supervisor, "claude", 9721);
    let room = mobility_room(&supervisor, "Team", vec![claude]);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();

    let id = Uuid::new_v4().to_string();
    let invite = shared_types::RoomAction::GuestInvite { room_id: None, label: Some("Scout".into()) };
    let invited = room_done(room_request(&supervisor, &caller, Some(&id), invite.clone()));
    let invitation = invited.invitation.clone().expect("the inviting member sees the token once");
    assert!(invitation.token.starts_with("prim1g_") && invitation.mcp_config_json.contains("--prim1-guest-mcp"));
    let replay = room_done(room_request(&supervisor, &caller, Some(&id), invite));
    assert!(replay.replayed && replay.invitation.is_none(), "a replay returns the guest without its token");
    let feed = feed_lines(&supervisor, room.room_id);
    assert_eq!(feed, [(format!("session {claude}"), "claude invited outside agent Scout".to_string())]);
    assert!(!feed.iter().any(|(_, content)| content.contains(&invitation.token)), "never posted");
    // The guest is real: its token works, and it is no member.
    let runtime = tokio::runtime::Builder::new_current_thread().enable_io().enable_time().build().unwrap();
    assert!(runtime.block_on(supervisor.apply_guest_request_async(&invitation.token, &[], GuestRequest::Rooms {})).ok);
    assert_eq!(room_now(&supervisor, room.room_id).member_ids, [claude]);

    let listed = room_done(room_request(&supervisor, &caller, None, shared_types::RoomAction::GuestList { room_id: None }));
    assert_eq!(listed.guests.len(), 1);
    room_done(room_request(&supervisor, &caller, Some(&Uuid::new_v4().to_string()),
        shared_types::RoomAction::GuestRevoke { guest_id: invitation.guest_id }));
    assert!(!runtime.block_on(supervisor.apply_guest_request_async(&invitation.token, &[], GuestRequest::Rooms {})).ok);
    assert!(feed_lines(&supervisor, room.room_id).last().unwrap().1.contains("revoked outside agent Scout"));
}

#[test]
fn a16_the_operator_entry_admits_only_a_process_outside_every_pane_and_names_it_truthfully() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let caller = mobility_pane(&supervisor, "claude", 9731);
    let create = SidebandRequest::Room { request_id: Some(Uuid::new_v4().to_string()),
        action: shared_types::RoomAction::Create { label: "Laptop YouTube".into(), brief_on_join: None,
            brief_template: None, resume_on_restart: Some(true) } };
    let rooms_before = supervisor.snapshot().rooms.len();

    // A pane keeps to its grants, even holding all of them.
    supervisor.set_member_permissions(claude, MemberPermissions { roam: true, maintain: true, manage: true }).unwrap();
    let pane = operator_exchange(&supervisor, JobAttribution::Pane(caller.clone()), Some("me"), &create);
    assert!(!pane.ok && pane.message.contains("cannot use the operator entry"), "{pane:?}");
    // Not proven outside: refused.
    let unknown = operator_exchange(&supervisor, JobAttribution::Unknown, None, &create);
    assert!(!unknown.ok && unknown.message.contains("could not confirm"), "{unknown:?}");
    assert_eq!(supervisor.snapshot().rooms.len(), rooms_before, "nothing was created");

    let created = operator_exchange(&supervisor, JobAttribution::Outside(4242), Some("MAINPC supervisor over SSH"), &create);
    assert!(created.ok, "{created:?}");
    let Some(SidebandResponsePayload::Room { result }) = created.payload else { panic!("{created:?}") };
    assert!(result.room.resume_on_restart);
    let entry = "local MAINPC supervisor over SSH (local process 4242)".to_string();
    assert_eq!(feed_lines(&supervisor, result.room.room_id), [(entry.clone(),
        "MAINPC supervisor over SSH (local process 4242) created this room".to_string())]);
    // A supervisor member with all three grants, started, through the entry.
    queue_mock_starts(&supervisor, 1);
    let lead = SidebandRequest::Member { request_id: Uuid::new_v4().to_string(), action: shared_types::MemberAction::Create { codex_transport: Default::default(),
        driver: DriverKind::Claude, label: "YouTube Lead".into(), permission_profile: shared_types::PermissionProfile::Unsafe,
        brief: false, brief_text: None, start: true, room_id: Some(result.room.room_id),
        grants: Some(MemberPermissions { roam: true, maintain: true, manage: true }) } };
    let added = operator_exchange(&supervisor, JobAttribution::Outside(4242), Some("MAINPC supervisor over SSH"), &lead);
    let Some(SidebandResponsePayload::Member { result: member }) = added.payload else { panic!("{added:?}") };
    assert!(member.member.running && member.member.permissions.manage, "{member:?}");
    assert_eq!(feed_lines(&supervisor, result.room.room_id).last().unwrap(), &(entry,
        "MAINPC supervisor over SSH (local process 4242) added YouTube Lead (Claude, bypass permissions, grants roam, maintain, manage)".to_string()));
    // Lists work; pane-only requests do not.
    assert!(operator_exchange(&supervisor, JobAttribution::Outside(4242), None, &SidebandRequest::MembersList {}).ok);
    assert!(operator_exchange(&supervisor, JobAttribution::Outside(4242), None, &SidebandRequest::RoomsList {}).ok);
    for request in [SidebandRequest::RoomPost { request_id: None, content: "x".into() },
        SidebandRequest::Tasks { query: "{ tasks { total } }".into(), variables: None, operation_name: None },
        SidebandRequest::MemberPermissions {}] {
        let refused = operator_exchange(&supervisor, JobAttribution::Outside(4242), None, &request);
        assert!(!refused.ok && refused.message.contains("operator entry takes"), "{refused:?}");
    }
}

#[test]
fn a16_operator_entry_labels_are_bounded_single_lines() {
    assert_eq!(member_lifecycle::LocalOperator::new(None, 7).label, "local process 7");
    assert_eq!(member_lifecycle::LocalOperator::new(Some("  "), 7).label, "local process 7");
    assert_eq!(member_lifecycle::LocalOperator::new(Some("a\nb\u{1b}[31m"), 7).label, "ab[31m (local process 7)");
    assert_eq!(member_lifecycle::LocalOperator::new(Some(&"x".repeat(200)), 7).label, format!("{} (local process 7)", "x".repeat(80)));
}

#[test]
fn a16_reviewer_repro_a_partial_effect_is_the_requests_outcome() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let helper = create_test_session(&supervisor, "helper", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9741);
    mobility_room(&supervisor, "Team", vec![claude, helper]);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    queue_mock_starts(&supervisor, 2);
    member_done(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), start_action("helper")));
    // The rename applies; the profile of a running member is refused.
    let id = Uuid::new_v4().to_string();
    let edit = update_action("helper", Some("Helper Two"), Some(shared_types::PermissionProfile::Unsafe), None);
    let partial = member_request(&supervisor, &caller, &id, edit.clone());
    assert!(!partial.ok && partial.message.contains("partly updated"), "{}", partial.message);
    assert_eq!(session_now(&supervisor, helper).label, "Helper Two");
    // Other arguments under that request_id are refused.
    member_refused(member_request(&supervisor, &caller, &id, update_action("Helper Two", Some("Other"), None, None)),
        "was already used for a different request");
    // Once the member is stopped, the same arguments replay the first
    // outcome and apply nothing more (Reviewer's repro).
    member_done(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), stop_action("Helper Two")));
    let retry = member_request(&supervisor, &caller, &id, edit);
    assert!(!retry.ok && retry.message.contains("replayed"), "{}", retry.message);
    let after = session_now(&supervisor, helper);
    assert_eq!((after.label.as_str(), after.permission_profile), ("Helper Two", shared_types::PermissionProfile::Normal));
    // A refusal before any effect leaves nothing behind: its id is free again.
    let refused_id = Uuid::new_v4().to_string();
    member_refused(member_request(&supervisor, &caller, &refused_id, stop_action("nobody")), "no member");
    member_done(member_request(&supervisor, &caller, &refused_id, start_action("Helper Two")));
}

#[test]
fn a16_reviewer_repro_cross_room_delete_rechecks_roam_after_removal() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let caller = mobility_pane(&supervisor, "claude", 9751);
    mobility_room(&supervisor, "Home", vec![claude]);
    let other = mobility_room(&supervisor, "Other", vec![codex]);
    supervisor.set_member_permissions(claude, manage_grant(true)).unwrap();
    // Roam is withdrawn after the admission, before the final delete.
    let withdraw = supervisor.clone();
    *supervisor.inner.member_before_effect.lock() = Some(Box::new(move || {
        withdraw.set_member_permissions(claude, manage_grant(false)).unwrap();
    }));
    member_refused(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), delete_action(&codex.to_string())),
        "Can roam");
    assert!(supervisor.snapshot().sessions.iter().any(|session| session.session_id == codex), "not deleted");
    assert_eq!(room_of(&supervisor, codex), Some(other.room_id), "not removed either");
}

#[test]
fn a16_reviewer_repro_invalid_fresh_choices_stop_nothing() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let helper = create_test_session(&supervisor, "helper", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9761);
    let room = mobility_room(&supervisor, "Team", vec![claude, helper]);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    queue_mock_starts(&supervisor, 1);
    member_done(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), start_action("helper")));
    let bad = shared_types::MemberAction::Start { member: "helper".into(), mode: shared_types::StartMode::Fresh,
        launch_choices: Some(SessionLaunchChoices { model: Some("bad\nmodel".into()), ..Default::default() }), search: false };
    let refused = member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), bad);
    assert!(!refused.ok, "{refused:?}");
    assert!(session_now(&supervisor, helper).running, "a refused fresh start stops nothing");
    assert_eq!(feed_lines(&supervisor, room.room_id).len(), 1, "only the first start's notice");
    // Naming yourself for a plain start is a no-op, not a refusal.
    let me = member_done(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), start_action("claude")));
    assert!(me.warnings.iter().any(|warning| warning.contains("you are running")), "{me:?}");
}

#[test]
fn a16_a_client_in_another_session_is_outside_every_pane_even_when_its_job_cannot_be_read() {
    // PC3, 20 Oct 11:26Z: the elevated SSH caller could not be opened, so
    // its Job query failed (not OutsideEveryPane).
    let unreadable = anyhow!("failed to open sideband client process: access denied");
    assert!(matches!(attribution_without_job(&unreadable, Some((5120, 0)), Some(1)), JobAttribution::Outside(5120)));
    // Same session as PRIM-1 (its panes'): unproven, as before.
    assert!(matches!(attribution_without_job(&unreadable, Some((5120, 1)), Some(1)), JobAttribution::Unknown));
    // Nothing read about the client or the server: unproven.
    assert!(matches!(attribution_without_job(&unreadable, None, Some(1)), JobAttribution::Unknown));
    assert!(matches!(attribution_without_job(&unreadable, Some((5120, 0)), None), JobAttribution::Unknown));
    // A proven outsider stays one.
    let outside: anyhow::Error = OutsideEveryPane(77).into();
    assert!(matches!(attribution_without_job(&outside, None, None), JobAttribution::Outside(77)));
}

fn runtime_request(supervisor: &SupervisorHandle, caller: &PaneCaller, request_id: Option<&str>, action: shared_types::RuntimeAction)
    -> SidebandResponse {
    supervisor.apply_sideband_request(caller, SidebandRequest::Runtime { request_id: request_id.map(Into::into), action })
}

#[test]
fn a16_runtime_settings_need_maintain_and_change_through_the_operators_setters() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let caller = mobility_pane(&supervisor, "claude", 9771);
    let room = mobility_room(&supervisor, "Team", vec![claude]);
    let folder = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let set = shared_types::RuntimeAction::SettingsSet { task_folder: Some(folder.path().display().to_string()),
        personal_repo: None, workspace: Some(workspace.path().display().to_string()) };

    // Manage is not maintain.
    supervisor.set_member_permissions(claude, manage_grant(true)).unwrap();
    let refused = runtime_request(&supervisor, &caller, Some(&Uuid::new_v4().to_string()), set.clone());
    assert!(!refused.ok && refused.message.contains("Can upgrade PRIM-1"), "{}", refused.message);
    let refused_get = runtime_request(&supervisor, &caller, None, shared_types::RuntimeAction::SettingsGet {});
    assert!(!refused_get.ok, "{refused_get:?}");

    supervisor.set_member_permissions(claude, MemberPermissions { roam: false, maintain: true, manage: false }).unwrap();
    let response = runtime_request(&supervisor, &caller, Some(&Uuid::new_v4().to_string()), set);
    assert!(response.ok, "{}", response.message);
    let status = supervisor.task_folder_status();
    assert_eq!(serde_json::to_value(&status).unwrap()["source"], "setting");
    assert!(supervisor.snapshot().workspace_preference.len() > 0);
    let notice = feed_lines(&supervisor, room.room_id).last().unwrap().1.clone();
    assert!(notice.starts_with("claude changed the runtime settings: task folder ") && notice.contains("default workspace"), "{notice}");
    let get = runtime_request(&supervisor, &caller, None, shared_types::RuntimeAction::SettingsGet {});
    let Some(SidebandResponsePayload::Runtime { result }) = get.payload else { panic!("{get:?}") };
    let settings = result.settings.unwrap();
    assert!(settings.get("task_folder").is_some() && settings.get("personal_repo").is_some() && settings.get("workspace").is_some());
    // A read with a request_id, and a change without one, are refused.
    assert!(!runtime_request(&supervisor, &caller, Some(&Uuid::new_v4().to_string()), shared_types::RuntimeAction::SettingsGet {}).ok);
    assert!(!runtime_request(&supervisor, &caller, None, shared_types::RuntimeAction::SettingsSet { task_folder: None,
        personal_repo: None, workspace: Some(workspace.path().display().to_string()) }).ok);
}

#[test]
fn a16_local_conversations_need_manage_and_attach_keeps_to_the_callers_scope() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let lobby = create_test_session(&supervisor, "Loner", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9781);
    mobility_room(&supervisor, "Home", vec![claude]);
    mobility_room(&supervisor, "Other", vec![codex]);
    let attach = |member: String| shared_types::RuntimeAction::ConversationAttach { member,
        conversation_id: Uuid::new_v4().to_string() };
    supervisor.set_member_permissions(claude, MemberPermissions { roam: true, maintain: true, manage: false }).unwrap();
    let scan = runtime_request(&supervisor, &caller, None, shared_types::RuntimeAction::ConversationsScan {});
    assert!(!scan.ok && scan.message.contains("Can manage agents"), "{}", scan.message);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    // Another room's member and a lobby session need roam; refused before any scan.
    for member in [codex.to_string(), lobby.to_string()] {
        let refused = runtime_request(&supervisor, &caller, Some(&Uuid::new_v4().to_string()), attach(member.clone()));
        assert!(!refused.ok && refused.message.contains("Can roam"), "{member}: {}", refused.message);
    }
    // Never the caller itself.
    let me = runtime_request(&supervisor, &caller, Some(&Uuid::new_v4().to_string()), attach("claude".into()));
    assert!(!me.ok && me.message.contains("itself"), "{}", me.message);
}

#[test]
fn a16_reviewer_source_room_stop_reports_an_unproved_stop_truthfully() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let helper = create_test_session(&supervisor, "helper", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9791);
    let room = mobility_room(&supervisor, "Team", vec![claude, helper]);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    let stubborn = mock_pty_session(Some(std::process::id()), MockKillBehavior::Error("access denied")).0;
    supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![stubborn])));
    member_done(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), start_action("helper")));
    let stopped = room_done(room_request(&supervisor, &caller, Some(&Uuid::new_v4().to_string()),
        shared_types::RoomAction::Stop { room_id: None }));
    assert!(stopped.members.iter().all(|member| member.session_id != helper), "{stopped:?}");
    assert!(stopped.warnings.iter().any(|warning| warning.starts_with("helper:")), "{stopped:?}");
    assert!(!feed_lines(&supervisor, room.room_id).iter().any(|(_, content)| content.contains("stopped the room")),
        "no member was proved stopped, so no notice says so");
}

#[test]
fn a16_reviewer_source_a_released_hold_before_a_failed_launch_is_a_partial_effect() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let helper = create_test_session(&supervisor, "helper", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9792);
    let room = mobility_room(&supervisor, "Team", vec![claude, helper]);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    let conversation = Uuid::new_v4().to_string();
    supervisor.inner.slots.lock().get_by_id_mut(helper).unwrap().recovery = Some(session::RecoveryReference {
        original_id: Some(conversation), provenance: "room_import_in_use".into(),
        reason: "Copied from the live PRIM-1.".into(), reported_codex: None });
    // No PTY can be spawned: the launch after the release fails.
    supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(Vec::new())));
    let id = Uuid::new_v4().to_string();
    let resume_here = start_mode_action("helper", shared_types::StartMode::ResumeHere);
    let partial = member_request(&supervisor, &caller, &id, resume_here.clone());
    assert!(!partial.ok && partial.message.contains("was released but it could not start"), "{}", partial.message);
    assert_eq!(session_now(&supervisor, helper).recovery, None, "the hold was released");
    assert!(feed_lines(&supervisor, room.room_id).iter()
        .any(|(_, content)| content == "claude released the copied conversation of helper for Resume here"));
    let retry = member_request(&supervisor, &caller, &id, resume_here);
    assert!(!retry.ok && retry.message.contains("replayed"), "{}", retry.message);
}

#[test]
fn a16_reviewer_source_a_stop_before_a_failed_removal_is_recorded() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let helper = create_test_session(&supervisor, "helper", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9793);
    let room = mobility_room(&supervisor, "Team", vec![claude, helper]);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    queue_mock_starts(&supervisor, 1);
    member_done(member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), start_action("helper")));
    supervisor.fail_next_room_catalog_write_for_tests();
    let partial = member_request(&supervisor, &caller, &Uuid::new_v4().to_string(), delete_action("helper"));
    assert!(!partial.ok && partial.message.contains("was stopped but could not leave its room"), "{}", partial.message);
    assert!(!session_now(&supervisor, helper).running);
    assert_eq!(room_of(&supervisor, helper), Some(room.room_id), "still a member");
    assert_eq!(feed_lines(&supervisor, room.room_id).last().unwrap().1, "claude stopped helper");
}

#[test]
fn a16_reviewer_source_the_final_delete_rechecks_the_callers_room_under_its_locks() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let helper = create_test_session(&supervisor, "helper", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9794);
    let home = mobility_room(&supervisor, "Home", vec![claude, helper]);
    let away = mobility_room(&supervisor, "Away", Vec::new());
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    let check = member_lifecycle::DeleteCheck { caller: member_lifecycle::Actor::Pane(&caller), original_room: Some(home.room_id) };
    let in_scope = { let rooms = supervisor.inner.rooms.lock(); check.check(&rooms, &supervisor.inner.slots.lock(), helper) };
    assert!(in_scope.is_ok(), "{in_scope:?}");
    // The caller moved during the drain wait and has no roam: refused.
    supervisor.remove_room_member(RemoveRoomMemberRequest { room_id: home.room_id, session_id: claude }).unwrap();
    supervisor.add_room_member(AddRoomMemberRequest { room_id: away.room_id, session_id: claude }).unwrap();
    let moved = { let rooms = supervisor.inner.rooms.lock(); check.check(&rooms, &supervisor.inner.slots.lock(), helper) };
    assert!(moved.is_err(), "a caller that left the original room needs roam");
    supervisor.set_member_permissions(claude, manage_grant(true)).unwrap();
    let roaming = { let rooms = supervisor.inner.rooms.lock(); check.check(&rooms, &supervisor.inner.slots.lock(), helper) };
    assert!(roaming.is_ok(), "{roaming:?}");
}

#[test]
fn a16_reviewer_source_a_settings_no_op_posts_nothing() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let caller = mobility_pane(&supervisor, "claude", 9795);
    let room = mobility_room(&supervisor, "Team", vec![claude]);
    supervisor.set_member_permissions(claude, MemberPermissions { roam: false, maintain: true, manage: false }).unwrap();
    let folder = tempfile::tempdir().unwrap();
    let set = || shared_types::RuntimeAction::SettingsSet { task_folder: Some(folder.path().display().to_string()),
        personal_repo: None, workspace: None };
    assert!(runtime_request(&supervisor, &caller, Some(&Uuid::new_v4().to_string()), set()).ok);
    let notices = feed_lines(&supervisor, room.room_id).len();
    // The same folder again, under a fresh request_id: in effect already.
    let again = runtime_request(&supervisor, &caller, Some(&Uuid::new_v4().to_string()), set());
    let Some(SidebandResponsePayload::Runtime { result }) = again.payload else { panic!("{again:?}") };
    assert!(result.warnings.iter().any(|warning| warning.contains("already in effect")), "{result:?}");
    assert_eq!(feed_lines(&supervisor, room.room_id).len(), notices, "a no-op posts no notice");
}

#[test]
fn a16_room_packages_save_inspect_and_load_on_the_operators_paths_with_the_grant_at_each_effect() {
    use shared_types::RuntimeAction;
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let helper = create_test_session(&supervisor, "helper", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9791);
    let home = mobility_room(&supervisor, "Home", vec![claude, helper]).room_id;
    let other = mobility_room(&supervisor, "Other", vec![codex]).room_id;
    supervisor.post_room_message_as(home, RoomMessageSender::Operator {}, "the plan".into(), None).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Home.prim1room");
    let path = file.display().to_string();
    let fresh = || Uuid::new_v4().to_string();
    let save = |room_id: Option<RoomId>, path: &str| RuntimeAction::RoomSave { room_id, path: path.into() };
    let load = || RuntimeAction::RoomLoad { path: path.clone(), source_room_id: None };
    let me = format!("session {claude}");
    let revoke_on_admission = |supervisor: &SupervisorHandle| {
        let revoker = supervisor.clone();
        *supervisor.inner.sideband_after_initial_authorization.lock() = Some(Box::new(move || {
            revoker.set_member_permissions(claude, MemberPermissions::default()).unwrap();
        }));
    };
    let rooms = |supervisor: &SupervisorHandle| supervisor.inner.rooms.lock().order.len();

    // Manage, not maintain; another room also needs roam.
    supervisor.set_member_permissions(claude, MemberPermissions { roam: true, maintain: true, manage: false }).unwrap();
    let refused = runtime_request(&supervisor, &caller, Some(&fresh()), save(None, &path));
    assert!(!refused.ok && refused.message.contains("Can manage agents"), "{}", refused.message);
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    let elsewhere = dir.path().join("Other.prim1room").display().to_string();
    let refused = runtime_request(&supervisor, &caller, Some(&fresh()), save(Some(other), &elsewhere));
    assert!(!refused.ok && refused.message.contains("Can roam"), "{}", refused.message);

    // Manage withdrawn after admission, before the feed is copied: no file.
    revoke_on_admission(&supervisor);
    let refused = runtime_request(&supervisor, &caller, Some(&fresh()), save(None, &path));
    assert!(!refused.ok && refused.message.contains("Can manage agents"), "{}", refused.message);
    assert!(!file.exists(), "nothing is written once the grant is gone");
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();

    // Save: the operator's file, one notice in the saved room, a replay writes nothing again.
    let id = fresh();
    let saved = runtime_request(&supervisor, &caller, Some(&id), save(None, &path));
    assert!(saved.ok, "{}", saved.message);
    let Some(SidebandResponsePayload::Runtime { result }) = saved.payload else { panic!("{}", saved.message) };
    let report = result.room_saved.unwrap();
    assert_eq!((report.path.as_str(), report.label.as_str(), report.members), (path.as_str(), "Home", 2));
    assert!(file.exists());
    let saved_notice = |supervisor: &SupervisorHandle| feed_lines(supervisor, home).into_iter()
        .filter(|(sender, text)| *sender == me && text.starts_with(&format!("claude saved this room to {path} ("))).count();
    assert_eq!(saved_notice(&supervisor), 1);
    let replay = runtime_request(&supervisor, &caller, Some(&id), save(None, &path));
    let Some(SidebandResponsePayload::Runtime { result }) = replay.payload else { panic!("{}", replay.message) };
    assert!(result.replayed);
    assert_eq!(saved_notice(&supervisor), 1);
    // An agent never replaces a room file, also one created while the room is written.
    let again = runtime_request(&supervisor, &caller, Some(&fresh()), save(None, &path));
    assert!(!again.ok && again.message.contains("already exists"), "{}", again.message);
    let raced = dir.path().join("Raced.prim1room");
    let sentinel = raced.clone();
    *supervisor.inner.sideband_after_initial_authorization.lock() = Some(Box::new(move || {
        fs::write(&sentinel, b"someone else's room").unwrap();
    }));
    let race = runtime_request(&supervisor, &caller, Some(&fresh()), save(None, &raced.display().to_string()));
    assert!(!race.ok && race.message.contains("already exists"), "{}", race.message);
    assert_eq!(fs::read(&raced).unwrap(), b"someone else's room");
    assert_eq!(saved_notice(&supervisor), 1, "no notice for a save that did not happen");
    assert!(feed_lines(&supervisor, home).iter().all(|(_, text)| !text.contains("Raced.prim1room")));
    let mut names = fs::read_dir(dir.path()).unwrap().map(|entry| entry.unwrap().file_name().into_string().unwrap()).collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["Home.prim1room", "Raced.prim1room"], "no temporary file is left");

    // Inspect reads only, without a request_id.
    let inspected = runtime_request(&supervisor, &caller, None, RuntimeAction::RoomInspect { path: path.clone() });
    let Some(SidebandResponsePayload::Runtime { result }) = inspected.payload else { panic!("{}", inspected.message) };
    let sources = result.room_sources.unwrap();
    assert_eq!((sources.len(), sources[0].label.as_str(), sources[0].members), (1, "Home", 2));
    assert!(!runtime_request(&supervisor, &caller, Some(&fresh()), RuntimeAction::RoomInspect { path: path.clone() }).ok);

    // Load with manage withdrawn after admission: no room, no member copy.
    let (room_count, sessions) = (rooms(&supervisor), supervisor.inner.slots.lock().ordered_slots().count());
    revoke_on_admission(&supervisor);
    let refused = runtime_request(&supervisor, &caller, Some(&fresh()), load());
    assert!(!refused.ok && refused.message.contains("Can manage agents"), "{}", refused.message);
    assert_eq!((rooms(&supervisor), supervisor.inner.slots.lock().ordered_slots().count()), (room_count, sessions));
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();

    // Load: a new room with new members, nothing started and no grants; both rooms record it.
    let id = fresh();
    let loaded = runtime_request(&supervisor, &caller, Some(&id), load());
    assert!(loaded.ok, "{}", loaded.message);
    let Some(SidebandResponsePayload::Runtime { result }) = loaded.payload else { panic!("{}", loaded.message) };
    let report = result.room_loaded.unwrap();
    assert_eq!(report.room.label, "Home (loaded)");
    assert_eq!(report.members.len(), 2);
    {
        let slots = supervisor.inner.slots.lock();
        for member in &report.members {
            assert!(member.session_id != claude && member.session_id != helper, "new identities");
            let slot = slots.get_by_id(member.session_id).unwrap();
            assert!(slot.running.is_none(), "nothing starts");
            assert_eq!(slot.permissions, MemberPermissions::default(), "no grants travel or are given");
        }
    }
    assert!(feed_lines(&supervisor, report.room.room_id).contains(&(me.clone(),
        format!("claude loaded this room from {path} (2 members, none started)"))));
    assert!(feed_lines(&supervisor, home).contains(&(me.clone(), format!("claude loaded room \"Home (loaded)\" from {path}"))));
    let replay = runtime_request(&supervisor, &caller, Some(&id), load());
    let Some(SidebandResponsePayload::Runtime { result }) = replay.payload else { panic!("{}", replay.message) };
    assert!(result.replayed);
    assert_eq!(rooms(&supervisor), room_count + 1, "a replay loads nothing again");
}

#[test]
fn a16_a_partial_room_load_is_recorded_once_in_the_callers_room_and_never_loaded_again() {
    // On the test thread, where the room_package seams apply.
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let helper = create_test_session(&supervisor, "helper", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9795);
    let home = mobility_room(&supervisor, "Home", vec![claude, helper]).room_id;
    supervisor.set_member_permissions(claude, manage_grant(false)).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Home.prim1room");
    supervisor.export_room(home, &file).unwrap();
    let path = file.display().to_string();
    let load = || shared_types::RuntimeAction::RoomLoad { path: path.clone(), source_room_id: None };
    let sessions = || supervisor.inner.slots.lock().ordered_slots().count();
    let partial = || feed_lines(&supervisor, home).into_iter().filter(|(sender, text)| *sender == format!("session {claude}")
        && text.starts_with(&format!("claude loaded {path} only in part: "))).count();
    let before = sessions();

    // The room catalog fails and so does undoing the held member copies.
    supervisor.fail_next_room_catalog_write_for_tests();
    let failing = supervisor.clone();
    room_package::SEAMS.with(|seams| seams.borrow_mut().insert("held_saved",
        Box::new(move || { failing.fail_next_catalog_write_for_tests(); Ok(()) })));
    let id = Uuid::new_v4().to_string();
    let error = supervisor.runtime_action(member_lifecycle::Actor::Pane(&caller), Some(id.clone()), load()).unwrap_err();
    assert!(error.is::<member_lifecycle::PartialEffect>() && format!("{error:#}").contains("stay in the lobby, held"), "{error:#}");
    assert_eq!(sessions(), before + 2, "the held copies remain");
    assert_eq!(partial(), 1);
    // The same request_id replays the outcome: nothing loads again, no second notice.
    let again = supervisor.runtime_action(member_lifecycle::Actor::Pane(&caller), Some(id), load()).unwrap_err();
    let replayed = format!("{again:#}");
    assert!(replayed.starts_with(&format!("{error:#}")) && replayed.contains("replayed"), "{replayed}");
    assert_eq!(sessions(), before + 2);
    assert_eq!(partial(), 1);
}

#[test]
fn a16_the_operator_entry_reads_and_sets_a_rooms_fallbacks_under_its_own_name() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let room = mobility_room(&supervisor, "PC3 Demo", vec![claude]).room_id;
    let outside = || JobAttribution::Outside(4242);
    let ask = |request: SidebandRequest| operator_exchange(&supervisor, outside(), Some("MAINPC supervisor"), &request);
    let get = |room_id: Option<RoomId>| SidebandRequest::RoomFallbacks { action: shared_types::RoomFallbackAction::Get,
        room_id, revision: None, settings: None };
    let set = |revision: u64, settings: shared_types::RoomFallbackSettings| SidebandRequest::RoomFallbacks {
        action: shared_types::RoomFallbackAction::Set, room_id: Some(room), revision: Some(revision), settings: Some(settings) };
    let snapshot = |response: SidebandResponse| match response.payload {
        Some(SidebandResponsePayload::RoomFallbacks { snapshot }) if response.ok => snapshot,
        _ => panic!("{response:?}"),
    };
    let every_30 = shared_types::RoomFallbackSettings { enabled: true, interval_minutes: 30,
        message: "Continue the existing task.".into(), ..Default::default() };

    // The entry has no room of its own.
    let roomless = ask(get(None));
    assert!(!roomless.ok && roomless.message.contains("--room"), "{roomless:?}");
    let read = snapshot(ask(get(Some(room))));
    assert!(!read.settings.enabled);
    // room_fallbacks' own validation and revision check.
    for (revision, settings, refusal) in [
        (read.revision + 1, every_30.clone(), "fallback settings changed"),
        (read.revision, shared_types::RoomFallbackSettings { interval_minutes: 45, ..every_30.clone() }, "interval"),
        (read.revision, shared_types::RoomFallbackSettings { message: " ".into(), ..every_30.clone() }, "needs a room message"),
    ] {
        let refused = ask(set(revision, settings));
        assert!(!refused.ok && refused.message.contains(refusal), "{refusal}: {refused:?}");
    }
    assert_eq!(supervisor.get_room_fallbacks(room).unwrap().revision, read.revision, "nothing was saved");
    assert!(feed_lines(&supervisor, room).is_empty());

    let saved = snapshot(ask(set(read.revision, every_30.clone())));
    assert_eq!(saved.revision, read.revision + 1);
    assert!(saved.settings.enabled && saved.members.iter().any(|member| member.session_id == claude && member.next_due_at.is_some()));
    assert_eq!(supervisor.get_room_fallbacks(room).unwrap().settings, every_30, "the operator's own read sees it");
    assert_eq!(feed_lines(&supervisor, room), [("local MAINPC supervisor (local process 4242)".to_string(),
        format!("MAINPC supervisor (local process 4242) updated and enabled room fallbacks (settings revision {}).", saved.revision))]);
    // The same request again is stale, not a second save.
    let again = ask(set(read.revision, every_30));
    assert!(!again.ok && again.message.contains("fallback settings changed"), "{again:?}");
    assert_eq!(feed_lines(&supervisor, room).len(), 1);
    // A pane keeps to its own room_fallbacks and grants.
    let caller = mobility_pane(&supervisor, "claude", 9761);
    let pane = operator_exchange(&supervisor, JobAttribution::Pane(caller), None, &get(Some(room)));
    assert!(!pane.ok && pane.message.contains("cannot use the operator entry"), "{pane:?}");
}

/// v3 W5 (CLI parity): the operator entry sends to a named room, recorded as
/// its local process, asking each recipient for an ACK; one member by label
/// or `all`; an unknown member or room is refused with nothing stored.
#[test]
fn v3_the_operator_entry_delivers_to_a_named_room_as_its_local_process() {
    let supervisor = test_supervisor();
    let (pty, _inputs) = recording_pty_session(4601);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude, Some(4601),
        pty, BracketedPasteMode::Unknown);
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let room = mobility_room(&supervisor, "Entry room", vec![claude, codex]).room_id;
    let send = |recipient: &str, content: &str| operator_exchange(&supervisor, JobAttribution::Outside(4242),
        Some("MAINPC supervisor"), &SidebandRequest::RoomDeliverTo { room_id: room, request_id: None,
            recipient: recipient.into(), content: content.into() });

    let one = send("claude", "to one member");
    let Some(SidebandResponsePayload::RoomDelivery { result }) = one.payload else { panic!("{one:?}") };
    assert_eq!(result.receipts.iter().map(|receipt| receipt.recipient_id).collect::<Vec<_>>(), vec![claude]);
    let all = send("all", "to everyone");
    let Some(SidebandResponsePayload::RoomDelivery { result }) = all.payload else { panic!("{all:?}") };
    assert_eq!(result.recipient_count, 2, "all is every member: the entry is no member");
    for refused in [send("nobody", "x"), operator_exchange(&supervisor, JobAttribution::Outside(4242), None,
        &SidebandRequest::RoomDeliverTo { room_id: Uuid::new_v4(), request_id: None, recipient: "all".into(), content: "x".into() })] {
        assert!(!refused.ok, "{refused:?}");
    }
    let entry = "local MAINPC supervisor (local process 4242)".to_string();
    let sent: Vec<_> = feed_lines(&supervisor, room).into_iter().filter(|(sender, _)| *sender == entry).collect();
    assert_eq!(sent, vec![(entry.clone(), "to one member".to_string()), (entry, "to everyone".to_string())]);
    let tracked = room_events(&supervisor, room).iter().filter(|event| matches!(event.item,
        RoomFeedItem::Message { ack_requested: true, .. })).count();
    assert_eq!(tracked, 2, "both ask their recipients for an ACK");
}

/// v3 W3: the operator entry disposes of a message for a recipient that has
/// left the room (the only way that obligation clears), recorded as its local
/// process; the same request replays; an unaddressed name and a pane are refused.
#[test]
fn v3_the_operator_entry_disposes_for_a_recipient_that_left_the_room() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let room = mobility_room(&supervisor, "Dispose room", vec![claude, codex]).room_id;
    let sent = operator_exchange(&supervisor, JobAttribution::Outside(4242), Some("MAINPC supervisor"),
        &SidebandRequest::RoomDeliverTo { room_id: room, request_id: None, recipient: "all".into(), content: "owed".into() });
    let Some(SidebandResponsePayload::RoomDelivery { result }) = sent.payload else { panic!("{sent:?}") };
    supervisor.remove_room_member(RemoveRoomMemberRequest { room_id: room, session_id: codex }).unwrap();
    // Leave, then rejoin: the old message predates the new membership, but the
    // operator still disposes of it (Reviewer 5f475977).
    supervisor.add_room_member(AddRoomMemberRequest { room_id: room, session_id: codex }).unwrap();
    let dispose_as = |pid: u32, recipient: &str, request_id: &str| operator_exchange(&supervisor, JobAttribution::Outside(pid),
        Some("MAINPC supervisor"), &SidebandRequest::RoomDispose { room_id: room, message_id: result.message_id,
            recipient: recipient.into(), request_id: Some(request_id.into()), next_action: None,
            decline: Some("Codex J1 left the room; handled elsewhere".into()) });
    let dispose = |recipient: &str, request_id: &str| dispose_as(4242, recipient, request_id);
    let stored = dispose(&codex.to_string(), "dispose-1");
    assert!(stored.ok, "{stored:?}");
    // A second CLI invocation is another process: the same request replays
    // (Reviewer 460f1b3f), and the stored record keeps the first label.
    let replay = dispose_as(4243, &codex.to_string(), "dispose-1");
    assert!(replay.ok && replay.message.contains("replayed"), "{replay:?}");
    let ack = room_events(&supervisor, room).into_iter().find_map(|event| match event.item {
        RoomFeedItem::Ack { recipient_id, actor, disposition, .. } if recipient_id == codex => Some((actor, disposition)),
        _ => None }).expect("the disposition is in the feed");
    assert_eq!(ack, (shared_types::RoomAckActor::LocalProcess { label: "MAINPC supervisor (local process 4242)".into() },
        shared_types::RoomAckDisposition::Declined));
    assert!(supervisor.inner.rooms.lock().get(room).unwrap().ack_for(result.message_id, codex).is_some());
    assert!(!dispose("nobody", "dispose-2").ok, "an unaddressed name is refused");
    let caller = mobility_pane(&supervisor, "claude", 9741);
    let pane = supervisor.apply_sideband_request(&caller, SidebandRequest::RoomDispose { room_id: room,
        message_id: result.message_id, recipient: "claude".into(), request_id: None, next_action: Some("x".into()), decline: None });
    assert!(!pane.ok, "a pane disposes of nothing: {pane:?}");
}
