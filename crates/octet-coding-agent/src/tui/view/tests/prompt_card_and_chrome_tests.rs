//! Persisted prompt cards, margin markers, event dots, and the bounded caches behind them.
//! Separate because they assert the per-row chrome that frames every transcript block.

use super::support::*;

use super::*;

#[test]
fn multiline_prompt_marks_only_the_first_row() {
    let theme = crate::tui::theme::test_theme();
    let block = TranscriptBlock::User {
        text: "help me fix a bug in octet when the prompt wraps across several rows".into(),
        model_lab: Some(ModelLab::OpenAi),
        prompt_color: None,
        persisted: true,
    };
    let rendered = render_block(
        None,
        &block,
        &theme,
        &theme.rich_renderer(),
        &theme.reasoning_renderer(),
        24,
        false,
    )
    .into_iter()
    .map(|line| strip_terminal_sequences(&line))
    .collect::<Vec<_>>();

    assert!(rendered.len() > 1, "prompt should wrap: {rendered:?}");
    assert!(rendered[0].starts_with("› "), "{rendered:?}");
    assert!(
        rendered[1..]
            .iter()
            .all(|line| line.starts_with("  ") && !line.contains('│') && !line.contains('|')),
        "continuation rows should be indented without rails: {rendered:?}"
    );
}

#[test]
fn prompt_card_keeps_exact_persisted_provenance_across_theme_changes() {
    let mut first_theme = crate::tui::theme::test_theme_for(
        TerminalBackground::Dark,
        crate::tui::terminal::TerminalCapabilities::test(
            true,
            true,
            crate::tui::terminal::ColorDepth::TrueColor,
        ),
    );
    crate::tui::theme::apply_model_lab(&mut first_theme, ModelLab::OpenAi);
    let block = TranscriptBlock::User {
        text: "safe\u{1b}[31m prompt".into(),
        model_lab: Some(ModelLab::OpenAi),
        prompt_color: Some("#123456".into()),
        persisted: true,
    };
    let first = render_block(
        None,
        &block,
        &first_theme,
        &first_theme.rich_renderer(),
        &first_theme.reasoning_renderer(),
        40,
        false,
    )
    .join("\n");

    let mut second_theme = crate::tui::theme::test_theme_for(
        TerminalBackground::Dark,
        crate::tui::terminal::TerminalCapabilities::test(
            true,
            true,
            crate::tui::terminal::ColorDepth::TrueColor,
        ),
    );
    crate::tui::theme::apply_model_lab(&mut second_theme, ModelLab::DeepSeek);
    let second = render_block(
        None,
        &block,
        &second_theme,
        &second_theme.rich_renderer(),
        &second_theme.reasoning_renderer(),
        40,
        false,
    )
    .join("\n");

    let background_at_prompt = |rendered: &str| {
        let rows = rendered.lines().map(str::to_owned).collect::<Vec<_>>();
        emulate_rows(&rows, 40)
            .screen()
            .cell(1, 2)
            .expect("prompt text cell")
            .bgcolor()
    };
    assert_eq!(background_at_prompt(&first), background_at_prompt(&second));
    assert_ne!(background_at_prompt(&first), vt100::Color::Default);
    assert!(!first.contains("\x1b[31m"), "{first:?}");
    assert!(
        first.lines().all(|line| visible_width(line) <= 40),
        "{first:?}"
    );
}

