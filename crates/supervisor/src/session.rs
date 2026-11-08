//! Requested launch choices and conversation recovery evidence. Conversation
//! selection never guesses by directory, modification time, or display name.

use std::{
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use shared_types::{DriverKind, LaunchSpec, SessionBriefChoice, SessionLaunchChoices};
use uuid::Uuid;

pub(crate) fn default_launch_choices(driver: DriverKind) -> SessionLaunchChoices {
    SessionLaunchChoices {
        brief: if matches!(driver, DriverKind::GenericTerminal | DriverKind::Prime) {
            SessionBriefChoice::None {}
        } else {
            SessionBriefChoice::RoomDefault {}
        },
        ..SessionLaunchChoices::default()
    }
}

pub(crate) fn validate_launch_choices(
    driver: DriverKind,
    choices: &SessionLaunchChoices,
) -> Result<()> {
    if matches!(driver, DriverKind::GenericTerminal | DriverKind::Prime) {
        if choices.model.is_some()
            || choices.effort.is_some()
            || choices.brief != (SessionBriefChoice::None {})
        {
            bail!("Terminal and Prime do not accept model, effort, or brief choices");
        }
        return Ok(());
    }
    if let Some(model) = &choices.model {
        if model.is_empty()
            || model.len() > 256
            || model.starts_with('-')
            || model.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            bail!("model must be a non-empty model name of at most 256 bytes");
        }
        if model.to_ascii_lowercase().contains("haiku") {
            bail!("Haiku is disabled by the operator's model policy");
        }
    }
    if let Some(effort) = &choices.effort {
        if effort.is_empty()
            || effort.len() > 64
            || effort.starts_with('-')
            || !effort
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
        {
            bail!("effort must be a non-empty effort name of at most 64 bytes");
        }
        // Installed Claude help enumerates these CLI values. Codex/Grok expose
        // model-advertised strings: do not fabricate per-model capability lists.
        if driver == DriverKind::Claude
            && !matches!(effort.as_str(), "low" | "medium" | "high" | "xhigh" | "max")
        {
            bail!("Claude effort must be low, medium, high, xhigh, or max");
        }
    }
    if let SessionBriefChoice::Custom { text } = &choices.brief {
        validate_launch_prompt(text)?;
    }
    Ok(())
}

pub(crate) fn validate_launch_prompt(text: &str) -> Result<()> {
    if text.len() > 8 * 1024 {
        bail!(
            "rendered brief is {} bytes; maximum 8192 bytes. Shorten the brief/template or choose Custom/None for this launch",
            text.len()
        );
    }
    if text.trim().is_empty() || text.contains('\0') {
        bail!("rendered brief must contain text and contain no NUL");
    }
    Ok(())
}

/// Claude's Commander dispatches a single operand matching a subcommand even
/// after `--`. A fixed ordinary user-prompt envelope prevents that dispatch;
/// it changes neither system instructions nor permissions and keeps all text.
pub(crate) fn initial_prompt_argument(driver: DriverKind, text: String) -> Result<String> {
    validate_launch_prompt(&text)?;
    if matches!(driver, DriverKind::GenericTerminal | DriverKind::Prime) {
        bail!("Terminal and Prime must never receive an initial brief");
    }
    Ok(if driver == DriverKind::Claude {
        format!("PRIM-1 initial brief:\n\n{text}")
    } else {
        text
    })
}

