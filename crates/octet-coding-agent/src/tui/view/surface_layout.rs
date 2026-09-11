use sexy_tui_rs::visible_width;

use crate::tui::layout::PresentationLayout;
use crate::tui::layout::PRIMARY_TEXT_GUTTER;
use crate::tui::theme::{
    OctetTheme, ThemeSurfaceAlign, ThemeSurfaceChrome, ThemeSurfaceHeading, ThemeSurfaceWidth,
};

use super::transcript_cache::SurfaceGeometry;
use super::transcript_selection::block_copy_text;
use super::{collapsed_reasoning_lines, transcript_transition_rows, TranscriptBlock};

#[derive(Clone, Copy, Debug)]
pub(super) struct SurfacePlan<'a> {
    pub(super) kind: &'static str,
    pub(super) chrome: ThemeSurfaceChrome,
    pub(super) heading: ThemeSurfaceHeading,
    pub(super) label: Option<&'a str>,
    pub(super) padding: u16,
    pub(super) frame_left: u16,
    pub(super) frame_width: u16,
    pub(super) geometry: SurfaceGeometry,
}

fn transcript_surface_kind(block: &TranscriptBlock) -> &'static str {
    match block {
        TranscriptBlock::User { .. } => "user",
        TranscriptBlock::Assistant(_) => "assistant",
        TranscriptBlock::Reasoning(_) => "reasoning",
        TranscriptBlock::Tool(_) => "tool",
        TranscriptBlock::Shell(_) => "shell",
        TranscriptBlock::Outcome(_) => "outcome",
        TranscriptBlock::UpdateAvailable(_)
        | TranscriptBlock::Notice(_)
        | TranscriptBlock::NoticeStatus { .. } => "notice",
        TranscriptBlock::Compaction(_) => "compaction",
    }
}

/// Semantic events reserve the shared marker gutter used by prompts and the
/// composer. Their marker sits at the presentation inset and their primary
/// text begins two cells later; nested content then begins from that text
/// column instead of inventing another renderer-specific offset.
fn uses_event_marker_gutter(block: &TranscriptBlock) -> bool {
    matches!(
        block,
        TranscriptBlock::Assistant(_)
            | TranscriptBlock::Reasoning(_)
            | TranscriptBlock::Tool(_)
            | TranscriptBlock::Shell(_)
            | TranscriptBlock::UpdateAvailable(_)
            | TranscriptBlock::Notice(_)
            | TranscriptBlock::NoticeStatus { .. }
    )
}

pub(super) fn surface_roles(kind: &str) -> (&'static str, &'static str, &'static str) {
    match kind {
        "user" => ("surface.user", "surface.user.border", "surface.user.label"),
        "assistant" => (
            "surface.assistant",
            "surface.assistant.border",
            "surface.assistant.label",
        ),
        "reasoning" => (
            "surface.reasoning",
            "surface.reasoning.border",
            "surface.reasoning.label",
        ),
        "tool" => ("surface.tool", "surface.tool.border", "surface.tool.label"),
        "shell" => (
            "surface.shell",
            "surface.shell.border",
            "surface.shell.label",
        ),
        "outcome" => (
            "surface.outcome",
            "surface.outcome.border",
            "surface.outcome.label",
        ),
        "notice" => (
            "surface.notice",
            "surface.notice.border",
            "surface.notice.label",
        ),
        "compaction" => (
            "surface.compaction",
            "surface.compaction.border",
            "surface.compaction.label",
        ),
        _ => ("text", "border", "muted"),
    }
}

