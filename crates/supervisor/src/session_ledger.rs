//! A108: the session register in the personal repo. Each member's exact
//! current conversation, its history and its holder, one Git-tracked file per
//! member. The runtime catalog keeps launch authority; the register is the
//! durable, portable identity record and never grants a launch. Display names
//! are never identity.
//!
//! Callers never wait on it. A successful catalog write, every proven
//! conversation change and every run start or end are handed to one
//! background writer over a channel; no file or Git I/O runs under the
//! slots, catalog or rooms locks. The writer
//! coalesces a burst, writes only the members this install holds (temp file
//! then replace, sorted keys, LF, a revision one above the file on disk) and
//! commits them about 20 s after the last write with `git commit --only`. It
//! never stages another path and never pushes. One writer at a time per
//! repository, across installs and copied runtimes: each file is re-read,
//! its holder and revision rechecked and replaced under an OS lock on
//! `prim1\.writer.lock`.
//!
//! Before it replaces a file, the writer records what it is about to write in
//! the catalog (`register_acks`). A file at or behind that watermark is this
//! install's own and may lag the catalog (a failed write, the register off, a
//! crash); the catalog then fills it in. Only a file ahead of the watermark,
//! changed by something else, can hold a member at start.
use super::*;
use chrono::SecondsFormat;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io::Read as _,
    process::{Child, Command, Stdio},
};

/// One change of a member's current conversation, proven by `via`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationChange {
    pub session_id: SessionId,
    pub harness: DriverKind,
    pub conversation_id: String,
    /// launch | clear | new | resume | fork | rename | start_fresh | resume_here | import
    /// | observed (the conversation is proven; why it became current is not)
    pub reason: &'static str,
    /// minted | pid_file | writer_lock | grok_log | codex_identify | resume_here | import
    /// | catalog (seen in this install's runtime catalog, no proof event)
    pub via: &'static str,
    pub run_id: Option<Uuid>,
    pub pid: Option<u32>,
}

/// The Settings choice, in the runtime directory, like `task-folder.json`.
pub const PERSONAL_REPO_SETTING_FILE_NAME: &str = "personal-repo.json";
/// `off` disables the register; a full path overrides the Settings choice.
/// QA and copied runtimes set it so they never write Victor's register.
pub const PERSONAL_REPO_ENV: &str = "PRIM1_PERSONAL_REPO";
/// One UUID per runtime, created once: this install's name as a holder.
pub const INSTALL_ID_FILE_NAME: &str = "install-id";
const PERSONAL_REPO_SETTING_SCHEMA_VERSION: u32 = 1;
const SETTING_MAX_BYTES: u64 = 64 * 1024;
pub(crate) const REGISTER_SCHEMA: u32 = 1;
pub(crate) const REGISTER_DIR: &str = "prim1";
pub(crate) const REGISTER_SESSIONS_DIR: &str = "sessions";
const REGISTER_FILE_MAX_BYTES: u64 = 1024 * 1024;
const REGISTER_GITIGNORE: &str = "*.tmp\n.*.tmp\n*.lock\n";
/// In `prim1`, ignored by Git (`*.lock`). An OS lock: a writer that dies
/// never leaves it held.
const REGISTER_WRITER_LOCK: &str = ".writer.lock";
const REGISTER_GITATTRIBUTES: &str = "* text eol=lf\n";
pub(crate) const REGISTER_DISAGREES: &str = "register_disagrees";
pub(crate) const REGISTER_DUPLICATE: &str = "register_duplicate";
/// Changes kept for a file that cannot be written yet; older ones are dropped.
const RETAINED_OPS_MAX: usize = 256;
const COMMIT_LABELS_MAX: usize = 5;
const GIT_OUTPUT_MAX_BYTES: u64 = 256 * 1024;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const ENV_DECIDES: &str = "PRIM1_PERSONAL_REPO is set for this PRIM-1, so it decides the personal repository; remove it and restart to choose one here.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PersonalRepoSource {
    /// `PRIM1_PERSONAL_REPO`.
    Environment,
    /// Chosen in Settings.
    Setting,
    /// Nothing chosen: the workspace.
    Default,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PersonalRepoHealth {
    /// The register is written here.
    On,
    /// No register by design: `off`, a held preview, or no workspace.
    Off,
    /// The choice cannot be used; no register until it is fixed.
    Error,
}

/// What Settings shows about the personal repository.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PersonalRepoStatus {
    /// The folder named, also when it cannot be used.
    pub folder: Option<String>,
    pub source: PersonalRepoSource,
    pub status: PersonalRepoHealth,
    /// Why it is off or unusable, or the writer's latest report (no Git, a
    /// file another install holds), in words for Settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PersonalRepo {
    /// Set only when the register can be written there.
    pub(crate) folder: Option<PathBuf>,
    named: Option<PathBuf>,
    source: PersonalRepoSource,
    health: PersonalRepoHealth,
    message: Option<String>,
}

impl PersonalRepo {
    fn at(source: PersonalRepoSource, folder: &Path) -> Self {
        match existing_folder(folder) {
            Ok(canonical) => Self {
                folder: Some(canonical.clone()),
                named: Some(canonical),
                source,
                health: PersonalRepoHealth::On,
                message: None,
            },
            Err(message) => Self {
                folder: None,
                named: Some(folder.to_path_buf()),
                source,
                health: PersonalRepoHealth::Error,
                message: Some(format!("{message} PRIM-1 keeps no session register until this is fixed.")),
            },
        }
    }

    fn not_written(source: PersonalRepoSource, health: PersonalRepoHealth, message: String) -> Self {
        Self { folder: None, named: None, source, health, message: Some(message) }
    }
}

/// `PRIM1_PERSONAL_REPO` > the Settings choice > the workspace > none. A
/// choice that cannot be used is reported, never replaced by the next one:
/// a typo in a QA override must not fall through to Victor's register.
pub(crate) fn resolve_personal_repo(env: Option<&OsStr>, runtime_dir: &Path, workspace: &Path) -> PersonalRepo {
    use PersonalRepoHealth::{Error, Off};
    use PersonalRepoSource::{Default, Environment, Setting};
    if let Some(value) = env_override(env) {
        return match value {
            Err(message) => PersonalRepo::not_written(Environment, Error, message),
            Ok(None) => PersonalRepo::not_written(
                Environment,
                Off,
                "PRIM1_PERSONAL_REPO is off: this PRIM-1 keeps no session register.".into(),
            ),
            Ok(Some(folder)) => PersonalRepo::at(Environment, &folder),
        };
    }
    match read_personal_repo_setting(runtime_dir) {
        Err(message) => PersonalRepo::not_written(Setting, Error, message),
        Ok(Some(folder)) => PersonalRepo::at(Setting, &folder),
        Ok(None) if workspace.is_dir() => PersonalRepo::at(Default, workspace),
        Ok(None) => PersonalRepo::not_written(
            Default,
            Off,
            format!(
                "The workspace {} is missing, so there is no personal repository and no session register. Choose a folder below.",
                workspace.display()
            ),
        ),
    }
}

/// None when unset or blank; Ok(None) for `off`.
fn env_override(env: Option<&OsStr>) -> Option<std::result::Result<Option<PathBuf>, String>> {
    let value = env?;
    let Some(text) = value.to_str() else {
        return Some(Err("PRIM1_PERSONAL_REPO is not valid Unicode; PRIM-1 keeps no session register until it is fixed.".into()));
    };
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.eq_ignore_ascii_case("off") {
        return Some(Ok(None));
    }
    let folder = PathBuf::from(text);
    if text.contains('\0') || !folder.is_absolute() {
        return Some(Err(format!(
            "PRIM1_PERSONAL_REPO must be off or the full path of a folder (not {}); PRIM-1 keeps no session register until it is fixed.",
            text.replace('\0', "")
        )));
    }
    Some(Ok(Some(folder)))
}

/// Its canonical path without `\\?\`. The folder must already exist: a
/// personal repository is never created by a typo.
fn existing_folder(folder: &Path) -> std::result::Result<PathBuf, String> {
    match fs::metadata(folder) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err(format!("{} is a file, not a folder.", folder.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("The folder {} does not exist.", folder.display()));
        }
        Err(error) => return Err(format!("The folder {} cannot be read ({error}).", folder.display())),
    }
    fs::canonicalize(folder)
        .map(|canonical| child_process_path(&canonical))
        .map_err(|error| format!("The folder {} cannot be resolved ({error}).", folder.display()))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersonalRepoSetting {
    schema_version: u32,
    folder: String,
}

/// None when nothing was saved.
pub(crate) fn read_personal_repo_setting(runtime_dir: &Path) -> std::result::Result<Option<PathBuf>, String> {
    let path = runtime_dir.join(PERSONAL_REPO_SETTING_FILE_NAME);
    let unusable = |detail: String| {
        format!(
            "The saved personal repository choice ({}) cannot be used: {detail}. Choose a folder in PRIM-1 Settings → Personal repository to replace it.",
            path.display()
        )
    };
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unusable(error.to_string())),
    };
    let mut bytes = Vec::new();
    file.take(SETTING_MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|error| unusable(error.to_string()))?;
    if bytes.len() as u64 > SETTING_MAX_BYTES {
        return Err(unusable("the file is larger than 64 KiB".into()));
    }
    let setting: PersonalRepoSetting = serde_json::from_slice(&bytes).map_err(|error| unusable(error.to_string()))?;
    if setting.schema_version != PERSONAL_REPO_SETTING_SCHEMA_VERSION {
        return Err(unusable(format!("unsupported schema version {}", setting.schema_version)));
    }
    let folder = PathBuf::from(&setting.folder);
    if setting.folder.trim() != setting.folder || setting.folder.contains('\0') || !folder.is_absolute() {
        return Err(unusable("the folder is not a full path".into()));
    }
    Ok(Some(folder))
}

fn persist_personal_repo_setting(runtime_dir: &Path, folder: &Path) -> std::result::Result<(), String> {
    let folder = folder.to_str().ok_or(
        "That folder path cannot be saved because it is not valid Unicode; the previous personal repository is still in use.",
    )?;
    let setting = PersonalRepoSetting { schema_version: PERSONAL_REPO_SETTING_SCHEMA_VERSION, folder: folder.to_owned() };
    persist_catalog(runtime_dir, PERSONAL_REPO_SETTING_FILE_NAME, "personal repository setting", &setting).map_err(
        |error| {
            format!(
                "The personal repository setting could not be saved ({error:#}); the previous one is still in use."
            )
        },
    )
}

/// Read, or created once (temp file then rename). An unreadable id is
/// reported, never replaced: a new id would orphan every file this install holds.
pub(crate) fn load_or_create_install_id(runtime_dir: &Path) -> std::result::Result<String, String> {
    let path = runtime_dir.join(INSTALL_ID_FILE_NAME);
    let unusable = |detail: String| {
        format!("This PRIM-1's install id ({}) cannot be used: {detail}. PRIM-1 keeps no session register until it is fixed.", path.display())
    };
    match File::open(&path) {
        Ok(file) => {
            let mut text = String::new();
            file.take(1024).read_to_string(&mut text).map_err(|error| unusable(error.to_string()))?;
            let id = Uuid::parse_str(text.trim()).map_err(|error| unusable(error.to_string()))?;
            if id.is_nil() {
                return Err(unusable("the id is nil".into()));
            }
            Ok(id.to_string())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let id = Uuid::new_v4().to_string();
            write_atomically(runtime_dir, INSTALL_ID_FILE_NAME, format!("{id}\n").as_bytes(), "install id")
                .map_err(|error| unusable(format!("{error:#}")))?;
            Ok(id)
        }
        Err(error) => Err(unusable(error.to_string())),
    }
}

