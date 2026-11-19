// Included in the supervisor test module. v3 W4 S2: a room wake for a Codex
// member on the App Server transport is one turn/start through its observer;
// its receipt comes only from the response to that request's own id
// (9828cc3b, c01d84f8, 1214431f; Reviewer 617c8f1c; supervisors c85f8ae0).

struct AppServerFixture {
    supervisor: SupervisorHandle,
    member: SessionId,
    room: RoomId,
    thread: String,
    inputs: Arc<Mutex<Vec<String>>>,
}

/// A running Codex member bound to `thread`, ready for a typed paste too, so
/// a wake that went the typed way would show in `inputs`.
fn app_server_fixture() -> AppServerFixture {
    let supervisor = test_supervisor();
    let (pty, inputs) = recording_pty_session(4911);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "codex", DriverKind::Codex, Some(4911),
        pty, BracketedPasteMode::Unknown);
    let member = test_session_id(&supervisor, "codex");
    let thread = Uuid::new_v4().to_string();
    {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id_mut(member).unwrap();
        slot.harness_session_id = Some(thread.clone());
        slot.recovery = None;
    }
    *supervisor.inner.submit_confirm_window.lock() = Some(Duration::from_millis(300));
    let room = supervisor.create_room(CreateRoomRequest { label: Some("App Server".into()),
        member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap().room_id;
    observe_current_run_paste_mode(&supervisor, member, "\x1b[?2004h");
    AppServerFixture { supervisor, member, room, thread, inputs }
}

fn install_dispatcher(fixture: &AppServerFixture, dispatcher: codex_app_server::Dispatcher) {
    let mut slots = fixture.supervisor.inner.slots.lock();
    slots.get_by_id_mut(fixture.member).unwrap().running.as_mut().unwrap().app_server = Some(dispatcher);
}

fn answering(outcome: codex_app_server::DispatchOutcome) -> impl FnMut(&str, &str) -> codex_app_server::TestAnswer {
    move |_, _| codex_app_server::TestAnswer::Outcome(outcome.clone())
}

fn delivery_items(supervisor: &SupervisorHandle, room: RoomId, message: Uuid) -> Vec<RoomFeedItem> {
    room_events(supervisor, room).into_iter().map(|event| event.item)
        .filter(|item| matches!(item, RoomFeedItem::Delivery { message_id, .. } if *message_id == message)).collect()
}

fn typed_pointers(fixture: &AppServerFixture) -> usize {
    fixture.inputs.lock().iter().filter(|input| input.contains("[PRIM-1]")).count()
}

#[test]
fn w4_s2_an_accepted_turn_start_is_written_with_its_turn_and_types_nothing() {
    let fixture = app_server_fixture();
    let (dispatcher, sent) = codex_app_server::dispatcher_for_tests(&fixture.thread, || {},
        answering(codex_app_server::DispatchOutcome::Accepted { turn_id: "t-1".into() }));
    install_dispatcher(&fixture, dispatcher);
    let result = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "please read");
    let receipt = &result.receipts[0];
    assert_eq!(receipt.status, RoomDeliveryStatus::Written, "{receipt:?}");
    assert_eq!(receipt.app_server_turn.as_deref(), Some("t-1"));
    assert_eq!(receipt.bytes_written, 0, "nothing was typed; no byte count is claimed");
    assert_eq!(typed_pointers(&fixture), 0, "one route per attempt: never also typed");
    let sent = sent.lock().clone();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains(&result.message_id.to_string()) && sent[0].contains("attempt "),
        "the same pointer text, with its attempt token: {}", sent[0]);
    assert!(delivery_items(&fixture.supervisor, fixture.room, result.message_id).iter().any(|item| matches!(item,
        RoomFeedItem::Delivery { status: RoomDeliveryStatus::Written, app_server_turn: Some(turn), bytes_written: 0, .. }
            if turn == "t-1")), "the durable receipt carries the turn");
}