#[test]
fn persisted_prompt_paints_a_full_model_adaptive_card() {
    const WIDTH: u16 = 24;
    let theme = crate::tui::theme::test_theme_for(
        TerminalBackground::Dark,
        crate::tui::terminal::TerminalCapabilities::test(
            true,
            true,
            crate::tui::terminal::ColorDepth::TrueColor,
        ),
    );
    let block = TranscriptBlock::User {
        text: "first line  \nsecond line".into(),
        model_lab: Some(ModelLab::OpenAi),
        prompt_color: Some("#123456".into()),
        persisted: true,
    };
    let rendered = render_block(
        None,
        &block,
        &theme,
        &theme.rich_renderer(),
        &theme.reasoning_renderer(),
        WIDTH,
        false,
    );
    let terminal = emulate_rows(&rendered, WIDTH);
    let plan = crate::tui::layout::PresentationLayout::new(&theme, WIDTH);
    assert_eq!(plan.inset, 0, "prompt marker retains the shared grid");
    assert_eq!(
        rendered.len(),
        4,
        "highlighted prompts need one breathing row on each side"
    );
    assert!(
        strip_terminal_sequences(&rendered[0]).trim().is_empty(),
        "leading prompt padding should remain visually blank: {rendered:?}"
    );
    assert!(
        strip_terminal_sequences(&rendered[1]).starts_with("› first line"),
        "{rendered:?}"
    );
    assert!(
        strip_terminal_sequences(&rendered[2]).starts_with("  second line"),
        "prompt continuations should keep a blank marker indent: {rendered:?}"
    );
    assert!(
        strip_terminal_sequences(rendered.last().expect("trailing prompt padding"))
            .trim()
            .is_empty()
    );
    let painted = emulate_rows(&[theme.prompt_provenance_card(Some("#123456"), "x")], 2)
        .screen()
        .cell(0, 0)
        .expect("card sample")
        .bgcolor();
    assert_ne!(painted, vt100::Color::Default);
    // The stored model colour fills the whole cell: marker gutter, padding,
    // trailing canvas, and both breathing rows. Provenance stays a card, not a
    // highlight around the wrapped text alone.
    for row in 0..rendered.len() as u16 {
        for column in 0..WIDTH {
            assert_eq!(
                terminal
                    .screen()
                    .cell(row, column)
                    .expect("prompt cell")
                    .bgcolor(),
                painted,
                "prompt card left row {row}, column {column} unpainted: {rendered:?}"
            );
        }
    }
    assert!(
        rendered
            .iter()
            .all(|row| visible_width(row) <= usize::from(WIDTH)),
        "{rendered:?}"
    );

    let expected = vt100::Color::Rgb(0x12, 0x34, 0x56);

    const CARD_WIDTH: u16 = 80;
    let card_theme = crate::tui::theme::test_theme_from_source(SURFACE_TEST_THEME);
    let card_plan = compile_surface_plan(None, &block, &card_theme, CARD_WIDTH);
    assert_eq!(card_plan.chrome, ThemeSurfaceChrome::Card);
    let card_rendered = render_block(
        None,
        &block,
        &card_theme,
        &card_theme.rich_renderer(),
        &card_theme.reasoning_renderer(),
        CARD_WIDTH,
        false,
    );
    let card_terminal = emulate_rows(&card_rendered, CARD_WIDTH);
    let content_row =
        u16::try_from(card_plan.geometry.transition_rows + card_plan.geometry.leading_rows)
            .expect("card content row fits in terminal coordinates");
    let left_border = card_plan.frame_left;
    let right_border = card_plan
        .frame_left
        .saturating_add(card_plan.frame_width)
        .saturating_sub(1);

    assert_eq!(
        card_terminal
            .screen()
            .cell(content_row, left_border)
            .expect("card left border cell")
            .bgcolor(),
        vt100::Color::Default,
        "structural card border must remain outside the surface"
    );
    let mut card_cells = 0;
    for column in left_border.saturating_add(1)..right_border {
        let color = card_terminal
            .screen()
            .cell(content_row, column)
            .expect("card inner prompt cell")
            .bgcolor();
        assert_eq!(
            color, expected,
            "prompt background did not cover theme padding at {column}"
        );
        card_cells += 1;
    }
    assert!(card_cells > 0, "card should retain its themed interior");
    assert_eq!(
        card_terminal
            .screen()
            .cell(content_row, right_border)
            .expect("card right border cell")
            .bgcolor(),
        vt100::Color::Default,
        "structural card border must remain outside the surface"
    );
}

#[test]
fn unknown_profile_keeps_rich_prompt_styling_on_the_terminal_canvas() {
    let theme = crate::tui::theme::test_theme();
    let renderer = theme.rich_renderer();
    let source = "ask `cargo check`";
    let styled = renderer
        .render(&parse_markdown(source), 30)
        .lines
        .remove(0)
        .styled;
    assert!(
        styled.contains('\x1b'),
        "inline code should be styled: {styled:?}"
    );
    let rows = render_user_prompt(
        source,
        &Some(ModelLab::OpenAi),
        Some("#123456"),
        &renderer,
        &theme,
        32,
    );
    assert!(rows.iter().any(|row| row.contains(&styled)), "{rows:?}");
    assert!(rows.iter().all(|row| !row.contains("48;")), "{rows:?}");
}

