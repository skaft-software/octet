//! Bounded, self-contained Mermaid `graph`/`flowchart` renderer.
//!
//! Upstream delegates diagram layout to the external `grok-mermaid` package
//! (`packages/coding-agent/src/modes/interactive/components/mermaid.ts`), which
//! this workspace cannot depend on (no network dependency in a renderer).
//! [`render_mermaid`] is an honest subset: it parses `graph`/`flowchart` with
//! `TD`/`TB`/`LR` direction, node definitions with labels, and chained/plain
//! links, layers the graph, lays it out on a character grid and emits
//! box-drawing output.
//!
//! # Contract
//!
//! [`render_mermaid`] returns [`MermaidArt`] (one plain-text line per terminal
//! row plus the widest row's cell width) or a typed [`MermaidError`]. It never
//! panics, never blocks on I/O, never returns a partially drawn diagram, and
//! never exceeds the size limits exported by this module.
//!
//! # Supported
//!
//! - header: `graph <dir>` / `flowchart <dir>` with `TD`, `TB` or `LR`, with an
//!   optional trailing `;`
//! - node ids (`[A-Za-z0-9_.-]`, stopped at a link token) with or without labels
//! - shapes `id[label]`, `id(label)`, `id{label}`, `id((label))`, `id([label])`,
//!   `id[[label]]`, `id{{label}}` — every shape renders as a box (the shape
//!   outline itself is not modelled)
//! - `:::class` decorations are ignored, as are `classDef`/`class`/`style`/
//!   `linkStyle`/`click` directives and `%%` comments
//! - links `-->`, `->`, `-.->`, `==>` (arrow head) and `---` (no head), each
//!   with an optional `|label|`; link *styling* is not modelled, so `-.->` and
//!   `==>` draw the same solid connector as `-->`
//! - quoted labels, wide (CJK) labels, and disconnected components (rendered as
//!   separate bands of rows)
//!
//! # Fails closed (typed [`MermaidError`], never a panic or unbounded work)
//!
//! - any other diagram type (`pie`, `sequenceDiagram`, `stateDiagram`, …)
//! - `BT`/`RL` layouts (accepted by Mermaid, but mirroring the grid would
//!   reverse node labels, so they are rejected rather than misrendered)
//! - subgraphs, `&` node lists, `A -- text --> B` inline link labels, other
//!   arrow tokens, unbalanced node brackets
//! - cyclic graphs and any edge that skips a layer (a longer path exists), so
//!   routing stays inside the gap between two adjacent layers
//! - inputs over the size limits in this module
//!
//! # Bounds
//!
//! Source bytes, node count, edge count, label width and the rendered diagram
//! size are all capped by the `MAX_MERMAID_*` constants; over-limit input
//! returns [`MermaidError::TooLarge`]. Layering is Kahn's algorithm (linear),
//! and every layout loop runs a bounded number of times over at most the
//! accepted node/edge counts.
//!
//! # Output and consumers
//!
//! The output is plain text. Upstream returns semantic style spans
//! (`border`/`text`/`edge`/…) and a warnings channel; theming and the
//! "unrendered diagram" fallback belong to the embedding component, which this
//! engine reports through `Err` instead of partially-rendered output. The
//! embedding component should also map `Err` onto upstream's
//! "could not render" fallback text.
//!
//! # Tests
//!
//! `crates/sexy-tui-rs/tests/mermaid_render.rs` locks the observed box-drawing
//! output for every supported construct, the fail-closed error messages, the
//! width invariant (no row wider than [`MermaidArt::width`]) and the size
//! limits.

use crate::width::display_width;

/// Maximum accepted source size.
pub const MAX_MERMAID_SOURCE_BYTES: usize = 16 * 1024;
/// Maximum accepted node count.
pub const MAX_MERMAID_NODES: usize = 64;
/// Maximum accepted edge count.
pub const MAX_MERMAID_EDGES: usize = 256;
/// Maximum accepted node/edge label width in cells.
pub const MAX_MERMAID_LABEL_CELLS: usize = 48;
/// Maximum rendered diagram width in cells.
pub const MAX_MERMAID_ART_WIDTH: usize = 400;
/// Maximum rendered diagram height in rows.
pub const MAX_MERMAID_ART_HEIGHT: usize = 200;

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
            Self::MissingHeader => write!(formatter, "dropped, expected a graph or flowchart header"),
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
}

#[derive(Clone, Debug)]
struct Node {
    id: String,
    label: String,
    /// Layer index (column for `LR`, row band for `TD`).
    layer: usize,
    /// Position inside the layer.
    slot: usize,
    x: usize,
    y: usize,
}

