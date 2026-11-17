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

const MAX_HTTP_MESSAGE_CHARS: usize = 200;
const MIN_REQUEST_ECHO_CHARS: usize = 24;
const MAX_REQUEST_TEXT_BYTES: usize = 64 * 1024;
const MAX_REQUEST_TEXT_PARTS: usize = 4096;

/// Fixed-width character windows make echo matching linear in the bounded
/// request text, rather than searching the request again for every substring.
fn echo_windows(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.char_indices()
        .map(|(start, _)| start)
        .zip(
            text.char_indices()
                .skip(MIN_REQUEST_ECHO_CHARS)
                .map(|(end, _)| end)
                .chain(std::iter::once(text.len())),
        )
        .filter_map(move |(start, end)| {
            (text[start..end].chars().count() == MIN_REQUEST_ECHO_CHARS)
                .then_some((start, &text[start..end]))
        })
}

/// Visit text lazily with a single budget shared across messages and parts,
/// including empty/media parts. No history-sized snapshot is retained.
fn visit_request_text(request: &crate::Request, mut visit: impl FnMut(&str) -> bool) {
    use crate::{AssistantPart, Message, ToolResultPart, UserPart};
    let mut remaining_parts = MAX_REQUEST_TEXT_PARTS;
    for message in request.messages.iter().rev().take(MAX_REQUEST_TEXT_PARTS) {
        match message {
            Message::User(message) => {
                for part in &message.content {
                    if remaining_parts == 0 {
                        return;
                    }
                    remaining_parts -= 1;
                    match part {
                        UserPart::Text(text) => {
                            if !visit(text) {
                                return;
                            }
                        }
                        UserPart::ToolResult(result) => {
                            for part in &result.content {
                                if remaining_parts == 0 {
                                    return;
                                }
                                remaining_parts -= 1;
                                if let ToolResultPart::Text(text) = part {
                                    if !visit(text) {
                                        return;
                                    }
                                }
                            }
                        }
                        UserPart::Media(_) => {}
                    }
                }
            }
            Message::Assistant(message) => {
                for part in &message.content {
                    if remaining_parts == 0 {
                        return;
                    }
                    remaining_parts -= 1;
                    let text = match part {
                        AssistantPart::Text(text) => text,
                        AssistantPart::ToolCall(call) => &call.arguments_json,
                        _ => continue,
                    };
                    if !visit(text) {
                        return;
                    }
                }
            }
        }
    }
    for text in request
        .system
        .as_deref()
        .into_iter()
        .chain(request.tools.iter().map(|tool| tool.description.as_str()))
    {
        if remaining_parts == 0 {
            return;
        }
        remaining_parts -= 1;
        if !visit(text) {
            return;
        }
    }
}

fn scrub_request_echoes(message: &str, request: &crate::Request) -> String {
    let mut candidates: std::collections::HashSet<&str> =
        echo_windows(message).map(|(_, text)| text).collect();
    if candidates.is_empty() {
        return message.to_owned();
    }
    let mut matches = std::collections::HashSet::new();
    // Inspect recent conversation text first, then system and tool descriptions.
    // This runs only on HTTP failure, with both byte and part-count bounds.
    let mut remaining = MAX_REQUEST_TEXT_BYTES;
    visit_request_text(request, |text| {
        let mut end = text.len().min(remaining);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        for (_, window) in echo_windows(&text[..end]) {
            if let Some(candidate) = candidates.take(window) {
                matches.insert(candidate);
            }
        }
        remaining -= end;
        remaining > 0 && !candidates.is_empty()
    });
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for (start, window) in echo_windows(message) {
        if matches.contains(window) {
            let end = start + window.len();
            if let Some((_, previous_end)) = spans.last_mut().filter(|(_, end)| start <= *end) {
                *previous_end = end;
            } else {
                spans.push((start, end));
            }
        }
    }
    let mut output = String::new();
    let mut copied = 0;
    for (start, end) in spans {
        let matched = &message[start..end];
        // Preserve existing surrounding whitespace for readable recovery text.
        let (start, end) = if matched.trim().is_empty() {
            (start, end)
        } else {
            (
                start + matched.len() - matched.trim_start().len(),
                end - (matched.len() - matched.trim_end().len()),
            )
        };
        output.push_str(&message[copied..start]);
        output.push_str("[request text]");
        copied = end;
    }
    output.push_str(&message[copied..]);
    output
}

fn scrub_long_quotes(message: &str) -> String {
    let mut output = String::new();
    let mut copied = 0;
    let mut opening = None;
    let mut escaped = false;
    for (index, character) in message.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if !matches!(character, '\'' | '"' | '`') {
            continue;
        }
        // An apostrophe inside a word is not a quote delimiter.
        if character == '\''
            && message[..index].ends_with(char::is_alphanumeric)
            && message[index + 1..].starts_with(char::is_alphanumeric)
        {
            continue;
        }
        if let Some((start, quote)) = opening {
            if character == quote {
                if message[start + 1..index].chars().count() > 48 {
                    output.push_str(&message[copied..start]);
                    output.push_str("[quoted text]");
                    copied = index + 1;
                }
                opening = None;
            }
        } else if character != '\'' || !message[..index].ends_with(char::is_alphanumeric) {
            opening = Some((index, character));
        }
    }
    // A bounded/truncated body may end before the closing delimiter.
    if let Some((start, _)) = opening {
        if message[start + 1..].chars().count() > 48 {
            output.push_str(&message[copied..start]);
            output.push_str("[quoted text]");
            copied = message.len();
        }
    }
    output.push_str(&message[copied..]);
    output
}

