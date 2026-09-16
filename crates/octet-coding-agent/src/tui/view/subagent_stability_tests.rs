//! Compact subagent presentation stability regressions.
use super::*;

fn child() -> octet_agent::DelegationTelemetryChild {
    octet_agent::DelegationTelemetryChild {
        child_id: "worker-1".into(),
        task_name: "inspect-markdown".into(),
        profile: None,
        model: "fixture".into(),
        state: "running".into(),
        phase: "thinking".into(),
        current_tool: None,
        tool_use_count: 4,
        input_tokens: 12_000,
        cache_read_tokens: 800,
        cache_write_tokens: 0,
        output_tokens: 220,
        reasoning_tokens: 60,
        total_tokens: 13_020,
        cost: None,
        cost_microdollars: Some(7_200),
        elapsed_ms: 42_000,
        failure_class: None,
        failure_reason: None,
        effective_tool_policy: octet_agent::SandboxConfig::new(".")
            .effective_tool_policy(octet_agent::EffectPolicy::Controlled),
        orchestration_provenance: octet_agent::DelegationOrchestrationProvenance::all(
            octet_agent::DelegationPolicySource::ParentInherited,
        ),
        session: Some("agent-session:opaque".into()),
    }
}

fn publish(shell: &mut InteractiveShell, child: &octet_agent::DelegationTelemetryChild) {
    shell.on_agent_event(&AgentEvent::DelegationUpdated {
        snapshot: octet_agent::DelegationTelemetrySnapshot {
            revision: child.elapsed_ms,
            captured_at_ms: child.elapsed_ms,
            children: vec![child.clone()],
            total_cost_microdollars: child.cost_microdollars,
            failure_reason: None,
            failure_class: None,
        },
    });
}

#[test]
fn compact_subagent_rows_retain_metrics_during_short_tools_and_keep_latest_telemetry() {
    for width in [20, 46, 80, 120] {
        for verbose in [false, true] {
            let mut shell = InteractiveShell::test_shell();
            shell.state.borrow_mut().verbose_tools = verbose;
            let mut child = child();
            publish(&mut shell, &child);
            let index = shell.state.borrow().subagent_activity_block.unwrap();
            let rows = shell.state.borrow().rendered_transcript(width).clone();
            let revision = shell.state.borrow().block_revisions[index];
            for tool in [Some("read"), None, Some("bash"), None] {
                child.current_tool = tool.map(str::to_owned);
                child.phase = tool.unwrap_or("thinking").into();
                child.elapsed_ms += 250;
                child.tool_use_count += 1;
                publish(&mut shell, &child);
                assert_eq!(*shell.state.borrow().rendered_transcript(width), rows);
                let state = shell.state.borrow();
                assert_eq!(state.block_revisions[index], revision);
                assert_eq!(
                    state.subagent_activity.as_ref().unwrap().telemetry[0],
                    child
                );
            }
            child.output_tokens += 100;
            publish(&mut shell, &child);
            assert!(shell.state.borrow().block_revisions[index] > revision);
            for state in ["failed", "running", "completed"] {
                child.state = state.into();
                child.failure_reason = (state == "failed").then(|| "provider unavailable".into());
                publish(&mut shell, &child);
                let rows = shell.state.borrow().rendered_transcript(120).join("\n");
                assert!(strip_terminal_sequences(&rows).contains(state), "{rows}");
            }
        }
    }
}

#[test]
fn live_subagent_marker_does_not_animate_or_invalidate_historical_rows() {
    let mut shell = InteractiveShell::test_shell();
    publish(&mut shell, &child());
    let index = shell.state.borrow().subagent_activity_block.unwrap();
    for n in 0..40 {
        shell.notice(format!("HISTORY-{n}"));
    }
    let baseline = shell.state.borrow().rendered_transcript(80).clone();
    let revision = shell.state.borrow().block_revisions[index];
    for _ in 0..6 {
        shell.state.borrow_mut().advance_event_dot_animation();
        assert_eq!(shell.state.borrow().block_revisions[index], revision);
        assert_eq!(*shell.state.borrow().rendered_transcript(80), baseline);
    }
    let state = shell.state.borrow();
    let block = &state.transcript[index];
    let marker = super::surface_frame::event_margin_marker_with_frame;
    assert_eq!(
        marker(block, &state.theme, 0, None, 0, false),
        marker(block, &state.theme, 1, None, 0, false)
    );
}

