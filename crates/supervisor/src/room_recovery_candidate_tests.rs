// Isolated locked-log fixtures, included in rooms::tests.

#[test]
fn torn_uncertain_message_allows_its_cursor_to_hold_a_proven_failed_repair() {
    for complete_repair in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let member = Uuid::new_v4();
        let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Torn plus repair", vec![member]));
        room.open_log(dir.path()).unwrap();
        let (message, _) = room.post("admitted".into(), RoomMessageSender::Operator {},
            "old attempt".into(), vec![member], 1).unwrap();
        let message_id = room.delivery_result(&message, false).message_id;
        let pending = room.append(retry_receipt(message_id, member, RoomDeliveryStatus::Pending, 0, false)).unwrap();
        let before = room.read(None, 0).unwrap();
        room.fail_next_commit_for_tests(false);
        assert!(room.post("torn-message".into(), RoomMessageSender::Operator {},
            "must not appear".into(), vec![member], 1).is_err());
        let witness = room.log.as_ref().unwrap().uncertain_message().unwrap();
        let terminal = retry_receipt(message_id, member, RoomDeliveryStatus::Written, 7, false);
        assert!(room.append_wake_completion(terminal.clone()).is_err());
        room.receipt_reservations = 1;
        room.retain_receipt_evidence(&message, pending.cursor.sequence, Some(pending.cursor), terminal);
        room.receipt_reservations = 0;
        room.fail_next_commit_for_tests(complete_repair);
        assert!(room.recover_poisoned(dir.path()).is_err());
        assert_eq!(room.read(None, 0).unwrap(), before);
        assert_eq!(room.log.as_ref().unwrap().uncertain_message(), Some(witness));
        let recovered = room.recover_poisoned(dir.path()).unwrap().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].cursor, witness.1);
        assert!(matches!(recovered[0].item, RoomFeedItem::Delivery {
            status: RoomDeliveryStatus::Written, bytes_written: 7, .. }));
        assert!(room.snapshot().writer_recovery.is_none());
        assert_eq!(room.receipt_evidence_count_for_tests(), 0);
        assert!(room.replayed_post("torn-message", &RoomMessageSender::Operator {},
            "must not appear", &[member]).unwrap().is_none());
        assert!(room.recover_poisoned(dir.path()).unwrap().is_none());
    }
}

#[test]
fn room_quota_headroom_repairs_uncertain_message_privately_and_old_unreserved_history_stays_held() {
    for (reserved, repair_complete) in [(true, false), (true, true), (false, false)] {
        let dir = tempfile::tempdir().unwrap();
        let member = Uuid::new_v4();
        let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Quota recovery", vec![member]));
        room.open_log(dir.path()).unwrap();
        room.post("acknowledged".into(), RoomMessageSender::Operator {}, "prefix remains readable".into(), vec![], 1).unwrap();
        let before = room.read(None, 0).unwrap();
        let prepared = room.prepare_append(RoomFeedItem::Message { guest_recipient_labels: Vec::new(), guest_recipient_ids: Vec::new(), ack_requested: false, fallback_origin: None, continuation_origin: None, message_id: Uuid::new_v4(),
            request_id: "at-ordinary-limit".into(), sender: RoomMessageSender::Operator {},
            content: "uncertain addressed body".into(), recipient_ids: vec![member], membership_revision: 1 }).unwrap();
        let encoded_bytes = serde_json::to_vec(&serde_json::json!({ "record": "event", "event": &prepared.event })).unwrap().len() as u64 + 1;
        let log = room.log.as_mut().unwrap();
        let ordinary = log.bytes_for_tests() + encoded_bytes;
        // The old-history case deliberately has no reserve: never pretend that
        // installing the recovered Message before its repair fixes that quota.
        log.set_quota_for_tests(ordinary, ordinary + if reserved { 4 * 1024 * 1024 } else { 1 });
        log.fail_commit_for_tests(true);
        assert!(room.commit_append(prepared).is_err());
        let uncertain_bytes = room.read_owned_log_for_tests();
        assert_eq!(room.read(None, 0).unwrap(), before);
        if reserved { room.fail_next_commit_for_tests(repair_complete); }
        let first = room.recover_poisoned(dir.path()).unwrap_err();
        assert_eq!(room.read(None, 0).unwrap(), before, "repair failure publishes no new cursor or receipt");
        assert!(room.writer_needs_replay());
        if !reserved {
            assert!(format!("{first:#}").contains("quota"));
            assert_eq!(room.read_owned_log_for_tests(), uncertain_bytes);
            assert!(room.snapshot().writer_recovery.unwrap().reason.contains("quota"));
            assert!(room.recover_poisoned(dir.path()).is_err());
            assert_eq!(room.read(None, 0).unwrap(), before);
            assert_eq!(room.read_owned_log_for_tests(), uncertain_bytes);
            continue;
        }
        let recovered = room.recover_poisoned(dir.path()).unwrap().unwrap();
        assert_eq!(recovered.len(), 2, "one original Message and one Held repair");
        assert!(matches!(recovered[1].item, RoomFeedItem::Delivery { status: RoomDeliveryStatus::Held,
            bytes_written: 0, zero_write_confirmed: true, .. }));
        assert!(!room.writer_needs_replay());
        assert!(room.log.as_ref().unwrap().bytes_for_tests() > ordinary, "repair used reserved headroom");
        let after = room.read(None, 0).unwrap();
        assert!(room.post("over-ordinary".into(), RoomMessageSender::Operator {}, "refused".into(), vec![], 1)
            .unwrap_err().to_string().contains("ordinary log quota"));
        assert!(room.record_fetched_page(&after, member, Uuid::new_v4(), 1).is_err());
        assert_eq!(room.read(None, 0).unwrap(), after, "observational receipts cannot spend the reserve");
    }
}

