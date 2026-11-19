// Included in the supervisor test module. A106 room delivery: the whole room
// message is typed like the live PRIM-1, and a member that cannot take it yet
// keeps it queued. Mock PTYs and temporary catalogs only; no harness runs.

/// What a Claude, Codex or Grok member receives for one room message.
fn typed_room_message(sender: &str, body: &str) -> String {
    format!("{BRACKETED_PASTE_START}[Room message from {sender}]\n{body}{BRACKETED_PASTE_END}")
}

/// Bounded wait for one recipient's receipt of one message, read from the feed.
fn wait_for_room_receipt(supervisor: &SupervisorHandle, room_id: RoomId, message: Uuid,
    recipient: SessionId, wanted: RoomDeliveryStatus) -> shared_types::RoomFeedEvent {
    let mut found = None;
    wait_until(&format!("a {wanted:?} receipt for {recipient}"), || {
        let page = supervisor.read_room_feed(ReadRoomFeedRequest { room_id, cursor: None }).unwrap();
        found = page.events.into_iter().find(|event| matches!(event.item, RoomFeedItem::Delivery {
            message_id, recipient_id, status, .. } if message_id == message && recipient_id == recipient && status == wanted));
        found.is_some()
    });
    found.unwrap()
}

/// The dispatcher removes an entry just after its outcome receipt is appended.
fn wait_for_no_queued_room_wake(supervisor: &SupervisorHandle, recipient: SessionId) {
    wait_until("the dispatcher finishes the member's queue", || supervisor.queued_room_wake_count_for_tests(recipient) == 0);
}

fn room_receipt_statuses(supervisor: &SupervisorHandle, room_id: RoomId, recipient: SessionId) -> Vec<RoomDeliveryStatus> {
    supervisor.read_room_feed(ReadRoomFeedRequest { room_id, cursor: None }).unwrap().events.iter()
        .filter_map(|event| match event.item {
            RoomFeedItem::Delivery { recipient_id, status, .. } if recipient_id == recipient => Some(status),
            _ => None,
        }).collect()
}

/// The member's terminal reports its paste mode, as its output would.
fn observe_current_run_paste_mode(supervisor: &SupervisorHandle, session_id: SessionId, control: &str) {
    let mut slots = supervisor.inner.slots.lock();
    let slot = slots.get_by_id_mut(session_id).unwrap();
    let binding = RunBinding { session_id, run_id: slot.run_id.unwrap(), generation: slot.generation };
    slot.bracketed_paste.observe_output(binding, control);
}

fn deliver_to(supervisor: &SupervisorHandle, room_id: RoomId, recipient: SessionId, body: &str) -> RoomDeliveryResult {
    supervisor.deliver_room_message(DeliverRoomMessageRequest { request_id: None, room_id,
        recipients: RoomRecipientSelection::One { session_id: recipient }, content: body.into() }).unwrap()
}

#[test]
fn a_stopped_member_keeps_its_room_message_stored_and_receives_it_once_started() {
    let supervisor = test_supervisor();
    let codex = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Stopped member".into()),
        member_ids: vec![codex], brief_on_join: false, brief_template: None }).unwrap();
    let body = "build is green\nplease review the diff";

    let result = deliver_to(&supervisor, room.room_id, codex, body);

    assert_eq!(result.receipts[0].status, RoomDeliveryStatus::Stored);
    assert_eq!(result.receipts[0].error, None);
    // A16 (T-30): the delivery's own receipt says why it waits, and the audit
    // log keeps it once; an unchanged recheck adds nothing.
    assert_eq!(result.receipts[0].queued_because.as_deref(),
        Some("the member is not running; the message waits until it starts"));
    let waits = |supervisor: &SupervisorHandle| fs::read_to_string(supervisor.audit_log_path()).unwrap_or_default()
        .lines().filter(|line| line.contains("room wake queued") || line.contains("room wake still waiting")).count();
    assert_eq!(waits(&supervisor), 1);
    let audited = fs::read_to_string(supervisor.audit_log_path()).unwrap();
    let line = audited.lines().find(|line| line.contains("room wake queued")).unwrap();
    assert!(line.contains(&result.message_id.to_string()) && line.contains("not running"), "{line}");
    assert!(!line.contains("please review the diff"), "never the message's text: {line}");
    assert!(!supervisor.queued_room_wake_pass_for_tests(codex), "still waiting");
    assert_eq!(waits(&supervisor), 1, "an unchanged recheck writes nothing");
    assert_eq!(result.written_count, 0);
    assert!(result.failures.is_empty(), "a waiting message is not a failure");
    assert_eq!(supervisor.queued_room_wake_count_for_tests(codex), 1);
    assert!(room_receipt_statuses(&supervisor, room.room_id, codex).is_empty(),
        "no Held or other receipt is appended while the member is stopped");

    // The member starts: its run is up, its prompt observed, paste mode on.
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, pty);
    let written = wait_for_room_receipt(&supervisor, room.room_id, result.message_id, codex, RoomDeliveryStatus::Written);

    // v3 W3: the pointer (never the body), then Codex's arrow-fenced submit.
    let typed = typed_pointer(&supervisor, 1, result.message_id);
    assert_eq!(*inputs.lock(), vec![typed.clone(), "\x1b[C\r".to_string()],
        "the pointer, then Codex's arrow-fenced submit");
    let RoomFeedItem::Delivery { bytes_written, run_id, .. } = written.item else { unreachable!() };
    assert_eq!(bytes_written, typed.len() + "\x1b[C\r".len());
    assert_eq!(run_id, Some(current_run_id(&supervisor, "codex")));
    wait_for_no_queued_room_wake(&supervisor, codex);
    assert_eq!(room_receipt_statuses(&supervisor, room.room_id, codex),
        vec![RoomDeliveryStatus::Pending, RoomDeliveryStatus::Written]);
}

#[test]
fn a_queued_room_message_the_member_already_read_is_never_typed() {
    let supervisor = test_supervisor();
    let pid = std::process::id();
    let (pty, inputs) = recording_pty_session(pid);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude,
        Some(pid), pty, BracketedPasteMode::Unknown);
    let claude = test_session_id(&supervisor, "claude");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Read first".into()),
        member_ids: vec![claude], brief_on_join: false, brief_template: None }).unwrap();
    let result = deliver_to(&supervisor, room.room_id, claude, "read it yourself");
    assert_eq!(result.receipts[0].status, RoomDeliveryStatus::Stored);
    assert_eq!(supervisor.queued_room_wake_count_for_tests(claude), 1);

    let caller = supervisor.resolve_pane_caller(TestPaneProcess::new(9001, [pid])).unwrap();
    let page = supervisor.read_room_feed_from_pane(&caller, None).unwrap();
    assert!(page.events.iter().any(|event| matches!(&event.item,
        RoomFeedItem::Message { content, .. } if content == "read it yourself")));
    // The member could now take a paste; the dispatcher must drop the entry instead.
    observe_current_run_paste_mode(&supervisor, claude, "\x1b[?2004h");
    wait_for_no_queued_room_wake(&supervisor, claude);

    assert!(inputs.lock().is_empty(), "a message the member already read is never typed");
    assert_eq!(room_receipt_statuses(&supervisor, room.room_id, claude), vec![RoomDeliveryStatus::Fetched]);
}

