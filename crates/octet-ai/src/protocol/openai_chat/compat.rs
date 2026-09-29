//! Compatibility tool-call recovery for OpenAI-compatible servers.
//!
//! Some OpenAI-compatible servers never populate `delta.tool_calls` and return
//! a tool call as model text instead: Qwen emits `<tool_call>…</tool_call>`
//! XML, others emit a bare JSON envelope, and a few emit a
//! `[tool_output_locked]` placeholder that must be kept out of the transcript.
//! This module owns that whole path — the incremental marker scanner, the
//! buffered emitter, and the XML/JSON parsers — because it is the only place
//! in the codec where emitted events are derived from text the provider never
//! labelled as a call, and it is therefore the only place where a partial
//! parse can invent or lose a tool call.
//!
//! It is kept out of `stream` and `response` on purpose: both of those trust
//! the provider's framing, while everything here has to re-derive it, and
//! merging the two would hide that distinction in the middle of a state
//! machine.

use crate::error::{AiError, DecodeError};
use crate::protocol::emit_event;
use crate::stream::{
    OpenAiChatCompatibilityState, ResponseBuilder, StreamEvent, MAX_RESPONSE_PARTS,
    MAX_TOOL_ARGUMENT_BYTES,
};
use crate::types::{StopReason, ToolCallId};

use super::stream::{get_canonical_index, segment_index};

const QWEN_XML_CLOSE: &str = "</tool_call>";
const TOOL_CALL_OPEN_PREFIX: &str = "<tool_call";
const FUNCTION_OPEN_PREFIX: &str = "<function";
pub(super) const TOOL_OUTPUT_LOCKED: &str = "[tool_output_locked]";

fn emit_text_delta(
    events: &mut Vec<StreamEvent>,
    builder: &mut ResponseBuilder,
    text: &str,
) -> Result<(), AiError> {
    if text.is_empty() {
        return Ok(());
    }
    let idx = segment_index(builder, "text");
    if !builder.text_buffers.contains_key(&idx) {
        emit_event(events, builder, StreamEvent::TextStart { index: idx })?;
    }
    emit_event(
        events,
        builder,
        StreamEvent::TextDelta {
            index: idx,
            delta: text.to_owned(),
        },
    )
}

