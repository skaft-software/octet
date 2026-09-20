//! Row 3.4: a runner-independent span assertion harness.
//!
//! The harness is deliberately small and has no dependency on a test framework
//! so both unit and integration tests can drive the same conformance cases.
//! Recording adapters (for example
//! [`InMemoryTelemetryContext`](super::spans::InMemoryTelemetryContext)) run the
//! full suite; the inert context is asserted separately to prove callbacks still
//! execute exactly once.

use std::future::Future;
use std::pin::Pin;

use super::spans::{
    AttributeValue, RecordedTelemetryEvent, RecordedTelemetrySpan, SpanAttributes, SpanStatus,
    TelemetryContext,
};

/// A fresh adapter instance and its detached span snapshots.
///
/// The trait is `Send + Sync` so shared references keep the boxed conformance
/// futures `Send`, matching how the agent drives these contexts.
pub trait TelemetryAdapterFixture: Send + Sync {
    /// The root context under test.
    fn context(&self) -> TelemetryContext;
    /// Detached snapshots in span-start order.
    fn get_spans(&self) -> Vec<RecordedTelemetrySpan>;
}

impl TelemetryAdapterFixture for super::spans::InMemoryTelemetryContext {
    fn context(&self) -> TelemetryContext {
        super::spans::InMemoryTelemetryContext::context(self)
    }

    fn get_spans(&self) -> Vec<RecordedTelemetrySpan> {
        super::spans::InMemoryTelemetryContext::get_spans(self)
    }
}

type CaseFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// One runner-independent conformance case for a recording adapter.
pub struct ConformanceCase {
    /// Coarse grouping for reporting.
    pub group: &'static str,
    /// Specific assertion name.
    pub name: &'static str,
    run: fn(&dyn TelemetryAdapterFixture) -> CaseFuture<'_>,
}

impl ConformanceCase {
    /// Runs the case against one fixture, panicking on assertion failure.
    pub async fn run(&self, fixture: &dyn TelemetryAdapterFixture) {
        (self.run)(fixture).await;
    }
}

/// Read-only assertions over a detached span snapshot.
pub struct SpanAssertions<'a> {
    spans: &'a [RecordedTelemetrySpan],
}

impl<'a> SpanAssertions<'a> {
    /// Wraps a snapshot for assertions.
    pub fn new(spans: &'a [RecordedTelemetrySpan]) -> Self {
        Self { spans }
    }

    /// Returns the single span with `name`, panicking when it is absent.
    pub fn find(&self, name: &str) -> &'a RecordedTelemetrySpan {
        self.spans
            .iter()
            .find(|span| span.name == name)
            .unwrap_or_else(|| panic!("expected a recorded span named {name}"))
    }

    /// Returns a span attribute value, when present.
    pub fn attr(&self, name: &str, key: &str) -> Option<&'a AttributeValue> {
        self.find(name).attributes.get(key)
    }

    /// Asserts the exact attribute payload of one span.
    pub fn assert_attributes(&self, name: &str, expected: SpanAttributes) {
        assert_eq!(self.find(name).attributes, expected, "attributes of {name}");
    }

    /// Asserts the exact ordered events of one span.
    pub fn assert_events(&self, name: &str, expected: Vec<RecordedTelemetryEvent>) {
        assert_eq!(self.find(name).events, expected, "events of {name}");
    }

    /// Asserts the terminal status of one span.
    pub fn assert_status(&self, name: &str, expected: SpanStatus) {
        assert_eq!(self.find(name).status, expected, "status of {name}");
    }

    /// Asserts that every recorded span has settled.
    pub fn assert_all_settled(&self) {
        for span in self.spans {
            assert!(span.settled, "span {} did not settle", span.name);
            assert!(
                span.end_sequence.is_some(),
                "span {} has no end sequence",
                span.name
            );
        }
    }

    /// Asserts a total span count.
    pub fn assert_count(&self, expected: usize) {
        assert_eq!(self.spans.len(), expected, "recorded span count");
    }

    /// Asserts that `child` is parented under `parent`.
    pub fn assert_parent(&self, child: &str, parent: &str) {
        assert_eq!(
            self.find(child).parent_id,
            Some(self.find(parent).id),
            "{child} parentage"
        );
    }
}

/// Convenience builder for one numeric attribute map.
pub fn numeric_attributes(pairs: &[(&str, f64)]) -> SpanAttributes {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), AttributeValue::Number(*value)))
        .collect()
}

