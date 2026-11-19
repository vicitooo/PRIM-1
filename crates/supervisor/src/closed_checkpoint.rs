//! Operator-only, Windows-first capture of an already closed desktop scratch
//! runtime. No supervisor, process, task, WebView or transport is started here.
//! Limits below bound this first maintenance slice, not application capacity.
//! Task-folder and personal-repository choices are preserved without opening
//! their external stores. The source retains its install-id for a same-runtime
//! upgrade; a separately activated copy must not inherit that writing identity,
//! so install-id is captured only as evidence, never in the replay runtime.
use crate::*;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom};

const MAX_FILES: usize = 512;
const MAX_PATH_BYTES: usize = 1024;
const CATALOG_MAX: u64 = 16 * 1024 * 1024;
const EVIDENCE_FILE_MAX: u64 = 128 * 1024 * 1024;
const EVIDENCE_MAX: u64 = 256 * 1024 * 1024;
const CAPTURE_MAX: u64 = 1024 * 1024 * 1024;
const ARTIFACT_MAX: u64 = 512 * 1024 * 1024;
const MANIFEST_MAX: usize = 2 * 1024 * 1024;
const GUEST_ENDPOINT_MAX: u64 = 64 * 1024;
// Matches task_sideband's setting reader; the copy never opens its target.
const TASK_FOLDER_SETTING_MAX: u64 = 64 * 1024;
// W1 session_ledger reader bounds. Keep metadata byte-exact; do not initialize
// its writer/identity or resolve the external personal repository during replay.
const PERSONAL_REPO_SETTING_MAX: u64 = 64 * 1024;
const INSTALL_ID_MAX: u64 = 1024;
const DESKTOP_LOCK: &str = "desktop-instance.lock";

/// Hashes are operator-supplied references to external closure/build evidence,
/// not proof manufactured by this copier. No source shutdown is requested.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClosedCheckpointProvenance {
    pub closure_receipt_sha256: String,
    pub old_executable_sha256: String,
    pub candidate_executable_sha256: String,
    pub reviewed_source_revision: String,
    pub build_receipt_sha256: String,
}