#[test]
fn w4_s2_a_refused_turn_start_fails_with_its_reason_and_no_zero_write_inference() {
    let fixture = app_server_fixture();
    let (dispatcher, sent) = codex_app_server::dispatcher_for_tests(&fixture.thread, || {},
        answering(codex_app_server::DispatchOutcome::Refused("thread not loaded".into())));
    install_dispatcher(&fixture, dispatcher);
    let result = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "please read");
    let receipt = &result.receipts[0];
    assert_eq!(receipt.status, RoomDeliveryStatus::Failed);
    assert!(receipt.error.as_deref().is_some_and(|error| error.contains("thread not loaded")), "{receipt:?}");
    assert_eq!(receipt.app_server_turn, None, "a server error never reads as accepted");
    assert!(delivery_items(&fixture.supervisor, fixture.room, result.message_id).iter().any(|item| matches!(item,
        RoomFeedItem::Delivery { status: RoomDeliveryStatus::Failed, zero_write_confirmed: false, readmit: false, .. })));
    thread::sleep(Duration::from_millis(1000));
    assert_eq!(sent.lock().len(), 1, "no retry");
    assert_eq!(typed_pointers(&fixture), 0, "and no typed fallback");
}

#[test]
fn w4_s2_an_unanswered_turn_start_is_submit_unknown_and_never_sent_again() {
    let fixture = app_server_fixture();
    let (dispatcher, sent) = codex_app_server::dispatcher_for_tests(&fixture.thread, || {},
        |_, _| codex_app_server::TestAnswer::Silent);
    install_dispatcher(&fixture, dispatcher);
    let result = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "please read");
    let receipt = &result.receipts[0];
    assert_eq!(receipt.status, RoomDeliveryStatus::Failed, "{receipt:?}");
    assert_eq!(receipt.app_server_turn, None, "no answer never reads as accepted");
    wait_for_code(&fixture.supervisor, fixture.room, result.message_id, "submit_unknown");
    assert!(room_events(&fixture.supervisor, fixture.room).iter().any(|event| matches!(&event.item,
        RoomFeedItem::Waiting { message_id, reason_code, attempt: Some(_), .. }
            if *message_id == result.message_id && reason_code == "submit_unknown")), "bound to its attempt");
    thread::sleep(Duration::from_millis(1000));
    assert_eq!(sent.lock().len(), 1, "never replayed");
    assert_eq!(typed_pointers(&fixture), 0, "never typed once it could have been sent");
}

#[test]
fn w4_s2_a_disposal_between_enqueue_and_the_socket_write_sends_nothing() {
    let fixture = app_server_fixture();
    let supervisor = fixture.supervisor.clone();
    let (room, member) = (fixture.room, fixture.member);
    let disposed = Arc::new(Mutex::new(None::<Uuid>));
    let target = disposed.clone();
    let (dispatcher, sent) = codex_app_server::dispatcher_for_tests(&fixture.thread, move || {
        // The operator declines the request for the member after the wake
        // was handed to the observer and before its write.
        let message = room_events(&supervisor, room).into_iter().find_map(|event| match event.item {
            RoomFeedItem::Message { message_id, .. } => Some(message_id), _ => None }).unwrap();
        supervisor.inner.rooms.lock().get_mut(room).unwrap().record_ack("op-s2".into(), message, member,
            shared_types::RoomAckActor::Operator {}, shared_types::RoomAckDisposition::Declined,
            "handled elsewhere".into(), None).unwrap();
        *target.lock() = Some(message);
    }, answering(codex_app_server::DispatchOutcome::Accepted { turn_id: "t-never".into() }));
    install_dispatcher(&fixture, dispatcher);
    let result = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "please read");
    assert_eq!(*disposed.lock(), Some(result.message_id));
    let receipt = &result.receipts[0];
    assert_eq!(receipt.status, RoomDeliveryStatus::Failed, "{receipt:?}");
    assert!(receipt.already_read, "judged at the write, not at enqueue: {receipt:?}");
    assert!(sent.lock().is_empty(), "the check refused before the claim, so nothing was sent");
    assert_eq!(typed_pointers(&fixture), 0);
}

