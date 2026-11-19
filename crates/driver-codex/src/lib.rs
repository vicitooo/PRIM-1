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

/// Codex paints an animated decorative layer of Braille Pattern glyphs
/// (U+2800-U+28FF) across the whole viewport, the prompt row and status footer
/// included. Each particle is drawn over a blank cell, so a row reads as
/// `"›<particle>Ask Codex to do anything"` rather than `"› Ask ..."`, and an
/// otherwise-empty row reads as non-empty. Structural matching has to see the
/// blanks the particles sit on, so map them back to spaces rather than dropping
/// them — dropping would close the gap the particle replaced.
fn without_decoration(text: &str) -> String {
    text.chars()
        .map(|character| {
            if matches!(character, '\u{2800}'..='\u{28ff}') {
                ' '
            } else {
                character
            }
        })
        .collect()
}

/// Measured composer geometry on this projection, not input authorization.
/// Callers must separately reject incomplete or synchronized redraws before
/// using it to dismiss a historical menu.
pub fn is_clean_prompt_screen(screen: &TrustedScreen<'_>) -> bool {
    if !screen.cursor_visible() || screen.cursor_col() != 2 {
        return false;
    }
    // Only the prompt row and the status footer are read, so only those rows
    // need to be freshly painted. Requiring the whole grid would strand any
    // pane whose client repaints just the prompt region after a resize.
    if !screen.row_trusted(screen.cursor_row()) {
        return false;
    }
    let Some(input) = screen
        .row_text(screen.cursor_row())
        .as_deref()
        .map(without_decoration)
    else {
        return false;
    };
    let input = input.trim_end();
    if input != "›" && !input.starts_with("› ") {
        return false;
    }
    // 0.164 keeps the composer and footer painted for the whole turn, with
    // the live status line (`• Working (4s • esc to interrupt) · 1 background
    // terminal running …`) as the nearest text above the composer. A composer
    // under a live timer is a busy pane, not a clean prompt (v3 M1 finding:
    // a busy Codex read Idle and woken mid-turn). Read whatever is painted:
    // a stale timer row fails closed until the next repaint.
    if status_row_above(screen, screen.cursor_row()).is_some_and(|row| is_live_working_status(&row)) {
        return false;
    }

    let footer = (screen.cursor_row() + 1..screen.rows()).find_map(|row| {
        let text = without_decoration(&screen.row_text(row)?);
        (!text.trim().is_empty()).then_some((row, text))
    });
    // An untrusted candidate is stale text left behind by an invalidation,
    // not a footer. Bail rather than scanning past it.
    let Some(footer) = footer
        .filter(|(row, _)| screen.row_trusted(*row))
        .map(|(_, text)| text)
        .filter(|footer| footer.starts_with("  "))
    else {
        return false;
    };
    // `model · cwd`, and on 0.164 more segments after it: the thread title once
    // the first turn named it (`model · cwd · title`) and a right-hand
    // `⚠ N warning · f2 to view`. Reading the last segment as the directory
    // made a titled thread never idle again (v3 W1 R3; consistent with the
    // 1066 case, not proved for it). Only the second segment is the directory.
    let mut segments = footer.trim().split(" · ");
    let model = segments.next().unwrap_or_default();
    !model.trim().is_empty()
        && segments
            .next()
            .is_some_and(|segment| looks_like_status_working_directory(segment.trim()))
}

/// The nearest non-blank row above `prompt_row`: where 0.164 paints its live
/// status line, two blank rows above the composer.
fn status_row_above(screen: &TrustedScreen<'_>, prompt_row: usize) -> Option<String> {
    (0..prompt_row).rev().find_map(|row| {
        let text = without_decoration(&screen.row_text(row)?);
        (!text.trim().is_empty()).then_some(text)
    })
}

/// A status row that is the live timer itself, in the shape measured on every
/// rendered 0.164 capture (cx-turn-hf-01, cx-busy-01/02; 145 frames): at
/// column 0, the spinner `•` or `◦`, one space, `Working (` and the elapsed
/// time. The finished turn's `Worked for 24s` summary, plain or `> `-quoted
/// timer-shaped prose and indented text are not (Reviewer 39ad2854 /
/// 218a040b). Residual, fail-closed: an agent message that itself begins
/// with the exact timer text renders the same row.
fn is_live_working_status(row: &str) -> bool {
    let mut chars = row.chars();
    matches!(chars.next(), Some('•' | '◦'))
        && chars.next() == Some(' ')
        && chars.as_str().starts_with("Working (")
        && contains_working_timer(chars.as_str())
}

/// How a `Blocked` classification is cleared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockerClass {
    /// A modal prompt that occupies the screen until it is answered (workspace
    /// trust, command approval, plan chooser). It stays latched for as long as
    /// it is visible on the trusted screen; unrelated output never clears it.
    Modal,
    /// A transient notice printed into the transcript (stream disconnect,
    /// rate/usage limit, auth). It is detected only from fresh output — never
    /// from a rescan of the visible screen — and it is cleared by the next
    /// clean prompt or by later activity. Its text staying in the transcript
    /// after Codex is back at its prompt is not a block (2026-09-08: a final
    /// answer ending in "…screenshot capture timed out." held a room pane
    /// unreachable for four hours).
    Notice,
}

pub fn blocker_class(detail: &str) -> BlockerClass {
    match detail {
        "workspace_trust"
        | "hooks_review"
        | "approval_prompt"
        | "plan_mode_prompt"
        | "update_prompt"
        | "model_migration_prompt"
        | "rate_limit_model_prompt"
        | "authentication" => BlockerClass::Modal,
        _ => BlockerClass::Notice,
    }
}

/// Codex's real connection-trouble banners (codex-cli 0.153 binary strings).
/// Bare "timed out" / "network error" / "retry your request" are NOT in this
/// list on purpose: they match ordinary tool output and agents' own prose.
const STREAM_DISCONNECT_PHRASES: [&str; 5] = [
    "stream disconnected",
    "reconnecting...",
    "reconnect failed",
    "network request disconnected",
    "request timed out",
];

const AUTH_REFRESH_PHRASES: [&str; 11] = [
    "access token could not be refreshed",
    "refresh token",
    "auth refresh",
    "please log out",
    "signed in to another account",
    "401 unauthorized",
    "error 401",
    "http 401",
    "403 forbidden",
    "error 403",
    "http 403",
];

/// Codex's own limit notices (0.163.0 strings: "rate limit exceeded: …",
/// "You are approaching rate limits"). A bare "rate limit" is prose: PRIM-1's
/// own "keeps hitting a rate limit" refusal, shown to a Codex member as a
/// tool result, latched a false block for hours (A16 T-34, 20 Oct).
const RATE_LIMIT_PHRASES: [&str; 5] = [
    "rate limit exceeded",
    "approaching rate limits",
    "429 too many",
    "error 429",
    "http 429",
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct Classified {
    state: WorkState,
    detail: Option<String>,
    /// The phrase that produced a `Blocked` state, for operator-facing errors.
    evidence: Option<String>,
    /// A question's choices (lowercase). Read off the screen, one must start
    /// a numbered option row and no clean prompt may be painted: the same
    /// words in the transcript (an agent quoting them, a diff of these
    /// fixtures) are not the question.
    menu: Option<&'static [&'static str]>,
}

impl Classified {
    fn state(state: WorkState, detail: Option<String>) -> Self {
        Self {
            state,
            detail,
            evidence: None,
            menu: None,
        }
    }

    fn blocked(detail: &str, evidence: &str) -> Self {
        Self {
            state: WorkState::Blocked,
            detail: Some(detail.into()),
            evidence: Some(evidence.into()),
            menu: None,
        }
    }

    fn question(detail: &str, evidence: &str, choices: &'static [&'static str]) -> Self {
        Self {
            menu: Some(choices),
            ..Self::blocked(detail, evidence)
        }
    }

    fn into_pair(self) -> (WorkState, Option<String>) {
        (self.state, self.detail)
    }
}

fn first_phrase<'a>(lower: &str, phrases: &[&'a str]) -> Option<&'a str> {
    phrases
        .iter()
        .copied()
        .find(|phrase| contains_words(lower, phrase))
}

