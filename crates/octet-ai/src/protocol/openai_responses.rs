//! OpenAI Responses private wire protocol codec.

use serde::{Deserialize, Serialize};

use crate::error::{AiError, ConfigError, DecodeError, ProviderError};
use crate::protocol::sse::SseEvent;
use crate::protocol::{
    cache_session_id, cache_session_id_for, emit_event, get_canonical_index, prompt_cache_key,
    prompt_cache_key_for, HttpRequestParts, WireImageUrl,
};
use crate::stream::{ResponseBuilder, StreamEvent};
use crate::types::{
    AssistantPart, CacheRetention, ImageSource, Media, Message, OutputFormat, Protocol,
    ReasoningConfig, ReasoningMode, ReasoningState, ReasoningStateKind, Request, ServiceTier,
    StopReason, ToolCallId, ToolChoice, ToolDef, ToolResultPart, Usage, UserPart,
};
use crate::validate::{
    normalize_request_reasoning, validate_reasoning_selection, validate_request,
};

// --- Private OpenAI Responses Request DTOs ---

#[derive(Serialize)]
struct ResponsesRequest {
    model: String,
    // Already-owned opaque trees are inserted by into_json, not serialized
    // through the typed metadata DTO into a second tree.
    #[serde(skip_serializing)]
    input: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_management: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ResponsesToolWire>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service_tier: Option<ServiceTier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ResponsesReasoningConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<ResponsesTextConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_retention: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_options: Option<ResponsesPromptCacheOptions>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    include: Vec<String>,
    store: bool,
    // The streaming intent must be in the body, not only the transport. Standard
    // OpenAI Responses needs it to stream, and the ChatGPT Codex backend
    // outright rejects its absence (`{"detail":"Stream must be set to true"}`).
    // This codec is always-streamed (there is no non-streaming Responses decode
    // path — see `decode_stream_event`), so it is unconditionally true.
    stream: bool,
}

/// The explicit cache-mode contract is distinct from legacy 24h retention.
#[derive(Serialize)]
struct ResponsesPromptCacheOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl: Option<&'static str>,
}

fn supports_explicit_prompt_cache_mode(model: &crate::catalog::Model) -> bool {
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

fn prompt_cache_options(
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
    fn into_json(self) -> Result<serde_json::Value, AiError> {
        let mut body = serde_json::to_value(&self)
            .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))?;
        body.as_object_mut()
            .expect("the Responses request DTO serializes to an object")
            .insert("input".to_owned(), self.input);
        Ok(body)
    }
}

fn into_wire_input(input: crate::responses::ResponsesInput) -> serde_json::Value {
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
enum ResponsesInputItem {
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
enum ResponsesContentPart {
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
enum ResponsesToolResultBlock {
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
enum ResponsesComputerScreenshot {
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
    fn from_blocks(blocks: &[ResponsesToolResultBlock]) -> Self {
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
struct ResponsesReasoningSummary {
    r#type: String,
    text: String,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ResponsesToolWire {
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
struct ResponsesComputerTool {
    r#type: &'static str,
    display_width: u32,
    display_height: u32,
    environment: &'static str,
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
const COMPUTER_ACTION_TYPES: &[&str] = &[
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
const MAX_COMPUTER_ACTION_BYTES: usize = 16 * 1024;

/// Bounded size of one inline screenshot replayed in a `computer_call_output`.
const MAX_COMPUTER_SCREENSHOT_BYTES: usize = 4 * 1024 * 1024;

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Serialize)]
struct ResponsesTool {
    #[serde(rename = "async", skip_serializing_if = "is_false")]
    async_execution: bool,
    r#type: &'static str,
    name: String,
    description: String,
    parameters: serde_json::Value,
    /// `true` only when the caller asked for strict JSON-schema sampling and
    /// the route could enforce the rewritten schema. The field is omitted
    /// entirely when the route cannot enforce strict tools at all, mirroring
    /// Pi's `convertResponsesTools` (`if (supportsStrictMode) functionTool.strict = strict`);
    /// a route that rejects unknown fields must not receive a misleading
    /// `strict: false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    strict: Option<bool>,
}

/// OpenAI Responses `custom` tool constrained by a Lark/regex grammar.
#[derive(Serialize)]
struct ResponsesCustomTool {
    #[serde(rename = "async", skip_serializing_if = "is_false")]
    async_execution: bool,
    r#type: &'static str,
    name: String,
    description: String,
    format: ResponsesGrammarFormat,
}

#[derive(Serialize)]
struct ResponsesGrammarFormat {
    r#type: &'static str,
    syntax: String,
    definition: String,
}

#[derive(Serialize)]
struct ResponsesReasoningConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<&'static str>,
    // Request visible summary deltas in addition to encrypted continuation
    // state. Without this, reasoning-capable Codex models think silently.
    summary: &'static str,
}

#[derive(Serialize)]
struct ResponsesTextConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<ResponsesFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verbosity: Option<&'static str>,
}

// Only non-default output formats produce a wire `text.format`. The private
// Codex route still emits `text` for its low-verbosity latency default.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponsesFormat {
    JsonObject,
    JsonSchema {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        schema: serde_json::Value,
        strict: bool,
    },
}

// --- Request Builder ---

fn opaque_input_item(item: ResponsesInputItem) -> crate::responses::ResponsesItem {
    crate::responses::ResponsesItem::new(
        serde_json::to_value(item).expect("private Responses input item serializes to an object"),
    )
    .expect("private Responses input item is always an object")
}

fn validate_terminal_async_markers(
    builder: &ResponseBuilder,
    output: &[crate::ResponsesItem],
) -> Result<(), AiError> {
    crate::responses::validate_provider_output_items(output)?;
    for item in output {
        let item = item.as_json();
        let Some(marker) = item.get("async") else {
            continue;
        };
        let marker = marker
            .as_bool()
            .ok_or_else(|| DecodeError::InvalidProviderField("invalid async call marker".into()))?;
        let call_id = item.get("call_id").and_then(serde_json::Value::as_str);
        let call = builder
            .tool_call_builders
            .values()
            .find(|call| Some(call.id.0.as_str()) == call_id);
        if call.is_some_and(|call| call.async_execution != marker) || (marker && call.is_none()) {
            return Err(DecodeError::InvalidProviderField(
                "terminal async call marker disagrees with call start".into(),
            )
            .into());
        }
    }
    Ok(())
}

fn validate_async_tools(model: &crate::Model, tools: &[ToolDef]) -> Result<(), AiError> {
    if tools.iter().any(|tool| tool.async_execution) && !model.responses_features().async_tools {
        return Err(
            ConfigError::Parse("async tools are not qualified for this route".into()).into(),
        );
    }
    Ok(())
}

fn validate_async_input(
    model: &crate::Model,
    input: &crate::ResponsesInput,
    tools: &[ToolDef],
) -> Result<(), AiError> {
    validate_async_tools(model, tools)?;
    // Only the ordered walk can accept these prospective historical pairs.
    // This index does not waive duplicate IDs or output-before-call checks.
    let historical_results: std::collections::HashSet<&str> = input
        .items()
        .iter()
        .filter_map(|item| {
            let item = item.as_json();
            match item.get("type").and_then(serde_json::Value::as_str) {
                Some("function_call_output" | "custom_tool_call_output") => {
                    item.get("call_id").and_then(serde_json::Value::as_str)
                }
                _ => None,
            }
        })
        .collect();
    let mut call_ids = std::collections::HashSet::new();
    let mut invalid_pending = std::collections::HashSet::new();
    let mut completed = std::collections::HashSet::new();
    for item in input.items() {
        let item = item.as_json();
        let kind = item.get("type").and_then(serde_json::Value::as_str);
        // Delta continuation may legitimately carry outputs for calls in the
        // server's retained prefix. Still record every visible result, so later
        // calls cannot reuse that identity or hide an output-before-call pair.
        let id = item.get("call_id").and_then(serde_json::Value::as_str);
        if matches!(
            kind,
            Some("function_call" | "custom_tool_call" | "computer_call")
        ) {
            if let Some(id) = id {
                if !call_ids.insert(id) || completed.contains(id) {
                    return Err(ConfigError::Parse(
                        "duplicate or out-of-order tool call ID in Responses input".into(),
                    )
                    .into());
                }
            }
        } else if matches!(
            kind,
            Some("function_call_output" | "custom_tool_call_output" | "computer_call_output")
        ) {
            if let Some(id) = id {
                if !completed.insert(id) {
                    return Err(ConfigError::Parse(
                        "duplicate tool result in Responses input".into(),
                    )
                    .into());
                }
                invalid_pending.remove(id);
            }
        }
        let Some(marker) = item.get("async") else {
            continue;
        };
        let enabled = marker
            .as_bool()
            .ok_or_else(|| ConfigError::Parse("invalid async call marker".into()))?;
        if !enabled {
            continue;
        }
        let name = item
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if !model.responses_features().async_tools
            || !matches!(kind, Some("function_call" | "custom_tool_call"))
            || name.is_empty()
        {
            return Err(
                ConfigError::Parse("unadvertised async call in Responses input".into()).into(),
            );
        }
        let id = item
            .get("call_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| ConfigError::Parse("async call requires a call_id".into()))?;
        if historical_results.contains(id) {
            // Retain envelope validation, but do not use a later tool schema or
            // advertisement as authority over already completed provider work.
            if kind == Some("custom_tool_call") {
                item.get("input")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        ConfigError::Parse("async custom call requires string input".into())
                    })?;
            } else {
                let arguments = item
                    .get("arguments")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        ConfigError::Parse("async function call requires arguments".into())
                    })?;
                crate::json_repair::normalize_json_object_value(arguments)?;
            }
            continue;
        }
        if !tools
            .iter()
            .any(|tool| tool.async_execution && tool.name == name)
        {
            return Err(ConfigError::Parse(
                "pending async call was not advertised for this tool".into(),
            )
            .into());
        }
        let arguments = if kind == Some("custom_tool_call") {
            let property =
                super::grammar::input_property(tools, name, super::grammar_tools_for(model))?
                    .ok_or_else(|| {
                        ConfigError::Parse("async custom call requires its declared grammar".into())
                    })?;
            let text = item
                .get("input")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    ConfigError::Parse("async custom call requires string input".into())
                })?;
            serde_json::json!({property: text})
        } else {
            let arguments = item
                .get("arguments")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    ConfigError::Parse("async function call requires arguments".into())
                })?;
            crate::json_repair::normalize_json_object_value(arguments)?
        };
        if !matches!(
            crate::json_repair::validate_tool_arguments(name, &arguments, tools)?,
            crate::ToolArgumentValidation::Valid
        ) {
            invalid_pending.insert(id);
        }
    }
    if let Some(id) = invalid_pending.into_iter().next() {
        return Err(crate::ValidationError::MissingToolResult(ToolCallId(id.to_owned())).into());
    }
    Ok(())
}

