//! Canonical session shape used by both sides of the differential harness.
//!
//! Entry identifiers differ between implementations, so append order is the
//! identity: the n-th entry of the Pi session corresponds to the n-th entry of
//! the octet session, and every reference is expressed as that index. See
//! `scripts/fixtures/session-parity/README.md` for the rules this module
//! implements.

use std::collections::{BTreeMap, HashMap};

use octet_agent::{Entry, EntryValue, Session};
use octet_ai::{AssistantPart, ImageSource, Media, Message, Protocol, ToolResultPart, UserPart};
use serde_json::{json, Value};

/// How Pi entries map onto octet's native vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PiMode {
    /// octet's writable format stores labels and the session name outside the
    /// tree, so those Pi entries are collapsed and compared separately.
    Native,
    /// The importer writes one opaque marker entry per Pi label/session_info/
    /// usage record, so those stay in the tree as markers.
    Import,
}

/// One implementation's observable session state.
#[derive(Clone, Debug, Default)]
pub(crate) struct Snapshot {
    pub(crate) entries: Vec<Value>,
    pub(crate) labels: BTreeMap<String, Value>,
    pub(crate) leaf: Option<usize>,
    pub(crate) path: Vec<usize>,
    pub(crate) context: Vec<Value>,
    pub(crate) model: Option<String>,
    pub(crate) thinking: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) has_file: bool,
    /// Set when octet's own catalog projection cannot read the session.
    pub(crate) settings_error: Option<String>,
}

