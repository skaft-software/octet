//! Non-destructive normalization of canonical history for a target model.

use std::collections::HashSet;

use crate::catalog::Model;
use crate::types::{
    AssistantMessage, AssistantPart, AudioPayload, ImageSource, Media, Message, Modality, Protocol,
    ToolCallId, ToolResult, ToolResultPart, UserMessage, UserPart,
};

const IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
const TOOL_IMAGE_PLACEHOLDER: &str = "(tool image omitted: model does not support images)";
const UNSUPPORTED_TOOL_IMAGE_PLACEHOLDER: &str =
    "(tool image omitted: target protocol does not support tool images)";
const UNAVAILABLE_IMAGE_PLACEHOLDER: &str =
    "(image omitted: provider media reference is unavailable)";
const UNAVAILABLE_TOOL_IMAGE_PLACEHOLDER: &str =
    "(tool image omitted: provider media reference is unavailable)";
const AUDIO_PLACEHOLDER: &str = "(audio omitted: model does not support audio)";
const TOOL_DOCUMENT_PLACEHOLDER: &str = "(tool document omitted: no tool-result document mapping)";
const ASSISTANT_DOCUMENT_PLACEHOLDER: &str =
    "(assistant document omitted: documents are input only)";
const TOOL_AUDIO_PLACEHOLDER: &str = "(tool audio omitted: model does not support audio)";
const ASSISTANT_IMAGE_PLACEHOLDER: &str =
    "(assistant image omitted: target model cannot replay images)";
const ASSISTANT_AUDIO_PLACEHOLDER: &str =
    "(assistant audio omitted: provider media reference is unavailable)";
const MISSING_TOOL_RESULT: &str = "No result provided";

fn audio_fallback_text(
    audio: &crate::types::AudioMedia,
    placeholder: &str,
    target: &Model,
) -> String {
    let text = audio
        .transcript
        .as_ref()
        .filter(|transcript| !transcript.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| placeholder.to_owned());
    format!(
        "{text} (provider {}, model {})",
        target.endpoint.id.0, target.spec.api_name
    )
}

/// Returns a target-compatible copy of canonical conversation history.
///
/// This is octet's non-destructive equivalent of pi-ai's `transformMessages()`:
/// the input slice is never changed. The transform:
///
/// - replaces unsupported images and audio with visible placeholders;
/// - drops cross-model reasoning, including readable text and opaque state;
/// - normalizes tool-call IDs to the common provider-safe wire shape; and
/// - inserts synthetic error results for tool calls that have no result before
///   the next assistant message (or the end of history), except async calls
///   whose pending state must survive until subsequent strict validation.
///
/// [`crate::AiClient`] applies this automatically before validation and wire
/// serialization. It is public for callers that need to inspect or estimate the
/// exact replay history in advance.
pub fn transform_messages(messages: &[Message], target: &Model) -> Vec<Message> {
    if target.spec.protocol == Protocol::MistralConversations {
        // Native entries have unconstrained string call IDs. Keep content intact
        // for explicit Strict/Lossy handling by the codec, not Chat placeholders.
        return mistral_history(messages.to_vec(), target, &mut Vec::new());
    }
    let transformed = messages
        .iter()
        .map(|message| transform_message(message, target))
        .collect();
    insert_missing_tool_results(transformed)
}

fn mistral_history(
    mut messages: Vec<Message>,
    target: &Model,
    diagnostics: &mut Vec<crate::Diagnostic>,
) -> Vec<Message> {
    for message in &mut messages {
        if let Message::Assistant(assistant) = message {
            if !assistant.produced_by(target.spec.protocol, &target.spec.id) {
                assistant.content.retain(|part| {
                    if matches!(part, AssistantPart::Reasoning(_)) {
                        diagnostics.push(crate::Diagnostic::dropped_reasoning_state());
                        false
                    } else {
                        true
                    }
                });
            }
        }
    }
    insert_missing_tool_results(messages)
}

