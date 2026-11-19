// Included in the supervisor test module. Only temporary catalogs and mock
// PTYs are used; replay never invokes an installed harness or live endpoint.

fn hold_room_startup_fence_for_tests(supervisor: &SupervisorHandle, room_id: RoomId, unbind_epoch: bool) {
    let mut rooms = supervisor.inner.rooms.lock();
    drop(rooms.by_id.remove(&room_id).unwrap());
    if unbind_epoch {
        rooms.catalog.rooms.iter_mut().find(|room| room.room_id == room_id).unwrap().join_floor_epoch = None;
        supervisor.persist_room_catalog_candidate(&rooms.catalog).unwrap();
    }
    let definition = rooms.catalog.rooms.iter().find(|room| room.room_id == room_id).unwrap().clone();
    let mut room = RoomRuntime::from_persisted(definition);
    let error = room.open_log_with_sync(supervisor.runtime_dir(),
        |_| Err(std::io::Error::other("injected startup fence refusal"))).unwrap_err();
    assert!(error.is::<room_log::StartupSyncFailed>());
    assert!(room.read(None, 0).is_err());
    assert!(room.startup_sync_pending());
    rooms.by_id.insert(room_id, room);
}

#[test]
fn room_startup_fence_retries_without_reopen_or_unsynced_history_publication() {
    let supervisor = test_supervisor();
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Startup fence".into()),
        member_ids: vec![], brief_on_join: false, brief_template: None }).unwrap();
    supervisor.post_room_message(PostRoomMessageRequest { room_id: room.room_id,
        request_id: Some("before-startup".into()), content: "durable prefix".into() }).unwrap();
    let before = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
    hold_room_startup_fence_for_tests(&supervisor, room.room_id, false);
    let bytes = supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().read_owned_log_for_tests();
    for _ in 0..2 {
        supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().fail_next_replay_sync_for_tests();
        let error = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap_err();
        assert_eq!(room_request_error_kind(&error), "recovery_required");
        let mut rooms = supervisor.inner.rooms.lock();
        let held = rooms.get_mut(room.room_id).unwrap();
        assert!(held.read(None, 0).is_err());
        assert!(held.snapshot().history_replay_pending);
        assert_eq!(held.read_owned_log_for_tests(), bytes);
        assert!(room_log::RoomLog::open(&supervisor.runtime_dir().join("rooms").join(format!("{}.jsonl", room.room_id)),
            room.room_id, |_| Ok(())).is_err(), "failed retries retain the original exclusive writer");
    }
    let after = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
    assert_eq!(after, before);
    let snapshot = supervisor.inner.rooms.lock().get(room.room_id).unwrap().snapshot();
    assert!(snapshot.recovery_error.is_none() && !snapshot.history_replay_pending);
    let next = supervisor.post_room_message(PostRoomMessageRequest { room_id: room.room_id,
        request_id: Some("after-fence".into()), content: "append at original EOF".into() }).unwrap();
    assert_eq!(next.cursor.epoch, before.cursor.epoch);
    assert_eq!(next.cursor.sequence, before.cursor.sequence + 1);
}

#[test]
fn room_startup_fence_persists_epoch_binding_before_read_admission() {
    let supervisor = test_supervisor();
    let member = test_session_id(&supervisor, "claude");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Startup catalog fence".into()),
        member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap();
    let before = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
    hold_room_startup_fence_for_tests(&supervisor, room.room_id, true);
    let path = room_catalog_path(supervisor.runtime_dir());
    let catalog_bytes = fs::read(&path).unwrap();
    supervisor.fail_next_room_catalog_write_for_tests();
    let error = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap_err();
    assert!(error.to_string().contains("catalog persistence failure"));
    assert_eq!(room_request_error_kind(&error), "recovery_required");
    assert_eq!(fs::read(&path).unwrap(), catalog_bytes);
    {
        let rooms = supervisor.inner.rooms.lock();
        assert!(rooms.get(room.room_id).unwrap().read(None, 0).is_err());
        assert!(rooms.catalog.rooms.iter().find(|row| row.room_id == room.room_id).unwrap().join_floor_epoch.is_none());
    }
    assert_eq!(supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap(), before);
    let catalog = read_room_catalog(&path).unwrap();
    let persisted = catalog.rooms.iter().find(|row| row.room_id == room.room_id).unwrap();
    assert_eq!(persisted.join_floor_epoch, Some(before.cursor.epoch));
    assert_eq!(persisted.join_floors.as_ref().unwrap().get(&member), Some(&0));
}

