// Included in the supervisor test module. A109 room mobility: operator grants
// persisted with the session, the roaming sideband requests they gate, and the
// join/leave notices. Mock PTYs, test pane processes and temporary catalogs
// only; no harness runs.

/// A running pane whose PTY reports `pid` (the slot keeps no OS pid, so the
/// liveness sweep never prunes it) and a caller proven inside it.
fn mobility_pane(supervisor: &SupervisorHandle, name: &str, pid: u32) -> PaneCaller {
    let (pty, _inputs) = recording_pty_session(pid);
    install_mock_running_session_with_bracketed_paste_enabled(supervisor, name, DriverKind::Claude, pty);
    supervisor.resolve_pane_caller(TestPaneProcess::new(pid + 10_000, [pid])).unwrap()
}

/// A pane that also takes room wakes at once (typed as Grok, which waits for
/// no first prompt), with what is typed into it.
fn mobility_wake_pane(supervisor: &SupervisorHandle, name: &str, pid: u32) -> (PaneCaller, Arc<Mutex<Vec<String>>>) {
    let (pty, inputs) = recording_pty_session(pid);
    install_mock_room_wake_recipient(supervisor, name, pty);
    (supervisor.resolve_pane_caller(TestPaneProcess::new(pid + 10_000, [pid])).unwrap(), inputs)
}

const ROAM: MemberPermissions = MemberPermissions { roam: true, maintain: false, manage: false };

/// Withdraws `session_id`'s grants at the next pane move or peek, after its
/// entry checks and room waits and before its effect.
fn withdraw_grants_before_mobility_commit(supervisor: &SupervisorHandle, session_id: SessionId) {
    let hooked = supervisor.clone();
    *supervisor.inner.room_mobility_before_commit.lock() = Some(Box::new(move || {
        hooked.set_member_permissions(session_id, MemberPermissions::default()).unwrap();
    }));
}

fn mobility_room(supervisor: &SupervisorHandle, label: &str, member_ids: Vec<SessionId>) -> RoomSnapshot {
    supervisor.create_room(CreateRoomRequest { label: Some(label.into()), member_ids,
        brief_on_join: false, brief_template: None }).unwrap()
}

fn member_permissions(supervisor: &SupervisorHandle, session_id: SessionId) -> MemberPermissions {
    supervisor.snapshot().sessions.into_iter().find(|session| session.session_id == session_id).unwrap().permissions
}

fn room_now(supervisor: &SupervisorHandle, room_id: RoomId) -> RoomSnapshot {
    supervisor.snapshot().rooms.into_iter().find(|room| room.room_id == room_id).unwrap()
}

/// Every stored message of the room (operator view), with its session sender.
fn mobility_messages(supervisor: &SupervisorHandle, room_id: RoomId) -> Vec<(Option<SessionId>, String)> {
    supervisor.read_room_feed(ReadRoomFeedRequest { room_id, cursor: None }).unwrap().events.into_iter()
        .filter_map(|event| match event.item {
            RoomFeedItem::Message { sender, content, .. } => Some((match sender {
                RoomMessageSender::Session { session_id } => Some(session_id),
                _ => None,
            }, content)),
            _ => None,
        }).collect()
}

fn fetched_receipts(supervisor: &SupervisorHandle, room_id: RoomId) -> usize {
    supervisor.read_room_feed(ReadRoomFeedRequest { room_id, cursor: None }).unwrap().events.iter()
        .filter(|event| matches!(event.item, RoomFeedItem::Delivery { status: RoomDeliveryStatus::Fetched, .. }))
        .count()
}

fn membership_items(supervisor: &SupervisorHandle, room_id: RoomId, session_id: SessionId) -> Vec<(RoomMembershipAction, RoomRevision)> {
    supervisor.read_room_feed(ReadRoomFeedRequest { room_id, cursor: None }).unwrap().events.into_iter()
        .filter_map(|event| match event.item {
            RoomFeedItem::Membership { action, session_id: member, membership_revision } if member == session_id =>
                Some((action, membership_revision)),
            _ => None,
        }).collect()
}

fn room_move(response: SidebandResponse) -> shared_types::RoomMoveResult {
    assert!(response.ok, "{}", response.message);
    match response.payload {
        Some(SidebandResponsePayload::RoomMove { result }) => result,
        other => panic!("expected a room move, got {other:?}"),
    }
}

fn page_of(response: SidebandResponse) -> RoomFeedPage {
    assert!(response.ok, "{}", response.message);
    match response.payload {
        Some(SidebandResponsePayload::RoomFeed { page }) => page,
        other => panic!("expected a feed page, got {other:?}"),
    }
}

fn page_contents(page: &RoomFeedPage) -> Vec<String> {
    page.events.iter().filter_map(|event| match &event.item {
        RoomFeedItem::Message { content, .. } => Some(content.clone()),
        _ => None,
    }).collect()
}

