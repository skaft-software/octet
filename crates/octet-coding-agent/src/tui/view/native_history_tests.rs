//! Native-reader probes retain actual isolated ShellComponent -> Pi -> VT frames.
use super::*;

struct NativeReplay {
    shell: InteractiveShell,
    bytes: Arc<Mutex<Vec<u8>>>,
    terminal: vt100::Parser,
    replay_frames: Vec<(u16, u16, Vec<u8>)>,
    width: u16,
    height: u16,
}

impl NativeReplay {
    fn new() -> Self {
        Self::with_size(80, 8)
    }

    fn with_size(width: u16, height: u16) -> Self {
        Self::with_theme(crate::tui::theme::test_theme(), width, height)
    }

    fn with_theme(theme: OctetTheme, width: u16, height: u16) -> Self {
        let mut replay = Self::unrendered(theme, width, height);
        replay.render(false);
        replay
    }

    fn unrendered(theme: OctetTheme, width: u16, height: u16) -> Self {
        let (mut shell, bytes) = emulated_shell_with_mode(theme, width, height, true, false);
        // Exercise production publication/materialization, not the inline test
        // component. Its first private layout starts at generation 1.
        shell.isolate_native_test_renderer();
        // Match the product's policy; the generic Pi default stays unchanged.
        shell.tui.as_mut().unwrap().set_clear_on_shrink(false);
        for index in 0..30 {
            shell.notice(format!("NATIVE-HISTORY-{index:02}"));
        }
        Self {
            shell,
            bytes,
            terminal: vt100::Parser::new(height, width, 2048),
            replay_frames: Vec::new(),
            width,
            height,
        }
    }

    fn render(&mut self, stable: bool) -> String {
        self.shell.render();
        let bytes = std::mem::take(&mut *self.bytes.lock().unwrap());
        let output = String::from_utf8(bytes.clone()).unwrap();
        if stable {
            assert!(
                !output.contains("\x1b[3J"),
                "saved-history clear: {output:?}"
            );
            assert!(
                !output.contains("NATIVE-HISTORY-"),
                "history replay: {output:?}"
            );
        }
        process_vt100_with_saved_line_clear(
            &mut self.terminal,
            &bytes,
            self.height,
            self.width,
            2048,
        );
        self.replay_frames.push((self.height, self.width, bytes));
        output
    }

    fn frame(&self) -> String {
        strip_terminal_sequences(&self.shell.tui.as_ref().unwrap().rendered_frame().join("\n"))
    }

    fn assert_canonical_transcript(&self) {
        let state = self.shell.state.borrow();
        let transcript = state.rendered_transcript(self.width);
        let expected = transcript
            .iter()
            .map(|row| strip_terminal_sequences(row))
            .collect::<Vec<_>>();
        let frame = self.shell.tui.as_ref().unwrap().rendered_frame();
        assert!(
            frame.len() >= expected.len(),
            "canonical transcript was clipped"
        );
        let actual = frame[..expected.len()]
            .iter()
            .map(|row| strip_terminal_sequences(row))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "Pi retained stale transcript rows");
    }

    fn history(&self) -> String {
        // vt100 0.15 underflows when a scrollback offset exceeds its viewport
        // height. Replay the same bytes/resizes into a disposable parser, then
        // enlarge only that snapshot. Resizing the retained parser would alter
        // cursor/wrap state and could hide defects in subsequent paints.
        let mut snapshot = vt100::Parser::new(self.height, self.width, 2048);
        for (height, width, bytes) in &self.replay_frames {
            if snapshot.screen().size() != (*height, *width) {
                snapshot.set_size(*height, *width);
            }
            process_vt100_with_saved_line_clear(&mut snapshot, bytes, *height, *width, 2048);
        }
        snapshot.set_size(2048, self.width);
        snapshot.set_scrollback(usize::MAX);
        let saved_rows = snapshot.screen().scrollback();
        let mut rows = snapshot
            .screen()
            .rows(0, self.width)
            .take(saved_rows)
            .collect::<Vec<_>>();
        snapshot.set_scrollback(0);
        rows.extend(
            snapshot
                .screen()
                .rows(0, self.width)
                .take(usize::from(self.height)),
        );
        let physical = rows.join("\n");
        for index in 0..30 {
            let marker = format!("NATIVE-HISTORY-{index:02}");
            assert_eq!(physical.matches(&marker).count(), 1, "{marker}: {physical}");
        }
        physical
    }
}