/// Call after all driver/MCP options. Callers supply a prompt only for a new
/// conversation; a resume must never re-deliver the task brief.
/// Arguments go straight to the process API; the prompt is never shell code or PTY input.
pub(crate) fn apply_launch_choices(
    spec: &mut LaunchSpec,
    driver: DriverKind,
    choices: &SessionLaunchChoices,
    prompt: Option<String>,
) -> Result<()> {
    validate_launch_choices(driver, choices)?;
    if let Some(model) = &choices.model {
        spec.args.extend(["--model".into(), model.clone()]);
    }
    if let Some(effort) = &choices.effort {
        match driver {
            DriverKind::Claude => spec.args.extend(["--effort".into(), effort.clone()]),
            DriverKind::Grok => spec
                .args
                .extend(["--reasoning-effort".into(), effort.clone()]),
            DriverKind::Codex => spec
                .args
                .extend(["-c".into(), format!("model_reasoning_effort=\"{effort}\"")]),
            _ => unreachable!("terminal choices were validated"),
        }
    }
    if let Some(prompt) = prompt {
        spec.args
            .extend(["--".into(), initial_prompt_argument(driver, prompt)?]);
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryReference {
    pub original_id: Option<String>,
    pub provenance: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_codex: Option<CodexIdentityEvidence>,
}

/// A report observed in one attested run. It is not proof that the harness
/// has kept that conversation active since then (notably after /new).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodexIdentityEvidence {
    pub thread_id: String,
    pub run_id: Uuid,
    pub generation: u64,
    pub rollout_path: PathBuf,
    pub reported_at: String,
}

impl RecoveryReference {
    pub(crate) fn duplicate_catalog_identity(original_id: String, sessions: &[Uuid]) -> Self {
        Self {
            original_id: Some(original_id),
            provenance: "duplicate_catalog_identity".into(),
            reason: format!(
                "This imported conversation reference collided with PRIM session references [{}]. Resume remains held; no replacement or owner was guessed.",
                sessions
                    .iter()
                    .map(Uuid::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            reported_codex: None,
        }
    }

    /// A108: this install's register file names another conversation than
    /// the catalog. Both are shown; neither is chosen.
    pub(crate) fn register_disagrees(catalog_id: Option<String>, registered_id: String, register_file: &Path) -> Self {
        Self {
            reason: format!(
                "The session register ({}) names conversation {registered_id} for this member, but this PRIM-1's catalog names {}. Resume remains held; neither was chosen.",
                register_file.display(),
                catalog_id.as_deref().unwrap_or("none"),
            ),
            original_id: catalog_id,
            provenance: crate::session_ledger::REGISTER_DISAGREES.into(),
            reported_codex: None,
        }
    }

    /// A108: this install's register names one conversation for this member
    /// and for others. No owner is guessed.
    pub(crate) fn register_duplicate(conversation: String, others: &[Uuid], register_file: &Path) -> Self {
        Self {
            reason: format!(
                "The session register ({}) names conversation {conversation} for this member and for PRIM session references [{}]. Resume remains held; no owner was guessed.",
                register_file.display(),
                others.iter().map(Uuid::to_string).collect::<Vec<_>>().join(", "),
            ),
            original_id: Some(conversation),
            provenance: crate::session_ledger::REGISTER_DUPLICATE.into(),
            reported_codex: None,
        }
    }

    pub(crate) fn unresolved_codex(original_id: Option<String>, imported: bool) -> Self {
        // `imported` marks a conversation PRIM-1 did not bind in this process:
        // a catalog reload (its own member after a restart) or a room import.
        // The flag cannot tell them apart, so the copy is neutral for both
        // (Claude PRIM-1, 25 Oct); the provenance and the hold are unchanged.
        let reason = match (imported, original_id.is_some()) {
            (true, true) => {
                "The stored Codex conversation has no verified launch binding. Its ID is retained; no replacement was guessed."
            }
            (true, false) => {
                "The stored Codex session has no verified conversation ID. No replacement was guessed."
            }
            (false, _) => {
                "Codex identity is unresolved. Resume and native wake remain unavailable until the current root and conversation freshness are proved."
            }
        };
        Self {
            original_id,
            provenance: if imported {
                "legacy_catalog"
            } else {
                "codex_launch"
            }
            .into(),
            reason: reason.into(),
            reported_codex: None,
        }
    }

    pub(crate) fn snapshot(&self) -> shared_types::SessionRecovery {
        shared_types::SessionRecovery {
            original_id: self.original_id.clone(),
            provenance: self.provenance.clone(),
            reason: self.reason.clone(),
        }
    }

    pub(crate) fn hold(&self, running: bool) -> RecoveryHold {
        RecoveryHold(format!(
            "{} {}",
            self.reason,
            if running {
                "Stop this run, then choose Start fresh session. Prior references will be retained."
            } else {
                "Choose Start fresh session to start a new conversation. Prior references will be retained."
            }
        ))
    }

    pub(crate) fn resume_refused(original_id: Option<String>, reason: String) -> Self {
        Self {
            original_id,
            provenance: "harness_resume_refusal".into(),
            reason,
            reported_codex: None,
        }
    }
}

/// An intentional conversation hold must not be reported as a process failure.
#[derive(Debug)]
pub(crate) struct RecoveryHold(pub String);

impl std::fmt::Display for RecoveryHold {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RecoveryHold {}

pub(crate) fn codex_sessions_root() -> Result<PathBuf> {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|path| PathBuf::from(path).join(".codex")))
        .or_else(|| cfg!(target_os = "macos").then(|| std::env::var_os("HOME")).flatten()
            .map(|path| PathBuf::from(path).join(".codex")))
        .context("Codex data directory is unavailable")?;
    if !home.is_absolute() {
        bail!("Codex data directory must be absolute");
    }
    Ok(home.join("sessions"))
}

pub(crate) fn canonical_thread_id(thread_id: &str) -> Result<String> {
    let id = Uuid::parse_str(thread_id).context("Codex thread ID must be a UUID")?;
    if id.is_nil() || id.to_string() != thread_id {
        bail!("Codex thread ID must be a canonical, non-nil UUID");
    }
    Ok(id.to_string())
}

/// No rollout file exists (yet) for the thread: a fresh thread holds its
/// writer lock before Codex writes it. Typed so a follower can wait.
#[derive(Debug)]
pub(crate) struct CodexRolloutMissing;

impl std::fmt::Display for CodexRolloutMissing {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("no rollout exists for the reported Codex thread ID")
    }
}

impl std::error::Error for CodexRolloutMissing {}

/// The rollout's first line is empty, partly written or not JSON yet: Codex
/// may still be writing it, so a follower waits rather than rejects.
#[derive(Debug)]
pub(crate) struct CodexRolloutIncomplete;

impl std::fmt::Display for CodexRolloutIncomplete {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Codex rollout metadata is incomplete or invalid")
    }
}

