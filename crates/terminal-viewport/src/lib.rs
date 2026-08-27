use unicode_width::UnicodeWidthChar as _;

pub const MAX_CELLS: usize = 64 * 1024;
pub const DEFAULT_COLS: usize = 120;
pub const DEFAULT_ROWS: usize = 40;

pub const CONTROL_MAX_CHARS: u16 = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalStringKind {
    Osc,
    Dcs,
    Sos,
    Pm,
    Apc,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum ParserState {
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct Screen {
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

impl Screen {
    fn new(cols: usize, rows: usize, trusted: bool) -> Option<Self> {
        let cell_count = cols.checked_mul(rows)?;
        if cols == 0 || rows == 0 || cell_count > MAX_CELLS {
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
        let continuations = self.continuations[source_start..source_start + self.cols].to_vec();
        let valid = self.valid[source_start..source_start + self.cols].to_vec();
        self.cells[destination_start..destination_start + self.cols].copy_from_slice(&cells);
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

    fn row_text(&self, row: usize) -> String {
        let start = self.index(row, 0);
        let end = start + self.cols;
        let last = self.cells[start..end]
            .iter()
            .enumerate()
            .rposition(|(col, character)| *character != ' ' || self.continuations[start + col])
            .map_or(0, |index| index + 1);
        let mut output = String::with_capacity(last);
        for col in 0..last {
            if !self.continuations[start + col] {
                output.push(self.cells[start + col]);
            }
        }
        output
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalSignals {
    pub cursor_hidden: bool,
    pub cursor_shown: bool,
    pub projection_invalidated: bool,
}

pub struct TrustedScreen<'a> {
    screen: &'a Screen,
    cursor_visible: bool,
}

impl TrustedScreen<'_> {
    pub fn cols(&self) -> usize {
        self.screen.cols
    }

    pub fn rows(&self) -> usize {
        self.screen.rows
    }

    pub fn cursor_row(&self) -> usize {
        self.screen.cursor_row
    }

    pub fn cursor_col(&self) -> usize {
        self.screen.cursor_col
    }

    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    pub fn row_text(&self, row: usize) -> Option<String> {
        (row < self.screen.rows).then(|| self.screen.row_text(row))
    }

    pub fn text(&self) -> String {
        self.screen.text()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalViewport {
    parser: ParserState,
    csi_parameters: String,
    screen: Option<Screen>,
    cursor_visible: bool,
    observation_invalidated: bool,
}

impl Default for TerminalViewport {
    fn default() -> Self {
        Self {
            parser: ParserState::Ground,
            csi_parameters: String::new(),
            screen: Screen::new(DEFAULT_COLS, DEFAULT_ROWS, true),
            cursor_visible: true,
            observation_invalidated: false,
        }
    }
}

impl TerminalViewport {
    pub fn begin_run(&mut self) {
        let (cols, rows) = self
            .screen
            .as_ref()
            .map_or((DEFAULT_COLS, DEFAULT_ROWS), |screen| {
                (screen.cols, screen.rows)
            });
        *self = Self::default();
        self.screen = Screen::new(cols, rows, true);
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.screen = Screen::new(usize::from(cols), usize::from(rows), false);
        self.cursor_visible = true;
    }

    pub fn trusted_screen(&self) -> Option<TrustedScreen<'_>> {
        self.screen
            .as_ref()
            .filter(|screen| self.parser == ParserState::Ground && screen.is_trusted())
            .map(|screen| TrustedScreen {
                screen,
                cursor_visible: self.cursor_visible,
            })
    }

    pub fn dimensions(&self) -> Option<(usize, usize)> {
        self.screen
            .as_ref()
            .map(|screen| (screen.cols, screen.rows))
    }

    pub fn observe_character(&mut self, character: char) -> TerminalSignals {
        self.observation_invalidated = false;
        let mut signals = if self.observe_global_transition(character) {
            TerminalSignals::default()
        } else {
            match self.parser {
                ParserState::Ground => {
                    if character == '\u{001b}' {
                        self.parser = ParserState::Escape;
                    } else if Self::is_c0_executable_or_del(character) {
                        self.execute_c0(character);
                    } else {
                        self.push_visible(character);
                    }
                    TerminalSignals::default()
                }
                ParserState::Escape => {
                    if Self::is_c0_executable_or_del(character) {
                        self.execute_c0(character);
                    } else {
                        self.observe_escape(character);
                    }
                    TerminalSignals::default()
                }
                ParserState::EscapeIntermediate { mut chars_seen } => {
                    if Self::is_c0_executable_or_del(character) {
                        self.execute_c0(character);
                    } else if ('\u{20}'..='\u{2f}').contains(&character) {
                        chars_seen = chars_seen.saturating_add(1);
                        if chars_seen > 4 {
                            self.invalidate_projection();
                            self.parser = ParserState::Ground;
                        } else {
                            self.parser = ParserState::EscapeIntermediate { chars_seen };
                        }
                    } else {
                        self.invalidate_projection();
                        self.parser = ParserState::Ground;
                    }
                    TerminalSignals::default()
                }
                ParserState::Csi { mut chars_seen } => {
                    chars_seen = chars_seen.saturating_add(1);
                    if chars_seen > CONTROL_MAX_CHARS {
                        self.invalidate_projection();
                        self.csi_parameters.clear();
                        self.parser = ParserState::CsiDiscard;
                        self.observe_csi_discard(character);
                        TerminalSignals::default()
                    } else if Self::is_c0_executable_or_del(character) {
                        self.execute_c0(character);
                        self.parser = ParserState::Csi { chars_seen };
                        TerminalSignals::default()
                    } else if character == '\u{001b}' {
                        self.csi_parameters.clear();
                        self.parser = ParserState::Escape;
                        TerminalSignals::default()
                    } else if ('\u{40}'..='\u{7e}').contains(&character) {
                        let parameters = std::mem::take(&mut self.csi_parameters);
                        self.parser = ParserState::Ground;
                        self.finish_csi(&parameters, character)
                    } else if character.is_ascii() {
                        self.csi_parameters.push(character);
                        self.parser = ParserState::Csi { chars_seen };
                        TerminalSignals::default()
                    } else {
                        self.invalidate_projection();
                        self.csi_parameters.clear();
                        self.parser = ParserState::Ground;
                        TerminalSignals::default()
                    }
                }
                ParserState::CsiDiscard => {
                    self.observe_csi_discard(character);
                    TerminalSignals::default()
                }
                ParserState::String { kind, chars_seen } => {
                    self.observe_string(character, kind, Some(chars_seen));
                    TerminalSignals::default()
                }
                ParserState::StringDiscard { kind } => {
                    self.observe_string(character, kind, None);
                    TerminalSignals::default()
                }
            }
        };
        signals.projection_invalidated = self.observation_invalidated;
        signals
    }

    fn observe_global_transition(&mut self, character: char) -> bool {
        self.parser = match character {
            '\u{0018}' | '\u{001a}' | '\u{009c}' => ParserState::Ground,
            '\u{0084}' => {
                if let Some(screen) = self.screen.as_mut() {
                    screen.line_feed();
                }
                ParserState::Ground
            }
            '\u{0085}' => {
                if let Some(screen) = self.screen.as_mut() {
                    screen.carriage_return();
                    screen.line_feed();
                }
                ParserState::Ground
            }
            '\u{008d}' => {
                self.invalidate_projection();
                ParserState::Ground
            }
            '\u{009b}' => {
                self.csi_parameters.clear();
                ParserState::Csi { chars_seen: 0 }
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
                ParserState::Ground
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
                ParserState::Csi { chars_seen: 0 }
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
                ParserState::Ground
            }
            'E' => {
                if let Some(screen) = self.screen.as_mut() {
                    screen.carriage_return();
                    screen.line_feed();
                }
                ParserState::Ground
            }
            'c' => {
                if let Some(screen) = self.screen.as_mut() {
                    screen.reset();
                }
                self.cursor_visible = true;
                ParserState::Ground
            }
            '\\' | '=' | '>' => ParserState::Ground,
            '\u{20}'..='\u{2f}' => ParserState::EscapeIntermediate { chars_seen: 1 },
            '\u{001b}' => ParserState::Escape,
            _ => {
                self.invalidate_projection();
                ParserState::Ground
            }
        };
    }

    fn observe_csi_discard(&mut self, character: char) {
        if Self::is_c0_executable_or_del(character) {
            self.execute_c0(character);
            return;
        }
        self.parser = match character {
            '\u{001b}' => ParserState::Escape,
            '\u{40}'..='\u{7e}' => ParserState::Ground,
            _ => ParserState::CsiDiscard,
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
                self.parser = ParserState::Ground;
            }
            '\u{001b}' => self.parser = ParserState::Escape,
            _ => match chars_seen {
                Some(chars_seen) => {
                    let chars_seen = chars_seen.saturating_add(1);
                    if chars_seen > CONTROL_MAX_CHARS {
                        self.invalidate_projection();
                        self.parser = ParserState::StringDiscard { kind };
                    } else {
                        self.parser = ParserState::String { kind, chars_seen };
                    }
                }
                None => self.parser = ParserState::StringDiscard { kind },
            },
        }
    }

    fn finish_csi(&mut self, parameters: &str, final_character: char) -> TerminalSignals {
        if matches!(final_character, 'h' | 'l') && parameters.starts_with('?') {
            return self.finish_private_mode(parameters, final_character == 'h');
        }
        if final_character == 'm' {
            return TerminalSignals::default();
        }
        if final_character == 'q'
            && parameters
                .strip_suffix(' ')
                .and_then(Self::numeric_parameters)
                .is_some_and(|values| values.len() <= 1)
        {
            return TerminalSignals::default();
        }

        let Some(values) = Self::numeric_parameters(parameters) else {
            self.invalidate_projection();
            return TerminalSignals::default();
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
        TerminalSignals::default()
    }

    fn finish_private_mode(&mut self, parameters: &str, enabled: bool) -> TerminalSignals {
        let Some(modes) = parameters
            .strip_prefix('?')
            .and_then(Self::numeric_parameters)
            .filter(|modes| !modes.is_empty() && modes.iter().all(|mode| *mode != 0))
        else {
            self.invalidate_projection();
            return TerminalSignals::default();
        };

        let mut cursor_visibility = None;
        for mode in modes {
            match mode {
                25 => cursor_visibility = Some(enabled),
                1049 if enabled => {
                    if let Some(screen) = self.screen.as_mut() {
                        screen.reset();
                    }
                }
                1049 => self.invalidate_projection(),
                1003 | 1004 | 1006 | 2004 | 2026 | 9001 => {}
                _ => self.invalidate_projection(),
            }
        }

        let mut signals = TerminalSignals::default();
        match cursor_visibility {
            Some(false) => {
                self.cursor_visible = false;
                signals.cursor_hidden = true;
            }
            Some(true) => {
                self.cursor_visible = true;
                signals.cursor_shown = true;
            }
            None => {}
        }
        signals
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
        self.observation_invalidated = true;
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

    fn terminal_string(kind: TerminalStringKind) -> ParserState {
        ParserState::String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_hide_show_commits_a_trusted_screen() {
        let mut terminal = TerminalViewport::default();
        let mut hidden = false;
        let mut shown = false;
        for character in
            "\u{1b}[?25l\u{1b}[2J\u{1b}[1;1H› prompt\r\n  model · ~\\work\u{1b}[1;3H\u{1b}[?25h"
                .chars()
        {
            let signals = terminal.observe_character(character);
            hidden |= signals.cursor_hidden;
            shown |= signals.cursor_shown;
        }
        let screen = terminal.trusted_screen().expect("trusted reconstruction");
        assert!(hidden && shown);
        assert!(screen.cursor_visible());
        assert_eq!((screen.cursor_row(), screen.cursor_col()), (0, 2));
        assert_eq!(screen.row_text(0).as_deref(), Some("› prompt"));
        assert_eq!(screen.row_text(1).as_deref(), Some("  model · ~\\work"));
    }

    #[test]
    fn unknown_control_taints_until_a_known_full_clear() {
        let mut terminal = TerminalViewport::default();
        for character in "\u{1b}[?25l\u{1b}[999z".chars() {
            terminal.observe_character(character);
        }
        assert!(terminal.trusted_screen().is_none());
        for character in "\u{1b}[2J\u{1b}[Hknown\u{1b}[?25h".chars() {
            terminal.observe_character(character);
        }
        assert_eq!(
            terminal
                .trusted_screen()
                .and_then(|screen| screen.row_text(0))
                .as_deref(),
            Some("known")
        );
    }

    #[test]
    fn codex_cursor_shape_is_a_measured_noop() {
        let mut terminal = TerminalViewport::default();
        for character in "\u{1b}[0 q".chars() {
            assert!(!terminal.observe_character(character).projection_invalidated);
        }
        assert!(terminal.trusted_screen().is_some());
    }

    #[test]
    fn wide_cells_wrap_and_partial_overwrite_clear_the_full_glyph() {
        let mut terminal = TerminalViewport::default();
        terminal.resize(4, 2);
        for character in "\u{1b}[2J\u{1b}[HA\u{6f22}BC\u{1b}[1;3Hx".chars() {
            terminal.observe_character(character);
        }
        let screen = terminal
            .trusted_screen()
            .expect("known full reconstruction");
        assert_eq!(screen.text(), "A xBC");
        assert_eq!((screen.cursor_row(), screen.cursor_col()), (0, 3));

        terminal.resize(4, 2);
        for character in "\u{1b}[2J\u{1b}[HABC\u{6f22}".chars() {
            terminal.observe_character(character);
        }
        let screen = terminal
            .trusted_screen()
            .expect("known full reconstruction");
        assert_eq!(screen.text(), "ABC \u{6f22}");
        assert_eq!((screen.cursor_row(), screen.cursor_col()), (1, 2));
    }
}
