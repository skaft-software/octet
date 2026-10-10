//! The status footer: its collapsed shape, live metadata, throughput, pricing, and session cost.
//! Separate because the footer is the one always-visible row with a strict height budget.

use super::support::*;

use super::*;

#[test]
fn default_footer_groups_live_metadata_and_right_aligns_workspace() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_workspace(PathBuf::from("/work/octet"));
    shell.set_identity("openai", "gpt-6-astra", "xhigh");
    {
        let mut state = shell.state.borrow_mut();
        state.model_display = "GPT-6-Astra".into();
        state.model_compact_names = vec!["GPT-6-Astra".into(), "GPT-6".into()];
        state.context_estimate = Some((84_320, 272_000));
        state.session_cost_microdollars = Some(295_000_000);
        state.price_display = PriceDisplay::Priced;
    }
    let now = Instant::now();
    let left = "GPT-6-Astra · xhigh · 31%/272K · $295";
    let cwd = "/work/octet";
    let expected = format!(
        "  {left}{}{cwd}",
        " ".repeat(96 - visible_width(left) - cwd.len())
    );
    assert_eq!(plain_footer(&shell, 100, now), expected);
    shell.set_context_estimate(136_000, 272_000);
    shell.state.borrow_mut().session_cost_microdollars = Some(296_000_000);
    let updated = plain_footer(&shell, 100, now);
    assert!(updated.contains("50%/272K · $296"), "{updated:?}");
    assert!(!updated.contains("31%") && !updated.contains("$295"));
    assert!(updated.ends_with(cwd));
    for width in 1..=120 {
        let footer = plain_footer(&shell, width, now);
        assert!(
            visible_width(&footer) <= usize::from(width),
            "{width}: {footer:?}"
        );
    }
    shell.state.borrow_mut().usage_uncertain = true;
    let unknown = plain_footer(&shell, 100, now);
    assert!(unknown.contains("$296"), "{unknown:?}");
    assert!(
        !unknown.contains("subtotal") && !unknown.contains('?'),
        "{unknown:?}"
    );
    assert!(shell.state.borrow().usage_uncertain);
    shell.state.borrow_mut().session_cost_microdollars = None;
    let unknown = plain_footer(&shell, 100, now);
    assert!(!unknown.contains("usage/cost unknown"), "{unknown:?}");
    assert!(shell.state.borrow().usage_uncertain);
    assert!(!unknown.contains('$'));
}

#[test]
fn default_footer_ascii_has_no_terminal_controls_or_unicode_separators() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    let mut shell = InteractiveShell::test_shell_with_theme(crate::tui::theme::test_theme_with(
        TerminalCapabilities::test(true, false, ColorDepth::None),
    ));
    shell.set_identity("openai", "gpt-5.6", "high");
    shell.set_context_estimate(84_320, 272_000);
    shell.set_workspace(PathBuf::from(
        "/a/long/directory/with/\x1b[31mcolors\x1b[0m\nnewline/project",
    ));
    shell.state.borrow_mut().session_cost_microdollars = Some(295_000_000);
    let now = Instant::now();
    for width in 1..=120 {
        let footer = crate::tui::composer_surface::render_composer_surface(
            &shell.state.borrow(),
            width,
            now,
        )
        .pop()
        .unwrap();
        assert!(
            visible_width(&footer) <= usize::from(width),
            "{width}: {footer:?}"
        );
        assert!(!footer.chars().any(char::is_control), "{footer:?}");
        assert!(!footer.contains('·') && !footer.contains('…'), "{footer:?}");
    }
    let footer = plain_footer(&shell, 120, now);
    assert!(
        footer.contains("31%/272K") && footer.contains("$295"),
        "{footer:?}"
    );
    assert!(footer.ends_with("/project"), "{footer:?}");
}

