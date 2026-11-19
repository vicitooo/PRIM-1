//! Positive evidence for a small room notification. Work-state timers and
//! startup readiness are deliberately not evidence of an empty composer.
use anyhow::{Result, anyhow};
use shared_types::DriverKind;
use terminal_viewport::TerminalViewport;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct InputActivity {
    pub epoch: u64,
    last_at: Option<Instant>,
    /// The operator typed text that no Enter, Ctrl+C or Ctrl+U has ended:
    /// an unsent draft that a room message must not be appended to.
    draft_open: bool,
    in_paste: bool,
}

impl InputActivity {
    pub fn record(&mut self) {
        self.epoch = self.epoch.saturating_add(1);
        self.last_at = Some(Instant::now());
    }

    /// Operator keystrokes: tracks whether they leave an unsent draft. Text
    /// inside a bracketed paste (CR included) is draft; Up recalls history
    /// into the input, so it opens one too. Other escape sequences do not.
    /// v3 W3: the desktop terminal's automatic replies to the harness's own
    /// queries (cursor position, device attributes, focus, OSC/DCS reports)
    /// are not operator typing: a freshly launched pane's wake was deferred
    /// as "the operator typed" though nobody had (M1 journey, 24 Oct).
    pub fn record_input(&mut self, input: &str) {
        if is_terminal_report(input) {
            return;
        }
        self.record();
        let mut chars = input.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '\u{1b}' if matches!(chars.peek(), Some('[') | Some('O')) => {
                    let mut sequence = String::new();
                    chars.next();
                    while let Some(c) = chars.next_if(|c| !('@'..='~').contains(c)) { sequence.push(c); }
                    let last = chars.next();
                    match (sequence.as_str(), last) {
                        ("200", Some('~')) => { self.in_paste = true; self.draft_open = true; }
                        ("201", Some('~')) => self.in_paste = false,
                        (_, Some('A')) => self.draft_open = true,
                        _ => {}
                    }
                }
                _ if self.in_paste => self.draft_open = true,
                '\r' | '\u{3}' | '\u{15}' => self.draft_open = false,
                ch if !ch.is_control() => self.draft_open = true,
                _ => {}
            }
        }
    }

    pub fn draft_open(&self) -> bool {
        self.draft_open
    }

    pub fn admits(&self, expected: Option<u64>) -> bool {
        self.epoch != u64::MAX && expected.is_none_or(|epoch| epoch == self.epoch)
            && self.last_at.is_none_or(|at| at.elapsed() >= Duration::from_millis(500))
    }

    /// No operator keystroke for at least `grace`: a pause this long is not
    /// mid-word typing, so a whole room message may follow.
    pub fn quiet_for(&self, grace: Duration) -> bool {
        self.epoch != u64::MAX && self.last_at.is_none_or(|at| at.elapsed() >= grace)
    }
}

/// Input made only of terminal replies: CSI cursor-position (`R`), focus
/// (`I`/`O`), device-attribute and mode reports (`?`/`>`/`=` parameters
/// ending `c`, `y` or `u`), and OSC or DCS strings. Anything else — a printable
/// character, CR, a control key, an arrow key — is operator input.
pub(super) fn is_terminal_report(input: &str) -> bool {
    let bytes = input.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != 0x1b || index + 1 >= bytes.len() {
            return false;
        }
        match bytes[index + 1] {
            b'[' => {
                let start = index + 2;
                let mut end = start;
                while end < bytes.len() && !(0x40..=0x7e).contains(&bytes[end]) { end += 1; }
                if end >= bytes.len() { return false; }
                let params = &bytes[start..end];
                let report = match bytes[end] {
                    // xterm sends a modified F3 as `ESC [1;<2..16>R`, the same
                    // shape as a cursor report at row 1: ambiguous, so it stays
                    // operator input (Reviewer 4d1c8ae3).
                    b'R' => !params.is_empty() && params.iter().all(|b| b.is_ascii_digit() || *b == b';')
                        && !modified_f3(params),
                    b'I' | b'O' => params.is_empty(),
                    b'c' | b'y' | b'u' => matches!(params.first(), Some(b'?' | b'>' | b'=')),
                    _ => false,
                };
                if !report { return false; }
                index = end + 1;
            }
            b']' | b'P' => {
                // A string up to BEL or ST (ESC \).
                let mut end = index + 2;
                loop {
                    if end >= bytes.len() { return false; }
                    if bytes[end] == 0x07 { end += 1; break; }
                    if bytes[end] == 0x1b && bytes.get(end + 1) == Some(&b'\\') { end += 2; break; }
                    end += 1;
                }
                index = end;
            }
            _ => return false,
        }
    }
    !input.is_empty()
}

fn modified_f3(params: &[u8]) -> bool {
    std::str::from_utf8(params).ok().and_then(|text| text.strip_prefix("1;"))
        .and_then(|modifier| modifier.parse::<u8>().ok()).is_some_and(|modifier| (2..=16).contains(&modifier))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ComposerEvidence {
    pub frame: u64,
    pub row: usize,
    pub start_col: usize,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, Default)]
