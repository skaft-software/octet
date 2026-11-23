//! Bash tool-result projection and compact terminal rendering.

use sexy_tui_rs::{visible_width, RichRenderer};

use super::output_window::head_tail_window;
use super::terminal_text::{normalize_carriage_return_progress, sanitize_for_terminal};
use super::tool_render::{tool_input_preview, tool_value_indent_width, ToolRows};
use super::{fit_line, subdued_text, wrap_hanging, ToolPanel, COMPACT_EXEC_OUTPUT_ROWS};
use crate::tui::theme::OctetTheme;
use crate::tui::theme_schema::ToolCardLayout;

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
    spill_notices: Vec<&'static str>,
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
fn compact_bash_output(panel: &ToolPanel, preserve_blank: bool) -> CompactBashOutput {
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
    let framed_result = result.lines().next().is_some_and(|line| {
        (line.starts_with("exit=") && line.contains("duration=")) || line.starts_with("error ")
    });
    let mut capture_metadata = false;
    let mut spill_notices = Vec::new();
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
            capture_metadata = false;
            protocol_error = false;
            expect_stream_header = false;
            continue;
        }
        if bash_capture_footer(trimmed).is_some() || is_bash_complete_footer(trimmed) {
            capture_metadata = framed_result;
            expect_stream_header = true;
            continue;
        }
        // Spill locations are capture-envelope metadata, not evidence. Limit
        // this filtering to the footer region: a command may legitimately print
        // `full_output_path=...` or `spill_error=true` as ordinary output.
        if capture_metadata {
            if trimmed.starts_with("full_output_path=")
                || trimmed.starts_with("partial_output_path=")
            {
                continue;
            }
            let notice = if trimmed.starts_with("spill_expired=true") {
                Some("spill output expired; unavailable to expand")
            } else if trimmed.starts_with("spill_truncated=true") {
                Some("spill byte limit reached; remaining bytes unavailable to expand")
            } else if trimmed.starts_with("spill_error=true") {
                Some("spill capture failed; full output unavailable to expand")
            } else {
                None
            };
            if let Some(notice) = notice {
                spill_notices.push(notice);
                continue;
            }
            if !trimmed.is_empty() {
                capture_metadata = false;
            }
        }
        if trimmed.is_empty() {
            if preserve_blank && !capture_metadata && !content.is_empty() {
                content.push(String::new());
            }
            continue;
        }
        if trimmed == "(no output)"
            || (capture_was_truncated && trimmed == "...")
            || (!panel.finished && matches!(trimmed, "..." | "…"))
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

    while content.last().is_some_and(String::is_empty) {
        content.pop();
    }
    CompactBashOutput {
        lines: content,
        capture_truncations,
        panel_elided,
        spill_notices,
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
    let compact = compact_bash_output(panel, false);
    compact.panel_elided || !compact.capture_truncations.is_empty() || !compact.lines.is_empty()
}

fn bash_content_gutter(action: &str) -> usize {
    tool_value_indent_width(action)
}

fn capture_loss_details(compact: &CompactBashOutput) -> Vec<String> {
    let mut details: Vec<String> = compact
        .spill_notices
        .iter()
        .map(|notice| (*notice).to_owned())
        .collect();
    if compact.panel_elided {
        details.push("older live output was elided; unavailable to expand".to_owned());
    }
    for truncation in &compact.capture_truncations {
        let omitted = truncation.omitted_bytes.map_or_else(
            || "some bytes".to_owned(),
            |bytes| format!("{bytes} {}", super::plural(bytes, "byte", "bytes")),
        );
        details.push(format!(
            "{} capture omitted {omitted}; unavailable to expand",
            truncation.stream
        ));
    }
    details
}

