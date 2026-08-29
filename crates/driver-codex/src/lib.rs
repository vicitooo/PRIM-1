use std::path::Path;

use shared_types::{
    HarnessLaunchSession, LaunchSpec, LaunchSpecError, PermissionProfile, SessionDefinition,
    WorkState,
};
use terminal_viewport::{TerminalViewport, TrustedScreen};

const WORK_STATE_CONTEXT_MAX_BYTES: usize = 16 * 1024;

fn looks_like_status_working_directory(value: &str) -> bool {
    value.starts_with(['~', '/', '\\', '…'])
        || value
            .as_bytes()
            .get(..2)
            .is_some_and(|prefix| prefix[0].is_ascii_alphabetic() && prefix[1] == b':')
}

fn is_clean_prompt_screen(screen: &TrustedScreen<'_>) -> bool {
    if !screen.cursor_visible() || screen.cursor_col() != 2 {
        return false;
    }
    let Some(input) = screen.row_text(screen.cursor_row()) else {
        return false;
    };
    if input != "›" && !input.starts_with("› ") {
        return false;
    }

    let footer = (screen.cursor_row() + 1..screen.rows())
        .filter_map(|row| screen.row_text(row))
        .find(|row| !row.trim().is_empty());
    let Some(footer) = footer.filter(|footer| footer.starts_with("  ")) else {
        return false;
    };
    let Some((model, working_dir)) = footer.trim().rsplit_once(" · ") else {
        return false;
    };
    !model.trim().is_empty() && looks_like_status_working_directory(working_dir.trim())
}

fn classify_current_screen_blocker(
    screen: &TrustedScreen<'_>,
) -> Option<(WorkState, Option<String>)> {
    classify_normalized_work_state(&screen.text()).filter(|(state, _)| *state == WorkState::Blocked)
}

/// Tracks Codex's bounded interactive-prompt context across arbitrary PTY
/// output chunks. A detected blocking prompt stays latched until a trusted,
/// blocker-free clean prompt is observed; silence, activity text, unrelated
/// output, and context eviction never clear it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkStateTracker {
    context: String,
    viewport: TerminalViewport,
    clean_commit_pending: bool,
    blocked: bool,
    blocked_detail: Option<String>,
    pending_classification: Option<(WorkState, Option<String>)>,
}

impl WorkStateTracker {
    pub fn begin_run(&mut self) {
        self.context.clear();
        self.viewport.begin_run();
        self.clean_commit_pending = false;
        self.blocked = false;
        self.blocked_detail = None;
        self.pending_classification = None;
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.viewport.resize(cols, rows);
    }