#[test]
fn two_room_messages_queued_for_one_member_are_typed_in_order() {
    let supervisor = test_supervisor();
    let codex = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Order".into()),
        member_ids: vec![codex], brief_on_join: false, brief_template: None }).unwrap();
    let first = deliver_to(&supervisor, room.room_id, codex, "first while stopped");
    let second = deliver_to(&supervisor, room.room_id, codex, "second while stopped");
    assert_eq!(first.receipts[0].status, RoomDeliveryStatus::Stored);
    assert_eq!(second.receipts[0].status, RoomDeliveryStatus::Stored);
    assert_eq!(supervisor.queued_room_wake_count_for_tests(codex), 2);

    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, pty);
    wait_for_room_receipt(&supervisor, room.room_id, second.message_id, codex, RoomDeliveryStatus::Written);
    wait_for_room_receipt(&supervisor, room.room_id, first.message_id, codex, RoomDeliveryStatus::Written);

    // v3 W3: one pointer for both queued messages, naming the newest; each
    // message still ends with its own Written receipt.
    assert_eq!(*inputs.lock(), vec![typed_pointer(&supervisor, 2, second.message_id), "\x1b[C\r".to_string()]);
    wait_for_no_queued_room_wake(&supervisor, codex);
    // Order is kept where the member reads them: room_read returns both, oldest first.
    let bodies: Vec<String> = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None })
        .unwrap().events.into_iter().filter_map(|event| match event.item {
            RoomFeedItem::Message { content, .. } => Some(content), _ => None }).collect();
    assert_eq!(bodies, vec!["first while stopped", "second while stopped"]);
}

#[test]
fn a_new_room_message_never_overtakes_messages_already_waiting_for_the_member() {
    let supervisor = test_supervisor();
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude,
        Some(std::process::id()), pty, BracketedPasteMode::Unknown);
    let claude = test_session_id(&supervisor, "claude");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("No overtaking".into()),
        member_ids: vec![claude], brief_on_join: false, brief_template: None }).unwrap();
    let waiting = deliver_to(&supervisor, room.room_id, claude, "sent while the paste mode was unknown");
    assert_eq!(waiting.receipts[0].status, RoomDeliveryStatus::Stored);

    // The member becomes ready and a new message arrives before the
    // dispatcher's next poll: it must queue behind the waiting one.
    observe_current_run_paste_mode(&supervisor, claude, "\x1b[?2004h");
    let later = deliver_to(&supervisor, room.room_id, claude, "sent once it was ready");
    wait_for_room_receipt(&supervisor, room.room_id, later.message_id, claude, RoomDeliveryStatus::Written);
    wait_for_room_receipt(&supervisor, room.room_id, waiting.message_id, claude, RoomDeliveryStatus::Written);

    // v3 W3: the later message queues behind the waiting one and is covered
    // by its pointer, which counts both. If the dispatcher happened to type
    // the waiting message before the later one arrived, that first pointer
    // names only the waiting message; no pointer ever names the later
    // message without counting the waiting one before it.
    let typed = inputs.lock().clone();
    let one_pointer = vec![typed_pointer(&supervisor, 2, later.message_id), "\r".to_string()];
    let two_pointers = vec![typed_pointer(&supervisor, 1, waiting.message_id), "\r".to_string(),
        typed_pointer(&supervisor, 2, later.message_id), "\r".to_string()];
    assert!(typed == one_pointer || typed == two_pointers,
        "the member is pointed at its messages in the order they were sent: {typed:?}");
}

#[test]
fn resume_here_releases_only_a_copied_conversation_no_other_session_here_uses() {
    let supervisor = test_supervisor();
    let copied = test_session_id(&supervisor, "claude");
    let other = test_session_id(&supervisor, "codex");
    let conversation = Uuid::new_v4().to_string();
    let hold = session::RecoveryReference { original_id: Some(conversation.clone()),
        provenance: "room_import_in_use".into(), reason: "Copied from the live PRIM-1.".into(), reported_codex: None };
    {
        let mut slots = supervisor.inner.slots.lock();
        slots.get_by_id_mut(copied).unwrap().recovery = Some(hold.clone());
        let owner = slots.get_by_id_mut(other).unwrap();
        owner.definition.driver = DriverKind::Claude;
        owner.harness_session_id = Some(conversation.clone());
    }
    let refused = supervisor.resume_held_conversation(copied).unwrap_err().to_string();
    assert!(refused.contains("also uses this conversation"), "{refused}");
    assert_eq!(supervisor.inner.slots.lock().get_by_id(copied).unwrap().recovery, Some(hold.clone()));

    // Two held copies of one conversation: neither may release.
    supervisor.inner.slots.lock().get_by_id_mut(other).unwrap().harness_session_id = None;
    supervisor.inner.slots.lock().get_by_id_mut(other).unwrap().recovery = Some(hold.clone());
    assert!(supervisor.resume_held_conversation(copied).is_err());

    // Once it is the only reference here, the operator's Resume here releases it.
    supervisor.inner.slots.lock().get_by_id_mut(other).unwrap().recovery = None;
    let released = supervisor.resume_held_conversation(copied).unwrap();
    assert!(released.recovery.is_none());
    let slots = supervisor.inner.slots.lock();
    let slot = slots.get_by_id(copied).unwrap();
    assert_eq!(slot.harness_session_id.as_deref(), Some(conversation.as_str()));
    assert!(slot.recovery_history.contains(&hold));
    drop(slots);
    let row = supervisor.inner.catalog.lock().sessions.iter().find(|row| row.session_id == copied).cloned().unwrap();
    assert_eq!((row.harness_session_id.as_deref(), row.recovery.is_none()), (Some(conversation.as_str()), true));
}

#[test]
fn prime_and_terminal_members_stay_held_and_are_never_queued() {
    for driver in [DriverKind::Prime, DriverKind::GenericTerminal] {
        for running in [true, false] {
            let supervisor = test_supervisor();
            let (pty, inputs) = recording_pty_session(std::process::id());
            let member = test_session_id(&supervisor, "claude");
            if running {
                install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "claude", driver, pty);
            } else {
                supervisor.inner.slots.lock().get_by_id_mut(member).unwrap().definition.driver = driver;
            }
            let room = supervisor.create_room(CreateRoomRequest { label: Some("No adapter".into()),
                member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap();

            let result = deliver_to(&supervisor, room.room_id, member, "context for a human-driven pane");

            assert_eq!(result.receipts[0].status, RoomDeliveryStatus::Held, "{driver:?} running={running}");
            assert!(result.failures[0].error.contains("no verified room-wake adapter"));
            assert_eq!(supervisor.queued_room_wake_count_for_tests(member), 0, "{driver:?} is never queued");
            assert_eq!(room_receipt_statuses(&supervisor, room.room_id, member), vec![RoomDeliveryStatus::Held]);
            assert!(inputs.lock().is_empty());
        }
    }
}

#[test]
fn a_thinking_claude_member_is_typed_into_at_once_like_the_live_prim1() {
    for work_state in [WorkState::Thinking, WorkState::ToolCall] {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(std::process::id());
        install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "claude", DriverKind::Claude, pty);
        let claude = test_session_id(&supervisor, "claude");
        {
            let mut slots = supervisor.inner.slots.lock();
            let slot = slots.get_by_id_mut(claude).unwrap();
            slot.state = LifecycleState::Busy;
            slot.work_state = work_state;
            slot.work_state_observed = true;
        }
        let room = supervisor.create_room(CreateRoomRequest { label: Some("Busy member".into()),
            member_ids: vec![claude], brief_on_join: false, brief_template: None }).unwrap();

        let result = deliver_to(&supervisor, room.room_id, claude, "queue this behind your turn");

        assert_eq!(result.receipts[0].status, RoomDeliveryStatus::Written, "{work_state:?}");
        assert_eq!(result.written_count, 1);
        assert_eq!(*inputs.lock(), vec![typed_pointer(&supervisor, 1, result.message_id), "\r".to_string()]);
        assert_eq!(supervisor.queued_room_wake_count_for_tests(claude), 0, "typed at once, never queued");
    }
}

