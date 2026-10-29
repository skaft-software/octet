use super::*;
use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

const TARGET: &str = "https://example.test/docs?x=1&y=2";

fn paint(shell: &mut InteractiveShell) -> Vec<String> {
    let width = shell.state.borrow().size.0;
    let threaded = shell.state.borrow().render_threaded;
    if threaded {
        shell.render_written_test_frame();
        shell
            .state
            .borrow()
            .pointer_render_geometry()
            .map_or_else(Vec::new, |geometry| geometry.visible_lines.clone())
    } else {
        render_shell_viewport_at(&shell.state.borrow(), width, Instant::now())
    }
}

fn fixture(threaded: bool, hyperlinks: bool) -> (InteractiveShell, u16, u16) {
    let mut capabilities = TerminalCapabilities::test(true, true, ColorDepth::TrueColor);
    capabilities.hyperlinks = hyperlinks;
    let theme = crate::tui::theme::test_theme_with(capabilities);
    assert_eq!(theme.capabilities().hyperlinks, hyperlinks);
    let mut shell = InteractiveShell::test_shell_with_theme(theme);
    shell.set_size(80, 20);
    shell.apply_edit(EditAction::Paste("kept draft".into()));
    {
        let mut state = shell.state.borrow_mut();
        state.application_viewport_requested = true;
        state.render_threaded = threaded;
        state.push_block(TranscriptBlock::Assistant(Box::new(
            AssistantBlock::finalized(format!("界é see [文docs]({TARGET}) now")),
        )));
    }
    if threaded {
        shell.isolate_native_test_renderer();
    }
    let rows = paint(&mut shell);
    let (row, col) = rows
        .iter()
        .enumerate()
        .find_map(|(row, line)| {
            (0..80).find_map(|col| {
                (sexy_tui_rs::hyperlink_at_column(line, col).as_deref() == Some(TARGET))
                    .then_some((row as u16, col as u16))
            })
        })
        .expect("painted link");
    (shell, row, col)
}

fn release(shell: &mut InteractiveShell, row: u16, col: u16) -> Vec<String> {
    let mut opened = Vec::new();
    shell.end_transcript_selection_with(row, col, |target| opened.push(target.to_owned()));
    opened
}

#[test]
fn noninteractive_link_rendering_remains_escape_free() {
    let theme = crate::tui::theme::test_theme_with(TerminalCapabilities::test(
        false,
        true,
        ColorDepth::None,
    ));
    let document = sexy_tui_rs::rich_text::markdown::parse(&format!("see [docs]({TARGET})"));
    for renderer in [theme.rich_renderer(), theme.reasoning_renderer()] {
        let rendered = renderer.render(&document, 100);
        assert!(rendered
            .lines
            .iter()
            .all(|line| !line.styled.contains('\x1b')));
        assert!(rendered.plain_lines().join("\n").contains(TARGET));
    }
}

#[test]
fn stationary_web_click_activates_once_after_release_and_survives_an_unchanged_paint() {
    for (threaded, hyperlinks) in [(false, false), (false, true), (true, false), (true, true)] {
        let (mut shell, row, col) = fixture(threaded, hyperlinks);
        assert!(release(&mut shell, row, col).is_empty());
        shell.begin_transcript_selection(row, col, false);
        assert_eq!(
            shell
                .state
                .borrow()
                .pending_link_press
                .as_ref()
                .unwrap()
                .target,
            TARGET
        );
        paint(&mut shell);
        assert_eq!(release(&mut shell, row, col), [TARGET]);
        assert!(release(&mut shell, row, col).is_empty());
        assert!(shell.state.borrow().transcript_selection.is_none());
        assert!(shell.state.borrow().copy_buffer.is_none());
        assert_eq!(shell.pending(), "kept draft");
    }
}

