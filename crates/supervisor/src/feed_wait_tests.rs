// A16 (T-31): a pane's room_wait long-poll, and the operator's Post reaching
// the room's Supervisors natively. Real catalog, feed and delivery paths with
// mock PTYs.

const A16_SUPERVISOR: MemberPermissions = MemberPermissions { roam: true, maintain: true, manage: true };

fn a16_feed_page(response: &SidebandResponse) -> &RoomFeedPage {
    match &response.payload {
        Some(SidebandResponsePayload::RoomFeed { page }) => page,
        _ => panic!("no feed page: {response:?}"),
    }
}

#[test]
fn a16_pane_room_wait_returns_on_news_from_others_and_times_out_otherwise() {
    let supervisor = test_supervisor();
    let waiter = test_session_id(&supervisor, "claude");
    let peer = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Waits".into()),
        member_ids: vec![waiter, peer], brief_on_join: false, brief_template: None }).unwrap();
    let (pty, waiter_inputs) = recording_pty_session(101);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude,
        Some(101), pty, BracketedPasteMode::Unknown);
    let caller = supervisor.resolve_pane_caller(TestPaneProcess::new(9001, [101])).unwrap();
    let (pty, _peer_inputs) = recording_pty_session(102);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "codex", DriverKind::Codex,
        Some(102), pty, BracketedPasteMode::Unknown);
    let peer_caller = supervisor.resolve_pane_caller(TestPaneProcess::new(9002, [102])).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let wait = |cursor, timeout_ms| runtime.block_on(
        supervisor.apply_sideband_request_async(&caller, SidebandRequest::RoomWait { cursor, timeout_ms }));
    let cursor = a16_feed_page(&supervisor.apply_sideband_request(&caller, SidebandRequest::RoomRead { owed_after: None, cursor: None, expect_room: None }))
        .cursor;

    // Its own post and card move the cursor but do not end the wait.
    assert!(supervisor.apply_sideband_request(&caller, SidebandRequest::RoomPost {
        content: "mine".into(), request_id: None }).ok);
    let card: shared_types::RoomCardContent = serde_json::from_value(serde_json::json!({
        "key": "status", "kind": "report", "title": "Mine", "summary": "own card" })).unwrap();
    assert!(supervisor.apply_sideband_request(&caller, SidebandRequest::RoomPostCard { request_id: None, card }).ok);
    let started = Instant::now();
    let quiet = wait(cursor, Some(300));
    assert!(quiet.ok && quiet.timed_out, "{quiet:?}");
    assert!(started.elapsed() >= Duration::from_millis(250));
    let cursor = a16_feed_page(&quiet).cursor;

    // Someone else's post ends it at once.
    let (poster, peer_pane) = (supervisor.clone(), peer_caller.clone());
    let writer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(150));
        assert!(poster.apply_sideband_request(&peer_pane, SidebandRequest::RoomPost {
            content: "news".into(), request_id: None }).ok);
    });
    let started = Instant::now();
    let woke = wait(cursor, None);
    writer.join().unwrap();
    assert!(woke.ok && !woke.timed_out, "{woke:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(guest_message_contents(a16_feed_page(&woke)), ["news"]);
    let cursor = a16_feed_page(&woke).cursor;

    // The operator's plain post is news as well (no Supervisor here: stored, typed to no one).
    supervisor.post_room_message(PostRoomMessageRequest { room_id: room.room_id, content: "from the operator".into(),
        request_id: None }).unwrap();
    let operator = wait(cursor, Some(10_000));
    assert!(operator.ok && !operator.timed_out, "{operator:?}");
    assert_eq!(guest_message_contents(a16_feed_page(&operator)), ["from the operator"]);
    let cursor = a16_feed_page(&operator).cursor;

    // A cursor from another epoch (a restart, another room) answers at once with the gap.
    let stale = wait(shared_types::RoomFeedCursor { epoch: Uuid::new_v4(), sequence: 1 }, Some(10_000));
    assert!(stale.ok && !stale.timed_out, "{stale:?}");
    assert!(matches!(&a16_feed_page(&stale).gap,
        Some(shared_types::RoomFeedGap { reason: shared_types::RoomFeedGapReason::EpochReset, .. })));

    // Too many waiters: the extra one answers after the slot grace, never at once.
    let permits: Vec<_> = (0..feed_wait::PANE_WAIT_SLOTS)
        .map(|_| supervisor.inner.pane_waits.clone().try_acquire_owned().unwrap()).collect();
    let started = Instant::now();
    let busy = wait(cursor, Some(10_000));
    assert!(busy.ok && busy.timed_out && busy.message.contains("too many members"), "{busy:?}");
    assert!(started.elapsed() >= feed_wait::PANE_WAIT_SLOT_GRACE - Duration::from_millis(50));
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(permits);

    // One open wait per pane: a second one gets the grace, not a slot.
    let (first, second) = runtime.block_on(async {
        let first = supervisor.apply_sideband_request_async(&caller,
            SidebandRequest::RoomWait { cursor, timeout_ms: Some(3_000) });
        let second = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let started = Instant::now();
            let second = supervisor.apply_sideband_request_async(&caller,
                SidebandRequest::RoomWait { cursor, timeout_ms: Some(10_000) }).await;
            assert!(started.elapsed() < Duration::from_secs(3));
            second
        };
        tokio::join!(first, second)
    });
    assert!(second.ok && second.timed_out && second.message.contains("another wait of yours"), "{second:?}");
    assert!(first.ok && first.timed_out && !first.message.contains("another wait"), "{first:?}");
    assert_eq!(supervisor.inner.pane_waits.available_permits(), feed_wait::PANE_WAIT_SLOTS);
    assert!(supervisor.inner.pane_waits_by_session.lock().is_empty(), "the pane's turn goes with its last wait");

    // Each read checks the caller again: out of the room, it is refused.
    supervisor.remove_room_member(RemoveRoomMemberRequest { room_id: room.room_id, session_id: waiter }).unwrap();
    assert!(!wait(cursor, Some(100)).ok);
    assert!(waiter_inputs.lock().is_empty(), "a wait never writes into a terminal");
    supervisor.shutdown().unwrap();
}

