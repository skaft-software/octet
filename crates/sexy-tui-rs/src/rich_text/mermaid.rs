//! Bounded terminal Mermaid flowcharts, using Pi's layout algorithms.
//!
//! Pi delegates to grok-mermaid 0.2.3, a TypeScript port of grok-build's Rust
//! renderer. The Apache-2.0 Rust layout and label-cleanup code is adapted in
//! the child modules (with retained license/attribution), without ratatui,
//! JavaScript, subprocesses, I/O or new dependencies at build/run time.
//!
//! Supports `graph`/`flowchart` TD/TB/BT/LR/RL, nested subgraphs, group links,
//! node lists, chained links, inline and pipe labels, class/style annotations,
//! cycles, self-links, and skip-layer links. Dotted/thick strokes and rounded
//! node outlines follow Pi. `<br/>` becomes whitespace, then node labels wrap
//! at 24 cells / 4 lines, as in Pi. Cross-group edges attach to the enclosing
//! group frame; local `direction` directives are accepted but ignored by Pi.
//!
//! Parsing remains fail-closed: malformed syntax, unknown arrows/diagram
//! families, invalid nesting and over-limit source return a typed error, not
//! partial art. State/class/ER/sequence diagrams and non-arrow edge heads are
//! not yet supported. Dotted/hyphenated IDs remain an octet extension. Inline
//! labels containing punctuation (e.g. `-.path ./file.->`) parse completely,
//! unlike grok-mermaid 0.2.3's warning-producing prefix parse.
//!
//! [`MermaidArt::width`] is the actual terminal-cell width, not a requested
//! viewport width. The embedding renderer must keep source when art is wider
//! than its viewport; never wrap/crop a graph's rows. All graph/layout loops
//! are bounded by source/node/edge/group and canvas caps. The canvas is checked
//! before allocation (at most 2^21 cells, 4096 columns, 2048 rows).
//!
//! `tests/mermaid_parity.rs` uses real grok-mermaid output, including the full
//! reported 64-node architecture graph. See the Mermaid fixture README for
//! upstream evidence, the equivalent edge spelling used by that oracle, and
//! the precise difference from Pi's final-warning fallback.

mod labels;
mod layout;
use labels::clean_label as normalize_label;

use crate::width::display_width;

/// Maximum accepted source size.
pub const MAX_MERMAID_SOURCE_BYTES: usize = 16 * 1024;
/// Maximum accepted node count.
pub const MAX_MERMAID_NODES: usize = 128;
/// Maximum accepted edge count.
pub const MAX_MERMAID_EDGES: usize = 512;
/// Maximum accepted node/edge label width in cells.
pub const MAX_MERMAID_LABEL_CELLS: usize = 1024;
/// Maximum rendered diagram width in cells.
pub const MAX_MERMAID_ART_WIDTH: usize = 4096;
/// Maximum rendered diagram height in rows.
pub const MAX_MERMAID_ART_HEIGHT: usize = 2048;

/// Why a diagram could not be rendered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MermaidError {
    /// The source has no `graph`/`flowchart` header line.
    MissingHeader,
    /// The header names a diagram type this engine does not support.
    UnsupportedDiagram { header: String },
    /// A line uses syntax outside the supported subset.
    UnsupportedSyntax { line: usize, detail: String },
    /// The graph shape cannot be routed by this engine.
    UnsupportedTopology { detail: String },
    /// The graph contains a cycle.
    Cycle { node: String },
    /// The input or the rendered diagram exceeds a hard limit.
    TooLarge { detail: String },
}

impl std::fmt::Display for MermaidError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingHeader => {
                write!(formatter, "dropped, expected a graph or flowchart header")
            }
            Self::UnsupportedDiagram { header } => {
                write!(formatter, "dropped, unsupported diagram type: \"{header}\"")
            }
            Self::UnsupportedSyntax { line, detail } => {
                write!(formatter, "dropped, line {line}: {detail}")
            }
            Self::UnsupportedTopology { detail } => write!(formatter, "dropped, {detail}"),
            Self::Cycle { node } => write!(formatter, "dropped, cycle through node \"{node}\""),
            Self::TooLarge { detail } => write!(formatter, "dropped, {detail}"),
        }
    }
}

impl std::error::Error for MermaidError {}

/// A rendered diagram: one plain-text line per terminal row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MermaidArt {
    pub lines: Vec<String>,
    pub width: usize,
}