#[test]
fn room_startup_fence_releases_registries_and_rechecks_pane_run_after_sync() {
    let supervisor = test_supervisor();
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude,
        Some(std::process::id()), pty, BracketedPasteMode::Unknown);
    let member = test_session_id(&supervisor, "claude");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Startup caller race".into()),
        member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap();
    let caller = supervisor.resolve_pane_caller(TestPaneProcess::new(9001, [std::process::id()])).unwrap();
    hold_room_startup_fence_for_tests(&supervisor, room.room_id, false);
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().before_replay_io_for_tests(move || {
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    let reader = supervisor.clone();
    let worker = thread::spawn(move || reader.read_room_feed_from_pane(&caller, None));
    let entered = entered_rx.recv_timeout(Duration::from_secs(3));
    let rooms_free = supervisor.inner.rooms.try_lock().is_some();
    let slots_free = if let Some(mut slots) = supervisor.inner.slots.try_lock() {
        let slot = slots.get_by_id_mut(member).unwrap();
        slot.run_id = Some(Uuid::new_v4());
        slot.generation += 1;
        true
    } else { false };
    let _ = release_tx.send(());
    let result = worker.join().unwrap();
    entered.expect("startup sync did not reach its barrier");
    assert!(rooms_free && slots_free, "startup sync must release both shared registries");
    assert!(result.is_err(), "a replaced run must not inherit the prior caller's read authority");
    assert!(inputs.lock().is_empty());
    let page = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
    assert!(!page.events.iter().any(|event| matches!(event.item, RoomFeedItem::Delivery { .. })));
}

#[test]
fn room_structural_delete_refusal_explains_restart_without_expanding_authority() {
    let supervisor = test_supervisor();
    let member = test_session_id(&supervisor, "claude");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Held deletion".into()),
        member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap();
    let bytes = {
        let mut rooms = supervisor.inner.rooms.lock();
        let runtime = rooms.get_mut(room.room_id).unwrap();
        let (message, _) = runtime.post("known-refusal".into(), RoomMessageSender::Operator {},
            "preserve receipt evidence".into(), vec![member], room.membership_revision).unwrap();
        let message_id = runtime.delivery_result(&message, false).message_id;
        let sequence = runtime.next_sequence;
        let item = RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, readmit: false, pointer: None, message_id, recipient_id: member,
            status: RoomDeliveryStatus::Held, bytes_written: 0, zero_write_confirmed: true,
            error: Some("known pretransport refusal".into()), run_id: None, generation: None };
        runtime.fail_next_commit_for_tests(false);
        assert!(runtime.append(item.clone()).is_err());
        runtime.receipt_reservations = 1;
        runtime.retain_receipt_evidence(&message, sequence, None, item);
        runtime.receipt_reservations = 0;
        runtime.append_invalid_complete_record_for_tests();
        runtime.read_owned_log_for_tests()
    };
    let error = supervisor.delete_room(DeleteRoomRequest { room_id: room.room_id }).unwrap_err();
    assert!(error.to_string().contains("Restart PRIM-1"));
    let mut rooms = supervisor.inner.rooms.lock();
    let held = rooms.get_mut(room.room_id).unwrap();
    assert_eq!(held.read_owned_log_for_tests(), bytes);
    assert_eq!(held.receipt_evidence_count_for_tests(), 1);
    assert_eq!(held.snapshot().writer_recovery.unwrap().kind, shared_types::RoomWriterRecoveryKind::Structural);
}

