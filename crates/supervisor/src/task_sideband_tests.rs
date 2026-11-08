//! Isolated catalogs, temporary task folders and synthetic runs only; no
//! executable or named pipe starts.
use super::*;
use serde_json::{Value, json};
use shared_types::{CreateSessionRequest, PermissionProfile};

fn fixture(task_folder: Option<PathBuf>) -> (tempfile::TempDir, SupervisorHandle) {
    let root = tempfile::tempdir().unwrap();
    let supervisor = supervisor_at(root.path(), task_folder);
    (root, supervisor)
}

/// A supervisor on `root/runtime` with workspace `root/work`; a second call on
/// the same root is a restart.
fn supervisor_at(root: &Path, task_folder: Option<PathBuf>) -> SupervisorHandle {
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
        task_folder,
    })
    .unwrap()
}

fn with_store() -> (tempfile::TempDir, SupervisorHandle) {
    let (root, supervisor) = fixture(None);
    let store = tasks::TaskStore::open(&root.path().join("tasks")).unwrap();
    assert!(supervisor.set_task_store(Some(Arc::new(store))).is_none());
    (root, supervisor)
}

/// A session that belongs to no room: tasks need no membership.
fn lobby_member(supervisor: &SupervisorHandle, label: &str) -> SessionId {
    supervisor
        .create_session(CreateSessionRequest { codex_transport: Default::default(), permissions: None,
            label: Some(label.into()),
            driver: DriverKind::Claude,
            permission_profile: PermissionProfile::Normal,
            linux_working_directory: None,
            room_id: None,
            launch_choices: None,
        })
        .unwrap()
        .session_id
}

fn synthetic_run(supervisor: &SupervisorHandle, id: SessionId) -> PaneCaller {
    let mut slots = supervisor.inner.slots.lock();
    let slot = slots.get_by_id_mut(id).unwrap();
    slot.running = Some(RunningSession::new(None).with_pane_secret(Some("fixture-secret".into())));
    slot.run_id = Some(Uuid::new_v4());
    slot.generation += 1;
    slot.state = LifecycleState::Busy;
    PaneCaller {
        session: slot.definition.alias.clone(),
        session_id: id,
        generation: slot.generation,
        run_id: slot.run_id.unwrap(),
        identity: PaneCallerIdentity::Secret("fixture-secret".into()),
    }
}

fn tasks(query: &str, variables: Option<Value>) -> SidebandRequest {
    SidebandRequest::Tasks { query: query.into(), variables, operation_name: None }
}

fn create_request(title: &str) -> SidebandRequest {
    tasks(
        "mutation($input: NewTaskInput!) { createTask(input: $input) { id revision log { by } } }",
        Some(json!({ "input": { "title": title, "request": "Victor's words", "source": "room 465" } })),
    )
}

fn result(response: &SidebandResponse) -> &Value {
    match &response.payload {
        Some(SidebandResponsePayload::Tasks { result }) => result,
        other => panic!("expected a tasks payload, got {other:?} ({})", response.message),
    }
}

#[test]
fn lobby_pane_changes_tasks_as_its_own_identity() {
    let (_root, supervisor) = with_store();
    let id = lobby_member(&supervisor, "Claude PRIM-1");
    let caller = synthetic_run(&supervisor, id);
    assert!(!supervisor.inner.rooms.lock().room_by_session.contains_key(&id));

    let created = supervisor.apply_sideband_request(&caller, create_request("first"));
    assert!(created.ok, "{}", created.message);
    let identity = format!("Claude PRIM-1 (session {id})");
    assert_eq!(result(&created)["data"]["createTask"], json!({ "id": "T-1", "revision": 1, "log": [{ "by": identity }] }));

    let read = supervisor.apply_sideband_request(&caller, tasks("{ tasks { total items { id title } } }", None));
    assert!(read.ok, "{}", read.message);
    assert_eq!(result(&read)["data"]["tasks"], json!({ "total": 1, "items": [{ "id": "T-1", "title": "first" }] }));

    // The schema has no actor argument; an attempt to supply one is refused.
    let forged = supervisor.apply_sideband_request(
        &caller,
        tasks(
            "mutation { updateTask(id: \"T-1\", expectedRevision: 1, patch: { note: \"x\" }, actor: \"Victor\") { id } }",
            None,
        ),
    );
    assert!(!forged.ok);
    assert!(result(&forged)["errors"][0]["message"].as_str().unwrap().contains("actor"));
    let task = supervisor.task_store().unwrap().get("T-1").unwrap();
    assert_eq!((task.revision, task.log.len(), task.log[0].by.as_str()), (1, 1, identity.as_str()));
}