pub(super) fn project_compact_bash_output(
    panel: &ToolPanel,
    theme: &OctetTheme,
    width: u16,
    expanded: bool,
    output_indent: &str,
) -> ToolRows {
    let pi = theme.layout().tool_cards == ToolCardLayout::Pi;
    let compact = compact_bash_output(panel, pi);
    let ellipsis = if theme.unicode() { "…" } else { "..." };
    let loss_details = capture_loss_details(&compact);
    let mut output_rows = Vec::new();
    for output_line in &compact.lines {
        if output_line.is_empty() {
            output_rows.push(String::new());
            continue;
        }
        let text = if pi || theme.is_compiled_default() {
            theme.fg("tool_output", output_line)
        } else {
            subdued_text(theme, output_line)
        };
        output_rows.extend(wrap_hanging(&text, output_indent, output_indent, width));
    }
    let mut projected = if output_rows.is_empty() {
        if pi {
            ToolRows::default()
        } else if !panel.finished {
            return ToolRows::default();
        } else {
            let placeholder = "(no output)";
            let placeholder_rows = wrap_hanging(
                &subdued_text(theme, placeholder),
                output_indent,
                output_indent,
                width,
            );
            let mut rows = ToolRows::default();
            for row in placeholder_rows {
                rows.decoration(row);
            }
            rows
        }
    } else {
        ToolRows::literal(
            output_rows,
            compact.lines.join("\n"),
            visible_width(output_indent) as u16,
        )
    };

    projected.retain_rows(&(0..projected.lines.len()).collect::<Vec<_>>());

    if pi {
        let mut rows = ToolRows::default();
        if !projected.lines.is_empty() || !loss_details.is_empty() {
            rows.decoration(String::new());
        }
        let hidden = projected
            .lines
            .len()
            .saturating_sub(COMPACT_EXEC_OUTPUT_ROWS);
        if !expanded && hidden > 0 {
            let retained = (hidden..projected.lines.len()).collect::<Vec<_>>();
            projected.retain_rows(&retained);
            rows.decoration(super::tool_render::pi_more_lines(
                theme, hidden, true, width,
            ));
        }
        rows.append(projected);
        for detail in loss_details {
            for row in wrap_hanging(&theme.fg("warning", &format!("[{detail}]")), "", "", width) {
                rows.decoration(row);
            }
        }
        return rows;
    }

    if expanded {
        let mut rows = ToolRows::default();
        for detail in loss_details {
            for row in wrap_hanging(
                &subdued_text(theme, &format!("{ellipsis} ({detail})")),
                output_indent,
                output_indent,
                width,
            ) {
                rows.decoration(row);
            }
        }
        rows.append(projected);
        return rows;
    }

    if loss_details.is_empty() && projected.lines.len() <= COMPACT_EXEC_OUTPUT_ROWS {
        return projected;
    }
    let (head, tail_start) = head_tail_window(projected.lines.len(), COMPACT_EXEC_OUTPUT_ROWS);
    let hidden_rows = tail_start - head;
    let retained = (0..head)
        .chain(tail_start..projected.lines.len())
        .collect::<Vec<_>>();
    projected.retain_rows(&retained);
    let available = usize::from(width).saturating_sub(visible_width(output_indent));
    let mut detail = if hidden_rows > 0 {
        super::tool_render::tool_omission_hint(theme, hidden_rows, "output", available)
    } else {
        ellipsis.to_owned()
    };
    if !loss_details.is_empty() {
        detail.push_str(super::semantic_separator(theme));
        detail.push_str(&loss_details.join(super::semantic_separator(theme)));
    }
    projected.lines.insert(
        head,
        fit_line(
            &format!("{output_indent}{}", subdued_text(theme, &detail)),
            width,
        ),
    );
    projected.copy_rows.insert(head, None);
    projected
}

#[cfg(test)]
pub(super) fn render_compact_bash_output(
    panel: &ToolPanel,
    theme: &OctetTheme,
    width: u16,
    expanded: bool,
    output_indent: &str,
) -> Vec<String> {
    project_compact_bash_output(panel, theme, width, expanded, output_indent).lines
}

