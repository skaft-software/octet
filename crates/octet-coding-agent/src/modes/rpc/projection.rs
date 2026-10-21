//! The Pi-compatible JSON value shapes the RPC frontend emits.
//!
//! Why this is separate: every one of these functions answers a single
//! question - "what does this octet value look like on the Pi wire?" - for a
//! different domain type: model, media, usage, message, tool result, session
//! history and the advertised command list. They are pure projections with no
//! framing, no event ordering and no I/O, which is why they can be checked one
//! at a time and why the command loop does not need to own them.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use octet_agent::{Entry, EntryId, EntryValue, InputPart, Session, UserInput};
use octet_ai::{
    AssistantMessage, AssistantPart, Cost, ImageSource, Media, Message, Modality, Model, Protocol,
    StopReason, ToolResult, ToolResultPart, Usage, UserMessage, UserPart,
};
use serde_json::{json, Value};

use crate::app::App;

pub(super) fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub(super) fn iso_timestamp(unix_ms: u64) -> String {
    let seconds = unix_ms / 1_000;
    let millis = unix_ms % 1_000;
    let days = (seconds / 86_400) as i64;
    let seconds_of_day = seconds % 86_400;

    // Gregorian civil date conversion from a Unix-day count. Session
    // timestamps are non-negative, but the formula also remains valid before
    // 1970 should a migrated record ever carry such a value.
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);

    let hour = seconds_of_day / 3_600;
    let minute = seconds_of_day % 3_600 / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

pub(super) fn protocol_name(protocol: &Protocol) -> &'static str {
    match protocol {
        Protocol::OpenAiResponses => "openai-responses",
        Protocol::OpenAiChat => "openai-completions",
        Protocol::AnthropicMessages => "anthropic-messages",
        Protocol::BedrockConverse => "bedrock-converse",
        Protocol::GoogleGenerativeAi => "google-generative-ai",
        Protocol::MistralConversations => "mistral-conversations",
        Protocol::PiMessages => "pi-messages",
    }
}

pub(super) fn dollars(microdollars: u64) -> f64 {
    microdollars as f64 / 1_000_000.0
}

