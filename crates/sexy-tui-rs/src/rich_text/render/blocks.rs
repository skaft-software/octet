//! The block dispatch, and the row builders for prose, lists, quotes, details
//! and tables.
//!
//! This module owns the `match` that decides which layout a [`Block`] gets and
//! every row builder beneath it. It is separate from `render` because dispatch
//! is the part that changes when the document model gains a block kind, and a
//! dispatch table is far easier to review as one list than as one method among
//! the renderer's public entry points. It is separate from [`super::code`] and
//! [`super::wrap`] because it composes them: it decides the geometry and asks
//! them for the rows, so a change to code geometry or to wrapping reaches every
//! block kind from one place.

use super::code::visible_code_language;
use super::lines::{push_blank, RichLine, RichRun};
use super::RichRenderer;

use crate::glyphs::GlyphSet;
use crate::rich_text::{
    Block, CodeBlock, DetailBlock, Inline, List, ListItem, ListKind, Table, TableAlignment,
};
use crate::style::{BlockRole, TextRole, TextStyle};

impl RichRenderer {
    pub(super) fn render_blocks(
        &self,
        blocks: &[Block],
        width: usize,
        compact: bool,
        syntax_highlighting: bool,
    ) -> Vec<RichLine> {
        let mut output = Vec::new();
        for (index, block) in blocks.iter().enumerate() {
            let mut rendered = self.render_block(block, width, syntax_highlighting);
            output.append(&mut rendered);
            if !compact && index + 1 < blocks.len() {
                push_blank(&mut output);
            }
        }
        while output.last().is_some_and(RichLine::is_empty) {
            output.pop();
        }
        if output.is_empty() && !blocks.is_empty() {
            output.push(RichLine::default());
        }
        output
    }

    pub(super) fn prose_width(&self, width: usize) -> usize {
        self.prose_width_indented(width, 0)
    }

    pub(super) fn prose_width_indented(&self, width: usize, indent: usize) -> usize {
        self.options.prose_width.map_or(width, |limit| {
            width.min(usize::from(limit).saturating_sub(indent))
        })
    }

    pub(super) fn render_block(
        &self,
        block: &Block,
        width: usize,
        syntax_highlighting: bool,
    ) -> Vec<RichLine> {
        self.render_block_indented(block, width, syntax_highlighting, 0)
    }

