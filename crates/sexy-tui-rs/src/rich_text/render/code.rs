//! Code-block geometry: borders, gutters, headers and the clip-or-wrap policy.
//!
//! Everything a fenced or indented code block needs to know about its own
//! shape lives here — the measured [`CodeLayout`], the border and header rows
//! drawn around it, and the two long-line policies. It is separate from
//! [`super::blocks`] because code is the one block kind whose layout is
//! measured rather than flowed, and from [`super::wrap`] because a code row is
//! wrapped literally: indentation and separator whitespace are never discarded
//! to find a word boundary.
//!
//! [`CodeLayout`] is `pub(super)` rather than private because
//! [`super::append_tail`] measures the same geometry while a stream grows, and
//! the two must not be able to disagree about how wide a code block is.

use unicode_segmentation::UnicodeSegmentation;

use super::{CodeOverflow, RichLine, RichRenderer, RichRun};

use crate::glyphs::GlyphSet;
use crate::rich_text::CodeBlock;
use crate::style::{BlockRole, Color, TextRole, TextStyle};
use crate::width::WidthPolicy;

impl RichRenderer {
    pub(super) fn render_code(
        &self,
        code: &CodeBlock,
        width: usize,
        syntax_highlighting: bool,
    ) -> Vec<RichLine> {
        // CommonMark includes the line ending immediately before a closing
        // fence in the code payload. It terminates the final source row; it is
        // not an additional blank row. Removing exactly one line ending keeps
        // intentional blank lines intact while avoiding a phantom row.
        let display_source = code
            .code
            .strip_suffix('\n')
            .map(|source| source.strip_suffix('\r').unwrap_or(source))
            .unwrap_or(&code.code);
        let source_lines = if display_source.is_empty() {
            vec![""]
        } else {
            display_source.split('\n').collect::<Vec<_>>()
        };
        // Generic prose-fence tags add no information and look like a stray
        // badge. Preserve meaningful language labels and the original code
        // metadata used for highlighting/copy behavior.
        let language = visible_code_language(code);

        // A code surface should fit its content rather than painting the full
        // terminal width for a short snippet. Long rows still use all available
        // space and follow the configured clip/wrap policy.
        let natural_content_width = source_lines
            .iter()
            .map(|line| self.options.width.line_width(&self.sanitize(line)))
            .max()
            .unwrap_or(0);
        let language_width = language
            .map(|label| self.options.width.line_width(&self.sanitize(label)))
            .unwrap_or(0);
        let layout = self.code_layout(language_width, width, natural_content_width);
        let code_text_style = layout.code_text_style;
        let mut output = self.code_header(&layout, language);

        let highlighted = syntax_highlighting
            .then(|| self.highlighted(code))
            .flatten();
        for (line_index, source) in source_lines.iter().enumerate() {
            let mut runs = if let Some(lines) = highlighted.as_deref() {
                lines
                    .get(line_index)
                    .map(|regions| {
                        regions
                            .iter()
                            .map(|region| {
                                RichRun::new(
                                    self.sanitize(&region.text),
                                    region.role.map_or(code_text_style, |role| {
                                        code_text_style.merge(self.theme.style(role))
                                    }),
                                    None,
                                )
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            } else {
                vec![RichRun::new(self.sanitize(source), code_text_style, None)]
            };
            if runs.is_empty() {
                runs.push(RichRun::new(String::new(), code_text_style, None));
            }
            let rows = match self.options.code_overflow {
                CodeOverflow::Clip => vec![self.clip_runs(&runs, layout.content_width)],
                CodeOverflow::Wrap => self.hard_wrap_runs(&runs, layout.content_width),
            };
            for row in rows {
                output.push(layout.row(self, row));
            }
        }

        output.extend(self.code_footer(&layout));
        output
    }

    pub(super) fn code_layout(
        &self,
        language_width: usize,
        width: usize,
        natural_content_width: usize,
    ) -> CodeLayout {
        let style = self.theme.block_style(BlockRole::Code);
        let mut code_text_style = style.text;
        let mut padding_style = TextStyle::plain();
        if let Some(background) = style.background {
            code_text_style.background = background;
            padding_style.background = background;
        }
        let bordered = self.options.code_borders && width >= 3;
        let frame_width = usize::from(bordered) * 2;
        let configured_left = usize::from(style.padding_left);
        let configured_right = usize::from(style.padding_right);
        let required_for_content = natural_content_width
            .saturating_add(configured_left)
            .saturating_add(configured_right)
            .saturating_add(frame_width);
        let required_for_label = if bordered {
            language_width.saturating_add(5)
        } else {
            language_width
                .saturating_add(configured_left)
                .saturating_add(configured_right)
        };
        let minimum_width = if bordered { 3 } else { 1 };
        let block_width = if self.options.stable_block_geometry {
            width
        } else {
            required_for_content
                .max(required_for_label)
                .max(minimum_width)
                .min(width)
        };
        let inner_width = block_width.saturating_sub(frame_width);
        let left_padding = configured_left.min(inner_width.saturating_sub(1));
        let right_padding =
            configured_right.min(inner_width.saturating_sub(left_padding).saturating_sub(1));
        let content_width = inner_width
            .saturating_sub(left_padding)
            .saturating_sub(right_padding);

        CodeLayout {
            block_width,
            content_width,
            left_padding,
            right_padding,
            bordered,
            padding_style,
            code_text_style,
            border_style: style.border,
            background: style.background.is_some(),
            background_color: style.background,
            padding_top: style.padding_top,
            padding_bottom: style.padding_bottom,
        }
    }

    pub(super) fn code_header(&self, layout: &CodeLayout, language: Option<&str>) -> Vec<RichLine> {
        let CodeLayout {
            block_width,
            content_width,
            left_padding,
            right_padding,
            bordered,
            padding_style,
            border_style,
            background,
            background_color,
            padding_top,
            ..
        } = *layout;
        let mut output = Vec::new();
        if bordered {
            output.push(self.code_border_line(block_width, language, true, border_style));
        } else if let Some(language) = language {
            let mut label_style = self.theme.style(TextRole::Muted);
            if let Some(background) = background_color {
                label_style.background = background;
            }
            let mut label = RichLine::default();
            label.push(" ".repeat(left_padding), padding_style, None);
            label.push(self.sanitize(language), label_style, None);
            if background {
                let label_width = self.line_width(&label);
                label.push(
                    " ".repeat(block_width.saturating_sub(label_width)),
                    padding_style,
                    None,
                );
            }
            output.push(self.clip_line(label, block_width));
        }

        for _ in 0..padding_top {
            output.push(self.code_content_line(
                RichLine::default(),
                block_width,
                content_width,
                left_padding,
                right_padding,
                bordered,
                padding_style,
                border_style,
                background,
            ));
        }

        output
    }

    pub(super) fn code_footer(&self, layout: &CodeLayout) -> Vec<RichLine> {
        let CodeLayout {
            block_width,
            content_width,
            left_padding,
            right_padding,
            bordered,
            padding_style,
            border_style,
            background,
            padding_bottom,
            ..
        } = *layout;
        let mut output = Vec::new();
        for _ in 0..padding_bottom {
            output.push(self.code_content_line(
                RichLine::default(),
                block_width,
                content_width,
                left_padding,
                right_padding,
                bordered,
                padding_style,
                border_style,
                background,
            ));
        }
        if bordered {
            output.push(self.code_border_line(block_width, None, false, border_style));
        }
        output
    }

    pub(super) fn code_border_line(
        &self,
        width: usize,
        label: Option<&str>,
        top: bool,
        border_style: TextStyle,
    ) -> RichLine {
        if width == 0 {
            return RichLine::default();
        }
        let glyphs = GlyphSet::for_capabilities(self.capabilities);
        if width < 3 {
            let mut line = RichLine::default();
            line.push(glyphs.horizontal.repeat(width), border_style, None);
            return line;
        }

        let mut line = RichLine::default();
        line.push(
            if top {
                glyphs.top_left
            } else {
                glyphs.bottom_left
            }
            .to_owned(),
            border_style,
            None,
        );
        let inner_width = width - 2;
        let safe_label = label.map(|label| self.sanitize(label));
        if top && safe_label.as_deref().is_some_and(|label| !label.is_empty()) && inner_width >= 4 {
            let label_width = inner_width.saturating_sub(3);
            let mut label_style = self.theme.style(TextRole::Muted);
            label_style.attributes.italic = true;
            let label = self.clip_runs(
                &[RichRun::new(
                    safe_label.unwrap_or_default(),
                    label_style,
                    None,
                )],
                label_width,
            );
            let rendered_label_width = self.line_width(&label);
            line.push(glyphs.horizontal.to_owned(), border_style, None);
            line.push(" ".to_owned(), border_style, None);
            line.extend(label);
            line.push(" ".to_owned(), border_style, None);
            line.push(
                glyphs
                    .horizontal
                    .repeat(inner_width.saturating_sub(rendered_label_width + 3)),
                border_style,
                None,
            );
        } else {
            line.push(glyphs.horizontal.repeat(inner_width), border_style, None);
        }
        line.push(
            if top {
                glyphs.top_right
            } else {
                glyphs.bottom_right
            }
            .to_owned(),
            border_style,
            None,
        );
        line
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn code_content_line(
        &self,
        row: RichLine,
        block_width: usize,
        content_width: usize,
        left_padding: usize,
        right_padding: usize,
        bordered: bool,
        padding_style: TextStyle,
        border_style: TextStyle,
        has_background: bool,
    ) -> RichLine {
        let glyphs = GlyphSet::for_capabilities(self.capabilities);
        let mut line = RichLine::default();
        if bordered {
            line.push(glyphs.vertical.to_owned(), border_style, None);
        }
        line.push(" ".repeat(left_padding), padding_style, None);
        let row_width = self.line_width(&row).min(content_width);
        line.extend(row);
        if bordered || has_background {
            line.push(
                " ".repeat(content_width.saturating_sub(row_width) + right_padding),
                padding_style,
                None,
            );
        }
        if bordered {
            line.push(glyphs.vertical.to_owned(), border_style, None);
        }
        self.clip_line(line, block_width)
    }
}

pub(super) fn visible_code_language(code: &CodeBlock) -> Option<&str> {
    code.language.as_deref().map(str::trim).filter(|language| {
        !language.is_empty()
            && !language.eq_ignore_ascii_case("text")
            && !language.eq_ignore_ascii_case("plaintext")
    })
}

// Includes the first overflowing grapheme. Never splits the overflow witness:
// a later combining/ZWJ suffix may still change that final grapheme's width.
pub(super) fn measured_prefix<'a>(
    source: &'a str,
    width: usize,
    policy: WidthPolicy,
    bytes: &mut u64,
) -> (&'a str, usize) {
    let mut end = 0;
    let mut cells: usize = 0;
    for (offset, grapheme) in source.grapheme_indices(true) {
        cells = cells.saturating_add(policy.grapheme_width(grapheme, cells));
        end = offset + grapheme.len();
        if cells > width {
            break;
        }
    }
    *bytes += end as u64;
    (&source[..end], cells)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CodeLayout {
    pub(super) block_width: usize,
    pub(super) content_width: usize,
    left_padding: usize,
    right_padding: usize,
    bordered: bool,
    padding_style: TextStyle,
    pub(super) code_text_style: TextStyle,
    border_style: TextStyle,
    background: bool,
    background_color: Option<Color>,
    padding_top: u16,
    padding_bottom: u16,
}

impl CodeLayout {
    pub(super) fn row(&self, renderer: &RichRenderer, row: RichLine) -> RichLine {
        renderer.code_content_line(
            row,
            self.block_width,
            self.content_width,
            self.left_padding,
            self.right_padding,
            self.bordered,
            self.padding_style,
            self.border_style,
            self.background,
        )
    }
}
