//! Source Markdown clipboard contracts, through real terminal mouse dispatch.
use super::*;
use crate::tui::keymap::{InputAction, PointerGesture};
use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

fn fixture(source: &str) -> InteractiveShell {
    fixture_kind(source, false)
}

fn fixture_kind(source: &str, live: bool) -> InteractiveShell {
    clipboard_transport::TEST_COPY.with(|copy| *copy.borrow_mut() = None);
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(100, 40);
    shell.capture_mouse = true;
    shell.state.borrow_mut().application_viewport_requested = true;
    shell
        .state
        .borrow_mut()
        .push_block(TranscriptBlock::Assistant(Box::new(if live {
            AssistantBlock::streaming(source)
        } else {
            AssistantBlock::finalized(source.into())
        })));
    shell.isolate_native_test_renderer();
    shell.render_written_test_frame();
    shell
}

fn cell(shell: &InteractiveShell, needle: &str, occurrence: usize) -> (u16, u16) {
    let state = shell.state.borrow();
    let geometry = state.pointer_render_geometry().unwrap();
    geometry
        .visible_lines
        .iter()
        .enumerate()
        .flat_map(|(row, line)| {
            let plain = sexy_tui_rs::strip_terminal_sequences(line);
            plain
                .match_indices(needle)
                .map(|(offset, _)| {
                    (
                        row as u16,
                        sexy_tui_rs::visible_width(&plain[..offset]) as u16,
                    )
                })
                .collect::<Vec<_>>()
        })
        .nth(occurrence)
        .unwrap_or_else(|| panic!("missing rendered {needle:?}: {:?}", geometry.visible_lines))
}

fn mouse(shell: &mut InteractiveShell, kind: MouseEventKind, (row, column): (u16, u16)) {
    let action = shell.translate_input(
        Some(Event::Mouse(MouseEvent {
            kind,
            row,
            column,
            modifiers: KeyModifiers::NONE,
        })),
        true,
    );
    match action {
        InputAction::TranscriptPointer(PointerGesture::Begin { row, col, extend }) => {
            shell.begin_transcript_selection(row, col, extend)
        }
        InputAction::TranscriptPointer(PointerGesture::Extend { row, col }) => {
            shell.extend_transcript_selection(row, col)
        }
        InputAction::TranscriptPointer(PointerGesture::End { row, col }) => {
            shell.end_transcript_selection(row, col)
        }
        InputAction::Ignore => {}
        other => panic!("unexpected {other:?}"),
    }
}

fn select(shell: &mut InteractiveShell, a: (u16, u16), b: (u16, u16), expected: &str) {
    for reverse in [false, true] {
        let (a, b) = if reverse { (b, a) } else { (a, b) };
        mouse(shell, MouseEventKind::Down(MouseButton::Left), a);
        mouse(shell, MouseEventKind::Drag(MouseButton::Left), b);
        mouse(shell, MouseEventKind::Up(MouseButton::Left), b);
        clipboard_transport::TEST_COPY.with(|copy| {
            assert_eq!(
                copy.borrow().as_deref(),
                Some(expected),
                "reverse={reverse}"
            )
        });
    }
}

#[test]
fn markdown_source_copy_whole_constructs_keep_original_spelling() {
    for (source, label, expected) in [
        ("### Title here ###", "Title here", "### Title here ###"),
        ("before __strong__ after", "strong", "__strong__"),
        (
            "before **界e\u{301}👩‍💻** after",
            "界e\u{301}👩‍💻",
            "**界e\u{301}👩‍💻**",
        ),
        ("before *emphasis* after", "emphasis", "*emphasis*"),
        (
            "before ``code ` span`` after",
            "code ` span",
            "``code ` span``",
        ),
        (
            "before [label](https://example.test/path \"title\") after",
            "label",
            "[label](https://example.test/path \"title\")",
        ),
        ("+ list item", "list item", "+ list item"),
        ("7) ordered item", "ordered item", "7) ordered item"),
        (
            "```rust\nlet x = 1;\n```",
            "let x = 1;",
            "```rust\nlet x = 1;\n```",
        ),
    ] {
        let mut shell = fixture(source);
        let a = cell(&shell, label, 0);
        select(
            &mut shell,
            a,
            (a.0, a.1 + sexy_tui_rs::visible_width(label) as u16),
            expected,
        );
    }
}

