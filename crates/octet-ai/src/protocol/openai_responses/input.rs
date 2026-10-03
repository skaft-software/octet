//! Canonical history to Responses `input` items: the ordered walk that turns
//! messages, authoritative provider output and opaque replay into the wire item
//! sequence, plus the async-marker validation that gates those items.
//!
//! This is separate because replay order is a property of the conversation, not
//! of the request body. The body builder only assembles what this module
//! produced, and a change to ordering rules (design §11) must be provably
//! invisible to everything that only assembles bytes.

use crate::error::{AiError, ConfigError, DecodeError};
use crate::protocol::WireImageUrl;
use crate::types::{
    AssistantPart, ImageSource, Media, Message, Protocol, ReasoningStateKind, Request, ToolCallId,
    ToolDef, ToolResultPart, UserPart,
};

use super::stream::canonical_computer_action;
use super::wire::{
    opaque_input_item, ResponsesComputerScreenshot, ResponsesContentPart, ResponsesInputItem,
    ResponsesReasoningSummary, ResponsesToolResultBlock, COMPUTER_TOOL_NAME,
};

pub(super) fn validate_async_tools(model: &crate::Model, tools: &[ToolDef]) -> Result<(), AiError> {
    if tools.iter().any(|tool| tool.async_execution) && !model.responses_features().async_tools {
        return Err(
            ConfigError::Parse("async tools are not qualified for this route".into()).into(),
        );
    }
    Ok(())
}

pub(super) fn validate_async_input(
    model: &crate::Model,
    input: &crate::ResponsesInput,
    tools: &[ToolDef],
) -> Result<(), AiError> {
    validate_async_tools(model, tools)?;
    // Only the ordered walk can accept these prospective historical pairs.
    // This index does not waive duplicate IDs or output-before-call checks.
    let historical_results: std::collections::HashSet<&str> = input
        .items()
        .iter()
        .filter_map(|item| {
            let item = item.as_json();
            match item.get("type").and_then(serde_json::Value::as_str) {
                Some("function_call_output" | "custom_tool_call_output") => {
                    item.get("call_id").and_then(serde_json::Value::as_str)
                }
                _ => None,
            }
        })
        .collect();
    let mut call_ids = std::collections::HashSet::new();
    let mut invalid_pending = std::collections::HashSet::new();
    let mut completed = std::collections::HashSet::new();
    for item in input.items() {
        let item = item.as_json();
        let kind = item.get("type").and_then(serde_json::Value::as_str);
        // Delta continuation may legitimately carry outputs for calls in the
        // server's retained prefix. Still record every visible result, so later
        // calls cannot reuse that identity or hide an output-before-call pair.
        let id = item.get("call_id").and_then(serde_json::Value::as_str);
        if matches!(
            kind,
            Some("function_call" | "custom_tool_call" | "computer_call")
        ) {
            if let Some(id) = id {
                if !call_ids.insert(id) || completed.contains(id) {
                    return Err(ConfigError::Parse(
                        "duplicate or out-of-order tool call ID in Responses input".into(),
                    )
                    .into());
                }
            }
        } else if matches!(
            kind,
            Some("function_call_output" | "custom_tool_call_output" | "computer_call_output")
        ) {
            if let Some(id) = id {
                if !completed.insert(id) {
                    return Err(ConfigError::Parse(
                        "duplicate tool result in Responses input".into(),
                    )
                    .into());
                }
                invalid_pending.remove(id);
            }
        }
        let Some(marker) = item.get("async") else {
            continue;
        };
        let enabled = marker
            .as_bool()
            .ok_or_else(|| ConfigError::Parse("invalid async call marker".into()))?;
        if !enabled {
            continue;
        }
        let name = item
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if !model.responses_features().async_tools
            || !matches!(kind, Some("function_call" | "custom_tool_call"))
            || name.is_empty()
        {
            return Err(
                ConfigError::Parse("unadvertised async call in Responses input".into()).into(),
            );
        }
        let id = item
            .get("call_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| ConfigError::Parse("async call requires a call_id".into()))?;
        if historical_results.contains(id) {
            // Retain envelope validation, but do not use a later tool schema or
            // advertisement as authority over already completed provider work.
            if kind == Some("custom_tool_call") {
                item.get("input")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        ConfigError::Parse("async custom call requires string input".into())
                    })?;
            } else {
                let arguments = item
                    .get("arguments")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        ConfigError::Parse("async function call requires arguments".into())
                    })?;
                crate::json_repair::normalize_json_object_value(arguments)?;
            }
            continue;
        }
        if !tools
            .iter()
            .any(|tool| tool.async_execution && tool.name == name)
        {
            return Err(ConfigError::Parse(
                "pending async call was not advertised for this tool".into(),
            )
            .into());
        }
        let arguments = if kind == Some("custom_tool_call") {
            let property = crate::protocol::grammar::input_property(
                tools,
                name,
                crate::protocol::grammar_tools_for(model),
            )?
            .ok_or_else(|| {
                ConfigError::Parse("async custom call requires its declared grammar".into())
            })?;
            let text = item
                .get("input")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    ConfigError::Parse("async custom call requires string input".into())
                })?;
            serde_json::json!({property: text})
        } else {
            let arguments = item
                .get("arguments")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    ConfigError::Parse("async function call requires arguments".into())
                })?;
            crate::json_repair::normalize_json_object_value(arguments)?
        };
        if !matches!(
            crate::json_repair::validate_tool_arguments(name, &arguments, tools)?,
            crate::ToolArgumentValidation::Valid
        ) {
            invalid_pending.insert(id);
        }
    }
    if let Some(id) = invalid_pending.into_iter().next() {
        return Err(crate::ValidationError::MissingToolResult(ToolCallId(id.to_owned())).into());
    }
    Ok(())
}

