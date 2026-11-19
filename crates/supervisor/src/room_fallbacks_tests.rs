// A111 uses real catalog/delivery paths, mock PTYs and an injected clock only.
use shared_types::{RoomFallbackAction, RoomFallbackMemberOverride, RoomFallbackOverrideMode,
    RoomFallbackRecipientMode, RoomFallbackSettings, SetRoomFallbacksRequest};

fn fallback_settings(minutes: u32) -> RoomFallbackSettings {
    RoomFallbackSettings { enabled: true, interval_minutes: minutes, message: "Continue the existing task.".into(), ..Default::default() }
}

fn fallback_fixture(ready: bool) -> (SupervisorHandle, SessionId, RoomId, Arc<Mutex<Vec<String>>>) {
    let supervisor = test_supervisor();
    let member = test_session_id(&supervisor, "claude");
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Fallback room".into()),
        member_ids: vec![member], brief_on_join: false, brief_template: None }).unwrap().room_id;
    let (pty, inputs) = recording_pty_session(9921);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude, None, pty,
        if ready { BracketedPasteMode::Enabled } else { BracketedPasteMode::Unknown });
    (supervisor, member, room, inputs)
}

// Save arms the next due time from its own clock, so a test anchors its tick clock
// before Save: an anchor taken after it races the tick one second before the boundary.
fn fallback_save(supervisor: &SupervisorHandle, room: RoomId, settings: RoomFallbackSettings) -> shared_types::RoomFallbackSnapshot {
    let revision = supervisor.get_room_fallbacks(room).unwrap().revision;
    supervisor.set_room_fallbacks(SetRoomFallbacksRequest { room_id: room, revision, settings }).unwrap()
}

fn fallback_posts(supervisor: &SupervisorHandle, room: RoomId) -> Vec<shared_types::RoomFeedEvent> {
    supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor: None }).unwrap().events.into_iter()
        .filter(|event| matches!(event.item, RoomFeedItem::Message { fallback_origin: Some(_), .. })).collect()
}

fn fallback_tick(supervisor: &SupervisorHandle, clock: &mut room_fallbacks::FallbackClock,
    base: DateTime<Utc>, mono: Instant, seconds: u64) {
    supervisor.tick_room_fallbacks(clock, base + chrono::Duration::seconds(seconds as i64), mono + Duration::from_secs(seconds));
}

#[test]
fn fallbacks_intervals_are_native_future_only_and_frame_the_literal_message() {
    for minutes in [15, 30, 60] {
        let (supervisor, member, room, inputs) = fallback_fixture(true);
        supervisor.inner.slots.lock().get_by_id_mut(member).unwrap().work_state = WorkState::Thinking;
        let mut clock = room_fallbacks::FallbackClock::default(); let now = Utc::now(); let mono = Instant::now();
        let saved = fallback_save(&supervisor, room, fallback_settings(minutes));
        assert!(saved.notice_warning.is_none());
        fallback_tick(&supervisor, &mut clock, now, mono, 0);
        fallback_tick(&supervisor, &mut clock, now, mono, u64::from(minutes) * 60 - 1);
        assert!(inputs.lock().is_empty());
        fallback_tick(&supervisor, &mut clock, now, mono, u64::from(minutes) * 60);
        let posts = fallback_posts(&supervisor, room);
        assert_eq!(posts.len(), 1);
        let RoomFeedItem::Message { fallback_origin: Some(origin), content, ack_requested, .. } = &posts[0].item
            else { unreachable!() };
        assert_eq!(content, "Continue the existing task.");
        assert_eq!(origin.session_id, member);
        // v3 W6 (Reviewer 9ba4c476): a reminder is an ACK-tracked request woken
        // by a pointer; its body is read with room_read, never typed.
        assert!(*ack_requested, "a reminder occurrence asks for an ACK");
        assert!(inputs.lock()[0].contains("new room message for you in room"), "{:?}", inputs.lock()[0]);
        assert!(!inputs.lock()[0].contains("Continue the existing task."));
        fallback_tick(&supervisor, &mut clock, now, mono, u64::from(minutes) * 60);
        assert_eq!(fallback_posts(&supervisor, room).len(), 1);
        supervisor.shutdown().unwrap();
    }
}

/// v3 W6 (J10, end of mandate): reminders run until their end and not after;
/// the status says so once; a past end cannot be saved, and nothing revives.
#[test]
fn v3_reminders_stop_at_the_end_of_their_mandate_and_never_revive() {
    let (supervisor, member, room, _) = fallback_fixture(true);
    supervisor.inner.slots.lock().get_by_id_mut(member).unwrap().work_state = WorkState::Thinking;
    let now = Utc::now(); let mono = Instant::now();
    let mut settings = fallback_settings(15);
    settings.until = Some((now + chrono::Duration::minutes(40)).to_rfc3339());
    fallback_save(&supervisor, room, settings);
    let mut clock = room_fallbacks::FallbackClock::default();
    // Each reminder is disposed of, so an owed one never coalesces the next:
    // only the end of the mandate can stop them.
    let dispose_latest = |supervisor: &SupervisorHandle| {
        let RoomFeedItem::Message { message_id, .. } = fallback_posts(supervisor, room).last().unwrap().item else { unreachable!() };
        supervisor.dispose_room_message(room, message_id, &member.to_string(), shared_types::RoomAckActor::Operator {},
            Some("noted".into()), None, None).unwrap();
    };
    fallback_tick(&supervisor, &mut clock, now, mono, 0);
    fallback_tick(&supervisor, &mut clock, now, mono, 15 * 60);
    dispose_latest(&supervisor);
    fallback_tick(&supervisor, &mut clock, now, mono, 30 * 60);
    assert_eq!(fallback_posts(&supervisor, room).len(), 2, "two occurrences before the end");
    dispose_latest(&supervisor);
    fallback_tick(&supervisor, &mut clock, now, mono, 45 * 60);
    fallback_tick(&supervisor, &mut clock, now, mono, 60 * 60);
    fallback_tick(&supervisor, &mut clock, now, mono, 120 * 60);
    assert_eq!(fallback_posts(&supervisor, room).len(), 2, "none after the end, and no catch-up");
    let status = supervisor.get_room_fallbacks(room).unwrap().members.into_iter().find(|row| row.session_id == member).unwrap();
    assert!(status.next_due_at.is_none());
    assert!(status.note.as_deref().is_some_and(|note| note.contains("mandate ended")), "{:?}", status.note);
    // A past end cannot be saved; a new future end starts again only by an explicit save.
    let revision = supervisor.get_room_fallbacks(room).unwrap().revision;
    let mut past = fallback_settings(15);
    past.until = Some((Utc::now() - chrono::Duration::minutes(1)).to_rfc3339());
    let refused = supervisor.set_room_fallbacks(SetRoomFallbacksRequest { room_id: room, revision, settings: past }).unwrap_err();
    assert!(refused.to_string().contains("until is in the past"), "{refused:#}");
    let mut bad = fallback_settings(15);
    bad.until = Some("tomorrow".into());
    assert!(supervisor.set_room_fallbacks(SetRoomFallbacksRequest { room_id: room, revision, settings: bad }).is_err());
    supervisor.shutdown().unwrap();
}

#[test]
fn fallbacks_custom_replaces_default_brief_is_appended_and_selected_excludes_new_members() {
    let (supervisor, member, room, _) = fallback_fixture(false);
    let codex = test_session_id(&supervisor, "codex");
    let mut settings = fallback_settings(30);
    settings.recipient_mode = RoomFallbackRecipientMode::Selected; settings.recipient_ids = vec![member];
    settings.overrides.push(RoomFallbackMemberOverride { session_id: member, mode: RoomFallbackOverrideMode::Custom,
        interval_minutes: Some(15), message: Some("Custom literal".into()), brief: Some("Keep source quotes.".into()) });
    fallback_save(&supervisor, room, settings);
    supervisor.add_room_member(AddRoomMemberRequest { room_id: room, session_id: codex }).unwrap();
    let mut clock = room_fallbacks::FallbackClock::default(); let now = Utc::now(); let mono = Instant::now();
    fallback_tick(&supervisor, &mut clock, now, mono, 0);
    fallback_tick(&supervisor, &mut clock, now, mono, 900);
    let posts = fallback_posts(&supervisor, room);
    assert_eq!(posts.len(), 1);
    assert!(matches!(&posts[0].item, RoomFeedItem::Message { content, recipient_ids, .. }
        if content == "Custom literal\n\nKeep source quotes." && recipient_ids == &vec![member]));
    fallback_tick(&supervisor, &mut clock, now, mono, 1800);
    assert_eq!(fallback_posts(&supervisor, room).len(), 1, "Stored blocks a second occurrence");
    supervisor.shutdown().unwrap();
}

