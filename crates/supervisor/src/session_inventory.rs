//! Read-only provider discovery. Candidates are not PRIM members or permission to resume.
//! Bounded metadata records only; no message fields returned, timestamps never select an owner.
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[path = "session_inventory_attach.rs"]
mod attach;
pub use attach::AttachLocalSessionRequest;

const HEADER_BYTES: u64 = 64 * 1024;
const CLAUDE_PREFIX_BYTES: u64 = 1024 * 1024;
const CLAUDE_PREFIX_RECORDS: usize = 16;
const MAX_ENTRIES: usize = 5_000;
const MAX_CANDIDATES: usize = 2_000;
const MAX_ERRORS: usize = 8;
const MAX_SOURCES: usize = 8;
const MAX_SCAN_TIME: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationKind {
    Root,
    Subagent,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub provider: String,
    pub id: String,
    pub label: Option<String>,
    pub workspace: String,
    pub metadata_paths: Vec<String>,
    /// Provider timestamp as recorded (no inference from transcript content or recency).
    pub updated_at: Option<String>,
    pub kind: ConversationKind,
    pub evidence: String,
    pub conflict: bool,
    /// Metadata may be discoverable while the existing resume path cannot use it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attach_unavailable: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct ProviderStatus {
    pub provider: String,
    pub entries_examined: usize,
    pub truncated: bool,
    pub errors: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Inventory {
    pub workspace: String,
    pub candidates: Vec<Candidate>,
    pub providers: Vec<ProviderStatus>,
    /// A bounded partial result, not an assertion that omitted conversations do not exist.
    pub truncated: bool,
}

/// Use the effective personal repository, never a stale UI workspace or a
/// fallback when the operator explicitly disabled the register. A register
/// writer error does not prevent a read-only scan of an otherwise valid folder.
pub fn discover_personal_repo(status: &crate::PersonalRepoStatus) -> Result<Inventory, String> {
    let workspace = discovery_scope(status)?;
    Ok(discover(&workspace))
}

fn discovery_scope(status: &crate::PersonalRepoStatus) -> Result<String, String> {
    if status.status == crate::PersonalRepoHealth::Off {
        let reason = status.message.as_deref().unwrap_or("The personal repository is off.");
        let action = if status.source == crate::PersonalRepoSource::Environment {
            " Remove PRIM1_PERSONAL_REPO=off and restart to choose a personal repository."
        } else { "" };
        return Err(format!("Local conversation discovery is disabled: {reason}{action}"));
    }
    let folder = status.folder.as_deref()
        .ok_or("Choose a usable personal repository before discovering local conversations")?;
    let path = Path::new(folder);
    if !path.is_absolute() || !path.is_dir() {
        return Err("The personal repository must be an existing absolute folder before discovering local conversations".into());
    }
    path.canonicalize().map(|path| crate::child_process_path(&path).display().to_string())
        .map_err(|error| format!("The personal repository cannot be read: {error}"))
}

pub fn discover(workspace: &str) -> Inventory {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| cfg!(target_os = "macos").then(|| std::env::var_os("HOME")).flatten())
        .map(PathBuf::from)
        .unwrap_or_default();
    let codex = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let claude = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"));
    discover_at(workspace, &codex, &claude, &home.join(".grok"))
}

pub fn discover_at(
    workspace: &str,
    codex_root: &Path,
    claude_root: &Path,
    grok_root: &Path,
) -> Inventory {
    let mut result = Inventory {
        workspace: workspace.into(),
        ..Default::default()
    };
    for (provider, root) in [
        ("codex", codex_root),
        ("claude", claude_root),
        ("grok", grok_root),
    ] {
        let mut scan = Scan {
            status: ProviderStatus {
                provider: provider.into(),
                ..Default::default()
            },
            started: Instant::now(),
            root: PathBuf::new(),
            wanted: normalize(workspace),
            rows: BTreeMap::new(),
            codex_names: BTreeMap::new(),
            codex_cli_paths: BTreeSet::new(),
            observed_workspaces: BTreeMap::new(),
            timestamp_conflicts: BTreeSet::new(),
        };
        if !absolute_workspace(workspace) || !root.is_absolute() {
            scan.error("workspace and provider root must be absolute paths".into());
        } else if plain(root).is_none_or(|metadata| !metadata.is_dir()) {
            scan.error(format!(
                "provider root is missing, unreadable, or a link: {}",
                root.display()
            ));
        } else {
            match root.canonicalize() {
                Err(error) => scan.error(format!("provider root: {error}")),
                Ok(root) => {
                    scan.root = root;
                    if provider == "codex" {
                        scan.codex_index();
                    }
                    let dirs = if provider == "claude" {
                        vec!["sessions", "projects"]
                    } else if provider == "codex" {
                        vec!["sessions", "archived_sessions"]
                    } else {
                        vec!["sessions"]
                    };
                    for dir in dirs {
                        scan.walk(scan.root.join(dir), 0);
                    }
                }
            }
        }
        if provider == "codex" {
            scan.mark_codex_attach_limits();
        }
        result.truncated |= scan.status.truncated;
        result.candidates.extend(scan.rows.into_values());
        result.providers.push(scan.status);
    }
    result
}

struct Scan {
    status: ProviderStatus,
    started: Instant,
    root: PathBuf,
    wanted: String,
    rows: BTreeMap<String, Candidate>,
    codex_names: BTreeMap<String, (String, Option<String>)>,
    // Eligibility belongs to the exact inspected record, never another path
    // with the same ID or a filename alone.
    codex_cli_paths: BTreeSet<PathBuf>,
    observed_workspaces: BTreeMap<String, BTreeMap<[u8; 32], String>>,
    timestamp_conflicts: BTreeSet<String>,
}

