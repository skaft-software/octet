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
fn subagent_metrics_update_chrome_without_invalidating_transcript() {
    for width in [20, 46, 80, 120, 240] {
        for verbose in [false, true] {
            let mut shell = InteractiveShell::test_shell();
            shell.set_size(width, 24);
            shell.state.borrow_mut().verbose_tools = verbose;
            shell.notice("HISTORY");
            let mut child = child();
            publish(&mut shell, &child);
            let rows = shell.state.borrow().rendered_transcript(width).clone();
            let revisions = shell.state.borrow().block_revisions.clone();
            let generation = shell.state.borrow().transcript_cache.borrow().generation;
            for tool in [Some("read"), None, Some("bash"), None] {
                child.current_tool = tool.map(str::to_owned);
                child.phase = tool.unwrap_or("thinking").into();
                child.elapsed_ms += 250;
                child.tool_use_count += 1;
                publish(&mut shell, &child);
                assert_eq!(*shell.state.borrow().rendered_transcript(width), rows);
                let state = shell.state.borrow();
                assert_eq!(state.block_revisions, revisions);
                assert_eq!(state.transcript_cache.borrow().generation, generation);
                assert_eq!(
                    state.subagent_activity.as_ref().unwrap().telemetry[0],
                    child
                );
                drop(state);
                let frame = frame_text(&shell, width);
                assert!(frame.contains("Subagents"), "{width}: {frame}");
                if width == 240 {
                    let worker_row = frame
                        .lines()
                        .find(|row| row.contains("inspect-markdown"))
                        .unwrap();
                    assert!(
                        worker_row
                            .split_whitespace()
                            .any(|cell| cell == child.tool_use_count.to_string()),
                        "{worker_row}"
                    );
                }
            }
            for status in ["completed", "running", "completed"] {
                child.state = status.into();
                publish(&mut shell, &child);
                assert_eq!(*shell.state.borrow().rendered_transcript(width), rows);
                assert_eq!(
                    frame_text(&shell, width).contains("Subagents"),
                    status == "running"
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
fn late_failed_or_parked_workers_leave_one_bounded_safe_notice_per_transition() {
    for status in ["failed", "timed_out", "awaiting_approval"] {
        let mut shell = InteractiveShell::test_shell();
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
        let reason = format!(
            "REASON-MARKER \x1b[3J\x1b]52;c;SECRET\x07\x00\x07 {}",
            "é".repeat(5000)
        );
        worker.state = status.into();
        worker.failure_reason = Some(reason.clone());
        publish(&mut shell, &worker);
        let notice = {
            let state = shell.state.borrow();
            assert_eq!(state.transcript.len(), block_count + 1);
            let TranscriptBlock::Notice(notice) = state.transcript.last().unwrap() else {
                panic!("the failure must append a notice, not a roster tool card");
            };
            assert!(
                notice.contains("LATE-WORKER") && notice.contains(status),
                "{notice}"
            );
            assert!(notice.contains("REASON-MARKER"), "{notice}");
            assert!(notice.ends_with("inspect with /subagents"), "{notice}");
            assert!(notice.len() <= 4096 + " · inspect with /subagents".len());
            assert!(
                notice.contains('…'),
                "oversized reasons must be visibly bounded"
            );
            assert!(!notice.chars().any(char::is_control), "{notice:?}");
            assert!(
                !notice.contains("SECRET"),
                "OSC clipboard payload must not survive: {notice:?}"
            );
            assert_eq!(
                state.subagent_activity.as_ref().unwrap().telemetry[0]
                    .failure_reason
                    .as_deref(),
                Some(reason.as_str())
            );
            notice.clone()
        };
        assert!(shell_chrome(&shell.state.borrow(), 120, Instant::now())
            .subagents
            .is_empty());
        let _ = shell.state.borrow().rendered_transcript(120);
        let revisions = shell.state.borrow().block_revisions.clone();
        let generation = shell.state.borrow().transcript_cache.borrow().generation;
        for update_metrics in [false, true, false] {
            if update_metrics {
                worker.tool_use_count += 1;
                worker.output_tokens += 100;
            }
            publish(&mut shell, &worker);
            let state = shell.state.borrow();
            assert_eq!(state.transcript.len(), block_count + 1);
            assert!(
                matches!(state.transcript.last(), Some(TranscriptBlock::Notice(text)) if text == &notice)
            );
            let _ = state.rendered_transcript(120);
            assert_eq!(state.block_revisions, revisions);
            assert_eq!(state.transcript_cache.borrow().generation, generation);
            assert_eq!(
                state.subagent_activity.as_ref().unwrap().telemetry[0],
                worker
            );
        }
        // A genuinely changed reason is new evidence, unlike repeated snapshots.
        worker.failure_reason = Some("UPDATED-REASON".into());
        publish(&mut shell, &worker);
        assert_eq!(shell.state.borrow().transcript.len(), block_count + 2);
        assert!(
            matches!(shell.state.borrow().transcript.last(), Some(TranscriptBlock::Notice(text)) if text.contains("UPDATED-REASON"))
        );
        publish(&mut shell, &worker);
        assert_eq!(shell.state.borrow().transcript.len(), block_count + 2);
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
        assert_eq!(*shell.state.borrow().rendered_transcript(80), baseline);
        assert_eq!(shell.state.borrow().block_revisions, revisions);
        assert_eq!(
            shell.state.borrow().transcript_cache.borrow().generation,
            generation
        );
        assert!(frame_text(&shell, 80).contains("Subagents"));
    }
    let mut settled = child();
    settled.state = "completed".into();
    publish(&mut shell, &settled);
    shell.state.borrow_mut().advance_event_dot_animation();
    assert_eq!(*shell.state.borrow().rendered_transcript(80), baseline);
    assert!(!frame_text(&shell, 80).contains("Subagents"));
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
    assert!(
        calls_updated
            .lines()
            .any(|row| row.contains("inspect-markdown")
                && row.split_whitespace().any(|cell| cell == "99")),
        "{calls_updated}"
    );
    activity.metrics.as_mut().unwrap().output_tokens += 100;
    shell
        .state
        .borrow_mut()
        .set_subagent_activity(view(&activity));
    assert_eq!(shell.state.borrow().block_revisions, revisions);
    assert_eq!(*shell.state.borrow().rendered_transcript(80), rows);
    let after = frame_text(&shell, 240);
    assert_ne!(before, after);
    assert!(
        after.lines().any(|row| row.contains("inspect-markdown")
            && row.split_whitespace().any(|cell| cell == "99")),
        "{after}"
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

/// Render the full semantic roster used by the explicit disclosure surface.
fn roster_rows(
    view: &SubagentActivityView,
    theme: &crate::tui::theme::OctetTheme,
    width: u16,
    verbose: bool,
) -> Vec<String> {
    subagent_activity_render_rows(view, theme, width, verbose)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect()
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
        assert!(!frame_text(&shell, 120).contains("Subagents"));
        // The outcome's aggregate running hint may change, but no worker
        // roster becomes transcript material.
        let baseline = shell.state.borrow().transcript.len();
        let fresh = named_worker("FRESH-WORKER", "running");
        publish_roster(
            &mut shell,
            native,
            &[old.clone(), unseen.clone(), fresh.clone()],
        );
        assert_eq!(shell.state.borrow().transcript.len(), baseline);
        assert!(!transcript_text(&shell).contains("WORKER"));
        assert!(frame_text(&shell, 120).contains("FRESH-WORKER"));
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
            assert_eq!(shell.state.borrow().transcript.len(), baseline);
            assert!(!transcript_text(&shell).contains("WORKER"));
            assert!(!frame_text(&shell, 120).contains("Subagents"));
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
        assert!(!frame_text(&shell, 120).contains("Subagents"));
        publish_roster(&mut shell, native, &[fresh]);
        assert!(frame_text(&shell, 120).contains("FRESH-WORKER"));
        assert!(!transcript_text(&shell).contains("Subagents"));
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
fn active_roster_is_pinned_once_independent_of_parent_run_and_follow_tail() {
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
                for label in ["Subagents", "LIVE-WORKER", "OTHER-WORKER"] {
                    assert_eq!(frame.matches(label).count(), 1, "{frame}");
                    assert!(!transcript_text(&shell).contains(label));
                }
            }
        }
        workers[0].state = "failed".into();
        publish_roster(&mut shell, native, &workers);
        assert!(frame_text(&shell, 120).contains("Subagents"));
        workers[1].state = "completed".into();
        publish_roster(&mut shell, native, &workers);
        assert!(!frame_text(&shell, 120).contains("Subagents"));
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
        assert!(frame_text(&shell, 120).contains("LIVE-WORKER"));
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
        assert!(frame_text(&shell, 120).contains("NEW-WORKER"));
    }
}

#[test]
fn pinned_roster_is_bounded_and_preserves_the_composer() {
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
            let heading_row = frame
                .lines()
                .position(|row| row.contains("Subagents"))
                .unwrap();
            assert!(
                heading_row < draft_row,
                "the roster belongs above the composer: {frame}"
            );
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
            assert!(!transcript_text(&shell).contains("WORKER-"));
        }
    }
}

/// A roster made of one state still prints the state column on every row, and
/// its heading stays a plain tool-call name: the state lives on the rows and
/// the margin dot, and the count lives on nothing at all.
#[test]
fn a_uniform_roster_keeps_the_state_column_and_a_plain_heading() {
    let theme = crate::tui::theme::test_theme();
    let uniform = SubagentActivityView {
        telemetry: vec![
            named_worker("worker-a", "running"),
            named_worker("worker-b", "running"),
        ],
        ..SubagentActivityView::default()
    };
    let rows = roster_rows(&uniform, &theme, 120, false);
    let plain = rows.join("\n");
    assert!(
        rows[0].trim_end().ends_with("Subagents"),
        "the heading is a plain tool-call name: {plain}"
    );
    assert!(
        plain.contains("state"),
        "the state column survives a uniform roster: {plain}"
    );
    assert!(
        plain.contains("running"),
        "every row names its own state: {plain}"
    );
    assert!(
        rows.iter().all(|row| !row.contains("running · ")),
        "no per-group sub-heading is printed: {plain}"
    );

    let mixed = SubagentActivityView {
        telemetry: vec![
            named_worker("worker-a", "running"),
            named_worker("worker-b", "completed"),
        ],
        ..SubagentActivityView::default()
    };
    let rows = roster_rows(&mixed, &theme, 120, false);
    let plain = rows.join("\n");
    assert!(
        plain.contains("running"),
        "mixed rosters keep every state word: {plain}"
    );
    assert!(plain.contains("completed"), "{plain}");
    assert!(plain.contains("state"), "{plain}");
    assert!(
        rows.iter().all(|row| !row.contains("running · ")),
        "mixed rosters print no per-group sub-heading either: {plain}"
    );
}

#[test]
fn the_model_identifier_is_printed_in_full_and_never_ellipsized() {
    // Native telemetry is the source that carries a model identifier; the
    // extension presentation has no model field to print.
    let mut worker = named_worker("deepseek-worker", "running");
    worker.model = "deepseek/deepseek-flash".into();
    let view = SubagentActivityView {
        telemetry: vec![worker],
        ..SubagentActivityView::default()
    };
    let theme = crate::tui::theme::test_theme();
    for width in [120u16, 80, 60] {
        let rows = roster_rows(&view, &theme, width, false);
        let plain = rows.join("\n");
        assert!(
            plain.contains("deepseek/deepseek-flash"),
            "the model id is never ellipsized at {width}: {plain}"
        );
        assert!(rows
            .iter()
            .all(|row| visible_width(row) <= usize::from(width)));
    }
    // The narrow fallback keeps the whole identifier readable: it wraps rather
    // than being replaced by an ellipsis.
    let rows = roster_rows(&view, &theme, 24, false);
    let compact = rows
        .join("\n")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        compact
            .split_whitespace()
            .collect::<String>()
            .contains("deepseek/deepseek-flash"),
        "{compact}"
    );
}

#[test]
fn grid_header_cells_align_with_the_worker_column_on_both_profiles() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    for unicode in [false, true] {
        let theme = crate::tui::theme::test_theme_with(TerminalCapabilities::test(
            true,
            unicode,
            ColorDepth::None,
        ));
        let view = SubagentActivityView {
            telemetry: vec![
                named_worker("審査-worker", "running"),
                named_worker("done-worker", "completed"),
            ],
            ..SubagentActivityView::default()
        };
        let rows = roster_rows(&view, &theme, 120, false);
        let header = rows
            .iter()
            .find(|row| row.contains("worker") && row.contains("state"))
            .unwrap_or_else(|| panic!("no grid header: {rows:?}"));
        let live = rows
            .iter()
            .find(|row| row.contains("審査-worker"))
            .unwrap_or_else(|| panic!("no live row: {rows:?}"));
        let header_column = visible_width(&header[..header.find("worker").unwrap()]);
        let live_column = visible_width(&live[..live.find("審査-worker").unwrap()]);
        assert_eq!(
            header_column, live_column,
            "header and worker rows pay the same prefix: {rows:?}"
        );
    }
}

#[test]
fn all_first_party_subagent_tools_hide_live_cards_and_keep_failures() {
    for name in crate::presentation::tool_display::SUBAGENT_TOOL_NAMES {
        let mut shell = InteractiveShell::test_shell();
        shell.begin_run("fixture");
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
        shell.on_agent_event(&AgentEvent::ToolFinished {
            id: id.clone(),
            result: Ok(octet_agent::ToolOutput::new("SECRET-RESULT")),
            duration: Duration::ZERO,
        });
        assert!(shell.state.borrow().tool_panels.is_empty());
        assert!(!shell
            .state
            .borrow()
            .transcript
            .iter()
            .any(|block| matches!(block, TranscriptBlock::Tool(_))));
        assert!(!transcript_text(&shell).contains("SECRET"));
        shell.on_agent_event(&AgentEvent::ToolFinished {
            id,
            result: Err(octet_agent::ToolError::new("worker quota reached")),
            duration: Duration::ZERO,
        });
        assert!(transcript_text(&shell).contains("Delegation failed: worker quota reached"));
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
        for _ in 0..2 {
            append_hydrated_items(
                &mut state,
                [TranscriptItem::ToolResult {
                    id: id.clone(),
                    text: "SECRET-RESULT".into(),
                    is_error: false,
                    duration_ms: None,
                    images: Vec::new(),
                }],
            );
        }
    }
    assert!(state.transcript.is_empty());
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
    assert!(
        matches!(&state.transcript[0], TranscriptBlock::Notice(text) if text.contains("catalog unavailable"))
    );
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
                text: "ordinary result".into(),
                is_error: false,
                duration_ms: None,
                images: Vec::new(),
            },
        ],
    );
    assert!(
        matches!(&state.transcript[1], TranscriptBlock::Tool(panel) if panel.finished && panel.output == "ordinary result")
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
                    is_error: false,
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
    assert!(!transcript_text(&shell).contains("Subagents"));
    assert!(frame_text(&shell, 120).contains("Subagents"));
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
        let mut view = SubagentActivityView {
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
        assert_eq!(subagent_rows(&view)[0].group, group, "{label}");
        let theme = crate::tui::theme::test_theme();
        for filter in SubagentStateGroup::ORDER {
            view.state_filter = Some(filter);
            let text = roster_rows(&view, &theme, 120, false).join("\n");
            assert_eq!(
                text.contains("STATE-WORKER"),
                filter == group,
                "{label}: {text}"
            );
            if filter == group {
                assert!(text.contains(label), "{label}: {text}");
            }
        }
        let mut shell = InteractiveShell::test_shell();
        publish(&mut shell, &named_worker("STATE-WORKER", "running"));
        publish(&mut shell, &worker);
        assert_eq!(
            frame_text(&shell, 120).contains("Subagents"),
            group == Running,
            "{label}"
        );
        assert_eq!(
            transcript_text(&shell).contains("STATE-WORKER"),
            matches!(group, Failed | Stopped),
            "{label}"
        );
        assert!(!transcript_text(&shell).contains("Subagents"));
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
fn expanded_full_retained_roster_exposes_all_workers_across_groups() {
    let theme = crate::tui::theme::test_theme();
    let mut view = SubagentActivityView {
        telemetry: (0..32)
            .map(|index| {
                let state = ["running", "completed", "failed", "shutdown"][index / 8];
                let mut worker = named_worker(&format!("W{index:02}"), state);
                worker.failure_reason = (state == "failed").then(|| "fixture failure".into());
                worker
            })
            .collect(),
        ..Default::default()
    };
    let collapsed = roster_rows(&view, &theme, 240, false).join("\n");
    for group in ["completed", "failed", "stopped"] {
        assert!(collapsed.contains(&format!("{group} · 8")), "{collapsed}");
    }
    assert_eq!(
        collapsed.matches("ctrl+o shows all").count(),
        3,
        "{collapsed}"
    );
    for width in [24, 80, 240] {
        let expanded = roster_rows(&view, &theme, width, true).join("\n");
        for worker in &view.telemetry {
            assert_eq!(
                expanded.matches(&worker.task_name).count(),
                1,
                "{width}: {expanded}"
            );
        }
        assert!(!expanded.contains("ctrl+o shows all"), "{expanded}");
        assert!(!expanded.contains(" more"), "{expanded}");
    }
    for group in SubagentStateGroup::ORDER {
        view.state_filter = Some(group);
        let filtered = roster_rows(&view, &theme, 120, false).join("\n");
        for worker in &view.telemetry {
            assert_eq!(
                filtered.contains(&worker.task_name),
                SubagentStateGroup::of_declared_state(&worker.state) == group,
                "{filtered}"
            );
        }
        assert!(!filtered.contains("ctrl+o shows all"), "{filtered}");
    }
}

#[test]
fn roster_tool_count_column_names_the_reported_metric() {
    let theme = crate::tui::theme::test_theme();
    let view = SubagentActivityView {
        telemetry: vec![child()],
        ..Default::default()
    };
    let text = roster_rows(&view, &theme, 240, true).join("\n");
    assert!(text.contains("tools"), "{text}");
    assert!(!text.contains("turns"), "{text}");
    let rows = subagent_rows(&view);
    assert_eq!(
        subagent_cell_text(&rows[0], SubagentColumn::Tools, true),
        "4"
    );
    let activity = serde_json::from_value(serde_json::json!({
        "id": "fallback", "kind": "subagent", "state": "running", "summary": "fallback",
        "metrics": {"tool_calls": 17}
    }))
    .unwrap();
    let fallback = SubagentActivityView {
        activities: vec![activity],
        ..Default::default()
    };
    let rows = subagent_rows(&fallback);
    assert_eq!(
        subagent_cell_text(&rows[0], SubagentColumn::Tools, true),
        "17"
    );
}

#[test]
fn generic_panel_filter_mirrors_worker_membership_not_lossy_state_labels() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut shell = InteractiveShell::test_shell();
    let mut workers = vec![
        named_worker("LIMIT", "running"),
        named_worker("DETACHED", "running"),
        named_worker("APPROVAL", "running"),
        named_worker("PENDING", "running"),
    ];
    publish_roster(&mut shell, true, &workers);
    for (worker, state) in
        workers
            .iter_mut()
            .zip(["limit_reached", "detached", "awaiting_approval", "pending"])
    {
        worker.state = state.into();
    }
    publish_roster(&mut shell, true, &workers);
    let items: Vec<_> = workers
        .iter()
        .map(|worker| worker.task_name.clone())
        .collect();
    let panel = SubagentPanel {
        node_ids: workers
            .iter()
            .map(|worker| format!("worker:{}", worker.child_id))
            .collect(),
        groups: vec![
            SubagentGroup {
                label: "Blocked".into(),
                indices: vec![0, 1],
                collapsible: false,
            },
            SubagentGroup {
                label: "Queued".into(),
                indices: vec![2, 3],
                collapsible: false,
            },
        ],
        collapsed: true,
        revealed_node: None,
        state_filter: None,
    };
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new("Subagents"),
        items: items.clone(),
        descriptions: vec![None; 4],
        selected: 0,
        filter: String::new(),
        action: PanelAction::SelectSubagent(panel.clone()),
    });
    let press = |shell: &mut InteractiveShell| {
        shell.panel_input(&crossterm::event::Event::Key(KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        )));
    };
    let assert_members = |shell: &InteractiveShell, expected: &[&str]| {
        let state = shell.state.borrow();
        let text = roster_rows(
            state.subagent_activity.as_ref().unwrap(),
            &state.theme,
            120,
            true,
        )
        .join("\n");
        for worker in &workers {
            assert_eq!(
                text.contains(&worker.task_name),
                expected.contains(&worker.task_name.as_str()),
                "{text}"
            );
        }
    };
    press(&mut shell);
    assert_members(&shell, &["LIMIT", "DETACHED"]);
    assert_eq!(
        shell
            .state
            .borrow()
            .subagent_activity
            .as_ref()
            .unwrap()
            .panel_filter
            .as_ref()
            .unwrap()
            .label,
        "Blocked"
    );
    // Republishing telemetry must retain the exact selected membership in
    // the retained view, even though Blocked spans Failed and Stopped groups.
    publish_roster(&mut shell, true, &workers);
    assert_members(&shell, &["LIMIT", "DETACHED"]);
    let state = shell.state.borrow();
    assert_eq!(
        state
            .subagent_activity
            .as_ref()
            .unwrap()
            .panel_filter
            .as_ref()
            .unwrap()
            .node_ids,
        vec!["worker:LIMIT", "worker:DETACHED"]
    );
    drop(state);
    press(&mut shell);
    assert_members(&shell, &["APPROVAL", "PENDING"]);
    assert_eq!(
        shell
            .state
            .borrow()
            .subagent_activity
            .as_ref()
            .unwrap()
            .panel_filter
            .as_ref()
            .unwrap()
            .label,
        "Queued"
    );
    // Membership is refreshed by stable ID when the selected group changes.
    let mut next = panel.clone();
    next.groups[0].indices = vec![0, 1, 3];
    next.groups[1].indices = vec![2];
    shell.refresh_subagent_panel("Subagents".into(), items.clone(), vec![None; 4], next);
    assert_members(&shell, &["APPROVAL"]);
    // Removing the selected group restores All rather than leaving stale IDs.
    let mut next = panel;
    next.groups.remove(1);
    next.groups[0].indices = vec![0, 1, 2, 3];
    shell.refresh_subagent_panel("Subagents".into(), items, vec![None; 4], next);
    assert_members(&shell, &["LIMIT", "DETACHED", "APPROVAL", "PENDING"]);

    // The fallback projection uses activity:{id}, while collection nodes use
    // worker:{id}; both refer to the same semantic worker.
    let activity = serde_json::from_value(serde_json::json!({
        "id": "activity:DETACHED", "kind": "subagent", "state": "degraded", "summary": "DETACHED"
    }))
    .unwrap();
    let fallback = SubagentActivityView {
        activities: vec![activity],
        panel_filter: Some(SubagentPanelFilter {
            label: "Blocked".into(),
            node_ids: vec!["worker:DETACHED".into()],
        }),
        ..Default::default()
    };
    let text = roster_rows(&fallback, &crate::tui::theme::test_theme(), 120, false).join("\n");
    assert!(text.contains("DETACHED"), "{text}");
}