fn throughput_finished_event(output_tokens: u64) -> AgentEvent {
    let usage = Usage {
        output_tokens,
        reasoning_tokens: output_tokens.saturating_sub(23),
        total_tokens: output_tokens,
        ..Usage::default()
    };
    AgentEvent::TurnFinished {
        turn_cost: None,
        message: octet_ai::AssistantMessage {
            content: vec![octet_ai::AssistantPart::Text("answer".into())],
            model: octet_ai::ModelId("test".into()),
            protocol: octet_ai::Protocol::AnthropicMessages,
        },
        stop_reason: octet_ai::StopReason::EndTurn,
        turn_usage: usage,
        usage,
        session_cost_microdollars: None,
        run_cost_microdollars: 0,
    }
}

#[test]
fn throughput_includes_hidden_thinking_and_works_without_visible_deltas() {
    for channel in [
        None,
        Some(OutputChannel::Text),
        Some(OutputChannel::Reasoning),
    ] {
        let mut shell = InteractiveShell::test_shell();
        let id = shell.begin_run("test");
        shell.on_run_event(id, &AgentEvent::TurnStarted);
        let requested = Instant::now() - Duration::from_secs(4);
        shell.state.borrow_mut().turn_requested_at = Some(requested);
        if let Some(channel) = channel {
            shell.on_run_event(
                id,
                &AgentEvent::OutputDelta {
                    channel,
                    text: "buffered output".into(),
                },
            );
            shell.state.borrow_mut().turn_generation_started_at =
                Some(requested + Duration::from_millis(3900));
        }
        shell.on_run_event(id, &throughput_finished_event(128));
        let state = shell.state.borrow();
        let elapsed = state.last_turn_provider_elapsed.unwrap();
        assert!(elapsed >= Duration::from_secs(4));
        let rate = state.last_turn_tokens_per_second.unwrap();
        assert_eq!(rate, 128.0 / elapsed.as_secs_f64());
        assert!(rate <= 32.0, "hidden thinking must not yield 1280 tok/s");
        assert_eq!(state.last_turn_generated_tokens, Some(128));
        assert_eq!(
            state.last_turn_first_token,
            channel.map(|_| Duration::from_millis(3900))
        );
        assert!(state.turn_requested_at.is_none());
        let status = status_telemetry(&state, Instant::now());
        assert!(status.contains("tok/s end-to-end, last attempt"));
        assert!(status.contains("request-to-completion; not server speed"));
    }
}

#[test]
fn throughput_resets_per_attempt_and_never_reuses_missing_measurements() {
    let mut shell = InteractiveShell::test_shell();
    let id = shell.begin_run("test");
    shell.on_run_event(id, &AgentEvent::TurnStarted);
    shell.state.borrow_mut().turn_requested_at = Some(Instant::now() - Duration::from_secs(60));
    shell.on_run_event(
        id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "discarded attempt".into(),
        },
    );
    shell.on_run_event(id, &retry_event(1));
    let before_retry = Instant::now();
    shell.on_run_event(id, &AgentEvent::TurnStarted);
    {
        let state = shell.state.borrow();
        assert!(state.turn_requested_at.unwrap() >= before_retry);
        assert!(state.turn_generation_started_at.is_none());
        assert!(state.last_turn_tokens_per_second.is_none());
    }
    shell.on_run_event(
        id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: String::new(),
        },
    );
    assert!(shell.state.borrow().turn_generation_started_at.is_none());
    shell.on_run_event(id, &throughput_finished_event(128));
    assert!(shell.state.borrow().last_turn_tokens_per_second.is_some());

    // Missing request timing cannot inherit the preceding turn's rate.
    shell.on_run_event(id, &throughput_finished_event(128));
    assert!(shell.state.borrow().last_turn_tokens_per_second.is_none());
    assert!(shell.state.borrow().last_turn_provider_elapsed.is_none());
    shell.on_run_event(id, &AgentEvent::TurnStarted);
    shell.on_run_event(id, &throughput_finished_event(0));
    assert!(shell.state.borrow().last_turn_tokens_per_second.is_none());
}