#[test]
fn room_structural_writer_hold_preserves_acknowledged_reads_bytes_and_removal_without_rescan() {
    let supervisor = test_supervisor();
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude,
        Some(std::process::id()), pty, BracketedPasteMode::Unknown);
    let member = test_session_id(&supervisor, "claude");
    let other = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Damaged writer".into()),
        member_ids: vec![member, other], brief_on_join: false, brief_template: None }).unwrap();
    supervisor.post_room_message(PostRoomMessageRequest { room_id: room.room_id,
        request_id: Some("acknowledged".into()), content: "keep visible".into() }).unwrap();
    let before = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
    let events = capture_runtime_events(&supervisor);
    let request = PostRoomMessageRequest { room_id: room.room_id,
        request_id: Some("uncertain-before-damage".into()), content: "unpublished".into() };
    supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().fail_next_commit_for_tests(true);
    assert!(supervisor.post_room_message(request.clone()).is_err());
    let bytes = {
        let mut rooms = supervisor.inner.rooms.lock();
        let room = rooms.get_mut(room.room_id).unwrap();
        room.append_invalid_complete_record_for_tests();
        room.read_owned_log_for_tests()
    };
    let error = supervisor.post_room_message(request.clone()).unwrap_err();
    assert_eq!(room_request_error_kind(&error), "history_unavailable");
    let snapshot = supervisor.snapshot().rooms.into_iter().find(|r| r.room_id == room.room_id).unwrap();
    assert!(snapshot.recovery_error.is_none() && snapshot.history_replay_pending);
    let hold = snapshot.writer_recovery.unwrap();
    assert_eq!(hold.kind, shared_types::RoomWriterRecoveryKind::Structural);
    assert!(hold.reason.contains("invalid complete") && hold.reason.contains(&room.room_id.to_string()));
    assert!(hold.reason.chars().count() <= 1024);
    assert_eq!(supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap(), before);
    supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().before_replay_io_for_tests(||
        panic!("permanent structural hold must not rescan the file"));
    let repeated = supervisor.post_room_message(request).unwrap_err();
    assert_eq!(room_request_error_kind(&repeated), "history_unavailable");
    assert_eq!(events.lock().iter().filter(|event| matches!(event,
        RuntimeEvent::RoomHistoryChanged { room_id, .. } if *room_id == room.room_id)).count(), 2,
        "poison and newly visible cause each invalidate once; identical held requests do not");
    let caller = supervisor.resolve_pane_caller(TestPaneProcess::new(9001, [std::process::id()])).unwrap();
    let event_count = events.lock().len();
    for _ in 0..2 {
        assert_eq!(supervisor.read_room_feed_from_pane(&caller, None).unwrap().events, before.events);
    }
    assert_eq!(events.lock().len(), event_count, "held reads add neither false receipts nor repeated warnings");
    let removed = supervisor.remove_room_member(RemoveRoomMemberRequest {
        room_id: room.room_id, session_id: member,
    }).unwrap();
    assert!(!removed.member_ids.contains(&member));
    assert!(supervisor.read_room_feed_from_pane(&caller, None).is_err());
    assert!(supervisor.add_room_member(AddRoomMemberRequest {
        room_id: room.room_id, session_id: member,
    }).is_err(), "adding remains behind writer admission");
    let page = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
    assert_eq!(page.events, before.events);
    assert_eq!(page.cursor, before.cursor);
    assert_eq!(supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().read_owned_log_for_tests(), bytes);
    assert_eq!(events.lock().iter().filter(|event| matches!(event,
        RuntimeEvent::RoomHistoryChanged { room_id, .. } if *room_id == room.room_id)).count(), 2);
    assert!(inputs.lock().is_empty());
}

