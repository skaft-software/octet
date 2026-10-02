//! Serializable schema metadata and compile-time scoped instrumentation.
//!
//! Row 3.3: typed span names, start attributes and completion attributes are
//! plain serializable data ([`TelemetrySchema`]) plus compile-time marker types
//! ([`SpanSchema`]). Row 3.6 lives in [`CompletionAttributes`]: reported usage
//! buckets stay disjoint, one-hour cache writes remain distinct from total
//! cache writes, and the uncertainty flag is carried through rather than
//! converted into fabricated zero usage.

use std::{collections::BTreeMap, future::Future, marker::PhantomData};

use serde::{Deserialize, Serialize};

use super::spans::*;

/// Declared attribute value shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributeType {
    /// A single text value.
    String,
    /// A single numeric value.
    Number,
    /// A single boolean value.
    Boolean,
    /// A list of text values.
    Strings,
    /// A list of numeric values.
    Numbers,
    /// A list of boolean values.
    Booleans,
}

/// Metadata for one declared start, end or event attribute.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttributeDefinition {
    /// Human-readable description.
    pub description: String,
    /// Declared value shape.
    pub value_type: AttributeType,
    /// Whether the attribute must be present.
    pub required: bool,
    /// Whether the attribute may contain sensitive operational data.
    pub sensitive: bool,
    /// Enumerated allowed scalar values, when bounded.
    pub values: Vec<AttributeValue>,
    /// Enumerated allowed element values for array attributes.
    pub element_values: Vec<AttributeValue>,
}

/// Declared parent relationships for a span.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParentDefinition {
    /// Any parent, including a root.
    Any,
    /// A root or a caller-owned external span.
    RootOrExternal,
    /// Exactly one of the listed span names.
    Spans {
        /// Allowed parent span names.
        spans: Vec<String>,
    },
}

/// Serializable definition of one span.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpanDefinition {
    /// Human-readable description.
    pub description: String,
    /// Declared parents.
    pub parents: ParentDefinition,
    /// Start attributes keyed by name.
    pub start_attributes: BTreeMap<String, AttributeDefinition>,
    /// Completion attributes keyed by name.
    pub end_attributes: BTreeMap<String, AttributeDefinition>,
    /// Events keyed by name, then attribute name.
    pub events: BTreeMap<String, BTreeMap<String, AttributeDefinition>>,
}

/// Serializable telemetry schema for one version.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TelemetrySchema {
    /// Schema version.
    pub version: u32,
    /// Span definitions keyed by span name.
    pub spans: BTreeMap<String, SpanDefinition>,
}

/// Compile-time bridge between a typed span and its serializable schema entry.
pub trait SpanSchema {
    /// Schema span name.
    const NAME: &'static str;
    /// Serializable start-attribute type.
    type Start: Serialize;
    /// Serializable completion-attribute type.
    type End: Serialize;
}

/// A typed span handle that rejects null completion attributes.
pub struct TypedTelemetrySpan<S: SpanSchema> {
    span: TelemetrySpan,
    schema: PhantomData<S>,
}

impl<S: SpanSchema> TypedTelemetrySpan<S> {
    /// Returns a context whose children are nested under this span.
    pub fn context(&self) -> TelemetryContext {
        self.span.context()
    }

    /// Merges typed completion attributes into this span.
    pub fn set_attributes(&self, attributes: S::End) {
        if let Some(attributes) = attributes_of(&attributes) {
            self.span.set_attributes(attributes);
        }
    }

    /// Records the terminal status of this span.
    pub fn set_status(&self, status: SpanStatus) {
        self.span.set_status(status);
    }
}

fn attributes_of(value: &impl Serialize) -> Option<SpanAttributes> {
    let serde_json::Value::Object(values) = serde_json::to_value(value).ok()? else {
        return None;
    };
    values
        .into_iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| serde_json::from_value(v).ok().map(|v| (k, v)))
        .collect()
}