pub(super) struct RoomComposer {
    viewport: TerminalViewport,
    completed_frames: u64,
    work_in_progress: bool,
    /// v3 W2: when this run last printed anything.
    last_output_at: Option<Instant>,
}

impl RoomComposer {
    pub fn begin_run(&mut self) {
        self.viewport.begin_run();
        self.completed_frames = 0;
        self.work_in_progress = false;
        self.last_output_at = None;
    }

    /// v3 W2: no output from this run for at least `settle` (a run that has
    /// printed nothing yet is quiet). Mid-turn, Codex 0.164 repaints at
    /// least every ~0.5 s (measured on three captures); at its prompt it is
    /// silent.
    pub fn output_quiet_for(&self, settle: Duration) -> bool {
        self.last_output_at.is_none_or(|at| at.elapsed() >= settle)
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.viewport.resize(cols, rows);
        self.completed_frames = 0;
    }

    /// A16: the run has drawn a non-blank glyph (read from its viewport).
    pub fn drew_visible_text(&self) -> bool {
        self.viewport.drew_visible_text()
    }

    pub fn invalidate_for_input(&mut self) {
        if let Some((cols, rows)) = self.viewport.dimensions() {
            self.resize(cols as u16, rows as u16);
        }
    }

    pub fn observe(&mut self, chunk: &str) {
        if !chunk.is_empty() {
            self.last_output_at = Some(Instant::now());
        }
        // A quiescence timer outside this tracker cannot erase positive busy
        // evidence. Only an explicit measured completion observation can.
        let classification = driver_grok::classify_work_state(chunk);
        if let Some((state, _)) = classification {
            match state {
                shared_types::WorkState::Thinking | shared_types::WorkState::ToolCall => self.work_in_progress = true,
                shared_types::WorkState::Idle => self.work_in_progress = false,
                _ => {}
            }
        }
        let prior_frame = self.completed_frames;
        for ch in chunk.chars() {
            let was_drawing = self.viewport.synchronized_output_active();
            self.viewport.observe_character(ch);
            if was_drawing && !self.viewport.synchronized_output_active() {
                self.completed_frames = self.completed_frames.saturating_add(1);
            }
        }
        // A busy word may be split across output chunks or ANSI controls.
        // Inspect the bounded, completed projection, not just chunk substrings.
        // Only explicit completion output clears the latch; old timer state
        // and an incomplete redraw cannot do so.
        if self.completed_frames > prior_frame && !self.viewport.synchronized_output_active()
            && classification.as_ref().is_none_or(|(state, _)| *state != shared_types::WorkState::Idle) {
            if let Some(screen) = self.viewport.projected_screen() {
                if (0..screen.rows()).filter(|row| screen.row_trusted(*row)).any(|row|
                    screen.row_text(row).and_then(|text| driver_grok::classify_work_state(&text))
                        .is_some_and(|(state, _)| matches!(state, shared_types::WorkState::Thinking | shared_types::WorkState::ToolCall))) {
                    self.work_in_progress = true;
                }
            }
        }
    }

