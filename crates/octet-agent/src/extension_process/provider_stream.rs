//! Model provider streams served by an extension.

use super::*;

pub(super) struct ExtensionProviderStreamTransport {
    pub(super) process: Weak<ExtensionProcessInner>,
    pub(super) binding: ProviderRouteBinding,
}

pub(super) enum ProviderRouteBinding {
    Projected(crate::extension_provider::ExtensionProviderRoute),
    Prepared {
        captured: crate::extension_provider::ExtensionProviderRoute,
        selected: octet_ai::HostStreamModel,
    },
}

pub(super) struct ProviderStreamCancellation {
    pub(super) connection: Weak<ProcessConnection>,
    pub(super) stream_id: String,
    pub(super) armed: bool,
}

impl ProviderStreamCancellation {
    pub(super) fn new(connection: &Arc<ProcessConnection>, stream_id: String) -> Self {
        Self {
            connection: Arc::downgrade(connection),
            stream_id,
            armed: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProviderStreamCancellation {
    fn drop(&mut self) {
        if self.armed {
            if let Some(connection) = self.connection.upgrade() {
                connection.cancel_provider_stream(&self.stream_id, "caller dropped stream");
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderStartedPayload {
    #[serde(default)]
    pub(super) response_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderIndexPayload {
    pub(super) index: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderDeltaPayload {
    pub(super) index: usize,
    pub(super) delta: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderToolCallStartPayload {
    pub(super) index: usize,
    pub(super) id: String,
    pub(super) name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderUsagePayload {
    #[serde(default)]
    pub(super) input_tokens: u64,
    #[serde(default)]
    pub(super) cache_read_tokens: u64,
    #[serde(default)]
    pub(super) cache_write_tokens: u64,
    #[serde(default)]
    pub(super) cache_write_1h_tokens: u64,
    #[serde(default)]
    pub(super) output_tokens: u64,
    #[serde(default)]
    pub(super) reasoning_tokens: u64,
    #[serde(default)]
    pub(super) total_tokens: u64,
}

impl From<ProviderUsagePayload> for Usage {
    fn from(value: ProviderUsagePayload) -> Self {
        Self {
            input_tokens: value.input_tokens,
            cache_read_tokens: value.cache_read_tokens,
            cache_write_tokens: value.cache_write_tokens,
            cache_write_1h_tokens: value.cache_write_1h_tokens,
            output_tokens: value.output_tokens,
            reasoning_tokens: value.reasoning_tokens,
            total_tokens: value.total_tokens,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderFinishedPayload {
    pub(super) stop_reason: StopReason,
}

pub(super) enum DecodedProviderStreamEvent {
    Emit(Box<StreamEvent>),
    Finish(StopReason),
    Heartbeat,
    Error,
}

impl DecodedProviderStreamEvent {
    pub(super) fn emit(event: StreamEvent) -> Self {
        Self::Emit(Box::new(event))
    }
}

pub(super) enum ProviderStreamWait {
    Event(Option<api_v03::ProviderStreamEvent>),
    Idle,
    Deadline,
    RouteInvalidated,
}

pub(super) fn invalid_provider_stream_event() -> AiError {
    AiError::StreamProtocol(octet_ai::StreamProtocolError::UnexpectedEvent(
        "invalid extension provider stream event".to_owned(),
    ))
}

pub(super) fn decode_provider_stream_payload<T: DeserializeOwned>(
    payload: serde_json::Value,
) -> Result<T, AiError> {
    serde_json::from_value(payload).map_err(|_| invalid_provider_stream_event())
}

pub(super) fn decode_provider_stream_event(
    event: api_v03::ProviderStreamEvent,
) -> Result<DecodedProviderStreamEvent, AiError> {
    let payload = event.payload;
    match event.kind.as_str() {
        "started" => {
            let payload: ProviderStartedPayload = decode_provider_stream_payload(payload)?;
            Ok(DecodedProviderStreamEvent::emit(StreamEvent::Started {
                response_id: payload.response_id,
            }))
        }
        "text_start" => {
            let payload: ProviderIndexPayload = decode_provider_stream_payload(payload)?;
            Ok(DecodedProviderStreamEvent::emit(StreamEvent::TextStart {
                index: payload.index,
            }))
        }
        "text_delta" => {
            let payload: ProviderDeltaPayload = decode_provider_stream_payload(payload)?;
            Ok(DecodedProviderStreamEvent::emit(StreamEvent::TextDelta {
                index: payload.index,
                delta: payload.delta,
            }))
        }
        "text_end" => {
            let payload: ProviderIndexPayload = decode_provider_stream_payload(payload)?;
            Ok(DecodedProviderStreamEvent::emit(StreamEvent::TextEnd {
                index: payload.index,
            }))
        }
        "reasoning_start" => {
            let payload: ProviderIndexPayload = decode_provider_stream_payload(payload)?;
            Ok(DecodedProviderStreamEvent::emit(
                StreamEvent::ReasoningStart {
                    index: payload.index,
                },
            ))
        }
        "reasoning_delta" => {
            let payload: ProviderDeltaPayload = decode_provider_stream_payload(payload)?;
            Ok(DecodedProviderStreamEvent::emit(
                StreamEvent::ReasoningDelta {
                    index: payload.index,
                    delta: payload.delta,
                },
            ))
        }
        "reasoning_end" => {
            let payload: ProviderIndexPayload = decode_provider_stream_payload(payload)?;
            Ok(DecodedProviderStreamEvent::emit(
                StreamEvent::ReasoningEnd {
                    index: payload.index,
                },
            ))
        }
        "tool_call_start" => {
            let payload: ProviderToolCallStartPayload = decode_provider_stream_payload(payload)?;
            if payload.id.is_empty() || payload.name.is_empty() {
                return Err(invalid_provider_stream_event());
            }
            Ok(DecodedProviderStreamEvent::emit(
                StreamEvent::ToolCallStart {
                    async_execution: false,
                    index: payload.index,
                    id: ToolCallId(payload.id),
                    name: payload.name,
                },
            ))
        }
        "tool_call_args_delta" => {
            let payload: ProviderDeltaPayload = decode_provider_stream_payload(payload)?;
            Ok(DecodedProviderStreamEvent::emit(
                StreamEvent::ToolCallArgsDelta {
                    index: payload.index,
                    delta: payload.delta,
                },
            ))
        }
        "tool_call_end" => {
            let payload: ProviderIndexPayload = decode_provider_stream_payload(payload)?;
            Ok(DecodedProviderStreamEvent::emit(StreamEvent::ToolCallEnd {
                index: payload.index,
                argument_error: None,
            }))
        }
        "usage" => {
            let payload: ProviderUsagePayload = decode_provider_stream_payload(payload)?;
            Ok(DecodedProviderStreamEvent::emit(StreamEvent::Usage(
                payload.into(),
            )))
        }
        "finished" => {
            let payload: ProviderFinishedPayload = decode_provider_stream_payload(payload)?;
            Ok(DecodedProviderStreamEvent::Finish(payload.stop_reason))
        }
        "heartbeat" => Ok(DecodedProviderStreamEvent::Heartbeat),
        "error" => Ok(DecodedProviderStreamEvent::Error),
        _ => Err(invalid_provider_stream_event()),
    }
}

pub(super) fn provider_transport_error(
    phase: TransportPhase,
    timeout: bool,
    message: &'static str,
) -> AiError {
    AiError::Transport(TransportError {
        phase,
        timeout,
        message: message.to_owned(),
    })
}

pub(super) fn provider_unavailable_error() -> AiError {
    AiError::Provider(ProviderError {
        code: None,
        kind: Some("extension_provider".to_owned()),
        message: "extension provider is unavailable".to_owned(),
        request_id: None,
    })
}

pub(super) fn provider_protocol_name(protocol: Protocol) -> Option<&'static str> {
    match protocol {
        Protocol::OpenAiChat => Some("openai_chat"),
        Protocol::OpenAiResponses => Some("openai_responses"),
        Protocol::AnthropicMessages => Some("anthropic_messages"),
        Protocol::GoogleGenerativeAi => Some("google_generative_ai"),
        Protocol::BedrockConverse | Protocol::MistralConversations | Protocol::PiMessages => None,
    }
}

pub(super) fn extension_provider_response_stream(
    connection: Arc<ProcessConnection>,
    stream_id: String,
    mut receiver: mpsc::Receiver<api_v03::ProviderStreamEvent>,
    model: HostStreamModel,
    request: octet_ai::Request,
    diagnostics: Vec<Diagnostic>,
    route: (
        Arc<ExtensionProviderRegistry>,
        crate::extension_provider::ExtensionProviderRoute,
    ),
) -> ResponseStream {
    // Own cancellation before the first poll: dropping a successfully opened
    // but never-polled stream must still release the accepted provider job.
    let mut cancellation = ProviderStreamCancellation::new(&connection, stream_id.clone());
    Box::pin(async_stream::try_stream! {
        let mut assembler = CanonicalStreamAssembler::new(
            model.id,
            model.protocol,
            model.pricing,
            &request.tools,
        )?;
        assembler.add_host_diagnostics(diagnostics);
        let started_at = tokio::time::Instant::now();
        let deadline = started_at + connection.provider_stream_deadline;
        let mut last_event_at = started_at;

        loop {
            if !connection_is_usable(&connection) || !route.0.route_is_active(&route.1) {
                Err(provider_unavailable_error())?;
            }
            let idle_deadline = last_event_at + connection.provider_stream_idle_timeout;
            let waiting = tokio::select! {
                _ = tokio::time::sleep_until(deadline) => ProviderStreamWait::Deadline,
                _ = tokio::time::sleep_until(idle_deadline) => ProviderStreamWait::Idle,
                event = receiver.recv() => ProviderStreamWait::Event(event),
                _ = route.0.route_invalidated(&route.1) => ProviderStreamWait::RouteInvalidated,
            };
            let event = match waiting {
                ProviderStreamWait::RouteInvalidated => Err(provider_unavailable_error())?,
                ProviderStreamWait::Deadline => Err(provider_transport_error(
                    TransportPhase::Body,
                    true,
                    "extension provider stream deadline exceeded",
                ))?,
                ProviderStreamWait::Idle => Err(provider_transport_error(
                    TransportPhase::Body,
                    true,
                    "extension provider stream idle timeout exceeded",
                ))?,
                ProviderStreamWait::Event(Some(event)) => event,
                ProviderStreamWait::Event(None) => {
                    Err(AiError::StreamProtocol(octet_ai::StreamProtocolError::MissingFinish))?
                }
            };
            if !connection_is_usable(&connection) || !route.0.route_is_active(&route.1) {
                Err(provider_unavailable_error())?;
            }
            last_event_at = tokio::time::Instant::now();
            assembler.observe_transport_event()?;
            match decode_provider_stream_event(event)? {
                DecodedProviderStreamEvent::Emit(event) => {
                    let event = *event;
                    assembler.push(event.clone())?;
                    yield event;
                }
                DecodedProviderStreamEvent::Finish(stop_reason) => {
                    let mut response = assembler.finish(stop_reason)?;
                    let delegate = lock_std_mutex(&connection.provider_streams).get(&stream_id).and_then(|ingress| ingress.delegate.clone());
                    if let Some(delegate) = delegate.filter(|delegate| delegate.used.load(Ordering::Acquire)) {
                        let native = { lock_std_mutex(&delegate.response).take() };
                        let mut native = native.ok_or_else(invalid_provider_stream_event)?;
                        let expected = native.usage;
                        let actual = response.usage;
                        if (expected.input_tokens, expected.output_tokens, expected.cache_read_tokens, expected.cache_write_tokens, expected.total_tokens)
                            != (actual.input_tokens, actual.output_tokens, actual.cache_read_tokens, actual.cache_write_tokens, actual.total_tokens) {
                            Err(invalid_provider_stream_event())?;
                        }
                        for (index, part) in native.message.content.iter().enumerate() {
                            if let octet_ai::AssistantPart::Reasoning(reasoning) = part {
                                if let Some(state) = &reasoning.state {
                                    let expected = if matches!(state.kind, octet_ai::ReasoningStateKind::AnthropicRedacted { .. }) { Some("[Reasoning redacted]") } else { reasoning.text.as_deref().or(Some("")) };
                                    if !matches!(response.message.content.get(index), Some(octet_ai::AssistantPart::Reasoning(actual)) if actual.text.as_deref() == expected && actual.state.is_none()) {
                                        Err(invalid_provider_stream_event())?;
                                    }
                                    response.message.content[index] = part.clone();
                                }
                            }
                        }
                        native.message = response.message;
                        native.diagnostics.extend(response.diagnostics);
                        response = native;
                    }
                    connection.settle_provider_stream(&stream_id);
                    cancellation.disarm();
                    yield StreamEvent::Finished(response);
                    return;
                }
                DecodedProviderStreamEvent::Heartbeat => {}
                DecodedProviderStreamEvent::Error => {
                    Err(AiError::Provider(ProviderError {
                        code: None,
                        kind: Some("extension_provider".to_owned()),
                        message: "extension provider reported an error".to_owned(),
                        request_id: None,
                    }))?
                }
            }
        }
    })
}

#[async_trait::async_trait]
impl HostStreamTransport for ExtensionProviderStreamTransport {
    fn custom_http_model(&self, selected: &octet_ai::Model) -> Option<octet_ai::Model> {
        let captured = match &self.binding {
            ProviderRouteBinding::Projected(route) => route,
            ProviderRouteBinding::Prepared { captured, .. } => captured,
        };
        let metadata = captured.model_metadata.as_ref()?;
        if metadata.inherit_provider.is_some() || metadata.base_url.is_empty() {
            return None;
        }
        let process = self.process.upgrade()?;
        let connection = read_std_lock(&process.connection).clone();
        if !connection_is_usable(&connection) || captured.owner != connection.provider_owner {
            return None;
        }
        if !connection
            .provider_registry
            .as_ref()?
            .route_is_active(captured)
        {
            return None;
        }
        let mut base_url = url::Url::parse(&metadata.base_url).ok()?;
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }
        let id = octet_ai::EndpointId(format!("{}/custom-http", selected.endpoint.id.0));
        let mut model = selected.clone();
        Arc::make_mut(&mut model.spec).endpoint = id.clone();
        model.endpoint = Arc::new(octet_ai::Endpoint {
            id,
            base_url,
            auth: octet_ai::Auth::None,
            default_headers: reqwest::header::HeaderMap::new(),
            transport: octet_ai::EndpointTransport::Http,
            runtime: Default::default(),
            timeout: selected.endpoint.timeout,
        });
        Some(model)
    }

    fn native_model(&self) -> Option<octet_ai::Model> {
        let captured = match &self.binding {
            ProviderRouteBinding::Projected(route) => route,
            ProviderRouteBinding::Prepared { captured, .. } => captured,
        };
        captured
            .model_metadata
            .as_ref()?
            .inherit_provider
            .as_ref()?;
        let process = self.process.upgrade()?;
        let connection = read_std_lock(&process.connection).clone();
        if !connection_is_usable(&connection) || captured.owner != connection.provider_owner {
            return None;
        }
        let registry = connection.provider_registry.as_ref()?;
        if !registry.route_is_active(captured) {
            return None;
        }
        registry
            .native_catalog()?
            .model(&captured.provider.id, &captured.model.api_name)
    }

    async fn stream(
        &self,
        model: HostStreamModel,
        request: octet_ai::Request,
        diagnostics: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        self.stream_inner(model, request, diagnostics, None).await
    }

    async fn stream_native(
        &self,
        model: HostStreamModel,
        request: octet_ai::Request,
        diagnostics: Vec<Diagnostic>,
        continuation: octet_ai::NativeRequestContinuation,
    ) -> Result<ResponseStream, AiError> {
        self.stream_inner(model, request, diagnostics, Some(continuation))
            .await
    }
}

impl ExtensionProviderStreamTransport {
    async fn stream_inner(
        &self,
        model: HostStreamModel,
        request: octet_ai::Request,
        diagnostics: Vec<Diagnostic>,
        mut continuation: Option<octet_ai::NativeRequestContinuation>,
    ) -> Result<ResponseStream, AiError> {
        let process = self
            .process
            .upgrade()
            .ok_or_else(provider_unavailable_error)?;
        let connection = read_std_lock(&process.connection).clone();
        {
            let protocol = read_std_lock(&connection.protocol);
            if protocol.version != EXTENSION_API_VERSION_0_3
                && (protocol.version != EXTENSION_API_VERSION_0_4
                    || !protocol.supports("provider_proxy_v1"))
            {
                return Err(provider_unavailable_error());
            }
        }
        if !connection_is_usable(&connection)
            || connection
                .require_api_v03_host_method(methods::PROVIDER_STREAM)
                .is_err()
        {
            return Err(provider_unavailable_error());
        }
        let Some(registry) = connection.provider_registry.clone() else {
            return Err(provider_unavailable_error());
        };
        let (projected, mut route) = match &self.binding {
            ProviderRouteBinding::Projected(projected) => {
                let route = registry
                    .resolve(&projected.provider.id, &projected.model.id)
                    .ok_or_else(provider_unavailable_error)?;
                (projected, route)
            }
            ProviderRouteBinding::Prepared { captured, selected } => {
                if model.id != selected.id
                    || model.protocol != selected.protocol
                    || model.pricing != selected.pricing
                {
                    return Err(provider_unavailable_error());
                }
                (captured, captured.clone())
            }
        };
        let Some(provider_protocol) = provider_protocol_name(model.protocol) else {
            return Err(provider_unavailable_error());
        };
        if route.owner != connection.provider_owner
            || route.owner != projected.owner
            || route.provider != projected.provider
            || route.model != projected.model
            || route.model_metadata != projected.model_metadata
            || route.model.protocol != provider_protocol
        {
            return Err(provider_unavailable_error());
        }

        let stream_sequence = connection
            .next_provider_stream_id
            .fetch_add(1, Ordering::AcqRel);
        if stream_sequence == u64::MAX {
            return Err(provider_transport_error(
                TransportPhase::ResponseHeaders,
                false,
                "extension provider stream identifier space is exhausted",
            ));
        }
        let stream_id = format!("provider-{}-{stream_sequence}", connection.generation);
        let request_value = serde_json::to_value(&request).map_err(|_| {
            provider_transport_error(
                TransportPhase::ResponseHeaders,
                false,
                "extension provider request could not be serialized",
            )
        })?;
        if api_v03::canonical_json(&request_value).is_err() {
            return Err(provider_transport_error(
                TransportPhase::ResponseHeaders,
                false,
                "extension provider request is not canonical API 0.3 JSON",
            ));
        }
        let params = api_v03::ProviderStreamRequest {
            stream_id: stream_id.clone(),
            provider_id: route.provider.id.clone(),
            model_id: route.model.id.clone(),
            request: request_value,
            authorization_lease: None,
        };
        let params = serde_json::to_value(params).map_err(|_| {
            provider_transport_error(
                TransportPhase::ResponseHeaders,
                false,
                "extension provider request could not be encoded",
            )
        })?;
        if api_v03::parse_provider_stream_request(params.clone()).is_err() {
            return Err(provider_transport_error(
                TransportPhase::ResponseHeaders,
                false,
                "extension provider request was rejected by the API contract",
            ));
        }
        let (sender, receiver) = mpsc::channel(connection.provider_stream_buffer);
        {
            let mut streams = lock_std_mutex(&connection.provider_streams);
            if streams.contains_key(&stream_id) {
                return Err(provider_transport_error(
                    TransportPhase::ResponseHeaders,
                    false,
                    "extension provider stream identifier collision",
                ));
            }
            streams.insert(
                stream_id.clone(),
                ProviderStreamIngress {
                    delegate: if continuation.is_some() {
                        let custom_http = route.model_metadata.as_ref().is_some_and(|metadata| {
                            metadata.inherit_provider.is_none() && !metadata.base_url.is_empty()
                        });
                        Some(Arc::new(native_provider::NativeDelegate {
                            continuation: StdMutex::new(Some(
                                continuation.take().ok_or_else(provider_unavailable_error)?,
                            )),
                            max_output_tokens: route.model.max_output_tokens as u64,
                            custom_http,
                            original: request.clone(),
                            connection: Arc::downgrade(&connection),
                            route: route.clone(),
                            used: AtomicBool::new(false),
                            response: StdMutex::new(None),
                            cancelled: Arc::new(Notify::new()),
                        }))
                    } else {
                        None
                    },
                    sender,
                    next_sequence: 0,
                    terminal: false,
                },
            );
        }

        // Protect the acceptance wait too. Cancellation at an ambiguous
        // acceptance boundary never leaves an ingress/job without an owner.
        let mut opening_cancellation =
            ProviderStreamCancellation::new(&connection, stream_id.clone());
        let accepted = match connection
            .request_provider_stream(params, connection.provider_stream_idle_timeout, &mut route)
            .await
        {
            Ok(value) => api_v03::parse_provider_stream_accepted(value).map_err(|_| {
                connection.cancel_provider_stream(&stream_id, "invalid stream acceptance");
                provider_transport_error(
                    TransportPhase::ResponseHeaders,
                    false,
                    "extension provider returned an invalid stream acceptance",
                )
            })?,
            Err(_) => {
                connection.cancel_provider_stream(&stream_id, "stream request failed");
                return Err(provider_transport_error(
                    TransportPhase::ResponseHeaders,
                    false,
                    "extension provider stream request failed",
                ));
            }
        };
        if accepted.stream_id != stream_id {
            connection.cancel_provider_stream(&stream_id, "stream identifier mismatch");
            return Err(provider_transport_error(
                TransportPhase::ResponseHeaders,
                false,
                "extension provider returned a mismatched stream acceptance",
            ));
        }
        if !accepted.accepted {
            connection.settle_provider_stream(&stream_id);
            return Err(provider_unavailable_error());
        }
        // The extension may have spent arbitrary time in setup before it
        // accepted. Do not turn that stale acceptance into a response stream if
        // its declaration, owner generation, authorization, or connection was
        // replaced while the request was in flight.
        if !connection_is_usable(&connection) || !registry.route_is_active(&route) {
            connection
                .cancel_provider_stream(&stream_id, "provider route changed before admission");
            return Err(provider_unavailable_error());
        }

        opening_cancellation.disarm();
        Ok(extension_provider_response_stream(
            connection,
            stream_id,
            receiver,
            model,
            request,
            diagnostics,
            (registry, route),
        ))
    }
}

pub(super) struct ArtifactGenerationGuard {
    pub(super) store: ArtifactStore,
    pub(super) generation: u64,
    pub(super) armed: bool,
}

impl ArtifactGenerationGuard {
    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ArtifactGenerationGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.store.settle_generation(self.generation);
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum ProviderHostResponseError {
    Invalid,
    ResourceExhausted,
    Unavailable,
}

pub(super) fn provider_registry_for_request(
    state: &ProtocolReadState,
) -> Result<Arc<ExtensionProviderRegistry>, ProviderHostResponseError> {
    {
        let protocol = read_std_lock(&state.protocol);
        if protocol.version != EXTENSION_API_VERSION_0_3
            && (protocol.version != EXTENSION_API_VERSION_0_4
                || !protocol.supports("provider_proxy_v1"))
        {
            return Err(ProviderHostResponseError::Unavailable);
        }
    }
    state
        .provider_registry
        .clone()
        .ok_or(ProviderHostResponseError::Unavailable)
}

pub(super) fn provider_registry_response_error(
    error: ExtensionProviderRegistryError,
) -> ProviderHostResponseError {
    match error {
        ExtensionProviderRegistryError::ResourceExhausted(_) => {
            ProviderHostResponseError::ResourceExhausted
        }
        ExtensionProviderRegistryError::Invalid(_)
        | ExtensionProviderRegistryError::StaleOwner
        | ExtensionProviderRegistryError::ProviderConflict => ProviderHostResponseError::Invalid,
    }
}

pub(super) fn provider_catalog_response_value(
    result: api_v03::ProviderCatalogResult,
) -> Result<serde_json::Value, ProviderHostResponseError> {
    let value = serde_json::to_value(result).map_err(|_| ProviderHostResponseError::Unavailable)?;
    api_v03::parse_provider_catalog_result(value.clone())
        .map_err(|_| ProviderHostResponseError::Unavailable)?;
    Ok(value)
}

pub(super) fn provider_authorization_response_value(
    result: api_v03::ProviderAuthorizationResult,
) -> Result<serde_json::Value, ProviderHostResponseError> {
    let value = serde_json::to_value(result).map_err(|_| ProviderHostResponseError::Unavailable)?;
    api_v03::parse_provider_authorization_result(value.clone())
        .map_err(|_| ProviderHostResponseError::Unavailable)?;
    Ok(value)
}

pub(super) fn queue_provider_host_response(
    state: &ProtocolReadState,
    id: &ExtensionRequestId,
    result: Result<serde_json::Value, ProviderHostResponseError>,
) -> Result<(), String> {
    let response = match result {
        Ok(result) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }),
        Err(error) => {
            let name = match error {
                ProviderHostResponseError::Invalid => "invalid_params",
                ProviderHostResponseError::ResourceExhausted => "resource_exhausted",
                ProviderHostResponseError::Unavailable => "capability_mismatch",
            };
            let error = api_v03::error_object(name, None)
                .map_err(|error| format!("invalid API 0.3 provider error response: {error}"))?;
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": error,
            })
        }
    };
    let queued = queue_writer_value(&state.writer, &state.frame_limit, response);
    settle_child_request(&state.child_requests, id);
    queued
}

pub(super) fn provider_stream_is_terminal(kind: &str) -> bool {
    matches!(kind, "finished" | "error")
}

pub(super) fn queue_provider_stream_cancel_from_reader(
    state: &ProtocolReadState,
    stream_id: String,
    reason: &'static str,
) -> Result<(), String> {
    let params = api_v03::ProviderStreamCancelParams {
        stream_id,
        reason: Some(reason.to_owned()),
    };
    let params = serde_json::to_value(params)
        .map_err(|error| format!("cannot encode provider stream cancellation: {error}"))?;
    api_v03::parse_provider_stream_cancel_params(params.clone())
        .map_err(|error| format!("invalid provider stream cancellation: {error}"))?;
    queue_writer_value(
        &state.writer,
        &state.frame_limit,
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": methods::PROVIDER_CANCEL,
            "params": params,
        }),
    )
}

pub(super) fn dispatch_provider_stream_event(
    state: &ProtocolReadState,
    event: api_v03::ProviderStreamEvent,
) -> Result<(), String> {
    provider_registry_for_request(state)
        .map_err(|_| "provider stream service is unavailable".to_owned())?;
    let payload = api_v03::canonical_json(&event.payload)
        .map_err(|error| format!("invalid provider stream payload: {error}"))?;
    if payload.len() > api_v03::MAX_PROVIDER_STREAM_EVENT_BYTES {
        return Err(format!(
            "provider stream payload exceeded {} bytes",
            api_v03::MAX_PROVIDER_STREAM_EVENT_BYTES
        ));
    }
    let stream_id = event.stream_id.clone();
    let terminal = provider_stream_is_terminal(&event.kind);
    let mut cancel = false;
    {
        let mut streams = lock_std_mutex(&state.provider_streams);
        let ingress = streams
            .get_mut(&stream_id)
            .ok_or_else(|| "provider stream event references an unknown stream".to_owned())?;
        if ingress.terminal {
            return Err("provider stream event arrived after a terminal event".into());
        }
        if event.sequence != ingress.next_sequence {
            return Err("provider stream event sequence is not contiguous".into());
        }
        if ingress.next_sequence >= api_v03::MAX_PROVIDER_STREAM_EVENTS {
            return Err("provider stream event limit exceeded".into());
        }
        match ingress.sender.try_send(event) {
            Ok(()) => {
                ingress.next_sequence = ingress
                    .next_sequence
                    .checked_add(1)
                    .ok_or_else(|| "provider stream event sequence overflowed".to_owned())?;
                ingress.terminal = terminal;
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_))
            | Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                cancel = true;
            }
        }
        if cancel {
            streams.remove(&stream_id);
        }
    }
    if cancel {
        let _ = state.events.send(ExtensionEvent::Diagnostic {
            message: "extension provider stream exceeded host backpressure capacity".into(),
        });
        queue_provider_stream_cancel_from_reader(
            state,
            stream_id,
            "host provider stream backpressure",
        )?;
    }
    Ok(())
}

pub(super) fn queue_catalog_update(
    state: &ProtocolReadState,
    request_id: ExtensionRequestId,
    mutation: CatalogMutation,
) -> Result<(), String> {
    let _registered = insert_child_request(state, request_id.clone(), None, None)?;
    let request = CatalogUpdateRequest {
        request_id: request_id.clone(),
        generation: state.generation,
        mutation,
        catalog: Arc::clone(&state.tool_catalog),
        writer: state.writer.clone(),
        child_requests: Arc::clone(&state.child_requests),
        max_message_bytes: state.max_message_bytes(),
    };
    if state.catalog_updates.try_send(request).is_err() {
        settle_child_request(&state.child_requests, &request_id);
        queue_writer_value(
            &state.writer,
            &state.frame_limit,
            serde_json::json!({
                "jsonrpc":"2.0",
                "id":request_id,
                "error":{
                    "code":-32000,
                    "message":"host tool catalog update queue is full",
                },
            }),
        )?;
    }
    Ok(())
}

pub(super) fn reject_unparented_child_request(
    state: &ProtocolReadState,
    request_id: ExtensionRequestId,
    message: impl Into<String>,
) -> Result<(), String> {
    let message = message.into();
    let _registered = insert_child_request(state, request_id.clone(), None, None)?;
    let delivery = try_queue_child_response(
        &state.child_requests,
        &request_id,
        &state.writer,
        state.max_message_bytes(),
        serde_json::json!({
            "jsonrpc":"2.0",
            "id":request_id,
            "error":{"code":-32602,"message":message},
        }),
    );
    if delivery.is_err() {
        settle_child_request(&state.child_requests, &request_id);
    }
    delivery.map(|_| ())
}

#[cfg(test)]
mod terminal_ingress_tests {
    use super::*;

    #[test]
    fn provider_terminal_ingress_retains_native_state_until_consumer_settlement() {
        let (events, _) = broadcast::channel(8);
        let (mut state, _frames) = super::super::tests::protocol_read_state_for_test(
            ManifestContributions::default(),
            events,
        );
        write_std_lock(&state.protocol).version = EXTENSION_API_VERSION_0_3.into();
        state.provider_registry = Some(Arc::new(ExtensionProviderRegistry::new()));
        let (sender, mut receiver) = mpsc::channel(1);
        lock_std_mutex(&state.provider_streams).insert(
            "terminal".into(),
            ProviderStreamIngress {
                delegate: None,
                sender,
                next_sequence: 0,
                terminal: false,
            },
        );
        let terminal = api_v03::ProviderStreamEvent {
            stream_id: "terminal".into(),
            sequence: 0,
            kind: "finished".into(),
            payload: serde_json::json!({"stop_reason":"stop"}),
        };
        dispatch_provider_stream_event(&state, terminal.clone()).unwrap();
        {
            let streams = lock_std_mutex(&state.provider_streams);
            let retained = streams
                .get("terminal")
                .expect("consumer still owns terminal state");
            assert!(retained.terminal);
            assert_eq!(retained.next_sequence, 1);
        }
        assert_eq!(receiver.try_recv().unwrap().kind, "finished");
        let mut extra = terminal;
        extra.sequence = 1;
        assert_eq!(
            dispatch_provider_stream_event(&state, extra).unwrap_err(),
            "provider stream event arrived after a terminal event"
        );
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn provider_terminal_backpressure_still_removes_ingress_and_cancels() {
        let (events, _) = broadcast::channel(8);
        let (mut state, mut frames) = super::super::tests::protocol_read_state_for_test(
            ManifestContributions::default(),
            events,
        );
        write_std_lock(&state.protocol).version = EXTENSION_API_VERSION_0_3.into();
        state.provider_registry = Some(Arc::new(ExtensionProviderRegistry::new()));
        let (sender, _receiver) = mpsc::channel(1);
        lock_std_mutex(&state.provider_streams).insert(
            "full".into(),
            ProviderStreamIngress {
                delegate: None,
                sender,
                next_sequence: 0,
                terminal: false,
            },
        );
        dispatch_provider_stream_event(
            &state,
            api_v03::ProviderStreamEvent {
                stream_id: "full".into(),
                sequence: 0,
                kind: "started".into(),
                payload: serde_json::json!({"response_id":"native"}),
            },
        )
        .unwrap();
        dispatch_provider_stream_event(
            &state,
            api_v03::ProviderStreamEvent {
                stream_id: "full".into(),
                sequence: 1,
                kind: "finished".into(),
                payload: serde_json::json!({"stop_reason":"stop"}),
            },
        )
        .unwrap();
        assert!(!lock_std_mutex(&state.provider_streams).contains_key("full"));
        let frame = frames.try_recv().unwrap();
        let cancellation: serde_json::Value = serde_json::from_slice(&frame.line).unwrap();
        assert_eq!(cancellation["method"], methods::PROVIDER_CANCEL);
        assert_eq!(cancellation["params"]["stream_id"], "full");
    }
}
