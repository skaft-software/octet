//! Stateful event-stream assembly, state machine invariants, and guards.

use crate::error::{AiError, DecodeError, Diagnostic, StreamProtocolError};
use crate::pricing::Pricing;
use crate::types::{
    AssistantMessage, AssistantPart, Media, ModelId, Protocol, ProviderPartMetadata, ReasoningPart,
    ReasoningState, Response, StopReason, ToolArgumentValidation, ToolCall, ToolCallArgumentError,
    ToolCallId, ToolDef, Usage,
};
use std::collections::{HashMap, HashSet};

use serde::Serialize;

/// A bounded advisory state reported by an opt-in OpenAI-compatible endpoint
/// while it prepares a cold model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderLifecycleState {
    /// The endpoint accepted the request but has not started loading it.
    Queued,
    /// The endpoint is loading or initializing the requested model.
    Loading,
    /// The endpoint is ready to generate the requested response.
    Ready,
}

impl ProviderLifecycleState {
    /// Stable lowercase wire and serialization value for this state.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Loading => "loading",
            Self::Ready => "ready",
        }
    }

    pub(crate) fn from_wire(value: &str) -> Option<Self> {
        match value.trim() {
            "queued" => Some(Self::Queued),
            "loading" => Some(Self::Loading),
            "ready" => Some(Self::Ready),
            _ => None,
        }
    }
}

/// Sanitized, non-semantic lifecycle feedback from an opt-in provider.
///
/// This advisory value is never included in an assembled [`Response`] and is
/// not suitable for replay or persistence. `detail`, when present, has already
/// crossed the client transport sanitization and byte bound.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProviderLifecycle {
    /// Endpoint-reported preparation state.
    pub state: ProviderLifecycleState,
    /// Optional bounded status detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Hard cap on accumulated tool-call argument bytes before assembly (design §20).
/// Crossing it is a [`DecodeError::ToolArgumentsTooLarge`], never a panic.
pub(crate) const MAX_TOOL_ARGUMENT_BYTES: usize = 16 * 1024 * 1024;
/// Absolute cap across streamed text, reasoning, tool arguments, and media.
pub(crate) const MAX_RESPONSE_CONTENT_BYTES: usize = 64 * 1024 * 1024;
/// Event-count cap prevents endless tiny deltas from holding a request open.
pub(crate) const MAX_RESPONSE_EVENTS: usize = 100_000;
/// Indexed-part cap bounds maps and provider-controlled sparse indices.
pub(crate) const MAX_RESPONSE_PARTS: usize = 1_024;

/// Unified events emitted by the client generation stream.
///
/// The final response stays inline to avoid a heap allocation and a public API
/// change at the one terminal event emitted per generation.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum StreamEvent {
    /// Stream started. Always first.
    Started {
        /// Provider-assigned response identifier.
        response_id: Option<String>,
    },

    /// Advisory lifecycle feedback from an opt-in provider.
    ///
    /// This is transport telemetry, not assistant content, and therefore is
    /// never assembled into a [`Response`].
    ProviderLifecycle(ProviderLifecycle),

    /// Text generation segment started.
    TextStart {
        /// Canonical part index.
        index: usize,
    },
    /// Text chunk generated.
    TextDelta {
        /// Canonical part index.
        index: usize,
        /// Newly generated text chunk.
        delta: String,
    },
    /// Text generation segment finished.
    TextEnd {
        /// Canonical part index.
        index: usize,
    },

    /// Reasoning text generation segment started.
    ReasoningStart {
        /// Canonical part index.
        index: usize,
    },
    /// Reasoning text chunk generated.
    ReasoningDelta {
        /// Canonical part index.
        index: usize,
        /// Newly generated reasoning text chunk.
        delta: String,
    },
    /// Reasoning text generation segment finished.
    ReasoningEnd {
        /// Canonical part index.
        index: usize,
    },

    /// Tool call generation started.
    ToolCallStart {
        /// Provider scheduling metadata, not tool-execution authority.
        async_execution: bool,
        /// Canonical part index.
        index: usize,
        /// Tool call identifier.
        id: ToolCallId,
        /// Name of the tool to invoke.
        name: String,
    },
    /// Tool call arguments chunk generated.
    ToolCallArgsDelta {
        /// Canonical part index.
        index: usize,
        /// Newly generated JSON arguments string chunk.
        delta: String,
    },
    /// Tool call generation finished.
    ToolCallEnd {
        /// Canonical part index.
        index: usize,
        /// Recoverable schema validation status for the completed call.
        ///
        /// Codecs emit `None`; stream assembly fills this after normalizing and
        /// validating the completed arguments against the request snapshot.
        argument_error: Option<ToolCallArgumentError>,
    },

    /// Self-contained multimodal media generated.
    MediaCompleted {
        /// Canonical part index.
        index: usize,
        /// Assembled media object.
        media: Media,
    },

    /// Intermediate or final token billing counters.
    Usage(Usage),
    /// Generation successfully finished. Always last on success.
    Finished(Response),
}

/// A pinned, boxed stream of generation events.
pub type ResponseStream =
    std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<StreamEvent, AiError>> + Send>>;

pub(crate) struct ToolCallBuilder {
    pub(crate) async_execution: bool,
    pub(crate) id: ToolCallId,
    pub(crate) name: String,
    pub(crate) arguments_json: String,
    pub(crate) argument_error: Option<ToolCallArgumentError>,
    /// True once arguments were normalized at an explicit `ToolCallEnd` or by
    /// the final pass; unrepairable output stays false until the stop reason
    /// decides between the truncation and malformed envelopes.
    pub(crate) arguments_normalized: bool,
}