#[test]
fn w4_s2_a_route_gone_before_any_send_is_planned_again_and_typed_through_full_admission() {
    let fixture = app_server_fixture();
    let (dispatcher, sent) = codex_app_server::dispatcher_for_tests(&fixture.thread, || {},
        answering(codex_app_server::DispatchOutcome::Accepted { turn_id: "t-never".into() }));
    let stopper = dispatcher.clone();
    install_dispatcher(&fixture, dispatcher);
    let mut once = Some(());
    fixture.supervisor.set_room_wake_before_write_for_tests(move |write| {
        if write == "app_server" && once.take().is_some() { stopper.stop_for_tests(); }
    });
    let result = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "please read");
    assert_eq!(result.receipts[0].status, RoomDeliveryStatus::Failed, "{:?}", result.receipts[0]);
    wait_for_code(&fixture.supervisor, fixture.room, result.message_id, "route_changed");
    let deadline = Instant::now() + Duration::from_secs(10);
    while typed_pointers(&fixture) == 0 {
        assert!(Instant::now() < deadline, "the re-planned wake was never typed");
        thread::sleep(Duration::from_millis(50));
    }
    assert!(sent.lock().is_empty(), "nothing went to the App Server");
    assert_eq!(typed_pointers(&fixture), 1, "typed once, after planning admitted it again");
    assert!(delivery_items(&fixture.supervisor, fixture.room, result.message_id).iter().any(|item| matches!(item,
        RoomFeedItem::Delivery { status: RoomDeliveryStatus::Failed, zero_write_confirmed: true, readmit: true, .. })));
}

#[test]
fn w4_s2_a_member_without_a_live_route_keeps_the_typed_wake() {
    let fixture = app_server_fixture();
    let (dispatcher, sent) = codex_app_server::dispatcher_for_tests(&fixture.thread, || {},
        answering(codex_app_server::DispatchOutcome::Accepted { turn_id: "t-never".into() }));
    dispatcher.stop_for_tests();
    install_dispatcher(&fixture, dispatcher);
    let result = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "please read");
    assert_eq!(result.receipts[0].status, RoomDeliveryStatus::Written);
    assert_eq!(result.receipts[0].app_server_turn, None);
    assert!(result.receipts[0].bytes_written > 0);
    assert!(sent.lock().is_empty());
    assert_eq!(typed_pointers(&fixture), 1);
}

// v3 W4 S2 (Reviewer 8caec584): the guest sender's grant is rechecked at the
// actual write, not only at enqueue: a revoke while the wake waits for the
// observer sends nothing, and the head's own receipt says why.
#[test]
fn w4_s2_a_guest_revoked_between_enqueue_and_the_socket_write_sends_nothing() {
    let fixture = app_server_fixture();
    let invitation = fixture.supervisor.invite_room_guest(shared_types::InviteRoomGuestRequest {
        room_id: fixture.room, label: Some("Scout".into()) }).unwrap();
    let guest_id = invitation.guest.guest_id;
    let supervisor = fixture.supervisor.clone();
    let (dispatcher, sent) = codex_app_server::dispatcher_for_tests(&fixture.thread, move || {
        supervisor.revoke_room_guest(shared_types::RevokeRoomGuestRequest { guest_id }).unwrap();
    }, answering(codex_app_server::DispatchOutcome::Accepted { turn_id: "t-never".into() }));
    install_dispatcher(&fixture, dispatcher);
    let result = fixture.supervisor.guest_post(guest_id, "from outside".into(), None).unwrap();
    let receipt = &result.receipts[0];
    assert_eq!(receipt.status, RoomDeliveryStatus::Failed, "{receipt:?}");
    assert!(receipt.error.as_deref().is_some_and(|error| error.contains("revoked")), "{receipt:?}");
    assert_eq!(receipt.app_server_turn, None);
    assert!(sent.lock().is_empty(), "refused at the write, before the claim");
    assert_eq!(typed_pointers(&fixture), 0);
}

// v3 W4 S2 (Reviewer 8caec584): a counted request whose guest sender is
// revoked at the write refuses the fixed pointer text; the group is planned
// again without it and sent once, counting only what is still current.
#[test]
fn w4_s2_a_counted_guest_request_revoked_at_the_write_is_planned_again_without_it() {
    let fixture = app_server_fixture();
    let invitation = fixture.supervisor.invite_room_guest(shared_types::InviteRoomGuestRequest {
        room_id: fixture.room, label: Some("Scout".into()) }).unwrap();
    let guest_id = invitation.guest.guest_id;
    let supervisor = fixture.supervisor.clone();
    let mut dispatches = 0;
    let (dispatcher, sent) = codex_app_server::dispatcher_for_tests(&fixture.thread, move || {
        dispatches += 1;
        // The second dispatch is the operator's pointer, which also counts the
        // guest request the member has not read yet.
        if dispatches == 2 {
            supervisor.revoke_room_guest(shared_types::RevokeRoomGuestRequest { guest_id }).unwrap();
        }
    }, answering(codex_app_server::DispatchOutcome::Accepted { turn_id: "t-1".into() }));
    install_dispatcher(&fixture, dispatcher);
    let guest = fixture.supervisor.guest_post(guest_id, "from outside".into(), None).unwrap();
    assert_eq!(guest.receipts[0].app_server_turn.as_deref(), Some("t-1"), "{:?}", guest.receipts[0]);
    let result = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "please read");
    assert_eq!(result.receipts[0].status, RoomDeliveryStatus::Failed, "{:?}", result.receipts[0]);
    assert!(result.receipts[0].error.as_deref().is_some_and(|error| error.contains("stopped being current")),
        "{:?}", result.receipts[0]);
    wait_for_code(&fixture.supervisor, fixture.room, result.message_id, "count_changed");
    let deadline = Instant::now() + Duration::from_secs(10);
    while sent.lock().len() < 2 {
        assert!(Instant::now() < deadline, "the re-planned wake was never sent: {:?}", sent.lock());
        thread::sleep(Duration::from_millis(50));
    }
    let sent = sent.lock().clone();
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(sent[1].starts_with("[PRIM-1] 1 new room message for you") && sent[1].contains(&result.message_id.to_string()),
        "counts only the operator's request: {}", sent[1]);
    assert_eq!(typed_pointers(&fixture), 0);
}

