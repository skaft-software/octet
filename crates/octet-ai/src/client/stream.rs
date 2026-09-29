//! Opening a streaming request and draining it into a [`ResponseStream`].
//!
//! One prepared request, three ways out. [`stream_http`] sends it and inspects
//! the response; a Bedrock Converse endpoint hands the body to
//! [`bedrock_response_stream`], which decodes AWS event-stream frames; every
//! other streaming endpoint decodes SSE inline. All three share the same
//! failure annotation, the same body-read clocks, and the same redaction, so a
//! caller cannot tell them apart by its error handling.
//!
//! It is separate from `client` because this is where the async body actually
//! lives. `client` decides *what* to send; this module decides how a partially
//! received generation is turned back into events, and that concern has its own
//! invariants worth reading in one place — most importantly that a mid-stream
//! failure is annotated with how far the response had progressed, and that a
//! WebSocket fallback is only replay-safe when the generation request provably
//! never left.
//!
//! [`ResponseStream`]: crate::stream::ResponseStream

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_stream::try_stream;
use futures_util::StreamExt;

use super::diagnostics::{
    json_scalar_string, lifecycle_from_sse_comment, parse_provider_lifecycle,
    provider_error_from_success_body, sanitize_ai_error, LIFECYCLE_HEADER, LIFECYCLE_REQUEST_VALUE,
    MAX_PROVIDER_LIFECYCLE_EVENTS,
};
use super::transport::{
    annotate_stream_failure, next_body_chunk, prepare_request_body, request_open_transport_error,
    reqwest_transport_error, MAX_COMPLETED_BODY_BYTES, MAX_ERROR_BODY_DEADLINE,
    MAX_ERROR_BODY_IDLE_TIMEOUT, MAX_SUCCESS_ERROR_BODY_BYTES,
};
use crate::auth::CredentialRedactor;
use crate::catalog::Model;
use crate::error::{
    AiError, DecodeError, HttpError, StreamProtocolError, TransportError, TransportPhase,
};
use crate::runtime::HookModelContext;
use crate::stream::{ResponseBuilder, ResponseStream, StreamEvent};
use crate::types::{Protocol, ToolDef};
pub(super) struct HttpStreamRequest {
    pub(super) model: Model,
    pub(super) compatibility: crate::types::CompatibilityMode,
    pub(super) parts: crate::protocol::HttpRequestParts,
    pub(super) headers: http::HeaderMap,
    pub(super) requested_audio_format: Option<crate::types::AudioFormat>,
    pub(super) requested_service_tier: Option<crate::types::ServiceTier>,
    pub(super) tool_definitions: Vec<ToolDef>,
    pub(super) pre_send_diagnostics: Vec<crate::error::Diagnostic>,
    pub(super) buffer_ambiguous_compatibility_content: bool,
    pub(super) diagnostic_redactor: CredentialRedactor,
    /// Optional host hook observing the HTTP response before its body is read.
    pub(super) on_response: Option<Arc<dyn crate::runtime::ResponseHook>>,
}

/// Falling back is replay-safe only when opening the WebSocket failed before
/// the generation request could have been sent. Once the request actor accepts
/// a frame, every timeout, decode failure, or disconnect is ambiguous and must
/// remain terminal unless the provider supplies an idempotency contract.
pub(super) fn websocket_open_failure_is_replay_safe(error: &AiError) -> bool {
    matches!(
        error,
        AiError::Transport(TransportError {
            phase: TransportPhase::Connect,
            ..
        }) | AiError::NetworkUnavailable(_)
    )
}

struct BedrockResponseStreamRequest {
    response: reqwest::Response,
    model: Model,
    tool_definitions: Vec<ToolDef>,
    pre_send_diagnostics: Vec<crate::error::Diagnostic>,
    buffer_ambiguous_compatibility_content: bool,
    diagnostic_redactor: CredentialRedactor,
    stream_initial_timeout: Duration,
    stream_idle_timeout: Duration,
    stream_deadline: Duration,
}