/// Incremental state for the OpenAI Chat content-tool compatibility parser.
///
/// Search offsets always point at the first byte not yet examined for the
/// state's delimiter. Keeping them here makes a marker split across SSE events
/// cheap to resume instead of rescanning the entire pending response prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpenAiChatCompatibilityState {
    /// Search ordinary content for an XML/control marker.
    Scanning { scan_from: usize },
    /// `<tool_call...` was found; search incrementally for its opening `>`.
    ToolCallOpen { scan_from: usize },
    /// The opening tag is complete; search incrementally for `</tool_call>`.
    ToolCallBody { open_end: usize, scan_from: usize },
    /// `<function...` was found; search incrementally for `</function>`.
    FunctionBody { scan_from: usize },
    /// A standalone `</function>` completed; briefly wait for an optional
    /// outer `</tool_call>` that may be split across the next provider delta.
    FunctionClosed { close_end: usize },
    /// An explicitly enabled ambiguous bare-JSON candidate is held to EOF.
    BareJson,
}

impl Default for OpenAiChatCompatibilityState {
    fn default() -> Self {
        Self::Scanning { scan_from: 0 }
    }
}

/// Helper builder that statefully assembles stream events into a finished Response.
pub(crate) struct ResponseBuilder {
    pub(crate) model: ModelId,
    pub(crate) protocol: Protocol,
    pub(crate) pricing: Option<Pricing>,
    /// The exact tier carried by this physical Responses request.
    pub(crate) requested_service_tier: Option<crate::types::ServiceTier>,
    /// None until a codec settles pricing; Some(None) explicitly means unpriced.
    pub(crate) response_cost: Option<Option<crate::pricing::Cost>>,
    pub(crate) server_timing: crate::inference::wire::ServerTiming,
    /// The request's exact tool-definition snapshot. `None` is reserved for
    /// direct schema-less codec fixtures; production assembly sets `Some`, even
    /// when the request has no tools, so known response tools are validated
    /// against the exact snapshot while unknown names remain available for
    /// the agent's bounded unknown-tool recovery path.
    pub(crate) tool_definitions: Option<Vec<ToolDef>>,
    pub(crate) strict_tool_sampling: bool,
    pub(crate) response_id: Option<String>,
    /// Authoritative terminal OpenAI Responses output, if supplied.
    pub(crate) responses_output: Option<crate::responses::ResponsesOutput>,
    /// Deferred provider handle when the provider parked this turn.
    pub(crate) deferred: Option<crate::deferred::DeferredHandle>,
    pub(crate) text_buffers: HashMap<usize, String>,
    pub(crate) reasoning_text_buffers: HashMap<usize, String>,
    pub(crate) reasoning_states: HashMap<usize, ReasoningState>,
    /// Opaque provider metadata retained immediately before its target part.
    pub(crate) part_metadata: HashMap<usize, ProviderPartMetadata>,
    pub(crate) tool_call_builders: HashMap<usize, ToolCallBuilder>,
    pub(crate) media_parts: HashMap<usize, Media>,
    pub(crate) usage: Option<Usage>,
    pub(crate) stop_reason: Option<StopReason>,
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) observed_indices: HashSet<usize>,
    pub(crate) aggregate_content_bytes: usize,
    /// Bytes retained outside canonical response parts while a codec waits for
    /// enough provider data to classify them. Together with
    /// `aggregate_content_bytes`, this may never exceed the response cap.
    pub(crate) buffered_content_bytes: usize,
    pub(crate) event_count: usize,
    /// Raw provider events are counted before decoding because compatibility
    /// buffering can otherwise consume arbitrarily many events without
    /// producing a canonical [`StreamEvent`].
    pub(crate) provider_event_count: usize,
    pub(crate) provider_to_canonical_indices: HashMap<String, usize>,
    pub(crate) temp_buffers: HashMap<String, String>,
    /// Parsed cumulative Google arguments and their reserved serialized size.
    pub(crate) google_function_args: HashMap<usize, (serde_json::Value, usize)>,
    /// Content buffered by a compatibility parser until it is known whether it
    /// is ordinary assistant text or a Qwen XML tool call. This is only used by
    /// the OpenAI Chat codec; keeping it in the shared builder avoids losing a
    /// marker split across SSE chunks.
    pub(crate) qwen_xml_pending: String,
    /// Incremental parser state for `qwen_xml_pending`.
    pub(crate) qwen_xml_state: OpenAiChatCompatibilityState,
    /// Whether ambiguous bare JSON may be held until turn completion and
    /// interpreted as a compatibility tool call. The default is deliberately
    /// false so ordinary streamed JSON remains visible.
    pub(crate) buffer_ambiguous_compatibility_content: bool,
    /// Complete compatibility calls held until turn completion. A later native
    /// structured call supersedes these without leaking duplicate calls.
    pub(crate) qwen_xml_buffered_calls: Vec<(String, String)>,
    /// Number of synthetic tool-call IDs allocated for content-based XML/JSON
    /// calls in this response.
    pub(crate) qwen_xml_call_count: usize,
    /// A local-model control placeholder was emitted instead of the intended
    /// tool call. The Chat codec suppresses it and requests a corrective turn.
    pub(crate) tool_output_locked_seen: bool,
    /// Whether the provider has emitted a structured tool call in this
    /// response. Structured calls are authoritative over compatibility text.
    pub(crate) native_tool_call_seen: bool,
    /// Canonical indices whose `*End` event was already emitted. Codecs consult
    /// this to keep provider quirks (duplicate finish chunks, deltas after a
    /// close) from violating the one-End-per-part invariant.
    pub(crate) ended_indices: HashSet<usize>,
    /// Next canonical index to allocate. Monotonic: it never decreases, so a
    /// re-keyed provider segment can never collide with an existing index.
    pub(crate) next_canonical_index: usize,
    /// Whether the `Started` event was emitted. Tracked separately from
    /// `response_id` so a first chunk with an empty/absent provider id does
    /// not re-arm the start gate.
    pub(crate) started: bool,
    /// Request translation policy for native codecs with non-canonical output.
    pub(crate) compatibility: crate::types::CompatibilityMode,
    /// Native Conversations terminal latch survives `finish_mut` replacement.
    pub(crate) mistral_finished: bool,
    /// Highest native entry index first observed, fencing reordered tool effects.
    pub(crate) mistral_last_output_index: Option<u64>,
}

