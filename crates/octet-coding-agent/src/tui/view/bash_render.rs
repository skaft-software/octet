//! Bash tool-result projection and compact terminal rendering.

use sexy_tui_rs::{visible_width, RichRenderer};

use super::output_window::bounded_tail_rows;
use super::terminal_text::{normalize_carriage_return_progress, sanitize_for_terminal};
use super::tool_render::tool_value_indent_width;
use super::{fit_line, subdued_text, wrap_hanging, ToolPanel, COMPACT_EXEC_OUTPUT_ROWS};
use crate::tui::theme::OctetTheme;

/// Locate the final canonical `bash` result after any legacy live progress
/// bytes. New panels replace live output at completion, while hydrated sessions
/// may still contain the older concatenated representation.
fn final_bash_result(output: &str) -> &str {
    for (index, _) in output.rmatch_indices("exit=") {
        let candidate = &output[index..];
        let mut lines = candidate.lines();
        let header = lines.next().unwrap_or_default();
        if !header
            .split_whitespace()
            .any(|part| part.starts_with("duration=") && part.len() > "duration=".len())
        {
            continue;
        }
        let next = lines.next().unwrap_or_default().trim();
        if index == 0 || next == "(no output)" || is_bash_stream_header(next) {
            return candidate;
        }
    }
    output
}

