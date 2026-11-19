// Included in the supervisor test module. v3 W3: pointer wakes, Waiting
// records and ACKs. Mock PTYs and temporary catalogs only; no harness runs.

/// What a member's terminal receives for a pointer wake.
fn typed_pointer(supervisor: &SupervisorHandle, count: usize, newest: Uuid) -> String {
    let room = supervisor.inner.rooms.lock().by_id.iter().find(|(_, room)| room.contains_message(newest))
        .map(|(room_id, _)| *room_id).expect("the newest message is retained in some room");
    format!("{BRACKETED_PASTE_START}{}{BRACKETED_PASTE_END}", room_delivery::room_pointer_text(count, room, newest, None))
}

fn room_events(supervisor: &SupervisorHandle, room_id: RoomId) -> Vec<shared_types::RoomFeedEvent> {
    supervisor.read_room_feed(ReadRoomFeedRequest { room_id, cursor: None }).unwrap().events
}

fn ack_from(supervisor: &SupervisorHandle, caller: &PaneCaller, message_id: Uuid, next: Option<&str>,
    decline: Option<&str>, request_id: &str) -> SidebandResponse {
    supervisor.apply_sideband_request(caller, SidebandRequest::RoomAck { request_id: Some(request_id.into()),
        message_id, next_action: next.map(str::to_owned), decline: decline.map(str::to_owned), outcome_ref: None })
}

/// Two running Claude members in one room, each with its own pane caller.
fn two_member_room(label: &str) -> (SupervisorHandle, RoomId, (SessionId, PaneCaller, Arc<Mutex<Vec<String>>>),
    (SessionId, PaneCaller)) {
    let supervisor = test_supervisor();
    let (pty_a, inputs_a) = recording_pty_session(4101);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude,
        Some(4101), pty_a, BracketedPasteMode::Enabled);
    let (pty_b, _) = recording_pty_session(4102);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "codex", DriverKind::Claude,
        Some(4102), pty_b, BracketedPasteMode::Enabled);
    let a = test_session_id(&supervisor, "claude");
    let b = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some(label.into()),
        member_ids: vec![a, b], brief_on_join: false, brief_template: None }).unwrap();
    let caller_a = supervisor.resolve_pane_caller(TestPaneProcess::new(9101, [4101])).unwrap();
    let caller_b = supervisor.resolve_pane_caller(TestPaneProcess::new(9102, [4102])).unwrap();
    (supervisor, room.room_id, (a, caller_a, inputs_a), (b, caller_b))
}

#[test]
fn v3_an_ordinary_room_message_wakes_its_member_with_a_content_neutral_pointer() {
    let (supervisor, room, (a, _, inputs), _) = two_member_room("Pointer");
    let result = deliver_to(&supervisor, room, a, "secret body words that must not be typed");
    wait_for_room_receipt(&supervisor, room, result.message_id, a, RoomDeliveryStatus::Written);
    let typed = inputs.lock().clone();
    assert_eq!(typed[0], typed_pointer(&supervisor, 1, result.message_id));
    assert!(typed.iter().all(|input| !input.contains("secret body")), "{typed:?}");
    assert!(!typed[0].contains("Pointer") && !typed[0].contains("operator"), "no room or sender labels: {typed:?}");
}

#[test]
fn v3_ten_messages_held_then_ready_become_one_pointer_with_ten_receipts() {
    let supervisor = test_supervisor();
    let (pty, inputs) = recording_pty_session(4201);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude,
        Some(4201), pty, BracketedPasteMode::Unknown);
    let claude = test_session_id(&supervisor, "claude");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Coalesce".into()),
        member_ids: vec![claude], brief_on_join: false, brief_template: None }).unwrap();
    let sent: Vec<RoomDeliveryResult> = (0..10)
        .map(|index| deliver_to(&supervisor, room.room_id, claude, &format!("request {index}"))).collect();
    assert!(sent.iter().all(|result| result.receipts[0].status == RoomDeliveryStatus::Stored));
    assert_eq!(supervisor.queued_room_wake_count_for_tests(claude), 10);

    observe_current_run_paste_mode(&supervisor, claude, "\x1b[?2004h");
    wait_for_no_queued_room_wake(&supervisor, claude);
    let statuses: Vec<_> = sent.iter().map(|result| supervisor.inner.rooms.lock().get(room.room_id).unwrap()
        .retained_delivery(&RoomMessageSender::Operator {}, &result.request_id).unwrap().receipts[0].status).collect();
    assert!(statuses.iter().all(|status| *status == RoomDeliveryStatus::Written),
        "each request keeps its own Written receipt: {statuses:?}");
    let pointer = typed_pointer(&supervisor, 10, sent.last().unwrap().message_id);
    assert_eq!(*inputs.lock(), vec![pointer.clone(), "\r".to_string()], "one pointer for ten distinct requests");
    // Only the head's receipt carries the typed bytes; each covered one names
    // the head's pointer and claims no write of its own (8597d39f).
    let head = sent[0].message_id;
    let deliveries: Vec<_> = room_events(&supervisor, room.room_id).into_iter().filter_map(|event| match event.item {
        RoomFeedItem::Delivery { message_id, status: RoomDeliveryStatus::Written, bytes_written, pointer, .. } =>
            Some((message_id, bytes_written, pointer)),
        _ => None,
    }).collect();
    assert_eq!(deliveries.len(), 10, "{deliveries:?}");
    for (message_id, bytes, pointer_of) in deliveries {
        if message_id == head {
            assert_eq!((bytes, pointer_of), (pointer.len() + 1, None));
        } else {
            assert_eq!((bytes, pointer_of), (0, Some(head)), "{message_id}");
        }
    }
    // Covered requests got their Pending before the write (one per request).
    let pendings = room_receipt_statuses(&supervisor, room.room_id, claude).into_iter()
        .filter(|status| *status == RoomDeliveryStatus::Pending).count();
    assert_eq!(pendings, 10);
}

#[test]
fn v3_a_partially_recorded_covering_set_types_nothing() {
    let supervisor = test_supervisor();
    let (pty, inputs) = recording_pty_session(4301);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude,
        Some(4301), pty, BracketedPasteMode::Unknown);
    let claude = test_session_id(&supervisor, "claude");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Cover fault".into()),
        member_ids: vec![claude], brief_on_join: false, brief_template: None }).unwrap();
    let sent: Vec<RoomDeliveryResult> = (0..3)
        .map(|index| deliver_to(&supervisor, room.room_id, claude, &format!("request {index}"))).collect();
    // The first covered Pending succeeds; the second covered one fails.
    supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().fail_cover_appends_after_for_tests = Some(1);
    observe_current_run_paste_mode(&supervisor, claude, "\x1b[?2004h");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let statuses: Vec<_> = sent.iter().map(|result| supervisor.inner.rooms.lock().get(room.room_id).unwrap()
            .retained_delivery(&RoomMessageSender::Operator {}, &result.request_id).unwrap().receipts[0].clone()).collect();
        if statuses[0].status == RoomDeliveryStatus::Failed { break; }
        assert!(Instant::now() < deadline, "head never failed with a known zero: {statuses:?} typed {:?} log {:?}", inputs.lock(),
            room_events(&supervisor, room.room_id).iter().map(|event| format!("{:?}", event.item)).collect::<Vec<_>>());
        std::thread::sleep(Duration::from_millis(50));
    }
    let status = |result: &RoomDeliveryResult| supervisor.inner.rooms.lock().get(room.room_id).unwrap()
        .retained_delivery(&RoomMessageSender::Operator {}, &result.request_id).unwrap().receipts[0].status;
    assert_eq!(status(&sent[1]), RoomDeliveryStatus::Failed, "a recorded covered request gets the same known-zero outcome");
    // The one request left out of the failed covering set is pointed at on
    // its own, complete, later attempt; nothing was typed for the failed one.
    wait_for_no_queued_room_wake(&supervisor, claude);
    let typed = inputs.lock().clone();
    assert!(typed.is_empty() || typed == vec![typed_pointer(&supervisor, 1, sent[2].message_id), "\r".to_string()],
        "never typed over a partial covering set: {typed:?}");
}

/// Reviewer 80495230 (a): a queued recipient whose transport blocks never
/// stalls another recipient's queued wake.
#[test]
fn v3_a_blocked_transport_for_one_queued_recipient_never_stalls_another() {
    let supervisor = test_supervisor();
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude, Some(4401),
        Box::new(FirstWriteBlockingPtySession { process_id: 4401, inputs: inputs.clone(), calls: calls.clone(),
            first_entered: entered_tx, release: release.clone() }), BracketedPasteMode::Unknown);
    let (other_pty, other_inputs) = recording_pty_session(4402);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "codex", DriverKind::Claude, Some(4402),
        other_pty, BracketedPasteMode::Unknown);
    let blocked = test_session_id(&supervisor, "claude");
    let other = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Isolation".into()),
        member_ids: vec![blocked, other], brief_on_join: false, brief_template: None }).unwrap();
    let to_blocked = deliver_to(&supervisor, room.room_id, blocked, "for the blocked member");
    let to_other = deliver_to(&supervisor, room.room_id, other, "for the other member");
    assert_eq!((to_blocked.receipts[0].status, to_other.receipts[0].status),
        (RoomDeliveryStatus::Stored, RoomDeliveryStatus::Stored));

    observe_current_run_paste_mode(&supervisor, blocked, "\x1b[?2004h");
    entered_rx.recv_timeout(Duration::from_secs(5)).expect("the blocked member's write began");
    observe_current_run_paste_mode(&supervisor, other, "\x1b[?2004h");
    wait_for_room_receipt(&supervisor, room.room_id, to_other.message_id, other, RoomDeliveryStatus::Written);
    assert_eq!(other_inputs.lock()[0], typed_pointer(&supervisor, 1, to_other.message_id));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the blocked write is still in progress");
    { let (released, ready) = &*release; *released.lock() = true; ready.notify_all(); }
    wait_for_room_receipt(&supervisor, room.room_id, to_blocked.message_id, blocked, RoomDeliveryStatus::Written);
}

