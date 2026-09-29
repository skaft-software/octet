//! The always-streamed decode half: the SSE frame tree, the terminal
//! reconciliation that runs once authoritative output arrives, the
//! computer-action validator, and `decode_stream_event`.
//!
//! This is separate because OpenAI Responses has no non-streaming decode path
//! (design §12.2). The stream owns every piece of canonical state the codec
//! builds incrementally, so a caller that only sends requests can be exercised
//! without any of it, and the fixture matrix can replay frames without any of
//! the request builder.

use serde::Deserialize;

use crate::error::{AiError, DecodeError, ProviderError};
use crate::protocol::sse::SseEvent;
use crate::protocol::{emit_event, get_canonical_index};
use crate::stream::{ResponseBuilder, StreamEvent};
use crate::types::{Protocol, ReasoningState, ReasoningStateKind, StopReason, ToolCallId, Usage};

use super::wire::{COMPUTER_ACTION_TYPES, COMPUTER_TOOL_NAME, MAX_COMPUTER_ACTION_BYTES};

// --- Terminal async markers ---

pub(super) fn validate_terminal_async_markers(
    builder: &ResponseBuilder,
    output: &[crate::ResponsesItem],
) -> Result<(), AiError> {
    crate::responses::validate_provider_output_items(output)?;
    for item in output {
        let item = item.as_json();
        let Some(marker) = item.get("async") else {
            continue;
        };
        let marker = marker
            .as_bool()
            .ok_or_else(|| DecodeError::InvalidProviderField("invalid async call marker".into()))?;
        let call_id = item.get("call_id").and_then(serde_json::Value::as_str);
        let call = builder
            .tool_call_builders
            .values()
            .find(|call| Some(call.id.0.as_str()) == call_id);
        if call.is_some_and(|call| call.async_execution != marker) || (marker && call.is_none()) {
            return Err(DecodeError::InvalidProviderField(
                "terminal async call marker disagrees with call start".into(),
            )
            .into());
        }
    }
    Ok(())
}

// --- SSE Frame / Terminal Response DTOs ---

#[derive(Deserialize)]
#[serde(tag = "type")]
pub(super) enum ResponsesSseEvent {
    #[serde(rename = "response.created")]
    ResponseCreated { response: ResponsesResponseIdBlock },
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded {
        output_index: usize,
        item: ResponsesResponseItem,
    },
    #[serde(rename = "response.content_part.added")]
    ContentPartAdded {
        output_index: usize,
        content_index: usize,
        part: ResponsesContentPartAdded,
    },
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta {
        output_index: usize,
        #[serde(default)]
        content_index: usize,
        delta: String,
    },
    #[serde(rename = "response.output_text.done")]
    OutputTextDone {
        output_index: usize,
        #[serde(default)]
        content_index: usize,
    },
    #[serde(rename = "response.reasoning_text.delta")]
    ReasoningTextDelta { output_index: usize, delta: String },
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ReasoningSummaryDelta { output_index: usize, delta: String },
    #[serde(rename = "response.custom_tool_call_input.delta")]
    CustomToolInputDelta { output_index: usize, delta: String },
    #[serde(rename = "response.custom_tool_call_input.done")]
    CustomToolInputDone { output_index: usize, input: String },
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionCallArgumentsDelta { output_index: usize, delta: String },
    #[serde(rename = "response.function_call_arguments.done")]
    FunctionCallArgumentsDone {
        output_index: usize,
        /// The Responses API includes the complete JSON argument string on
        /// the terminal `*.done` event. Some Codex gateways omit the
        /// intermediate `*.delta` events (or deliver only this event), so it
        /// must be retained instead of ending an empty tool call.
        #[serde(default)]
        arguments: Option<String>,
    },
    #[serde(rename = "response.output_item.done")]
    OutputItemDone {
        output_index: usize,
        item: ResponsesResponseItemDone,
    },
    #[serde(rename = "response.completed")]
    ResponseCompleted {
        response: ResponsesResponseCompletedBlock,
    },
    #[serde(rename = "response.incomplete")]
    ResponseIncomplete {
        response: ResponsesResponseIncompleteBlock,
    },
    #[serde(rename = "response.failed")]
    ResponseFailed {
        response: ResponsesResponseFailedBlock,
    },
    // Top-level stream error event (apidocs openai-responses
    // 07-streaming-events.md §error: `{type:"error", code, message, param,
    // sequence_number}`). Distinct from `response.failed`, which nests the error
    // under `response.error`. Without this branch `#[serde(other)]` would swallow
    // it and the stream would surface `PrematureEof` instead of the real cause.
    #[serde(rename = "error")]
    StreamError {
        #[serde(default)]
        code: Option<String>,
        #[serde(default)]
        message: Option<String>,
        /// Codex's Responses gateway nests the documented error fields under
        /// `error`, while the public OpenAI API emits them at the top level.
        #[serde(default)]
        error: Option<ResponsesErrorDto>,
    },
    // Out-of-scope event families
    #[serde(other)]
    IgnoredEvent,
}

