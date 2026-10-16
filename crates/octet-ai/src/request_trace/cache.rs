//! Opt-in, bounded prefix comparisons. No payload bytes are written to the sink.

use std::collections::VecDeque;
use std::hash::{BuildHasher, Hasher};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{Model, Request, Usage};

const MAX_BODY: usize = 2 * 1024 * 1024;
const MAX_RETAINED: usize = 16 * 1024 * 1024;
const MAX_SNAPSHOTS: usize = 12;

struct Snapshot {
    lane: String,
    stage: &'static str,
    id: u64,
    raw: Vec<u8>,
    prefix: Vec<u8>,
}

/// Shared only by clones of one client; disabled tracing retains no requests.
pub(crate) struct CacheTraceState {
    salt: [u8; 32],
    snapshots: VecDeque<Snapshot>,
}

impl std::fmt::Debug for CacheTraceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheTraceState")
            .field("snapshots", &self.snapshots.len())
            .finish_non_exhaustive()
    }
}

impl Default for CacheTraceState {
    fn default() -> Self {
        // RandomState supplies process-private OS-seeded keys without introducing
        // a new dependency. Never export this salt or an unkeyed content digest.
        let mut salt = [0; 32];
        for chunk in salt.chunks_exact_mut(8) {
            let state = std::collections::hash_map::RandomState::new();
            chunk.copy_from_slice(&state.build_hasher().finish().to_ne_bytes());
        }
        Self {
            salt,
            snapshots: VecDeque::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct CacheTrace {
    state: Arc<Mutex<CacheTraceState>>,
    lane: String,
}

impl CacheTrace {
    pub(super) fn new(state: Arc<Mutex<CacheTraceState>>, model: &Model, req: &Request) -> Self {
        let lane = {
            let state = state.lock().unwrap_or_else(|e| e.into_inner());
            digest(
                &state.salt,
                &serde_json::to_vec(&(
                    &model.endpoint.id,
                    model.endpoint.base_url.as_str(),
                    &model.spec.id,
                    &model.spec.api_name,
                    model.spec.protocol,
                    &req.session_id,
                ))
                .expect("request identity serializes"),
            )
        };
        Self { state, lane }
    }

    pub(super) fn observe(&self, id: u64, stage: &'static str, raw: &[u8]) -> Value {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut line = json!({
            "event": "cache_prefix", "id": id, "lane": self.lane, "stage": stage,
            "bytes": raw.len(), "digest": digest(&state.salt, raw),
            "token_estimator": "serialized_utf8_bytes_div_4_not_provider_tokens",
        });
        if raw.len() > MAX_BODY {
            state
                .snapshots
                .retain(|s| s.lane != self.lane || s.stage != stage);
            line["comparison_unavailable"] = json!("body_exceeds_2_MiB");
            return line;
        }
        let Ok(body) = serde_json::from_slice::<Value>(raw) else {
            state
                .snapshots
                .retain(|s| s.lane != self.lane || s.stage != stage);
            line["comparison_unavailable"] = json!("non_json_body");
            return line;
        };
        line["cache_controls"] = cache_controls(&body);
        let (prefix, segments) = prefix_view(&body);
        line["prefix_view_bytes"] = json!(prefix.len());
        let mut controls = body.clone();
        if let Some(object) = controls.as_object_mut() {
            for key in [
                "tools",
                "toolConfig",
                "system",
                "systemInstruction",
                "instructions",
                "messages",
                "input",
                "inputs",
                "contents",
                "context",
            ] {
                object.remove(key);
            }
        }
        line["controls_digest"] = json!(digest(
            &state.salt,
            &serde_json::to_vec(&controls).expect("JSON controls serialize")
        ));
        line["segments"] = Value::Array(
            segments
                .iter()
                .take(256)
                .map(|(path, start, end)| {
                    json!({
                        "path": path, "offset": start, "bytes": end - start,
                        "digest": digest(&state.salt, &prefix[*start..*end]),
                    })
                })
                .collect(),
        );
        line["segments_omitted"] = json!(segments.len().saturating_sub(256));
        if let Some(index) = state
            .snapshots
            .iter()
            .position(|s| s.lane == self.lane && s.stage == stage)
        {
            let previous = state.snapshots.remove(index).expect("located snapshot");
            let raw_lcp = lcp(&previous.raw, raw);
            let prefix_lcp = lcp(&previous.prefix, &prefix);
            line["previous_id"] = json!(previous.id);
            line["raw_common_prefix_bytes"] = json!(raw_lcp);
            line["raw_previous_bytes"] = json!(previous.raw.len());
            line["prefix_common_bytes"] = json!(prefix_lcp);
            line["prefix_previous_bytes"] = json!(previous.prefix.len());
            line["previous_suffix_estimated_tokens"] =
                json!(previous.prefix.len().saturating_sub(prefix_lcp).div_ceil(4));
            line["current_suffix_estimated_tokens"] =
                json!(prefix.len().saturating_sub(prefix_lcp).div_ceil(4));
            line["first_changed_segment"] = json!(segments
                .iter()
                .find(|(_, _, end)| *end > prefix_lcp)
                .map(|(path, _, _)| path.as_str()));
        }
        let retained = raw.len() + prefix.len();
        while state.snapshots.len() >= MAX_SNAPSHOTS
            || state
                .snapshots
                .iter()
                .map(|s| s.raw.len() + s.prefix.len())
                .sum::<usize>()
                + retained
                > MAX_RETAINED
        {
            state.snapshots.pop_front();
        }
        state.snapshots.push_back(Snapshot {
            lane: self.lane.clone(),
            stage,
            id,
            raw: raw.to_vec(),
            prefix,
        });
        line
    }
}

pub(super) fn usage(id: u64, usage: &Usage) -> Value {
    let fresh = usage.input_tokens.saturating_add(usage.cache_write_tokens);
    let total = fresh.saturating_add(usage.cache_read_tokens);
    json!({
        "event": "cache_usage", "id": id, "source": "provider_reported",
        "uncached_input_tokens": usage.input_tokens, "cache_read_tokens": usage.cache_read_tokens,
        "cache_write_tokens": usage.cache_write_tokens, "cache_write_1h_tokens": usage.cache_write_1h_tokens,
        "fresh_input_tokens": fresh, "total_input_tokens": total,
        "cache_efficiency": (total != 0).then(|| usage.cache_read_tokens as f64 / total as f64),
    })
}

fn digest(salt: &[u8; 32], bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(salt);
    hash.update(bytes);
    format!("{:x}", hash.finalize())
}

fn lcp(a: &[u8], b: &[u8]) -> usize {
    a.iter()
        .zip(b)
        .position(|(a, b)| a != b)
        .unwrap_or(a.len().min(b.len()))
}

/// A diagnostic projection, NOT a provider tokenizer: tools, system, history.
/// Excludes only native cache annotations, never tool arguments/schema fields.
fn prefix_view(body: &Value) -> (Vec<u8>, Vec<(String, usize, usize)>) {
    let nested = body.get("context").filter(|value| value.is_object());
    let body = nested.unwrap_or(body);
    let path_prefix = if nested.is_some() { "/context" } else { "" };
    let mut bytes = Vec::new();
    let mut segments = Vec::new();
    let mut push = |path: String, value: Value| {
        let start = bytes.len();
        serde_json::to_writer(&mut bytes, &value).expect("JSON value serializes");
        bytes.push(b'\n');
        segments.push((path, start, bytes.len()));
    };
    for key in ["tools", "toolConfig"] {
        if let Some(value) = body.get(key) {
            let mut value = value.clone();
            if key == "tools" {
                strip_block_markers(&mut value);
            }
            push(format!("{path_prefix}/{key}"), value);
        }
    }
    for key in [
        "system",
        "systemInstruction",
        "systemPrompt",
        "instructions",
    ] {
        if let Some(value) = body.get(key) {
            let mut value = value.clone();
            strip_block_markers(&mut value);
            push(format!("{path_prefix}/{key}"), value);
        }
    }
    for key in ["messages", "input", "inputs", "contents"] {
        if let Some(values) = body.get(key).and_then(Value::as_array) {
            for (index, value) in values.iter().enumerate() {
                let mut value = value.clone();
                if let Some(content) = value.get_mut("content") {
                    strip_block_markers(content);
                }
                if key == "input" {
                    strip_responses_markers(&mut value);
                }
                push(format!("{path_prefix}/{key}/{index}"), value);
            }
        }
    }
    (bytes, segments)
}

fn cache_controls(body: &Value) -> Value {
    let mut points = Vec::new();
    for key in ["tools", "system"] {
        for (index, block) in body[key].as_array().into_iter().flatten().enumerate() {
            if block.get("cache_control").is_some() || block.get("cachePoint").is_some() {
                points.push(format!("/{key}/{index}"));
            }
        }
    }
    let mut content_blocks = 0;
    for (index, message) in body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        for (block_index, block) in message["content"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            content_blocks += 1;
            if block.get("cache_control").is_some() || block.get("cachePoint").is_some() {
                points.push(format!("/messages/{index}/content/{block_index}"));
            }
        }
    }
    for (index, item) in body["input"].as_array().into_iter().flatten().enumerate() {
        let Some(field) = responses_block_field(item) else {
            continue;
        };
        for (block_index, block) in item[field].as_array().into_iter().flatten().enumerate() {
            if is_responses_input_block(block) && block.get("prompt_cache_breakpoint").is_some() {
                points.push(format!("/input/{index}/{field}/{block_index}"));
            }
        }
    }
    json!({
        "breakpoint_count": points.len(), "breakpoints": points.into_iter().take(256).collect::<Vec<_>>(),
        "message_content_blocks": content_blocks,
        "prompt_cache_key_present": body.get("prompt_cache_key").is_some(),
        "session_id_present": body.get("session_id").is_some() || body["options"].get("sessionId").is_some(),
        "previous_response_id_present": body.get("previous_response_id").is_some(),
    })
}

