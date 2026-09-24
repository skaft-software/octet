//! Transcript subagent projection and inspector telemetry stability regressions.
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
fn subagent_live_lines_update_without_invalidating_earlier_history() {
    for width in [20, 46, 80, 120, 240] {
        for verbose in [false, true] {
            let mut shell = InteractiveShell::test_shell();
            shell.set_size(width, 24);
            shell.state.borrow_mut().verbose_tools = verbose;
            shell.notice("HISTORY");
            let mut child = child();
            publish(&mut shell, &child);
            let history = shell.state.borrow().rendered_transcript(width)[0].clone();
            let revisions = shell.state.borrow().block_revisions.clone();
            for tool in [Some("read"), None, Some("bash"), None] {
                child.current_tool = tool.map(str::to_owned);
                child.phase = tool.unwrap_or("thinking").into();
                child.total_tokens += 1_000;
                child.elapsed_ms += 250;
                child.tool_use_count += 1;
                publish(&mut shell, &child);
                let state = shell.state.borrow();
                let first_row = state.rendered_transcript(width)[0].clone();
                assert_eq!(first_row, history);
                assert_eq!(
                    &state.block_revisions[..revisions.len() - 1],
                    &revisions[..revisions.len() - 1]
                );
                assert_eq!(
                    state.subagent_activity.as_ref().unwrap().telemetry[0],
                    child
                );
                drop(state);
                let frame = frame_text(&shell, width);
                assert!(frame.contains("/subagents"), "{width}: {frame}");
                if width >= 46 {
                    assert!(frame.contains("inspect-markdown"), "{frame}");
                    assert!(
                        frame.contains(&format!("{}K tok", child.total_tokens / 1_000)),
                        "{frame}"
                    );
                }
            }
            for status in ["completed", "running", "completed"] {
                child.state = status.into();
                publish(&mut shell, &child);
                let updated = shell.state.borrow().rendered_transcript(width).join("\n");
                assert!(updated.contains("/subagents"));
                assert!(
                    matches!(shell.state.borrow().transcript.last(), Some(TranscriptBlock::Subagents(summary)) if if status == "running" { summary.running == 1 } else { summary.succeeded == 1 }),
                    "{updated}"
                );
                assert_eq!(
                    shell
                        .state
                        .borrow()
                        .subagent_activity
                        .as_ref()
                        .unwrap()
                        .telemetry[0],
                    child
                );
            }
        }
    }
}

#[test]
fn late_failed_or_parked_workers_remain_inspectable_without_transcript_notices() {
    for status in [
        "failed",
        "timed_out",
        "interrupted",
        "stopped",
        "shutdown",
        "detached",
        "awaiting_approval",
    ] {
        let mut shell = InteractiveShell::test_shell();
        shell.notice("UNRELATED-NOTICE");
        let run = shell.begin_run("fixture");
        let mut worker = named_worker("LATE-WORKER", "running");
        publish(&mut shell, &worker);
        shell.on_run_event(
            run,
            &AgentEvent::RunFinished {
                head: octet_agent::EntryId("head".into()),
                reason: octet_agent::FinishReason::Completed,
            },
        );
        assert!(!shell.state.borrow().run.is_active());
        assert!(frame_text(&shell, 120).contains("Subagents"));
        let block_count = shell.state.borrow().transcript.len();
        worker.state = status.into();
        worker.failure_reason = Some(format!(
            "REASON-MARKER \x1b[3J\x1b]52;c;SECRET\x07\x00\x07 {}",
            "é".repeat(5000)
        ));
        publish(&mut shell, &worker);
        assert!(shell_chrome(&shell.state.borrow(), 120, Instant::now())
            .subagents
            .is_empty());
        let _ = shell.state.borrow().rendered_transcript(120);
        let revisions = shell.state.borrow().block_revisions.clone();
        let generation = shell.state.borrow().transcript_cache.borrow().generation;
        // Repeats, metric updates, and changed reasons all remain inspector-only.
        for update in [0, 1, 0, 2, 0] {
            if update == 1 {
                worker.tool_use_count += 1;
                worker.output_tokens += 100;
            } else if update == 2 {
                worker.failure_reason = Some("UPDATED-REASON".into());
            }
            publish(&mut shell, &worker);
            let state = shell.state.borrow();
            assert_eq!(state.transcript.len(), block_count);
            let rendered = state.rendered_transcript(120).join("\n");
            let copied = state
                .transcript
                .iter()
                .map(block_copy_text)
                .collect::<String>();
            for text in [&rendered, &copied] {
                assert!(text.contains("UNRELATED-NOTICE"), "{text}");
                assert!(text.contains("Subagents"), "{text}");
                for hidden in ["LATE-WORKER", "REASON", "SECRET"] {
                    assert!(!text.contains(hidden), "{text}");
                }
            }
            assert_eq!(state.block_revisions, revisions);
            assert_eq!(state.transcript_cache.borrow().generation, generation);
            let retained = state.subagent_activity.as_ref().unwrap();
            assert_eq!(retained.telemetry[0], worker);
            assert_ne!(
                SubagentStateGroup::of_declared_state(status),
                SubagentStateGroup::Completed
            );
            let reason = sanitize_for_terminal(worker.failure_reason.as_deref().unwrap());
            assert!(reason.contains("REASON"));
            assert!(!reason.contains("SECRET"));
        }
    }
}

