//! Explicit callback-owned spans. No ambient parent, persistence or exporter.
//!
//! This is the vendor-neutral substrate for rows 3.1-3.4: a caller owns an
//! explicit [`TelemetryContext`], hands a callback a [`TelemetrySpan`], and the
//! callback's future determines the span lifetime. There is no process-global
//! current span and no exporter. Recording is passive: a misbehaving observer
//! never changes business behavior.
//!
//! Accounting is deliberately out of scope here. These spans observe; durable
//! usage, cost and uncertainty accounting stay in [`crate::session`] and the
//! `--telemetry` JSONL path in [`crate::telemetry`]. Dropping to
//! [`NOOP_TELEMETRY_CONTEXT`] loses observations, never accounting.

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    future::Future,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{Arc, Mutex},
};

/// One scalar or homogeneous-array span attribute value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AttributeValue {
    /// A text value.
    String(String),
    /// A finite numeric value.
    Number(f64),
    /// A boolean value.
    Boolean(bool),
    /// A homogeneous list of text values.
    Strings(Vec<String>),
    /// A homogeneous list of finite numeric values.
    Numbers(Vec<f64>),
    /// A homogeneous list of boolean values.
    Booleans(Vec<bool>),
}

/// Attribute payload attached to one span or event.
pub type SpanAttributes = BTreeMap<String, AttributeValue>;

/// Start options for one span: a stable name and optional attributes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpanOptions {
    /// Span name, normally a schema constant such as `octet.agent.turn`.
    pub name: String,
    /// Attributes known at span start.
    #[serde(default)]
    pub attributes: SpanAttributes,
}

impl SpanOptions {
    /// Creates options with the given name and no attributes.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            attributes: BTreeMap::new(),
        }
    }
}

/// Terminal status recorded for a span.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SpanStatus {
    /// The observed work completed without a recorded failure.
    Ok,
    /// The observed work failed or was explicitly marked as an error.
    Error,
}

/// Adapter-side lifecycle. Implementations must record passively; panics are
/// suppressed by the context. Business callbacks never run inside the adapter.
///
/// [`finish`](TelemetryBackend::finish) is adapter plumbing driven by the
/// callback settlement, not a public span operation.
pub trait TelemetryBackend: Send + Sync {
    /// Starts a span, returning its opaque id, or `None` to drop it.
    fn start(&self, parent: Option<u64>, options: SpanOptions) -> Option<u64>;
    /// Merges attributes into an existing unsettled span.
    fn set_attributes(&self, id: u64, attributes: SpanAttributes);
    /// Appends an event to an existing unsettled span.
    fn add_event(&self, id: u64, name: String, attributes: SpanAttributes);
    /// Records the terminal status of an existing unsettled span.
    fn set_status(&self, id: u64, status: SpanStatus);
    /// Settles a span; `failed` applies an automatic error when no explicit
    /// status was recorded. Called at most once per span.
    fn finish(&self, id: u64, failed: bool);
}

/// Explicit span factory. Cloning shares the backend and never creates an
/// ambient parent; only [`start_span`](TelemetryContext::start_span) nests.
#[derive(Clone, Default)]
pub struct TelemetryContext {
    backend: Option<Arc<dyn TelemetryBackend>>,
    parent: Option<Arc<SpanState>>,
}

/// Shared inert context: retains neither payloads nor callback spans.
pub const NOOP_TELEMETRY_CONTEXT: TelemetryContext = TelemetryContext {
    backend: None,
    parent: None,
};

impl std::fmt::Debug for TelemetryContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelemetryContext")
            .field("enabled", &self.backend.is_some())
            .finish()
    }
}

struct SpanState {
    id: u64,
    settled: Mutex<bool>,
}

/// A handle to the span owned by the current callback.
///
/// Calls are inert after settlement and inert entirely under
/// [`NOOP_TELEMETRY_CONTEXT`].
#[derive(Clone, Debug, Default)]
pub struct TelemetrySpan {
    context: TelemetryContext,
}

fn passive<T>(f: impl FnOnce() -> T) -> Option<T> {
    catch_unwind(AssertUnwindSafe(f)).ok()
}

impl TelemetryContext {
    /// Creates a context backed by an explicit adapter.
    pub fn new(backend: Arc<dyn TelemetryBackend>) -> Self {
        Self {
            backend: Some(backend),
            parent: None,
        }
    }

    /// Invokes the callback immediately and exactly once, then owns its future
    /// until settlement. Rust `Result` preserves the original error; dropping
    /// the future (including unwinding) settles as error, without error text.
    pub fn start_span<T, E, F: Future<Output = Result<T, E>>>(
        &self,
        options: SpanOptions,
        callback: impl FnOnce(TelemetrySpan) -> F,
    ) -> impl Future<Output = Result<T, E>> {
        let guard = self.begin(options);
        let future = callback(guard.span.clone());
        async move {
            let result = future.await;
            guard.finish(result.is_err());
            result
        }
    }

