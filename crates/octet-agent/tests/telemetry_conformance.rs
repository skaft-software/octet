//! Behavioral conformance for the vendor-neutral callback telemetry substrate.
//!
//! Rows 3.1-3.4: one runner-independent case list runs against the recording
//! adapter, the inert context is asserted to stay passive without swallowing
//! callbacks, and the serializable schema round-trips. The durable JSONL
//! accounting path is asserted separately so a telemetry observer can never be
//! mistaken for an accounting authority.

use octet_agent::telemetry::schema::{
    agent_telemetry_schema, cache_hit_rate, AttributeType, CompletionAttributes, EmptyAttributes,
    ParentDefinition, ProviderOperation, RequestAttributes, SpanSchema, ToolAttributes, TurnSpan,
};
use octet_agent::telemetry::spans::{
    AttributeValue, InMemoryTelemetryContext, RecordedTelemetrySpan, SpanAttributes, SpanOptions,
    SpanStatus, TelemetryContext, TelemetryLimits, NOOP_TELEMETRY_CONTEXT,
};
use octet_agent::telemetry::testing::{conformance_cases, SpanAssertions};
use octet_agent::telemetry::{TelemetryObserver, TELEMETRY_SCHEMA};
use octet_ai::Usage;

fn numeric(pairs: &[(&str, f64)]) -> SpanAttributes {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), AttributeValue::Number(*value)))
        .collect()
}

#[tokio::test]
async fn every_conformance_case_passes_for_the_recording_adapter() {
    for case in conformance_cases() {
        let fixture = InMemoryTelemetryContext::default();
        case.run(&fixture).await;
        eprintln!("ok [{}] {}", case.group, case.name);
    }
}

