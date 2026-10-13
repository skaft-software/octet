//! Context budgeting, prompt rendering, local shell output, the steering queue and its previews,
//! and the composer grid at hostile widths. Separate because they assert composer geometry
//! against queued or streamed input rather than against a single keystroke.

use super::support::*;

use super::*;

#[test]
fn context_uses_single_turn_provider_total_not_cumulative_run_usage() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("openai", "gpt-5", "high");
    shell.set_context_estimate(80, 272_000);
    shell.begin_run("openai");
    let turn_usage = Usage {
        input_tokens: 10_000,
        cache_read_tokens: 200_000,
        output_tokens: 10_000,
        total_tokens: 220_000,
        ..Usage::default()
    };
    shell.on_agent_event(&AgentEvent::TurnFinished {
        turn_cost: None,
        message: octet_ai::AssistantMessage {
            content: vec![octet_ai::AssistantPart::Text("done".into())],
            model: ModelId("gpt-5".into()),
            protocol: octet_ai::Protocol::OpenAiResponses,
        },
        stop_reason: octet_ai::StopReason::EndTurn,
        turn_usage,
        usage: Usage {
            input_tokens: 20_000,
            cache_read_tokens: 370_000,
            output_tokens: 20_000,
            total_tokens: 410_000,
            ..Usage::default()
        },
        session_cost_microdollars: None,
        run_cost_microdollars: 0,
    });

    let state = shell.state.borrow();
    assert_eq!(state.last_turn_usage, Some(turn_usage));
    assert_eq!(state.run_context_estimate, Some((220_000, 272_000)));
    assert_eq!(state.context_estimate, Some((220_000, 272_000)));
}

#[test]
fn submitted_prompts_render_immediately_with_real_context_budget() {
    let mut shell = InteractiveShell::test_shell();
    shell.on_prompt_submitted("second prompt");
    shell.set_identity("deepseek", "deepseek-v4-pro", "high");
    shell.set_context_estimate(900_000, 967_232);
    let snapshot = shell.debug_snapshot();
    assert!(snapshot.contains("second prompt"));
    let rendered = render_shell(&shell.state.borrow(), 120);
    let footer = rendered.last().expect("single composer footer");
    assert!(
        strip_terminal_sequences(footer).contains("93%/967K"),
        "footer was {footer:?}"
    );
}

