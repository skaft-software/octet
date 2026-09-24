//! Declarative provider/model preset metadata.
//!
//! Provider presets are **data**, not code: a new OpenAI/Anthropic-compatible
//! endpoint is described here and consumed by a codec, so provider identity
//! never branches the request loop. This module owns the typed, validated
//! shape of the parity-relevant preset fields that upstream `pi` carries on
//! its `Provider`/`Model` objects:
//!
//! - per-model [`ModelPreset::sampling_params`] and [`ModelPreset::headers`];
//! - `vllmPriority`, `supportsMaxOutputTokens`, `thinkingTokenBudgetField`;
//! - `chatTemplateArgs` / `chatTemplateKwargs` with `{ "$var": ... }`
//!   interpolation and the `string-thinking` thinking format;
//! - provider credential environment aliases such as `ANTHROPIC_AUTH_TOKEN`,
//!   `ANTHROPIC_OAUTH_TOKEN` and `GOOGLE_CLOUD_API_KEY`;
//! - per-route/per-model capability compat records: `supportsStrictMode`,
//!   `supportsOpenAIGrammarTools` and the Anthropic Messages record
//!   (eager tool-input streaming, strict tools, mid-conversation effort,
//!   empty-signature replay, allowed fallback models with local pricing);
//! - request-local Codex transport selection and connect deadline.
//!
//! This is deliberately description only. It performs no network, filesystem or
//! credential access, and it never weakens an existing host auth policy. The
//! values are fail-closed: unknown `$var` names, malformed headers, empty
//! identifiers and oversized sampling values are rejected by
//! [`ModelPreset::validate`] and [`ProviderCredentialPreset::validate`] rather
//! than silently ignored.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub(crate) mod azure;
pub mod bedrock;
pub mod codex;
pub mod proxy;
pub mod radius;
pub use azure::AzureRequestOptions;

/// Maximum serialized size of a single declaration object.
pub const MAX_DECLARATION_BYTES: usize = 64 * 1024;

/// Maximum accepted `maxRetries` value (matching bounded client retry budgets).
pub const MAX_RETRIES_CEILING: u32 = 10;

/// Wire field used by vLLM for a reasoning-token budget.
pub const VLLM_THINKING_TOKEN_BUDGET_FIELD: &str = "thinking_token_budget";

/// Failure returned by declaration validation.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DeclarationError {
    /// The serialized declaration exceeded [`MAX_DECLARATION_BYTES`].
    #[error("declaration exceeds the {max}-byte limit")]
    TooLarge {
        /// Maximum accepted serialized size in bytes.
        max: usize,
    },
    /// A configured header name is not a valid HTTP token.
    #[error("invalid declaration header name: {0}")]
    InvalidHeaderName(String),
    /// A configured header value is not a valid HTTP header value.
    #[error("invalid declaration header value for {name}")]
    InvalidHeaderValue {
        /// Offending header name.
        name: String,
    },
    /// A `chatTemplateArgs`/`chatTemplateKwargs` value used an unknown `$var`.
    #[error("unknown chat-template $var in {name}")]
    UnknownChatTemplateVariable {
        /// The template key carrying the unknown variable.
        name: String,
    },
    /// A generic, human-readable validation failure.
    #[error("{0}")]
    Invalid(String),
}

fn check_headers(headers: &BTreeMap<String, String>) -> Result<(), DeclarationError> {
    let mut names = std::collections::BTreeSet::new();
    for (name, value) in headers {
        let parsed = http::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| DeclarationError::InvalidHeaderName(name.clone()))?;
        if !names.insert(parsed.as_str().to_owned())
            || matches!(
                parsed.as_str(),
                "host"
                    | "content-length"
                    | "transfer-encoding"
                    | "connection"
                    | "upgrade"
                    | "proxy-authorization"
                    | "content-encoding"
            )
            || parsed.as_str().starts_with("sec-websocket-")
        {
            return Err(DeclarationError::Invalid(
                "duplicate or transport-owned request header".into(),
            ));
        }
        http::HeaderValue::from_str(value)
            .map_err(|_| DeclarationError::InvalidHeaderValue { name: name.clone() })?;
    }
    Ok(())
}

fn check_object_size(value: &impl Serialize) -> Result<(), DeclarationError> {
    if serde_json::to_vec(value)
        .map_err(|_| DeclarationError::Invalid("invalid declaration".into()))?
        .len()
        > MAX_DECLARATION_BYTES
    {
        return Err(DeclarationError::TooLarge {
            max: MAX_DECLARATION_BYTES,
        });
    }
    Ok(())
}

