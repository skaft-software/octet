//! Anthropic Messages private wire protocol codec.

use serde::{Deserialize, Serialize};

use crate::error::{AiError, ConfigError, DecodeError, ProviderError, UnsupportedError};
use crate::protocol::sse::SseEvent;
use crate::protocol::{
    cache_control, emit_event, get_canonical_index, Base64Bytes, CacheControl, HttpRequestParts,
};
use crate::stream::{ResponseBuilder, StreamEvent};
use crate::types::{
    AssistantPart, CacheRetention, ImageSource, Media, Message, Protocol, ReasoningConfig,
    ReasoningState, ReasoningStateKind, Request, StopReason, ToolCallId, ToolChoice,
    ToolResultPart, Usage, UserPart,
};
use crate::validate::{normalize_request_reasoning, validate_request};

/// Documented Anthropic base64 image media type, or `None` if absent/unsupported.
///
/// Anthropic's `source.type == "base64"` requires an explicit media type from a
/// documented set (apidocs anthropic). A missing or out-of-set type has no wire
/// mapping, so — rather than guess (design §75) — the codec drops the part
/// (validation already emitted the diagnostic).
fn anthropic_image_media_type(image: &crate::types::ImageMedia) -> Option<String> {
    let mime = image.media_type.as_ref()?.to_string();
    match mime.as_str() {
        "image/jpeg" | "image/png" | "image/gif" | "image/webp" => Some(mime),
        _ => None,
    }
}

// --- Private Anthropic request DTOs ---

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<AnthropicSystem>,
    max_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stop_sequences: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<AnthropicToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<AnthropicThinkingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<AnthropicOutputConfig>,
    /// Declared server-side refusal fallback targets. Omitted (never empty)
    /// when the route declares none, because Anthropic rejects the field for a
    /// model with no permitted fallback target.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    fallbacks: Vec<AnthropicFallback>,
    stream: bool,
}

#[derive(Serialize)]
struct AnthropicFallback {
    model: String,
}

#[derive(Serialize)]
#[serde(untagged)]
enum AnthropicSystem {
    Blocks(Vec<AnthropicSystemBlock>),
}

