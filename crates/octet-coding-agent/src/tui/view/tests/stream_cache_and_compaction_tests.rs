//! Streaming block caching, diff classification, and compaction disclosure. Separate because
//! they assert that a streaming run repaints only the block suffix that actually changed.

use super::support::*;

use super::*;

#[test]
fn streaming_prose_diff_classification_never_searches_the_growing_first_line() {
    use super::tool_render::take_diff_classification_line_bytes;

    let shell = InteractiveShell::test_shell();
    let mut state = shell.state.borrow_mut();
    state.push_block(TranscriptBlock::Assistant(Box::new(
        AssistantBlock::streaming("word xyz "),
    )));
    take_diff_classification_line_bytes();
    for _ in 0..1000 {
        let TranscriptBlock::Assistant(assistant) = &mut state.transcript[0] else {
            unreachable!()
        };
        assistant.append("word xyz ");
        state.touch_block(0);
        let _ = state.rendered_transcript(80);
    }
    assert_eq!(take_diff_classification_line_bytes(), 0);
    assert!(looks_like_diff(
        "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new"
    ));
    assert!(take_diff_classification_line_bytes() > 0);
}

#[test]
fn diff_prefix_rejection_preserves_first_nonblank_line_semantics() {
    fn reference(text: &str) -> bool {
        let mut lines = text.lines().map(str::trim_start);
        let Some(first) = lines.find(|line| !line.is_empty()) else {
            return false;
        };
        first.starts_with("diff --git ")
            || (first.starts_with("--- ")
                && lines.any(|line| line.starts_with("+++ "))
                && text.lines().any(|line| line.trim_start().starts_with("@@")))
    }
    for prefix in ["", " ", "\n\t", "\r\n  ", "\u{85}\n\u{2003}"] {
        for first in [
            "",
            "normal prose",
            "diff --git ",
            "--- a/file",
            "---",
            "```diff",
        ] {
            for suffix in [
                "",
                "\n+++ b/file",
                "\n@@",
                "\n+++ b/file\n@@",
                "\r\n@@\r\n+++ b/file",
                "\n```diff\n--- a/file\n+++ b/file\n@@",
            ] {
                let text = format!("{prefix}{first}{suffix}");
                assert_eq!(looks_like_diff(&text), reference(&text), "{text:?}");
            }
        }
    }
}

#[test]
fn streaming_assistant_cache_replaces_only_the_mutable_block_suffix() {
    const WIDTH: u16 = 80;
    let shell = InteractiveShell::test_shell();
    {
        let mut state = shell.state.borrow_mut();
        state.push_block(TranscriptBlock::Assistant(Box::new(
            AssistantBlock::streaming("# Stable heading\n\nmutable"),
        )));
        let _ = state.rendered_transcript(WIDTH);
    }

    let (block_start, old_lines) = {
        let state = shell.state.borrow();
        let cache = state.transcript_cache.borrow();
        (cache.block_starts[0], cache.lines.clone())
    };
    {
        let mut state = shell.state.borrow_mut();
        let TranscriptBlock::Assistant(assistant) = &mut state.transcript[0] else {
            unreachable!()
        };
        assistant.append(" tail");
        state.touch_block(0);
        let _ = state.rendered_transcript(WIDTH);
    }

    let state = shell.state.borrow();
    let cache = state.transcript_cache.borrow();
    assert!(cache.last_update_start > block_start);
    assert_eq!(
        &cache.lines[..cache.last_update_start],
        &old_lines[..cache.last_update_start],
        "parser-committed rows were rebuilt"
    );
    let expected = render_block(
        None,
        &TranscriptBlock::Assistant(Box::new(AssistantBlock::streaming(
            "# Stable heading\n\nmutable tail",
        ))),
        &state.theme,
        &state.theme.rich_renderer(),
        &state.theme.reasoning_renderer(),
        WIDTH,
        false,
    );
    let start = cache.block_starts[0];
    let end = start + cache.block_lengths[0];
    assert_eq!(&cache.lines[start..end], expected);
}