/// `phrase` occurs with no letter or digit directly before or after it. Plain
/// substring matching let two innocent words spell a notice across the space
/// between them (v3 W1 R5: the end of one word, a space and the start of the
/// next latched a pasted room message and withheld its Enter).
fn contains_words(lower: &str, phrase: &str) -> bool {
    lower.match_indices(phrase).any(|(start, matched)| {
        let before = lower[..start].chars().next_back();
        let after = lower[start + matched.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

/// Only modal blockers are read off the visible screen: a modal is on screen
/// exactly while it blocks. Notices are transcript text and never come from
/// a screen rescan (see `BlockerClass::Notice`).
fn classify_current_screen_blocker(screen: &TrustedScreen<'_>) -> Option<Classified> {
    if plan_menu_on_screen(screen) {
        return Some(Classified::blocked(
            "plan_mode_prompt",
            "Implement this plan?",
        ));
    }
    classify_normalized(&without_decoration(&screen.text())).filter(|classified| {
        classified.state == WorkState::Blocked
            && classified
                .detail
                .as_deref()
                .is_some_and(|detail| blocker_class(detail) == BlockerClass::Modal)
            && classified
                .menu
                .is_none_or(|choices| question_menu_on_screen(screen, choices))
    })
}

/// Plan-mode words also occur in the ordinary footer and in agent discussion.
/// Unlike the older question classifiers, this chooser is NEVER inferred from
/// a fresh raw chunk. Require the composed selector below the transcript: its
/// exact heading, actual choices, one selected numbered row, and no subsequent
/// output. Codex rust-v0.162.1/rust-v0.163.0 selection_row_layout.rs uses a label column,
/// then two or more spaces and a description; wrapped descriptions retain
/// their column. Disabled options have no number and do not consume one.
fn plan_menu_on_screen(screen: &TrustedScreen<'_>) -> bool {
    if is_clean_prompt_screen(screen) {
        return false;
    }
    let rows: Vec<String> = (0..screen.rows())
        .filter_map(|row| screen.row_text(row))
        .map(|row| without_decoration(&row).trim_end().to_ascii_lowercase())
        .collect();
    let Some(heading) = rows.iter().rposition(|row| row.trim() == "implement this plan?") else {
        return false;
    };
    let mut options = 0;
    let mut selected = 0;
    let mut implement = false;
    let mut stay = false;
    let mut last_choice = None;
    let mut description_column = None;
    let mut footer = false;
    for row in rows.iter().skip(heading + 1).filter(|row| !row.trim().is_empty()) {
        if footer {
            return false;
        }
        if matches!(row.trim(), "enter select · esc back" | "enter confirm · esc back") {
            footer = true;
            continue;
        }
        // Only the description column may continue onto another line (e.g.
        // "89% used)"). Keep indentation: trim_start would erase this proof.
        if description_column.is_some_and(|column| {
            row.chars().take_while(|character| *character == ' ').count() >= column
        }) {
            continue;
        }
        let (item, highlighted) = if let Some(item) = row.strip_prefix("› ") {
            (item, true)
        } else if let Some(item) = row.strip_prefix("  ") {
            (item.trim_start(), false)
        } else {
            return false;
        };
        let (number, item) = match item.split_once(". ") {
            Some((number, item)) if number.bytes().all(|byte| byte.is_ascii_digit()) => {
                (number.parse::<usize>().ok(), item)
            }
            _ => (None, item),
        };
        let (label, description) = item.split_once("  ").unwrap_or((item, ""));
        let (label, disabled) = label
            .strip_suffix(" (disabled)")
            .map_or((label, false), |label| (label, true));
        let choice = match label {
            "yes, implement this plan" => 0,
            "yes, clear context and implement" => 1,
            "no, stay in plan mode" => 2,
            _ => return false,
        };
        if last_choice.is_some_and(|last| last >= choice)
            || (disabled && (number.is_some() || highlighted || choice == 2))
            || (!disabled && number != Some(options + 1))
        {
            return false;
        }
        last_choice = Some(choice);
        implement |= choice < 2;
        stay |= choice == 2;
        options += usize::from(!disabled);
        selected += usize::from(highlighted);
        let description = description.trim_start();
        if !description.is_empty() {
            let column = row[..row.len() - description.len()].chars().count();
            if description_column.is_some_and(|previous| previous != column) {
                return false;
            }
            description_column = Some(column);
        }
    }
    implement && stay && (1..=3).contains(&options) && selected == 1
}

/// A question replaces the composer: one of its choices starts a numbered
/// option row ("› 1. Update now", "  3. Skip until next version") and no
/// clean prompt is painted.
fn question_menu_on_screen(screen: &TrustedScreen<'_>, choices: &[&str]) -> bool {
    !is_clean_prompt_screen(screen)
        && (0..screen.rows())
            .filter_map(|row| screen.row_text(row))
            .any(|row| {
                let row = without_decoration(&row).to_ascii_lowercase();
                let row = row.trim_start();
                let row = row.strip_prefix('\u{203a}').unwrap_or(row).trim_start();
                row.split_once(". ").is_some_and(|(number, label)| {
                    !number.is_empty()
                        && number.bytes().all(|byte| byte.is_ascii_digit())
                        && choices.iter().any(|choice| label.starts_with(choice))
                })
            })
}

/// v3 W2 #2: the run-state Codex writes into its terminal title when it is
/// launched with `tui.terminal_title=['run-state','thread-title']` (W1,
/// codex-cli 0.164: `ESC ] 0 ; Ready | <thread> BEL`). A hint only: any child
/// tool can print the same bytes, so it never grants readiness, identity or
/// acceptance; a busy title only makes a wake wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleRunState {
    Starting,
    Ready,
    Working,
    Thinking,
    ActionRequired,
}

impl TitleRunState {
    fn parse(title: &str) -> Option<Self> {
        // The thread title follows the first " | " and is never read.
        match title.split(" | ").next()?.trim() {
            "Starting" => Some(Self::Starting),
            "Ready" => Some(Self::Ready),
            "Working" => Some(Self::Working),
            "Thinking" => Some(Self::Thinking),
            "Action Required" => Some(Self::ActionRequired),
            _ => None,
        }
    }

    /// Observed in W1: Ready, Starting (also on a clean composer) and Working.
    /// Thinking and Action Required are named by the binary, not yet seen.
    pub fn is_busy(self) -> bool {
        matches!(self, Self::Working | Self::Thinking | Self::ActionRequired)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Starting => "Starting",
            Self::Ready => "Ready",
            Self::Working => "Working",
            Self::Thinking => "Thinking",
            Self::ActionRequired => "Action Required",
        }
    }
}

const TITLE_MAX_CHARS: usize = 256;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum TitleScan {
    #[default]
    Ground,
    Escape,
    Number(String),
    /// `title` false: an OSC that is not a title, skipped to its end.
    Payload { title: bool, text: String, oversized: bool },
    PayloadEscape { title: bool, text: String, oversized: bool },
}

/// Split-safe: an OSC 0/2 frame may arrive across any number of chunks. A
/// malformed, oversized or unknown title clears the hint rather than keeping
/// an old one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TitleHint {
    scan: TitleScan,
    state: Option<TitleRunState>,
    /// When the title last turned busy from not busy; a busy title that
    /// changes (Working to Thinking) keeps it.
    busy_since: Option<std::time::Instant>,
}

impl TitleHint {
    fn observe(&mut self, character: char) {
        self.scan = match std::mem::take(&mut self.scan) {
            TitleScan::Ground => if character == '\u{1b}' { TitleScan::Escape } else { TitleScan::Ground },
            TitleScan::Escape => after_escape(character),
            TitleScan::Number(mut number) => match character {
                ';' => TitleScan::Payload { title: number == "0" || number == "2", text: String::new(), oversized: false },
                digit if digit.is_ascii_digit() && number.len() < 4 => { number.push(digit); TitleScan::Number(number) }
                '\u{1b}' => TitleScan::Escape,
                _ => TitleScan::Ground,
            },
            TitleScan::Payload { title, mut text, mut oversized } => match character {
                '\u{7}' => { self.finish(title, &text, oversized); TitleScan::Ground }
                '\u{1b}' => TitleScan::PayloadEscape { title, text, oversized },
                control if control.is_control() => { self.abandon(title); TitleScan::Ground }
                other => {
                    if text.chars().count() < TITLE_MAX_CHARS { text.push(other) } else { oversized = true }
                    TitleScan::Payload { title, text, oversized }
                }
            },
            TitleScan::PayloadEscape { title, text, oversized } => {
                if character == '\\' {
                    self.finish(title, &text, oversized);
                    TitleScan::Ground
                } else {
                    // An unterminated frame: the ESC starts the next sequence.
                    self.abandon(title);
                    after_escape(character)
                }
            }
        };
    }

    fn finish(&mut self, title: bool, text: &str, oversized: bool) {
        if title { self.set(if oversized { None } else { TitleRunState::parse(text) }); }
    }

    fn abandon(&mut self, title: bool) {
        if title { self.set(None); }
    }

    fn set(&mut self, state: Option<TitleRunState>) {
        let busy = state.is_some_and(TitleRunState::is_busy);
        self.busy_since = match (busy, self.busy_since) {
            (true, Some(since)) => Some(since),
            (true, None) => Some(std::time::Instant::now()),
            (false, _) => None,
        };
        self.state = state;
    }
}

/// The scan state after an ESC that is followed by `character`.
fn after_escape(character: char) -> TitleScan {
    match character {
        ']' => TitleScan::Number(String::new()),
        '\u{1b}' => TitleScan::Escape,
        _ => TitleScan::Ground,
    }
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
    blocked_evidence: Option<String>,
    pending_classification: Option<(WorkState, Option<String>)>,
    title: TitleHint,
}

impl WorkStateTracker {
    pub fn begin_run(&mut self) {
        self.title = TitleHint::default();
        self.context.clear();
        self.viewport.begin_run();
        self.clean_commit_pending = false;
        self.blocked = false;
        self.blocked_detail = None;
        self.blocked_evidence = None;
        self.pending_classification = None;
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.viewport.resize(cols, rows);
    }

    pub fn observe_output(&mut self, chunk: &str) -> Option<(WorkState, Option<String>)> {
        let mut observed: Option<Classified> = None;
        // A latched modal is resolved only by the screen; a latched notice is
        // stale as soon as any later classification (prompt or activity) lands.
        let mut blocker_resolved =
            !self.blocked || self.latched_class() == Some(BlockerClass::Notice);
        for character in chunk.chars() {
            self.title.observe(character);
            self.context.push(character);
            if self.context.len() > WORK_STATE_CONTEXT_MAX_BYTES * 2 {
                trim_context_suffix(&mut self.context);
            }

            let signals = self.viewport.observe_character(character);
            if (signals.cursor_hidden || signals.projection_invalidated)
                && observed
                    .as_ref()
                    .is_some_and(|classified| classified.state == WorkState::Idle)
            {
                observed = None;
            }
            if signals.cursor_hidden || signals.projection_invalidated {
                self.clean_commit_pending = false;
            }

            if signals.cursor_shown {
                if let Some(classification) = self.classify_context() {
                    self.clean_commit_pending = false;
                    if classification.state == WorkState::Blocked {
                        blocker_resolved = false;
                        observed = Some(classification);
                    } else if blocker_resolved {
                        observed = Some(classification);
                    }
                }
                if let Some(blocker) = self
                    .viewport
                    .trusted_screen()
                    .as_ref()
                    .and_then(classify_current_screen_blocker)
                {
                    self.clean_commit_pending = false;
                    blocker_resolved = false;
                    observed = Some(blocker);
                } else if self
                    .viewport
                    .projected_screen()
                    .as_ref()
                    .is_some_and(is_clean_prompt_screen)
                {
                    self.clean_commit_pending = true;
                    blocker_resolved = true;
                    observed = Some(Classified::state(WorkState::Idle, None));
                }
                self.context.clear();
            }
        }

        if let Some(classification) = self.classify_context() {
            self.clean_commit_pending = false;
            if classification.state == WorkState::Blocked {
                blocker_resolved = false;
                observed = Some(classification);
            } else if blocker_resolved {
                observed = Some(classification);
            }
        }

        if let Some(blocker) = self
            .viewport
            .trusted_screen()
            .as_ref()
            .and_then(classify_current_screen_blocker)
        {
            self.clean_commit_pending = false;
            blocker_resolved = false;
            let repeated_in_this_chunk = observed.as_ref().is_some_and(|classification| {
                classification.state == WorkState::Blocked
                    && classification.detail == blocker.detail
            });
            observed = if repeated_in_this_chunk
                || !self.blocked
                || self.blocked_detail.as_deref() != blocker.detail.as_deref()
            {
                Some(blocker)
            } else {
                None
            };
        } else if let Some(screen) = self.viewport.projected_screen() {
            if is_clean_prompt_screen(&screen) {
                if observed.is_none() && self.clean_commit_pending {
                    blocker_resolved = true;
                    observed = Some(Classified::state(WorkState::Idle, None));
                }
            } else {
                if observed
                    .as_ref()
                    .is_some_and(|classified| classified.state == WorkState::Idle)
                {
                    observed = None;
                }
                self.clean_commit_pending = false;
            }
        } else if observed
            .as_ref()
            .is_some_and(|classified| classified.state == WorkState::Idle)
        {
            observed = None;
        }

        trim_context_suffix(&mut self.context);
        if let Some(classification) = observed.filter(|classification| {
            classification.state == WorkState::Blocked || !self.blocked || blocker_resolved
        }) {
            if classification.state == WorkState::Idle {
                self.clean_commit_pending = false;
            }
            self.blocked = classification.state == WorkState::Blocked;
            self.blocked_detail = classification.detail.clone().filter(|_| self.blocked);
            self.blocked_evidence = classification.evidence.clone().filter(|_| self.blocked);
            let pair = classification.into_pair();
            self.pending_classification = Some(pair.clone());
            Some(pair)
        } else {
            None
        }
    }

    /// v3 W2 #2: the run-state in this run's latest Codex title, if it named
    /// one. A hint only (see [`TitleRunState`]).
    pub fn title_run_state(&self) -> Option<TitleRunState> {
        self.title.state
    }

    /// v3 W2 #2: a busy title (Working, Thinking or Action Required) and how
    /// long the title has read busy without a break.
    pub fn title_busy(&self) -> Option<(TitleRunState, std::time::Duration)> {
        let state = self.title.state.filter(|state| state.is_busy())?;
        Some((state, self.title.busy_since.map_or(std::time::Duration::ZERO, |since| since.elapsed())))
    }

    fn latched_class(&self) -> Option<BlockerClass> {
        self.blocked_detail
            .as_deref()
            .filter(|_| self.blocked)
            .map(blocker_class)
    }

    fn classify_context(&mut self) -> Option<Classified> {
        let normalized = strip_ansi_and_controls(&self.context);
        let classification = classify_normalized(&normalized);
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

    /// The phrase that produced the current block, e.g. `stream disconnected`.
    pub fn blocked_evidence(&self) -> Option<&str> {
        self.blocked_evidence.as_deref()
    }

    pub fn blocked_class(&self) -> Option<BlockerClass> {
        self.latched_class()
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
    classify_normalized(&normalized).map(Classified::into_pair)
}

/// Like `classify_work_state`, also returning the phrase behind a `Blocked`.
pub fn classify_work_state_with_evidence(
    chunk: &str,
) -> Option<(WorkState, Option<String>, Option<String>)> {
    let normalized = strip_ansi_and_controls(chunk);
    classify_normalized(&normalized)
        .map(|classified| (classified.state, classified.detail, classified.evidence))
}

fn classify_normalized(normalized: &str) -> Option<Classified> {
    let lower = normalized.to_ascii_lowercase();

    if lower.contains("do you trust the contents of this directory?")
        && lower.contains("yes, continue")
        && lower.contains("press enter to continue")
    {
        return Some(Classified::blocked(
            "workspace_trust",
            "Do you trust the contents of this directory?",
        ));
    }
    // codex-cli 0.162.1 and 0.163.0 reworded the trust screen.
    if lower.contains("trust this folder?") && lower.contains("trust and continue") {
        return Some(Classified::question(
            "workspace_trust",
            "Trust this folder?",
            &["trust and continue", "open restricted"],
        ));
    }

    if (lower.contains("would you like to run the following command?")
        || lower.contains("do you want to allow codex to run this command?"))
        && lower.contains("yes")
        && lower.contains("no")
    {
        return Some(Classified::blocked(
            "approval_prompt",
            "Would you like to run the following command?",
        ));
    }

    // Questions that hold the TUI until a person answers (0.163.0 binary
    // strings); nothing answers them automatically. The transcript line
    // "Update available! ... Run <cmd> to update." has neither choice, so it
    // is not a question. Fresh output matches on the phrases (fail closed;
    // the next clean prompt resolves a quote); a screen rescan also needs
    // the menu shape, so a quote left on screen does not latch.
    if lower.contains("update available")
        && (lower.contains("update now (runs") || lower.contains("skip until next version"))
    {
        return Some(Classified::question(
            "update_prompt",
            "Update available!",
            &["update now (runs", "skip until next version"],
        ));
    }
    if lower.contains("try new model") && lower.contains("use existing model") {
        return Some(Classified::question(
            "model_migration_prompt",
            "Try new model",
            &["try new model", "use existing model"],
        ));
    }
    // Before the rate-limit notice phrases: this one waits for a choice.
    if lower.contains("approaching rate limits") && lower.contains("keep current model") {
        return Some(Classified::question(
            "rate_limit_model_prompt",
            "Approaching rate limits",
            &["keep current model"],
        ));
    }
    // 0.164: hooks it has not seen wait for a person's trust decision.
    if lower.contains("hooks need review") && lower.contains("continue without trusting") {
        return Some(Classified::question(
            "hooks_review",
            "Hooks need review",
            &["review hooks", "trust all and continue", "continue without trusting"],
        ));
    }
    if lower.contains("sign in with chatgpt") && lower.contains("sign in with device code") {
        return Some(Classified::question(
            "authentication",
            "Sign in with ChatGPT",
            &["sign in with chatgpt", "sign in with device code"],
        ));
    }

    // Numeric status codes require their HTTP phrasing: a bare "401"/"403"
    // matches any digit run (token counts, offsets, sequence numbers — a
    // resumed replay carried 131 of them and latched a false auth block).
    if let Some(phrase) = first_phrase(&lower, &AUTH_REFRESH_PHRASES) {
        return Some(Classified::blocked("auth_refresh", phrase));
    }

    if contains_words(&lower, "hit your usage") {
        return Some(Classified::blocked("usage_limit", "hit your usage limit"));
    }
    if let Some(phrase) = first_phrase(&lower, &RATE_LIMIT_PHRASES) {
        return Some(Classified::blocked("rate_limit", phrase));
    }
    if let Some(phrase) = first_phrase(&lower, &STREAM_DISCONNECT_PHRASES) {
        return Some(Classified::blocked("stream_disconnected", phrase));
    }

    if contains_working_timer(normalized) {
        return Some(Classified::state(
            WorkState::Thinking,
            extract_working_detail(normalized),
        ));
    }

    // 0.164 draws its activity as `• Ran <command>` and `• Explored` with
    // `└ Read <file>` rows under it (all three captured, v3 W1). A ToolCall is
    // not cosmetic: it marks the run observed and clears a latched notice
    // (never a modal), so only captured forms are listed.
    if lower.contains("> run ")
        || lower.contains("\nrun ")
        || lower.contains("> read ")
        || lower.contains("\nread ")
        || lower.contains("tool call")
        || lower.contains("apply_patch")
        || lower.contains("• ran ")
        || lower.contains("• explored")
        || lower.contains("└ read ")
    {
        return Some(Classified::state(WorkState::ToolCall, None));
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
    let mut saw_paren = false;
    for ch in rest.chars().take(24) {
        if ch.is_ascii_digit() {
            saw_digit = true;
        }
        if saw_digit && (ch == 'm' || ch == 's') {
            saw_unit = true;
            break;
        }
        // 0.164: `Working (3s • esc to interrupt)` — one bracket before the
        // timer (v3 W1; 11 Oct report item 3).
        if ch == '(' && !saw_digit && !saw_paren {
            saw_paren = true;
            continue;
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
        if ch == '\r' || ch == '\n' || ch == '·' || ch == '•' || ch == '|' {
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
    terminal_launch_spec(definition, program, prefix_args, harness_session, None)
}

/// v3 W4 (opt-in App Server member): the visible terminal, attached with
/// `--remote` to the member's own private owner at `endpoint`. The owner
/// holds the thread and runs its tools, so the pane MCP goes on the owner,
/// not here, and `--no-daemon` (which only keeps the TUI off the shared
/// daemon) does not apply. A resume names the stored thread, as without the
/// owner (trial 11: `--remote <endpoint> resume <id>`).
pub fn remote_launch_spec(
    definition: &SessionDefinition,
    program: &str,
    prefix_args: &[String],
    harness_session: &HarnessLaunchSession,
    endpoint: &str,
) -> Result<LaunchSpec, LaunchSpecError> {
    terminal_launch_spec(definition, program, prefix_args, harness_session, Some(endpoint))
}

/// v3 W4: the member's private App Server owner, one per run, listening only
/// on `endpoint` (a socket in a directory private to the user; Codex refuses
/// any other). Its permission settings match the terminal's flags, so the
/// thread the terminal starts runs under the member's profile whichever side
/// applies them. No terminal of its own: the visible TUI attaches to it.
pub fn app_server_owner_spec(
    definition: &SessionDefinition,
    program: &str,
    prefix_args: &[String],
    endpoint: &str,
) -> Result<LaunchSpec, LaunchSpecError> {
    validate_direct_program(program)?;
    let mut args = prefix_args.to_vec();
    args.extend(["app-server".into(), "--listen".into(), endpoint.to_string()]);
    let (approval, sandbox) = match definition.permission_profile {
        PermissionProfile::Normal => ("on-request", "workspace-write"),
        PermissionProfile::Unsafe => ("never", "danger-full-access"),
    };
    args.extend([
        "-c".into(),
        format!("approval_policy='{approval}'"),
        "-c".into(),
        format!("sandbox_mode='{sandbox}'"),
        "-c".into(),
        "check_for_update_on_startup=false".into(),
        "-c".into(),
        "features.worktrees=false".into(),
    ]);
    Ok(LaunchSpec {
        program: program.to_string(),
        args,
        working_dir: definition.working_dir.clone(),
        env: Vec::new(),
        display_name: format!("{} (App Server owner)", definition.label),
    })
}

fn terminal_launch_spec(
    definition: &SessionDefinition,
    program: &str,
    prefix_args: &[String],
    harness_session: &HarnessLaunchSession,
    remote: Option<&str>,
) -> Result<LaunchSpec, LaunchSpecError> {
    validate_direct_program(program)?;

    let mut args = prefix_args.to_vec();
    if let Some(endpoint) = remote {
        args.extend(["--remote".into(), endpoint.to_string()]);
    }
    // Codex cannot pin an id at launch; it needs a run-bound identity report.
    // Resuming is the `resume` subcommand, which accepts the same
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
    // Per-process only, no config is written: the startup update prompt
    // cannot hold a pane, and without the shared app-server daemon the
    // pane's own process holds its thread writer lock, so its conversation
    // is provable. Both verified in codex-cli 0.162.1 and 0.163.0 --help.
    // The `worktrees` feature (stable, on by default) makes /new move a git
    // repository's thread into ~/.codex/worktrees/<n>/<repo>: the member
    // would leave its folder and the new thread could not be followed (19 Oct
    // walk). A pane member keeps working in its own folder.
    args.extend([
        "-c".into(),
        "check_for_update_on_startup=false".into(),
        "-c".into(),
        "features.worktrees=false".into(),
        // v3 W2 #2: Codex writes its run-state into the terminal title, a
        // readiness hint for PRIM-1 (W1). TOML literal strings, so no double
        // quote needs escaping in the command line; codex-cli 0.164 parses
        // this form as the same sequence (`features list`, 25 Oct).
        "-c".into(),
        "tui.terminal_title=['run-state','thread-title']".into(),
    ]);
    if remote.is_none() {
        args.push("--no-daemon".into());
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
    #[test]
    fn a16_prose_about_limits_is_not_a_notice_but_codexs_own_notice_is() {
        // PRIM-1's own refusal and a report about it, as a Codex member saw them.
        for prose in [
            "Claude A (0a1b2c3d) can't take a routed message right now: it keeps hitting a rate limit.",
            "PRIM-1 reported repeated rate limiting on Claude PRIM-1.",
        ] {
            assert_eq!(classify_work_state(prose), None, "{prose:?}");
        }
        assert_eq!(classify_work_state("error: rate limit exceeded: retry in 20s").unwrap(),
            (WorkState::Blocked, Some("rate_limit".into())));
        // The refusal reaching a tracker as fresh output latches nothing.
        let mut tracker = WorkStateTracker::default();
        assert_eq!(tracker.observe_output(
            "Claude A (0a1b2c3d) can't take a routed message right now: it keeps hitting a rate limit.\r\n"), None);
        assert!(!tracker.is_blocked());
    }

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
            codex_transport: Default::default(),
        }
    }

    const ENDPOINT: &str = "unix://C:/runtime/cxo/0123/run1.sock";

    /// v3 W4 S1: the remote terminal keeps the member's flags, adds the
    /// endpoint before any resume, and drops only `--no-daemon`.
    #[test]
    fn w4_remote_terminal_attaches_to_its_owner_and_keeps_the_members_flags() {
        let definition = definition(PermissionProfile::Normal);
        let local = launch_spec(&definition, CODEX_EXECUTABLE, &[], &HarnessLaunchSession::Fresh).unwrap();
        let remote = remote_launch_spec(&definition, CODEX_EXECUTABLE, &[], &HarnessLaunchSession::Fresh, ENDPOINT).unwrap();
        assert_eq!(&remote.args[..2], &["--remote".to_string(), ENDPOINT.to_string()]);
        let without: Vec<&String> = local.args.iter().filter(|arg| *arg != "--no-daemon").collect();
        assert_eq!(remote.args[2..].iter().collect::<Vec<_>>(), without);
        let resume = remote_launch_spec(&definition, CODEX_EXECUTABLE, &[],
            &HarnessLaunchSession::Resume { session_id: "019a0000-0000-7000-8000-000000000001".into() }, ENDPOINT).unwrap();
        assert_eq!(&resume.args[..4], &["--remote".to_string(), ENDPOINT.to_string(), "resume".to_string(),
            "019a0000-0000-7000-8000-000000000001".to_string()]);
        assert!(!remote.args.iter().any(|arg| arg.contains("mcp_servers")), "the owner carries the pane MCP");
    }

    /// v3 W4 S1: the owner listens only on its endpoint and carries the
    /// member's permission profile as configuration.
    #[test]
    fn w4_owner_listens_on_its_private_endpoint_with_the_members_profile() {
        let normal = app_server_owner_spec(&definition(PermissionProfile::Normal), CODEX_EXECUTABLE, &[], ENDPOINT).unwrap();
        assert_eq!(normal.args, ["app-server", "--listen", ENDPOINT, "-c", "approval_policy='on-request'", "-c",
            "sandbox_mode='workspace-write'", "-c", "check_for_update_on_startup=false", "-c", "features.worktrees=false"]
            .map(String::from));
        assert_eq!(normal.working_dir, WORKSPACE_ROOT);
        let prefix = vec![CODEX_JS.to_string()];
        let unsafe_owner = app_server_owner_spec(&definition(PermissionProfile::Unsafe), NODE_EXECUTABLE, &prefix, ENDPOINT).unwrap();
        assert_eq!(unsafe_owner.args[0], CODEX_JS);
        assert!(unsafe_owner.args.contains(&"approval_policy='never'".to_string())
            && unsafe_owner.args.contains(&"sandbox_mode='danger-full-access'".to_string()));
        assert!(app_server_owner_spec(&definition(PermissionProfile::Normal), "codex", &[], ENDPOINT).is_err(),
            "an unqualified program is refused as for the terminal");
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
                "-c".to_string(),
                "check_for_update_on_startup=false".to_string(),
                "-c".to_string(),
                "features.worktrees=false".to_string(),
                "-c".to_string(),
                "tui.terminal_title=['run-state','thread-title']".to_string(),
                "--no-daemon".to_string(),
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
                "-c".to_string(),
                "check_for_update_on_startup=false".to_string(),
                "-c".to_string(),
                "features.worktrees=false".to_string(),
                "-c".to_string(),
                "tui.terminal_title=['run-state','thread-title']".to_string(),
                "--no-daemon".to_string(),
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
    fn resume_keeps_the_update_check_off_and_the_daemon_out() {
        let definition = definition(PermissionProfile::Unsafe);
        let spec = launch_spec(
            &definition,
            CODEX_EXECUTABLE,
            &[],
            &HarnessLaunchSession::Resume {
                session_id: "019a0000-0000-7000-8000-000000000001".into(),
            },
        )
        .unwrap();
        assert_eq!(
            spec.args,
            vec![
                "resume".to_string(),
                "019a0000-0000-7000-8000-000000000001".to_string(),
                "--dangerously-bypass-approvals-and-sandbox".to_string(),
                "-c".to_string(),
                "check_for_update_on_startup=false".to_string(),
                "-c".to_string(),
                "features.worktrees=false".to_string(),
                "-c".to_string(),
                "tui.terminal_title=['run-state','thread-title']".to_string(),
                "--no-daemon".to_string(),
                "--no-alt-screen".to_string(),
                "-C".to_string(),
                WORKSPACE_ROOT.to_string(),
            ]
        );
        assert!(
            spec.env.is_empty(),
            "the override is argv only, never config or env"
        );
    }

    #[test]
    fn classify_codex_working_timer() {
        assert_eq!(
            classify_work_state("Working 12s").unwrap(),
            (WorkState::Thinking, Some("Working 12s".into()))
        );
        // 0.164 (v3 W1).
        assert_eq!(
            classify_work_state("◦ Working (3s • esc to interrupt)").unwrap(),
            (WorkState::Thinking, Some("Working (3s".into()))
        );
        assert_eq!(classify_work_state("Working (on it) soon"), None);
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
            classify_work_state("Create a plan? shift+tab use Plan mode"),
            None,
            "ordinary plan-mode instructions are not a selector"
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
    fn current_screen_modal_blockers_and_clean_commits_preserve_stream_order() {
        let clean = current_prompt_screen("");
        let modal = "Would you like to run the following command?\n› 1. Yes\n  2. No";

        let mut persistent = WorkStateTracker::default();
        assert_eq!(
            persistent.observe_output(&current_prompt_screen(modal)),
            Some((WorkState::Blocked, Some("approval_prompt".into())))
        );
        assert_eq!(persistent.blocked_class(), Some(BlockerClass::Modal));
        persistent.acknowledge_pending();
        assert_eq!(persistent.observe_output("unrelated repaint"), None);
        assert!(persistent.is_blocked());
        assert_eq!(
            persistent.observe_output("\u{1b}[999zWorking 1s"),
            None,
            "activity text on an untrusted projection cannot clear a modal"
        );
        assert!(persistent.is_blocked());

        let mut blocker_after_clean = WorkStateTracker::default();
        assert_eq!(
            blocker_after_clean.observe_output(&format!("{clean}{modal}")),
            Some((WorkState::Blocked, Some("approval_prompt".into()))),
            "a modal painted after the clean commit must win"
        );

        let mut clean_after_blocker = WorkStateTracker::default();
        assert_eq!(
            clean_after_blocker.observe_output(&format!("{modal}{clean}")),
            Some((WorkState::Idle, None)),
            "a later trusted repaint may clear an erased modal"
        );

        let mut final_clean = WorkStateTracker::default();
        assert_eq!(
            final_clean.observe_output(&format!("{clean}{modal}{clean}")),
            Some((WorkState::Idle, None)),
            "the final trusted current screen is authoritative"
        );
    }

    #[test]
    fn notice_in_the_transcript_does_not_block_a_pane_that_is_back_at_its_prompt() {
        // The 2026-09-08 room incident, byte-shape: Codex prints its final
        // answer (which happens to contain a notice phrase), then paints a
        // clean prompt. The pane is typeable; nothing is blocked.
        let clean = current_prompt_screen("");
        let notice = "You've hit your usage limit.";

        let mut back_at_prompt = WorkStateTracker::default();
        assert_eq!(
            back_at_prompt.observe_output(&current_prompt_screen(notice)),
            Some((WorkState::Idle, None)),
            "notice text under a clean prompt on the same screen is not a block"
        );
        assert!(!back_at_prompt.is_blocked());

        let mut notice_then_prompt = WorkStateTracker::default();
        assert_eq!(
            notice_then_prompt.observe_output(notice),
            Some((WorkState::Blocked, Some("usage_limit".into())))
        );
        assert_eq!(
            notice_then_prompt.blocked_class(),
            Some(BlockerClass::Notice)
        );
        assert_eq!(
            notice_then_prompt.blocked_evidence(),
            Some("hit your usage limit")
        );
        notice_then_prompt.acknowledge_pending();
        assert_eq!(
            notice_then_prompt.observe_output("unrelated repaint"),
            None,
            "silence does not clear a notice either"
        );
        assert!(notice_then_prompt.is_blocked());
        assert_eq!(
            notice_then_prompt.observe_output(&clean),
            Some((WorkState::Idle, None)),
            "the next clean prompt clears a notice even though its text stays in the transcript"
        );
        assert!(!notice_then_prompt.is_blocked());
        assert_eq!(notice_then_prompt.blocked_evidence(), None);

        // A keystroke echo (the operator typing) must not re-latch from the
        // transcript text still on screen.
        assert_eq!(
            notice_then_prompt.observe_output("\u{1b}[5;3Ht\u{1b}[?25h"),
            None
        );
        assert!(!notice_then_prompt.is_blocked());

        // /new + /resume re-renders the transcript with the notice in it and
        // then paints the prompt: same chunk, same answer.
        let mut resumed = WorkStateTracker::default();
        assert_eq!(
            resumed.observe_output(&format!("{notice}{clean}")),
            Some((WorkState::Idle, None))
        );
        assert!(!resumed.is_blocked());
    }

    #[test]
    fn stream_disconnect_banner_blocks_until_codex_recovers() {
        let banner = "stream disconnected - retrying sampling request (1/5 in 200ms)...";
        let mut tracker = WorkStateTracker::default();
        assert_eq!(
            tracker.observe_output(banner),
            Some((WorkState::Blocked, Some("stream_disconnected".into())))
        );
        assert_eq!(tracker.blocked_evidence(), Some("stream disconnected"));
        assert_eq!(tracker.blocked_class(), Some(BlockerClass::Notice));
        tracker.acknowledge_pending();

        // Codex recovered and is working again: the notice is stale.
        assert_eq!(
            tracker.observe_output("Working 4s · esc to interrupt"),
            Some((WorkState::Thinking, Some("Working 4s".into())))
        );
        assert!(!tracker.is_blocked());

        let mut failed = WorkStateTracker::default();
        assert_eq!(
            failed.observe_output("Reconnect failed"),
            Some((WorkState::Blocked, Some("stream_disconnected".into())))
        );
        assert_eq!(failed.blocked_evidence(), Some("reconnect failed"));
        assert_eq!(
            failed.observe_output(&current_prompt_screen("")),
            Some((WorkState::Idle, None))
        );
    }

    #[test]
    fn modal_wins_over_stale_notice_text_on_the_same_screen() {
        // Adversarial: the transcript still shows a notice phrase AND an
        // approval modal is up. The modal is what blocks, and it stays latched
        // through unrelated output.
        let screen = current_prompt_screen(
            "the request timed out earlier\nWould you like to run the following command?\n› 1. Yes\n  2. No",
        );
        let mut tracker = WorkStateTracker::default();
        assert_eq!(
            tracker.observe_output(&screen),
            Some((WorkState::Blocked, Some("approval_prompt".into())))
        );
        assert_eq!(tracker.blocked_class(), Some(BlockerClass::Modal));
        tracker.acknowledge_pending();
        assert_eq!(tracker.observe_output("Working 2s"), None);
        assert!(tracker.is_blocked());
    }

    #[test]
    fn ordinary_timed_out_prose_is_not_a_connection_banner() {
        for prose in [
            "Mobile layout measurements passed; the final phone screenshot capture timed out.",
            "{\"message\":\"Wait timed out.\",\"timed_out\":true}",
            "network error handling is covered by the retry tests",
            "please retry your request tomorrow, said the docs",
        ] {
            assert_eq!(classify_work_state(prose), None, "{prose:?}");
        }
        for banner in [
            "stream disconnected - retrying sampling request (2/5 in 400ms)...",
            "Reconnecting...",
            "Reconnect failed",
            "Network request disconnected after 30s",
            "request timed out",
        ] {
            assert_eq!(
                classify_work_state(banner).map(|(state, _)| state),
                Some(WorkState::Blocked),
                "{banner:?}"
            );
        }
        assert_eq!(
            classify_work_state_with_evidence("Reconnecting..."),
            Some((
                WorkState::Blocked,
                Some("stream_disconnected".into()),
                Some("reconnecting...".into())
            ))
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

    /// A pane whose grid is invalidated after the initial full-canvas paint
    /// (`TerminalViewport::resize` marks every cell invalid) must still reach
    /// Idle from the inline prompt repaints Codex emits under
    /// `--no-alt-screen`. Demanding whole-grid trust stranded such a pane
    /// permanently: no Idle meant `work_state_observed` stayed false and the
    /// supervisor refused every routed room delivery to it.
    #[test]
    fn clean_prompt_survives_an_invalidated_grid_without_a_full_repaint() {
        let stream: String =
            serde_json::from_str(include_str!("fixtures/codex-clean-prompt-e16a-prefix.json"))
                .expect("e16a production prefix should remain valid JSON");

        let mut tracker = WorkStateTracker::default();
        tracker.begin_run();
        tracker.resize(196, 22);
        assert_eq!(
            tracker.observe_output(&stream),
            Some((WorkState::Idle, None)),
            "baseline: the full-canvas paint reaches Idle"
        );

        // The supervisor resizes the viewport (session start, or any pane
        // geometry change), invalidating every cell.
        tracker.resize(196, 22);

        // Codex repaints only the prompt region from here on.
        let inline_repaint = &stream[stream.len() - 700..];
        assert_eq!(
            tracker.observe_output(inline_repaint),
            Some((WorkState::Idle, None)),
            "an inline prompt repaint must re-establish Idle after invalidation"
        );
    }

    /// Stale cells left behind by a projection taint must not be read as a
    /// status footer: only rows repainted since the taint count. Unlike a
    /// resize (which allocates a blank grid), a taint keeps the old contents
    /// while dropping their validity, so an unguarded footer scan would match
    /// text that is no longer on screen.
    #[test]
    fn stale_rows_below_the_prompt_are_not_accepted_as_a_footer() {
        let stream: String =
            serde_json::from_str(include_str!("fixtures/codex-clean-prompt-e16a-prefix.json"))
                .expect("e16a production prefix should remain valid JSON");

        let mut tracker = WorkStateTracker::default();
        tracker.begin_run();
        tracker.resize(196, 22);
        assert_eq!(
            tracker.observe_output(&stream),
            Some((WorkState::Idle, None)),
            "baseline: the full-canvas paint reaches Idle"
        );

        // Taint the projection: cell contents survive, their validity does not.
        tracker.observe_output("\u{1b}[999z");

        // Repaint the prompt row alone -- erase-to-end-of-line makes that row
        // trusted again -- while the status footer below stays stale.
        let prompt_row_only = concat!(
            "\u{1b}[?25l\u{1b}[13;1H\u{1b}[K",
            "\u{1b}[38;2;247;181;85m\u{1b}[1m\u{203a}\u{1b}[39m\u{1b}[22m",
            " \u{1b}[2mSummarize recent commits\u{1b}[22m\u{1b}[K",
            "\u{1b}[13;3H\u{1b}[?25h",
        );
        assert_eq!(
            tracker.observe_output(prompt_row_only),
            None,
            "a trusted prompt row above a stale footer is not a clean prompt"
        );
    }

    /// Codex v0.154 paints an animated Braille-glyph particle layer over the
    /// whole viewport. Particles land on the blank after the prompt marker
    /// (`"›<particle>Ask Codex..."`) and turn otherwise-empty rows above the
    /// status footer into non-empty ones, which made the clean-prompt check
    /// reject every settled prompt. The pane then never reported Idle, so
    /// `work_state_observed` stayed false and the supervisor refused every
    /// routed room delivery to it. Captured from a live 447x88 pane.
    #[test]
    fn decorated_prompt_is_still_recognised_as_a_clean_prompt() {
        let stream: String =
            serde_json::from_str(include_str!("fixtures/codex-decorated-prompt-v0154.json"))
                .expect("decorated prompt fixture should remain valid JSON");
        assert!(
            stream.chars().any(|c| matches!(c, '\u{2800}'..='\u{28ff}')),
            "fixture must carry the decorative particle layer"
        );

        let mut tracker = WorkStateTracker::default();
        tracker.begin_run();
        tracker.resize(120, 24);
        assert_eq!(
            tracker.observe_output(&stream),
            Some((WorkState::Idle, None)),
            "a decorated settled prompt must still classify as Idle"
        );
    }

    #[test]
    fn decorated_prompt_after_resize_clears_a_transient_notice() {
        let stream: String =
            serde_json::from_str(include_str!("fixtures/codex-decorated-prompt-v0154.json"))
                .expect("decorated prompt fixture should remain valid JSON");
        let mut tracker = WorkStateTracker::default();
        tracker.begin_run();
        assert_eq!(
            tracker.observe_output("stream disconnected - retrying sampling request"),
            Some((WorkState::Blocked, Some("stream_disconnected".into())))
        );
        tracker.resize(120, 24);
        assert_eq!(tracker.observe_output(&stream), Some((WorkState::Idle, None)));
        assert!(!tracker.is_blocked());
        assert_eq!(tracker.blocked_evidence(), None);
    }

    /// The particle layer must be neutralised without closing the gap it was
    /// drawn over: dropping the glyph would splice `"›"` onto the prompt text.
    #[test]
    fn decoration_is_mapped_to_blanks_not_removed() {
        assert_eq!(without_decoration("›⠁Ask Codex"), "› Ask Codex");
        assert_eq!(without_decoration("   ⠄  ⢀ ").trim(), "");
        assert_eq!(
            without_decoration(r"  gpt-6-astra high · ~\Desktop\sample-main"),
            r"  gpt-6-astra high · ~\Desktop\sample-main",
            "undecorated text must pass through untouched"
        );
    }

    /// The 0.163.0 startup update prompt, composed from its binary strings:
    /// header, release-notes line, three numbered choices and the key hint.
    const UPDATE_PROMPT_V0163: &str = concat!(
        "\u{1b}[?25l\u{1b}[2J\u{1b}[H",
        "  \u{2728} Update available! 0.163.0 -> 0.164.0\r\n\r\n",
        "  Release notes: https://github.com/openai/codex/releases/latest\r\n\r\n",
        "\u{203a} 1. Update now (runs `npm install -g @openai/codex`)\r\n",
        "  2. Skip\r\n",
        "  3. Skip until next version\r\n\r\n",
        "  enter continue \u{b7} esc skip\r\n",
        "\u{1b}[?25h",
    );

    #[test]
    fn startup_and_model_questions_are_modal_blocks_with_their_own_names() {
        for (screen, detail) in [
            (UPDATE_PROMPT_V0163, "update_prompt"),
            (
                "Update available! 0.162.1 -> 0.163.0\n› 1. Update now (runs `codex update`)\n  2. Skip",
                "update_prompt",
            ),
            (
                "Update available! 0.162.1 -> 0.163.0\n  2. Skip\n› 3. Skip until next version",
                "update_prompt",
            ),
            (
                "Codex just got an upgrade. Introducing gpt-6.\n› 1. Try new model\n  2. Use existing model\n  enter confirm · ctrl + c quit",
                "model_migration_prompt",
            ),
            (
                "Approaching rate limits\nSwitch to gpt-6-mini for lower credit usage?\n› 1. Switch to gpt-6-mini\n  2. Keep current model\n  3. Keep current model (never show again)\n  Hide future rate limit reminders about switching models",
                "rate_limit_model_prompt",
            ),
            (
                "Sign in with ChatGPT to use Codex as part of your paid plan\n› 1. Sign in with ChatGPT\n  2. Sign in with Device Code\n  3. Provide your own API key",
                "authentication",
            ),
            (
                "Trust this folder? Codex can read, edit, and run files here.\n› 1. Trust and continue\n  2. Open restricted\n  3. Quit",
                "workspace_trust",
            ),
        ] {
            assert_eq!(
                classify_work_state(screen),
                Some((WorkState::Blocked, Some(detail.into()))),
                "{screen:?}"
            );
            assert_eq!(blocker_class(detail), BlockerClass::Modal, "{detail}");
        }
        assert_eq!(
            classify_work_state_with_evidence("Approaching rate limits\n  2. Keep current model")
                .map(|(_, detail, evidence)| (detail, evidence)),
            Some((
                Some("rate_limit_model_prompt".into()),
                Some("Approaching rate limits".into())
            )),
            "the question wins over the rate-limit notice phrase inside it"
        );
    }

    #[test]
    fn transcript_notices_and_single_phrases_are_not_questions() {
        // The non-blocking history line Codex prints when an update exists.
        let history = "✨ Update available! 0.163.0 -> 0.164.0\nSee full release notes:\nhttps://github.com/openai/codex/releases/latest\nRun codex update to update.";
        assert_ne!(
            classify_work_state(history).map(|(state, _)| state),
            Some(WorkState::Blocked)
        );
        for prose in [
            "The docs say: pick Try new model to switch.",
            "Sign in with ChatGPT to use /usage.",
            "Trust this folder? is the new trust heading.",
            "Update available! is printed after a release.",
        ] {
            assert_eq!(classify_work_state(prose), None, "{prose:?}");
        }
        assert_eq!(
            classify_work_state("You are approaching rate limits."),
            Some((WorkState::Blocked, Some("rate_limit".into()))),
            "without its choices the phrase stays the passing notice"
        );
        assert_eq!(blocker_class("rate_limit"), BlockerClass::Notice);
    }

    fn screen_blocker(text: &str) -> Option<(WorkState, Option<String>)> {
        let mut viewport = TerminalViewport::default();
        for character in format!("\u{1b}[2J\u{1b}[H{}", text.replace('\n', "\r\n")).chars() {
            viewport.observe_character(character);
        }
        viewport
            .trusted_screen()
            .as_ref()
            .and_then(classify_current_screen_blocker)
            .map(Classified::into_pair)
    }

    // Verbatim snapshot bodies (frontmatter excluded) from openai/codex tags
    // rust-v0.162.1 AND rust-v0.163.0, codex-rs/tui/src/chatwidget/snapshots/:
    // codex_tui__chatwidget__tests__plan_implementation_popup.snap
    // codex_tui__chatwidget__tests__plan_implementation_popup_context_usage.snap
    // All four complete snapshot files are byte-identical across those tags;
    // 0.163.0 is the operator's launched standalone, 0.162.1 the npm install.
    // The new SelectedBelowWhenNarrow renderer mode in 0.163.0 does not apply
    // to this plan picker: picker() uses HideWhenNarrow with a 24-cell minimum
    // description width. Wide layouts use columns; narrow ones hide descriptions.
    // Disabled gutters are identical in both versions.
    // Preserve indentation and description columns. These are upstream render
    // snapshots, not local PTY captures or a substitute for the Send walk.
    const PLAN_MENU_V01621: &str = "  Implement this plan?


› 1. Yes, implement this plan          Switch to Default and start coding
  2. Yes, clear context and implement  Fresh thread with this plan
  3. No, stay in Plan mode             Continue planning with the model

  enter select · esc back
";
    const PLAN_MENU_CONTEXT_V01621: &str = "  Implement this plan?


› 1. Yes, implement this plan          Switch to Default and start coding
  2. Yes, clear context and implement  Start a fresh thread (current context:
                                       89% used)
  3. No, stay in Plan mode             Continue planning with the model

  enter select · esc back
";

    // Same pinned snapshot directories, respectively model_selection_popup.snap
    // and model_reasoning_selection_popup.snap (same filename prefix as above).
    const MODEL_MENU_V01621: &str = "  Select Model and Effort


  1. GPT-6.1-Sol        Latest workhorse model for coding and everyday work.
  2. GPT-6-Astra        Frontier intelligence for the most demanding work.
  3. GPT-6-Sol          Previous generation workhorse model.
  4. GPT-6-Luna         Fast and affordable model for easier tasks.
  5. GPT-5.6-Sol        Older generation workhorse model.
  6. GPT-5.6-Terra      Older balanced model for straightforward work.
  7. GPT-5.6-Luna       Older fast and efficient model.
› 8. GPT-5.5 (current)  Legacy coding model.

  enter select · esc back
";
    const EFFORT_MENU_V01621: &str = "  Select Reasoning Level for GPT-5.5


  1. Low               Fast responses with lighter reasoning
  2. Medium (default)  Balances speed and reasoning depth for everyday tasks
› 3. High (current)    Greater reasoning depth for complex problems
  4. Extra high        Extra high reasoning depth for complex problems
  5. Persistent        Continue working until put to sleep
  6. More reasoning…   Max and Ultra consume usage limits faster

  enter default · s session · esc back
";

    fn plan_menu_frame() -> String {
        format!("\u{1b}[?25l\u{1b}[2J\u{1b}[H{}\u{1b}[?25h", PLAN_MENU_V01621.replace('\n', "\r\n"))
    }

    #[test]
    fn upstream_plan_column_snapshots_and_disabled_rows_are_modal() {
        for menu in [PLAN_MENU_V01621, PLAN_MENU_CONTEXT_V01621] {
            assert_eq!(screen_blocker(menu), Some((WorkState::Blocked, Some("plan_mode_prompt".into()))));
            let frame = format!("\u{1b}[2J\u{1b}[H{}", menu.replace('\n', "\r\n"));
            for split in 0..=frame.len() {
                if !frame.is_char_boundary(split) { continue; }
                let mut tracker = WorkStateTracker::default();
                tracker.observe_output(&frame[..split]);
                tracker.observe_output(&frame[split..]);
                assert_eq!(tracker.blocked_detail(), Some("plan_mode_prompt"), "split {split}");
            }
        }
        // Renderer-derived variants, not upstream snapshots: list_selection_view
        // gives disabled rows five gutter spaces and numbers only enabled rows;
        // selection_row_layout appends (disabled) and the reason in the right column.
        // Five gutter cells + 32 label cells + 11 suffix cells + 2 gap cells
        // put every description/continuation at column 50 (zero-based).
        for menu in [
            "  Implement this plan?\n\n› 1. Yes, implement this plan                     Switch to Default and start coding\n     Yes, clear context and implement (disabled)  Fresh thread with this plan (disabled: No\n                                                  approved plan available)\n  2. No, stay in Plan mode                        Continue planning with the model\n\n  enter select · esc back",
            "  Implement this plan?\n\n     Yes, implement this plan (disabled)          Switch to Default and start coding\n                                                  (disabled: Default mode unavailable)\n     Yes, clear context and implement (disabled)  Fresh thread with this plan (disabled:\n                                                  Default mode unavailable)\n› 1. No, stay in Plan mode                        Continue planning with the model\n\n  enter select · esc back",
        ] {
            assert_eq!(screen_blocker(menu), Some((WorkState::Blocked, Some("plan_mode_prompt".into()))), "{menu}");
        }
    }

    #[test]
    fn selected_slash_model_and_effort_rows_are_not_plan_implementation_choosers() {
        // command_popup.rs selection shape and slash_command.rs's exact Plan
        // description at the same tag; this one is source-derived, not a snapshot.
        let slash = "› /plan  switch to Plan mode\n  /model  choose what model and reasoning effort to use";
        for menu in [slash, MODEL_MENU_V01621, EFFORT_MENU_V01621] {
            assert_eq!(screen_blocker(menu), None, "{menu}");
            let mut working = WorkStateTracker::default();
            working.observe_output("Working 2s");
            let frame = format!("\u{1b}[2J\u{1b}[H{}", menu.replace('\n', "\r\n"));
            assert_ne!(working.observe_output(&frame).map(|pair| pair.0), Some(WorkState::Blocked));
            assert!(!working.is_blocked());
        }
        assert_eq!(screen_blocker(&format!("{PLAN_MENU_CONTEXT_V01621}\nWorking 12s")), None);
    }

    #[test]
    fn plan_chooser_needs_the_current_selected_menu_not_raw_phrases() {
        for menu in [
            "Implement this plan?\n› 1. Yes, implement this plan\n  2. No, stay in Plan mode",
            "Implement this plan?\n  1. Yes, implement this plan\n› 2. No, stay in Plan mode",
            "Implement this plan?\n› 1. Yes, clear context and implement\n  2. No, stay in Plan mode",
            "Implement this plan?\n  1. Yes, implement this plan\n› 2. Yes, clear context and implement\n  3. No, stay in Plan mode",
        ] {
            assert_eq!(classify_work_state(menu), None, "raw text: {menu:?}");
            assert_eq!(
                screen_blocker(menu),
                Some((WorkState::Blocked, Some("plan_mode_prompt".into()))),
                "composed menu: {menu:?}"
            );
        }
        for not_menu in [
            "Implement this plan?",
            "Implement this plan?\n› 1. Yes, implement this plan",
            "Implement this plan?\n  1. Yes, implement this plan\n  2. No, stay in Plan mode",
            "Implement this plan?\n› 1. Yes, implement this plan\n› 2. No, stay in Plan mode",
            "Implement this plan?\n> › 1. Yes, implement this plan\n> 2. No, stay in Plan mode",
            "Implement this plan?\n+ › 1. Yes, implement this plan\n+ 2. No, stay in Plan mode",
            "Implement this plan?\n› 1. Yes, implement this plan\n  2. No, stay in Plan mode\nWorking 12s · esc to interrupt",
            "Implement this plan?\n› 1. Yes, implement this plan\n  2. No, stay in Plan mode\n• Here is why those options appear.",
        ] {
            assert_eq!(screen_blocker(not_menu), None, "{not_menu:?}");
        }
    }

    #[test]
    fn plan_discussion_and_footer_never_block_an_active_worker() {
        for discussion in [
            "• Create a plan, choose a plan, press [tab], or use shift+tab for Plan mode.\r\n",
            "• The chooser says Implement this plan? with Yes, implement this plan and No, stay in Plan mode.\r\n",
            "• The other option is Yes, clear context and implement; No, stay in Plan mode keeps planning.\r\n",
            "Implement this plan?\r\n  1. Yes, implement this plan\r\n  2. No, stay in Plan mode\r\n",
        ] {
            for split in 0..=discussion.len() {
                if !discussion.is_char_boundary(split) {
                    continue;
                }
                let mut tracker = WorkStateTracker::default();
                assert_eq!(
                    tracker.observe_output("\u{1b}[2J\u{1b}[HWorking 2s\r\n").map(|pair| pair.0),
                    Some(WorkState::Thinking)
                );
                tracker.acknowledge_pending();
                for chunk in [&discussion[..split], &discussion[split..]] {
                    assert_ne!(
                        tracker.observe_output(chunk).map(|pair| pair.0),
                        Some(WorkState::Blocked),
                        "split {split}: {discussion:?}"
                    );
                    assert!(!tracker.is_blocked(), "split {split}: {discussion:?}");
                }
                assert!(!tracker.has_pending_classification());
            }
        }
    }

    #[test]
    fn plan_chooser_is_unknown_until_composed_then_latched_until_clean_prompt() {
        let mut partial = WorkStateTracker::default();
        assert_eq!(
            partial.observe_output("\u{1b}[?25l\u{1b}[2J\u{1b}[HImplement this plan?\r\n› 1. Yes, implement this plan\r\n"),
            None,
            "partial startup paint has no work-state observation"
        );
        assert!(!partial.has_pending_classification());
        assert!(!partial.is_blocked());
        assert_eq!(
            partial.observe_output("  2. No, stay in Plan mode\r\n\u{1b}[?25h"),
            Some((WorkState::Blocked, Some("plan_mode_prompt".into())))
        );
        assert_eq!(partial.blocked_evidence(), Some("Implement this plan?"));
        partial.acknowledge_pending();
        partial.resize(120, 40);
        assert_eq!(partial.observe_output("Working 2s"), None);
        assert!(
            partial.is_blocked(),
            "an untrusted partial repaint cannot clear the selector"
        );
        let old_menu =
            "Implement this plan?\r\n› 1. Yes, implement this plan\r\n  2. No, stay in Plan mode";
        assert_eq!(
            partial.observe_output(&current_prompt_screen(old_menu)),
            Some((WorkState::Idle, None)),
            "the clean composer wins over a menu retained in the transcript"
        );
        assert!(!partial.is_blocked());

        let frame = plan_menu_frame();
        let mut working = WorkStateTracker::default();
        assert_eq!(
            working.observe_output("Working 2s").map(|pair| pair.0),
            Some(WorkState::Thinking)
        );
        assert_eq!(
            working.observe_output(&frame),
            Some((WorkState::Blocked, Some("plan_mode_prompt".into())))
        );
        working.acknowledge_pending();
        assert_eq!(working.observe_output("Working 3s"), None);
        assert!(working.is_blocked(), "activity alone cannot answer a real chooser");
        for split in 0..=frame.len() {
            if !frame.is_char_boundary(split) {
                continue;
            }
            let mut tracker = WorkStateTracker::default();
            tracker.observe_output(&frame[..split]);
            tracker.observe_output(&frame[split..]);
            assert_eq!(
                tracker.blocked_detail(),
                Some("plan_mode_prompt"),
                "split {split}"
            );
        }
        let mut tracker = WorkStateTracker::default();
        for character in frame.chars() {
            tracker.observe_output(&character.to_string());
        }
        assert_eq!(tracker.blocked_detail(), Some("plan_mode_prompt"));
    }

    #[test]
    fn a_question_on_screen_needs_its_numbered_menu() {
        for (screen, detail) in [
            (
                "  Update available! 0.163.0 -> 0.164.0\n\n\u{203a} 1. Update now (runs `codex update`)\n  2. Skip\n  3. Skip until next version\n\n  enter continue \u{b7} esc skip",
                "update_prompt",
            ),
            (
                "Codex just got an upgrade.\n\u{203a} 1. Try new model\n  2. Use existing model",
                "model_migration_prompt",
            ),
            (
                "Approaching rate limits\n\u{203a} 1. Switch to gpt-6-mini\n  2. Keep current model",
                "rate_limit_model_prompt",
            ),
            (
                "Sign in with ChatGPT to use Codex\n\u{203a} 1. Sign in with ChatGPT\n  2. Sign in with Device Code",
                "authentication",
            ),
            (
                "Trust this folder? Codex can read, edit, and run files here.\n\u{203a} 1. Trust and continue\n  2. Open restricted\n  3. Quit",
                "workspace_trust",
            ),
        ] {
            assert_eq!(
                screen_blocker(screen),
                Some((WorkState::Blocked, Some(detail.into()))),
                "{screen:?}"
            );
        }
        // Both phrases in the transcript of a working Codex: prose, a diff of
        // these fixtures, a quoted menu line. None is an option row.
        for transcript in [
            "\u{2022} Codex printed Update available! and offered Skip until next version.\n  Working (12s \u{2022} esc to interrupt)",
            "+        \"  3. Skip until next version\\r\\n\",\n+        \"  \u{2728} Update available! 0.163.0 -> 0.164.0\\r\\n\",",
            "\u{2022} The menu reads Try new model / Use existing model.",
            "\u{2022} Trust this folder? asks you to Trust and continue.",
        ] {
            assert_eq!(screen_blocker(transcript), None, "{transcript:?}");
        }
    }

    #[test]
    fn a_transcript_quoting_a_question_above_a_clean_prompt_is_idle() {
        let transcript = concat!(
            "\u{2022} Codex printed Update available! 0.163.0 -> 0.164.0 and\r\n",
            "  3. Skip until next version; also Try new model / Use existing model.\r\n",
        );
        let mut tracker = WorkStateTracker::default();
        assert_eq!(
            tracker.observe_output(&current_prompt_screen(transcript)),
            Some((WorkState::Idle, None))
        );
        assert!(!tracker.is_blocked());

        // Fresh output of the quote may latch for a moment (fail closed);
        // the next clean prompt resolves it although the quote stays visible.
        let mut quoted = WorkStateTracker::default();
        assert_eq!(
            quoted.observe_output(transcript),
            Some((WorkState::Blocked, Some("update_prompt".into())))
        );
        quoted.acknowledge_pending();
        assert_eq!(
            quoted.observe_output(&current_prompt_screen(transcript)),
            Some((WorkState::Idle, None))
        );
        assert!(!quoted.is_blocked());
    }

    #[test]
    fn update_prompt_stays_latched_until_codex_paints_its_prompt() {
        let mut tracker = WorkStateTracker::default();
        assert_eq!(
            tracker.observe_output(UPDATE_PROMPT_V0163),
            Some((WorkState::Blocked, Some("update_prompt".into())))
        );
        assert_eq!(tracker.blocked_class(), Some(BlockerClass::Modal));
        assert_eq!(tracker.blocked_evidence(), Some("Update available!"));
        tracker.acknowledge_pending();
        assert_eq!(tracker.observe_output("unrelated repaint"), None);
        assert_eq!(tracker.observe_output("Working 1s"), None);
        assert!(tracker.is_blocked(), "only the screen resolves a modal");
        assert_eq!(
            tracker.observe_output(&live_clean_prompt_frame("skipped the update")),
            Some((WorkState::Idle, None))
        );
        assert!(!tracker.is_blocked());

        for split in 0..=UPDATE_PROMPT_V0163.len() {
            if !UPDATE_PROMPT_V0163.is_char_boundary(split) {
                continue;
            }
            let mut tracker = WorkStateTracker::default();
            let first = tracker.observe_output(&UPDATE_PROMPT_V0163[..split]);
            let second = tracker.observe_output(&UPDATE_PROMPT_V0163[split..]);
            assert!(
                [first, second].iter().any(
                    |observed| matches!(observed, Some((WorkState::Blocked, Some(detail))) if detail == "update_prompt")
                ),
                "split {split} did not detect the update prompt"
            );
        }
    }

    /// One recorded ConPTY session (v3 W1 captures, codex-cli 0.164.0 standalone,
    /// 120x40) replayed chunk by chunk as the supervisor feeds it: begin_run,
    /// the pane size, then every read in order. Returns each classification
    /// with the capture time of the chunk that produced it.
    fn replay_capture(fixture: &str) -> Vec<(f64, WorkState, Option<String>)> {
        let doc: serde_json::Value = serde_json::from_str(fixture).expect("capture fixture is JSON");
        let mut tracker = WorkStateTracker::default();
        tracker.begin_run();
        tracker.resize(
            doc["cols"].as_u64().unwrap() as u16,
            doc["rows"].as_u64().unwrap() as u16,
        );
        let mut observed = Vec::new();
        for chunk in doc["chunks"].as_array().unwrap() {
            if let Some((state, detail)) = tracker.observe_output(chunk["data"].as_str().unwrap()) {
                observed.push((chunk["t"].as_f64().unwrap(), state, detail));
            }
        }
        observed
    }

    fn first_at_or_after(
        observed: &[(f64, WorkState, Option<String>)],
        time: f64,
        state: WorkState,
    ) -> Option<f64> {
        observed
            .iter()
            .find(|(at, observed_state, _)| *at >= time && *observed_state == state)
            .map(|(at, _, _)| *at)
    }

    /// v3 W1 R3 (consistent with the 1066 / C3 pattern; not proved for those
    /// historical screens): after its first turn, 0.164 names the thread and
    /// appends the title to the footer (`model · cwd · title`).
    /// Splitting the footer on its last separator read the title as the
    /// working directory, so a titled or resumed thread was never idle again.
    /// The capture: startup prompt, one typed prompt at 10 s, `◦ Working (3s •
    /// esc to interrupt)`, `• Explored / └ Read …`, the answer, then the
    /// settled prompt above the titled footer.
    #[test]
    fn v3_codex_0164_titled_footer_is_idle_after_the_first_turn() {
        let observed = replay_capture(include_str!("fixtures/codex-0164-fresh-turn-titled-footer.json"));
        assert!(
            first_at_or_after(&observed, 0.0, WorkState::Idle).is_some_and(|at| at < 10.0),
            "the fresh prompt is idle before the typed prompt: {observed:?}"
        );
        assert!(
            first_at_or_after(&observed, 10.0, WorkState::Thinking).is_some(),
            "the 0.164 `Working (Ns •` timer reads as thinking: {observed:?}"
        );
        assert!(
            first_at_or_after(&observed, 10.0, WorkState::ToolCall).is_some(),
            "the 0.164 `• Explored / └ Read` activity reads as a tool call: {observed:?}"
        );
        let last = observed.last().expect("classifications");
        assert_eq!(
            (last.1, last.2.as_deref()),
            (WorkState::Idle, None),
            "after the turn the titled footer is a clean prompt: {observed:?}"
        );
        assert!(last.0 > 18.0, "the final idle comes after the turn: {observed:?}");
        assert!(
            observed.iter().all(|(_, state, _)| *state != WorkState::Blocked),
            "nothing in an ordinary turn blocks: {observed:?}"
        );
    }

    /// v3 M1 finding: a Codex busy in one long turn was read Idle mid-turn and
    /// woken (candidate 39761da, busy journey: one turn 15:27:20–15:28:19,
    /// Idle at :30.6, :49.1, :56.9). The capture: a 15 s command typed at
    /// 10 s; 0.164 extends the status line to `• Working (4s • esc to
    /// interrupt) · 1 background terminal running · /ps to view · /stop to
    /// close` and titles the footer. The whole turn, from the submit to the
    /// `Worked for` summary, never reads Idle.
    #[test]
    fn v3_codex_0164_a_long_command_with_a_background_terminal_is_never_idle_mid_turn() {
        let observed = replay_capture(include_str!("fixtures/codex-0164-long-command-background-terminal.json"));
        assert!(first_at_or_after(&observed, 0.0, WorkState::Idle).is_some_and(|at| at < 10.0),
            "the fresh prompt is idle first: {observed:?}");
        let turn: Vec<_> = observed.iter().filter(|(at, _, _)| (11.3..=34.9).contains(at)).collect();
        assert!(!turn.is_empty(), "the turn is classified: {observed:?}");
        assert!(turn.iter().all(|(_, state, _)| *state != WorkState::Idle),
            "never Idle while the command runs or the answer streams: {turn:?}");
        let last = observed.last().expect("classifications");
        assert_eq!((last.1, last.2.as_deref()), (WorkState::Idle, None), "idle after the turn: {observed:?}");
        assert!(last.0 > 34.9, "the final idle comes after the turn: {observed:?}");
    }

    /// A replay that splits every recorded chunk into `piece`-byte reads (on
    /// char boundaries): a pane's reads never match the capture's, so a
    /// mid-repaint state must not be read as a settled screen at any split.
    fn replay_capture_split(fixture: &str, piece: usize) -> Vec<(f64, WorkState, Option<String>)> {
        let doc: serde_json::Value = serde_json::from_str(fixture).expect("capture fixture is JSON");
        let mut tracker = WorkStateTracker::default();
        tracker.begin_run();
        tracker.resize(doc["cols"].as_u64().unwrap() as u16, doc["rows"].as_u64().unwrap() as u16);
        let mut observed = Vec::new();
        for chunk in doc["chunks"].as_array().unwrap() {
            let data = chunk["data"].as_str().unwrap();
            let mut start = 0;
            while start < data.len() {
                let mut end = (start + piece).min(data.len());
                while !data.is_char_boundary(end) { end += 1; }
                if let Some((state, detail)) = tracker.observe_output(&data[start..end]) {
                    observed.push((chunk["t"].as_f64().unwrap(), state, detail));
                }
                start = end;
            }
        }
        observed
    }

    /// v3 M1 re-run on 4250194: a Codex waiting on a 40 s background command
    /// was still read Idle mid-turn. The capture: the same command typed at
    /// 10 s; the turn ends with `Worked for 50s` at about 61 s. Replayed as
    /// recorded and split into small reads, it never reads Idle mid-turn.
    #[test]
    fn v3_codex_0164_a_forty_second_command_is_never_idle_mid_turn_at_any_read_split() {
        let fixture = include_str!("fixtures/codex-0164-forty-second-command.json");
        for piece in [usize::MAX, 4096, 512, 64, 7, 1] {
            let observed = if piece == usize::MAX { replay_capture(fixture) } else { replay_capture_split(fixture, piece) };
            let mid_turn: Vec<_> = observed.iter()
                .filter(|(at, state, _)| (11.3..=60.8).contains(at) && *state == WorkState::Idle).collect();
            assert!(mid_turn.is_empty(), "read split {piece}: Idle mid-turn {mid_turn:?}");
            assert_eq!(observed.last().map(|(_, state, detail)| (*state, detail.clone())), Some((WorkState::Idle, None)),
                "read split {piece}: idle after the turn");
        }
    }

    /// v3 finding (a), 24 Oct: a plain `codex resume <id>` after a candidate
    /// restart stayed unobserved for 7 minutes. The offline replay of the same
    /// launch: the resumed history, an advisory weekly-usage notice above the
    /// composer, and a warning marker in the footer. The settled screen must
    /// read as a prompt (Idle), the notice staying advisory.
    #[test]
    fn v3_codex_0164_a_resumed_thread_with_an_advisory_notice_settles_idle() {
        for fixture in [include_str!("fixtures/codex-0164-resume-with-usage-notice.json"),
            // The live pane's size (331x89).
            include_str!("fixtures/codex-0164-resume-wide-pane.json")] {
            let observed = replay_capture(fixture);
            let last = observed.last().expect("classifications");
            assert_eq!((last.1, last.2.as_deref()), (WorkState::Idle, None), "settled resume reads as a prompt: {observed:?}");
        }
    }

    /// v3 W1 R5: a notice phrase must stand as whole words. The fixtures are
    /// split so the phrase never appears contiguously in this source.
    #[test]
    fn v3_a_notice_phrase_spelled_across_two_innocent_words_is_not_a_notice() {
        for prose in [
            concat!("a sepa", "rate li", "mit exc", "eeded the budget"),
            concat!("the corpo", "rate li", "mit exce", "eded plan"),
            concat!("no ", "http 4", "035 here"),
        ] {
            assert_eq!(classify_work_state(prose), None, "{prose:?}");
            let mut tracker = WorkStateTracker::default();
            assert_eq!(tracker.observe_output(&format!("{prose}\r\n")), None);
            assert!(!tracker.is_blocked());
        }
        assert_eq!(
            classify_work_state(concat!("error: ra", "te limit ex", "ceeded: retry in 20s")).unwrap(),
            (WorkState::Blocked, Some("rate_limit".into()))
        );
    }

    #[test]
    fn v3_codex_0164_footer_segments_after_the_working_directory_are_ignored() {
        for footer in [
            r"  GPT-6-Astra medium · D:\prim1-qa\personal-repo · Show download log",
            r"  GPT-6-Astra medium · D:\prim1-qa\personal-repo                    ⚠ 1 warning · f2 to view",
            r"  gpt-5.6-sol max · ~\mywork",
        ] {
            let screen = format!(
                "\u{1b}[?25l\u{1b}[2J\u{1b}[H\u{1b}[5;1H› Ask Codex to do anything\u{1b}[7;1H{footer}\u{1b}[5;3H\u{1b}[?25h"
            );
            let mut tracker = WorkStateTracker::default();
            assert_eq!(tracker.observe_output(&screen), Some((WorkState::Idle, None)), "{footer}");
        }
        // Prose with a separator is still not a footer, and a path-like later
        // segment (a thread title) does not stand in for the directory.
        for footer in ["  plan · next step · later", r"  gpt-6 · not-a-cwd · D:\example"] {
            let screen = format!(
                "\u{1b}[?25l\u{1b}[2J\u{1b}[H\u{1b}[5;1H› Ask Codex to do anything\u{1b}[7;1H{footer}\u{1b}[5;3H\u{1b}[?25h"
            );
            assert_eq!(WorkStateTracker::default().observe_output(&screen), None, "{footer}");
        }
    }

    /// 0.164 asks before running hooks it has not seen ("Hooks need review",
    /// three numbered choices). It is a human trust decision: a modal PRIM-1
    /// never answers, recognised on screen with its menu. After the operator
    /// skipped it, the footer carries `⚠ 1 warning · f2 to view` and the
    /// prompt is still idle.
    #[test]
    fn v3_codex_0164_hooks_review_is_a_modal_and_the_warning_footer_is_idle() {
        let observed = replay_capture(include_str!("fixtures/codex-0164-hooks-review-then-warning-footer.json"));
        let blocked = observed
            .iter()
            .find(|(_, state, _)| *state == WorkState::Blocked)
            .unwrap_or_else(|| panic!("the hooks review blocks: {observed:?}"));
        assert_eq!(blocked.2.as_deref(), Some("hooks_review"), "{observed:?}");
        assert_eq!(blocker_class("hooks_review"), BlockerClass::Modal);
        let last = observed.last().unwrap();
        assert_eq!((last.1, last.2.as_deref()), (WorkState::Idle, None), "{observed:?}");
    }

    fn title_after(chunks: &[&str]) -> Option<TitleRunState> {
        let mut tracker = WorkStateTracker::default();
        tracker.begin_run();
        for chunk in chunks { tracker.observe_output(chunk); }
        tracker.title_run_state()
    }

    /// v3 W2 #2: the real 0.164 capture (titles Ready, Ready | <thread>,
    /// Starting | <thread>, Ready | <thread>) ends Ready, in whole chunks and
    /// split into pieces of every small size.
    #[test]
    fn v3_codex_0164_title_run_state_follows_the_captured_frames_across_any_split() {
        let fixture = include_str!("fixtures/codex-0164-hooks-review-then-warning-footer.json");
        let doc: serde_json::Value = serde_json::from_str(fixture).unwrap();
        let data: String = doc["chunks"].as_array().unwrap().iter().map(|chunk| chunk["data"].as_str().unwrap()).collect();
        assert_eq!(title_after(&[&data]), Some(TitleRunState::Ready));
        for piece in [1, 2, 3, 7, 64] {
            let chars: Vec<char> = data.chars().collect();
            let pieces: Vec<String> = chars.chunks(piece).map(|part| part.iter().collect()).collect();
            let refs: Vec<&str> = pieces.iter().map(String::as_str).collect();
            assert_eq!(title_after(&refs), Some(TitleRunState::Ready), "pieces of {piece}");
        }
    }

    /// v3 W2 #2: only a well-formed OSC 0/2 title names a state; the thread
    /// title is never read; anything malformed, oversized or unknown clears
    /// the hint; a new run starts with none.
    #[test]
    fn v3_codex_title_hint_is_bounded_and_fails_to_no_hint() {
        assert_eq!(title_after(&["\x1b]0;Work", "ing | Ready\x07"]), Some(TitleRunState::Working), "split, and the thread title is not read");
        assert_eq!(title_after(&["\x1b]2;Thinking\x1b\\"]), Some(TitleRunState::Thinking), "OSC 2 with ST");
        assert_eq!(title_after(&["\x1b]0;Action Required | t\x07"]), Some(TitleRunState::ActionRequired));
        assert_eq!(title_after(&["\x1b]0;Working\x07", "\x1b]0;personal-repo\x07"]), None, "an unknown title clears it");
        assert_eq!(title_after(&["\x1b]0;Working\x07", "\x1b]0;Ready\x1b[0m"]), None, "an unterminated frame clears it");
        assert_eq!(title_after(&["\x1b]0;Working\x07", "\x1b]0;Ready\nmore\x07"]), None, "a control character inside clears it");
        let long = format!("\x1b]0;Ready | {}\x07", "x".repeat(400));
        assert_eq!(title_after(&["\x1b]0;Working\x07", &long]), None, "an oversized title clears it");
        assert_eq!(title_after(&["\x1b]0;Working\x07", "\x1b]7;file://host/dir\x07"]), Some(TitleRunState::Working),
            "another OSC is not a title");
        assert_eq!(title_after(&["Working | Ready\r\n"]), None, "plain text is never a title");
        assert_eq!(title_after(&["\x1b]0;working\x07"]), None, "exact names only");
        let mut tracker = WorkStateTracker::default();
        tracker.observe_output("\x1b]0;Working\x07");
        assert!(tracker.title_busy().is_some_and(|(state, _)| state == TitleRunState::Working));
        tracker.observe_output("\x1b]0;Ready\x07");
        assert_eq!(tracker.title_busy(), None, "a ready title ends the busy time");
        tracker.observe_output("\x1b]0;Working\x07");
        tracker.begin_run();
        assert_eq!(tracker.title_run_state(), None, "a new run starts without a hint");
        assert_eq!(tracker.title_busy(), None);
        assert!(TitleRunState::Working.is_busy() && TitleRunState::Thinking.is_busy() && TitleRunState::ActionRequired.is_busy());
        assert!(!TitleRunState::Ready.is_busy() && !TitleRunState::Starting.is_busy(), "Starting is seen on a clean composer");
    }

    /// v3 W2 (Reviewer 68dcd58e): 0.164 activity text never resolves a modal
    /// on screen, and replayed activity in a run with no state yet reads as a
    /// tool call (the supervisor then counts the run as observed; a modal
    /// painted later is still latched and holds).
    #[test]
    fn v3_codex_0164_activity_never_clears_a_modal_and_replayed_activity_is_a_tool_call() {
        let mut tracker = WorkStateTracker::default();
        assert_eq!(
            tracker.observe_output(UPDATE_PROMPT_V0163),
            Some((WorkState::Blocked, Some("update_prompt".into())))
        );
        tracker.acknowledge_pending();
        for activity in ["• Ran Get-ChildItem\r\n", "• Explored\r\n  └ Read notes.txt\r\n"] {
            assert_eq!(tracker.observe_output(activity), None, "{activity:?}");
            assert!(tracker.is_blocked(), "only the screen resolves a modal: {activity:?}");
        }

        let mut replay = WorkStateTracker::default();
        replay.begin_run();
        assert_eq!(
            replay.observe_output("• Explored\r\n  └ Read notes.txt\r\n"),
            Some((WorkState::ToolCall, None))
        );
        assert_eq!(
            replay.observe_output(UPDATE_PROMPT_V0163),
            Some((WorkState::Blocked, Some("update_prompt".into()))),
            "a modal after replayed activity still latches"
        );
        assert_eq!(classify_work_state("└ Search src"), None, "uncaptured forms are not activity");
    }

    /// 0.164's reworded trust screen ("Trust this folder?", `1. Trust and
    /// continue` / `2. Quit`) stays a modal block and never reads as idle.
    #[test]
    fn v3_codex_0164_trust_folder_screen_is_a_modal_and_never_idle() {
        let observed = replay_capture(include_str!("fixtures/codex-0164-trust-folder-modal.json"));
        assert!(
            observed
                .iter()
                .any(|(_, state, detail)| *state == WorkState::Blocked && detail.as_deref() == Some("workspace_trust")),
            "{observed:?}"
        );
        assert!(observed.iter().all(|(_, state, _)| *state != WorkState::Idle), "{observed:?}");
    }
}
