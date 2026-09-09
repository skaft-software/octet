//! Native-reader producer probes use actual ShellComponent -> Pi -> VT bytes.
use super::*;

struct NativeReplay {
    shell: InteractiveShell,
    bytes: Arc<Mutex<Vec<u8>>>,
    terminal: vt100::Parser,
}

impl NativeReplay {
    fn new() -> Self {
        let (mut shell, bytes) =
            emulated_shell_with_mode(crate::tui::theme::test_theme(), 80, 8, true, false);
        // Match the product's policy; the generic Pi default stays unchanged.
        shell.tui.as_mut().unwrap().set_clear_on_shrink(false);
        for index in 0..30 {
            shell.notice(format!("NATIVE-HISTORY-{index:02}"));
        }
        let mut replay = Self {
            shell,
            bytes,
            terminal: vt100::Parser::new(8, 80, 2048),
        };
        replay.render(false);
        replay
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
        process_vt100_with_saved_line_clear(&mut self.terminal, &bytes, 8, 80, 2048);
        output
    }

    fn history(&mut self) -> String {
        self.terminal.set_size(2048, 80);
        self.terminal.set_scrollback(usize::MAX);
        let physical = self.terminal.screen().contents();
        for index in 0..30 {
            let marker = format!("NATIVE-HISTORY-{index:02}");
            assert_eq!(physical.matches(&marker).count(), 1, "{marker}: {physical}");
        }
        physical
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

#[test]
fn native_offscreen_concurrent_roster_animation_is_quiet_and_state_is_not_frozen() {
    let mut replay = NativeReplay::new();
    let mut children = vec![worker("ROSTER-A"), worker("ROSTER-B")];
    publish_workers(&mut replay, children.clone());
    replay.render(true);
    for index in 0..20 {
        replay.shell.notice(format!("ROSTER-LATER-{index:02}"));
    }
    replay.render(true);
    for _ in 0..12 {
        replay
            .shell
            .state
            .borrow_mut()
            .advance_event_dot_animation();
        replay.render(true);
    }
    // Current-tool/phase/clock telemetry is retained, but not historical row text.
    children[0].current_tool = Some("read".into());
    children[0].phase = "using_tool".into();
    children[0].elapsed_ms += 250;
    publish_workers(&mut replay, children.clone());
    replay.render(true);
    children[0].state = "completed".into();
    children[1].state = "failed".into();
    children[1].failure_reason = Some("ROSTER-FAILURE".into());
    publish_workers(&mut replay, children);
    let output = replay.render(false);
    // Evidence probe for the still-open semantic mutation boundary. Do not
    // falsely satisfy no-ED3 by freezing a historical worker in running state.
    eprintln!(
        "#392 remaining offscreen roster state: ED3={}",
        output.matches("\x1b[3J").count()
    );
    let state = replay.shell.state.borrow();
    let index = state.subagent_activity_block.unwrap();
    let semantic = block_copy_text(&state.transcript[index]);
    assert!(
        semantic.contains("completed") && semantic.contains("failed"),
        "{semantic}"
    );
    drop(state);
    let physical = replay.history();
    for id in ["ROSTER-A", "ROSTER-B"] {
        assert_eq!(physical.matches(id).count(), 1, "{physical}");
    }
    assert!(
        !physical.contains("running"),
        "historical running state was frozen: {physical}"
    );
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