pub(super) fn project_pi_bash_call(
    panel: &ToolPanel,
    command: &str,
    theme: &OctetTheme,
    width: u16,
    content_role: &str,
) -> ToolRows {
    let prompt = if panel.name == "powershell" {
        "PS>"
    } else {
        "$"
    };
    let command = format!("{prompt} {}", sanitize_for_terminal(command));
    let mut painted = theme.apply_transcript_role_on_surface("tool_header", content_role, &command);
    if let Ok(args) = serde_json::from_str::<serde_json::Value>(&panel.args) {
        let timeout = args["timeout"]
            .as_u64()
            .map(|s| format!("{s}s"))
            .or_else(|| args["timeout_ms"].as_u64().map(|ms| format!("{ms}ms")));
        if let Some(timeout) = timeout {
            painted.push_str(&subdued_text(theme, &format!(" (timeout {timeout})")));
        }
    }
    let command_rows = wrap_hanging(
        &theme.apply_transcript_role_on_surface("tool_header", content_role, &command),
        "",
        "",
        width,
    );
    let mut painted_rows = wrap_hanging(&painted, "", "", width);
    let mut result = ToolRows::literal(command_rows, command, 0);
    result.lines.clear();
    // Timeout text is decoration. It may wrap, but must not enter command copy.
    result.lines.append(&mut painted_rows);
    result.copy_rows.resize(result.lines.len(), None);
    result
}

/// A flat command card: a small muted tool label stacked above the command
/// it runs. Selected by `flat_tool_cards`.
pub(super) fn project_labeled_bash_call(
    panel: &ToolPanel,
    command: &str,
    renderer: &RichRenderer,
    theme: &OctetTheme,
    width: u16,
    expanded: bool,
) -> ToolRows {
    let mut rows = ToolRows::default();
    // The label is decoration and never enters copied command text.
    rows.decoration(fit_line(
        &theme.fg("muted", super::tool_render::shell_row_label(&panel.name)),
        width,
    ));
    let content_width = width.saturating_sub(2).max(1);
    let highlighted = renderer.render_inline_syntax_wrapped(command, "bash", content_width);
    let use_plain = theme.capabilities().color == crate::tui::terminal::ColorDepth::None;
    let source_text = highlighted.copy_text;
    let source_rows = highlighted.lines.len();
    let command_lines = highlighted
        .lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let content = if use_plain { line.plain } else { line.styled };
            if index == 0 {
                format!("$ {content}")
            } else {
                format!("  {content}")
            }
        })
        .collect();
    // Commands have their own visual-row budget, independent of output.
    // Disclosure reveals the rest; copy follows the presented command window.
    let mut command_lines =
        tool_input_preview(command_lines, "command", "  ", theme, width, expanded);
    let hint = (!expanded && source_rows > 12)
        .then(|| command_lines.pop())
        .flatten();
    let mut projected = ToolRows::rich_literal(command_lines, source_text, 2, content_width);
    projected.retain_rows(&(0..projected.lines.len()).collect::<Vec<_>>());
    // Copy keeps the `$ ` prompt prefix, like the inline bash card.
    projected.prefix("$ ");
    if let Some(hint) = hint {
        projected.decoration(hint);
    }
    rows.append(projected);
    rows
}

