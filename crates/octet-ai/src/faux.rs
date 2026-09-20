//! In-process `faux` provider test double with a deferred-response lifecycle.
//!
//! This mirrors upstream Pi's `providers/faux.ts`: a deterministic provider
//! that scripts assistant messages, can park a request as a deferred handle,
//! and then exposes pending / ready / failed / cancelled poll outcomes. It is a
//! host transport, so it never touches the network, credentials, or the model
//! catalog. Tests (and embedding harnesses) register it with
//! [`FauxProvider::register`] and drive the ordinary [`crate::AiClient`] API.
//!
//! The double deliberately performs no implicit retries and no scheduling: each
//! [`HostStreamTransport::fetch_deferred`] call is one poll, exactly as the
//! caller's permit authorizes.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;

use crate::auth::Auth;
use crate::catalog::Model;
use crate::client::AiClient;
use crate::deferred::DeferredHandle;
use crate::error::{AiError, Diagnostic, ProviderError};
use crate::host_transport::{HostStreamModel, HostStreamTransport};
use crate::stream::{CanonicalStreamAssembler, ResponseStream, StreamEvent};
use crate::types::{
    AssistantMessage, Capabilities, Endpoint, EndpointId, EndpointTransport, ModalitySet, ModelId,
    ModelLimits, ModelSpec, Protocol, Request, Response, StopReason, ToolCallId, Usage,
};

/// Scripted assistant content for one faux response.
#[derive(Clone, Debug, Default)]
pub struct FauxMessage {
    /// Assistant text.
    pub text: String,
    /// Optional reasoning text emitted before the assistant text.
    pub reasoning: Option<String>,
    /// Tool calls emitted after text.
    pub tool_calls: Vec<FauxToolCall>,
    /// Terminal stop reason. Defaults to [`StopReason::EndTurn`].
    pub stop_reason: Option<StopReason>,
    /// Usage counters reported with the response.
    pub usage: Usage,
    /// Optional provider-assigned response identifier.
    pub response_id: Option<String>,
}

impl FauxMessage {
    /// Creates a text-only message.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    /// Adds reasoning text emitted before the assistant text.
    pub fn with_reasoning(mut self, reasoning: impl Into<String>) -> Self {
        self.reasoning = Some(reasoning.into());
        self
    }

    /// Adds one tool call.
    pub fn with_tool_call(mut self, tool_call: FauxToolCall) -> Self {
        self.tool_calls.push(tool_call);
        self
    }

    /// Overrides the terminal stop reason.
    pub fn with_stop_reason(mut self, stop_reason: StopReason) -> Self {
        self.stop_reason = Some(stop_reason);
        self
    }

    /// Overrides the usage counters.
    pub fn with_usage(mut self, usage: Usage) -> Self {
        self.usage = usage;
        self
    }
}

/// One scripted tool call.
#[derive(Clone, Debug)]
pub struct FauxToolCall {
    /// Tool call identifier. An empty id is replaced with a deterministic
    /// provider-assigned id at stream time.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Canonical JSON arguments object.
    pub arguments: serde_json::Value,
}

impl FauxToolCall {
    /// Creates a tool call with a provider-assigned id.
    pub fn new(name: impl Into<String>, arguments: serde_json::Value) -> Self {
        Self {
            id: String::new(),
            name: name.into(),
            arguments,
        }
    }