impl Scan {
    fn mark_codex_attach_limits(&mut self) {
        let sessions = self.root.join("sessions");
        let archived = self.root.join("archived_sessions");
        for candidate in self.rows.values_mut() {
            let canonical_name = format!("-{}.jsonl", candidate.id);
            let supported = candidate.metadata_paths.iter().any(|path| {
                let path = Path::new(path);
                self.codex_cli_paths.contains(path) && path.starts_with(&sessions)
                    && path.file_name().and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(&canonical_name))
            });
            if supported { continue; }
            let only_archived = candidate.metadata_paths.iter().filter(|path|
                Path::new(path).extension().is_some_and(|extension| extension == "jsonl")
                    && Path::new(path).file_name().is_none_or(|name| name != "session_index.jsonl"))
                .all(|path| Path::new(path).starts_with(&archived));
            let cli_source = candidate.metadata_paths.iter()
                .any(|path| self.codex_cli_paths.contains(Path::new(path)));
            candidate.attach_unavailable = Some(if !cli_source {
                "This Codex conversation is not a recognized root CLI source; PRIM-1 cannot attach it yet"
            } else if only_archived {
                "Archived in Codex; unarchive it in Codex first"
            } else {
                "This Codex conversation file is not resumable by PRIM-1 yet"
            }.into());
        }
    }

    fn error(&mut self, error: String) {
        if !self.status.errors.contains(&error) {
            if self.status.errors.len() < MAX_ERRORS {
                self.status.errors.push(error);
            } else {
                self.status.truncated = true;
            }
        }
    }
    fn budget(&mut self) -> bool {
        if self.status.entries_examined >= MAX_ENTRIES || self.started.elapsed() >= MAX_SCAN_TIME {
            self.status.truncated = true;
            false
        } else {
            true
        }
    }
    fn walk(&mut self, directory: PathBuf, depth: usize) {
        if !self.budget() {
            return;
        }
        if !directory.exists() {
            return;
        } // Optional provider subdirectories need not exist.
        if !self.safe(&directory) {
            return;
        }
        let mut entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(e) => {
                self.error(format!("{}: {e}", directory.display()));
                return;
            }
        };
        // Enumerate only the remaining entry budget, then prefer recent date
        // directories/rollouts. The admitted batch is still processed at the cap.
        let mut paths = Vec::new();
        while self.budget() {
            let Some(entry) = entries.next() else {
                break;
            };
            self.status.entries_examined += 1;
            match entry {
                Ok(entry) => paths.push(entry.path()),
                Err(e) => {
                    self.error(e.to_string());
                }
            }
        }
        if self.status.provider == "codex" {
            paths.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
        }
        for path in paths {
            if self.started.elapsed() >= MAX_SCAN_TIME {
                self.status.truncated = true;
                break;
            }
            // Grok's percent encoding is reversible. Filter workspace folders
            // before stat/canonicalization of their potentially large children.
            if self.status.provider == "grok" && depth == 0 {
                match path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(percent_decode)
                {
                    Some(workspace) if normalize(&workspace) == self.wanted => {}
                    Some(_) => continue,
                    None => {
                        self.error("invalid Grok workspace folder encoding".into());
                        continue;
                    }
                }
            }
            if !self.safe(&path) {
                continue;
            }
            let Some(metadata) = plain(&path) else {
                continue;
            };
            if metadata.is_dir() {
                if self.status.provider == "grok" && depth == 1 {
                    self.grok(&path);
                } else if self.status.provider == "claude" && depth >= 1 {
                    continue;
                }
                // Never enter transcript subagent trees.
                else if depth < 4 {
                    self.walk(path, depth + 1);
                } else {
                    self.status.truncated = true;
                }
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let relevant = match self.status.provider.as_str() {
                "codex" => name.starts_with("rollout-") && name.ends_with(".jsonl"),
                "claude" => {
                    name.ends_with(".jsonl")
                        || (directory == self.root.join("sessions") && name.ends_with(".json"))
                        || name == "sessions-index.json"
                }
                _ => false,
            };
            if !relevant {
                continue;
            }
            let metadata = if self.status.provider == "claude" && name.ends_with(".jsonl") {
                jsonl_metadata(&path, true)
            } else {
                metadata_json(&path, name.ends_with(".jsonl"))
            };
            match metadata {
                Ok(value) => {
                    if self.status.provider == "codex" {
                        self.codex(&path, &value);
                    } else {
                        self.claude(&path, &value);
                    }
                }
                Err(error) => {
                    self.status.truncated |= error.contains("64 KiB") || error.contains("scan limit");
                    self.error(format!("{}: {error}", path.display()));
                }
            }
        }
    }
    fn safe(&mut self, path: &Path) -> bool {
        if plain(path).is_some() && path.canonicalize().is_ok_and(|p| p.starts_with(&self.root)) {
            true
        } else {
            self.error(format!(
                "skipped unreadable/link/outside-root entry: {}",
                path.display()
            ));
            false
        }
    }
    fn insert(
        &mut self,
        path: &Path,
        id: &str,
        workspace: &str,
        label: Option<&str>,
        updated: Option<String>,
        kind: ConversationKind,
        evidence: &str,
    ) {
        let Ok(uuid) = uuid::Uuid::parse_str(id) else {
            self.error(format!("invalid conversation ID in {}", path.display()));
            return;
        };
        if uuid.is_nil() {
            return;
        }
        let id = uuid.to_string();
        let source = path.display().to_string();
        let normalized_workspace = normalize(workspace);
        let workspace_key: [u8; 32] = Sha256::digest(normalized_workspace.as_bytes()).into();
        let observed = self.observed_workspaces.entry(id.clone()).or_default();
        if !observed.contains_key(&workspace_key) {
            if observed.len() < MAX_SOURCES {
                observed.insert(workspace_key, source.clone());
            } else {
                self.status.truncated = true;
            }
        }
        let conflicting = (observed.len() > 1).then(|| {
            observed
                .values()
                .take(MAX_SOURCES)
                .cloned()
                .collect::<Vec<_>>()
        });
        if normalized_workspace != self.wanted {
            if let (Some(old), Some(paths)) = (self.rows.get_mut(&id), conflicting) {
                old.conflict = true;
                old.kind = ConversationKind::Unknown;
                old.evidence = "Same provider ID appears in different workspaces; no association is established".into();
                for path in paths {
                    if old.metadata_paths.len() < MAX_SOURCES && !old.metadata_paths.contains(&path)
                    {
                        old.metadata_paths.push(path);
                    }
                }
            }
            return;
        }
        let label = label
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(160).collect::<String>());
        if let Some(old) = self.rows.get_mut(&id) {
            // Missing fields are not contradictory evidence. Preserve an earlier
            // conflict, and combine complementary PID/index metadata without
            // selecting a conversation owner or preferring the newest timestamp.
            if !old.conflict {
                let kind_conflict = old.kind != ConversationKind::Unknown
                    && kind != ConversationKind::Unknown
                    && old.kind != kind;
                if kind_conflict {
                    old.conflict = true;
                    old.label = None;
                    old.updated_at = None;
                    old.kind = ConversationKind::Unknown;
                    old.evidence = "Conflicting provider metadata; inspect listed sources before associating this conversation".into();
                } else {
                    // Titles are mutable display metadata, never identity evidence.
                    if old.label.is_none() {
                        old.label = label;
                    }
                    if old.kind == ConversationKind::Unknown && kind != ConversationKind::Unknown {
                        old.kind = kind;
                        old.evidence = evidence.into();
                    }
                    if self.timestamp_conflicts.contains(&id) {
                        old.updated_at = None;
                    } else if old
                        .updated_at
                        .as_ref()
                        .zip(updated.as_ref())
                        .is_some_and(|(a, b)| a != b)
                    {
                        self.timestamp_conflicts.insert(id.clone());
                        old.updated_at = None;
                    } else if old.updated_at.is_none() {
                        old.updated_at = updated;
                    }
                }
            }
            if !old.metadata_paths.contains(&source) {
                if old.metadata_paths.len() < MAX_SOURCES {
                    old.metadata_paths.push(source);
                } else {
                    old.conflict = true;
                    self.status.truncated = true;
                }
            }
        } else if self.rows.len() < MAX_CANDIDATES {
            self.rows.insert(
                id.clone(),
                Candidate {
                    provider: self.status.provider.clone(),
                    id: id.clone(),
                    label,
                    workspace: workspace.into(),
                    metadata_paths: vec![source],
                    updated_at: updated,
                    kind,
                    evidence: evidence.into(),
                    conflict: conflicting.is_some(),
                    attach_unavailable: None,
                },
            );
        } else {
            self.status.truncated = true;
        }
        if let (Some(old), Some(paths)) = (self.rows.get_mut(&id), conflicting) {
            old.conflict = true;
            old.kind = ConversationKind::Unknown;
            old.evidence =
                "Same provider ID appears in different workspaces; no association is established"
                    .into();
            for path in paths {
                if old.metadata_paths.len() < MAX_SOURCES && !old.metadata_paths.contains(&path) {
                    old.metadata_paths.push(path);
                }
            }
        }
    }
    fn codex(&mut self, path: &Path, value: &Value) {
        if value["type"] != "session_meta" {
            self.error(format!(
                "no Codex session_meta first record: {}",
                path.display()
            ));
            return;
        }
        let p = &value["payload"];
        let (Some(id), Some(cwd)) = (p["id"].as_str(), p["cwd"].as_str()) else {
            self.error(format!("Codex metadata lacks id/cwd: {}", path.display()));
            return;
        };
        if !codex_filename_matches(path, id) {
            self.error(format!(
                "Codex filename/header ID conflict: {}",
                path.display()
            ));
            return;
        }
        let child = p["parent_thread_id"].as_str().is_some()
            || p["thread_source"] == "subagent"
            || p["source"].get("subagent").is_some();
        let kind = if child {
            ConversationKind::Subagent
        } else if p["thread_source"] == "user" {
            ConversationKind::Root
        } else {
            ConversationKind::Unknown
        };
        let canonical_id = uuid::Uuid::parse_str(id)
            .map(|id| id.to_string())
            .unwrap_or_default();
        let named = self.codex_names.get(&canonical_id).cloned();
        let (label, updated) = named
            .as_ref()
            .map(|(name, time)| (Some(name.as_str()), time.clone()))
            .unwrap_or((p["name"].as_str(), text_timestamp(&p["updated_at"])));
        self.insert(path, id, cwd, label, updated, kind, "Codex session_meta: thread_source/source/parent_thread_id; label may come from session_index.jsonl; not a PRIM binding");
        if p["thread_source"] == "user" && p["source"] == "cli"
            && p["parent_thread_id"].is_null() && normalize(cwd) == self.wanted
            && self.rows.contains_key(&canonical_id)
        {
            self.codex_cli_paths.insert(path.to_path_buf());
        }
        if let Some(row) = self.rows.get_mut(&canonical_id) {
            let index = self.root.join("session_index.jsonl").display().to_string();
            if named.is_some() && !row.metadata_paths.contains(&index) {
                if row.metadata_paths.len() < MAX_SOURCES {
                    row.metadata_paths.push(index);
                } else {
                    self.status.truncated = true;
                }
            }
        }
    }
    fn codex_index(&mut self) {
        let path = self.root.join("session_index.jsonl");
        if !path.exists() || !self.safe(&path) {
            return;
        }
        let bytes = match fs::File::open(&path).and_then(|file| {
            let mut bytes = Vec::new();
            file.take(HEADER_BYTES + 1).read_to_end(&mut bytes)?;
            Ok(bytes)
        }) {
            Ok(bytes) if bytes.len() as u64 <= HEADER_BYTES => bytes,
            Ok(_) => {
                self.status.truncated = true;
                self.error(
                    "Codex name index exceeds 64 KiB; rollout discovery continues without names"
                        .into(),
                );
                return;
            }
            Err(e) => {
                self.error(format!("Codex name index: {e}"));
                return;
            }
        };
        for line in bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
            if !self.budget() {
                break;
            }
            self.status.entries_examined += 1;
            let Ok(v) = serde_json::from_slice::<Value>(line) else {
                self.error("invalid Codex name index record".into());
                continue;
            };
            let (Some(id), Some(name)) = (v["id"].as_str(), v["thread_name"].as_str()) else {
                continue;
            };
            let Ok(id) = uuid::Uuid::parse_str(id) else {
                continue;
            };
            let new = (
                name.chars().take(160).collect(),
                text_timestamp(&v["updated_at"]),
            );
            self.codex_names
                .entry(id.to_string())
                .and_modify(|(old_name, old_time)| {
                    // This append-only name index records renames, not owners.
                    old_name.clone_from(&new.0);
                    if *old_time != new.1 {
                        *old_time = None;
                    }
                })
                .or_insert(new);
        }
    }
    fn claude(&mut self, path: &Path, value: &Value) {
        if path.file_name().is_some_and(|n| n == "sessions-index.json") {
            if let Some(entries) = value["entries"].as_array() {
                for entry in entries {
                    if !self.budget() {
                        break;
                    }
                    self.status.entries_examined += 1;
                    self.claude_record(path, entry);
                }
            } else {
                self.error(format!("unrecognized Claude index: {}", path.display()));
            }
        } else {
            self.claude_record(path, value);
        }
    }
    fn claude_record(&mut self, path: &Path, value: &Value) {
        let id = value["sessionId"].as_str();
        let cwd = value["cwd"]
            .as_str()
            .or_else(|| value["projectPath"].as_str());
        let (Some(id), Some(cwd)) = (id, cwd) else {
            self.error("Claude records without exact workspace metadata are omitted; encoded project folders are ambiguous".into());
            return;
        };
        if path.extension().is_some_and(|e| e == "jsonl")
            && path.file_stem().and_then(|s| s.to_str()) != Some(id)
        {
            self.error(format!(
                "Claude filename/header ID conflict: {}",
                path.display()
            ));
            return;
        }
        let kind = match value["isSidechain"].as_bool() {
            Some(true) => ConversationKind::Subagent,
            Some(false) => ConversationKind::Root,
            None => ConversationKind::Unknown,
        };
        self.insert(
            path,
            id,
            cwd,
            value["customTitle"]
                .as_str()
                .or_else(|| value["aiTitle"].as_str())
                .or_else(|| value["name"].as_str()),
            text_timestamp(&value["updatedAt"]).or_else(|| text_timestamp(&value["modified"])),
            kind,
            "Claude exact cwd/projectPath and isSidechain; identity within 1 MiB/16 records, titles within a 64 KiB tail; no message fields returned",
        );
        // The transcript's bounded title tail carries later /rename metadata.
        // A prior PID/index label must not hide it; titles never change identity.
        if path.extension().is_some_and(|extension| extension == "jsonl")
            && let Some(title) = value["customTitle"].as_str().or_else(|| value["aiTitle"].as_str())
            && let Ok(id) = uuid::Uuid::parse_str(id)
            && let Some(row) = self.rows.get_mut(&id.to_string()).filter(|row| !row.conflict)
        {
            row.label = Some(title.chars().take(160).collect());
        }
    }
    fn grok(&mut self, path: &Path) {
        let Some(id) = path.file_name().and_then(|n| n.to_str()) else {
            return;
        };
        let Some(encoded) = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
        else {
            return;
        };
        let Some(cwd) = percent_decode(encoded) else {
            self.error(format!("invalid Grok workspace encoding: {encoded}"));
            return;
        };
        let summary = path.join("summary.json");
        if !summary.exists() {
            self.insert(path, id, &cwd, None, None, ConversationKind::Unknown, "Grok workspace/UUID directory only; metadata missing, root/subagent not established");
            return;
        }
        if !self.safe(&summary) {
            return;
        }
        match metadata_json(&summary, false) {
            Ok(v) if v["info"]["id"].as_str() == Some(id) && v["info"]["cwd"].as_str().is_some_and(|s| normalize(s) == normalize(&cwd)) =>
                self.insert(&summary, id, &cwd, v["generated_title"].as_str(), text_timestamp(&v["updated_at"]), ConversationKind::Root, "Grok summary info.id/info.cwd match direct workspace/UUID path; nested subagents excluded"),
            Ok(_) => self.error(format!("Grok summary/path identity or workspace conflict: {}", summary.display())),
            Err(e) => {
                self.status.truncated |= e.contains("64 KiB");
                self.error(format!("{}: {e}", summary.display()));
            }
        }
    }
}

