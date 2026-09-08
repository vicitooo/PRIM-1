use std::path::Path;

use shared_types::{
    HarnessLaunchSession, LaunchSpec, LaunchSpecError, PermissionProfile, SessionDefinition,
    WorkState,
};
#[cfg(test)]
use terminal_viewport::CONTROL_MAX_CHARS as TERMINAL_CONTROL_MAX_CHARS;
use terminal_viewport::TerminalViewport;
use uuid::Uuid;

// Measured against Grok Build 1.0.0 (3cd0d0cbce); re-measured against 1.0.5
// (2026-08-28: minimal mode paints ONE completed frame — welcome box +
// "minimal · /help" statusline + ">" composer; the ❯ glyph and the separate
// "Starting session..." splash frame are gone, and the splash text is now
// "Signing in… starting your session."). Unknown future text stays
// fail-closed in lifecycle Starting rather than being inferred ready.
pub const LAUNCHER_MENU_DETAIL: &str = "launcher_menu";
pub const SESSION_STARTING_DETAIL: &str = "session_starting";
pub use terminal_viewport::MAX_CELLS as STARTUP_SCREEN_MAX_CELLS;
pub const MINIMAL_MODE_READY_MARKER: &str = "minimal · /help";

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

/// Applies Grok's startup policy to one shared bounded terminal viewport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupTracker {
    phase: StartupPhase,
    viewport: TerminalViewport,
    frame_active: bool,
    frame_invalid: bool,
    /// Tail of recently observed raw output, so the minimal statusline is
    /// recognised even when it spans a chunk boundary.
    raw_tail: String,
}

impl Default for StartupTracker {
    fn default() -> Self {
        Self {
            phase: StartupPhase::AwaitingStarting,
            viewport: TerminalViewport::default(),
            frame_active: false,
            frame_invalid: false,
            raw_tail: String::new(),
        }
    }
}

impl StartupTracker {
    pub fn begin_run(&mut self) {
        self.phase = StartupPhase::AwaitingStarting;
        self.viewport.begin_run();
        self.frame_active = false;
        self.frame_invalid = false;
        self.raw_tail.clear();
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if self.frame_active {
            self.frame_invalid = true;
        }
        self.viewport.resize(cols, rows);
    }

    pub fn is_interactive_ready(&self) -> bool {
        self.phase == StartupPhase::Complete
    }

    pub fn observe_output_with_mode(
        &mut self,
        chunk: &str,
        bracketed_paste_enabled: bool,
    ) -> StartupProgress {
        if self.phase == StartupPhase::Complete {
            return StartupProgress::None;
        }

        let mut progress = StartupProgress::None;
        for character in chunk.chars() {
            let signals = self.viewport.observe_character(character);
            if signals.projection_invalidated && self.frame_active {
                self.frame_invalid = true;
            }
            if signals.cursor_hidden {
                self.begin_frame();
            }
            if signals.cursor_shown && self.frame_active {
                let observed = self.finish_frame(bracketed_paste_enabled);
                if observed == StartupProgress::InteractiveReady {
                    return observed;
                }
                if observed == StartupProgress::StartingObserved {
                    progress = observed;
                }
            }
        }
        // Grok 1.0.5 minimal is scrollback-native: after the one early frame,
        // the "minimal · /help" statusline lands with the cursor visible —
        // outside any hide/show cycle. Evaluate the settled screen at chunk
        // end too (only until Complete; a string split across chunks simply
        // completes on the next chunk).
        if self.phase != StartupPhase::Complete && !self.frame_active {
            let observed = self.evaluate_screen(bracketed_paste_enabled);
            if observed == StartupProgress::InteractiveReady {
                return observed;
            }
            if observed == StartupProgress::StartingObserved && progress == StartupProgress::None {
                progress = observed;
            }
        }
        // The screen projection is loseable — a resumed replay paints with
        // no frames, a resize can invalidate the one startup frame, and
        // unknown controls can taint the projection — while the raw stream
        // is not. Two raw signals admit startup (measured 2026-08-29,
        // grok 1.0.5):
        //   minimal:    the "minimal · /help" statusline;
        //   fullscreen: the composer+footer burst (❯ + Shift+Tab + Ctrl+x),
        //               repainted on every activity, so even a lost first
        //               frame self-heals on the next burst.
        // Paste-awareness comes from the tracked mode OR the 2004h enable
        // observed in the same window.
        if self.phase != StartupPhase::Complete {
            let mut window =
                String::with_capacity(self.raw_tail.len() + chunk.len());
            window.push_str(&self.raw_tail);
            window.push_str(chunk);
            let window_lower = window.to_ascii_lowercase();
            let paste_observed =
                bracketed_paste_enabled || window.contains("\x1b[?2004h");
            let launcher_visible = window_lower.contains("new worktree");
            // A window still showing the starting splash defers: input is not
            // accepted yet. The next composer burst after startup admits.
            let starting_visible = window_lower.contains("starting session")
                || window_lower.contains("starting your session");
            let minimal_ready = window.contains(MINIMAL_MODE_READY_MARKER);
            let fullscreen_ready =
                window_lower.contains("shift+tab") && window_lower.contains("ctrl+x");
            if paste_observed
                && !launcher_visible
                && !starting_visible
                && (minimal_ready || fullscreen_ready)
            {
                self.phase = StartupPhase::Complete;
                return StartupProgress::InteractiveReady;
            }
            let tail: String = window
                .chars()
                .rev()
                .take(1024)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            self.raw_tail = tail;
        }
        progress
    }