// v3 W4 S2 (Reviewer 8caec584): removing the recipient cannot land while its
// wake is in flight (the room's delivery lease refuses it), so it cannot slip
// between enqueue and the write. The at-write membership check is the
// backstop if that lease ever narrows: modelled here by changing the stored
// membership directly.
#[test]
fn w4_s2_a_recipient_removed_before_the_socket_write_is_never_sent_to() {
    let fixture = app_server_fixture();
    let (supervisor, room, member) = (fixture.supervisor.clone(), fixture.room, fixture.member);
    let mut dispatches = 0;
    let removal_refused = Arc::new(Mutex::new(None::<String>));
    let seen = removal_refused.clone();
    let (dispatcher, sent) = codex_app_server::dispatcher_for_tests(&fixture.thread, move || {
        dispatches += 1;
        if dispatches == 1 {
            let refused = supervisor.remove_room_member(shared_types::RemoveRoomMemberRequest {
                room_id: room, session_id: member }).expect_err("a removal never lands under an in-flight wake");
            *seen.lock() = Some(refused.to_string());
        } else {
            supervisor.inner.rooms.lock().get_mut(room).unwrap().definition.member_ids.retain(|id| *id != member);
        }
    }, answering(codex_app_server::DispatchOutcome::Accepted { turn_id: "t-1".into() }));
    install_dispatcher(&fixture, dispatcher);
    let first = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "first");
    assert!(removal_refused.lock().as_deref().is_some_and(|error| error.contains("delivery in progress")),
        "{:?}", removal_refused.lock());
    assert_eq!(first.receipts[0].app_server_turn.as_deref(), Some("t-1"), "{:?}", first.receipts[0]);
    let second = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "second");
    let receipt = &second.receipts[0];
    assert_eq!(receipt.status, RoomDeliveryStatus::Failed, "{receipt:?}");
    assert!(receipt.error.as_deref().is_some_and(|error| error.contains("removed from this room")), "{receipt:?}");
    assert_eq!(sent.lock().len(), 1, "only the first was sent");
    assert_eq!(typed_pointers(&fixture), 0);
}

/// v3 W4 S2 (Reviewer be2267e4): the member settles the request after the
/// observer claimed the send and before an answer that never comes.
fn settled_while_unanswered(settle: impl Fn(&SupervisorHandle, RoomId, SessionId, Uuid) + Send + 'static) {
    let fixture = app_server_fixture();
    let (supervisor, room, member) = (fixture.supervisor.clone(), fixture.room, fixture.member);
    let (dispatcher, sent) = codex_app_server::dispatcher_for_tests(&fixture.thread, || {}, move |_, _| {
        let message = room_events(&supervisor, room).into_iter().find_map(|event| match event.item {
            RoomFeedItem::Message { message_id, .. } => Some(message_id), _ => None }).unwrap();
        settle(&supervisor, room, member, message);
        codex_app_server::TestAnswer::Silent
    });
    install_dispatcher(&fixture, dispatcher);
    let result = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "please read");
    assert_eq!(result.receipts[0].app_server_turn, None, "no answer never reads as accepted");
    thread::sleep(Duration::from_millis(1000));
    let codes = waiting_codes(&fixture.supervisor, fixture.room, result.message_id);
    assert!(!codes.iter().any(|code| code == "submit_unknown"), "no uncertainty over a settled request: {codes:?}");
    assert_eq!(sent.lock().len(), 1, "never replayed");
    assert_eq!(typed_pointers(&fixture), 0);
}

