//! Room save/load (A92): temporary runtimes and fixture harness stores only.
//! No harness, executable or named pipe starts.
use super::*;
use room_package::{HarnessStores, SEAMS};
use sha2::Digest as _;
use shared_types::{
    AddRoomMemberRequest, CreateSessionRequest, PermissionProfile, RemoveRoomMemberRequest,
    RoomFeedEvent, RoomImportMemberState as State, RoomImportReport, RoomSequence,
};

fn supervisor_at(root: &Path) -> SupervisorHandle {
    let work = root.join("work");
    fs::create_dir_all(&work).unwrap();
    SupervisorHandle::new(SupervisorConfig {
        working_root: work,
        runtime_dir: root.join("runtime"),
        pane_mcp_executable: None,
        heartbeat_interval: Some(Duration::from_secs(3600)),
        auto_restart_on_stall_sessions: Some(Vec::new()),
        auto_restart_stall_threshold: None,
        room_brief_on_join: false,
        task_folder: None,
    })
    .unwrap()
}

fn session(supervisor: &SupervisorHandle, label: &str, driver: DriverKind, profile: PermissionProfile) -> SessionId {
    supervisor.create_session(CreateSessionRequest { codex_transport: Default::default(), permissions: None, label: Some(label.into()), driver,
        permission_profile: profile, linux_working_directory: None, room_id: None, launch_choices: None })
        .unwrap().session_id
}

fn set_conversation(supervisor: &SupervisorHandle, id: SessionId, live: Option<&str>, hold: Option<RecoveryReference>) {
    {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id_mut(id).unwrap();
        slot.harness_session_id = live.map(Into::into);
        slot.recovery = hold;
    }
    supervisor.persist_slot_harness_state(id);
}

fn post(supervisor: &SupervisorHandle, room_id: RoomId, content: &str) {
    supervisor.post_room_message(PostRoomMessageRequest { room_id, request_id: None, content: content.into() }).unwrap();
}

fn folder_of(supervisor: &SupervisorHandle, id: SessionId) -> String {
    supervisor.inner.slots.lock().get_by_id(id).unwrap().definition.working_dir.clone()
}

struct Source { room: RoomId, claude: SessionId, grok: SessionId, terminal: SessionId, codex: SessionId, former: SessionId, claude_conversation: String }

/// Claude (with a conversation, unsafe), Terminal, Codex (held), a former
/// member who spoke, Grok added later (floor > 0), posts and a card.
fn source_room(supervisor: &SupervisorHandle) -> Source {
    let claude = session(supervisor, "Claude PRIM-1", DriverKind::Claude, PermissionProfile::Unsafe);
    let grok = session(supervisor, "Grok", DriverKind::Grok, PermissionProfile::Normal);
    let terminal = session(supervisor, "Terminal", DriverKind::GenericTerminal, PermissionProfile::Normal);
    let codex = session(supervisor, "Codex", DriverKind::Codex, PermissionProfile::Normal);
    let former = session(supervisor, "Former", DriverKind::Claude, PermissionProfile::Normal);
    let claude_conversation = Uuid::new_v4().to_string();
    set_conversation(supervisor, claude, Some(&claude_conversation), None);
    let codex_thread = Uuid::new_v4().to_string();
    set_conversation(supervisor, codex, Some(&codex_thread), Some(RecoveryReference::unresolved_codex(Some(codex_thread.clone()), false)));
    let room = supervisor.create_room(CreateRoomRequest { label: Some("Walk room".into()),
        member_ids: vec![claude, terminal, codex, former], brief_on_join: true,
        brief_template: Some("Brief for {room_label}".into()) }).unwrap().room_id;
    post(supervisor, room, "first");
    {
        let mut rooms = supervisor.inner.rooms.lock();
        let runtime = rooms.get_mut(room).unwrap();
        let revision = runtime.definition.membership_revision;
        runtime.append(RoomFeedItem::Message { guest_recipient_labels: Vec::new(), guest_recipient_ids: Vec::new(), ack_requested: false, fallback_origin: None, continuation_origin: None, request_id: "former-1".into(), message_id: Uuid::new_v4(),
            sender: RoomMessageSender::Session { session_id: former }, content: "from a former member".into(),
            recipient_ids: vec![claude], membership_revision: revision }).unwrap();
    }
    supervisor.remove_room_member(RemoveRoomMemberRequest { room_id: room, session_id: former }).unwrap();
    post(supervisor, room, "second");
    supervisor.add_room_member(AddRoomMemberRequest { room_id: room, session_id: grok }).unwrap();
    supervisor.inner.rooms.lock().get_mut(room).unwrap().post_card("card-1".into(), claude,
        serde_json::from_value(serde_json::json!({"key": "ship", "kind": "decision",
            "title": "Ship it?", "summary": "Waiting for Victor"})).unwrap()).unwrap();
    post(supervisor, room, "third");
    {
        let mut rooms = supervisor.inner.rooms.lock();
        let runtime = rooms.get_mut(room).unwrap();
        let message_id = feed_message_id(runtime);
        runtime.append(RoomFeedItem::Delivery { app_server_turn: None, title_busy: None, already_read: false, readmit: false, pointer: None, message_id, recipient_id: claude,
            status: shared_types::RoomDeliveryStatus::Written, bytes_written: 12, zero_write_confirmed: false,
            error: None, run_id: Some(Uuid::new_v4()), generation: Some(3) }).unwrap();
        // v3 W3: an actor-bearing ACK, so the id remapping covers Ack.actor too.
        runtime.append(RoomFeedItem::Ack { request_id: "ack-fixture".into(), message_id, recipient_id: claude,
            actor: shared_types::RoomAckActor::Session { session_id: claude },
            disposition: shared_types::RoomAckDisposition::Acked, note: "fixture ack".into(), outcome_ref: None }).unwrap();
    }
    Source { room, claude, grok, terminal, codex, former, claude_conversation }
}

