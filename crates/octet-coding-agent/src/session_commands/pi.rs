//! Pi 1.0.2 v3 JSONL vocabulary, not a guessed role-tagged transcript.
use super::SessionUserMetadata;
use octet_agent::{
    Entry, EntryId, EntryMetadata, EntryValue, SessionRecord, UsageRecord, UsageRecordKind,
};
use octet_ai::{
    AssistantMessage, AssistantPart, Cost, EndpointId, Message, ModelId, Protocol, ReasoningPart,
    StopReason, ToolCall, ToolCallId, ToolResult, ToolResultPart, Usage, UserMessage, UserPart,
};
use serde_json::{json, Value};
use std::collections::HashSet;

type Converted = (Vec<Value>, SessionUserMetadata, String, Vec<String>);
pub(super) fn convert(source: Vec<Value>) -> anyhow::Result<Converted> {
    let header = &source[0];
    fields(
        header,
        &["type", "version", "id", "timestamp", "cwd", "parentSession"],
    )?;
    if header["version"] != 3 {
        anyhow::bail!("unsupported Pi session version; Pi 1.0.2 v3 JSONL required (legacy v1/v2 migration is not transcript import)");
    }
    string(header, "id")?;
    string(header, "cwd")?;
    timestamp(string(header, "timestamp")?)?;
    if header.get("parentSession").is_some() {
        string(header, "parentSession")?;
    }
    let mut ids = HashSet::new();
    let mut out = Vec::new();
    let mut metadata = SessionUserMetadata::default();
    let mut warnings = vec!["Pi JSONL's durable head is its last entry; branch-only Pi exports cannot recover branches omitted by Pi's exporter. Radius organization sharing is not supported.".into()];
    if header.get("parentSession").is_some() {
        warnings.push("Pi parentSession is foreign provenance, not local resume authority; the imported session has no parent-session path.".into());
    }
    let mut head = None;
    let mut total_pico = 0u64;
    for (index, record) in source.iter().enumerate().skip(1) {
        let kind = string(record, "type")?;
        let id = EntryId(string(record, "id")?.to_owned());
        if id.0.is_empty() || id.0.len() > 128 || id.0.chars().any(char::is_control) {
            anyhow::bail!("invalid Pi entry id");
        }
        let parent = match record.get("parentId") {
            Some(Value::Null) => None,
            Some(Value::String(p)) if ids.contains(&EntryId(p.clone())) => Some(EntryId(p.clone())),
            _ => anyhow::bail!(
                "Pi record {} has dangling/forward parent or cycle",
                index + 1
            ),
        };
        if !ids.insert(id.clone()) {
            anyhow::bail!("duplicate Pi entry id");
        }
        let stamp = timestamp(string(record, "timestamp")?)?;
        let mut entry_metadata = EntryMetadata::default();
        let mut usage = None;
        let value = match kind {
            "message" => {
                fields(record, &["type", "id", "parentId", "timestamp", "message"])?;
                let message = &record["message"];
                let role = string(message, "role")?;
                let message_data: serde_json::Map<String, Value> = message.as_object()
                    .ok_or_else(|| anyhow::anyhow!("Pi message must be an object"))?.iter()
                    .filter(|(key, _)| key.as_str() != "content")
                    .map(|(key, value)| (key.clone(), value.clone())).collect();
                entry_metadata.tool_output = Some(octet_agent::ToolOutputDetails::try_new(Some(json!({"piMessage": message_data})), None)?);
                match role {
                    "user" => {
                        fields(message, &["role", "content", "timestamp"])?;
                        integer(message, "timestamp")?;
                        EntryValue::Message(Message::User(UserMessage { content: user_content(&message["content"])? }))
                    }
                    "assistant" => {
                        fields(message, &["role", "content", "timestamp", "api", "provider", "model", "usage", "stopReason", "responseModel", "responseId", "providerThinkingLevel", "thinkingLevel", "diagnostics", "errorMessage", "rawStopReason", "endTurn"])?;
                        integer(message, "timestamp")?;
                        let provider = identifier(message, "provider")?;
                        let model = identifier(message, "model")?;
                        let protocol = protocol(string(message, "api")?)?;
                        let parts = array(&message["content"], "Pi assistant content")?.iter().map(assistant_part).collect::<anyhow::Result<Vec<_>>>()?;
                        let reason = match string(message, "stopReason")? {
                            "stop" => StopReason::EndTurn,
                            "length" => StopReason::MaxTokens,
                            "toolUse" => StopReason::ToolUse,
                            "error" => StopReason::Other("error".into()),
                            "aborted" => StopReason::Other("aborted".into()),
                            _ => anyhow::bail!("unsupported Pi assistant stopReason (pending/deferred attempts are not resumable imports)"),
                        };
                        if let Some(reported) = message.get("usage") {
                            usage = Some((reported.clone(), UsageRecordKind::AssistantTurn { assistant: id.clone() }, Some(EndpointId(provider.into())), Some(ModelId(model.into())), Some(reason)));
                        } else {
                            out.push(serde_json::to_value(SessionRecord::UsageUncertainty { record: octet_agent::UsageUncertaintyRecord {
                                endpoint: EndpointId(provider.into()), model: ModelId(model.into()), operation: "pi_import_assistant".into(),
                            }, bound: None })?);
                            warnings.push("A Pi assistant omitted usage; imported as unknown, not zero.".into());
                        }
                        entry_metadata.replay_safe_tool_calls = Some(Default::default());
                        EntryValue::Message(Message::Assistant(AssistantMessage { content: parts, model: ModelId(model.into()), protocol }))
                    }
                    "toolResult" => {
                        fields(message, &["role", "toolCallId", "toolName", "content", "details", "isError", "timestamp"])?;
                        integer(message, "timestamp")?;
                        let parts = user_content(&message["content"])?;
                        let content = parts.into_iter().map(|part| match part { UserPart::Text(t) => ToolResultPart::Text(t), UserPart::Media(m) => ToolResultPart::Media(m), _ => unreachable!("Pi content contains no tool results") }).collect();
                        string(message, "toolName")?;
                        EntryValue::Message(Message::User(UserMessage { content: vec![UserPart::ToolResult(ToolResult {
                            tool_call_id: ToolCallId(string(message, "toolCallId")?.into()), content,
                            is_error: boolean(message, "isError")?, added_tool_names: None,
                        })] }))
                    }
                    _ => anyhow::bail!("unsupported Pi message role at record {}: system/custom/bashExecution or other semantic messages require a compatible importer", index + 1),
                }
            }
            "branch_summary" => {
                fields(record, &["type", "id", "parentId", "timestamp", "fromId", "summary", "details", "usage", "fromHook"])?;
                if let Some(reported) = record.get("usage") {
                    usage = Some((reported.clone(), UsageRecordKind::Compaction, None, None, None));
                    warnings.push("Pi branch-summary usage retained in Octet's summary/compaction accounting category; Pi omits its route attribution.".into());
                }
                retain_summary_provenance(record, &mut entry_metadata)?;
                EntryValue::BranchSummary { summary: string(record, "summary")?.into(), from_entry: EntryId(string(record,"fromId")?.into()), details: details(record)? }
            }
            "compaction" => {
                fields(record, &["type", "id", "parentId", "timestamp", "summary", "firstKeptEntryId", "tokensBefore", "details", "usage", "fromHook"])?;
                integer(record, "tokensBefore")?;
                let first_kept = EntryId(string(record, "firstKeptEntryId")?.into());
                if first_kept == id { anyhow::bail!("unsupported Pi empty-retention compaction (firstKeptEntryId equals compaction id)"); }
                if let Some(reported) = record.get("usage") { usage = Some((reported.clone(), UsageRecordKind::Compaction, None, None, None)); }
                retain_summary_provenance(record, &mut entry_metadata)?;
                EntryValue::Compaction { summary: string(record, "summary")?.into(), snapcompact: None, first_kept, active_skills: Vec::new(), skill_resources: Vec::new(), details: details(record)? }
            }
            "custom_message" => {
                fields(record, &["type", "id", "parentId", "timestamp", "customType", "content", "details", "display"])?;
                let custom = octet_agent::session::CustomMessage {
                    custom_type: string(record, "customType")?.into(),
                    content: serde_json::from_value(record["content"].clone())?,
                    display: boolean(record, "display")?,
                    details: record.get("details").cloned(),
                };
                custom.validate()?;
                let parts = custom.user_parts();
                entry_metadata.custom_message = Some(custom);
                EntryValue::Message(Message::User(UserMessage { content: parts }))
            }
            "model_change" => {
                fields(record, &["type", "id", "parentId", "timestamp", "provider", "modelId"])?;
                EntryValue::Config { model: Some(format!("{}/{}", identifier(record,"provider")?, identifier(record,"modelId")?)), reasoning: None, reasoning_mode: None }
            }
            "thinking_level_change" => {
                fields(record, &["type", "id", "parentId", "timestamp", "thinkingLevel"])?;
                let level = string(record,"thinkingLevel")?;
                if !matches!(level, "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max") { anyhow::bail!("unsupported Pi thinking level"); }
                EntryValue::Config { model: None, reasoning: Some(level.into()), reasoning_mode: None }
            }
            "session_info" => {
                fields(record, &["type", "id", "parentId", "timestamp", "name"])?;
                metadata.name = record.get("name").map(|_| string(record,"name").map(str::to_owned)).transpose()?;
                empty_config()
            }
            "label" => {
                fields(record, &["type", "id", "parentId", "timestamp", "targetId", "label"])?;
                let target = EntryId(string(record,"targetId")?.into());
                if !ids.contains(&target) || target == id { anyhow::bail!("dangling Pi label target"); }
                out.push(serde_json::to_value(SessionRecord::EntryLabel { entry_id: target, label: record.get("label").map(|_| string(record,"label")).transpose()?.unwrap_or("").into() })?);
                empty_config()
            }
            "usage" => {
                fields(record, &["type", "id", "parentId", "timestamp", "kind", "provider", "model", "usage", "note"])?;
                if string(record,"kind")? != "cache_warm" { anyhow::bail!("unsupported Pi standalone usage category; only cache_warm is representable"); }
                usage = Some((record["usage"].clone(), UsageRecordKind::CacheWarm, Some(EndpointId(identifier(record,"provider")?.into())), Some(ModelId(identifier(record,"model")?.into())), None));
                if record.get("note").is_some() {
                    string(record, "note")?;
                    entry_metadata.tool_output = Some(octet_agent::ToolOutputDetails::try_new(Some(json!({"piUsageNote": record["note"]})), None)?);
                }
                empty_config()
            }
            _ => anyhow::bail!("unsupported Pi semantic record type at record {} (custom/context_edit or unknown records are never silently dropped)", index + 1),
        };
        let entry = Entry {
            id: id.clone(),
            parent,
            metadata: Some(entry_metadata),
            timestamp_unix_ms: Some(stamp),
            value,
        };
        out.push(serde_json::to_value(SessionRecord::Entry(Box::new(entry)))?);
        if let Some((reported, kind, endpoint, model, stop_reason)) = usage {
            let (usage, cost) = reported_usage(&reported)?;
            let pico = cost
                .total
                .checked_mul(1_000_000)
                .and_then(|v| v.checked_add(u64::from(cost.total_picodollars_remainder)))
                .ok_or_else(|| anyhow::anyhow!("Pi cost overflow"))?;
            total_pico = total_pico
                .checked_add(pico)
                .ok_or_else(|| anyhow::anyhow!("Pi cumulative cost overflow"))?;
            out.push(serde_json::to_value(SessionRecord::Usage {
                record: UsageRecord {
                    kind,
                    usage,
                    stop_reason,
                    endpoint,
                    model,
                    completed_at_unix_ms: Some(stamp),
                    cost: Some(cost),
                    cost_microdollars: Some(cost.total),
                    session_cost_microdollars: Some(total_pico / 1_000_000),
                    session_cost_picodollars_remainder: Some((total_pico % 1_000_000) as u32),
                },
            })?);
        }
        head = Some(id);
    }
    match head {
        Some(id) => out.push(serde_json::to_value(SessionRecord::Head {
            id,
            total_cost_microdollars: total_pico / 1_000_000,
            total_cost_picodollars_remainder: (total_pico % 1_000_000) as u32,
        })?),
        None => anyhow::bail!("Pi import has no entries"),
    }
    warnings.sort();
    warnings.dedup();
    Ok((out, metadata, "pi-v3-jsonl".into(), warnings))
}

