//! Georgi themes: how full each agent's context window is, read from the
//! harness's own conversation record (read only, bounded tail).
//!
//! - Codex writes `token_count` events carrying the exact window
//!   (`model_context_window`) and the last turn's usage.
//! - Claude Code writes `usage` on every assistant record but not the window,
//!   so the window is estimated from the model and the size already reached.

use serde::Serialize;
use serde_json::Value;
use shared_types::{DriverKind, SessionId};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Enough tail for the last usage record even after a large tool result.
const TAIL_BYTES: u64 = 512 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ContextUsage {
    pub session_id: SessionId,
    /// Tokens the last turn sent to the model: what the context holds now.
    pub used_tokens: u64,
    pub window_tokens: u64,
    /// False when the window size is an estimate (Claude).
    pub exact: bool,
    pub model: Option<String>,
}

/// The last usage record in `path`, for `driver`.
pub(crate) fn read_usage(session_id: SessionId, driver: DriverKind, path: &Path) -> Option<ContextUsage> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES))).ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    let lines = tail.split(|byte| *byte == b'\n').rev();
    match driver {
        DriverKind::Codex => lines
            .filter(|line| line.windows(11).any(|w| w == b"token_count"))
            .find_map(|line| codex_usage(session_id, serde_json::from_slice(line).ok()?)),
        DriverKind::Claude => lines
            .filter(|line| line.windows(7).any(|w| w == b"\"usage\""))
            .find_map(|line| claude_usage(session_id, serde_json::from_slice(line).ok()?)),
        _ => None,
    }
}

fn number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn codex_usage(session_id: SessionId, record: Value) -> Option<ContextUsage> {
    let info = record.get("payload")?.get("info")?;
    let last = info.get("last_token_usage")?;
    let window = info.get("model_context_window")?.as_u64()?;
    let used = number(last, "input_tokens") + number(last, "output_tokens");
    Some(ContextUsage { session_id, used_tokens: used, window_tokens: window, exact: true, model: None })
}

fn claude_usage(session_id: SessionId, record: Value) -> Option<ContextUsage> {
    if record.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let message = record.get("message")?;
    let usage = message.get("usage")?;
    let used = number(usage, "input_tokens")
        + number(usage, "cache_creation_input_tokens")
        + number(usage, "cache_read_input_tokens")
        + number(usage, "output_tokens");
    if used == 0 {
        return None;
    }
    let model = message.get("model").and_then(Value::as_str).map(str::to_string);
    Some(ContextUsage { session_id, used_tokens: used, window_tokens: claude_window(model.as_deref(), used), exact: false, model })
}

/// 1M for the long-context models (and any conversation already past 200k),
/// else the standard 200k.
fn claude_window(model: Option<&str>, used: u64) -> u64 {
    let model = model.unwrap_or_default().to_ascii_lowercase();
    let long = model.contains("[1m]") || model.contains("-1m") || ["claude-opus-5", "claude-sonnet-5", "claude-fable-5"]
        .iter()
        .any(|family| model.starts_with(family));
    if long || used > 200_000 { 1_000_000 } else { 200_000 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(lines: &[&str]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        file
    }

    #[test]
    fn reads_the_last_codex_token_count() {
        let file = write(&[
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":1000,"output_tokens":10},"model_context_window":258400}}}"#,
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":57291,"output_tokens":42},"model_context_window":258400}}}"#,
            r#"{"type":"response_item","payload":{}}"#,
        ]);
        let usage = read_usage(SessionId::nil(), DriverKind::Codex, file.path()).unwrap();
        assert_eq!((usage.used_tokens, usage.window_tokens, usage.exact), (57333, 258400, true));
    }

    #[test]
    fn reads_the_last_claude_assistant_usage() {
        let file = write(&[
            r#"{"type":"assistant","message":{"model":"claude-haiku-4","usage":{"input_tokens":5,"cache_read_input_tokens":90000,"cache_creation_input_tokens":1000,"output_tokens":200}}}"#,
            r#"{"type":"user","message":{"content":"hi"}}"#,
        ]);
        let usage = read_usage(SessionId::nil(), DriverKind::Claude, file.path()).unwrap();
        assert_eq!((usage.used_tokens, usage.window_tokens, usage.exact), (91205, 200_000, false));
        assert_eq!(claude_window(Some("claude-opus-5-5"), 1000), 1_000_000);
        assert_eq!(claude_window(Some("claude-haiku-4"), 250_000), 1_000_000);
    }
}