// The caller styles the label with the shared tool_header role on its surface.
// Pi: packages/coding-agent/src/core/tools/renderers/bash.ts:25 and read.ts:34
// use the same bold toolTitle token. Native paint owns these locally cached roles.
pub(super) fn project_bash_row(
    command: &str,
    action: &str,
    renderer: &RichRenderer,
    theme: &OctetTheme,
    width: u16,
    expanded: bool,
) -> ToolRows {
    let action_gap = tool_value_indent_width(action).saturating_sub(visible_width(action));
    let prefix = format!("{action}{}", " ".repeat(action_gap));
    let continuation = " ".repeat(bash_content_gutter(action));
    let content_width = width
        .saturating_sub(u16::try_from(visible_width(&prefix)).unwrap_or(u16::MAX))
        .max(1);
    // Commands have their own visual-row budget, independent of output.
    // Disclosure reveals the rest; copy follows the presented command window.
    let command = renderer.render_inline_syntax_wrapped(command, "bash", content_width);
    let use_plain = theme.capabilities().color == crate::tui::terminal::ColorDepth::None;
    let source_text = command.copy_text;
    let source_rows = command.lines.len();
    let rows = command
        .lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let prefix = if index == 0 { &prefix } else { &continuation };
            let content = if use_plain { line.plain } else { line.styled };
            fit_line(&format!("{prefix}{content}"), width)
        })
        .collect();
    let mut lines = tool_input_preview(rows, "command", &continuation, theme, width, expanded);
    let hint = (!expanded && source_rows > 12).then(|| lines.pop().unwrap());
    let mut projected = ToolRows::rich_literal(
        lines,
        source_text,
        visible_width(&prefix) as u16,
        content_width,
    );
    projected.retain_rows(&(0..projected.lines.len()).collect::<Vec<_>>());
    projected.prefix("$ ");
    if let Some(hint) = hint {
        projected.decoration(hint);
    }
    projected
}

#[cfg(test)]
fn render_bash_row(
    command: &str,
    action: &str,
    renderer: &RichRenderer,
    theme: &OctetTheme,
    width: u16,
    expanded: bool,
) -> Vec<String> {
    project_bash_row(
        command,
        &theme.apply_transcript_role_on_surface("tool_header", "surface.tool", action),
        renderer,
        theme,
        width,
        expanded,
    )
    .lines
}

#[cfg(test)]
mod tests {
    use octet_ai::ToolCallId;
    use sexy_tui_rs::strip_terminal_sequences;

    use super::*;
    use crate::presentation::summarize_tool;
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    use crate::tui::theme::{test_theme, test_theme_with};

    #[test]
    fn bash_duration_elbow_aligns_with_output() {
        let mut panel = panel(
            "printf 'one\\ntwo\\n'",
            "exit=0 duration=5.92s\nstdout: 2 lines\none\ntwo",
        );
        panel.duration = Some(std::time::Duration::from_millis(5_920));
        let mut theme = test_theme();
        theme.set_duration_preference(Some(true));
        for expanded in [false, true] {
            let rendered = super::super::transcript_render::render_tool(
                &panel,
                &theme,
                &theme.rich_renderer(),
                80,
                expanded,
                "tool_output",
            );
            let rows = plain_rows(&rendered.lines);
            assert_eq!(&rows[1..], ["└ one", "  two", "└ Took 5.92s"]);
            assert!(rendered.copy_rows.last().unwrap().is_none());
        }
    }

    #[test]
    fn flat_tool_cards_stack_a_muted_label_above_the_command() {
        let theme = crate::tui::theme::test_theme_from_source("[colors]\nflat_tool_cards = true");
        let mut panel = panel(
            "cargo test --lib",
            "exit=0 duration=1.2s\nstdout: 2 lines\nrunning 38 tests\ntest ok",
        );
        panel.duration = Some(std::time::Duration::from_millis(1_200));
        let rendered = super::super::transcript_render::render_tool(
            &panel,
            &theme,
            &theme.rich_renderer(),
            80,
            false,
            "tool_output",
        );
        let rows = plain_rows(&rendered.lines);
        assert_eq!(rows[0], "Bash");
        assert_eq!(rows[1], "$ cargo test --lib");
        assert!(
            rows.iter().any(|row| row.starts_with("  running 38 tests")),
            "output must indent without stems: {rows:?}"
        );
        assert!(
            !rows
                .iter()
                .any(|row| row.contains('└') || row.starts_with('│')),
            "flat cards have no connector bars: {rows:?}"
        );
        assert!(
            !rows.iter().any(|row| row.contains("Took")),
            "flat cards show no timing: {rows:?}"
        );
        assert!(
            rendered.text.starts_with("$ cargo test --lib"),
            "{:?}",
            rendered.text
        );
        assert!(!rendered.text.starts_with("Bash"), "{:?}", rendered.text);
    }

