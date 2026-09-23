//! The `AiClient`, the resolved `Model` handle, and request dispatch.

use async_stream::try_stream;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::error::Error as _;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::auth::CredentialRedactor;
use crate::catalog::Model;
use crate::error::{
    AiError, DecodeError, HttpError, ProviderError, StreamProgress, StreamProtocolError,
    TransportError, TransportPhase,
};
use crate::deferred::DeferredHandle;
use crate::host_transport::{HostStreamModel, HostStreamTransport};
use crate::runtime::{is_reserved_header, HookModelContext, HostRequestOptions};
use crate::responses_ws::{ResponsesWsLiveness, ResponsesWsPool};
use crate::stream::{
    ProviderLifecycle, ProviderLifecycleState, ResponseBuilder, ResponseStream, StreamEvent,
};
use crate::types::{EndpointId, Protocol, Request, Response, ToolDef};
use crate::{ResponsesCompactRequest, ResponsesCompactResponse};

fn merge_preset_headers(
    headers: &mut http::HeaderMap,
    values: &std::collections::BTreeMap<String, String>,
) -> Result<(), AiError> {
    for (name, value) in values {
        let name = http::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| crate::ConfigError::Parse("invalid request header name".into()))?;
        let mut value = http::HeaderValue::from_str(value)
            .map_err(|_| crate::ConfigError::Parse("invalid request header value".into()))?;
        value.set_sensitive(true);
        headers.insert(name, value);
    }
    Ok(())
}

/// Maximum encoded request body a host payload hook may produce.
const MAX_HOOKED_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Applies one host payload hook to an encoded JSON request body.
///
/// The hook sees exactly the codec's payload. A non-JSON body, an oversized
/// replacement, or a hook error fails the attempt before authentication or
/// dispatch; there is no hidden retry.
fn apply_payload_hook(
    hook: &Arc<dyn crate::runtime::PayloadHook>,
    model: &Model,
    body: bytes::Bytes,
) -> Result<bytes::Bytes, AiError> {
    if body.is_empty() {
        return Ok(body);
    }
    let payload: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))?;
    let host_model = HookModelContext::from_model(model);
    let Some(replacement) = hook.on_payload(payload, &host_model)? else {
        return Ok(body);
    };
    let encoded = serde_json::to_vec(&replacement)
        .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))?;
    if encoded.len() > MAX_HOOKED_BODY_BYTES {
        return Err(crate::ConfigError::Parse(format!(
            "payload hook produced a body larger than the {MAX_HOOKED_BODY_BYTES}-byte limit"
        ))
        .into());
    }
    Ok(bytes::Bytes::from(encoded))
}

/// Canonical preparation shared by every host-mediated attempt.
///
/// Replay history is derived without mutating the caller's conversation and
/// strict validation runs before the transport sees the request, so a host
/// transport can never observe a request that the built-in path would reject.
fn prepare_host_request(
    model: &Model,
    req: Request,
) -> Result<(Request, Vec<crate::error::Diagnostic>), AiError> {
    let mut request = req;
    request.messages = crate::transform::transform_request_messages_owned(request.messages, model);
    let request =
        crate::validate::normalize_request_reasoning(&request, &model.spec.capabilities)
            .into_owned();
    let diagnostics = crate::validate::validate_request(
        &request,
        &model.spec.capabilities,
        &model.spec.limits,
        model.spec.protocol,
        &model.spec.id,
        crate::CompatibilityMode::Strict,
    )?;
    Ok((request, diagnostics))
}

// A session may select new model headers, credentials or an endpoint URL.
// Never reuse a handshake bound to an earlier authority/configuration, and
// never place raw credentials in the connection pool's identity.
fn responses_websocket_key(
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
    for value in [model.endpoint.id.0.as_str(), model.spec.id.0.as_str(), session, url.as_str()] {
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

/// A native compact response whose HTTP headers have actually arrived.
///
/// Both successful and non-success statuses reach this boundary before body or
/// optional error-snippet reads. Dropping this value cancels the body; neither
/// opening nor completion retries the request or resolves credentials again.
#[must_use = "complete the response body or drop it to cancel"]
pub struct PendingResponsesCompact {
    response: reqwest::Response,
    diagnostic_redactor: CredentialRedactor,
    stream_idle_timeout: Duration,
    stream_initial_timeout: Duration,
    stream_deadline: Duration,
    opened_at: Instant,
}

impl std::fmt::Debug for PendingResponsesCompact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingResponsesCompact")
            .finish_non_exhaustive()
    }
}