#[test]
fn footer_omits_noisy_throughput_but_keeps_final_rate_in_status() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("openai", "gpt-5.6", "high");
    let started = Instant::now();
    {
        let mut state = shell.state.borrow_mut();
        let id = state.run.begin_at("codex", started).unwrap();
        state.run_model = Some(state.model.clone());
        state.telemetry_model = state.run_model.clone();
        state.run_model_display = Some(state.model_display.clone());
        state.run_model_compact_names = state.model_compact_names.clone();
        state.run_reasoning = Some(state.reasoning.clone());
        state.run_price_display = Some(PriceDisplay::Unknown);
        state.run_context_estimate = Some((21_000, 256_000));
        state.run.set_phase_at(
            id,
            RunPhase::AwaitingProvider {
                provider: "codex".into(),
            },
            started,
        );
        state.turn_generation_started_at = Some(started);
        state.turn_requested_at = Some(started);
        state.turn_streamed_output_bytes = 2_520;
        state.context_estimate = Some((21_000, 256_000));
        state.price_display = PriceDisplay::Unknown;
        state.run_cost_available = false;
    }
    let now = started + Duration::from_millis(8_700);
    let live = plain_footer(&shell, 100, now);
    assert!(!live.contains("Working"), "{live:?}");
    assert!(!live.contains("waiting for API"), "{live:?}");
    assert!(
        !live.contains("tok/s"),
        "noisy live throughput leaked into footer: {live:?}"
    );
    assert!(
        !live.contains('↑') && !live.contains('↓'),
        "token counters leaked into the simplified footer: {live:?}"
    );
    assert!(
        live.contains("~8%/256K"),
        "live context pressure missing: {live:?}"
    );
    assert!(
        !live.contains("cost"),
        "unknown price stays quiet: {live:?}"
    );
    assert!(!live.contains('—'), "unknown price stays quiet: {live:?}");
    assert!(
        !live.contains("esc"),
        "implicit controls stay out: {live:?}"
    );
    assert!(
        visible_width(&live) <= 98,
        "status stays inside the right inset"
    );
    let live_diagnostics = status_telemetry(&shell.state.borrow(), now);
    assert!(live_diagnostics.contains("awaiting turn completion"));
    assert!(!live_diagnostics.contains("tok/s"));

    {
        let mut state = shell.state.borrow_mut();
        state.price_display = PriceDisplay::Priced;
        state.run_price_display = Some(PriceDisplay::Priced);
        state.run_cost_available = true;
        state.run_cost_microdollars = 82_000;
        state.session_cost_microdollars = Some(120_000);
    }
    let paid = plain_footer(&shell, 100, now);
    assert!(
        paid.contains("$0.120"),
        "accumulated session cost should be visible: {paid:?}"
    );
    assert!(
        paid.contains(" · $0.120"),
        "durable session spend should follow the context group: {paid:?}"
    );
    assert!(!paid.contains("Working"), "{paid:?}");

    {
        let mut state = shell.state.borrow_mut();
        state.turn_generation_started_at = None;
        state.turn_streamed_output_bytes = 0;
        state.last_turn_tokens_per_second = Some(72.4);
        state.last_turn_provider_elapsed = Some(Duration::from_secs(2));
        state.last_turn_generated_tokens = Some(145);
        let id = state.run.current_id().unwrap();
        state.run.set_phase_at(
            id,
            RunPhase::RunningTool {
                summary: "running tests".into(),
            },
            started,
        );
    }
    let active_sample = plain_footer(&shell, 100, now);
    assert!(
        !active_sample.contains("tok/s"),
        "final throughput leaked into footer while tools run: {active_sample:?}"
    );
    assert!(
        !active_sample.contains("8.7s"),
        "default timer leaked: {active_sample:?}"
    );
    assert!(!active_sample.contains("tool"));
    let final_diagnostics = status_telemetry(&shell.state.borrow(), now);
    assert!(final_diagnostics.contains("72.4 tok/s end-to-end, last attempt"));

    {
        let mut state = shell.state.borrow_mut();
        let id = state.run.current_id().unwrap();
        state.run.interrupt_at(id, now);
    }
    let completed_sample = plain_footer(&shell, 100, now);
    assert!(
        !completed_sample.contains("tok/s"),
        "final throughput leaked into settled footer: {completed_sample:?}"
    );
    assert!(!completed_sample.contains('~'), "{completed_sample:?}");
}

