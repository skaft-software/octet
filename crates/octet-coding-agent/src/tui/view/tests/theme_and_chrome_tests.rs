//! Model switching, custom themes, density and inset geometry, and card degradation. Separate
//! because they assert theme tokens and the geometry those tokens imply.

use super::support::*;

use super::*;

#[test]
fn active_model_switch_keeps_run_identity_and_clears_stale_idle_telemetry() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(46, 12);
    shell.set_identity("openai", "gpt-5.6", "high");
    {
        let mut state = shell.state.borrow_mut();
        crate::tui::theme::apply_model_lab(&mut state.theme, ModelLab::OpenAi);
        state.model_lab = Some(ModelLab::OpenAi);
        state.context_estimate = Some((12_000, 256_000));
        state.price_display = PriceDisplay::Priced;
    }
    shell.on_prompt_submitted("prompt for A");
    let run_id = shell.begin_run("openai");
    let now = Instant::now();
    let before =
        crate::tui::composer_surface::render_composer_surface(&shell.state.borrow(), 46, now);

    shell.set_identity("deepseek", "deepseek-v4-pro", "medium");
    {
        let mut state = shell.state.borrow_mut();
        crate::tui::theme::apply_model_lab(&mut state.theme, ModelLab::DeepSeek);
        state.model_lab = Some(ModelLab::DeepSeek);
        state.context_estimate = Some((2_000, 128_000));
        state.last_turn_tokens_per_second = Some(55.0);
        state.run_cost_microdollars = 3_000;
        state.run_cost_available = true;
    }
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "Checking ownership".into(),
        },
    );
    let active_footer = plain_footer(&shell, 46, now);
    assert!(active_footer.contains("GPT-5.6"), "{active_footer:?}");
    assert!(!active_footer.contains("DeepSeek"), "{active_footer:?}");
    shell.set_size(24, 12);
    let narrow_active = plain_footer(&shell, 24, now);
    assert!(narrow_active.contains("GPT-5.6"), "{narrow_active:?}");
    assert!(!narrow_active.contains("Working"), "{narrow_active:?}");
    assert!(!narrow_active.contains("tool"), "{narrow_active:?}");
    shell.set_size(46, 12);
    {
        let state = shell.state.borrow();
        let TranscriptBlock::Reasoning(reasoning) = state.transcript.last().unwrap() else {
            panic!("streamed reasoning block expected");
        };
        assert_eq!(reasoning.model_lab, Some(ModelLab::OpenAi));
    }
    let after =
        crate::tui::composer_surface::render_composer_surface(&shell.state.borrow(), 46, now);
    assert_eq!(before.len(), after.len());
    assert_eq!(visible_width(&before[0]), visible_width(&after[0]));

    shell.interrupt_run(run_id);
    let idle_footer = plain_footer(&shell, 46, now);
    assert!(idle_footer.contains("DeepSeek V4 Pro"), "{idle_footer:?}");
    assert!(!idle_footer.contains("55.0 tok/s"), "{idle_footer:?}");
    assert!(!idle_footer.contains("$0.003"), "{idle_footer:?}");
    shell.on_prompt_submitted("prompt for B");
    let state = shell.state.borrow();
    let prompts = state
        .transcript
        .iter()
        .filter_map(|block| match block {
            TranscriptBlock::User {
                text,
                model_lab,
                prompt_color,
                ..
            } => Some((text.as_str(), *model_lab, prompt_color.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        prompts,
        vec![
            (
                "prompt for A",
                Some(ModelLab::OpenAi),
                Some(crate::tui::theme::prompt_color_for_model_id("gpt-5.6")),
            ),
            (
                "prompt for B",
                Some(ModelLab::DeepSeek),
                Some(crate::tui::theme::prompt_color_for_model_id(
                    "deepseek-v4-pro",
                )),
            ),
        ]
    );
}

#[test]
fn custom_theme_composer_tokens_render_framed_ruled_and_shaded_composers() {
    let composer_lines = |source: &str| -> Vec<String> {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(60, 12);
        shell.set_theme(crate::tui::theme::test_theme_from_source(source));
        let rendered = render_shell(&shell.state.borrow(), 60);
        rendered
    };

    let framed = composer_lines(
        r##"
            [colors]
            composer = "framed"
            composer_border = "#6688aa"
            [layout]
            composer_padding = 1
        "##,
    );
    let plain = framed
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>();
    let top = plain
        .iter()
        .position(|line| line.trim_start().starts_with('╭') && line.ends_with('╮'))
        .unwrap_or_else(|| panic!("framed composer lost its top corners: {plain:?}"));
    assert!(
        plain[top + 1..]
            .iter()
            .any(|line| line.trim_start().starts_with('╰') && line.ends_with('╯')),
        "framed composer lost its bottom corners"
    );
    assert!(
        plain[top + 1].trim_start().starts_with('│') && plain[top + 1].ends_with('│'),
        "framed composer content rows lost their side borders: {:?}",
        plain[top + 1]
    );

    let ruled = composer_lines(
        r##"
            [colors]
            composer = "boxed"
            composer_border = "#6688aa"
            [layout]
            composer_padding = 1
        "##,
    )
    .iter()
    .map(|line| strip_terminal_sequences(line))
    .collect::<Vec<_>>();
    assert!(
        ruled
            .iter()
            .filter(|line| {
                !line.trim().is_empty() && line.trim().chars().all(|character| character == '─')
            })
            .count()
            >= 2,
        "ruled composer lost its rules: {ruled:?}"
    );

    let shaded = composer_lines(
        r##"
            [colors]
            composer = "shaded"
            composer_bg = "#323232"
            [layout]
            prompt_padding = true
            composer_padding = 1
        "##,
    );
    let plain = shaded
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>();
    assert!(
        !plain
            .iter()
            .any(|line| line.contains('─') || line.contains('╭') || line.contains('│')),
        "shaded composer must not draw rules or frames: {plain:?}"
    );
    assert_eq!(
        shaded
            .iter()
            .filter(|line| line.contains("\x1b[48;2;50;50;50m"))
            .count(),
        3,
        "shaded composer should paint exactly three rows"
    );
}

#[test]
fn transcript_and_composer_have_exactly_one_breathing_row() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 5);
    shell.on_prompt_submitted("question");
    shell
        .state
        .borrow_mut()
        .push_block(TranscriptBlock::Assistant(Box::new(
            AssistantBlock::finalized("answer".into()),
        )));
    let lines = render_shell(&shell.state.borrow(), 80)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>();
    let composer = lines
        .iter()
        .position(|line| {
            !line.trim().is_empty() && line.trim().chars().all(|c| c == '─' || c == '-')
        })
        .expect("composer top rule");
    assert!(composer > 0);
    assert!(lines[composer - 1].is_empty());
    assert!(composer < 2 || !lines[composer - 2].is_empty());
}

