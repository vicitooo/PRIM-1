//! Small owned filesystem fixtures only. No supervisor/background runtime,
//! desktop, task, transport, installed profile or harness is opened. Reparse
//! coverage uses the repository's existing Windows junction fixture helper.
use super::*;

struct Fixture {
    _root: tempfile::TempDir,
    source: PathBuf,
    local: PathBuf,
    parent: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let local = root.path().join("local");
        let parent = root.path().join("attempts");
        for path in [&source, &local, &parent] {
            fs::create_dir(path).unwrap();
        }
        // Explicit desktop-backed fixture setup; the API never creates this.
        fs::write(source.join(DESKTOP_LOCK), []).unwrap();
        let catalog = SessionCatalogV1::empty(QualifiedWorkingDirectory {
            namespace: WorkingDirectoryNamespace::Windows,
            canonical_path: root.path().join("work").to_string_lossy().into_owned(),
            identity: "fixture-volume-and-directory-id".into(),
        });
        fs::write(
            source.join(SESSION_CATALOG_FILE_NAME),
            serde_json::to_vec(&catalog).unwrap(),
        )
        .unwrap();
        fs::write(
            source.join(ROOM_CATALOG_FILE_NAME),
            serde_json::to_vec(&RoomCatalogV1::empty()).unwrap(),
        )
        .unwrap();
        Self {
            _root: root,
            source,
            local,
            parent,
        }
    }

    fn request(&self) -> ClosedCheckpointRequest {
        let id = Uuid::new_v4();
        ClosedCheckpointRequest {
            source_runtime: self.source.clone(),
            attempt_root: self.parent.join(id.to_string()),
            attempt_id: id,
            provenance: ClosedCheckpointProvenance {
                closure_receipt_sha256: "1".repeat(64),
                old_executable_sha256: "2".repeat(64),
                candidate_executable_sha256: "3".repeat(64),
                reviewed_source_revision: "4".repeat(40),
                build_receipt_sha256: "5".repeat(64),
            },
        }
    }

    fn room(&self, legacy_epoch: bool, tail: &[u8]) -> (Uuid, String) {
        let id = Uuid::new_v4();
        let epoch = Uuid::new_v4();
        let catalog = RoomCatalogV1 {
            schema_version: 1,
            rooms: vec![PersistedRoomV1 {
                fallbacks: Default::default(),
                join_floors: Some(HashMap::new()),
                join_floor_epoch: (!legacy_epoch).then_some(epoch),
                history_restarted: false,
                room_id: id,
                label: "Closed fixture".into(),
                resume_on_restart: true,
                member_ids: vec![],
                membership_revision: 1,
                brief_on_join: false,
                brief_template: None,
                briefed_member_ids: Some(vec![]),
            }],
        };
        fs::write(
            self.source.join(ROOM_CATALOG_FILE_NAME),
            serde_json::to_vec(&catalog).unwrap(),
        )
        .unwrap();
        fs::create_dir(self.source.join("rooms")).unwrap();
        let relative = format!("rooms/{id}.jsonl");
        let mut bytes = serde_json::to_vec(
            &serde_json::json!({"record":"header", "version":1, "room_id":id, "epoch":epoch}),
        )
        .unwrap();
        bytes.push(b'\n');
        bytes.extend_from_slice(tail);
        fs::write(self.source.join(&relative), bytes).unwrap();
        (id, relative)
    }

    fn bytes(&self) -> BTreeMap<String, Vec<u8>> {
        inventory(&self.source)
            .unwrap()
            .into_iter()
            .filter(|e| !e.directory)
            .map(|entry| {
                (
                    entry.path.clone(),
                    fs::read(self.source.join(entry.path)).unwrap(),
                )
            })
            .collect()
    }
}