fn native_profile_matrix() -> impl Iterator<Item = (OctetTheme, u16, u16)> {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    use crate::tui::theme::TerminalBackground;

    [(80, 8), (96, 18), (120, 40)]
        .into_iter()
        .flat_map(|(width, height)| {
            [ColorDepth::TrueColor, ColorDepth::None].map(move |depth| {
                let theme = crate::tui::theme::test_theme_for(
                    TerminalBackground::Light,
                    TerminalCapabilities::test(true, true, depth),
                );
                (theme, width, height)
            })
        })
}

#[test]
fn native_isolated_non_tail_insertion_and_removal_repair_generation_one_history() {
    for (theme, width, height) in native_profile_matrix() {
        for insert in [false, true] {
            let mut replay = NativeReplay::unrendered(theme.clone(), width, height);
            // Keep the edit above the old viewport even at 120x40. All of this
            // is accepted before the isolated component's first layout.
            for index in 0..48 {
                replay.shell.notice(format!("STRUCTURAL-TAIL-{index:02}"));
            }
            if !insert {
                replay
                    .shell
                    .state
                    .borrow_mut()
                    .insert_block(2, TranscriptBlock::Notice("REMOVABLE-HISTORY".into()));
            }
            replay.render(false);
            replay.assert_canonical_transcript();
            let baseline = replay.shell.tui.as_ref().unwrap().full_redraws();
            let epoch = replay.shell.state.borrow().transcript_epoch;
            if insert {
                replay
                    .shell
                    .state
                    .borrow_mut()
                    .insert_block(2, TranscriptBlock::Notice("INSERTED-HISTORY".into()));
            } else {
                replay
                    .shell
                    .state
                    .borrow_mut()
                    .remove_transient_activity_block(2);
            }
            // This is a publication reset within the same session, not a new
            // transcript epoch that could accidentally mask the collision.
            assert_eq!(replay.shell.state.borrow().transcript_epoch, epoch);
            let output = replay.render(false);
            assert_eq!(output.matches("\x1b[3J").count(), 1, "{output:?}");
            assert_eq!(
                replay.shell.tui.as_ref().unwrap().full_redraws(),
                baseline + 1
            );
            replay.assert_canonical_transcript();
            for _ in 0..3 {
                replay.render(true);
            }
            let physical = replay.history();
            assert_eq!(
                physical.matches("INSERTED-HISTORY").count(),
                usize::from(insert)
            );
            assert!(!physical.contains("REMOVABLE-HISTORY"), "{physical}");
            for index in 0..48 {
                assert_eq!(
                    physical
                        .matches(&format!("STRUCTURAL-TAIL-{index:02}"))
                        .count(),
                    1,
                    "{physical}"
                );
            }
        }
    }
}