#[test]
fn fallbacks_settings_revision_validation_and_catalog_failure_preserve_the_saved_choice() {
    let (supervisor, member, room, _) = fallback_fixture(false);
    let before = supervisor.get_room_fallbacks(room).unwrap();
    for minutes in [0, 1, 16, 1440] {
        assert!(supervisor.set_room_fallbacks(SetRoomFallbacksRequest { room_id: room, revision: 0,
            settings: fallback_settings(minutes) }).is_err());
    }
    let mut missing = fallback_settings(15); missing.recipient_ids.push(Uuid::new_v4());
    assert!(supervisor.set_room_fallbacks(SetRoomFallbacksRequest { room_id: room, revision: 0, settings: missing }).is_err());
    supervisor.inner.fail_next_room_catalog_write.store(true, Ordering::Release);
    assert!(supervisor.set_room_fallbacks(SetRoomFallbacksRequest { room_id: room, revision: 0, settings: fallback_settings(15) }).is_err());
    assert_eq!(supervisor.get_room_fallbacks(room).unwrap(), before);
    fallback_save(&supervisor, room, fallback_settings(15));
    assert!(supervisor.set_room_fallbacks(SetRoomFallbacksRequest { room_id: room, revision: 0, settings: fallback_settings(30) }).is_err());
    assert_eq!(supervisor.get_room_fallbacks(room).unwrap().members[0].session_id, member);
    supervisor.shutdown().unwrap();
}

/// v3 W6 (ruling b133623a): this test's older expectation, that a long
/// suspension skips its elapsed intervals with no reminder, is replaced: ONE
/// occurrence covers all of them, and none is repeated. A UTC jump still moves
/// nothing.
#[test]
fn fallbacks_suspension_coalesces_into_one_occurrence_and_wall_clock_changes_never_catch_up() {
    let (supervisor, member, room, _) = fallback_fixture(true);
    fallback_save(&supervisor, room, fallback_settings(15));
    let now = Utc::now(); let mono = Instant::now(); let mut clock = room_fallbacks::FallbackClock::default();
    fallback_tick(&supervisor, &mut clock, now, mono, 0);
    supervisor.tick_room_fallbacks(&mut clock, now + chrono::Duration::days(5), mono + Duration::from_secs(10));
    assert!(fallback_posts(&supervisor, room).is_empty(), "UTC jump does not advance monotonic due");
    fallback_tick(&supervisor, &mut clock, now, mono, 7200);
    let posts = fallback_posts(&supervisor, room);
    assert_eq!(posts.len(), 1, "a long suspension posts one occurrence for its missed times");
    let RoomFeedItem::Message { content, message_id, .. } = &posts[0].item else { unreachable!() };
    assert!(content.starts_with("Continue the existing task.\n\n[One reminder for 8 scheduled times since"), "{content}");
    let note = supervisor.get_room_fallbacks(room).unwrap().members[0].note.clone().unwrap_or_default();
    assert!(note.starts_with("One reminder for 8 scheduled times since"), "the status keeps it after the outcome: {note}");
    fallback_tick(&supervisor, &mut clock, now, mono, 7205);
    assert_eq!(fallback_posts(&supervisor, room).len(), 1, "the missed times are never repeated");
    supervisor.dispose_room_message(room, *message_id, &member.to_string(), shared_types::RoomAckActor::Operator {},
        Some("noted".into()), None, None).unwrap();
    let mut restarted_clock = room_fallbacks::FallbackClock::default();
    fallback_tick(&supervisor, &mut restarted_clock, now, mono, 8000);
    fallback_tick(&supervisor, &mut restarted_clock, now, mono, 8099);
    assert_eq!(fallback_posts(&supervisor, room).len(), 1, "a restart before the next due time misses nothing");
    supervisor.tick_room_fallbacks(&mut restarted_clock, now - chrono::Duration::days(5), mono + Duration::from_secs(8100));
    let posts = fallback_posts(&supervisor, room);
    assert_eq!(posts.len(), 2, "a backward UTC jump cannot freeze the monotonic schedule");
    assert!(matches!(&posts[1].item, RoomFeedItem::Message { content, .. } if content == "Continue the existing task."),
        "an on-time occurrence carries no missed note");
    supervisor.shutdown().unwrap();
}

/// v3 W6 (ruling b133623a): this test's older expectation, that a cold clock
/// skips past due times with no prompt, is replaced: one occurrence covers
/// them, scheduled at the first, and the phase is kept.
#[test]
fn fallbacks_cold_clock_preserves_remaining_phase_coalesces_past_and_caps_clock_reversal() {
    let (supervisor, _, room, _) = fallback_fixture(false);
    let now = Utc::now(); let mono = Instant::now();
    let saved = fallback_save(&supervisor, room, fallback_settings(15));
    let due = DateTime::parse_from_rfc3339(saved.members[0].next_due_at.as_ref().unwrap()).unwrap().with_timezone(&Utc);
    // Five minutes of a saved interval have elapsed: restarting does not add 15 more.
    let mut clock = room_fallbacks::FallbackClock::default();
    supervisor.tick_room_fallbacks(&mut clock, due - chrono::Duration::seconds(600), mono);
    assert_eq!(supervisor.get_room_fallbacks(room).unwrap().members[0].next_due_at, saved.members[0].next_due_at);
    // Several missed intervals: one occurrence for the three, and the next
    // due time keeps the original phase.
    let mut clock = room_fallbacks::FallbackClock::default();
    supervisor.tick_room_fallbacks(&mut clock, due + chrono::Duration::seconds(1850), mono);
    let advanced = supervisor.get_room_fallbacks(room).unwrap().members[0].next_due_at.clone().unwrap();
    assert_eq!(DateTime::parse_from_rfc3339(&advanced).unwrap().with_timezone(&Utc), due + chrono::Duration::seconds(2700));
    let posts = fallback_posts(&supervisor, room);
    assert_eq!(posts.len(), 1);
    let RoomFeedItem::Message { fallback_origin: Some(origin), content, .. } = &posts[0].item else { unreachable!() };
    assert!(content.contains("One reminder for 3 scheduled times since"), "{content}");
    assert_eq!(DateTime::parse_from_rfc3339(&origin.scheduled_at).unwrap().with_timezone(&Utc), due, "scheduled at the first");
    let mut clock = room_fallbacks::FallbackClock::default();
    supervisor.tick_room_fallbacks(&mut clock, now - chrono::Duration::days(5), mono);
    let capped = supervisor.get_room_fallbacks(room).unwrap().members[0].next_due_at.clone().unwrap();
    assert_eq!(DateTime::parse_from_rfc3339(&capped).unwrap().with_timezone(&Utc), now - chrono::Duration::days(5) + chrono::Duration::seconds(900));
    assert_eq!(fallback_posts(&supervisor, room).len(), 1, "a reversed clock misses nothing");
    supervisor.shutdown().unwrap();
}

/// Reviewer 5e979b6b: a reminder at the body limit has no room for the
/// missed-times note; its body is sent unchanged and the status keeps the note.
#[test]
fn v3_a_full_size_reminder_keeps_its_missed_times_note_in_the_status() {
    let (supervisor, _, room, _) = fallback_fixture(true);
    let mut settings = fallback_settings(15);
    settings.message = "x".repeat(MESSAGE_BODY_MAX_BYTES);
    let saved = fallback_save(&supervisor, room, settings.clone());
    let due = DateTime::parse_from_rfc3339(saved.members[0].next_due_at.as_ref().unwrap()).unwrap().with_timezone(&Utc);
    let mut clock = room_fallbacks::FallbackClock::default();
    supervisor.tick_room_fallbacks(&mut clock, due + chrono::Duration::seconds(1850), Instant::now());
    let posts = fallback_posts(&supervisor, room);
    assert!(matches!(&posts[..], [post] if matches!(&post.item, RoomFeedItem::Message { content, .. }
        if *content == settings.message)), "the body is never truncated or altered");
    let note = supervisor.get_room_fallbacks(room).unwrap().members[0].note.clone().unwrap_or_default();
    assert!(note.starts_with("One reminder for 3 scheduled times since"), "{note}");
    supervisor.shutdown().unwrap();
}