fn map_responses_tools(
    model: &crate::catalog::Model,
    tools: &[ToolDef],
) -> Result<Option<Vec<ResponsesToolWire>>, AiError> {
    if tools.is_empty() || !model.spec.capabilities.tools {
        return Ok(None);
    }
    let mut mapped = Vec::with_capacity(tools.len());
    for tool in tools {
        // Grammar-constrained tools are caller-opted OpenAI `custom` tools;
        // every other tool is a strict-resolved function tool.
        if let Some(grammar) =
            crate::constrained_sampling::resolve_grammar(tool, super::grammar_tools_for(model))?
        {
            mapped.push(ResponsesToolWire::Custom(ResponsesCustomTool {
                async_execution: tool.async_execution,
                r#type: "custom",
                name: tool.name.clone(),
                description: tool.description.clone(),
                format: ResponsesGrammarFormat {
                    r#type: "grammar",
                    syntax: grammar.format.to_owned(),
                    definition: grammar.definition,
                },
            }));
            continue;
        }
        let supports_strict = super::strict_mode_for(model);
        let (parameters, strict) =
            crate::constrained_sampling::function_tool_parameters(tool, supports_strict)?;
        mapped.push(ResponsesToolWire::Function(ResponsesTool {
            async_execution: tool.async_execution,
            r#type: "function",
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters,
            strict: supports_strict.then_some(strict),
        }));
    }
    Ok(Some(mapped))
}

fn map_responses_lite_tools(
    model: &crate::catalog::Model,
    tools: &[ToolDef],
) -> Result<Vec<serde_json::Value>, AiError> {
    if tools.is_empty() || !model.spec.capabilities.tools {
        return Ok(Vec::new());
    }
    let tools = tools
        .iter()
        .map(|tool| {
            let (parameters, strict) =
                crate::constrained_sampling::function_tool_parameters(tool, false)?;
            let mut value = serde_json::json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "strict": strict,
                "parameters": parameters,
            });
            if tool.async_execution {
                value["async"] = true.into();
            }
            Ok(value)
        })
        .collect::<Result<Vec<_>, AiError>>()?;
    Ok(vec![serde_json::json!({
        "type": "namespace",
        "name": "functions",
        "description": "",
        "tools": tools,
    })])
}

fn responses_lite_prefix(
    model: &crate::catalog::Model,
    instructions: Option<&str>,
    tools: &[ToolDef],
) -> Result<Vec<crate::responses::ResponsesItem>, AiError> {
    let mut prefix = vec![opaque_input_item(ResponsesInputItem::AdditionalTools {
        role: "developer".to_owned(),
        tools: map_responses_lite_tools(model, tools)?,
    })];
    if let Some(instructions) = instructions.filter(|instructions| !instructions.is_empty()) {
        prefix.push(opaque_input_item(ResponsesInputItem::Message {
            role: "developer".to_owned(),
            content: vec![ResponsesContentPart::InputText {
                text: instructions.to_owned(),
            }],
        }));
    }
    Ok(prefix)
}

fn responses_reasoning_effort(effort: crate::types::ReasoningEffort) -> &'static str {
    use crate::types::ReasoningEffort;

    match effort {
        ReasoningEffort::Minimal => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Xhigh => "xhigh",
        ReasoningEffort::Max => "max",
        // Ultra is a host-orchestration tier; current Codex wire requests use
        // maximum model reasoning while the V2 runtime supplies delegation.
        ReasoningEffort::Ultra => "max",
    }
}

fn map_responses_reasoning(
    model: &crate::catalog::Model,
    reasoning: &ReasoningConfig,
    _reasoning_mode: ReasoningMode,
) -> Option<ResponsesReasoningConfig> {
    let cap = model.spec.capabilities.reasoning.as_ref()?;
    let effort = if *reasoning == ReasoningConfig::Effort(crate::types::ReasoningEffort::Ultra)
        && model.spec.capabilities.agent_delegation == Some(crate::types::AgentDelegation::V2)
    {
        // Ultra's exact advertised choice still requires the existing V2 gate;
        // the model half of that orchestration contract remains max.
        Some(responses_reasoning_effort(crate::types::ReasoningEffort::Ultra).to_owned())
    } else {
        cap.wire_value(reasoning).filter(|value| value != "default")
    };
    let context = model
        .spec
        .capabilities
        .responses_lite
        .then_some("all_turns");
    (effort.is_some() || context.is_some()).then_some(ResponsesReasoningConfig {
        effort,
        context,
        summary: "auto",
    })
}