fn write_atomically(dir: &Path, file_name: &str, bytes: &[u8], subject: &str) -> Result<()> {
    let destination = dir.join(file_name);
    let temporary = dir.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("failed to create temporary {subject} {}", temporary.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace_catalog_file(&temporary, &destination, subject)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// The catalog fields the register follows, for Claude, Codex and Grok rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LedgerRow {
    session_id: SessionId,
    label: String,
    driver: DriverKind,
    working_dir: String,
    harness_session_id: Option<String>,
    /// Provenance of the current hold, if any.
    hold: Option<String>,
    running_at_shutdown: bool,
    /// A verified Codex report: (thread, rollout), for the transcript hint.
    codex_rollout: Option<(String, PathBuf)>,
}

pub(crate) fn ledger_rows(sessions: &[PersistedSessionV1]) -> Vec<LedgerRow> {
    sessions
        .iter()
        .filter(|row| matches!(row.driver, DriverKind::Claude | DriverKind::Codex | DriverKind::Grok))
        .map(|row| LedgerRow {
            session_id: row.session_id,
            label: row.label.clone(),
            driver: row.driver,
            working_dir: row.working_directory.canonical_path.clone(),
            harness_session_id: row.harness_session_id.clone(),
            hold: row.recovery.as_ref().map(|recovery| recovery.provenance.clone()),
            running_at_shutdown: row.running_at_shutdown,
            codex_rollout: row
                .recovery
                .as_ref()
                .and_then(|recovery| recovery.reported_codex.as_ref())
                .map(|evidence| (evidence.thread_id.clone(), evidence.rollout_path.clone())),
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LedgerRoom {
    id: RoomId,
    label: String,
    members: Vec<SessionId>,
}

pub(crate) fn ledger_rooms(rooms: &[rooms::PersistedRoomV1]) -> Vec<LedgerRoom> {
    rooms
        .iter()
        .map(|room| LedgerRoom { id: room.room_id, label: room.label.clone(), members: room.member_ids.clone() })
        .collect()
}

/// What the writer last wrote, or was about to write, to one member's file:
/// the catalog's watermark for that file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegisterAck {
    pub(crate) revision: u64,
    pub(crate) conversation: Option<String>,
}

/// The catalog key for a personal repository's watermarks.
pub(crate) fn repo_key(repo: &Path) -> String {
    comparable_storage_path(repo).to_string_lossy().into_owned()
}

impl RegisterAck {
    /// The file is this writer's: at the watermark, or behind it (a write
    /// that never landed, or an older version Git put back).
    fn covers(&self, record: &RegisterRecordV1) -> bool {
        let conversation = record.conversation.as_ref().map(|conversation| conversation_key(&conversation.id));
        record.revision < self.revision
            || (record.revision == self.revision && conversation == self.conversation.as_deref().map(conversation_key))
    }
}

/// `<repo>\prim1\sessions\<prim-session-uuid>.json`, schema v1. Written with
/// every object's keys sorted. Fields a later build adds are kept as they are
/// (`extra`); only a breaking change raises `schema`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RegisterRecordV1 {
    pub(crate) conversation: Option<RegisterConversation>,
    pub(crate) harness: DriverKind,
    /// Append-only: one entry per conversation that became current. Only the
    /// open entry's `to` is ever set, once, when its successor arrives.
    pub(crate) history: Vec<RegisterHistoryEntry>,
    pub(crate) holder: RegisterHolder,
    pub(crate) label: String,
    pub(crate) prim_session_id: SessionId,
    /// One above the revision on disk at every write.
    pub(crate) revision: u64,
    pub(crate) room: Option<RegisterRoom>,
    pub(crate) schema: u32,
    /// Display only, never identity.
    pub(crate) titles: RegisterTitles,
    pub(crate) transcript_hint: Option<TranscriptHint>,
    pub(crate) updated_at: String,
    pub(crate) working_dir: String,
    #[serde(flatten)]
    pub(crate) extra: Extra,
}

/// Fields this build does not know, kept and written back unchanged.
pub(crate) type Extra = BTreeMap<String, serde_json::Value>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RegisterConversation {
    pub(crate) confirmed_at: String,
    pub(crate) id: String,
    pub(crate) since: String,
    pub(crate) via: String,
    #[serde(flatten)]
    pub(crate) extra: Extra,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RegisterHistoryEntry {
    pub(crate) from: String,
    pub(crate) id: String,
    pub(crate) reason: String,
    pub(crate) run_id: Option<Uuid>,
    pub(crate) to: Option<String>,
    pub(crate) via: String,
    #[serde(flatten)]
    pub(crate) extra: Extra,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RegisterHolder {
    pub(crate) install_id: String,
    pub(crate) machine: String,
    pub(crate) pid: Option<u32>,
    pub(crate) proc_start: Option<String>,
    pub(crate) run_id: Option<Uuid>,
    pub(crate) since: String,
    /// running | stopped | retired (the member was deleted; files are kept).
    pub(crate) state: String,
    #[serde(flatten)]
    pub(crate) extra: Extra,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RegisterRoom {
    pub(crate) id: RoomId,
    pub(crate) label: String,
    #[serde(flatten)]
    pub(crate) extra: Extra,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RegisterTitles {
    pub(crate) current: Option<String>,
    pub(crate) source: Option<String>,
    #[serde(flatten)]
    pub(crate) extra: Extra,
}

/// Where the harness keeps the transcript, relative to its data folder. The
/// transcript itself never enters the register.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TranscriptHint {
    pub(crate) home: String,
    pub(crate) relative: String,
    #[serde(flatten)]
    pub(crate) extra: Extra,
}

impl RegisterRecordV1 {
    fn new(row: &LedgerRow, install_id: &str, machine: &str, at: &str) -> Self {
        Self {
            conversation: None,
            harness: row.driver,
            history: Vec::new(),
            holder: RegisterHolder {
                install_id: install_id.into(),
                machine: machine.into(),
                pid: None,
                proc_start: None,
                run_id: None,
                since: at.into(),
                state: "stopped".into(),
                extra: Extra::new(),
            },
            label: row.label.clone(),
            prim_session_id: row.session_id,
            revision: 0,
            room: None,
            schema: REGISTER_SCHEMA,
            titles: RegisterTitles::default(),
            transcript_hint: None,
            updated_at: at.into(),
            working_dir: row.working_dir.clone(),
            extra: Extra::new(),
        }
    }

    /// `confirm` refreshes `confirmed_at` when the id is already current.
    fn apply_conversation(&mut self, id: Option<&str>, change: &ConversationOp, at: &str) {
        let current = self.conversation.as_ref().map(|conversation| conversation_key(&conversation.id));
        match id {
            Some(id) if current.as_deref() == Some(conversation_key(id).as_str()) => {
                if change.confirm
                    && let Some(conversation) = self.conversation.as_mut()
                {
                    conversation.confirmed_at = at.into();
                }
            }
            Some(id) => {
                self.close_open_entry(at);
                self.conversation = Some(RegisterConversation {
                    confirmed_at: at.into(),
                    id: id.into(),
                    since: at.into(),
                    via: change.via.into(),
                    extra: Extra::new(),
                });
                self.history.push(RegisterHistoryEntry {
                    from: at.into(),
                    id: id.into(),
                    reason: change.reason.into(),
                    run_id: change.run_id,
                    to: None,
                    via: change.via.into(),
                    extra: Extra::new(),
                });
            }
            None if self.conversation.is_some() => {
                self.close_open_entry(at);
                self.conversation = None;
            }
            None => {}
        }
    }

    fn close_open_entry(&mut self, at: &str) {
        if let Some(open) = self.history.last_mut()
            && open.to.is_none()
        {
            open.to = Some(at.into());
        }
    }

    fn same_content(&self, other: &Self) -> bool {
        let neutral = |record: &Self| Self { revision: 0, updated_at: String::new(), ..record.clone() };
        neutral(self) == neutral(other)
    }
}

pub(crate) fn register_path(repo: &Path, session_id: SessionId) -> PathBuf {
    repo.join(REGISTER_DIR).join(REGISTER_SESSIONS_DIR).join(format!("{session_id}.json"))
}

/// None when absent. A file that is too large, unparseable or of another
/// schema is an error: it is reported and never overwritten.
pub(crate) fn read_register_file(path: &Path) -> std::result::Result<Option<RegisterRecordV1>, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("The session register file {} cannot be read ({error}).", path.display())),
    };
    let mut bytes = Vec::new();
    file.take(REGISTER_FILE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("The session register file {} cannot be read ({error}).", path.display()))?;
    if bytes.len() as u64 > REGISTER_FILE_MAX_BYTES {
        return Err(format!("The session register file {} is larger than 1 MiB; it is left as it is.", path.display()));
    }
    let record: RegisterRecordV1 = serde_json::from_slice(&bytes).map_err(|error| {
        format!("The session register file {} is not a register v1 file ({error}); it is left as it is.", path.display())
    })?;
    if record.schema != REGISTER_SCHEMA {
        return Err(format!(
            "The session register file {} has schema {}; this PRIM-1 writes schema {REGISTER_SCHEMA} and leaves it as it is.",
            path.display(),
            record.schema
        ));
    }
    Ok(Some(record))
}

fn serialize_register(record: &RegisterRecordV1) -> Vec<u8> {
    let value = sorted_json(serde_json::to_value(record).expect("register records always serialize"));
    let mut bytes = serde_json::to_vec_pretty(&value).expect("register records always serialize");
    bytes.push(b'\n');
    bytes
}

/// Every object's keys in order, kept fields and declared ones alike.
fn sorted_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(String, serde_json::Value)> = map.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            serde_json::Value::Object(entries.into_iter().map(|(key, value)| (key, sorted_json(value))).collect())
        }
        serde_json::Value::Array(items) => serde_json::Value::Array(items.into_iter().map(sorted_json).collect()),
        other => other,
    }
}

/// UUID spelling normalized for comparison only.
fn conversation_key(id: &str) -> String {
    Uuid::parse_str(id).map(|id| id.to_string()).unwrap_or_else(|_| id.to_owned())
}

fn register_now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn machine_name() -> String {
    ["COMPUTERNAME", "HOSTNAME"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|name| name.trim().to_owned())
        .find(|name| !name.is_empty() && !name.chars().any(char::is_control))
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(windows)]
fn process_start_time(pid: u32) -> Option<String> {
    // FILETIME: 100 ns ticks since 1601-01-01 UTC.
    const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
    let ticks = PinnedProcess::open(pid).ok()?.creation_time_filetime().checked_sub(UNIX_EPOCH_TICKS)?;
    let at = DateTime::<Utc>::from_timestamp((ticks / 10_000_000) as i64, ((ticks % 10_000_000) * 100) as u32)?;
    Some(at.to_rfc3339_opts(SecondsFormat::Millis, true))
}

#[cfg(not(windows))]
fn process_start_time(_pid: u32) -> Option<String> {
    None
}

fn safe_conversation_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Claude: `projects\<cwd with every non-alphanumeric as '-'>\<id>.jsonl`.
/// Longer project names are hashed by Claude, so no hint is given for them.
fn claude_project_dir(working_dir: &str) -> Option<String> {
    let name: String = working_dir
        .chars()
        .flat_map(|c| {
            let dashes = if c.is_ascii_alphanumeric() { 0 } else { c.len_utf16() };
            std::iter::repeat_n('-', dashes).chain(c.is_ascii_alphanumeric().then_some(c))
        })
        .collect();
    (!name.is_empty() && name.len() <= 200).then_some(name)
}