#[test]
fn default_prompt_card_keeps_inline_markdown_attributes() {
    // The card supplies the cell background only. Bold, italic, and inline-code
    // runs keep their own attributes instead of being flattened onto one
    // provenance colour.
    let theme = crate::tui::theme::test_theme_for(
        TerminalBackground::Dark,
        crate::tui::terminal::TerminalCapabilities::test(
            true,
            true,
            crate::tui::terminal::ColorDepth::TrueColor,
        ),
    );
    let block = TranscriptBlock::User {
        text: "run `cargo check` and **fix** the prompt card".into(),
        model_lab: Some(ModelLab::OpenAi),
        prompt_color: Some("#123456".into()),
        persisted: true,
    };
    let rendered = render_block(
        None,
        &block,
        &theme,
        &theme.rich_renderer(),
        &theme.reasoning_renderer(),
        48,
        false,
    );
    let body = strip_terminal_sequences(&rendered.join("\n"));
    assert!(
        body.contains("run cargo check and fix the prompt card"),
        "{body:?}"
    );
    let row = rendered
        .iter()
        .find(|row| strip_terminal_sequences(row).contains("cargo check"))
        .expect("prompt body row");
    assert!(row.contains("48;"), "prompt card lost its fill: {row:?}");
    // The rich runs survive inside the card: the inline code keeps its own
    // foreground, `fix` keeps its bold, and the card's fill is reopened after
    // every inline reset instead of being flattened onto one colour.
    let card = theme.prompt_provenance_card(Some("#123456"), "x");
    let (open, _) = card.split_once('x').expect("card preserves text");
    assert!(row.starts_with(open), "card did not open the row: {row:?}");
    assert!(
        row.matches(open).count() > 1,
        "card background was not restored after an inline reset: {row:?}"
    );
    let vt100::Color::Rgb(red, green, blue) = role_rgb_color(&theme, "md_code") else {
        panic!("inline code must resolve to a truecolor role");
    };
    assert!(
        row.contains(&format!("38;2;{red};{green};{blue}m")),
        "inline code lost its own colour: {row:?}"
    );
    assert!(row.contains("\x1b[1mfix\x1b[0m"), "bold lost: {row:?}");
}