impl MermaidArt {
    /// Diagram rows joined by newlines.
    pub fn plain(&self) -> String {
        self.lines.join("\n")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    LeftRight,
    TopDown,
    RightLeft,
    BottomUp,
}

#[derive(Clone, Debug)]
struct Node {
    id: String,
    label: String,
    group: Option<usize>,
    rounded: bool,
}

#[derive(Clone, Debug)]
struct Group {
    id: String,
    title: String,
    parent: Option<usize>,
}

#[derive(Clone, Debug)]
struct Edge {
    from: usize,
    to: usize,
    label: Option<String>,
    /// `false` for the undirected `---` link, which draws a plain connector
    /// instead of an arrow head.
    directed: bool,
    stroke: Stroke,
}

#[derive(Clone, Copy, Debug)]
enum Stroke {
    Solid,
    Dotted,
    Thick,
}

/// Render a Mermaid `graph`/`flowchart` block as box-drawing text.
pub fn render_mermaid(source: &str) -> Result<MermaidArt, MermaidError> {
    if source.len() > MAX_MERMAID_SOURCE_BYTES {
        return Err(MermaidError::TooLarge {
            detail: format!(
                "source is {} bytes, limit is {MAX_MERMAID_SOURCE_BYTES}",
                source.len()
            ),
        });
    }
    if source
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(syntax(
            1,
            "terminal control characters are not allowed".into(),
        ));
    }
    let (direction, nodes, edges, groups) = parse(source)?;
    layout::layout(direction, nodes, &edges, &groups)
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

const LINK_TOKENS: &[&str] = &["-.->", "==>", "-->", "---", "->"];
const DIRECTIVE_KEYWORDS: &[&str] = &["classdef", "class", "style", "linkstyle", "click"];
/// Drop a `%%` comment, which Mermaid allows anywhere outside a quoted label.
fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    let mut previous = '\0';
    for (index, character) in line.char_indices() {
        match character {
            '"' if previous != '\\' => quoted = !quoted,
            '%' if !quoted && previous == '%' => return &line[..index - 1],
            _ => {}
        }
        previous = character;
    }
    line
}

/// Split a line on `;` statement terminators, ignoring semicolons inside quoted
/// labels and bracket pairs.
fn split_statements(line: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    let mut quoted = false;
    let mut depth = 0usize;
    let mut previous = '\0';
    for (index, character) in line.char_indices() {
        match character {
            '"' if previous != '\\' => quoted = !quoted,
            '[' | '(' | '{' if !quoted => depth = depth.saturating_add(1),
            ']' | ')' | '}' if !quoted => depth = depth.saturating_sub(1),
            ';' if !quoted && depth == 0 => {
                segments.push(&line[start..index]);
                start = index + 1;
            }
            _ => {}
        }
        previous = character;
    }
    segments.push(&line[start..]);
    segments
}

type ParsedGraph = (Direction, Vec<Node>, Vec<Edge>, Vec<Group>);

fn parse(source: &str) -> Result<ParsedGraph, MermaidError> {
    let mut direction: Option<Direction> = None;
    let mut nodes: Vec<Node> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut groups: Vec<Group> = Vec::new();
    let mut group_stack = Vec::new();

    for (index, raw_line) in source.lines().enumerate() {
        let line_number = index + 1;
        // `%%` starts a comment anywhere on a line, outside quoted labels.
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        // `;` terminates a statement, so the header may share a line with the
        // first statements (`flowchart LR; A --> B; B --> C`).
        for statement in split_statements(line) {
            let statement = statement.trim();
            if statement.is_empty() {
                continue;
            }
            if direction.is_none() {
                direction = Some(parse_header(statement)?);
                continue;
            }
            let lowered = statement.to_ascii_lowercase();
            if DIRECTIVE_KEYWORDS
                .iter()
                .any(|keyword| lowered.starts_with(keyword) && boundary(&lowered[keyword.len()..]))
            {
                continue;
            }
            if lowered.starts_with("subgraph") && boundary(&lowered[8..]) {
                if groups.len() >= 24 || group_stack.len() >= 6 {
                    return Err(MermaidError::TooLarge {
                        detail: "more than 24 subgraphs or nesting exceeds 6".into(),
                    });
                }
                let (id, title) = parse_group(statement[8..].trim(), line_number)?;
                if groups.iter().any(|group| group.id == id) {
                    return Err(syntax(line_number, format!("duplicate subgraph id `{id}`")));
                }
                let parent = group_stack.last().copied();
                groups.push(Group { id, title, parent });
                group_stack.push(groups.len() - 1);
                continue;
            }
            if lowered == "end" {
                if group_stack.pop().is_none() {
                    return Err(syntax(line_number, "`end` without a subgraph".into()));
                }
                continue;
            }
            if lowered.starts_with("direction") && boundary(&lowered[9..]) {
                // grok-mermaid accepts but ignores per-subgraph direction.
                parse_header(&format!("graph {}", statement[9..].trim()))?;
                continue;
            }
            parse_statement(
                statement,
                line_number,
                &mut nodes,
                &mut edges,
                group_stack.last().copied(),
            )?;
            if nodes.len() > MAX_MERMAID_NODES {
                return Err(MermaidError::TooLarge {
                    detail: format!("diagram has more than {MAX_MERMAID_NODES} nodes"),
                });
            }
            if edges.len() > MAX_MERMAID_EDGES {
                return Err(MermaidError::TooLarge {
                    detail: format!("diagram has more than {MAX_MERMAID_EDGES} links"),
                });
            }
        }
    }

    let Some(direction) = direction else {
        return Err(MermaidError::MissingHeader);
    };
    if !group_stack.is_empty() {
        return Err(syntax(source.lines().count(), "unclosed subgraph".into()));
    }
    Ok((direction, nodes, edges, groups))
}

