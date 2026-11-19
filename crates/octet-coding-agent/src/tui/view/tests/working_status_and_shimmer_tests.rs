//! The working indicator, its shimmer clock, and the collapsed thinking animation. Separate
//! because they assert animation phase over time rather than static layout.

use super::*;

#[test]
fn active_run_starts_with_working_until_reasoning_is_observed() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("codex", "gpt-5.3-codex-spark", "high");
    shell.begin_run("codex");
    let rendered = rendered_with_working(&shell.state.borrow(), 80)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>();
    assert_eq!(rendered.len(), 3, "{rendered:?}");
    assert!(rendered[0].starts_with("• Working ("), "{rendered:?}");
}

#[test]
fn working_chrome_clocks_stay_addressable_independently_of_saved_history() {
    for source in [
        None,
        Some("[surfaces.reasoning]\nchrome = \"card\"\npadding = 1"),
    ] {
        let theme = source.map_or_else(
            crate::tui::theme::test_theme,
            crate::tui::theme::test_theme_from_source,
        );
        let mut shell = InteractiveShell::test_shell_with_theme(theme);
        shell.notice("history");
        shell.begin_run("openai");
        let mut state = shell.state.borrow_mut();
        let revisions = state.block_revisions.clone();
        for threaded in [false, true] {
            state.render_threaded = threaded;
            state.native_animation_viewport_top.set(Some(usize::MAX));
            assert!(state.active_reasoning.is_none());
            assert!(state.has_active_status_shimmer());
            assert!(state.has_active_status_timer());
            state.advance_status_shimmer();
            state.advance_status_timer();
            assert_eq!(
                state.block_revisions, revisions,
                "chrome must not dirty history"
            );
            assert!(shell_chrome(&state, 80, Instant::now())
                .working
                .iter()
                .any(|row| strip_terminal_sequences(row).contains("Working")));
        }
    }
}

#[test]
fn max_and_ultra_working_rainbow_fades_for_two_seconds_only() {
    assert_eq!(
        status_rainbow_strength_at(Some("max"), Some(Duration::ZERO)),
        100
    );
    assert_eq!(
        status_rainbow_strength_at(Some("ultra"), Some(Duration::from_millis(500))),
        75
    );
    assert_eq!(
        status_rainbow_strength_at(Some("max"), Some(Duration::from_secs(1))),
        50
    );
    assert_eq!(
        status_rainbow_strength_at(Some("max"), Some(Duration::from_millis(1_500))),
        25
    );
    assert_eq!(
        status_rainbow_strength_at(Some("max"), Some(Duration::from_secs(2))),
        0
    );
    assert_eq!(
        status_rainbow_strength_at(Some("high"), Some(Duration::ZERO)),
        0
    );
    assert_eq!(status_rainbow_strength_at(Some("ultra"), None), 0);
}

#[test]
fn activity_shimmer_clock_can_cross_long_labels() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("codex", "gpt-5.3-codex-spark", "high");
    shell.begin_run("codex");
    let mut state = shell.state.borrow_mut();
    assert!(state.has_active_status_shimmer());
    for frame in 1..=48 {
        state.advance_status_shimmer();
        assert_eq!(state.status_shimmer_frame, frame);
    }
}