/// Normalizes replay history while preserving the pending user-input suffix for
/// strict capability validation. A user message is historical once an
/// assistant message follows it; user messages after the final assistant are
/// inputs for the request being opened and must not be replaced by replay
/// placeholders before validation.
#[cfg(test)]
pub(crate) fn transform_request_messages(messages: &[Message], target: &Model) -> Vec<Message> {
    if target.spec.protocol == Protocol::MistralConversations {
        return mistral_history(messages.to_vec(), target, &mut Vec::new());
    }
    let final_assistant = messages
        .iter()
        .rposition(|message| matches!(message, Message::Assistant(_)));
    let transformed = messages
        .iter()
        .enumerate()
        .map(|(index, message)| match message {
            Message::User(user) if final_assistant.is_none_or(|assistant| index > assistant) => {
                Message::User(UserMessage {
                    content: user
                        .content
                        .iter()
                        .map(|part| match part {
                            UserPart::Media(Media::Video(_))
                                if !target.spec.supports_video_input() =>
                            {
                                UserPart::Text(video_placeholder(target))
                            }
                            _ => normalize_pending_user_part(part),
                        })
                        .collect(),
                })
            }
            _ => transform_message(message, target),
        })
        .collect();
    insert_missing_tool_results(transformed)
}

pub(crate) fn prepare_request_history(
    request: &mut crate::Request,
    target: &Model,
) -> Vec<crate::Diagnostic> {
    let mut diagnostics = Vec::new();
    request.messages = transform_request_messages_owned(
        std::mem::take(&mut request.messages),
        target,
        &mut diagnostics,
    );
    diagnostics
}

/// Owned request-history normalization used on the send path.
///
/// A request is consumed by [`crate::AiClient`], so copying every text block,
/// tool payload, and media handle before wire serialization only increases
/// latency and peak memory. This transform moves unchanged canonical values and
/// allocates only when compatibility normalization actually replaces content.
pub(crate) fn transform_request_messages_owned(
    messages: Vec<Message>,
    target: &Model,
    diagnostics: &mut Vec<crate::Diagnostic>,
) -> Vec<Message> {
    if target.spec.protocol == Protocol::MistralConversations {
        return mistral_history(messages, target, diagnostics);
    }
    let final_assistant = messages
        .iter()
        .rposition(|message| matches!(message, Message::Assistant(_)));
    let transformed = messages
        .into_iter()
        .enumerate()
        .map(|(index, message)| match message {
            Message::User(user) if final_assistant.is_none_or(|assistant| index > assistant) => {
                Message::User(UserMessage {
                    content: user
                        .content
                        .into_iter()
                        .map(|part| match part {
                            // A read can retain original video on a text-only route.
                            // Continue this same tool turn without erasing its source.
                            UserPart::Media(Media::Video(_))
                                if !target.spec.supports_video_input() =>
                            {
                                UserPart::Text(video_placeholder(target))
                            }
                            _ => normalize_pending_user_part_owned(part),
                        })
                        .collect(),
                })
            }
            message => transform_message_owned(message, target, diagnostics),
        })
        .collect();
    insert_missing_tool_results(transformed)
}

// Validation probe for hook-introduced media: normalize reasoning and tool
// identities exactly as dispatch does, but never replace media with a fallback.
pub(crate) fn transform_request_preserving_media(
    messages: Vec<Message>,
    target: &Model,
) -> Vec<Message> {
    if target.spec.protocol == Protocol::MistralConversations {
        return mistral_history(messages, target, &mut Vec::new());
    }
    let transformed = messages
        .into_iter()
        .map(|message| match message {
            Message::User(user) => Message::User(UserMessage {
                content: user
                    .content
                    .into_iter()
                    .map(normalize_pending_user_part_owned)
                    .collect(),
            }),
            Message::Assistant(assistant) => {
                let same_model = assistant.produced_by(target.spec.protocol, &target.spec.id);
                Message::Assistant(AssistantMessage {
                    content: assistant
                        .content
                        .into_iter()
                        .filter_map(|part| match part {
                            AssistantPart::Media(_) => Some(part),
                            other => transform_assistant_part_owned(
                                other,
                                same_model,
                                target,
                                &mut || {},
                            ),
                        })
                        .collect(),
                    model: assistant.model,
                    protocol: assistant.protocol,
                })
            }
        })
        .collect();
    insert_missing_tool_results(transformed)
}

fn normalize_pending_user_part_owned(part: UserPart) -> UserPart {
    match part {
        UserPart::Text(_) | UserPart::Media(_) => part,
        UserPart::ToolResult(mut result) => {
            result.tool_call_id = normalize_id_owned(result.tool_call_id);
            UserPart::ToolResult(result)
        }
    }
}

