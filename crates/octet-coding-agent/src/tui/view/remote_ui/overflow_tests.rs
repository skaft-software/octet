use super::*;
use std::sync::Arc;

use crate::tui::extension_components::ExtensionComponentSurface;
use crate::tui::view::renderer_runtime::ShellFrameState;
use crate::tui::view::{native_scrollback, viewport, InteractiveShell, TranscriptBlock};
use sexy_tui_rs::strip_terminal_sequences;

const NOTICE: &str = "No active harness job. Run /harness start <requirement> to begin.";

fn projection(size: (u16, u16), widget_rows: usize, footer_rows: usize) -> Projection {
    let mut components = ExtensionComponentSurface::default();
    let mut mounts = Vec::new();
    for (id, placement, lines) in [
        ("header", Placement::Header, vec!["remote header".into()]),
        (
            "editor",
            Placement::Editor,
            vec![
                "editor top".into(),
                "editor draft".into(),
                "editor bottom".into(),
            ],
        ),
        (
            "footer",
            Placement::Footer,
            (0..footer_rows)
                .map(|row| format!("footer {row}"))
                .collect(),
        ),
        (
            "above",
            Placement::AboveEditor,
            (0..widget_rows)
                .map(|row| format!("warning {row}"))
                .collect(),
        ),
        ("below", Placement::BelowEditor, vec!["below widget".into()]),
    ] {
        components.register_component(id).unwrap();
        components.store_render(id, size.0, lines).unwrap();
        mounts.push(MountView {
            id: id.into(),
            surface_id: id.into(),
            title: id.into(),
            placement,
            columns: size.0,
            rows: size.1,
            mouse_capture: false,
            native_editor: false,
        });
    }
    Projection {
        components: Arc::new(components),
        mounts,
        chrome: None,
    }
}

fn repaint(
    shell: &InteractiveShell,
    application: bool,
    frame: &mut ShellFrameState,
    retained: &mut Vec<String>,
) -> Vec<String> {
    let state = shell.state.borrow();
    let update = if application {
        viewport::render_shell_viewport_update(&state, state.size.0, Instant::now(), frame)
    } else {
        native_scrollback::render_shell_update_without_cursor(
            &state,
            state.size.0,
            Instant::now(),
            frame,
        )
    };
    assert!(update.stable_prefix <= retained.len());
    retained.truncate(update.stable_prefix);
    retained.extend(update.replacement);
    retained[retained.len().saturating_sub(usize::from(state.size.1))..]
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect()
}

#[test]
fn remote_widget_overflow_keeps_real_notice_visible_after_repaints_in_both_policies() {
    for size in [(120, 40), (80, 24)] {
        for application in [false, true] {
            let mut shell = InteractiveShell::test_shell();
            shell.set_size(size.0, size.1);
            shell.set_remote_ui(projection(size, 1, 2));
            shell.notice(NOTICE);
            let mut frame = ShellFrameState::default();
            let mut retained = Vec::new();
            for widget_rows in [1, 64, 128, 64] {
                shell.set_remote_ui(projection(size, widget_rows, 2));
                let visible = repaint(&shell, application, &mut frame, &mut retained);
                let text = visible.join("\n");
                assert!(
                    text.contains(NOTICE),
                    "{size:?} application={application}: {text}"
                );
                for expected in [
                    "editor top",
                    "editor draft",
                    "editor bottom",
                    "below widget",
                    "footer 0",
                    "footer 1",
                ] {
                    assert!(text.contains(expected), "missing {expected}: {text}");
                }
                assert!(visible.last().unwrap().contains("footer 1"));
                assert!(visible.len() <= usize::from(size.1));
                let state = shell.state.borrow();
                assert_eq!(state.transcript.len(), 1);
                assert!(
                    matches!(&state.transcript[0], TranscriptBlock::Notice(text) if text == NOTICE)
                );
                assert!(shell_chrome(&state, size.0, Instant::now()).transcript_rows >= 2);
                let chrome = shell_chrome(&state, size.0, Instant::now());
                let capacity =
                    viewport::transcript_viewport_capacity(chrome.transcript_rows, false);
                let transcript = state.rendered_transcript(size.0);
                let tail = &transcript[transcript.len().saturating_sub(capacity)..];
                assert!(tail.iter().any(|line| line.contains(NOTICE)));
                assert!(tail
                    .last()
                    .is_some_and(|line| { strip_terminal_sequences(line).trim().is_empty() }));
            }
        }
    }
}