#[test]
fn a_dropped_supervisor_ends_its_room_message_dispatcher_and_releases_room_history() {
    let root = tempfile::tempdir().unwrap();
    let supervisor = test_supervisor_with_root(root.path().to_path_buf());
    let codex = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Restart in place".into()),
        member_ids: vec![codex], brief_on_join: false, brief_template: None }).unwrap();
    let waiting = deliver_to(&supervisor, room.room_id, codex, "still waiting at restart");
    assert_eq!(waiting.receipts[0].status, RoomDeliveryStatus::Stored);
    assert_eq!(supervisor.queued_room_wake_count_for_tests(codex), 1);

    let inner = Arc::downgrade(&supervisor.inner);
    drop(supervisor);
    // Like every other background worker, the dispatcher must not keep a
    // dropped supervisor (its room writers and PTYs) alive for 12 hours.
    wait_until("the room message dispatcher releases the dropped supervisor", || inner.strong_count() == 0);

    let reopened = SupervisorHandle::new(test_supervisor_config(root.path())).unwrap();
    let page = reopened.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
    assert!(page.events.iter().any(|event| matches!(&event.item,
        RoomFeedItem::Message { content, .. } if content == "still waiting at restart")));
}

#[test]
fn a16_a_started_harness_is_ready_only_after_visible_output_and_a_silent_one_stalls() {
    // PC3, 20 Oct: a Codex whose node never ran read "ready" for five minutes.
    let supervisor = test_supervisor();
    let codex = test_session_id(&supervisor, "codex");
    let claude = test_session_id(&supervisor, "claude");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Silent start".into()),
        member_ids: vec![codex], brief_on_join: false, brief_template: None }).unwrap();
    let drawing = create_test_session(&supervisor, "Drawing", DriverKind::Claude, shared_types::PermissionProfile::Normal).session_id;
    let events = capture_runtime_events(&supervisor);
    supervisor.set_startup_silence_for_tests(Duration::from_millis(300));
    let silent = |count: usize| (0..count).map(|_| mock_pty_session(Some(std::process::id()), MockKillBehavior::Immediate).0).collect();
    supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::silent(silent(2))));

    let started = supervisor.start_session_by_id(codex).unwrap();
    assert_eq!(started.lifecycle_state, LifecycleState::Starting, "a process alone is not ready");
    // The pseudoconsole's own setup, written before its program runs, is not a sign of life.
    handle_current_pty_event(&supervisor, "codex", started.generation,
        PtyEvent::Output("\x1b[?9001h\x1b[?1004h\x1b]0;node.exe\x07\x1b[2J\x1b[H".into()));
    assert_eq!(session_now(&supervisor, codex).lifecycle_state, LifecycleState::Starting);
    let brief = deliver_to(&supervisor, room.room_id, codex, "the brief");
    assert_eq!(brief.receipts[0].status, RoomDeliveryStatus::Stored);
    assert_eq!(brief.receipts[0].queued_because.as_deref(), Some("the member is still starting"));

    wait_until("the silent run is Stalled", || session_now(&supervisor, codex).lifecycle_state == LifecycleState::Stalled);
    assert!(events.lock().iter().any(|event| matches!(event, RuntimeEvent::SessionState { state: LifecycleState::Stalled, reason, .. }
        if reason.contains("no visible output"))));
    // The dispatcher's waiting log is asynchronous (one worker thread per
    // recipient since v3 W3): it follows Stalled, not necessarily at once.
    wait_until("the queued brief names the stall", || events.lock().iter().any(|event| matches!(event,
        RuntimeEvent::SystemLog { message, .. }
        if message.contains("codex has shown no output") && message.contains("stop it and start it again"))));
    assert!(!supervisor.queued_room_wake_pass_for_tests(codex), "nothing is typed into a stalled member");
    assert!(room_receipt_statuses(&supervisor, room.room_id, codex).is_empty());
    // Visible output later makes it Ready.
    handle_current_pty_event(&supervisor, "codex", started.generation, PtyEvent::Output("OpenAI Codex".into()));
    assert_eq!(session_now(&supervisor, codex).lifecycle_state, LifecycleState::Ready);

    // A run that draws within the window is Ready and never stalls.
    let other = supervisor.start_session_by_id(claude).unwrap();
    assert_eq!(other.lifecycle_state, LifecycleState::Starting);
    handle_current_pty_event(&supervisor, "claude", other.generation, PtyEvent::Output("Welcome to Claude Code".into()));
    assert_eq!(session_now(&supervisor, claude).lifecycle_state, LifecycleState::Ready);
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(session_now(&supervisor, claude).lifecycle_state, LifecycleState::Ready);
    assert!(!events.lock().iter().any(|event| matches!(event, RuntimeEvent::SystemLog { message, .. }
        if message.contains("claude has shown no output"))));

    // A first screen drawn while the spawn is still being confirmed counts.
    queue_mock_starts(&supervisor, 1);
    assert_eq!(supervisor.start_session_by_id(drawing).unwrap().lifecycle_state, LifecycleState::Ready);
}

// The Reviewer's independent repros of the D1 HOLD on 6709800, kept as
// regressions: a split title is not text, one-character reads are, Grok keeps
// its measured startup, and an old watchdog never touches another run.
fn reviewer_d1_silent(driver: DriverKind, count: usize) -> (SupervisorHandle, SessionId) {
    let supervisor = test_supervisor();
    supervisor.set_startup_silence_for_tests(Duration::from_secs(3600));
    let id = match driver {
        DriverKind::Claude => test_session_id(&supervisor, "claude"),
        DriverKind::Codex => test_session_id(&supervisor, "codex"),
        _ => create_test_session(&supervisor, "Reviewer silent", driver,
            shared_types::PermissionProfile::Normal).session_id,
    };
    let sessions = (0..count)
        .map(|_| mock_pty_session(Some(std::process::id()), MockKillBehavior::Immediate).0)
        .collect();
    supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::silent(sessions)));
    (supervisor, id)
}

