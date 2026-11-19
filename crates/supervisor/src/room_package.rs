//! Save and load one room (A92). A `.prim1room` file is UTF-8 JSON Lines:
//! line 1 is the manifest (the room definition and each member's identity and
//! launch choices, under the catalogs' own field names), lines 2..n are the
//! room log exactly as `room_log.rs` writes it. Harness conversation files,
//! run state, last launches and working-folder identities never travel.
//!
//! A room can also be read from a copied runtime folder (its
//! `room-catalog-v1.json`), read-only; it becomes the same in-memory package
//! and goes through the same checks and import.
//!
//! Load creates a NEW room with NEW sessions and starts nothing. Imported
//! sessions are saved held (`room_import_pending`) before the room-catalog
//! commit and released to their honest states only after it, so a crash
//! between the two catalogs never leaves a resumable orphan copy.
use std::io::Read as _;
use std::fs::TryLockError;

use super::*;
use anyhow::ensure;
use room_log::ReplayRecord;
use rooms::ROOM_LABEL_MAX_CHARS;
use sha2::{Digest, Sha256};
use shared_types::{
    PermissionProfile, RoomExportReport, RoomFeedEvent, RoomImportMember, RoomImportMemberState,
    RoomImportReport, RoomSequence, RoomSourceSummary, SessionRecovery,
};

const PACKAGE_RECORD: &str = "prim1_room_package";
const PACKAGE_VERSION: u32 = 1;
const MANIFEST_MAX_BYTES: usize = 1024 * 1024;
const PACKAGE_MAX_BYTES: u64 = room_log::MAX_LOG_BYTES + MANIFEST_MAX_BYTES as u64;
const STORE_SCAN_MAX_DIRS: usize = 5_000;
const IMPORT_PENDING: &str = "room_import_pending";
/// Native hold reasons carry run diagnostics; they travel unchanged up to
/// this bound (the 1 MiB manifest cap still bounds the whole file).
const HOLD_REASON_MAX_CHARS: usize = 16 * 1024;
const NOT_A_ROOM: &str = "This file is not a PRIM-1 room.";
const INCOMPLETE: &str = "The room file is incomplete: it ends inside a record. Copy it again.";
const TOO_LARGE: &str = "The room file exceeds 257 MiB; a room's history is limited to 256 MiB.";
const CATALOG_MAX_BYTES: u64 = 16 * 1024 * 1024;
const RUNNING: &str = "A running PRIM-1 holds this room's log. Close it, or copy its folder first.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Manifest {
    record: String,
    version: u32,
    exported_at: String,
    app_version: String,
    #[serde(default)]
    source_machine: Option<String>,
    room: ManifestRoom,
    sessions: Vec<ManifestSession>,
    feed: ManifestFeed,
}

/// Completeness of lines 2..n: a file cut at a record boundary keeps a
/// continuous sequence, so the count, last sequence and digest must match.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ManifestFeed {
    records: u64,
    last_sequence: RoomSequence,
    sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ManifestRoom {
    room_id: RoomId,
    label: String,
    member_ids: Vec<SessionId>,
    membership_revision: RoomRevision,
    brief_on_join: bool,
    #[serde(default)]
    brief_template: Option<String>,
    /// Floors mean something only in this log epoch; absent on v1 rooms.
    #[serde(default)]
    join_floor_epoch: Option<Uuid>,
    #[serde(default)]
    join_floors: Option<BTreeMap<SessionId, RoomSequence>>,
    #[serde(default)]
    history_restarted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ManifestSession {
    session_id: SessionId,
    label: String,
    driver: DriverKind,
    working_directory: ManifestDirectory,
    permission_profile: PermissionProfile,
    launch_choices: SessionLaunchChoices,
    #[serde(default)]
    harness_session_id: Option<String>,
    /// The current hold only, carrying its original conversation ID. Earlier
    /// recovery history stays with its source, and the save says so.
    #[serde(default)]
    recovery: Option<SessionRecovery>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ManifestDirectory {
    namespace: WorkingDirectoryNamespace,
    canonical_path: String,
}

struct Package<'a> {
    manifest: Manifest,
    log: &'a [u8],
    epoch: Uuid,
    tail: RoomSequence,
    records: u64,
}

fn damaged(line: usize, reason: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("The room file is damaged at line {line}: {reason}.")
}

fn canonical_uuid(id: &str) -> bool {
    Uuid::parse_str(id).is_ok_and(|uuid| !uuid.is_nil() && uuid.to_string() == id)
}

/// Only for hold text this module writes itself; saved holds are never cut.
fn bounded_reason(reason: &str) -> String {
    reason.chars().take(HOLD_REASON_MAX_CHARS).collect()
}

fn bounded_text(value: &str, max_chars: usize) -> bool {
    !value.is_empty() && value.chars().count() <= max_chars && !value.contains('\0')
}

/// Every check runs on untrusted input before anything is written.
fn validate_manifest(manifest: &Manifest) -> Result<()> {
    let room = &manifest.room;
    ensure!(bounded_text(&manifest.exported_at, 64) && bounded_text(&manifest.app_version, 64)
        && manifest.source_machine.as_deref().is_none_or(|name| bounded_text(name, 64)),
        "the save details are malformed");
    validate_room_label(&room.label)?;
    ensure!(room.membership_revision > 0, "the membership revision is zero");
    ensure!(room.member_ids.len() <= ROOM_MEMBER_MAX_COUNT, "more than {ROOM_MEMBER_MAX_COUNT} members");
    ensure!(room.member_ids.iter().collect::<HashSet<_>>().len() == room.member_ids.len(), "a member is listed twice");
    ensure!(manifest.sessions.len() == room.member_ids.len()
        && manifest.sessions.iter().map(|session| session.session_id).collect::<HashSet<_>>()
            == room.member_ids.iter().copied().collect::<HashSet<_>>(),
        "the member list and the member details differ");
    normalize_room_brief_template(room.brief_template.clone())?;
    for session in &manifest.sessions {
        let subject = format!("member \"{}\"", session.label);
        validate_session_label(&session.label)?;
        let terminal = matches!(session.driver, DriverKind::GenericTerminal | DriverKind::Prime);
        ensure!(!(terminal && session.permission_profile == PermissionProfile::Unsafe),
            "{subject} cannot use the unsafe profile");
        ensure!((session.driver == DriverKind::Prime) == (session.working_directory.namespace == WorkingDirectoryNamespace::WslUbuntu),
            "{subject} has a folder of the wrong kind");
        let path = &session.working_directory.canonical_path;
        ensure!(bounded_text(path, 4096) && path.trim() == path && match session.working_directory.namespace {
            WorkingDirectoryNamespace::Windows => Path::new(path).is_absolute(),
            WorkingDirectoryNamespace::WslUbuntu => path.starts_with('/'),
        }, "{subject} has an invalid folder path");
        session::validate_launch_choices(session.driver, &session.launch_choices)
            .with_context(|| format!("{subject} has invalid launch choices"))?;
        if terminal {
            ensure!(session.harness_session_id.is_none() && session.recovery.is_none(),
                "{subject} is a terminal but names a conversation");
        }
        // IDs are canonical before any store path is built from them.
        ensure!(session.harness_session_id.as_deref().is_none_or(canonical_uuid),
            "{subject} has an invalid conversation ID");
        if let Some(hold) = &session.recovery {
            ensure!(hold.provenance.len() <= 64 && !hold.provenance.is_empty()
                && hold.provenance.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                && bounded_text(&hold.reason, HOLD_REASON_MAX_CHARS)
                && hold.original_id.as_deref().is_none_or(|id| bounded_text(id, 128) && !id.chars().any(char::is_control)),
                "{subject} has a malformed hold");
        }
    }
    Ok(())
}

/// Parse and check the whole file with the same rules as replay: header,
/// room and epoch identity, sequence continuity, schema, cards and bounds.
fn parse_package(bytes: &[u8]) -> Result<Package<'_>> {
    if bytes.len() as u64 > PACKAGE_MAX_BYTES { bail!(TOO_LARGE); }
    let Some(end) = bytes.iter().take(MANIFEST_MAX_BYTES + 1).position(|byte| *byte == b'\n') else {
        if bytes.starts_with(b"{\"record\":\"prim1_room_package\"") {
            if bytes.len() > MANIFEST_MAX_BYTES { return Err(damaged(1, "the room details exceed 1 MiB")); }
            bail!(INCOMPLETE);
        }
        bail!(NOT_A_ROOM);
    };
    let line = &bytes[..end];
    #[derive(Deserialize)]
    struct Head { record: String, version: u32 }
    let head = serde_json::from_slice::<Head>(line).ok()
        .filter(|head| head.record == PACKAGE_RECORD).ok_or_else(|| anyhow!(NOT_A_ROOM))?;
    if head.version > PACKAGE_VERSION {
        bail!("This room was saved by a newer PRIM-1 (package version {}); this build reads version {PACKAGE_VERSION}.", head.version);
    }
    ensure!(head.version == PACKAGE_VERSION, NOT_A_ROOM);
    let manifest: Manifest = serde_json::from_slice(line).map_err(|error| damaged(1, error))?;
    validate_manifest(&manifest).map_err(|error| damaged(1, format!("{error:#}")))?;
    let room_id = manifest.room.room_id;
    let log = &bytes[end + 1..];
    if log.len() as u64 > room_log::MAX_LOG_BYTES { bail!(TOO_LARGE); }
    if format!("{:x}", Sha256::digest(log)) != manifest.feed.sha256 {
        bail!("The room file is incomplete: its feed does not match the copy that was saved. Copy it again.");
    }
    let mut scratch = RoomRuntime::from_persisted(PersistedRoomV1 {
        fallbacks: Default::default(),
        join_floors: None, join_floor_epoch: None, history_restarted: false, room_id,
        label: manifest.room.label.clone(), resume_on_restart: false, member_ids: Vec::new(),
        membership_revision: 1, brief_on_join: false, brief_template: None, briefed_member_ids: None,
    });
    let (mut epoch, mut tail, mut records) = (None, 0, 0u64);
    for (index, line) in log.split_inclusive(|byte| *byte == b'\n').enumerate() {
        let number = index + 2;
        if line.len() > room_log::MAX_RECORD_BYTES { return Err(damaged(number, "a feed record exceeds its size bound")); }
        if line.last() != Some(&b'\n') { bail!(INCOMPLETE); }
        let (record, header_room) = room_log::parse_record_line(line).map_err(|error| damaged(number, error))?;
        match (&record, epoch) {
            (ReplayRecord::Header(value), None) if header_room == Some(room_id) => epoch = Some(*value),
            (ReplayRecord::Event(event), Some(value)) => {
                if event.room_id != room_id || event.cursor.epoch != value || event.cursor.sequence != tail + 1
                    || event.schema_version != ROOM_EVENT_SCHEMA_VERSION {
                    return Err(damaged(number, "feed identity, schema or sequence mismatch"));
                }
                tail = event.cursor.sequence;
                records += 1;
            }
            _ => return Err(damaged(number, "the feed header is missing, repeated or names another room")),
        }
        scratch.apply_replay_record(record).map_err(|error| damaged(number, format!("{error:#}")))?;
    }
    let epoch = epoch.ok_or_else(|| anyhow!(INCOMPLETE))?;
    if (records, tail) != (manifest.feed.records, manifest.feed.last_sequence) {
        return Err(damaged(1, "the feed record count or last sequence does not match the feed"));
    }
    Ok(Package { manifest, log, epoch, tail, records })
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path).with_context(|| format!("Could not open {}", path.display()))?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "{} is not a file", path.display());
    if metadata.len() > PACKAGE_MAX_BYTES { bail!(TOO_LARGE); }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(PACKAGE_MAX_BYTES + 1).read_to_end(&mut bytes)
        .with_context(|| format!("Could not read {}", path.display()))?;
    if bytes.len() as u64 > PACKAGE_MAX_BYTES { bail!(TOO_LARGE); }
    Ok(bytes)
}

