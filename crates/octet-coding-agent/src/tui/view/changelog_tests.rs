//! Release notes reuse ordinary reports and the canonical rich renderer.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use sexy_tui_rs::{strip_terminal_sequences, visible_width};

use super::*;
use crate::tui::terminal::{ColorDepth, TerminalCapabilities};

fn rows(shell: &InteractiveShell) -> Vec<String> {
    let state = shell.state.borrow();
    let chrome = shell_chrome(&state, state.size.0, Instant::now());
    viewport::overlay_lines(&state, state.size.0, chrome.transcript_rows)
}

fn plain_rows(shell: &InteractiveShell) -> String {
    rows(shell)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn key(shell: &mut InteractiveShell, code: KeyCode) -> OverlayInputResult {
    shell.overlay_input(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

#[test]
fn changelog_renders_current_bundled_markdown_not_source_or_transcript() {
    for color in [
        ColorDepth::TrueColor,
        ColorDepth::Ansi256,
        ColorDepth::Ansi16,
        ColorDepth::None,
    ] {
        let theme =
            crate::tui::theme::test_theme_with(TerminalCapabilities::test(true, true, color));
        let mut shell = InteractiveShell::test_shell_with_theme(theme);
        shell.set_size(80, 24);
        shell.show_changelog();
        let rendered = rows(&shell);
        let plain = plain_rows(&shell);
        assert!(
            plain.contains(&format!("octet {}", env!("CARGO_PKG_VERSION"))),
            "{plain}"
        );
        assert!(plain.contains("Fixed"), "{plain}");
        assert!(
            !plain.contains("# octet") && !plain.contains("## Fixed"),
            "raw Markdown: {plain}"
        );
        if color == ColorDepth::None {
            assert!(rendered.iter().all(|line| !line.contains('\x1b')));
        }
        assert!(rendered.iter().all(|line| visible_width(line) <= 80));
        assert!(shell.state.borrow().transcript.is_empty());
        assert!(shell.debug_snapshot().is_empty());
    }
}

#[test]
fn changelog_rich_report_uses_existing_renderer_for_headings_lists_and_code() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 30);
    let source = "# Release heading\n\nA **bold** fix with `inline code`.\n\n- List item\n\n```rust\nlet release = true;\n```";
    let document = parse_markdown(source);
    shell.show_report(
        OrdinarySurfaceMetadata::with_purpose("Changelog", "Bundled release notes"),
        ReportBody::Markdown(document.clone()),
    );
    let state = shell.state.borrow();
    let layout = crate::tui::layout::PresentationLayout::new(&state.theme, 80);
    let expected = state
        .theme
        .rich_renderer()
        .render(&document, layout.content_width);
    let actual = viewport::overlay_lines(&state, 80, 25);
    for line in expected.lines {
        let expected = fit_line(
            &format!(
                "{}{line}",
                " ".repeat(usize::from(layout.inset)),
                line = line.styled
            ),
            80,
        );
        assert!(
            actual.contains(&expected),
            "rich row missing: {expected:?}\n{actual:?}"
        );
    }
    let plain = actual
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        plain.contains("Release heading")
            && plain.contains("List item")
            && plain.contains("let release = true;")
    );
    assert!(
        !plain.contains("**bold**")
            && !plain.contains("```rust")
            && !plain.contains("`inline code`")
    );
}

#[test]
fn changelog_scrolls_reflows_and_closes_without_editing_the_composer() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(46, 12);
    shell.apply_edit(EditAction::Paste("untouched draft".into()));
    shell.show_changelog();
    let first = plain_rows(&shell);
    assert!(first.contains(&format!("octet {}", env!("CARGO_PKG_VERSION"))));
    let maximum = viewport::report_scroll_metrics_for_state(&shell.state.borrow())
        .unwrap()
        .0;
    assert!(maximum > 0);
    for code in [KeyCode::Down, KeyCode::PageDown, KeyCode::End] {
        assert_eq!(key(&mut shell, code), OverlayInputResult::Consumed);
    }
    let tail = plain_rows(&shell);
    assert_ne!(first, tail);
    assert!(
        tail.contains("unchanged"),
        "End should reach the last release-note paragraph: {tail}"
    );
    let Some(ShellOverlay::Report(report)) = shell.state.borrow().overlay.clone() else {
        panic!("report closed")
    };
    assert_eq!(report.scroll_from_top, maximum);
    for (width, height) in [(24, 8), (46, 8), (80, 24), (120, 40), (8, 8)] {
        shell.set_size(width, height);
        let rendered = rows(&shell);
        assert!(rendered.len() <= usize::from(height));
        assert!(rendered
            .iter()
            .all(|line| visible_width(line) <= usize::from(width)));
        assert_eq!(key(&mut shell, KeyCode::End), OverlayInputResult::Consumed);
    }
    shell.set_size(46, 12);
    for code in [KeyCode::Up, KeyCode::PageUp, KeyCode::Home] {
        assert_eq!(key(&mut shell, code), OverlayInputResult::Consumed);
    }
    assert_eq!(plain_rows(&shell), first);
    assert_eq!(key(&mut shell, KeyCode::Left), OverlayInputResult::Closed);
    assert_eq!(shell.pending(), "untouched draft");
    assert!(shell.state.borrow().transcript.is_empty());
    shell.show_changelog();
    assert_eq!(key(&mut shell, KeyCode::Esc), OverlayInputResult::Closed);
}

#[test]
fn changelog_is_discoverable_in_the_inline_slash_surface() {
    let mut shell = InteractiveShell::test_shell();
    shell.apply_edit(EditAction::Paste("/chang".into()));
    let rendered = input_overlays::render_slash_suggestions(&shell.state.borrow(), 80, 4);
    let text = rendered
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("/changelog") && text.contains("release notes"),
        "{text}"
    );
}
