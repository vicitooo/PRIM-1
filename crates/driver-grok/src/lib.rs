use std::path::Path;

use shared_types::{LaunchSpec, LaunchSpecError, PermissionProfile, SessionDefinition, WorkState};

pub fn launch_spec(
    definition: &SessionDefinition,
    executable: &str,
) -> Result<LaunchSpec, LaunchSpecError> {
    validate_direct_program(executable)?;

    let permission_mode = match definition.permission_profile {
        PermissionProfile::Normal => "default",
        PermissionProfile::Unsafe => "bypassPermissions",
    };
    let args = vec![
        "--permission-mode".into(),
        permission_mode.into(),
        "--cwd".into(),
        definition.working_dir.clone(),
    ];

    Ok(LaunchSpec {
        program: executable.to_string(),
        args,
        working_dir: definition.working_dir.clone(),
        env: Vec::new(),
        display_name: definition.label.clone(),
    })
}

pub fn classify_work_state(chunk: &str) -> Option<(WorkState, Option<String>)> {
    let normalized = strip_ansi_and_controls(chunk).replace('\u{2026}', "...");
    let lower = normalized.to_ascii_lowercase();

    if lower.contains("rate limit") || lower.contains("usage limit") {
        return Some((WorkState::ErrorLoop, Some("rate_limit".into())));
    }
    if lower.contains("stream disconnected")
        || lower.contains("network error")
        || lower.contains("retry your request")
        || lower.contains("timed out")
    {
        return Some((WorkState::Blocked, Some("stream_disconnected".into())));
    }
    if (lower.contains("sign in") || lower.contains("log in"))
        && lower.contains("grok")
        && (lower.contains("oauth") || lower.contains("authentication"))
    {
        return Some((WorkState::Blocked, Some("authentication".into())));
    }

    if normalized.contains("Thinking...") || normalized.contains("Responding...") {
        return Some((WorkState::Thinking, None));
    }
    if normalized.contains("◆ Run ") || normalized.contains("◆ Edit ") {
        return Some((WorkState::ToolCall, None));
    }
    if lower.contains("worked for ")
        || (lower.contains("grok build")
            && lower.contains("new worktree")
            && lower.contains("resume session"))
    {
        return Some((WorkState::Idle, None));
    }

    None
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

#[cfg(test)]
mod tests {
    use super::*;
    use shared_types::{DriverKind, SessionId};

    #[cfg(windows)]
    const WORKSPACE_ROOT: &str = r"C:\Users\example\workspace & (qa)";
    #[cfg(windows)]
    const GROK_EXECUTABLE: &str = r"C:\Users\example\.grok\bin\grok.exe";
    #[cfg(windows)]
    const SHELL_SHIM: &str = r"C:\Users\example\AppData\Roaming\grok.cmd";

    #[cfg(not(windows))]
    const WORKSPACE_ROOT: &str = "/home/example/workspace & (qa)";
    #[cfg(not(windows))]
    const GROK_EXECUTABLE: &str = "/opt/grok/bin/grok";
    #[cfg(not(windows))]
    const SHELL_SHIM: &str = "/tmp/grok.cmd";

    fn definition(permission_profile: PermissionProfile) -> SessionDefinition {
        SessionDefinition {
            session_id: SessionId::nil(),
            alias: "session-00000000-0000-0000-0000-000000000000".into(),
            label: "Grok & calc.exe".into(),
            driver: DriverKind::Grok,
            working_dir: WORKSPACE_ROOT.into(),
            permission_profile,
        }
    }

    #[test]
    fn normal_launch_is_direct_and_explicitly_uses_default_permissions() {
        let spec = launch_spec(&definition(PermissionProfile::Normal), GROK_EXECUTABLE).unwrap();
        assert_eq!(spec.program, GROK_EXECUTABLE);
        assert_eq!(
            spec.args,
            vec!["--permission-mode", "default", "--cwd", WORKSPACE_ROOT,]
        );
        assert_eq!(spec.working_dir, WORKSPACE_ROOT);
        assert!(spec.env.is_empty());
        assert!(
            !spec
                .args
                .iter()
                .any(|argument| argument.contains("calc.exe"))
        );
    }

    #[test]
    fn unsafe_launch_uses_the_measured_grok_bypass_mode() {
        let spec = launch_spec(&definition(PermissionProfile::Unsafe), GROK_EXECUTABLE).unwrap();
        assert_eq!(spec.args[0..2], ["--permission-mode", "bypassPermissions"]);
    }

    #[test]
    fn relative_and_shell_mediated_programs_are_rejected() {
        let definition = definition(PermissionProfile::Normal);
        assert!(matches!(
            launch_spec(&definition, "grok"),
            Err(LaunchSpecError::ProgramNotQualified { .. })
        ));
        assert!(matches!(
            launch_spec(&definition, SHELL_SHIM),
            Err(LaunchSpecError::ShellMediatedProgram { .. })
        ));
    }

    #[test]
    fn classifies_measured_grok_tui_markers() {
        assert_eq!(
            classify_work_state("◆ Thinking…").unwrap().0,
            WorkState::Thinking
        );
        assert_eq!(
            classify_work_state("❙  ◆ Run Print current working directory")
                .unwrap()
                .0,
            WorkState::ToolCall
        );
        assert_eq!(
            classify_work_state("Worked for 6.3s").unwrap().0,
            WorkState::Idle
        );
        assert_eq!(
            classify_work_state("Grok Build 1.0.0\nNew worktree\nResume session")
                .unwrap()
                .0,
            WorkState::Idle
        );
    }
}