fn empty_config() -> EntryValue {
    EntryValue::Config {
        model: None,
        reasoning: None,
        reasoning_mode: None,
    }
}
fn fields(value: &Value, allowed: &[&str]) -> anyhow::Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Pi record/content must be an object"))?;
    if let Some(field) = object.keys().find(|k| !allowed.contains(&k.as_str())) {
        // Only field names, never credential-bearing record values, enter diagnostics.
        let field: String = field
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(80)
            .collect();
        anyhow::bail!("unsupported Pi semantic field {field}; refusing lossy import");
    }
    Ok(())
}
fn string<'a>(v: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Pi {key} must be a string"))
}
fn identifier<'a>(v: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    let s = string(v, key)?;
    if s.is_empty()
        || s.len() > 128
        || !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:/".contains(&b))
        || s.contains("://")
    {
        anyhow::bail!("Pi {key} must be a bounded route/model identifier");
    }
    Ok(s)
}
fn integer(v: &Value, key: &str) -> anyhow::Result<u64> {
    v[key]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("Pi {key} must be a nonnegative integer"))
}
fn boolean(v: &Value, key: &str) -> anyhow::Result<bool> {
    v[key]
        .as_bool()
        .ok_or_else(|| anyhow::anyhow!("Pi {key} must be boolean"))
}
fn array<'a>(v: &'a Value, label: &str) -> anyhow::Result<&'a Vec<Value>> {
    v.as_array()
        .ok_or_else(|| anyhow::anyhow!("{label} must be an array"))
}
fn protocol(api: &str) -> anyhow::Result<Protocol> {
    Ok(match api {
        "openai-completions" => Protocol::OpenAiChat,
        // Pi does not export Octet's authoritative Responses sidecars. Importing
        // these as Chat would claim replay fidelity that the file cannot provide.
        "anthropic-messages" => Protocol::AnthropicMessages,
        "bedrock-converse-stream" => Protocol::BedrockConverse,
        "google-generative-ai" | "google-vertex" => Protocol::GoogleGenerativeAi,
        "mistral-conversations" => Protocol::MistralConversations,
        "pi-messages" => Protocol::PiMessages,
        _ => anyhow::bail!("unsupported Pi assistant API: Responses/custom APIs require native replay metadata; no Chat substitution"),
    })
}
fn user_content(v: &Value) -> anyhow::Result<Vec<UserPart>> {
    if let Some(text) = v.as_str() {
        return Ok(vec![UserPart::Text(text.into())]);
    }
    array(v, "Pi user/tool content")?
        .iter()
        .map(|part| match string(part, "type")? {
            "text" => {
                fields(part, &["type", "text"])?;
                Ok(UserPart::Text(string(part, "text")?.into()))
            }
            "image" => {
                fields(part, &["type", "data", "mimeType"])?;
                let image = octet_agent::session::CustomMessagePart::Image {
                    data: string(part, "data")?.into(),
                    mime_type: string(part, "mimeType")?.into(),
                };
                Ok(UserPart::Media(octet_ai::Media::Image(
                    image.image()?.expect("image variant"),
                )))
            }
            _ => anyhow::bail!("unsupported Pi user/tool content block"),
        })
        .collect()
}
fn assistant_part(part: &Value) -> anyhow::Result<AssistantPart> {
    match string(part, "type")? {
        "text" => {
            fields(part, &["type", "text"])?;
            Ok(AssistantPart::Text(string(part, "text")?.into()))
        }
        "thinking" => {
            fields(part, &["type", "thinking"])?;
            Ok(AssistantPart::Reasoning(ReasoningPart {
                text: Some(string(part, "thinking")?.into()),
                state: None,
            }))
        }
        "toolCall" => {
            fields(part, &["type", "id", "name", "arguments"])?;
            if !part["arguments"].is_object() {
                anyhow::bail!("Pi tool arguments must be an object");
            }
            Ok(AssistantPart::ToolCall(ToolCall {
                async_execution: false,
                id: ToolCallId(string(part, "id")?.into()),
                name: string(part, "name")?.into(),
                arguments_json: serde_json::to_string(&part["arguments"])?,
                argument_error: None,
            }))
        }
        _ => anyhow::bail!("unsupported Pi assistant content block"),
    }
}
fn details(record: &Value) -> anyhow::Result<octet_agent::compaction::CompactionDetails> {
    match record.get("details") {
        None => Ok(Default::default()),
        Some(v) => {
            fields(v, &["readFiles", "modifiedFiles"])?;
            Ok(serde_json::from_value(v.clone())?)
        }
    }
}
fn retain_summary_provenance(record: &Value, metadata: &mut EntryMetadata) -> anyhow::Result<()> {
    let mut details = serde_json::Map::new();
    for key in ["tokensBefore", "fromHook"] {
        if let Some(value) = record.get(key) {
            if key == "fromHook" {
                boolean(record, key)?;
            }
            details.insert(key.into(), value.clone());
        }
    }
    if !details.is_empty() {
        metadata.tool_output = Some(octet_agent::ToolOutputDetails::try_new(
            Some(json!({"piSummary": details})),
            None,
        )?);
    }
    Ok(())
}