    #[cfg(test)]
    fn observe_output(&mut self, chunk: &str) -> StartupProgress {
        self.observe_output_with_mode(chunk, true)
    }

    fn begin_frame(&mut self) {
        self.frame_active = self.phase != StartupPhase::Complete;
        self.frame_invalid = false;
    }

    fn finish_frame(&mut self, bracketed_paste_enabled: bool) -> StartupProgress {
        self.frame_active = false;
        if self.frame_invalid {
            self.frame_invalid = false;
            return StartupProgress::None;
        }
        self.evaluate_screen(bracketed_paste_enabled)
    }

    /// The measured readiness predicates against the current settled screen —
    /// shared by completed hide/show frames and chunk-end evaluation.
    fn evaluate_screen(&mut self, bracketed_paste_enabled: bool) -> StartupProgress {
        let Some(screen) = self.viewport.trusted_screen() else {
            return StartupProgress::None;
        };
        let normalized = screen.text().replace('\u{2026}', "...");
        let lower = normalized.to_ascii_lowercase();
        let has_starting = lower.contains("starting session...")
            || lower.contains("starting your session");
        let has_launcher = lower.contains("new worktree") || lower.contains("resume session");
        // Glyph-free: without TERM in its env grok paints ">" instead of the
        // composer glyph. The footer chrome is the grok-unique signature; the
        // launcher screen also shows it and stays excluded by has_launcher.
        let has_fullscreen_interactive_composer =
            lower.contains("shift+tab") && lower.contains("ctrl+x");
        let has_minimal_interactive_composer = normalized.contains(MINIMAL_MODE_READY_MARKER);

        match self.phase {
            StartupPhase::AwaitingStarting | StartupPhase::StartingObserved
                if bracketed_paste_enabled
                    && !has_launcher
                    && (has_minimal_interactive_composer
                        || (!has_starting && has_fullscreen_interactive_composer)) =>
            {
                self.phase = StartupPhase::Complete;
                StartupProgress::InteractiveReady
            }
            StartupPhase::AwaitingStarting if has_starting => {
                self.phase = StartupPhase::StartingObserved;
                StartupProgress::StartingObserved
            }
            _ => StartupProgress::None,
        }
    }
}

pub fn launch_spec(
    definition: &SessionDefinition,
    executable: &str,
    harness_session: &HarnessLaunchSession,
) -> Result<LaunchSpec, LaunchSpecError> {
    match harness_session {
        HarnessLaunchSession::New { session_id } => {
            launch_spec_with_session_arg(definition, executable, SessionArg::New(session_id))
        }
        HarnessLaunchSession::Resume { session_id } => {
            launch_spec_with_session_arg(definition, executable, SessionArg::Resume(session_id))
        }
        HarnessLaunchSession::Fresh => {
            launch_spec_with_session_id(definition, executable, Uuid::new_v4())
        }
    }
}

