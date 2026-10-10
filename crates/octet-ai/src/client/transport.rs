//! Turning a reqwest outcome into an [`AiError`], and the body-read clocks.
//!
//! This module owns the two halves of the transport boundary that must stay
//! consistent with each other: how a failure is classified, and how long octet
//! is willing to wait for bytes. The timeouts are deliberately not
//! `RequestBuilder::timeout` — a valid SSE generation can run for an hour, so
//! the endpoint timeout only covers the pre-stream phase and the body carries
//! its own initial/idle/deadline trio.
//!
//! It is separate from `client` because the classification rules are the same
//! for the streaming path, the compaction path, and the batch path, and because
//! `reqwest` should not appear anywhere in the dispatch code. The error
//! messages produced here are built by walking only the source chain, so they
//! never carry the request URL.

use std::error::Error as _;
use std::time::{Duration, Instant};

use futures_util::StreamExt;

use super::diagnostics::truncate_transport_message;
use crate::error::{AiError, StreamProgress, TransportError, TransportPhase};
use crate::stream::ResponseBuilder;
/// Hard cap on a buffered non-streaming response body before JSON decode
/// (design §20). Crossing it is a [`DecodeError::BodyTooLarge`].
pub(super) const MAX_COMPLETED_BODY_BYTES: usize = 64 * 1024 * 1024;
/// Enough of an unexpected successful-status body to decode a structured
/// provider error without buffering an unbounded non-SSE response.
pub(super) const MAX_SUCCESS_ERROR_BODY_BYTES: usize = 64 * 1024;
/// Bound DNS/TCP/TLS establishment independently from a provider's header
/// timeout. Without this, a dead route can consume the full endpoint timeout on
/// every retry before the UI receives an error.
pub(crate) const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum time to wait for the first response-body chunk after headers. A
/// provider may have accepted the request and still be processing a very large
/// prompt or loading a local model.
pub(super) const DEFAULT_STREAM_INITIAL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Maximum silence allowed between SSE body chunks after the response starts.
/// Slow local servers can pause for several minutes between reasoning/output
/// chunks without being dead, especially while paging or swapping a large
/// model.
pub(super) const DEFAULT_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Absolute deadline for one response-body read. This remains finite so a
/// wedged provider is eventually surfaced, while leaving room for large
/// compaction/reasoning turns and rate-limited gateways.
pub(super) const DEFAULT_STREAM_DEADLINE: Duration = Duration::from_secs(60 * 60);
/// Error bodies are optional diagnostics after the status and retry metadata
/// are already known. Never let a slow snippet inherit generation-scale waits.
pub(super) const MAX_ERROR_BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const MAX_ERROR_BODY_DEADLINE: Duration = Duration::from_secs(5);
/// Compression level used by the ChatGPT Codex SSE endpoint and the official
/// Codex-compatible client. Level 3 is fast enough to keep request preparation
/// cheap while substantially shrinking replayed tool history.
const CODEX_REQUEST_ZSTD_LEVEL: i32 = 3;
/// Annotate a mid-stream failure with how far the response had progressed.
///
/// Wrapping only happens inside the response-body loop, where the builder is
/// still alive: the raw provider frame/event counts and the retained content
/// bytes are exactly what distinguishes "the provider sent 400 frames and
/// then went silent" from "the provider sent nothing". Pre-stream failures
/// (connection, headers, HTTP status) are left unannotated.
pub(super) fn annotate_stream_failure(
    inner: AiError,
    builder: &ResponseBuilder,
    first_body_chunk: bool,
    started_at: Instant,
    last_event_at: Option<Instant>,
) -> AiError {
    AiError::StreamFailure {
        inner: Box::new(inner),
        progress: StreamProgress {
            provider_events: builder.provider_event_count,
            decoded_events: builder.event_count,
            content_bytes: builder.aggregate_content_bytes,
            buffered_bytes: builder.buffered_content_bytes,
            first_body_seen: !first_body_chunk,
            elapsed_ms: u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            last_event_ms: last_event_at.map(|event_at| {
                u64::try_from(event_at.duration_since(started_at).as_millis()).unwrap_or(u64::MAX)
            }),
        },
    }
}

