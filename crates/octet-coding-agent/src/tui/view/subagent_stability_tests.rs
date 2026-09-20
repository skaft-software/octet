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
fn live_subagent_marker_pulses_without_invalidating_historical_rows() {
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
    let pulse = if state.theme.unicode() { "•" } else { "*" };
    let lit = marker(block, &state.theme, 0, None, 0, false);
    let resting = marker(block, &state.theme, 1, None, 0, false);
    assert_eq!(lit, Some(state.theme.fg("foreground", pulse)));
    assert_eq!(
        resting,
        Some(state.theme.settled_event_dot("neutral", pulse))
    );
    assert_ne!(
        lit, resting,
        "a live roster pulses on the shared spinner clock"
    );
    drop(state);

    // A settled roster resolves from the declared child states: green when
    // every worker finished successfully, red when one failed, neutral when
    // work was stopped.
    let mut settled = child();
    settled.state = "completed".into();
    publish(&mut shell, &settled);
    let state = shell.state.borrow();
    let block = &state.transcript[index];
    assert_eq!(
        marker(block, &state.theme, 0, None, 0, false),
        Some(state.theme.settled_event_dot("success", pulse))
    );
    drop(state);

    let mut stopped = settled.clone();
    stopped.state = "stopped".into();
    publish(&mut shell, &stopped);
    let state = shell.state.borrow();
    let block = &state.transcript[index];
    assert_eq!(
        marker(block, &state.theme, 0, None, 0, false),
        Some(state.theme.settled_event_dot("neutral", pulse))
    );
    drop(state);

    let mut failed = settled.clone();
    failed.state = "failed".into();
    failed.failure_reason = Some("provider unavailable".into());
    publish(&mut shell, &failed);
    let state = shell.state.borrow();
    let block = &state.transcript[index];
    assert_eq!(
        marker(block, &state.theme, 0, None, 0, false),
        Some(state.theme.settled_event_dot("error", pulse))
    );
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
    assert_eq!(
        subagent_activity_aggregate(&view(vec![running.clone()])),
        Some(SubagentStateGroup::Running)
    );
    assert_eq!(
        subagent_activity_aggregate(&view(vec![running.clone(), failed.clone()])),
        Some(SubagentStateGroup::Running),
        "a live child keeps the roster in its running shape"
    );
    assert_eq!(
        subagent_activity_aggregate(&view(vec![completed.clone(), failed.clone()])),
        Some(SubagentStateGroup::Failed)
    );
    assert_eq!(
        subagent_activity_aggregate(&view(vec![completed.clone()])),
        Some(SubagentStateGroup::Completed)
    );
    assert_eq!(
        subagent_activity_aggregate(&view(vec![named_worker("STOPPED", "stopped")])),
        Some(SubagentStateGroup::Stopped)
    );
    assert_eq!(subagent_activity_aggregate(&view(Vec::new())), None);
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

/// Render one roster exactly as the transcript block does.
fn roster_rows(
    view: &SubagentActivityView,
    theme: &crate::tui::theme::OctetTheme,
    width: u16,
    verbose: bool,
) -> Vec<String> {
    render_block(
        None,
        &TranscriptBlock::Tool(Box::new(ToolPanel::subagent_activity(view))),
        theme,
        &theme.rich_renderer(),
        &theme.reasoning_renderer(),
        width,
        verbose,
    )
    .iter()
    .map(|line| strip_terminal_sequences(line))
    .collect()
}

#[test]
fn mixed_session_rosters_do_not_replay_known_or_unseen_terminal_workers() {
    for native in [false, true] {
        let mut shell = InteractiveShell::test_shell();
        let run = shell.begin_run("fixture");
        shell.on_prompt_submitted("first prompt");
        publish_roster(&mut shell, native, &[named_worker("OLD-WORKER", "running")]);
        publish_roster(
            &mut shell,
            native,
            &[named_worker("OLD-WORKER", "completed")],
        );
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
            &[
                old.clone(),
                named_worker("FINISHED-BEFORE-ATTACH", "completed"),
            ],
        );
        assert!(shell.state.borrow().subagent_activity_block.is_none());

        let fresh = named_worker("FRESH-WORKER", "running");
        publish_roster(
            &mut shell,
            native,
            &[old.clone(), unseen.clone(), fresh.clone()],
        );
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
        publish_roster(
            &mut shell,
            native,
            &[old.clone(), unseen.clone(), settled.clone()],
        );
        let settled_text = transcript_text(&shell);
        for _ in 0..3 {
            publish_roster(
                &mut shell,
                native,
                &[old.clone(), unseen.clone(), settled.clone()],
            );
            assert_eq!(shell.state.borrow().subagent_activity_block, Some(index));
            assert_eq!(transcript_text(&shell), settled_text);
            assert!(
                shell_chrome(&shell.state.borrow(), 120, Instant::now())
                    .composer
                    .iter()
                    .all(|row| !row.contains("Subagents")),
                "a settled roster is transcript material, never pinned chrome"
            );
        }

        // The same durable worker can legitimately be continued later, after
        // the owning parent run (not just its workers) has settled.
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
        assert!(shell.state.borrow().subagent_activity_block.is_none());
        let text = transcript_text(&shell);
        assert!(!text
            .split_once("third prompt")
            .unwrap()
            .1
            .contains("Subagents"));
        publish_roster(&mut shell, native, &[fresh]);
        let text = transcript_text(&shell);
        assert!(text
            .split_once("third prompt")
            .unwrap()
            .1
            .contains("FRESH-WORKER"));
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
fn a_rendered_frame_shows_the_roster_exactly_once() {
    for native in [false, true] {
        let mut shell = InteractiveShell::test_shell();
        shell.begin_run("fixture");
        shell.on_prompt_submitted("owning prompt");
        publish_roster(
            &mut shell,
            native,
            &[
                named_worker("LIVE-WORKER", "running"),
                named_worker("DONE-WORKER", "running"),
            ],
        );
        let frame = frame_text(&shell, 120);
        assert_eq!(frame.matches("Subagents").count(), 1, "{frame}");
        assert_eq!(frame.matches("LIVE-WORKER").count(), 1, "{frame}");
        assert_eq!(frame.matches("DONE-WORKER").count(), 1, "{frame}");
        assert!(
            shell_chrome(&shell.state.borrow(), 120, Instant::now())
                .composer
                .iter()
                .all(|row| !row.contains("Subagents") && !row.contains("LIVE-WORKER")),
            "nothing above the composer repeats the roster"
        );

        // A live roster cannot turn subsequent conversation into its preview.
        // Even far above the live tail, every worker and later row is present
        // exactly once in the native frame, with no duplicate composer surface.
        for n in 0..80 {
            shell.notice(format!("filler-{n}"));
        }
        let frame = frame_text(&shell, 120);
        assert_eq!(frame.matches("Subagents").count(), 1, "{frame}");
        assert_eq!(frame.matches("LIVE-WORKER").count(), 1, "{frame}");
        assert_eq!(frame.matches("DONE-WORKER").count(), 1, "{frame}");
        for n in 0..80 {
            assert!(
                frame
                    .lines()
                    .any(|row| row.ends_with(&format!("filler-{n}"))),
                "{frame}"
            );
        }
        assert!(!frame.contains("result pending"), "{frame}");

        publish_roster(
            &mut shell,
            native,
            &[
                named_worker("LIVE-WORKER", "completed"),
                named_worker("DONE-WORKER", "completed"),
            ],
        );
        let frame = frame_text(&shell, 120);
        assert_eq!(frame.matches("Subagents").count(), 1, "{frame}");
        assert_eq!(frame.matches("LIVE-WORKER").count(), 1, "{frame}");
    }
}

#[test]
fn active_roster_preserves_the_complete_native_frame_and_pinned_commit_fence() {
    for native in [false, true] {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(80, 24);
        shell.begin_run("fixture");
        shell.on_prompt_submitted("owning prompt");
        publish_roster(
            &mut shell,
            native,
            &[named_worker("LIVE-WORKER", "running")],
        );
        let index = shell.state.borrow().subagent_activity_block.unwrap();
        // Enough trailing history that the frame is taller than the viewport,
        // which gives the pinned commit ledger a non-trivial maximum row to
        // advance through without clipping the roster's own mutable tail.
        for n in 0..40 {
            shell.notice(format!("filler-{n}"));
        }
        // The cache only has block starts after a frame render; this is test
        // setup, not a product step, so render once before reading the seam.
        let _ = render_shell(&shell.state.borrow(), 80);
        let start = shell.state.borrow().transcript_cache.borrow().block_starts[index];
        assert!(start > 0, "the prompt precedes the roster: {start}");

        // Native row replacement must retain the complete conversation. The
        // experimental pinned path below has a separate semantic commit fence;
        // it must not be implemented by clipping the native logical frame.
        let mut frame = ShellFrameState::default();
        let update = super::native_scrollback::render_shell_update_without_cursor(
            &shell.state.borrow(),
            80,
            Instant::now(),
            &mut frame,
        );
        assert!(update.stable_prefix <= start, "{}", update.stable_prefix);
        let mut materialized = update.replacement;
        assert!(materialized.join("\n").contains("LIVE-WORKER"));
        assert_eq!(frame.pending_tool_start, None);
        let update = super::native_scrollback::render_shell_update_without_cursor(
            &shell.state.borrow(),
            80,
            Instant::now(),
            &mut frame,
        );
        materialized.truncate(update.stable_prefix);
        materialized.extend(update.replacement);
        let text = materialized.join("\n");
        assert_eq!(text.matches("Subagents").count(), 1, "{text}");
        assert_eq!(text.matches("LIVE-WORKER").count(), 1, "{text}");
        for n in 0..40 {
            assert!(
                strip_terminal_sequences(&text)
                    .lines()
                    .any(|row| row.ends_with(&format!("filler-{n}"))),
                "{text}"
            );
        }
        assert!(!text.contains("result pending"), "{text}");

        // The pinned/extended path owns the same invariant through its commit
        // ledger: no row of the active roster may be proven immutable and no
        // commit target may cross it.
        let pinned = render_shell_update(
            &shell.state.borrow(),
            80,
            Instant::now(),
            &mut ShellFrameState::default(),
        );
        let committed = pinned
            .pinned
            .expect("the pinned path carries commit metadata");
        assert!(
            committed.stable_rows <= start,
            "the commit ledger must not prove the active roster immutable: {} <= {start}",
            committed.stable_rows
        );
        assert!(
            committed
                .target
                .is_none_or(|position| position.row <= start),
            "no commit target may cross the active roster: {:?}",
            committed.target
        );

        // Settlement releases the tail on both paths: the same block becomes
        // ordinary history without changing identity.
        publish_roster(
            &mut shell,
            native,
            &[named_worker("LIVE-WORKER", "completed")],
        );
        assert_eq!(shell.state.borrow().subagent_activity_block, Some(index));
        let mut frame = ShellFrameState::default();
        let _ = super::native_scrollback::render_shell_update_without_cursor(
            &shell.state.borrow(),
            80,
            Instant::now(),
            &mut frame,
        );
        assert_eq!(
            frame.pending_tool_start, None,
            "a settled roster is ordinary transcript history"
        );
        let pinned = render_shell_update(
            &shell.state.borrow(),
            80,
            Instant::now(),
            &mut ShellFrameState::default(),
        );
        let committed = pinned
            .pinned
            .expect("the pinned path carries commit metadata");
        assert!(
            committed
                .target
                .is_some_and(|position| position.row > start),
            "a settled roster commits normally: {:?}",
            committed.target
        );
    }
}

#[test]
fn live_roster_rows_update_in_place_while_the_reader_reads_history() {
    for native in [false, true] {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(80, 24);
        shell.begin_run("fixture");
        shell.on_prompt_submitted("owning prompt");
        for n in 0..40 {
            shell.notice(format!("HISTORY-{n}"));
        }
        let mut live = named_worker("LIVE-WORKER", "running");
        publish_roster(&mut shell, native, &[live.clone()]);
        for n in 0..4 {
            shell.notice(format!("TAIL-{n}"));
        }
        shell.scroll_lines(-3);
        shell.scroll_lines(-1);
        assert!(
            !shell.state.borrow().follow_tail,
            "the reader left the tail"
        );
        let before = visible_rows(&shell, 80);
        assert!(
            before.iter().any(|row| row.contains("LIVE-WORKER")),
            "{before:?}"
        );

        // Same row count: only the live worker's own cell changes, so nothing
        // the reader is looking at may move.
        live.output_tokens += 100;
        let scroll_before = shell.state.borrow().scroll_from_bottom.get();
        publish_roster(&mut shell, native, &[live.clone()]);
        let after = visible_rows(&shell, 80);
        assert_eq!(shell.state.borrow().scroll_from_bottom.get(), scroll_before);
        assert!(!shell.state.borrow().follow_tail);
        assert_eq!(after.len(), before.len(), "{before:?}\n{after:?}");
        assert_eq!(
            before.iter().zip(&after).filter(|(a, b)| a != b).count(),
            1,
            "{before:?}\n{after:?}"
        );
        assert!(
            after.iter().any(|row| row.contains("LIVE-WORKER")),
            "{after:?}"
        );

        // A row-count change must not yank the reader to the tail either.
        publish_roster(
            &mut shell,
            native,
            &[live.clone(), named_worker("NEW-WORKER", "running")],
        );
        assert!(
            !shell.state.borrow().follow_tail,
            "a live update never hijacks the reader"
        );
        assert!(
            shell.state.borrow().scroll_from_bottom.get() >= scroll_before,
            "the reader's distance from the live tail is never silently reset"
        );
        let grown = visible_rows(&shell, 80);
        assert!(
            grown.iter().any(|row| row.contains("LIVE-WORKER")),
            "the row the reader was anchored to stays on screen: {grown:?}"
        );
    }
}

#[test]
fn expanded_rosters_keep_quiet_headings_and_explicit_state_columns() {
    let theme = crate::tui::theme::test_theme();
    let uniform = SubagentActivityView {
        telemetry: vec![
            named_worker("worker-a", "running"),
            named_worker("worker-b", "running"),
        ],
        ..SubagentActivityView::default()
    };
    let rows = roster_rows(&uniform, &theme, 120, false);
    let plain = rows
        .iter()
        .map(|row| row.to_owned())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rows[0] == "• Subagents", "{plain}");
    assert!(
        rows.iter()
            .all(|row| !row.trim_start().starts_with("running")),
        "the per-group sub-heading is folded away: {plain}"
    );
    assert!(
        plain.contains("state"),
        "expanded rows keep the state column: {plain}"
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
        rows.iter().any(|row| row.trim() == "running"),
        "expanded group headings omit counts: {plain}"
    );
    assert!(plain.contains("completed · 1"), "{plain}");
    assert!(
        plain.contains("state"),
        "the state column survives a mixed roster: {plain}"
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
        compact.contains("deepseek/deepseek-flash")
            || (compact.contains("deepseek") && compact.contains("flash")),
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