impl ResponseBuilder {
    /// Creates a new ResponseBuilder.
    pub(crate) fn new(model: ModelId, protocol: Protocol, pricing: Option<Pricing>) -> Self {
        Self {
            model,
            protocol,
            pricing,
            requested_service_tier: None,
            response_cost: None,
            server_timing: Default::default(),
            tool_definitions: None,
            strict_tool_sampling: false,
            response_id: None,
            responses_output: None,
            deferred: None,
            text_buffers: HashMap::with_capacity(4),
            reasoning_text_buffers: HashMap::with_capacity(2),
            reasoning_states: HashMap::with_capacity(2),
            part_metadata: HashMap::with_capacity(2),
            tool_call_builders: HashMap::with_capacity(4),
            media_parts: HashMap::with_capacity(2),
            usage: None,
            stop_reason: None,
            diagnostics: Vec::new(),
            observed_indices: HashSet::with_capacity(4),
            aggregate_content_bytes: 0,
            buffered_content_bytes: 0,
            event_count: 0,
            provider_event_count: 0,
            provider_to_canonical_indices: HashMap::with_capacity(4),
            temp_buffers: HashMap::with_capacity(2),
            google_function_args: HashMap::new(),
            qwen_xml_pending: String::new(),
            qwen_xml_state: OpenAiChatCompatibilityState::default(),
            buffer_ambiguous_compatibility_content: false,
            qwen_xml_buffered_calls: Vec::new(),
            qwen_xml_call_count: 0,
            tool_output_locked_seen: false,
            native_tool_call_seen: false,
            ended_indices: HashSet::with_capacity(4),
            next_canonical_index: 0,
            started: false,
            compatibility: crate::types::CompatibilityMode::Strict,
            mistral_finished: false,
            mistral_last_output_index: None,
        }
    }

    /// Installs the exact request tool-definition snapshot used to validate
    /// assembled calls. A failed schema check leaves the builder unchanged.
    pub(crate) fn set_tool_definitions(&mut self, definitions: &[ToolDef]) -> Result<(), AiError> {
        crate::json_repair::validate_tool_definitions(definitions).map_err(AiError::Decode)?;
        self.tool_definitions = Some(definitions.to_vec());
        Ok(())
    }

    /// Records a diagnostic from lossy translation.
    pub(crate) fn add_diagnostic(&mut self, diag: Diagnostic) {
        // Diagnostics are non-semantic hints. Bound them rather than allowing a
        // malformed provider response to retain an unlimited vector.
        if self.diagnostics.len() < MAX_RESPONSE_PARTS {
            self.diagnostics.push(diag);
        }
    }

    fn add_content_bytes(&mut self, bytes: usize) -> Result<(), AiError> {
        let aggregate = self
            .aggregate_content_bytes
            .checked_add(bytes)
            .ok_or(AiError::Decode(DecodeError::ResponseTooLarge))?;
        aggregate
            .checked_add(self.buffered_content_bytes)
            .filter(|total| *total <= MAX_RESPONSE_CONTENT_BYTES)
            .ok_or(AiError::Decode(DecodeError::ResponseTooLarge))?;
        self.aggregate_content_bytes = aggregate;
        Ok(())
    }

    /// Reserves bytes retained by a codec before they become canonical stream
    /// events. This closes the gap where pre-ID tool arguments and content
    /// compatibility candidates previously bypassed the aggregate limit.
    pub(crate) fn reserve_buffered_content(&mut self, bytes: usize) -> Result<(), AiError> {
        let buffered = self
            .buffered_content_bytes
            .checked_add(bytes)
            .ok_or(AiError::Decode(DecodeError::ResponseTooLarge))?;
        self.aggregate_content_bytes
            .checked_add(buffered)
            .filter(|total| *total <= MAX_RESPONSE_CONTENT_BYTES)
            .ok_or(AiError::Decode(DecodeError::ResponseTooLarge))?;
        self.buffered_content_bytes = buffered;
        Ok(())
    }

    /// Releases a codec reservation immediately before the corresponding data
    /// is emitted, discarded as control syntax, or replaced.
    pub(crate) fn release_buffered_content(&mut self, bytes: usize) {
        debug_assert!(bytes <= self.buffered_content_bytes);
        self.buffered_content_bytes = self.buffered_content_bytes.saturating_sub(bytes);
    }

    pub(crate) fn resize_buffered_content(
        &mut self,
        old: usize,
        new: usize,
    ) -> Result<(), AiError> {
        let without_old = self
            .buffered_content_bytes
            .checked_sub(old)
            .ok_or_else(|| {
                AiError::Decode(DecodeError::Json(
                    "internal buffered-content accounting underflow".to_string(),
                ))
            })?;
        let buffered = without_old
            .checked_add(new)
            .ok_or(AiError::Decode(DecodeError::ResponseTooLarge))?;
        self.aggregate_content_bytes
            .checked_add(buffered)
            .filter(|total| *total <= MAX_RESPONSE_CONTENT_BYTES)
            .ok_or(AiError::Decode(DecodeError::ResponseTooLarge))?;
        self.buffered_content_bytes = buffered;
        Ok(())
    }

    /// Replaces a temporary provider field while preserving aggregate buffer
    /// accounting. OpenAI Chat uses this for IDs/names that may arrive before
    /// the first argument delta.
    pub(crate) fn replace_temp_buffer(
        &mut self,
        key: String,
        value: String,
    ) -> Result<(), AiError> {
        let old = self.temp_buffers.get(&key).map_or(0, String::len);
        self.resize_buffered_content(old, value.len())?;
        self.temp_buffers.insert(key, value);
        Ok(())
    }

    /// Appends a temporary provider field, enforcing both its category cap and
    /// the aggregate response cap before allocating/growing the buffer.
    pub(crate) fn append_temp_buffer_bounded(
        &mut self,
        key: String,
        delta: &str,
        max_bytes: usize,
    ) -> Result<(), AiError> {
        let old = self.temp_buffers.get(&key).map_or(0, String::len);
        let new = old
            .checked_add(delta.len())
            .filter(|size| *size <= max_bytes)
            .ok_or(AiError::Decode(DecodeError::ToolArgumentsTooLarge))?;
        self.resize_buffered_content(old, new)?;
        self.temp_buffers.entry(key).or_default().push_str(delta);
        Ok(())
    }

