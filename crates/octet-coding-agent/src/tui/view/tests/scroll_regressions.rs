//! Scrolling regressions against the real shell renderer: the reader's viewport must not move
//! when new output arrives, and native scrollback must keep every committed row. Its own module
//! because each probe replays a full transcript through the production renderer.
use super::support::*;
use super::*;
use crate::tui::keymap::InputAction;

const WIDTH: u16 = 80;
const HEIGHT: u16 = 16;
const HISTORY: &str = "scroll-reading-row";

fn history_rows(lines: impl IntoIterator<Item = String>) -> Vec<(usize, String)> {
    lines
        .into_iter()
        .enumerate()
        .filter_map(|(row, line)| {
            let plain = strip_terminal_sequences(&line);
            plain
                .contains(HISTORY)
                .then(|| (row, plain.trim_end().to_owned()))
        })
        .collect()
}

// Read the navigation result without rendering (which used to be the only
// operation that captured an anchor). Events may arrive before that frame.
fn pending_history_rows(shell: &InteractiveShell) -> Vec<(usize, String)> {
    let state = shell.state.borrow();
    let chrome = shell_chrome(&state, WIDTH, Instant::now());
    let cache = state.transcript_cache.borrow();
    assert!(!cache.dirty);
    let viewport = TranscriptViewport::new(&chrome, &cache.lines, true);
    history_rows(
        cache.lines[viewport.history_window(state.scroll_from_bottom.get())]
            .iter()
            .cloned(),
    )
}

fn rendered_history_rows(shell: &InteractiveShell) -> Vec<(usize, String)> {
    history_rows(render_shell_viewport_at(
        &shell.state.borrow(),
        WIDTH,
        Instant::now(),
    ))
}

// Appending a mutable tail row must not change the first semantic history row
// or the order of the overlapping part of the reader's window.
fn assert_history_prefix(actual: &[(usize, String)], expected: &[(usize, String)]) {
    assert!(!actual.is_empty() && !expected.is_empty());
    let overlap = actual.len().min(expected.len());
    assert_eq!(&actual[..overlap], &expected[..overlap]);
}

fn roster(count: usize, state: &str) -> octet_agent::ExtensionPresentationSnapshot {
    serde_json::from_value(serde_json::json!({
        "revision": count + 1,
        "status": {"state": "active", "label": "Subagents"},
        "activities": (0..count).map(|index| serde_json::json!({
            "id": format!("worker-{index}"),
            "kind": "subagent",
            "state": state,
            "summary": format!("worker-{index} using read"),
            "metrics": {"tool_calls": index + 1, "input_tokens": 1000, "output_tokens": 50}
        })).collect::<Vec<_>>(),
        "actions": []
    }))
    .unwrap()
}

fn worker_history_shell() -> InteractiveShell {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(WIDTH, HEIGHT);
    shell.begin_run("openai");
    assert!(shell.set_subagent_presentation(Some(&roster(1, "running")), true));
    for index in 0..100 {
        shell.notice(format!("{HISTORY}-{index:03}"));
    }
    let _ = render_shell_viewport_at(&shell.state.borrow(), WIDTH, Instant::now());
    shell
}

#[test]
fn first_scroll_reveals_one_older_row_and_returns_to_live_without_resizing() {
    let mut shell = worker_history_shell();
    let capacity = {
        let state = shell.state.borrow();
        let chrome = shell_chrome(&state, WIDTH, Instant::now());
        transcript_viewport_capacity(chrome.transcript_rows + chrome.working.len(), false)
    };
    let render = |shell: &InteractiveShell| {
        render_shell_viewport_at(&shell.state.borrow(), WIDTH, Instant::now())
            .into_iter()
            .take(capacity)
            .collect::<Vec<_>>()
    };
    let live = render(&shell);
    shell.scroll_lines(-1);
    let scrolled = render(&shell);
    assert!(capacity > 1);
    assert_eq!(scrolled.len(), live.len());
    assert_eq!(&scrolled[1..], &live[..live.len() - 1]);
    assert_eq!(shell.state.borrow().scroll_from_bottom.get(), 1);
    shell.scroll_lines(1);
    assert_eq!(render(&shell), live);
    assert!(shell.state.borrow().follow_tail);
}