impl TelemetryContext {
    /// Runs `callback` inside a typed span, returning its future unchanged.
    pub fn start_typed<S: SpanSchema, T, E, F: Future<Output = Result<T, E>>>(
        &self,
        start: S::Start,
        callback: impl FnOnce(TypedTelemetrySpan<S>) -> F,
    ) -> impl Future<Output = Result<T, E>> {
        let options = SpanOptions {
            name: S::NAME.into(),
            attributes: attributes_of(&start).unwrap_or_default(),
        };
        self.start_span(options, |span| {
            callback(TypedTelemetrySpan {
                span,
                schema: PhantomData,
            })
        })
    }

    /// Generator-driven boundary hook: opens the typed span for a run, turn,
    /// provider, tool, compaction, or summary boundary.
    pub(crate) fn begin_typed<S: SpanSchema>(&self, start: S::Start) -> super::spans::SpanGuard {
        self.begin(SpanOptions {
            name: S::NAME.into(),
            attributes: attributes_of(&start).unwrap_or_default(),
        })
    }
}

/// No start attributes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmptyAttributes {}

/// Start attributes for one provider request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestAttributes {
    /// Logical provider operation.
    pub operation: ProviderOperation,
}

/// Kind of provider operation a request span covers.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderOperation {
    /// One assistant turn.
    Assistant,
    /// One compaction summary.
    Summary,
    /// One native Responses compaction.
    NativeCompaction,
    /// One terminal-gate decision.
    TerminalGate,
}

/// Start attributes for one tool execution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolAttributes {
    /// Registered tool name, never its arguments.
    pub name: String,
}

/// Start attributes for one deferred-run lifecycle boundary.
///
/// The typed deferred stop reason (or the durable decision that replaced it)
/// reaches telemetry here: `stop_reason` is the normalized reason the provider
/// reported, while `phase` is the durable leaf state the decision produced.
/// Only host-selected identities and the provider's non-secret handle shape are
/// recorded; the opaque handle payload is hashed out of telemetry entirely.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeferredRunAttributes {
    /// Durable operation identity of the parked request.
    pub operation_id: String,
    /// Normalized provider stop reason that produced this boundary.
    pub stop_reason: String,
    /// Durable leaf state after this boundary.
    pub phase: String,
    /// Poll number at this boundary.
    pub poll: u64,
    /// Durable generation at this boundary.
    pub generation: u64,
    /// Whether the poll replaced an unknown-outcome `effect_pending` leaf.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recovery: bool,
    /// Number of redacted response diagnostics observed with the boundary.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub diagnostics: usize,
}

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

/// Optional completion attributes capturing reported provider usage.
///
/// Buckets stay disjoint: `cache_write_1h_tokens` is a subset of
/// `cache_write_tokens` rather than an addition, and `reasoning_tokens` is a
/// subset of `output_tokens`. `has_uncertain_usage` records that the totals are
/// a known subtotal, so an observer never reads a fabricated zero.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionAttributes {
    /// Reported uncached input tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Reported output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Reported cache-read tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    /// Reported total cache-write tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    /// Reported one-hour cache-write tokens (subset of cache writes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_1h_tokens: Option<u64>,
    /// Reported reasoning tokens (subset of output).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    /// Reported total tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    /// Cache-hit fraction over reported prompt traffic.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_hit_rate: Option<f64>,
    /// Whether the recorded usage is a known subtotal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_uncertain_usage: Option<bool>,
    /// Client clock origin, not a server clock.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inference_scope: Option<octet_ai::inference::ClientTimingScope>,
    /// Client origin to canonical completion, before persistence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_elapsed_ns: Option<u64>,
    /// First nonempty canonical output offset, not a token timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_first_output_ns: Option<u64>,
    /// First answer-text offset, independent of reasoning.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_first_text_ns: Option<u64>,
    /// First streamed reasoning offset, not hidden thinking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_first_reasoning_ns: Option<u64>,
    /// First nonempty tool-argument offset before tool admission.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_first_tool_arguments_ns: Option<u64>,
    /// Nonempty canonical output events, never token count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_output_events: Option<u64>,
    /// Last observed output to canonical completion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_completion_tail_ns: Option<u64>,
    /// Native server generation counter, independent of billing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_generation_tokens: Option<u64>,
    /// Matching native server generation duration, in nanoseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_generation_ns: Option<u64>,
    /// Recognized server count/duration wire contract.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_timing_source: Option<octet_ai::inference::ServerTimingSource>,
    /// Explicit reason the server rate is unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_timing_unavailable: Option<octet_ai::inference::ServerTimingUnavailable>,
    /// Client-derived decode estimate, not native server accounting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decode_estimated_tokens_per_second: Option<f64>,
    /// Pair-slope dispersion; not a probability of GPU accuracy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decode_relative_dispersion: Option<f64>,
    /// Explicit failure of client evidence, with no E2E fallback.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decode_estimate_unavailable: Option<octet_ai::DecodeEstimateUnavailable>,
}

