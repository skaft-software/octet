//! Subagent rosters, worker rows, telemetry, and the terminal snapshot. Separate because they
//! assert the worker chrome and must not disturb the parent transcript.

use super::support::*;

use super::*;

#[test]
fn extension_tools_render_label_and_argument_like_core_tools() {
    let mut shell = InteractiveShell::test_shell();
    shell.on_agent_event(&octet_agent::AgentEvent::ToolStarted {
        id: octet_ai::ToolCallId("ext-call".into()),
        name: "web_search".into(),
        args: serde_json::json!({"query": "rust tui transcript"}),
    });
    let transcript =
        strip_terminal_sequences(&shell.state.borrow().rendered_transcript(120).join("\n"));
    assert!(transcript.contains("Web search"), "{transcript}");
    assert!(transcript.contains("rust tui transcript"), "{transcript}");
    // The tool name must not be duplicated in the value column, and the
    // legacy "Used" lead stays gone.
    assert!(!transcript.contains("Used "), "{transcript}");
    assert!(!transcript.contains("web search: rust tui"), "{transcript}");

    shell.on_agent_event(&octet_agent::AgentEvent::ToolStarted {
        id: octet_ai::ToolCallId("ssh-call".into()),
        name: "ssh_exec".into(),
        args: serde_json::json!({"argv": ["cargo", "test"]}),
    });
    let transcript =
        strip_terminal_sequences(&shell.state.borrow().rendered_transcript(120).join("\n"));
    assert!(transcript.contains("SSH"), "{transcript}");
    assert!(!transcript.contains("Ssh"), "{transcript}");
}

#[test]
fn hydrating_a_replacement_session_clears_subagent_activity() {
    let mut shell = InteractiveShell::test_shell();
    let snapshot = octet_agent::DelegationTelemetrySnapshot {
        revision: 1,
        captured_at_ms: 1_700_000_000_000,
        children: vec![octet_agent::DelegationTelemetryChild {
            child_id: "agent-1".into(),
            task_name: "Inspect tests".into(),
            profile: Some("explore".into()),
            model: "test-model".into(),
            state: "running".into(),
            phase: "using_tool".into(),
            current_tool: Some("read".into()),
            tool_use_count: 1,
            input_tokens: 100,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            estimated_output_tokens: None,
            output_tokens: 10,
            reasoning_tokens: 0,
            total_tokens: 110,
            cost: None,
            cost_microdollars: Some(1),
            elapsed_ms: 500,
            failure_class: None,
            failure_reason: None,
            effective_tool_policy: test_effective_tool_policy(),
            orchestration_provenance: inherited_delegation_provenance(),
            session: Some("agent-session:opaque".into()),
        }],
        total_cost_microdollars: Some(1),
        failure_reason: None,
        failure_class: None,
    };
    assert!(shell.set_subagent_telemetry(Some(&snapshot), true));
    assert!(shell.state.borrow().subagent_activity.is_some());
    let directory = tempfile::tempdir().unwrap();
    let session = Session::create(directory.path().join("replacement.jsonl")).unwrap();

    shell.hydrate(&session).unwrap();

    assert!(shell.state.borrow().subagent_activity.is_none());
    assert!(shell_chrome(&shell.state.borrow(), 120, Instant::now())
        .subagents
        .is_empty());
}

