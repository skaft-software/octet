//! A prompt injected into a running turn (steering or follow-up) is collapsed by
//! default so a long paste cannot consume the transcript's scrollable area;
//! Ctrl+O (the existing expansion toggle) reveals it in full.

use super::support::*;

use super::*;

fn steering_block(text: &str, injected: bool) -> TranscriptBlock {
    TranscriptBlock::User {
        text: text.to_owned(),
        model_lab: None,
        prompt_color: None,
        persisted: true,
        injected,
}
}

fn rendered(text: &str, injected: bool, width: u16, verbose: bool) -> Vec<String> {
    let theme = crate::tui::theme::test_theme();
    render_block(
        None,
        &steering_block(text, injected),
        &theme,
        &theme.rich_renderer(),
        &theme.reasoning_renderer(),
        width,
        verbose,
    )
    .into_iter()
    .map(|line| strip_terminal_sequences(&line))
    .collect()
}

fn long_prompt() -> String {
    (0..40)
        .map(|index| format!("steering line {index}: keep the transcript scrollable"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn injected_prompt_collapses_by_default_and_keeps_transcript_space() {
    let full = rendered(&long_prompt(), true, 100, true);
    let collapsed = rendered(&long_prompt(), true, 100, false);
    assert!(
        full.len() >= collapsed.len() + 10,
        "expanded prompt must keep its rows: full={} collapsed={}",
        full.len(),
        collapsed.len()
    );
    assert!(
        collapsed.len() <= 4,
        "collapsed injected prompt is at most three rows plus one Ctrl+O hint: {collapsed:?}"
    );
    assert!(
        collapsed.last().expect("hint row").contains("ctrl+o"),
        "the hint must name the expander: {collapsed:?}"
    );
    assert!(
        collapsed.last().expect("hint row").contains("lines"),
        "the hint must say how much is hidden: {collapsed:?}"
    );
}

#[test]
fn ctrl_o_expands_and_collapses_the_same_block() {
    let text = long_prompt();
    let collapsed = rendered(&text, true, 100, false);
    let expanded = rendered(&text, true, 100, true);
    assert!(expanded.len() > collapsed.len());
    // The toggle is the existing global expansion state, so both directions are
    // deterministic and re-rendering always agrees with it.
    assert_eq!(rendered(&text, true, 100, false), collapsed);
    assert_eq!(rendered(&text, true, 100, true), expanded);
}

#[test]
fn opening_prompt_is_never_collapsed() {
    let text = long_prompt();
    let typed = rendered(&text, false, 100, false);
    let expanded = rendered(&text, true, 100, true);
    assert_eq!(
        typed.len(),
        expanded.len(),
        "a prompt typed as the turn's opening message keeps every row"
    );
    assert!(!typed.iter().any(|line| line.contains("ctrl+o")));
}

#[test]
fn narrow_terminal_still_bounds_the_collapsed_prompt() {
    let text = long_prompt();
    for width in [20u16, 30, 40, 60] {
        let collapsed = rendered(&text, true, width, false);
        assert!(
            collapsed.len() <= 4,
            "width {width}: a collapsed injected prompt must stay within four rows: {collapsed:?}"
        );
        for line in &collapsed {
            assert!(
                visible_width(line) <= usize::from(width),
                "width {width}: row exceeds the terminal: {line:?}"
            );
        }
    }
}

#[test]
fn short_injected_prompt_is_untouched() {
    let short = "check the docs";
    let lines = rendered(short, true, 80, false);
    assert!(lines.iter().any(|line| line.contains(short)), "{lines:?}");
    assert!(!lines.iter().any(|line| line.contains("ctrl+o")), "{lines:?}");
}

#[test]
fn queued_steering_preview_is_bounded_in_fullscreen() {
    let mut shell = InteractiveShell::test_shell();
    let long = long_prompt();
    shell.queue_steering(&ComposedInput::from_text(long));
    shell.queue_steering(&ComposedInput::from_text("second queued steering".into()));
    let preview = crate::tui::view::input_overlays::render_pending_steering(&shell.state.borrow(), 120, 3);
    assert!(
        preview.len() <= 3,
        "the queued steering preview must respect its row budget: {preview:?}"
    );
    assert!(preview.iter().any(|line| line.contains("queued")), "{preview:?}");
}
