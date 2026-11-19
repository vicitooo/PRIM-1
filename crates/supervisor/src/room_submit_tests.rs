// Included in the supervisor test module. v3 W3: a Written room wake is not a
// submitted one; its submission is confirmed from the member's own record
// (Supervisors 1086baa2, 50fb64e7, 0d81f25d; Reviewers be7a79e7, c49038b0).

struct SubmitFixture {
    supervisor: SupervisorHandle,
    member: SessionId,
    room: RoomId,
    transcript: std::path::PathBuf,
    inputs: Arc<Mutex<Vec<String>>>,
    _home: tempfile::TempDir,
}

/// A Claude member whose bound transcript exists (empty), with a short
/// confirmation window.
fn submit_fixture(bound: bool) -> SubmitFixture {
    submit_fixture_ready(bound, true)
}

fn submit_fixture_ready(bound: bool, ready: bool) -> SubmitFixture {
    let supervisor = test_supervisor();
    let (pty, inputs) = recording_pty_session(4901);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude, Some(4901),
        pty, BracketedPasteMode::Unknown);
    let member = test_session_id(&supervisor, "claude");
    let home = tempfile::tempdir().unwrap();
    supervisor.inner.identity_tracker.set_claude_home(Some(home.path().to_path_buf()));
    let conversation = Uuid::new_v4().to_string();
    let working_dir = {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id_mut(member).unwrap();
        slot.harness_session_id = bound.then(|| conversation.clone());
        slot.recovery = None;
        slot.definition.working_dir.clone()
    };
    let folder: String = working_dir.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let transcript = home.path().join("projects").join(folder).join(format!("{conversation}.jsonl"));
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(&transcript, "").unwrap();
    *supervisor.inner.submit_confirm_window.lock() = Some(Duration::from_millis(300));
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Submit".into()),
        member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap().room_id;
    if ready {
        observe_current_run_paste_mode(&supervisor, member, "\x1b[?2004h");
    }
    SubmitFixture { supervisor, member, room, transcript, inputs, _home: home }
}

fn waiting_codes(supervisor: &SupervisorHandle, room: RoomId, message: Uuid) -> Vec<String> {
    room_events(supervisor, room).into_iter().filter_map(|event| match event.item {
        RoomFeedItem::Waiting { message_id, reason_code, .. } if message_id == message => Some(reason_code),
        _ => None,
    }).collect()
}

fn wait_for_code(supervisor: &SupervisorHandle, room: RoomId, message: Uuid, code: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !waiting_codes(supervisor, room, message).iter().any(|seen| seen == code) {
        assert!(Instant::now() < deadline, "no {code}: {:?}", waiting_codes(supervisor, room, message));
        thread::sleep(Duration::from_millis(50));
    }
}

fn append_user_record(path: &std::path::Path, text: &str) {
    let record = serde_json::json!({"type": "user", "timestamp": Utc::now().to_rfc3339(),
        "message": {"role": "user", "content": text}});
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    std::io::Write::write_all(&mut file, format!("{record}\n").as_bytes()).unwrap();
}

#[test]
fn v3_a_written_wake_without_a_matching_record_is_unconfirmed_and_never_retyped() {
    let fixture = submit_fixture(true);
    let sent = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "please read");
    assert_eq!(sent.receipts[0].status, RoomDeliveryStatus::Written);
    wait_for_code(&fixture.supervisor, fixture.room, sent.message_id, "submit_unconfirmed");
    thread::sleep(Duration::from_millis(1500));
    let pointers = fixture.inputs.lock().iter().filter(|input| input.contains("[PRIM-1]")).count();
    assert_eq!(pointers, 1, "the absence of a record never types the pointer again");
}

