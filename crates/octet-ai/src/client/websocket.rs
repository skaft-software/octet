//! The bidirectional Responses WebSocket transport.
//!
//! When an endpoint advertises WebSocket support, generation frames go over a
//! long-lived connection instead of a single POST. That buys steerability — a
//! consumer can send a steering frame into a response that is still running —
//! and it costs a connection pool whose handshakes must not outlive the
//! credentials they were made with.
//!
//! This is separate from [`stream`](super::stream) because the failure model is
//! inverted. An HTTP stream that dies can be retried once nothing observable
//! has been emitted; a WebSocket stream cannot, because the provider may have
//! already committed the generation. Everything here exists to make that
//! asymmetry explicit: [`ResponsesResume`] re-fetches only the events the
//! consumer has not seen, and the pool key hashes the authority, session, and
//! headers so a credential change never reuses an earlier handshake.
//!
//! [`ResponsesResume`]: ResponsesResume

use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use async_stream::try_stream;
use futures_util::StreamExt;
use tokio::sync::mpsc;

use super::diagnostics::sanitize_ai_error;
use super::transport::annotate_stream_failure;
use crate::auth::CredentialRedactor;
use crate::catalog::Model;
use crate::error::{AiError, DecodeError, StreamProtocolError, TransportError, TransportPhase};
use crate::responses_ws::ResponsesWsPool;
use crate::stream::{ResponseBuilder, ResponseStream, StreamEvent};
use crate::types::{Request, ToolDef};
// A session may select new model headers, credentials or an endpoint URL.
// Never reuse a handshake bound to an earlier authority/configuration, and
// never place raw credentials in the connection pool's identity.
pub(super) fn responses_websocket_key(
    model: &Model,
    session: &str,
    url: &url::Url,
    headers: &http::HeaderMap,
) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    let mut field = |value: &[u8]| {
        digest.update((value.len() as u64).to_le_bytes());
        digest.update(value);
    };
    for value in [
        model.endpoint.id.0.as_str(),
        model.spec.id.0.as_str(),
        session,
        url.as_str(),
    ] {
        field(value.as_bytes());
    }
    let mut entries: Vec<_> = headers.iter().collect();
    // Stable sorting preserves the ordering of repeated values of one header.
    entries.sort_by(|(left, _), (right, _)| left.as_str().cmp(right.as_str()));
    for (name, value) in entries {
        field(name.as_str().as_bytes());
        field(value.as_bytes());
    }
    format!("responses:{:x}", digest.finalize())
}
/// Resumes a retained Responses generation after a WebSocket drop.
///
/// Reads the Responses retrieve endpoint
/// (`GET <responses>/{id}?stream=true&starting_after=N`) and hands the remaining
/// raw events to the WebSocket actor, which forwards only the ones the consumer
/// has not seen. This is the transport the provider documents for continuing an
/// in-flight response, and it is only reachable when the request asked the
/// provider to store the response ([`crate::responses_ws::body_requests_storage`]).
pub(super) struct ResponsesResume {
    pub(super) http: reqwest::Client,
    pub(super) endpoint: url::Url,
    pub(super) headers: http::HeaderMap,
}

impl ResponsesResume {
    /// Boxes this reader into the actor's resumer hook.
    pub(super) fn resumer(self: Arc<Self>) -> crate::responses_ws::ResponseResumer {
        Arc::new(move |response_id: String, starting_after: u64| {
            let this = Arc::clone(&self);
            Box::pin(async move { this.open(&response_id, starting_after).await })
                as crate::responses_ws::ResumeFuture
        })
    }

    /// Opens one resumed read and streams decoded events to the actor.
    pub(super) async fn open(
        &self,
        response_id: &str,
        starting_after: u64,
    ) -> Result<mpsc::Receiver<Result<serde_json::Value, AiError>>, AiError> {
        let mut url = self.endpoint.clone();
        url.path_segments_mut()
            .map_err(|_| {
                AiError::Config(crate::error::ConfigError::Parse(
                    "Responses resume endpoint is a base URL".to_owned(),
                ))
            })?
            .pop_if_empty()
            .push(response_id);
        url.query_pairs_mut()
            .append_pair("stream", "true")
            .append_pair("starting_after", &starting_after.to_string());
        let response = self
            .http
            .get(url)
            .headers(self.headers.clone())
            .send()
            .await
            .map_err(|error| {
                AiError::Transport(TransportError {
                    phase: TransportPhase::ResponseHeaders,
                    timeout: error.is_timeout(),
                    message: format!("Responses resume request: {error}"),
                })
            })?;
        if !response.status().is_success() {
            return Err(AiError::Transport(TransportError {
                phase: TransportPhase::ResponseHeaders,
                timeout: false,
                message: format!(
                    "Responses resume rejected with status {}",
                    response.status()
                ),
            }));
        }
        let (sender, receiver) = mpsc::channel(16);
        let mut stream = response.bytes_stream();
        tokio::spawn(async move {
            let mut decoder = crate::protocol::sse::SseDecoder::new();
            loop {
                let chunk = tokio::select! {
                    biased;
                    _ = sender.closed() => return,
                    chunk = stream.next() => chunk,
                };
                let Some(Ok(chunk)) = chunk else {
                    return;
                };
                let Ok(events) = decoder.push(&chunk) else {
                    return;
                };
                for event in events {
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(&event.data) else {
                        continue;
                    };
                    if sender.send(Ok(value)).await.is_err() {
                        return;
                    }
                }
            }
        });
        Ok(receiver)
    }
}