impl Node {
    fn box_width(&self) -> usize {
        display_width(&self.label) + 4
    }
}

#[derive(Clone, Debug)]
struct Edge {
    from: usize,
    to: usize,
    label: Option<String>,
    /// `false` for the undirected `---` link, which draws a plain connector
    /// instead of an arrow head.
    directed: bool,
}

/// Render a Mermaid `graph`/`flowchart` block as box-drawing text.
pub fn render_mermaid(source: &str) -> Result<MermaidArt, MermaidError> {
    if source.len() > MAX_MERMAID_SOURCE_BYTES {
        return Err(MermaidError::TooLarge {
            detail: format!("source is {} bytes, limit is {MAX_MERMAID_SOURCE_BYTES}", source.len()),
        });
    }
    let (direction, nodes, edges) = parse(source)?;
    let (mut nodes, edges) = layer(direction, nodes, edges)?;
    let canvas = match direction {
        Direction::LeftRight => layout_left_right(&mut nodes, &edges),
        Direction::TopDown => layout_top_down(&mut nodes, &edges),
    };
    if canvas.width > MAX_MERMAID_ART_WIDTH || canvas.rows.len() > MAX_MERMAID_ART_HEIGHT {
        return Err(MermaidError::TooLarge {
            detail: format!(
                "diagram is {}x{}, limit is {}x{}",
                canvas.width,
                canvas.rows.len(),
                MAX_MERMAID_ART_WIDTH,
                MAX_MERMAID_ART_HEIGHT
            ),
        });
    }
    let lines: Vec<String> = canvas.rows.iter().map(|row| row_to_line(row)).collect();
    let width = lines.iter().map(|line| display_width(line)).max().unwrap_or(0);
    Ok(MermaidArt { lines, width })
}