#[cfg(test)]
fn normalize_pending_user_part(part: &UserPart) -> UserPart {
    match part {
        UserPart::Text(_) | UserPart::Media(_) => part.clone(),
        UserPart::ToolResult(result) => UserPart::ToolResult(ToolResult {
            tool_call_id: normalize_id(&result.tool_call_id),
            content: result.content.clone(),
            is_error: result.is_error,
            added_tool_names: result.added_tool_names.clone(),
            tool_name: result.tool_name.clone(),
        }),
    }
}

fn transform_message(message: &Message, target: &Model) -> Message {
    match message {
        Message::User(user) => Message::User(UserMessage {
            content: user
                .content
                .iter()
                .map(|part| transform_user_part(part, target))
                .collect(),
        }),
        Message::Assistant(assistant) => Message::Assistant(AssistantMessage {
            content: assistant
                .content
                .iter()
                .filter_map(|part| transform_assistant_part(part, assistant, target))
                .collect(),
            model: assistant.model.clone(),
            protocol: assistant.protocol,
        }),
    }
}

fn transform_message_owned(
    message: Message,
    target: &Model,
    diagnostics: &mut Vec<crate::Diagnostic>,
) -> Message {
    match message {
        Message::User(user) => Message::User(UserMessage {
            content: user
                .content
                .into_iter()
                .map(|part| transform_user_part_owned(part, target))
                .collect(),
        }),
        Message::Assistant(assistant) => {
            let same_model = assistant.produced_by(target.spec.protocol, &target.spec.id);
            Message::Assistant(AssistantMessage {
                content: assistant
                    .content
                    .into_iter()
                    .filter_map(|part| {
                        transform_assistant_part_owned(part, same_model, target, &mut || {
                            diagnostics.push(crate::Diagnostic::dropped_reasoning_state())
                        })
                    })
                    .collect(),
                model: assistant.model,
                protocol: assistant.protocol,
            })
        }
    }
}

fn transform_user_part(part: &UserPart, target: &Model) -> UserPart {
    match part {
        UserPart::Text(text) => UserPart::Text(text.clone()),
        UserPart::Media(media) => match media {
            Media::Image(image) => {
                if !target
                    .spec
                    .capabilities
                    .input_modalities
                    .contains(Modality::Image)
                {
                    UserPart::Text(IMAGE_PLACEHOLDER.to_string())
                } else if let ImageSource::ProviderRef(reference) = &image.source {
                    if crate::validate::provider_ref_is_usable(reference, target.spec.protocol)
                        && target.spec.protocol == crate::types::Protocol::OpenAiResponses
                    {
                        UserPart::Media(media.clone())
                    } else {
                        UserPart::Text(UNAVAILABLE_IMAGE_PLACEHOLDER.to_string())
                    }
                } else {
                    UserPart::Media(media.clone())
                }
            }
            Media::Audio(audio) => {
                if user_audio_is_replayable(audio, target) {
                    UserPart::Media(media.clone())
                } else {
                    UserPart::Text(audio_fallback_text(audio, AUDIO_PLACEHOLDER, target))
                }
            }
            Media::Video(_) => {
                if target.spec.supports_video_input() {
                    UserPart::Media(media.clone())
                } else {
                    UserPart::Text(video_placeholder(target))
                }
            }
            Media::Document(document) => {
                if document_is_replayable(target) {
                    UserPart::Media(media.clone())
                } else {
                    UserPart::Text(document_placeholder(document))
                }
            }
        },
        UserPart::ToolResult(result) => UserPart::ToolResult(ToolResult {
            tool_call_id: normalize_id(&result.tool_call_id),
            content: result
                .content
                .iter()
                .map(|part| transform_tool_result_part(part, target))
                .collect(),
            is_error: result.is_error,
            added_tool_names: result.added_tool_names.clone(),
            tool_name: result.tool_name.clone(),
        }),
    }
}