#[test]
fn streamed_delta_marks_only_its_changed_cached_block() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_theme(crate::tui::theme::test_theme_from_source(
        SURFACE_TEST_THEME,
    ));
    for number in 0..500 {
        shell.notice(format!("historic {number}"));
    }
    shell.on_agent_event(&AgentEvent::OutputDelta {
        channel: OutputChannel::Text,
        text: "first".into(),
    });
    let _ = render_shell(&shell.state.borrow(), 120);
    let assistant_index = shell
        .state
        .borrow()
        .active_text
        .expect("active assistant block");

    // Keep a later block in the layout so this exercises the splice/start
    // adjustments as well as the no-history-scan dirty path.
    shell.notice("later block");
    shell.on_agent_event(&AgentEvent::OutputDelta {
        channel: OutputChannel::Text,
        text: " second".into(),
    });
    {
        let state = shell.state.borrow();
        let cache = state.transcript_cache.borrow();
        assert_eq!(cache.dirty_blocks, vec![assistant_index]);
    }

    let rendered = render_shell(&shell.state.borrow(), 120).join("\n");
    assert!(rendered.contains("first second"));
    assert!(rendered.contains("later block"));
    assert!(shell
        .state
        .borrow()
        .transcript_cache
        .borrow()
        .dirty_blocks
        .is_empty());
    {
        let state = shell.state.borrow();
        let cache = state.transcript_cache.borrow();
        assert_eq!(cache.block_geometries.len(), state.transcript.len());
        assert_eq!(cache.block_geometries[assistant_index].leading_rows, 1);
        assert_eq!(cache.block_geometries[assistant_index].trailing_rows, 1);
        let later = assistant_index + 1;
        assert_eq!(
            cache.block_starts[later],
            cache.block_starts[assistant_index] + cache.block_lengths[assistant_index]
        );
    }
}

#[test]
fn hidden_reasoning_stream_does_not_grow_native_scrollback() {
    const WIDTH: u16 = 64;
    const HEIGHT: u16 = 10;
    let (mut shell, bytes) = emulated_shell(crate::tui::theme::test_theme(), WIDTH, HEIGHT);
    let mut terminal = vt100::Parser::new(HEIGHT, WIDTH, 512);
    let drain = |bytes: &Arc<Mutex<Vec<u8>>>| {
        std::mem::take(&mut *bytes.lock().expect("emulated terminal bytes"))
    };
    terminal.process(&drain(&bytes));
    terminal.set_scrollback(usize::MAX);
    let baseline_scrollback = terminal.screen().scrollback();
    terminal.set_scrollback(0);

    let run_id = shell.begin_run("openai");
    for index in 0..160 {
        shell.on_run_event(
            run_id,
            &AgentEvent::OutputDelta {
                channel: OutputChannel::Reasoning,
                text: format!("private sentinel {index}\n"),
            },
        );
        shell.render();
    }
    terminal.process(&drain(&bytes));
    terminal.set_scrollback(usize::MAX);
    assert_eq!(
        terminal.screen().scrollback(),
        baseline_scrollback,
        "collapsed streaming reasoning must not commit mutable rows"
    );
    terminal.set_scrollback(0);
    let visible = terminal.screen().contents();
    assert!(visible.contains("Thinking"), "{visible:?}");
    assert!(!visible.contains("Working"), "{visible:?}");
    assert!(visible.contains("Ctrl+O expand"), "{visible:?}");
    assert!(!visible.contains("private sentinel"), "{visible:?}");
    let state = shell.state.borrow();
    let TranscriptBlock::Reasoning(reasoning) = state.transcript.last().unwrap() else {
        panic!("reasoning block expected");
    };
    assert!(reasoning.text.contains("private sentinel 159"));
}

#[test]
fn streamed_assistant_rows_enter_native_scrollback_once() {
    const WIDTH: u16 = 96;
    const HEIGHT: u16 = 48;
    let (mut shell, bytes) = emulated_shell(crate::tui::theme::test_theme(), WIDTH, HEIGHT);
    let mut terminal = vt100::Parser::new(HEIGHT, WIDTH, 512);
    let drain = |bytes: &Arc<Mutex<Vec<u8>>>| {
        std::mem::take(&mut *bytes.lock().expect("emulated terminal bytes"))
    };
    terminal.process(&drain(&bytes));

    let run_id = shell.begin_run("openai");
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "private reasoning sentinel".into(),
        },
    );
    shell.render();
    terminal.process(&drain(&bytes));
    let mut response = String::from("# Stream report\n\n## Findings\n\n");
    for index in 0..48 {
        response.push_str(&format!(
            "- **stream-sentinel-{index:02}**: detailed finding for row {index}\n"
        ));
        if index == 15 {
            response.push_str("\n## Nested concerns\n\n");
        } else if index == 31 {
            response.push_str("\n## Final checks\n\n");
        }
    }
    let response_chars = response.chars().collect::<Vec<_>>();
    for chunk in response_chars.chunks(7) {
        shell.state.borrow_mut().advance_event_dot_animation();
        shell.render();
        terminal.process(&drain(&bytes));
        shell.on_run_event(
            run_id,
            &AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: chunk.iter().collect(),
            },
        );
        shell.render();
        terminal.process(&drain(&bytes));
    }

    // Grow the parser's viewport before looking back so its public
    // contents API can expose the complete retained history at once.
    terminal.set_size(512, WIDTH);
    terminal.set_scrollback(usize::MAX);
    let physical = terminal.screen().contents();

    for index in 0..48 {
        let sentinel = format!("stream-sentinel-{index:02}");
        assert_eq!(
            physical.matches(&sentinel).count(),
            1,
            "{sentinel} was duplicated in native scrollback:\n{physical}"
        );
    }
}

