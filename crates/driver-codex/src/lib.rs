use std::path::Path;

use shared_types::{LaunchSpec, LaunchSpecError, PermissionProfile, SessionDefinition, WorkState};

const WORK_STATE_CONTEXT_MAX_BYTES: usize = 16 * 1024;

/// Tracks Codex's bounded interactive-prompt context across arbitrary PTY
/// output chunks. A detected blocking prompt stays latched until Codex emits
/// an explicit non-blocked state; silence, unrelated output, and context
/// eviction never clear it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkStateTracker {
    context: String,
    blocked: bool,
    blocked_detail: Option<String>,
    pending_classification: Option<(WorkState, Option<String>)>,
}

impl WorkStateTracker {
    pub fn begin_run(&mut self) {
        *self = Self::default();
    }

    pub fn observe_output(&mut self, chunk: &str) -> Option<(WorkState, Option<String>)> {
        self.context.push_str(chunk);
        let normalized_context = strip_ansi_and_controls(&self.context);

        if let Some(classification) = classify_normalized_work_state(&normalized_context) {
            self.context.clear();
            self.blocked = classification.0 == WorkState::Blocked;
            self.blocked_detail = classification.1.clone().filter(|_| self.blocked);
            self.pending_classification = Some(classification.clone());
            return Some(classification);
        }

        if self.blocked {
            trim_context_suffix(&mut self.context);
            return None;
        }

        trim_context_suffix(&mut self.context);
        None
    }

    pub fn take_pending_classification(&mut self) -> Option<(WorkState, Option<String>)> {
        self.pending_classification.take()
    }

    pub fn acknowledge_pending(&mut self) {
        self.pending_classification = None;
    }

    pub fn has_pending_classification(&self) -> bool {
        self.pending_classification.is_some()
    }

    pub fn blocked_detail(&self) -> Option<&str> {
        self.blocked_detail.as_deref()
    }

    pub fn is_blocked(&self) -> bool {
        self.blocked
    }
}

fn trim_context_suffix(context: &mut String) {
    if context.len() <= WORK_STATE_CONTEXT_MAX_BYTES {
        return;
    }
    let mut start = context.len() - WORK_STATE_CONTEXT_MAX_BYTES;
    while !context.is_char_boundary(start) {
        start += 1;
    }
    context.drain(..start);
}

pub fn classify_work_state(chunk: &str) -> Option<(WorkState, Option<String>)> {
    let normalized = strip_ansi_and_controls(chunk);
    classify_normalized_work_state(&normalized)
}