fn natural_surface_width(block: &TranscriptBlock, theme: &OctetTheme) -> u16 {
    let copy = match block {
        TranscriptBlock::Reasoning(reasoning) if !reasoning.reasoning_expanded => {
            collapsed_reasoning_lines(theme, reasoning).join("\n")
        }
        TranscriptBlock::Compaction(compaction) if !compaction.expanded => {
            format!("{} · (ctrl+o to view)", compaction.label)
        }
        _ => block_copy_text(block),
    };
    let natural = copy.lines().map(visible_width).max().unwrap_or(1);
    let inner_prefix = match block {
        TranscriptBlock::User { .. } => 2,
        TranscriptBlock::Tool(_) => 8,
        TranscriptBlock::UpdateAvailable(_)
        | TranscriptBlock::Notice(_)
        | TranscriptBlock::NoticeStatus { .. } => 0,
        TranscriptBlock::Shell(_) => visible_width(theme.glyph("shell")).saturating_add(1),
        TranscriptBlock::Compaction(_) => visible_width(theme.glyph("note")).saturating_add(1),
        TranscriptBlock::Assistant(_)
        | TranscriptBlock::Reasoning(_)
        | TranscriptBlock::Outcome(_) => 0,
    };
    u16::try_from(natural.saturating_add(inner_prefix)).unwrap_or(u16::MAX)
}

pub(super) fn compile_surface_plan<'a>(
    previous: Option<&TranscriptBlock>,
    block: &TranscriptBlock,
    theme: &'a OctetTheme,
    outer_width: u16,
) -> SurfacePlan<'a> {
    let layout = theme.layout_for_width(outer_width);
    let presentation = PresentationLayout::new(theme, outer_width);
    let kind = transcript_surface_kind(block);
    let resolved = theme.surface_for_width(kind, outer_width);
    let event_gutter =
        if uses_event_marker_gutter(block) && presentation.content_width > PRIMARY_TEXT_GUTTER {
            PRIMARY_TEXT_GUTTER
        } else {
            0
        };
    let inset = presentation.inset.saturating_add(event_gutter);
    let available = presentation
        .content_width
        .saturating_sub(event_gutter)
        .max(1);
    // Keep default tool headers and all nested output off the terminal edge.
    // Reserve this before wrapping rather than padding source text or moving
    // the shared left baseline. Custom surfaces retain their own geometry.
    let right_inset = if theme.is_compiled_default()
        && matches!(block, TranscriptBlock::Tool(_) | TranscriptBlock::Shell(_))
    {
        2.min(available.saturating_sub(1))
    } else {
        0
    };
    let available = available.saturating_sub(right_inset);
    let mut chrome = resolved.chrome;
    let mut heading = if resolved.label.is_some() {
        resolved.heading
    } else {
        ThemeSurfaceHeading::None
    };
    let mut padding = resolved.padding;

    let overhead_for = |chrome: ThemeSurfaceChrome, padding: u16| -> u16 {
        let horizontal_padding = padding.saturating_mul(2);
        match chrome {
            ThemeSurfaceChrome::Plain | ThemeSurfaceChrome::Band | ThemeSurfaceChrome::Rule => {
                horizontal_padding
            }
            ThemeSurfaceChrome::Rail => u16::try_from(visible_width(theme.glyph("rail")))
                .unwrap_or(u16::MAX)
                .saturating_add(1)
                .saturating_add(horizontal_padding),
            ThemeSurfaceChrome::Card => 2u16.saturating_add(horizontal_padding),
        }
    };
    let mut overhead = overhead_for(chrome, padding);
    if available <= overhead.saturating_add(3) {
        chrome = ThemeSurfaceChrome::Plain;
        heading = ThemeSurfaceHeading::None;
        padding = 0;
        overhead = 0;
    }

    let frame_limit = resolved
        .max_width
        .unwrap_or(available)
        .min(available)
        .max(1);
    let frame_width = match resolved.width {
        ThemeSurfaceWidth::Full => frame_limit,
        ThemeSurfaceWidth::Content => {
            let requested = natural_surface_width(block, theme).saturating_add(overhead);
            requested.max(frame_limit.min(12)).min(frame_limit)
        }
    };
    if frame_width <= overhead {
        chrome = ThemeSurfaceChrome::Plain;
        heading = ThemeSurfaceHeading::None;
        padding = 0;
        overhead = 0;
    }
    let frame_offset = match resolved.align {
        ThemeSurfaceAlign::Left => 0,
        ThemeSurfaceAlign::Center => available.saturating_sub(frame_width) / 2,
        ThemeSurfaceAlign::Right => available.saturating_sub(frame_width),
    };
    let frame_left = inset.saturating_add(frame_offset);
    let chrome_left = match chrome {
        ThemeSurfaceChrome::Rail => u16::try_from(visible_width(theme.glyph("rail")))
            .unwrap_or(u16::MAX)
            .saturating_add(1),
        ThemeSurfaceChrome::Card => 1,
        ThemeSurfaceChrome::Plain | ThemeSurfaceChrome::Band | ThemeSurfaceChrome::Rule => 0,
    };
    let content_left = frame_left
        .saturating_add(chrome_left)
        .saturating_add(padding);
    let content_width = frame_width.saturating_sub(overhead).max(1);
    let has_heading_row = chrome == ThemeSurfaceChrome::Card
        || chrome == ThemeSurfaceChrome::Rule
        || heading != ThemeSurfaceHeading::None;
    let has_bottom_row = chrome == ThemeSurfaceChrome::Card;
    let highlighted_user = theme.uses_model_lab_color()
        && matches!(
            block,
            TranscriptBlock::User {
                prompt_color: Some(_),
                ..
            }
        );
    // Model-coloured prompt cards retain their breathing row above and below
    // the content while sharing the global horizontal grid.
    let vertical_padding_rows = usize::from(
        highlighted_user
            || (kind == "user" && (layout.prompt_padding || chrome == ThemeSurfaceChrome::Card)),
    );
    let leading_rows = usize::from(has_heading_row) + vertical_padding_rows;
    let trailing_rows = usize::from(has_bottom_row) + vertical_padding_rows;
    SurfacePlan {
        kind,
        chrome,
        heading,
        label: resolved.label,
        padding,
        frame_left,
        frame_width,
        geometry: SurfaceGeometry {
            transition_rows: transcript_transition_rows(previous, layout.density),
            leading_rows,
            trailing_rows,
            content_left,
            content_width,
        },
    }
}

