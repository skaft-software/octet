//! The provider trust boundary: redaction, inert rendering, and size bounds.
//!
//! Every byte octet prints, persists, or returns from a provider crosses a
//! trust boundary first. This module owns that crossing. Its single rule is
//! that a provider-controlled string must pass through `sanitize_diagnostic`
//! before it is stored anywhere a human or a log will read it: request
//! credentials are redacted, control and bidirectional-format characters are
//! escaped to their literal spelling, and the result is hard-bounded.
//!
//! It is separate from `client` and from `error` because the redaction has to
//! apply uniformly to every variant of [`AiError`] and to the advisory
//! lifecycle telemetry, none of which belong to any one dispatch path. Keeping
//! the bounds and the escaping in one place is what makes the guarantee
//! checkable: a new error variant either names its fields here or deliberately
//! carries only octet-owned wording.
//!
//! [`AiError`]: crate::error::AiError

use crate::auth::CredentialRedactor;
use crate::error::{AiError, DecodeError, ProviderError, StreamProtocolError};
use crate::stream::{ProviderLifecycle, ProviderLifecycleState};
pub(super) const MAX_PROVIDER_DIAGNOSTIC_BYTES: usize = 4096;
const MAX_DIAGNOSTIC_METADATA_BYTES: usize = 512;
/// A stream can surface only a finite amount of advisory endpoint telemetry.
pub(super) const MAX_PROVIDER_LIFECYCLE_EVENTS: usize = 64;
/// Lifecycle detail remains brief enough for a status row and cannot retain an
/// endpoint-controlled unbounded string.
pub(super) const MAX_PROVIDER_LIFECYCLE_DETAIL_BYTES: usize = 160;
pub(super) const LIFECYCLE_HEADER: &str = "x-octet-lifecycle";
pub(super) const LIFECYCLE_REQUEST_VALUE: &str = "1";
const LIFECYCLE_COMMENT_PREFIX: &str = "octet-lifecycle:";
/// Parses octet's explicitly negotiated OpenAI-compatible lifecycle value.
///
/// The wire form is `state` or `state; detail`, where state is one of
/// `queued`, `loading`, or `ready`. Unknown states and malformed namespaces
/// are deliberately ignored: this is advisory telemetry, never assistant text.
pub(super) fn parse_provider_lifecycle(
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
pub(super) fn lifecycle_from_sse_comment(
    comment: &str,
    diagnostic_redactor: &CredentialRedactor,
) -> Option<ProviderLifecycle> {
    parse_provider_lifecycle(
        comment.strip_prefix(LIFECYCLE_COMMENT_PREFIX)?.trim_start(),
        diagnostic_redactor,
    )
}
pub(super) fn truncate_transport_message(message: &mut String, max_bytes: usize) {
    if message.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
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
pub(super) fn sanitize_diagnostic(
    redactor: &CredentialRedactor,
    input: &str,
    max_bytes: usize,
) -> String {
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

// A provider can echo even a one-character or transformed excerpt in any
// body field. Release diagnostics classify the body, never quote it.
fn sanitize_http_error(redactor: &CredentialRedactor, error: &mut crate::error::HttpError) {
    sanitize_optional_diagnostic(
        redactor,
        &mut error.request_id,
        MAX_DIAGNOSTIC_METADATA_BYTES,
    );
    let raw = error.body_snippet.take();
    let json = raw
        .as_deref()
        .and_then(|body| serde_json::from_str::<serde_json::Value>(body).ok());
    let payload = json
        .as_ref()
        .map(|value| value.get("error").unwrap_or(value));
    let field = |name| {
        payload
            .and_then(|value| value.get(name))
            .and_then(json_scalar_string)
    };
    let known_code = |code: &str| {
        // A numeric permanent code (for example 422 behind a gateway 503)
        // vetoes retries, so keep it as its canonical status number only.
        if let Some(status) = code
            .parse::<u16>()
            .ok()
            .filter(|_| crate::error::permanent_provider_code(code))
        {
            return Some(status.to_string());
        }
        (code == error.status.as_str()
            || (code.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                && crate::error::permanent_provider_code(code))
            || matches!(
                code,
                "invalid_request"
                    | "invalid_reasoning_pair"
                    | "rate_limit_exceeded"
                    | "request_too_large"
                    | "model_context_window_exceeded"
                    | "INVALID_ARGUMENT"
                    | "RESOURCE_EXHAUSTED"
                    | "UNAUTHENTICATED"
                    | "PERMISSION_DENIED"
            ))
        .then(|| code.to_owned())
    };
    let kind = field("type")
        .or_else(|| field("status"))
        .as_deref()
        .and_then(known_code);
    let code = error
        .provider_code
        .as_deref()
        .and_then(known_code)
        .or_else(|| field("code").as_deref().and_then(known_code));
    let remote_message = if let Some(payload) = payload {
        ["message", "detail", "description"]
            .iter()
            .find_map(|field| payload.get(*field).and_then(serde_json::Value::as_str))
            .or_else(|| payload.as_str())
            .or_else(|| json.as_ref()?.get("detail")?.as_str())
    } else {
        raw.as_deref()
    };
    let context_overflow = matches!(
        code.as_deref().or(kind.as_deref()),
        Some("context_length_exceeded" | "model_context_window_exceeded")
    ) || remote_message.is_some_and(crate::error::mentions_context_limit);
    let message = if context_overflow && matches!(error.status.as_u16(), 400 | 413 | 422) {
        "Input exceeds the context window. Reduce the conversation size and try again."
    } else if code.as_deref() == Some("invalid_reasoning_pair") {
        "The provider requires a reasoning item and its paired function call to be replayed together."
    } else {
        match error.status.as_u16() {
            401 | 403 => "The provider denied access. Check the configured credentials and model permissions.",
            404 => "The provider could not find the requested route or model.",
            413 => "The provider rejected the request size. Use fewer or smaller attachments.",
            429 => "The provider limited this request. Check rate limits and quota.",
            500..=599 => "The provider or gateway failed to handle the request.",
            _ => "The provider rejected the request. Check the model and request settings.",
        }
    };
    // Preserve classification, not remote prose, across the HTTP diagnostic boundary.
    error.provider_code = code.clone().or_else(|| kind.clone());
    if error.provider_code.as_deref() == Some("slow_down")
        && error.status == http::StatusCode::TOO_MANY_REQUESTS
        && remote_message.is_some_and(crate::error::mentions_rate_ramp)
    {
        error.provider_code = Some(
            kind.as_deref()
                .filter(|kind| *kind != "slow_down" && crate::error::permanent_provider_code(kind))
                .unwrap_or("rate_limit_exceeded")
                .into(),
        );
    }
    if raw.is_some() || error.provider_code.is_some() {
        let mut fields = serde_json::Map::from_iter([("message".into(), message.into())]);
        if let Some(code) = code {
            fields.insert("code".into(), code.into());
        }
        if let Some(kind) = kind {
            fields.insert("type".into(), kind.into());
        }
        error.body_snippet = Some(serde_json::json!({"error": fields}).to_string());
    }
}

pub(crate) fn sanitize_ai_error(redactor: &CredentialRedactor, mut error: AiError) -> AiError {
    match &mut error {
        AiError::Http(error) => sanitize_http_error(redactor, error),
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
pub(super) fn json_scalar_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

/// Some OpenAI-compatible servers return a JSON error envelope with HTTP 200
/// for request-validation failures. Detect that envelope before the empty SSE
/// stream is misreported as a missing terminal event.
pub(super) fn provider_error_from_success_body(body: &[u8]) -> Option<ProviderError> {
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