/// Maintainer report: "EVERY new prompt shows the current session's subagents
/// even if they're completed!" The delegation team is session-scoped, so its
/// final snapshot keeps being republished after the turn it belongs to has
/// ended. The single settled row must remain above later prompts, not replay.
#[test]
fn a_settled_subagent_roster_never_replays_under_a_later_prompt() {
    use octet_agent::{EntryId, FinishReason};

    let mut shell = InteractiveShell::test_shell();
    let child = |id: &str, state: &str| octet_agent::DelegationTelemetryChild {
        child_id: id.into(),
        task_name: "Inspect tests".into(),
        profile: Some("explore".into()),
        model: "test-model".into(),
        state: state.into(),
        phase: "using_tool".into(),
        current_tool: Some("read".into()),
        tool_use_count: 1,
        input_tokens: 100,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        estimated_output_tokens: None,
        output_tokens: 10,
        reasoning_tokens: 0,
        total_tokens: 110,
        cost: None,
        cost_microdollars: Some(1),
        elapsed_ms: 500,
        failure_class: None,
        failure_reason: None,
        effective_tool_policy: test_effective_tool_policy(),
        orchestration_provenance: inherited_delegation_provenance(),
        session: Some("agent-session:opaque".into()),
    };
    let snapshot = |revision: u64, children: Vec<octet_agent::DelegationTelemetryChild>| {
        octet_agent::DelegationTelemetrySnapshot {
            revision,
            captured_at_ms: 1_700_000_000_000 + revision,
            children,
            total_cost_microdollars: Some(1),
            failure_reason: None,
            failure_class: None,
        }
    };
    let delegation = |snapshot: octet_agent::DelegationTelemetrySnapshot| {
        octet_agent::AgentEvent::DelegationUpdated { snapshot }
    };
    let transcripts = |shell: &InteractiveShell| {
        shell
            .state
            .borrow()
            .rendered_transcript(120)
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .collect::<Vec<_>>()
            .join("\n")
    };

    // Turn one: the live strip disappears on settlement without history rows.
    let run_id = shell.begin_run("test-provider");
    shell.on_run_event(
        run_id,
        &delegation(snapshot(1, vec![child("agent-1", "running")])),
    );
    shell.on_run_event(
        run_id,
        &delegation(snapshot(2, vec![child("agent-1", "completed")])),
    );
    let settled = transcripts(&shell);
    assert!(settled.contains("Subagents · 1 completed"), "{settled}");
    assert!(shell_chrome(&shell.state.borrow(), 120, Instant::now())
        .subagents
        .is_empty());
    assert!(!settled.contains("Inspect tests"), "{settled}");
    shell.on_run_event(
        run_id,
        &AgentEvent::RunFinished {
            head: EntryId("head".into()),
            reason: FinishReason::Completed,
        },
    );
    let after_finish = transcripts(&shell);
    assert!(
        after_finish.contains("Subagents · 1 completed"),
        "{after_finish}"
    );

    // Turn two: the endpoint republishes the same, already-completed roster.
    shell.begin_run("test-provider");
    shell.on_prompt_submitted("next question");
    shell.on_agent_event(&delegation(snapshot(
        3,
        vec![child("agent-1", "completed")],
    )));

    let state = shell.state.borrow();
    assert!(
        state.subagent_activity.is_some(),
        "the session roster remains available across root turns"
    );
    assert!(
        shell_chrome(&state, 120, Instant::now())
            .subagents
            .is_empty(),
        "the settled session roster never reappears in chrome"
    );
    let replayed = state
        .rendered_transcript(120)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>()
        .join("\n");
    // Neither this turn nor the prior turn gets a synthetic roster block.
    assert_eq!(
        replayed.matches("Subagents").count(),
        1,
        "the completed roster must not be rendered again: {replayed}"
    );
    assert!(replayed.contains("next question"));
    // And the new turn's own status row is still the last thing in the
    // transcript, so the working indicator is not displaced.
    assert!(
        replayed
            .trim_end()
            .ends_with("Working (0s • esc to interrupt)")
            || replayed.trim_end().ends_with("Working"),
        "the new turn's status row must remain the transcript tail: {replayed}"
    );
    // Nothing below the new prompt belongs to the settled team: the correction
    // was that EVERY further prompt replayed a completed session roster.
    let rows = state
        .rendered_transcript(120)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>();
    let prompt_row = rows
        .iter()
        .position(|row| row.contains("next question"))
        .expect("the new prompt is a rendered row");
    let below = rows[prompt_row + 1..].join("\n");
    for replayed_again in ["Subagents", "Inspect tests", "completed"] {
        assert!(
            !below.contains(replayed_again),
            "the completed roster replayed below the new prompt: {rows:?}"
        );
    }
}