impl std::error::Error for CodexRolloutIncomplete {}

/// Rollout headers PRIM-1 reads. Codex 0.163 writes about 55 KB (its base
/// instructions); the bound leaves room for that to grow.
pub(crate) const CODEX_HEADER_LIMIT: u64 = 1024 * 1024;

#[derive(Debug)]
pub(crate) struct CodexRolloutHeaderTooLarge;

impl std::fmt::Display for CodexRolloutHeaderTooLarge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Codex rollout header exceeds the bounded metadata limit ({} KiB)", CODEX_HEADER_LIMIT / 1024)
    }
}

impl std::error::Error for CodexRolloutHeaderTooLarge {}

/// Inspect only exact-ID candidates. No newest-file or cwd-based selection.
/// Unknown metadata, duplicate segments, and bounded-search exhaustion refuse
/// identification rather than choosing a plausible file. Filesystem work runs
/// outside the supervisor's registry lock and async transport thread.
pub(crate) fn verify_codex_rollout(
    sessions_root: &Path,
    thread_id: &str,
    working_dir: &str,
) -> Result<PathBuf> {
    let thread_id = canonical_thread_id(thread_id)?;
    let suffix = format!("-{thread_id}.jsonl");
    let root = sessions_root
        .canonicalize()
        .context("Codex sessions directory is missing")?;
    let mut pending = vec![(root.clone(), 0usize)];
    let mut found = None;
    let mut entries_seen = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        for entry in fs::read_dir(&directory).context("cannot enumerate Codex sessions")? {
            let entry = entry?;
            entries_seen += 1;
            if entries_seen > 20_000 {
                bail!(
                    "Codex identity lookup exceeded its bounded search; identity stays unresolved"
                );
            }
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() && depth < 3 {
                let path = entry.path().canonicalize()?;
                if path.starts_with(&root) {
                    pending.push((path, depth + 1));
                }
            } else if kind.is_file() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with("rollout-") || !name.ends_with(&suffix) {
                    continue;
                }
                let path = entry.path().canonicalize()?;
                if !path.starts_with(&root) {
                    bail!("Codex rollout resolves outside its sessions directory");
                }
                if found.replace(path).is_some() {
                    bail!("multiple Codex rollouts match the reported ID; identity is ambiguous");
                }
            }
        }
    }
    let path = found.ok_or(CodexRolloutMissing)?;
    let file = fs::File::open(&path).context("cannot open reported Codex rollout")?;
    let mut line = Vec::new();
    BufReader::new(file.take(CODEX_HEADER_LIMIT + 1)).read_until(b'\n', &mut line)?;
    if line.len() as u64 > CODEX_HEADER_LIMIT {
        return Err(CodexRolloutHeaderTooLarge.into());
    }
    let header: serde_json::Value =
        serde_json::from_slice(&line).context(CodexRolloutIncomplete)?;
    let payload = &header["payload"];
    if header["type"].as_str() != Some("session_meta")
        || payload["id"].as_str() != Some(thread_id.as_str())
    {
        bail!("Codex rollout metadata does not match the reported thread ID");
    }
    if payload["thread_source"].as_str() != Some("user")
        || !payload["parent_thread_id"].is_null()
        || payload["source"].as_str() != Some("cli")
    {
        bail!("Codex rollout is not a recognized root CLI conversation");
    }
    let cwd = payload["cwd"]
        .as_str()
        .context("Codex rollout has no working directory")?;
    if normalize_windows_path(cwd) != normalize_windows_path(working_dir) {
        bail!("Codex rollout working directory differs from the attested run");
    }
    Ok(path)
}