#[test]
fn native_pending_tool_progress_then_result_is_addressable_and_exactly_once() {
    for (failed, final_rows) in [false, true]
        .into_iter()
        .flat_map(|failed| [0, 1, 5, 40].map(|rows| (failed, rows)))
    {
        let mut replay = NativeReplay::new();
        replay.shell.state.borrow_mut().verbose_tools = final_rows > 5;
        let run = replay.shell.begin_run("openai");
        let id = ToolCallId("native-tool".into());
        replay.shell.on_run_event(
            run,
            &AgentEvent::ToolStarted {
                id: id.clone(),
                name: "bash".into(),
                args: serde_json::json!({"command": "native-fixture"}),
            },
        );
        replay.render(true);
        assert!(replay
            .terminal
            .screen()
            .contents()
            .contains("native-fixture"));
        let baseline = replay.shell.tui.as_ref().unwrap().full_redraws();
        // A real progress frame after every row, not a single coalesced result.
        for index in 0..5 {
            replay.shell.on_run_event(
                run,
                &AgentEvent::ToolProgress {
                    id: id.clone(),
                    progress: ToolProgress::Output {
                        stream: octet_agent::OutputStream::Stdout,
                        bytes: bytes::Bytes::from(format!("RESULT-{index:02}\n")),
                    },
                },
            );
            replay.render(true);
            let visible = replay.terminal.screen().contents();
            assert!(
                visible.contains(&format!("RESULT-{index:02}")),
                "newest live output must remain promptly visible: {visible}"
            );
            assert!(
                visible.contains("native-fixture"),
                "pending intent vanished: {visible}"
            );
            replay
                .shell
                .state
                .borrow_mut()
                .advance_event_dot_animation();
            replay.render(true);
        }
        let result_text = (0..final_rows)
            .map(|index| format!("RESULT-{index:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = if failed {
            Err(octet_agent::ToolError::new(result_text.clone()))
        } else {
            Ok(octet_agent::ToolOutput::new(result_text.clone()))
        };
        replay.shell.on_run_event(
            run,
            &AgentEvent::ToolFinished {
                id: id.clone(),
                result,
                duration: Duration::from_millis(10),
            },
        );
        replay.render(true);
        for _ in 0..3 {
            replay.render(true);
        }
        assert_eq!(replay.shell.tui.as_ref().unwrap().full_redraws(), baseline);
        assert_eq!(replay.shell.debug_tool_output(&id).unwrap(), result_text);
        let physical = replay.history();
        for index in 0..final_rows {
            let marker = format!("RESULT-{index:02}");
            assert_eq!(physical.matches(&marker).count(), 1, "{marker}: {physical}");
        }
        for index in final_rows..5 {
            assert!(
                !physical.contains(&format!("RESULT-{index:02}")),
                "ephemeral output leaked into history: {physical}"
            );
        }
        assert!(!physical.contains("result pending"), "{physical}");
    }
}

#[test]
fn native_ordinary_streaming_paragraph_boundaries_do_not_replay() {
    let mut replay = NativeReplay::with_size(96, 18);
    let run = replay.shell.begin_run("openai");
    let heading = "# APPEND heading\n\n";
    replay.shell.on_run_event(
        run,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: heading.into(),
        },
    );
    replay.render(true);
    let baseline = replay.shell.tui.as_ref().unwrap().full_redraws();
    let mut source = heading.to_owned();

    for index in 0..48 {
        let mut chunk = format!(
            "APPEND_{index:02} deterministic streamed prose with Markdown boundaries and enough words to occupy a physical row.\n"
        );
        if matches!(index, 7 | 15 | 23 | 31 | 39 | 47) {
            chunk.push('\n');
        }
        source.push_str(&chunk);
        replay.shell.on_run_event(
            run,
            &AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: chunk,
            },
        );
        replay.render(true);
    }

    assert_eq!(
        replay.shell.tui.as_ref().unwrap().full_redraws(),
        baseline,
        "ordinary streamed paragraph boundaries must not trigger Pi replay"
    );
    let state = replay.shell.state.borrow();
    let index = state.active_text.expect("active streamed text");
    let TranscriptBlock::Assistant(assistant) = &state.transcript[index] else {
        panic!("assistant");
    };
    assert_eq!(assistant.text, source);
    assert_eq!(
        block_copy_text(&state.transcript[index]),
        sexy_tui_rs::parse_markdown(&source).plain_text()
    );
}

#[test]
fn native_offscreen_tool_animation_does_not_mutate_history() {
    let mut replay = NativeReplay::new();
    let run = replay.shell.begin_run("openai");
    replay.shell.on_run_event(
        run,
        &AgentEvent::ToolStarted {
            id: ToolCallId("offscreen-tool".into()),
            name: "bash".into(),
            args: serde_json::json!({"command": "still-running"}),
        },
    );
    replay.render(true);
    for index in 0..20 {
        replay.shell.notice(format!("LATER-{index:02}"));
    }
    replay.render(true);
    let baseline = replay.shell.tui.as_ref().unwrap().full_redraws();
    let revision = replay.shell.state.borrow().block_revisions[30];
    for _ in 0..12 {
        replay
            .shell
            .state
            .borrow_mut()
            .advance_event_dot_animation();
        replay.shell.state.borrow_mut().advance_status_shimmer();
        replay.shell.state.borrow_mut().advance_status_timer();
        replay.render(true);
    }
    assert_eq!(replay.shell.state.borrow().block_revisions[30], revision);
    assert_eq!(replay.shell.tui.as_ref().unwrap().full_redraws(), baseline);
    assert_eq!(replay.history().matches("still-running").count(), 1);
}

fn worker(id: &str) -> octet_agent::DelegationTelemetryChild {
    octet_agent::DelegationTelemetryChild {
        child_id: id.into(),
        task_name: id.into(),
        profile: None,
        model: "fixture".into(),
        state: "running".into(),
        phase: "thinking".into(),
        current_tool: None,
        tool_use_count: 4,
        input_tokens: 100,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        output_tokens: 20,
        reasoning_tokens: 0,
        total_tokens: 120,
        cost: None,
        cost_microdollars: Some(10),
        elapsed_ms: 1000,
        failure_class: None,
        failure_reason: None,
        effective_tool_policy: test_effective_tool_policy(),
        orchestration_provenance: inherited_delegation_provenance(),
        session: None,
    }
}

