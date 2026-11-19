fn sample_card(key: &str, pinned: bool) -> shared_types::RoomCardContent {
    shared_types::RoomCardContent { key: key.into(),
        kind: if pinned { shared_types::RoomCardKind::Decision } else { shared_types::RoomCardKind::Report },
        title: "Test result".into(), summary: "Evidence retained".into(), body: String::new(),
        state: shared_types::RoomCardState::Open, data: Default::default() }
}

#[test]
fn cards_replay_latest_independently_of_feed_with_join_floor_and_sender_dedup() {
    let dir = tempfile::tempdir().unwrap();
    let a = Uuid::new_v4(); let b = Uuid::new_v4();
    let definition = definition(Uuid::new_v4(), "Cards", vec![a, b]);
    let mut room = RoomRuntime::from_persisted(definition.clone());
    room.open_log(dir.path()).unwrap();
    let old = room.post_card("a-first".into(), a, sample_card("direction", true)).unwrap().0;
    let other = room.post_card("b-first".into(), b, sample_card("direction", false)).unwrap().0;
    for index in 0..520 { room.post(format!("chatter-{index}"), RoomMessageSender::Operator {}, "chatter".into(), vec![], 1).unwrap(); }
    let mut updated = sample_card("direction", true); updated.summary = "Updated decision".into();
    let latest = room.post_card("a-update".into(), a, updated.clone()).unwrap().0;
    assert_eq!(room.cards.snapshot(0), vec![latest.clone(), other.clone()], "updates keep their identity's position");
    assert_eq!(room.read(None, old.cursor.sequence).unwrap().cards, vec![latest.clone(), other.clone()]);
    assert_eq!(room.read(None, other.cursor.sequence).unwrap().cards, vec![latest.clone()]);
    assert!(!room.read(None, 0).unwrap().events.contains(&old));
    assert!(room.post_card("a-update".into(), a, updated.clone()).unwrap().1);
    let before = room.next_sequence;
    assert!(room.post("a-update".into(), RoomMessageSender::Session { session_id: a }, "collision".into(), vec![], 1).is_err());
    room.post("message-key".into(), RoomMessageSender::Session { session_id: a }, "stored".into(), vec![], 1).unwrap();
    assert!(room.post_card("message-key".into(), a, updated.clone()).is_err());
    assert_eq!(room.next_sequence, before + 1);
    let expected = room.cards.snapshot(0);
    drop(room);
    let mut room = RoomRuntime::from_persisted(definition);
    room.open_log(dir.path()).unwrap();
    assert_eq!(room.cards.snapshot(0), expected);
    assert!(room.post_card("a-update".into(), a, updated).unwrap().1);
    assert_eq!(room.read(None, 0).unwrap().cards_cursor.unwrap().sequence, before);
}

#[test]
fn cards_capacity_pins_open_decisions_and_unpins_done_without_reordering_updates() {
    let author = Uuid::new_v4();
    let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Bounded", vec![author]));
    for index in 0..64 { room.post_card(format!("p{index}"), author, sample_card(&format!("p{index}"), true)).unwrap(); }
    let before = room.cards.snapshot(0);
    assert!(room.post_card("overflow".into(), author, sample_card("new", false)).is_err());
    assert_eq!(room.cards.snapshot(0), before);
    let mut done = sample_card("p3", true); done.state = shared_types::RoomCardState::Done;
    room.post_card("done".into(), author, done).unwrap();
    assert_eq!(card_identity(&room.cards.snapshot(0)[3]), Some((author, "p3")));
    room.post_card("new".into(), author, sample_card("new", false)).unwrap();
    assert_eq!(room.cards.entries.len(), 64);
    assert!(!room.cards.snapshot(0).iter().any(|e| card_identity(e) == Some((author, "p3"))));
    assert_eq!(card_identity(&room.cards.snapshot(0)[0]), Some((author, "p0")));
}

#[test]
fn cards_share_quota_and_uncertain_commit_recovery_without_publishing_unsynced_projection() {
    let dir = tempfile::tempdir().unwrap(); let author = Uuid::new_v4();
    let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Card recovery", vec![author]));
    room.open_log(dir.path()).unwrap();
    room.post_card("initial".into(), author, sample_card("topic", false)).unwrap();
    let before = room.cards.snapshot(0);
    let mut changed = sample_card("topic", false); changed.summary = "Uncertain update".into();
    room.fail_next_commit_for_tests(true);
    assert!(room.post_card("uncertain".into(), author, changed.clone()).is_err());
    assert_eq!(room.cards.snapshot(0), before);
    assert!(room.post_card("blocked".into(), author, sample_card("another", false)).is_err());
    room.recover_poisoned(dir.path()).unwrap();
    assert!(room.post_card("uncertain".into(), author, changed).unwrap().1);
    assert_eq!(room.cards.snapshot(0).len(), 1);
    let committed = room.cards.snapshot(0);
    room.log.as_mut().unwrap().exhaust_quota_for_tests();
    assert!(room.post_card("quota".into(), author, sample_card("new", false)).is_err());
    assert_eq!(room.cards.snapshot(0), committed);
}

#[test]
fn cards_index_enforces_byte_cap_even_below_key_cap() {
    let author = Uuid::new_v4();
    let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Bytes", vec![author]));
    // A PNG header with bounded trailing data exercises the storage cap; this
    // checks header admission, not successful browser image decoding.
    for index in 0..64 {
        let mut card = sample_card(&format!("key{index}"), false);
        card.kind = shared_types::RoomCardKind::Image;
        card.data.png_base64 = Some(format!("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwC{}", "A".repeat(80 * 1024)));
        card.data.alt = Some("Bounded image fixture".into());
        room.post_card(format!("id{index}"), author, card).unwrap();
    }
    assert!(room.cards.entries.iter().map(|(_, n)| n).sum::<usize>() <= ROOM_CARD_INDEX_MAX_BYTES);
    assert!(room.cards.entries.len() < 64);
    assert!(!room.cards.snapshot(0).iter().any(|e| card_identity(e) == Some((author, "key0"))));
    // Exercise the aggregate budget directly at the admitted-event boundary.
    let event = room.cards.entries[0].0.clone();
    assert!(room.cards.victims(&event, ROOM_CARD_INDEX_MAX_BYTES + 1).is_err());
}

#[test]
fn cards_invalid_complete_replay_record_holds_history_without_rewrite() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap(); let author = Uuid::new_v4();
    let definition = definition(Uuid::new_v4(), "Invalid card", vec![author]);
    let mut room = RoomRuntime::from_persisted(definition.clone());
    room.open_log(dir.path()).unwrap();
    let mut event = room.post_card("good".into(), author, sample_card("topic", false)).unwrap().0;
    event.cursor.sequence += 1;
    if let RoomFeedItem::Card { card, request_id, .. } = &mut event.item {
        *request_id = "bad".into(); card.title = "x".repeat(61);
    }
    let path = dir.path().join("rooms").join(format!("{}.jsonl", definition.room_id));
    drop(room);
    let mut bytes = serde_json::to_vec(&serde_json::json!({ "record":"event", "event":event })).unwrap();
    bytes.push(b'\n');
    std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(&bytes).unwrap();
    let original = std::fs::read(&path).unwrap();
    let mut room = RoomRuntime::from_persisted(definition);
    assert!(room.open_log(dir.path()).is_err());
    assert!(room.read(None, 0).is_err());
    assert_eq!(std::fs::read(path).unwrap(), original);
}