    /// A selection menu is open on the member's screen (trust, bypass,
    /// approval, update or sign-in question): a typed message plus Enter
    /// would answer it. The shape is a highlighted option row with a
    /// confirm/cancel hint below it: Claude's "❯" row, Codex's "›" row (a
    /// numbered option only, since its composer and transcript rows also
    /// start with "›"), or Grok's keyed "y  Yes, proceed" trust row. Quoted
    /// chat text rarely has both. Read from the screen because these dialogs
    /// arrive split across output chunks.
    pub fn open_question(&self) -> Option<&'static str> {
        let screen = self.viewport.projected_screen()?;
        // A quoted menu can remain above Codex's real composer. Reuse the
        // driver's measured cursor/prompt/footer proof, from a fully trusted,
        // finished screen only. This never suppresses a menu below that prompt
        // or one whose hint is still below it. Claude has no equivalent measured
        // prompt predicate; its footer substrings cannot safely dismiss a menu.
        let clean_codex_row = if self.viewport.synchronized_output_active() {
            None
        } else {
            self.viewport.trusted_screen()
                .filter(driver_codex::is_clean_prompt_screen)
                .map(|screen| screen.cursor_row())
        };
        let rows: Vec<String> = (0..screen.rows())
            .map(|row| screen.row_text(row).unwrap_or_default().to_ascii_lowercase()).collect();
        rows.iter().enumerate().any(|(index, text)| option_row(text.trim_start())
            .is_some_and(|menu| rows[index..].iter().enumerate().any(|(offset, row)| {
                hint_row(menu, row) && !(matches!(menu, Menu::Codex)
                    && clean_codex_row.is_some_and(|prompt| index < prompt && index + offset < prompt))
            })))
            .then_some("a question is open in the member's terminal; the message waits until it is answered")
    }

    /// A latched question may still be open: one is visible, or the screen
    /// is not fully repainted since a resize or mid-redraw, so its absence is
    /// unknown rather than confirmed.
    pub fn question_may_be_open(&self) -> bool {
        self.open_question().is_some() || self.viewport.trusted_screen().is_none()
            || self.viewport.synchronized_output_active()
    }

    /// The screen a driver's classifier may read for an open question, never
    /// mid-redraw: the whole screen once every cell is repainted (a dialog
    /// whose layout the menu shape above does not cover, such as Claude's
    /// select dialogs without a hint), or, before that (a resize at launch,
    /// a harness that never clears the screen), the projected screen when it
    /// also shows a selection menu with its hint. Stale text alone never
    /// reads as a question.
    pub fn question_text(&self) -> Option<String> {
        if self.viewport.synchronized_output_active() {
            return None;
        }
        if let Some(screen) = self.viewport.trusted_screen() {
            return Some(screen.text());
        }
        self.open_question()?;
        self.viewport.projected_screen().map(|screen| screen.text())
    }

    pub fn empty(&self, driver: DriverKind, payload: &str) -> Result<ComposerEvidence> {
        let evidence = self.exact(driver, "")?;
        if payload.is_empty() || payload.len() > evidence.max_bytes
            || !payload.bytes().all(|b| (b' '..=b'~').contains(&b)) {
            return Err(anyhow!("room wake does not fit one measured composer row"));
        }
        Ok(evidence)
    }

    pub fn echoed(&self, driver: DriverKind, payload: &str, before: ComposerEvidence) -> bool {
        self.exact(driver, payload).is_ok_and(|now| now.frame > before.frame
            && now.row == before.row && now.start_col == before.start_col
            && now.max_bytes == before.max_bytes)
    }

    fn exact(&self, driver: DriverKind, content: &str) -> Result<ComposerEvidence> {
        // There is no captured Claude TUI composer/approval/echo fixture in
        // this source tree. An idle word or a footer substring cannot admit it.
        if driver != DriverKind::Grok {
            return Err(anyhow!("this harness has no verified room-wake composer adapter"));
        }
        if self.work_in_progress {
            return Err(anyhow!("room wake is held while the harness is working"));
        }
        if self.completed_frames == 0 || self.completed_frames == u64::MAX
            || self.viewport.synchronized_output_active() {
            return Err(anyhow!("room wake is held until a completed composer redraw"));
        }
        let screen = self.viewport.projected_screen()
            .ok_or_else(|| anyhow!("room composer projection is incomplete"))?;
        let row = screen.cursor_row();
        // Measured Grok fullscreen: one input row bounded above/below, then
        // a blank row and its mode/shortcut footer. Minimal mode is unproven.
        if !screen.cursor_visible() || row < 3 || row + 4 >= screen.rows() {
            return Err(anyhow!("room wake has no measured fullscreen composer"));
        }
        let lines: Option<Vec<String>> = (row - 3..=row + 4)
            .map(|r| screen.row_trusted(r).then(|| screen.row_text(r)).flatten()).collect();
        let lines = lines.ok_or_else(|| anyhow!("room composer rows need a fresh repaint"))?;
        let border = lines[3].chars().collect::<Vec<_>>();
        if border.len() < 9 || border[0..4] != [' ', ' ', '\u{2502}', ' ']
            || !matches!(border[4], '\u{276f}' | '>') || border[5] != ' '
            || border.last() != Some(&'\u{2502}') {
            return Err(anyhow!("room composer is not the measured single-line input"));
        }
        let end = border.len() - 1;
        let max_bytes = end.saturating_sub(7); // Leave the rightmost interior cell blank.
        let expected = format!("{}{}", content, " ".repeat(end.saturating_sub(6 + content.len())));
        if content.len() > max_bytes || screen.cursor_col() != 6 + content.len()
            || border[6..end].iter().collect::<String>() != expected
            || !lines[0].is_empty() || !lines[1].is_empty()
            || !lines[5].is_empty() || !lines[7].is_empty()
            || lines[6] != "  Shift+Tab:mode  \u{2502}  Ctrl+x:shortcuts"
            || !top_border(&lines[2], border.len()) || !bottom_border(&lines[4], border.len()) {
            return Err(anyhow!("room wake is held for a draft, busy indicator, modal or unknown layout"));
        }
        Ok(ComposerEvidence { frame: self.completed_frames, row, start_col: 6, max_bytes })
    }
}

#[derive(Clone, Copy)]
enum Menu {
    Claude,
    Codex,
    GrokTrust,
}

