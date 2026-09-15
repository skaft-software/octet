//! LaTeX math rendering for terminals.
//!
//! Port of the upstream reference `packages/tui/src/latex.ts`
//! (pi @ `8a7b0c03dfb702663acafb6dc29f8acaa4ffe391`): the symbol tables, the
//! `LatexParser`, and the fraction/operator/matrix layout pass. [`render_latex`]
//! is the entry point; the `tables` module holds the mechanically generated
//! symbol tables.
//!
//! # Contract
//!
//! [`render_latex`] returns `Some(rendered)` or `None` — never a panic, never a
//! partial guess. `None` is the upstream `undefined` result: the expression
//! used syntax this renderer does not implement, or was malformed.
//!
//! Layout composes *semantic text*, not ANSI. Embedding components decide how
//! the returned lines are styled, wrapped, or clipped.
//!
//! The rich markdown renderer consumes this function: [`super::markdown::parse`]
//! renders a completed ```` ```latex ```` fence in display mode and falls back to
//! the original code-block source when the expression is unsupported or
//! oversized (see [`super::markdown::MAX_DIAGRAM_FENCE_BYTES`]).
//!
//! # Supported
//!
//! - the upstream symbol tables: greek letters, relations, arrows, operators,
//!   delimiters, `\mathbb`, `\mathcal`, `\mathfrak`, `\mathbf`, `\mathrm`,
//!   `\text`, negation (`\not`, `\nleq`, …), accents, and wrapped names
//! - sub/superscripts, including the Unicode-script fallback
//!   (`x_i^2` → `xᵢ²`)
//! - fractions (`\frac`, `\dfrac`, `\tfrac`), roots (`\sqrt`, `\sqrt[n]`),
//!   `\binom`, `\boxed`
//! - operator limits: in display mode a limit-taking command stacks its bounds
//!   over and under the operator (`\sum_{i=1}^{n}` renders three rows);
//!   `\limits` forces stacking and `\nolimits` forces inline scripts
//! - delimiter sizing: `\left…\right`, `\bigl`/`\Bigl`/…, `\middle`, and
//!   `\left.`/`\right|`
//! - environments: `matrix`, `pmatrix`, `bmatrix`, `Bmatrix`, `vmatrix`,
//!   `Vmatrix`, `smallmatrix`, `array`, `cases` (and `cases*`), `aligned`,
//!   `align`(`*`), `alignedat`/`alignat`, `gather`ed, `multline`,
//!   `split`, `equation`(`*`)
//! - multiple lines: `\\` row breaks and `&` alignment columns inside
//!   environments, with the upstream whitespace and line-joining rules
//!
//! # Fails closed
//!
//! Unsupported commands, missing/mismatched arguments and unbalanced groups
//! return `None` rather than rendering something misleading. Commands the
//! reference renderer does not implement (`\cfrac`, `\genfrac`, `\cancel`,
//! `\phantom`, `\hspace`, `\xrightarrow`, `\verb`, `\def`, `\usepackage`,
//! `tikzpicture`, …) are unsupported here too — see
//! `tests/latex_render.rs::UNSUPPORTED_COMMANDS`.
//!
//! Recursion is bounded by [`MAX_LATEX_NESTING_DEPTH`]: input deeper than the
//! cap fails closed instead of exhausting the stack. Once an expression is
//! known to be unrenderable the parser stops descending, so neither a deeply
//! nested `\frac` chain nor a wide expression does unbounded work.
//!
//! # Known difference from the reference
//!
//! The reference implementation indexes JavaScript UTF-16 code units, so a
//! non-BMP character (emoji, regional indicator) can be split mid-code-point
//! and produce lone surrogates. This port indexes `char`s, so it never emits
//! invalid UTF-8/UTF-16; on 2913 differential cases the only divergences were
//! of exactly this kind (11 of 3000, all on inputs containing non-BMP
//! characters, 0 on the same corpus with those inputs removed).
//!
//! # Tests
//!
//! `crates/sexy-tui-rs/tests/latex_render.rs` holds the behavioral goldens:
//! the upstream `latex.test.ts` corpus, captured box-drawing expectations for
//! operator limits / fractions / every matrix environment, and fail-closed
//! cases. Every expectation in it was captured from the reference renderer
//! running under Node with its real `visibleWidth`, so wide and combining
//! glyphs are measured identically.

mod tables;

use crate::width::display_width;

use tables::{
    accent, blackboard, is_display_limit_symbol, is_ignored_command, is_limit_operator,
    is_named_operator, is_negative_spacing_command, is_plain_wrapper, is_relation_command,
    is_size_command, is_spacing_command, negated_symbol, subscript, superscript, symbol,
};

/// Rendering options for [`render_latex`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderLatexOptions {
    /// Stack fractions and operator limits vertically for display math
    /// (upstream option `display`, default `false`).
    pub display: bool,
}

impl RenderLatexOptions {
    pub const fn display() -> Self {
        Self { display: true }
    }
}

const LAYOUT_MARKER_START: char = '\u{f0000}';
const LAYOUT_MARKER_END: char = '\u{f0001}';
const PROTECTED_SPACE: char = '\u{f0002}';
const NAMED_OPERATOR_START: char = '\u{f0004}';
const NAMED_OPERATOR_END: char = '\u{f0005}';
const NEGATIVE_SPACE: &str = "\u{0000}";
const COMBINING_LONG_SOLIDUS: char = '\u{338}';