pub(super) fn map_system_input(
    model: &crate::catalog::Model,
    system: Option<&str>,
) -> Vec<ResponsesInputItem> {
    let Some(system) = system else {
        return Vec::new();
    };
    let role = if model.spec.capabilities.reasoning.is_some() {
        "developer"
    } else {
        "system"
    };
    vec![ResponsesInputItem::Message {
        role: role.to_owned(),
        content: vec![ResponsesContentPart::InputText {
            text: system.to_owned(),
        }],
    }]
}

pub(super) fn map_user_input(
    model: &crate::catalog::Model,
    user: &crate::types::UserMessage,
    preserve_tool_call_ids: bool,
    pending_tool_calls: &mut std::collections::BTreeSet<String>,
    synthetic_tool_results: &std::collections::HashSet<String>,
    computer_call_ids: &std::collections::BTreeSet<String>,
) -> Vec<ResponsesInputItem> {
    let mut input = Vec::new();
    let mut content = Vec::new();
    for part in &user.content {
        match part {
            UserPart::Text(text) => {
                content.push(ResponsesContentPart::InputText { text: text.clone() });
            }
            UserPart::Media(Media::Image(image)) => {
                if !model
                    .spec
                    .capabilities
                    .input_modalities
                    .contains(crate::types::Modality::Image)
                {
                    continue;
                }

                let (image_url, file_id) = match &image.source {
                    ImageSource::Url(url) => (Some(WireImageUrl::Url(url.to_string())), None),
                    ImageSource::Inline(bytes) => {
                        // No documented default MIME; do not guess a wire field
                        // (design §75). Validation already diagnosed the drop.
                        let Some(media_type) = image.media_type.as_ref() else {
                            continue;
                        };
                        (
                            Some(WireImageUrl::Inline {
                                media_type: media_type.to_string(),
                                data: bytes.clone(),
                            }),
                            None,
                        )
                    }
                    ImageSource::ProviderRef(reference) => {
                        // An expired or wrong-protocol provider ref is dropped
                        // (validation already emitted the diagnostic).
                        if !crate::validate::provider_ref_is_usable(
                            reference,
                            Protocol::OpenAiResponses,
                        ) {
                            continue;
                        }
                        (None, Some(reference.id.clone()))
                    }
                };

                let detail = image.detail.map(|detail| match detail {
                    crate::types::ImageDetail::Auto => "auto".to_owned(),
                    crate::types::ImageDetail::Low => "low".to_owned(),
                    crate::types::ImageDetail::High => "high".to_owned(),
                });
                content.push(ResponsesContentPart::InputImage {
                    image_url,
                    file_id,
                    detail,
                });
            }
            UserPart::Media(Media::Audio(_)) => {}
            UserPart::ToolResult(result) => {
                if synthetic_tool_results.contains(&result.tool_call_id.0) {
                    continue;
                }
                pending_tool_calls.remove(&result.tool_call_id.0);
                let mut outputs = Vec::new();
                for result_part in &result.content {
                    match result_part {
                        ToolResultPart::Text(text) => {
                            outputs
                                .push(ResponsesToolResultBlock::InputText { text: text.clone() });
                        }
                        ToolResultPart::Media(Media::Image(image)) => match &image.source {
                            ImageSource::Url(url) => {
                                outputs.push(ResponsesToolResultBlock::InputImage {
                                    image_url: Some(WireImageUrl::Url(url.to_string())),
                                    file_id: None,
                                });
                            }
                            ImageSource::Inline(bytes) => {
                                // Do not guess a wire MIME (§75); drop the part
                                // if absent.
                                if let Some(media_type) = image.media_type.as_ref() {
                                    outputs.push(ResponsesToolResultBlock::InputImage {
                                        image_url: Some(WireImageUrl::Inline {
                                            media_type: media_type.to_string(),
                                            data: bytes.clone(),
                                        }),
                                        file_id: None,
                                    });
                                }
                            }
                            ImageSource::ProviderRef(reference) => {
                                if crate::validate::provider_ref_is_usable(
                                    reference,
                                    Protocol::OpenAiResponses,
                                ) {
                                    outputs.push(ResponsesToolResultBlock::InputImage {
                                        image_url: None,
                                        file_id: Some(reference.id.clone()),
                                    });
                                }
                            }
                        },
                        ToolResultPart::Media(Media::Audio(_)) => {}
                    }
                }
                // Preserve canonical order: emit buffered user content before
                // this standalone tool-result item.
                flush_user_content(&mut input, &mut content);
                let call_id = if preserve_tool_call_ids {
                    result.tool_call_id.0.clone()
                } else {
                    crate::protocol::normalize_tool_call_id(&result.tool_call_id.0)
                };
                if computer_call_ids.contains(&result.tool_call_id.0) {
                    // A tool result for a computer call is a
                    // `computer_call_output`, never a `function_call_output`:
                    // the provider pairs it with the earlier `computer_call`
                    // item by `call_id` and rejects the function shape.
                    input.push(ResponsesInputItem::ComputerCallOutput {
                        call_id,
                        output: ResponsesComputerScreenshot::from_blocks(&outputs),
                    });
                } else {
                    input.push(ResponsesInputItem::FunctionCallOutput {
                        call_id,
                        output: outputs,
                    });
                }
            }
        }
    }
    flush_user_content(&mut input, &mut content);
    input
}