#[test]
fn reviewer_d1_split_title_control_is_not_visible_output() {
    let (s, id) = reviewer_d1_silent(DriverKind::Codex, 1);
    let started = s.start_session_by_id(id).unwrap();
    for chunk in ["\x1b]0;", "node.exe\x07"] {
        handle_current_pty_event(&s, "codex", started.generation, PtyEvent::Output(chunk.into()));
    }
    let actual = session_now(&s, id).lifecycle_state;
    s.shutdown().unwrap();
    assert_eq!(actual, LifecycleState::Starting,
        "splitting an OSC title across valid PTY reads must not manufacture visible text");
}

#[test]
fn reviewer_d1_one_character_chunks_still_draw_visible_output() {
    let (s, id) = reviewer_d1_silent(DriverKind::Codex, 1);
    let started = s.start_session_by_id(id).unwrap();
    for ch in "OpenAI".chars() {
        handle_current_pty_event(&s, "codex", started.generation, PtyEvent::Output(ch.to_string()));
    }
    let actual = session_now(&s, id).lifecycle_state;
    s.shutdown().unwrap();
    assert_eq!(actual, LifecycleState::Ready,
        "PTY read boundaries do not change whether the harness drew its first screen");
}

#[test]
fn reviewer_d1_silent_grok_keeps_measured_interactive_startup() {
    let (s, id) = reviewer_d1_silent(DriverKind::Grok, 1);
    let started = s.start_session_by_id(id).unwrap();
    assert_eq!(started.lifecycle_state, LifecycleState::Starting);
    let run = s.inner.slots.lock().get_by_id(id).unwrap().run_id.unwrap();
    s.fire_startup_watchdog(id, started.generation, run, Duration::from_secs(90));
    s.handle_pty_event_by_id(id, started.generation, run, PtyEvent::Output("not an interactive screen".into()));
    let actual = session_now(&s, id).lifecycle_state;
    assert!(!s.inner.slots.lock().get_by_id(id).unwrap().grok_startup.is_interactive_ready());
    s.shutdown().unwrap();
    assert_ne!(actual, LifecycleState::Ready,
        "D1 is Claude/Codex-only; a silent Grok must not bypass its measured startup after the watchdog");
}

#[test]
fn reviewer_d1_old_watchdog_does_not_change_a_replacement_run() {
    let (s, id) = reviewer_d1_silent(DriverKind::Claude, 2);
    let old = s.start_session_by_id(id).unwrap();
    let old_run = s.inner.slots.lock().get_by_id(id).unwrap().run_id.unwrap();
    s.stop_session_by_id(id).unwrap();
    let next = s.start_session_by_id(id).unwrap();
    let next_run = s.inner.slots.lock().get_by_id(id).unwrap().run_id.unwrap();
    assert_ne!(old_run, next_run);
    s.fire_startup_watchdog(id, old.generation, old_run, Duration::from_secs(90));
    assert_eq!(session_now(&s, id).lifecycle_state, LifecycleState::Starting);
    s.fire_startup_watchdog(id, next.generation, Uuid::new_v4(), Duration::from_secs(90));
    assert_eq!(session_now(&s, id).lifecycle_state, LifecycleState::Starting);
    s.fire_startup_watchdog(id, next.generation, next_run, Duration::from_secs(90));
    assert_eq!(session_now(&s, id).lifecycle_state, LifecycleState::Stalled);
    s.shutdown().unwrap();
}

#[test]
fn reviewer_d1_watchdog_does_not_resurrect_a_retired_run() {
    let (s, id) = reviewer_d1_silent(DriverKind::Codex, 1);
    let started = s.start_session_by_id(id).unwrap();
    let run = s.inner.slots.lock().get_by_id(id).unwrap().run_id.unwrap();
    s.handle_pty_event_by_id(id, started.generation, run, PtyEvent::Closed);
    let before = session_now(&s, id).lifecycle_state;
    assert!(s.inner.slots.lock().get_by_id(id).unwrap().run_id.is_none());
    let events = capture_runtime_events(&s);
    s.fire_startup_watchdog(id, started.generation, run, Duration::from_secs(90));
    assert_eq!(session_now(&s, id).lifecycle_state, before);
    assert!(!events.lock().iter().any(|e| matches!(e,
        RuntimeEvent::SessionState { state: LifecycleState::Stalled, .. })));
    s.shutdown().unwrap();
}

#[test]
fn a16_prose_and_prim1s_own_refusal_are_not_notices_but_a_real_banner_is() {
    // T-34 (Victor, room 2844: "They do say blocked but they don't seem blocked").
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Notices".into()),
        member_ids: vec![claude, codex], brief_on_join: false, brief_template: None }).unwrap();
    let (claude_pty, _) = recording_pty_session(9871);
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "claude", DriverKind::Claude, claude_pty);
    let (codex_pty, _) = recording_pty_session(9872);
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, codex_pty);
    let state = |id: SessionId| { let slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id(id).unwrap();
        (slot.work_state, slot.work_detail.clone()) };
    let feed = |name: &str, text: &str| {
        // One lock: `test_session_id` locks the slots too, and the mutex is not re-entrant.
        let generation = supervisor.inner.slots.lock().get(name).unwrap().generation;
        handle_current_pty_event(&supervisor, name, generation, PtyEvent::Output(text.into()));
    };
    let blocked = |id: SessionId| matches!(state(id).0, WorkState::Blocked | WorkState::ErrorLoop);

    // A review report (invented), and PRIM-1's refusal shown to Codex PRIM-1.
    feed("claude", "Counts 3/3/3, no browser/network errors, no failed responses.\r\n");
    assert!(!blocked(claude), "{:?}", state(claude));
    feed("codex", "Claude A (0a1b2c3d) can't take a routed message right now: it keeps hitting a rate limit.\r\n");
    assert!(!blocked(codex), "{:?}", state(codex));
    // The refusal itself no longer carries the banner's words.
    let (reason, _) = describe_work_state_block(WorkState::ErrorLoop, Some("rate_limit"));
    assert!(!reason.contains("rate limit"), "{reason}");

    // A real banner is shown as advisory status (v3 W2, 11 Oct item 1): the
    // room message is still typed.
    feed("claude", "  \u{23bf}  API Error: Connection error.\r\n");
    assert_eq!(state(claude), (WorkState::Blocked, Some("stream_disconnected".into())));
    let advisory = deliver_to(&supervisor, room.room_id, claude, "status?");
    assert_eq!(advisory.receipts[0].status, RoomDeliveryStatus::Written, "{advisory:?}");

    // A question on screen still holds: typed text would answer it.
    feed("claude", "Do you want to proceed?\r\n\u{276f} 1. Yes\r\n  2. No\r\n\r\nEsc to cancel \u{b7} Tab to amend\r\n");
    assert_eq!(state(claude), (WorkState::Blocked, Some("approval_prompt".into())));
    let waiting = deliver_to(&supervisor, room.room_id, claude, "status again?");
    assert_eq!(waiting.receipts[0].status, RoomDeliveryStatus::Stored);
}