/// Reviewer 5abde321: a revoked guest's queued request is never covered by
/// another message's pointer, in either order, and is not counted as new work.
#[test]
fn v3_a_revoked_guests_request_is_never_covered_or_counted_in_either_order() {
    for revoked_first in [false, true] {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(4501);
        install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude, Some(4501),
            pty, BracketedPasteMode::Unknown);
        let member = test_session_id(&supervisor, "claude");
        let room = supervisor.create_room(CreateRoomRequest { label: Some("Revoked".into()),
            member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap();
        let leaver = guest_invite(&supervisor, room.room_id, "Leaver").guest.guest_id;
        let (revoked, eligible) = if revoked_first {
            let revoked = supervisor.guest_post(leaver, "from the leaver".into(), None).unwrap();
            (revoked, deliver_to(&supervisor, room.room_id, member, "eligible work"))
        } else {
            let eligible = deliver_to(&supervisor, room.room_id, member, "eligible work");
            (supervisor.guest_post(leaver, "from the leaver".into(), None).unwrap(), eligible)
        };
        supervisor.revoke_room_guest(shared_types::RevokeRoomGuestRequest { guest_id: leaver }).unwrap();
        observe_current_run_paste_mode(&supervisor, member, "\x1b[?2004h");
        wait_for_room_receipt(&supervisor, room.room_id, eligible.message_id, member, RoomDeliveryStatus::Written);
        wait_for_room_receipt(&supervisor, room.room_id, revoked.message_id, member, RoomDeliveryStatus::Held);
        let typed = inputs.lock().clone();
        assert_eq!(typed, vec![typed_pointer(&supervisor, 1, eligible.message_id), "\r".to_string()],
            "one pointer counting only the eligible request (revoked first: {revoked_first})");
        let covered_revoked = room_events(&supervisor, room.room_id).iter().any(|event| matches!(event.item,
            RoomFeedItem::Delivery { message_id, pointer: Some(_), .. } if message_id == revoked.message_id));
        assert!(!covered_revoked, "the revoked request was never covered (revoked first: {revoked_first})");
    }
}

/// Reviewer 371647cb: a request revoked between planning and the admission
/// lock is neither covered nor counted by the pointer typed for the head.
#[test]
fn v3_a_request_revoked_after_planning_is_not_counted_by_the_typed_pointer() {
    let supervisor = test_supervisor();
    let (pty, inputs) = recording_pty_session(4701);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude, Some(4701),
        pty, BracketedPasteMode::Unknown);
    let member = test_session_id(&supervisor, "claude");
    supervisor.hold_room_wake_dispatch_for_tests(member);
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Stale plan".into()),
        member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap();
    let stayer = guest_invite(&supervisor, room.room_id, "Stayer").guest.guest_id;
    let leaver = guest_invite(&supervisor, room.room_id, "Leaver").guest.guest_id;
    let kept = supervisor.guest_post(stayer, "kept request".into(), None).unwrap();
    let revoked = supervisor.guest_post(leaver, "revoked request".into(), None).unwrap();
    observe_current_run_paste_mode(&supervisor, member, "\x1b[?2004h");
    let head = room_events(&supervisor, room.room_id).into_iter().find(|event| matches!(event.item,
        RoomFeedItem::Message { message_id, .. } if message_id == kept.message_id)).unwrap();
    let outcome = supervisor.wake_with_stale_plan_for_tests(&head, member, || {
        supervisor.revoke_room_guest(shared_types::RevokeRoomGuestRequest { guest_id: leaver }).unwrap();
    }).unwrap().expect("the head was attempted");
    assert_eq!(outcome.status, RoomDeliveryStatus::Written);
    assert_eq!(inputs.lock()[0], typed_pointer(&supervisor, 1, kept.message_id),
        "the pointer counts only what was admitted under the lock");
    let covered_revoked = room_events(&supervisor, room.room_id).iter().any(|event| matches!(event.item,
        RoomFeedItem::Delivery { message_id, pointer: Some(_), .. } if message_id == revoked.message_id));
    assert!(!covered_revoked);
}

/// Reviewer 4e12572e: a request planned on its original direct path, then
/// covered by a later request's pointer before that plan runs, admits nothing:
/// no second Pending, no byte. An explicit Retry is still judged by its
/// expected receipt cursor alone.
#[test]
fn v3_a_covered_request_admits_no_second_attempt_on_its_original_direct_path() {
    let supervisor = test_supervisor();
    let (pty, inputs) = recording_pty_session(4702);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude, Some(4702),
        pty, BracketedPasteMode::Unknown);
    let member = test_session_id(&supervisor, "claude");
    supervisor.hold_room_wake_dispatch_for_tests(member);
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Covered direct".into()),
        member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap();
    let post = |request_id: &str, body: &str| {
        let mut rooms = supervisor.inner.rooms.lock();
        let runtime = rooms.get_mut(room.room_id).unwrap();
        let revision = runtime.definition.membership_revision;
        runtime.post_requesting_acks(request_id.into(), RoomMessageSender::Operator {}, body.into(), vec![member], revision)
            .unwrap().0
    };
    let covered = post("covered-b", "request B");
    let RoomFeedItem::Message { message_id: covered_id, .. } = covered.item else { unreachable!() };
    observe_current_run_paste_mode(&supervisor, member, "\x1b[?2004h");
    let mut head_id = None;
    let outcome = supervisor.wake_with_stale_plan_for_tests(&covered, member, || {
        let head = post("head-a", "request A");
        let written = supervisor.wake_with_stale_plan_for_tests(&head, member, || {}).unwrap()
            .expect("the later request was attempted");
        assert_eq!(written.status, RoomDeliveryStatus::Written);
        let RoomFeedItem::Message { message_id, .. } = head.item else { unreachable!() };
        head_id = Some(message_id);
    }).unwrap();
    let head_id = head_id.unwrap();
    assert!(outcome.is_none(), "the covered request's direct plan admits nothing: {outcome:?}");
    let typed = inputs.lock().clone();
    assert_eq!(typed.len(), 2, "one pointer and its submit, typed once: {typed:?}");
    assert_eq!(typed[0], typed_pointer(&supervisor, 2, head_id), "the later pointer counts both");
    let receipts: Vec<_> = room_events(&supervisor, room.room_id).into_iter().filter(|event| matches!(event.item,
        RoomFeedItem::Delivery { message_id, recipient_id, .. } if message_id == covered_id && recipient_id == member))
        .collect();
    assert!(!receipts.is_empty() && receipts.iter().all(|event| matches!(event.item,
        RoomFeedItem::Delivery { pointer: Some(pointer), .. } if pointer == head_id)),
        "every receipt of the covered request is the pointer's cover, none its own attempt: {receipts:?}");
    // Retry: a stale expected cursor changes nothing; the current one is a
    // successful cover and is refused, never typed again.
    let retry = |expected: shared_types::RoomFeedCursor| supervisor.retry_room_delivery(
        shared_types::RetryRoomDeliveryRequest { room_id: room.room_id,
        message_id: covered_id, recipient_id: member, expected_receipt: expected });
    if receipts.len() > 1 {
        let stale = retry(receipts[0].cursor).expect("a stale expected cursor is answered with the stored state");
        assert!(stale.replayed);
    }
    let error = retry(receipts.last().unwrap().cursor).expect_err("a written cover is not retried");
    assert!(error.to_string().contains("cannot be safely retried"), "{error}");
    assert_eq!(inputs.lock().len(), 2, "nothing typed again");
}

/// Reviewer 998e10ed: an ACK stored after planning and before Pending admits
/// nothing — no Pending, no byte.
#[test]
fn v3_an_ack_after_planning_admits_no_wake_for_the_head() {
    let supervisor = test_supervisor();
    let (pty, inputs) = recording_pty_session(4801);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude, Some(4801),
        pty, BracketedPasteMode::Unknown);
    let member = test_session_id(&supervisor, "claude");
    supervisor.hold_room_wake_dispatch_for_tests(member);
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Disposed head".into()),
        member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap();
    let sent = deliver_to(&supervisor, room.room_id, member, "owed then disposed");
    observe_current_run_paste_mode(&supervisor, member, "\x1b[?2004h");
    let head = room_events(&supervisor, room.room_id).into_iter().find(|event| matches!(event.item,
        RoomFeedItem::Message { message_id, .. } if message_id == sent.message_id)).unwrap();
    let outcome = supervisor.wake_with_stale_plan_for_tests(&head, member, || {
        supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().record_ack("op-1".into(), sent.message_id,
            member, shared_types::RoomAckActor::Operator {}, shared_types::RoomAckDisposition::Declined,
            "handled elsewhere".into(), None).unwrap();
    }).unwrap();
    assert!(outcome.is_none(), "nothing admitted: {outcome:?}");
    assert!(inputs.lock().is_empty());
    assert!(!room_events(&supervisor, room.room_id).iter().any(|event| matches!(event.item,
        RoomFeedItem::Delivery { message_id, status: RoomDeliveryStatus::Pending, .. } if message_id == sent.message_id)));
}