    /// Appends a temporary provider field whose only category limit is the
    /// aggregate response cap (for example, an opaque reasoning signature).
    pub(crate) fn append_temp_buffer(&mut self, key: String, delta: &str) -> Result<(), AiError> {
        let old = self.temp_buffers.get(&key).map_or(0, String::len);
        let new = old
            .checked_add(delta.len())
            .ok_or(AiError::Decode(DecodeError::ResponseTooLarge))?;
        self.resize_buffered_content(old, new)?;
        self.temp_buffers.entry(key).or_default().push_str(delta);
        Ok(())
    }

    /// Removes a temporary provider field and releases its reservation.
    pub(crate) fn take_temp_buffer(&mut self, key: &str) -> Option<String> {
        let value = self.temp_buffers.remove(key)?;
        self.release_buffered_content(value.len());
        Some(value)
    }

    /// Selects whether the OpenAI Chat codec may buffer ambiguous bare JSON.
    pub(crate) fn set_buffer_ambiguous_compatibility_content(&mut self, enabled: bool) {
        self.buffer_ambiguous_compatibility_content = enabled;
    }

    /// Counts a raw provider stream event before it reaches a codec. Canonical
    /// output events remain independently guarded by [`Self::on_event`].
    pub(crate) fn observe_provider_stream_event(&mut self) -> Result<(), AiError> {
        self.provider_event_count = self
            .provider_event_count
            .checked_add(1)
            .filter(|count| *count <= MAX_RESPONSE_EVENTS)
            .ok_or(AiError::Decode(DecodeError::TooManyStreamEvents))?;
        Ok(())
    }

    fn observe_index(&mut self, index: usize) -> Result<(), AiError> {
        self.observed_indices.insert(index);
        if self.observed_indices.len() > MAX_RESPONSE_PARTS {
            return Err(AiError::Decode(DecodeError::TooManyResponseParts));
        }
        Ok(())
    }

