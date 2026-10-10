//! The OpenRouter batch HTTP surface.
//!
//! Batch submission, polling, and cancellation are a second, non-streaming
//! protocol living behind the same `AiClient`. It is isolated here because it
//! shares almost nothing with generation dispatch: no codec, no
//! [`ResponseBuilder`], no SSE — only the auth resolution, the redaction, and
//! the bounded body reader, all of which it borrows rather than reimplements.
//!
//! Keeping it out of `client` is also what keeps the batch path honest about
//! its own limits: the body cap and the error-snippet cap are the only place a
//! provider response is buffered in full, so they are stated next to the code
//! that enforces them rather than in the dispatch module.
//!
//! [`ResponseBuilder`]: crate::stream::ResponseBuilder

use std::time::{Duration, Instant};

use super::diagnostics::{json_scalar_string, sanitize_ai_error};
use super::transport::{
    next_body_chunk, request_open_transport_error, MAX_ERROR_BODY_DEADLINE,
    MAX_ERROR_BODY_IDLE_TIMEOUT,
};
use super::AiClient;
use crate::error::{AiError, DecodeError, HttpError, TransportError, TransportPhase};
pub(super) const MAX_BATCH_BODY_BYTES: usize = 256 * 1024 * 1024;
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

pub(super) fn openrouter_batch_url(endpoint: &crate::types::Endpoint) -> Result<url::Url, AiError> {
    if endpoint.id.0 != "openrouter" {
        return Err(crate::batch::BatchError::UnsupportedProvider(endpoint.id.0.clone()).into());
    }
    crate::catalog::validate_endpoint(endpoint)?;
    endpoint
        .base_url
        .join("../beta/batches")
        .map_err(|error| crate::error::ConfigError::Parse(error.to_string()).into())
}

pub(super) fn openrouter_batch_item_url(
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

pub(super) async fn batch_http_request(
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
