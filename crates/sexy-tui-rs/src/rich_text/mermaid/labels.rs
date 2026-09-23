// Copyright 2023-2026 SpaceXAI
// Copyright 2026 Alexey Zaytsev
// SPDX-License-Identifier: Apache-2.0
// Adapted from grok-build / grok-mermaid 0.2.3 label cleanup, with Rust 2021
// syntax and module visibility changes. See LICENSE-APACHE in this directory.

pub(super) fn clean_label(raw: &str) -> String {
    let stripped = strip_html_tags(raw.trim());
    let trimmed = stripped.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|t| t.strip_suffix('\''))
        })
        .unwrap_or(trimmed)
        .trim();
    let text = if let Some(md) = unquoted.strip_prefix('`').and_then(|t| t.strip_suffix('`')) {
        strip_markdown(md.trim())
    } else {
        unquoted.to_string()
    };
    // Decode after tag-stripping so `<b>` is removed as markup while `&lt;b&gt;` survives as a literal `<b>`
    // One decode at the single return covers both paths
    decode_html_entities(&text)
}

const ENTITY_LOOKAHEAD: usize = 10;

// Label text decodes HTML entities once: via clean_label for bracketed labels, or explicitly at each direct-push sink.
fn decode_html_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        let Some(&c) = chars.get(i) else {
            break;
        };
        if c != '&' {
            out.push(c);
            i += 1;
            continue;
        }
        // Scan window (includes the terminating `;`) so a stray `&` or over-long run stays literal.
        let hi = (i + 1 + ENTITY_LOOKAHEAD).min(chars.len());
        let semi = (i + 1..hi).find(|&j| chars.get(j) == Some(&';'));
        let decoded = semi.and_then(|j| {
            let body: String = chars.get(i + 1..j)?.iter().collect();
            decode_entity_body(&body).map(|c| (c, j))
        });
        match decoded {
            // Resume past the `;`; the single pass never re-scans emitted text, so `&amp;lt;` decodes to the literal `&lt;` rather than to `<`
            Some((c, j)) => {
                out.push(c);
                i = j + 1;
            }
            None => {
                out.push('&');
                i += 1;
            }
        }
    }
    out
}

fn decode_entity_body(body: &str) -> Option<char> {
    match body {
        "lt" => Some('<'),
        "gt" => Some('>'),
        "amp" => Some('&'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let num = body.strip_prefix('#')?;
            let code = match num.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => num.parse::<u32>().ok()?,
            };
            // Reject control chars: NUL collides with the CONT sentinel and ESC would inject ANSI into scrollback.
            char::from_u32(code).filter(|c| !c.is_control())
        }
    }
}

fn strip_markdown(s: &str) -> String {
    let no_code: String = s.chars().filter(|&c| c != '`').collect();
    let no_strong = no_code.replace("**", "").replace("__", "");
    let chars: Vec<char> = no_strong.chars().collect();
    let mut out = String::with_capacity(no_strong.len());
    for (i, &c) in chars.iter().enumerate() {
        if (c == '*' || c == '_')
            && !(i > 0
                && i.checked_sub(1)
                    .and_then(|j| chars.get(j))
                    .is_some_and(|p| p.is_alphanumeric())
                && chars.get(i + 1).is_some_and(|n| n.is_alphanumeric()))
        {
            continue;
        }
        out.push(c);
    }
    out.trim().to_string()
}

const HTML_FORMAT_TAGS: &[&str] = &[
    "b", "strong", "i", "em", "u", "s", "strike", "del", "ins", "mark", "small", "big", "sub",
    "sup", "code", "kbd", "samp", "var", "tt", "span", "font", "q", "abbr", "cite", "pre",
];

fn strip_html_tags(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if let Some((name, end)) = (chars.get(i) == Some(&'<'))
            .then(|| html_tag_at(&chars, i))
            .flatten()
        {
            let lower = name.to_ascii_lowercase();
            if lower == "br" {
                out.push(' ');
                i = end;
                continue;
            }
            if HTML_FORMAT_TAGS.contains(&lower.as_str()) {
                i = end;
                continue;
            }
        }
        let Some(&c) = chars.get(i) else {
            break;
        };
        out.push(c);
        i += 1;
    }
    out
}

fn html_tag_at(chars: &[char], start: usize) -> Option<(String, usize)> {
    let mut i = start + 1;
    if chars.get(i) == Some(&'/') {
        i += 1;
    }
    let name_start = i;
    while chars.get(i).is_some_and(|c| c.is_ascii_alphanumeric()) {
        i += 1;
    }
    if i == name_start {
        return None;
    }
    let name: String = chars.get(name_start..i)?.iter().collect();
    while chars.get(i).is_some_and(|&c| c != '>') {
        if chars.get(i) == Some(&'<') {
            return None;
        }
        i += 1;
    }
    if chars.get(i) == Some(&'>') {
        Some((name, i + 1))
    } else {
        None
    }
}