/// Consume assistant content while keeping compatibility tool syntax and local
/// control placeholders out of the visible transcript. Native structured calls
/// remain authoritative; this path is only used when an OpenAI-compatible
/// server returned model text instead of `delta.tool_calls`.
pub(super) fn consume_qwen_xml_content(
    events: &mut Vec<StreamEvent>,
    builder: &mut ResponseBuilder,
    delta: &str,
) -> Result<(), AiError> {
    builder.reserve_buffered_content(delta.len())?;
    builder.qwen_xml_pending.push_str(delta);
    // Prefixes are consumed logically and compacted once before returning.
    // This keeps a single event containing many compatibility blocks linear:
    // repeatedly draining a String prefix would shift the remaining event for
    // every block.
    let mut pending_head = 0usize;

    loop {
        match builder.qwen_xml_state {
            OpenAiChatCompatibilityState::BareJson => {
                compact_pending_prefix(builder, pending_head);
                return Ok(());
            }
            OpenAiChatCompatibilityState::Scanning { scan_from } => {
                let pending = &builder.qwen_xml_pending[pending_head..];
                if builder.buffer_ambiguous_compatibility_content && json_tool_candidate(pending) {
                    // Bare JSON is ambiguous until EOF. Holding it is a lossy
                    // compatibility behavior and therefore must be explicitly
                    // enabled by the request; strict/default streams expose it
                    // immediately as ordinary assistant text.
                    builder.qwen_xml_state = OpenAiChatCompatibilityState::BareJson;
                    compact_pending_prefix(builder, pending_head);
                    return Ok(());
                }

                let Some((index, marker)) = earliest_compat_marker(pending, scan_from) else {
                    if !builder.qwen_xml_buffered_calls.is_empty() {
                        // Preserve ordering after a deferred call, but resume
                        // searching at the unexamined suffix next time.
                        builder.qwen_xml_state = OpenAiChatCompatibilityState::Scanning {
                            scan_from: next_scan_offset(pending, 0, MAX_COMPAT_MARKER_LEN),
                        };
                        compact_pending_prefix(builder, pending_head);
                        return Ok(());
                    }
                    let keep = compatibility_marker_suffix_len(
                        pending,
                        builder.buffer_ambiguous_compatibility_content,
                    );
                    let flush_len = pending.len().saturating_sub(keep);
                    if flush_len == 0 {
                        builder.qwen_xml_state =
                            OpenAiChatCompatibilityState::Scanning { scan_from: 0 };
                        compact_pending_prefix(builder, pending_head);
                        return Ok(());
                    }
                    let text = take_pending_prefix_at(builder, &mut pending_head, flush_len);
                    builder.qwen_xml_state =
                        OpenAiChatCompatibilityState::Scanning { scan_from: 0 };
                    compact_pending_prefix(builder, pending_head);
                    if builder.qwen_xml_call_count == 0 || !text.trim().is_empty() {
                        emit_text_delta(events, builder, &text)?;
                    }
                    return Ok(());
                };

                if index > 0 {
                    let text = take_pending_prefix_at(builder, &mut pending_head, index);
                    if !text.trim().is_empty() {
                        emit_text_delta(events, builder, &text)?;
                    }
                }

                match marker {
                    CompatMarker::ToolOutputLocked => {
                        skip_pending_prefix_at(
                            builder,
                            &mut pending_head,
                            TOOL_OUTPUT_LOCKED.len(),
                        );
                        builder.tool_output_locked_seen = true;
                        builder.qwen_xml_state =
                            OpenAiChatCompatibilityState::Scanning { scan_from: 0 };
                    }
                    CompatMarker::ToolCall => {
                        builder.qwen_xml_state = OpenAiChatCompatibilityState::ToolCallOpen {
                            scan_from: TOOL_CALL_OPEN_PREFIX.len(),
                        };
                    }
                    CompatMarker::Function => {
                        builder.qwen_xml_state = OpenAiChatCompatibilityState::FunctionBody {
                            scan_from: FUNCTION_OPEN_PREFIX.len(),
                        };
                    }
                }
            }
            OpenAiChatCompatibilityState::ToolCallOpen { scan_from } => {
                let pending = &builder.qwen_xml_pending[pending_head..];
                let Some(open_end) = pending[scan_from..]
                    .find('>')
                    .map(|offset| scan_from + offset + 1)
                else {
                    builder.qwen_xml_state = OpenAiChatCompatibilityState::ToolCallOpen {
                        scan_from: pending.len(),
                    };
                    compact_pending_prefix(builder, pending_head);
                    return Ok(());
                };
                builder.qwen_xml_state = OpenAiChatCompatibilityState::ToolCallBody {
                    open_end,
                    scan_from: open_end,
                };
            }
            OpenAiChatCompatibilityState::ToolCallBody {
                open_end,
                scan_from,
            } => {
                let pending = &builder.qwen_xml_pending[pending_head..];
                let Some(close) = pending[scan_from..]
                    .find(QWEN_XML_CLOSE)
                    .map(|offset| scan_from + offset)
                else {
                    builder.qwen_xml_state = OpenAiChatCompatibilityState::ToolCallBody {
                        open_end,
                        scan_from: next_scan_offset(pending, open_end, QWEN_XML_CLOSE.len()),
                    };
                    compact_pending_prefix(builder, pending_head);
                    return Ok(());
                };
                let block = pending[open_end..close].to_owned();
                let consumed = close + QWEN_XML_CLOSE.len();
                skip_pending_prefix_at(builder, &mut pending_head, consumed);
                builder.qwen_xml_state = OpenAiChatCompatibilityState::Scanning { scan_from: 0 };
                let calls = parse_compat_tool_calls(&block)?;
                buffer_compat_tool_calls(builder, calls)?;
            }
            OpenAiChatCompatibilityState::FunctionBody { scan_from } => {
                const FUNCTION_CLOSE: &str = "</function>";
                let pending = &builder.qwen_xml_pending[pending_head..];
                let Some(close) = pending[scan_from..]
                    .find(FUNCTION_CLOSE)
                    .map(|offset| scan_from + offset + FUNCTION_CLOSE.len())
                else {
                    builder.qwen_xml_state = OpenAiChatCompatibilityState::FunctionBody {
                        scan_from: next_scan_offset(
                            pending,
                            FUNCTION_OPEN_PREFIX.len(),
                            FUNCTION_CLOSE.len(),
                        ),
                    };
                    compact_pending_prefix(builder, pending_head);
                    return Ok(());
                };
                builder.qwen_xml_state =
                    OpenAiChatCompatibilityState::FunctionClosed { close_end: close };
            }
            OpenAiChatCompatibilityState::FunctionClosed { close_end } => {
                let pending = &builder.qwen_xml_pending[pending_head..];
                let trailing = &pending[close_end..];
                if trailing.len() < QWEN_XML_CLOSE.len() && QWEN_XML_CLOSE.starts_with(trailing) {
                    compact_pending_prefix(builder, pending_head);
                    return Ok(());
                }
                let block = pending[..close_end].to_owned();
                let consumed = if trailing.starts_with(QWEN_XML_CLOSE) {
                    close_end + QWEN_XML_CLOSE.len()
                } else {
                    close_end
                };
                skip_pending_prefix_at(builder, &mut pending_head, consumed);
                builder.qwen_xml_state = OpenAiChatCompatibilityState::Scanning { scan_from: 0 };
                let calls = parse_compat_tool_calls(&block)?;
                buffer_compat_tool_calls(builder, calls)?;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompatMarker {
    ToolOutputLocked,
    ToolCall,
    Function,
}

const MAX_COMPAT_MARKER_LEN: usize = TOOL_OUTPUT_LOCKED.len();

fn earliest_compat_marker(input: &str, scan_from: usize) -> Option<(usize, CompatMarker)> {
    let suffix = &input[scan_from.min(input.len())..];
    [
        suffix
            .find(TOOL_OUTPUT_LOCKED)
            .map(|index| (scan_from + index, CompatMarker::ToolOutputLocked)),
        suffix
            .find(TOOL_CALL_OPEN_PREFIX)
            .map(|index| (scan_from + index, CompatMarker::ToolCall)),
        suffix
            .find(FUNCTION_OPEN_PREFIX)
            .map(|index| (scan_from + index, CompatMarker::Function)),
    ]
    .into_iter()
    .flatten()
    .min_by_key(|(index, _)| *index)
}

fn next_scan_offset(input: &str, floor: usize, marker_len: usize) -> usize {
    let mut offset = input
        .len()
        .saturating_sub(marker_len.saturating_sub(1))
        .max(floor);
    while offset > floor && !input.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn take_pending_prefix_at(
    builder: &mut ResponseBuilder,
    pending_head: &mut usize,
    bytes: usize,
) -> String {
    let start = *pending_head;
    let end = start + bytes;
    let value = builder.qwen_xml_pending[start..end].to_owned();
    *pending_head = end;
    builder.release_buffered_content(bytes);
    value
}

fn skip_pending_prefix_at(builder: &mut ResponseBuilder, pending_head: &mut usize, bytes: usize) {
    *pending_head += bytes;
    builder.release_buffered_content(bytes);
}

#[cfg(test)]
#[derive(Clone, Copy, Default)]
pub(super) struct PendingWork {
    pub(super) scanned_bytes: usize,
    pub(super) compactions: usize,
    pub(super) shifted_bytes: usize,
}

#[cfg(test)]
thread_local! {
    pub(super) static PENDING_WORK: std::cell::Cell<PendingWork> = const {
        std::cell::Cell::new(PendingWork { scanned_bytes: 0, compactions: 0, shifted_bytes: 0 })
    };
}

fn compact_pending_prefix(builder: &mut ResponseBuilder, pending_head: usize) {
    if pending_head > 0 {
        #[cfg(test)]
        PENDING_WORK.with(|work| {
            let mut count = work.get();
            count.compactions += 1;
            count.shifted_bytes += builder.qwen_xml_pending.len() - pending_head;
            work.set(count);
        });
        builder.qwen_xml_pending.drain(..pending_head);
    }
}

fn buffer_compat_tool_calls(
    builder: &mut ResponseBuilder,
    calls: Vec<(String, String)>,
) -> Result<(), AiError> {
    if builder
        .qwen_xml_buffered_calls
        .len()
        .checked_add(calls.len())
        .is_none_or(|count| count > MAX_RESPONSE_PARTS)
    {
        return Err(AiError::Decode(DecodeError::TooManyResponseParts));
    }
    let mut bytes = 0usize;
    for (name, arguments) in &calls {
        if arguments.len() > MAX_TOOL_ARGUMENT_BYTES {
            return Err(AiError::Decode(DecodeError::ToolArgumentsTooLarge));
        }
        bytes = bytes
            .checked_add(name.len())
            .and_then(|total| total.checked_add(arguments.len()))
            .ok_or(AiError::Decode(DecodeError::ResponseTooLarge))?;
    }
    builder.reserve_buffered_content(bytes)?;
    builder.qwen_xml_buffered_calls.extend(calls);
    Ok(())
}

fn clear_buffered_compat_tool_calls(builder: &mut ResponseBuilder) {
    let bytes = builder
        .qwen_xml_buffered_calls
        .iter()
        .map(|(name, arguments)| name.len().saturating_add(arguments.len()))
        .sum();
    builder.qwen_xml_buffered_calls.clear();
    builder.release_buffered_content(bytes);
}

fn emit_compat_tool_call(
    events: &mut Vec<StreamEvent>,
    builder: &mut ResponseBuilder,
    name: String,
    arguments_json: String,
) -> Result<(), AiError> {
    close_open_text_parts(events, builder)?;
    builder.set_stop_reason(StopReason::ToolUse);
    let call_number = builder.qwen_xml_call_count;
    builder.qwen_xml_call_count = builder.qwen_xml_call_count.saturating_add(1);
    let index = get_canonical_index(builder, &format!("qwen_xml_tool_{call_number}"));
    emit_event(
        events,
        builder,
        StreamEvent::ToolCallStart {
            async_execution: false,
            index,
            id: ToolCallId(format!("qwen_xml_call_{}", call_number + 1)),
            name,
        },
    )?;
    emit_event(
        events,
        builder,
        StreamEvent::ToolCallArgsDelta {
            index,
            delta: arguments_json,
        },
    )?;
    emit_event(
        events,
        builder,
        StreamEvent::ToolCallEnd {
            index,
            argument_error: None,
        },
    )
}

pub(super) fn emit_text_without_locked_marker(
    events: &mut Vec<StreamEvent>,
    builder: &mut ResponseBuilder,
    delta: &str,
) -> Result<(), AiError> {
    builder.reserve_buffered_content(delta.len())?;
    builder.qwen_xml_pending.push_str(delta);
    // Consume prefixes logically and compact the String once. Repeatedly
    // draining its front shifts every remaining dense marker and turns a
    // single provider frame into quadratic work.
    let mut pending_head = 0usize;
    loop {
        let pending = &builder.qwen_xml_pending[pending_head..];
        let index = pending.find(TOOL_OUTPUT_LOCKED);
        #[cfg(test)]
        PENDING_WORK.with(|work| {
            let mut count = work.get();
            count.scanned_bytes +=
                index.map_or(pending.len(), |index| index + TOOL_OUTPUT_LOCKED.len());
            work.set(count);
        });
        let Some(index) = index else {
            break;
        };
        if index > 0 {
            let text = take_pending_prefix_at(builder, &mut pending_head, index);
            if let Err(error) = emit_text_delta(events, builder, &text) {
                compact_pending_prefix(builder, pending_head);
                return Err(error);
            }
        }
        skip_pending_prefix_at(builder, &mut pending_head, TOOL_OUTPUT_LOCKED.len());
        builder.tool_output_locked_seen = true;
    }
    let pending = &builder.qwen_xml_pending[pending_head..];
    let keep = marker_suffix_len(pending, TOOL_OUTPUT_LOCKED);
    let flush = pending.len().saturating_sub(keep);
    if flush > 0 {
        let text = take_pending_prefix_at(builder, &mut pending_head, flush);
        if let Err(error) = emit_text_delta(events, builder, &text) {
            compact_pending_prefix(builder, pending_head);
            return Err(error);
        }
    }
    compact_pending_prefix(builder, pending_head);
    Ok(())
}

fn close_open_text_parts(
    events: &mut Vec<StreamEvent>,
    builder: &mut ResponseBuilder,
) -> Result<(), AiError> {
    let open: Vec<usize> = builder
        .text_buffers
        .keys()
        .filter(|index| !builder.ended_indices.contains(index))
        .copied()
        .collect();
    for index in open {
        emit_event(events, builder, StreamEvent::TextEnd { index })?;
    }
    Ok(())
}

pub(super) fn flush_qwen_xml_pending(
    events: &mut Vec<StreamEvent>,
    builder: &mut ResponseBuilder,
) -> Result<(), AiError> {
    let mut pending = std::mem::take(&mut builder.qwen_xml_pending);
    builder.release_buffered_content(pending.len());
    builder.qwen_xml_state = OpenAiChatCompatibilityState::default();
    if pending.contains(TOOL_OUTPUT_LOCKED) {
        pending = pending.replace(TOOL_OUTPUT_LOCKED, "");
        builder.tool_output_locked_seen = true;
    }
    let trimmed = pending.trim();

    // A provider may emit compatibility content first and structured calls in
    // a later chunk. Structured calls are authoritative: discard only content
    // that successfully parses (or structurally identifies) as the duplicate
    // call, while preserving unrelated prose.
    if builder.native_tool_call_seen {
        clear_buffered_compat_tool_calls(builder);
        let duplicate_xml =
            trimmed.contains(TOOL_CALL_OPEN_PREFIX) || trimmed.contains(FUNCTION_OPEN_PREFIX);
        let duplicate_json = builder.buffer_ambiguous_compatibility_content
            && json_tool_candidate(trimmed)
            && (parse_compat_tool_calls(trimmed).is_ok() || looks_like_json_tool_envelope(trimmed));
        if !trimmed.is_empty() && !duplicate_xml && !duplicate_json {
            emit_text_delta(events, builder, &pending)?;
        }
        return Ok(());
    }

    let mut trailing_text = None;
    if !trimmed.is_empty()
        && builder.buffer_ambiguous_compatibility_content
        && json_tool_candidate(trimmed)
    {
        match parse_compat_tool_calls(trimmed) {
            Ok(calls) => buffer_compat_tool_calls(builder, calls)?,
            Err(error) if looks_like_json_tool_envelope(trimmed) => return Err(error),
            Err(_) => trailing_text = Some(pending),
        }
    } else if !trimmed.is_empty()
        && (trimmed.contains(TOOL_CALL_OPEN_PREFIX) || trimmed.contains(FUNCTION_OPEN_PREFIX))
    {
        if (trimmed.contains(TOOL_CALL_OPEN_PREFIX) && !trimmed.contains(QWEN_XML_CLOSE))
            || (trimmed.contains(FUNCTION_OPEN_PREFIX) && !trimmed.contains("</function>"))
        {
            return Err(invalid_tool_field("incomplete XML tool call"));
        }
        let calls = parse_compat_tool_calls(strip_compat_outer(trimmed))?;
        buffer_compat_tool_calls(builder, calls)?;
    } else if !trimmed.is_empty() {
        trailing_text = Some(pending);
    }

    for (name, arguments) in std::mem::take(&mut builder.qwen_xml_buffered_calls) {
        builder.release_buffered_content(name.len().saturating_add(arguments.len()));
        emit_compat_tool_call(events, builder, name, arguments)?;
    }
    if let Some(text) = trailing_text {
        emit_text_delta(events, builder, &text)?;
    }
    Ok(())
}

fn marker_suffix_len(input: &str, marker: &str) -> usize {
    (1..=input.len().min(marker.len().saturating_sub(1)))
        .rev()
        .find(|&len| input.ends_with(&marker[..len]))
        .unwrap_or(0)
}

fn compatibility_marker_suffix_len(input: &str, include_json_fence: bool) -> usize {
    [
        TOOL_CALL_OPEN_PREFIX,
        FUNCTION_OPEN_PREFIX,
        TOOL_OUTPUT_LOCKED,
    ]
    .into_iter()
    .chain(include_json_fence.then_some("```json"))
    .chain(include_json_fence.then_some("```JSON"))
    .map(|marker| marker_suffix_len(input, marker))
    .max()
    .unwrap_or_default()
}

pub(super) fn content_may_contain_tool_call(text: &str) -> bool {
    text.contains(TOOL_CALL_OPEN_PREFIX)
        || text.contains(FUNCTION_OPEN_PREFIX)
        || text.contains(TOOL_OUTPUT_LOCKED)
        || json_tool_candidate(text)
}

fn json_tool_candidate(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with('{')
        || trimmed.starts_with('[')
        || trimmed.starts_with("```json")
        || trimmed.starts_with("```JSON")
}

fn looks_like_json_tool_envelope(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    (lower.contains("\"arguments\"")
        || lower.contains("'arguments'")
        || lower.contains("arguments:"))
        && (lower.contains("\"name\"")
            || lower.contains("'name'")
            || lower.contains("name:")
            || lower.contains("\"tool\"")
            || lower.contains("'tool'")
            || lower.contains("tool:")
            || lower.contains("\"function\"")
            || lower.contains("'function'")
            || lower.contains("function:"))
}

fn strip_compat_outer(input: &str) -> &str {
    let trimmed = input.trim();
    if trimmed.starts_with(TOOL_CALL_OPEN_PREFIX) {
        if let Some(open_end) = trimmed.find('>') {
            let body = &trimmed[open_end + 1..];
            return body
                .strip_suffix(QWEN_XML_CLOSE)
                .map(str::trim)
                .unwrap_or(body);
        }
    }
    trimmed
}

pub(super) fn parse_compat_tool_calls(block: &str) -> Result<Vec<(String, String)>, AiError> {
    let trimmed = strip_compat_outer(block);
    if json_tool_candidate(trimmed) {
        let value = crate::json_repair::parse_json_value(trimmed).map_err(AiError::Decode)?;
        return calls_from_json_value(&value);
    }

    if let Some(function) = trimmed.find(FUNCTION_OPEN_PREFIX) {
        if !trimmed[..function].trim().is_empty() {
            return Err(invalid_tool_field(
                "unexpected content before XML function call",
            ));
        }
        return parse_xml_function_call(&trimmed[function..]).map(|call| vec![call]);
    }

    if let (Some(name), Some(arguments)) = (
        xml_element(trimmed, "name"),
        xml_element(trimmed, "arguments"),
    ) {
        let arguments = crate::json_repair::normalize_json_object(&xml_unescape(arguments.trim()))
            .map_err(AiError::Decode)?;
        return Ok(vec![(xml_unescape(name.trim()), arguments)]);
    }

    Err(invalid_tool_field(
        "content looked like a tool call but no supported XML/JSON envelope was found",
    ))
}

fn calls_from_json_value(value: &serde_json::Value) -> Result<Vec<(String, String)>, AiError> {
    if let Some(calls) = value.as_array() {
        let mut parsed = Vec::with_capacity(calls.len());
        for call in calls {
            parsed.extend(calls_from_json_value(call)?);
        }
        if parsed.is_empty() {
            return Err(invalid_tool_field("JSON tool-call array was empty"));
        }
        return Ok(parsed);
    }

    let object = value
        .as_object()
        .ok_or_else(|| invalid_tool_field("JSON tool call must be an object or array"))?;
    if let Some(calls) = object.get("tool_calls").or_else(|| object.get("calls")) {
        return calls_from_json_value(calls);
    }

    let nested = object
        .get("function")
        .and_then(serde_json::Value::as_object);
    let name = nested
        .and_then(|function| function.get("name"))
        .or_else(|| object.get("name"))
        .or_else(|| object.get("tool"))
        .or_else(|| object.get("function_name"))
        .or_else(|| object.get("function").filter(|value| value.is_string()))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| invalid_tool_field("JSON tool call is missing a function name"))?;
    if name.chars().any(char::is_whitespace) {
        return Err(invalid_tool_field(
            "JSON tool-call name contains whitespace",
        ));
    }

    let arguments = nested
        .and_then(|function| {
            function
                .get("arguments")
                .or_else(|| function.get("args"))
                .or_else(|| function.get("parameters"))
                .or_else(|| function.get("input"))
        })
        .or_else(|| object.get("arguments"))
        .or_else(|| object.get("args"))
        .or_else(|| object.get("parameters"))
        .or_else(|| object.get("input"));
    let arguments = match arguments {
        None | Some(serde_json::Value::Null) => "{}".to_string(),
        Some(serde_json::Value::String(arguments)) => {
            crate::json_repair::normalize_json_object(arguments).map_err(AiError::Decode)?
        }
        Some(arguments) if arguments.is_object() => serde_json::to_string(arguments)
            .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))?,
        Some(_) => return Err(invalid_tool_field("tool arguments must be a JSON object")),
    };
    Ok(vec![(name.to_string(), arguments)])
}

fn parse_xml_function_call(input: &str) -> Result<(String, String), AiError> {
    let open_end = input
        .find('>')
        .ok_or_else(|| invalid_tool_field("XML function tag is incomplete"))?;
    let name = tag_name(&input[..=open_end], "function")?;
    let body_start = open_end + 1;
    let body_end = input[body_start..]
        .find("</function>")
        .map(|relative| body_start + relative)
        .ok_or_else(|| invalid_tool_field("XML function tag is incomplete"))?;
    let trailing = input
        .get(body_end + "</function>".len()..)
        .unwrap_or_default()
        .trim();
    if !trailing.is_empty() && trailing != QWEN_XML_CLOSE {
        return Err(invalid_tool_field(
            "unexpected content after XML function call",
        ));
    }
    let body = &input[body_start..body_end];

    if !body.contains("<parameter") {
        let trimmed = body.trim();
        if trimmed.is_empty() {
            return Ok((name, "{}".to_string()));
        }
        if json_tool_candidate(trimmed) {
            let arguments =
                crate::json_repair::normalize_json_object(trimmed).map_err(AiError::Decode)?;
            return Ok((name, arguments));
        }
        return Err(invalid_tool_field(
            "XML function contains text outside a parameter",
        ));
    }

    let mut arguments = serde_json::Map::new();
    let mut cursor = 0usize;
    while cursor < body.len() {
        let rest = &body[cursor..];
        let Some(relative_open) = rest.find("<parameter") else {
            if !rest.trim().is_empty() {
                return Err(invalid_tool_field(
                    "XML function contains text outside a parameter",
                ));
            }
            break;
        };
        if !rest[..relative_open].trim().is_empty() {
            return Err(invalid_tool_field(
                "XML function contains text outside a parameter",
            ));
        }
        let open = cursor + relative_open;
        let open_end = body[open..]
            .find('>')
            .map(|relative| open + relative)
            .ok_or_else(|| invalid_tool_field("XML parameter tag is incomplete"))?;
        let key = tag_name(&body[open..=open_end], "parameter")?;
        let value_start = open_end + 1;
        let explicit_close = body[value_start..]
            .find("</parameter>")
            .map(|relative| value_start + relative);
        let next_parameter = body[value_start..]
            .find("<parameter")
            .map(|relative| value_start + relative);
        let value_end = explicit_close.or(next_parameter).unwrap_or(body.len());
        let raw = xml_unescape(body[value_start..value_end].trim());
        arguments.insert(key, qwen_xml_value(&raw));
        cursor = explicit_close
            .map(|close| close + "</parameter>".len())
            .unwrap_or(value_end);
    }

    serde_json::to_string(&serde_json::Value::Object(arguments))
        .map(|arguments| (name, arguments))
        .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))
}

