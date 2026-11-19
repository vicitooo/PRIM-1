// Temporary room catalogs and recording PTYs only. No installed harness.

#[test]
fn room_torn_own_pending_repairs_retry_proof_but_complete_pending_stays_held() {
    for complete_pending in [false, true] {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(std::process::id());
        install_mock_room_wake_recipient(&supervisor, "claude", pty);
        let member = test_session_id(&supervisor, "claude");
        let room = supervisor.create_room(CreateRoomRequest { label: Some("Own uncertain claim".into()),
            member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap();
        let inner = Arc::downgrade(&supervisor.inner);
        supervisor.set_room_event_after_append_for_tests(move || {
            inner.upgrade().unwrap().rooms.lock().get_mut(room.room_id).unwrap()
                .fail_next_commit_for_tests(complete_pending);
        });
        let request = DeliverRoomMessageRequest { room_id: room.room_id,
            request_id: Some("own-claim".into()), recipients: RoomRecipientSelection::All {}, content: "stored body".into() };
        let first = supervisor.deliver_room_message(request.clone()).unwrap();
        assert!(!first.receipts[0].recorded);
        assert!(inputs.lock().is_empty());
        assert_eq!(supervisor.inner.rooms.lock().get(room.room_id).unwrap().receipt_evidence_count_for_tests(), 1);
        let replay = supervisor.deliver_room_message(request).unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.receipts[0].status, if complete_pending { RoomDeliveryStatus::Pending } else { RoomDeliveryStatus::Held });
        assert!(replay.receipts[0].recorded);
        assert!(inputs.lock().is_empty(), "history recovery never attempts transport");
        let page = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
        let cursor = page.events.last().unwrap().cursor;
        let retry = supervisor.retry_room_delivery(shared_types::RetryRoomDeliveryRequest {
            room_id: room.room_id, message_id: first.message_id, recipient_id: member, expected_receipt: cursor });
        if complete_pending {
            assert!(retry.unwrap_err().to_string().contains("cannot be safely retried"));
            assert!(inputs.lock().is_empty());
        } else {
            assert_eq!(retry.unwrap().written_count, 1);
            assert_eq!(inputs.lock().len(), 2, "only explicit retry crosses transport");
        }
        assert_eq!(supervisor.inner.rooms.lock().get(room.room_id).unwrap().receipt_evidence_count_for_tests(), 0);
    }
}

#[test]
fn room_removal_recovers_known_outcome_before_member_moves_without_repeating_wake() {
    let supervisor = test_supervisor();
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_room_wake_recipient(&supervisor, "claude", pty);
    let member = test_session_id(&supervisor, "claude");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Old room".into()),
        member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap();
    let inner = Arc::downgrade(&supervisor.inner);
    let fault = AtomicBool::new(true);
    supervisor.set_event_sink(move |event| {
        if matches!(event, RuntimeEvent::RoomFeedEvent { feed_event: shared_types::RoomFeedEvent {
            item: RoomFeedItem::Delivery { status: RoomDeliveryStatus::Pending, .. }, .. } })
            && fault.swap(false, Ordering::SeqCst) {
            inner.upgrade().unwrap().rooms.lock().get_mut(room.room_id).unwrap().fail_next_commit_for_tests(false);
        }
    });
    supervisor.deliver_room_message(DeliverRoomMessageRequest { room_id: room.room_id, request_id: None,
        recipients: RoomRecipientSelection::All {}, content: "known completion".into() }).unwrap();
    let gate = supervisor.inner.slots.lock().get_by_id(member).unwrap().running.as_ref().unwrap().input_gate.clone();
    assert!(gate.room_wake_pending.load(Ordering::SeqCst));
    let removed = supervisor.remove_room_member(RemoveRoomMemberRequest { room_id: room.room_id, session_id: member }).unwrap();
    assert!(!removed.member_ids.contains(&member));
    assert!(!removed.history_replay_pending);
    assert!(!gate.room_wake_pending.load(Ordering::SeqCst));
    assert_eq!(inputs.lock().len(), 2);
    let new_room = supervisor.create_room(CreateRoomRequest { label: Some("New room".into()),
        member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap();
    let next = supervisor.deliver_room_message(DeliverRoomMessageRequest { room_id: new_room.room_id, request_id: None,
        recipients: RoomRecipientSelection::All {}, content: "new independent intent".into() }).unwrap();
    assert_eq!(next.written_count, 1);
    assert_eq!(inputs.lock().len(), 4);
}

#[test]
fn room_missing_completion_headroom_refuses_before_storing_or_waking() {
    let supervisor = test_supervisor();
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_room_wake_recipient(&supervisor, "claude", pty);
    let member = test_session_id(&supervisor, "claude");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Legacy quota".into()),
        member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap();
    let before = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
    let bytes = {
        let mut rooms = supervisor.inner.rooms.lock();
        let runtime = rooms.get_mut(room.room_id).unwrap();
        let bytes = runtime.read_owned_log_for_tests();
        runtime.set_log_quotas_for_tests(bytes.len() as u64, bytes.len() as u64 + 1);
        bytes
    };
    let error = supervisor.deliver_room_message(DeliverRoomMessageRequest { room_id: room.room_id,
        request_id: Some("no-headroom".into()), recipients: RoomRecipientSelection::All {}, content: "not stored".into() }).unwrap_err();
    assert!(error.to_string().contains("receipt headroom is unavailable"));
    assert!(!room_commit_is_uncertain(&error));
    assert_eq!(supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap(), before);
    let mut rooms = supervisor.inner.rooms.lock();
    let runtime = rooms.get_mut(room.room_id).unwrap();
    assert_eq!(runtime.read_owned_log_for_tests(), bytes);
    assert_eq!(runtime.receipt_reservations, 0);
    assert_eq!(runtime.receipt_evidence_count_for_tests(), 0);
    assert!(inputs.lock().is_empty());
}

#[test]
fn room_interrupted_send_repairs_remaining_recipients_without_repeating_success() {
    for repair_complete in [false, true] {
        let supervisor = test_supervisor();
        create_test_session(&supervisor, "third", DriverKind::Grok, shared_types::PermissionProfile::Normal);
        let mut inputs = Vec::new();
        let members: Vec<_> = ["claude", "codex", "third"].iter().map(|name| {
            let (pty, recorded) = recording_pty_session(std::process::id());
            install_mock_room_wake_recipient(&supervisor, name, pty);
            inputs.push(recorded);
            test_session_id(&supervisor, name)
        }).collect();
        let room = supervisor.create_room(CreateRoomRequest { label: Some("Interrupted fanout".into()),
            member_ids: members.clone(), brief_on_join: false, brief_template: None }).unwrap();
        let inner = Arc::downgrade(&supervisor.inner);
        let fault = AtomicBool::new(true);
        supervisor.set_event_sink(move |event| {
            if matches!(event, RuntimeEvent::RoomFeedEvent { feed_event: shared_types::RoomFeedEvent {
                item: RoomFeedItem::Delivery { status: RoomDeliveryStatus::Pending, .. }, .. } })
                && fault.swap(false, Ordering::SeqCst) {
                inner.upgrade().unwrap().rooms.lock().get_mut(room.room_id).unwrap().fail_next_commit_for_tests(false);
            }
        });
        let request = DeliverRoomMessageRequest { room_id: room.room_id, request_id: Some("fanout-original".into()),
            recipients: RoomRecipientSelection::All {}, content: "one original body".into() };
        let first = supervisor.deliver_room_message(request.clone()).unwrap();
        assert!(first.receipts.iter().all(|receipt| !receipt.recorded));
        assert_eq!(inputs.iter().map(|input| input.lock().len()).collect::<Vec<_>>(), vec![2, 0, 0]);
        {
            let mut rooms = supervisor.inner.rooms.lock();
            let runtime = rooms.get_mut(room.room_id).unwrap();
            assert_eq!(runtime.receipt_reservations, 0);
            assert_eq!(runtime.receipt_evidence_count_for_tests(), 3);
            runtime.fail_next_commit_for_tests(repair_complete);
        }
        assert!(supervisor.deliver_room_message(request.clone()).is_err(), "failed repair cannot publish evidence");
        assert_eq!(supervisor.inner.rooms.lock().get(room.room_id).unwrap().receipt_evidence_count_for_tests(), 3);
        let repaired = supervisor.deliver_room_message(request.clone()).unwrap();
        assert!(repaired.replayed);
        assert_eq!(repaired.receipts.iter().map(|receipt| receipt.status).collect::<Vec<_>>(),
            vec![RoomDeliveryStatus::Written, RoomDeliveryStatus::Held, RoomDeliveryStatus::Held]);
        assert!(repaired.receipts.iter().all(|receipt| receipt.recorded));
        assert_eq!(inputs.iter().map(|input| input.lock().len()).collect::<Vec<_>>(), vec![2, 0, 0]);
        let page = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
        let held = page.events.iter().find(|event| matches!(event.item, RoomFeedItem::Delivery {
            recipient_id, status: RoomDeliveryStatus::Held, .. } if recipient_id == members[1])).unwrap();
        supervisor.retry_room_delivery(shared_types::RetryRoomDeliveryRequest { room_id: room.room_id,
            message_id: repaired.message_id, recipient_id: members[1], expected_receipt: held.cursor }).unwrap();
        assert_eq!(inputs.iter().map(|input| input.lock().len()).collect::<Vec<_>>(), vec![2, 2, 0]);
        supervisor.deliver_room_message(request).unwrap();
        assert_eq!(inputs.iter().map(|input| input.lock().len()).collect::<Vec<_>>(), vec![2, 2, 0]);
        let runtime = supervisor.inner.rooms.lock();
        let runtime = runtime.get(room.room_id).unwrap();
        assert_eq!(runtime.receipt_evidence_count_for_tests(), 0);
        assert_eq!(runtime.receipt_reservations, 0);
        runtime.ensure_receipt_evidence_capacity(64).unwrap();
    }
}

#[test]
fn room_refused_terminal_receipt_repairs_after_removal_and_failed_repair_without_retargeting() {
    for (replaced, zero_write) in [(false, false), (true, false), (false, true)] {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(std::process::id());
        install_mock_room_wake_recipient(&supervisor, "claude", pty);
        let member = test_session_id(&supervisor, "claude");
        let room = supervisor.create_room(CreateRoomRequest { label: Some("Historical receipt".into()),
            member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap();
        let old_gate = supervisor.inner.slots.lock().get_by_id(member).unwrap().running.as_ref().unwrap().input_gate.clone();
        let inner = Arc::downgrade(&supervisor.inner);
        let fault = AtomicBool::new(true);
        supervisor.set_event_sink(move |event| {
            if let RuntimeEvent::RoomFeedEvent { feed_event: shared_types::RoomFeedEvent {
                item: RoomFeedItem::Delivery { message_id, status: RoomDeliveryStatus::Pending, run_id, generation, .. }, .. } } = event {
                if fault.swap(false, Ordering::SeqCst) {
                    let inner = inner.upgrade().unwrap();
                    let mut rooms = inner.rooms.lock();
                    let runtime = rooms.get_mut(room.room_id).unwrap();
                    runtime.fail_next_commit_for_tests(true);
                    assert!(runtime.append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, readmit: false, pointer: None, message_id, recipient_id: member,
                        status: RoomDeliveryStatus::Fetched, bytes_written: 0, zero_write_confirmed: false,
                        error: None, run_id, generation }).is_err());
                    if zero_write {
                        inner.slots.lock().get_by_id(member).unwrap().running.as_ref().unwrap()
                            .input_gate.room_input_activity.lock().record();
                    }
                }
            }
        });
        let first = supervisor.deliver_room_message(DeliverRoomMessageRequest { room_id: room.room_id,
            request_id: Some("historical-outcome".into()), recipients: RoomRecipientSelection::All {}, content: "original".into() }).unwrap();
        assert!(!first.receipts[0].recorded);
        assert_eq!(first.receipts[0].status, if zero_write { RoomDeliveryStatus::Failed } else { RoomDeliveryStatus::Written });
        assert!(old_gate.room_wake_pending.load(Ordering::SeqCst));
        // Removal now attempts recovery but remains available if that fails.
        supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().fail_next_replay_sync_for_tests();
        supervisor.remove_room_member(RemoveRoomMemberRequest { room_id: room.room_id, session_id: member }).unwrap();
        if replaced {
            let mut slots = supervisor.inner.slots.lock();
            let slot = slots.get_by_id_mut(member).unwrap();
            slot.run_id = Some(Uuid::new_v4());
            slot.generation += 1;
            let gate = Arc::new(RunInputGate::new());
            gate.room_wake_pending.store(true, Ordering::SeqCst);
            *gate.room_wake_claim.lock() = Some((Uuid::new_v4(), first.cursor));
            slot.running.as_mut().unwrap().input_gate = gate;
        }
        supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().fail_next_commit_for_tests(true);
        let trigger = PostRoomMessageRequest { room_id: room.room_id, request_id: Some("after-repair".into()), content: "operator post".into() };
        assert!(supervisor.post_room_message(trigger.clone()).is_err());
        assert!(old_gate.room_wake_pending.load(Ordering::SeqCst));
        supervisor.post_room_message(trigger).unwrap();
        let gate = supervisor.inner.slots.lock().get_by_id(member).unwrap().running.as_ref().unwrap().input_gate.clone();
        assert_eq!(gate.room_wake_pending.load(Ordering::SeqCst), replaced);
        assert_eq!(inputs.lock().len(), if zero_write { 0 } else { 2 });
        let page = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
        let terminal: Vec<_> = page.events.iter().filter(|event| matches!(event.item, RoomFeedItem::Delivery {
            message_id, status: RoomDeliveryStatus::Written | RoomDeliveryStatus::Failed, .. } if message_id == first.message_id)).collect();
        assert_eq!(terminal.len(), 1, "historical terminal outcome is durable once despite membership removal");
        assert!(matches!(terminal[0].item, RoomFeedItem::Delivery { zero_write_confirmed: false, .. }),
            "full replay Fetched suppresses retry proof, including after failed repair sync");
        assert_eq!(supervisor.inner.rooms.lock().get(room.room_id).unwrap().receipt_evidence_count_for_tests(), 0);
    }
}

#[test]
fn room_evidence_capacity_refuses_new_effects_but_preserves_existing_intent_replay_and_releases() {
    let supervisor = test_supervisor();
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_room_wake_recipient(&supervisor, "claude", pty);
    let member = test_session_id(&supervisor, "claude");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Capacity".into()),
        member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap();
    let request = DeliverRoomMessageRequest { room_id: room.room_id, request_id: Some("capacity-intent".into()),
        recipients: RoomRecipientSelection::All {}, content: "bounded effects".into() };
    let original = supervisor.deliver_room_message(request.clone()).unwrap();
    let before = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
    supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().receipt_reservations = 64;
    let replay = supervisor.deliver_room_message(request.clone()).unwrap();
    assert!(replay.replayed && replay.message_id == original.message_id);
    let mut another = request;
    another.request_id = Some("new-over-capacity".into());
    assert!(supervisor.deliver_room_message(another.clone()).unwrap_err().to_string().contains("evidence capacity"));
    assert_eq!(supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap(), before);
    assert_eq!(inputs.lock().len(), 2);
    supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().receipt_reservations = 0;
    supervisor.deliver_room_message(another).unwrap();
    let rooms = supervisor.inner.rooms.lock();
    let runtime = rooms.get(room.room_id).unwrap();
    assert_eq!(runtime.receipt_reservations, 0);
    assert_eq!(runtime.receipt_evidence_count_for_tests(), 0);
    runtime.ensure_receipt_evidence_capacity(64).unwrap();
    assert_eq!(inputs.lock().len(), 4);
}

#[test]
fn room_delete_preserves_unsaved_outcomes_until_repair_without_repeating_transport() {
    let supervisor = test_supervisor();
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_room_wake_recipient(&supervisor, "claude", pty);
    let member = test_session_id(&supervisor, "claude");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Delete after repair".into()),
        member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap();
    let inner = Arc::downgrade(&supervisor.inner);
    let fault = AtomicBool::new(true);
    supervisor.set_event_sink(move |event| {
        if matches!(event, RuntimeEvent::RoomFeedEvent { feed_event: shared_types::RoomFeedEvent {
            item: RoomFeedItem::Delivery { status: RoomDeliveryStatus::Pending, .. }, .. } })
            && fault.swap(false, Ordering::SeqCst) {
            inner.upgrade().unwrap().rooms.lock().get_mut(room.room_id).unwrap().fail_next_commit_for_tests(false);
        }
    });
    supervisor.deliver_room_message(DeliverRoomMessageRequest { room_id: room.room_id, request_id: None,
        recipients: RoomRecipientSelection::All {}, content: "known completed wake".into() }).unwrap();
    let gate = supervisor.inner.slots.lock().get_by_id(member).unwrap().running.as_ref().unwrap().input_gate.clone();
    assert!(gate.room_wake_pending.load(Ordering::SeqCst));
    let path = supervisor.runtime_dir().join("rooms").join(format!("{}.jsonl", room.room_id));
    supervisor.inner.rooms.lock().get_mut(room.room_id).unwrap().fail_next_replay_sync_for_tests();
    let error = supervisor.delete_room(DeleteRoomRequest { room_id: room.room_id }).unwrap_err();
    assert!(error.to_string().contains("Room not deleted") && error.to_string().contains("injected room replay sync failure"));
    assert!(path.exists());
    assert_eq!(supervisor.inner.rooms.lock().get(room.room_id).unwrap().receipt_evidence_count_for_tests(), 1);
    assert!(gate.room_wake_pending.load(Ordering::SeqCst));
    supervisor.delete_room(DeleteRoomRequest { room_id: room.room_id }).unwrap();
    assert!(!path.exists());
    assert!(supervisor.inner.rooms.lock().get(room.room_id).is_none());
    assert!(!gate.room_wake_pending.load(Ordering::SeqCst));
    assert_eq!(inputs.lock().len(), 2);
}

#[test]
fn room_denied_claim_repair_never_restores_a_removed_members_retry_right() {
    let supervisor = test_supervisor();
    let mut inputs = Vec::new();
    let members: Vec<_> = ["claude", "codex"].iter().map(|name| {
        let (pty, recorded) = recording_pty_session(std::process::id());
        install_mock_room_wake_recipient(&supervisor, name, pty);
        inputs.push(recorded);
        test_session_id(&supervisor, name)
    }).collect();
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Revoked retry".into()),
        member_ids: members.clone(), brief_on_join: false, brief_template: None }).unwrap();
    let inner = Arc::downgrade(&supervisor.inner);
    let fault = AtomicBool::new(true);
    supervisor.set_event_sink(move |event| {
        if matches!(event, RuntimeEvent::RoomFeedEvent { feed_event: shared_types::RoomFeedEvent {
            item: RoomFeedItem::Delivery { status: RoomDeliveryStatus::Pending, .. }, .. } })
            && fault.swap(false, Ordering::SeqCst) {
            inner.upgrade().unwrap().rooms.lock().get_mut(room.room_id).unwrap().fail_next_commit_for_tests(false);
        }
    });
    let result = supervisor.deliver_room_message(DeliverRoomMessageRequest { room_id: room.room_id,
        request_id: None, recipients: RoomRecipientSelection::All {}, content: "before revocation".into() }).unwrap();
    supervisor.remove_room_member(RemoveRoomMemberRequest { room_id: room.room_id, session_id: members[1] }).unwrap();
    {
        let _publication = supervisor.inner.room_event_publish.lock();
        supervisor.recover_room_writer(room.room_id).unwrap();
    }
    supervisor.add_room_member(AddRoomMemberRequest { room_id: room.room_id, session_id: members[1] }).unwrap();
    let page = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room.room_id, cursor: None }).unwrap();
    assert!(!page.events.iter().any(|event| matches!(event.item, RoomFeedItem::Delivery {
        message_id, recipient_id, status: RoomDeliveryStatus::Held, .. }
        if message_id == result.message_id && recipient_id == members[1])));
    assert!(supervisor.retry_room_delivery(shared_types::RetryRoomDeliveryRequest { room_id: room.room_id,
        message_id: result.message_id, recipient_id: members[1], expected_receipt: result.cursor })
        .unwrap_err().to_string().contains("predates"));
    assert_eq!(inputs[0].lock().len(), 2);
    assert!(inputs[1].lock().is_empty());
}