fn reported_usage(v: &Value) -> anyhow::Result<(Usage, Cost)> {
    fields(
        v,
        &[
            "input",
            "output",
            "cacheRead",
            "cacheWrite",
            "cacheWrite1h",
            "reasoning",
            "totalTokens",
            "cost",
        ],
    )?;
    let usage = Usage {
        input_tokens: integer(v, "input")?,
        output_tokens: integer(v, "output")?,
        cache_read_tokens: integer(v, "cacheRead")?,
        cache_write_tokens: integer(v, "cacheWrite")?,
        cache_write_1h_tokens: v
            .get("cacheWrite1h")
            .map(|_| integer(v, "cacheWrite1h"))
            .transpose()?
            .unwrap_or(0),
        reasoning_tokens: v
            .get("reasoning")
            .map(|_| integer(v, "reasoning"))
            .transpose()?
            .unwrap_or(0),
        total_tokens: integer(v, "totalTokens")?,
    };
    if usage.cache_write_1h_tokens > usage.cache_write_tokens
        || usage.reasoning_tokens > usage.output_tokens
    {
        anyhow::bail!("invalid Pi usage subsets");
    }
    let cost = &v["cost"];
    fields(
        cost,
        &["input", "output", "cacheRead", "cacheWrite", "total"],
    )?;
    fn pico(v: &Value, key: &str) -> anyhow::Result<u64> {
        let dollars = v[key]
            .as_f64()
            .ok_or_else(|| anyhow::anyhow!("Pi usage cost must be numeric"))?;
        let pico = (dollars * 1_000_000_000_000.0).round();
        if !pico.is_finite() || pico < 0.0 || pico >= u64::MAX as f64 {
            anyhow::bail!("Pi usage cost is invalid/out of range");
        }
        Ok(pico as u64)
    }
    let total = pico(cost, "total")?;
    Ok((
        usage,
        Cost {
            input: pico(cost, "input")? / 1_000_000,
            output: pico(cost, "output")? / 1_000_000,
            reasoning: 0,
            cache_read: pico(cost, "cacheRead")? / 1_000_000,
            cache_write: pico(cost, "cacheWrite")? / 1_000_000,
            total: total / 1_000_000,
            total_picodollars_remainder: (total % 1_000_000) as u32,
        },
    ))
}