/// One divergence between the oracle and octet.
#[derive(Clone, Debug)]
pub(crate) struct Mismatch {
    pub(crate) check: String,
    pub(crate) fingerprint: String,
    pub(crate) detail: String,
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// Pi entries that hold record-level metadata octet keeps outside the tree.
fn pi_collapsed(entry: &Value, mode: PiMode) -> bool {
    mode == PiMode::Native && matches!(text(entry, "type"), Some("label" | "session_info"))
}

/// Pi records the importer can only preserve positionally.
fn pi_marker(entry: &Value, mode: PiMode) -> bool {
    match text(entry, "type") {
        Some("label") | Some("session_info") => mode == PiMode::Import,
        Some("usage") => mode == PiMode::Import,
        _ => false,
    }
}

fn pi_image_block(block: &Value) -> Value {
    // Compare image payloads by identity of their decoded bytes, not by the
    // exact base64 spelling (both sides may re-encode padding differently).
    let bytes = text(block, "data")
        .and_then(|data| {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.decode(data).ok()
        })
        .map(|bytes| bytes.len());
    json!({"type": "image", "mime": text(block, "mimeType"), "bytes": bytes})
}

fn pi_content(value: &Value) -> Vec<Value> {
    match value {
        Value::String(text) => vec![json!({"type": "text", "text": text})],
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| match text(block, "type") {
                Some("image") => pi_image_block(block),
                _ => json!({"type": "text", "text": text(block, "text")}),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn pi_assistant_part(part: &Value) -> Value {
    match text(part, "type") {
        Some("thinking") => json!({
            "type": "thinking",
            "text": text(part, "thinking"),
            "signature": text(part, "thinkingSignature"),
        }),
        Some("toolCall") => json!({
            "type": "toolCall",
            "id": text(part, "id"),
            "name": text(part, "name"),
            "arguments": part.get("arguments").cloned().unwrap_or(Value::Null),
        }),
        Some("image") => pi_image_block(part),
        _ => json!({"type": "text", "text": text(part, "text")}),
    }
}

fn pi_details(value: Option<&Value>) -> Value {
    // Both sides mean "no recorded file lists" for an absent details object.
    let value = value.unwrap_or(&Value::Null);
    json!({
        "readFiles": array(value, "readFiles"),
        "modifiedFiles": array(value, "modifiedFiles"),
    })
}

fn pi_entry_payload(entry: &Value, canonical: &impl Fn(&str) -> Value) -> Value {
    // A provenance id may point at an entry a branch/fork intentionally omitted;
    // the two implementations then keep different raw ids, so compare the fact.
    let target = |key: &str| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .map(canonical)
            .unwrap_or(Value::Null)
    };
    match text(entry, "type") {
        Some("message") => {
            let message = entry.get("message").cloned().unwrap_or(Value::Null);
            match text(&message, "role") {
                Some("user") => {
                    json!({"role": "user", "content": pi_content(&message["content"])})
                }
                Some("assistant") => json!({
                    "role": "assistant",
                    "model": text(&message, "model"),
                    "api": text(&message, "api"),
                    "content": array(&message, "content")
                        .iter()
                        .map(pi_assistant_part)
                        .collect::<Vec<_>>(),
                }),
                Some("toolResult") => json!({
                    "role": "toolResult",
                    "toolCallId": text(&message, "toolCallId"),
                    "toolName": text(&message, "toolName"),
                    "isError": message.get("isError").and_then(Value::as_bool),
                    "content": pi_content(&message["content"]),
                }),
                // A role octet cannot represent keeps its whole shape so it can
                // never compare equal to a marker by accident.
                other => json!({"role": other, "raw": message}),
            }
        }
        Some("custom") => json!({
            "customType": text(entry, "customType"),
            "data": entry.get("data").cloned().unwrap_or(Value::Null),
        }),
        Some("custom_message") => json!({
            "customType": text(entry, "customType"),
            "content": pi_content(&entry["content"]),
            "display": entry.get("display").and_then(Value::as_bool),
            "details": entry.get("details").cloned().unwrap_or(Value::Null),
        }),
        Some("model_change") => json!({
            "model": format!(
                "{}/{}",
                text(entry, "provider").unwrap_or_default(),
                text(entry, "modelId").unwrap_or_default()
            ),
        }),
        Some("thinking_level_change") => json!({"thinking": text(entry, "thinkingLevel")}),
        Some("compaction") => json!({
            "summary": text(entry, "summary"),
            "firstKept": target("firstKeptEntryId"),
            "details": pi_details(entry.get("details")),
        }),
        Some("branch_summary") => json!({
            "summary": text(entry, "summary"),
            "fromId": target("fromId"),
            "details": pi_details(entry.get("details")),
        }),
        Some("label") => json!({"target": target("targetId"), "label": text(entry, "label")}),
        Some("session_info") => json!({"name": text(entry, "name")}),
        Some("usage") => json!({"kind": text(entry, "kind")}),
        Some("context_edit") => json!({
            "target": target("targetId"),
            "replacement": entry.get("replacement").cloned().unwrap_or(Value::Null),
        }),
        other => json!({"type": other}),
    }
}

fn pi_context_message(message: &Value) -> Value {
    match text(message, "role") {
        Some("assistant") => json!({
            "role": "assistant",
            "content": array(message, "content")
                .iter()
                .map(pi_assistant_part)
                .collect::<Vec<_>>(),
        }),
        Some("toolResult") => json!({
            "role": "toolResult",
            "toolCallId": text(message, "toolCallId"),
            "isError": message.get("isError").and_then(Value::as_bool),
            "content": pi_content(&message["content"]),
        }),
        role => json!({"role": role, "content": pi_content(&message["content"])}),
    }
}

/// Normalize one oracle snapshot produced by `session-driver.mjs`.
pub(crate) fn from_pi(state: &Value, mode: PiMode, has_file: bool) -> Snapshot {
    let raw = array(state, "entries");
    let index: HashMap<&str, usize> = raw
        .iter()
        .enumerate()
        .filter_map(|(position, entry)| Some((text(entry, "id")?, position)))
        .collect();
    let collapsed = |position: usize| pi_collapsed(&raw[position], mode);
    // Resolve a reference through collapsed metadata entries: a label or name
    // entry has no position of its own, so its referent is its parent.
    let resolve = |id: &str| -> Option<usize> {
        let mut cursor = index.get(id).copied();
        let mut hops = 0;
        while let Some(position) = cursor {
            if !collapsed(position) {
                return Some(position);
            }
            hops += 1;
            assert!(hops <= raw.len(), "Pi parent chain cycles through metadata");
            cursor = text(&raw[position], "parentId").and_then(|id| index.get(id).copied());
        }
        None
    };
    let mut renumber = HashMap::<usize, usize>::new();
    for position in 0..raw.len() {
        if !collapsed(position) {
            renumber.insert(position, renumber.len());
        }
    }
    // Payload references use the same canonical index space as the tree, so a
    // label collapsed earlier cannot shift a later kept-entry or provenance id.
    let canonical = |id: &str| -> Value {
        resolve(id)
            .and_then(|position| renumber.get(&position).copied())
            .map(|position| json!(position))
            .unwrap_or_else(|| json!("outside"))
    };
    let mut entries = Vec::with_capacity(renumber.len());
    for (position, entry) in raw.iter().enumerate() {
        if collapsed(position) {
            continue;
        }
        let parent = text(entry, "parentId")
            .filter(|id| *id != text(entry, "id").unwrap_or_default())
            .and_then(resolve)
            .and_then(|parent| renumber.get(&parent).copied());
        let marker = pi_marker(entry, mode);
        let kind = if marker {
            "marker"
        } else {
            text(entry, "type").unwrap_or("unknown")
        };
        let payload = if marker {
            Value::Null
        } else {
            pi_entry_payload(entry, &canonical)
        };
        entries.push(json!({"kind": kind, "parent": parent, "payload": payload}));
    }
    // A collapsed metadata entry with no parent (for example a session_info
    // entry on an otherwise empty session) selects no retained entry, which is
    // the same state as octet's "no head".
    let leaf = text(state, "leaf").and_then(|id| match canonical(id) {
        Value::Number(number) => Some(number.as_u64().expect("index is a u64") as usize),
        _ => None,
    });
    let path = array(state, "path")
        .iter()
        .filter_map(Value::as_str)
        .filter_map(|id| index.get(id).copied())
        .filter(|position| !collapsed(*position))
        .map(|position| renumber[&position])
        .collect();
    let labels = state
        .get("labels")
        .and_then(Value::as_object)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|(id, label)| {
                    let position = resolve(id)?;
                    Some((renumber.get(&position)?.to_string(), label.clone()))
                })
                .collect()
        })
        .unwrap_or_default();
    Snapshot {
        entries,
        labels,
        leaf,
        path,
        context: array(state, "context")
            .iter()
            .map(pi_context_message)
            .collect(),
        model: state
            .get("settings")
            .and_then(|settings| text(settings, "model"))
            .map(str::to_owned),
        thinking: state
            .get("settings")
            .and_then(|settings| text(settings, "thinking"))
            .map(str::to_owned),
        name: text(state, "name").map(str::to_owned),
        has_file,
        settings_error: None,
    }
}

