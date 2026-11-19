//! A108 register: temporary runtimes, workspaces and Git repositories only. No
//! harness starts; conversation changes are reported directly or written to
//! the catalog first, as the followers do. The Git parts are skipped when no
//! `git` is on PATH. The production call sites are covered in
//! `session_ledger_flow_tests.rs`.
use super::*;
use crate::session_ledger::{
    self as ledger, ConversationChange, INSTALL_ID_FILE_NAME, PERSONAL_REPO_SETTING_FILE_NAME, REGISTER_DISAGREES,
    REGISTER_DUPLICATE, RegisterConversation, read_register_file, register_path,
};
use shared_types::{CreateRoomRequest, CreateSessionRequest, PermissionProfile, RemoveRoomMemberRequest};
use std::ffi::OsStr;
use std::process::Command;

/// Restores the test default (register off) when the test ends.
struct EnvGuard;

impl EnvGuard {
    fn set(value: Option<&OsStr>) -> Self {
        ledger::set_personal_repo_env_for_tests(value);
        Self
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        ledger::set_personal_repo_env_for_tests(Some(OsStr::new("off")));
    }
}

fn git_available() -> bool {
    Command::new("git").arg("--version").output().is_ok_and(|output| output.status.success())
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git").arg("-C").arg(repo).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// An empty repository with a local identity and no hooks.
fn init_git(repo: &Path) {
    let hooks = repo.parent().unwrap().join("no-hooks");
    fs::create_dir_all(&hooks).unwrap();
    git(repo, &["init", "-q"]);
    for (key, value) in [
        ("user.name", "PRIM-1 test"),
        ("user.email", "test@example.invalid"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", hooks.to_str().unwrap()),
    ] {
        git(repo, &["config", key, value]);
    }
}

/// A repository with one commit, an unstaged edit and a staged file that
/// belong to someone else.
fn init_repo(repo: &Path) {
    init_git(repo);
    fs::write(repo.join("notes.txt"), "first\n").unwrap();
    git(repo, &["add", "notes.txt"]);
    git(repo, &["commit", "-q", "-m", "first"]);
    fs::write(repo.join("notes.txt"), "edited, not staged\n").unwrap();
    fs::write(repo.join("staged.txt"), "staged by someone else\n").unwrap();
    git(repo, &["add", "staged.txt"]);
}

/// A supervisor on `root/runtime` with workspace `root/work`; a second call
/// on the same root is a restart.
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

/// Closes and starts again on the same root.
fn restart(supervisor: SupervisorHandle, root: &Path) -> SupervisorHandle {
    supervisor.shutdown().unwrap();
    drop(supervisor);
    supervisor_at(root)
}

fn member(supervisor: &SupervisorHandle, label: &str, driver: DriverKind) -> SessionId {
    supervisor
        .create_session(CreateSessionRequest { codex_transport: Default::default(), permissions: None,
            label: Some(label.into()),
            driver,
            permission_profile: PermissionProfile::Normal,
            linux_working_directory: None,
            room_id: None,
            launch_choices: None,
        })
        .unwrap()
        .session_id
}

fn change(id: SessionId, conversation: &str, reason: &'static str, via: &'static str) -> ConversationChange {
    ConversationChange {
        session_id: id,
        harness: DriverKind::Claude,
        conversation_id: conversation.into(),
        reason,
        via,
        run_id: Some(Uuid::new_v4()),
        pid: None,
    }
}

/// The catalog moves first, as a follower writes it.
fn set_catalog_conversation(supervisor: &SupervisorHandle, id: SessionId, conversation: Option<&str>) {
    let mut slots = supervisor.inner.slots.lock();
    let mut catalog = supervisor.inner.catalog.lock();
    let mut candidate = catalog.clone();
    candidate.sessions.iter_mut().find(|row| row.session_id == id).unwrap().harness_session_id = conversation.map(str::to_owned);
    supervisor.persist_catalog_candidate(&candidate).unwrap();
    slots.get_by_id_mut(id).unwrap().harness_session_id = conversation.map(str::to_owned);
    *catalog = candidate;
}

/// Edits the closed runtime's catalog on disk, row by row.
fn edit_catalog(root: &Path, mut edit: impl FnMut(SessionId, &mut serde_json::Value)) {
    let path = root.join("runtime").join(SESSION_CATALOG_FILE_NAME);
    let mut catalog: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for row in catalog["sessions"].as_array_mut().unwrap() {
        let id: SessionId = serde_json::from_value(row["session_id"].clone()).unwrap();
        edit(id, row);
    }
    fs::write(&path, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();
}

/// Something other than this install's writer changes the file: one
/// revision past the watermark.
fn outside_edit(path: &Path, edit: impl FnOnce(&mut ledger::RegisterRecordV1)) {
    let mut record = read_register_file(path).unwrap().unwrap();
    edit(&mut record);
    record.revision += 1;
    let mut bytes = serde_json::to_vec_pretty(&record).unwrap();
    bytes.push(b'\n');
    fs::write(path, bytes).unwrap();
}

fn conversation(id: &str) -> Option<RegisterConversation> {
    Some(RegisterConversation {
        confirmed_at: "2026-10-19T00:00:00.000Z".into(),
        id: id.into(),
        since: "2026-10-19T00:00:00.000Z".into(),
        via: "pid_file".into(),
        extra: Default::default(),
    })
}

/// The writer's answer, whatever it wrote.
fn try_flush(supervisor: &SupervisorHandle) -> ledger::LedgerReport {
    supervisor.flush_session_ledger(false, true, Duration::from_secs(60)).expect("the register writer did not answer")
}

/// Every change written.
fn flush(supervisor: &SupervisorHandle) -> ledger::LedgerReport {
    let report = try_flush(supervisor);
    assert_eq!(report.unwritten_members, 0, "{report:?}");
    report
}

fn take_writer_lock(repo: &Path) -> File {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(repo.join("prim1").join(".writer.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    lock
}

fn record(supervisor: &SupervisorHandle, id: SessionId) -> ledger::RegisterRecordV1 {
    let repo = supervisor.personal_repo_folder().unwrap();
    read_register_file(&register_path(&repo, id)).unwrap().expect("register file")
}

fn registered(supervisor: &SupervisorHandle, id: SessionId) -> Option<String> {
    record(supervisor, id).conversation.map(|conversation| conversation.id)
}

fn recovery(supervisor: &SupervisorHandle, id: SessionId) -> Option<RecoveryReference> {
    supervisor.inner.slots.lock().get_by_id(id).unwrap().recovery.clone()
}

fn provenance(supervisor: &SupervisorHandle, id: SessionId) -> Option<String> {
    recovery(supervisor, id).map(|recovery| recovery.provenance)
}

fn install_id(root: &Path) -> String {
    fs::read_to_string(root.join("runtime").join(INSTALL_ID_FILE_NAME)).unwrap().trim().to_owned()
}

fn status_message(supervisor: &SupervisorHandle) -> String {
    supervisor.personal_repo_status().message.unwrap_or_default()
}

#[test]
fn resolution_order_is_environment_then_setting_then_workspace() {
    let root = tempfile::tempdir().unwrap();
    let runtime = root.path().join("runtime");
    let work = root.path().join("work");
    let scratch = root.path().join("scratch");
    for folder in [&runtime, &work, &scratch] {
        fs::create_dir_all(folder).unwrap();
    }
    let resolve = |env: Option<&str>| ledger::resolve_personal_repo(env.map(OsStr::new), &runtime, &work);
    let canonical = |path: &Path| child_process_path(&fs::canonicalize(path).unwrap());

    // Nothing chosen: the workspace.
    assert_eq!(resolve(None).folder, Some(canonical(&work)));
    // Blank is unset; `off` (any case) disables; a path overrides.
    assert_eq!(resolve(Some("  ")).folder, Some(canonical(&work)));
    let off = resolve(Some("OFF"));
    assert_eq!(off.folder, None);
    assert_eq!(resolve(Some(scratch.to_str().unwrap())).folder, Some(canonical(&scratch)));

    // A saved choice beats the workspace; the environment beats both.
    fs::write(
        runtime.join(PERSONAL_REPO_SETTING_FILE_NAME),
        serde_json::to_vec(&serde_json::json!({ "schema_version": 1, "folder": scratch })).unwrap(),
    )
    .unwrap();
    assert_eq!(resolve(None).folder, Some(canonical(&scratch)));
    assert_eq!(resolve(Some("off")).folder, None);

    // An override or a saved choice that cannot be used never falls through
    // to the workspace.
    assert_eq!(resolve(Some("relative\\folder")).folder, None);
    assert_eq!(resolve(Some(root.path().join("missing").to_str().unwrap())).folder, None);
    fs::write(runtime.join(PERSONAL_REPO_SETTING_FILE_NAME), b"{\"schema_version\":2,\"folder\":\"C:\\\\x\"}").unwrap();
    assert_eq!(resolve(None).folder, None);
    fs::remove_file(runtime.join(PERSONAL_REPO_SETTING_FILE_NAME)).unwrap();

    // No workspace: no register.
    assert_eq!(ledger::resolve_personal_repo(None, &runtime, &root.path().join("gone")).folder, None);
}

#[test]
fn install_id_is_created_once_and_never_replaced() {
    let root = tempfile::tempdir().unwrap();
    let first = ledger::load_or_create_install_id(root.path()).unwrap();
    assert_eq!(ledger::load_or_create_install_id(root.path()).unwrap(), first);
    assert_eq!(fs::read_to_string(root.path().join(INSTALL_ID_FILE_NAME)).unwrap(), format!("{first}\n"));
    assert!(Uuid::parse_str(&first).is_ok_and(|id| !id.is_nil()));

    fs::write(root.path().join(INSTALL_ID_FILE_NAME), "not an id").unwrap();
    assert!(ledger::load_or_create_install_id(root.path()).unwrap_err().contains("install id"));
    assert_eq!(fs::read_to_string(root.path().join(INSTALL_ID_FILE_NAME)).unwrap(), "not an id");
}

#[test]
fn environment_off_writes_nothing() {
    let _env = EnvGuard::set(Some(OsStr::new("off")));
    let root = tempfile::tempdir().unwrap();
    let supervisor = supervisor_at(root.path());
    let id = member(&supervisor, "Claude", DriverKind::Claude);
    supervisor.note_conversation_change(change(id, "11111111-1111-4111-8111-111111111111", "launch", "minted"));
    assert_eq!(supervisor.flush_session_ledger(false, true, Duration::from_secs(1)), None);
    let status = supervisor.personal_repo_status();
    assert_eq!((status.source, status.status, status.folder), (PersonalRepoSource::Environment, PersonalRepoHealth::Off, None));
    assert!(status.message.unwrap().contains(ledger::PERSONAL_REPO_ENV));
    assert!(!root.path().join("work").join("prim1").exists());
    // A runtime with the register off keeps no install id.
    assert!(!root.path().join("runtime").join(INSTALL_ID_FILE_NAME).exists());
    assert_eq!(supervisor.select_personal_repo(root.path().join("work").to_str().unwrap()).unwrap_err(), ledger_env_decides());
}

fn ledger_env_decides() -> String {
    "PRIM1_PERSONAL_REPO is set for this PRIM-1, so it decides the personal repository; remove it and restart to choose one here.".into()
}

/// The checkpoint the design names for self-upgrade and copies: a runtime
/// that ran once with the register off holds nothing it does not know.
#[cfg(windows)]
#[test]
fn a_runtime_with_the_register_off_can_be_checkpointed() {
    let root = tempfile::tempdir().unwrap();
    let supervisor = supervisor_at(root.path());
    member(&supervisor, "Claude", DriverKind::Claude);
    supervisor.shutdown().unwrap();
    drop(supervisor);
    let runtime = root.path().join("runtime");
    assert!(!runtime.join(INSTALL_ID_FILE_NAME).exists());
    fs::write(runtime.join("desktop-instance.lock"), []).unwrap();
    let attempts = root.path().join("attempts");
    fs::create_dir_all(&attempts).unwrap();
    let attempt_id = Uuid::new_v4();
    let manifest = crate::prepare_closed_checkpoint(crate::ClosedCheckpointRequest {
        source_runtime: runtime,
        attempt_root: attempts.join(attempt_id.to_string()),
        attempt_id,
        provenance: crate::ClosedCheckpointProvenance {
            closure_receipt_sha256: "1".repeat(64),
            old_executable_sha256: "2".repeat(64),
            candidate_executable_sha256: "3".repeat(64),
            reviewed_source_revision: "4".repeat(40),
            build_receipt_sha256: "5".repeat(64),
        },
    });
    assert!(manifest.is_ok(), "{manifest:?}");
}

#[test]
fn register_records_conversations_append_only_and_sorted() {
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let supervisor = supervisor_at(root.path());
    let status = supervisor.personal_repo_status();
    assert_eq!((status.source, status.status), (PersonalRepoSource::Default, PersonalRepoHealth::On), "{status:?}");
    let repo = supervisor.personal_repo_folder().unwrap();
    assert_eq!(repo, child_process_path(&fs::canonicalize(root.path().join("work")).unwrap()));

    let id = member(&supervisor, "Claude PRIM-1", DriverKind::Claude);
    let terminal = member(&supervisor, "Shell", DriverKind::GenericTerminal);
    flush(&supervisor);
    let created = record(&supervisor, id);
    assert_eq!((created.conversation.as_ref(), created.history.len(), created.revision), (None, 0, 1));
    assert_eq!((created.label.as_str(), created.harness, created.prim_session_id), ("Claude PRIM-1", DriverKind::Claude, id));
    assert_eq!((created.holder.install_id.clone(), created.holder.state.as_str()), (install_id(root.path()), "stopped"));
    // A terminal has no conversation and no register file.
    assert!(!register_path(&repo, terminal).exists());

    // Layout files, exactly.
    assert_eq!(fs::read_to_string(repo.join("prim1").join(".gitignore")).unwrap(), "*.tmp\n.*.tmp\n*.lock\n");
    assert_eq!(fs::read_to_string(repo.join("prim1").join(".gitattributes")).unwrap(), "* text eol=lf\n");

    let first = "aaaaaaaa-0000-4000-8000-000000000001";
    let second = "bbbbbbbb-0000-4000-8000-000000000002";
    let launch = change(id, first, "launch", "minted");
    supervisor.note_conversation_change(launch.clone());
    flush(&supervisor);
    let launched = record(&supervisor, id);
    let current = launched.conversation.clone().unwrap();
    assert_eq!((current.id.as_str(), current.via.as_str()), (first, "minted"));
    assert_eq!(launched.history.len(), 1);
    assert_eq!((launched.history[0].reason.as_str(), launched.history[0].run_id, launched.history[0].to.as_ref()), ("launch", launch.run_id, None));
    assert_eq!(launched.revision, 2);
    let hint = launched.transcript_hint.clone().unwrap();
    assert_eq!(hint.home, "~/.claude");
    assert!(hint.relative.starts_with("projects/") && hint.relative.ends_with(&format!("/{first}.jsonl")), "{hint:?}");

    // The same conversation again confirms it; no history entry.
    supervisor.note_conversation_change(change(id, &first.to_uppercase(), "resume", "pid_file"));
    flush(&supervisor);
    let confirmed = record(&supervisor, id);
    assert_eq!(confirmed.history.len(), 1);
    assert_eq!(confirmed.conversation.as_ref().unwrap().since, current.since);

    // /clear: the open entry is closed once and a new one is appended.
    supervisor.note_conversation_change(change(id, second, "clear", "pid_file"));
    flush(&supervisor);
    let cleared = record(&supervisor, id);
    assert_eq!(cleared.conversation.as_ref().unwrap().id, second);
    assert_eq!(cleared.history.len(), 2);
    assert_eq!(cleared.history[0].id, first);
    assert_eq!(cleared.history[0].to.as_deref(), Some(cleared.history[1].from.as_str()));
    assert_eq!((cleared.history[1].reason.as_str(), cleared.history[1].via.as_str()), ("clear", "pid_file"));
    assert_eq!(cleared.history[0].from, launched.history[0].from);

    // Titles are display only; a rename of the member changes `label`.
    supervisor.note_conversation_title(id, "Register work".into(), "claude_name");
    supervisor.rename_session(id, "Claude register").unwrap();
    flush(&supervisor);
    let renamed = record(&supervisor, id);
    assert_eq!((renamed.label.as_str(), renamed.titles.current.as_deref(), renamed.titles.source.as_deref()), ("Claude register", Some("Register work"), Some("claude_name")));
    assert_eq!(renamed.conversation, cleared.conversation);

    // Room membership.
    let room = supervisor
        .create_room(CreateRoomRequest { label: Some("Register room".into()), member_ids: vec![id], brief_on_join: false, brief_template: None })
        .unwrap();
    flush(&supervisor);
    let roomed = record(&supervisor, id);
    assert_eq!(roomed.room.as_ref().map(|room| (room.id, room.label.as_str())), Some((room.room_id, "Register room")));

    // On disk: sorted keys, LF only, no temporary files, no transcript text.
    let text = fs::read_to_string(register_path(&repo, id)).unwrap();
    assert!(!text.contains('\r') && text.ends_with("}\n"));
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    let keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["conversation", "harness", "history", "holder", "label", "prim_session_id", "revision", "room", "schema", "titles", "transcript_hint", "updated_at", "working_dir"]
    );
    let top_level_order: Vec<usize> = keys.iter().map(|key| text.find(&format!("\"{key}\"")).unwrap()).collect();
    assert!(top_level_order.windows(2).all(|pair| pair[0] < pair[1]), "keys are written in sorted order");
    let holder: Vec<&str> = value["holder"].as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(holder, ["install_id", "machine", "pid", "proc_start", "run_id", "since", "state"]);
    assert!(fs::read_dir(repo.join("prim1").join("sessions")).unwrap().all(|entry| {
        entry.unwrap().file_name().to_string_lossy().ends_with(".json")
    }));

    // Leaving the room clears it; a deleted member is retired, never deleted.
    supervisor.remove_room_member(RemoveRoomMemberRequest { room_id: room.room_id, session_id: id }).unwrap();
    flush(&supervisor);
    assert_eq!(record(&supervisor, id).room, None);
    supervisor.delete_session(id).unwrap();
    flush(&supervisor);
    let retired = record(&supervisor, id);
    assert_eq!((retired.holder.state.as_str(), retired.history.len()), ("retired", 2));
}

/// Codex review 478dd9d: a branch checkout can restore this install's own,
/// older register file. A later rename-only update must start from the newer
/// record this install wrote, never write the older conversation and history back.
#[test]
fn a_restored_older_own_register_file_is_not_written_back_by_a_rename() {
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let supervisor = supervisor_at(root.path());
    let repo = supervisor.personal_repo_folder().unwrap();
    let id = member(&supervisor, "Claude", DriverKind::Claude);
    let (a, b, c) = (
        "aaaaaaaa-0000-4000-8000-00000000000a",
        "bbbbbbbb-0000-4000-8000-00000000000b",
        "cccccccc-0000-4000-8000-00000000000c",
    );
    supervisor.note_conversation_change(change(id, a, "launch", "minted"));
    flush(&supervisor);
    let path = register_path(&repo, id);
    let older = fs::read(&path).unwrap();
    supervisor.note_conversation_change(change(id, b, "clear", "pid_file"));
    flush(&supervisor);
    supervisor.note_conversation_change(change(id, c, "clear", "pid_file"));
    flush(&supervisor);
    let newest = record(&supervisor, id);
    assert_eq!((newest.conversation.as_ref().unwrap().id.as_str(), newest.history.len()), (c, 3));

    // A checkout restores the older file; then only the label changes.
    fs::write(&path, &older).unwrap();
    supervisor.rename_session(id, "Claude renamed").unwrap();
    flush(&supervisor);
    let after = record(&supervisor, id);
    assert_eq!(after.label, "Claude renamed");
    assert_eq!(after.conversation, newest.conversation, "the newer current conversation stands");
    assert_eq!(after.history, newest.history, "the history is not rolled back");
    assert!(after.revision > newest.revision, "the revision never goes backwards");
}

/// A field a later build adds, at any level, is kept as it is; every
/// object's keys stay sorted.
#[test]
fn fields_this_build_does_not_know_are_kept() {
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let supervisor = supervisor_at(root.path());
    let id = member(&supervisor, "Later fields", DriverKind::Claude);
    supervisor.note_conversation_title(id, "Old title".into(), "claude_name");
    flush(&supervisor);
    let path = register_path(&supervisor.personal_repo_folder().unwrap(), id);
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["titles"]["formerNames"] = serde_json::json!(["Before"]);
    value["zz_later"] = serde_json::json!({ "b": 1, "a": 2 });
    value["holder"]["host_kind"] = serde_json::json!("desktop");
    fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    assert!(read_register_file(&path).unwrap().is_some());

    supervisor.rename_session(id, "Later fields renamed").unwrap();
    supervisor.note_conversation_title(id, "New title".into(), "claude_name");
    flush(&supervisor);
    let text = fs::read_to_string(&path).unwrap();
    let written: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(written["label"], "Later fields renamed");
    assert_eq!(written["titles"]["current"], "New title");
    assert_eq!(written["titles"]["formerNames"], serde_json::json!(["Before"]));
    assert_eq!(written["zz_later"], serde_json::json!({ "a": 2, "b": 1 }));
    assert_eq!(written["holder"]["host_kind"], "desktop");
    assert!(text.find("\"working_dir\"").unwrap() < text.find("\"zz_later\"").unwrap());
    assert!(text.find("\"a\": 2").unwrap() < text.find("\"b\": 1").unwrap());
    assert!(text.find("\"current\"").unwrap() < text.find("\"formerNames\"").unwrap());
}

#[test]
fn commit_holds_only_register_files_and_waits_out_index_lock() {
    if !git_available() {
        eprintln!("git is not available; skipping the commit test");
        return;
    }
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    fs::create_dir_all(&work).unwrap();
    init_repo(&work);
    let supervisor = supervisor_at(root.path());
    let id = member(&supervisor, "Codex reviewer", DriverKind::Codex);
    assert_eq!(flush(&supervisor).uncommitted_files, 0);

    let head = git(&work, &["log", "-1", "--format=%s"]);
    assert_eq!(head.trim(), "ADD: PRIM-1 session register - Codex reviewer");
    let mut files: Vec<String> = git(&work, &["show", "--name-only", "--format=", "HEAD"]).lines().map(str::to_owned).collect();
    files.sort();
    assert_eq!(files, ["prim1/.gitattributes".to_owned(), "prim1/.gitignore".to_owned(), format!("prim1/sessions/{id}.json")]);
    // Someone else's staged and unstaged work is untouched.
    let status = git(&work, &["status", "--porcelain"]);
    assert!(status.contains("A  staged.txt") && status.contains(" M notes.txt"), "{status}");
    assert!(git(&work, &["log", "--format=%B", "-1"]).lines().all(|line| !line.contains("Co-Authored-By")));

    // A held index.lock: the file is written, the commit waits, and the
    // flush says so.
    let lock = work.join(".git").join("index.lock");
    fs::write(&lock, b"").unwrap();
    supervisor.rename_session(id, "Codex reviewer two").unwrap();
    assert_eq!(flush(&supervisor).uncommitted_files, 1);
    assert_eq!(record(&supervisor, id).label, "Codex reviewer two");
    assert_eq!(git(&work, &["log", "-1", "--format=%s"]).trim(), "ADD: PRIM-1 session register - Codex reviewer");
    fs::remove_file(&lock).unwrap();
    assert_eq!(flush(&supervisor).uncommitted_files, 0);
    assert_eq!(git(&work, &["log", "-1", "--format=%s"]).trim(), "ADD: PRIM-1 session register - Codex reviewer two");
    assert_eq!(git(&work, &["show", "--name-only", "--format=", "HEAD"]).trim(), format!("prim1/sessions/{id}.json"));
    assert!(git(&work, &["status", "--porcelain", "--", "prim1"]).trim().is_empty());
    // Nothing was pushed: there is no remote at all.
    assert!(git(&work, &["remote"]).trim().is_empty());
}

/// A new register file is never staged: a commit someone else makes before
/// the register's own does not take it.
#[test]
fn another_commit_meanwhile_never_takes_a_register_file() {
    if !git_available() {
        eprintln!("git is not available; skipping the commit test");
        return;
    }
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    fs::create_dir_all(&work).unwrap();
    init_repo(&work);
    let supervisor = supervisor_at(root.path());
    flush(&supervisor);
    // The commit waits behind a held index.lock after the files are written.
    let lock = work.join(".git").join("index.lock");
    fs::write(&lock, b"").unwrap();
    let id = member(&supervisor, "Untouched", DriverKind::Claude);
    flush(&supervisor);
    fs::remove_file(&lock).unwrap();
    // Someone commits what they staged.
    git(&work, &["commit", "-q", "-m", "someone else"]);
    assert_eq!(git(&work, &["show", "--name-only", "--format=", "HEAD"]).trim(), "staged.txt");
    flush(&supervisor);
    assert_eq!(git(&work, &["log", "-1", "--format=%s"]).trim(), "ADD: PRIM-1 session register - Untouched");
    assert!(git(&work, &["show", "--name-only", "--format=", "HEAD"]).contains(&format!("prim1/sessions/{id}.json")));
    assert!(git(&work, &["diff", "--cached", "--name-only"]).trim().is_empty());
}

/// Never onto a detached HEAD or into a rebase: the commit would be lost
/// with it. The files wait, uncommitted, until the repository is back on a
/// branch.
#[test]
fn commit_waits_out_a_rebase_and_a_detached_head() {
    if !git_available() {
        eprintln!("git is not available; skipping the commit test");
        return;
    }
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    fs::create_dir_all(&work).unwrap();
    init_git(&work);
    fs::write(work.join("notes.txt"), "base\n").unwrap();
    git(&work, &["add", "notes.txt"]);
    git(&work, &["commit", "-q", "-m", "base"]);
    let branch = git(&work, &["symbolic-ref", "--short", "HEAD"]).trim().to_owned();
    git(&work, &["checkout", "-q", "-b", "side"]);
    fs::write(work.join("notes.txt"), "side\n").unwrap();
    git(&work, &["commit", "-q", "-am", "side"]);
    git(&work, &["checkout", "-q", &branch]);
    fs::write(work.join("notes.txt"), "main\n").unwrap();
    git(&work, &["commit", "-q", "-am", "main"]);
    let main_head = git(&work, &["rev-parse", "HEAD"]);
    let rebase = Command::new("git").arg("-C").arg(&work).args(["rebase", "side"]).output().unwrap();
    assert!(!rebase.status.success(), "the rebase stops on its conflict");
    let stopped_head = git(&work, &["rev-parse", "HEAD"]);

    let supervisor = supervisor_at(root.path());
    let id = member(&supervisor, "Rebase", DriverKind::Claude);
    flush(&supervisor);
    assert!(register_path(&fs::canonicalize(&work).unwrap(), id).is_file());
    assert_eq!(git(&work, &["rev-parse", "HEAD"]), stopped_head);
    assert!(status_message(&supervisor).contains("in the middle of a rebase"), "{}", status_message(&supervisor));
    assert!(git(&work, &["status", "--porcelain", "--", "prim1"]).lines().all(|line| line.starts_with("??")));

    git(&work, &["rebase", "--abort"]);
    flush(&supervisor);
    assert_eq!(git(&work, &["symbolic-ref", "--short", "HEAD"]).trim(), branch);
    assert_eq!(git(&work, &["log", "-1", "--format=%s"]).trim(), "ADD: PRIM-1 session register - Rebase");
    assert_eq!(git(&work, &["rev-parse", "HEAD~1"]), main_head);
    assert!(git(&work, &["status", "--porcelain", "--", "prim1"]).trim().is_empty());
    assert!(!status_message(&supervisor).contains("rebase"));

    git(&work, &["checkout", "-q", "--detach"]);
    let detached = git(&work, &["rev-parse", "HEAD"]);
    supervisor.rename_session(id, "Rebase two").unwrap();
    flush(&supervisor);
    assert_eq!(git(&work, &["rev-parse", "HEAD"]), detached);
    assert!(status_message(&supervisor).contains("detached HEAD"), "{}", status_message(&supervisor));
    git(&work, &["checkout", "-q", &branch]);
    flush(&supervisor);
    assert_eq!(git(&work, &["log", "-1", "--format=%s"]).trim(), "ADD: PRIM-1 session register - Rebase two");
    assert!(git(&work, &["status", "--porcelain", "--", "prim1"]).trim().is_empty());
}

#[test]
fn the_next_start_commits_what_a_close_left_uncommitted() {
    if !git_available() {
        eprintln!("git is not available; skipping the commit test");
        return;
    }
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    fs::create_dir_all(&work).unwrap();
    init_repo(&work);
    let (id, gone) = {
        let supervisor = supervisor_at(root.path());
        let id = member(&supervisor, "Owed", DriverKind::Grok);
        let gone = member(&supervisor, "Gone", DriverKind::Claude);
        flush(&supervisor);
        // Deleted and closed within the commit's quiet time: written
        // `retired`, never committed, and no longer in the catalog.
        supervisor.delete_session(gone).unwrap();
        supervisor.shutdown().unwrap();
        (id, gone)
    };
    let commits = || git(&work, &["rev-list", "--count", "HEAD"]).trim().parse::<u32>().unwrap();
    let before = commits();
    // What a close leaves: this install's file, written, not committed.
    let repo = fs::canonicalize(&work).unwrap();
    let path = register_path(&repo, id);
    let mut owed = read_register_file(&path).unwrap().unwrap();
    owed.titles.current = Some("written at close".into());
    let mut bytes = serde_json::to_vec_pretty(&owed).unwrap();
    bytes.push(b'\n');
    fs::write(&path, bytes).unwrap();
    assert_eq!(read_register_file(&register_path(&repo, gone)).unwrap().unwrap().holder.state, "retired");
    let owed_status = git(&work, &["status", "--porcelain", "--", "prim1"]);
    assert!(owed_status.contains(&id.to_string()) && owed_status.contains(&gone.to_string()), "{owed_status}");

    let supervisor = supervisor_at(root.path());
    flush(&supervisor);
    assert_eq!(commits(), before + 1);
    let subject = git(&work, &["log", "-1", "--format=%s"]);
    assert!(subject.starts_with("ADD: PRIM-1 session register - ") && subject.contains("Owed") && subject.contains("Gone"), "{subject}");
    assert!(git(&work, &["status", "--porcelain", "--", "prim1"]).trim().is_empty());
    assert_eq!(record(&supervisor, id).titles.current.as_deref(), Some("written at close"));
}

#[test]
fn a_folder_outside_git_keeps_files_without_commits() {
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let supervisor = supervisor_at(root.path());
    let id = member(&supervisor, "Grok", DriverKind::Grok);
    supervisor.note_conversation_change(ConversationChange { harness: DriverKind::Grok, ..change(id, "cccccccc-0000-4000-8000-000000000003", "launch", "minted") });
    flush(&supervisor);
    let written = record(&supervisor, id);
    assert_eq!(written.conversation.as_ref().unwrap().id, "cccccccc-0000-4000-8000-000000000003");
    let hint = written.transcript_hint.unwrap();
    assert_eq!(hint.home, "~/.grok");
    assert!(hint.relative.starts_with("sessions/") && hint.relative.ends_with("/cccccccc-0000-4000-8000-000000000003"));
    // A change that names another harness is not this member's conversation.
    supervisor.note_conversation_change(change(id, "dddddddd-0000-4000-8000-000000000004", "clear", "pid_file"));
    flush(&supervisor);
    assert_eq!(record(&supervisor, id).history.len(), 1);
    if git_available() {
        assert!(status_message(&supervisor).contains("not in a Git work tree"), "{}", status_message(&supervisor));
    }
}

/// Only a file changed by something other than this install's writer can
/// hold a member. One that lags behind the catalog (the catalog is written
/// first) follows it.
#[test]
fn startup_holds_only_a_register_changed_outside_this_install() {
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let (held, lagging, agreeing, seeded) = {
        let supervisor = supervisor_at(root.path());
        let ids = (
            member(&supervisor, "Held", DriverKind::Claude),
            member(&supervisor, "Lagging", DriverKind::Claude),
            member(&supervisor, "Agreeing", DriverKind::Claude),
            member(&supervisor, "Seeded", DriverKind::Claude),
        );
        for (id, launched) in [
            (ids.0, "eeeeeeee-0000-4000-8000-000000000005"),
            (ids.1, "abababab-0000-4000-8000-00000000000a"),
            (ids.2, "ffffffff-0000-4000-8000-000000000006"),
        ] {
            supervisor.note_conversation_change(change(id, launched, "launch", "minted"));
            set_catalog_conversation(&supervisor, id, Some(launched));
        }
        flush(&supervisor);
        supervisor.shutdown().unwrap();
        ids
    };
    let repo = fs::canonicalize(root.path().join("work")).unwrap();
    // Something else rewrites `held`'s file.
    outside_edit(&register_path(&repo, held), |record| record.conversation = conversation("12121212-0000-4000-8000-000000000012"));
    // The catalog moved on alone for `lagging` (a close between the catalog
    // write and the register's); `seeded` names one the register never saw.
    edit_catalog(root.path(), |id, row| {
        if id == lagging {
            row["harness_session_id"] = serde_json::json!("cdcdcdcd-0000-4000-8000-00000000000c");
        } else if id == agreeing {
            row["harness_session_id"] = serde_json::json!("FFFFFFFF-0000-4000-8000-000000000006");
        } else if id == seeded {
            row["harness_session_id"] = serde_json::json!("77777777-0000-4000-8000-000000000007");
        }
    });

    let supervisor = supervisor_at(root.path());
    let hold = recovery(&supervisor, held).expect("held");
    assert_eq!(hold.provenance, REGISTER_DISAGREES);
    assert_eq!(hold.original_id.as_deref(), Some("eeeeeeee-0000-4000-8000-000000000005"));
    assert!(hold.reason.contains("12121212-0000-4000-8000-000000000012") && hold.reason.contains("eeeeeeee-0000-4000-8000-000000000005"), "{}", hold.reason);
    for id in [lagging, agreeing, seeded] {
        assert_eq!(recovery(&supervisor, id), None);
    }
    let snapshot = supervisor.snapshot();
    let shown = snapshot.sessions.iter().find(|session| session.session_id == held).unwrap();
    assert_eq!(shown.recovery.as_ref().map(|recovery| recovery.provenance.as_str()), Some(REGISTER_DISAGREES));
    assert!(!shown.resume_available);

    // Launch stays held; Start fresh mints a new conversation as before.
    let definition = supervisor.inner.slots.lock().get_by_id(held).unwrap().definition.clone();
    let refused = supervisor.resolve_harness_launch_session(&definition, false).unwrap_err();
    assert!(refused.to_string().contains("Start fresh"), "{refused}");
    assert!(matches!(supervisor.resolve_harness_launch_session(&definition, true).unwrap(), HarnessLaunchSession::New { .. }));

    // The register keeps its own value for the held member; the lagging and
    // the seeded ones take the catalog's, marked as observed.
    flush(&supervisor);
    assert_eq!(registered(&supervisor, held).as_deref(), Some("12121212-0000-4000-8000-000000000012"));
    for (id, expected, entries) in [
        (lagging, "cdcdcdcd-0000-4000-8000-00000000000c", 2),
        (seeded, "77777777-0000-4000-8000-000000000007", 1),
    ] {
        let followed = record(&supervisor, id);
        assert_eq!(followed.conversation.as_ref().map(|conversation| conversation.id.as_str()), Some(expected));
        assert_eq!(followed.history.len(), entries);
        let last = followed.history.last().unwrap();
        assert_eq!((last.reason.as_str(), last.via.as_str()), ("observed", "catalog"));
    }
    assert_eq!(record(&supervisor, agreeing).history.len(), 1);

    // The next start holds only `held` again.
    let supervisor = restart(supervisor, root.path());
    assert_eq!(provenance(&supervisor, held).as_deref(), Some(REGISTER_DISAGREES));
    for id in [lagging, agreeing, seeded] {
        assert_eq!(recovery(&supervisor, id), None);
    }
}

/// A file that cannot be read or replaced keeps its changes; they are
/// written once it can be, and a close in between never leaves a hold.
#[cfg(windows)]
#[test]
fn a_failed_write_keeps_its_changes_and_the_next_start_never_holds() {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_SHARE_READ: u32 = 0x1;
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let (first, second, third) = (
        "a1a1a1a1-0000-4000-8000-000000000001",
        "b2b2b2b2-0000-4000-8000-000000000002",
        "c3c3c3c3-0000-4000-8000-000000000003",
    );
    let supervisor = supervisor_at(root.path());
    let id = member(&supervisor, "Writer", DriverKind::Claude);
    supervisor.note_conversation_change(change(id, first, "launch", "minted"));
    set_catalog_conversation(&supervisor, id, Some(first));
    flush(&supervisor);
    let path = register_path(&supervisor.personal_repo_folder().unwrap(), id);
    let hold_file = |share| OpenOptions::new().read(true).share_mode(share).open(&path).unwrap();

    // Unreadable: the catalog moves on and the change waits; the flush says so.
    let held = hold_file(0);
    set_catalog_conversation(&supervisor, id, Some(second));
    assert_eq!(try_flush(&supervisor).unwritten_members, 1);
    assert!(status_message(&supervisor).contains("cannot be read"), "{}", status_message(&supervisor));
    drop(held);
    assert_eq!(read_register_file(&path).unwrap().unwrap().conversation.unwrap().id, first);
    flush(&supervisor);
    let written = record(&supervisor, id);
    assert_eq!(written.conversation.as_ref().unwrap().id, second);
    assert_eq!((written.history[1].reason.as_str(), written.history[1].via.as_str()), ("observed", "catalog"));
    assert!(!status_message(&supervisor).contains("cannot be read"));

    // Readable but not replaceable, and PRIM-1 closes: the register lags.
    let held = hold_file(FILE_SHARE_READ);
    set_catalog_conversation(&supervisor, id, Some(third));
    assert_eq!(try_flush(&supervisor).unwritten_members, 1);
    assert!(status_message(&supervisor).contains("could not be written"), "{}", status_message(&supervisor));
    supervisor.shutdown().unwrap();
    drop(supervisor);
    drop(held);
    assert_eq!(read_register_file(&path).unwrap().unwrap().conversation.unwrap().id, second);

    let supervisor = supervisor_at(root.path());
    assert_eq!(recovery(&supervisor, id), None);
    flush(&supervisor);
    let caught_up = record(&supervisor, id);
    assert_eq!(caught_up.conversation.as_ref().unwrap().id, third);
    assert_eq!(caught_up.history.len(), 3);
    let supervisor = restart(supervisor, root.path());
    assert_eq!(recovery(&supervisor, id), None);
}

/// With the register off the catalog moves alone; turned back on, the
/// register follows it and nothing is held.
#[test]
fn register_off_then_on_follows_the_catalog_without_a_hold() {
    let root = tempfile::tempdir().unwrap();
    let (first, second) = ("d4d4d4d4-0000-4000-8000-000000000004", "e5e5e5e5-0000-4000-8000-000000000005");
    let id = {
        let _env = EnvGuard::set(None);
        let supervisor = supervisor_at(root.path());
        let id = member(&supervisor, "Toggled", DriverKind::Grok);
        supervisor.note_conversation_change(ConversationChange { harness: DriverKind::Grok, ..change(id, first, "launch", "minted") });
        set_catalog_conversation(&supervisor, id, Some(first));
        flush(&supervisor);
        supervisor.shutdown().unwrap();
        id
    };
    {
        let _env = EnvGuard::set(Some(OsStr::new("off")));
        let supervisor = supervisor_at(root.path());
        set_catalog_conversation(&supervisor, id, Some(second));
        supervisor.shutdown().unwrap();
    }
    let _env = EnvGuard::set(None);
    let supervisor = supervisor_at(root.path());
    assert_eq!(recovery(&supervisor, id), None);
    flush(&supervisor);
    assert_eq!(registered(&supervisor, id).as_deref(), Some(second));
    let supervisor = restart(supervisor, root.path());
    assert_eq!(recovery(&supervisor, id), None);
}

/// Back in a repository written earlier, its files lag behind the catalog;
/// they follow it, then and at the next start.
#[test]
fn switching_back_to_an_earlier_repository_follows_the_catalog() {
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let other = root.path().join("other");
    fs::create_dir_all(&other).unwrap();
    let (first, second) = ("f6f6f6f6-0000-4000-8000-000000000006", "07070707-0000-4000-8000-000000000007");
    let supervisor = supervisor_at(root.path());
    let id = member(&supervisor, "Switched", DriverKind::Claude);
    supervisor.note_conversation_change(change(id, first, "launch", "minted"));
    set_catalog_conversation(&supervisor, id, Some(first));
    flush(&supervisor);
    let a = supervisor.personal_repo_folder().unwrap();

    supervisor.select_personal_repo(other.to_str().unwrap()).unwrap();
    set_catalog_conversation(&supervisor, id, Some(second));
    flush(&supervisor);
    assert_eq!(registered(&supervisor, id).as_deref(), Some(second));
    assert_eq!(read_register_file(&register_path(&a, id)).unwrap().unwrap().conversation.unwrap().id, first);

    supervisor.select_personal_repo(a.to_str().unwrap()).unwrap();
    flush(&supervisor);
    assert_eq!(supervisor.personal_repo_folder().unwrap(), a);
    let back = record(&supervisor, id);
    assert_eq!(back.conversation.as_ref().unwrap().id, second);
    assert_eq!((back.history[1].reason.as_str(), back.history[1].via.as_str()), ("observed", "catalog"));

    let supervisor = restart(supervisor, root.path());
    assert_eq!(supervisor.personal_repo_folder().unwrap(), a);
    assert_eq!(recovery(&supervisor, id), None);
}

/// A member another hold covers keeps the register's own conversation: the
/// catalog's duplicate never overwrites it.
#[test]
fn a_duplicate_catalog_identity_keeps_the_register_value() {
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let (first, second) = ("18181818-0000-4000-8000-000000000018", "29292929-0000-4000-8000-000000000029");
    let (one, two) = {
        let supervisor = supervisor_at(root.path());
        let one = member(&supervisor, "One", DriverKind::Claude);
        let two = member(&supervisor, "Two", DriverKind::Claude);
        for (id, launched) in [(one, first), (two, second)] {
            supervisor.note_conversation_change(change(id, launched, "launch", "minted"));
            set_catalog_conversation(&supervisor, id, Some(launched));
        }
        flush(&supervisor);
        supervisor.shutdown().unwrap();
        (one, two)
    };
    edit_catalog(root.path(), |id, row| {
        if id == two {
            row["harness_session_id"] = serde_json::json!(first);
        }
    });
    let supervisor = supervisor_at(root.path());
    for id in [one, two] {
        assert_eq!(provenance(&supervisor, id).as_deref(), Some("duplicate_catalog_identity"));
    }
    flush(&supervisor);
    assert_eq!(registered(&supervisor, two).as_deref(), Some(second));
    assert_eq!(record(&supervisor, two).history.len(), 1);
    assert_eq!(registered(&supervisor, one).as_deref(), Some(first));
}

/// A stale register never replaces an in-use copy's hold (Codex room 223):
/// the catalog names B under `room_import_in_use`, the register still A. The
/// import hold stays active, so Resume here stays offered; A is kept in the
/// recovery history.
#[test]
fn a_stale_register_never_replaces_an_import_hold() {
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let (a, b, c) = (
        "a0a0a0a0-0000-4000-8000-0000000000a0",
        "b0b0b0b0-0000-4000-8000-0000000000b0",
        "c0c0c0c0-0000-4000-8000-0000000000c0",
    );
    let id = {
        let supervisor = supervisor_at(root.path());
        let id = member(&supervisor, "Attached", DriverKind::Claude);
        supervisor.note_conversation_change(change(id, a, "launch", "minted"));
        set_catalog_conversation(&supervisor, id, Some(a));
        flush(&supervisor);
        supervisor.shutdown().unwrap();
        id
    };
    // Attach for recovery committed B with its hold; the register write for
    // B never happened.
    let import_hold = RecoveryReference {
        original_id: Some(b.into()),
        provenance: "room_import_in_use".into(),
        reason: "This conversation is also used by the live PRIM-1.".into(),
        reported_codex: None,
    };
    edit_catalog(root.path(), |row_id, row| {
        if row_id == id {
            row["harness_session_id"] = serde_json::json!(b);
            row["recovery"] = serde_json::to_value(&import_hold).unwrap();
        }
    });
    let held_here = |supervisor: &SupervisorHandle, register: &str| {
        let slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id(id).unwrap();
        assert_eq!(slot.recovery.as_ref(), Some(&import_hold));
        assert!(
            slot.recovery_history.iter().any(|seen| seen.provenance == REGISTER_DISAGREES
                && seen.original_id.as_deref() == Some(b)
                && seen.reason.contains(register)),
            "{:?}",
            slot.recovery_history
        );
    };

    let supervisor = supervisor_at(root.path());
    held_here(&supervisor, a);
    // The catalog keeps it too, once the writer's watermark is saved.
    flush(&supervisor);
    assert_eq!(registered(&supervisor, id).as_deref(), Some(b));
    let persisted = read_session_catalog(&root.path().join("runtime").join(SESSION_CATALOG_FILE_NAME)).unwrap();
    let row = persisted.sessions.iter().find(|row| row.session_id == id).unwrap();
    assert_eq!(row.recovery.as_ref(), Some(&import_hold));
    assert!(row.recovery_history.iter().any(|seen| seen.provenance == REGISTER_DISAGREES && seen.reason.contains(a)));
    let supervisor = restart(supervisor, root.path());
    assert_eq!(recovery(&supervisor, id).as_ref(), Some(&import_hold));

    // Changed outside PRIM-1 as well: the import hold still decides.
    supervisor.shutdown().unwrap();
    drop(supervisor);
    let repo = fs::canonicalize(root.path().join("work")).unwrap();
    outside_edit(&register_path(&repo, id), |record| record.conversation = conversation(c));
    let supervisor = supervisor_at(root.path());
    held_here(&supervisor, c);

    // Resume here is still the operator's to choose.
    supervisor.resume_held_conversation(id).unwrap();
    assert_eq!(recovery(&supervisor, id), None);
    flush(&supervisor);
    let resumed = record(&supervisor, id);
    assert_eq!(resumed.conversation.as_ref().unwrap().id, b);
    assert_eq!(resumed.history.last().map(|entry| entry.reason.as_str()), Some("resume_here"));
}

/// One conversation the register names for two members holds both; the
/// holds are lifted once the register agrees again, and a Codex member gets
/// its identity hold back.
#[test]
fn one_conversation_named_twice_is_held_and_released_when_resolved() {
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let (shared, own, thread, other) = (
        "3a3a3a3a-0000-4000-8000-00000000003a",
        "4b4b4b4b-0000-4000-8000-00000000004b",
        "5c5c5c5c-0000-4000-8000-00000000005c",
        "6d6d6d6d-0000-4000-8000-00000000006d",
    );
    let (first, second, codex) = {
        let supervisor = supervisor_at(root.path());
        let ids = (
            member(&supervisor, "First", DriverKind::Claude),
            member(&supervisor, "Second", DriverKind::Claude),
            member(&supervisor, "Codex", DriverKind::Codex),
        );
        for (id, current) in [(ids.0, shared), (ids.1, own), (ids.2, thread)] {
            set_catalog_conversation(&supervisor, id, Some(current));
        }
        flush(&supervisor);
        supervisor.shutdown().unwrap();
        ids
    };
    let repo = fs::canonicalize(root.path().join("work")).unwrap();
    outside_edit(&register_path(&repo, second), |record| record.conversation = conversation(shared));
    outside_edit(&register_path(&repo, codex), |record| record.conversation = conversation(other));

    let supervisor = supervisor_at(root.path());
    assert_eq!(provenance(&supervisor, second).as_deref(), Some(REGISTER_DISAGREES));
    let duplicate = recovery(&supervisor, first).expect("held");
    assert_eq!(duplicate.provenance, REGISTER_DUPLICATE);
    assert_eq!(duplicate.original_id.as_deref(), Some(shared));
    assert!(duplicate.reason.contains(&second.to_string()), "{}", duplicate.reason);
    // The Codex identity hold it covered is retained.
    assert_eq!(provenance(&supervisor, codex).as_deref(), Some(REGISTER_DISAGREES));
    let covered = supervisor.inner.slots.lock().get_by_id(codex).unwrap().recovery_history.clone();
    assert!(covered.iter().any(|recovery| recovery.provenance == "legacy_catalog"), "{covered:?}");
    // Persisted with the next catalog write.
    supervisor.rename_session(first, "First renamed").unwrap();
    flush(&supervisor);
    let persisted = read_session_catalog(&root.path().join("runtime").join(SESSION_CATALOG_FILE_NAME)).unwrap();
    let row = |id: SessionId| persisted.sessions.iter().find(|row| row.session_id == id).unwrap().recovery.clone().map(|recovery| recovery.provenance);
    assert_eq!(row(first).as_deref(), Some(REGISTER_DUPLICATE));
    assert_eq!(row(second).as_deref(), Some(REGISTER_DISAGREES));
    supervisor.shutdown().unwrap();
    drop(supervisor);

    // Resolved outside PRIM-1: the register agrees with the catalog again.
    outside_edit(&register_path(&repo, second), |record| record.conversation = conversation(own));
    outside_edit(&register_path(&repo, codex), |record| record.conversation = conversation(thread));
    let supervisor = supervisor_at(root.path());
    assert_eq!(recovery(&supervisor, first), None);
    assert_eq!(recovery(&supervisor, second), None);
    let restored = recovery(&supervisor, codex).expect("Codex identity hold");
    assert_eq!((restored.provenance.as_str(), restored.original_id.as_deref()), ("legacy_catalog", Some(thread)));
    let history = supervisor.inner.slots.lock().get_by_id(second).unwrap().recovery_history.clone();
    assert!(history.iter().any(|recovery| recovery.provenance == REGISTER_DISAGREES));
}

#[test]
fn a_file_held_by_another_install_is_never_overwritten_or_used_for_holds() {
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let id = {
        let supervisor = supervisor_at(root.path());
        let id = member(&supervisor, "Shared", DriverKind::Claude);
        flush(&supervisor);
        supervisor.shutdown().unwrap();
        id
    };
    let repo = fs::canonicalize(root.path().join("work")).unwrap();
    let path = register_path(&repo, id);
    let mut other = read_register_file(&path).unwrap().unwrap();
    other.holder.install_id = Uuid::new_v4().to_string();
    other.conversation = conversation("12121212-0000-4000-8000-000000000012");
    let bytes = serde_json::to_vec_pretty(&other).unwrap();
    fs::write(&path, &bytes).unwrap();

    let supervisor = supervisor_at(root.path());
    assert_eq!(recovery(&supervisor, id), None);
    supervisor.rename_session(id, "Shared renamed").unwrap();
    flush(&supervisor);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(status_message(&supervisor).contains("another PRIM-1 install"), "{}", status_message(&supervisor));
}

/// Rechecked under the writer lock before the replace (Codex room 187): a
/// file another install took at the very revision the writer read is left
/// as it is.
#[test]
fn a_file_another_install_took_at_the_same_revision_is_never_replaced() {
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let (repo, id) = {
        let supervisor = supervisor_at(root.path());
        let id = member(&supervisor, "Taken", DriverKind::Claude);
        flush(&supervisor);
        let repo = supervisor.personal_repo_folder().unwrap();
        supervisor.shutdown().unwrap();
        (repo, id)
    };
    let path = register_path(&repo, id);
    let read = read_register_file(&path).unwrap().unwrap();
    let planned = ledger::RegisterRecordV1 { label: "Planned".into(), revision: read.revision + 1, ..read.clone() };
    let mut taken = read.clone();
    taken.holder.install_id = Uuid::new_v4().to_string();
    let bytes = serde_json::to_vec_pretty(&taken).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert!(!ledger::write_record_for_tests(&repo, &install_id(root.path()), planned.clone(), read.revision));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    // Still this install's at that revision: the same plan replaces it.
    fs::write(&path, serde_json::to_vec_pretty(&read).unwrap()).unwrap();
    assert!(ledger::write_record_for_tests(&repo, &install_id(root.path()), planned, read.revision));
    assert_eq!(read_register_file(&path).unwrap().unwrap().label, "Planned");
}

/// One writer at a time (Codex room 187): while another holds the register's
/// lock, changes wait like a failed write, the flush says they are not
/// written, and they land once it is released. A close meanwhile reports
/// them, and the next start writes them.
#[test]
fn a_held_writer_lock_defers_writes_and_the_answers_say_so() {
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let supervisor = supervisor_at(root.path());
    let id = member(&supervisor, "Locked", DriverKind::Claude);
    flush(&supervisor);
    let repo = supervisor.personal_repo_folder().unwrap();

    let lock = take_writer_lock(&repo);
    supervisor.rename_session(id, "Locked one").unwrap();
    assert_eq!(try_flush(&supervisor).unwritten_members, 1);
    assert_eq!(record(&supervisor, id).label, "Locked");
    assert!(status_message(&supervisor).contains(".writer.lock"), "{}", status_message(&supervisor));
    lock.unlock().unwrap();
    drop(lock);
    flush(&supervisor);
    assert_eq!(record(&supervisor, id).label, "Locked one");
    assert!(!status_message(&supervisor).contains(".writer.lock"));

    let lock = take_writer_lock(&repo);
    supervisor.rename_session(id, "Locked two").unwrap();
    let closed = supervisor.close_session_ledger(Duration::from_secs(60)).expect("the writer answers its close");
    assert_eq!((closed.unwritten_members, closed.git_running), (1, false));
    assert_eq!(record(&supervisor, id).label, "Locked one");
    lock.unlock().unwrap();
    drop(lock);
    let supervisor = restart(supervisor, root.path());
    flush(&supervisor);
    assert_eq!(record(&supervisor, id).label, "Locked two");
}

/// After the close nothing writes the register again: a repository chosen
/// then is refused, and a change is never written.
#[test]
fn nothing_writes_the_register_after_its_close() {
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let other = root.path().join("other");
    fs::create_dir_all(&other).unwrap();
    let supervisor = supervisor_at(root.path());
    let id = member(&supervisor, "Closed", DriverKind::Claude);
    flush(&supervisor);
    let closed = supervisor.close_session_ledger(Duration::from_secs(60)).expect("the writer answers its close");
    assert_eq!(closed.unwritten_members, 0);
    assert_eq!(supervisor.select_personal_repo(other.to_str().unwrap()).unwrap_err(), "PRIM-1 is closing");
    supervisor.rename_session(id, "Closed renamed").unwrap();
    assert_eq!(supervisor.flush_session_ledger(false, true, Duration::from_secs(1)), None);
    assert!(!other.join("prim1").exists());
    assert_eq!(record(&supervisor, id).label, "Closed");
}

/// A repository switch the close takes in starts no git: the old
/// repository gets no commit at close, and the member is written in the
/// new one.
#[test]
fn a_repository_switch_taken_in_at_close_starts_no_git() {
    if !git_available() {
        eprintln!("git is not available; skipping the commit test");
        return;
    }
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    fs::create_dir_all(&work).unwrap();
    init_repo(&work);
    let other = root.path().join("other");
    fs::create_dir_all(&other).unwrap();
    let supervisor = supervisor_at(root.path());
    let id = member(&supervisor, "Switching", DriverKind::Claude);
    flush(&supervisor);
    let commits = || git(&work, &["rev-list", "--count", "HEAD"]).trim().parse::<u32>().unwrap();
    let before = commits();

    // The writer is held in the pass that writes a change (it needs the
    // slots lock) while the switch and the close queue up behind it, in that
    // order: the change is written in the old repository, owed a commit,
    // when the close takes the switch in.
    let slots = supervisor.inner.slots.lock();
    supervisor.note_conversation_change(change(id, "5e5e5e5e-0000-4000-8000-00000000005e", "launch", "minted"));
    assert_eq!(supervisor.flush_session_ledger(false, false, Duration::from_millis(1)), None);
    supervisor.select_personal_repo(other.to_str().unwrap()).unwrap();
    assert_eq!(supervisor.close_session_ledger(Duration::from_millis(1)), None);
    drop(slots);

    // The close writes the member in the new repository last.
    let moved = register_path(&fs::canonicalize(&other).unwrap(), id);
    let deadline = Instant::now() + Duration::from_secs(60);
    while !moved.is_file() {
        assert!(Instant::now() < deadline, "the close never wrote the new repository");
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(commits(), before);
    let owed = git(&work, &["status", "--porcelain", "--", "prim1"]);
    assert!(owed.contains(&format!("prim1/sessions/{id}.json")), "{owed}");
}

/// A git started earlier and still running at close gets the close's budget,
/// is never killed, and is reported; one that ends within it is waited for.
#[test]
fn a_git_still_running_at_close_is_waited_for_never_killed_and_reported() {
    if !git_available() {
        eprintln!("git is not available; skipping the commit test");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let git = || {
        Command::new("git")
            .args(["hash-object", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut running = git();
    let input = running.stdin.take();
    let (report, child) = ledger::close_with_git_for_tests(root.path(), running, Instant::now() + Duration::from_millis(300));
    assert!(report.git_running, "{report:?}");
    let mut child = child.expect("the git is handed back");
    assert!(child.try_wait().unwrap().is_none(), "the git was killed");
    drop(input);
    assert!(child.wait().unwrap().success());

    let mut ending = git();
    drop(ending.stdin.take());
    let (report, child) = ledger::close_with_git_for_tests(root.path(), ending, Instant::now() + Duration::from_secs(60));
    assert!(!report.git_running && child.is_none(), "{report:?}");
}

/// Codex room 300: a commit the writer is waiting for (up to `git_wait`,
/// 30 s) does not hold the close past its budget. The writer stops waiting,
/// keeps the git, and the close reports it; the git is not killed and its
/// commit lands.
#[test]
fn a_close_during_a_slow_commit_answers_in_its_budget_and_leaves_the_git_running() {
    if !git_available() {
        eprintln!("git is not available; skipping the commit test");
        return;
    }
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    fs::create_dir_all(&work).unwrap();
    init_repo(&work);
    let supervisor = supervisor_at(root.path());
    let id = member(&supervisor, "Hooked", DriverKind::Claude);
    assert_eq!(flush(&supervisor).uncommitted_files, 0);
    let commits = || git(&work, &["rev-list", "--count", "HEAD"]).trim().parse::<u32>().unwrap();
    let before = commits();

    // From now on each commit runs a pre-commit hook that marks its start
    // and sleeps well past the close budget.
    let started = work.join(".git").join("prim1-hook-started");
    let hook = root.path().join("no-hooks").join("pre-commit");
    fs::write(&hook, "#!/bin/sh\n: > .git/prim1-hook-started\nsleep 4\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    supervisor.rename_session(id, "Hooked renamed").unwrap();
    let flusher = {
        let supervisor = supervisor.clone();
        thread::spawn(move || supervisor.flush_session_ledger(false, true, Duration::from_secs(60)))
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    while !started.is_file() {
        assert!(Instant::now() < deadline, "the commit never started");
        thread::sleep(Duration::from_millis(10));
    }

    let budget = Duration::from_secs(2);
    let asked = Instant::now();
    let closed = supervisor.close_session_ledger(budget).expect("the close is answered within its budget");
    assert!(asked.elapsed() <= budget, "{:?}", asked.elapsed());
    assert!(closed.git_running, "{closed:?}");
    assert_eq!((closed.unwritten_members, closed.uncommitted_files), (0, 1), "{closed:?}");
    let flushed = flusher.join().unwrap().expect("the flush is answered");
    assert_eq!(flushed.uncommitted_files, 1, "{flushed:?}");

    // Not killed: the hook ends and the commit lands.
    let deadline = Instant::now() + Duration::from_secs(60);
    while commits() == before {
        assert!(Instant::now() < deadline, "the git left running never committed");
        thread::sleep(Duration::from_millis(50));
    }
    let committed = git(&work, &["show", &format!("HEAD:prim1/sessions/{id}.json")]);
    assert!(committed.contains("Hooked renamed"), "{committed}");
}

#[test]
fn settings_choice_moves_the_register_and_the_default_task_folder() {
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let chosen = root.path().join("personal");
    fs::create_dir_all(&chosen).unwrap();
    let supervisor = supervisor_at(root.path());
    let missing = supervisor.select_personal_repo(root.path().join("nowhere").to_str().unwrap()).unwrap_err();
    assert!(missing.contains("does not exist"), "{missing}");
    assert!(!root.path().join("nowhere").exists());

    let status = supervisor.select_personal_repo(&format!("\"{}\"", chosen.display())).unwrap();
    assert_eq!((status.source, status.status), (PersonalRepoSource::Setting, PersonalRepoHealth::On), "{status:?}");
    assert!(root.path().join("runtime").join(PERSONAL_REPO_SETTING_FILE_NAME).is_file());
    let id = member(&supervisor, "Claude", DriverKind::Claude);
    flush(&supervisor);
    let repo = fs::canonicalize(&chosen).unwrap();
    assert!(register_path(&repo, id).is_file());
    drop(supervisor);

    // From the next start the default task folder is `<personal repo>\tasks`.
    let supervisor = supervisor_at(root.path());
    let tasks = supervisor.open_task_folder_from_setting();
    assert_eq!(tasks.source, TaskFolderSource::Default);
    assert_eq!(
        comparable_storage_path(Path::new(tasks.folder.as_deref().unwrap())),
        comparable_storage_path(&child_process_path(&repo).join("tasks"))
    );
}

/// A register off at start gets an install id when a folder is chosen.
#[test]
fn choosing_a_folder_after_an_unusable_one_starts_the_register() {
    let _env = EnvGuard::set(None);
    let root = tempfile::tempdir().unwrap();
    let chosen = root.path().join("personal");
    fs::create_dir_all(&chosen).unwrap();
    fs::create_dir_all(root.path().join("runtime")).unwrap();
    fs::write(root.path().join("runtime").join(PERSONAL_REPO_SETTING_FILE_NAME), b"not json").unwrap();
    let supervisor = supervisor_at(root.path());
    assert_eq!(supervisor.personal_repo_status().status, PersonalRepoHealth::Error);
    assert!(!root.path().join("runtime").join(INSTALL_ID_FILE_NAME).exists());
    let status = supervisor.select_personal_repo(chosen.to_str().unwrap()).unwrap();
    assert_eq!(status.status, PersonalRepoHealth::On, "{status:?}");
    let id = member(&supervisor, "Late", DriverKind::Claude);
    flush(&supervisor);
    assert_eq!(record(&supervisor, id).holder.install_id, install_id(root.path()));
}

#[test]
fn transcript_hints_follow_each_harness_layout() {
    let row = |driver, rollout: Option<(&str, &str)>| {
        ledger::ledger_rows(&[PersistedSessionV1 { codex_transport: Default::default(),
            session_id: Uuid::new_v4(),
            label: "x".into(),
            driver,
            working_directory: QualifiedWorkingDirectory {
                namespace: WorkingDirectoryNamespace::Windows,
                canonical_path: r"D:\All Repos\victor".into(),
                identity: "id".into(),
            },
            permission_profile: PermissionProfile::Normal,
            launch_choices: None,
            last_launch: None,
            harness_session_id: None,
            recovery: rollout.map(|(thread, path)| RecoveryReference {
                original_id: None,
                provenance: "job_attested_codex_shell_report".into(),
                reason: String::new(),
                reported_codex: Some(session::CodexIdentityEvidence {
                    thread_id: thread.into(),
                    run_id: Uuid::nil(),
                    generation: 1,
                    rollout_path: PathBuf::from(path),
                    reported_at: String::new(),
                }),
            }),
            recovery_history: Vec::new(),
            running_at_shutdown: false,
            permissions: Default::default(),
            last_pty_cols: None,
            last_pty_rows: None,
            created_by: None,
        }])
        .remove(0)
    };
    let id = "0199aaaa-0000-7000-8000-000000000001";
    let hint = |row: &ledger::LedgerRow| ledger::transcript_hint(row, Some(id)).map(|hint| (hint.home, hint.relative));
    assert_eq!(hint(&row(DriverKind::Claude, None)), Some(("~/.claude".into(), format!("projects/D--All-Repos-victor/{id}.jsonl"))));
    assert_eq!(hint(&row(DriverKind::Grok, None)), Some(("~/.grok".into(), format!("sessions/D%3A%5CAll%20Repos%5Cvictor/{id}"))));
    let rollout = format!(r"\\?\C:\Users\x\.codex\sessions\2026\10\19\rollout-2026-10-19T10-00-00-{id}.jsonl");
    assert_eq!(
        hint(&row(DriverKind::Codex, Some((id, &rollout)))),
        Some(("~/.codex".into(), format!("sessions/2026/10/19/rollout-2026-10-19T10-00-00-{id}.jsonl")))
    );
    // A report about another thread, or none, gives no Codex hint.
    assert_eq!(hint(&row(DriverKind::Codex, Some(("0199bbbb-0000-7000-8000-000000000002", &rollout)))), None);
    assert_eq!(hint(&row(DriverKind::Codex, None)), None);
    // Nothing unusual reaches a path.
    assert_eq!(ledger::transcript_hint(&row(DriverKind::Claude, None), Some("../x")), None);
}

#[test]
fn held_trial_has_no_register() {
    let root = tempfile::tempdir().unwrap();
    let ledger = ledger::SessionLedger::open(&root.path().join("runtime"), root.path(), true);
    let mut slots = SessionRegistry::default();
    let mut catalog = SessionCatalogV1::empty(QualifiedWorkingDirectory {
        namespace: WorkingDirectoryNamespace::Windows,
        canonical_path: root.path().display().to_string(),
        identity: "id".into(),
    });
    ledger.hold_register_disagreements(&mut slots, &mut catalog);
    assert!(catalog.sessions.is_empty());
    assert!(!root.path().join("runtime").join(INSTALL_ID_FILE_NAME).exists());
    assert_eq!(ledger.folder(), None);
}