#[test]
fn remote_widget_overflow_reserves_extra_semantic_row_only_for_actual_history() {
    for size in [(120, 40), (80, 24)] {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(size.0, size.1);
        shell.set_remote_ui(projection(size, 128, 2));
        let without_history = {
            let state = shell.state.borrow();
            assert!(state.transcript.is_empty());
            shell_chrome(&state, size.0, Instant::now())
        };
        assert_eq!(without_history.transcript_rows, 1);
        shell.notice(NOTICE);
        let state = shell.state.borrow();
        let with_history = shell_chrome(&state, size.0, Instant::now());
        assert_eq!(with_history.transcript_rows, 3);
        assert_eq!(
            without_history.extension_above.len(),
            with_history.extension_above.len() + 2
        );
        assert_eq!(without_history.composer, with_history.composer);
        assert_eq!(
            without_history.extension_below,
            with_history.extension_below
        );
    }
}

#[test]
fn remote_widget_overflow_keeps_notice_after_empty_frame_and_resize() {
    for application in [false, true] {
        let mut shell = InteractiveShell::test_shell();
        let mut frame = ShellFrameState::default();
        let mut retained = Vec::new();
        shell.set_size(120, 40);
        shell.set_remote_ui(projection((120, 40), 128, 2));
        let empty = repaint(&shell, application, &mut frame, &mut retained);
        assert!(!empty.join("\n").contains(NOTICE));
        shell.notice(NOTICE);
        for size in [(120, 40), (80, 24), (120, 40)] {
            shell.set_size(size.0, size.1);
            shell.set_remote_ui(projection(size, 128, 2));
            let visible = repaint(&shell, application, &mut frame, &mut retained);
            let text = visible.join("\n");
            assert!(
                text.contains(NOTICE),
                "{size:?} application={application}: {text}"
            );
            assert!(text.contains("editor draft"));
            assert!(text.contains("below widget"));
            assert!(visible.last().unwrap().contains("footer 1"));
            let warnings = visible
                .iter()
                .filter(|line| line.starts_with("warning "))
                .collect::<Vec<_>>();
            assert_eq!(warnings.len(), usize::from(size.1) - 9);
            for (row, line) in warnings.into_iter().enumerate() {
                assert_eq!(line, &format!("warning {row}"));
            }
            assert!(visible.len() <= usize::from(size.1));
        }
    }
}

#[test]
fn remote_widget_overflow_keeps_tiny_height_bounds_and_footer_priority() {
    for height in 1..=8 {
        for application in [false, true] {
            for history in [false, true] {
                let size = (80, height);
                let mut shell = InteractiveShell::test_shell();
                shell.set_size(size.0, size.1);
                shell.set_remote_ui(projection(size, 128, 128));
                if history {
                    shell.notice(NOTICE);
                }
                let state = shell.state.borrow();
                let chrome = shell_chrome(&state, size.0, Instant::now());
                assert!(
                    super::super::shell_chrome::shell_chrome_rows(&chrome) <= usize::from(height)
                );
                assert!(chrome.extension_above.is_empty());
                assert!(chrome
                    .extension_below
                    .iter()
                    .all(|line| line.starts_with("footer ")));
                assert!(chrome.composer.len() <= usize::from(height.saturating_sub(1)));
                assert_eq!(chrome.transcript_rows, 1);
                drop(state);
                let visible = repaint(
                    &shell,
                    application,
                    &mut ShellFrameState::default(),
                    &mut Vec::new(),
                );
                assert!(visible.len() <= usize::from(height));
                if height >= 5 {
                    assert!(visible.iter().any(|line| line.contains("footer 0")));
                    assert!(visible.iter().any(|line| line.contains("editor draft")));
                }
            }
        }
    }
}