#[test]
fn running_local_shell_repaints_the_latest_output_tail_before_exit() {
    let mut shell = InteractiveShell::test_shell();
    let id = shell.append_shell_in_progress("long command".into());
    shell.update_shell_output(
        &id,
        (1..=8)
            .map(|line| format!("LIVE OUTPUT {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let rows = shell
        .state
        .borrow()
        .rendered_transcript(80)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>();
    let output = rows
        .iter()
        .find(|line| line.contains("LIVE OUTPUT 5"))
        .expect("first retained local-shell output");
    let nested = rows
        .iter()
        .find(|line| line.contains("4 earlier visual rows hidden"))
        .expect("local-shell output metadata");
    let elbow_byte = nested.find('└').expect("local-shell output elbow");
    let text_byte = output
        .find("LIVE OUTPUT 5")
        .expect("local-shell output text");
    assert_eq!(visible_width(&nested[..elbow_byte]), 2, "{rows:?}");
    assert_eq!(visible_width(&output[..text_byte]), 4, "{rows:?}");
    let rendered = rows.join("\n");
    assert!(!rendered.contains("LIVE OUTPUT 1"), "{rendered}");
    assert!(!rendered.contains("LIVE OUTPUT 4"), "{rendered}");
    assert!(rendered.contains("LIVE OUTPUT 5"), "{rendered}");
    assert!(
        rendered.contains("4 earlier visual rows hidden"),
        "{rendered}"
    );
    assert!(rendered.contains("LIVE OUTPUT 8"), "{rendered}");
}

#[test]
fn local_shell_commands_do_not_claim_a_model_prompt_color() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("openai", "gpt-5.6", "high");
    shell.on_local_command_submitted("!git status");
    let state = shell.state.borrow();
    let TranscriptBlock::User { prompt_color, .. } = &state.transcript[0] else {
        panic!("local command transcript row expected");
    };
    assert_eq!(prompt_color, &None);
    let rendered = render_block(
        None,
        &state.transcript[0],
        &state.theme,
        &state.theme.rich_renderer(),
        &state.theme.reasoning_renderer(),
        80,
        false,
    )
    .join("\n");
    assert!(!rendered.contains("\x1b[48;"), "{rendered:?}");
}

#[test]
fn steering_messages_are_queued_above_prompt_and_delivered_as_a_batch() {
    let mut shell = InteractiveShell::test_shell();
    shell.queue_steering(&ComposedInput::from_text("check the docs".into()));
    shell.queue_steering(&ComposedInput::from_text("then run the tests".into()));

    let rendered = render_shell(&shell.state.borrow(), 120);
    let prompt = rendered
        .iter()
        .position(|line| line.contains(CURSOR_MARKER))
        .expect("prompt line");
    let plain = rendered
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>();
    let queue = plain
        .iter()
        .position(|line| line.contains("Steering · 2 queued"))
        .expect("steering queue");
    let first = plain
        .iter()
        .position(|line| line.starts_with("  └ check the docs"))
        .expect("first steering message");
    let second = plain
        .iter()
        .position(|line| line.starts_with("  └ then run the tests"))
        .expect("second steering message");
    assert!(queue < first && first < second && second < prompt);
    assert!(!plain.iter().any(|line| line.contains("+1 more")));

    shell.on_agent_event(&AgentEvent::SteeringDelivered {
        messages: vec!["check the docs".into(), "then run the tests".into()],
    });
    let snapshot = shell.debug_snapshot();
    assert!(snapshot.contains("check the docs"));
    assert!(snapshot.contains("then run the tests"));
    assert!(!render_shell(&shell.state.borrow(), 120)
        .iter()
        .any(|line| line.contains("Steering ·")));
}

#[test]
fn queued_steering_uses_active_model_color() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("anthropic", "claude-sonnet-4", "high");
    shell.queue_steering(&ComposedInput::from_text("check the docs".into()));

    let state = shell.state.borrow();
    let model_color = state
        .theme
        .model_rgb(state.model_lab)
        .expect("active model color");
    let ui_color = state.theme.role_rgb("accent").expect("UI accent");
    assert_ne!(model_color, ui_color);
    let model_sequence = format!(
        "38;2;{};{};{}m",
        model_color.0, model_color.1, model_color.2
    );

    let rendered = input_overlays::render_pending_steering(&state, 80, 2);
    assert!(rendered[0].contains(&model_sequence), "{rendered:?}");
    assert!(rendered[1].contains(&model_sequence), "{rendered:?}");
}

#[test]
fn steering_preview_wraps_the_complete_message() {
    let mut shell = InteractiveShell::test_shell();
    shell.queue_steering(&ComposedInput::from_text(
        "i'm sending you a longer steering prompt just because i want to see how octet's tui handles showing this in the queued prompts area".into(),
    ));

    let rendered = input_overlays::render_pending_steering(&shell.state.borrow(), 71, 8)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>();

    assert!(rendered.len() > 2, "{rendered:?}");
    assert!(rendered[1].starts_with("  └ i'm sending"), "{rendered:?}");
    assert!(
        rendered.last().unwrap().contains("queued prompts area"),
        "{rendered:?}"
    );
    assert!(!rendered.join("\n").contains('…'));
    assert!(rendered.iter().all(|line| visible_width(line) <= 71));
}

#[test]
fn steering_messages_preserve_explicit_newlines() {
    let mut shell = InteractiveShell::test_shell();
    shell.queue_steering(&ComposedInput::from_text(
        "first line\nsecond 👩‍💻 line".into(),
    ));

    let rendered = input_overlays::render_pending_steering(&shell.state.borrow(), 40, 8)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>();

    assert_eq!(rendered.len(), 3, "{rendered:?}");
    assert!(rendered[1].contains("first line"), "{rendered:?}");
    assert!(rendered[2].contains("second 👩‍💻 line"), "{rendered:?}");
    assert!(rendered.iter().all(|line| visible_width(line) <= 40));
}

#[test]
fn steering_overflow_previews_first_prompt_and_counts_the_rest() {
    let mut shell = InteractiveShell::test_shell();
    shell.queue_steering(&ComposedInput::from_text(
        "first prompt has enough words to require several wrapped display rows".into(),
    ));
    shell.queue_steering(&ComposedInput::from_text(
        "second prompt also has enough words to require several wrapped rows".into(),
    ));

    let rendered = input_overlays::render_pending_steering(&shell.state.borrow(), 30, 5)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>();
    let joined = rendered.join("\n");

    assert_eq!(rendered.len(), 5, "{rendered:?}");
    assert!(joined.contains("└ first prompt"), "{rendered:?}");
    assert!(joined.contains("queued rows hidden"), "{rendered:?}");
    assert!(rendered.iter().all(|line| visible_width(line) <= 30));
}

#[test]
fn steering_overflow_reports_entirely_hidden_prompts() {
    let mut shell = InteractiveShell::test_shell();
    for index in 1..=5 {
        shell.queue_steering(&ComposedInput::from_text(format!("prompt {index}")));
    }

    let rendered = input_overlays::render_pending_steering(&shell.state.borrow(), 40, 4)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>();
    let joined = rendered.join("\n");

    assert_eq!(rendered.len(), 4, "{rendered:?}");
    assert!(joined.contains("└ prompt 1"), "{rendered:?}");
    assert!(joined.contains("└ prompt 2"), "{rendered:?}");
    assert!(joined.contains("3 queued rows hidden"), "{rendered:?}");
}

#[test]
fn mixed_pending_messages_keep_both_texts_and_fit_the_shell() {
    for (width, height) in [(20, 8), (40, 12), (80, 24), (120, 40)] {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(width, height);
        shell.queue_steering(&ComposedInput::from_text("steer one\nsteer two".into()));
        shell.queue_follow_up(ComposedInput::from_text("follow one\nfollow two".into()));
        let queued = input_overlays::render_pending_steering(&shell.state.borrow(), width, 20);
        let text = queued
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("steer two") && text.contains("follow two"),
            "{text}"
        );
        let frame = render_shell(&shell.state.borrow(), width);
        assert!(frame.len() <= usize::from(height));
        assert!(frame
            .iter()
            .all(|row| visible_width(row) <= usize::from(width)));
    }
}

