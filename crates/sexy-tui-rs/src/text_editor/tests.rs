//! Unit tests for `crate::text_editor`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `text_editor.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::text_editor`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

fn assert_layout_invariants(text: &str, layout: &TextEditorLayout) {
    assert!(!layout.lines().is_empty());
    assert!(layout.cursor_row() < layout.lines().len());
    assert!(is_grapheme_boundary(text, layout.cursor_offset()));
    for line in layout.lines() {
        assert!(line.start() <= line.visible_end());
        assert!(line.visible_end() <= line.end());
        assert!(line.end() <= text.len());
        for offset in [line.start(), line.visible_end(), line.end()] {
            assert!(
                is_grapheme_boundary(text, offset),
                "{text:?} had non-grapheme layout offset {offset}"
            );
        }
    }
}

fn projection_rows(text: &str, projection: &TextEditorProjection, marker: &str) -> Vec<String> {
    projection
        .lines()
        .iter()
        .enumerate()
        .map(|(row, _)| {
            let line = projection.line(text, row).unwrap();
            if row == projection.cursor().row() {
                let offset = projection.cursor().offset();
                let visual = &projection.lines()[row];
                format!(
                    "{}{}{}",
                    &text[visual.start()..offset],
                    marker,
                    &text[offset..visual.visible_end()]
                )
            } else {
                line.to_owned()
            }
        })
        .collect()
}

#[test]
fn user_range_edits_preserve_history_and_reject_invalid_ranges() {
    let mut editor = TextEditor::with_text("a🦀e\u{301}");
    let original = editor.text().to_owned();
    assert!(editor.edit_range(1..5, "雪", 4));
    assert_eq!(editor.text(), "a雪e\u{301}");
    assert!(editor.cursor_is_valid());
    // Byte offsets inside UTF-8 or a combining grapheme must not change undo.
    assert!(!editor.edit_range(2..3, "x", 3));
    assert!(!editor.edit_range(4..5, "x", 5));
    assert!(!editor.edit_range(0..100, "x", 1));
    assert!(editor.edit_range(0..0, "", 0));
    assert!(editor.apply(TextEditAction::Undo, 80));
    assert_eq!(editor.text(), original);
    // A caret move after undo must not clear the redo branch.
    assert!(editor.edit_range(0..0, "", 1));
    assert!(editor.apply(TextEditAction::Redo, 80));
    assert_eq!(editor.text(), "a雪e\u{301}");
    assert_eq!(editor.cursor(), 0);
}

#[test]
fn empty_text_and_boundary_actions_keep_a_valid_cursor() {
    let mut editor = TextEditor::new();
    for action in [
        TextEditAction::Backspace,
        TextEditAction::Delete,
        TextEditAction::Left,
        TextEditAction::Right,
        TextEditAction::Up,
        TextEditAction::Down,
        TextEditAction::Home,
        TextEditAction::End,
    ] {
        assert!(!editor.apply(action, 0));
        assert!(editor.cursor_is_valid());
        assert_eq!(editor.cursor(), 0);
    }
    let projection = editor.projection(0);
    assert_eq!(projection.line(editor.text(), 0), Some(""));
    assert_eq!(projection.cursor().row(), 0);
    assert_eq!(projection.cursor().column(), 0);
}

#[test]
fn paste_normalizes_crlf_and_newline_is_an_editable_hard_boundary() {
    let mut editor = TextEditor::with_text("a");
    editor.apply(TextEditAction::Paste("b\r\nc\rd".into()), 80);
    editor.apply(TextEditAction::Newline, 80);
    assert_eq!(editor.text(), "ab\nc\nd\n");
    assert_eq!(editor.cursor(), editor.text().len());
    assert_eq!(
        editor
            .layout(80)
            .lines()
            .iter()
            .map(|line| &editor.text()[line.start()..line.visible_end()])
            .collect::<Vec<_>>(),
        vec!["ab", "c", "d", ""]
    );
}