#[test]
fn stale_or_unknown_callers_are_refused_before_any_task_io() {
    let (_root, supervisor) = with_store();
    let id = lobby_member(&supervisor, "Codex");
    let caller = synthetic_run(&supervisor, id);
    let mut stale = caller.clone();
    stale.generation -= 1;
    let mut wrong_secret = caller.clone();
    wrong_secret.identity = PaneCallerIdentity::Secret("someone-else".into());
    for refused in [stale, wrong_secret] {
        let response = supervisor.apply_sideband_request(&refused, create_request("refused"));
        assert!(!response.ok);
        assert!(response.message.contains("stale"), "{}", response.message);
        assert!(response.payload.is_none());
    }
    // Size bounds are part of authorization.
    let oversized = supervisor.apply_sideband_request(&caller, tasks(&format!("{{ tasks {{ total }} }}{}", " ".repeat(16 * 1024)), None));
    assert!(!oversized.ok && oversized.payload.is_none(), "{}", oversized.message);
    assert!(oversized.message.contains("16 KiB"), "{}", oversized.message);
    let store = supervisor.task_store().unwrap();
    assert_eq!(store.list(&tasks::TaskFilter::default(), 0, 10).unwrap().total, 0);
}

#[test]
fn caller_is_revalidated_after_worker_admission_before_execution() {
    let (_root, supervisor) = with_store();
    let id = lobby_member(&supervisor, "Claude");
    let caller = synthetic_run(&supervisor, id);
    let handle = supervisor.clone();
    // The run is replaced after the initial authorization, before execution.
    *supervisor.inner.sideband_after_initial_authorization.lock() = Some(Box::new(move || {
        handle.inner.slots.lock().get_by_id_mut(id).unwrap().generation += 1;
    }));
    let response = supervisor.apply_sideband_request(&caller, create_request("too late"));
    assert!(!response.ok);
    assert!(response.message.contains("stale"), "{}", response.message);
    let store = supervisor.task_store().unwrap();
    assert_eq!(store.list(&tasks::TaskFilter::default(), 0, 10).unwrap().total, 0);
    assert!(!store.dir().join("T-1.json").exists());
}

#[test]
fn missing_or_unopenable_task_folder_is_reported_plainly() {
    let (_root, supervisor) = fixture(None);
    let id = lobby_member(&supervisor, "Claude");
    let caller = synthetic_run(&supervisor, id);
    let response = supervisor.apply_sideband_request(&caller, tasks("{ tasks { total } }", None));
    assert!(!response.ok);
    assert_eq!(response.message, format!("task request returned 1 error(s); first: {}", task_graphql::NO_TASK_FOLDER));
    assert_eq!(result(&response)["errors"][0]["extensions"]["code"], "NO_TASK_FOLDER");

    // A folder another store owns cannot be opened at start; the runtime
    // still starts and says why.
    let folders = tempfile::tempdir().unwrap();
    let owned = folders.path().join("owned");
    let owner = tasks::TaskStore::open(&owned).unwrap();
    let (_root, blocked) = fixture(Some(owned.clone()));
    assert!(blocked.task_store().is_none());
    let id = lobby_member(&blocked, "Claude");
    let caller = synthetic_run(&blocked, id);
    let response = blocked.apply_sideband_request(&caller, tasks("{ tasks { total } }", None));
    let message = result(&response)["errors"][0]["message"].as_str().unwrap().to_owned();
    assert!(message.contains("could not be opened") && message.contains("another PRIM-1 runtime"), "{message}");
    drop(owner);

    // A configured folder opens at start and can be replaced.
    let (_root, configured) = fixture(Some(owned.clone()));
    let first = configured.task_store().expect("configured folder opens at start");
    assert_eq!(first.dir(), owned.as_path());
    let other = Arc::new(tasks::TaskStore::open(&folders.path().join("other")).unwrap());
    let previous = configured.set_task_store(Some(Arc::clone(&other))).unwrap();
    assert!(Arc::ptr_eq(&previous, &first));
    assert!(Arc::ptr_eq(&configured.task_store().unwrap(), &other));
    drop((first, previous));
    // Dropping the replaced store released its folder.
    drop(tasks::TaskStore::open(&owned).unwrap());
}

// ── Settings → Task folder and the desktop's read-only view ──

use crate::task_sideband::{TASK_FOLDER_SETTING_FILE_NAME, default_task_folder, read_task_folder_setting};

fn canonical(path: &Path) -> PathBuf {
    child_process_path(&fs::canonicalize(path).unwrap())
}