/// Grok: `sessions\<percent-encoded cwd>\<id>`; unreserved characters stay.
fn grok_session_dir(working_dir: &str) -> String {
    working_dir
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// Codex: the reported rollout, `sessions\YYYY\MM\DD\rollout-…-<id>.jsonl`.
fn codex_relative(rollout: &Path) -> Option<String> {
    let parts: Vec<String> = child_process_path(rollout)
        .components()
        .filter_map(|part| match part {
            std::path::Component::Normal(part) => part.to_str().map(str::to_owned),
            _ => None,
        })
        .collect();
    let tail = parts.get(parts.len().checked_sub(5)?..)?;
    let file = &tail[4];
    (tail[0] == "sessions" && file.starts_with("rollout-") && file.ends_with(".jsonl")).then(|| tail.join("/"))
}

pub(crate) fn transcript_hint(row: &LedgerRow, conversation: Option<&str>) -> Option<TranscriptHint> {
    let id = conversation.filter(|id| safe_conversation_id(id))?;
    let (home, relative) = match row.driver {
        DriverKind::Claude => ("~/.claude", format!("projects/{}/{id}.jsonl", claude_project_dir(&row.working_dir)?)),
        DriverKind::Grok => ("~/.grok", format!("sessions/{}/{id}", grok_session_dir(&row.working_dir))),
        DriverKind::Codex => {
            let (thread, rollout) = row.codex_rollout.as_ref()?;
            if conversation_key(thread) != conversation_key(id) {
                return None;
            }
            ("~/.codex", codex_relative(rollout)?)
        }
        DriverKind::Prime | DriverKind::GenericTerminal => return None,
    };
    Some(TranscriptHint { home: home.into(), relative, extra: Extra::new() })
}

/// Codex identity holds: lifted by proof of the exact thread, never guessed.
fn identity_hold(provenance: &str) -> bool {
    matches!(provenance, "legacy_catalog" | "codex_launch" | "job_attested_codex_shell_report")
}

/// The holds the register itself places at start.
pub(crate) fn register_hold(provenance: &str) -> bool {
    provenance == REGISTER_DISAGREES || provenance == REGISTER_DUPLICATE
}

/// A hold that doubts the catalog's conversation: the register keeps its own
/// until a proven change replaces it. Under any other hold (an in-use copy, a
/// Codex identity hold) the register follows the catalog, as it does live.
fn keeps_register_value(hold: Option<&str>) -> bool {
    matches!(hold, Some(REGISTER_DISAGREES | "duplicate_catalog_identity"))
}

/// Replaces an earlier register hold; any other hold it covers is retained.
fn apply_register_hold(slot: &mut SessionSlot, hold: RecoveryReference) {
    match slot.recovery.take() {
        Some(previous) if register_hold(&previous.provenance) || previous == hold => {}
        Some(previous) => {
            if !slot.recovery_history.contains(&previous) {
                slot.recovery_history.push(previous);
            }
        }
        None => {}
    }
    slot.recovery = Some(hold);
}

/// The register agrees again. Claude and Grok are released; Codex returns to
/// the identity hold for the catalog's thread (the one the hold covered when
/// it is retained), which still needs the thread's rollout to resume.
fn lift_register_hold(slot: &mut SessionSlot, driver: DriverKind, catalog: Option<&str>) {
    let Some(lifted) = slot.recovery.take() else {
        return;
    };
    let base = match (driver, catalog) {
        (DriverKind::Codex, Some(catalog)) => Some(
            slot.recovery_history
                .iter()
                .rev()
                .find(|recovery| {
                    identity_hold(&recovery.provenance)
                        && recovery
                            .reported_codex
                            .as_ref()
                            .map(|evidence| evidence.thread_id.as_str())
                            .or(recovery.original_id.as_deref())
                            .is_some_and(|thread| conversation_key(thread) == conversation_key(catalog))
                })
                .cloned()
                .unwrap_or_else(|| RecoveryReference::unresolved_codex(Some(catalog.into()), true)),
        ),
        _ => None,
    };
    if !slot.recovery_history.contains(&lifted) {
        slot.recovery_history.push(lifted);
    }
    slot.recovery = base;
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct LedgerTiming {
    /// A burst is written once this long passes without another message…
    quiet: Duration,
    /// …or once it is this old.
    batch_max: Duration,
    /// The commit waits this long after the last write.
    commit_quiet: Duration,
    /// Retry after a held `index.lock`; doubled per other failure.
    commit_retry: Duration,
    commit_backoff_max: Duration,
    /// Waiting for one git command; a git still running is left alone (never
    /// killed: that could leave `index.lock` behind) and no other starts.
    git_wait: Duration,
}

impl LedgerTiming {
    #[cfg(not(test))]
    const DEFAULT: Self = Self {
        quiet: Duration::from_millis(500),
        batch_max: Duration::from_secs(5),
        commit_quiet: Duration::from_secs(20),
        commit_retry: Duration::from_secs(20),
        commit_backoff_max: Duration::from_secs(600),
        git_wait: Duration::from_secs(30),
    };
    #[cfg(test)]
    const DEFAULT: Self = Self {
        quiet: Duration::from_millis(20),
        batch_max: Duration::from_millis(200),
        commit_quiet: Duration::from_millis(150),
        commit_retry: Duration::from_millis(100),
        commit_backoff_max: Duration::from_millis(400),
        git_wait: Duration::from_secs(30),
    };
}

/// The writer's answer to a flush or a close, counted after its pass. Git is
/// never needed for a file to count as written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LedgerReport {
    /// Members whose changes are not in their file: kept and tried again,
    /// or after a close brought up to the catalog by the next start.
    pub(crate) unwritten_members: usize,
    /// Files written and owed a commit (Git busy or failing, or a close,
    /// which starts no git); the next start commits them.
    pub(crate) uncommitted_files: usize,
    /// Close only: a git started earlier was still running when the close
    /// budget ran out. PRIM-1 never kills it; the desktop's job may still end
    /// it when the app exits.
    pub(crate) git_running: bool,
}

/// The writer stops waiting for a running git at close this long before the
/// caller stops waiting for its answer.
const CLOSE_ANSWER_MARGIN: Duration = Duration::from_millis(500);

enum LedgerMessage {
    Catalog(Vec<LedgerRow>),
    Rooms(Vec<LedgerRoom>),
    Change(ConversationChange, String),
    Title { session_id: SessionId, title: String, source: &'static str },
    /// The member's slot got or lost its running process: its holder is read
    /// again from the slot, also when no conversation, label or room changed
    /// (a resume, a restart, an exit).
    Run(SessionId),
    Repo(Option<PathBuf>),
    /// Write now, and commit when `commit`; `refresh` re-reads every
    /// member's run state.
    Flush { refresh: bool, commit: bool, done: mpsc::SyncSender<LedgerReport> },
    /// Shutdown: write what it can, each member's run state included, start
    /// no git, give a git still running until `git_until`, and stop.
    Close { git_until: Instant, done: mpsc::SyncSender<LedgerReport> },
}

/// The shutdown's one log line about the register, from its close answer
/// (None: it did not answer within `timeout`).
pub(crate) fn close_log(report: Option<LedgerReport>, timeout: Duration) -> (LogLevel, String) {
    let Some(report) = report else {
        return (
            LogLevel::Warn,
            format!(
                "The session register writer did not answer within {} s at close; the next start brings its files up to the session catalog and commits what is owed.",
                timeout.as_secs()
            ),
        );
    };
    let written = match report.unwritten_members {
        0 => "every change written".to_owned(),
        members => format!("{members} members not written (the next start brings them up to the session catalog)"),
    };
    let mut message =
        format!("Session register at close: {written}; {} files owed a commit at the next start", report.uncommitted_files);
    if report.git_running {
        message.push_str("; a git was still running at close and is left to finish");
    }
    message.push('.');
    let level = if report.unwritten_members > 0 || report.git_running { LogLevel::Warn } else { LogLevel::Info };
    (level, message)
}

#[cfg(not(test))]
fn personal_repo_env() -> Option<OsString> {
    std::env::var_os(PERSONAL_REPO_ENV)
}

#[cfg(test)]
thread_local! {
    /// Unit tests never read the process environment; the register is off
    /// unless a test names a temporary repository.
    static PERSONAL_REPO_ENV_FOR_TESTS: std::cell::RefCell<Option<OsString>> =
        std::cell::RefCell::new(Some("off".into()));
}

#[cfg(test)]
fn personal_repo_env() -> Option<OsString> {
    PERSONAL_REPO_ENV_FOR_TESTS.with(|value| value.borrow().clone())
}

/// For supervisors built on this thread afterwards; None means unset.
#[cfg(test)]
pub(crate) fn set_personal_repo_env_for_tests(value: Option<&OsStr>) {
    PERSONAL_REPO_ENV_FOR_TESTS.with(|current| *current.borrow_mut() = value.map(OsStr::to_os_string));
}

struct LedgerShared {
    /// `PRIM1_PERSONAL_REPO` as read at start.
    env: Option<OsString>,
    trial: bool,
    repo: PersonalRepo,
    /// Read or created only once the register is on: a runtime with the
    /// register off keeps no `install-id`.
    install_id: Option<std::result::Result<String, String>>,
    sender: Option<mpsc::Sender<LedgerMessage>>,
    timing: LedgerTiming,
    /// Set by the close: no writer starts again and nothing is written after it.
    closed: bool,
}

/// The register's handle in the supervisor. `shared` is a leaf lock held
/// only to read state or hand over a message, so it may be taken under the
/// slots, catalog or rooms locks.
pub(crate) struct SessionLedger {
    shared: Mutex<LedgerShared>,
    /// The writer's open reports for Settings, by key. Separate from
    /// `shared`, which owns the sender: the writer must not keep its own
    /// channel open.
    notes: Notes,
    select: Mutex<()>,
    /// Set by the close before it sends: the writer stops waiting for a git
    /// it is running and starts none, so the close is answered in time.
    closing: Arc<AtomicBool>,
}

impl SessionLedger {
    /// Resolves the personal repository, and this install's id when the
    /// register is on. A held trial reads and writes neither.
    pub(crate) fn open(runtime_dir: &Path, workspace: &Path, trial: bool) -> Self {
        let env = personal_repo_env();
        let repo = if trial {
            PersonalRepo::not_written(
                PersonalRepoSource::Default,
                PersonalRepoHealth::Off,
                "This preview is held; it keeps no session register.".into(),
            )
        } else {
            resolve_personal_repo(env.as_deref(), runtime_dir, workspace)
        };
        let install_id = repo.folder.is_some().then(|| load_or_create_install_id(runtime_dir));
        Self {
            shared: Mutex::new(LedgerShared {
                env,
                trial,
                repo,
                install_id,
                sender: None,
                timing: LedgerTiming::DEFAULT,
                closed: false,
            }),
            notes: Notes::default(),
            select: Mutex::new(()),
            closing: Arc::new(AtomicBool::new(false)),
        }
    }

    fn writable(&self) -> Option<(PathBuf, String)> {
        let shared = self.shared.lock();
        if shared.trial {
            return None;
        }
        Some((shared.repo.folder.clone()?, shared.install_id.clone()?.ok()?))
    }

    /// At start, before anything can launch, each member's register file held
    /// by this install is compared with the catalog. A file at or behind the
    /// catalog's watermark is this install's own and only lags: the writer
    /// fills it in from the catalog. A file ahead of the watermark (or with
    /// none) that names another conversation holds the member as
    /// `register_disagrees`, both IDs shown; one conversation the register
    /// names for two members holds them as `register_duplicate`. A register
    /// hold whose cause is gone is lifted; no other hold is ever released or
    /// replaced (an in-use copy keeps Resume here): the register's differing
    /// conversation is only added to its recovery history. An unreadable file
    /// or one another install holds changes nothing here; the writer reports it.
    pub(crate) fn hold_register_disagreements(&self, slots: &mut SessionRegistry, catalog: &mut SessionCatalogV1) {
        struct Member {
            id: SessionId,
            driver: DriverKind,
            path: PathBuf,
            catalog: Option<String>,
            registered: Option<String>,
            disagrees: bool,
            /// Held by something else, which decides.
            other_hold: bool,
            /// What the register names once the writer's first pass is done.
            effective: Option<String>,
        }
        let Some((repo, install_id)) = self.writable() else {
            return;
        };
        let acks = catalog.register_acks.get(&repo_key(&repo)).cloned().unwrap_or_default();
        let mut members = Vec::new();
        for row in &catalog.sessions {
            if !matches!(row.driver, DriverKind::Claude | DriverKind::Codex | DriverKind::Grok) {
                continue;
            }
            let Some(slot) = slots.get_by_id(row.session_id) else {
                continue;
            };
            let path = register_path(&repo, row.session_id);
            let record = match read_register_file(&path) {
                Ok(Some(record))
                    if record.holder.install_id == install_id
                        && record.prim_session_id == row.session_id
                        && record.harness == row.driver =>
                {
                    Some(record)
                }
                Ok(None) => None,
                _ => continue,
            };
            let registered = record.as_ref().and_then(|record| record.conversation.as_ref()).map(|conversation| conversation.id.clone());
            let agrees = registered.as_deref().is_none_or(|registered| {
                row.harness_session_id.as_deref().map(conversation_key) == Some(conversation_key(registered))
            });
            let own = record
                .as_ref()
                .is_none_or(|record| acks.get(&row.session_id).is_some_and(|ack| ack.covers(record)));
            let current = slot.recovery.as_ref().map(|recovery| recovery.provenance.as_str());
            let other_hold = current.is_some_and(|provenance| !identity_hold(provenance) && !register_hold(provenance));
            // A disagreement stays until the IDs agree (the writer keeps the
            // register's value meanwhile); a lagging own file never makes one.
            let disagrees = !agrees && !other_hold && (!own || current == Some(REGISTER_DISAGREES));
            // As the writer's first pass leaves it: it keeps the register's
            // value only for a file changed outside, or under a hold that
            // doubts the catalog.
            let final_hold = if disagrees { Some(REGISTER_DISAGREES) } else if other_hold { current } else { None };
            let keeps = !agrees && (!own || keeps_register_value(final_hold));
            let effective = if keeps { registered.clone() } else { row.harness_session_id.clone() };
            members.push(Member {
                id: row.session_id,
                driver: row.driver,
                path,
                catalog: row.harness_session_id.clone(),
                registered,
                disagrees,
                other_hold,
                effective,
            });
        }
        let mut named: HashMap<(DriverKind, String), Vec<SessionId>> = HashMap::new();
        for member in &members {
            if let Some(effective) = &member.effective {
                named.entry((member.driver, conversation_key(effective))).or_default().push(member.id);
            }
        }
        let mut changed = HashSet::new();
        for member in &members {
            let Some(slot) = slots.get_by_id_mut(member.id) else {
                continue;
            };
            if member.other_hold {
                // That hold stays active. A differing register conversation
                // is kept in sight, never chosen.
                if let Some(registered) = &member.registered
                    && member.catalog.as_deref().map(conversation_key) != Some(conversation_key(registered))
                {
                    let seen = RecoveryReference::register_disagrees(member.catalog.clone(), registered.clone(), &member.path);
                    if !slot.recovery_history.contains(&seen) {
                        slot.recovery_history.push(seen);
                        changed.insert(member.id);
                    }
                }
                continue;
            }
            let others = member
                .effective
                .as_ref()
                .and_then(|effective| named.get(&(member.driver, conversation_key(effective))))
                .filter(|owners| owners.len() > 1)
                .map(|owners| owners.iter().copied().filter(|id| *id != member.id).collect::<Vec<_>>());
            let hold = match (&member.registered, others) {
                (Some(registered), _) if member.disagrees => {
                    Some(RecoveryReference::register_disagrees(member.catalog.clone(), registered.clone(), &member.path))
                }
                (_, Some(others)) => member
                    .effective
                    .clone()
                    .map(|effective| RecoveryReference::register_duplicate(effective, &others, &member.path)),
                _ => None,
            };
            let before = (slot.recovery.clone(), slot.recovery_history.len());
            match hold {
                Some(hold) => apply_register_hold(slot, hold),
                None if slot.recovery.as_ref().is_some_and(|recovery| register_hold(&recovery.provenance)) => {
                    lift_register_hold(slot, member.driver, member.catalog.as_deref());
                }
                None => {}
            }
            if (slot.recovery.clone(), slot.recovery_history.len()) != before {
                changed.insert(member.id);
            }
        }
        // In the baseline; the next catalog write persists it.
        for row in &mut catalog.sessions {
            if changed.contains(&row.session_id)
                && let Some(slot) = slots.get_by_id(row.session_id)
            {
                row.recovery = slot.recovery.clone();
                row.recovery_history = slot.recovery_history.clone();
            }
        }
    }

    /// Starts the writer when the register is on, never after the close.
    /// Idempotent.
    fn start(&self, inner: Weak<SupervisorInner>, rows: Vec<LedgerRow>, rooms: Vec<LedgerRoom>) {
        let mut shared = self.shared.lock();
        if shared.trial || shared.closed || shared.sender.is_some() {
            return;
        }
        let (Some(repo), Some(Ok(install_id))) = (shared.repo.folder.clone(), shared.install_id.clone()) else {
            return;
        };
        let (sender, receiver) = mpsc::channel();
        let writer =
            Writer::new(repo, install_id, inner, shared.timing, Arc::clone(&self.notes), Arc::clone(&self.closing), rows, rooms);
        match thread::Builder::new().name("prim1-session-register".into()).spawn(move || writer.run(receiver)) {
            Ok(_) => shared.sender = Some(sender),
            Err(error) => {
                self.notes.lock().insert("writer".into(), format!("The session register writer could not start ({error})."));
            }
        }
    }

    fn send(&self, message: impl FnOnce() -> LedgerMessage) {
        let shared = self.shared.lock();
        if let Some(sender) = &shared.sender {
            let _ = sender.send(message());
        }
    }

    /// After a successful session catalog write.
    pub(crate) fn feed_catalog(&self, sessions: &[PersistedSessionV1]) {
        self.send(|| LedgerMessage::Catalog(ledger_rows(sessions)));
    }

    /// After a successful room catalog write.
    pub(crate) fn feed_rooms(&self, rooms: &[rooms::PersistedRoomV1]) {
        self.send(|| LedgerMessage::Rooms(ledger_rooms(rooms)));
    }

    /// Waits up to `timeout` for the writer to write (and commit) what it
    /// has. None when it did not answer in time or is not running.
    #[cfg_attr(not(test), allow(dead_code))]
    fn flush(&self, refresh: bool, commit: bool, timeout: Duration) -> Option<LedgerReport> {
        let (done, finished) = mpsc::sync_channel(1);
        let sent = {
            let shared = self.shared.lock();
            shared
                .sender
                .as_ref()
                .is_some_and(|sender| sender.send(LedgerMessage::Flush { refresh, commit, done }).is_ok())
        };
        if sent { finished.recv_timeout(timeout).ok() } else { None }
    }

    /// Hands the writer its last message: nothing is written after it, and
    /// the register never starts again in this process. None when it did
    /// not answer in time or is not running.
    fn close(&self, timeout: Duration) -> Option<LedgerReport> {
        let (done, finished) = mpsc::sync_channel(1);
        let answer_by = Instant::now() + timeout;
        let git_until = Instant::now() + timeout.saturating_sub(CLOSE_ANSWER_MARGIN);
        let sender = {
            let mut shared = self.shared.lock();
            shared.closed = true;
            shared.sender.take()
        };
        // Before the message: a writer waiting for a git sees it and comes
        // to the close, which waits for that git within the budget.
        self.closing.store(true, Ordering::Release);
        sender?.send(LedgerMessage::Close { git_until, done }).ok()?;
        finished.recv_timeout(answer_by.saturating_duration_since(Instant::now())).ok()
    }

    pub(crate) fn folder(&self) -> Option<PathBuf> {
        self.shared.lock().repo.folder.clone()
    }

    fn status(&self) -> PersonalRepoStatus {
        let note = notes_text(&self.notes.lock());
        let shared = self.shared.lock();
        let mut status = PersonalRepoStatus {
            folder: shared.repo.named.as_ref().map(|folder| folder.display().to_string()),
            source: shared.repo.source,
            status: shared.repo.health,
            message: shared.repo.message.clone(),
        };
        if status.status == PersonalRepoHealth::On {
            match &shared.install_id {
                Some(Err(message)) => {
                    status.status = PersonalRepoHealth::Error;
                    status.message = Some(message.clone());
                }
                _ if shared.sender.is_none() => {
                    status.status = PersonalRepoHealth::Error;
                    status.message = Some(note.unwrap_or_else(|| "The session register writer is not running.".into()));
                }
                _ => status.message = note,
            }
        }
        status
    }
}

impl SupervisorHandle {
    /// Records a proven conversation change in the register. Never blocks the
    /// caller on file or Git I/O; safe under the slots, catalog or rooms locks.
    pub(crate) fn note_conversation_change(&self, change: ConversationChange) {
        self.inner.session_ledger.send(|| LedgerMessage::Change(change, register_now()));
    }

    /// The member's slot got or lost its running process (a run installed, a
    /// run withdrawn): the writer reads its holder again from the slot. Never
    /// blocks; safe under the slots, catalog or rooms locks.
    pub(crate) fn note_run_changed(&self, session_id: SessionId) {
        self.inner.session_ledger.send(|| LedgerMessage::Run(session_id));
    }

    /// A harness-side title for the register, display only. Never blocks.
    pub(crate) fn note_conversation_title(&self, session_id: SessionId, title: String, source: &'static str) {
        self.inner.session_ledger.send(|| LedgerMessage::Title { session_id, title, source });
    }

    /// Settings → Personal repository.
    pub fn personal_repo_status(&self) -> PersonalRepoStatus {
        self.inner.session_ledger.status()
    }

    /// The folder the register is written to, if any.
    pub(crate) fn personal_repo_folder(&self) -> Option<PathBuf> {
        self.inner.session_ledger.folder()
    }

    /// Writes, and commits when `commit`, what the writer holds. None when
    /// it did not answer in time or is not running.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn flush_session_ledger(&self, refresh: bool, commit: bool, timeout: Duration) -> Option<LedgerReport> {
        self.inner.session_ledger.flush(refresh, commit, timeout)
    }

    /// At shutdown the writer writes what it can, each member's stopped state
    /// included, and stops; the report says what it could not write. No git
    /// starts: the app's job ends its children, and a git ended mid-commit
    /// would leave `index.lock`; one already running gets the rest of
    /// `timeout`, also when the writer was waiting for it, and PRIM-1 never
    /// kills it (the desktop's job may still end a git that outlasts it when
    /// the app exits). The next start commits what is owed and brings files
    /// the close could not write up to the catalog.
    pub(crate) fn close_session_ledger(&self, timeout: Duration) -> Option<LedgerReport> {
        self.inner.session_ledger.close(timeout)
    }

    pub(super) fn start_session_ledger(&self, rows: Vec<LedgerRow>, rooms: Vec<LedgerRoom>) {
        self.inner.session_ledger.start(Arc::downgrade(&self.inner), rows, rooms);
    }

    /// The writer's watermarks for `repo`, from the catalog.
    fn register_acks(&self, repo: &Path) -> HashMap<SessionId, RegisterAck> {
        let catalog = self.inner.catalog.lock();
        catalog.register_acks.get(&repo_key(repo)).map(|acks| acks.iter().map(|(id, ack)| (*id, ack.clone())).collect()).unwrap_or_default()
    }

    /// Records, before the files are replaced, what the writer is about to
    /// write. Watermarks of deleted members are dropped. Called on the writer
    /// thread only, never under the slots, catalog or rooms locks.
    fn record_register_acks(&self, repo: &Path, acks: &[(SessionId, RegisterAck)]) -> Result<()> {
        let mut catalog = self.inner.catalog.lock();
        let mut candidate = catalog.clone();
        let members: HashSet<SessionId> = candidate.sessions.iter().map(|row| row.session_id).collect();
        let marks = candidate.register_acks.entry(repo_key(repo)).or_default();
        marks.retain(|id, _| members.contains(id));
        for (id, ack) in acks {
            if members.contains(id) {
                marks.insert(*id, ack.clone());
            }
        }
        if candidate.register_acks.get(&repo_key(repo)).is_some_and(BTreeMap::is_empty) {
            candidate.register_acks.remove(&repo_key(repo));
        }
        if candidate != *catalog {
            self.persist_catalog_candidate(&candidate)?;
            *catalog = candidate;
        }
        Ok(())
    }

    /// Settings → Personal repository. The folder must exist. Saves the choice
    /// and moves the register's writes there; files already written stay where
    /// they are. A task folder chosen by default follows from the next start.
    /// Refused while `PRIM1_PERSONAL_REPO` decides. Blocking file I/O.
    pub fn select_personal_repo(&self, requested: &str) -> std::result::Result<PersonalRepoStatus, String> {
        self.select_personal_repo_checked(requested, None)
    }

    /// `check`: a member's runtime_settings set (A16), run under the session
    /// lock, which is then held through the saved choice and the register's
    /// switch (sessions before the register's own lock, as everywhere).
    pub(crate) fn select_personal_repo_checked(
        &self, requested: &str, check: Option<&dyn Fn(&SessionRegistry) -> anyhow::Result<()>>,
    ) -> std::result::Result<PersonalRepoStatus, String> {
        self.ensure_active().map_err(|error| error.to_string())?;
        let ledger = &self.inner.session_ledger;
        let _one_at_a_time = ledger.select.lock();
        let folder = existing_folder(&task_sideband::parse_folder_input(requested)?)?;
        let needs_install_id = {
            let shared = ledger.shared.lock();
            if shared.trial {
                return Err("This preview is held; it keeps no session register.".into());
            }
            if shared.closed {
                return Err("PRIM-1 is closing".into());
            }
            if env_override(shared.env.as_deref()).is_some() {
                return Err(ENV_DECIDES.into());
            }
            if let Some(Err(message)) = &shared.install_id {
                return Err(message.clone());
            }
            let current = shared.repo.folder.as_deref().map(comparable_storage_path);
            if shared.repo.source == PersonalRepoSource::Setting && current == Some(comparable_storage_path(&folder)) {
                drop(shared);
                return Ok(ledger.status());
            }
            shared.install_id.is_none()
        };
        // The register was off until now: this install gets its holder id.
        if needs_install_id {
            let id = load_or_create_install_id(&self.inner.runtime_dir)?;
            ledger.shared.lock().install_id = Some(Ok(id));
        }
        // Read before the grant is held: catalog and rooms are never taken
        // while the session lock is.
        let rows = ledger_rows(&self.inner.catalog.lock().sessions);
        let rooms = ledger_rooms(&self.inner.rooms.lock().catalog.rooms);
        let authority = task_sideband::authorize_selection(&self.inner.slots, check)?;
        persist_personal_repo_setting(&self.inner.runtime_dir, &folder)?;
        let running = {
            let mut shared = ledger.shared.lock();
            shared.repo = PersonalRepo::at(PersonalRepoSource::Setting, &folder);
            shared.sender.as_ref().map(|sender| sender.send(LedgerMessage::Repo(Some(folder.clone()))).is_ok())
        };
        drop(authority);
        ledger.notes.lock().clear();
        if running != Some(true) {
            ledger.shared.lock().sender = None;
            ledger.start(Arc::downgrade(&self.inner), rows, rooms);
        }
        Ok(ledger.status())
    }
}

#[derive(Clone, Debug)]
struct ConversationOp {
    reason: &'static str,
    via: &'static str,
    run_id: Option<Uuid>,
    confirm: bool,
}

#[derive(Clone, Debug)]
enum Op {
    Conversation { id: Option<String>, change: ConversationOp, at: String },
    /// The catalog's conversation, for a file that names none or that only
    /// lags behind this install's own writes.
    Seed { at: String },
    Title { title: String, source: &'static str },
    Retire,
}

#[derive(Clone, Copy, Debug, Default)]
struct LiveRun {
    running: bool,
    /// The process is still owned (or its termination is unproven) while
    /// the run is being stopped: the register keeps the prior identity and
    /// says "stopping" until the release is proved (Codex room 343).
    stopping: bool,
    run_id: Option<Uuid>,
    pid: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GitProbe {
    Unknown,
    Ready,
    NotRepository,
    Missing,
}

enum GitRun {
    Done { success: bool, stdout: String, stderr: String },
    Missing,
    /// A previous git is still running, this one outlived `git_wait` or was
    /// running when the close was requested, or the close was requested.
    Busy,
    Failed(String),
}

/// One member's file in a write pass.
enum Planned {
    /// Already right.
    Current(RegisterRecordV1),
    /// No such member.
    Nothing,
    /// Another install holds the file: its changes are dropped.
    Foreign,
    /// Not now: the changes are kept and tried again.
    Later,
    Write { record: RegisterRecordV1, on_disk: u64 },
}

struct Writer {
    repo: Option<PathBuf>,
    install_id: String,
    machine: String,
    inner: Weak<SupervisorInner>,
    timing: LedgerTiming,
    notes: Notes,
    rows: HashMap<SessionId, LedgerRow>,
    rooms: HashMap<SessionId, (RoomId, String)>,
    /// Last content written or read: carried over when a file disappears or
    /// the repository changes.
    records: HashMap<SessionId, RegisterRecordV1>,
    /// The latest pid a change reported for a run, when the slot has none.
    reported_pids: HashMap<SessionId, (Uuid, u32)>,
    /// The catalog's watermarks for `repo`.
    acks: HashMap<SessionId, RegisterAck>,
    /// Changes not yet written, oldest first. A failed write keeps them.
    pending: BTreeMap<SessionId, Vec<Op>>,
    write_retry_at: Option<Instant>,
    write_failures: u32,
    /// Repo-relative path → member label, written but not yet committed.
    uncommitted: BTreeMap<String, String>,
    last_write: Option<Instant>,
    retry_at: Option<Instant>,
    commit_failures: u32,
    git: GitProbe,
    /// The repository folder inside its work tree, as `git status` prints it.
    git_prefix: String,
    /// A git command that outlived `git_wait`, or was running when the close
    /// was requested; nothing else starts until it ends.
    git_child: Option<Child>,
    /// The close was requested: no git starts, and one running is kept in
    /// `git_child` for the close to wait for.
    closing: Arc<AtomicBool>,
    reported: HashSet<String>,
}

impl Writer {
    fn new(
        repo: PathBuf,
        install_id: String,
        inner: Weak<SupervisorInner>,
        timing: LedgerTiming,
        notes: Notes,
        closing: Arc<AtomicBool>,
        rows: Vec<LedgerRow>,
        rooms: Vec<LedgerRoom>,
    ) -> Self {
        Self {
            repo: Some(repo),
            install_id,
            machine: machine_name(),
            inner,
            timing,
            notes,
            rows: rows.into_iter().map(|row| (row.session_id, row)).collect(),
            rooms: room_map(rooms),
            records: HashMap::new(),
            reported_pids: HashMap::new(),
            acks: HashMap::new(),
            pending: BTreeMap::new(),
            write_retry_at: None,
            write_failures: 0,
            uncommitted: BTreeMap::new(),
            last_write: None,
            retry_at: None,
            commit_failures: 0,
            git: GitProbe::Unknown,
            git_prefix: String::new(),
            git_child: None,
            closing,
            reported: HashSet::new(),
        }
    }

    fn run(mut self, receiver: mpsc::Receiver<LedgerMessage>) {
        // The first pass writes what the catalog already says: files missing
        // or lagging since the last run, and run states a crash left behind.
        self.acks = self.load_acks();
        self.mark_all(true);
        self.write_pending();
        self.queue_owed();
        let mut batch = Vec::new();
        let mut batch_started: Option<Instant> = None;
        let mut last_message = Instant::now();
        loop {
            let batch_due = batch_started.map(|started| (last_message + self.timing.quiet).min(started + self.timing.batch_max));
            let wait = [batch_due, self.commit_due(), self.write_due()].into_iter().flatten().min();
            let received = match wait {
                Some(at) => receiver.recv_timeout(at.saturating_duration_since(Instant::now())),
                None => receiver.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
            };
            match received {
                Ok(LedgerMessage::Flush { refresh, commit, done }) => {
                    self.absorb(std::mem::take(&mut batch), false);
                    batch_started = None;
                    if refresh {
                        self.mark_all(false);
                    }
                    self.write_pending();
                    if commit {
                        self.commit(true);
                    }
                    let _ = done.send(self.answer());
                }
                Ok(LedgerMessage::Close { git_until, done }) => {
                    let report = self.close(std::mem::take(&mut batch), git_until);
                    let _ = done.send(report);
                    return;
                }
                Ok(message) => {
                    batch.push(message);
                    last_message = Instant::now();
                    batch_started.get_or_insert(last_message);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let now = Instant::now();
                    let batch_due = batch_started.map(|started| (last_message + self.timing.quiet).min(started + self.timing.batch_max));
                    if batch_due.is_some_and(|due| due <= now) {
                        self.absorb(std::mem::take(&mut batch), false);
                        batch_started = None;
                        self.write_pending();
                    } else if self.write_due().is_some_and(|due| due <= now) {
                        self.write_pending();
                    }
                    if self.commit_due().is_some_and(|due| due <= now) {
                        self.commit(false);
                    }
                }
                // Teardown writes but starts no git: PRIM-1's job ends its
                // children, and a git ended mid-commit leaves `index.lock`.
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.absorb(batch, true);
                    self.write_pending();
                    return;
                }
            }
        }
    }

    /// Shutdown: what is queued is taken in without starting git, every
    /// member's run state is written where it can be, and a git started
    /// earlier gets until `git_until` to end. PRIM-1 never kills it (that
    /// could leave `index.lock`); the desktop's job may still end it when the
    /// app exits.
    fn close(&mut self, batch: Vec<LedgerMessage>, git_until: Instant) -> LedgerReport {
        self.absorb(batch, true);
        self.mark_all(false);
        self.write_pending();
        let mut git_running = false;
        while let Some(child) = self.git_child.as_mut() {
            match child.try_wait() {
                Ok(None) if Instant::now() < git_until => thread::sleep(Duration::from_millis(10)),
                Ok(None) => {
                    git_running = true;
                    break;
                }
                _ => self.git_child = None,
            }
        }
        LedgerReport { git_running, ..self.answer() }
    }

    /// What is not written yet and what is owed a commit, after a pass.
    fn answer(&self) -> LedgerReport {
        LedgerReport { unwritten_members: self.pending.len(), uncommitted_files: self.uncommitted.len(), git_running: false }
    }

    fn commit_due(&self) -> Option<Instant> {
        if self.uncommitted.is_empty() || self.repo.is_none() {
            return None;
        }
        let quiet = self.last_write.map(|at| at + self.timing.commit_quiet);
        [quiet, self.retry_at].into_iter().flatten().max().or_else(|| Some(Instant::now()))
    }

    /// Changes a failed write kept are tried again on this timer, and with
    /// the next change.
    fn write_due(&self) -> Option<Instant> {
        if self.pending.is_empty() { None } else { self.write_retry_at }
    }

    fn load_acks(&self) -> HashMap<SessionId, RegisterAck> {
        match (self.inner.upgrade(), &self.repo) {
            (Some(inner), Some(repo)) => SupervisorHandle { inner }.register_acks(repo),
            _ => HashMap::new(),
        }
    }

    /// One burst. An explicit change and the catalog write that carries the
    /// same conversation arrive together; the explicit reason wins. `closing`
    /// starts no git.
    fn absorb(&mut self, batch: Vec<LedgerMessage>, closing: bool) {
        let explicit: HashSet<(SessionId, String)> = batch
            .iter()
            .filter_map(|message| match message {
                LedgerMessage::Change(change, _) => Some((change.session_id, conversation_key(&change.conversation_id))),
                _ => None,
            })
            .collect();
        for message in batch {
            match message {
                LedgerMessage::Catalog(rows) => self.absorb_catalog(rows, &explicit),
                LedgerMessage::Rooms(rooms) => {
                    let next = room_map(rooms);
                    for id in self.rows.keys() {
                        if self.rooms.get(id) != next.get(id) {
                            self.pending.entry(*id).or_default();
                        }
                    }
                    self.rooms = next;
                }
                LedgerMessage::Change(change, at) => {
                    if self.rows.get(&change.session_id).is_none_or(|row| row.driver != change.harness) {
                        continue;
                    }
                    if let (Some(run_id), Some(pid)) = (change.run_id, change.pid) {
                        self.reported_pids.insert(change.session_id, (run_id, pid));
                    }
                    self.pending.entry(change.session_id).or_default().push(Op::Conversation {
                        id: Some(change.conversation_id),
                        change: ConversationOp {
                            reason: change.reason,
                            via: change.via,
                            run_id: change.run_id,
                            confirm: true,
                        },
                        at,
                    });
                }
                LedgerMessage::Title { session_id, title, source } => {
                    if self.rows.contains_key(&session_id) {
                        self.pending.entry(session_id).or_default().push(Op::Title { title, source });
                    }
                }
                // No op: the pass reads the run state from the slot.
                LedgerMessage::Run(session_id) => {
                    if self.rows.contains_key(&session_id) {
                        self.pending.entry(session_id).or_default();
                    }
                }
                LedgerMessage::Repo(repo) if closing => {
                    // No commit and no `git status` at close: the next start
                    // finds what the new repository is owed. Commits owed to
                    // the old one are no longer counted.
                    self.repo = repo;
                    self.uncommitted.clear();
                    self.acks = self.load_acks();
                }
                LedgerMessage::Repo(repo) => {
                    // What the old repository still owes is committed there first.
                    self.commit(true);
                    self.resolved("");
                    self.repo = repo;
                    self.uncommitted.clear();
                    self.retry_at = None;
                    self.commit_failures = 0;
                    self.git = GitProbe::Unknown;
                    self.git_prefix.clear();
                    self.write_retry_at = None;
                    self.write_failures = 0;
                    // As at start: files there may lag behind the catalog
                    // (written before, then left), or be owed a commit.
                    self.acks = self.load_acks();
                    self.mark_all(true);
                    self.queue_owed();
                }
                LedgerMessage::Flush { done, .. } | LedgerMessage::Close { done, .. } => {
                    let _ = done.send(self.answer());
                }
            }
        }
    }

    fn absorb_catalog(&mut self, rows: Vec<LedgerRow>, explicit: &HashSet<(SessionId, String)>) {
        let next: HashMap<SessionId, LedgerRow> = rows.into_iter().map(|row| (row.session_id, row)).collect();
        for (id, row) in &next {
            let previous = self.rows.get(id);
            if previous == Some(row) {
                continue;
            }
            let ops = self.pending.entry(*id).or_default();
            let conversation_changed = previous.map(|previous| &previous.harness_session_id) != Some(&row.harness_session_id);
            let proven_elsewhere = row
                .harness_session_id
                .as_deref()
                .is_some_and(|conversation| explicit.contains(&(*id, conversation_key(conversation))));
            // A member held because the catalog's value is in doubt keeps the
            // register's until a proven change replaces it.
            if conversation_changed && !proven_elsewhere && !keeps_register_value(row.hold.as_deref()) {
                // Only a room import creates a member that already has one.
                let (reason, via) = if previous.is_none() { ("import", "import") } else { ("observed", "catalog") };
                ops.push(Op::Conversation {
                    id: row.harness_session_id.clone(),
                    change: ConversationOp { reason, via, run_id: None, confirm: false },
                    at: register_now(),
                });
            }
        }
        for id in self.rows.keys() {
            if !next.contains_key(id) {
                self.pending.entry(*id).or_default().push(Op::Retire);
            }
        }
        self.rows = next;
    }

    /// Every member is looked at again; `seed` also brings in the catalog's
    /// conversation where the file names none or only lags (no history is
    /// invented for what happened meanwhile).
    fn mark_all(&mut self, seed: bool) {
        let at = register_now();
        for id in self.rows.keys() {
            let ops = self.pending.entry(*id).or_default();
            if seed {
                ops.push(Op::Seed { at: at.clone() });
            }
        }
    }

    fn live_runs(&self, ids: &[SessionId]) -> HashMap<SessionId, LiveRun> {
        let Some(inner) = self.inner.upgrade() else {
            return HashMap::new();
        };
        let slots = inner.slots.lock();
        ids.iter()
            .filter_map(|id| {
                let slot = slots.get_by_id(*id)?;
                let running = slot.running.is_some() && slot.run_id.is_some();
                let stopping = !running && (slot.running.is_some() || slot.termination_uncertain);
                Some((*id, LiveRun { running, stopping, run_id: slot.run_id.filter(|_| running), pid: slot.process_id.filter(|_| running) }))
            })
            .collect()
    }

    fn write_pending(&mut self) {
        if self.pending.is_empty() {
            self.write_retry_at = None;
            return;
        }
        let Some(repo) = self.repo.clone() else {
            self.pending.clear();
            return;
        };
        if let Err(error) = self.prepare_layout(&repo) {
            self.report(
                format!("layout:{}", repo.display()),
                format!("The session register folder in {} cannot be prepared ({error}); PRIM-1 tries again.", repo.display()),
            );
            return self.write_failed();
        }
        self.resolved("layout:");
        let pending = std::mem::take(&mut self.pending);
        let ids: Vec<SessionId> = pending.keys().copied().collect();
        let live = self.live_runs(&ids);
        let mut writes = Vec::new();
        for (id, ops) in pending {
            match self.plan_member(&repo, id, &ops, live.get(&id).copied().unwrap_or_default()) {
                Planned::Current(record) => {
                    self.records.insert(id, record);
                    self.member_written(id);
                }
                Planned::Nothing | Planned::Foreign => {}
                Planned::Later => self.keep(id, ops),
                Planned::Write { record, on_disk } => writes.push((id, ops, record, on_disk)),
            }
        }
        // One writer at a time in this repository, across installs and
        // copied runtimes: under the lock each file is read again, its holder
        // and revision rechecked, and replaced. Held elsewhere, the changes
        // wait like a failed write.
        let lock = if writes.is_empty() {
            None
        } else {
            match writer_lock(&repo) {
                Ok(lock) => {
                    self.resolved("lock");
                    Some(lock)
                }
                Err(error) => {
                    self.report(
                        "lock".into(),
                        format!(
                            "The session register lock {} cannot be taken ({error}): another PRIM-1 may be writing this register. PRIM-1 tries again.",
                            repo.join(REGISTER_DIR).join(REGISTER_WRITER_LOCK).display()
                        ),
                    );
                    for (id, ops, ..) in std::mem::take(&mut writes) {
                        self.keep(id, ops);
                    }
                    None
                }
            }
        };
        // The watermark first: a crash in between leaves a file behind it,
        // which the catalog fills in, never one ahead of it.
        let marks: Vec<(SessionId, RegisterAck)> = writes
            .iter()
            .filter(|(id, ..)| self.rows.contains_key(id))
            .map(|(id, _, record, _)| {
                let conversation = record.conversation.as_ref().map(|conversation| conversation.id.clone());
                (*id, RegisterAck { revision: record.revision, conversation })
            })
            .collect();
        if !marks.is_empty() {
            match self.record_acks(&repo, &marks) {
                Ok(()) => {
                    self.resolved("acks");
                    self.acks.extend(marks);
                }
                Err(error) => {
                    self.report(
                        "acks".into(),
                        format!("The session register's watermark could not be saved in the catalog ({error}); its files are written once it can be."),
                    );
                    for (id, ops, ..) in std::mem::take(&mut writes) {
                        self.keep(id, ops);
                    }
                }
            }
        }
        for (id, ops, record, on_disk) in writes {
            if self.write_record(&repo, id, record, on_disk) {
                self.member_written(id);
            } else {
                self.keep(id, ops);
            }
        }
        if let Some(lock) = lock {
            let _ = lock.unlock();
        }
        if self.pending.is_empty() {
            self.write_failures = 0;
            self.write_retry_at = None;
        } else {
            self.write_failed();
        }
    }

    fn keep(&mut self, id: SessionId, mut ops: Vec<Op>) {
        if ops.len() > RETAINED_OPS_MAX {
            ops.drain(..ops.len() - RETAINED_OPS_MAX);
        }
        self.pending.insert(id, ops);
    }

    /// The first retry after `commit_retry`, doubled per failure.
    fn write_failed(&mut self) {
        self.write_failures = self.write_failures.saturating_add(1);
        let delay = self
            .timing
            .commit_retry
            .saturating_mul(1 << self.write_failures.saturating_sub(1).min(10))
            .min(self.timing.commit_backoff_max);
        self.write_retry_at = Some(Instant::now() + delay);
    }

    fn record_acks(&self, repo: &Path, marks: &[(SessionId, RegisterAck)]) -> std::result::Result<(), String> {
        let inner = self.inner.upgrade().ok_or_else(|| "PRIM-1 is closing".to_owned())?;
        SupervisorHandle { inner }.record_register_acks(repo, marks).map_err(|error| format!("{error:#}"))
    }

    /// `prim1\sessions`, `.gitignore` and `.gitattributes`; the two files are
    /// created only when missing and committed with the next session files.
    fn prepare_layout(&mut self, repo: &Path) -> std::io::Result<()> {
        let base = repo.join(REGISTER_DIR);
        fs::create_dir_all(base.join(REGISTER_SESSIONS_DIR))?;
        for (name, content) in [(".gitignore", REGISTER_GITIGNORE), (".gitattributes", REGISTER_GITATTRIBUTES)] {
            match OpenOptions::new().write(true).create_new(true).open(base.join(name)) {
                Ok(mut file) => {
                    file.write_all(content.as_bytes())?;
                    file.sync_all()?;
                    self.uncommitted.insert(format!("{REGISTER_DIR}/{name}"), String::new());
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn member_written(&mut self, id: SessionId) {
        for kind in ["unreadable", "held", "write", "revision"] {
            self.resolved(&format!("{kind}:{id}"));
        }
    }

    fn plan_member(&mut self, repo: &Path, id: SessionId, ops: &[Op], live: LiveRun) -> Planned {
        let path = register_path(repo, id);
        let disk = match read_register_file(&path) {
            Ok(disk) => disk,
            Err(message) => {
                self.report(format!("unreadable:{id}"), message);
                return Planned::Later;
            }
        };
        if let Some(record) = &disk
            && record.holder.install_id != self.install_id
        {
            self.report_held(id, &path, &record.holder);
            return Planned::Foreign;
        }
        // At or behind the watermark the file is this install's own and only
        // lags; ahead of it, something else changed it.
        let own = disk.as_ref().is_none_or(|disk| self.acks.get(&id).is_some_and(|ack| ack.covers(disk)));
        let Some(record) = self.next_record(id, disk.as_ref(), ops, live, own) else {
            return Planned::Nothing;
        };
        if disk.as_ref().is_some_and(|disk| disk.same_content(&record)) {
            return Planned::Current(record);
        }
        let on_disk = disk.as_ref().map_or(0, |disk| disk.revision);
        // Never below the record it continues, so a restored older file does
        // not make the revision go backwards.
        let revision = on_disk.max(record.revision) + 1;
        Planned::Write { record: RegisterRecordV1 { revision, updated_at: register_now(), ..record }, on_disk }
    }

    fn report_held(&mut self, id: SessionId, path: &Path, holder: &RegisterHolder) {
        self.report(
            format!("held:{id}"),
            format!(
                "{} is held by another PRIM-1 install ({} on {}); this install leaves it as it is.",
                path.display(),
                holder.install_id,
                holder.machine
            ),
        );
    }

    /// Under the writer lock. False when another install took the file, it
    /// changed since it was read, or it cannot be replaced; the changes are
    /// then kept and tried again (a file another install holds drops them
    /// at the next pass).
    fn write_record(&mut self, repo: &Path, id: SessionId, record: RegisterRecordV1, on_disk: u64) -> bool {
        let path = register_path(repo, id);
        let file_name = format!("{id}.json");
        match read_register_file(&path) {
            Ok(Some(now)) if now.holder.install_id != self.install_id => {
                self.report_held(id, &path, &now.holder);
                return false;
            }
            Ok(now) if now.as_ref().map_or(0, |now| now.revision) == on_disk => {}
            _ => {
                self.report(
                    format!("revision:{id}"),
                    format!("The session register file {} changed while it was written; PRIM-1 tries again.", path.display()),
                );
                return false;
            }
        }
        let dir = repo.join(REGISTER_DIR).join(REGISTER_SESSIONS_DIR);
        if let Err(error) = write_atomically(&dir, &file_name, &serialize_register(&record), "session register file") {
            self.report(
                format!("write:{id}"),
                format!("The session register file {} could not be written ({error:#}); PRIM-1 tries again.", path.display()),
            );
            return false;
        }
        self.uncommitted.insert(format!("{REGISTER_DIR}/{REGISTER_SESSIONS_DIR}/{file_name}"), record.label.clone());
        self.records.insert(id, record);
        self.last_write = Some(Instant::now());
        true
    }

    /// The member's record after `ops`, from the file on disk, else the last
    /// one written. None when there is nothing to write. `own`: the file is
    /// this install's and at most lags behind it.
    fn next_record(
        &self,
        id: SessionId,
        disk: Option<&RegisterRecordV1>,
        ops: &[Op],
        live: LiveRun,
        own: bool,
    ) -> Option<RegisterRecordV1> {
        let row = self.rows.get(&id);
        let now = register_now();
        // An own file that lags this install's last write (a checkout restored
        // an older one) never wins: start from the newer record, so an update
        // that carries no conversation op cannot write older history back.
        let cached = self.records.get(&id);
        let base = match (disk, cached) {
            (Some(disk), Some(cached)) if own && cached.revision > disk.revision => Some(cached),
            (disk, cached) => disk.or(cached),
        };
        let mut record = match (base, row) {
            (Some(record), _) => record.clone(),
            (None, Some(row)) => RegisterRecordV1::new(row, &self.install_id, &self.machine, &now),
            (None, None) => return None,
        };
        if record.prim_session_id != id || row.is_some_and(|row| row.driver != record.harness) {
            return None;
        }
        let mut retired = false;
        for op in ops {
            match op {
                Op::Conversation { id, change, at } => record.apply_conversation(id.as_deref(), change, at),
                Op::Seed { at } => {
                    let Some(row) = row else {
                        continue;
                    };
                    let catalog = row.harness_session_id.as_deref();
                    let current = record.conversation.as_ref().map(|conversation| conversation_key(&conversation.id));
                    if current == catalog.map(conversation_key) {
                        continue;
                    }
                    // A file naming nothing takes the catalog's conversation;
                    // a lagging own file follows the catalog unless a hold
                    // keeps the register's value.
                    if (current.is_none() && catalog.is_some()) || (own && !keeps_register_value(row.hold.as_deref())) {
                        let change = ConversationOp { reason: "observed", via: "catalog", run_id: None, confirm: false };
                        record.apply_conversation(catalog, &change, at);
                    }
                }
                Op::Title { title, source } => {
                    record.titles.current = Some(title.clone());
                    record.titles.source = Some((*source).into());
                }
                Op::Retire => retired = true,
            }
        }
        let (state, run_id, pid) = match row {
            None if retired => ("retired", None, None),
            None => return None,
            Some(_) if live.running => {
                let pid = live.pid.or_else(|| {
                    self.reported_pids.get(&id).filter(|(run, _)| Some(*run) == live.run_id).map(|(_, pid)| *pid)
                });
                ("running", live.run_id, pid)
            }
            // A stop that has not proved release never claims "stopped" and
            // never erases the identity of a process that may still run.
            Some(_) if live.stopping => ("stopping", record.holder.run_id, record.holder.pid),
            Some(_) => ("stopped", None, None),
        };
        let holder = &mut record.holder;
        let proc_start = if pid.is_some() && pid == holder.pid && run_id == holder.run_id {
            holder.proc_start.clone()
        } else {
            pid.and_then(process_start_time)
        };
        if !(holder.install_id == self.install_id && holder.state == state && holder.run_id == run_id) {
            holder.since = now.clone();
        }
        holder.install_id = self.install_id.clone();
        holder.machine = self.machine.clone();
        holder.pid = pid;
        holder.proc_start = proc_start;
        holder.run_id = run_id;
        holder.state = state.into();
        if let Some(row) = row {
            record.label = row.label.clone();
            record.working_dir = row.working_dir.clone();
            let room = self.rooms.get(&id).map(|(room, label)| (*room, label.clone()));
            if record.room.as_ref().map(|room| (room.id, room.label.clone())) != room {
                record.room = room.map(|(id, label)| RegisterRoom { id, label, extra: Extra::new() });
            }
            let hint = transcript_hint(row, record.conversation.as_ref().map(|conversation| conversation.id.as_str()));
            if record.transcript_hint.as_ref().map(|hint| (&hint.home, &hint.relative))
                != hint.as_ref().map(|hint| (&hint.home, &hint.relative))
            {
                record.transcript_hint = hint;
            }
        }
        Some(record)
    }

    /// Shown in Settings until resolved; logged once per key.
    fn report(&mut self, key: String, message: String) {
        self.notes.lock().insert(key.clone(), message.clone());
        if !self.reported.insert(key) {
            return;
        }
        if let Some(inner) = self.inner.upgrade() {
            SupervisorHandle { inner }.emit(RuntimeEvent::SystemLog { level: LogLevel::Warn, message, timestamp: now_rfc3339() });
        }
    }

    /// Reports whose key starts with `prefix` no longer hold; a recurrence
    /// is logged again.
    fn resolved(&mut self, prefix: &str) {
        self.notes.lock().retain(|key, _| !key.starts_with(prefix));
        self.reported.retain(|key| !key.starts_with(prefix));
    }

    /// Probes once per repository; a busy or failing git leaves it unknown.
    fn probe_git(&mut self, repo: &Path) -> GitProbe {
        if self.git == GitProbe::Unknown {
            self.git = match self.run_git(repo, &["rev-parse", "--is-inside-work-tree", "--show-prefix"]) {
                GitRun::Done { success: true, stdout, .. } if stdout.lines().next().map(str::trim) == Some("true") => {
                    self.git_prefix = stdout.lines().nth(1).unwrap_or_default().trim().to_owned();
                    GitProbe::Ready
                }
                GitRun::Done { .. } => GitProbe::NotRepository,
                GitRun::Missing => GitProbe::Missing,
                GitRun::Busy | GitRun::Failed(_) => GitProbe::Unknown,
            };
        }
        self.git
    }

    /// What this install wrote and no commit took: a close starts no git,
    /// and a deleted member's file is never written again. Found once per
    /// repository with `git status`; files another install holds are left.
    fn queue_owed(&mut self) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        if self.probe_git(&repo) != GitProbe::Ready {
            return;
        }
        let GitRun::Done { success: true, stdout, .. } =
            self.run_git(&repo, &["status", "--porcelain", "-z", "--untracked-files=all", "--", REGISTER_DIR])
        else {
            return;
        };
        let sessions = format!("{REGISTER_DIR}/{REGISTER_SESSIONS_DIR}/");
        let mut entries = stdout.split('\0');
        while let Some(entry) = entries.next() {
            let Some((status, path)) = entry.split_at_checked(3) else {
                continue;
            };
            // A rename or copy names its source next.
            if status.contains(['R', 'C']) {
                entries.next();
            }
            let Some(path) = path.strip_prefix(self.git_prefix.as_str()) else {
                continue;
            };
            if path == format!("{REGISTER_DIR}/.gitignore") || path == format!("{REGISTER_DIR}/.gitattributes") {
                self.uncommitted.entry(path.to_owned()).or_default();
            } else if path.strip_prefix(&sessions).is_some_and(|name| name.ends_with(".json") && !name.contains('/'))
                && let Ok(Some(record)) = read_register_file(&repo.join(path))
                && record.holder.install_id == self.install_id
            {
                self.uncommitted.insert(path.to_owned(), record.label);
            }
        }
    }

    /// The state that makes a commit unsafe, if any: on a detached HEAD, or
    /// in a rebase, merge, cherry-pick, revert or bisect, a commit can be
    /// dropped with it.
    fn repository_busy(&mut self, repo: &Path) -> std::result::Result<Option<&'static str>, GitRun> {
        const STATES: [(&str, &str); 6] = [
            ("rebase-merge", "in the middle of a rebase"),
            ("rebase-apply", "in the middle of a rebase"),
            ("MERGE_HEAD", "in the middle of a merge"),
            ("CHERRY_PICK_HEAD", "in the middle of a cherry-pick"),
            ("REVERT_HEAD", "in the middle of a revert"),
            ("BISECT_LOG", "in the middle of a bisect"),
        ];
        let mut args = vec!["rev-parse".to_owned()];
        for (name, _) in STATES {
            args.extend(["--git-path".to_owned(), name.to_owned()]);
        }
        let stdout = match self.run_git_owned(repo, &args) {
            GitRun::Done { success: true, stdout, .. } => stdout,
            other => return Err(other),
        };
        for ((_, state), path) in STATES.iter().zip(stdout.lines()) {
            if repo.join(path.trim()).exists() {
                return Ok(Some(state));
            }
        }
        match self.run_git(repo, &["symbolic-ref", "-q", "HEAD"]) {
            GitRun::Done { success: true, .. } => Ok(None),
            GitRun::Done { success: false, stderr, .. } if stderr.trim().is_empty() => Ok(Some("on a detached HEAD")),
            other => Err(other),
        }
    }

    fn commit(&mut self, force: bool) {
        if self.uncommitted.is_empty() {
            return;
        }
        let Some(repo) = self.repo.clone() else {
            self.uncommitted.clear();
            return;
        };
        if !force && self.commit_due().is_some_and(|due| due > Instant::now()) {
            return;
        }
        match self.probe_git(&repo) {
            GitProbe::Ready => {}
            GitProbe::Unknown => return self.retry_later(None),
            GitProbe::Missing => {
                self.uncommitted.clear();
                return self.report(
                    "git:missing".into(),
                    "Git was not found, so the session register files are written but not committed.".into(),
                );
            }
            GitProbe::NotRepository => {
                self.uncommitted.clear();
                return self.report(
                    format!("git:not-repository:{}", repo.display()),
                    format!("{} is not in a Git work tree, so the session register files are written but not committed.", repo.display()),
                );
            }
        }
        match self.repository_busy(&repo) {
            Ok(None) => {}
            Ok(Some(state)) => {
                self.report(
                    format!("git:state:{}", repo.display()),
                    format!("The personal repository {} is {state}; the session register commits once that ends.", repo.display()),
                );
                return self.retry_later(None);
            }
            Err(outcome) => return self.commit_failed(outcome),
        }
        let paths: Vec<String> = self.uncommitted.keys().cloned().collect();
        let with_paths = |head: &[&str], paths: &[String]| -> Vec<String> {
            head.iter().map(|part| (*part).to_owned()).chain(std::iter::once("--".to_owned())).chain(paths.iter().cloned()).collect()
        };
        let status = with_paths(&["status", "--porcelain", "--untracked-files=all", "--ignored=matching"], &paths);
        match self.run_git_owned(&repo, &status) {
            GitRun::Done { success: true, stdout, .. } if stdout.trim().is_empty() => {
                self.uncommitted.clear();
                return;
            }
            GitRun::Done { success: true, stdout, .. } if stdout.lines().any(|line| line.starts_with("!!")) => {
                self.uncommitted.clear();
                return self.report(
                    format!("git:ignored:{}", repo.display()),
                    format!("Git ignores the session register in {} (a .gitignore matches prim1), so its files are written but not committed.", repo.display()),
                );
            }
            GitRun::Done { success: true, .. } => {}
            other => return self.commit_failed(other),
        }
        // Nothing is staged: a tracked file is committed from the working
        // tree, and a new one is only marked with intent to add, which a
        // commit someone else makes meanwhile does not take.
        let tracked: HashSet<String> = match self.run_git_owned(&repo, &with_paths(&["ls-files", "-z"], &paths)) {
            GitRun::Done { success: true, stdout, .. } => stdout.split('\0').filter(|path| !path.is_empty()).map(str::to_owned).collect(),
            other => return self.commit_failed(other),
        };
        let untracked: Vec<String> = paths.iter().filter(|path| !tracked.contains(*path)).cloned().collect();
        if !untracked.is_empty() {
            match self.run_git_owned(&repo, &with_paths(&["add", "--intent-to-add"], &untracked)) {
                GitRun::Done { success: true, .. } => {}
                other => return self.commit_failed(other),
            }
        }
        let message = self.commit_message();
        match self.run_git_owned(&repo, &with_paths(&["commit", "--only", "-m", &message], &paths)) {
            GitRun::Done { success: true, .. } => {}
            GitRun::Done { stdout, .. } if stdout.contains("nothing to commit") || stdout.contains("no changes added") => {}
            other => return self.commit_failed(other),
        }
        self.uncommitted.clear();
        self.retry_at = None;
        self.commit_failures = 0;
        self.resolved("git:");
    }

    /// `ADD: PRIM-1 session register - <labels>`.
    fn commit_message(&self) -> String {
        let mut labels: Vec<&str> = Vec::new();
        for label in self.uncommitted.values() {
            if !label.is_empty() && !labels.contains(&label.as_str()) {
                labels.push(label);
            }
        }
        let mut named = labels.iter().take(COMMIT_LABELS_MAX).copied().collect::<Vec<_>>().join(", ");
        if labels.len() > COMMIT_LABELS_MAX {
            named.push_str(&format!(" and {} more", labels.len() - COMMIT_LABELS_MAX));
        }
        if named.is_empty() {
            named.push_str("setup");
        }
        format!("ADD: PRIM-1 session register - {named}")
    }

    fn commit_failed(&mut self, outcome: GitRun) {
        let detail = match outcome {
            GitRun::Done { stderr, stdout, .. } => {
                let text = if stderr.trim().is_empty() { stdout } else { stderr };
                // Another git holds the index: wait without counting a failure.
                if text.contains("index.lock") {
                    return self.retry_later(None);
                }
                text.trim().lines().take(3).collect::<Vec<_>>().join(" ")
            }
            GitRun::Busy => return self.retry_later(None),
            GitRun::Missing => "Git was not found".into(),
            GitRun::Failed(error) => error,
        };
        self.retry_later(Some(detail));
    }

    fn retry_later(&mut self, failure: Option<String>) {
        let delay = match failure {
            None => self.timing.commit_retry,
            Some(detail) => {
                self.commit_failures = self.commit_failures.saturating_add(1);
                let delay = self
                    .timing
                    .commit_retry
                    .saturating_mul(1 << self.commit_failures.min(10))
                    .min(self.timing.commit_backoff_max);
                self.report(
                    format!("git:commit:{detail}"),
                    format!("The session register commit failed ({detail}); PRIM-1 tries again in {} s.", delay.as_secs().max(1)),
                );
                delay
            }
        };
        self.retry_at = Some(Instant::now() + delay);
    }

    fn run_git(&mut self, repo: &Path, args: &[&str]) -> GitRun {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        self.run_git_owned(repo, &args)
    }

    /// `git -C <repo> …`, no window, no prompt, optional locks off. Waits up
    /// to `git_wait`, or until the close is requested; a git still running
    /// then is left to finish on its own. None starts after the close.
    fn run_git_owned(&mut self, repo: &Path, args: &[String]) -> GitRun {
        if let Some(child) = self.git_child.as_mut() {
            match child.try_wait() {
                Ok(None) => return GitRun::Busy,
                _ => self.git_child = None,
            }
        }
        if self.closing.load(Ordering::Acquire) {
            return GitRun::Busy;
        }
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        // A test folder never commits into a repository around it.
        #[cfg(test)]
        if let Some(parent) = repo.parent() {
            command.env("GIT_CEILING_DIRECTORIES", parent);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return GitRun::Missing,
            Err(error) => return GitRun::Failed(format!("git could not start ({error})")),
        };
        let stdout = child.stdout.take().map(read_pipe);
        let stderr = child.stderr.take().map(read_pipe);
        let deadline = Instant::now() + self.timing.git_wait;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    // A helper git started may still hold a pipe; never wait on it long.
                    let collect = |pipe: Option<mpsc::Receiver<String>>| {
                        pipe.and_then(|pipe| pipe.recv_timeout(Duration::from_secs(1)).ok()).unwrap_or_default()
                    };
                    return GitRun::Done { success: status.success(), stdout: collect(stdout), stderr: collect(stderr) };
                }
                // The close waits for it within its budget and reports it.
                Ok(None) if self.closing.load(Ordering::Acquire) => {
                    self.git_child = Some(child);
                    return GitRun::Busy;
                }
                Ok(None) if Instant::now() >= deadline => {
                    self.git_child = Some(child);
                    self.report(
                        "git:slow".into(),
                        format!("A git command in the personal repository did not finish within {} s; PRIM-1 waits for it before committing again.", self.timing.git_wait.as_secs()),
                    );
                    return GitRun::Busy;
                }
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(error) => return GitRun::Failed(format!("git could not be waited for ({error})")),
            }
        }
    }
}

/// `prim1\.writer.lock`, taken without waiting. Unlocked after the pass, and
/// by the OS if the writer dies.
fn writer_lock(repo: &Path) -> std::io::Result<File> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(repo.join(REGISTER_DIR).join(REGISTER_WRITER_LOCK))?;
    lock.try_lock()?;
    Ok(lock)
}