#[test]
fn markdown_source_copy_partial_inline_constructs_omit_enclosing_syntax() {
    for (source, label, from, to, expected) in [
        ("before **strong** after", "strong", 1, 5, "tron"),
        (
            "before **界e\u{301}👩‍💻** after",
            "界e\u{301}👩‍💻",
            2,
            3,
            "e\u{301}",
        ),
        ("before `code span` after", "code span", 2, 6, "de s"),
        (
            "before [label](https://example.test) after",
            "label",
            1,
            4,
            "abe",
        ),
        ("before **strong** after", "strong after", 2, 9, "rong af"),
        (
            "a **outer *inner* end** z",
            "outer inner end",
            6,
            11,
            "*inner*",
        ),
    ] {
        let mut shell = fixture(source);
        let a = cell(&shell, label, 0);
        select(&mut shell, (a.0, a.1 + from), (a.0, a.1 + to), expected);
    }
}

#[test]
fn markdown_source_copy_partial_list_row_keeps_only_selected_marker_source() {
    let mut shell = fixture("399. MOUSE-ROW-399 clipboard line with unique words");
    shell.set_size(46, 12);
    shell.render_written_test_frame();
    let marker = cell(&shell, "399.", 0);
    let body = cell(&shell, "MOUSE-ROW-399", 0);
    let end = cell(&shell, "with", 0);
    select(
        &mut shell,
        marker,
        (end.0, end.1 + 4),
        "399. MOUSE-ROW-399 clipboard line with",
    );
    select(
        &mut shell,
        body,
        (end.0, end.1 + 4),
        "MOUSE-ROW-399 clipboard line with",
    );
    select(&mut shell, marker, (marker.0, marker.1 + 4), "399.");
}

#[test]
fn markdown_source_copy_partial_blocks_keep_interblock_whitespace_not_markers() {
    let mut shell = fixture("hello **world**\n\n## target end");
    let a = cell(&shell, "world", 0);
    let b = cell(&shell, "target", 0);
    select(&mut shell, (a.0, a.1 + 2), (b.0, b.1 + 3), "rld\n\ntar");
}

#[test]
fn markdown_source_copy_partial_fence_is_raw_code_and_full_fence_keeps_language() {
    let mut shell = fixture("~~~python\nfirst = 1\nsecond = 2\nthird = 3\n~~~");
    let a = cell(&shell, "first = 1", 0);
    let b = cell(&shell, "second = 2", 0);
    select(
        &mut shell,
        (a.0, a.1 + 2),
        (b.0, b.1 + 6),
        "rst = 1\nsecond",
    );
    let c = cell(&shell, "third = 3", 0);
    select(
        &mut shell,
        a,
        (c.0, c.1 + 9),
        "~~~python\nfirst = 1\nsecond = 2\nthird = 3\n~~~",
    );
}

#[test]
fn markdown_source_copy_table_rows_keep_pipes_only_for_whole_rows() {
    let mut shell =
        fixture("| Name | Value |\n| :--- | ---: |\n| **alpha** | `beta` |\n| gamma | delta |");
    let a = cell(&shell, "alpha", 0);
    let b = cell(&shell, "beta", 0);
    select(&mut shell, a, (b.0, b.1 + 4), "| **alpha** | `beta` |");
    select(&mut shell, (a.0, a.1 + 1), (a.0, a.1 + 4), "lph");
    select(&mut shell, (a.0, a.1 + 1), (b.0, b.1 + 2), "lpha\tbe");
    let head = cell(&shell, "Name", 0);
    let tail = cell(&shell, "delta", 0);
    select(
        &mut shell,
        head,
        (tail.0, tail.1 + 5),
        "| Name | Value |\n| :--- | ---: |\n| **alpha** | `beta` |\n| gamma | delta |",
    );
    let mut shell = fixture("| A | B |\n|---|---|\n| a\\|b | same |");
    let a = cell(&shell, "a|b", 0);
    select(&mut shell, a, (a.0, a.1 + 3), "a\\|b");
    let mut shell = fixture("| A | B | C |\n|---|---|---|\n| first | | last |");
    let a = cell(&shell, "first", 0);
    let b = cell(&shell, "last", 0);
    select(&mut shell, (a.0, a.1 + 1), (b.0, b.1 + 3), "irst\t\tlas");
}