#[test]
fn v3_an_exact_user_record_confirms_the_submission_as_input_observed() {
    let fixture = submit_fixture(true);
    let sent = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "please read");
    let typed = fixture.inputs.lock()[0].clone();
    let pointer = typed.trim_start_matches(BRACKETED_PASTE_START).trim_end_matches(BRACKETED_PASTE_END).to_string();
    assert!(pointer.contains(&sent.message_id.to_string()), "the pointer carries the full newest id: {pointer}");
    append_user_record(&fixture.transcript, &pointer);
    thread::sleep(Duration::from_millis(2500));
    assert!(waiting_codes(&fixture.supervisor, fixture.room, sent.message_id).is_empty(), "confirmed: no record");
    let observed = room_events(&fixture.supervisor, fixture.room).into_iter().filter(|event| matches!(&event.item,
        RoomFeedItem::InputObserved { message_id, recipient_id, source, .. }
            if *message_id == sent.message_id && *recipient_id == fixture.member && source == "claude_transcript")).count();
    assert_eq!(observed, 1, "W3b: the positive match is durable, attempt-bound, never an ACK");
    assert!(!room_events(&fixture.supervisor, fixture.room).iter().any(|event| matches!(event.item, RoomFeedItem::Ack { .. })));
}

fn observed_count(supervisor: &SupervisorHandle, room: RoomId) -> usize {
    room_events(supervisor, room).iter().filter(|event| matches!(event.item, RoomFeedItem::InputObserved { .. })).count()
}

/// W3b (Reviewers 2efa5499, 02655ed3): a matching record that appears after
/// the window is reconciled later, from the attempt's own recipe: a record in
/// another conversation, or one carrying another attempt's token, never counts.
#[test]
fn v3_a_late_matching_record_is_reconciled_against_its_attempt() {
    let fixture = submit_fixture(true);
    let sent = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "please read");
    let typed = fixture.inputs.lock()[0].clone();
    let pointer = typed.trim_start_matches(BRACKETED_PASTE_START).trim_end_matches(BRACKETED_PASTE_END).to_string();
    assert!(pointer.contains(", attempt "), "the pointer carries its attempt token: {pointer}");
    wait_for_code(&fixture.supervisor, fixture.room, sent.message_id, "submit_unconfirmed");
    fixture.supervisor.reconcile_submissions_once_for_tests();
    assert_eq!(observed_count(&fixture.supervisor, fixture.room), 0, "nothing to reconcile yet");

    // Case 2: the same pointer with another attempt's token never matches.
    let (head, tail) = pointer.split_once(", attempt ").unwrap();
    let (epoch, rest) = tail.split_once(':').unwrap();
    assert_eq!(epoch.len(), 36, "the token carries the feed epoch: {pointer}");
    let other_attempt = format!("{head}, attempt {epoch}:999999{}", &rest[rest.find(')').unwrap()..]);
    append_user_record(&fixture.transcript, &other_attempt);
    // Case 1: the member moves to another conversation whose record holds the
    // exact pointer; the recipe still names the attempted conversation.
    let other_conversation = Uuid::new_v4().to_string();
    let other_transcript = fixture.transcript.with_file_name(format!("{other_conversation}.jsonl"));
    fs::write(&other_transcript, "").unwrap();
    append_user_record(&other_transcript, &pointer);
    fixture.supervisor.inner.slots.lock().get_by_id_mut(fixture.member).unwrap().harness_session_id = Some(other_conversation);
    fixture.supervisor.reconcile_submissions_once_for_tests();
    assert_eq!(observed_count(&fixture.supervisor, fixture.room), 0, "neither a foreign token nor another conversation counts");

    append_user_record(&fixture.transcript, &pointer);
    fixture.supervisor.reconcile_submissions_once_for_tests();
    let observed: Vec<_> = room_events(&fixture.supervisor, fixture.room).into_iter().filter_map(|event| match event.item {
        RoomFeedItem::InputObserved { message_id, locator, .. } if message_id == sent.message_id => Some(locator), _ => None,
    }).collect();
    assert_eq!(observed.len(), 1, "reconciled once");
    assert!(observed[0].starts_with(fixture.transcript.file_name().unwrap().to_str().unwrap()), "exact locator: {}", observed[0]);
    // Hoisted: room_events takes the rooms lock itself (Reviewer 4a72b640).
    let message = room_events(&fixture.supervisor, fixture.room).into_iter().find(|event| matches!(event.item,
        RoomFeedItem::Message { message_id, .. } if message_id == sent.message_id)).unwrap();
    let waiting = fixture.supervisor.inner.rooms.lock().get(fixture.room).unwrap()
        .delivery_result(&message, false).receipts[0].waiting.clone();
    assert_eq!(waiting, None, "the uncertainty of that attempt is cleared");
    fixture.supervisor.reconcile_submissions_once_for_tests();
    assert_eq!(observed_count(&fixture.supervisor, fixture.room), 1, "idempotent: nothing open, nothing appended");
}