pub struct ClosedCheckpointRequest {
    pub source_runtime: PathBuf,
    /// Existing parent, absent final component, ending in attempt_id.
    pub attempt_root: PathBuf,
    pub attempt_id: Uuid,
    pub provenance: ClosedCheckpointProvenance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointRole {
    State,
    Evidence,
    Artifact,
    Ownership,
    Directory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointEntry {
    pub path: String,
    pub directory: bool,
    pub role: CheckpointRole,
    pub bytes: u64,
    pub sha256: Option<String>,
    /// Relative to the attempt; source names are preserved within each area.
    pub captured_path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointChange {
    pub path: String,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointCompatibility {
    pub copy_replay_ok: bool,
    pub catalog_error: Option<String>,
    pub rooms: Vec<RoomSnapshot>,
    pub session_recovery_holds: Vec<SessionId>,
    /// Differences derived by existing constructors but not persisted by this
    /// validator. File hashes alone cannot expose these startup decisions.
    pub in_memory_normalizations: Vec<String>,
    pub changes: Vec<CheckpointChange>,
    pub holds: Vec<String>,
    pub settings_preservation_verified: bool,
    pub restart_authorized: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClosedCheckpointManifest {
    pub schema_version: u32,
    pub attempt_id: Uuid,
    pub captured_at: String,
    pub source_runtime: PathBuf,
    pub provenance: ClosedCheckpointProvenance,
    pub entries: Vec<CheckpointEntry>,
    pub compatibility: CheckpointCompatibility,
    pub limitations: Vec<String>,
}

/// Capture only a desktop-backed scratch runtime with its existing instance
/// lock. The caller supplies closure evidence; locks verify current ownership
/// exclusion, not successful earlier shutdown or complete persisted state.
///
/// No existing path is overwritten or removed. Errors after destination
/// creation leave an incomplete attempt without checkpoint-v1.json. Success
/// still authorizes no launch, promotion, resume or rollback.
pub fn prepare_closed_checkpoint(
    request: ClosedCheckpointRequest,
) -> Result<ClosedCheckpointManifest> {
    #[cfg(windows)]
    {
        capture(request, &known_local_data()?, |_| Ok(()))
    }
    #[cfg(not(windows))]
    {
        let _ = request;
        bail!("closed checkpoint requires Windows ownership semantics");
    }
}

#[derive(Clone, Copy)]
enum Stage {
    Locked,
    CopyChunk,
    BeforeCommit,
}

#[cfg(windows)]
fn known_local_data() -> Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{
        System::Com::CoTaskMemFree,
        UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath},
    };
    let mut value = std::ptr::null_mut();
    // Flags=0 neither creates the folder nor trusts environment overrides.
    let result = unsafe {
        SHGetKnownFolderPath(&FOLDERID_LocalAppData, 0, std::ptr::null_mut(), &mut value)
    };
    let path = if result >= 0 && !value.is_null() {
        let mut length = 0;
        while length < 32768 && unsafe { *value.add(length) } != 0 {
            length += 1;
        }
        if length == 32768 {
            Err(anyhow!("known local folder exceeds path bound"))
        } else {
            Ok(PathBuf::from(std::ffi::OsString::from_wide(unsafe {
                std::slice::from_raw_parts(value, length)
            })))
        }
    } else {
        Err(anyhow!(
            "cannot resolve Windows local data folder: HRESULT {result:#x}"
        ))
    };
    if !value.is_null() {
        unsafe {
            CoTaskMemFree(value.cast());
        }
    }
    path
}

// Existing read handles deny both writes and delete/replace on Windows. This
// closes the catalog gap left by desktop/log advisory locks. Holding directory
// handles also prevents renaming the captured tree; newly added entries are
// detected by full inventory comparison. This is cooperative maintenance, not
// a defense against a hostile same-user filesystem actor.
fn open_source(path: &Path, directory: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        options.share_mode(if directory {
            FILE_SHARE_READ | FILE_SHARE_WRITE
        } else {
            FILE_SHARE_READ
        });
        if directory {
            options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
        }
    }
    let file = options.open(path).with_context(|| {
        format!(
            "source is owned, writable or unavailable: {}",
            path.display()
        )
    })?;
    Ok(file)
}

fn validate_request(request: &ClosedCheckpointRequest, local: &Path) -> Result<(PathBuf, PathBuf)> {
    let valid_hex = |s: &str, n| {
        s.len() == n
            && s.bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    };
    let provenance = &request.provenance;
    anyhow::ensure!(
        !request.attempt_id.is_nil()
            && request.attempt_root.is_absolute()
            && request.source_runtime.is_absolute()
            && request.attempt_root.file_name().and_then(|s| s.to_str())
                == Some(&request.attempt_id.to_string()),
        "checkpoint needs absolute paths and a fresh UUID-named attempt"
    );
    anyhow::ensure!(
        valid_hex(&provenance.reviewed_source_revision, 40)
            && [
                &provenance.closure_receipt_sha256,
                &provenance.old_executable_sha256,
                &provenance.candidate_executable_sha256,
                &provenance.build_receipt_sha256
            ]
            .into_iter()
            .all(|s| valid_hex(s, 64)),
        "checkpoint requires exact source and evidence hashes"
    );
    let source = canonical_destination_without_reparse_points(&request.source_runtime)?;
    let target = canonical_destination_without_reparse_points(&request.attempt_root)?;
    anyhow::ensure!(
        source.is_dir() && target.parent().is_some_and(Path::is_dir) && !target.try_exists()?,
        "source/attempt parent must exist and attempt must be absent"
    );
    anyhow::ensure!(
        !paths_overlap(&source, &target),
        "checkpoint source and destination overlap"
    );
    let local = canonical_destination_without_reparse_points(local)?;
    for path in [&source, &target] {
        super::reject_live_overlap_under(path, &local)?;
        anyhow::ensure!(
            !paths_overlap(path, &local.join("io.prim1.v2")),
            "checkpoint overlaps shared v2 state"
        );
    }
    Ok((source, target))
}

fn canonical_uuid(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
}

fn classify(path: &str, directory: bool) -> Result<(CheckpointRole, u64)> {
    if path == "maintenance/upgrades" {
        return classify_upgrade("", directory);
    }
    if let Some(relative) = path.strip_prefix("maintenance/upgrades/") {
        return classify_upgrade(relative, directory);
    }
    if directory {
        anyhow::ensure!(
            matches!(path, "rooms" | "audit" | "maintenance"),
            "unrecognized source directory: {path}"
        );
        return Ok((CheckpointRole::Directory, 0));
    }
    let role = match path {
        SESSION_CATALOG_FILE_NAME | ROOM_CATALOG_FILE_NAME | room_guests::ROOM_GUESTS_FILE_NAME => {
            (CheckpointRole::State, CATALOG_MAX)
        }
        task_sideband::TASK_FOLDER_SETTING_FILE_NAME => (CheckpointRole::State, TASK_FOLDER_SETTING_MAX),
        "personal-repo.json" => (CheckpointRole::State, PERSONAL_REPO_SETTING_MAX),
        "install-id" => (CheckpointRole::Evidence, INSTALL_ID_MAX),
        DESKTOP_LOCK => (CheckpointRole::Ownership, 0),
        "desktop-events.jsonl" => (CheckpointRole::Evidence, EVIDENCE_FILE_MAX),
        // Left behind only by a PRIM-1 that did not shut down; grants nothing.
        shared_types::GUEST_ENDPOINT_FILE_NAME => (CheckpointRole::Evidence, GUEST_ENDPOINT_MAX),
        _ => {
            if let Some(name) = path.strip_prefix("rooms/") {
                if name.strip_suffix(".jsonl").is_some_and(canonical_uuid) {
                    return Ok((CheckpointRole::State, room_log::MAX_LOG_BYTES));
                }
                if let Some((room, tail)) = name.split_once(".torn-")
                    && canonical_uuid(room)
                    && tail.strip_suffix(".bin").is_some_and(canonical_uuid)
                {
                    return Ok((CheckpointRole::Evidence, room_log::MAX_RECORD_BYTES as u64));
                }
            }
            if let Some(name) = path.strip_prefix("audit/") {
                let plain = name.strip_suffix(".gz").unwrap_or(name);
                if parse_audit_file_date(plain).is_some() {
                    return Ok((CheckpointRole::Evidence, EVIDENCE_FILE_MAX));
                }
            }
            for catalog in [
                SESSION_CATALOG_FILE_NAME,
                ROOM_CATALOG_FILE_NAME,
                room_guests::ROOM_GUESTS_FILE_NAME,
                shared_types::GUEST_ENDPOINT_FILE_NAME,
            ] {
                if path
                    .strip_prefix(&format!(".{catalog}."))
                    .and_then(|s| s.strip_suffix(".tmp"))
                    .is_some_and(canonical_uuid)
                {
                    return Ok((CheckpointRole::Evidence, CATALOG_MAX));
                }
            }
            for (name, max) in [
                (task_sideband::TASK_FOLDER_SETTING_FILE_NAME, TASK_FOLDER_SETTING_MAX),
                ("personal-repo.json", PERSONAL_REPO_SETTING_MAX),
                ("install-id", INSTALL_ID_MAX),
            ] {
                if path
                    .strip_prefix(&format!(".{name}."))
                    .and_then(|s| s.strip_suffix(".tmp"))
                    .is_some_and(canonical_uuid)
                {
                    return Ok((CheckpointRole::Evidence, max));
                }
            }
            if let Some(name) = path.strip_prefix("maintenance/") {
                if matches!(name, "candidate.exe" | "candidate.partial") {
                    return Ok((CheckpointRole::Artifact, ARTIFACT_MAX));
                }
                for (record, max, retained) in [
                    ("incidents-v1.json", 1024 * 1024, true),
                    ("restart-preflight-v1.json", 2 * 1024 * 1024, true),
                    ("candidate-v1.json", 2 * 1024 * 1024, false),
                ] {
                    if name == record
                        || name == format!(".{record}.pending")
                        || (retained && name == format!(".{record}.retained"))
                    {
                        return Ok((CheckpointRole::Evidence, max));
                    }
                }
            }
            bail!("unrecognized source entry: {path}");
        }
    };
    Ok(role)
}

/// Upgrade records are historical evidence in a checkpoint, never resumable
/// transaction state. Only the exact staged binaries use the artifact budget.
fn classify_upgrade(path: &str, directory: bool) -> Result<(CheckpointRole, u64)> {
    let parts: Vec<_> = path.split('/').collect();
    if directory {
        anyhow::ensure!(matches!(parts.as_slice(), [""])
            || matches!(parts.as_slice(), [attempt] | [attempt, "candidate"] if canonical_uuid(attempt)),
            "unrecognized upgrade directory: {path}");
        return Ok((CheckpointRole::Directory, 0));
    }
    let record = |name: &str, committed: &str| {
        name == committed || name == format!(".{committed}.pending")
            || name.strip_prefix(&format!(".{committed}."))
                .and_then(|id| id.strip_suffix(".partial")).is_some_and(canonical_uuid)
    };
    let max = match parts.as_slice() {
        ["transaction.lock"] => 0,
        [name] if record(name, "current.json") => 16 * 1024,
        [attempt, "candidate", "prim1.exe" | "prim1.partial"] if canonical_uuid(attempt) => {
            return Ok((CheckpointRole::Artifact, ARTIFACT_MAX));
        }
        [attempt, "candidate", ".env"] if canonical_uuid(attempt) => 64 * 1024,
        [attempt, name] if canonical_uuid(attempt) => match *name {
            "request.json" | "request.partial" => 2 * 1024 * 1024,
            "continue-upgrade.ps1" | "continue-upgrade.partial" => EVIDENCE_FILE_MAX,
            "helper-claim.json" | "helper-ready.json" | "helper-result.json"
                | "helper-claim.json.tmp" | "helper-ready.json.tmp" | "helper-result.json.tmp" => 16 * 1024,
            name if record(name, "state.json") => 16 * 1024,
            _ => bail!("unrecognized upgrade entry: {path}"),
        },
        _ => bail!("unrecognized upgrade entry: {path}"),
    };
    Ok((CheckpointRole::Evidence, max))
}

fn inventory(root: &Path) -> Result<Vec<CheckpointEntry>> {
    fn visit(
        root: &Path,
        prefix: &str,
        entries: &mut Vec<CheckpointEntry>,
        files: &mut usize,
    ) -> Result<()> {
        for child in fs::read_dir(root.join(prefix))? {
            let child = child?;
            let name = child
                .file_name()
                .into_string()
                .map_err(|_| anyhow!("source name is not UTF-8"))?;
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            anyhow::ensure!(
                relative.len() <= MAX_PATH_BYTES && !relative.contains('\\'),
                "inventory path exceeds bound"
            );
            let metadata = fs::symlink_metadata(child.path())?;
            anyhow::ensure!(
                !metadata_is_link_or_reparse(&metadata),
                "reparse entry refused: {relative}"
            );
            let directory = metadata.is_dir();
            anyhow::ensure!(
                directory || metadata.is_file(),
                "non-regular entry refused: {relative}"
            );
            let (role, max) = classify(&relative, directory)?;
            if !directory {
                *files += 1;
            }
            anyhow::ensure!(
                *files <= MAX_FILES && entries.len() < MAX_FILES + 3,
                "checkpoint file count exceeds limit"
            );
            let bytes = if directory { 0 } else { metadata.len() };
            anyhow::ensure!(
                bytes <= max,
                "checkpoint entry exceeds byte limit: {relative}"
            );
            entries.push(CheckpointEntry {
                path: relative.clone(),
                directory,
                role,
                bytes,
                sha256: None,
                captured_path: String::new(),
            });
            if directory {
                visit(root, &relative, entries, files)?;
            }
        }
        Ok(())
    }
    let mut entries = Vec::new();
    visit(root, "", &mut entries, &mut 0)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    check_totals(&entries)?;
    Ok(entries)
}

fn check_totals(entries: &[CheckpointEntry]) -> Result<()> {
    let mut total = 0_u64;
    let mut evidence = 0_u64;
    let mut artifacts = 0_u64;
    for entry in entries {
        if entry.role == CheckpointRole::Artifact {
            artifacts = artifacts.checked_add(entry.bytes).context("artifact total overflow")?;
            continue;
        }
        total = total
            .checked_add(entry.bytes)
            .context("checkpoint total overflow")?;
        if entry.role == CheckpointRole::Evidence {
            evidence = evidence
                .checked_add(entry.bytes)
                .context("evidence total overflow")?;
        }
    }
    anyhow::ensure!(
        // The old allowlist admitted at most two 512 MiB artifact paths.
        // More attempts share that budget; this copier never deletes any.
        total <= CAPTURE_MAX && evidence <= EVIDENCE_MAX && artifacts <= 2 * ARTIFACT_MAX,
        "checkpoint aggregate byte limit exceeded"
    );
    Ok(())
}

fn same_inventory(before: &[CheckpointEntry], after: &[CheckpointEntry]) -> bool {
    before.len() == after.len()
        && before
            .iter()
            .zip(after)
            .all(|(a, b)| a.path == b.path && a.directory == b.directory && a.bytes == b.bytes)
}

fn digest(file: &mut File, size: u64) -> Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut read = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        read += count as u64;
        anyhow::ensure!(read <= size, "file grew during checkpoint");
        hash.update(&buffer[..count]);
    }
    anyhow::ensure!(read == size, "file shortened during checkpoint");
    Ok(format!("{:x}", hash.finalize()))
}