/// J3 (contract journey 3): a tool's unauthenticated-download warning that
/// mentions a usage-cap class phrase, and banner text printed back by a file
/// read of a test fixture, hold no wake: advisory status at most. The phrases
/// are assembled at run time so this file never prints them whole.
#[test]
fn j3_tool_warnings_and_printed_fixtures_never_hold_a_wake() {
    let supervisor = test_supervisor();
    let claude = test_session_id(&supervisor, "claude");
    let codex = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("J3".into()),
        member_ids: vec![claude, codex], brief_on_join: false, brief_template: None }).unwrap();
    let (claude_pty, _) = recording_pty_session(9881);
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "claude", DriverKind::Claude, claude_pty);
    let (codex_pty, _) = recording_pty_session(9882);
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, codex_pty);
    let feed = |name: &str, text: &str| {
        let generation = supervisor.inner.slots.lock().get(name).unwrap().generation;
        handle_current_pty_event(&supervisor, name, generation, PtyEvent::Output(text.into()));
    };
    let holds = |id: SessionId| { let slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id(id).unwrap();
        slot.work_state_observed && work_state_holds_delivery(slot.work_state, slot.work_detail.as_deref()) };
    let cap = ["rate", "limits"].join(" ");
    let warning = format!("Warning: You are sending unauthenticated requests to the HF Hub. Please set a HF_TOKEN \
        to enable higher {cap} and faster downloads.\r\n");
    let banner = format!("You've hit your {} limit.", "usage");
    let fixture_print = format!("     12\t    \"text\": \"{banner}\",\r\n     13\t    \"kind\": \"notice\"\r\n");
    for (name, id) in [("claude", claude), ("codex", codex)] {
        feed(name, &warning);
        assert!(!holds(id), "{name}: tool warning holds");
        feed(name, &fixture_print);
        assert!(!holds(id), "{name}: printed fixture holds");
    }
    let sent = deliver_to(&supervisor, room.room_id, claude, "status?");
    assert_eq!(sent.receipts[0].status, RoomDeliveryStatus::Written, "{sent:?}");
}

/// W2 #4 (Reviewers 61218a1a, c4627b84, ded940b6): a Codex advisory notice
/// quiet for the expiry period stops being presented as current, once: no
/// work state and no blocked reason are exported, while the internal state,
/// detail and admission stay. Unrelated output never revives it; a fresh
/// matched notice does, and activity ends it. A modal or another driver never
/// expires (Claude expiry stays open).
#[test]
fn w2_a_quiet_codex_advisory_is_presented_as_an_earlier_notice_only() {
    let supervisor = test_supervisor();
    let (codex_pty, _) = recording_pty_session(9891);
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, codex_pty);
    let (claude_pty, _) = recording_pty_session(9892);
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "claude", DriverKind::Claude, claude_pty);
    let codex = test_session_id(&supervisor, "codex");
    let claude = test_session_id(&supervisor, "claude");
    let set = |id: SessionId, detail: &str, quiet: u64| {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id_mut(id).unwrap();
        slot.work_state_observed = true;
        slot.work_state = WorkState::ErrorLoop;
        slot.work_detail = Some(detail.into());
        slot.last_real_output_at = Instant::now().checked_sub(Duration::from_secs(quiet));
    };
    let reason = |id: SessionId| blocked_reason(supervisor.inner.slots.lock().get_by_id(id).unwrap()).unwrap_or_default();
    let exported = |id: SessionId| {
        let snapshot = supervisor.inner.slots.lock().get_by_id(id).unwrap().snapshot();
        (snapshot.work_state, snapshot.blocked_reason.is_some())
    };
    let notice = |id: SessionId, state: WorkState, detail: &str| {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id_mut(id).unwrap();
        let (alias, run) = (slot.definition.alias.clone(), slot.run_id.unwrap());
        transition_work_state_locked(&alias, slot, run, state, Some(detail.into()));
        slot.last_real_output_at = Some(Instant::now());
    };
    let events = capture_runtime_events(&supervisor);
    let expired = || events.lock().iter().filter(|event| matches!(event, RuntimeEvent::SessionPresentationChanged { .. })).count();

    set(codex, "usage_limit", 29);
    supervisor.expire_quiet_advisory_notices(Instant::now());
    assert_eq!(expired(), 0);
    assert_eq!(exported(codex), (Some(WorkState::ErrorLoop), true), "current before the expiry period");
    set(codex, "usage_limit", 31);
    supervisor.expire_quiet_advisory_notices(Instant::now());
    supervisor.expire_quiet_advisory_notices(Instant::now());
    assert_eq!(expired(), 1, "announced once");
    assert_eq!(exported(codex), (None, false), "no current blocker is asserted");
    {
        let slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id(codex).unwrap();
        assert_eq!((slot.work_state, slot.work_detail.as_deref(), slot.work_state_observed),
            (WorkState::ErrorLoop, Some("usage_limit"), true), "internal evidence stays");
    }
    supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().last_real_output_at = Some(Instant::now());
    assert_eq!(exported(codex), (None, false), "unrelated output never revives it");
    notice(codex, WorkState::ErrorLoop, "usage_limit");
    assert_eq!(exported(codex), (Some(WorkState::ErrorLoop), true), "a fresh matched notice is current again");
    assert!(!reason(codex).is_empty());
    supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().last_real_output_at = Instant::now().checked_sub(Duration::from_secs(31));
    supervisor.expire_quiet_advisory_notices(Instant::now());
    assert_eq!((expired(), exported(codex)), (2, (None, false)), "and expires again once quiet");
    notice(codex, WorkState::Thinking, "working");
    assert_eq!(exported(codex), (Some(WorkState::Thinking), false), "activity ends it");

    set(codex, "approval_prompt", 300);
    set(claude, "usage_limit", 300);
    supervisor.expire_quiet_advisory_notices(Instant::now());
    assert_eq!(expired(), 2, "a modal and another driver never expire");
    assert_eq!((exported(codex).1, exported(claude).1), (true, true), "both still current");
}

/// Reviewer c428530b: after the expiry, the same notice printed again by the
/// member (its output path, the state unchanged) tells the desktop to refresh
/// the card: no work-state event exists for a repeated state.
#[test]
fn w2_a_fresh_same_state_notice_after_expiry_refreshes_the_card() {
    let supervisor = test_supervisor();
    let (codex_pty, _) = recording_pty_session(9894);
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, codex_pty);
    let codex = test_session_id(&supervisor, "codex");
    let feed = |text: &str| {
        let generation = supervisor.inner.slots.lock().get("codex").unwrap().generation;
        handle_current_pty_event(&supervisor, "codex", generation, PtyEvent::Output(text.into()));
    };
    let exported = || {
        let snapshot = supervisor.inner.slots.lock().get_by_id(codex).unwrap().snapshot();
        (snapshot.work_state, snapshot.blocked_reason.is_some())
    };
    let events = capture_runtime_events(&supervisor);
    let refreshes = || events.lock().iter().filter(|event| matches!(event,
        RuntimeEvent::SessionPresentationChanged { session_id, .. } if *session_id == codex)).count();
    let work_states = || events.lock().iter().filter(|event| matches!(event,
        RuntimeEvent::SessionWorkState { identity, .. } if identity.session_id == codex)).count();
    let notice = format!("You've hit your {} limit.\r\n", "usage");

    feed(&notice);
    let (state, current) = exported();
    assert!(matches!(state, Some(WorkState::Blocked | WorkState::ErrorLoop)) && current, "{state:?}");
    let internal = supervisor.inner.slots.lock().get_by_id(codex).unwrap().work_state;
    supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().last_real_output_at =
        Instant::now().checked_sub(Duration::from_secs(31));
    supervisor.expire_quiet_advisory_notices(Instant::now());
    assert_eq!((refreshes(), exported()), (1, (None, false)), "expired once");
    let before = work_states();

    feed(&notice);
    assert_eq!(supervisor.inner.slots.lock().get_by_id(codex).unwrap().work_state, internal, "the state repeats");
    assert_eq!(work_states(), before, "no work-state event for a repeated state");
    assert_eq!(refreshes(), 2, "the desktop is told to refresh the card");
    assert_eq!(exported(), (Some(internal), true), "current again");
    feed(&notice);
    assert_eq!(refreshes(), 2, "a notice that is already current needs no refresh");
}

