use std::path::Path;

use shared_types::{DriverKind, LaunchSpec, SessionDefinition, WorkState};

pub fn classify_work_state(chunk: &str) -> Option<(WorkState, Option<String>)> {
    let normalized = strip_ansi_and_controls(chunk).replace('\u{2026}', "...");
    let lower = normalized.to_ascii_lowercase();

    if let Some(detail) = terminal_signature(&normalized, "claude") {
        return Some((WorkState::Exited, Some(detail.into())));
    }

    if lower.contains("stream disconnected")
        || lower.contains("retry your request")
        || lower.contains("network error")
        || lower.contains("timed out")
    {
        return Some((WorkState::Blocked, Some("stream_disconnected".into())));
    }

    if lower.contains("rate limit") || lower.contains("usage limit") {
        return Some((WorkState::ErrorLoop, Some("rate_limit".into())));
    }
    if lower.contains("api error") {
        return Some((WorkState::ErrorLoop, Some("api_error".into())));
    }
    if lower.contains("context length exceeded") || lower.contains("context_length_exceeded") {
        return Some((WorkState::ErrorLoop, Some("context_length_exceeded".into())));
    }

    if normalized.contains("Thinking...") || normalized.contains("Thinking ") {
        return Some((WorkState::Thinking, Some("Thinking...".into())));
    }

    if lower.contains("tool use")
        || lower.contains("tool_use")
        || lower.contains("<tool")
        || lower.contains("⏺")
        || lower.contains("●")
        || lower.contains("⎿")
    {
        return Some((WorkState::ToolCall, None));
    }

    if lower.contains("? for shortcuts") || lower.contains("esc to interrupt") {
        return Some((WorkState::Idle, None));
    }

    None
}

fn terminal_signature<'a>(chunk: &'a str, agent: &str) -> Option<&'a str> {
    let lines = nonempty_trimmed_lines(chunk);

    if lines
        .iter()
        .any(|line| is_command_not_found_line(line, agent))
    {
        return Some("command_not_found");
    }
    if lines.iter().any(|line| is_process_exited_line(line)) {
        return Some("process_exited");
    }
    if lines.iter().any(|line| is_npm_cleanup_line(line)) {
        return Some("npm_cleanup");
    }
    if lines.iter().any(|line| is_claude_launch_banner(line)) {
        return Some("launch_banner");
    }
    if is_terminal_prompt_chunk(&lines, agent) {
        return Some("shell_prompt");
    }

    None
}

fn strip_ansi_and_controls(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if matches!(chars.peek(), Some('[' | ']' | '(' | ')')) {
                let introducer = chars.next();
                for next in chars.by_ref() {
                    if introducer == Some(']') && next == '\u{7}' {
                        break;
                    }
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            continue;
        }

        if ch.is_control() && ch != '\n' && ch != '\r' && ch != '\t' {
            continue;
        }

        output.push(ch);
    }

    output
}

fn nonempty_trimmed_lines(chunk: &str) -> Vec<&str> {
    chunk
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect()
}

fn is_process_exited_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.starts_with("[process exited")
        || lower.starts_with("process exited")
        || lower.starts_with("process terminated")
}

fn is_npm_cleanup_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.starts_with("npm ")
        && (lower.contains("cleanup")
            || lower.contains("exit handler")
            || lower.contains("failed to remove"))
}

fn is_command_not_found_line(line: &str, agent: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.starts_with(&format!("{agent}: command not found"))
        || lower.contains(&format!("'{agent}' is not recognized"))
        || lower.contains(&format!("{agent}.cmd")) && lower.contains("not recognized")
        || lower.contains(&format!("the term '{agent}' is not recognized"))
}

fn is_claude_launch_banner(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.starts_with("claude") && (lower.contains("code") || lower.contains("cli"))
        || lower.contains("welcome to claude code")
}

fn is_terminal_prompt_chunk(lines: &[&str], agent: &str) -> bool {
    let Some(last) = lines.last() else {
        return false;
    };
    if !is_shell_prompt_line(last) {
        return false;
    }
    lines.len() == 1
        || lines[..lines.len() - 1]
            .iter()
            .all(|line| is_terminal_context_line(line, agent))
}

fn is_terminal_context_line(line: &str, agent: &str) -> bool {
    is_process_exited_line(line)
        || is_npm_cleanup_line(line)
        || is_command_not_found_line(line, agent)
}