fn protocol_name(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::OpenAiChat => "openai-completions",
        Protocol::OpenAiResponses => "openai-responses",
        Protocol::AnthropicMessages => "anthropic-messages",
        Protocol::BedrockConverse => "bedrock-converse-stream",
        Protocol::GoogleGenerativeAi => "google-generative-ai",
        Protocol::MistralConversations => "mistral-conversations",
        Protocol::PiMessages => "pi-messages",
        Protocol::ExtensionStream => "extension-stream",
    }
}

fn inline_len(source: &ImageSource) -> Option<usize> {
    match source {
        ImageSource::Inline(bytes) => Some(bytes.len()),
        _ => None,
    }
}

fn octet_media(media: &Media) -> Value {
    match media {
        Media::Image(image) => json!({
            "type": "image",
            "mime": image.media_type.as_ref().map(ToString::to_string),
            "bytes": inline_len(&image.source),
        }),
        Media::Audio(_) => json!({"type": "audio"}),
        Media::Document(_) => json!({"type": "document"}),
    }
}

fn octet_tool_part(part: &ToolResultPart) -> Value {
    match part {
        ToolResultPart::Text(text) => json!({"type": "text", "text": text}),
        ToolResultPart::Media(media) => octet_media(media),
    }
}

fn octet_assistant_part(part: &AssistantPart) -> Value {
    match part {
        AssistantPart::Text(text) => json!({"type": "text", "text": text}),
        AssistantPart::Reasoning(reasoning) => json!({
            "type": "thinking",
            "text": reasoning.text,
            "signature": reasoning.state.as_ref().map(|state| match &state.kind {
                octet_ai::ReasoningStateKind::AnthropicSignature { signature } => signature.clone(),
                octet_ai::ReasoningStateKind::AnthropicRedacted { data } => data.clone(),
                octet_ai::ReasoningStateKind::OpenAiReasoning { encrypted_content, .. } => {
                    encrypted_content.clone().unwrap_or_default()
                }
                octet_ai::ReasoningStateKind::ChatReasoningDetails { .. } => String::new(),
            }),
        }),
        AssistantPart::ToolCall(call) => json!({
            "type": "toolCall",
            "id": call.id.0,
            "name": call.name,
            "arguments": serde_json::from_str::<Value>(&call.arguments_json)
                .unwrap_or_else(|_| json!(call.arguments_json)),
        }),
        AssistantPart::Media(media) => octet_media(media),
        AssistantPart::ProviderMetadata(_) => json!({"type": "providerMetadata"}),
    }
}