pub(super) fn map_assistant_input(
    assistant: &crate::types::AssistantMessage,
    model: &crate::catalog::Model,
    pending_tool_calls: &mut std::collections::BTreeSet<String>,
    computer_call_ids: &mut std::collections::BTreeSet<String>,
) -> Vec<ResponsesInputItem> {
    let mut input = Vec::new();
    // Preserve canonical part order: buffered assistant text is flushed as a
    // `message` item immediately before each function/reasoning item.
    let mut text_parts = Vec::new();
    for part in &assistant.content {
        match part {
            AssistantPart::Text(text) => text_parts.push(text.clone()),
            AssistantPart::ToolCall(tool_call) => {
                flush_assistant_text(&mut input, &mut text_parts);
                if !tool_call.async_execution {
                    pending_tool_calls.insert(tool_call.id.0.clone());
                }
                let call_id = crate::protocol::normalize_tool_call_id(&tool_call.id.0);
                if tool_call.name == COMPUTER_TOOL_NAME {
                    // Canonical history replays a computer call as a
                    // `computer_call` item, not as a function call the route
                    // never declared. The action is carried in the call's
                    // canonical arguments and is re-emitted only when it is a
                    // documented action type.
                    computer_call_ids.insert(tool_call.id.0.clone());
                    input.push(ResponsesInputItem::ComputerCall {
                        call_id,
                        action: canonical_computer_action(&tool_call.arguments_json),
                    });
                } else {
                    input.push(ResponsesInputItem::FunctionCall {
                        async_execution: tool_call.async_execution,
                        call_id,
                        name: tool_call.name.clone(),
                        arguments: tool_call.arguments_json.clone(),
                    });
                }
            }
            AssistantPart::Reasoning(reasoning) => {
                if let Some(state) = &reasoning.state {
                    if state.protocol == Protocol::OpenAiResponses && state.model == model.spec.id {
                        if let ReasoningStateKind::OpenAiReasoning {
                            item_id,
                            encrypted_content,
                        } = &state.kind
                        {
                            flush_assistant_text(&mut input, &mut text_parts);
                            input.push(ResponsesInputItem::Reasoning {
                                id: item_id.clone(),
                                summary: reasoning
                                    .text
                                    .as_ref()
                                    .map(|text| {
                                        vec![ResponsesReasoningSummary {
                                            r#type: "summary_text".to_owned(),
                                            text: text.clone(),
                                        }]
                                    })
                                    .unwrap_or_default(),
                                encrypted_content: encrypted_content.clone(),
                            });
                        }
                    }
                }
            }
            AssistantPart::Media(_) => {}
            AssistantPart::ProviderMetadata(_) => {}
        }
    }
    flush_assistant_text(&mut input, &mut text_parts);
    input
}