fn feed_message_id(runtime: &RoomRuntime) -> Uuid {
    runtime.read(None, 0).unwrap().events.iter().rev().find_map(|event| match &event.item {
        RoomFeedItem::Message { message_id, .. } => Some(*message_id),
        _ => None,
    }).unwrap()
}

/// Fixture stores holding this Claude conversation under its exact folder.
fn claude_store(root: &Path, folder: &str, conversation: &str) -> HarnessStores {
    let project: String = folder.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let directory = root.join("claude").join("projects").join(project);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(format!("{conversation}.jsonl")), b"{}\n").unwrap();
    HarnessStores { claude: Some(root.join("claude")), grok: Some(root.join("grok")) }
}

fn no_stores() -> HarnessStores { HarnessStores { claude: None, grok: None } }

fn export(supervisor: &SupervisorHandle, room: RoomId, dir: &Path) -> PathBuf {
    let path = dir.join("walk.prim1room");
    supervisor.export_room(room, &path).unwrap();
    path
}

/// Catalog bytes plus every file under rooms/: a refusal must change none.
fn fingerprint(supervisor: &SupervisorHandle) -> Vec<(String, Vec<u8>)> {
    let runtime = supervisor.runtime_dir();
    let mut files = vec![
        (SESSION_CATALOG_FILE_NAME.to_owned(), fs::read(runtime.join(SESSION_CATALOG_FILE_NAME)).unwrap()),
        (ROOM_CATALOG_FILE_NAME.to_owned(), fs::read(runtime.join(ROOM_CATALOG_FILE_NAME)).unwrap()),
    ];
    if let Ok(entries) = fs::read_dir(runtime.join("rooms")) {
        for entry in entries.map(Result::unwrap) {
            files.push((entry.file_name().to_string_lossy().into_owned(), fs::read(entry.path()).unwrap()));
        }
    }
    files.sort();
    files
}

fn feed(supervisor: &SupervisorHandle, room_id: RoomId) -> Vec<RoomFeedEvent> {
    supervisor.read_room_feed(ReadRoomFeedRequest { room_id, cursor: None }).unwrap().events
}

fn feed_session_ids(events: &[RoomFeedEvent]) -> Vec<SessionId> {
    events.iter().flat_map(|event| match &event.item {
        RoomFeedItem::Card { author, .. } => vec![*author],
        RoomFeedItem::Message { sender, recipient_ids, .. } => {
            let mut ids = recipient_ids.clone();
            if let RoomMessageSender::Session { session_id } = sender { ids.push(*session_id); }
            ids
        }
        RoomFeedItem::Membership { session_id, .. } => vec![*session_id],
        RoomFeedItem::Delivery { recipient_id, .. } | RoomFeedItem::Waiting { recipient_id, .. } | RoomFeedItem::InputObserved { recipient_id, .. } | RoomFeedItem::SubmitAttempt { recipient_id, .. } => vec![*recipient_id],
        RoomFeedItem::Ack { recipient_id, actor, .. } => {
            let mut ids = vec![*recipient_id];
            if let shared_types::RoomAckActor::Session { session_id } = actor { ids.push(*session_id); }
            ids
        }
        RoomFeedItem::Cancel { actor, not_typed, reached, .. } => {
            let mut ids: Vec<_> = not_typed.iter().chain(reached).copied().collect();
            if let shared_types::RoomAckActor::Session { session_id } = actor { ids.push(*session_id); }
            ids
        }
        RoomFeedItem::GuestAck { .. } => Vec::new(),
    }).collect()
}

fn member<'a>(report: &'a RoomImportReport, label: &str) -> &'a shared_types::RoomImportMember {
    report.members.iter().find(|member| member.label == label).unwrap()
}

fn rewrite_manifest(path: &Path, edit: impl FnOnce(&mut serde_json::Value)) -> PathBuf {
    let bytes = fs::read(path).unwrap();
    let end = bytes.iter().position(|byte| *byte == b'\n').unwrap();
    let mut manifest: serde_json::Value = serde_json::from_slice(&bytes[..end]).unwrap();
    edit(&mut manifest);
    let mut out = serde_json::to_vec(&manifest).unwrap();
    out.extend_from_slice(&bytes[end..]);
    let edited = path.with_file_name(format!("edited-{}.prim1room", Uuid::new_v4()));
    fs::write(&edited, out).unwrap();
    edited
}

/// Edit the feed lines of a saved file; `reseal` recomputes only the digest,
/// as a forger would, so the count and replay checks are still exercised.
fn with_log(path: &Path, edit: impl FnOnce(&mut Vec<Vec<u8>>), reseal: bool) -> PathBuf {
    let bytes = fs::read(path).unwrap();
    let mut lines: Vec<Vec<u8>> = bytes.split_inclusive(|byte| *byte == b'\n').map(<[u8]>::to_vec).collect();
    let mut manifest: serde_json::Value = serde_json::from_slice(&lines.remove(0)).unwrap();
    edit(&mut lines);
    let log = lines.concat();
    if reseal { manifest["feed"]["sha256"] = serde_json::json!(format!("{:x}", sha2::Sha256::digest(&log))); }
    let mut out = serde_json::to_vec(&manifest).unwrap();
    out.push(b'\n');
    out.extend(log);
    let edited = path.with_file_name(format!("edited-{}.prim1room", Uuid::new_v4()));
    fs::write(&edited, out).unwrap();
    edited
}