    /// Creates a tool call with an explicit id.
    pub fn with_id(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
}

/// One scripted faux response.
#[derive(Clone, Debug)]
pub enum FauxResponse {
    /// A completed assistant message.
    Message(FauxMessage),
    /// A provider-side failure (model error, or a failed deferred poll).
    Failure(String),
}

/// Construction options for a [`FauxProvider`].
#[derive(Clone, Debug)]
pub struct FauxOptions {
    /// Provider id recorded on deferred handles (default `faux`).
    pub provider: String,
    /// Api id recorded on deferred handles (default `faux`).
    pub api: String,
    /// Catalog endpoint id the provider registers under (default `faux`).
    pub endpoint_id: EndpointId,
    /// Model id (default `faux-1`).
    pub model_id: ModelId,
    /// Protocol the double advertises (default OpenAI Chat).
    pub protocol: Protocol,
    /// Number of polls that return the handle again before the scripted
    /// response becomes ready (default `0`).
    pub pending_fetches: u32,
    /// Provider-suggested minimum delay before the next poll.
    pub poll_after_ms: Option<u64>,
}

impl Default for FauxOptions {
    fn default() -> Self {
        Self {
            provider: "faux".to_owned(),
            api: "faux".to_owned(),
            endpoint_id: EndpointId("faux".to_owned()),
            model_id: ModelId("faux-1".to_owned()),
            protocol: Protocol::OpenAiChat,
            pending_fetches: 0,
            poll_after_ms: None,
        }
    }
}

/// Provider-side status of one deferred handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FauxDeferredStatus {
    /// The provider still reports the handle; poll again after the delay.
    Pending,
    /// The scripted terminal response is available.
    Ready,
    /// The scripted response is a provider failure.
    Failed,
    /// The caller cancelled the parked response.
    Cancelled,
}

/// Observable counters for one [`FauxProvider`].
#[derive(Clone, Debug, PartialEq)]
pub struct FauxState {
    /// Number of ordinary (non-deferred) stream calls.
    pub call_count: u64,
    /// Number of deferred submissions.
    pub deferred_submission_count: u64,
    /// Number of deferred polls (pending or ready).
    pub deferred_fetch_count: u64,
    /// Handles cancelled by the caller, in order.
    pub cancelled_deferred: Vec<DeferredHandle>,
    /// Scripted responses still queued for future calls.
    pub pending_responses: usize,
}

#[derive(Clone, Debug)]
enum FauxScript {
    Message(FauxMessage),
    Failure(String),
}

#[derive(Clone, Debug)]
struct FauxDeferredEntry {
    handle: DeferredHandle,
    script: FauxScript,
    pending_fetches: u32,
    cancelled: bool,
    /// Terminal resolution cached after the first ready poll, exactly like
    /// upstream: a repeated ready poll returns the same finalized message and
    /// never re-runs the script.
    resolved: Option<Result<FauxMessage, String>>,
}

struct FauxInner {
    options: FauxOptions,
    scripts: VecDeque<FauxScript>,
    deferred: BTreeMap<String, FauxDeferredEntry>,
    call_count: u64,
    deferred_submission_count: u64,
    deferred_fetch_count: u64,
    cancelled_deferred: Vec<DeferredHandle>,
    next_call_id: u64,
}

/// Deterministic in-process provider used to exercise streaming and the
/// deferred lifecycle without network, credentials, or a model catalog.
#[derive(Clone)]
pub struct FauxProvider {
    inner: Arc<Mutex<FauxInner>>,
    model: Model,
}

impl FauxProvider {
    /// Creates a provider with an empty response script.
    pub fn new(options: FauxOptions) -> Self {
        let endpoint = Endpoint {
            id: options.endpoint_id.clone(),
            base_url: url::Url::parse("http://localhost:0/").expect("static faux base URL"),
            auth: Auth::None,
            default_headers: http::HeaderMap::new(),
            transport: EndpointTransport::Http,
            runtime: crate::types::RequestRuntime::default(),
            timeout: Duration::from_secs(10),
        };
        let spec = ModelSpec {
            id: options.model_id.clone(),
            endpoint: options.endpoint_id.clone(),
            api_name: options.model_id.0.clone(),
            display_name: Some("Faux Model".to_owned()),
            protocol: options.protocol,
            capabilities: Capabilities {
                input_modalities: ModalitySet::none(),
                output_modalities: ModalitySet::none(),
                tools: true,
                parallel_tool_calls: false,
                reasoning: None,
                responses_lite: false,
                agent_delegation: None,
                structured_output: false,
                deferred_tool_loading: false,
            },
            limits: ModelLimits {
                context_window: 128_000,
                max_output_tokens: 16_384,
            },
            pricing: None,
            cache: crate::types::CacheCompatibility::default(),
            preset: crate::declarations::ModelPreset::default(),
        };
        Self {
            inner: Arc::new(Mutex::new(FauxInner {
                options,
                scripts: VecDeque::new(),
                deferred: BTreeMap::new(),
                call_count: 0,
                deferred_submission_count: 0,
                deferred_fetch_count: 0,
                cancelled_deferred: Vec::new(),
                next_call_id: 0,
            })),
            model: Model {
                spec: Arc::new(spec),
                endpoint: Arc::new(endpoint),
            },
        }
    }

