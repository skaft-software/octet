//! Semantic transcript-block dispatch into focused presenters and surface framing.

use sexy_tui_rs::{visible_width, Color, RichRenderer};

use super::assistant_block::AssistantBlock;
use super::bash_render::{render_bash_row, render_compact_bash_output};
use super::outcome_render::render_outcome;
use super::reasoning_render::render_reasoning_on_surface_with_rainbow;
use super::surface_frame::{
    decorate_surface_content_suffix, decorate_surface_with_frame, event_margin_marker_with_frame,
};
use super::surface_layout::{compile_surface_plan, surface_roles};
use super::terminal_text::sanitize_for_terminal;
use super::tool_render::{
    render_compact_tool_output, render_diff_only, render_tool_failure_reason, tool_diff,
    tool_display_label, tool_grid_label, tool_value_indent, tool_value_indent_width,
    without_redundant_tool_lead,
};
use super::transcript_cache::{RenderedTranscriptBlock, SurfaceGeometry};
use super::{
    activity_elbow, finish_transcript_block, fit_line, render_shell_output, render_user_prompt,
    subdued_text, wrap_hanging, TranscriptBlock, ACTIVITY_DETAIL_INDENT,
};
use crate::tui::theme::{OctetTheme, ThemeSurfaceChrome};

fn nest_tool_output(rows: Vec<String>, theme: &OctetTheme, width: u16) -> Vec<String> {
    let mut first_content_row = true;
    rows.into_iter()
        .map(|row| {
            if row.is_empty() {
                return row;
            }
            let prefix = if first_content_row {
                first_content_row = false;
                format!("{} ", subdued_text(theme, activity_elbow(theme)))
            } else {
                ACTIVITY_DETAIL_INDENT.to_owned()
            };
            fit_line(&format!("{prefix}{row}"), width)
        })
        .collect()
}

/// Connect a wrapped tool header to its nested output. The tool label, every
/// vertical stem cell, and the final elbow share one column; replacing one
/// leading continuation space preserves that header's command-value column.
fn render_progress_decoration(
    decoration: &octet_agent::ToolProgressDecoration,
    theme: &OctetTheme,
    width: u16,
) -> Vec<String> {
    let label = sanitize_for_terminal(decoration.label());
    let detail = decoration
        .detail()
        .map(sanitize_for_terminal)
        .filter(|detail| !detail.is_empty());
    let text = detail.map_or(label.clone(), |detail| format!("{label} · {detail}"));
    wrap_hanging(&theme.fg("muted", &text), "", "", width)
}

fn append_nested_tool_output(
    header: &mut Vec<String>,
    rows: Vec<String>,
    theme: &OctetTheme,
    width: u16,
) {
    if !rows.iter().any(|row| !row.is_empty()) {
        return;
    }

    let stem = subdued_text(theme, theme.glyph("vertical"));
    for continuation in header.iter_mut().skip(1) {
        if let Some(rest) = continuation.strip_prefix(' ') {
            *continuation = fit_line(&format!("{stem}{rest}"), width);
        }
    }
    header.extend(nest_tool_output(rows, theme, width));
}

pub(super) struct RenderedTranscriptBlockUpdate {
    pub(super) stable_rows: usize,
    pub(super) replacement: Vec<String>,
    pub(super) geometry: SurfaceGeometry,
}