#[tokio::test]
async fn inert_context_runs_callbacks_once_without_recording() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let calls = std::sync::Arc::new(AtomicU32::new(0));
    let nested_calls = std::sync::Arc::new(AtomicU32::new(0));
    let outer = calls.clone();
    let inner = nested_calls.clone();
    let result: Result<u32, &'static str> = NOOP_TELEMETRY_CONTEXT
        .start_span(SpanOptions::new("noop"), move |span| {
            outer.fetch_add(1, Ordering::SeqCst);
            let inner = inner.clone();
            async move {
                let child: Result<(), ()> = span
                    .start_span(SpanOptions::new("noop-child"), move |_child| {
                        inner.fetch_add(1, Ordering::SeqCst);
                        async { Ok(()) }
                    })
                    .await;
                child.unwrap();
                Err("kept")
            }
        })
        .await;
    assert_eq!(result, Err("kept"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(nested_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn inert_context_is_default_and_never_panics_on_late_calls() {
    let context = TelemetryContext::default();
    assert!(format!("{context:?}").contains("enabled: false"));
    let span = {
        let mut captured = None;
        let _: Result<(), ()> = context
            .start_span(SpanOptions::new("noop-sync"), |span| {
                captured = Some(span);
                async { Ok(()) }
            })
            .await;
        captured.unwrap()
    };
    span.set_attributes(numeric(&[("late", 1.0)]));
    span.add_event("late", SpanAttributes::new());
    span.set_status(SpanStatus::Error);
    assert!(format!("{:?}", NOOP_TELEMETRY_CONTEXT).contains("enabled: false"));
}

#[tokio::test]
async fn recording_adapter_enforces_bounds_without_dropping_callbacks() {
    let fixture = InMemoryTelemetryContext::new(TelemetryLimits {
        spans: 2,
        events_per_span: 1,
        attributes_per_span: 1,
        payload_bytes: 64,
    });
    let context = fixture.context();
    let mut calls = 0;
    for index in 0..4 {
        let _: Result<(), ()> = context
            .start_span(SpanOptions::new(format!("bounded-{index}")), |span| {
                calls += 1;
                span.add_event("first", numeric(&[("a", 1.0)]));
                span.add_event("second", numeric(&[("b", 2.0)]));
                async { Ok(()) }
            })
            .await;
    }
    assert_eq!(calls, 4, "bounds must never suppress business callbacks");
    let spans = fixture.get_spans();
    assert_eq!(spans.len(), 2, "only bounded spans are retained");
    assert_eq!(fixture.dropped_spans(), 2);
    assert!(spans.iter().all(|span| span.events.len() == 1));
}

#[tokio::test]
async fn typed_instrumentation_nests_children_under_the_typed_span() {
    let fixture = InMemoryTelemetryContext::default();
    let context = fixture.context();
    let outcome: Result<u32, ()> = context
        .start_typed::<TurnSpan, _, _, _>(EmptyAttributes {}, |typed| async move {
            let child: Result<(), ()> = typed
                .context()
                .start_span(SpanOptions::new("child"), |_child| async { Ok(()) })
                .await;
            child?;
            Ok(7)
        })
        .await;
    assert_eq!(outcome, Ok(7));
    let spans = fixture.get_spans();
    let assertions = SpanAssertions::new(&spans);
    assert_eq!(assertions.find(TurnSpan::NAME).name, "octet.agent.turn");
    assertions.assert_parent("child", TurnSpan::NAME);
    assertions.assert_all_settled();
}

#[test]
fn serializable_schema_and_completion_usage_preserve_disjoint_buckets() {
    let schema = agent_telemetry_schema();
    assert_eq!(schema.version, 1);
    let turn = schema.spans.get(TurnSpan::NAME).expect("turn span");
    assert_eq!(turn.parents, ParentDefinition::Any);
    assert_eq!(
        turn.end_attributes.get("cache_write_1h_tokens").unwrap().value_type,
        AttributeType::Number
    );
    assert_eq!(
        turn.end_attributes
            .get("has_uncertain_usage")
            .unwrap()
            .value_type,
        AttributeType::Boolean
    );
    let request = schema.spans.get("octet.ai.request").expect("request span");
    assert!(request.start_attributes.get("operation").unwrap().required);
    let definition = request
        .start_attributes
        .get("operation")
        .expect("operation attribute");
    assert!(definition
        .values
        .contains(&AttributeValue::String("summary".into())));

    let usage = Usage {
        input_tokens: 100,
        cache_read_tokens: 50,
        cache_write_tokens: 50,
        cache_write_1h_tokens: 20,
        output_tokens: 10,
        reasoning_tokens: 3,
        total_tokens: 210,
    };
    let attributes = CompletionAttributes::usage(&usage).with_uncertainty(true);
    assert_eq!(attributes.cache_write_1h_tokens, Some(20));
    assert_eq!(attributes.cache_write_tokens, Some(50));
    assert_eq!(attributes.has_uncertain_usage, Some(true));
    assert_eq!(attributes.cache_hit_rate, Some(0.25));
    assert_eq!(cache_hit_rate(&Usage::default()), None);
    let encoded = serde_json::to_value(&attributes).unwrap();
    assert_eq!(encoded["cache_write_1h_tokens"], 20);
    assert_eq!(encoded["has_uncertain_usage"], true);
    // The typed start attributes round-trip through serde as plain data.
    let tool = ToolAttributes { name: "read".into() };
    assert_eq!(serde_json::to_value(&tool).unwrap()["name"], "read");
    let request_start = RequestAttributes {
        operation: ProviderOperation::Summary,
    };
    assert_eq!(
        serde_json::to_value(&request_start).unwrap()["operation"],
        "summary"
    );
}

#[test]
fn jsonl_observer_path_is_untouched_and_records_usage_uncertainty() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace.jsonl");
    let observer = TelemetryObserver::new(&path, TELEMETRY_SCHEMA).unwrap();
    let lines = std::fs::read_to_string(observer.path()).unwrap();
    let header: serde_json::Value = serde_json::from_str(lines.lines().next().unwrap()).unwrap();
    assert_eq!(header["record"], "header");
    assert_eq!(header["version"], TELEMETRY_SCHEMA);
    assert!(lines.contains('\n'));
}

#[tokio::test]
async fn inert_and_in_memory_spans_never_change_accounting_outcomes() {
    // The same business closure runs under the inert and recording adapters and
    // must return the identical accounting value either way.
    async fn account(context: &TelemetryContext, uncertain: bool) -> (u64, bool) {
        context
            .start_span(SpanOptions::new("accounting"), move |_span| async move {
                let tokens = 42u64;
                let flag = uncertain;
                Ok::<_, ()>((tokens, flag))
            })
            .await
            .unwrap()
    }
    assert_eq!(account(&NOOP_TELEMETRY_CONTEXT, true).await, (42, true));
    let fixture = InMemoryTelemetryContext::default();
    assert_eq!(account(&fixture.context(), true).await, (42, true));
    let spans: Vec<RecordedTelemetrySpan> = fixture.get_spans();
    SpanAssertions::new(&spans).assert_all_settled();
}

#[test]
fn usage_totals_include_tool_turns_and_summaries_and_keep_1h_distinct() {
    use octet_agent::telemetry::schema::UsageTotals;
    use octet_agent::{EntryId, UsageRecord, UsageRecordKind};

    let record = |kind: UsageRecordKind, usage: Usage| UsageRecord {
        kind,
        usage,
        stop_reason: None,
        endpoint: None,
        model: None,
        completed_at_unix_ms: None,
        cost: None,
        cost_microdollars: None,
        session_cost_microdollars: None,
        session_cost_picodollars_remainder: None,
    };

    // A tool-driving assistant turn, a compaction summary, a mirrored delegated
    // child and a terminal gate all contribute to the folded totals.
    let records = vec![
        record(
            UsageRecordKind::AssistantTurn {
                assistant: EntryId("a1".into()),
            },
            Usage {
                input_tokens: 100,
                cache_read_tokens: 50,
                cache_write_tokens: 30,
                cache_write_1h_tokens: 25,
                output_tokens: 20,
                reasoning_tokens: 5,
                total_tokens: 200,
            },
        ),
        record(
            UsageRecordKind::Compaction,
            Usage {
                input_tokens: 75,
                output_tokens: 10,
                total_tokens: 85,
                ..Usage::default()
            },
        ),
        record(
            UsageRecordKind::DelegatedAgent {
                agent_id: "child-1".into(),
                turn_count: 2,
                tool_call_count: 3,
            },
            Usage {
                total_tokens: 40,
                ..Usage::default()
            },
        ),
        record(
            UsageRecordKind::TerminalGate { returned: Some(true) },
            Usage {
                total_tokens: 5,
                ..Usage::default()
            },
        ),
    ];

    let totals = UsageTotals::from_records(&records);
    assert_eq!(totals.assistant_records, 1);
    assert_eq!(totals.summary_records, 1);
    assert_eq!(totals.delegated_records, 1);
    // Every provider operation is in the grand total, including child work.
    assert_eq!(totals.total_tokens, 200 + 85 + 40 + 5);
    // Own-context totals exclude the mirrored child record but keep summary and
    // tool-driving assistant usage.
    assert_eq!(totals.own_context_total_tokens, 200 + 85 + 5);
    // One-hour cache writes stay a distinct subset and are not folded in.
    assert_eq!(totals.cache_write_tokens, 30);
    assert_eq!(totals.cache_write_1h_tokens, 25);
    assert_eq!(totals.reasoning_tokens, 5);
    assert_eq!(totals.cache_hit_rate(), Some(50.0 / 255.0));
    assert_eq!(UsageTotals::default().cache_hit_rate(), None);
}