/// v3 W6 (ruling b133623a, Reviewer 496079e4, the restart trap): after a
/// PRIM-1 restart the member is still starting when the missed time is found.
/// The one occurrence waits for it; a regular boundary reached meanwhile folds
/// into the same occurrence. An owed reminder and a pause each mean nothing.
#[test]
fn v3_missed_reminders_wait_for_a_starting_member_as_one_and_yield_to_owed_and_pause() {
    let (supervisor, member, room, _) = fallback_fixture(true);
    let saved = fallback_save(&supervisor, room, fallback_settings(15));
    let due = DateTime::parse_from_rfc3339(saved.members[0].next_due_at.as_ref().unwrap()).unwrap().with_timezone(&Utc);
    let set_state = |state| supervisor.inner.slots.lock().get_by_id_mut(member).unwrap().state = state;
    let note = || supervisor.get_room_fallbacks(room).unwrap().members[0].note.clone().unwrap_or_default();
    let at = |seconds: i64| due + chrono::Duration::seconds(seconds);
    let mono = Instant::now();
    let after = |seconds: u64| mono + Duration::from_secs(seconds);

    // Still starting at the next regular boundary: it folds into the same
    // aggregate, and one occurrence follows once the member runs.
    set_state(LifecycleState::Starting);
    let mut clock = room_fallbacks::FallbackClock::default();
    supervisor.tick_room_fallbacks(&mut clock, at(60), mono);
    assert!(fallback_posts(&supervisor, room).is_empty());
    assert!(note().contains("1 scheduled time since") && note().contains("once the member is running"), "{}", note());
    supervisor.tick_room_fallbacks(&mut clock, at(900), after(840));
    assert!(fallback_posts(&supervisor, room).is_empty());
    assert!(note().contains("2 scheduled times since"), "folded: {}", note());
    set_state(LifecycleState::Ready);
    supervisor.tick_room_fallbacks(&mut clock, at(905), after(845));
    let posts = fallback_posts(&supervisor, room);
    assert_eq!(posts.len(), 1, "one occurrence for both");
    let RoomFeedItem::Message { fallback_origin: Some(origin), content, message_id, .. } = &posts[0].item else { unreachable!() };
    assert!(content.contains("One reminder for 2 scheduled times since"), "{content}");
    assert_eq!(DateTime::parse_from_rfc3339(&origin.scheduled_at).unwrap().with_timezone(&Utc), due);
    supervisor.dispose_room_message(room, *message_id, &member.to_string(), shared_types::RoomAckActor::Operator {},
        Some("noted".into()), None, None).unwrap();
    supervisor.tick_room_fallbacks(&mut clock, at(1800), after(1740));
    let posts = fallback_posts(&supervisor, room);
    assert!(matches!(&posts[1].item, RoomFeedItem::Message { content, .. } if content == "Continue the existing task."),
        "the following boundary is on time: {posts:?}");
    let RoomFeedItem::Message { message_id, .. } = posts[1].item else { unreachable!() };
    supervisor.dispose_room_message(room, message_id, &member.to_string(), shared_types::RoomAckActor::Operator {},
        Some("noted".into()), None, None).unwrap();

    // A restart while the member starts: the saved due time stays at the
    // first missed one while the occurrence waits.
    let saved_due = || supervisor.get_room_fallbacks(room).unwrap().members[0].next_due_at.as_deref()
        .map(|at| DateTime::parse_from_rfc3339(at).unwrap().with_timezone(&Utc));
    set_state(LifecycleState::Starting);
    let mut clock = room_fallbacks::FallbackClock::default();
    supervisor.tick_room_fallbacks(&mut clock, at(2700 + 1000), mono);
    supervisor.tick_room_fallbacks(&mut clock, at(2700 + 1005), after(5));
    assert_eq!(fallback_posts(&supervisor, room).len(), 2, "waits while the member starts");
    assert_eq!(saved_due(), Some(at(2700)), "the first missed due is kept while it waits");
    // Reviewer 43addeb5: PRIM-1 restarts again (a new clock reads the saved
    // record) while the member still starts, past one more boundary: the
    // missed times are counted again from the saved due, none lost.
    let mut clock = room_fallbacks::FallbackClock::default();
    supervisor.tick_room_fallbacks(&mut clock, at(4605), mono);
    assert_eq!(fallback_posts(&supervisor, room).len(), 2);
    set_state(LifecycleState::Ready);
    supervisor.tick_room_fallbacks(&mut clock, at(4610), after(5));
    let posts = fallback_posts(&supervisor, room);
    assert_eq!(posts.len(), 3, "one occurrence once it runs");
    let RoomFeedItem::Message { fallback_origin: Some(origin), content, .. } = &posts[2].item else { unreachable!() };
    assert!(content.contains("One reminder for 3 scheduled times since"), "{content}");
    assert_eq!(DateTime::parse_from_rfc3339(&origin.scheduled_at).unwrap().with_timezone(&Utc), at(2700));
    assert_eq!(saved_due(), Some(at(5400)), "then the phase continues");
    supervisor.tick_room_fallbacks(&mut clock, at(4615), after(10));
    assert_eq!(fallback_posts(&supervisor, room).len(), 3, "exactly one");

    // That one is still owed: a later restart past due posts nothing.
    let mut clock = room_fallbacks::FallbackClock::default();
    supervisor.tick_room_fallbacks(&mut clock, at(9000), mono);
    assert_eq!(fallback_posts(&supervisor, room).len(), 3, "an owed reminder suppresses the missed ones");
    assert!(note().contains("still owed"), "{}", note());

    // Paused: nothing, however long.
    let mut paused = fallback_settings(15);
    paused.enabled = false;
    fallback_save(&supervisor, room, paused);
    let mut clock = room_fallbacks::FallbackClock::default();
    supervisor.tick_room_fallbacks(&mut clock, at(90000), mono);
    assert_eq!(fallback_posts(&supervisor, room).len(), 3, "a pause wins");
    supervisor.shutdown().unwrap();
}

#[test]
fn fallbacks_unused_rooms_do_not_grow_or_write_default_settings() {
    let (supervisor, _, room, _) = fallback_fixture(false);
    let before = supervisor.inner.rooms.lock().catalog.clone();
    assert!(serde_json::to_value(&before).unwrap()["rooms"].as_array().unwrap().iter().all(|room| room.get("fallbacks").is_none()));
    let mut clock = room_fallbacks::FallbackClock::default(); let now = Utc::now(); let mono = Instant::now();
    supervisor.fail_next_room_catalog_write_for_tests();
    fallback_tick(&supervisor, &mut clock, now, mono, 0);
    fallback_tick(&supervisor, &mut clock, now, mono, 3600);
    assert_eq!(supervisor.inner.rooms.lock().catalog, before);
    assert!(supervisor.inner.fail_next_room_catalog_write.load(Ordering::Acquire), "unconfigured ticks did not attempt a catalog write");
    supervisor.inner.fail_next_room_catalog_write.store(false, Ordering::Release);
    fallback_save(&supervisor, room, RoomFallbackSettings::default());
    let rooms = supervisor.inner.rooms.lock();
    assert!(rooms.get(room).unwrap().definition.fallbacks.members.is_empty(), "disabled save adds no member rows");
    assert!(serde_json::to_value(&rooms.get(room).unwrap().definition).unwrap().get("fallbacks").is_some(), "explicit revision remains persisted");
    drop(rooms);
    supervisor.shutdown().unwrap();
}

// Inject a persisted pre-post intent, as a crash after occurrence admission but
// before journal append would leave it. All paths are the fixture's own.
fn fallback_saved_intent(supervisor: &SupervisorHandle, room: RoomId, member: SessionId,
    status: &str) -> shared_types::RoomFallbackOrigin {
    let (run_id, generation) = {
        let slots = supervisor.inner.slots.lock(); let slot = slots.get_by_id(member).unwrap();
        (slot.run_id.unwrap(), slot.generation)
    };
    let mut rooms = supervisor.inner.rooms.lock();
    let mut value = serde_json::to_value(&rooms.get(room).unwrap().definition.fallbacks).unwrap();
    let origin = shared_types::RoomFallbackOrigin { revision: value["revision"].as_u64().unwrap(), session_id: member,
        run_id, generation, occurrence_id: Uuid::new_v4(), scheduled_at: Utc::now().to_rfc3339() };
    let row = &mut value["members"][member.to_string()];
    row["occurrence"] = serde_json::to_value(&origin).unwrap(); row["unresolved"] = serde_json::json!(true);
    row["last_status"] = serde_json::json!(status);
    let record: room_fallbacks::RoomFallbackRecord = serde_json::from_value(value).unwrap();
    rooms.get_mut(room).unwrap().definition.fallbacks = record.clone();
    rooms.catalog.rooms.iter_mut().find(|definition| definition.room_id == room).unwrap().fallbacks = record;
    supervisor.persist_room_catalog_candidate(&rooms.catalog).unwrap();
    origin
}