#[test]
fn room_transient_replay_cause_is_visible_once_and_clears_after_durable_recovery() {
    let supervisor = test_supervisor();
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Retryable writer".into()),
        member_ids: vec![], brief_on_join: false, brief_template: None }).unwrap();
    let before = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
    let events = capture_runtime_events(&supervisor);
    let request = PostRoomMessageRequest { room_id: room.room_id,
        request_id: Some("transient-cause".into()), content: "one exact message".into() };
    supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().fail_next_commit_for_tests(true);
    assert!(supervisor.post_room_message(request.clone()).is_err());
    for _ in 0..2 {
        supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().fail_next_replay_sync_for_tests();
        let error = supervisor.post_room_message(request.clone()).unwrap_err();
        assert_eq!(room_request_error_kind(&error), "recovery_required");
        let snapshot = supervisor.snapshot().rooms.into_iter().find(|r| r.room_id == room.room_id).unwrap();
        assert!(snapshot.recovery_error.is_none() && snapshot.history_replay_pending);
        let hold = snapshot.writer_recovery.unwrap();
        assert_eq!(hold.kind, shared_types::RoomWriterRecoveryKind::Transient);
        assert!(hold.reason.contains("replay sync failure") && hold.reason.chars().count() <= 1024);
        assert_eq!(supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap(), before);
        assert_eq!(events.lock().iter().filter(|event| matches!(event,
            RuntimeEvent::RoomHistoryChanged { room_id, .. } if *room_id == room.room_id)).count(), 2);
    }
    assert!(supervisor.post_room_message(request).unwrap().replayed);
    let snapshot = supervisor.snapshot().rooms.into_iter().find(|r| r.room_id == room.room_id).unwrap();
    assert!(snapshot.writer_recovery.is_none() && !snapshot.history_replay_pending);
    assert_eq!(events.lock().iter().filter(|event| matches!(event,
        RuntimeEvent::RoomHistoryChanged { room_id, .. } if *room_id == room.room_id)).count(), 3);
    let page = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
    assert_eq!(page.events.iter().filter(|event| matches!(event.item, RoomFeedItem::Message { .. })).count(), 1);
}

#[test]
fn room_recovery_io_keeps_old_reads_input_lock_and_concurrent_settings_safe() {
    let supervisor = test_supervisor();
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_running_session_with_process_id_and_mode(&supervisor, "codex", DriverKind::Codex,
        Some(std::process::id()), pty, BracketedPasteMode::Unknown);
    let input_member = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Replay outside registry".into()),
        member_ids: vec![test_session_id(&supervisor, "claude")],
        brief_on_join: false, brief_template: None }).unwrap();
    let before = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
    let request = PostRoomMessageRequest { room_id: room.room_id,
        request_id: Some("recover-outside-lock".into()), content: "preserved uncertain body".into() };
    supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().fail_next_commit_for_tests(true);
    assert!(supervisor.post_room_message(request.clone()).is_err());
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().before_replay_io_for_tests(move || {
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    let writer = supervisor.clone();
    let replay = thread::spawn(move || writer.post_room_message(request));
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

    let (settings_started_tx, settings_started_rx) = mpsc::sync_channel(1);
    let (settings_done_tx, settings_done_rx) = mpsc::sync_channel(1);
    let writer = supervisor.clone();
    let settings = thread::spawn(move || {
        settings_started_tx.send(()).unwrap();
        settings_done_tx.send(writer.update_room_brief(shared_types::UpdateRoomBriefRequest {
            room_id: room.room_id, brief_on_join: true, brief_template: Some("settings after replay".into()),
        })).unwrap();
    });
    settings_started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let settings_before_release = settings_done_rx.recv_timeout(Duration::from_millis(100));
    let (observed_tx, observed_rx) = mpsc::sync_channel(1);
    let observer = supervisor.clone();
    let read = thread::spawn(move || {
        let page = observer.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None });
        let snapshot = observer.snapshot();
        let input = observer.send_input(SendInputRequest { session_id: input_member, input: "still typing".into() });
        let path = observer.inner.runtime_dir.join("rooms").join(format!("{}.jsonl", room.room_id));
        let second_writer_refused = crate::room_log::RoomLog::open(&path, room.room_id, |_| Ok(())).is_err();
        observed_tx.send((page, snapshot, input, second_writer_refused)).unwrap();
    });
    let observed_before_release = observed_rx.recv_timeout(Duration::from_secs(2));
    // Release all waiters before reporting a lock regression.
    release_tx.send(()).unwrap();
    let result = replay.join().unwrap().unwrap();
    settings.join().unwrap();
    read.join().unwrap();
    assert!(matches!(settings_before_release, Err(mpsc::RecvTimeoutError::Timeout)),
        "settings must serialize with candidate publication");
    settings_done_rx.recv().unwrap().unwrap();
    let (page, snapshot, input, second_writer_refused) = observed_before_release
        .expect("room replay held the registry needed by reads or another pane");
    assert_eq!(page.unwrap(), before);
    assert!(snapshot.rooms.iter().find(|row| row.room_id == room.room_id).unwrap().history_replay_pending);
    input.unwrap();
    assert_eq!(inputs.lock().as_slice(), &["still typing"]);
    assert!(second_writer_refused);
    assert!(result.replayed);
    let settings = supervisor.read_room_brief(shared_types::ReadRoomBriefRequest { room_id: room.room_id }).unwrap();
    assert_eq!(settings.brief_template.as_deref(), Some("settings after replay"));
    assert!(settings.brief_on_join);
    let path = supervisor.inner.runtime_dir.join("rooms").join(format!("{}.jsonl", room.room_id));
    assert!(crate::room_log::RoomLog::open(&path, room.room_id, |_| Ok(())).is_err(),
        "dropping the replay clone must not release the original writer lock");
}