    pub(super) fn render_block_indented(
        &self,
        block: &Block,
        width: usize,
        syntax_highlighting: bool,
        indent: usize,
    ) -> Vec<RichLine> {
        match block {
            Block::Paragraph(content) => {
                let runs = self.inline_runs(content, self.theme.style(TextRole::Text));
                self.wrap_runs(&runs, self.prose_width_indented(width, indent))
            }
            Block::Heading { level, content } => {
                let mut base = self.theme.style(TextRole::Heading);
                if *level <= 3 {
                    base.attributes.bold = true;
                }
                let runs = self.inline_runs(content, base);
                self.wrap_runs(&runs, self.prose_width_indented(width, indent))
            }
            Block::CodeBlock(code)
                if code.language.as_deref().is_some_and(|lang| {
                    lang.eq_ignore_ascii_case("diff") || lang.eq_ignore_ascii_case("patch")
                }) =>
            {
                // Fenced diff blocks carry the same semantics as bare unified
                // diffs. Render them through the dedicated diff pipeline so
                // they get full-width backgrounds, color, and line numbers.
                self.render_diff_as_rich_lines(&code.code, width)
            }
            Block::CodeBlock(code) => self.render_code(code, width, syntax_highlighting),
            Block::Diagram { source, rendered } => {
                // Pi keeps the original fence when the art does not fit. Count
                // terminal cells, not bytes, and include our code-block chrome.
                let natural_width = rendered
                    .lines()
                    .map(|line| self.options.width.line_width(&self.sanitize(line)))
                    .max()
                    .unwrap_or(0);
                let language_width = visible_code_language(source)
                    .map(|label| self.options.width.line_width(&self.sanitize(label)))
                    .unwrap_or(0);
                if self
                    .code_layout(language_width, width, natural_width)
                    .content_width
                    < natural_width
                {
                    self.render_code(source, width, syntax_highlighting)
                } else {
                    self.render_code(
                        &CodeBlock {
                            language: source.language.clone(),
                            code: rendered.clone(),
                        },
                        width,
                        false,
                    )
                }
            }
            Block::List(list) => {
                self.render_list_indented(list, width, syntax_highlighting, indent)
            }
            Block::BlockQuote(blocks) => {
                self.render_quote_indented(blocks, width, syntax_highlighting, indent)
            }
            Block::Divider => {
                if width == 0 {
                    vec![RichLine::default()]
                } else {
                    let glyphs = GlyphSet::for_capabilities(self.capabilities);
                    let count =
                        width.min(60) / self.options.width.line_width(glyphs.horizontal).max(1);
                    let mut border = self.theme.style(TextRole::Border);
                    if let Some(color) = self.theme.resolve_color("md_hr") {
                        border.foreground = color;
                    }
                    let mut line = RichLine::default();
                    line.push(glyphs.horizontal.repeat(count), border, None);
                    vec![line]
                }
            }
            Block::Table(table) if self.options.tables => self.render_table(table, width),
            Block::Table(table) => self.render_table_fallback(table, width),
            Block::Detail(detail) => self.render_detail(detail, width, syntax_highlighting),
            Block::Plain(text) => {
                let safe = self.sanitize(text);
                safe.split('\n')
                    .flat_map(|line| {
                        let run =
                            RichRun::new(line.to_owned(), self.theme.style(TextRole::Text), None);
                        self.wrap_runs(&[run], self.prose_width_indented(width, indent))
                    })
                    .collect()
            }
        }
    }

    pub(super) fn render_list_indented(
        &self,
        list: &List,
        width: usize,
        syntax_highlighting: bool,
        indent: usize,
    ) -> Vec<RichLine> {
        self.render_list_with_commit_ends_indented(list, width, syntax_highlighting, indent)
            .0
    }

    pub(super) fn render_list_with_commit_ends(
        &self,
        list: &List,
        width: usize,
        syntax_highlighting: bool,
    ) -> (Vec<RichLine>, Vec<usize>) {
        self.render_list_with_commit_ends_indented(list, width, syntax_highlighting, 0)
    }

    pub(super) fn render_list_with_commit_ends_indented(
        &self,
        list: &List,
        width: usize,
        syntax_highlighting: bool,
        indent: usize,
    ) -> (Vec<RichLine>, Vec<usize>) {
        let mut output = Vec::new();
        let mut ends = Vec::with_capacity(list.items.len());
        for (index, item) in list.items.iter().enumerate() {
            if index > 0 && (item.blocks.len() > 1 || list.items[index - 1].blocks.len() > 1) {
                push_blank(&mut output);
            }
            let marker = match list.kind {
                ListKind::Unordered => format!(
                    "{} ",
                    self.options.unordered_list_marker.glyph(self.capabilities)
                ),
                ListKind::Ordered { start } => format!("{}. ", start.saturating_add(index as u64)),
            };
            let marker = match item.task {
                Some(true) => format!("{marker}[x] "),
                Some(false) => format!("{marker}[ ] "),
                None => marker,
            };
            self.render_list_item(
                item,
                &marker,
                width,
                syntax_highlighting,
                indent,
                &mut output,
            );
            ends.push(output.len());
        }
        (output, ends)
    }