#[test]
fn resize_defers_exactly_one_transcript_reflow_to_the_render_thread() {
    let mut shell = InteractiveShell::test_shell();
    {
        let mut state = shell.state.borrow_mut();
        for index in 0..512 {
            state.push_block(TranscriptBlock::Assistant(Box::new(
                AssistantBlock::finalized(format!(
                    "long stable answer {index} with enough words to wrap across widths"
                )),
            )));
        }
        let _ = state.rendered_transcript(100);
    }
    let generation = shell.state.borrow().transcript_cache.borrow().generation;
    shell.set_size(52, 20);
    {
        let state = shell.state.borrow();
        let cache = state.transcript_cache.borrow();
        assert_eq!(cache.generation, generation, "input thread must not reflow");
        assert_eq!(cache.width, None);
    }
    {
        let state = shell.state.borrow();
        let _ = state.rendered_transcript(52);
    }
    let state = shell.state.borrow();
    let cache = state.transcript_cache.borrow();
    assert_eq!(cache.generation, generation + 1);
    assert_eq!(cache.width, Some(52));
}

#[test]
fn slash_popup_keeps_selection_visible_across_paging_filtering_and_resize() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 14);
    shell.apply_edit(EditAction::Char('/'));
    shell.slash_menu(SlashMenuAction::Last);
    let last = commands::slash_suggestions("/").len() - 1;
    assert_eq!(shell.state.borrow().slash_selection, last);

    shell.set_size(34, 9);
    let resized = shell_chrome(&shell.state.borrow(), 34, Instant::now()).suggestions;
    let resized_plain = resized
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>();
    assert!(resized_plain.iter().any(|line| line.contains("/exit")));
    assert!(resized_plain
        .iter()
        .any(|line| line.contains('›') && line.contains("/exit")));
    assert!(resized_plain.first().is_some_and(|line| line.contains('/')));
    assert!(resized.iter().all(|line| visible_width(line) <= 34));

    let page = resized.len().saturating_sub(1).max(1);
    shell.slash_menu(SlashMenuAction::PageUp);
    assert_eq!(
        shell.state.borrow().slash_selection,
        last.saturating_sub(page)
    );
    shell.slash_menu(SlashMenuAction::First);
    shell.slash_menu(SlashMenuAction::PageDown);
    assert_eq!(shell.state.borrow().slash_selection, page.min(last));

    shell.slash_menu(SlashMenuAction::Last);
    shell.apply_edit(EditAction::Char('m'));
    let state = shell.state.borrow();
    assert_eq!(state.editor.text(), "/m");
    assert_eq!(state.slash_selection, 0);
    assert_eq!(state.slash_scroll, 0);
    drop(state);

    shell.set_size(1, 9);
    let narrow = render_slash_suggestions(&shell.state.borrow(), 1, 5);
    assert!(narrow.iter().all(|line| visible_width(line) <= 1));
}