fn fallback_member_record(supervisor: &SupervisorHandle, room: RoomId, member: SessionId) -> serde_json::Value {
    serde_json::to_value(&supervisor.inner.rooms.lock().get(room).unwrap().definition.fallbacks.members[&member]).unwrap()
}

#[test]
fn fallbacks_absent_prepost_intent_is_suppressed_but_unknown_physical_outcomes_stay_held() {
    for (status, unresolved) in [("posting", false), ("stored", false), ("pending", true), ("failed", true), ("uncertain", true)] {
        let (supervisor, member, room, inputs) = fallback_fixture(false);
        fallback_save(&supervisor, room, fallback_settings(15));
        let origin = fallback_saved_intent(&supervisor, room, member, status);
        let mut clock = room_fallbacks::FallbackClock::default(); let now = Utc::now(); let mono = Instant::now();
        fallback_tick(&supervisor, &mut clock, now, mono, 0);
        assert_eq!(fallback_member_record(&supervisor, room, member)["unresolved"], unresolved, "{status}");
        fallback_tick(&supervisor, &mut clock, now, mono, 900);
        assert_eq!(fallback_posts(&supervisor, room).len(), usize::from(!unresolved), "{status}");
        assert!(inputs.lock().is_empty());
        if !unresolved {
            let post = fallback_posts(&supervisor, room).pop().unwrap();
            assert_ne!(room_fallbacks::message_origin(&post).unwrap().occurrence_id, origin.occurrence_id, "old intent never reposted");
        } else {
            let prior = fallback_member_record(&supervisor, room, member);
            let mut restarted = room_fallbacks::FallbackClock::default(); let after_save = Utc::now(); let after_mono = Instant::now();
            fallback_save(&supervisor, room, fallback_settings(30));
            let rearmed = fallback_member_record(&supervisor, room, member);
            assert_eq!(rearmed["unresolved"], false, "explicit Save rearms an absent {status}");
            assert_eq!(rearmed["last_status"], prior["last_status"]);
            assert!(rearmed["note"].as_str().unwrap().starts_with(prior["note"].as_str().unwrap()));
            assert!(rearmed["note"].as_str().unwrap().contains("prior outcome remains unconfirmed"));
            fallback_tick(&supervisor, &mut restarted, after_save, after_mono, 0);
            fallback_tick(&supervisor, &mut restarted, after_save, after_mono, 1799);
            assert!(fallback_posts(&supervisor, room).is_empty(), "Save waits the new full interval");
            fallback_tick(&supervisor, &mut restarted, after_save, after_mono, 1800);
            let posts = fallback_posts(&supervisor, room);
            assert_eq!(posts.len(), 1);
            assert_ne!(room_fallbacks::message_origin(&posts[0]).unwrap().occurrence_id, origin.occurrence_id);
            assert!(inputs.lock().is_empty());
        }
        supervisor.shutdown().unwrap();
    }
}

#[test]
fn fallbacks_save_releases_recorded_terminal_outcomes_but_retains_pending() {
    for (receipt, bytes, zero, expected) in [(None, 0, false, false),
        (Some(RoomDeliveryStatus::Pending), 0, false, true),
        (Some(RoomDeliveryStatus::Failed), 4, false, false),
        (Some(RoomDeliveryStatus::Failed), 0, false, false),
        (Some(RoomDeliveryStatus::Failed), 0, true, false)] {
        let (supervisor, member, room, inputs) = fallback_fixture(false);
        fallback_save(&supervisor, room, fallback_settings(15));
        let origin = fallback_saved_intent(&supervisor, room, member, "posting");
        {
            let mut rooms = supervisor.inner.rooms.lock(); let runtime = rooms.get_mut(room).unwrap();
            let revision = runtime.definition.membership_revision;
            let (event, _) = runtime.post_with_fallback(room_fallbacks::request_id(&origin), RoomMessageSender::Operator {},
                "Continue the existing task.".into(), vec![member], revision, Some(origin.clone())).unwrap();
            let RoomFeedItem::Message { message_id, .. } = event.item else { unreachable!() };
            if let Some(status) = receipt {
                runtime.append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, readmit: false, pointer: None, message_id, recipient_id: member, status, bytes_written: bytes,
                    zero_write_confirmed: zero, error: (bytes > 0).then(|| "partial-write evidence".into()),
                    run_id: Some(origin.run_id), generation: Some(origin.generation) }).unwrap();
            }
        }
        let now = Utc::now(); let mono = Instant::now(); let mut clock = room_fallbacks::FallbackClock::default();
        fallback_save(&supervisor, room, fallback_settings(30));
        assert_eq!(fallback_member_record(&supervisor, room, member)["unresolved"], expected, "{receipt:?}/{bytes}/{zero}");
        if receipt == Some(RoomDeliveryStatus::Failed) && bytes > 0 {
            assert_eq!(fallback_member_record(&supervisor, room, member)["last_status"], "failed", "Save does not relabel already-written bytes as unattempted");
            assert!(fallback_member_record(&supervisor, room, member)["note"].as_str().unwrap().starts_with("partial-write evidence; "));
        }
        fallback_tick(&supervisor, &mut clock, now, mono, 0);
        fallback_tick(&supervisor, &mut clock, now, mono, 1799);
        assert_eq!(fallback_posts(&supervisor, room).len(), 1, "Save does not send immediately");
        fallback_tick(&supervisor, &mut clock, now, mono, 1800);
        assert_eq!(fallback_posts(&supervisor, room).len(), if expected { 1 } else { 2 });
        if !expected {
            let last = fallback_posts(&supervisor, room).pop().unwrap();
            assert_ne!(room_fallbacks::message_origin(&last).unwrap().occurrence_id, origin.occurrence_id);
        }
        assert!(inputs.lock().is_empty());
        supervisor.shutdown().unwrap();
    }
}

fn fallback_damage_history(supervisor: &SupervisorHandle, room_id: RoomId, startup: bool) {
    let mut rooms = supervisor.inner.rooms.lock();
    let room = rooms.get_mut(room_id).unwrap();
    room.append_invalid_complete_record_for_tests();
    if startup {
        let replacement = rooms::RoomRuntime::from_persisted(room.definition.clone());
        drop(std::mem::replace(room, replacement));
        assert!(room.open_log(supervisor.runtime_dir()).is_err());
        assert!(room.snapshot().recovery_error.is_some());
    } else { assert!(room.snapshot().history_replay_pending); }
}

#[test]
fn fallbacks_recovering_history_skips_admission_and_late_prepost_refusal_is_not_uncertain() {
    for startup in [false, true] { for late in [false, true] {
        let (supervisor, member, room, inputs) = fallback_fixture(true);
        fallback_save(&supervisor, room, fallback_settings(15));
        let now = Utc::now(); let mono = Instant::now(); let mut clock = room_fallbacks::FallbackClock::default();
        fallback_tick(&supervisor, &mut clock, now, mono, 0);
        if late {
            let fault = supervisor.clone();
            supervisor.set_room_delivery_after_preflight_for_tests(move || fallback_damage_history(&fault, room, startup));
        } else { fallback_damage_history(&supervisor, room, startup); }
        let boundary = supervisor.inner.rooms.lock().get(room).unwrap().next_sequence;
        fallback_tick(&supervisor, &mut clock, now, mono, 900);
        let row = fallback_member_record(&supervisor, room, member);
        assert_eq!(row["unresolved"], false, "startup={startup}, late={late}");
        assert_eq!(row["last_status"], if late { "held" } else { "skipped" });
        assert_eq!(row["occurrence"].is_null(), !late);
        fallback_tick(&supervisor, &mut clock, now, mono, 1800);
        assert_eq!(supervisor.inner.rooms.lock().get(room).unwrap().next_sequence, boundary);
        assert!(inputs.lock().is_empty());
        supervisor.shutdown().unwrap();
    } }
}

#[test]
fn fallbacks_save_keeps_absent_uncertain_occurrence_held_while_history_is_unhealthy() {
    for startup in [false, true] {
        let (supervisor, member, room, inputs) = fallback_fixture(true);
        fallback_save(&supervisor, room, fallback_settings(15));
        fallback_saved_intent(&supervisor, room, member, "uncertain");
        fallback_damage_history(&supervisor, room, startup);
        let before = fallback_member_record(&supervisor, room, member);
        let saved = fallback_save(&supervisor, room, fallback_settings(30));
        assert!(saved.notice_warning.is_some());
        let after = fallback_member_record(&supervisor, room, member);
        assert_eq!(after["unresolved"], true);
        assert_eq!(after["last_status"], before["last_status"]);
        assert_eq!(after["note"], before["note"]);
        assert_eq!(after["occurrence"], before["occurrence"]);
        let now = Utc::now(); let mono = Instant::now(); let mut clock = room_fallbacks::FallbackClock::default();
        fallback_tick(&supervisor, &mut clock, now, mono, 0);
        fallback_tick(&supervisor, &mut clock, now, mono, 1800);
        assert!(inputs.lock().is_empty());
        supervisor.shutdown().unwrap();
    }
}