#[test]
fn dragging_a_link_never_opens_it_even_after_returning_to_the_press_cell() {
    for (threaded, hyperlinks) in [(false, false), (false, true), (true, false), (true, true)] {
        for movement in ["release only", "drag", "return", "wide cell"] {
            let (mut shell, row, col) = fixture(threaded, hyperlinks);
            shell.begin_transcript_selection(row, col, false);
            let release_col = match movement {
                "drag" => {
                    shell.extend_transcript_selection(row, col + 4);
                    col + 4
                }
                "return" => {
                    shell.extend_transcript_selection(row, col + 4);
                    shell.extend_transcript_selection(row, col);
                    col
                }
                "wide cell" => {
                    // Both columns of 文 map to the same semantic grapheme.
                    shell.extend_transcript_selection(row, col + 1);
                    col
                }
                _ => col + 4,
            };
            assert!(
                release(&mut shell, row, release_col).is_empty(),
                "{movement}"
            );
            if matches!(movement, "release only" | "drag") {
                assert!(shell.state.borrow().copy_buffer.is_some(), "{movement}");
            }
            assert_eq!(shell.pending(), "kept draft");
        }
    }
}

#[test]
fn shift_selection_and_presses_outside_the_link_cannot_activate_it() {
    for (threaded, hyperlinks) in [(false, false), (false, true), (true, false), (true, true)] {
        for (press_col, extend) in [(0, false), (0, true), (u16::MAX, false)] {
            let (mut shell, row, col) = fixture(threaded, hyperlinks);
            shell.begin_transcript_selection(row, press_col, extend);
            assert!(release(&mut shell, row, col).is_empty());
        }
        let (mut shell, row, col) = fixture(threaded, hyperlinks);
        shell.begin_transcript_selection(row, col, true);
        assert!(release(&mut shell, row, col).is_empty());
        assert!(shell.transcript_link_at_screen_cell(19, col).is_none());
        shell.begin_transcript_selection(19, col, false);
        assert!(release(&mut shell, row, col).is_empty());
    }
}

#[test]
fn input_and_layout_changes_revoke_a_pending_link_click() {
    for (threaded, hyperlinks) in [(false, false), (false, true), (true, false), (true, true)] {
        for change in [
            "resize", "wheel", "focus", "keyboard", "overlay", "output", "draft",
        ] {
            let (mut shell, row, col) = fixture(threaded, hyperlinks);
            shell.begin_transcript_selection(row, col, false);
            match change {
                "resize" => {
                    shell.set_size(79, 20);
                    shell.set_size(80, 20);
                }
                "wheel" => {
                    shell.translate_input(
                        Some(Event::Mouse(MouseEvent {
                            kind: MouseEventKind::ScrollUp,
                            row,
                            column: col,
                            modifiers: KeyModifiers::NONE,
                        })),
                        false,
                    );
                }
                "focus" => {
                    shell.translate_input(Some(Event::FocusLost), false);
                }
                "keyboard" => {
                    shell.translate_input(
                        Some(Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))),
                        false,
                    );
                }
                "overlay" => shell.show_report_text("Report", "Owner change", "body".into()),
                "output" => shell.notice("new transcript output"),
                "draft" => shell.apply_edit(EditAction::Char('x')),
                _ => unreachable!(),
            }
            paint(&mut shell);
            assert!(release(&mut shell, row, col).is_empty(), "{change}");
            assert!(shell.state.borrow().pending_link_press.is_none());
        }
    }
}

#[test]
fn a_press_without_painted_geometry_cannot_gain_link_authority_later() {
    let (mut shell, row, col) = fixture(true, false);
    {
        let mut state = shell.state.borrow_mut();
        state.render_geometry = None;
        state.painted_geometry = None;
    }
    shell.begin_transcript_selection(row, col, false);
    assert!(shell.state.borrow().pending_link_press.is_none());
    paint(&mut shell);
    assert!(release(&mut shell, row, col).is_empty());
}

#[test]
fn browser_handoff_only_accepts_valid_http_and_https_targets() {
    let shell = InteractiveShell::test_shell();
    let state = shell.state.borrow();
    for target in [
        TARGET,
        "http://127.0.0.1:1234/path",
        "HTTPS://example.test/docs",
    ] {
        assert!(TranscriptLinkPress::new(&state, 0, 0, target.into()).is_some());
    }
    for target in [
        "file:///tmp/executable",
        "mailto:person@example.test",
        "ssh://example.test",
        "git://example.test/repo",
        "javascript:alert(1)",
        "data:text/html,test",
        "https://",
        "https://example.test/\npage",
        "https://example.test/\x1bpage",
        "--help",
    ] {
        assert!(
            TranscriptLinkPress::new(&state, 0, 0, target.into()).is_none(),
            "{target:?}"
        );
    }
}