    /// Synchronous counterpart of [`start_span`](Self::start_span).
    pub fn start_span_sync<T, E>(
        &self,
        options: SpanOptions,
        callback: impl FnOnce(TelemetrySpan) -> Result<T, E>,
    ) -> Result<T, E> {
        let guard = self.begin(options);
        let result = callback(guard.span.clone());
        guard.finish(result.is_err());
        result
    }

    // Generator-driven agent operations need a scope guard because they yield
    // events to the caller. It is deliberately not part of the public API.
    pub(crate) fn begin(&self, options: SpanOptions) -> SpanGuard {
        let span = (|| {
            let backend = self.backend.as_ref()?;
            let parent_lock = self
                .parent
                .as_ref()
                .map(|p| p.settled.lock().unwrap_or_else(|e| e.into_inner()));
            if parent_lock.as_ref().is_some_and(|settled| **settled) {
                return None;
            }
            let id = passive(|| backend.start(self.parent.as_ref().map(|p| p.id), options))??;
            Some(TelemetrySpan {
                context: Self {
                    backend: Some(backend.clone()),
                    parent: Some(Arc::new(SpanState {
                        id,
                        settled: Mutex::new(false),
                    })),
                },
            })
        })()
        .unwrap_or_default();
        SpanGuard { span }
    }
}

impl TelemetrySpan {
    /// Returns a context whose children are nested under this span.
    pub fn context(&self) -> TelemetryContext {
        self.context.clone()
    }

    /// Starts a child span nested under this span.
    pub fn start_span<T, E, F: Future<Output = Result<T, E>>>(
        &self,
        options: SpanOptions,
        callback: impl FnOnce(TelemetrySpan) -> F,
    ) -> impl Future<Output = Result<T, E>> {
        self.context.start_span(options, callback)
    }

    fn record(&self, f: impl FnOnce(&dyn TelemetryBackend, u64)) {
        if let (Some(backend), Some(state)) = (&self.context.backend, &self.context.parent) {
            let settled = state.settled.lock().unwrap_or_else(|e| e.into_inner());
            if !*settled {
                passive(|| f(backend.as_ref(), state.id));
            }
        }
    }

    /// Merges attributes into this span.
    pub fn set_attributes(&self, attributes: SpanAttributes) {
        self.record(|b, id| b.set_attributes(id, attributes));
    }

    /// Appends a named event with attributes to this span.
    pub fn add_event(&self, name: impl Into<String>, attributes: SpanAttributes) {
        self.record(|b, id| b.add_event(id, name.into(), attributes));
    }

    /// Records the terminal status of this span.
    pub fn set_status(&self, status: SpanStatus) {
        self.record(|b, id| b.set_status(id, status));
    }
}

pub(crate) struct SpanGuard {
    pub(crate) span: TelemetrySpan,
}

impl SpanGuard {
    /// Context for children nested under this settled-on-drop guard.
    pub(crate) fn context(&self) -> TelemetryContext {
        self.span.context()
    }

    pub(crate) fn finish(self, failed: bool) {
        self.settle(failed);
    }

    fn settle(&self, failed: bool) {
        if let (Some(backend), Some(state)) =
            (&self.span.context.backend, &self.span.context.parent)
        {
            let mut settled = state.settled.lock().unwrap_or_else(|e| e.into_inner());
            if !*settled {
                *settled = true;
                passive(|| backend.finish(state.id, failed));
            }
        }
    }
}

impl Drop for SpanGuard {
    fn drop(&mut self) {
        self.settle(true);
    }
}

/// One recorded span event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecordedTelemetryEvent {
    /// Event name.
    pub name: String,
    /// Event attributes.
    pub attributes: SpanAttributes,
}

/// One recorded span snapshot returned by [`InMemoryTelemetryContext`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecordedTelemetrySpan {
    /// One-based span id in start order.
    pub id: u64,
    /// Parent span id when nested.
    pub parent_id: Option<u64>,
    /// Span name.
    pub name: String,
    /// Merged start and end attributes.
    pub attributes: SpanAttributes,
    /// Ordered events.
    pub events: Vec<RecordedTelemetryEvent>,
    /// Terminal status; `ok` until settled as error.
    pub status: SpanStatus,
    /// Whether the span has settled.
    pub settled: bool,
    /// One-based settlement order, present once settled.
    pub end_sequence: Option<u64>,
}