#[test]
fn compiled_prompt_highlight_preserves_media_labels_unicode_copy_and_terminal_fallbacks() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};

    let source = "界🙂 [Image #1]  \nnext [Audio #2]";
    let block = TranscriptBlock::User {
        text: source.into(),
        model_lab: Some(ModelLab::OpenAi),
        prompt_color: Some("#123456".into()),
        persisted: true,
    };
    assert_eq!(block_copy_text(&block), source);
    for background in [
        TerminalBackground::Unknown,
        TerminalBackground::Dark,
        TerminalBackground::Light,
    ] {
        for color in [ColorDepth::None, ColorDepth::Ansi16, ColorDepth::TrueColor] {
            for unicode in [false, true] {
                let theme = crate::tui::theme::test_theme_for(
                    background,
                    TerminalCapabilities::test(true, unicode, color),
                );
                let rendered = render_block(
                    None,
                    &block,
                    &theme,
                    &theme.rich_renderer(),
                    &theme.reasoning_renderer(),
                    24,
                    false,
                );
                let plain = rendered
                    .iter()
                    .map(|row| strip_terminal_sequences(row))
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(
                    plain.contains("[Image #1]") && plain.contains("[Audio #2]"),
                    "{plain:?}"
                );
                assert!(
                    rendered.iter().all(|row| visible_width(row) <= 24),
                    "{rendered:?}"
                );
                if color == ColorDepth::None {
                    assert!(
                        rendered.iter().all(|row| !row.contains('\x1b')),
                        "{rendered:?}"
                    );
                }
                let terminal = emulate_rows(&rendered, 24);
                let screen = terminal.screen();
                // The stored model colour is one card across the whole row, so
                // the marker gutter and the trailing canvas are painted too.
                let washed = color != ColorDepth::None && background != TerminalBackground::Unknown;
                for column in [0, 2, 23] {
                    assert_eq!(
                        screen.cell(1, column).unwrap().bgcolor() != vt100::Color::Default,
                        washed,
                        "unexpected card coverage at column {column}: {rendered:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn tool_lifecycle_styles_are_visible_in_terminal_cells() {
    let theme = crate::tui::theme::test_theme_from_source(
        r##"
                [metadata]
                name = "Tool lifecycle cells"

                [colors]
                foreground = "#f4f4f4"
                muted = "#686868"
                error = "#e43f4f"

                [roles."extension.live"]
                foreground = "#00ff00"
                bold = true
            "##,
    );
    let renderer = theme.rich_renderer();
    let muted = role_rgb_color(&theme, "muted");
    let foreground = role_rgb_color(&theme, "foreground");
    let error = role_rgb_color(&theme, "error");
    let syntax_string = role_rgb_color(&theme, "syntax_string");
    assert_ne!(muted, foreground);
    assert_ne!(error, foreground);

    let args = serde_json::json!({"path":"src/lib.rs"});
    let mut active_panel = ToolPanel::new(
        ToolCallId("active-read".into()),
        "read".into(),
        args.to_string(),
        summarize_tool("read", &args),
        "live raw evidence".into(),
        false,
        false,
        None,
        None,
    );
    active_panel.extension_render_segments =
        vec![octet_agent::extension_process::ToolRenderSegment {
            text: "live output".into(),
            style_role: Some("extension.live".into()),
        }];
    let active = render_block(
        None,
        &TranscriptBlock::Tool(Box::new(active_panel)),
        &theme,
        &renderer,
        &renderer,
        80,
        true,
    );
    let active = emulate_rows(&active, 80);
    assert_ascii_foreground(&active, "Read", foreground);
    assert_ascii_bold(&active, "Read");
    assert_ascii_foreground(&active, "src/lib.rs", muted);
    assert!(!active.screen().contents().contains("live raw evidence"));
    assert!(!active.screen().contents().contains("live output"));

    let completed = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("completed-read".into()),
        "read".into(),
        args.to_string(),
        summarize_tool("read", &args),
        String::new(),
        true,
        false,
        None,
        None,
    )));
    let completed = render_block(None, &completed, &theme, &renderer, &renderer, 80, false);
    let completed = emulate_rows(&completed, 80);
    assert_ascii_foreground(&completed, "Read", foreground);
    assert_ascii_bold(&completed, "Read");
    assert_ascii_foreground(&completed, "src/lib.rs", muted);

    let failed = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("failed-read".into()),
        "read".into(),
        args.to_string(),
        summarize_tool("read", &args),
        "error\npermission denied".into(),
        true,
        true,
        Some("permission denied".into()),
        None,
    )));
    let failed = render_block(None, &failed, &theme, &renderer, &renderer, 80, false);
    let failed = emulate_rows(&failed, 80);
    assert_ascii_foreground(&failed, "Read", foreground);
    assert_ascii_bold(&failed, "Read");
    assert!(failed.screen().contents().contains("permission denied"));
    assert_ascii_foreground(&failed, "permission denied", error);

    let active_bash_args = serde_json::json!({"command":"echo \"active\""});
    let active_bash = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("active-bash".into()),
        "bash".into(),
        active_bash_args.to_string(),
        summarize_tool("bash", &active_bash_args),
        "private streaming output".into(),
        false,
        false,
        None,
        None,
    )));
    let active_bash = render_block(None, &active_bash, &theme, &renderer, &renderer, 80, true);
    let active_bash = emulate_rows(&active_bash, 80);
    assert_ascii_foreground(&active_bash, "Bash", foreground);
    assert_ascii_bold(&active_bash, "Bash");
    assert_ascii_foreground(&active_bash, "\"active\"", syntax_string);
    assert_ascii_foreground(&active_bash, "private streaming output", muted);
    assert!(active_bash
        .screen()
        .contents()
        .contains("private streaming output"));

    for (command, is_error) in [("echo \"complete\"", false), ("echo \"failed\"", true)] {
        let args = serde_json::json!({"command":command});
        let panel = TranscriptBlock::Tool(Box::new(ToolPanel::new(
            ToolCallId(command.into()),
            "bash".into(),
            args.to_string(),
            summarize_tool("bash", &args),
            String::new(),
            true,
            is_error,
            is_error.then(|| "exit 1".into()),
            None,
        )));
        let rendered = render_block(None, &panel, &theme, &renderer, &renderer, 80, false);
        let terminal = emulate_rows(&rendered, 80);
        assert_ascii_foreground(&terminal, "Bash", foreground);
        let quoted = if is_error {
            "\"failed\""
        } else {
            "\"complete\""
        };
        assert_ascii_foreground(&terminal, quoted, syntax_string);
        let (_, bash_column) =
            find_ascii_cell(terminal.screen(), "Bash").expect("Bash label rendered");
        assert_ne!(
            terminal
                .screen()
                .cell(0, bash_column)
                .expect("Bash label cell")
                .fgcolor(),
            error,
            "tool label must not inherit lifecycle red"
        );
    }
}

