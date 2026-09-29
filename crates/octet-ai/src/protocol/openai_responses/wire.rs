//! The request wire tree: every private `Serialize` DTO the Responses endpoint
//! can be asked to send, the two gates that decide which optional fields a
//! given route is allowed to carry, and the documented computer-action
//! vocabulary the codec is willing to send or accept.
//!
//! This is separate from `request` because these types are the only place the
//! exact shape of an outgoing body is decided. Nothing here reads a response,
//! so a change to this module cannot alter decoding, and a fixture that pins
//! the request bytes needs none of the stream machinery to run.

use serde::Serialize;

use crate::error::{AiError, DecodeError};
use crate::protocol::WireImageUrl;
use crate::types::{CacheRetention, ServiceTier};

#[derive(Serialize)]
pub(super) struct ResponsesRequest {
    pub(super) model: String,
    // Already-owned opaque trees are inserted by into_json, not serialized
    // through the typed metadata DTO into a second tree.
    #[serde(skip_serializing)]
    pub(super) input: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) previous_response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) context_management: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tools: Option<Vec<ResponsesToolWire>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) max_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) service_tier: Option<ServiceTier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reasoning: Option<ResponsesReasoningConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) text: Option<ResponsesTextConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) prompt_cache_retention: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) prompt_cache_options: Option<ResponsesPromptCacheOptions>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) include: Vec<String>,
    pub(super) store: bool,
    // The streaming intent must be in the body, not only the transport. Standard
    // OpenAI Responses needs it to stream, and the ChatGPT Codex backend
    // outright rejects its absence (`{"detail":"Stream must be set to true"}`).
    // This codec is always-streamed (there is no non-streaming Responses decode
    // path — see `decode_stream_event`), so it is unconditionally true.
    pub(super) stream: bool,
}

/// The explicit cache-mode contract is distinct from legacy 24h retention.
#[derive(Serialize)]
pub(super) struct ResponsesPromptCacheOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl: Option<&'static str>,
}

pub(super) fn supports_explicit_prompt_cache_mode(model: &crate::catalog::Model) -> bool {
    let url = &model.endpoint.base_url;
    // A copied model record must not assert this public-API contract for a
    // compatible gateway, Azure, or the subscription route.
    model.spec.cache.supports_explicit_prompt_cache_mode
        && model.endpoint.runtime.responses_profile
            == crate::types::ResponsesRuntimeProfile::Default
        && url.scheme() == "https"
        && url.host_str() == Some("api.openai.com")
        && url.path() == "/v1/"
}

pub(super) fn prompt_cache_options(
    model: &crate::catalog::Model,
    retention: CacheRetention,
) -> Option<ResponsesPromptCacheOptions> {
    if !supports_explicit_prompt_cache_mode(model) {
        return None;
    }
    match retention {
        CacheRetention::None => Some(ResponsesPromptCacheOptions {
            // No explicit breakpoints are emitted by this route, so this
            // disables prompt caching instead of merely omitting affinity.
            mode: Some("explicit"),
            ttl: None,
        }),
        CacheRetention::Long if model.spec.cache.supports_long_retention => {
            Some(ResponsesPromptCacheOptions {
                mode: None,
                ttl: Some("30m"),
            })
        }
        CacheRetention::Short | CacheRetention::WarmShort | CacheRetention::Long => None,
    }
}

impl ResponsesRequest {
    pub(super) fn into_json(self) -> Result<serde_json::Value, AiError> {
        let mut body = serde_json::to_value(&self)
            .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))?;
        body.as_object_mut()
            .expect("the Responses request DTO serializes to an object")
            .insert("input".to_owned(), self.input);
        Ok(body)
    }
}