#[test]
fn v3_room_pointer_text_is_fixed_wording_with_opaque_ids_only() {
    let room: RoomId = "11111111-2222-3333-4444-555555555555".parse().unwrap();
    let newest: Uuid = "abcdef12-0000-0000-0000-000000000000".parse().unwrap();
    assert_eq!(room_delivery::room_pointer_text(2, room, newest, None),
        "[PRIM-1] 2 new room messages for you in room 11111111-2222-3333-4444-555555555555 (newest abcdef12-0000-0000-0000-000000000000). \
         Run room_read with that room_id, then room_ack each.");
    assert_eq!(room_delivery::room_pointer_text(1, room, newest, None),
        "[PRIM-1] 1 new room message for you in room 11111111-2222-3333-4444-555555555555 (newest abcdef12-0000-0000-0000-000000000000). \
         Run room_read with that room_id, then room_ack each.");
}

#[test]
fn v3_a_request_id_reused_for_another_message_is_a_conflict() {
    let (supervisor, room, (a, caller_a, _), _) = two_member_room("Request ids");
    let first = deliver_to(&supervisor, room, a, "one");
    let second = deliver_to(&supervisor, room, a, "two");
    assert!(ack_from(&supervisor, &caller_a, first.message_id, Some("on it"), None, "same-id").ok);
    let reused = ack_from(&supervisor, &caller_a, second.message_id, Some("on it"), None, "same-id");
    assert!(!reused.ok && reused.message.contains("already names another disposition"), "{}", reused.message);
    assert!(supervisor.inner.rooms.lock().get(room).unwrap().ack_for(second.message_id, a).is_none());
}

#[test]
fn v3_a_member_that_moved_after_its_pointer_cannot_read_or_ack_the_old_room_from_the_new_one() {
    let (supervisor, room, (a, caller_a, _), (b, _)) = two_member_room("Origin");
    let result = deliver_to(&supervisor, room, a, "answer me here");
    wait_for_room_receipt(&supervisor, room, result.message_id, a, RoomDeliveryStatus::Written);
    // The operator moves a to another room before it read or acknowledged.
    supervisor.remove_room_member(RemoveRoomMemberRequest { room_id: room, session_id: a }).unwrap();
    let other = supervisor.create_room(CreateRoomRequest { label: Some("Elsewhere".into()),
        member_ids: vec![a], brief_on_join: false, brief_template: None }).unwrap();
    let caller_a = supervisor.resolve_pane_caller(TestPaneProcess::new(9101, [4101])).unwrap_or(caller_a);

    let read = supervisor.apply_sideband_request(&caller_a, SidebandRequest::RoomRead { owed_after: None, cursor: None, expect_room: Some(room) });
    assert!(!read.ok && read.message.contains("scope unavailable"), "{}", read.message);
    let ack = ack_from(&supervisor, &caller_a, result.message_id, Some("done"), None, "moved-ack");
    assert!(!ack.ok && ack.message.contains("scope unavailable") && ack.message.contains(&room.to_string()), "{}", ack.message);
    // Its obligation stays open in the original room; nothing reached the new one.
    let rooms = supervisor.inner.rooms.lock();
    assert!(rooms.get(room).unwrap().ack_for(result.message_id, a).is_none());
    assert!(!rooms.get(other.room_id).unwrap().contains_message(result.message_id));
    let _ = b;
}

#[test]
fn v3_an_unacknowledged_body_that_left_the_window_comes_back_through_the_members_own_read() {
    let (supervisor, room, (a, _, _), (b, caller_b)) = two_member_room("Window");
    let result = deliver_to(&supervisor, room, b, "still owed to b");
    wait_for_room_receipt(&supervisor, room, result.message_id, b, RoomDeliveryStatus::Written);
    for index in 0..600 {
        supervisor.post_room_message(PostRoomMessageRequest { room_id: room, request_id: None,
            content: format!("churn {index}") }).unwrap();
    }
    let response = supervisor.apply_sideband_request(&caller_b, SidebandRequest::RoomRead { owed_after: None, cursor: None, expect_room: Some(room) });
    assert!(response.ok, "{}", response.message);
    let Some(SidebandResponsePayload::RoomFeed { page }) = response.payload else { panic!("a feed page") };
    assert!(page.events.iter().all(|event| event.cursor.sequence > result.cursor.sequence), "it left the window");
    assert!(page.retained.iter().any(|event| matches!(&event.item,
        RoomFeedItem::Message { message_id, content, .. } if *message_id == result.message_id && content == "still owed to b")),
        "{:?}", page.retained);
    // Reading it records Fetched; it stays owed until b acknowledges.
    let delivered = supervisor.inner.rooms.lock().get(room).unwrap()
        .retained_delivery(&RoomMessageSender::Operator {}, &result.request_id).unwrap();
    assert_eq!(delivered.receipts[0].status, RoomDeliveryStatus::Fetched);
    let _ = a;
}

#[test]
fn v3_a_deferred_wake_is_recorded_as_waiting_at_once_and_ends_nothing() {
    let supervisor = test_supervisor();
    let codex = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Waiting".into()),
        member_ids: vec![codex], brief_on_join: false, brief_template: None }).unwrap();
    let result = deliver_to(&supervisor, room.room_id, codex, "for a stopped member");
    assert_eq!(result.receipts[0].status, RoomDeliveryStatus::Stored);
    assert_eq!(result.receipts[0].waiting.as_deref(), Some("stopped"));
    let waiting: Vec<_> = room_events(&supervisor, room.room_id).into_iter()
        .filter(|event| matches!(&event.item, RoomFeedItem::Waiting { recipient_id, reason_code, .. }
            if *recipient_id == codex && reason_code == "stopped")).collect();
    assert_eq!(waiting.len(), 1);
    assert!(!supervisor.queued_room_wake_pass_for_tests(codex), "still queued: Waiting is no receipt");
    assert_eq!(room_events(&supervisor, room.room_id).iter()
        .filter(|event| matches!(event.item, RoomFeedItem::Waiting { .. })).count(), 1, "an unchanged reason adds nothing");

    // Started: one pointer, and the transport outcome supersedes the waiting code.
    let (pty, _) = recording_pty_session(std::process::id());
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, pty);
    wait_for_room_receipt(&supervisor, room.room_id, result.message_id, codex, RoomDeliveryStatus::Written);
    let delivered = supervisor.inner.rooms.lock().get(room.room_id).unwrap()
        .retained_delivery(&RoomMessageSender::Operator {}, &result.request_id).unwrap();
    assert_eq!(delivered.receipts[0].waiting, None);
}

/// v3 W2 (M1 re-run on 4250194): a Codex whose screen printed a moment ago
/// is not woken on an Idle read; the wake waits with `output_settling` and is
/// typed once the output has been quiet for the settle time.
#[test]
fn v3_a_codex_wake_waits_for_its_output_to_settle() {
    let supervisor = test_supervisor();
    *supervisor.inner.room_wake_output_settle.lock() = Duration::from_millis(600);
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, pty);
    let codex = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Settle".into()),
        member_ids: vec![codex], brief_on_join: false, brief_template: None }).unwrap();
    // The pane just repainted.
    supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().room_composer.observe("\x1b[1;1H");
    let sent = deliver_to(&supervisor, room.room_id, codex, "after the repaint");
    assert_eq!(sent.receipts[0].status, RoomDeliveryStatus::Stored, "not typed during the repaint");
    assert!(inputs.lock().is_empty());
    let waiting = room_events(&supervisor, room.room_id).into_iter().any(|event| matches!(&event.item,
        RoomFeedItem::Waiting { recipient_id, reason_code, .. } if *recipient_id == codex && reason_code == "output_settling"));
    assert!(waiting, "the wait is visible with its reason code");
    wait_for_room_receipt(&supervisor, room.room_id, sent.message_id, codex, RoomDeliveryStatus::Written);
    assert_eq!(inputs.lock()[0], typed_pointer(&supervisor, 1, sent.message_id));
}

/// v3 W2 #2: Codex's own terminal title saying Working makes a wake wait,
/// visibly (`title_busy`), even when its screen reads Idle and its output is
/// quiet; the title turning Ready lets it through. The hint never admits a
/// wake by itself.
#[test]
fn v3_a_codex_wake_waits_while_its_own_title_reads_working() {
    let supervisor = test_supervisor();
    *supervisor.inner.room_wake_output_settle.lock() = Duration::ZERO;
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, pty);
    let codex = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Title".into()),
        member_ids: vec![codex], brief_on_join: false, brief_template: None }).unwrap();
    let title = |supervisor: &SupervisorHandle, text: &str| {
        supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().codex_work_state
            .observe_output(&format!("\x1b]0;{text}\x07"));
    };
    title(&supervisor, "Working | 01a16e26-c76a-75d2-afbc-f78eed578d93");
    let sent = deliver_to(&supervisor, room.room_id, codex, "while the title reads Working");
    assert_eq!(sent.receipts[0].status, RoomDeliveryStatus::Stored);
    thread::sleep(Duration::from_millis(900));
    assert!(inputs.lock().is_empty(), "nothing is typed while the title reads Working");
    assert!(room_events(&supervisor, room.room_id).into_iter().any(|event| matches!(&event.item,
        RoomFeedItem::Waiting { recipient_id, reason_code, reason, .. }
            if *recipient_id == codex && reason_code == "title_busy" && reason.contains("title reads Working"))),
        "the wait is visible with its reason");
    title(&supervisor, "Ready | 01a16e26-c76a-75d2-afbc-f78eed578d93");
    wait_for_room_receipt(&supervisor, room.room_id, sent.message_id, codex, RoomDeliveryStatus::Written);
    assert_eq!(inputs.lock()[0], typed_pointer(&supervisor, 1, sent.message_id));
}

