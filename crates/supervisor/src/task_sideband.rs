//! The pane `tasks` request, the desktop's read-only task view, and the task
//! folder this runtime owns: which folder (the Settings choice saved in the
//! runtime directory, else the default in the personal repository), opening
//! it at start, and switching to another. Switching selects a folder; nothing
//! is copied or moved. GraphQL and task file I/O run on a blocking worker that
//! holds no session or room lock; the slots lock is taken only for the short
//! caller checks.
use super::*;
use crate::{
    task_graphql::{self, MutationGuard, TaskOutcome},
    tasks::{TaskError, TaskStore},
};
use parking_lot::RwLockUpgradableReadGuard;
use serde_json::Value as Json;

/// The Settings choice, in the runtime directory. A file of its own rather than
/// a session-catalog field, so an older build reading the catalog is unaffected.
pub const TASK_FOLDER_SETTING_FILE_NAME: &str = "task-folder.json";
const TASK_FOLDER_SETTING_SCHEMA_VERSION: u32 = 1;
const TASK_FOLDER_SETTING_MAX_BYTES: u64 = 64 * 1024;
/// With no saved choice: `<personal repo>\tasks`; with the register off,
/// `<workspace>\tasks`, or `<runtime>\tasks` when the workspace is gone.
pub const DEFAULT_TASK_FOLDER_NAME: &str = "tasks";
const TASK_FOLDER_HELD: &str =
    "This preview is held, so it does not open the task folder; the running PRIM-1 keeps it.";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskFolderSource {
    /// Chosen in Settings (or given by the embedding program).
    #[default]
    Setting,
    /// Nothing chosen yet.
    Default,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskFolderHealth {
    Open,
    Error,
}

/// What the desktop shows about the task folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TaskFolderStatus {
    /// The selected folder, open or not. None only when no folder was given or
    /// the saved choice cannot be read.
    pub folder: Option<String>,
    pub source: TaskFolderSource,
    pub status: TaskFolderHealth,
    /// Why the folder is not open, in words for the person reading Settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskFolderSetting {
    schema_version: u32,
    folder: String,
}

/// The task folder this runtime owns, if any.
#[derive(Default)]
pub(crate) struct TaskFolderState {
    store: Option<Arc<TaskStore>>,
    /// The selected folder, also while it is not open.
    folder: Option<PathBuf>,
    source: TaskFolderSource,
    /// Why the selected folder is not open.
    unavailable: Option<String>,
}

impl TaskFolderState {
    /// A held trial never opens the folder: it must not take the live
    /// runtime's lock, and it admits no task requests anyway.
    pub(crate) fn open_at_startup(folder: Option<&Path>, trial: bool) -> Self {
        let Some(folder) = folder else {
            return Self::default();
        };
        let selected = Self { folder: Some(folder.to_path_buf()), ..Self::default() };
        if trial {
            return Self { unavailable: Some(TASK_FOLDER_HELD.into()), ..selected };
        }
        match TaskStore::open(folder) {
            Ok(store) => Self { store: Some(Arc::new(store)), ..selected },
            Err(error) => Self { unavailable: Some(not_opened(folder, &error)), ..selected },
        }
    }

    pub(crate) fn unavailable(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    fn status(&self) -> TaskFolderStatus {
        let folder = self.store.as_ref().map(|store| store.dir().to_path_buf()).or_else(|| self.folder.clone());
        TaskFolderStatus {
            folder: folder.map(|folder| folder.display().to_string()),
            source: self.source,
            status: if self.store.is_some() { TaskFolderHealth::Open } else { TaskFolderHealth::Error },
            error: self.store.is_none().then(|| {
                self.unavailable.clone().unwrap_or_else(|| task_graphql::NO_TASK_FOLDER.to_owned())
            }),
        }
    }
}

fn not_opened(folder: &Path, error: &TaskError) -> String {
    format!(
        "The task folder {} could not be opened: {error}. Choose a folder in PRIM-1 Settings → Task folder.",
        folder.display()
    )
}

impl SupervisorHandle {
    /// Installs `store` (or removes the current one with `None`) and returns
    /// the previous store. A store is released when its last holder drops it,
    /// so a request already running finishes against the store it started on.
    pub fn set_task_store(&self, store: Option<Arc<TaskStore>>) -> Option<Arc<TaskStore>> {
        let mut folder = self.inner.task_folder.write();
        folder.unavailable = None;
        folder.folder = store.as_ref().map(|store| store.dir().to_path_buf());
        std::mem::replace(&mut folder.store, store)
    }

    pub fn task_store(&self) -> Option<Arc<TaskStore>> {
        self.inner.task_folder.read().store.clone()
    }