pub(super) fn model_value(model: &Model) -> Value {
    let mut input = vec!["text"];
    if model
        .spec
        .capabilities
        .input_modalities
        .contains(Modality::Image)
    {
        input.push("image");
    }
    let cost = model.spec.pricing.as_ref().map_or_else(
        || json!({"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}),
        |pricing| {
            json!({
                "input": dollars(pricing.input.0),
                "output": dollars(pricing.output.0),
                "cacheRead": dollars(pricing.cache_read.0),
                "cacheWrite": dollars(pricing.cache_write_5m.0)
            })
        },
    );
    json!({
        "id": model.spec.id.0,
        "name": model.spec.display_name.as_deref().unwrap_or(&model.spec.id.0),
        "api": protocol_name(&model.spec.protocol),
        "provider": model.endpoint.id.0,
        "baseUrl": model.endpoint.base_url.as_str().trim_end_matches('/'),
        "reasoning": model.spec.capabilities.reasoning.is_some(),
        "input": input,
        "cost": cost,
        "contextWindow": model.spec.limits.context_window,
        "maxTokens": model.spec.limits.max_output_tokens
    })
}

pub(super) fn media_content(media: &Media) -> Value {
    match media {
        Media::Image(image) => match &image.source {
            ImageSource::Inline(data) => json!({
                "type": "image",
                "data": base64::engine::general_purpose::STANDARD.encode(data),
                "mimeType": image.media_type.as_ref().map_or("image/png", mime::Mime::as_ref)
            }),
            ImageSource::Url(url) => {
                json!({"type": "text", "text": format!("[image: {url}]")})
            }
            ImageSource::ProviderRef(_) => json!({"type": "text", "text": "[image]"}),
        },
        Media::Audio(_) => json!({"type": "text", "text": "[audio]"}),
        Media::Document(document) => {
            json!({"type": "text", "text": format!("[PDF: {}]", document.file_name())})
        }
    }
}

fn user_content_with_optional_timestamp(
    parts: impl IntoIterator<Item = Value>,
    timestamp: Option<u64>,
) -> Value {
    let mut value = json!({
        "role": "user",
        "content": parts.into_iter().collect::<Vec<_>>()
    });
    if let Some(timestamp) = timestamp {
        value["timestamp"] = json!(timestamp);
    }
    value
}

pub(super) fn user_content_at(parts: impl IntoIterator<Item = Value>, timestamp: u64) -> Value {
    user_content_with_optional_timestamp(parts, Some(timestamp))
}

fn user_content(parts: impl IntoIterator<Item = Value>) -> Value {
    user_content_at(parts, now_millis().min(u128::from(u64::MAX)) as u64)
}

pub(super) fn user_input_value(input: &UserInput) -> Value {
    user_content(input.parts.iter().map(|part| match part {
        InputPart::Text(text) => json!({"type": "text", "text": text}),
        InputPart::Media(media) => media_content(media),
    }))
}

pub(super) fn user_value(text: &str) -> Value {
    user_content([json!({"type": "text", "text": text})])
}

pub(super) fn usage_value(usage: &Usage, cost: Option<Cost>) -> Value {
    // Unknown pricing stays null; only settled provider cost can make this a
    // known amount. Dollar numbers are display projections of the exact ledger.
    let cost = cost.map(|cost| json!({
        "input": dollars(cost.input),
        "output": dollars(cost.output.saturating_add(cost.reasoning)),
        "cacheRead": dollars(cost.cache_read),
        "cacheWrite": dollars(cost.cache_write),
        "total": dollars(cost.total) + f64::from(cost.total_picodollars_remainder) / 1_000_000_000_000.0
    }));
    json!({
        "input": usage.input_tokens,
        "output": usage.output_tokens,
        "cacheRead": usage.cache_read_tokens,
        "cacheWrite": usage.cache_write_tokens,
        "totalTokens": usage.total_tokens,
        "cost": cost
    })
}

fn pi_stop_reason(reason: &StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn | StopReason::StopSequence => "stop",
        StopReason::MaxTokens => "length",
        StopReason::ToolUse => "toolUse",
        // A deferred response parked the run instead of finishing the turn. Pi's
        // RPC vocabulary has no completed-turn value for it, and the durable
        // suspension is reported through the run failure
        // (`AgentError::DeferredSuspended`) and the `run_suspend` observer, never
        // as a settled assistant turn.
        StopReason::Deferred
        | StopReason::Refusal
        | StopReason::PauseTurn
        | StopReason::Other(_) => "error",
        // `StopReason` is non-exhaustive, so a future variant must keep the
        // existing conservative projection until it is mapped deliberately.
        _ => "error",
    }
}

pub(super) fn inferred_stop_reason(message: &AssistantMessage) -> StopReason {
    if message
        .content
        .iter()
        .any(|part| matches!(part, AssistantPart::ToolCall(_)))
    {
        StopReason::ToolUse
    } else {
        StopReason::EndTurn
    }
}

fn assistant_content(message: &AssistantMessage) -> Vec<Value> {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            AssistantPart::Text(text) => Some(json!({"type": "text", "text": text})),
            AssistantPart::Reasoning(reasoning) => Some(
                json!({"type": "thinking", "thinking": reasoning.text.as_deref().unwrap_or_default()}),
            ),
            AssistantPart::ToolCall(call) => Some(json!({
                "type": "toolCall",
                "id": call.id.0,
                "name": call.name,
                "arguments": serde_json::from_str::<Value>(&call.arguments_json).unwrap_or(Value::Null)
            })),
            AssistantPart::Media(media) => Some(media_content(media)),
            // Thought signatures are opaque continuation credentials. They stay
            // in the persisted canonical message but never cross the RPC view.
            AssistantPart::ProviderMetadata(_) => None,
        })
        .collect()
}

