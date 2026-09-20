#![allow(missing_docs)]

//! Paste admission and text-side path classification.
//!
//! This module owns the conservative boundary between ordinary prose, explicit
//! dropped paths, collapsed large text, and command/path-looking editor text.
//! It never reads attachment bytes; the ledger owns that side effect.

use std::ops::Range;
use std::path::{Path, PathBuf};

use super::attachments::{file_kind_for_path, media_kind_for_path};

/// A paste larger than either bound collapses to a placeholder chip.
pub const LARGE_PASTE_LINES: usize = 10;
pub const LARGE_PASTE_CHARS: usize = 2048;

fn unescape_path_token(text: &str) -> String {
    let mut unescaped = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\\'
            && characters
                .peek()
                .is_some_and(|next| *next == '\\' || next.is_whitespace())
        {
            unescaped.push(characters.next().expect("peeked escaped character"));
        } else {
            unescaped.push(character);
        }
    }
    unescaped
}

/// One path token admitted by an explicit paste/drop event.
///
/// `range` indexes the original paste text, while `path` is the decoded local
/// file used for the attachment. Keeping both lets the caller replace each
/// token with a distinct chip without changing separators or prompt text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DroppedPath {
    pub range: Range<usize>,
    pub path: PathBuf,
}

/// Tokenize shell-quoted path text without performing shell expansion.
///
/// Drag/drop implementations disagree about whether paths with spaces are
/// quoted or backslash-escaped. This parser accepts both forms, preserves the
/// original byte range, and deliberately does not implement globbing,
/// variables, command substitution, or other shell behavior.
fn shell_token_ranges(text: &str) -> Option<Vec<Range<usize>>> {
    let mut ranges = Vec::new();
    let mut start = None;
    let mut quote = None;
    let mut escaped = false;

    for (index, character) in text.char_indices() {
        let Some(token_start) = start else {
            if character.is_whitespace() {
                continue;
            }
            start = Some(index);
            if character == '\\' {
                escaped = true;
            } else if matches!(character, '\'' | '"') {
                quote = Some(character);
            }
            continue;
        };

        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
            continue;
        }
        if character.is_whitespace() {
            ranges.push(token_start..index);
            start = None;
        }
    }

    if escaped || quote.is_some() {
        return None;
    }
    if let Some(token_start) = start {
        ranges.push(token_start..text.len());
    }
    Some(ranges)
}

/// Decode only the quoting/escaping syntax used by terminal path drops.
fn decode_shell_path_token(raw: &str) -> Option<String> {
    let mut decoded = String::with_capacity(raw.len());
    let mut quote = None;
    let mut characters = raw.chars().peekable();

    while let Some(character) = characters.next() {
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else if delimiter == '"' && character == '\\' {
                let next = characters.peek().copied()?;
                if next == '\\' || next.is_whitespace() || matches!(next, '\'' | '"') {
                    decoded.push(next);
                    characters.next();
                } else {
                    // Keep Windows-style or literal backslashes whose next
                    // character is not a drop escape.
                    decoded.push('\\');
                }
            } else {
                decoded.push(character);
            }
            continue;
        }

        match character {
            '\\' => {
                let next = characters.peek().copied()?;
                if next == '\\' || next.is_whitespace() || matches!(next, '\'' | '"') {
                    decoded.push(next);
                    characters.next();
                } else {
                    decoded.push('\\');
                }
            }
            '\'' | '"' => quote = Some(character),
            _ => decoded.push(character),
        }
    }

    quote.is_none().then_some(decoded)
}

fn existing_file(path: PathBuf) -> Option<PathBuf> {
    path.is_file().then_some(path)
}