fn write_synced(temporary: &Path, target: &Path, bytes: &[u8]) -> Result<()> {
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(temporary, target)?;
        Ok(())
    })();
    if result.is_err() { let _ = fs::remove_file(temporary); }
    result
}

/// As `write_synced`, but `target` is never replaced (A16, an agent's save):
/// the completed, synced file is published by a hard link, which the file
/// system refuses when the name exists, then the temporary name goes. A file
/// system without hard links refuses the save.
fn write_synced_new(temporary: &Path, target: &Path, bytes: &[u8]) -> Result<()> {
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::hard_link(temporary, target).map_err(|error| match error.kind() {
            std::io::ErrorKind::AlreadyExists =>
                anyhow!("{} already exists; choose a new file name (an agent never replaces a room file)", target.display()),
            _ => error.into(),
        })
    })();
    let _ = fs::remove_file(temporary);
    result
}

/// Line 1 plus the log, with the log's completeness record. Save and the
/// runtime-copy reader both build packages here, so one parser checks both.
fn compose_package(room: &PersistedRoomV1, sessions: Vec<ManifestSession>, log: &[u8], last_sequence: RoomSequence) -> Result<Vec<u8>> {
    let manifest = Manifest {
        record: PACKAGE_RECORD.into(), version: PACKAGE_VERSION, exported_at: now_rfc3339(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        source_machine: std::env::var("COMPUTERNAME").ok().filter(|name| bounded_text(name, 64)),
        room: ManifestRoom {
            room_id: room.room_id, label: room.label.clone(), member_ids: room.member_ids.clone(),
            membership_revision: room.membership_revision, brief_on_join: room.brief_on_join,
            brief_template: room.brief_template.clone(), join_floor_epoch: room.join_floor_epoch,
            join_floors: room.join_floors.as_ref().map(|floors| floors.iter().map(|(id, floor)| (*id, *floor)).collect()),
            history_restarted: room.history_restarted,
        },
        sessions,
        feed: ManifestFeed { records: log_records(log), last_sequence, sha256: format!("{:x}", Sha256::digest(log)) },
    };
    let mut bytes = serde_json::to_vec(&manifest)?;
    bytes.push(b'\n');
    bytes.extend_from_slice(log);
    Ok(bytes)
}

/// Complete event records: every newline after the header's.
fn log_records(log: &[u8]) -> u64 {
    (log.iter().filter(|byte| **byte == b'\n').count() as u64).saturating_sub(1)
}

/// A copied runtime folder, read-only: its two catalogs, nothing else yet.
struct RuntimeCopy {
    dir: PathBuf,
    rooms: RoomCatalogV1,
    sessions: SessionCatalogV1,
}

fn read_catalog<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let file = File::open(path).with_context(|| format!("Could not open {}", path.display()))?;
    ensure!(file.metadata()?.len() <= CATALOG_MAX_BYTES, "{} is larger than a catalog can be", path.display());
    let mut bytes = Vec::new();
    file.take(CATALOG_MAX_BYTES + 1).read_to_end(&mut bytes)?;
    serde_json::from_slice(&bytes).with_context(|| format!("{} is not a PRIM-1 catalog", path.display()))
}