/// Returns the full recording-adapter conformance suite.
pub fn conformance_cases() -> Vec<ConformanceCase> {
    vec![
        ConformanceCase {
            group: "callback lifecycle",
            name: "invokes the callback once and preserves the result",
            run: |fixture| {
                Box::pin(async move {
                    let mut calls = 0;
                    let result = fixture
                        .context()
                        .start_span(super::spans::SpanOptions::new("success"), |_span| {
                            calls += 1;
                            async { Ok::<_, ()>(42) }
                        })
                        .await;
                    assert_eq!(calls, 1, "callback must run exactly once");
                    assert_eq!(result, Ok(42));
                    let spans = fixture.get_spans();
                    let assertions = SpanAssertions::new(&spans);
                    assertions.assert_count(1);
                    assertions.assert_status("success", SpanStatus::Ok);
                    assertions.assert_all_settled();
                })
            },
        },
        ConformanceCase {
            group: "callback lifecycle",
            name: "preserves the original error and marks the span failed",
            run: |fixture| {
                Box::pin(async move {
                    let result: Result<(), &'static str> = fixture
                        .context()
                        .start_span(super::spans::SpanOptions::new("failure"), |_span| async {
                            Err("boom")
                        })
                        .await;
                    assert_eq!(result, Err("boom"));
                    let spans = fixture.get_spans();
                    SpanAssertions::new(&spans).assert_status("failure", SpanStatus::Error);
                })
            },
        },
        ConformanceCase {
            group: "status",
            name: "keeps an explicit status without automatic overwrite",
            run: |fixture| {
                Box::pin(async move {
                    let _: Result<(), ()> = fixture
                        .context()
                        .start_span(super::spans::SpanOptions::new("explicit"), |span| {
                            span.set_status(SpanStatus::Ok);
                            async { Err(()) }
                        })
                        .await;
                    let spans = fixture.get_spans();
                    SpanAssertions::new(&spans).assert_status("explicit", SpanStatus::Ok);
                })
            },
        },
        ConformanceCase {
            group: "recording",
            name: "merges attributes and records ordered events",
            run: |fixture| {
                Box::pin(async move {
                    let options = super::spans::SpanOptions {
                        name: "recording".into(),
                        attributes: numeric_attributes(&[("start", 1.0), ("overwrite", 2.0)]),
                    };
                    let _: Result<(), ()> = fixture
                        .context()
                        .start_span(options, |span| {
                            span.set_attributes(numeric_attributes(&[("extra", 3.0)]));
                            span.add_event("first", numeric_attributes(&[("index", 1.0)]));
                            span.add_event("second", numeric_attributes(&[("index", 2.0)]));
                            async { Ok(()) }
                        })
                        .await;
                    let spans = fixture.get_spans();
                    let assertions = SpanAssertions::new(&spans);
                    assertions.assert_attributes(
                        "recording",
                        numeric_attributes(&[("start", 1.0), ("overwrite", 2.0), ("extra", 3.0)]),
                    );
                    assert_eq!(assertions.find("recording").events.len(), 2);
                    assert_eq!(assertions.find("recording").events[0].name, "first");
                    assert_eq!(assertions.find("recording").events[1].name, "second");
                })
            },
        },
        ConformanceCase {
            group: "recording",
            name: "makes calls after settlement inert",
            run: |fixture| {
                Box::pin(async move {
                    let handle = std::sync::Arc::new(std::sync::Mutex::new(None));
                    let captured = handle.clone();
                    let _: Result<(), ()> = fixture
                        .context()
                        .start_span(super::spans::SpanOptions::new("settled"), |span| {
                            *captured.lock().unwrap() = Some(span);
                            async { Ok(()) }
                        })
                        .await;
                    let span = handle.lock().unwrap().clone().expect("captured span");
                    span.set_attributes(numeric_attributes(&[("late", 9.0)]));
                    span.add_event("late", SpanAttributes::new());
                    span.set_status(SpanStatus::Error);
                    let child: Result<(), ()> = span
                        .start_span(
                            super::spans::SpanOptions::new("late-child"),
                            |_child| async { Ok(()) },
                        )
                        .await;
                    assert_eq!(child, Ok(()));
                    let spans = fixture.get_spans();
                    let assertions = SpanAssertions::new(&spans);
                    assertions.assert_count(1);
                    assertions.assert_attributes("settled", SpanAttributes::new());
                    assertions.assert_events("settled", vec![]);
                    assertions.assert_status("settled", SpanStatus::Ok);
                })
            },
        },
        ConformanceCase {
            group: "parentage",
            name: "records nested and concurrent child relationships",
            run: |fixture| {
                Box::pin(async move {
                    let _: Result<(), ()> = fixture
                        .context()
                        .start_span(
                            super::spans::SpanOptions::new("parent"),
                            |parent| async move {
                                let first = parent.start_span(
                                    super::spans::SpanOptions::new("first-child"),
                                    |_c| async { Ok::<_, ()>(()) },
                                );
                                let second: Result<(), ()> = parent
                                    .start_span(
                                        super::spans::SpanOptions::new("second-child"),
                                        |_c| async { Ok::<_, ()>(()) },
                                    )
                                    .await;
                                second?;
                                first.await?;
                                Ok(())
                            },
                        )
                        .await;
                    let spans = fixture.get_spans();
                    let assertions = SpanAssertions::new(&spans);
                    assert_eq!(assertions.find("parent").parent_id, None);
                    assertions.assert_parent("first-child", "parent");
                    assertions.assert_parent("second-child", "parent");
                    assertions.assert_all_settled();
                    let second_end = assertions.find("second-child").end_sequence.unwrap();
                    let first_end = assertions.find("first-child").end_sequence.unwrap();
                    let parent_end = assertions.find("parent").end_sequence.unwrap();
                    assert!(second_end < first_end && first_end < parent_end);
                })
            },
        },
    ]
}