pub(crate) fn check_sampling(
    values: &BTreeMap<String, serde_json::Value>,
) -> Result<(), DeclarationError> {
    // Deliberately closed: new provider controls need explicit admission here.
    // An unknown key must never become a tool/prompt/billing side channel.
    for (name, value) in values {
        let number_in = |min: f64, max: f64| {
            value
                .as_f64()
                .is_some_and(|v| v.is_finite() && v >= min && v <= max)
        };
        let valid = match name.as_str() {
            "temperature" => number_in(0.0, 2.0),
            "top_p" | "min_p" | "typical_p" => number_in(0.0, 1.0),
            "frequency_penalty" | "presence_penalty" => number_in(-2.0, 2.0),
            "repetition_penalty" => value.as_f64().is_some_and(|v| v.is_finite() && v > 0.0),
            "top_k" => value
                .as_i64()
                .is_some_and(|v| (-1..=i64::from(i32::MAX)).contains(&v)),
            "seed" => value.as_i64().is_some(),
            "logprobs" => value.is_boolean(),
            "top_logprobs" => value.as_u64().is_some_and(|v| v <= 20),
            "logit_bias" => value.as_object().is_some_and(|values| {
                values.iter().all(|(key, value)| {
                    !key.is_empty()
                        && key.bytes().all(|b| b.is_ascii_digit())
                        && value
                            .as_f64()
                            .is_some_and(|v| v.is_finite() && (-100.0..=100.0).contains(&v))
                })
            }),
            "stop" => {
                value.is_string()
                    || value
                        .as_array()
                        .is_some_and(|values| values.iter().all(serde_json::Value::is_string))
            }
            _ => false,
        };
        if !valid {
            return Err(DeclarationError::Invalid(
                "unsupported or invalid sampling parameter".into(),
            ));
        }
    }
    Ok(())
}

/// Why a fallback model attribute can diverge from a models.dev snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingFormat {
    /// OpenAI-style top-level `reasoning_effort`.
    #[serde(rename = "openai")]
    OpenAi,
    /// OpenRouter `reasoning: { effort }`.
    #[serde(rename = "openrouter")]
    OpenRouter,
    /// DeepSeek `thinking: { type }` plus `reasoning_effort` when supported.
    #[serde(rename = "deepseek")]
    DeepSeek,
    /// Together `reasoning: { enabled }` plus `reasoning_effort` when supported.
    #[serde(rename = "together")]
    Together,
    /// Baseten configurable `chat_template_args` plus `reasoning_effort`.
    #[serde(rename = "baseten")]
    Baseten,
    /// z.ai `thinking: { type }`.
    #[serde(rename = "zai")]
    Zai,
    /// Qwen/DashScope top-level `enable_thinking: bool`.
    #[serde(rename = "qwen")]
    Qwen,
    /// Generic configurable `chat_template_kwargs`.
    #[serde(rename = "chat-template")]
    ChatTemplate,
    /// Qwen `chat_template_kwargs.enable_thinking` plus `preserve_thinking`.
    #[serde(rename = "qwen-chat-template")]
    QwenChatTemplate,
    /// Top-level `thinking` string (string-valued thinking).
    #[serde(rename = "string-thinking")]
    StringThinking,
    /// ant-ling `reasoning: { effort }` only when the mapped effort is non-null.
    #[serde(rename = "ant-ling")]
    AntLing,
}

impl ThinkingFormat {
    /// Whether this format emits a top-level string `thinking` field.
    pub const fn is_string_thinking(self) -> bool {
        matches!(self, Self::StringThinking)
    }
}

/// Top-level request field used to cap reasoning tokens from a thinking budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThinkingTokenBudgetField {
    /// vLLM `thinking_token_budget`.
    #[serde(rename = "thinking_token_budget")]
    Vllm,
    /// Qwen/DashScope/SGLang `thinking_budget`.
    #[serde(rename = "thinking_budget")]
    Qwen,
    /// llama.cpp `thinking_budget_tokens`.
    #[serde(rename = "thinking_budget_tokens")]
    LlamaCpp,
}

impl ThinkingTokenBudgetField {
    /// The exact top-level request field name.
    pub const fn field_name(self) -> &'static str {
        match self {
            Self::Vllm => VLLM_THINKING_TOKEN_BUDGET_FIELD,
            Self::Qwen => "thinking_budget",
            Self::LlamaCpp => "thinking_budget_tokens",
        }
    }
}

