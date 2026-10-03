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
mod tests;
