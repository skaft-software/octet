//! The working indicator, its shimmer clock, and the collapsed thinking animation. Separate
//! because they assert animation phase over time rather than static layout.

use super::*;

#[test]
fn active_run_starts_with_working_until_reasoning_is_observed() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("codex", "gpt-5.3-codex-spark", "high");
    shell.begin_run("codex");
    let rendered = shell
        .state
        .borrow()
        .rendered_transcript(80)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>();
    assert_eq!(rendered.len(), 1, "{rendered:?}");
    assert!(
        rendered[0].starts_with("• Working (0s • esc to interrupt)"),
        "{rendered:?}"
    );
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
        shell
            .state
            .borrow()
            .rendered_transcript(80)
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

    // The first reasoning delta turns the same row into `Thinking`. The clock
    // must keep running, and the label must move.
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

#[test]
fn working_activity_shimmers_only_the_label_not_its_margin_dot() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("codex", "gpt-5.3-codex-spark", "high");
    shell.begin_run("codex");

    let raw = |shell: &InteractiveShell| {
        shell
            .state
            .borrow()
            .rendered_transcript(80)
            .iter()
            .find(|line| strip_terminal_sequences(line).contains("Working"))
            .cloned()
            .expect("working status row")
    };
    let marker_prefix = |line: &str| {
        let marker = line.find('•').expect("working margin marker");
        line[..marker + '•'.len_utf8()].to_owned()
    };
    let before = raw(&shell);
    {
        let mut state = shell.state.borrow_mut();
        assert!(state.has_active_status_shimmer());
        state.advance_status_shimmer_by(8);
    }
    let after = raw(&shell);

    assert_eq!(marker_prefix(&after), marker_prefix(&before));
    assert_ne!(after, before, "the Working label still shimmers");
    assert!(
        !after.contains("\x1b[48;"),
        "status shimmer must stay foreground-only"
    );
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
    let reasoning = TranscriptBlock::Reasoning(Box::new(
        AssistantBlock::streaming_reasoning("private detail")
            .with_model_lab(Some(ModelLab::OpenAi)),
    ));
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
        &reasoning,
        &theme,
        &renderer,
        &renderer,
        80,
        true,
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
        reasoning_line.contains("Ctrl+O expand"),
        "{reasoning_line:?}"
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
        "compiled hint must stay inline: {reasoning_line:?}"
    );
}