#[test]
fn composer_border_stays_stable_when_draft_content_changes() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("anthropic", "claude-sonnet-4", "high");
    {
        let mut state = shell.state.borrow_mut();
        crate::tui::theme::apply_model_lab(&mut state.theme, ModelLab::Anthropic);
        state.model_lab = Some(ModelLab::Anthropic);
    }
    let now = Instant::now();
    let idle =
        crate::tui::composer_surface::render_composer_surface(&shell.state.borrow(), 60, now);
    shell.apply_edit(EditAction::Char('x'));
    let focused =
        crate::tui::composer_surface::render_composer_surface(&shell.state.borrow(), 60, now);

    assert_eq!(idle[0], focused[0]);
    let accent = {
        let state = shell.state.borrow();
        state
            .theme
            .model_rgb(Some(ModelLab::Anthropic))
            .expect("Anthropic model accent")
    };
    let accent = format!("38;2;{};{};{}", accent.0, accent.1, accent.2);
    assert!(focused[0].contains(&accent), "{:?}", focused[0]);
    assert!(focused[0].contains("38;2;169;99;76"), "{:?}", focused[0]);
    let plain_edge = strip_terminal_sequences(&idle[0]);
    assert!(
        !plain_edge.starts_with(' '),
        "full-width separator must reach the terminal edge: {plain_edge:?}"
    );
    assert_eq!(visible_width(&plain_edge), 60);
    let prompt = strip_terminal_sequences(&idle[1]);
    assert!(
        prompt.starts_with('›'),
        "composer content must use the shared grid: {prompt:?}"
    );
    let wide =
        crate::tui::composer_surface::render_composer_surface(&shell.state.borrow(), 120, now);
    assert_eq!(wide[0].matches("\x1b[38;2;").count(), 1);
    assert!(
        wide[0].len() < 450,
        "120-column uniform border encoded {} bytes",
        wide[0].len()
    );
    for edge in [
        &idle[0],
        &idle[idle.len() - 2],
        &focused[0],
        &focused[focused.len() - 2],
    ] {
        assert_eq!(
            edge.matches("\x1b[38;2;").count(),
            1,
            "uniform border reopened its RGB style per cell: {edge:?}"
        );
        assert!(
            edge.len() < 240,
            "uniform border encoded {} bytes",
            edge.len()
        );
    }
}

#[test]
fn explicit_theme_preserves_composer_and_code_chrome() {
    let theme = theme_with_layout("composer_padding = 2");
    assert!(theme.rich_renderer().options().code_borders);
    let shell = InteractiveShell::test_shell_with_theme(theme);
    let rendered = plain_composer_surface(&shell, 60, Instant::now());
    let is_rule = |line: &String| line.trim().chars().all(|c| c == '─' || c == '-');
    assert!(rendered.first().is_some_and(is_rule));
    assert!(rendered.get(1).is_some_and(
        |line| line.trim_start().starts_with('›') || line.trim_start().starts_with('>')
    ));
    assert!(
        rendered
            .get(1)
            .is_some_and(|line| !line.contains('│') && !line.contains('|')),
        "composer rows must not carry side borders"
    );
    assert!(rendered.get(2).is_some_and(is_rule));
}