#[test]
fn w4_s2_an_ack_after_the_send_claim_is_never_followed_by_submit_unknown() {
    settled_while_unanswered(|supervisor, room, member, message| {
        supervisor.inner.rooms.lock().get_mut(room).unwrap().record_ack("op-s2-late".into(), message, member,
            shared_types::RoomAckActor::Operator {}, shared_types::RoomAckDisposition::Declined,
            "handled elsewhere".into(), None).unwrap();
    });
}

#[test]
fn w4_s2_a_fetch_after_the_send_claim_is_never_followed_by_submit_unknown() {
    settled_while_unanswered(|supervisor, room, member, _| {
        let page = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor: None }).unwrap();
        let (run_id, generation) = {
            let slots = supervisor.inner.slots.lock();
            let slot = slots.get_by_id(member).unwrap();
            (slot.run_id.unwrap(), slot.generation)
        };
        supervisor.inner.rooms.lock().get_mut(room).unwrap().record_fetched_page(&page, member, run_id, generation).unwrap();
    });
}

fn person_request(id: u64, thread: &str) -> serde_json::Value {
    serde_json::json!({ "id": id, "method": "item/commandExecution/requestApproval",
        "params": { "threadId": thread, "turnId": "t", "itemId": "i" } })
}

fn person_resolved(id: u64, thread: &str) -> serde_json::Value {
    serde_json::json!({ "method": "serverRequest/resolved", "params": { "threadId": thread, "requestId": id } })
}

fn waiting_reasons(supervisor: &SupervisorHandle, room: RoomId, message: Uuid) -> Vec<String> {
    room_events(supervisor, room).into_iter().filter_map(|event| match event.item {
        RoomFeedItem::Waiting { message_id, reason, .. } if message_id == message => Some(reason), _ => None }).collect()
}

fn wait_for_sent(sent: &Arc<Mutex<Vec<String>>>, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while sent.lock().len() < count {
        assert!(Instant::now() < deadline, "only {} sent: {:?}", sent.lock().len(), sent.lock());
        thread::sleep(Duration::from_millis(50));
    }
}

// v3 W4 S3: while the member's App Server waits on a person, the wake waits
// with that reason visible; nothing is sent or typed, and PRIM-1 answers
// nothing. The request's own resolution releases it, and it is sent once.
#[test]
fn w4_s3_a_wake_waits_while_a_person_request_is_open_and_goes_once_it_is_resolved() {
    let fixture = app_server_fixture();
    let (dispatcher, sent) = codex_app_server::dispatcher_for_tests(&fixture.thread, || {},
        answering(codex_app_server::DispatchOutcome::Accepted { turn_id: "t-1".into() }));
    let owner = dispatcher.clone();
    install_dispatcher(&fixture, dispatcher);
    owner.observe_for_tests(person_request(7, &fixture.thread));
    let result = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "please read");
    assert_ne!(result.receipts[0].status, RoomDeliveryStatus::Written, "{:?}", result.receipts[0]);
    wait_for_code(&fixture.supervisor, fixture.room, result.message_id, "modal");
    assert!(waiting_reasons(&fixture.supervisor, fixture.room, result.message_id).iter().any(|reason|
        reason.contains("asking a person") && reason.contains("item/commandExecution/requestApproval")
            && reason.contains("never answers")), "{:?}", waiting_reasons(&fixture.supervisor, fixture.room, result.message_id));
    thread::sleep(Duration::from_millis(1000));
    assert!(sent.lock().is_empty(), "nothing goes to the App Server while the person is asked");
    assert_eq!(typed_pointers(&fixture), 0, "and nothing is typed over the prompt");
    // Another request's resolution, or the same id on another thread, is not this one's.
    owner.observe_for_tests(person_resolved(8, &fixture.thread));
    owner.observe_for_tests(person_resolved(7, &Uuid::new_v4().to_string()));
    thread::sleep(Duration::from_millis(1000));
    assert!(sent.lock().is_empty());
    owner.observe_for_tests(person_resolved(7, &fixture.thread));
    wait_for_sent(&sent, 1);
    let delivered = || delivery_items(&fixture.supervisor, fixture.room, result.message_id).iter().any(|item| matches!(item,
        RoomFeedItem::Delivery { status: RoomDeliveryStatus::Written, app_server_turn: Some(turn), .. } if turn == "t-1"));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !delivered() {
        assert!(Instant::now() < deadline, "the released wake has no Written receipt");
        thread::sleep(Duration::from_millis(50));
    }
    thread::sleep(Duration::from_millis(500));
    assert_eq!(sent.lock().len(), 1, "sent once");
    assert_eq!(typed_pointers(&fixture), 0);
}