/// W3b (Reviewer 7f071b01): a recipe that cannot be stored types nothing;
/// the head gets a known-zero Failed, as for an incomplete covering set.
#[test]
fn v3_a_recipe_that_cannot_be_stored_types_nothing() {
    let fixture = submit_fixture_ready(true, false);
    let sent = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "please read");
    fixture.supervisor.inner.rooms.lock().get_mut(fixture.room).unwrap().fail_submit_attempt_for_tests = true;
    observe_current_run_paste_mode(&fixture.supervisor, fixture.member, "\x1b[?2004h");
    let status = || fixture.supervisor.inner.rooms.lock().get(fixture.room).unwrap()
        .retained_delivery(&RoomMessageSender::Operator {}, &sent.request_id).unwrap().receipts[0].clone();
    let deadline = Instant::now() + Duration::from_secs(10);
    while status().status != RoomDeliveryStatus::Failed {
        assert!(Instant::now() < deadline, "no known-zero Failed: {:?}", status());
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(status().bytes_written, 0);
    assert!(fixture.inputs.lock().is_empty(), "nothing typed: {:?}", fixture.inputs.lock());
    assert!(!room_events(&fixture.supervisor, fixture.room).iter().any(|event| matches!(event.item, RoomFeedItem::SubmitAttempt { .. })));
}

/// W3b (Claude PRIM-1, eaa0739 walk #85/#87): the early confirmation and the
/// reconciler both see the same record; the attempt is observed exactly once.
#[test]
fn v3_both_confirmation_paths_record_one_input_observed_per_attempt() {
    let fixture = submit_fixture(true);
    let sent = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "please read");
    let typed = fixture.inputs.lock()[0].clone();
    let pointer = typed.trim_start_matches(BRACKETED_PASTE_START).trim_end_matches(BRACKETED_PASTE_END).to_string();
    append_user_record(&fixture.transcript, &pointer);
    // The reconciler records it first...
    fixture.supervisor.reconcile_submissions_once_for_tests();
    // ...then the early worker, still inside its window, finds the same record.
    thread::sleep(Duration::from_millis(1500));
    fixture.supervisor.reconcile_submissions_once_for_tests();
    let observed = room_events(&fixture.supervisor, fixture.room).iter().filter(|event| matches!(&event.item,
        RoomFeedItem::InputObserved { message_id, .. } if *message_id == sent.message_id)).count();
    assert_eq!(observed, 1, "one InputObserved per message, recipient and attempt");
}

#[test]
fn v3_an_unbound_conversation_is_submit_unknown() {
    let fixture = submit_fixture(false);
    let sent = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "please read");
    wait_for_code(&fixture.supervisor, fixture.room, sent.message_id, "submit_unknown");
}