/// W2 #4 for Claude (Reviewer dcd64067): a Claude advisory notice needs no
/// expiry of its own. The existing 3 s quiesce reads quiet under it as Idle
/// when no question is on screen, and a room message is then typed. A modal
/// still holds: quiet_under_any_claude_startup_question_keeps_it_blocked_and_room_messages_queued.
#[test]
fn w2_a_quiet_claude_advisory_notice_ends_through_the_existing_quiesce() {
    for (notice, detail) in [("\u{23bf}  API Error: overloaded".to_string(), "api_error"),
        (format!("{} limit reached \u{b7} resets 3pm", "Usage"), "rate_limit")] {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(std::process::id());
        install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "claude", DriverKind::Claude, pty);
        let claude = test_session_id(&supervisor, "claude");
        let room = supervisor.create_room(CreateRoomRequest { label: Some("Claude notice".into()),
            member_ids: vec![claude], brief_on_join: false, brief_template: None }).unwrap();
        handle_current_pty_event(&supervisor, "claude", 0, PtyEvent::Output(format!("{notice}\r\n")));
        assert_eq!(supervisor.inner.slots.lock().get("claude").unwrap().work_detail.as_deref(), Some(detail), "{notice}");
        fire_claude_quiesce_after_last_output(&supervisor);
        assert_eq!(supervisor.inner.slots.lock().get("claude").unwrap().work_state, WorkState::Idle, "{notice}");
        let sent = deliver_to(&supervisor, room.room_id, claude, "status?");
        wait_for_room_receipt(&supervisor, room.room_id, sent.message_id, claude, RoomDeliveryStatus::Written);
        assert!(!inputs.lock().is_empty(), "typed after the notice went quiet: {notice}");
    }
}

/// J4 (Claude PRIM-1, contract journey 4): a structured provider error ending
/// the current run's latest turn holds a wake, with its reason in plain words
/// for sender and operator; a later clean turn releases it.
#[test]
fn j4_a_provider_error_on_this_run_holds_with_its_reason_and_a_clean_turn_releases_it() {
    let supervisor = test_supervisor();
    let codex = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("J4".into()),
        member_ids: vec![codex], brief_on_join: false, brief_template: None }).unwrap();
    let (pty, _) = recording_pty_session(9893);
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, pty);
    let run = {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id_mut(codex).unwrap();
        slot.work_state_observed = true;
        slot.work_state = WorkState::Idle;
        slot.harness_session_id = Some("conversation-a".into());
        slot.run_id.unwrap()
    };
    supervisor.apply_provider_turn(codex, run, "conversation-a", submit_confirm::ProviderTurn::Error("unauthorized".into()));
    let reason = blocked_reason(supervisor.inner.slots.lock().get_by_id(codex).unwrap()).unwrap();
    assert!(reason.contains("provider:unauthorized") && reason.contains("credentials"), "{reason}");
    let held = deliver_to(&supervisor, room.room_id, codex, "status?");
    assert_eq!(held.receipts[0].status, RoomDeliveryStatus::Stored, "{held:?}");
    let why = held.receipts[0].queued_because.clone().unwrap_or_default();
    assert!(why.contains("credentials"), "the sender sees why: {why}");

    supervisor.apply_provider_turn(codex, run, "conversation-a", submit_confirm::ProviderTurn::Unknown);
    assert!(supervisor.inner.slots.lock().get_by_id(codex).unwrap().provider_error.is_some(), "Unknown changes nothing");
    supervisor.apply_provider_turn(codex, Uuid::new_v4(), "conversation-a", submit_confirm::ProviderTurn::Clear);
    assert!(supervisor.inner.slots.lock().get_by_id(codex).unwrap().provider_error.is_some(), "another run changes nothing");
    // Reviewer 42c82e54: the same run's follower moved to another
    // conversation during the off-lock read; the old conversation's result
    // neither clears this hold nor sets anything.
    supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().harness_session_id = Some("conversation-b".into());
    supervisor.apply_provider_turn(codex, run, "conversation-a", submit_confirm::ProviderTurn::Clear);
    assert!(supervisor.inner.slots.lock().get_by_id(codex).unwrap().provider_error.is_some(),
        "a scan of the previous conversation changes nothing");
    supervisor.apply_provider_turn(codex, run, "conversation-b", submit_confirm::ProviderTurn::Clear);
    wait_for_room_receipt(&supervisor, room.room_id, held.message_id, codex, RoomDeliveryStatus::Written);
}