pub(super) fn into_wire_input(input: crate::responses::ResponsesInput) -> serde_json::Value {
    serde_json::Value::Array(
        input
            .into_items()
            .into_iter()
            .map(crate::responses::ResponsesItem::into_json)
            .collect(),
    )
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ResponsesInputItem {
    AdditionalTools {
        role: String,
        tools: Vec<serde_json::Value>,
    },
    Message {
        role: String,
        content: Vec<ResponsesContentPart>,
    },
    FunctionCall {
        #[serde(rename = "async", skip_serializing_if = "is_false")]
        async_execution: bool,
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: Vec<ResponsesToolResultBlock>,
    },
    /// Authoritative `computer_call` input item, replayed from canonical
    /// history. Pairing uses the same `call_id` as the matching
    /// `computer_call_output`.
    ComputerCall {
        call_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        action: Option<serde_json::Value>,
    },
    ComputerCallOutput {
        call_id: String,
        output: ResponsesComputerScreenshot,
    },
    Reasoning {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        // The Responses API requires `summary` on replayed reasoning items even
        // when the model returned no visible summary (`[]`). Omitting it makes
        // newer Codex models reject the post-tool continuation request.
        summary: Vec<ResponsesReasoningSummary>,
        #[serde(skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<String>,
    },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ResponsesContentPart {
    InputText {
        text: String,
    },
    // Replayed assistant messages are output items, not new user input. Newer
    // Responses/Codex models reject `input_text` under role `assistant`.
    OutputText {
        text: String,
        annotations: Vec<serde_json::Value>,
    },
    InputImage {
        #[serde(skip_serializing_if = "Option::is_none")]
        image_url: Option<WireImageUrl>,
        #[serde(skip_serializing_if = "Option::is_none")]
        file_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ResponsesToolResultBlock {
    InputText {
        text: String,
    },
    InputImage {
        #[serde(skip_serializing_if = "Option::is_none")]
        image_url: Option<WireImageUrl>,
        #[serde(skip_serializing_if = "Option::is_none")]
        file_id: Option<String>,
    },
}

/// The single documented output of a `computer_call_output` item.
///
/// The Responses schema types this as one `computer_screenshot` object, so the
/// codec never forwards arbitrary canonical parts here: only the first usable
/// screenshot source is sent, and an oversized inline image is dropped rather
/// than forwarded unbounded.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ResponsesComputerScreenshot {
    ComputerScreenshot {
        #[serde(skip_serializing_if = "Option::is_none")]
        image_url: Option<WireImageUrl>,
        #[serde(skip_serializing_if = "Option::is_none")]
        file_id: Option<String>,
    },
}

impl ResponsesComputerScreenshot {
    /// Empty screenshot: the wire schema makes both sources optional, so a
    /// caller with no screenshot authority still returns a well-formed pairing.
    fn empty() -> Self {
        Self::ComputerScreenshot {
            image_url: None,
            file_id: None,
        }
    }

    /// First usable screenshot source from canonical tool-result blocks.
    pub(super) fn from_blocks(blocks: &[ResponsesToolResultBlock]) -> Self {
        for block in blocks {
            match block {
                ResponsesToolResultBlock::InputText { .. } => {}
                ResponsesToolResultBlock::InputImage { image_url, file_id } => {
                    if let Some(WireImageUrl::Inline { data, .. }) = image_url {
                        if data.len() > MAX_COMPUTER_SCREENSHOT_BYTES {
                            continue;
                        }
                    }
                    if image_url.is_some() || file_id.is_some() {
                        return Self::ComputerScreenshot {
                            image_url: image_url.clone(),
                            file_id: file_id.clone(),
                        };
                    }
                }
            }
        }
        Self::empty()
    }
}

#[derive(Serialize)]
pub(super) struct ResponsesReasoningSummary {
    pub(super) r#type: String,
    pub(super) text: String,
}

#[derive(Serialize)]
#[serde(untagged)]
pub(super) enum ResponsesToolWire {
    Function(ResponsesTool),
    Custom(ResponsesCustomTool),
    Computer(ResponsesComputerTool),
}

/// OpenAI Responses `computer_use_preview` built-in tool declaration.
///
/// The declaration carries no programmable schema: the provider's model answers
/// with `computer_call` items carrying an `action`, which this codec maps to a
/// canonical tool call named [`COMPUTER_TOOL_NAME`].
#[derive(Serialize)]
pub(super) struct ResponsesComputerTool {
    pub(super) r#type: &'static str,
    pub(super) display_width: u32,
    pub(super) display_height: u32,
    pub(super) environment: &'static str,
}

/// Canonical tool name assigned to a provider `computer_call` item.
///
/// A `computer_call` has no function name on the wire, so the codec synthesizes
/// this stable name for the canonical tool call and recognizes it again when
/// replaying canonical history. It is the documented wire tool type, not a
/// provider identity.
pub(crate) const COMPUTER_TOOL_NAME: &str = "computer_use_preview";

/// Documented OpenAI computer action discriminators.
///
/// The codec refuses every other action type (including a missing one) instead
/// of handing an unvetted action to a caller: computer-use authority lives
/// outside this crate, and an unknown action cannot be represented safely.
pub(super) const COMPUTER_ACTION_TYPES: &[&str] = &[
    "click",
    "double_click",
    "drag",
    "keypress",
    "move",
    "screenshot",
    "scroll",
    "type",
    "wait",
];

/// Bounded size of the canonical argument payload built from a computer action.
pub(super) const MAX_COMPUTER_ACTION_BYTES: usize = 16 * 1024;

/// Bounded size of one inline screenshot replayed in a `computer_call_output`.
pub(super) const MAX_COMPUTER_SCREENSHOT_BYTES: usize = 4 * 1024 * 1024;

pub(super) fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Serialize)]
pub(super) struct ResponsesTool {
    #[serde(rename = "async", skip_serializing_if = "is_false")]
    pub(super) async_execution: bool,
    pub(super) r#type: &'static str,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) parameters: serde_json::Value,
    /// `true` only when the caller asked for strict JSON-schema sampling and
    /// the route could enforce the rewritten schema. The field is omitted
    /// entirely when the route cannot enforce strict tools at all, mirroring
    /// Pi's `convertResponsesTools` (`if (supportsStrictMode) functionTool.strict = strict`);
    /// a route that rejects unknown fields must not receive a misleading
    /// `strict: false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) strict: Option<bool>,
}

/// OpenAI Responses `custom` tool constrained by a Lark/regex grammar.
#[derive(Serialize)]
pub(super) struct ResponsesCustomTool {
    #[serde(rename = "async", skip_serializing_if = "is_false")]
    pub(super) async_execution: bool,
    pub(super) r#type: &'static str,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) format: ResponsesGrammarFormat,
}

#[derive(Serialize)]
pub(super) struct ResponsesGrammarFormat {
    pub(super) r#type: &'static str,
    pub(super) syntax: String,
    pub(super) definition: String,
}

#[derive(Serialize)]
pub(super) struct ResponsesReasoningConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) context: Option<&'static str>,
    // Request visible summary deltas in addition to encrypted continuation
    // state. Without this, reasoning-capable Codex models think silently.
    pub(super) summary: &'static str,
}

#[derive(Serialize)]
pub(super) struct ResponsesTextConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) format: Option<ResponsesFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) verbosity: Option<&'static str>,
}

// Only non-default output formats produce a wire `text.format`. The private
// Codex route still emits `text` for its low-verbosity latency default.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ResponsesFormat {
    JsonObject,
    JsonSchema {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        schema: serde_json::Value,
        strict: bool,
    },
}

pub(super) fn opaque_input_item(item: ResponsesInputItem) -> crate::responses::ResponsesItem {
    crate::responses::ResponsesItem::new(
        serde_json::to_value(item).expect("private Responses input item serializes to an object"),
    )
    .expect("private Responses input item is always an object")
}
