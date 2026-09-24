//! Pi's math delimiters, recognized before CommonMark can consume escapes,
//! underscores, pipes, or a standalone `=` as Markdown syntax.
use std::{borrow::Cow, ops::Range};

use pulldown_cmark::{Event, LinkType, Parser, Tag};

use super::{render_latex, RenderLatexOptions};
use crate::rich_text::markdown::{parser_options, MAX_DIAGRAM_FENCE_BYTES};

pub(crate) struct MathToken {
    pub range: Range<usize>,
    pub display: bool,
    pub pending: bool,
    body: Range<usize>,
    container_prefix: String,
}

impl MathToken {
    pub fn render(&self, source: &str) -> String {
        let body = self.without_container(&source[self.body.clone()]);
        if !self.pending && body.len() <= MAX_DIAGRAM_FENCE_BYTES {
            if let Some(text) = render_latex(
                if self.display { body.trim() } else { &body },
                RenderLatexOptions {
                    display: self.display,
                },
            ) {
                return text;
            }
        }
        let raw = self.without_container(&source[self.range.clone()]);
        if self.display {
            raw.trim().to_owned()
        } else {
            raw.into_owned()
        }
    }

    fn without_container<'a>(&self, text: &'a str) -> Cow<'a, str> {
        if self.container_prefix.is_empty() {
            return Cow::Borrowed(text);
        }
        Cow::Owned(
            text.split('\n')
                .enumerate()
                .map(|(index, line)| {
                    if index == 0 {
                        line
                    } else {
                        line.strip_prefix(&self.container_prefix).unwrap_or(line)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }
}

/// Byte-length-preserving protection keeps all parser offsets in original-source
/// coordinates. Only recognized math is masked; code, escapes and HTML are not.
/// Display delimiters become blank boundaries, and embedded newlines disappear
/// so even malformed/pending math cannot become a heading, table, or code fence.
pub(crate) fn protect(source: &str) -> (Cow<'_, str>, Vec<MathToken>) {
    if !source.contains('$') && !source.contains("\\(") && !source.contains("\\[") {
        return (Cow::Borrowed(source), Vec::new());
    }
    let mut excluded = Vec::new();
    let mut containers = Vec::new();
    let mut inline_scopes = Vec::new();
    let parser = Parser::new_ext(source, parser_options());
    excluded.extend(
        parser
            .reference_definitions()
            .iter()
            .map(|(_, definition)| definition.span.clone()),
    );
    for (event, range) in parser.into_offset_iter() {
        if matches!(
            event,
            Event::Start(Tag::List(_) | Tag::Item | Tag::BlockQuote(_))
        ) {
            containers.push(range.clone());
        }
        if matches!(
            event,
            Event::Start(
                Tag::Paragraph | Tag::Heading { .. } | Tag::TableRow | Tag::TableHead | Tag::Item
            )
        ) {
            inline_scopes.push(range.clone());
        }
        if matches!(
            event,
            Event::Start(Tag::CodeBlock(_))
                | Event::Code(_)
                | Event::Html(_)
                | Event::InlineHtml(_)
        ) {
            excluded.push(range);
        } else if let Event::Start(Tag::Link { link_type, .. } | Tag::Image { link_type, .. }) =
            event
        {
            if matches!(link_type, LinkType::Autolink | LinkType::Email) {
                excluded.push(range);
            } else if let Some(split) = source[range.clone()].rfind("](") {
                excluded.push(range.start + split..range.end);
            }
        }
    }
    excluded.sort_by_key(|range| range.start);
    let mut excluded_index = 0;
    let mut tokens = Vec::new();
    let mut cursor = 0;
    let mut line_start = 0;
    let mut masked = source.as_bytes().to_vec();
    while cursor < source.len() {
        while excluded_index < excluded.len() && excluded[excluded_index].end <= cursor {
            excluded_index += 1;
        }
        if let Some(range) = excluded
            .get(excluded_index)
            .filter(|range| range.contains(&cursor))
        {
            if let Some(relative) = source[cursor..range.end].rfind('\n') {
                line_start = cursor + relative + 1;
            }
            cursor = range.end;
            continue;
        }
        let rest = &source[cursor..];
        if (rest.starts_with("https://") || rest.starts_with("http://"))
            && (cursor == 0 || source[..cursor].ends_with(char::is_whitespace))
        {
            cursor += rest.find(char::is_whitespace).unwrap_or(rest.len());
            continue;
        }
        if !escaped(source, cursor)
            && (rest.starts_with('$') || rest.starts_with("\\(") || rest.starts_with("\\["))
        {
            let prefix = &source[line_start..cursor];
            let nested = containers.iter().any(|range| range.contains(&cursor));
            let container_prefix = nested.then(|| math_container_prefix(prefix)).flatten();
            let display = (prefix.len() <= 3 && prefix.bytes().all(|b| b == b' '))
                || container_prefix.is_some();
            // Marked invokes extensions on a container's local source and inline
            // extensions on a single paragraph/heading/cell, not the document.
            // Respect those bounds before searching for a closing delimiter.
            let container_end = scope_end(source, cursor, &containers);
            let inline_end = scope_end(source, cursor, &inline_scopes).min(container_end);
            if let Some(mut token) = tokenize(source, cursor, display, container_end, inline_end) {
                if token.display {
                    token.container_prefix = container_prefix.unwrap_or_default();
                }
                masked[token.range.clone()].fill(b'x');
                if token.display && !nested && token.range.len() > 4 {
                    masked[cursor..cursor + 2].fill(b'\n');
                    if !token.pending {
                        masked[token.range.end - 2..token.range.end].fill(b'\n');
                    }
                }
                if let Some(relative) = source[cursor..token.range.end].rfind('\n') {
                    line_start = cursor + relative + 1;
                }
                cursor = token.range.end;
                tokens.push(token);
                continue;
            }
        }
        let character = rest.chars().next().unwrap();
        cursor += character.len_utf8();
        if character == '\n' {
            line_start = cursor;
        }
    }
    (
        Cow::Owned(String::from_utf8(masked).expect("math masks retain UTF-8 boundaries")),
        tokens,
    )
}

/// Container decoration removed by the block lexer before Pi's math tokenizer.
/// Return the corresponding continuation prefix (a list marker becomes spaces).
fn math_container_prefix(prefix: &str) -> Option<String> {
    let trimmed = prefix.trim_start_matches(' ');
    if trimmed.is_empty() || trimmed.chars().all(|ch| matches!(ch, '>' | ' ')) {
        return Some(prefix.to_owned());
    }
    let marker = trimmed.trim_start_matches('>').trim_start_matches(' ');
    let list = matches!(marker.trim(), "-" | "+" | "*") || {
        let digits = marker.trim_end().trim_end_matches(['.', ')']);
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    };
    list.then(|| {
        let start = prefix.len() - marker.len();
        format!("{}{}", &prefix[..start], " ".repeat(marker.len()))
    })
}

fn scope_end(source: &str, start: usize, scopes: &[Range<usize>]) -> usize {
    let end = scopes
        .iter()
        .filter(|range| range.contains(&start))
        .map(|range| range.end)
        .min()
        .unwrap_or(source.len());
    // A block's terminating newline belongs to Markdown structure, not math.
    start + source[start..end].trim_end_matches(['\r', '\n']).len()
}

fn escaped(source: &str, index: usize) -> bool {
    source.as_bytes()[..index]
        .iter()
        .rev()
        .take_while(|b| **b == b'\\')
        .count()
        % 2
        == 1
}

fn pending_dollar(body: &str) -> bool {
    body.chars()
        .any(|c| "_^=+*/<>()[]|±≤≥≠≈∈→⇒∞∫∑√-".contains(c))
        || body
            .as_bytes()
            .windows(2)
            .any(|pair| pair[0] == b'\\' && pair[1].is_ascii_alphabetic())
}

fn shell_variable(body: &str, after: &str) -> bool {
    if !after.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        return false;
    }
    let mut chars = body.chars();
    if !chars
        .next()
        .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
    {
        return false;
    }
    let suffix: String = chars
        .skip_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
        .collect();
    suffix.is_empty()
        || (suffix.chars().count() == 1
            && suffix
                .chars()
                .all(|c| !c.is_ascii_alphanumeric() && c != '_' && !c.is_whitespace()))
}

fn tokenize(
    source: &str,
    start: usize,
    block_start: bool,
    container_end: usize,
    inline_end: usize,
) -> Option<MathToken> {
    let rest = &source[start..];
    let (opening, closing) = if rest.starts_with("$$") {
        ("$$", "$$")
    } else if rest.starts_with("\\(") {
        ("\\(", "\\)")
    } else if rest.starts_with("\\[") {
        ("\\[", "\\]")
    } else if rest.starts_with('$') && !rest[1..].starts_with(char::is_whitespace) {
        ("$", "$")
    } else {
        return None;
    };
    let body_start = start + opening.len();
    if block_start && matches!(opening, "$$" | "\\[") {
        let source = &source[..container_end];
        // Pi's block regexp deliberately differs from inline closing escapes.
        for (relative, _) in source[body_start..].match_indices(closing) {
            let end = body_start + relative;
            let after = source[end + closing.len()..].trim_start_matches([' ', '\t', '\r']);
            if end > body_start && (after.is_empty() || after.starts_with('\n')) {
                return Some(MathToken {
                    range: start..end + closing.len(),
                    body: body_start..end,
                    display: true,
                    pending: false,
                    container_prefix: String::new(),
                });
            }
        }
        if opening == "\\[" || pending_dollar(&source[body_start..]) {
            return Some(MathToken {
                range: start..source.len(),
                body: body_start..source.len(),
                display: true,
                pending: true,
                container_prefix: String::new(),
            });
        }
    }
    let source = &source[..inline_end];
    let end = source[body_start..]
        .match_indices(closing)
        .map(|(i, _)| body_start + i)
        .find(|i| !escaped(source, *i));
    let Some(end) = end else {
        return (opening.starts_with('\\') || pending_dollar(&source[body_start..])).then_some(
            MathToken {
                range: start..source.len(),
                body: body_start..source.len(),
                display: false,
                pending: true,
                container_prefix: String::new(),
            },
        );
    };
    let body = &source[body_start..end];
    let after = &source[end + closing.len()..];
    if opening == "$"
        && (body.ends_with(char::is_whitespace)
            || after.starts_with(|c: char| c.is_ascii_digit())
            || shell_variable(body, after)
            || body.contains('`'))
    {
        return None;
    }
    if body.is_empty() || body.contains('\n') {
        return None;
    }
    Some(MathToken {
        range: start..end + closing.len(),
        body: body_start..end,
        display: false,
        pending: false,
        container_prefix: String::new(),
    })
}
