//! Non-destructive normalization of canonical history for a target model.

use std::collections::HashSet;
use std::sync::Arc;

use crate::catalog::Model;
use crate::request_history::{common_stamp_prefix, LentHistory, RequestHistory};
use crate::types::{
    AssistantMessage, AssistantPart, AudioPayload, ImageSource, Media, Message, Modality,
    ModelSpec, Protocol, ToolCallId, ToolResult, ToolResultPart, UserMessage, UserPart,
};

/// Leading messages of a lent history already found to need no rewriting for
/// one target model.
///
/// The prefix always ends where no synchronous tool call is still waiting for
/// its result, and never covers a message whose verdict depends on the clock.
#[derive(Default)]
pub(crate) struct CleanPrefix {
    target: Option<Arc<ModelSpec>>,
    stamps: Vec<u64>,
}

impl CleanPrefix {
    /// Whether the kept verdicts were reached for `target`.
    ///
    /// [`message_needs_transform`] reads the model identity, its protocol and
    /// its capabilities, and nothing else of the target.
    fn is_for(&self, target: &Model) -> bool {
        self.target.as_ref().is_some_and(|kept| {
            Arc::ptr_eq(kept, &target.spec)
                || (kept.id == target.spec.id
                    && kept.protocol == target.spec.protocol
                    && kept.capabilities == target.spec.capabilities)
        })
    }
}

#[cfg(test)]
thread_local! {
    static COMPATIBILITY_CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Messages examined for compatibility by this thread since the last reset.
#[cfg(test)]
pub(crate) fn take_compatibility_checks() -> usize {
    COMPATIBILITY_CHECKS.with(|count| count.replace(0))
}

fn media_has_expiry(media: &Media) -> bool {
    match media {
        Media::Image(image) => {
            matches!(&image.source, ImageSource::ProviderRef(r) if r.expires_at.is_some())
        }
        Media::Audio(audio) => matches!(
            &audio.payload,
            AudioPayload::ProviderRef(r)
                | AudioPayload::InlineWithProviderRef { reference: r, .. }
                if r.expires_at.is_some()
        ),
        Media::Document(_) => false,
    }
}

pub(crate) fn message_has_expiring_media(message: &Message) -> bool {
    match message {
        Message::User(user) => user.content.iter().any(|part| match part {
            UserPart::Text(_) => false,
            UserPart::Media(media) => media_has_expiry(media),
            UserPart::ToolResult(result) => result.content.iter().any(|tr_part| match tr_part {
                ToolResultPart::Text(_) => false,
                ToolResultPart::Media(media) => media_has_expiry(media),
            }),
        }),
        Message::Assistant(assistant) => assistant.content.iter().any(|part| match part {
            AssistantPart::Media(media) => media_has_expiry(media),
            _ => false,
        }),
    }
}

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

fn audio_fallback_text(audio: &crate::types::AudioMedia, placeholder: &str) -> String {
    audio
        .transcript
        .as_ref()
        .filter(|transcript| !transcript.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| placeholder.to_owned())
}

/// Returns a target-compatible copy of canonical conversation history.
///
/// This is octet's non-destructive equivalent of pi-ai's `transformMessages()`:
/// the input slice is never changed. The transform:
///
/// - replaces unsupported images and audio with visible placeholders;
/// - converts non-empty cross-model reasoning text into ordinary assistant
///   text while dropping opaque/empty cross-model reasoning;
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
        return insert_missing_tool_results(messages.to_vec());
    }
    let transformed = messages
        .iter()
        .map(|message| transform_message(message, target))
        .collect();
    insert_missing_tool_results(transformed)
}

/// Normalizes replay history while preserving the pending user-input suffix for
/// strict capability validation. A user message is historical once an
/// assistant message follows it; user messages after the final assistant are
/// inputs for the request being opened and must not be replaced by replay
/// placeholders before validation.
#[cfg(test)]
pub(crate) fn transform_request_messages(messages: &[Message], target: &Model) -> Vec<Message> {
    if target.spec.protocol == Protocol::MistralConversations {
        return insert_missing_tool_results(messages.to_vec());
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
                        .map(normalize_pending_user_part)
                        .collect(),
                })
            }
            _ => transform_message(message, target),
        })
        .collect();
    insert_missing_tool_results(transformed)
}