    /// The selected model handle for this provider.
    pub fn model(&self) -> &Model {
        &self.model
    }

    /// Replaces the queued response script.
    pub fn set_responses(&self, responses: Vec<FauxResponse>) {
        let mut inner = self.lock();
        inner.scripts = responses.into_iter().map(script_of).collect();
    }

    /// Appends to the queued response script.
    pub fn append_responses(&self, responses: Vec<FauxResponse>) {
        let mut inner = self.lock();
        inner.scripts.extend(responses.into_iter().map(script_of));
    }

    /// Queues one response.
    pub fn push_response(&self, response: FauxResponse) {
        self.lock().scripts.push_back(script_of(response));
    }

    /// How many scripted responses are still queued.
    pub fn pending_response_count(&self) -> usize {
        self.lock().scripts.len()
    }

    /// Snapshot of the provider counters.
    pub fn state(&self) -> FauxState {
        let inner = self.lock();
        FauxState {
            call_count: inner.call_count,
            deferred_submission_count: inner.deferred_submission_count,
            deferred_fetch_count: inner.deferred_fetch_count,
            cancelled_deferred: inner.cancelled_deferred.clone(),
            pending_responses: inner.scripts.len(),
        }
    }

    /// Provider-side status of one deferred handle, if it belongs to this
    /// provider.
    pub fn deferred_status(&self, handle: &DeferredHandle) -> Option<FauxDeferredStatus> {
        let inner = self.lock();
        inner.deferred.get(&handle.id).map(|entry| {
            if entry.cancelled {
                FauxDeferredStatus::Cancelled
            } else if entry.pending_fetches > 0 {
                FauxDeferredStatus::Pending
            } else if matches!(entry.script, FauxScript::Failure(_)) {
                FauxDeferredStatus::Failed
            } else {
                FauxDeferredStatus::Ready
            }
        })
    }

    /// Registers this provider as the host stream transport for its endpoint.
    pub fn register(&self, client: &AiClient) {
        client
            .register_host_stream_transport(self.model.endpoint.id.clone(), Arc::new(self.clone()));
    }

    fn lock(&self) -> MutexGuard<'_, FauxInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn next_provider_call_id(inner: &mut FauxInner) -> String {
        inner.next_call_id += 1;
        format!("faux-call-{}", inner.next_call_id)
    }
}

fn script_of(response: FauxResponse) -> FauxScript {
    match response {
        FauxResponse::Message(message) => FauxScript::Message(message),
        FauxResponse::Failure(message) => FauxScript::Failure(message),
    }
}

fn provider_failure(message: impl Into<String>) -> AiError {
    AiError::Provider(ProviderError {
        code: Some("faux_error".to_owned()),
        kind: Some("error".to_owned()),
        message: message.into(),
        request_id: None,
    })
}

fn deferred_events(model: &Model, handle: DeferredHandle) -> Vec<Result<StreamEvent, AiError>> {
    let response = Response {
        message: AssistantMessage {
            content: Vec::new(),
            model: model.spec.id.clone(),
            protocol: model.spec.protocol,
        },
        stop_reason: StopReason::Deferred,
        usage: Usage::default(),
        cost: None,
        response_id: None,
        responses_output: None,
        deferred: Some(handle),
        diagnostics: Vec::new(),
    };
    vec![
        Ok(StreamEvent::Started { response_id: None }),
        Ok(StreamEvent::Finished(response)),
    ]
}

