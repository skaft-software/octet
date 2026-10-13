//! Promoting a fenced `diff`/`patch` code block into a real diff render.
//!
//! A unified diff is a code block with meaning attached, so it gets its own
//! layout: full-width line backgrounds, per-marker color and optional line
//! numbers, all of which come from the parsed [`UnifiedDiff`] rather than from
//! the generic code path. The shell-command scanner lives here too, because
//! what it produces is used only by a hunk header — the one line of a diff that
//! describes a command — and it deliberately stops short of parsing shell
//! grammar: it locates executable words so they can be styled, and skips
//! assignments and redirection targets so flags keep their normal roles.

use super::lines::{push_run, RichLine, RichRun};
use super::{CodeOverflow, RichRenderer};

use crate::rich_text::diff::{DiffLineKind, UnifiedDiff};
use crate::style::{Color, TextRole, TextStyle};

impl RichRenderer {
    /// Render a unified-diff string through the semantic diff pipeline and
    /// return styled RichLines. Fenced `diff` / `patch` code blocks in
    /// Markdown are promoted to this path so they get full-width backgrounds,
    /// color, and optional line numbers instead of bare monospace.
    pub(super) fn render_diff_as_rich_lines(&self, source: &str, width: usize) -> Vec<RichLine> {
        let diff = UnifiedDiff::parse(source);
        let number_width = diff
            .lines
            .iter()
            .flat_map(|line| [line.old_number, line.new_number])
            .flatten()
            .max()
            .map_or(1, |number| number.to_string().len());
        let show_numbers = width >= 70;

        // Extract the language hint from file headers so code lines can be
        // syntax-highlighted.  Multiple files in one diff update the language
        // as each new header is encountered.
        let mut language: Option<String> = None;

        let mut output = Vec::new();
        for line in &diff.lines {
            // Track the file language from `+++ b/…` headers.
            if line.kind == DiffLineKind::FileHeader {
                if let Some(hint) = diff_language_hint(&line.text) {
                    language = Some(hint);
                }
            }

            let role = match line.kind {
                DiffLineKind::Addition => TextRole::DiffAdd,
                DiffLineKind::Removal => TextRole::DiffRemove,
                DiffLineKind::Context | DiffLineKind::Metadata | DiffLineKind::Binary => {
                    TextRole::DiffContext
                }
                DiffLineKind::HunkHeader => TextRole::DiffHunk,
                DiffLineKind::FileHeader => TextRole::DiffHeader,
            };
            let base_style = self.theme.style(role);
            let mut gutter_style = self.theme.style(TextRole::Subtle);
            gutter_style.background = base_style.background;

            let mut prefix = String::new();
            if show_numbers {
                let number = line.new_number.or(line.old_number).map_or_else(
                    || " ".repeat(number_width),
                    |number| format!("{number:>number_width$}"),
                );
                prefix = format!("{number} | ");
            }

            let prefix_width = self.options.width.line_width(&prefix);
            let available = width.saturating_sub(prefix_width);
            let text = self.sanitize(&line.text);
            let diff_style = base_style;

            // Build runs: try syntax highlighting for code lines when we
            // know the file language, otherwise render as plain text.
            // Strip the single-character diff prefix (+, -, space) from code
            // lines so the syntax highlighter receives valid source text.
            let runs = self
                .diff_code_runs(&text, line.kind, language.as_deref(), diff_style)
                .unwrap_or_else(|| vec![RichRun::new(text, diff_style, None)]);

            let rows = if self.options.code_overflow == CodeOverflow::Wrap {
                self.wrap_runs(&runs, available)
            } else {
                vec![self.clip_runs(&runs, available)]
            };

            for (row_index, row) in rows.into_iter().enumerate() {
                let mut rendered = RichLine::default();
                if row_index == 0 && !prefix.is_empty() {
                    rendered.push(prefix.clone(), gutter_style, None);
                } else if !prefix.is_empty() {
                    rendered.push(" ".repeat(prefix_width), gutter_style, None);
                }
                rendered.extend(row);

                // Pad to full width so backgrounds span the entire line.
                let current_width = self.line_width(&rendered);
                if current_width < width {
                    let pad_style = rendered
                        .runs
                        .last()
                        .map(|run| run.style)
                        .unwrap_or(diff_style);
                    rendered.push(" ".repeat(width - current_width), pad_style, None);
                }
                output.push(rendered);
            }
        }
        output
    }