fn private_directory(path: &Path) -> Result<()> {
    fs::create_dir(path)?;
    restrict_path_to_current_user(path, true)
}

fn copy_file(
    source: &mut File,
    size: u64,
    destination: &Path,
    hook: &mut impl FnMut(Stage) -> Result<()>,
) -> Result<String> {
    source.seek(SeekFrom::Start(0))?;
    let mut target = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    restrict_path_to_current_user(destination, false)?;
    let mut hash = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        copied += count as u64;
        anyhow::ensure!(copied <= size, "source grew during copy");
        target.write_all(&buffer[..count])?;
        hash.update(&buffer[..count]);
        hook(Stage::CopyChunk)?;
    }
    anyhow::ensure!(copied == size, "source shortened during copy");
    target.sync_all()?;
    drop(target);
    let hash = format!("{:x}", hash.finalize());
    anyhow::ensure!(
        digest(&mut File::open(destination)?, size)? == hash,
        "copied bytes failed verification"
    );
    Ok(hash)
}

fn capture(
    request: ClosedCheckpointRequest,
    local: &Path,
    mut hook: impl FnMut(Stage) -> Result<()>,
) -> Result<ClosedCheckpointManifest> {
    let (source, target) = validate_request(&request, local)?;
    let mut directories = vec![open_source(&source, true)?];
    let mut handles = BTreeMap::new();
    // Acquire the ownership and both catalog fences before enumeration. There
    // is no never-started source exception and no source creation path.
    for name in [
        DESKTOP_LOCK,
        SESSION_CATALOG_FILE_NAME,
        ROOM_CATALOG_FILE_NAME,
    ] {
        ensure_existing_path_chain_has_no_reparse_points(&source.join(name))?;
        let file = open_source(&source.join(name), false)?;
        if name == DESKTOP_LOCK {
            file.try_lock()
                .context("source desktop is already locked")?;
        }
        handles.insert(name.to_string(), file);
    }
    let mut entries = inventory(&source)?;
    for entry in &entries {
        if entry.directory {
            directories.push(open_source(&source.join(&entry.path), true)?);
        } else if !handles.contains_key(&entry.path) {
            let file = open_source(&source.join(&entry.path), false)?;
            if entry.path.starts_with("rooms/") && entry.path.ends_with(".jsonl") {
                file.try_lock()
                    .context("source room writer is already locked")?;
            }
            // Maintenance pending/retained names can legitimately be hard
            // links after an interrupted retention operation. Denying write
            // and delete protects both; do not self-conflict by locking each.
            handles.insert(entry.path.clone(), file);
        }
    }
    anyhow::ensure!(
        same_inventory(&entries, &inventory(&source)?),
        "source inventory drifted during acquisition"
    );
    hook(Stage::Locked)?;
    let mut holds = Vec::new();
    // Classification reads only a retained bounded handle. Invalid catalogs
    // remain capturable evidence; validation below reports the actual error.
    let room_ids: HashSet<_> = {
        let file = handles
            .get_mut(ROOM_CATALOG_FILE_NAME)
            .expect("required catalog handle");
        file.seek(SeekFrom::Start(0))?;
        match serde_json::from_reader::<_, RoomCatalogV1>(file.take(CATALOG_MAX + 1)) {
            Ok(catalog) => catalog.rooms.into_iter().map(|room| room.room_id).collect(),
            Err(_) => {
                holds.push("room_catalog_unreadable_log_association_unverified".into());
                HashSet::new()
            }
        }
    };
    for entry in &mut entries {
        if let Some(id) = entry
            .path
            .strip_prefix("rooms/")
            .and_then(|s| s.strip_suffix(".jsonl"))
            && !room_ids.contains(&Uuid::parse_str(id)?)
        {
            entry.role = CheckpointRole::Evidence;
            holds.push(format!("orphan_room_log:{}", entry.path));
        }
        if entry.path.starts_with('.') && entry.path.ends_with(".tmp") {
            holds.push(format!("pending_catalog:{}", entry.path));
        }
        let area = if entry.role == CheckpointRole::State || entry.path == "rooms" {
            "checkpoint"
        } else {
            "evidence"
        };
        entry.captured_path = format!("{area}/{}", entry.path);
    }
    check_totals(&entries)?;
    // All source handles/limits and protected-path checks precede any write.
    private_directory(&target)?;
    for name in ["checkpoint", "evidence", "runtime", "profile"] {
        private_directory(&target.join(name))?;
    }
    // Evidence room files have a separate parent even when all active logs
    // belong to checkpoint/runtime. Empty source directories are inventoried.
    for entry in entries.iter().filter(|e| e.directory) {
        private_directory(&target.join(&entry.captured_path))?;
        if entry.path == "rooms" {
            private_directory(&target.join("runtime/rooms"))?;
            private_directory(&target.join("evidence/rooms"))?;
        }
    }
    for entry in entries.iter_mut().filter(|e| !e.directory) {
        let file = handles.get_mut(&entry.path).expect("retained source file");
        let hash = copy_file(
            file,
            entry.bytes,
            &target.join(&entry.captured_path),
            &mut hook,
        )?;
        if entry.role == CheckpointRole::State {
            anyhow::ensure!(
                copy_file(
                    file,
                    entry.bytes,
                    &target.join("runtime").join(&entry.path),
                    &mut hook
                )? == hash,
                "working copy differs from checkpoint"
            );
        }
        entry.sha256 = Some(hash);
    }
    let before = hashed_inventory(&target.join("runtime"))?;
    let mut compatibility = validate_copy(&target.join("runtime"));
    compatibility.holds.extend(holds);
    let after = hashed_inventory(&target.join("runtime"))?;
    compatibility.changes = changes(&before, &after);
    if !compatibility.changes.is_empty() {
        compatibility
            .holds
            .push("working_copy_changed_review_required".into());
    }
    compatibility.copy_replay_ok =
        compatibility.catalog_error.is_none() && compatibility.holds.is_empty();
    hook(Stage::BeforeCommit)?;
    anyhow::ensure!(
        same_inventory(&entries, &inventory(&source)?),
        "source inventory drifted during capture"
    );
    for entry in entries.iter().filter(|e| !e.directory) {
        let expected = entry.sha256.as_deref().expect("copied digest");
        anyhow::ensure!(
            digest(
                handles.get_mut(&entry.path).expect("retained source file"),
                entry.bytes
            )? == expected,
            "source content changed during capture"
        );
        anyhow::ensure!(
            digest(
                &mut File::open(target.join(&entry.captured_path))?,
                entry.bytes
            )? == expected,
            "pristine checkpoint/evidence changed during validation"
        );
    }
    let manifest = ClosedCheckpointManifest {
        schema_version: 1,
        attempt_id: request.attempt_id,
        captured_at: now_rfc3339(),
        source_runtime: source,
        provenance: request.provenance,
        entries,
        compatibility,
        limitations: vec![
            "operator_supplied_closure_and_build_provenance".into(),
            "cooperative_windows_capture_not_live_quiescence".into(),
            "shutdown_persistence_completeness_unverified".into(),
            "fresh_profile_settings_preservation_unverified".into(),
            "external_task_contents_not_inspected_or_captured".into(),
            "conversation_recovery_unverified".into(),
            "c2_r_delivery_witness_loss_unaccepted".into(),
            "no_launch_promotion_or_rollback_authority".into(),
            "first_slice_limits_not_full_application_capacity".into(),
        ],
    };
    super::persist_bounded_json(&target, "checkpoint-v1.json", &manifest, MANIFEST_MAX)?;
    // Keep source file and directory handles alive through manifest sync.
    drop(handles);
    drop(directories);
    Ok(manifest)
}