    pub fn observe_output(&mut self, chunk: &str) -> Option<(WorkState, Option<String>)> {
        let mut observed = None;
        let mut blocker_resolved = !self.blocked;
        for character in chunk.chars() {
            self.context.push(character);
            if self.context.len() > WORK_STATE_CONTEXT_MAX_BYTES * 2 {
                trim_context_suffix(&mut self.context);
            }

            let signals = self.viewport.observe_character(character);
            if (signals.cursor_hidden || signals.projection_invalidated)
                && matches!(observed, Some((WorkState::Idle, _)))
            {
                observed = None;
            }
            if signals.cursor_hidden || signals.projection_invalidated {
                self.clean_commit_pending = false;
            }

            if signals.cursor_shown {
                if let Some(classification) = self.classify_context() {
                    self.clean_commit_pending = false;
                    if classification.0 == WorkState::Blocked {
                        blocker_resolved = false;
                        observed = Some(classification);
                    } else if blocker_resolved {
                        observed = Some(classification);
                    }
                }
                if let Some(screen) = self.viewport.trusted_screen() {
                    if let Some(blocker) = classify_current_screen_blocker(&screen) {
                        self.clean_commit_pending = false;
                        blocker_resolved = false;
                        observed = Some(blocker);
                    } else if is_clean_prompt_screen(&screen) {
                        self.clean_commit_pending = true;
                        blocker_resolved = true;
                        observed = Some((WorkState::Idle, None));
                    }
                }
                self.context.clear();
            }
        }

        if let Some(classification) = self.classify_context() {
            self.clean_commit_pending = false;
            if classification.0 == WorkState::Blocked {
                blocker_resolved = false;
                observed = Some(classification);
            } else if blocker_resolved {
                observed = Some(classification);
            }
        }

        if let Some(screen) = self.viewport.trusted_screen() {
            if let Some(blocker) = classify_current_screen_blocker(&screen) {
                self.clean_commit_pending = false;
                blocker_resolved = false;
                let repeated_in_this_chunk = observed.as_ref().is_some_and(|classification| {
                    classification.0 == WorkState::Blocked && classification.1 == blocker.1
                });
                observed = if repeated_in_this_chunk
                    || !self.blocked
                    || self.blocked_detail.as_deref() != blocker.1.as_deref()
                {
                    Some(blocker)
                } else {
                    None
                };
            } else if is_clean_prompt_screen(&screen) {
                if observed.is_none() && self.clean_commit_pending {
                    blocker_resolved = true;
                    observed = Some((WorkState::Idle, None));
                }
            } else {
                if matches!(observed, Some((WorkState::Idle, _))) {
                    observed = None;
                }
                self.clean_commit_pending = false;
            }
        } else if matches!(observed, Some((WorkState::Idle, _))) {
            observed = None;
        }

        trim_context_suffix(&mut self.context);
        if let Some(classification) = observed.filter(|classification| {
            classification.0 == WorkState::Blocked || !self.blocked || blocker_resolved
        }) {
            if classification.0 == WorkState::Idle {
                self.clean_commit_pending = false;
            }
            self.blocked = classification.0 == WorkState::Blocked;
            self.blocked_detail = classification.1.clone().filter(|_| self.blocked);
            self.pending_classification = Some(classification.clone());
            Some(classification)
        } else {
            None
        }
    }

