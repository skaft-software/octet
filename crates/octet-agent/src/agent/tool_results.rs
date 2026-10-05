//! Tool result shaping: truncation, repetition annotations, media lowering and cancellation.

use super::*;

pub(super) fn active_branch_entries(session: &Session) -> Vec<&crate::session::Entry> {
    let mut reverse = Vec::new();
    let mut cursor = session.head();
    while let Some(id) = cursor {
        let Some(entry) = session.entry(&id) else {
            break;
        };
        cursor = entry.parent.clone();
        reverse.push(entry);
    }
    reverse.reverse();
    reverse
}

pub(super) fn resolve_tool_delivery_after_persistence(
    result: &Result<ToolOutput, ToolError>,
    text_limit: usize,
) {
    if let Ok(output) = result {
        output.resolve_delivery(output.text.len() <= text_limit);
    }
}

pub(super) fn cancelled_tool_error() -> ToolError {
    ToolError::new(
        "tool execution cancelled by user; state may be partially changed and must not be replayed automatically",
    )
}

pub(super) fn pending_tool_state(
    session: &Session,
) -> Option<(Vec<ToolCall>, HashSet<octet_ai::ToolCallId>)> {
    let mut persisted = HashSet::new();
    let mut calls = Vec::new();
    let mut latest_assistant = true;
    let mut cursor = session.head_ref();
    while let Some(id) = cursor {
        let entry = session.entry(id)?;
        match &entry.value {
            EntryValue::Message(Message::Assistant(assistant)) => {
                calls.extend(assistant.content.iter().filter_map(|part| match part {
                    AssistantPart::ToolCall(call) if latest_assistant || call.async_execution => {
                        Some(call.clone())
                    }
                    _ => None,
                }));
                latest_assistant = false;
            }
            EntryValue::Message(Message::User(user)) => {
                for part in &user.content {
                    if let UserPart::ToolResult(result) = part {
                        persisted.insert(result.tool_call_id.clone());
                    }
                }
            }
            _ => {}
        }
        cursor = entry.parent.as_ref();
    }
    (!calls.is_empty()).then_some((calls, persisted))
}

pub(super) fn tool_call_arguments_fingerprint(name: &str, args: &serde_json::Value) -> String {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(name.as_bytes());
    bytes.push(0);
    if serde_json::to_writer(&mut bytes, args).is_err() {
        bytes.extend_from_slice(b"<invalid-json>");
    }
    content_hash(&bytes)
}

pub(super) fn repeated_tool_annotation(repeated_recently: usize) -> String {
    format!(
        "\n[agent diagnostic: exact call repeated {}x recently; if no progress, change approach or verify state.]",
        repeated_recently.saturating_add(1)
    )
}

pub(super) fn annotate_repeated_tool_result(
    result: Result<ToolOutput, ToolError>,
    repeated_recently: usize,
) -> Result<ToolOutput, ToolError> {
    if repeated_recently < REPEATED_TOOL_CALL_THRESHOLD {
        return result;
    }
    let annotation = repeated_tool_annotation(repeated_recently);
    match result {
        Ok(output) if serde_json::from_str::<serde_json::Value>(&output.text).is_ok() => {
            // Keep machine-readable tool contracts valid. A trailing hint
            // would turn an otherwise valid JSON result into invalid JSON;
            // the next model turn can still be diagnosed from telemetry.
            Ok(output)
        }
        Ok(output) => Ok(output.with_model_annotation(&annotation)),
        Err(error) if serde_json::from_str::<serde_json::Value>(&error.message).is_ok() => {
            Err(error)
        }
        Err(error) => {
            let message = format!("{}{}", error.message, annotation);
            match error.policy_denial_code() {
                Some(code) => Err(ToolError::policy_denied(code, message)),
                None => Err(ToolError::new(message)),
            }
        }
    }
}

pub(super) fn assistant_has_terminal_content(assistant: &AssistantMessage) -> bool {
    assistant.content.iter().any(|part| match part {
        AssistantPart::Text(text) => !text.trim().is_empty(),
        AssistantPart::ToolCall(_) | AssistantPart::Media(_) => true,
        AssistantPart::Reasoning(_) | AssistantPart::ProviderMetadata(_) => false,
    })
}