#[test]
fn member_permissions_persist_with_the_session_and_old_catalogs_load_without_them() {
    let root = tempfile::tempdir().unwrap();
    let mut supervisor = test_supervisor_with_root(root.path().to_path_buf());
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let events = capture_runtime_events(&supervisor);
    assert_eq!(member_permissions(&supervisor, claude), MemberPermissions::default());

    let granted = MemberPermissions { roam: true, maintain: true, manage: false };
    assert_eq!(supervisor.set_member_permissions(claude, granted).unwrap().permissions, granted);
    supervisor.set_member_permissions(claude, granted).unwrap();
    let changes: Vec<_> = events.lock().iter().filter_map(|event| match event {
        RuntimeEvent::SessionMemberPermissionsChanged { session_id, old_permissions, new_permissions, .. } =>
            Some((*session_id, *old_permissions, *new_permissions)),
        _ => None,
    }).collect();
    assert_eq!(changes, [(claude, MemberPermissions::default(), granted)], "an unchanged grant emits nothing");
    assert!(supervisor.set_member_permissions(Uuid::new_v4(), granted).is_err());

    // Only a granted row carries the field; the others stay as written before.
    let path = session_catalog_path(supervisor.runtime_dir());
    let catalog: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let row = |id: SessionId| catalog["sessions"].as_array().unwrap().iter()
        .find(|row| row["session_id"] == serde_json::json!(id)).unwrap().clone();
    assert_eq!(row(claude)["permissions"], serde_json::json!({ "roam": true, "maintain": true }));
    assert!(row(codex).get("permissions").is_none());

    drop(events);
    drop(supervisor);
    supervisor = SupervisorHandle::new(test_supervisor_config(root.path())).unwrap();
    assert_eq!(member_permissions(&supervisor, claude), granted, "grants survive a restart");
    assert_eq!(member_permissions(&supervisor, codex), MemberPermissions::default());

    // A catalog written before grants existed loads with every grant off.
    drop(supervisor);
    let mut catalog: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    for row in catalog["sessions"].as_array_mut().unwrap() {
        row.as_object_mut().unwrap().remove("permissions");
    }
    fs::write(&path, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();
    supervisor = SupervisorHandle::new(test_supervisor_config(root.path())).unwrap();
    assert_eq!(member_permissions(&supervisor, claude), MemberPermissions::default());
    assert_eq!(supervisor.snapshot().sessions.len(), 2);
}

#[test]
fn roaming_requests_are_refused_without_roam_and_change_nothing() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let caller = mobility_pane(&supervisor, "claude", 9301);
    let alpha = mobility_room(&supervisor, "Alpha", vec![claude]);
    let beta = mobility_room(&supervisor, "Beta", vec![codex]);
    let (alpha_before, beta_before) = (room_now(&supervisor, alpha.room_id), room_now(&supervisor, beta.room_id));

    let permissions = supervisor.apply_sideband_request(&caller, SidebandRequest::MemberPermissions {});
    assert!(permissions.ok, "{}", permissions.message);
    assert_eq!(permissions.payload, Some(SidebandResponsePayload::MemberPermissions {
        permissions: MemberPermissions::default() }));
    for request in [
        SidebandRequest::RoomsList {},
        SidebandRequest::RoomPeek { room_id: beta.room_id, cursor: None },
        SidebandRequest::RoomJoin { room_id: beta.room_id },
        SidebandRequest::RoomLeave {},
        SidebandRequest::RoomDeliverTo { room_id: beta.room_id, request_id: None, recipient: "codex".into(), content: "x".into() },
        // Naming its own room is still a roaming request.
        SidebandRequest::RoomDeliverTo { room_id: alpha.room_id, request_id: None, recipient: "all".into(), content: "x".into() },
    ] {
        let refused = supervisor.apply_sideband_request(&caller, request.clone());
        assert!(!refused.ok && refused.message.contains("Can roam"), "{request:?}: {}", refused.message);
        assert!(refused.payload.is_none());
    }
    assert_eq!(room_now(&supervisor, alpha.room_id), alpha_before);
    assert_eq!(room_now(&supervisor, beta.room_id), beta_before);
    assert!(mobility_messages(&supervisor, beta.room_id).is_empty());
    assert_eq!(supervisor.inner.rooms.lock().room_by_session.get(&claude), Some(&alpha.room_id));

    // The operator's grant is read on the next request.
    let granted = MemberPermissions { roam: true, maintain: true, manage: false };
    supervisor.set_member_permissions(claude, granted).unwrap();
    let permissions = supervisor.apply_sideband_request(&caller, SidebandRequest::MemberPermissions {});
    assert_eq!(permissions.payload, Some(SidebandResponsePayload::MemberPermissions { permissions: granted }));
    assert!(supervisor.apply_sideband_request(&caller, SidebandRequest::RoomsList {}).ok);

    // A stale run is refused before any grant is consulted.
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "claude", DriverKind::Claude,
        recording_pty_session(9302).0);
    let stale = supervisor.apply_sideband_request(&caller, SidebandRequest::RoomsList {});
    assert!(!stale.ok && !stale.message.contains("Can roam"), "{}", stale.message);
}