#[test]
fn closed_capture_preserves_source_and_separates_evidence_and_writable_copy() {
    let fixture = Fixture::new();
    fixture.room(false, b"");
    fs::create_dir(fixture.source.join("audit")).unwrap();
    fs::write(
        fixture.source.join("audit/2026-10-14.jsonl"),
        b"audit evidence\n",
    )
    .unwrap();
    fs::write(
        fixture.source.join("desktop-events.jsonl"),
        b"desktop evidence\n",
    )
    .unwrap();
    let original = fixture.bytes();
    let request = fixture.request();
    let target = request.attempt_root.clone();
    let manifest = capture(request, &fixture.local, |_| Ok(())).unwrap();
    assert_eq!(fixture.bytes(), original);
    assert!(manifest.compatibility.copy_replay_ok);
    assert!(!manifest.compatibility.restart_authorized);
    assert!(!manifest.compatibility.settings_preservation_verified);
    assert!(manifest.compatibility.changes.is_empty());
    assert!(target.join("checkpoint-v1.json").is_file());
    assert!(target.join("evidence/audit/2026-10-14.jsonl").is_file());
    assert!(!target.join("runtime/audit").exists());
    assert!(!target.join("runtime/desktop-instance.lock").exists());
    assert_eq!(fs::read_dir(target.join("profile")).unwrap().count(), 0);
    let pristine = fs::read(target.join("checkpoint").join(SESSION_CATALOG_FILE_NAME)).unwrap();
    fs::write(
        target.join("runtime").join(SESSION_CATALOG_FILE_NAME),
        b"independent edit",
    )
    .unwrap();
    assert_eq!(
        fs::read(target.join("checkpoint").join(SESSION_CATALOG_FILE_NAME)).unwrap(),
        pristine
    );
    assert_eq!(fixture.bytes(), original);
}

#[test]
fn retained_catalog_handles_deny_write_delete_and_atomic_replacement() {
    let fixture = Fixture::new();
    let original = fixture.bytes();
    let replacement = fixture.parent.join("replacement.json");
    fs::write(&replacement, b"replacement").unwrap();
    let mut checked = false;
    capture(fixture.request(), &fixture.local, |stage| {
        if matches!(stage, Stage::Locked) {
            for name in [SESSION_CATALOG_FILE_NAME, ROOM_CATALOG_FILE_NAME] {
                let path = fixture.source.join(name);
                assert!(OpenOptions::new().write(true).open(&path).is_err());
                assert!(fs::remove_file(&path).is_err());
                assert!(
                    replace_catalog_file(&replacement, &path, "checkpoint ownership test").is_err()
                );
            }
            checked = true;
        }
        Ok(())
    })
    .unwrap();
    assert!(checked);
    assert_eq!(fixture.bytes(), original);
    assert_eq!(fs::read(replacement).unwrap(), b"replacement");
}

#[test]
fn missing_or_owned_desktop_lock_and_owned_room_writer_refuse_before_destination() {
    for kind in ["missing", "desktop", "room"] {
        let fixture = Fixture::new();
        let (_, room_path) = fixture.room(false, b"");
        let held = match kind {
            "missing" => {
                fs::remove_file(fixture.source.join(DESKTOP_LOCK)).unwrap();
                None
            }
            name => {
                let path = if name == "desktop" {
                    DESKTOP_LOCK
                } else {
                    &room_path
                };
                let handle = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(fixture.source.join(path))
                    .unwrap();
                handle.try_lock().unwrap();
                Some(handle)
            }
        };
        let request = fixture.request();
        let target = request.attempt_root.clone();
        assert!(
            capture(request, &fixture.local, |_| Ok(())).is_err(),
            "{kind}"
        );
        assert!(!target.exists());
        drop(held);
    }
}

#[test]
fn copy_fault_leaves_source_unchanged_and_attempt_uncommitted() {
    let fixture = Fixture::new();
    let original = fixture.bytes();
    let request = fixture.request();
    let target = request.attempt_root.clone();
    let error = capture(request, &fixture.local, |stage| {
        if matches!(stage, Stage::CopyChunk) {
            bail!("injected disk copy failure");
        }
        Ok(())
    })
    .unwrap_err();
    assert!(error.to_string().contains("injected disk copy failure"));
    assert!(target.exists());
    assert!(!target.join("checkpoint-v1.json").exists());
    assert_eq!(fixture.bytes(), original);
    let mut repeat = fixture.request();
    repeat.attempt_id = Uuid::parse_str(target.file_name().unwrap().to_str().unwrap()).unwrap();
    repeat.attempt_root = target;
    assert!(capture(repeat, &fixture.local, |_| Ok(())).is_err());
}