fn path_from_decoded_token(decoded: &str) -> Option<PathBuf> {
    if decoded.is_empty() {
        return None;
    }
    let expanded = if let Some(rest) = decoded.strip_prefix("file://") {
        // Only local file URLs are safe to resolve in the TUI. A non-local
        // hostname must not turn into a relative path under the process cwd.
        let path = if rest == "localhost" {
            return None;
        } else if let Some(path) = rest.strip_prefix("localhost/") {
            format!("/{path}")
        } else if rest.starts_with('/') {
            rest.to_owned()
        } else {
            return None;
        };
        // `file://` URLs percent-encode spaces and non-ASCII bytes; plain
        // dropped paths are left untouched (a literal `%20` in a filename).
        percent_encoding::percent_decode_str(&path)
            .decode_utf8()
            .map(|decoded| decoded.into_owned())
            .unwrap_or(path)
    } else if let Some(rest) = decoded.strip_prefix("~/") {
        let home = dirs::home_dir()?;
        return existing_file(home.join(rest));
    } else {
        decoded.to_owned()
    };
    existing_file(PathBuf::from(expanded))
}

/// Interpret an explicit paste/drop payload as an ordered list of existing
/// local files. Every non-whitespace token must resolve to a regular file.
pub fn explicit_dropped_paths(text: &str) -> Option<Vec<DroppedPath>> {
    let ranges = shell_token_ranges(text)?;
    if ranges.is_empty() {
        return None;
    }

    ranges
        .into_iter()
        .map(|range| {
            let decoded = decode_shell_path_token(&text[range.clone()])?;
            let path = path_from_decoded_token(&decoded)?;
            Some(DroppedPath { range, path })
        })
        .collect()
}

/// Interpret a paste payload as one existing local file path, if it is one.
/// Lists deliberately use [`parse_dropped_paths`] instead.
pub fn parse_dropped_path(text: &str) -> Option<PathBuf> {
    let mut paths = explicit_dropped_paths(text)?.into_iter();
    let path = paths.next()?.path;
    paths.next().is_none().then_some(path)
}

fn classify_existing_path(path: PathBuf) -> PasteKind {
    if media_kind_for_path(&path).is_some() {
        PasteKind::MediaFile(path)
    } else if file_kind_for_path(&path).is_some() {
        PasteKind::DocumentFile(path)
    } else {
        PasteKind::NonMediaFile(path)
    }
}

/// Return whether editor text should be treated as an absolute filesystem path
/// instead of a slash command.
pub fn looks_like_absolute_path(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.contains('\n') {
        return false;
    }
    if parse_dropped_path(trimmed).is_some() {
        return true;
    }

    let unquoted = if let Some(value) = trimmed
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
    {
        value
    } else if let Some(value) = trimmed.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        value
    } else {
        trimmed
    };
    // macOS terminals commonly paste drag/drop paths shell-escaped as
    // `/Users/me/Screenshot\\ 2026.png`. Normalize escaped spaces before the
    // lexical command/path decision; this also covers missing destinations.
    let mut escaped = false;
    let prefix_end = unquoted
        .char_indices()
        .find_map(|(index, character)| {
            if escaped {
                escaped = false;
                return None;
            }
            if character == '\\' {
                escaped = true;
                return None;
            }
            character.is_whitespace().then_some(index)
        })
        .unwrap_or(unquoted.len());
    let normalized = unquoted[..prefix_end].replace("\\ ", " ");
    if !normalized.starts_with('/') {
        return false;
    }
    // A second separator distinguishes ordinary absolute paths from slash
    // command names. Existing files/directories are paths even with one level.
    normalized[1..].contains('/') || Path::new(&normalized).exists()
}

/// How a paste payload should enter the composer.
#[derive(Clone, Debug, PartialEq)]
pub enum PasteKind {
    Verbatim,
    LargeText,
    MediaFile(PathBuf),
    DocumentFile(PathBuf),
    NonMediaFile(PathBuf),
}

/// Classify a single paste using the explicit-file check before size limits.
pub fn classify_paste(text: &str) -> PasteKind {
    if let Some(path) = parse_dropped_path(text) {
        return classify_existing_path(path);
    }
    if text.lines().count() > LARGE_PASTE_LINES || text.chars().count() > LARGE_PASTE_CHARS {
        return PasteKind::LargeText;
    }
    PasteKind::Verbatim
}

// Used by the path-completion sibling; kept crate-visible rather than exposed
// through the stable composer facade.
pub(crate) fn unescape_for_completion(text: &str) -> String {
    unescape_path_token(text)
}
