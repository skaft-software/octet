//! Credential-free native-cache *simulation*, never a provider hit-rate test.
//! Exercises real codecs with a ~150K-token (bytes/4 estimate) coding history.

#[path = "cache_audit_responses_tests.rs"]
mod responses;

use std::collections::HashSet;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::test_fixtures::base_request;
use crate::{
    AssistantMessage, AssistantPart, Message, Protocol, ToolCall, ToolCallId, ToolDef, ToolResult,
    ToolResultPart, UserMessage, UserPart,
};

fn build(protocol: Protocol, request: &crate::Request) -> Value {
    let mut model = super::harness::model(protocol, None);
    std::sync::Arc::make_mut(&mut model.spec).api_name =
        "anthropic.claude-sonnet-4-20250514-v1:0".into();
    if !matches!(
        protocol,
        Protocol::AnthropicMessages | Protocol::BedrockConverse
    ) {
        std::sync::Arc::make_mut(&mut model.spec)
            .capabilities
            .reasoning = None;
    }
    let parts = match protocol {
        Protocol::AnthropicMessages => super::anthropic::build_request(&model, request),
        Protocol::BedrockConverse => super::bedrock::build_request(&model, request),
        Protocol::OpenAiChat => super::openai_chat::build_request(&model, request),
        Protocol::OpenAiResponses => super::openai_responses::build_request(&model, request),
        Protocol::GoogleGenerativeAi => super::google::build_request(&model, request),
        Protocol::MistralConversations => {
            super::mistral_conversations::build_request(&model, request)
        }
        Protocol::PiMessages => super::pi_messages::build_request(&model, request),
    }
    .unwrap();
    serde_json::from_slice(&parts.body).unwrap()
}

/// Each entry is a real provider content block; annotations aren't token content.
fn blocks(body: &Value) -> Vec<(Vec<u8>, bool)> {
    let mut result = Vec::new();
    if let Some(tools) = body["tools"]
        .as_array()
        .or_else(|| body["toolConfig"]["tools"].as_array())
    {
        for tool in tools {
            let mut tool = tool.clone();
            let marker = tool
                .as_object_mut()
                .unwrap()
                .remove("cache_control")
                .is_some();
            result.push((serde_json::to_vec(&tool).unwrap(), marker));
        }
    }
    for group in std::iter::once(&body["system"]).chain(
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| &message["content"]),
    ) {
        for block in group.as_array().into_iter().flatten() {
            if block.get("cachePoint").is_some() {
                result.last_mut().unwrap().1 = true;
                continue;
            }
            let mut block = block.clone();
            let marker = block
                .as_object_mut()
                .unwrap()
                .remove("cache_control")
                .is_some();
            result.push((serde_json::to_vec(&block).unwrap(), marker));
        }
    }
    result
}

#[derive(Default)]
struct CacheModel {
    entries: HashSet<[u8; 32]>,
}

impl CacheModel {
    fn request(&mut self, body: &Value) -> (usize, usize, usize) {
        let blocks = blocks(body);
        let mut hash = Sha256::new();
        let mut prefixes = Vec::new();
        let mut bytes = 0;
        for (block, _) in &blocks {
            hash.update(block);
            bytes += block.len();
            prefixes.push((<[u8; 32]>::from(hash.clone().finalize()), bytes));
        }
        let mut cached = 0;
        let mut markers = 0;
        for (index, (_, marker)) in blocks.iter().enumerate() {
            if !marker {
                continue;
            }
            markers += 1;
            // Anthropic searches at most 20 block boundaries per breakpoint.
            for (key, bytes) in &prefixes[index.saturating_sub(19)..=index] {
                if self.entries.contains(key) {
                    cached = cached.max(*bytes);
                }
            }
        }
        for (index, (_, marker)) in blocks.iter().enumerate() {
            if *marker {
                self.entries.insert(prefixes[index].0);
            }
        }
        (cached, bytes, markers)
    }
}

fn history() -> crate::Request {
    let context = (0..6000).map(|i| format!("fn case_{i:05}() {{ let actual = parser::parse(source_{i:05}); assert_eq!(actual, expected_{i:05}); }}\n")).collect::<String>();
    crate::Request {
        system: Some("You are a coding agent. Preserve source and verify changes.\n".repeat(32)),
        messages: vec![Message::User(UserMessage { content: vec![UserPart::Text(context)] })],
        tools: ["read", "edit", "bash", "extension_probe"].into_iter().map(|name| ToolDef {
            name: name.into(), description: format!("Coding tool {name}"),
            parameters: json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
            constrained_sampling: None, async_execution: false,
        }).collect(),
        ..base_request()
    }
}

fn append_batch(request: &mut crate::Request, protocol: Protocol, turn: usize, count: usize) {
    let calls: Vec<_> = (0..count)
        .map(|index| ToolCall {
            id: ToolCallId(format!("call_{turn}_{index}")),
            name: "read".into(),
            arguments_json: format!("{{\"path\":\"src/case_{index}.rs\"}}"),
            argument_error: None,
            async_execution: false,
        })
        .collect();
    request.messages.push(Message::Assistant(AssistantMessage {
        content: calls.iter().cloned().map(AssistantPart::ToolCall).collect(),
        model: crate::ModelId("fixture-model".into()),
        protocol,
    }));
    request.messages.push(Message::User(UserMessage {
        content: calls
            .into_iter()
            .map(|call| {
                UserPart::ToolResult(ToolResult {
                    tool_call_id: call.id,
                    content: vec![ToolResultPart::Text("source inspected; no mutation".into())],
                    is_error: false,
                    added_tool_names: None,
                })
            })
            .collect(),
    }));
}