fn map_responses_text(
    model: &crate::catalog::Model,
    output_format: &OutputFormat,
) -> Option<ResponsesTextConfig> {
    let text_format = match output_format {
        OutputFormat::Text => None,
        _ if !model.spec.capabilities.structured_output => None,
        OutputFormat::JsonObject => Some(ResponsesFormat::JsonObject),
        OutputFormat::JsonSchema(schema) => Some(ResponsesFormat::JsonSchema {
            name: schema.name.clone(),
            description: schema.description.clone(),
            schema: schema.schema.clone(),
            strict: schema.strict,
        }),
    };
    let verbosity = model
        .endpoint
        .runtime
        .responses_profile
        .uses_low_verbosity()
        .then_some("low");
    (text_format.is_some() || verbosity.is_some()).then_some(ResponsesTextConfig {
        format: text_format,
        verbosity,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_compact_request(
    model: &crate::catalog::Model,
    mut input: crate::responses::ResponsesInput,
    instructions: Option<String>,
    tools: &[ToolDef],
    reasoning: &ReasoningConfig,
    reasoning_mode: ReasoningMode,
    output_format: &OutputFormat,
    cache_retention: CacheRetention,
    session_id: Option<&str>,
) -> Result<crate::responses::ResponsesCompactRequest, AiError> {
    crate::catalog::validate_model_spec(&model.spec)?;
    if model.spec.protocol != Protocol::OpenAiResponses {
        return Err(crate::error::UnsupportedError::ResponsesOptions.into());
    }
    if reasoning_mode == ReasoningMode::Pro {
        return Err(crate::error::UnsupportedError::ReasoningMode.into());
    }
    validate_reasoning_selection(reasoning, &model.spec.capabilities, model.spec.protocol)?;
    crate::responses::validate_responses_input(model, &input, reasoning, true)?;
    validate_async_tools(model, tools)?;
    // The private ChatGPT Codex compact route accepts the same active tool and
    // generation controls as normal Responses calls. Public OpenAI compact
    // currently exposes a narrower schema and may reject these extra fields.
    let responses_lite = model.spec.capabilities.responses_lite;
    let rich_codex_schema = model
        .endpoint
        .runtime
        .responses_profile
        .supports_rich_compact_schema()
        || model.spec.cache.session_affinity_format
            == Some(crate::types::SessionAffinityFormat::Codex)
        || responses_lite;
    let mapped_tools = if responses_lite || !rich_codex_schema {
        None
    } else {
        map_responses_tools(model, tools)?.map(|tools| {
            tools
                .into_iter()
                .map(|tool| serde_json::to_value(tool).expect("Responses tool serializes"))
                .collect()
        })
    };
    let parallel_tool_calls = if responses_lite {
        // The internal Responses Lite route requires an explicit false even
        // when the model otherwise advertises parallel tool-call support.
        Some(false)
    } else {
        mapped_tools
            .as_ref()
            .map(|_| model.spec.capabilities.parallel_tool_calls)
    };
    let (input, instructions) = if responses_lite {
        input.strip_image_details_for_responses_lite();
        let mut items = responses_lite_prefix(model, instructions.as_deref(), tools)?;
        items.extend(input.into_items());
        (crate::responses::ResponsesInput::new(items), None)
    } else {
        (input, instructions)
    };
    let reasoning = rich_codex_schema
        .then(|| map_responses_reasoning(model, reasoning, reasoning_mode))
        .flatten()
        .map(|config| serde_json::to_value(config).expect("Responses reasoning serializes"));
    let text = rich_codex_schema
        .then(|| map_responses_text(model, output_format))
        .flatten()
        .map(|config| serde_json::to_value(config).expect("Responses text serializes"));
    Ok(crate::responses::ResponsesCompactRequest {
        model: model.spec.api_name.clone(),
        input,
        instructions,
        parallel_tool_calls,
        tools: mapped_tools,
        reasoning,
        text,
        prompt_cache_key: prompt_cache_key_for(cache_retention, session_id),
        session_id: cache_session_id_for(cache_retention, session_id).map(str::to_owned),
    })
}

/// Validate the public raw compact DTO before credentials or transport. An
/// omitted control is provider-default intent, not an explicit Off request.
pub(crate) fn validate_compact_reasoning(
    model: &crate::catalog::Model,
    reasoning: Option<&serde_json::Value>,
) -> Result<(), AiError> {
    let Some(reasoning) = reasoning else {
        return Ok(());
    };
    let unsupported = || AiError::Unsupported(crate::error::UnsupportedError::Reasoning);
    let object = reasoning.as_object().ok_or_else(unsupported)?;
    if object.contains_key("mode") {
        return Err(crate::error::UnsupportedError::ReasoningMode.into());
    }
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "effort" | "summary" | "context"))
    {
        return Err(unsupported());
    }
    let cap = model
        .spec
        .capabilities
        .reasoning
        .as_ref()
        .ok_or_else(unsupported)?;
    if let Some(effort) = object.get("effort") {
        let effort = effort.as_str().ok_or_else(unsupported)?;
        // Compare actual wire spellings, including the V2-only Ultra -> max
        // mapping. Raw `ultra` is never a substitute for that typed gate.
        let supported = cap.choices().iter().any(|choice| {
            validate_reasoning_selection(choice, &model.spec.capabilities, model.spec.protocol)
                .is_ok()
                && map_responses_reasoning(model, choice, ReasoningMode::Standard)
                    .and_then(|r| r.effort)
                    .as_deref()
                    == Some(effort)
        });
        if !supported {
            return Err(unsupported());
        }
    }
    if object
        .get("summary")
        .is_some_and(|value| !matches!(value.as_str(), Some("auto" | "concise" | "detailed")))
    {
        return Err(unsupported());
    }
    if object.get("context").is_some_and(|value| {
        !model.spec.capabilities.responses_lite || value.as_str() != Some("all_turns")
    }) {
        return Err(unsupported());
    }
    Ok(())
}

pub(crate) fn responses_affinity_headers(
    model: &crate::catalog::Model,
    session_id: Option<&str>,
) -> Result<http::HeaderMap, AiError> {
    let mut headers = http::HeaderMap::new();
    if model.spec.capabilities.responses_lite {
        headers.insert(
            http::HeaderName::from_static("x-openai-internal-codex-responses-lite"),
            http::HeaderValue::from_static("true"),
        );
    }
    let Some(session_id) = session_id.filter(|id| !id.is_empty()) else {
        return Ok(headers);
    };
    let value = http::HeaderValue::from_str(session_id)
        .map_err(|_| ConfigError::InvalidHeader("session affinity".into()))?;
    match model.spec.cache.session_affinity_format {
        Some(crate::types::SessionAffinityFormat::OpenRouter) => {
            headers.insert(http::HeaderName::from_static("x-session-id"), value);
        }
        // Mistral is a Chat route; accept the configuration here without
        // applying a Chat-only header to a Responses request.
        Some(crate::types::SessionAffinityFormat::Mistral) => {}
        Some(crate::types::SessionAffinityFormat::Codex) => {
            headers.insert(http::HeaderName::from_static("session-id"), value.clone());
            headers.insert(http::HeaderName::from_static("x-client-request-id"), value);
        }
        Some(crate::types::SessionAffinityFormat::OpenAiNoSession) => {
            headers.insert(
                http::HeaderName::from_static("x-client-request-id"),
                value.clone(),
            );
            headers.insert(http::HeaderName::from_static("x-session-affinity"), value);
        }
        Some(crate::types::SessionAffinityFormat::OpenAi) => {
            headers.insert(
                http::HeaderName::from_static("x-client-request-id"),
                value.clone(),
            );
            headers.insert(
                http::HeaderName::from_static("x-session-affinity"),
                value.clone(),
            );
            if model.spec.cache.send_session_id_header {
                headers.insert(http::HeaderName::from_static("session_id"), value);
            }
        }
        None => {
            headers.insert(
                http::HeaderName::from_static("x-client-request-id"),
                value.clone(),
            );
            if model.spec.cache.send_session_id_header {
                headers.insert(http::HeaderName::from_static("session_id"), value);
            }
        }
    }
    Ok(headers)
}

fn map_system_input(
    model: &crate::catalog::Model,
    system: Option<&str>,
) -> Vec<ResponsesInputItem> {
    let Some(system) = system else {
        return Vec::new();
    };
    let role = if model.spec.capabilities.reasoning.is_some() {
        "developer"
    } else {
        "system"
    };
    vec![ResponsesInputItem::Message {
        role: role.to_owned(),
        content: vec![ResponsesContentPart::InputText {
            text: system.to_owned(),
        }],
    }]
}

fn map_user_input(
    model: &crate::catalog::Model,
    user: &crate::types::UserMessage,
    preserve_tool_call_ids: bool,
    pending_tool_calls: &mut std::collections::BTreeSet<String>,
    synthetic_tool_results: &std::collections::HashSet<String>,
    computer_call_ids: &std::collections::BTreeSet<String>,
) -> Vec<ResponsesInputItem> {
    let mut input = Vec::new();
    let mut content = Vec::new();
    for part in &user.content {
        match part {
            UserPart::Text(text) => {
                content.push(ResponsesContentPart::InputText { text: text.clone() });
            }
            UserPart::Media(Media::Image(image)) => {
                if !model
                    .spec
                    .capabilities
                    .input_modalities
                    .contains(crate::types::Modality::Image)
                {
                    continue;
                }

                let (image_url, file_id) = match &image.source {
                    ImageSource::Url(url) => (Some(WireImageUrl::Url(url.to_string())), None),
                    ImageSource::Inline(bytes) => {
                        // No documented default MIME; do not guess a wire field
                        // (design §75). Validation already diagnosed the drop.
                        let Some(media_type) = image.media_type.as_ref() else {
                            continue;
                        };
                        (
                            Some(WireImageUrl::Inline {
                                media_type: media_type.to_string(),
                                data: bytes.clone(),
                            }),
                            None,
                        )
                    }
                    ImageSource::ProviderRef(reference) => {
                        // An expired or wrong-protocol provider ref is dropped
                        // (validation already emitted the diagnostic).
                        if !crate::validate::provider_ref_is_usable(
                            reference,
                            Protocol::OpenAiResponses,
                        ) {
                            continue;
                        }
                        (None, Some(reference.id.clone()))
                    }
                };

                let detail = image.detail.map(|detail| match detail {
                    crate::types::ImageDetail::Auto => "auto".to_owned(),
                    crate::types::ImageDetail::Low => "low".to_owned(),
                    crate::types::ImageDetail::High => "high".to_owned(),
                });
                content.push(ResponsesContentPart::InputImage {
                    image_url,
                    file_id,
                    detail,
                });
            }
            UserPart::Media(Media::Audio(_)) => {}
            UserPart::ToolResult(result) => {
                if synthetic_tool_results.contains(&result.tool_call_id.0) {
                    continue;
                }
                pending_tool_calls.remove(&result.tool_call_id.0);
                let mut outputs = Vec::new();
                for result_part in &result.content {
                    match result_part {
                        ToolResultPart::Text(text) => {
                            outputs
                                .push(ResponsesToolResultBlock::InputText { text: text.clone() });
                        }
                        ToolResultPart::Media(Media::Image(image)) => match &image.source {
                            ImageSource::Url(url) => {
                                outputs.push(ResponsesToolResultBlock::InputImage {
                                    image_url: Some(WireImageUrl::Url(url.to_string())),
                                    file_id: None,
                                });
                            }
                            ImageSource::Inline(bytes) => {
                                // Do not guess a wire MIME (§75); drop the part
                                // if absent.
                                if let Some(media_type) = image.media_type.as_ref() {
                                    outputs.push(ResponsesToolResultBlock::InputImage {
                                        image_url: Some(WireImageUrl::Inline {
                                            media_type: media_type.to_string(),
                                            data: bytes.clone(),
                                        }),
                                        file_id: None,
                                    });
                                }
                            }
                            ImageSource::ProviderRef(reference) => {
                                if crate::validate::provider_ref_is_usable(
                                    reference,
                                    Protocol::OpenAiResponses,
                                ) {
                                    outputs.push(ResponsesToolResultBlock::InputImage {
                                        image_url: None,
                                        file_id: Some(reference.id.clone()),
                                    });
                                }
                            }
                        },
                        ToolResultPart::Media(Media::Audio(_)) => {}
                    }
                }
                // Preserve canonical order: emit buffered user content before
                // this standalone tool-result item.
                flush_user_content(&mut input, &mut content);
                let call_id = if preserve_tool_call_ids {
                    result.tool_call_id.0.clone()
                } else {
                    crate::protocol::normalize_tool_call_id(&result.tool_call_id.0)
                };
                if computer_call_ids.contains(&result.tool_call_id.0) {
                    // A tool result for a computer call is a
                    // `computer_call_output`, never a `function_call_output`:
                    // the provider pairs it with the earlier `computer_call`
                    // item by `call_id` and rejects the function shape.
                    input.push(ResponsesInputItem::ComputerCallOutput {
                        call_id,
                        output: ResponsesComputerScreenshot::from_blocks(&outputs),
                    });
                } else {
                    input.push(ResponsesInputItem::FunctionCallOutput {
                        call_id,
                        output: outputs,
                    });
                }
            }
        }
    }
    flush_user_content(&mut input, &mut content);
    input
}

fn map_assistant_input(
    assistant: &crate::types::AssistantMessage,
    model: &crate::catalog::Model,
    pending_tool_calls: &mut std::collections::BTreeSet<String>,
    computer_call_ids: &mut std::collections::BTreeSet<String>,
) -> Vec<ResponsesInputItem> {
    let mut input = Vec::new();
    // Preserve canonical part order: buffered assistant text is flushed as a
    // `message` item immediately before each function/reasoning item.
    let mut text_parts = Vec::new();
    for part in &assistant.content {
        match part {
            AssistantPart::Text(text) => text_parts.push(text.clone()),
            AssistantPart::ToolCall(tool_call) => {
                flush_assistant_text(&mut input, &mut text_parts);
                if !tool_call.async_execution {
                    pending_tool_calls.insert(tool_call.id.0.clone());
                }
                let call_id = crate::protocol::normalize_tool_call_id(&tool_call.id.0);
                if tool_call.name == COMPUTER_TOOL_NAME {
                    // Canonical history replays a computer call as a
                    // `computer_call` item, not as a function call the route
                    // never declared. The action is carried in the call's
                    // canonical arguments and is re-emitted only when it is a
                    // documented action type.
                    computer_call_ids.insert(tool_call.id.0.clone());
                    input.push(ResponsesInputItem::ComputerCall {
                        call_id,
                        action: canonical_computer_action(&tool_call.arguments_json),
                    });
                } else {
                    input.push(ResponsesInputItem::FunctionCall {
                        async_execution: tool_call.async_execution,
                        call_id,
                        name: tool_call.name.clone(),
                        arguments: tool_call.arguments_json.clone(),
                    });
                }
            }
            AssistantPart::Reasoning(reasoning) => {
                if let Some(state) = &reasoning.state {
                    if state.protocol == Protocol::OpenAiResponses && state.model == model.spec.id {
                        if let ReasoningStateKind::OpenAiReasoning {
                            item_id,
                            encrypted_content,
                        } = &state.kind
                        {
                            flush_assistant_text(&mut input, &mut text_parts);
                            input.push(ResponsesInputItem::Reasoning {
                                id: item_id.clone(),
                                summary: reasoning
                                    .text
                                    .as_ref()
                                    .map(|text| {
                                        vec![ResponsesReasoningSummary {
                                            r#type: "summary_text".to_owned(),
                                            text: text.clone(),
                                        }]
                                    })
                                    .unwrap_or_default(),
                                encrypted_content: encrypted_content.clone(),
                            });
                        }
                    }
                }
            }
            AssistantPart::Media(_) => {}
            AssistantPart::ProviderMetadata(_) => {}
        }
    }
    flush_assistant_text(&mut input, &mut text_parts);
    input
}

pub(crate) fn encode_canonical_input(
    model: &crate::catalog::Model,
    system: Option<&str>,
    messages: &[Message],
    compatibility: crate::CompatibilityMode,
) -> crate::responses::ResponsesInput {
    let mut input = map_system_input(model, system);
    let mut pending_tool_calls = std::collections::BTreeSet::new();
    let mut synthetic_tool_results = std::collections::HashSet::new();
    let mut computer_call_ids = std::collections::BTreeSet::new();
    for message in messages {
        match message {
            Message::User(user) => input.extend(map_user_input(
                model,
                user,
                false,
                &mut pending_tool_calls,
                &synthetic_tool_results,
                &computer_call_ids,
            )),
            Message::Assistant(assistant) => {
                if compatibility == crate::CompatibilityMode::Lossy {
                    push_synthetic_tool_results(
                        &mut input,
                        &mut pending_tool_calls,
                        &mut synthetic_tool_results,
                    );
                }
                input.extend(map_assistant_input(
                    assistant,
                    model,
                    &mut pending_tool_calls,
                    &mut computer_call_ids,
                ));
            }
        }
    }
    if compatibility == crate::CompatibilityMode::Lossy {
        push_synthetic_tool_results(
            &mut input,
            &mut pending_tool_calls,
            &mut synthetic_tool_results,
        );
    }
    crate::responses::ResponsesInput::new(input.into_iter().map(opaque_input_item).collect())
}

pub(crate) fn encode_replay_input(
    model: &crate::catalog::Model,
    system: Option<&str>,
    replay: &[crate::responses::ResponsesReplayItem],
) -> Result<crate::responses::ResponsesInput, AiError> {
    let compacted_base = matches!(
        replay.first(),
        Some(crate::responses::ResponsesReplayItem::Compacted(_))
    );
    let mut input: Vec<crate::responses::ResponsesItem> = if compacted_base {
        Vec::new()
    } else {
        map_system_input(model, system)
            .into_iter()
            .map(opaque_input_item)
            .collect()
    };
    let mut pending_tool_calls = std::collections::BTreeSet::new();
    let synthetic_tool_results = std::collections::HashSet::new();
    let mut computer_call_ids = std::collections::BTreeSet::new();
    for item in replay {
        match item {
            crate::responses::ResponsesReplayItem::ConfigurationUpdate(update) => {
                input.push(update.to_item())
            }
            crate::responses::ResponsesReplayItem::User(user) => {
                input.extend(
                    map_user_input(
                        model,
                        user,
                        true,
                        &mut pending_tool_calls,
                        &synthetic_tool_results,
                        &computer_call_ids,
                    )
                    .into_iter()
                    .map(opaque_input_item),
                );
            }
            crate::responses::ResponsesReplayItem::LocalAssistant(assistant) => {
                input.extend(
                    map_assistant_input(
                        assistant,
                        model,
                        &mut pending_tool_calls,
                        &mut computer_call_ids,
                    )
                    .into_iter()
                    .map(opaque_input_item),
                );
            }
            crate::responses::ResponsesReplayItem::Output(output)
            | crate::responses::ResponsesReplayItem::Compacted(output) => {
                output.validate_provider_output()?;
                // Authoritative provider output carries the only trustworthy
                // computer-call provenance: recognize `computer_call` items
                // verbatim so the caller's tool result for that `call_id` is
                // replayed as `computer_call_output`.
                for item in output.items() {
                    if item
                        .as_json()
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        == Some("computer_call")
                    {
                        if let Some(call_id) = item
                            .as_json()
                            .get("call_id")
                            .and_then(serde_json::Value::as_str)
                        {
                            computer_call_ids.insert(call_id.to_owned());
                        }
                    }
                }
                input.extend(output.items().iter().cloned());
            }
        }
    }
    Ok(crate::responses::ResponsesInput::new(input))
}

/// Builds the OpenAI Responses HTTP request parts.
pub(crate) fn build_request(
    model: &crate::catalog::Model,
    req: &Request,
) -> Result<HttpRequestParts, AiError> {
    // 1. Normalize model-gated reasoning, then run validation.
    let defaults = super::preset::request_defaults(model, req)?;
    let req = normalize_request_reasoning(&defaults, &model.spec.capabilities);
    let mut effective_capabilities = model.spec.capabilities.clone();
    effective_capabilities.responses_features = model.responses_features();
    let diagnostics = validate_request(
        &req,
        &effective_capabilities,
        &model.spec.limits,
        Protocol::OpenAiResponses,
        &model.spec.id,
        req.compatibility,
    )?;
    if req
        .responses
        .as_ref()
        .is_some_and(|options| options.input.is_some() && options.previous_response_id.is_some())
    {
        return Err(ConfigError::Parse(
            "Responses raw input and previous_response_id cannot be used together".to_owned(),
        )
        .into());
    }

    // 4. Map tools & tool_choice
    let grammar_tools = super::grammar_tools_for(model);
    let responses_lite = model.spec.capabilities.responses_lite;
    let mut tools_opt = if responses_lite {
        None
    } else {
        map_responses_tools(model, &req.tools)?
    };

    // 4b. Declared computer-use tool. The declaration is endpoint-gated data,
    // never a provider-name branch: a route whose profile does not declare the
    // tool fails closed instead of silently dropping the caller's declaration.
    // Responses Lite cannot carry tools at all, so it fails closed too.
    let computer_use = req
        .responses
        .as_ref()
        .and_then(|options| options.computer_use);
    if let Some(tool) = computer_use {
        if responses_lite {
            return Err(ConfigError::Parse(
                "computer use cannot be declared on a Responses Lite route".to_owned(),
            )
            .into());
        }
        if !model
            .endpoint
            .runtime
            .responses_profile
            .accepts_computer_use()
        {
            return Err(crate::error::UnsupportedError::ComputerUse.into());
        }
        tools_opt
            .get_or_insert_with(Vec::new)
            .push(ResponsesToolWire::Computer(ResponsesComputerTool {
                r#type: COMPUTER_TOOL_NAME,
                display_width: tool.display_width,
                display_height: tool.display_height,
                environment: tool.environment.wire_value(),
            }));
    }

    let tool_choice_opt = if !model.spec.capabilities.tools {
        None
    } else {
        match &req.tool_choice {
            ToolChoice::Auto => Some(serde_json::Value::String("auto".to_string())),
            ToolChoice::Required => Some(serde_json::Value::String("required".to_string())),
            ToolChoice::None => Some(serde_json::Value::String("none".to_string())),
            ToolChoice::Named(name) => Some(serde_json::json!({
                "type": if super::grammar::input_property(&req.tools, name, grammar_tools)?.is_some() { "custom" } else { "function" },
                "name": name
            })),
        }
    };

    // 5. Reasoning Configuration
    let reasoning_opt = map_responses_reasoning(model, &req.reasoning, req.reasoning_mode);

    // 6. Text / Output Format Config
    //
    // Design §7: a Lossy structured-output downgrade must actually drop the
    // capability from the wire request, not just emit a diagnostic. Strict mode
    // has already returned `Err` in `validate_request` above, so an unsupported
    // format only reaches here under Lossy — in which case we serialize plain
    // text (`text` omitted) rather than send a `text.format` the model lacks.
    let text_opt = map_responses_text(model, &req.output_format);

    // 7. Request Encrypted Reasoning
    let include = if model.spec.capabilities.reasoning.is_some() {
        vec!["reasoning.encrypted_content".to_string()]
    } else {
        vec![]
    };

    // Only forward an explicit caller cap. The Responses API treats this as
    // optional, and the ChatGPT Codex backend rejects it outright
    // (`{"detail":"Unsupported parameter: max_output_tokens"}`), so we never
    // synthesize a default from the local capacity limit. Subscription
    // endpoints that reject this parameter select omission through runtime
    // metadata rather than a codec-side provider identity check.
    let max_output_tokens = crate::effective_output_token_cap(model, req.max_output_tokens);

    let responses_options = req.responses.as_ref();
    // Codex `service_tier`: a declared endpoint capability, never a provider
    // identity. A route whose profile does not declare the field fails closed
    // instead of silently dropping a caller's billing-changing control.
    let service_tier = responses_options.and_then(|options| options.service_tier);
    if service_tier.is_some()
        && !model
            .endpoint
            .runtime
            .responses_profile
            .accepts_service_tier()
    {
        return Err(crate::error::UnsupportedError::ServiceTier.into());
    }
    let raw_input = responses_options.and_then(|options| options.input.as_ref());
    let refresh_instructions = raw_input
        .is_some_and(crate::responses::ResponsesInput::contains_compaction)
        .then(|| req.system.clone())
        .flatten();
    // Opaque replay is authoritative: do not encode canonical history only to
    // discard it when a raw input is present.
    let mut input = raw_input.cloned().unwrap_or_else(|| {
        crate::responses::encode_canonical_responses_input(
            model,
            req.system.as_deref(),
            &req.messages,
            req.compatibility,
        )
    });
    crate::responses::validate_responses_input(model, &input, &req.reasoning, false)?;
    if input.contains_configuration_updates()
        && responses_options
            .and_then(|options| options.context_management.as_ref())
            .is_some()
    {
        return Err(ConfigError::Parse(
            "configuration updates cannot be combined with automatic context management".into(),
        )
        .into());
    }
    validate_async_input(model, &input, &req.tools)?;
    let instructions = if responses_lite {
        input.strip_image_details_for_responses_lite();
        let mut items = responses_lite_prefix(model, refresh_instructions.as_deref(), &req.tools)?;
        items.extend(input.into_items());
        input = crate::responses::ResponsesInput::new(items);
        None
    } else {
        refresh_instructions
    };
    let mut wire_input = into_wire_input(input);
    if !responses_lite {
        map_grammar_replay(
            &mut wire_input,
            &req,
            raw_input.is_none(),
            super::grammar_tools_for(model),
        )?;
    }
    let explicit_cache_mode = supports_explicit_prompt_cache_mode(model);
    let responses_req = ResponsesRequest {
        model: model.spec.api_name.clone(),
        input: wire_input,
        instructions,
        previous_response_id: responses_options
            .and_then(|options| options.previous_response_id.clone()),
        context_management: responses_options
            .and_then(|options| options.context_management.clone()),
        tools: tools_opt,
        tool_choice: tool_choice_opt,
        // The internal Responses Lite route requires an explicit false even
        // when the model otherwise advertises parallel tool-call support.
        parallel_tool_calls: if responses_lite {
            Some(false)
        } else {
            (!req.tools.is_empty() && model.spec.capabilities.tools)
                .then_some(model.spec.capabilities.parallel_tool_calls)
        },
        max_output_tokens,
        temperature: req.temperature,
        reasoning: reasoning_opt,
        text: text_opt,
        service_tier,
        prompt_cache_key: prompt_cache_key(&req),
        prompt_cache_retention: (req.cache_retention == CacheRetention::Long
            && model.spec.cache.supports_long_retention
            && !explicit_cache_mode)
            .then_some("24h"),
        prompt_cache_options: prompt_cache_options(model, req.cache_retention),
        include,
        store: responses_options.is_some_and(|options| options.store),
        stream: true,
    };

    let mut body = responses_req.into_json()?;
    super::preset::sampling(model, &req, &mut body)?;
    let body_bytes =
        serde_json::to_vec(&body).map_err(|e| AiError::Decode(DecodeError::Json(e.to_string())))?;

    let url = crate::protocol::endpoint_url(&model.endpoint.base_url, "responses")?;

    let mut headers = responses_affinity_headers(model, cache_session_id(&req))?;
    crate::protocol::add_opencode_session_header(model, &req, &mut headers)?;

    Ok(HttpRequestParts {
        url,
        headers,
        body: bytes::Bytes::from(body_bytes),
        streaming: true,
        diagnostics,
    })
}

/// Convert canonical function-shaped history using the immutable request tool
/// schema, and preserve authoritative custom-call provenance on opaque replay.
/// Results are paired by call id, never inferred from their text payload.
fn map_grammar_replay(
    input: &mut serde_json::Value,
    req: &Request,
    canonical_calls: bool,
    grammar_tools: bool,
) -> Result<(), AiError> {
    let mut custom_ids = std::collections::HashSet::new();
    for message in req.messages.iter().filter(|_| canonical_calls) {
        if let Message::Assistant(assistant) = message {
            for part in &assistant.content {
                if let AssistantPart::ToolCall(call) = part {
                    if super::grammar::input_property(&req.tools, &call.name, grammar_tools)?
                        .is_some()
                    {
                        custom_ids.insert(call.id.0.clone());
                        custom_ids.insert(crate::protocol::normalize_tool_call_id(&call.id.0));
                    }
                }
            }
        }
    }
    let items = input.as_array_mut().expect("Responses input is an array");
    for item in items.iter_mut() {
        if canonical_calls
            && item.get("type").and_then(serde_json::Value::as_str) == Some("function_call")
        {
            if let Some(name) = item.get("name").and_then(serde_json::Value::as_str) {
                if let Some(property) =
                    super::grammar::input_property(&req.tools, name, grammar_tools)?
                {
                    let arguments = item
                        .get("arguments")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| {
                            DecodeError::InvalidProviderField(
                                "custom replay call has no arguments".to_owned(),
                            )
                        })?;
                    let text = super::grammar::replay_input(arguments, &property)?;
                    let object = item.as_object_mut().expect("a function call is an object");
                    object.remove("arguments");
                    object.insert("type".to_owned(), "custom_tool_call".into());
                    object.insert("input".to_owned(), text.into());
                }
            }
        }
        if item.get("type").and_then(serde_json::Value::as_str) == Some("custom_tool_call") {
            if let Some(id) = item.get("call_id").and_then(serde_json::Value::as_str) {
                custom_ids.insert(id.to_owned());
            }
        }
    }
    for item in items {
        if item.get("type").and_then(serde_json::Value::as_str) == Some("function_call_output")
            && item
                .get("call_id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|id| custom_ids.contains(id))
        {
            item["type"] = "custom_tool_call_output".into();
        }
    }
    Ok(())
}

/// Flush buffered user content parts as a `message` item, preserving canonical
/// order relative to interleaved `function_call_output` items (design §11).
fn flush_user_content(
    input: &mut Vec<ResponsesInputItem>,
    content: &mut Vec<ResponsesContentPart>,
) {
    if !content.is_empty() {
        input.push(ResponsesInputItem::Message {
            role: "user".to_string(),
            content: std::mem::take(content),
        });
    }
}

/// Flush buffered assistant text as a `message` item, preserving canonical
/// order relative to interleaved `function_call`/`reasoning` items (design §11
/// immutable replay). Consecutive text parts are joined; a `\n` boundary only
/// appears where the canonical parts were themselves adjacent text.
fn flush_assistant_text(input: &mut Vec<ResponsesInputItem>, text_parts: &mut Vec<String>) {
    if !text_parts.is_empty() {
        input.push(ResponsesInputItem::Message {
            role: "assistant".to_string(),
            content: vec![ResponsesContentPart::OutputText {
                text: std::mem::take(text_parts).join("\n"),
                annotations: vec![],
            }],
        });
    }
}

fn push_synthetic_tool_results(
    input: &mut Vec<ResponsesInputItem>,
    pending: &mut std::collections::BTreeSet<String>,
    synthetic: &mut std::collections::HashSet<String>,
) {
    for call_id in std::mem::take(pending) {
        synthetic.insert(call_id.clone());
        input.push(ResponsesInputItem::FunctionCallOutput {
            // Wire write: normalize to match the paired `function_call` above.
            call_id: crate::protocol::normalize_tool_call_id(&call_id),
            output: vec![ResponsesToolResultBlock::InputText {
                text: "Tool execution result was not supplied by the caller.".to_string(),
            }],
        });
    }
}

// --- SSE Chunk / Responses Response DTOs ---

#[derive(Deserialize)]
#[serde(tag = "type")]
enum ResponsesSseEvent {
    #[serde(rename = "response.created")]
    ResponseCreated { response: ResponsesResponseIdBlock },
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded {
        output_index: usize,
        item: ResponsesResponseItem,
    },
    #[serde(rename = "response.content_part.added")]
    ContentPartAdded {
        output_index: usize,
        content_index: usize,
        part: ResponsesContentPartAdded,
    },
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta {
        output_index: usize,
        #[serde(default)]
        content_index: usize,
        delta: String,
    },
    #[serde(rename = "response.output_text.done")]
    OutputTextDone {
        output_index: usize,
        #[serde(default)]
        content_index: usize,
    },
    #[serde(rename = "response.reasoning_text.delta")]
    ReasoningTextDelta { output_index: usize, delta: String },
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ReasoningSummaryDelta { output_index: usize, delta: String },
    #[serde(rename = "response.custom_tool_call_input.delta")]
    CustomToolInputDelta { output_index: usize, delta: String },
    #[serde(rename = "response.custom_tool_call_input.done")]
    CustomToolInputDone { output_index: usize, input: String },
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionCallArgumentsDelta { output_index: usize, delta: String },
    #[serde(rename = "response.function_call_arguments.done")]
    FunctionCallArgumentsDone {
        output_index: usize,
        /// The Responses API includes the complete JSON argument string on
        /// the terminal `*.done` event. Some Codex gateways omit the
        /// intermediate `*.delta` events (or deliver only this event), so it
        /// must be retained instead of ending an empty tool call.
        #[serde(default)]
        arguments: Option<String>,
    },
    #[serde(rename = "response.output_item.done")]
    OutputItemDone {
        output_index: usize,
        item: ResponsesResponseItemDone,
    },
    #[serde(rename = "response.completed")]
    ResponseCompleted {
        response: ResponsesResponseCompletedBlock,
    },
    #[serde(rename = "response.incomplete")]
    ResponseIncomplete {
        response: ResponsesResponseIncompleteBlock,
    },
    #[serde(rename = "response.failed")]
    ResponseFailed {
        response: ResponsesResponseFailedBlock,
    },
    // Top-level stream error event (apidocs openai-responses
    // 07-streaming-events.md §error: `{type:"error", code, message, param,
    // sequence_number}`). Distinct from `response.failed`, which nests the error
    // under `response.error`. Without this branch `#[serde(other)]` would swallow
    // it and the stream would surface `PrematureEof` instead of the real cause.
    #[serde(rename = "error")]
    StreamError {
        #[serde(default)]
        code: Option<String>,
        #[serde(default)]
        message: Option<String>,
        /// Codex's Responses gateway nests the documented error fields under
        /// `error`, while the public OpenAI API emits them at the top level.
        #[serde(default)]
        error: Option<ResponsesErrorDto>,
    },
    // Out-of-scope event families
    #[serde(other)]
    IgnoredEvent,
}

#[derive(Deserialize)]
struct ResponsesResponseIdBlock {
    id: String,
}

#[derive(Deserialize)]
struct ResponsesContentPartAdded {
    r#type: String,
}

#[derive(Deserialize)]
struct ResponsesResponseItem {
    #[serde(default, rename = "async")]
    async_execution: bool,
    id: String,
    r#type: String,
    #[serde(default)]
    name: Option<String>,
    // A function_call item carries a `call_id` that pairs with its
    // `function_call_output` (design §12.2); prefer it over the item `id`.
    #[serde(default)]
    call_id: Option<String>,
    /// Some codex/Responses endpoints send the full arguments inline in the
    /// `output_item.added` event rather than (or in addition to) separate
    /// `function_call_arguments.delta` events. Capture them here so they are
    /// not silently dropped by serde (unknown-field ignore).
    #[serde(default)]
    arguments: Option<String>,
    #[serde(default)]
    input: Option<String>,
    /// Provider computer-use action (`computer_call` items only). Retained as
    /// raw JSON so the codec can validate the action discriminator and bound
    /// the canonical payload before surfacing it.
    #[serde(default)]
    action: Option<serde_json::Value>,
    /// Provider-reported pending safety checks for a computer call.
    #[serde(default)]
    pending_safety_checks: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct ResponsesResponseItemDone {
    #[serde(default, rename = "async")]
    async_execution: Option<bool>,
    id: String,
    r#type: String,
    #[serde(default)]
    encrypted_content: Option<String>,
    /// A few Responses-compatible gateways put the final function-call
    /// arguments only on `response.output_item.done`. Preserve this fallback
    /// shape as well as the documented `function_call_arguments.done` form.
    #[serde(default)]
    arguments: Option<String>,
    #[serde(default)]
    input: Option<String>,
    /// Terminal computer-use action; see [`ResponsesResponseItem::action`].
    #[serde(default)]
    action: Option<serde_json::Value>,
    /// Terminal pending safety checks for a computer call.
    #[serde(default)]
    pending_safety_checks: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct ResponsesResponseCompletedBlock {
    #[serde(default)]
    service_tier: Option<String>,
    /// Full terminal output is the only authoritative raw replay source. Added
    /// events are intentionally not used because some servers send skeletons.
    #[serde(default)]
    output: Option<Vec<crate::responses::ResponsesItem>>,
    // `usage` is nullable in the Responses object (apidocs
    // openai-responses/01-responses.md: `usage: null` on non-terminal snapshots,
    // populated on completion). Model it as optional so a documented terminal
    // event without usage still decodes to a default-usage `Finished`.
    #[serde(default)]
    usage: Option<ResponsesUsageDto>,
}

#[derive(Deserialize)]
struct ResponsesResponseIncompleteBlock {
    #[serde(default)]
    service_tier: Option<String>,
    /// Incomplete terminal responses carry the authoritative output produced
    /// before the limit/refusal stopped generation. Preserve it for exact
    /// Responses replay just as we do for completed responses.
    #[serde(default)]
    output: Option<Vec<crate::responses::ResponsesItem>>,
    // The documented field is `incomplete_details` (object with `reason`), not
    // `status_details` (apidocs openai-responses/01-responses.md:6013,15394).
    incomplete_details: ResponsesIncompleteDetailsDto,
    #[serde(default)]
    usage: Option<ResponsesUsageDto>,
}

#[derive(Deserialize)]
struct ResponsesIncompleteDetailsDto {
    reason: String,
}

#[derive(Deserialize)]
struct ResponsesResponseFailedBlock {
    error: Option<ResponsesFailedErrorDto>,
}

// Native failed terminals permit absent/null error messages. Keep typed policy
// denials even without prose, without relaxing arbitrary top-level error DTOs.
#[derive(Default, Deserialize)]
struct ResponsesFailedErrorDto {
    code: Option<String>,
    message: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct ResponsesErrorDto {
    /// OpenAI-compatible gateways emit JSON `null` when no stable error code is
    /// available. Preserve the provider message instead of turning that valid
    /// error envelope into a decoder failure.
    #[serde(default)]
    code: Option<String>,
    /// The human-readable provider error remains required. Missing or nullable
    /// messages are malformed and must not be silently replaced.
    message: String,
    #[serde(default, rename = "type")]
    kind: Option<String>,
}

// OpenAI Responses usage uses `input_tokens`/`output_tokens` (NOT the Chat
// `prompt_tokens`/`completion_tokens`), with cache + reasoning detail objects
// (design §15; docs/research/apidocs/openai-responses/02-create.md).
#[derive(Deserialize)]
struct ResponsesUsageDto {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    input_tokens_details: Option<ResponsesInputTokensDetails>,
    #[serde(default)]
    output_tokens_details: Option<ResponsesOutputTokensDetails>,
}

#[derive(Deserialize)]
struct ResponsesInputTokensDetails {
    #[serde(default)]
    cached_tokens: u64,
    #[serde(default)]
    cache_write_tokens: u64,
}

#[derive(Deserialize)]
struct ResponsesOutputTokensDetails {
    #[serde(default)]
    reasoning_tokens: u64,
}

// --- Decode Implementations ---
//
// OpenAI Responses is always streamed (design §12.2); there is no non-streaming
// decode path, so this codec deliberately exposes none.

/// Backfill opaque encrypted reasoning from the authoritative terminal output.
///
/// Some Responses-compatible gateways (Azure OpenAI, xAI) omit
/// `reasoning.encrypted_content` from `response.output_item.done` and provide it
/// only in `response.completed.response.output`. Without this, `store:false`
/// multi-turn replay would drop the reasoning continuation for those turns.
/// Only an existing opaque reasoning state is enriched; a missing item is left
/// alone rather than inventing one.
fn backfill_reasoning_signatures(
    builder: &mut ResponseBuilder,
    output: &[crate::responses::ResponsesItem],
) -> Result<(), AiError> {
    for item in output {
        let json = item.as_json();
        if json.get("type").and_then(serde_json::Value::as_str) != Some("reasoning") {
            continue;
        }
        let Some(encrypted) = json
            .get("encrypted_content")
            .and_then(serde_json::Value::as_str)
            .filter(|content| !content.is_empty())
        else {
            continue;
        };
        let item_id = json.get("id").and_then(serde_json::Value::as_str);
        let target = builder
            .reasoning_states
            .iter()
            .find_map(|(index, state)| match &state.kind {
                ReasoningStateKind::OpenAiReasoning {
                    item_id: stored_id,
                    encrypted_content,
                } if encrypted_content.is_none() && stored_id.as_deref() == item_id => Some(*index),
                _ => None,
            });
        let Some(index) = target else { continue };
        let mut state = builder.reasoning_states[&index].clone();
        if let ReasoningStateKind::OpenAiReasoning {
            encrypted_content, ..
        } = &mut state.kind
        {
            *encrypted_content = Some(encrypted.to_owned());
        }
        builder.set_reasoning_state(index, state)?;
    }
    Ok(())
}

/// Close any tool-call parts that a provider left open before its terminal
/// response event. Some Responses-compatible gateways send complete arguments
/// in `output_item.added` and omit `function_call_arguments.done`; closing here
/// keeps the canonical stream balanced without prematurely rejecting a later
/// argument delta.
fn close_open_tool_calls(
    events: &mut Vec<StreamEvent>,
    builder: &mut ResponseBuilder,
) -> Result<(), AiError> {
    let open: Vec<usize> = builder
        .tool_call_builders
        .keys()
        .copied()
        .filter(|index| !builder.ended_indices.contains(index))
        .collect();
    for index in open {
        if super::grammar::is_open(builder, index) {
            super::grammar::finish(events, builder, index, None)?;
            continue;
        }
        // A computer call whose action never validated is not a representable
        // exchange: fail closed before the terminal response instead of
        // surfacing an actionless call for a caller to guess at.
        if builder.tool_call_builders.get(&index).is_some_and(|call| {
            call.name == COMPUTER_TOOL_NAME && call.arguments_json.trim().is_empty()
        }) {
            return Err(computer_action_error("missing"));
        }
        emit_event(
            events,
            builder,
            StreamEvent::ToolCallEnd {
                index,
                argument_error: None,
            },
        )?;
    }
    Ok(())
}

/// Terminal opaque custom input must agree with the call exposed to the host.
/// A late monotonic suffix can complete an open call; changed closed input is
/// rejected rather than leaving canonical execution and opaque replay divergent.
fn reconcile_custom_output(
    events: &mut Vec<StreamEvent>,
    builder: &mut ResponseBuilder,
    output: &[crate::responses::ResponsesItem],
) -> Result<(), AiError> {
    for item in output {
        let item = item.as_json();
        if item.get("type").and_then(serde_json::Value::as_str) != Some("custom_tool_call") {
            continue;
        }
        let invalid = || {
            DecodeError::InvalidProviderField(
                "terminal custom tool call disagrees with its streamed envelope".to_owned(),
            )
        };
        let id = item
            .get("call_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(invalid)?;
        let name = item
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(invalid)?;
        let input = item
            .get("input")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(invalid)?;
        let index = builder
            .tool_call_builders
            .iter()
            .find(|(_, call)| call.id.0 == id && call.name == name)
            .map(|(index, _)| *index)
            .ok_or_else(invalid)?;
        super::grammar::finish(events, builder, index, Some(input))?;
    }
    Ok(())
}

/// Settle tier-aware pricing only after authoritative terminal usage/tier.
/// Missing usage or an undeclared tariff is unpriced, never fabricated as zero.
fn settle_responses_cost(
    model: &crate::catalog::Model,
    builder: &mut ResponseBuilder,
    echoed: Option<&str>,
) -> Result<(), AiError> {
    let cost = match (&builder.pricing, &builder.usage) {
        (Some(pricing), Some(usage)) => crate::pricing::responses_cost_of(
            pricing,
            usage,
            model.endpoint.runtime.responses_profile,
            &model.spec.api_name,
            builder.requested_service_tier,
            echoed,
        )?,
        _ => None,
    };
    builder.response_cost = Some(cost);
    if cost.is_none() && builder.pricing.is_some() {
        builder.add_diagnostic(crate::Diagnostic {
            code: "unpriced_responses_tier".to_owned(),
            message:
                "Responses cost is unknown: missing usage or an unqualified service-tier tariff"
                    .to_owned(),
        });
    }
    Ok(())
}

/// Validates and bounds a provider computer action into the canonical argument
/// payload (`{"action": …, "pending_safety_checks": …}`).
///
/// The codec fails closed on a missing or unknown action: computer-use
/// authority lives outside this crate, so an unrecognized action must never be
/// handed to a caller as if it were a known, bounded instruction.
fn computer_call_arguments(
    action: Option<&serde_json::Value>,
    pending_safety_checks: Option<&serde_json::Value>,
) -> Result<String, AiError> {
    let kind = action
        .and_then(|action| action.get("type"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("missing");
    if !COMPUTER_ACTION_TYPES.contains(&kind) {
        return Err(computer_action_error(kind));
    }
    let action = action.expect("validated action is present");
    if pending_safety_checks.is_some_and(|checks| !checks.is_array()) {
        return Err(AiError::Decode(DecodeError::Json(
            "OpenAI Responses computer safety checks must be an array".to_owned(),
        )));
    }
    let mut payload = serde_json::Map::with_capacity(2);
    payload.insert("action".to_owned(), action.clone());
    if let Some(checks) =
        pending_safety_checks.filter(|checks| checks.as_array().is_some_and(|c| !c.is_empty()))
    {
        payload.insert("pending_safety_checks".to_owned(), checks.clone());
    }
    let arguments = serde_json::to_string(&serde_json::Value::Object(payload))
        .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))?;
    if arguments.len() > MAX_COMPUTER_ACTION_BYTES {
        return Err(AiError::Decode(DecodeError::Json(format!(
            "OpenAI Responses computer action is {} bytes, over the {} byte bound",
            arguments.len(),
            MAX_COMPUTER_ACTION_BYTES
        ))));
    }
    Ok(arguments)
}

fn computer_action_error(kind: &str) -> AiError {
    AiError::Decode(DecodeError::Json(format!(
        "unsupported OpenAI Responses computer action `{kind}`"
    )))
}

/// Extracts a replayable action from canonical computer-call arguments.
///
/// The codec emits `{"action": …, "pending_safety_checks": …}`, but a caller
/// may hand back a bare action object. Anything that is not a documented action
/// type yields no action at all, so canonical replay never re-sends an
/// unrecognized instruction as if the provider had produced it.
fn canonical_computer_action(arguments_json: &str) -> Option<serde_json::Value> {
    let parsed: serde_json::Value = serde_json::from_str(arguments_json).ok()?;
    let action = parsed.get("action").unwrap_or(&parsed);
    let kind = action.get("type").and_then(serde_json::Value::as_str)?;
    COMPUTER_ACTION_TYPES
        .contains(&kind)
        .then(|| action.clone())
}

/// Decodes a streaming SSE event from OpenAI Responses, emitting StreamEvents.
pub(crate) fn decode_stream_event(
    model: &crate::catalog::Model,
    sse_event: &SseEvent,
    builder: &mut ResponseBuilder,
) -> Result<Vec<StreamEvent>, AiError> {
    builder.observe_provider_stream_event()?;
    let raw_data = sse_event.data.trim();
    if raw_data.is_empty() {
        return Ok(vec![]);
    }

    let event: ResponsesSseEvent = serde_json::from_str(raw_data).map_err(|error| {
        let value = serde_json::from_str::<serde_json::Value>(raw_data).ok();
        let event_type = value
            .as_ref()
            .and_then(|value| value.get("type"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let keys = value
            .as_ref()
            .and_then(serde_json::Value::as_object)
            .map(|object| object.keys().cloned().collect::<Vec<_>>().join(","))
            .unwrap_or_default();
        let error_keys = value
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(serde_json::Value::as_object)
            .map(|object| object.keys().cloned().collect::<Vec<_>>().join(","))
            .unwrap_or_default();
        AiError::Decode(DecodeError::Json(format!(
            "invalid OpenAI Responses `{event_type}` event ({keys}; error={error_keys}): {error}"
        )))
    })?;

    let mut events = Vec::new();

    match event {
        ResponsesSseEvent::ResponseCreated { response } => {
            builder.response_id = Some(response.id.clone());
            emit_event(
                &mut events,
                builder,
                StreamEvent::Started {
                    response_id: Some(response.id),
                },
            )?;
        }
        ResponsesSseEvent::OutputItemAdded { output_index, item } => {
            crate::responses::validate_provider_output_type(&item.r#type)?;
            if item.async_execution
                && (!matches!(item.r#type.as_str(), "function_call" | "custom_tool_call")
                    || !model.responses_features().async_tools
                    || !builder.tool_definitions.as_ref().is_some_and(|tools| {
                        tools.iter().any(|tool| {
                            tool.async_execution && Some(&tool.name) == item.name.as_ref()
                        })
                    }))
            {
                return Err(DecodeError::InvalidProviderField(
                    "unadvertised async tool call".into(),
                )
                .into());
            }
            if item.r#type == "custom_tool_call" {
                let key = format!("item_{output_index}");
                let index = get_canonical_index(builder, &key);
                let name = item.name.ok_or_else(|| {
                    DecodeError::InvalidProviderField(
                        "custom tool call is missing its name".to_owned(),
                    )
                })?;
                if builder.tool_call_builders.contains_key(&index) {
                    return Err(DecodeError::InvalidProviderField(
                        "custom tool call started more than once".to_owned(),
                    )
                    .into());
                }
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::ToolCallStart {
                        async_execution: item.async_execution,
                        index,
                        id: ToolCallId(item.call_id.unwrap_or(item.id)),
                        name,
                    },
                )?;
                super::grammar::start(builder, index)?;
                if let Some(input) = item.input {
                    super::grammar::delta(&mut events, builder, index, &input)?;
                }
            } else if item.r#type == "function_call" {
                let key = format!("item_{}", output_index);
                let canonical_idx = get_canonical_index(builder, &key);
                if let Some(name) = item.name {
                    let call_id = item.call_id.unwrap_or(item.id);
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ToolCallStart {
                            async_execution: item.async_execution,
                            index: canonical_idx,
                            id: ToolCallId(call_id),
                            name,
                        },
                    )?;
                    // Some codex/Responses endpoints send the full arguments inline
                    // in the `output_item.added` event rather than via separate
                    // `function_call_arguments.delta` events. Feed them as an
                    // initial delta so the tool call builder is populated even
                    // when no delta events follow.
                    if let Some(ref inline_args) = item.arguments {
                        if !inline_args.trim().is_empty() {
                            emit_event(
                                &mut events,
                                builder,
                                StreamEvent::ToolCallArgsDelta {
                                    index: canonical_idx,
                                    delta: inline_args.clone(),
                                },
                            )?;
                        }
                    }
                }
            } else if item.r#type == "computer_call" {
                let key = format!("item_{}", output_index);
                let canonical_idx = get_canonical_index(builder, &key);
                let call_id = item.call_id.clone().unwrap_or_else(|| item.id.clone());
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::ToolCallStart {
                        async_execution: false,
                        index: canonical_idx,
                        id: ToolCallId(call_id),
                        name: COMPUTER_TOOL_NAME.to_owned(),
                    },
                )?;
                // The action may be deferred to `output_item.done`; when it is
                // present here the terminal check in `close_open_tool_calls`
                // only accepts a payload that already validated.
                if item.action.is_some() || item.pending_safety_checks.is_some() {
                    let arguments = computer_call_arguments(
                        item.action.as_ref(),
                        item.pending_safety_checks.as_ref(),
                    )?;
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ToolCallArgsDelta {
                            index: canonical_idx,
                            delta: arguments,
                        },
                    )?;
                }
            }
        }
        ResponsesSseEvent::ContentPartAdded {
            output_index,
            content_index,
            part,
        } => {
            if part.r#type == "output_text" {
                let key = format!("item_{}_content_{}", output_index, content_index);
                let canonical_idx = get_canonical_index(builder, &key);
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::TextStart {
                        index: canonical_idx,
                    },
                )?;
            }
        }
        ResponsesSseEvent::OutputTextDelta {
            output_index,
            content_index,
            delta,
        } => {
            if !delta.is_empty() {
                let key = format!("item_{}_content_{}", output_index, content_index);
                let canonical_idx = get_canonical_index(builder, &key);
                if !builder.text_buffers.contains_key(&canonical_idx) {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::TextStart {
                            index: canonical_idx,
                        },
                    )?;
                }
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::TextDelta {
                        index: canonical_idx,
                        delta,
                    },
                )?;
            }
        }
        ResponsesSseEvent::OutputTextDone {
            output_index,
            content_index,
        } => {
            let key = format!("item_{}_content_{}", output_index, content_index);
            let canonical_idx = get_canonical_index(builder, &key);
            // Tolerate a duplicated `output_text.done` (§8: one *End per part).
            if builder.text_buffers.contains_key(&canonical_idx)
                && !builder.ended_indices.contains(&canonical_idx)
            {
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::TextEnd {
                        index: canonical_idx,
                    },
                )?;
            }
        }
        ResponsesSseEvent::ReasoningTextDelta {
            output_index,
            delta,
        } => {
            if !delta.is_empty() {
                let key = format!("reasoning_{}", output_index);
                let canonical_idx = get_canonical_index(builder, &key);
                if !builder.reasoning_text_buffers.contains_key(&canonical_idx) {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ReasoningStart {
                            index: canonical_idx,
                        },
                    )?;
                }
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::ReasoningDelta {
                        index: canonical_idx,
                        delta,
                    },
                )?;
            }
        }
        ResponsesSseEvent::ReasoningSummaryDelta {
            output_index,
            delta,
        } => {
            if !delta.is_empty() {
                let key = format!("reasoning_{}", output_index);
                let canonical_idx = get_canonical_index(builder, &key);
                if !builder.reasoning_text_buffers.contains_key(&canonical_idx) {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ReasoningStart {
                            index: canonical_idx,
                        },
                    )?;
                }
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::ReasoningDelta {
                        index: canonical_idx,
                        delta,
                    },
                )?;
            }
        }
        ResponsesSseEvent::CustomToolInputDelta {
            output_index,
            delta,
        } => {
            let index = get_canonical_index(builder, &format!("item_{output_index}"));
            super::grammar::delta(&mut events, builder, index, &delta)?;
        }
        ResponsesSseEvent::CustomToolInputDone {
            output_index,
            input,
        } => {
            let index = get_canonical_index(builder, &format!("item_{output_index}"));
            super::grammar::finish(&mut events, builder, index, Some(&input))?;
        }
        ResponsesSseEvent::FunctionCallArgumentsDelta {
            output_index,
            delta,
        } => {
            if !delta.is_empty() {
                let key = format!("item_{}", output_index);
                let canonical_idx = get_canonical_index(builder, &key);
                if super::grammar::is_open(builder, canonical_idx) {
                    return Err(DecodeError::InvalidProviderField(
                        "custom tool call received function arguments".to_owned(),
                    )
                    .into());
                }
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::ToolCallArgsDelta {
                        index: canonical_idx,
                        delta,
                    },
                )?;
            }
        }
        ResponsesSseEvent::FunctionCallArgumentsDone {
            output_index,
            arguments,
        } => {
            let key = format!("item_{}", output_index);
            let canonical_idx = get_canonical_index(builder, &key);
            if super::grammar::is_open(builder, canonical_idx) {
                return Err(DecodeError::InvalidProviderField(
                    "custom tool call received function arguments".to_owned(),
                )
                .into());
            }
            // Providers are allowed to send the complete argument payload
            // only on the terminal event. If no deltas populated the builder,
            // feed that payload before closing the call. If deltas already
            // arrived, ignore the duplicate complete value to avoid appending
            // arguments twice.
            if let Some(arguments) = arguments {
                if !arguments.trim().is_empty()
                    && builder
                        .tool_call_builders
                        .get(&canonical_idx)
                        .is_some_and(|call| call.arguments_json.trim().is_empty())
                {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ToolCallArgsDelta {
                            index: canonical_idx,
                            delta: arguments,
                        },
                    )?;
                }
            }
            // Tolerate a duplicated `function_call_arguments.done` (§8).
            if builder.tool_call_builders.contains_key(&canonical_idx)
                && !builder.ended_indices.contains(&canonical_idx)
            {
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::ToolCallEnd {
                        index: canonical_idx,
                        argument_error: None,
                    },
                )?;
            }
        }
        ResponsesSseEvent::OutputItemDone { output_index, item } => {
            crate::responses::validate_provider_output_type(&item.r#type)?;
            if let Some(marker) = item.async_execution {
                let index = get_canonical_index(builder, &format!("item_{output_index}"));
                if builder
                    .tool_call_builders
                    .get(&index)
                    .is_none_or(|call| call.async_execution != marker)
                {
                    return Err(DecodeError::InvalidProviderField(
                        "async call marker changed after call start".into(),
                    )
                    .into());
                }
            }
            if item.r#type == "custom_tool_call" {
                let index = get_canonical_index(builder, &format!("item_{output_index}"));
                super::grammar::finish(&mut events, builder, index, item.input.as_deref())?;
            } else if item.r#type == "reasoning" {
                let key = format!("reasoning_{}", output_index);
                let canonical_idx = get_canonical_index(builder, &key);
                // A duplicated `output_item.done` must not re-emit End (§8).
                let already_ended = builder.ended_indices.contains(&canonical_idx);
                let had_visible_text = builder.reasoning_text_buffers.contains_key(&canonical_idx);
                if had_visible_text && !already_ended {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ReasoningEnd {
                            index: canonical_idx,
                        },
                    )?;
                } else if item.encrypted_content.is_some() && !already_ended {
                    // Opaque reasoning with no visible delta (design §6.3/§14):
                    // still surface a reasoning part so the opaque `item_id`/
                    // `encrypted_content` is preserved. Without an observed part
                    // (`ReasoningStart`), `ResponseBuilder::finish` — which only
                    // assembles observed indices — would silently drop the state.
                    // The empty text buffer becomes `ReasoningPart.text = None`.
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ReasoningStart {
                            index: canonical_idx,
                        },
                    )?;
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ReasoningEnd {
                            index: canonical_idx,
                        },
                    )?;
                }

                // Persist opaque reasoning state for an observed reasoning part
                // even when `encrypted_content` is absent here: a few gateways
                // (Azure OpenAI, xAI) send it only in the terminal
                // `response.completed` output, where `backfill_reasoning_signatures`
                // merges it into this state for `store:false` replay.
                if item.encrypted_content.is_some() || had_visible_text {
                    builder.set_reasoning_state(
                        canonical_idx,
                        ReasoningState {
                            model: builder.model.clone(),
                            protocol: Protocol::OpenAiResponses,
                            kind: ReasoningStateKind::OpenAiReasoning {
                                item_id: Some(item.id),
                                encrypted_content: item.encrypted_content,
                            },
                        },
                    )?;
                }
            } else if item.r#type == "function_call" {
                // Some Codex-compatible streams omit both argument deltas and
                // `function_call_arguments.done`, putting the complete payload
                // on output_item.done. Feed it before terminal closure.
                let key = format!("item_{}", output_index);
                let canonical_idx = get_canonical_index(builder, &key);
                if let Some(arguments) = item.arguments {
                    if !arguments.trim().is_empty()
                        && builder
                            .tool_call_builders
                            .get(&canonical_idx)
                            .is_some_and(|call| call.arguments_json.trim().is_empty())
                    {
                        emit_event(
                            &mut events,
                            builder,
                            StreamEvent::ToolCallArgsDelta {
                                index: canonical_idx,
                                delta: arguments,
                            },
                        )?;
                    }
                }
                if builder.tool_call_builders.contains_key(&canonical_idx)
                    && !builder.ended_indices.contains(&canonical_idx)
                {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ToolCallEnd {
                            index: canonical_idx,
                            argument_error: None,
                        },
                    )?;
                }
            } else if item.r#type == "computer_call" {
                // A terminal action/check must not silently replace an already
                // published payload. The canonical stream has no replacement
                // event, so refuse a changed instruction before ToolCallEnd
                // rather than losing a late safety check or executing stale data.
                let key = format!("item_{}", output_index);
                let canonical_idx = get_canonical_index(builder, &key);
                if let Some(call) = builder.tool_call_builders.get(&canonical_idx) {
                    if item.action.is_some() || item.pending_safety_checks.is_some() {
                        let prior: Option<serde_json::Value> =
                            serde_json::from_str(&call.arguments_json).ok();
                        let arguments = computer_call_arguments(
                            item.action
                                .as_ref()
                                .or_else(|| prior.as_ref()?.get("action")),
                            item.pending_safety_checks
                                .as_ref()
                                .or_else(|| prior.as_ref()?.get("pending_safety_checks")),
                        )?;
                        if let Some(prior) = prior {
                            let terminal: serde_json::Value = serde_json::from_str(&arguments)
                                .expect("computer_call_arguments produces JSON");
                            if prior != terminal {
                                return Err(AiError::Decode(DecodeError::Json(
                                    "OpenAI Responses terminal computer action or safety checks changed after publication".to_owned(),
                                )));
                            }
                        } else {
                            emit_event(
                                &mut events,
                                builder,
                                StreamEvent::ToolCallArgsDelta {
                                    index: canonical_idx,
                                    delta: arguments,
                                },
                            )?;
                        }
                    }
                }
                if builder.tool_call_builders.contains_key(&canonical_idx)
                    && !builder.ended_indices.contains(&canonical_idx)
                {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ToolCallEnd {
                            index: canonical_idx,
                            argument_error: None,
                        },
                    )?;
                }
            }
        }
        ResponsesSseEvent::ResponseCompleted { response } => {
            // Design §15: a completed response that produced a function call is a
            // tool-use stop; otherwise it is a normal end-of-turn.
            let stop = if builder.tool_call_builders.is_empty() {
                StopReason::EndTurn
            } else {
                StopReason::ToolUse
            };
            builder.set_stop_reason(stop);
            if let Some(output) = response.output.filter(|output| !output.is_empty()) {
                validate_terminal_async_markers(builder, &output)?;
                backfill_reasoning_signatures(builder, &output)?;
                reconcile_custom_output(&mut events, builder, &output)?;
                builder.responses_output = Some(crate::responses::ResponsesOutput::new(output));
            }
            close_open_tool_calls(&mut events, builder)?;
            // Usage is optional on the wire; only emit a `Usage` event when the
            // provider reported one so `Finished.usage` is a default rather than a
            // misleading all-zero count.
            if let Some(usage) = &response.usage {
                let u = map_usage(usage)?;
                emit_event(&mut events, builder, StreamEvent::Usage(u))?;
            }

            settle_responses_cost(model, builder, response.service_tier.as_deref())?;
            let resp = builder.finish_mut()?;
            emit_event(&mut events, builder, StreamEvent::Finished(resp))?;
        }
        ResponsesSseEvent::ResponseIncomplete { response } => {
            let stop = match response.incomplete_details.reason.as_str() {
                "max_output_tokens" => StopReason::MaxTokens,
                "content_filter" => StopReason::Refusal,
                "steered" => StopReason::Steered,
                other => StopReason::Other(other.to_string()),
            };
            builder.set_stop_reason(stop);
            if let Some(output) = response.output.filter(|output| !output.is_empty()) {
                validate_terminal_async_markers(builder, &output)?;
                backfill_reasoning_signatures(builder, &output)?;
                reconcile_custom_output(&mut events, builder, &output)?;
                builder.responses_output = Some(crate::responses::ResponsesOutput::new(output));
            }
            close_open_tool_calls(&mut events, builder)?;

            if let Some(usage) = &response.usage {
                let u = map_usage(usage)?;
                emit_event(&mut events, builder, StreamEvent::Usage(u))?;
            }

            settle_responses_cost(model, builder, response.service_tier.as_deref())?;
            let resp = builder.finish_mut()?;
            emit_event(&mut events, builder, StreamEvent::Finished(resp))?;
        }
        ResponsesSseEvent::ResponseFailed { response } => {
            let error = response.error.unwrap_or_default();
            return Err(AiError::ResponsesFailed(ProviderError {
                code: error.code,
                kind: error.kind,
                message: error
                    .message
                    .unwrap_or_else(|| "response.failed event received".into()),
                request_id: None,
            }));
        }
        ResponsesSseEvent::StreamError {
            code,
            message,
            error,
        } => {
            let nested_code = error.as_ref().and_then(|error| error.code.clone());
            let nested_kind = error.as_ref().and_then(|error| error.kind.clone());
            let nested_message = error.map(|error| error.message);
            return Err(AiError::Provider(ProviderError {
                code: code.or(nested_code),
                kind: nested_kind,
                message: message
                    .or(nested_message)
                    .unwrap_or_else(|| "provider stream error".to_owned()),
                request_id: None,
            }));
        }
        ResponsesSseEvent::IgnoredEvent => {}
    }

    Ok(events)
}