/// A pi-controlled thinking value referenced from a chat template.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThinkingVariable {
    /// `{ "$var": "thinking.enabled" }` — resolves to a boolean.
    #[serde(rename = "thinking.enabled")]
    Enabled,
    /// `{ "$var": "thinking.effort" }` — resolves to the selected level, mapped through the model.
    #[serde(rename = "thinking.effort")]
    Effort,
    /// `{ "$var": "thinking.budget" }` — resolves to a numeric token budget.
    #[serde(rename = "thinking.budget")]
    Budget,
}

/// A `{ "$var": ... }` chat-template reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatTemplateVariable {
    /// The pi-controlled value to substitute.
    #[serde(rename = "$var")]
    pub variable: ThinkingVariable,
    /// When true, the whole key is omitted while thinking is off.
    #[serde(default, rename = "omitWhenOff")]
    pub omit_when_off: bool,
}

/// One value inside a `chatTemplateArgs`/`chatTemplateKwargs` map.
///
/// Deserialization is untagged, so an object that is neither a literal nor a
/// well-known `{ "$var": ... }` reference deserializes as a literal and is
/// rejected by validation rather than silently forwarded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatTemplateValue {
    /// A pi-controlled variable reference.
    Variable(ChatTemplateVariable),
    /// A literal JSON value forwarded verbatim.
    Literal(serde_json::Value),
}

impl ChatTemplateValue {
    fn validate(&self, name: &str) -> Result<(), DeclarationError> {
        if let Self::Literal(serde_json::Value::Object(object)) = self {
            if object.contains_key("$var") {
                return Err(DeclarationError::UnknownChatTemplateVariable {
                    name: name.to_owned(),
                });
            }
        }
        Ok(())
    }

    /// Resolve this value against the caller's thinking selection.
    ///
    /// Returns `None` when the value is intentionally omitted (an
    /// `omitWhenOff` variable while thinking is off, an unmapped
    /// `thinking.effort`, or a `thinking.budget` with no computed budget).
    pub fn interpolate(&self, thinking: &ThinkingSelection) -> Option<serde_json::Value> {
        match self {
            Self::Literal(value) => Some(value.clone()),
            Self::Variable(variable) => {
                if variable.omit_when_off && !thinking.enabled {
                    return None;
                }
                match variable.variable {
                    ThinkingVariable::Enabled => Some(serde_json::Value::Bool(thinking.enabled)),
                    ThinkingVariable::Budget => thinking.budget.map(serde_json::Value::from),
                    ThinkingVariable::Effort => {
                        let raw = thinking.effort.as_deref()?;
                        match thinking.level_map.get(raw) {
                            Some(None) => None,
                            Some(Some(mapped)) => Some(serde_json::Value::String(mapped.clone())),
                            None => Some(serde_json::Value::String(raw.to_owned())),
                        }
                    }
                }
            }
        }
    }
}

/// The caller-selected reasoning state used to resolve chat-template variables.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThinkingSelection {
    /// Whether reasoning is enabled for this request.
    pub enabled: bool,
    /// The selected thinking level (e.g. `"high"`), if any.
    pub effort: Option<String>,
    /// The computed reasoning-token budget, if any.
    pub budget: Option<u64>,
    /// Optional level map; `None` maps a level to "unsupported" (omit).
    pub level_map: BTreeMap<String, Option<String>>,
}

impl ThinkingSelection {
    /// Resolve a full `chatTemplateArgs`/`chatTemplateKwargs` map.
    ///
    /// Keys whose values resolve to `None` are dropped; an all-omitted map
    /// returns `None` so callers omit the field entirely.
    pub fn interpolate_chat_template(
        &self,
        values: &BTreeMap<String, ChatTemplateValue>,
    ) -> Option<serde_json::Value> {
        let mut resolved = serde_json::Map::new();
        for (key, value) in values {
            if let Some(resolved_value) = value.interpolate(self) {
                resolved.insert(key.clone(), resolved_value);
            }
        }
        if resolved.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(resolved))
        }
    }
}