impl PendingResponsesCompact {
    /// Reads the bounded body under its own phase-specific timeouts.
    /// Non-success responses retain their HTTP status and bounded diagnostics.
    pub async fn complete(self) -> Result<ResponsesCompactResponse, AiError> {
        let response = self.response;
        let diagnostic_redactor = self.diagnostic_redactor;
        let status = response.status();
        let request_id = response
            .headers()
            .get("x-request-id")
            .or_else(|| response.headers().get("request-id"))
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_secs);
        if !status.is_success() {
            let mut body = Vec::with_capacity(4096);
            let mut body_stream = response.bytes_stream();
            let started_at = self.opened_at;
            while body.len() < 4096 {
                match next_body_chunk(
                    &mut body_stream,
                    self.stream_idle_timeout.min(MAX_ERROR_BODY_IDLE_TIMEOUT),
                    self.stream_idle_timeout.min(MAX_ERROR_BODY_IDLE_TIMEOUT),
                    false,
                    started_at,
                    self.stream_deadline.min(MAX_ERROR_BODY_DEADLINE),
                    "compact HTTP error response body",
                )
                .await
                {
                    Ok(Some(chunk)) => {
                        let remaining = 4096 - body.len();
                        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                    }
                    Ok(None) | Err(_) => break,
                }
            }
            let snippet = String::from_utf8_lossy(&body).into_owned();
            let provider_code = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .get("error")
                        .and_then(|error| error.get("code"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                });
            let retryable = matches!(
                status,
                http::StatusCode::REQUEST_TIMEOUT
                    | http::StatusCode::TOO_MANY_REQUESTS
                    | http::StatusCode::BAD_GATEWAY
                    | http::StatusCode::SERVICE_UNAVAILABLE
                    | http::StatusCode::GATEWAY_TIMEOUT
            );
            return Err(sanitize_ai_error(
                &diagnostic_redactor,
                HttpError {
                    status,
                    request_id,
                    retry_after,
                    provider_code,
                    body_snippet: (!snippet.is_empty()).then_some(snippet),
                    retryable,
                }
                .into(),
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_COMPLETED_BODY_BYTES as u64)
        {
            return Err(DecodeError::BodyTooLarge.into());
        }
        let mut body = Vec::with_capacity(
            response
                .content_length()
                .unwrap_or_default()
                .min(MAX_COMPLETED_BODY_BYTES as u64) as usize,
        );
        let mut body_stream = response.bytes_stream();
        let mut first_body_chunk = true;
        let started_at = self.opened_at;
        while let Some(chunk) = next_body_chunk(
            &mut body_stream,
            self.stream_idle_timeout,
            self.stream_initial_timeout,
            first_body_chunk,
            started_at,
            self.stream_deadline,
            "compact response body",
        )
        .await
        .map_err(|error| sanitize_ai_error(&diagnostic_redactor, error))?
        {
            first_body_chunk = false;
            if body
                .len()
                .checked_add(chunk.len())
                .is_none_or(|size| size > MAX_COMPLETED_BODY_BYTES)
            {
                return Err(DecodeError::BodyTooLarge.into());
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|error| {
            sanitize_ai_error(
                &diagnostic_redactor,
                AiError::Decode(DecodeError::Json(error.to_string())),
            )
        })
    }
}

/// Hard cap on a buffered non-streaming response body before JSON decode
/// (design §20). Crossing it is a [`DecodeError::BodyTooLarge`].
const MAX_COMPLETED_BODY_BYTES: usize = 64 * 1024 * 1024;
/// Enough of an unexpected successful-status body to decode a structured
/// provider error without buffering an unbounded non-SSE response.
const MAX_SUCCESS_ERROR_BODY_BYTES: usize = 64 * 1024;
/// Bound DNS/TCP/TLS establishment independently from a provider's header
/// timeout. Without this, a dead route can consume the full endpoint timeout on
/// every retry before the UI receives an error.
pub(crate) const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum time to wait for the first response-body chunk after headers. A
/// provider may have accepted the request and still be processing a very large
/// prompt or loading a local model.
const DEFAULT_STREAM_INITIAL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Maximum silence allowed between SSE body chunks after the response starts.
/// Slow local servers can pause for several minutes between reasoning/output
/// chunks without being dead, especially while paging or swapping a large
/// model.
const DEFAULT_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Absolute deadline for one response-body read. This remains finite so a
/// wedged provider is eventually surfaced, while leaving room for large
/// compaction/reasoning turns and rate-limited gateways.
const DEFAULT_STREAM_DEADLINE: Duration = Duration::from_secs(60 * 60);
/// Error bodies are optional diagnostics after the status and retry metadata
/// are already known. Never let a slow snippet inherit generation-scale waits.
const MAX_ERROR_BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_ERROR_BODY_DEADLINE: Duration = Duration::from_secs(5);
/// Compression level used by the ChatGPT Codex SSE endpoint and the official
/// Codex-compatible client. Level 3 is fast enough to keep request preparation
/// cheap while substantially shrinking replayed tool history.
const CODEX_REQUEST_ZSTD_LEVEL: i32 = 3;

fn truncate_transport_message(message: &mut String, max_bytes: usize) {
    if message.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
}

const MAX_PROVIDER_DIAGNOSTIC_BYTES: usize = 4096;
const MAX_DIAGNOSTIC_METADATA_BYTES: usize = 512;
/// A stream can surface only a finite amount of advisory endpoint telemetry.
const MAX_PROVIDER_LIFECYCLE_EVENTS: usize = 64;
/// Lifecycle detail remains brief enough for a status row and cannot retain an
/// endpoint-controlled unbounded string.
const MAX_PROVIDER_LIFECYCLE_DETAIL_BYTES: usize = 160;
const LIFECYCLE_HEADER: &str = "x-octet-lifecycle";
const LIFECYCLE_REQUEST_VALUE: &str = "1";
const LIFECYCLE_COMMENT_PREFIX: &str = "octet-lifecycle:";

/// Parses octet's explicitly negotiated OpenAI-compatible lifecycle value.
///
/// The wire form is `state` or `state; detail`, where state is one of
/// `queued`, `loading`, or `ready`. Unknown states and malformed namespaces
/// are deliberately ignored: this is advisory telemetry, never assistant text.
fn parse_provider_lifecycle(
    value: &str,
    diagnostic_redactor: &CredentialRedactor,
) -> Option<ProviderLifecycle> {
    let (state, detail) = value
        .split_once(';')
        .map_or((value, None), |(state, detail)| (state, Some(detail)));
    let state = ProviderLifecycleState::from_wire(state)?;
    let detail = detail.and_then(|detail| {
        let detail = detail.trim();
        (!detail.is_empty()).then(|| {
            sanitize_diagnostic(
                diagnostic_redactor,
                detail,
                MAX_PROVIDER_LIFECYCLE_DETAIL_BYTES,
            )
        })
    });
    Some(ProviderLifecycle { state, detail })
}

/// Extracts a lifecycle comment without treating ordinary SSE comments as
/// provider data. The `octet-lifecycle:` namespace is accepted only after the
/// endpoint explicitly opted in through the request header.
fn lifecycle_from_sse_comment(
    comment: &str,
    diagnostic_redactor: &CredentialRedactor,
) -> Option<ProviderLifecycle> {
    parse_provider_lifecycle(
        comment.strip_prefix(LIFECYCLE_COMMENT_PREFIX)?.trim_start(),
        diagnostic_redactor,
    )
}

fn is_bidi_format_control(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

/// Redact request credentials, render control characters inert, and retain a
/// hard post-sanitization byte bound. Provider diagnostics cross a trust
/// boundary: they must be safe to persist or print verbatim.
fn sanitize_diagnostic(redactor: &CredentialRedactor, input: &str, max_bytes: usize) -> String {
    let redacted = redactor.redact(input);
    let mut output = String::with_capacity(redacted.len().min(max_bytes));
    let mut truncated = false;

    for character in redacted.chars() {
        if character.is_control() || is_bidi_format_control(character) {
            let escaped = character.escape_default().to_string();
            if output.len().saturating_add(escaped.len()) > max_bytes {
                truncated = true;
                break;
            }
            output.push_str(&escaped);
        } else {
            if output.len().saturating_add(character.len_utf8()) > max_bytes {
                truncated = true;
                break;
            }
            output.push(character);
        }
    }

    if truncated && max_bytes >= '…'.len_utf8() {
        let limit = max_bytes - '…'.len_utf8();
        while output.len() > limit {
            let _ = output.pop();
        }
        output.push('…');
    }
    output
}

fn sanitize_optional_diagnostic(
    redactor: &CredentialRedactor,
    value: &mut Option<String>,
    max_bytes: usize,
) {
    if let Some(value) = value {
        *value = sanitize_diagnostic(redactor, value, max_bytes);
    }
}

fn sanitize_batch_error(redactor: &CredentialRedactor, error: &mut crate::batch::BatchError) {
    use crate::batch::BatchError;

    match error {
        BatchError::InvalidEndpoint(value)
        | BatchError::InvalidCustomId(value)
        | BatchError::DuplicateCustomId(value)
        | BatchError::RequestBodyNotObject(value)
        | BatchError::InvalidBatchId(value)
        | BatchError::InvalidStatus(value)
        | BatchError::UnsupportedProvider(value) => {
            *value = sanitize_diagnostic(redactor, value, MAX_DIAGNOSTIC_METADATA_BYTES);
        }
        BatchError::ModelMismatch { custom_id, model } => {
            *custom_id = sanitize_diagnostic(redactor, custom_id, MAX_DIAGNOSTIC_METADATA_BYTES);
            *model = sanitize_diagnostic(redactor, model, MAX_DIAGNOSTIC_METADATA_BYTES);
        }
        BatchError::EmptyModel | BatchError::EmptyRequests | BatchError::InvalidLimit(_) => {}
    }
}

fn sanitize_ai_error(redactor: &CredentialRedactor, mut error: AiError) -> AiError {
    match &mut error {
        AiError::Http(error) => {
            sanitize_optional_diagnostic(
                redactor,
                &mut error.request_id,
                MAX_DIAGNOSTIC_METADATA_BYTES,
            );
            sanitize_optional_diagnostic(
                redactor,
                &mut error.provider_code,
                MAX_DIAGNOSTIC_METADATA_BYTES,
            );
            sanitize_optional_diagnostic(
                redactor,
                &mut error.body_snippet,
                MAX_PROVIDER_DIAGNOSTIC_BYTES,
            );
        }
        AiError::Transport(error) | AiError::NetworkUnavailable(error) => {
            error.message =
                sanitize_diagnostic(redactor, &error.message, MAX_DIAGNOSTIC_METADATA_BYTES);
        }
        AiError::Provider(error) | AiError::ResponsesFailed(error) => {
            sanitize_optional_diagnostic(redactor, &mut error.code, MAX_DIAGNOSTIC_METADATA_BYTES);
            sanitize_optional_diagnostic(redactor, &mut error.kind, MAX_DIAGNOSTIC_METADATA_BYTES);
            error.message =
                sanitize_diagnostic(redactor, &error.message, MAX_PROVIDER_DIAGNOSTIC_BYTES);
            sanitize_optional_diagnostic(
                redactor,
                &mut error.request_id,
                MAX_DIAGNOSTIC_METADATA_BYTES,
            );
        }
        AiError::Decode(
            DecodeError::Json(message) | DecodeError::InvalidProviderField(message),
        )
        | AiError::StreamProtocol(StreamProtocolError::UnexpectedEvent(message)) => {
            *message = sanitize_diagnostic(redactor, message, MAX_PROVIDER_DIAGNOSTIC_BYTES);
        }
        AiError::StreamFailure { inner, .. } => {
            // The progress counters are purely numeric and can never carry
            // provider text; only the wrapped inner error can, so sanitize
            // that in place.
            let drained = std::mem::replace(&mut **inner, AiError::Canceled);
            **inner = sanitize_ai_error(redactor, drained);
        }
        AiError::Batch(error) => sanitize_batch_error(redactor, error),
        // A deferred poll refusal carries only the static refusal wording and
        // numeric permit/leaf generations, so there is nothing provider-owned
        // to redact here.
        AiError::Deferred(_) => {}
        AiError::Config(_)
        | AiError::Auth(_)
        | AiError::Validation(_)
        | AiError::Unsupported(_)
        | AiError::Decode(_)
        | AiError::Pricing(_)
        | AiError::StreamProtocol(_)
        | AiError::Canceled => {}
    }
    error
}

/// Annotate a mid-stream failure with how far the response had progressed.
///
/// Wrapping only happens inside the response-body loop, where the builder is
/// still alive: the raw provider frame/event counts and the retained content
/// bytes are exactly what distinguishes "the provider sent 400 frames and
/// then went silent" from "the provider sent nothing". Pre-stream failures
/// (connection, headers, HTTP status) are left unannotated.
fn annotate_stream_failure(
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

fn reqwest_transport_error(
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

fn transient_connection_source(error: &(dyn std::error::Error + 'static)) -> bool {
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

fn request_open_transport_error(error: reqwest::Error, operation: &str) -> AiError {
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

fn json_scalar_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

/// Some OpenAI-compatible servers return a JSON error envelope with HTTP 200
/// for request-validation failures. Detect that envelope before the empty SSE
/// stream is misreported as a missing terminal event.
fn provider_error_from_success_body(body: &[u8]) -> Option<ProviderError> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let error = value.get("error")?;
    let mut message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .or_else(|| error.as_str())?
        .to_owned();
    truncate_transport_message(&mut message, 4096);
    let code = error.get("code").and_then(json_scalar_string);
    let kind = error
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let request_id = value
        .get("request_id")
        .or_else(|| error.get("request_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    Some(ProviderError {
        code,
        kind,
        message,
        request_id,
    })
}

/// Apply the request runtime selected by the endpoint declaration.
///
/// Codecs produce canonical bodies. Endpoint declarations independently opt
/// into documented transport behavior, so providers sharing a codec never need
/// a provider-name branch here. Compression failure is only an optimization
/// miss: preserve the valid uncompressed request instead of failing the model
/// turn. The zstd work runs on the blocking thread pool so multi-hundred-KB
/// request bodies never stall the async runtime worker.
async fn prepare_request_body(
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

async fn next_body_chunk<S>(
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

const MAX_BATCH_BODY_BYTES: usize = 256 * 1024 * 1024;
const MAX_BATCH_ERROR_SNIPPET_BYTES: usize = 4096;

async fn read_batch_body(
    response: reqwest::Response,
    initial_timeout: Duration,
    idle_timeout: Duration,
    deadline: Duration,
    operation: &'static str,
) -> Result<Vec<u8>, AiError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BATCH_BODY_BYTES as u64)
    {
        return Err(DecodeError::BodyTooLarge.into());
    }

    let mut body = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or_default()
            .min(MAX_BATCH_BODY_BYTES as u64) as usize,
    );
    let mut stream = response.bytes_stream();
    let started_at = Instant::now();
    let mut first_chunk = true;

    while let Some(chunk) = next_body_chunk(
        &mut stream,
        idle_timeout,
        initial_timeout,
        first_chunk,
        started_at,
        deadline,
        operation,
    )
    .await?
    {
        first_chunk = false;
        if body
            .len()
            .checked_add(chunk.len())
            .is_none_or(|size| size > MAX_BATCH_BODY_BYTES)
        {
            return Err(DecodeError::BodyTooLarge.into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn openrouter_batch_url(endpoint: &crate::types::Endpoint) -> Result<url::Url, AiError> {
    if endpoint.id.0 != "openrouter" {
        return Err(crate::batch::BatchError::UnsupportedProvider(endpoint.id.0.clone()).into());
    }
    crate::catalog::validate_endpoint(endpoint)?;
    endpoint
        .base_url
        .join("../beta/batches")
        .map_err(|error| crate::error::ConfigError::Parse(error.to_string()).into())
}

fn openrouter_batch_item_url(
    endpoint: &crate::types::Endpoint,
    id: &str,
) -> Result<url::Url, AiError> {
    crate::batch::validate_batch_id(id)?;
    let mut url = openrouter_batch_url(endpoint)?;
    let path = format!("{}/{}", url.path().trim_end_matches('/'), id);
    url.set_path(&path);
    Ok(url)
}

async fn read_batch_error_snippet(
    response: reqwest::Response,
    initial_timeout: Duration,
    idle_timeout: Duration,
    deadline: Duration,
) -> String {
    let mut body = Vec::with_capacity(MAX_BATCH_ERROR_SNIPPET_BYTES);
    let mut stream = response.bytes_stream();
    let started_at = Instant::now();
    while body.len() < MAX_BATCH_ERROR_SNIPPET_BYTES {
        match next_body_chunk(
            &mut stream,
            idle_timeout.min(MAX_ERROR_BODY_IDLE_TIMEOUT),
            initial_timeout.min(MAX_ERROR_BODY_IDLE_TIMEOUT),
            false,
            started_at,
            deadline.min(MAX_ERROR_BODY_DEADLINE),
            "batch HTTP error response body",
        )
        .await
        {
            Ok(Some(chunk)) => {
                let remaining = MAX_BATCH_ERROR_SNIPPET_BYTES - body.len();
                body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            }
            Ok(None) | Err(_) => break,
        }
    }
    String::from_utf8_lossy(&body).into_owned()
}

async fn batch_http_request(
    client: &AiClient,
    endpoint: &crate::types::Endpoint,
    method: http::Method,
    url: url::Url,
    body: Option<bytes::Bytes>,
    operation: &'static str,
) -> Result<serde_json::Value, AiError> {
    let proxy = client.request_proxy(&url)?;
    let mut headers = endpoint.default_headers.clone();
    if body.is_some() {
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
    }

    let resolved_headers = crate::auth::resolve_headers(&endpoint.auth)
        .await
        .map_err(AiError::Auth)?;
    let mut diagnostic_redactor = resolved_headers.redactor;
    diagnostic_redactor.include_header_values(&endpoint.default_headers);
    if let Some(proxy) = &proxy {
        diagnostic_redactor.include_proxy_url(proxy);
    }
    let mut current_key = None;
    for (key, value) in resolved_headers.headers {
        if let Some(key) = key {
            current_key = Some(key.clone());
            headers.insert(key, value);
        } else if let Some(key) = &current_key {
            headers.append(key.clone(), value);
        }
    }

    let builder = client.http.request(method, url).headers(headers);
    let builder = if let Some(body) = body {
        builder.body(body)
    } else {
        builder
    };
    let response = tokio::time::timeout(endpoint.timeout, builder.send())
        .await
        .map_err(|_| {
            AiError::Transport(TransportError {
                phase: TransportPhase::ResponseHeaders,
                timeout: true,
                message: format!("{operation} timed out waiting for response headers"),
            })
        })?
        .map_err(|error| request_open_transport_error(error, operation))
        .map_err(|error| sanitize_ai_error(&diagnostic_redactor, error))?;

    let status = response.status();
    let request_id = response
        .headers()
        .get("x-request-id")
        .or_else(|| response.headers().get("request-id"))
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs);

    if !status.is_success() {
        let snippet = read_batch_error_snippet(
            response,
            client.stream_initial_timeout,
            client.stream_idle_timeout,
            client.stream_deadline,
        )
        .await;
        let provider_code = serde_json::from_str::<serde_json::Value>(&snippet)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(|error| error.get("code"))
                    .and_then(json_scalar_string)
            });
        let retryable = matches!(
            status,
            http::StatusCode::REQUEST_TIMEOUT
                | http::StatusCode::TOO_MANY_REQUESTS
                | http::StatusCode::BAD_GATEWAY
                | http::StatusCode::SERVICE_UNAVAILABLE
                | http::StatusCode::GATEWAY_TIMEOUT
        );
        return Err(sanitize_ai_error(
            &diagnostic_redactor,
            HttpError {
                status,
                request_id,
                retry_after,
                provider_code,
                body_snippet: (!snippet.is_empty()).then_some(snippet),
                retryable,
            }
            .into(),
        ));
    }

    let body = read_batch_body(
        response,
        client.stream_initial_timeout,
        client.stream_idle_timeout,
        client.stream_deadline,
        "batch response body",
    )
    .await
    .map_err(|error| sanitize_ai_error(&diagnostic_redactor, error))?;
    serde_json::from_slice(&body).map_err(|error| {
        sanitize_ai_error(
            &diagnostic_redactor,
            AiError::Decode(DecodeError::Json(error.to_string())),
        )
    })
}

struct HttpStreamRequest {
    model: Model,
    compatibility: crate::types::CompatibilityMode,
    parts: crate::protocol::HttpRequestParts,
    headers: http::HeaderMap,
    requested_audio_format: Option<crate::types::AudioFormat>,
    requested_service_tier: Option<crate::types::ServiceTier>,
    tool_definitions: Vec<ToolDef>,
    pre_send_diagnostics: Vec<crate::error::Diagnostic>,
    buffer_ambiguous_compatibility_content: bool,
    diagnostic_redactor: CredentialRedactor,
    /// Optional host hook observing the HTTP response before its body is read.
    on_response: Option<Arc<dyn crate::runtime::ResponseHook>>,
}

/// Falling back is replay-safe only when opening the WebSocket failed before
/// the generation request could have been sent. Once the request actor accepts
/// a frame, every timeout, decode failure, or disconnect is ambiguous and must
/// remain terminal unless the provider supplies an idempotency contract.
fn websocket_open_failure_is_replay_safe(error: &AiError) -> bool {
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

async fn stream_http(
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
        hook.on_response(
            status,
            res.headers(),
            &HookModelContext::from_model(&model),
        );
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

/// Resumes a retained Responses generation after a WebSocket drop.
///
/// Reads the Responses retrieve endpoint
/// (`GET <responses>/{id}?stream=true&starting_after=N`) and hands the remaining
/// raw events to the WebSocket actor, which forwards only the ones the consumer
/// has not seen. This is the transport the provider documents for continuing an
/// in-flight response, and it is only reachable when the request asked the
/// provider to store the response ([`crate::responses_ws::body_requests_storage`]).
struct ResponsesResume {
    http: reqwest::Client,
    endpoint: url::Url,
    headers: http::HeaderMap,
}

impl ResponsesResume {
    /// Boxes this reader into the actor's resumer hook.
    fn resumer(self: Arc<Self>) -> crate::responses_ws::ResponseResumer {
        Arc::new(move |response_id: String, starting_after: u64| {
            let this = Arc::clone(&self);
            Box::pin(async move { this.open(&response_id, starting_after).await })
                as crate::responses_ws::ResumeFuture
        })
    }

    /// Opens one resumed read and streams decoded events to the actor.
    async fn open(
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
            while let Some(chunk) = stream.next().await {
                let Ok(chunk) = chunk else {
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
fn steering_event_stream(
    pool: ResponsesWsPool,
    key: Option<String>,
    model: Model,
    mut raw: crate::responses_ws::EventReceiver,
    request: Arc<StdMutex<Request>>,
    ledger: crate::steering::Ledger,
    completed: Arc<StdMutex<Option<crate::AssistantMessage>>>,
    diagnostics: Vec<crate::Diagnostic>,
    redactor: CredentialRedactor,
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
                if first { for diagnostic in &diagnostics { builder.add_diagnostic(diagnostic.clone()); } first=false; }
                let (tx, mut rx) = mpsc::channel(1);
                let guard = crate::stream::guard(try_stream! { while let Some(event) = rx.recv().await { yield event; } });
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
fn responses_websocket_stream(
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

/// Client wrapper for executing AI service requests.
#[derive(Clone)]
pub struct AiClient {
    http: reqwest::Client,
    proxy_environment: Option<Arc<crate::declarations::proxy::ProxyEnvironment>>,
    responses_ws: ResponsesWsPool,
    host_stream_transports: Arc<StdMutex<HashMap<EndpointId, Arc<dyn HostStreamTransport>>>>,
    stream_initial_timeout: Duration,
    stream_idle_timeout: Duration,
    stream_deadline: Duration,
    request_dispatch: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl Default for AiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl AiClient {
    /// Clones this client with fresh, sticky dispatch tracking for one attempt.
    /// The original client is unaffected. Do not reuse this clone for a new attempt.
    pub fn track_request_dispatch(&self) -> Self {
        let mut client = self.clone();
        client.request_dispatch = Some(Arc::new(std::sync::atomic::AtomicBool::new(false)));
        client
    }

    /// Whether a tracked attempt may have reached a provider or opaque host transport.
    /// False is meaningful only on a fresh tracked clone. True is conservative:
    /// it establishes neither actual transmission nor acceptance or billing.
    pub fn request_may_have_been_sent(&self) -> bool {
        self.request_dispatch
            .as_ref()
            .is_some_and(|state| state.load(std::sync::atomic::Ordering::Acquire))
    }

    pub(crate) fn mark_request_dispatch(&self) {
        if let Some(state) = &self.request_dispatch {
            state.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    /// Creates a new AiClient using the default reqwest client.
    ///
    /// [`Self::try_new`] is available to callers that need to handle client
    /// construction errors. This convenience constructor fails loudly rather
    /// than silently replacing the explicit no-redirect policy with reqwest's
    /// redirect-following default.
    pub fn new() -> Self {
        Self::try_new().expect("failed to initialize the octet HTTP client")
    }

    /// Creates a new AiClient, preserving octet's no-redirect transport policy.
    ///
    /// Reqwest has no useful generation deadline by itself. octet applies the
    /// endpoint timeout while waiting for headers, then allows a generous
    /// first body chunk before enforcing inter-chunk idle and overall body
    /// deadlines in [`Self::stream`].
    pub fn try_new() -> Result<Self, reqwest::Error> {
        let env = crate::declarations::proxy::ProxyEnvironment::NAMES
            .into_iter()
            .filter_map(|name| {
                std::env::var_os(name)
                    .map(|value| (name.to_owned(), value.to_string_lossy().into_owned()))
            })
            .collect();
        Self::try_with_proxy_environment(env)
    }

    /// Creates the ordinary no-redirect client with an explicit proxy environment
    /// instead of process/OS proxy settings. The eight HTTP(S)/ALL/NO_PROXY names
    /// are snapshotted; an empty map disables proxying. Malformed/unsupported
    /// proxies fail before credentials or dispatch, never fall through to direct
    /// egress. Preferred WebSocket routes use HTTP when a proxy is selected.
    pub fn try_with_proxy_environment(
        env: std::collections::BTreeMap<String, String>,
    ) -> Result<Self, reqwest::Error> {
        let proxy_environment = Arc::new(crate::declarations::proxy::ProxyEnvironment::new(env));
        Ok(Self {
            http: proxy_environment
                .clone()
                .configure(
                    reqwest::Client::builder()
                        .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
                        .redirect(reqwest::redirect::Policy::none()),
                )
                .build()?,
            proxy_environment: Some(proxy_environment),
            responses_ws: ResponsesWsPool::default(),
            host_stream_transports: Arc::new(StdMutex::new(HashMap::new())),
            stream_initial_timeout: DEFAULT_STREAM_INITIAL_TIMEOUT,
            stream_idle_timeout: DEFAULT_STREAM_IDLE_TIMEOUT,
            stream_deadline: DEFAULT_STREAM_DEADLINE,
            request_dispatch: None,
        })
    }

    /// Creates an AiClient wrapping a custom reqwest HTTP client.
    pub fn with_http_client(http: reqwest::Client) -> Self {
        Self {
            http,
            proxy_environment: None,
            responses_ws: ResponsesWsPool::default(),
            host_stream_transports: Arc::new(StdMutex::new(HashMap::new())),
            stream_initial_timeout: DEFAULT_STREAM_INITIAL_TIMEOUT,
            stream_idle_timeout: DEFAULT_STREAM_IDLE_TIMEOUT,
            stream_deadline: DEFAULT_STREAM_DEADLINE,
            request_dispatch: None,
        }
    }

    fn request_proxy(&self, target: &url::Url) -> Result<Option<url::Url>, AiError> {
        self.proxy_environment
            .as_ref()
            .map(|env| env.resolve(target))
            .transpose()
            .map(Option::flatten)
    }

    /// Registers a host-owned stream transport for one catalog endpoint.
    ///
    /// The registration is shared by clones of this client. The transport sees
    /// only canonical request data and [`crate::HostStreamModel`]; resolved
    /// credentials, endpoint URLs, and headers are deliberately not exposed.
    /// Replacing a transport is an explicit host authority action.
    pub fn register_host_stream_transport(
        &self,
        endpoint: EndpointId,
        transport: Arc<dyn HostStreamTransport>,
    ) {
        self.host_stream_transports
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(endpoint, transport);
    }

    /// Removes a host-owned stream transport for one endpoint.
    pub fn remove_host_stream_transport(&self, endpoint: &EndpointId) {
        self.host_stream_transports
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(endpoint);
    }

    /// Sets the maximum quiet interval and absolute lifetime of response bodies,
    /// including SSE and completed JSON. Bounded HTTP error snippets also obey
    /// these values but retain shorter internal ceilings so an already-known
    /// status is surfaced promptly.
    /// The idle value also bounds the first body chunk; callers that need a
    /// longer time-to-first-body-byte can override it with
    /// [`Self::with_initial_stream_timeout`].
    /// Callers can use shorter values in tests or batch workers.
    pub fn with_stream_timeouts(mut self, idle_timeout: Duration, deadline: Duration) -> Self {
        let idle_timeout = idle_timeout.max(Duration::from_millis(1));
        self.stream_initial_timeout = idle_timeout;
        self.stream_idle_timeout = idle_timeout;
        self.stream_deadline = deadline.max(Duration::from_millis(1));
        self
    }

    /// Sets the maximum time allowed for the first successful response-body
    /// chunk after headers arrive, or the first decoded WebSocket event. This is
    /// independent from the shorter inter-chunk idle timeout and is useful for
    /// large prompts or cold local model servers. Bounded error bodies use a
    /// separate short ceiling so their already-known HTTP status is surfaced
    /// promptly.
    pub fn with_initial_stream_timeout(mut self, timeout: Duration) -> Self {
        self.stream_initial_timeout = timeout.max(Duration::from_millis(1));
        self
    }

    /// Executes one provider request and returns a pinned stream of events.
    ///
    /// This transport deliberately performs no automatic retries. Callers own
    /// retry count, backoff, cancellation, and idempotency policy; structured
    /// HTTP errors retain `retry_after` and `retryable` metadata for that use.
    pub async fn stream(&self, model: &Model, req: Request) -> Result<ResponseStream, AiError> {
        self.stream_with_overrides(model, req, crate::RequestOverrides::default()).await
    }

    /// Executes one inference attempt with private request-local configuration.
    ///
    /// Sampling overrides replace model defaults (explicit canonical stop wins).
    /// Header precedence is endpoint < model < caller < codec < authoritative auth.
    /// Environment values overlay only this request; process state and the
    /// catalog remain unchanged. Nonzero retry controls are explicitly refused.
    /// `timeout_ms` bounds opening plus body lifetime, including credential waits.
    /// Transport-specific overrides cannot cross a host/extension transport.
    pub async fn stream_with_overrides(
        &self,
        model: &Model,
        req: Request,
        overrides: crate::RequestOverrides,
    ) -> Result<ResponseStream, AiError> {
        self.stream_with_host_options(model, req, overrides, HostRequestOptions::default())
            .await
    }

    /// Executes one inference attempt with private request-local configuration
    /// and host-owned runtime options.
    ///
    /// [`HostRequestOptions`] carries per-request hooks and a credential
    /// override that must never become canonical request data. The built-in
    /// HTTP path applies them; a host stream transport refuses them so a hook
    /// can never observe or rewrite wire material it does not own.
    pub async fn stream_with_host_options(
        &self,
        model: &Model,
        mut req: Request,
        overrides: crate::RequestOverrides,
        host_options: HostRequestOptions,
    ) -> Result<ResponseStream, AiError> {
        overrides.validate().map_err(|error| crate::ConfigError::Parse(error.to_string()))?;
        host_options.validate()?;
        if !host_options.metadata.is_empty() {
            return Err(crate::ConfigError::Parse(
                "per-request metadata is unsupported until the selected codec declares a wire field"
                    .into(),
            )
            .into());
        }
        if overrides.max_retries.unwrap_or(0) != 0 || overrides.max_retry_delay_ms.unwrap_or(0) != 0 {
            return Err(crate::ConfigError::Parse("client retries are host-owned; nonzero retry overrides are unsupported".into()).into());
        }
        crate::catalog::validate_endpoint(&model.endpoint)?;
        crate::catalog::validate_model_spec(&model.spec)?;
        if host_options.api_key.is_some()
            && matches!(&model.endpoint.auth, crate::auth::Auth::RequestSigner(_))
        {
            return Err(crate::ConfigError::Parse(
                "a per-request api key override cannot be applied to a request-aware signer".into(),
            )
            .into());
        }
        let host_transport = host_options.fetch.is_some()
            || self.host_stream_transports.lock().unwrap_or_else(|p| p.into_inner()).contains_key(&model.endpoint.id);
        if host_transport && (!overrides.headers.is_empty() || !overrides.env.is_empty()
            || !overrides.sampling_params.is_empty() || overrides.azure.is_some()) {
            return Err(crate::ConfigError::Parse("wire overrides are unsupported by a host stream transport".into()).into());
        }
        if host_transport && host_options.has_wire_hooks() {
            return Err(crate::ConfigError::Parse(
                "per-request api key, metadata, and payload/header/response hooks are unsupported by a host stream transport".into(),
            )
            .into());
        }
        let mut model = model.clone();
        if !overrides.sampling_params.is_empty() || !overrides.headers.is_empty() {
            let preset = &mut Arc::make_mut(&mut model.spec).preset;
            preset.sampling_params.extend(overrides.sampling_params.clone());
            for (name, value) in &overrides.headers {
                preset.headers.retain(|old, _| !old.eq_ignore_ascii_case(name));
                preset.headers.insert(name.clone(), value.clone());
            }
            // Explicit sampling temperature is a per-call control, not a model
            // default; materialize it before ordinary canonical validation.
            if let Some(value) = overrides.sampling_params.get("temperature") {
                req.temperature = value.as_f64().map(|value| value as f32);
            }
        }
        if !host_transport {
            crate::declarations::azure::apply(&mut model, overrides.azure.as_ref(), &overrides.env)?;
        }
        let mut client = self.with_environment_overlay(&overrides.env)?;
        let deadline = if let Some(timeout) = overrides.timeout_ms {
            let timeout = Duration::from_millis(timeout);
            let deadline = tokio::time::Instant::now().checked_add(timeout)
                .ok_or_else(|| crate::ConfigError::Parse("request timeout is not representable".into()))?;
            Arc::make_mut(&mut model.endpoint).timeout = timeout;
            client.stream_initial_timeout = client.stream_initial_timeout.min(timeout);
            client.stream_idle_timeout = client.stream_idle_timeout.min(timeout);
            client.stream_deadline = client.stream_deadline.min(timeout);
            Some(deadline)
        } else { None };
        let open = client.stream_once(&model, req, &overrides.env, &host_options);
        let Some(deadline) = deadline else { return open.await; };
        let mut stream = tokio::time::timeout_at(deadline, open).await.map_err(|_| {
            AiError::Transport(TransportError { phase: TransportPhase::ResponseHeaders, timeout: true, message: "request-local opening deadline exceeded".into() })
        })??;
        Ok(Box::pin(try_stream! {
            loop {
                let item = tokio::time::timeout_at(deadline, stream.next()).await.map_err(|_| {
                    AiError::Transport(TransportError { phase: TransportPhase::Body, timeout: true, message: "request-local response deadline exceeded".into() })
                })?;
                let Some(item) = item else { break; };
                yield item?;
            }
        }))
    }

    /// Opens an explicitly multi-response, in-flight steering operation.
    ///
    /// Both model and endpoint must advertise steering and select native
    /// WebSockets. There is no HTTP fallback, reconnect or inference retry.
    /// Proxies, request signers and opaque host transports fail closed rather
    /// than bypassing their transport/authentication requirements.
    pub async fn steerable_responses(
        &self,
        model: &Model,
        mut req: Request,
    ) -> Result<crate::steering::SteeringSession, AiError> {
        use crate::steering::{SteeringControl, SteeringSession};
        let mut prepared = model.clone();
        crate::declarations::azure::apply(&mut prepared, None, &Default::default())?;
        let model = &prepared;
        crate::catalog::validate_endpoint(&model.endpoint)?;
        crate::catalog::validate_model_spec(&model.spec)?;
        if model.spec.endpoint != model.endpoint.id {
            return Err(crate::ConfigError::UnknownEndpoint(model.spec.endpoint.clone()).into());
        }
        if !model.responses_features().steering
            || model.spec.protocol != Protocol::OpenAiResponses
            || model.endpoint.transport != crate::EndpointTransport::WebSocketPreferred
            || matches!(&model.endpoint.auth, crate::Auth::RequestSigner(_))
            || self
                .host_stream_transports
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .contains_key(&model.endpoint.id)
        {
            return Err(crate::steering::invalid(
                "route is not qualified for native Responses steering",
            ));
        }
        req.messages = crate::transform::transform_request_messages_owned(req.messages, model);
        crate::json_repair::validate_tool_definitions(&req.tools).map_err(AiError::Decode)?;
        let parts = crate::protocol::openai_responses::build_request(model, &req)?;
        if parts.body.len() > 64 * 1024 * 1024 {
            return Err(DecodeError::ResponseTooLarge.into());
        }
        if self.request_proxy(&parts.url)?.is_some() || !parts.streaming {
            return Err(crate::steering::invalid(
                "native steering requires an unproxied streaming WebSocket route",
            ));
        }
        let mut headers = model.endpoint.default_headers.clone();
        merge_preset_headers(&mut headers, &model.spec.preset.headers)?;
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        for (key, value) in &parts.headers {
            headers.insert(key.clone(), value.clone());
        }
        let resolved = crate::auth::resolve_headers(&model.endpoint.auth)
            .await
            .map_err(AiError::Auth)?;
        let mut redactor = resolved.redactor;
        let mut current_key = None;
        for (key, value) in resolved.headers {
            if let Some(key) = key {
                current_key = Some(key.clone());
                headers.insert(key, value);
            } else if let Some(key) = &current_key {
                headers.append(key.clone(), value);
            }
        }
        redactor.include_header_values(&headers);
        if model
            .endpoint
            .runtime
            .responses_profile
            .sends_websocket_beta_header()
        {
            headers.insert(
                http::HeaderName::from_static("openai-beta"),
                http::HeaderValue::from_static(ResponsesWsPool::beta_header_value()),
            );
        }
        let mut body: serde_json::Value = serde_json::from_slice(&parts.body)
            .map_err(|e| AiError::Decode(DecodeError::Json(e.to_string())))?;
        if let Some(object) = body.as_object_mut() {
            object.remove("stream");
            object.remove("background");
        }
        let key = req
            .session_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .map(|id| responses_websocket_key(model, id, &parts.url, &headers));
        let (sender, commands) = mpsc::channel(crate::steering::MAX_STEERS);
        let ledger = Arc::new(StdMutex::new(Vec::new()));
        let request = Arc::new(StdMutex::new(req));
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let operation = crate::responses_ws::SteeringOperation {
            commands,
            ledger: ledger.clone(),
            request: request.clone(),
            initial_timeout: self.stream_initial_timeout,
            idle_timeout: self.stream_idle_timeout,
            deadline: self.stream_deadline,
            cancel: cancelled,
            redactor: redactor.clone(),
        };
        self.mark_request_dispatch();
        let events = self
            .responses_ws
            .request_operation(
                key.as_deref(),
                parts.url,
                headers,
                body,
                ResponsesWsLiveness::for_response_idle(self.stream_idle_timeout),
                model.endpoint.timeout,
                None,
                Some(operation),
            )
            .await
            .map_err(|e| sanitize_ai_error(&redactor, e))?;
        let completed = Arc::new(StdMutex::new(None));
        let control = SteeringControl {
            sender,
            ledger: ledger.clone(),
            model: model.clone(),
            completed: completed.clone(),
        };
        Ok(SteeringSession {
            control,
            cancel: Some(cancel),
            events: steering_event_stream(
                self.responses_ws.clone(),
                key,
                model.clone(),
                events,
                request,
                ledger,
                completed,
                parts.diagnostics,
                redactor,
            ),
        })
    }

    /// Best-effort prewarms a cached OpenAI Responses WebSocket.
    ///
    /// The request is sent with the provider-specific `generate=false` flag,
    /// so this establishes connection/continuation state without consuming a
    /// model turn. Callers can invoke it from an input/composer task and ignore
    /// the result; ordinary [`Self::stream`] calls always retain HTTP/SSE
    /// fallback behavior.
    pub async fn prewarm_responses(&self, model: &Model, req: Request) -> Result<(), AiError> {
        let mut prepared = model.clone();
        crate::declarations::azure::apply(&mut prepared, None, &Default::default())?;
        let model = &prepared;
        crate::catalog::validate_endpoint(&model.endpoint)?;
        crate::catalog::validate_model_spec(&model.spec)?;
        if model.spec.endpoint != model.endpoint.id {
            return Err(crate::ConfigError::UnknownEndpoint(model.spec.endpoint.clone()).into());
        }
        if !matches!(
            model.endpoint.transport,
            crate::types::EndpointTransport::WebSocketPreferred
        ) || model.spec.protocol != Protocol::OpenAiResponses
        {
            return Ok(());
        }
        let session_id = req.session_id.clone().filter(|id| !id.is_empty());
        let Some(session_id) = session_id else {
            return Ok(());
        };
        let mut req = req;
        req.messages = crate::transform::transform_request_messages_owned(req.messages, model);
        crate::json_repair::validate_tool_definitions(&req.tools).map_err(AiError::Decode)?;
        let parts = crate::protocol::openai_responses::build_request(model, &req)?;
        if self.request_proxy(&parts.url)?.is_some() {
            return Ok(());
        }
        let mut headers = http::HeaderMap::new();
        for (key, value) in &model.endpoint.default_headers {
            headers.insert(key.clone(), value.clone());
        }
        merge_preset_headers(&mut headers, &model.spec.preset.headers)?;
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        for (key, value) in &parts.headers {
            headers.insert(key.clone(), value.clone());
        }
        let resolved_headers = crate::auth::resolve_headers(&model.endpoint.auth)
            .await
            .map_err(AiError::Auth)?;
        let mut diagnostic_redactor = resolved_headers.redactor;
        diagnostic_redactor.include_header_values(&headers);
        let mut current_key = None;
        for (key, value) in resolved_headers.headers {
            if let Some(key) = key {
                current_key = Some(key.clone());
                headers.insert(key, value);
            } else if let Some(key) = &current_key {
                headers.append(key.clone(), value);
            }
        }
        if model
            .endpoint
            .runtime
            .responses_profile
            .sends_websocket_beta_header()
        {
            headers.insert(
                http::HeaderName::from_static("openai-beta"),
                http::HeaderValue::from_static(ResponsesWsPool::beta_header_value()),
            );
        }
        let body = serde_json::from_slice::<serde_json::Value>(&parts.body)
            .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))?;
        let key = responses_websocket_key(model, &session_id, &parts.url, &headers);
        let result = self
            .responses_ws
            .prewarm(
                &key,
                parts.url,
                headers,
                body,
                ResponsesWsLiveness::for_response_idle(self.stream_idle_timeout),
                model.endpoint.timeout.min(DEFAULT_CONNECT_TIMEOUT),
            )
            .await;
        result.map_err(|error| sanitize_ai_error(&diagnostic_redactor, error))
    }

    async fn stream_once(&self, model: &Model, req: Request, environment: &std::collections::BTreeMap<String, String>, host_options: &HostRequestOptions) -> Result<ResponseStream, AiError> {
        crate::catalog::validate_endpoint(&model.endpoint)?;
        crate::catalog::validate_model_spec(&model.spec)?;
        if model.spec.endpoint != model.endpoint.id {
            return Err(crate::ConfigError::UnknownEndpoint(model.spec.endpoint.clone()).into());
        }

        let host_transport = host_options.fetch.clone().or_else(|| {
            self.host_stream_transports
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&model.endpoint.id)
                .cloned()
        });
        if let Some(transport) = host_transport {
            // Keep host-mediated transports on the canonical side of the same
            // replay-history and capability boundary as HTTP codecs. Unlike a
            // protocol codec they cannot safely perform lossy wire-specific
            // degradation, so validate strictly rather than exposing an
            // unsupported canonical feature to an extension transport.
            let (request, diagnostics) = prepare_host_request(model, req)?;
            self.mark_request_dispatch();
            let stream = transport
                .stream(HostStreamModel::from(model), request, diagnostics)
                .await?;
            return Ok(crate::stream::guard(stream));
        }

        // Derive target-compatible replay history without mutating the caller's
        // canonical conversation. This must happen before strict validation:
        // cross-model reasoning, unsupported historical media, and interrupted
        // tool turns are normalized into valid canonical messages first.
        let mut req = req;
        req.messages = crate::transform::transform_request_messages_owned(req.messages, model);
        let tool_definitions = req.tools.clone();
        // Reject malformed schemas before a provider request can consume them.
        // The same immutable snapshot is retained by response assembly below.
        crate::json_repair::validate_tool_definitions(&tool_definitions)
            .map_err(AiError::Decode)?;
        // Ambiguous bare JSON must remain visible in the default strict stream.
        // Lossy mode is the explicit opt-in for holding it to EOF and
        // interpreting a provider's text as compatibility tool syntax.
        let buffer_ambiguous_compatibility_content =
            req.compatibility == crate::CompatibilityMode::Lossy;

        let requested_audio_format = match &req.output_modalities {
            crate::types::OutputModalities::TextAndAudio(options) => Some(options.format),
            crate::types::OutputModalities::Text => None,
        };
        // 1. Build the HTTP request parts via the protocol codec
        let mut parts = match model.spec.protocol {
            Protocol::OpenAiChat => crate::protocol::openai_chat::build_request(model, &req)?,
            Protocol::AnthropicMessages => crate::protocol::anthropic::build_request(model, &req)?,
            Protocol::OpenAiResponses => {
                crate::protocol::openai_responses::build_request(model, &req)?
            }
            Protocol::BedrockConverse => crate::protocol::bedrock::build_request(model, &req)?,
            Protocol::GoogleGenerativeAi => crate::protocol::google::build_request(model, &req)?,
            Protocol::MistralConversations => {
                crate::protocol::mistral_conversations::build_request(model, &req)?
            }
            Protocol::PiMessages => crate::protocol::pi_messages::build_request(model, &req)?,
        };

        // A host payload hook sees and may replace exactly the encoded JSON the
        // codec produced, before any credential is attached or a byte is sent.
        if let Some(hook) = &host_options.on_payload {
            parts.body = apply_payload_hook(hook, model, parts.body)?;
        }

        let proxy = self.request_proxy(&parts.url)?;

        // Pre-send Lossy diagnostics (capability drops computed in `build_request`)
        // must reach the terminal `Finished` response (design §7). Capture them
        // here and seed the assembly with them below.
        let pre_send_diagnostics = parts.diagnostics.clone();

        // 2. Compose headers in precedence order:
        //    a. Endpoint default headers
        //    b. Request-specific/codec headers
        //    c. Dynamic/Resolved auth headers
        let mut headers = http::HeaderMap::new();

        for (k, v) in &model.endpoint.default_headers {
            headers.insert(k.clone(), v.clone());
        }
        merge_preset_headers(&mut headers, &model.spec.preset.headers)?;

        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        for (k, v) in &parts.headers {
            headers.insert(k.clone(), v.clone());
        }

        // A host header transform runs after endpoint/model/codec headers and
        // before authentication, so request-aware signers still cover the
        // final set. It cannot add or change authentication, host, framing, or
        // signing headers.
        if let Some(transform) = &host_options.transform_headers {
            let before = headers.clone();
            transform.transform_headers(&mut headers, &HookModelContext::from_model(model))?;
            for name in headers.keys() {
                if is_reserved_header(name) && before.get(name) != headers.get(name) {
                    return Err(crate::ConfigError::ReservedHeader(name.clone()).into());
                }
            }
        }

        // Request-aware signers (SigV4) must run after body encoding, so the
        // exact body and final header set are covered. Ordinary auth remains
        // resolved here so the Responses WebSocket path can use it directly.
        let request_aware_signer =
            matches!(&model.endpoint.auth, crate::auth::Auth::RequestSigner(_));
        let mut diagnostic_redactor = CredentialRedactor::default();
        if !request_aware_signer {
            let resolved_headers = crate::auth::resolve_headers_with_api_key(
                &model.endpoint.auth,
                environment,
                host_options.api_key.as_ref(),
            )
            .await
            .map_err(AiError::Auth)?;
            diagnostic_redactor = resolved_headers.redactor;
            let mut current_key = None;
            for (key, value) in resolved_headers.headers {
                if let Some(key) = key {
                    current_key = Some(key.clone());
                    headers.insert(key, value);
                } else if let Some(key) = &current_key {
                    headers.append(key.clone(), value);
                }
            }
        }
        diagnostic_redactor.include_header_values(&headers);
        if let Some(proxy) = &proxy {
            diagnostic_redactor.include_proxy_url(proxy);
        }

        let fallback_request = HttpStreamRequest {
            model: model.clone(),
            compatibility: req.compatibility,
            parts,
            headers,
            requested_audio_format,
            requested_service_tier: req
                .responses
                .as_ref()
                .and_then(|options| options.service_tier),
            tool_definitions,
            pre_send_diagnostics,
            buffer_ambiguous_compatibility_content,
            diagnostic_redactor: diagnostic_redactor.clone(),
            on_response: host_options.on_response.clone(),
        };

        // Responses WebSockets are deliberately opt-in per endpoint. A
        // connection/handshake failure is replay-safe and falls back to the
        // ordinary HTTP/SSE request below. Once the generation frame may have
        // been sent, every timeout or disconnect is terminal: silently replaying
        // the POST could duplicate provider work and billing.
        if matches!(
            model.endpoint.transport,
            crate::types::EndpointTransport::WebSocketPreferred
        ) && model.spec.protocol == Protocol::OpenAiResponses
            && !request_aware_signer
            && proxy.is_none()
            && fallback_request.parts.streaming
        {
            let session_key = req.session_id.as_deref().filter(|id| !id.is_empty());
            let mut ws_headers = fallback_request.headers.clone();
            if model
                .endpoint
                .runtime
                .responses_profile
                .sends_websocket_beta_header()
            {
                ws_headers.insert(
                    http::HeaderName::from_static("openai-beta"),
                    http::HeaderValue::from_static(ResponsesWsPool::beta_header_value()),
                );
            }
            let websocket_key = session_key.map(|session| {
                responses_websocket_key(model, session, &fallback_request.parts.url, &ws_headers)
            });
            if let Ok(body) =
                serde_json::from_slice::<serde_json::Value>(&fallback_request.parts.body)
            {
                // A retained response can be resumed by cursor after a drop; a
                // non-retained one (octet's durable-replay `store: false`) has
                // nothing to resume, so the actor fails closed instead.
                let resumer = crate::responses_ws::body_requests_storage(&body).then(|| {
                    Arc::new(ResponsesResume {
                        http: self.http.clone(),
                        endpoint: fallback_request.parts.url.clone(),
                        headers: fallback_request.headers.clone(),
                    })
                    .resumer()
                });
                self.mark_request_dispatch();
                let result = self
                    .responses_ws
                    .request(
                        websocket_key.as_deref(),
                        fallback_request.parts.url.clone(),
                        ws_headers,
                        body,
                        ResponsesWsLiveness::for_response_idle(self.stream_idle_timeout),
                        model.endpoint.timeout,
                        resumer,
                    )
                    .await;
                match result {
                    Ok(events) => {
                        return Ok(responses_websocket_stream(
                            self.responses_ws.clone(),
                            websocket_key.clone(),
                            model.clone(),
                            fallback_request.requested_service_tier,
                            events,
                            fallback_request.pre_send_diagnostics.clone(),
                            fallback_request.tool_definitions.clone(),
                            buffer_ambiguous_compatibility_content,
                            diagnostic_redactor,
                            self.stream_initial_timeout,
                            self.stream_idle_timeout,
                            self.stream_deadline,
                        ));
                    }
                    Err(error) if websocket_open_failure_is_replay_safe(&error) => {}
                    Err(error) => {
                        return Err(sanitize_ai_error(&diagnostic_redactor, error));
                    }
                }
            }
        }

        stream_http(
            self.http.clone(),
            fallback_request,
            self.request_dispatch.clone(),
            self.stream_initial_timeout,
            self.stream_idle_timeout,
            self.stream_deadline,
        )
        .await
    }

    /// Calls OpenAI's native `POST /responses/compact` endpoint.
    ///
    /// The returned output is opaque and intentionally unpruned; callers may
    /// use [`crate::ResponsesCompactResponse::output`] directly as the next
    /// full replay window.
    pub async fn compact_responses(
        &self,
        model: &Model,
        request: ResponsesCompactRequest,
    ) -> Result<ResponsesCompactResponse, AiError> {
        self.open_compact_responses(model, request)
            .await?
            .complete()
            .await
    }

    /// Sends one native compact request and returns at actual HTTP header arrival.
    ///
    /// This includes non-2xx responses: their optional diagnostic body is read
    /// only by [`PendingResponsesCompact::complete`]. Callers can bound opening
    /// independently without cancelling a healthy body at an outage deadline.
    pub async fn open_compact_responses(
        &self,
        model: &Model,
        mut request: ResponsesCompactRequest,
    ) -> Result<PendingResponsesCompact, AiError> {
        crate::catalog::validate_endpoint(&model.endpoint)?;
        crate::catalog::validate_model_spec(&model.spec)?;
        if model.spec.endpoint != model.endpoint.id {
            return Err(crate::ConfigError::UnknownEndpoint(model.spec.endpoint.clone()).into());
        }
        if model.spec.protocol != Protocol::OpenAiResponses {
            return Err(crate::error::UnsupportedError::ResponsesOptions.into());
        }
        if request.model != model.spec.api_name {
            return Err(crate::ConfigError::Parse(format!(
                "compact request model {:?} does not match selected model {:?}",
                request.model, model.spec.api_name
            ))
            .into());
        }
        crate::protocol::openai_responses::validate_compact_reasoning(
            model,
            request.reasoning.as_ref(),
        )?;
        // Raw compact DTOs must cross the same replay-update authority boundary
        // as ResponsesCompactRequest::for_model, before credentials or dispatch.
        let baseline = request
            .reasoning
            .as_ref()
            .and_then(|reasoning| reasoning.get("effort"))
            .and_then(serde_json::Value::as_str)
            .and_then(crate::ReasoningConfig::from_provider_value)
            .or_else(|| model.spec.capabilities.reasoning.as_ref()
                .and_then(|capability| capability.default_selection()))
            .unwrap_or(crate::ReasoningConfig::Off);
        crate::responses::validate_responses_input(model, &request.input, &baseline, true)?;
        let rich_codex_schema = model
            .endpoint
            .runtime
            .responses_profile
            .supports_rich_compact_schema()
            || model.spec.cache.session_affinity_format
                == Some(crate::types::SessionAffinityFormat::Codex)
            || model.spec.capabilities.responses_lite;
        if !rich_codex_schema {
            // Public OpenAI compact has a narrower body than the private Codex
            // and Responses Lite contracts. Fail closed at the transport
            // boundary even for callers that constructed the public DTO
            // manually.
            request.tools = None;
            request.parallel_tool_calls = None;
            request.reasoning = None;
            request.text = None;
        }
        let url = model
            .endpoint
            .base_url
            .join("responses/compact")
            .map_err(|error| crate::error::ConfigError::Parse(error.to_string()))?;
        let proxy = self.request_proxy(&url)?;
        let body = serde_json::to_vec(&request)
            .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))?;
        let mut headers = model.endpoint.default_headers.clone();
        merge_preset_headers(&mut headers, &model.spec.preset.headers)?;
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        for (key, value) in crate::protocol::openai_responses::responses_affinity_headers(
            model,
            request.session_id.as_deref(),
        )? {
            if let Some(key) = key {
                headers.insert(key, value);
            }
        }
        // Codex compresses ordinary streaming Responses requests, but its
        // compact endpoint contract is plain JSON. Do not apply the normal
        // Responses transport compression policy here.
        let body = bytes::Bytes::from(body);
        let resolved_headers = crate::auth::resolve_headers(&model.endpoint.auth)
            .await
            .map_err(AiError::Auth)?;
        let mut diagnostic_redactor = resolved_headers.redactor;
        diagnostic_redactor.include_header_values(&headers);
        let mut current_key = None;
        for (key, value) in resolved_headers.headers {
            if let Some(key) = key {
                current_key = Some(key.clone());
                headers.insert(key, value);
            } else if let Some(key) = &current_key {
                headers.append(key.clone(), value);
            }
        }
        self.mark_request_dispatch();
        if let Some(proxy) = &proxy {
            diagnostic_redactor.include_proxy_url(proxy);
        }
        let response = tokio::time::timeout(
            model.endpoint.timeout,
            self.http.post(url).headers(headers).body(body).send(),
        )
        .await
        .map_err(|_| {
            AiError::Transport(TransportError {
                phase: TransportPhase::ResponseHeaders,
                timeout: true,
                message: "compact request timed out waiting for response headers".to_owned(),
            })
        })?
        .map_err(|error| request_open_transport_error(error, "compact request"))
        .map_err(|error| sanitize_ai_error(&diagnostic_redactor, error))?;
        Ok(PendingResponsesCompact {
            response,
            diagnostic_redactor,
            stream_idle_timeout: self.stream_idle_timeout,
            stream_initial_timeout: self.stream_initial_timeout,
            stream_deadline: self.stream_deadline,
            opened_at: Instant::now(),
        })
    }

    /// Submits one asynchronous OpenRouter Batch API job.
    ///
    /// The batch is deliberately not part of [`Self::stream`]: OpenRouter
    /// completes it asynchronously and returns results only after processing.
    /// `endpoint` must be the catalog's `openrouter` endpoint, and all request
    /// bodies must already use the selected batch endpoint's native JSON shape.
    pub async fn submit_openrouter_batch(
        &self,
        endpoint: &crate::types::Endpoint,
        request: crate::batch::OpenRouterBatchRequest,
    ) -> Result<crate::batch::OpenRouterBatch, AiError> {
        request.validate()?;
        let body = serde_json::to_vec(&request)
            .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))?;
        if body.len() > MAX_BATCH_BODY_BYTES {
            return Err(DecodeError::BodyTooLarge.into());
        }
        let value = batch_http_request(
            self,
            endpoint,
            http::Method::POST,
            openrouter_batch_url(endpoint)?,
            Some(bytes::Bytes::from(body)),
            "batch submission",
        )
        .await?;
        serde_json::from_value(value).map_err(|error| {
            AiError::Decode(DecodeError::Json(format!(
                "invalid OpenRouter batch response: {error}"
            )))
        })
    }

    /// Retrieves one OpenRouter Batch API job and its inline results, if ready.
    pub async fn get_openrouter_batch(
        &self,
        endpoint: &crate::types::Endpoint,
        id: &str,
    ) -> Result<crate::batch::OpenRouterBatch, AiError> {
        let value = batch_http_request(
            self,
            endpoint,
            http::Method::GET,
            openrouter_batch_item_url(endpoint, id)?,
            None,
            "batch retrieval",
        )
        .await?;
        serde_json::from_value(value).map_err(|error| {
            AiError::Decode(DecodeError::Json(format!(
                "invalid OpenRouter batch response: {error}"
            )))
        })
    }

    /// Lists OpenRouter Batch API jobs using cursor and status filters.
    pub async fn list_openrouter_batches(
        &self,
        endpoint: &crate::types::Endpoint,
        options: &crate::batch::OpenRouterBatchListOptions,
    ) -> Result<crate::batch::OpenRouterBatchList, AiError> {
        options.validate()?;
        let mut url = openrouter_batch_url(endpoint)?;
        {
            let mut query = url.query_pairs_mut();
            if let Some(limit) = options.limit {
                query.append_pair("limit", &limit.to_string());
            }
            if let Some(after) = &options.after {
                query.append_pair("after", after);
            }
            for status in &options.statuses {
                query.append_pair("status", status);
            }
            if let Some(created_after) = &options.created_after {
                query.append_pair("created_after", created_after);
            }
            if let Some(created_before) = &options.created_before {
                query.append_pair("created_before", created_before);
            }
        }
        let value = batch_http_request(
            self,
            endpoint,
            http::Method::GET,
            url,
            None,
            "batch listing",
        )
        .await?;
        serde_json::from_value(value).map_err(|error| {
            AiError::Decode(DecodeError::Json(format!(
                "invalid OpenRouter batch list response: {error}"
            )))
        })
    }

    /// Executes a request and drives the stream to completion, returning the final Response.
    pub async fn complete(&self, model: &Model, req: Request) -> Result<Response, AiError> {
        self.complete_with_overrides(model, req, crate::RequestOverrides::default()).await
    }

    /// Executes and collects one request using the same private overrides and
    /// no-retry contract as [`Self::stream_with_overrides`].
    pub async fn complete_with_overrides(&self, model: &Model, req: Request, overrides: crate::RequestOverrides) -> Result<Response, AiError> {
        self.complete_with_host_options(model, req, overrides, HostRequestOptions::default())
            .await
    }

    /// Executes and collects one request with host-owned runtime options.
    pub async fn complete_with_host_options(
        &self,
        model: &Model,
        req: Request,
        overrides: crate::RequestOverrides,
        host_options: HostRequestOptions,
    ) -> Result<Response, AiError> {
        let mut stream = self
            .stream_with_host_options(model, req, overrides, host_options)
            .await?;
        let mut final_response = None;

        while let Some(ev_res) = stream.next().await {
            let ev = ev_res?;
            if let StreamEvent::Finished(resp) = ev {
                final_response = Some(resp);
            }
        }

        final_response.ok_or_else(|| AiError::StreamProtocol(StreamProtocolError::MissingFinish))
    }

    /// The reqwest transport owned by this client, for crate-internal auxiliary
    /// APIs (the image-generation adapter) that share the same proxy snapshot.
    pub(crate) fn http_transport(&self) -> &reqwest::Client {
        &self.http
    }

    /// Resolves the proxy for `target` under this client's snapshot.
    pub(crate) fn proxy_for(&self, target: &url::Url) -> Result<Option<url::Url>, AiError> {
        self.request_proxy(target)
    }

    /// Clones this client with a request-local proxy overlay when `env` selects
    /// one of the proxy variables. The overlay never mutates process state or
    /// the original client; malformed values fail closed.
    pub(crate) fn with_environment_overlay(
        &self,
        env: &std::collections::BTreeMap<String, String>,
    ) -> Result<AiClient, AiError> {
        if !crate::declarations::proxy::ProxyEnvironment::NAMES
            .iter()
            .any(|name| env.contains_key(*name))
        {
            return Ok(self.clone());
        }
        let current = self.proxy_environment.as_ref().ok_or_else(|| {
            crate::ConfigError::Parse("proxy overrides require the built-in HTTP transport".into())
        })?;
        let proxy = Arc::new(current.overlay(env));
        let mut client = self.clone();
        client.http = proxy
            .clone()
            .configure(
                reqwest::Client::builder()
                    .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
                    .redirect(reqwest::redirect::Policy::none()),
            )
            .build()
            .map_err(|_| {
                crate::ConfigError::Parse("could not configure request-local HTTP transport".into())
            })?;
        client.proxy_environment = Some(proxy);
        Ok(client)
    }

    fn deferred_endpoint_transport(
        &self,
        model: &Model,
    ) -> Result<Arc<dyn HostStreamTransport>, AiError> {
        self.host_stream_transports
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&model.endpoint.id)
            .cloned()
            .ok_or_else(|| crate::error::UnsupportedError::Deferred.into())
    }

    fn validate_deferred_overrides(overrides: &crate::RequestOverrides) -> Result<(), AiError> {
        overrides
            .validate()
            .map_err(|error| crate::ConfigError::Parse(error.to_string()))?;
        if !overrides.headers.is_empty()
            || !overrides.sampling_params.is_empty()
            || overrides.azure.is_some()
            || overrides.max_retries.unwrap_or(0) != 0
            || overrides.max_retry_delay_ms.unwrap_or(0) != 0
        {
            return Err(crate::ConfigError::Parse(
                "deferred requests accept only request-local environment and timeout overrides".into(),
            )
            .into());
        }
        Ok(())
    }

    /// Submits one request that the provider may park instead of completing.
    ///
    /// A parked turn finishes with [`crate::StopReason::Deferred`] and a
    /// [`DeferredHandle`] on the response; it is never silently retried. Only a
    /// transport that implements
    /// [`HostStreamTransport::submit_deferred`] can park a request.
    /// `poll_after_ms` is the caller's request-local minimum delay before the
    /// next poll.
    pub async fn submit_deferred(
        &self,
        model: &Model,
        req: Request,
        overrides: crate::RequestOverrides,
        poll_after_ms: Option<u64>,
    ) -> Result<ResponseStream, AiError> {
        crate::catalog::validate_endpoint(&model.endpoint)?;
        crate::catalog::validate_model_spec(&model.spec)?;
        Self::validate_deferred_overrides(&overrides)?;
        let transport = self.deferred_endpoint_transport(model)?;
        let (request, diagnostics) = prepare_host_request(model, req)?;
        self.mark_request_dispatch();
        let stream = transport
            .submit_deferred(
                HostStreamModel::from(model),
                request,
                diagnostics,
                poll_after_ms,
            )
            .await?;
        Ok(crate::stream::guard(stream))
    }

    /// Polls one deferred handle under a one-shot, generation-bound permit.
    ///
    /// The permit is consumed before any provider work: a missing, already
    /// consumed, or stale permit fails closed, so one driving pass can never
    /// admit two billable polls. The caller owns the durable leaf generation it
    /// minted the permit for. `wait_ms` bounds the provider long-poll; `Some(0)`
    /// performs one status check.
    pub async fn fetch_deferred(
        &self,
        model: &Model,
        handle: DeferredHandle,
        mut permit: crate::deferred::DeferredPollPermit,
        leaf_generation: u64,
        wait_ms: Option<u64>,
    ) -> Result<ResponseStream, AiError> {
        crate::catalog::validate_endpoint(&model.endpoint)?;
        crate::catalog::validate_model_spec(&model.spec)?;
        permit.consume(leaf_generation)?;
        if handle.id.is_empty() {
            return Err(
                crate::ConfigError::Parse("deferred handle has an empty provider id".into())
                    .into(),
            );
        }
        if handle.model_id != model.spec.id.0 {
            return Err(crate::ConfigError::Parse(
                "deferred handle belongs to a different model".into(),
            )
            .into());
        }
        let transport = self.deferred_endpoint_transport(model)?;
        self.mark_request_dispatch();
        let stream = transport
            .fetch_deferred(HostStreamModel::from(model), handle, wait_ms)
            .await?;
        Ok(crate::stream::guard(stream))
    }

    /// Best-effort cancellation of one deferred handle.
    ///
    /// Cancellation does not un-send provider work or erase usage uncertainty;
    /// it only asks the owning transport to release the parked response.
    pub async fn cancel_deferred(
        &self,
        model: &Model,
        handle: DeferredHandle,
    ) -> Result<(), AiError> {
        crate::catalog::validate_endpoint(&model.endpoint)?;
        crate::catalog::validate_model_spec(&model.spec)?;
        if handle.model_id != model.spec.id.0 {
            return Err(crate::ConfigError::Parse(
                "deferred handle belongs to a different model".into(),
            )
            .into());
        }
        let transport = self.deferred_endpoint_transport(model)?;
        self.mark_request_dispatch();
        transport
            .cancel_deferred(HostStreamModel::from(model), handle)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn client_generated_websocket_errors_fence_pool_before_publication() {
        let catalog = crate::catalog::ModelCatalog::builtin().unwrap();
        let id = catalog
            .models()
            .find(|model| model.protocol == Protocol::OpenAiResponses)
            .unwrap()
            .id
            .clone();
        let model = catalog.resolve(&id).unwrap();
        for deadline in [Duration::ZERO, Duration::from_secs(5)] {
            let pool = ResponsesWsPool::default();
            let (sender, receiver) = crate::responses_ws::event_channel(1);
            sender
                .send(Ok(serde_json::json!({
                    "type": "error", "code": "invalid_request_error", "message": "invalid"
                })))
                .await
                .unwrap();
            // No actor exists to observe receiver closure or perform cleanup.
            // Both a client deadline and decoded provider error must fence the
            // session themselves before exposing failure to the consumer.
            let mut stream = responses_websocket_stream(
                pool.clone(),
                Some("poisoned".into()),
                model.clone(),
                None,
                receiver,
                Vec::new(),
                Vec::new(),
                false,
                CredentialRedactor::default(),
                Duration::from_secs(5),
                Duration::from_secs(5),
                deadline,
            );
            loop {
                match stream.next().await {
                    Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                    None => panic!("expected client failure"),
                }
            }
            let error = pool
                .request(
                    Some("poisoned"),
                    url::Url::parse("ws://127.0.0.1:9/").unwrap(),
                    http::HeaderMap::new(),
                    serde_json::json!({}),
                    ResponsesWsLiveness::for_response_idle(Duration::from_secs(5)),
                    Duration::from_secs(5),
                    None,
                )
                .await
                .unwrap_err();
            assert!(error
                .to_string()
                .contains("disabled after an earlier failure"));
            drop(sender);
        }
    }

    #[tokio::test]
    async fn declared_request_runtime_compresses_without_provider_identity() {
        let original = bytes::Bytes::from(vec![b'a'; 128 * 1024]);
        let mut headers = http::HeaderMap::new();
        let compressed = prepare_request_body(
            crate::types::RequestRuntime {
                body_encoding: crate::types::RequestBodyEncoding::Zstd,
                ..crate::types::RequestRuntime::default()
            },
            &mut headers,
            original.clone(),
        )
        .await;
        assert_eq!(headers[http::header::CONTENT_ENCODING], "zstd");
        assert!(compressed.len() < original.len() / 10);
        assert_eq!(
            zstd::stream::decode_all(compressed.as_ref()).unwrap(),
            original.as_ref()
        );

        let mut generic_headers = http::HeaderMap::new();
        let generic = prepare_request_body(
            crate::types::RequestRuntime::default(),
            &mut generic_headers,
            original.clone(),
        )
        .await;
        assert_eq!(generic, original);
        assert!(generic_headers
            .get(http::header::CONTENT_ENCODING)
            .is_none());
    }

    #[tokio::test]
    async fn transport_diagnostic_keeps_cause_but_removes_request_url() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let secret = "must-not-appear";
        let url = format!("http://{address}/private/catalog?token={secret}");
        let error = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .expect_err("the released listener must refuse the connection");
        let AiError::NetworkUnavailable(error) = request_open_transport_error(error, "request")
        else {
            unreachable!()
        };
        assert_eq!(error.phase, TransportPhase::Connect);
        assert!(error.message.starts_with("request connection failed:"));
        assert!(error.message.contains("refused") || error.message.contains("connect"));
        assert!(!error.message.contains(secret));
        assert!(!error.message.contains("/private/catalog"));
        assert!(!error.message.contains(&address.to_string()));
    }

    #[tokio::test]
    async fn stalled_tls_connect_timeout_is_network_unavailable_before_post() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (release, release_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            // No TLS acknowledgement, so HTTP POST cannot have been sent.
            let _ = release_rx.await;
        });
        let error = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_millis(50))
            .build()
            .unwrap()
            .post(format!("https://{address}/responses"))
            .body("not accepted")
            .send()
            .await
            .unwrap_err();
        let error = request_open_transport_error(error, "request");
        assert!(
            matches!(error, AiError::NetworkUnavailable(ref transport)
            if transport.phase == TransportPhase::Connect && transport.timeout),
            "{error:?}"
        );
        drop(release);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn invalid_tls_handshake_is_not_network_unavailable() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut hello = [0_u8; 4096];
            assert!(socket.read(&mut hello).await.unwrap() > 0);
            socket.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
        });
        let error = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("https://{address}/responses"))
            .send()
            .await
            .unwrap_err();
        assert!(error.is_connect());
        let error = request_open_transport_error(error, "request");
        assert!(
            matches!(error, AiError::Transport(ref transport)
            if transport.phase == TransportPhase::Connect && !transport.timeout),
            "{error:?}"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn disconnect_after_post_is_not_network_unavailable() {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let count = socket.read(&mut request).await.unwrap();
            assert!(count > 0, "POST reached the server before disconnect");
        });
        let error = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("http://{address}/responses"))
            .body("may already be accepted")
            .send()
            .await
            .unwrap_err();
        assert!(!error.is_connect());
        assert!(matches!(
            request_open_transport_error(error, "request"),
            AiError::Transport(TransportError {
                phase: TransportPhase::ResponseHeaders,
                ..
            })
        ));
        server.await.unwrap();
    }

    #[test]
    fn invalid_certificate_configuration_is_not_network_unavailable() {
        let certificate = reqwest::Certificate::from_der(b"invalid certificate").unwrap();
        let error = reqwest::Client::builder()
            .add_root_certificate(certificate)
            .build()
            .unwrap_err();
        assert!(!matches!(
            request_open_transport_error(error, "request"),
            AiError::NetworkUnavailable(_)
        ));
    }

    #[tokio::test]
    async fn dns_failure_requires_typed_transient_evidence() {
        struct FailedDns(std::io::ErrorKind);
        impl reqwest::dns::Resolve for FailedDns {
            fn resolve(&self, _name: reqwest::dns::Name) -> reqwest::dns::Resolving {
                let kind = self.0;
                Box::pin(async move {
                    Err(Box::new(std::io::Error::new(kind, "connection refused"))
                        as Box<dyn std::error::Error + Send + Sync>)
                })
            }
        }
        for (kind, transient) in [
            (std::io::ErrorKind::TimedOut, true),
            (std::io::ErrorKind::NetworkUnreachable, true),
            (std::io::ErrorKind::NotFound, false),
            (std::io::ErrorKind::Other, false),
        ] {
            let error = reqwest::Client::builder()
                .no_proxy()
                .dns_resolver(Arc::new(FailedDns(kind)))
                .build()
                .unwrap()
                .post("http://offline.invalid/responses")
                .send()
                .await
                .unwrap_err();
            assert!(error.is_connect());
            assert_eq!(
                matches!(
                    request_open_transport_error(error, "request"),
                    AiError::NetworkUnavailable(_)
                ),
                transient,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn connection_source_classification_uses_io_kinds_not_messages() {
        for kind in [
            std::io::ErrorKind::ConnectionRefused,
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::ConnectionAborted,
            std::io::ErrorKind::NetworkDown,
            std::io::ErrorKind::NetworkUnreachable,
            std::io::ErrorKind::HostUnreachable,
            std::io::ErrorKind::TimedOut,
        ] {
            assert!(transient_connection_source(&std::io::Error::new(
                kind,
                "invalid certificate"
            )));
        }
        for kind in [
            std::io::ErrorKind::InvalidData,
            std::io::ErrorKind::InvalidInput,
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::Other,
        ] {
            assert!(!transient_connection_source(&std::io::Error::new(
                kind,
                "connection refused; timed out"
            )));
        }
    }

    #[test]
    fn lifecycle_details_are_redacted_control_safe_and_bounded() {
        let mut headers = http::HeaderMap::new();
        headers.insert("authorization", "Bearer lifecycle-secret".parse().unwrap());
        let mut redactor = CredentialRedactor::default();
        redactor.include_header_values(&headers);
        let detail = format!("Bearer lifecycle-secret \x1b{}", "é".repeat(200));

        let lifecycle = parse_provider_lifecycle(&format!("loading; {detail}"), &redactor)
            .expect("known lifecycle state");
        assert_eq!(lifecycle.state, ProviderLifecycleState::Loading);
        let detail = lifecycle.detail.expect("nonempty detail");
        assert!(detail.len() <= MAX_PROVIDER_LIFECYCLE_DETAIL_BYTES);
        assert!(detail.is_char_boundary(detail.len()));
        assert!(detail.contains("[REDACTED]"));
        assert!(!detail.contains("lifecycle-secret"));
        assert!(!detail.chars().any(char::is_control));
        assert!(lifecycle_from_sse_comment("ordinary keepalive", &redactor).is_none());
        assert!(parse_provider_lifecycle("unknown; ignored", &redactor).is_none());
    }

    #[test]
    fn provider_diagnostics_are_control_safe_and_post_sanitize_bounded() {
        let input = format!("\x1b\x07\u{202e}{}", "é".repeat(3_000));
        let output = sanitize_diagnostic(
            &CredentialRedactor::default(),
            &input,
            MAX_PROVIDER_DIAGNOSTIC_BYTES,
        );
        assert!(output.len() <= MAX_PROVIDER_DIAGNOSTIC_BYTES);
        assert!(output.is_char_boundary(output.len()));
        assert!(output.ends_with('…'));
        assert!(!output.chars().any(char::is_control));
        assert!(!output.contains('\u{202e}'));
        assert!(output.contains(r"\u{1b}"));
        assert!(output.contains(r"\u{7}"));
        assert!(output.contains(r"\u{202e}"));
    }

    #[test]
    fn transport_diagnostic_truncation_preserves_utf8_boundaries() {
        let mut message = format!("{}étail", "a".repeat(511));
        truncate_transport_message(&mut message, 512);
        assert_eq!(message.len(), 511);
        assert!(message.chars().all(|character| character == 'a'));
    }
}