/// Only documented Responses input content can carry native breakpoints.
/// Do not recursively strip identically named schema, argument or opaque fields.
fn responses_block_field(item: &Value) -> Option<&'static str> {
    match item.get("type").and_then(Value::as_str) {
        Some("function_call_output" | "custom_tool_call_output") => Some("output"),
        None | Some("message")
            if matches!(item["role"].as_str(), Some("developer" | "system" | "user")) =>
        {
            Some("content")
        }
        _ => None,
    }
}

fn is_responses_input_block(block: &Value) -> bool {
    matches!(
        block["type"].as_str(),
        Some("input_text" | "input_image" | "input_file")
    )
}

fn strip_responses_markers(item: &mut Value) {
    let Some(field) = responses_block_field(item) else {
        return;
    };
    for block in item
        .get_mut(field)
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        if is_responses_input_block(block) {
            block
                .as_object_mut()
                .expect("typed content block")
                .remove("prompt_cache_breakpoint");
        }
    }
}

fn strip_block_markers(value: &mut Value) {
    if let Some(blocks) = value.as_array_mut() {
        blocks.retain(|block| block.get("cachePoint").is_none());
        for block in blocks {
            if let Some(object) = block.as_object_mut() {
                object.remove("cache_control");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_breakpoint_is_not_a_content_mutation() {
        let a = json!({"system":[{"text":"stable"}], "messages":[{"role":"user","content":[{"type":"text","text":"old","cache_control":{"type":"ephemeral"}}]}]});
        let b = json!({"system":[{"text":"stable"}], "messages":[{"role":"user","content":[{"type":"text","text":"old"}]},{"role":"assistant","content":[{"type":"text","text":"new"}]}]});
        let (a, _) = prefix_view(&a);
        let (b, _) = prefix_view(&b);
        assert_eq!(lcp(&a, &b), a.len());
    }

    #[test]
    fn never_strips_argument_or_schema_fields_named_cache_control() {
        let a = json!({"tools":[{"input_schema":{"cache_control": "keep"}}], "messages":[{"content":[{"type":"tool_use","input":{"cache_control":"keep"}}]}]});
        let (bytes, _) = prefix_view(&a);
        assert_eq!(
            String::from_utf8(bytes)
                .unwrap()
                .matches("cache_control")
                .count(),
            2
        );
    }

    #[test]
    fn responses_breakpoints_are_reported_at_supported_content_boundaries() {
        let body = json!({
            "tools": [{"parameters": {"prompt_cache_breakpoint": "SECRET schema"}}],
            "input": [
                {"role":"developer","content":[{"type":"input_text","text":"SECRET instructions","prompt_cache_breakpoint":{"mode":"explicit"}}]},
                {"type":"message","role":"user","content":[
                    {"type":"input_image","image_url":"SECRET image","prompt_cache_breakpoint":{"mode":"explicit"}},
                    {"type":"input_file","file_id":"SECRET file","prompt_cache_breakpoint":{"mode":"explicit"}}
                ]},
                {"type":"function_call_output","call_id":"SECRET call","output":[{"type":"input_text","text":"SECRET result","prompt_cache_breakpoint":{"mode":"explicit"}}]},
                {"type":"custom_tool_call_output","call_id":"SECRET custom","output":[{"type":"input_text","text":"SECRET custom result","prompt_cache_breakpoint":{"mode":"explicit"}}]},
                {"type":"additional_tools","tools":[{"prompt_cache_breakpoint":"SECRET schema"}]},
                {"type":"message","role":"assistant","content":[{"type":"output_text","text":"SECRET output","prompt_cache_breakpoint":"not a native boundary"}]}
            ]
        });
        let line = observer().observe(1, "encoded", &serde_json::to_vec(&body).unwrap());
        assert_eq!(line["cache_controls"]["breakpoint_count"], 5);
        assert_eq!(
            line["cache_controls"]["breakpoints"],
            json!([
                "/input/0/content/0",
                "/input/1/content/0",
                "/input/1/content/1",
                "/input/2/output/0",
                "/input/3/output/0"
            ])
        );
        assert!(!line.to_string().contains("SECRET"));
    }

    #[test]
    fn responses_breakpoint_positions_keep_the_diagnostic_output_bound() {
        let body = json!({"input":[{"role":"user","content":vec![json!({
            "type":"input_text", "text":"SECRET", "prompt_cache_breakpoint":{"mode":"explicit"}
        }); 300]}]});
        let line = observer().observe(1, "wire_websocket", &serde_json::to_vec(&body).unwrap());
        assert_eq!(line["cache_controls"]["breakpoint_count"], 300);
        assert_eq!(
            line["cache_controls"]["breakpoints"]
                .as_array()
                .unwrap()
                .len(),
            256
        );
        assert!(!line.to_string().contains("SECRET"));
    }

    #[test]
    fn moving_responses_breakpoints_are_not_history_mutations() {
        let first = json!({"input":[
            {"type":"message","role":"user","content":[{"type":"input_text","text":"SECRET prompt","prompt_cache_breakpoint":{"mode":"explicit"}}]},
            {"type":"function_call_output","call_id":"call","output":[{"type":"input_text","text":"SECRET tool","prompt_cache_breakpoint":{"mode":"explicit"}}]}
        ]});
        let mut next = first.clone();
        next["input"][0]["content"][0]
            .as_object_mut()
            .unwrap()
            .remove("prompt_cache_breakpoint");
        next["input"][1]["output"][0]
            .as_object_mut()
            .unwrap()
            .remove("prompt_cache_breakpoint");
        next["input"].as_array_mut().unwrap().push(json!({"role":"user","content":[{"type":"input_text","text":"new","prompt_cache_breakpoint":{"mode":"explicit"}}]}));
        let observer = observer();
        let first = observer.observe(1, "encoded", &serde_json::to_vec(&first).unwrap());
        let next = observer.observe(2, "encoded", &serde_json::to_vec(&next).unwrap());
        assert_eq!(next["prefix_common_bytes"], first["prefix_view_bytes"]);
        assert_eq!(next["previous_suffix_estimated_tokens"], 0);
        assert_eq!(next["first_changed_segment"], "/input/2");
        assert_ne!(next["raw_common_prefix_bytes"], first["bytes"]);
    }

    #[test]
    fn responses_marker_removal_never_rewrites_tool_data_or_opaque_output() {
        let body = json!({"tools":[{"parameters":{"prompt_cache_breakpoint":"keep schema"}}], "input":[
            {"type":"function_call","arguments":"{\"prompt_cache_breakpoint\":\"keep argument\"}"},
            {"type":"reasoning","summary":[{"prompt_cache_breakpoint":"keep opaque"}]},
            {"type":"message","role":"assistant","content":[{"type":"output_text","prompt_cache_breakpoint":"keep output"}]},
            {"type":"message","role":"user","content":[{"type":"future_input","prompt_cache_breakpoint":"keep unknown"}]}
        ]});
        let (bytes, _) = prefix_view(&body);
        let text = String::from_utf8(bytes).unwrap();
        for content in [
            "keep schema",
            "keep argument",
            "keep opaque",
            "keep output",
            "keep unknown",
        ] {
            assert!(text.contains(content), "{content}");
        }
        assert_eq!(cache_controls(&body)["breakpoint_count"], 0);
    }

    fn observer() -> CacheTrace {
        CacheTrace {
            state: Arc::default(),
            lane: "fixture-lane".into(),
        }
    }

    #[test]
    fn salt_is_client_local_and_diagnostics_never_debug_payloads() {
        let a = observer();
        let b = observer();
        let raw = br#"{"system":"SECRET","messages":[]}"#;
        let first = a.observe(1, "wire_http", raw);
        let second = a.observe(2, "wire_http", raw);
        assert_eq!(first["digest"], second["digest"]);
        assert_ne!(first["digest"], b.observe(1, "wire_http", raw)["digest"]);
        assert_eq!(second["raw_common_prefix_bytes"], raw.len());
        assert!(!format!("{a:?}").contains("SECRET"));
        assert!(!first.to_string().contains("SECRET"));
    }

    #[test]
    fn lane_and_stage_isolation_eviction_and_oversize_gaps() {
        let a = observer();
        let mut b = a.clone();
        b.lane = "other-lane".into();
        let raw = br#"{"messages":[{"content":"stable"}]}"#;
        a.observe(1, "canonical", raw);
        assert!(a.observe(2, "encoded", raw).get("previous_id").is_none());
        assert!(b.observe(3, "canonical", raw).get("previous_id").is_none());
        assert_eq!(a.observe(4, "canonical", raw)["previous_id"], 1);
        assert_eq!(
            a.observe(5, "canonical", &vec![b'x'; MAX_BODY + 1])["comparison_unavailable"],
            "body_exceeds_2_MiB"
        );
        assert!(a.observe(6, "canonical", raw).get("previous_id").is_none());
        a.observe(7, "canonical", b"invalid JSON");
        assert!(a.observe(8, "canonical", raw).get("previous_id").is_none());
        for id in 9..30 {
            let mut lane = a.clone();
            lane.lane = id.to_string();
            lane.observe(id, "wire_http", raw);
        }
        let state = a.state.lock().unwrap();
        assert_eq!(state.snapshots.len(), MAX_SNAPSHOTS);
        assert!(
            state
                .snapshots
                .iter()
                .map(|s| s.raw.len() + s.prefix.len())
                .sum::<usize>()
                <= MAX_RETAINED
        );
    }

    #[test]
    fn retained_byte_budget_evicts_before_the_snapshot_count_limit() {
        let a = observer();
        let raw = serde_json::to_vec(&json!({"system":"x".repeat(1_100_000)})).unwrap();
        for id in 0..10 {
            let mut lane = a.clone();
            lane.lane = id.to_string();
            lane.observe(id, "wire_http", &raw);
        }
        let state = a.state.lock().unwrap();
        assert!(state.snapshots.len() < MAX_SNAPSHOTS);
        assert!(state.snapshots.front().unwrap().id > 0);
        assert!(
            state
                .snapshots
                .iter()
                .map(|s| s.raw.len() + s.prefix.len())
                .sum::<usize>()
                <= MAX_RETAINED
        );
    }

    #[test]
    fn nested_pi_and_mistral_histories_are_not_invisible() {
        for body in [
            json!({"context":{"systemPrompt":"stable","messages":[{"content":"SECRET"}],"tools":[]}}),
            json!({"instructions":"stable","inputs":[{"content":"SECRET"}],"tools":[]}),
        ] {
            let (bytes, segments) = prefix_view(&body);
            assert!(String::from_utf8(bytes).unwrap().contains("SECRET"));
            assert_eq!(segments.len(), 3);
        }
    }

    #[test]
    fn accounting_keeps_writes_fresh_and_1h_a_subset() {
        let line = usage(
            1,
            &Usage {
                input_tokens: 1,
                cache_read_tokens: 98,
                cache_write_tokens: 1,
                cache_write_1h_tokens: 1,
                ..Usage::default()
            },
        );
        assert_eq!(line["total_input_tokens"], 100);
        assert_eq!(line["cache_efficiency"], 0.98);
        assert!(usage(2, &Usage::default())["cache_efficiency"].is_null());
    }
}