#[test]
fn room_poison_recovery_reconciles_lost_post_reply_before_new_wake() {
    for after_write in [false, true] {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(std::process::id());
        install_mock_room_wake_recipient(&supervisor, "claude", pty);
        let recipient = test_session_id(&supervisor, "claude");
        let room = supervisor.create_room(CreateRoomRequest { label: Some("Replay".into()),
            member_ids: vec![recipient], brief_on_join: false, brief_template: None }).unwrap();
        let events = capture_runtime_events(&supervisor);
        let request = DeliverRoomMessageRequest { room_id: room.room_id,
            request_id: Some("lost-reply".into()), recipients: RoomRecipientSelection::All {},
            content: "exact original body".into() };
        supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().fail_next_commit_for_tests(after_write);
        let error = supervisor.deliver_room_message(request.clone()).unwrap_err();
        assert!(room_commit_is_uncertain(&error));
        assert!(supervisor.snapshot().rooms.iter().find(|r| r.room_id == room.room_id).unwrap().history_replay_pending);
        assert!(inputs.lock().is_empty());
        if after_write {
            // Even a conflicting requested body must publish the recovered
            // original, while refusing to change its intent or perform a wake.
            let mut conflict = request.clone();
            conflict.content = "changed body".into();
            let conflict = supervisor.deliver_room_message(conflict).unwrap_err();
            assert_eq!(room_request_error_kind(&conflict), "request_conflict");
            assert!(conflict.to_string().contains("request_id conflict"));
        }
        let recovered = supervisor.deliver_room_message(request.clone()).unwrap();
        assert_eq!(recovered.replayed, after_write);
        if after_write {
            assert_eq!(recovered.receipts[0].status, RoomDeliveryStatus::Held);
            assert!(recovered.receipts[0].recorded);
        }
        assert!(!supervisor.snapshot().rooms.iter().find(|r| r.room_id == room.room_id).unwrap().history_replay_pending);
        assert_eq!(inputs.lock().len(), if after_write { 0 } else { 2 });
        let before = inputs.lock().clone();
        let repeated = supervisor.deliver_room_message(request).unwrap();
        assert!(repeated.replayed);
        assert_eq!(repeated.message_id, recovered.message_id);
        assert_eq!(*inputs.lock(), before);
        let page = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
        let messages: Vec<_> = page.events.iter().filter(|event| matches!(event.item, RoomFeedItem::Message { .. })).collect();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].cursor.epoch, room.feed_epoch);
        assert_eq!(last_stored_room_body(&supervisor, recipient), "exact original body");
        assert_eq!(events.lock().iter().filter(|event| matches!(event,
            RuntimeEvent::RoomHistoryChanged { room_id, .. } if *room_id == room.room_id)).count(), 2,
            "one poisoned and one recovered invalidation, never one per append");
        let published: Vec<_> = events.lock().iter().filter_map(|event| match event {
            RuntimeEvent::RoomFeedEvent { feed_event } => Some(feed_event.clone()), _ => None,
        }).collect();
        assert_eq!(published.iter().filter(|event| matches!(event.item, RoomFeedItem::Message { .. })).count(), 1);
        assert!(published.windows(2).all(|pair| pair[0].cursor.sequence < pair[1].cursor.sequence));
    }
}