fn metadata_json(path: &Path, first_line: bool) -> Result<Value, String> {
    if first_line {
        return jsonl_metadata(path, false);
    }
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    let mut reader = BufReader::new(file.take(HEADER_BYTES + 1));
    reader.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > HEADER_BYTES {
        return Err("metadata exceeds 64 KiB; conversation body was not scanned".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "invalid/incomplete metadata JSON".into())
}
fn jsonl_metadata(path: &Path, seek_claude: bool) -> Result<Value, String> {
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    if !seek_claude {
        let mut bytes = Vec::new();
        BufReader::new(file.take(HEADER_BYTES + 1)).read_until(b'\n', &mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > HEADER_BYTES {
            return Err("metadata prefix exceeds 64 KiB; later records were not inspected".into());
        }
        return serde_json::from_slice(&bytes).map_err(|_| "invalid/incomplete metadata JSON".into());
    }
    let mut reader = BufReader::new((&mut file).take(CLAUDE_PREFIX_BYTES));
    let mut bytes = Vec::new();
    let mut title: Option<(String, String, String)> = None;
    let mut oversized = false;
    for _ in 0..CLAUDE_PREFIX_RECORDS {
        let (consumed, complete, fits) = bounded_record(&mut reader, &mut bytes)
            .map_err(|error| error.to_string())?;
        if !complete && reader.get_ref().limit() == 0 {
            break;
        }
        if consumed == 0 {
            return Err(if oversized {
                "Claude metadata scan limit: records over 64 KiB were skipped; exact sessionId/workspace metadata was not found"
            } else {
                "Claude records lack exact sessionId/workspace metadata; encoded project folders are ambiguous"
            }.into());
        }
        if !fits { oversized = true; continue; }
        let mut value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "invalid/incomplete metadata JSON".to_string())?;
        if let Some((id, field, name)) = claude_title(&value) {
            title = Some((id.into(), field.into(), name.chars().take(160).collect()));
        }
        if value["sessionId"].is_string()
            && (value["cwd"].is_string() || value["projectPath"].is_string())
        {
            if let Some((id, field, name)) = title
                && value["sessionId"].as_str() == Some(id.as_str())
                && value["customTitle"].is_null()
                && value["aiTitle"].is_null()
            {
                value[field.as_str()] = Value::String(name);
            }
            // Release the prefix reader before seeking the same open file.
            drop(reader);
            // The tail title is display-only: a failed read keeps the row and
            // its prefix label rather than hiding a valid conversation.
            if let Some((field, name)) = claude_tail_title(&mut file, value["sessionId"].as_str().unwrap()).ok().flatten() {
                value["customTitle"] = Value::Null;
                value["aiTitle"] = Value::Null;
                value[field] = Value::String(name);
            }
            // Parsing a metadata-bearing record may encounter a message field;
            // keep only the metadata, never retain/copy its conversation body.
            let fields = ["sessionId", "cwd", "projectPath", "isSidechain", "customTitle",
                "aiTitle", "name", "updatedAt", "modified"];
            value.as_object_mut().unwrap().retain(|key, _| fields.contains(&key.as_str()));
            return Ok(value);
        }
    }
    Err("Claude metadata scan limit: exact sessionId/workspace not found within 1 MiB or 16 records; records over 64 KiB are skipped".into())
}

/// Stream oversized records to their newline without ever retaining >64 KiB.
/// The caller's Take bounds total bytes, including discarded data.
fn bounded_record(reader: &mut impl BufRead, bytes: &mut Vec<u8>) -> std::io::Result<(usize, bool, bool)> {
    bytes.clear();
    let mut consumed = 0;
    let mut fits = true;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() { return Ok((consumed, false, fits)); }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let length = newline.map_or(available.len(), |at| at + 1);
        consumed += length;
        if consumed as u64 <= HEADER_BYTES { bytes.extend_from_slice(&available[..length]); }
        else { fits = false; bytes.clear(); }
        reader.consume(length);
        if newline.is_some() { return Ok((consumed, true, fits)); }
    }
}