enum SessionArg<'a> {
    New(&'a str),
    Resume(&'a str),
}

fn launch_spec_with_session_id(
    definition: &SessionDefinition,
    executable: &str,
    session_id: Uuid,
) -> Result<LaunchSpec, LaunchSpecError> {
    let session_id = session_id.to_string();
    launch_spec_with_session_arg(definition, executable, SessionArg::New(&session_id))
}

fn launch_spec_with_session_arg(
    definition: &SessionDefinition,
    executable: &str,
    session: SessionArg<'_>,
) -> Result<LaunchSpec, LaunchSpecError> {
    validate_direct_program(executable)?;

    let permission_mode = match definition.permission_profile {
        PermissionProfile::Normal => "default",
        PermissionProfile::Unsafe => "bypassPermissions",
    };
    let mut args: Vec<String> = vec![
        "--fullscreen".into(),
        "--permission-mode".into(),
        permission_mode.into(),
        "--cwd".into(),
        definition.working_dir.clone(),
    ];
    match session {
        SessionArg::New(session_id) => {
            args.extend(["--session-id".into(), session_id.to_string()]);
        }
        // Fused `=` form: grok's --resume takes an optional value and a
        // separated token would be parsed as the prompt positional.
        SessionArg::Resume(session_id) => {
            args.push(format!("--resume={session_id}"));
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

pub fn classify_work_state(chunk: &str) -> Option<(WorkState, Option<String>)> {
    let normalized = strip_ansi_and_controls(chunk).replace('\u{2026}', "...");
    let lower = normalized.to_ascii_lowercase();

    if lower.contains("rate limit") || lower.contains("usage limit") {
        return Some((WorkState::ErrorLoop, Some("rate_limit".into())));
    }
    // Connection banners only; bare "timed out" is ordinary prose (see the
    // Claude and Codex drivers for the 2026-09-08 room incident).
    if lower.contains("stream disconnected")
        || lower.contains("network error")
        || lower.contains("retry your request")
        || lower.contains("request timed out")
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
    if lower.contains("starting session...") || lower.contains("starting your session") {
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
                "--fullscreen",
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
        assert_eq!(
            spec.args[0..3],
            ["--fullscreen", "--permission-mode", "bypassPermissions"]
        );
    }

    #[test]
    fn each_launch_gets_one_fresh_grok_session_id_without_a_bootstrap_prompt() {
        let definition = definition(PermissionProfile::Normal);
        let first = launch_spec(&definition, GROK_EXECUTABLE, &HarnessLaunchSession::Fresh).unwrap();
        let second = launch_spec(&definition, GROK_EXECUTABLE, &HarnessLaunchSession::Fresh).unwrap();

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
            launch_spec(&definition, "grok", &HarnessLaunchSession::Fresh),
            Err(LaunchSpecError::ProgramNotQualified { .. })
        ));
        assert!(matches!(
            launch_spec(&definition, SHELL_SHIM, &HarnessLaunchSession::Fresh),
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
            classify_work_state("Signing in… starting your session."),
            Some((WorkState::Blocked, Some(SESSION_STARTING_DETAIL.into()))),
            "the 1.0.5 splash phrasing is still a blocked startup"
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
        assert_eq!(
            classify_work_state("request timed out").unwrap(),
            (WorkState::Blocked, Some("stream_disconnected".into()))
        );
        assert_eq!(
            classify_work_state("the screenshot capture timed out."),
            None,
            "bare \"timed out\" prose is not a connection banner"
        );
    }

    fn repaint(body: &str) -> String {
        format!("\x1b[?25l\x1b[2J\x1b[H{body}\x1b[?25h")
    }

    fn starting_repaint() -> String {
        "\x1b[?2004h\x1b[?25l\x1b[2J\x1b[HGrok Build 1.0.0  Starting session… 0.0s  Shift+Tab:mode  Ctrl+x:shortcuts\x1b[?25h".into()
    }

    fn ready_repaint(with_banner: bool) -> String {
        let banner = if with_banner {
            "Help improve Grok [Opt out] [Opt in] Off by default  "
        } else {
            ""
        };
        repaint(&format!("{banner}❯  Shift+Tab:mode  Ctrl+x:shortcuts"))
    }

    fn minimal_ready_repaint() -> String {
        concat!(
            "\x1b[?25l",
            "\x1b[15;1Hminimal · /help",
            "\x1b[16;1H❯",
            "\x1b[16;3H\x1b[?25h",
        )
        .into()
    }

    fn positioned_starting_repaint() -> String {
        concat!(
            "\x1b[?2004h\x1b[?25l\x1b[2J",
            "\x1b[11;5HStarting session...",
            "\x1b[18;7H❯",
            "\x1b[21;3HShift+Tab:mode  Ctrl+x:shortcuts",
            "\x1b[18;7H\x1b[?25h",
        )
        .into()
    }

    fn tracker_with_positioned_starting_screen() -> StartupTracker {
        let mut tracker = StartupTracker::default();
        assert_eq!(
            tracker.observe_output(&positioned_starting_repaint()),
            StartupProgress::StartingObserved
        );
        tracker
    }

    #[test]
    fn startup_tracker_admits_the_composer_with_or_without_a_prior_starting_frame() {
        // Measured 2026-08-29: fullscreen 1.0.5 paints no splash frame — the
        // composer frame is the whole startup, exactly like minimal.
        let mut direct = StartupTracker::default();
        assert_eq!(
            direct.observe_output(&ready_repaint(true)),
            StartupProgress::InteractiveReady,
            "the fullscreen composer is the measured ready signal"
        );

        let mut tracker = StartupTracker::default();
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
    fn startup_tracker_admits_the_measured_minimal_mode_completion_frame() {
        let mut tracker = StartupTracker::default();
        assert_eq!(
            tracker.observe_output(&starting_repaint()),
            StartupProgress::StartingObserved
        );
        assert_eq!(
            tracker.observe_output(&minimal_ready_repaint()),
            StartupProgress::InteractiveReady,
            "minimal mode retains the stale startup row, so its exact completed chrome marker must establish readiness"
        );
    }

    #[test]
    fn startup_tracker_admits_the_minimal_statusline_without_a_prior_starting_frame() {
        // Grok 1.0.5 paints no separate starting frame: the statusline frame
        // is the whole measured startup, glyph or no glyph.
        let mut tracker = StartupTracker::default();
        assert_eq!(
            tracker.observe_output(&minimal_ready_repaint()),
            StartupProgress::InteractiveReady
        );

        let mut glyphless = StartupTracker::default();
        assert_eq!(
            glyphless.observe_output(concat!(
                "\x1b[?25l",
                "\x1b[15;1Hminimal · /help",
                "\x1b[16;1H>",
                "\x1b[16;3H\x1b[?25h",
            )),
            StartupProgress::InteractiveReady,
            "1.0.5 replaced the ❯ composer glyph with '>'"
        );
    }

    #[test]
    fn startup_tracker_rejects_minimal_chrome_without_its_exact_marker() {
        let mut incomplete = StartupTracker::default();
        assert_eq!(
            incomplete.observe_output(&starting_repaint()),
            StartupProgress::StartingObserved
        );
        assert_eq!(
            incomplete.observe_output("\x1b[?25l\x1b[15;1H/help\x1b[16;1H❯\x1b[?25h"),
            StartupProgress::None
        );
    }

    #[test]
    fn startup_tracker_admits_a_resumed_replay_without_frames_or_clears() {
        // A --resume replay: transcript lines appended cursor-shown, no 2J,
        // no cursor hide/show, statusline at the end — split mid-marker
        // across two chunks.
        let mut tracker = StartupTracker::default();
        assert_eq!(
            tracker.observe_output_with_mode(
                "\x1b[?2004hyesterday's transcript line one\r\nline two\r\nminimal \u{b7} /he",
                true,
            ),
            StartupProgress::None,
            "the split marker must not admit early"
        );
        assert_eq!(
            tracker.observe_output_with_mode("lp\r\n> \r\n", true),
            StartupProgress::InteractiveReady,
            "the raw statusline completes a frameless resumed replay"
        );

        // The launcher menu never admits through the raw path.
        let mut launcher = StartupTracker::default();
        assert_eq!(
            launcher.observe_output_with_mode(
                "New worktree  Resume session  minimal \u{b7} /help",
                true,
            ),
            StartupProgress::None
        );

        // Without bracketed paste the raw path stays closed.
        let mut unpasted = StartupTracker::default();
        assert_eq!(
            unpasted.observe_output_with_mode("minimal \u{b7} /help\r\n> ", false),
            StartupProgress::None
        );
    }

    #[test]
    fn startup_tracker_admits_a_fullscreen_burst_raw_after_a_lost_frame() {
        // The one startup frame is invalidated by a resize mid-frame; grok
        // sits silent, then a later activity burst repaints composer+footer
        // cursor-shown — the raw window must admit it.
        let mut tracker = StartupTracker::default();
        tracker.observe_output_with_mode("\x1b[?2004h\x1b[?25l welcome box ", true);
        tracker.resize(150, 40); // invalidates the active frame
        assert_eq!(
            tracker.observe_output_with_mode("more paint\x1b[?25h", true),
            StartupProgress::None,
            "the invalidated frame must not admit"
        );
        assert_eq!(
            tracker.observe_output_with_mode("❯ ", true),
            StartupProgress::None,
            "half a burst is not a composer"
        );
        assert_eq!(
            tracker.observe_output_with_mode("  Shift+Tab:mode  Ctrl+x:shortcuts", true),
            StartupProgress::InteractiveReady,
            "the composer+footer burst admits raw, split across chunks"
        );

        // Paste-awareness from the window itself, before the tracked mode
        // flips.
        let mut same_chunk = StartupTracker::default();
        assert_eq!(
            same_chunk.observe_output_with_mode(
                "\x1b[?2004h❯  Shift+Tab:mode  Ctrl+x:shortcuts",
                false,
            ),
            StartupProgress::InteractiveReady
        );

        // The launcher menu never admits through the raw path.
        let mut launcher = StartupTracker::default();
        assert_eq!(
            launcher.observe_output_with_mode(
                "New worktree  Resume session  ❯  Shift+Tab  Ctrl+x",
                true,
            ),
            StartupProgress::None
        );
    }

    #[test]
    fn startup_tracker_observes_the_v105_splash_phrasing_as_starting() {
        let mut tracker = StartupTracker::default();
        assert_eq!(
            tracker.observe_output(&repaint("Signing in… starting your session.")),
            StartupProgress::StartingObserved
        );
    }

    #[test]
    fn startup_tracker_admits_the_exact_v105_fullscreen_noterm_startup_stream() {
        // The icon-launch shape: no TERM in the env, grok paints ASCII
        // fallbacks — no composer glyph anywhere in the stream.
        let stream: String = serde_json::from_str(include_str!(
            "fixtures/grok-startup-fullscreen-noterm-v105.json"
        ))
        .expect("no-TERM fullscreen startup fixture should remain valid JSON");
        assert!(!stream.contains('❯'), "the fixture must be glyph-free");
        let mut tracker = StartupTracker::default();
        tracker.resize(197, 56);

        assert_eq!(
            tracker.observe_output(&stream),
            StartupProgress::InteractiveReady,
            "a glyph-free fullscreen startup must reach the interactive repaint"
        );
    }

    #[test]
    fn startup_tracker_admits_the_exact_v105_fullscreen_startup_stream() {
        let stream: String = serde_json::from_str(include_str!(
            "fixtures/grok-startup-fullscreen-v105.json"
        ))
        .expect("v1.0.5 fullscreen startup fixture should remain valid JSON");
        let mut tracker = StartupTracker::default();
        tracker.resize(197, 56);

        assert_eq!(
            tracker.observe_output(&stream),
            StartupProgress::InteractiveReady,
            "the measured fullscreen startup must reach the interactive repaint"
        );
    }

    #[test]
    fn startup_tracker_admits_the_exact_v105_single_frame_minimal_stream() {
        let stream: String = serde_json::from_str(include_str!(
            "fixtures/grok-startup-minimal-single-frame-v105.json"
        ))
        .expect("v1.0.5 minimal startup fixture should remain valid JSON");
        let mut tracker = StartupTracker::default();
        tracker.resize(120, 30);

        assert_eq!(
            tracker.observe_output(&stream),
            StartupProgress::InteractiveReady,
            "the exact wedge-night stream must reach the measured interactive repaint"
        );
        assert_eq!(
            tracker.observe_output(&stream),
            StartupProgress::None,
            "an admitted run must not re-enter startup"
        );
    }

    #[test]
    fn startup_tracker_refuses_same_epoch_and_launcher_candidates() {
        let mut tracker = StartupTracker::default();
        tracker.observe_output("\x1b[?2004h");
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
    fn startup_tracker_rejects_a_partial_frame_that_leaves_starting_visible() {
        let mut tracker = StartupTracker::default();
        assert_eq!(
            tracker.observe_output(&starting_repaint()),
            StartupProgress::StartingObserved
        );
        assert_eq!(
            tracker.observe_output("\x1b[?25l❯ Shift+Tab Ctrl+x\x1b[?25h"),
            StartupProgress::None,
            "absence checks are unsafe while the projected startup marker remains visible"
        );
        assert_eq!(
            tracker.observe_output(&ready_repaint(false)),
            StartupProgress::InteractiveReady
        );
    }

    #[test]
    fn startup_tracker_admits_measured_ech_erasure_without_a_home_repaint() {
        let mut tracker = tracker_with_positioned_starting_screen();
        assert_eq!(
            tracker.observe_output("\x1b[?25l\x1b[11;5H\x1b[20X\x1b[18;7H\x1b[?25h"),
            StartupProgress::InteractiveReady
        );
    }

    #[test]
    fn startup_tracker_generalizes_starting_erasure_without_layout_markers() {
        for erasure in ["\x1b[11;5H\x1b[2K", "\x1b[11;5H                    "] {
            let mut tracker = tracker_with_positioned_starting_screen();
            assert_eq!(
                tracker.observe_output(&format!("\x1b[?25l{erasure}\x1b[18;7H\x1b[?25h")),
                StartupProgress::InteractiveReady,
                "erasure form {erasure:?} did not update the current screen"
            );
        }

        let mut tracker = StartupTracker::default();
        let scrolling_start = concat!(
            "\x1b[?2004h\x1b[?25l\x1b[2J\x1b[H",
            "Starting session...\r\n",
            "❯ Shift+Tab:mode Ctrl+x:shortcuts",
            "\x1b[?25h",
        );
        tracker.resize(40, 3);
        assert_eq!(
            tracker.observe_output(scrolling_start),
            StartupProgress::StartingObserved
        );
        assert_eq!(
            tracker.observe_output("\x1b[?25l\x1b[3;1H\r\n\x1b[?25h"),
            StartupProgress::InteractiveReady,
            "a measured newline scroll that removes the startup row must update current-screen state"
        );
    }

    #[test]
    fn startup_tracker_detects_start_and_launcher_markers_across_wrapped_rows() {
        let mut starting = StartupTracker::default();
        starting.resize(12, 12);
        assert_eq!(
            starting.observe_output(concat!(
                "\x1b[?2004h\x1b[?25l\x1b[2J\x1b[HxxxxxxStarting session...",
                "\x1b[5;1H❯\x1b[8;1HShift+Tab Ctrl+x\x1b[?25h",
            )),
            StartupProgress::StartingObserved
        );
        assert_eq!(
            starting.observe_output("\x1b[?25l\x1b[?25h"),
            StartupProgress::None,
            "a wrapped startup marker must remain a current-screen blocker"
        );

        let mut launcher = tracker_with_positioned_starting_screen();
        launcher.resize(12, 12);
        assert_eq!(
            launcher.observe_output(concat!(
                "\x1b[?25l\x1b[2J\x1b[HxxxxxxxxNew worktree",
                "\x1b[5;1H❯\x1b[8;1HShift+Tab Ctrl+x\x1b[?25h",
            )),
            StartupProgress::None,
            "a wrapped launcher marker whose space lands at the row boundary must block admission"
        );
    }

    #[test]
    fn startup_tracker_projects_decoded_unicode_widths_and_matches_ctrl_x_in_place() {
        let mut tracker = tracker_with_positioned_starting_screen();
        assert_eq!(
            tracker.observe_output(concat!(
                "\x1b[?25l\x1b[2J\x1b[Hλ😀漢",
                "\x1b[5;1H❯\x1b[8;1HShift+Tab Ctrl+q",
                "\x1b[40;120Hx\x1b[?25h",
            )),
            StartupProgress::None,
            "an unrelated final x cannot substitute for the explicit Ctrl+x binding"
        );
        assert_eq!(
            tracker.observe_output(&ready_repaint(false)),
            StartupProgress::InteractiveReady,
            "decoded Unicode widths must not corrupt a later measured repaint"
        );
    }

    #[test]
    fn startup_tracker_requires_bracketed_paste_and_rejects_launcher_state() {
        let mut disabled = tracker_with_positioned_starting_screen();
        assert_eq!(
            disabled.observe_output_with_mode(
                "\x1b[?2004l\x1b[?25l\x1b[11;5H\x1b[2K\x1b[18;7H\x1b[?25h",
                false,
            ),
            StartupProgress::None
        );

        let mut launcher = tracker_with_positioned_starting_screen();
        assert_eq!(
            launcher.observe_output(concat!(
                "\x1b[?25l\x1b[11;5H\x1b[2K",
                "\x1b[4;1HNew worktree\x1b[5;1HResume session",
                "\x1b[18;7H\x1b[?25h",
            )),
            StartupProgress::None
        );
    }

    #[test]
    fn startup_tracker_unknown_control_taints_until_a_known_full_clear() {
        let mut tracker = tracker_with_positioned_starting_screen();
        assert_eq!(
            tracker.observe_output("\x1b[?25l\x1b[11;5H\x1b[20X\x1b[999z\x1b[18;7H\x1b[?25h"),
            StartupProgress::None
        );
        assert_eq!(
            tracker.observe_output("\x1b[?25l\x1b[18;7H\x1b[?25h"),
            StartupProgress::None,
            "a clean partial frame cannot repair an unknown screen mutation"
        );
        assert_eq!(
            tracker.observe_output(&ready_repaint(false)),
            StartupProgress::InteractiveReady,
            "a known full clear and repaint must recover the bounded projector"
        );
    }

    #[test]
    fn startup_tracker_preserves_dimensions_across_runs_and_fails_closed_on_oversize() {
        let mut tracker = StartupTracker::default();
        tracker.resize(196, 22);
        assert_eq!(tracker.viewport.dimensions(), Some((196, 22)));
        tracker.begin_run();
        assert_eq!(tracker.viewport.dimensions(), Some((196, 22)));

        tracker.resize(u16::MAX, u16::MAX);
        assert_eq!(tracker.viewport.dimensions(), None);
        assert_eq!(
            tracker.observe_output(&starting_repaint()),
            StartupProgress::None
        );
        tracker.resize(120, 40);
        assert_eq!(
            tracker.observe_output(&starting_repaint()),
            StartupProgress::StartingObserved
        );
    }

    #[test]
    fn startup_tracker_recovers_from_a_mid_startup_resize_only_after_full_reconstruction() {
        let mut tracker = StartupTracker::default();
        assert_eq!(
            tracker.observe_output(&starting_repaint()),
            StartupProgress::StartingObserved
        );
        tracker.resize(32, 4);
        assert_eq!(
            tracker.observe_output("\x1b[?25l\x1b[1;1H❯\x1b[2;1HShift+Tab Ctrl+x\x1b[?25h"),
            StartupProgress::None,
            "a partial post-resize repaint cannot establish absent-screen facts"
        );

        let reconstruction = format!(
            "\x1b[?25l\x1b[H{}\x1b[1;1H❯\x1b[2;1HShift+Tab Ctrl+x\x1b[?25h",
            " ".repeat(32 * 4)
        );
        assert_eq!(
            tracker.observe_output(&reconstruction),
            StartupProgress::InteractiveReady,
            "a complete measured reconstruction must recover from a live startup resize"
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
        for stream in [
            format!("{}{}", starting_repaint(), ready_repaint(false)),
            format!("{}{}", starting_repaint(), minimal_ready_repaint()),
            format!(
                "{}{}",
                positioned_starting_repaint(),
                "\x1b[?25l\x1b[11;5H\x1b[20X\x1b[18;7H\x1b[?25h"
            ),
        ] {
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
    }

    #[test]
    fn startup_tracker_accepts_semantic_home_forms_after_a_known_clear() {
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
            let second = format!("\x1b[?25l\x1b[2J{home}❯ Shift+Tab Ctrl+x\x1b[?25h");
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
            let hidden =
                format!("\x1b[?25l\x1b[2J\x1b[H{opener}❯ Shift+Tab Ctrl+x{terminator}\x1b[?25h");
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
    fn startup_tracker_oversized_control_fails_closed_and_recovers_after_a_clear_frame() {
        let mut tracker = StartupTracker::default();
        assert_eq!(
            tracker.observe_output(&starting_repaint()),
            StartupProgress::StartingObserved
        );
        let oversized = format!(
            "\x1b[?25l\x1b[{}H\x1b[?25h",
            "1".repeat(usize::from(TERMINAL_CONTROL_MAX_CHARS) + 1)
        );
        assert_eq!(tracker.observe_output(&oversized), StartupProgress::None);
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
        tracker.resize(196, 22);

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

    #[test]
    fn startup_tracker_accepts_the_exact_av_false_starting_stream() {
        let stream: String = serde_json::from_str(include_str!("fixtures/grok-startup-av.json"))
            .expect("AV Grok startup fixture should remain valid JSON");
        let mut tracker = StartupTracker::default();
        tracker.resize(196, 22);

        assert_eq!(
            tracker.observe_output(&stream),
            StartupProgress::InteractiveReady,
            "the exact AV stream accepted input and a model turn while the old Home-only tracker stayed Starting"
        );
    }
}