/// Per-model preset metadata declared for a provider route.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelPreset {
    /// Default sampling parameters merged into the request body; per-request
    /// keys override these. Applies only to OpenAI-compatible codecs.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sampling_params: BTreeMap<String, serde_json::Value>,
    /// Per-model HTTP headers. Caller headers override these.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// vLLM scheduler priority (top-level `priority`); lower runs earlier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vllm_priority: Option<i64>,
    /// Whether this model accepts `max_output_tokens`; `None` means codec default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_max_output_tokens: Option<bool>,
    /// Top-level request field used for a reasoning-token budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_token_budget_field: Option<ThinkingTokenBudgetField>,
    /// Arguments sent as `chat_template_args` (e.g. Baseten).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_template_args: Option<BTreeMap<String, ChatTemplateValue>>,
    /// Kwargs sent as `chat_template_kwargs` (e.g. generic chat-template format).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_template_kwargs: Option<BTreeMap<String, ChatTemplateValue>>,
    /// Reasoning/thinking wire format selected for this model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_format: Option<ThinkingFormat>,
    /// Exact effort remapping; a null value means omit that wire effort.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub thinking_level_map: BTreeMap<String, Option<String>>,
    /// Whether a declared thinking format also emits reasoning_effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_reasoning_effort: Option<bool>,
    /// Explicit Mistral Chat reasoning request contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mistral_reasoning: Option<MistralReasoningProfile>,
    /// Whether this model accepts the `strict` tool field / strict JSON-schema
    /// constrained sampling. `None` selects the route's own default, exactly
    /// like Pi's per-API compat defaults; `Some(false)` disables strict even on
    /// a route whose default would enable it.
    #[serde(
        default,
        alias = "supportsStrictMode",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_strict_mode: Option<bool>,
    /// Whether this model accepts OpenAI grammar `custom` tools. Pi defaults
    /// this to `false` on every route and lets generated metadata enable it.
    #[serde(
        default,
        alias = "supportsOpenAIGrammarTools",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_openai_grammar_tools: Option<bool>,
    /// Anthropic Messages-specific compatibility record (Pi's
    /// `AnthropicMessagesCompat`).
    #[serde(
        default,
        alias = "anthropicCompat",
        skip_serializing_if = "Option::is_none"
    )]
    pub anthropic_compat: Option<AnthropicCompatPreset>,
    /// Explicit per-model inline user-image limits; absent metadata uses the
    /// host's bounded fallback, not a provider-name inference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_input_limits: Option<crate::media::ImageInputLimits>,
}

/// Mistral Chat reasoning controls, selected by model data rather than its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MistralReasoningProfile {
    /// Emit reasoning_effort using the declared level mapping.
    ReasoningEffort,
    /// Emit prompt_mode = "reasoning" while enabled.
    PromptMode,
}

/// Anthropic Messages compatibility record (Pi's `AnthropicMessagesCompat`).
///
/// Every field is optional: `None` keeps the route default documented on the
/// field, so a declaration only records a deviation from a compliant route.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AnthropicCompatPreset {
    /// Whether the route accepts per-tool `eager_input_streaming`. Default
    /// `true`; `false` omits it and sends the legacy
    /// `fine-grained-tool-streaming-2025-05-14` beta for tool-enabled requests.
    #[serde(
        default,
        alias = "supportsEagerToolInputStreaming",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_eager_tool_input_streaming: Option<bool>,
    /// Whether the route accepts Anthropic strict tool schemas. Default `false`.
    #[serde(
        default,
        alias = "supportsStrictTools",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_strict_tools: Option<bool>,
    /// Whether the exact transport supports effort-only system messages and the
    /// thinking binding controls. Default `false`.
    #[serde(
        default,
        alias = "supportsMidConvoEffort",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_mid_convo_effort: Option<bool>,
    /// Whether to force adaptive thinking regardless of the model id.
    #[serde(
        default,
        alias = "forceAdaptiveThinking",
        skip_serializing_if = "Option::is_none"
    )]
    pub force_adaptive_thinking: Option<bool>,
    /// Whether an empty thinking signature may be replayed as `signature: ""`
    /// instead of converting the thinking block to text. Default `false`.
    #[serde(
        default,
        alias = "allowEmptySignature",
        skip_serializing_if = "Option::is_none"
    )]
    pub allow_empty_signature: Option<bool>,
    /// Models this route accepts in the `fallbacks` request field. Empty means
    /// the field is omitted (Anthropic rejects it with no permitted target).
    #[serde(
        default,
        alias = "allowedFallbackModels",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub allowed_fallback_models: Vec<AnthropicFallbackModel>,
}

/// One Anthropic server-side refusal fallback target.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AnthropicFallbackModel {
    /// Provider that owns the fallback model.
    pub provider: String,
    /// Fallback model identifier accepted in `fallbacks[].model`.
    pub model: String,
    /// Local pricing metadata for a response the fallback model produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<AnthropicFallbackCost>,
}

/// Per-million-token prices for an Anthropic fallback model.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AnthropicFallbackCost {
    /// Input-token rate per million tokens.
    pub input: f64,
    /// Output-token rate per million tokens.
    pub output: f64,
    /// Cache-read rate per million tokens.
    #[serde(alias = "cacheRead")]
    pub cache_read: f64,
    /// Cache-write rate per million tokens.
    #[serde(alias = "cacheWrite")]
    pub cache_write: f64,
}