/// v3 W2 #2 (Claude PRIM-1, d9613ce walk): a title that keeps reading
/// Working holds a wake only for the cap; then the wake is typed, as A106
/// types to a working member, and the Written receipt says the title still
/// read Working.
#[test]
fn v3_a_busy_codex_title_holds_a_wake_only_for_its_cap() {
    let supervisor = test_supervisor();
    *supervisor.inner.room_wake_output_settle.lock() = Duration::ZERO;
    *supervisor.inner.room_wake_title_busy_cap.lock() = Duration::from_millis(1200);
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, pty);
    let codex = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Title cap".into()),
        member_ids: vec![codex], brief_on_join: false, brief_template: None }).unwrap();
    supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().codex_work_state
        .observe_output("\x1b]0;Working | t\x07");
    let sent = deliver_to(&supervisor, room.room_id, codex, "during a long turn");
    thread::sleep(Duration::from_millis(500));
    assert!(inputs.lock().is_empty(), "held while under the cap");
    // A repeated busy title keeps the original start: the cap is not reset.
    supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().codex_work_state
        .observe_output("\x1b]0;Working | t\x07");
    wait_for_room_receipt(&supervisor, room.room_id, sent.message_id, codex, RoomDeliveryStatus::Written);
    assert_eq!(inputs.lock()[0], typed_pointer(&supervisor, 1, sent.message_id));
    let written = room_events(&supervisor, room.room_id).into_iter().find_map(|event| match event.item {
        RoomFeedItem::Delivery { message_id, status: RoomDeliveryStatus::Written, title_busy, .. }
            if message_id == sent.message_id => Some(title_busy),
        _ => None }).unwrap();
    assert_eq!(written.as_deref(), Some("Working"), "the receipt says it was typed while the title read Working");
}

/// Supervisor 2ab2f1c5 / Reviewer cfee3ce3: the settle boundary on the same
/// seam. A pane that keeps printing (a busy turn, or chatter while its
/// classifier reads Idle) keeps the message Stored with a visible
/// `waiting{output_settling}` on its receipt; once the output stops it is
/// typed. Nothing is typed while output continues.
#[test]
fn v3_continuous_codex_output_keeps_a_wake_visibly_waiting_until_it_stops() {
    let supervisor = test_supervisor();
    *supervisor.inner.room_wake_output_settle.lock() = Duration::from_millis(400);
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, pty);
    let codex = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Chatter".into()),
        member_ids: vec![codex], brief_on_join: false, brief_template: None }).unwrap();
    let print = |supervisor: &SupervisorHandle| supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap()
        .room_composer.observe("\x1b[1;1H");
    print(&supervisor);
    let sent = deliver_to(&supervisor, room.room_id, codex, "during output");
    // Output every 150 ms (below the 400 ms settle) for 1.5 s: never typed.
    let chatter_until = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < chatter_until {
        print(&supervisor);
        thread::sleep(Duration::from_millis(150));
        assert!(inputs.lock().is_empty(), "nothing is typed while the pane keeps printing");
    }
    // Hoisted: room_events takes the rooms lock itself (Reviewer cb384bf1).
    let event = room_events(&supervisor, room.room_id).into_iter().find(|event| matches!(event.item,
        RoomFeedItem::Message { message_id, .. } if message_id == sent.message_id)).unwrap();
    let receipt = supervisor.inner.rooms.lock().get(room.room_id).unwrap()
        .delivery_result(&event, false).receipts[0].clone();
    assert_eq!((receipt.status, receipt.waiting.as_deref()), (RoomDeliveryStatus::Stored, Some("output_settling")),
        "the owed message is visibly waiting, with its reason");
    wait_for_room_receipt(&supervisor, room.room_id, sent.message_id, codex, RoomDeliveryStatus::Written);
    assert_eq!(inputs.lock()[0], typed_pointer(&supervisor, 1, sent.message_id));
}

/// Reviewer 0324b01f, Supervisor 43891c0e / 9aaf9e8b: output after planning,
/// before the first byte, refuses the write with a confirmed zero; the whole
/// admitted group (head and covered requests) waits visibly and is re-admitted
/// automatically as ONE pointer, every original request kept with its
/// append-only attempt facts (Pending, Failed zero, Waiting, Pending, Written).
#[test]
fn v3_a_transient_zero_write_readmits_the_whole_group_as_one_pointer() {
    let supervisor = test_supervisor();
    // A settle no queuing can outlast, shortened once all three are Stored:
    // the precondition never depends on machine load (PC3 full suite, line 424).
    *supervisor.inner.room_wake_output_settle.lock() = Duration::from_secs(60);
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, pty);
    let codex = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Stale quiet".into()),
        member_ids: vec![codex], brief_on_join: false, brief_template: None }).unwrap();
    // Three requests queue while the pane is printing.
    supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().room_composer.observe("\x1b[1;1H");
    let sent: Vec<_> = (0..3).map(|n| deliver_to(&supervisor, room.room_id, codex, &format!("request {n}"))).collect();
    assert!(sent.iter().all(|result| result.receipts[0].status == RoomDeliveryStatus::Stored));
    // The first admitted attempt sees fresh output just before its first byte.
    let hook_handle = supervisor.clone();
    let fired = Arc::new(AtomicBool::new(false));
    let fired_in_hook = fired.clone();
    supervisor.set_room_wake_before_write_for_tests(move |_| {
        if !fired_in_hook.swap(true, Ordering::SeqCst) {
            hook_handle.inner.slots.lock().get_by_id_mut(codex).unwrap().room_composer.observe("\x1b[1;1H");
        }
    });
    *supervisor.inner.room_wake_output_settle.lock() = Duration::from_millis(300);
    for result in &sent {
        wait_for_room_receipt(&supervisor, room.room_id, result.message_id, codex, RoomDeliveryStatus::Written);
    }
    assert!(fired.load(Ordering::SeqCst), "the transient refusal happened");
    let typed = inputs.lock().clone();
    let events = room_events(&supervisor, room.room_id);
    // Diagnostics only: every receipt and waiting fact in feed order, so a
    // double type shows which attempts reached the terminal.
    let history: Vec<String> = events.iter().filter_map(|event| match &event.item {
        RoomFeedItem::Delivery { message_id, status, bytes_written, zero_write_confirmed, readmit, pointer, .. } =>
            Some(format!("#{} {}:{status:?}:{bytes_written}:{zero_write_confirmed}:readmit={readmit}:ptr={:?}", event.cursor.sequence,
                sent.iter().position(|result| result.message_id == *message_id).unwrap_or(9),
                pointer.as_ref().map(|head| sent.iter().position(|result| result.message_id == *head).unwrap_or(9)))),
        RoomFeedItem::Waiting { message_id, reason_code, .. } => Some(format!("#{} {}:Waiting:{reason_code}",
            event.cursor.sequence, sent.iter().position(|result| result.message_id == *message_id).unwrap_or(9))),
        _ => None,
    }).collect();
    // Codex submits with its own sequence after the paste.
    assert_eq!((typed.len(), typed[0].clone()), (2, typed_pointer(&supervisor, 3, sent[2].message_id)),
        "one pointer for the whole group, typed once: {typed:?}\nfeed: {history:?}");
    for result in &sent {
        let facts: Vec<String> = events.iter().filter_map(|event| match &event.item {
            RoomFeedItem::Delivery { message_id, status, bytes_written, zero_write_confirmed, .. } if *message_id == result.message_id =>
                Some(format!("{status:?}:{bytes_written}:{zero_write_confirmed}")),
            RoomFeedItem::Waiting { message_id, reason_code, .. } if *message_id == result.message_id => Some(format!("Waiting:{reason_code}")),
            _ => None,
        }).collect();
        let failed = facts.iter().position(|fact| fact == "Failed:0:true").expect("a confirmed-zero Failed");
        let waiting_after = facts.iter().skip(failed).position(|fact| fact == "Waiting:output_settling");
        assert!(waiting_after.is_some(), "visible Waiting after the refusal: {facts:?}");
        assert_eq!(facts.last().map(String::as_str).map(|fact| fact.starts_with("Written")), Some(true), "{facts:?}");
    }
}