/// Content-free evidence for a normally ended turn with no terminal content.
/// Only allowlisted diagnostic codes are read, never their messages or IDs.
pub(super) fn incomplete_terminal_response_reason(
    assistant: &AssistantMessage,
    stop_reason: &StopReason,
    usage: &Usage,
    diagnostics: &[octet_ai::Diagnostic],
    request_max_output_tokens: u64,
) -> String {
    let base = if assistant
        .content
        .iter()
        .any(|part| matches!(part, AssistantPart::Reasoning(_)))
    {
        "provider returned reasoning but no answer text"
    } else {
        "provider returned no user-visible content"
    };
    let mut chat_stop_defaulted = false;
    let mut usage_missing = false;
    for diagnostic in diagnostics {
        match diagnostic.code.as_str() {
            "chat_defaulted_stop_reason" => chat_stop_defaulted = true,
            "chat_usage_missing" => usage_missing = true,
            _ => {}
        }
    }
    let stop = match stop_reason {
        StopReason::Other(_) => "other",
        reason => reason.as_canonical(),
    };
    let usage = if usage_missing {
        "usage=not_reported".to_owned()
    } else {
        format!(
            "usage=canonical; output_tokens={}; reasoning_tokens={}",
            usage.output_tokens, usage.reasoning_tokens
        )
    };
    format!(
        "{base} (stop={stop}; chat_stop_defaulted={chat_stop_defaulted}; {usage}; request_max_output_tokens={request_max_output_tokens}; not automatically retried)"
    )
}

pub(super) fn truncate_tool_text(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    if limit == 0 {
        return String::new();
    }
    if limit <= TOOL_TRUNCATION_MARKER.len() {
        return TOOL_TRUNCATION_MARKER[..limit].to_owned();
    }
    let available = limit - TOOL_TRUNCATION_MARKER.len();
    let head = available / 2;
    let tail = available - head;
    let mut result = String::with_capacity(limit);
    let mut head_end = head.min(text.len());
    while head_end > 0 && !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    result.push_str(&text[..head_end]);
    result.push_str(TOOL_TRUNCATION_MARKER);
    let mut tail_start = text.len().saturating_sub(tail);
    while tail_start < text.len() && !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    result.push_str(&text[tail_start..]);
    result
}

pub(super) fn truncate_ordered_tool_text(
    content_parts: &[ToolOutputContentPart],
    limit: usize,
) -> Vec<Option<String>> {
    let mut lowered = vec![None; content_parts.len()];
    let text_indices = content_parts
        .iter()
        .enumerate()
        .filter_map(|(index, part)| matches!(part, ToolOutputContentPart::Text(_)).then_some(index))
        .collect::<Vec<_>>();
    let Some(&first_text_index) = text_indices.first() else {
        return lowered;
    };
    let total_text_bytes = text_indices.iter().fold(0usize, |total, &index| {
        let ToolOutputContentPart::Text(text) = &content_parts[index] else {
            unreachable!("text_indices contains only text parts");
        };
        total.saturating_add(text.len())
    });
    if total_text_bytes <= limit {
        for index in text_indices {
            let ToolOutputContentPart::Text(text) = &content_parts[index] else {
                unreachable!("text_indices contains only text parts");
            };
            lowered[index] = Some(text.clone());
        }
        return lowered;
    }
    if limit == 0 {
        lowered[first_text_index] = Some(String::new());
        return lowered;
    }
    if limit <= TOOL_TRUNCATION_MARKER.len() {
        lowered[first_text_index] = Some(TOOL_TRUNCATION_MARKER[..limit].to_owned());
        return lowered;
    }

    let available = limit - TOOL_TRUNCATION_MARKER.len();
    let mut head_remaining = available / 2;
    let mut tail_remaining = available - head_remaining;
    let mut prefixes = vec![String::new(); content_parts.len()];
    let mut suffixes = vec![String::new(); content_parts.len()];

    for &index in &text_indices {
        if head_remaining == 0 {
            break;
        }
        let ToolOutputContentPart::Text(text) = &content_parts[index] else {
            unreachable!("text_indices contains only text parts");
        };
        let mut end = head_remaining.min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        prefixes[index].push_str(&text[..end]);
        head_remaining -= end;
    }
    for &index in text_indices.iter().rev() {
        if tail_remaining == 0 {
            break;
        }
        let ToolOutputContentPart::Text(text) = &content_parts[index] else {
            unreachable!("text_indices contains only text parts");
        };
        let mut start = text.len().saturating_sub(tail_remaining);
        while start < text.len() && !text.is_char_boundary(start) {
            start += 1;
        }
        suffixes[index].push_str(&text[start..]);
        tail_remaining -= text.len() - start;
    }

    let marker_index = text_indices
        .iter()
        .rev()
        .copied()
        .find(|&index| !prefixes[index].is_empty())
        .unwrap_or(first_text_index);
    for index in text_indices {
        let mut text = std::mem::take(&mut prefixes[index]);
        if index == marker_index {
            text.push_str(TOOL_TRUNCATION_MARKER);
        }
        text.push_str(&suffixes[index]);
        if !text.is_empty() {
            lowered[index] = Some(text);
        }
    }
    lowered
}