fn publish_workers(
    replay: &mut NativeReplay,
    children: Vec<octet_agent::DelegationTelemetryChild>,
) {
    replay.shell.on_agent_event(&AgentEvent::DelegationUpdated {
        snapshot: octet_agent::DelegationTelemetrySnapshot {
            revision: 1,
            captured_at_ms: 1000,
            children,
            total_cost_microdollars: Some(20),
            failure_reason: None,
            failure_class: None,
        },
    });
}

fn assert_parent_history_is_not_worker_preview(replay: &mut NativeReplay, expanded: bool) {
    for text in [replay.frame(), replay.history()] {
        let chrome = shell_chrome(&replay.shell.state.borrow(), replay.width, Instant::now())
            .subagents
            .join("\n");
        assert_eq!(
            text.matches("Subagents").count(),
            usize::from(chrome.contains("Subagents")),
            "{text}"
        );
        assert_eq!(
            text.matches("LIVE-WORKER").count(),
            usize::from(chrome.contains("LIVE-WORKER")),
            "{text}"
        );
        assert!(!text.contains("result pending"), "{text}");
        for index in 0..48 {
            assert_eq!(
                text.matches(&format!("PARENT-{index:02}")).count(),
                1,
                "{text}"
            );
        }
        for tool in 0..2 {
            assert_eq!(
                text.matches(&format!("parent-command-{tool}")).count(),
                1,
                "{text}"
            );
            assert_eq!(
                text.matches(&format!("FINAL-{tool}-11")).count(),
                1,
                "{text}"
            );
            if expanded {
                for row in 0..12 {
                    assert_eq!(
                        text.matches(&format!("FINAL-{tool}-{row:02}")).count(),
                        1,
                        "{text}"
                    );
                }
            }
        }
    }
}