    /// Feeds a stream event into the builder.
    pub(crate) fn on_event(&mut self, event: &StreamEvent) -> Result<(), AiError> {
        self.event_count = self
            .event_count
            .checked_add(1)
            .filter(|count| *count <= MAX_RESPONSE_EVENTS)
            .ok_or(AiError::Decode(DecodeError::TooManyStreamEvents))?;
        match event {
            StreamEvent::Started { response_id } => {
                self.response_id = response_id.clone();
                self.started = true;
            }
            StreamEvent::TextStart { index } => {
                self.observe_index(*index)?;
                self.text_buffers.insert(*index, String::new());
            }
            StreamEvent::TextDelta { index, delta } => {
                self.add_content_bytes(delta.len())?;
                if let Some(buf) = self.text_buffers.get_mut(index) {
                    buf.push_str(delta);
                }
            }
            StreamEvent::ReasoningStart { index } => {
                self.observe_index(*index)?;
                self.reasoning_text_buffers.insert(*index, String::new());
            }
            StreamEvent::ReasoningDelta { index, delta } => {
                self.add_content_bytes(delta.len())?;
                if let Some(buf) = self.reasoning_text_buffers.get_mut(index) {
                    buf.push_str(delta);
                }
            }
            StreamEvent::ToolCallStart {
                index,
                id,
                name,
                async_execution,
            } => {
                if *async_execution
                    && (self.protocol != Protocol::OpenAiResponses
                        || !self.tool_definitions.as_ref().is_some_and(|tools| {
                            tools
                                .iter()
                                .any(|tool| tool.async_execution && tool.name == *name)
                        }))
                {
                    return Err(DecodeError::InvalidProviderField(
                        "async call is not advertised by the request".into(),
                    )
                    .into());
                }
                self.observe_index(*index)?;
                self.add_content_bytes(id.0.len().saturating_add(name.len()))?;
                self.tool_call_builders.insert(
                    *index,
                    ToolCallBuilder {
                        async_execution: *async_execution,
                        id: id.clone(),
                        name: name.clone(),
                        arguments_json: String::new(),
                        argument_error: None,
                        arguments_normalized: false,
                    },
                );
            }
            StreamEvent::ToolCallArgsDelta { index, delta } => {
                self.add_content_bytes(delta.len())?;
                if let Some(builder) = self.tool_call_builders.get_mut(index) {
                    if builder
                        .arguments_json
                        .len()
                        .checked_add(delta.len())
                        .is_none_or(|size| size > MAX_TOOL_ARGUMENT_BYTES)
                    {
                        return Err(AiError::Decode(DecodeError::ToolArgumentsTooLarge));
                    }
                    builder.arguments_json.push_str(delta);
                }
            }
            StreamEvent::MediaCompleted { index, media } => {
                self.observe_index(*index)?;
                let bytes = serde_json::to_vec(media)
                    .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))?;
                self.add_content_bytes(bytes.len())?;
                self.media_parts.insert(*index, media.clone());
            }
            StreamEvent::ToolCallEnd { index, .. } => {
                // A completed, parseable call is schema-checked before this
                // terminal event reaches consumers. This prevents downstream
                // speculative execution from observing unchecked arguments.
                // Unparseable output is deferred to `finish`, where the stop
                // reason decides whether it is a max-token truncation or a
                // malformed call that keeps its envelope for the model.
                self.normalize_completed_tool_arguments(*index)?;
                self.ended_indices.insert(*index);
            }
            StreamEvent::TextEnd { index } | StreamEvent::ReasoningEnd { index } => {
                self.ended_indices.insert(*index);
            }
            StreamEvent::Usage(u) => {
                self.usage = Some(*u);
            }
            _ => {}
        }
        Ok(())
    }

    /// Sets the stop reason at stream finish.
    pub(crate) fn set_stop_reason(&mut self, reason: StopReason) {
        self.stop_reason = Some(reason);
    }

    /// Sets the deferred handle describing a [`StopReason::Deferred`] terminal.
    pub(crate) fn set_deferred(&mut self, deferred: crate::deferred::DeferredHandle) {
        self.deferred = Some(deferred);
    }

    /// Replaces retained reasoning continuation state within the response budget.
    /// A rejected replacement leaves both the prior state and accounting intact.
    pub(crate) fn set_reasoning_state(
        &mut self,
        index: usize,
        state: ReasoningState,
    ) -> Result<(), AiError> {
        fn retained_bytes(state: &ReasoningState) -> Option<usize> {
            match &state.kind {
                crate::types::ReasoningStateKind::AnthropicSignature { signature } => {
                    Some(signature.len())
                }
                crate::types::ReasoningStateKind::AnthropicRedacted { data } => Some(data.len()),
                crate::types::ReasoningStateKind::OpenAiReasoning {
                    item_id,
                    encrypted_content,
                } => item_id
                    .as_ref()
                    .map_or(0, String::len)
                    .checked_add(encrypted_content.as_ref().map_or(0, String::len)),
            }
        }

        let old = self
            .reasoning_states
            .get(&index)
            .map_or(Some(0), retained_bytes)
            .ok_or(AiError::Decode(DecodeError::ResponseTooLarge))?;
        let new = retained_bytes(&state).ok_or(AiError::Decode(DecodeError::ResponseTooLarge))?;
        let aggregate = (self.aggregate_content_bytes - old)
            .checked_add(new)
            .ok_or(AiError::Decode(DecodeError::ResponseTooLarge))?;
        aggregate
            .checked_add(self.buffered_content_bytes)
            .filter(|total| *total <= MAX_RESPONSE_CONTENT_BYTES)
            .ok_or(AiError::Decode(DecodeError::ResponseTooLarge))?;
        self.aggregate_content_bytes = aggregate;
        self.reasoning_states.insert(index, state);
        Ok(())
    }

    /// Atomically replace a provider's argument preview within both limits.
    pub(crate) fn replace_tool_arguments(
        &mut self,
        index: usize,
        arguments: String,
    ) -> Result<(), AiError> {
        if arguments.len() > MAX_TOOL_ARGUMENT_BYTES {
            return Err(AiError::Decode(DecodeError::ResponseTooLarge));
        }
        let old = self.tool_call_builders[&index].arguments_json.len();
        let aggregate = (self.aggregate_content_bytes - old)
            .checked_add(arguments.len())
            .ok_or(AiError::Decode(DecodeError::ResponseTooLarge))?;
        aggregate
            .checked_add(self.buffered_content_bytes)
            .filter(|total| *total <= MAX_RESPONSE_CONTENT_BYTES)
            .ok_or(AiError::Decode(DecodeError::ResponseTooLarge))?;
        self.aggregate_content_bytes = aggregate;
        let call = self
            .tool_call_builders
            .get_mut(&index)
            .expect("open tool call");
        call.arguments_json = arguments;
        call.arguments_normalized = false;
        Ok(())
    }

    fn apply_normalized_tool_arguments(
        builder: &mut ToolCallBuilder,
        mut arguments_json: String,
        tool_definitions: Option<&[ToolDef]>,
        strict_tool_sampling: bool,
    ) -> Result<(), AiError> {
        let argument_error = if let Some(definitions) = tool_definitions {
            let mut arguments = serde_json::from_str(&arguments_json)
                .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))?;
            crate::constrained_sampling::normalize_tool_arguments(
                &builder.name,
                &mut arguments,
                definitions,
                strict_tool_sampling,
            )?;
            arguments_json = arguments.to_string();
            match crate::json_repair::validate_tool_arguments(
                &builder.name,
                &arguments,
                definitions,
            )
            .map_err(AiError::Decode)?
            {
                ToolArgumentValidation::SchemaMismatch => {
                    Some(ToolCallArgumentError::SchemaMismatch)
                }
                ToolArgumentValidation::Valid | ToolArgumentValidation::UnknownTool => None,
            }
        } else {
            None
        };
        builder.arguments_json = arguments_json;
        builder.argument_error = argument_error;
        builder.arguments_normalized = true;
        Ok(())
    }

    /// Normalizes a parseable call at its explicit terminal event.
    ///
    /// A malformed value is deliberately deferred to [`Self::normalize_tool_arguments`]:
    /// the eventual stop reason decides whether a max-token response discards
    /// truncated arguments or a malformed call keeps its envelope marked
    /// [`ToolCallArgumentError::Malformed`]. A parseable call, including a
    /// schema mismatch, is marked before consumers can speculate on it.
    fn normalize_completed_tool_arguments(&mut self, index: usize) -> Result<(), AiError> {
        let tool_definitions = self.tool_definitions.as_deref();
        let Some(builder) = self.tool_call_builders.get_mut(&index) else {
            return Ok(());
        };
        if builder.arguments_normalized {
            return Ok(());
        }
        let arguments_json = {
            let raw_arguments = if builder.arguments_json.trim().is_empty() {
                "{}"
            } else {
                builder.arguments_json.as_str()
            };
            match crate::json_repair::normalize_json_object(raw_arguments) {
                Ok(arguments_json) => arguments_json,
                Err(_) => return Ok(()),
            }
        };
        Self::apply_normalized_tool_arguments(
            builder,
            arguments_json,
            tool_definitions,
            self.strict_tool_sampling,
        )
    }

    /// Returns the recoverable argument marker computed for an explicitly
    /// completed streamed call. `None` also covers a call whose unparseable
    /// arguments must wait for final stop-reason handling.
    pub(crate) fn tool_call_argument_error(&self, index: usize) -> Option<ToolCallArgumentError> {
        self.tool_call_builders
            .get(&index)
            .and_then(|builder| builder.argument_error)
    }

    /// Attaches opaque provider metadata to an already-started canonical part.
    ///
    /// The metadata is retained immediately before that part during assembly so
    /// a later request can replay provider continuation context without
    /// reclassifying the content as reasoning.
    pub(crate) fn set_provider_metadata(
        &mut self,
        index: usize,
        metadata: ProviderPartMetadata,
    ) -> Result<(), AiError> {
        if self.part_metadata.contains_key(&index) {
            return Ok(());
        }
        if !self.observed_indices.contains(&index)
            || self
                .observed_indices
                .len()
                .checked_add(self.part_metadata.len())
                .is_none_or(|parts| parts >= MAX_RESPONSE_PARTS)
        {
            return Err(AiError::Decode(DecodeError::TooManyResponseParts));
        }
        let bytes = match &metadata {
            ProviderPartMetadata::GoogleThoughtSignature { signature } => signature.len(),
        };
        self.add_content_bytes(bytes)?;
        self.part_metadata.insert(index, metadata);
        Ok(())
    }

    /// Normalizes unprocessed provider-generated tool arguments before consuming
    /// the builder.
    ///
    /// A max-token terminal may cut a tool argument string in the middle. Keep
    /// the call envelope so the agent can pair it with a synthetic error result,
    /// but never expose guessed partial arguments for execution. Any other
    /// unrepairable completion keeps its envelope too, marked
    /// [`ToolCallArgumentError::Malformed`]: Pi parses such arguments leniently
    /// and lets the call fail its own validation, so one malformed call must not
    /// end the run. Performing this pass before [`Self::finish_mut`] replaces the
    /// builder also preserves stream-progress counters when strict normalization
    /// of a repairable call fails.
    fn normalize_tool_arguments(&mut self) -> Result<(), AiError> {
        let output_truncated = matches!(self.stop_reason, Some(StopReason::MaxTokens));
        let mut discarded_truncated_arguments = false;
        let mut malformed_arguments = false;
        {
            let tool_definitions = self.tool_definitions.as_deref();
            for builder in self.tool_call_builders.values_mut() {
                if builder.arguments_normalized {
                    continue;
                }
                let raw_arguments = if builder.arguments_json.trim().is_empty() {
                    "{}"
                } else {
                    builder.arguments_json.as_str()
                };
                let (arguments_json, argument_error) =
                    crate::json_repair::normalize_completed_tool_arguments(raw_arguments);
                if let Some(argument_error) = argument_error {
                    // An authoritative max-token terminal makes truncation the
                    // expected explanation, so the call is discarded without a
                    // marker and the agent's truncation path names it. Any other
                    // unrepairable text is malformed provider output the model
                    // must see and correct.
                    builder.arguments_json =
                        crate::json_repair::UNREPAIRABLE_TOOL_ARGUMENTS.to_owned();
                    builder.argument_error = (!output_truncated).then_some(argument_error);
                    builder.arguments_normalized = true;
                    if output_truncated {
                        discarded_truncated_arguments = true;
                    } else {
                        malformed_arguments = true;
                    }
                    continue;
                }
                Self::apply_normalized_tool_arguments(
                    builder,
                    arguments_json,
                    tool_definitions,
                    self.strict_tool_sampling,
                )?;
            }
        }
        if discarded_truncated_arguments {
            self.add_diagnostic(Diagnostic {
                code: "discarded_truncated_tool_arguments".to_owned(),
                message: "Tool arguments truncated at the provider output limit were replaced with an empty object and must not be executed".to_owned(),
            });
        }
        if malformed_arguments {
            self.add_diagnostic(crate::json_repair::malformed_tool_arguments_diagnostic());
        }
        Ok(())
    }

    /// Assembles the final Response by replacing the builder with an empty one.
    pub(crate) fn finish_mut(&mut self) -> Result<Response, AiError> {
        self.normalize_tool_arguments()?;
        let dummy = Self::new(self.model.clone(), self.protocol, self.pricing.clone());
        let owned = std::mem::replace(self, dummy);
        owned.finish_normalized()
    }

    /// Assembles the final Response.
    pub(crate) fn finish(mut self) -> Result<Response, AiError> {
        self.normalize_tool_arguments()?;
        self.finish_normalized()
    }

    fn finish_normalized(mut self) -> Result<Response, AiError> {
        let mut content = Vec::new();

        // Sort indices based on first-observation order
        let mut indices = self.observed_indices.into_iter().collect::<Vec<_>>();
        indices.sort_unstable();

        for index in indices {
            if let Some(metadata) = self.part_metadata.remove(&index) {
                content.push(AssistantPart::ProviderMetadata(metadata));
            }
            if let Some(text) = self.text_buffers.remove(&index) {
                content.push(AssistantPart::Text(text));
            } else if let Some(reasoning_text) = self.reasoning_text_buffers.remove(&index) {
                // Redacted/opaque reasoning carries no visible text (design §6.3):
                // an empty buffer becomes `None`, not `Some("")`.
                content.push(AssistantPart::Reasoning(ReasoningPart {
                    text: if reasoning_text.is_empty() {
                        None
                    } else {
                        Some(reasoning_text)
                    },
                    state: self.reasoning_states.remove(&index),
                }));
            } else if let Some(builder) = self.tool_call_builders.remove(&index) {
                content.push(AssistantPart::ToolCall(ToolCall {
                    async_execution: builder.async_execution,
                    id: builder.id,
                    name: builder.name,
                    arguments_json: builder.arguments_json,
                    argument_error: builder.argument_error,
                }));
            } else if let Some(media) = self.media_parts.remove(&index) {
                content.push(AssistantPart::Media(media));
            }
        }

        let message = AssistantMessage {
            content,
            model: self.model,
            protocol: self.protocol,
        };

        let usage = self.usage.unwrap_or_default();
        let cost = match self.response_cost {
            Some(cost) => cost,
            None => self
                .pricing
                .as_ref()
                .map(|p| crate::pricing::cost_of(p, &usage).map_err(AiError::Pricing))
                .transpose()?,
        };

        Ok(Response {
            message,
            stop_reason: self.stop_reason.unwrap_or(StopReason::EndTurn),
            usage,
            cost,
            response_id: self.response_id,
            responses_output: self.responses_output,
            deferred: self.deferred,
            inference: Some(self.server_timing.finish()),
            diagnostics: self.diagnostics,
        })
    }
}