#[test]
fn fallback_subagent_call_counts_are_retained_without_invalidating_rows() {
    let shell = InteractiveShell::test_shell();
    let mut activity = octet_agent::ExtensionPresentationActivity {
        id: "worker-1".into(),
        kind: "subagent".into(),
        state: octet_agent::ExtensionPresentationState::Running,
        summary: "inspect-markdown".into(),
        provenance: None,
        started_at_ms: None,
        completed_at_ms: None,
        metrics: Some(octet_agent::ExtensionPresentationMetrics {
            tool_calls: 1,
            input_tokens: 200,
            output_tokens: 20,
            cost_microdollars: Some(10),
            ..Default::default()
        }),
        references: Vec::new(),
    };
    let view = |activity: &octet_agent::ExtensionPresentationActivity| SubagentActivityView {
        activities: vec![activity.clone()],
        ..Default::default()
    };
    shell
        .state
        .borrow_mut()
        .set_subagent_activity(view(&activity));
    let index = shell.state.borrow().subagent_activity_block.unwrap();
    let rows = shell.state.borrow().rendered_transcript(80).clone();
    let revision = shell.state.borrow().block_revisions[index];
    activity.metrics.as_mut().unwrap().tool_calls = 99;
    shell
        .state
        .borrow_mut()
        .set_subagent_activity(view(&activity));
    assert_eq!(shell.state.borrow().block_revisions[index], revision);
    assert_eq!(*shell.state.borrow().rendered_transcript(80), rows);
    assert_eq!(
        shell
            .state
            .borrow()
            .subagent_activity
            .as_ref()
            .unwrap()
            .activities[0],
        activity
    );
    activity.metrics.as_mut().unwrap().output_tokens += 100;
    shell
        .state
        .borrow_mut()
        .set_subagent_activity(view(&activity));
    assert!(shell.state.borrow().block_revisions[index] > revision);
    assert_ne!(*shell.state.borrow().rendered_transcript(80), rows);
}

// Exercise both native telemetry and the extension-presentation fallback at
// their public shell entry points; both receive session-wide rosters.
fn publish_roster(
    shell: &mut InteractiveShell,
    native: bool,
    children: &[octet_agent::DelegationTelemetryChild],
) {
    if native {
        shell.on_agent_event(&AgentEvent::DelegationUpdated {
            snapshot: octet_agent::DelegationTelemetrySnapshot {
                revision: 1,
                captured_at_ms: 1,
                children: children.to_vec(),
                total_cost_microdollars: None,
                failure_reason: None,
                failure_class: None,
            },
        });
    } else {
        let snapshot = serde_json::from_value(serde_json::json!({
            "revision": 1,
            "status": {"state": "active", "label": "Subagents"},
            "activities": children.iter().map(|child| serde_json::json!({
                "id": child.child_id,
                "kind": "subagent",
                "state": if child.state == "completed" { "succeeded" } else { child.state.as_str() },
                "summary": child.task_name,
                "metrics": {
                    "input_tokens": child.input_tokens,
                    "output_tokens": child.output_tokens,
                    "cost_microdollars": child.cost_microdollars,
                },
            })).collect::<Vec<_>>(),
            "actions": [],
        })).unwrap();
        shell.set_subagent_presentation(Some(&snapshot), true);
    }
}

fn named_worker(name: &str, state: &str) -> octet_agent::DelegationTelemetryChild {
    octet_agent::DelegationTelemetryChild {
        child_id: name.into(),
        task_name: name.into(),
        state: state.into(),
        ..child()
    }
}

fn transcript_text(shell: &InteractiveShell) -> String {
    strip_terminal_sequences(&shell.state.borrow().rendered_transcript(120).join("\n"))
}