fn claude_title(value: &Value) -> Option<(&str, &'static str, &str)> {
    let field = match value["type"].as_str()? {
        "custom-title" => "customTitle",
        "ai-title" => "aiTitle",
        _ => return None,
    };
    let title = value[field].as_str().filter(|title| !title.is_empty())?;
    Some((value["sessionId"].as_str()?, field, title))
}

fn claude_tail_title(file: &mut fs::File, id: &str) -> Result<Option<(&'static str, String)>, String> {
    let length = file.metadata().map_err(|error| error.to_string())?.len();
    let start = length.saturating_sub(HEADER_BYTES);
    file.seek(SeekFrom::Start(start)).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.take(HEADER_BYTES).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
    let mut lines = bytes.split(|byte| *byte == b'\n');
    if start > 0 { lines.next(); } // A cut first record cannot establish a title.
    let (mut custom, mut ai) = (None, None);
    for line in lines {
        let Ok(value) = serde_json::from_slice::<Value>(line) else { continue; };
        if let Some((session_id, field, title)) = claude_title(&value)
            && session_id == id
        {
            let title = title.chars().take(160).collect::<String>();
            if field == "customTitle" { custom = Some(title); } else { ai = Some(title); }
        }
    }
    Ok(custom.map(|title| ("customTitle", title)).or_else(|| ai.map(|title| ("aiTitle", title))))
}
fn codex_filename_matches(path: &Path, id: &str) -> bool {
    let Some(stem) = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("rollout-"))
        .and_then(|name| name.strip_suffix(".jsonl"))
    else {
        return false;
    };
    let primary = if let Some((primary, suffix)) = stem.rsplit_once('_') {
        if suffix.len() != 36 || uuid::Uuid::parse_str(suffix).is_err() {
            return false;
        }
        primary
    } else {
        stem
    };
    let Some(primary_id) = primary.get(primary.len().saturating_sub(36)..) else {
        return false;
    };
    let boundary = primary.len().saturating_sub(36);
    if boundary > 0 && primary.as_bytes()[boundary - 1] != b'-' {
        return false;
    }
    matches!((uuid::Uuid::parse_str(primary_id), uuid::Uuid::parse_str(id)),
        (Ok(primary), Ok(header)) if primary == header && !header.is_nil())
}
fn plain(path: &Path) -> Option<fs::Metadata> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() {
        return None;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return None;
        }
    }
    Some(metadata)
}
fn normalize(path: &str) -> String {
    let path = path.replace('/', "\\");
    path.strip_prefix("\\\\?\\")
        .unwrap_or(&path)
        .trim_end_matches('\\')
        .to_lowercase()
}
fn absolute_workspace(path: &str) -> bool {
    Path::new(path).is_absolute()
        || (path.as_bytes().get(1) == Some(&b':')
            && matches!(path.as_bytes().get(2), Some(b'\\' | b'/')))
}
fn text_timestamp(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_i64().map(|n| n.to_string()))
}
fn percent_decode(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            out.push(
                u8::from_str_radix(std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?, 16).ok()?,
            );
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    #[test]
    fn personal_repo_scope_honors_off_and_never_falls_back_to_a_workspace() {
        use crate::{PersonalRepoHealth, PersonalRepoSource, PersonalRepoStatus};
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().display().to_string();
        let mut status = PersonalRepoStatus {
            folder: Some(folder), source: PersonalRepoSource::Environment,
            status: PersonalRepoHealth::Off, message: None,
        };
        assert!(discover_personal_repo(&status).unwrap_err().contains("disabled"));
        status.status = PersonalRepoHealth::On;
        let scope = discovery_scope(&status).unwrap();
        assert_eq!(scope, crate::child_process_path(&root.path().canonicalize().unwrap()).display().to_string());
        assert!(!scope.starts_with(r"\\?\"), "presentation path, not the filesystem's verbatim spelling");
        // A failed register writer does not disable read-only metadata discovery.
        status.status = PersonalRepoHealth::Error;
        status.message = Some("The session register writer is not running".into());
        assert!(discovery_scope(&status).is_ok());
        status.folder = Some(root.path().join("missing").display().to_string());
        assert!(discovery_scope(&status).is_err());
        status.folder = Some("relative".into());
        assert!(discovery_scope(&status).is_err());
        status.folder = None;
        assert!(discovery_scope(&status).is_err());
    }
    #[test]
    fn off_discovery_reports_the_actual_status_reason() {
        use crate::{PersonalRepoHealth, PersonalRepoSource, PersonalRepoStatus};
        for (source, reason) in [
            (PersonalRepoSource::Default, "No default personal repository is available"),
            (PersonalRepoSource::Setting, "This preview is held; the session register is disabled"),
            (PersonalRepoSource::Environment, "PRIM1_PERSONAL_REPO=off disables the session register"),
        ] {
            let status = PersonalRepoStatus { folder: None, source, status: PersonalRepoHealth::Off,
                message: Some(reason.into()) };
            let error = discovery_scope(&status).unwrap_err();
            assert!(error.contains(reason));
            assert_eq!(error.contains("Remove PRIM1_PERSONAL_REPO=off"), source == PersonalRepoSource::Environment);
        }
    }
    #[cfg(windows)]
    #[test]
    fn metadata_only_exact_workspace_and_provider_errors_are_independent() {
        let dir = tempfile::tempdir().unwrap();
        let c = dir.path().join("codex");
        let a = dir.path().join("claude");
        let g = dir.path().join("grok");
        let id = "00000000-0000-4000-8000-000000000001";
        write(
            &c.join(format!("sessions/2026/10/19/rollout-test-{id}.jsonl")),
            &format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"C:/work\",\"thread_source\":\"user\"}}}}\nnot JSON: private transcript must not be read"
            ),
        );
        write(
            &a.join("sessions/42.json"),
            &format!("{{\"sessionId\":\"{id}\",\"cwd\":\"C:/elsewhere\",\"name\":\"Other\"}}"),
        );
        fs::create_dir_all(g.join(format!("sessions/C%3A%5Cwork/{id}"))).unwrap();
        let got = discover_at(r"\\?\C:\WORK\", &c, &a, &g);
        assert_eq!(got.candidates.len(), 2);
        assert_eq!(got.candidates[0].kind, ConversationKind::Root);
        assert_eq!(got.candidates[1].kind, ConversationKind::Unknown);
        let bad = discover_at("C:/work", &dir.path().join("missing"), &a, &g);
        assert!(!bad.providers[0].errors.is_empty());
        assert_eq!(bad.candidates.len(), 1);
    }
    #[test]
    fn duplicates_conflict_and_oversized_headers_do_not_scan_forward() {
        let dir = tempfile::tempdir().unwrap();
        let id = "00000000-0000-4000-8000-000000000001";
        for (day, source) in [("18", "user"), ("19", "subagent")] {
            write(
                &dir.path()
                    .join(format!("sessions/2026/10/{day}/rollout-{id}.jsonl")),
                &format!(
                    "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"C:/work\",\"thread_source\":\"{source}\"}}}}\n"
                ),
            );
        }
        write(
            &dir.path().join("sessions/rollout-too-big.jsonl"),
            &"x".repeat(HEADER_BYTES as usize + 1),
        );
        let got = discover_at(
            "C:/work",
            dir.path(),
            &dir.path().join("none"),
            &dir.path().join("none"),
        );
        assert_eq!(got.candidates.len(), 1);
        assert!(got.candidates[0].conflict);
        assert_eq!(got.candidates[0].metadata_paths.len(), 2);
        assert_eq!(got.candidates[0].kind, ConversationKind::Unknown);
        assert!(got.providers[0].errors.iter().any(|e| e.contains("64 KiB")));
    }
    #[test]
    fn claude_ambiguous_project_path_is_not_an_exact_workspace_match() {
        let dir = tempfile::tempdir().unwrap();
        let id = "00000000-0000-4000-8000-000000000001";
        write(
            &dir.path().join(format!("projects/C--work/{id}.jsonl")),
            &format!(
                "{{\"type\":\"custom-title\",\"sessionId\":\"{id}\",\"customTitle\":\"Named\"}}\n"
            ),
        );
        let got = discover_at(
            "C:/work",
            &dir.path().join("none"),
            dir.path(),
            &dir.path().join("none"),
        );
        assert!(got.candidates.is_empty());
        assert!(
            got.providers[1]
                .errors
                .iter()
                .any(|e| e.contains("ambiguous"))
        );
    }
    fn metadata_scan() -> Scan {
        Scan {
            status: ProviderStatus {
                provider: "claude".into(),
                ..Default::default()
            },
            started: Instant::now(),
            root: PathBuf::new(),
            wanted: normalize("C:/work"),
            rows: BTreeMap::new(),
            codex_names: BTreeMap::new(),
            codex_cli_paths: BTreeSet::new(),
            observed_workspaces: BTreeMap::new(),
            timestamp_conflicts: BTreeSet::new(),
        }
    }
    #[test]
    fn claude_preamble_records_find_exact_metadata_then_stop() {
        let dir = tempfile::tempdir().unwrap();
        let id = "00000000-0000-4000-8000-000000000001";
        let path = dir.path().join(format!("projects/C--work/{id}.jsonl"));
        for workspace_key in ["cwd", "projectPath"] {
            write(
                &path,
                &format!(
                    "{{\"type\":\"custom-title\",\"sessionId\":\"{id}\",\"customTitle\":\"Renamed\"}}\n{{\"type\":\"mode\"}}\n{{\"sessionId\":\"{id}\",\"{workspace_key}\":\"C:/work\",\"isSidechain\":false,\"message\":{{\"content\":\"private sentinel\"}}}}\ninvalid trailing record"
                ),
            );
            let got = discover_at(
                "C:/work",
                &dir.path().join("none"),
                dir.path(),
                &dir.path().join("none"),
            );
            assert_eq!(got.candidates.len(), 1);
            assert_eq!(got.candidates[0].label.as_deref(), Some("Renamed"));
            assert_eq!(got.candidates[0].kind, ConversationKind::Root);
            assert!(got.providers[1].errors.is_empty());
            assert!(
                !serde_json::to_string(&got)
                    .unwrap()
                    .contains("private sentinel")
            );
        }
        // A colliding encoded folder cannot establish the record's workspace.
        write(
            &path,
            &serde_json::json!({"sessionId": id, "cwd": "C:/other"}).to_string(),
        );
        assert!(
            discover_at(
                "C:/work",
                &dir.path().join("none"),
                dir.path(),
                &dir.path().join("none")
            )
            .candidates
            .is_empty()
        );
    }
    #[test]
    fn claude_identity_scan_stops_at_byte_and_record_limits() {
        let dir = tempfile::tempdir().unwrap();
        let id = "00000000-0000-4000-8000-000000000001";
        let path = dir.path().join(format!("projects/C--work/{id}.jsonl"));
        let header = serde_json::json!({"sessionId": id, "cwd": "C:/work"}).to_string();
        for prefix in [
            "{}\n".repeat(CLAUDE_PREFIX_RECORDS),
            format!(
                "{{\"message\":\"{}\"}}\n",
                "x".repeat(CLAUDE_PREFIX_BYTES as usize)
            ),
        ] {
            write(&path, &(prefix + &header));
            let got = discover_at(
                "C:/work",
                &dir.path().join("none"),
                dir.path(),
                &dir.path().join("none"),
            );
            assert!(got.candidates.is_empty());
            assert!(got.providers[1].truncated && got.truncated);
            assert!(
                got.providers[1]
                    .errors
                    .iter()
                    .any(|error| error.contains("scan limit"))
            );
        }
        write(&path, &("{}\n".repeat(CLAUDE_PREFIX_RECORDS - 1) + &header));
        assert!(jsonl_metadata(&path, true).is_ok(), "the sixteenth complete metadata record is admitted");
    }
    #[cfg(windows)]
    #[test]
    fn claude_skips_a_giant_first_record_without_retaining_its_body() {
        let dir = tempfile::tempdir().unwrap();
        let id = "00000000-0000-4000-8000-000000000001";
        let path = dir.path().join(format!("projects/C--work/{id}.jsonl"));
        let giant = format!("{{\"message\":\"private sentinel {}\"}}\n", "x".repeat(HEADER_BYTES as usize * 2));
        let metadata = serde_json::json!({"sessionId": id, "cwd": "C:/work", "isSidechain": false,
            "message": {"content": "private second sentinel"}}).to_string();
        write(&path, &(giant.clone() + &metadata + "\ninvalid later record"));
        let value = jsonl_metadata(&path, true).unwrap();
        assert_eq!(value["sessionId"], id);
        assert!(value.get("message").is_none());
        let got = discover_at("C:/work", &dir.path().join("none"), dir.path(), &dir.path().join("none"));
        assert_eq!(got.candidates.len(), 1);
        assert_eq!(got.candidates[0].kind, ConversationKind::Root);
        assert!(!got.providers[1].truncated);
        assert!(!serde_json::to_string(&got).unwrap().contains("private"));
        let mut reader = std::io::Cursor::new(giant.as_bytes());
        let mut retained = Vec::new();
        let (count, complete, fits) = bounded_record(&mut reader, &mut retained).unwrap();
        assert_eq!(count, giant.len());
        assert!(complete && !fits && retained.is_empty());
    }
    #[test]
    fn claude_tail_uses_latest_custom_title_else_latest_ai_for_the_same_id() {
        let dir = tempfile::tempdir().unwrap();
        let id = "00000000-0000-4000-8000-000000000001";
        let other_id = "00000000-0000-4000-8000-000000000002";
        let path = dir.path().join(format!("projects/C--work/{id}.jsonl"));
        write(&dir.path().join("sessions/42.json"), &serde_json::json!({"sessionId": id,
            "cwd": "C:/work", "name": "Stale PID title"}).to_string());
        let prefix = serde_json::json!({"sessionId": id, "cwd": "C:/work", "isSidechain": false}).to_string()
            + "\n" + &serde_json::json!({"type": "custom-title", "sessionId": id,
                "customTitle": "Old title outside the tail"}).to_string() + "\n"
            + &format!("{{\"message\":\"private sentinel {}\"}}\n", "x".repeat(HEADER_BYTES as usize * 2));
        for custom in [false, true] {
            let mut tail = String::new();
            for (kind, field, name) in [("ai-title", "aiTitle", "Old AI"),
                ("custom-title", "customTitle", "Old custom"), ("custom-title", "customTitle", "Renamed"),
                ("ai-title", "aiTitle", "Latest AI")] {
                if kind == "custom-title" && !custom { continue; }
                tail += &(serde_json::json!({"type": kind, "sessionId": id, field: name}).to_string() + "\n");
            }
            tail += &serde_json::json!({"type": "custom-title", "sessionId": other_id,
                "customTitle": "Wrong conversation"}).to_string();
            write(&path, &(prefix.clone() + &tail));
            let got = discover_at("C:/work", &dir.path().join("none"), dir.path(), &dir.path().join("none"));
            assert_eq!(got.candidates.len(), 1);
            assert_eq!(got.candidates[0].label.as_deref(), Some(if custom { "Renamed" } else { "Latest AI" }));
            assert!(!got.candidates[0].conflict);
            assert!(!serde_json::to_string(&got).unwrap().contains("private sentinel"));
        }
        // No title in the bounded tail: do not seek backwards through history.
        write(&path, &(prefix + "{}\n"));
        assert!(claude_tail_title(&mut fs::File::open(&path).unwrap(), id).unwrap().is_none());
    }
    #[test]
    fn codex_attach_requires_cli_evidence_on_the_supported_rollout_itself() {
        let dir = tempfile::tempdir().unwrap();
        let id = "00000000-0000-4000-8000-000000000001";
        let suffix = "00000000-0000-4000-8000-000000000002";
        let canonical = dir.path().join(format!("sessions/rollout-{id}.jsonl"));
        let suffixed = dir.path().join(format!("sessions/rollout-{id}_{suffix}.jsonl"));
        let header = |source: &str| serde_json::json!({"type": "session_meta", "payload": {
            "id": id, "cwd": "C:/work", "thread_source": "user", "source": source}}).to_string();
        for source in ["vscode", "exec", "unknown"] {
            write(&canonical, &header(source));
            let got = discover_at("C:/work", dir.path(), &dir.path().join("none"), &dir.path().join("none"));
            assert_eq!(got.candidates[0].kind, ConversationKind::Root);
            assert!(got.candidates[0].attach_unavailable.as_deref().unwrap().contains("root CLI source"));
        }
        write(&suffixed, &header("cli"));
        let got = discover_at("C:/work", dir.path(), &dir.path().join("none"), &dir.path().join("none"));
        assert!(got.candidates[0].attach_unavailable.is_some(), "a CLI sibling cannot validate a non-CLI canonical file");
        write(&canonical, &header("cli"));
        write(&suffixed, &header("vscode"));
        let got = discover_at("C:/work", dir.path(), &dir.path().join("none"), &dir.path().join("none"));
        assert!(got.candidates[0].attach_unavailable.is_none(), "the canonical root CLI source is usable");
        assert!(!got.candidates[0].conflict, "source availability is separate from identity conflict");
    }
    #[test]
    fn renamed_claude_titles_preserve_identity_in_either_source_order() {
        let id = "00000000-0000-4000-8000-000000000001";
        for names in [["Old title", "New title"], ["New title", "Old title"]] {
            let mut scan = metadata_scan();
            for (n, name) in names.iter().enumerate() {
                scan.claude_record(Path::new(&format!("sessions/{n}.json")),
                    &serde_json::json!({"sessionId": id, "cwd": "C:/work", "name": name, "isSidechain": false}));
            }
            assert!(!scan.rows[id].conflict);
            assert_eq!(scan.rows[id].kind, ConversationKind::Root);
            assert_eq!(scan.rows[id].label.as_deref(), Some(names[0]));
        }
    }
    #[test]
    fn codex_rollout_suffix_requires_primary_header_match_and_uuid_suffix() {
        let id = "00000000-0000-4000-8000-000000000001";
        let suffix = "00000000-0000-4000-8000-000000000002";
        let name = format!("rollout-2026-10-19T12-00-00-{id}_{suffix}.jsonl");
        assert!(codex_filename_matches(Path::new(&name), id));
        assert!(!codex_filename_matches(Path::new(&name), suffix));
        for bad in [
            format!("rollout-{id}_arbitrary.jsonl"),
            format!("rollout-{id}_{suffix}_extra.jsonl"),
            format!("other-{id}_{suffix}.jsonl"),
        ] {
            assert!(!codex_filename_matches(Path::new(&bad), id));
        }
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("sessions").join(name),
            &serde_json::json!({"type": "session_meta",
            "payload": {"id": id, "cwd": "C:/work", "thread_source": "user"}})
            .to_string(),
        );
        let got = discover_at(
            "C:/work",
            dir.path(),
            &dir.path().join("none"),
            &dir.path().join("none"),
        );
        assert_eq!(got.candidates.len(), 1);
        assert_eq!(got.candidates[0].id, id);
    }
    #[test]
    fn newest_codex_dates_are_processed_first_and_admitted_entries_survive_cap() {
        let dir = tempfile::tempdir().unwrap();
        let id = "00000000-0000-4000-8000-000000000001";
        for (day, title) in [("01", "Old"), ("19", "New")] {
            write(
                &dir.path()
                    .join(format!("sessions/2026/10/{day}/rollout-{id}.jsonl")),
                &serde_json::json!({"type": "session_meta", "payload": {
                    "id": id, "cwd": "C:/work", "name": title, "thread_source": "user"}})
                .to_string(),
            );
        }
        let got = discover_at(
            "C:/work",
            dir.path(),
            &dir.path().join("none"),
            &dir.path().join("none"),
        );
        assert_eq!(got.candidates[0].label.as_deref(), Some("New"));
        assert!(!got.candidates[0].conflict);
        let limited = tempfile::tempdir().unwrap();
        for n in 1..=3 {
            write(
                &limited.path().join(format!("sessions/{n}.json")),
                &serde_json::json!({"sessionId": uuid::Uuid::from_u128(n), "cwd": "C:/work"})
                    .to_string(),
            );
        }
        let mut scan = metadata_scan();
        scan.root = limited.path().canonicalize().unwrap();
        scan.status.entries_examined = MAX_ENTRIES - 2;
        scan.walk(scan.root.join("sessions"), 0);
        assert_eq!(scan.status.entries_examined, MAX_ENTRIES);
        assert_eq!(scan.rows.len(), 2);
        assert!(scan.status.truncated);
    }
    #[test]
    fn grok_skips_unrelated_workspace_children_before_inspection() {
        let dir = tempfile::tempdir().unwrap();
        let id = "00000000-0000-4000-8000-000000000001";
        write(
            &dir.path()
                .join(format!("sessions/C%3A%5Cother/{id}/summary.json")),
            "invalid private data",
        );
        write(
            &dir.path()
                .join(format!("sessions/C%3A%5Cwork/{id}/summary.json")),
            &serde_json::json!({"info": {"id": id, "cwd": "C:/work"}}).to_string(),
        );
        let got = discover_at(
            "C:/work",
            &dir.path().join("none"),
            &dir.path().join("none"),
            dir.path(),
        );
        assert_eq!(got.candidates.len(), 1);
        assert_eq!(got.providers[2].entries_examined, 3); // Two workspace folders + one admitted ID.
        assert!(got.providers[2].errors.is_empty());
    }
    #[test]
    fn claude_pid_and_index_complement_each_other_in_either_order() {
        let id = "00000000-0000-4000-8000-000000000001";
        for name_in_pid in [false, true] {
            let pid = serde_json::json!({"sessionId": id, "cwd": "C:/work",
                "name": if name_in_pid { Some("Work") } else { None }});
            let index = serde_json::json!({"entries": [{"sessionId": id, "projectPath": "C:/work",
                "customTitle": if name_in_pid { None } else { Some("Work") },
                "modified": "2026-10-19T10:00:00Z", "isSidechain": false}]});
            for index_first in [false, true] {
                let mut scan = metadata_scan();
                let pid_record = (Path::new("sessions/42.json"), &pid);
                let index_record = (Path::new("projects/C--work/sessions-index.json"), &index);
                let records = if index_first {
                    [index_record, pid_record]
                } else {
                    [pid_record, index_record]
                };
                for (path, record) in records {
                    scan.claude(path, record);
                }
                let row = &scan.rows[id];
                assert!(!row.conflict);
                assert_eq!(row.kind, ConversationKind::Root);
                assert_eq!(row.label.as_deref(), Some("Work"));
                assert_eq!(row.updated_at.as_deref(), Some("2026-10-19T10:00:00Z"));
                assert_eq!(row.metadata_paths.len(), 2);
            }
        }
    }
    #[test]
    fn differing_timestamps_do_not_invalidate_identity_or_reappear_later() {
        let id = "00000000-0000-4000-8000-000000000001";
        let earlier = "2026-10-19T10:00:00Z";
        let later = "2026-10-19T11:00:00Z";
        for times in [[earlier, later, earlier], [later, earlier, later]] {
            let mut scan = metadata_scan();
            for (n, time) in times.into_iter().enumerate() {
                scan.claude_record(
                    Path::new(&format!("sessions/{n}.json")),
                    &serde_json::json!({"sessionId": id, "cwd": "C:/work", "name": "Work",
                        "isSidechain": false, "updatedAt": time}),
                );
            }
            let row = &scan.rows[id];
            assert!(!row.conflict);
            assert_eq!(row.kind, ConversationKind::Root);
            assert_eq!(row.label.as_deref(), Some("Work"));
            assert_eq!(row.updated_at, None);
        }
    }
    #[test]
    fn contradictory_kinds_remain_conflicted_after_complementary_metadata() {
        let id = "00000000-0000-4000-8000-000000000001";
        let mut scan = metadata_scan();
        for sidechain in [Some(false), Some(true), None, Some(false)] {
            scan.claude_record(Path::new("sessions/42.json"),
                &serde_json::json!({"sessionId": id, "cwd": "C:/work", "name": "Work", "isSidechain": sidechain}));
        }
        let row = &scan.rows[id];
        assert!(row.conflict);
        assert_eq!(row.kind, ConversationKind::Unknown);
        assert_eq!(row.label, None);
    }
    #[test]
    fn workspace_conflict_evidence_is_capped_even_for_large_variants() {
        let id = "00000000-0000-4000-8000-000000000001";
        let mut scan = metadata_scan();
        scan.insert(
            Path::new("matching.json"),
            id,
            "C:/work",
            None,
            None,
            ConversationKind::Root,
            "fixture",
        );
        let padding = "x".repeat(HEADER_BYTES as usize / 2);
        for n in 0..MAX_SOURCES + 2 {
            scan.insert(
                Path::new(&format!("variant-{n}.json")),
                id,
                &format!("C:/other/{n}/{padding}"),
                None,
                None,
                ConversationKind::Root,
                "fixture",
            );
        }
        assert_eq!(scan.observed_workspaces[id].len(), MAX_SOURCES);
        assert!(scan.status.truncated);
        assert!(scan.rows[id].conflict);
        assert_eq!(scan.rows[id].kind, ConversationKind::Unknown);
        assert!(scan.rows[id].metadata_paths.len() <= MAX_SOURCES);
    }
    #[test]
    fn grok_summary_conflicts_and_nested_subagents_are_not_root_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let id = "00000000-0000-4000-8000-000000000001";
        let root = dir.path().join(format!("sessions/C%3A%5Cwork/{id}"));
        write(
            &root.join("summary.json"),
            &format!(
                "{{\"info\":{{\"id\":\"{id}\",\"cwd\":\"C:/work\"}},\"generated_title\":\"Root\",\"updated_at\":\"2026-10-19T10:00:00Z\"}}"
            ),
        );
        write(
            &root.join("subagents/00000000-0000-4000-8000-000000000002/meta.json"),
            "not read",
        );
        let got = discover_at(
            "C:/work",
            &dir.path().join("none"),
            &dir.path().join("none"),
            dir.path(),
        );
        assert_eq!(got.candidates.len(), 1);
        assert_eq!(got.candidates[0].label.as_deref(), Some("Root"));
        assert_eq!(got.candidates[0].kind, ConversationKind::Root);
        write(
            &root.join("summary.json"),
            &format!("{{\"info\":{{\"id\":\"{id}\",\"cwd\":\"C:/elsewhere\"}}}}"),
        );
        let got = discover_at(
            "C:/work",
            &dir.path().join("none"),
            &dir.path().join("none"),
            dir.path(),
        );
        assert!(got.candidates.is_empty());
        assert!(
            got.providers[2]
                .errors
                .iter()
                .any(|e| e.contains("conflict"))
        );
    }
    #[test]
    fn codex_name_index_renames_do_not_conflict_with_identity() {
        let dir = tempfile::tempdir().unwrap();
        let mut index = String::new();
        for n in 1..=1 {
            let id = uuid::Uuid::from_u128(n as u128).to_string();
            write(
                &dir.path().join(format!("sessions/rollout-{id}.jsonl")),
                &format!(
                    "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"C:/work\",\"thread_source\":\"user\"}}}}\n"
                ),
            );
            if n == 1 {
                index = format!(
                    "{{\"id\":\"{id}\",\"thread_name\":\"First\"}}\n{{\"id\":\"{id}\",\"thread_name\":\"Different\"}}\n"
                );
            }
        }
        write(&dir.path().join("session_index.jsonl"), &index);
        let got = discover_at(
            "C:/work",
            dir.path(),
            &dir.path().join("none"),
            &dir.path().join("none"),
        );
        assert_eq!(got.candidates.len(), 1);
        assert!(!got.candidates[0].conflict);
        assert_eq!(got.candidates[0].label.as_deref(), Some("Different"));
    }
    #[test]
    fn codex_index_timestamp_churn_keeps_the_name_without_choosing_recency() {
        let id = "00000000-0000-4000-8000-000000000001";
        let earlier = Some("2026-10-19T10:00:00Z");
        let later = Some("2026-10-19T11:00:00Z");
        for (times, expected) in [
            ([earlier, later, earlier], None),
            ([None, earlier, earlier], None),
            ([earlier, earlier, earlier], earlier),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let index = times
                .into_iter()
                .map(|time| {
                    serde_json::json!({"id": id, "thread_name": "Work", "updated_at": time})
                        .to_string()
                        + "\n"
                })
                .collect::<String>();
            write(&dir.path().join("session_index.jsonl"), &index);
            write(
                &dir.path().join(format!("sessions/rollout-{id}.jsonl")),
                &serde_json::json!({"type": "session_meta", "payload": {
                    "id": id, "cwd": "C:/work", "thread_source": "user"}})
                .to_string(),
            );
            let got = discover_at(
                "C:/work",
                dir.path(),
                &dir.path().join("none"),
                &dir.path().join("none"),
            );
            let row = &got.candidates[0];
            assert!(!row.conflict);
            assert_eq!(row.label.as_deref(), Some("Work"));
            assert_eq!(row.updated_at.as_deref(), expected);
            assert_eq!(row.kind, ConversationKind::Root);
        }
    }
    #[test]
    fn cross_workspace_conflicts_and_result_limit_are_order_independent() {
        for workspaces in [["C:/work", "C:/other"], ["C:/other", "C:/work"]] {
            let mut scan = Scan {
                status: ProviderStatus::default(),
                started: Instant::now(),
                root: PathBuf::new(),
                wanted: normalize("C:/work"),
                rows: BTreeMap::new(),
                codex_names: BTreeMap::new(),
                codex_cli_paths: BTreeSet::new(),
                observed_workspaces: BTreeMap::new(),
                timestamp_conflicts: BTreeSet::new(),
            };
            let id = uuid::Uuid::from_u128(1).to_string();
            for workspace in workspaces {
                scan.insert(
                    Path::new(workspace),
                    &id,
                    workspace,
                    None,
                    None,
                    ConversationKind::Root,
                    "fixture",
                );
            }
            assert_eq!(scan.rows.len(), 1);
            assert!(scan.rows[&id].conflict);
            assert_eq!(scan.rows[&id].metadata_paths.len(), 2);
            for n in 2..=MAX_CANDIDATES + 1 {
                scan.insert(
                    Path::new("fixture"),
                    &uuid::Uuid::from_u128(n as u128).to_string(),
                    "C:/work",
                    None,
                    None,
                    ConversationKind::Root,
                    "fixture",
                );
            }
            assert_eq!(scan.rows.len(), MAX_CANDIDATES);
            assert!(scan.status.truncated);
        }
    }
    #[cfg(unix)]
    #[test]
    fn linked_store_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("sessions")).unwrap();
        let got = discover_at(
            "C:/work",
            dir.path(),
            &dir.path().join("none"),
            &dir.path().join("none"),
        );
        assert!(got.candidates.is_empty());
        assert!(got.providers[0].errors.iter().any(|e| e.contains("link")));
    }
}