#[test]
fn pi_renderer_keeps_streamed_transcript_complete_while_a_draft_is_open() {
    const WIDTH: u16 = 72;
    const HEIGHT: u16 = 12;
    let (mut shell, bytes) = emulated_shell(crate::tui::theme::test_theme(), WIDTH, HEIGHT);
    let drain = |bytes: &Arc<Mutex<Vec<u8>>>| {
        std::mem::take(&mut *bytes.lock().expect("emulated terminal bytes"))
    };
    let mut terminal = vt100::Parser::new(HEIGHT, WIDTH, 512);
    process_vt100_with_saved_line_clear(&mut terminal, &drain(&bytes), HEIGHT, WIDTH, 512);

    for index in 0..12 {
        shell.notice(format!("DRAFT-HISTORY-{index:02}"));
    }
    shell.apply_edit(EditAction::Char('x'));
    shell.render();
    process_vt100_with_saved_line_clear(&mut terminal, &drain(&bytes), HEIGHT, WIDTH, 512);

    let run_id = shell.begin_run("openai");
    for index in 0..32 {
        shell.on_run_event(
            run_id,
            &AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: format!(
                    "DRAFT-STREAM-{index:02} stays present while the mutable composer remains open.\n\n"
                ),
            },
        );
        shell.render();
        process_vt100_with_saved_line_clear(&mut terminal, &drain(&bytes), HEIGHT, WIDTH, 512);
    }

    terminal.set_size(256, WIDTH);
    terminal.set_scrollback(usize::MAX);
    let physical = terminal.screen().contents();
    for index in 0..12 {
        let sentinel = format!("DRAFT-HISTORY-{index:02}");
        assert_eq!(
            physical.matches(&sentinel).count(),
            1,
            "{sentinel}:\n{physical}"
        );
    }
    for index in 0..32 {
        let sentinel = format!("DRAFT-STREAM-{index:02}");
        assert_eq!(
            physical.matches(&sentinel).count(),
            1,
            "{sentinel}:\n{physical}"
        );
    }
}

#[test]
fn ctrl_o_expands_and_collapses_the_inline_compaction_summary() {
    let mut shell = InteractiveShell::test_shell();
    shell.compaction_marker(
        "Context compacted · 12,000 input tokens summarized",
        "# Grounded summary\n\n- kept decision\n- **summary sentinel**",
    );
    let plain = |shell: &InteractiveShell| {
        strip_terminal_sequences(&shell.state.borrow().rendered_transcript(80).join("\n"))
    };

    let collapsed = plain(&shell);
    assert!(
        collapsed.contains("12,000 input tokens summarized"),
        "{collapsed}"
    );
    assert!(collapsed.contains("ctrl+o to view"), "{collapsed}");
    assert!(!collapsed.contains("summary sentinel"), "{collapsed}");

    shell.toggle_disclosure();
    let expanded = plain(&shell);
    assert!(expanded.contains("Grounded summary"), "{expanded}");
    assert!(expanded.contains("summary sentinel"), "{expanded}");
    assert!(expanded.contains("ctrl+o to collapse"), "{expanded}");
    assert!(!shell.has_overlay(), "compaction must expand inline");

    shell.toggle_disclosure();
    let collapsed_again = plain(&shell);
    assert!(
        !collapsed_again.contains("summary sentinel"),
        "{collapsed_again}"
    );
    assert!(
        collapsed_again.contains("ctrl+o to view"),
        "{collapsed_again}"
    );
}