    pub fn task_folder_status(&self) -> TaskFolderStatus {
        self.inner.task_folder.read().status()
    }

    /// At start: opens the folder saved in Settings, or the default when none
    /// was chosen. A saved choice that cannot be read is reported, never
    /// replaced by the default. A held trial resolves the folder but does not
    /// open it. Does nothing while a folder is already open. Blocking file I/O.
    pub fn open_task_folder_from_setting(&self) -> TaskFolderStatus {
        let trial = self.inner.startup_admission.is_trial();
        let workspace = PathBuf::from(&self.inner.catalog.lock().workspace_preference.canonical_path);
        let personal_repo = self.personal_repo_folder();
        let state = self.inner.task_folder.upgradable_read();
        if state.store.is_some() {
            return state.status();
        }
        let next = match resolve_task_folder(&self.inner.runtime_dir, &workspace, personal_repo.as_deref()) {
            Err(message) => TaskFolderState { unavailable: Some(message), ..TaskFolderState::default() },
            Ok((folder, source)) if trial => TaskFolderState {
                folder: Some(folder),
                source,
                unavailable: Some(TASK_FOLDER_HELD.into()),
                store: None,
            },
            Ok((folder, source)) => match open_canonical(&folder) {
                Ok(store) => TaskFolderState {
                    folder: Some(store.dir().to_path_buf()),
                    store: Some(Arc::new(store)),
                    source,
                    unavailable: None,
                },
                Err(error) => TaskFolderState {
                    unavailable: Some(not_opened(&folder, &error)),
                    folder: Some(folder),
                    source,
                    store: None,
                },
            },
        };
        let mut state = RwLockUpgradableReadGuard::upgrade(state);
        *state = next;
        let status = state.status();
        drop(state);
        if let (false, Some(message)) = (trial, &status.error) {
            self.emit(RuntimeEvent::SystemLog {
                level: LogLevel::Warn,
                message: message.clone(),
                timestamp: now_rfc3339(),
            });
        }
        status
    }

    /// Settings → Task folder. Opens `requested` (creating it if needed) and
    /// takes its lock before anything changes; only then saves the choice and
    /// switches. Any failure keeps the current folder and setting. The folder
    /// already open is a successful no-op that takes no second lock (it only
    /// saves the choice if it was the default). Blocking file I/O.
    pub fn select_task_folder(&self, requested: &str) -> std::result::Result<TaskFolderStatus, String> {
        self.select_task_folder_checked(requested, None)
    }

    /// `check`: a member's runtime_settings set (A16), run under the session
    /// lock, which is then held through the saved choice and the switch (the
    /// new folder is open and locked first; a refusal drops it and keeps the
    /// current one). Lock order: task folder, then sessions; no path takes
    /// them the other way round.
    pub(crate) fn select_task_folder_checked(
        &self, requested: &str, check: Option<&dyn Fn(&SessionRegistry) -> Result<()>>,
    ) -> std::result::Result<TaskFolderStatus, String> {
        self.ensure_active().map_err(|error| error.to_string())?;
        let folder = canonical_folder(&parse_folder_input(requested)?).map_err(|error| selection_error(&error))?;
        // One selection at a time; readers and pane requests are not blocked.
        let state = self.inner.task_folder.upgradable_read();
        let already_open = state.store.as_ref().is_some_and(|store| same_folder(store.dir(), &folder));
        if already_open {
            if state.source == TaskFolderSource::Setting {
                return Ok(state.status());
            }
            let _authority = authorize_selection(&self.inner.slots, check)?;
            let dir = state.store.as_ref().map(|store| store.dir().to_path_buf()).unwrap_or(folder);
            persist_task_folder_setting(&self.inner.runtime_dir, &dir)?;
            let mut state = RwLockUpgradableReadGuard::upgrade(state);
            state.source = TaskFolderSource::Setting;
            return Ok(state.status());
        }
        let store = TaskStore::open(&folder).map_err(|error| selection_error(&error))?;
        let authority = authorize_selection(&self.inner.slots, check)?;
        // A failed save drops `store`, releasing the new folder's lock.
        persist_task_folder_setting(&self.inner.runtime_dir, &folder)?;
        let mut state = RwLockUpgradableReadGuard::upgrade(state);
        let previous = state.store.replace(Arc::new(store));
        state.folder = Some(folder);
        state.source = TaskFolderSource::Setting;
        state.unavailable = None;
        let status = state.status();
        drop(state);
        drop(authority);
        // Released once any request still running on it finishes.
        drop(previous);
        Ok(status)
    }

