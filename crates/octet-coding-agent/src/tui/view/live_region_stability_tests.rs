//! Consecutive fullscreen frames keep content-only live updates in their rows.
use super::*;
use sexy_tui_rs::Component;

pub(super) fn worker(name: &str, state: &str) -> octet_agent::DelegationTelemetryChild {
    octet_agent::DelegationTelemetryChild {
        child_id: name.into(),
        task_name: name.into(),
        profile: None,
        model: "fixture".into(),
        state: state.into(),
        phase: state.into(),
        current_tool: None,
        tool_use_count: 0,
        input_tokens: 100,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        estimated_output_tokens: None,
        output_tokens: 10,
        reasoning_tokens: 0,
        total_tokens: 110,
        cost: None,
        cost_microdollars: Some(0),
        elapsed_ms: 1,
        failure_class: None,
        failure_reason: None,
        effective_tool_policy: octet_agent::SandboxConfig::new(".")
            .effective_tool_policy(octet_agent::EffectPolicy::Controlled),
        orchestration_provenance: octet_agent::DelegationOrchestrationProvenance::all(
            octet_agent::DelegationPolicySource::ParentInherited,
        ),
        session: None,
    }
}

fn snapshot(revision: u64, states: &[(&str, &str)]) -> octet_agent::DelegationTelemetrySnapshot {
    octet_agent::DelegationTelemetrySnapshot {
        revision,
        captured_at_ms: revision,
        children: states
            .iter()
            .map(|(name, state)| worker(name, state))
            .collect(),
        total_cost_microdollars: Some(0),
        failure_reason: None,
        failure_class: None,
    }
}

fn fixture(
    width: u16,
    height: u16,
    isolated: bool,
) -> (InteractiveShell, renderer_runtime::ShellComponent) {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(width, height);
    shell.state.borrow_mut().application_viewport_requested = true;
    for index in 0..40 {
        shell.notice(format!("HISTORY-{index}"));
    }
    shell.begin_run("fixture");
    shell
        .state
        .borrow_mut()
        .working_status
        .as_mut()
        .unwrap()
        .activity_started_at = Some(Instant::now() + Duration::from_secs(3600));
    for block in &mut shell.state.borrow_mut().transcript {
        if let TranscriptBlock::Reasoning(reasoning) = block {
            reasoning.activity_started_at = Some(Instant::now() + Duration::from_secs(3600));
        }
    }
    let component = if isolated {
        renderer_runtime::ShellComponent::isolated(shell.state.clone(), true)
    } else {
        renderer_runtime::ShellComponent::new(shell.state.clone(), true)
    };
    (shell, component)
}

fn frame(component: &renderer_runtime::ShellComponent, width: u16) -> Vec<String> {
    component
        .render(width)
        .iter()
        .map(|row| strip_terminal_sequences(row))
        .collect()
}

fn history_positions(rows: &[String]) -> Vec<(usize, String)> {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| row.contains("HISTORY-"))
        .map(|(index, row)| (index, row.clone()))
        .collect()
}