// --- Helpers ---

fn map_usage(usage: &ResponsesUsageDto) -> Result<Usage, AiError> {
    // Design §15: OpenAI `input_tokens` INCLUDES cache, so cache read + write are
    // subtracted out to keep the canonical buckets disjoint (full-rate input only).
    let cache_read = usage
        .input_tokens_details
        .as_ref()
        .map(|d| d.cached_tokens)
        .unwrap_or(0);
    let cache_write = usage
        .input_tokens_details
        .as_ref()
        .map(|d| d.cache_write_tokens)
        .unwrap_or(0);
    let reasoning = usage
        .output_tokens_details
        .as_ref()
        .map(|d| d.reasoning_tokens)
        .unwrap_or(0);
    // Some OpenAI-compatible gateways emit detail counters that exceed the
    // nominal aggregate. Preserve disjoint buckets and the completed response
    // by flooring only the residual full-rate input bucket.
    let input = usage
        .input_tokens
        .saturating_sub(cache_read)
        .saturating_sub(cache_write);
    crate::responses::normalize_responses_usage(
        input,
        cache_read,
        cache_write,
        usage.output_tokens,
        reasoning,
    )
    .ok_or(AiError::Decode(DecodeError::UsageUnderflow))
}

// --- Tests ---

#[cfg(test)]
mod tests;

/// Offline fixture matrix for the OpenAI Responses stream decoder
/// (design §19; plan Task 11.2).
#[cfg(test)]
mod fixture_tests;

#[cfg(test)]
#[path = "openai_responses_gpt6_tests.rs"]
mod gpt6_tests;
