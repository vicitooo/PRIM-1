//! v3 W3: a typed room wake is Written when its bytes and CR reach the
//! member's terminal; whether the harness submitted it is a separate fact.
//! It is confirmed only from the member's own bound conversation record — the
//! Claude transcript or the Codex rollout of the run's identified thread — as
//! a user record whose text equals the exact typed pointer, timestamped at or
//! after the attempt began; or by the recipient reading or acknowledging the
//! message. A record that cannot be found, read or bound is Unknown, never
//! "no record" (Supervisor 1086baa2; Reviewers be7a79e7, e02495a5).
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use shared_types::DriverKind;

/// How much of the record's end is read at most.
const SUBMIT_RECORD_TAIL_MAX: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SubmitEvidence {
    /// The exact pointer is a user record in the member's own conversation,
    /// at this locator (`<file name>@<byte offset>`).
    Confirmed(String),
    /// The bound record was read and holds no such record since the attempt.
    NotFound,
    /// The record is missing, unbound, unreadable or not readable far enough.
    Unknown(&'static str),
}

/// Where this member's conversation is recorded, or why that is unknown.
pub(super) fn record_path(driver: DriverKind, claude_home: Option<&Path>, harness_id: Option<&str>,
    identity_unresolved: bool, working_dir: &str) -> Result<PathBuf, &'static str> {
    let id = harness_id.filter(|id| !identity_unresolved && !id.is_empty()).ok_or("the member's conversation is not bound")?;
    match driver {
        DriverKind::Claude => {
            let home = claude_home.ok_or("the Claude data folder is unknown")?;
            crate::identity_tracker::claude_transcript_path(home, working_dir, id)
                .ok_or("the Claude transcript was not found")
        }
        DriverKind::Codex => {
            let root = crate::session::codex_sessions_root().map_err(|_| "the Codex data folder is unknown")?;
            crate::session::verify_codex_rollout(&root, id, working_dir).map_err(|_| "the Codex rollout is not verified")
        }
        _ => Err("this harness has no conversation record PRIM-1 reads"),
    }
}

/// Whether a user record equal to `payload`, timestamped at or after `since`,
/// is in the record. Only the bounded tail is read; when the tail starts after
/// `since` could have begun and holds no match, that is Unknown.
pub(super) fn submitted_since(driver: DriverKind, path: &Path, since: DateTime<Utc>, matches: &dyn Fn(&str) -> bool) -> SubmitEvidence {
    let Ok(mut file) = fs::File::open(path) else { return SubmitEvidence::Unknown("the conversation record is unreadable") };
    let Ok(len) = file.metadata().map(|metadata| metadata.len()) else {
        return SubmitEvidence::Unknown("the conversation record is unreadable");
    };
    let start = len.saturating_sub(SUBMIT_RECORD_TAIL_MAX);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return SubmitEvidence::Unknown("the conversation record is unreadable");
    }
    let mut tail = Vec::new();
    if file.take(len - start).read_to_end(&mut tail).is_err() || (tail.len() as u64) < len - start {
        return SubmitEvidence::Unknown("the conversation record could not be read in full");
    }
    let mut oldest_seen: Option<DateTime<Utc>> = None;
    let mut damaged = false;
    let file_name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    // A cut first line is skipped; a malformed or unfinished record makes a
    // missing match Unknown, never negative (Reviewer 8b8f774b).
    let mut offset = start;
    for (index, line) in tail.split(|byte| *byte == b'\n').enumerate() {
        let line_offset = offset;
        offset += line.len() as u64 + 1;
        if (index == 0 && start > 0) || line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<serde_json::Value>(line) else { damaged = true; continue };
        let Some(at) = record["timestamp"].as_str().and_then(|text| DateTime::parse_from_rfc3339(text).ok())
            .map(|at| at.with_timezone(&Utc)) else { continue };
        oldest_seen = Some(oldest_seen.map_or(at, |oldest| oldest.min(at)));
        if at >= since && user_texts(driver, &record).iter().any(|text| matches(text.trim())) {
            return SubmitEvidence::Confirmed(format!("{file_name}@{line_offset}"));
        }
    }
    if damaged || !tail.ends_with(b"\n") && !tail.is_empty() {
        return SubmitEvidence::Unknown("the conversation record has a malformed or unfinished entry");
    }
    if start > 0 && oldest_seen.is_none_or(|oldest| oldest > since) {
        return SubmitEvidence::Unknown("the conversation record's bounded tail does not reach the attempt");
    }
    SubmitEvidence::NotFound
}