#[test]
fn subagent_animation_never_invalidates_transcript_history() {
    let mut shell = InteractiveShell::test_shell();
    for n in 0..40 {
        shell.notice(format!("HISTORY-{n}"));
    }
    publish(&mut shell, &child());
    let baseline = shell.state.borrow().rendered_transcript(80).clone();
    let revisions = shell.state.borrow().block_revisions.clone();
    let generation = shell.state.borrow().transcript_cache.borrow().generation;
    for _ in 0..12 {
        shell.state.borrow_mut().advance_event_dot_animation();
        let rows = shell.state.borrow().rendered_transcript(80).clone();
        assert_eq!(&rows[..baseline.len() - 2], &baseline[..baseline.len() - 2]);
        assert_eq!(
            &shell.state.borrow().block_revisions[..revisions.len() - 1],
            &revisions[..revisions.len() - 1]
        );
        assert!(shell.state.borrow().transcript_cache.borrow().generation >= generation);
        assert!(frame_text(&shell, 80).contains("Subagents"));
    }
    let mut settled = child();
    settled.state = "completed".into();
    publish(&mut shell, &settled);
    shell.state.borrow_mut().advance_event_dot_animation();
    assert_eq!(shell.state.borrow().transcript.len(), revisions.len());
    assert!(transcript_text(&shell).contains("Subagents · 1 completed"));
}