#[test]
fn fallbacks_torn_uncommitted_post_needs_explicit_save_after_healthy_replay() {
    let (supervisor, member, room, inputs) = fallback_fixture(false);
    fallback_save(&supervisor, room, fallback_settings(15));
    let now = Utc::now(); let mono = Instant::now(); let mut clock = room_fallbacks::FallbackClock::default();
    fallback_tick(&supervisor, &mut clock, now, mono, 0);
    let fault = supervisor.clone();
    supervisor.set_room_delivery_after_preflight_for_tests(move || {
        fault.inner.rooms.lock().get_mut(room).unwrap().fail_next_commit_for_tests(false);
    });
    fallback_tick(&supervisor, &mut clock, now, mono, 900);
    let uncertain = fallback_member_record(&supervisor, room, member);
    assert_eq!(uncertain["unresolved"], true);
    assert_eq!(uncertain["last_status"], "uncertain");
    // Operator reads deliberately retain the acknowledged prefix; they do not
    // recover an uncertain writer. A real feed-only post performs that recovery.
    assert!(fallback_posts(&supervisor, room).is_empty());
    assert!(supervisor.inner.rooms.lock().get(room).unwrap().snapshot().history_replay_pending);
    supervisor.post_room_message(PostRoomMessageRequest { room_id: room,
        request_id: Some("fixture-recovery-check".into()), content: "Check the recovered room history.".into() }).unwrap();
    assert!(!supervisor.inner.rooms.lock().get(room).unwrap().snapshot().history_replay_pending);
    assert!(fallback_posts(&supervisor, room).is_empty(), "the torn Message was not published or reposted");
    assert!(inputs.lock().is_empty(), "feed-only recovery performs no PTY write");
    fallback_tick(&supervisor, &mut clock, now, mono, 1800);
    assert!(fallback_posts(&supervisor, room).is_empty());
    let mut restarted = room_fallbacks::FallbackClock::default(); let after_save = Utc::now(); let after_mono = Instant::now();
    fallback_save(&supervisor, room, fallback_settings(30));
    let rearmed = fallback_member_record(&supervisor, room, member);
    assert_eq!(rearmed["unresolved"], false);
    assert_eq!(rearmed["last_status"], "uncertain");
    assert!(rearmed["note"].as_str().unwrap().starts_with(uncertain["note"].as_str().unwrap()));
    fallback_tick(&supervisor, &mut restarted, after_save, after_mono, 0);
    fallback_tick(&supervisor, &mut restarted, after_save, after_mono, 1799);
    assert!(fallback_posts(&supervisor, room).is_empty());
    fallback_tick(&supervisor, &mut restarted, after_save, after_mono, 1800);
    let posts = fallback_posts(&supervisor, room);
    assert_eq!(posts.len(), 1);
    assert_ne!(serde_json::to_value(room_fallbacks::message_origin(&posts[0]).unwrap()).unwrap(), uncertain["occurrence"]);
    assert!(inputs.lock().is_empty());
    supervisor.shutdown().unwrap();
}

#[test]
fn fallbacks_evicted_partial_failure_stays_automatic_hold_but_save_rearms_a_new_occurrence() {
    let (supervisor, member, room, inputs) = fallback_fixture(false);
    fallback_save(&supervisor, room, fallback_settings(15));
    let origin = fallback_saved_intent(&supervisor, room, member, "failed");
    {
        let mut rooms = supervisor.inner.rooms.lock(); let runtime = rooms.get_mut(room).unwrap();
        let revision = runtime.definition.membership_revision;
        let (event, _) = runtime.post_with_fallback(room_fallbacks::request_id(&origin), RoomMessageSender::Operator {},
            "Continue the existing task.".into(), vec![member], revision, Some(origin.clone())).unwrap();
        let RoomFeedItem::Message { message_id, .. } = event.item else { unreachable!() };
        runtime.append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, readmit: false, pointer: None, message_id, recipient_id: member, status: RoomDeliveryStatus::Failed,
            bytes_written: 4, zero_write_confirmed: false, error: Some("retained partial-write evidence".into()),
            run_id: Some(origin.run_id), generation: Some(origin.generation) }).unwrap();
    }
    let now = Utc::now(); let mono = Instant::now(); let mut clock = room_fallbacks::FallbackClock::default();
    fallback_tick(&supervisor, &mut clock, now, mono, 0); // persist the actual receipt summary
    {
        let mut rooms = supervisor.inner.rooms.lock(); let runtime = rooms.get_mut(room).unwrap();
        for index in 0..1024 { // existing bounded post horizon, using the real journal path
            runtime.post(format!("later-{index}"), RoomMessageSender::Operator {}, "later chatter".into(), vec![], 1).unwrap();
        }
        assert!(runtime.fallback_delivery(&room_fallbacks::request_id(&origin), member).is_none());
    }
    fallback_tick(&supervisor, &mut clock, now, mono, 900);
    assert_eq!(fallback_member_record(&supervisor, room, member)["unresolved"], true);
    let mut restarted = room_fallbacks::FallbackClock::default(); let after_save = Utc::now(); let after_mono = Instant::now();
    fallback_save(&supervisor, room, fallback_settings(30));
    let rearmed = fallback_member_record(&supervisor, room, member);
    assert_eq!(rearmed["unresolved"], false);
    assert_eq!(rearmed["last_status"], "failed");
    assert!(rearmed["note"].as_str().unwrap().starts_with("retained partial-write evidence; "));
    let boundary = supervisor.inner.rooms.lock().get(room).unwrap().next_sequence;
    fallback_tick(&supervisor, &mut restarted, after_save, after_mono, 0);
    fallback_tick(&supervisor, &mut restarted, after_save, after_mono, 1799);
    assert_eq!(supervisor.inner.rooms.lock().get(room).unwrap().next_sequence, boundary);
    fallback_tick(&supervisor, &mut restarted, after_save, after_mono, 1800);
    let next: shared_types::RoomFallbackOrigin = serde_json::from_value(fallback_member_record(&supervisor, room, member)["occurrence"].clone()).unwrap();
    assert_ne!(next.occurrence_id, origin.occurrence_id);
    assert!(supervisor.inner.rooms.lock().get(room).unwrap().fallback_delivery(&room_fallbacks::request_id(&next), member).is_some());
    assert!(inputs.lock().is_empty());
    supervisor.shutdown().unwrap();
}

#[test]
fn fallbacks_replay_older_than_twelve_hours_records_hold_for_the_old_run() {
    let root = tempfile::tempdir().unwrap();
    let supervisor = test_supervisor_with_root(root.path().to_path_buf());
    let member = test_session_id(&supervisor, "claude");
    let room = mobility_room(&supervisor, "Old fallback", vec![member]).room_id;
    let (pty, _) = recording_pty_session(9927);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude,
        None, pty, BracketedPasteMode::Unknown);
    fallback_save(&supervisor, room, fallback_settings(15));
    let origin = fallback_saved_intent(&supervisor, room, member, "stored");
    let message_id = {
        let mut rooms = supervisor.inner.rooms.lock(); let runtime = rooms.get_mut(room).unwrap();
        let revision = runtime.definition.membership_revision;
        let (event, _) = runtime.post_with_fallback(room_fallbacks::request_id(&origin), RoomMessageSender::Operator {},
            "Continue the existing task.".into(), vec![member], revision, Some(origin)).unwrap();
        let RoomFeedItem::Message { message_id, .. } = event.item else { unreachable!() }; message_id
    };
    let log = supervisor.inner.runtime_dir.join("rooms").join(format!("{room}.jsonl"));
    supervisor.shutdown().unwrap(); drop(supervisor);
    // Closed fixture journal only: replay an old Stored message as a long
    // downtime would. No test process writes an opened room history.
    let mut changed = 0;
    let records = std::fs::read_to_string(&log).unwrap().lines().map(|line| {
        let mut record: serde_json::Value = serde_json::from_str(line).unwrap();
        if record["event"]["item"]["message_id"] == message_id.to_string() {
            record["event"]["timestamp"] = serde_json::json!((Utc::now() - chrono::Duration::hours(13)).to_rfc3339());
            changed += 1;
        }
        serde_json::to_string(&record).unwrap()
    }).collect::<Vec<_>>().join("\n") + "\n";
    assert_eq!(changed, 1); std::fs::write(&log, records).unwrap();
    let reopened = SupervisorHandle::new(test_supervisor_config(root.path())).unwrap();
    assert_eq!(reopened.resume_room_wake_queue(), 1);
    wait_for_room_receipt(&reopened, room, message_id, member, RoomDeliveryStatus::Held);
    wait_for_no_queued_room_wake(&reopened, member);
    assert_eq!(fallback_posts(&reopened, room).len(), 1, "only the original stored intent remains");
    reopened.shutdown().unwrap();
}