/// The expected loaded event: the same record with only room, epoch and
/// session IDs remapped.
fn remapped(event: &RoomFeedEvent, room: RoomId, epoch: Uuid, map: &HashMap<SessionId, SessionId>) -> RoomFeedEvent {
    let mut event = event.clone();
    event.room_id = room;
    event.cursor.epoch = epoch;
    match &mut event.item {
        RoomFeedItem::Card { author, .. } => *author = map[author],
        RoomFeedItem::Message { sender, recipient_ids, .. } => {
            if let RoomMessageSender::Session { session_id } = sender { *session_id = map[session_id]; }
            recipient_ids.iter_mut().for_each(|id| *id = map[id]);
        }
        RoomFeedItem::Membership { session_id, .. } => *session_id = map[session_id],
        RoomFeedItem::Delivery { recipient_id, .. } | RoomFeedItem::Waiting { recipient_id, .. } | RoomFeedItem::InputObserved { recipient_id, .. } | RoomFeedItem::SubmitAttempt { recipient_id, .. } => *recipient_id = map[recipient_id],
        RoomFeedItem::Ack { recipient_id, actor, .. } => {
            *recipient_id = map[recipient_id];
            if let shared_types::RoomAckActor::Session { session_id } = actor { *session_id = map[session_id]; }
        }
        RoomFeedItem::Cancel { actor, not_typed, reached, .. } => {
            if let shared_types::RoomAckActor::Session { session_id } = actor { *session_id = map[session_id]; }
            not_typed.iter_mut().chain(reached.iter_mut()).for_each(|id| *id = map[id]);
        }
        RoomFeedItem::GuestAck { .. } => {}
    }
    event
}

fn floors(supervisor: &SupervisorHandle, room: RoomId) -> HashMap<SessionId, RoomSequence> {
    supervisor.inner.rooms.lock().catalog.rooms.iter().find(|row| row.room_id == room).unwrap().join_floors.clone().unwrap()
}

#[test]
fn round_trip_keeps_definition_feed_and_floors_under_new_ids_and_starts_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let a = supervisor_at(&temp.path().join("a"));
    let source = source_room(&a);
    let path = export(&a, source.room, temp.path());
    let first_line = fs::read_to_string(&path).unwrap().lines().next().unwrap().to_owned();
    assert!(first_line.starts_with("{\"record\":\"prim1_room_package\",\"version\":1"), "{first_line}");
    assert!(!first_line.contains("\"last_launch\"") && !first_line.contains("\"identity\"") && !first_line.contains("\"running_at_shutdown\""),
        "no run state or folder identity travels");
    let source_floors = floors(&a, source.room);
    let source_feed = feed(&a, source.room);

    let b_root = temp.path().join("b");
    let b = supervisor_at(&b_root);
    let stores = claude_store(temp.path(), &folder_of(&a, source.claude), &source.claude_conversation);
    let report = b.import_room_with_stores(&path, None, &stores).unwrap();
    let states: Vec<_> = report.members.iter().map(|member| (member.label.as_str(), member.state)).collect();
    assert_eq!(states, [("Claude PRIM-1", State::Resumes), ("Terminal", State::StartsFresh),
        ("Codex", State::Held), ("Grok", State::StartsFresh)]);
    assert_eq!(member(&report, "Claude PRIM-1").permission_profile, PermissionProfile::Unsafe, "kept as saved and reported");
    assert_eq!(member(&report, "Claude PRIM-1").note.as_deref(), Some("close it in the PRIM-1 it came from first"));
    assert!(report.warnings.contains(&"1 member resumes a conversation that the PRIM-1 this room came from may still be running. Close it there before you wake it here, so no agent runs in two PRIM-1s.".to_owned()),
        "{:?}", report.warnings);
    assert!(member(&report, "Terminal").note.is_none(), "only resuming members get the note");
    let map: HashMap<SessionId, SessionId> = [(source.claude, "Claude PRIM-1"), (source.terminal, "Terminal"),
        (source.codex, "Codex"), (source.grok, "Grok")].into_iter()
        .map(|(old, label)| (old, member(&report, label).session_id)).collect();
    assert!(map.iter().all(|(old, new)| old != new) && report.room.room_id != source.room);
    let room = report.room.room_id;
    let check = |b: &SupervisorHandle| {
        let snapshot = b.snapshot();
        let room_snapshot = snapshot.rooms.iter().find(|candidate| candidate.room_id == room).unwrap();
        assert_eq!(room_snapshot.label, "Walk room");
        assert!(!room_snapshot.resume_on_restart);
        assert_eq!(room_snapshot.member_ids, report.room.member_ids);
        let sessions: HashMap<_, _> = snapshot.sessions.iter().map(|session| (session.session_id, session)).collect();
        let claude = sessions[&map[&source.claude]];
        assert_eq!(claude.permission_profile, PermissionProfile::Unsafe);
        assert!(claude.resume_available && !claude.running && !claude.was_running_at_shutdown);
        assert_eq!(sessions[&map[&source.codex]].recovery.as_ref().unwrap().original_id, a.inner.slots.lock()
            .get_by_id(source.codex).unwrap().harness_session_id);
        assert!(sessions.values().all(|session| !session.running));
        let definition = b.inner.rooms.lock().catalog.rooms.iter().find(|row| row.room_id == room).unwrap().clone();
        assert_eq!(definition.brief_template.as_deref(), Some("Brief for {room_label}"));
        assert!(definition.brief_on_join);
        assert_eq!(floors(b, room), source_floors.iter().filter(|(id, _)| map.contains_key(id))
            .map(|(id, floor)| (map[id], *floor)).collect());
        assert!(floors(b, room)[&map[&source.grok]] > 0, "the later member keeps its boundary");
    };
    check(&b);
    // Every event is equal field for field (content, cards, receipts with
    // their run IDs) except room, epoch and session IDs, which follow one
    // consistent map; the former member gets a fresh ID no session here has.
    let loaded_feed = feed(&b, room);
    assert_eq!(loaded_feed.len(), source_feed.len());
    let epoch = loaded_feed[0].cursor.epoch;
    assert_ne!(epoch, source_feed[0].cursor.epoch);
    let mut seen: HashMap<SessionId, SessionId> = map.clone();
    for (old, new) in source_feed.iter().zip(&loaded_feed) {
        for (old_id, new_id) in feed_session_ids(std::slice::from_ref(old)).into_iter().zip(feed_session_ids(std::slice::from_ref(new))) {
            assert_eq!(*seen.entry(old_id).or_insert(new_id), new_id, "one consistent mapping");
        }
        assert_eq!(remapped(old, room, epoch, &seen), *new);
    }
    assert!(source_feed.iter().any(|event| matches!(event.item, RoomFeedItem::Card { .. }))
        && source_feed.iter().any(|event| matches!(event.item, RoomFeedItem::Delivery { .. })), "cards and receipts are covered");
    assert!(!b.snapshot().sessions.iter().any(|session| session.session_id == seen[&source.former]));
    drop(b);
    check(&supervisor_at(&b_root));
}