pub(crate) fn encode_canonical_input(
    model: &crate::catalog::Model,
    system: Option<&str>,
    messages: &[Message],
    compatibility: crate::CompatibilityMode,
) -> crate::responses::ResponsesInput {
    let mut input = map_system_input(model, system);
    let mut pending_tool_calls = std::collections::BTreeSet::new();
    let mut synthetic_tool_results = std::collections::HashSet::new();
    let mut computer_call_ids = std::collections::BTreeSet::new();
    for message in messages {
        match message {
            Message::User(user) => input.extend(map_user_input(
                model,
                user,
                false,
                &mut pending_tool_calls,
                &synthetic_tool_results,
                &computer_call_ids,
            )),
            Message::Assistant(assistant) => {
                if compatibility == crate::CompatibilityMode::Lossy {
                    push_synthetic_tool_results(
                        &mut input,
                        &mut pending_tool_calls,
                        &mut synthetic_tool_results,
                    );
                }
                input.extend(map_assistant_input(
                    assistant,
                    model,
                    &mut pending_tool_calls,
                    &mut computer_call_ids,
                ));
            }
        }
    }
    if compatibility == crate::CompatibilityMode::Lossy {
        push_synthetic_tool_results(
            &mut input,
            &mut pending_tool_calls,
            &mut synthetic_tool_results,
        );
    }
    crate::responses::ResponsesInput::new(input.into_iter().map(opaque_input_item).collect())
}

pub(crate) fn encode_replay_input(
    model: &crate::catalog::Model,
    system: Option<&str>,
    replay: &[crate::responses::ResponsesReplayItem],
) -> Result<crate::responses::ResponsesInput, AiError> {
    let compacted_base = matches!(
        replay.first(),
        Some(crate::responses::ResponsesReplayItem::Compacted(_))
    );
    let mut input: Vec<crate::responses::ResponsesItem> = if compacted_base {
        Vec::new()
    } else {
        map_system_input(model, system)
            .into_iter()
            .map(opaque_input_item)
            .collect()
    };
    let mut pending_tool_calls = std::collections::BTreeSet::new();
    let synthetic_tool_results = std::collections::HashSet::new();
    let mut computer_call_ids = std::collections::BTreeSet::new();
    for item in replay {
        match item {
            crate::responses::ResponsesReplayItem::ConfigurationUpdate(update) => {
                input.push(update.to_item())
            }
            crate::responses::ResponsesReplayItem::User(user) => {
                input.extend(
                    map_user_input(
                        model,
                        user,
                        true,
                        &mut pending_tool_calls,
                        &synthetic_tool_results,
                        &computer_call_ids,
                    )
                    .into_iter()
                    .map(opaque_input_item),
                );
            }
            crate::responses::ResponsesReplayItem::LocalAssistant(assistant) => {
                input.extend(
                    map_assistant_input(
                        assistant,
                        model,
                        &mut pending_tool_calls,
                        &mut computer_call_ids,
                    )
                    .into_iter()
                    .map(opaque_input_item),
                );
            }
            crate::responses::ResponsesReplayItem::Output(output)
            | crate::responses::ResponsesReplayItem::Compacted(output) => {
                output.validate_provider_output()?;
                // Authoritative provider output carries the only trustworthy
                // computer-call provenance: recognize `computer_call` items
                // verbatim so the caller's tool result for that `call_id` is
                // replayed as `computer_call_output`.
                for item in output.items() {
                    if item
                        .as_json()
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        == Some("computer_call")
                    {
                        if let Some(call_id) = item
                            .as_json()
                            .get("call_id")
                            .and_then(serde_json::Value::as_str)
                        {
                            computer_call_ids.insert(call_id.to_owned());
                        }
                    }
                }
                input.extend(output.items().iter().cloned());
            }
        }
    }
    Ok(crate::responses::ResponsesInput::new(input))
}