/// `Working` and `Thinking` share one shimmer: the same sweep, on the same
/// monotonic phase, continuing across the transition between them.
///
/// The clock used to be gated on the pre-delta `Working` row alone, so the
/// first reasoning delta froze it and `Thinking` rendered a stalled sweep.
#[test]
fn collapsed_thinking_shimmers_on_the_shared_working_phase() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("codex", "gpt-5.3-codex-spark", "high");
    let run_id = shell.begin_run("codex");

    let row = |shell: &InteractiveShell, label: &str| {
        rendered_with_working(&shell.state.borrow(), 80)
            .iter()
            .find(|line| strip_terminal_sequences(line).contains(label))
            .cloned()
            .unwrap_or_else(|| panic!("{label} status row"))
    };
    let marker_prefix = |line: &str| {
        let marker = line.find('•').expect("reasoning margin marker");
        line[..marker + '•'.len_utf8()].to_owned()
    };

    // Before any delta the row is `Working` and the clock is running.
    let working = row(&shell, "Working");
    assert!(
        strip_terminal_sequences(&working).starts_with("• Working ("),
        "{working:?}"
    );
    {
        let mut state = shell.state.borrow_mut();
        assert!(state.has_active_status_shimmer());
        state.advance_status_shimmer_by(4);
    }
    let working = row(&shell, "Working");

    // Thinking grows above the independent Working slot; both keep the clock.
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "private trace".into(),
        },
    );
    let thinking = row(&shell, "Thinking");
    assert!(
        strip_terminal_sequences(&thinking).starts_with("• Thinking ("),
        "{thinking:?}"
    );
    {
        let mut state = shell.state.borrow_mut();
        assert!(
            state.has_active_status_shimmer(),
            "Thinking must keep the shared status clock running"
        );
        let before = state.status_shimmer_frame;
        state.advance_status_shimmer_by(1);
        assert_eq!(
            state.status_shimmer_frame,
            before + 1,
            "the phase must advance instead of freezing"
        );
    }
    let thinking = row(&shell, "Thinking");

    // Both labels paint a foreground-only sweep that shares the one phase, and
    // the margin marker stays a solid glyph on either row.
    assert_ne!(thinking, working, "the Thinking label must sweep");
    assert!(
        !thinking.contains("\x1b[48;"),
        "status shimmer must stay foreground-only: {thinking:?}"
    );
    assert!(
        !thinking.contains("\x1b[2m"),
        "the sweep must not dim the label: {thinking:?}"
    );
    // The marker is a single solid glyph on both labels; it is not part of the
    // label's text run and never gains a background or a dim attribute.
    for (label, line) in [("Working", &working), ("Thinking", &thinking)] {
        let marker = marker_prefix(line);
        assert!(marker.ends_with('•'), "{label} marker: {marker:?}");
        assert!(
            !marker.contains("\x1b[48;") && !marker.contains("\x1b[2m"),
            "{label} marker must stay a plain foreground glyph: {marker:?}"
        );
    }
}

#[test]
fn compaction_uses_a_timer_without_shimmer() {
    for width in [46, 80] {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(width, 24);
        shell.set_run_label("compacting");
        let before = shell.state.borrow().rendered_transcript(width).join("\n");
        {
            let mut state = shell.state.borrow_mut();
            assert!(!renderer_runtime::status_shimmer_animating(&state));
            assert!(renderer_runtime::status_timer_active(&state));
            assert!(!state.has_active_status_shimmer());
            state.advance_status_shimmer();
        }
        let after = shell.state.borrow().rendered_transcript(width).join("\n");
        assert_eq!(
            strip_terminal_sequences(&before),
            strip_terminal_sequences(&after)
        );
        assert_eq!(before, after, "compaction labels must not shimmer");
        shell.set_run_label("idle");
        assert!(!shell.state.borrow().has_active_status_shimmer());
    }
}

/// The `Working` sweep enters from before the margin dot, so the dot shares the
/// label's frame: it lights as the band crosses it and then returns to its
/// parked colour. That pulse is what makes the dot read as blinking with the
/// other activity markers instead of sitting still for the whole run.
#[test]
fn working_activity_shimmer_crosses_its_margin_dot() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("codex", "gpt-5.3-codex-spark", "high");
    shell.begin_run("codex");

    let raw = |shell: &InteractiveShell| {
        rendered_with_working(&shell.state.borrow(), 80)
            .iter()
            .find(|line| strip_terminal_sequences(line).contains("Working"))
            .cloned()
            .expect("working status row")
    };
    let marker_prefix = |line: &str| {
        let marker = line.find('•').expect("working margin marker");
        line[..marker + '•'.len_utf8()].to_owned()
    };
    let label_after_marker = |line: &str| line[marker_prefix(line).len()..].to_owned();

    // Frame 0 parks the sweep before the dot, so this is the dot at rest.
    let resting = marker_prefix(&raw(&shell));
    let resting_label = label_after_marker(&raw(&shell));
    let mut lit = false;
    let mut rested_after_pulse = false;
    let mut label_moved = false;
    // A `Working` cycle is 19 classic frames and 12 physical ones, so 38 frames
    // observe the pulse and its return to rest on either field.
    for _ in 0..38 {
        {
            let mut state = shell.state.borrow_mut();
            assert!(state.has_active_status_shimmer());
            state.advance_status_shimmer();
        }
        let line = raw(&shell);
        let marker = marker_prefix(&line);
        if marker == resting {
            rested_after_pulse = lit;
        } else {
            lit = true;
        }
        if label_after_marker(&line) != resting_label {
            label_moved = true;
        }
        assert!(marker.ends_with('•'), "the dot keeps its glyph: {marker:?}");
        assert!(
            !marker.contains("\x1b[48;") && !marker.contains("\x1b[2m"),
            "the dot must stay a plain foreground glyph: {marker:?}"
        );
        assert!(
            !line.contains("\x1b[48;"),
            "status shimmer must stay foreground-only"
        );
    }
    assert!(lit, "the sweep must cross the Working margin dot");
    assert!(
        rested_after_pulse,
        "the dot must return to its rest colour once the sweep has passed"
    );
    assert!(label_moved, "the Working label still shimmers");
}