/// The user-typed texts one record carries: a Claude `user` record's message
/// content, a Codex `user_message` event or user `message` item.
fn user_texts(driver: DriverKind, record: &serde_json::Value) -> Vec<String> {
    let texts_of = |content: &serde_json::Value| -> Vec<String> {
        match content {
            serde_json::Value::String(text) => vec![text.clone()],
            serde_json::Value::Array(parts) => parts.iter()
                .filter(|part| matches!(part["type"].as_str(), Some("text" | "input_text")))
                .filter_map(|part| part["text"].as_str().map(str::to_owned)).collect(),
            _ => Vec::new(),
        }
    };
    match driver {
        // A genuine prompt: not a meta record and not a tool result, which
        // Claude also records with the user role (Reviewer c49038b0).
        DriverKind::Claude if record["type"].as_str() == Some("user") && record["isMeta"].as_bool() != Some(true)
            && record.get("toolUseResult").is_none() => texts_of(&record["message"]["content"]),
        DriverKind::Codex => {
            let payload = &record["payload"];
            match (record["type"].as_str(), payload["type"].as_str()) {
                (Some("event_msg"), Some("user_message")) => payload["message"].as_str().map(str::to_owned).into_iter().collect(),
                (Some("response_item"), Some("message")) if payload["role"].as_str() == Some("user") => texts_of(&payload["content"]),
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

/// v3 J4: the provider's verdict on a Codex member's latest turn of this run,
/// from its own rollout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ProviderTurn {
    /// The latest turn ended with a structured provider error (its code only).
    Error(String),
    /// A later turn started, or the latest turn completed without an error.
    Clear,
    /// No turn of this run is in the readable record (or it is unreadable).
    Unknown,
}

/// The latest turn of this run, from records timestamped at or after `since`.
/// The captured shape (Codex 0.164) is an `event_msg` whose payload is
/// `task_complete` with `error.codex_error_info` as a string; an object form
/// names its code by its single key. The error's message is never read out.
pub(super) fn latest_provider_turn(path: &Path, since: DateTime<Utc>) -> ProviderTurn {
    scan_rollout_turns(path, since).0
}

/// v3 W2 (Claude PRIM-1, eaa0739 walk): the provider verdict and whether the
/// latest turn of this run is still open (a `task_started` with no later
/// `task_complete` or `turn_aborted`); None when no turn of this run is read.
/// A background exec cell keeps a turn open while its screen is quiet.
pub(super) fn scan_rollout_turns(path: &Path, since: DateTime<Utc>) -> (ProviderTurn, Option<bool>) {
    let Ok(mut file) = fs::File::open(path) else { return (ProviderTurn::Unknown, None) };
    let Ok(len) = file.metadata().map(|metadata| metadata.len()) else { return (ProviderTurn::Unknown, None) };
    let start = len.saturating_sub(SUBMIT_RECORD_TAIL_MAX);
    let mut tail = Vec::new();
    if file.seek(SeekFrom::Start(start)).is_err() || file.take(len - start).read_to_end(&mut tail).is_err() {
        return (ProviderTurn::Unknown, None);
    }
    // v3 W2 (Reviewer 42c82e54): a short read leaves later evidence unread,
    // so nothing read here is current; Unknown changes nothing.
    if tail.len() as u64 != len - start { return (ProviderTurn::Unknown, None); }
    let mut verdict = ProviderTurn::Unknown;
    let mut open = None;
    // A line that does not parse (a record still being written, or a damaged
    // one) after the last verdict read may hold later evidence: the result is
    // then Unknown, never an earlier Clear presented as current. A later
    // verdict supersedes it.
    let mut unread_after = false;
    for line in tail.split(|byte| *byte == b'\n').skip(usize::from(start > 0)) {
        if line.iter().all(u8::is_ascii_whitespace) { continue; }
        let Ok(record) = serde_json::from_slice::<serde_json::Value>(line) else { unread_after = true; continue };
        if record["type"] != "event_msg" { continue; }
        let Some(at) = record["timestamp"].as_str().and_then(|text| DateTime::parse_from_rfc3339(text).ok())
            .map(|at| at.with_timezone(&Utc)) else { continue };
        if at < since { continue; }
        let payload = &record["payload"];
        if matches!(payload["type"].as_str(), Some("task_started" | "task_complete" | "turn_aborted")) {
            unread_after = false;
        }
        match payload["type"].as_str() {
            Some("task_started") => { verdict = ProviderTurn::Clear; open = Some(true); }
            Some("task_complete") => {
                open = Some(false);
                verdict = match provider_error_code(&payload["error"]) {
                    Some(code) => ProviderTurn::Error(code),
                    None if payload["error"].is_null() => ProviderTurn::Clear,
                    // An error this build cannot name still holds, by its class.
                    None => ProviderTurn::Error("unknown".into()),
                };
            }
            Some("turn_aborted") => open = Some(false),
            _ => {}
        }
    }
    if unread_after { return (ProviderTurn::Unknown, None); }
    (verdict, open)
}

/// The code of a structured provider error: a short snake_case word only.
fn provider_error_code(error: &serde_json::Value) -> Option<String> {
    let info = &error["codex_error_info"];
    let code = info.as_str().map(str::to_owned)
        .or_else(|| info.as_object().filter(|map| map.len() == 1).and_then(|map| map.keys().next().cloned()))?;
    (!code.is_empty() && code.len() <= 64 && code.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'_'))
        .then_some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    const POINTER: &str = "[PRIM-1] 1 new room message for you in room r (newest m). Run room_read with that room_id, then room_ack each.";

    fn write(path: &Path, lines: &[serde_json::Value]) {
        fs::write(path, lines.iter().map(|line| format!("{line}\n")).collect::<String>()).unwrap();
    }

    #[test]
    fn a_claude_user_record_with_the_exact_pointer_after_the_attempt_confirms_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let since = DateTime::parse_from_rfc3339("2026-10-24T17:41:00Z").unwrap().with_timezone(&Utc);
        let earlier = serde_json::json!({"type": "user", "timestamp": "2026-10-24T17:40:00Z", "message": {"content": POINTER}});
        let quoted = serde_json::json!({"type": "assistant", "timestamp": "2026-10-24T17:41:05Z",
            "message": {"content": [{"type": "text", "text": POINTER}]}});
        write(&path, &[earlier.clone(), quoted.clone()]);
        assert_eq!(submitted_since(DriverKind::Claude, &path, since, &|text| text == POINTER), SubmitEvidence::NotFound,
            "an earlier identical record, or the pointer quoted by the assistant, is not this attempt");
        let user = serde_json::json!({"type": "user", "timestamp": "2026-10-24T17:41:02Z",
            "message": {"content": [{"type": "text", "text": POINTER}]}});
        write(&path, &[earlier, quoted, user]);
        assert!(matches!(submitted_since(DriverKind::Claude, &path, since, &|text| text == POINTER), SubmitEvidence::Confirmed(_)));
    }

    #[test]
    fn a_codex_user_message_confirms_and_a_missing_or_unbound_record_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let since = DateTime::parse_from_rfc3339("2026-10-24T17:41:00Z").unwrap().with_timezone(&Utc);
        write(&path, &[serde_json::json!({"type": "session_meta", "timestamp": "2026-10-24T17:00:00Z"}),
            serde_json::json!({"type": "event_msg", "timestamp": "2026-10-24T17:41:03Z",
                "payload": {"type": "user_message", "message": POINTER}})]);
        assert!(matches!(submitted_since(DriverKind::Codex, &path, since, &|text| text == POINTER), SubmitEvidence::Confirmed(_)));
        assert!(matches!(submitted_since(DriverKind::Codex, &dir.path().join("gone.jsonl"), since, &|text| text == POINTER), SubmitEvidence::Unknown(_)));
        assert!(record_path(DriverKind::Codex, None, Some("t"), true, "D:\\x").is_err(), "an unresolved identity is unbound");
        assert!(record_path(DriverKind::Claude, None, None, false, "D:\\x").is_err(), "no conversation id is unbound");
    }

    #[test]
    fn a_malformed_or_unfinished_record_is_unknown_and_tool_results_never_confirm() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let since = DateTime::parse_from_rfc3339("2026-10-24T17:41:00Z").unwrap().with_timezone(&Utc);
        let tool = serde_json::json!({"type": "user", "timestamp": "2026-10-24T17:41:02Z", "toolUseResult": {},
            "message": {"content": [{"type": "text", "text": POINTER}]}});
        write(&path, &[tool]);
        assert_eq!(submitted_since(DriverKind::Claude, &path, since, &|text| text == POINTER), SubmitEvidence::NotFound,
            "a tool result echoing the pointer is not a prompt");
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str("{\"type\": \"user\", \"timesta");
        fs::write(&path, &text).unwrap();
        assert!(matches!(submitted_since(DriverKind::Claude, &path, since, &|text| text == POINTER), SubmitEvidence::Unknown(_)),
            "an unfinished entry is Unknown, never negative");
        fs::write(&path, "not json\n").unwrap();
        assert!(matches!(submitted_since(DriverKind::Claude, &path, since, &|text| text == POINTER), SubmitEvidence::Unknown(_)));
    }

    /// v3 J4: captured shape (Codex 0.164 rollouts on MAINPC, structure only,
    /// content removed): `task_complete` with `error.codex_error_info` as a string.
    fn turn(at: &str, kind: &str, error: Option<serde_json::Value>) -> serde_json::Value {
        let mut payload = serde_json::json!({ "type": kind, "turn_id": "t", "root_turn_id": "t",
            "started_at": 1, "completed_at": 2, "duration_ms": 1, "last_agent_message": null });
        if let Some(error) = error { payload["error"] = error; }
        serde_json::json!({ "timestamp": at, "type": "event_msg", "payload": payload })
    }

    #[test]
    fn the_latest_errored_turn_of_this_run_names_its_provider_code_and_a_later_turn_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let since: DateTime<Utc> = "2026-10-24T21:00:00Z".parse().unwrap();
        let error = |code: &str| Some(serde_json::json!({ "codex_error_info": code, "message": "removed" }));
        for code in ["unauthorized", "cyber_policy", "server_overloaded", "usage_limit_exceeded"] {
            write(&path, &[turn("2026-10-24T21:37:00Z", "task_started", None),
                turn("2026-10-24T21:37:32Z", "task_complete", error(code))]);
            assert_eq!(latest_provider_turn(&path, since), ProviderTurn::Error(code.into()), "{code}");
        }
        write(&path, &[turn("2026-10-24T21:37:32Z", "task_complete", error("unauthorized")),
            turn("2026-10-24T21:40:00Z", "task_started", None)]);
        assert_eq!(latest_provider_turn(&path, since), ProviderTurn::Clear, "a later turn start clears");
        write(&path, &[turn("2026-10-24T21:37:32Z", "task_complete", error("unauthorized")),
            turn("2026-10-24T21:41:00Z", "task_complete", None)]);
        assert_eq!(latest_provider_turn(&path, since), ProviderTurn::Clear, "an error-free completion clears");
        write(&path, &[turn("2026-10-24T20:00:00Z", "task_complete", error("unauthorized"))]);
        assert_eq!(latest_provider_turn(&path, since), ProviderTurn::Unknown, "an earlier run's error never holds this run");
        write(&path, &[turn("2026-10-24T21:37:32Z", "task_complete",
            Some(serde_json::json!({ "codex_error_info": { "http_connection_failed": { "http_status_code": 502 } } })))]);
        assert_eq!(latest_provider_turn(&path, since), ProviderTurn::Error("http_connection_failed".into()), "object form");
        write(&path, &[turn("2026-10-24T21:37:32Z", "task_complete", Some(serde_json::json!({ "message": "removed" })))]);
        assert_eq!(latest_provider_turn(&path, since), ProviderTurn::Error("unknown".into()), "an unnamed error still holds");
        assert_eq!(latest_provider_turn(&dir.path().join("missing.jsonl"), since), ProviderTurn::Unknown);
    }

    /// Reviewer 42c82e54: an unparsed line after the last verdict read (a
    /// record still being written, or a damaged one) makes the scan Unknown,
    /// never the earlier Clear or open state; a later verdict supersedes it.
    #[test]
    fn an_unparsed_line_after_the_last_verdict_makes_the_scan_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let since: DateTime<Utc> = "2026-10-24T21:00:00Z".parse().unwrap();
        let clean = format!("{}\n", turn("2026-10-24T21:37:32Z", "task_complete", None));
        let started = format!("{}\n", turn("2026-10-24T21:40:00Z", "task_started", None));
        fs::write(&path, format!("{clean}{{\"timestamp\":\"2026-10-24T21:41:00Z\",\"type\":\"event_msg\",\"payl")).unwrap();
        assert_eq!(scan_rollout_turns(&path, since), (ProviderTurn::Unknown, None), "a truncated tail record");
        fs::write(&path, format!("{clean}not json\n")).unwrap();
        assert_eq!(scan_rollout_turns(&path, since), (ProviderTurn::Unknown, None), "a damaged later line");
        fs::write(&path, format!("not json\n{clean}")).unwrap();
        assert_eq!(scan_rollout_turns(&path, since), (ProviderTurn::Clear, Some(false)), "a later verdict supersedes it");
        fs::write(&path, format!("{clean}{started}\n")).unwrap();
        assert_eq!(scan_rollout_turns(&path, since), (ProviderTurn::Clear, Some(true)), "a blank line is not evidence");
    }

    /// v3 W2: the captured exec-cell shape (Codex 0.164, eaa0739 walk, structure
    /// only): a turn started, tool calls and a queued user message follow, no
    /// completion yet, so the turn is open whatever the screen shows.
    #[test]
    fn a_started_turn_without_a_completion_is_open_and_a_completion_or_abort_closes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let since: DateTime<Utc> = "2026-10-24T22:00:00Z".parse().unwrap();
        let item = |at: &str, kind: &str| serde_json::json!({ "timestamp": at, "type": "response_item",
            "payload": { "type": kind, "name": "exec" } });
        write(&path, &[turn("2026-10-24T22:22:13Z", "task_started", None), item("2026-10-24T22:22:17Z", "custom_tool_call"),
            item("2026-10-24T22:22:29Z", "custom_tool_call"), item("2026-10-24T22:23:46Z", "function_call")]);
        assert_eq!(scan_rollout_turns(&path, since).1, Some(true), "an exec cell keeps the turn open");
        write(&path, &[turn("2026-10-24T22:22:13Z", "task_started", None), turn("2026-10-24T22:25:00Z", "task_complete", None)]);
        assert_eq!(scan_rollout_turns(&path, since), (ProviderTurn::Clear, Some(false)));
        write(&path, &[turn("2026-10-24T22:22:13Z", "task_started", None), turn("2026-10-24T22:23:00Z", "turn_aborted", None)]);
        assert_eq!(scan_rollout_turns(&path, since).1, Some(false), "an aborted turn is closed");
        write(&path, &[turn("2026-10-24T21:00:00Z", "task_started", None)]);
        assert_eq!(scan_rollout_turns(&path, since).1, None, "an earlier run's turn says nothing about this run");
    }
}