#[test]
fn a_roaming_member_lists_peeks_without_receipts_joins_and_leaves_with_notices() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let grok = create_test_session(&supervisor, "grok", DriverKind::Grok, shared_types::PermissionProfile::Normal).session_id;
    let caller = mobility_pane(&supervisor, "claude", 9311);
    let alpha = mobility_room(&supervisor, "Alpha", vec![claude, codex]);
    let beta = mobility_room(&supervisor, "Beta", vec![grok]);
    supervisor.invite_room_guest(shared_types::InviteRoomGuestRequest { room_id: beta.room_id, label: Some("Scout".into()) }).unwrap();
    supervisor.set_member_permissions(claude, MemberPermissions { roam: true, maintain: false, manage: false }).unwrap();
    let addressed = supervisor.deliver_room_message(DeliverRoomMessageRequest { room_id: alpha.room_id, request_id: None,
        recipients: RoomRecipientSelection::One { session_id: claude }, content: "for claude".into() }).unwrap();
    // Typed at once: no queued wake is left to race the moves below.
    assert_eq!(addressed.receipts[0].status, RoomDeliveryStatus::Written);
    supervisor.post_room_message(PostRoomMessageRequest { room_id: beta.room_id, request_id: None,
        content: "before you came".into() }).unwrap();

    // rooms_list: every room, its members, guests, and where the caller is.
    let listed = supervisor.apply_sideband_request(&caller, SidebandRequest::RoomsList {});
    assert!(listed.ok, "{}", listed.message);
    assert_eq!(listed.message, format!("2 rooms; you are in \"Alpha\" (current_room_id {})", alpha.room_id));
    let Some(SidebandResponsePayload::Rooms { current_room_id, rooms }) = listed.payload else { panic!("missing rooms") };
    assert_eq!(current_room_id, Some(alpha.room_id));
    assert_eq!(rooms.iter().map(|room| room.label.as_str()).collect::<Vec<_>>(), ["Alpha", "Beta"]);
    let labels = |index: usize| rooms[index].members.iter().map(|member| member.label.clone()).collect::<Vec<_>>();
    assert_eq!((labels(0), labels(1)), (vec!["claude".to_string(), "codex".into()], vec!["grok".to_string()]));
    assert_eq!(rooms[1].members[0].driver, DriverKind::Grok);
    assert_eq!((rooms[0].guest_count, rooms[1].guest_count), (0, 1));

    // room_peek is the operator's read: no Fetched receipt, no join floor.
    let own = page_of(supervisor.apply_sideband_request(&caller, SidebandRequest::RoomPeek { room_id: alpha.room_id, cursor: None }));
    assert!(page_contents(&own).contains(&"for claude".to_string()));
    assert_eq!(fetched_receipts(&supervisor, alpha.room_id), 0, "peeking records no receipt");
    page_of(supervisor.apply_sideband_request(&caller, SidebandRequest::RoomRead { owed_after: None, cursor: None, expect_room: None }));
    assert_eq!(fetched_receipts(&supervisor, alpha.room_id), 1, "room_read still records Fetched");
    let other = page_of(supervisor.apply_sideband_request(&caller, SidebandRequest::RoomPeek { room_id: beta.room_id, cursor: None }));
    assert_eq!(other.room_id, beta.room_id);
    assert_eq!(page_contents(&other), ["before you came"]);
    assert_eq!(other.members.iter().map(|member| member.session_id).collect::<Vec<_>>(), [grok]);
    assert_eq!(other.guests.len(), 1);
    let tail = page_of(supervisor.apply_sideband_request(&caller, SidebandRequest::RoomPeek { room_id: beta.room_id,
        cursor: Some(rooms[1].cursor) }));
    assert!(tail.events.is_empty(), "a rooms_list cursor reads only what follows it");
    let unknown = supervisor.apply_sideband_request(&caller, SidebandRequest::RoomPeek { room_id: Uuid::new_v4(), cursor: None });
    assert!(!unknown.ok && unknown.message.contains("unknown room id"), "{}", unknown.message);

    // room_join: leaves Alpha with a notice there, joins Beta with one there.
    let (alpha_revision, beta_revision) = (room_now(&supervisor, alpha.room_id).membership_revision,
        room_now(&supervisor, beta.room_id).membership_revision);
    let moved = room_move(supervisor.apply_sideband_request(&caller, SidebandRequest::RoomJoin { room_id: beta.room_id }));
    assert_eq!(moved.left, Some(shared_types::RoomMembershipChange { room_id: alpha.room_id, label: "Alpha".into(),
        membership_revision: alpha_revision + 1 }));
    assert_eq!(moved.joined, Some(shared_types::RoomMembershipChange { room_id: beta.room_id, label: "Beta".into(),
        membership_revision: beta_revision + 1 }));
    assert!(moved.warnings.is_empty(), "{:?}", moved.warnings);
    assert_eq!(room_now(&supervisor, alpha.room_id).member_ids, [codex]);
    assert_eq!(room_now(&supervisor, beta.room_id).member_ids, [grok, claude]);
    assert_eq!(membership_items(&supervisor, alpha.room_id, claude).last(),
        Some(&(RoomMembershipAction::Removed, alpha_revision + 1)));
    assert_eq!(membership_items(&supervisor, beta.room_id, claude), [(RoomMembershipAction::Joined, beta_revision + 1)]);
    assert_eq!(mobility_messages(&supervisor, alpha.room_id).last(), Some(&(Some(claude), "claude left for Beta".to_string())));
    assert_eq!(mobility_messages(&supervisor, beta.room_id).last(), Some(&(Some(claude), "claude joined".to_string())));

    // room_read, room_post and room_deliver now act in Beta, from the join on.
    let read = page_of(supervisor.apply_sideband_request(&caller, SidebandRequest::RoomRead { owed_after: None, cursor: None, expect_room: None }));
    assert_eq!(read.room_id, beta.room_id);
    assert_eq!(page_contents(&read), ["claude joined"], "history before the join stays behind the floor");
    let again = supervisor.apply_sideband_request(&caller, SidebandRequest::RoomJoin { room_id: beta.room_id });
    assert!(!again.ok && again.message.contains("already in room \"Beta\""), "{}", again.message);

    // room_leave: back to the lobby with a "left" notice.
    let left = room_move(supervisor.apply_sideband_request(&caller, SidebandRequest::RoomLeave {}));
    assert_eq!((left.left.map(|room| room.room_id), left.joined), (Some(beta.room_id), None));
    assert!(!supervisor.inner.rooms.lock().room_by_session.contains_key(&claude));
    assert_eq!(room_now(&supervisor, beta.room_id).member_ids, [grok]);
    assert_eq!(mobility_messages(&supervisor, beta.room_id).last(), Some(&(Some(claude), "claude left".to_string())));
    let lobby_read = supervisor.apply_sideband_request(&caller, SidebandRequest::RoomRead { owed_after: None, cursor: None, expect_room: None });
    assert!(!lobby_read.ok && lobby_read.message.contains("not a room member"), "{}", lobby_read.message);
    let lobby_leave = supervisor.apply_sideband_request(&caller, SidebandRequest::RoomLeave {});
    assert!(!lobby_leave.ok && lobby_leave.message.contains("already in the lobby"), "{}", lobby_leave.message);
    let lobby_list = supervisor.apply_sideband_request(&caller, SidebandRequest::RoomsList {});
    assert_eq!(lobby_list.message, "2 rooms; you are in the lobby (current_room_id is null)");
    assert!(matches!(lobby_list.payload, Some(SidebandResponsePayload::Rooms { current_room_id: None, .. })));

    // From the lobby a join posts only the arrival.
    let alpha_messages = mobility_messages(&supervisor, alpha.room_id).len();
    let back = room_move(supervisor.apply_sideband_request(&caller, SidebandRequest::RoomJoin { room_id: alpha.room_id }));
    assert_eq!((back.left, back.joined.map(|room| room.room_id)), (None, Some(alpha.room_id)));
    let alpha_after = mobility_messages(&supervisor, alpha.room_id);
    assert_eq!(alpha_after.len(), alpha_messages + 1);
    assert_eq!(alpha_after.last(), Some(&(Some(claude), "claude joined".to_string())));
    assert_eq!(mobility_messages(&supervisor, beta.room_id).last(), Some(&(Some(claude), "claude left".to_string())));
}