#[test]
fn native_active_roster_preserves_parent_answers_results_and_authoritative_updates() {
    for (theme, width, height) in native_profile_matrix() {
        let mut replay = NativeReplay::with_theme(theme, width, height);
        let run = replay.shell.begin_run("openai");
        replay.shell.on_prompt_submitted("parent owns this answer");
        let mut live = worker("LIVE-WORKER");
        publish_workers(&mut replay, vec![live.clone()]);
        replay.render(false);
        assert!(!replay.frame().contains("result pending"));
        assert!(!replay
            .shell
            .state
            .borrow()
            .rendered_transcript(width)
            .join("\n")
            .contains("LIVE-WORKER"));

        let mut source = String::new();
        for index in 0..48 {
            let chunk = format!("PARENT-{index:02} unrelated **assistant** text β.\n\n");
            source.push_str(&chunk);
            replay.shell.on_run_event(
                run,
                &AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text: chunk,
                },
            );
            if index % 16 == 15 {
                replay.render(false);
                let frame = replay.frame();
                let chrome = shell_chrome(&replay.shell.state.borrow(), width, Instant::now());
                assert_eq!(
                    frame.matches("LIVE-WORKER").count(),
                    usize::from(
                        chrome
                            .subagents
                            .iter()
                            .any(|row| row.contains("LIVE-WORKER"))
                    ),
                    "{frame}"
                );
                assert!(!frame.contains("result pending"), "{frame}");
                for accepted in 0..=index {
                    assert_eq!(
                        frame.matches(&format!("PARENT-{accepted:02}")).count(),
                        1,
                        "{frame}"
                    );
                }
            }
        }
        let mut results = Vec::new();
        for tool in 0..2 {
            let id = ToolCallId(format!("parent-tool-{tool}"));
            replay.shell.on_run_event(
                run,
                &AgentEvent::ToolStarted {
                    id: id.clone(),
                    name: "bash".into(),
                    args: serde_json::json!({"command": format!("parent-command-{tool}")}),
                },
            );
            replay.shell.on_run_event(
                run,
                &AgentEvent::ToolProgress {
                    id: id.clone(),
                    progress: ToolProgress::Output {
                        stream: octet_agent::OutputStream::Stdout,
                        bytes: bytes::Bytes::from("pending output\n".repeat(12)),
                    },
                },
            );
            replay.render(false);
            // The ordinary trailing tool may have a preview, never the roster
            // or the already accepted answer before that tool.
            assert_eq!(replay.frame().matches("PARENT-00").count(), 1);
            let chrome = shell_chrome(&replay.shell.state.borrow(), width, Instant::now());
            assert_eq!(
                replay.frame().matches("LIVE-WORKER").count(),
                usize::from(
                    chrome
                        .subagents
                        .iter()
                        .any(|row| row.contains("LIVE-WORKER"))
                )
            );
            let output = (0..12)
                .map(|row| format!("FINAL-{tool}-{row:02}"))
                .collect::<Vec<_>>()
                .join("\n");
            replay.shell.on_run_event(
                run,
                &AgentEvent::ToolFinished {
                    id: id.clone(),
                    result: Ok(octet_agent::ToolOutput::new(output.clone())),
                    duration: Duration::from_millis(10),
                },
            );
            results.push((id, output));
            replay.render(false);
            assert!(!replay.frame().contains("result pending"));
            assert!(!replay.history().contains("pending output"));
        }
        assert_parent_history_is_not_worker_preview(&mut replay, false);
        replay.shell.on_run_event(
            run,
            &AgentEvent::RunFinished {
                head: octet_agent::EntryId("parent-head".into()),
                reason: octet_agent::FinishReason::Completed,
            },
        );
        replay.render(false);
        replay.assert_canonical_transcript();

        // Telemetry is current even after long history and parent settlement.
        // Real Shell -> Pi -> VT updates stay in chrome (plus the visible
        // aggregate outcome hint): no ED3 or parent-history replay is permitted.
        let transcript = replay
            .shell
            .state
            .borrow()
            .rendered_transcript(width)
            .clone();
        let revisions = replay.shell.state.borrow().block_revisions.clone();
        live.output_tokens = 987;
        live.tool_use_count = 27;
        live.total_tokens = live.input_tokens + live.output_tokens;
        for children in [
            vec![live.clone()],
            vec![live.clone(), worker("SECOND-WORKER")],
            vec![live.clone()],
            vec![octet_agent::DelegationTelemetryChild {
                state: "completed".into(),
                ..live.clone()
            }],
        ] {
            let completed = children[0].state == "completed";
            let baseline = replay.shell.tui.as_ref().unwrap().full_redraws();
            publish_workers(&mut replay, children.clone());
            replay.render(true);
            assert_eq!(replay.shell.tui.as_ref().unwrap().full_redraws(), baseline);
            replay.assert_canonical_transcript();
            assert_parent_history_is_not_worker_preview(&mut replay, false);
            let state = replay.shell.state.borrow();
            assert_eq!(
                state.subagent_activity.as_ref().unwrap().telemetry,
                children
            );
            if !completed {
                assert_eq!(*state.rendered_transcript(width), transcript);
                assert_eq!(state.block_revisions, revisions);
            } else {
                // Settlement removes the outcome's aggregate running hint;
                // all actual transcript evidence and revisions remain stable.
                for ((block, before), after) in state
                    .transcript
                    .iter()
                    .zip(&revisions)
                    .zip(&state.block_revisions)
                {
                    if !matches!(block, TranscriptBlock::Outcome(_)) {
                        assert_eq!(before, after);
                    }
                }
            }
            drop(state);
            let visible = replay.terminal.screen().contents();
            assert_eq!(visible.contains("Subagents"), !completed, "{visible}");
            if !completed && height >= 18 {
                assert!(visible.contains("LIVE-WORKER"), "{visible}");
                assert!(visible.contains("987"), "{visible}");
            }
            replay.render(true);
        }

        for expanded in [true, false, true] {
            replay.shell.set_verbose_tools(expanded);
            replay.render(false);
            replay.assert_canonical_transcript();
            assert_parent_history_is_not_worker_preview(&mut replay, expanded);
            replay.render(true);
        }
        for (columns, rows) in [(120, 40), (80, 8), (96, 18), (width, height)] {
            if (columns, rows) == (replay.width, replay.height) {
                continue;
            }
            replay.terminal.set_size(rows, columns);
            replay.width = columns;
            replay.height = rows;
            replay.shell.set_size(columns, rows);
            let baseline = replay.shell.tui.as_ref().unwrap().full_redraws();
            let output = replay.render(false);
            assert_eq!(output.matches("\x1b[3J").count(), 1, "{output:?}");
            assert_eq!(
                replay.shell.tui.as_ref().unwrap().full_redraws(),
                baseline + 1
            );
            replay.assert_canonical_transcript();
            assert_parent_history_is_not_worker_preview(&mut replay, true);
            replay.render(true);
        }
        let state = replay.shell.state.borrow();
        let assistant = state
            .transcript
            .iter()
            .find(|block| matches!(block, TranscriptBlock::Assistant(_)))
            .unwrap();
        let TranscriptBlock::Assistant(block) = assistant else {
            unreachable!()
        };
        assert_eq!(block.text, source);
        assert!(block.finished);
        assert_eq!(
            block_copy_text(assistant),
            sexy_tui_rs::parse_markdown(&source).plain_text()
        );
        for (tool, (id, output)) in results.iter().enumerate() {
            let index = state.tool_panels[id];
            let TranscriptBlock::Tool(panel) = &state.transcript[index] else {
                unreachable!()
            };
            assert_eq!(&panel.output, output);
            assert_eq!(
                block_copy_text(&state.transcript[index]),
                format!("$ parent-command-{tool}")
            );
        }
    }
}