    fn classify_context(&mut self) -> Option<(WorkState, Option<String>)> {
        let normalized = strip_ansi_and_controls(&self.context);
        let classification = classify_normalized_work_state(&normalized);
        if classification.is_some() {
            self.context.clear();
        }
        classification
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

    // Numeric status codes require their HTTP phrasing: a bare "401"/"403"
    // matches any digit run (token counts, offsets, sequence numbers — a
    // resumed replay carried 131 of them and latched a false auth block).
    if lower.contains("access token could not be refreshed")
        || lower.contains("refresh token")
        || lower.contains("auth refresh")
        || lower.contains("please log out")
        || lower.contains("signed in to another account")
        || lower.contains("401 unauthorized")
        || lower.contains("error 401")
        || lower.contains("http 401")
        || lower.contains("403 forbidden")
        || lower.contains("error 403")
        || lower.contains("http 403")
    {
        return Some((WorkState::Blocked, Some("auth_refresh".into())));
    }

    if lower.contains("hit your usage") {
        return Some((WorkState::Blocked, Some("usage_limit".into())));
    }
    if lower.contains("rate limit")
        || lower.contains("429 too many")
        || lower.contains("error 429")
        || lower.contains("http 429")
    {
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
    harness_session: &HarnessLaunchSession,
) -> Result<LaunchSpec, LaunchSpecError> {
    validate_direct_program(program)?;

    let mut args = prefix_args.to_vec();
    // Codex cannot pin an id at launch (it is captured from the rollout after
    // spawn); resuming is the `resume` subcommand, which accepts the same
    // safety, alt-screen and cwd flags as a fresh launch.
    if let HarnessLaunchSession::Resume { session_id } = harness_session {
        args.extend(["resume".into(), session_id.clone()]);
    }
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

    fn synchronized_frame(body: &str, cursor_column: usize) -> String {
        format!(
            "\u{1b}[?2026h\u{1b}[?25l\u{1b}[1;1H{body}\u{1b}[11;{cursor_column}H\u{1b}[?25h\u{1b}[?2026l"
        )
    }

    fn live_clean_prompt_frame(suggestion: &str) -> String {
        synchronized_frame(
            &format!(
                concat!(
                    "\r\n╭───────────────────────────────────────────────╮\r\n",
                    "│ >_ OpenAI Codex (v0.147.0)                    │\r\n",
                    "│                                               │\r\n",
                    "│ model:     gpt-5.6-sol max   /model to change │\r\n",
                    "│ directory: ~\\mywork                           │\r\n",
                    "╰───────────────────────────────────────────────╯\r\n",
                    "\r\n\r\n\r\n",
                    "› {}\r\n",
                    "\r\n",
                    "  gpt-5.6-sol max · ~\\mywork\r\n",
                    "\r\n\r\n\r\n"
                ),
                suggestion
            ),
            3,
        )
    }

    fn current_prompt_screen(extra: &str) -> String {
        format!(
            concat!(
                "\u{1b}[?25l\u{1b}[2J\u{1b}[H{}",
                "\u{1b}[5;1H› Summarize recent commits",
                "\u{1b}[7;1H  gpt-5.6-sol max · ~\\mywork",
                "\u{1b}[5;3H\u{1b}[?25h"
            ),
            extra
        )
    }

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
        let spec = launch_spec(&definition, CODEX_EXECUTABLE, &[], &HarnessLaunchSession::Fresh).unwrap();

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
        let spec = launch_spec(&definition, NODE_EXECUTABLE, &prefix_args, &HarnessLaunchSession::Fresh).unwrap();

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
            launch_spec(&definition, "codex", &[], &HarnessLaunchSession::Fresh),
            Err(LaunchSpecError::ProgramNotQualified { .. })
        ));
        assert!(matches!(
            launch_spec(&definition, SHELL_SHIM, &[], &HarnessLaunchSession::Fresh),
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
        assert_eq!(
            classify_work_state("HTTP 401 Unauthorized").unwrap(),
            (WorkState::Blocked, Some("auth_refresh".into()))
        );
        assert_eq!(
            classify_work_state("server said 429 Too Many Requests").unwrap(),
            (WorkState::Blocked, Some("rate_limit".into()))
        );
        // Digit runs in ordinary output are NOT status codes: the resumed
        // replay that latched a false auth block carried lines like these.
        assert_eq!(classify_work_state("sequence\": 4013,"), None);
        assert_eq!(classify_work_state("read 403 lines from index.html"), None);
        assert_eq!(classify_work_state("commit 8f403abc integrated (470742d)"), None);
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
    fn clean_prompt_tracker_detects_one_completed_frame_at_every_chunk_boundary() {
        let prompt = live_clean_prompt_frame("Find and fix a bug in @filename");
        for split in 0..=prompt.len() {
            if !prompt.is_char_boundary(split) {
                continue;
            }
            let mut tracker = WorkStateTracker::default();
            let first = tracker.observe_output(&prompt[..split]);
            let second = tracker.observe_output(&prompt[split..]);
            assert!(
                matches!(first, Some((WorkState::Idle, None)))
                    || matches!(second, Some((WorkState::Idle, None))),
                "split {split} did not detect the committed clean prompt"
            );
        }
    }

    #[test]
    fn clean_prompt_tracker_handles_one_character_chunks_and_variable_placeholder() {
        for suggestion in [
            "Find and fix a bug in @filename",
            "",
            "A completely different future suggestion",
        ] {
            let mut tracker = WorkStateTracker::default();
            let mut observed = None;
            for character in live_clean_prompt_frame(suggestion).chars() {
                observed = tracker.observe_output(&character.to_string()).or(observed);
            }
            assert_eq!(observed, Some((WorkState::Idle, None)), "{suggestion:?}");
        }
    }

    #[test]
    fn current_screen_tracker_accepts_the_exact_e16a_production_prefix() {
        // Exact prefix through the first post-MCP clean commit from the immutable
        // e16a raw stream (SHA-256 5B2FE2DD...E66FC). It deliberately contains
        // multiple rendering transactions and the obsolete interrupt hint.
        let stream: String =
            serde_json::from_str(include_str!("fixtures/codex-clean-prompt-e16a-prefix.json"))
                .expect("e16a production prefix should remain valid JSON");
        assert!(stream.contains("Summarize recent commits"));
        assert!(stream.contains("esc to interrupt"));

        let mut whole = WorkStateTracker::default();
        whole.resize(196, 22);
        assert_eq!(
            whole.observe_output(&stream),
            Some((WorkState::Idle, None)),
            "the measured multi-transaction screen must admit without a synthetic warm-up"
        );

        let mut one_character = WorkStateTracker::default();
        one_character.resize(196, 22);
        let mut observed = None;
        for character in stream.chars() {
            observed = one_character
                .observe_output(&character.to_string())
                .or(observed);
        }
        assert_eq!(observed, Some((WorkState::Idle, None)));
    }

    #[test]
    fn current_screen_blockers_and_clean_commits_preserve_stream_order() {
        let clean = current_prompt_screen("");
        let blocker = "You've hit your usage limit.";

        let mut persistent = WorkStateTracker::default();
        assert_eq!(
            persistent.observe_output(&current_prompt_screen(blocker)),
            Some((WorkState::Blocked, Some("usage_limit".into())))
        );
        persistent.acknowledge_pending();
        assert_eq!(persistent.observe_output("unrelated repaint"), None);
        assert!(persistent.is_blocked());
        assert_eq!(
            persistent.observe_output("\u{1b}[999zWorking 1s"),
            None,
            "activity text on an untrusted projection cannot clear a blocker"
        );
        assert!(persistent.is_blocked());

        let mut blocker_after_clean = WorkStateTracker::default();
        assert_eq!(
            blocker_after_clean.observe_output(&format!("{clean}{blocker}")),
            Some((WorkState::Blocked, Some("usage_limit".into()))),
            "a blocker observed after the clean commit must win"
        );

        let mut clean_after_blocker = WorkStateTracker::default();
        assert_eq!(
            clean_after_blocker.observe_output(&format!("{blocker}{clean}")),
            Some((WorkState::Idle, None)),
            "a later trusted repaint may clear an erased blocker"
        );

        let mut final_clean = WorkStateTracker::default();
        assert_eq!(
            final_clean.observe_output(&format!("{clean}{blocker}{clean}")),
            Some((WorkState::Idle, None)),
            "the final trusted current screen is authoritative"
        );
    }

    #[test]
    fn resize_fails_closed_until_reconstruction_and_run_reset_drops_old_authority() {
        let mut tracker = WorkStateTracker::default();
        tracker.resize(80, 20);
        assert_eq!(
            tracker.observe_output(concat!(
                "\u{1b}[?25l\u{1b}[5;1H› partial",
                "\u{1b}[7;1H  gpt-5.6-sol max · ~\\mywork",
                "\u{1b}[5;3H\u{1b}[?25h"
            )),
            None
        );
        assert_eq!(
            tracker.observe_output(&current_prompt_screen("")),
            Some((WorkState::Idle, None))
        );

        tracker.observe_output(LIVE_WORKSPACE_TRUST_PROMPT);
        assert!(tracker.is_blocked());
        tracker.begin_run();
        assert!(!tracker.is_blocked());
        assert!(!tracker.has_pending_classification());
        assert_eq!(tracker.viewport.dimensions(), Some((80, 20)));
        assert_eq!(tracker.observe_output("\u{1b}[?25h"), None);
    }

    #[test]
    fn current_screen_tracker_rejects_disjoint_prose_and_malformed_control_state() {
        let mut cross_frame = WorkStateTracker::default();
        assert_eq!(
            cross_frame.observe_output(&synchronized_frame("\r\n› stale input row\r\n", 3)),
            None
        );
        assert_eq!(
            cross_frame.observe_output(&synchronized_frame(
                "\r\n  gpt-5.6-sol max · ~\\mywork\r\n",
                3,
            )),
            None,
            "a footer-only later paint cannot establish a clean cursor/input screen"
        );

        let mut prose = WorkStateTracker::default();
        assert_eq!(
            prose.observe_output(concat!(
                "A transcript can mention › Find and fix a bug in @filename\n",
                "and gpt-5.6-sol max · ~\\mywork plus \u{1b}[?25h controls."
            )),
            None
        );
        assert_eq!(
            prose.observe_output(&synchronized_frame(
                concat!(
                    "\r\nHere is a prompt transcript:\r\n",
                    "› Find and fix a bug in @filename\r\n\r\n",
                    "  gpt-5.6-sol max · ~\\mywork\r\n",
                    "model output continues after the transcript\r\n"
                ),
                3,
            )),
            None
        );

        let mut wrong_cursor = WorkStateTracker::default();
        let wrong_cursor_frame =
            live_clean_prompt_frame("future suggestion").replace("\u{1b}[11;3H", "\u{1b}[11;4H");
        assert_eq!(wrong_cursor.observe_output(&wrong_cursor_frame), None);

        for terminal_string in [
            "\u{1b}]0;› spoof\r\n\r\n  gpt-5.6-sol max · ~\\mywork\u{7}",
            "\u{1b}P› spoof\r\n\r\n  gpt-5.6-sol max · ~\\mywork\u{1b}\\",
            "\u{009d}0;› spoof\r\n\r\n  gpt-5.6-sol max · ~\\mywork\u{009c}",
        ] {
            let mut terminal_string_spoof = WorkStateTracker::default();
            assert_eq!(
                terminal_string_spoof.observe_output(&synchronized_frame(terminal_string, 3)),
                None,
                "terminal-string content must not become prompt evidence"
            );
        }

        let mut next_frame_started = WorkStateTracker::default();
        let mut clean_then_partial_start = live_clean_prompt_frame("first");
        clean_then_partial_start.push_str("\u{1b}[?202");
        assert_eq!(
            next_frame_started.observe_output(&clean_then_partial_start),
            None,
            "a split start for a newer paint must suppress the older prompt frame"
        );
        assert_eq!(next_frame_started.observe_output("6hpartial redraw"), None);

        let clean_body = live_clean_prompt_frame("nested")
            .trim_start_matches("\u{1b}[?2026h")
            .trim_end_matches("\u{1b}[?2026l")
            .to_string();
        let mut nested = WorkStateTracker::default();
        assert_eq!(
            nested.observe_output(&format!(
                "\u{1b}[?2026h{clean_body}\u{1b}[?2026h{clean_body}\u{1b}[?2026l"
            )),
            Some((WorkState::Idle, None)),
            "synchronized-output mode is a rendering hint; current-screen state remains authoritative"
        );
    }

    #[test]
    fn clean_prompt_tracker_consumes_well_formed_terminal_strings() {
        for terminal_string in [
            "\u{1b}]0;Codex\u{7}",
            "\u{1b}]0;Codex\u{1b}\\",
            "\u{1b}Pignored\u{1b}\\",
            "\u{1b}Xignored\u{1b}\\",
            "\u{1b}^ignored\u{1b}\\",
            "\u{1b}_ignored\u{1b}\\",
            "\u{009d}0;Codex\u{009c}",
            "\u{0090}ignored\u{009c}",
            "\u{0098}ignored\u{009c}",
            "\u{009e}ignored\u{009c}",
            "\u{009f}ignored\u{009c}",
        ] {
            let prompt = live_clean_prompt_frame("future suggestion").replacen(
                "\u{1b}[?25l",
                &format!("\u{1b}[?25l{terminal_string}"),
                1,
            );
            let mut tracker = WorkStateTracker::default();
            assert_eq!(
                tracker.observe_output(&prompt),
                Some((WorkState::Idle, None)),
                "well-formed terminal string should not suppress a clean prompt: {terminal_string:?}"
            );
        }
    }

    #[test]
    fn terminal_string_cursor_controls_follow_xterm_global_transitions() {
        for hidden_cursor_lookalike in [
            "\u{1b}]0;[11;3H[?25h\u{7}",
            "\u{1b}P[11;3H[?25h\u{1b}\\",
            "\u{009d}0;[11;3H[?25h\u{009c}",
        ] {
            let wrong_visible_cursor = live_clean_prompt_frame("future suggestion")
                .replace("\u{1b}[11;3H", "\u{1b}[11;4H")
                .replacen(
                    "\u{1b}[?2026l",
                    &format!("{hidden_cursor_lookalike}\u{1b}[?2026l"),
                    1,
                );
            let mut tracker = WorkStateTracker::default();
            assert_eq!(
                tracker.observe_output(&wrong_visible_cursor),
                None,
                "printable cursor lookalikes in a terminal string must not authorize Idle"
            );
        }

        for real_cursor_proof_after_string_exit in [
            "\u{1b}]0;title\u{1b}[11;3H\u{1b}[?25h\u{7}",
            "\u{1b}Ppayload\u{1b}[11;3H\u{1b}[?25h\u{7}",
            "\u{009d}0;title\u{009b}11;3H\u{009b}?25h\u{009c}",
        ] {
            let prompt = live_clean_prompt_frame("future suggestion")
                .replace("\u{1b}[11;3H", "\u{1b}[11;4H")
                .replacen(
                    "\u{1b}[?2026l",
                    &format!("{real_cursor_proof_after_string_exit}\u{1b}[?2026l"),
                    1,
                );
            let mut tracker = WorkStateTracker::default();
            assert_eq!(
                tracker.observe_output(&prompt),
                Some((WorkState::Idle, None)),
                "ESC and C1 CSI globally exit terminal strings in the shipped xterm parser"
            );
        }

        for real_cursor_hide_after_string_exit in [
            "\u{1b}]0;title\u{1b}[?25l\u{7}",
            "\u{1b}Ppayload\u{1b}[?25l\u{7}",
            "\u{009d}0;title\u{009b}?25l\u{009c}",
        ] {
            let prompt = live_clean_prompt_frame("future suggestion").replacen(
                "\u{1b}[?2026l",
                &format!("{real_cursor_hide_after_string_exit}\u{1b}[?2026l"),
                1,
            );
            let mut tracker = WorkStateTracker::default();
            assert_eq!(
                tracker.observe_output(&prompt),
                None,
                "a real hide after globally exiting a string must revoke cursor proof"
            );
        }
    }

    #[test]
    fn clean_prompt_tracker_rejects_unterminated_terminal_strings() {
        for terminal_string in [
            "\u{1b}]0;unterminated",
            "\u{1b}Punterminated",
            "\u{009d}0;unterminated",
            "\u{0090}unterminated",
        ] {
            let prompt = format!(
                "{}{terminal_string}",
                live_clean_prompt_frame("future suggestion")
            );
            let mut tracker = WorkStateTracker::default();
            assert_eq!(
                tracker.observe_output(&prompt),
                None,
                "unterminated terminal string must fail closed: {terminal_string:?}"
            );
        }
    }

    #[test]
    fn clean_prompt_tracker_rejects_incomplete_and_unknown_repaints_then_recovers() {
        let mut tracker = WorkStateTracker::default();
        assert_eq!(
            tracker.observe_output("\u{1b}[?25l\u{1b}[H› partial"),
            None,
            "a hidden cursor is not a committed interactive screen"
        );
        let oversized_control = format!("\u{1b}[{}H", "1".repeat(129));
        assert_eq!(tracker.observe_output(&oversized_control), None);

        let recovered =
            live_clean_prompt_frame("recovered").replacen("\u{1b}[?25l", "\u{1b}[?25l\u{1b}[2J", 1);
        assert_eq!(
            tracker.observe_output(&recovered),
            Some((WorkState::Idle, None))
        );
    }

    #[test]
    fn trusted_clean_screen_clears_a_resolved_latched_block() {
        let mut tracker = WorkStateTracker::default();
        assert_eq!(
            tracker.observe_output(LIVE_WORKSPACE_TRUST_PROMPT),
            Some((WorkState::Blocked, Some("workspace_trust".into())))
        );
        tracker.acknowledge_pending();
        assert_eq!(
            tracker.observe_output(&live_clean_prompt_frame("background redraw")),
            Some((WorkState::Idle, None))
        );
        assert!(!tracker.is_blocked());
        assert_eq!(tracker.blocked_detail(), None);
    }

    #[test]
    fn visible_blocker_stays_latched_until_a_trusted_clean_screen_and_resets_per_run() {
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
            None,
            "raw activity text cannot override a blocker that remains on the current screen"
        );
        assert!(tracker.is_blocked());

        assert_eq!(
            tracker.observe_output(&live_clean_prompt_frame("resolved")),
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
    fn classify_codex_tool_patterns_without_legacy_idle_glyphs() {
        assert_eq!(
            classify_work_state("> Run Get-Content package.json")
                .unwrap()
                .0,
            WorkState::ToolCall
        );
        assert_eq!(classify_work_state("▌"), None);
        assert_eq!(classify_work_state("esc to interrupt"), None);
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

        assert_eq!(classify_work_state("Codex CLI v1.2.3\n▌"), None);
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