fn classify_normalized_work_state(normalized: &str) -> Option<(WorkState, Option<String>)> {
    let lower = normalized.to_ascii_lowercase();

    if lower.contains("do you trust the contents of this directory?")
        && lower.contains("yes, continue")
        && lower.contains("press enter to continue")
    {
        return Some((WorkState::Blocked, Some("workspace_trust".into())));
    }

    if (lower.contains("would you like to run the following command?")
        || lower.contains("do you want to allow codex to run this command?"))
        && lower.contains("yes")
        && lower.contains("no")
    {
        return Some((WorkState::Blocked, Some("approval_prompt".into())));
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

    if lower.contains("hit your usage") {
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

    if contains_working_timer(normalized) {
        return Some((WorkState::Thinking, extract_working_detail(normalized)));
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

pub fn launch_spec(
    definition: &SessionDefinition,
    program: &str,
    prefix_args: &[String],
) -> Result<LaunchSpec, LaunchSpecError> {
    validate_direct_program(program)?;

    let mut args = prefix_args.to_vec();
    match definition.permission_profile {
        PermissionProfile::Normal => {
            args.extend([
                "--ask-for-approval".into(),
                "on-request".into(),
                "--sandbox".into(),
                "workspace-write".into(),
            ]);
        }
        PermissionProfile::Unsafe => {
            args.push("--dangerously-bypass-approvals-and-sandbox".into());
        }
    }
    args.extend([
        "--no-alt-screen".into(),
        "-C".into(),
        definition.working_dir.clone(),
    ]);

    Ok(LaunchSpec {
        program: program.to_string(),
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

    const LIVE_WORKSPACE_TRUST_PROMPT: &str = concat!(
        "\u{1b}[?2026h\u{1b}[?2026l> \u{1b}[1mYou are in ",
        "C:\\workspace\u{1b}[K\r\n\u{1b}[K\r\n  ",
        "Do you trust the contents of this directory? Working with untrusted contents ",
        "comes with higher risk of prompt injection.\u{1b}[K\r\n",
        "  policies to load.\u{1b}[K\r\n\u{1b}[K\u{1b}[38;5;6m\r\n",
        "› 1. Yes, continue\u{1b}[K\u{1b}[m\r\n",
        "  2. No, quit\u{1b}[K\r\n\u{1b}[K\r\n  ",
        "\u{1b}[2mPress enter to continue\u{1b}[22m\u{1b}[K\r\n",
    );

    #[cfg(windows)]
    const WORKSPACE_ROOT: &str = r"D:\workspace & (qa)";
    #[cfg(windows)]
    const CODEX_EXECUTABLE: &str = r"C:\Program Files\OpenAI\codex.exe";
    #[cfg(windows)]
    const NODE_EXECUTABLE: &str = r"C:\Program Files\nodejs\node.exe";
    #[cfg(windows)]
    const CODEX_JS: &str =
        r"C:\Users\example\AppData\Roaming\npm\node_modules\@openai\codex\bin\codex.js";
    #[cfg(windows)]
    const SHELL_SHIM: &str = r"C:\Users\example\AppData\Roaming\npm\codex.cmd";

    #[cfg(not(windows))]
    const WORKSPACE_ROOT: &str = "/workspace & (qa)";
    #[cfg(not(windows))]
    const CODEX_EXECUTABLE: &str = "/opt/openai/bin/codex";
    #[cfg(not(windows))]
    const NODE_EXECUTABLE: &str = "/usr/bin/node";
    #[cfg(not(windows))]
    const CODEX_JS: &str = "/opt/openai/lib/codex.js";
    #[cfg(not(windows))]
    const SHELL_SHIM: &str = "/tmp/codex.cmd";

    fn definition(permission_profile: PermissionProfile) -> SessionDefinition {
        SessionDefinition {
            session_id: SessionId::nil(),
            alias: "session-00000000-0000-0000-0000-000000000000".into(),
            label: "Codex & calc.exe".into(),
            driver: DriverKind::Codex,
            working_dir: WORKSPACE_ROOT.into(),
            permission_profile,
        }
    }

    #[test]
    fn normal_native_launch_is_direct_and_omits_unsafe_mode() {
        let definition = definition(PermissionProfile::Normal);
        let spec = launch_spec(&definition, CODEX_EXECUTABLE, &[]).unwrap();

        assert_eq!(spec.program, CODEX_EXECUTABLE);
        assert_eq!(
            spec.args,
            vec![
                "--ask-for-approval".to_string(),
                "on-request".to_string(),
                "--sandbox".to_string(),
                "workspace-write".to_string(),
                "--no-alt-screen".to_string(),
                "-C".to_string(),
                WORKSPACE_ROOT.to_string(),
            ]
        );
        assert_eq!(spec.working_dir, WORKSPACE_ROOT);
        assert_eq!(spec.display_name, definition.label);
        assert!(spec.env.is_empty());
        assert!(
            !spec
                .args
                .iter()
                .any(|arg| arg == "--dangerously-bypass-approvals-and-sandbox")
        );
        assert!(!spec.args.iter().any(|arg| arg.contains("calc.exe")));
    }

    #[test]
    fn unsafe_node_launch_preserves_prefix_and_adds_exactly_the_bypass_flag() {
        let definition = definition(PermissionProfile::Unsafe);
        let prefix_args = vec![CODEX_JS.to_string()];
        let spec = launch_spec(&definition, NODE_EXECUTABLE, &prefix_args).unwrap();

        assert_eq!(spec.program, NODE_EXECUTABLE);
        assert_eq!(
            spec.args,
            vec![
                CODEX_JS.to_string(),
                "--dangerously-bypass-approvals-and-sandbox".to_string(),
                "--no-alt-screen".to_string(),
                "-C".to_string(),
                WORKSPACE_ROOT.to_string(),
            ]
        );
        assert_eq!(
            spec.args
                .iter()
                .filter(|arg| *arg == "--dangerously-bypass-approvals-and-sandbox")
                .count(),
            1
        );
        assert!(!spec.args.iter().any(|arg| arg == "--ask-for-approval"));
        assert!(!spec.args.iter().any(|arg| arg == "--sandbox"));
    }

    #[test]
    fn relative_and_shell_mediated_programs_are_rejected() {
        let definition = definition(PermissionProfile::Normal);
        assert!(matches!(
            launch_spec(&definition, "codex", &[]),
            Err(LaunchSpecError::ProgramNotQualified { .. })
        ));
        assert!(matches!(
            launch_spec(&definition, SHELL_SHIM, &[]),
            Err(LaunchSpecError::ShellMediatedProgram { .. })
        ));
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
            classify_work_state(
                "Do you trust the contents of this directory?\n› 1. Yes, continue\n  2. No, exit\nPress enter to continue"
            )
            .unwrap(),
            (WorkState::Blocked, Some("workspace_trust".into()))
        );
        assert_eq!(
            classify_work_state(
                "The documentation asks: Do you trust the contents of this directory?"
            ),
            None
        );
        assert_eq!(
            classify_work_state(
                "Would you like to run the following command?\n› 1. Yes, proceed (y)\n  2. Yes, and don't ask again\n  3. No, and tell Codex what to do differently (esc)"
            )
            .unwrap(),
            (WorkState::Blocked, Some("approval_prompt".into()))
        );
        assert_eq!(
            classify_work_state("Docs: Would you like to run the following command?"),
            None,
            "approval prose without live yes/no choices is not authoritative"
        );
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
        for available_usage in [
            "You have 1 usage limit reset available. Run /usage to use one.",
            "Heads up, you have less than 10% of your weekly limit left. Run /status for details.",
        ] {
            assert_eq!(
                classify_work_state(available_usage),
                None,
                "remaining capacity is not an exhausted usage limit: {available_usage}"
            );
        }
    }

    #[test]
    fn workspace_trust_tracker_detects_the_live_prompt_at_every_chunk_boundary() {
        for split in 0..=LIVE_WORKSPACE_TRUST_PROMPT.len() {
            if !LIVE_WORKSPACE_TRUST_PROMPT.is_char_boundary(split) {
                continue;
            }
            let mut tracker = WorkStateTracker::default();
            let first = tracker.observe_output(&LIVE_WORKSPACE_TRUST_PROMPT[..split]);
            let second = tracker.observe_output(&LIVE_WORKSPACE_TRUST_PROMPT[split..]);
            assert!(
                matches!(first, Some((WorkState::Blocked, Some(ref detail))) if detail == "workspace_trust")
                    || matches!(second, Some((WorkState::Blocked, Some(ref detail))) if detail == "workspace_trust"),
                "split {split} did not detect the live trust prompt"
            );
        }
    }

    #[test]
    fn workspace_trust_tracker_detects_one_character_chunks_and_rejects_prose() {
        let mut tracker = WorkStateTracker::default();
        let mut observed = None;
        for character in LIVE_WORKSPACE_TRUST_PROMPT.chars() {
            observed = tracker.observe_output(&character.to_string()).or(observed);
        }
        assert_eq!(
            observed,
            Some((WorkState::Blocked, Some("workspace_trust".into())))
        );

        let mut prose = WorkStateTracker::default();
        for character in
            "The documentation asks: Do you trust the contents of this directory?".chars()
        {
            assert_ne!(
                prose.observe_output(&character.to_string()),
                Some((WorkState::Blocked, Some("workspace_trust".into())))
            );
        }
        assert!(!prose.has_pending_classification());
    }

    #[test]
    fn workspace_trust_tracker_reassembles_control_sequences_split_inside_an_anchor() {
        let prompt = LIVE_WORKSPACE_TRUST_PROMPT.replace(
            "contents of this directory",
            "cont\u{1b}[31ments of this directory",
        );
        let mut tracker = WorkStateTracker::default();
        let mut observed = None;
        for character in prompt.chars() {
            observed = tracker.observe_output(&character.to_string()).or(observed);
        }
        assert_eq!(
            observed,
            Some((WorkState::Blocked, Some("workspace_trust".into())))
        );
    }

    #[test]
    fn blocked_prompt_is_latched_until_explicit_non_blocked_state_and_resets_per_run() {
        let mut tracker = WorkStateTracker::default();
        assert_eq!(
            tracker.observe_output(LIVE_WORKSPACE_TRUST_PROMPT),
            Some((WorkState::Blocked, Some("workspace_trust".into())))
        );
        assert_eq!(
            tracker.take_pending_classification(),
            Some((WorkState::Blocked, Some("workspace_trust".into())))
        );
        assert!(!tracker.has_pending_classification());
        assert_eq!(tracker.observe_output("unrelated cursor repaint"), None);
        assert_eq!(
            tracker.observe_output("Working 12s"),
            Some((WorkState::Thinking, Some("Working 12s".into())))
        );

        tracker.observe_output(LIVE_WORKSPACE_TRUST_PROMPT);
        tracker.acknowledge_pending();
        assert_eq!(
            tracker.observe_output("\u{258c}"),
            Some((WorkState::Idle, None))
        );

        tracker.observe_output(&LIVE_WORKSPACE_TRUST_PROMPT[..64]);
        tracker.begin_run();
        assert!(tracker.context.is_empty());
        assert!(!tracker.is_blocked());
        assert!(tracker.blocked_detail.is_none());
        assert!(!tracker.has_pending_classification());
    }

    #[test]
    fn work_state_tracker_context_is_bounded_without_manufacturing_a_state() {
        let mut tracker = WorkStateTracker::default();
        assert_eq!(
            tracker.observe_output(&"ordinary output ".repeat(2_048)),
            None
        );
        assert!(tracker.context.len() <= WORK_STATE_CONTEXT_MAX_BYTES);
        assert!(!tracker.has_pending_classification());
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
    fn terminal_text_cannot_claim_process_exit() {
        for text in [
            "\u{1b}[33mnpm warn cleanup Failed to remove some directories\u{1b}[0m\r\nPS C:\\Projects\\PRIM-1>",
            "[process exited with code 0]",
            "PS C:\\Projects\\PRIM-1>",
            "Codex CLI v1.2.3",
        ] {
            assert_eq!(classify_work_state(text), None, "{text:?}");
        }

        assert_eq!(
            classify_work_state("Codex CLI v1.2.3\n▌").unwrap().0,
            WorkState::Idle
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