#[test]
fn footer_distinguishes_explicit_zero_from_unavailable_pricing() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("local", "qwen3.6-35b-a3b", "high");
    let now = Instant::now();

    shell.state.borrow_mut().price_display = PriceDisplay::Unknown;
    let unknown = plain_footer(&shell, 80, now);
    assert!(!unknown.contains('$'));
    assert!(!unknown.contains("cost"));

    {
        let mut state = shell.state.borrow_mut();
        state.price_display = PriceDisplay::Priced;
        state.run_cost_available = true;
        state.run_cost_microdollars = 0;
    }
    let not_yet_charged = plain_footer(&shell, 80, now);
    assert!(!not_yet_charged.contains('$'));

    shell.state.borrow_mut().price_display = PriceDisplay::ExplicitZero;
    let free = plain_footer(&shell, 80, now);
    assert!(free.ends_with("$0"), "{free:?}");

    for width in 1..=120 {
        let surface = plain_composer_surface(&shell, width, now);
        assert!(surface
            .iter()
            .all(|line| visible_width(line) <= usize::from(width)));
    }
}

#[test]
fn idle_footer_shows_accumulated_session_cost_without_opt_in() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("openai", "gpt-5.6-luna", "high");
    {
        let mut state = shell.state.borrow_mut();
        state.price_display = PriceDisplay::Priced;
        state.session_cost_microdollars = Some(91_400);
        state.cache_hit_rate_basis_points = Some(9_240);
        state.context_estimate = Some((102, 272_000));
        state.telemetry_model = Some(state.model.clone());
    }

    let footer = plain_footer(&shell, 120, Instant::now());
    assert!(footer.contains("0%/272K"), "{footer:?}");
    assert!(!footer.contains("102/272k"), "{footer:?}");
    assert!(!footer.contains("cache 92.4%"), "{footer:?}");
    assert!(!footer.contains("session"), "{footer:?}");
    assert!(
        footer.contains("$0.0914"),
        "accumulated session cost missing: {footer:?}"
    );
    assert!(!footer.contains('~'), "{footer:?}");
}