#[test]
fn a_roaming_member_delivers_to_a_member_of_another_room_through_the_room_wake_path() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let caller = mobility_pane(&supervisor, "claude", 9321);
    let (codex_pty, codex_inputs) = recording_pty_session(9322);
    install_mock_room_wake_recipient(&supervisor, "codex", codex_pty);
    let alpha = mobility_room(&supervisor, "Alpha", vec![claude]);
    let beta = mobility_room(&supervisor, "Beta", vec![codex]);
    let deliver_to = |room_id: RoomId, recipient: &str, content: &str| supervisor.apply_sideband_request(&caller,
        SidebandRequest::RoomDeliverTo { room_id, request_id: None, recipient: recipient.into(), content: content.into() });

    let refused = deliver_to(beta.room_id, "codex", "not yet");
    assert!(!refused.ok && refused.message.contains("Can roam"), "{}", refused.message);
    assert!(codex_inputs.lock().is_empty());
    assert!(mobility_messages(&supervisor, beta.room_id).is_empty());

    supervisor.set_member_permissions(claude, MemberPermissions { roam: true, maintain: false, manage: false }).unwrap();
    let delivered = deliver_to(beta.room_id, "codex", "status from Alpha");
    assert!(delivered.ok, "{}", delivered.message);
    let Some(SidebandResponsePayload::RoomDelivery { result }) = delivered.payload else { panic!("missing delivery") };
    assert_eq!((result.room_id, result.recipient_count, result.written_count), (beta.room_id, 1, 1));
    // v3 W3: the room wake path types the pointer (no label, room or body);
    // the message and its visiting sender are stored in Beta's feed.
    assert_eq!(*codex_inputs.lock(), vec![typed_pointer(&supervisor, 1, result.message_id), "\r".to_string()]);
    assert!(!codex_inputs.lock()[0].contains("visiting") && !codex_inputs.lock()[0].contains("Alpha"));
    assert_eq!(mobility_messages(&supervisor, beta.room_id), [(Some(claude), "status from Alpha".to_string())]);
    assert!(mobility_messages(&supervisor, alpha.room_id).is_empty(), "nothing is stored in the sender's room");
    assert_eq!(room_now(&supervisor, beta.room_id).member_ids, [codex], "delivering joins nothing");

    // Its own room by id is an ordinary delivery; unknown rooms and members are named.
    let own = deliver_to(alpha.room_id, "codex", "x");
    assert!(!own.ok && own.message.contains("no other room member is labelled 'codex'"), "{}", own.message);
    let unknown = deliver_to(Uuid::new_v4(), "codex", "x");
    assert!(!unknown.ok && unknown.message.contains("unknown room id"), "{}", unknown.message);
    let plain = supervisor.apply_sideband_request(&caller, SidebandRequest::RoomDeliver { request_id: None,
        recipient: "codex".into(), content: "x".into() });
    assert!(!plain.ok, "room_deliver without room_id stays inside the caller's room");

    // Withdrawn roam is honoured at once; the member keeps its room.
    supervisor.set_member_permissions(claude, MemberPermissions::default()).unwrap();
    let withdrawn = deliver_to(beta.room_id, "codex", "again");
    assert!(!withdrawn.ok && withdrawn.message.contains("Can roam"), "{}", withdrawn.message);
    assert_eq!(codex_inputs.lock().len(), 2);
    assert_eq!(supervisor.inner.rooms.lock().room_by_session.get(&claude), Some(&alpha.room_id));
}

#[test]
fn the_room_brief_names_the_roaming_tools_only_to_roaming_members() {
    let claude = RoomMember { session_id: Uuid::new_v4(), label: "CC".into(), driver: DriverKind::Claude };
    let grok = RoomMember { session_id: Uuid::new_v4(), label: "Grok".into(), driver: DriverKind::Grok };
    let members = vec![claude.clone(), grok.clone()];
    let roam = MemberPermissions { roam: true, ..MemberPermissions::default() };
    let staying = render_room_brief("Team A", &members, &claude, MemberPermissions::default(), None, None);
    assert!(!staying.contains("roam") && !staying.contains("{roaming}"), "{staying}");
    let roaming = render_room_brief("Team A", &members, &claude, roam, None, None);
    for tool in ["rooms_list", "room_peek", "room_join", "room_leave", "room_deliver with room_id"] {
        assert!(roaming.contains(tool), "{tool}: {roaming}");
    }
    let grok_roaming = render_room_brief("Team A", &members, &grok, roam, None, None);
    assert!(grok_roaming.contains("--prim1-room rooms") && grok_roaming.contains("deliver --room"), "{grok_roaming}");
    assert!(validate_message_framing(&grok_roaming, routed_message_submit_behavior(DriverKind::Grok)).is_ok());
    // An edited brief from before the placeholder still tells a roaming member.
    let edited = render_room_brief("Team A", &members, &claude, roam, None, Some("Tools: {tools}"));
    assert!(edited.contains("rooms_list"), "{edited}");
    assert!(!render_room_brief("Team A", &members, &claude, MemberPermissions::default(), None, Some("Tools: {tools}")).contains("roam"));
}