/// The same rule with a live worker: a roster for the *current* turn still
/// renders, so the fix is attribution and not a blanket suppression.
#[test]
fn live_workers_for_a_later_turn_open_a_new_transcript_row() {
    use octet_agent::{EntryId, FinishReason};

    let mut shell = InteractiveShell::test_shell();
    shell.set_size(120, 60);
    let live = |id: &str| octet_agent::DelegationTelemetryChild {
        child_id: id.into(),
        task_name: format!("Inspect tests {id}"),
        profile: Some("explore".into()),
        model: "test-model".into(),
        state: "running".into(),
        phase: "using_tool".into(),
        current_tool: Some("read".into()),
        tool_use_count: 1,
        input_tokens: 100,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        estimated_output_tokens: None,
        output_tokens: 10,
        reasoning_tokens: 0,
        total_tokens: 110,
        cost: None,
        cost_microdollars: Some(1),
        elapsed_ms: 500,
        failure_class: None,
        failure_reason: None,
        effective_tool_policy: test_effective_tool_policy(),
        orchestration_provenance: inherited_delegation_provenance(),
        session: Some("agent-session:opaque".into()),
    };
    let snapshot = |children| octet_agent::DelegationTelemetrySnapshot {
        revision: 1,
        captured_at_ms: 1_700_000_000_000,
        children,
        total_cost_microdollars: Some(1),
        failure_reason: None,
        failure_class: None,
    };

    // Turn one settles one worker.
    let run_id = shell.begin_run("test-provider");
    shell.on_run_event(
        run_id,
        &AgentEvent::DelegationUpdated {
            snapshot: snapshot(vec![live("agent-1")]),
        },
    );
    let settled_child = octet_agent::DelegationTelemetryChild {
        state: "completed".into(),
        ..live("agent-1")
    };
    shell.on_run_event(
        run_id,
        &AgentEvent::DelegationUpdated {
            snapshot: octet_agent::DelegationTelemetrySnapshot {
                revision: 2,
                captured_at_ms: 1_700_000_000_002,
                ..snapshot(vec![settled_child.clone()])
            },
        },
    );
    shell.on_run_event(
        run_id,
        &AgentEvent::RunFinished {
            head: EntryId("head".into()),
            reason: FinishReason::Completed,
        },
    );

    // Turn two starts a genuinely new worker: the completed roster from turn
    // one must not appear again, and the new worker must.
    shell.begin_run("test-provider");
    shell.on_agent_event(&AgentEvent::DelegationUpdated {
        snapshot: octet_agent::DelegationTelemetrySnapshot {
            revision: 3,
            captured_at_ms: 1_700_000_000_003,
            children: vec![settled_child, live("agent-2")],
            total_cost_microdollars: Some(2),
            failure_reason: None,
            failure_class: None,
        },
    });
    let rendered =
        strip_terminal_sequences(&shell.state.borrow().rendered_transcript(120).join("\n"));
    assert_eq!(rendered.matches("Subagents").count(), 2, "{rendered}");
    assert!(
        rendered.contains("1 completed") && rendered.contains("1 running"),
        "{rendered}"
    );
    assert!(rendered.contains("Inspect tests agent-2"), "{rendered}");
    assert!(!rendered.contains("Inspect tests agent-1"), "{rendered}");
}

/// The maintainer's correction, spelled out: "EVERY new prompt shows the current
/// session's subagents even if they're completed!" A session-scoped roster that
/// has no live worker is not fresh turn material even when this transcript never
/// saw those workers live - a resumed session, an interrupted parent that settled
/// before its first snapshot, or a manager whose team finished before the UI was
/// attached. It must render nothing at all, and it must not displace the new
/// turn's `Working` row.
#[test]
fn an_all_completed_roster_never_opens_a_block_under_a_later_prompt() {
    use octet_agent::{EntryId, FinishReason};

    let mut shell = InteractiveShell::test_shell();
    let child = |id: &str, state: &str| octet_agent::DelegationTelemetryChild {
        child_id: id.into(),
        task_name: "Inspect tests".into(),
        profile: Some("explore".into()),
        model: "test-model".into(),
        state: state.into(),
        phase: "using_tool".into(),
        current_tool: None,
        tool_use_count: 1,
        input_tokens: 100,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        estimated_output_tokens: None,
        output_tokens: 10,
        reasoning_tokens: 0,
        total_tokens: 110,
        cost: None,
        cost_microdollars: Some(1),
        elapsed_ms: 500,
        failure_class: None,
        failure_reason: None,
        effective_tool_policy: test_effective_tool_policy(),
        orchestration_provenance: inherited_delegation_provenance(),
        session: Some("agent-session:opaque".into()),
    };
    let transcript = |shell: &InteractiveShell| {
        shell
            .state
            .borrow()
            .rendered_transcript(120)
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let ends_with_working = |rendered: &str| {
        rendered
            .trim_end()
            .ends_with("Working (0s • esc to interrupt)")
            || rendered.trim_end().ends_with("Working")
    };

    // Turn one is interrupted before the delegation is ever published live.
    let run_id = shell.begin_run("test-provider");
    shell.on_prompt_submitted("first question");
    shell.on_run_event(
        run_id,
        &AgentEvent::RunFinished {
            head: EntryId("head".into()),
            reason: FinishReason::Aborted,
        },
    );

    // Turn two: the reader submits a new prompt, and the session-scoped manager
    // republishes the roster with every worker already finished.
    shell.begin_run("test-provider");
    shell.on_prompt_submitted("second question");
    let submitted = transcript(&shell);
    assert!(
        ends_with_working(&submitted),
        "the new turn's working indicator must appear immediately: {submitted}"
    );

    shell.on_agent_event(&AgentEvent::DelegationUpdated {
        snapshot: octet_agent::DelegationTelemetrySnapshot {
            revision: 9,
            captured_at_ms: 1_700_000_000_009,
            children: vec![child("agent-1", "completed"), child("agent-2", "completed")],
            total_cost_microdollars: Some(2),
            failure_reason: None,
            failure_class: None,
        },
    });

    let state = shell.state.borrow();
    assert!(
        state.subagent_activity.is_none(),
        "an unseen all-completed roster must not become live state"
    );
    assert!(
        shell_chrome(&state, 120, Instant::now())
            .subagents
            .is_empty(),
        "an all-completed roster must not open a transcript block"
    );
    let rendered = state
        .rendered_transcript(120)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        rendered.matches("Subagents").count(),
        0,
        "completed workers produce no output at all: {rendered}"
    );
    assert!(!rendered.contains("Inspect tests"), "{rendered}");
    assert!(
        ends_with_working(&rendered),
        "the working indicator keeps its place after the roster settles: {rendered}"
    );
    assert!(
        shell_chrome(&state, 120, Instant::now())
            .subagents
            .iter()
            .all(|row| !row.contains("Subagents")),
        "nothing about the roster may be composed into pinned chrome"
    );
}