fn assistant_value_with_optional_timestamp(
    message: &AssistantMessage,
    endpoint: &str,
    usage: &Usage,
    cost: Option<Cost>,
    stop_reason: &StopReason,
    timestamp: Option<u64>,
) -> Value {
    let mut value = json!({
        "role": "assistant",
        "content": assistant_content(message),
        "api": protocol_name(&message.protocol),
        "provider": endpoint,
        "model": message.model.0,
        "usage": usage_value(usage, cost),
        "stopReason": pi_stop_reason(stop_reason)
    });
    if let Some(timestamp) = timestamp {
        value["timestamp"] = json!(timestamp);
    }
    value
}

pub(super) fn assistant_value(
    message: &AssistantMessage,
    endpoint: &str,
    usage: &Usage,
    cost: Option<Cost>,
    stop_reason: &StopReason,
    timestamp: Option<u64>,
) -> Value {
    assistant_value_with_optional_timestamp(
        message,
        endpoint,
        usage,
        cost,
        stop_reason,
        Some(timestamp.unwrap_or_else(|| now_millis().min(u128::from(u64::MAX)) as u64)),
    )
}

pub(super) fn tool_result_content(result: &ToolResult) -> Vec<Value> {
    result
        .content
        .iter()
        .map(|part| match part {
            ToolResultPart::Text(text) => json!({"type": "text", "text": text}),
            ToolResultPart::Media(media) => media_content(media),
        })
        .collect()
}

fn tool_result_value_with_optional_timestamp(
    result: &ToolResult,
    tool_name: &str,
    timestamp: Option<u64>,
) -> Value {
    let mut value = json!({
        "role": "toolResult",
        "toolCallId": result.tool_call_id.0,
        "toolName": tool_name,
        "content": tool_result_content(result),
        "isError": result.is_error
    });
    if let Some(timestamp) = timestamp {
        value["timestamp"] = json!(timestamp);
    }
    value
}

pub(super) fn tool_result_value_at(result: &ToolResult, tool_name: &str, timestamp: u64) -> Value {
    tool_result_value_with_optional_timestamp(result, tool_name, Some(timestamp))
}

struct RpcHistoryMessage {
    message: Message,
    timestamp_unix_ms: Option<u64>,
    user_part_timestamps: Vec<Option<u64>>,
    assistant_entry: Option<EntryId>,
}

fn history_message(entry: &Entry) -> RpcHistoryMessage {
    let message = match &entry.value {
        EntryValue::Message(message) => message.clone(),
        EntryValue::BranchSummary { summary, .. } => Message::User(UserMessage {
            content: vec![UserPart::Text(format!(
                "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n{summary}</summary>"
            ))],
        }),
        _ => unreachable!("history_message is called for visible entries only"),
    };
    let user_part_timestamps = match &message {
        Message::User(message) => vec![entry.timestamp_unix_ms; message.content.len()],
        Message::Assistant(_) => Vec::new(),
    };
    let assistant_entry = matches!(&message, Message::Assistant(_)).then(|| entry.id.clone());
    RpcHistoryMessage {
        message,
        timestamp_unix_ms: entry.timestamp_unix_ms,
        user_part_timestamps,
        assistant_entry,
    }
}

fn tool_result_turn(message: &UserMessage) -> bool {
    !message.content.is_empty()
        && message
            .content
            .iter()
            .any(|part| matches!(part, UserPart::ToolResult(_)))
        && message
            .content
            .iter()
            .all(|part| matches!(part, UserPart::ToolResult(_) | UserPart::Media(_)))
}

