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
