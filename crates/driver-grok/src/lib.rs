use std::path::Path;

use shared_types::{LaunchSpec, LaunchSpecError, PermissionProfile, SessionDefinition, WorkState};
use uuid::Uuid;

// Measured against Grok Build 1.0.0 (3cd0d0cbce). Unknown future text stays
// fail-closed in lifecycle Starting rather than being inferred ready.
pub const LAUNCHER_MENU_DETAIL: &str = "launcher_menu";
pub const SESSION_STARTING_DETAIL: &str = "session_starting";
pub const TELEMETRY_CONSENT_DETAIL: &str = "telemetry_consent";

pub fn launch_spec(
    definition: &SessionDefinition,
    executable: &str,
) -> Result<LaunchSpec, LaunchSpecError> {
    launch_spec_with_session_id(definition, executable, Uuid::new_v4())
}

fn launch_spec_with_session_id(
    definition: &SessionDefinition,
    executable: &str,
    session_id: Uuid,
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
        "--session-id".into(),
        session_id.to_string(),
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
    if lower.contains("help improve grok")
        && (lower.contains("opt in") || lower.contains("opt out"))
    {
        return Some((WorkState::Blocked, Some(TELEMETRY_CONSENT_DETAIL.into())));
    }
    if lower.contains("grok build")
        && lower.contains("new worktree")
        && lower.contains("resume session")
    {
        return Some((WorkState::Blocked, Some(LAUNCHER_MENU_DETAIL.into())));
    }
    if lower.contains("starting session...") {
        return Some((WorkState::Blocked, Some(SESSION_STARTING_DETAIL.into())));
    }

    if normalized.contains("Thinking...") || normalized.contains("Responding...") {
        return Some((WorkState::Thinking, None));
    }
    if normalized.contains("◆ Run ") || normalized.contains("◆ Edit ") {
        return Some((WorkState::ToolCall, None));
    }
    if lower.contains("worked for ") {
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
        let session_id = Uuid::parse_str("8fe042e1-9007-43cc-80bc-ee3d53301ee2").unwrap();
        let spec = launch_spec_with_session_id(
            &definition(PermissionProfile::Normal),
            GROK_EXECUTABLE,
            session_id,
        )
        .unwrap();
        assert_eq!(spec.program, GROK_EXECUTABLE);
        assert_eq!(
            spec.args,
            vec![
                "--permission-mode",
                "default",
                "--cwd",
                WORKSPACE_ROOT,
                "--session-id",
                "8fe042e1-9007-43cc-80bc-ee3d53301ee2",
            ]
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
        let spec = launch_spec_with_session_id(
            &definition(PermissionProfile::Unsafe),
            GROK_EXECUTABLE,
            Uuid::nil(),
        )
        .unwrap();
        assert_eq!(spec.args[0..2], ["--permission-mode", "bypassPermissions"]);
    }

    #[test]
    fn each_launch_gets_one_fresh_grok_session_id_without_a_bootstrap_prompt() {
        let definition = definition(PermissionProfile::Normal);
        let first = launch_spec(&definition, GROK_EXECUTABLE).unwrap();
        let second = launch_spec(&definition, GROK_EXECUTABLE).unwrap();

        let session_id = |spec: &LaunchSpec| {
            let positions = spec
                .args
                .iter()
                .enumerate()
                .filter_map(|(index, value)| (value == "--session-id").then_some(index))
                .collect::<Vec<_>>();
            assert_eq!(positions.len(), 1);
            assert_eq!(positions[0] + 2, spec.args.len());
            Uuid::parse_str(&spec.args[positions[0] + 1]).unwrap()
        };

        assert_ne!(session_id(&first), session_id(&second));
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
            classify_work_state("Grok Build 1.0.0\nNew worktree\nResume session"),
            Some((WorkState::Blocked, Some(LAUNCHER_MENU_DETAIL.into())))
        );
        assert_eq!(
            classify_work_state("Starting session… 0.0s"),
            Some((WorkState::Blocked, Some(SESSION_STARTING_DETAIL.into())))
        );
        assert_eq!(
            classify_work_state("Help improve Grok [Opt out] [Opt in]"),
            Some((WorkState::Blocked, Some(TELEMETRY_CONSENT_DETAIL.into())))
        );
        assert_eq!(
            classify_work_state("Responding… 0.4s 16K / 500K")
                .unwrap()
                .0,
            WorkState::Thinking
        );
        assert_eq!(classify_work_state("16K / 500K"), None);
    }
}