#[test]
fn moves_and_peeks_recheck_the_caller_after_their_waits() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let caller = mobility_pane(&supervisor, "claude", 9331);
    let alpha = mobility_room(&supervisor, "Alpha", vec![claude]);
    let beta = mobility_room(&supervisor, "Beta", vec![codex]);
    supervisor.post_room_message(PostRoomMessageRequest { room_id: beta.room_id, request_id: None,
        content: "for Beta only".into() }).unwrap();
    supervisor.set_member_permissions(claude, ROAM).unwrap();
    let rooms_now = || (room_now(&supervisor, alpha.room_id), room_now(&supervisor, beta.room_id));
    let unchanged = rooms_now();

    // room_join passes its checks, then roam is withdrawn while it waits for
    // the target's writer: nothing moves and no notice is posted.
    withdraw_grants_before_mobility_commit(&supervisor, claude);
    let join = supervisor.apply_sideband_request(&caller, SidebandRequest::RoomJoin { room_id: beta.room_id });
    assert!(!join.ok && join.message.contains("Can roam") && join.message.contains("nothing changed"), "{}", join.message);
    assert!(supervisor.inner.room_mobility_before_commit.lock().is_none(), "withdrawn after the entry checks");
    assert_eq!(rooms_now(), unchanged);
    assert_eq!(supervisor.inner.rooms.lock().room_by_session.get(&claude), Some(&alpha.room_id));

    // room_peek: withdrawn after its wait, before the read; nothing is returned.
    supervisor.set_member_permissions(claude, ROAM).unwrap();
    withdraw_grants_before_mobility_commit(&supervisor, claude);
    let peek = supervisor.apply_sideband_request(&caller, SidebandRequest::RoomPeek { room_id: beta.room_id, cursor: None });
    assert!(!peek.ok && peek.message.contains("Can roam") && peek.payload.is_none(), "{}", peek.message);
    assert!(supervisor.inner.room_mobility_before_commit.lock().is_none());

    // room_leave: the run is replaced while it waits; the stale caller moves nothing.
    supervisor.set_member_permissions(claude, ROAM).unwrap();
    let hooked = supervisor.clone();
    *supervisor.inner.room_mobility_before_commit.lock() = Some(Box::new(move || {
        install_mock_running_session_with_bracketed_paste_enabled(&hooked, "claude", DriverKind::Claude,
            recording_pty_session(9332).0);
    }));
    let leave = supervisor.apply_sideband_request(&caller, SidebandRequest::RoomLeave {});
    assert!(!leave.ok && leave.message.contains("stale"), "{}", leave.message);
    assert!(supervisor.inner.room_mobility_before_commit.lock().is_none());
    assert_eq!(rooms_now(), unchanged);
    assert_eq!(supervisor.inner.rooms.lock().room_by_session.get(&claude), Some(&alpha.room_id));
}

#[test]
fn a_join_is_one_catalog_write_so_a_failure_never_strands_the_member() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let caller = mobility_pane(&supervisor, "claude", 9341);
    let alpha = mobility_room(&supervisor, "Alpha", vec![claude]);
    let beta = mobility_room(&supervisor, "Beta", vec![codex]);
    supervisor.set_member_permissions(claude, ROAM).unwrap();
    let join = |room_id: RoomId| supervisor.apply_sideband_request(&caller, SidebandRequest::RoomJoin { room_id });

    // When a move was a leave and then a join, this failed the join's write
    // after the leave was saved, leaving the member in the lobby. As one
    // write the move commits whole, and the failure stays armed.
    let hooked = supervisor.clone();
    *supervisor.inner.room_catalog_before_write.lock() = Some(Box::new(move || {
        hooked.fail_next_room_catalog_write_for_tests();
    }));
    let moved = room_move(join(beta.room_id));
    assert_eq!((moved.left.map(|room| room.room_id), moved.joined.map(|room| room.room_id)),
        (Some(alpha.room_id), Some(beta.room_id)));
    assert_eq!(supervisor.inner.rooms.lock().room_by_session.get(&claude), Some(&beta.room_id));

    // The armed failure now refuses the next move's only write: the member
    // stays in Beta, neither room changes, and no notice is posted.
    let unchanged = (room_now(&supervisor, alpha.room_id), room_now(&supervisor, beta.room_id));
    let refused = join(alpha.room_id);
    assert!(!refused.ok && refused.message.contains("injected room catalog persistence failure")
        && refused.message.contains("nothing changed"), "{}", refused.message);
    assert_eq!((room_now(&supervisor, alpha.room_id), room_now(&supervisor, beta.room_id)), unchanged);
    assert_eq!(supervisor.inner.rooms.lock().room_by_session.get(&claude), Some(&beta.room_id));
    assert_eq!(room_move(join(alpha.room_id)).joined.map(|room| room.room_id), Some(alpha.room_id));
}