/// Whether the transform would change this message for `target`.
///
/// `is_pending_user` marks a user message after the final assistant turn: an
/// input of the request being opened, which only has its tool-call identities
/// normalized.
fn message_needs_transform(message: &Message, is_pending_user: bool, target: &Model) -> bool {
    if target.spec.protocol == Protocol::MistralConversations {
        return matches!(message, Message::User(user) if user.content.is_empty());
    }
    match message {
        Message::User(user) => {
            if user.content.is_empty() {
                return true;
            }
            if is_pending_user {
                return user.content.iter().any(|part| {
                    matches!(
                        part,
                        UserPart::ToolResult(result)
                            if !crate::protocol::tool_call_id_is_wire_valid(&result.tool_call_id.0)
                    )
                });
            }
            user.content.iter().any(|part| match part {
                UserPart::Text(_) => false,
                UserPart::Media(Media::Image(image)) => {
                    if !target
                        .spec
                        .capabilities
                        .input_modalities
                        .contains(Modality::Image)
                    {
                        true
                    } else if let ImageSource::ProviderRef(reference) = &image.source {
                        !(crate::validate::provider_ref_is_usable(reference, target.spec.protocol)
                            && target.spec.protocol == Protocol::OpenAiResponses)
                    } else {
                        false
                    }
                }
                UserPart::Media(Media::Audio(audio)) => !user_audio_is_replayable(audio, target),
                UserPart::Media(Media::Document(_)) => !document_is_replayable(target),
                UserPart::ToolResult(result) => {
                    !crate::protocol::tool_call_id_is_wire_valid(&result.tool_call_id.0)
                        || result.content.iter().any(|tr_part| match tr_part {
                            ToolResultPart::Text(_) => false,
                            ToolResultPart::Media(Media::Image(image)) => {
                                if !target
                                    .spec
                                    .capabilities
                                    .input_modalities
                                    .contains(Modality::Image)
                                    || target.spec.protocol == Protocol::OpenAiChat
                                {
                                    true
                                } else if let ImageSource::ProviderRef(reference) = &image.source {
                                    !(target.spec.protocol == Protocol::OpenAiResponses
                                        && crate::validate::provider_ref_is_usable(
                                            reference,
                                            target.spec.protocol,
                                        ))
                                } else {
                                    false
                                }
                            }
                            ToolResultPart::Media(Media::Audio(_))
                            | ToolResultPart::Media(Media::Document(_)) => true,
                        })
                }
            })
        }
        Message::Assistant(assistant) => {
            let same_model =
                assistant.model == target.spec.id && assistant.protocol == target.spec.protocol;
            assistant.content.iter().any(|part| match part {
                AssistantPart::Text(_) => false,
                AssistantPart::ProviderMetadata(_) => {
                    !(same_model && target.spec.protocol == Protocol::GoogleGenerativeAi)
                }
                AssistantPart::ToolCall(call) => {
                    !crate::protocol::tool_call_id_is_wire_valid(&call.id.0)
                }
                AssistantPart::Reasoning(_) => !same_model,
                AssistantPart::Media(Media::Image(_))
                | AssistantPart::Media(Media::Document(_)) => true,
                AssistantPart::Media(Media::Audio(audio)) => {
                    !assistant_audio_is_replayable(audio, target)
                }
            })
        }
    }
}

/// Whether [`transform_request_messages_owned`] would change `messages`.
///
/// With a lent history, the leading messages already found clean for this
/// target are not examined again.
fn request_messages_need_transform(
    messages: &[Message],
    target: &Model,
    lent: Option<LentHistory<'_>>,
) -> bool {
    let final_assistant = messages
        .iter()
        .rposition(|message| matches!(message, Message::Assistant(_)));
    // A user message is judged as replay history only once an assistant
    // message follows it, so a verdict is kept for those messages alone.
    let historical_len = final_assistant.map_or(0, |index| index + 1);

    let mut kept = lent.map(|LentHistory { stamps, caches }| {
        let clean = &mut caches.clean;
        if !clean.is_for(target) {
            clean.target = Some(target.spec.clone());
            clean.stamps.clear();
        }
        let unchanged = common_stamp_prefix(&clean.stamps, stamps).min(historical_len);
        clean.stamps.truncate(unchanged);
        (stamps, clean)
    });
    let start = kept.as_ref().map_or(0, |(_, clean)| clean.stamps.len());

    let mut pending_sync_calls: Vec<&ToolCallId> = Vec::new();
    // End of the leading run whose verdict can be kept: historical, not
    // dependent on the clock, and with every synchronous call answered.
    let mut settled = start;
    let mut can_settle = true;
    let mut needs_transform = false;
    for (index, message) in messages.iter().enumerate().skip(start) {
        #[cfg(test)]
        COMPATIBILITY_CHECKS.with(|count| count.set(count.get() + 1));
        let is_pending_user = final_assistant.is_none_or(|assistant| index > assistant);
        if message_needs_transform(message, is_pending_user, target) {
            needs_transform = true;
            break;
        }
        match message {
            Message::Assistant(assistant) => {
                // A following assistant turn with a call still unanswered
                // gets a synthetic result inserted before it.
                if !pending_sync_calls.is_empty() {
                    needs_transform = true;
                    break;
                }
                for part in &assistant.content {
                    if let AssistantPart::ToolCall(call) = part {
                        if !call.async_execution {
                            pending_sync_calls.push(&call.id);
                        }
                    }
                }
            }
            Message::User(user) => {
                for part in &user.content {
                    if let UserPart::ToolResult(result) = part {
                        pending_sync_calls.retain(|id| *id != &result.tool_call_id);
                    }
                }
            }
        }
        can_settle &= index < historical_len && !message_has_expiring_media(message);
        if can_settle && pending_sync_calls.is_empty() {
            settled = index + 1;
        }
    }
    if let Some((stamps, clean)) = kept.as_mut() {
        clean.stamps.extend_from_slice(&stamps[start..settled]);
    }
    needs_transform || !pending_sync_calls.is_empty()
}