pub(super) fn lower_tool_media_part(
    media: &Media,
    model: &Model,
    inline_media: bool,
    result_parts: &mut Vec<ToolResultPart>,
    adjacent_media: &mut Vec<Media>,
    accepted_kinds: &mut Vec<ToolOutputMediaKind>,
    omissions: &mut Vec<String>,
) {
    match media {
        Media::Image(_) => {
            if !model
                .spec
                .capabilities
                .input_modalities
                .contains(octet_ai::Modality::Image)
            {
                omissions
                    .push("[image omitted: the active model does not accept image input]".into());
            } else {
                accepted_kinds.push(ToolOutputMediaKind::Image);
                if inline_media {
                    result_parts.push(ToolResultPart::Media(media.clone()));
                } else {
                    adjacent_media.push(media.clone());
                }
            }
        }
        Media::Audio(audio) => {
            if !model
                .spec
                .capabilities
                .input_modalities
                .contains(octet_ai::Modality::Audio)
            {
                omissions
                    .push("[audio omitted: the active model does not accept audio input]".into());
            } else if model.spec.protocol != Protocol::OpenAiChat {
                omissions
                    .push("[audio omitted: this protocol cannot replay audio tool output]".into());
            } else if !matches!(
                audio.format,
                octet_ai::AudioFormat::Wav | octet_ai::AudioFormat::Mp3
            ) {
                omissions.push(format!(
                    "[audio omitted: OpenAI Chat accepts WAV or MP3 input, got {:?}]",
                    audio.format
                ));
            } else {
                accepted_kinds.push(ToolOutputMediaKind::Audio);
                adjacent_media.push(media.clone());
            }
        }
    }
}

