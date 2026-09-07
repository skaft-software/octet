use super::*;

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
    let scroll = state.scroll_from_bottom.get();
    let capacity = transcript_viewport_capacity(chrome.transcript_rows, scroll > 0);
    let end = cache.lines.len().saturating_sub(scroll);
    history_rows(
        cache.lines[end.saturating_sub(capacity)..end]
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
        while !shell.state.borrow().follow_tail {
            shell.scroll(1);
        }
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
        assert_eq!(rendered_history_rows(&shell), expected);
        assert!(shell.state.borrow().transcript_cache.borrow().lines.len() > old_length);
        assert!(shell.set_subagent_presentation(Some(&roster(1, "succeeded")), false));
        assert_eq!(rendered_history_rows(&shell), expected);
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
fn semantic_scroll_keeps_reasoning_anchor_when_first_roster_is_inserted() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(WIDTH, HEIGHT);
    let run = shell.begin_run("openai");
    shell.toggle_disclosure();
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
    assert_eq!(rendered_history_rows(&shell), expected);
    let state = shell.state.borrow();
    let current = state.viewport_anchor.get().unwrap();
    assert_eq!(current.commit_id, anchor.commit_id);
    assert_eq!(current.text_offset, anchor.text_offset);
    let index = state.subagent_activity_block.unwrap();
    let cache = state.transcript_cache.borrow();
    assert!(cache.lines
        [cache.block_starts[index]..cache.block_starts[index] + cache.block_lengths[index]]
        .iter()
        .any(|line| line.contains("Subagents")));
    assert_eq!(cache.block_starts.len(), state.transcript.len());
}