#[test]
fn same_runtime_load_holds_the_copy_and_old_lines_never_credit_a_live_session() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("a");
    let a = supervisor_at(&root);
    let source = source_room(&a);
    let path = export(&a, source.room, temp.path());
    let existing: HashSet<SessionId> = a.snapshot().sessions.iter().map(|session| session.session_id).collect();
    let stores = claude_store(temp.path(), &folder_of(&a, source.claude), &source.claude_conversation);
    let report = a.import_room_with_stores(&path, None, &stores).unwrap();
    assert_eq!(report.room.label, "Walk room (loaded)");
    let copy = member(&report, "Claude PRIM-1");
    assert_eq!(copy.state, State::Held);
    assert!(copy.note.as_deref().unwrap().contains("\"Claude PRIM-1\" in room \"Walk room\""), "{:?}", copy.note);
    let copy_id = copy.session_id;
    let loaded_ids = feed_session_ids(&feed(&a, report.room.room_id));
    assert!(!loaded_ids.is_empty() && loaded_ids.iter().all(|id| !existing.contains(id)),
        "no loaded line names a session that already lives here");
    drop(a);
    let a = supervisor_at(&root);
    let slots = a.inner.slots.lock();
    let original = slots.get_by_id(source.claude).unwrap();
    assert!(original.recovery.is_none() && original.snapshot().resume_available, "no duplicate hold on the original");
    let held = slots.get_by_id(copy_id).unwrap();
    assert!(held.harness_session_id.is_none());
    assert_eq!(held.recovery.as_ref().unwrap().original_id.as_deref(), Some(source.claude_conversation.as_str()));
}

#[test]
fn collisions_include_held_ids_and_staged_members_and_holds_keep_their_original_id() {
    let temp = tempfile::tempdir().unwrap();
    let a = supervisor_at(&temp.path().join("a"));
    let (x, y) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
    let twin_one = session(&a, "Twin one", DriverKind::Claude, PermissionProfile::Normal);
    let twin_two = session(&a, "Twin two", DriverKind::Claude, PermissionProfile::Normal);
    let keeper = session(&a, "Keeper", DriverKind::Claude, PermissionProfile::Normal);
    let resumer = session(&a, "Resumer", DriverKind::Claude, PermissionProfile::Normal);
    let z = Uuid::new_v4().to_string();
    set_conversation(&a, twin_one, Some(&x), None);
    a.inner.slots.lock().get_by_id_mut(twin_two).unwrap().harness_session_id = Some(x.clone());
    // A hold without a live ID: its conversation identity must survive.
    let longest = "r".repeat(2048);
    set_conversation(&a, keeper, None, Some(RecoveryReference::resume_refused(Some(y.clone()), longest.clone())));
    set_conversation(&a, resumer, Some(&z), None);
    let room = a.create_room(CreateRoomRequest { label: Some("Collide".into()),
        member_ids: vec![twin_one, twin_two, keeper, resumer], brief_on_join: false, brief_template: None }).unwrap().room_id;
    let path = export(&a, room, temp.path());

    let b = supervisor_at(&temp.path().join("b"));
    let holder = session(&b, "Holder", DriverKind::Claude, PermissionProfile::Normal);
    set_conversation(&b, holder, None, Some(RecoveryReference::resume_refused(Some(z.clone()), "held here".into())));
    let report = b.import_room_with_stores(&path, None, &claude_store(temp.path(), &folder_of(&a, resumer), &z)).unwrap();
    for twin in ["Twin one", "Twin two"] {
        let staged = member(&report, twin);
        assert_eq!(staged.state, State::Held);
        assert!(staged.note.as_deref().unwrap().contains("in the same room file"), "{:?}", staged.note);
    }
    let resumer_copy = member(&report, "Resumer");
    assert_eq!(resumer_copy.state, State::Held, "a held original ID here is in use too");
    assert!(resumer_copy.note.as_deref().unwrap().contains("\"Holder\" in the lobby"));
    let keeper_copy = member(&report, "Keeper");
    assert_eq!(keeper_copy.state, State::Held);
    assert!(keeper_copy.note.as_deref().unwrap().starts_with("Held in the saved room (harness_resume_refusal): "));
    let hold = b.inner.slots.lock().get_by_id(keeper_copy.session_id).unwrap().recovery.clone().unwrap();
    assert_eq!((hold.original_id.as_deref(), hold.provenance.as_str(), &hold.reason),
        (Some(y.as_str()), "harness_resume_refusal", &longest), "a saved hold is kept exactly, never wrapped");
    // Saving and loading again still carries the held conversation ID.
    let again = b.export_room(report.room.room_id, &temp.path().join("again.prim1room")).unwrap();
    let c = supervisor_at(&temp.path().join("c"));
    let report = c.import_room_with_stores(Path::new(&again.path), None, &no_stores()).unwrap();
    let keeper_again = member(&report, "Keeper").session_id;
    let hold = c.inner.slots.lock().get_by_id(keeper_again).unwrap().recovery.clone().unwrap();
    assert_eq!((hold.original_id.as_deref(), &hold.reason), (Some(y.as_str()), &longest), "unchanged after a second cycle");
}