#[test]
fn new_pending_or_orphan_entry_during_capture_prevents_final_manifest() {
    for orphan in [false, true] {
        let fixture = Fixture::new();
        fixture.room(false, b"");
        let request = fixture.request();
        let target = request.attempt_root.clone();
        let added = if orphan {
            format!("rooms/{}.jsonl", Uuid::new_v4())
        } else {
            format!(".{SESSION_CATALOG_FILE_NAME}.{}.tmp", Uuid::new_v4())
        };
        let error = capture(request, &fixture.local, |stage| {
            if matches!(stage, Stage::BeforeCommit) {
                fs::write(fixture.source.join(&added), b"concurrent writer evidence")?;
            }
            Ok(())
        })
        .unwrap_err();
        assert!(error.to_string().contains("inventory drifted"));
        assert!(!target.join("checkpoint-v1.json").exists());
        assert_eq!(
            fs::read(fixture.source.join(&added)).unwrap(),
            b"concurrent writer evidence"
        );
    }
}

#[test]
fn torn_tail_and_epoch_normalization_affect_only_working_copy_and_are_reported() {
    let fixture = Fixture::new();
    let (_, relative) = fixture.room(true, b"{\"record\":");
    let original = fixture.bytes();
    let request = fixture.request();
    let target = request.attempt_root.clone();
    let manifest = capture(request, &fixture.local, |_| Ok(())).unwrap();
    assert_eq!(fixture.bytes(), original);
    assert_eq!(
        fs::read(target.join("checkpoint").join(&relative)).unwrap(),
        original[&relative]
    );
    assert!(
        manifest
            .compatibility
            .changes
            .iter()
            .any(|e| e.path == relative)
    );
    assert!(
        manifest
            .compatibility
            .changes
            .iter()
            .any(|e| e.path == ROOM_CATALOG_FILE_NAME)
    );
    let torn = manifest
        .compatibility
        .changes
        .iter()
        .find(|e| e.path.contains(".torn-"))
        .unwrap();
    assert!(torn.before_sha256.is_none());
    assert_eq!(
        fs::read(target.join("runtime").join(&torn.path)).unwrap(),
        b"{\"record\":"
    );
    assert!(!manifest.compatibility.copy_replay_ok);
    assert!(
        manifest
            .compatibility
            .holds
            .iter()
            .any(|h| h == "working_copy_changed_review_required")
    );
}

#[test]
fn invalid_complete_history_is_successful_byte_capture_with_explicit_recovery_hold() {
    let fixture = Fixture::new();
    fixture.room(false, b"invalid complete record\n");
    let original = fixture.bytes();
    let request = fixture.request();
    let target = request.attempt_root.clone();
    let manifest = capture(request, &fixture.local, |_| Ok(())).unwrap();
    assert!(target.join("checkpoint-v1.json").is_file());
    assert!(!manifest.compatibility.copy_replay_ok);
    assert!(manifest.compatibility.rooms[0].recovery_error.is_some());
    assert_eq!(fixture.bytes(), original);
    assert!(manifest.compatibility.changes.is_empty());
}

#[test]
fn malformed_catalog_is_preserved_without_claiming_successful_replay() {
    let fixture = Fixture::new();
    fs::write(
        fixture.source.join(SESSION_CATALOG_FILE_NAME),
        b"unparseable catalog",
    )
    .unwrap();
    let original = fixture.bytes();
    let manifest = capture(fixture.request(), &fixture.local, |_| Ok(())).unwrap();
    assert!(manifest.compatibility.catalog_error.is_some());
    assert!(!manifest.compatibility.copy_replay_ok);
    assert_eq!(fixture.bytes(), original);
}