fn transform_user_part_owned(part: UserPart, target: &Model) -> UserPart {
    match part {
        UserPart::Text(_) => part,
        UserPart::Media(Media::Image(image)) => {
            if !target
                .spec
                .capabilities
                .input_modalities
                .contains(Modality::Image)
            {
                UserPart::Text(IMAGE_PLACEHOLDER.to_string())
            } else if let ImageSource::ProviderRef(reference) = &image.source {
                if crate::validate::provider_ref_is_usable(reference, target.spec.protocol)
                    && target.spec.protocol == crate::types::Protocol::OpenAiResponses
                {
                    UserPart::Media(Media::Image(image))
                } else {
                    UserPart::Text(UNAVAILABLE_IMAGE_PLACEHOLDER.to_string())
                }
            } else {
                UserPart::Media(Media::Image(image))
            }
        }
        UserPart::Media(Media::Audio(audio)) => {
            if user_audio_is_replayable(&audio, target) {
                UserPart::Media(Media::Audio(audio))
            } else {
                let fallback = audio_fallback_text(&audio, AUDIO_PLACEHOLDER, target);
                UserPart::Text(fallback)
            }
        }
        UserPart::Media(Media::Video(video)) => {
            if target.spec.supports_video_input() {
                UserPart::Media(Media::Video(video))
            } else {
                UserPart::Text(video_placeholder(target))
            }
        }
        UserPart::Media(Media::Document(document)) => {
            if document_is_replayable(target) {
                UserPart::Media(Media::Document(document))
            } else {
                UserPart::Text(document_placeholder(&document))
            }
        }
        UserPart::ToolResult(result) => UserPart::ToolResult(ToolResult {
            tool_call_id: normalize_id_owned(result.tool_call_id),
            content: result
                .content
                .into_iter()
                .map(|part| transform_tool_result_part_owned(part, target))
                .collect(),
            is_error: result.is_error,
            added_tool_names: result.added_tool_names,
            tool_name: result.tool_name,
        }),
    }
}

fn transform_tool_result_part(part: &ToolResultPart, target: &Model) -> ToolResultPart {
    match part {
        ToolResultPart::Text(text) => ToolResultPart::Text(text.clone()),
        ToolResultPart::Media(media) => match media {
            Media::Image(image) => {
                if !target
                    .spec
                    .capabilities
                    .input_modalities
                    .contains(Modality::Image)
                {
                    ToolResultPart::Text(TOOL_IMAGE_PLACEHOLDER.to_string())
                } else if target.spec.protocol == crate::types::Protocol::OpenAiChat {
                    ToolResultPart::Text(UNSUPPORTED_TOOL_IMAGE_PLACEHOLDER.to_string())
                } else if let ImageSource::ProviderRef(reference) = &image.source {
                    if target.spec.protocol == crate::types::Protocol::OpenAiResponses
                        && crate::validate::provider_ref_is_usable(reference, target.spec.protocol)
                    {
                        ToolResultPart::Media(media.clone())
                    } else {
                        ToolResultPart::Text(UNAVAILABLE_TOOL_IMAGE_PLACEHOLDER.to_string())
                    }
                } else {
                    ToolResultPart::Media(media.clone())
                }
            }
            Media::Audio(audio) => {
                ToolResultPart::Text(audio_fallback_text(audio, TOOL_AUDIO_PLACEHOLDER, target))
            }
            Media::Video(_) => {
                if target.spec.supports_video_input() {
                    ToolResultPart::Media(media.clone())
                } else {
                    ToolResultPart::Text(video_placeholder(target))
                }
            }
            Media::Document(_) => ToolResultPart::Text(TOOL_DOCUMENT_PLACEHOLDER.to_string()),
        },
    }
}

fn transform_tool_result_part_owned(part: ToolResultPart, target: &Model) -> ToolResultPart {
    match part {
        ToolResultPart::Text(_) => part,
        ToolResultPart::Media(Media::Image(image)) => {
            if !target
                .spec
                .capabilities
                .input_modalities
                .contains(Modality::Image)
            {
                ToolResultPart::Text(TOOL_IMAGE_PLACEHOLDER.to_string())
            } else if target.spec.protocol == crate::types::Protocol::OpenAiChat {
                ToolResultPart::Text(UNSUPPORTED_TOOL_IMAGE_PLACEHOLDER.to_string())
            } else if let ImageSource::ProviderRef(reference) = &image.source {
                if target.spec.protocol == crate::types::Protocol::OpenAiResponses
                    && crate::validate::provider_ref_is_usable(reference, target.spec.protocol)
                {
                    ToolResultPart::Media(Media::Image(image))
                } else {
                    ToolResultPart::Text(UNAVAILABLE_TOOL_IMAGE_PLACEHOLDER.to_string())
                }
            } else {
                ToolResultPart::Media(Media::Image(image))
            }
        }
        ToolResultPart::Media(Media::Audio(audio)) => {
            let fallback = audio_fallback_text(&audio, TOOL_AUDIO_PLACEHOLDER, target);
            ToolResultPart::Text(fallback)
        }
        ToolResultPart::Media(Media::Video(video)) => {
            if target.spec.supports_video_input() {
                ToolResultPart::Media(Media::Video(video))
            } else {
                ToolResultPart::Text(video_placeholder(target))
            }
        }
        ToolResultPart::Media(Media::Document(_)) => {
            ToolResultPart::Text(TOOL_DOCUMENT_PLACEHOLDER.to_string())
        }
    }
}