#[test]
fn the_roster_aggregate_stays_running_while_any_child_is_active() {
    let running = named_worker("LIVE-WORKER", "running");
    let failed = named_worker("BROKEN-WORKER", "failed");
    let completed = named_worker("DONE-WORKER", "completed");
    let view = |children: Vec<octet_agent::DelegationTelemetryChild>| SubagentActivityView {
        telemetry: children,
        ..SubagentActivityView::default()
    };
    assert!(subagent_activity_is_active(&view(vec![running.clone()])));
    assert!(subagent_activity_is_active(&view(vec![
        running,
        failed.clone()
    ])));
    assert!(!subagent_activity_is_active(&view(vec![
        completed.clone(),
        failed
    ])));
    assert!(!subagent_activity_is_active(&view(vec![completed])));
    assert!(!subagent_activity_is_active(&view(vec![named_worker(
        "STOPPED", "stopped"
    )])));
    assert!(!subagent_activity_is_active(&view(Vec::new())));
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
    let rows = shell.state.borrow().rendered_transcript(80).clone();
    let revisions = shell.state.borrow().block_revisions.clone();
    let before = frame_text(&shell, 240);
    activity.metrics.as_mut().unwrap().tool_calls = 99;
    shell
        .state
        .borrow_mut()
        .set_subagent_activity(view(&activity));
    assert_eq!(shell.state.borrow().block_revisions, revisions);
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
    let calls_updated = frame_text(&shell, 240);
    assert_eq!(calls_updated, before, "usage changes stay in the inspector");
    assert_eq!(
        shell
            .state
            .borrow()
            .subagent_activity
            .as_ref()
            .unwrap()
            .activities[0]
            .metrics
            .unwrap()
            .tool_calls,
        99
    );
    activity.metrics.as_mut().unwrap().output_tokens += 100;
    shell
        .state
        .borrow_mut()
        .set_subagent_activity(view(&activity));
    assert_eq!(shell.state.borrow().block_revisions, revisions);
    assert_eq!(*shell.state.borrow().rendered_transcript(80), rows);
    let after = frame_text(&shell, 240);
    assert_eq!(
        before, after,
        "usage changes must not reflow the pinned preview"
    );
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
                    "tool_calls": child.tool_use_count,
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
fn mixed_session_rosters_stay_out_of_transcript_and_account_only_new_spend() {
    for native in [false, true] {
        let mut shell = InteractiveShell::test_shell();
        let run = shell.begin_run("fixture");
        shell.on_prompt_submitted("first prompt");
        publish_roster(&mut shell, native, &[named_worker("OLD-WORKER", "running")]);
        let old = named_worker("OLD-WORKER", "completed");
        publish_roster(&mut shell, native, &[old.clone()]);
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
        let unseen = named_worker("UNSEEN-OLD-WORKER", "completed");
        publish_roster(&mut shell, native, &[old.clone(), unseen.clone()]);
        assert_eq!(transcript_text(&shell).matches("Subagents").count(), 1);
        // The outcome's aggregate running hint may change, but no worker
        // roster becomes transcript material.
        let baseline = shell.state.borrow().transcript.len();
        let fresh = named_worker("FRESH-WORKER", "running");
        publish_roster(
            &mut shell,
            native,
            &[old.clone(), unseen.clone(), fresh.clone()],
        );
        assert_eq!(shell.state.borrow().transcript.len(), baseline + 1);
        assert_eq!(transcript_text(&shell).contains("FRESH-WORKER"), native);
        assert_eq!(transcript_text(&shell).matches("Subagents").count(), 2);
        assert_eq!(
            shell.state.borrow().displayed_session_cost_microdollars(),
            Some(107_200)
        );
        let settled = named_worker("FRESH-WORKER", "completed");
        for _ in 0..3 {
            publish_roster(
                &mut shell,
                native,
                &[old.clone(), unseen.clone(), settled.clone()],
            );
            assert_eq!(shell.state.borrow().transcript.len(), baseline + 1);
            assert!(!transcript_text(&shell).contains("WORKER"));
            assert_eq!(transcript_text(&shell).matches("Subagents").count(), 2);
        }
        let run = shell.current_run_id().unwrap();
        shell.on_run_event(
            run,
            &AgentEvent::RunFinished {
                head: octet_agent::EntryId("second-head".into()),
                reason: octet_agent::FinishReason::Completed,
            },
        );
        shell.begin_run("fixture");
        shell.on_prompt_submitted("third prompt");
        publish_roster(&mut shell, native, &[old, unseen, settled]);
        assert_eq!(transcript_text(&shell).matches("Subagents").count(), 2);
        publish_roster(&mut shell, native, &[fresh]);
        assert_eq!(transcript_text(&shell).matches("Subagents").count(), 3);
        assert_eq!(
            shell.state.borrow().displayed_session_cost_microdollars(),
            Some(100_000)
        );
        let mut continued = named_worker("FRESH-WORKER", "running");
        continued.cost_microdollars = Some(8_200);
        publish_roster(&mut shell, native, &[continued]);
        assert_eq!(
            shell.state.borrow().displayed_session_cost_microdollars(),
            Some(101_000)
        );
    }
}

/// The complete rendered frame for one shell, transcript and chrome together.
fn frame_text(shell: &InteractiveShell, width: u16) -> String {
    strip_terminal_sequences(&render_shell(&shell.state.borrow(), width).join("\n"))
}

/// The rows the reader is actually looking at: the same transcript cache,
/// chrome height, and resolver the renderer uses.
fn visible_rows(shell: &InteractiveShell, width: u16) -> Vec<String> {
    let state = shell.state.borrow();
    let chrome = shell_chrome(&state, width, Instant::now());
    let transcript = transcript_lines(&state, width);
    let scroll = resolved_scroll_from_bottom(&state, transcript.len(), chrome.transcript_rows);
    let capacity = transcript_viewport_capacity(chrome.transcript_rows, scroll > 0);
    let end = transcript.len().saturating_sub(scroll);
    let start = end.saturating_sub(capacity);
    transcript[start..end]
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect()
}

#[test]
fn active_roster_stays_one_transcript_row_independent_of_parent_run() {
    for native in [false, true] {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(120, 24);
        let run = shell.begin_run("fixture");
        shell.on_prompt_submitted("owning prompt");
        for n in 0..80 {
            shell.notice(format!("HISTORY-{n}"));
        }
        let mut workers = vec![
            named_worker("LIVE-WORKER", "running"),
            named_worker("OTHER-WORKER", "running"),
        ];
        publish_roster(&mut shell, native, &workers);
        for parent_settled in [false, true] {
            if parent_settled {
                shell.on_run_event(
                    run,
                    &AgentEvent::RunFinished {
                        head: octet_agent::EntryId("head".into()),
                        reason: octet_agent::FinishReason::Completed,
                    },
                );
            }
            for follow_tail in [true, false] {
                shell.state.borrow_mut().follow_tail = follow_tail;
                let frame = frame_text(&shell, 120);
                assert!(transcript_text(&shell).contains("Subagents · 2 running"));
                assert!(shell_chrome(&shell.state.borrow(), 120, Instant::now())
                    .subagents
                    .is_empty());
                assert_eq!(transcript_text(&shell).contains("LIVE-WORKER"), native);
                assert_eq!(transcript_text(&shell).contains("OTHER-WORKER"), native);
                if !follow_tail {
                    assert!(frame.contains("HISTORY-"));
                }
            }
        }
        workers[0].state = "failed".into();
        publish_roster(&mut shell, native, &workers);
        assert!(frame_text(&shell, 120).contains("Subagents"));
        workers[1].state = "completed".into();
        publish_roster(&mut shell, native, &workers);
        assert!(transcript_text(&shell).contains("Subagents · 1 completed · 1 failed"));
    }
}

#[test]
fn active_roster_does_not_hold_back_transcript_commit_boundaries() {
    for native in [false, true] {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(80, 24);
        let run = shell.begin_run("fixture");
        shell.on_prompt_submitted("owning prompt");
        publish_roster(
            &mut shell,
            native,
            &[named_worker("LIVE-WORKER", "running")],
        );
        for n in 0..40 {
            shell.notice(format!("filler-{n}"));
        }
        shell.on_run_event(
            run,
            &AgentEvent::RunFinished {
                head: octet_agent::EntryId("head".into()),
                reason: octet_agent::FinishReason::Completed,
            },
        );
        let mut frame = ShellFrameState::default();
        let _ = super::native_scrollback::render_shell_update_without_cursor(
            &shell.state.borrow(),
            80,
            Instant::now(),
            &mut frame,
        );
        let transcript_len = shell.state.borrow().rendered_transcript(80).len();
        let mut worker = named_worker("LIVE-WORKER", "running");
        worker.output_tokens += 100;
        publish_roster(&mut shell, native, &[worker]);
        let update = super::native_scrollback::render_shell_update_without_cursor(
            &shell.state.borrow(),
            80,
            Instant::now(),
            &mut frame,
        );
        assert_eq!(update.stable_prefix, transcript_len);
        assert_eq!(frame.pending_tool_start, None);
        let pinned = render_shell_update(
            &shell.state.borrow(),
            80,
            Instant::now(),
            &mut ShellFrameState::default(),
        );
        let committed = pinned.pinned.expect("pinned commit metadata");
        assert!(committed.stable_rows > transcript_len / 2,
            "a roster near the start must not prevent later history committing: {} of {transcript_len}", committed.stable_rows);
        assert!(committed.target.is_some_and(|position| position.row > 0));
    }
}

#[test]
fn live_roster_updates_leave_the_application_history_viewport_anchored() {
    for native in [false, true] {
        let mut shell = InteractiveShell::test_shell();
        shell.capture_mouse = true;
        shell.set_size(120, 24);
        for n in 0..80 {
            shell.notice(format!("HISTORY-{n}"));
        }
        let mut live = named_worker("LIVE-WORKER", "running");
        publish_roster(&mut shell, native, &[live.clone()]);
        shell.scroll_lines(-12);
        assert!(!shell.state.borrow().follow_tail);
        let before = visible_rows(&shell, 120);
        assert!(
            before.iter().any(|row| row.contains("HISTORY-")),
            "{before:?}"
        );
        let scroll_before = shell.state.borrow().scroll_from_bottom.get();
        live.output_tokens += 100;
        live.tool_use_count += 1;
        publish_roster(&mut shell, native, &[live.clone()]);
        assert_eq!(visible_rows(&shell, 120), before);
        assert_eq!(shell.state.borrow().scroll_from_bottom.get(), scroll_before);
        assert!(transcript_text(&shell).contains("Subagents · 1 running"));
        publish_roster(
            &mut shell,
            native,
            &[live, named_worker("NEW-WORKER", "running")],
        );
        assert!(!shell.state.borrow().follow_tail);
        let grown = visible_rows(&shell, 120);
        // Changing chrome height may expose fewer history rows, but preserves
        // the same anchored first row instead of jumping to live output.
        assert_eq!(grown.first(), before.first(), "{before:?}\n{grown:?}");
        assert!(transcript_text(&shell).contains("Subagents · 2 running"));
    }
}

#[test]
fn transcript_roster_is_bounded_and_preserves_the_composer() {
    for width in [24, 80, 120] {
        for height in [8, 12, 24] {
            let mut shell = InteractiveShell::test_shell();
            shell.set_size(width, height);
            shell.state.borrow_mut().editor.set_text("DRAFT-MARKER");
            let workers: Vec<_> = (0..32)
                .map(|n| named_worker(&format!("WORKER-{n:02}"), "running"))
                .collect();
            publish_roster(&mut shell, true, &workers);
            let frame = frame_text(&shell, width);
            assert!(frame.contains("DRAFT-MARKER"), "{width}x{height}: {frame}");
            assert!(
                frame.contains("/subagents"),
                "omitted rows need an inspector hint: {frame}"
            );
            assert!(
                workers
                    .iter()
                    .filter(|worker| frame.contains(&worker.task_name))
                    .count()
                    < workers.len()
            );
            let chrome = shell_chrome(&shell.state.borrow(), width, Instant::now());
            assert!(chrome.subagents.len() <= usize::from(height) / 3);
            let draft_row = frame
                .lines()
                .position(|row| row.contains("DRAFT-MARKER"))
                .unwrap();
            assert!(draft_row > 0);
            assert!(transcript_text(&shell).contains("/subagents"));
            assert!(chrome.subagents.is_empty());
            assert!(
                chrome.transcript_rows > 0,
                "the roster must leave history space"
            );
            assert!(
                frame
                    .lines()
                    .all(|line| visible_width(line) <= usize::from(width)),
                "{frame}"
            );
            assert_eq!(transcript_text(&shell).matches("WORKER-").count(), 4);
        }
    }
}

#[test]
fn all_first_party_subagent_tools_hide_live_cards_and_errors_without_losing_accounting() {
    for name in crate::presentation::tool_display::SUBAGENT_TOOL_NAMES {
        let mut shell = InteractiveShell::test_shell();
        shell.begin_run("fixture");
        shell.notice("UNRELATED-NOTICE");
        let id = ToolCallId(name.into());
        assert_eq!(
            summarize_tool_with_workspace(name, &serde_json::json!({}), None).plain_tag,
            "delegation"
        );
        shell.on_agent_event(&AgentEvent::ToolStarted {
            id: id.clone(),
            name: name.into(),
            args: serde_json::json!({"prompt": "SECRET-PROMPT"}),
        });
        shell.on_agent_event(&AgentEvent::ToolProgress {
            id: id.clone(),
            progress: ToolProgress::Status("SECRET-PROGRESS".into()),
        });
        shell.state.borrow_mut().run_context_estimate = Some((0, 100_000));
        for result in [
            Ok(octet_agent::ToolOutput::new("SECRET-RESULT")),
            Err(octet_agent::ToolError::new("SECRET worker quota reached")),
            Ok(octet_agent::ToolOutput::new("SECRET semantic error").with_is_error(true)),
        ] {
            let before = shell.state.borrow().run_context_estimate.unwrap().0;
            shell.on_agent_event(&AgentEvent::ToolFinished {
                id: id.clone(),
                result,
                duration: Duration::ZERO,
            });
            let state = shell.state.borrow();
            assert!(state.tool_panels.is_empty());
            assert!(state.run_context_estimate.unwrap().0 > before);
            assert!(!state
                .transcript
                .iter()
                .any(|block| matches!(block, TranscriptBlock::Tool(_))));
            let copied = state
                .transcript
                .iter()
                .map(block_copy_text)
                .collect::<String>();
            assert!(copied.contains("UNRELATED-NOTICE"));
            assert!(!copied.contains("SECRET") && !copied.contains("Delegation failed"));
            drop(state);
            assert!(!transcript_text(&shell).contains("SECRET"));
        }
        // A reused ID must not hide an ordinary failed tool.
        shell.on_agent_event(&AgentEvent::ToolStarted {
            id: id.clone(),
            name: "read".into(),
            args: serde_json::json!({"path": "file"}),
        });
        shell.on_agent_event(&AgentEvent::ToolFinished {
            id,
            result: Err(octet_agent::ToolError::new("ordinary failure")),
            duration: Duration::ZERO,
        });
        assert!(transcript_text(&shell).contains("ordinary failure"));
    }
}

#[test]
fn subagent_hydration_hides_calls_and_results_across_batches_and_id_reuse() {
    use crate::hydrate::TranscriptItem;
    let mut state = ShellState::default();
    for name in crate::presentation::tool_display::SUBAGENT_TOOL_NAMES {
        let id = ToolCallId(name.into());
        append_hydrated_items(
            &mut state,
            [TranscriptItem::ToolCall {
                id: id.clone(),
                name: name.into(),
                args: serde_json::json!({}),
            }],
        );
        for is_error in [false, true, true] {
            append_hydrated_items(
                &mut state,
                [TranscriptItem::ToolResult {
                    id: id.clone(),
                    text: "SECRET-RESULT".into(),
                    is_error,
                    duration_ms: None,
                    images: Vec::new(),
                }],
            );
        }
    }
    assert_eq!(
        state
            .transcript
            .iter()
            .filter(|block| matches!(block, TranscriptBlock::Subagents(_)))
            .count(),
        4
    );
    assert!(!state
        .rendered_transcript(120)
        .join("\n")
        .contains("SECRET-RESULT"));
    let id = ToolCallId("subagent_models".into());
    append_hydrated_items(
        &mut state,
        [TranscriptItem::ToolResult {
            id: id.clone(),
            text: "catalog unavailable".into(),
            is_error: true,
            duration_ms: None,
            images: Vec::new(),
        }],
    );
    assert_eq!(
        state
            .transcript
            .iter()
            .filter(|block| matches!(block, TranscriptBlock::Subagents(_)))
            .count(),
        4
    );
    assert!(!state
        .rendered_transcript(120)
        .join("\n")
        .contains("SECRET-RESULT"));
    append_hydrated_items(
        &mut state,
        [
            TranscriptItem::ToolCall {
                id: id.clone(),
                name: "read".into(),
                args: serde_json::json!({"path": "file"}),
            },
            TranscriptItem::ToolResult {
                id,
                text: "ordinary failure".into(),
                is_error: true,
                duration_ms: None,
                images: Vec::new(),
            },
        ],
    );
    assert!(
        matches!(&state.transcript[4], TranscriptBlock::Tool(panel) if panel.finished && panel.is_error && panel.output == "ordinary failure")
    );
}

#[test]
fn subagent_tail_hydration_and_deferred_prepend_hide_boundary_results() {
    use octet_ai::{
        AssistantMessage, AssistantPart, ModelId, Protocol, ToolCall, ToolResult, ToolResultPart,
        UserMessage, UserPart,
    };
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    for i in 0..70 {
        session
            .append(EntryValue::Message(octet_ai::Message::User(UserMessage {
                content: vec![UserPart::Text(format!("HISTORY-{i}"))],
            })))
            .unwrap();
    }
    for name in crate::presentation::tool_display::SUBAGENT_TOOL_NAMES {
        session
            .append(EntryValue::Message(octet_ai::Message::Assistant(
                AssistantMessage {
                    content: vec![AssistantPart::ToolCall(ToolCall {
                        async_execution: false,
                        id: ToolCallId(name.into()),
                        name: name.into(),
                        arguments_json: "{}".into(),
                        argument_error: None,
                    })],
                    model: ModelId("fixture".into()),
                    protocol: Protocol::OpenAiResponses,
                },
            )))
            .unwrap();
        session
            .append(EntryValue::Message(octet_ai::Message::User(UserMessage {
                content: vec![UserPart::ToolResult(ToolResult {
                    tool_call_id: ToolCallId(name.into()),
                    content: vec![ToolResultPart::Text("SECRET-RESULT".into())],
                    is_error: true,
                    added_tool_names: None,
                })],
            })))
            .unwrap();
    }
    // A cut directly at the last result still brings its matching call with it.
    let (items, truncated) = crate::hydrate::hydrate_transcript_tail(&session, 1).unwrap();
    assert!(truncated);
    let mut state = ShellState::default();
    append_hydrated_items(&mut state, items);
    // The last boundary belongs to a discovery call, not an orchestration wave.
    assert!(state.transcript.is_empty());

    let mut shell = InteractiveShell::test_shell();
    shell.capture_mouse = true;
    shell.set_size(80, 8);
    shell.hydrate(&session).unwrap();
    assert!(shell.state.borrow().deferred_session_history.is_some());
    let run = shell.begin_run("fixture");
    publish(&mut shell, &child());
    let retained = shell
        .state
        .borrow()
        .subagent_activity
        .as_ref()
        .unwrap()
        .telemetry
        .clone();
    // Replay must not replace a live classification when providers reuse IDs.
    shell.on_agent_event(&AgentEvent::ToolStarted {
        id: ToolCallId("subagent_models".into()),
        name: "subagent_continue".into(),
        args: serde_json::json!({}),
    });
    assert!(shell.materialize_deferred_history().unwrap());
    assert_eq!(
        shell.state.borrow().hidden_subagent_calls[&ToolCallId("subagent_models".into())],
        "subagent_continue"
    );
    let state = shell.state.borrow();
    assert_eq!(
        state.subagent_activity.as_ref().unwrap().telemetry,
        retained
    );
    assert!(state.tool_panels.is_empty());
    assert!(!state
        .rendered_transcript(120)
        .join("\n")
        .contains("SECRET-RESULT"));
    drop(state);
    shell.on_run_event(
        run,
        &AgentEvent::RunFinished {
            head: octet_agent::EntryId("head".into()),
            reason: octet_agent::FinishReason::Completed,
        },
    );
    shell.hydrate(&session).unwrap();
    assert!(shell.state.borrow().subagent_activity.is_none());
    publish(&mut shell, &child());
    assert!(transcript_text(&shell).contains("Subagents"));
    assert!(!transcript_text(&shell).contains("SECRET-RESULT"));
}
#[test]
fn subagent_restart_and_durable_refresh_subtract_committed_worker_spend() {
    use octet_agent::{SessionRecord, UsageRecord, UsageRecordKind};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    drop(Session::create(&path).unwrap());
    let record = SessionRecord::Usage {
        record: UsageRecord {
            kind: UsageRecordKind::DelegatedAgent {
                agent_id: "worker-1".into(),
                turn_count: 1,
                tool_call_count: 0,
            },
            usage: Usage::default(),
            stop_reason: None,
            endpoint: None,
            model: None,
            completed_at_unix_ms: None,
            cost: None,
            cost_microdollars: Some(7_200),
            session_cost_microdollars: Some(7_200),
            session_cost_picodollars_remainder: None,
        },
    };
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(file, "{}", serde_json::to_string(&record).unwrap()).unwrap();
    drop(file);
    let session = Session::open_read_only(&path).unwrap();
    let mut shell = InteractiveShell::test_shell();
    shell.hydrate(&session).unwrap();
    let mut worker = child();
    publish(&mut shell, &worker);
    assert_eq!(
        shell.state.borrow().displayed_session_cost_microdollars(),
        Some(7_200)
    );
    worker.cost_microdollars = Some(8_200);
    publish(&mut shell, &worker);
    assert_eq!(
        shell.state.borrow().displayed_session_cost_microdollars(),
        Some(8_200)
    );
    shell.state.borrow_mut().subagent_committed_costs.clear();
    shell.set_session_telemetry(&session, None);
    assert_eq!(
        shell.state.borrow().displayed_session_cost_microdollars(),
        Some(8_200)
    );
}

#[test]
fn every_host_worker_state_has_consistent_group_filter_and_chrome_visibility() {
    use octet_agent::DelegatedAgentStatus as Status;
    use SubagentStateGroup::{Completed, Failed, Running, Stopped};
    let states = [
        (Status::Pending, Running),
        (Status::Running, Running),
        (
            Status::Completed {
                output: String::new(),
            },
            Completed,
        ),
        (
            Status::LimitReached {
                output: String::new(),
                turn_count: 2,
                turn_limit: 2,
            },
            Failed,
        ),
        (Status::Interrupted, Stopped),
        (
            Status::Failed {
                error: String::new(),
            },
            Failed,
        ),
        (Status::TimedOut, Failed),
        (Status::Detached, Stopped),
        (
            Status::AwaitingApproval {
                reason: String::new(),
            },
            Stopped,
        ),
        (Status::Shutdown, Stopped),
    ];
    for (status, group) in states {
        let wire = serde_json::to_value(status).unwrap();
        let label = wire["state"].as_str().unwrap();
        let worker = named_worker("STATE-WORKER", label);
        let view = SubagentActivityView {
            telemetry: vec![worker.clone()],
            ..Default::default()
        };
        assert_eq!(
            SubagentStateGroup::of_declared_state(label),
            group,
            "{label}"
        );
        assert_eq!(
            subagent_activity_is_active(&view),
            group == Running,
            "{label}"
        );
        let mut shell = InteractiveShell::test_shell();
        publish(&mut shell, &named_worker("STATE-WORKER", "running"));
        publish(&mut shell, &worker);
        assert!(transcript_text(&shell).contains("Subagents"), "{label}");
        assert_eq!(
            transcript_text(&shell).contains("STATE-WORKER"),
            matches!(label, "pending" | "running"),
            "{label}"
        );
        assert!(shell
            .state
            .borrow()
            .transcript
            .iter()
            .all(|block| !block_copy_text(block).contains("STATE-WORKER")));
        assert!(transcript_text(&shell).contains("Subagents"));
        assert!(transcript_text(&shell).contains(match group {
            Running => "running",
            Completed => "completed",
            Failed => "failed",
            Stopped => "stopped",
        }));
        assert_eq!(
            shell
                .state
                .borrow()
                .subagent_activity
                .as_ref()
                .unwrap()
                .telemetry[0],
            worker
        );
    }
}

#[test]
fn orchestration_is_one_blinking_tail_row_then_settles_in_place() {
    let mut shell = InteractiveShell::test_shell();
    shell.notice("parent before");
    let first = named_worker("worker-a", "running");
    publish_roster(&mut shell, true, &[first.clone()]);
    {
        let state = shell.state.borrow();
        assert!(
            matches!(state.transcript.last(), Some(TranscriptBlock::Subagents(summary)) if summary.running == 1)
        );
        assert_eq!(
            state
                .transcript
                .iter()
                .filter(|block| matches!(block, TranscriptBlock::Subagents(_)))
                .count(),
            1
        );
        assert!(state.has_active_event_dot());
        assert!(shell_chrome(&state, 120, Instant::now())
            .subagents
            .is_empty());
        assert!(block_copy_text(state.transcript.last().unwrap()).contains("1 running"));
        assert!(!block_copy_text(state.transcript.last().unwrap()).contains("worker-a"));
    }
    let before = shell
        .state
        .borrow()
        .rendered_transcript(120)
        .iter()
        .find(|row| row.contains("Subagents"))
        .cloned()
        .unwrap();
    shell.state.borrow_mut().advance_event_dot_animation();
    let after = shell
        .state
        .borrow()
        .rendered_transcript(120)
        .iter()
        .find(|row| row.contains("Subagents"))
        .cloned()
        .unwrap();
    assert_ne!(before, after, "active marker must blink");
    shell.notice("parent after");
    {
        let state = shell.state.borrow();
        assert!(matches!(
            state.transcript[state.transcript.len() - 2],
            TranscriptBlock::Notice(_)
        ));
        assert!(matches!(
            state.transcript.last(),
            Some(TranscriptBlock::Subagents(_))
        ));
        assert!(state
            .transcript_commit_ids
            .windows(2)
            .all(|pair| pair[0] < pair[1]));
        let frame = strip_terminal_sequences(&state.rendered_transcript(120).join("\n"));
        assert!(frame.find("parent after").unwrap() < frame.find("Subagents").unwrap());
    }
    let settled = named_worker("worker-a", "completed");
    publish_roster(&mut shell, true, &[settled]);
    let index = shell.state.borrow().transcript.len() - 1;
    assert!(
        matches!(&shell.state.borrow().transcript[index], TranscriptBlock::Subagents(summary) if summary.settled_role() == "success")
    );
    assert!(!shell.state.borrow().has_active_event_dot());
    shell.notice("later prompt");
    let state = shell.state.borrow();
    assert!(matches!(
        state.transcript[index],
        TranscriptBlock::Subagents(_)
    ));
    assert!(matches!(
        state.transcript.last(),
        Some(TranscriptBlock::Notice(_))
    ));
    assert_eq!(
        state
            .transcript
            .iter()
            .filter(|block| matches!(block, TranscriptBlock::Subagents(_)))
            .count(),
        1
    );
}

#[test]
fn orchestration_settlement_marker_classifies_failed_and_mixed_rosters() {
    for (statuses, expected) in [
        (["failed", "failed"], "error"),
        (["completed", "failed"], "warning"),
    ] {
        let mut shell = InteractiveShell::test_shell();
        let a = named_worker("a", "running");
        let b = named_worker("b", "running");
        publish_roster(&mut shell, true, &[a, b]);
        publish_roster(
            &mut shell,
            true,
            &[
                named_worker("a", statuses[0]),
                named_worker("b", statuses[1]),
            ],
        );
        let state = shell.state.borrow();
        let row = state.transcript.last().unwrap();
        assert!(
            matches!(row, TranscriptBlock::Subagents(summary) if summary.settled_role() == expected)
        );
        assert!(!block_copy_text(row).contains("worker"));
        assert!(shell_chrome(&state, 120, Instant::now())
            .subagents
            .is_empty());
    }
}