/// Request-history normalization for a history that may be lent by its owner.
///
/// A lent history that needs no rewriting is returned as it is and stays lent.
/// One that does need rewriting is handed back to its owner untouched, and the
/// request continues on a rewritten copy: the owner's history is canonical and
/// must never carry one target model's replay placeholders.
pub(crate) fn transform_request_history(
    messages: Vec<Message>,
    target: &Model,
    history: Option<&RequestHistory>,
) -> Vec<Message> {
    let Some(history) = history else {
        return transform_request_messages_owned(messages, target);
    };
    let mut state = history.lock();
    let Some(lent) = state.lent(&messages) else {
        drop(state);
        return transform_request_messages_owned(messages, target);
    };
    if !request_messages_need_transform(&messages, target, Some(lent)) {
        // Debug builds prove the shortcut against the real transform on every
        // request, so the whole test suite checks the two stay equivalent.
        debug_assert_eq!(
            serde_json::to_value(transform_request_messages_owned(messages.clone(), target)).ok(),
            serde_json::to_value(&messages).ok(),
            "a history judged compatible must be left unchanged by the transform"
        );
        return messages;
    }
    let rewritten = transform_request_messages_owned(messages.clone(), target);
    state.give_back(messages);
    rewritten
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
) -> Vec<Message> {
    if target.spec.protocol == Protocol::MistralConversations {
        return insert_missing_tool_results(messages);
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
                        .map(normalize_pending_user_part_owned)
                        .collect(),
                })
            }
            message => transform_message_owned(message, target),
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

fn transform_message_owned(message: Message, target: &Model) -> Message {
    match message {
        Message::User(user) => Message::User(UserMessage {
            content: user
                .content
                .into_iter()
                .map(|part| transform_user_part_owned(part, target))
                .collect(),
        }),
        Message::Assistant(assistant) => {
            let same_model =
                assistant.model == target.spec.id && assistant.protocol == target.spec.protocol;
            Message::Assistant(AssistantMessage {
                content: assistant
                    .content
                    .into_iter()
                    .filter_map(|part| transform_assistant_part_owned(part, same_model, target))
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
                    UserPart::Text(audio_fallback_text(audio, AUDIO_PLACEHOLDER))
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
                let fallback = audio_fallback_text(&audio, AUDIO_PLACEHOLDER);
                UserPart::Text(fallback)
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
                ToolResultPart::Text(audio_fallback_text(audio, TOOL_AUDIO_PLACEHOLDER))
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
            let fallback = audio_fallback_text(&audio, TOOL_AUDIO_PLACEHOLDER);
            ToolResultPart::Text(fallback)
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
            let same_model =
                source.model == target.spec.id && source.protocol == target.spec.protocol;
            if same_model {
                return Some(AssistantPart::Reasoning(reasoning.clone()));
            }

            let redacted = reasoning.state.as_ref().is_some_and(|state| {
                matches!(
                    state.kind,
                    crate::types::ReasoningStateKind::AnthropicRedacted { .. }
                )
            });
            if redacted {
                return None;
            }
            reasoning
                .text
                .as_ref()
                .filter(|text| !text.trim().is_empty())
                .cloned()
                .map(AssistantPart::Text)
        }
        AssistantPart::Media(Media::Image(_)) => {
            Some(AssistantPart::Text(ASSISTANT_IMAGE_PLACEHOLDER.to_string()))
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
                )))
            }
        }
    }
}

fn transform_assistant_part_owned(
    part: AssistantPart,
    same_model: bool,
    target: &Model,
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
            if reasoning.state.as_ref().is_some_and(|state| {
                matches!(
                    state.kind,
                    crate::types::ReasoningStateKind::AnthropicRedacted { .. }
                )
            }) {
                return None;
            }
            reasoning
                .text
                .filter(|text| !text.trim().is_empty())
                .map(AssistantPart::Text)
        }
        AssistantPart::Media(Media::Image(_)) => {
            Some(AssistantPart::Text(ASSISTANT_IMAGE_PLACEHOLDER.to_string()))
        }
        AssistantPart::Media(Media::Document(_)) => Some(AssistantPart::Text(
            ASSISTANT_DOCUMENT_PLACEHOLDER.to_string(),
        )),
        AssistantPart::Media(Media::Audio(audio)) => {
            if assistant_audio_is_replayable(&audio, target) {
                Some(AssistantPart::Media(Media::Audio(audio)))
            } else {
                let fallback = audio_fallback_text(&audio, ASSISTANT_AUDIO_PLACEHOLDER);
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
            })
        })
        .collect();
    out.push(Message::User(UserMessage { content }));
}

#[cfg(test)]
mod tests;