/// v3 W3 (PC3 full-suite double type at 48a0e1c): a request posted before
/// the pointer is planned, whose own queue entry is not in yet, is counted by
/// the pointer, so it is covered by it too: one pointer is typed, and the
/// late request already has its cover receipt, which makes its own wake moot.
#[test]
fn v3_a_counted_request_without_a_queue_entry_yet_is_covered_and_never_woken_twice() {
    let supervisor = test_supervisor();
    // A long settle window: the second request is posted well before planning.
    *supervisor.inner.room_wake_output_settle.lock() = Duration::from_millis(2000);
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, pty);
    let codex = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Late entry".into()),
        member_ids: vec![codex], brief_on_join: false, brief_template: None }).unwrap();
    supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().room_composer.observe("\x1b[1;1H");
    let first = deliver_to(&supervisor, room.room_id, codex, "request 1");
    assert_eq!(first.receipts[0].status, RoomDeliveryStatus::Stored);
    // The second request is in the feed but has no queue entry (the race window).
    let late = {
        let revision = supervisor.inner.rooms.lock().get(room.room_id).unwrap().definition.membership_revision;
        let (event, _) = supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap()
            .post_requesting_acks("late-entry".into(), RoomMessageSender::Operator {}, "request 2".into(), vec![codex], revision)
            .unwrap();
        let RoomFeedItem::Message { message_id, .. } = event.item else { unreachable!() };
        message_id
    };
    wait_for_room_receipt(&supervisor, room.room_id, first.message_id, codex, RoomDeliveryStatus::Written);
    let typed = inputs.lock().clone();
    assert_eq!((typed.len(), typed[0].clone()), (2, typed_pointer(&supervisor, 2, late)),
        "one pointer counting both, typed once: {typed:?}");
    let covered = room_events(&supervisor, room.room_id).into_iter().any(|event| matches!(event.item,
        RoomFeedItem::Delivery { message_id, pointer: Some(head), .. } if message_id == late && head == first.message_id));
    assert!(covered, "the counted request carries the pointer's cover receipt, so its own wake is moot");
    thread::sleep(Duration::from_millis(2500));
    assert_eq!(inputs.lock().len(), 2, "nothing typed again");
}

/// The production threshold itself (1200 ms), not a test value.
#[test]
fn v3_the_production_codex_settle_is_1200_ms() {
    let supervisor = test_supervisor();
    *supervisor.inner.room_wake_output_settle.lock() = room_delivery::ROOM_WAKE_CODEX_OUTPUT_SETTLE;
    assert_eq!(room_delivery::ROOM_WAKE_CODEX_OUTPUT_SETTLE, Duration::from_millis(1200));
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, pty);
    let codex = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Real settle".into()),
        member_ids: vec![codex], brief_on_join: false, brief_template: None }).unwrap();
    let printed_at = Instant::now();
    supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().room_composer.observe("\x1b[1;1H");
    let sent = deliver_to(&supervisor, room.room_id, codex, "after one repaint");
    thread::sleep(Duration::from_millis(900).saturating_sub(printed_at.elapsed()));
    assert!(inputs.lock().is_empty(), "not typed before 1200 ms of quiet");
    wait_for_room_receipt(&supervisor, room.room_id, sent.message_id, codex, RoomDeliveryStatus::Written);
    assert!(printed_at.elapsed() >= Duration::from_millis(1200), "typed only after the settle");
    assert_eq!(inputs.lock()[0], typed_pointer(&supervisor, 1, sent.message_id));
}

#[test]
fn v3_only_the_addressed_member_can_ack_and_a_second_different_ack_is_refused() {
    let (supervisor, room, (a, caller_a, _), (_b, caller_b)) = two_member_room("Ack");
    let result = deliver_to(&supervisor, room, a, "please review");
    wait_for_room_receipt(&supervisor, room, result.message_id, a, RoomDeliveryStatus::Written);

    let other = ack_from(&supervisor, &caller_b, result.message_id, Some("I'll do it"), None, "ack-b");
    assert!(!other.ok && other.message.contains("not addressed to this member"), "{}", other.message);

    let first = ack_from(&supervisor, &caller_a, result.message_id, Some("reviewing now"), None, "ack-a");
    assert!(first.ok, "{}", first.message);
    let replay = ack_from(&supervisor, &caller_a, result.message_id, Some("reviewing now"), None, "ack-a");
    assert!(replay.ok && replay.message.contains("already stored"), "{}", replay.message);
    let conflict = ack_from(&supervisor, &caller_a, result.message_id, None, Some("changed my mind"), "ack-a2");
    assert!(!conflict.ok && conflict.message.contains("already has a disposition"), "{}", conflict.message);
    let acks = room_events(&supervisor, room).into_iter().filter(|event| matches!(event.item, RoomFeedItem::Ack { .. })).count();
    assert_eq!(acks, 1, "no duplicate ACK");

    let neither = supervisor.apply_sideband_request(&caller_a, SidebandRequest::RoomAck { request_id: None,
        message_id: result.message_id, next_action: None, decline: None, outcome_ref: None });
    assert!(!neither.ok);
}

#[test]
fn v3_an_ack_survives_a_later_transport_failure_and_an_unacked_read_body_stays_retained() {
    let (supervisor, room, (a, caller_a, _), (b, caller_b)) = two_member_room("Retention");
    let result = supervisor.deliver_room_message(DeliverRoomMessageRequest { request_id: None, room_id: room,
        recipients: RoomRecipientSelection::All {}, content: "to both".into() }).unwrap();
    for member in [a, b] {
        wait_for_room_receipt(&supervisor, room, result.message_id, member, RoomDeliveryStatus::Written);
    }
    assert!(ack_from(&supervisor, &caller_a, result.message_id, Some("on it"), None, "ack-1").ok);
    // A late transport receipt for the same recipient must not erase the ACK.
    {
        let mut rooms = supervisor.inner.rooms.lock();
        let runtime = rooms.get_mut(room).unwrap();
        runtime.append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, readmit: false, pointer: None, message_id: result.message_id, recipient_id: a,
            status: RoomDeliveryStatus::Failed, bytes_written: 0, zero_write_confirmed: false,
            error: Some("late".into()), run_id: None, generation: None }).unwrap();
    }
    let delivered = supervisor.inner.rooms.lock().get(room).unwrap()
        .retained_delivery(&RoomMessageSender::Operator {}, &result.request_id).unwrap();
    let mine = delivered.receipts.iter().find(|receipt| receipt.recipient_id == a).unwrap();
    assert_eq!(mine.ack.as_ref().map(|ack| ack.disposition), Some(shared_types::RoomAckDisposition::Acked));

    // b reads but never ACKs: the body outlives far more traffic than the index holds.
    supervisor.read_room_feed_from_pane(&caller_b, None).unwrap();
    for index in 0..1100 {
        supervisor.post_room_message(PostRoomMessageRequest { room_id: room, request_id: None,
            content: format!("churn {index}") }).unwrap();
    }
    assert!(supervisor.inner.rooms.lock().get(room).unwrap()
        .retained_delivery(&RoomMessageSender::Operator {}, &result.request_id).is_some(),
        "an unacknowledged addressed body stays retained");
}

/// v3 W2: a member whose launch program reports a version other than the
/// tested one gets one PRIM-1 notice per run in its room, and the directory
/// shows the version; the tested version, or an unknown one, says nothing.
#[test]
fn v3_a_harness_version_drift_is_said_once_per_run_and_unknown_says_nothing() {
    for observed in [Some("2.1.999"), Some("2.1.310"), None] {
        let supervisor = test_supervisor();
        supervisor.set_executable_resolver_for_tests(Arc::new(TestExecutableResolver));
        *supervisor.inner.harness_version_for_tests.lock() = observed.map(str::to_owned);
        let member = create_test_session(&supervisor, "claude", DriverKind::Claude,
            shared_types::PermissionProfile::Normal).session_id;
        let room = supervisor.create_room(CreateRoomRequest { label: Some("Drift".into()),
            member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap();
        supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![recording_pty_session(std::process::id()).0])));
        supervisor.start_session_by_id_with_mode(member, true).unwrap();
        let run_id = supervisor.inner.slots.lock().get_by_id(member).unwrap().run_id.unwrap();
        thread::sleep(Duration::from_millis(400));
        assert_eq!(supervisor.inner.slots.lock().get_by_id(member).unwrap().harness_version.as_deref(), observed);
        let launch = supervisor.inner.slots.lock().get_by_id(member).unwrap().last_launch.clone().unwrap();
        assert_eq!(launch.effective.harness_version.as_deref(), observed, "the launch record carries the observation");
        let persisted = supervisor.inner.catalog.lock().sessions.iter().find(|row| row.session_id == member).unwrap().last_launch.clone();
        assert_eq!(persisted.as_ref(), Some(&launch), "and the catalog keeps it");
        // Observing the same run again never says it twice.
        supervisor.observe_harness_version(member, run_id,
            harness_executable::ResolvedLaunchProgram { program: "claude.exe".into(), prefix_args: Vec::new() });
        thread::sleep(Duration::from_millis(400));
        let notices: Vec<String> = room_events(&supervisor, room.room_id).into_iter().filter_map(|event| match event.item {
            RoomFeedItem::Message { sender: RoomMessageSender::Prim1 {}, content, recipient_ids, .. }
                if content.contains(" runs Claude Code ") => { assert!(recipient_ids.is_empty(), "feed only"); Some(content) }
            _ => None }).collect();
        match observed {
            Some("2.1.999") => {
                assert_eq!(notices.len(), 1, "{notices:?}");
                assert!(notices[0].contains("runs Claude Code 2.1.999; this PRIM-1 build was tested with 2.1.310"), "{}", notices[0]);
            }
            _ => assert!(notices.is_empty(), "{observed:?}: {notices:?}"),
        }
        supervisor.shutdown().unwrap();
    }
}