#[test]
fn grapheme_navigation_and_deletion_do_not_split_combining_or_emoji_text() {
    let text = "a e\u{301} 👩‍💻 界";
    let mut editor = TextEditor::with_text(text);
    let original_len = editor.text().len();

    editor.apply(TextEditAction::Left, 80);
    assert_eq!(&editor.text()[editor.cursor()..], "界");
    editor.apply(TextEditAction::Delete, 80);
    assert_eq!(editor.text(), "a e\u{301} 👩‍💻 ");
    assert!(editor.cursor_is_valid());

    editor.apply(TextEditAction::Backspace, 80);
    editor.apply(TextEditAction::Backspace, 80);
    assert_eq!(editor.text(), "a e\u{301} ");
    assert!(editor.cursor_is_valid());
    assert!(editor.text().len() < original_len);

    editor.set_text("e\u{301}");
    editor.apply(TextEditAction::Backspace, 80);
    assert!(editor.is_empty());
    assert_eq!(editor.cursor(), 0);
}

#[test]
fn suffix_joining_edits_ceil_the_cursor_without_changing_set_cursor_flooring() {
    let cases = [
        ("\u{301}x", "a\u{301}bx"),
        ("\u{fe0f}x", "❤\u{fe0f}bx"),
        ("🇧x", "🇦🇧bx"),
        ("\u{200d}💻x", "👩\u{200d}💻bx"),
    ];
    for (suffix, expected) in cases {
        let mut editor = TextEditor::with_text(suffix);
        editor.apply(TextEditAction::Home, 80);
        let first = match suffix {
            "🇧x" => TextEditAction::Paste("🇦".into()),
            "\u{200d}💻x" => TextEditAction::Paste("👩".into()),
            "\u{fe0f}x" => TextEditAction::Char('❤'),
            _ => TextEditAction::Char('a'),
        };
        assert!(editor.apply(first, 80), "{suffix:?}");
        assert!(editor.cursor_is_valid(), "{suffix:?}");
        assert!(editor.apply(TextEditAction::Char('b'), 80), "{suffix:?}");
        assert_eq!(editor.text(), expected, "{suffix:?}");
        assert!(editor.cursor_is_valid(), "{suffix:?}");
    }

    let mut replacement = TextEditor::with_text("\u{301}x");
    replacement.set_cursor(0);
    assert!(replacement.replace_range(0..0, "a"));
    assert!(replacement.apply(TextEditAction::Char('b'), 80));
    assert_eq!(replacement.text(), "a\u{301}bx");

    let mut floor = TextEditor::with_text("e\u{301}");
    floor.set_cursor(1);
    assert_eq!(floor.cursor(), 0, "set_cursor remains a floor operation");
}

#[test]
fn visual_layout_wraps_words_without_losing_source_ranges() {
    let editor = TextEditor::with_text("alpha beta gamma");
    let layout = editor.layout(10);
    let visible = layout
        .lines()
        .iter()
        .map(|line| &editor.text()[line.start()..line.visible_end()])
        .collect::<Vec<_>>();
    assert_eq!(visible, vec!["alpha beta", "gamma"]);
    assert_eq!(
        layout
            .lines()
            .iter()
            .map(|line| &editor.text()[line.start()..line.end()])
            .collect::<String>(),
        editor.text()
    );
    assert_layout_invariants(editor.text(), &layout);
}

#[test]
fn structured_projection_never_confuses_source_with_a_cursor_marker() {
    let editor = TextEditor::with_text("source <cursor> token");
    let projection = editor.projection(80);
    assert_eq!(
        projection.line(editor.text(), 0),
        Some("source <cursor> token")
    );
    assert_eq!(projection.cursor().offset(), editor.text().len());
    let rows = projection_rows(editor.text(), &projection, "<cursor>");
    assert_eq!(rows, ["source <cursor> token<cursor>"]);
    assert_eq!(
        projection.cursor_parts(editor.text()),
        Some(("source <cursor> token", ""))
    );

    let marker_source = format!("source {} token", crate::CURSOR_MARKER);
    let editor = TextEditor::with_text(marker_source.clone());
    let projection = editor.projection(80);
    assert_eq!(
        projection.line(editor.text(), 0),
        Some(marker_source.as_str())
    );
    assert_eq!(
        projection.cursor_parts(editor.text()),
        Some((marker_source.as_str(), ""))
    );
}

#[test]
fn layout_and_projection_handle_zeroish_widths_wide_cells_and_cursor_coordinates() {
    let mut editor = TextEditor::with_text("e\u{301}界👩‍💻 alpha");
    editor.set_cursor("e\u{301}界".len());
    for width in 0..=4 {
        let layout = editor.layout(width);
        assert_layout_invariants(editor.text(), &layout);
        let projection = editor.projection(width);
        assert_eq!(projection.cursor().row(), layout.cursor_row());
        assert!(projection.cursor().offset() <= editor.text().len());
        assert!(is_grapheme_boundary(
            editor.text(),
            projection.cursor().offset()
        ));
        assert!(projection
            .line(editor.text(), projection.cursor().row())
            .is_some());
    }
}