fn shown(path: &Path) -> Option<String> {
    Some(path.display().to_string())
}

fn setting_path(root: &Path) -> PathBuf {
    root.join("runtime").join(TASK_FOLDER_SETTING_FILE_NAME)
}

#[test]
fn default_folder_opens_at_start_and_a_chosen_folder_persists_across_restart() {
    let (root, supervisor) = fixture(None);
    let work = canonical(&root.path().join("work"));
    let status = supervisor.open_task_folder_from_setting();
    assert_eq!(
        status,
        TaskFolderStatus {
            folder: shown(&work.join("tasks")),
            source: TaskFolderSource::Default,
            status: TaskFolderHealth::Open,
            error: None,
        }
    );
    // The default is not saved, and a second start call changes nothing.
    assert!(!setting_path(root.path()).exists());
    let default_store = supervisor.task_store().unwrap();
    assert_eq!(supervisor.open_task_folder_from_setting(), status);
    assert!(Arc::ptr_eq(&supervisor.task_store().unwrap(), &default_store));
    drop(default_store);

    // Explorer's "Copy as path" quotes; the folder does not exist yet.
    let chosen = root.path().join("chosen tasks");
    let selected = supervisor.select_task_folder(&format!("  \"{}\"  ", chosen.display())).unwrap();
    let chosen = canonical(&chosen);
    assert_eq!(
        selected,
        TaskFolderStatus {
            folder: shown(&chosen),
            source: TaskFolderSource::Setting,
            status: TaskFolderHealth::Open,
            error: None,
        }
    );
    assert_eq!(supervisor.task_folder_status(), selected);
    assert_eq!(read_task_folder_setting(&root.path().join("runtime")).unwrap(), Some(chosen.clone()));
    // Selecting moved nothing, and the default folder's lock was released.
    assert_eq!(fs::read_dir(&chosen).unwrap().count(), 1, "only the chosen folder's own lock file");
    drop(tasks::TaskStore::open(&work.join("tasks")).unwrap());

    // Restart returns to the chosen folder, not the default.
    drop(supervisor);
    let restarted = supervisor_at(root.path(), None);
    assert_eq!(restarted.open_task_folder_from_setting(), selected);
    assert_eq!(restarted.task_store().unwrap().dir(), chosen.as_path());

    // With the workspace gone, the default is the runtime directory's tasks folder.
    let runtime = root.path().join("runtime");
    assert_eq!(default_task_folder(&runtime, &root.path().join("missing")), runtime.join("tasks"));
    assert_eq!(default_task_folder(&runtime, &work), work.join("tasks"));
}

#[cfg(windows)]
#[test]
fn choosing_the_open_folder_again_is_a_no_op_without_a_second_lock() {
    let (root, supervisor) = fixture(None);
    supervisor.open_task_folder_from_setting();
    let open = supervisor.task_store().unwrap();
    // The same folder spelled differently: other case, a `.` component and a
    // trailing separator. Opening it again would fail: this process owns it.
    let spelled = format!("{}\\.\\", open.dir().display()).to_uppercase();
    let saved = supervisor.select_task_folder(&spelled).unwrap();
    assert!(Arc::ptr_eq(&supervisor.task_store().unwrap(), &open));
    // Picking the default explicitly saves it as the choice.
    assert_eq!(saved.source, TaskFolderSource::Setting);
    assert_eq!(saved.folder, shown(open.dir()));
    let setting = fs::read(setting_path(root.path())).unwrap();
    assert_eq!(read_task_folder_setting(&root.path().join("runtime")).unwrap().as_deref(), Some(open.dir()));

    let again = supervisor.select_task_folder(&open.dir().display().to_string()).unwrap();
    assert_eq!(again, saved);
    assert!(Arc::ptr_eq(&supervisor.task_store().unwrap(), &open));
    assert_eq!(fs::read(setting_path(root.path())).unwrap(), setting);
}

