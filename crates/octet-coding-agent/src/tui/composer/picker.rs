#![allow(missing_docs)]

//! Composer-owned completion discovery.
//!
//! This module supplies the bounded workspace/mention/path candidate set used by
//! the view's popup. Rendering, selection state, and key dispatch remain with
//! the surface/view owners; this module owns only query interpretation and
//! filesystem discovery.

use std::fs;
use std::path::{Path, PathBuf};

use super::paste::{looks_like_absolute_path, unescape_for_completion};

/// List workspace files (relative, sorted, gitignore-aware), capped.
pub fn workspace_files(root: &Path, cap: usize) -> Vec<String> {
    let mut files = Vec::new();
    // `require_git(false)` honors .gitignore files even when the workspace is
    // not (yet) a git repository, which is also useful for new projects.
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .require_git(false)
        .build();
    for entry in walker.flatten() {
        if files.len() >= cap {
            break;
        }
        if entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            if let Ok(relative) = entry.path().strip_prefix(root) {
                files.push(relative.to_string_lossy().into_owned());
            }
        }
    }
    files.sort();
    files
}

fn active_token(text: &str) -> Option<&str> {
    let mut token_start = 0;
    let mut escaped = false;
    for (index, character) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
        } else if character.is_whitespace() {
            token_start = index + character.len_utf8();
        }
    }
    (token_start < text.len()).then(|| &text[token_start..])
}

/// The mention query when the text ends in an `@`-prefixed token.
pub fn active_mention(text: &str) -> Option<&str> {
    active_token(text)?.strip_prefix('@')
}

/// Whether a mention query should be completed by walking one filesystem
/// directory instead of fuzzy-matching the workspace file index.
pub fn is_path_query(query: &str) -> bool {
    query.starts_with(['.', '~', '/']) || query.contains('/')
}

/// A trailing literal path token eligible for Tab completion.
///
/// Bare words are deliberately excluded so Tab remains inert while writing
/// prose. A leading slash is treated as a path only when it is distinguishable
/// from a slash command; command arguments can always contain path tokens.
pub fn active_path(text: &str) -> Option<&str> {
    let token = active_token(text)?;
    if token.starts_with('@') || token.contains("://") || !is_path_query(token) {
        return None;
    }
    let token_is_entire_input = text.trim_start() == token;
    if token.starts_with('/') && token_is_entire_input && !looks_like_absolute_path(token) {
        return None;
    }
    Some(token)
}

/// Case-insensitive substring match on relative paths; earlier and shorter
/// matches rank first.
pub fn mention_matches<'a>(files: &'a [String], query: &str, limit: usize) -> Vec<&'a str> {
    let needle = query.to_lowercase();
    let mut scored: Vec<(usize, usize, &str)> = files
        .iter()
        .filter_map(|file| {
            file.to_lowercase()
                .find(&needle)
                .map(|at| (at, file.len(), file.as_str()))
        })
        .collect();
    scored.sort();
    scored
        .into_iter()
        .take(limit)
        .map(|(_, _, file)| file)
        .collect()
}

/// One filesystem path offered to the composer for Tab completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathSuggestion {
    /// Text that replaces the active query, preserving `./`, `../`, `~/`, or
    /// absolute syntax from the user's input.
    pub completion: String,
    /// Resolved path used for attachment classification and reads.
    pub path: PathBuf,
    /// Directories remain active after completion so another Tab can descend.
    pub is_dir: bool,
}

/// List entries matching a path-shaped query.
///
/// Only the query's immediate directory is read. Relative, parent, home, and
/// absolute prefixes are preserved in the returned completion text; whitespace
/// is backslash-escaped, and hidden entries appear only after the user starts a
/// basename with `.`.
pub fn path_matches(root: &Path, query: &str, limit: usize) -> Vec<PathSuggestion> {
    const MAX_SCANNED_ENTRIES: usize = 10_000;

    if limit == 0 {
        return Vec::new();
    }
    let query = unescape_for_completion(query);

    // These directory aliases do not appear in read_dir(), but completing them
    // first makes `.` / `..` / `~` behave like an ordinary shell prompt.
    let directory_alias = match query.as_str() {
        "." => Some(("./", root.to_path_buf())),
        ".." => Some(("../", root.join(".."))),
        "~" => dirs::home_dir().map(|home| ("~/", home)),
        _ => None,
    };
    if let Some((completion, path)) = directory_alias {
        return if path.is_dir() {
            vec![PathSuggestion {
                completion: completion.to_owned(),
                path,
                is_dir: true,
            }]
        } else {
            Vec::new()
        };
    }

    // `~other-user` expansion is intentionally unsupported: dirs::home_dir()
    // resolves only the current user's home, so guessing would produce a path
    // that looks valid but points somewhere else.
    if query.starts_with('~') && !query.starts_with("~/") {
        return Vec::new();
    }

    let basename_start = query.rfind('/').map_or(0, |index| index + 1);
    let directory_prefix = &query[..basename_start];
    let basename_prefix = &query[basename_start..];
    let search_dir = if let Some(home_relative) = directory_prefix.strip_prefix("~/") {
        let Some(home) = dirs::home_dir() else {
            return Vec::new();
        };
        home.join(home_relative)
    } else if Path::new(directory_prefix).is_absolute() {
        PathBuf::from(directory_prefix)
    } else {
        root.join(directory_prefix)
    };

    let Ok(entries) = fs::read_dir(search_dir) else {
        return Vec::new();
    };
    let folded_prefix = basename_prefix.to_lowercase();
    let mut suggestions = Vec::new();
    for entry in entries.flatten().take(MAX_SCANNED_ENTRIES) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.chars().any(char::is_control)
            || (name.starts_with('.') && !basename_prefix.starts_with('.'))
        {
            continue;
        }
        if !name.to_lowercase().starts_with(&folded_prefix) {
            continue;
        }

        let path = entry.path();
        let is_dir = path.is_dir();
        if !is_dir && !path.is_file() {
            continue;
        }
        let completion = format!("{directory_prefix}{name}{}", if is_dir { "/" } else { "" });
        suggestions.push(PathSuggestion {
            completion: escape_path_token(&completion),
            path,
            is_dir,
        });
    }

    suggestions.sort_by(|left, right| {
        left.completion
            .to_lowercase()
            .cmp(&right.completion.to_lowercase())
            .then_with(|| left.completion.cmp(&right.completion))
    });
    suggestions.truncate(limit);
    suggestions
}

fn escape_path_token(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if character == '\\' || character.is_whitespace() {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}