#[test]
fn in_memory_codex_and_duplicate_identity_recovery_keep_compatibility_held() {
    for driver in [DriverKind::Codex, DriverKind::Claude] {
        let fixture = Fixture::new();
        let path = fixture.source.join(SESSION_CATALOG_FILE_NAME);
        let mut catalog = read_session_catalog(&path).unwrap();
        let conversation = Uuid::new_v4().to_string();
        for number in 0..2 {
            catalog.sessions.push(PersistedSessionV1 { codex_transport: Default::default(),
                session_id: Uuid::new_v4(),
                label: format!("Recovery {number}"),
                driver,
                working_directory: catalog.workspace_preference.clone(),
                permission_profile: shared_types::PermissionProfile::Normal,
                launch_choices: None,
                last_launch: None,
                harness_session_id: Some(conversation.clone()),
                recovery: None,
                recovery_history: vec![],
                running_at_shutdown: false,
                last_pty_cols: None,
                last_pty_rows: None,
                permissions: Default::default(),
                created_by: None,
            });
        }
        fs::write(path, serde_json::to_vec(&catalog).unwrap()).unwrap();
        let original = fixture.bytes();
        let manifest = capture(fixture.request(), &fixture.local, |_| Ok(())).unwrap();
        assert!(!manifest.compatibility.copy_replay_ok);
        assert_eq!(manifest.compatibility.session_recovery_holds.len(), 2);
        assert_eq!(manifest.compatibility.in_memory_normalizations.len(), 2);
        assert!(
            manifest.compatibility.changes.is_empty(),
            "startup did not persist identity normalization"
        );
        assert_eq!(fixture.bytes(), original);
    }
}

#[test]
fn grandfathered_room_brief_members_are_reported_without_manufacturing_disk_changes() {
    let fixture = Fixture::new();
    fixture.room(false, b"");
    let path = fixture.source.join(ROOM_CATALOG_FILE_NAME);
    let mut catalog = read_room_catalog(&path).unwrap();
    catalog.rooms[0].briefed_member_ids = None;
    fs::write(path, serde_json::to_vec(&catalog).unwrap()).unwrap();
    let original = fixture.bytes();
    let manifest = capture(fixture.request(), &fixture.local, |_| Ok(())).unwrap();
    assert!(!manifest.compatibility.copy_replay_ok);
    assert_eq!(manifest.compatibility.in_memory_normalizations.len(), 1);
    assert!(manifest.compatibility.changes.is_empty());
    assert_eq!(fixture.bytes(), original);
}

#[test]
fn pending_orphan_torn_and_maintenance_records_survive_outside_working_runtime() {
    let fixture = Fixture::new();
    let (room, _) = fixture.room(false, b"");
    let pending = format!(".{ROOM_CATALOG_FILE_NAME}.{}.tmp", Uuid::new_v4());
    let orphan = format!("rooms/{}.jsonl", Uuid::new_v4());
    let torn = format!("rooms/{room}.torn-{}.bin", Uuid::new_v4());
    fs::create_dir(fixture.source.join("maintenance")).unwrap();
    let paths = [
        pending.as_str(),
        orphan.as_str(),
        torn.as_str(),
        "maintenance/incidents-v1.json",
        "maintenance/.incidents-v1.json.pending",
        "maintenance/.incidents-v1.json.retained",
        "maintenance/restart-preflight-v1.json",
        "maintenance/.restart-preflight-v1.json.pending",
        "maintenance/.restart-preflight-v1.json.retained",
        "maintenance/candidate-v1.json",
        "maintenance/.candidate-v1.json.pending",
        "maintenance/candidate.exe",
        "maintenance/candidate.partial",
    ];
    for path in paths {
        if let Some(prefix) = path.strip_suffix(".retained") {
            fs::hard_link(
                fixture.source.join(format!("{prefix}.pending")),
                fixture.source.join(path),
            )
            .unwrap();
        } else {
            fs::write(fixture.source.join(path), b"exact retained evidence").unwrap();
        }
    }
    let original = fixture.bytes();
    let request = fixture.request();
    let target = request.attempt_root.clone();
    let manifest = capture(request, &fixture.local, |_| Ok(())).unwrap();
    assert!(!manifest.compatibility.copy_replay_ok);
    for path in paths {
        assert_eq!(
            fs::read(target.join("evidence").join(path)).unwrap(),
            b"exact retained evidence"
        );
        assert!(!target.join("runtime").join(path).exists());
    }
    assert_eq!(fixture.bytes(), original);
}