#[test]
fn prompt_padding_adds_static_prompt_background_rows() {
    let theme = crate::tui::theme::test_theme_from_source("[layout]\nprompt_padding = true");
    let prompt = TranscriptBlock::User {
        text: "padded prompt".into(),
        model_lab: None,
        prompt_color: None,
        persisted: true,
    };

    let plan = compile_surface_plan(None, &prompt, &theme, 120);

    assert_eq!(plan.geometry.leading_rows, 1);
    assert_eq!(plan.geometry.trailing_rows, 1);
}

#[test]
fn notice_markers_use_neutral_success_and_error_lifecycle_tones() {
    let theme = crate::tui::theme::test_theme();
    let neutral = TranscriptBlock::Notice("model changed".into());
    let approved = TranscriptBlock::NoticeStatus {
        text: "action approved".into(),
        tone: NoticeTone::Success,
        reserved_rows: 0,
    };
    let denied = TranscriptBlock::NoticeStatus {
        text: "action denied".into(),
        tone: NoticeTone::Error,
        reserved_rows: 0,
    };

    assert_eq!(
        event_margin_marker(&neutral, &theme, false, false),
        Some(theme.settled_event_dot("neutral", "•"))
    );
    assert_eq!(
        event_margin_marker(&approved, &theme, false, false),
        Some(theme.settled_event_dot("success", "•"))
    );
    assert_eq!(
        event_margin_marker(&denied, &theme, false, false),
        Some(theme.settled_event_dot("error", "•"))
    );
}

#[test]
fn still_tool_dots_breathe_smoothly_without_status_colours_or_blinking() {
    let source = r##"
        [colors]
        tool_dot_breathing = true
        tool_dot_dim = "#646464"
        tool_dot_bright = "#a0a0a0"
    "##;
    let theme = crate::tui::theme::test_theme_from_source(source);
    let args = serde_json::json!({"path":"src/lib.rs"});
    let panel = |finished, is_error| {
        TranscriptBlock::Tool(Box::new(ToolPanel::new(
            ToolCallId("read".into()),
            "read".into(),
            args.to_string(),
            summarize_tool("read", &args),
            String::new(),
            finished,
            is_error,
            is_error.then(|| "failed".into()),
            None,
        )))
    };
    let marker = |block: &TranscriptBlock, frame| {
        surface_frame::event_margin_marker_with_frame(block, &theme, frame, None, 0, false).unwrap()
    };
    let active = panel(false, false);
    let frames = (0..12)
        .map(|frame| marker(&active, frame))
        .collect::<Vec<_>>();
    assert_eq!(frames[0], frames[10]);
    assert_eq!(frames[1], frames[9]);
    assert_ne!(frames[5], frames[11]);
    for frame in &frames {
        assert_eq!(strip_terminal_sequences(frame), "•");
        assert!(!frame.contains("\x1b[5m"));
    }
    assert_eq!(
        marker(&panel(true, false), 0),
        marker(&panel(true, true), 0)
    );
    let group = |tone| TranscriptBlock::NoticeStatus {
        text: "Explored".into(),
        tone,
        reserved_rows: 0,
    };
    assert_eq!(marker(&group(NoticeTone::ToolActive), 5), frames[5]);
    assert_eq!(
        marker(&group(NoticeTone::ToolSuccess), 0),
        marker(&group(NoticeTone::ToolError), 0)
    );

    let ascii = crate::tui::theme::test_theme_source_with(
        source,
        crate::tui::terminal::TerminalCapabilities::test(
            false,
            false,
            crate::tui::terminal::ColorDepth::None,
        ),
        TerminalBackground::Unknown,
    );
    assert_eq!(
        surface_frame::event_margin_marker_with_frame(&active, &ascii, 1, None, 0, false),
        Some("*".into())
    );
}