/// One live delegation creates one mutable transcript row. Settlement updates
/// that row, and a republished completed roster cannot add another.
#[test]
fn subagent_lifecycle_retains_one_row_without_republishing() {
    use octet_agent::{EntryId, FinishReason};

    let mut shell = InteractiveShell::test_shell();
    let child = |id: &str, state: &str| octet_agent::DelegationTelemetryChild {
        child_id: id.into(),
        task_name: "Inspect tests".into(),
        profile: Some("explore".into()),
        model: "test-model".into(),
        state: state.into(),
        phase: "using_tool".into(),
        current_tool: Some("read".into()),
        tool_use_count: 1,
        input_tokens: 100,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        estimated_output_tokens: None,
        output_tokens: 10,
        reasoning_tokens: 0,
        total_tokens: 110,
        cost: None,
        cost_microdollars: Some(1),
        elapsed_ms: 500,
        failure_class: None,
        failure_reason: None,
        effective_tool_policy: test_effective_tool_policy(),
        orchestration_provenance: inherited_delegation_provenance(),
        session: Some("agent-session:opaque".into()),
    };
    let snapshot = |revision: u64, children| octet_agent::DelegationTelemetrySnapshot {
        revision,
        captured_at_ms: 1_700_000_000_000 + revision,
        children,
        total_cost_microdollars: Some(1),
        failure_reason: None,
        failure_class: None,
    };
    let transcript_len = |shell: &InteractiveShell| shell.state.borrow().transcript.len();
    let strip = |shell: &InteractiveShell| {
        shell_chrome(&shell.state.borrow(), 120, Instant::now()).subagents
    };

    // Turn one: the delegation creates one live transcript row.
    let run_id = shell.begin_run("test-provider");
    shell.on_prompt_submitted("first question");
    let before_delegation = transcript_len(&shell);
    shell.on_run_event(
        run_id,
        &octet_agent::AgentEvent::DelegationUpdated {
            snapshot: snapshot(1, vec![child("agent-1", "running")]),
        },
    );
    assert_eq!(transcript_len(&shell), before_delegation + 1);
    assert!(strip(&shell).is_empty());

    // Settlement updates the same row; the end of the run leaves it in place.
    shell.on_run_event(
        run_id,
        &octet_agent::AgentEvent::DelegationUpdated {
            snapshot: snapshot(2, vec![child("agent-1", "completed")]),
        },
    );
    assert_eq!(transcript_len(&shell), before_delegation + 1);
    assert!(strip(&shell).is_empty());
    shell.on_run_event(
        run_id,
        &octet_agent::AgentEvent::RunFinished {
            head: EntryId("head".into()),
            reason: FinishReason::Aborted,
        },
    );
    assert!(strip(&shell).is_empty());

    // Turn two republishes the same completed roster without appending history.
    shell.begin_run("test-provider");
    shell.on_prompt_submitted("second question");
    let before_republish = transcript_len(&shell);
    shell.on_agent_event(&octet_agent::AgentEvent::DelegationUpdated {
        snapshot: snapshot(3, vec![child("agent-1", "completed")]),
    });
    assert_eq!(transcript_len(&shell), before_republish);
    assert!(strip(&shell).is_empty());
    shell.select_all_transcript();
    let copy = shell.copy_selected_plain_text().unwrap();
    assert!(copy.contains("first question") && copy.contains("second question"));
    assert_eq!(copy.matches("Subagents").count(), 1);
    assert!(!copy.contains("Inspect tests"));
}