// v3 W4 S3: a person request that opens after planning and before the write
// refuses the wake before anything is sent; it is re-admitted, waits with the
// reason, and goes once the request is resolved.
#[test]
fn w4_s3_a_person_request_opened_before_the_write_holds_the_wake_again() {
    let fixture = app_server_fixture();
    let owner_slot = Arc::new(Mutex::new(None::<codex_app_server::Dispatcher>));
    let opener = owner_slot.clone();
    let thread_id = fixture.thread.clone();
    let mut dispatches = 0;
    let (dispatcher, sent) = codex_app_server::dispatcher_for_tests(&fixture.thread, move || {
        dispatches += 1;
        if dispatches == 1 {
            opener.lock().as_ref().unwrap().observe_for_tests(person_request(3, &thread_id));
        }
    }, answering(codex_app_server::DispatchOutcome::Accepted { turn_id: "t-1".into() }));
    *owner_slot.lock() = Some(dispatcher.clone());
    let owner = dispatcher.clone();
    install_dispatcher(&fixture, dispatcher);
    let result = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "please read");
    let receipt = &result.receipts[0];
    assert_eq!(receipt.status, RoomDeliveryStatus::Failed, "{receipt:?}");
    assert!(receipt.error.as_deref().is_some_and(|error| error.contains("began asking a person")), "{receipt:?}");
    assert!(delivery_items(&fixture.supervisor, fixture.room, result.message_id).iter().any(|item| matches!(item,
        RoomFeedItem::Delivery { status: RoomDeliveryStatus::Failed, zero_write_confirmed: true, readmit: true, .. })));
    wait_for_code(&fixture.supervisor, fixture.room, result.message_id, "modal");
    thread::sleep(Duration::from_millis(1000));
    assert!(sent.lock().is_empty(), "held at planning while the request is open");
    owner.observe_for_tests(person_resolved(3, &fixture.thread));
    wait_for_sent(&sent, 1);
    assert_eq!(typed_pointers(&fixture), 0);
}

// v3 W4 S3 (Reviewer 7b925a34): losing the observer is not a resolution. A
// request seen open stays open for this run, now uncertain, and the wake
// waits on the typed route too, saying so; nothing is sent or typed. A run
// without any request seen keeps the typed wake (no blanket hold; covered by
// w4_s2_a_member_without_a_live_route_keeps_the_typed_wake).
#[test]
fn w4_s3_a_request_seen_open_stays_open_and_uncertain_after_the_observer_stops() {
    let fixture = app_server_fixture();
    let (dispatcher, sent) = codex_app_server::dispatcher_for_tests(&fixture.thread, || {},
        answering(codex_app_server::DispatchOutcome::Accepted { turn_id: "t-never".into() }));
    dispatcher.observe_for_tests(person_request(4, &fixture.thread));
    dispatcher.stop_for_tests();
    let (open, followed) = dispatcher.open_request().expect("still open");
    assert_eq!((open.id, followed), (serde_json::json!(4), false));
    install_dispatcher(&fixture, dispatcher);
    let result = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "please read");
    assert_ne!(result.receipts[0].status, RoomDeliveryStatus::Written, "{:?}", result.receipts[0]);
    wait_for_code(&fixture.supervisor, fixture.room, result.message_id, "modal");
    let reasons = waiting_reasons(&fixture.supervisor, fixture.room, result.message_id);
    assert!(reasons.iter().any(|reason| reason.contains("lost its connection to it") && reason.contains("restarting the member clears it")
        && reason.contains("unknown") && reason.contains("never answers")), "{reasons:?}");
    thread::sleep(Duration::from_millis(1000));
    assert!(sent.lock().is_empty());
    assert_eq!(typed_pointers(&fixture), 0, "nothing typed over a prompt that may still be open");
}