#[test]
fn probes_check_exact_names_and_a_missing_folder_needs_history() {
    let temp = tempfile::tempdir().unwrap();
    let a = supervisor_at(&temp.path().join("a"));
    let moved = temp.path().join("moved-folder");
    fs::create_dir_all(&moved).unwrap();
    let ids: Vec<_> = (0..4).map(|_| Uuid::new_v4().to_string()).collect();
    let here = session(&a, "Claude here", DriverKind::Claude, PermissionProfile::Normal);
    let elsewhere = session(&a, "Claude elsewhere", DriverKind::Claude, PermissionProfile::Normal);
    let grok = session(&a, "Grok here", DriverKind::Grok, PermissionProfile::Normal);
    let lost = session(&a, "Claude lost folder", DriverKind::Claude, PermissionProfile::Normal);
    a.set_session_working_directory(lost, &moved).unwrap();
    for (id, conversation) in [here, elsewhere, grok, lost].into_iter().zip(&ids) {
        set_conversation(&a, id, Some(conversation), None);
    }
    let room = a.create_room(CreateRoomRequest { label: Some("Probes".into()),
        member_ids: vec![here, elsewhere, grok, lost], brief_on_join: false, brief_template: None }).unwrap().room_id;
    let path = export(&a, room, temp.path());
    let folder = folder_of(&a, here);
    let stores = claude_store(temp.path(), &folder, &ids[0]);
    let other = temp.path().join("claude").join("projects").join("C--some-other-project");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join(format!("{}.jsonl", ids[1])), b"{}\n").unwrap();
    let encoded: String = folder.bytes().map(|byte| if byte.is_ascii_alphanumeric() { (byte as char).to_string() }
        else { format!("%{byte:02X}") }).collect();
    fs::create_dir_all(temp.path().join("grok").join("sessions").join(encoded).join(&ids[2])).unwrap();
    fs::remove_dir(&moved).unwrap();

    let b = supervisor_at(&temp.path().join("b"));
    let report = b.import_room_with_stores(&path, None, &stores).unwrap();
    assert_eq!(member(&report, "Claude here").state, State::Resumes);
    assert_eq!(member(&report, "Grok here").state, State::Resumes);
    assert!(report.warnings.iter().any(|warning| warning.starts_with("2 members resume conversations that the PRIM-1 this room came from may still be running. Close them there")));
    let elsewhere = member(&report, "Claude elsewhere");
    assert_eq!(elsewhere.state, State::HistoryNeeded);
    assert!(elsewhere.note.as_deref().unwrap().contains("C--some-other-project"));
    let lost = member(&report, "Claude lost folder");
    assert_eq!(lost.state, State::HistoryNeeded);
    assert!(lost.note.as_deref().unwrap().contains("moved-folder"));
    assert_eq!(folder_of(&b, lost.session_id), b.inner.catalog.lock().workspace_preference.canonical_path);
    // No store at all: history needed, never a guess.
    let c = supervisor_at(&temp.path().join("c"));
    let report = c.import_room_with_stores(&path, None, &no_stores()).unwrap();
    assert!(report.members.iter().all(|member| member.state == State::HistoryNeeded));
    assert!(c.snapshot().sessions.iter().all(|session| !session.resume_available));
}

#[test]
fn floors_are_normalized_before_the_new_epoch_and_v1_rooms_fall_back_to_the_tail() {
    let temp = tempfile::tempdir().unwrap();
    let a = supervisor_at(&temp.path().join("a"));
    let source = source_room(&a);
    let path = export(&a, source.room, temp.path());
    let tail = feed(&a, source.room).last().unwrap().cursor.sequence;
    let b = supervisor_at(&temp.path().join("b"));
    let cases: [(&str, Box<dyn FnOnce(&mut serde_json::Value)>); 3] = [
        ("stale epoch", Box::new(|manifest| manifest["room"]["join_floor_epoch"] = serde_json::json!(Uuid::new_v4()))),
        ("floor past the tail", Box::new(move |manifest| {
            let id = manifest["room"]["member_ids"][0].as_str().unwrap().to_owned();
            manifest["room"]["join_floors"][id] = serde_json::json!(tail + 1);
        })),
        ("v1: neither field", Box::new(|manifest| {
            let room = manifest["room"].as_object_mut().unwrap();
            room.remove("join_floor_epoch");
            room.remove("join_floors");
        })),
    ];
    for (case, edit) in cases {
        let report = b.import_room_with_stores(&rewrite_manifest(&path, edit), None, &no_stores()).unwrap();
        assert!(floors(&b, report.room.room_id).values().all(|floor| *floor == tail), "{case}");
        assert!(report.warnings.iter().any(|warning| warning.contains("see only messages posted after this load")), "{case}");
        assert_eq!(feed(&b, report.room.room_id).len() as u64, tail, "{case}: the operator still reads the whole feed");
    }
}