#[test]
fn fallbacks_outcome_save_failure_does_not_abandon_other_committed_recipients() {
    let (supervisor, _, room, first_inputs) = fallback_fixture(true);
    let second = test_session_id(&supervisor, "codex");
    supervisor.add_room_member(AddRoomMemberRequest { room_id: room, session_id: second }).unwrap();
    let (pty, second_inputs) = recording_pty_session(9926);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "codex", DriverKind::Codex,
        None, pty, BracketedPasteMode::Enabled);
    fallback_save(&supervisor, room, fallback_settings(15));
    let events = capture_runtime_events(&supervisor);
    let fault = supervisor.clone();
    supervisor.set_room_event_after_append_for_tests(move || fault.fail_next_room_catalog_write_for_tests());
    let now = Utc::now(); let mono = Instant::now(); let mut clock = room_fallbacks::FallbackClock::default();
    fallback_tick(&supervisor, &mut clock, now, mono, 0);
    fallback_tick(&supervisor, &mut clock, now, mono, 900);
    assert_eq!(fallback_posts(&supervisor, room).len(), 2);
    assert!(!first_inputs.lock().is_empty() && !second_inputs.lock().is_empty());
    assert!(events.lock().iter().any(|event| matches!(event, RuntimeEvent::SystemLog { level: LogLevel::Error, message, .. }
        if message.contains("outcome could not be saved") && message.contains("other recipients"))));
    supervisor.shutdown().unwrap();
}

#[test]
fn fallbacks_save_and_tick_release_slots_before_catalog_io_and_notice_keys_do_not_collide() {
    let (supervisor, _, room, _) = fallback_fixture(false);
    supervisor.post_room_message(PostRoomMessageRequest { room_id: room, request_id: Some("fallback-settings-1".into()), content: "Earlier operator message".into() }).unwrap();
    let checked = Arc::new(AtomicUsize::new(0));
    for tick in [false, true] {
        let owner = Arc::downgrade(&supervisor.inner); let count = checked.clone();
        *supervisor.inner.room_catalog_before_write.lock() = Some(Box::new(move || {
            assert!(owner.upgrade().unwrap().slots.try_lock().is_some(), "catalog fsync must not retain slots");
            count.fetch_add(1, Ordering::SeqCst);
        }));
        if !tick { assert!(fallback_save(&supervisor, room, fallback_settings(15)).notice_warning.is_none()); }
        else {
            let now = Utc::now(); let mono = Instant::now(); let mut clock = room_fallbacks::FallbackClock::default();
            fallback_tick(&supervisor, &mut clock, now, mono, 0);
            fallback_tick(&supervisor, &mut clock, now, mono, 900);
        }
    }
    assert_eq!(checked.load(Ordering::SeqCst), 2);
    let events = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor: None }).unwrap().events;
    let keys: Vec<_> = events.iter().filter_map(|event| match &event.item {
        RoomFeedItem::Message { request_id, content, .. } if content.contains("room fallbacks (settings revision") => Some(request_id), _ => None,
    }).collect();
    assert_eq!(keys.len(), 1);
    assert!(Uuid::parse_str(keys[0].strip_prefix("fallback-settings-").unwrap()).is_ok());
    supervisor.shutdown().unwrap();
}

#[test]
fn fallbacks_tick_panic_is_reported_and_a_fresh_clock_can_continue() {
    let (supervisor, _, room, _) = fallback_fixture(false);
    fallback_save(&supervisor, room, fallback_settings(15));
    let events = capture_runtime_events(&supervisor);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let result = runtime.block_on(async {
        tokio::task::spawn_blocking(|| -> room_fallbacks::FallbackClock { panic!("injected fallback tick panic") }).await
    });
    let mut clock = supervisor.resume_fallback_clock(result).expect("panic resets clock rather than stopping timer");
    let now = Utc::now(); let mono = Instant::now();
    fallback_tick(&supervisor, &mut clock, now, mono, 0);
    fallback_tick(&supervisor, &mut clock, now, mono, 900);
    assert_eq!(fallback_posts(&supervisor, room).len(), 1);
    assert!(events.lock().iter().any(|event| matches!(event, RuntimeEvent::SystemLog { level: LogLevel::Error, message, .. }
        if message.contains("timer worker failed") && message.contains("scheduling will continue"))));
    supervisor.shutdown().unwrap();
}

#[test]
fn fallbacks_room_brief_describes_reminders_to_all_but_tools_only_to_managers() {
    for driver in [DriverKind::Claude, DriverKind::Grok] {
        let member = RoomMember { session_id: Uuid::new_v4(), label: "Agent".into(), driver };
        for manage in [false, true] { for roam in [false, true] {
            let text = render_room_brief("Team", &[member.clone()], &member, MemberPermissions { manage, roam, maintain: false }, None, None);
            assert!(text.contains("scheduled reminders") && text.contains("not a new live instruction from Victor"));
            assert_eq!(text.contains(if driver == DriverKind::Claude { "room_fallbacks" } else { "fallbacks get" }), manage);
            assert_eq!(text.contains("roam"), roam);
        } }
    }
}

#[test]
fn fallbacks_pause_after_queue_and_stop_run_change_suppress_without_waking() {
    for stop in [false, true] {
        let (supervisor, member, room, inputs) = fallback_fixture(false);
        fallback_save(&supervisor, room, fallback_settings(15));
        let now = Utc::now(); let mono = Instant::now(); let mut clock = room_fallbacks::FallbackClock::default();
        fallback_tick(&supervisor, &mut clock, now, mono, 0);
        fallback_tick(&supervisor, &mut clock, now, mono, 900);
        let message = fallback_posts(&supervisor, room).pop().unwrap();
        let RoomFeedItem::Message { message_id, .. } = message.item else { unreachable!() };
        if stop { supervisor.stop_session_by_id(member).unwrap(); }
        else { let mut settings = fallback_settings(15); settings.enabled = false; fallback_save(&supervisor, room, settings); }
        wait_for_room_receipt(&supervisor, room, message_id, member, RoomDeliveryStatus::Held);
        assert!(inputs.lock().is_empty());
        fallback_tick(&supervisor, &mut clock, now, mono, 1800);
        assert_eq!(fallback_posts(&supervisor, room).len(), 1);
        supervisor.shutdown().unwrap();
    }
}

#[test]
fn fallbacks_change_between_occurrence_commit_and_post_is_a_known_noncommit_not_permanent_hold() {
    let (supervisor, _, room, _) = fallback_fixture(false);
    fallback_save(&supervisor, room, fallback_settings(15));
    let now = Utc::now(); let mono = Instant::now(); let mut clock = room_fallbacks::FallbackClock::default();
    fallback_tick(&supervisor, &mut clock, now, mono, 0);
    let edit = supervisor.clone();
    supervisor.set_room_delivery_after_preflight_for_tests(move || {
        let mut settings = fallback_settings(15); settings.message = "Replacement".into(); fallback_save(&edit, room, settings);
    });
    fallback_tick(&supervisor, &mut clock, now, mono, 900);
    assert!(fallback_posts(&supervisor, room).is_empty());
    assert_eq!(supervisor.get_room_fallbacks(room).unwrap().members[0].last_status.as_deref(), Some("held"));
    fallback_tick(&supervisor, &mut clock, now, mono, 901); // new revision starts a future interval
    fallback_tick(&supervisor, &mut clock, now, mono, 1801);
    assert_eq!(fallback_posts(&supervisor, room).len(), 1);
    supervisor.shutdown().unwrap();
}

#[test]
fn fallbacks_pause_after_pending_before_paste_records_zero_write_without_typing() {
    let (supervisor, _, room, inputs) = fallback_fixture(true);
    fallback_save(&supervisor, room, fallback_settings(15));
    let now = Utc::now(); let mono = Instant::now(); let mut clock = room_fallbacks::FallbackClock::default();
    fallback_tick(&supervisor, &mut clock, now, mono, 0);
    let pause = supervisor.clone();
    supervisor.set_room_wake_before_write_for_tests(move |_| {
        let mut settings = fallback_settings(15); settings.enabled = false; fallback_save(&pause, room, settings);
    });
    fallback_tick(&supervisor, &mut clock, now, mono, 900);
    assert!(inputs.lock().is_empty());
    let page = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor: None }).unwrap();
    assert!(page.events.iter().any(|event| matches!(event.item, RoomFeedItem::Delivery {
        status: RoomDeliveryStatus::Pending, .. })));
    assert!(page.events.iter().any(|event| matches!(event.item, RoomFeedItem::Delivery {
        status: RoomDeliveryStatus::Failed, bytes_written: 0, zero_write_confirmed: true, .. })));
    supervisor.shutdown().unwrap();
}

