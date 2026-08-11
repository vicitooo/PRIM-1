use std::path::Path;

use shared_types::{LaunchSpec, LaunchSpecError, PermissionProfile, SessionDefinition, WorkState};
use unicode_width::UnicodeWidthChar as _;
use uuid::Uuid;

// Measured against Grok Build 1.0.0 (3cd0d0cbce). Unknown future text stays
// fail-closed in lifecycle Starting rather than being inferred ready.
pub const LAUNCHER_MENU_DETAIL: &str = "launcher_menu";
pub const SESSION_STARTING_DETAIL: &str = "session_starting";
pub const STARTUP_SCREEN_MAX_CELLS: usize = 64 * 1024;

const TERMINAL_CONTROL_MAX_CHARS: u16 = 128;
const DEFAULT_TERMINAL_COLS: usize = 120;
const DEFAULT_TERMINAL_ROWS: usize = 40;

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
    EscapeIntermediate {
        chars_seen: u8,
    },
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

#[derive(Debug)]
struct StartupScreen {
    cols: usize,
    rows: usize,
    cells: Vec<char>,
    continuations: Vec<bool>,
    valid: Vec<bool>,
    wrapped: Vec<bool>,
    cursor_row: usize,
    cursor_col: usize,
    wrap_pending: bool,
}

impl StartupScreen {
    fn new(cols: usize, rows: usize, trusted: bool) -> Option<Self> {
        let cell_count = cols.checked_mul(rows)?;
        if cols == 0 || rows == 0 || cell_count > STARTUP_SCREEN_MAX_CELLS {
            return None;
        }
        Some(Self {
            cols,
            rows,
            cells: vec![' '; cell_count],
            continuations: vec![false; cell_count],
            valid: vec![trusted; cell_count],
            wrapped: vec![false; rows],
            cursor_row: 0,
            cursor_col: 0,
            wrap_pending: false,
        })
    }

    fn index(&self, row: usize, col: usize) -> usize {
        row * self.cols + col
    }

    fn is_trusted(&self) -> bool {
        self.valid.iter().all(|valid| *valid)
    }

    fn invalidate(&mut self) {
        self.valid.fill(false);
    }

    fn erase_all(&mut self) {
        self.cells.fill(' ');
        self.continuations.fill(false);
        self.valid.fill(true);
        self.wrapped.fill(false);
        self.wrap_pending = false;
    }

    fn reset(&mut self) {
        self.erase_all();
        self.cursor_row = 0;
        self.cursor_col = 0;
    }

    fn set_cell(&mut self, row: usize, col: usize, character: char) {
        let index = self.index(row, col);
        if self.continuations[index] && col > 0 {
            let primary = self.index(row, col - 1);
            self.cells[primary] = ' ';
            self.continuations[primary] = false;
            self.valid[primary] = true;
        }
        if col + 1 < self.cols && self.continuations[index + 1] {
            self.cells[index + 1] = ' ';
            self.continuations[index + 1] = false;
            self.valid[index + 1] = true;
        }
        self.cells[index] = character;
        self.continuations[index] = false;
        self.valid[index] = true;
    }

    fn set_cursor(&mut self, row: usize, col: usize) {
        self.cursor_row = row.min(self.rows - 1);
        self.cursor_col = col.min(self.cols - 1);
        self.wrap_pending = false;
    }

    fn move_cursor(&mut self, row_delta: isize, col_delta: isize) {
        let row = self
            .cursor_row
            .saturating_add_signed(row_delta)
            .min(self.rows - 1);
        let col = self
            .cursor_col
            .saturating_add_signed(col_delta)
            .min(self.cols - 1);
        self.set_cursor(row, col);
    }

    fn write(&mut self, character: char, width: usize) -> bool {
        if width == 0 {
            return true;
        }
        if width > 2 || width > self.cols {
            return false;
        }
        if self.wrap_pending {
            self.cursor_col = 0;
            self.line_feed();
            self.wrapped[self.cursor_row] = true;
            self.wrap_pending = false;
        }

        if width == 2 && self.cursor_col + 1 == self.cols {
            self.cursor_col = 0;
            self.line_feed();
            self.wrapped[self.cursor_row] = true;
        }

        self.set_cell(self.cursor_row, self.cursor_col, character);
        if width == 2 {
            let continuation = self.index(self.cursor_row, self.cursor_col + 1);
            self.set_cell(self.cursor_row, self.cursor_col + 1, ' ');
            self.continuations[continuation] = true;
        }
        let last_col = self.cursor_col + width - 1;
        if last_col + 1 == self.cols {
            self.cursor_col = last_col;
            self.wrap_pending = true;
        } else {
            self.cursor_col = last_col + 1;
        }
        true
    }