    pub(super) fn diff_code_runs(
        &self,
        text: &str,
        kind: DiffLineKind,
        language: Option<&str>,
        diff_style: TextStyle,
    ) -> Option<Vec<RichRun>> {
        if !matches!(
            kind,
            DiffLineKind::Context | DiffLineKind::Addition | DiffLineKind::Removal
        ) {
            return None;
        }
        let (marker, source) = match kind {
            DiffLineKind::Addition => ("+", text.strip_prefix('+').unwrap_or(text)),
            DiffLineKind::Removal => ("-", text.strip_prefix('-').unwrap_or(text)),
            DiffLineKind::Context if text.starts_with(' ') => {
                (" ", text.strip_prefix(' ').unwrap_or(text))
            }
            DiffLineKind::Context => ("", text),
            _ => return None,
        };

        let mut runs = vec![RichRun::new(
            marker.to_owned(),
            self.diff_marker_style(kind, diff_style),
            None,
        )];
        #[cfg(not(feature = "syntax-highlighting"))]
        let _ = language;

        #[cfg(feature = "syntax-highlighting")]
        if self.options.syntax_highlighting {
            if let Some(language) = language {
                if let Some(highlighted) = self
                    .syntax_cache
                    .borrow_mut()
                    .get_or_insert(language, source)
                {
                    for region in highlighted.iter().flatten() {
                        let token_style = region
                            .role
                            .map_or(diff_style, |role| diff_style.merge(self.theme.style(role)));
                        runs.push(RichRun::new(region.text.clone(), token_style, None));
                    }
                    return Some(runs);
                }
            }
        }

        runs.push(RichRun::new(source.to_owned(), diff_style, None));
        Some(runs)
    }

    pub(super) fn diff_marker_style(&self, kind: DiffLineKind, mut style: TextStyle) -> TextStyle {
        let token = match kind {
            DiffLineKind::Addition => "diff_added_marker",
            DiffLineKind::Removal => "diff_removed_marker",
            _ => return style,
        };
        if let Some(color) = self
            .theme
            .resolve_color(token)
            .filter(|color| *color != Color::Default)
        {
            style.foreground = color;
        }
        style
    }
}

pub(super) fn shell_program_ranges(source: &str) -> Vec<(usize, usize)> {
    let bytes = source.as_bytes();
    let mut ranges = Vec::new();
    let mut index = 0usize;
    let mut expect_program = true;
    let mut skip_redirection_target = false;

    while index < bytes.len() {
        match bytes[index] {
            b' ' | b'\t' | b'\r' => {
                index += 1;
                continue;
            }
            b'\n' => {
                expect_program = true;
                skip_redirection_target = false;
                index += 1;
                continue;
            }
            b'#' => {
                index = source[index..]
                    .find('\n')
                    .map_or(bytes.len(), |newline| index + newline);
                continue;
            }
            b'|' | b';' | b'&' => {
                expect_program = true;
                skip_redirection_target = false;
                let separator = bytes[index];
                index += 1;
                if bytes.get(index) == Some(&separator)
                    || (separator == b'|' && bytes.get(index) == Some(&b'&'))
                {
                    index += 1;
                }
                continue;
            }
            b'(' => {
                expect_program = true;
                skip_redirection_target = false;
                index += 1;
                continue;
            }
            b')' => {
                expect_program = false;
                skip_redirection_target = false;
                index += 1;
                continue;
            }
            b'<' | b'>' => {
                skip_redirection_target = true;
                index += 1;
                while matches!(bytes.get(index), Some(b'<') | Some(b'>')) {
                    index += 1;
                }
                if bytes.get(index) == Some(&b'&') {
                    index += 1;
                }
                continue;
            }
            b'$' if bytes.get(index + 1) == Some(&b'(') => {
                expect_program = true;
                skip_redirection_target = false;
                index += 2;
                continue;
            }
            _ => {}
        }

        let start = index;
        let mut quote = None;
        while index < bytes.len() {
            let byte = bytes[index];
            if let Some(delimiter) = quote {
                if byte == b'\\' && delimiter == b'"' {
                    index = (index + 2).min(bytes.len());
                } else {
                    index += 1;
                    if byte == delimiter {
                        quote = None;
                    }
                }
                continue;
            }
            match byte {
                b'\'' | b'"' | b'`' => {
                    quote = Some(byte);
                    index += 1;
                }
                b'\\' => index = (index + 2).min(bytes.len()),
                b' ' | b'\t' | b'\r' | b'\n' | b'|' | b';' | b'&' | b'(' | b')' | b'<' | b'>' => {
                    break
                }
                _ => index += 1,
            }
        }

        if start == index {
            index += 1;
            continue;
        }
        let word = &source[start..index];
        if skip_redirection_target {
            skip_redirection_target = false;
            continue;
        }
        if !expect_program || shell_assignment(word) {
            continue;
        }
        if shell_reserved_word(word) {
            expect_program = matches!(
                word,
                "if" | "then" | "elif" | "else" | "do" | "while" | "until" | "!"
            );
            continue;
        }
        ranges.push((start, index));
        expect_program = false;
    }

    ranges
}