impl AnthropicFallbackCost {
    /// Convert declared dollars per million tokens to a local price, rounding
    /// up fractional microdollars. Unrepresentable rates remain unpriced.
    pub fn pricing(self) -> Option<crate::pricing::Pricing> {
        fn rate(value: f64) -> Option<crate::pricing::TokenRate> {
            let microdollars = (value * 1_000_000.0).ceil();
            (value >= 0.0 && microdollars.is_finite() && microdollars < u64::MAX as f64)
                .then_some(crate::pricing::TokenRate(microdollars as u64))
        }
        Some(crate::pricing::Pricing {
            input: rate(self.input)?,
            output: rate(self.output)?,
            cache_read: rate(self.cache_read)?,
            cache_write_5m: rate(self.cache_write)?,
            // No declared one-hour rate: a response reporting such writes
            // remains unpriced (checked by the Anthropic stream decoder).
            cache_write_1h: None,
            reasoning: None,
            tiers: vec![],
        })
    }
}

impl AnthropicCompatPreset {
    fn validate(&self) -> Result<(), DeclarationError> {
        for fallback in &self.allowed_fallback_models {
            if fallback.provider.trim().is_empty()
                || fallback.provider.len() > 256
                || fallback.model.trim().is_empty()
                || fallback.model.len() > 256
            {
                return Err(DeclarationError::Invalid(
                    "invalid Anthropic fallback model declaration".into(),
                ));
            }
            if let Some(cost) = &fallback.cost {
                if [cost.input, cost.output, cost.cache_read, cost.cache_write]
                    .into_iter()
                    .any(|rate| !rate.is_finite() || rate < 0.0)
                {
                    return Err(DeclarationError::Invalid(
                        "invalid Anthropic fallback model cost".into(),
                    ));
                }
            }
        }
        Ok(())
    }
}

impl std::fmt::Debug for ModelPreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelPreset")
            .field("sampling_params", &"<configured>")
            .field("headers", &"<redacted>")
            .field("thinking_format", &self.thinking_format)
            .field("mistral_reasoning", &self.mistral_reasoning)
            .finish_non_exhaustive()
    }
}

// Config serialization remains explicit configuration, like EndpointConfig's
// default headers. Runtime ModelSpec projections must never serialize secrets.
pub(crate) fn serialize_public_preset<S: serde::Serializer>(
    preset: &ModelPreset,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut public = preset.clone();
    public.headers.clear();
    public.serialize(serializer)
}

impl ModelPreset {
    pub(crate) fn validate_protocol(
        &self,
        protocol: crate::Protocol,
    ) -> Result<(), DeclarationError> {
        let chat = self.vllm_priority.is_some()
            || self.thinking_format.is_some()
            || self.thinking_token_budget_field.is_some()
            || self.chat_template_args.is_some()
            || self.chat_template_kwargs.is_some()
            || self.supports_reasoning_effort.is_some();
        if (chat && protocol != crate::Protocol::OpenAiChat)
            || (self.supports_max_output_tokens.is_some()
                && protocol != crate::Protocol::OpenAiResponses)
            || (self.mistral_reasoning.is_some()
                && !matches!(
                    protocol,
                    crate::Protocol::OpenAiChat | crate::Protocol::MistralConversations
                ))
            || (self.anthropic_compat.is_some() && protocol != crate::Protocol::AnthropicMessages)
            || (!self.sampling_params.is_empty()
                && match protocol {
                    crate::Protocol::OpenAiChat => false,
                    crate::Protocol::OpenAiResponses => self.sampling_params.keys().any(|name| {
                        !matches!(name.as_str(), "temperature" | "top_p" | "top_logprobs")
                    }),
                    _ => true,
                })
            || (!self.thinking_level_map.is_empty()
                && !matches!(protocol, crate::Protocol::OpenAiChat))
        {
            return Err(DeclarationError::Invalid(
                "model preset is unsupported by this protocol".into(),
            ));
        }
        Ok(())
    }

