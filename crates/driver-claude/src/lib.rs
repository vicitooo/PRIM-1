use std::path::Path;

use shared_types::{
    HarnessLaunchSession, LaunchSpec, LaunchSpecError, PermissionProfile, SessionDefinition,
    WorkState,
};

/// Screens that hold Claude until a person answers them, each named by two
/// anchors from its own wording (Claude Code 2.1.303 binary strings), so a
/// quote of one phrase is not a block. Nothing answers them automatically:
/// terms, logins, keys and imports are the operator's decision.
const BLOCKING_SCREENS: [(&str, &str, &str); 9] = [
    (
        "consumer_terms_notice",
        "updates to consumer terms and policies",
        "accept terms",
    ),
    // Printed just before Claude exits below its minimum version.
    (
        "version_required",
        "needs an update",
        "is required to continue",
    ),
    (
        "onboarding",
        "choose the text style that looks best with your terminal",
        "dark mode",
    ),
    (
        "onboarding",
        "select login method:",
        "claude account with subscription",
    ),
    ("onboarding", "take the tour", "skip for now"),
    (
        "api_key_prompt",
        "detected a custom api key",
        "do you want to use this api key?",
    ),
    (
        "mcp_server_approval",
        "new mcp server found",
        "use this mcp server",
    ),
    (
        "mcp_server_approval",
        "new mcp servers found",
        "select any you wish to enable",
    ),
    (
        "external_imports_prompt",
        "allow external claude.md file imports?",
        "disable external imports",
    ),
];

/// Claude's minimum-version notice, its last output before it exits.
pub fn is_version_required_notice(text: &str) -> bool {
    let lower = strip_ansi_and_controls(text).to_ascii_lowercase();
    BLOCKING_SCREENS.iter().any(|(detail, title, choice)| {
        *detail == "version_required" && lower.contains(title) && lower.contains(choice)
    })
}