#[test]
fn mixed_session_rosters_do_not_replay_known_or_unseen_terminal_workers() {
    for native in [false, true] {
        let mut shell = InteractiveShell::test_shell();
        let run = shell.begin_run("fixture");
        shell.on_prompt_submitted("first prompt");
        publish_roster(&mut shell, native, &[named_worker("OLD-WORKER", "running")]);
        publish_roster(&mut shell, native, &[named_worker("OLD-WORKER", "completed")]);
        shell.on_run_event(
            run,
            &AgentEvent::RunFinished {
                head: octet_agent::EntryId("head".into()),
                reason: octet_agent::FinishReason::Completed,
            },
        );

        shell.begin_run("fixture");
        shell.on_prompt_submitted("second prompt");
        shell.state.borrow_mut().session_cost_microdollars = Some(100_000);
        let old = named_worker("OLD-WORKER", "completed");
        let unseen = named_worker("UNSEEN-OLD-WORKER", "completed");
        // A first-observed all-terminal roster is history, even after resume.
        publish_roster(
            &mut shell,
            native,
            &[old.clone(), named_worker("FINISHED-BEFORE-ATTACH", "completed")],
        );
        assert!(shell.state.borrow().subagent_activity_block.is_none());

        let fresh = named_worker("FRESH-WORKER", "running");
        publish_roster(&mut shell, native, &[old.clone(), unseen.clone(), fresh.clone()]);
        let index = shell.state.borrow().subagent_activity_block.unwrap();
        let text = transcript_text(&shell);
        let tail = text.split_once("second prompt").unwrap().1;
        assert!(tail.contains("FRESH-WORKER"), "{text}");
        assert!(!tail.contains("OLD-WORKER"), "{text}");
        assert_eq!(text.matches("Subagents").count(), 2, "{text}");
        assert_eq!(
            shell.state.borrow().displayed_session_cost_microdollars(),
            Some(107_200)
        );

        let settled = named_worker("FRESH-WORKER", "completed");
        publish_roster(&mut shell, native, &[old.clone(), unseen.clone(), settled.clone()]);
        let settled_text = transcript_text(&shell);
        for _ in 0..3 {
            publish_roster(&mut shell, native, &[old.clone(), unseen.clone(), settled.clone()]);
            assert_eq!(shell.state.borrow().subagent_activity_block, Some(index));
            assert_eq!(transcript_text(&shell), settled_text);
            assert!(shell_chrome(&shell.state.borrow(), 120, Instant::now()).subagents.is_empty());
        }

        // The same durable worker can legitimately be continued later, after
        // the owning parent run (not just its workers) has settled.
        let run = shell.current_run_id().unwrap();
        shell.on_run_event(run, &AgentEvent::RunFinished {
            head: octet_agent::EntryId("second-head".into()),
            reason: octet_agent::FinishReason::Completed,
        });
        shell.begin_run("fixture");
        shell.on_prompt_submitted("third prompt");
        publish_roster(&mut shell, native, &[old, unseen, settled]);
        assert!(shell.state.borrow().subagent_activity_block.is_none());
        let text = transcript_text(&shell);
        assert!(!text.split_once("third prompt").unwrap().1.contains("Subagents"));
        publish_roster(&mut shell, native, &[fresh]);
        let text = transcript_text(&shell);
        assert!(text.split_once("third prompt").unwrap().1.contains("FRESH-WORKER"));
    }
}

#[test]
fn live_strip_is_active_only_and_settles_in_the_original_transcript_block() {
    for native in [false, true] {
        let mut shell = InteractiveShell::test_shell();
        shell.begin_run("fixture");
        shell.on_prompt_submitted("owning prompt");
        publish_roster(&mut shell, native, &[
            named_worker("LIVE-WORKER", "running"),
            named_worker("DONE-WORKER", "running"),
        ]);
        let index = shell.state.borrow().subagent_activity_block.unwrap();
        publish_roster(&mut shell, native, &[
            named_worker("LIVE-WORKER", "running"),
            named_worker("DONE-WORKER", "completed"),
        ]);
        // Transcript controls must not hide the live worker in the strip.
        shell.state.borrow_mut().subagent_activity.as_mut().unwrap().state_filter =
            Some(SubagentStateGroup::Failed);
        let chrome = shell_chrome(&shell.state.borrow(), 120, Instant::now());
        let strip = strip_terminal_sequences(&chrome.subagents.join("\n"));
        assert!(strip.contains("LIVE-WORKER"), "{strip}");
        assert!(!strip.contains("DONE-WORKER"), "{strip}");
        assert!(!strip.contains("completed"), "{strip}");
        for (width, height) in [(24, 4), (46, 8), (120, 40)] {
            shell.set_size(width, height);
            let state = shell.state.borrow();
            let chrome = shell_chrome(&state, width, Instant::now());
            assert!(shell_chrome::shell_chrome_rows(&chrome) <= usize::from(height));
            assert!(chrome.subagents.iter().all(|row| visible_width(row) <= usize::from(width)));
            assert!(chrome.composer.iter().any(|row| row.contains(sexy_tui_rs::CURSOR_MARKER)));
        }
        publish_roster(&mut shell, native, &[
            named_worker("LIVE-WORKER", "completed"),
            named_worker("DONE-WORKER", "completed"),
        ]);
        assert_eq!(shell.state.borrow().subagent_activity_block, Some(index));
        assert!(shell_chrome(&shell.state.borrow(), 120, Instant::now()).subagents.is_empty());
        let text = transcript_text(&shell);
        assert_eq!(text.matches("Subagents").count(), 1, "{text}");
        assert!(text.contains("LIVE-WORKER") && text.contains("DONE-WORKER"), "{text}");
    }
}