    /// Reject malformed preset metadata fail-closed.
    pub fn validate(&self) -> Result<(), DeclarationError> {
        check_object_size(self)?;
        if let Some(limits) = self.image_input_limits {
            limits
                .validate()
                .map_err(|_| DeclarationError::Invalid("invalid image input limits".into()))?;
        }
        check_headers(&self.headers)?;
        check_sampling(&self.sampling_params)?;
        if let Some(anthropic) = &self.anthropic_compat {
            anthropic.validate()?;
        }
        if self.mistral_reasoning.is_some() && self.thinking_format.is_some() {
            return Err(DeclarationError::Invalid(
                "Mistral reasoning and generic thinking formats are mutually exclusive".into(),
            ));
        }
        if self.thinking_level_map.iter().any(|(key, value)| {
            key.is_empty()
                || value
                    .as_ref()
                    .is_some_and(|v| v.is_empty() || v.len() > 128)
        }) {
            return Err(DeclarationError::Invalid(
                "invalid thinking level mapping".into(),
            ));
        }
        for values in [&self.chat_template_args, &self.chat_template_kwargs]
            .into_iter()
            .flatten()
        {
            for (name, value) in values {
                value.validate(name)?;
            }
        }
        if self
            .thinking_format
            .is_some_and(|format| format.is_string_thinking())
            && self.chat_template_args.is_some()
        {
            return Err(DeclarationError::Invalid(
                "string-thinking format must not also declare chat_template_args".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Provider credential environment aliases.
///
/// This is declared plumbing only: it lists the environment variables a
/// provider setup surface should recognize and which of them must be sent as
/// `Authorization: Bearer` rather than the provider's ordinary API-key header.
/// It never reads, stores or transmits a credential.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderCredentialPreset {
    /// Environment variables checked in priority order.
    pub environment_variables: Vec<String>,
    /// Variables that must be presented as `Authorization: Bearer`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bearer_token_variables: Vec<String>,
}

impl ProviderCredentialPreset {
    /// Reject empty or duplicate variable names.
    pub fn validate(&self) -> Result<(), DeclarationError> {
        let mut seen = std::collections::BTreeSet::new();
        for variable in &self.environment_variables {
            if variable.trim().is_empty() {
                return Err(DeclarationError::Invalid(
                    "credential environment variable name must not be empty".to_owned(),
                ));
            }
            if !seen.insert(variable.as_str()) {
                return Err(DeclarationError::Invalid(format!(
                    "duplicate credential environment variable {variable}"
                )));
            }
        }
        let mut bearer_seen = std::collections::BTreeSet::new();
        for variable in &self.bearer_token_variables {
            if !bearer_seen.insert(variable.as_str()) {
                return Err(DeclarationError::Invalid(format!(
                    "duplicate bearer variable {variable}"
                )));
            }
            if !self.environment_variables.contains(variable) {
                return Err(DeclarationError::Invalid(format!(
                    "bearer variable {variable} must also be listed in environment_variables"
                )));
            }
        }
        Ok(())
    }

    /// Whether a resolved environment variable must be sent as `Authorization: Bearer`.
    pub fn presents_as_bearer(&self, variable: &str) -> bool {
        self.bearer_token_variables
            .iter()
            .any(|name| name == variable)
    }
}

/// Bound-checked per-request transport overrides (declared surface).
///
/// The callback hooks upstream exposes (`fetch`, `onPayload`, `onResponse`,
/// `transformHeaders`) and resolved secrets (`apiKey`) are **host-owned
/// runtime** hooks and are intentionally absent here; octet already expresses
/// them through [`crate::HostStreamTransport`] and its authentication lifecycle.
/// This type captures the bounded, data-only knobs so a host can validate them
/// before applying overrides.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RequestOverrides {
    /// Supported sampling controls, overriding model sampling defaults.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sampling_params: BTreeMap<String, serde_json::Value>,
    /// Declared Azure Responses routing overrides, outside the canonical request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub azure: Option<AzureRequestOptions>,
    /// Extra request headers; caller values override provider defaults.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Provider-scoped environment values, taking precedence over the process.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// HTTP request timeout in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Maximum client-side retry attempts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    /// Maximum delay in milliseconds to wait for a server-requested retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retry_delay_ms: Option<u64>,
    /// Request-local Codex Responses transport selection. Only a route that
    /// declares [`crate::EndpointTransport::WebSocketPreferred`] may open the
    /// Responses WebSocket.
    #[serde(
        default,
        alias = "codexTransport",
        skip_serializing_if = "Option::is_none"
    )]
    pub codex_transport: Option<codex::CodexTransport>,
    /// Request-local Codex WebSocket connect deadline in milliseconds.
    #[serde(
        default,
        alias = "codexConnectTimeoutMs",
        skip_serializing_if = "Option::is_none"
    )]
    pub codex_connect_timeout_ms: Option<u64>,
}