/// Incrementally decorate a streaming assistant or expanded reasoning tail.
/// Stable Markdown rows and their outer surface frame remain in
/// `TranscriptCache`; only the mutable suffix plus trailing frame rows are rebuilt.
pub(super) fn render_assistant_update_planned(
    previous: Option<&TranscriptBlock>,
    block: &TranscriptBlock,
    theme: &OctetTheme,
    rich_renderer: &RichRenderer,
    reasoning_renderer: &RichRenderer,
    outer_width: u16,
    show_reasoning: bool,
) -> Option<RenderedTranscriptBlockUpdate> {
    let (assistant, renderer) = match block {
        TranscriptBlock::Assistant(assistant) => (assistant, rich_renderer),
        TranscriptBlock::Reasoning(reasoning)
            if (reasoning.reasoning_expanded || show_reasoning)
                && !(reasoning.text.is_empty() && !reasoning.show_reasoning_hint) =>
        {
            (reasoning, reasoning_renderer)
        }
        _ => return None,
    };
    let plan = compile_surface_plan(previous, block, theme, outer_width);
    let update = assistant.render_update(renderer, theme, plan.geometry.content_width)?;
    if update.stable_prefix == 0 {
        return None;
    }

    let stable_rows = plan
        .geometry
        .transition_rows
        .saturating_add(plan.geometry.leading_rows)
        .saturating_add(update.stable_prefix);
    let replacement =
        decorate_surface_content_suffix(update.replacement, &plan, theme, outer_width, None, false);
    Some(RenderedTranscriptBlockUpdate {
        stable_rows,
        replacement,
        geometry: plan.geometry,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_block_planned(
    previous: Option<&TranscriptBlock>,
    block: &TranscriptBlock,
    theme: &OctetTheme,
    rich_renderer: &RichRenderer,
    reasoning_renderer: &RichRenderer,
    outer_width: u16,
    verbose_tools: bool,
    spinner_frame: usize,
    status_shimmer_frame: usize,
) -> RenderedTranscriptBlock {
    render_block_planned_with_rainbow(
        previous,
        block,
        theme,
        rich_renderer,
        reasoning_renderer,
        outer_width,
        verbose_tools,
        spinner_frame,
        status_shimmer_frame,
        0,
        // Document/measurement projections have no live roster; only the
        // retained transcript cache passes the render-time liveness flag.
        false,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_block_planned_with_rainbow(
    previous: Option<&TranscriptBlock>,
    block: &TranscriptBlock,
    theme: &OctetTheme,
    rich_renderer: &RichRenderer,
    reasoning_renderer: &RichRenderer,
    outer_width: u16,
    verbose_tools: bool,
    spinner_frame: usize,
    status_shimmer_frame: usize,
    rainbow_strength: u16,
    subagents_running: bool,
) -> RenderedTranscriptBlock {
    let plan = compile_surface_plan(previous, block, theme, outer_width);
    let width = plan.geometry.content_width;
    let content_background = matches!(
        plan.chrome,
        ThemeSurfaceChrome::Card | ThemeSurfaceChrome::Band
    )
    .then(|| theme.semantic_style(surface_roles(plan.kind).0).background)
    .filter(|background| *background != Color::Default);
    let collapsed_reasoning = matches!(
        block,
        TranscriptBlock::Reasoning(reasoning)
            if !reasoning.reasoning_expanded && !verbose_tools
    );
    let lines = match block {
        TranscriptBlock::User {
            text,
            model_lab,
            prompt_color,
            ..
        } => render_user_prompt(
            text,
            model_lab,
            prompt_color
                .as_deref()
                .filter(|_| theme.uses_model_lab_color()),
            rich_renderer,
            theme,
            width,
        ),
        TranscriptBlock::Subagents(summary) => {
            let full = summary.label();
            let label = if visible_width(&full) <= usize::from(width) {
                full
            } else if usize::from(width) >= visible_width("Subagents · /subagents") {
                "Subagents · /subagents".to_owned()
            } else {
                "/subagents".to_owned()
            };
            let label = sanitize_for_terminal(&label);
            let role = if summary.active_count() > 0 {
                "foreground"
            } else {
                summary.settled_role()
            };
            let label = if let Some(rest) = label.strip_prefix("Subagents") {
                format!(
                    "{}{}",
                    theme.bold(&theme.fg(role, "Subagents")),
                    theme.fg(role, rest)
                )
            } else {
                theme.fg(role, &label)
            };
            let mut lines = vec![fit_line(&label, width)];
            let hidden = summary
                .active_count()
                .saturating_sub(summary.live_workers.len());
            for (index, worker) in summary.live_workers.iter().enumerate() {
                let last = index + 1 == summary.live_workers.len() && hidden == 0;
                let branch = if theme.unicode() {
                    if last {
                        "└─"
                    } else {
                        "├─"
                    }
                } else if last {
                    "`-"
                } else {
                    "|-"
                };
                let name = sanitize_for_terminal(&worker.name);
                let estimate = if worker.output_estimated { "~" } else { "" };
                let text = format!(
                    "{name} · ↑{} ↓{estimate}{}",
                    worker.input_tokens, worker.output_tokens
                );
                lines.push(fit_line(
                    &theme.fg("muted", &format!("  {branch} {text}")),
                    width,
                ));
            }
            if hidden > 0 && !summary.live_workers.is_empty() {
                let branch = if theme.unicode() { "└─" } else { "`-" };
                lines.push(fit_line(
                    &theme.fg("muted", &format!("  {branch} +{hidden} more")),
                    width,
                ));
            }
            finish_transcript_block(lines)
        }
        TranscriptBlock::Assistant(assistant) => finish_transcript_block(
            assistant.render_on_surface(rich_renderer, theme, width, content_background),
        ),
        TranscriptBlock::Reasoning(reasoning) => render_reasoning_on_surface_with_rainbow(
            reasoning,
            reasoning_renderer,
            theme,
            width,
            verbose_tools,
            content_background,
            status_shimmer_frame,
            rainbow_strength,
        ),
        TranscriptBlock::Tool(panel) => {
            let compact_bash = matches!(panel.name.as_str(), "bash" | "exec")
                && panel.display.shell_command.is_some();
            let mut lines = if let Some(command) = panel.display.shell_command.as_deref() {
                render_bash_row(command, rich_renderer, theme, width, verbose_tools)
            } else {
                let compact = width < 60;
                let summary = if !panel.finished {
                    if compact {
                        &panel.display.compact_active
                    } else {
                        &panel.display.active
                    }
                } else if panel.is_error {
                    if compact {
                        &panel.display.compact_failure
                    } else {
                        &panel.display.failure
                    }
                } else if compact {
                    &panel.display.compact_success
                } else {
                    &panel.display.success
                };
                let tool = tool_grid_label(&tool_display_label(&panel.name));
                let label = theme.bold(&theme.fg("foreground", &tool));
                let label_width = visible_width(&tool);
                let text = match panel.display.value.as_deref() {
                    Some(value) => sanitize_for_terminal(value),
                    None => {
                        without_redundant_tool_lead(&panel.name, &sanitize_for_terminal(summary))
                    }
                };
                let text = theme.fg("muted", &text);
                let gap = tool_value_indent_width(&tool).saturating_sub(label_width);
                let label_prefix = format!("{label}{}", " ".repeat(gap));
                let continuation = tool_value_indent(&tool);
                wrap_hanging(&text, &label_prefix, &continuation, width)
            };
            let nested_width = width.saturating_sub(2).max(1);
            let mut output_lines = Vec::new();

            if !panel.finished {
                if let Some(decoration) = panel.progress_decoration.as_ref() {
                    output_lines.extend(render_progress_decoration(
                        decoration,
                        theme,
                        nested_width,
                    ));
                }
            }
            if panel.finished && panel.is_error {
                output_lines.extend(render_tool_failure_reason(panel, theme, nested_width, ""));
            }

            match panel.name.as_str() {
                "bash" | "exec" if compact_bash => output_lines.extend(render_compact_bash_output(
                    panel,
                    theme,
                    nested_width,
                    verbose_tools,
                    "",
                )),
                "search" if !panel.is_error => output_lines.extend(render_compact_tool_output(
                    panel,
                    theme,
                    nested_width,
                    verbose_tools,
                    "",
                )),
                "edit" | "write" if !panel.is_error && tool_diff(panel).is_some() => output_lines
                    .extend(render_diff_only(
                        panel,
                        rich_renderer,
                        theme,
                        nested_width,
                        verbose_tools,
                        "",
                    )),
                _ => {}
            }
            // Image reservations are visual-only and deliberately remain out
            // of `panel.output`, selection, and plain/print projections.
            output_lines.extend(panel.image_rows(nested_width));
            append_nested_tool_output(&mut lines, output_lines, theme, width);
            finish_transcript_block(lines)
        }
        TranscriptBlock::Outcome(outcome) => {
            render_outcome(outcome, theme, width, subagents_running)
        }
        TranscriptBlock::UpdateAvailable(version) => finish_transcript_block(
            super::startup_update::render_update_notice(version, rich_renderer, theme, width),
        ),
        TranscriptBlock::Notice(text) => {
            let text = theme.fg("muted", &sanitize_for_terminal(text));
            finish_transcript_block(wrap_hanging(&text, "", "", width))
        }
        TranscriptBlock::NoticeStatus { text, .. } => {
            let text = theme.fg("muted", &sanitize_for_terminal(text));
            finish_transcript_block(wrap_hanging(&text, "", "", width))
        }
        TranscriptBlock::Compaction(compaction) => {
            let marker = theme.glyph("note");
            let prefix = format!("{} ", theme.fg("model_accent", marker));
            let continuation = " ".repeat(visible_width(&prefix));
            let expanded = compaction.expanded || verbose_tools;
            let action = if expanded {
                "ctrl+o to collapse"
            } else {
                "ctrl+o to view"
            };
            let label = format!("{} · ({action})", sanitize_for_terminal(&compaction.label));
            let mut lines = wrap_hanging(&label, &prefix, &continuation, width);
            if expanded {
                let summary = AssistantBlock::finalized(compaction.summary.clone());
                let summary_width = width.saturating_sub(2).max(1);
                lines.extend(
                    summary
                        .render_on_surface(rich_renderer, theme, summary_width, content_background)
                        .into_iter()
                        .map(|line| {
                            if line.is_empty() {
                                String::new()
                            } else {
                                fit_line(&format!("  {line}"), width)
                            }
                        }),
                );
            }
            finish_transcript_block(lines)
        }
        TranscriptBlock::Shell(shell) => {
            let marker = theme.glyph("shell");
            let prefix = format!("{} ", theme.bold(&theme.fg("model_accent", marker)));
            let status = if shell.running {
                theme.dim("…")
            } else if shell.exit_code == 0 {
                theme.dim("[ok]")
            } else {
                theme.fg("error", "[failed]")
            };
            let mut lines = vec![fit_line(
                &format!(
                    "{} {} {}",
                    prefix,
                    theme.dim(&sanitize_for_terminal(&shell.command)),
                    status,
                ),
                width,
            )];
            let nested_width = width.saturating_sub(2).max(1);
            let output = render_shell_output(shell, theme, nested_width, verbose_tools, "");
            append_nested_tool_output(&mut lines, output, theme, width);
            finish_transcript_block(lines)
        }
    };

    if lines.is_empty() {
        return RenderedTranscriptBlock {
            lines,
            geometry: SurfaceGeometry::default(),
        };
    }
    let prompt_color = match block {
        TranscriptBlock::User { prompt_color, .. } => prompt_color
            .as_deref()
            .filter(|_| theme.uses_model_lab_color()),
        _ => None,
    };
    let marker = event_margin_marker_with_frame(
        block,
        theme,
        spinner_frame,
        Some(status_shimmer_frame),
        rainbow_strength,
        collapsed_reasoning,
    );
    let lines = decorate_surface_with_frame(
        lines,
        &plan,
        theme,
        outer_width,
        prompt_color,
        collapsed_reasoning,
        marker,
    );
    RenderedTranscriptBlock {
        lines,
        geometry: plan.geometry,
    }
}

#[cfg(test)]
pub(super) fn render_block(
    previous: Option<&TranscriptBlock>,
    block: &TranscriptBlock,
    theme: &OctetTheme,
    rich_renderer: &RichRenderer,
    reasoning_renderer: &RichRenderer,
    outer_width: u16,
    verbose_tools: bool,
) -> Vec<String> {
    render_block_planned(
        previous,
        block,
        theme,
        rich_renderer,
        reasoning_renderer,
        outer_width,
        verbose_tools,
        0,
        0,
    )
    .lines
}