#[test]
fn guest_registry_is_state_and_its_endpoint_and_temporary_files_are_evidence() {
    let fixture = Fixture::new();
    let (room, _) = fixture.room(false, b"");
    let guest = |room_id: Uuid| {
        serde_json::json!({"guest_id": Uuid::new_v4(), "room_id": room_id, "label": "Scout",
            "token_sha256": "a".repeat(64), "created_at": "2026-10-19T00:00:00Z", "last_seen": null,
            "floor": 0, "floor_epoch": Uuid::new_v4()})
    };
    let registry_path = fixture.source.join(room_guests::ROOM_GUESTS_FILE_NAME);
    let write_registry = |guests: Vec<serde_json::Value>| {
        fs::write(
            &registry_path,
            serde_json::to_vec(&serde_json::json!({"schema_version": 1, "guests": guests})).unwrap(),
        )
        .unwrap()
    };
    write_registry(vec![guest(room)]);
    fs::write(
        fixture.source.join(shared_types::GUEST_ENDPOINT_FILE_NAME),
        br#"{"endpoint":"\\\\.\\pipe\\left-behind","server_pid":1,"server_started":1}"#,
    )
    .unwrap();
    let original = fixture.bytes();
    let request = fixture.request();
    let target = request.attempt_root.clone();
    let manifest = capture(request, &fixture.local, |_| Ok(())).unwrap();
    let role = |manifest: &ClosedCheckpointManifest, path: &str| {
        manifest.entries.iter().find(|entry| entry.path == path).unwrap().role
    };
    assert_eq!(role(&manifest, room_guests::ROOM_GUESTS_FILE_NAME), CheckpointRole::State);
    assert_eq!(role(&manifest, shared_types::GUEST_ENDPOINT_FILE_NAME), CheckpointRole::Evidence);
    assert!(target.join("runtime").join(room_guests::ROOM_GUESTS_FILE_NAME).is_file());
    assert!(!target.join("runtime").join(shared_types::GUEST_ENDPOINT_FILE_NAME).exists());
    assert!(manifest.compatibility.copy_replay_ok, "{:?}", manifest.compatibility);
    assert_eq!(fixture.bytes(), original);

    // A deleted room's guest, an interrupted registry write and an unreadable
    // registry are captured and held for review.
    let lapsed = guest(Uuid::new_v4());
    let lapsed_id = lapsed["guest_id"].as_str().unwrap().to_string();
    write_registry(vec![guest(room), lapsed]);
    let pending = format!(".{}.{}.tmp", room_guests::ROOM_GUESTS_FILE_NAME, Uuid::new_v4());
    fs::write(fixture.source.join(&pending), b"partial").unwrap();
    let manifest = capture(fixture.request(), &fixture.local, |_| Ok(())).unwrap();
    let compatibility = &manifest.compatibility;
    assert!(!compatibility.copy_replay_ok);
    assert!(compatibility.in_memory_normalizations.contains(&format!("room_guest_lapsed:{lapsed_id}")), "{compatibility:?}");
    assert!(compatibility.holds.contains(&format!("pending_catalog:{pending}")), "{compatibility:?}");
    assert_eq!(role(&manifest, &pending), CheckpointRole::Evidence);
    fs::write(&registry_path, b"{not a registry").unwrap();
    let manifest = capture(fixture.request(), &fixture.local, |_| Ok(())).unwrap();
    assert!(manifest.compatibility.holds.contains(&"guest_registry_unreadable".to_string()));
}

#[test]
fn task_folder_choice_is_exact_state_without_opening_its_external_store() {
    let fixture = Fixture::new();
    let external = fixture._root.path().join("external-tasks");
    fs::create_dir(&external).unwrap();
    let task = external.join("T-1.json");
    fs::write(&task, b"external task bytes, not checkpoint input").unwrap();
    // A fixture sentinel, not an acquired TaskStore lock. Accidentally opening
    // a TaskStore would consume its delete-on-close lock file on Windows.
    let lock = external.join(tasks::LOCK_FILE_NAME);
    fs::write(&lock, b"external ownership sentinel").unwrap();
    let setting = task_sideband::TASK_FOLDER_SETTING_FILE_NAME;
    let mut bytes = serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version": 1, "folder": external,
    })).unwrap();
    bytes.extend_from_slice(b"\r\n  ");
    fs::write(fixture.source.join(setting), &bytes).unwrap();
    let original = fixture.bytes();
    let request = fixture.request();
    let target = request.attempt_root.clone();
    let manifest = capture(request, &fixture.local, |stage| {
        if matches!(stage, Stage::Locked) {
            assert!(OpenOptions::new().write(true).open(fixture.source.join(setting)).is_err());
        }
        Ok(())
    }).unwrap();
    assert_eq!(fixture.bytes(), original);
    for area in ["checkpoint", "runtime"] {
        assert_eq!(fs::read(target.join(area).join(setting)).unwrap(), bytes);
        assert!(!target.join(area).join("tasks").exists());
    }
    assert_eq!(manifest.entries.iter().find(|entry| entry.path == setting).unwrap().role,
        CheckpointRole::State);
    assert!(manifest.compatibility.copy_replay_ok, "{:?}", manifest.compatibility);
    assert!(manifest.compatibility.changes.is_empty());
    assert!(!manifest.compatibility.settings_preservation_verified);
    assert!(manifest.limitations.contains(&"external_task_contents_not_inspected_or_captured".into()));
    assert_eq!(fs::read(&task).unwrap(), b"external task bytes, not checkpoint input");
    assert_eq!(fs::read(&lock).unwrap(), b"external ownership sentinel");
    assert_eq!(fs::read_dir(&external).unwrap().count(), 2);
}