#[test]
fn markdown_source_copy_wrapped_tables_keep_cell_offsets_on_reflow() {
    let mut shell = fixture("| A | B |\n|---|---|\n| first second third | east west |");
    for width in [100, 40, 15, 100] {
        shell.set_size(width, 40);
        shell.render_written_test_frame();
        let (third, third_end, west, west_end, first) = if width == 15 {
            let third = cell(&shell, "thi", 0);
            let end = cell(&shell, "rd", 0);
            let west = cell(&shell, "wes", 0);
            (
                third,
                (end.0, end.1 + 2),
                west,
                (west.0 + 1, west.1 + 1),
                cell(&shell, "fir", 0),
            )
        } else {
            let third = cell(&shell, "third", 0);
            let west = cell(&shell, "west", 0);
            (
                third,
                (third.0, third.1 + 5),
                west,
                (west.0, west.1 + 4),
                cell(&shell, "first", 0),
            )
        };
        select(&mut shell, third, third_end, "third");
        select(&mut shell, west, west_end, "west");
        {
            let state = shell.state.borrow();
            let selection = state.transcript_selection.as_ref().unwrap();
            let top = state.pointer_render_geometry().unwrap().visible_start;
            // `select` finishes with the reverse gesture.
            for (position, row) in [(selection.focus, west.0), (selection.anchor, west_end.0)] {
                assert_eq!(
                    transcript_selection::visual_line_for_transcript_position(&state, position),
                    Some(top + usize::from(row)),
                    "width={width}"
                );
            }
        }
        select(
            &mut shell,
            first,
            west_end,
            "| first second third | east west |",
        );
    }
}

#[test]
fn markdown_source_copy_narrow_table_labels_are_not_cell_source() {
    let mut shell = fixture("| A | B | C |\n|---|---|---|\n| **alpha** | beta | gamma |");
    shell.set_size(18, 40);
    shell.render_written_test_frame();
    let a = cell(&shell, "alpha", 0);
    let b = cell(&shell, "beta", 0);
    let c = cell(&shell, "gamma", 0);
    select(&mut shell, (a.0, a.1 + 1), (a.0, a.1 + 4), "lph");
    select(&mut shell, a, (b.0, b.1 + 4), "**alpha**\tbeta");
    select(
        &mut shell,
        a,
        (c.0, c.1 + 5),
        "| **alpha** | beta | gamma |",
    );
}

#[test]
fn markdown_source_copy_repeated_escaped_and_unicode_text_names_its_occurrence() {
    let mut shell = fixture(
        "**same** then _same_ then [same](https://example.test) and \\*literal\\* &amp; 界é",
    );
    for (i, expected) in ["**same**", "_same_", "[same](https://example.test)"]
        .iter()
        .enumerate()
    {
        let a = cell(&shell, "same", i);
        select(&mut shell, a, (a.0, a.1 + 4), expected);
    }
    let a = cell(&shell, "*literal* & 界é", 0);
    select(&mut shell, a, (a.0, a.1 + 15), "\\*literal\\* &amp; 界é");
}

#[test]
fn markdown_source_copy_mixed_blocks_preserve_order_and_nonassistant_text() {
    let mut shell = fixture("## Middle **answer**");
    shell
        .state
        .borrow_mut()
        .push_block(TranscriptBlock::Notice("notice *literal*".into()));
    shell.render_written_test_frame();
    let first = cell(&shell, "Middle", 0);
    let last = cell(&shell, "notice *literal*", 0);
    select(
        &mut shell,
        first,
        (last.0, last.1 + 16),
        "## Middle **answer**\n\nnotice *literal*",
    );
}