#[derive(Serialize)]
struct AnthropicSystemBlock {
    r#type: &'static str,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

#[derive(Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
enum AnthropicMessage {
    User {
        content: Vec<AnthropicContentBlock>,
    },
    Assistant {
        content: Vec<AnthropicContentBlock>,
    },
    /// Effort-only system message inserted by the declared mid-conversation
    /// effort capability (`mid-conversation-output-config-2026-07-01`).
    System {
        content: Vec<AnthropicContentBlock>,
        output_config: AnthropicEffortOnly,
    },
}

#[derive(Serialize)]
struct AnthropicEffortOnly {
    effort: String,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    Image {
        source: AnthropicImageSource,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<AnthropicToolResultBlock>,
        is_error: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicImageSource {
    Base64 {
        media_type: String,
        data: Base64Bytes,
    },
    Url {
        url: String,
    },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicToolResultBlock {
    Text { text: String },
    Image { source: AnthropicImageSource },
}

#[derive(Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    /// Emitted by default (`supportsEagerToolInputStreaming ?? true`); a route
    /// that declares `false` omits it and requests the legacy
    /// fine-grained-tool-streaming beta instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    eager_input_streaming: Option<bool>,
    input_schema: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    strict: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicToolChoice {
    Auto,
    Any,
    Tool { name: String },
}

#[derive(Serialize)]
struct AnthropicThinkingConfig {
    r#type: String,
    /// Present only for extended-thinking (`type: "enabled"`) budget control;
    /// omitted for adaptive thinking (`type: "adaptive"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    budget_tokens: Option<u64>,
}

#[derive(Serialize)]
struct AnthropicOutputConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<AnthropicOutputFormat>,
    /// Adaptive-thinking effort level (`low`|`medium`|`high`|`xhigh`|`max`).
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
}

#[derive(Serialize)]
struct AnthropicOutputFormat {
    r#type: String,
    schema: serde_json::Value,
}

/// OAuth bearer variables for the Anthropic Messages route.
///
/// Anthropic documents these as OAuth/subscription token variables rather than
/// API keys. Keying the rule on the *variable* keeps it declarative: a route
/// reusing the variable inherits the behavior, no provider name is consulted.
const ANTHROPIC_OAUTH_BEARER_VARIABLES: [&str; 2] =
    ["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_OAUTH_TOKEN"];

/// Betas a Claude Code client sends when it authenticates with an OAuth token.
const ANTHROPIC_CLAUDE_CODE_BETA: &str = "claude-code-20250219";
const ANTHROPIC_OAUTH_BETA: &str = "oauth-2025-04-20";
/// Interleaved thinking, paired with extended (budget) thinking.
const ANTHROPIC_INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";
/// Legacy per-tool streaming control, used only when the route does not accept
/// per-tool `eager_input_streaming` (`supportsEagerToolInputStreaming: false`).
const ANTHROPIC_FINE_GRAINED_TOOL_STREAMING_BETA: &str = "fine-grained-tool-streaming-2025-05-14";
/// Server-side refusal fallback, used only when the route declares at least one
/// permitted fallback model.
const ANTHROPIC_SERVER_SIDE_FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Mid-conversation effort and its thinking binding control.
const ANTHROPIC_MID_CONVERSATION_OUTPUT_CONFIG_BETA: &str =
    "mid-conversation-output-config-2026-07-01";
const ANTHROPIC_THINKING_BINDING_CONTROLS_BETA: &str = "thinking-binding-controls-2026-08-01";

/// Resolved Anthropic compat for one model: Pi's route defaults with the
/// declaration's overrides applied.
struct AnthropicCompat {
    eager_tool_input_streaming: bool,
    supports_mid_convo_effort: bool,
    force_adaptive_thinking: bool,
    allow_empty_signature: bool,
}

fn anthropic_compat(model: &crate::catalog::Model) -> AnthropicCompat {
    let declared = model.spec.preset.anthropic_compat.as_ref();
    AnthropicCompat {
        eager_tool_input_streaming: declared
            .and_then(|compat| compat.supports_eager_tool_input_streaming)
            .unwrap_or(true),
        supports_mid_convo_effort: declared
            .and_then(|compat| compat.supports_mid_convo_effort)
            .unwrap_or(false),
        force_adaptive_thinking: declared
            .and_then(|compat| compat.force_adaptive_thinking)
            .unwrap_or(false),
        allow_empty_signature: declared
            .and_then(|compat| compat.allow_empty_signature)
            .unwrap_or(false),
    }
}

/// Declared fallback model identifiers, in declaration order.
fn anthropic_fallback_models(model: &crate::catalog::Model) -> Vec<String> {
    model
        .spec
        .preset
        .anthropic_compat
        .as_ref()
        .map(|compat| {
            compat
                .allowed_fallback_models
                .iter()
                .map(|fallback| fallback.model.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the declared route authenticates with an Anthropic OAuth token.
fn route_uses_oauth_bearer(model: &crate::catalog::Model) -> bool {
    matches!(
        &model.endpoint.auth,
        crate::auth::Auth::BearerEnv { var }
            if ANTHROPIC_OAUTH_BEARER_VARIABLES.contains(&var.as_str())
    )
}

/// Beta features inferred from the declared route and the request.
///
/// These are only used when the caller configured no `anthropic-beta` list at
/// all: an explicit caller list stays authoritative and replaces (never merges
/// with) the inferred features, exactly like upstream `getBetaFeatures`.
///
/// Every inferred feature comes from a declared route capability, never from a
/// provider identity: OAuth bearer presentation, tool-enabled routes that do
/// not accept per-tool eager input streaming, extended thinking on a route that
/// does not force adaptive thinking, a declared fallback-model list, and a
/// declared mid-conversation-effort capability.
fn inferred_anthropic_betas(
    model: &crate::catalog::Model,
    extended_thinking: bool,
    tool_enabled: bool,
) -> Vec<&'static str> {
    let compat = anthropic_compat(model);
    let mut betas = Vec::new();
    if route_uses_oauth_bearer(model) {
        betas.push(ANTHROPIC_CLAUDE_CODE_BETA);
        betas.push(ANTHROPIC_OAUTH_BETA);
    }
    if tool_enabled && !compat.eager_tool_input_streaming {
        betas.push(ANTHROPIC_FINE_GRAINED_TOOL_STREAMING_BETA);
    }
    if extended_thinking && !compat.force_adaptive_thinking {
        betas.push(ANTHROPIC_INTERLEAVED_THINKING_BETA);
    }
    if !anthropic_fallback_models(model).is_empty() {
        betas.push(ANTHROPIC_SERVER_SIDE_FALLBACK_BETA);
    }
    if compat.supports_mid_convo_effort {
        betas.push(ANTHROPIC_MID_CONVERSATION_OUTPUT_CONFIG_BETA);
        betas.push(ANTHROPIC_THINKING_BINDING_CONTROLS_BETA);
    }
    betas
}

/// Map a portable reasoning effort onto Anthropic's adaptive-thinking effort
/// scale. Anthropic exposes `low`|`medium`|`high`|`xhigh`|`max` (no `minimal`
/// tier), so `Minimal` folds into `low`.
fn anthropic_effort(effort: crate::types::ReasoningEffort) -> String {
    use crate::types::ReasoningEffort::{High, Low, Max, Medium, Minimal, Ultra, Xhigh};
    match effort {
        Minimal | Low => "low",
        Medium => "medium",
        High => "high",
        Xhigh => "xhigh",
        Max => "max",
        // Anthropic does not currently advertise an Ultra tier. This arm is a
        // defensive fallback for direct codec use; normal request
        // normalization clamps Ultra to the model's advertised maximum.
        Ultra => "max",
    }
    .to_string()
}

// --- Private Anthropic Response / SSE Chunk DTOs ---

#[derive(Deserialize)]
#[serde(tag = "type")]
enum AnthropicSseData {
    #[serde(rename = "message_start")]
    MessageStart { message: AnthropicResponseMessage },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: usize,
        content_block: AnthropicResponseContentBlock,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta {
        index: usize,
        delta: AnthropicResponseDelta,
    },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: usize },
    #[serde(rename = "message_delta")]
    MessageDelta {
        delta: AnthropicResponseMsgDelta,
        #[serde(default)]
        usage: Option<AnthropicResponseUsage>,
    },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "error")]
    Error { error: AnthropicResponseError },
}

#[derive(Deserialize)]
struct AnthropicResponseMessage {
    id: String,
    #[serde(default)]
    model: Option<String>,
    usage: AnthropicResponseUsage,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicResponseContentBlock {
    // Text/Thinking block openers carry a (usually empty) initial payload; the
    // real content arrives via `content_block_delta`, so the payload is unused
    // and the extra wire field is simply ignored on deserialize.
    Text {},
    Thinking {},
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
    },
    /// Server-side fallback marker. A pre-content fallback is transparent; a
    /// mid-output fallback is rejected (see the content block handler).
    Fallback {},
}

// Anthropic content_block_delta uses `*_delta` type tags on the wire (see
// docs/research/apidocs/anthropic-messages/messages.md), NOT bare snake_case.
#[derive(Deserialize)]
#[serde(tag = "type")]
enum AnthropicResponseDelta {
    #[serde(rename = "text_delta")]
    Text { text: String },
    #[serde(rename = "thinking_delta")]
    Thinking { thinking: String },
    #[serde(rename = "signature_delta")]
    Signature { signature: String },
    #[serde(rename = "input_json_delta")]
    InputJson { partial_json: String },
}

#[derive(Deserialize)]
struct AnthropicResponseMsgDelta {
    stop_reason: Option<String>,
    #[serde(default)]
    stop_details: Option<AnthropicRefusalStopDetails>,
}

/// Anthropic `stop_details` for a `refusal` stop reason.
///
/// The explanation is provider prose about *why* the model refused. It is
/// surfaced as a bounded response diagnostic rather than dropped, because a
/// refusal with no explanation is not actionable for the user.
#[derive(Deserialize)]
struct AnthropicRefusalStopDetails {
    #[serde(default)]
    explanation: Option<String>,
}

/// Upper bound on the refusal explanation copied into a diagnostic. Provider
/// prose is untrusted input; anything longer is truncated on a character
/// boundary rather than rejected (the refusal itself already happened).
const MAX_ANTHROPIC_REFUSAL_EXPLANATION_BYTES: usize = 8192;

fn bounded_refusal_explanation(explanation: &str) -> String {
    if explanation.len() <= MAX_ANTHROPIC_REFUSAL_EXPLANATION_BYTES {
        return explanation.to_owned();
    }
    let mut end = MAX_ANTHROPIC_REFUSAL_EXPLANATION_BYTES;
    while end > 0 && !explanation.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &explanation[..end])
}

#[derive(Deserialize)]
struct AnthropicResponseUsage {
    // `input_tokens` appears on `message_start`; `message_delta` usage carries
    // only `output_tokens`. Both default so either shape deserializes.
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation: Option<AnthropicCacheCreation>,
    // `message_delta` usage carries a cumulative `output_tokens_details` with the
    // documented `thinking_tokens` subset (apidocs anthropic-messages
    // messages.md §"Message Delta Usage"). Optional so `message_start` (which
    // omits it) still deserializes.
    #[serde(default)]
    output_tokens_details: Option<AnthropicOutputTokensDetails>,
}

#[derive(Deserialize)]
struct AnthropicCacheCreation {
    #[serde(default)]
    ephemeral_1h_input_tokens: u64,
    #[serde(default)]
    ephemeral_5m_input_tokens: u64,
}

#[derive(Deserialize)]
struct AnthropicOutputTokensDetails {
    #[serde(default)]
    thinking_tokens: u64,
}

#[derive(Deserialize)]
struct AnthropicResponseError {
    r#type: String,
    message: String,
}

// --- Request Builder ---

/// Builds the Anthropic Messages HTTP request parts.
pub(crate) fn build_request(
    model: &crate::catalog::Model,
    req: &Request,
) -> Result<HttpRequestParts, AiError> {
    // 1. Normalize model-gated reasoning, then run validation.
    let req = normalize_request_reasoning(req, &model.spec.capabilities);
    let diagnostics = validate_request(
        &req,
        &model.spec.capabilities,
        &model.spec.limits,
        Protocol::AnthropicMessages,
        &model.spec.id,
        req.compatibility,
    )?;

    // 2. Map system prompt and allocate one provider-compatible breakpoint.
    let compat = anthropic_compat(model);
    let cache_marker = cache_control(&req, &model.spec.cache);
    let system = req.system.as_ref().map(|system| {
        AnthropicSystem::Blocks(vec![AnthropicSystemBlock {
            r#type: "text",
            text: system.clone(),
            cache_control: cache_marker,
        }])
    });

    // 3. Map messages with alternation merging
    let mut messages = Vec::new();
    let mut pending_tool_calls = std::collections::BTreeSet::new();
    let mut synthetic_tool_results = std::collections::HashSet::new();
    for msg in &req.messages {
        match msg {
            Message::User(ref user) => {
                let mut blocks = Vec::new();
                for part in &user.content {
                    match part {
                        UserPart::Text(ref text) => {
                            blocks.push(AnthropicContentBlock::Text {
                                text: text.clone(),
                                cache_control: None,
                            });
                        }
                        UserPart::Media(Media::Image(ref image)) => {
                            if !model
                                .spec
                                .capabilities
                                .input_modalities
                                .contains(crate::types::Modality::Image)
                            {
                                continue;
                            }

                            let source = match &image.source {
                                ImageSource::Inline(bytes) => {
                                    let Some(media_type) = anthropic_image_media_type(image) else {
                                        continue;
                                    };
                                    AnthropicImageSource::Base64 {
                                        media_type,
                                        data: Base64Bytes::from(bytes),
                                    }
                                }
                                ImageSource::Url(url) => AnthropicImageSource::Url {
                                    url: url.to_string(),
                                },
                                ImageSource::ProviderRef(_) => continue,
                            };
                            blocks.push(AnthropicContentBlock::Image {
                                source,
                                cache_control: None,
                            });
                        }
                        UserPart::Media(Media::Audio(_)) => {}
                        UserPart::ToolResult(ref tr) => {
                            if synthetic_tool_results.contains(&tr.tool_call_id.0) {
                                continue;
                            }
                            pending_tool_calls.remove(&tr.tool_call_id.0);
                            let mut tool_blocks = Vec::new();
                            for tr_part in &tr.content {
                                match tr_part {
                                    ToolResultPart::Text(ref text) => {
                                        tool_blocks.push(AnthropicToolResultBlock::Text {
                                            text: text.clone(),
                                        });
                                    }
                                    ToolResultPart::Media(Media::Image(ref image)) => {
                                        let source = match &image.source {
                                            ImageSource::Inline(bytes) => {
                                                anthropic_image_media_type(image).map(
                                                    |media_type| AnthropicImageSource::Base64 {
                                                        media_type,
                                                        data: Base64Bytes::from(bytes),
                                                    },
                                                )
                                            }
                                            ImageSource::Url(url) => {
                                                Some(AnthropicImageSource::Url {
                                                    url: url.to_string(),
                                                })
                                            }
                                            ImageSource::ProviderRef(_) => None,
                                        };
                                        if let Some(source) = source {
                                            tool_blocks
                                                .push(AnthropicToolResultBlock::Image { source });
                                        }
                                    }
                                    ToolResultPart::Media(Media::Audio(_)) => {}
                                }
                            }
                            blocks.push(AnthropicContentBlock::ToolResult {
                                tool_use_id: crate::protocol::normalize_tool_call_id(
                                    &tr.tool_call_id.0,
                                ),
                                content: tool_blocks,
                                is_error: tr.is_error,
                                cache_control: None,
                            });
                        }
                    }
                }

                if !blocks.is_empty() {
                    if let Some(AnthropicMessage::User { ref mut content }) = messages.last_mut() {
                        content.extend(blocks);
                    } else {
                        messages.push(AnthropicMessage::User { content: blocks });
                    }
                }
            }
            Message::Assistant(ref assistant) => {
                if req.compatibility == crate::CompatibilityMode::Lossy {
                    push_synthetic_tool_results(
                        &mut messages,
                        &mut pending_tool_calls,
                        &mut synthetic_tool_results,
                    );
                }
                let mut blocks = Vec::new();
                for part in &assistant.content {
                    match part {
                        AssistantPart::Text(ref text) => {
                            blocks.push(AnthropicContentBlock::Text {
                                text: text.clone(),
                                cache_control: None,
                            });
                        }
                        AssistantPart::ToolCall(ref tc) => {
                            pending_tool_calls.insert(tc.id.0.clone());
                            let input_val: serde_json::Value =
                                serde_json::from_str(&tc.arguments_json).map_err(|e| {
                                    AiError::Decode(DecodeError::Json(e.to_string()))
                                })?;
                            blocks.push(AnthropicContentBlock::ToolUse {
                                id: crate::protocol::normalize_tool_call_id(&tc.id.0),
                                name: tc.name.clone(),
                                input: input_val,
                            });
                        }
                        AssistantPart::Reasoning(ref reasoning) => {
                            if let Some(ref state) = reasoning.state {
                                if state.protocol == Protocol::AnthropicMessages
                                    && state.model == model.spec.id
                                {
                                    match &state.kind {
                                        ReasoningStateKind::AnthropicSignature { signature } => {
                                            if let Some(text) = &reasoning.text {
                                                let has_signature = !signature.trim().is_empty();
                                                if !has_signature && text.trim().is_empty() {
                                                    // Nothing to replay; an empty
                                                    // thinking block is never sent.
                                                } else if !has_signature {
                                                    // An empty signature (e.g. an
                                                    // aborted stream) is only a
                                                    // thinking block on a route
                                                    // that declares it accepts one;
                                                    // otherwise it becomes text.
                                                    if compat.allow_empty_signature {
                                                        blocks.push(
                                                            AnthropicContentBlock::Thinking {
                                                                thinking: text.clone(),
                                                                signature: String::new(),
                                                            },
                                                        );
                                                    } else {
                                                        blocks.push(AnthropicContentBlock::Text {
                                                            text: text.clone(),
                                                            cache_control: None,
                                                        });
                                                    }
                                                } else {
                                                    blocks.push(AnthropicContentBlock::Thinking {
                                                        thinking: text.clone(),
                                                        signature: signature.clone(),
                                                    });
                                                }
                                            }
                                        }
                                        ReasoningStateKind::AnthropicRedacted { data } => {
                                            blocks.push(AnthropicContentBlock::RedactedThinking {
                                                data: data.clone(),
                                            });
                                        }
                                        ReasoningStateKind::OpenAiReasoning { .. } => {}
                                    }
                                }
                            }
                        }
                        AssistantPart::Media(_) => {}
                        AssistantPart::ProviderMetadata(_) => {}
                    }
                }

                if !blocks.is_empty() {
                    if let Some(AnthropicMessage::Assistant { ref mut content }) =
                        messages.last_mut()
                    {
                        content.extend(blocks);
                    } else {
                        messages.push(AnthropicMessage::Assistant { content: blocks });
                    }
                }
            }
        }
    }

    if req.compatibility == crate::CompatibilityMode::Lossy {
        push_synthetic_tool_results(
            &mut messages,
            &mut pending_tool_calls,
            &mut synthetic_tool_results,
        );
    }

    // A normal request marks its final user block. A one-off cache warm has a
    // synthetic final user turn: mark the preceding canonical message instead,
    // so a subsequent real prompt can reuse the warmed conversation prefix.
    if let Some(marker) = cache_marker {
        let target = if req.cache_retention == CacheRetention::WarmShort {
            messages.iter_mut().rev().nth(1)
        } else {
            messages.last_mut()
        };
        let content = match target {
            Some(AnthropicMessage::User { content } | AnthropicMessage::Assistant { content }) => {
                Some(content)
            }
            Some(AnthropicMessage::System { .. }) | None => None,
        };
        if let Some(block) = content.and_then(|content| content.last_mut()) {
            set_content_cache_control(block, marker);
        }
    }

    // 4. Map tools & tool_choice
    let tools_opt = if req.tools.is_empty()
        || !model.spec.capabilities.tools
        || matches!(req.tool_choice, ToolChoice::None)
    {
        None
    } else {
        let mut built = Vec::with_capacity(req.tools.len());
        for (index, t) in req.tools.iter().enumerate() {
            // Strict JSON-schema constrained sampling rewrites the tool input
            // schema into Anthropic's enforced subset; otherwise the canonical
            // schema is sent unchanged.
            let (parameters, strict) = crate::constrained_sampling::function_tool_parameters(
                t,
                super::strict_mode_for(model),
            )?;
            let input_schema = if strict {
                parameters
            } else {
                t.parameters.clone()
            };
            built.push(AnthropicTool {
                name: t.name.clone(),
                description: t.description.clone(),
                eager_input_streaming: compat.eager_tool_input_streaming.then_some(true),
                input_schema,
                strict: strict.then_some(true),
                cache_control: (index + 1 == req.tools.len()
                    && model.spec.cache.supports_cache_control_on_tools)
                    .then_some(cache_marker)
                    .flatten(),
            });
        }
        Some(built)
    };
    // Captured before the request literal below takes ownership of `tools_opt`:
    // the inferred anthropic-beta list is decided from whether tools are present
    // at all, not from the moved value.
    let tools_present = tools_opt.as_ref().is_some_and(|tools| !tools.is_empty());

    let tool_choice_opt = if !model.spec.capabilities.tools {
        None
    } else {
        match &req.tool_choice {
            ToolChoice::Auto => Some(AnthropicToolChoice::Auto),
            ToolChoice::Required => Some(AnthropicToolChoice::Any),
            ToolChoice::None => None,
            ToolChoice::Named(name) => Some(AnthropicToolChoice::Tool { name: name.clone() }),
        }
    };

    // 5. Thinking / reasoning config.
    //
    // Effort-controlled models use adaptive thinking: `thinking: {type:
    // "adaptive"}` plus `output_config.effort`. Budget-controlled models use
    // extended thinking: `thinking: {type: "enabled", budget_tokens: N}`. The
    // two paths are mutually exclusive per model.
    let mut thinking_opt: Option<AnthropicThinkingConfig> = None;
    let mut effort_opt: Option<String> = None;
    if let Some(cap) = model.spec.capabilities.reasoning.as_ref() {
        match req.reasoning {
            ReasoningConfig::Off => {
                thinking_opt = Some(AnthropicThinkingConfig {
                    r#type: "disabled".to_owned(),
                    budget_tokens: None,
                });
            }
            ReasoningConfig::On if cap.control == crate::types::ReasoningControl::Toggle => {
                thinking_opt = Some(AnthropicThinkingConfig {
                    r#type: "enabled".to_owned(),
                    budget_tokens: None,
                });
            }
            ReasoningConfig::On => {}
            ReasoningConfig::Effort(effort) => match cap.control {
                crate::types::ReasoningControl::Effort => {
                    thinking_opt = Some(AnthropicThinkingConfig {
                        r#type: "adaptive".to_string(),
                        budget_tokens: None,
                    });
                    effort_opt = Some(if cap.options.is_some() {
                        cap.wire_value(&req.reasoning)
                            .expect("validated effort choice")
                    } else {
                        anthropic_effort(effort)
                    });
                }
                crate::types::ReasoningControl::TokenBudget => {
                    if let Some(b) = cap.effort_budgets.as_ref() {
                        let limit = match effort {
                            crate::types::ReasoningEffort::Minimal => b.minimal,
                            crate::types::ReasoningEffort::Low => b.low,
                            crate::types::ReasoningEffort::Medium => b.medium,
                            crate::types::ReasoningEffort::High => b.high,
                            crate::types::ReasoningEffort::Xhigh => b.xhigh,
                            crate::types::ReasoningEffort::Max
                            | crate::types::ReasoningEffort::Ultra => b.max,
                        };
                        thinking_opt = Some(AnthropicThinkingConfig {
                            r#type: "enabled".to_string(),
                            budget_tokens: Some(limit),
                        });
                    }
                }
                crate::types::ReasoningControl::AlwaysOn
                | crate::types::ReasoningControl::Toggle => {}
            },
            ReasoningConfig::Budget(b) => {
                thinking_opt = Some(AnthropicThinkingConfig {
                    r#type: "enabled".to_string(),
                    budget_tokens: Some(b),
                });
            }
        }
    }

    // Design §7: a Lossy structured-output downgrade must drop the capability
    // from the wire, not merely emit a diagnostic. Strict mode already errored in
    // `validate_request`, so an unsupported format reaching here is Lossy and is
    // serialized as plain text (format omitted). Effort and format share one
    // `output_config` object.
    let format_opt = match &req.output_format {
        crate::types::OutputFormat::JsonSchema(schema)
            if model.spec.capabilities.structured_output =>
        {
            Some(AnthropicOutputFormat {
                r#type: "json_schema".to_string(),
                schema: schema.schema.clone(),
            })
        }
        _ => None,
    };

    // Extended (budget) thinking is the only mode upstream pairs with the
    // interleaved-thinking beta: adaptive thinking is already interleaved by
    // construction, and a disabled/toggle-off request sends no beta at all.
    let extended_thinking = matches!(&thinking_opt, Some(config) if config.r#type == "enabled");

    let output_config = if format_opt.is_some() || effort_opt.is_some() {
        Some(AnthropicOutputConfig {
            format: format_opt,
            effort: effort_opt.clone(),
        })
    } else {
        None
    };

    // Mid-conversation effort appends an effort-only system message carrying
    // the active level, exactly like upstream's `insertThinkingLevelMessages`.
    // Historical per-turn levels require `AssistantMessage.providerThinkingLevel`
    // (row 1c.10), so only the active level is inserted today; the route's
    // declaration is the admission gate for the beta pair.
    if compat.supports_mid_convo_effort {
        messages.push(AnthropicMessage::System {
            content: Vec::new(),
            output_config: AnthropicEffortOnly {
                effort: effort_opt.clone().unwrap_or_else(|| "high".to_owned()),
            },
        });
    }

    // 6. Max tokens
    let max_tokens = crate::effective_output_token_cap(model, req.max_output_tokens)
        .expect("Anthropic always emits an output cap");

    let anth_req = AnthropicRequest {
        model: model.spec.api_name.clone(),
        messages,
        system,
        max_tokens,
        temperature: req.temperature,
        stop_sequences: req.stop.clone(),
        tools: tools_opt,
        tool_choice: tool_choice_opt,
        thinking: thinking_opt,
        output_config,
        fallbacks: anthropic_fallback_models(model)
            .into_iter()
            .map(|model| AnthropicFallback { model })
            .collect(),
        stream: true,
    };

    let body_bytes = serde_json::to_vec(&anth_req)
        .map_err(|e| AiError::Decode(DecodeError::Json(e.to_string())))?;

    let url = crate::protocol::endpoint_url(&model.endpoint.base_url, "messages")?;

    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::HeaderName::from_static("anthropic-version"),
        http::HeaderValue::from_static("2023-06-01"),
    );
    // Current Pi treats an explicit caller beta list as authoritative (not
    // additive to inferred defaults). Normalize repeated/comma-separated values
    // without dropping caller features or introducing OAuth/provider defaults.
    // When the caller configured no list at all, the route's inferred features
    // are used instead (see `inferred_anthropic_betas`).
    let configured_betas = model.endpoint.default_headers.get_all("anthropic-beta");
    let mut betas = Vec::new();
    for value in configured_betas.iter() {
        let value = value
            .to_str()
            .map_err(|_| ConfigError::InvalidHeader("anthropic-beta".into()))?;
        for beta in value
            .split(',')
            .map(str::trim)
            .filter(|beta| !beta.is_empty())
        {
            if !betas.contains(&beta) {
                betas.push(beta);
            }
        }
    }
    if model
        .endpoint
        .default_headers
        .contains_key("anthropic-beta")
    {
        headers.insert(
            http::HeaderName::from_static("anthropic-beta"),
            http::HeaderValue::from_str(&betas.join(","))
                .map_err(|_| ConfigError::InvalidHeader("anthropic-beta".into()))?,
        );
    } else {
        let inferred = inferred_anthropic_betas(model, extended_thinking, tools_present);
        if !inferred.is_empty() {
            headers.insert(
                http::HeaderName::from_static("anthropic-beta"),
                http::HeaderValue::from_str(&inferred.join(","))
                    .map_err(|_| ConfigError::InvalidHeader("anthropic-beta".into()))?,
            );
        }
    }
    if model.spec.cache.send_session_affinity_headers
        && !crate::protocol::is_opencode_session_route(model)
    {
        if let Some(session_id) = crate::protocol::cache_session_id(&req) {
            let value = http::HeaderValue::from_str(session_id)
                .map_err(|_| ConfigError::InvalidHeader("x-session-affinity".into()))?;
            headers.insert(http::HeaderName::from_static("x-session-affinity"), value);
        }
    }
    crate::protocol::add_opencode_session_header(model, &req, &mut headers)?;

    Ok(HttpRequestParts {
        url,
        headers,
        body: bytes::Bytes::from(body_bytes),
        streaming: true,
        diagnostics,
    })
}

fn set_content_cache_control(block: &mut AnthropicContentBlock, marker: CacheControl) {
    match block {
        AnthropicContentBlock::Text { cache_control, .. }
        | AnthropicContentBlock::Image { cache_control, .. }
        | AnthropicContentBlock::ToolResult { cache_control, .. } => {
            *cache_control = Some(marker);
        }
        AnthropicContentBlock::ToolUse { .. }
        | AnthropicContentBlock::Thinking { .. }
        | AnthropicContentBlock::RedactedThinking { .. } => {}
    }
}

fn push_synthetic_tool_results(
    messages: &mut Vec<AnthropicMessage>,
    pending: &mut std::collections::BTreeSet<String>,
    synthetic_ids: &mut std::collections::HashSet<String>,
) {
    let synthetic: Vec<_> = std::mem::take(pending)
        .into_iter()
        .map(|call_id| {
            synthetic_ids.insert(call_id.clone());
            AnthropicContentBlock::ToolResult {
                // Wire write: normalize to match the paired `tool_use` above.
                tool_use_id: crate::protocol::normalize_tool_call_id(&call_id),
                content: vec![AnthropicToolResultBlock::Text {
                    text: "Tool execution result was not supplied by the caller.".to_string(),
                }],
                is_error: true,
                cache_control: None,
            }
        })
        .collect();
    if !synthetic.is_empty() {
        if let Some(AnthropicMessage::User { content }) = messages.last_mut() {
            content.extend(synthetic);
        } else {
            messages.push(AnthropicMessage::User { content: synthetic });
        }
    }
}

// --- SSE Stream Decoder ---
//
// Anthropic Messages is always streamed (design §12.3); there is no
// non-streaming decode path, so this codec deliberately exposes none.

/// Decodes a streaming SSE event from Anthropic, emitting StreamEvents.
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