#[test]
fn a_folder_in_use_or_unusable_keeps_the_current_folder_and_setting() {
    let (root, supervisor) = fixture(None);
    let first = root.path().join("first");
    supervisor.select_task_folder(&first.display().to_string()).unwrap();
    let first = canonical(&first);
    let open = supervisor.task_store().unwrap();
    let before = supervisor.task_folder_status();
    let setting = fs::read(setting_path(root.path())).unwrap();

    // "Still in use": any other owner of the folder's lock.
    let owned = root.path().join("owned");
    let owner = tasks::TaskStore::open(&owned).unwrap();
    let file = root.path().join("a file.txt");
    fs::write(&file, b"not a folder").unwrap();
    for (input, expected) in [
        (owned.display().to_string(), "That folder is still in use (by another PRIM-1, or by a request still finishing here). Try again in a moment."),
        (file.display().to_string(), "is a file, not a folder"),
        (file.join("below").display().to_string(), "Cannot create or write that folder"),
        (r"relative\tasks".to_owned(), "full path"),
        ("  ".to_owned(), "Enter a folder path"),
    ] {
        let error = supervisor.select_task_folder(&input).unwrap_err();
        assert!(error.contains(expected), "{input}: {error}");
        assert!(Arc::ptr_eq(&supervisor.task_store().unwrap(), &open), "{input}");
        assert_eq!(supervisor.task_folder_status(), before, "{input}");
        assert_eq!(fs::read(setting_path(root.path())).unwrap(), setting, "{input}");
    }

    // The new folder opens, but the choice cannot be saved: nothing switches
    // and the new folder's lock is released again.
    let unsaved = root.path().join("unsaved");
    fs::remove_file(setting_path(root.path())).unwrap();
    fs::create_dir(setting_path(root.path())).unwrap();
    let error = supervisor.select_task_folder(&unsaved.display().to_string()).unwrap_err();
    assert!(error.contains("could not be saved") && error.contains("previous folder is still in use"), "{error}");
    assert!(Arc::ptr_eq(&supervisor.task_store().unwrap(), &open));
    assert_eq!(supervisor.task_folder_status(), before);
    drop(tasks::TaskStore::open(&unsaved).unwrap());
    fs::remove_dir(setting_path(root.path())).unwrap();

    // Once the other owner lets go, the same folder can be chosen; the old
    // folder is released and its tasks stay where they were.
    drop(owner);
    let switched = supervisor.select_task_folder(&owned.display().to_string()).unwrap();
    assert_eq!(switched.folder, shown(&canonical(&owned)));
    assert!(!Arc::ptr_eq(&supervisor.task_store().unwrap(), &open));
    drop(open);
    drop(tasks::TaskStore::open(&first).unwrap());
}

#[test]
fn an_unreadable_saved_choice_is_reported_not_replaced_by_the_default() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("runtime")).unwrap();
    fs::write(setting_path(root.path()), b"{ not json").unwrap();
    let supervisor = supervisor_at(root.path(), None);
    let status = supervisor.open_task_folder_from_setting();
    assert_eq!(
        (status.folder.as_deref(), status.source, status.status),
        (None, TaskFolderSource::Setting, TaskFolderHealth::Error)
    );
    assert!(status.error.as_deref().unwrap().contains("cannot be used"), "{status:?}");
    assert!(!root.path().join("work").join("tasks").exists(), "the default must not be opened instead");
    let read = supervisor.read_tasks("{ tasks { total } }", None, None);
    assert_eq!(read["errors"][0]["extensions"]["code"], "NO_TASK_FOLDER");

    // Choosing a folder replaces the broken file.
    let fixed = root.path().join("fixed");
    supervisor.select_task_folder(&fixed.display().to_string()).unwrap();
    assert_eq!(read_task_folder_setting(&root.path().join("runtime")).unwrap(), Some(canonical(&fixed)));
}

#[test]
fn desktop_task_reads_are_read_only() {
    let (_root, supervisor) = with_store();
    let id = lobby_member(&supervisor, "Claude");
    let caller = synthetic_run(&supervisor, id);
    assert!(supervisor.apply_sideband_request(&caller, create_request("seeded")).ok);

    let list = "query($state: TaskState!) { tasks(state: $state, offset: 0, limit: 200) { total items { id title owner state updatedAt } unreadable { file error } } }";
    let read = supervisor.read_tasks(list, Some(&json!({ "state": "open" })), None);
    assert!(read.get("errors").is_none(), "{read}");
    assert_eq!(read["data"]["tasks"]["total"], 1);
    assert_eq!(read["data"]["tasks"]["items"][0]["title"], "seeded");

    for mutation in [
        "mutation { createTask(input: { title: \"t\", request: \"r\", source: \"s\" }) { id } }",
        "mutation { updateTask(id: \"T-1\", expectedRevision: 1, patch: { state: done }) { id } }",
    ] {
        let refused = supervisor.read_tasks(mutation, None, None);
        assert_eq!(refused["errors"][0]["extensions"]["code"], "READ_ONLY", "{refused}");
    }
    let task = supervisor.task_store().unwrap().get("T-1").unwrap();
    assert_eq!((task.revision, task.state), (1, tasks::TaskState::Open));
    assert_eq!(supervisor.task_store().unwrap().list(&tasks::TaskFilter::default(), 0, 10).unwrap().total, 1);
}