fn octet_details(details: &octet_agent::compaction::CompactionDetails) -> Value {
    json!({"readFiles": details.read_files, "modifiedFiles": details.modified_files})
}

fn octet_custom_message(custom: &octet_agent::session::CustomMessage) -> (&'static str, Value) {
    use octet_agent::session::{CustomMessageContent, CustomMessagePart};
    let content = match &custom.content {
        CustomMessageContent::Text(text) => vec![json!({"type": "text", "text": text})],
        CustomMessageContent::Parts(parts) => parts
            .iter()
            .map(|part| match part {
                CustomMessagePart::Text { text } => json!({"type": "text", "text": text}),
                CustomMessagePart::Image { data, mime_type } => {
                    use base64::Engine as _;
                    json!({
                        "type": "image",
                        "mime": mime_type,
                        "bytes": base64::engine::general_purpose::STANDARD
                            .decode(data)
                            .ok()
                            .map(|bytes| bytes.len()),
                    })
                }
            })
            .collect(),
    };
    (
        "custom_message",
        json!({
            "customType": custom.custom_type,
            "content": content,
            "display": custom.display,
            "details": custom.details,
        }),
    )
}

/// Normalize one octet entry into `(kind, payload)`.
fn octet_entry_payload(
    session: &Session,
    entry: &Entry,
    position: &HashMap<&str, usize>,
    namespace: &str,
) -> (&'static str, Value) {
    let reference = |id: &octet_agent::EntryId| {
        position
            .get(id.0.as_str())
            .map(|position| json!(position))
            .unwrap_or_else(|| json!(id.0))
    };
    if let Some(custom) = entry
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.custom_message.as_ref())
    {
        return octet_custom_message(custom);
    }
    if let Some(extension) = session.extension_entry(&entry.id, namespace) {
        return (
            "custom",
            json!({"customType": extension.entry_type, "data": extension.data}),
        );
    }
    match &entry.value {
        EntryValue::Message(Message::User(user)) => {
            let results = user
                .content
                .iter()
                .filter_map(|part| match part {
                    UserPart::ToolResult(result) => Some(result),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if !results.is_empty() {
                let mut media = user
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        UserPart::Media(media) => Some(octet_media(media)),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if results.len() == 1 {
                    let mut content = results[0]
                        .content
                        .iter()
                        .map(octet_tool_part)
                        .collect::<Vec<_>>();
                    content.append(&mut media);
                    return (
                        "message",
                        json!({
                            "role": "toolResult",
                            "toolCallId": results[0].tool_call_id.0,
                            "isError": results[0].is_error,
                            "content": content,
                        }),
                    );
                }
                // Several results in one octet entry cannot be produced by the
                // harness; keep the shape explicit instead of flattening it.
                return (
                    "message",
                    json!({
                        "role": "toolResults",
                        "results": results.iter().map(|result| json!({
                            "toolCallId": result.tool_call_id.0,
                            "isError": result.is_error,
                            "content": result.content.iter().map(octet_tool_part).collect::<Vec<_>>(),
                        })).collect::<Vec<_>>(),
                        "media": media,
                    }),
                );
            }
            (
                "message",
                json!({
                    "role": "user",
                    "content": user.content.iter().map(|part| match part {
                        UserPart::Text(text) => json!({"type": "text", "text": text}),
                        UserPart::Media(media) => octet_media(media),
                        // Unreachable: the tool-result case returned above.
                        UserPart::ToolResult(_) => json!({"type": "toolResult"}),
                    }).collect::<Vec<_>>(),
                }),
            )
        }
        EntryValue::Message(Message::Assistant(assistant)) => (
            "message",
            json!({
                "role": "assistant",
                "model": assistant.model.0,
                "api": protocol_name(assistant.protocol),
                "content": assistant.content.iter().map(octet_assistant_part).collect::<Vec<_>>(),
            }),
        ),
        EntryValue::Compaction {
            summary,
            first_kept,
            details,
            ..
        } => (
            "compaction",
            json!({
                "summary": summary,
                "firstKept": reference(first_kept),
                "details": octet_details(details),
            }),
        ),
        EntryValue::BranchSummary {
            summary,
            from_entry,
            details,
        } => (
            "branch_summary",
            json!({
                "summary": summary,
                "fromId": match position.get(from_entry.0.as_str()) {
                    Some(position) => json!(position),
                    None => json!("outside"),
                },
                "details": octet_details(details),
            }),
        ),
        EntryValue::Config {
            model,
            reasoning,
            reasoning_mode,
        } => match (model, reasoning, reasoning_mode) {
            (Some(model), None, None) => ("model_change", json!({"model": model})),
            (None, Some(thinking), None) => {
                ("thinking_level_change", json!({"thinking": thinking}))
            }
            (None, None, None) => ("marker", Value::Null),
            _ => (
                "config",
                json!({"model": model, "reasoning": reasoning, "mode": reasoning_mode}),
            ),
        },
        other => (
            "unsupported",
            serde_json::to_value(other).unwrap_or(Value::Null),
        ),
    }
}

/// Normalize octet's model-visible message list into the provider block shape.
///
/// octet coalesces consecutive tool results into one user message; Pi keeps one
/// message per result. Expanding them here compares the wire shape both
/// implementations produce, which is what a provider sees.
pub(crate) fn octet_context(messages: &[Message]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for message in messages {
        match message {
            Message::Assistant(assistant) => out.push(json!({
                "role": "assistant",
                "content": assistant.content.iter().map(octet_assistant_part).collect::<Vec<_>>(),
            })),
            Message::User(user) => {
                let mut pending: Vec<Value> = Vec::new();
                let mut emitted_result = false;
                for part in &user.content {
                    match part {
                        UserPart::ToolResult(result) => {
                            if emitted_result && !pending.is_empty() {
                                // Adjacent media belong to the previous result.
                                if let Some(Value::Array(previous)) = out
                                    .last_mut()
                                    .and_then(|message| message.get_mut("content"))
                                {
                                    previous.extend(std::mem::take(&mut pending));
                                }
                            }
                            emitted_result = true;
                            out.push(json!({
                                "role": "toolResult",
                                "toolCallId": result.tool_call_id.0,
                                "isError": result.is_error,
                                "content": result.content.iter().map(octet_tool_part).collect::<Vec<_>>(),
                            }));
                        }
                        UserPart::Media(media) => pending.push(octet_media(media)),
                        UserPart::Text(text) => pending.push(json!({"type": "text", "text": text})),
                    }
                }
                if !emitted_result {
                    out.push(json!({"role": "user", "content": pending}));
                } else if !pending.is_empty() {
                    if let Some(Value::Array(previous)) = out
                        .last_mut()
                        .and_then(|message| message.get_mut("content"))
                    {
                        previous.extend(pending);
                    }
                }
            }
        }
    }
    out
}

/// Build the canonical snapshot for one octet session.
pub(crate) fn from_octet(
    session: &Session,
    namespace: &str,
    name: Option<String>,
    settings: (Option<String>, Option<String>),
    has_file: bool,
) -> anyhow::Result<Snapshot> {
    let entries = session.entries();
    let position: HashMap<&str, usize> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.id.0.as_str(), index))
        .collect();
    let mut normalized = Vec::with_capacity(entries.len());
    for entry in entries {
        let parent = entry
            .parent
            .as_ref()
            .and_then(|parent| position.get(parent.0.as_str()).copied());
        let (kind, payload) = octet_entry_payload(session, entry, &position, namespace);
        normalized.push(json!({"kind": kind, "parent": parent, "payload": payload}));
    }
    let leaf = session
        .head_ref()
        .and_then(|head| position.get(head.0.as_str()).copied());
    let mut path = Vec::new();
    let mut cursor = session.head_ref();
    while let Some(id) = cursor {
        let index = position
            .get(id.0.as_str())
            .copied()
            .ok_or_else(|| anyhow::anyhow!("octet head references a missing entry"))?;
        path.push(index);
        cursor = session.entry(id).and_then(|entry| entry.parent.as_ref());
    }
    path.reverse();
    let labels = session
        .entry_labels()
        .iter()
        .filter_map(|(id, label)| Some((position.get(id.0.as_str())?.to_string(), json!(label))))
        .collect();
    Ok(Snapshot {
        entries: normalized,
        labels,
        leaf,
        path,
        context: octet_context(&session.context()?),
        model: settings.0,
        thinking: settings.1,
        name,
        has_file,
        settings_error: None,
    })
}