#[test]
fn replay_candidate_rejects_changed_boundary_without_losing_original_writer_or_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let id = Uuid::new_v4();
    let mut room = RoomRuntime::from_persisted(definition(id, "Before replay", vec![]));
    room.open_log(dir.path()).unwrap();
    let before = room.read(None, 0).unwrap();
    room.fail_next_commit_for_tests(true);
    assert!(room.post("held-candidate".into(), RoomMessageSender::Operator {},
        "exact body".into(), vec![], 1).is_err());
    let work = room.prepare_recovery(dir.path(), HashMap::new()).unwrap().unwrap();
    let candidate = work.run().unwrap();
    assert_eq!(room.read(None, 0).unwrap(), before, "private replay cannot expose an unswapped cursor");
    room.definition.label = "Changed boundary".into();
    assert!(room.finish_recovery(candidate).is_err());
    assert_eq!(room.read(None, 0).unwrap(), before);
    assert!(room.writer_needs_replay());
    assert!(room.log.as_ref().unwrap().uncertain_message().is_some());
    let path = dir.path().join("rooms").join(format!("{id}.jsonl"));
    assert!(crate::room_log::RoomLog::open(&path, id, |_| Ok(())).is_err());
    let recovered = room.recover_poisoned(dir.path()).unwrap().unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(room.definition.label, "Changed boundary");
    assert!(room.post("held-candidate".into(), RoomMessageSender::Operator {},
        "exact body".into(), vec![], 1).unwrap().1);
    assert!(crate::room_log::RoomLog::open(&path, id, |_| Ok(())).is_err());
}