#[test]
fn subagent_chrome_renders_live_metrics_and_rolls_cost_into_footer_once() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("openai", "gpt-5.6-luna", "high");
    shell.state.borrow_mut().session_cost_microdollars = Some(91_400);
    let snapshot: octet_agent::ExtensionPresentationSnapshot =
        serde_json::from_value(serde_json::json!({
            "revision": 1,
            "status": {"state": "active", "label": "Subagents"},
            "activities": [{
                "id": "activity:agent-1",
                "kind": "subagent",
                "state": "running",
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
        .unwrap();

    assert!(shell.set_subagent_presentation(Some(&snapshot), true));
    // Workers stay visible even while the root run is idle.
    assert!(!shell.state.borrow().run.is_active());
    let chrome = shell_chrome(&shell.state.borrow(), 120, Instant::now());
    assert!(
        chrome.composer.iter().all(|row| !row.contains("Subagents")),
        "{:?}",
        chrome.composer
    );
    assert!(chrome.subagents.is_empty());
    let activity = shell
        .state
        .borrow()
        .rendered_transcript(120)
        .iter()
        .map(|row| strip_terminal_sequences(row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        activity.contains("Subagents · 1 running · /subagents"),
        "{activity}"
    );
    assert!(
        !activity.contains("read-diffs")
            && !activity.contains("↑88.2k")
            && !activity.contains("$0.209"),
        "{activity}"
    );
    assert_eq!(
        shell
            .state
            .borrow()
            .subagent_activity
            .as_ref()
            .unwrap()
            .activities,
        snapshot.activities,
        "rendering must retain tool-call and usage accounting"
    );
    assert!(plain_footer(&shell, 120, Instant::now()).contains("$0.300"));

    assert!(shell.set_subagent_presentation(Some(&snapshot), false));
    let footer = plain_footer(&shell, 120, Instant::now());
    assert!(footer.contains("$0.0914"), "{footer}");
    assert!(!footer.contains("$0.300"), "{footer}");
}

#[test]
fn live_subagent_heading_is_bold_and_worker_metadata_is_terminal_safe() {
    let shell = InteractiveShell::test_shell();
    let mut view = subagent_transcript_test_view(true);
    view.telemetry[0].task_name = "audit\x1b]52;c;SECRET\x07".into();
    shell.state.borrow_mut().set_subagent_activity(view);
    let state = shell.state.borrow();
    let rows = state.rendered_transcript(80);
    let heading = rows.iter().find(|row| row.contains("Subagents")).unwrap();
    assert!(
        heading.contains("\x1b[1m") && heading.contains("\x1b[22m"),
        "{heading:?}"
    );
    let plain = strip_terminal_sequences(&rows.join("\n"));
    assert!(plain.contains("  └ audit · ↑5.6M ↓3.9K"), "{plain}");
    assert!(!plain.contains("SECRET"), "{plain}");
}

#[test]
fn live_subagent_output_progress_is_marked_as_estimated_until_usage_settles() {
    let shell = InteractiveShell::test_shell();
    let mut view = subagent_transcript_test_view(true);
    view.telemetry[0].task_name = "audit".into();
    view.telemetry[0].estimated_output_tokens = Some(4_021);
    shell.state.borrow_mut().set_subagent_activity(view.clone());
    let live = strip_terminal_sequences(&shell.state.borrow().rendered_transcript(80).join("\n"));
    assert!(live.contains("audit · ↑5.6M ↓~4K"), "{live}");
    assert_eq!(view.telemetry[0].output_tokens, 3_900);
    assert_eq!(view.telemetry[0].total_tokens, 5_603_900);

    view.telemetry[0].output_tokens = 4_010;
    view.telemetry[0].estimated_output_tokens = None;
    shell.state.borrow_mut().set_subagent_activity(view);
    let settled =
        strip_terminal_sequences(&shell.state.borrow().rendered_transcript(80).join("\n"));
    assert!(settled.contains("audit · ↑5.6M ↓4K"), "{settled}");
    assert!(!settled.contains("↓~"), "{settled}");
}

#[test]
fn subagent_rows_share_thinking_indent_and_elbow() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};

    for unicode in [false, true] {
        for width in [32, 46, 80, 120] {
            let theme = crate::tui::theme::test_theme_with(TerminalCapabilities::test(
                true,
                unicode,
                ColorDepth::None,
            ));
            let mut shell = InteractiveShell::test_shell_with_theme(theme);
            let run_id = shell.begin_run("test");
            shell.on_run_event(
                run_id,
                &AgentEvent::OutputDelta {
                    channel: OutputChannel::Reasoning,
                    text: "## Editing docs\n\n".into(),
                },
            );
            let mut view = subagent_transcript_test_view(true);
            let worker = view.telemetry[0].clone();
            view.telemetry = (0..5)
                .map(|index| {
                    let mut child = worker.clone();
                    child.child_id = format!("worker-{index}");
                    child.task_name = format!("worker-{index}");
                    child
                })
                .collect();
            shell.state.borrow_mut().set_subagent_activity(view);
            let frame = strip_terminal_sequences(
                &shell.state.borrow().rendered_transcript(width).join("\n"),
            );
            let prefix = if unicode { "  └ " } else { "  `- " };
            let thinking = frame
                .lines()
                .find(|line| line.contains("Editing docs"))
                .unwrap();
            assert!(thinking.starts_with(prefix), "{width}: {frame}");
            let workers: Vec<_> = frame
                .lines()
                .filter(|line| line.contains("worker-"))
                .collect();
            assert_eq!(workers.len(), 4, "{width}: {frame}");
            assert!(
                workers.iter().all(|line| line.starts_with(prefix)),
                "{width}: {frame}"
            );
            assert!(
                frame.lines().any(|line| line == format!("{prefix}+1 more")),
                "{width}: {frame}"
            );
            assert!(
                !frame.contains("└─") && !frame.contains("├─"),
                "{width}: {frame}"
            );
        }
    }
}

#[test]
fn subagent_token_labels_are_compact_without_changing_usage() {
    for (tokens, label) in [
        (0, "0"),
        (999, "999"),
        (1_000, "1K"),
        (1_050, "1.1K"),
        (1_371, "1.4K"),
        (2_020, "2K"),
        (20_992, "21K"),
        (40_582, "40.6K"),
        (99_950, "100K"),
        (215_552, "216K"),
        (284_364, "284K"),
        (999_499, "999K"),
        (999_500, "1M"),
        (5_600_000, "5.6M"),
        (999_500_000, "1B"),
        (1_500_000_000, "1.5B"),
        (999_500_000_000, "1T"),
        (1_200_000_000_000, "1.2T"),
        (u64::MAX, "18446744T"),
    ] {
        let shell = InteractiveShell::test_shell();
        let mut view = subagent_transcript_test_view(true);
        view.telemetry.truncate(1);
        let child = &mut view.telemetry[0];
        child.task_name = "audit".into();
        child.input_tokens = tokens;
        child.cache_read_tokens = 0;
        child.cache_write_tokens = 0;
        child.output_tokens = tokens;
        child.total_tokens = tokens.saturating_mul(2);
        for estimated in [false, true] {
            view.telemetry[0].estimated_output_tokens = estimated.then_some(tokens);
            shell.state.borrow_mut().set_subagent_activity(view.clone());
            let state = shell.state.borrow();
            let frame = strip_terminal_sequences(&state.rendered_transcript(80).join("\n"));
            let marker = if estimated { "~" } else { "" };
            assert!(
                frame.contains(&format!("audit · ↑{label} ↓{marker}{label}")),
                "{tokens}: {frame}"
            );
            assert_eq!(
                state.subagent_activity.as_ref().unwrap().telemetry,
                view.telemetry
            );
        }
    }
}

#[test]
fn subagent_stop_hint_is_visible_only_while_workers_are_active() {
    for width in [32, 46, 80, 120] {
        let shell = InteractiveShell::test_shell();
        let mut view = subagent_transcript_test_view(true);
        shell.state.borrow_mut().set_subagent_activity(view.clone());
        let live =
            strip_terminal_sequences(&shell.state.borrow().rendered_transcript(width).join("\n"));
        assert!(live.contains("/subagents stop all"), "{width}: {live}");
        view.telemetry[0].state = "completed".into();
        shell.state.borrow_mut().set_subagent_activity(view);
        let settled =
            strip_terminal_sequences(&shell.state.borrow().rendered_transcript(width).join("\n"));
        assert!(
            !settled.contains("/subagents stop all"),
            "{width}: {settled}"
        );
    }
}

#[test]
fn subagent_transcript_row_is_height_bounded_and_points_to_the_inspector() {
    for width in [32, 80, 120] {
        for height in [12, 16, 24] {
            let mut shell = InteractiveShell::test_shell();
            shell.set_size(width, height);
            let mut view = subagent_transcript_test_view(true);
            let worker = view.telemetry[0].clone();
            view.telemetry = (0..8)
                .map(|index| {
                    let mut child = worker.clone();
                    child.child_id = format!("worker-{index}");
                    child.task_name = format!("worker-{index}");
                    child
                })
                .collect();
            shell.state.borrow_mut().set_subagent_activity(view);
            let state = shell.state.borrow();
            let rows = state.rendered_transcript(width);
            assert!(shell_chrome(&state, width, Instant::now())
                .subagents
                .is_empty());
            assert_eq!(
                rows.iter().filter(|row| row.contains("/subagents")).count(),
                1
            );
            assert!(rows
                .iter()
                .all(|row| visible_width(row) <= usize::from(width)));
            let plain = rows
                .iter()
                .map(|row| strip_terminal_sequences(row))
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(plain.matches("worker-").count(), 4, "{plain}");
            assert!(plain.contains("+4 more"), "{plain}");
            assert!(!plain.contains("worker-4"), "{plain}");
            assert_eq!(state.subagent_activity.as_ref().unwrap().telemetry.len(), 8);
        }
    }
}

#[test]
fn native_subagent_telemetry_renders_failure_and_hides_generic_spawn_tools() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(120, 60);
    let child = |id: &str, task: &str, state: &str, reason: Option<&str>| {
        octet_agent::DelegationTelemetryChild {
            child_id: id.into(),
            task_name: task.into(),
            profile: Some("explore".into()),
            model: "cerebras-gemma-4-31b".into(),
            state: state.into(),
            phase: if state == "failed" {
                "failed"
            } else {
                "using_tool"
            }
            .into(),
            current_tool: (state == "running").then(|| "read".into()),
            tool_use_count: 4,
            input_tokens: 12_000,
            cache_read_tokens: 800,
            cache_write_tokens: 0,
            output_tokens: 220,
            estimated_output_tokens: None,
            reasoning_tokens: 60,
            total_tokens: 13_020,
            cost: None,
            cost_microdollars: Some(7_200),
            elapsed_ms: 42_000,
            failure_class: reason.map(|_| "provider_failure".into()),
            failure_reason: reason.map(str::to_owned),
            effective_tool_policy: test_effective_tool_policy(),
            orchestration_provenance: inherited_delegation_provenance(),
            session: Some("agent-session:opaque".into()),
        }
    };
    let snapshot = octet_agent::DelegationTelemetrySnapshot {
        revision: 4,
        captured_at_ms: 1_700_000_000_000,
        children: vec![
            child("agent-1", "Read release history", "running", None),
            child(
                "agent-2",
                "Audit release surface",
                "failed",
                Some("provider request failed: upstream unavailable"),
            ),
        ],
        total_cost_microdollars: Some(14_400),
        failure_reason: Some("spawn rejected: worker limit reached".into()),
        failure_class: Some("spawn_rejected".into()),
    };
    publish_current_turn_roster(&mut shell, snapshot);
    let block = shell
        .state
        .borrow()
        .rendered_transcript(120)
        .iter()
        .map(|row| strip_terminal_sequences(row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        block.contains("Subagents · 1 running · 1 failed · /subagents"),
        "{block}"
    );
    assert!(
        block.contains("Read release history · ↑12.8K ↓220"),
        "{block}"
    );
    assert!(
        !block.contains("Audit release surface") && !block.contains("provider request failed"),
        "{block}"
    );
    assert!(shell_chrome(&shell.state.borrow(), 120, Instant::now())
        .subagents
        .is_empty());
    let state = shell.state.borrow();
    let view = state.subagent_activity.as_ref().unwrap();
    assert!(view.telemetry.iter().all(|child| child.tool_use_count == 4));
    assert!(view
        .telemetry
        .iter()
        .any(|child| child.failure_reason.as_deref()
            == Some("provider request failed: upstream unavailable")));
    assert_eq!(
        view.failure_reason.as_deref(),
        Some("spawn rejected: worker limit reached")
    );
    let notices = state
        .transcript
        .iter()
        .filter_map(|block| match block {
            TranscriptBlock::Notice(text) => Some(text),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        notices.is_empty(),
        "worker diagnostics remain inspector-only"
    );
    drop(state);
    assert!(shell
        .state
        .borrow()
        .rendered_transcript(120)
        .join("\n")
        .contains("Subagents"));

    // An empty cleanup snapshot must not erase retained inspector telemetry.
    shell.on_agent_event(&octet_agent::AgentEvent::DelegationUpdated {
        snapshot: octet_agent::DelegationTelemetrySnapshot {
            revision: 5,
            captured_at_ms: 1_700_000_000_001,
            children: Vec::new(),
            total_cost_microdollars: None,
            failure_reason: None,
            failure_class: None,
        },
    });
    assert!(shell.state.borrow().subagent_activity.is_some());
    assert!(shell_chrome(&shell.state.borrow(), 120, Instant::now())
        .subagents
        .is_empty());

    shell.on_agent_event(&octet_agent::AgentEvent::ToolStarted {
        id: octet_ai::ToolCallId("spawn-call".into()),
        name: "subagent_spawn".into(),
        args: serde_json::json!({"name": "worker"}),
    });
    let transcript = shell.state.borrow().rendered_transcript(120).join("\n");
    assert!(!transcript.contains("Used subagent spawn"), "{transcript}");
}

#[test]
fn settled_subagent_attention_is_quiet_with_inspector_retention() {
    for terminal in ["failed", "stopped", "awaiting_approval"] {
        let shell = InteractiveShell::test_shell();
        let mut view = subagent_transcript_test_view(true);
        view.telemetry.truncate(1);
        let notices = |shell: &InteractiveShell| {
            shell
                .state
                .borrow()
                .transcript
                .iter()
                .filter_map(|block| match block {
                    TranscriptBlock::Notice(text) => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        shell.state.borrow_mut().set_subagent_activity(view.clone());
        assert!(notices(&shell).is_empty());
        view.telemetry[0].tool_use_count += 1;
        shell.state.borrow_mut().set_subagent_activity(view.clone());
        assert!(notices(&shell).is_empty(), "live metrics are chrome-only");
        assert!(!shell.state.borrow().run.is_active());

        view.telemetry[0].state = terminal.into();
        view.telemetry[0].failure_reason =
            Some("action required: \x1b[31mreview worker\x1b[0m".into());
        shell.state.borrow_mut().set_subagent_activity(view.clone());
        assert!(shell_chrome(&shell.state.borrow(), 120, Instant::now())
            .subagents
            .is_empty());
        let first = notices(&shell);
        assert!(
            first.is_empty(),
            "settlement does not append worker notices"
        );

        // Repeated state/reason, even with fresh accounting, must not spam history.
        view.telemetry[0].output_tokens += 1;
        view.telemetry[0].elapsed_ms += 100;
        shell.state.borrow_mut().set_subagent_activity(view.clone());
        assert_eq!(notices(&shell), first);
        assert_eq!(
            shell
                .state
                .borrow()
                .subagent_activity
                .as_ref()
                .unwrap()
                .telemetry,
            view.telemetry
        );

        view.telemetry[0].failure_reason = Some("new diagnostic ".repeat(1000));
        shell.state.borrow_mut().set_subagent_activity(view.clone());
        let changed = notices(&shell);
        assert!(
            changed.is_empty(),
            "changed worker diagnostics stay in the inspector"
        );
        assert_eq!(
            shell
                .state
                .borrow()
                .subagent_activity
                .as_ref()
                .unwrap()
                .telemetry,
            view.telemetry
        );
        assert!(!shell
            .state
            .borrow()
            .transcript
            .iter()
            .any(|block| matches!(block, TranscriptBlock::Tool(_))));

        view.telemetry[0].state = "completed".into();
        view.telemetry[0].failure_reason = None;
        shell.state.borrow_mut().set_subagent_activity(view);
        assert_eq!(
            notices(&shell),
            changed,
            "ordinary completion adds no history"
        );
    }
}
