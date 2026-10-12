//! Completion surfaces use whole-run output rates over provider-active time.

use super::*;

#[test]
fn single_response_completion_uses_reported_output_over_dispatch_to_stream_end() {
    let mut metrics = octet_ai::InferenceMetrics::default();
    let outcome = crate::presentation::RunOutcome::Completed {
        elapsed: Duration::from_secs(2),
        summary: crate::presentation::RunSummary {
            files_changed: 0,
            tool_calls: 0,
            warnings: 0,
        },
    };
    let text = |metrics| {
        let block = super::super::OutcomeBlock::new(outcome.clone(), Some(metrics));
        outcome_parts(&block)
            .into_iter()
            .map(|span| span.t)
            .collect::<String>()
    };
    assert_eq!(text(metrics.clone()), "2.0s · completed · 0 tools");
    let without_metrics = super::super::OutcomeBlock::new(outcome.clone(), None);
    assert_eq!(
        outcome_parts(&without_metrics)
            .into_iter()
            .map(|span| span.t)
            .collect::<String>(),
        "2.0s · completed · 0 tools"
    );
    metrics.decode_estimate = Some(octet_ai::DecodeEstimate {
        tokens_per_second: 100.0,
        reported_visible_tokens: 100,
        observed_ns: 1_000_000_000,
        samples: 20,
        relative_dispersion: 0.01,
        reasoning_tokens_excluded: 0,
    });
    metrics.server = Some(octet_ai::ServerGenerationMetrics {
        source: octet_ai::ServerTimingSource::TimingsPredicted,
        tokens: 100,
        generation_ns: 500_000_000,
        reported_unit: octet_ai::ReportedTimingUnit::Milliseconds,
        prompt_ns: None,
        queue_ns: None,
        total_ns: None,
    });
    // Neither a decode fit nor a server counter carries the dispatch-to-stream-end
    // interval, so neither becomes the displayed rate.
    assert_eq!(text(metrics.clone()), "2.0s · completed · 0 tools");
    metrics.client = Some(octet_ai::ClientInferenceMetrics {
        scope: Some(octet_ai::ClientTimingScope::Request),
        elapsed_ns: 1_600_000_000,
        reported_output_tokens: 120,
        ..Default::default()
    });
    assert_eq!(
        text(metrics),
        "2.0s · completed · 0 tools · 75.0 tok/s · 1 response"
    );
}

#[test]
fn outcome_views_use_owner_gated_whole_run_rate_instead_of_last_response() {
    fn measured(tokens: u64, elapsed_ns: u64) -> octet_ai::InferenceMetrics {
        octet_ai::InferenceMetrics {
            client: Some(octet_ai::ClientInferenceMetrics {
                scope: Some(octet_ai::ClientTimingScope::Request),
                elapsed_ns,
                reported_output_tokens: tokens,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    let mut shell = super::super::InteractiveShell::test_shell();
    let run = shell.begin_run("provider");
    for metrics in [measured(100, 1_000_000_000), measured(200, 4_000_000_000)] {
        shell.on_run_event(run, &octet_agent::AgentEvent::TurnStarted);
        shell.on_run_event(run, &octet_agent::AgentEvent::ProviderInference { metrics });
    }
    shell.on_run_event(
        run,
        &octet_agent::AgentEvent::RunFinished {
            head: octet_agent::EntryId("test-head".into()),
            reason: octet_agent::FinishReason::Completed,
        },
    );
    let block = shell
        .state
        .borrow()
        .transcript
        .iter()
        .find_map(|block| match block {
            super::super::TranscriptBlock::Outcome(block) => Some(block.clone()),
            _ => None,
        })
        .expect("owner-gated run events should settle an outcome");

    let last_response_rate = shell
        .state
        .borrow()
        .last_turn_inference
        .as_ref()
        .and_then(|metrics| metrics.client.as_ref())
        .and_then(octet_ai::ClientInferenceMetrics::output_rate_sample)
        .and_then(|sample| sample.tokens_per_second())
        .expect("the last response has client timing");
    assert_eq!(last_response_rate, 50.0);

    let theme = crate::tui::theme::test_theme();
    let rendered = super::super::outcome_render::render_outcome(&block, &theme, 160, false)
        .into_iter()
        .map(|line| sexy_tui_rs::strip_terminal_sequences(&line))
        .collect::<Vec<_>>()
        .join("\n");
    let tern = outcome_parts(&block)
        .into_iter()
        .map(|span| span.t)
        .collect::<String>();
    let copied = super::super::block_copy_text(&super::super::TranscriptBlock::Outcome(block));
    let expected = "60.0 tok/s · 2 responses";
    for view in [rendered, tern, copied] {
        assert!(view.contains(expected), "{view:?}");
        assert!(
            !view.contains("50.0 tok/s"),
            "last-response rate leaked: {view:?}"
        );
        assert!(!view.contains("E2E"), "{view:?}");
    }
}