#[test]
fn untrusted_files_are_refused_and_leave_the_runtime_byte_identical() {
    let temp = tempfile::tempdir().unwrap();
    let a = supervisor_at(&temp.path().join("a"));
    let source = source_room(&a);
    let path = export(&a, source.room, temp.path());
    let bytes = fs::read(&path).unwrap();
    let b = supervisor_at(&temp.path().join("b"));
    let before = fingerprint(&b);
    let write = |name: &str, content: &[u8]| { let file = temp.path().join(name); fs::write(&file, content).unwrap(); file };
    let oversized = temp.path().join("oversized.prim1room");
    File::create(&oversized).unwrap().set_len(257 * 1024 * 1024 + 1).unwrap();
    let cases = [
        (write("truncated.prim1room", &bytes[..bytes.len() - 1]), "incomplete"),
        (with_log(&path, |lines| { lines.pop(); }, false), "incomplete"),
        (with_log(&path, |lines| lines.truncate(1), false), "incomplete"),
        (with_log(&path, |lines| { lines.pop(); }, true), "record count or last sequence"),
        (with_log(&path, |lines| { lines.remove(2); }, true), "sequence mismatch"),
        (oversized, "exceeds 257 MiB"),
        (write("other.prim1room", b"{\"hello\":1}\n"), "not a PRIM-1 room"),
        (rewrite_manifest(&path, |m| m["version"] = serde_json::json!(2)), "newer PRIM-1 (package version 2)"),
        (rewrite_manifest(&path, |m| m["extra"] = serde_json::json!(true)), "damaged at line 1"),
        (rewrite_manifest(&path, |m| m["sessions"][0]["harness_session_id"] = serde_json::json!("..\\x")), "invalid conversation ID"),
        (rewrite_manifest(&path, |m| { let first = m["room"]["member_ids"][0].clone();
            m["room"]["member_ids"].as_array_mut().unwrap().push(first); }), "listed twice"),
        (rewrite_manifest(&path, |m| m["room"]["room_id"] = serde_json::json!(Uuid::new_v4())), "damaged at line 2"),
        (rewrite_manifest(&path, |m| m["room"]["brief_template"] = serde_json::json!("x".repeat(9000))), "maximum is 8192"),
    ];
    for (file, expected) in cases {
        let error = b.import_room_with_stores(&file, None, &no_stores()).unwrap_err().to_string();
        assert!(error.contains(expected) && error.ends_with("Nothing was changed."), "{expected}: {error}");
        assert_eq!(fingerprint(&b), before, "{expected}");
    }
    assert!(b.snapshot().sessions.is_empty() && b.snapshot().rooms.is_empty());
}

#[test]
fn catalog_failures_roll_back_or_stay_held_with_an_honest_message() {
    let temp = tempfile::tempdir().unwrap();
    let a = supervisor_at(&temp.path().join("a"));
    let source = source_room(&a);
    let path = export(&a, source.room, temp.path());
    let stores = claude_store(temp.path(), &folder_of(&a, source.claude), &source.claude_conversation);
    let b_root = temp.path().join("b");
    let b = supervisor_at(&b_root);
    let before = fingerprint(&b);
    for room_catalog in [false, true] {
        if room_catalog { b.fail_next_room_catalog_write_for_tests(); } else { b.fail_next_catalog_write_for_tests(); }
        let error = b.import_room_with_stores(&path, None, &stores).unwrap_err().to_string();
        assert!(error.starts_with("Loading failed") && error.ends_with("Nothing was changed."), "{error}");
        assert_eq!(fingerprint(&b), before);
        assert!(b.snapshot().sessions.is_empty() && b.snapshot().rooms.is_empty());
    }
    // The room catalog fails and so does undoing the held sessions.
    b.fail_next_room_catalog_write_for_tests();
    let failing = b.clone();
    SEAMS.with(|seams| seams.borrow_mut().insert("held_saved", Box::new(move || { failing.fail_next_catalog_write_for_tests(); Ok(()) })));
    let error = b.import_room_with_stores(&path, None, &stores).unwrap_err().to_string();
    assert!(error.contains("stay in the lobby, held, and resume nothing"), "{error}");
    let held = |b: &SupervisorHandle| {
        let snapshot = b.snapshot();
        assert!(snapshot.rooms.is_empty() && snapshot.sessions.len() == 4);
        assert!(snapshot.sessions.iter().all(|session| !session.resume_available
            && session.recovery.as_ref().is_some_and(|hold| hold.provenance == "room_import_pending")));
    };
    held(&b);
    drop(b);
    held(&supervisor_at(&b_root));
}

#[test]
fn a_crash_between_the_catalog_saves_leaves_only_held_copies() {
    for point in ["held_saved", "room_saved"] {
        let temp = tempfile::tempdir().unwrap();
        let a = supervisor_at(&temp.path().join("a"));
        let source = source_room(&a);
        let path = export(&a, source.room, temp.path());
        let stores = claude_store(temp.path(), &folder_of(&a, source.claude), &source.claude_conversation);
        let root = temp.path().join("b");
        let b = supervisor_at(&root);
        SEAMS.with(|seams| seams.borrow_mut().insert(point, Box::new(|| Err(anyhow!("simulated power cut")))));
        assert!(b.import_room_with_stores(&path, None, &stores).is_err());
        drop(b);
        let b = supervisor_at(&root);
        let snapshot = b.snapshot();
        assert_eq!(snapshot.rooms.len(), usize::from(point == "room_saved"), "{point}");
        assert_eq!(snapshot.sessions.len(), 4, "{point}");
        for session in &snapshot.sessions {
            let hold = session.recovery.as_ref().unwrap();
            assert!(!session.resume_available && hold.provenance == "room_import_pending", "{point}");
            assert!(hold.reason.contains("PRIM-1 stopped while loading room \"Walk room\""), "{}", hold.reason);
        }
        let claude = snapshot.sessions.iter().find(|session| session.label == "Claude PRIM-1").unwrap();
        assert_eq!(claude.recovery.as_ref().unwrap().original_id.as_deref(), Some(source.claude_conversation.as_str()));
    }
}

#[test]
fn import_releases_the_session_registry_before_the_room_catalog_write() {
    let temp = tempfile::tempdir().unwrap();
    let a = supervisor_at(&temp.path().join("a"));
    let source = source_room(&a);
    let path = export(&a, source.room, temp.path());
    let b = supervisor_at(&temp.path().join("b"));
    let probe = b.clone();
    let (free_tx, free_rx) = mpsc::sync_channel(1);
    *b.inner.room_catalog_before_write.lock() = Some(Box::new(move || {
        free_tx.send(probe.inner.slots.try_lock().is_some() && probe.inner.catalog.try_lock().is_some()).unwrap();
    }));
    b.import_room_with_stores(&path, None, &no_stores()).unwrap();
    assert!(free_rx.recv().unwrap(), "unrelated panes keep the session registry during room I/O");
}