/// Inline PDF bytes replay wherever the target route and model accept PDFs.
fn document_is_replayable(target: &Model) -> bool {
    target.spec.protocol.supports_document_input()
        && target
            .spec
            .capabilities
            .input_modalities
            .contains(Modality::Pdf)
}

/// Name the omitted file so the model can still find it with its own tools.
fn document_placeholder(document: &crate::types::DocumentMedia) -> String {
    format!(
        "(PDF {} omitted: model does not support PDF input)",
        document.file_name()
    )
}

fn transform_assistant_part(
    part: &AssistantPart,
    source: &AssistantMessage,
    target: &Model,
) -> Option<AssistantPart> {
    match part {
        AssistantPart::Text(text) => Some(AssistantPart::Text(text.clone())),
        AssistantPart::ProviderMetadata(metadata)
            if source.model == target.spec.id
                && source.protocol == crate::types::Protocol::GoogleGenerativeAi
                && target.spec.protocol == crate::types::Protocol::GoogleGenerativeAi =>
        {
            Some(AssistantPart::ProviderMetadata(metadata.clone()))
        }
        AssistantPart::ProviderMetadata(_) => None,
        AssistantPart::ToolCall(call) => {
            let mut call = call.clone();
            call.id = normalize_id(&call.id);
            Some(AssistantPart::ToolCall(call))
        }
        AssistantPart::Reasoning(reasoning) => {
            let same_model = source.produced_by(target.spec.protocol, &target.spec.id);
            if same_model {
                return Some(AssistantPart::Reasoning(reasoning.clone()));
            }

            // Pi compatibility: packages/ai/src/api/transform-messages.ts:127-137.
            // Foreign thinking is not a visible reply and its signatures are
            // only valid for the producing model. The saved history is untouched.
            None
        }
        AssistantPart::Media(Media::Image(_)) => {
            Some(AssistantPart::Text(ASSISTANT_IMAGE_PLACEHOLDER.to_string()))
        }
        AssistantPart::Media(Media::Video(_)) => {
            Some(AssistantPart::Text("(video is input only)".into()))
        }
        AssistantPart::Media(Media::Document(_)) => Some(AssistantPart::Text(
            ASSISTANT_DOCUMENT_PLACEHOLDER.to_string(),
        )),
        AssistantPart::Media(Media::Audio(audio)) => {
            if assistant_audio_is_replayable(audio, target) {
                Some(part.clone())
            } else {
                Some(AssistantPart::Text(audio_fallback_text(
                    audio,
                    ASSISTANT_AUDIO_PLACEHOLDER,
                    target,
                )))
            }
        }
    }
}

fn transform_assistant_part_owned(
    part: AssistantPart,
    same_model: bool,
    target: &Model,
    on_dropped_reasoning: &mut impl FnMut(),
) -> Option<AssistantPart> {
    match part {
        AssistantPart::Text(_) => Some(part),
        AssistantPart::ProviderMetadata(metadata)
            if same_model && target.spec.protocol == crate::types::Protocol::GoogleGenerativeAi =>
        {
            Some(AssistantPart::ProviderMetadata(metadata))
        }
        AssistantPart::ProviderMetadata(_) => None,
        AssistantPart::ToolCall(mut call) => {
            call.id = normalize_id_owned(call.id);
            Some(AssistantPart::ToolCall(call))
        }
        AssistantPart::Reasoning(reasoning) => {
            if same_model {
                return Some(AssistantPart::Reasoning(reasoning));
            }
            on_dropped_reasoning();
            None
        }
        AssistantPart::Media(Media::Image(_)) => {
            Some(AssistantPart::Text(ASSISTANT_IMAGE_PLACEHOLDER.to_string()))
        }
        AssistantPart::Media(Media::Video(_)) => {
            Some(AssistantPart::Text("(video is input only)".into()))
        }
        AssistantPart::Media(Media::Document(_)) => Some(AssistantPart::Text(
            ASSISTANT_DOCUMENT_PLACEHOLDER.to_string(),
        )),
        AssistantPart::Media(Media::Audio(audio)) => {
            if assistant_audio_is_replayable(&audio, target) {
                Some(AssistantPart::Media(Media::Audio(audio)))
            } else {
                let fallback = audio_fallback_text(&audio, ASSISTANT_AUDIO_PLACEHOLDER, target);
                Some(AssistantPart::Text(fallback))
            }
        }
    }
}