    #[test]
    fn tool_duration_is_hidden_by_default_without_hiding_output() {
        let mut panel = panel(
            "true",
            "exit=0 duration=99s\nstdout: 1 line\nfixture output",
        );
        panel.duration = Some(std::time::Duration::from_millis(1250));
        let theme = test_theme();
        let rows = super::super::transcript_render::render_tool(
            &panel,
            &theme,
            &theme.rich_renderer(),
            80,
            true,
            "tool_output",
        );
        let text = plain_rows(&rows.lines).join("\n");
        assert!(text.contains("fixture output"), "{text}");
        assert!(!text.contains("Took"), "{text}");
    }

    #[test]
    fn shell_duration_is_themeable_final_only_and_copy_safe() {
        let mut panel = panel("true", "exit=0 duration=99s\n(no output)");
        panel.duration = Some(std::time::Duration::from_millis(1250));
        let mut theme = test_theme();
        theme.set_duration_preference(Some(true));
        for expanded in [false, true] {
            for is_error in [false, true] {
                panel.is_error = is_error;
                let rows = super::super::transcript_render::render_tool(
                    &panel,
                    &theme,
                    &theme.rich_renderer(),
                    80,
                    expanded,
                    "tool_output",
                );
                assert!(plain_rows(&rows.lines).join("\n").contains("└ Took 1.25s"));
                assert!(!rows.text.contains("Took"));
                assert!(rows.copy_rows.last().unwrap().is_none());
            }
        }
        for (template, expected) in [
            ("finished in {duration}", "finished in 1.25s"),
            ("", ""),
            ("  ", ""),
        ] {
            let theme = crate::tui::theme::test_theme_from_source(&format!(
                "[tokens]\nshow_durations = true\ntool_duration = {template:?}\n"
            ));
            let rows = super::super::transcript_render::render_tool(
                &panel,
                &theme,
                &theme.rich_renderer(),
                80,
                false,
                "tool_output",
            );
            let text = plain_rows(&rows.lines).join("\n");
            assert!(!text.contains("Took"));
            if !expected.is_empty() {
                assert!(text.contains(expected), "{text}");
            } else {
                assert!(!text.contains("1.25s"), "{text}");
            }
        }
        for (finished, duration) in [(false, panel.duration), (true, None)] {
            panel.finished = finished;
            panel.duration = duration;
            let rows = super::super::transcript_render::render_tool(
                &panel,
                &theme,
                &theme.rich_renderer(),
                80,
                false,
                "tool_output",
            );
            assert!(!plain_rows(&rows.lines).join("\n").contains("Took"));
        }
    }

    #[test]
    fn bash_output_excludes_capture_paths_but_keeps_literal_output_and_loss_notices() {
        let output = "exit=0 duration=1ms\nstdout: 999 bytes, showing first 2 and last 1 lines\nfull_output_path=ordinary-output\nspill_error=true ordinary-output\nretained tail\ntruncated_stdout=head:2 tail:1 omitted_bytes:123\nfull_output_path=/private/capture.log\npartial_output_path=/private/partial.log\nspill_truncated=true (per-stream byte limit reached)\nspill_error=true (full output could not be retained)\nstderr: 999 bytes, showing first 1 and last 1 lines\nretained stderr\ntruncated_stderr=head:1 tail:1 omitted_bytes:456\nspill_expired=true (owner retention evicted this output)";
        let panel = ToolPanel::new(
            ToolCallId("projection".into()),
            "bash".into(),
            "{}".into(),
            summarize_tool("bash", &serde_json::json!({"command":"printf retained"})),
            output.into(),
            true,
            false,
            None,
            None,
        );
        let theme = test_theme();
        let text =
            plain_rows(&render_compact_bash_output(&panel, &theme, 160, true, "")).join("\n");
        assert!(text.contains("full_output_path=ordinary-output"));
        assert!(text.contains("spill_error=true ordinary-output"));
        assert!(text.contains("retained tail"));
        assert!(text.contains("retained stderr"));
        assert!(!text.contains("/private/"));
        assert!(!text.contains("spill_truncated=true"));
        assert!(text.contains("spill byte limit reached"));
        assert!(text.contains("spill capture failed"));
        assert!(text.contains("spill output expired"));
        assert!(text.contains("123 bytes"));
        assert!(text.contains("456 bytes"));
    }