#[cfg(test)]
mod tests {
    use sexy_tui_rs::strip_terminal_sequences;

    use super::super::{render_block_planned, ShellOutput, ToolPanel};
    use super::*;
    use crate::presentation::summarize_tool;
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    use crate::tui::theme::{
        test_theme, test_theme_for, test_theme_from_source, TerminalBackground,
    };

    fn tool(
        name: &str,
        args: serde_json::Value,
        output: String,
        reason: Option<String>,
    ) -> TranscriptBlock {
        TranscriptBlock::Tool(Box::new(ToolPanel::new(
            octet_ai::ToolCallId("right-gutter".into()),
            name.into(),
            args.to_string(),
            summarize_tool(name, &args),
            output,
            true,
            reason.is_some(),
            reason,
            None,
        )))
    }

    fn shell(command: &str, output: &str) -> TranscriptBlock {
        TranscriptBlock::Shell(Box::new(ShellOutput {
            id: "right-gutter-shell".into(),
            command: command.into(),
            output: output.into(),
            exit_code: 0,
            running: false,
        }))
    }

    fn rendered_with_gutter(
        block: &TranscriptBlock,
        theme: &OctetTheme,
        width: u16,
        verbose: bool,
    ) -> Vec<String> {
        let plan = compile_surface_plan(None, block, theme, width);
        assert_eq!(plan.frame_left, 2);
        assert_eq!(plan.geometry.content_left, 2);
        assert_eq!(plan.frame_width, width - 4);
        assert_eq!(plan.geometry.content_width, width - 4);
        let renderer = theme.rich_renderer();
        let rendered = render_block_planned(
            None, block, theme, &renderer, &renderer, width, verbose, 0, 0,
        );
        assert_eq!(rendered.geometry, plan.geometry);
        let rows = rendered
            .lines
            .iter()
            .map(|row| strip_terminal_sequences(row))
            .collect::<Vec<_>>();
        assert!(!rows.is_empty());
        for row in &rows {
            assert!(
                visible_width(row) <= usize::from(width - 2),
                "width={width}: {row:?}"
            );
        }
        rows
    }