pub(super) fn reqwest_transport_error(
    error: reqwest::Error,
    phase: TransportPhase,
    operation: &str,
) -> AiError {
    let timeout = error.is_timeout();
    let category = if error.is_connect() {
        "connection failed"
    } else if timeout {
        "timed out"
    } else if error.is_body() {
        "body transfer failed"
    } else {
        "transport failed"
    };
    // Reqwest's top-level Display includes the request URL. Walk only its
    // source chain so DNS/TCP/TLS/reset details survive without endpoint paths,
    // queries, or URL credentials. Bound it because third-party TLS/DNS errors
    // are not under octet's control.
    let mut details = Vec::new();
    let mut source = error.source();
    while let Some(cause) = source {
        let detail = cause.to_string();
        if !detail.trim().is_empty() && details.last() != Some(&detail) {
            details.push(detail);
        }
        if details.len() == 4 {
            break;
        }
        source = cause.source();
    }
    let mut message = format!("{operation} {category}");
    if !details.is_empty() {
        message.push_str(": ");
        message.push_str(&details.join(": "));
    }
    truncate_transport_message(&mut message, 512);
    let transient_pre_send = phase == TransportPhase::Connect
        && error.is_connect()
        && (timeout || transient_connection_source(&error));
    let transport = TransportError {
        phase,
        timeout,
        message,
    };
    if transient_pre_send {
        AiError::NetworkUnavailable(transport)
    } else {
        AiError::Transport(transport)
    }
}

pub(super) fn transient_connection_source(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut source = Some(error);
    while let Some(cause) = source {
        if cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(crate::error::transient_connection_io)
        {
            return true;
        }
        source = cause.source();
    }
    false
}

pub(super) fn request_open_transport_error(error: reqwest::Error, operation: &str) -> AiError {
    let phase = if error.is_connect() {
        TransportPhase::Connect
    } else {
        // Once connection establishment succeeded, request-send failures and
        // response-header failures are ambiguous: the provider may have
        // accepted the POST even though no response was observed locally.
        TransportPhase::ResponseHeaders
    };
    reqwest_transport_error(error, phase, operation)
}
/// Apply the request runtime selected by the endpoint declaration.
///
/// Codecs produce canonical bodies. Endpoint declarations independently opt
/// into documented transport behavior, so providers sharing a codec never need
/// a provider-name branch here. Compression failure is only an optimization
/// miss: preserve the valid uncompressed request instead of failing the model
/// turn. The zstd work runs on the blocking thread pool so multi-hundred-KB
/// request bodies never stall the async runtime worker.
pub(super) async fn prepare_request_body(
    runtime: crate::types::RequestRuntime,
    headers: &mut http::HeaderMap,
    body: bytes::Bytes,
) -> bytes::Bytes {
    if runtime.body_encoding != crate::types::RequestBodyEncoding::Zstd {
        return body;
    }

    let owned = body.clone();
    match tokio::task::spawn_blocking(move || {
        zstd::bulk::compress(owned.as_ref(), CODEX_REQUEST_ZSTD_LEVEL)
    })
    .await
    {
        Ok(Ok(compressed)) => {
            headers.insert(
                http::header::CONTENT_ENCODING,
                http::HeaderValue::from_static("zstd"),
            );
            bytes::Bytes::from(compressed)
        }
        // Compression failure (or a panicked worker) is only an optimization
        // miss: send the valid uncompressed body.
        _ => body,
    }
}

pub(super) async fn next_body_chunk<S>(
    stream: &mut S,
    idle_timeout: Duration,
    initial_timeout: Duration,
    first_chunk: bool,
    started_at: Instant,
    deadline: Duration,
    body_name: &'static str,
) -> Result<Option<bytes::Bytes>, AiError>
where
    S: futures_core::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin,
{
    let remaining = deadline.saturating_sub(started_at.elapsed());
    if remaining.is_zero() {
        return Err(AiError::Transport(TransportError {
            phase: TransportPhase::Body,
            timeout: true,
            message: format!("{body_name} exceeded its overall deadline"),
        }));
    }
    let quiet_timeout = if first_chunk {
        initial_timeout
    } else {
        idle_timeout
    };
    let wait_for = remaining.min(quiet_timeout);
    match tokio::time::timeout(wait_for, stream.next()).await {
        Err(_) => Err(AiError::Transport(TransportError {
            phase: TransportPhase::Body,
            timeout: true,
            message: if remaining <= quiet_timeout {
                format!("{body_name} exceeded its overall deadline")
            } else if first_chunk {
                format!("{body_name} was idle beyond its initial timeout")
            } else {
                format!("{body_name} was idle beyond its timeout")
            },
        })),
        Ok(Some(Err(error))) => Err(reqwest_transport_error(
            error,
            TransportPhase::Body,
            body_name,
        )),
        Ok(Some(Ok(chunk))) => Ok(Some(chunk)),
        Ok(None) => Ok(None),
    }
}