#[test]
fn fallbacks_get_and_set_require_manage_roam_and_recheck_revocation() {
    let supervisor = test_supervisor(); let member = test_session_id(&supervisor, "claude");
    let caller = mobility_pane(&supervisor, "claude", 9922);
    let own = mobility_room(&supervisor, "Own", vec![member]);
    let other = mobility_room(&supervisor, "Other", Vec::new());
    let request = || SidebandRequest::RoomFallbacks { action: RoomFallbackAction::Get,
        room_id: None, revision: None, settings: None };
    assert!(!supervisor.apply_sideband_request(&caller, request()).ok);
    supervisor.set_member_permissions(member, manage_grant(false)).unwrap();
    assert!(supervisor.apply_sideband_request(&caller, request()).ok);
    assert!(!supervisor.apply_sideband_request(&caller, SidebandRequest::RoomFallbacks { action: RoomFallbackAction::Get,
        room_id: Some(other.room_id), revision: None, settings: None }).ok);
    let revoke = supervisor.clone();
    *supervisor.inner.sideband_after_initial_authorization.lock() = Some(Box::new(move || {
        revoke.set_member_permissions(member, MemberPermissions::default()).unwrap();
    }));
    assert!(!supervisor.apply_sideband_request(&caller, SidebandRequest::RoomFallbacks { action: RoomFallbackAction::Set,
        room_id: Some(own.room_id), revision: Some(0), settings: Some(fallback_settings(15)) }).ok);
    assert!(!supervisor.get_room_fallbacks(own.room_id).unwrap().settings.enabled);
    supervisor.shutdown().unwrap();
}

#[test]
fn fallbacks_removed_rejoined_and_replacement_runs_hold_the_old_queued_intent() {
    for rejoin in [true, false] {
        let (supervisor, member, room, inputs) = fallback_fixture(false);
        fallback_save(&supervisor, room, fallback_settings(15));
        let now = Utc::now(); let mono = Instant::now(); let mut clock = room_fallbacks::FallbackClock::default();
        fallback_tick(&supervisor, &mut clock, now, mono, 0);
        fallback_tick(&supervisor, &mut clock, now, mono, 900);
        let event = fallback_posts(&supervisor, room).pop().unwrap();
        let RoomFeedItem::Message { message_id, .. } = event.item else { unreachable!() };
        if rejoin {
            supervisor.remove_room_member(RemoveRoomMemberRequest { room_id: room, session_id: member }).unwrap();
            supervisor.add_room_member(AddRoomMemberRequest { room_id: room, session_id: member }).unwrap();
            observe_current_run_paste_mode(&supervisor, member, "\x1b[?2004h");
        } else {
            let (replacement, replacement_inputs) = recording_pty_session(9923);
            install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude,
                None, replacement, BracketedPasteMode::Enabled);
            wait_for_room_receipt(&supervisor, room, message_id, member, RoomDeliveryStatus::Held);
            assert!(replacement_inputs.lock().is_empty());
        }
        wait_for_room_receipt(&supervisor, room, message_id, member, RoomDeliveryStatus::Held);
        assert!(inputs.lock().is_empty());
        supervisor.shutdown().unwrap();
    }
}

#[test]
fn fallbacks_changes_notify_after_commit_and_unchanged_ticks_do_not() {
    let (supervisor, _, room, _) = fallback_fixture(false);
    let changes = Arc::new(AtomicUsize::new(0)); let observed = changes.clone();
    supervisor.set_event_sink(move |event| {
        if matches!(event, RuntimeEvent::RoomFallbacksChanged { room_id } if room_id == room) {
            observed.fetch_add(1, Ordering::SeqCst);
        }
    });
    supervisor.fail_next_room_catalog_write_for_tests();
    assert!(supervisor.set_room_fallbacks(SetRoomFallbacksRequest { room_id: room, revision: 0,
        settings: fallback_settings(15) }).is_err());
    assert_eq!(changes.load(Ordering::SeqCst), 0);
    fallback_save(&supervisor, room, fallback_settings(15));
    assert_eq!(changes.load(Ordering::SeqCst), 1);
    let now = Utc::now(); let mono = Instant::now(); let mut clock = room_fallbacks::FallbackClock::default();
    fallback_tick(&supervisor, &mut clock, now, mono, 0);
    let after_due_saved = changes.load(Ordering::SeqCst);
    fallback_tick(&supervisor, &mut clock, now, mono, 5);
    assert_eq!(changes.load(Ordering::SeqCst), after_due_saved);
    supervisor.shutdown().unwrap();
}

#[test]
fn fallbacks_uncertain_pending_survives_catalog_reload_and_coalesces() {
    let root = tempfile::tempdir().unwrap();
    let supervisor = test_supervisor_with_root(root.path().to_path_buf());
    let member = test_session_id(&supervisor, "claude");
    let room = mobility_room(&supervisor, "Uncertain fallback", vec![member]).room_id;
    let (pty, inputs) = recording_pty_session(9924);
    install_mock_running_session_with_process_id_and_mode(&supervisor, "claude", DriverKind::Claude,
        None, pty, BracketedPasteMode::Enabled);
    fallback_save(&supervisor, room, fallback_settings(15));
    let now = Utc::now(); let mono = Instant::now(); let mut clock = room_fallbacks::FallbackClock::default();
    fallback_tick(&supervisor, &mut clock, now, mono, 0);
    let fault = supervisor.clone();
    supervisor.set_room_event_after_append_for_tests(move || {
        fault.inner.rooms.lock().get_mut(room).unwrap().fail_next_commit_for_tests(true);
    });
    fallback_tick(&supervisor, &mut clock, now, mono, 900);
    assert!(inputs.lock().is_empty(), "unconfirmed Pending never enters the PTY");
    drop(supervisor);
    let reopened = SupervisorHandle::new(test_supervisor_config(root.path())).unwrap();
    assert!(reopened.get_room_fallbacks(room).unwrap().settings.enabled);
    let (replacement, replacement_inputs) = recording_pty_session(9925);
    install_mock_running_session_with_process_id_and_mode(&reopened, "claude", DriverKind::Claude,
        None, replacement, BracketedPasteMode::Enabled);
    let mut restarted = room_fallbacks::FallbackClock::default();
    fallback_tick(&reopened, &mut restarted, now, mono, 1800);
    fallback_tick(&reopened, &mut restarted, now, mono, 2700);
    assert_eq!(fallback_posts(&reopened, room).len(), 1);
    assert_eq!(reopened.get_room_fallbacks(room).unwrap().members[0].last_status.as_deref(), Some("pending"));
    assert!(replacement_inputs.lock().is_empty(), "unknown transport cannot become another occurrence after restart");
    reopened.shutdown().unwrap();
}

#[test]
fn a16_a_grant_revoked_while_an_agents_fallback_save_is_written_waits_for_that_commit() {
    let supervisor = test_supervisor(); let member = test_session_id(&supervisor, "claude");
    let caller = mobility_pane(&supervisor, "claude", 9932);
    let room = mobility_room(&supervisor, "Own", vec![member]).room_id;
    supervisor.set_member_permissions(member, manage_grant(false)).unwrap();
    let revoked = Arc::new(AtomicBool::new(false));
    let revoker = Arc::new(Mutex::new(None));
    let (owner, flag, handle) = (supervisor.clone(), revoked.clone(), revoker.clone());
    let inner = Arc::downgrade(&supervisor.inner);
    // Between the final check and the commit, the operator unticks manage.
    *supervisor.inner.room_catalog_before_write.lock() = Some(Box::new(move || {
        // The inverse of A111's direct-save test: an agent's save holds slots here.
        assert!(inner.upgrade().unwrap().slots.try_lock().is_none(), "an agent's save holds slots through the catalog write");
        let done = flag.clone();
        *handle.lock() = Some(thread::spawn(move || {
            owner.set_member_permissions(member, MemberPermissions::default()).unwrap();
            done.store(true, Ordering::SeqCst);
        }));
        thread::sleep(Duration::from_millis(300));
        assert!(!flag.load(Ordering::SeqCst), "the revocation must wait for the save it would invalidate");
    }));
    let set = |revision: u64| SidebandRequest::RoomFallbacks { action: RoomFallbackAction::Set,
        room_id: Some(room), revision: Some(revision), settings: Some(fallback_settings(15)) };
    let revision = supervisor.get_room_fallbacks(room).unwrap().revision;
    let saved = supervisor.apply_sideband_request(&caller, set(revision));
    assert!(saved.ok, "committed under the grant it held: {saved:?}");
    revoker.lock().take().expect("the hook ran").join().unwrap();
    assert!(revoked.load(Ordering::SeqCst));
    assert!(supervisor.get_room_fallbacks(room).unwrap().settings.enabled);
    let after = supervisor.apply_sideband_request(&caller, set(revision + 1));
    assert!(!after.ok && after.message.contains("Can manage"), "{after:?}");
    supervisor.shutdown().unwrap();
}