#[test]
fn event_margin_markers_cover_responses_tools_and_collapsed_reasoning() {
    let theme = crate::tui::theme::test_theme();
    let args = serde_json::json!({"path":"src/lib.rs"});
    let panel = |finished, is_error| {
        TranscriptBlock::Tool(Box::new(ToolPanel::new(
            ToolCallId("edit".into()),
            "edit".into(),
            args.to_string(),
            summarize_tool("edit", &args),
            String::new(),
            finished,
            is_error,
            is_error.then(|| "failed".into()),
            None,
        )))
    };

    let active_tool = event_margin_marker(&panel(false, false), &theme, true, false)
        .expect("visible active tool marker");
    let quiet_tool = event_margin_marker(&panel(false, false), &theme, false, false)
        .expect("quiet active tool marker");
    assert_eq!(strip_terminal_sequences(&active_tool), "•");
    assert_eq!(strip_terminal_sequences(&quiet_tool), "•");
    assert_eq!(active_tool, theme.fg("foreground", "•"));
    assert_eq!(quiet_tool, theme.settled_event_dot("neutral", "•"));
    assert!(!active_tool.contains("\x1b[5m"), "{active_tool:?}");
    assert_ne!(
        active_tool, quiet_tool,
        "active tool dots should pulse through tone"
    );

    let successful_tool =
        event_margin_marker(&panel(true, false), &theme, false, false).expect("success marker");
    assert_eq!(successful_tool, theme.settled_event_dot("success", "•"));
    let failed_tool =
        event_margin_marker(&panel(true, true), &theme, false, false).expect("failure marker");
    assert_eq!(failed_tool, theme.settled_event_dot("error", "•"));

    let streaming_response =
        TranscriptBlock::Assistant(Box::new(AssistantBlock::streaming("working")));
    let streaming_visible = event_margin_marker(&streaming_response, &theme, true, false)
        .expect("streaming assistant marker");
    let streaming_quiet = event_margin_marker(&streaming_response, &theme, false, false)
        .expect("quiet streaming assistant marker");
    assert_eq!(streaming_visible, theme.fg("foreground", "•"));
    assert_eq!(
        streaming_quiet, streaming_visible,
        "assistant dots stay solid light instead of pulsing into a dim slot"
    );
    let finished_response =
        TranscriptBlock::Assistant(Box::new(AssistantBlock::finalized("done".into())));
    assert_eq!(
        event_margin_marker(&finished_response, &theme, false, false),
        Some(theme.fg("foreground", "•"))
    );

    let prompt = TranscriptBlock::User {
        text: "prompt".into(),
        model_lab: None,
        prompt_color: None,
        persisted: true,
    };
    assert_eq!(event_margin_marker(&prompt, &theme, true, false), None);
    let reasoning =
        TranscriptBlock::Reasoning(Box::new(AssistantBlock::streaming_reasoning("private")));
    assert_eq!(event_margin_marker(&reasoning, &theme, true, false), None);
    assert_eq!(event_margin_marker(&reasoning, &theme, false, false), None);
    assert_eq!(
        event_margin_marker(&reasoning, &theme, true, true)
            .map(|marker| strip_terminal_sequences(&marker)),
        Some("•".into())
    );
    let reasoning_slot = event_margin_marker(&reasoning, &theme, false, true)
        .expect("steady collapsed reasoning marker");
    assert_eq!(strip_terminal_sequences(&reasoning_slot), "•");
    assert_eq!(reasoning_slot, theme.model_fg(None, "•"));
    let compaction = TranscriptBlock::Compaction(Box::new(CompactionBlock {
        label: "Context compacted".into(),
        summary: "summary".into(),
        expanded: false,
    }));
    assert_eq!(event_margin_marker(&compaction, &theme, true, false), None);
    let outcome = TranscriptBlock::Outcome(OutcomeBlock::new(
        RunOutcome::CompletedWithWarnings {
            elapsed: Duration::from_secs(1),
            warnings: 1,
            summary: crate::presentation::RunSummary {
                files_changed: 0,
                tool_calls: 1,
                warnings: 1,
            },
        },
        None,
    ));
    assert_eq!(event_margin_marker(&outcome, &theme, true, false), None);
}