/// W2 #6 (11 Oct item 6): a member held for a while whose output or room
/// posts continue is surfaced once per hold, in its room as PRIM-1; activity
/// in the hold's first moments does not count, and an ended hold re-arms.
#[test]
fn w2_a_held_member_that_keeps_working_is_surfaced_once_as_blocked_but_active() {
    let supervisor = test_supervisor();
    let codex = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Held but active".into()),
        member_ids: vec![codex], brief_on_join: false, brief_template: None }).unwrap();
    let (pty, _) = recording_pty_session(9895);
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, pty);
    let events = capture_runtime_events(&supervisor);
    let alerts = || events.lock().iter().filter(|event| matches!(event,
        RuntimeEvent::SupervisorAlert { action: Some(action), .. } if action == "blocked_but_active")).count();
    let t0 = Instant::now();
    supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().provider_error = Some("unauthorized".into());
    supervisor.alert_blocked_but_active(t0);
    // Output only in the hold's first moments: no alert, even much later.
    supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().last_real_output_at = Some(t0 + Duration::from_secs(10));
    supervisor.alert_blocked_but_active(t0 + Duration::from_secs(90));
    assert_eq!(alerts(), 0);
    // Output after the grace, once the hold has lasted: one alert, one notice.
    supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().last_real_output_at = Some(t0 + Duration::from_secs(45));
    supervisor.alert_blocked_but_active(t0 + Duration::from_secs(95));
    supervisor.alert_blocked_but_active(t0 + Duration::from_secs(120));
    assert_eq!(alerts(), 1, "once per hold");
    let notice = room_events(&supervisor, room.room_id).into_iter().find_map(|event| match event.item {
        RoomFeedItem::Message { sender: RoomMessageSender::Prim1 {}, content, recipient_ids, .. } => Some((content, recipient_ids)),
        _ => None,
    }).expect("a PRIM-1 notice in its room");
    assert!(notice.0.contains("held") && notice.0.contains("terminal output continued") && notice.1.is_empty(), "{notice:?}");
    // The hold ends and a new one begins: it can be surfaced again.
    supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().provider_error = None;
    supervisor.alert_blocked_but_active(t0 + Duration::from_secs(130));
    supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().provider_error = Some("unauthorized".into());
    let t1 = t0 + Duration::from_secs(140);
    supervisor.alert_blocked_but_active(t1);
    supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().last_room_post_at = Some(t1 + Duration::from_secs(40));
    supervisor.alert_blocked_but_active(t1 + Duration::from_secs(70));
    assert_eq!(alerts(), 2, "a new hold re-arms; room posts count as activity");

    // Reviewer dfe575e7: a verified Claude question screen without a key hint
    // holds delivery through its observed work state, not open_question(); a
    // member posting to its room under it is surfaced too. An advisory notice
    // never holds, so it is never surfaced.
    let claude = test_session_id(&supervisor, "claude");
    supervisor.add_room_member(AddRoomMemberRequest { room_id: room.room_id, session_id: claude }).unwrap();
    let (claude_pty, _) = recording_pty_session(9896);
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "claude", DriverKind::Claude, claude_pty);
    let (screen, detail) = CLAUDE_QUESTION_SCREENS.iter().copied()
        .find(|(screen, _)| !screen.contains("Enter to confirm") && !screen.contains("Esc"))
        .expect("a question screen without a key hint");
    handle_current_pty_event(&supervisor, "claude", 0, PtyEvent::Output(format!("\x1b[2J\x1b[H{screen}")));
    {
        let slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id(claude).unwrap();
        assert_eq!((slot.work_state, slot.work_detail.as_deref()), (WorkState::Blocked, Some(detail)));
        assert!(slot.room_composer.open_question().is_none(), "precondition: no key-hint shape");
    }
    let t2 = t1 + Duration::from_secs(200);
    supervisor.alert_blocked_but_active(t2);
    supervisor.inner.slots.lock().get_by_id_mut(claude).unwrap().last_room_post_at = Some(t2 + Duration::from_secs(40));
    supervisor.alert_blocked_but_active(t2 + Duration::from_secs(70));
    assert_eq!(alerts(), 3, "a modal hold with continued room posts is surfaced");
    {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id_mut(claude).unwrap();
        slot.work_state = WorkState::ErrorLoop;
        slot.work_detail = Some("rate_limit".into());
        slot.last_room_post_at = Some(t2 + Duration::from_secs(300));
    }
    supervisor.alert_blocked_but_active(t2 + Duration::from_secs(250));
    supervisor.alert_blocked_but_active(t2 + Duration::from_secs(400));
    assert_eq!(alerts(), 3, "an advisory notice holds nothing and is never surfaced");
}

/// W2 open turn (Claude PRIM-1, after b7ba404): a quiet screen with a turn
/// open in the member's own record reads busy and says where that came from;
/// a closed turn or a busy screen carries no such note.
#[test]
fn w2_an_open_turn_in_the_record_reads_busy_and_names_its_source() {
    let supervisor = test_supervisor();
    let codex = test_session_id(&supervisor, "codex");
    let (pty, _) = recording_pty_session(9894);
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, pty);
    let run = {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id_mut(codex).unwrap();
        slot.work_state_observed = true;
        slot.work_state = WorkState::Idle;
        slot.harness_session_id = Some("conversation-a".into());
        slot.run_id.unwrap()
    };
    let shown = || {
        let snapshot = supervisor.inner.slots.lock().get_by_id(codex).unwrap().snapshot();
        (snapshot.work_state, snapshot.work_state_from_record)
    };
    assert_eq!(shown(), (Some(WorkState::Idle), false));
    supervisor.apply_rollout_scan(codex, run, "conversation-a", submit_confirm::ProviderTurn::Clear, Some(true));
    assert_eq!(shown(), (Some(WorkState::Thinking), true), "quiet screen, open turn");
    let json = serde_json::to_value(supervisor.inner.slots.lock().get_by_id(codex).unwrap().snapshot()).unwrap();
    assert_eq!(json["work_state_from_record"], true);
    supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().work_state = WorkState::ToolCall;
    assert_eq!(shown(), (Some(WorkState::ToolCall), false), "the screen's own busy state needs no note");
    supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().work_state = WorkState::Idle;
    supervisor.apply_rollout_scan(codex, Uuid::new_v4(), "conversation-a", submit_confirm::ProviderTurn::Clear, Some(false));
    assert_eq!(shown(), (Some(WorkState::Thinking), true), "another run changes nothing");
    supervisor.inner.slots.lock().get_by_id_mut(codex).unwrap().harness_session_id = Some("conversation-b".into());
    supervisor.apply_rollout_scan(codex, run, "conversation-a", submit_confirm::ProviderTurn::Clear, Some(false));
    assert_eq!(shown(), (Some(WorkState::Thinking), true), "the previous conversation's scan changes nothing (Reviewer 42c82e54)");
    supervisor.apply_rollout_scan(codex, run, "conversation-b", submit_confirm::ProviderTurn::Clear, Some(false));
    assert_eq!(shown(), (Some(WorkState::Idle), false), "a closed turn ends it");
    let json = serde_json::to_value(supervisor.inner.slots.lock().get_by_id(codex).unwrap().snapshot()).unwrap();
    assert!(json.get("work_state_from_record").is_none(), "absent when false");
}

/// v3 W6 (0b708ae flapping): the first code of a wait is recorded at once; a
/// change only after it has held for the settle time; a flip back cancels it.
#[test]
fn w6_a_flapping_waiting_code_is_recorded_only_once_it_holds() {
    let settle = Duration::from_secs(2);
    let t0 = Instant::now();
    let mut pending = None;
    assert!(room_delivery::settle_waiting_code(None, &mut pending, "output_settling", t0, settle), "the first code at once");
    assert!(!room_delivery::settle_waiting_code(Some("output_settling"), &mut pending, "output_settling", t0, settle));
    // The 0b708ae pattern: not_observed, back, again, all inside 2 s.
    assert!(!room_delivery::settle_waiting_code(Some("output_settling"), &mut pending, "not_observed", t0, settle));
    assert!(!room_delivery::settle_waiting_code(Some("output_settling"), &mut pending, "output_settling",
        t0 + Duration::from_millis(500), settle));
    assert_eq!(pending, None, "a flip back cancels the change");
    let t1 = t0 + Duration::from_millis(900);
    assert!(!room_delivery::settle_waiting_code(Some("output_settling"), &mut pending, "not_observed", t1, settle));
    assert!(!room_delivery::settle_waiting_code(Some("output_settling"), &mut pending, "not_observed",
        t1 + Duration::from_millis(1900), settle), "not yet held for 2 s");
    assert!(room_delivery::settle_waiting_code(Some("output_settling"), &mut pending, "not_observed",
        t1 + settle, settle), "held: recorded");
    // A third code restarts the clock.
    assert!(!room_delivery::settle_waiting_code(Some("not_observed"), &mut pending, "draft_open",
        t1 + settle, settle));
}