#[test]
fn theme_density_and_transcript_inset_change_semantic_block_geometry() {
    let previous = TranscriptBlock::Notice("previous".into());
    let current = TranscriptBlock::Notice("current".into());
    let render = |density: &str, inset: u16| {
        let theme = theme_with_layout(&format!(
            "density = \"{density}\"\ntranscript_inset = {inset}"
        ));
        let renderer = theme.rich_renderer();
        render_block(
            Some(&previous),
            &current,
            &theme,
            &renderer,
            &renderer,
            80,
            false,
        )
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>()
    };

    let compact = render("compact", 1);
    let comfortable = render("comfortable", 2);
    let airy = render("airy", 4);
    assert_eq!(compact.iter().take_while(|line| line.is_empty()).count(), 0);
    assert_eq!(
        comfortable
            .iter()
            .take_while(|line| line.is_empty())
            .count(),
        1
    );
    assert_eq!(airy.iter().take_while(|line| line.is_empty()).count(), 2);
    let dot = if theme_with_layout("").unicode() {
        "•"
    } else {
        "*"
    };
    assert!(compact[0].starts_with(&format!("{dot} current")));
    assert!(comfortable[1].starts_with(&format!("{dot} current")));
    assert!(airy[2].starts_with(&format!("  {dot} current")));

    let hidden_theme = theme_with_layout(
        "density = \"airy\"\nshow_reasoning = false\nnarrow_show_reasoning = false",
    );
    let hidden_renderer = hidden_theme.rich_renderer();
    let hidden_reasoning = TranscriptBlock::Reasoning(Box::new(
        AssistantBlock::finalized_reasoning("hidden".into()),
    ));
    let collapsed_reasoning = render_block(
        None,
        &hidden_reasoning,
        &hidden_theme,
        &hidden_renderer,
        &hidden_renderer,
        80,
        false,
    );
    assert_eq!(
        collapsed_reasoning.len(),
        0,
        "finished reasoning produces no collapsed lines when hidden: {collapsed_reasoning:?}"
    );
    let first_visible = render_block(
        Some(&hidden_reasoning),
        &current,
        &hidden_theme,
        &hidden_renderer,
        &hidden_renderer,
        80,
        false,
    );
    assert_eq!(
        first_visible
            .iter()
            .take_while(|line| line.is_empty())
            .count(),
        2
    );
}

#[test]
fn layout_breakpoint_is_resolved_from_terminal_width_before_inset() {
    let theme = theme_with_layout(
        r#"
                transcript_inset = 4
                narrow_breakpoint = 72
                show_reasoning = true
                narrow_show_reasoning = false
            "#,
    );
    let at_breakpoint = theme.layout_for_width(72);
    let below_breakpoint = theme.layout_for_width(71);
    assert!(
        !at_breakpoint.narrow && at_breakpoint.show_reasoning,
        "width == breakpoint stays on the wide layout"
    );
    assert!(
        below_breakpoint.narrow && !below_breakpoint.show_reasoning,
        "narrow fallbacks apply below the breakpoint before any inset"
    );
}

#[test]
fn selection_mapping_excludes_density_rows_and_transcript_inset() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_theme(theme_with_layout(
        "density = \"airy\"\ntranscript_inset = 4",
    ));
    {
        let mut state = shell.state.borrow_mut();
        state.push_block(TranscriptBlock::Assistant(Box::new(
            AssistantBlock::finalized("alpha".into()),
        )));
        state.push_block(TranscriptBlock::Assistant(Box::new(
            AssistantBlock::finalized("bravo".into()),
        )));
    }
    let second_start = {
        let state = shell.state.borrow();
        let _ = state.rendered_transcript(80);
        let second_start = state.transcript_cache.borrow().block_starts[1];
        second_start
    };
    assert!(selection_position_for_visual_cell(&shell.state.borrow(), second_start, 2).is_none());
    let start = selection_position_for_visual_cell(&shell.state.borrow(), second_start + 2, 4)
        .expect("first content cell should map");
    assert_eq!(start.block, 1);
    assert_eq!(start.offset, 0);
    let two_cells = selection_position_for_visual_cell(&shell.state.borrow(), second_start + 2, 6)
        .expect("content cell should map");
    assert_eq!(two_cells.offset, 2);
}

