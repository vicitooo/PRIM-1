#[test]
fn pane_cards_bind_author_room_join_floor_and_never_write_a_pty() {
    let supervisor = test_supervisor();
    let author = test_session_id(&supervisor, "claude");
    let reader = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Typed cards".into()),
        member_ids: vec![author], brief_on_join: false, brief_template: None }).unwrap();
    let (pty, inputs) = recording_pty_session(101);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude,
        Some(101), pty, BracketedPasteMode::Unknown);
    let caller = supervisor.resolve_pane_caller(TestPaneProcess::new(9001, [101])).unwrap();
    let card: shared_types::RoomCardContent = serde_json::from_value(serde_json::json!({
        "key":"result", "kind":"report", "title":"Before join", "summary":"Private prefix" })).unwrap();
    let request = SidebandRequest::RoomPostCard { request_id: Some("same".into()), card: card.clone() };
    let stored = supervisor.apply_sideband_request(&caller, request.clone());
    assert!(stored.ok, "{}", stored.message);
    let Some(SidebandResponsePayload::RoomCard { result }) = stored.payload else { panic!("card receipt missing") };
    assert_eq!(result.room_id, room.room_id);
    assert!(!result.replayed);
    let repeated = supervisor.apply_sideband_request(&caller, request.clone());
    let Some(SidebandResponsePayload::RoomCard { result: repeated }) = repeated.payload else { panic!("card receipt missing") };
    assert!(repeated.replayed);
    assert_eq!(result.card_id, repeated.card_id);
    supervisor.add_room_member(AddRoomMemberRequest { room_id: room.room_id, session_id: reader }).unwrap();
    let (pty, reader_inputs) = recording_pty_session(102);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "codex", DriverKind::Codex,
        Some(102), pty, BracketedPasteMode::Unknown);
    let reader_caller = supervisor.resolve_pane_caller(TestPaneProcess::new(9002, [102])).unwrap();
    let read = supervisor.apply_sideband_request(&reader_caller, SidebandRequest::RoomRead { owed_after: None, cursor: None, expect_room: None });
    let Some(SidebandResponsePayload::RoomFeed { page }) = read.payload else { panic!("page missing") };
    assert!(page.cards.is_empty(), "snapshot cannot bypass the join floor");
    let mut update = card; update.summary = "Published after join".into();
    assert!(supervisor.apply_sideband_request(&caller, SidebandRequest::RoomPostCard {
        request_id: Some("update".into()), card: update }).ok);
    let read = supervisor.apply_sideband_request(&reader_caller, SidebandRequest::RoomRead { owed_after: None, cursor: None, expect_room: None });
    let Some(SidebandResponsePayload::RoomFeed { page }) = read.payload else { panic!("page missing") };
    assert_eq!(page.cards.len(), 1);
    assert!(matches!(&page.cards[0].item, RoomFeedItem::Card { author: id, card, .. }
        if *id == author && card.summary == "Published after join"));
    supervisor.remove_room_member(RemoveRoomMemberRequest { room_id: room.room_id, session_id: author }).unwrap();
    assert!(!supervisor.apply_sideband_request(&caller, request).ok);
    assert!(inputs.lock().is_empty());
    assert!(reader_inputs.lock().is_empty());
}

#[test]
fn pane_card_receipt_uses_the_room_bound_when_the_worker_commits() {
    let supervisor = test_supervisor();
    let author = test_session_id(&supervisor, "claude");
    let first = supervisor.create_room(CreateRoomRequest { label: Some("Initial room".into()),
        member_ids: vec![author], brief_on_join: false, brief_template: None }).unwrap();
    let second = supervisor.create_room(CreateRoomRequest { label: Some("Commit room".into()),
        member_ids: vec![], brief_on_join: false, brief_template: None }).unwrap();
    let (pty, inputs) = recording_pty_session(101);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude,
        Some(101), pty, BracketedPasteMode::Unknown);
    let caller = supervisor.resolve_pane_caller(TestPaneProcess::new(9001, [101])).unwrap();
    let mover = supervisor.clone();
    *supervisor.inner.sideband_after_initial_authorization.lock() = Some(Box::new(move || {
        mover.remove_room_member(RemoveRoomMemberRequest { room_id: first.room_id, session_id: author }).unwrap();
        mover.add_room_member(AddRoomMemberRequest { room_id: second.room_id, session_id: author }).unwrap();
    }));
    let request = SidebandRequest::RoomPostCard { request_id: Some("moved-room".into()),
        card: serde_json::from_value(serde_json::json!({"key":"move", "kind":"report",
            "title":"Moved", "summary":"Commit-time membership"})).unwrap() };
    let response = supervisor.apply_sideband_request(&caller, request.clone());
    assert!(response.ok, "{}", response.message);
    let Some(SidebandResponsePayload::RoomCard { result }) = response.payload else { panic!("card receipt missing") };
    let page = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: second.room_id, cursor: None }).unwrap();
    assert_eq!(page.cards.len(), 1);
    assert_eq!(result.room_id, page.cards[0].room_id);
    assert_eq!(result.room_id, second.room_id);
    assert_eq!(result.cursor, page.cards[0].cursor);
    assert!(matches!(&page.cards[0].item, RoomFeedItem::Card { card_id, author: id, .. }
        if *card_id == result.card_id && *id == author));
    assert!(supervisor.read_room_feed(ReadRoomFeedRequest { room_id: first.room_id, cursor: None }).unwrap().cards.is_empty());
    let repeated = supervisor.apply_sideband_request(&caller, request);
    let Some(SidebandResponsePayload::RoomCard { result: repeated }) = repeated.payload else { panic!("card receipt missing") };
    assert!(repeated.replayed);
    assert_eq!(repeated.room_id, second.room_id);
    assert_eq!(repeated.card_id, result.card_id);
    assert!(inputs.lock().is_empty());
}

#[test]
fn card_metadata_audit_omits_all_agent_content_and_media() {
    let card: shared_types::RoomCardContent = serde_json::from_value(serde_json::json!({
        "key":"private-key", "kind":"report", "title":"Private title", "summary":"Private summary",
        "body":"Private body" })).unwrap();
    let event = RuntimeEvent::RoomFeedEvent { feed_event: shared_types::RoomFeedEvent {
        schema_version: 1, room_id: Uuid::new_v4(),
        cursor: shared_types::RoomFeedCursor { epoch: Uuid::new_v4(), sequence: 1 },
        timestamp: shared_types::now_rfc3339(),
        item: RoomFeedItem::Card { request_id: "metadata".into(), card_id: Uuid::new_v4(), author: Uuid::new_v4(), card } } };
    let supervisor = test_supervisor();
    let audited = serde_json::to_string(&audit_event_projection(&event, &supervisor.inner)).unwrap();
    assert!(!audited.contains("Private"));
    assert!(!audited.contains("private-key"));
}