#[test]
fn task_folder_setting_and_temporary_evidence_obey_the_64kib_boundary() {
    let fixture = Fixture::new();
    let missing = fixture._root.path().join("never-opened-tasks");
    let setting = task_sideband::TASK_FOLDER_SETTING_FILE_NAME;
    let mut bytes = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1, "folder": missing,
    })).unwrap();
    bytes.resize(TASK_FOLDER_SETTING_MAX as usize, b' ');
    fs::write(fixture.source.join(setting), &bytes).unwrap();
    let request = fixture.request();
    let target = request.attempt_root.clone();
    let manifest = capture(request, &fixture.local, |_| Ok(())).unwrap();
    assert!(manifest.compatibility.copy_replay_ok, "{:?}", manifest.compatibility);
    assert_eq!(fs::read(target.join("runtime").join(setting)).unwrap(), bytes);
    assert!(!missing.exists(), "capture must not create the selected task directory");

    let pending = format!(".{setting}.{}.tmp", Uuid::new_v4());
    let partial = vec![b'?'; TASK_FOLDER_SETTING_MAX as usize];
    fs::write(fixture.source.join(&pending), &partial).unwrap();
    let request = fixture.request();
    let target = request.attempt_root.clone();
    let manifest = capture(request, &fixture.local, |_| Ok(())).unwrap();
    assert!(!manifest.compatibility.copy_replay_ok);
    assert!(manifest.compatibility.holds.contains(&format!("pending_catalog:{pending}")));
    assert_eq!(manifest.entries.iter().find(|entry| entry.path == pending).unwrap().role,
        CheckpointRole::Evidence);
    assert_eq!(fs::read(target.join("evidence").join(&pending)).unwrap(), partial);
    assert!(!target.join("runtime").join(&pending).exists());
    assert!(!target.join("checkpoint").join(&pending).exists());

    for name in [setting, pending.as_str()] {
        let oversized = Fixture::new();
        let bytes = vec![b' '; TASK_FOLDER_SETTING_MAX as usize + 1];
        fs::write(oversized.source.join(name), &bytes).unwrap();
        let request = oversized.request();
        let target = request.attempt_root.clone();
        assert!(capture(request, &oversized.local, |_| Ok(())).is_err(), "{name}");
        assert!(!target.exists(), "refuse oversize before destination writes");
        assert_eq!(fs::read(oversized.source.join(name)).unwrap(), bytes);
    }
}

