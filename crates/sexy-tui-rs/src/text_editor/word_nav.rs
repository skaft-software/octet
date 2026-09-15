//! Word-boundary navigation for the editor.
//!
//! Ported from the upstream reference `packages/tui/src/word-navigation.ts`
//! (`findWordBackward` / `findWordForward`) without depending on the host
//! `Intl.Segmenter`. Word-like graphemes (Unicode alphanumerics plus `_`) form
//! one run, a whitespace run is skipped, and any other run (punctuation,
//! emoji) forms a third class. Both functions are pure: they return a byte
//! offset in `text` and never mutate state.

use unicode_segmentation::UnicodeSegmentation;

/// The classification used to form navigation runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    /// A whitespace grapheme.
    Space,
    /// A word-like grapheme: a Unicode alphanumeric or `_`.
    Word,
    /// Any other grapheme (punctuation, symbols, emoji).
    Punct,
}

fn classify(grapheme: &str) -> Class {
    if grapheme.chars().all(char::is_whitespace) {
        Class::Space
    } else if grapheme.chars().any(|c| c.is_alphanumeric() || c == '_') {
        Class::Word
    } else {
        Class::Punct
    }
}

fn char_boundary_floor(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Cursor position after moving one word backward from `cursor`.
///
/// Trailing whitespace is skipped, then the cursor stops at the start of the
/// preceding word, punctuation, or emoji run. Returns 0 at the start of `text`.
#[must_use]
pub fn find_word_backward(text: &str, cursor: usize) -> usize {
    let cursor = char_boundary_floor(text, cursor);
    let graphemes: Vec<(usize, &str)> = text[..cursor].grapheme_indices(true).collect();
    let mut index = graphemes.len();

    while index > 0 && classify(graphemes[index - 1].1) == Class::Space {
        index -= 1;
    }
    if index == 0 {
        return 0;
    }

    let class = classify(graphemes[index - 1].1);
    while index > 0 && classify(graphemes[index - 1].1) == class {
        index -= 1;
    }
    graphemes[index].0
}

/// Cursor position after moving one word forward from `cursor`.
///
/// Leading whitespace is skipped, then the cursor stops at the end of the
/// following word, punctuation, or emoji run. Returns `text.len()` at the end.
#[must_use]
pub fn find_word_forward(text: &str, cursor: usize) -> usize {
    let cursor = char_boundary_floor(text, cursor);
    let after = &text[cursor..];
    let mut consumed = 0usize;
    let mut graphemes = after.grapheme_indices(true).peekable();

    while let Some(&(_, grapheme)) = graphemes.peek() {
        if classify(grapheme) == Class::Space {
            consumed += grapheme.len();
            graphemes.next();
        } else {
            break;
        }
    }

    if let Some(&(_, first)) = graphemes.peek() {
        let class = classify(first);
        while let Some(&(_, grapheme)) = graphemes.peek() {
            if classify(grapheme) == class {
                consumed += grapheme.len();
                graphemes.next();
            } else {
                break;
            }
        }
    }

    (cursor + consumed).min(text.len())
}

#[cfg(test)]
mod tests {
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
}