// v3 W4 (Claude PRIM-1 60384b19 (b)): the App Server opt-in is refused at
// creation where the launch's own socket-path check could never pass, with
// that check's reason; no member is created. The ordinary transport is
// unaffected by the runtime folder's length.
#[test]
fn w4_the_app_server_opt_in_is_refused_where_its_socket_path_would_exceed_the_limit() {
    let root = std::env::temp_dir().join(format!("prim1-w4-a-runtime-folder-long-enough-to-exceed-the-limit-{}",
        Uuid::new_v4().simple()));
    let supervisor = empty_test_supervisor_with_root(root);
    let request = |transport| shared_types::CreateSessionRequest { codex_transport: transport, permissions: None,
        launch_choices: None, room_id: None, label: Some("codex".into()), driver: DriverKind::Codex,
        permission_profile: shared_types::PermissionProfile::Normal, linux_working_directory: None };
    let error = supervisor.create_session(request(shared_types::CodexTransport::AppServer))
        .expect_err("refused at creation").to_string();
    assert!(error.contains("cannot be used in this PRIM-1") && error.contains("the limit is 107")
        && error.contains("bytes"), "{error}");
    assert!(supervisor.snapshot().sessions.is_empty(), "nothing was created");
    let audit = fs::read_to_string(supervisor.inner.audit.path()).unwrap();
    assert!(audit.contains("not created: the Codex App Server transport cannot be used in this PRIM-1")
        && audit.contains("the limit is 107"), "the refusal is kept in the audit file: {audit}");
    supervisor.create_session(request(shared_types::CodexTransport::Pty)).expect("the terminal transport still works");
}

// v3 W4 S4: after a reconnect to the same owner, a request seen open before
// the connection was lost stays an uncertain hold, saying restarting clears
// it; its replay makes it an ordinary open request again, and only its own
// resolution releases the wake, which is then sent once.
#[test]
fn w4_s4_a_request_from_before_a_reconnect_holds_as_uncertain_until_replayed_and_resolved() {
    let fixture = app_server_fixture();
    let (dispatcher, sent) = codex_app_server::dispatcher_for_tests(&fixture.thread, || {},
        answering(codex_app_server::DispatchOutcome::Accepted { turn_id: "t-1".into() }));
    let owner = dispatcher.clone();
    install_dispatcher(&fixture, dispatcher);
    owner.observe_for_tests(person_request(9, &fixture.thread));
    owner.connection_lost_for_tests();
    assert_eq!(owner.open_request().map(|(request, followed)| (request.id, followed)), Some((serde_json::json!(9), false)));
    let result = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "please read");
    wait_for_code(&fixture.supervisor, fixture.room, result.message_id, "modal");
    assert!(waiting_reasons(&fixture.supervisor, fixture.room, result.message_id).iter()
        .any(|reason| reason.contains("lost its connection") && reason.contains("restarting the member clears it")));
    owner.observe_for_tests(person_request(9, &fixture.thread));
    assert_eq!(owner.open_request().map(|(_, followed)| followed), Some(true), "replayed: still open, no longer uncertain");
    thread::sleep(Duration::from_millis(1000));
    assert!(sent.lock().is_empty());
    owner.observe_for_tests(person_resolved(9, &fixture.thread));
    wait_for_sent(&sent, 1);
    assert_eq!(typed_pointers(&fixture), 0);
}

