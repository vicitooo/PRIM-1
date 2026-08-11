use std::path::Path;

use shared_types::{LaunchSpec, LaunchSpecError, PermissionProfile, SessionDefinition, WorkState};
use uuid::Uuid;

// Measured against Grok Build 1.0.0 (3cd0d0cbce). Unknown future text stays
// fail-closed in lifecycle Starting rather than being inferred ready.
pub const LAUNCHER_MENU_DETAIL: &str = "launcher_menu";
pub const SESSION_STARTING_DETAIL: &str = "session_starting";
pub const STARTUP_REPAINT_MAX_BYTES: usize = 32 * 1024;

const TERMINAL_CONTROL_MAX_CHARS: u16 = 128;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StartupProgress {
    #[default]
    None,
    StartingObserved,
    InteractiveReady,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum StartupPhase {
    #[default]
    AwaitingStarting,
    StartingObserved,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalStringKind {
    Osc,
    Dcs,
    Sos,
    Pm,
    Apc,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum StartupParserState {
    #[default]
    Ground,
    Escape,
    Csi {
        chars_seen: u16,
    },
    CsiDiscard,
    String {
        kind: TerminalStringKind,
        chars_seen: u16,
    },
    StringDiscard {
        kind: TerminalStringKind,
    },
}

/// Tracks only the bounded repaint structure needed to admit Grok's first
/// interactive screen. It is deliberately not a terminal emulator.
#[derive(Debug, Default)]
pub struct StartupTracker {
    phase: StartupPhase,
    parser: StartupParserState,
    csi_parameters: String,
    frame_active: bool,
    frame_invalid: bool,
    frame_saw_home: bool,
    frame_text: String,
}

impl StartupTracker {
    pub fn begin_run(&mut self) {
        *self = Self::default();
    }

    pub fn observe_output(&mut self, chunk: &str) -> StartupProgress {
        if self.phase == StartupPhase::Complete {
            return StartupProgress::None;
        }

        let mut progress = StartupProgress::None;
        for character in chunk.chars() {
            let observed = self.observe_character(character);
            if observed == StartupProgress::InteractiveReady {
                return observed;
            }
            if observed == StartupProgress::StartingObserved {
                progress = observed;
            }
        }
        progress
    }

    fn observe_character(&mut self, character: char) -> StartupProgress {
        if self.observe_global_transition(character) {
            return StartupProgress::None;
        }

        match self.parser {
            StartupParserState::Ground => {
                if character == '\u{001b}' {
                    self.parser = StartupParserState::Escape;
                } else {
                    self.push_visible(character);
                }
                StartupProgress::None
            }
            StartupParserState::Escape => {
                self.observe_escape(character);
                StartupProgress::None
            }
            StartupParserState::Csi { mut chars_seen } => {
                chars_seen = chars_seen.saturating_add(1);
                if chars_seen > TERMINAL_CONTROL_MAX_CHARS {
                    self.invalidate_frame();
                    self.csi_parameters.clear();
                    self.parser = StartupParserState::CsiDiscard;
                    self.observe_csi_discard(character);
                    return StartupProgress::None;
                }
                if Self::is_c0_executable_or_del(character) {
                    self.parser = StartupParserState::Csi { chars_seen };
                    return StartupProgress::None;
                }
                if character == '\u{001b}' {
                    self.csi_parameters.clear();
                    self.parser = StartupParserState::Escape;
                    return StartupProgress::None;
                }
                if ('\u{40}'..='\u{7e}').contains(&character) {
                    let parameters = std::mem::take(&mut self.csi_parameters);
                    self.parser = StartupParserState::Ground;
                    return self.finish_csi(&parameters, character);
                }
                if character.is_ascii() {
                    self.csi_parameters.push(character);
                    self.parser = StartupParserState::Csi { chars_seen };
                } else {
                    self.invalidate_frame();
                    self.csi_parameters.clear();
                    self.parser = StartupParserState::Ground;
                }
                StartupProgress::None
            }
            StartupParserState::CsiDiscard => {
                self.observe_csi_discard(character);
                StartupProgress::None
            }
            StartupParserState::String { kind, chars_seen } => {
                self.observe_string(character, kind, Some(chars_seen));
                StartupProgress::None
            }
            StartupParserState::StringDiscard { kind } => {
                self.observe_string(character, kind, None);
                StartupProgress::None
            }
        }
    }

    fn observe_global_transition(&mut self, character: char) -> bool {
        self.parser = match character {
            '\u{0018}'
            | '\u{001a}'
            | '\u{0080}'..='\u{008f}'
            | '\u{0091}'..='\u{0097}'
            | '\u{0099}'..='\u{009a}'
            | '\u{009c}' => StartupParserState::Ground,
            '\u{009b}' => {
                self.csi_parameters.clear();
                StartupParserState::Csi { chars_seen: 0 }
            }
            '\u{009d}' => Self::terminal_string(TerminalStringKind::Osc),
            '\u{0090}' => Self::terminal_string(TerminalStringKind::Dcs),
            '\u{0098}' => Self::terminal_string(TerminalStringKind::Sos),
            '\u{009e}' => Self::terminal_string(TerminalStringKind::Pm),
            '\u{009f}' => Self::terminal_string(TerminalStringKind::Apc),
            _ => return false,
        };
        true
    }

    fn observe_escape(&mut self, character: char) {
        if Self::is_c0_executable_or_del(character) {
            return;
        }
        self.parser = match character {
            '[' => {
                self.csi_parameters.clear();
                StartupParserState::Csi { chars_seen: 0 }
            }
            ']' => Self::terminal_string(TerminalStringKind::Osc),
            'P' => Self::terminal_string(TerminalStringKind::Dcs),
            'X' => Self::terminal_string(TerminalStringKind::Sos),
            '^' => Self::terminal_string(TerminalStringKind::Pm),
            '_' => Self::terminal_string(TerminalStringKind::Apc),
            '\u{001b}' => StartupParserState::Escape,
            _ => StartupParserState::Ground,
        };
    }

    fn observe_csi_discard(&mut self, character: char) {
        if Self::is_c0_executable_or_del(character) {
            return;
        }
        self.parser = match character {
            '\u{001b}' => StartupParserState::Escape,
            '\u{40}'..='\u{7e}' => StartupParserState::Ground,
            _ => StartupParserState::CsiDiscard,
        };
    }

    fn observe_string(
        &mut self,
        character: char,
        kind: TerminalStringKind,
        chars_seen: Option<u16>,
    ) {
        match character {
            '\u{0007}' if kind == TerminalStringKind::Osc => {
                self.parser = StartupParserState::Ground;
            }
            '\u{001b}' => self.parser = StartupParserState::Escape,
            _ => match chars_seen {
                Some(chars_seen) => {
                    let chars_seen = chars_seen.saturating_add(1);
                    if chars_seen > TERMINAL_CONTROL_MAX_CHARS {
                        self.invalidate_frame();
                        self.parser = StartupParserState::StringDiscard { kind };
                    } else {
                        self.parser = StartupParserState::String { kind, chars_seen };
                    }
                }
                None => self.parser = StartupParserState::StringDiscard { kind },
            },
        }
    }

    fn finish_csi(&mut self, parameters: &str, final_character: char) -> StartupProgress {
        if matches!(final_character, 'H' | 'f') && Self::is_home_position(parameters) {
            if self.frame_active {
                self.frame_saw_home = true;
                self.frame_invalid = false;
                self.frame_text.clear();
            }
            return StartupProgress::None;
        }

        if Self::private_mode_contains(parameters, 25) {
            match final_character {
                'l' => self.begin_frame(),
                'h' if self.frame_active => return self.finish_frame(),
                _ => {}
            }
        }

        StartupProgress::None
    }

    fn begin_frame(&mut self) {
        self.frame_active = self.phase != StartupPhase::Complete;
        self.frame_invalid = false;
        self.frame_saw_home = false;
        self.frame_text.clear();
    }

    fn finish_frame(&mut self) -> StartupProgress {
        self.frame_active = false;
        if self.frame_invalid {
            self.frame_invalid = false;
            self.frame_saw_home = false;
            self.frame_text.clear();
            return StartupProgress::None;
        }

        let saw_home = std::mem::take(&mut self.frame_saw_home);
        let normalized = self.frame_text.replace('\u{2026}', "...");
        let lower = normalized.to_ascii_lowercase();
        self.frame_text.clear();

        let has_starting = lower.contains("starting session...");
        let has_launcher = lower.contains("new worktree") || lower.contains("resume session");
        let has_interactive_composer =
            normalized.contains('❯') && lower.contains("shift+tab") && lower.contains("ctrl+x");

        match self.phase {
            StartupPhase::AwaitingStarting if has_starting => {
                self.phase = StartupPhase::StartingObserved;
                StartupProgress::StartingObserved
            }
            StartupPhase::StartingObserved
                if saw_home && !has_starting && !has_launcher && has_interactive_composer =>
            {
                self.phase = StartupPhase::Complete;
                StartupProgress::InteractiveReady
            }
            _ => StartupProgress::None,
        }
    }

    fn push_visible(&mut self, character: char) {
        if !self.frame_active || self.frame_invalid {
            return;
        }
        let character = if matches!(character, '\r' | '\n' | '\t') {
            ' '
        } else if character.is_control() {
            return;
        } else {
            character
        };
        // The cap counts accumulated visible UTF-8 bytes after the last Home,
        // not styling/control bytes. A new hide or in-frame Home resets it.
        if self.frame_text.len() + character.len_utf8() > STARTUP_REPAINT_MAX_BYTES {
            self.invalidate_frame();
            return;
        }
        self.frame_text.push(character);
    }

    fn invalidate_frame(&mut self) {
        if self.frame_active {
            self.frame_invalid = true;
            self.frame_text.clear();
        }
    }

    fn is_home_position(parameters: &str) -> bool {
        if !parameters
            .chars()
            .all(|character| character.is_ascii_digit() || character == ';')
        {
            return false;
        }
        let values = parameters.split(';').collect::<Vec<_>>();
        if values.len() > 2 {
            return false;
        }
        values.into_iter().all(|value| {
            value.is_empty() || value.parse::<u16>().is_ok_and(|position| position <= 1)
        })
    }

    fn private_mode_contains(parameters: &str, expected: u16) -> bool {
        let Some(parameters) = parameters.strip_prefix('?') else {
            return false;
        };
        parameters
            .split(';')
            .any(|parameter| parameter.parse::<u16>() == Ok(expected))
    }

    fn terminal_string(kind: TerminalStringKind) -> StartupParserState {
        StartupParserState::String {
            kind,
            chars_seen: 0,
        }
    }

    fn is_c0_executable_or_del(character: char) -> bool {
        matches!(
            character,
            '\u{0000}'..='\u{0017}'
                | '\u{0019}'
                | '\u{001c}'..='\u{001f}'
                | '\u{007f}'
        )
    }
}

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
            None,
            "the measured banner is optional non-modal chrome; production receipts prove Grok accepts and answers routed input while it remains visible"
        );
        assert_eq!(
            classify_work_state("Responding… 0.4s 16K / 500K")
                .unwrap()
                .0,
            WorkState::Thinking
        );
        assert_eq!(classify_work_state("16K / 500K"), None);
    }

    fn repaint(body: &str) -> String {
        format!("\x1b[?25l\x1b[H{body}\x1b[?25h")
    }

    fn starting_repaint() -> String {
        "\x1b[?25lGrok Build 1.0.0  Starting session… 0.0s  Shift+Tab:mode  Ctrl+x:shortcuts\x1b[?25h".into()
    }

    fn ready_repaint(with_banner: bool) -> String {
        let banner = if with_banner {
            "Help improve Grok [Opt out] [Opt in] Off by default  "
        } else {
            ""
        };
        repaint(&format!("{banner}❯  Shift+Tab:mode  Ctrl+x:shortcuts"))
    }

    #[test]
    fn startup_tracker_requires_ordered_completed_repaints() {
        let mut tracker = StartupTracker::default();
        assert_eq!(
            tracker.observe_output(&ready_repaint(true)),
            StartupProgress::None,
            "an interactive-looking pre-start screen must not admit"
        );
        assert_eq!(
            tracker.observe_output(&starting_repaint()),
            StartupProgress::StartingObserved
        );
        assert_eq!(
            tracker.observe_output("\x1b[2;168H⠙ MCP (0/9)\x1b[?25h"),
            StartupProgress::None,
            "an in-place MCP repaint is not an interactive screen"
        );
        assert_eq!(
            tracker.observe_output(&ready_repaint(true)),
            StartupProgress::InteractiveReady
        );
        assert_eq!(
            tracker.observe_output(&starting_repaint()),
            StartupProgress::None,
            "a completed tracker cannot re-enter startup"
        );
    }

    #[test]
    fn startup_tracker_does_not_require_the_optional_telemetry_banner() {
        let mut tracker = StartupTracker::default();
        assert_eq!(
            tracker.observe_output(&starting_repaint()),
            StartupProgress::StartingObserved
        );
        assert_eq!(
            tracker.observe_output(&ready_repaint(false)),
            StartupProgress::InteractiveReady
        );
    }

    #[test]
    fn startup_tracker_refuses_same_epoch_and_launcher_candidates() {
        let mut tracker = StartupTracker::default();
        assert_eq!(
            tracker.observe_output(&repaint(
                "New worktree Resume session Starting session… ❯ Shift+Tab Ctrl+x"
            )),
            StartupProgress::StartingObserved
        );
        assert_eq!(
            tracker.observe_output(&repaint("New worktree Resume session ❯ Shift+Tab Ctrl+x")),
            StartupProgress::None
        );
        assert_eq!(
            tracker.observe_output(&ready_repaint(false)),
            StartupProgress::InteractiveReady
        );
    }

    #[test]
    fn startup_tracker_requires_home_in_the_qualifying_ready_frame() {
        let mut tracker = StartupTracker::default();
        assert_eq!(
            tracker.observe_output(&starting_repaint()),
            StartupProgress::StartingObserved
        );
        assert_eq!(
            tracker.observe_output("\x1b[?25l❯ Shift+Tab Ctrl+x\x1b[?25h"),
            StartupProgress::None,
            "absence checks are unsafe on a partial repaint"
        );
        assert_eq!(
            tracker.observe_output(&ready_repaint(false)),
            StartupProgress::InteractiveReady
        );
    }

    #[test]
    fn startup_tracker_discards_an_incomplete_frame_on_the_next_hide() {
        let mut tracker = StartupTracker::default();
        assert_eq!(
            tracker.observe_output(&starting_repaint()),
            StartupProgress::StartingObserved
        );
        assert_eq!(
            tracker.observe_output("\x1b[?25l\x1b[H❯\x1b[?25lShift+Tab Ctrl+x\x1b[?25h"),
            StartupProgress::None
        );
        assert_eq!(
            tracker.observe_output(&ready_repaint(false)),
            StartupProgress::InteractiveReady
        );
    }

    #[test]
    fn startup_tracker_handles_every_chunk_boundary() {
        let stream = format!("{}{}", starting_repaint(), ready_repaint(false));
        for split in 0..=stream.len() {
            if !stream.is_char_boundary(split) {
                continue;
            }
            let mut tracker = StartupTracker::default();
            let first = tracker.observe_output(&stream[..split]);
            let second = tracker.observe_output(&stream[split..]);
            assert!(
                first == StartupProgress::InteractiveReady
                    || second == StartupProgress::InteractiveReady,
                "split {split} did not admit the measured ordered stream"
            );
        }
    }

    #[test]
    fn startup_tracker_accepts_semantic_home_forms_and_next_home_completion() {
        for home in [
            "\x1b[H",
            "\x1b[1H",
            "\x1b[;H",
            "\x1b[1;H",
            "\x1b[;1H",
            "\x1b[1;1H",
        ] {
            let mut tracker = StartupTracker::default();
            assert_eq!(
                tracker.observe_output(&starting_repaint()),
                StartupProgress::StartingObserved
            );
            let second = format!("\x1b[?25l{home}❯ Shift+Tab Ctrl+x\x1b[?25h");
            assert_eq!(
                tracker.observe_output(&second),
                StartupProgress::InteractiveReady,
                "home form {home:?} did not establish the full ready frame"
            );
        }
    }

    #[test]
    fn startup_tracker_ignores_control_string_lookalikes() {
        for (opener, terminator) in [
            ("\x1b]", "\x07"),
            ("\x1bP", "\x1b\\"),
            ("\x1bX", "\x1b\\"),
            ("\x1b^", "\x1b\\"),
            ("\x1b_", "\x1b\\"),
        ] {
            let mut tracker = StartupTracker::default();
            let lookalike = format!(
                "\x1b[?25l{opener}[HStarting session… ❯ Shift+Tab Ctrl+x[?25h{terminator}\x1b[?25h"
            );
            assert_eq!(tracker.observe_output(&lookalike), StartupProgress::None);
            assert_eq!(
                tracker.observe_output(&starting_repaint()),
                StartupProgress::StartingObserved
            );
            assert_eq!(
                tracker.observe_output(&ready_repaint(false)),
                StartupProgress::InteractiveReady
            );
        }
    }

    #[test]
    fn startup_tracker_hides_ready_markers_in_every_terminal_string_form() {
        for (opener, terminator) in [
            ("\x1b]", "\x07"),
            ("\x1bP", "\x1b\\"),
            ("\x1bX", "\x1b\\"),
            ("\x1b^", "\x1b\\"),
            ("\x1b_", "\x1b\\"),
            ("\u{009d}", "\u{009c}"),
            ("\u{0090}", "\u{009c}"),
            ("\u{0098}", "\u{009c}"),
            ("\u{009e}", "\u{009c}"),
            ("\u{009f}", "\u{009c}"),
        ] {
            let hidden = format!("\x1b[?25l\x1b[H{opener}❯ Shift+Tab Ctrl+x{terminator}\x1b[?25h");
            for split in 0..=hidden.len() {
                if !hidden.is_char_boundary(split) {
                    continue;
                }
                let mut tracker = StartupTracker::default();
                assert_eq!(
                    tracker.observe_output(&starting_repaint()),
                    StartupProgress::StartingObserved
                );
                assert_ne!(
                    tracker.observe_output(&hidden[..split]),
                    StartupProgress::InteractiveReady,
                    "hidden marker prefix admitted for {opener:?} at split {split}"
                );
                assert_ne!(
                    tracker.observe_output(&hidden[split..]),
                    StartupProgress::InteractiveReady,
                    "hidden marker suffix admitted for {opener:?} at split {split}"
                );
            }
        }
    }

    #[test]
    fn startup_tracker_overflow_fails_closed_and_recovers_at_the_next_hide() {
        let mut tracker = StartupTracker::default();
        let oversized = format!(
            "\x1b[?25l{}Starting session… ❯ Shift+Tab Ctrl+x\x1b[?25h",
            "x".repeat(STARTUP_REPAINT_MAX_BYTES)
        );
        assert_eq!(tracker.observe_output(&oversized), StartupProgress::None);
        assert_eq!(
            tracker.observe_output(&starting_repaint()),
            StartupProgress::StartingObserved
        );
        assert_eq!(
            tracker.observe_output(&ready_repaint(false)),
            StartupProgress::InteractiveReady
        );
    }

    #[test]
    fn startup_tracker_accepts_the_exact_failed_production_stream() {
        let stream: String = serde_json::from_str(include_str!("fixtures/grok-startup-at.json"))
            .expect("production Grok startup fixture should remain valid JSON");
        let mut tracker = StartupTracker::default();

        assert_eq!(
            tracker.observe_output(&stream),
            StartupProgress::InteractiveReady,
            "the exact installed-run stream must reach the measured interactive repaint"
        );
        assert_eq!(
            tracker.observe_output(&stream),
            StartupProgress::None,
            "an admitted run must not re-enter startup"
        );
    }
}