#[test]
fn native_two_runs_retain_telemetry_and_late_settlement_never_repairs_history() {
    for (theme, width, height) in native_profile_matrix() {
        let mut replay = NativeReplay::with_theme(theme, width, height);
        let first_run = replay.shell.begin_run("fixture");
        replay.shell.on_prompt_submitted("FIRST-ROOT-PROMPT");
        let mut first = worker("FIRST-ROOT-WORKER");
        publish_workers(&mut replay, vec![first.clone()]);
        replay.render(true);
        first.state = "completed".into();
        publish_workers(&mut replay, vec![first.clone()]);
        replay.shell.on_run_event(
            first_run,
            &AgentEvent::RunFinished {
                head: octet_agent::EntryId("first-root-head".into()),
                reason: octet_agent::FinishReason::Completed,
            },
        );
        replay.render(true);
        assert!(!replay.frame().contains("Subagents"));
        let second_run = replay.shell.begin_run("fixture");
        replay.shell.on_prompt_submitted("SECOND-ROOT-PROMPT");
        publish_workers(&mut replay, vec![first.clone()]);
        assert!(!render_shell(&replay.shell.state.borrow(), width)
            .join("\n")
            .contains("Subagents"));
        let mut second = worker("SECOND-ROOT-WORKER");
        publish_workers(&mut replay, vec![first.clone(), second.clone()]);
        replay.render(true);
        for index in 0..48 {
            replay.shell.notice(format!("SECOND-ROOT-TAIL-{index:02}"));
        }
        // The worker keeps its pinned presence after its parent settles.
        replay.shell.on_run_event(
            second_run,
            &AgentEvent::RunFinished {
                head: octet_agent::EntryId("second-root-head".into()),
                reason: octet_agent::FinishReason::Completed,
            },
        );
        replay.render(false);
        assert!(replay.terminal.screen().contents().contains("Subagents"));
        let revisions = replay.shell.state.borrow().block_revisions.clone();
        let transcript = replay
            .shell
            .state
            .borrow()
            .rendered_transcript(width)
            .clone();
        second.state = "failed".into();
        second.failure_reason = Some("late worker failure".into());
        publish_workers(&mut replay, vec![first.clone(), second.clone()]);
        replay.render(true);
        replay.assert_canonical_transcript();
        let state = replay.shell.state.borrow();
        assert_eq!(
            state.subagent_activity.as_ref().unwrap().telemetry,
            vec![first, second]
        );
        assert!(!transcript.join("\n").contains("ROOT-WORKER"));
        for ((block, before), after) in state
            .transcript
            .iter()
            .zip(&revisions)
            .zip(&state.block_revisions)
        {
            if !matches!(block, TranscriptBlock::Outcome(_)) {
                assert_eq!(before, after);
            }
        }
        drop(state);
        for _ in 0..3 {
            replay.render(true);
        }
        for text in [replay.frame(), replay.history()] {
            assert!(!text.contains("Subagents"), "{text}");
            assert!(!text.contains("FIRST-ROOT-WORKER"), "{text}");
            assert!(!text.contains("SECOND-ROOT-WORKER"), "{text}");
            assert!(!text.contains("late worker failure"), "{text}");
            for prompt in ["FIRST-ROOT-PROMPT", "SECOND-ROOT-PROMPT"] {
                assert_eq!(text.matches(prompt).count(), 1, "{text}");
            }
            for index in 0..48 {
                assert_eq!(
                    text.matches(&format!("SECOND-ROOT-TAIL-{index:02}"))
                        .count(),
                    1,
                    "{text}"
                );
            }
        }
    }
}