/// Maintainer report: "no working indicator or thinking for a bit" after a new
/// prompt. A submitted prompt leaves the turn's liveness row directly below it,
/// with nothing active in between.
#[test]
fn a_new_prompt_shows_the_working_indicator_immediately() {
    let mut shell = InteractiveShell::test_shell();
    shell.begin_run("test-provider");
    shell.on_prompt_submitted("new question");

    let state = shell.state.borrow();
    let prompt = state
        .transcript
        .iter()
        .position(
            |block| matches!(block, TranscriptBlock::User { text, .. } if text == "new question"),
        )
        .expect("the prompt is in the transcript");
    let status = state
        .transcript
        .iter()
        .position(|block| {
            matches!(
                block,
                TranscriptBlock::Reasoning(reasoning)
                    if reasoning.reasoning_heading.as_deref() == Some("Working")
            )
        })
        .expect("the liveness row is open");
    assert_eq!(
        status,
        prompt + 1,
        "the working indicator must sit directly below the submitted prompt"
    );
    drop(state);
    let rows = shell
        .state
        .borrow()
        .rendered_transcript(120)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>();
    assert!(
        rows.last().is_some_and(|row| row.contains("Working")),
        "the new turn's status row must be the transcript tail: {rows:?}"
    );
}

#[test]
fn terminal_subagent_snapshots_hide_the_activity_strip() {
    let mut shell = InteractiveShell::test_shell();
    let child = |id: &str, state: &str| octet_agent::DelegationTelemetryChild {
        child_id: id.into(),
        task_name: "Inspect tests".into(),
        profile: Some("explore".into()),
        model: "test-model".into(),
        state: state.into(),
        phase: "using_tool".into(),
        current_tool: Some("read".into()),
        tool_use_count: 1,
        input_tokens: 100,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        estimated_output_tokens: None,
        output_tokens: 10,
        reasoning_tokens: 0,
        total_tokens: 110,
        cost: None,
        cost_microdollars: Some(1),
        elapsed_ms: 500,
        failure_class: None,
        failure_reason: None,
        effective_tool_policy: test_effective_tool_policy(),
        orchestration_provenance: inherited_delegation_provenance(),
        session: Some("agent-session:opaque".into()),
    };

    // A live worker gets one transcript row...
    shell.on_agent_event(&octet_agent::AgentEvent::DelegationUpdated {
        snapshot: octet_agent::DelegationTelemetrySnapshot {
            revision: 1,
            captured_at_ms: 1_700_000_000_000,
            children: vec![child("agent-1", "running")],
            total_cost_microdollars: Some(1),
            failure_reason: None,
            failure_class: None,
        },
    });
    assert!(shell.state.borrow().subagent_activity.is_some());
    assert!(shell_chrome(&shell.state.borrow(), 120, Instant::now())
        .subagents
        .is_empty());

    // ...and final settlement updates that row in place.
    shell.on_agent_event(&octet_agent::AgentEvent::DelegationUpdated {
        snapshot: octet_agent::DelegationTelemetrySnapshot {
            revision: 2,
            captured_at_ms: 1_700_000_000_001,
            children: vec![child("agent-1", "completed")],
            total_cost_microdollars: Some(1),
            failure_reason: None,
            failure_class: None,
        },
    });
    assert!(shell.state.borrow().subagent_activity.is_some());
    assert!(
        shell_chrome(&shell.state.borrow(), 120, Instant::now())
            .subagents
            .iter()
            .all(|row| !row.contains("Subagents")),
        "a settled roster never composes into pinned chrome"
    );
    let settled = shell
        .state
        .borrow()
        .rendered_transcript(120)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(settled.contains("Subagents · 1 completed"), "{settled}");
    assert!(!settled.contains("Inspect tests"), "{settled}");
    assert!(settled.contains("completed"), "{settled}");

    // A spawn failure is retained for the inspector, not synthetic history.
    shell.on_agent_event(&octet_agent::AgentEvent::DelegationUpdated {
        snapshot: octet_agent::DelegationTelemetrySnapshot {
            revision: 3,
            captured_at_ms: 1_700_000_000_002,
            children: Vec::new(),
            total_cost_microdollars: None,
            failure_reason: Some("spawn rejected: worker limit reached".into()),
            failure_class: Some("spawn_rejected".into()),
        },
    });
    assert!(shell.state.borrow().subagent_activity.is_some());
    assert_eq!(
        shell
            .state
            .borrow()
            .subagent_activity
            .as_ref()
            .unwrap()
            .failure_reason
            .as_deref(),
        Some("spawn rejected: worker limit reached")
    );
    assert!(shell_chrome(&shell.state.borrow(), 120, Instant::now())
        .subagents
        .is_empty());
    let failed = shell
        .state
        .borrow()
        .rendered_transcript(120)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !failed.contains("spawn rejected: worker limit reached"),
        "{failed}"
    );
}

