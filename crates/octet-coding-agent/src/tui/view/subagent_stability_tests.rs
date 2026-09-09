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
