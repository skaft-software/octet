use sexy_tui_rs::visible_width;

use crate::tui::layout::PresentationLayout;
use crate::tui::layout::PRIMARY_TEXT_GUTTER;
use crate::tui::theme::{
    ModelLab, OctetTheme, ThemeSurfaceAlign, ThemeSurfaceChrome, ThemeSurfaceHeading,
    ThemeSurfaceWidth,
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
    pub(super) user_model_lab: Option<ModelLab>,
    pub(super) padding: u16,
    pub(super) frame_left: u16,
    pub(super) frame_width: u16,
    /// Columns a borderless filled surface paints to the left of its frame and
    /// to the right of it, so a theme that caps the reading column still shades
    /// the full terminal width instead of floating a narrow island. Both are
    /// zero for bordered cards, text-only surfaces, and uncapped themes, where
    /// the frame already reaches the terminal edges.
    pub(super) bleed_left: u16,
    pub(super) bleed_right: u16,
    pub(super) geometry: SurfaceGeometry,
}

impl SurfacePlan<'_> {
    /// Whether this surface paints a fill beyond its own frame.
    pub(super) fn bleeds(&self) -> bool {
        self.bleed_left > 0 || self.bleed_right > 0
    }
}

fn transcript_surface_kind(block: &TranscriptBlock) -> &'static str {
    match block {
        TranscriptBlock::User { .. } => "user",
        TranscriptBlock::Assistant(_) => "assistant",
        TranscriptBlock::Reasoning(_) => "reasoning",
        TranscriptBlock::Tool(_) | TranscriptBlock::Subagents(_) => "tool",
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
            | TranscriptBlock::Subagents(_)
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
        TranscriptBlock::Subagents(_) => 0,
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
    // File themes paint full-width user surfaces (bands/rails/cards) that
    // would otherwise start one gutter left of every marker-aligned block.
    // Reserve the shared gutter so prompt cards align edge to edge with
    // tool/shell cards; plain user text and the compiled default keep the
    // historical compact prompt grid.
    let full_width_user = matches!(block, TranscriptBlock::User { .. })
        && !theme.is_compiled_default()
        && matches!(
            resolved.chrome,
            ThemeSurfaceChrome::Card | ThemeSurfaceChrome::Band | ThemeSurfaceChrome::Rail
        );
    let flush_rails = theme.resolve::<bool>("transcript_flush").unwrap_or(false);
    // Still's soft user band occupies the same full column as its shaded
    // composer. Its prompt chevron is already inside the band; other themes
    // retain the historical event gutter for cards and rails.
    let still_user_band = full_width_user
        && resolved.chrome == ThemeSurfaceChrome::Band
        && theme
            .resolve::<bool>("quiet_tool_summaries")
            .unwrap_or(false);
    let needs_marker_gutter = uses_event_marker_gutter(block)
        || (full_width_user && !still_user_band)
        || (flush_rails && resolved.chrome != ThemeSurfaceChrome::Rail);
    let marker_gutter_width = theme
        .resolve::<u16>("event_marker_gutter")
        .unwrap_or(PRIMARY_TEXT_GUTTER)
        .max(PRIMARY_TEXT_GUTTER)
        .min(presentation.content_width.saturating_sub(1));
    let event_gutter = if flush_rails && resolved.chrome == ThemeSurfaceChrome::Rail {
        0
    } else if needs_marker_gutter && presentation.content_width > PRIMARY_TEXT_GUTTER {
        marker_gutter_width
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
        && matches!(
            block,
            TranscriptBlock::Tool(_) | TranscriptBlock::Subagents(_) | TranscriptBlock::Shell(_)
        ) {
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
                .saturating_add(padding.max(1))
                .saturating_add(padding),
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
            .saturating_add(u16::from(padding == 0)),
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
    // the content while sharing the global horizontal grid. Borderless
    // band/rail surfaces with an explicit padding opt into the same single
    // cushion row so shaded fills never touch their own edges.
    let vertical_padding_rows = usize::from(
        highlighted_user
            || (kind == "user" && (layout.prompt_padding || chrome == ThemeSurfaceChrome::Card))
            || ((chrome == ThemeSurfaceChrome::Band || chrome == ThemeSurfaceChrome::Rail)
                && padding > 0),
    );
    let leading_rows = usize::from(has_heading_row) + vertical_padding_rows;
    let trailing_rows = usize::from(has_bottom_row) + vertical_padding_rows;
    let still = theme
        .resolve::<bool>("quiet_tool_summaries")
        .unwrap_or(false);
    let compact_activity = |block: &TranscriptBlock| {
        still
            && matches!(
                block,
                TranscriptBlock::Tool(_)
                    | TranscriptBlock::NoticeStatus {
                        tone: super::NoticeTone::ToolActive
                            | super::NoticeTone::ToolSuccess
                            | super::NoticeTone::ToolError,
                        ..
                    }
            )
    };
    let transition_rows = if compact_activity(block) && previous.is_some_and(compact_activity) {
        0
    } else {
        transcript_transition_rows(previous, layout.density)
    };
    // A theme that caps the reading column would otherwise paint its band/rail
    // fills inside that column only, leaving terminal-coloured bars beside
    // them on a wide screen. Borderless filled chrome extends its fill to the
    // terminal edges while the text inside stays in the reading column. Cards
    // keep their own border, and a heading or bottom row is rendered without
    // the fill, so neither bleeds.
    let bleeds = presentation.capped
        && matches!(chrome, ThemeSurfaceChrome::Band | ThemeSurfaceChrome::Rail)
        && !has_heading_row
        && !has_bottom_row;
    let (bleed_left, bleed_right) = if bleeds {
        (
            frame_left,
            outer_width
                .saturating_sub(frame_left)
                .saturating_sub(frame_width),
        )
    } else {
        (0, 0)
    };
    SurfacePlan {
        kind,
        chrome,
        heading,
        label: resolved.label,
        user_model_lab: match block {
            TranscriptBlock::User { model_lab, .. } => *model_lab,
            _ => None,
        },
        padding,
        frame_left,
        frame_width,
        bleed_left,
        bleed_right,
        geometry: SurfaceGeometry {
            transition_rows,
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
        test_theme, test_theme_for, test_theme_from_source, test_theme_source_with,
        TerminalBackground,
    };

    #[test]
    fn capped_theme_prompt_and_tool_rows_share_a_centered_column() {
        let theme = test_theme_from_source(
            "[colors]\ncontent_max_width = 112\nevent_marker_gutter = 3\nquiet_tool_summaries = true\n[layout]\ntranscript_inset = 2\n[surfaces.user]\nchrome = \"band\"\npadding = 1\n[surfaces.tool]\nchrome = \"plain\"",
        );
        let prompt = TranscriptBlock::User {
            text: "hello".into(),
            model_lab: None,
            prompt_color: None,
            persisted: true,
        };
        let activity = tool(
            "read",
            serde_json::json!({"path": "src/a.rs"}),
            String::new(),
            None,
        );
        let prose = TranscriptBlock::Assistant(Box::new(super::super::AssistantBlock::finalized(
            "hello".into(),
        )));
        let renderer = theme.rich_renderer();
        let text_column = |block: &TranscriptBlock, width| {
            render_block_planned(
                None, block, &theme, &renderer, &renderer, width, false, 0, 0,
            )
            .lines
            .iter()
            .find_map(|line| {
                let plain = strip_terminal_sequences(line);
                let start = plain.find("hello")?;
                Some(visible_width(&plain[..start]))
            })
            .expect("hello should be rendered")
        };
        for (width, user_expected, tool_expected) in
            [(160, (24, 112), (27, 109)), (48, (0, 48), (3, 45))]
        {
            let user = compile_surface_plan(None, &prompt, &theme, width);
            let tool = compile_surface_plan(Some(&prompt), &activity, &theme, width);
            assert_eq!((user.frame_left, user.frame_width), user_expected);
            assert_eq!((tool.frame_left, tool.frame_width), tool_expected);
            assert_eq!(text_column(&prompt, width), text_column(&prose, width));
        }
    }

    #[test]
    fn built_in_still_uses_the_full_terminal_width() {
        let theme =
            test_theme_from_source(include_str!("../../../../../examples/themes/Still.toml"));
        let prompt = TranscriptBlock::User {
            text: "hello".into(),
            model_lab: None,
            prompt_color: None,
            persisted: true,
        };
        let prose = TranscriptBlock::Assistant(Box::new(super::super::AssistantBlock::finalized(
            "hello".into(),
        )));
        let activity = tool(
            "read",
            serde_json::json!({"path": "src/a.rs"}),
            String::new(),
            None,
        );
        let renderer = theme.rich_renderer();
        for width in [48, 160] {
            let presentation = PresentationLayout::new(&theme, width);
            assert_eq!((presentation.inset, presentation.content_width), (0, width));
            let user = compile_surface_plan(None, &prompt, &theme, width);
            let tool = compile_surface_plan(Some(&prompt), &activity, &theme, width);
            assert_eq!((user.frame_left, user.frame_width), (0, width));
            assert_eq!((tool.frame_left, tool.frame_width), (3, width - 3));
            for block in [&prompt, &prose] {
                let rows = render_block_planned(
                    None, block, &theme, &renderer, &renderer, width, false, 0, 0,
                )
                .lines;
                let text_column = rows
                    .iter()
                    .find_map(|row| {
                        let plain = strip_terminal_sequences(row);
                        plain.find("hello").map(|at| visible_width(&plain[..at]))
                    })
                    .expect("prompt/prose text row");
                assert!(
                    text_column <= 4,
                    "Still content was centered at width {width}: {rows:?}"
                );
            }
        }
    }

    #[test]
    fn a_capped_prompt_band_bleeds_its_fill_to_the_terminal_edges() {
        // A width-capped custom theme shades user prompts. Its band must
        // span the terminal while the text stays in the capped column.
        let theme = test_theme_from_source(
            "[colors]\ncontent_max_width = 112\nquiet_tool_summaries = true\n[roles.\"surface.user\"]\nbackground = \"#202020\"\n[surfaces.user]\nchrome = \"band\"\npadding = 1\n[surfaces.tool]\nchrome = \"plain\"",
        );
        let prompt = TranscriptBlock::User {
            text: "hello".into(),
            model_lab: None,
            prompt_color: None,
            persisted: true,
        };
        let renderer = theme.rich_renderer();
        let plan = compile_surface_plan(None, &prompt, &theme, 160);
        assert!(plan.bleeds(), "a capped band should bleed: {plan:?}");
        let rows = render_block_planned(
            None, &prompt, &theme, &renderer, &renderer, 160, false, 0, 0,
        )
        .lines;
        // Leading transition rows are inter-block spacing, not part of the band.
        let band = &rows[plan.geometry.transition_rows..];
        assert!(!band.is_empty(), "a band must render at least one row");
        for (index, row) in band.iter().enumerate() {
            let terminal = super::super::surface_frame::emulate_row_for_test(row, 160);
            for column in 0..160u16 {
                assert_ne!(
                    terminal.screen().cell(0, column).expect("cell").bgcolor(),
                    vt100::Color::Default,
                    "band row {index} left column {column} unpainted: {row:?}"
                );
            }
            // The text still lands on the reading column, not at the edge.
            let plain = strip_terminal_sequences(row);
            if let Some(start) = plain.find("hello") {
                assert!(start >= 24, "prompt text left the column: {plain:?}");
            }
        }
    }

    #[test]
    fn an_uncapped_band_keeps_its_column_width() {
        let theme = test_theme_from_source(
            "[colors]\nquiet_tool_summaries = true\n[surfaces.user]\nchrome = \"band\"\npadding = 1",
        );
        let prompt = TranscriptBlock::User {
            text: "hello".into(),
            model_lab: None,
            prompt_color: None,
            persisted: true,
        };
        let plan = compile_surface_plan(None, &prompt, &theme, 160);
        assert!(!plan.bleeds(), "an uncapped band must not bleed: {plan:?}");
        assert_eq!((plan.bleed_left, plan.bleed_right), (0, 0));
    }

    #[test]
    fn still_keeps_adjacent_activity_tight_without_affecting_other_themes() {
        let still = test_theme_from_source(
            "[colors]\nquiet_tool_summaries = true\n[layout]\ndensity = \"comfortable\"",
        );
        let ordinary = test_theme_from_source("[layout]\ndensity = \"comfortable\"");
        let first = tool(
            "read",
            serde_json::json!({"path": "src/a.rs"}),
            String::new(),
            None,
        );
        let next = tool(
            "edit",
            serde_json::json!({"path": "src/a.rs"}),
            String::new(),
            None,
        );
        assert_eq!(
            compile_surface_plan(Some(&first), &next, &still, 120)
                .geometry
                .transition_rows,
            0
        );
        assert_eq!(
            compile_surface_plan(Some(&first), &next, &ordinary, 120)
                .geometry
                .transition_rows,
            1
        );
        let prose = TranscriptBlock::Assistant(Box::new(super::super::AssistantBlock::finalized(
            "hello".into(),
        )));
        assert_eq!(
            compile_surface_plan(Some(&first), &prose, &still, 120)
                .geometry
                .transition_rows,
            1
        );
    }

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
    fn rail_and_band_surfaces_with_padding_keep_vertical_cushion_rows() {
        let theme = test_theme_from_source(
            r##"
                [surfaces.tool]
                chrome = "rail"
                padding = 1

                [surfaces.shell]
                chrome = "band"
                padding = 1

                [roles."surface.tool"]
                background = "#1e1e1e"

                [roles."surface.shell"]
                background = "#1e1e1e"
            "##,
        );
        let block = tool("bash", serde_json::json!({}), "ok".into(), None);
        let plan = compile_surface_plan(None, &block, &theme, 80);
        assert_eq!(plan.chrome, ThemeSurfaceChrome::Rail);
        assert_eq!(plan.geometry.leading_rows, 1);
        assert_eq!(plan.geometry.trailing_rows, 1);
        let shell_block = shell("echo hi", "hi");
        let shell_plan = compile_surface_plan(None, &shell_block, &theme, 80);
        assert_eq!(shell_plan.chrome, ThemeSurfaceChrome::Band);
        assert_eq!(shell_plan.geometry.leading_rows, 1);
        assert_eq!(shell_plan.geometry.trailing_rows, 1);

        // Padding zero keeps borderless surfaces flush.
        let flush_theme = test_theme_from_source(
            r##"
                [surfaces.tool]
                chrome = "rail"
                padding = 0
            "##,
        );
        let flush = compile_surface_plan(None, &block, &flush_theme, 80);
        assert_eq!(flush.geometry.leading_rows, 0);
        assert_eq!(flush.geometry.trailing_rows, 0);
    }

    #[test]
    fn rail_surfaces_fill_the_full_frame_width() {
        let theme = test_theme_from_source(
            r##"
                [surfaces.tool]
                chrome = "rail"
                padding = 1

                [roles."surface.tool"]
                background = "#1e1e1e"
            "##,
        );
        let block = tool("bash", serde_json::json!({}), "ok".into(), None);
        let plan = compile_surface_plan(None, &block, &theme, 80);
        let renderer = theme.rich_renderer();
        let rendered =
            render_block_planned(None, &block, &theme, &renderer, &renderer, 80, false, 0, 0);
        let expected = usize::from(plan.frame_left) + usize::from(plan.frame_width);
        let widths = rendered
            .lines
            .iter()
            .map(|row| strip_terminal_sequences(row))
            .filter(|row| !row.is_empty())
            .map(|row| visible_width(&row))
            .collect::<Vec<_>>();
        assert!(!widths.is_empty());
        assert!(
            widths.iter().all(|&width| width == expected),
            "{widths:?} expected every row at {expected}"
        );
    }

    #[test]
    fn cards_rails_use_one_padding_cell_and_each_prompts_stored_model_color() {
        let source = include_str!("../../../../../examples/themes/Cards.toml");
        for background in [TerminalBackground::Dark, TerminalBackground::Light] {
            let mut theme = test_theme_source_with(
                source,
                TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
                background,
            );
            let bash = tool(
                "bash",
                serde_json::json!({"command": "echo hi"}),
                "ok".into(),
                None,
            );
            let tool_plan = compile_surface_plan(None, &bash, &theme, 80);
            assert_eq!(tool_plan.frame_left, 0);
            assert_eq!(tool_plan.geometry.content_left, 2);
            assert_eq!(tool_plan.geometry.content_width, 77);
            assert_eq!(tool_plan.geometry.leading_rows, 1);
            assert_eq!(tool_plan.geometry.trailing_rows, 1);
            let renderer = theme.rich_renderer();
            let tool_rows =
                render_block_planned(None, &bash, &theme, &renderer, &renderer, 80, false, 0, 0)
                    .lines;
            assert!(
                tool_rows
                    .iter()
                    .any(|row| { strip_terminal_sequences(row).starts_with("│ Bash") }),
                "{tool_rows:?}"
            );

            for source_color in ["#aa0000", "#0000aa"] {
                let user = TranscriptBlock::User {
                    text: "hello".into(),
                    model_lab: None,
                    prompt_color: Some(source_color.into()),
                    persisted: true,
                };
                let plan = compile_surface_plan(None, &user, &theme, 80);
                assert_eq!(plan.frame_left, tool_plan.frame_left);
                assert_eq!(plan.geometry.content_left, 2);
                let renderer = theme.rich_renderer();
                let rows = render_block_planned(
                    None, &user, &theme, &renderer, &renderer, 80, false, 0, 0,
                )
                .lines;
                let rail = theme.prompt_color_marker(Some(source_color), theme.glyph("rail"));
                assert!(
                    rows.iter()
                        .all(|row| { row.is_empty() || row.starts_with(&rail) }),
                    "{rows:?}"
                );
                assert!(
                    rows.iter()
                        .any(|row| { strip_terminal_sequences(row).starts_with("│ ❯ hello") }),
                    "{rows:?}"
                );
            }
            assert_eq!(
                compile_surface_plan(None, &shell("echo hi", "hi"), &theme, 80).frame_left,
                0
            );
            for block in [
                TranscriptBlock::Assistant(Box::new(super::super::AssistantBlock::finalized(
                    "hello".into(),
                ))),
                TranscriptBlock::Notice("notice".into()),
                TranscriptBlock::NoticeStatus {
                    text: "status".into(),
                    tone: super::super::NoticeTone::Success,
                    reserved_rows: 0,
                },
                TranscriptBlock::Reasoning(Box::new(
                    super::super::AssistantBlock::streaming_reasoning("thinking"),
                )),
                TranscriptBlock::Reasoning(Box::new({
                    let mut working = super::super::AssistantBlock::streaming_reasoning("");
                    working.reasoning_heading = Some("Working".into());
                    working
                })),
            ] {
                assert_eq!(compile_surface_plan(None, &block, &theme, 80).frame_left, 2);
            }
            crate::tui::theme::apply_model_lab(&mut theme, crate::tui::theme::ModelLab::OpenAi);
            for model_lab in [None, Some(ModelLab::Anthropic)] {
                let user = TranscriptBlock::User {
                    text: "hello".into(),
                    model_lab,
                    prompt_color: None,
                    persisted: true,
                };
                let renderer = theme.rich_renderer();
                let rows = render_block_planned(
                    None, &user, &theme, &renderer, &renderer, 80, false, 0, 0,
                )
                .lines;
                let fallback = theme.model_fg(model_lab, theme.glyph("rail"));
                assert!(
                    rows.iter()
                        .all(|row| { row.is_empty() || row.starts_with(&fallback) }),
                    "{rows:?}"
                );
            }
        }
    }

    #[test]
    fn file_theme_user_surfaces_align_with_marker_gutter_blocks() {
        let theme = test_theme_from_source(
            r##"
                [surfaces.user]
                chrome = "rail"
                padding = 1

                [surfaces.tool]
                chrome = "rail"
                padding = 1
            "##,
        );
        let user = TranscriptBlock::User {
            text: "hello".into(),
            model_lab: None,
            prompt_color: None,
            persisted: true,
        };
        let bash = tool("bash", serde_json::json!({}), "ok".into(), None);
        let user_plan = compile_surface_plan(None, &user, &theme, 80);
        let tool_plan = compile_surface_plan(None, &bash, &theme, 80);
        assert_eq!(user_plan.frame_left, tool_plan.frame_left);
        assert_eq!(user_plan.frame_width, tool_plan.frame_width);
        // The compiled default keeps its historical compact prompt grid.
        let default = test_theme();
        let default_user = compile_surface_plan(None, &user, &default, 80);
        let default_tool = compile_surface_plan(None, &bash, &default, 80);
        assert_eq!(
            default_user.frame_left + PRIMARY_TEXT_GUTTER,
            default_tool.frame_left
        );
        // Plain user text in a file theme also keeps the historical grid;
        // only full-width card surfaces reserve the gutter.
        let plain_theme = test_theme_from_source(
            r##"
                [surfaces.user]
                chrome = "plain"
                padding = 0
            "##,
        );
        let plain_user = compile_surface_plan(None, &user, &plain_theme, 80);
        let plain_tool = compile_surface_plan(None, &bash, &plain_theme, 80);
        assert_eq!(
            plain_user.frame_left + PRIMARY_TEXT_GUTTER,
            plain_tool.frame_left
        );
    }

    #[test]
    fn prompt_wash_token_keeps_themed_fill_over_model_wash() {
        let filled = test_theme_from_source(
            r##"
                prompt_wash = false

                [colors]
                model.use_lab_color = "true"

                [surfaces.user]
                chrome = "rail"
                padding = 1

                [roles."surface.user"]
                background = "#1e1e1e"

                [roles."surface.user.border"]
                foreground = "#d29c54"
            "##,
        );
        let user = TranscriptBlock::User {
            text: "hello".into(),
            model_lab: None,
            prompt_color: Some("#123456".into()),
            persisted: true,
        };
        let renderer = filled.rich_renderer();
        let rendered =
            render_block_planned(None, &user, &filled, &renderer, &renderer, 80, false, 0, 0)
                .lines
                .join("\n");
        // No model-colour wash (18,52,86); the themed fill (30,30,30) wins
        // while the chevron keeps its prompt colour.
        assert!(!rendered.contains("48;2;18;52;86"), "{rendered:?}");
        assert!(rendered.contains("48;2;30;30;30"), "{rendered:?}");
        assert!(
            strip_terminal_sequences(&rendered).contains("› hello"),
            "{rendered:?}"
        );

        // Without an explicit fill the model wash still applies.
        let washed = test_theme_from_source(
            r##"
                [colors]
                model.use_lab_color = "true"

                [surfaces.user]
                chrome = "rail"
                padding = 1
            "##,
        );
        let renderer = washed.rich_renderer();
        let rendered =
            render_block_planned(None, &user, &washed, &renderer, &renderer, 80, false, 0, 0)
                .lines
                .join("\n");
        assert!(rendered.contains("48;2;18;52;86"), "{rendered:?}");
    }

    #[test]
    fn margin_markers_token_suppresses_event_dots() {
        let quiet = test_theme_from_source("margin_markers = false\n");
        let block = tool("bash", serde_json::json!({}), "ok".into(), None);
        assert!(
            super::super::surface_frame::event_margin_marker(&block, &quiet, true, false).is_none()
        );
        let loud = test_theme();
        assert!(
            super::super::surface_frame::event_margin_marker(&block, &loud, true, false).is_some()
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
            assert_eq!(
                plan.geometry.content_width,
                if width == 46 { 27 } else { 26 }
            );
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