/// A copied log another process holds: its PRIM-1 is running.
#[derive(Debug)]
struct RunningCopy;
impl std::fmt::Display for RunningCopy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(RUNNING) }
}
impl std::error::Error for RunningCopy {}

/// Open a copied room log for reading only. A shared lock fails while a
/// running PRIM-1 holds the writer's exclusive lock; nothing is ever written.
fn open_copy_log(dir: &Path, room: &PersistedRoomV1) -> Result<File> {
    let path = dir.join("rooms").join(format!("{}.jsonl", room.room_id));
    // Plain words for the Rooms page: the copy-relative file name, no `\\?\`
    // prefix and no OS error number.
    let shown = format!(r"rooms\{}.jsonl", room.room_id);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound =>
            bail!("Room \"{}\" has no feed in the copy: {shown} is missing.", room.label),
        Err(error) => bail!("Room \"{}\": {shown} could not be opened ({}).", room.label, error.kind()),
    };
    match file.try_lock_shared() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(RunningCopy.into()),
        Err(TryLockError::Error(error)) => bail!("Room \"{}\": {shown} could not be read ({}).", room.label, error.kind()),
    }
    ensure!(file.metadata()?.len() <= room_log::MAX_LOG_BYTES, "{TOO_LARGE}");
    Ok(file)
}

/// Where each harness keeps its conversations on this machine. Probes take
/// the roots as parameters so tests use fixture stores only.
pub(crate) struct HarnessStores {
    pub(crate) claude: Option<PathBuf>,
    pub(crate) grok: Option<PathBuf>,
}

impl HarnessStores {
    fn from_env() -> Self {
        let home = std::env::var_os("USERPROFILE").map(PathBuf::from).filter(|path| path.is_absolute());
        Self {
            claude: std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).filter(|path| path.is_absolute())
                .or_else(|| home.as_ref().map(|home| home.join(".claude"))),
            grok: home.map(|home| home.join(".grok")),
        }
    }
}

enum Probe { Here, Elsewhere(String), Missing(String) }

fn plain_entry(path: &Path, directory: bool) -> bool {
    // symlink_metadata never follows a link; a link is neither file nor dir.
    fs::symlink_metadata(path).is_ok_and(|metadata| if directory { metadata.is_dir() } else { metadata.is_file() })
}

/// Read-only, exact names only, one level, at most STORE_SCAN_MAX_DIRS folders.
fn scan_store(root: Option<&Path>, store: &str, mut matches: impl FnMut(&str, &Path) -> Option<bool>) -> Probe {
    let Some(root) = root else { return Probe::Missing(format!("no {store} store is configured on this computer")); };
    let Ok(entries) = fs::read_dir(root) else {
        return Probe::Missing(format!("there is no {store} store at {}", root.display()));
    };
    let mut elsewhere = None;
    for (count, entry) in entries.enumerate() {
        if count >= STORE_SCAN_MAX_DIRS { return Probe::Missing(format!("the {store} store has more than {STORE_SCAN_MAX_DIRS} folders; not searched further")); }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) { continue; }
        let name = entry.file_name().to_string_lossy().into_owned();
        match matches(&name, &entry.path()) {
            Some(true) => return Probe::Here,
            Some(false) => { elsewhere.get_or_insert(name); }
            None => {}
        }
    }
    elsewhere.map_or_else(|| Probe::Missing(format!("its conversation is not in the {store} store on this computer")), Probe::Elsewhere)
}

/// Claude keeps `<config>\projects\<folder>\<id>.jsonl`, where the folder is
/// the working folder with every non-alphanumeric character replaced by '-'.
fn probe_claude(root: Option<&Path>, id: &str, folder: &str) -> Probe {
    let project: String = folder.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let file = format!("{id}.jsonl");
    let projects = root.map(|root| root.join("projects"));
    if projects.as_ref().is_some_and(|projects| plain_entry(&projects.join(&project).join(&file), false)) { return Probe::Here; }
    scan_store(projects.as_deref(), "Claude", |name, path| plain_entry(&path.join(&file), false).then(|| name == project))
}

/// Grok keeps `<home>\.grok\sessions\<percent-encoded folder>\<id>\`. Folder
/// names are decoded and compared, never built from ours.
fn probe_grok(root: Option<&Path>, id: &str, folder: &str) -> Probe {
    let wanted = session::normalize_windows_path(folder);
    let sessions = root.map(|root| root.join("sessions"));
    scan_store(sessions.as_deref(), "Grok", |name, path| plain_entry(&path.join(id), true)
        .then(|| percent_decode(name).is_some_and(|decoded| session::normalize_windows_path(&decoded) == wanted)))
}

fn percent_decode(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else { out.push(bytes[index]); index += 1; }
    }
    String::from_utf8(out).ok()
}

/// One member being loaded: its new identity and, once settled, its state.
struct Staged {
    source: ManifestSession,
    new_id: SessionId,
    directory: QualifiedWorkingDirectory,
    folder_note: Option<String>,
    probe: Option<Probe>,
    pending: RecoveryReference,
    state: RoomImportMemberState,
    harness_session_id: Option<String>,
    recovery: Option<RecoveryReference>,
    note: Option<String>,
}

impl Staged {
    /// Every conversation ID this member carries: its live ID and its hold's.
    fn conversation_ids(&self) -> impl Iterator<Item = &str> {
        let held = self.source.recovery.as_ref().and_then(|hold| hold.original_id.as_deref());
        self.source.harness_session_id.as_deref().into_iter()
            .chain(held.filter(|id| Some(*id) != self.source.harness_session_id.as_deref()))
    }

    fn hold(&self, original_id: Option<String>, provenance: &str, reason: String) -> RecoveryReference {
        RecoveryReference { original_id, provenance: provenance.into(), reason: bounded_reason(&reason), reported_codex: None }
    }

    /// Final state, most specific first. Only "resumes" keeps a live ID;
    /// every hold keeps the conversation ID as its original_id.
    fn settle(&mut self, in_use_by: Option<String>) {
        let live = self.source.harness_session_id.clone();
        // A saved hold is historical data: its provenance and reason are
        // kept exactly; "held in the saved room" is report context only.
        let (state, recovery) = if let Some(saved) = &self.source.recovery {
            (RoomImportMemberState::Held, Some(RecoveryReference { original_id: saved.original_id.clone().or(live.clone()),
                provenance: saved.provenance.clone(), reason: saved.reason.clone(), reported_codex: None }))
        } else if let Some(owner) = in_use_by {
            (RoomImportMemberState::Held, Some(self.hold(live.clone(), "room_import_in_use", format!(
                "This conversation is also used by {owner}. One session drives a conversation at a time, so this copy is held."))))
        } else if live.is_none() {
            (RoomImportMemberState::StartsFresh, None)
        } else if self.source.driver == DriverKind::Codex {
            (RoomImportMemberState::Held, Some(RecoveryReference::unresolved_codex(live.clone(), true)))
        } else if let Some(note) = &self.folder_note {
            (RoomImportMemberState::HistoryNeeded, Some(self.hold(live.clone(), "room_import_history_missing",
                format!("Resume needs the saved folder: {note}."))))
        } else {
            match self.probe.take() {
                Some(Probe::Here) => (RoomImportMemberState::Resumes, None),
                Some(Probe::Elsewhere(folder)) => (RoomImportMemberState::HistoryNeeded, Some(self.hold(live.clone(),
                    "room_import_history_missing", format!("Its conversation is stored under another folder ({}), not this member's folder.",
                        percent_decode(&folder).unwrap_or(folder))))),
                Some(Probe::Missing(why)) => (RoomImportMemberState::HistoryNeeded, Some(self.hold(live.clone(),
                    "room_import_history_missing", format!("History needed: {why}.")))),
                None => (RoomImportMemberState::HistoryNeeded, Some(self.hold(live.clone(),
                    "room_import_history_missing", "History needed: the conversation was not checked.".into()))),
            }
        };
        self.harness_session_id = (state == RoomImportMemberState::Resumes).then_some(live).flatten();
        self.note = match (&self.source.recovery, &recovery) {
            (Some(saved), _) => Some(format!("Held in the saved room ({}): {}", saved.provenance, saved.reason)),
            (None, hold) => hold.as_ref().map(|hold| hold.reason.clone()).or_else(|| self.folder_note.clone()),
        };
        self.state = state;
        self.recovery = recovery;
    }