    fn carriage_return(&mut self) {
        self.cursor_col = 0;
        self.wrap_pending = false;
    }

    fn line_feed(&mut self) {
        self.wrap_pending = false;
        if self.cursor_row + 1 == self.rows {
            self.scroll_up(1);
        } else {
            self.cursor_row = (self.cursor_row + 1).min(self.rows - 1);
        }
    }

    fn blank_row(&mut self, row: usize) {
        for col in 0..self.cols {
            self.set_cell(row, col, ' ');
        }
        self.wrapped[row] = false;
    }

    fn copy_row(&mut self, source: usize, destination: usize) {
        let source_start = self.index(source, 0);
        let destination_start = self.index(destination, 0);
        let cells = self.cells[source_start..source_start + self.cols].to_vec();
        let valid = self.valid[source_start..source_start + self.cols].to_vec();
        self.cells[destination_start..destination_start + self.cols].copy_from_slice(&cells);
        let continuations = self.continuations[source_start..source_start + self.cols].to_vec();
        self.continuations[destination_start..destination_start + self.cols]
            .copy_from_slice(&continuations);
        self.valid[destination_start..destination_start + self.cols].copy_from_slice(&valid);
        self.wrapped[destination] = self.wrapped[source];
    }

    fn scroll_up(&mut self, amount: usize) {
        let amount = amount.min(self.rows);
        if amount < self.rows {
            for row in 0..self.rows - amount {
                self.copy_row(row + amount, row);
            }
        }
        for row in self.rows - amount..self.rows {
            self.blank_row(row);
        }
    }

    fn erase_chars(&mut self, amount: usize) {
        let end = self.cursor_col.saturating_add(amount).min(self.cols);
        for col in self.cursor_col..end {
            self.set_cell(self.cursor_row, col, ' ');
        }
        if self.cursor_col == 0 && end == self.cols {
            self.wrapped[self.cursor_row] = false;
        }
    }

    fn erase_line(&mut self, mode: usize) -> bool {
        let range = match mode {
            0 => self.cursor_col..self.cols,
            1 => 0..self.cursor_col + 1,
            2 => 0..self.cols,
            _ => return false,
        };
        let clears_full_row = range.start == 0 && range.end == self.cols;
        for col in range {
            self.set_cell(self.cursor_row, col, ' ');
        }
        if clears_full_row {
            self.wrapped[self.cursor_row] = false;
        }
        true
    }

    fn erase_display(&mut self, mode: usize) -> bool {
        match mode {
            0 => {
                self.erase_line(0);
                for row in self.cursor_row + 1..self.rows {
                    self.blank_row(row);
                }
            }
            1 => {
                for row in 0..self.cursor_row {
                    self.blank_row(row);
                }
                self.erase_line(1);
            }
            2 | 3 => self.erase_all(),
            _ => return false,
        }
        true
    }

    fn text(&self) -> String {
        let mut output = String::with_capacity(self.cells.len() + self.rows);
        for row in 0..self.rows {
            if row > 0 && !self.wrapped[row] {
                output.push('\n');
            }
            let start = self.index(row, 0);
            let end = start + self.cols;
            let last = if row + 1 < self.rows && self.wrapped[row + 1] {
                self.cols
            } else {
                self.cells[start..end]
                    .iter()
                    .enumerate()
                    .rposition(|(col, character)| {
                        *character != ' ' || self.continuations[start + col]
                    })
                    .map_or(0, |index| index + 1)
            };
            for col in 0..last {
                if !self.continuations[start + col] {
                    output.push(self.cells[start + col]);
                }
            }
        }
        output
    }
}

/// Projects only Grok's bounded startup viewport. It has no scrollback,
/// styling, or general terminal-widget behavior.
#[derive(Debug)]
pub struct StartupTracker {
    phase: StartupPhase,
    parser: StartupParserState,
    csi_parameters: String,
    frame_active: bool,
    frame_invalid: bool,
    bracketed_paste_enabled: bool,
    screen: Option<StartupScreen>,
}