#[test]
fn native_concurrent_roster_updates_and_failures_are_chrome_only() {
    let mut replay = NativeReplay::with_size(120, 24);
    let mut children = vec![worker("ROSTER-A"), worker("ROSTER-B")];
    publish_workers(&mut replay, children.clone());
    replay.render(true);
    for index in 0..48 {
        replay.shell.notice(format!("ROSTER-LATER-{index:02}"));
    }
    replay.render(true);
    let transcript = replay.shell.state.borrow().rendered_transcript(120).clone();
    let revisions = replay.shell.state.borrow().block_revisions.clone();
    let redraws = replay.shell.tui.as_ref().unwrap().full_redraws();
    for _ in 0..12 {
        replay
            .shell
            .state
            .borrow_mut()
            .advance_event_dot_animation();
        replay.render(true);
    }
    children[0].current_tool = Some("read".into());
    children[0].phase = "using_tool".into();
    children[0].elapsed_ms += 250;
    children[0].tool_use_count = 37;
    children[0].output_tokens = 987;
    publish_workers(&mut replay, children.clone());
    replay.render(true);
    let visible = replay.terminal.screen().contents();
    assert!(
        visible.contains("ROSTER-A") && visible.contains("987"),
        "{visible}"
    );
    children[0].state = "completed".into();
    children[1].state = "failed".into();
    children[1].failure_reason = Some("ROSTER-FAILURE".into());
    publish_workers(&mut replay, children.clone());
    replay.render(true);
    let state = replay.shell.state.borrow();
    assert_eq!(
        state.subagent_activity.as_ref().unwrap().telemetry,
        children
    );
    assert_eq!(state.transcript.len(), revisions.len());
    assert_eq!(state.block_revisions, revisions);
    assert_eq!(state.rendered_transcript(120).as_slice(), transcript.as_slice());
    drop(state);
    assert_eq!(replay.shell.tui.as_ref().unwrap().full_redraws(), redraws);
    let physical = replay.history();
    assert!(!physical.contains("Subagents"), "{physical}");
    assert!(
        !physical.contains("ROSTER-A"),
        "successful worker chrome leaked into history: {physical}"
    );
    assert!(!physical.contains("ROSTER-B"), "{physical}");
    assert!(!physical.contains("ROSTER-FAILURE"), "{physical}");
    publish_workers(&mut replay, children);
    replay.render(true);
    assert_eq!(
        replay.shell.state.borrow().transcript.len(),
        revisions.len()
    );
    for index in 0..48 {
        assert_eq!(
            physical
                .matches(&format!("ROSTER-LATER-{index:02}"))
                .count(),
            1,
            "{physical}"
        );
    }
}

#[test]
fn native_long_markdown_finish_retains_source_and_exactly_once_rows() {
    use octet_ai::{AssistantMessage, AssistantPart, ModelId, Protocol, StopReason};
    let mut replay = NativeReplay::new();
    let run = replay.shell.begin_run("openai");
    let mut source = String::from("| Marker | Description |\n|---|---|\n");
    replay.shell.on_run_event(
        run,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: source.clone(),
        },
    );
    replay.render(true);
    for index in 0..24 {
        let row =
            format!("| FINISH-{index:02} | words to wrap in this table's stable geometry |\n");
        source.push_str(&row);
        replay.shell.on_run_event(
            run,
            &AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: row,
            },
        );
        replay.render(true);
    }
    let index = replay.shell.state.borrow().active_text.unwrap();
    replay.shell.on_run_event(
        run,
        &AgentEvent::TurnFinished {
            turn_cost: None,
            message: AssistantMessage {
                content: vec![AssistantPart::Text(source.clone())],
                model: ModelId("fixture".into()),
                protocol: Protocol::OpenAiResponses,
            },
            stop_reason: StopReason::EndTurn,
            turn_usage: Usage::default(),
            usage: Usage::default(),
            session_cost_microdollars: None,
            run_cost_microdollars: 0,
        },
    );
    replay.render(true);
    let state = replay.shell.state.borrow();
    let TranscriptBlock::Assistant(assistant) = &state.transcript[index] else {
        panic!("assistant");
    };
    assert_eq!(assistant.text, source);
    assert!(assistant.finished);
    assert_eq!(
        block_copy_text(&state.transcript[index]),
        sexy_tui_rs::parse_markdown(&source).plain_text()
    );
    drop(state);
    let physical = replay.history();
    for index in 0..24 {
        let marker = format!("FINISH-{index:02}");
        assert_eq!(physical.matches(&marker).count(), 1, "{marker}: {physical}");
    }
}

