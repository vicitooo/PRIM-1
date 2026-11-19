//! Review regression for W2 choice 13: transcript text is not a live timer.
//! Reuses the production viewport and public clean-screen API; no harness runs.
use driver_codex::is_clean_prompt_screen;
use terminal_viewport::TerminalViewport;

fn clean_with_nearest_transcript(text: &str) -> bool {
    let mut viewport = TerminalViewport::default();
    let frame = format!(
        "\x1b[?25l\x1b[2J\x1b[H{text}\x1b[5;1H› Summarize recent commits\x1b[7;1H  gpt-6-astra high · ~\\mywork\x1b[5;3H\x1b[?25h"
    );
    for ch in frame.chars() {
        viewport.observe_character(ch);
    }
    is_clean_prompt_screen(&viewport.trusted_screen().expect("fully painted fixture"))
}

#[test]
fn otherwise_identical_idle_screen_is_clean() {
    assert!(clean_with_nearest_transcript("Finished reviewing the example."));
}

#[test]
fn plain_timer_shaped_transcript_does_not_veto_idle() {
    assert!(clean_with_nearest_transcript("Working (3s • esc to interrupt)"),
        "plain transcript text lacks the claimed measured spinner evidence");
}

#[test]
fn quoted_timer_shaped_transcript_does_not_veto_idle() {
    for text in ["> Working (3s • esc to interrupt)", "`Working (3s • esc to interrupt)`"] {
        assert!(clean_with_nearest_transcript(text), "quoted transcript vetoed idle: {text:?}");
    }
}