/// v3 W6 (J6 condition 4, ff843076): while a member owes a reminder, a later
/// tick posts nothing new and says why; the owed reminder is in the member's
/// pointer count like any ACK-tracked request.
#[test]
fn v3_an_owed_reminder_coalesces_later_ticks_and_counts_in_the_pointer() {
    let (supervisor, member, room, inputs) = fallback_fixture(true);
    supervisor.inner.slots.lock().get_by_id_mut(member).unwrap().work_state = WorkState::Thinking;
    fallback_save(&supervisor, room, fallback_settings(15));
    let mut clock = room_fallbacks::FallbackClock::default(); let now = Utc::now(); let mono = Instant::now();
    fallback_tick(&supervisor, &mut clock, now, mono, 0);
    fallback_tick(&supervisor, &mut clock, now, mono, 15 * 60);
    assert_eq!(fallback_posts(&supervisor, room).len(), 1);
    let unread = supervisor.inner.rooms.lock().get(room).unwrap().unread_for(member);
    assert_eq!(unread.len(), 1, "the reminder counts as owed, unread work");
    fallback_tick(&supervisor, &mut clock, now, mono, 30 * 60);
    assert_eq!(fallback_posts(&supervisor, room).len(), 1, "no second reminder while the first is owed");
    let snapshot = supervisor.get_room_fallbacks(room).unwrap();
    let row = snapshot.members.iter().find(|row| row.session_id == member).unwrap();
    assert!(row.note.as_deref().is_some_and(|note| note.contains("still owed")), "{row:?}");
    let pointers = inputs.lock().iter().filter(|input| input.contains("new room message")).count();
    assert_eq!(pointers, 1, "one pointer, nothing typed for the skipped tick: {:?}", inputs.lock());
    supervisor.shutdown().unwrap();
}


/// Reviewer b307957c: a reminder that is no longer current (here, paused) is
/// neither counted nor covered by another request's pointer, although it is
/// the newest unread message; its own turn records the suppression and types
/// nothing.
#[test]
fn v3_a_paused_reminder_is_neither_counted_nor_covered_by_another_pointer() {
    let (supervisor, member, room, inputs) = fallback_fixture(false);
    supervisor.hold_room_wake_dispatch_for_tests(member);
    let request = deliver_to(&supervisor, room, member, "ordinary request");
    assert_eq!(request.receipts[0].status, RoomDeliveryStatus::Stored);
    fallback_save(&supervisor, room, fallback_settings(15));
    let now = Utc::now(); let mono = Instant::now(); let mut clock = room_fallbacks::FallbackClock::default();
    fallback_tick(&supervisor, &mut clock, now, mono, 0);
    fallback_tick(&supervisor, &mut clock, now, mono, 900);
    let reminder = fallback_posts(&supervisor, room).pop().unwrap();
    let RoomFeedItem::Message { message_id: reminder_id, .. } = reminder.item else { unreachable!() };
    let mut paused = fallback_settings(15); paused.enabled = false;
    fallback_save(&supervisor, room, paused);
    assert!(supervisor.inner.rooms.lock().get(room).unwrap().unread_for(member).iter().any(|event| event.cursor == reminder.cursor),
        "precondition: the paused reminder is still unread, with no receipt of its own");
    observe_current_run_paste_mode(&supervisor, member, "\x1b[?2004h");
    let head = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor: None }).unwrap().events.into_iter()
        .find(|event| matches!(event.item, RoomFeedItem::Message { message_id, .. } if message_id == request.message_id))
        .unwrap();
    let outcome = supervisor.wake_with_stale_plan_for_tests(&head, member, || {}).unwrap().expect("the request was attempted");
    assert_eq!(outcome.status, RoomDeliveryStatus::Written, "{outcome:?}");
    assert_eq!(inputs.lock()[0], typed_pointer(&supervisor, 1, request.message_id), "the paused reminder is not counted");
    let covered = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor: None }).unwrap().events.iter()
        .any(|event| matches!(event.item, RoomFeedItem::Delivery { message_id, pointer: Some(_), .. } if message_id == reminder_id));
    assert!(!covered, "the paused reminder never receives another request's cover");
    let typed = inputs.lock().len();
    supervisor.release_room_wake_dispatch_for_tests(member);
    wait_for_room_receipt(&supervisor, room, reminder_id, member, RoomDeliveryStatus::Held);
    wait_for_no_queued_room_wake(&supervisor, member);
    assert_eq!(inputs.lock().len(), typed, "nothing typed for the paused reminder");
    supervisor.shutdown().unwrap();
}

/// Reviewer 79ab53d8: a reminder counted and covered at Pending, then paused
/// while the writer waits, refuses the whole pointer before any byte. The
/// group is planned again without it: the request alone is typed, and the
/// reminder's own turn records the suppression.
#[test]
fn v3_a_covered_reminder_paused_before_the_paste_is_never_typed_through_another_head() {
    let (supervisor, member, room, inputs) = fallback_fixture(false);
    supervisor.hold_room_wake_dispatch_for_tests(member);
    let request = deliver_to(&supervisor, room, member, "ordinary request");
    fallback_save(&supervisor, room, fallback_settings(15));
    let now = Utc::now(); let mono = Instant::now(); let mut clock = room_fallbacks::FallbackClock::default();
    fallback_tick(&supervisor, &mut clock, now, mono, 0);
    fallback_tick(&supervisor, &mut clock, now, mono, 900);
    let reminder = fallback_posts(&supervisor, room).pop().unwrap();
    let RoomFeedItem::Message { message_id: reminder_id, .. } = reminder.item else { unreachable!() };
    observe_current_run_paste_mode(&supervisor, member, "\x1b[?2004h");
    let pause = supervisor.clone();
    let mut paused = false;
    supervisor.set_room_wake_before_write_for_tests(move |_| {
        if std::mem::replace(&mut paused, true) { return; }
        let mut settings = fallback_settings(15); settings.enabled = false; fallback_save(&pause, room, settings);
    });
    let head = supervisor.read_room_feed(ReadRoomFeedRequest { room_id: room, cursor: None }).unwrap().events.into_iter()
        .find(|event| matches!(event.item, RoomFeedItem::Message { message_id, .. } if message_id == request.message_id))
        .unwrap();
    let outcome = supervisor.wake_with_stale_plan_for_tests(&head, member, || {}).unwrap().expect("the request was attempted");
    assert_eq!((outcome.status, outcome.bytes_written), (RoomDeliveryStatus::Failed, 0), "{outcome:?}");
    assert!(inputs.lock().is_empty(), "nothing typed over the stale count: {:?}", inputs.lock());
    let refused = room_events(&supervisor, room).into_iter().any(|event| matches!(&event.item,
        RoomFeedItem::Delivery { message_id, status: RoomDeliveryStatus::Failed, pointer: Some(pointer), error: Some(error), .. }
            if *message_id == reminder_id && *pointer == request.message_id && error.contains("stopped being current")));
    assert!(refused, "the reminder's cover records why nothing was typed");
    supervisor.release_room_wake_dispatch_for_tests(member);
    wait_for_room_receipt(&supervisor, room, request.message_id, member, RoomDeliveryStatus::Written);
    wait_for_no_queued_room_wake(&supervisor, member);
    let typed = inputs.lock().clone();
    assert_eq!(typed[0], typed_pointer(&supervisor, 1, request.message_id), "planned again without the reminder: {typed:?}");
    assert_eq!(typed.iter().filter(|input| input.contains("new room message")).count(), 1, "{typed:?}");
    let latest = room_events(&supervisor, room).into_iter().filter_map(|event| match event.item {
        RoomFeedItem::Delivery { message_id, status, .. } if message_id == reminder_id => Some(status), _ => None }).last();
    assert_eq!(latest, Some(RoomDeliveryStatus::Held), "its own turn records the suppression");
    supervisor.shutdown().unwrap();
}