#[test]
fn card_geometry_keeps_prompt_identity_and_decorations_out_of_selection() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_theme(crate::tui::theme::test_theme_from_source(
        SURFACE_TEST_THEME,
    ));
    {
        let mut state = shell.state.borrow_mut();
        state.push_block(TranscriptBlock::User {
            text: "hello surface".into(),
            model_lab: Some(ModelLab::Alibaba),
            prompt_color: Some("#ff7018".into()),
            persisted: true,
        });
    }
    let (start, length, geometry, rows) = {
        let state = shell.state.borrow();
        let rows = state.rendered_transcript(80).clone();
        let cache = state.transcript_cache.borrow();
        (
            cache.block_starts[0],
            cache.block_lengths[0],
            cache.block_geometries[0],
            rows,
        )
    };
    assert_eq!(geometry.leading_rows, 2);
    assert_eq!(geometry.trailing_rows, 2);
    assert!(selection_position_for_visual_cell(&shell.state.borrow(), start, 0).is_none());
    assert!(
        selection_position_for_visual_cell(&shell.state.borrow(), start + length - 1, 79,)
            .is_none()
    );

    let body_row = start + geometry.transition_rows + geometry.leading_rows;
    let first = selection_position_for_visual_cell(
        &shell.state.borrow(),
        body_row,
        geometry.content_left + 2,
    )
    .expect("first prompt text cell");
    assert_eq!(first.offset, 0);
    let second = selection_position_for_visual_cell(
        &shell.state.borrow(),
        body_row,
        geometry.content_left + 3,
    )
    .expect("second prompt text cell");
    assert_eq!(second.offset, 1);
    let row_start = selection_position_for_visual_cell(&shell.state.borrow(), body_row, 0)
        .expect("left card margin should select the row start");
    let row_end = selection_position_for_visual_cell(&shell.state.borrow(), body_row, 79)
        .expect("right card margin should select the row end");
    assert_eq!(row_start.offset, 0);
    assert_eq!(row_end.offset, "hello surface".len());

    let body = &rows[body_row];
    assert!(
        strip_terminal_sequences(body).contains("› hello surface"),
        "{body:?}"
    );
    assert!(
        body.contains("\x1b[48;2;255;112;24m"),
        "the prompt card must retain its exact stored model background: {body:?}"
    );
    assert!(
        !body.contains("\x1b[48;2;17;34;51m"),
        "the neutral surface colour must not replace model provenance: {body:?}"
    );
    assert!(
        body.ends_with("\x1b[0m"),
        "surface background leaked: {body:?}"
    );

    {
        let mut state = shell.state.borrow_mut();
        state.transcript_selection = Some(TranscriptSelection {
            anchor: TranscriptPosition {
                block: 0,
                offset: 1,
                trailing_affinity: false,
            },
            focus: TranscriptPosition {
                block: 0,
                offset: 5,
                trailing_affinity: false,
            },
        });
        assert!(state.copy_buffer.is_none());
    }
    assert_eq!(shell.selected_plain_text().as_deref(), Some("ello"));
    assert!(shell.state.borrow().copy_buffer.is_none());
}

#[test]
fn card_surface_degrades_to_rail_with_exact_cached_narrow_geometry() {
    let theme = crate::tui::theme::test_theme_from_source(SURFACE_TEST_THEME);
    let block = TranscriptBlock::User {
        text: "narrow request".into(),
        model_lab: None,
        prompt_color: None,
        persisted: true,
    };
    let wide = compile_surface_plan(None, &block, &theme, 80);
    assert_eq!(wide.chrome, ThemeSurfaceChrome::Card);
    assert_eq!(wide.geometry.leading_rows, 2);
    assert_eq!(wide.geometry.trailing_rows, 2);

    let narrow = compile_surface_plan(None, &block, &theme, 40);
    assert_eq!(narrow.chrome, ThemeSurfaceChrome::Rail);
    assert_eq!(narrow.heading, ThemeSurfaceHeading::None);
    assert_eq!(narrow.geometry.leading_rows, 0);
    assert_eq!(narrow.geometry.trailing_rows, 0);
    let renderer = theme.rich_renderer();
    let rendered =
        render_block_planned(None, &block, &theme, &renderer, &renderer, 40, false, 0, 0);
    assert_eq!(rendered.geometry, narrow.geometry);
    let plain = rendered
        .lines
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>();
    assert!(
        plain.first().is_some_and(|line| line.contains("┃")),
        "{plain:?}"
    );
    assert!(plain
        .iter()
        .all(|line| !line.contains('╭') && !line.contains('╰')));
}