#[test]
fn terminal_native_prompt_stays_within_every_viewport() {
    for (width, height) in [
        (1, 5),
        (2, 5),
        (3, 5),
        (4, 5),
        (8, 5),
        (12, 7),
        (24, 10),
        (40, 12),
        (60, 18),
        (80, 24),
        (120, 30),
        (160, 40),
    ] {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(width, height);
        for character in "a long prompt that must wrap cleanly at every width".chars() {
            shell.apply_edit(EditAction::Char(character));
        }

        let rendered = render_shell(&shell.state.borrow(), width);
        assert!(rendered.len() <= usize::from(height));
        assert!(
            rendered
                .iter()
                .all(|line| visible_width(line) <= usize::from(width)),
            "{width}x{height}: {rendered:?}"
        );
        assert!(!rendered.iter().any(|line| {
            line.chars()
                .any(|character| matches!(character, '┏' | '┓' | '┗' | '┛'))
        }));
        assert!(
            rendered.iter().any(|line| line.contains(CURSOR_MARKER)),
            "focused cursor missing at {width}x{height}: {rendered:?}"
        );
    }
}

#[test]
fn narrow_composer_clips_an_oversized_grapheme_without_losing_the_cursor() {
    for width in [3, 4] {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(width, 8);
        shell.state.borrow_mut().editor.set_text("界");

        let rendered = render_shell(&shell.state.borrow(), width);
        assert!(
            rendered
                .iter()
                .all(|line| visible_width(line) <= usize::from(width)),
            "{width}: {rendered:?}"
        );
        assert_eq!(
            rendered
                .iter()
                .map(|line| line.matches(CURSOR_MARKER).count())
                .sum::<usize>(),
            1,
            "{width}: {rendered:?}"
        );
        let cursor_line = rendered
            .iter()
            .find(|line| line.contains(CURSOR_MARKER))
            .expect("focused composer cursor");
        let cursor_byte = cursor_line.find(CURSOR_MARKER).expect("cursor marker byte");
        assert!(
            visible_width(&cursor_line[..cursor_byte]) < usize::from(width),
            "{width}: {cursor_line:?}"
        );
    }
}