#[allow(clippy::too_many_arguments)]
// A separate guard and builder are created for every response.created. A tiny
// in-memory channel feeds already-decoded canonical events to the existing
// guard one at a time; no extra inference loop or background decoder is needed.
pub(super) fn steering_event_stream(
    pool: ResponsesWsPool,
    key: Option<String>,
    model: Model,
    mut raw: crate::responses_ws::EventReceiver,
    request: Arc<StdMutex<Request>>,
    ledger: crate::steering::Ledger,
    completed: Arc<StdMutex<Option<crate::AssistantMessage>>>,
    diagnostics: Vec<crate::Diagnostic>,
    redactor: CredentialRedactor,
    started_at: Instant,
) -> std::pin::Pin<
    Box<dyn futures_core::Stream<Item = Result<crate::steering::SteeringEvent, AiError>> + Send>,
> {
    use crate::steering::SteeringEvent;
    let decode = try_stream! {
        let mut segment: Option<(String, ResponseBuilder, mpsc::Sender<StreamEvent>, ResponseStream)> = None;
        let mut first = true;
        while let Some(value) = raw.recv().await {
            let value = value?;
            if value.get("type").and_then(serde_json::Value::as_str)==Some("octet.steer.update") {
                let update = serde_json::from_value(value.get("update").cloned().unwrap_or_default())
                    .map_err(|_| crate::steering::invalid("invalid internal steering update"))?;
                yield SteeringEvent::Steer(update);
                continue;
            }
            if value.get("type").and_then(serde_json::Value::as_str)==Some("response.created") {
                if segment.is_some() { Err(crate::steering::invalid("overlapping response segments"))?; }
                let id = value.pointer("/response/id").and_then(serde_json::Value::as_str)
                    .ok_or_else(|| crate::steering::invalid("response segment has no id"))?.to_owned();
                let req = request.lock().unwrap_or_else(|p|p.into_inner()).clone();
                let mut builder = ResponseBuilder::new(model.spec.id.clone(), model.spec.protocol, model.spec.pricing.clone());
                builder.set_tool_definitions(&req.tools)?;
                builder.requested_service_tier = req.responses.as_ref().and_then(|o|o.service_tier);
                builder.set_buffer_ambiguous_compatibility_content(req.compatibility==crate::CompatibilityMode::Lossy);
                let (origin, scope) = if first {
                    (started_at, crate::inference::ClientTimingScope::Request)
                } else {
                    (Instant::now(), crate::inference::ClientTimingScope::ResponseSegment)
                };
                if first { for diagnostic in &diagnostics { builder.add_diagnostic(diagnostic.clone()); } first=false; }
                let (tx, mut rx) = mpsc::channel(1);
                let guard = crate::stream::guard(try_stream! { while let Some(event) = rx.recv().await { yield event; } });
                let guard = crate::inference::measured_stream(guard, origin, scope);
                segment = Some((id,builder,tx,guard));
            }
            let (id,builder,tx,guard) = segment.as_mut()
                .ok_or_else(|| crate::steering::invalid("provider event outside response segment"))?;
            let sse = crate::protocol::sse::SseEvent { event:None, data:value.to_string() };
            let decoded = crate::protocol::openai_responses::decode_stream_event(&model,&sse,builder)?;
            let mut finished = false;
            for event in decoded {
                tx.send(event).await.map_err(|_| crate::steering::invalid("response segment guard closed"))?;
                let event = guard.next().await.ok_or_else(|| crate::steering::invalid("response segment guard ended"))??;
                finished = matches!(&event,StreamEvent::Finished(_));
                if let StreamEvent::Finished(response) = &event {
                    *completed.lock().unwrap_or_else(|p|p.into_inner()) = Some(response.message.clone());
                }
                yield SteeringEvent::Response {response_id:id.clone(),event};
            }
            if finished {
                let (_,_,tx,mut guard) = segment.take().expect("active segment");
                drop(tx);
                if let Some(event) = guard.next().await { event?; }
            }
        }
        if segment.is_some() { Err(AiError::StreamProtocol(StreamProtocolError::PrematureEof))?; }
    };
    let stream = decode.then(move |item| {
        let pool = pool.clone();
        let key = key.clone();
        let ledger = ledger.clone();
        let redactor = redactor.clone();
        async move {
            if item.is_err() {
                crate::steering::ambiguous(&ledger);
                pool.disable(key.as_deref()).await;
            }
            item.map_err(|e| sanitize_ai_error(&redactor, e))
        }
    });
    Box::pin(stream)
}