fn boundary(rest: &str) -> bool {
    rest.is_empty() || rest.starts_with(char::is_whitespace)
}

fn parse_header(line: &str) -> Result<Direction, MermaidError> {
    // Mermaid tolerates a trailing statement terminator on the header line.
    let line = line.trim_end().trim_end_matches(';').trim_end();
    let mut parts = line.split_whitespace();
    let keyword = parts.next().unwrap_or_default().to_ascii_lowercase();
    if keyword != "graph" && keyword != "flowchart" {
        return Err(MermaidError::UnsupportedDiagram {
            header: line.split_whitespace().next().unwrap_or(line).to_owned(),
        });
    }
    let direction = match parts.next().map(str::to_ascii_uppercase).as_deref() {
        None | Some("TD") | Some("TB") => Direction::TopDown,
        Some("LR") => Direction::LeftRight,
        Some("BT") => Direction::BottomUp,
        Some("RL") => Direction::RightLeft,
        other => {
            return Err(MermaidError::UnsupportedDiagram {
                header: other.unwrap_or(line).to_owned(),
            })
        }
    };
    if parts.next().is_some() {
        return Err(MermaidError::UnsupportedSyntax {
            line: 1,
            detail: format!("unexpected trailing header tokens in \"{line}\""),
        });
    }
    Ok(direction)
}

fn node_index(
    nodes: &mut Vec<Node>,
    id: &str,
    label: Option<String>,
    group: Option<usize>,
) -> usize {
    if let Some(index) = nodes.iter().position(|node| node.id == id) {
        if label.is_some() {
            nodes[index].label = label.unwrap_or_default();
        }
        return index;
    }
    nodes.push(Node {
        id: id.to_owned(),
        label: label.unwrap_or_else(|| id.to_owned()),
        group,
        rounded: false,
    });
    nodes.len() - 1
}

fn parse_group(value: &str, line: usize) -> Result<(String, String), MermaidError> {
    if value.is_empty() {
        return Err(syntax(line, "expected a subgraph title".into()));
    }
    let chars: Vec<char> = value.chars().collect();
    let mut position = 0;
    let mut id = read_id(&chars, &mut position);
    position = skip_spaces(&chars, position);
    let label = if matches!(chars.get(position), Some('[')) {
        let label = read_label(&chars, &mut position, line)?.unwrap();
        if skip_spaces(&chars, position) != chars.len() {
            return Err(syntax(
                line,
                "unexpected tokens after subgraph title".into(),
            ));
        }
        label
    } else {
        id = unquote(value);
        normalize_label(value)
    };
    check_label_size(&label, line)?;
    Ok((id, label))
}

fn parse_statement(
    statement: &str,
    line: usize,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
    group: Option<usize>,
) -> Result<(), MermaidError> {
    let characters: Vec<char> = statement.chars().collect();
    let mut position = 0usize;
    let mut from = read_node_list(&characters, &mut position, line, nodes, group)?;
    while position < characters.len() {
        let (directed, stroke, inline_label, next) = read_edge(&characters, position, line)?;
        position = skip_spaces(&characters, next);
        let label = if inline_label.is_some() {
            inline_label
        } else {
            read_link_label(&characters, &mut position, line)?
        };
        if let Some(label) = &label {
            check_label_size(label, line)?;
        }
        position = skip_spaces(&characters, position);
        if position == characters.len() {
            return Err(syntax(line, "trailing link without a target node".into()));
        }
        let to = read_node_list(&characters, &mut position, line, nodes, group)?;
        for &source in &from {
            for &target in &to {
                if edges.len() >= MAX_MERMAID_EDGES {
                    return Err(MermaidError::TooLarge {
                        detail: format!("diagram has more than {MAX_MERMAID_EDGES} links"),
                    });
                }
                edges.push(Edge {
                    from: source,
                    to: target,
                    label: label.clone(),
                    directed,
                    stroke,
                });
            }
        }
        from = to;
    }
    Ok(())
}