impl std::fmt::Debug for RequestOverrides {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestOverrides")
            .field("headers", &"<redacted>")
            .field("env", &"<redacted>")
            .field("timeout_ms", &self.timeout_ms)
            .field("max_retries", &self.max_retries)
            .field("max_retry_delay_ms", &self.max_retry_delay_ms)
            .finish_non_exhaustive()
    }
}

impl RequestOverrides {
    /// Reject overrides that are empty, collision-prone or unbounded.
    pub fn validate(&self) -> Result<(), DeclarationError> {
        check_object_size(self)?;
        check_sampling(&self.sampling_params)?;
        if let Some(azure) = &self.azure {
            azure.validate()?;
        }
        check_headers(&self.headers)?;
        if self.timeout_ms == Some(0) {
            return Err(DeclarationError::Invalid(
                "timeout_ms must be greater than zero".to_owned(),
            ));
        }
        codex::normalize_codex_timeout_ms(self.codex_connect_timeout_ms)?;
        if let Some(retries) = self.max_retries {
            if retries > MAX_RETRIES_CEILING {
                return Err(DeclarationError::Invalid(format!(
                    "max_retries {retries} exceeds the {MAX_RETRIES_CEILING}-attempt ceiling"
                )));
            }
        }
        for variable in self.env.keys() {
            if variable.is_empty()
                || variable.len() > 256
                || !variable
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                return Err(DeclarationError::Invalid(
                    "invalid provider environment variable name".into(),
                ));
            }
            if self.env[variable].len() > crate::auth::MAX_ENV_VALUE_BYTES {
                return Err(DeclarationError::Invalid(
                    "provider environment value exceeds its byte limit".into(),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(value: serde_json::Value) -> BTreeMap<String, ChatTemplateValue> {
        serde_json::from_value(value).expect("fixture chat template map")
    }

    #[test]
    fn chat_template_arguments_interpolate_variables_and_literals() {
        let template = args(json!({
            "enable_thinking": {"$var": "thinking.enabled"},
            "budget": {"$var": "thinking.budget"},
            "effort": {"$var": "thinking.effort"},
            "static": {"nested": [1, 2]}
        }));
        let selection = ThinkingSelection {
            enabled: true,
            effort: Some("high".to_owned()),
            budget: Some(8192),
            level_map: BTreeMap::from([("high".to_owned(), Some("retain".to_owned()))]),
        };
        assert_eq!(
            selection.interpolate_chat_template(&template),
            Some(json!({
                "enable_thinking": true,
                "budget": 8192,
                "effort": "retain",
                "static": {"nested": [1, 2]}
            }))
        );
    }

    #[test]
    fn chat_template_omits_unmapped_and_off_values() {
        let template = args(json!({
            "enable_thinking": {"$var": "thinking.enabled"},
            "budget": {"$var": "thinking.budget"},
            "effort": {"$var": "thinking.effort"},
            "omit": {"$var": "thinking.enabled", "omitWhenOff": true}
        }));
        let selection = ThinkingSelection {
            enabled: false,
            effort: Some("medium".to_owned()),
            budget: None,
            level_map: BTreeMap::from([("medium".to_owned(), None)]),
        };
        // `omit` drops for off; `effort` maps to unsupported; `budget` has no value.
        assert_eq!(
            selection.interpolate_chat_template(&template),
            Some(json!({"enable_thinking": false}))
        );

        let empty = args(json!({
            "budget": {"$var": "thinking.budget"},
            "omit": {"$var": "thinking.enabled", "omitWhenOff": true}
        }));
        assert_eq!(selection.interpolate_chat_template(&empty), None);
    }

    #[test]
    fn unknown_chat_template_variable_fails_closed() {
        let template = args(json!({"x": {"$var": "thinking.bogus"}}));
        let preset = ModelPreset {
            chat_template_args: Some(template),
            ..ModelPreset::default()
        };
        assert_eq!(
            preset.validate(),
            Err(DeclarationError::UnknownChatTemplateVariable {
                name: "x".to_owned()
            })
        );
    }

    #[test]
    fn declared_image_limits_are_model_specific_and_checked() {
        let limits = crate::media::ImageInputLimits {
            max_width: 2_000,
            max_height: 1_500,
            max_bytes: 2_000_000,
        };
        let declared = ModelPreset {
            image_input_limits: Some(limits),
            ..Default::default()
        };
        declared.validate().unwrap();
        let roundtrip: ModelPreset =
            serde_json::from_value(serde_json::to_value(&declared).unwrap()).unwrap();
        assert_eq!(roundtrip.image_input_limits, Some(limits));
        assert_eq!(ModelPreset::default().image_input_limits, None);
        let invalid = ModelPreset {
            image_input_limits: Some(crate::media::ImageInputLimits {
                max_width: 0,
                ..limits
            }),
            ..Default::default()
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn model_preset_round_trips_and_rejects_bad_headers() {
        let preset = ModelPreset {
            sampling_params: BTreeMap::from([("top_p".to_owned(), json!(0.9))]),
            headers: BTreeMap::from([("x-model".to_owned(), "glm".to_owned())]),
            vllm_priority: Some(-5),
            supports_max_output_tokens: Some(false),
            thinking_token_budget_field: Some(ThinkingTokenBudgetField::Vllm),
            chat_template_args: Some(args(json!({
                "enable_thinking": {"$var": "thinking.enabled"}
            }))),
            thinking_format: Some(ThinkingFormat::Baseten),
            ..ModelPreset::default()
        };
        preset.validate().unwrap();
        let encoded = serde_json::to_value(&preset).unwrap();
        assert_eq!(encoded["vllm_priority"], json!(-5));
        assert_eq!(encoded["supports_max_output_tokens"], json!(false));
        assert_eq!(
            encoded["thinking_token_budget_field"],
            json!("thinking_token_budget")
        );
        assert_eq!(encoded["thinking_format"], json!("baseten"));
        let decoded: ModelPreset = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, preset);

        let bad = ModelPreset {
            headers: BTreeMap::from([("bad header".to_owned(), "v".to_owned())]),
            ..ModelPreset::default()
        };
        assert!(matches!(
            bad.validate(),
            Err(DeclarationError::InvalidHeaderName(_))
        ));
    }

    #[test]
    fn string_thinking_is_distinct_and_excludes_chat_template_args() {
        assert_eq!(
            serde_json::to_value(ThinkingFormat::StringThinking).unwrap(),
            json!("string-thinking")
        );
        assert!(ThinkingFormat::StringThinking.is_string_thinking());
        let conflicted = ModelPreset {
            thinking_format: Some(ThinkingFormat::StringThinking),
            chat_template_args: Some(args(json!({"x": true}))),
            ..ModelPreset::default()
        };
        assert!(matches!(
            conflicted.validate(),
            Err(DeclarationError::Invalid(_))
        ));
    }

    #[test]
    fn provider_credential_preset_orders_and_marks_bearer_aliases() {
        let preset = ProviderCredentialPreset {
            environment_variables: vec![
                "ANTHROPIC_AUTH_TOKEN".to_owned(),
                "ANTHROPIC_OAUTH_TOKEN".to_owned(),
                "ANTHROPIC_API_KEY".to_owned(),
            ],
            bearer_token_variables: vec![
                "ANTHROPIC_AUTH_TOKEN".to_owned(),
                "ANTHROPIC_OAUTH_TOKEN".to_owned(),
            ],
        };
        preset.validate().unwrap();
        assert!(preset.presents_as_bearer("ANTHROPIC_AUTH_TOKEN"));
        assert!(preset.presents_as_bearer("ANTHROPIC_OAUTH_TOKEN"));
        assert!(!preset.presents_as_bearer("ANTHROPIC_API_KEY"));

        let unlisted = ProviderCredentialPreset {
            environment_variables: vec!["ANTHROPIC_API_KEY".to_owned()],
            bearer_token_variables: vec!["ANTHROPIC_AUTH_TOKEN".to_owned()],
        };
        assert!(unlisted.validate().is_err());
        let vertex = ProviderCredentialPreset {
            environment_variables: vec!["GOOGLE_CLOUD_API_KEY".to_owned()],
            bearer_token_variables: Vec::new(),
        };
        vertex.validate().unwrap();
        assert!(!vertex.presents_as_bearer("GOOGLE_CLOUD_API_KEY"));
    }

    #[test]
    fn request_overrides_are_bounded() {
        let ok = RequestOverrides {
            headers: BTreeMap::from([("x-session-affinity".to_owned(), "abc".to_owned())]),
            env: BTreeMap::from([("HTTP_PROXY".to_owned(), "http://localhost:8080".to_owned())]),
            timeout_ms: Some(600_000),
            max_retries: Some(2),
            max_retry_delay_ms: Some(60_000),
            ..Default::default()
        };
        ok.validate().unwrap();
        assert!(RequestOverrides {
            timeout_ms: Some(0),
            ..RequestOverrides::default()
        }
        .validate()
        .is_err());
        assert!(RequestOverrides {
            max_retries: Some(MAX_RETRIES_CEILING + 1),
            ..RequestOverrides::default()
        }
        .validate()
        .is_err());
    }
}