#[test]
fn room_recovered_terminal_outcome_releases_only_matching_run_after_successful_install() {
    for (complete, replaced, fetched) in [(true, false, false), (false, false, false),
        (true, true, false), (true, false, true)] {
        let supervisor = test_supervisor();
        // The fetched case reads inside the paste: a read before the first
        // byte now makes the pointer moot (v3 W3), and this test is about a
        // typed wake whose message was read before its outcome was committed.
        let inputs = Arc::new(Mutex::new(Vec::new()));
        let on_paste: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>> = Arc::new(Mutex::new(None));
        let pty: Box<dyn PtySessionTrait> = Box::new(ReadAfterPastePty { process_id: std::process::id(),
            inputs: inputs.clone(), on_first: on_paste.clone() });
        install_mock_room_wake_recipient(&supervisor, "claude", pty);
        let recipient = test_session_id(&supervisor, "claude");
        let room = supervisor.create_room(CreateRoomRequest { label: Some("Terminal recovery".into()),
            member_ids: vec![recipient], brief_on_join: false, brief_template: None }).unwrap();
        let gate = supervisor.inner.slots.lock().get_by_id(recipient).unwrap()
            .running.as_ref().unwrap().input_gate.clone();
        let inner = Arc::downgrade(&supervisor.inner);
        let fault = AtomicBool::new(true);
        supervisor.set_event_sink(move |event| {
            if let RuntimeEvent::RoomFeedEvent { feed_event: shared_types::RoomFeedEvent {
                item: RoomFeedItem::Delivery { message_id, status: RoomDeliveryStatus::Pending,
                    run_id, generation, .. }, .. } } = event {
                if fault.swap(false, Ordering::SeqCst) {
                    let room_id = room.room_id;
                    let inner = inner.clone();
                    let fetch_then_fault = move || {
                        let inner = inner.upgrade().unwrap();
                        let mut rooms = inner.rooms.lock();
                        let room = rooms.get_mut(room_id).unwrap();
                        if fetched {
                            room.append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, readmit: false, pointer: None, message_id, recipient_id: recipient,
                                status: RoomDeliveryStatus::Fetched, bytes_written: 0, zero_write_confirmed: false,
                                error: None, run_id, generation }).unwrap();
                        }
                        room.fail_next_commit_for_tests(complete);
                    };
                    if fetched { *on_paste.lock() = Some(Box::new(fetch_then_fault)); } else { fetch_then_fault(); }
                }
            }
        });
        let request = DeliverRoomMessageRequest { request_id: Some("terminal-outcome".into()),
            room_id: room.room_id, recipients: RoomRecipientSelection::All {}, content: "original".into() };
        let uncertain = supervisor.deliver_room_message(request.clone()).unwrap();
        assert_eq!(uncertain.receipts[0].status, RoomDeliveryStatus::Written);
        assert!(!uncertain.receipts[0].recorded);
        assert_eq!(inputs.lock().len(), 2);
        assert!(gate.room_wake_pending.load(Ordering::SeqCst));
        if replaced {
            let mut slots = supervisor.inner.slots.lock();
            let slot = slots.get_by_id_mut(recipient).unwrap();
            slot.run_id = Some(Uuid::new_v4());
            slot.generation += 1;
            // Model a replacement run already holding its own unknown attempt.
            let new_gate = Arc::new(RunInputGate::new());
            new_gate.room_wake_pending.store(true, Ordering::SeqCst);
            slot.running.as_mut().unwrap().input_gate = new_gate;
        }
        supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().fail_next_replay_sync_for_tests();
        assert!(supervisor.deliver_room_message(request.clone()).is_err());
        assert!(gate.room_wake_pending.load(Ordering::SeqCst), "failed recovery cannot release a claim");
        let recovered = supervisor.deliver_room_message(request).unwrap();
        assert_eq!(inputs.lock().len(), 2, "recovery never retries transport");
        let current_gate = supervisor.inner.slots.lock().get_by_id(recipient).unwrap()
            .running.as_ref().unwrap().input_gate.clone();
        assert_eq!(current_gate.room_wake_pending.load(Ordering::SeqCst), replaced);
        assert_eq!(recovered.receipts[0].status, if fetched { RoomDeliveryStatus::Fetched }
            else { RoomDeliveryStatus::Written });
        if !replaced {
            // Supply a fresh empty composer observation, keeping the same run.
            supervisor.inner.slots.lock().get_by_id_mut(recipient).unwrap().room_composer.observe(&room_empty_fixture());
            let next = supervisor.deliver_room_message(DeliverRoomMessageRequest { request_id: None,
                room_id: room.room_id, recipients: RoomRecipientSelection::All {}, content: "new intent".into() }).unwrap();
            assert_eq!(next.receipts[0].status, RoomDeliveryStatus::Written);
            assert_eq!(inputs.lock().len(), 4, "only a separately requested new intent wakes again");
        }
    }
}