#[test]
fn cursor_only_layouts_share_rows_and_keep_a_separate_text_revision() {
    let mut editor = TextEditor::with_text("alpha beta gamma");
    let first = editor.layout(6);
    let revision = editor.revision();
    let text_revision = editor.text_revision();

    editor.set_cursor(0);
    let second = editor.layout(6);
    assert!(Arc::ptr_eq(&first.lines, &second.lines));
    assert_eq!(editor.revision(), revision + 1);
    assert_eq!(editor.text_revision(), text_revision);

    let projection = TextEditor::projection_for_layout(editor.text(), &second, 5);
    assert!(Arc::ptr_eq(&second.lines, &projection.layout().lines));
    assert_eq!(projection.cursor().offset(), 5);

    assert!(editor.apply(TextEditAction::Char('!'), 6));
    let changed = editor.layout(6);
    assert!(!Arc::ptr_eq(&second.lines, &changed.lines));
    assert_eq!(editor.text_revision(), text_revision + 1);
}

#[test]
fn vertical_movement_keeps_the_preferred_cell_column_across_short_rows() {
    let mut editor = TextEditor::with_text("012345\nxy\nabcdefghi");
    editor.set_cursor(5);
    editor.apply(TextEditAction::Down, 80);
    assert_eq!(editor.cursor(), "012345\nxy".len());
    editor.apply(TextEditAction::Down, 80);
    assert_eq!(editor.cursor(), "012345\nxy\nabcde".len());

    let mut cells = TextEditor::with_text("界界a\nq\n界界abc");
    cells.set_cursor("界界".len());
    cells.apply(TextEditAction::Down, 80);
    assert_eq!(cells.cursor(), "界界a\nq".len());
    cells.apply(TextEditAction::Down, 80);
    assert_eq!(cells.cursor(), "界界a\nq\n界界".len());
}

#[test]
fn static_projection_visual_targets_share_one_layout() {
    let text = "abcdef\nxy\nabcdefgh";
    let projection = TextEditor::projection_for(text, 5, 80);
    let mut preferred = None;
    let target = projection
        .visual_target(text, &TextEditAction::Down, &mut preferred)
        .unwrap();
    assert_eq!(target, "abcdef\nxy".len());
    let projection = TextEditor::projection_for(text, target, 80);
    let target = projection
        .visual_target(text, &TextEditAction::Down, &mut preferred)
        .unwrap();
    assert_eq!(target, "abcdef\nxy\nabcde".len());
    let projection = TextEditor::projection_for(text, target, 80);
    let home = projection
        .visual_target(text, &TextEditAction::Home, &mut preferred)
        .unwrap();
    assert_eq!(home, "abcdef\nxy\n".len());
    assert_eq!(preferred, None);
}

#[test]
fn soft_wraps_and_resize_keep_the_same_source_cursor_affinity() {
    let mut editor = TextEditor::with_text("abcdefghij");
    editor.set_cursor(7);
    assert_eq!(editor.layout(3).cursor_row(), 2);
    assert_eq!(editor.layout(5).cursor_row(), 1);

    editor.apply(TextEditAction::Up, 3);
    assert_eq!(editor.cursor(), 4);
    editor.apply(TextEditAction::Down, 5);
    assert_eq!(editor.cursor(), 6);
}

#[test]
fn replacement_and_cursor_setters_cannot_create_invalid_positions() {
    let mut editor = TextEditor::with_text("e\u{301}界");
    editor.set_cursor(1);
    assert_eq!(editor.cursor(), 0);
    editor.set_cursor(usize::MAX);
    assert_eq!(editor.cursor(), editor.text().len());
    assert!(!editor.replace_range(1..2, "x"));
    assert!(editor.replace_range(0.."e\u{301}".len(), "z"));
    assert_eq!(editor.text(), "z界");
    assert!(editor.cursor_is_valid());
}