#[test]
fn personal_repo_choice_is_exact_state_but_install_identity_and_temporaries_are_evidence() {
    let fixture = Fixture::new();
    let external = fixture._root.path().join("never-opened-personal-repo");
    let mut setting = serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version": 1, "folder": external,
    })).unwrap();
    setting.extend_from_slice(b"\r\n");
    setting.resize(PERSONAL_REPO_SETTING_MAX as usize, b' ');
    let mut identity = format!("{}\n", Uuid::new_v4()).into_bytes();
    identity.resize(INSTALL_ID_MAX as usize, b' ');
    let pending_setting = format!(".personal-repo.json.{}.tmp", Uuid::new_v4());
    let pending_identity = format!(".install-id.{}.tmp", Uuid::new_v4());
    let files = [
        ("personal-repo.json", CheckpointRole::State, setting),
        ("install-id", CheckpointRole::Evidence, identity),
        (pending_setting.as_str(), CheckpointRole::Evidence,
            vec![b'?'; PERSONAL_REPO_SETTING_MAX as usize]),
        (pending_identity.as_str(), CheckpointRole::Evidence,
            vec![b'?'; INSTALL_ID_MAX as usize]),
    ];
    for (name, _, bytes) in &files {
        fs::write(fixture.source.join(name), bytes).unwrap();
    }
    let original = fixture.bytes();
    let request = fixture.request();
    let target = request.attempt_root.clone();
    let manifest = capture(request, &fixture.local, |stage| {
        if matches!(stage, Stage::Locked) {
            for (name, _, _) in &files {
                assert!(OpenOptions::new().write(true).open(fixture.source.join(name)).is_err());
            }
        }
        Ok(())
    }).unwrap();
    assert_eq!(fixture.bytes(), original, "same-runtime identity and settings stay untouched");
    for (name, role, bytes) in &files {
        let entry = manifest.entries.iter().find(|entry| entry.path == *name).unwrap();
        let area = if *role == CheckpointRole::State { "checkpoint" } else { "evidence" };
        assert_eq!(entry.role, *role);
        assert_eq!(entry.bytes, bytes.len() as u64);
        assert_eq!(entry.sha256, Some(format!("{:x}", Sha256::digest(bytes))));
        assert_eq!(entry.captured_path, format!("{area}/{name}"));
        assert_eq!(fs::read(target.join(&entry.captured_path)).unwrap(), *bytes);
        if *role == CheckpointRole::State {
            assert_eq!(fs::read(target.join("runtime").join(name)).unwrap(), *bytes);
        } else {
            assert!(!target.join("runtime").join(name).exists());
            assert!(!target.join("checkpoint").join(name).exists());
        }
    }
    assert!(!external.exists(), "capture must not initialize the external register");
    assert!(manifest.compatibility.catalog_error.is_none());
    assert!(manifest.compatibility.changes.is_empty());
    assert!(!manifest.compatibility.settings_preservation_verified);
    assert!(!manifest.compatibility.copy_replay_ok, "temporary writes remain held evidence");
    for pending in [&pending_setting, &pending_identity] {
        assert!(manifest.compatibility.holds.contains(&format!("pending_catalog:{pending}")));
    }
}

#[test]
fn malformed_task_choices_are_preserved_and_held_without_defaulting() {
    let fixture = Fixture::new();
    let setting = task_sideband::TASK_FOLDER_SETTING_FILE_NAME;
    for bytes in [
        b"{not a setting".to_vec(),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 999, "folder": fixture._root.path().join("external"),
        })).unwrap(),
        br#"{"schema_version":1,"folder":"relative-tasks"}"#.to_vec(),
    ] {
        fs::write(fixture.source.join(setting), &bytes).unwrap();
        let original = fixture.bytes();
        let request = fixture.request();
        let target = request.attempt_root.clone();
        let manifest = capture(request, &fixture.local, |_| Ok(())).unwrap();
        assert!(target.join("checkpoint-v1.json").is_file());
        assert!(!manifest.compatibility.copy_replay_ok);
        assert!(manifest.compatibility.holds.iter()
            .any(|hold| hold.starts_with("task_folder_setting_unreadable:")), "{:?}", manifest.compatibility);
        assert!(manifest.compatibility.changes.is_empty());
        assert!(manifest.compatibility.in_memory_normalizations.is_empty());
        for area in ["checkpoint", "runtime"] {
            assert_eq!(fs::read(target.join(area).join(setting)).unwrap(), bytes);
            assert!(!target.join(area).join("tasks").exists());
        }
        assert_eq!(fixture.bytes(), original);
    }
}