fn wheel_step(shell: &mut InteractiveShell, upward: bool) {
    use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};

    let action = shell.translate_input(
        Some(Event::Mouse(MouseEvent {
            kind: if upward {
                MouseEventKind::ScrollUp
            } else {
                MouseEventKind::ScrollDown
            },
            column: 20,
            row: 1,
            modifiers: KeyModifiers::NONE,
        })),
        false,
    );
    let direction = if upward { -1 } else { 1 };
    assert_eq!(action, InputAction::ScrollLines(direction));
    shell.scroll_lines(direction);
}

fn wheel_history_shell(height: u16, threaded: bool, tool_tail: bool) -> (InteractiveShell, RunId) {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(WIDTH, height);
    shell.capture_mouse = true;
    shell.state.borrow_mut().application_viewport_requested = true;
    for index in 0..100 {
        shell.notice(format!("{HISTORY}-{index:03}"));
    }
    let run = shell.begin_run("openai");
    if tool_tail {
        let id = ToolCallId("wheel-live-tool".into());
        shell.on_run_event(
            run,
            &AgentEvent::ToolStarted {
                id: id.clone(),
                name: "bash".into(),
                args: serde_json::json!({"command": "audit\n".repeat(12)}),
            },
        );
        shell.on_run_event(
            run,
            &AgentEvent::ToolProgress {
                id,
                progress: ToolProgress::Output {
                    stream: octet_agent::OutputStream::Stdout,
                    bytes: bytes::Bytes::from("retained tool output\n".repeat(16)),
                },
            },
        );
    } else {
        shell.on_run_event(
            run,
            &AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: (0..40)
                    .map(|index| format!("stream-row-{index:03}\n\n"))
                    .collect(),
            },
        );
    }
    if threaded {
        shell.isolate_native_test_renderer();
    }
    shell.render_written_test_frame();
    (shell, run)
}

#[test]
fn small_upward_wheel_steps_survive_repaints_near_the_tail() {
    for threaded in [false, true] {
        for height in [8, 10, 16, 32] {
            for tool_tail in [false, true] {
                let (mut shell, _) = wheel_history_shell(height, threaded, tool_tail);
                // Small gestures stop at one, two and three rows from the tail.
                // Each line event is followed by two paints, like the recorded snap.
                for target in 1..=3 {
                    wheel_step(&mut shell, true);
                    assert_eq!(shell.state.borrow().scroll_from_bottom.get(), target);
                    for paint in 0..2 {
                        shell.render_written_test_frame();
                        let state = shell.state.borrow();
                        assert_eq!(
                            state.scroll_from_bottom.get(), target,
                            "threaded={threaded}, height={height}, tool_tail={tool_tail}, target={target}, paint={paint}",
                        );
                        assert!(!state.follow_tail);
                    }
                }
                for remaining in (0..3).rev() {
                    wheel_step(&mut shell, false);
                    shell.render_written_test_frame();
                    let state = shell.state.borrow();
                    assert_eq!(state.scroll_from_bottom.get(), remaining);
                    assert_eq!(state.follow_tail, remaining == 0);
                }
            }
        }
    }
}