    pub(super) fn render_list_item(
        &self,
        item: &ListItem,
        marker: &str,
        width: usize,
        syntax_highlighting: bool,
        indent: usize,
        output: &mut Vec<RichLine>,
    ) {
        let marker_style = self.theme.style(TextRole::ListMarker);
        let marker_width = self.options.width.line_width(marker);
        let mut blocks = item.blocks.as_slice();
        if let Some(Block::Paragraph(content)) = blocks.first() {
            let runs = self.inline_runs(content, self.theme.style(TextRole::Text));
            let rows = self.wrap_runs(
                &runs,
                self.prose_width_indented(
                    width.saturating_sub(marker_width),
                    indent.saturating_add(marker_width),
                ),
            );
            if rows.is_empty() {
                let mut line = RichLine::default();
                line.push(marker.to_owned(), marker_style, None);
                output.push(line);
            } else {
                for (index, row) in rows.into_iter().enumerate() {
                    let mut line = RichLine::default();
                    if index == 0 {
                        line.push(marker.to_owned(), marker_style, None);
                    } else {
                        line.push(" ".repeat(marker_width), TextStyle::plain(), None);
                    }
                    line.extend(row);
                    output.push(line);
                }
            }
            blocks = &blocks[1..];
        } else {
            let mut line = RichLine::default();
            line.push(marker.to_owned(), marker_style, None);
            output.push(line);
        }

        for block in blocks {
            let nested_width = width.saturating_sub(marker_width);
            for row in self.render_block_indented(
                block,
                nested_width,
                syntax_highlighting,
                indent.saturating_add(marker_width),
            ) {
                let mut line = RichLine::from_plain(" ".repeat(marker_width));
                line.extend(row);
                output.push(line);
            }
        }
    }

    pub(super) fn render_quote_indented(
        &self,
        blocks: &[Block],
        width: usize,
        syntax_highlighting: bool,
        indent: usize,
    ) -> Vec<RichLine> {
        let glyphs = GlyphSet::for_capabilities(self.capabilities);
        let prefix = format!("{} ", glyphs.vertical);
        let prefix_width = self.options.width.line_width(&prefix);
        let inner_width = width.saturating_sub(prefix_width);
        let mut inner = Vec::new();
        for (index, block) in blocks.iter().enumerate() {
            // Indentation participates in the prose lane, while technical
            // content inside quotes retains the full remaining viewport.
            let mut rows = self.render_block_indented(
                block,
                inner_width,
                syntax_highlighting,
                indent.saturating_add(prefix_width),
            );
            inner.append(&mut rows);
            if index + 1 < blocks.len() {
                push_blank(&mut inner);
            }
        }
        while inner.last().is_some_and(RichLine::is_empty) {
            inner.pop();
        }
        let block_style = self.theme.block_style(BlockRole::Quote);
        let mut output = Vec::new();
        for mut row in inner {
            for run in &mut row.runs {
                run.style = block_style.text.merge(run.style);
                if let Some(background) = block_style.background {
                    run.style.background = background;
                }
            }
            let mut line = RichLine::default();
            line.push(prefix.clone(), block_style.border, None);
            line.extend(row);
            output.push(line);
        }
        if output.is_empty() {
            let mut line = RichLine::default();
            line.push(prefix, block_style.border, None);
            output.push(line);
        }
        output
    }

    pub(super) fn render_detail(
        &self,
        detail: &DetailBlock,
        width: usize,
        syntax_highlighting: bool,
    ) -> Vec<RichLine> {
        let glyphs = GlyphSet::for_capabilities(self.capabilities);
        let marker = format!(
            "{} ",
            if detail.expanded {
                glyphs.detail_expanded
            } else {
                glyphs.detail_collapsed
            }
        );
        let marker_width = self.options.width.line_width(&marker);
        let block_style = self.theme.block_style(BlockRole::Detail);
        let runs = self.inline_runs(
            &detail.summary,
            block_style.text.merge(self.theme.style(TextRole::Strong)),
        );
        let mut output = Vec::new();
        for (index, row) in self
            .wrap_runs(&runs, width.saturating_sub(marker_width))
            .into_iter()
            .enumerate()
        {
            let mut line = RichLine::default();
            if index == 0 {
                line.push(marker.clone(), block_style.border, None);
            } else {
                line.push(" ".repeat(marker_width), TextStyle::plain(), None);
            }
            line.extend(row);
            output.push(line);
        }
        if detail.expanded {
            for row in self.render_blocks(
                &detail.blocks,
                width.saturating_sub(marker_width),
                false,
                syntax_highlighting,
            ) {
                let mut line = RichLine::from_plain(" ".repeat(marker_width));
                line.extend(row);
                output.push(line);
            }
        }
        output
    }