#[test]
fn markdown_source_copy_mixed_streaming_release_uses_painted_source() {
    let mut shell = fixture_kind("**selected**", true);
    shell
        .state
        .borrow_mut()
        .push_block(TranscriptBlock::Notice("notice".into()));
    shell.render_written_test_frame();
    let a = cell(&shell, "selected", 0);
    let b = cell(&shell, "notice", 0);
    mouse(&mut shell, MouseEventKind::Down(MouseButton::Left), a);
    mouse(
        &mut shell,
        MouseEventKind::Drag(MouseButton::Left),
        (b.0, b.1 + 6),
    );
    {
        let mut state = shell.state.borrow_mut();
        let TranscriptBlock::Assistant(block) = &mut state.transcript[0] else {
            panic!()
        };
        block.append("\n\nunpainted output");
        state.touch_block(0);
    }
    mouse(
        &mut shell,
        MouseEventKind::Up(MouseButton::Left),
        (b.0, b.1 + 6),
    );
    clipboard_transport::TEST_COPY
        .with(|copy| assert_eq!(copy.borrow().as_deref(), Some("**selected**\n\nnotice")));
}

#[test]
fn markdown_source_copy_streaming_and_reflow_preserve_or_explicitly_clear_range() {
    let mut shell = fixture_kind("prefix **selected** suffix\n\n", true);
    let a = cell(&shell, "selected", 0);
    mouse(&mut shell, MouseEventKind::Down(MouseButton::Left), a);
    mouse(
        &mut shell,
        MouseEventKind::Drag(MouseButton::Left),
        (a.0, a.1 + 8),
    );
    {
        let mut state = shell.state.borrow_mut();
        let TranscriptBlock::Assistant(block) = &mut state.transcript[0] else {
            panic!()
        };
        block.append("\n\nmore output below");
        state.touch_block(0);
    }
    shell.set_size(60, 40);
    shell.render_written_test_frame();
    assert_eq!(shell.selected_plain_text().as_deref(), Some("**selected**"));
    let mut shell = fixture_kind("prefix *selected", true);
    let a = cell(&shell, "selected", 0);
    mouse(&mut shell, MouseEventKind::Down(MouseButton::Left), a);
    mouse(
        &mut shell,
        MouseEventKind::Drag(MouseButton::Left),
        (a.0, a.1 + 8),
    );
    {
        let mut state = shell.state.borrow_mut();
        let TranscriptBlock::Assistant(block) = &mut state.transcript[0] else {
            panic!()
        };
        block.append("*\n\n");
        state.touch_block(0);
    }
    shell.render_written_test_frame();
    assert!(shell.state.borrow().transcript_selection.is_none());
    assert!(shell
        .state
        .borrow()
        .copy_notice
        .as_ref()
        .unwrap()
        .0
        .contains("selection"));
}

#[test]
fn markdown_source_copy_pointer_work_is_independent_of_message_size() {
    for rows in [100, 10_000] {
        let source = (0..rows)
            .map(|i| format!("row {i:05} **token**\n\n"))
            .collect::<String>();
        let mut shell = fixture(&source);
        let a = cell(&shell, "token", 2);
        let _ = transcript_selection::take_copy_mapping_work();
        assert!(markdown_source::take_source_mapping_work() >= source.len());
        for _ in 0..30 {
            mouse(&mut shell, MouseEventKind::Down(MouseButton::Left), a);
            mouse(
                &mut shell,
                MouseEventKind::Drag(MouseButton::Left),
                (a.0, a.1 + 5),
            );
        }
        mouse(
            &mut shell,
            MouseEventKind::Up(MouseButton::Left),
            (a.0, a.1 + 5),
        );
        assert_eq!(transcript_selection::take_copy_mapping_work(), (0, 0));
        assert_eq!(markdown_source::take_source_mapping_work(), 0);
        clipboard_transport::TEST_COPY
            .with(|copy| assert_eq!(copy.borrow().as_deref(), Some("**token**")));
    }
}