#[test]
fn collapsed_reasoning_uses_a_margin_dot_without_an_expanded_content_bullet() {
    let theme = crate::tui::theme::test_theme();
    let renderer = theme.rich_renderer();
    let args = serde_json::json!({"path":"README.md"});
    let tool = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("read".into()),
        "read".into(),
        args.to_string(),
        summarize_tool("read", &args),
        String::new(),
        true,
        false,
        None,
        None,
    )));
    let reasoning_block = |expanded: bool| {
        TranscriptBlock::Reasoning(Box::new(
            AssistantBlock::streaming_reasoning("private detail")
                .with_reasoning_expanded(expanded)
                .with_model_lab(Some(ModelLab::OpenAi)),
        ))
    };
    let TranscriptBlock::Reasoning(mut slot) = reasoning_block(false) else {
        unreachable!()
    };
    slot.status_activity = true;
    slot.thinking_slot = true;
    let reasoning = TranscriptBlock::Reasoning(slot);
    let expanded_reasoning = reasoning_block(true);
    let plain = |lines: Vec<String>| {
        lines
            .into_iter()
            .map(|line| strip_terminal_sequences(&line))
            .collect::<Vec<_>>()
    };
    let tool_lines = plain(render_block(
        None, &tool, &theme, &renderer, &renderer, 80, false,
    ));
    let reasoning_lines = plain(render_block(
        Some(&tool),
        &reasoning,
        &theme,
        &renderer,
        &renderer,
        80,
        false,
    ));
    let expanded_reasoning_lines = plain(render_block(
        Some(&tool),
        &expanded_reasoning,
        &theme,
        &renderer,
        &renderer,
        80,
        false,
    ));
    let tool_line = tool_lines
        .iter()
        .find(|line| line.contains("Read"))
        .expect("read row");
    let reasoning_line = reasoning_lines
        .iter()
        .find(|line| line.contains("Thinking"))
        .expect("reasoning row");
    assert!(
        reasoning_lines
            .iter()
            .any(|line| line.contains("└ (Ctrl+T to expand)")),
        "{reasoning_lines:?}"
    );
    let visual_column = |line: &str, needle: &str| {
        line.find(needle)
            .map(|offset| visible_width(&line[..offset]))
    };
    assert!(tool_line.starts_with("• "), "{tool_line:?}");
    assert!(
        reasoning_line.starts_with("• "),
        "collapsed reasoning must carry the blinking event dot: {reasoning_line:?}"
    );
    let expanded_reasoning_line = expanded_reasoning_lines
        .iter()
        .find(|line| line.contains("private detail"))
        .expect("expanded reasoning row");
    assert!(
        expanded_reasoning_line.starts_with("  private detail"),
        "expanded reasoning must retain its gutter without a dot or content bullet: {expanded_reasoning_line:?}"
    );
    assert_eq!(
        visual_column(tool_line, "Read"),
        visual_column(reasoning_line, "Thinking"),
        "reasoning labels must align with tool labels: {tool_line:?} vs {reasoning_line:?}"
    );
    assert!(
        !reasoning_line.contains('└'),
        "the heading row must not contain the detail connector: {reasoning_line:?}"
    );
}