#[test]
fn a16_operator_posts_wake_only_the_rooms_supervisors_and_stay_distinct_from_fallbacks() {
    let (supervisor, lead, room, lead_inputs) = fallback_fixture(true);
    let worker = test_session_id(&supervisor, "codex");
    supervisor.add_room_member(AddRoomMemberRequest { room_id: room, session_id: worker }).unwrap();
    let (pty, worker_inputs) = recording_pty_session(9922);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "codex", DriverKind::Codex, None, pty,
        BracketedPasteMode::Enabled);
    let message = |cursor: shared_types::RoomFeedCursor| supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor: None })
        .unwrap().events.into_iter().find(|event| event.cursor == cursor).map(|event| event.item).unwrap();

    // No Supervisor in the room: stored, addressed to no one, typed to no one.
    let plain = supervisor.post_room_message(PostRoomMessageRequest { room_id: room, content: "noted".into(),
        request_id: None }).unwrap();
    assert!(matches!(message(plain.cursor), RoomFeedItem::Message { recipient_ids, .. } if recipient_ids.is_empty()));
    assert!(lead_inputs.lock().is_empty() && worker_inputs.lock().is_empty());

    // The lead is designated Supervisor (all three grants); the worker holds two of them.
    supervisor.set_member_permissions(lead, A16_SUPERVISOR).unwrap();
    supervisor.set_member_permissions(worker, MemberPermissions { roam: true, maintain: false, manage: true }).unwrap();
    let request = PostRoomMessageRequest { room_id: room, content: "Victor: are you seeing this?".into(),
        request_id: Some("operator-post-1".into()) };
    let posted = supervisor.post_room_message(request.clone()).unwrap();
    assert!(!posted.replayed);
    let RoomFeedItem::Message { sender, recipient_ids, fallback_origin, .. } = message(posted.cursor) else { unreachable!() };
    assert_eq!(sender, RoomMessageSender::Operator {});
    assert_eq!(recipient_ids, vec![lead]);
    assert!(fallback_origin.is_none());
    // The wake is the paste and then its submit. v3 W3: an operator post to
    // the Supervisor asks for an ACK, so the paste is the pointer; the
    // operator's words reach the lead through room_read.
    let woken = lead_inputs.lock().len();
    let typed = lead_inputs.lock().concat();
    assert!(woken > 0 && typed.starts_with(&typed_pointer(&supervisor, 1, posted.message_id)), "{typed:?}");
    assert!(!typed.contains("Victor: are you seeing this?"));
    assert!(matches!(message(posted.cursor), RoomFeedItem::Message { content, .. } if content == "Victor: are you seeing this?"));
    assert!(!typed.contains("[PRIM-1 fallback"));
    assert!(worker_inputs.lock().is_empty(), "two grants are not a Supervisor");

    // A retry of the same post is that post: nothing typed again.
    let replay = supervisor.post_room_message(request).unwrap();
    assert!(replay.replayed && replay.message_id == posted.message_id);
    assert_eq!(lead_inputs.lock().len(), woken);

    // A scheduled fallback to the same Supervisor keeps its own marking, never the operator's.
    supervisor.inner.slots.lock().get_by_id_mut(lead).unwrap().work_state = WorkState::Thinking;
    fallback_save(&supervisor, room, fallback_settings(15));
    let mut clock = room_fallbacks::FallbackClock::default(); let now = Utc::now(); let mono = Instant::now();
    fallback_tick(&supervisor, &mut clock, now, mono, 0);
    fallback_tick(&supervisor, &mut clock, now, mono, 900);
    let posts = fallback_posts(&supervisor, room);
    assert!(!posts.is_empty() && posts.iter().all(|event|
        matches!(&event.item, RoomFeedItem::Message { message_id, .. } if *message_id != posted.message_id)));
    // v3 W6 (Reviewer 9ba4c476): the reminder's own marking is its stored
    // fallback_origin, and it asks for an ACK; its wake is a pointer that
    // counts both owed requests (the unacknowledged operator post and the
    // reminder), never the operator's words or a typed reminder body.
    assert!(posts.iter().all(|event| matches!(&event.item,
        RoomFeedItem::Message { fallback_origin: Some(_), ack_requested: true, .. })));
    let fallback = lead_inputs.lock()[woken..].concat();
    assert!(fallback.contains("[PRIM-1] 2 new room messages for you"), "{fallback:?}");
    assert!(!fallback.contains("Room message from operator") && !fallback.contains("Victor: are you seeing this?"));
    supervisor.shutdown().unwrap();
}