/// Pi emits `new Date().toISOString()`; validate calendar values, then convert
/// its UTC timestamp without pulling in a second date/time implementation.
fn timestamp(s: &str) -> anyhow::Result<u64> {
    let b = s.as_bytes();
    if b.len() != 24
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'.'
        || b[23] != b'Z'
    {
        anyhow::bail!("Pi timestamp must be ISO UTC with milliseconds");
    }
    fn number(b: &[u8]) -> anyhow::Result<i64> {
        if !b.iter().all(u8::is_ascii_digit) {
            anyhow::bail!("invalid Pi timestamp digits");
        }
        Ok(b.iter().fold(0, |n, b| n * 10 + i64::from(b - b'0')))
    }
    let (year, month, day, h, m, sec, ms) = (
        number(&b[0..4])?,
        number(&b[5..7])?,
        number(&b[8..10])?,
        number(&b[11..13])?,
        number(&b[14..16])?,
        number(&b[17..19])?,
        number(&b[20..23])?,
    );
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    if year < 1970 || day < 1 || day > days || h > 23 || m > 59 || sec > 59 {
        anyhow::bail!("invalid Pi timestamp calendar value");
    }
    let y = year - i64::from(month <= 2);
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Ok(((days * 86400 + h * 3600 + m * 60 + sec) * 1000 + ms) as u64)
}
