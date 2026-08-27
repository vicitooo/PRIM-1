use std::path::Path;

use shared_types::{LaunchSpec, LaunchSpecError, PermissionProfile, SessionDefinition, WorkState};

pub fn classify_work_state(chunk: &str) -> Option<(WorkState, Option<String>)> {
    let normalized = strip_ansi_and_controls(chunk).replace('\u{2026}', "...");
    let lower = normalized.to_ascii_lowercase();

    if (lower.contains("do you trust the files in this folder?")
        || lower.contains("do you trust this folder?"))
        && lower.contains("yes, proceed")
        && lower.contains("no, exit")
    {
        return Some((WorkState::Blocked, Some("workspace_trust".into())));
    }

    if (lower.contains("do you want to proceed?") || lower.contains("would you like to proceed?"))
        && lower.contains("yes")
        && lower.contains("no")
        && (lower.contains("esc to cancel") || lower.contains("tab to amend"))
    {
        return Some((WorkState::Blocked, Some("approval_prompt".into())));
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
        || lower.contains("⎿")
    {
        return Some((WorkState::ToolCall, None));
    }

    if lower.contains("? for shortcuts") || lower.contains("esc to interrupt") {
        return Some((WorkState::Idle, None));
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

pub fn launch_spec(
    definition: &SessionDefinition,
    executable: &str,
) -> Result<LaunchSpec, LaunchSpecError> {
    validate_direct_program(executable)?;

    let mut args = vec!["-n".into(), definition.alias.clone()];
    match definition.permission_profile {
        PermissionProfile::Normal => {
            args.extend(["--permission-mode".into(), "manual".into()]);
        }
        PermissionProfile::Unsafe => {
            args.push("--dangerously-skip-permissions".into());
        }
    }

    Ok(LaunchSpec {
        program: executable.to_string(),
        args,
        working_dir: definition.working_dir.clone(),
        env: Vec::new(),
        display_name: definition.label.clone(),
    })
}

fn validate_direct_program(program: &str) -> Result<(), LaunchSpecError> {
    let path = Path::new(program);
    if program.trim().is_empty() || !path.is_absolute() {
        return Err(LaunchSpecError::ProgramNotQualified {
            program: program.to_string(),
        });
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(extension.as_str(), "cmd" | "bat" | "ps1")
        || matches!(
            file_name.as_str(),
            "cmd.exe" | "powershell.exe" | "pwsh.exe"
        )
    {
        return Err(LaunchSpecError::ShellMediatedProgram {
            program: program.to_string(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_types::{DriverKind, SessionId};

    #[cfg(windows)]
    const WORKSPACE_ROOT: &str = r"C:\Users\example\workspace & (qa)";
    #[cfg(windows)]
    const CLAUDE_EXECUTABLE: &str = r"C:\Users\example\.local\bin\claude.exe";
    #[cfg(windows)]
    const SHELL_SHIM: &str = r"C:\Windows\System32\cmd.exe";

    #[cfg(not(windows))]
    const WORKSPACE_ROOT: &str = "/home/example/workspace & (qa)";
    #[cfg(not(windows))]
    const CLAUDE_EXECUTABLE: &str = "/opt/claude/bin/claude";
    #[cfg(not(windows))]
    const SHELL_SHIM: &str = "/tmp/claude.cmd";

    fn definition(permission_profile: PermissionProfile) -> SessionDefinition {
        SessionDefinition {
            session_id: SessionId::nil(),
            alias: "session-00000000-0000-0000-0000-000000000000".into(),
            label: "Claude & calc.exe".into(),
            driver: DriverKind::Claude,
            working_dir: WORKSPACE_ROOT.into(),
            permission_profile,
        }
    }

    #[test]
    fn normal_launch_is_direct_and_omits_unsafe_and_wrapper_arguments() {
        let definition = definition(PermissionProfile::Normal);
        let spec = launch_spec(&definition, CLAUDE_EXECUTABLE).unwrap();

        assert_eq!(spec.program, CLAUDE_EXECUTABLE);
        assert_eq!(
            spec.args,
            vec![
                "-n".to_string(),
                definition.alias.clone(),
                "--permission-mode".to_string(),
                "manual".to_string(),
            ]
        );
        assert_eq!(spec.working_dir, WORKSPACE_ROOT);
        assert_eq!(spec.display_name, definition.label);
        assert!(spec.env.is_empty());
        assert!(
            !spec
                .args
                .iter()
                .any(|arg| arg == "--dangerously-skip-permissions")
        );
        assert!(!spec.args.iter().any(|arg| arg.contains("calc.exe")));
    }

    #[test]
    fn unsafe_launch_adds_exactly_the_claude_unsafe_flag() {
        let definition = definition(PermissionProfile::Unsafe);
        let spec = launch_spec(&definition, CLAUDE_EXECUTABLE).unwrap();

        assert_eq!(
            spec.args,
            vec![
                "-n".to_string(),
                definition.alias,
                "--dangerously-skip-permissions".to_string(),
            ]
        );
    }

    #[test]
    fn relative_and_shell_mediated_programs_are_rejected() {
        let definition = definition(PermissionProfile::Normal);
        assert!(matches!(
            launch_spec(&definition, "claude"),
            Err(LaunchSpecError::ProgramNotQualified { .. })
        ));
        assert!(matches!(
            launch_spec(&definition, SHELL_SHIM),
            Err(LaunchSpecError::ShellMediatedProgram { .. })
        ));
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
        assert_eq!(
            classify_work_state(
                "Do you trust the files in this folder?\n❯ 1. Yes, proceed\n  2. No, exit"
            )
            .unwrap(),
            (WorkState::Blocked, Some("workspace_trust".into()))
        );
        assert_eq!(
            classify_work_state(
                "Do you want to proceed?\n❯ 1. Yes\n  2. Yes, and don't ask again\n  3. No\nEsc to cancel · Tab to amend"
            )
            .unwrap(),
            (WorkState::Blocked, Some("approval_prompt".into()))
        );
        assert_eq!(
            classify_work_state("Documentation asks: do you want to proceed?"),
            None,
            "approval prose without the live modal controls is not authoritative"
        );
    }

    #[test]
    fn classify_claude_tool_and_idle_patterns() {
        assert_eq!(
            classify_work_state("⏺ Read(file)").unwrap().0,
            WorkState::ToolCall
        );
        assert_eq!(
            classify_work_state("● PRIM1_V9_CLAUDE_RAW_R9A1"),
            None,
            "Claude's normal assistant-response marker is not a tool call"
        );
        assert_eq!(
            classify_work_state("? for shortcuts").unwrap().0,
            WorkState::Idle
        );
    }

    #[test]
    fn terminal_text_cannot_claim_process_exit() {
        for text in [
            "\u{1b}[31mnpm warn cleanup Failed to remove some directories\u{1b}[0m\r\nC:\\Users\\me\\repo>",
            "process exited with code 0",
            "user@host:~/repo$",
            "Claude Code v2.0.0",
        ] {
            assert_eq!(classify_work_state(text), None, "{text:?}");
        }

        assert_eq!(
            classify_work_state("Claude Code v2.0.0\n? for shortcuts")
                .unwrap()
                .0,
            WorkState::Idle
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