pub fn classify_work_state(chunk: &str) -> Option<(WorkState, Option<String>)> {
    let normalized = strip_ansi_and_controls(chunk).replace('\u{2026}', "...");
    let lower = normalized.to_ascii_lowercase();

    if let Some((detail, _, _)) = BLOCKING_SCREENS
        .iter()
        .find(|(_, title, choice)| lower.contains(title) && lower.contains(choice))
    {
        return Some((WorkState::Blocked, Some((*detail).into())));
    }

    if (lower.contains("do you trust the files in this folder?")
        || lower.contains("do you trust this folder?"))
        && lower.contains("yes, proceed")
        && lower.contains("no, exit")
    {
        return Some((WorkState::Blocked, Some("workspace_trust".into())));
    }

    // Newer trust and bypass warnings preselect "No, exit": a typed room
    // message plus Enter quits Claude (A106 walk, 19 Oct 2026).
    if lower.contains("no, exit")
        && (lower.contains("trust this folder") || lower.contains("quick safety check"))
    {
        return Some((WorkState::Blocked, Some("workspace_trust".into())));
    }
    if lower.contains("bypass permissions mode")
        && (lower.contains("yes, i accept") || lower.contains("no, exit"))
    {
        return Some((WorkState::Blocked, Some("bypass_permissions_warning".into())));
    }

    if (lower.contains("do you want to proceed?") || lower.contains("would you like to proceed?"))
        && lower.contains("yes")
        && lower.contains("no")
        && (lower.contains("esc to cancel") || lower.contains("tab to amend"))
    {
        return Some((WorkState::Blocked, Some("approval_prompt".into())));
    }

    // Connection, limit and API notices only as Claude Code paints its own
    // banners, at the start of a line or under its result gutter. Anywhere
    // else the words are prose, a tool's output or a pasted room message
    // (A16 T-34, 20 Oct: "no browser/network errors" in a typed review, and
    // PRIM-1's own "keeps hitting a rate limit" refusal, held members blocked
    // for hours; 2026-09-08: bare "timed out" did the same).
    if let Some(classified) = lower.lines().find_map(banner_line) {
        return Some(classified);
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

    // The bypass and accept-edits footers replace "? for shortcuts".
    if lower.contains("? for shortcuts") || lower.contains("esc to interrupt")
        || lower.contains("(shift+tab to cycle)") {
        return Some((WorkState::Idle, None));
    }

    None
}

/// One line of Claude Code's own error banners (2.1.304 strings: "API
/// Error: …", "Request timed out. Check your internet connection…",
/// "Connection error.", "Usage limit reached…", "Prompt is too long…"),
/// read only at the line's start, after its "⎿" result gutter.
fn banner_line(line: &str) -> Option<(WorkState, Option<String>)> {
    let line = line.trim_start();
    let line = line.strip_prefix('\u{23bf}').unwrap_or(line).trim_start();
    let starts = |prefixes: &[&str]| prefixes.iter().any(|prefix| line.starts_with(prefix));
    let blocked = |detail: &str| Some((WorkState::Blocked, Some(detail.into())));
    let looping = |detail: &str| Some((WorkState::ErrorLoop, Some(detail.into())));
    if starts(&["api error"]) {
        return if line.contains("request timed out") || line.contains("connection error") {
            blocked("stream_disconnected")
        } else if line.contains("rate limit") || line.contains("429") {
            looping("rate_limit")
        } else if line.contains("prompt is too long") {
            looping("context_length_exceeded")
        } else {
            looping("api_error")
        };
    }
    if starts(&["stream disconnected", "request timed out", "connection error", "unable to connect to api"]) {
        return blocked("stream_disconnected");
    }
    if starts(&["usage limit reached", "claude usage limit reached", "claude ai usage limit reached"]) {
        return looping("rate_limit");
    }
    if starts(&["prompt is too long", "context length exceeded"]) {
        return looping("context_length_exceeded");
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
    harness_session: &HarnessLaunchSession,
) -> Result<LaunchSpec, LaunchSpecError> {
    validate_direct_program(executable)?;

    let mut args = vec!["-n".into(), definition.alias.clone()];
    match harness_session {
        // `--session-id` pins a fresh conversation's id so a later launch can
        // `--resume` it; Claude keeps the id stable across plain resumes.
        HarnessLaunchSession::New { session_id } => {
            args.extend(["--session-id".into(), session_id.clone()]);
        }
        // Fused `=` form: the flag takes an optional value, and a separated
        // token must never be able to fall through as a prompt.
        HarnessLaunchSession::Resume { session_id } => {
            args.push(format!("--resume={session_id}"));
        }
        HarnessLaunchSession::Fresh => {}
    }
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
            codex_transport: Default::default(),
        }
    }

    #[test]
    fn normal_launch_is_direct_and_omits_unsafe_and_wrapper_arguments() {
        let definition = definition(PermissionProfile::Normal);
        let spec = launch_spec(&definition, CLAUDE_EXECUTABLE, &HarnessLaunchSession::Fresh).unwrap();

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
        let spec = launch_spec(&definition, CLAUDE_EXECUTABLE, &HarnessLaunchSession::Fresh).unwrap();

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
            launch_spec(&definition, "claude", &HarnessLaunchSession::Fresh),
            Err(LaunchSpecError::ProgramNotQualified { .. })
        ));
        assert!(matches!(
            launch_spec(&definition, SHELL_SHIM, &HarnessLaunchSession::Fresh),
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
            classify_work_state("API Error: Request timed out.").unwrap(),
            (WorkState::Blocked, Some("stream_disconnected".into()))
        );
        for prose in [
            "the final phone screenshot capture timed out.",
            "{\"message\":\"Wait timed out.\",\"timed_out\":true}",
        ] {
            assert_eq!(
                classify_work_state(prose),
                None,
                "bare \"timed out\" prose is not a connection banner: {prose:?}"
            );
        }
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
                "\x1b[1mWARNING: Claude Code running in Bypass Permissions mode\x1b[0m\n❯ 1. No, exit\n  2. Yes, I accept"
            )
            .unwrap(),
            (WorkState::Blocked, Some("bypass_permissions_warning".into()))
        );
        assert_eq!(
            classify_work_state(
                "Accessing workspace:\n\nD:\\work\n\nQuick safety check: Is this a project you created or one you trust?\n❯ No, exit\n  Yes, I trust this folder\n\nEnter to confirm · Esc to cancel"
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

    /// A16 T-34 (20 Oct): notices are Claude's own banners, never words in
    /// prose, tool output or a pasted room message, not even a quoted banner.
    #[test]
    fn notices_are_claudes_own_banners_never_prose_or_a_pasted_room_message() {
        for (banner, expected) in [
            ("  \u{23bf}  API Error: Connection error.", (WorkState::Blocked, "stream_disconnected")),
            ("Request timed out. Check your internet connection and proxy settings", (WorkState::Blocked, "stream_disconnected")),
            ("\u{23bf}  API Error: 429 rate limit", (WorkState::ErrorLoop, "rate_limit")),
            ("Usage limit reached \u{b7} resets 3pm", (WorkState::ErrorLoop, "rate_limit")),
            ("Prompt is too long", (WorkState::ErrorLoop, "context_length_exceeded")),
            ("\u{23bf}  API Error: overloaded", (WorkState::ErrorLoop, "api_error")),
        ] {
            assert_eq!(classify_work_state(banner), Some((expected.0, Some(expected.1.into()))), "{banner:?}");
        }
        for prose in [
            // A review report (invented): prose that mentions network errors is not a notice.
            "Bounded review PASS. I read the three receipts, reran the read-only checks, viewed all 3 screenshots and checked that the old port is unreachable. Counts 3/3/3, no browser/network errors.",
            // PRIM-1's own refusal, shown to another member as a tool result.
            "Claude A (0a1b2c3d) can't take a routed message right now: it keeps hitting a rate limit. The message was pasted but not submitted.",
            // A quoted banner inside a room message.
            "[Room message from operator]\n> it printed \"API Error: Request timed out.\" earlier",
            "\u{23fa} The usage limit reset at noon, and the network error handling is covered.",
            "please retry your request tomorrow, said the docs",
            "the context length exceeded nothing here",
        ] {
            // Not a notice. An assistant line's "⏺" still reads as a tool call.
            assert!(!matches!(classify_work_state(prose), Some((WorkState::Blocked | WorkState::ErrorLoop, _))),
                "{prose:?}: {:?}", classify_work_state(prose));
        }
    }

    /// Wording from Claude Code 2.1.303's own strings, laid out as its dialogs
    /// paint them (title, choices, key hint).
    #[test]
    fn classify_claude_blocking_startup_screens() {
        for (screen, detail) in [
            (
                "\x1b[1mUpdates to Consumer Terms and Policies\x1b[0m\r\nPlease select how you'd like to continue\r\nYour choice takes effect immediately upon confirmation.\r\n\u{276f} 1. Accept terms \u{b7} Help improve our AI models: ON\r\n  2. Accept terms \u{b7} Help improve our AI models: OFF\r\n  3. Not now\r\nEnter to confirm \u{b7} Esc to cancel",
                "consumer_terms_notice",
            ),
            (
                "It looks like your version of Claude Code (2.1.200) needs an update.\nA newer version (2.1.300 or higher) is required to continue.\n\nTo update, please run:\n    claude update",
                "version_required",
            ),
            (
                "Let's get started.\r\nChoose the text style that looks best with your terminal\r\n\u{276f} 1. Dark mode \u{2714}\r\n  2. Light mode\r\n  3. Dark mode (colorblind-friendly)",
                "onboarding",
            ),
            (
                "Select login method:\r\n\u{276f} 1. Claude account with subscription \u{b7} Pro, Max, Team, or Enterprise\r\n  2. Anthropic Console account \u{b7} API usage billing",
                "onboarding",
            ),
            ("Take the tour  Skip for now", "onboarding"),
            (
                "Detected a custom API key in your environment\r\nANTHROPIC_API_KEY: sk-ant-...abcd\r\nDo you want to use this API key?\r\n  1. Yes\r\n\u{276f} 2. No (recommended)",
                "api_key_prompt",
            ),
            (
                "New MCP server found in this project: fixture\r\n\u{276f} 1. Use this MCP server\r\n  2. Use this and all future MCP servers in this project\r\n  3. Continue without using this MCP server",
                "mcp_server_approval",
            ),
            (
                "2 new MCP servers found in this project\r\nSelect any you wish to enable.\r\n\u{276f} [x] one\r\n  [x] two",
                "mcp_server_approval",
            ),
            (
                "Allow external CLAUDE.md file imports?\r\nThis project's CLAUDE.md or .claude/rules imports files outside the current working directory.\r\n\u{276f} No, disable external imports\r\n  Yes, allow external imports",
                "external_imports_prompt",
            ),
        ] {
            assert_eq!(
                classify_work_state(screen),
                Some((WorkState::Blocked, Some(detail.into()))),
                "{screen:?}"
            );
        }
    }

    #[test]
    fn the_minimum_version_notice_is_recognised_in_an_exit_tail() {
        assert!(is_version_required_notice(
            "\x1b[33mIt looks like your version of Claude Code (2.1.200) needs an update.\x1b[0m\r\nA newer version (2.1.300 or higher) is required to continue.\r\n\r\nTo update, please run:\r\n    claude update\r\n"
        ));
        assert!(!is_version_required_notice(
            "This dependency needs an update before the release."
        ));
        assert!(!is_version_required_notice(
            "\u{2713} Update installed \u{b7} Restart to apply"
        ));
    }

    #[test]
    fn claude_update_footers_and_quoted_titles_are_not_blocking_screens() {
        // Claude's own update messages are footer lines, never a question.
        for footer in [
            "\u{2713} Update installed \u{b7} Restart to apply",
            "Update available! Run: claude update (auto-update failed)",
            "Auto-update failed \u{b7} Run claude doctor",
        ] {
            assert_eq!(classify_work_state(footer), None, "{footer:?}");
        }
        for quote in [
            "The terms dialog is titled Updates to Consumer Terms and Policies.",
            "This dependency needs an update before the release.",
            "Docs: Allow external CLAUDE.md file imports? appears once per project.",
            "Detected a custom API key in your environment is the dialog title.",
        ] {
            assert_eq!(classify_work_state(quote), None, "{quote:?}");
        }
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
        assert_eq!(
            classify_work_state("  \u{23f5}\u{23f5} bypass permissions on (shift+tab to cycle)").unwrap().0,
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