/// Convert canonical function-shaped history using the immutable request tool
/// schema, and preserve authoritative custom-call provenance on opaque replay.
/// Results are paired by call id, never inferred from their text payload.
pub(super) fn map_grammar_replay(
    input: &mut serde_json::Value,
    req: &Request,
    canonical_calls: bool,
    grammar_tools: bool,
) -> Result<(), AiError> {
    let mut custom_ids = std::collections::HashSet::new();
    for message in req.messages.iter().filter(|_| canonical_calls) {
        if let Message::Assistant(assistant) = message {
            for part in &assistant.content {
                if let AssistantPart::ToolCall(call) = part {
                    if crate::protocol::grammar::input_property(
                        &req.tools,
                        &call.name,
                        grammar_tools,
                    )?
                    .is_some()
                    {
                        custom_ids.insert(call.id.0.clone());
                        custom_ids.insert(crate::protocol::normalize_tool_call_id(&call.id.0));
                    }
                }
            }
        }
    }
    let items = input.as_array_mut().expect("Responses input is an array");
    for item in items.iter_mut() {
        if canonical_calls
            && item.get("type").and_then(serde_json::Value::as_str) == Some("function_call")
        {
            if let Some(name) = item.get("name").and_then(serde_json::Value::as_str) {
                if let Some(property) =
                    crate::protocol::grammar::input_property(&req.tools, name, grammar_tools)?
                {
                    let arguments = item
                        .get("arguments")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| {
                            DecodeError::InvalidProviderField(
                                "custom replay call has no arguments".to_owned(),
                            )
                        })?;
                    let text = crate::protocol::grammar::replay_input(arguments, &property)?;
                    let object = item.as_object_mut().expect("a function call is an object");
                    object.remove("arguments");
                    object.insert("type".to_owned(), "custom_tool_call".into());
                    object.insert("input".to_owned(), text.into());
                }
            }
        }
        if item.get("type").and_then(serde_json::Value::as_str) == Some("custom_tool_call") {
            if let Some(id) = item.get("call_id").and_then(serde_json::Value::as_str) {
                custom_ids.insert(id.to_owned());
            }
        }
    }
    for item in items {
        if item.get("type").and_then(serde_json::Value::as_str) == Some("function_call_output")
            && item
                .get("call_id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|id| custom_ids.contains(id))
        {
            item["type"] = "custom_tool_call_output".into();
        }
    }
    Ok(())
}

/// Flush buffered user content parts as a `message` item, preserving canonical
/// order relative to interleaved `function_call_output` items (design §11).
pub(super) fn flush_user_content(
    input: &mut Vec<ResponsesInputItem>,
    content: &mut Vec<ResponsesContentPart>,
) {
    if !content.is_empty() {
        input.push(ResponsesInputItem::Message {
            role: "user".to_string(),
            content: std::mem::take(content),
        });
    }
}

/// Flush buffered assistant text as a `message` item, preserving canonical
/// order relative to interleaved `function_call`/`reasoning` items (design §11
/// immutable replay). Consecutive text parts are joined; a `\n` boundary only
/// appears where the canonical parts were themselves adjacent text.
pub(super) fn flush_assistant_text(
    input: &mut Vec<ResponsesInputItem>,
    text_parts: &mut Vec<String>,
) {
    if !text_parts.is_empty() {
        input.push(ResponsesInputItem::Message {
            role: "assistant".to_string(),
            content: vec![ResponsesContentPart::OutputText {
                text: std::mem::take(text_parts).join("\n"),
                annotations: vec![],
            }],
        });
    }
}

pub(super) fn push_synthetic_tool_results(
    input: &mut Vec<ResponsesInputItem>,
    pending: &mut std::collections::BTreeSet<String>,
    synthetic: &mut std::collections::HashSet<String>,
) {
    for call_id in std::mem::take(pending) {
        synthetic.insert(call_id.clone());
        input.push(ResponsesInputItem::FunctionCallOutput {
            // Wire write: normalize to match the paired `function_call` above.
            call_id: crate::protocol::normalize_tool_call_id(&call_id),
            output: vec![ResponsesToolResultBlock::InputText {
                text: "Tool execution result was not supplied by the caller.".to_string(),
            }],
        });
    }
}