/// Reviewer c49038b0 / ecc822cc: two requests under ONE coalesced pointer;
/// reading one settles only that one, the other stays uncertain.
#[test]
fn v3_a_read_settles_only_its_own_request_never_the_whole_pointer() {
    let fixture = submit_fixture_ready(true, false);
    fixture.supervisor.hold_room_wake_dispatch_for_tests(fixture.member);
    let first = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "first");
    let second = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "second");
    assert!([&first, &second].iter().all(|sent| sent.receipts[0].status == RoomDeliveryStatus::Stored), "both queued");
    observe_current_run_paste_mode(&fixture.supervisor, fixture.member, "\x1b[?2004h");
    assert!(fixture.supervisor.queued_room_wake_pass_for_tests(fixture.member), "the head's pointer was attempted");
    let covered = room_events(&fixture.supervisor, fixture.room).iter().any(|event| matches!(event.item,
        RoomFeedItem::Delivery { message_id, pointer: Some(_), status: RoomDeliveryStatus::Written, .. } if message_id == second.message_id));
    assert!(covered, "one pointer covers both");
    fixture.supervisor.inner.rooms.lock().get_mut(fixture.room).unwrap().append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, pointer: None,
        message_id: first.message_id, recipient_id: fixture.member, status: RoomDeliveryStatus::Fetched, bytes_written: 0,
        zero_write_confirmed: false, error: None, run_id: None, generation: None, readmit: false }).unwrap();
    wait_for_code(&fixture.supervisor, fixture.room, second.message_id, "submit_unconfirmed");
    let first_codes = waiting_codes(&fixture.supervisor, fixture.room, first.message_id);
    assert!(!first_codes.iter().any(|code| code.starts_with("submit_")), "the read request is settled: {first_codes:?}");
}

/// Supervisors 2af36863 / 585dfa48: no extra CR on an unconfirmed
/// submission, whatever the input row shows.
#[test]
fn v3_an_unconfirmed_submission_never_gets_an_extra_cr() {
    let fixture = submit_fixture(true);
    let sent = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "please read");
    wait_for_code(&fixture.supervisor, fixture.room, sent.message_id, "submit_unconfirmed");
    thread::sleep(Duration::from_millis(1500));
    let inputs = fixture.inputs.lock().clone();
    assert_eq!(inputs.len(), 2, "one paste and the wake's own CR, nothing after: {inputs:?}");
}


/// Reviewer 621948fa / supervisor a5d87af3: an old monitor cannot publish a
/// current-looking uncertainty for a newer recorded attempt/run.
#[test]
fn v3_review_old_submit_monitor_cannot_mark_a_newer_attempt_uncertain() {
    let fixture = submit_fixture(true);
    *fixture.supervisor.inner.submit_confirm_window.lock() = Some(Duration::from_millis(1500));
    let sent = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "one request");
    assert_eq!(sent.receipts[0].status, RoomDeliveryStatus::Written);
    let newer_run = Uuid::new_v4();
    {
        let mut slots = fixture.supervisor.inner.slots.lock();
        slots.get_by_id_mut(fixture.member).unwrap().run_id = Some(newer_run);
    }
    // Use the existing authoritative store to represent a later attempt; no
    // extra transport, model turn or retry of the ambiguous first write.
    fixture.supervisor.inner.rooms.lock().get_mut(fixture.room).unwrap().append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false,
        pointer: None, message_id: sent.message_id, recipient_id: fixture.member,
        status: RoomDeliveryStatus::Pending, bytes_written: 0, zero_write_confirmed: false,
        error: None, run_id: Some(newer_run), generation: Some(2), readmit: false,
    }).unwrap();
    thread::sleep(Duration::from_millis(2700));
    let codes = waiting_codes(&fixture.supervisor, fixture.room, sent.message_id);
    assert!(!codes.iter().any(|code| matches!(code.as_str(), "submit_unconfirmed" | "submit_unknown")),
        "old confirmation must not annotate the newer attempt: {codes:?}");
}