#[test]
fn unknown_entry_excess_size_or_count_refuses_without_destination() {
    for defect in ["unknown", "size", "count", "personal-setting-size", "install-id-size",
        "personal-setting-temp-size", "install-id-temp-size", "personal-setting-invalid-temp",
        "install-id-invalid-temp"] {
        let fixture = Fixture::new();
        match defect {
            "unknown" => fs::write(fixture.source.join("unexplained.txt"), b"preserve").unwrap(),
            "size" => OpenOptions::new()
                .write(true)
                .open(fixture.source.join(SESSION_CATALOG_FILE_NAME))
                .unwrap()
                .set_len(CATALOG_MAX + 1)
                .unwrap(),
            "personal-setting-size" | "install-id-size" | "personal-setting-temp-size" | "install-id-temp-size" => {
                let (name, max) = if defect.starts_with("personal-setting") {
                    ("personal-repo.json", PERSONAL_REPO_SETTING_MAX)
                } else {
                    ("install-id", INSTALL_ID_MAX)
                };
                let path = if defect.contains("temp") {
                    format!(".{name}.{}.tmp", Uuid::new_v4())
                } else {
                    name.to_string()
                };
                fs::write(fixture.source.join(path), vec![b' '; max as usize + 1]).unwrap();
            }
            "personal-setting-invalid-temp" | "install-id-invalid-temp" => {
                let name = if defect.starts_with("personal-setting") { "personal-repo.json" } else { "install-id" };
                fs::write(fixture.source.join(format!(".{name}.not-a-uuid.tmp")), b"preserve").unwrap();
            }
            _ => {
                fs::create_dir(fixture.source.join("rooms")).unwrap();
                for _ in 0..MAX_FILES {
                    fs::write(
                        fixture.source.join(format!(
                            "rooms/{}.torn-{}.bin",
                            Uuid::new_v4(),
                            Uuid::new_v4()
                        )),
                        [],
                    )
                    .unwrap();
                }
            }
        }
        let request = fixture.request();
        let target = request.attempt_root.clone();
        assert!(
            capture(request, &fixture.local, |_| Ok(())).is_err(),
            "{defect}"
        );
        assert!(!target.exists());
    }
}

#[test]
fn protected_source_destination_and_overlap_refuse_before_writes() {
    let fixture = Fixture::new();
    for protected in ["io.prim1.runtime", "Programs/PRIM-1", "io.prim1.v2"] {
        let forbidden = fixture.local.join(protected);
        fs::create_dir_all(&forbidden).unwrap();
        let mut request = fixture.request();
        request.attempt_root = forbidden.join(request.attempt_id.to_string());
        let target = request.attempt_root.clone();
        assert!(capture(request, &fixture.local, |_| Ok(())).is_err());
        assert!(!target.exists());
        let mut request = fixture.request();
        request.source_runtime = forbidden;
        let target = request.attempt_root.clone();
        assert!(capture(request, &fixture.local, |_| Ok(())).is_err());
        assert!(!target.exists());
    }
    let mut request = fixture.request();
    request.attempt_root = fixture.source.join(request.attempt_id.to_string());
    let target = request.attempt_root.clone();
    assert!(capture(request, &fixture.local, |_| Ok(())).is_err());
    assert!(!target.exists());
}

#[test]
fn reparse_descendant_is_refused_without_following_it() {
    let fixture = Fixture::new();
    let outside = fixture.parent.join("untouched");
    fs::create_dir(&outside).unwrap();
    let link = fixture.source.join("rooms");
    crate::tests::create_windows_junction(&link, &outside);
    let request = fixture.request();
    let target = request.attempt_root.clone();
    let outcome = capture(request, &fixture.local, |_| Ok(()));
    // Unlink only the explicit fixture junction before TempDir cleanup.
    fs::remove_dir(&link).unwrap();
    assert!(outcome.is_err());
    assert!(!target.exists());
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn first_slice_aggregate_limits_count_artifacts_separately_without_large_files() {
    let entry = |role, bytes| CheckpointEntry {
        path: "metadata-only".into(),
        directory: false,
        role,
        bytes,
        sha256: None,
        captured_path: String::new(),
    };
    assert!(
        check_totals(&[
            entry(CheckpointRole::State, CAPTURE_MAX),
            entry(CheckpointRole::Artifact, ARTIFACT_MAX),
            entry(CheckpointRole::Artifact, ARTIFACT_MAX)
        ])
        .is_ok()
    );
    assert!(
        check_totals(&[
            entry(CheckpointRole::State, CAPTURE_MAX),
            entry(CheckpointRole::Evidence, 1)
        ])
        .is_err()
    );
    assert!(check_totals(&[entry(CheckpointRole::Evidence, EVIDENCE_MAX + 1)]).is_err());
}

#[test]
fn authoritative_local_folder_lookup_is_absolute_and_existing() {
    let local = known_local_data().unwrap();
    assert!(local.is_absolute() && local.is_dir());
}