#[test]
fn composer_geometry_keeps_full_rows_and_visual_edges_in_one_grid() {
    for (chrome, width) in [("boxed", 12), ("framed", 12), ("shaded", 12), ("boxed", 8)] {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(width, 12);
        let text_len = {
            let mut state = shell.state.borrow_mut();
            if chrome != "boxed" {
                state.theme.override_token("composer", chrome);
            }
            let geometry = crate::tui::composer_surface::composer_editor_geometry(&state, width);
            state.editor.set_text("x".repeat(geometry.text_width()));
            state.editor.text().len()
        };

        let rendered = render_shell(&shell.state.borrow(), width);
        let cursor_line = rendered
            .iter()
            .find(|line| line.contains(CURSOR_MARKER))
            .unwrap_or_else(|| panic!("{chrome} at {width} lost the cursor: {rendered:?}"));
        let cursor_byte = cursor_line.find(CURSOR_MARKER).expect("cursor marker byte");
        assert!(
            visible_width(&cursor_line[..cursor_byte]) < usize::from(width),
            "{chrome} at {width} placed the hardware cursor outside the viewport: {cursor_line:?}"
        );
        assert!(visible_width(cursor_line) <= usize::from(width));

        // The action path uses this same cached projected layout rather than a
        // raw-width approximation, so full rows still have a visual start/end.
        shell.apply_edit(EditAction::Home);
        assert_eq!(
            shell.state.borrow().editor.cursor(),
            0,
            "{chrome} at {width}"
        );
        shell.apply_edit(EditAction::End);
        assert_eq!(
            shell.state.borrow().editor.cursor(),
            text_len,
            "{chrome} at {width}"
        );
    }
}

#[test]
fn composer_visual_navigation_uses_the_same_safe_tab_and_control_projection_as_paint() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(8, 12);
    let tabbed = "   \tabcdef";
    shell.state.borrow_mut().editor.set_text(tabbed);
    shell.apply_edit(EditAction::Home);
    assert_eq!(
        shell.state.borrow().editor.cursor(),
        "   \ta".len(),
        "Home must target the displayed second row after a positional tab"
    );
    shell.apply_edit(EditAction::End);
    assert_eq!(shell.state.borrow().editor.cursor(), tabbed.len());

    let control_plus_combining = "\x1b\u{0301}x";
    {
        let mut state = shell.state.borrow_mut();
        state.editor.set_text(control_plus_combining);
        state.editor.set_cursor("\x1b".len());
    }
    let rendered = render_shell(&shell.state.borrow(), 20);
    let cursor_line = rendered
        .iter()
        .find(|line| line.contains(CURSOR_MARKER))
        .expect("composer cursor row");
    let safe_control = cursor_line.find("␛").expect("visualized ESC");
    let cursor = cursor_line.find(CURSOR_MARKER).expect("cursor marker");
    assert!(
        safe_control < cursor,
        "cursor split or preceded the joined visible control grapheme: {cursor_line:?}"
    );

    shell.apply_edit(EditAction::Home);
    assert_eq!(shell.state.borrow().editor.cursor(), 0);
    shell.apply_edit(EditAction::End);
    assert_eq!(
        shell.state.borrow().editor.cursor(),
        control_plus_combining.len()
    );
}