    /// The catalog row saved before the room commit: held, no live ID.
    fn pending_row(&self) -> PersistedSessionV1 {
        PersistedSessionV1 { codex_transport: Default::default(),
            session_id: self.new_id, label: self.source.label.clone(), driver: self.source.driver,
            working_directory: self.directory.clone(), permission_profile: self.source.permission_profile,
            launch_choices: Some(self.source.launch_choices.clone()), last_launch: None,
            harness_session_id: None, recovery: Some(self.pending.clone()), recovery_history: Vec::new(), running_at_shutdown: false,
            last_pty_cols: None, last_pty_rows: None,
            // Grants are the operator's per runtime; a copied room carries none.
            permissions: MemberPermissions::default(),
            created_by: None,
        }
    }

    fn slot(&self) -> SessionSlot {
        let mut slot = closed_session_slot(SessionDefinition { codex_transport: Default::default(),
            session_id: self.new_id, alias: session_alias(self.new_id), label: self.source.label.clone(),
            driver: self.source.driver, working_dir: self.directory.canonical_path.clone(),
            permission_profile: self.source.permission_profile,
        });
        slot.launch_choices = self.source.launch_choices.clone();
        slot.qualified_working_directory = Some(self.directory.clone());
        slot.recovery = Some(self.pending.clone());
        slot
    }
}

/// Collisions with this runtime's live AND held conversation IDs, and among
/// the staged members themselves (Codex 612/620).
fn settle_members(staged: &mut [Staged], slots: &SessionRegistry, rooms: &RoomState) {
    let key = |driver: DriverKind, id: &str| (driver, Uuid::parse_str(id).map(|id| id.to_string()).unwrap_or_else(|_| id.to_owned()));
    let mut owners: HashMap<(DriverKind, String), String> = HashMap::new();
    for slot in slots.ordered_slots() {
        let place = rooms.room_by_session.get(&slot.session_id).and_then(|room| rooms.get(*room))
            .map_or_else(|| "in the lobby".to_owned(), |room| format!("in room \"{}\"", room.definition.label));
        let held = slot.recovery.as_ref().and_then(|hold| hold.original_id.as_deref());
        for id in slot.harness_session_id.as_deref().into_iter().chain(held) {
            owners.entry(key(slot.definition.driver, id))
                .or_insert_with(|| format!("\"{}\" {place}", slot.definition.label));
        }
    }
    let mut staged_owners: HashMap<(DriverKind, String), Vec<usize>> = HashMap::new();
    for (index, member) in staged.iter().enumerate() {
        for id in member.conversation_ids() {
            staged_owners.entry(key(member.source.driver, id)).or_default().push(index);
        }
    }
    for index in 0..staged.len() {
        let in_use = staged[index].conversation_ids().find_map(|id| {
            let id = key(staged[index].source.driver, id);
            owners.get(&id).cloned().or_else(|| staged_owners.get(&id)
                .and_then(|others| others.iter().find(|other| **other != index))
                .map(|other| format!("\"{}\" in the same room file", staged[*other].source.label)))
        });
        staged[index].settle(in_use);
    }
}

fn rewrite_event(event: &mut RoomFeedEvent, room_id: RoomId, epoch: Uuid, ids: &mut HashMap<SessionId, SessionId>) {
    // Former members get a fresh ID too, the same one throughout: an old line
    // can never credit a session that already lives in this runtime.
    let mut map = |id: &mut SessionId| *id = *ids.entry(*id).or_insert_with(Uuid::new_v4);
    event.room_id = room_id;
    event.cursor.epoch = epoch;
    match &mut event.item {
        RoomFeedItem::Card { author, .. } => map(author),
        RoomFeedItem::Message { sender, recipient_ids, .. } => {
            if let RoomMessageSender::Session { session_id } = sender { map(session_id); }
            recipient_ids.iter_mut().for_each(map);
        }
        RoomFeedItem::Membership { session_id, .. } => map(session_id),
        // run_id and generation stay as historical receipts of the source.
        RoomFeedItem::Delivery { recipient_id, .. } => map(recipient_id),
        // v3 W3b: attempt tokens follow the new feed epoch, like every cursor.
        RoomFeedItem::Waiting { recipient_id, attempt, .. } => {
            map(recipient_id);
            if let Some(attempt) = attempt { attempt.epoch = epoch; }
        }
        RoomFeedItem::InputObserved { recipient_id, attempt, .. } => {
            map(recipient_id);
            attempt.epoch = epoch;
        }
        // An imported recipe names no conversation here: it never resolves to
        // a local record, so nothing is reconciled against another runtime's.
        RoomFeedItem::SubmitAttempt { recipient_id, attempt, conversation_id, working_dir, .. } => {
            map(recipient_id);
            attempt.epoch = epoch;
            conversation_id.clear();
            working_dir.clear();
        }
        RoomFeedItem::Ack { recipient_id, actor, .. } => {
            map(recipient_id);
            if let shared_types::RoomAckActor::Session { session_id } = actor { map(session_id); }
        }
        RoomFeedItem::Cancel { actor, not_typed, reached, notified, .. } => {
            if let shared_types::RoomAckActor::Session { session_id } = actor { map(session_id); }
            not_typed.iter_mut().chain(reached.iter_mut()).chain(notified.iter_mut()).for_each(map);
        }
        // v3 W5: guest ids are not session ids; a package carries no guest
        // invitation, so a guest ACK is kept as the source's history.
        RoomFeedItem::GuestAck { .. } => {}
    }
}

struct PreparedImport {
    definition: PersistedRoomV1,
    log: Vec<u8>,
    staged: Vec<Staged>,
    warnings: Vec<String>,
}

#[cfg(test)]
thread_local! {
    /// Test seams by name ("held_saved", "room_saved"): each runs once; an Err
    /// simulates PRIM-1 stopping right there, so no cleanup runs.
    pub(crate) static SEAMS: std::cell::RefCell<HashMap<&'static str, Box<dyn FnOnce() -> Result<()>>>> =
        std::cell::RefCell::new(HashMap::new());
}

fn seam(name: &'static str) -> Result<()> {
    #[cfg(test)]
    if let Some(hook) = SEAMS.with(|seams| seams.borrow_mut().remove(name)) { return hook(); }
    let _ = name;
    Ok(())
}

enum Commit { Yes, No(anyhow::Error), Unknown(String) }