#[test]
fn small_upward_wheel_steps_hold_position_through_streaming_and_live_heights() {
    for height in [8, 10, 16, 32] {
        for steps in 1..=3 {
            let (mut shell, run) = wheel_history_shell(height, true, true);
            shell.set_verbose_tools(true);
            shell.render_written_test_frame();
            for _ in 0..steps {
                wheel_step(&mut shell, true);
            }
            shell.render_written_test_frame();
            assert_eq!(shell.state.borrow().scroll_from_bottom.get(), steps);
            let start = shell
                .state
                .borrow()
                .retained_render_geometry()
                .unwrap()
                .visible_start;
            for (tick, count) in [4, 1, 8, 2].into_iter().enumerate() {
                // Text inserts before Working; reasoning changes its height.
                // Expanded rosters grow while running and contract on settlement.
                for event in [
                    AgentEvent::OutputDelta {
                        channel: OutputChannel::Text,
                        text: format!("\n\nnewer streamed row {tick}"),
                    },
                    AgentEvent::OutputDelta {
                        channel: OutputChannel::Reasoning,
                        text: "\n\n### Inspecting the next result\n".into(),
                    },
                ] {
                    shell.on_run_event(run, &event);
                    let running = tick % 2 == 0;
                    let status = if running { "running" } else { "succeeded" };
                    shell.set_subagent_presentation(Some(&roster(count, status)), running);
                    for paint in 0..2 {
                        shell.render_written_test_frame();
                        let state = shell.state.borrow();
                        let geometry = state.retained_render_geometry().unwrap();
                        // Tail growth changes the bottom-relative distance, not
                        // the row the reader chose. Every later paint must agree.
                        let expected = geometry.total_rows - geometry.viewport_rows - start;
                        assert_eq!(
                            state.scroll_from_bottom.get(),
                            expected,
                            "height={height}, steps={steps}, tick={tick}, paint={paint}"
                        );
                        assert_eq!(geometry.visible_start, start);
                        assert!(!state.follow_tail);
                    }
                }
            }
            let distance = shell.state.borrow().scroll_from_bottom.get();
            for remaining in (0..distance).rev() {
                wheel_step(&mut shell, false);
                shell.render_written_test_frame();
                let state = shell.state.borrow();
                assert_eq!(state.scroll_from_bottom.get(), remaining);
                assert_eq!(state.follow_tail, remaining == 0);
            }
        }
    }
}

#[test]
fn live_tail_follows_streamed_output() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(WIDTH, HEIGHT);
    for index in 0..100 {
        shell.notice(format!("{HISTORY}-{index:03}"));
    }
    let run = shell.begin_run("openai");
    for index in 0..12 {
        let marker = format!("live-stream-marker-{index:03}");
        shell.on_run_event(
            run,
            &AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: format!("\n\n{marker}"),
            },
        );
        let rows = render_shell_viewport_at(&shell.state.borrow(), WIDTH, Instant::now());
        assert!(
            rows.iter().any(|row| row.contains(&marker)),
            "{marker}: {rows:?}"
        );
        let state = shell.state.borrow();
        assert!(state.follow_tail);
        assert_eq!(state.scroll_from_bottom.get(), 0);
    }
}