fn shell_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let name = name.strip_suffix('+').unwrap_or(name);
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn shell_reserved_word(word: &str) -> bool {
    matches!(
        word,
        "if" | "then"
            | "elif"
            | "else"
            | "fi"
            | "for"
            | "while"
            | "until"
            | "do"
            | "done"
            | "case"
            | "in"
            | "esac"
            | "select"
            | "function"
            | "time"
            | "coproc"
            | "!"
            | "{"
            | "}"
    )
}

pub(super) fn restyle_ranges(
    runs: Vec<RichRun>,
    ranges: &[(usize, usize)],
    range_style: TextStyle,
) -> Vec<RichRun> {
    if ranges.is_empty() {
        return runs;
    }
    let mut output = Vec::new();
    let mut offset = 0usize;
    let mut range_index = 0usize;
    for run in runs {
        let run_end = offset.saturating_add(run.text.len());
        let mut cursor = offset;
        while cursor < run_end {
            while ranges
                .get(range_index)
                .is_some_and(|(_, end)| *end <= cursor)
            {
                range_index += 1;
            }
            let Some((range_start, range_end)) = ranges.get(range_index).copied() else {
                push_run(
                    &mut output,
                    run.text[cursor - offset..].to_owned(),
                    run.style,
                    run.link.clone(),
                );
                break;
            };
            if range_start >= run_end {
                push_run(
                    &mut output,
                    run.text[cursor - offset..].to_owned(),
                    run.style,
                    run.link.clone(),
                );
                break;
            }
            if cursor < range_start {
                let end = range_start.min(run_end);
                push_run(
                    &mut output,
                    run.text[cursor - offset..end - offset].to_owned(),
                    run.style,
                    run.link.clone(),
                );
                cursor = end;
                continue;
            }
            let end = range_end.min(run_end);
            push_run(
                &mut output,
                run.text[cursor - offset..end - offset].to_owned(),
                range_style,
                run.link.clone(),
            );
            cursor = end;
        }
        offset = run_end;
    }
    output
}

/// Infer the syntax token from a unified-diff file header. Timestamps are
/// ignored, and backup suffixes are not treated as programming languages.
pub(super) fn diff_language_hint(header: &str) -> Option<String> {
    let path = header
        .strip_prefix("+++ ")
        .or_else(|| header.strip_prefix("--- "))?
        .split('\t')
        .next()?;
    if path == "/dev/null" {
        return None;
    }
    let name = path.rsplit('/').next()?;
    let token = name
        .rsplit_once('.')
        .map_or(name, |(_, extension)| extension);
    (!token.is_empty()
        && token.len() <= 16
        && !token.eq_ignore_ascii_case("orig")
        && !token.eq_ignore_ascii_case("bak"))
    .then(|| token.to_ascii_lowercase())
}