    #[test]
    fn default_tool_and_shell_gutter_wraps_headers_and_output_without_changing_source_or_copy() {
        let command = format!("printf '%s' {}", "界e\u{301}argument".repeat(35));
        let output = format!("{}\noutput-marker", "界e\u{301}output".repeat(45));
        let args = serde_json::json!({"command": command});
        let captured = format!("exit=0 duration=0.2s\nstdout:\n{output}\ncomplete_stdout=true");
        let bash = tool("bash", args.clone(), captured.clone(), None);
        let local_shell = shell(&command, &output);
        for background in [
            TerminalBackground::Unknown,
            TerminalBackground::Dark,
            TerminalBackground::Light,
        ] {
            for color in [ColorDepth::TrueColor, ColorDepth::None] {
                let theme =
                    test_theme_for(background, TerminalCapabilities::test(true, true, color));
                for width in [46, 80, 120] {
                    for verbose in [false, true] {
                        for block in [&bash, &local_shell] {
                            let copy = block_copy_text(block);
                            let rows = rendered_with_gutter(block, &theme, width, verbose);
                            let header = rows
                                .iter()
                                .find(|row| {
                                    row.contains(if matches!(block, TranscriptBlock::Tool(_)) {
                                        "Bash"
                                    } else {
                                        "$"
                                    })
                                })
                                .expect("header is visible");
                            assert!(header.starts_with("• "), "{header:?}");
                            assert!(
                                rows.iter().any(|row| row.contains("output-marker")),
                                "{rows:?}"
                            );
                            assert!(rows.iter().any(|row| row.starts_with("  └ ")), "{rows:?}");
                            assert_eq!(block_copy_text(block), copy);
                        }
                    }
                }
            }
        }
        let TranscriptBlock::Tool(panel) = &bash else {
            unreachable!()
        };
        assert_eq!(panel.args, args.to_string());
        assert_eq!(panel.output, captured);
        assert_eq!(
            panel.display.shell_command.as_deref(),
            Some(command.as_str())
        );
        assert_eq!(block_copy_text(&bash), format!("$ {command}"));
        let TranscriptBlock::Shell(panel) = &local_shell else {
            unreachable!()
        };
        assert_eq!(panel.command, command);
        assert_eq!(panel.output, output);
        assert_eq!(
            block_copy_text(&local_shell),
            format!("$ {command} [completed]")
        );
    }

    #[test]
    fn default_tool_gutter_covers_long_arguments_diffs_and_failure_reasons() {
        let theme = test_theme();
        let path = format!("/work/{}/source.rs", "長い-directory/".repeat(20));
        let diff = format!(
            "--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-old {}\n+new {}\n",
            "界x".repeat(90),
            "界y".repeat(90)
        );
        let edited = tool(
            "edit",
            serde_json::json!({"path": path}),
            diff.clone(),
            None,
        );
        let reason = format!("permission denied: {path}");
        let failed = tool(
            "read",
            serde_json::json!({"path": path}),
            "private output".into(),
            Some(reason.clone()),
        );
        for width in [46, 80, 120] {
            for verbose in [false, true] {
                let copy = block_copy_text(&edited);
                let rows = rendered_with_gutter(&edited, &theme, width, verbose);
                assert!(rows.iter().any(|row| row.contains("Edit")), "{rows:?}");
                assert!(rows.iter().any(|row| row.contains("-old")), "{rows:?}");
                assert!(rows.iter().any(|row| row.contains("+new")), "{rows:?}");
                assert_eq!(block_copy_text(&edited), copy);
                assert!(!copy.contains("-old"));
                let copy = block_copy_text(&failed);
                let rows = rendered_with_gutter(&failed, &theme, width, verbose);
                assert!(rows.iter().any(|row| row.contains("Read")), "{rows:?}");
                assert!(
                    rows.iter().any(|row| row.contains("permission denied:")),
                    "{rows:?}"
                );
                assert_eq!(block_copy_text(&failed), copy);
                assert!(copy.contains("permission denied:"));
                assert!(!copy.contains("private output"));
            }
        }
        let TranscriptBlock::Tool(panel) = &edited else {
            unreachable!()
        };
        assert_eq!(panel.output, diff);
        let TranscriptBlock::Tool(panel) = &failed else {
            unreachable!()
        };
        assert_eq!(panel.failure_reason.as_deref(), Some(reason.as_str()));
    }