#[derive(Deserialize)]
pub(super) struct ResponsesResponseIdBlock {
    id: String,
}

#[derive(Deserialize)]
pub(super) struct ResponsesContentPartAdded {
    r#type: String,
}

#[derive(Deserialize)]
pub(super) struct ResponsesResponseItem {
    #[serde(default, rename = "async")]
    async_execution: bool,
    id: String,
    r#type: String,
    #[serde(default)]
    name: Option<String>,
    // A function_call item carries a `call_id` that pairs with its
    // `function_call_output` (design §12.2); prefer it over the item `id`.
    #[serde(default)]
    call_id: Option<String>,
    /// Some codex/Responses endpoints send the full arguments inline in the
    /// `output_item.added` event rather than (or in addition to) separate
    /// `function_call_arguments.delta` events. Capture them here so they are
    /// not silently dropped by serde (unknown-field ignore).
    #[serde(default)]
    arguments: Option<String>,
    #[serde(default)]
    input: Option<String>,
    /// Provider computer-use action (`computer_call` items only). Retained as
    /// raw JSON so the codec can validate the action discriminator and bound
    /// the canonical payload before surfacing it.
    #[serde(default)]
    action: Option<serde_json::Value>,
    /// Provider-reported pending safety checks for a computer call.
    #[serde(default)]
    pending_safety_checks: Option<serde_json::Value>,
}

#[derive(Deserialize)]
pub(super) struct ResponsesResponseItemDone {
    #[serde(default, rename = "async")]
    async_execution: Option<bool>,
    id: String,
    r#type: String,
    #[serde(default)]
    encrypted_content: Option<String>,
    /// A few Responses-compatible gateways put the final function-call
    /// arguments only on `response.output_item.done`. Preserve this fallback
    /// shape as well as the documented `function_call_arguments.done` form.
    #[serde(default)]
    arguments: Option<String>,
    #[serde(default)]
    input: Option<String>,
    /// Terminal computer-use action; see [`ResponsesResponseItem::action`].
    #[serde(default)]
    action: Option<serde_json::Value>,
    /// Terminal pending safety checks for a computer call.
    #[serde(default)]
    pending_safety_checks: Option<serde_json::Value>,
}

#[derive(Deserialize)]
pub(super) struct ResponsesResponseCompletedBlock {
    #[serde(default)]
    service_tier: Option<String>,
    /// Full terminal output is the only authoritative raw replay source. Added
    /// events are intentionally not used because some servers send skeletons.
    #[serde(default)]
    output: Option<Vec<crate::responses::ResponsesItem>>,
    // `usage` is nullable in the Responses object (apidocs
    // openai-responses/01-responses.md: `usage: null` on non-terminal snapshots,
    // populated on completion). Model it as optional so a documented terminal
    // event without usage still decodes to a default-usage `Finished`.
    #[serde(default)]
    usage: Option<ResponsesUsageDto>,
}

#[derive(Deserialize)]
pub(super) struct ResponsesResponseIncompleteBlock {
    #[serde(default)]
    service_tier: Option<String>,
    /// Incomplete terminal responses carry the authoritative output produced
    /// before the limit/refusal stopped generation. Preserve it for exact
    /// Responses replay just as we do for completed responses.
    #[serde(default)]
    output: Option<Vec<crate::responses::ResponsesItem>>,
    // The documented field is `incomplete_details` (object with `reason`), not
    // `status_details` (apidocs openai-responses/01-responses.md:6013,15394).
    incomplete_details: ResponsesIncompleteDetailsDto,
    #[serde(default)]
    usage: Option<ResponsesUsageDto>,
}

#[derive(Deserialize)]
pub(super) struct ResponsesIncompleteDetailsDto {
    reason: String,
}

#[derive(Deserialize)]
pub(super) struct ResponsesResponseFailedBlock {
    error: Option<ResponsesFailedErrorDto>,
}