    fn plain_rows(rows: &[String]) -> Vec<String> {
        rows.iter()
            .map(|row| strip_terminal_sequences(row))
            .collect()
    }

    fn assert_input_window(
        command: &str,
        action: &str,
        theme: &OctetTheme,
        width: u16,
    ) -> (Vec<String>, Vec<String>) {
        let renderer = theme.rich_renderer();
        let collapsed = render_bash_row(command, action, &renderer, theme, width, false);
        let expanded = render_bash_row(command, action, &renderer, theme, width, true);
        if expanded.len() <= 12 {
            assert_eq!(collapsed, expanded, "short commands must remain complete");
        } else {
            assert_eq!(collapsed.len(), 13, "twelve command rows plus one hint");
            assert_eq!(collapsed[..12], expanded[..12]);
            let hint = strip_terminal_sequences(&collapsed[12]);
            assert!(hint.contains(&format!("{}", expanded.len() - 12)), "{hint}");
            assert!(hint.contains("Ctrl+O"), "{hint}");
        }
        for rows in [&collapsed, &expanded] {
            assert!(rows
                .iter()
                .all(|row| visible_width(row) <= usize::from(width)));
            assert!(plain_rows(rows)
                .iter()
                .skip(1)
                .all(|row| row.starts_with(&" ".repeat(bash_content_gutter(action)))));
            if theme.capabilities().color == ColorDepth::None {
                assert!(rows.iter().all(|row| !row.contains('\x1b')));
            }
        }
        assert_eq!(
            collapsed,
            render_bash_row(command, action, &renderer, theme, width, false),
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
        for command in ["", "printf one", "one\ntwo"] {
            let (collapsed, expanded) = assert_input_window(command, "Bash", &theme, 80);
            assert!(collapsed.len() <= 2);
            assert_eq!(collapsed, expanded);
            assert!(strip_terminal_sequences(&collapsed[0]).starts_with("Bash  "));
        }
    }

    #[test]
    fn bash_input_multiline_preview_preserves_blank_lines_and_hidden_suffix() {
        let theme = test_theme();
        let command = "cat <<'EOF'\nfirst\n\n    fourth\nEOF\nprintf done";
        let (collapsed, expanded) = assert_input_window(command, "Bash", &theme, 100);
        assert_eq!(expanded.len(), 6);
        assert_eq!(collapsed.len(), 6);
        assert_eq!(
            plain_rows(&collapsed)[..2],
            ["Bash  cat <<'EOF'", "      first"]
        );
        let full_command = plain_rows(&expanded)
            .iter()
            .map(|row| &row[bash_content_gutter("Bash")..])
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(full_command, command);
        assert!(collapsed.join("\n").contains("fourth"));
        let (multiline, _) = assert_input_window("one\ntwo\nthree\nfour", "Bash", &theme, 100);
        assert_eq!(multiline.len(), 4);
        let longer = format!("{command}\nprintf seventh\nprintf eighth\nprintf ninth\nprintf tenth\nprintf eleventh\nprintf twelfth\nprintf thirteenth\nprintf fourteenth");
        let (collapsed, expanded) = assert_input_window(&longer, "Bash", &theme, 100);
        assert_eq!(expanded.len(), 14);
        assert!(collapsed[12].contains("2 command rows hidden"));
        assert!(!collapsed.join("\n").contains("thirteenth"));
        assert!(expanded.join("\n").contains("fourteenth"));
    }

    #[test]
    fn bash_input_single_line_preview_counts_wrapping_and_reflows() {
        let theme = test_theme();
        let command = format!("printf {}", "x".repeat(250));
        for width in [18, 42, 80, 120] {
            let (collapsed, expanded) = assert_input_window(&command, "Bash", &theme, width);
            let content_width = usize::from(width) - bash_content_gutter("Bash");
            assert!(expanded.len() >= command.len().div_ceil(content_width));
            assert_eq!(
                plain_rows(&expanded)
                    .iter()
                    .map(|row| &row[bash_content_gutter("Bash")..])
                    .collect::<String>(),
                command.replace(' ', "")
            );
            // Soft wrapping may drop boundary whitespace visually; semantic
            // copy still preserves the exact sanitized command.
            assert_eq!(
                theme
                    .rich_renderer()
                    .render_inline_syntax_wrapped(&command, "bash", content_width as u16)
                    .copy_text,
                command,
            );
            assert_eq!(
                collapsed.len(),
                if expanded.len() > 12 {
                    13
                } else {
                    expanded.len()
                }
            );
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
                    let (_, expanded) = assert_input_window(&command, "Bash", &theme, width);
                    let plain = plain_rows(&expanded);
                    let mut content = plain.iter().map(|row| &row[bash_content_gutter("Bash")..]);
                    assert_eq!(
                        content.clone().collect::<String>(),
                        command.replace(' ', "")
                    );
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
                    let (collapsed, expanded) = assert_input_window(command, "Bash", &theme, width);
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
        let command = "printf first\nprintf second\nprintf third\nprintf fourth\nprintf fifth\nprintf sixth\nprintf seventh\nprintf eighth\nprintf ninth\nprintf tenth\nprintf eleventh\nprintf twelfth\nprintf thirteenth\nprintf fourteenth";
        let output =
            "exit=0 duration=0.2s\nstdout: 8 lines\n1\n2\n3\n4\n5\n6\n7\n8\ncomplete_stdout=true";
        let panel = panel(command, output);
        let args_before = panel.args.clone();
        for expanded in [false, true, false] {
            let input = render_bash_row(
                panel.display.shell_command.as_deref().unwrap(),
                "Bash",
                &theme.rich_renderer(),
                &theme,
                100,
                expanded,
            );
            assert_eq!(input.len(), if expanded { 14 } else { 13 });
            let tail = render_compact_bash_output(&panel, &theme, 100, false, "");
            assert_eq!(tail.len(), COMPACT_EXEC_OUTPUT_ROWS);
            let plain = plain_rows(&tail);
            assert_eq!(&plain[..2], ["1", "2"]);
            assert_eq!(&plain[3..], ["7", "8"]);
            assert!(plain[2].contains("4 output rows hidden"));
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
            assert!(compact_bash_output(&panel, false).lines.is_empty());
            assert!(bash_output_changes_when_expanded(&panel));
        }
    }

    #[test]
    fn powershell_input_renders_full_command_under_its_own_label() {
        use super::super::tool_render::shell_row_label;

        let args = serde_json::json!({
            "command": "git grep -n -I \"public_ai_error_diagnostic|public_error_diagnostic\" | Select-String foo-bar-baz-qux",
        });
        let display = summarize_tool("powershell", &args);
        let command = display
            .shell_command
            .as_deref()
            .expect("powershell keeps the full command for the shell renderer");
        assert!(
            command.chars().count() > 72,
            "regression input must exceed the old generic cap"
        );
        assert_eq!(display.active, format!("running {command}"));
        assert!(!display.active.contains('…'));
        assert_eq!(display.plain_tag, "powershell");
        assert_eq!(shell_row_label("powershell"), "Powershell");
        assert_eq!(shell_row_label("exec"), "Bash");

        let theme = test_theme();
        let (collapsed, _) = assert_input_window(command, "Powershell", &theme, 80);
        let plain = plain_rows(&collapsed).join("\n");
        assert!(plain.starts_with("Powershell  "), "{plain:?}");
        for fragment in [
            "git",
            "public_ai_error_diagnostic",
            "Select-String",
            "foo-bar-baz-qux",
        ] {
            assert!(plain.contains(fragment), "missing {fragment}: {plain:?}");
        }
    }
}
