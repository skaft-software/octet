//! Unit tests for `crate::utils`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `utils.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::utils`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
#[test]
fn pi_width_and_normalization() {
    assert_eq!(visible_width("\t\x1b[31m界\x1b[0m"), 5);
    assert_eq!(visible_width("🇨"), 2);
    assert_eq!(normalize_terminal_output("ำຳ\t"), "ําໍາ   ");
    assert_eq!(
        normalize_terminal_output("\x1b]0;a\tb\x1b\\x\t"),
        "\x1b]0;a\tb\x1b\\x   "
    )
}
#[test]
fn pi_truncation_resets_styles() {
    assert_eq!(
        truncate_to_width_padded("abcdef", 2, "🙂", false),
        "\x1b[0m🙂\x1b[0m"
    );
    assert_eq!(truncate_to_width_padded("abcdef", 1, "🙂", false), "");
    let value = truncate_to_width_padded("\x1b[31mhello hello", 8, "…", false);
    assert!(value.ends_with("\x1b[0m…\x1b[0m"));
    assert!(visible_width(&value) <= 8)
}
#[test]
fn pi_column_slices_and_segments_handle_tabs() {
    let value = "out 192M\t.pi/skill-tests/results-ha";
    assert_eq!(
        slice_with_width(value, 0, 10, true),
        ColumnSlice {
            text: "out 192M".into(),
            width: 8
        }
    );
    let segments = extract_segments(value, 11, 13, 10, true);
    assert_eq!(segments.before, "out 192M\t");
    assert_eq!(segments.before_width, 11)
}
#[test]
fn pi_wrap_preserves_specific_style_state() {
    let lines = wrap_text_with_ansi(
        "\x1b[44mhello world this is blue background text\x1b[0m",
        15,
    );
    assert!(lines.iter().all(|line| line.contains("\x1b[44m")));
    assert!(lines[..lines.len() - 1]
        .iter()
        .all(|line| !line.ends_with("\x1b[0m")));
    let underlined = wrap_text_with_ansi(
        "prefix \x1b[4mhttps://example.com/very/long/path\x1b[24m",
        18,
    );
    assert!(underlined
        .iter()
        .skip(1)
        .any(|line| line.starts_with("\x1b[4m")))
}
#[test]
fn pi_logical_line_endings() {
    assert_eq!(
        logical_lines("first\nsecond\r\nthird\rfourth"),
        vec!["first", "second", "third", "fourth"]
    )
}

#[test]
fn sanitizer_keeps_utf8_boundaries_after_malformed_escape() {
    assert_eq!(strip_terminal_sequences("before\x1b�after"), "before�after");
    assert_eq!(strip_terminal_sequences("\x1b🙂界"), "🙂界");
    assert_eq!(strip_terminal_sequences("a\x1b(Bb"), "ab");
}

#[test]
fn hyperlinks_resolve_by_visible_cell() {
    let row = "plain \x1b]8;;https://example.test/docs\x07docs\x1b]8;;\x07 tail";
    assert_eq!(hyperlink_at_column(row, 0), None);
    assert_eq!(
        hyperlink_at_column(row, 6).as_deref(),
        Some("https://example.test/docs")
    );
    assert_eq!(
        hyperlink_at_column(row, 9).as_deref(),
        Some("https://example.test/docs")
    );
    assert_eq!(hyperlink_at_column(row, 10), None);
    // A link closed on the previous row is not inherited.
    assert_eq!(hyperlink_at_column("no link here", 2), None);
    // Wide and accented cells keep their own boundaries.
    let wide = "\x1b]8;;https://wide.test\x07審査\x1b]8;;\x07x";
    assert_eq!(
        hyperlink_at_column(wide, 1).as_deref(),
        Some("https://wide.test")
    );
    assert_eq!(
        hyperlink_at_column(wide, 3).as_deref(),
        Some("https://wide.test")
    );
    assert_eq!(hyperlink_at_column(wide, 4), None);
}