    let data: AnthropicSseData = serde_json::from_str(raw_data)
        .map_err(|e| AiError::Decode(DecodeError::Json(e.to_string())))?;

    let mut events = Vec::new();

    match data {
        AnthropicSseData::MessageStart { message } => {
            // A relay's model label is not authority to rebind an ordinary
            // response. Only a route that declared server-side fallbacks may
            // attribute a different terminal model. Match its price by exact
            // declared target, never by the requested model's tariff or a
            // similarly named catalog entry.
            if let (Some(terminal), Some(compat)) = (
                message.model.as_deref(),
                model.spec.preset.anthropic_compat.as_ref(),
            ) {
                if terminal != model.spec.api_name && !compat.allowed_fallback_models.is_empty() {
                    // The echoed id has no separate catalog validation. Keep
                    // it within the same bound as a declared fallback target.
                    if terminal.trim().is_empty() || terminal.len() > 256 {
                        return Err(AiError::Decode(DecodeError::Json(
                            "invalid terminal fallback model".into(),
                        )));
                    }
                    let mut matches = compat
                        .allowed_fallback_models
                        .iter()
                        .filter(|fallback| fallback.model == terminal);
                    builder.pricing = match (matches.next(), matches.next()) {
                        (Some(fallback), None) => {
                            fallback.cost.as_ref().and_then(|cost| cost.pricing())
                        }
                        _ => None,
                    };
                    builder.model = crate::types::ModelId(terminal.to_owned());
                }
            }
            builder.response_id = Some(message.id.clone());
            emit_event(
                &mut events,
                builder,
                StreamEvent::Started {
                    response_id: Some(message.id),
                },
            )?;

            // Store initial usage input tokens
            let u = map_usage(&message.usage)?;
            builder.usage = Some(u);
        }
        AnthropicSseData::ContentBlockStart {
            index,
            content_block,
        } => {
            let key = format!("block_{}", index);
            let canonical_idx = get_canonical_index(builder, &key);

            match content_block {
                AnthropicResponseContentBlock::Text { .. } => {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::TextStart {
                            index: canonical_idx,
                        },
                    )?;
                }
                AnthropicResponseContentBlock::Thinking { .. } => {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ReasoningStart {
                            index: canonical_idx,
                        },
                    )?;
                }
                AnthropicResponseContentBlock::RedactedThinking { data } => {
                    // Opaque, no-visible-text reasoning (design §6.3, §12.3): open a
                    // reasoning part and attach the redacted continuation state; it
                    // is never flattened into visible text.
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ReasoningStart {
                            index: canonical_idx,
                        },
                    )?;
                    builder.set_reasoning_state(
                        canonical_idx,
                        ReasoningState {
                            model: builder.model.clone(),
                            protocol: Protocol::AnthropicMessages,
                            kind: ReasoningStateKind::AnthropicRedacted { data },
                        },
                    )?;
                }
                AnthropicResponseContentBlock::ToolUse { id, name } => {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ToolCallStart {
                            async_execution: false,
                            index: canonical_idx,
                            id: ToolCallId(id),
                            name,
                        },
                    )?;
                }
                AnthropicResponseContentBlock::Fallback {} => {
                    // A server-side fallback before any content is transparent:
                    // the replacement model produces the same turn, and the
                    // marker block carries no content. Once content has already
                    // streamed, the two models' output cannot be merged into one
                    // assistant turn, so fail closed instead of corrupting it.
                    if !builder.observed_indices.is_empty() {
                        return Err(UnsupportedError::MidOutputModelFallback.into());
                    }
                    // Without a distinct terminal model in message_start, the
                    // marker cannot identify which tariff to apply.
                    if builder.model == model.spec.id {
                        builder.pricing = None;
                    }
                }
            }
        }
        AnthropicSseData::ContentBlockDelta { index, delta } => {
            let key = format!("block_{}", index);
            let canonical_idx = get_canonical_index(builder, &key);

            match delta {
                AnthropicResponseDelta::Text { text } => {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::TextDelta {
                            index: canonical_idx,
                            delta: text,
                        },
                    )?;
                }
                AnthropicResponseDelta::Thinking { thinking } => {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ReasoningDelta {
                            index: canonical_idx,
                            delta: thinking,
                        },
                    )?;
                }
                AnthropicResponseDelta::Signature { signature } => {
                    let sig_key = format!("sig_{}", index);
                    builder.append_temp_buffer(sig_key, &signature)?;
                }
                AnthropicResponseDelta::InputJson { partial_json } => {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ToolCallArgsDelta {
                            index: canonical_idx,
                            delta: partial_json,
                        },
                    )?;
                }
            }
        }
        AnthropicSseData::ContentBlockStop { index } => {
            let key = format!("block_{}", index);
            let canonical_idx = get_canonical_index(builder, &key);

            // Determine if it was text, reasoning, or tool. A gateway that
            // duplicates `content_block_stop` must not re-emit the End event
            // (§8: exactly one *End per part).
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
            } else if builder.reasoning_text_buffers.contains_key(&canonical_idx)
                && !builder.ended_indices.contains(&canonical_idx)
            {
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::ReasoningEnd {
                        index: canonical_idx,
                    },
                )?;

                // Check signature
                let sig_key = format!("sig_{}", index);
                if let Some(sig) = builder.take_temp_buffer(&sig_key) {
                    builder.set_reasoning_state(
                        canonical_idx,
                        ReasoningState {
                            model: builder.model.clone(),
                            protocol: Protocol::AnthropicMessages,
                            kind: ReasoningStateKind::AnthropicSignature { signature: sig },
                        },
                    )?;
                }
            } else if builder.tool_call_builders.contains_key(&canonical_idx)
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
        AnthropicSseData::MessageDelta { delta, usage } => {
            if let Some(ref reason) = delta.stop_reason {
                let stop = map_stop_reason(reason);
                builder.set_stop_reason(stop);
            }
            // Upstream maps Anthropic `refusal` + `stop_details.explanation` onto
            // the assistant message's error text. The canonical `Response` has no
            // error-message field yet (roadmap 1c.10), so the explanation is
            // carried by the response's diagnostics channel instead of being
            // dropped: a refusal nobody can explain is not actionable.
            if let Some(explanation) = delta
                .stop_details
                .as_ref()
                .and_then(|details| details.explanation.as_deref())
                .map(str::trim)
                .filter(|explanation| !explanation.is_empty())
            {
                builder.add_diagnostic(crate::error::Diagnostic {
                    code: "anthropic_refusal".to_owned(),
                    message: bounded_refusal_explanation(explanation),
                });
            }

            if let Some(u_dto) = usage {
                // `message_delta` usage is cumulative and authoritative (apidocs
                // MessageDeltaUsage): it carries the final output count and may
                // also restate input/cache buckets and the thinking-token subset.
                // Merge every field the delta actually reports; fall back to the
                // `message_start` baseline only where the delta omits a value.
                let delta_usage = map_usage(&u_dto)?;
                match builder.usage.as_mut() {
                    Some(existing) => {
                        existing.output_tokens = delta_usage.output_tokens;
                        if delta_usage.input_tokens > 0 {
                            existing.input_tokens = delta_usage.input_tokens;
                        }
                        if delta_usage.cache_read_tokens > 0 {
                            existing.cache_read_tokens = delta_usage.cache_read_tokens;
                        }
                        if delta_usage.cache_write_tokens > 0 {
                            existing.cache_write_tokens = delta_usage.cache_write_tokens;
                            existing.cache_write_1h_tokens = delta_usage.cache_write_1h_tokens;
                        }
                        if delta_usage.reasoning_tokens > 0 {
                            existing.reasoning_tokens = delta_usage.reasoning_tokens;
                        }
                        if existing.reasoning_tokens > existing.output_tokens {
                            return Err(AiError::Decode(DecodeError::UsageUnderflow));
                        }
                        existing.total_tokens = existing
                            .input_tokens
                            .checked_add(existing.cache_read_tokens)
                            .and_then(|value| value.checked_add(existing.cache_write_tokens))
                            .and_then(|value| value.checked_add(existing.output_tokens))
                            .ok_or(AiError::Decode(DecodeError::UsageUnderflow))?;
                    }
                    None => builder.usage = Some(delta_usage),
                }
            }
        }
        AnthropicSseData::MessageStop => {
            if builder.model != model.spec.id
                && builder
                    .usage
                    .is_some_and(|usage| usage.cache_write_1h_tokens > 0)
            {
                builder.pricing = None;
            }
            // Send final usage if we have one
            if let Some(u) = builder.usage {
                emit_event(&mut events, builder, StreamEvent::Usage(u))?;
            }

            let resp = builder.finish_mut()?;
            emit_event(&mut events, builder, StreamEvent::Finished(resp))?;
        }
        AnthropicSseData::Ping => {}
        AnthropicSseData::Error { error } => {
            return Err(AiError::Provider(ProviderError {
                code: None,
                kind: Some(error.r#type),
                message: error.message,
                request_id: None,
            }));
        }
    }

    Ok(events)
}