#[test]
fn home_end_and_soft_wrap_separators_follow_visual_rows() {
    let mut editor = TextEditor::with_text("alpha beta");
    let layout = editor.layout(6);
    assert_eq!(
        layout
            .lines()
            .iter()
            .map(|line| &editor.text()[line.start()..line.visible_end()])
            .collect::<Vec<_>>(),
        ["alpha", "beta"]
    );

    editor.apply(TextEditAction::Home, 6);
    assert_eq!(editor.cursor(), "alpha ".len());
    editor.apply(TextEditAction::End, 6);
    assert_eq!(editor.cursor(), editor.text().len());

    editor.set_cursor("alpha".len());
    editor.apply(TextEditAction::Right, 6);
    assert_eq!(editor.cursor(), "alpha ".len());
    assert!(editor.cursor_is_valid());
}

#[test]
fn raw_crlf_text_keeps_cursor_and_layout_on_grapheme_boundaries() {
    let mut editor = TextEditor::with_text("a\r\nb");
    let layout = editor.layout(8);
    assert_eq!(
        layout
            .lines()
            .iter()
            .map(|line| &editor.text()[line.start()..line.visible_end()])
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_layout_invariants(editor.text(), &layout);

    editor.set_cursor(1);
    editor.apply(TextEditAction::Right, 8);
    assert_eq!(editor.cursor(), 3);
    editor.apply(TextEditAction::Left, 8);
    assert_eq!(editor.cursor(), 1);
    assert!(editor.cursor_is_valid());
}

#[test]
fn static_projection_clamps_untrusted_offsets_without_mutating_source() {
    let text = "e\u{301}界";
    let projection = TextEditor::projection_for(text, 1, 1);
    assert_eq!(projection.cursor().offset(), 0);
    assert_eq!(text, "e\u{301}界");
    assert_layout_invariants(text, projection.layout());
}

#[test]
fn deterministic_action_matrix_preserves_cursor_and_layout_invariants() {
    let sources = [
        "",
        "ascii",
        "e\u{301}",
        "界界",
        "👩‍💻x",
        "one two three",
        "one\ntwo\n",
        "\r\n",
        "a\tb",
        "\x1b\u{301}x",
    ];
    let widths = [0, 1, 2, 3, 7, 80];
    for source in sources {
        for width in widths {
            let mut editor = TextEditor::with_text(source);
            for action in [
                TextEditAction::Left,
                TextEditAction::Right,
                TextEditAction::Up,
                TextEditAction::Down,
                TextEditAction::Home,
                TextEditAction::End,
                TextEditAction::Backspace,
                TextEditAction::Delete,
                TextEditAction::Char('界'),
                TextEditAction::Paste("\r\ne\u{301}👩‍💻".into()),
                TextEditAction::Newline,
            ] {
                editor.apply(action, width);
                assert!(editor.cursor_is_valid(), "{source:?} at width {width}");
                let projection = editor.projection(width);
                assert_layout_invariants(editor.text(), projection.layout());
                assert!(projection
                    .line(editor.text(), projection.cursor().row())
                    .is_some());
            }
        }
    }
}

#[test]
fn deterministic_randomized_unicode_edits_and_layouts_stay_safe() {
    const FRAGMENTS: [&str; 12] = [
        "a", "界", "e\u{301}", "\u{fe0f}", "🇦", "👩", "\u{200d}", "💻", "\t", "\x1b", "\r\n", "\n",
    ];
    let mut seed = 0x5eed_cafe_u64;
    let mut editor = TextEditor::new();
    for _ in 0..4_000 {
        if editor.text().len() > 128 {
            editor.clear();
        }
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let width = ((seed >> 8) as usize % 12).max(1);
        let action = match seed % 11 {
            0 => TextEditAction::Left,
            1 => TextEditAction::Right,
            2 => TextEditAction::Up,
            3 => TextEditAction::Down,
            4 => TextEditAction::Home,
            5 => TextEditAction::End,
            6 => TextEditAction::Backspace,
            7 => TextEditAction::Delete,
            8 => TextEditAction::Newline,
            9 => TextEditAction::Char('界'),
            _ => TextEditAction::Paste(
                FRAGMENTS[((seed >> 16) as usize) % FRAGMENTS.len()].to_owned(),
            ),
        };
        editor.apply(action, width);
        assert!(editor.text().is_char_boundary(editor.cursor()));
        assert!(editor.cursor_is_valid());
        let projection = editor.projection(width);
        assert_layout_invariants(editor.text(), projection.layout());
        let cursor = projection.cursor();
        assert!(cursor.row() < projection.lines().len());
        assert!(is_grapheme_boundary(editor.text(), cursor.offset()));
    }
}

#[test]
fn large_draft_deletions_keep_undo_and_redo_within_byte_budgets() {
    let mut editor = TextEditor::with_text("x".repeat(65_536));
    for _ in 0..2048 {
        editor.apply(TextEditAction::Backspace, 80);
    }
    assert!(
        editor.undo.len() < 70,
        "history must be byte-bounded, not one full draft per deletion"
    );
    let mut undo_bytes = 0;
    let mut history = editor.undo.clone();
    while let Some(snapshot) = history.pop() {
        undo_bytes += snapshot.text.capacity() + std::mem::size_of::<EditorSnapshot>();
    }
    assert!(undo_bytes <= HISTORY_BYTES);
    while editor.undo() {}
    let mut redo_bytes = 0;
    let mut history = editor.redo.clone();
    while let Some(snapshot) = history.pop() {
        redo_bytes += snapshot.text.capacity() + std::mem::size_of::<EditorSnapshot>();
    }
    assert!(redo_bytes <= HISTORY_BYTES);
    while editor.redo() {}
    assert_eq!(editor.text().len(), 65_536 - 2048);
}

#[test]
fn fish_style_undo_coalesces_word_runs_and_splits_on_space() {
    let mut editor = TextEditor::new();
    for character in "hello world".chars() {
        assert!(editor.apply(TextEditAction::Char(character), 80));
    }
    assert_eq!(editor.text(), "hello world");

    assert!(editor.apply(TextEditAction::Undo, 80));
    assert_eq!(editor.text(), "hello");
    assert!(editor.apply(TextEditAction::Undo, 80));
    assert_eq!(editor.text(), "");
    assert!(!editor.apply(TextEditAction::Undo, 80));
    assert_eq!(editor.cursor(), 0);
}

#[test]
fn undo_and_redo_round_trip_and_new_edits_clear_redo() {
    let mut editor = TextEditor::with_text("abc");
    assert!(editor.apply(TextEditAction::Char('d'), 80));
    assert_eq!(editor.text(), "abcd");

    assert!(editor.apply(TextEditAction::Undo, 80));
    assert_eq!(editor.text(), "abc");
    assert_eq!(editor.cursor(), 3);
    assert!(editor.apply(TextEditAction::Redo, 80));
    assert_eq!(editor.text(), "abcd");
    assert_eq!(editor.cursor(), 4);

    assert!(editor.apply(TextEditAction::Undo, 80));
    assert!(editor.apply(TextEditAction::Char('z'), 80));
    assert_eq!(editor.text(), "abcz");
    assert!(!editor.apply(TextEditAction::Redo, 80));
}

#[test]
fn bounded_history_preserves_recent_unicode_pastes_and_cursor() {
    let mut editor = TextEditor::new();
    let atom = "👩🏽‍💻e\u{301}界[image:1]";
    for _ in 0..HISTORY_COUNT + 10 {
        assert!(editor.apply(TextEditAction::Paste(atom.into()), 80));
    }
    assert_eq!(editor.undo.len(), HISTORY_COUNT);
    assert!(editor.undo.retained_bytes() <= HISTORY_BYTES);
    for remaining in (10..HISTORY_COUNT + 10).rev() {
        assert!(editor.apply(TextEditAction::Undo, 80));
        assert_eq!(editor.text(), atom.repeat(remaining));
        assert_eq!(editor.cursor(), editor.text().len());
    }
    assert!(!editor.apply(TextEditAction::Undo, 80));
    for _ in 0..HISTORY_COUNT {
        assert!(editor.apply(TextEditAction::Redo, 80));
    }
    assert_eq!(editor.text(), atom.repeat(HISTORY_COUNT + 10));
    assert!(!editor.apply(TextEditAction::Redo, 80));
}

#[test]
fn history_bytes_evict_oldest_and_oversize_is_an_undo_barrier() {
    let mut editor = TextEditor::with_text("界".repeat(HISTORY_BYTES / 12));
    for _ in 0..12 {
        editor.apply(TextEditAction::Paste("界".into()), 80);
    }
    assert!(editor.undo.len() < 12);
    assert!(editor.undo.retained_bytes() <= HISTORY_BYTES);
    assert!(editor.undo());
    assert!(editor.redo.retained_bytes() <= HISTORY_BYTES);
    editor.set_text("x".repeat(HISTORY_BYTES));
    editor.apply(TextEditAction::Paste("界".into()), 80);
    assert!(editor.undo.is_empty());
    assert!(!editor.undo());
}

#[test]
fn kill_ring_yank_and_yank_pop_cycle_entries() {
    let mut editor = TextEditor::with_text("one two");
    // Kill "two", then break accumulation with a motion, then kill "one".
    editor.set_cursor("one ".len());
    assert!(editor.apply(TextEditAction::DeleteToLineEnd, 80));
    assert_eq!(editor.text(), "one ");
    assert!(editor.apply(TextEditAction::Left, 80));
    assert!(editor.apply(TextEditAction::DeleteToLineStart, 80));
    assert_eq!(editor.text(), " ");

    assert!(editor.apply(TextEditAction::Yank, 80));
    assert_eq!(editor.text(), "one ");
    assert!(editor.apply(TextEditAction::YankPop, 80));
    assert_eq!(editor.text(), "two ");
    assert!(editor.apply(TextEditAction::YankPop, 80));
    assert_eq!(editor.text(), "one ");
}

#[test]
fn consecutive_word_kills_accumulate_into_one_entry() {
    let mut editor = TextEditor::with_text("alpha beta gamma");
    assert!(editor.apply(TextEditAction::DeleteWordBackward, 80));
    assert_eq!(editor.text(), "alpha beta ");
    assert!(editor.apply(TextEditAction::DeleteWordBackward, 80));
    assert_eq!(editor.text(), "alpha ");
    assert!(editor.apply(TextEditAction::DeleteWordBackward, 80));
    assert_eq!(editor.text(), "");

    // The accumulated run re-yanks in source order as one entry.
    assert!(editor.apply(TextEditAction::Yank, 80));
    assert_eq!(editor.text(), "alpha beta gamma");
}

#[test]
fn word_moves_and_jumps_stay_on_grapheme_boundaries() {
    let mut editor = TextEditor::with_text("alpha beta");
    editor.set_cursor(0);
    assert!(editor.apply(TextEditAction::WordRight, 80));
    assert_eq!(editor.cursor(), "alpha".len());
    assert!(editor.apply(TextEditAction::WordRight, 80));
    assert_eq!(editor.cursor(), "alpha beta".len());
    assert!(editor.apply(TextEditAction::WordLeft, 80));
    assert_eq!(editor.cursor(), "alpha ".len());
    assert!(editor.apply(TextEditAction::WordLeft, 80));
    assert_eq!(editor.cursor(), 0);

    let mut jumps = TextEditor::with_text("a,b,c");
    jumps.set_cursor(0);
    assert!(jumps.apply(TextEditAction::JumpForward(','), 80));
    assert_eq!(jumps.cursor(), 1);
    assert!(jumps.apply(TextEditAction::JumpForward(','), 80));
    assert_eq!(jumps.cursor(), 3);
    assert!(jumps.apply(TextEditAction::JumpBackward(','), 80));
    assert_eq!(jumps.cursor(), 1);
    assert!(!jumps.apply(TextEditAction::JumpBackward(','), 80));
    assert_eq!(jumps.cursor(), 1);
    assert!(jumps.cursor_is_valid());
}

#[test]
fn line_edge_kills_merge_lines_and_are_undoable() {
    let mut editor = TextEditor::with_text("abc\ndef");
    editor.set_cursor("abc\nd".len());
    assert!(editor.apply(TextEditAction::DeleteToLineStart, 80));
    assert_eq!(editor.text(), "abc\nef");
    assert!(editor.apply(TextEditAction::DeleteToLineStart, 80));
    assert_eq!(editor.text(), "abcef");
    assert_eq!(editor.cursor(), 3);
    assert!(editor.apply(TextEditAction::Undo, 80));
    assert_eq!(editor.text(), "abc\nef");

    let mut tail = TextEditor::with_text("one\ntwo");
    tail.set_cursor(0);
    assert!(!tail.apply(TextEditAction::DeleteToLineStart, 80));
    assert_eq!(tail.text(), "one\ntwo");
    tail.set_cursor("one".len());
    assert!(tail.apply(TextEditAction::DeleteToLineEnd, 80));
    assert_eq!(tail.text(), "onetwo");
    assert_eq!(tail.cursor(), 3);
    assert!(tail.apply(TextEditAction::Undo, 80));
    assert_eq!(tail.text(), "one\ntwo");
}