fn read_node_list(
    chars: &[char],
    position: &mut usize,
    line: usize,
    nodes: &mut Vec<Node>,
    group: Option<usize>,
) -> Result<Vec<usize>, MermaidError> {
    let mut result = Vec::new();
    loop {
        *position = skip_spaces(chars, *position);
        let start = *position;
        let id = read_id(chars, position);
        if id.is_empty() {
            return Err(syntax(
                line,
                format!("expected a node id at \"{}\"", rest(chars, start)),
            ));
        }
        *position = skip_spaces(chars, *position);
        let rounded = matches!(chars.get(*position), Some('(' | '{'))
            || (chars.get(*position) == Some(&'[') && chars.get(*position + 1) == Some(&'('));
        let label = read_label(chars, position, line)?;
        let has_label = label.is_some();
        check_label_size(label.as_deref().unwrap_or(&id), line)?;
        *position = skip_class_annotation(chars, *position);
        let index = node_index(nodes, &id, label, group);
        if nodes.len() > MAX_MERMAID_NODES {
            return Err(MermaidError::TooLarge {
                detail: format!("diagram has more than {MAX_MERMAID_NODES} nodes"),
            });
        }
        if has_label {
            nodes[index].rounded = rounded;
        }
        result.push(index);
        *position = skip_spaces(chars, *position);
        if chars.get(*position) != Some(&'&') {
            return Ok(result);
        }
        *position += 1;
    }
}

/// Mermaid's inline edge labels use different opening/closing strokes.
fn read_edge(
    chars: &[char],
    position: usize,
    line: usize,
) -> Result<(bool, Stroke, Option<String>, usize), MermaidError> {
    if let Some((token, next)) = read_link(chars, position) {
        return Ok((token != "---", stroke(token), None, next));
    }
    let remaining: String = chars[position..].iter().collect();
    for (open, close) in [("-.", ".->"), ("--", "-->"), ("==", "==>")] {
        if let Some(tail) = remaining.strip_prefix(open) {
            if let Some(end) = tail.find(close) {
                let label = normalize_label(&unquote(tail[..end].trim()));
                if label.is_empty() {
                    break;
                }
                return Ok((
                    true,
                    stroke(open),
                    Some(label),
                    position + open.chars().count() + tail[..end].chars().count() + close.len(),
                ));
            }
        }
    }
    Err(syntax(
        line,
        format!("expected a link, found \"{}\"", rest(chars, position)),
    ))
}

fn stroke(token: &str) -> Stroke {
    if token.contains('=') {
        Stroke::Thick
    } else if token.contains('.') {
        Stroke::Dotted
    } else {
        Stroke::Solid
    }
}

fn rest(characters: &[char], from: usize) -> String {
    characters[from..]
        .iter()
        .collect::<String>()
        .trim()
        .to_owned()
}

fn skip_spaces(characters: &[char], mut position: usize) -> usize {
    while position < characters.len() && characters[position].is_whitespace() {
        position += 1;
    }
    position
}

fn skip_class_annotation(characters: &[char], mut position: usize) -> usize {
    loop {
        let mut cursor = skip_spaces(characters, position);
        if characters.get(cursor) == Some(&':') && characters.get(cursor + 1) == Some(&':') {
            cursor += 2;
            while characters.get(cursor) == Some(&':') {
                cursor += 1;
            }
            while cursor < characters.len()
                && (characters[cursor].is_alphanumeric()
                    || characters[cursor] == '_'
                    || characters[cursor] == '-')
            {
                cursor += 1;
            }
            // Class names may contain `-`, but the edge in `A:::c-->B`
            // starts immediately after the name.
            while cursor > position && characters.get(cursor - 1) == Some(&'-') {
                cursor -= 1;
            }
            position = cursor;
            continue;
        }
        return position;
    }
}