fn coalesce_rpc_history(messages: Vec<RpcHistoryMessage>) -> Vec<RpcHistoryMessage> {
    let mut output: Vec<RpcHistoryMessage> = Vec::with_capacity(messages.len());
    for message in messages {
        let merge = match &message.message {
            Message::User(current) if tool_result_turn(current) => {
                output.last().is_some_and(|previous| {
                    matches!(&previous.message, Message::User(previous) if tool_result_turn(previous))
                })
            }
            _ => false,
        };
        if !merge {
            output.push(message);
            continue;
        }

        let Message::User(current) = &message.message else {
            unreachable!("only tool-result user messages are coalesced");
        };
        let previous = output
            .last_mut()
            .expect("a matching previous tool-result message exists");
        let Message::User(previous_user) = &mut previous.message else {
            unreachable!("the matching previous message is a user message");
        };
        let media_start = previous_user
            .content
            .iter()
            .position(|part| matches!(part, UserPart::Media(_)))
            .unwrap_or(previous_user.content.len());
        let current_results = current
            .content
            .iter()
            .zip(&message.user_part_timestamps)
            .filter(|(part, _)| matches!(part, UserPart::ToolResult(_)))
            .map(|(part, timestamp)| (part.clone(), *timestamp))
            .collect::<Vec<_>>();
        previous_user.content.splice(
            media_start..media_start,
            current_results.iter().map(|(part, _)| part.clone()),
        );
        previous.user_part_timestamps.splice(
            media_start..media_start,
            current_results.iter().map(|(_, timestamp)| *timestamp),
        );
        let current_media = current
            .content
            .iter()
            .zip(&message.user_part_timestamps)
            .filter(|(part, _)| matches!(part, UserPart::Media(_)))
            .map(|(part, timestamp)| (part.clone(), *timestamp));
        for (part, timestamp) in current_media {
            previous_user.content.push(part);
            previous.user_part_timestamps.push(timestamp);
        }
    }
    output
}

/// The entries the RPC history projects: the newest compaction on the active
/// branch (whose summary opens the history), and the visible message and
/// branch-summary entries after its boundary, newest first.
fn rpc_history_source(session: &Session) -> (Option<&Entry>, Vec<&Entry>) {
    let mut newest_first = Vec::new();
    let mut compaction = None;
    let mut boundary: Option<&EntryId> = None;
    let mut cursor = session.head_ref();
    while let Some(id) = cursor {
        let entry = session
            .entry(id)
            .expect("session head and parent links refer to durable entries");
        match &entry.value {
            EntryValue::Message(_) | EntryValue::BranchSummary { .. } => {
                newest_first.push(entry);
            }
            EntryValue::Compaction { first_kept, .. } => {
                if boundary.is_none() {
                    compaction = Some(entry);
                    boundary = Some(first_kept);
                }
            }
            EntryValue::ResponsesReasoning { .. }
            | EntryValue::ResponsesSteering { .. }
            | EntryValue::Config { .. }
            | EntryValue::PromptTemplateSelected { .. }
            | EntryValue::ResponsesTurn { .. }
            | EntryValue::ResponsesCompaction { .. }
            | EntryValue::SkillActivated { .. }
            | EntryValue::SkillResourceRead { .. }
            | EntryValue::SkillDeactivated { .. } => {}
        }
        if boundary == Some(id) {
            break;
        }
        cursor = entry.parent.as_ref();
    }
    (compaction, newest_first)
}

fn rpc_history_messages(session: &Session) -> Vec<RpcHistoryMessage> {
    let (compaction, newest_first) = rpc_history_source(session);
    let mut messages = Vec::with_capacity(newest_first.len().saturating_add(1));
    if let Some(Entry {
        value:
            EntryValue::Compaction {
                summary,
                snapcompact,
                ..
            },
        timestamp_unix_ms,
        ..
    }) = compaction
    {
        let mut content = vec![UserPart::Text(format!(
            "[summary of earlier conversation]\n{summary}"
        ))];
        if let Some(snapcompact) = snapcompact {
            content.extend(snapcompact.frames.iter().cloned().map(UserPart::Media));
        }
        messages.push(RpcHistoryMessage {
            user_part_timestamps: vec![*timestamp_unix_ms; content.len()],
            message: Message::User(UserMessage { content }),
            timestamp_unix_ms: *timestamp_unix_ms,
            assistant_entry: None,
        });
    }
    messages.extend(newest_first.into_iter().rev().map(history_message));
    coalesce_rpc_history(messages)
}

/// The shape of one user part, which is all the RPC message count depends on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RpcPartShape {
    Text,
    Media,
    ToolResult,
}

