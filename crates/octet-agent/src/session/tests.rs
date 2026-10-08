//! Tests for [`crate::session`].
//!
//! The session file is one append-only JSONL log with six independent
//! concerns: the entry log itself, the durable file lifecycle, the
//! branching/checkpoint tree, the usage ledger, the model-visible context
//! projection, the Responses replay sidecar, and the bounded
//! partial-assistant journal. Each concern has its own module here so a
//! failure names the subsystem that broke rather than "somewhere in the
//! 3000-line session test block". The shared builders below are used by
//! all of them.

use super::*;
use octet_ai::{AssistantMessage, AssistantPart, ModelId, Protocol, ToolCallId, ToolResult};

fn user(text: &str) -> EntryValue {
    EntryValue::Message(Message::User(UserMessage {
        content: vec![UserPart::Text(text.to_string())],
    }))
}

fn assistant(text: &str) -> EntryValue {
    EntryValue::Message(Message::Assistant(AssistantMessage {
        content: vec![AssistantPart::Text(text.to_string())],
        model: ModelId("m".to_string()),
        protocol: Protocol::AnthropicMessages,
    }))
}

fn responses_assistant(text: &str) -> EntryValue {
    EntryValue::Message(Message::Assistant(AssistantMessage {
        content: vec![AssistantPart::Text(text.to_string())],
        model: ModelId("m".to_string()),
        protocol: Protocol::OpenAiResponses,
    }))
}

fn responses_output(id: &str) -> octet_ai::ResponsesOutput {
    octet_ai::ResponsesOutput::new(vec![octet_ai::ResponsesItem::new(serde_json::json!({
        "type": "message",
        "id": id,
        "role": "assistant",
        "content": [{
            "type": "output_text",
            "text": id,
            "annotations": []
        }],
        "unknown": {"preserved": true}
    }))
    .unwrap()])
}

fn responses_compact_output(id: &str) -> octet_ai::ResponsesOutput {
    octet_ai::ResponsesOutput::new(vec![
        octet_ai::ResponsesItem::new(serde_json::json!({
            "type": "message",
            "id": format!("leading-{id}")
        }))
        .unwrap(),
        octet_ai::ResponsesItem::new(serde_json::json!({
            "type": "compaction",
            "id": id,
            "encrypted_content": "opaque"
        }))
        .unwrap(),
    ])
}

fn tool_result(call_id: &str, text: &str) -> EntryValue {
    EntryValue::Message(Message::User(UserMessage {
        content: vec![UserPart::ToolResult(ToolResult {
            tool_call_id: ToolCallId(call_id.to_string()),
            content: vec![octet_ai::ToolResultPart::Text(text.to_string())],
            is_error: false,
            added_tool_names: None,
        })],
    }))
}

fn text_of(m: &Message) -> String {
    match m {
        Message::User(u) => u
            .content
            .iter()
            .map(|p| match p {
                UserPart::Text(t) => t.clone(),
                UserPart::ToolResult(r) => format!("result:{}", r.tool_call_id.0),
                UserPart::Media(_) => "media".to_string(),
            })
            .collect::<Vec<_>>()
            .join("|"),
        Message::Assistant(a) => a
            .content
            .iter()
            .map(|p| match p {
                AssistantPart::Text(t) => t.clone(),
                _ => "other".to_string(),
            })
            .collect::<Vec<_>>()
            .join("|"),
    }
}

fn temp_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("session.jsonl")
}

fn skill_descriptor(id: &str) -> crate::skills::SkillDescriptor {
    crate::skills::SkillDescriptor {
        id: id.to_string(),
        name: id.to_string(),
        description: String::new(),
        license: None,
        compatibility: None,
        metadata: Default::default(),
        allowed_tools: vec![],
        disable_model_invocation: false,
        version: None,
        source: crate::skills::SkillSource::BuiltIn,
        trust: crate::skills::SkillTrust::BuiltIn,
        required_tools: Vec::new(),
        tags: Vec::new(),
    }
}

/// Records one provider attempt that never reported usage. Shared by the
/// usage-ledger and branching suites: both need a session carrying an
/// unsettled uncertainty record before they exercise their own invariant.
fn record_unknown_attempt(session: &mut Session) -> Result<(), SessionError> {
    session.record_usage_uncertainty(
        EndpointId("codex".into()),
        ModelId("openai/gpt-5.4".into()),
        "assistant_turn",
    )
}

#[cfg(test)]
mod branching;
#[cfg(test)]
mod context;
#[cfg(test)]
mod entries;
#[cfg(test)]
mod journal;
#[cfg(test)]
mod replay;
#[cfg(test)]
mod store;
#[cfg(test)]
mod usage;