#[test]
fn event_dot_animation_invalidates_active_tool_rows_in_lockstep() {
    let shell = InteractiveShell::test_shell();
    {
        let mut state = shell.state.borrow_mut();
        for (id, name) in [("read", "read"), ("edit", "edit")] {
            let args = serde_json::json!({"path":"src/lib.rs"});
            let index = state.transcript.len();
            state.push_block(TranscriptBlock::Tool(Box::new(ToolPanel::new(
                ToolCallId(id.into()),
                name.into(),
                args.to_string(),
                summarize_tool(name, &args),
                String::new(),
                false,
                false,
                None,
                None,
            ))));
            state.register_active_event(index);
        }
        state.event_dot_visible = true;
        assert!(event_dot_animating(&state));
    }

    let active_rows = || {
        shell
            .state
            .borrow()
            .rendered_transcript(80)
            .iter()
            .filter(|line| line.contains("Read") || line.contains("Edit"))
            .cloned()
            .collect::<Vec<_>>()
    };
    let uses_uniform_dot = |lines: &[String]| {
        lines
            .iter()
            .all(|line| strip_terminal_sequences(line).starts_with("• "))
    };
    let visible = active_rows();
    assert_eq!(visible.len(), 2, "{visible:?}");
    assert!(uses_uniform_dot(&visible), "{visible:?}");

    shell.state.borrow_mut().advance_event_dot_animation();
    let quiet = active_rows();
    assert_eq!(quiet.len(), 2, "{quiet:?}");
    assert!(uses_uniform_dot(&quiet), "{quiet:?}");
    assert_ne!(
        quiet, visible,
        "event dots should pulse through colour only"
    );

    shell.state.borrow_mut().advance_event_dot_animation();
    let visible_again = active_rows();
    assert_eq!(visible_again, visible);
}

#[test]
fn streaming_response_dot_stays_solid_light_and_settles_solid() {
    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("openai");
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "Streaming answer".into(),
        },
    );

    let response_row = || {
        shell
            .state
            .borrow()
            .rendered_transcript(80)
            .iter()
            .find(|line| line.contains("Streaming answer"))
            .cloned()
            .expect("streaming response row")
    };
    let visible = response_row();
    assert!(
        strip_terminal_sequences(&visible).starts_with("• Streaming answer"),
        "{visible:?}"
    );
    {
        let mut state = shell.state.borrow_mut();
        // The assistant keeps its solid provenance dot while the separate
        // Working row carries continuing run liveness.
        assert_eq!(state.active_event_blocks, vec![0, 1]);
        assert!(!event_dot_animating(&state));
        state.advance_event_dot_animation();
    }
    let quiet = response_row();
    assert_eq!(
        quiet, visible,
        "assistant dots should stay solid light while streaming"
    );

    {
        let mut state = shell.state.borrow_mut();
        state.close_streaming_blocks();
        assert!(!event_dot_animating(&state));
        assert!(state.active_event_blocks.is_empty());
        let response = state.transcript.first().expect("finished response");
        assert_eq!(
            event_margin_marker(response, &state.theme, false, false),
            Some(state.theme.fg("foreground", "•"))
        );
    }
}

#[test]
fn event_dot_tracking_stays_bounded_in_long_sessions() {
    let shell = InteractiveShell::test_shell();
    {
        let mut state = shell.state.borrow_mut();
        for index in 0..10_000 {
            state.push_block(TranscriptBlock::Notice(format!("history {index}")));
        }
    }

    // A running tool drives the pulse; its block revision must move without
    // touching any of the settled history above it.
    let args = serde_json::json!({"path":"src/lib.rs"});
    let active_index = {
        let mut state = shell.state.borrow_mut();
        let index = state.transcript.len();
        state.push_block(TranscriptBlock::Tool(Box::new(ToolPanel::new(
            ToolCallId("edit".into()),
            "edit".into(),
            args.to_string(),
            summarize_tool("edit", &args),
            String::new(),
            false,
            false,
            None,
            None,
        ))));
        state.register_active_event(index);
        assert!(event_dot_animating(&state));
        index
    };
    shell.state.borrow_mut().advance_event_dot_animation();
    {
        let state = shell.state.borrow();
        assert_eq!(state.block_revisions[active_index], 1);
        assert!(state.block_revisions[..active_index]
            .iter()
            .all(|revision| *revision == 0));
    }

    shell
        .state
        .borrow_mut()
        .unregister_active_event(active_index);
    assert!(shell.state.borrow().active_event_blocks.is_empty());
}