#[test]
fn native_hardware_cursor_stays_inside_full_composer_rows() {
    fn horizontal_positions(bytes: &[u8]) -> Vec<u16> {
        let mut positions = Vec::new();
        let mut start = 0;
        while start + 3 <= bytes.len() {
            if bytes[start..].starts_with(b"\x1b[") {
                let mut end = start + 2;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                if end > start + 2 && bytes.get(end) == Some(&b'G') {
                    let column = std::str::from_utf8(&bytes[start + 2..end])
                        .expect("CSI column is ASCII")
                        .parse::<u16>()
                        .expect("CSI column is numeric");
                    positions.push(column);
                }
            }
            start += 1;
        }
        positions
    }

    for (chrome, width) in [
        ("boxed", 1),
        ("boxed", 2),
        ("boxed", 12),
        ("framed", 12),
        ("shaded", 12),
        ("boxed", 8),
    ] {
        let mut theme = crate::tui::theme::test_theme();
        if chrome != "boxed" {
            theme.override_token("composer", chrome);
        }
        let (mut shell, bytes) = emulated_shell(theme, width, 12);
        shell
            .tui
            .as_mut()
            .expect("emulated shell owns a TUI")
            .set_show_hardware_cursor(true);
        let text_width = {
            let state = shell.state.borrow();
            crate::tui::composer_surface::composer_editor_geometry(&state, width).text_width()
        };
        shell
            .state
            .borrow_mut()
            .editor
            .set_text("x".repeat(text_width));
        shell.render();

        let output = bytes
            .lock()
            .expect("emulated terminal output mutex poisoned")
            .clone();
        let positions = horizontal_positions(&output);
        assert!(
            positions.iter().any(|column| *column > 0),
            "{chrome} at {width} did not position a hardware cursor: {output:?}"
        );
        assert!(
            positions.iter().all(|column| *column <= width),
            "{chrome} at {width} emitted an out-of-viewport cursor column: {positions:?}"
        );
    }
}

#[test]
fn composer_projection_cache_reuses_an_unchanged_large_draft_and_refreshes_on_changes() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 24);
    shell
        .state
        .borrow_mut()
        .editor
        .set_text("x".repeat(256 * 1024));

    let first_cache = {
        let state = shell.state.borrow();
        let geometry = crate::tui::composer_surface::composer_editor_geometry(&state, 80);
        let projection = state.composer_editor_projection(geometry);
        let expected_rows = (256_usize * 1024).div_ceil(geometry.text_width());
        assert_eq!(projection.line_count(), expected_rows);
        let _visible_row = projection.visible_row(projection.cursor_row(), CURSOR_MARKER, 80);
        let cache = state.composer_editor_cache.borrow();
        let entry = cache.as_ref().expect("projection populated the cache");
        assert_eq!(entry.text_width(), geometry.text_width());
        entry.source()
    };
    let second_cache = {
        let state = shell.state.borrow();
        let geometry = crate::tui::composer_surface::composer_editor_geometry(&state, 80);
        let _projection = state.composer_editor_projection(geometry);
        let cache_source = state
            .composer_editor_cache
            .borrow()
            .as_ref()
            .expect("projection retained the cache")
            .source();
        cache_source
    };
    assert_eq!(
        first_cache, second_cache,
        "unchanged draft rebuilt its layout"
    );

    shell.state.borrow_mut().editor.set_cursor(0);
    let cursor_cache = {
        let state = shell.state.borrow();
        let geometry = crate::tui::composer_surface::composer_editor_geometry(&state, 80);
        let projection = state.composer_editor_projection(geometry);
        assert_eq!(projection.cursor_row(), 0);
        assert!(
            projection
                .visible_row(0, CURSOR_MARKER, geometry.text_width())
                .starts_with(CURSOR_MARKER),
            "cursor-only refresh did not update the structured projection"
        );
        let source = state
            .composer_editor_cache
            .borrow()
            .as_ref()
            .expect("cursor refresh retained the cache")
            .source();
        source
    };
    assert_eq!(
        first_cache, cursor_cache,
        "cursor movement should retain the text-keyed display cache"
    );

    shell.apply_edit(EditAction::Char('y'));
    let refreshed_cache = {
        let state = shell.state.borrow();
        let geometry = crate::tui::composer_surface::composer_editor_geometry(&state, 80);
        let _projection = state.composer_editor_projection(geometry);
        let cache_source = state
            .composer_editor_cache
            .borrow()
            .as_ref()
            .expect("edit refreshed the cache")
            .source();
        cache_source
    };
    assert_ne!(
        first_cache, refreshed_cache,
        "edit did not refresh cached layout"
    );
}