/// v3 W4 S5: the member's snapshot says how PRIM-1 follows its App Server,
/// from the run's own dispatcher: waiting for a thread, followed,
/// reconnecting, not followed. Absent without an App Server run.
#[test]
fn w4_s5_the_snapshot_tracks_how_prim1_follows_the_app_server() {
    use shared_types::AppServerFollow;
    let fixture = app_server_fixture();
    let follow = || fixture.supervisor.inner.slots.lock().get_by_id(fixture.member).unwrap().snapshot().app_server;
    assert_eq!(follow(), None, "a terminal-transport run has no App Server state");

    install_dispatcher(&fixture, codex_app_server::Dispatcher::unbound_for_tests());
    assert_eq!(follow(), Some(AppServerFollow::WaitingForThread));

    let (dispatcher, _sent) = codex_app_server::dispatcher_for_tests(&fixture.thread, || {},
        answering(codex_app_server::DispatchOutcome::Accepted { turn_id: "t-1".into() }));
    install_dispatcher(&fixture, dispatcher.clone());
    assert_eq!(follow(), Some(AppServerFollow::Followed));
    dispatcher.reconnecting_for_tests(true);
    assert_eq!(follow(), Some(AppServerFollow::Reconnecting));
    assert!(!dispatcher.serves(&fixture.thread), "no wake goes this way while reconnecting");
    dispatcher.reconnecting_for_tests(false);
    assert_eq!(follow(), Some(AppServerFollow::Followed));
    dispatcher.stop_for_tests();
    assert_eq!(follow(), Some(AppServerFollow::NotFollowed));

    let json = serde_json::to_value(fixture.supervisor.inner.slots.lock().get_by_id(fixture.member).unwrap().snapshot()).unwrap();
    assert_eq!(json["app_server"], "not_followed");

    // Reviewer 2542ee80: a person request open on the App Server is the
    // snapshot's blocked reason (the room's own hold words), uncertain after a
    // lost connection, gone on its exact resolution; a stronger existing
    // reason keeps its place.
    let (dispatcher, _sent) = codex_app_server::dispatcher_for_tests(&fixture.thread, || {},
        answering(codex_app_server::DispatchOutcome::Accepted { turn_id: "t-2".into() }));
    install_dispatcher(&fixture, dispatcher.clone());
    let reason = || fixture.supervisor.inner.slots.lock().get_by_id(fixture.member).unwrap().snapshot().blocked_reason;
    assert_eq!(reason(), None);
    dispatcher.observe_for_tests(person_request(5, &fixture.thread));
    let open = reason().expect("an open person request is a blocked reason");
    assert!(open.contains("asking a person in its terminal") && open.contains("request 5"), "{open}");
    assert_eq!(follow(), Some(AppServerFollow::Followed), "the route fact stays separate from the hold");
    dispatcher.connection_lost_for_tests();
    assert!(reason().unwrap().contains("whether it was answered is unknown"), "{:?}", reason());
    fixture.supervisor.inner.slots.lock().get_by_id_mut(fixture.member).unwrap().provider_error = Some("unauthorized".into());
    assert!(reason().unwrap().contains("provider:unauthorized"), "the stronger existing reason wins: {:?}", reason());
    fixture.supervisor.inner.slots.lock().get_by_id_mut(fixture.member).unwrap().provider_error = None;
    dispatcher.observe_for_tests(person_resolved(5, &fixture.thread));
    assert_eq!(reason(), None, "its exact resolution clears it");
    fixture.supervisor.inner.slots.lock().get_by_id_mut(fixture.member).unwrap().running = None;
    assert_eq!(follow(), None, "a stopped member has none");
}

/// v3 W5 (Reviewer cb415335): the room directory reads the same route-aware
/// admission as a pointer wake: an open person request makes an App Server
/// member not ready on either route, and fresh output does not hold the
/// App Server route as it holds a typed paste.
#[test]
fn w5_the_directory_follows_the_app_server_route_and_its_person_request_hold() {
    let fixture = app_server_fixture();
    let (dispatcher, _sent) = codex_app_server::dispatcher_for_tests(&fixture.thread, || {},
        answering(codex_app_server::DispatchOutcome::Accepted { turn_id: "t-3".into() }));
    install_dispatcher(&fixture, dispatcher.clone());
    let member = || fixture.supervisor.room_directory_for_room(fixture.room).unwrap().members.into_iter()
        .find(|member| member.session_id == fixture.member).unwrap();
    // Output within the settle window: a typed paste would wait for it.
    *fixture.supervisor.inner.room_wake_output_settle.lock() = Duration::from_secs(10);
    fixture.supervisor.inner.slots.lock().get_by_id_mut(fixture.member).unwrap().room_composer.observe("working…");
    let followed = member();
    assert!(followed.ready, "the App Server route does not wait for the screen: {:?}", followed.not_ready);

    dispatcher.observe_for_tests(person_request(6, &fixture.thread));
    let held = member();
    assert!(!held.ready);
    assert!(held.not_ready.as_deref().is_some_and(|reason| reason.contains("asking a person in its terminal")
        && reason.contains("request 6")), "{:?}", held.not_ready);
    dispatcher.observe_for_tests(person_resolved(6, &fixture.thread));
    assert!(member().ready);

    // Without a followed route the typed paste's screen wait applies again.
    dispatcher.stop_for_tests();
    let typed = member();
    assert!(!typed.ready);
    assert_eq!(typed.not_ready.as_deref(), Some("the member's screen is still changing; a wake waits until it settles"));
}
