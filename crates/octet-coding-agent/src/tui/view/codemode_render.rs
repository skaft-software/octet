//! Literal JavaScript source and bounded output for Codemode tool calls.

use sexy_tui_rs::{visible_width, RichRenderer};

use super::terminal_text::sanitize_for_terminal;
use super::tool_render::tool_value_indent_width;
use super::{fit_line, subdued_text, wrap_hanging, ToolPanel};
use crate::tui::theme::OctetTheme;

const COMPACT_SOURCE_ROWS: usize = 3;
const COMPACT_OUTPUT_LINES: usize = 5;
const COMPACT_OUTPUT_CHARS: usize = 600;

/// Only Codemode's documented `code` field is a source display contract.
/// Never infer source from another extension's arbitrary argument envelope.
pub(super) fn source(panel: &ToolPanel) -> Option<String> {
    if panel.name != "codemode" {
        return None;
    }
    let args: serde_json::Value = serde_json::from_str(&panel.args).ok()?;
    args.get("code")
        .and_then(serde_json::Value::as_str)
        .map(sanitize_for_terminal)
}

pub(super) fn render_source(
    source: &str,
    renderer: &RichRenderer,
    theme: &OctetTheme,
    width: u16,
    expanded: bool,
) -> Vec<String> {
    let label = "Codemode";
    let indent = tool_value_indent_width(label);
    let prefix = format!(
        "{}{}",
        theme.bold(&theme.fg("foreground", label)),
        " ".repeat(indent.saturating_sub(visible_width(label)))
    );
    let continuation = " ".repeat(indent);
    let content_width = width.saturating_sub(indent as u16).max(1);
    let document = renderer.render_inline_syntax_wrapped(source, "javascript", content_width);
    let hidden = if expanded {
        0
    } else {
        document.lines.len().saturating_sub(COMPACT_SOURCE_ROWS)
    };
    let shown = document.lines.len() - hidden;
    let plain = theme.capabilities().color == crate::tui::terminal::ColorDepth::None;
    let mut rows = document
        .lines
        .into_iter()
        .take(shown)
        .enumerate()
        .map(|(index, line)| {
            let prefix = if index == 0 { &prefix } else { &continuation };
            let text = if plain { line.plain } else { line.styled };
            fit_line(&format!("{prefix}{text}"), width)
        })
        .collect::<Vec<_>>();
    if hidden > 0 {
        let ellipsis = if theme.unicode() { "…" } else { "..." };
        let unit = if hidden == 1 { "row" } else { "rows" };
        let separator = if theme.unicode() { " · " } else { " - " };
        let long = format!("{ellipsis} {hidden} script {unit} hidden{separator}Ctrl+O");
        let short = format!("{ellipsis}+{hidden} Ctrl+O");
        let available = usize::from(width).saturating_sub(indent);
        let hint = if visible_width(&long) <= available {
            long
        } else {
            short
        };
        rows.push(fit_line(
            &format!("{continuation}{}", subdued_text(theme, &hint)),
            width,
        ));
    }
    rows
}

/// A bounded literal preview; long single-line JSON cannot flood the native
/// surface. This is UI elision, not a claim that captured bytes were discarded.
pub(super) fn output_text(panel: &ToolPanel, expanded: bool) -> String {
    let output = sanitize_for_terminal(&panel.output);
    if expanded {
        return output;
    }
    let mut end = 0;
    let mut lines = 1;
    for (count, (offset, ch)) in output.char_indices().enumerate() {
        if count == COMPACT_OUTPUT_CHARS || (ch == '\n' && lines == COMPACT_OUTPUT_LINES) {
            break;
        }
        lines += usize::from(ch == '\n');
        end = offset + ch.len_utf8();
    }
    if end == output.len() {
        output
    } else {
        format!(
            "{}\n(output preview; Ctrl+O for full output)",
            &output[..end]
        )
    }
}