#[test]
fn thinking_adds_detail_to_the_same_live_transcript_item_above_queued_prompts() {
    use octet_agent::{EntryId, FinishReason};

    let mut shell = InteractiveShell::test_shell();
    shell
        .state
        .borrow_mut()
        .theme
        .set_duration_preference(Some(true));
    shell.set_identity("codex", "gpt-5.3-codex-spark", "high");
    let run_id = shell.begin_run("codex");
    let frame = |shell: &InteractiveShell| {
        let state = shell.state.borrow();
        let mut lines = state.rendered_transcript(80).clone();
        builtin_shell_chrome::append_chrome(
            &mut lines,
            shell_chrome(&state, 80, Instant::now()),
            1,
        );
        lines
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .collect::<Vec<_>>()
    };
    let composer_row = |lines: &[String]| {
        lines
            .iter()
            .position(|line| line.trim_start().starts_with('›'))
            .unwrap_or_else(|| panic!("composer row: {lines:#?}"))
    };
    let count =
        |lines: &[String], label: &str| lines.iter().filter(|line| line.contains(label)).count();

    let working = frame(&shell);
    assert_eq!(count(&working, "• Working ("), 1, "{working:#?}");
    assert_eq!(count(&working, "Thinking"), 0, "{working:#?}");

    // Thinking replaces Working and fills the reserved detail row, with the run
    // timer and interrupt hint shown once.
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "**Assessing TUI assertion**\n\nprivate trace".into(),
        },
    );
    let thinking = frame(&shell);
    let status = thinking
        .iter()
        .position(|line| line.contains("• Thinking ("))
        .unwrap_or_else(|| panic!("thinking status: {thinking:#?}"));
    assert!(
        thinking[status].contains("Esc to interrupt"),
        "{thinking:#?}"
    );
    assert!(
        thinking[status + 1].contains("Assessing TUI assertion (Ctrl+T to expand)"),
        "{thinking:#?}"
    );
    assert_eq!(count(&thinking, "Thinking"), 1, "{thinking:#?}");
    assert_eq!(count(&thinking, "Working"), 0, "{thinking:#?}");
    assert_eq!(count(&thinking, "Esc to interrupt"), 1, "{thinking:#?}");
    assert!(!thinking.join("\n").contains("private trace"));
    assert_eq!(
        composer_row(&thinking),
        composer_row(&working),
        "the reserved Thinking detail must not move the composer"
    );

    // Set a deterministic phase duration before public output closes the trace.
    {
        let mut state = shell.state.borrow_mut();
        let index = state.active_reasoning.unwrap();
        let TranscriptBlock::Reasoning(reasoning) = &mut state.transcript[index] else {
            unreachable!()
        };
        reasoning.reasoning_started_at = Some(Instant::now() - Duration::from_secs(15));
    }
    // Output resumes: Working returns and only a settled marker remains.
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "Answer".into(),
        },
    );
    shell.queue_steering(&ComposedInput::from_text("hi".into()));
    let answered = frame(&shell);
    assert_eq!(count(&answered, "Thinking"), 0, "{answered:#?}");
    assert_eq!(count(&answered, "• Working ("), 1, "{answered:#?}");
    assert_eq!(count(&answered, "• Thought for 15s"), 1, "{answered:#?}");
    let thought_row = answered
        .iter()
        .position(|line| line.contains("• Thought for 15s"));
    let answer_row = answered.iter().position(|line| line.contains("Answer"));
    assert!(thought_row.unwrap() < answer_row.unwrap(), "{answered:#?}");
    assert_eq!(count(&answered, "private trace"), 0, "{answered:#?}");
    assert_eq!(
        count(&answered, "Assessing TUI assertion"),
        0,
        "{answered:#?}"
    );
    // Queued prompts sit between the status slot and the composer.
    let working_row = answered
        .iter()
        .position(|line| line.contains("• Working ("))
        .unwrap();
    let steering_row = answered
        .iter()
        .position(|line| line.contains("Steering"))
        .unwrap_or_else(|| panic!("steering: {answered:#?}"));
    assert!(working_row < steering_row, "{answered:#?}");
    assert!(steering_row < composer_row(&answered), "{answered:#?}");
    assert_eq!(steering_row, working_row + 3, "{answered:#?}");

    shell.on_run_event(
        run_id,
        &AgentEvent::RunFinished {
            head: EntryId("head".into()),
            reason: FinishReason::Completed,
        },
    );
    let finished = frame(&shell);
    assert_eq!(count(&finished, "• Working ("), 0, "{finished:#?}");
    assert_eq!(count(&finished, "• Thought for 15s"), 1, "{finished:#?}");
    shell.set_thinking_expanded(true);
    let expanded = frame(&shell);
    assert_eq!(count(&expanded, "private trace"), 1, "{expanded:#?}");
    assert_eq!(count(&expanded, "Thought for"), 0, "{expanded:#?}");
    shell.set_thinking_expanded(false);
    let collapsed = frame(&shell);
    assert_eq!(count(&collapsed, "• Thought for 15s"), 1, "{collapsed:#?}");
    assert_eq!(count(&collapsed, "private trace"), 0, "{collapsed:#?}");
}