    #[test]
    fn default_tool_gutter_reduces_safely_in_tiny_panes() {
        let theme = test_theme();
        let renderer = theme.rich_renderer();
        for block in [
            tool(
                "bash",
                serde_json::json!({"command": "echo 界e\u{301}"}),
                "界e\u{301}output".into(),
                None,
            ),
            shell("echo 界e\u{301}", "界e\u{301}output"),
        ] {
            for width in 0u16..=12 {
                let plan = compile_surface_plan(None, &block, &theme, width);
                let left = if width > 2 { 2 } else { 0 };
                let available = width.saturating_sub(left).max(1);
                let gutter = 2.min(available - 1);
                assert_eq!(plan.geometry.content_left, left);
                assert_eq!(plan.geometry.content_width, available - gutter);
                let rendered = render_block_planned(
                    None, &block, &theme, &renderer, &renderer, width, true, 0, 0,
                );
                assert_eq!(rendered.geometry, plan.geometry);
                for row in rendered.lines {
                    assert!(
                        visible_width(&row) <= usize::from(width),
                        "width={width}: {row:?}"
                    );
                    let plain = strip_terminal_sequences(&row);
                    assert!(
                        visible_width(plain.trim_end())
                            <= usize::from(width.saturating_sub(gutter)),
                        "width={width}: {plain:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn tool_gutter_does_not_shrink_other_surfaces_or_custom_theme_geometry() {
        let theme = test_theme();
        let custom = test_theme_from_source(
            r#"
            [surfaces.tool]
            chrome = "card"
            padding = 1
            width = "full"
            max_width = 30
            align = "right"
        "#,
        );
        let bash = tool(
            "bash",
            serde_json::json!({"command": "echo hello"}),
            "hello".into(),
            None,
        );
        let local_shell = shell("echo hello", &"x".repeat(300));
        let renderer = custom.rich_renderer();
        for width in [46, 80, 120] {
            let notice = TranscriptBlock::Notice("unchanged".into());
            let plan = compile_surface_plan(None, &notice, &theme, width);
            assert_eq!(plan.geometry.content_left, 2);
            assert_eq!(plan.geometry.content_width, width - 2);
            let prompt = TranscriptBlock::User {
                text: "unchanged".into(),
                model_lab: None,
                prompt_color: None,
                persisted: true,
            };
            assert_eq!(
                compile_surface_plan(None, &prompt, &theme, width).frame_width,
                width
            );
            let plan = compile_surface_plan(None, &bash, &custom, width);
            assert_eq!(plan.frame_width, 30);
            assert_eq!(plan.frame_left, width - 30);
            assert_eq!(
                plan.chrome,
                if width == 46 {
                    ThemeSurfaceChrome::Rail
                } else {
                    ThemeSurfaceChrome::Card
                }
            );
            assert_eq!(plan.padding, 1);
            assert_eq!(plan.geometry.content_width, 26);
            let plan = compile_surface_plan(None, &local_shell, &custom, width);
            assert_eq!(plan.geometry.content_left, 2);
            assert_eq!(plan.geometry.content_width, width - 2);
            let rendered = render_block_planned(
                None,
                &local_shell,
                &custom,
                &renderer,
                &renderer,
                width,
                true,
                0,
                0,
            );
            assert_eq!(rendered.geometry, plan.geometry);
            assert!(rendered
                .lines
                .iter()
                .any(|row| visible_width(row) == usize::from(width)));
        }
    }
}