fn is_shell_prompt_line(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() || line.len() > 180 || line.contains('`') {
        return false;
    }

    if line == "$" || line == "#" {
        return true;
    }
    if line.starts_with("PS ") && line.ends_with('>') {
        return line.contains(":\\") || line.contains(":/");
    }
    if is_cmd_prompt_line(line) {
        return true;
    }
    if (line.ends_with('$') || line.ends_with('#'))
        && (line.contains('@') || line.contains(':') || line.contains("~/") || line.contains('/'))
    {
        return true;
    }

    false
}

fn is_cmd_prompt_line(line: &str) -> bool {
    let bytes = line.as_bytes();
    bytes.len() >= 4
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
        && bytes[bytes.len() - 1] == b'>'
}

pub fn default_session(working_dir: &str) -> SessionDefinition {
    SessionDefinition {
        name: "claude".into(),
        title: "Claude".into(),
        driver: DriverKind::Claude,
        working_dir: working_dir.into(),
        command: None,
        args: vec![],
        env: vec![],
        auto_start: false,
    }
}

pub fn launch_spec(definition: &SessionDefinition) -> LaunchSpec {
    let wrapper_root = wrapper_root_for_session(&definition.working_dir);

    let (program, mut args) = if cfg!(windows) {
        // On Windows, portable_pty can't resolve .cmd shims directly.
        // Using cmd.exe /c claude (no extension) finds either claude.exe
        // (native installer) or claude.cmd (npm) via PATHEXT.
        (
            "cmd.exe".to_string(),
            vec![
                "/d".into(),
                "/c".into(),
                "claude".into(),
                "-n".into(),
                definition.name.clone(),
                "--dangerously-skip-permissions".into(),
                "--add-dir".into(),
                definition.working_dir.clone(),
                "--add-dir".into(),
                wrapper_root,
            ],
        )
    } else {
        (
            definition
                .command
                .clone()
                .unwrap_or_else(|| "claude".into()),
            vec![
                "-n".into(),
                definition.name.clone(),
                "--dangerously-skip-permissions".into(),
                "--add-dir".into(),
                definition.working_dir.clone(),
                "--add-dir".into(),
                wrapper_root,
            ],
        )
    };
    args.extend(definition.args.clone());

    LaunchSpec {
        program,
        args,
        working_dir: definition.working_dir.clone(),
        env: definition.env.clone(),
        display_name: definition.title.clone(),
    }
}

