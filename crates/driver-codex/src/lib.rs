use shared_types::{DriverKind, LaunchSpec, SessionDefinition, WorkState};

pub fn classify_work_state(chunk: &str) -> Option<(WorkState, Option<String>)> {
    let normalized = strip_ansi_and_controls(chunk);
    let lower = normalized.to_ascii_lowercase();

    if let Some(detail) = terminal_signature(&normalized, "codex") {
        return Some((WorkState::Exited, Some(detail.into())));
    }

    if lower.contains("choose a plan")
        || lower.contains("create a plan")
        || lower.contains("press [tab]")
        || lower.contains("shift+tab")
        || lower.contains("plan mode")
    {
        return Some((WorkState::Blocked, Some("plan_mode_prompt".into())));
    }

    if lower.contains("access token could not be refreshed")
        || lower.contains("refresh token")
        || lower.contains("auth refresh")
        || lower.contains("please log out")
        || lower.contains("signed in to another account")
        || lower.contains("401")
        || lower.contains("403")
    {
        return Some((WorkState::Blocked, Some("auth_refresh".into())));
    }

    if lower.contains("usage limit") || lower.contains("hit your usage") {
        return Some((WorkState::Blocked, Some("usage_limit".into())));
    }
    if lower.contains("rate limit") || lower.contains("429") {
        return Some((WorkState::Blocked, Some("rate_limit".into())));
    }
    if lower.contains("stream disconnected")
        || lower.contains("retry your request")
        || lower.contains("network error")
        || lower.contains("timed out")
    {
        return Some((WorkState::Blocked, Some("stream_disconnected".into())));
    }

    if contains_working_timer(&normalized) {
        return Some((WorkState::Thinking, extract_working_detail(&normalized)));
    }

    if lower.contains("> run ")
        || lower.contains("\nrun ")
        || lower.contains("> read ")
        || lower.contains("\nread ")
        || lower.contains("tool call")
        || lower.contains("apply_patch")
    {
        return Some((WorkState::ToolCall, None));
    }

    if normalized.contains('▌') || lower.contains("esc to interrupt") {
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
    if lines.iter().any(|line| is_codex_launch_banner(line)) {
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

fn is_codex_launch_banner(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    (lower.starts_with("codex") || lower.contains("openai codex"))
        && (lower.contains("cli") || lower.contains("codex"))
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

fn contains_working_timer(chunk: &str) -> bool {
    let Some(index) = chunk.find("Working") else {
        return false;
    };
    let rest = &chunk[index + "Working".len()..];
    let mut saw_digit = false;
    let mut saw_unit = false;
    for ch in rest.chars().take(24) {
        if ch.is_ascii_digit() {
            saw_digit = true;
        }
        if saw_digit && (ch == 'm' || ch == 's') {
            saw_unit = true;
            break;
        }
        if !ch.is_ascii_digit() && !ch.is_ascii_whitespace() && ch != 'm' && ch != 's' {
            break;
        }
    }
    saw_digit && saw_unit
}

fn extract_working_detail(chunk: &str) -> Option<String> {
    let index = chunk.find("Working")?;
    let mut detail = String::new();
    for ch in chunk[index..].chars() {
        if ch == '\r' || ch == '\n' || ch == '·' || ch == '|' {
            break;
        }
        detail.push(ch);
        if detail.len() >= 32 {
            break;
        }
    }
    Some(detail.trim().to_string()).filter(|value| !value.is_empty())
}

pub fn default_session(working_dir: &str) -> SessionDefinition {
    SessionDefinition {
        name: "codex".into(),
        title: "Codex".into(),
        driver: DriverKind::Codex,
        working_dir: working_dir.into(),
        command: None,
        args: vec![],
        env: vec![],
        auto_start: false,
    }
}

pub fn launch_spec(definition: &SessionDefinition) -> LaunchSpec {
    let (program, mut args) = if cfg!(windows) {
        (
            "cmd.exe".to_string(),
            vec![
                "/d".into(),
                "/c".into(),
                "codex.cmd".into(),
                "--yolo".into(),
                "--no-alt-screen".into(),
                "-C".into(),
                definition.working_dir.clone(),
            ],
        )
    } else {
        (
            definition.command.clone().unwrap_or_else(|| "codex".into()),
            vec![
                "--yolo".into(),
                "--no-alt-screen".into(),
                "-C".into(),
                definition.working_dir.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_launch_spec_uses_cmd_shim_and_cwd_flag() {
        let definition = default_session(r"D:\workspace");
        let spec = launch_spec(&definition);

        assert_eq!(spec.program, "cmd.exe");
        assert_eq!(
            spec.args,
            vec![
                "/d".to_string(),
                "/c".to_string(),
                "codex.cmd".to_string(),
                "--yolo".to_string(),
                "--no-alt-screen".to_string(),
                "-C".to_string(),
                r"D:\workspace".to_string(),
            ]
        );
        assert_eq!(spec.working_dir, r"D:\workspace");
        assert_eq!(spec.display_name, "Codex");
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_launch_spec_uses_codex_binary_and_cwd_flag() {
        let definition = default_session("/workspace");
        let spec = launch_spec(&definition);

        assert_eq!(spec.program, "codex");
        assert_eq!(
            spec.args,
            vec![
                "--yolo".to_string(),
                "--no-alt-screen".to_string(),
                "-C".to_string(),
                "/workspace".to_string(),
            ]
        );
        assert_eq!(spec.working_dir, "/workspace");
        assert_eq!(spec.display_name, "Codex");
    }

    #[test]
    fn classify_codex_working_timer() {
        assert_eq!(
            classify_work_state("Working 12s").unwrap(),
            (WorkState::Thinking, Some("Working 12s".into()))
        );
        assert_eq!(
            classify_work_state("Working 3m 4s · esc to interrupt")
                .unwrap()
                .0,
            WorkState::Thinking
        );
    }

    #[test]
    fn classify_codex_blocked_patterns() {
        assert_eq!(
            classify_work_state("Create a plan? shift+tab use Plan mode").unwrap(),
            (WorkState::Blocked, Some("plan_mode_prompt".into()))
        );
        assert_eq!(
            classify_work_state(
                "Your access token could not be refreshed because your refresh token was revoked."
            )
            .unwrap(),
            (WorkState::Blocked, Some("auth_refresh".into()))
        );
        assert_eq!(
            classify_work_state("You've hit your usage limit.").unwrap(),
            (WorkState::Blocked, Some("usage_limit".into()))
        );
    }

    #[test]
    fn classify_codex_tool_and_idle_patterns() {
        assert_eq!(
            classify_work_state("> Run Get-Content package.json")
                .unwrap()
                .0,
            WorkState::ToolCall
        );
        assert_eq!(classify_work_state("▌").unwrap().0, WorkState::Idle);
    }

    #[test]
    fn classify_codex_terminal_signatures_as_exited() {
        assert_eq!(
            classify_work_state("\u{1b}[33mnpm warn cleanup Failed to remove some directories\u{1b}[0m\r\nPS C:\\Projects\\PRIM-1>")
                .unwrap(),
            (WorkState::Exited, Some("npm_cleanup".into()))
        );
        assert_eq!(
            classify_work_state("[process exited with code 0]").unwrap(),
            (WorkState::Exited, Some("process_exited".into()))
        );
        assert_eq!(
            classify_work_state("PS C:\\Projects\\PRIM-1>").unwrap(),
            (WorkState::Exited, Some("shell_prompt".into()))
        );
        assert_eq!(
            classify_work_state("Codex CLI v1.2.3").unwrap(),
            (WorkState::Exited, Some("launch_banner".into()))
        );
    }

    #[test]
    fn prompt_like_text_inside_prose_is_not_exited() {
        assert_eq!(
            classify_work_state("The log included this prompt:\nPS C:\\Projects\\PRIM-1>"),
            None
        );
        assert_eq!(
            classify_work_state("A bash prompt such as user@host:~/repo$ can appear in docs."),
            None
        );
        assert_eq!(
            classify_work_state("literal PowerShell example: `PS C:\\Projects\\PRIM-1>`"),
            None
        );
    }
}