#[test]
fn autonomous_compaction_events_show_work_success_and_failure_inline() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("openai", "gpt-5.6", "high");
    let run_id = shell.begin_run("openai");
    shell.on_run_event(
        run_id,
        &AgentEvent::CompactionStarted {
            reason: octet_agent::CompactionReason::Threshold,
        },
    );
    let footer = strip_terminal_sequences(
        &crate::tui::composer_surface::render_composer_surface(
            &shell.state.borrow(),
            80,
            Instant::now() + Duration::from_secs(1),
        )
        .join("\n"),
    );
    assert!(!footer.contains("Working"), "{footer}");
    assert!(!footer.contains("compacting"), "{footer}");
    let compacting =
        strip_terminal_sequences(&shell.state.borrow().rendered_transcript(80).join("\n"));
    assert!(compacting.contains("• Compacting context"), "{compacting}");
    assert!(!compacting.contains("ctrl+o"), "{compacting}");

    shell.on_run_event(
        run_id,
        &AgentEvent::CompactionFinished {
            reason: octet_agent::CompactionReason::Threshold,
            result: Ok(octet_agent::CompactionInfo {
                kind: octet_agent::CompactionKind::Local,
                summary: "# Automatic summary\n\nauto-summary sentinel".into(),
                first_kept: octet_agent::EntryId("kept".into()),
                usage: octet_ai::Usage::default(),
                elapsed: Duration::ZERO,
                cost_microdollars: None,
            }),
        },
    );
    let collapsed =
        strip_terminal_sequences(&shell.state.borrow().rendered_transcript(80).join("\n"));
    assert!(
        collapsed.contains("Context compacted automatically"),
        "{collapsed}"
    );
    assert!(!collapsed.contains("Compacting context"), "{collapsed}");
    assert!(!collapsed.contains("auto-summary sentinel"), "{collapsed}");
    shell.toggle_disclosure();
    let expanded =
        strip_terminal_sequences(&shell.state.borrow().rendered_transcript(80).join("\n"));
    assert!(expanded.contains("auto-summary sentinel"), "{expanded}");

    let mut native_shell = InteractiveShell::test_shell();
    let native_run = native_shell.begin_run("openai");
    native_shell.on_run_event(
        native_run,
        &AgentEvent::CompactionFinished {
            reason: octet_agent::CompactionReason::Threshold,
            result: Ok(octet_agent::CompactionInfo {
                kind: octet_agent::CompactionKind::NativeResponses {
                    checkpoint: octet_agent::EntryId("checkpoint".into()),
                    covered_through: octet_agent::EntryId("covered".into()),
                },
                summary: String::new(),
                first_kept: octet_agent::EntryId("covered".into()),
                usage: octet_ai::Usage::default(),
                elapsed: Duration::ZERO,
                cost_microdollars: None,
            }),
        },
    );
    let native = strip_terminal_sequences(
        &native_shell
            .state
            .borrow()
            .rendered_transcript(80)
            .join("\n"),
    );
    assert!(native.contains("Context compacted natively"), "{native}");
    assert!(native.contains("opaque Responses state"), "{native}");
    assert!(native.contains("retained"), "{native}");
    assert!(!native.contains("checkpoint"), "{native}");

    let mut failed_shell = InteractiveShell::test_shell();
    let failed_run = failed_shell.begin_run("openai");
    failed_shell.on_run_event(
        failed_run,
        &AgentEvent::CompactionStarted {
            reason: octet_agent::CompactionReason::Overflow,
        },
    );
    failed_shell.on_run_event(
        failed_run,
        &AgentEvent::CompactionFinished {
            reason: octet_agent::CompactionReason::Overflow,
            result: Err("cold endpoint timed out".into()),
        },
    );
    assert_eq!(
        failed_shell.debug_error().as_deref(),
        Some("automatic compaction failed: cold endpoint timed out")
    );
    assert!(failed_shell.state.borrow().run_label.is_empty());
}