#[test]
fn a16_a_retried_operator_post_replays_whoever_is_supervisor_now() {
    let (supervisor, lead, room, lead_inputs) = fallback_fixture(true);
    let posts_with = |request_id: &str| supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor: None })
        .unwrap().events.into_iter()
        .filter(|event| matches!(&event.item, RoomFeedItem::Message { request_id: id, .. } if id == request_id)).count();

    // Stored with no Supervisor in the room; the lead is designated before the retry.
    let first = PostRoomMessageRequest { room_id: room, content: "noted before".into(), request_id: Some("retry-a".into()) };
    let stored = supervisor.post_room_message(first.clone()).unwrap();
    supervisor.set_member_permissions(lead, A16_SUPERVISOR).unwrap();
    let replay = supervisor.post_room_message(first).unwrap();
    assert!(replay.replayed && replay.message_id == stored.message_id && replay.cursor == stored.cursor, "{replay:?}");
    assert!(lead_inputs.lock().is_empty(), "a replay never wakes the new Supervisor");

    // Stored to the Supervisor; the grant is withdrawn before the retry.
    let second = PostRoomMessageRequest { room_id: room, content: "for the supervisor".into(), request_id: Some("retry-b".into()) };
    let delivered = supervisor.post_room_message(second.clone()).unwrap();
    let woken = lead_inputs.lock().len();
    assert!(woken > 0);
    supervisor.set_member_permissions(lead, MemberPermissions::default()).unwrap();
    let replay = supervisor.post_room_message(second).unwrap();
    assert!(replay.replayed && replay.message_id == delivered.message_id, "{replay:?}");
    assert_eq!(lead_inputs.lock().len(), woken, "a replay never wakes again");

    // The same key with other text is still refused.
    let conflict = supervisor.post_room_message(PostRoomMessageRequest { room_id: room, content: "changed".into(),
        request_id: Some("retry-b".into()) }).unwrap_err();
    assert!(conflict.to_string().contains("request_id conflict"), "{conflict:#}");
    assert_eq!((posts_with("retry-a"), posts_with("retry-b")), (1, 1));
    supervisor.shutdown().unwrap();
}