/// The menu a trimmed, lowercased row is the highlighted option of, if any.
fn option_row(row: &str) -> Option<Menu> {
    if let Some(rest) = row.strip_prefix('\u{276f}').map(str::trim_start) {
        // Multi-select rows carry a box or a radio mark: "❯ [x] one", "❯ ◉ one".
        (rest.starts_with(|c: char| c.is_ascii_digit() || matches!(c, '[' | '\u{25ef}' | '\u{25c9}'))
            || rest.starts_with("yes") || rest.starts_with("no"))
            .then_some(Menu::Claude)
    } else if let Some(rest) = row.strip_prefix('\u{203a}').map(str::trim_start) {
        rest.split_once('.').is_some_and(|(number, _)| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
            .then_some(Menu::Codex)
    } else {
        row.strip_prefix("y ").is_some_and(|rest| rest.trim_start().starts_with("yes, proceed"))
            .then_some(Menu::GrokTrust)
    }
}

/// A lowercased row is that menu's confirm/cancel hint.
fn hint_row(menu: Menu, row: &str) -> bool {
    match menu {
        // Claude's dialog guides; Grok may paint its trust choices with "❯" too.
        Menu::Claude => ["esc to cancel", "enter to confirm", "esc to exit", "enter or y to trust", "space to select"]
            .iter().any(|hint| row.contains(hint)),
        // Codex 0.15x: "Press enter to continue"; 0.162+ key hints such as
        // "enter continue · esc skip" or "Press enter to confirm or esc to go
        // back", or the 0.162.1/0.163.0 selected effort scope footer. A hint row
        // starts with its key, which transcript prose rarely does.
        Menu::Codex => {
            let row = row.trim_start();
            (row.starts_with("press ") || row.starts_with("enter "))
                && (["continue", "confirm", "go back", "cancel", "select"].iter().any(|word| row.contains(word))
                    || row == "enter default · s session · esc back")
        }
        Menu::GrokTrust => row.contains("enter or y to trust"),
    }
}

fn top_border(line: &str, width: usize) -> bool {
    let chars: Vec<_> = line.chars().collect();
    chars.len() == width && chars.starts_with(&[' ', ' ', '\u{256d}'])
        && chars.last() == Some(&'\u{256e}')
        && chars[3..width - 1].iter().all(|ch| *ch == '\u{2500}')
}

fn bottom_border(line: &str, width: usize) -> bool {
    let chars: Vec<_> = line.chars().collect();
    chars.len() == width && chars.starts_with(&[' ', ' ', '\u{2570}'])
        && chars.last() == Some(&'\u{256f}')
        && line.contains(" Grok ")
        && chars[3..width - 1].iter().all(|ch| *ch == '\u{2500}' || ch.is_ascii_graphic() || *ch == ' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULLSCREEN: &str = include_str!("../../driver-grok/src/fixtures/grok-startup-fullscreen-v105.json");
    const ASCII: &str = include_str!("../../driver-grok/src/fixtures/grok-startup-fullscreen-noterm-v105.json");
    const WAKE: &str = "Room message 00000000-0000-4000-8000-000000000001; room_read.";

    fn measured(fixture: &str) -> RoomComposer {
        let mut tracker = RoomComposer::default();
        tracker.resize(197, 56);
        tracker.observe(&serde_json::from_str::<String>(fixture).unwrap());
        tracker
    }

    #[test]
    fn open_selection_menus_hold_but_quoted_text_and_composers_do_not() {
        let screen = |text: &str| {
            let mut tracker = RoomComposer::default();
            tracker.resize(100, 20);
            tracker.observe(&format!("\x1b[2J\x1b[H{}", text.replace('\n', "\r\n")));
            tracker
        };
        for modal in [
            "Accessing workspace:\n\nQuick safety check: Is this a project you trust?\n\n\u{276f} No, exit\n  Yes, I trust this folder\n\nEnter to confirm \u{b7} Esc to cancel",
            "Do you want to proceed?\n\u{276f} 1. Yes\n  2. Yes, and don't ask again\n  3. No\n\nEsc to cancel \u{b7} Tab to amend",
            "WARNING: Bypass Permissions mode\n\u{276f} 1. No, exit\n  2. Yes, I accept\n\nEnter to confirm \u{b7} Esc to exit",
            // Claude's multi-select: a checkbox or radio row and a space hint.
            "2 new MCP servers found in this project\nSelect any you wish to enable.\n\u{276f} [x] one\n  [x] two\n\nSpace to select \u{b7} Enter to confirm \u{b7} Esc to reject all",
            "Select any you wish to enable.\n\u{276f} \u{25c9} one\n  \u{25ef} two\n\nSpace to select",
        ] {
            assert!(screen(modal).open_question().is_some(), "{modal}");
        }
        for calm in [
            "I wrote: No, exit is preselected; Esc to cancel closes it.\n\u{256d}\u{2500}\u{256e}\n\u{2502} > \u{2502}\n? for shortcuts",
            "  \u{2502} \u{276f} hello                     \u{2502}\n  Shift+Tab:mode  \u{2502}  Ctrl+x:shortcuts",
            "\u{276f} [draft] release notes\n? for shortcuts",
        ] {
            assert!(screen(calm).open_question().is_none(), "{calm}");
        }
    }

    #[test]
    fn codex_numbered_menus_and_grok_trust_hold_but_codex_composer_and_transcript_do_not() {
        let screen = |text: &str| {
            let mut tracker = RoomComposer::default();
            tracker.resize(100, 20);
            tracker.observe(&format!("\x1b[2J\x1b[H{}", text.replace('\n', "\r\n")));
            tracker
        };
        for modal in [
            // Codex 0.15x trust, as driver-codex's live fixture paints it.
            "Do you trust the contents of this directory?\n\n\u{203a} 1. Yes, continue\n  2. No, quit\n\n  Press enter to continue",
            // Codex 0.163.0 update, rate-limit and migration questions (binary strings).
            "  \u{2728} Update available! 0.163.0 -> 0.164.0\n\n  Release notes: https://github.com/openai/codex/releases/latest\n\n\u{203a} 1. Update now (runs `codex update`)\n  2. Skip\n  3. Skip until next version\n\n  enter continue \u{b7} esc skip",
            "Approaching rate limits\n\u{203a} 1. Switch to gpt-6-mini\n  2. Keep current model\n\n  Press enter to confirm or esc to go back",
            "  Codex just got an upgrade.\n\u{203a} 1. Try new model\n  2. Use existing model\n\n  enter confirm \u{b7} ctrl + c quit",
            // Grok 1.0.62 trust (binary strings), keyed rows or a highlighted choice.
            "Do you trust the contents of this directory?\nGrok Build may run or modify contents in this directory,\nposing security risks.\n\ny  Yes, proceed\nn  No, quit\n\nEnter or y to trust \u{b7} n or Esc to quit",
            "Do you trust the contents of this directory?\n\u{276f} Yes, proceed\n  No, quit\n\nEnter or y to trust \u{b7} n or Esc to quit",
        ] {
            assert!(screen(modal).open_question().is_some(), "{modal}");
        }
        for calm in [
            // A numbered draft in the Codex composer above its status footer.
            "\u{203a} 1. fix the parser\n\n  gpt-5.6-sol max \u{b7} ~\\mywork",
            // A user turn, then prose quoting the old hint, then the composer.
            "\u{203a} please continue\n\nPress enter to continue is what the old prompt said.\n\n\u{203a} Ask Codex to do anything\n\n  gpt-5.6-sol max \u{b7} ~\\mywork",
            "\u{203a} 2026 roadmap\nwe can go back to confirm or drop it later",
            // A numbered user turn, then prose that mentions the hint words.
            "\u{203a} 1. fix the parser\nI will go back to confirm or cancel it; press on to continue.\n\n\u{203a} Ask Codex to do anything",
            // A quote of the Grok hint with no keyed choice above it.
            "The trust hint reads: Enter or y to trust.\n  \u{2502} \u{276f} hello \u{2502}",
            "y  yes, proceed was the old label\n? for shortcuts",
        ] {
            assert!(screen(calm).open_question().is_none(), "{calm}");
        }
    }

    #[test]
    fn codex_upstream_plan_model_and_effort_snapshots_hold_without_auto_confirm() {
        // Verbatim bodies, excluding frontmatter, from openai/codex tags
        // rust-v0.162.1 AND rust-v0.163.0 (all four files byte-identical),
        // codex-rs/tui/src/chatwidget/snapshots/, filename prefix
        // codex_tui__chatwidget__tests__: plan_implementation_popup.snap,
        // plan_implementation_popup_context_usage.snap, model_selection_popup.snap,
        // model_reasoning_selection_popup.snap. Keep the actual column spacing.
        for modal in [
            "  Implement this plan?


› 1. Yes, implement this plan          Switch to Default and start coding
  2. Yes, clear context and implement  Fresh thread with this plan
  3. No, stay in Plan mode             Continue planning with the model

  enter select · esc back
",
            "  Implement this plan?


› 1. Yes, implement this plan          Switch to Default and start coding
  2. Yes, clear context and implement  Start a fresh thread (current context:
                                       89% used)
  3. No, stay in Plan mode             Continue planning with the model

  enter select · esc back
",
            "  Select Model and Effort


  1. GPT-6.1-Sol        Latest workhorse model for coding and everyday work.
  2. GPT-6-Astra        Frontier intelligence for the most demanding work.
  3. GPT-6-Sol          Previous generation workhorse model.
  4. GPT-6-Luna         Fast and affordable model for easier tasks.
  5. GPT-5.6-Sol        Older generation workhorse model.
  6. GPT-5.6-Terra      Older balanced model for straightforward work.
  7. GPT-5.6-Luna       Older fast and efficient model.
› 8. GPT-5.5 (current)  Legacy coding model.

  enter select · esc back
",
            "  Select Reasoning Level for GPT-5.5


  1. Low               Fast responses with lighter reasoning
  2. Medium (default)  Balances speed and reasoning depth for everyday tasks
› 3. High (current)    Greater reasoning depth for complex problems
  4. Extra high        Extra high reasoning depth for complex problems
  5. Persistent        Continue working until put to sleep
  6. More reasoning…   Max and Ultra consume usage limits faster

  enter default · s session · esc back
",
        ] {
            let frame = format!("\x1b[2J\x1b[H{}", modal.replace('\n', "\r\n"));
            for split in 0..=frame.len() {
                if !frame.is_char_boundary(split) { continue; }
                let mut tracker = RoomComposer::default();
                tracker.resize(100, 24);
                tracker.observe(&frame[..split]);
                tracker.observe(&frame[split..]);
                assert!(tracker.open_question().is_some(), "split {split}: {modal}");
            }
        }
        for not_modal in [
            "› /plan  switch to Plan mode\n  enter select · esc back",
            "  1. High\n  enter default · s session · esc back",
            "› 1. draft\n  enter default is the suggested setting",
            "› 1. draft\n  enter default · s session · esc back is what the old popup said",
        ] {
            let mut tracker = RoomComposer::default();
            tracker.observe(&format!("\x1b[2J\x1b[H{}", not_modal.replace('\n', "\r\n")));
            assert!(tracker.open_question().is_none(), "{not_modal}");
        }
    }

    #[test]
    fn codex_historical_menu_above_measured_composer_is_not_a_current_question() {
        // Same verbatim upstream plan snapshot body as above.
        let menu = "  Implement this plan?


› 1. Yes, implement this plan          Switch to Default and start coding
  2. Yes, clear context and implement  Fresh thread with this plan
  3. No, stay in Plan mode             Continue planning with the model

  enter select · esc back
".replace('\n', "\r\n");
        let prompt = "\x1b[14;1H› Ask Codex to do anything\x1b[16;1H  gpt-5.6-sol max · ~\\mywork\x1b[14;3H\x1b[?25h";
        let clean = format!("\x1b[2J\x1b[H{menu}{prompt}");
        for split in 0..=clean.len() {
            if !clean.is_char_boundary(split) { continue; }
            let mut tracker = RoomComposer::default();
            tracker.resize(120, 24);
            tracker.observe(&clean[..split]);
            tracker.observe(&clean[split..]);
            assert!(tracker.open_question().is_none(), "clean split {split}");
            assert!(!tracker.question_may_be_open(), "settled trusted screen, split {split}");
        }
        // A prompt-like pair above the actual chooser cannot suppress it,
        // even if the terminal cursor still points at that earlier prompt.
        let inverse = format!("\x1b[2J\x1b[H› Ask Codex to do anything\x1b[3;1H  gpt-5.6-sol max · ~\\mywork\x1b[5;1H{menu}\x1b[1;3H\x1b[?25h");
        let mut tracker = RoomComposer::default();
        tracker.observe(&inverse);
        assert!(tracker.open_question().is_some());

        for unsafe_screen in [
            format!("{clean}\x1b[?2026h"),
            format!("{clean}\x1b[?25l"),
            format!("{clean}\x1b[14;4H"),
            clean.replace("~\\mywork", "unqualified-directory"),
            // The old selected row and a NEW menu hint below the composer
            // are not a wholly historical pair.
            format!("{clean}\x1b[20;1H  enter select · esc back\x1b[14;3H"),
        ] {
            let mut tracker = RoomComposer::default();
            tracker.observe(&unsafe_screen);
            assert!(tracker.open_question().is_some(), "{unsafe_screen:?}");
            assert!(tracker.question_may_be_open());
        }
        let mut partial = RoomComposer::default();
        partial.resize(120, 24);
        partial.observe(&format!("\x1b[H{menu}{prompt}"));
        assert!(partial.open_question().is_some(), "partial rows cannot dismiss a menu");
        assert!(partial.question_may_be_open());

        let mut malformed = RoomComposer::default();
        malformed.observe(&format!("{clean}\x1b["));
        assert!(malformed.question_may_be_open(), "unfinished control is unknown, not a clean return");

        let mut claude = RoomComposer::default();
        claude.observe(&format!("\x1b[2J\x1b[HDo you want to proceed?\r\n❯ 1. Yes\r\n  2. No\r\nEsc to cancel\r\n{prompt}"));
        assert!(claude.open_question().is_some(), "Codex proof never clears a Claude menu");
    }

    #[test]
    fn a_question_is_only_known_gone_after_a_full_repaint_without_it() {
        let mut tracker = RoomComposer::default();
        tracker.resize(60, 8);
        tracker.observe(&"\x1b[2J\x1b[HQuick safety check\r\n\u{276f} No, exit\r\n  Yes, I trust this folder\r\nEnter to confirm \u{b7} Esc to cancel".to_string());
        assert!(tracker.question_may_be_open());
        tracker.resize(70, 8);
        assert!(tracker.open_question().is_none(), "the resize leaves only unknown cells");
        assert!(tracker.question_may_be_open(), "an unknown screen keeps the latch");
        let blank_rows: String = (1..=8).map(|row| format!("\x1b[{row};1H\x1b[2K")).collect();
        tracker.observe(&format!("{blank_rows}\x1b[1;1H\u{2502} > \u{2502}\x1b[8;1H? for shortcuts"));
        assert!(!tracker.question_may_be_open(), "a full repaint without the question clears it");

        let mut split = RoomComposer::default();
        split.resize(60, 8);
        split.observe("\x1b[2J\x1b[H\u{276f} No, exit\r\n  Yes, I trust this folder\r\nEnter to confirm \u{b7} Esc to cancel");
        split.observe("\x1b[?2026h\x1b[2J");
        assert!(split.question_may_be_open(), "a cleared screen mid-frame is not a finished repaint");
        split.observe("\x1b[H\u{2502} > \u{2502}\x1b[8;1H? for shortcuts\x1b[?2026l");
        assert!(!split.question_may_be_open());
    }

    #[test]
    fn question_text_is_a_finished_full_repaint_or_a_partial_one_with_a_menu() {
        let mut tracker = RoomComposer::default();
        tracker.resize(60, 8);
        tracker.observe("\x1b[HNew MCP server found in this project: fixture");
        assert_eq!(tracker.question_text(), None, "cells left from before the resize are unknown");
        tracker.observe("\x1b[2J\x1b[HNew MCP server found in this project: fixture\r\n\u{276f} 1. Use this MCP server");
        assert!(tracker.question_text().is_some_and(|text| text.contains("Use this MCP server")));
        tracker.observe("\x1b[?2026h\x1b[2J");
        assert_eq!(tracker.question_text(), None, "a redraw in progress is not a finished screen");
        tracker.observe("\x1b[H? for shortcuts\x1b[?2026l");
        assert_eq!(tracker.question_text().as_deref().map(str::trim), Some("? for shortcuts"));

        // Never cleared since the launch resize: only a menu with its hint is read.
        let mut partial = RoomComposer::default();
        partial.resize(60, 8);
        partial.observe("\x1b[1;1HQuick safety check\x1b[3;1H\u{276f} No, exit\x1b[4;3HYes, I trust this folder\x1b[6;1HEnter to confirm \u{b7} Esc to cancel");
        assert!(partial.question_text().is_some_and(|text| text.contains("Quick safety check")));
        let mut quoted = RoomComposer::default();
        quoted.resize(60, 8);
        quoted.observe("\x1b[1;1HI wrote: No, exit is preselected after Quick safety check.");
        assert_eq!(quoted.question_text(), None, "no menu, no full repaint");
    }

    #[test]
    fn terminal_replies_are_not_operator_typing_but_keys_are() {
        for report in ["\x1b[12;5R", "\x1b[?1;2c", "\x1b[>0;276;0c", "\x1b[I", "\x1b[O", "\x1b[?1u",
            "\x1b]11;rgb:0000/0000/0000\x07", "\x1bP>|xterm.js(5.5.0)\x1b\\", "\x1b[12;5R\x1b[?62;22c"] {
            assert!(is_terminal_report(report), "{report:?}");
            let mut activity = InputActivity::default();
            activity.record_input(report);
            assert!(activity.quiet_for(Duration::from_secs(5)) && activity.epoch == 0 && !activity.draft_open(), "{report:?}");
        }
        for typed in ["a", "\r", "\x1b[A", "\x1b[D", "\u{3}", "\x1b[200~x\x1b[201~", "\x1b[12;5Rx", "", "\x1b[5~",
            // Modified F3 (xterm Keyboard.ts) and a report split across reads.
            "\x1b[1;5R", "\x1b[1;2R", "\x1b[12;5", "\x1b]11;rgb:0000"] {
            assert!(!is_terminal_report(typed), "{typed:?}");
        }
        let mut activity = InputActivity::default();
        activity.record_input("x");
        assert!(!activity.quiet_for(Duration::from_secs(5)), "typing still counts");
    }

    #[test]
    fn operator_draft_stays_open_until_enter_ctrl_c_or_ctrl_u() {
        let mut activity = InputActivity::default();
        assert!(!activity.draft_open());
        activity.record_input("hel");
        assert!(activity.draft_open());
        activity.record_input("\x1b[D\x1b[C");
        assert!(activity.draft_open(), "left/right neither open nor end a draft");
        activity.record_input("lo\r");
        assert!(!activity.draft_open());
        activity.record_input("\x1b[D");
        assert!(!activity.draft_open(), "a cursor key alone opens no draft");
        activity.record_input("\x1b[A");
        assert!(activity.draft_open(), "Up recalls history into the input");
        activity.record_input("\u{3}");
        activity.record_input("\x1b[200~line one\rline two\x1b[201~");
        assert!(activity.draft_open(), "CR inside a paste is draft text, not a submit");
        activity.record_input("\x1b[200~pasted\x1b[201~\r");
        assert!(!activity.draft_open());
        activity.record_input("\x1b[200~pasted\x1b[201~");
        assert!(activity.draft_open());
        activity.record_input("\u{3}");
        assert!(!activity.draft_open());
        activity.record_input("x\u{15}");
        assert!(!activity.draft_open());
        assert!(!activity.quiet_for(Duration::from_secs(5)));
    }

    #[test]
    fn measured_fullscreen_empty_composers_admit_one_row_notification() {
        for fixture in [FULLSCREEN, ASCII] {
            let tracker = measured(fixture);
            let evidence = tracker.empty(DriverKind::Grok, WAKE).unwrap();
            assert_eq!((evidence.row, evidence.start_col), (51, 6));
            assert!(evidence.max_bytes > WAKE.len());
            assert!(!tracker.echoed(DriverKind::Grok, WAKE, evidence));
            assert!(tracker.empty(DriverKind::Claude, WAKE).is_err());
            assert!(tracker.empty(DriverKind::Codex, WAKE).is_err());
            assert!(tracker.empty(DriverKind::GenericTerminal, WAKE).is_err());
        }
    }

    #[test]
    fn later_draft_modal_busy_shell_and_partial_redraw_are_not_startup_ready_forever() {
        for change in [
            "\x1b[?2026h\x1b[52;7Huser draft\x1b[?2026l",
            "\x1b[?2026h\x1b[55;1H\x1b[2K  Allow this tool? [y/n]\x1b[?2026l",
            "\x1b[?2026h\x1b[50;1HWorking... Esc to interrupt\x1b[52;7H\x1b[?2026l",
            "\x1b[?2026h\x1b[52;1H\x1b[2KPS C:\\workspace>\x1b[?2026l",
            "\x1b[?25l",
            "\x1b[?2026h\x1b[?2026h\x1b[?25h",
            "\x1b[?2026h\x1b[999z\x1b[?2026l",
            "\x1b[",
        ] {
            let mut tracker = measured(FULLSCREEN);
            tracker.observe(change);
            assert!(tracker.empty(DriverKind::Grok, WAKE).is_err(), "admitted {change:?}");
        }
        let mut tracker = measured(FULLSCREEN);
        tracker.resize(196, 55);
        assert!(tracker.empty(DriverKind::Grok, WAKE).is_err());
        tracker.begin_run();
        assert!(tracker.empty(DriverKind::Grok, WAKE).is_err());
    }

    #[test]
    fn submit_needs_exact_echo_in_a_new_completed_frame() {
        let mut tracker = measured(FULLSCREEN);
        let before = tracker.empty(DriverKind::Grok, WAKE).unwrap();
        tracker.observe(&format!("\x1b[?2026h\x1b[?2026h\x1b[52;7H{WAKE}\x1b[?25h"));
        assert!(!tracker.echoed(DriverKind::Grok, WAKE, before), "cursor-show does not finish sync redraw");
        tracker.observe("\x1b[?2026l");
        assert!(tracker.echoed(DriverKind::Grok, WAKE, before));
        assert!(!tracker.echoed(DriverKind::Grok, "different", before));
        assert!(tracker.empty(DriverKind::Grok, WAKE).is_err(), "pasted text is no longer empty");
        tracker.observe("\x1b[?2026h\x1b[55;1H\x1b[2KApprove command?\x1b[52;65H\x1b[?2026l");
        assert!(!tracker.echoed(DriverKind::Grok, WAKE, before));
    }

    #[test]
    fn wake_text_is_control_free_ascii_and_never_wraps_or_uses_unicode_width_guesses() {
        let tracker = measured(FULLSCREEN);
        for invalid in ["", "x\r", "x\n", "x\t", "x\x1b", "x\u{202e}", "\u{1f600}"] {
            assert!(tracker.empty(DriverKind::Grok, invalid).is_err());
        }
        let max = tracker.empty(DriverKind::Grok, WAKE).unwrap().max_bytes;
        assert!(tracker.empty(DriverKind::Grok, &"x".repeat(max)).is_ok());
        assert!(tracker.empty(DriverKind::Grok, &"x".repeat(max + 1)).is_err());
    }

    #[test]
    fn unmeasured_minimal_and_measured_existing_draft_remain_held() {
        for (fixture, cols, rows) in [
            (include_str!("../../driver-grok/src/fixtures/grok-startup-minimal-single-frame-v105.json"), 120, 30),
            (include_str!("../../driver-grok/src/fixtures/grok-startup-av.json"), 196, 22),
        ] {
            let mut tracker = RoomComposer::default();
            tracker.resize(cols, rows);
            tracker.observe(&serde_json::from_str::<String>(fixture).unwrap());
            assert!(tracker.empty(DriverKind::Grok, WAKE).is_err());
        }
    }

    #[test]
    fn busy_evidence_away_from_composer_needs_explicit_completion() {
        let mut tracker = measured(FULLSCREEN);
        tracker.observe("\x1b[?2026h\x1b[20;1HThink");
        tracker.observe("\x1b[0ming...\x1b[52;7H\x1b[?2026l");
        assert!(tracker.empty(DriverKind::Grok, WAKE).is_err());
        tracker.observe("\x1b[?2026h\x1b[21;1Hstill waiting\x1b[52;7H\x1b[?2026l");
        assert!(tracker.empty(DriverKind::Grok, WAKE).is_err());
        tracker.observe("\x1b[?2026h\x1b[22;1HWorked for 4s\x1b[52;7H\x1b[?2026l");
        assert!(tracker.empty(DriverKind::Grok, WAKE).is_ok());
    }
}