impl CompletionAttributes {
    /// Builds completion attributes from provider-reported usage.
    pub fn usage(usage: &octet_ai::Usage) -> Self {
        Self {
            input_tokens: Some(usage.input_tokens),
            output_tokens: Some(usage.output_tokens),
            cache_read_tokens: Some(usage.cache_read_tokens),
            cache_write_tokens: Some(usage.cache_write_tokens),
            cache_write_1h_tokens: Some(usage.cache_write_1h_tokens),
            reasoning_tokens: Some(usage.reasoning_tokens),
            total_tokens: Some(usage.total_tokens),
            cache_hit_rate: cache_hit_rate(usage),
            has_uncertain_usage: None,
            ..Self::default()
        }
    }

    /// Marks whether the recorded usage is a known subtotal.
    pub fn with_uncertainty(mut self, uncertain: bool) -> Self {
        self.has_uncertain_usage = Some(uncertain);
        self
    }

    /// Adds independent frozen client/server inference observations. This never
    /// replaces usage buckets or derives server timing from client offsets.
    pub fn with_inference(
        mut self,
        metrics: Option<&octet_ai::inference::InferenceMetrics>,
    ) -> Self {
        if let Some(metrics) = metrics {
            if let Some(client) = &metrics.client {
                self.inference_scope = client.scope;
                self.client_elapsed_ns = Some(client.elapsed_ns);
                self.client_first_output_ns = client.first_output_ns;
                self.client_first_text_ns = client.first_text_ns;
                self.client_first_reasoning_ns = client.first_reasoning_ns;
                self.client_first_tool_arguments_ns = client.first_tool_arguments_ns;
                self.client_output_events = Some(client.output_events);
                self.client_completion_tail_ns = client.completion_tail_ns();
            }
            if let Some(server) = &metrics.server {
                self.server_generation_tokens = Some(server.tokens);
                self.server_generation_ns = Some(server.generation_ns);
                self.server_timing_source = Some(server.source);
            }
            self.server_timing_unavailable = metrics.server_unavailable;
            if let Some(estimate) = &metrics.decode_estimate {
                self.decode_estimated_tokens_per_second = Some(estimate.tokens_per_second);
                self.decode_relative_dispersion = Some(estimate.relative_dispersion);
            }
            self.decode_estimate_unavailable = metrics.decode_unavailable;
        }
        self
    }

    /// Adapter-style completion recording used to settle a boundary span.
    pub(crate) fn record(self, span: &TelemetrySpan) {
        if let Some(attributes) = attributes_of(&self) {
            span.set_attributes(attributes);
        }
    }
}

/// Fraction of reported prompt tokens served from cache. Missing/empty prompt
/// traffic is unavailable, not a fabricated zero-rate observation. 1h writes
/// are a subset of writes, and reasoning a subset of output; neither is added.
pub fn cache_hit_rate(usage: &octet_ai::Usage) -> Option<f64> {
    let input = usage.input_tokens as f64
        + usage.cache_read_tokens as f64
        + usage.cache_write_tokens as f64;
    (input > 0.0).then(|| usage.cache_read_tokens as f64 / input)
}