fn hashed_inventory(root: &Path) -> Result<Vec<CheckpointEntry>> {
    let mut entries = inventory(root)?;
    for entry in entries.iter_mut().filter(|e| !e.directory) {
        entry.sha256 = Some(digest(
            &mut File::open(root.join(&entry.path))?,
            entry.bytes,
        )?);
    }
    Ok(entries)
}

fn changes(before: &[CheckpointEntry], after: &[CheckpointEntry]) -> Vec<CheckpointChange> {
    let old: BTreeMap<_, _> = before
        .iter()
        .filter(|e| !e.directory)
        .map(|e| (e.path.clone(), e.sha256.clone()))
        .collect();
    let new: BTreeMap<_, _> = after
        .iter()
        .filter(|e| !e.directory)
        .map(|e| (e.path.clone(), e.sha256.clone()))
        .collect();
    let paths: std::collections::BTreeSet<_> = old.keys().chain(new.keys()).cloned().collect();
    paths
        .into_iter()
        .filter_map(|path| {
            let previous = old.get(&path).cloned().flatten();
            let current = new.get(&path).cloned().flatten();
            (previous != current).then_some(CheckpointChange {
                path,
                before_sha256: previous,
                after_sha256: current,
            })
        })
        .collect()
}

fn validate_copy(runtime: &Path) -> CheckpointCompatibility {
    let mut report = CheckpointCompatibility {
        copy_replay_ok: false,
        catalog_error: None,
        rooms: Vec::new(),
        session_recovery_holds: Vec::new(),
        in_memory_normalizations: Vec::new(),
        changes: Vec::new(),
        holds: Vec::new(),
        settings_preservation_verified: false,
        restart_authorized: false,
    };
    // Validate only the copied setting using the startup reader. Do not resolve
    // the default or open the selected TaskStore: those paths may belong to a
    // different runtime. An invalid choice remains byte-exact and held.
    if let Err(error) = task_sideband::read_task_folder_setting(runtime) {
        report.holds.push(format!(
            "task_folder_setting_unreadable:{}",
            error.chars().take(4096).collect::<String>()
        ));
    }
    let result = (|| -> Result<()> {
        let catalog = read_session_catalog(&runtime.join(SESSION_CATALOG_FILE_NAME))?;
        let sessions = SessionRegistry::from_catalog(&catalog)?;
        report.session_recovery_holds = sessions
            .ordered_slots()
            .filter(|s| s.recovery.is_some())
            .map(|s| s.session_id)
            .collect();
        for id in &report.session_recovery_holds {
            report.holds.push(format!("session_recovery:{id}"));
        }
        for row in &catalog.sessions {
            let slot = sessions
                .get_by_id(row.session_id)
                .expect("validated session");
            if slot.recovery != row.recovery || slot.recovery_history != row.recovery_history {
                report
                    .in_memory_normalizations
                    .push(format!("session_recovery_identity:{}", row.session_id));
            }
            if slot.last_pty_size != row.last_pty_cols.zip(row.last_pty_rows) {
                report
                    .in_memory_normalizations
                    .push(format!("session_pty_dimensions:{}", row.session_id));
            }
        }
        let room_catalog = read_room_catalog(&runtime.join(ROOM_CATALOG_FILE_NAME))?;
        let known_sessions = sessions.by_id.keys().copied().collect();
        let mut rooms = RoomState::from_catalog(room_catalog.clone(), &known_sessions)?;
        for (original, normalized) in room_catalog.rooms.iter().zip(&rooms.catalog.rooms) {
            if original != normalized {
                report
                    .in_memory_normalizations
                    .push(format!("room_catalog:{}", original.room_id));
            }
        }
        // Guests load beside the rooms, as at startup: an unreadable registry
        // holds them all, and a deleted room's guests lapse in memory.
        let (guest_error, lapsed) =
            GuestRegistry::load(runtime, &rooms).replay_report(runtime);
        if guest_error.is_some() {
            report.holds.push("guest_registry_unreadable".into());
        }
        for guest_id in lapsed {
            report
                .in_memory_normalizations
                .push(format!("room_guest_lapsed:{guest_id}"));
        }
        if !report.in_memory_normalizations.is_empty() {
            report
                .holds
                .push("in_memory_normalization_review_required".into());
        }
        rooms.open_logs(runtime, |candidate| {
            persist_room_catalog(runtime, candidate)
        });
        report.rooms = rooms.snapshots();
        for room in &report.rooms {
            if room.recovery_error.is_some()
                || room.history_replay_pending
                || room.writer_recovery.is_some()
            {
                report.holds.push(format!("room_recovery:{}", room.room_id));
            }
            if room.history_restarted {
                report
                    .holds
                    .push(format!("room_history_restarted:{}", room.room_id));
            }
        }
        // Dropping this private RoomState closes only working-copy writers.
        Ok(())
    })();
    if let Err(error) = result {
        report.catalog_error = Some(format!("{error:#}").chars().take(4096).collect());
    }
    report
}

