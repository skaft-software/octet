//! Markdown source provenance, prepared with layout and shared by paint receipts.
//! Native terminal selection owns these coordinates, not the process adapter.
//! Pi compatibility reference: packages/tui/src/tui-alt-screen.ts:1453-1474
//! copies painted plain text. The source-copy contract intentionally differs;
//! only the native renderer has the retained source and painted cell authority.

#[cfg(test)]
thread_local! {
    static SOURCE_BYTES_MAPPED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn take_source_mapping_work() -> usize {
    SOURCE_BYTES_MAPPED.with(|work| work.replace(0))
}
use std::ops::Range;

use pulldown_cmark::{Event, Parser, Tag};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Block,
    Item,
    Prefix,
    Inline,
    Row,
    Table,
    Cell,
    Text,
}

#[derive(Clone, Debug)]
struct Node {
    kind: Kind,
    plain: Range<usize>,
    source: Range<usize>,
    children: Vec<Node>,
    /// Only transformed text (entities, escapes, code-span normalization) needs
    /// scalar boundaries. Ordinary text uses a constant-size affine span.
    units: Vec<(Range<usize>, Range<usize>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Piece {
    Source(Range<usize>),
    Separator(&'static str),
}

#[derive(Clone, Debug)]
pub(super) struct SourceMap {
    nodes: Vec<Node>,
    pub(super) plain_len: usize,
    source_len: usize,
    source: std::sync::Arc<str>,
    pub(super) table_rows: Vec<Vec<Range<usize>>>,
    pub(super) tables: Vec<Range<usize>>,
}

struct Frame {
    kind: Kind,
    source: Range<usize>,
    children: Vec<Node>,
    link: Option<String>,
}

impl SourceMap {
    /// The canonical semantic text is the layout's coordinate space. Parser
    /// offset events supply original-source ranges, never reconstructed syntax.
    pub(super) fn build(source: &str, plain: &str) -> Self {
        #[cfg(test)]
        SOURCE_BYTES_MAPPED.with(|work| work.set(work.get() + source.len()));
        let mut stack = vec![Frame {
            kind: Kind::Block,
            source: 0..source.len(),
            children: Vec::new(),
            link: None,
        }];
        let mut cursor = 0;
        for (event, range) in
            Parser::new_ext(source, sexy_tui_rs::rich_text::markdown::parser_options())
                .into_offset_iter()
        {
            match event {
                Event::Start(tag) => {
                    let kind = match tag {
                        Tag::Emphasis
                        | Tag::Strong
                        | Tag::Strikethrough
                        | Tag::Link { .. }
                        | Tag::Image { .. } => Kind::Inline,
                        Tag::Table(_) => Kind::Table,
                        Tag::TableHead | Tag::TableRow => Kind::Row,
                        Tag::TableCell => Kind::Cell,
                        Tag::Item => Kind::Item,
                        _ => Kind::Block,
                    };
                    let link = match tag {
                        Tag::Link { dest_url, .. } => Some(dest_url.into_string()),
                        _ => None,
                    };
                    stack.push(Frame {
                        kind,
                        source: range,
                        children: Vec::new(),
                        link,
                    });
                }
                Event::End(_) => {
                    let mut frame = stack.pop().expect("balanced Markdown tags");
                    let mut start = frame
                        .children
                        .first()
                        .map_or(cursor, |node| node.plain.start);
                    let mut end = frame.children.last().map_or(start, |node| node.plain.end);
                    if frame.kind == Kind::Cell {
                        end = cursor;
                    }
                    if matches!(frame.kind, Kind::Block | Kind::Item) {
                        end = start + plain[start..end].trim_end_matches(['\n', '\r']).len();
                    }
                    if frame.kind == Kind::Item && !frame.children.is_empty() {
                        let prefix_start = plain[..start].rfind('\n').map_or(0, |at| at + 1);
                        let raw = frame.source.start..frame.children[0].source.start;
                        if prefix_start < start && !raw.is_empty() {
                            let prefix = prefix_start..start;
                            // Lists normalize marker spelling and indentation.
                            // Keep the original marker's provenance at layout,
                            // including for a partially selected wrapped item.
                            let units = if raw.len() == prefix.len() {
                                Vec::new()
                            } else {
                                vec![(prefix.clone(), raw.clone())]
                            };
                            frame.children.insert(
                                0,
                                Node {
                                    kind: Kind::Prefix,
                                    plain: prefix,
                                    source: raw,
                                    children: Vec::new(),
                                    units,
                                },
                            );
                            start = prefix_start;
                        }
                    }
                    if let Some(target) = frame.link {
                        let suffix = format!(" ({target})");
                        if plain[cursor..].starts_with(&suffix) {
                            cursor += suffix.len();
                        }
                    }
                    let mut source_range = trim_line_end(source, frame.source);
                    if frame.kind == Kind::Cell {
                        let raw = &source[source_range.clone()];
                        source_range.start += raw.len() - raw.trim_start().len();
                        source_range.end = source_range.start + raw.trim().len();
                    }
                    stack.last_mut().unwrap().children.push(Node {
                        kind: frame.kind,
                        plain: start..end,
                        source: source_range,
                        children: frame.children,
                        units: Vec::new(),
                    });
                }
                Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                    append_text(
                        &mut stack.last_mut().unwrap().children,
                        source,
                        plain,
                        &mut cursor,
                        &text,
                        range,
                        false,
                    );
                }
                Event::Code(text) => {
                    let mut children = Vec::new();
                    append_text(
                        &mut children,
                        source,
                        plain,
                        &mut cursor,
                        &text,
                        range.clone(),
                        true,
                    );
                    if let Some(child) = children.first() {
                        let span = child.plain.start..children.last().unwrap().plain.end;
                        stack.last_mut().unwrap().children.push(Node {
                            kind: Kind::Inline,
                            plain: span,
                            source: range,
                            children,
                            units: Vec::new(),
                        });
                    }
                }
                Event::SoftBreak | Event::HardBreak => {
                    let text = if matches!(event, Event::SoftBreak) {
                        " "
                    } else {
                        "\n"
                    };
                    append_text(
                        &mut stack.last_mut().unwrap().children,
                        source,
                        plain,
                        &mut cursor,
                        text,
                        range,
                        false,
                    );
                }
                Event::Rule => append_text(
                    &mut stack.last_mut().unwrap().children,
                    source,
                    plain,
                    &mut cursor,
                    "---",
                    range,
                    false,
                ),
                _ => {}
            }
        }
        let nodes = stack.pop().unwrap().children;
        let mut table_rows = Vec::new();
        let mut tables = Vec::new();
        fn rows(
            nodes: &[Node],
            output: &mut Vec<Vec<Range<usize>>>,
            tables: &mut Vec<Range<usize>>,
        ) {
            for node in nodes {
                if node.kind == Kind::Row {
                    output.push(
                        node.children
                            .iter()
                            .map(|cell| cell.plain.clone())
                            .collect(),
                    );
                } else {
                    let start = output.len();
                    rows(&node.children, output, tables);
                    if node.kind == Kind::Table {
                        tables.push(start..output.len());
                    }
                }
            }
        }
        rows(&nodes, &mut table_rows, &mut tables);
        Self {
            nodes,
            plain_len: plain.len(),
            source_len: source.len(),
            source: std::sync::Arc::from(source),
            table_rows,
            tables,
        }
    }

    pub(super) fn pieces(&self, from: usize, to: usize) -> Vec<Piece> {
        let source = &self.source;
        let mut output = Vec::new();
        if from == 0 && to >= self.plain_len {
            output.push(Piece::Source(0..self.source_len));
        } else {
            collect_children(&self.nodes, source, from, to, None, &mut output);
        }
        output
    }

    pub(super) fn copy(&self, from: usize, to: usize) -> Option<String> {
        let source = &self.source;
        let mut output = String::new();
        for piece in self.pieces(from, to) {
            match piece {
                Piece::Source(range) => output.push_str(source.get(range)?),
                Piece::Separator(text) => output.push_str(text),
            }
        }
        Some(output)
    }

    pub(super) fn same_selection(&self, other: &Self, from: usize, to: usize) -> bool {
        let a = self.pieces(from, to);
        let b = other.pieces(from, to);
        a == b
            && a.iter().all(|piece| match piece {
                Piece::Source(range) => {
                    self.source.get(range.clone()) == other.source.get(range.clone())
                }
                Piece::Separator(_) => true,
            })
    }
}

fn trim_line_end(source: &str, mut range: Range<usize>) -> Range<usize> {
    range.end = range.start + source[range.clone()].trim_end_matches(['\n', '\r']).len();
    range
}

fn append_text(
    nodes: &mut Vec<Node>,
    source: &str,
    plain: &str,
    cursor: &mut usize,
    text: &str,
    range: Range<usize>,
    code: bool,
) {
    let Some(relative) = plain[*cursor..].find(text) else {
        return;
    };
    let start = *cursor + relative;
    *cursor = start + text.len();
    let mut raw = range.clone();
    if !code
        && source[..raw.start]
            .bytes()
            .rev()
            .take_while(|c| *c == b'\\')
            .count()
            % 2
            == 1
    {
        raw.start -= 1;
    }
    if code {
        let marker = source[raw.clone()]
            .bytes()
            .take_while(|c| *c == b'`')
            .count();
        raw.start += marker;
        raw.end = raw.end.saturating_sub(marker).max(raw.start);
        let content = &source[raw.clone()];
        if content.starts_with(' ') && content.ends_with(' ') && !content.trim().is_empty() {
            raw.start += 1;
            raw.end -= 1;
        }
    }
    let mut units = Vec::new();
    if source.get(raw.clone()) != Some(text) {
        let normalized = source[raw.clone()].replace(['\n', '\r'], " ");
        if code && normalized == text {
            for ((offset, c), (original, raw_char)) in
                text.char_indices().zip(source[raw.clone()].char_indices())
            {
                units.push((
                    start + offset..start + offset + c.len_utf8(),
                    raw.start + original..raw.start + original + raw_char.len_utf8(),
                ));
            }
        } else if text.chars().count() == 1 {
            units.push((start..*cursor, raw.clone()));
        } else {
            // Escapes may be combined into one Text event. Match original
            // scalars monotonically, retaining each escape's backslash.
            let mut at = raw.start;
            for (offset, c) in text.char_indices() {
                let Some(found) = source[at..raw.end].find(c) else {
                    continue;
                };
                let position = at + found;
                let first = if source[at..position].ends_with('\\') {
                    position - 1
                } else {
                    position
                };
                at = position + c.len_utf8();
                units.push((start + offset..start + offset + c.len_utf8(), first..at));
            }
        }
    }
    nodes.push(Node {
        kind: Kind::Text,
        plain: start..*cursor,
        source: raw,
        children: Vec::new(),
        units,
    });
}

fn collect(node: &Node, source: &str, from: usize, to: usize, output: &mut Vec<Piece>) {
    if node.plain.is_empty() || from >= node.plain.end || to <= node.plain.start {
        return;
    }
    let content_start = if node.kind == Kind::Item {
        node.children
            .iter()
            .find(|child| child.kind != Kind::Prefix)
            .map_or(node.plain.start, |child| child.plain.start)
    } else {
        node.plain.start
    };
    if from <= content_start && to >= node.plain.end {
        output.push(Piece::Source(node.source.clone()));
    } else if matches!(node.kind, Kind::Text | Kind::Prefix) {
        if node.units.is_empty() {
            output.push(Piece::Source(
                node.source.start + from.saturating_sub(node.plain.start)
                    ..node.source.start + to.min(node.plain.end) - node.plain.start,
            ));
        } else {
            for (plain, raw) in node
                .units
                .iter()
                .skip(node.units.partition_point(|(span, _)| span.end <= from))
            {
                if plain.start >= to {
                    break;
                }
                output.push(Piece::Source(raw.clone()));
            }
        }
    } else {
        let separator = match node.kind {
            Kind::Row => Some("\t"),
            Kind::Table => Some("\n"),
            _ => None,
        };
        collect_children(&node.children, source, from, to, separator, output);
    }
}

fn collect_children(
    nodes: &[Node],
    source: &str,
    from: usize,
    to: usize,
    separator: Option<&'static str>,
    output: &mut Vec<Piece>,
) {
    let mut previous = None;
    for node in nodes
        .iter()
        .skip(nodes.partition_point(|node| node.plain.end <= from))
    {
        if node.plain.start >= to {
            break;
        }
        let mut pieces = Vec::new();
        collect(node, source, from, to, &mut pieces);
        // A covered empty table cell still contributes its field separator.
        if pieces.is_empty() && !(node.kind == Kind::Cell && node.plain.is_empty()) {
            continue;
        }
        if let Some(end) = previous {
            if let Some(separator) = separator {
                output.push(Piece::Separator(separator));
            } else {
                // Sibling constructs own their delimiters. Keep only the
                // whitespace *between* their outer source ranges, not the
                // opening/closing syntax omitted by partial child selections.
                let gap = &source[end..node.source.start];
                if !gap.is_empty() && gap.chars().all(char::is_whitespace) {
                    output.push(Piece::Source(end..node.source.start));
                }
            }
        }
        previous = Some(node.source.end);
        output.extend(pieces);
    }
}

impl super::ShellState {
    pub(super) fn markdown_source_map(&self, index: usize) -> Option<std::sync::Arc<SourceMap>> {
        let id = self.transcript_commit_ids.get(index)?;
        if let Some(map) = self.markdown_selection_maps.borrow().get(id) {
            return Some(map.clone());
        }
        if self.render_threaded {
            return self
                .gesture_render_geometry()?
                .blocks
                .iter()
                .find(|block| block.id == *id)?
                .source_map
                .clone();
        }
        match &self.transcript[index] {
            super::TranscriptBlock::Assistant(block) => block.source_map.borrow().clone(),
            _ => None,
        }
    }

    pub(super) fn retain_markdown_source_map(&self, index: usize) {
        let map = if self.render_threaded {
            self.gesture_render_geometry()
                .and_then(|geometry| geometry.blocks.iter().find(|block| block.index == index))
                .and_then(|block| block.source_map.clone())
        } else {
            match &self.transcript[index] {
                super::TranscriptBlock::Assistant(block) => block.source_map.borrow().clone(),
                _ => None,
            }
        };
        let Some(map) = map else {
            return;
        };
        let mut ids = vec![self.transcript_commit_ids[index]];
        if let Some(selection) = &self.transcript_selection {
            ids.extend([
                self.transcript_commit_ids[selection.anchor.block],
                self.transcript_commit_ids[selection.focus.block],
            ]);
        }
        if let Some(anchor) = self.pending_selection_anchor {
            ids.push(self.transcript_commit_ids[anchor.block]);
        }
        let mut maps = self.markdown_selection_maps.borrow_mut();
        maps.retain(|id, _| ids.contains(id));
        maps.insert(self.transcript_commit_ids[index], map);
    }

    pub(super) fn accept_markdown_source_maps(&mut self) {
        let Some(geometry) = self.pointer_render_geometry() else {
            return;
        };
        let mut updates = Vec::new();
        let mut invalid = false;
        for block in &geometry.blocks {
            let Some(new) = &block.source_map else {
                continue;
            };
            let maps = self.markdown_selection_maps.borrow();
            let Some(old) = maps.get(&block.id) else {
                continue;
            };
            if std::sync::Arc::ptr_eq(old, new) {
                continue;
            }
            if let Some(selection) = &self.transcript_selection {
                let (start, end) = if (selection.anchor.block, selection.anchor.offset)
                    <= (selection.focus.block, selection.focus.offset)
                {
                    (selection.anchor, selection.focus)
                } else {
                    (selection.focus, selection.anchor)
                };
                if block.index >= start.block && block.index <= end.block {
                    let from = if block.index == start.block {
                        start.offset
                    } else {
                        0
                    };
                    let to = if block.index == end.block {
                        end.offset
                    } else {
                        old.plain_len
                    };
                    invalid |= !old.same_selection(new, from, to);
                }
            }
            updates.push((block.id, new.clone()));
        }
        self.markdown_selection_maps.borrow_mut().extend(updates);
        if invalid {
            self.transcript_selection = None;
            self.cancel_transcript_pointer();
            self.copy_notice = Some((
                "Markdown selection cleared: source mapping changed".into(),
                std::time::Instant::now() + std::time::Duration::from_millis(1500),
            ));
        }
    }
}