/// Folded usage totals across durable [`UsageRecord`]s.
///
/// Row 3.6: assistant turns that drove tools, compaction summaries and every
/// other provider operation contribute to the same totals. Buckets stay
/// disjoint, so one-hour cache writes are reported distinctly and never added
/// into total cache writes. `own_context_total_tokens` mirrors the reservation
/// view: mirrored delegated-child usage is a separate total rather than being
/// counted twice in the root's own context.
///
/// [`UsageRecord`]: crate::session::UsageRecord
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UsageTotals {
    /// Uncached input tokens across all operations.
    pub input_tokens: u64,
    /// Cache-read tokens.
    pub cache_read_tokens: u64,
    /// Total cache-write tokens.
    pub cache_write_tokens: u64,
    /// One-hour cache-write tokens, a distinct subset of cache writes.
    pub cache_write_1h_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Reasoning tokens, a subset of output.
    pub reasoning_tokens: u64,
    /// Total tokens across every operation, including mirrored child work.
    pub total_tokens: u64,
    /// Total tokens excluding mirrored delegated-child records.
    pub own_context_total_tokens: u64,
    /// Number of assistant-turn records folded.
    pub assistant_records: u64,
    /// Number of compaction-summary records folded.
    pub summary_records: u64,
    /// Number of mirrored delegated-child records folded.
    pub delegated_records: u64,
}

impl UsageTotals {
    /// Folds every record; no operation kind is excluded from the totals.
    pub fn from_records(records: &[crate::session::UsageRecord]) -> Self {
        use crate::session::UsageRecordKind;
        let mut totals = Self::default();
        for record in records {
            let usage = &record.usage;
            totals.input_tokens = totals.input_tokens.saturating_add(usage.input_tokens);
            totals.cache_read_tokens = totals
                .cache_read_tokens
                .saturating_add(usage.cache_read_tokens);
            totals.cache_write_tokens = totals
                .cache_write_tokens
                .saturating_add(usage.cache_write_tokens);
            totals.cache_write_1h_tokens = totals
                .cache_write_1h_tokens
                .saturating_add(usage.cache_write_1h_tokens);
            totals.output_tokens = totals.output_tokens.saturating_add(usage.output_tokens);
            totals.reasoning_tokens = totals
                .reasoning_tokens
                .saturating_add(usage.reasoning_tokens);
            let tokens = if usage.total_tokens > 0 {
                usage.total_tokens
            } else {
                usage
                    .input_tokens
                    .saturating_add(usage.cache_read_tokens)
                    .saturating_add(usage.cache_write_tokens)
                    .saturating_add(usage.output_tokens)
            };
            totals.total_tokens = totals.total_tokens.saturating_add(tokens);
            match &record.kind {
                UsageRecordKind::AssistantTurn { .. } => {
                    totals.assistant_records = totals.assistant_records.saturating_add(1);
                    totals.own_context_total_tokens =
                        totals.own_context_total_tokens.saturating_add(tokens);
                }
                UsageRecordKind::Compaction => {
                    totals.summary_records = totals.summary_records.saturating_add(1);
                    totals.own_context_total_tokens =
                        totals.own_context_total_tokens.saturating_add(tokens);
                }
                UsageRecordKind::DelegatedAgent { .. } => {
                    totals.delegated_records = totals.delegated_records.saturating_add(1);
                }
                UsageRecordKind::CacheWarm
                | UsageRecordKind::RejectedResponsesTurn
                | UsageRecordKind::TerminalGate { .. } => {
                    totals.own_context_total_tokens =
                        totals.own_context_total_tokens.saturating_add(tokens);
                }
            }
        }
        totals
    }

    /// Cache-hit fraction over these totals, or `None` without prompt traffic.
    pub fn cache_hit_rate(&self) -> Option<f64> {
        let input = self.input_tokens as f64
            + self.cache_read_tokens as f64
            + self.cache_write_tokens as f64;
        (input > 0.0).then(|| self.cache_read_tokens as f64 / input)
    }
}