/// `rpc_messages_from_session(session, _).len()` without projecting anything.
///
/// `get_state` reports only the count, and projecting a long resumed history
/// to JSON (cloning every message) dominated its cost. This walks the same
/// entries and applies the same tool-result coalescing and user-message
/// splitting to part shapes instead of values.
pub(super) fn rpc_message_count(session: &Session) -> usize {
    use RpcPartShape::{Media as MediaPart, Text, ToolResult as ResultPart};
    let (compaction, newest_first) = rpc_history_source(session);
    // `None` is an assistant message; `Some(parts)` a user message.
    let mut shapes: Vec<Option<Vec<RpcPartShape>>> = Vec::with_capacity(newest_first.len() + 1);
    if let Some(Entry {
        value: EntryValue::Compaction { snapcompact, .. },
        ..
    }) = compaction
    {
        let frames = snapcompact
            .as_ref()
            .map_or(0, |snapcompact| snapcompact.frames.len());
        let mut parts = vec![Text];
        parts.extend(std::iter::repeat_n(MediaPart, frames));
        shapes.push(Some(parts));
    }
    let tool_result_turn = |parts: &[RpcPartShape]| {
        parts.contains(&ResultPart) && parts.iter().all(|part| *part != Text)
    };
    for entry in newest_first.into_iter().rev() {
        let shape = match &entry.value {
            EntryValue::Message(Message::Assistant(_)) => None,
            EntryValue::Message(Message::User(message)) => Some(
                message
                    .content
                    .iter()
                    .map(|part| match part {
                        UserPart::Text(_) => Text,
                        UserPart::Media(_) => MediaPart,
                        UserPart::ToolResult(_) => ResultPart,
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => Some(vec![Text]),
        };
        // Mirrors `coalesce_rpc_history`: consecutive tool-result turns merge,
        // results before the previous turn's media and new media at the end.
        if let (Some(current), Some(Some(previous))) = (&shape, shapes.last_mut()) {
            if tool_result_turn(current) && tool_result_turn(previous) {
                let media_start = previous
                    .iter()
                    .position(|part| *part == MediaPart)
                    .unwrap_or(previous.len());
                let results = current.iter().filter(|part| **part == ResultPart).count();
                previous.splice(
                    media_start..media_start,
                    std::iter::repeat_n(ResultPart, results),
                );
                previous.extend(current.iter().filter(|part| **part == MediaPart));
                continue;
            }
        }
        shapes.push(shape);
    }
    shapes
        .iter()
        .map(|shape| match shape {
            None => 1,
            // Mirrors the user arm of `rpc_messages_from_session`: each tool
            // result is a message, and each run of other parts is one more.
            Some(parts) => {
                let mut count = 0;
                let mut ordinary = false;
                for part in parts {
                    if *part == ResultPart {
                        count += usize::from(ordinary) + 1;
                        ordinary = false;
                    } else {
                        ordinary = true;
                    }
                }
                count + usize::from(ordinary)
            }
        })
        .sum()
}

pub(super) fn rpc_messages(app: &App) -> Vec<Value> {
    rpc_messages_from_session(app.agent.session(), &app.model)
}

pub(super) fn rpc_messages_from_session(session: &Session, model: &Model) -> Vec<Value> {
    let usage_by_assistant = session
        .usage_records()
        .iter()
        .filter_map(|record| match &record.kind {
            octet_agent::UsageRecordKind::AssistantTurn { assistant } => {
                Some((assistant.clone(), record))
            }
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let mut tool_names = HashMap::<String, String>::new();
    let mut messages = Vec::new();
    for source in rpc_history_messages(session) {
        let RpcHistoryMessage {
            message,
            timestamp_unix_ms,
            user_part_timestamps,
            assistant_entry,
        } = source;
        match message {
            Message::Assistant(message) => {
                for part in &message.content {
                    if let AssistantPart::ToolCall(call) = part {
                        tool_names.insert(call.id.0.clone(), call.name.clone());
                    }
                }
                let record = assistant_entry
                    .as_ref()
                    .and_then(|entry| usage_by_assistant.get(entry).copied());
                let reason = record
                    .and_then(|record| record.stop_reason.clone())
                    .unwrap_or_else(|| inferred_stop_reason(&message));
                messages.push(assistant_value_with_optional_timestamp(
                    &message,
                    record
                        .and_then(|record| record.endpoint.as_ref())
                        .map_or(model.endpoint.id.0.as_str(), |endpoint| endpoint.0.as_str()),
                    record.map_or(&Usage::default(), |record| &record.usage),
                    record.and_then(|record| record.cost),
                    &reason,
                    timestamp_unix_ms
                        .or_else(|| record.and_then(|record| record.completed_at_unix_ms)),
                ));
            }
            Message::User(message) => {
                let mut ordinary = Vec::new();
                let mut ordinary_timestamp = None;
                for (part, timestamp) in message.content.iter().zip(user_part_timestamps) {
                    match part {
                        UserPart::Text(text) => {
                            if ordinary.is_empty() {
                                ordinary_timestamp = timestamp;
                            }
                            ordinary.push(json!({"type": "text", "text": text}));
                        }
                        UserPart::Media(media) => {
                            if ordinary.is_empty() {
                                ordinary_timestamp = timestamp;
                            }
                            ordinary.push(media_content(media));
                        }
                        UserPart::ToolResult(result) => {
                            if !ordinary.is_empty() {
                                messages.push(user_content_with_optional_timestamp(
                                    std::mem::take(&mut ordinary),
                                    ordinary_timestamp.take(),
                                ));
                            }
                            let name = tool_names
                                .get(&result.tool_call_id.0)
                                .map_or("", String::as_str);
                            messages.push(tool_result_value_with_optional_timestamp(
                                result, name, timestamp,
                            ));
                        }
                    }
                }
                if !ordinary.is_empty() {
                    messages.push(user_content_with_optional_timestamp(
                        ordinary,
                        ordinary_timestamp,
                    ));
                }
            }
        }
    }
    messages
}

fn skill_source_info(path: &std::path::Path, trust: octet_agent::SkillTrust) -> Value {
    let (source, scope) = match trust {
        octet_agent::SkillTrust::UserInstalled | octet_agent::SkillTrust::BuiltIn => {
            ("local", "user")
        }
        octet_agent::SkillTrust::Workspace => ("local", "project"),
        octet_agent::SkillTrust::ExplicitExternal => ("cli", "temporary"),
    };
    json!({
        "path": path,
        "source": source,
        "scope": scope,
        "origin": "top-level",
        "baseDir": path.parent()
    })
}

fn prompt_source_info(path: &std::path::Path, trust: crate::prompts::PromptTrust) -> Value {
    let (source, scope) = match trust {
        crate::prompts::PromptTrust::UserInstalled => ("local", "user"),
        crate::prompts::PromptTrust::Workspace => ("local", "project"),
        crate::prompts::PromptTrust::ExplicitExternal => ("cli", "temporary"),
    };
    json!({
        "path": path,
        "source": source,
        "scope": scope,
        "origin": "top-level",
        "baseDir": path.parent()
    })
}

pub(super) fn rpc_commands(app: &App) -> Value {
    let mut commands = Vec::new();
    for (name, description) in app.executable_extensions.command_suggestions() {
        commands.push(json!({
            "name": name,
            "description": description,
            "source": "extension",
            "sourceInfo": {
                "path": name,
                "source": "extension",
                "scope": "temporary",
                "origin": "top-level"
            }
        }));
    }
    for prompt in app.prompts.descriptors().iter() {
        commands.push(json!({
            "name": prompt.name,
            "description": prompt.description,
            "source": "prompt",
            "sourceInfo": prompt_source_info(&prompt.path, prompt.trust)
        }));
    }
    for skill in app.skills.descriptors().iter() {
        if let octet_agent::SkillSource::FileSystem { entrypoint, .. } = &skill.source {
            commands.push(json!({
                "name": format!("skill:{}", skill.id),
                "description": skill.description,
                "source": "skill",
                "sourceInfo": skill_source_info(entrypoint, skill.trust)
            }));
        }
    }
    json!({"commands": commands})
}