#[test]
fn a_roaming_delivery_rechecks_roam_when_it_commits() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let caller = mobility_pane(&supervisor, "claude", 9351);
    let (codex_pty, codex_inputs) = recording_pty_session(9352);
    install_mock_room_wake_recipient(&supervisor, "codex", codex_pty);
    mobility_room(&supervisor, "Alpha", vec![claude]);
    let beta = mobility_room(&supervisor, "Beta", vec![codex]);
    supervisor.set_member_permissions(claude, ROAM).unwrap();

    // Withdrawn after the delivery spec was built and authorized.
    let hooked = supervisor.clone();
    supervisor.set_room_delivery_after_preflight_for_tests(move || {
        hooked.set_member_permissions(claude, MemberPermissions::default()).unwrap();
    });
    let refused = supervisor.apply_sideband_request(&caller, SidebandRequest::RoomDeliverTo { room_id: beta.room_id,
        request_id: None, recipient: "codex".into(), content: "after the grant was withdrawn".into() });
    assert!(!refused.ok && refused.message.contains("Can roam"), "{}", refused.message);
    assert!(supervisor.inner.room_delivery_after_preflight.lock().is_none(), "withdrawn after preflight");
    assert!(mobility_messages(&supervisor, beta.room_id).is_empty(), "nothing is stored in the target feed");
    assert!(codex_inputs.lock().is_empty(), "nothing is typed");
    assert!(supervisor.inner.rooms.lock().cross_room_senders.is_empty(), "no answer is admitted");
}

#[test]
fn a_member_answers_a_visitor_with_plain_room_deliver_while_the_visitor_roams() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let (visitor, claude_inputs) = mobility_wake_pane(&supervisor, "claude", 9361);
    let (member, codex_inputs) = mobility_wake_pane(&supervisor, "codex", 9363);
    let alpha = mobility_room(&supervisor, "Alpha", vec![claude]);
    let beta = mobility_room(&supervisor, "Beta", vec![codex]);
    supervisor.set_member_permissions(claude, ROAM).unwrap();
    let answer = |recipient: &str, content: &str| supervisor.apply_sideband_request(&member,
        SidebandRequest::RoomDeliver { request_id: None, recipient: recipient.into(), content: content.into() });

    let early = answer("claude", "hello?");
    assert!(!early.ok && early.message.contains("no other room member is labelled 'claude'"), "{}", early.message);

    let visit = supervisor.apply_sideband_request(&visitor, SidebandRequest::RoomDeliverTo { room_id: beta.room_id,
        request_id: None, recipient: "codex".into(), content: "question from Alpha".into() });
    assert!(visit.ok, "{}", visit.message);
    let Some(SidebandResponsePayload::RoomDelivery { result: visit }) = visit.payload else { panic!("missing visit") };
    // v3 W3: the visit wakes codex with a pointer.
    assert_eq!(codex_inputs.lock()[0], typed_pointer(&supervisor, 1, visit.message_id));

    // codex may not roam; its plain room_deliver reaches the visitor in Alpha.
    let answered = answer("claude", "answer from Beta");
    assert!(answered.ok, "{}", answered.message);
    let Some(SidebandResponsePayload::RoomDelivery { result }) = answered.payload else { panic!("missing delivery") };
    assert_eq!((result.room_id, result.recipient_count, result.written_count), (alpha.room_id, 1, 1));
    assert_eq!(*claude_inputs.lock(), vec![typed_pointer(&supervisor, 1, result.message_id), "\r".to_string()]);
    assert_eq!(mobility_messages(&supervisor, alpha.room_id), [(Some(codex), "answer from Beta".to_string())]);
    assert_eq!(mobility_messages(&supervisor, beta.room_id), [(Some(claude), "question from Alpha".to_string())]);
    assert_eq!((room_now(&supervisor, alpha.room_id).member_ids, room_now(&supervisor, beta.room_id).member_ids),
        (vec![claude], vec![codex]), "answering joins nothing");
    // The label as the message header shows it works too.
    assert!(answer("claude (visiting from Alpha)", "second answer").ok);
    assert_eq!(claude_inputs.lock().len(), 4);

    // Rechecked at commit: the visitor's roam withdrawn after the answer was resolved.
    let hooked = supervisor.clone();
    supervisor.set_room_delivery_after_preflight_for_tests(move || {
        hooked.set_member_permissions(claude, MemberPermissions::default()).unwrap();
    });
    let late = answer("claude", "after roam was withdrawn");
    assert!(!late.ok && late.message.contains("no longer lets it roam"), "{}", late.message);
    assert_eq!(mobility_messages(&supervisor, alpha.room_id).len(), 2, "nothing more is stored");
    assert_eq!(claude_inputs.lock().len(), 4, "nothing more is typed");
    let withdrawn = answer("claude", "x");
    assert!(!withdrawn.ok && withdrawn.message.contains("no longer lets it roam"), "{}", withdrawn.message);

    // A visitor in the lobby has no room to be answered in.
    supervisor.set_member_permissions(claude, ROAM).unwrap();
    room_move(supervisor.apply_sideband_request(&visitor, SidebandRequest::RoomLeave {}));
    let lobby = answer("claude", "x");
    assert!(!lobby.ok && lobby.message.contains("in the lobby now"), "{}", lobby.message);

    // Only within 24 hours of the visitor's last delivery.
    room_move(supervisor.apply_sideband_request(&visitor, SidebandRequest::RoomJoin { room_id: alpha.room_id }));
    supervisor.inner.rooms.lock().cross_room_senders
        .insert((codex, claude), chrono::Utc::now() - chrono::Duration::hours(25));
    let expired = answer("claude", "x");
    assert!(!expired.ok && expired.message.contains("no other room member is labelled 'claude'")
        && expired.message.contains("no reply window to \"claude\" in another room")
        && expired.message.contains("before PRIM-1 restarted or over 24 hours ago"), "{}", expired.message);
}

