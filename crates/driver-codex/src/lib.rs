use shared_types::{DriverKind, LaunchSpec, SessionDefinition, WorkState};

pub fn classify_work_state(chunk: &str) -> Option<(WorkState, Option<String>)> {
    let lower = chunk.to_ascii_lowercase();

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

    if contains_working_timer(chunk) {
        return Some((WorkState::Thinking, extract_working_detail(chunk)));
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

    if chunk.contains('▌') || lower.contains("esc to interrupt") {
        return Some((WorkState::Idle, None));
    }

    None
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
}