    /// The desktop's read-only task view: GraphQL `{data, errors}` from a
    /// schema with no mutation type. Blocking file I/O.
    pub fn read_tasks(&self, query: &str, variables: Option<&Json>, operation_name: Option<&str>) -> Json {
        let store = {
            let folder = self.inner.task_folder.read();
            match &folder.store {
                Some(store) => Arc::clone(store),
                None => {
                    let message = folder.unavailable.as_deref().unwrap_or(task_graphql::NO_TASK_FOLDER);
                    return task_graphql::no_task_folder(message).result;
                }
            }
        };
        task_graphql::execute_read_only(store, query, variables, operation_name).result
    }

    /// Runs on a blocking worker, after any wait for one.
    pub(super) fn apply_tasks_request(&self, caller: &PaneCaller, request: SidebandRequest) -> SidebandResponse {
        if let Err(error) = self.authorize_sideband_request(caller, &request) {
            return Self::rejected_sideband_response(error.to_string());
        }
        #[cfg(test)]
        self.run_sideband_after_initial_authorization_for_tests();
        let SidebandRequest::Tasks { query, variables, operation_name } = request else {
            return Self::rejected_sideband_response("not a task request");
        };
        let store = {
            let folder = self.inner.task_folder.read();
            match &folder.store {
                Some(store) => Arc::clone(store),
                None => {
                    let message = folder.unavailable.as_deref().unwrap_or(task_graphql::NO_TASK_FOLDER);
                    return tasks_response(task_graphql::no_task_folder(message));
                }
            }
        };
        // Revalidated immediately before execution, and again by the guard
        // immediately before each mutation reaches the store.
        let actor = match self.task_actor(caller) {
            Ok(actor) => actor,
            Err(error) => return Self::rejected_sideband_response(error.to_string()),
        };
        let guard_handle = self.clone();
        let guard_caller = caller.clone();
        let guard: MutationGuard = Box::new(move || {
            guard_handle.task_actor(&guard_caller).map(drop).map_err(|error| error.to_string())
        });
        let outcome = task_graphql::execute_agent(
            store,
            &actor,
            Some(guard),
            &query,
            variables.as_ref(),
            operation_name.as_deref(),
        );
        // A result is returned only to the run that is still current. Task
        // content is withheld; what took effect is still named.
        if let Err(error) = self.task_actor(caller) {
            let mut message = error.to_string();
            if !outcome.committed.is_empty() {
                message.push_str(&format!(
                    "; confirmed current task results (including replays) before the check: {}",
                    task_graphql::describe_confirmed(&outcome.committed)
                ));
            }
            return Self::rejected_sideband_response(message);
        }
        tasks_response(outcome)
    }

    /// Confirms the caller is still the current, admitted run and names it:
    /// the session's label and id. A request never supplies its own actor.
    fn task_actor(&self, caller: &PaneCaller) -> Result<String> {
        self.ensure_active()?;
        let slots = self.inner.slots.lock();
        Self::validate_pane_caller_locked(caller, &slots)?;
        let slot = slots
            .get_by_id(caller.session_id)
            .ok_or_else(|| anyhow!("sideband caller run is stale"))?;
        Ok(format!("{} (session {})", slot.definition.label, caller.session_id))
    }
}

/// The saved choice, else the default. A saved choice that cannot be read is
/// an error: falling back would quietly show a different task list.
pub(crate) fn resolve_task_folder(
    runtime_dir: &Path,
    workspace: &Path,
    personal_repo: Option<&Path>,
) -> std::result::Result<(PathBuf, TaskFolderSource), String> {
    Ok(match read_task_folder_setting(runtime_dir)? {
        Some(folder) => (folder, TaskFolderSource::Setting),
        None => (
            personal_repo.map_or_else(
                || default_task_folder(runtime_dir, workspace),
                |repo| repo.join(DEFAULT_TASK_FOLDER_NAME),
            ),
            TaskFolderSource::Default,
        ),
    })
}

pub(crate) fn default_task_folder(runtime_dir: &Path, workspace: &Path) -> PathBuf {
    let base = if workspace.is_dir() { workspace } else { runtime_dir };
    base.join(DEFAULT_TASK_FOLDER_NAME)
}

/// None when nothing was saved.
pub(crate) fn read_task_folder_setting(runtime_dir: &Path) -> std::result::Result<Option<PathBuf>, String> {
    use std::io::Read as _;

    let path = runtime_dir.join(TASK_FOLDER_SETTING_FILE_NAME);
    let unusable = |detail: String| {
        format!(
            "The saved task folder choice ({}) cannot be used: {detail}. Choose a folder in PRIM-1 Settings → Task folder to replace it.",
            path.display()
        )
    };
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unusable(error.to_string())),
    };
    let mut bytes = Vec::new();
    file.take(TASK_FOLDER_SETTING_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| unusable(error.to_string()))?;
    if bytes.len() as u64 > TASK_FOLDER_SETTING_MAX_BYTES {
        return Err(unusable("the file is larger than 64 KiB".into()));
    }
    let setting: TaskFolderSetting = serde_json::from_slice(&bytes).map_err(|error| unusable(error.to_string()))?;
    if setting.schema_version != TASK_FOLDER_SETTING_SCHEMA_VERSION {
        return Err(unusable(format!("unsupported schema version {}", setting.schema_version)));
    }
    let folder = PathBuf::from(&setting.folder);
    if setting.folder.trim() != setting.folder || setting.folder.contains('\0') || !folder.is_absolute() {
        return Err(unusable("the folder is not a full path".into()));
    }
    Ok(Some(folder))
}

