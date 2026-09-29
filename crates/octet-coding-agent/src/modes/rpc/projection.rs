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
use octet_agent::{InputPart, UserInput};
use octet_ai::{
    AssistantMessage, AssistantPart, Cost, ImageSource, Media, Message, Modality, Model, Protocol,
    StopReason, ToolResult, ToolResultPart, Usage, UserPart,
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
    }
}

pub(super) fn user_content_at(parts: impl IntoIterator<Item = Value>, timestamp: u64) -> Value {
    json!({
        "role": "user",
        "content": parts.into_iter().collect::<Vec<_>>(),
        "timestamp": timestamp
    })
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

pub(super) fn assistant_value(
    message: &AssistantMessage,
    endpoint: &str,
    usage: &Usage,
    cost: Option<Cost>,
    stop_reason: &StopReason,
    timestamp: Option<u64>,
) -> Value {
    json!({
        "role": "assistant",
        "content": assistant_content(message),
        "api": protocol_name(&message.protocol),
        "provider": endpoint,
        "model": message.model.0,
        "usage": usage_value(usage, cost),
        "stopReason": pi_stop_reason(stop_reason),
        "timestamp": timestamp.map_or_else(now_millis, u128::from)
    })
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

pub(super) fn tool_result_value_at(result: &ToolResult, tool_name: &str, timestamp: u64) -> Value {
    json!({
        "role": "toolResult",
        "toolCallId": result.tool_call_id.0,
        "toolName": tool_name,
        "content": tool_result_content(result),
        "isError": result.is_error,
        "timestamp": timestamp
    })
}

fn tool_result_value(result: &ToolResult, tool_name: &str) -> Value {
    tool_result_value_at(
        result,
        tool_name,
        now_millis().min(u128::from(u64::MAX)) as u64,
    )
}

pub(super) fn rpc_messages(app: &App) -> Vec<Value> {
    let usage_records = app
        .agent
        .session()
        .usage_records()
        .iter()
        .filter(|record| {
            matches!(
                &record.kind,
                octet_agent::UsageRecordKind::AssistantTurn { .. }
            )
        })
        .collect::<Vec<_>>();
    let mut usage_index = 0usize;
    let mut tool_names = HashMap::<String, String>::new();
    let mut messages = Vec::new();
    for message in app.agent.session().context().unwrap_or_default() {
        match message {
            Message::Assistant(message) => {
                for part in &message.content {
                    if let AssistantPart::ToolCall(call) = part {
                        tool_names.insert(call.id.0.clone(), call.name.clone());
                    }
                }
                let record = usage_records.get(usage_index).copied();
                usage_index = usage_index.saturating_add(1);
                let reason = record
                    .and_then(|record| record.stop_reason.clone())
                    .unwrap_or_else(|| inferred_stop_reason(&message));
                messages.push(assistant_value(
                    &message,
                    record
                        .and_then(|record| record.endpoint.as_ref())
                        .map_or(app.model.endpoint.id.0.as_str(), |endpoint| {
                            endpoint.0.as_str()
                        }),
                    record.map_or(&Usage::default(), |record| &record.usage),
                    record.and_then(|record| record.cost),
                    &reason,
                    record.and_then(|record| record.completed_at_unix_ms),
                ));
            }
            Message::User(message) => {
                let mut ordinary = Vec::new();
                for part in &message.content {
                    match part {
                        UserPart::Text(text) => {
                            ordinary.push(json!({"type": "text", "text": text}));
                        }
                        UserPart::Media(media) => ordinary.push(media_content(media)),
                        UserPart::ToolResult(result) => {
                            if !ordinary.is_empty() {
                                messages.push(user_content(std::mem::take(&mut ordinary)));
                            }
                            let name = tool_names
                                .get(&result.tool_call_id.0)
                                .map_or("", String::as_str);
                            messages.push(tool_result_value(result, name));
                        }
                    }
                }
                if !ordinary.is_empty() {
                    messages.push(user_content(ordinary));
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