#[test]
fn resumed_compaction_summary_remains_expandable_after_theme_switch() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    let first_kept = session
        .append(EntryValue::Config {
            model: Some("gpt-5.6".into()),
            reasoning: Some("high".into()),
            reasoning_mode: None,
        })
        .unwrap();
    session
        .append(EntryValue::Compaction {
            summary: "# Resumed summary\n\nresume-only sentinel".into(),
            first_kept,
            active_skills: Vec::new(),
            skill_resources: Vec::new(),
            details: Default::default(),
            snapcompact: None,
        })
        .unwrap();
    drop(session);

    let resumed = Session::open(path).unwrap();
    let mut shell = InteractiveShell::test_shell();
    shell.show_overlay_text("stale session overlay".into());
    shell.hydrate(&resumed).unwrap();
    assert!(
        !shell.has_overlay(),
        "resume must close session-local overlays"
    );
    let render = |shell: &InteractiveShell| {
        strip_terminal_sequences(&shell.state.borrow().rendered_transcript(72).join("\n"))
    };
    assert!(!render(&shell).contains("resume-only sentinel"));

    shell.toggle_disclosure();
    assert!(render(&shell).contains("resume-only sentinel"));
    shell.set_theme(crate::tui::theme::test_theme());
    let restyled = render(&shell);
    assert!(restyled.contains("resume-only sentinel"), "{restyled}");
    assert!(restyled.contains("ctrl+o to collapse"), "{restyled}");
}

#[test]
fn compaction_disclosure_preserves_native_presentation() {
    const WIDTH: u16 = 88;
    const HEIGHT: u16 = 18;
    for synchronized_output in [false, true] {
        let (mut shell, bytes) = emulated_shell_with_sync(
            crate::tui::theme::test_theme(),
            WIDTH,
            HEIGHT,
            synchronized_output,
        );
        let mut terminal = vt100::Parser::new(HEIGHT, WIDTH, 512);
        let drain = |bytes: &Arc<Mutex<Vec<u8>>>| {
            std::mem::take(&mut *bytes.lock().expect("emulated terminal bytes"))
        };
        terminal.process(&drain(&bytes));

        for index in 0..24 {
            shell.notice(format!("compaction-history-{index:02}"));
        }
        let summary = format!(
            "# Summary\n\n{}",
            (0..40)
                .map(|index| format!("- compaction-detail-{index:02}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        shell.compaction_marker("Context compacted", summary);
        shell.render();
        terminal.process(&drain(&bytes));

        shell.toggle_disclosure();
        shell.render();
        let expansion = drain(&bytes);
        let expansion_text = String::from_utf8_lossy(&expansion);
        assert!(
            !expansion
                .windows(b"\x1b[3J".len())
                .any(|bytes| bytes == b"\x1b[3J"),
            "Pi can repaint a disclosure that begins inside the visible viewport: {expansion_text:?}"
        );
        assert!(
            !expansion_text.contains("compaction-history-00"),
            "visible-tail differential update replayed off-screen history: {expansion_text:?}"
        );

        terminal.process(&expansion);
        terminal.set_scrollback(0);
        let visible = terminal.screen().contents();
        assert!(visible.contains("compaction-detail-39"), "{visible}");
        assert!(
            visible
                .lines()
                .any(|line| line == default_composer_rule(WIDTH)),
            "composer disappeared: {visible}"
        );

        shell.toggle_disclosure();
        shell.render();
        let collapse = drain(&bytes);
        let collapse_text = String::from_utf8_lossy(&collapse);
        assert!(
            collapse
                .windows(b"\x1b[3J".len())
                .any(|bytes| bytes == b"\x1b[3J"),
            "Pi parity requires contraction above the viewport to clear and replay: {collapse_text:?}"
        );
        assert!(collapse_text.contains("compaction-history-00"));
        process_vt100_with_saved_line_clear(&mut terminal, &collapse, HEIGHT, WIDTH, 512);
        terminal.set_scrollback(0);
        let collapsed = terminal.screen().contents();
        assert!(collapsed.contains("ctrl+o to view"), "{collapsed}");
        assert!(!collapsed.contains("compaction-detail-"), "{collapsed}");
        assert!(
            collapsed
                .lines()
                .any(|line| line == default_composer_rule(WIDTH)),
            "composer disappeared: {collapsed}"
        );

        terminal.set_size(512, WIDTH);
        terminal.set_scrollback(usize::MAX);
        let physical = terminal.screen().contents();
        for index in 0..24 {
            let sentinel = format!("compaction-history-{index:02}");
            assert_eq!(
                physical.matches(&sentinel).count(),
                1,
                "{sentinel} was lost or duplicated with synchronized_output={synchronized_output}:\n{physical}"
            );
        }
    }
}