/// v3 W5 (Reviewer 60269256): the directory names each member's room
/// operations, wake support and readiness, the PRIM-1 build, and its latest
/// read and ACK as feed positions only; the harness version stays unknown and
/// no path or conversation id is exposed.
#[test]
fn v3_the_room_directory_reports_capabilities_and_event_facts_only() {
    let (supervisor, room, (a, caller_a, _), (b, caller_b)) = two_member_room("Directory");
    let result = supervisor.deliver_room_message(DeliverRoomMessageRequest { request_id: None, room_id: room,
        recipients: RoomRecipientSelection::All {}, content: "to both".into() }).unwrap();
    for member in [a, b] {
        wait_for_room_receipt(&supervisor, room, result.message_id, member, RoomDeliveryStatus::Written);
    }
    supervisor.read_room_feed_from_pane(&caller_b, None).unwrap();
    assert!(ack_from(&supervisor, &caller_a, result.message_id, Some("on it"), None, "ack-directory").ok);
    let response = supervisor.apply_sideband_request(&caller_a, SidebandRequest::RoomDirectory {});
    assert!(response.ok, "{}", response.message);
    let Some(SidebandResponsePayload::RoomDirectory { directory }) = response.payload else { panic!("no directory") };
    assert_eq!(directory.room_id, room);
    assert!(directory.prim1_build.starts_with(env!("CARGO_PKG_VERSION")), "{}", directory.prim1_build);
    let row = |id| directory.members.iter().find(|member| member.session_id == id).unwrap().clone();
    for member in [row(a), row(b)] {
        assert_eq!(member.operations, ["read", "wait", "post", "deliver", "ack"]);
        assert!(member.wake_supported && member.wake_note.is_none());
        assert_eq!(member.ready, member.not_ready.is_none(), "{member:?}");
        assert_eq!(member.harness_version, None, "never claimed");
    }
    assert!(row(a).last_acked.is_some() && row(b).last_acked.is_none());
    assert!(row(b).last_fetched.is_some());
    let text = serde_json::to_string(&directory).unwrap();
    for id in [a, b] {
        let slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id(id).unwrap();
        let folder = serde_json::to_string(&slot.definition.working_dir).unwrap();
        assert!(!text.contains(folder.trim_matches('"')), "no working folder: {text}");
        if let Some(conversation) = &slot.harness_session_id {
            assert!(!text.contains(conversation.as_str()), "no conversation id: {text}");
        }
    }
}

/// Deliveries of `message_id` for `recipient` in feed order:
/// (status, bytes, zero_write_confirmed, readmit, already_read), and Waiting.
fn delivery_facts(supervisor: &SupervisorHandle, room: RoomId, message_id: Uuid, recipient: SessionId) -> Vec<String> {
    room_events(supervisor, room).into_iter().filter_map(|event| match event.item {
        RoomFeedItem::Delivery { message_id: id, recipient_id, status, bytes_written, zero_write_confirmed, readmit,
            already_read, .. } if id == message_id && recipient_id == recipient =>
            Some(format!("{status:?}:{bytes_written}:{zero_write_confirmed}:readmit={readmit}:read={already_read}")),
        RoomFeedItem::Waiting { message_id: id, recipient_id, reason_code, .. } if id == message_id && recipient_id == recipient =>
            Some(format!("Waiting:{reason_code}")),
        _ => None,
    }).collect()
}

/// v3 W3 (Claude PRIM-1 ruling on the 26a7f92 walk, (a)): a pointer whose
/// every counted request the member read after planning and before the first
/// byte is not typed, and "already read" is never a failure: it is not
/// re-admitted, not Retry-eligible, and not a failure in the delivery result.
/// The stored Fetched is kept.
#[test]
fn v3_a_pointer_whose_messages_were_all_read_before_the_paste_is_not_typed_and_never_a_failure() {
    let (supervisor, room, (a, caller_a, inputs), _) = two_member_room("Already read");
    let reader = supervisor.clone();
    let fired = Arc::new(AtomicBool::new(false));
    let fired_in_hook = fired.clone();
    supervisor.set_room_wake_before_write_for_tests(move |write| {
        if write == "paste" && !fired_in_hook.swap(true, Ordering::SeqCst) {
            let read = reader.apply_sideband_request(&caller_a, SidebandRequest::RoomRead {
                owed_after: None, cursor: None, expect_room: Some(room) });
            assert!(read.ok, "{}", read.message);
        }
    });
    let sent = deliver_to(&supervisor, room, a, "read before the paste");
    assert!(fired.load(Ordering::SeqCst), "the read landed between planning and the paste");
    assert!(inputs.lock().is_empty(), "nothing typed: {:?}", inputs.lock());
    let receipt = &sent.receipts[0];
    assert_eq!((receipt.status, receipt.bytes_written, receipt.already_read), (RoomDeliveryStatus::Failed, 0, true), "{receipt:?}");
    assert!(sent.failures.is_empty(), "already read is not a failure: {:?}", sent.failures);
    assert_eq!(delivery_facts(&supervisor, room, sent.message_id, a),
        ["Pending:0:false:readmit=false:read=false", "Fetched:0:false:readmit=false:read=false",
         "Failed:0:false:readmit=false:read=true"], "never re-admitted, and no retry proof after the read");
    // The stored state keeps the read, so the result reports it.
    let stored = supervisor.inner.rooms.lock().get(room).unwrap()
        .retained_delivery(&RoomMessageSender::Operator {}, &sent.request_id).unwrap();
    assert_eq!(stored.receipts[0].status, RoomDeliveryStatus::Fetched);
    assert!(stored.failures.is_empty());
    // No Retry, whichever receipt it is asked against; nothing is ever typed.
    for expected in room_events(&supervisor, room).into_iter().filter(|event| matches!(event.item,
        RoomFeedItem::Delivery { message_id, .. } if message_id == sent.message_id)).map(|event| event.cursor) {
        let retried = supervisor.retry_room_delivery(shared_types::RetryRoomDeliveryRequest {
            room_id: room, message_id: sent.message_id, recipient_id: a, expected_receipt: expected });
        assert!(retried.as_ref().map_or(true, |result| result.receipts.iter().all(|r| r.status != RoomDeliveryStatus::Written)),
            "{retried:?}");
    }
    thread::sleep(Duration::from_millis(900));
    assert!(inputs.lock().is_empty(), "nothing typed later either: {:?}", inputs.lock());
    assert_eq!(supervisor.queued_room_wake_count_for_tests(a), 0);
    assert_eq!(delivery_facts(&supervisor, room, sent.message_id, a).len(), 3, "no later attempt");
}

/// A recording PTY that runs the shared `on_first` once, right after its
/// first write (the paste), before the submit.
struct ReadAfterPastePty {
    process_id: u32,
    inputs: Arc<Mutex<Vec<String>>>,
    on_first: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>,
}

impl PtySessionTrait for ReadAfterPastePty {
    fn send_input(&self, input: &str) -> pty_host::PtyWriteResult {
        self.inputs.lock().push(input.to_string());
        let on_first = self.on_first.lock().take();
        if let Some(on_first) = on_first { on_first(); }
        Ok(input.len())
    }
    fn resize(&self, _cols: u16, _rows: u16) -> Result<()> { Ok(()) }
    fn kill(&self) -> Result<()> { Ok(()) }
    fn try_wait(&self) -> Result<Option<PtyExitStatus>> { Ok(None) }
    fn process_id(&self) -> Option<u32> { Some(self.process_id) }
    fn agent_alive(&self, _driver: DriverKind) -> Result<AgentLiveness> { Ok(AgentLiveness::Alive(Vec::new())) }
}

/// v3 W3 (Claude PRIM-1 ruling, (b); the 26a7f92 J6 chain: the paste, then
/// the echo-settle cap, then the CR after both reads): a typed pointer whose
/// counted messages were all read before its submit says so on its Written,
/// and that is not a failure either.
#[test]
fn v3_a_pointer_submitted_after_its_messages_were_read_says_so_on_its_written() {
    let supervisor = test_supervisor();
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let on_first: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>> = Arc::new(Mutex::new(None));
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude, Some(4301),
        Box::new(ReadAfterPastePty { process_id: 4301, inputs: inputs.clone(), on_first: on_first.clone() }),
        BracketedPasteMode::Enabled);
    let a = test_session_id(&supervisor, "claude");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Read before submit".into()),
        member_ids: vec![a], brief_on_join: false, brief_template: None }).unwrap().room_id;
    let caller = supervisor.resolve_pane_caller(TestPaneProcess::new(9301, [4301])).unwrap();
    let reader = supervisor.clone();
    *on_first.lock() = Some(Box::new(move || {
        let read = reader.apply_sideband_request(&caller, SidebandRequest::RoomRead {
            owed_after: None, cursor: None, expect_room: Some(room) });
        assert!(read.ok, "{}", read.message);
    }));
    let sent = deliver_to(&supervisor, room, a, "read between paste and submit");
    assert!(on_first.lock().is_none(), "the read ran inside the paste");
    assert_eq!(inputs.lock().len(), 2, "paste and submit: {:?}", inputs.lock());
    let receipt = &sent.receipts[0];
    assert_eq!((receipt.status, receipt.already_read), (RoomDeliveryStatus::Written, true), "{receipt:?}");
    assert!(sent.failures.is_empty());
    let facts = delivery_facts(&supervisor, room, sent.message_id, a);
    let last = facts.last().expect("a completion");
    assert!(last.starts_with("Written:") && last.ends_with(":readmit=false:read=true"), "{facts:?}");
    // A pointer typed before any read carries no such note.
    let unread = deliver_to(&supervisor, room, a, "typed before any read");
    assert_eq!((unread.receipts[0].status, unread.receipts[0].already_read), (RoomDeliveryStatus::Written, false));
}

