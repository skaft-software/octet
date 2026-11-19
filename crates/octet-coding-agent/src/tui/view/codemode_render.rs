//! Literal JavaScript source and bounded output for Codemode tool calls.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use sexy_tui_rs::{truncate_to_width, visible_width, RichRenderer};

use super::terminal_text::sanitize_for_terminal;
use super::tool_render::bounded_tool_failure_reason;
use super::tool_render::{tool_value_indent_width, ToolRows};
use super::{fit_line, subdued_text, wrap_hanging, ToolPanel};
use crate::tui::theme::OctetTheme;
use crate::tui::theme_schema::ToolCardLayout;

const PI_SOURCE_ROWS: usize = 10;
const PI_CALL_ROWS: usize = 8;

#[derive(Debug, PartialEq, Eq)]
struct CodemodeCall {
    name: String,
    args: String,
    status: CallStatus,
    duration_ms: Option<u64>,
    error: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum CallStatus {
    Running,
    Ok,
    Error,
    Cancelled,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CodemodeDetails {
    calls: Vec<CodemodeCall>,
    omitted: u64,
    full_output_path: Option<String>,
}

/// The native extension publishes these fields in bounded result metadata.
/// Unknown/legacy results still have the ordinary output projection.
pub(crate) fn details(metadata: &Value) -> Option<Arc<CodemodeDetails>> {
    let calls = metadata.get("calls")?.as_array()?;
    let string = |value: &Value| value.as_str().map(sanitize_for_terminal);
    let details = CodemodeDetails {
        calls: calls
            .iter()
            .filter_map(|row| {
                let status = match row.get("status")?.as_str()? {
                    "running" => CallStatus::Running,
                    "ok" => CallStatus::Ok,
                    "error" => CallStatus::Error,
                    "cancelled" => CallStatus::Cancelled,
                    _ => return None,
                };
                Some(CodemodeCall {
                    name: string(&row["name"])?,
                    args: string(&row["args"]).unwrap_or_default(),
                    duration_ms: row["duration_ms"].as_u64(),
                    error: string(&row["error"]),
                    status,
                })
            })
            .collect(),
        omitted: metadata["calls_omitted"].as_u64().unwrap_or(0),
        full_output_path: string(&metadata["full_output_path"]),
    };
    Some(Arc::new(details))
}

fn pi_calls(panel: &ToolPanel, theme: &OctetTheme, width: u16, expanded: bool) -> ToolRows {
    let mut rows = ToolRows::default();
    let Some(details) = &panel.codemode_details else {
        return rows;
    };
    if details.calls.is_empty() && details.omitted == 0 {
        return rows;
    }
    rows.decoration(String::new());
    let first = if expanded {
        0
    } else {
        details.calls.len().saturating_sub(PI_CALL_ROWS)
    };
    if first > 0 {
        rows.decoration(fit_line(
            &subdued_text(
                theme,
                &format!("... ({first} earlier calls, ctrl+o to expand)"),
            ),
            width,
        ));
    }
    for call in &details.calls[first..] {
        let (icon, ascii, role) = match call.status {
            CallStatus::Running => ("…", ".", "warning"),
            CallStatus::Ok => ("✓", "+", "success"),
            CallStatus::Error => ("✗", "x", "error"),
            CallStatus::Cancelled => ("⊘", "-", "muted"),
        };
        let mut line = format!(
            "{} {}",
            theme.fg(role, if theme.unicode() { icon } else { ascii }),
            theme.fg("tool_title", &call.name)
        );
        if !call.args.is_empty() {
            let args = if expanded {
                call.args.clone()
            } else {
                truncate_to_width(&call.args, 80, Some("..."))
            };
            line.push_str(&format!(" {}", subdued_text(theme, &args)));
        }
        if let Some(ms) = call.duration_ms {
            let duration = crate::presentation::format_duration(Duration::from_millis(ms));
            line.push_str(&format!(" {}", theme.dim(&duration)));
        }
        for row in wrap_hanging(&line, "", "", width) {
            rows.decoration(row);
        }
        if expanded {
            if let Some(error) = &call.error {
                for line in error.lines() {
                    for row in wrap_hanging(&theme.fg("error", line), "  ", "  ", width) {
                        rows.decoration(row);
                    }
                }
            }
        }
    }
    if details.omitted > 0 {
        rows.decoration(fit_line(
            &subdued_text(
                theme,
                &format!("... ({} calls not retained)", details.omitted),
            ),
            width,
        ));
    }
    rows
}

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

pub(super) fn project_source(
    source: &str,
    renderer: &RichRenderer,
    theme: &OctetTheme,
    width: u16,
    expanded: bool,
    content_role: &str,
) -> ToolRows {
    if theme.layout().tool_cards == ToolCardLayout::Pi {
        return project_pi_source(source, renderer, theme, width, expanded, content_role);
    }
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
    let source_text = document.copy_text;
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
    let mut projected = ToolRows::rich_literal(
        std::mem::take(&mut rows),
        source_text,
        indent as u16,
        content_width,
    );
    projected.retain_rows(&(0..projected.lines.len()).collect::<Vec<_>>());
    projected.prefix("Codemode\n");
    if hidden > 0 {
        let available = usize::from(width).saturating_sub(indent);
        let hint = super::tool_render::tool_omission_hint(theme, hidden, "script", available);
        projected.decoration(fit_line(
            &format!("{continuation}{}", subdued_text(theme, &hint)),
            width,
        ));
    }
    projected
}

fn project_pi_source(
    source: &str,
    renderer: &RichRenderer,
    theme: &OctetTheme,
    width: u16,
    expanded: bool,
    content_role: &str,
) -> ToolRows {
    let title = "codemode";
    let painted = theme.apply_transcript_role_on_surface("tool_header", content_role, title);
    let mut result = ToolRows::literal(vec![fit_line(&painted, width)], title.into(), 0);
    if !source.trim_end().is_empty() {
        let doc = renderer.render_inline_syntax_wrapped(source.trim_end(), "javascript", width);
        let shown = if expanded {
            doc.lines.len()
        } else {
            doc.lines.len().min(PI_SOURCE_ROWS)
        };
        let hidden = doc.lines.len() - shown;
        let plain = theme.capabilities().color == crate::tui::terminal::ColorDepth::None;
        let rows = doc
            .lines
            .into_iter()
            .take(shown)
            .map(|line| if plain { line.plain } else { line.styled })
            .collect();
        let mut projected = ToolRows::rich_literal(rows, doc.copy_text, 0, width);
        projected.retain_rows(&(0..shown).collect::<Vec<_>>());
        result.append(projected);
        if hidden > 0 {
            result.decoration(super::tool_render::pi_more_lines(
                theme, hidden, false, width,
            ));
        }
    }
    result
}

/// Byte length of the leading blank lines plus a first non-empty line that only
/// repeats the failure status the card already shows. The status compared is
/// the displayed one (`bounded_tool_failure_reason`: sanitized, first non-empty
/// line, bounded), and the output line is bounded the same way, so leading blank
/// lines, sanitized text and over-long statuses still dedupe. The match is exact
/// and leading: output that merely mentions a status stays intact.
fn repeated_failure_status_len(panel: &ToolPanel, sanitized: &str) -> usize {
    let Some(reason) = bounded_tool_failure_reason(panel) else {
        return 0;
    };
    let mut offset = 0;
    for line in sanitized.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            offset += line.len();
            continue;
        }
        if crate::presentation::concise_line(trimmed) == reason {
            return offset + line.len();
        }
        return 0;
    }
    0
}