#[test]
fn reasoning_to_working_to_tool_reuses_the_cached_tail_in_long_sessions() {
    let mut shell = InteractiveShell::test_shell();
    {
        let mut state = shell.state.borrow_mut();
        for index in 0..4_096 {
            state.push_block(TranscriptBlock::Notice(format!("history {index}")));
        }
    }
    let run_id = shell.begin_run("openai");
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "## Inspecting the repository\n".into(),
        },
    );
    shell.state.borrow_mut().finish_turn_streaming_blocks();
    let (working_start, history_lines, generation) = {
        let state = shell.state.borrow();
        assert!(matches!(
            state.transcript.last(),
            Some(TranscriptBlock::Reasoning(reasoning)) if reasoning.is_working_activity()
        ));
        assert!(matches!(
            state.transcript.get(state.transcript.len() - 2),
            Some(TranscriptBlock::Reasoning(reasoning)) if reasoning.finished
        ));
        let lines = state.rendered_transcript(80);
        let cache = state.transcript_cache.borrow();
        let working_start = *cache.block_starts.last().expect("Working block start");
        (
            working_start,
            lines[..working_start].to_vec(),
            cache.generation,
        )
    };

    shell.on_run_event(
        run_id,
        &AgentEvent::ToolStarted {
            id: ToolCallId("responsive-read".into()),
            name: "read".into(),
            args: serde_json::json!({"path": "README.md"}),
        },
    );
    {
        let state = shell.state.borrow();
        let cache = state.transcript_cache.borrow();
        assert_eq!(
            cache.width,
            Some(80),
            "tool admission must not force reflow"
        );
        assert_eq!(cache.lines, history_lines);
        assert_eq!(cache.block_revisions.len() + 2, state.transcript.len());
    }

    shell.apply_edit(EditAction::Char('x'));
    {
        let state = shell.state.borrow();
        let rendered = state.rendered_transcript(80);
        let cache = state.transcript_cache.borrow();
        assert_eq!(state.editor.text(), "x");
        assert_eq!(cache.generation, generation + 1);
        assert_eq!(cache.last_update_start, working_start);
        assert_eq!(&rendered[..working_start], history_lines.as_slice());
    }
}

#[test]
fn unrendered_working_handoff_preserves_the_long_session_cache() {
    let mut shell = InteractiveShell::test_shell();
    {
        let mut state = shell.state.borrow_mut();
        for index in 0..4_096 {
            state.push_block(TranscriptBlock::Notice(format!("history {index}")));
        }
    }
    let run_id = shell.begin_run("openai");
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "## Inspecting the repository\n".into(),
        },
    );
    let (reasoning_start, history_lines, generation, cached_blocks) = {
        let state = shell.state.borrow();
        let lines = state.rendered_transcript(80);
        let cache = state.transcript_cache.borrow();
        let reasoning_start = *cache.block_starts.last().expect("Thinking block start");
        (
            reasoning_start,
            lines[..reasoning_start].to_vec(),
            cache.generation,
            cache.block_revisions.len(),
        )
    };

    // Provider settlement and tool admission commonly arrive in one event
    // burst, before the intermediate Working row receives a frame.
    shell.state.borrow_mut().finish_turn_streaming_blocks();
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolStarted {
            id: ToolCallId("immediate-read".into()),
            name: "read".into(),
            args: serde_json::json!({"path": "README.md"}),
        },
    );
    {
        let state = shell.state.borrow();
        let cache = state.transcript_cache.borrow();
        assert_eq!(cache.width, Some(80), "the cache width must be retained");
        assert_eq!(cache.block_revisions.len(), cached_blocks);
        assert_eq!(cache.block_revisions.len() + 2, state.transcript.len());
    }

    shell.apply_edit(EditAction::Char('x'));
    {
        let state = shell.state.borrow();
        let rendered = state.rendered_transcript(80);
        let cache = state.transcript_cache.borrow();
        assert_eq!(state.editor.text(), "x");
        assert_eq!(cache.generation, generation + 1);
        assert_eq!(cache.last_update_start, reasoning_start);
        assert_eq!(&rendered[..reasoning_start], history_lines.as_slice());
    }
}

#[test]
fn tool_summaries_do_not_repeat_the_action_label() {
    assert_eq!(
        without_redundant_tool_lead("read", "read /tmp/src/lib.rs"),
        "/tmp/src/lib.rs"
    );
    assert_eq!(
        without_redundant_tool_lead("bash", "running cargo test --workspace"),
        "cargo test --workspace"
    );
    assert_eq!(
        without_redundant_tool_lead("edit", "updated src/lib.rs"),
        "src/lib.rs"
    );
    assert_eq!(
        without_redundant_tool_lead("write", "wrote src/lib.rs"),
        "src/lib.rs"
    );
}