/// A writer of `install_id` with no supervisor.
#[cfg(test)]
fn writer_for_tests(repo: &Path, install_id: &str) -> Writer {
    Writer::new(
        repo.to_path_buf(),
        install_id.into(),
        Weak::new(),
        LedgerTiming::DEFAULT,
        Notes::default(),
        Arc::new(AtomicBool::new(false)),
        Vec::new(),
        Vec::new(),
    )
}

/// One replace of `record`'s file, planned when the file was at `on_disk`.
#[cfg(test)]
pub(crate) fn write_record_for_tests(repo: &Path, install_id: &str, record: RegisterRecordV1, on_disk: u64) -> bool {
    writer_for_tests(repo, install_id).write_record(repo, record.prim_session_id, record, on_disk)
}

/// A close while `git` (started earlier) may still run; the child is handed
/// back, never killed by the writer.
#[cfg(test)]
pub(crate) fn close_with_git_for_tests(repo: &Path, git: Child, git_until: Instant) -> (LedgerReport, Option<Child>) {
    let mut writer = writer_for_tests(repo, &Uuid::new_v4().to_string());
    writer.git_child = Some(git);
    let report = writer.close(Vec::new(), git_until);
    (report, writer.git_child.take())
}

fn read_pipe(pipe: impl std::io::Read + Send + 'static) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.take(GIT_OUTPUT_MAX_BYTES).read_to_end(&mut bytes);
        let _ = sender.send(String::from_utf8_lossy(&bytes).into_owned());
    });
    receiver
}

type Notes = Arc<Mutex<BTreeMap<String, String>>>;

/// The open reports in one paragraph; files held by another install are
/// counted, not listed.
fn notes_text(notes: &BTreeMap<String, String>) -> Option<String> {
    let held: Vec<&String> = notes.iter().filter(|(key, _)| key.starts_with("held:")).map(|(_, note)| note).collect();
    let mut parts: Vec<String> = notes.iter().filter(|(key, _)| !key.starts_with("held:")).map(|(_, note)| note.clone()).collect();
    match held.as_slice() {
        [] => {}
        [one] => parts.push((*one).clone()),
        many => parts.push(format!(
            "{} register files are held by another PRIM-1 install; this install leaves them as they are.",
            many.len()
        )),
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn room_map(rooms: Vec<LedgerRoom>) -> HashMap<SessionId, (RoomId, String)> {
    rooms
        .into_iter()
        .flat_map(|LedgerRoom { id, label, members }| members.into_iter().map(move |member| (member, (id, label.clone()))))
        .collect()
}