impl Default for StartupTracker {
    fn default() -> Self {
        Self {
            phase: StartupPhase::AwaitingStarting,
            parser: StartupParserState::Ground,
            csi_parameters: String::new(),
            frame_active: false,
            frame_invalid: false,
            bracketed_paste_enabled: false,
            screen: StartupScreen::new(DEFAULT_TERMINAL_COLS, DEFAULT_TERMINAL_ROWS, true),
        }
    }
}

impl StartupTracker {
    pub fn begin_run(&mut self) {
        let (cols, rows) = self
            .screen
            .as_ref()
            .map_or((DEFAULT_TERMINAL_COLS, DEFAULT_TERMINAL_ROWS), |screen| {
                (screen.cols, screen.rows)
            });
        *self = Self::default();
        self.screen = StartupScreen::new(cols, rows, true);
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if self.frame_active {
            self.frame_invalid = true;
        }
        self.screen = StartupScreen::new(usize::from(cols), usize::from(rows), false);
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
                } else if Self::is_c0_executable_or_del(character) {
                    self.execute_c0(character);
                } else {
                    self.push_visible(character);
                }
                StartupProgress::None
            }
            StartupParserState::Escape => {
                if Self::is_c0_executable_or_del(character) {
                    self.execute_c0(character);
                    return StartupProgress::None;
                }
                self.observe_escape(character);
                StartupProgress::None
            }
            StartupParserState::EscapeIntermediate { mut chars_seen } => {
                if Self::is_c0_executable_or_del(character) {
                    self.execute_c0(character);
                    return StartupProgress::None;
                }
                if ('\u{20}'..='\u{2f}').contains(&character) {
                    chars_seen = chars_seen.saturating_add(1);
                    if chars_seen > 4 {
                        self.invalidate_projection();
                        self.parser = StartupParserState::Ground;
                    } else {
                        self.parser = StartupParserState::EscapeIntermediate { chars_seen };
                    }
                } else {
                    self.invalidate_projection();
                    self.parser = StartupParserState::Ground;
                }
                StartupProgress::None
            }
            StartupParserState::Csi { mut chars_seen } => {
                chars_seen = chars_seen.saturating_add(1);
                if chars_seen > TERMINAL_CONTROL_MAX_CHARS {
                    self.invalidate_projection();
                    self.csi_parameters.clear();
                    self.parser = StartupParserState::CsiDiscard;
                    self.observe_csi_discard(character);
                    return StartupProgress::None;
                }
                if Self::is_c0_executable_or_del(character) {
                    self.execute_c0(character);
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
                    self.invalidate_projection();
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
            '\u{0018}' | '\u{001a}' | '\u{009c}' => StartupParserState::Ground,
            '\u{0084}' => {
                if let Some(screen) = self.screen.as_mut() {
                    screen.line_feed();
                }
                StartupParserState::Ground
            }
            '\u{0085}' => {
                if let Some(screen) = self.screen.as_mut() {
                    screen.carriage_return();
                    screen.line_feed();
                }
                StartupParserState::Ground
            }
            '\u{008d}' => {
                self.invalidate_projection();
                StartupParserState::Ground
            }
            '\u{009b}' => {
                self.csi_parameters.clear();
                StartupParserState::Csi { chars_seen: 0 }
            }
            '\u{009d}' => Self::terminal_string(TerminalStringKind::Osc),
            '\u{0090}' => Self::terminal_string(TerminalStringKind::Dcs),
            '\u{0098}' => Self::terminal_string(TerminalStringKind::Sos),
            '\u{009e}' => Self::terminal_string(TerminalStringKind::Pm),
            '\u{009f}' => Self::terminal_string(TerminalStringKind::Apc),
            '\u{0080}'..='\u{0083}'
            | '\u{0086}'..='\u{008c}'
            | '\u{008e}'..='\u{008f}'
            | '\u{0091}'..='\u{0097}'
            | '\u{0099}'..='\u{009a}' => {
                self.invalidate_projection();
                StartupParserState::Ground
            }
            _ => return false,
        };
        true
    }

    fn execute_c0(&mut self, character: char) {
        if matches!(character, '\u{0008}' | '\u{0009}') {
            self.invalidate_projection();
            return;
        }
        let Some(screen) = self.screen.as_mut() else {
            return;
        };
        match character {
            '\u{000a}'..='\u{000c}' => screen.line_feed(),
            '\u{000d}' => screen.carriage_return(),
            _ => {}
        }
    }

    fn observe_escape(&mut self, character: char) {
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
            'D' => {
                if let Some(screen) = self.screen.as_mut() {
                    screen.line_feed();
                }
                StartupParserState::Ground
            }
            'E' => {
                if let Some(screen) = self.screen.as_mut() {
                    screen.carriage_return();
                    screen.line_feed();
                }
                StartupParserState::Ground
            }
            'c' => {
                if let Some(screen) = self.screen.as_mut() {
                    screen.reset();
                }
                self.bracketed_paste_enabled = false;
                StartupParserState::Ground
            }
            '\\' | '=' | '>' => StartupParserState::Ground,
            '\u{20}'..='\u{2f}' => StartupParserState::EscapeIntermediate { chars_seen: 1 },
            '\u{001b}' => StartupParserState::Escape,
            _ => {
                self.invalidate_projection();
                StartupParserState::Ground
            }
        };
    }

    fn observe_csi_discard(&mut self, character: char) {
        if Self::is_c0_executable_or_del(character) {
            self.execute_c0(character);
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
                        self.invalidate_projection();
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
        if matches!(final_character, 'h' | 'l') && parameters.starts_with('?') {
            return self.finish_private_mode(parameters, final_character == 'h');
        }

        if final_character == 'm' {
            return StartupProgress::None;
        }

        let Some(values) = Self::numeric_parameters(parameters) else {
            self.invalidate_projection();
            return StartupProgress::None;
        };
        let first = Self::parameter(&values, 0, 1);

        match final_character {
            'H' | 'f' if values.len() <= 2 => {
                let row = Self::parameter(&values, 0, 1).saturating_sub(1);
                let col = Self::parameter(&values, 1, 1).saturating_sub(1);
                if let Some(screen) = self.screen.as_mut() {
                    screen.set_cursor(row, col);
                }
            }
            'A' => self.move_screen_cursor(-(first as isize), 0),
            'B' | 'e' => self.move_screen_cursor(first as isize, 0),
            'C' | 'a' => self.move_screen_cursor(0, first as isize),
            'D' | 'j' => self.move_screen_cursor(0, -(first as isize)),
            'E' => {
                self.move_screen_cursor(first as isize, 0);
                if let Some(screen) = self.screen.as_mut() {
                    screen.carriage_return();
                }
            }
            'F' => {
                self.move_screen_cursor(-(first as isize), 0);
                if let Some(screen) = self.screen.as_mut() {
                    screen.carriage_return();
                }
            }
            'G' | '`' if values.len() <= 1 => {
                if let Some(screen) = self.screen.as_mut() {
                    screen.set_cursor(screen.cursor_row, first.saturating_sub(1));
                }
            }
            'd' if values.len() <= 1 => {
                if let Some(screen) = self.screen.as_mut() {
                    screen.set_cursor(first.saturating_sub(1), screen.cursor_col);
                }
            }
            'X' if values.len() <= 1 => {
                if let Some(screen) = self.screen.as_mut() {
                    screen.erase_chars(first);
                }
            }
            'K' if values.len() <= 1 => {
                let mode = values.first().copied().unwrap_or(0);
                if !self
                    .screen
                    .as_mut()
                    .is_some_and(|screen| screen.erase_line(mode))
                {
                    self.invalidate_projection();
                }
            }
            'J' if values.len() <= 1 => {
                let mode = values.first().copied().unwrap_or(0);
                if !self
                    .screen
                    .as_mut()
                    .is_some_and(|screen| screen.erase_display(mode))
                {
                    self.invalidate_projection();
                }
            }
            'n' => {}
            _ => self.invalidate_projection(),
        }
        StartupProgress::None
    }

    fn finish_private_mode(&mut self, parameters: &str, enabled: bool) -> StartupProgress {
        let Some(modes) = parameters
            .strip_prefix('?')
            .and_then(Self::numeric_parameters)
            .filter(|modes| !modes.is_empty() && modes.iter().all(|mode| *mode != 0))
        else {
            self.invalidate_projection();
            return StartupProgress::None;
        };

        let mut cursor_visibility = None;
        for mode in modes {
            match mode {
                25 => cursor_visibility = Some(enabled),
                2004 => self.bracketed_paste_enabled = enabled,
                1049 if enabled => {
                    if let Some(screen) = self.screen.as_mut() {
                        screen.reset();
                    }
                }
                1049 => self.invalidate_projection(),
                1003 | 1004 | 1006 | 2026 | 9001 => {}
                _ => self.invalidate_projection(),
            }
        }

        match cursor_visibility {
            Some(false) => {
                self.begin_frame();
                StartupProgress::None
            }
            Some(true) if self.frame_active => self.finish_frame(),
            _ => StartupProgress::None,
        }
    }

    fn begin_frame(&mut self) {
        self.frame_active = self.phase != StartupPhase::Complete;
        self.frame_invalid = false;
    }

    fn finish_frame(&mut self) -> StartupProgress {
        self.frame_active = false;
        if self.frame_invalid {
            self.frame_invalid = false;
            return StartupProgress::None;
        }

        let Some(screen) = self.screen.as_ref().filter(|screen| screen.is_trusted()) else {
            return StartupProgress::None;
        };
        let normalized = screen.text().replace('\u{2026}', "...");
        let lower = normalized.to_ascii_lowercase();

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
                if self.bracketed_paste_enabled
                    && !has_starting
                    && !has_launcher
                    && has_interactive_composer =>
            {
                self.phase = StartupPhase::Complete;
                StartupProgress::InteractiveReady
            }
            _ => StartupProgress::None,
        }
    }

    fn push_visible(&mut self, character: char) {
        if character.is_control() {
            return;
        }
        let Some(width) = character.width() else {
            self.invalidate_projection();
            return;
        };
        if self
            .screen
            .as_mut()
            .is_some_and(|screen| !screen.write(character, width))
        {
            self.invalidate_projection();
        }
    }

    fn invalidate_projection(&mut self) {
        if self.frame_active {
            self.frame_invalid = true;
        }
        if let Some(screen) = self.screen.as_mut() {
            screen.invalidate();
        }
    }

    fn move_screen_cursor(&mut self, row_delta: isize, col_delta: isize) {
        if let Some(screen) = self.screen.as_mut() {
            screen.move_cursor(row_delta, col_delta);
        }
    }

    fn numeric_parameters(parameters: &str) -> Option<Vec<usize>> {
        if parameters.is_empty() {
            return Some(Vec::new());
        }
        parameters
            .split(';')
            .map(|parameter| {
                if parameter.is_empty() {
                    Some(0)
                } else {
                    parameter.parse::<u16>().ok().map(usize::from)
                }
            })
            .collect()
    }

    fn parameter(values: &[usize], index: usize, default: usize) -> usize {
        match values.get(index).copied().unwrap_or(0) {
            0 => default,
            value => value,
        }
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
        assert_eq!('❯'.width(), Some(1));
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
    fn startup_screen_tracks_wide_cells_and_clears_a_partially_overwritten_glyph() {
        let mut screen = StartupScreen::new(4, 2, true).unwrap();
        assert!(screen.write('A', 1));
        assert!(screen.write('漢', 2));
        assert!(screen.write('B', 1));
        assert!(screen.write('C', 1));
        assert_eq!(screen.text(), "A漢BC");

        screen.set_cursor(0, 2);
        assert!(screen.write('x', 1));
        assert_eq!(screen.text(), "A xBC");
        assert!(screen.is_trusted());

        let mut edge = StartupScreen::new(4, 2, true).unwrap();
        for character in ['A', 'B', 'C'] {
            assert!(edge.write(character, 1));
        }
        assert!(edge.write('漢', 2));
        assert_eq!(edge.text(), "ABC 漢");
        assert_eq!((edge.cursor_row, edge.cursor_col), (1, 2));
        assert!(edge.is_trusted());
    }

    #[test]
    fn startup_tracker_requires_bracketed_paste_and_rejects_launcher_state() {
        let mut disabled = tracker_with_positioned_starting_screen();
        assert_eq!(
            disabled.observe_output("\x1b[?2004l\x1b[?25l\x1b[11;5H\x1b[2K\x1b[18;7H\x1b[?25h"),
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
        assert_eq!(
            tracker
                .screen
                .as_ref()
                .map(|screen| (screen.cols, screen.rows)),
            Some((196, 22))
        );
        tracker.begin_run();
        assert_eq!(
            tracker
                .screen
                .as_ref()
                .map(|screen| (screen.cols, screen.rows)),
            Some((196, 22))
        );

        tracker.resize(u16::MAX, u16::MAX);
        assert!(tracker.screen.is_none());
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