#[test]
fn an_explicit_own_room_id_still_needs_roam_when_it_commits() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let caller = mobility_pane(&supervisor, "claude", 9381);
    let (codex_pty, codex_inputs) = recording_pty_session(9382);
    install_mock_room_wake_recipient(&supervisor, "codex", codex_pty);
    let alpha = mobility_room(&supervisor, "Alpha", vec![claude, codex]);
    supervisor.set_member_permissions(claude, ROAM).unwrap();
    let deliver = |content: &str| supervisor.apply_sideband_request(&caller, SidebandRequest::RoomDeliverTo {
        room_id: alpha.room_id, request_id: None, recipient: "codex".into(), content: content.into() });

    // Roam withdrawn after the spec was built: the room_id was the caller's
    // own, so the sender is no visitor, but the request still needs roam.
    let hooked = supervisor.clone();
    supervisor.set_room_delivery_after_preflight_for_tests(move || {
        hooked.set_member_permissions(claude, MemberPermissions::default()).unwrap();
    });
    let refused = deliver("after the grant was withdrawn");
    assert!(!refused.ok && refused.message.contains("Can roam"), "{}", refused.message);
    assert!(supervisor.inner.room_delivery_after_preflight.lock().is_none(), "withdrawn after preflight");
    assert!(mobility_messages(&supervisor, alpha.room_id).is_empty(), "nothing is stored");
    assert!(codex_inputs.lock().is_empty(), "nothing is typed");

    // With roam it is an ordinary own-room delivery: no visiting label and
    // no answer window for the recipient.
    supervisor.set_member_permissions(claude, ROAM).unwrap();
    let own = deliver("own room by id");
    assert!(own.ok, "{}", own.message);
    let Some(SidebandResponsePayload::RoomDelivery { result: own }) = own.payload else { panic!("missing delivery") };
    // v3 W3: the wake is the pointer; the stored sender is the plain member.
    assert_eq!(*codex_inputs.lock(), vec![typed_pointer(&supervisor, 1, own.message_id), "\r".to_string()]);
    assert_eq!(mobility_messages(&supervisor, alpha.room_id), [(Some(claude), "own room by id".to_string())]);
    assert!(supervisor.inner.rooms.lock().cross_room_senders.is_empty(), "no same-room reply window");
}

#[test]
fn only_a_name_that_matches_no_member_falls_back_to_a_visitor() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let (visitor, claude_inputs) = mobility_wake_pane(&supervisor, "claude", 9401);
    let (member, codex_inputs) = mobility_wake_pane(&supervisor, "codex", 9403);
    // Labels are presentation only: two of codex's room-mates are also
    // "Claude", and a second roaming visitor is labelled like codex itself.
    let local = [create_test_session(&supervisor, "Claude", DriverKind::Grok, shared_types::PermissionProfile::Normal),
        create_test_session(&supervisor, "Claude", DriverKind::Grok, shared_types::PermissionProfile::Normal)]
        .map(|session| session.session_id);
    let namesake = create_test_session(&supervisor, "Codex", DriverKind::Grok, shared_types::PermissionProfile::Normal)
        .session_id;
    let alpha = mobility_room(&supervisor, "Alpha", vec![claude, namesake]);
    let beta = mobility_room(&supervisor, "Beta", vec![codex, local[0], local[1]]);
    supervisor.set_member_permissions(claude, ROAM).unwrap();
    supervisor.set_member_permissions(namesake, ROAM).unwrap();
    let visit = supervisor.apply_sideband_request(&visitor, SidebandRequest::RoomDeliverTo { room_id: beta.room_id,
        request_id: None, recipient: codex.to_string(), content: "question from Alpha".into() });
    assert!(visit.ok, "{}", visit.message);
    assert_eq!(codex_inputs.lock().len(), 2);
    supervisor.inner.rooms.lock().cross_room_senders.insert((codex, namesake), chrono::Utc::now());
    let answer = |recipient: &str| supervisor.apply_sideband_request(&member,
        SidebandRequest::RoomDeliver { request_id: None, recipient: recipient.into(), content: "answer".into() });

    // Ambiguous in codex's own room: refused as before, nothing reaches the visitor.
    let ambiguous = answer("claude");
    assert!(!ambiguous.ok && ambiguous.message.contains("'claude' names more than one room member; use a session id"),
        "{}", ambiguous.message);
    // The caller itself, by id or by its own label: refused as before.
    let by_id = answer(&codex.to_string());
    assert!(!by_id.ok && by_id.message.contains("room delivery cannot target the calling pane itself"), "{}", by_id.message);
    let by_label = answer("codex");
    assert!(!by_label.ok && by_label.message.starts_with("no other room member is labelled 'codex'")
        && !by_label.message.contains("reply window"), "{}", by_label.message);
    let empty = answer(" ");
    assert!(!empty.ok && empty.message.contains("needs a recipient"), "{}", empty.message);
    assert!(mobility_messages(&supervisor, alpha.room_id).is_empty(), "nothing was sent to Alpha");
    assert!(claude_inputs.lock().is_empty(), "nothing was typed into the visitor");

    // A genuine no-match still reaches the visitor: by its session id, and by
    // its label once no room-mate carries it.
    assert!(answer(&claude.to_string()).ok);
    for session_id in local {
        supervisor.remove_room_member(RemoveRoomMemberRequest { room_id: beta.room_id, session_id }).unwrap();
    }
    let answered = answer("claude");
    assert!(answered.ok, "{}", answered.message);
    let Some(SidebandResponsePayload::RoomDelivery { result }) = answered.payload else { panic!("missing delivery") };
    assert_eq!(result.room_id, alpha.room_id);
    assert_eq!(mobility_messages(&supervisor, alpha.room_id), vec![(Some(codex), "answer".to_string()); 2]);
    assert_eq!(claude_inputs.lock().len(), 4);
}