impl SupervisorHandle {
    /// Write one room to `<path>` (`*.prim1room`). The feed's committed prefix
    /// is copied through the writer's own handle under the rooms lock only;
    /// building, checking and writing the file happen after it is released.
    pub fn export_room(&self, room_id: RoomId, path: &Path) -> Result<RoomExportReport> {
        self.export_room_checked(room_id, path, None)
    }

    /// A16: `check` (an agent's grant and scope) runs under the rooms lock
    /// the feed is copied with, so the file holds what the caller could read
    /// at that moment. With a check (an agent), an existing file is never
    /// replaced, even one created while the room was being written; the
    /// operator's save replaces after its dialog asked.
    pub(crate) fn export_room_checked(&self, room_id: RoomId, path: &Path,
        check: Option<&dyn Fn(&RoomState, &SessionRegistry) -> Result<()>>) -> Result<RoomExportReport> {
        let replace = check.is_none();
        self.ensure_active()?;
        ensure!(path.is_absolute(), "Choose a full path for the room file.");
        ensure!(path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("prim1room")),
            "A room file name must end in .prim1room.");
        ensure!(path.parent().is_some_and(Path::is_dir), "The folder for {} does not exist.", path.display());
        let (definition, sessions, log, last_sequence, paused, history_dropped) = {
            let _publish = self.inner.room_event_publish.lock();
            let label = self.inner.rooms.lock().get(room_id).map(|room| room.definition.label.clone())
                .ok_or_else(|| anyhow!("unknown room id '{room_id}'"))?;
            let needs_recovery = |error: anyhow::Error| anyhow!("Room \"{label}\" needs history recovery before it can be saved: {error:#}");
            self.recover_room_writer(room_id).map_err(needs_recovery)?;
            let mut rooms = self.inner.rooms.lock();
            if let Some(check) = check {
                check(&rooms, &self.inner.slots.lock())?;
            }
            let room = rooms.get_mut(room_id).ok_or_else(|| anyhow!("unknown room id '{room_id}'"))?;
            let snapshot = room.snapshot();
            if let Some(reason) = snapshot.recovery_error.clone()
                .or_else(|| snapshot.writer_recovery.as_ref().map(|hold| hold.reason.clone()))
                .or_else(|| snapshot.history_replay_pending.then(|| "a replay is pending".into())) {
                return Err(needs_recovery(anyhow!(reason)));
            }
            let definition = room.definition.clone();
            let (sessions, history_dropped) = {
                let slots = self.inner.slots.lock();
                let mut dropped = 0;
                let sessions = definition.member_ids.iter().map(|id| {
                    let slot = slots.get_by_id(*id).ok_or_else(|| anyhow!("member '{id}' is missing"))?;
                    dropped += usize::from(!slot.recovery_history.is_empty());
                    export_session(slot)
                }).collect::<Result<Vec<_>>>().map_err(|error| anyhow!("Room \"{label}\" was not saved: {error:#}"))?;
                (sessions, dropped)
            }; // Slots are released before the history copy (Codex 620).
            let started = Instant::now();
            let copied = room.read_committed_log();
            let last_sequence = room.next_sequence - 1;
            let paused = started.elapsed();
            drop(rooms);
            let log = copied.map_err(|error| {
                // A failed position restore holds the writer; say so now.
                self.notify_room_history_transition(room_id);
                anyhow!("Room \"{label}\" was not saved: {error:#}")
            })?;
            (definition, sessions, log, last_sequence, paused, history_dropped)
        };
        let label = definition.label.clone();
        let bytes = compose_package(&definition, sessions, &log, last_sequence)?;
        drop(log);
        let records = parse_package(&bytes)
            .map_err(|error| anyhow!("Room \"{label}\" was not saved: the copy failed its own check: {error:#}"))?.records;
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let temporary = path.with_file_name(format!("{name}.{}.tmp", Uuid::new_v4()));
        (if replace { write_synced(&temporary, path, &bytes) } else { write_synced_new(&temporary, path, &bytes) })
            .map_err(|error| anyhow!("Room \"{label}\" was not saved: could not write {}: {error:#}", path.display()))?;
        let mut warnings = Vec::new();
        if history_dropped > 0 {
            warnings.push(format!("Only each member's current hold is saved; the earlier recovery history of {history_dropped} member(s) stays in this PRIM-1."));
        }
        Ok(RoomExportReport { path: path.display().to_string(), label, members: definition.member_ids.len(),
            records, bytes: bytes.len() as u64, paused_ms: paused.as_millis() as u64, warnings })
    }

    /// What a file or a copied runtime offers: one entry for a `.prim1room`,
    /// one per room for a runtime copy. Reads only.
    pub fn inspect_room_source(&self, path: &Path) -> Result<Vec<RoomSourceSummary>> {
        self.ensure_active()?;
        if !is_runtime_copy(path) {
            let bytes = read_bounded(path)?;
            let package = parse_package(&bytes)?;
            return Ok(vec![RoomSourceSummary { source_room_id: package.manifest.room.room_id,
                label: package.manifest.room.label, members: package.manifest.sessions.len(), records: package.records,
                unavailable: None }]);
        }
        let copy = self.open_runtime_copy(path)?;
        copy.rooms.rooms.iter().map(|room| {
            // Count complete records in bounded chunks; a torn tail is not one.
            let counted = (|| -> Result<u64> {
                let mut file = open_copy_log(&copy.dir, room)?;
                let (mut buffer, mut newlines) = (vec![0u8; 64 * 1024], 0u64);
                loop {
                    let read = file.read(&mut buffer)?;
                    if read == 0 { break; }
                    newlines += buffer[..read].iter().filter(|byte| **byte == b'\n').count() as u64;
                }
                Ok(newlines.saturating_sub(1))
            })();
            let (records, unavailable) = match counted {
                Ok(records) => (records, None),
                // A held log means that PRIM-1 is running: the whole copy is refused.
                Err(error) if error.is::<RunningCopy>() => return Err(error),
                // Any other unreadable log blocks only its own room.
                Err(error) => (0, Some(format!("{error:#}"))),
            };
            Ok(RoomSourceSummary { source_room_id: room.room_id, label: room.label.clone(),
                members: room.member_ids.len(), records, unavailable })
        }).collect()
    }

    /// Load a room as a new room with new members. Nothing starts. `path` is
    /// a `.prim1room` file or a copied runtime's `room-catalog-v1.json`;
    /// `source_room_id` picks the room (required when a copy has several).
    pub fn import_room(&self, path: &Path, source_room_id: Option<RoomId>) -> Result<RoomImportReport> {
        self.import_room_with_stores(path, source_room_id, &HarnessStores::from_env())
    }

    /// A16: `check` (an agent's grant) runs under the locks the first member
    /// copy is saved with. An `Err` that is a `PartialEffect` left listed
    /// member copies behind; any other `Err` changed nothing listed.
    pub(crate) fn import_room_checked(&self, path: &Path, source_room_id: Option<RoomId>,
        check: Option<&dyn Fn(&RoomState, &SessionRegistry) -> Result<()>>) -> Result<RoomImportReport> {
        self.import_room_with_stores_checked(path, source_room_id, &HarnessStores::from_env(), check)
    }

    pub(crate) fn import_room_with_stores(&self, path: &Path, source_room_id: Option<RoomId>, stores: &HarnessStores) -> Result<RoomImportReport> {
        self.import_room_with_stores_checked(path, source_room_id, stores, None)
    }

    fn import_room_with_stores_checked(&self, path: &Path, source_room_id: Option<RoomId>, stores: &HarnessStores,
        check: Option<&dyn Fn(&RoomState, &SessionRegistry) -> Result<()>>) -> Result<RoomImportReport> {
        self.ensure_active()?;
        let prepared = self.read_room_source(path, source_room_id)
            .and_then(|(bytes, warnings)| self.prepare_import(&bytes, source_room_id, warnings, stores))
            .map_err(|error| anyhow!("{error:#} Nothing was changed."))?;
        self.commit_import(prepared, check)
    }

    /// The package bytes to load, plus anything the reader left out.
    fn read_room_source(&self, path: &Path, source_room_id: Option<RoomId>) -> Result<(Vec<u8>, Vec<String>)> {
        if !is_runtime_copy(path) { return Ok((read_bounded(path)?, Vec::new())); }
        let copy = self.open_runtime_copy(path)?;
        let room = match source_room_id {
            Some(id) => copy.rooms.rooms.iter().find(|room| room.room_id == id)
                .ok_or_else(|| anyhow!("That room is not in this runtime copy."))?,
            None if copy.rooms.rooms.len() == 1 => &copy.rooms.rooms[0],
            None => bail!("This runtime copy has {} rooms; choose one.", copy.rooms.rooms.len()),
        };
        let mut log = Vec::new();
        open_copy_log(&copy.dir, room)?.take(room_log::MAX_LOG_BYTES + 1).read_to_end(&mut log)?;
        // A torn last record is left out of the load and reported; the copy
        // itself is never repaired or otherwise changed.
        let complete = log.iter().rposition(|byte| *byte == b'\n').map_or(0, |index| index + 1);
        let torn = log.len() - complete;
        log.truncate(complete);
        let mut warnings = Vec::new();
        if torn > 0 {
            warnings.push(format!("The copy's last feed record was incomplete ({torn} bytes) and was left out; the copy was not changed."));
        }
        let mut dropped = 0;
        let sessions = room.member_ids.iter().map(|id| {
            let row = copy.sessions.sessions.iter().find(|row| row.session_id == *id)
                .ok_or_else(|| anyhow!("Member {id} of room \"{}\" is missing from the copy's session catalog.", room.label))?;
            dropped += usize::from(!row.recovery_history.is_empty());
            let choices = row.launch_choices.clone().unwrap_or_else(|| session::default_launch_choices(row.driver));
            manifest_session(row.session_id, &row.label, row.driver, &row.working_directory, row.permission_profile,
                choices, row.harness_session_id.as_deref(), row.recovery.as_ref())
        }).collect::<Result<Vec<_>>>()?;
        if dropped > 0 {
            warnings.push(format!("Only each member's current hold is loaded; the earlier recovery history of {dropped} member(s) stays in the copy."));
        }
        let records = log_records(&log);
        Ok((compose_package(room, sessions, &log, records)?, warnings))
    }

    /// Read-only. Never this app's own runtime: Save… is the path for that.
    fn open_runtime_copy(&self, catalog: &Path) -> Result<RuntimeCopy> {
        let dir = catalog.parent().ok_or_else(|| anyhow!("Choose the room-catalog-v1.json inside a copied runtime folder."))?;
        let theirs = fs::canonicalize(dir).with_context(|| format!("Could not open {}", dir.display()))?;
        let own = fs::canonicalize(&self.inner.runtime_dir)?;
        ensure!(comparable_storage_path(&theirs) != comparable_storage_path(&own),
            "That folder is this PRIM-1's own runtime. Use Save… instead.");
        let rooms: RoomCatalogV1 = read_catalog(catalog)?;
        let sessions: SessionCatalogV1 = read_catalog(&dir.join(SESSION_CATALOG_FILE_NAME))?;
        ensure!(rooms.schema_version == rooms::ROOM_CATALOG_SCHEMA_VERSION && sessions.schema_version == SESSION_CATALOG_SCHEMA_VERSION,
            "This runtime copy uses a catalog version this build does not read.");
        ensure!(rooms.rooms.len() <= ROOM_MAX_COUNT, "This runtime copy lists more than {ROOM_MAX_COUNT} rooms.");
        // A duplicated identity has no single definition; never pick the first.
        let mut seen = HashSet::new();
        if let Some(room) = rooms.rooms.iter().find(|room| !seen.insert(room.room_id)) {
            bail!("This runtime copy lists room {} more than once, so it cannot be read.", room.room_id);
        }
        let mut seen = HashSet::new();
        if let Some(row) = sessions.sessions.iter().find(|row| !seen.insert(row.session_id)) {
            bail!("This runtime copy lists session {} more than once, so it cannot be read.", row.session_id);
        }
        Ok(RuntimeCopy { dir: theirs, rooms, sessions })
    }

    /// Validation, rewriting and read-only probes. Writes nothing.
    fn prepare_import(&self, bytes: &[u8], source_room_id: Option<RoomId>, mut warnings: Vec<String>, stores: &HarnessStores) -> Result<PreparedImport> {
        let package = parse_package(bytes)?;
        let manifest = &package.manifest;
        ensure!(source_room_id.is_none_or(|id| id == manifest.room.room_id), "That room is not in this file.");
        ensure!(self.inner.rooms.lock().order.len() < ROOM_MAX_COUNT,
            "PRIM-1 already has {ROOM_MAX_COUNT} rooms; delete one before loading.");
        let (room_id, epoch) = (Uuid::new_v4(), Uuid::new_v4());
        let workspace = self.inner.catalog.lock().workspace_preference.clone();
        let mut ids = HashMap::new();
        let mut staged = Vec::new();
        for member in &manifest.room.member_ids {
            let source = manifest.sessions.iter().find(|session| session.session_id == *member)
                .expect("validated member details").clone();
            if source.driver == DriverKind::Prime {
                warnings.push(format!("Prime member \"{}\" was left out: Prime is parked in this PRIM-1.", source.label));
                continue;
            }
            let new_id = Uuid::new_v4();
            ids.insert(source.session_id, new_id);
            let saved = &source.working_directory.canonical_path;
            let (directory, folder_note) = match qualify_working_directory(Path::new(saved), &self.inner.runtime_dir) {
                Ok((directory, _lease)) => (directory, None),
                Err(error) => {
                    revalidate_qualified_directory("workspace preference", &workspace, &self.inner.runtime_dir)?;
                    (workspace.clone(), Some(format!("{saved} is not available here ({error:#}); this member uses the workspace {}",
                        workspace.canonical_path)))
                }
            };
            // Probe only a member that could resume: a live ID, no hold.
            let probe = match (&source.harness_session_id, &source.recovery, &folder_note) {
                (Some(id), None, None) => match source.driver {
                    DriverKind::Claude => Some(probe_claude(stores.claude.as_deref(), id, &directory.canonical_path)),
                    DriverKind::Grok => Some(probe_grok(stores.grok.as_deref(), id, &directory.canonical_path)),
                    _ => None,
                },
                _ => None,
            };
            let pending = RecoveryReference {
                original_id: source.harness_session_id.clone()
                    .or_else(|| source.recovery.as_ref().and_then(|hold| hold.original_id.clone())),
                provenance: IMPORT_PENDING.into(),
                reason: format!("PRIM-1 stopped while loading room \"{}\" ({room_id}). This copy stays held: if that room is listed, choose Start fresh session or load the room again; if it is not, delete this session. No conversation was changed.",
                    manifest.room.label),
                reported_codex: None,
            };
            staged.push(Staged { source, new_id, directory, folder_note, probe, pending,
                state: RoomImportMemberState::Held, harness_session_id: None, recovery: None, note: None });
        }
        // Floors are kept only when they verifiably belong to this exact log:
        // the saved floor epoch is its header epoch, every member has one, and
        // each is within the log. Anything else, including a genuine v1 room
        // without floors, gets the conservative tail rule: loaded members see
        // only traffic after the load. That is a visibility change, not the
        // startup behaviour (which treats absent v1 floors as zero); the
        // operator still reads the complete feed.
        let saved_floors = manifest.room.join_floors.as_ref().filter(|floors|
            manifest.room.join_floor_epoch == Some(package.epoch)
                && floors.len() == manifest.room.member_ids.len()
                && manifest.room.member_ids.iter().all(|id| floors.get(id).is_some_and(|floor| *floor <= package.tail)));
        if saved_floors.is_none() && package.tail > 0 && !staged.is_empty() {
            warnings.push("The members' history boundaries could not be verified, so loaded members see only messages posted after this load. You still see the whole feed.".into());
        }
        let join_floors = staged.iter().map(|member| (member.new_id,
            saved_floors.map_or(package.tail, |floors| floors[&member.source.session_id]))).collect();
        let mut log = Vec::with_capacity(package.log.len() + 128);
        log.extend(room_log::encode_record_line(room_id, &ReplayRecord::Header(epoch))?);
        for line in package.log.split_inclusive(|byte| *byte == b'\n').skip(1) {
            let (ReplayRecord::Event(mut event), _) = room_log::parse_record_line(line)? else { bail!("unexpected feed header") };
            rewrite_event(&mut event, room_id, epoch, &mut ids);
            log.extend(room_log::encode_record_line(room_id, &ReplayRecord::Event(event))?);
        }
        ensure!((log.len() as u64) < room_log::ORDINARY_LOG_BYTES,
            "The room's history is too large to take new messages here.");
        let definition = PersistedRoomV1 {
            fallbacks: Default::default(),
            join_floors: Some(join_floors), join_floor_epoch: Some(epoch),
            history_restarted: manifest.room.history_restarted, room_id, label: manifest.room.label.clone(),
            resume_on_restart: false, member_ids: staged.iter().map(|member| member.new_id).collect(),
            membership_revision: manifest.room.membership_revision, brief_on_join: manifest.room.brief_on_join,
            brief_template: normalize_room_brief_template(manifest.room.brief_template.clone())?,
            briefed_member_ids: Some(Vec::new()),
        };
        Ok(PreparedImport { definition, log, staged, warnings })
    }

    /// The failure happened before the room existed: remove its log copy.
    fn abandon_import(&self, room_id: RoomId, error: anyhow::Error) -> anyhow::Error {
        match room_log::remove_room_files(&self.inner.runtime_dir, room_id) {
            Ok(()) => anyhow!("Loading failed: {error:#}. Nothing was changed."),
            Err(cleanup) => anyhow!("Loading failed: {error:#}. An unused feed copy remains at rooms\\{room_id}.jsonl ({cleanup:#}); it is not listed anywhere."),
        }
    }

    fn commit_import(&self, prepared: PreparedImport,
        check: Option<&dyn Fn(&RoomState, &SessionRegistry) -> Result<()>>) -> Result<RoomImportReport> {
        let PreparedImport { definition, log, mut staged, mut warnings } = prepared;
        let room_id = definition.room_id;
        let runtime_dir = self.inner.runtime_dir.clone();
        // The log copy, then a replay of it that takes its writer lock: a
        // second full validation, outside every registry lock.
        let directory = runtime_dir.join("rooms");
        let mut room = RoomRuntime::from_persisted(definition);
        let opened = fs::create_dir_all(&directory).map_err(anyhow::Error::from)
            .and_then(|()| write_synced(&directory.join(format!(".{room_id}.import.tmp")),
                &directory.join(format!("{room_id}.jsonl")), &log))
            .and_then(|()| room.open_log(&runtime_dir));
        drop(log);
        if let Err(error) = opened { drop(room); return Err(self.abandon_import(room_id, error)); }

        let _publish = self.inner.room_event_publish.lock();
        let mut rooms = self.inner.rooms.lock();
        let admitted = self.ensure_active().and_then(|()| {
            ensure!(rooms.order.len() < ROOM_MAX_COUNT, "PRIM-1 already has {ROOM_MAX_COUNT} rooms; delete one before loading");
            Ok(())
        });
        if let Err(error) = admitted { drop(rooms); drop(room); return Err(self.abandon_import(room_id, error)); }
        let label = room.definition.label.clone();
        if rooms.catalog.rooms.iter().any(|other| other.label == label) {
            let suffix = " (loaded)";
            let base: String = label.chars().take(ROOM_LABEL_MAX_CHARS - suffix.len()).collect();
            room.definition.label = format!("{}{suffix}", base.trim_end());
            warnings.push(format!("A room named \"{label}\" already exists, so this one is \"{}\".", room.definition.label));
        }

        // 1. Every imported session is saved HELD before the room exists.
        let held = {
            let mut slots = self.inner.slots.lock();
            let mut catalog = self.inner.catalog.lock();
            settle_members(&mut staged, &slots, &rooms);
            let mut candidate = catalog.clone();
            let mut new_slots = Vec::with_capacity(staged.len());
            // A16: an agent's grant, under the locks of the first write.
            let admitted = check.map_or(Ok(()), |check| check(&rooms, &slots));
            let result = admitted.and_then(|()| staged.iter().try_for_each(|member| {
                let slot = member.slot();
                slots.validate_insert(&slot)?;
                candidate.sessions.push(member.pending_row());
                new_slots.push(slot);
                Ok::<(), anyhow::Error>(())
            })).and_then(|()| self.persist_catalog_candidate(&candidate));
            if result.is_ok() {
                for slot in new_slots {
                    self.inner.run_event_publish.lock().insert(slot.session_id, RunEventPublishState::new(1));
                    slots.insert_prevalidated(slot);
                }
                *catalog = candidate;
            }
            result
        }; // Slots are released before the room-catalog I/O (Codex 620).
        if let Err(error) = held { drop(rooms); drop(room); return Err(self.abandon_import(room_id, error)); }
        seam("held_saved")?;

        // 2. The room-catalog commit, confirmed from disk when the save errs.
        let mut room_candidate = rooms.catalog.clone();
        room_candidate.rooms.push(room.definition.clone());
        let commit = match self.persist_room_catalog_candidate(&room_candidate) {
            Ok(()) => Commit::Yes,
            Err(error) => match read_room_catalog(&room_catalog_path(&runtime_dir)) {
                Ok(disk) if disk.rooms.iter().any(|row| row.room_id == room_id) => Commit::Yes,
                Ok(_) => Commit::No(error),
                Err(read) => Commit::Unknown(format!("{error:#}; reading it back failed: {read:#}")),
            },
        };
        if let Commit::No(error) = commit {
            let ids: HashSet<_> = staged.iter().map(|member| member.new_id).collect();
            let undone = {
                let mut slots = self.inner.slots.lock();
                let mut catalog = self.inner.catalog.lock();
                let mut candidate = catalog.clone();
                candidate.sessions.retain(|row| !ids.contains(&row.session_id));
                self.persist_catalog_candidate(&candidate).map(|()| {
                    for id in &ids {
                        slots.remove(*id);
                        self.inner.run_event_publish.lock().remove(id);
                    }
                    *catalog = candidate;
                })
            };
            drop(rooms);
            drop(room);
            return Err(match undone {
                Ok(()) => self.abandon_import(room_id, error),
                Err(undo) => {
                    let _ = room_log::remove_room_files(&runtime_dir, room_id);
                    self.emit_imported_sessions(&ids);
                    // Listed copies remain: a retry must not load again.
                    member_lifecycle::PartialEffect(format!("Loading failed: {error:#}. PRIM-1 could not remove the {} member copies it had added ({undo:#}); they stay in the lobby, held, and resume nothing. Delete them. No conversation was changed.", ids.len())).into()
                }
            });
        }
        if let Commit::Unknown(why) = &commit {
            warnings.push(format!("PRIM-1 could not confirm that this room was saved ({why}). Its members stay held. If the room is missing after a restart, delete their held copies from the lobby."));
        }
        seam("room_saved")?;

        // 3. Release each pending hold into the member's honest state.
        let snapshots = {
            let mut slots = self.inner.slots.lock();
            let mut catalog = self.inner.catalog.lock();
            let release = matches!(commit, Commit::Yes).then(|| {
                let mut candidate = catalog.clone();
                for member in &staged {
                    let still_pending = slots.get_by_id(member.new_id).is_some_and(|slot| slot.recovery.as_ref() == Some(&member.pending));
                    if let Some(row) = candidate.sessions.iter_mut().find(|row| row.session_id == member.new_id && still_pending) {
                        row.harness_session_id = member.harness_session_id.clone();
                        row.recovery = member.recovery.clone();
                    }
                }
                self.persist_catalog_candidate(&candidate).map(|()| candidate)
            });
            match release {
                Some(Ok(candidate)) => {
                    for member in &staged {
                        if let Some(slot) = slots.get_by_id_mut(member.new_id).filter(|slot| slot.recovery.as_ref() == Some(&member.pending)) {
                            slot.harness_session_id = member.harness_session_id.clone();
                            slot.recovery = member.recovery.clone();
                        }
                    }
                    *catalog = candidate;
                }
                Some(Err(error)) => warnings.push(format!("The members' resume states could not be saved ({error:#}), so every member stays held. Choose Start fresh session on a member, or delete this room and load the file again.")),
                None => {}
            }
            staged.iter().filter_map(|member| slots.get_by_id(member.new_id).map(SessionSlot::snapshot)).collect::<Vec<_>>()
        };
        for member in &mut staged {
            if snapshots.iter().any(|snapshot| snapshot.session_id == member.new_id
                && snapshot.recovery.as_ref().is_some_and(|hold| hold.provenance == IMPORT_PENDING)) {
                member.state = RoomImportMemberState::Held;
                member.note = Some(member.pending.reason.clone());
            }
        }
        // The resume check sees only this runtime: the PRIM-1 the room came
        // from may still drive these conversations. Say so on screen.
        let resuming = staged.iter_mut().filter(|member| member.state == RoomImportMemberState::Resumes)
            .map(|member| member.note = Some("close it in the PRIM-1 it came from first".into())).count();
        if resuming == 1 {
            warnings.push("1 member resumes a conversation that the PRIM-1 this room came from may still be running. Close it there before you wake it here, so no agent runs in two PRIM-1s.".into());
        } else if resuming > 1 {
            warnings.push(format!("{resuming} members resume conversations that the PRIM-1 this room came from may still be running. Close them there before you wake them here, so no agent runs in two PRIM-1s."));
        }
        for member in &staged { rooms.room_by_session.insert(member.new_id, room_id); }
        let room_snapshot = room.snapshot();
        rooms.order.push(room_id);
        rooms.by_id.insert(room_id, room);
        rooms.catalog = room_candidate;
        drop(rooms);
        drop(_publish);
        for session in snapshots {
            self.emit(RuntimeEvent::SessionCreated { schema_version: 1, session, timestamp: now_rfc3339() });
        }
        self.emit(RuntimeEvent::RoomCreated { schema_version: ROOM_EVENT_SCHEMA_VERSION, room: room_snapshot.clone(), timestamp: now_rfc3339() });
        Ok(RoomImportReport {
            room: room_snapshot,
            members: staged.into_iter().map(|member| RoomImportMember {
                session_id: member.new_id, label: member.source.label, driver: member.source.driver,
                permission_profile: member.source.permission_profile, state: member.state, note: member.note,
            }).collect(),
            warnings,
        })
    }

    fn emit_imported_sessions(&self, ids: &HashSet<SessionId>) {
        let snapshots: Vec<_> = {
            let slots = self.inner.slots.lock();
            ids.iter().filter_map(|id| slots.get_by_id(*id).map(SessionSlot::snapshot)).collect()
        };
        for session in snapshots {
            self.emit(RuntimeEvent::SessionCreated { schema_version: 1, session, timestamp: now_rfc3339() });
        }
    }
}