#[test]
fn a16_queued_since_header_names_the_stored_time_and_its_age() {
    // D3 (A10, 20 Oct): a member started hours late acted on its Stored backlog as live.
    let now = DateTime::parse_from_rfc3339("2026-10-20T17:14:00Z").unwrap().with_timezone(&Utc);
    let after = Duration::from_secs(300);
    assert_eq!(room_delivery::queued_sender_label("Claude PRIM-1", "2026-10-20T11:45:10Z", after, now),
        "Claude PRIM-1, queued since 11:45 UTC, 5 h 28 min ago");
    assert_eq!(room_delivery::queued_sender_label("operator", "2026-10-20T17:02:00Z", after, now),
        "operator, queued since 17:02 UTC, 12 min ago");
    assert_eq!(room_delivery::queued_sender_label("operator", "2026-10-20T17:10:00Z", after, now), "operator",
        "a message typed within five minutes keeps its plain header");
    assert_eq!(room_delivery::queued_sender_label("operator", "not a time", after, now), "operator");
}

#[test]
fn a16_a_message_typed_long_after_it_was_stored_says_since_when() {
    let supervisor = test_supervisor();
    supervisor.set_queued_note_after_for_tests(Duration::from_millis(200));
    let codex = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Backlog".into()),
        member_ids: vec![codex], brief_on_join: false, brief_template: None }).unwrap();
    let old = deliver_to(&supervisor, room.room_id, codex, "use the 11:45 plan");
    assert_eq!(old.receipts[0].status, RoomDeliveryStatus::Stored);
    // v3 W3: a brief keeps the full-body framing, so it still carries the
    // queued-since header; an ordinary message is typed as a pointer and its
    // stored time is read from the feed.
    let brief = supervisor.brief_room_member(BriefRoomMemberRequest { room_id: room.room_id, session_id: codex }).unwrap();
    assert_eq!(brief.receipts[0].status, RoomDeliveryStatus::Stored);
    std::thread::sleep(Duration::from_millis(400));
    // The member starts later and receives the backlog with its age.
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "codex", DriverKind::Codex, pty);
    wait_for_room_receipt(&supervisor, room.room_id, old.message_id, codex, RoomDeliveryStatus::Written);
    wait_for_room_receipt(&supervisor, room.room_id, brief.message_id, codex, RoomDeliveryStatus::Written);
    let typed = inputs.lock().clone();
    assert_eq!(typed[0], typed_pointer(&supervisor, 1, old.message_id));
    assert!(!typed[0].contains("queued since") && !typed[0].contains("11:45 plan"), "{typed:?}");
    let brief_typed = typed.iter().find(|input| input.contains("[Room message from operator, queued since "))
        .unwrap_or_else(|| panic!("the queued brief names its age: {typed:?}"));
    assert!(brief_typed.contains(" UTC, under a minute ago]\n"), "{brief_typed:?}");
    let stored = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap()
        .events.into_iter().find(|event| matches!(&event.item, RoomFeedItem::Message { message_id, .. }
            if *message_id == old.message_id)).unwrap();
    assert!(DateTime::parse_from_rfc3339(&stored.timestamp).is_ok(), "the feed keeps the stored time");
}

/// v3 (e20ec145, Claude PRIM-1 00b17bb1): a member whose launch was refused
/// for a definite reason keeps the message waiting with that reason, not
/// the generic "not running"; nothing is typed or failed. A member that
/// failed after it ran, with its termination not yet proved (Codex PRIM-1),
/// gets the same neutral words and no advice to start it.
#[test]
fn a_member_whose_launch_failed_says_why_its_message_waits() {
    let supervisor = empty_test_supervisor();
    let session = create_test_session(&supervisor, "Unresolved driver", DriverKind::Claude,
        shared_types::PermissionProfile::Normal);
    supervisor.set_executable_resolver_for_tests(Arc::new(FailingExecutableResolver));
    assert!(supervisor.start_session_by_id(session.session_id).is_err());
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Failed launch".into()),
        member_ids: vec![session.session_id], brief_on_join: false, brief_template: None }).unwrap();

    let result = deliver_to(&supervisor, room.room_id, session.session_id, "please review");

    assert_eq!(result.receipts[0].status, RoomDeliveryStatus::Stored);
    assert_eq!(result.receipts[0].queued_because.as_deref(), Some(
        "the member is not running; it failed (injected unresolved Claude executable); the message waits until it is started again"));
    assert!(result.failures.is_empty(), "a waiting message is not a failure");
    assert_eq!(supervisor.queued_room_wake_count_for_tests(session.session_id), 1);
    // The room directory gives the same honest reason (Reviewer e7eb2154).
    let directory = |supervisor: &SupervisorHandle| supervisor.room_directory_for_room(room.room_id).unwrap().members
        .into_iter().find(|member| member.session_id == session.session_id).unwrap().not_ready;
    assert_eq!(directory(&supervisor).as_deref(), result.receipts[0].queued_because.as_deref());

    // A very long launch error is quoted in part only.
    let long = "x".repeat(400);
    supervisor.inner.slots.lock().get_by_id_mut(session.session_id).unwrap().last_error = Some(long);
    let reason = room_delivery::room_wake_busy(supervisor.inner.slots.lock().get_by_id(session.session_id).unwrap())
        .unwrap().into_owned();
    assert!(reason.starts_with("the member is not running; it failed (") && reason.contains("x…)") && reason.len() < 400,
        "{reason}");

    // The shape a run that failed after it started leaves (lib.rs, the PTY,
    // liveness or exit failure path): run gone, Failed, termination unproved.
    {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id_mut(session.session_id).unwrap();
        slot.run_id = None;
        slot.termination_uncertain = true;
        slot.last_error = Some("the PTY closed; process-scope termination proof is in progress".into());
    }
    let reason = room_delivery::room_wake_busy(supervisor.inner.slots.lock().get_by_id(session.session_id).unwrap())
        .unwrap().into_owned();
    assert_eq!(reason, "the member is not running; it failed (the PTY closed; process-scope termination proof is in progress) and PRIM-1 has not yet confirmed that its process ended, so the message waits");
    assert!(!reason.contains("did not start") && !reason.contains("started again"), "{reason}");
    assert_eq!(directory(&supervisor).as_deref(), Some(reason.as_str()));
    supervisor.inner.slots.lock().get_by_id_mut(session.session_id).unwrap().termination_uncertain = false;

    // A member that is merely stopped (no failed launch) keeps the old words.
    {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id_mut(session.session_id).unwrap();
        slot.state = LifecycleState::Closed;
        slot.last_error = None;
    }
    assert_eq!(room_delivery::room_wake_busy(supervisor.inner.slots.lock().get_by_id(session.session_id).unwrap())
        .as_deref(), Some("the member is not running; the message waits until it starts"));
}