pub(crate) fn normalize_windows_path(path: &str) -> String {
    let path = path.replace('/', "\\");
    path.strip_prefix("\\\\?\\")
        .unwrap_or(&path)
        .trim_end_matches('\\')
        .to_lowercase()
}

#[cfg(test)]
pub(crate) fn write_codex_fixture(root: &Path, thread_id: &str, cwd: &str) -> PathBuf {
    let directory = root.join("2026/10/13");
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("rollout-2026-10-13T12-00-00-{thread_id}.jsonl"));
    let header = serde_json::json!({
        "type": "session_meta",
        "payload": {"id": thread_id, "cwd": cwd, "thread_source": "user", "source": "cli"}
    });
    fs::write(&path, format!("{header}\n")).unwrap();
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_choices_validate_known_constraints_without_a_model_allowlist() {
        for driver in [DriverKind::Claude, DriverKind::Codex, DriverKind::Grok] {
            let mut choices = SessionLaunchChoices {
                model: Some("future-provider/model-v9".into()),
                effort: Some("max".into()),
                brief: SessionBriefChoice::None {},
            };
            assert!(validate_launch_choices(driver, &choices).is_ok());
            for model in ["", "--unsafe", "bad\nmodel", "claude-haiku-4-5", "HAIKU"] {
                choices.model = Some(model.into());
                assert!(
                    validate_launch_choices(driver, &choices).is_err(),
                    "{model}"
                );
            }
            choices.model = None;
            for effort in ["", "--unsafe", "max\"\nother=true", "high effort"] {
                choices.effort = Some(effort.into());
                assert!(
                    validate_launch_choices(driver, &choices).is_err(),
                    "{effort}"
                );
            }
            choices.effort = Some("deep".into());
            assert_eq!(
                validate_launch_choices(driver, &choices).is_ok(),
                driver != DriverKind::Claude
            );
        }
        for driver in [DriverKind::GenericTerminal, DriverKind::Prime] {
            assert!(validate_launch_choices(driver, &default_launch_choices(driver)).is_ok());
            assert!(validate_launch_choices(driver, &SessionLaunchChoices::default()).is_err());
        }
    }

    #[test]
    fn initial_prompt_is_one_last_argv_value_even_when_it_looks_like_shell_code() {
        let prompt = "--permission-mode bypassPermissions\n`hello` $(whoami) \"quoted\" Здравей";
        for driver in [DriverKind::Claude, DriverKind::Codex, DriverKind::Grok] {
            for prefix in [
                vec![],
                vec!["resume".into(), "thread-id".into()],
                vec!["--resume=thread-id".into(), "--fork-session".into()],
            ] {
                let choices = SessionLaunchChoices {
                    model: Some("requested-model".into()),
                    effort: Some("max".into()),
                    brief: SessionBriefChoice::Custom {
                        text: prompt.into(),
                    },
                };
                let mut spec = LaunchSpec {
                    program: "fixture.exe".into(),
                    args: prefix.clone(),
                    working_dir: "fixture".into(),
                    env: vec![],
                    display_name: "fixture".into(),
                };
                apply_launch_choices(&mut spec, driver, &choices, Some(prompt.into())).unwrap();
                assert_eq!(&spec.args[..prefix.len()], prefix.as_slice());
                let expected = initial_prompt_argument(driver, prompt.into()).unwrap();
                assert_eq!(
                    &spec.args[spec.args.len() - 2..],
                    &["--", expected.as_str()]
                );
                let expected_effort = match driver {
                    DriverKind::Claude => ["--effort", "max"],
                    DriverKind::Grok => ["--reasoning-effort", "max"],
                    _ => ["-c", "model_reasoning_effort=\"max\""],
                };
                assert!(spec.args.windows(2).any(|args| args == expected_effort));
            }
        }
    }

    #[test]
    fn codex_identity_uses_exact_reported_id_among_same_cwd_roots() {
        let root = tempfile::tempdir().unwrap();
        let first = Uuid::new_v4().to_string();
        let second = Uuid::new_v4().to_string();
        let expected = write_codex_fixture(root.path(), &first, r"C:\work");
        write_codex_fixture(root.path(), &second, r"C:\work");
        assert_eq!(
            verify_codex_rollout(root.path(), &first, "c:/work").unwrap(),
            expected.canonicalize().unwrap()
        );
        assert!(verify_codex_rollout(root.path(), &first, r"C:\elsewhere").is_err());
        assert!(
            verify_codex_rollout(root.path(), &Uuid::new_v4().to_string(), r"C:\work").is_err()
        );
    }

    #[test]
    fn codex_identity_rejects_children_unknown_metadata_and_duplicate_files() {
        let root = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4().to_string();
        let path = write_codex_fixture(root.path(), &id, r"C:\work");
        let original: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for (field, value) in [
            ("parent_thread_id", serde_json::json!(Uuid::new_v4())),
            ("thread_source", serde_json::json!("subagent")),
            ("thread_source", serde_json::Value::Null),
            ("source", serde_json::json!("vscode")),
            ("id", serde_json::json!(Uuid::new_v4())),
        ] {
            let mut header = original.clone();
            header["payload"][field] = value;
            fs::write(&path, format!("{header}\n")).unwrap();
            assert!(
                verify_codex_rollout(root.path(), &id, r"C:\work").is_err(),
                "accepted invalid {field}"
            );
        }
        fs::write(&path, format!("{original}\n")).unwrap();
        fs::copy(
            &path,
            path.with_file_name(format!("rollout-duplicate-{id}.jsonl")),
        )
        .unwrap();
        assert!(
            verify_codex_rollout(root.path(), &id, r"C:\work")
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
    }

    #[test]
    fn codex_identity_rejects_bad_ids_and_unbounded_or_truncated_headers() {
        let root = tempfile::tempdir().unwrap();
        write_codex_fixture(
            root.path(),
            "00000000-0000-0000-0000-000000000000",
            r"C:\work",
        );
        for id in ["../escape", "", "00000000-0000-0000-0000-000000000000"] {
            let error = verify_codex_rollout(root.path(), id, r"C:\work").unwrap_err();
            assert!(
                error.to_string().contains("Codex thread ID"),
                "wrong rejection: {error:#}"
            );
        }
        let id = Uuid::new_v4().to_string();
        let path = write_codex_fixture(root.path(), &id, r"C:\work");
        let mut header: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        // Codex 0.163 headers are about 55 KB; they keep growing.
        header["payload"]["base_instructions"] = serde_json::json!("x".repeat(200 * 1024));
        fs::write(&path, format!("{header}\n")).unwrap();
        assert!(verify_codex_rollout(root.path(), &id, r"C:\work").is_ok());
        header["payload"]["base_instructions"] = serde_json::json!("x".repeat(CODEX_HEADER_LIMIT as usize));
        fs::write(&path, format!("{header}\n")).unwrap();
        let error = verify_codex_rollout(root.path(), &id, r"C:\work").unwrap_err();
        assert!(error.is::<CodexRolloutHeaderTooLarge>(), "{error:#}");
        assert!(error.to_string().contains("exceeds the bounded metadata limit"));
        // Empty, partly written or not yet JSON: incomplete, not a mismatch.
        for partial in ["", "{\"type\":", "{\"type\":\"session_meta\",\"payload\":{\"id\":\"x"] {
            fs::write(&path, partial).unwrap();
            let error = verify_codex_rollout(root.path(), &id, r"C:\work").unwrap_err();
            assert!(error.is::<CodexRolloutIncomplete>(), "{partial:?}: {error:#}");
            assert!(error.to_string().contains("incomplete or invalid"));
        }
    }

    #[test]
    fn recovery_keeps_original_id_reason_and_provenance_across_reload() {
        let unknown = RecoveryReference::unresolved_codex(None, true);
        assert!(!unknown.reason.contains("original ID is retained"));
        assert!(!unknown.reason.contains("waiting"));
        let reference = RecoveryReference::resume_refused(
            Some("original-dead-id".into()),
            "conversation file is missing".into(),
        );
        let restored: RecoveryReference =
            serde_json::from_str(&serde_json::to_string(&reference).unwrap()).unwrap();
        assert_eq!(restored, reference);
        assert_eq!(restored.original_id.as_deref(), Some("original-dead-id"));
    }
}