// Native failed terminals permit absent/null error messages. Keep typed policy
// denials even without prose, without relaxing arbitrary top-level error DTOs.
#[derive(Default, Deserialize)]
pub(super) struct ResponsesFailedErrorDto {
    code: Option<String>,
    message: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct ResponsesErrorDto {
    /// OpenAI-compatible gateways emit JSON `null` when no stable error code is
    /// available. Preserve the provider message instead of turning that valid
    /// error envelope into a decoder failure.
    #[serde(default)]
    code: Option<String>,
    /// The human-readable provider error remains required. Missing or nullable
    /// messages are malformed and must not be silently replaced.
    message: String,
    #[serde(default, rename = "type")]
    kind: Option<String>,
}

// OpenAI Responses usage uses `input_tokens`/`output_tokens` (NOT the Chat
// `prompt_tokens`/`completion_tokens`), with cache + reasoning detail objects
// (design §15; docs/research/apidocs/openai-responses/02-create.md).
#[derive(Deserialize)]
pub(super) struct ResponsesUsageDto {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    input_tokens_details: Option<ResponsesInputTokensDetails>,
    #[serde(default)]
    output_tokens_details: Option<ResponsesOutputTokensDetails>,
}

#[derive(Deserialize)]
pub(super) struct ResponsesInputTokensDetails {
    #[serde(default)]
    cached_tokens: u64,
    #[serde(default)]
    cache_write_tokens: u64,
}

#[derive(Deserialize)]
pub(super) struct ResponsesOutputTokensDetails {
    #[serde(default)]
    reasoning_tokens: u64,
}

// --- Decode Implementations ---
//
// OpenAI Responses is always streamed (design §12.2); there is no non-streaming
// decode path, so this codec deliberately exposes none.

/// Backfill opaque encrypted reasoning from the authoritative terminal output.
///
/// Some Responses-compatible gateways (Azure OpenAI, xAI) omit
/// `reasoning.encrypted_content` from `response.output_item.done` and provide it
/// only in `response.completed.response.output`. Without this, `store:false`
/// multi-turn replay would drop the reasoning continuation for those turns.
/// Only an existing opaque reasoning state is enriched; a missing item is left
/// alone rather than inventing one.
pub(super) fn backfill_reasoning_signatures(
    builder: &mut ResponseBuilder,
    output: &[crate::responses::ResponsesItem],
) -> Result<(), AiError> {
    for item in output {
        let json = item.as_json();
        if json.get("type").and_then(serde_json::Value::as_str) != Some("reasoning") {
            continue;
        }
        let Some(encrypted) = json
            .get("encrypted_content")
            .and_then(serde_json::Value::as_str)
            .filter(|content| !content.is_empty())
        else {
            continue;
        };
        let item_id = json.get("id").and_then(serde_json::Value::as_str);
        let target = builder
            .reasoning_states
            .iter()
            .find_map(|(index, state)| match &state.kind {
                ReasoningStateKind::OpenAiReasoning {
                    item_id: stored_id,
                    encrypted_content,
                } if encrypted_content.is_none() && stored_id.as_deref() == item_id => Some(*index),
                _ => None,
            });
        let Some(index) = target else { continue };
        let mut state = builder.reasoning_states[&index].clone();
        if let ReasoningStateKind::OpenAiReasoning {
            encrypted_content, ..
        } = &mut state.kind
        {
            *encrypted_content = Some(encrypted.to_owned());
        }
        builder.set_reasoning_state(index, state)?;
    }
    Ok(())
}

/// Close any tool-call parts that a provider left open before its terminal
/// response event. Some Responses-compatible gateways send complete arguments
/// in `output_item.added` and omit `function_call_arguments.done`; closing here
/// keeps the canonical stream balanced without prematurely rejecting a later
/// argument delta.
pub(super) fn close_open_tool_calls(
    events: &mut Vec<StreamEvent>,
    builder: &mut ResponseBuilder,
) -> Result<(), AiError> {
    let open: Vec<usize> = builder
        .tool_call_builders
        .keys()
        .copied()
        .filter(|index| !builder.ended_indices.contains(index))
        .collect();
    for index in open {
        if crate::protocol::grammar::is_open(builder, index) {
            crate::protocol::grammar::finish(events, builder, index, None)?;
            continue;
        }
        // A computer call whose action never validated is not a representable
        // exchange: fail closed before the terminal response instead of
        // surfacing an actionless call for a caller to guess at.
        if builder.tool_call_builders.get(&index).is_some_and(|call| {
            call.name == COMPUTER_TOOL_NAME && call.arguments_json.trim().is_empty()
        }) {
            return Err(computer_action_error("missing"));
        }
        emit_event(
            events,
            builder,
            StreamEvent::ToolCallEnd {
                index,
                argument_error: None,
            },
        )?;
    }
    Ok(())
}

/// Terminal opaque custom input must agree with the call exposed to the host.
/// A late monotonic suffix can complete an open call; changed closed input is
/// rejected rather than leaving canonical execution and opaque replay divergent.
pub(super) fn reconcile_custom_output(
    events: &mut Vec<StreamEvent>,
    builder: &mut ResponseBuilder,
    output: &[crate::responses::ResponsesItem],
) -> Result<(), AiError> {
    for item in output {
        let item = item.as_json();
        if item.get("type").and_then(serde_json::Value::as_str) != Some("custom_tool_call") {
            continue;
        }
        let invalid = || {
            DecodeError::InvalidProviderField(
                "terminal custom tool call disagrees with its streamed envelope".to_owned(),
            )
        };
        let id = item
            .get("call_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(invalid)?;
        let name = item
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(invalid)?;
        let input = item
            .get("input")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(invalid)?;
        let index = builder
            .tool_call_builders
            .iter()
            .find(|(_, call)| call.id.0 == id && call.name == name)
            .map(|(index, _)| *index)
            .ok_or_else(invalid)?;
        crate::protocol::grammar::finish(events, builder, index, Some(input))?;
    }
    Ok(())
}

/// Settle tier-aware pricing only after authoritative terminal usage/tier.
/// Missing usage or an undeclared tariff is unpriced, never fabricated as zero.
pub(super) fn settle_responses_cost(
    model: &crate::catalog::Model,
    builder: &mut ResponseBuilder,
    echoed: Option<&str>,
) -> Result<(), AiError> {
    let cost = match (&builder.pricing, &builder.usage) {
        (Some(pricing), Some(usage)) => crate::pricing::responses_cost_of(
            pricing,
            usage,
            model.endpoint.runtime.responses_profile,
            &model.spec.api_name,
            builder.requested_service_tier,
            echoed,
        )?,
        _ => None,
    };
    builder.response_cost = Some(cost);
    if cost.is_none() && builder.pricing.is_some() {
        builder.add_diagnostic(crate::Diagnostic {
            code: "unpriced_responses_tier".to_owned(),
            message:
                "Responses cost is unknown: missing usage or an unqualified service-tier tariff"
                    .to_owned(),
        });
    }
    Ok(())
}

/// Validates and bounds a provider computer action into the canonical argument
/// payload (`{"action": …, "pending_safety_checks": …}`).
///
/// The codec fails closed on a missing or unknown action: computer-use
/// authority lives outside this crate, so an unrecognized action must never be
/// handed to a caller as if it were a known, bounded instruction.
pub(super) fn computer_call_arguments(
    action: Option<&serde_json::Value>,
    pending_safety_checks: Option<&serde_json::Value>,
) -> Result<String, AiError> {
    let kind = action
        .and_then(|action| action.get("type"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("missing");
    if !COMPUTER_ACTION_TYPES.contains(&kind) {
        return Err(computer_action_error(kind));
    }
    let action = action.expect("validated action is present");
    if pending_safety_checks.is_some_and(|checks| !checks.is_array()) {
        return Err(AiError::Decode(DecodeError::Json(
            "OpenAI Responses computer safety checks must be an array".to_owned(),
        )));
    }
    let mut payload = serde_json::Map::with_capacity(2);
    payload.insert("action".to_owned(), action.clone());
    if let Some(checks) =
        pending_safety_checks.filter(|checks| checks.as_array().is_some_and(|c| !c.is_empty()))
    {
        payload.insert("pending_safety_checks".to_owned(), checks.clone());
    }
    let arguments = serde_json::to_string(&serde_json::Value::Object(payload))
        .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))?;
    if arguments.len() > MAX_COMPUTER_ACTION_BYTES {
        return Err(AiError::Decode(DecodeError::Json(format!(
            "OpenAI Responses computer action is {} bytes, over the {} byte bound",
            arguments.len(),
            MAX_COMPUTER_ACTION_BYTES
        ))));
    }
    Ok(arguments)
}

pub(super) fn computer_action_error(kind: &str) -> AiError {
    AiError::Decode(DecodeError::Json(format!(
        "unsupported OpenAI Responses computer action `{kind}`"
    )))
}

/// Extracts a replayable action from canonical computer-call arguments.
///
/// The codec emits `{"action": …, "pending_safety_checks": …}`, but a caller
/// may hand back a bare action object. Anything that is not a documented action
/// type yields no action at all, so canonical replay never re-sends an
/// unrecognized instruction as if the provider had produced it.
pub(super) fn canonical_computer_action(arguments_json: &str) -> Option<serde_json::Value> {
    let parsed: serde_json::Value = serde_json::from_str(arguments_json).ok()?;
    let action = parsed.get("action").unwrap_or(&parsed);
    let kind = action.get("type").and_then(serde_json::Value::as_str)?;
    COMPUTER_ACTION_TYPES
        .contains(&kind)
        .then(|| action.clone())
}

/// Decodes a streaming SSE event from OpenAI Responses, emitting StreamEvents.
pub(crate) fn decode_stream_event(
    model: &crate::catalog::Model,
    sse_event: &SseEvent,
    builder: &mut ResponseBuilder,
) -> Result<Vec<StreamEvent>, AiError> {
    builder.observe_provider_stream_event()?;
    let raw_data = sse_event.data.trim();
    if raw_data.is_empty() {
        return Ok(vec![]);
    }

    let event: ResponsesSseEvent = serde_json::from_str(raw_data).map_err(|error| {
        let value = serde_json::from_str::<serde_json::Value>(raw_data).ok();
        let event_type = value
            .as_ref()
            .and_then(|value| value.get("type"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let keys = value
            .as_ref()
            .and_then(serde_json::Value::as_object)
            .map(|object| object.keys().cloned().collect::<Vec<_>>().join(","))
            .unwrap_or_default();
        let error_keys = value
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(serde_json::Value::as_object)
            .map(|object| object.keys().cloned().collect::<Vec<_>>().join(","))
            .unwrap_or_default();
        AiError::Decode(DecodeError::Json(format!(
            "invalid OpenAI Responses `{event_type}` event ({keys}; error={error_keys}): {error}"
        )))
    })?;

    let mut events = Vec::new();

    match event {
        ResponsesSseEvent::ResponseCreated { response } => {
            builder.response_id = Some(response.id.clone());
            emit_event(
                &mut events,
                builder,
                StreamEvent::Started {
                    response_id: Some(response.id),
                },
            )?;
        }
        ResponsesSseEvent::OutputItemAdded { output_index, item } => {
            crate::responses::validate_provider_output_type(&item.r#type)?;
            if item.async_execution
                && (!matches!(item.r#type.as_str(), "function_call" | "custom_tool_call")
                    || !model.responses_features().async_tools
                    || !builder.tool_definitions.as_ref().is_some_and(|tools| {
                        tools.iter().any(|tool| {
                            tool.async_execution && Some(&tool.name) == item.name.as_ref()
                        })
                    }))
            {
                return Err(DecodeError::InvalidProviderField(
                    "unadvertised async tool call".into(),
                )
                .into());
            }
            if item.r#type == "custom_tool_call" {
                let key = format!("item_{output_index}");
                let index = get_canonical_index(builder, &key);
                let name = item.name.ok_or_else(|| {
                    DecodeError::InvalidProviderField(
                        "custom tool call is missing its name".to_owned(),
                    )
                })?;
                if builder.tool_call_builders.contains_key(&index) {
                    return Err(DecodeError::InvalidProviderField(
                        "custom tool call started more than once".to_owned(),
                    )
                    .into());
                }
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::ToolCallStart {
                        async_execution: item.async_execution,
                        index,
                        id: ToolCallId(item.call_id.unwrap_or(item.id)),
                        name,
                    },
                )?;
                crate::protocol::grammar::start(builder, index)?;
                if let Some(input) = item.input {
                    crate::protocol::grammar::delta(&mut events, builder, index, &input)?;
                }
            } else if item.r#type == "function_call" {
                let key = format!("item_{}", output_index);
                let canonical_idx = get_canonical_index(builder, &key);
                if let Some(name) = item.name {
                    let call_id = item.call_id.unwrap_or(item.id);
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ToolCallStart {
                            async_execution: item.async_execution,
                            index: canonical_idx,
                            id: ToolCallId(call_id),
                            name,
                        },
                    )?;
                    // Some codex/Responses endpoints send the full arguments inline
                    // in the `output_item.added` event rather than via separate
                    // `function_call_arguments.delta` events. Feed them as an
                    // initial delta so the tool call builder is populated even
                    // when no delta events follow.
                    if let Some(ref inline_args) = item.arguments {
                        if !inline_args.trim().is_empty() {
                            emit_event(
                                &mut events,
                                builder,
                                StreamEvent::ToolCallArgsDelta {
                                    index: canonical_idx,
                                    delta: inline_args.clone(),
                                },
                            )?;
                        }
                    }
                }
            } else if item.r#type == "computer_call" {
                let key = format!("item_{}", output_index);
                let canonical_idx = get_canonical_index(builder, &key);
                let call_id = item.call_id.clone().unwrap_or_else(|| item.id.clone());
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::ToolCallStart {
                        async_execution: false,
                        index: canonical_idx,
                        id: ToolCallId(call_id),
                        name: COMPUTER_TOOL_NAME.to_owned(),
                    },
                )?;
                // The action may be deferred to `output_item.done`; when it is
                // present here the terminal check in `close_open_tool_calls`
                // only accepts a payload that already validated.
                if item.action.is_some() || item.pending_safety_checks.is_some() {
                    let arguments = computer_call_arguments(
                        item.action.as_ref(),
                        item.pending_safety_checks.as_ref(),
                    )?;
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ToolCallArgsDelta {
                            index: canonical_idx,
                            delta: arguments,
                        },
                    )?;
                }
            }
        }
        ResponsesSseEvent::ContentPartAdded {
            output_index,
            content_index,
            part,
        } => {
            if part.r#type == "output_text" {
                let key = format!("item_{}_content_{}", output_index, content_index);
                let canonical_idx = get_canonical_index(builder, &key);
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::TextStart {
                        index: canonical_idx,
                    },
                )?;
            }
        }
        ResponsesSseEvent::OutputTextDelta {
            output_index,
            content_index,
            delta,
        } => {
            if !delta.is_empty() {
                let key = format!("item_{}_content_{}", output_index, content_index);
                let canonical_idx = get_canonical_index(builder, &key);
                if !builder.text_buffers.contains_key(&canonical_idx) {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::TextStart {
                            index: canonical_idx,
                        },
                    )?;
                }
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::TextDelta {
                        index: canonical_idx,
                        delta,
                    },
                )?;
            }
        }
        ResponsesSseEvent::OutputTextDone {
            output_index,
            content_index,
        } => {
            let key = format!("item_{}_content_{}", output_index, content_index);
            let canonical_idx = get_canonical_index(builder, &key);
            // Tolerate a duplicated `output_text.done` (§8: one *End per part).
            if builder.text_buffers.contains_key(&canonical_idx)
                && !builder.ended_indices.contains(&canonical_idx)
            {
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::TextEnd {
                        index: canonical_idx,
                    },
                )?;
            }
        }
        ResponsesSseEvent::ReasoningTextDelta {
            output_index,
            delta,
        } => {
            if !delta.is_empty() {
                let key = format!("reasoning_{}", output_index);
                let canonical_idx = get_canonical_index(builder, &key);
                if !builder.reasoning_text_buffers.contains_key(&canonical_idx) {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ReasoningStart {
                            index: canonical_idx,
                        },
                    )?;
                }
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::ReasoningDelta {
                        index: canonical_idx,
                        delta,
                    },
                )?;
            }
        }
        ResponsesSseEvent::ReasoningSummaryDelta {
            output_index,
            delta,
        } => {
            if !delta.is_empty() {
                let key = format!("reasoning_{}", output_index);
                let canonical_idx = get_canonical_index(builder, &key);
                if !builder.reasoning_text_buffers.contains_key(&canonical_idx) {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ReasoningStart {
                            index: canonical_idx,
                        },
                    )?;
                }
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::ReasoningDelta {
                        index: canonical_idx,
                        delta,
                    },
                )?;
            }
        }
        ResponsesSseEvent::CustomToolInputDelta {
            output_index,
            delta,
        } => {
            let index = get_canonical_index(builder, &format!("item_{output_index}"));
            crate::protocol::grammar::delta(&mut events, builder, index, &delta)?;
        }
        ResponsesSseEvent::CustomToolInputDone {
            output_index,
            input,
        } => {
            let index = get_canonical_index(builder, &format!("item_{output_index}"));
            crate::protocol::grammar::finish(&mut events, builder, index, Some(&input))?;
        }
        ResponsesSseEvent::FunctionCallArgumentsDelta {
            output_index,
            delta,
        } => {
            if !delta.is_empty() {
                let key = format!("item_{}", output_index);
                let canonical_idx = get_canonical_index(builder, &key);
                if crate::protocol::grammar::is_open(builder, canonical_idx) {
                    return Err(DecodeError::InvalidProviderField(
                        "custom tool call received function arguments".to_owned(),
                    )
                    .into());
                }
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::ToolCallArgsDelta {
                        index: canonical_idx,
                        delta,
                    },
                )?;
            }
        }
        ResponsesSseEvent::FunctionCallArgumentsDone {
            output_index,
            arguments,
        } => {
            let key = format!("item_{}", output_index);
            let canonical_idx = get_canonical_index(builder, &key);
            if crate::protocol::grammar::is_open(builder, canonical_idx) {
                return Err(DecodeError::InvalidProviderField(
                    "custom tool call received function arguments".to_owned(),
                )
                .into());
            }
            // Providers are allowed to send the complete argument payload
            // only on the terminal event. If no deltas populated the builder,
            // feed that payload before closing the call. If deltas already
            // arrived, ignore the duplicate complete value to avoid appending
            // arguments twice.
            if let Some(arguments) = arguments {
                if !arguments.trim().is_empty()
                    && builder
                        .tool_call_builders
                        .get(&canonical_idx)
                        .is_some_and(|call| call.arguments_json.trim().is_empty())
                {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ToolCallArgsDelta {
                            index: canonical_idx,
                            delta: arguments,
                        },
                    )?;
                }
            }
            // Tolerate a duplicated `function_call_arguments.done` (§8).
            if builder.tool_call_builders.contains_key(&canonical_idx)
                && !builder.ended_indices.contains(&canonical_idx)
            {
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::ToolCallEnd {
                        index: canonical_idx,
                        argument_error: None,
                    },
                )?;
            }
        }
        ResponsesSseEvent::OutputItemDone { output_index, item } => {
            crate::responses::validate_provider_output_type(&item.r#type)?;
            if let Some(marker) = item.async_execution {
                let index = get_canonical_index(builder, &format!("item_{output_index}"));
                if builder
                    .tool_call_builders
                    .get(&index)
                    .is_none_or(|call| call.async_execution != marker)
                {
                    return Err(DecodeError::InvalidProviderField(
                        "async call marker changed after call start".into(),
                    )
                    .into());
                }
            }
            if item.r#type == "custom_tool_call" {
                let index = get_canonical_index(builder, &format!("item_{output_index}"));
                crate::protocol::grammar::finish(
                    &mut events,
                    builder,
                    index,
                    item.input.as_deref(),
                )?;
            } else if item.r#type == "reasoning" {
                let key = format!("reasoning_{}", output_index);
                let canonical_idx = get_canonical_index(builder, &key);
                // A duplicated `output_item.done` must not re-emit End (§8).
                let already_ended = builder.ended_indices.contains(&canonical_idx);
                let had_visible_text = builder.reasoning_text_buffers.contains_key(&canonical_idx);
                if had_visible_text && !already_ended {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ReasoningEnd {
                            index: canonical_idx,
                        },
                    )?;
                } else if item.encrypted_content.is_some() && !already_ended {
                    // Opaque reasoning with no visible delta (design §6.3/§14):
                    // still surface a reasoning part so the opaque `item_id`/
                    // `encrypted_content` is preserved. Without an observed part
                    // (`ReasoningStart`), `ResponseBuilder::finish` — which only
                    // assembles observed indices — would silently drop the state.
                    // The empty text buffer becomes `ReasoningPart.text = None`.
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ReasoningStart {
                            index: canonical_idx,
                        },
                    )?;
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ReasoningEnd {
                            index: canonical_idx,
                        },
                    )?;
                }

                // Persist opaque reasoning state for an observed reasoning part
                // even when `encrypted_content` is absent here: a few gateways
                // (Azure OpenAI, xAI) send it only in the terminal
                // `response.completed` output, where `backfill_reasoning_signatures`
                // merges it into this state for `store:false` replay.
                if item.encrypted_content.is_some() || had_visible_text {
                    builder.set_reasoning_state(
                        canonical_idx,
                        ReasoningState {
                            model: builder.model.clone(),
                            protocol: Protocol::OpenAiResponses,
                            kind: ReasoningStateKind::OpenAiReasoning {
                                item_id: Some(item.id),
                                encrypted_content: item.encrypted_content,
                            },
                        },
                    )?;
                }
            } else if item.r#type == "function_call" {
                // Some Codex-compatible streams omit both argument deltas and
                // `function_call_arguments.done`, putting the complete payload
                // on output_item.done. Feed it before terminal closure.
                let key = format!("item_{}", output_index);
                let canonical_idx = get_canonical_index(builder, &key);
                if let Some(arguments) = item.arguments {
                    if !arguments.trim().is_empty()
                        && builder
                            .tool_call_builders
                            .get(&canonical_idx)
                            .is_some_and(|call| call.arguments_json.trim().is_empty())
                    {
                        emit_event(
                            &mut events,
                            builder,
                            StreamEvent::ToolCallArgsDelta {
                                index: canonical_idx,
                                delta: arguments,
                            },
                        )?;
                    }
                }
                if builder.tool_call_builders.contains_key(&canonical_idx)
                    && !builder.ended_indices.contains(&canonical_idx)
                {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ToolCallEnd {
                            index: canonical_idx,
                            argument_error: None,
                        },
                    )?;
                }
            } else if item.r#type == "computer_call" {
                // A terminal action/check must not silently replace an already
                // published payload. The canonical stream has no replacement
                // event, so refuse a changed instruction before ToolCallEnd
                // rather than losing a late safety check or executing stale data.
                let key = format!("item_{}", output_index);
                let canonical_idx = get_canonical_index(builder, &key);
                if let Some(call) = builder.tool_call_builders.get(&canonical_idx) {
                    if item.action.is_some() || item.pending_safety_checks.is_some() {
                        let prior: Option<serde_json::Value> =
                            serde_json::from_str(&call.arguments_json).ok();
                        let arguments = computer_call_arguments(
                            item.action
                                .as_ref()
                                .or_else(|| prior.as_ref()?.get("action")),
                            item.pending_safety_checks
                                .as_ref()
                                .or_else(|| prior.as_ref()?.get("pending_safety_checks")),
                        )?;
                        if let Some(prior) = prior {
                            let terminal: serde_json::Value = serde_json::from_str(&arguments)
                                .expect("computer_call_arguments produces JSON");
                            if prior != terminal {
                                return Err(AiError::Decode(DecodeError::Json(
                                    "OpenAI Responses terminal computer action or safety checks changed after publication".to_owned(),
                                )));
                            }
                        } else {
                            emit_event(
                                &mut events,
                                builder,
                                StreamEvent::ToolCallArgsDelta {
                                    index: canonical_idx,
                                    delta: arguments,
                                },
                            )?;
                        }
                    }
                }
                if builder.tool_call_builders.contains_key(&canonical_idx)
                    && !builder.ended_indices.contains(&canonical_idx)
                {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ToolCallEnd {
                            index: canonical_idx,
                            argument_error: None,
                        },
                    )?;
                }
            }
        }
        ResponsesSseEvent::ResponseCompleted { response } => {
            // Design §15: a completed response that produced a function call is a
            // tool-use stop; otherwise it is a normal end-of-turn.
            let stop = if builder.tool_call_builders.is_empty() {
                StopReason::EndTurn
            } else {
                StopReason::ToolUse
            };
            builder.set_stop_reason(stop);
            if let Some(output) = response.output.filter(|output| !output.is_empty()) {
                validate_terminal_async_markers(builder, &output)?;
                backfill_reasoning_signatures(builder, &output)?;
                reconcile_custom_output(&mut events, builder, &output)?;
                builder.responses_output = Some(crate::responses::ResponsesOutput::new(output));
            }
            close_open_tool_calls(&mut events, builder)?;
            // Usage is optional on the wire; only emit a `Usage` event when the
            // provider reported one so `Finished.usage` is a default rather than a
            // misleading all-zero count.
            if let Some(usage) = &response.usage {
                let u = map_usage(usage)?;
                emit_event(&mut events, builder, StreamEvent::Usage(u))?;
            }

            settle_responses_cost(model, builder, response.service_tier.as_deref())?;
            let resp = builder.finish_mut()?;
            emit_event(&mut events, builder, StreamEvent::Finished(resp))?;
        }
        ResponsesSseEvent::ResponseIncomplete { response } => {
            let stop = match response.incomplete_details.reason.as_str() {
                "max_output_tokens" => StopReason::MaxTokens,
                "content_filter" => StopReason::Refusal,
                "steered" => StopReason::Steered,
                other => StopReason::Other(other.to_string()),
            };
            builder.set_stop_reason(stop);
            if let Some(output) = response.output.filter(|output| !output.is_empty()) {
                validate_terminal_async_markers(builder, &output)?;
                backfill_reasoning_signatures(builder, &output)?;
                reconcile_custom_output(&mut events, builder, &output)?;
                builder.responses_output = Some(crate::responses::ResponsesOutput::new(output));
            }
            close_open_tool_calls(&mut events, builder)?;

            if let Some(usage) = &response.usage {
                let u = map_usage(usage)?;
                emit_event(&mut events, builder, StreamEvent::Usage(u))?;
            }

            settle_responses_cost(model, builder, response.service_tier.as_deref())?;
            let resp = builder.finish_mut()?;
            emit_event(&mut events, builder, StreamEvent::Finished(resp))?;
        }
        ResponsesSseEvent::ResponseFailed { response } => {
            let error = response.error.unwrap_or_default();
            return Err(AiError::ResponsesFailed(ProviderError {
                code: error.code,
                kind: error.kind,
                message: error
                    .message
                    .unwrap_or_else(|| "response.failed event received".into()),
                request_id: None,
            }));
        }
        ResponsesSseEvent::StreamError {
            code,
            message,
            error,
        } => {
            let nested_code = error.as_ref().and_then(|error| error.code.clone());
            let nested_kind = error.as_ref().and_then(|error| error.kind.clone());
            let nested_message = error.map(|error| error.message);
            return Err(AiError::Provider(ProviderError {
                code: code.or(nested_code),
                kind: nested_kind,
                message: message
                    .or(nested_message)
                    .unwrap_or_else(|| "provider stream error".to_owned()),
                request_id: None,
            }));
        }
        ResponsesSseEvent::IgnoredEvent => {}
    }

    Ok(events)
}

// --- Helpers ---

pub(super) fn map_usage(usage: &ResponsesUsageDto) -> Result<Usage, AiError> {
    // Design §15: OpenAI `input_tokens` INCLUDES cache, so cache read + write are
    // subtracted out to keep the canonical buckets disjoint (full-rate input only).
    let cache_read = usage
        .input_tokens_details
        .as_ref()
        .map(|d| d.cached_tokens)
        .unwrap_or(0);
    let cache_write = usage
        .input_tokens_details
        .as_ref()
        .map(|d| d.cache_write_tokens)
        .unwrap_or(0);
    let reasoning = usage
        .output_tokens_details
        .as_ref()
        .map(|d| d.reasoning_tokens)
        .unwrap_or(0);
    // Some OpenAI-compatible gateways emit detail counters that exceed the
    // nominal aggregate. Preserve disjoint buckets and the completed response
    // by flooring only the residual full-rate input bucket.
    let input = usage
        .input_tokens
        .saturating_sub(cache_read)
        .saturating_sub(cache_write);
    crate::responses::normalize_responses_usage(
        input,
        cache_read,
        cache_write,
        usage.output_tokens,
        reasoning,
    )
    .ok_or(AiError::Decode(DecodeError::UsageUnderflow))
}