#[test]
fn room_recovered_message_requires_explicit_supported_retry_without_duplicate_message() {
    for supported in [false, true] {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(std::process::id());
        install_mock_room_wake_recipient(&supervisor, "claude", pty);
        let recipient = test_session_id(&supervisor, "claude");
        // A106: Claude now has the live adapter; Prime is the unsupported pane.
        if !supported { supervisor.inner.slots.lock().get_by_id_mut(recipient).unwrap().definition.driver = DriverKind::Prime; }
        let room = supervisor.create_room(CreateRoomRequest { label: Some("Explicit recovery retry".into()),
            member_ids: vec![recipient], brief_on_join: false, brief_template: None }).unwrap();
        let request = DeliverRoomMessageRequest { request_id: Some("message-only".into()), room_id: room.room_id,
            recipients: RoomRecipientSelection::All {}, content: "unchanged body".into() };
        supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().fail_next_commit_for_tests(true);
        assert!(supervisor.deliver_room_message(request.clone()).is_err());
        let recovered = supervisor.deliver_room_message(request).unwrap();
        assert_eq!(recovered.receipts[0].status, RoomDeliveryStatus::Held);
        assert!(inputs.lock().is_empty());
        let page = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
        let held = page.events.iter().find(|event| matches!(event.item,
            RoomFeedItem::Delivery { status: RoomDeliveryStatus::Held, .. })).unwrap();
        let retry = shared_types::RetryRoomDeliveryRequest { room_id: room.room_id,
            message_id: recovered.message_id, recipient_id: recipient, expected_receipt: held.cursor };
        let result = supervisor.retry_room_delivery(retry.clone());
        if supported {
            assert_eq!(result.unwrap().receipts[0].status, RoomDeliveryStatus::Written);
            assert_eq!(inputs.lock().len(), 2);
            assert!(supervisor.retry_room_delivery(retry).unwrap().replayed);
            assert_eq!(inputs.lock().len(), 2, "stale retry cursor cannot create another attempt");
        } else {
            assert!(result.unwrap_err().to_string().contains("no verified room-wake adapter"));
            assert!(inputs.lock().is_empty());
        }
        let page = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
        assert_eq!(page.events.iter().filter(|event| matches!(event.item, RoomFeedItem::Message { .. })).count(), 1);
    }
}