#[test]
fn semantic_scroll_holds_anchor_through_tool_and_model_lifecycle() {
    use octet_agent::ToolOutput;

    // Both captured wheel navigation and PageUp with native selection retained
    // must use the same semantic viewport. This is not a native-wheel test.
    for application_viewport in [false, true] {
        let (mut shell, bytes) = emulated_shell_with_mode(
            crate::tui::theme::test_theme(),
            WIDTH,
            HEIGHT,
            true,
            application_viewport,
        );
        shell.tui.as_mut().unwrap().set_show_hardware_cursor(true);
        for index in 0..100 {
            shell.notice(format!("{HISTORY}-{index:03}"));
        }
        let run = shell.begin_run("openai");
        shell.render();
        if application_viewport {
            shell.scroll_lines(-8);
        } else {
            shell.scroll(-1);
        }
        let expected = pending_history_rows(&shell);
        assert!(!expected.is_empty());
        assert!(shell.state.borrow().viewport_anchor.get().is_some());
        assert_eq!(shell.capture_mouse, application_viewport);

        let id = ToolCallId("scroll-tool".into());
        let events = [
            AgentEvent::ToolStarted {
                id: id.clone(),
                name: "bash".into(),
                args: serde_json::json!({"command": "long-running audit ".repeat(12)}),
            },
            AgentEvent::ToolProgress {
                id: id.clone(),
                progress: ToolProgress::Output {
                    stream: octet_agent::OutputStream::Stdout,
                    bytes: bytes::Bytes::from("live tool row\n".repeat(16)),
                },
            },
            AgentEvent::ToolFinished {
                id,
                result: Ok(ToolOutput::new("done")),
                duration: Duration::from_millis(10),
            },
            AgentEvent::OutputDelta {
                channel: OutputChannel::Reasoning,
                text: "### Next step\n\nInspect the remaining changes.".into(),
            },
            AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: "Assistant response with wrapping. ".repeat(60),
            },
            AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: "\n\nAnother paragraph. ".repeat(40),
            },
        ];
        let mut terminal = vt100::Parser::new(HEIGHT, WIDTH, 1024);
        let mut full_redraws = None;
        let mut lengths = Vec::new();
        for (step, event) in events.iter().enumerate() {
            shell.on_run_event(run, event);
            // In production the first tool admission can win the renderer's
            // lock before the navigation frame has ever been painted.
            shell.render();
            let output = std::mem::take(&mut *bytes.lock().unwrap());
            process_vt100_with_saved_line_clear(&mut terminal, &output, HEIGHT, WIDTH, 1024);
            assert_eq!(
                history_rows(terminal.screen().rows(0, WIDTH)),
                expected,
                "step {step}, application_viewport={application_viewport}"
            );
            assert!(!shell.state.borrow().follow_tail);
            let redraws = shell.tui.as_ref().unwrap().full_redraws();
            if let Some(previous) = full_redraws {
                assert_eq!(redraws, previous, "tool updates must not replay history");
                assert!(!output.windows(4).any(|window| window == b"\x1b[3J"));
            }
            full_redraws = Some(redraws);
            lengths.push(shell.state.borrow().transcript_cache.borrow().lines.len());
        }
        assert!(lengths[1] > lengths[0], "progress must add visible rows");
        assert!(lengths[2] < lengths[1], "completion must contract the tool");
        assert!(shell.state.borrow().new_output_count > 0);

        // Editing does not steal the history reader; submission and explicit
        // live navigation still do, without changing mouse ownership.
        shell.apply_edit(EditAction::Char('x'));
        assert!(!shell.state.borrow().follow_tail);
        let maximum_pages = shell.state.borrow().transcript_cache.borrow().lines.len() + 1;
        for page in 0..maximum_pages {
            if shell.state.borrow().follow_tail {
                break;
            }
            let previous = shell.state.borrow().scroll_from_bottom.get();
            shell.scroll(1);
            let state = shell.state.borrow();
            assert!(
                state.follow_tail || state.scroll_from_bottom.get() < previous,
                "PageDown must advance toward live: page={page}, previous={previous}, current={}, anchor={:?}, application_viewport={application_viewport}",
                state.scroll_from_bottom.get(),
                state.viewport_anchor.get(),
            );
        }
        assert!(shell.state.borrow().follow_tail, "PageDown must reach live");
        assert_eq!(shell.pending(), "x");
        assert!(shell.state.borrow().viewport_anchor.get().is_none());
        assert_eq!(shell.state.borrow().new_output_count, 0);
        shell.scroll_lines(-8);
        shell.jump_to_tail();
        assert!(shell.state.borrow().follow_tail);
        shell.scroll_lines(-8);
        shell.on_prompt_submitted("next prompt");
        assert!(shell.state.borrow().follow_tail);
        assert!(shell.state.borrow().viewport_anchor.get().is_none());
        assert_eq!(shell.capture_mouse, application_viewport);
    }
}