// --- Helpers ---

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "tool_use" => StopReason::ToolUse,
        "pause_turn" => StopReason::PauseTurn,
        "refusal" => StopReason::Refusal,
        other => StopReason::Other(other.to_string()),
    }
}

fn map_usage(usage: &AnthropicResponseUsage) -> Result<Usage, AiError> {
    // Design §15: Anthropic `input_tokens` ALREADY excludes cache, so it maps
    // directly (no subtraction — that is the OpenAI rule). `total` includes cache.
    let cache_read = usage.cache_read_input_tokens.unwrap_or(0);
    let cache_write_1h = usage
        .cache_creation
        .as_ref()
        .map(|c| c.ephemeral_1h_input_tokens)
        .unwrap_or(0);
    let cache_write = usage
        .cache_creation_input_tokens
        .or_else(|| {
            usage.cache_creation.as_ref().and_then(|c| {
                c.ephemeral_1h_input_tokens
                    .checked_add(c.ephemeral_5m_input_tokens)
            })
        })
        .unwrap_or(0);
    let input = usage.input_tokens;
    let total_tokens = input
        .checked_add(cache_read)
        .and_then(|value| value.checked_add(cache_write))
        .and_then(|value| value.checked_add(usage.output_tokens))
        .ok_or(AiError::Decode(DecodeError::UsageUnderflow))?;
    if cache_write_1h > cache_write {
        return Err(AiError::Decode(DecodeError::UsageUnderflow));
    }
    // `thinking_tokens` is the documented reasoning subset of `output_tokens`
    // (apidocs; always ≤ output_tokens). Absent on `message_start`, reported on
    // `message_delta`. Resolves the design §15 "reasoning=0" note in favor of the
    // checked-in API docs (which establish the wire mapping, design §0/§7).
    let reasoning = usage
        .output_tokens_details
        .as_ref()
        .map(|d| d.thinking_tokens)
        .unwrap_or(0);
    if reasoning > usage.output_tokens {
        return Err(AiError::Decode(DecodeError::UsageUnderflow));
    }
    Ok(Usage {
        input_tokens: input,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        cache_write_1h_tokens: cache_write_1h,
        output_tokens: usage.output_tokens,
        reasoning_tokens: reasoning,
        total_tokens,
    })
}

// --- Tests ---

#[cfg(test)]
mod tests;

/// Offline fixture matrix for the Anthropic Messages stream decoder
/// (design §19; plan Task 10.2).
#[cfg(test)]
mod fixture_tests;