fn is_bash_stream_header(line: &str) -> bool {
    ["stdout", "stderr"].into_iter().any(|stream| {
        let Some(detail) = line
            .strip_prefix(stream)
            .and_then(|line| line.strip_prefix(':'))
        else {
            return false;
        };
        let detail = detail.trim();
        detail.is_empty()
            || detail
                .strip_suffix(" lines")
                .is_some_and(|count| count.parse::<usize>().is_ok())
            || (detail.contains(" bytes, showing first ") && detail.contains(" and last "))
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BashCaptureTruncation {
    stream: &'static str,
    omitted_bytes: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CompactBashOutput {
    lines: Vec<String>,
    capture_truncations: Vec<BashCaptureTruncation>,
    panel_elided: bool,
}

fn bash_capture_footer(line: &str) -> Option<(&'static str, &str)> {
    ["stdout", "stderr"].into_iter().find_map(|stream| {
        line.strip_prefix("truncated_")
            .and_then(|line| line.strip_prefix(stream))
            .and_then(|line| line.strip_prefix('='))
            .map(|detail| (stream, detail))
    })
}

fn is_bash_complete_footer(line: &str) -> bool {
    ["stdout", "stderr"].into_iter().any(|stream| {
        line.strip_prefix("complete_")
            .and_then(|line| line.strip_prefix(stream))
            .is_some_and(|detail| detail == "=true")
    })
}

/// Project a bounded result into display-oriented output. Protocol envelope
/// lines are excluded; capture loss is retained separately because Ctrl+O can
/// reveal UI-tail omissions but cannot recover bytes discarded by the tool.
fn compact_bash_output(panel: &ToolPanel) -> CompactBashOutput {
    let normalized = normalize_carriage_return_progress(final_bash_result(&panel.output));
    let result = sanitize_for_terminal(&normalized);
    let mut capture_truncations = Vec::new();
    for line in result.lines().map(str::trim) {
        let Some((stream, detail)) = bash_capture_footer(line) else {
            continue;
        };
        if detail == "false" {
            continue;
        }
        let omitted_bytes = detail
            .split_whitespace()
            .find_map(|part| part.strip_prefix("omitted_bytes:"))
            .and_then(|count| count.parse::<usize>().ok());
        capture_truncations.push(BashCaptureTruncation {
            stream,
            omitted_bytes,
        });
    }

    let capture_was_truncated = !capture_truncations.is_empty();
    let failure_reason = panel.failure_reason.as_deref().map(str::trim);
    let mut content = Vec::new();
    let mut panel_elided = false;
    let mut protocol_error = false;
    let mut expect_stream_header = false;
    for (line_index, raw) in result.lines().enumerate() {
        let line = raw.trim_end();
        let trimmed = line.trim();
        if line_index == 0 && trimmed.starts_with("error ") {
            protocol_error = true;
            expect_stream_header = true;
            continue;
        }
        if trimmed.starts_with("exit=") && trimmed.contains("duration=") {
            expect_stream_header = true;
            continue;
        }
        if expect_stream_header && is_bash_stream_header(trimmed) {
            protocol_error = false;
            expect_stream_header = false;
            continue;
        }
        if bash_capture_footer(trimmed).is_some() || is_bash_complete_footer(trimmed) {
            expect_stream_header = true;
            continue;
        }
        if trimmed.is_empty()
            || trimmed == "(no output)"
            || (capture_was_truncated && trimmed == "...")
            || (content.is_empty() && failure_reason.is_some_and(|reason| reason == trimmed))
        {
            continue;
        }
        if trimmed == "… older tool output elided …" {
            panel_elided = true;
            continue;
        }
        content.push(line.to_owned());
        if !protocol_error {
            expect_stream_header = false;
        }
    }

    CompactBashOutput {
        lines: content,
        capture_truncations,
        panel_elided,
    }
}

/// Disclosure-sensitive Bash rows must cross native history only at a complete
/// semantic boundary. Width-dependent wrapping makes retained commands and output
/// conservatively sensitive, including a single long command with no output.
pub(super) fn bash_output_changes_when_expanded(panel: &ToolPanel) -> bool {
    if panel
        .display
        .shell_command
        .as_deref()
        .is_some_and(|command| !command.is_empty())
    {
        return true;
    }
    let compact = compact_bash_output(panel);
    compact.panel_elided || !compact.capture_truncations.is_empty() || !compact.lines.is_empty()
}

fn bash_content_gutter() -> usize {
    tool_value_indent_width("Bash")
}

fn capture_loss_details(compact: &CompactBashOutput) -> Vec<String> {
    let mut details = Vec::new();
    if compact.panel_elided {
        details.push("older live output was elided; unavailable to expand".to_owned());
    }
    for truncation in &compact.capture_truncations {
        let omitted = truncation
            .omitted_bytes
            .map_or_else(|| "some bytes".to_owned(), |bytes| format!("{bytes} bytes"));
        details.push(format!(
            "{} capture omitted {omitted}; unavailable to expand",
            truncation.stream
        ));
    }
    details
}

pub(super) fn render_compact_bash_output(
    panel: &ToolPanel,
    theme: &OctetTheme,
    width: u16,
    expanded: bool,
    output_indent: &str,
) -> Vec<String> {
    let compact = compact_bash_output(panel);
    let ellipsis = if theme.unicode() { "…" } else { "..." };
    let loss_details = capture_loss_details(&compact);
    let mut output_rows = Vec::new();
    for output_line in compact.lines {
        output_rows.extend(wrap_hanging(
            &subdued_text(theme, &output_line),
            output_indent,
            output_indent,
            width,
        ));
    }
    if output_rows.is_empty() {
        let placeholder = if panel.finished {
            "(no output)"
        } else {
            "(waiting for output)"
        };
        output_rows.extend(wrap_hanging(
            &subdued_text(theme, placeholder),
            output_indent,
            output_indent,
            width,
        ));
    }

    if expanded {
        let mut rows = Vec::new();
        for detail in loss_details {
            rows.extend(wrap_hanging(
                &subdued_text(theme, &format!("{ellipsis} ({detail})")),
                output_indent,
                output_indent,
                width,
            ));
        }
        rows.extend(output_rows);
        return rows;
    }

    let force_metadata = !loss_details.is_empty();
    bounded_tail_rows(
        output_rows,
        COMPACT_EXEC_OUTPUT_ROWS,
        force_metadata,
        move |hidden_rows| {
            let mut details = loss_details;
            if hidden_rows > 0 {
                let unit = if hidden_rows == 1 { "row" } else { "rows" };
                details.push(format!("{hidden_rows} earlier visual {unit} hidden"));
            }
            let detail = format!("{ellipsis} {}", details.join(" · "));
            fit_line(
                &format!("{output_indent}{}", subdued_text(theme, &detail)),
                width,
            )
        },
    )
}

/// Maximum command-content rows in terse mode, independent of the output tail.
const COMPACT_BASH_INPUT_ROWS: usize = 3;

pub(super) fn render_bash_row(
    command: &str,
    renderer: &RichRenderer,
    theme: &OctetTheme,
    width: u16,
    expanded: bool,
) -> Vec<String> {
    let action = "Bash";
    let action_gap = tool_value_indent_width(action).saturating_sub(visible_width(action));
    let prefix = format!(
        "{}{}",
        theme.bold(&theme.fg("foreground", action)),
        " ".repeat(action_gap)
    );
    let continuation = " ".repeat(bash_content_gutter());
    let content_width = width
        .saturating_sub(u16::try_from(visible_width(&prefix)).unwrap_or(u16::MAX))
        .max(1);
    // Count after terminal sanitization and literal syntax wrapping so newlines,
    // wide graphemes, tabs, and long single-line commands share one visual budget.
    // Only this display projection is shortened; the retained command is intact.
    let command = renderer.render_inline_syntax(command, "bash", content_width);
    let hidden_rows = if expanded {
        0
    } else {
        command.lines.len().saturating_sub(COMPACT_BASH_INPUT_ROWS)
    };
    let visible_rows = command.lines.len() - hidden_rows;
    let use_plain = theme.capabilities().color == crate::tui::terminal::ColorDepth::None;
    let mut rows: Vec<String> = command
        .lines
        .into_iter()
        .take(visible_rows)
        .enumerate()
        .map(|(index, line)| {
            let prefix = if index == 0 { &prefix } else { &continuation };
            let content = if use_plain { line.plain } else { line.styled };
            fit_line(&format!("{prefix}{content}"), width)
        })
        .collect();
    if hidden_rows > 0 {
        let ellipsis = if theme.unicode() { "…" } else { "..." };
        let unit = if hidden_rows == 1 { "line" } else { "lines" };
        let hint = format!("{ellipsis} {hidden_rows} more {unit} hidden (ctrl+o to expand)");
        // Keep the entire hint readable on narrow terminals without spending a
        // command-preview row on it or changing the nested output's own budget.
        rows.extend(wrap_hanging(
            &subdued_text(theme, &hint),
            &continuation,
            &continuation,
            width,
        ));
    }
    rows
}

#[cfg(test)]
mod tests {
    use octet_ai::ToolCallId;
    use sexy_tui_rs::strip_terminal_sequences;

    use super::*;
    use crate::presentation::summarize_tool;
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    use crate::tui::theme::{test_theme, test_theme_with};

    fn plain_rows(rows: &[String]) -> Vec<String> {
        rows.iter()
            .map(|row| strip_terminal_sequences(row))
            .collect()
    }

    fn assert_input_window(
        command: &str,
        theme: &OctetTheme,
        width: u16,
    ) -> (Vec<String>, Vec<String>) {
        let renderer = theme.rich_renderer();
        let collapsed = render_bash_row(command, &renderer, theme, width, false);
        let expanded = render_bash_row(command, &renderer, theme, width, true);
        let retained = expanded.len().min(COMPACT_BASH_INPUT_ROWS);
        assert_eq!(&collapsed[..retained], &expanded[..retained]);
        if expanded.len() <= COMPACT_BASH_INPUT_ROWS {
            assert_eq!(collapsed, expanded);
        } else {
            let hidden = expanded.len() - COMPACT_BASH_INPUT_ROWS;
            let unit = if hidden == 1 { "line" } else { "lines" };
            let ellipsis = if theme.unicode() { "…" } else { "..." };
            let hint = plain_rows(&collapsed[COMPACT_BASH_INPUT_ROWS..])
                .iter()
                .map(|row| row.trim())
                .collect::<Vec<_>>()
                .join(" ");
            assert_eq!(
                hint,
                format!("{ellipsis} {hidden} more {unit} hidden (ctrl+o to expand)"),
                "width {width}: {collapsed:?}"
            );
        }
        for rows in [&collapsed, &expanded] {
            assert!(rows
                .iter()
                .all(|row| visible_width(row) <= usize::from(width)));
            assert!(plain_rows(rows)
                .iter()
                .skip(1)
                .all(|row| row.starts_with(&" ".repeat(bash_content_gutter()))));
            if theme.capabilities().color == ColorDepth::None {
                assert!(rows.iter().all(|row| !row.contains('\x1b')));
            }
        }
        assert_eq!(
            collapsed,
            render_bash_row(command, &renderer, theme, width, false),
            "collapsing again must restore the same preview"
        );
        (collapsed, expanded)
    }

    fn panel(command: &str, output: &str) -> ToolPanel {
        let args = serde_json::json!({"command": command});
        ToolPanel::new(
            ToolCallId("bash-input-window".into()),
            "bash".into(),
            args.to_string(),
            summarize_tool("bash", &args),
            output.into(),
            true,
            false,
            None,
            None,
        )
    }

    #[test]
    fn bash_input_short_commands_do_not_reserve_a_hint_row() {
        let theme = test_theme();
        for command in ["", "printf one", "one\ntwo", "one\n\nthree", "one\ntwo\n"] {
            let (collapsed, expanded) = assert_input_window(command, &theme, 80);
            assert!(collapsed.len() <= COMPACT_BASH_INPUT_ROWS);
            assert_eq!(collapsed, expanded);
            assert!(strip_terminal_sequences(&collapsed[0]).starts_with("Bash  "));
        }
    }

    #[test]
    fn bash_input_multiline_preview_preserves_blank_lines_and_hidden_suffix() {
        let theme = test_theme();
        let command = "cat <<'EOF'\nfirst\n\n    fourth\nEOF\nprintf done";
        let (collapsed, expanded) = assert_input_window(command, &theme, 100);
        assert_eq!(expanded.len(), 6);
        assert_eq!(collapsed.len(), 4);
        assert_eq!(
            plain_rows(&collapsed)[..3],
            ["Bash  cat <<'EOF'", "      first", "      "]
        );
        let full_command = plain_rows(&expanded)
            .iter()
            .map(|row| &row[bash_content_gutter()..])
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(full_command, command);
        assert!(!collapsed.join("\n").contains("fourth"));
        let (singular, _) = assert_input_window("one\ntwo\nthree\nfour", &theme, 100);
        assert!(strip_terminal_sequences(&singular[3]).contains("1 more line hidden"));
    }

    #[test]
    fn bash_input_single_line_preview_counts_wrapping_and_reflows() {
        let theme = test_theme();
        let command = format!("printf {}", "x".repeat(250));
        for width in [18, 42, 80, 120] {
            let (collapsed, expanded) = assert_input_window(&command, &theme, width);
            let content_width = usize::from(width) - bash_content_gutter();
            assert_eq!(expanded.len(), command.len().div_ceil(content_width));
            assert_eq!(
                plain_rows(&expanded)
                    .iter()
                    .map(|row| &row[bash_content_gutter()..])
                    .collect::<String>(),
                command
            );
            if width == 120 {
                assert_eq!(collapsed, expanded, "the wide frame no longer hides input");
            } else {
                assert!(expanded.len() > COMPACT_BASH_INPUT_ROWS);
            }
        }
    }

    #[test]
    fn bash_input_unicode_wrapping_is_grapheme_safe_in_all_color_modes() {
        let command = format!("printf {}", "界e\u{301}👩‍💻".repeat(32));
        for color in [
            ColorDepth::None,
            ColorDepth::Ansi16,
            ColorDepth::Ansi256,
            ColorDepth::TrueColor,
        ] {
            for unicode in [false, true] {
                let theme = test_theme_with(TerminalCapabilities::test(true, unicode, color));
                for width in [18, 42, 80] {
                    let (_, expanded) = assert_input_window(&command, &theme, width);
                    let plain = plain_rows(&expanded);
                    let mut content = plain.iter().map(|row| &row[bash_content_gutter()..]);
                    assert_eq!(content.clone().collect::<String>(), command);
                    assert!(content.all(|row| !row.starts_with(['\u{301}', '\u{200d}'])));
                }
            }
        }
    }

    #[test]
    fn bash_input_sanitizes_controls_before_counting_with_ascii_fallbacks() {
        let command = concat!(
            "printf '\x1b[2J\x1b]52;c;payload\x07\x00'\r\n",
            "\tprintf '\u{009b}31m\u{202e}'\n",
            "printf third\nprintf fourth\nprintf fifth"
        );
        for unicode in [false, true] {
            for color in [ColorDepth::None, ColorDepth::TrueColor] {
                let theme = test_theme_with(TerminalCapabilities::test(true, unicode, color));
                for width in [18, 80] {
                    let (collapsed, expanded) = assert_input_window(command, &theme, width);
                    for row in plain_rows(&collapsed)
                        .into_iter()
                        .chain(plain_rows(&expanded))
                    {
                        assert!(!row.chars().any(char::is_control));
                        assert!(!row.contains('\u{202e}'));
                        if !unicode {
                            assert!(row.is_ascii(), "{row:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn bash_input_window_leaves_arguments_and_output_tail_independent() {
        let theme = test_theme();
        let command = "printf first\nprintf second\nprintf third\nprintf fourth\nprintf fifth";
        let output =
            "exit=0 duration=0.2s\nstdout: 8 lines\n1\n2\n3\n4\n5\n6\n7\n8\ncomplete_stdout=true";
        let panel = panel(command, output);
        let args_before = panel.args.clone();
        for expanded in [false, true, false] {
            let input = render_bash_row(
                panel.display.shell_command.as_deref().unwrap(),
                &theme.rich_renderer(),
                &theme,
                100,
                expanded,
            );
            assert_eq!(input.len(), if expanded { 5 } else { 4 });
            let tail = render_compact_bash_output(&panel, &theme, 100, false, "");
            assert_eq!(tail.len(), COMPACT_EXEC_OUTPUT_ROWS);
            assert_eq!(plain_rows(&tail)[1..], ["5", "6", "7", "8"]);
            assert!(strip_terminal_sequences(&tail[0]).contains("4 earlier visual rows hidden"));
            assert_eq!(
                render_compact_bash_output(&panel, &theme, 100, true, "").len(),
                8
            );
        }
        assert_eq!(panel.args, args_before);
        assert_eq!(panel.display.shell_command.as_deref(), Some(command));
        assert_eq!(panel.output, output);
    }

    #[test]
    fn bash_input_with_no_output_is_disclosure_sensitive_for_native_history() {
        for command in ["one\ntwo\nthree\nfour".to_owned(), "x".repeat(300)] {
            let panel = panel(&command, "exit=0 duration=0.1s\n(no output)");
            assert!(compact_bash_output(&panel).lines.is_empty());
            assert!(bash_output_changes_when_expanded(&panel));
        }
    }
}
