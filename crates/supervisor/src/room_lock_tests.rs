// Temporary catalogs and mock PTYs only. Barriers hold the exact contested
// operation; cleanup releases them even when the bounded assertion fails.

#[test]
fn room_catalog_commit_does_not_block_other_pane_raw_input() {
    let supervisor = test_supervisor();
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_running_session_with_process_id_and_mode(&supervisor, "codex", DriverKind::Codex,
        Some(std::process::id()), pty, BracketedPasteMode::Unknown);
    let input_recipient = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Slow settings save".into()),
        member_ids: vec![test_session_id(&supervisor, "claude")],
        brief_on_join: false, brief_template: None }).unwrap();
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    *supervisor.inner.room_catalog_before_write.lock() = Some(Box::new(move || {
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    }));
    let writer = supervisor.clone();
    let save = thread::spawn(move || writer.update_room_brief(shared_types::UpdateRoomBriefRequest {
        room_id: room.room_id, brief_on_join: true, brief_template: Some("fresh room brief".into()),
    }));
    entered_rx.recv_timeout(Duration::from_secs(2)).expect("room commit did not reach its barrier");
    let (input_tx, input_rx) = mpsc::sync_channel(1);
    let writer = supervisor.clone();
    let input = thread::spawn(move || {
        input_tx.send(writer.send_input(SendInputRequest { session_id: input_recipient,
            input: "operator typing".into() }).map(|_| ())).unwrap();
    });
    let before_release = input_rx.recv_timeout(Duration::from_secs(2));
    release_tx.send(()).unwrap();
    save.join().unwrap().unwrap();
    input.join().unwrap();
    before_release.expect("room persistence held the registry needed by another pane").unwrap();
    assert_eq!(inputs.lock().as_slice(), &["operator typing"]);
}

#[test]
fn room_workers_leave_async_executor_free_and_recheck_queued_authority() {
    for replace_run in [false, true] {
        for action in 0..4 {
            let supervisor = test_supervisor();
            let (pty, inputs) = recording_pty_session(std::process::id());
            install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude,
                Some(std::process::id()), pty, BracketedPasteMode::Unknown);
            let member = test_session_id(&supervisor, "claude");
            let room = supervisor.create_room(CreateRoomRequest { label: Some("Queued authority".into()),
                member_ids: vec![member, test_session_id(&supervisor, "codex")],
                brief_on_join: false, brief_template: None }).unwrap();
            supervisor.post_room_message(PostRoomMessageRequest { room_id: room.room_id,
                content: "earlier room history".into(), request_id: None }).unwrap();
            let caller = supervisor.resolve_pane_caller(TestPaneProcess::new(9001, [std::process::id()])).unwrap();
            let request = match action {
                0 => SidebandRequest::RoomRead { owed_after: None, cursor: None, expect_room: None },
                1 => SidebandRequest::RoomPost { content: "must not be stored".into(), request_id: None },
                3 => SidebandRequest::RoomPostCard { request_id: None,
                    card: serde_json::from_value(serde_json::json!({"key":"guard", "kind":"report",
                        "title":"Must not be stored", "summary":"Caller revoked"})).unwrap() },
                _ => SidebandRequest::RoomDeliver { recipient: "all".into(),
                    content: "must not be delivered".into(), request_id: None },
            };
            let (busy_tx, busy_rx) = mpsc::sync_channel(1);
            let (release_tx, release_rx) = mpsc::sync_channel(1);
            let (progress_tx, progress_rx) = mpsc::sync_channel(1);
            let (begin_tx, begin_rx) = mpsc::sync_channel(1);
            let rooms = supervisor.inner.rooms.lock();
            let handle = supervisor.clone();
            let worker = thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_time().max_blocking_threads(1).build().unwrap();
                let blocker = runtime.spawn_blocking(move || {
                    busy_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                });
                // The blocking pool need not run tasks in enqueue order.
                // Occupy its only worker before polling the room request.
                begin_rx.recv().unwrap();
                runtime.block_on(async {
                    let response = handle.apply_sideband_request_async(&caller, request);
                    tokio::pin!(response);
                    let result = tokio::select! {
                        biased;
                        result = &mut response => result,
                        _ = async {
                            // The response future has been polled first. Its
                            // room lock wait must be on the occupied worker pool.
                            progress_tx.send(()).unwrap();
                            std::future::pending::<()>().await;
                        } => unreachable!(),
                    };
                    blocker.await.unwrap();
                    result
                })
            });
            let blocking_ready = busy_rx.recv_timeout(Duration::from_secs(2));
            begin_tx.send(()).unwrap();
            let executor_progressed = if blocking_ready.is_ok() {
                progress_rx.recv_timeout(Duration::from_secs(2))
            } else {
                Err(mpsc::RecvTimeoutError::Timeout)
            };
            drop(rooms);
            if executor_progressed.is_ok() {
                if replace_run {
                    supervisor.inner.slots.lock().get_by_id_mut(member).unwrap().generation += 1;
                } else {
                    supervisor.remove_room_member(RemoveRoomMemberRequest {
                        room_id: room.room_id, session_id: member,
                    }).unwrap();
                }
            }
            let before_release = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
            release_tx.send(()).unwrap();
            let response = worker.join().unwrap();
            blocking_ready.expect("blocking pool worker did not enter its barrier");
            executor_progressed.expect("initial room authorization blocked the async executor");
            assert!(!response.ok, "queued action {action} retained stale authority");
            assert!(response.payload.is_none(), "stale read must return no prior feed");
            assert!(response.message.contains(if replace_run { "stale" } else { "not a room member" }), "{}", response.message);
            assert_eq!(supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap(), before_release);
            assert!(inputs.lock().is_empty());
        }
    }
}