#[test]
fn working_slot_stays_below_reasoning_and_tools_for_the_entire_run() {
    // Pi compatibility: packages/coding-agent/src/modes/interactive/
    // interactive-mode.ts:2413-2423,3490-3503,3714-3726
    // owns status separately from chat, retaining it through thinking/tools.
    for isolated in [false, true] {
        for component_line in [true, false] {
            for expanded in [true, false] {
                let (width, height) = (80, 32);
                let (mut shell, component) = fixture(width, height, isolated);
                shell.set_thinking_expanded(expanded);
                if component_line {
                    shell.set_extension_ui(ShellExtensionUi {
                        working: Some(ShellExtensionWorking {
                            component_line: Some("◟ Thoda time lagega boss...".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    });
                }
                let mut previous = frame(&component, width);
                let working_label = if component_line {
                    "Thoda time"
                } else {
                    "Working"
                };
                let working_row = previous
                    .iter()
                    .position(|row| row.contains(working_label))
                    .unwrap();
                let mut paint = |shell: &mut InteractiveShell| {
                    {
                        let mut state = shell.state.borrow_mut();
                        if let Some(index) = state.active_reasoning {
                            if let TranscriptBlock::Reasoning(reasoning) =
                                &mut state.transcript[index]
                            {
                                let future = Instant::now() + Duration::from_secs(3600);
                                reasoning.activity_started_at = Some(future);
                                reasoning.reasoning_started_at = Some(future);
                            }
                        }
                    }
                    let rows = frame(&component, width);
                    assert_eq!(
                        rows.iter()
                            .filter(|row| row.contains(working_label))
                            .count(),
                        1,
                        "{isolated}/{component_line}/{expanded}: {rows:?}"
                    );
                    assert_eq!(
                        rows.iter().position(|row| row.contains(working_label)),
                        Some(working_row),
                        "working slot moved: {previous:?} -> {rows:?}"
                    );
                    for (old_row, text) in history_positions(&previous) {
                        if let Some(row) = rows.iter().position(|current| current == &text) {
                            assert!(
                                row <= old_row,
                                "history moved down: {previous:?} -> {rows:?}"
                            );
                        }
                    }
                    previous = rows;
                    shell.state.borrow_mut().advance_status_timer();
                    assert_eq!(frame(&component, width), previous);
                };
                for cycle in 0..2 {
                    shell.on_agent_event(&AgentEvent::OutputDelta {
                        channel: OutputChannel::Reasoning,
                        text: "# Tracing loading reset paths\n\n".into(),
                    });
                    paint(&mut shell);
                    shell.on_agent_event(&AgentEvent::OutputDelta {
                        channel: OutputChannel::Reasoning,
                        text: "# Checking stale cache behavior\n\n".into(),
                    });
                    paint(&mut shell);
                    if component_line {
                        shell.set_extension_ui(ShellExtensionUi {
                            working: Some(ShellExtensionWorking {
                                component_line: Some(format!(
                                    "◞ Thoda time lagega boss... {cycle}"
                                )),
                                ..Default::default()
                            }),
                            ..Default::default()
                        });
                        paint(&mut shell);
                    }
                    for text in ["Continuing investigation.", " Checking next tool."] {
                        shell.on_agent_event(&AgentEvent::OutputDelta {
                            channel: OutputChannel::Text,
                            text: text.into(),
                        });
                        paint(&mut shell);
                    }
                    // The durable assistant boundary closes model output
                    // before the independently delivered tool-start event.
                    shell.on_agent_event(&AgentEvent::TurnFinished {
                        message: octet_ai::AssistantMessage {
                            content: vec![octet_ai::AssistantPart::Text(
                                "Continuing investigation. Checking next tool.".into(),
                            )],
                            model: octet_ai::ModelId("fixture".into()),
                            protocol: octet_ai::Protocol::OpenAiResponses,
                        },
                        stop_reason: octet_ai::StopReason::ToolUse,
                        turn_cost: None,
                        usage: Default::default(),
                        turn_usage: Default::default(),
                        session_cost_microdollars: None,
                        run_cost_microdollars: 0,
                    });
                    paint(&mut shell);
                    let id = ToolCallId(format!("cycle-{cycle}"));
                    shell.on_agent_event(&AgentEvent::ToolStarted {
                        id: id.clone(),
                        name: "bash".into(),
                        args: serde_json::json!({"command":"true"}),
                    });
                    paint(&mut shell);
                    shell.on_agent_event(&AgentEvent::ToolFinished {
                        id,
                        result: Ok(octet_agent::ToolOutput::new(
                            "exit=0 duration=10ms\n(no output)",
                        )),
                        duration: Duration::from_millis(10),
                    });
                    paint(&mut shell);
                    shell.on_agent_event(&AgentEvent::TurnStarted);
                    paint(&mut shell);
                }
                shell.on_agent_event(&AgentEvent::RunFinished {
                    reason: octet_agent::FinishReason::Completed,
                    head: octet_agent::EntryId("fixture".into()),
                });
                assert!(!frame(&component, width)
                    .iter()
                    .any(|row| row.contains(working_label)));
            }
        }
    }
}

#[test]
fn working_keeps_a_neutral_row_after_the_shaded_prompt_card() {
    let theme = crate::tui::theme::test_theme_for(
        crate::tui::theme::TerminalBackground::Dark,
        crate::tui::terminal::TerminalCapabilities::test(
            true,
            true,
            crate::tui::terminal::ColorDepth::TrueColor,
        ),
    );
    let mut shell = InteractiveShell::test_shell_with_theme(theme);
    shell.set_size(80, 24);
    shell.set_identity("codex", "gpt-6.2-astra", "high");
    for index in 0..24 {
        shell.notice(format!("history {index}"));
    }
    shell.on_prompt_submitted("continue");
    shell.mark_prompt_persisted();
    shell.begin_run("codex");
    let state = shell.state.borrow();
    for rows in [
        render_shell_at(&state, 80, Instant::now()),
        render_shell_viewport_at(&state, 80, Instant::now()),
    ] {
        let prompt = rows
            .iter()
            .position(|row| strip_terminal_sequences(row).contains("› continue"))
            .unwrap();
        let working = rows
            .iter()
            .position(|row| strip_terminal_sequences(row).contains("• Working ("))
            .unwrap();
        let bg = |row: usize| sexy_tui_rs::utils::background_sgr_at_column(&rows[row], 2);
        assert!(
            bg(prompt + 1).is_some(),
            "fixture needs a painted bottom edge"
        );
        assert_eq!(
            working,
            prompt + 3,
            "expected a neutral row outside the prompt card"
        );
        assert!(strip_terminal_sequences(&rows[working - 1])
            .trim()
            .is_empty());
        assert_eq!(bg(working - 1), None);
    }
}

#[test]
fn working_slot_keeps_composer_and_completion_hits_on_painted_rows() {
    let cell = |rows: &[String], text: &str| {
        rows.iter()
            .enumerate()
            .find_map(|(row, line)| {
                line.find(text)
                    .map(|column| (row as u16, visible_width(&line[..column]) as u16))
            })
            .unwrap_or_else(|| panic!("missing {text:?}: {rows:?}"))
    };
    for threaded in [false, true] {
        for component_line in [false, true] {
            let (mut shell, component) = fixture(40, 24, true);
            shell.state.borrow_mut().render_threaded = threaded;
            shell.state.borrow_mut().editor.set_text("editable draft");
            if component_line {
                shell.set_extension_ui(ShellExtensionUi {
                    working: Some(ShellExtensionWorking {
                        component_line: Some("◟ Working fixture".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                });
            }
            let rows = frame(&component, 40);
            shell.state.frame_written_at(None);
            let working = cell(&rows, "Working").0;
            let (row, column) = cell(&rows, "editable");
            assert_eq!(
                shell
                    .state
                    .borrow()
                    .composer_pointer_geometry()
                    .unwrap()
                    .composer
                    .offset(row, column, true),
                Some(0)
            );
            assert!(shell.begin_composer_pointer(row, column, false));
            shell.on_agent_event(&AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: "Streaming above the working slot".into(),
            });
            let rows = frame(&component, 40);
            shell.state.frame_written_at(None);
            assert_eq!(cell(&rows, "Working").0, working);
            assert_eq!(cell(&rows, "editable"), (row, column));
            shell.end_composer_pointer(row, column);
            assert_eq!(shell.state.borrow().editor.cursor(), 0);

            shell.state.borrow_mut().extension_commands =
                vec![("fixture-command".into(), "Fixture".into())].into();
            shell.state.borrow_mut().editor.set_text("/fixture-");
            let rows = frame(&component, 40);
            shell.state.frame_written_at(None);
            let (row, column) = cell(&rows, "fixture-command");
            assert!(shell.begin_composer_pointer(row, column, false));
            shell.end_composer_pointer(row, column);
            assert_eq!(shell.pending(), "/fixture-command ");
        }
    }
}

#[test]
fn collapsed_reasoning_completion_preserves_rows_at_inline_hint_widths() {
    for width in [32, 40, 48, 64, 80] {
        let (mut shell, component) = fixture(width, 24, true);
        shell.on_agent_event(&AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "A private trace without a summary heading".into(),
        });
        let before = frame(&component, width);
        shell.state.borrow_mut().finish_turn_streaming_blocks();
        let after = frame(&component, width);
        assert_eq!(
            history_positions(&after),
            history_positions(&before),
            "completion changed row geometry at width {width}: {before:?} -> {after:?}"
        );
    }
}

#[test]
fn bash_completion_paints_final_geometry_atomically() {
    // Pi compatibility: packages/coding-agent/src/modes/interactive/
    // interactive-mode.ts:3689-3709 updates the existing tool component's
    // partial/final result before requesting a render, without removing it.
    for isolated in [false, true] {
        for (width, height) in [(24, 24), (80, 24), (120, 32)] {
            for (output_rows, exit) in [(0, 0), (1, 0), (20, 0), (1, 7), (20, 7)] {
                for continuation in ["text", "reasoning", "tool"] {
                    let (mut shell, component) = fixture(width, height, isolated);
                    shell
                        .state
                        .borrow_mut()
                        .theme
                        .set_duration_preference(Some(true));
                    let id = ToolCallId("bash-fixture".into());
                    shell.on_agent_event(&AgentEvent::ToolStarted {
                        id: id.clone(),
                        name: "bash".into(),
                        args: serde_json::json!({"command":"fixture"}),
                    });
                    frame(&component, width);
                    let output = (0..output_rows)
                        .map(|index| format!("OUTPUT-{index:02}\n"))
                        .collect::<String>();
                    for line in output.split_inclusive('\n') {
                        shell.on_agent_event(&AgentEvent::ToolProgress {
                            id: id.clone(),
                            progress: ToolProgress::Output {
                                stream: octet_agent::OutputStream::Stdout,
                                bytes: bytes::Bytes::copy_from_slice(line.as_bytes()),
                            },
                        });
                        frame(&component, width);
                    }
                    let before = frame(&component, width);
                    let anchor_row = |rows: &[String]| {
                        rows.iter()
                            .position(|row| row.contains("HISTORY-39"))
                            .unwrap()
                    };
                    let before_row = anchor_row(&before);
                    shell.on_agent_event(&AgentEvent::ToolFinished {
                        id: id.clone(),
                        result: Ok(octet_agent::ToolOutput::new(format!(
                            "exit={exit} duration=10ms\nstdout: {output_rows} lines\n{output}complete_stdout=true"
                        ))),
                        duration: Duration::from_millis(10),
                    });
                    let finished = frame(&component, width);
                    let finished_row = anchor_row(&finished);
                    if output_rows == 20 {
                        for marker in ["OUTPUT-00", "OUTPUT-01", "OUTPUT-18", "OUTPUT-19"] {
                            assert!(
                                finished.iter().any(|row| row.contains(marker)),
                                "{finished:?}"
                            );
                        }
                        assert!(!finished.iter().any(|row| row.contains("OUTPUT-02")));
                    }
                    shell.state.borrow_mut().advance_status_timer();
                    assert_eq!(
                        frame(&component, width),
                        finished,
                        "timer tick changed completed geometry"
                    );
                    let cold =
                        renderer_runtime::ShellComponent::isolated(shell.state.clone(), true);
                    assert_eq!(
                        frame(&cold, width),
                        finished,
                        "first completion paint differs from canonical geometry"
                    );
                    assert!(
                        finished_row <= before_row,
                        "{isolated}/{width}/{output_rows}/{exit}: {before:?} -> {finished:?}"
                    );
                    for _ in 0..3 {
                        assert_eq!(frame(&component, width), finished);
                    }
                    shell.on_agent_event(&AgentEvent::TurnStarted);
                    assert_eq!(frame(&component, width), finished);
                    if continuation == "tool" {
                        shell.on_agent_event(&AgentEvent::ToolStarted {
                            id: ToolCallId("next-bash".into()),
                            name: "bash".into(),
                            args: serde_json::json!({"command":"next"}),
                        });
                    } else {
                        shell.on_agent_event(&AgentEvent::OutputDelta {
                            channel: if continuation == "text" {
                                OutputChannel::Text
                            } else {
                                OutputChannel::Reasoning
                            },
                            text: "NEXT-ASSISTANT".into(),
                        });
                    }
                    let after = frame(&component, width);
                    assert!(
                        anchor_row(&after) <= finished_row,
                        "{isolated}/{width}/{output_rows}/{exit}: {finished:?} -> {after:?}"
                    );
                    assert_eq!(frame(&component, width), after);
                }
            }
        }
    }
}

#[test]
fn bash_completion_keeps_cached_renderer_geometry_until_the_final_frame_arrives() {
    use octet_agent::extension_process::{ExtensionResourceOwner, TranscriptRenderResponse};

    // A Pi renderer updates its existing component in place. An asynchronous
    // host renderer must not insert a differently sized native fallback between
    // that component's partial and final snapshots.
    for isolated in [false, true] {
        for (width, height) in [(24, 24), (80, 24), (120, 32)] {
            for (output_rows, exit) in [(0, 0), (1, 0), (20, 0), (1, 7), (20, 7)] {
                let (mut shell, component) = fixture(width, height, isolated);
                let owner = ExtensionResourceOwner {
                    session_id: "fixture".into(),
                    extension_instance_id: "bash-renderer".into(),
                    process_generation: 1,
                };
                shell.set_transcript_render_owners(vec![owner.clone()]);
                let id = ToolCallId("rendered-bash".into());
                shell.on_agent_event(&AgentEvent::ToolStarted {
                    id: id.clone(),
                    name: "bash".into(),
                    args: serde_json::json!({"command":"fixture"}),
                });
                let accept = |shell: &mut InteractiveShell, label: &str| {
                    let candidate = shell.transcript_render_candidates().into_iter().find(|candidate| {
                        matches!(&candidate.render, octet_agent::extension_process::TranscriptRenderContent::Tool { tool_call_id, .. } if tool_call_id == &id.0)
                    }).expect("updated bash render candidate");
                    assert!(shell.accept_transcript_render(
                        &candidate,
                        owner.clone(),
                        TranscriptRenderResponse {
                            registered: true,
                            lines: Some(vec!["Bash fixture".into(), label.into()]),
                            markdown: None,
                            render_shell: None,
                        }
                    ));
                };
                accept(&mut shell, "starting");
                frame(&component, width);
                let output = (0..output_rows)
                    .map(|index| format!("OUTPUT-{index:02}\n"))
                    .collect::<String>();
                shell.on_agent_event(&AgentEvent::ToolProgress {
                    id: id.clone(),
                    progress: ToolProgress::Output {
                        stream: octet_agent::OutputStream::Stdout,
                        bytes: bytes::Bytes::copy_from_slice(output.as_bytes()),
                    },
                });
                accept(&mut shell, "streaming output");
                let before = history_positions(&frame(&component, width));
                assert!(!before.is_empty());
                shell.on_agent_event(&AgentEvent::ToolFinished {
                    id: id.clone(),
                    result: Ok(octet_agent::ToolOutput::new(format!("exit={exit} duration=10ms\nstdout: {output_rows} lines\n{output}complete_stdout=true"))),
                    duration: Duration::from_millis(10),
                });
                for _ in 0..3 {
                    let rows = frame(&component, width);
                    assert_eq!(
                        history_positions(&rows),
                        before,
                        "{isolated}/{width}/{output_rows}/{exit}: {rows:?}"
                    );
                }
                // Completion may be followed immediately by more model/tool
                // events while the final renderer reply is still in flight.
                shell.on_agent_event(&AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text: "NEXT-ASSISTANT".into(),
                });
                let after_delta = history_positions(&frame(&component, width));
                shell.on_agent_event(&AgentEvent::ToolStarted {
                    id: ToolCallId("next-bash".into()),
                    name: "bash".into(),
                    args: serde_json::json!({"command":"next"}),
                });
                let after_start = history_positions(&frame(&component, width));
                assert!(after_delta.last().unwrap().0 <= before.last().unwrap().0);
                assert!(after_start.last().unwrap().0 <= after_delta.last().unwrap().0);
                accept(&mut shell, "finished output");
                let final_rows = frame(&component, width);
                assert!(final_rows.iter().any(|row| row.contains("finished output")));
                assert_eq!(history_positions(&final_rows), after_start);
                assert_eq!(frame(&component, width), final_rows);
            }
        }
    }
}

#[test]
fn live_roster_settlement_reserves_rows_until_the_wave_settles() {
    for isolated in [false, true] {
        for (width, height) in [(24, 16), (80, 24), (120, 32)] {
            let (mut shell, component) = fixture(width, height, isolated);
            let mut states = vec![
                ("wheel-input", "pending"),
                ("scroll-caches", "running"),
                ("viewport-chrome", "pending"),
                ("fourth", "pending"),
                ("fifth", "pending"),
            ];
            shell.set_subagent_telemetry(Some(&snapshot(1, &states)), true);
            let before = history_positions(&frame(&component, width));
            assert!(!before.is_empty());
            for (index, state) in ["failed", "running", "completed", "failed"]
                .into_iter()
                .enumerate()
            {
                states[index].1 = state;
                shell.set_subagent_telemetry(Some(&snapshot(index as u64 + 2, &states)), true);
                let rows = frame(&component, width);
                assert_eq!(
                    history_positions(&rows),
                    before,
                    "{isolated}/{width}: {rows:?}"
                );
                assert_eq!(
                    rows.iter().filter(|row| row.contains("/subagents")).count(),
                    1
                );
                let state = shell.state.borrow();
                let TranscriptBlock::Subagents(summary) = state.transcript.last().unwrap() else {
                    panic!("live roster missing");
                };
                let native = tern_agents::transcript(1, summary, false);
                assert_eq!(native.c.unwrap()[0].c.as_ref().unwrap().len(), 5);
            }
            let live_height = shell.state.borrow().rendered_transcript(width).len();
            for worker in &mut states {
                worker.1 = "completed";
            }
            shell.set_subagent_telemetry(Some(&snapshot(8, &states)), true);
            assert!(shell.state.borrow().rendered_transcript(width).len() < live_height);
            assert_eq!(
                shell
                    .state
                    .borrow()
                    .transcript
                    .iter()
                    .filter(|block| matches!(block, TranscriptBlock::Subagents(_)))
                    .count(),
                1
            );
        }
    }
}

#[test]
fn live_roster_growth_is_monotonic_and_content_changes_keep_its_height() {
    let (mut shell, component) = fixture(120, 32, true);
    let mut states = vec![("first", "running")];
    shell.set_subagent_telemetry(Some(&snapshot(1, &states)), true);
    frame(&component, 120);
    let mut previous = shell.state.borrow().rendered_transcript(120).len();
    for (index, name) in ["second", "third", "fourth", "fifth", "sixth"]
        .into_iter()
        .enumerate()
    {
        states.push((name, "pending"));
        shell.set_subagent_telemetry(Some(&snapshot(index as u64 * 2 + 2, &states)), true);
        let grown = shell.state.borrow().rendered_transcript(120).len();
        assert!(grown >= previous);
        let before = history_positions(&frame(&component, 120));
        states[index].1 = "failed";
        shell.set_subagent_telemetry(Some(&snapshot(index as u64 * 2 + 3, &states)), true);
        assert_eq!(history_positions(&frame(&component, 120)), before);
        assert_eq!(shell.state.borrow().rendered_transcript(120).len(), grown);
        previous = grown;
    }
}

#[test]
fn live_roster_uses_the_latest_host_snapshot_across_event_and_idle_paths() {
    let (mut shell, component) = fixture(120, 32, true);
    let run = shell.current_run_id().unwrap();
    let older = snapshot(
        10,
        &[("wheel-input", "pending"), ("scroll-caches", "running")],
    );
    let fresh = snapshot(
        11,
        &[("wheel-input", "failed"), ("scroll-caches", "running")],
    );
    shell.set_subagent_telemetry(Some(&older), true);
    shell.on_run_event(
        run,
        &AgentEvent::DelegationUpdated {
            snapshot: fresh.clone(),
        },
    );
    let before = frame(&component, 120);
    for _ in 0..4 {
        assert!(!shell.set_subagent_telemetry(Some(&older), true));
        shell.on_run_event(
            run,
            &AgentEvent::DelegationUpdated {
                snapshot: older.clone(),
            },
        );
        assert_eq!(frame(&component, 120), before);
        assert_eq!(
            shell
                .state
                .borrow()
                .subagent_activity
                .as_ref()
                .unwrap()
                .telemetry,
            fresh.children
        );
    }
    // An independently cached extension/inspector presentation has no authority
    // to replace native worker telemetry once the host stream is established.
    let cached = serde_json::from_value(serde_json::json!({
        "revision": 200, "status": {"state":"active", "label":"Subagents"},
        "activities": [{"id":"wheel-input", "kind":"subagent", "state":"pending", "summary":"stale"}],
        "actions": []
    })).unwrap();
    assert!(!shell.set_subagent_presentation(Some(&cached), true));
    assert_eq!(frame(&component, 120), before);
    assert!(
        !shell.set_subagent_telemetry(Some(&fresh), true),
        "duplicate delivery is inert"
    );
    let cleanup = snapshot(12, &[]);
    assert!(!shell.set_subagent_telemetry(Some(&cleanup), true));
    assert!(!shell.set_subagent_telemetry(Some(&older), true));
    assert_eq!(frame(&component, 120), before);

    let directory = tempfile::tempdir().unwrap();
    let session = Session::create(directory.path().join("replacement.jsonl")).unwrap();
    shell.hydrate(&session).unwrap();
    assert!(shell.set_subagent_telemetry(Some(&snapshot(1, &[("new-owner", "running")])), true));
    assert!(frame(&component, 120).join("\n").contains("new-owner"));
}

#[test]
fn live_working_component_and_native_fallback_keep_the_same_rows() {
    // Pi compatibility source: packages/tui/src/components/loader.ts:44-45
    // renders its live status; packages/coding-agent/src/modes/interactive/
    // interactive-mode.ts:2740-2744 updates that same component's message.
    // Native terminal ownership keeps the corresponding status slot stable.
    for isolated in [false, true] {
        for pi in [false, true] {
            let (mut shell, component) = fixture(80, 24, isolated);
            if pi {
                shell.set_theme(crate::tui::theme::test_theme_from_source(
                    "[metadata]\nprofile = \"pi\"\n",
                ));
            }
            let before = history_positions(&frame(&component, 80));
            assert!(!before.is_empty());
            for patched in [true, false, true, false] {
                shell.set_extension_ui(ShellExtensionUi {
                    working: patched.then(|| ShellExtensionWorking {
                        component_line: Some("⠋ Running JavaScript".into()),
                        message: Some("Running JavaScript".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                });
                let rows = frame(&component, 80);
                assert_eq!(
                    history_positions(&rows),
                    before,
                    "{isolated}/{pi}: {rows:?}"
                );
            }
        }
    }
}

#[test]
fn live_codemode_status_output_grows_without_contracting_between_phases() {
    // Codemode's status messages are append-only output, unlike replaceable
    // decorations. Keep those bytes and their bounded head/tail preview intact.
    for isolated in [false, true] {
        for width in [24, 80, 120] {
            let (mut shell, component) = fixture(width, 32, isolated);
            let id = ToolCallId("status-fixture".into());
            shell.on_agent_event(&AgentEvent::ToolStarted {
                id: id.clone(),
                name: "codemode".into(),
                args: serde_json::json!({"code":"return await tools.read({path: 'README.md'})"}),
            });
            frame(&component, width);
            let mut previous = shell.state.borrow().rendered_transcript(width).len();
            for label in [
                "Preparing frozen tool snapshot",
                "Running JavaScript (quickjs; deadline 120000 ms, up to 256 calls)",
                "Nested tool 1/256",
                "Nested tool 2/256",
                "Nested tool 256/256",
            ] {
                shell.on_agent_event(&AgentEvent::ToolProgress {
                    id: id.clone(),
                    progress: ToolProgress::Status(label.into()),
                });
                let rows = frame(&component, width);
                assert_eq!(rows.iter().filter(|row| row.contains("Working")).count(), 1);
                let state = shell.state.borrow();
                let current = state.rendered_transcript(width).len();
                assert!(current >= previous, "{width}: {rows:?}");
                let index = state.tool_panels[&id];
                let TranscriptBlock::Tool(panel) = &state.transcript[index] else {
                    unreachable!()
                };
                assert!(panel.output.contains(label));
                previous = current;
            }
        }
    }
}

#[test]
fn live_tool_decoration_replacements_keep_fullscreen_history_in_place() {
    for isolated in [false, true] {
        for width in [24, 80, 120] {
            let (mut shell, component) = fixture(width, 32, isolated);
            let id = ToolCallId("codemode-fixture".into());
            shell.on_agent_event(&AgentEvent::ToolStarted {
                id: id.clone(),
                name: "codemode".into(),
                args: serde_json::json!({"code":"await tools.read({path: 'README.md'})"}),
            });
            let mut baseline = None;
            for label in [
                "Running JavaScript (quickjs; deadline 120000 ms, up to 256 calls)",
                "Preparing frozen tool snapshot",
                "Nested tool 1/256",
                "Running JavaScript",
                "Nested tool 256/256",
            ] {
                shell.on_agent_event(&AgentEvent::ToolProgress {
                    id: id.clone(),
                    progress: ToolProgress::Decoration(
                        ToolProgressDecoration::new(label, None).unwrap(),
                    ),
                });
                let rows = frame(&component, width);
                let history = history_positions(&rows);
                assert!(!history.is_empty());
                if let Some(before) = &baseline {
                    assert_eq!(&history, before, "{width}: {rows:?}");
                } else {
                    baseline = Some(history);
                }
                assert_eq!(rows.iter().filter(|row| row.contains("Working")).count(), 1);
            }
        }
    }
}