fn read_id(characters: &[char], position: &mut usize) -> String {
    let mut id = String::new();
    while *position < characters.len() {
        let character = characters[*position];
        if character.is_alphanumeric() || character == '_' || character == '.' {
            id.push(character);
            *position += 1;
            continue;
        }
        if character == '-'
            && !matches!(characters.get(*position + 1), Some('-' | '.'))
            && read_link(characters, *position).is_none()
        {
            id.push(character);
            *position += 1;
            continue;
        }
        break;
    }
    id
}

fn read_link(characters: &[char], position: usize) -> Option<(&'static str, usize)> {
    for token in LINK_TOKENS {
        let token_chars: Vec<char> = token.chars().collect();
        if position + token_chars.len() <= characters.len()
            && characters[position..position + token_chars.len()] == token_chars[..]
        {
            return Some((token, position + token_chars.len()));
        }
    }
    None
}

fn read_link_label(
    characters: &[char],
    position: &mut usize,
    line: usize,
) -> Result<Option<String>, MermaidError> {
    if characters.get(*position) != Some(&'|') {
        return Ok(None);
    }
    let start = *position + 1;
    let mut cursor = start;
    while cursor < characters.len() && characters[cursor] != '|' {
        cursor += 1;
    }
    if cursor >= characters.len() {
        return Err(syntax(line, "unterminated link label".to_owned()));
    }
    let label: String = characters[start..cursor].iter().collect();
    *position = cursor + 1;
    Ok(Some(normalize_label(&unquote(label.trim()))))
}

fn read_label(
    characters: &[char],
    position: &mut usize,
    line: usize,
) -> Result<Option<String>, MermaidError> {
    let (open, close) = match characters.get(*position) {
        Some('[') if characters.get(*position + 1) == Some(&'[') => ("[[", "]]"),
        Some('[') if characters.get(*position + 1) == Some(&'(') => ("[(", ")]"),
        Some('>') => (">", "]"),
        Some('(') if characters.get(*position + 1) == Some(&'(') => ("((", "))"),
        Some('(') if characters.get(*position + 1) == Some(&'[') => ("([", "])"),
        Some('{') if characters.get(*position + 1) == Some(&'{') => ("{{", "}}"),
        Some('[') => ("[", "]"),
        Some('(') => ("(", ")"),
        Some('{') => ("{", "}"),
        _ => return Ok(None),
    };
    let start = skip_spaces(characters, *position + open.chars().count());
    let closing: Vec<char> = close.chars().collect();
    // A quoted label may contain the closing delimiter (`A["a[b]c"]`), so a
    // leading quote is closed by its matching quote followed by the delimiter.
    if characters.get(start) == Some(&'"') {
        let mut quote_end = start + 1;
        while quote_end < characters.len() && characters[quote_end] != '"' {
            quote_end += 1;
        }
        if quote_end >= characters.len() {
            return Err(syntax(
                line,
                format!("unterminated quoted label opened with `{open}`"),
            ));
        }
        let close_start = skip_spaces(characters, quote_end + 1);
        if characters.len() - close_start < closing.len()
            || characters[close_start..close_start + closing.len()] != closing[..]
        {
            return Err(syntax(
                line,
                format!("quoted label opened with `{open}` is not closed by `{close}`"),
            ));
        }
        let raw: String = characters[start + 1..quote_end].iter().collect();
        *position = close_start + closing.len();
        return Ok(Some(normalize_label(&raw)));
    }
    let mut cursor = start;
    while cursor < characters.len() {
        if characters.len() - cursor >= closing.len()
            && characters[cursor..cursor + closing.len()] == closing[..]
        {
            let raw: String = characters[start..cursor].iter().collect();
            *position = cursor + closing.len();
            return Ok(Some(normalize_label(&unquote(raw.trim()))));
        }
        cursor += 1;
    }
    Err(syntax(
        line,
        format!("unbalanced node label opened with `{open}`"),
    ))
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        return trimmed[1..trimmed.len() - 1].to_owned();
    }
    trimmed.to_owned()
}

fn check_label_size(label: &str, line: usize) -> Result<(), MermaidError> {
    if label.chars().any(|c| c.is_control() && c != '\n') {
        return Err(syntax(
            line,
            "terminal control characters are not allowed in labels".into(),
        ));
    }
    if label.split('\n').count() > 8
        || label
            .split('\n')
            .any(|row| display_width(row) > MAX_MERMAID_LABEL_CELLS)
    {
        return Err(MermaidError::TooLarge {
            detail: format!("line {line}: label wider than {MAX_MERMAID_LABEL_CELLS} cells"),
        });
    }
    Ok(())
}

fn syntax(line: usize, detail: String) -> MermaidError {
    MermaidError::UnsupportedSyntax { line, detail }
}