/// A bounded literal preview; long single-line JSON cannot flood the native
/// surface. This is UI elision, not a claim that captured bytes were discarded.
/// One owned copy per call: the sanitized text is trimmed in place.
fn output_preview(panel: &ToolPanel, expanded: bool) -> (String, bool) {
    let mut output = sanitize_for_terminal(&panel.output);
    let skip = repeated_failure_status_len(panel, &output);
    if skip > 0 {
        output.drain(..skip);
    }
    if expanded {
        return (output, false);
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
    let hidden = end < output.len();
    output.truncate(end);
    (output, hidden)
}

pub(super) fn project_output(
    panel: &ToolPanel,
    theme: &OctetTheme,
    width: u16,
    expanded: bool,
) -> ToolRows {
    if theme.layout().tool_cards == ToolCardLayout::Pi {
        return project_pi_output(panel, theme, width, expanded);
    }
    // Native cards use the same wrapped-row window as shell cards. The bounded
    // source preview below remains the semantic Tern projection, not ANSI layout.
    let (text, _) = output_preview(panel, true);
    let rows = text
        .lines()
        .filter(|line| expanded || !line.trim().is_empty())
        .flat_map(|line| wrap_hanging(&theme.fg("tool_output", line), "", "", width))
        .collect();
    let mut output = ToolRows::literal(rows, text, 0);
    output.retain_rows(&(0..output.lines.len()).collect::<Vec<_>>());
    if !expanded && output.lines.len() > super::COMPACT_EXEC_OUTPUT_ROWS {
        let (head, tail_start) = super::output_window::head_tail_window(
            output.lines.len(),
            super::COMPACT_EXEC_OUTPUT_ROWS,
        );
        let hidden = tail_start - head;
        let retained = (0..head)
            .chain(tail_start..output.lines.len())
            .collect::<Vec<_>>();
        output.retain_rows(&retained);
        output.lines.insert(
            head,
            subdued_text(
                theme,
                &super::tool_render::tool_omission_hint(
                    theme,
                    hidden,
                    "output",
                    usize::from(width),
                ),
            ),
        );
        output.copy_rows.insert(head, None);
    }
    output
}

fn project_pi_output(
    panel: &ToolPanel,
    theme: &OctetTheme,
    width: u16,
    expanded: bool,
) -> ToolRows {
    let mut result = pi_calls(panel, theme, width, expanded);
    if !panel.finished {
        return result;
    }
    let text = sanitize_for_terminal(&panel.output);
    let mut lines = text.splitn(4, '\n');
    let header = matches!(lines.next(), Some("Script completed" | "Script failed"))
        && lines.next().is_some_and(|line| {
            line.strip_prefix("Wall time ")
                .and_then(|time| time.strip_suffix(" seconds"))
                .is_some_and(|time| {
                    !time.is_empty()
                        && time
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || byte == b'.')
                })
        })
        && lines.next() == Some("Output:");
    let output = if header {
        lines.next().unwrap_or_default()
    } else {
        &text[repeated_failure_status_len(panel, &text)..]
    }
    .trim();
    let role = if panel.is_error {
        "error"
    } else {
        "tool_output"
    };
    if !output.is_empty() {
        let rows = output
            .lines()
            .flat_map(|line| wrap_hanging(&theme.fg(role, line), "", "", width))
            .collect();
        let mut projected = ToolRows::literal(rows, output.into(), 0);
        let hidden = projected.lines.len().saturating_sub(COMPACT_OUTPUT_LINES);
        result.decoration(String::new());
        if !expanded && hidden > 0 {
            projected.retain_rows(&(0..COMPACT_OUTPUT_LINES).collect::<Vec<_>>());
            projected.decoration(super::tool_render::pi_more_lines(
                theme, hidden, false, width,
            ));
            if let Some(path) = panel
                .codemode_details
                .as_ref()
                .and_then(|details| details.full_output_path.as_deref())
            {
                projected.decoration(fit_line(
                    &subdued_text(theme, &format!("Full output: {path}")),
                    width,
                ));
            }
        }
        result.append(projected);
    }
    result
}

