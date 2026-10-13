//! Unit tests for `crate::text_editor::word_nav`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::text_editor::word_nav`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn backward_skips_whitespace_then_stops_at_word_start() {
    let text = "alpha beta gamma";
    assert_eq!(find_word_backward(text, text.len()), "alpha beta ".len());
    assert_eq!(find_word_backward(text, "alpha beta".len()), "alpha ".len());
    assert_eq!(find_word_backward(text, "alpha".len()), 0);
    // Trailing whitespace is skipped before the word run.
    assert_eq!(find_word_backward("alpha   ", 8), 0);
}

#[test]
fn forward_skips_whitespace_then_stops_at_word_end() {
    let text = "alpha beta gamma";
    assert_eq!(find_word_forward(text, 0), "alpha".len());
    assert_eq!(find_word_forward(text, "alpha ".len()), "alpha beta".len());
    assert_eq!(find_word_forward(text, text.len()), text.len());
    // Leading whitespace is skipped before the word run.
    assert_eq!(find_word_forward("   alpha", 0), "   alpha".len());
}

#[test]
fn punctuation_and_unicode_form_runs() {
    assert_eq!(find_word_backward("foo->bar", 8), "foo->".len());
    assert_eq!(find_word_forward("foo->bar", 0), "foo".len());
    assert_eq!(find_word_forward("foo->bar", 3), "foo->".len());
    // Multi-byte word-like run stays a single unit.
    assert_eq!(find_word_forward("界界界 a", 0), "界界界".len());
    assert_eq!(find_word_backward("a 界界界", "a 界界界".len()), "a ".len());
}

#[test]
fn boundaries_are_clamped_and_never_panic() {
    assert_eq!(find_word_backward("abc", 0), 0);
    assert_eq!(find_word_backward("", 0), 0);
    assert_eq!(find_word_forward("", 0), 0);
    assert_eq!(find_word_forward("abc", 999), 3);
    assert_eq!(find_word_backward("abc", 999), 0);
}
