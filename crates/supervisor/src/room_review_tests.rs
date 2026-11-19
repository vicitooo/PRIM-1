// Cross-family correction fixtures: isolated temporary catalogs and mock PTYs.

#[test]
fn room_removal_revokes_membership_even_when_history_is_poisoned_full_or_corrupt() {
    for damage in ["poison", "quota", "corrupt"] {
        let root = tempfile::tempdir().unwrap();
        let mut supervisor = test_supervisor_with_root(root.path().to_path_buf());
        let recipient = test_session_id(&supervisor, "claude");
        let other = test_session_id(&supervisor, "codex");
        let room = supervisor.create_room(CreateRoomRequest { label: Some("Removal".into()),
            member_ids: vec![recipient], brief_on_join: false, brief_template: None }).unwrap();
        let path = supervisor.runtime_dir().join("rooms").join(format!("{}.jsonl", room.room_id));
        if damage == "corrupt" {
            drop(supervisor);
            OpenOptions::new().append(true).open(&path).unwrap().write_all(b"invalid complete record\n").unwrap();
            supervisor = SupervisorHandle::new(test_supervisor_config(root.path())).unwrap();
        } else {
            let mut rooms = supervisor.inner.rooms.lock();
            let runtime = rooms.get_mut(room.room_id).unwrap();
            if damage == "quota" { runtime.exhaust_log_quota_for_tests(); }
            else {
                runtime.fail_next_commit_for_tests(true);
                assert!(runtime.append(RoomFeedItem::Membership { action: RoomMembershipAction::Joined,
                    session_id: recipient, membership_revision: room.membership_revision }).is_err());
                runtime.fail_next_replay_sync_for_tests();
            }
        }
        // Add still preflights; no pre-join history can leak through a failed writer.
        let catalog = fs::read(room_catalog_path(supervisor.runtime_dir())).unwrap();
        assert!(supervisor.add_room_member(AddRoomMemberRequest { room_id: room.room_id, session_id: other }).is_err());
        assert_eq!(fs::read(room_catalog_path(supervisor.runtime_dir())).unwrap(), catalog);
        let events = capture_runtime_events(&supervisor);
        let removed = supervisor.remove_room_member(RemoveRoomMemberRequest { room_id: room.room_id, session_id: recipient }).unwrap();
        assert!(!removed.member_ids.contains(&recipient));
        assert!(!supervisor.inner.rooms.lock().room_by_session.contains_key(&recipient));
        assert!(events.lock().iter().any(|event| matches!(event, RuntimeEvent::RoomMemberRemoved { session_id, .. } if *session_id == recipient)));
        assert!(events.lock().iter().any(|event| matches!(event, RuntimeEvent::SystemLog { level: LogLevel::Error, message, .. }
            if message.contains("membership saved; feed record was not confirmed"))));
        supervisor.delete_session(recipient).unwrap();
        let saved: rooms::RoomCatalogV1 = serde_json::from_slice(&fs::read(room_catalog_path(supervisor.runtime_dir())).unwrap()).unwrap();
        assert!(!saved.rooms.iter().find(|r| r.room_id == room.room_id).unwrap().member_ids.contains(&recipient));
    }
}