fn bedrock_response_stream(request: BedrockResponseStreamRequest) -> ResponseStream {
    let BedrockResponseStreamRequest {
        response,
        model,
        tool_definitions,
        pre_send_diagnostics,
        buffer_ambiguous_compatibility_content,
        diagnostic_redactor,
        stream_initial_timeout,
        stream_idle_timeout,
        stream_deadline,
    } = request;
    let raw_event_stream = try_stream! {
        let mut decoder = crate::protocol::bedrock::BedrockEventStreamDecoder::new();
        let mut state = crate::protocol::bedrock::BedrockStreamState::default();
        let mut builder = ResponseBuilder::new(
            model.spec.id.clone(),
            model.spec.protocol,
            model.spec.pricing.clone(),
        );
        builder.set_tool_definitions(&tool_definitions)?;
        builder.strict_tool_sampling = crate::protocol::strict_mode_for(&model);
        builder.set_buffer_ambiguous_compatibility_content(
            buffer_ambiguous_compatibility_content,
        );
        for diagnostic in &pre_send_diagnostics {
            builder.add_diagnostic(diagnostic.clone());
        }

        let mut stream = response.bytes_stream();
        let mut terminal_seen = false;
        let mut provider_event_seen = false;
        let mut successful_body_prefix = Vec::new();
        let mut first_body_chunk = true;
        let started_at = Instant::now();
        let mut last_event_at = None;
        'read: loop {
            let remaining = stream_deadline.saturating_sub(started_at.elapsed());
            if remaining.is_zero() {
                Err(annotate_stream_failure(
                    AiError::Transport(TransportError {
                        phase: TransportPhase::Body,
                        timeout: true,
                        message: "stream exceeded its overall deadline".to_owned(),
                    }),
                    &builder,
                    first_body_chunk,
                    started_at,
                    last_event_at,
                ))?;
            }
            let quiet_timeout = if first_body_chunk {
                stream_initial_timeout
            } else {
                stream_idle_timeout
            };
            let wait_for = remaining.min(quiet_timeout);
            let chunk_result = tokio::time::timeout(wait_for, stream.next())
                .await
                .map_err(|_| {
                    annotate_stream_failure(
                        AiError::Transport(TransportError {
                            phase: TransportPhase::Body,
                            timeout: true,
                            message: if remaining <= quiet_timeout {
                                "stream exceeded its overall deadline".to_owned()
                            } else if first_body_chunk {
                                "stream was idle beyond its initial timeout".to_owned()
                            } else {
                                "stream was idle beyond its timeout".to_owned()
                            },
                        }),
                        &builder,
                        first_body_chunk,
                        started_at,
                        last_event_at,
                    )
                })?;
            let Some(chunk_result) = chunk_result else {
                break;
            };
            let chunk = chunk_result.map_err(|error| {
                annotate_stream_failure(
                    reqwest_transport_error(error, TransportPhase::Body, "Bedrock response body"),
                    &builder,
                    first_body_chunk,
                    started_at,
                    last_event_at,
                )
            })?;
            first_body_chunk = false;
            if !provider_event_seen && successful_body_prefix.len() < MAX_SUCCESS_ERROR_BODY_BYTES {
                let remaining = MAX_SUCCESS_ERROR_BODY_BYTES - successful_body_prefix.len();
                successful_body_prefix.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            }
            let messages = decoder.push(&chunk).map_err(|error| {
                annotate_stream_failure(
                    AiError::Decode(error),
                    &builder,
                    first_body_chunk,
                    started_at,
                    last_event_at,
                )
            })?;
            if !messages.is_empty() {
                provider_event_seen = true;
                last_event_at = Some(Instant::now());
                successful_body_prefix.clear();
            }
            for message in messages {
                let events = crate::protocol::bedrock::decode_stream_event(
                    &model,
                    &message,
                    &mut builder,
                    &mut state,
                )
                .map_err(|error| {
                    annotate_stream_failure(
                        error,
                        &builder,
                        first_body_chunk,
                        started_at,
                        last_event_at,
                    )
                })?;
                for event in events {
                    let terminal = matches!(event, StreamEvent::Finished(_));
                    yield event;
                    if terminal {
                        terminal_seen = true;
                        break 'read;
                    }
                }
            }
        }

        if !terminal_seen {
            decoder.finish().map_err(|error| {
                annotate_stream_failure(
                    AiError::Decode(error),
                    &builder,
                    first_body_chunk,
                    started_at,
                    last_event_at,
                )
            })?;
            let mut final_events = Vec::new();
            crate::protocol::bedrock::finish_stream(&mut builder, &mut state, &mut final_events)
                .map_err(|error| {
                    annotate_stream_failure(
                        error,
                        &builder,
                        first_body_chunk,
                        started_at,
                        last_event_at,
                    )
                })?;
            for event in final_events {
                let terminal = matches!(event, StreamEvent::Finished(_));
                yield event;
                terminal_seen |= terminal;
            }
        }
        if !terminal_seen && !provider_event_seen {
            if let Some(error) = provider_error_from_success_body(&successful_body_prefix) {
                Err(annotate_stream_failure(
                    AiError::Provider(error),
                    &builder,
                    first_body_chunk,
                    started_at,
                    last_event_at,
                ))?;
            }
        }
    };
    let sanitized = raw_event_stream
        .map(move |event| event.map_err(|error| sanitize_ai_error(&diagnostic_redactor, error)));
    crate::stream::guard(sanitized)
}