/// Wraps a raw stream of events, statefully enforcing the stream protocol invariants.
/// Public, strict assembler for canonical events emitted by host-mediated
/// provider transports.
///
/// Native protocol codecs keep using the crate-private `ResponseBuilder`.
/// This adapter intentionally exposes only canonical event ingestion: an
/// integration cannot alter pricing, diagnostics, response snapshots, or the
/// request tool-definition snapshot while a response is being assembled.
/// Callers must deliver one `Started` event, balanced parts, and then call
/// [`Self::finish`] exactly once with the provider's terminal stop reason.
pub struct CanonicalStreamAssembler {
    builder: ResponseBuilder,
    active_parts: HashMap<usize, CanonicalPartKind>,
    started: bool,
    finished: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CanonicalPartKind {
    Text,
    Reasoning,
    ToolCall,
}

impl CanonicalStreamAssembler {
    /// Creates an assembler for one selected model and an exact request tool
    /// snapshot. The snapshot is validated before any provider event is
    /// accepted.
    pub fn new(
        model: ModelId,
        protocol: Protocol,
        pricing: Option<Pricing>,
        tool_definitions: &[ToolDef],
    ) -> Result<Self, AiError> {
        let mut builder = ResponseBuilder::new(model, protocol, pricing);
        builder.set_tool_definitions(tool_definitions)?;
        Ok(Self {
            builder,
            active_parts: HashMap::new(),
            started: false,
            finished: false,
        })
    }