#[test]
fn room_unsynced_or_quota_refused_claim_releases_only_the_pretransport_gate() {
    for quota in [false, true] {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(std::process::id());
        install_mock_room_wake_recipient(&supervisor, "claude", pty);
        let recipient = test_session_id(&supervisor, "claude");
        let room = supervisor.create_room(CreateRoomRequest { label: Some("Claim failure".into()),
            member_ids: vec![recipient], brief_on_join: false, brief_template: None }).unwrap();
        let hook = supervisor.clone();
        supervisor.set_room_event_after_append_for_tests(move || {
            let mut rooms = hook.inner.rooms.lock();
            let runtime = rooms.get_mut(room.room_id).unwrap();
            if quota { runtime.exhaust_log_quota_for_tests(); }
            else { runtime.fail_next_commit_for_tests(true); }
        });
        let events = capture_runtime_events(&supervisor);
        let result = supervisor.deliver_room_message(DeliverRoomMessageRequest { room_id: room.room_id,
            recipients: RoomRecipientSelection::All {}, content: "first stored intent".into(), request_id: None }).unwrap();
        assert!(!result.receipts[0].recorded);
        assert!(inputs.lock().is_empty());
        let warnings: Vec<_> = events.lock().iter().filter_map(|event| match event {
            RuntimeEvent::SystemLog { message, .. } if message.contains("Room wake receipt") => Some(message.clone()),
            _ => None,
        }).collect();
        assert!(warnings.iter().any(|message| message.contains(if quota {
            "Receipt storage was refused before append"
        } else { "Receipt commit is uncertain" })));
        assert!(warnings.iter().all(|message| !message.contains("history replays before the next")));
        let gate = supervisor.inner.slots.lock().get_by_id(recipient).unwrap().running.as_ref().unwrap().input_gate.clone();
        assert!(!gate.room_wake_pending.load(Ordering::SeqCst), "no transport means no sticky run hold");
        supervisor.remove_room_member(RemoveRoomMemberRequest { room_id: room.room_id, session_id: recipient }).unwrap();
        let new_room = supervisor.create_room(CreateRoomRequest { label: Some("Healthy room".into()),
            member_ids: vec![recipient], brief_on_join: false, brief_template: None }).unwrap();
        let next = supervisor.deliver_room_message(DeliverRoomMessageRequest { room_id: new_room.room_id,
            recipients: RoomRecipientSelection::All {}, content: "new intent, same run".into(), request_id: None }).unwrap();
        assert_eq!(next.receipts[0].status, RoomDeliveryStatus::Written);
        assert_eq!(inputs.lock().len(), 2);
    }
}