pub(super) fn lower_tool_result(
    call_id: octet_ai::ToolCallId,
    result: &Result<ToolOutput, ToolError>,
    model: &Model,
    text_limit: usize,
    added_tool_names: Vec<String>,
) -> (
    UserMessage,
    Vec<ToolOutputMediaKind>,
    String,
    bool,
    Option<ToolOutputDetails>,
) {
    // Rich policy-error replacements lower like any canonical output, while
    // their typed denial remains on the authoritative ToolError at the caller.
    let rich_result;
    let result = if let Err(error) = result {
        if let Some(output) = error.output() {
            rich_result = Ok(output.clone().with_is_error(true));
            &rich_result
        } else { result }
    } else { result };
    let (raw_text, is_error) = match result {
        Ok(output) => (output.text.as_str(), output.is_error()),
        Err(error) => (error.message.as_str(), true),
    };
    let persisted_text = truncate_tool_text(raw_text, text_limit);
    let mut result_parts = Vec::new();
    let mut adjacent_media = Vec::new();
    let mut accepted_kinds = Vec::new();
    let mut omissions = Vec::new();

    match result {
        Err(_) => result_parts.push(ToolResultPart::Text(persisted_text.clone())),
        Ok(output)
            if matches!(
                model.spec.protocol,
                Protocol::OpenAiResponses | Protocol::AnthropicMessages
            ) =>
        {
            let bounded_text = truncate_ordered_tool_text(output.content_parts(), text_limit);
            for (index, part) in output.content_parts().iter().enumerate() {
                match part {
                    ToolOutputContentPart::Text(_) => {
                        if let Some(text) = bounded_text[index].as_ref() {
                            result_parts.push(ToolResultPart::Text(text.clone()));
                        }
                    }
                    ToolOutputContentPart::Media(media) => lower_tool_media_part(
                        media,
                        model,
                        true,
                        &mut result_parts,
                        &mut adjacent_media,
                        &mut accepted_kinds,
                        &mut omissions,
                    ),
                }
            }
        }
        Ok(output) => {
            result_parts.push(ToolResultPart::Text(persisted_text.clone()));
            for media in output.media() {
                lower_tool_media_part(
                    media,
                    model,
                    false,
                    &mut result_parts,
                    &mut adjacent_media,
                    &mut accepted_kinds,
                    &mut omissions,
                );
            }
        }
    }
    result_parts.extend(omissions.iter().cloned().map(ToolResultPart::Text));
    let effective_is_error = is_error
        || result
            .as_ref()
            .is_ok_and(|output| !output.media().is_empty() && accepted_kinds.is_empty());
    let presented_text = if omissions.is_empty() {
        persisted_text.clone()
    } else if persisted_text.is_empty() {
        omissions.join("\n")
    } else {
        format!("{persisted_text}\n{}", omissions.join("\n"))
    };

    let mut content = Vec::with_capacity(1 + adjacent_media.len());
    content.push(UserPart::ToolResult(ToolResult {
        tool_call_id: call_id,
        content: result_parts,
        is_error: effective_is_error,
        added_tool_names: (!added_tool_names.is_empty()).then_some(added_tool_names),
    }));
    content.extend(adjacent_media.into_iter().map(UserPart::Media));
    (
        UserMessage { content },
        accepted_kinds,
        presented_text,
        effective_is_error,
        result.as_ref().ok().and_then(ToolOutput::details).cloned(),
    )
}

/// The lowerer emits exactly one paired result followed by protocol-adjacent
/// media. Use that authoritative message, not the tool's unaccepted raw output.
pub(super) fn lowered_tool_result_media(message: &UserMessage) -> impl Iterator<Item = &Media> {
    message.content.iter().flat_map(|part| {
        let (nested, adjacent) = match part {
            UserPart::ToolResult(result) => (result.content.as_slice(), None),
            UserPart::Media(media) => (&[][..], Some(media)),
            UserPart::Text(_) => (&[][..], None),
        };
        nested
            .iter()
            .filter_map(|part| match part {
                ToolResultPart::Media(media) => Some(media),
                ToolResultPart::Text(_) => None,
            })
            .chain(adjacent)
    })
}

pub(super) fn persist_pending_cancellations(session: &mut Session) -> Result<(), AgentError> {
    let Some((calls, persisted)) = pending_tool_state(session) else {
        return Ok(());
    };
    let unresolved = calls
        .into_iter()
        .filter(|call| !persisted.contains(&call.id));
    for call in unresolved {
        let text = match call.argument_error {
            Some(argument_error) => rejected_argument_tool_error(argument_error).message,
            None => cancelled_tool_error().message,
        };
        session.append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::ToolResult(ToolResult {
                tool_call_id: call.id,
                content: vec![ToolResultPart::Text(text)],
                is_error: true,
                added_tool_names: None,
            })],
        })))?;
    }
    Ok(())
}

pub(super) fn close_failed_turn(session: &mut Session, model: &Model) -> Result<(), AgentError> {
    let ends_with_user = {
        let context = session.context_ref()?;
        matches!(context.last(), Some(Message::User(_)))
    };
    if ends_with_user {
        session.append_with_metadata(
            EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text(FAILED_TURN_CONTEXT_MARKER.to_owned())],
                model: model.spec.id.clone(),
                protocol: model.spec.protocol,
            })),
            Some(EntryMetadata {
                native_steering: None,
                local_synthetic_assistant: true,
                ..EntryMetadata::default()
            }),
        )?;
    }
    Ok(())
}