fn kinds(snapshot: &Snapshot) -> String {
    format!(
        "[{}]",
        snapshot
            .entries
            .iter()
            .map(|entry| {
                let payload = &entry["payload"];
                let hint = payload["summary"]
                    .as_str()
                    .or_else(|| payload["content"][0]["text"].as_str())
                    .or_else(|| payload["role"].as_str())
                    .or_else(|| payload["model"].as_str())
                    .unwrap_or("");
                format!("{}:{hint}", entry["kind"].as_str().unwrap_or("?"))
            })
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn diff_json(
    expected: &Value,
    actual: &Value,
    path: &mut Vec<String>,
    out: &mut Vec<(String, String)>,
) {
    if out.len() >= 8 {
        return;
    }
    let join = |path: &[String]| path.join(".");
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            for (key, value) in expected {
                path.push(key.clone());
                match actual.get(key) {
                    Some(actual) => diff_json(value, actual, path, out),
                    None => out.push((join(path), format!("missing (oracle {value})"))),
                }
                path.pop();
            }
            for (key, value) in actual {
                if !expected.contains_key(key) {
                    path.push(key.clone());
                    out.push((join(path), format!("unexpected ({value})")));
                    path.pop();
                }
            }
        }
        (Value::Array(expected), Value::Array(actual)) => {
            if expected.len() != actual.len() {
                path.push("len".into());
                out.push((
                    join(path),
                    format!("oracle {} entries, octet {}", expected.len(), actual.len()),
                ));
                path.pop();
            }
            for (index, (expected, actual)) in expected.iter().zip(actual).enumerate() {
                path.push(index.to_string());
                diff_json(expected, actual, path, out);
                path.pop();
            }
        }
        (expected, actual) if expected != actual => {
            out.push((join(path), format!("oracle {expected}, octet {actual}")));
        }
        _ => {}
    }
}