#[test]
fn room_routed_refusal_keeps_dash_in_label_and_reason_before_and_after_pending() {
    for after_pending in [false, true] {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(std::process::id());
        if after_pending {
            install_mock_room_wake_recipient(&supervisor, "claude", pty);
        } else {
            install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Grok,
                Some(std::process::id()), pty, BracketedPasteMode::Unknown);
            // A106: before Pending the refusal only queues the message; its
            // reason is recorded when the wait limit holds it.
            supervisor.set_room_wake_max_wait_for_tests(Duration::ZERO);
        }
        let member = test_session_id(&supervisor, "claude");
        let label = "Reviewer \u{2014} east";
        supervisor.inner.slots.lock().get_by_id_mut(member).unwrap().definition.label = label.into();
        let room = supervisor.create_room(CreateRoomRequest { label: Some("Structured refusal".into()),
            member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap();
        if after_pending {
            let inner = Arc::downgrade(&supervisor.inner);
            supervisor.set_event_sink(move |event| {
                if matches!(event, RuntimeEvent::RoomFeedEvent { feed_event: shared_types::RoomFeedEvent {
                    item: RoomFeedItem::Delivery { status: RoomDeliveryStatus::Pending, .. }, .. } }) {
                    let inner = inner.upgrade().unwrap();
                    let mut slots = inner.slots.lock();
                    let slot = slots.get_by_id_mut(member).unwrap();
                    slot.work_state = WorkState::Blocked;
                    slot.work_state_observed = true;
                    slot.work_detail = Some("trust_prompt".into());
                }
            });
        }
        let result = supervisor.deliver_room_message(DeliverRoomMessageRequest { room_id: room.room_id,
            request_id: None, recipients: RoomRecipientSelection::All {}, content: "already stored".into() }).unwrap();
        let (status, reason, bytes_written) = if after_pending {
            let receipt = &result.receipts[0];
            (receipt.status, receipt.error.clone().unwrap(), receipt.bytes_written)
        } else {
            assert_eq!(result.receipts[0].status, RoomDeliveryStatus::Stored);
            let held = wait_for_room_receipt(&supervisor, room.room_id, result.message_id, member, RoomDeliveryStatus::Held);
            let RoomFeedItem::Delivery { status, error, bytes_written, .. } = held.item else { unreachable!() };
            (status, error.unwrap(), bytes_written)
        };
        assert_eq!(status, if after_pending { RoomDeliveryStatus::Failed } else { RoomDeliveryStatus::Held });
        assert!(reason.contains(label), "label was truncated: {reason}");
        assert!(reason.contains(if after_pending { "trust" } else { "has not yet confirmed" }), "reason lost: {reason}");
        assert!(!reason.contains("send again"), "stored-message guidance still asks to repost: {reason}");
        assert_eq!(bytes_written, 0);
        assert!(inputs.lock().is_empty());
    }
}

#[test]
fn room_permanent_adapter_holds_refuse_retry_without_appending_another_receipt() {
    // A106: Claude and Codex now have the live adapter; Prime and Terminal
    // panes remain the permanent holds.
    for driver in [DriverKind::Prime, DriverKind::GenericTerminal] {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(std::process::id());
        install_mock_running_session_with_bracketed_paste_enabled(&supervisor, "claude", driver, pty);
        let recipient = test_session_id(&supervisor, "claude");
        let room = supervisor.create_room(CreateRoomRequest { label: Some("Unsupported".into()),
            member_ids: vec![recipient], brief_on_join: false, brief_template: None }).unwrap();
        let result = supervisor.deliver_room_message(DeliverRoomMessageRequest { room_id: room.room_id,
            recipients: RoomRecipientSelection::All {}, content: "stored for room_read".into(), request_id: None }).unwrap();
        let page = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
        let refusal = supervisor.retry_room_delivery(shared_types::RetryRoomDeliveryRequest { room_id: room.room_id,
            message_id: result.message_id, recipient_id: recipient, expected_receipt: page.events.last().unwrap().cursor }).unwrap_err();
        assert_eq!(result.receipts[0].status, RoomDeliveryStatus::Held);
        assert!(refusal.to_string().contains("provide context manually"));
        assert_eq!(supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap(), page);
        assert_eq!(supervisor.queued_room_wake_count_for_tests(recipient), 0, "a refused retry never queues");
        assert!(inputs.lock().is_empty());
    }
}

#[test]
fn room_refusal_reasons_never_instruct_reposting_a_stored_message() {
    for (state, detail, mode) in [
        (WorkState::Idle, None, BracketedPasteMode::Unknown),
        (WorkState::Idle, None, BracketedPasteMode::Disabled),
        (WorkState::Blocked, Some("trust_prompt"), BracketedPasteMode::Enabled),
        (WorkState::Blocked, Some("login_required"), BracketedPasteMode::Enabled),
        (WorkState::Blocked, Some("menu"), BracketedPasteMode::Enabled),
        (WorkState::ErrorLoop, None, BracketedPasteMode::Enabled),
        (WorkState::Exited, None, BracketedPasteMode::Enabled),
    ] {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(std::process::id());
        install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Grok,
            Some(std::process::id()), pty, mode);
        let recipient = test_session_id(&supervisor, "claude");
        {
            let mut slots = supervisor.inner.slots.lock();
            let slot = slots.get_by_id_mut(recipient).unwrap();
            slot.state = LifecycleState::Idle;
            slot.work_state = state;
            slot.work_state_observed = true;
            slot.work_detail = detail.map(str::to_owned);
        }
        let room = supervisor.create_room(CreateRoomRequest { label: Some("Refusal reason".into()),
            member_ids: vec![recipient], brief_on_join: false, brief_template: None }).unwrap();
        // A106: these refusals queue the message; the reason is recorded only
        // when the wait limit holds it.
        supervisor.set_room_wake_max_wait_for_tests(Duration::ZERO);
        let result = supervisor.deliver_room_message(DeliverRoomMessageRequest { room_id: room.room_id,
            recipients: RoomRecipientSelection::All {}, content: "one stored body".into(), request_id: None }).unwrap();
        // With a zero wait limit the dispatcher may already have held the
        // queued wake when the first result reads its receipt (90511a9 suite,
        // once under load): Stored or Held, never typed.
        assert!(matches!(result.receipts[0].status, RoomDeliveryStatus::Stored | RoomDeliveryStatus::Held),
            "{state:?} {detail:?} {mode:?}: {:?}", result.receipts[0].status);
        let held = wait_for_room_receipt(&supervisor, room.room_id, result.message_id, recipient, RoomDeliveryStatus::Held);
        let RoomFeedItem::Delivery { error, .. } = held.item else { unreachable!() };
        let text = error.unwrap();
        assert!(!text.contains("send again") && !text.contains("send it again"), "{text}");
        assert!(text.contains("The message is stored"), "{text}");
        assert!(inputs.lock().is_empty());
    }
}