fn export_session(slot: &SessionSlot) -> Result<ManifestSession> {
    let directory = slot.qualified_working_directory.as_ref()
        .ok_or_else(|| anyhow!("member \"{}\" has no qualified working folder", slot.definition.label))?;
    let definition = &slot.definition;
    manifest_session(slot.session_id, &definition.label, definition.driver, directory, definition.permission_profile,
        slot.launch_choices.clone(), slot.harness_session_id.as_deref(), slot.recovery.as_ref())
}

/// One member as saved. A conversation ID PRIM-1 cannot check (not a UUID)
/// travels only as a held reference, never as a resumable one.
#[allow(clippy::too_many_arguments)]
fn manifest_session(session_id: SessionId, label: &str, driver: DriverKind, directory: &QualifiedWorkingDirectory,
    permission_profile: PermissionProfile, launch_choices: SessionLaunchChoices, live: Option<&str>,
    hold: Option<&RecoveryReference>) -> Result<ManifestSession> {
    let conversation = !matches!(driver, DriverKind::GenericTerminal | DriverKind::Prime);
    let harness = live.filter(|_| conversation);
    let checked = harness.map(|id| Uuid::parse_str(id).ok().filter(|uuid| !uuid.is_nil()).map(|uuid| uuid.to_string()).ok_or(id));
    let mut recovery = hold.filter(|_| conversation).map(RecoveryReference::snapshot);
    if let Some(hold) = recovery.as_mut() {
        // The hold carries the conversation identity (Codex 620).
        hold.original_id = hold.original_id.take().or_else(|| harness.map(str::to_owned));
        ensure!(hold.reason.chars().count() <= HOLD_REASON_MAX_CHARS,
            "Member \"{label}\" has a hold reason longer than {HOLD_REASON_MAX_CHARS} characters, which a room file cannot carry unchanged.");
    }
    let harness_session_id = match checked {
        Some(Ok(id)) => Some(id),
        Some(Err(raw)) => {
            recovery.get_or_insert_with(|| SessionRecovery { original_id: Some(raw.to_owned()), provenance: "unchecked_conversation_id".into(),
                reason: "This conversation ID is not a UUID, so PRIM-1 cannot check it; it is kept as a held reference.".into() });
            None
        }
        None => None,
    };
    Ok(ManifestSession {
        session_id, label: label.to_owned(), driver,
        working_directory: ManifestDirectory { namespace: directory.namespace, canonical_path: directory.canonical_path.clone() },
        permission_profile, launch_choices, harness_session_id, recovery,
    })
}

fn is_runtime_copy(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name.eq_ignore_ascii_case(ROOM_CATALOG_FILE_NAME))
}