#[test]
fn semantic_scroll_navigation_round_trips_markdown_spacing() {
    for page in [false, true] {
        for paint_navigation in [false, true] {
            let mut shell = InteractiveShell::test_shell();
            shell.set_size(WIDTH, HEIGHT);
            let mut markdown = (0..40)
                .map(|index| format!("{HISTORY}-{index:03}"))
                .collect::<Vec<_>>()
                .join("\n\n");
            // A boxed table paints several rows for one semantic line, and a
            // table taller than the viewport keeps such non-round-tripping rows
            // under the anchor as the reader scrolls through it.
            markdown.push_str("\n\n| head | value |\n| --- | --- |\n");
            for index in 0..20 {
                markdown.push_str(&format!("| {HISTORY}-table-{index:03} | {index} |\n"));
            }
            shell
                .state
                .borrow_mut()
                .push_block(TranscriptBlock::Assistant(Box::new(
                    AssistantBlock::finalized(markdown),
                )));
            let _ = rendered_history_rows(&shell);
            let maximum = max_scroll_from_bottom(&shell.state.borrow(), WIDTH);
            let amount = if page { usize::from(HEIGHT / 2) } else { 1 };
            let mut corrected_anchor_seen = false;
            for direction in [-1, 1] {
                for step in 0..=maximum {
                    let previous = shell.state.borrow().scroll_from_bottom.get();
                    let expected = if direction < 0 {
                        previous.saturating_add(amount).min(maximum)
                    } else {
                        previous.saturating_sub(amount)
                    };
                    if page {
                        shell.scroll(direction);
                    } else {
                        shell.scroll_lines(direction);
                    }
                    assert_eq!(
                        shell.state.borrow().scroll_from_bottom.get(), expected,
                        "navigation step={step}, direction={direction}, page={page}, paint={paint_navigation}",
                    );
                    if paint_navigation {
                        let _ = rendered_history_rows(&shell);
                        assert_eq!(
                            shell.state.borrow().scroll_from_bottom.get(), expected,
                            "painting must not undo navigation: step={step}, direction={direction}, page={page}",
                        );
                    }
                    let state = shell.state.borrow();
                    assert_eq!(state.follow_tail, expected == 0);
                    corrected_anchor_seen |= state
                        .viewport_anchor
                        .get()
                        .is_some_and(|anchor| anchor.semantic_row_correction != 0);
                    if expected == if direction < 0 { maximum } else { 0 } {
                        break;
                    }
                }
                assert_eq!(
                    shell.state.borrow().scroll_from_bottom.get(),
                    if direction < 0 { maximum } else { 0 },
                );
            }
            assert!(
                corrected_anchor_seen,
                "fixture must exercise non-round-tripping copy rows"
            );
            assert!(shell.state.borrow().viewport_anchor.get().is_none());
        }
    }
}

#[test]
fn semantic_scroll_captures_history_before_worker_growth_without_a_frame() {
    for page in [false, true] {
        let mut shell = worker_history_shell();
        if page {
            shell.scroll(-1);
        } else {
            shell.scroll_lines(-8);
        }
        let expected = pending_history_rows(&shell);
        assert!(!expected.is_empty());
        let old_length = shell.state.borrow().transcript_cache.borrow().lines.len();
        assert!(shell.set_subagent_presentation(Some(&roster(8, "running")), true));
        assert_history_prefix(&rendered_history_rows(&shell), &expected);
        assert_eq!(
            shell.state.borrow().transcript_cache.borrow().lines.len(),
            old_length
        );
        assert!(shell_chrome(&shell.state.borrow(), WIDTH, Instant::now())
            .subagents
            .is_empty());
        assert_eq!(
            shell
                .state
                .borrow()
                .rendered_transcript(WIDTH)
                .iter()
                .filter(|line| line.contains("Subagents"))
                .count(),
            1
        );
        assert!(shell.set_subagent_presentation(Some(&roster(1, "succeeded")), false));
        assert_history_prefix(&rendered_history_rows(&shell), &expected);
        assert!(!shell.state.borrow().follow_tail);
    }
}