/// Hard cap on recursive descent (`{…}` groups, `\frac` arguments, nested
/// environments).
///
/// The parser is recursive, so a hostile or malformed expression such as
/// thousands of unmatched `{` or `\frac{` would otherwise exhaust the thread
/// stack and abort the process — an unbounded-work failure, not a rendering
/// failure. Real math nesting stays far below this bound; input that exceeds
/// it fails closed ([`render_latex`] returns `None`) instead of recursing
/// further.
///
/// 64 is chosen against the smallest stack the renderer realistically runs
/// with (Rust's 2 MiB default thread stack) in an unoptimised build: a
/// `\frac` chain costs four parser frames per level, and that chain aborts
/// somewhere between 300 and 500 levels of input on a 2 MiB stack, so the cap
/// keeps a wide margin.
pub const MAX_LATEX_NESTING_DEPTH: usize = 64;

/// Render a basic LaTeX math expression as terminal-friendly Unicode text.
///
/// Returns `None` when the expression contains unsupported or malformed syntax
/// (the upstream `undefined` contract).
pub fn render_latex(source: &str, options: RenderLatexOptions) -> Option<String> {
    let mut layout_nodes: Vec<LayoutNode> = Vec::new();
    let rendered = {
        let mut parser = LatexParser::new(source, &mut layout_nodes, options.display);
        parser.render()?
    };
    if layout_nodes.is_empty() {
        return Some(rendered.replace(PROTECTED_SPACE, " "));
    }

    let lines = render_layout(&rendered, &layout_nodes).lines;
    let indentation = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.chars().count() - line.trim_start().chars().count())
        .min()
        .unwrap_or(0);
    Some(
        lines
            .iter()
            .map(|line| drop_chars(line, indentation).trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .replace(PROTECTED_SPACE, " "),
    )
}

/// Conventional rendering width of one layout cell.
fn visible_width(value: &str) -> usize {
    display_width(value)
}