/// v3 (Claude PRIM-1 ef3870d walk #10): a notice PRIM-1 writes itself has
/// sender PRIM-1 for every reader, is feed only, and wakes nobody; its key
/// replays it.
#[test]
fn v3_a_prim1_notice_is_marked_as_prim1s_and_wakes_nobody() {
    let (supervisor, room, (a, _, inputs), _) = two_member_room("Notice");
    let posted = supervisor.post_prim1_notice(room, "[PRIM-1 recovery] Claude J: started fresh.".into(),
        Some("notice-1".into())).unwrap();
    let item = room_events(&supervisor, room).into_iter().find_map(|event| match event.item {
        RoomFeedItem::Message { message_id, sender, recipient_ids, ack_requested, .. }
            if message_id == posted.message_id => Some((sender, recipient_ids, ack_requested)),
        _ => None,
    }).unwrap();
    assert_eq!(item, (RoomMessageSender::Prim1 {}, Vec::new(), false), "PRIM-1's own, never the operator's");
    thread::sleep(Duration::from_millis(500));
    assert!(inputs.lock().is_empty(), "nobody is woken");
    assert!(supervisor.post_prim1_notice(room, "[PRIM-1 recovery] Claude J: started fresh.".into(),
        Some("notice-1".into())).unwrap().replayed);
    let _ = a;
}

/// v3 W3 (D6): the operator withdraws a queued message. The recipient not yet
/// attempted is Held with the reason, its queued wake is dropped and nothing
/// is typed; a Retry is refused; the same request replays and a different
/// second withdrawal is refused; it stops counting in later pointers.
#[test]
fn v3_a_withdrawn_message_is_held_with_its_reason_never_typed_retried_or_counted() {
    let supervisor = test_supervisor();
    *supervisor.inner.room_wake_output_settle.lock() = Duration::from_secs(60);
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, pty);
    let codex = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Withdraw".into()),
        member_ids: vec![codex], brief_on_join: false, brief_template: None }).unwrap();
    supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().room_composer.observe("\x1b[1;1H");
    let sent = deliver_to(&supervisor, room.room_id, codex, "sent to the wrong room");
    assert_eq!(sent.receipts[0].status, RoomDeliveryStatus::Stored);
    assert_eq!(supervisor.queued_room_wake_count_for_tests(codex), 1);

    let cancel = |request_id: &str, reason: &str| supervisor.cancel_room_message(room.room_id, sent.message_id,
        shared_types::RoomAckActor::Operator {}, reason.into(), Some(request_id.into()));
    let withdrawn = cancel("c-1", "wrong room").unwrap();
    assert!(!withdrawn.replayed);
    assert_eq!(withdrawn.held, vec![codex]);
    assert!(withdrawn.reached.is_empty());
    assert_eq!(supervisor.queued_room_wake_count_for_tests(codex), 0, "its queued wake is dropped");
    let facts: Vec<_> = room_events(&supervisor, room.room_id).into_iter().filter_map(|event| match event.item {
        RoomFeedItem::Cancel { message_id, reason, actor, not_typed, reached, .. } if message_id == sent.message_id => {
            assert_eq!((not_typed, reached), (vec![codex], Vec::new()));
            Some(format!("cancel:{reason}:{actor:?}"))
        }
        RoomFeedItem::Delivery { message_id, status, error, .. } if message_id == sent.message_id =>
            Some(format!("{status:?}:{}", error.unwrap_or_default())),
        _ => None,
    }).collect();
    assert_eq!(facts, ["cancel:wrong room:Operator", "Held:withdrawn: the operator cancelled this message (wrong room); it is not typed"]);
    assert!(cancel("c-1", "wrong room").unwrap().replayed, "the same request replays");
    assert!(cancel("c-2", "another reason").unwrap_err().to_string().contains("already withdrawn"));
    let held_cursor = room_events(&supervisor, room.room_id).into_iter().rev().find(|event| matches!(event.item,
        RoomFeedItem::Delivery { message_id, .. } if message_id == sent.message_id)).unwrap().cursor;
    assert!(supervisor.retry_room_delivery(shared_types::RetryRoomDeliveryRequest { room_id: room.room_id,
        message_id: sent.message_id, recipient_id: codex, expected_receipt: held_cursor })
        .unwrap_err().to_string().contains("withdrawn"));

    // Later work is unaffected and the withdrawn message is not counted.
    *supervisor.inner.room_wake_output_settle.lock() = Duration::ZERO;
    let next = deliver_to(&supervisor, room.room_id, codex, "the real request");
    wait_for_room_receipt(&supervisor, room.room_id, next.message_id, codex, RoomDeliveryStatus::Written);
    assert_eq!(inputs.lock()[0], typed_pointer(&supervisor, 1, next.message_id), "1 new, not 2");
    assert!(!inputs.lock().iter().any(|input| input.contains(&sent.message_id.to_string())));
}

/// v3 W3 (D6): a withdrawal that lands after an attempt was planned and before
/// its first byte leaves nothing open for it, so nothing is typed.
#[test]
fn v3_a_withdrawal_before_the_paste_types_nothing() {
    let (supervisor, room, (a, _, inputs), _) = two_member_room("Withdraw in flight");
    let canceller = supervisor.clone();
    let fired = Arc::new(AtomicBool::new(false));
    let fired_in_hook = fired.clone();
    let message = Arc::new(Mutex::new(None::<Uuid>));
    let message_in_hook = message.clone();
    supervisor.set_room_wake_before_write_for_tests(move |write| {
        if write != "paste" || fired_in_hook.swap(true, Ordering::SeqCst) { return; }
        // The message id is the one feed message addressed to a.
        let id = canceller.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor: None }).unwrap().events.into_iter()
            .find_map(|event| match event.item { RoomFeedItem::Message { message_id, .. } => Some(message_id), _ => None }).unwrap();
        *message_in_hook.lock() = Some(id);
        canceller.cancel_room_message(room, id, shared_types::RoomAckActor::Operator {}, "changed my mind".into(), None).unwrap();
    });
    let sent = deliver_to(&supervisor, room, a, "about to be withdrawn");
    assert!(fired.load(Ordering::SeqCst));
    assert_eq!(*message.lock(), Some(sent.message_id));
    assert!(inputs.lock().is_empty(), "nothing typed: {:?}", inputs.lock());
    assert_eq!((sent.receipts[0].status, sent.receipts[0].bytes_written), (RoomDeliveryStatus::Failed, 0));
    assert!(sent.failures.is_empty(), "not a failure");
}


/// v3 W3 (b): from the desktop, the person at the screen disposes of a
/// message for one recipient or withdraws it; both are stored as the
/// operator's (actor Operator), never as the recipient's own ACK.
#[test]
fn v3_the_desktop_records_the_operators_disposition_and_withdrawal_as_the_operators() {
    let (supervisor, room, (a, _, _), (b, _)) = two_member_room("Desktop actions");
    let first = deliver_to(&supervisor, room, a, "for a");
    let result = supervisor.dispose_room_message_as_operator(shared_types::DisposeRoomMessageRequest {
        room_id: room, message_id: first.message_id, recipient: a.to_string(), next_action: None,
        decline: Some("handled in a call".into()), request_id: Some("ui-1".into()) }).unwrap();
    assert!(!result.replayed);
    let stored = supervisor.inner.rooms.lock().get(room).unwrap().ack_for(first.message_id, a).unwrap();
    assert_eq!((stored.actor, stored.disposition, stored.note.as_str()),
        (shared_types::RoomAckActor::Operator {}, shared_types::RoomAckDisposition::Declined, "handled in a call"));
    let second = deliver_to(&supervisor, room, b, "for b");
    let withdrawn = supervisor.cancel_room_message_as_operator(shared_types::CancelRoomMessageRequest {
        room_id: room, message_id: second.message_id, reason: "sent twice".into(), request_id: None }).unwrap();
    let cancel = supervisor.inner.rooms.lock().get(room).unwrap().cancel_for(second.message_id).unwrap();
    assert_eq!((cancel.actor, cancel.reason.as_str()), (shared_types::RoomAckActor::Operator {}, "sent twice"));
    assert!(!withdrawn.replayed);
}

