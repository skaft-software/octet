//! Compact roster history and bounded live-wave geometry.
use super::*;

fn snapshot(revision: u64, states: &[(&str, &str)]) -> octet_agent::DelegationTelemetrySnapshot {
    octet_agent::DelegationTelemetrySnapshot {
        revision,
        captured_at_ms: revision,
        children: states
            .iter()
            .map(|(name, state)| live_region_stability_tests::worker(name, state))
            .collect(),
        total_cost_microdollars: Some(0),
        failure_class: None,
        failure_reason: None,
    }
}

fn rosters(shell: &InteractiveShell) -> Vec<SubagentTranscript> {
    shell
        .state
        .borrow()
        .transcript
        .iter()
        .filter_map(|block| match block {
            TranscriptBlock::Subagents(summary) => Some(summary.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn roster_repeated_settled_waits_and_status_keep_one_block() {
    let mut shell = InteractiveShell::test_shell();
    shell.begin_run("fixture");
    shell.set_subagent_telemetry(Some(&snapshot(1, &[("worker", "running")])), true);
    shell.set_subagent_telemetry(Some(&snapshot(2, &[("worker", "completed")])), true);
    for (revision, name) in [
        (3, "subagent_wait"),
        (4, "subagent_status"),
        (5, "subagent_wait"),
    ] {
        let id = ToolCallId(format!("wait-{revision}"));
        shell.on_agent_event(&AgentEvent::ToolStarted {
            id: id.clone(),
            name: name.into(),
            args: serde_json::json!({}),
        });
        shell.set_subagent_telemetry(Some(&snapshot(revision, &[("worker", "completed")])), true);
        shell.on_agent_event(&AgentEvent::ToolFinished {
            id,
            result: Ok(octet_agent::ToolOutput::new("unchanged")),
            duration: Duration::ZERO,
        });
        assert_eq!(rosters(&shell).len(), 1);
    }
}

#[test]
fn roster_identical_settlement_is_omitted_but_distinct_workers_keep_their_wave() {
    let mut shell = InteractiveShell::test_shell();
    for (revision, name, state) in [
        (1, "worker", "running"),
        (2, "worker", "completed"),
        (3, "worker", "running"),
        (4, "worker", "completed"),
    ] {
        shell.set_subagent_telemetry(Some(&snapshot(revision, &[(name, state)])), true);
        // Materialize every version so settlement must also remove cached rows.
        let _ = shell.state.borrow().rendered_transcript(120);
    }
    assert_eq!(rosters(&shell).len(), 1);
    assert_eq!(
        shell
            .state
            .borrow()
            .rendered_transcript(120)
            .join("\n")
            .matches("Subagents")
            .count(),
        1
    );
    assert!(shell.state.borrow().active_event_blocks.is_empty());
    shell.set_subagent_telemetry(Some(&snapshot(5, &[("next", "running")])), true);
    shell.set_subagent_telemetry(Some(&snapshot(6, &[("next", "completed")])), true);
    assert_eq!(rosters(&shell).len(), 2);
}

#[test]
fn roster_reserves_only_detail_rows_used_by_the_live_wave_and_releases_on_settle() {
    let mut shell = InteractiveShell::test_shell();
    let mut states = vec![("first", "running")];
    shell.set_subagent_telemetry(Some(&snapshot(1, &states)), true);
    for (index, name) in ["second", "third", "fourth", "fifth", "sixth"]
        .into_iter()
        .enumerate()
    {
        // Older members finish before a new member arrives: this wave never
        // displays more than one live worker, despite its growing membership.
        states.last_mut().unwrap().1 = "completed";
        states.push((name, "running"));
        shell.set_subagent_telemetry(Some(&snapshot(index as u64 + 2, &states)), true);
        let summaries = rosters(&shell);
        let summary = summaries.last().unwrap();
        assert_eq!(summary.reserved_worker_rows(), 1);
        let state = shell.state.borrow();
        let rendered = render_block_planned(
            None,
            &TranscriptBlock::Subagents(summary.clone()),
            &state.theme,
            &state.theme.rich_renderer(),
            &state.theme.reasoning_renderer(),
            120,
            false,
            0,
            0,
        );
        assert_eq!(
            rendered.lines.len() - rendered.geometry.leading_rows - rendered.geometry.trailing_rows,
            2
        );
        let native = tern_agents::transcript(1, summary, false);
        assert_eq!(native.c.unwrap()[0].c.as_ref().unwrap().len(), 1);
    }
    for worker in &mut states {
        worker.1 = "completed";
    }
    shell.set_subagent_telemetry(Some(&snapshot(10, &states)), true);
    assert_eq!(rosters(&shell)[0].reserved_worker_rows(), 0);
    shell.set_subagent_telemetry(Some(&snapshot(11, &[("new-wave", "running")])), true);
    assert_eq!(rosters(&shell).last().unwrap().reserved_worker_rows(), 1);
}

#[test]
fn roster_compaction_during_a_live_wave_keeps_one_summary() {
    let mut shell = InteractiveShell::test_shell();
    let run = shell.begin_run("fixture");
    shell.set_subagent_telemetry(Some(&snapshot(1, &[("worker", "running")])), true);
    shell.on_run_event(
        run,
        &AgentEvent::CompactionStarted {
            reason: octet_agent::CompactionReason::Overflow,
        },
    );
    shell.on_run_event(
        run,
        &AgentEvent::CompactionFinished {
            reason: octet_agent::CompactionReason::Overflow,
            result: Ok(octet_agent::CompactionInfo {
                kind: octet_agent::CompactionKind::Local,
                summary: "checkpoint".into(),
                first_kept: octet_agent::EntryId("kept".into()),
                usage: octet_ai::Usage::default(),
                elapsed: Duration::ZERO,
                cost_microdollars: None,
            }),
        },
    );
    for revision in 2..5 {
        shell.set_subagent_telemetry(Some(&snapshot(revision, &[("worker", "running")])), true);
        assert_eq!(rosters(&shell).len(), 1);
    }
    shell.set_subagent_telemetry(Some(&snapshot(5, &[("worker", "completed")])), true);
    assert_eq!(rosters(&shell).len(), 1);
    assert_eq!(rosters(&shell)[0].reserved_worker_rows(), 0);
    assert!(shell
        .state
        .borrow()
        .rendered_transcript(120)
        .join("\n")
        .contains("Context compacted automatically"));
}