/// Atomic replacement, the same way the session and room catalogs are written.
fn persist_task_folder_setting(runtime_dir: &Path, folder: &Path) -> std::result::Result<(), String> {
    let folder = folder
        .to_str()
        .ok_or("That folder path cannot be saved because it is not valid Unicode; the previous folder is still in use.")?;
    let setting = TaskFolderSetting { schema_version: TASK_FOLDER_SETTING_SCHEMA_VERSION, folder: folder.to_owned() };
    persist_catalog(runtime_dir, TASK_FOLDER_SETTING_FILE_NAME, "task folder setting", &setting).map_err(|error| {
        format!("The task folder setting could not be saved ({error:#}); the previous folder is still in use.")
    })
}

/// The Settings text box: trimmed, one pair of surrounding quotes removed
/// (Explorer's "Copy as path" adds them), and a full path.
pub(crate) fn parse_folder_input(input: &str) -> std::result::Result<PathBuf, String> {
    let trimmed = input.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(trimmed)
        .trim();
    if unquoted.is_empty() {
        return Err("Enter a folder path, such as D:\\Work\\tasks.".into());
    }
    let path = PathBuf::from(unquoted);
    if unquoted.contains('\0') || !path.is_absolute() {
        return Err(format!("Enter the full path of a folder, such as D:\\Work\\tasks (not {}).", unquoted.replace('\0', "")));
    }
    Ok(path)
}

/// Creates `folder` if needed and returns its canonical path without the
/// `\\?\` prefix, so one folder compares equal however it was typed.
fn canonical_folder(folder: &Path) -> std::result::Result<PathBuf, TaskError> {
    if folder.exists() && !folder.is_dir() {
        return Err(TaskError::Invalid(format!("{} is a file, not a folder.", folder.display())));
    }
    fs::create_dir_all(folder)
        .map_err(|error| TaskError::Io(format!("could not create {} ({error})", folder.display())))?;
    let canonical = fs::canonicalize(folder)
        .map_err(|error| TaskError::Io(format!("could not resolve {} ({error})", folder.display())))?;
    Ok(child_process_path(&canonical))
}

fn open_canonical(folder: &Path) -> std::result::Result<TaskStore, TaskError> {
    TaskStore::open(&canonical_folder(folder)?)
}

fn same_folder(open: &Path, candidate: &Path) -> bool {
    let open = fs::canonicalize(open).map_or_else(|_| open.to_path_buf(), |path| child_process_path(&path));
    comparable_storage_path(&open) == comparable_storage_path(candidate)
}

fn selection_error(error: &TaskError) -> String {
    match error {
        TaskError::StoreOwned => {
            "That folder is still in use (by another PRIM-1, or by a request still finishing here). Try again in a moment.".into()
        }
        TaskError::Io(detail) => format!("Cannot create or write that folder: {detail}."),
        TaskError::Invalid(message) => message.clone(),
        other => format!("That folder cannot be used for tasks: {other}"),
    }
}

fn tasks_response(outcome: TaskOutcome) -> SidebandResponse {
    let errors = outcome.errors().len();
    let message = match outcome.first_error_message() {
        None => "task request completed".to_owned(),
        Some(first) => format!("task request returned {errors} error(s); first: {first}"),
    };
    SidebandResponse {
        ok: errors == 0,
        message,
        timed_out: false,
        payload: Some(SidebandResponsePayload::Tasks { result: outcome.result }),
        request_id: None,
    }
}

/// A16: a member's grant, checked under the session lock, which the caller
/// keeps until its choice is saved and switched.
pub(crate) fn authorize_selection<'a>(
    slots: &'a parking_lot::Mutex<SessionRegistry>, check: Option<&dyn Fn(&SessionRegistry) -> Result<()>>,
) -> std::result::Result<Option<parking_lot::MutexGuard<'a, SessionRegistry>>, String> {
    let Some(check) = check else { return Ok(None) };
    let guard = slots.lock();
    check(&guard).map_err(|error| error.to_string())?;
    Ok(Some(guard))
}