/// v3 W3 (D6, Claude PRIM-1 after 25e7ecf): a withdrawal never pretends to
/// undo a delivery. A recipient already typed to is reported as reached, gets
/// no Held receipt, and reads the Cancel item on its next room_read.
#[test]
fn v3_a_withdrawal_reports_who_it_already_reached_and_they_read_it() {
    let (supervisor, room, (a, caller_a, _), _) = two_member_room("Withdraw reached");
    let sent = deliver_to(&supervisor, room, a, "already typed");
    wait_for_room_receipt(&supervisor, room, sent.message_id, a, RoomDeliveryStatus::Written);
    let withdrawn = supervisor.cancel_room_message(room, sent.message_id, shared_types::RoomAckActor::Operator {},
        "superseded".into(), None).unwrap();
    assert_eq!((withdrawn.held, withdrawn.reached), (Vec::new(), vec![a]));
    let held_for_a = room_events(&supervisor, room).into_iter().any(|event| matches!(event.item,
        RoomFeedItem::Delivery { message_id, status: RoomDeliveryStatus::Held, .. } if message_id == sent.message_id));
    assert!(!held_for_a, "a delivery is never relabelled as not typed");
    let read = supervisor.apply_sideband_request(&caller_a, SidebandRequest::RoomRead { owed_after: None, cursor: None,
        expect_room: Some(room) });
    let Some(SidebandResponsePayload::RoomFeed { page }) = read.payload else { panic!("a page: {}", read.message) };
    assert!(page.events.iter().any(|event| matches!(&event.item,
        RoomFeedItem::Cancel { message_id, reason, reached, .. } if *message_id == sent.message_id && reason == "superseded" && reached == &vec![a])),
        "the member reads the withdrawal and its reason");
}

/// v3 W3 (D6 wake, Claude PRIM-1): the members a withdrawal reached are
/// woken by the content-neutral pointer to a PRIM-1 notice that asks no ACK;
/// its Fetched receipt says who has seen it. A replayed withdrawal posts no
/// second notice.
#[test]
fn v3_a_withdrawal_wakes_the_members_it_reached_with_a_pointer_and_asks_no_ack() {
    let (supervisor, room, (a, caller_a, inputs), _) = two_member_room("Withdraw wake");
    let sent = deliver_to(&supervisor, room, a, "start the migration");
    wait_for_room_receipt(&supervisor, room, sent.message_id, a, RoomDeliveryStatus::Written);
    let withdraw = || supervisor.cancel_room_message(room, sent.message_id, shared_types::RoomAckActor::Operator {},
        "wrong environment".into(), Some("w-1".into())).unwrap();
    let withdrawn = withdraw();
    let notice = withdrawn.notice_message_id.expect("a notice for the reached member");
    wait_for_room_receipt(&supervisor, room, notice, a, RoomDeliveryStatus::Written);
    let typed = inputs.lock().clone();
    assert_eq!(typed.last().unwrap(), "\r");
    assert!(typed.iter().all(|input| !input.contains("wrong environment")), "the reason is never typed: {typed:?}");
    let item = room_events(&supervisor, room).into_iter().find_map(|event| match event.item {
        RoomFeedItem::Message { message_id, sender, recipient_ids, ack_requested, content, .. } if message_id == notice =>
            Some((sender, recipient_ids, ack_requested, content)),
        _ => None,
    }).unwrap();
    assert_eq!((item.0, item.1, item.2), (RoomMessageSender::Prim1 {}, vec![a], false));
    assert!(item.3.contains("withdrew") && item.3.contains("wrong environment") && item.3.contains("no ACK"), "{}", item.3);
    // Seen by: the member's read records Fetched for the notice.
    assert!(supervisor.apply_sideband_request(&caller_a, SidebandRequest::RoomRead { owed_after: None, cursor: None,
        expect_room: Some(room) }).ok);
    wait_for_room_receipt(&supervisor, room, notice, a, RoomDeliveryStatus::Fetched);
    // Never owed: it asks no ACK.
    assert!(supervisor.inner.rooms.lock().get(room).unwrap().unread_for(a).is_empty());
    // A replay finds the same notice and posts nothing new; its response is
    // the recorded one (Reviewer bde87a63).
    let again = withdraw();
    assert!(again.replayed);
    assert_eq!((again.cursor, again.held.clone(), again.reached.clone(), again.notice_message_id),
        (withdrawn.cursor, withdrawn.held.clone(), withdrawn.reached.clone(), Some(notice)));
    let notices = room_events(&supervisor, room).into_iter().filter(|event| matches!(event.item,
        RoomFeedItem::Message { sender: RoomMessageSender::Prim1 {}, .. })).count();
    assert_eq!(notices, 1);
}

/// Reviewer 5571e300: a reached member that has not read its withdrawal
/// notice when the feed window and the post index churn past it still gets
/// the notice body, and its stop instruction, from its own read; its Fetched
/// ends that. Never owed and no ACK asked.
#[test]
fn v3_an_unread_withdrawal_notice_survives_feed_churn_until_its_member_reads_it() {
    let (supervisor, room, (a, caller_a, _), _) = two_member_room("Withdraw retained");
    let sent = deliver_to(&supervisor, room, a, "start the migration");
    wait_for_room_receipt(&supervisor, room, sent.message_id, a, RoomDeliveryStatus::Written);
    let withdrawn = supervisor.cancel_room_message(room, sent.message_id, shared_types::RoomAckActor::Operator {},
        "wrong environment".into(), Some("w-retain".into())).unwrap();
    let notice = withdrawn.notice_message_id.expect("a notice for the reached member");
    wait_for_room_receipt(&supervisor, room, notice, a, RoomDeliveryStatus::Written);
    // Written, so no wake pins it any more; churn past the feed window and the post index.
    for index in 0..1100 {
        supervisor.post_room_message(PostRoomMessageRequest { room_id: room, request_id: None,
            content: format!("churn {index}") }).unwrap();
    }
    assert!(supervisor.inner.rooms.lock().get(room).unwrap().contains_message(notice), "kept while unread");
    let read = || match supervisor.apply_sideband_request(&caller_a, SidebandRequest::RoomRead { owed_after: None,
        cursor: None, expect_room: Some(room) }) {
        response if response.ok => match response.payload {
            Some(SidebandResponsePayload::RoomFeed { page }) => page,
            other => panic!("a feed page: {other:?}"),
        },
        response => panic!("{}", response.message),
    };
    let page = read();
    assert!(!page.events.iter().any(|event| matches!(&event.item,
        RoomFeedItem::Message { message_id, .. } if *message_id == notice)), "it left the window");
    let body = page.retained.iter().find_map(|event| match &event.item {
        RoomFeedItem::Message { message_id, content, sender: RoomMessageSender::Prim1 {}, ack_requested: false, .. }
            if *message_id == notice => Some(content.clone()),
        _ => None,
    }).expect("the notice body comes back through the member's own read");
    assert!(body.contains("withdrew") && body.contains("wrong environment") && body.contains("no ACK"), "{body}");
    // Recorded inside that read; past the churn it sits beyond the feed's first page, so ask the post.
    assert!(!supervisor.inner.rooms.lock().get(room).unwrap().needs_fetched_receipt(notice, a), "the read recorded Fetched");
    // Read now: no longer retained, and never owed.
    assert!(!read().retained.iter().any(|event| matches!(&event.item,
        RoomFeedItem::Message { message_id, .. } if *message_id == notice)), "its Fetched ends the retention");
    assert!(supervisor.inner.rooms.lock().get(room).unwrap().unread_for(a).is_empty());
}

/// Reviewer bde87a63: a crash after the durable Cancel and before its notice.
/// The replay posts the notice it promised, for exactly the recipients the
/// Cancel recorded, once; a further replay returns the same notice.
#[test]
fn v3_a_withdrawal_replay_after_a_crash_before_its_notice_finishes_that_notice_once() {
    let (supervisor, room, (a, _, _), (b, _)) = two_member_room("Withdraw crash");
    let sent = supervisor.deliver_room_message(DeliverRoomMessageRequest { request_id: None, room_id: room,
        recipients: RoomRecipientSelection::All {}, content: "start the migration".into() }).unwrap();
    wait_for_room_receipt(&supervisor, room, sent.message_id, a, RoomDeliveryStatus::Written);
    wait_for_room_receipt(&supervisor, room, sent.message_id, b, RoomDeliveryStatus::Written);
    // The durable Cancel alone, as a crash right after it would leave it.
    let recorded = supervisor.inner.rooms.lock().get_mut(room).unwrap().record_cancel("w-crash".into(), sent.message_id,
        shared_types::RoomAckActor::Operator {}, "wrong environment".into()).unwrap();
    let mut notified = recorded.5.clone();
    notified.sort();
    let mut both = vec![a, b];
    both.sort();
    assert_eq!(notified, both, "both were reached past their attempts");
    let notices = || room_events(&supervisor, room).into_iter().filter_map(|event| match event.item {
        RoomFeedItem::Message { sender: RoomMessageSender::Prim1 {}, message_id, mut recipient_ids, .. } => {
            recipient_ids.sort(); Some((message_id, recipient_ids)) }
        _ => None }).collect::<Vec<_>>();
    assert!(notices().is_empty(), "precondition: no notice yet");
    let withdraw = || supervisor.cancel_room_message(room, sent.message_id, shared_types::RoomAckActor::Operator {},
        "wrong environment".into(), Some("w-crash".into())).unwrap();
    let replayed = withdraw();
    assert!(replayed.replayed);
    let notice = replayed.notice_message_id.expect("the promised notice is finished");
    assert_eq!(notices(), vec![(notice, both.clone())], "for exactly the recorded recipients");
    let again = withdraw();
    assert_eq!(again.notice_message_id, Some(notice), "the same notice");
    assert_eq!(notices().len(), 1, "never a second one");
}