#[test]
fn card_background_and_glyphs_degrade_across_terminal_capabilities() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    use crate::tui::theme::TerminalBackground;

    let block = TranscriptBlock::Assistant(Box::new(AssistantBlock::finalized(
        "# Result\n\n```rust\nlet answer = 42;\n```".into(),
    )));
    let render = |capabilities, background| {
        let theme =
            crate::tui::theme::test_theme_source_with(SURFACE_TEST_THEME, capabilities, background);
        let renderer = theme.rich_renderer();
        render_block(None, &block, &theme, &renderer, &renderer, 72, false)
    };

    let truecolor = render(
        TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        TerminalBackground::Dark,
    );
    assert!(truecolor
        .iter()
        .any(|line| line.contains("\x1b[48;2;34;17;51m")));
    assert!(truecolor
        .iter()
        .filter(|line| !line.is_empty())
        .all(|line| line.ends_with("\x1b[0m")));

    let ansi = render(
        TerminalCapabilities::test(true, false, ColorDepth::Ansi16),
        TerminalBackground::Dark,
    );
    assert!(ansi
        .iter()
        .any(|line| line.contains("\x1b[4") || line.contains("\x1b[10")));
    assert!(ansi.iter().all(|line| !line.contains("48;2")));
    let ansi_plain = ansi
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>();
    assert!(ansi_plain
        .first()
        .is_some_and(|line| line.trim_start().ends_with("+")));

    let no_color = render(
        TerminalCapabilities::test(false, false, ColorDepth::None),
        TerminalBackground::Dark,
    );
    assert!(no_color.iter().all(|line| !line.contains('\x1b')));
    assert!(no_color
        .first()
        .is_some_and(|line| line.trim_start().ends_with("+")));

    let adaptive_source = SURFACE_TEST_THEME.replace("adaptive = false", "adaptive = true");
    let unknown = crate::tui::theme::test_theme_source_with(
        &adaptive_source,
        TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        TerminalBackground::Unknown,
    );
    let renderer = unknown.rich_renderer();
    let unknown = render_block(None, &block, &unknown, &renderer, &renderer, 72, false);
    assert!(
        unknown.iter().any(|line| line.contains("\x1b[48;")),
        "unknown terminal backgrounds must retain adaptive surfaces"
    );
}

#[test]
fn theme_header_footer_status_and_composer_padding_have_narrow_fallbacks() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 20);
    shell.set_identity("local", "qwen3.6-27b", "high");
    shell.set_theme(theme_with_layout(
        r#"
                show_header = true
                show_footer = false
                show_status_line = false
                composer_padding = 3
                narrow_breakpoint = 50
                narrow_show_header = false
                narrow_show_footer = false
                narrow_show_status_line = false
            "#,
    ));

    let now = Instant::now();
    let composer = plain_composer_surface(&shell, 80, now);
    assert_eq!(composer.len(), 3, "hidden footer leaves only the frame");
    assert!(composer[1].trim_start().starts_with('›'), "{composer:?}");
    assert!(composer[1].starts_with("  "), "{composer:?}");
    assert_eq!(visible_width(&composer[0]), 80);
    assert_eq!(visible_width(&composer[1]), 78);
    assert_eq!(visible_width(&composer[2]), 80);

    let wide_header = shell_chrome(&shell.state.borrow(), 80, now).header;
    assert_eq!(wide_header.len(), 1);
    let wide_header = strip_terminal_sequences(&wide_header[0]);
    assert!(wide_header.contains("octet"));
    assert!(
        wide_header.contains("local / Qwen3.6 27B"),
        "{wide_header:?}"
    );
    assert!(shell_chrome(&shell.state.borrow(), 40, now)
        .header
        .is_empty());
    let narrow_composer = plain_composer_surface(&shell, 40, now);
    assert_eq!(
        narrow_composer.len(),
        3,
        "extensions cannot force persistent header or footer chrome"
    );
}
