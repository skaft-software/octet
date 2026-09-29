//! Unit tests for `crate::width`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `width.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::width`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn unicode_combining_and_wide_widths_are_cell_correct() {
    let policy = WidthPolicy::default();
    assert_eq!(policy.line_width("e\u{301}"), 1);
    assert_eq!(policy.line_width("界"), 2);
    assert_eq!(policy.line_width("a界b"), 4);
    assert_eq!(policy.line_width("👩‍💻"), 2);
}

#[test]
fn tabs_follow_current_column() {
    let policy = WidthPolicy::default();
    assert_eq!(policy.line_width("\t"), 4);
    assert_eq!(policy.line_width("a\tb"), 5);
    assert_eq!(policy.expand_tabs("a\tb", 0), "a   b");
}

#[test]
fn zero_and_one_column_layout_do_not_panic() {
    let policy = WidthPolicy::default();
    assert_eq!(policy.wrap("abc", 0), vec![""]);
    assert_eq!(policy.wrap("abc", 1), vec!["a", "b", "c"]);
    assert_eq!(policy.truncate("界", 1, "…"), "…");
}

#[test]
fn wrapping_prefers_words_then_breaks_long_identifiers() {
    let policy = WidthPolicy::default();
    assert_eq!(policy.wrap("alpha beta", 6), vec!["alpha", "beta"]);
    assert_eq!(policy.wrap("abcdefgh", 3), vec!["abc", "def", "gh"]);
}