    /// Adds diagnostics generated by the host while validating a request.
    ///
    /// Diagnostics are non-semantic and remain bounded by the canonical
    /// response builder. Provider transports must not use this to inject raw
    /// provider errors or credential-bearing data.
    pub fn add_host_diagnostics(&mut self, diagnostics: impl IntoIterator<Item = Diagnostic>) {
        for diagnostic in diagnostics {
            self.builder.add_diagnostic(diagnostic);
        }
    }

    /// Records one raw, transport-level frame before it is decoded into a
    /// canonical event. This preserves the normal response-event limit for
    /// adapters which receive many small wire frames.
    pub fn observe_transport_event(&mut self) -> Result<(), AiError> {
        self.ensure_open()?;
        self.builder.observe_provider_stream_event()
    }

    /// Sets the deferred handle for a provider-parked response.
    ///
    /// Callers pair this with [`Self::finish`] and [`StopReason::Deferred`]: the
    /// handle is transport data and never becomes assistant content. The
    /// host/kernel layer owns the durable suspension decision.
    pub fn set_deferred(&mut self, deferred: crate::deferred::DeferredHandle) {
        self.builder.set_deferred(deferred);
    }

    /// Validates and records a canonical event.
    ///
    /// A terminal [`StreamEvent::Finished`] is rejected because final response
    /// construction remains host-owned; use [`Self::finish`] instead.
    pub fn push(&mut self, event: StreamEvent) -> Result<(), AiError> {
        self.ensure_open()?;
        match &event {
            StreamEvent::Started { .. } => {
                if self.started {
                    return Err(StreamProtocolError::DuplicateStart.into());
                }
                self.started = true;
            }
            StreamEvent::Finished(_) => {
                return Err(StreamProtocolError::UnexpectedEvent(
                    "host-mediated transports must finish through CanonicalStreamAssembler::finish"
                        .to_owned(),
                )
                .into());
            }
            _ if !self.started => return Err(StreamProtocolError::MissingStart.into()),
            StreamEvent::TextStart { index } => self.start_part(*index, CanonicalPartKind::Text)?,
            StreamEvent::ReasoningStart { index } => {
                self.start_part(*index, CanonicalPartKind::Reasoning)?
            }
            StreamEvent::ToolCallStart { index, .. } => {
                self.start_part(*index, CanonicalPartKind::ToolCall)?
            }
            StreamEvent::TextDelta { index, .. } => {
                self.require_part(*index, CanonicalPartKind::Text)?
            }
            StreamEvent::ReasoningDelta { index, .. } => {
                self.require_part(*index, CanonicalPartKind::Reasoning)?
            }
            StreamEvent::ToolCallArgsDelta { index, .. } => {
                self.require_part(*index, CanonicalPartKind::ToolCall)?
            }
            StreamEvent::TextEnd { index } => self.end_part(*index, CanonicalPartKind::Text)?,
            StreamEvent::ReasoningEnd { index } => {
                self.end_part(*index, CanonicalPartKind::Reasoning)?
            }
            StreamEvent::ToolCallEnd { index, .. } => {
                self.end_part(*index, CanonicalPartKind::ToolCall)?
            }
            StreamEvent::MediaCompleted { .. }
            | StreamEvent::ProviderLifecycle(_)
            | StreamEvent::Usage(_) => {}
        }
        self.builder.on_event(&event)
    }

    /// Completes the response with an explicit terminal reason.
    pub fn finish(&mut self, stop_reason: StopReason) -> Result<Response, AiError> {
        self.ensure_open()?;
        if !self.started {
            return Err(StreamProtocolError::MissingStart.into());
        }
        if let Some(index) = self.active_parts.keys().min().copied() {
            return Err(StreamProtocolError::UnbalancedPart { index }.into());
        }
        self.finished = true;
        self.builder.set_stop_reason(stop_reason);
        self.builder.finish_mut()
    }

    fn ensure_open(&self) -> Result<(), AiError> {
        if self.finished {
            Err(StreamProtocolError::EventAfterFinish.into())
        } else {
            Ok(())
        }
    }

    fn start_part(&mut self, index: usize, kind: CanonicalPartKind) -> Result<(), AiError> {
        if self.active_parts.insert(index, kind).is_some() {
            return Err(StreamProtocolError::UnexpectedEvent(format!(
                "part {index} was started more than once"
            ))
            .into());
        }
        Ok(())
    }

    fn require_part(&self, index: usize, kind: CanonicalPartKind) -> Result<(), AiError> {
        if self.active_parts.get(&index) == Some(&kind) {
            Ok(())
        } else {
            Err(StreamProtocolError::UnexpectedEvent(format!(
                "event does not match an active part at index {index}"
            ))
            .into())
        }
    }