#[test]
fn uncertain_message_repairs_are_private_durable_and_idempotent_after_partial_or_full_failure() {
    for after_write in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let members: Vec<_> = (0..3).map(|_| Uuid::new_v4()).collect();
        let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Repair", members.clone()));
        room.open_log(dir.path()).unwrap();
        let before = room.read(None, 0).unwrap();
        room.fail_next_commit_for_tests(true);
        assert!(room.post("uncertain-message".into(), RoomMessageSender::Operator {},
            "preserve original".into(), members.clone(), 1).is_err());
        let hint = room.log.as_ref().unwrap().uncertain_message().unwrap();
        let runs: HashMap<_, _> = members.iter().map(|id| (*id, (Uuid::new_v4(), 7))).collect();
        // This hook is consumed by the repair append, not by read-only replay.
        room.fail_next_commit_for_tests(after_write);
        assert!(room.recover_poisoned_with_runs(dir.path(), &runs).is_err());
        assert_eq!(room.read(None, 0).unwrap(), before, "no premature candidate installation");
        assert_eq!(room.log.as_ref().unwrap().uncertain_message(), Some(hint));
        assert!(room.log.as_ref().unwrap().needs_replay());
        let recovered = room.recover_poisoned_with_runs(dir.path(), &runs).unwrap().unwrap();
        assert_eq!(recovered.len(), 4, "one original Message and exactly one Held per target");
        assert!(recovered.windows(2).all(|pair| pair[1].cursor.sequence == pair[0].cursor.sequence + 1));
        let (message, replayed) = room.post("uncertain-message".into(), RoomMessageSender::Operator {},
            "preserve original".into(), members.clone(), 1).unwrap();
        assert!(replayed);
        let result = room.delivery_result(&message, true);
        assert_eq!(result.written_count, 0);
        for receipt in result.receipts {
            assert_eq!(receipt.status, RoomDeliveryStatus::Held);
            assert_eq!(receipt.bytes_written, 0);
            assert_eq!((receipt.run_id, receipt.generation),
                (Some(runs[&receipt.recipient_id].0), Some(7)));
        }
        assert_eq!(room.log.as_ref().unwrap().uncertain_message(), None);
        assert!(room.recover_poisoned(dir.path()).unwrap().is_none());
        let after = room.read(None, 0).unwrap();
        let definition = room.definition.clone();
        drop(room);
        let mut reopened = RoomRuntime::from_persisted(definition);
        reopened.open_log(dir.path()).unwrap();
        assert_eq!(reopened.read(None, 0).unwrap(), after);
    }
}

#[test]
fn uncertain_message_repair_respects_removal_and_rejoin_floor() {
    let dir = tempfile::tempdir().unwrap();
    let removed = Uuid::new_v4();
    let rejoined = Uuid::new_v4();
    let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Authorization", vec![removed, rejoined]));
    room.open_log(dir.path()).unwrap();
    room.fail_next_commit_for_tests(true);
    assert!(room.post("old-membership".into(), RoomMessageSender::Operator {}, "original".into(),
        vec![removed, rejoined], 1).is_err());
    let (_, cursor) = room.log.as_ref().unwrap().uncertain_message().unwrap();
    room.definition.member_ids = vec![rejoined];
    room.definition.membership_revision += 1;
    room.join_floor_by_session = HashMap::from([(rejoined, cursor.sequence)]);
    room.definition.join_floors = Some(room.join_floor_by_session.clone());
    let recovered = room.recover_poisoned(dir.path()).unwrap().unwrap();
    assert_eq!(recovered.len(), 1);
    assert!(matches!(recovered[0].item, RoomFeedItem::Message { .. }));
    assert!(room.read(None, cursor.sequence).unwrap().events.is_empty());
}

#[test]
fn uncertain_message_receipt_absence_is_proved_from_full_log_not_retained_maps() {
    for status in [RoomDeliveryStatus::Pending, RoomDeliveryStatus::Fetched,
        RoomDeliveryStatus::Written, RoomDeliveryStatus::Failed] {
        let dir = tempfile::tempdir().unwrap();
        let member = Uuid::new_v4();
        let id = Uuid::new_v4();
        let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Full proof", vec![member]));
        room.open_log(dir.path()).unwrap();
        // Conservative imported-history fixture: even evidence preceding the
        // hinted Message suppresses any manufactured zero-write proof.
        room.append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, readmit: false, pointer: None, message_id: id, recipient_id: member,
            status, bytes_written: 0, zero_write_confirmed: false,
            error: None, run_id: None, generation: None }).unwrap();
        for index in 0..ROOM_FEED_MAX_EVENTS {
            room.append(message(index, "unrelated".into())).unwrap();
        }
        assert!(room.feed.iter().all(|(event, _)| !matches!(event.item, RoomFeedItem::Delivery { .. })));
        room.fail_next_commit_for_tests(true);
        assert!(room.append(RoomFeedItem::Message { guest_recipient_labels: Vec::new(), guest_recipient_ids: Vec::new(), ack_requested: false, fallback_origin: None, continuation_origin: None, message_id: id, request_id: "uncertain".into(),
            sender: RoomMessageSender::Operator {}, content: "original".into(),
            recipient_ids: vec![member], membership_revision: 1 }).is_err());
        let recovered = room.recover_poisoned(dir.path()).unwrap().unwrap();
        assert_eq!(recovered.len(), 1, "any full-log receipt forbids a synthetic Held repair");
        assert!(matches!(recovered[0].item, RoomFeedItem::Message { .. }));
    }
}