macro_rules! schema {
    ($($(#[$meta:meta])* $ty:ident, $name:literal, $start:ty;)+) => {
        $(
            $(#[$meta])*
            #[doc = concat!("Typed span marker for `", $name, "`.")]
            pub struct $ty;
            impl SpanSchema for $ty {
                const NAME: &'static str = $name;
                type Start = $start;
                type End = CompletionAttributes;
            }
        )+
    };
}

schema! {
    /// One admitted agent run.
    RunSpan, "octet.agent.run", EmptyAttributes;
    /// One assistant response and its tool batch.
    TurnSpan, "octet.agent.turn", EmptyAttributes;
    /// One logical request to a provider.
    ProviderRequestSpan, "octet.ai.request", RequestAttributes;
    /// One streaming provider response.
    ProviderStreamSpan, "octet.ai.stream", EmptyAttributes;
    /// One raw tool execution.
    ToolSpan, "octet.agent.tool", ToolAttributes;
    /// One context compaction.
    CompactionSpan, "octet.agent.compaction", EmptyAttributes;
    /// One compaction summary request.
    SummarySpan, "octet.agent.summary", EmptyAttributes;
    /// One delegated child run.
    DelegationSpan, "octet.agent.delegation", EmptyAttributes;
    /// One deferred suspend/poll boundary for a parked provider request.
    DeferredRunSpan, "octet.agent.deferred_run", DeferredRunAttributes;
}

/// The serializable schema for every agent and provider span boundary.
pub fn agent_telemetry_schema() -> TelemetrySchema {
    let attribute = |value_type, required, description: &str| AttributeDefinition {
        description: description.into(),
        value_type,
        required,
        sensitive: false,
        values: Vec::new(),
        element_values: Vec::new(),
    };
    let mut end_attributes = BTreeMap::new();
    for name in [
        "input_tokens",
        "output_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "cache_write_1h_tokens",
        "reasoning_tokens",
        "total_tokens",
        "cache_hit_rate",
    ] {
        end_attributes.insert(
            name.into(),
            attribute(
                AttributeType::Number,
                false,
                "Reported usage; subset buckets are not added twice",
            ),
        );
    }
    end_attributes.insert(
        "has_uncertain_usage".into(),
        attribute(
            AttributeType::Boolean,
            false,
            "Known usage is only a subtotal",
        ),
    );
    end_attributes.insert(
        "inference_scope".into(),
        attribute(
            AttributeType::String,
            false,
            "Client clock origin, not a server clock.",
        ),
    );
    end_attributes.insert(
        "client_elapsed_ns".into(),
        attribute(
            AttributeType::Number,
            false,
            "Client origin to canonical completion, before persistence.",
        ),
    );
    end_attributes.insert(
        "client_first_output_ns".into(),
        attribute(
            AttributeType::Number,
            false,
            "First nonempty canonical output offset, not a token timestamp.",
        ),
    );
    end_attributes.insert(
        "client_first_text_ns".into(),
        attribute(
            AttributeType::Number,
            false,
            "First answer-text offset, independent of reasoning.",
        ),
    );
    end_attributes.insert(
        "client_first_reasoning_ns".into(),
        attribute(
            AttributeType::Number,
            false,
            "First streamed reasoning offset, not hidden thinking.",
        ),
    );
    end_attributes.insert(
        "client_first_tool_arguments_ns".into(),
        attribute(
            AttributeType::Number,
            false,
            "First nonempty tool-argument offset before tool admission.",
        ),
    );
    end_attributes.insert(
        "client_output_events".into(),
        attribute(
            AttributeType::Number,
            false,
            "Nonempty canonical output events, never token count.",
        ),
    );
    end_attributes.insert(
        "client_completion_tail_ns".into(),
        attribute(
            AttributeType::Number,
            false,
            "Last observed output to canonical completion.",
        ),
    );
    end_attributes.insert(
        "server_generation_tokens".into(),
        attribute(
            AttributeType::Number,
            false,
            "Native server generation counter, independent of billing.",
        ),
    );
    end_attributes.insert(
        "server_generation_ns".into(),
        attribute(
            AttributeType::Number,
            false,
            "Matching native server generation duration, in nanoseconds.",
        ),
    );
    end_attributes.insert(
        "server_timing_source".into(),
        attribute(
            AttributeType::String,
            false,
            "Recognized server count/duration wire contract.",
        ),
    );
    end_attributes.insert(
        "server_timing_unavailable".into(),
        attribute(
            AttributeType::String,
            false,
            "Explicit reason the server rate is unavailable.",
        ),
    );
    for (name, kind, description) in [
        (
            "decode_estimated_tokens_per_second",
            AttributeType::Number,
            "Robust client-stream decode estimate, not E2E or native server timing.",
        ),
        (
            "decode_relative_dispersion",
            AttributeType::Number,
            "Pair-slope dispersion, not GPU accuracy confidence.",
        ),
        (
            "decode_estimate_unavailable",
            AttributeType::String,
            "Why client evidence could not resolve decode cadence.",
        ),
    ] {
        end_attributes.insert(name.into(), attribute(kind, false, description));
    }
    let mut spans = BTreeMap::new();
    for name in [
        RunSpan::NAME,
        TurnSpan::NAME,
        ProviderRequestSpan::NAME,
        ProviderStreamSpan::NAME,
        ToolSpan::NAME,
        CompactionSpan::NAME,
        SummarySpan::NAME,
        DelegationSpan::NAME,
        DeferredRunSpan::NAME,
    ] {
        let mut start_attributes = BTreeMap::new();
        if name == ProviderRequestSpan::NAME {
            let mut operation = attribute(AttributeType::String, true, "Provider operation");
            operation.values = ["assistant", "summary", "native_compaction", "terminal_gate"]
                .map(|s| AttributeValue::String(s.into()))
                .into();
            start_attributes.insert("operation".into(), operation);
        }
        if name == ToolSpan::NAME {
            start_attributes.insert(
                "name".into(),
                attribute(
                    AttributeType::String,
                    true,
                    "Registered tool name, never arguments",
                ),
            );
        }
        if name == DeferredRunSpan::NAME {
            start_attributes.insert(
                "operation_id".into(),
                attribute(
                    AttributeType::String,
                    true,
                    "Durable deferred-run operation identity",
                ),
            );
            let mut stop_reason = attribute(
                AttributeType::String,
                true,
                "Normalized provider stop reason",
            );
            stop_reason.values = [
                "deferred", "settled", "failed", "aborted", "waiting", "refused",
            ]
            .map(|s| AttributeValue::String(s.into()))
            .into();
            start_attributes.insert("stop_reason".into(), stop_reason);
            let mut phase = attribute(AttributeType::String, true, "Durable leaf state");
            phase.values = [
                "suspended",
                "effect_pending",
                "settled",
                "cancelled",
                "failed",
            ]
            .map(|s| AttributeValue::String(s.into()))
            .into();
            start_attributes.insert("phase".into(), phase);
            for (field, description) in [
                ("poll", "Poll number at this boundary"),
                ("generation", "Durable generation at this boundary"),
            ] {
                start_attributes.insert(
                    field.into(),
                    attribute(AttributeType::Number, true, description),
                );
            }
            start_attributes.insert(
                "recovery".into(),
                attribute(
                    AttributeType::Boolean,
                    false,
                    "The poll replaced an unknown-outcome leaf",
                ),
            );
            start_attributes.insert(
                "diagnostics".into(),
                attribute(
                    AttributeType::Number,
                    false,
                    "Redacted response diagnostics observed at this boundary",
                ),
            );
        }
        spans.insert(
            name.into(),
            SpanDefinition {
                description: name.into(),
                parents: ParentDefinition::Any,
                start_attributes,
                end_attributes: end_attributes.clone(),
                events: BTreeMap::new(),
            },
        );
    }
    TelemetrySchema { version: 1, spans }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::spans::{InMemoryTelemetryContext, SpanStatus};

    /// Exercises the generator-driven scope-guard path: a typed run guard owns a
    /// child turn guard, completion usage is recorded through the typed span, and
    /// both settle in the correct order with the correct parentage.
    #[test]
    fn generator_scope_guard_nests_typed_spans_and_records_completion_usage() {
        let fixture = InMemoryTelemetryContext::default();
        let context = fixture.context();
        let run_guard = context.begin_typed::<RunSpan>(EmptyAttributes {});
        let run_context = run_guard.context();
        let turn_guard = run_context.begin_typed::<TurnSpan>(EmptyAttributes {});
        let usage = octet_ai::Usage {
            input_tokens: 7,
            output_tokens: 3,
            cache_read_tokens: 2,
            cache_write_1h_tokens: 1,
            total_tokens: 12,
            ..octet_ai::Usage::default()
        };
        CompletionAttributes::usage(&usage).record(&turn_guard.span);
        turn_guard.finish(false);
        run_guard.finish(false);

        let spans = fixture.get_spans();
        assert_eq!(spans.len(), 2);
        let run = spans.iter().find(|s| s.name == RunSpan::NAME).unwrap();
        let turn = spans.iter().find(|s| s.name == TurnSpan::NAME).unwrap();
        assert_eq!(run.parent_id, None);
        assert_eq!(turn.parent_id, Some(run.id));
        assert_eq!(run.status, SpanStatus::Ok);
        assert_eq!(turn.status, SpanStatus::Ok);
        assert_eq!(
            turn.attributes.get("cache_write_1h_tokens"),
            Some(&AttributeValue::Number(1.0))
        );
        assert_eq!(
            turn.attributes.get("input_tokens"),
            Some(&AttributeValue::Number(7.0))
        );
        assert!(turn.end_sequence.unwrap() < run.end_sequence.unwrap());
    }

    /// A generator that abandons its scope guard must settle the span as an
    /// error rather than leaking an unsettled span.
    #[test]
    fn dropped_scope_guard_settles_as_error() {
        let fixture = InMemoryTelemetryContext::default();
        {
            let _guard = fixture
                .context()
                .begin_typed::<SummarySpan>(EmptyAttributes {});
        }
        let spans = fixture.get_spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].status, SpanStatus::Error);
        assert!(spans[0].settled);
    }
}

#[cfg(test)]
mod inference_tests {
    use super::*;
    #[test]
    fn inference_span_attributes_do_not_overwrite_accounting() {
        let metrics = octet_ai::InferenceMetrics {
            client: Some(octet_ai::ClientInferenceMetrics {
                scope: Some(octet_ai::ClientTimingScope::Request),
                elapsed_ns: 2_000_000_000,
                first_output_ns: Some(100_000_000),
                ..Default::default()
            }),
            server: Some(octet_ai::ServerGenerationMetrics {
                source: octet_ai::ServerTimingSource::TimingsPredicted,
                tokens: 100,
                generation_ns: 500_000_000,
                reported_unit: octet_ai::ReportedTimingUnit::Milliseconds,
                prompt_ns: None,
                queue_ns: None,
                total_ns: None,
            }),
            server_unavailable: None,
            ..Default::default()
        };
        let usage = octet_ai::Usage {
            output_tokens: 150,
            reasoning_tokens: 50,
            ..Default::default()
        };
        let attrs =
            attributes_of(&CompletionAttributes::usage(&usage).with_inference(Some(&metrics)))
                .unwrap();
        assert_eq!(attrs["output_tokens"], AttributeValue::Number(150.0));
        assert_eq!(
            attrs["server_generation_tokens"],
            AttributeValue::Number(100.0)
        );
        assert_eq!(
            attrs["client_elapsed_ns"],
            AttributeValue::Number(2_000_000_000.0)
        );
        assert_eq!(
            attrs["server_timing_source"],
            AttributeValue::String("timings_predicted".into())
        );
        let schema = agent_telemetry_schema();
        for key in attrs.keys() {
            assert!(schema.spans[ProviderRequestSpan::NAME]
                .end_attributes
                .contains_key(key));
        }
    }
}