    fn end_part(&mut self, index: usize, kind: CanonicalPartKind) -> Result<(), AiError> {
        self.require_part(index, kind)?;
        self.active_parts.remove(&index);
        Ok(())
    }
}

pub(crate) fn guard<S>(inner: S) -> ResponseStream
where
    S: futures_core::Stream<Item = Result<StreamEvent, AiError>> + Send + 'static,
{
    use async_stream::try_stream;
    use futures_util::StreamExt;

    let mut inner = Box::pin(inner);
    let mut started = false;
    let mut finished = false;
    let mut part_states = HashMap::with_capacity(4);
    let mut usage_seen = false;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum PartState {
        Streaming(PartKind),
        Completed,
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum PartKind {
        Text,
        Reasoning,
        ToolCall,
    }

    let stream = try_stream! {
        while let Some(res) = inner.next().await {
            let ev = res?;

            if finished {
                Err(AiError::StreamProtocol(StreamProtocolError::EventAfterFinish))?;
            }

            match &ev {
                StreamEvent::Started { .. } => {
                    if started {
                        Err(AiError::StreamProtocol(StreamProtocolError::DuplicateStart))?;
                    }
                    started = true;
                }
                _ => {
                    if !started {
                        Err(AiError::StreamProtocol(StreamProtocolError::MissingStart))?;
                    }
                }
            }

            match &ev {
                StreamEvent::Started { .. } | StreamEvent::ProviderLifecycle(_) => {}
                StreamEvent::TextStart { index } => {
                    if part_states.contains_key(index) {
                        Err(AiError::StreamProtocol(StreamProtocolError::UnexpectedEvent(format!("TextStart on index {}", index))))?;
                    }
                    part_states.insert(*index, PartState::Streaming(PartKind::Text));
                }
                StreamEvent::TextDelta { index, .. } => {
                    match part_states.get(index) {
                        Some(PartState::Streaming(PartKind::Text)) => {}
                        _ => Err(AiError::StreamProtocol(StreamProtocolError::UnexpectedEvent(format!("TextDelta on index {}", index))))?,
                    }
                }
                StreamEvent::TextEnd { index } => {
                    match part_states.get(index) {
                        Some(PartState::Streaming(PartKind::Text)) => {
                            part_states.insert(*index, PartState::Completed);
                        }
                        _ => Err(AiError::StreamProtocol(StreamProtocolError::UnexpectedEvent(format!("TextEnd on index {}", index))))?,
                    }
                }
                StreamEvent::ReasoningStart { index } => {
                    if part_states.contains_key(index) {
                        Err(AiError::StreamProtocol(StreamProtocolError::UnexpectedEvent(format!("ReasoningStart on index {}", index))))?;
                    }
                    part_states.insert(*index, PartState::Streaming(PartKind::Reasoning));
                }
                StreamEvent::ReasoningDelta { index, .. } => {
                    match part_states.get(index) {
                        Some(PartState::Streaming(PartKind::Reasoning)) => {}
                        _ => Err(AiError::StreamProtocol(StreamProtocolError::UnexpectedEvent(format!("ReasoningDelta on index {}", index))))?,
                    }
                }
                StreamEvent::ReasoningEnd { index } => {
                    match part_states.get(index) {
                        Some(PartState::Streaming(PartKind::Reasoning)) => {
                            part_states.insert(*index, PartState::Completed);
                        }
                        _ => Err(AiError::StreamProtocol(StreamProtocolError::UnexpectedEvent(format!("ReasoningEnd on index {}", index))))?,
                    }
                }
                StreamEvent::ToolCallStart { index, .. } => {
                    if part_states.contains_key(index) {
                        Err(AiError::StreamProtocol(StreamProtocolError::UnexpectedEvent(format!("ToolCallStart on index {}", index))))?;
                    }
                    part_states.insert(*index, PartState::Streaming(PartKind::ToolCall));
                }
                StreamEvent::ToolCallArgsDelta { index, .. } => {
                    match part_states.get(index) {
                        Some(PartState::Streaming(PartKind::ToolCall)) => {}
                        _ => Err(AiError::StreamProtocol(StreamProtocolError::UnexpectedEvent(format!("ToolCallArgsDelta on index {}", index))))?,
                    }
                }
                StreamEvent::ToolCallEnd { index, .. } => {
                    match part_states.get(index) {
                        Some(PartState::Streaming(PartKind::ToolCall)) => {
                            part_states.insert(*index, PartState::Completed);
                        }
                        _ => Err(AiError::StreamProtocol(StreamProtocolError::UnexpectedEvent(format!("ToolCallEnd on index {}", index))))?,
                    }
                }
                StreamEvent::MediaCompleted { index, .. } => {
                    if part_states.contains_key(index) {
                        Err(AiError::StreamProtocol(StreamProtocolError::UnexpectedEvent(format!("MediaCompleted on index {}", index))))?;
                    }
                    part_states.insert(*index, PartState::Completed);
                }
                StreamEvent::Usage(_) => {
                    if usage_seen {
                        Err(AiError::StreamProtocol(StreamProtocolError::UnexpectedEvent(
                            "duplicate Usage event".to_string(),
                        )))?;
                    }
                    usage_seen = true;
                }
                StreamEvent::Finished(_) => {
                    // Check if all streaming parts are completed
                    for (idx, state) in &part_states {
                        if let PartState::Streaming(_) = state {
                            Err(AiError::StreamProtocol(StreamProtocolError::UnbalancedPart { index: *idx }))?;
                        }
                    }
                    finished = true;
                }
            }

            yield ev;
        }

        // A started stream whose transport closed before the provider's terminal
        // event (`[DONE]` / `message_stop` / `response.completed`) is a premature
        // EOF (design §8 terminal table, §17). `MissingFinish` is reserved for a
        // stream that yields no `Finished` at all (handled in `complete()`).
        if started && !finished {
            Err(AiError::StreamProtocol(StreamProtocolError::PrematureEof))?;
        }
    };

    Box::pin(stream)
}

#[cfg(test)]
mod tests;