fn message_events(
    model: &Model,
    tools: &[crate::types::ToolDef],
    mut message: FauxMessage,
    provider_call_id: String,
) -> Vec<Result<StreamEvent, AiError>> {
    let mut assembler = match CanonicalStreamAssembler::new(
        model.spec.id.clone(),
        model.spec.protocol,
        model.spec.pricing.clone(),
        tools,
    ) {
        Ok(assembler) => assembler,
        Err(error) => return vec![Err(error)],
    };
    let mut events: Vec<Result<StreamEvent, AiError>> = Vec::new();
    // The assembler owns the canonical response and rejects every part event
    // that arrives before `Started`, exactly like a real host transport; the
    // consumer must still observe each accepted event, so one event is both
    // recorded on the assembler and exposed on the stream.
    let push = |assembler: &mut CanonicalStreamAssembler,
                events: &mut Vec<Result<StreamEvent, AiError>>,
                event: StreamEvent| {
        match assembler.push(event.clone()) {
            Ok(()) => events.push(Ok(event)),
            Err(error) => events.push(Err(error)),
        }
    };
    push(
        &mut assembler,
        &mut events,
        StreamEvent::Started {
            response_id: message.response_id.clone(),
        },
    );
    let mut index = 0usize;
    if let Some(reasoning) = message.reasoning.take() {
        push(
            &mut assembler,
            &mut events,
            StreamEvent::ReasoningStart { index },
        );
        push(
            &mut assembler,
            &mut events,
            StreamEvent::ReasoningDelta {
                index,
                delta: reasoning,
            },
        );
        push(
            &mut assembler,
            &mut events,
            StreamEvent::ReasoningEnd { index },
        );
        index += 1;
    }
    if !message.text.is_empty() {
        push(
            &mut assembler,
            &mut events,
            StreamEvent::TextStart { index },
        );
        push(
            &mut assembler,
            &mut events,
            StreamEvent::TextDelta {
                index,
                delta: message.text.clone(),
            },
        );
        push(&mut assembler, &mut events, StreamEvent::TextEnd { index });
        index += 1;
    }
    for (offset, tool_call) in message.tool_calls.iter().enumerate() {
        let id = if tool_call.id.is_empty() {
            format!("{provider_call_id}-{offset}")
        } else {
            tool_call.id.clone()
        };
        push(
            &mut assembler,
            &mut events,
            StreamEvent::ToolCallStart {
                index,
                id: ToolCallId(id),
                name: tool_call.name.clone(),
            },
        );
        let encoded = match serde_json::to_string(&tool_call.arguments) {
            Ok(encoded) => encoded,
            Err(error) => {
                events.push(Err(AiError::Decode(crate::error::DecodeError::Json(
                    error.to_string(),
                ))));
                return events;
            }
        };
        push(
            &mut assembler,
            &mut events,
            StreamEvent::ToolCallArgsDelta {
                index,
                delta: encoded,
            },
        );
        push(
            &mut assembler,
            &mut events,
            StreamEvent::ToolCallEnd {
                index,
                argument_error: None,
            },
        );
        index += 1;
    }
    if message.usage != Usage::default() {
        push(
            &mut assembler,
            &mut events,
            StreamEvent::Usage(message.usage),
        );
    }
    let stop_reason = match message.stop_reason {
        Some(reason) => reason,
        None if !message.tool_calls.is_empty() => StopReason::ToolUse,
        None => StopReason::EndTurn,
    };
    match assembler.finish(stop_reason) {
        Ok(response) => events.push(Ok(StreamEvent::Finished(response))),
        Err(error) => events.push(Err(error)),
    }
    events
}

fn script_events(
    model: &Model,
    tools: &[crate::types::ToolDef],
    script: FauxScript,
    provider_call_id: String,
) -> Vec<Result<StreamEvent, AiError>> {
    match script {
        FauxScript::Message(message) => message_events(model, tools, message, provider_call_id),
        FauxScript::Failure(message) => vec![Err(provider_failure(message))],
    }
}

#[async_trait]
impl HostStreamTransport for FauxProvider {
    async fn stream(
        &self,
        model: HostStreamModel,
        request: Request,
        _diagnostics: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        let _ = model;
        let (script, provider_call_id) = {
            let mut inner = self.lock();
            inner.call_count += 1;
            let script = inner
                .scripts
                .pop_front()
                .ok_or_else(|| provider_failure("no more faux responses queued"))?;
            (script, FauxProvider::next_provider_call_id(&mut inner))
        };
        let events = script_events(&self.model, &request.tools, script, provider_call_id);
        Ok(Box::pin(futures_util::stream::iter(events)))
    }