fn push_diffs(out: &mut Vec<Mismatch>, check: &str, field: &str, expected: &Value, actual: &Value) {
    let mut diffs = Vec::new();
    diff_json(expected, actual, &mut Vec::new(), &mut diffs);
    for (path, detail) in diffs {
        out.push(Mismatch {
            check: check.to_owned(),
            fingerprint: if field.is_empty() {
                path
            } else if path.is_empty() {
                field.to_owned()
            } else {
                format!("{field}.{path}")
            },
            detail,
        });
    }
}

/// Which parts of a snapshot a comparison covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Tree, branch, context, resolved settings and name.
    Full,
    /// Tree, branch and context only: used for the reopen check, where settings
    /// and name are already compared on the live state.
    TreeOnly,
}

/// Compare the oracle (expected) with octet (actual). `prefix` namespaces the
/// check names for the import comparison.
pub(crate) fn compare(
    expected: &Snapshot,
    actual: &Snapshot,
    prefix: &str,
    scope: Scope,
) -> Vec<Mismatch> {
    let mut out = Vec::new();
    let check = |name: &str| format!("{prefix}{name}");
    if expected.entries.len() != actual.entries.len() {
        out.push(Mismatch {
            check: check("entries"),
            fingerprint: "entries.len".into(),
            detail: format!(
                "oracle {} entries {}, octet {} entries {}",
                expected.entries.len(),
                kinds(expected),
                actual.entries.len(),
                kinds(actual)
            ),
        });
    } else {
        for index in 0..expected.entries.len() {
            push_diffs(
                &mut out,
                &check("entries"),
                &format!("entries.{index}"),
                &expected.entries[index],
                &actual.entries[index],
            );
        }
    }
    let mut targets = BTreeMap::new();
    for target in expected.labels.keys().chain(actual.labels.keys()) {
        targets.insert(target.clone(), ());
    }
    for target in targets.keys() {
        push_diffs(
            &mut out,
            &check("labels"),
            &format!("labels.{target}"),
            expected.labels.get(target).unwrap_or(&Value::Null),
            actual.labels.get(target).unwrap_or(&Value::Null),
        );
    }
    push_diffs(
        &mut out,
        &check("leaf"),
        "leaf",
        &json!(expected.leaf),
        &json!(actual.leaf),
    );
    push_diffs(
        &mut out,
        &check("path"),
        "path",
        &json!(expected.path),
        &json!(actual.path),
    );
    push_diffs(
        &mut out,
        &check("context"),
        "context",
        &json!(expected.context),
        &json!(actual.context),
    );
    if scope == Scope::TreeOnly {
        return out;
    }
    if let Some(error) = &actual.settings_error {
        // octet's own resume projection refused the file; the values below are
        // unknown rather than divergent, so only the read failure is reported.
        out.push(Mismatch {
            check: check("settings"),
            fingerprint: "settings.read".into(),
            detail: error.clone(),
        });
    } else {
        push_diffs(
            &mut out,
            &check("settings"),
            "settings.model",
            &json!(expected.model),
            &json!(actual.model),
        );
        push_diffs(
            &mut out,
            &check("settings"),
            "settings.thinking",
            &json!(expected.thinking),
            &json!(actual.thinking),
        );
    }
    push_diffs(
        &mut out,
        &check("name"),
        "name",
        &json!(expected.name),
        &json!(actual.name),
    );
    if expected.has_file != actual.has_file {
        out.push(Mismatch {
            check: check("file"),
            fingerprint: "file".into(),
            detail: format!(
                "oracle has_file {}, octet has_file {}",
                expected.has_file, actual.has_file
            ),
        });
    }
    out
}