#[test]
fn export_errors_restore_the_append_position_or_hold_the_writer() {
    for restore_fails in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("a");
        let a = supervisor_at(&root);
        let source = source_room(&a);
        let target = temp.path().join("never.prim1room");
        let tail = feed(&a, source.room).last().unwrap().cursor.sequence;
        a.inner.rooms.lock().get_mut(source.room).unwrap().fail_next_log_copy_for_tests(restore_fails);
        let error = a.export_room(source.room, &target).unwrap_err().to_string();
        assert!(error.contains("was not saved"), "{error}");
        assert!(!target.exists() && fs::read_dir(temp.path()).unwrap().all(|entry|
            !entry.unwrap().file_name().to_string_lossy().contains("never")), "no partial file");
        let held = a.snapshot().rooms.iter().find(|room| room.room_id == source.room).unwrap().history_replay_pending;
        assert_eq!(held, restore_fails, "a failed restore holds the writer instead of appending at a wrong position");
        post(&a, source.room, "after the failed save");
        drop(a);
        let a = supervisor_at(&root);
        let sequences: Vec<_> = feed(&a, source.room).iter().map(|event| event.cursor.sequence).collect();
        assert_eq!(sequences, (1..=tail + 1).collect::<Vec<_>>(), "the log replays cleanly with the new post at its end");
        a.export_room(source.room, &target).unwrap();
    }
}

/// Every file under a folder, recursively, with its bytes.
fn tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let (mut files, mut pending) = (Vec::new(), vec![root.to_path_buf()]);
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap().map(Result::unwrap) {
            if entry.file_type().unwrap().is_dir() { pending.push(entry.path()); }
            else { files.push((entry.path(), fs::read(entry.path()).unwrap())); }
        }
    }
    files.sort();
    files
}