pub(super) fn output_text(panel: &ToolPanel, expanded: bool) -> String {
    let (mut text, hidden) = output_preview(panel, expanded);
    if hidden {
        text.push_str("\noutput preview (Ctrl+O to expand)");
    }
    text
}

#[cfg(test)]
fn render_output(panel: &ToolPanel, theme: &OctetTheme, width: u16, expanded: bool) -> Vec<String> {
    project_output(panel, theme, width, expanded).lines
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

    fn failed_panel(code: &str, output: &str, reason: &str) -> ToolPanel {
        let args = serde_json::json!({"code":code});
        ToolPanel::new(
            ToolCallId("codemode-failure".into()),
            "codemode".into(),
            args.to_string(),
            summarize_tool("codemode", &args),
            output.into(),
            true,
            true,
            Some(reason.into()),
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
            assert!(preview.contains("output preview (Ctrl+O to expand)"));
            assert!(preview.chars().count() < 700);
            assert_eq!(output_text(&p, true), output);
            assert_eq!(p.output, output);
        }
        let p = panel("return 1", "one\n\n    two", true, false);
        assert_eq!(output_text(&p, false), "one\n\n    two");
        assert!(render_output(&p, &test_theme(), 80, false)
            .iter()
            .all(|row| !row.is_empty()));
        assert!(render_output(&p, &test_theme(), 80, true)
            .iter()
            .any(|row| row.is_empty()));
    }

    #[test]
    fn codemode_failed_output_does_not_repeat_the_failure_status() {
        let output =
            "Script failed\nWall time 0.0 seconds\nOutput:\n\nScript error:\nTypeError: boom";
        for width in [46, 120] {
            for expanded in [false, true] {
                let block = TranscriptBlock::Tool(Box::new(failed_panel(
                    "throw new TypeError('boom')",
                    output,
                    "Script failed",
                )));
                let theme = test_theme();
                let renderer = theme.rich_renderer();
                let rows =
                    render_block(None, &block, &theme, &renderer, &renderer, width, expanded);
                let text = strip_terminal_sequences(&rows.join("\n"));
                assert_eq!(
                    text.matches("Script failed").count(),
                    1,
                    "width {width}, expanded {expanded}: {text}"
                );
                assert!(text.contains("Wall time 0.0 seconds"));
                assert!(text.contains("Script error:"));
                assert!(text.contains("TypeError: boom"));
            }
        }
    }

    #[test]
    fn codemode_output_that_only_resembles_the_failure_status_is_kept() {
        let output = "Script failed\nWall time 0.0 seconds\nOutput:\nmanual note";
        let p = failed_panel("return 1", output, "TypeError: boom");
        assert_eq!(output_text(&p, false), output);
        assert_eq!(output_text(&p, true), output);
        let only_status = failed_panel("return 1", "Script failed", "Script failed");
        assert_eq!(output_text(&only_status, false), "");
        assert_eq!(output_text(&only_status, true), "");
    }

    #[test]
    fn codemode_failed_preview_stays_bounded_after_dropping_the_duplicate() {
        let output = format!(
            "Script failed\nWall time 0.0 seconds\nOutput:\n{}",
            "雪".repeat(2000)
        );
        let p = failed_panel("return 1", &output, "Script failed");
        let preview = output_text(&p, false);
        assert!(!preview.starts_with("Script failed"));
        assert!(preview.contains("Wall time 0.0 seconds"));
        assert!(preview.contains("output preview (Ctrl+O to expand)"));
        assert!(preview.chars().count() < 700);
        assert_eq!(
            output_text(&p, true),
            output.replacen("Script failed\n", "", 1)
        );
    }

    #[test]
    fn codemode_success_output_keeps_its_status_header() {
        let output = "Script completed\nWall time 0.1 seconds\nOutput:\n42";
        let p = panel("return 42", output, true, false);
        assert_eq!(output_text(&p, false), output);
        assert_eq!(output_text(&p, true), output);
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

    #[test]
    fn codemode_failure_dedupe_uses_the_displayed_reason_and_first_nonblank_line() {
        // Leading blank lines do not hide the repeated status.
        let p = failed_panel(
            "return 1",
            "\n  \nScript failed\nWall time 0.0 seconds",
            "Script failed",
        );
        assert_eq!(output_text(&p, true), "Wall time 0.0 seconds");
        assert_eq!(output_text(&p, false), "Wall time 0.0 seconds");

        // The reason is shown sanitized; terminal controls in it still dedupe.
        let p = failed_panel(
            "return 1",
            "Script failed\nWall time 0.0 seconds",
            "Script\x1b[3J failed",
        );
        assert_eq!(output_text(&p, true), "Wall time 0.0 seconds");

        // A reason longer than the display bound is shown truncated; the
        // matching first output line is still the same status.
        let long = format!("Script failed: {}", "x".repeat(300));
        let p = failed_panel("return 1", &format!("{long}\nWall time 0.0 seconds"), &long);
        assert_eq!(output_text(&p, true), "Wall time 0.0 seconds");

        // A multi-line reason is shown as its first non-empty line.
        let p = failed_panel(
            "return 1",
            "Script failed\nWall time 0.0 seconds",
            "\nScript failed\nstack frame",
        );
        assert_eq!(output_text(&p, true), "Wall time 0.0 seconds");

        // Unrelated first lines are kept, blank lines included.
        let p = failed_panel("return 1", "\nmanual note\nScript failed", "Script failed");
        assert_eq!(output_text(&p, true), "\nmanual note\nScript failed");
    }
}