#[cfg(all(test, windows))]
#[path = "closed_checkpoint_tests.rs"]
mod tests;

#[cfg(all(test, windows))]
#[test]
fn closed_checkpoint_captures_upgrade_attempts_only_as_evidence_and_artifacts() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let local = root.path().join("local");
    for directory in [&source, &local] { fs::create_dir(directory).unwrap(); }
    fs::write(source.join(DESKTOP_LOCK), []).unwrap();
    let catalog = SessionCatalogV1::empty(QualifiedWorkingDirectory {
        namespace: WorkingDirectoryNamespace::Windows,
        canonical_path: root.path().join("work").display().to_string(),
        identity: "fixture-volume-and-directory-id".into(),
    });
    fs::write(source.join(SESSION_CATALOG_FILE_NAME), serde_json::to_vec(&catalog).unwrap()).unwrap();
    fs::write(source.join(ROOM_CATALOG_FILE_NAME), serde_json::to_vec(&RoomCatalogV1::empty()).unwrap()).unwrap();
    let attempt = Uuid::new_v4();
    let temporary = Uuid::new_v4();
    let base = format!("maintenance/upgrades/{attempt}");
    let mut files = vec![
        ("maintenance/upgrades/transaction.lock".into(), Vec::new()),
        ("maintenance/upgrades/current.json".into(), b"unparsed pointer evidence".to_vec()),
        (format!("maintenance/upgrades/.current.json.{temporary}.partial"), b"partial pointer".to_vec()),
    ];
    for name in ["request.json", "request.partial", "state.json", ".state.json.pending",
        "continue-upgrade.ps1", "continue-upgrade.partial", "helper-claim.json", "helper-ready.json",
        "helper-result.json", "helper-claim.json.tmp", "helper-ready.json.tmp", "helper-result.json.tmp",
        "candidate/.env", "candidate/prim1.exe", "candidate/prim1.partial"] {
        files.push((format!("{base}/{name}"), format!("retained {name}; never executed or replayed").into_bytes()));
    }
    files.push((format!("{base}/.state.json.{temporary}.partial"), b"partial state".to_vec()));
    for (name, bytes) in &files {
        fs::create_dir_all(source.join(name).parent().unwrap()).unwrap();
        fs::write(source.join(name), bytes).unwrap();
    }
    let checkpoint = Uuid::new_v4();
    let target = root.path().join(checkpoint.to_string());
    let manifest = capture(ClosedCheckpointRequest {
        source_runtime: source.clone(), attempt_root: target.clone(), attempt_id: checkpoint,
        provenance: ClosedCheckpointProvenance {
            closure_receipt_sha256: "1".repeat(64), old_executable_sha256: "2".repeat(64),
            candidate_executable_sha256: "3".repeat(64), reviewed_source_revision: "4".repeat(40),
            build_receipt_sha256: "5".repeat(64),
        },
    }, &local, |_| Ok(())).unwrap();
    for (name, bytes) in &files {
        let entry = manifest.entries.iter().find(|entry| entry.path == *name).unwrap();
        let binary = name.ends_with("/candidate/prim1.exe") || name.ends_with("/candidate/prim1.partial");
        assert_eq!(entry.role, if binary { CheckpointRole::Artifact } else { CheckpointRole::Evidence });
        assert_eq!(entry.captured_path, format!("evidence/{name}"));
        assert_eq!(entry.sha256, Some(format!("{:x}", Sha256::digest(bytes))));
        assert_eq!(fs::read(target.join(&entry.captured_path)).unwrap(), *bytes);
        assert_eq!(fs::read(source.join(name)).unwrap(), *bytes);
    }
    assert!(!target.join("runtime/maintenance").exists());
    assert!(!target.join("checkpoint/maintenance").exists());
    assert!(manifest.compatibility.copy_replay_ok, "upgrade evidence must not be parsed or replayed");
    assert!(!manifest.compatibility.restart_authorized);
    // Preserve the old maximum of two 512 MiB artifacts across all attempts,
    // without allocating or copying large files in this fixture.
    let mut artifact = manifest.entries.iter().find(|entry| entry.role == CheckpointRole::Artifact).unwrap().clone();
    artifact.bytes = ARTIFACT_MAX;
    assert!(check_totals(&[artifact.clone(), artifact.clone()]).is_ok());
    assert!(check_totals(&[artifact.clone(), artifact.clone(), artifact]).is_err());
    for (relative, directory) in [("foreign".into(), true), ("current.json".into(), true),
        (format!("{attempt}/candidate/nested"), true), (format!("{attempt}/candidate/other.exe"), false),
        (format!("{attempt}/.state.json.not-a-uuid.partial"), false)] {
        assert!(classify_upgrade(&relative, directory).is_err());
    }
    fs::write(source.join("maintenance/upgrades/unexpected.json"), b"unknown sibling").unwrap();
    assert!(inventory(&source).unwrap_err().to_string().contains("unrecognized upgrade entry"));
}