#[test]
fn subagent_activity_renders_complete_roster_in_both_disclosure_modes() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(120, 120);
    let child = |id: &str, task: &str, state: &str| octet_agent::DelegationTelemetryChild {
        child_id: id.into(),
        task_name: task.into(),
        profile: Some("explore".into()),
        model: "test-model".into(),
        state: state.into(),
        phase: "using_tool".into(),
        current_tool: (state == "running").then(|| "read".into()),
        tool_use_count: 2,
        input_tokens: 100,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        estimated_output_tokens: None,
        output_tokens: 10,
        reasoning_tokens: 0,
        total_tokens: 110,
        cost: None,
        cost_microdollars: Some(1),
        elapsed_ms: 500,
        failure_class: None,
        failure_reason: None,
        effective_tool_policy: test_effective_tool_policy(),
        orchestration_provenance: inherited_delegation_provenance(),
        session: Some("agent-session:opaque".into()),
    };
    let snapshot = octet_agent::DelegationTelemetrySnapshot {
        revision: 1,
        captured_at_ms: 1_700_000_000_000,
        children: vec![
            child("agent-1", "Read release history", "running"),
            child("agent-2", "Audit release surface", "running"),
            child("agent-3", "Scan changelog", "completed"),
            child("agent-4", "Inspect tests", "running"),
            child("agent-5", "Check package map", "running"),
            child("agent-6", "Review docs", "completed"),
            child("agent-7", "Audit extensions", "running"),
            child("agent-8", "Verify release", "completed"),
        ],
        total_cost_microdollars: Some(3),
        failure_reason: None,
        failure_class: None,
    };
    publish_current_turn_roster(&mut shell, snapshot);

    let compact = shell
        .state
        .borrow()
        .rendered_transcript(120)
        .iter()
        .map(|row| strip_terminal_sequences(row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        compact.contains("Subagents · 5 running · 3 completed · /subagents"),
        "{compact}"
    );
    assert!(
        compact.contains("Read release history · ↑100 ↓10"),
        "{compact}"
    );
    assert!(compact.contains("+1 more"), "{compact}");
    assert!(!compact.contains("Verify release"), "{compact}");
    shell.toggle_disclosure();
    let expanded = shell.state.borrow().rendered_transcript(120).join("\n");
    assert!(strip_terminal_sequences(&expanded).contains("Subagents · 5 running · 3 completed"));
    assert_eq!(
        shell
            .state
            .borrow()
            .subagent_activity
            .as_ref()
            .unwrap()
            .telemetry
            .len(),
        8
    );

    // Settlement hides the strip regardless of disclosure, retaining telemetry.
    shell.on_agent_event(&octet_agent::AgentEvent::DelegationUpdated {
        snapshot: octet_agent::DelegationTelemetrySnapshot {
            revision: 2,
            captured_at_ms: 1_700_000_000_001,
            children: vec![
                child("agent-1", "Read release history", "completed"),
                child("agent-2", "Audit release surface", "completed"),
                child("agent-3", "Scan changelog", "completed"),
            ],
            total_cost_microdollars: Some(3),
            failure_reason: None,
            failure_class: None,
        },
    });
    shell.toggle_disclosure();
    let settled = shell
        .state
        .borrow()
        .rendered_transcript(120)
        .iter()
        .map(|row| strip_terminal_sequences(row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        settled.contains("Subagents · 3 completed · /subagents"),
        "{settled}"
    );
    assert_eq!(
        shell
            .state
            .borrow()
            .subagent_activity
            .as_ref()
            .unwrap()
            .telemetry
            .len(),
        3
    );
}

/// Ctrl+F narrows the dedicated worker inspector without filtering the
/// transcript lifecycle row.
#[test]
fn subagent_panel_keyboard_cycles_its_state_filter() {
    let mut shell = InteractiveShell::test_shell();
    open_grouped_subagent_panel(&mut shell, 2, 1);
    let before = strip_terminal_sequences(&render_shell(&shell.state.borrow(), 120).join("\n"));
    assert!(before.contains("live-0"), "{before}");
    shell.panel_input(&panel_key_with_modifiers(
        crossterm::event::KeyCode::Char('f'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    let filtered = strip_terminal_sequences(&render_shell(&shell.state.borrow(), 120).join("\n"));
    assert!(filtered.contains("Running"), "{filtered}");
    shell.panel_input(&panel_key_with_modifiers(
        crossterm::event::KeyCode::Char('f'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    let filtered = strip_terminal_sequences(&render_shell(&shell.state.borrow(), 120).join("\n"));
    assert!(filtered.contains("Done"), "{filtered}");
}

/// Enter confirms the drill-in for the selected worker and Esc closes the
/// panel without selecting one.
#[test]
fn subagent_panel_enter_opens_and_esc_closes() {
    let mut shell = InteractiveShell::test_shell();
    open_grouped_subagent_panel(&mut shell, 2, 0);
    let (result, action) = shell
        .panel_input(&panel_key(crossterm::event::KeyCode::Enter))
        .expect("enter confirms the selected worker");
    assert!(matches!(result, PanelResult::Confirm(0)), "{result:?}");
    assert!(matches!(action, PanelAction::SelectSubagent(_)));
    assert!(!shell.has_panel());

    open_grouped_subagent_panel(&mut shell, 2, 0);
    let (result, _) = shell
        .panel_input(&panel_key(crossterm::event::KeyCode::Esc))
        .expect("esc closes the panel");
    assert_eq!(result, PanelResult::Cancel);
    assert!(!shell.has_panel());
}

#[test]
fn extension_presentation_hides_terminal_subagent_activities() {
    let mut shell = InteractiveShell::test_shell();
    shell.begin_run("fixture");
    let snapshot = |state: &str| -> octet_agent::ExtensionPresentationSnapshot {
        serde_json::from_value(serde_json::json!({
            "revision": 1,
            "status": {"state": "active", "label": "Subagents"},
            "activities": [{
                "id": "activity:agent-1",
                "kind": "subagent",
                "state": state,
                "summary": "read-diffs · using read",
                "metrics": {
                    "tool_calls": 16,
                    "input_tokens": 80_000,
                    "cache_read_tokens": 8_200,
                    "cache_write_tokens": 0,
                    "output_tokens": 99,
                    "reasoning_tokens": 20,
                    "cost_microdollars": 208_600
                }
            }],
            "actions": []
        }))
        .unwrap()
    };

    assert!(shell.set_subagent_presentation(Some(&snapshot("running")), true));
    assert!(shell.state.borrow().subagent_activity.is_some());
    assert!(shell_chrome(&shell.state.borrow(), 120, Instant::now())
        .subagents
        .is_empty());

    // Terminal extension activities hide the strip but retain accounting.
    assert!(shell.set_subagent_presentation(Some(&snapshot("succeeded")), true));
    assert!(shell.state.borrow().subagent_activity.is_some());
    assert!(
        shell_chrome(&shell.state.borrow(), 120, Instant::now())
            .subagents
            .iter()
            .all(|row| !row.contains("Subagents")),
        "a settled roster never composes into pinned chrome"
    );
    let settled = shell
        .state
        .borrow()
        .rendered_transcript(120)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(settled.contains("Subagents · 1 completed"), "{settled}");
    assert!(!settled.contains("read-diffs · using read"), "{settled}");
    assert!(settled.contains("completed"), "{settled}");
}