    pub(super) fn render_table(&self, table: &Table, width: usize) -> Vec<RichLine> {
        self.render_table_with_commit_ends(table, width).0
    }

    pub(super) fn render_table_with_commit_ends(
        &self,
        table: &Table,
        width: usize,
    ) -> (Vec<RichLine>, Vec<usize>) {
        let columns = table
            .header
            .len()
            .max(table.rows.iter().map(Vec::len).max().unwrap_or(0));
        if columns == 0 {
            return (Vec::new(), Vec::new());
        }
        let overhead = columns.saturating_mul(3).saturating_add(1);
        // One-cell columns are technically drawable but not readable. Prefer
        // the labeled-list fallback until each column can hold a short token.
        if width < overhead.saturating_add(columns.saturating_mul(3)) {
            return self.render_table_fallback_with_commit_ends(table, width);
        }

        let mut natural = vec![width; columns];
        if !self.options.stable_block_geometry {
            natural.fill(1);
            for row in std::iter::once(&table.header).chain(table.rows.iter()) {
                for (column, cell) in row.iter().enumerate() {
                    let runs = self.inline_runs(cell, self.theme.style(TextRole::Text));
                    natural[column] = natural[column].max(self.runs_width(&runs).min(40));
                }
            }
        }
        let interior = width.saturating_sub(overhead);
        let mut widths = vec![1usize; columns];
        let mut remaining = interior.saturating_sub(columns);
        while remaining > 0 {
            let mut progressed = false;
            for column in 0..columns {
                if remaining == 0 {
                    break;
                }
                if widths[column] < natural[column] {
                    widths[column] += 1;
                    remaining -= 1;
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }

        let unicode = self.capabilities.unicode && !self.capabilities.plain;
        let (
            vertical,
            left,
            middle,
            right,
            top_left,
            top_mid,
            top_right,
            bottom_left,
            bottom_mid,
            bottom_right,
        ) = if unicode {
            ("│", "├", "┼", "┤", "┌", "┬", "┐", "└", "┴", "┘")
        } else {
            ("|", "+", "+", "+", "+", "+", "+", "+", "+", "+")
        };
        let horizontal = if unicode { "─" } else { "-" };
        let border_style = self.theme.block_style(BlockRole::Table).border;
        let border = |left: &str, middle: &str, right: &str| {
            let text = format!(
                "{}{}{}",
                left,
                widths
                    .iter()
                    .map(|width| horizontal.repeat(width + 2))
                    .collect::<Vec<_>>()
                    .join(middle),
                right
            );
            let mut line = RichLine::default();
            line.push(text, border_style, None);
            line
        };

        let mut output = vec![border(top_left, top_mid, top_right)];
        output.extend(self.render_table_row(
            &table.header,
            &widths,
            &table.alignments,
            vertical,
            true,
        ));
        output.push(border(left, middle, right));
        let mut ends = Vec::with_capacity(table.rows.len().max(1));
        for (index, row) in table.rows.iter().enumerate() {
            output.extend(self.render_table_row(row, &widths, &table.alignments, vertical, false));
            if index + 1 < table.rows.len() {
                output.push(border(left, middle, right));
            }
            ends.push(output.len());
        }
        output.push(border(bottom_left, bottom_mid, bottom_right));
        if let Some(end) = ends.last_mut() {
            *end = output.len();
        }
        (
            output
                .into_iter()
                .map(|line| self.clip_line(line, width))
                .collect(),
            ends,
        )
    }

    pub(super) fn render_table_row(
        &self,
        row: &[Vec<Inline>],
        widths: &[usize],
        alignments: &[TableAlignment],
        vertical: &str,
        header: bool,
    ) -> Vec<RichLine> {
        let mut cells = Vec::new();
        let mut height = 1usize;
        for (column, width) in widths.iter().copied().enumerate() {
            let table_style = self.theme.block_style(BlockRole::Table);
            let base = if header {
                table_style.text.merge(self.theme.style(TextRole::Strong))
            } else {
                table_style.text
            };
            let runs = row
                .get(column)
                .map_or_else(Vec::new, |cell| self.inline_runs(cell, base));
            let wrapped = self.wrap_runs(&runs, width);
            height = height.max(wrapped.len());
            cells.push(wrapped);
        }

        let mut output = Vec::new();
        for line_index in 0..height {
            let mut line = RichLine::default();
            line.push(
                vertical.to_owned(),
                self.theme.block_style(BlockRole::Table).border,
                None,
            );
            for (column, width) in widths.iter().copied().enumerate() {
                line.push(" ".into(), TextStyle::plain(), None);
                let cell = cells[column].get(line_index).cloned().unwrap_or_default();
                let cell_width = self.line_width(&cell);
                let padding = width.saturating_sub(cell_width);
                let alignment = alignments.get(column).copied().unwrap_or_default();
                let left = match alignment {
                    TableAlignment::Right => padding,
                    TableAlignment::Center => padding / 2,
                    TableAlignment::None | TableAlignment::Left => 0,
                };
                line.push(" ".repeat(left), TextStyle::plain(), None);
                line.extend(cell);
                line.push(" ".repeat(padding - left), TextStyle::plain(), None);
                line.push(" ".into(), TextStyle::plain(), None);
                line.push(
                    vertical.to_owned(),
                    self.theme.block_style(BlockRole::Table).border,
                    None,
                );
            }
            output.push(line);
        }
        output
    }

    pub(super) fn render_table_fallback(&self, table: &Table, width: usize) -> Vec<RichLine> {
        self.render_table_fallback_with_commit_ends(table, width).0
    }

    pub(super) fn render_table_fallback_with_commit_ends(
        &self,
        table: &Table,
        width: usize,
    ) -> (Vec<RichLine>, Vec<usize>) {
        let mut output = Vec::new();
        let mut ends = Vec::with_capacity(table.rows.len().max(1));
        for (row_index, row) in table.rows.iter().enumerate() {
            if row_index > 0 {
                push_blank(&mut output);
            }
            for (column, value) in row.iter().enumerate() {
                let label = table
                    .header
                    .get(column)
                    .cloned()
                    .unwrap_or_else(|| vec![Inline::Text(format!("Column {}", column + 1))]);
                let table_style = self.theme.block_style(BlockRole::Table);
                let mut runs = self.inline_runs(
                    &label,
                    table_style.text.merge(self.theme.style(TextRole::Strong)),
                );
                runs.push(RichRun::new(
                    ": ".into(),
                    self.theme.style(TextRole::Text),
                    None,
                ));
                runs.extend(self.inline_runs(value, table_style.text));
                output.extend(self.wrap_runs(&runs, width));
            }
            ends.push(output.len());
        }
        if table.rows.is_empty() {
            for cell in &table.header {
                output.extend(
                    self.wrap_runs(
                        &self.inline_runs(
                            cell,
                            self.theme
                                .block_style(BlockRole::Table)
                                .text
                                .merge(self.theme.style(TextRole::Strong)),
                        ),
                        width,
                    ),
                );
            }
            ends.push(output.len());
        }
        (output, ends)
    }
}