/// Join one grid row, dropping the grid cell that a double-width glyph covers.
///
/// A wide glyph occupies two terminal columns but one grid cell, so the cell a
/// `Canvas::text` caller advanced past must not be emitted: the terminal
/// already advances two columns for the glyph itself. Without this the right
/// border of every box with a CJK label drifts one column right.
fn row_to_line(row: &[char]) -> String {
    let mut line = String::with_capacity(row.len());
    let mut covered = 0usize;
    for &character in row {
        if covered > 0 {
            covered -= 1;
            continue;
        }
        covered = display_width(&character.to_string()).saturating_sub(1);
        line.push(character);
    }
    line.trim_end().to_owned()
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

const LINK_TOKENS: &[&str] = &["-.->", "==>", "-->", "---", "->"];
const DIRECTIVE_KEYWORDS: &[&str] = &["classdef", "class", "style", "linkstyle", "click"];

fn parse(source: &str) -> Result<(Direction, Vec<Node>, Vec<Edge>), MermaidError> {
    let mut direction: Option<Direction> = None;
    let mut nodes: Vec<Node> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();

    for (index, raw_line) in source.lines().enumerate() {
        let line_number = index + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        if direction.is_none() {
            direction = Some(parse_header(line)?);
            continue;
        }
        let statement = line.trim_end_matches(';').trim();
        if statement.is_empty() {
            continue;
        }
        let lowered = statement.to_ascii_lowercase();
        if DIRECTIVE_KEYWORDS
            .iter()
            .any(|keyword| lowered.starts_with(keyword) && boundary(&lowered[keyword.len()..]))
        {
            continue;
        }
        parse_statement(statement, line_number, &mut nodes, &mut edges)?;
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

    let Some(direction) = direction else {
        return Err(MermaidError::MissingHeader);
    };
    Ok((direction, nodes, edges))
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
        Some("TD") | Some("TB") => Direction::TopDown,
        Some("LR") => Direction::LeftRight,
        // `BT`/`RL` would need a mirrored grid, which would reverse the node
        // labels; reject instead of drawing the wrong picture.
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

fn node_index(nodes: &mut Vec<Node>, id: &str, label: Option<String>) -> usize {
    if let Some(index) = nodes.iter().position(|node| node.id == id) {
        if label.is_some() {
            nodes[index].label = label.unwrap_or_default();
        }
        return index;
    }
    nodes.push(Node {
        id: id.to_owned(),
        label: label.unwrap_or_else(|| id.to_owned()),
        layer: 0,
        slot: 0,
        x: 0,
        y: 0,
    });
    nodes.len() - 1
}

fn parse_statement(
    statement: &str,
    line: usize,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) -> Result<(), MermaidError> {
    let characters: Vec<char> = statement.chars().collect();
    let mut position = 0usize;
    let mut pending_from: Option<usize> = None;
    let mut pending_label: Option<Option<String>> = None;
    let mut pending_directed = true;

    loop {
        position = skip_spaces(&characters, position);
        let start = position;
        let id = read_id(&characters, &mut position);
        if id.is_empty() {
            return Err(syntax(
                line,
                format!("expected a node id at \"{}\"", rest(&characters, start)),
            ));
        }
        let label = read_label(&characters, &mut position, line)?;
        if let Some(label) = &label {
            check_label_size(label, line)?;
        }
        position = skip_class_annotation(&characters, position);
        let index = node_index(nodes, &id, label);
        if let Some(from) = pending_from {
            edges.push(Edge {
                from,
                to: index,
                label: pending_label.take().flatten(),
                directed: pending_directed,
            });
        }

        position = skip_spaces(&characters, position);
        if position >= characters.len() {
            return Ok(());
        }
        if characters[position] == '&' {
            return Err(syntax(line, "node lists with `&` are not supported".to_owned()));
        }
        let (token, next) = read_link(&characters, position).ok_or_else(|| {
            syntax(
                line,
                format!("expected a link, found \"{}\"", rest(&characters, position)),
            )
        })?;
        position = skip_spaces(&characters, next);
        let label = read_link_label(&characters, &mut position, line)?;
        if let Some(label) = &label {
            check_label_size(label, line)?;
        }
        position = skip_spaces(&characters, position);
        if position >= characters.len() {
            return Err(syntax(line, "trailing link without a target node".to_owned()));
        }
        pending_from = Some(index);
        pending_label = Some(label);
        pending_directed = token != "---";
    }
}

fn rest(characters: &[char], from: usize) -> String {
    characters[from..].iter().collect::<String>().trim().to_owned()
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
                && (characters[cursor].is_alphanumeric() || characters[cursor] == '_' || characters[cursor] == '-')
            {
                cursor += 1;
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
        if character == '-' && read_link(characters, *position).is_none() {
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
    Ok(Some(label.trim().to_owned()))
}

fn read_label(
    characters: &[char],
    position: &mut usize,
    line: usize,
) -> Result<Option<String>, MermaidError> {
    let (open, close) = match characters.get(*position) {
        Some('[') if characters.get(*position + 1) == Some(&'[') => ("[[", "]]"),
        Some('(') if characters.get(*position + 1) == Some(&'(') => ("((", "))"),
        Some('(') if characters.get(*position + 1) == Some(&'[') => ("([", "])"),
        Some('{') if characters.get(*position + 1) == Some(&'{') => ("{{", "}}"),
        Some('[') => ("[", "]"),
        Some('(') => ("(", ")"),
        Some('{') => ("{", "}"),
        _ => return Ok(None),
    };
    let start = *position + open.chars().count();
    let closing: Vec<char> = close.chars().collect();
    let mut cursor = start;
    while cursor < characters.len() {
        if characters.len() - cursor >= closing.len()
            && characters[cursor..cursor + closing.len()] == closing[..]
        {
            let raw: String = characters[start..cursor].iter().collect();
            *position = cursor + closing.len();
            return Ok(Some(unquote(raw.trim())));
        }
        cursor += 1;
    }
    Err(syntax(line, format!("unbalanced node label opened with `{open}`")))
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        return trimmed[1..trimmed.len() - 1].to_owned();
    }
    trimmed.to_owned()
}

fn check_label_size(label: &str, line: usize) -> Result<(), MermaidError> {
    if display_width(label) > MAX_MERMAID_LABEL_CELLS {
        return Err(MermaidError::TooLarge {
            detail: format!("line {line}: label wider than {MAX_MERMAID_LABEL_CELLS} cells"),
        });
    }
    Ok(())
}

fn syntax(line: usize, detail: String) -> MermaidError {
    MermaidError::UnsupportedSyntax { line, detail }
}

// ---------------------------------------------------------------------------
// Layering
// ---------------------------------------------------------------------------

/// Longest-path layering with cycle detection (Kahn's algorithm).
fn layer(
    _direction: Direction,
    mut nodes: Vec<Node>,
    edges: Vec<Edge>,
) -> Result<(Vec<Node>, Vec<Edge>), MermaidError> {
    let mut successors: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    let mut indegree: Vec<usize> = vec![0; nodes.len()];
    for edge in &edges {
        successors[edge.from].push(edge.to);
        indegree[edge.to] += 1;
    }

    let mut queue: Vec<usize> = (0..nodes.len()).filter(|index| indegree[*index] == 0).collect();
    let mut order: Vec<usize> = Vec::new();
    let mut processed = 0usize;
    while let Some(index) = queue.pop() {
        order.push(index);
        processed += 1;
        for successor in &successors[index] {
            indegree[*successor] -= 1;
            if indegree[*successor] == 0 {
                queue.push(*successor);
            }
        }
    }
    if processed != nodes.len() {
        let stuck = indegree
            .iter()
            .position(|degree| *degree > 0)
            .unwrap_or_default();
        return Err(MermaidError::Cycle {
            node: nodes[stuck].id.clone(),
        });
    }

    for index in order {
        let current = nodes[index].layer;
        for successor in &successors[index] {
            if nodes[*successor].layer < current + 1 {
                nodes[*successor].layer = current + 1;
            }
        }
    }

    for edge in &edges {
        let from = &nodes[edge.from];
        let to = &nodes[edge.to];
        if to.layer != from.layer + 1 {
            return Err(MermaidError::UnsupportedTopology {
                detail: format!(
                    "link {} --> {} spans {} layers; only links between adjacent layers are supported",
                    from.id,
                    to.id,
                    to.layer - from.layer
                ),
            });
        }
    }

    let last_layer = nodes.iter().map(|node| node.layer).max().unwrap_or(0);
    let mut used: Vec<usize> = vec![0; last_layer + 1];
    for index in 0..nodes.len() {
        let layer = nodes[index].layer;
        nodes[index].slot = used[layer];
        used[layer] += 1;
    }
    Ok((nodes, edges))
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// Growable character grid.
#[derive(Clone, Debug, Default)]
struct Canvas {
    rows: Vec<Vec<char>>,
    width: usize,
}

impl Canvas {
    fn put(&mut self, x: usize, y: usize, character: char) {
        while self.rows.len() <= y {
            self.rows.push(vec![' '; self.width]);
        }
        if self.rows[y].len() <= x {
            let grow_to = x + 1;
            for row in &mut self.rows {
                if row.len() < grow_to {
                    row.resize(grow_to, ' ');
                }
            }
            self.width = self.width.max(grow_to);
        }
        self.rows[y][x] = merge(self.rows[y][x], character);
    }

    fn text(&mut self, x: usize, y: usize, value: &str) {
        let mut column = x;
        for character in value.chars() {
            let extra = (display_width(&character.to_string()).max(1)) - 1;
            self.put(column, y, character);
            column += 1 + extra;
        }
    }
}

/// Merge a new glyph into an occupied cell, keeping junctions readable.
fn merge(existing: char, new: char) -> char {
    if existing == ' ' || existing == new {
        return new;
    }
    match (existing, new) {
        ('─', '│') | ('│', '─') => '┼',
        ('─', '┬') | ('┬', '─') => '┬',
        ('│', '├') | ('├', '│') => '├',
        ('├', '─') | ('─', '├') => '├',
        ('│', '┴') | ('┴', '│') => '┴',
        (_, new) if matches!(new, '▶' | '▼' | '┬') => new,
        (existing, _) => existing,
    }
}

fn draw_node(canvas: &mut Canvas, node: &Node) {
    let width = node.box_width();
    canvas.put(node.x, node.y, '┌');
    for offset in 1..width - 1 {
        canvas.put(node.x + offset, node.y, '─');
    }
    canvas.put(node.x + width - 1, node.y, '┐');

    canvas.put(node.x, node.y + 1, '│');
    canvas.put(node.x + 1, node.y + 1, ' ');
    canvas.text(node.x + 2, node.y + 1, &node.label);
    canvas.put(node.x + width - 2, node.y + 1, ' ');
    canvas.put(node.x + width - 1, node.y + 1, '│');

    canvas.put(node.x, node.y + 2, '└');
    for offset in 1..width - 1 {
        canvas.put(node.x + offset, node.y + 2, '─');
    }
    canvas.put(node.x + width - 1, node.y + 2, '┘');
}

/// Gap between two adjacent `LR` layers, widened for link labels.
fn left_right_gaps(nodes: &[Node], edges: &[Edge], layers: usize) -> Vec<usize> {
    let mut gaps = vec![4usize; layers.saturating_sub(1)];
    for edge in edges {
        let boundary = nodes[edge.from].layer;
        if boundary >= gaps.len() {
            continue;
        }
        let needed = edge
            .label
            .as_deref()
            .map(|label| display_width(label) + 2)
            .unwrap_or(4);
        gaps[boundary] = gaps[boundary].max(needed);
    }
    gaps
}

fn layout_left_right(nodes: &mut [Node], edges: &[Edge]) -> Canvas {
    let layers = nodes.iter().map(|node| node.layer).max().unwrap_or(0) + 1;
    let gaps = left_right_gaps(nodes, edges, layers);
    let mut layer_widths = vec![0usize; layers];
    for node in nodes.iter() {
        layer_widths[node.layer] = layer_widths[node.layer].max(node.box_width());
    }
    let mut layer_x = vec![0usize; layers];
    for layer in 1..layers {
        layer_x[layer] = layer_x[layer - 1] + layer_widths[layer - 1] + gaps[layer - 1];
    }
    for node in nodes.iter_mut() {
        node.x = layer_x[node.layer];
        node.y = node.slot * 4;
    }

    let mut canvas = Canvas::default();
    for node in nodes.iter() {
        draw_node(&mut canvas, node);
    }
    for edge in edges {
        let from = &nodes[edge.from];
        let to = &nodes[edge.to];
        let start_x = from.x + from.box_width() - 1;
        let start_y = from.y + 1;
        let end_x = to.x - 1;
        let end_y = to.y + 1;
        canvas.put(start_x, start_y, '├');
        if let Some(label) = &edge.label {
            let span = end_x.saturating_sub(start_x + 1);
            let label_width = display_width(label);
            let label_x = start_x + 1 + span.saturating_sub(label_width) / 2;
            canvas.text(label_x, start_y.saturating_sub(1), label);
        }
        if start_y == end_y {
            for x in start_x + 1..end_x {
                canvas.put(x, start_y, '─');
            }
        } else {
            let jog_x = start_x + 1;
            for x in start_x + 1..jog_x {
                canvas.put(x, start_y, '─');
            }
            canvas.put(
                jog_x,
                start_y,
                if end_y > start_y { '┐' } else { '┘' },
            );
            for y in (start_y.min(end_y) + 1)..start_y.max(end_y) {
                canvas.put(jog_x, y, '│');
            }
            canvas.put(
                jog_x,
                end_y,
                if end_y > start_y { '└' } else { '┌' },
            );
            for x in jog_x + 1..end_x {
                canvas.put(x, end_y, '─');
            }
        }
        canvas.put(end_x, end_y, if edge.directed { '▶' } else { '─' });
    }
    canvas
}

fn layout_top_down(nodes: &mut [Node], edges: &[Edge]) -> Canvas {
    let layers = nodes.iter().map(|node| node.layer).max().unwrap_or(0) + 1;
    let gap_rows = 3usize;
    let mut layer_heights: Vec<usize> = Vec::with_capacity(layers);
    let mut layer_y = vec![0usize; layers];
    for layer in 0..layers {
        let mut cursor = 0usize;
        for node in nodes.iter_mut().filter(|node| node.layer == layer) {
            node.x = cursor;
            cursor += node.box_width() + 3;
        }
        layer_heights.push(3);
    }
    for layer in 1..layers {
        layer_y[layer] = layer_y[layer - 1] + layer_heights[layer - 1] + gap_rows;
    }
    for node in nodes.iter_mut() {
        node.y = layer_y[node.layer];
    }

    let mut canvas = Canvas::default();
    for node in nodes.iter() {
        draw_node(&mut canvas, node);
    }
    for edge in edges {
        let from = &nodes[edge.from];
        let to = &nodes[edge.to];
        let start_x = from.x + from.box_width() / 2;
        let start_y = from.y + 2;
        let end_x = to.x + to.box_width() / 2;
        let end_y = to.y;
        canvas.put(start_x, start_y, '┬');
        if start_x == end_x {
            for y in start_y + 1..end_y.saturating_sub(1) {
                canvas.put(start_x, y, '│');
            }
            canvas.put(end_x, end_y.saturating_sub(1), if edge.directed { '▼' } else { '│' });
        } else {
            let jog_y = end_y.saturating_sub(2);
            for y in start_y + 1..jog_y {
                canvas.put(start_x, y, '│');
            }
            let (left, right) = if start_x < end_x {
                (start_x, end_x)
            } else {
                (end_x, start_x)
            };
            canvas.put(
                start_x,
                jog_y,
                if start_x < end_x { '└' } else { '┘' },
            );
            for x in left + 1..right {
                canvas.put(x, jog_y, '─');
            }
            canvas.put(
                end_x,
                jog_y,
                if start_x < end_x { '┐' } else { '┌' },
            );
            canvas.put(end_x, end_y.saturating_sub(1), if edge.directed { '▼' } else { '│' });
        }
        if let Some(label) = &edge.label {
            canvas.text(start_x + 2, start_y + 1, label);
        }
    }
    canvas
}