fn normalize_id(id: &ToolCallId) -> ToolCallId {
    ToolCallId(crate::protocol::normalize_tool_call_id(&id.0))
}

fn normalize_id_owned(id: ToolCallId) -> ToolCallId {
    ToolCallId(crate::protocol::normalize_tool_call_id_owned(id.0))
}

fn user_audio_is_replayable(audio: &crate::types::AudioMedia, target: &Model) -> bool {
    target.spec.supports_audio_input(audio.format)
        && matches!(
            audio.payload,
            AudioPayload::Inline(_) | AudioPayload::InlineWithProviderRef { .. }
        )
        || (target.spec.supports_audio_input(audio.format)
            && target.spec.protocol == Protocol::GoogleGenerativeAi
            && matches!(&audio.payload, AudioPayload::ProviderRef(reference) if crate::google_files::usable(reference)))
}

fn assistant_audio_is_replayable(audio: &crate::types::AudioMedia, target: &Model) -> bool {
    if !target.spec.supports_audio_output(audio.format) {
        return false;
    }
    let reference = match &audio.payload {
        AudioPayload::ProviderRef(reference)
        | AudioPayload::InlineWithProviderRef { reference, .. } => reference,
        AudioPayload::Inline(_) => return false,
    };
    crate::validate::provider_ref_is_usable(reference, target.spec.protocol)
}

fn insert_missing_tool_results(messages: Vec<Message>) -> Vec<Message> {
    let mut out = Vec::with_capacity(messages.len());
    let mut pending = Vec::<ToolCallId>::new();
    let mut synthetic_ids = HashSet::<ToolCallId>::new();

    for message in messages {
        match message {
            Message::Assistant(assistant) => {
                push_synthetic_results(&mut out, &mut pending, &mut synthetic_ids);
                for part in &assistant.content {
                    if let AssistantPart::ToolCall(call) = part {
                        // Preserve pending async work and its later real result.
                        // Route/tool/schema authority is checked by request validation;
                        // a provider marker alone never authorizes execution.
                        if call.async_execution {
                            continue;
                        }
                        pending.push(call.id.clone());
                    }
                }
                out.push(Message::Assistant(assistant));
            }
            Message::User(mut user) => {
                user.content.retain(|part| {
                    let UserPart::ToolResult(result) = part else {
                        return true;
                    };
                    if synthetic_ids.contains(&result.tool_call_id) {
                        // A result arriving after a later assistant turn can no
                        // longer satisfy the original protocol position. Keep
                        // the synthetic result and avoid emitting a duplicate.
                        return false;
                    }
                    pending.retain(|id| id != &result.tool_call_id);
                    true
                });
                if !user.content.is_empty() {
                    out.push(Message::User(user));
                }
            }
        }
    }
    push_synthetic_results(&mut out, &mut pending, &mut synthetic_ids);
    out
}

fn push_synthetic_results(
    out: &mut Vec<Message>,
    pending: &mut Vec<ToolCallId>,
    synthetic_ids: &mut HashSet<ToolCallId>,
) {
    if pending.is_empty() {
        return;
    }
    let content = std::mem::take(pending)
        .into_iter()
        .map(|tool_call_id| {
            synthetic_ids.insert(tool_call_id.clone());
            UserPart::ToolResult(ToolResult {
                tool_call_id,
                content: vec![ToolResultPart::Text(MISSING_TOOL_RESULT.to_string())],
                is_error: true,
                added_tool_names: None,
                tool_name: None,
            })
        })
        .collect();
    out.push(Message::User(UserMessage { content }));
}

#[cfg(test)]
mod tests;

pub(crate) fn video_placeholder(target: &Model) -> String {
    format!("(video omitted: provider {} model {} does not support native video or images; original retained)", target.endpoint.id.0, target.spec.api_name)
}