#[test]
fn room_refusal_after_the_paste_never_instructs_reposting_the_stored_message() {
    let supervisor = test_supervisor();
    let inputs = install_prompt_after_paste_recipient(&supervisor, "claude");
    let recipient = test_session_id(&supervisor, "claude");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Refusal after paste".into()),
        member_ids: vec![recipient], brief_on_join: false, brief_template: None }).unwrap();
    let result = supervisor.deliver_room_message(DeliverRoomMessageRequest { room_id: room.room_id,
        recipients: RoomRecipientSelection::All {}, content: "one stored body".into(), request_id: None }).unwrap();
    assert_eq!(result.receipts[0].status, RoomDeliveryStatus::Failed);
    assert_eq!(inputs.lock().len(), 1, "the paste, never the submit");
    let text = result.receipts[0].error.as_deref().unwrap();
    assert!(text.contains("asking for approval"), "{text}");
    assert!(!text.contains("send again") && !text.contains("send it again"), "{text}");
}

#[test]
fn room_pane_storage_transitions_invalidate_snapshot_once_without_feed_noise() {
    for fetching in [false, true] {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(std::process::id());
        install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude,
            Some(std::process::id()), pty, BracketedPasteMode::Unknown);
        let recipient = test_session_id(&supervisor, "claude");
        let room = supervisor.create_room(CreateRoomRequest { label: Some("Pane replay state".into()),
            member_ids: vec![recipient], brief_on_join: false, brief_template: None }).unwrap();
        let caller = supervisor.resolve_pane_caller(TestPaneProcess::new(9001, [std::process::id()])).unwrap();
        if fetching {
            supervisor.deliver_room_message(DeliverRoomMessageRequest { room_id: room.room_id,
                recipients: RoomRecipientSelection::All {}, content: "fetch body".into(), request_id: None }).unwrap();
        }
        let events = capture_runtime_events(&supervisor);
        supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().fail_next_commit_for_tests(true);
        if fetching {
            let page = supervisor.read_room_feed_from_pane(&caller, None).unwrap();
            assert!(page.events.iter().any(|event| matches!(&event.item, RoomFeedItem::Message { content, .. } if content == "fetch body")));
            // A second failed receipt append is not another poison transition.
            supervisor.read_room_feed_from_pane(&caller, None).unwrap();
        } else {
            assert!(room_commit_is_uncertain(&supervisor.post_room_message_from_pane(&caller,
                "pane body".into(), Some("pane-retry".into())).unwrap_err()));
        }
        assert!(supervisor.snapshot().rooms.iter().find(|r| r.room_id == room.room_id).unwrap().history_replay_pending);
        assert_eq!(events.lock().iter().filter(|event| matches!(event, RuntimeEvent::RoomHistoryChanged { .. })).count(), 1);
        supervisor.post_room_message_from_pane(&caller, "pane body".into(), Some("pane-retry".into())).unwrap();
        assert!(!supervisor.snapshot().rooms.iter().find(|r| r.room_id == room.room_id).unwrap().history_replay_pending);
        assert_eq!(events.lock().iter().filter(|event| matches!(event, RuntimeEvent::RoomHistoryChanged { .. })).count(), 2);
        assert!(inputs.lock().is_empty());
    }
}