/// Script output is literal data, not Markdown or a guessed diff.
pub(super) fn render_output(
    panel: &ToolPanel,
    theme: &OctetTheme,
    width: u16,
    expanded: bool,
) -> Vec<String> {
    output_text(panel, expanded)
        .lines()
        .flat_map(|line| wrap_hanging(&theme.fg("tool_output", line), "", "", width))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation::summarize_tool;
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    use crate::tui::theme::{test_theme, test_theme_with};
    use crate::tui::view::{
        transcript_render::render_block, transcript_selection::block_copy_text, TranscriptBlock,
    };
    use octet_ai::ToolCallId;
    use sexy_tui_rs::strip_terminal_sequences;

    fn panel(code: &str, output: &str, finished: bool, failed: bool) -> ToolPanel {
        let args = serde_json::json!({"code":code});
        ToolPanel::new(
            ToolCallId("codemode-fixture".into()),
            "codemode".into(),
            args.to_string(),
            summarize_tool("codemode", &args),
            output.into(),
            finished,
            failed,
            failed.then(|| "SyntaxError: unexpected token".into()),
            None,
        )
    }

    #[test]
    fn codemode_source_is_explicit_sanitized_and_not_a_shell_summary() {
        let mut p = panel("const x = '雪';\n\nreturn x;\x1b[3J", "", false, false);
        assert_eq!(source(&p).unwrap(), "const x = '雪';\n\nreturn x;");
        assert_eq!(p.display.value.as_deref(), Some("JavaScript"));
        assert!(p.display.shell_command.is_none());
        p.name = "another_extension".into();
        assert!(source(&p).is_none());
        p.name = "codemode".into();
        for args in ["{", "{\"code\":42}", "{\"secret\":\"not a script\"}"] {
            p.args = args.into();
            assert!(source(&p).is_none());
        }
    }

    #[test]
    fn codemode_output_preview_bounds_lines_unicode_and_single_line_json() {
        for output in ["one\ntwo\nthree\nfour\nfive\nsix".into(), "雪".repeat(2000)] {
            let p = panel("return 1", &output, true, false);
            let preview = output_text(&p, false);
            assert!(preview.contains("Ctrl+O for full output"));
            assert!(preview.chars().count() < 700);
            assert_eq!(output_text(&p, true), output);
            assert_eq!(p.output, output);
        }
        let p = panel("return 1", "one\n\n    two", true, false);
        assert_eq!(output_text(&p, false), "one\n\n    two");
        assert!(render_output(&p, &test_theme(), 80, false)
            .iter()
            .any(|row| row.is_empty()));
    }

    #[test]
    fn codemode_ansi_preview_and_expansion_keep_source_output_and_copy() {
        let code = "const x = '雪🦀';\n\n  const y = 2;\nreturn x;\n// final source";
        let output = "Script completed\nOutput:\nfirst\nsecond\nthird\nfinal output";
        for theme in [
            test_theme(),
            test_theme_with(TerminalCapabilities::test(true, true, ColorDepth::None)),
            test_theme_with(TerminalCapabilities::test(false, false, ColorDepth::Ansi16)),
        ] {
            for width in [18, 46, 80, 120] {
                for (finished, failed) in [(false, false), (true, false), (true, true)] {
                    let block =
                        TranscriptBlock::Tool(Box::new(panel(code, output, finished, failed)));
                    let renderer = theme.rich_renderer();
                    let render = |expanded| {
                        render_block(None, &block, &theme, &renderer, &renderer, width, expanded)
                    };
                    let compact = render(false);
                    let expanded = render(true);
                    for rows in [&compact, &expanded] {
                        assert!(rows
                            .iter()
                            .all(|row| visible_width(row) <= usize::from(width)));
                        if theme.capabilities().color == ColorDepth::None {
                            assert!(rows.iter().all(|row| !row.contains('\x1b')));
                        }
                    }
                    let compact = strip_terminal_sequences(&compact.join("\n"));
                    let expanded = strip_terminal_sequences(&expanded.join("\n"));
                    assert!(compact.contains("Codemode"));
                    if width >= 46 {
                        assert!(compact.contains("Script completed"));
                        assert!(!compact.contains("final source"));
                        assert!(expanded.contains("final source"));
                        assert!(expanded.contains("final output"));
                        if failed {
                            assert!(compact.contains("SyntaxError"));
                        }
                    }
                    let copied = block_copy_text(&block);
                    assert!(copied.contains(code));
                    assert!(!copied.contains("final output"));
                    assert!(!copied.contains("Ctrl+O"));
                }
            }
        }
    }
}
