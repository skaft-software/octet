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

pub(crate) fn sanitize_ai_error(redactor: &CredentialRedactor, mut error: AiError) -> AiError {
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