#[test]
fn native_structural_stream_resize_replays_once_without_losing_source_or_history() {
    let mut replay = NativeReplay::with_size(96, 18);
    let run = replay.shell.begin_run("openai");
    let mut source = String::from("# RESIZE-HEADING\n\n| Marker | Details |\n|---|---|\n");
    replay.shell.on_run_event(
        run,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: source.clone(),
        },
    );
    replay.render(true);
    for index in 0..24 {
        let row =
            format!("| RESIZE-{index:02} | stable table geometry while the viewport changes |\n");
        source.push_str(&row);
        // Fragment inside cells as a provider would, not only at complete rows.
        for chunk in row.as_bytes().chunks(7) {
            replay.shell.on_run_event(
                run,
                &AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text: String::from_utf8(chunk.to_vec()).unwrap(),
                },
            );
            replay.render(true);
        }
        if let Some((width, height)) = match index {
            6 => Some((40, 8)),
            12 => Some((120, 30)),
            18 => Some((96, 18)),
            _ => None,
        } {
            let baseline = replay.shell.tui.as_ref().unwrap().full_redraws();
            // Model only emitted reset/replay, not a modern emulator's reflow.
            replay.terminal.set_size(height, width);
            replay.width = width;
            replay.height = height;
            replay.shell.set_size(width, height);
            let output = replay.render(false);
            assert_eq!(output.matches("\x1b[3J").count(), 1, "{output:?}");
            assert_eq!(output.matches("\x1b[2J").count(), 1, "{output:?}");
            assert_eq!(
                replay.shell.tui.as_ref().unwrap().full_redraws(),
                baseline + 1
            );
            for sentinel in 0..30 {
                assert_eq!(
                    output
                        .matches(&format!("NATIVE-HISTORY-{sentinel:02}"))
                        .count(),
                    1
                );
            }
            for _ in 0..3 {
                replay.render(true);
            }
        }
    }
    let state = replay.shell.state.borrow();
    let index = state.active_text.unwrap();
    let TranscriptBlock::Assistant(assistant) = &state.transcript[index] else {
        panic!("assistant")
    };
    assert_eq!(assistant.text, source);
    assert_eq!(
        block_copy_text(&state.transcript[index]),
        sexy_tui_rs::parse_markdown(&source).plain_text()
    );
    drop(state);
    let physical = replay.history();
    for index in 0..24 {
        assert_eq!(
            physical.matches(&format!("RESIZE-{index:02}")).count(),
            1,
            "{physical}"
        );
    }
}

#[test]
fn native_late_reference_finalization_repairs_history_once_then_stays_quiet() {
    use octet_ai::{AssistantMessage, AssistantPart, ModelId, Protocol, StopReason};
    let mut replay = NativeReplay::with_size(80, 12);
    let run = replay.shell.begin_run("openai");
    let mut source = String::from("[REFERENCE-LABEL][late]\n\n");
    for index in 0..24 {
        source.push_str(&format!("REFERENCE-BODY-{index:02}\n\n"));
    }
    replay.shell.on_run_event(
        run,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: source.clone(),
        },
    );
    replay.render(true);
    let baseline = replay.shell.tui.as_ref().unwrap().full_redraws();
    let suffix = "[late]: https://example.invalid/reference\n";
    source.push_str(suffix);
    replay.shell.on_run_event(
        run,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: suffix.into(),
        },
    );
    let mut output = replay.render(false);
    replay.shell.on_run_event(
        run,
        &AgentEvent::TurnFinished {
            turn_cost: None,
            message: AssistantMessage {
                content: vec![AssistantPart::Text(source.clone())],
                model: ModelId("fixture".into()),
                protocol: Protocol::OpenAiResponses,
            },
            stop_reason: StopReason::EndTurn,
            turn_usage: Usage::default(),
            usage: Usage::default(),
            session_cost_microdollars: None,
            run_cost_microdollars: 0,
        },
    );
    output.push_str(&replay.render(false));
    assert_eq!(
        output.matches("\x1b[3J").count(),
        1,
        "late semantic repair: {output:?}"
    );
    assert_eq!(
        replay.shell.tui.as_ref().unwrap().full_redraws(),
        baseline + 1
    );
    for _ in 0..3 {
        replay.render(true);
    }
    let state = replay.shell.state.borrow();
    let block = state
        .transcript
        .iter()
        .find(|block| matches!(block, TranscriptBlock::Assistant(_)))
        .unwrap();
    let TranscriptBlock::Assistant(assistant) = block else {
        unreachable!()
    };
    assert_eq!(assistant.text, source);
    assert!(assistant.finished);
    assert_eq!(
        block_copy_text(block),
        sexy_tui_rs::parse_markdown(&source).plain_text()
    );
    drop(state);
    let physical = replay.history();
    assert_eq!(physical.matches("REFERENCE-LABEL").count(), 1, "{physical}");
    assert!(
        !physical.contains("[late]"),
        "unresolved reference leaked: {physical}"
    );
    for index in 0..24 {
        assert_eq!(
            physical
                .matches(&format!("REFERENCE-BODY-{index:02}"))
                .count(),
            1,
            "{physical}"
        );
    }
}
