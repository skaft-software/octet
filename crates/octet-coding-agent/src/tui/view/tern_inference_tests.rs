//! Native completion shows only an available rate, preferring server timing.

use super::*;

#[test]
fn native_completion_prefers_server_generation_then_estimated_decode() {
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
    assert_eq!(
        text(metrics.clone()),
        "2.0s · completed · 0 tools · 100.0 tok/s"
    );
    metrics.server = Some(octet_ai::ServerGenerationMetrics {
        source: octet_ai::ServerTimingSource::TimingsPredicted,
        tokens: 100,
        generation_ns: 500_000_000,
        reported_unit: octet_ai::ReportedTimingUnit::Milliseconds,
        prompt_ns: None,
        queue_ns: None,
        total_ns: None,
    });
    let native = text(metrics);
    assert_eq!(native, "2.0s · completed · 0 tools · 200.0 tok/s");
    assert!(!native.contains("decode"));
    assert!(!native.contains("E2E"));
}