    async fn submit_deferred(
        &self,
        _model: HostStreamModel,
        request: Request,
        _diagnostics: Vec<Diagnostic>,
        poll_after_ms: Option<u64>,
    ) -> Result<ResponseStream, AiError> {
        let _ = request;
        let mut inner = self.lock();
        inner.deferred_submission_count += 1;
        let script = inner
            .scripts
            .pop_front()
            .ok_or_else(|| provider_failure("no more faux responses queued"))?;
        let provider = inner.options.provider.clone();
        let model_id = inner.options.model_id.0.clone();
        let api = inner.options.api.clone();
        let default_poll_after_ms = inner.options.poll_after_ms;
        let pending_fetches = inner.options.pending_fetches;
        let handle = DeferredHandle {
            provider,
            model_id,
            api,
            id: FauxProvider::next_provider_call_id(&mut inner),
            expires_at_ms: None,
            poll_after_ms: poll_after_ms.or(default_poll_after_ms),
            data: None,
        };
        inner.deferred.insert(
            handle.id.clone(),
            FauxDeferredEntry {
                handle: handle.clone(),
                script,
                pending_fetches,
                cancelled: false,
                resolved: None,
            },
        );
        drop(inner);
        Ok(Box::pin(futures_util::stream::iter(deferred_events(
            &self.model,
            handle,
        ))))
    }

    async fn fetch_deferred(
        &self,
        _model: HostStreamModel,
        handle: DeferredHandle,
        _wait_ms: Option<u64>,
    ) -> Result<ResponseStream, AiError> {
        let mut inner = self.lock();
        inner.deferred_fetch_count += 1;
        let entry = inner.deferred.get_mut(&handle.id).ok_or_else(|| {
            provider_failure(format!("unknown faux deferred response: {}", handle.id))
        })?;
        if entry.handle.provider != handle.provider
            || entry.handle.model_id != handle.model_id
            || entry.handle.api != handle.api
        {
            return Err(provider_failure(format!(
                "faux deferred handle identity mismatch: {}",
                handle.id
            )));
        }
        if entry.cancelled {
            return Err(provider_failure(format!(
                "faux deferred response was cancelled: {}",
                handle.id
            )));
        }
        if entry.pending_fetches > 0 {
            entry.pending_fetches -= 1;
            let stored = entry.handle.clone();
            drop(inner);
            return Ok(Box::pin(futures_util::stream::iter(deferred_events(
                &self.model,
                stored,
            ))));
        }
        if entry.resolved.is_none() {
            entry.resolved = Some(match &entry.script {
                FauxScript::Message(message) => Ok(message.clone()),
                FauxScript::Failure(message) => Err(message.clone()),
            });
        }
        let provider_call_id = format!("{}-poll", handle.id);
        let resolved = entry.resolved.clone().expect("resolved just above");
        drop(inner);
        let events = match resolved {
            Ok(message) => message_events(&self.model, &[], message, provider_call_id),
            Err(message) => vec![Err(provider_failure(message))],
        };
        Ok(Box::pin(futures_util::stream::iter(events)))
    }

    async fn cancel_deferred(
        &self,
        _model: HostStreamModel,
        handle: DeferredHandle,
    ) -> Result<(), AiError> {
        let mut inner = self.lock();
        inner.cancelled_deferred.push(handle.clone());
        if let Some(entry) = inner.deferred.get_mut(&handle.id) {
            entry.cancelled = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferred_status_requires_an_owned_handle() {
        let provider = FauxProvider::new(FauxOptions {
            pending_fetches: 1,
            ..FauxOptions::default()
        });
        provider.set_responses(vec![
            FauxResponse::Message(FauxMessage::new("ready")),
            FauxResponse::Failure("boom".to_owned()),
        ]);
        assert_eq!(provider.pending_response_count(), 2);

        // No deferred submission happened yet, so no handle is known.
        let handle = DeferredHandle::new("faux", "faux-1", "faux", "faux-call-1");
        assert_eq!(provider.deferred_status(&handle), None);

        let state = provider.state();
        assert_eq!(state.call_count, 0);
        assert_eq!(state.deferred_submission_count, 0);
        assert_eq!(state.deferred_fetch_count, 0);
        assert!(state.cancelled_deferred.is_empty());
    }
}