#[test]
fn semantic_scroll_rebases_pending_roster_reflow_before_next_navigation() {
    for page in [false, true] {
        let mut pending = worker_history_shell();
        let mut painted = worker_history_shell();
        for shell in [&mut pending, &mut painted] {
            shell.scroll_lines(-24);
            assert!(!rendered_history_rows(shell).is_empty());
        }
        for (count, direction) in [(8, -1), (1, 1), (6, -1)] {
            let snapshot = roster(count, "running");
            assert!(pending.set_subagent_presentation(Some(&snapshot), true));
            assert!(painted.set_subagent_presentation(Some(&snapshot), true));
            // Only the reference gets a renderer frame before the next input.
            let _ = rendered_history_rows(&painted);
            for shell in [&mut pending, &mut painted] {
                if page {
                    shell.scroll(direction);
                } else {
                    shell.scroll_lines(direction);
                }
            }
            assert_eq!(
                rendered_history_rows(&pending),
                rendered_history_rows(&painted),
                "count={count}, direction={direction}, page={page}"
            );
            assert!(!pending.state.borrow().follow_tail);
        }
    }
}

#[test]
fn semantic_scroll_keeps_reasoning_anchor_when_first_roster_appears() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(WIDTH, HEIGHT);
    let run = shell.begin_run("openai");
    shell.toggle_thinking();
    shell.on_run_event(
        run,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: (0..100)
                .map(|index| format!("{HISTORY}-{index:03}"))
                .collect::<Vec<_>>()
                .join("\n\n"),
        },
    );
    let _ = rendered_history_rows(&shell);
    shell.scroll_lines(-24);
    let expected = rendered_history_rows(&shell);
    assert!(!expected.is_empty());
    let anchor = shell.state.borrow().viewport_anchor.get().unwrap();
    assert!(shell.set_subagent_presentation(Some(&roster(4, "running")), true));
    assert_history_prefix(&rendered_history_rows(&shell), &expected);
    let state = shell.state.borrow();
    let current = state.viewport_anchor.get().unwrap();
    assert_eq!(current.commit_id, anchor.commit_id);
    assert_eq!(current.text_offset, anchor.text_offset);
    assert!(shell_chrome(&state, WIDTH, Instant::now())
        .subagents
        .is_empty());
    let cache = state.transcript_cache.borrow();
    assert_eq!(
        cache
            .lines
            .iter()
            .filter(|line| line.contains("Subagents"))
            .count(),
        1
    );
    assert_eq!(cache.block_starts.len(), state.transcript.len());
}

#[test]
fn worker_roster_stays_at_the_live_tail_while_reading_history() {
    let mut shell = worker_history_shell();
    // The active roster is one transcript tail row, not composer chrome.
    let live = rendered_history_rows(&shell);
    assert!(!live.is_empty());
    shell.scroll_lines(-24);
    let expected = rendered_history_rows(&shell);
    assert!(!expected.is_empty());
    assert!(
        shell.state.borrow().viewport_anchor.get().is_some(),
        "scrolling back anchors the reader's window"
    );

    // A live roster update repaints the tail without returning the reader to
    // it or moving the semantic history anchor.
    assert!(shell.set_subagent_presentation(Some(&roster(8, "running")), true));
    assert!(!shell.state.borrow().follow_tail);
    let after = rendered_history_rows(&shell);
    assert_history_prefix(&after, &expected);
    let chrome = shell_chrome(&shell.state.borrow(), WIDTH, Instant::now());
    assert!(chrome.subagents.is_empty());
    assert_eq!(
        shell
            .state
            .borrow()
            .rendered_transcript(WIDTH)
            .iter()
            .filter(|line| line.contains("/subagents"))
            .count(),
        1
    );
    assert!(chrome.transcript_rows >= 1);

    shell.scroll_lines(i16::MAX);
    assert!(shell.state.borrow().follow_tail);
    let state = shell.state.borrow();
    assert!(shell_chrome(&state, WIDTH, Instant::now())
        .subagents
        .is_empty());
    let cache = state.transcript_cache.borrow();
    assert_eq!(
        cache
            .lines
            .iter()
            .filter(|line| line.contains("Subagents"))
            .count(),
        1
    );
}