/// Drop `count` leading characters, clamping like JavaScript `String.slice`.
fn drop_chars(value: &str, count: usize) -> &str {
    match value.char_indices().nth(count) {
        Some((index, _)) => &value[index..],
        None => "",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScriptKind {
    Sub,
    Sup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LowerStyle {
    Bracket,
    Script,
}

/// A deferred layout unit referenced by index from the rendered text.
#[derive(Clone, Debug, PartialEq, Eq)]
enum LayoutNode {
    Fraction {
        numerator: String,
        denominator: String,
    },
    Operator {
        operator: String,
        lower: Option<String>,
        upper: Option<String>,
    },
    Matrix {
        lines: Vec<String>,
        baseline: usize,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Layout {
    lines: Vec<String>,
    width: usize,
    baseline: usize,
}

// ---------------------------------------------------------------------------
// Inline text formatting
// ---------------------------------------------------------------------------

fn replace_characters(
    value: &str,
    table: fn(char) -> Option<&'static str>,
) -> Option<String> {
    let mut result = String::new();
    for character in value.chars() {
        result.push_str(table(character)?);
    }
    Some(result)
}

/// Upstream `value.replace(/\s*([=+-])\s*/g, "$1")`: whitespace that touches a
/// sign or relation character is dropped.
fn compact_sign_spacing(value: &str) -> String {
    let characters: Vec<char> = value.chars().collect();
    let mut output = String::with_capacity(value.len());
    let mut index = 0;
    while index < characters.len() {
        if !characters[index].is_whitespace() {
            output.push(characters[index]);
            index += 1;
            continue;
        }
        let mut run_end = index;
        while run_end < characters.len() && characters[run_end].is_whitespace() {
            run_end += 1;
        }
        let previous = index.checked_sub(1).map(|i| characters[i]);
        let next = characters.get(run_end).copied();
        let touches_sign = |value: Option<char>| matches!(value, Some('=' | '+' | '-'));
        if !touches_sign(previous) && !touches_sign(next) {
            for character in &characters[index..run_end] {
                output.push(*character);
            }
        }
        index = run_end;
    }
    output
}

fn format_script(value: &str, kind: ScriptKind) -> String {
    let value = value.trim();
    let table = match kind {
        ScriptKind::Sub => subscript,
        ScriptKind::Sup => superscript,
    };
    if let Some(unicode) = replace_characters(&compact_sign_spacing(value), table) {
        return unicode;
    }
    let prefix = match kind {
        ScriptKind::Sub => "_",
        ScriptKind::Sup => "^",
    };
    let single_character = value.chars().count() == 1;
    let plain_word = matches!(kind, ScriptKind::Sub)
        && !value.is_empty()
        && value.chars().all(|character| character.is_ascii_alphabetic());
    if single_character || plain_word {
        return format!("{prefix}{value}");
    }
    format!("{prefix}({value})")
}

fn is_simple_math_word(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_alphabetic() || character.is_numeric() || character == '.')
}

fn format_fraction(numerator: &str, denominator: &str) -> String {
    let numerator = numerator.trim();
    let denominator = denominator.trim();
    let simple_numerator = is_simple_math_word(numerator);
    let simple_denominator = (!denominator.is_empty()
        && denominator
            .chars()
            .all(|character| character.is_numeric() || character == '.'))
        || denominator.chars().count() == 1;
    format!(
        "{}/{}",
        if simple_numerator {
            numerator.to_owned()
        } else {
            format!("({numerator})")
        },
        if simple_denominator {
            denominator.to_owned()
        } else {
            format!("({denominator})")
        }
    )
}

fn format_root(value: &str, symbol: &str) -> String {
    let value = value.trim();
    if is_simple_math_word(value) {
        format!("{symbol}{value}")
    } else {
        format!("{symbol}({value})")
    }
}

fn is_letter_or_number(character: char) -> bool {
    character.is_alphabetic() || character.is_numeric()
}

/// Upstream `normalizeOutput` trimming/filtering rules, without the line pass.
fn normalize_named_operators(value: &str) -> String {
    let characters: Vec<char> = value.chars().collect();
    let mut output = String::with_capacity(value.len());
    for (index, character) in characters.iter().enumerate() {
        if *character == NAMED_OPERATOR_START {
            let previous = index.checked_sub(1).map(|i| characters[i]);
            let spaced = matches!(
                previous,
                Some(previous)
                    if is_letter_or_number(previous)
                        || previous == ')'
                        || previous == '}'
                        || previous == LAYOUT_MARKER_END
            );
            if spaced {
                output.push(' ');
            }
            continue;
        }
        if *character == NAMED_OPERATOR_END {
            let next = characters.get(index + 1).copied();
            let spaced = matches!(
                next,
                Some(next)
                    if is_letter_or_number(next)
                        || next == '√'
                        || next == LAYOUT_MARKER_START
            );
            if spaced {
                output.push(' ');
            }
            continue;
        }
        output.push(*character);
    }
    output
}

fn collapse_inline_whitespace(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut previous_was_space = false;
    for character in value.chars() {
        if character == ' ' || character == '\t' {
            if !previous_was_space {
                output.push(' ');
                previous_was_space = true;
            }
            continue;
        }
        previous_was_space = false;
        output.push(character);
    }
    output
}

fn normalize_output(value: &str) -> String {
    let normalized = normalize_named_operators(value);
    let lines: Vec<String> = normalized
        .split('\n')
        .map(|line| collapse_inline_whitespace(line).trim().to_owned())
        .collect();
    let filtered: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(index, line)| !line.is_empty() || (*index > 0 && *index < lines.len() - 1))
        .map(|(_, line)| line.as_str())
        .collect();
    filtered.join("\n").trim().to_owned()
}

// ---------------------------------------------------------------------------
// Layout pass
// ---------------------------------------------------------------------------

fn pad_layout_line(line: &str, width: usize, centered: bool) -> String {
    let padding = width.saturating_sub(visible_width(line));
    let left = if centered { padding / 2 } else { 0 };
    format!(
        "{}{}{}",
        " ".repeat(left),
        line,
        " ".repeat(padding - left)
    )
}

fn join_layouts(layouts: &[Layout]) -> Layout {
    if layouts.is_empty() {
        return Layout {
            lines: vec![String::new()],
            width: 0,
            baseline: 0,
        };
    }
    let baseline = layouts
        .iter()
        .map(|layout| layout.baseline)
        .max()
        .unwrap_or(0);
    let below = layouts
        .iter()
        .map(|layout| layout.lines.len().saturating_sub(layout.baseline + 1))
        .max()
        .unwrap_or(0);
    let mut lines = Vec::with_capacity(baseline + below + 1);
    for row in 0..=baseline + below {
        let mut line = String::new();
        for layout in layouts {
            let source_row = row as isize - baseline as isize + layout.baseline as isize;
            if source_row >= 0 && (source_row as usize) < layout.lines.len() {
                line.push_str(&pad_layout_line(
                    &layout.lines[source_row as usize],
                    layout.width,
                    false,
                ));
            } else {
                line.push_str(&" ".repeat(layout.width));
            }
        }
        lines.push(line.trim_end().to_owned());
    }
    Layout {
        lines,
        width: layouts.iter().map(|layout| layout.width).sum(),
        baseline,
    }
}

/// Find the next `\u{f0000}<digits>\u{f0001}` marker at or after `from`.
/// Returns `(start, end_exclusive, node_index)`.
fn next_layout_marker(characters: &[char], from: usize) -> Option<(usize, usize, usize)> {
    let mut index = from;
    while index < characters.len() {
        if characters[index] != LAYOUT_MARKER_START {
            index += 1;
            continue;
        }
        let mut cursor = index + 1;
        let digits_start = cursor;
        while cursor < characters.len() && characters[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if cursor > digits_start && characters.get(cursor) == Some(&LAYOUT_MARKER_END) {
            let digits: String = characters[digits_start..cursor].iter().collect();
            if let Ok(node_index) = digits.parse::<usize>() {
                return Some((index, cursor + 1, node_index));
            }
        }
        index += 1;
    }
    None
}

/// Detect a trailing `\u{f0000}<digits>\u{f0001}` (`TRAILING_LAYOUT_MARKER_PATTERN$`).
fn trailing_layout_marker(value: &str) -> Option<usize> {
    let mut characters = value.chars().rev();
    if characters.next() != Some(LAYOUT_MARKER_END) {
        return None;
    }
    let mut digits = String::new();
    for character in characters {
        if character.is_ascii_digit() {
            digits.push(character);
            continue;
        }
        if character == LAYOUT_MARKER_START && !digits.is_empty() {
            let digits: String = digits.chars().rev().collect();
            return digits.parse::<usize>().ok();
        }
        return None;
    }
    None
}

fn starts_with_whitespace(value: &str) -> bool {
    value.chars().next().is_some_and(char::is_whitespace)
}

fn ends_with_whitespace(value: &str) -> bool {
    value.chars().next_back().is_some_and(char::is_whitespace)
}

fn render_layout(source: &str, nodes: &[LayoutNode]) -> Layout {
    let mut rendered_lines: Vec<String> = Vec::new();
    let mut first_baseline = 0usize;
    for source_line in source.split('\n') {
        let characters: Vec<char> = source_line.chars().collect();
        let mut layouts: Vec<Layout> = Vec::new();
        let mut position = 0usize;
        let mut previous_node: Option<&LayoutNode> = None;
        let mut scan = 0usize;
        while let Some((start, end, node_index)) = next_layout_marker(&characters, scan) {
            scan = end;
            let Some(node) = nodes.get(node_index) else {
                continue;
            };
            if start > position {
                let sliced: String = characters[position..start].iter().collect();
                let trimmed = if previous_node.is_some() {
                    sliced.trim_start()
                } else {
                    sliced.as_str()
                };
                let trimmed = trimmed.trim_end();
                let preserve_leading_space =
                    matches!(previous_node, Some(LayoutNode::Matrix { .. }))
                        && starts_with_whitespace(&sliced);
                let preserve_trailing_space = matches!(node, LayoutNode::Matrix { .. })
                    && ends_with_whitespace(&sliced);
                let text = if !trimmed.is_empty() {
                    format!(
                        "{}{}{}",
                        if preserve_leading_space { " " } else { "" },
                        trimmed,
                        if preserve_trailing_space { " " } else { "" }
                    )
                } else if preserve_leading_space || preserve_trailing_space {
                    " ".to_owned()
                } else {
                    String::new()
                };
                layouts.push(Layout {
                    width: visible_width(&text),
                    lines: vec![text],
                    baseline: 0,
                });
            }
            match node {
                LayoutNode::Fraction {
                    numerator,
                    denominator,
                } => {
                    let numerator = render_layout(numerator, nodes);
                    let denominator = render_layout(denominator, nodes);
                    let content_width = numerator.width.max(denominator.width).max(1);
                    let width = content_width + 2;
                    let mut lines: Vec<String> = numerator
                        .lines
                        .iter()
                        .map(|line| pad_layout_line(line, width, true))
                        .collect();
                    lines.push(format!(" {} ", "─".repeat(content_width)));
                    lines.extend(
                        denominator
                            .lines
                            .iter()
                            .map(|line| pad_layout_line(line, width, true)),
                    );
                    layouts.push(Layout {
                        baseline: numerator.lines.len(),
                        lines,
                        width,
                    });
                }
                LayoutNode::Operator {
                    operator,
                    lower,
                    upper,
                } => {
                    let content_width = visible_width(operator)
                        .max(lower.as_deref().map(visible_width).unwrap_or(0))
                        .max(upper.as_deref().map(visible_width).unwrap_or(0));
                    let mut lines: Vec<String> = Vec::new();
                    if let Some(upper) = upper {
                        lines.push(format!("{} ", pad_layout_line(upper, content_width, true)));
                    }
                    lines.push(format!(
                        "{} ",
                        pad_layout_line(operator, content_width, true)
                    ));
                    if let Some(lower) = lower {
                        lines.push(format!("{} ", pad_layout_line(lower, content_width, true)));
                    }
                    layouts.push(Layout {
                        baseline: if upper.is_some() { 1 } else { 0 },
                        lines,
                        width: content_width + 1,
                    });
                }
                LayoutNode::Matrix { lines, baseline } => {
                    let width = lines.iter().map(|line| visible_width(line)).max().unwrap_or(0);
                    layouts.push(Layout {
                        lines: lines
                            .iter()
                            .map(|line| pad_layout_line(line, width, false))
                            .collect(),
                        width,
                        baseline: *baseline,
                    });
                }
            }
            position = end;
            previous_node = Some(node);
        }
        if position < characters.len() {
            let sliced: String = characters[position..].iter().collect();
            let trimmed = if previous_node.is_some() {
                sliced.trim_start()
            } else {
                sliced.as_str()
            };
            let text = if matches!(previous_node, Some(LayoutNode::Matrix { .. }))
                && starts_with_whitespace(&sliced)
            {
                format!(" {trimmed}")
            } else {
                trimmed.to_owned()
            };
            layouts.push(Layout {
                width: visible_width(&text),
                lines: vec![text],
                baseline: 0,
            });
        }
        let line_layout = join_layouts(&layouts);
        if rendered_lines.is_empty() {
            first_baseline = line_layout.baseline;
        }
        rendered_lines.extend(line_layout.lines);
    }
    Layout {
        width: rendered_lines
            .iter()
            .map(|line| visible_width(line))
            .max()
            .unwrap_or(0),
        lines: rendered_lines,
        baseline: first_baseline,
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct LatexParser<'nodes> {
    source: Vec<char>,
    layout_nodes: &'nodes mut Vec<LayoutNode>,
    display: bool,
    position: usize,
    supported: bool,
    stack_fractions: bool,
    depth: usize,
}

impl<'nodes> LatexParser<'nodes> {
    fn new(source: &str, layout_nodes: &'nodes mut Vec<LayoutNode>, display: bool) -> Self {
        Self::nested(source, layout_nodes, display, 0)
    }

    fn nested(
        source: &str,
        layout_nodes: &'nodes mut Vec<LayoutNode>,
        display: bool,
        depth: usize,
    ) -> Self {
        Self {
            source: source.chars().collect(),
            layout_nodes,
            display,
            position: 0,
            supported: true,
            stack_fractions: true,
            depth,
        }
    }

    fn character_at(&self, index: usize) -> Option<char> {
        self.source.get(index).copied()
    }

    fn render(&mut self) -> Option<String> {
        let rendered = self.parse_sequence(None);
        if !self.supported || self.position != self.source.len() {
            return None;
        }
        Some(normalize_output(&rendered))
    }

    fn parse_sequence(&mut self, end_character: Option<char>) -> String {
        if self.depth >= MAX_LATEX_NESTING_DEPTH {
            self.supported = false;
            return String::new();
        }
        self.depth += 1;
        let result = self.parse_sequence_inner(end_character);
        self.depth -= 1;
        result
    }

    fn parse_sequence_inner(&mut self, end_character: Option<char>) -> String {
        let mut result = String::new();
        while self.position < self.source.len() {
            // Once the expression is known to be unrenderable (unsupported
            // syntax or the nesting cap), stop descending. Continuing would
            // keep re-entering the argument parser on the same unconsumed
            // token and grow the stack with the input length.
            if !self.supported {
                return result;
            }
            let character = self.source[self.position];
            if end_character == Some(character) {
                self.position += 1;
                return result;
            }
            if character == '}' {
                self.supported = false;
                return result;
            }
            if character == '{' {
                self.position += 1;
                let nested = self.parse_sequence(Some('}'));
                result.push_str(&nested);
                continue;
            }
            if character == '\\' {
                let command = self.parse_command();
                if command == NEGATIVE_SPACE {
                    let mut trimmed = result.trim_end().to_owned();
                    if trimmed.ends_with(NAMED_OPERATOR_END) {
                        trimmed.truncate(trimmed.len() - NAMED_OPERATOR_END.len_utf8());
                    }
                    result = trimmed;
                } else {
                    result.push_str(&command);
                }
                continue;
            }
            if character == '^' || character == '_' {
                self.position += 1;
                let trimmed = result.trim_end().to_owned();
                result = trimmed;
                let argument = self.parse_required_argument(false);
                let script = format_script(
                    &argument,
                    if character == '_' {
                        ScriptKind::Sub
                    } else {
                        ScriptKind::Sup
                    },
                );
                if result.ends_with(NAMED_OPERATOR_END) {
                    let head = result.len() - NAMED_OPERATOR_END.len_utf8();
                    result.truncate(head);
                    result.push_str(&script);
                    result.push(NAMED_OPERATOR_END);
                } else {
                    result.push_str(&script);
                }
                continue;
            }
            if character.is_whitespace() {
                result.push_str(&self.parse_whitespace());
                continue;
            }
            if character == '=' || character == '<' || character == '>' {
                let trimmed = result.trim_end().to_owned();
                result = format!("{trimmed} {character} ");
                self.position += 1;
                continue;
            }
            if character == '&' {
                self.position += 1;
                continue;
            }
            if character == '~' {
                self.position += 1;
                result.push(' ');
                continue;
            }
            if character == '.' {
                if let Some(node_index) = trailing_layout_marker(&result) {
                    let is_matrix = matches!(
                        self.layout_nodes.get(node_index),
                        Some(LayoutNode::Matrix { .. })
                    );
                    if is_matrix {
                        if let Some(LayoutNode::Matrix { lines, .. }) =
                            self.layout_nodes.get_mut(node_index)
                        {
                            if let Some(last_line) = lines.last_mut() {
                                last_line.push(character);
                            }
                        }
                        self.position += 1;
                        continue;
                    }
                }
            }
            result.push(character);
            self.position += 1;
        }

        if end_character.is_some() {
            self.supported = false;
        }
        result
    }

    fn parse_whitespace(&mut self) -> String {
        while self.position < self.source.len() && self.source[self.position].is_whitespace() {
            self.position += 1;
        }
        " ".to_owned()
    }

    fn parse_command(&mut self) -> String {
        if !self.supported {
            return String::new();
        }
        self.position += 1;
        if self.position >= self.source.len() {
            self.supported = false;
            return String::new();
        }

        let first = self.source[self.position];
        if first == '\n' || first == '\r' {
            self.position += 1;
            if first == '\r' && self.character_at(self.position) == Some('\n') {
                self.position += 1;
            }
            return " ".to_owned();
        }

        let command: String = if first.is_ascii_alphabetic() {
            let start = self.position;
            while self.position < self.source.len()
                && self.source[self.position].is_ascii_alphabetic()
            {
                self.position += 1;
            }
            self.source[start..self.position].iter().collect()
        } else {
            self.position += 1;
            first.to_string()
        };
        let command = command.as_str();

        if command == "\\" {
            return "\n".to_owned();
        }
        if is_spacing_command(command) {
            return " ".to_owned();
        }
        if is_negative_spacing_command(command) {
            return NEGATIVE_SPACE.to_owned();
        }
        if is_ignored_command(command) {
            return String::new();
        }
        if matches!(command, "{" | "}" | "$" | "%" | "#" | "_" | "&") {
            return command.to_owned();
        }
        if command == "|" {
            return "‖".to_owned();
        }
        if command == "not" {
            let value = self.parse_required_argument(false);
            let value = value.trim();
            if let Some(negated) = negated_symbol(value) {
                return format!(" {negated} ");
            }
            let mut characters = value.chars();
            let Some(first) = characters.next() else {
                self.supported = false;
                return String::new();
            };
            let rest: String = characters.collect();
            return format!(" {first}{COMBINING_LONG_SOLIDUS}{rest} ");
        }
        if is_limit_operator(command) {
            return self.parse_operator(command, LowerStyle::Bracket, true, true);
        }
        if let Some(symbol) = symbol(command) {
            if is_display_limit_symbol(command) {
                return self.parse_operator(symbol, LowerStyle::Script, true, false);
            }
            if command == "cdot" || command == "times" || is_relation_command(command) {
                return format!(" {symbol} ");
            }
            return symbol.to_owned();
        }
        if is_named_operator(command) {
            return format!("{NAMED_OPERATOR_START}{command}{NAMED_OPERATOR_END}");
        }
        if is_size_command(command) {
            return String::new();
        }
        if matches!(command, "left" | "middle" | "right") {
            if self.character_at(self.position) == Some('.') {
                self.position += 1;
            }
            return String::new();
        }
        if matches!(command, "frac" | "dfrac" | "tfrac") {
            let should_stack = self.display && self.stack_fractions && command != "tfrac";
            let numerator = self.parse_required_argument(!should_stack);
            let denominator = self.parse_required_argument(!should_stack);
            if should_stack {
                let index = self.layout_nodes.len();
                self.layout_nodes.push(LayoutNode::Fraction {
                    numerator: normalize_output(&numerator),
                    denominator: normalize_output(&denominator),
                });
                return format!("{LAYOUT_MARKER_START}{index}{LAYOUT_MARKER_END}");
            }
            return format_fraction(&numerator, &denominator);
        }
        if command == "sqrt" {
            let degree = self
                .parse_optional_argument()
                .map(|value| value.trim().to_owned());
            let value = self.parse_required_argument(true);
            return match degree.as_deref() {
                None | Some("2") => format_root(&value, "√"),
                Some("3") => format_root(&value, "∛"),
                Some("4") => format_root(&value, "∜"),
                Some(degree) => format!(
                    "{}{}",
                    format_script(degree, ScriptKind::Sup),
                    format_root(&value, "√")
                ),
            };
        }
        if command == "boxed" || command == "fbox" {
            let value = self.parse_required_argument(true);
            return format!("[{}]", value.trim());
        }
        if matches!(command, "binom" | "dbinom" | "tbinom") {
            let upper = self.parse_required_argument(true);
            let lower = self.parse_required_argument(true);
            return format!("({upper} choose {lower})");
        }
        if let Some(accent) = accent(command) {
            let value = self.parse_required_argument(true);
            return if value.chars().count() == 1 {
                format!("{value}{accent}")
            } else {
                format!("{command}({value})")
            };
        }
        if command == "mathbb" {
            let value = self.parse_required_argument(true);
            let mut output = String::with_capacity(value.len());
            for character in value.chars() {
                match blackboard(character) {
                    Some(symbol) => output.push_str(symbol),
                    None => output.push(character),
                }
            }
            return output;
        }
        if command == "operatorname" {
            let starred = self.character_at(self.position) == Some('*');
            if starred {
                self.position += 1;
            }
            let argument = self.parse_required_argument(true);
            let operator = normalize_output(&argument).trim().to_owned();
            return self.parse_operator(&operator, LowerStyle::Bracket, starred, true);
        }
        if command == "mod" || command == "bmod" {
            return " mod ".to_owned();
        }
        if command == "pmod" || command == "pod" {
            let value = self.parse_required_argument(true);
            let value = value.trim();
            return if command == "pmod" {
                format!(" (mod {value})")
            } else {
                format!(" ({value})")
            };
        }
        if command == "overset" || command == "stackrel" {
            let upper = self.parse_required_argument(true);
            let value = self.parse_required_argument(true);
            return format!("{}{}", value.trim(), format_script(&upper, ScriptKind::Sup));
        }
        if command == "underset" {
            let lower = self.parse_required_argument(true);
            let value = self.parse_required_argument(true);
            return format!("{}{}", value.trim(), format_script(&lower, ScriptKind::Sub));
        }
        if is_plain_wrapper(command) {
            let value = self.parse_required_argument(true);
            return if command.starts_with("text") || command == "mbox" {
                value
            } else {
                value.trim().to_owned()
            };
        }
        if command == "begin" {
            return self.parse_environment();
        }
        if command == "end" {
            self.supported = false;
            return String::new();
        }

        self.supported = false;
        format!("\\{command}")
    }

    fn parse_operator(
        &mut self,
        operator: &str,
        inline_lower_style: LowerStyle,
        display_limits: bool,
        spaced: bool,
    ) -> String {
        let mut use_display_limits = display_limits;
        let mut modifier_position = self.position;
        while modifier_position < self.source.len()
            && matches!(self.source[modifier_position], ' ' | '\t')
        {
            modifier_position += 1;
        }
        if self.character_at(modifier_position) == Some('\\') {
            let start = modifier_position + 1;
            let mut end = start;
            while end < self.source.len() && self.source[end].is_ascii_alphabetic() {
                end += 1;
            }
            let name: String = self.source[start..end].iter().collect();
            if name == "limits" || name == "nolimits" {
                use_display_limits = name == "limits";
                self.position = end;
            }
        }

        let mut lower: Option<String> = None;
        let mut upper: Option<String> = None;
        loop {
            let mut script_position = self.position;
            while script_position < self.source.len()
                && matches!(self.source[script_position], ' ' | '\t')
            {
                script_position += 1;
            }
            let kind = self.character_at(script_position);
            if kind != Some('_') && kind != Some('^') {
                break;
            }
            self.position = script_position + 1;
            let argument = self.parse_required_argument(false);
            let value: String = normalize_output(&argument)
                .chars()
                .filter(|character| *character != ' ')
                .collect();
            if kind == Some('_') {
                if lower.is_some() {
                    self.supported = false;
                }
                lower = Some(value);
            } else {
                if upper.is_some() {
                    self.supported = false;
                }
                upper = Some(value);
            }
        }

        if self.display && use_display_limits && (lower.is_some() || upper.is_some()) {
            let index = self.layout_nodes.len();
            self.layout_nodes.push(LayoutNode::Operator {
                operator: operator.to_owned(),
                lower,
                upper,
            });
            return format!("{LAYOUT_MARKER_START}{index}{LAYOUT_MARKER_END}");
        }

        let mut rendered = operator.to_owned();
        if let Some(lower) = &lower {
            rendered.push_str(&match inline_lower_style {
                LowerStyle::Bracket => format!("[{lower}]"),
                LowerStyle::Script => format_script(lower, ScriptKind::Sub),
            });
        }
        if let Some(upper) = &upper {
            rendered.push_str(&format_script(upper, ScriptKind::Sup));
        }
        if spaced {
            format!(" {rendered} ")
        } else {
            rendered
        }
    }

    fn parse_required_argument(&mut self, stack_fractions: bool) -> String {
        let previous_stack_fractions = self.stack_fractions;
        self.stack_fractions = previous_stack_fractions && stack_fractions;
        let value = self.parse_required_argument_value();
        self.stack_fractions = previous_stack_fractions;
        value
    }

    fn parse_required_argument_value(&mut self) -> String {
        if !self.supported {
            return String::new();
        }
        while self.position < self.source.len() && self.source[self.position].is_whitespace() {
            self.position += 1;
        }
        if self.position >= self.source.len() {
            self.supported = false;
            return String::new();
        }
        match self.source[self.position] {
            '{' => {
                self.position += 1;
                self.parse_sequence(Some('}'))
            }
            '\\' => self.parse_command(),
            character => {
                self.position += 1;
                character.to_string()
            }
        }
    }

    fn parse_optional_argument(&mut self) -> Option<String> {
        while self.position < self.source.len()
            && matches!(self.source[self.position], ' ' | '\t')
        {
            self.position += 1;
        }
        if self.character_at(self.position) != Some('[') {
            return None;
        }
        let end = ((self.position + 1)..self.source.len())
            .find(|index| self.source[*index] == ']');
        let Some(end) = end else {
            self.supported = false;
            return None;
        };
        let value: String = self.source[self.position + 1..end].iter().collect();
        self.position = end + 1;
        Some(self.render_nested(&value, true))
    }

    fn read_raw_group(&mut self) -> Option<String> {
        while self.position < self.source.len()
            && matches!(self.source[self.position], ' ' | '\t')
        {
            self.position += 1;
        }
        if self.character_at(self.position) != Some('{') {
            self.supported = false;
            return None;
        }

        self.position += 1;
        let start = self.position;
        let mut depth = 1usize;
        while self.position < self.source.len() {
            let character = self.source[self.position];
            if character == '\\' {
                self.position += 2;
                continue;
            }
            if character == '{' {
                depth += 1;
            }
            if character == '}' {
                depth -= 1;
            }
            if depth == 0 {
                let value: String = self.source[start..self.position].iter().collect();
                self.position += 1;
                return Some(value);
            }
            self.position += 1;
        }
        self.supported = false;
        None
    }

    fn parse_environment(&mut self) -> String {
        let Some(environment) = self.read_raw_group() else {
            return String::new();
        };
        let end_marker = format!("\\end{{{environment}}}");
        let Some(end) = find_from(&self.source, &end_marker, self.position) else {
            self.supported = false;
            return String::new();
        };
        let body: String = self.source[self.position..end].iter().collect();
        self.position = end + end_marker.chars().count();

        if matches!(
            environment.as_str(),
            "equation" | "equation*" | "displaymath"
        ) {
            return self.render_nested(&body, true).trim().to_owned();
        }

        if matches!(
            environment.as_str(),
            "aligned"
                | "align"
                | "align*"
                | "alignedat"
                | "alignat"
                | "alignat*"
                | "gather"
                | "gathered"
                | "multline"
                | "multline*"
                | "split"
        ) {
            let aligned_at = matches!(environment.as_str(), "alignedat" | "alignat" | "alignat*");
            let aligned_body = if aligned_at {
                strip_leading_group(&body)
            } else {
                body.clone()
            };
            let rows: Vec<String> = split_environment_rows(&aligned_body)
                .iter()
                .map(|row| {
                    let cells: Vec<&str> = row.split('&').collect();
                    let source = if aligned_at {
                        let mut groups: Vec<String> = Vec::new();
                        let mut index = 0;
                        while index < cells.len() {
                            groups.push(format!(
                                "{}{}",
                                cells[index],
                                cells.get(index + 1).copied().unwrap_or("")
                            ));
                            index += 2;
                        }
                        groups.join(" ")
                    } else {
                        cells.join("")
                    };
                    self.render_nested(&source, true).trim().to_owned()
                })
                .filter(|row| !row.is_empty())
                .collect();
            return rows.join("\n");
        }

        if matches!(environment.as_str(), "cases" | "cases*") {
            let rows: Vec<Vec<String>> = split_environment_rows(&body)
                .iter()
                .map(|row| {
                    row.split('&')
                        .map(|cell| self.render_nested(cell, false).trim().to_owned())
                        .collect()
                })
                .filter(|row: &Vec<String>| row.iter().any(|cell| !cell.is_empty()))
                .collect();
            return rows
                .iter()
                .enumerate()
                .map(|(index, row)| {
                    let value = strip_trailing_comma(row.first().map(String::as_str).unwrap_or(""));
                    let condition = row.get(1).cloned().unwrap_or_default();
                    let delimiter = if index == 0 {
                        "⎧"
                    } else if index == rows.len() - 1 {
                        "⎩"
                    } else {
                        "⎨"
                    };
                    let condition_prefix = if starts_with_case_keyword(&condition) {
                        " "
                    } else {
                        " if "
                    };
                    let suffix = if condition.is_empty() {
                        String::new()
                    } else {
                        format!("{condition_prefix}{condition}")
                    };
                    format!("{delimiter} {value}{suffix}")
                })
                .collect::<Vec<_>>()
                .join("\n");
        }

        if matches!(
            environment.as_str(),
            "array"
                | "matrix"
                | "smallmatrix"
                | "pmatrix"
                | "bmatrix"
                | "Bmatrix"
                | "vmatrix"
                | "Vmatrix"
        ) {
            let matrix_body = if environment == "array" {
                strip_leading_group(&body)
            } else {
                body
            };
            return self.render_matrix(&environment, &matrix_body);
        }

        self.supported = false;
        body
    }

    fn render_matrix(&mut self, environment: &str, body: &str) -> String {
        let matrix: Vec<Vec<String>> = split_environment_rows(body)
            .iter()
            .map(|row| {
                row.split('&')
                    .map(|cell| self.render_nested(cell, false).trim().to_owned())
                    .collect()
            })
            .filter(|row: &Vec<String>| row.iter().any(|cell| !cell.is_empty()))
            .collect();
        let column_count = matrix.iter().map(|row| row.len()).max().unwrap_or(0);
        let column_widths: Vec<usize> = (0..column_count)
            .map(|column| {
                matrix
                    .iter()
                    .map(|row| {
                        visible_width(row.get(column).map(String::as_str).unwrap_or(""))
                    })
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let rows: Vec<String> = matrix
            .iter()
            .map(|row| {
                (0..column_count)
                    .map(|column| {
                        let cell = row.get(column).map(String::as_str).unwrap_or("");
                        format!(
                            "{cell}{}",
                            PROTECTED_SPACE
                                .to_string()
                                .repeat(column_widths[column].saturating_sub(visible_width(cell)))
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" │ ")
            })
            .collect();

        let lines: Vec<String> = if matches!(environment, "array" | "matrix" | "smallmatrix") {
            rows
        } else {
            let delimiters: [&str; 6] = match environment {
                "pmatrix" => ["⎛", "⎞", "⎜", "⎟", "⎝", "⎠"],
                "bmatrix" => ["⎡", "⎤", "⎢", "⎥", "⎣", "⎦"],
                "Bmatrix" => ["⎧", "⎫", "⎨", "⎬", "⎩", "⎭"],
                "vmatrix" => ["│", "│", "│", "│", "│", "│"],
                "Vmatrix" => ["║", "║", "║", "║", "║", "║"],
                _ => {
                    self.supported = false;
                    return rows.join("\n");
                }
            };
            rows.iter()
                .enumerate()
                .map(|(index, row)| {
                    let left = if index == 0 {
                        delimiters[0]
                    } else if index == rows.len() - 1 {
                        delimiters[4]
                    } else {
                        delimiters[2]
                    };
                    let right = if index == 0 {
                        delimiters[1]
                    } else if index == rows.len() - 1 {
                        delimiters[5]
                    } else {
                        delimiters[3]
                    };
                    format!("{left} {row} {right}")
                })
                .collect()
        };

        if lines.len() <= 1 {
            return lines.first().cloned().unwrap_or_default();
        }
        let index = self.layout_nodes.len();
        self.layout_nodes.push(LayoutNode::Matrix { lines, baseline: 0 });
        format!("{LAYOUT_MARKER_START}{index}{LAYOUT_MARKER_END}")
    }

    fn render_nested(&mut self, source: &str, stack_fractions: bool) -> String {
        let display = self.display && stack_fractions;
        let depth = self.depth;
        let rendered = {
            let mut parser = LatexParser::nested(source, &mut *self.layout_nodes, display, depth);
            parser.render()
        };
        match rendered {
            Some(rendered) => rendered,
            None => {
                self.supported = false;
                source.to_owned()
            }
        }
    }
}

/// Find `needle` in `characters` at or after `from` (`String.indexOf`).
fn find_from(characters: &[char], needle: &str, from: usize) -> Option<usize> {
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() || from + needle.len() > characters.len() {
        return None;
    }
    (from..=characters.len() - needle.len())
        .find(|index| characters[*index..*index + needle.len()] == needle[..])
}

/// Split environment rows on `\\` with an optional `\\[4pt]` row spacing.
fn split_environment_rows(body: &str) -> Vec<String> {
    let characters: Vec<char> = body.chars().collect();
    let mut rows: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut index = 0;
    while index < characters.len() {
        if characters[index] == '\\' && characters.get(index + 1) == Some(&'\\') {
            let mut next = index + 2;
            if characters.get(next) == Some(&'[') {
                let mut close = next + 1;
                while close < characters.len() && characters[close] != ']' && characters[close] != '\n'
                {
                    close += 1;
                }
                if characters.get(close) == Some(&']') {
                    next = close + 1;
                }
            }
            rows.push(std::mem::take(&mut current));
            index = next;
            continue;
        }
        current.push(characters[index]);
        index += 1;
    }
    rows.push(current);
    rows
}

/// Drop a leading `{...}` column specification (`array`, `alignedat`).
fn strip_leading_group(body: &str) -> String {
    let characters: Vec<char> = body.chars().collect();
    let mut index = 0;
    while index < characters.len() && characters[index].is_whitespace() {
        index += 1;
    }
    if characters.get(index) != Some(&'{') {
        return body.to_owned();
    }
    let Some(close) = (index + 1..characters.len()).find(|cursor| characters[*cursor] == '}') else {
        return body.to_owned();
    };
    characters[close + 1..].iter().collect()
}

fn strip_trailing_comma(value: &str) -> String {
    let trimmed = value.trim_end();
    match trimmed.strip_suffix(',') {
        Some(head) => head.trim_end().to_owned(),
        None => trimmed.to_owned(),
    }
}

/// `/^(?:if|when|for|otherwise)\b/i`
fn starts_with_case_keyword(condition: &str) -> bool {
    let lowered = condition.to_lowercase();
    for keyword in ["if", "when", "for", "otherwise"] {
        if let Some(rest) = lowered.strip_prefix(keyword) {
            return match rest.chars().next() {
                None => true,
                Some(character) => !(character.is_alphanumeric() || character == '_'),
            };
        }
    }
    false
}
