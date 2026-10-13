//! Markdown cell escaping for the comparison report.
//!
//! A report cell is attacker-controlled text that ends up inside a Markdown
//! table cell, so every character that could change the *structure* of the
//! rendered table or make the output invisible is escaped, and nothing else is.
//! Deciding which characters those are requires the surrounding context — an
//! underscore is markup only between word characters, a backslash only before a
//! punctuation character — so the predicates below take lookbehind arguments
//! rather than testing a character in isolation.
//!
//! This is the whole of the report's rendering policy, which is why it is one
//! small module: a second place that decides how to escape a cell would be a
//! second policy, and the two would disagree on exactly one character.

use std::fmt::Write as _;

pub(super) fn markdown_cell(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    let mut previous = None;
    let mut preceding = [None; 3];

    while let Some(character) = characters.next() {
        match character {
            '\n' => escaped.push_str("<br>"),
            '\r' if matches!(characters.peek(), Some('\n')) => {
                characters.next();
                escaped.push_str("<br>");
            }
            _ if requires_visible_escape(character) => {
                write!(escaped, "\\u{{{:04X}}}", u32::from(character))
                    .expect("writing a Unicode escape to a String cannot fail");
            }
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            '\\' => escaped.push_str("\\\\"),
            '.' if has_www_prefix(preceding) => {
                escaped.push('\\');
                escaped.push('.');
            }
            '_' if is_intraword_underscore(previous, characters.peek().copied()) => {
                escaped.push('_');
            }
            '!' | '#' | '$' | '(' | ')' | '*' | '+' | '/' | ':' | '@' | '[' | ']' | '`' | '_'
            | '{' | '|' | '}' | '~' | '^' => {
                escaped.push('\\');
                escaped.push(character);
            }
            _ => escaped.push(character),
        }
        previous = Some(character);
        preceding.rotate_left(1);
        preceding[2] = Some(character);
    }

    escaped
}

pub(super) fn has_www_prefix(preceding: [Option<char>; 3]) -> bool {
    preceding
        .iter()
        .copied()
        .all(|character| matches!(character, Some('w' | 'W')))
}

pub(super) fn is_intraword_underscore(previous: Option<char>, next: Option<char>) -> bool {
    previous.is_some_and(char::is_alphanumeric) && next.is_some_and(char::is_alphanumeric)
}

// This fixed table is the v1 bounded policy for Unicode
// `Default_Ignorable_Code_Point`. It is intentionally scalar-based: diagnostic
// validation does not depend on normalization, Unicode database loading, or
// locale-specific rendering behavior.
pub(super) fn is_default_ignorable_scalar(character: char) -> bool {
    matches!(
        character as u32,
        0x00AD
            | 0x034F
            | 0x061C
            | 0x115F..=0x1160
            | 0x17B4..=0x17B5
            | 0x180B..=0x180F
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0x3164
            | 0xFE00..=0xFE0F
            | 0xFEFF
            | 0xFFA0
            | 0xFFF0..=0xFFF8
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0000..=0xE0FFF
    )
}

pub(super) fn requires_visible_escape(character: char) -> bool {
    matches!(
        character,
        '\u{0000}'..='\u{001F}'
            | '\u{007F}'..='\u{009F}'
            | '\u{061C}'
            | '\u{200E}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2066}'..='\u{206F}'
    )
}