fn tag_name(tag: &str, kind: &str) -> Result<String, AiError> {
    let inner = tag
        .strip_prefix('<')
        .and_then(|tag| tag.strip_suffix('>'))
        .ok_or_else(|| invalid_tool_field("malformed XML tool tag"))?;
    let remainder = inner
        .strip_prefix(kind)
        .ok_or_else(|| invalid_tool_field("unexpected XML tool tag"))?
        .trim();
    let raw = if let Some(value) = remainder.strip_prefix('=') {
        value.trim()
    } else if let Some(value) = remainder.strip_prefix("name=") {
        value.trim()
    } else if let Some(value) = remainder.strip_prefix("name =") {
        value.trim()
    } else {
        remainder
    };
    let name = raw.trim_matches(['\'', '"']).trim();
    if name.is_empty()
        || name.chars().any(char::is_whitespace)
        || !name
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '_' | '-' | '.'))
    {
        return Err(invalid_tool_field("XML tool tag has an invalid name"));
    }
    Ok(name.to_string())
}

fn xml_element<'a>(input: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = input.find(&open)? + open.len();
    let end = input[start..].find(&close)? + start;
    Some(&input[start..end])
}

fn invalid_tool_field(message: impl Into<String>) -> AiError {
    AiError::Decode(DecodeError::InvalidProviderField(message.into()))
}

fn qwen_xml_value(raw: &str) -> serde_json::Value {
    if raw.is_empty() {
        return serde_json::Value::String(String::new());
    }
    if let Ok(value) = crate::json_repair::parse_json_value(raw) {
        return value;
    }
    if raw.starts_with('\'') && raw.ends_with('\'') && raw.len() >= 2 {
        return serde_json::Value::String(raw[1..raw.len() - 1].to_string());
    }
    serde_json::Value::String(raw.to_string())
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}