fn wrapper_root_for_session(working_dir: &str) -> String {
    // Explicit override via env var — canonical mechanism for pointing the
    // Claude pane's --add-dir at the wrapper's source tree, regardless of
    // where the pane's working_dir sits in the user's filesystem.
    if let Ok(root) = std::env::var("PRIM1_WRAPPER_ROOT") {
        let trimmed = root.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    // Fallback heuristic: assume the wrapper lives at <working_dir>/<name>.
    // <name> defaults to "PRIM-1" (the canonical repo name); can be
    // overridden via PRIM1_WRAPPER_DIRNAME for users who clone under a
    // different directory name. "CLI-master-wrapper" is recognized as a
    // wrapper root for backward compatibility with pre-rename setups.
    let wrapper_name = std::env::var("PRIM1_WRAPPER_DIRNAME")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "PRIM-1".to_string());

    let path = Path::new(working_dir);
    let is_wrapper_root = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| {
            name.eq_ignore_ascii_case(&wrapper_name)
                || name.eq_ignore_ascii_case("CLI-master-wrapper")
        })
        .unwrap_or(false);

    if is_wrapper_root {
        working_dir.to_string()
    } else {
        path.join(&wrapper_name).to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    const WORKSPACE_ROOT: &str = r"C:\Users\example\workspace";
    #[cfg(windows)]
    const WRAPPER_ROOT: &str = r"C:\Users\example\workspace\PRIM-1";

    #[cfg(not(windows))]
    const WORKSPACE_ROOT: &str = "/home/example/workspace";
    #[cfg(not(windows))]
    const WRAPPER_ROOT: &str = "/home/example/workspace/PRIM-1";

    #[cfg(windows)]
    #[test]
    fn windows_launch_spec_uses_cmd_shim() {
        let definition = default_session(WORKSPACE_ROOT);
        let spec = launch_spec(&definition);

        assert_eq!(spec.program, "cmd.exe");
        assert_eq!(
            spec.args,
            vec![
                "/d".to_string(),
                "/c".to_string(),
                "claude".to_string(),
                "-n".to_string(),
                "claude".to_string(),
                "--dangerously-skip-permissions".to_string(),
                "--add-dir".to_string(),
                WORKSPACE_ROOT.to_string(),
                "--add-dir".to_string(),
                WRAPPER_ROOT.to_string(),
            ]
        );
        assert_eq!(spec.working_dir, WORKSPACE_ROOT);
        assert_eq!(spec.display_name, "Claude");
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_launch_spec_uses_claude_binary() {
        let definition = default_session(WORKSPACE_ROOT);
        let spec = launch_spec(&definition);

        assert_eq!(spec.program, "claude");
        assert_eq!(
            spec.args,
            vec![
                "-n".to_string(),
                "claude".to_string(),
                "--dangerously-skip-permissions".to_string(),
                "--add-dir".to_string(),
                WORKSPACE_ROOT.to_string(),
                "--add-dir".to_string(),
                WRAPPER_ROOT.to_string(),
            ]
        );
        assert_eq!(spec.working_dir, WORKSPACE_ROOT);
        assert_eq!(spec.display_name, "Claude");
    }

    #[test]
    fn launch_spec_appends_session_definition_args_after_base_args() {
        let base = launch_spec(&default_session(WORKSPACE_ROOT));
        let mut definition = default_session(WORKSPACE_ROOT);
        definition.args = vec!["--resume".into(), "abc-123".into()];

        let spec = launch_spec(&definition);

        assert_eq!(&spec.args[..base.args.len()], base.args.as_slice());
        assert_eq!(
            &spec.args[base.args.len()..],
            &["--resume".to_string(), "abc-123".to_string()]
        );
    }

    #[test]
    fn wrapper_root_helper_keeps_existing_wrapper_path() {
        assert_eq!(wrapper_root_for_session(WRAPPER_ROOT), WRAPPER_ROOT);
    }

    #[test]
    fn classify_claude_thinking_markers() {
        assert_eq!(
            classify_work_state("Thinking…").unwrap().0,
            WorkState::Thinking
        );
        assert_eq!(
            classify_work_state("Thinking... next").unwrap().0,
            WorkState::Thinking
        );
    }

    #[test]
    fn classify_claude_error_and_blocked_patterns() {
        assert_eq!(
            classify_work_state("stream disconnected before completion").unwrap(),
            (WorkState::Blocked, Some("stream_disconnected".into()))
        );
        assert_eq!(
            classify_work_state("API Error: rate limit exceeded")
                .unwrap()
                .0,
            WorkState::ErrorLoop
        );
        assert_eq!(
            classify_work_state("context length exceeded").unwrap(),
            (WorkState::ErrorLoop, Some("context_length_exceeded".into()))
        );
    }

    #[test]
    fn classify_claude_tool_and_idle_patterns() {
        assert_eq!(
            classify_work_state("⏺ Read(file)").unwrap().0,
            WorkState::ToolCall
        );
        assert_eq!(
            classify_work_state("? for shortcuts").unwrap().0,
            WorkState::Idle
        );
    }

    #[test]
    fn classify_claude_terminal_signatures_as_exited() {
        assert_eq!(
            classify_work_state("\u{1b}[31mnpm warn cleanup Failed to remove some directories\u{1b}[0m\r\nC:\\Users\\me\\repo>")
                .unwrap(),
            (WorkState::Exited, Some("npm_cleanup".into()))
        );
        assert_eq!(
            classify_work_state("process exited with code 0").unwrap(),
            (WorkState::Exited, Some("process_exited".into()))
        );
        assert_eq!(
            classify_work_state("user@host:~/repo$").unwrap(),
            (WorkState::Exited, Some("shell_prompt".into()))
        );
        assert_eq!(
            classify_work_state("Claude Code v2.0.0").unwrap(),
            (WorkState::Exited, Some("launch_banner".into()))
        );
    }

    #[test]
    fn prompt_like_text_inside_prose_is_not_exited() {
        assert_eq!(
            classify_work_state("The transcript ended with:\nC:\\Users\\me\\repo>"),
            None
        );
        assert_eq!(
            classify_work_state("A log line can mention user@host:~/repo$ without being a prompt."),
            None
        );
        assert_eq!(
            classify_work_state("literal cmd example: `C:\\Users\\me\\repo>`"),
            None
        );
    }
}