/// Design case 7: a runtime written by this build, then reshaped to v1 (no
/// floor epoch or floors on its rooms, no launch choices on its sessions).
/// It holds two rooms; the PRIM-1 that wrote it is closed.
fn v1_copy(temp: &Path) -> (PathBuf, Source, RoomId) {
    let root = temp.join("v1");
    let a = supervisor_at(&root);
    let source = source_room(&a);
    let solo = session(&a, "Solo", DriverKind::Claude, PermissionProfile::Normal);
    let second = a.create_room(CreateRoomRequest { label: Some("Second room".into()), member_ids: vec![solo],
        brief_on_join: false, brief_template: None }).unwrap().room_id;
    post(&a, second, "only here");
    drop(a);
    let runtime = root.join("runtime");
    let reshape = |name: &str, list: &str, fields: &[&str]| {
        let path = runtime.join(name);
        let mut catalog: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for row in catalog[list].as_array_mut().unwrap() {
            for field in fields { row.as_object_mut().unwrap().remove(*field); }
        }
        fs::write(&path, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();
    };
    reshape(ROOM_CATALOG_FILE_NAME, "rooms", &["join_floor_epoch", "join_floors"]);
    reshape(SESSION_CATALOG_FILE_NAME, "sessions", &["launch_choices"]);
    (runtime.join(ROOM_CATALOG_FILE_NAME), source, second)
}

#[test]
fn runtime_copy_v1_room_loads_read_only_with_the_tail_rule_and_a_torn_tail_left_out() {
    let temp = tempfile::tempdir().unwrap();
    let (catalog, source, second) = v1_copy(temp.path());
    let copy = catalog.parent().unwrap().to_path_buf();
    let torn = b"{\"record\":\"event\",\"ev";
    OpenOptions::new().append(true).open(copy.join("rooms").join(format!("{}.jsonl", source.room))).unwrap()
        .write_all(torn).unwrap();
    let before = tree(&copy);
    let b = supervisor_at(&temp.path().join("b"));
    let sources = b.inspect_room_source(&catalog).unwrap();
    assert_eq!(sources.len(), 2);
    let walk = sources.iter().find(|summary| summary.source_room_id == source.room).unwrap();
    assert_eq!((walk.label.as_str(), walk.members), ("Walk room", 4));
    assert!(b.import_room_with_stores(&catalog, None, &no_stores()).unwrap_err().to_string().contains("has 2 rooms; choose one"));

    let report = b.import_room_with_stores(&catalog, Some(source.room), &no_stores()).unwrap();
    let room = report.room.room_id;
    assert_eq!((report.room.label.as_str(), report.members.len()), ("Walk room", 4));
    assert!(report.warnings.iter().any(|warning| warning.contains(&format!("last feed record was incomplete ({} bytes) and was left out", torn.len()))));
    assert!(report.warnings.iter().any(|warning| warning.contains("see only messages posted after this load")));
    assert_eq!(feed(&b, room).len() as u64, walk.records, "every complete record, none of the torn one");
    assert!(floors(&b, room).values().all(|floor| *floor == walk.records), "v1: the tail rule");
    assert_eq!(member(&report, "Codex").state, State::Held);
    let claude = member(&report, "Claude PRIM-1").session_id;
    assert_eq!(b.inner.slots.lock().get_by_id(claude).unwrap().launch_choices, session::default_launch_choices(DriverKind::Claude));

    let other = b.import_room_with_stores(&catalog, Some(second), &no_stores()).unwrap();
    assert_eq!((other.room.label.as_str(), other.members.len()), ("Second room", 1));
    assert_eq!(feed(&b, other.room.room_id).len(), 2, "its join and its one post");
    assert_eq!(tree(&copy), before, "the copy is never modified");
}

#[test]
fn runtime_copy_refuses_its_own_runtime_and_a_running_prim1() {
    let temp = tempfile::tempdir().unwrap();
    let a = supervisor_at(&temp.path().join("a"));
    let source = source_room(&a);
    let catalog = a.runtime_dir().join(ROOM_CATALOG_FILE_NAME);
    let b = supervisor_at(&temp.path().join("b"));
    let before = fingerprint(&b);
    // A is still running: its writers hold their logs.
    assert!(b.inspect_room_source(&catalog).unwrap_err().to_string().contains("A running PRIM-1 holds this room's log"));
    let error = b.import_room_with_stores(&catalog, Some(source.room), &no_stores()).unwrap_err().to_string();
    assert!(error.contains("A running PRIM-1 holds this room's log") && error.ends_with("Nothing was changed."), "{error}");
    assert_eq!(fingerprint(&b), before);
    for error in [a.inspect_room_source(&catalog).unwrap_err(),
        a.import_room_with_stores(&catalog, Some(source.room), &no_stores()).unwrap_err()] {
        assert!(error.to_string().contains("That folder is this PRIM-1's own runtime. Use Save… instead."), "{error}");
    }
    post(&a, source.room, "A's writer is untouched");
}

#[test]
fn native_hold_reasons_travel_unchanged_and_an_oversized_one_is_refused_by_name() {
    let temp = tempfile::tempdir().unwrap();
    let a = supervisor_at(&temp.path().join("a"));
    let held = session(&a, "Long reason", DriverKind::Claude, PermissionProfile::Normal);
    let conversation = Uuid::new_v4().to_string();
    // A run diagnosis has no bound of its own; 2049 characters is past the old limit.
    let reason = "d".repeat(2049);
    set_conversation(&a, held, Some(&conversation), Some(RecoveryReference::resume_refused(Some(conversation.clone()), reason.clone())));
    let room = a.create_room(CreateRoomRequest { label: Some("Diagnostics".into()), member_ids: vec![held],
        brief_on_join: false, brief_template: None }).unwrap().room_id;
    let path = export(&a, room, temp.path());
    let b = supervisor_at(&temp.path().join("b"));
    let loaded = b.import_room_with_stores(&path, None, &no_stores()).unwrap();
    let again = b.export_room(loaded.room.room_id, &temp.path().join("again.prim1room")).unwrap();
    let c = supervisor_at(&temp.path().join("c"));
    let report = c.import_room_with_stores(Path::new(&again.path), None, &no_stores()).unwrap();
    let hold = c.inner.slots.lock().get_by_id(report.members[0].session_id).unwrap().recovery.clone().unwrap();
    assert_eq!((hold.reason, hold.provenance.as_str()), (reason, "harness_resume_refusal"), "never cut, never wrapped");

    let too_long = "e".repeat(16 * 1024 + 1);
    set_conversation(&a, held, Some(&conversation), Some(RecoveryReference::resume_refused(Some(conversation.clone()), too_long)));
    let target = temp.path().join("refused.prim1room");
    let error = a.export_room(room, &target).unwrap_err().to_string();
    assert!(error.contains("Member \"Long reason\" has a hold reason longer than 16384 characters"), "{error}");
    assert!(!target.exists());
}

#[test]
fn runtime_copy_with_a_duplicated_room_or_session_id_is_refused_by_id() {
    let temp = tempfile::tempdir().unwrap();
    let (catalog, source, _) = v1_copy(temp.path());
    let copy = catalog.parent().unwrap().to_path_buf();
    let b = supervisor_at(&temp.path().join("b"));
    for (name, list) in [(ROOM_CATALOG_FILE_NAME, "rooms"), (SESSION_CATALOG_FILE_NAME, "sessions")] {
        let path = copy.join(name);
        let original = fs::read(&path).unwrap();
        let mut edited: serde_json::Value = serde_json::from_slice(&original).unwrap();
        let rows = edited[list].as_array_mut().unwrap();
        let mut twin = rows[0].clone();
        twin["label"] = serde_json::json!("A different definition");
        rows.push(twin);
        let id = rows[0][if list == "rooms" { "room_id" } else { "session_id" }].as_str().unwrap().to_owned();
        fs::write(&path, serde_json::to_vec_pretty(&edited).unwrap()).unwrap();
        let before = tree(&copy);
        let expected = format!("lists {} {id} more than once", &list[..list.len() - 1]);
        assert!(b.inspect_room_source(&catalog).unwrap_err().to_string().contains(&expected), "{list}");
        let error = b.import_room_with_stores(&catalog, Some(source.room), &no_stores()).unwrap_err().to_string();
        assert!(error.contains(&expected) && error.ends_with("Nothing was changed."), "{error}");
        assert_eq!(tree(&copy), before, "{list}: the copy is not changed");
        fs::write(&path, original).unwrap();
    }
    assert!(b.snapshot().rooms.is_empty() && b.snapshot().sessions.is_empty());
}

#[test]
fn an_unreadable_room_in_a_copy_is_listed_unavailable_and_blocks_only_itself() {
    let temp = tempfile::tempdir().unwrap();
    let (catalog, source, second) = v1_copy(temp.path());
    fs::remove_file(catalog.parent().unwrap().join("rooms").join(format!("{second}.jsonl"))).unwrap();
    let b = supervisor_at(&temp.path().join("b"));
    let sources = b.inspect_room_source(&catalog).unwrap();
    assert_eq!(sources.len(), 2, "every room is still listed");
    let walk = sources.iter().find(|summary| summary.source_room_id == source.room).unwrap();
    assert!(walk.unavailable.is_none() && walk.records > 0);
    let missing = sources.iter().find(|summary| summary.source_room_id == second).unwrap();
    let reason = missing.unavailable.clone().unwrap();
    assert!(reason.contains("Room \"Second room\" has no feed in the copy") && reason.ends_with(".jsonl is missing."), "{reason}");
    assert!(!reason.contains(r"\\?\") && !reason.contains("os error"), "plain words only: {reason}");
    assert_eq!(b.import_room_with_stores(&catalog, Some(source.room), &no_stores()).unwrap().room.label, "Walk room");
    let error = b.import_room_with_stores(&catalog, Some(second), &no_stores()).unwrap_err().to_string();
    assert!(error.contains("Room \"Second room\" has no feed in the copy") && error.ends_with("Nothing was changed."), "{error}");
    assert_eq!(b.snapshot().rooms.len(), 1);
}