#[test]
fn room_poison_recovery_replays_unknown_pending_without_repeating_transport() {
    let supervisor = test_supervisor();
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_room_wake_recipient(&supervisor, "claude", pty);
    let recipient = test_session_id(&supervisor, "claude");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Pending replay".into()),
        member_ids: vec![recipient], brief_on_join: false, brief_template: None }).unwrap();
    let inner = Arc::downgrade(&supervisor.inner);
    let fault = AtomicBool::new(true);
    supervisor.set_event_sink(move |event| {
        if matches!(event, RuntimeEvent::RoomFeedEvent { feed_event: shared_types::RoomFeedEvent {
            item: RoomFeedItem::Message { .. }, .. } }) && fault.swap(false, Ordering::SeqCst) {
            inner.upgrade().unwrap().rooms.lock().get_mut(room.room_id).unwrap().fail_next_commit_for_tests(true);
        }
    });
    let request = DeliverRoomMessageRequest { request_id: Some("pending-replay".into()),
        room_id: room.room_id, recipients: RoomRecipientSelection::All {}, content: "pending original".into() };
    let uncertain = supervisor.deliver_room_message(request.clone()).unwrap();
    assert!(!uncertain.receipts[0].recorded);
    assert!(inputs.lock().is_empty());
    supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().fail_next_replay_sync_for_tests();
    assert_eq!(room_request_error_kind(&supervisor.deliver_room_message(request.clone()).unwrap_err()), "recovery_required");
    let recovered = supervisor.deliver_room_message(request).unwrap();
    assert!(recovered.replayed);
    assert_eq!(recovered.receipts[0].status, RoomDeliveryStatus::Pending);
    let page = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
    let pending = page.events.iter().find(|event| matches!(event.item,
        RoomFeedItem::Delivery { status: RoomDeliveryStatus::Pending, .. })).unwrap();
    assert!(supervisor.retry_room_delivery(shared_types::RetryRoomDeliveryRequest {
        room_id: room.room_id, message_id: recovered.message_id, recipient_id: recipient,
        expected_receipt: pending.cursor,
    }).unwrap_err().to_string().contains("cannot be safely retried"));
    let later = supervisor.deliver_room_message(DeliverRoomMessageRequest { request_id: None,
        room_id: room.room_id, recipients: RoomRecipientSelection::All {}, content: "new stored body".into() }).unwrap();
    assert_eq!(later.receipts[0].status, RoomDeliveryStatus::Written,
        "a failed claim had no transport effect and cannot strand this run");
    assert_eq!(inputs.lock().len(), 2, "only the new intent writes one paste and Enter");
}

#[test]
fn room_poison_recovery_waits_for_active_delivery_then_repairs_its_known_outcome() {
    let supervisor = test_supervisor();
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    install_mock_room_wake_recipient(&supervisor, "claude", Box::new(FirstWriteBlockingPtySession {
        process_id: std::process::id(), inputs: inputs.clone(), calls: calls.clone(),
        first_entered: entered_tx, release: release.clone(),
    }));
    let recipient = test_session_id(&supervisor, "claude");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Active replay".into()),
        member_ids: vec![recipient], brief_on_join: false, brief_template: None }).unwrap();
    let handle = supervisor.clone();
    let wake = thread::spawn(move || handle.deliver_room_message(DeliverRoomMessageRequest {
        request_id: None, room_id: room.room_id, recipients: RoomRecipientSelection::All {}, content: "active wake".into(),
    }));
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().fail_next_commit_for_tests(true);
    let request = PostRoomMessageRequest { request_id: Some("during-active".into()),
        room_id: room.room_id, content: "stored during active wake".into() };
    let uncertain = supervisor.post_room_message(request.clone());
    let held = supervisor.post_room_message(request.clone());
    { let (released, ready) = &*release; *released.lock() = true; ready.notify_all(); }
    let settled = wake.join().unwrap().unwrap();
    assert!(room_commit_is_uncertain(&uncertain.unwrap_err()));
    assert!(held.unwrap_err().to_string().contains("current delivery to settle"));
    assert!(!settled.receipts[0].recorded);
    assert!(supervisor.post_room_message(request).unwrap().replayed);
    let later = supervisor.deliver_room_message(DeliverRoomMessageRequest { request_id: None,
        room_id: room.room_id, recipients: RoomRecipientSelection::All {}, content: "later explicit wake".into() }).unwrap();
    assert_eq!(later.receipts[0].status, RoomDeliveryStatus::Written);
    assert_eq!(calls.load(Ordering::SeqCst), 4, "replay performs no transport; the explicit new intent adds one paste/Enter");
}