/// Decode a cached Responses WebSocket using the same protocol builder as the
/// ordinary SSE path. The wire event shape is JSON rather than `data:` framed
/// SSE, so each message is wrapped in the codec's private event view.
#[allow(clippy::too_many_arguments)]
pub(super) fn responses_websocket_stream(
    pool: ResponsesWsPool,
    pool_key: Option<String>,
    model: Model,
    requested_service_tier: Option<crate::types::ServiceTier>,
    mut events: crate::responses_ws::EventReceiver,
    diagnostics: Vec<crate::error::Diagnostic>,
    tool_definitions: Vec<ToolDef>,
    buffer_ambiguous_compatibility_content: bool,
    diagnostic_redactor: CredentialRedactor,
    stream_initial_timeout: Duration,
    stream_idle_timeout: Duration,
    stream_deadline: Duration,
) -> ResponseStream {
    let raw_event_stream = try_stream! {
        let mut builder = ResponseBuilder::new(
            model.spec.id.clone(),
            model.spec.protocol,
            model.spec.pricing.clone(),
        );
        builder.requested_service_tier = requested_service_tier;
        builder.set_tool_definitions(&tool_definitions)?;
        builder.strict_tool_sampling = crate::protocol::strict_mode_for(&model);
        builder.set_buffer_ambiguous_compatibility_content(
            buffer_ambiguous_compatibility_content,
        );
        for diagnostic in diagnostics {
            builder.add_diagnostic(diagnostic);
        }

        let started_at = Instant::now();
        let mut terminal_seen = false;
        let mut emitted_event = false;
        let mut first_provider_event = false;
        let mut last_event_at = None;
        while !terminal_seen {
            let remaining = stream_deadline.saturating_sub(started_at.elapsed());
            let event_result = if remaining.is_zero() {
                Err(AiError::Transport(TransportError {
                    phase: TransportPhase::Body,
                    timeout: true,
                    message: "websocket stream exceeded its overall deadline".to_owned(),
                }))
            } else {
                let quiet_timeout = if emitted_event {
                    stream_idle_timeout
                } else {
                    stream_initial_timeout
                };
                tokio::time::timeout(remaining.min(quiet_timeout), events.recv())
                    .await
                    .map_err(|_| AiError::Transport(TransportError {
                        phase: TransportPhase::Body,
                        timeout: true,
                        message: if remaining <= quiet_timeout {
                            "websocket stream exceeded its overall deadline".to_owned()
                        } else if emitted_event {
                            "websocket stream was idle beyond its timeout".to_owned()
                        } else {
                            "websocket stream was idle beyond its initial timeout".to_owned()
                        },
                    }))
            };
            let event = match event_result {
                Ok(Some(event)) => event,
                Ok(None) => Err(AiError::Transport(TransportError {
                    phase: TransportPhase::Body,
                    timeout: false,
                    message: "Responses WebSocket ended before completion".to_owned(),
                })),
                Err(error) => Err(error),
            };
            let event = match event {
                Ok(event) => event,
                Err(error) => {
                    events.close();
                    Err(annotate_stream_failure(
                        error,
                        &builder,
                        !first_provider_event,
                        started_at,
                        last_event_at,
                    ))?
                }
            };
            first_provider_event = true;
            last_event_at = Some(Instant::now());
            let data = match serde_json::to_string(&event) {
                Ok(data) => data,
                Err(error) => {
                    events.close();
                    Err(annotate_stream_failure(
                        AiError::Decode(DecodeError::Json(error.to_string())),
                        &builder,
                        !first_provider_event,
                        started_at,
                        last_event_at,
                    ))?
                }
            };
            let sse_event = crate::protocol::sse::SseEvent {
                event: None,
                data,
            };
            let decoded = match crate::protocol::openai_responses::decode_stream_event(
                &model,
                &sse_event,
                &mut builder,
            ) {
                Ok(decoded) => decoded,
                Err(error) => {
                    events.close();
                    Err(annotate_stream_failure(
                        error,
                        &builder,
                        !first_provider_event,
                        started_at,
                        last_event_at,
                    ))?
                }
            };
            for event in decoded {
                let terminal = matches!(event, StreamEvent::Finished(_));
                emitted_event = true;
                yield event;
                if terminal {
                    terminal_seen = true;
                    break;
                }
            }
        }
    };
    let guarded = crate::stream::guard(raw_event_stream);
    Box::pin(guarded.then(move |event| {
        let pool = pool.clone();
        let pool_key = pool_key.clone();
        let redactor = diagnostic_redactor.clone();
        async move {
            if event.is_err() {
                pool.disable(pool_key.as_deref()).await;
            }
            event.map_err(|error| sanitize_ai_error(&redactor, error))
        }
    }))
}