/// Hard bounds applied to retained spans, events and attribute payloads.
#[derive(Clone, Copy, Debug)]
pub struct TelemetryLimits {
    /// Maximum retained spans.
    pub spans: usize,
    /// Maximum events per span.
    pub events_per_span: usize,
    /// Maximum attributes per span or event.
    pub attributes_per_span: usize,
    /// Maximum serialized bytes for one attribute payload or name.
    pub payload_bytes: usize,
}

impl Default for TelemetryLimits {
    fn default() -> Self {
        Self {
            spans: 1024,
            events_per_span: 64,
            attributes_per_span: 64,
            payload_bytes: 8192,
        }
    }
}

#[derive(Default)]
struct MemoryState {
    spans: Vec<(RecordedTelemetrySpan, bool)>,
    end_sequence: u64,
    dropped: u64,
}

struct MemoryBackend {
    state: Mutex<MemoryState>,
    limits: TelemetryLimits,
}

impl MemoryBackend {
    fn bounded(&self, attributes: &SpanAttributes) -> bool {
        attributes.len() <= self.limits.attributes_per_span
            && attributes.values().all(|v| match v {
                AttributeValue::Number(n) => n.is_finite(),
                AttributeValue::Numbers(ns) => ns.iter().all(|n| n.is_finite()),
                _ => true,
            })
            && serde_json::to_vec(attributes).is_ok_and(|v| v.len() <= self.limits.payload_bytes)
    }

    fn mutate(&self, id: u64, f: impl FnOnce(&mut RecordedTelemetrySpan, &mut bool)) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((span, explicit)) = state.spans.get_mut(id.saturating_sub(1) as usize) {
            if !span.settled {
                f(span, explicit);
            }
        }
    }
}

impl TelemetryBackend for MemoryBackend {
    fn start(&self, parent_id: Option<u64>, options: SpanOptions) -> Option<u64> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.spans.len() >= self.limits.spans
            || options.name.len() > self.limits.payload_bytes
            || !self.bounded(&options.attributes)
        {
            state.dropped = state.dropped.saturating_add(1);
            return None;
        }
        let id = state.spans.len() as u64 + 1;
        state.spans.push((
            RecordedTelemetrySpan {
                id,
                parent_id,
                name: options.name,
                attributes: options.attributes,
                events: Vec::new(),
                status: SpanStatus::Ok,
                settled: false,
                end_sequence: None,
            },
            false,
        ));
        Some(id)
    }

    fn set_attributes(&self, id: u64, attributes: SpanAttributes) {
        self.mutate(id, |s, _| {
            let mut merged = s.attributes.clone();
            merged.extend(attributes);
            if self.bounded(&merged) {
                s.attributes = merged;
            }
        });
    }

    fn add_event(&self, id: u64, name: String, attributes: SpanAttributes) {
        self.mutate(id, |s, _| {
            if s.events.len() < self.limits.events_per_span
                && name.len() <= self.limits.payload_bytes
                && self.bounded(&attributes)
            {
                s.events.push(RecordedTelemetryEvent { name, attributes });
            }
        });
    }

    fn set_status(&self, id: u64, status: SpanStatus) {
        self.mutate(id, |s, explicit| {
            s.status = status;
            *explicit = true;
        });
    }

    fn finish(&self, id: u64, failed: bool) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.end_sequence += 1;
        let sequence = state.end_sequence;
        if let Some((s, explicit)) = state.spans.get_mut(id.saturating_sub(1) as usize) {
            if !s.settled {
                if failed && !*explicit {
                    s.status = SpanStatus::Error;
                }
                s.settled = true;
                s.end_sequence = Some(sequence);
            }
        }
    }
}

/// Bounded, process-local, deterministic recording. Snapshots are detached.
#[derive(Clone)]
pub struct InMemoryTelemetryContext {
    backend: Arc<MemoryBackend>,
}

impl Default for InMemoryTelemetryContext {
    fn default() -> Self {
        Self::new(TelemetryLimits::default())
    }
}

impl InMemoryTelemetryContext {
    /// Creates an isolated recorder with explicit bounds.
    pub fn new(limits: TelemetryLimits) -> Self {
        Self {
            backend: Arc::new(MemoryBackend {
                state: Mutex::new(MemoryState::default()),
                limits,
            }),
        }
    }

    /// Returns a root context that records into this instance.
    pub fn context(&self) -> TelemetryContext {
        TelemetryContext::new(self.backend.clone())
    }

    /// Returns detached snapshots in span-start order.
    pub fn get_spans(&self) -> Vec<RecordedTelemetrySpan> {
        self.backend
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .spans
            .iter()
            .map(|(s, _)| s.clone())
            .collect()
    }

    /// Returns the number of spans dropped because a bound was exceeded.
    pub fn dropped_spans(&self) -> u64 {
        self.backend
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .dropped
    }
}