#[test]
fn room_workers_recheck_authority_after_initial_worker_authorization() {
    for action in 0..4 {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(std::process::id());
        install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude,
            Some(std::process::id()), pty, BracketedPasteMode::Unknown);
        let member = test_session_id(&supervisor, "claude");
        let room = supervisor.create_room(CreateRoomRequest { label: Some("Worker recheck".into()),
            member_ids: vec![member, test_session_id(&supervisor, "codex")],
            brief_on_join: false, brief_template: None }).unwrap();
        let caller = supervisor.resolve_pane_caller(TestPaneProcess::new(9001, [std::process::id()])).unwrap();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        *supervisor.inner.sideband_after_initial_authorization.lock() = Some(Box::new(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        }));
        let request = match action {
            0 => SidebandRequest::RoomRead { owed_after: None, cursor: None, expect_room: None },
            1 => SidebandRequest::RoomPost { content: "must not be stored".into(), request_id: None },
                3 => SidebandRequest::RoomPostCard { request_id: None,
                    card: serde_json::from_value(serde_json::json!({"key":"guard", "kind":"report",
                        "title":"Must not be stored", "summary":"Caller revoked"})).unwrap() },
            _ => SidebandRequest::RoomDeliver { recipient: "all".into(),
                content: "must not be delivered".into(), request_id: None },
        };
        let handle = supervisor.clone();
        let worker = thread::spawn(move || handle.apply_sideband_request(&caller, request));
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        supervisor.remove_room_member(RemoveRoomMemberRequest { room_id: room.room_id, session_id: member }).unwrap();
        let before = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
        release_tx.send(()).unwrap();
        let response = worker.join().unwrap();
        assert!(!response.ok && response.payload.is_none());
        assert!(response.message.contains("not a room member"));
        assert_eq!(supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap(), before);
        assert!(inputs.lock().is_empty());
    }
}

#[test]
fn room_pane_deliver_rechecks_sender_run_after_planning_before_commit() {
    let supervisor = test_supervisor();
    let (sender_pty, sender_inputs) = recording_pty_session(std::process::id());
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude,
        Some(std::process::id()), sender_pty, BracketedPasteMode::Unknown);
    let (recipient_pty, recipient_inputs) = recording_pty_session(9191);
    install_mock_room_wake_recipient(&supervisor, "codex", recipient_pty);
    let sender = test_session_id(&supervisor, "claude");
    let recipient = test_session_id(&supervisor, "codex");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Late sender restart".into()),
        member_ids: vec![sender, recipient], brief_on_join: false, brief_template: None }).unwrap();
    let caller = supervisor.resolve_pane_caller(TestPaneProcess::new(9001, [std::process::id()])).unwrap();
    let before = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    supervisor.set_room_delivery_after_preflight_for_tests(move || {
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    let handle = supervisor.clone();
    let delivery = thread::spawn(move || handle.apply_sideband_request(&caller,
        SidebandRequest::RoomDeliver { recipient: "all".into(), content: "stale sender body".into(), request_id: None }));
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    supervisor.inner.slots.lock().get_by_id_mut(sender).unwrap().generation += 1;
    release_tx.send(()).unwrap();
    let response = delivery.join().unwrap();
    assert!(!response.ok && response.payload.is_none());
    assert!(response.message.contains("stale"));
    assert_eq!(supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap(), before);
    assert!(sender_inputs.lock().is_empty());
    assert!(recipient_inputs.lock().is_empty(), "stale sender cannot wake another run");
}
