//! Redacted provider-request trace.
//!
//! Off unless `OCTET_PROVIDER_REQUEST_TRACE` is set to `stderr` or to the path
//! of a file; the value is read from the request's environment overrides first
//! and then from the process. Each request appends one JSON line describing
//! only the *shape* of what was sent (item types and counts, image counts and
//! bytes, whether reasoning carried `encrypted_content`, the transport and
//! whether `previous_response_id` was set) and, when the provider answers with
//! an error status, one line with that status and request id.
//!
//! Nothing here records a prompt, tool argument, tool output, header, URL,
//! credential, response id, image data or reasoning content: it exists to make
//! a provider rejection such as an opaque HTTP 400 diagnosable without
//! publishing a raw session.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Map, Value};

/// Environment variable naming the trace sink (`stderr` or a file path).
pub(crate) const REQUEST_TRACE_ENV: &str = "OCTET_PROVIDER_REQUEST_TRACE";

static NEXT_TRACE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
enum Sink {
    Stderr,
    File(PathBuf),
}

/// One traced request; correlates its response line with its request line.
#[derive(Clone, Debug)]
pub(crate) struct RequestTrace {
    sink: Sink,
    id: u64,
}

impl RequestTrace {
    /// Returns a trace when the environment enables one.
    pub(crate) fn from_env(overrides: &BTreeMap<String, String>) -> Option<Self> {
        let configured = overrides
            .get(REQUEST_TRACE_ENV)
            .cloned()
            .or_else(|| std::env::var(REQUEST_TRACE_ENV).ok())?;
        let configured = configured.trim();
        let sink = match configured {
            "" => return None,
            "stderr" => Sink::Stderr,
            path => Sink::File(PathBuf::from(path)),
        };
        Some(Self {
            sink,
            id: NEXT_TRACE_ID.fetch_add(1, Ordering::Relaxed),
        })
    }

    /// Records the shape of a request body about to be sent.
    pub(crate) fn request(&self, transport: &'static str, api_model: &str, body: &Value) {
        let mut line = Map::new();
        line.insert("event".into(), json!("request"));
        line.insert("id".into(), json!(self.id));
        line.insert("ts_ms".into(), json!(now_ms()));
        line.insert("transport".into(), json!(transport));
        line.insert("model".into(), json!(api_model));
        if let Some(frame_type) = body.get("type").and_then(Value::as_str) {
            line.insert("frame_type".into(), json!(frame_type));
        }
        line.insert("body_bytes".into(), json!(body.to_string().len()));
        line.extend(request_shape(body));
        self.emit(Value::Object(line));
    }

    /// Records a request body that could not be parsed as JSON.
    pub(crate) fn request_bytes(
        &self,
        transport: &'static str,
        api_model: &str,
        body_bytes: usize,
    ) {
        self.emit(json!({
            "event": "request",
            "id": self.id,
            "ts_ms": now_ms(),
            "transport": transport,
            "model": api_model,
            "body_bytes": body_bytes,
            "unparsed": true,
        }));
    }

    /// Records a non-success provider response.
    pub(crate) fn error_response(&self, status: u16, provider_request_id: Option<&str>) {
        self.emit(json!({
            "event": "response_error",
            "id": self.id,
            "ts_ms": now_ms(),
            "status": status,
            "provider_request_id": provider_request_id.map(sanitize_id),
        }));
    }