/// Reviewer71834de8: cursor order alone does not bind a later receipt to
/// an older recipe. Model a crash after a newer Pending but before its recipe.
#[test]
fn v3_review_old_recipe_cannot_confirm_a_newer_pending_attempt() {
    let fixture = submit_fixture(true);
    let sent = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "one request");
    wait_for_code(&fixture.supervisor, fixture.room, sent.message_id, "submit_unconfirmed");
    let typed = fixture.inputs.lock()[0].clone();
    let old_pointer = typed.trim_start_matches(BRACKETED_PASTE_START)
        .trim_end_matches(BRACKETED_PASTE_END).to_string();
    let newer_run = Uuid::new_v4();
    fixture.supervisor.inner.slots.lock().get_by_id_mut(fixture.member).unwrap().run_id = Some(newer_run);
    // An authoritative later attempt record, not a replay through transport.
    fixture.supervisor.inner.rooms.lock().get_mut(fixture.room).unwrap().append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false,
        pointer: None, message_id: sent.message_id, recipient_id: fixture.member,
        status: RoomDeliveryStatus::Pending, bytes_written: 0, zero_write_confirmed: false,
        error: None, run_id: Some(newer_run), generation: Some(2), readmit: false,
    }).unwrap();
    append_user_record(&fixture.transcript, &old_pointer);
    fixture.supervisor.reconcile_submissions_once_for_tests();
    assert_eq!(observed_count(&fixture.supervisor, fixture.room), 0,
        "old input may be historical evidence, never current observation for a newer recipe-less attempt");
    assert_eq!(fixture.inputs.lock().len(), 2, "no replay or extra Enter");
}

/// W3b (Reviewer 68568692): the early worker reads no match; the reconciler
/// then records this attempt's InputObserved; only then does the early worker
/// take the append lock. It appends no uncertainty and nothing is typed again.
#[test]
fn v3_an_observation_between_the_early_read_and_its_append_settles_the_attempt() {
    let fixture = submit_fixture(true);
    let (entered_tx, entered_rx) = mpsc::sync_channel(0);
    let (release_tx, release_rx) = mpsc::sync_channel::<()>(0);
    *fixture.supervisor.inner.submit_waiting_before_append.lock() = Some(Box::new(move || {
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    }));
    let sent = deliver_to(&fixture.supervisor, fixture.room, fixture.member, "please read");
    assert_eq!(sent.receipts[0].status, RoomDeliveryStatus::Written);
    entered_rx.recv_timeout(Duration::from_secs(10)).expect("the early worker read no match");
    let typed = fixture.inputs.lock()[0].clone();
    let pointer = typed.trim_start_matches(BRACKETED_PASTE_START).trim_end_matches(BRACKETED_PASTE_END).to_string();
    append_user_record(&fixture.transcript, &pointer);
    fixture.supervisor.reconcile_submissions_once_for_tests();
    assert_eq!(observed_count(&fixture.supervisor, fixture.room), 1, "the reconciler observed the attempt first");
    release_tx.send(()).unwrap();
    thread::sleep(Duration::from_millis(1000));
    assert!(waiting_codes(&fixture.supervisor, fixture.room, sent.message_id).is_empty(),
        "no uncertainty after the observation: {:?}", waiting_codes(&fixture.supervisor, fixture.room, sent.message_id));
    let message = room_events(&fixture.supervisor, fixture.room).into_iter().find(|event| matches!(event.item,
        RoomFeedItem::Message { message_id, .. } if message_id == sent.message_id)).unwrap();
    assert_eq!(fixture.supervisor.inner.rooms.lock().get(fixture.room).unwrap()
        .delivery_result(&message, false).receipts[0].waiting, None);
    fixture.supervisor.reconcile_submissions_once_for_tests();
    assert_eq!(observed_count(&fixture.supervisor, fixture.room), 1, "no second observation");
    let pointers = fixture.inputs.lock().iter().filter(|input| input.contains("[PRIM-1]")).count();
    assert_eq!(pointers, 1, "no retry");
}