fn without_pi_timestamps(mut body: Value) -> Value {
    if let Some(messages) = body
        .get_mut("context")
        .and_then(|c| c.get_mut("messages"))
        .and_then(Value::as_array_mut)
    {
        for message in messages {
            message.as_object_mut().unwrap().remove("timestamp");
        }
    }
    body
}

#[test]
fn all_codecs_preserve_replayed_text_and_catalogs_without_canonical_mutation() {
    for protocol in [
        Protocol::AnthropicMessages,
        Protocol::BedrockConverse,
        Protocol::OpenAiChat,
        Protocol::OpenAiResponses,
        Protocol::GoogleGenerativeAi,
        Protocol::MistralConversations,
        Protocol::PiMessages,
    ] {
        let mut request = history();
        request.cache_retention = crate::CacheRetention::None;
        append_batch(&mut request, protocol, 0, 1);
        let original = serde_json::to_value(&request).unwrap();
        let first = build(protocol, &request);
        if protocol == Protocol::PiMessages {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let second = build(protocol, &request);
        let a = serde_json::to_vec(&first).unwrap();
        let b = serde_json::to_vec(&second).unwrap();
        let prefix = a
            .iter()
            .zip(&b)
            .position(|(a, b)| a != b)
            .unwrap_or(a.len().min(b.len()));
        println!("CODEC protocol={protocol:?} repeated_body_bytes={} raw_lcp={prefix} timestamp_metadata={}", a.len(), protocol == Protocol::PiMessages);
        assert_eq!(
            without_pi_timestamps(first.clone()),
            without_pi_timestamps(second)
        );
        if protocol != Protocol::PiMessages {
            assert_eq!(prefix, a.len());
        }
        append_batch(&mut request, protocol, 1, 1);
        let next = without_pi_timestamps(build(protocol, &request));
        let first = without_pi_timestamps(first);
        let a = first.get("context").unwrap_or(&first);
        let b = next.get("context").unwrap_or(&next);
        for key in [
            "tools",
            "toolConfig",
            "system",
            "systemInstruction",
            "systemPrompt",
            "instructions",
        ] {
            assert_eq!(a.get(key), b.get(key), "{protocol:?}: {key}");
        }
        for key in ["messages", "input", "inputs", "contents"] {
            if let Some(history) = a.get(key).and_then(Value::as_array) {
                assert_eq!(
                    history,
                    &b[key].as_array().unwrap()[..history.len()],
                    "{protocol:?}: old history changed"
                );
            }
        }
        request.messages.truncate(request.messages.len() - 2);
        assert_eq!(original, serde_json::to_value(&request).unwrap());
    }
}

/// Reconstruct only the pre-fix annotations, not an alternate production codec.
fn previous_cache_policy(protocol: Protocol, mut body: Value) -> Value {
    if protocol == Protocol::BedrockConverse {
        if let Some(system) = body["system"].as_array_mut() {
            system.retain(|block| block.get("cachePoint").is_none());
        }
    }
    let messages = body["messages"].as_array_mut().unwrap();
    let last = messages.len() - 1;
    for (index, message) in messages.iter_mut().enumerate() {
        let content = message["content"].as_array_mut().unwrap();
        if protocol == Protocol::BedrockConverse {
            content.retain(|block| block.get("cachePoint").is_none());
        } else if index != last {
            for block in content {
                block.as_object_mut().unwrap().remove("cache_control");
            }
        }
    }
    body
}

#[test]
fn cache_audit_report() {
    for protocol in [Protocol::AnthropicMessages, Protocol::BedrockConverse] {
        for batch in [1, 32] {
            for before in [true, false] {
                let mut request = history();
                let mut cache = CacheModel::default();
                let mut read = 0;
                let mut input = 0;
                let mut markers = 0;
                for turn in 0..9 {
                    let body = build(protocol, &request);
                    let body = if before {
                        previous_cache_policy(protocol, body)
                    } else {
                        body
                    };
                    let (hit, total, count) = cache.request(&body);
                    if turn != 0 {
                        read += hit;
                        input += total;
                    }
                    markers = count;
                    append_batch(&mut request, protocol, turn, batch);
                }
                let efficiency = read as f64 / input as f64;
                if !before {
                    assert!(efficiency >= 0.98);
                }
                let policy = if before {
                    "before_annotations"
                } else {
                    "after"
                };
                println!("CACHE_MODEL policy={policy} protocol={protocol:?} batch={batch} requests=9 warmup=1 input_bytes={input} cached_bytes={read} fresh_est_tokens={} input_est_tokens={} efficiency={:.6}% markers={markers}", (input-read).div_ceil(4), input.div_ceil(4), 100.0*efficiency);
            }
        }
    }
}

#[test]
fn large_tool_batch_keeps_the_previous_conversation_breakpoint_reachable() {
    for protocol in [Protocol::AnthropicMessages, Protocol::BedrockConverse] {
        let mut request = history();
        let mut cache = CacheModel::default();
        cache.request(&build(protocol, &request));
        append_batch(&mut request, protocol, 0, 32);
        let (read, total, markers) = cache.request(&build(protocol, &request));
        assert!(markers <= 4);
        assert!(
            read as f64 / total as f64 >= 0.98,
            "cache model: read={read} total={total}"
        );
    }
}