    fn emit(&self, line: Value) {
        let mut text = line.to_string();
        text.push('\n');
        match &self.sink {
            Sink::Stderr => {
                let _ = std::io::stderr().write_all(text.as_bytes());
            }
            Sink::File(path) => {
                if let Ok(mut file) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                {
                    let _ = file.write_all(text.as_bytes());
                }
            }
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Provider request ids are opaque tokens; keep only a bounded safe alphabet.
fn sanitize_id(id: &str) -> String {
    id.chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        .take(96)
        .collect()
}

/// Shape of a provider request body: array lengths for every top-level array,
/// plus the detailed Responses breakdown when the body has an `input` array.
pub(crate) fn request_shape(body: &Value) -> Map<String, Value> {
    let mut shape = Map::new();
    let Some(object) = body.as_object() else {
        return shape;
    };
    let arrays: Map<String, Value> = object
        .iter()
        .filter_map(|(key, value)| Some((key.clone(), json!(value.as_array()?.len()))))
        .collect();
    shape.insert("array_lengths".into(), Value::Object(arrays));
    shape.insert(
        "previous_response_id".into(),
        json!(object
            .get("previous_response_id")
            .is_some_and(|value| !value.is_null())),
    );
    if let Some(store) = object.get("store").and_then(Value::as_bool) {
        shape.insert("store".into(), json!(store));
    }
    if let Some(items) = object.get("input").and_then(Value::as_array) {
        shape.extend(responses_input_shape(items));
    }
    shape
}

#[derive(Default)]
struct ImageTally {
    count: u64,
    inline: u64,
    url: u64,
    file_id: u64,
    inline_wire_bytes: u64,
    largest_inline_wire_bytes: u64,
}

impl ImageTally {
    fn add_image_url(&mut self, value: &Value) {
        let url = match value {
            Value::String(url) => Some(url.as_str()),
            Value::Object(object) => object.get("url").and_then(Value::as_str),
            _ => None,
        };
        let Some(url) = url else {
            return;
        };
        self.count += 1;
        if url.starts_with("data:") {
            self.inline += 1;
            let bytes = url.len() as u64;
            self.inline_wire_bytes += bytes;
            self.largest_inline_wire_bytes = self.largest_inline_wire_bytes.max(bytes);
        } else {
            self.url += 1;
        }
    }

    fn add_file_id(&mut self) {
        self.count += 1;
        self.file_id += 1;
    }

    fn add_block(&mut self, block: &Value) {
        if block.get("type").and_then(Value::as_str) != Some("input_image") {
            return;
        }
        if let Some(url) = block.get("image_url") {
            self.add_image_url(url);
        } else if block.get("file_id").is_some_and(|id| !id.is_null()) {
            self.add_file_id();
        }
    }
}

fn responses_input_shape(items: &[Value]) -> Map<String, Value> {
    let mut types: BTreeMap<String, u64> = BTreeMap::new();
    let mut images = ImageTally::default();
    let mut reasoning_items = 0u64;
    let mut reasoning_encrypted = 0u64;
    let mut reasoning_encrypted_chars = 0u64;
    let mut screenshots_without_image = 0u64;
    for item in items {
        let kind = item.get("type").and_then(Value::as_str).unwrap_or_else(|| {
            if item.get("role").is_some() {
                "message"
            } else {
                "unknown"
            }
        });
        *types.entry(kind.to_owned()).or_default() += 1;
        match kind {
            "message" => {
                if let Some(parts) = item.get("content").and_then(Value::as_array) {
                    parts.iter().for_each(|part| images.add_block(part));
                }
            }
            "function_call_output" | "custom_tool_call_output" => {
                if let Some(blocks) = item.get("output").and_then(Value::as_array) {
                    blocks.iter().for_each(|block| images.add_block(block));
                }
            }
            "computer_call_output" => {
                let output = item.get("output");
                let before = images.count;
                if let Some(url) = output.and_then(|output| output.get("image_url")) {
                    images.add_image_url(url);
                } else if output
                    .and_then(|output| output.get("file_id"))
                    .is_some_and(|id| !id.is_null())
                {
                    images.add_file_id();
                }
                if images.count == before {
                    screenshots_without_image += 1;
                }
            }
            "reasoning" => {
                reasoning_items += 1;
                if let Some(encrypted) = item.get("encrypted_content").and_then(Value::as_str) {
                    reasoning_encrypted += 1;
                    reasoning_encrypted_chars += encrypted.len() as u64;
                }
            }
            _ => {}
        }
    }
    let trailing = items
        .last()
        .and_then(|item| item.get("type").and_then(Value::as_str))
        .unwrap_or("none");
    let mut shape = Map::new();
    shape.insert("input_items".into(), json!(items.len()));
    shape.insert("item_types".into(), json!(types));
    shape.insert("trailing_item_type".into(), json!(trailing));
    shape.insert(
        "images".into(),
        json!({
            "count": images.count,
            "inline": images.inline,
            "url": images.url,
            "file_id": images.file_id,
            "inline_wire_bytes": images.inline_wire_bytes,
            "largest_inline_wire_bytes": images.largest_inline_wire_bytes,
        }),
    );
    shape.insert(
        "reasoning".into(),
        json!({
            "items": reasoning_items,
            "with_encrypted_content": reasoning_encrypted,
            "without_encrypted_content": reasoning_items - reasoning_encrypted,
            "encrypted_content_chars": reasoning_encrypted_chars,
        }),
    );
    shape.insert(
        "computer_call_outputs_without_image".into(),
        json!(screenshots_without_image),
    );
    shape
}

#[cfg(test)]
mod tests;