#[test]
fn a_restart_closes_the_reply_window_and_the_refusal_says_so() {
    let root = tempfile::tempdir().unwrap();
    let mut supervisor = test_supervisor_with_root(root.path().to_path_buf());
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let (visitor, _) = mobility_wake_pane(&supervisor, "claude", 9411);
    mobility_wake_pane(&supervisor, "codex", 9413);
    let alpha = mobility_room(&supervisor, "Alpha", vec![claude]);
    let beta = mobility_room(&supervisor, "Beta", vec![codex]);
    supervisor.set_member_permissions(claude, ROAM).unwrap();
    let visit = supervisor.apply_sideband_request(&visitor, SidebandRequest::RoomDeliverTo { room_id: beta.room_id,
        request_id: None, recipient: "codex".into(), content: "question from Alpha".into() });
    assert!(visit.ok, "{}", visit.message);
    assert!(!supervisor.inner.rooms.lock().cross_room_senders.is_empty());

    drop(supervisor);
    supervisor = SupervisorHandle::new(test_supervisor_config(root.path())).unwrap();
    assert_eq!(member_permissions(&supervisor, claude), ROAM, "the grant survives; the window does not");
    let (_, claude_inputs) = mobility_wake_pane(&supervisor, "claude", 9415);
    let (member, _) = mobility_wake_pane(&supervisor, "codex", 9417);
    let refused = supervisor.apply_sideband_request(&member,
        SidebandRequest::RoomDeliver { request_id: None, recipient: "claude".into(), content: "answer".into() });
    assert!(!refused.ok && refused.message.contains("no reply window to \"claude\" in another room")
        && refused.message.contains("before PRIM-1 restarted"), "{}", refused.message);
    assert!(mobility_messages(&supervisor, alpha.room_id).is_empty());
    assert!(claude_inputs.lock().is_empty());
}

#[test]
fn a_retried_cross_room_wake_is_still_headed_as_visiting() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let caller = mobility_pane(&supervisor, "claude", 9371);
    let (codex_pty, codex_inputs) = recording_pty_session(9372);
    install_mock_room_wake_recipient(&supervisor, "codex", codex_pty);
    mobility_room(&supervisor, "Alpha", vec![claude]);
    let beta = mobility_room(&supervisor, "Beta", vec![codex]);
    supervisor.set_member_permissions(claude, ROAM).unwrap();
    // The wake's Pending receipt is torn: no transport, Held after recovery.
    let inner = Arc::downgrade(&supervisor.inner);
    let beta_id = beta.room_id;
    supervisor.set_room_event_after_append_for_tests(move || {
        inner.upgrade().unwrap().rooms.lock().get_mut(beta_id).unwrap().fail_next_commit_for_tests(false);
    });
    let deliver = || supervisor.apply_sideband_request(&caller, SidebandRequest::RoomDeliverTo { room_id: beta.room_id,
        request_id: Some("visit".into()), recipient: "codex".into(), content: "held at first".into() });
    let first = deliver();
    let Some(SidebandResponsePayload::RoomDelivery { result: first }) = first.payload else { panic!("{}", first.message) };
    assert!(!first.receipts[0].recorded);
    assert!(deliver().ok, "the replay records the Held receipt");
    assert!(codex_inputs.lock().is_empty());
    let cursor = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: beta.room_id, cursor: None }).unwrap()
        .events.last().unwrap().cursor;
    let retried = supervisor.retry_room_delivery(shared_types::RetryRoomDeliveryRequest { room_id: beta.room_id,
        message_id: first.message_id, recipient_id: codex, expected_receipt: cursor }).unwrap();
    assert_eq!(retried.written_count, 1);
    // v3 W3: the retried wake is the same pointer as a first wake; the
    // visiting sender stays recorded in Beta's feed, not in the paste.
    assert_eq!(*codex_inputs.lock(), vec![typed_pointer(&supervisor, 1, first.message_id), "\r".to_string()]);
    assert_eq!(mobility_messages(&supervisor, beta.room_id), [(Some(claude), "held at first".to_string())]);
}

#[test]
fn prime_and_terminal_panes_take_no_grants() {
    let supervisor = test_supervisor();
    supervisor.set_wsl_control_for_tests(Arc::new(FakeWslControl::new()));
    let prime = supervisor.create_session(shared_types::CreateSessionRequest { codex_transport: Default::default(), permissions: None, launch_choices: None, room_id: None,
        label: Some("prime".into()), driver: DriverKind::Prime, permission_profile: shared_types::PermissionProfile::Normal,
        linux_working_directory: Some("/home/test".into()) }).unwrap().session_id;
    let terminal = create_test_session(&supervisor, "terminal", DriverKind::GenericTerminal,
        shared_types::PermissionProfile::Normal).session_id;
    let both = MemberPermissions { roam: true, maintain: true, manage: false };
    for session_id in [prime, terminal] {
        // A110: manage alone is refused too.
        for permissions in [ROAM, MemberPermissions { roam: false, maintain: true, manage: false }, both,
            MemberPermissions { manage: true, ..MemberPermissions::default() }] {
            let error = supervisor.set_member_permissions(session_id, permissions).unwrap_err().to_string();
            assert!(error.contains("have no room tools"), "{error}");
            assert_eq!(member_permissions(&supervisor, session_id), MemberPermissions::default());
        }
        // A grant stored before this check can still be cleared.
        supervisor.inner.slots.lock().get_by_id_mut(session_id).unwrap().permissions = both;
        assert_eq!(supervisor.set_member_permissions(session_id, MemberPermissions::default()).unwrap().permissions,
            MemberPermissions::default());
    }
    let grok = create_test_session(&supervisor, "grok", DriverKind::Grok, shared_types::PermissionProfile::Normal).session_id;
    for session_id in [test_session_id(&supervisor, "claude"), test_session_id(&supervisor, "codex"), grok] {
        assert_eq!(supervisor.set_member_permissions(session_id, both).unwrap().permissions, both);
    }
}