fn is_stack_frame(text: &str) -> bool {
    text.split_once('(')
        .is_some_and(|(name, _)| name.contains('.') && !name.contains(char::is_whitespace))
}

fn sanitize_http_message(
    redactor: &CredentialRedactor,
    message: &str,
    request: Option<&crate::Request>,
) -> Option<String> {
    let mut message = message.split(['\r', '\n']).next()?.trim();
    while let Some((class, human)) = message.split_once(':') {
        if (class.ends_with("Error") || class.ends_with("Exception"))
            && class
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$'))
        {
            message = human.trim();
        } else {
            break;
        }
    }
    // A first-line frame/trace has no human diagnostic to retain. Inline stack
    // material is removed without looking at or retaining subsequent lines.
    if message.starts_with("Traceback")
        || message.starts_with("File \"")
        || message.starts_with("File '")
        || message.strip_prefix("at ").is_some_and(is_stack_frame)
    {
        return None;
    }
    for marker in ["Traceback (", " at "] {
        if let Some((human, stack)) = message.split_once(marker) {
            if marker.starts_with("Traceback") || is_stack_frame(stack) {
                message = human.trim_end();
            }
        }
    }
    // Bound untrusted input even for callers that did not read a capped body.
    let mut end = message.len().min(MAX_PROVIDER_DIAGNOSTIC_BYTES);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    // Redact before any replacement/cut too: an echo or quote can overlap a
    // credential, and replacing only that overlap must not expose its remainder.
    let redacted = redactor.redact(&message[..end]);
    let scrubbed = request.map_or_else(
        || redacted.clone(),
        |request| scrub_request_echoes(&redacted, request),
    );
    let scrubbed = scrub_long_quotes(&scrubbed);
    if !scrubbed
        .replace("[request text]", "")
        .replace("[quoted text]", "")
        .chars()
        .any(char::is_alphanumeric)
    {
        return None;
    }
    let mut scrubbed = scrubbed;
    if let Some((end, _)) = scrubbed.char_indices().nth(MAX_HTTP_MESSAGE_CHARS) {
        scrubbed.truncate(end);
        scrubbed.pop();
        scrubbed.push('…');
    }
    (!scrubbed.trim().is_empty())
        .then(|| sanitize_diagnostic(redactor, &scrubbed, MAX_PROVIDER_DIAGNOSTIC_BYTES / 4))
}

// Pi compatibility: packages/ai/src/api/openai-codex-responses.ts:1613-1642
// extracts error.message and error.code/error.type before falling back to status.
// octet additionally scrubs and caps human messages per AGENTS.md's privacy rule.
// Parse before escaping: escaping pretty-printed JSON's whitespace first makes
// it invalid JSON and hides the useful error behind a bare HTTP 400.
fn sanitize_http_body(
    redactor: &CredentialRedactor,
    body: &str,
    request: Option<&crate::Request>,
) -> Option<String> {
    let value: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        // Malformed/truncated JSON cannot safely identify a message field.
        Err(_) if body.trim_start().starts_with(['{', '[']) => return None,
        Err(_) => return sanitize_http_message(redactor, body, request),
    };
    let error = value.get("error").unwrap_or(&value);
    let mut fields = serde_json::Map::new();
    for field in ["code", "type"] {
        if let Some(value) = error.get(field).and_then(json_scalar_string) {
            fields.insert(
                field.into(),
                serde_json::Value::String(sanitize_diagnostic(
                    redactor,
                    &value,
                    MAX_DIAGNOSTIC_METADATA_BYTES / 2,
                )),
            );
        }
    }
    if let Some(message) = ["message", "detail", "description"]
        .iter()
        .find_map(|field| error.get(*field).and_then(serde_json::Value::as_str))
        .or_else(|| error.as_str())
    {
        if let Some(message) = sanitize_http_message(redactor, message, request) {
            fields.insert("message".into(), serde_json::Value::String(message));
        }
    }
    if fields.is_empty() {
        return None;
    }
    // Only selected, scrubbed diagnostic fields cross this boundary.
    // Per-field caps leave room for JSON escaping within the body snippet cap.
    Some(serde_json::json!({"error": fields}).to_string())
}

pub(crate) fn sanitize_ai_error(redactor: &CredentialRedactor, error: AiError) -> AiError {
    sanitize_request_error(redactor, error, None)
}

pub(super) fn sanitize_request_error(
    redactor: &CredentialRedactor,
    mut error: AiError,
    request: Option<&crate::Request>,
) -> AiError {
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
            if let Some(body) = &mut error.body_snippet {
                if error
                    .provider_code
                    .as_ref()
                    .is_none_or(|code| code.trim().is_empty())
                {
                    error.provider_code = serde_json::from_str::<serde_json::Value>(body)
                        .ok()
                        .and_then(|value| {
                            let error = value.get("error").unwrap_or(&value);
                            error
                                .get("code")
                                .and_then(json_scalar_string)
                                .filter(|code| !code.trim().is_empty())
                                .or_else(|| error.get("type").and_then(json_scalar_string))
                        })
                        .map(|kind| {
                            sanitize_diagnostic(redactor, &kind, MAX_DIAGNOSTIC_METADATA_BYTES)
                        });
                }
                error.body_snippet = sanitize_http_body(redactor, body, request);
            }
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
            **inner = sanitize_request_error(redactor, drained, request);
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