pub(super) async fn stream_http(
    http: reqwest::Client,
    request: HttpStreamRequest,
    request_dispatch: Option<Arc<std::sync::atomic::AtomicBool>>,
    stream_initial_timeout: Duration,
    stream_idle_timeout: Duration,
    stream_deadline: Duration,
) -> Result<ResponseStream, AiError> {
    let HttpStreamRequest {
        model,
        compatibility,
        parts,
        mut headers,
        requested_audio_format,
        requested_service_tier,
        tool_definitions,
        pre_send_diagnostics,
        buffer_ambiguous_compatibility_content,
        mut diagnostic_redactor,
        on_response,
    } = request;
    let lifecycle_feedback = parts.streaming
        && model.spec.protocol == Protocol::OpenAiChat
        && model.endpoint.runtime.lifecycle_feedback;
    if lifecycle_feedback {
        headers.insert(
            http::HeaderName::from_static(LIFECYCLE_HEADER),
            http::HeaderValue::from_static(LIFECYCLE_REQUEST_VALUE),
        );
    }
    let request_body =
        prepare_request_body(model.endpoint.runtime, &mut headers, parts.body.clone()).await;
    if matches!(&model.endpoint.auth, crate::auth::Auth::RequestSigner(_)) {
        let resolved = crate::auth::resolve_headers_for_request(
            &model.endpoint.auth,
            http::Method::POST,
            parts.url.clone(),
            request_body.clone(),
            headers.clone(),
        )
        .await
        .map_err(AiError::Auth)?;
        diagnostic_redactor.include(resolved.redactor);
        diagnostic_redactor.include_header_values(&headers);
        let mut current_key = None;
        for (key, value) in resolved.headers {
            if let Some(key) = key {
                current_key = Some(key.clone());
                headers.insert(key, value);
            } else if let Some(key) = &current_key {
                headers.append(key.clone(), value);
            }
        }
    }

    // 3. Send the HTTP request
    let builder = http
        .post(parts.url.clone())
        .headers(headers)
        .body(request_body);

    // `RequestBuilder::timeout` applies until the response body is fully
    // consumed, which kills valid long-running SSE generations. Bound only
    // the pre-stream phase instead: after headers arrive, the caller owns
    // the stream lifetime and may cancel by dropping it.
    if let Some(state) = &request_dispatch {
        state.store(true, std::sync::atomic::Ordering::Release);
    }
    let res = tokio::time::timeout(model.endpoint.timeout, builder.send())
        .await
        .map_err(|_| {
            AiError::Transport(TransportError {
                phase: TransportPhase::ResponseHeaders,
                timeout: true,
                message: "request timed out waiting for response headers".to_string(),
            })
        })?
        .map_err(|error| request_open_transport_error(error, "request"))
        .map_err(|error| sanitize_ai_error(&diagnostic_redactor, error))?;

    // A host response hook observes every provider response (success or error)
    // before the body stream is touched. It is advisory and cannot replace or
    // retry the response.
    let status = res.status();
    if let Some(hook) = on_response {
        hook.on_response(status, res.headers(), &HookModelContext::from_model(&model));
    }

    // 4. Handle non-2xx HTTP errors
    if !status.is_success() {
        // Extract only the two headers needed for the structured error
        // before consuming the response. Cloning the whole HeaderMap
        // here adds an allocation on every non-2xx response.
        let request_id = res
            .headers()
            .get("x-request-id")
            .or_else(|| res.headers().get("x-amzn-requestid"))
            .or_else(|| res.headers().get("request-id"))
            .and_then(|h| h.to_str().ok())
            .map(String::from);
        let retry_after = res
            .headers()
            .get("retry-after")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs);

        let mut body = Vec::with_capacity(4096);
        let mut error_stream = res.bytes_stream();
        let started_at = Instant::now();
        while body.len() < 4096 {
            match next_body_chunk(
                &mut error_stream,
                stream_idle_timeout.min(MAX_ERROR_BODY_IDLE_TIMEOUT),
                stream_idle_timeout.min(MAX_ERROR_BODY_IDLE_TIMEOUT),
                false,
                started_at,
                stream_deadline.min(MAX_ERROR_BODY_DEADLINE),
                "HTTP error response body",
            )
            .await
            {
                Ok(Some(chunk)) => {
                    let remaining = 4096 - body.len();
                    body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                }
                // The status and retry metadata are already known. Preserve
                // that structured HTTP error if its optional snippet stalls.
                Ok(None) | Err(_) => break,
            }
        }
        let body_bytes = String::from_utf8_lossy(&body).into_owned();

        let mut code = None;

        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&body_bytes) {
            if let Some(err_obj) = val.get("error") {
                code = err_obj.get("code").and_then(json_scalar_string);
            }
        }

        // Mark only gateway/transient statuses as replay-safe. The agent
        // still gates retries on having seen no generated bytes, so a
        // POST cannot duplicate a completed tool-producing turn.
        let retryable = matches!(
            status,
            http::StatusCode::REQUEST_TIMEOUT
                | http::StatusCode::INTERNAL_SERVER_ERROR
                | http::StatusCode::TOO_MANY_REQUESTS
                | http::StatusCode::BAD_GATEWAY
                | http::StatusCode::SERVICE_UNAVAILABLE
                | http::StatusCode::GATEWAY_TIMEOUT
        );

        return Err(sanitize_ai_error(
            &diagnostic_redactor,
            AiError::Http(HttpError {
                status,
                request_id,
                retry_after,
                provider_code: code,
                body_snippet: if body_bytes.is_empty() {
                    None
                } else {
                    Some(body_bytes)
                },
                retryable,
            }),
        ));
    }

    // 5. Decode ResponseStream
    let initial_lifecycle = lifecycle_feedback
        .then(|| {
            res.headers()
                .get(LIFECYCLE_HEADER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| parse_provider_lifecycle(value, &diagnostic_redactor))
        })
        .flatten();
    let model_clone = model.clone();
    if parts.streaming && model.spec.protocol == Protocol::BedrockConverse {
        return Ok(bedrock_response_stream(BedrockResponseStreamRequest {
            response: res,
            model: model_clone,
            tool_definitions,
            pre_send_diagnostics,
            buffer_ambiguous_compatibility_content,
            diagnostic_redactor,
            stream_initial_timeout,
            stream_idle_timeout,
            stream_deadline,
        }));
    }
    if parts.streaming {
        let byte_stream = res.bytes_stream();
        let diags = pre_send_diagnostics;
        let lifecycle_redactor = diagnostic_redactor.clone();
        let raw_event_stream = try_stream! {
            let mut sse_decoder = crate::protocol::sse::SseDecoder::new();
            let mut builder = ResponseBuilder::new(
                model_clone.spec.id.clone(),
                model_clone.spec.protocol,
                model_clone.spec.pricing.clone()
            );
            builder.compatibility = compatibility;
            builder.requested_service_tier = requested_service_tier;
            builder.set_tool_definitions(&tool_definitions)?;
            builder.strict_tool_sampling = crate::protocol::strict_mode_for(&model_clone);
            builder.set_buffer_ambiguous_compatibility_content(
                buffer_ambiguous_compatibility_content,
            );
            for d in &diags {
                builder.add_diagnostic(d.clone());
            }

            let mut lifecycle_stream_started = false;
            let mut lifecycle_events_emitted = 0usize;
            if let Some(lifecycle) = initial_lifecycle {
                // Header feedback arrives before any provider SSE event. Seed
                // the canonical stream first so advisory telemetry still obeys
                // the `Started`-is-first invariant.
                let started = StreamEvent::Started { response_id: None };
                builder.on_event(&started)?;
                yield started;
                lifecycle_stream_started = true;
                lifecycle_events_emitted += 1;
                yield StreamEvent::ProviderLifecycle(lifecycle);
            }

            let mut stream = byte_stream;
            // The provider's terminal event (`[DONE]` / `response.completed`
            // / `message_stop`) yields a `Finished`. Per design §8 ("No events
            // after `Finished"), the HTTP body read must stop there: reading
            // further can block after success, surface a late body transport
            // error, or feed post-terminal frames into the codec. We stop the
            // instant the codec emits `Finished`.
            let mut terminal_seen = false;
            let mut provider_event_seen = false;
            let mut successful_body_prefix = Vec::new();
            let mut first_body_chunk = true;
            let started_at = Instant::now();
            let mut last_event_at = None;
            'read: loop {
                let remaining = stream_deadline.saturating_sub(started_at.elapsed());
                if remaining.is_zero() {
                    Err(annotate_stream_failure(
                        AiError::Transport(TransportError {
                            phase: TransportPhase::Body,
                            timeout: true,
                            message: "stream exceeded its overall deadline".to_string(),
                        }),
                        &builder,
                        first_body_chunk,
                        started_at,
                        last_event_at,
                    ))?;
                }
                let quiet_timeout = if first_body_chunk {
                    stream_initial_timeout
                } else {
                    stream_idle_timeout
                };
                let wait_for = remaining.min(quiet_timeout);
                let chunk_res = tokio::time::timeout(wait_for, stream.next())
                    .await
                    .map_err(|_| {
                        annotate_stream_failure(
                            AiError::Transport(TransportError {
                                phase: TransportPhase::Body,
                                timeout: true,
                                message: if remaining <= quiet_timeout {
                                    "stream exceeded its overall deadline".to_string()
                                } else if first_body_chunk {
                                    "stream was idle beyond its initial timeout".to_string()
                                } else {
                                    "stream was idle beyond its timeout".to_string()
                                },
                            }),
                            &builder,
                            first_body_chunk,
                            started_at,
                            last_event_at,
                        )
                    })?;
                let Some(chunk_res) = chunk_res else {
                    break;
                };
                let chunk = chunk_res.map_err(|error| {
                    annotate_stream_failure(
                        reqwest_transport_error(error, TransportPhase::Body, "response body"),
                        &builder,
                        first_body_chunk,
                        started_at,
                        last_event_at,
                    )
                })?;
                first_body_chunk = false;

                if !provider_event_seen
                    && successful_body_prefix.len() < MAX_SUCCESS_ERROR_BODY_BYTES
                {
                    let remaining = MAX_SUCCESS_ERROR_BODY_BYTES - successful_body_prefix.len();
                    successful_body_prefix
                        .extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                }

                let sse_frames = if lifecycle_feedback {
                    sse_decoder.push_frames(&chunk)
                } else {
                    sse_decoder.push(&chunk).map(|events| {
                        events
                            .into_iter()
                            .map(crate::protocol::sse::SseFrame::Event)
                            .collect()
                    })
                }
                .map_err(|error| {
                    annotate_stream_failure(
                        AiError::Decode(error),
                        &builder,
                        first_body_chunk,
                        started_at,
                        last_event_at,
                    )
                })?;
                let lifecycle_frame_seen = lifecycle_feedback
                    && lifecycle_events_emitted < MAX_PROVIDER_LIFECYCLE_EVENTS
                    && sse_frames.iter().any(|frame| {
                        matches!(
                            frame,
                            crate::protocol::sse::SseFrame::Comment(comment)
                                if lifecycle_from_sse_comment(comment, &lifecycle_redactor).is_some()
                        )
                    });
                if sse_frames.iter().any(|frame| matches!(frame, crate::protocol::sse::SseFrame::Event(_)))
                    || lifecycle_frame_seen
                {
                    provider_event_seen = true;
                    last_event_at = Some(Instant::now());
                    successful_body_prefix.clear();
                }

                for frame in sse_frames {
                    match frame {
                        crate::protocol::sse::SseFrame::Comment(comment) => {
                            if lifecycle_feedback
                                && lifecycle_events_emitted < MAX_PROVIDER_LIFECYCLE_EVENTS
                            {
                                if let Some(lifecycle) =
                                    lifecycle_from_sse_comment(&comment, &lifecycle_redactor)
                                {
                                    if !lifecycle_stream_started {
                                        let started = StreamEvent::Started { response_id: None };
                                        builder.on_event(&started)?;
                                        yield started;
                                        lifecycle_stream_started = true;
                                    }
                                    lifecycle_events_emitted += 1;
                                    yield StreamEvent::ProviderLifecycle(lifecycle);
                                }
                            }
                        }
                        crate::protocol::sse::SseFrame::Event(sse) => {
                            let stream_events = match model_clone.spec.protocol {
                                Protocol::OpenAiChat => crate::protocol::openai_chat::decode_stream_event(&model_clone, &sse, &mut builder),
                                Protocol::AnthropicMessages => crate::protocol::anthropic::decode_stream_event(&model_clone, &sse, &mut builder),
                                Protocol::OpenAiResponses => crate::protocol::openai_responses::decode_stream_event(&model_clone, &sse, &mut builder),
                                Protocol::BedrockConverse => unreachable!("Bedrock uses AWS Event Stream, not SSE"),
                                Protocol::GoogleGenerativeAi => crate::protocol::google::decode_stream_event(&model_clone, &sse, &mut builder),
                                Protocol::MistralConversations => crate::protocol::mistral_conversations::decode_stream_event(&model_clone, &sse, &mut builder),
                                Protocol::PiMessages => crate::protocol::pi_messages::decode_stream_event(&model_clone, &sse, &mut builder),
                            }
                            .map_err(|error| {
                                annotate_stream_failure(
                                    error,
                                    &builder,
                                    first_body_chunk,
                                    started_at,
                                    last_event_at,
                                )
                            })?;
                            for ev in stream_events {
                                let started = matches!(ev, StreamEvent::Started { .. });
                                let terminal = matches!(ev, StreamEvent::Finished(_));
                                yield ev;
                                lifecycle_stream_started |= started;
                                if terminal {
                                    terminal_seen = true;
                                    break 'read;
                                }
                            }
                        }
                    }
                }
            }

            // Only flush trailing SSE frames if no terminal event was seen;
            // after `Finished` the stream is closed and any residue is ignored
            // rather than decoded into post-terminal events.
            if !terminal_seen {
                let trailing_frames = if lifecycle_feedback {
                    sse_decoder.finish_frames()
                } else {
                    sse_decoder.finish().map(|event| {
                        event
                            .into_iter()
                            .map(crate::protocol::sse::SseFrame::Event)
                            .collect()
                    })
                }
                .map_err(|error| {
                    annotate_stream_failure(
                        AiError::Decode(error),
                        &builder,
                        first_body_chunk,
                        started_at,
                        last_event_at,
                    )
                })?;
                let lifecycle_frame_seen = lifecycle_feedback
                    && lifecycle_events_emitted < MAX_PROVIDER_LIFECYCLE_EVENTS
                    && trailing_frames.iter().any(|frame| {
                        matches!(
                            frame,
                            crate::protocol::sse::SseFrame::Comment(comment)
                                if lifecycle_from_sse_comment(comment, &lifecycle_redactor).is_some()
                        )
                    });
                if trailing_frames.iter().any(|frame| matches!(frame, crate::protocol::sse::SseFrame::Event(_)))
                    || lifecycle_frame_seen
                {
                    provider_event_seen = true;
                    last_event_at = Some(Instant::now());
                    successful_body_prefix.clear();
                }

                for frame in trailing_frames {
                    if terminal_seen {
                        break;
                    }
                    match frame {
                        crate::protocol::sse::SseFrame::Comment(comment) => {
                            if lifecycle_feedback
                                && lifecycle_events_emitted < MAX_PROVIDER_LIFECYCLE_EVENTS
                            {
                                if let Some(lifecycle) =
                                    lifecycle_from_sse_comment(&comment, &lifecycle_redactor)
                                {
                                    if !lifecycle_stream_started {
                                        let started = StreamEvent::Started { response_id: None };
                                        builder.on_event(&started)?;
                                        yield started;
                                        lifecycle_stream_started = true;
                                    }
                                    lifecycle_events_emitted += 1;
                                    yield StreamEvent::ProviderLifecycle(lifecycle);
                                }
                            }
                        }
                        crate::protocol::sse::SseFrame::Event(sse) => {
                            let stream_events = match model_clone.spec.protocol {
                                Protocol::OpenAiChat => crate::protocol::openai_chat::decode_stream_event(&model_clone, &sse, &mut builder),
                                Protocol::AnthropicMessages => crate::protocol::anthropic::decode_stream_event(&model_clone, &sse, &mut builder),
                                Protocol::OpenAiResponses => crate::protocol::openai_responses::decode_stream_event(&model_clone, &sse, &mut builder),
                                Protocol::BedrockConverse => unreachable!("Bedrock uses AWS Event Stream, not SSE"),
                                Protocol::GoogleGenerativeAi => crate::protocol::google::decode_stream_event(&model_clone, &sse, &mut builder),
                                Protocol::MistralConversations => crate::protocol::mistral_conversations::decode_stream_event(&model_clone, &sse, &mut builder),
                                Protocol::PiMessages => crate::protocol::pi_messages::decode_stream_event(&model_clone, &sse, &mut builder),
                            }
                            .map_err(|error| {
                                annotate_stream_failure(
                                    error,
                                    &builder,
                                    first_body_chunk,
                                    started_at,
                                    last_event_at,
                                )
                            })?;
                            for ev in stream_events {
                                let started = matches!(ev, StreamEvent::Started { .. });
                                let terminal = matches!(ev, StreamEvent::Finished(_));
                                yield ev;
                                lifecycle_stream_started |= started;
                                if terminal {
                                    terminal_seen = true;
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            if !terminal_seen && !provider_event_seen {
                if let Some(error) =
                    provider_error_from_success_body(&successful_body_prefix)
                {
                    Err(annotate_stream_failure(
                        AiError::Provider(error),
                        &builder,
                        first_body_chunk,
                        started_at,
                        last_event_at,
                    ))?;
                }
            }
            // Native Conversations and pi-messages entries settle only on their
            // own terminal event, even when their deltas already form valid
            // JSON. Classify the missing native terminal here before the generic
            // guard handles raw EOF.
            if matches!(
                model_clone.spec.protocol,
                Protocol::MistralConversations | Protocol::PiMessages
            ) && !terminal_seen {
                Err(annotate_stream_failure(
                    AiError::StreamProtocol(StreamProtocolError::MissingFinish),
                    &builder,
                    first_body_chunk,
                    started_at,
                    last_event_at,
                ))?;
            }
        };

        let sanitized_event_stream = raw_event_stream.map(move |event| {
            event.map_err(|error| sanitize_ai_error(&diagnostic_redactor, error))
        });
        Ok(crate::stream::guard(sanitized_event_stream))
    } else {
        // Non-streaming path (completed response, e.g. Chat Audio output)
        let mut body_bytes = Vec::new();
        let mut byte_stream = res.bytes_stream();
        let mut first_body_chunk = true;
        let started_at = Instant::now();

        while let Some(chunk) = next_body_chunk(
            &mut byte_stream,
            stream_idle_timeout,
            stream_initial_timeout,
            first_body_chunk,
            started_at,
            stream_deadline,
            "completed response body",
        )
        .await
        .map_err(|error| sanitize_ai_error(&diagnostic_redactor, error))?
        {
            first_body_chunk = false;
            if body_bytes
                .len()
                .checked_add(chunk.len())
                .is_none_or(|size| size > MAX_COMPLETED_BODY_BYTES)
            {
                return Err(AiError::Decode(DecodeError::BodyTooLarge));
            }
            body_bytes.extend_from_slice(&chunk);
        }

        // The non-streaming path exists solely for the OpenAI Chat audio-output
        // request (design §12.1). Only that codec sets `streaming = false`;
        // Responses and Anthropic always stream, so no other codec needs a
        // non-streaming decoder. This is an invariant of `build_request`, not
        // a runtime branch, so no per-codec `decode_response` stub exists.
        debug_assert!(
            matches!(model_clone.spec.protocol, Protocol::OpenAiChat),
            "non-streaming path is Chat-only",
        );
        let mut response = crate::protocol::openai_chat::decode_response_with_tools(
            &model_clone,
            &body_bytes,
            requested_audio_format,
            &tool_definitions,
        )
        .map_err(|error| sanitize_ai_error(&diagnostic_redactor, error))?;
        response.diagnostics.extend(pre_send_diagnostics);

        let response_id = response.response_id.clone();
        let message = response.message.clone();
        let usage = response.usage;

        let raw_event_stream = try_stream! {
            yield StreamEvent::Started { response_id: response_id.clone() };

            let mut index_counter = 0;
            for part in &message.content {
                match part {
                    crate::types::AssistantPart::ProviderMetadata(_) => {}
                    crate::types::AssistantPart::Text(text) => {
                        let idx = index_counter;
                        index_counter += 1;
                        yield StreamEvent::TextStart { index: idx };
                        yield StreamEvent::TextDelta { index: idx, delta: text.clone() };
                        yield StreamEvent::TextEnd { index: idx };
                    }
                    crate::types::AssistantPart::Reasoning(reasoning) => {
                        let idx = index_counter;
                        index_counter += 1;
                        yield StreamEvent::ReasoningStart { index: idx };
                        if let Some(ref text) = reasoning.text {
                            yield StreamEvent::ReasoningDelta { index: idx, delta: text.clone() };
                        }
                        yield StreamEvent::ReasoningEnd { index: idx };
                    }
                    crate::types::AssistantPart::Media(media) => {
                        let idx = index_counter;
                        index_counter += 1;
                        yield StreamEvent::MediaCompleted { index: idx, media: media.clone() };
                    }
                    crate::types::AssistantPart::ToolCall(tc) => {
                        let idx = index_counter;
                        index_counter += 1;
                        yield StreamEvent::ToolCallStart {
                            async_execution: false,
                            index: idx,
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                        };
                        yield StreamEvent::ToolCallArgsDelta {
                            index: idx,
                            delta: tc.arguments_json.clone(),
                        };
                        yield StreamEvent::ToolCallEnd {
                            index: idx,
                            argument_error: tc.argument_error,
                        };
                    }
                }
            }

            yield StreamEvent::Usage(usage);
            yield StreamEvent::Finished(response);
        };

        Ok(crate::stream::guard(raw_event_stream))
    }
}
