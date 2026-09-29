//! Model catalog, configuration loading, and the embedded snapshot.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::auth::CredentialResolverRegistry;
use crate::error::ConfigError;
use crate::pricing::Pricing;
use crate::types::{
    Capabilities, Endpoint, EndpointId, Modality, ModelId, ModelLimits, ModelSpec,
    OpenAiChatReasoningMode, Protocol, ReasoningControl,
};

fn default_timeout_secs() -> u64 {
    30
}

/// Serialization shape for the complete catalog configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogConfig {
    /// List of endpoint configurations.
    pub endpoints: Vec<EndpointConfig>,
    /// List of model configurations.
    pub models: Vec<ModelConfig>,
}

/// Configuration for an endpoint.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EndpointConfig {
    /// Endpoint identifier.
    pub id: EndpointId,
    /// Base URL of the endpoint (must trailing-slash).
    pub base_url: url::Url,
    /// Auth strategy for the endpoint.
    pub auth: AuthConfig,
    /// Default headers to apply to outgoing requests.
    #[serde(default)]
    pub default_headers: BTreeMap<String, String>,
    /// Preferred response transport.
    #[serde(default)]
    pub transport: crate::types::EndpointTransport,
    /// Endpoint-specific request runtime behavior selected by a provider declaration.
    #[serde(default)]
    pub runtime: crate::types::RequestRuntime,
    /// Maximum time to send a request and receive response headers, in seconds.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

/// Serialization configuration for auth credentials.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthConfig {
    /// No authentication.
    None,
    /// Bearer token referenced by an environment variable.
    BearerEnv {
        /// Env var name.
        var: String,
    },
    /// Custom header credentials referenced by an environment variable.
    HeaderEnv {
        /// Header name.
        name: String,
        /// Env var name.
        var: String,
    },
    /// Bearer token forwarded in a custom header referenced by an environment variable.
    HeaderBearerEnv {
        /// Header name.
        name: String,
        /// Env var name.
        var: String,
    },
    /// Dynamic token resolver bound at load-time.
    Dynamic {
        /// Registry identifier for the resolver.
        resolver_id: String,
    },
}

/// Configuration for a model specification.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Model identifier.
    pub id: ModelId,
    /// Identifier of the endpoint this model uses.
    pub endpoint: EndpointId,
    /// Wire-level API model name.
    pub api_name: String,
    /// Optional human-facing name without provider or artifact details.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Protocol used to communicate with this model.
    pub protocol: Protocol,
    /// Capabilities of this model.
    pub capabilities: Capabilities,
    /// Model limits.
    pub limits: ModelLimits,
    /// Pricing rates for this model.
    #[serde(default)]
    pub pricing: Option<Pricing>,
    /// Prompt-cache compatibility settings for this model/endpoint.
    #[serde(default)]
    pub cache: crate::types::CacheCompatibility,
    /// Model defaults consumed by request encoding and HTTP header composition.
    #[serde(default)]
    pub preset: crate::declarations::ModelPreset,
}

/// Resolved binding of a model specification and its destination endpoint.
#[derive(Clone)]
pub struct Model {
    /// Canonical model specification.
    pub spec: Arc<ModelSpec>,
    /// Destination endpoint configuration.
    pub endpoint: Arc<Endpoint>,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Model")
            .field("spec", &self.spec.id)
            .field("endpoint", &self.endpoint.id)
            .finish()
    }
}

/// The output-token bound actually emitted by an inference codec.
///
/// `None` means no enforceable wire bound, not zero or a trusted model maximum.
/// Hosts must refuse such requests under hard token/cost ceilings. This covers
/// both Codex's mandatory omission and an explicitly unsupported Responses cap.
/// Anthropic/Bedrock return their model default only because they emit it.
///
/// This is a pure cap-selection function, not full request validation. Call it
/// with the final model/profile and canonical cap used for dispatch; request
/// overrides cannot change cap/profile after host reservation. Native Responses
/// compact is a separate operation with no output-cap field and must not borrow
/// a fictional inference bound from this function.
pub fn effective_output_token_cap(model: &Model, requested: Option<u64>) -> Option<u64> {
    match model.spec.protocol {
        crate::Protocol::OpenAiResponses
            if model
                .endpoint
                .runtime
                .responses_profile
                .omits_max_output_tokens()
                || model.spec.preset.supports_max_output_tokens == Some(false) =>
        {
            None
        }
        crate::Protocol::AnthropicMessages | crate::Protocol::BedrockConverse => {
            Some(requested.unwrap_or(model.spec.limits.max_output_tokens))
        }
        _ => requested,
    }
}

/// Registry of models and endpoints.
#[derive(Clone, Default)]
pub struct ModelCatalog {
    endpoints: HashMap<EndpointId, Arc<Endpoint>>,
    models: HashMap<ModelId, Arc<ModelSpec>>,
    endpoint_labels: HashMap<EndpointId, String>,
}

impl ModelCatalog {
    /// Parse and validate the embedded JSON model catalog snapshot.
    pub fn builtin() -> Result<Self, ConfigError> {
        let raw = include_str!("../models/catalog.json");
        let cfg: CatalogConfig =
            serde_json::from_str(raw).map_err(|e| ConfigError::Parse(e.to_string()))?;
        Self::from_config(cfg)
    }

    /// Loads configurations containing static or env-based auth.
    ///
    /// Returns `ConfigError::MissingCredentialResolver` if any dynamic auth is declared.
    pub fn from_config(cfg: CatalogConfig) -> Result<Self, ConfigError> {
        Self::from_config_with_resolvers(cfg, &HashMap::new())
    }

    /// Loads configurations resolving dynamic auth providers from the registry.
    pub fn from_config_with_resolvers(
        cfg: CatalogConfig,
        resolvers: &CredentialResolverRegistry,
    ) -> Result<Self, ConfigError> {
        let mut catalog = Self::default();

        for ep_cfg in cfg.endpoints {
            let endpoint = translate_endpoint(ep_cfg, resolvers)?;
            catalog.register_endpoint(endpoint)?;
        }

        for m_cfg in cfg.models {
            let spec = ModelSpec {
                id: m_cfg.id,
                endpoint: m_cfg.endpoint,
                api_name: m_cfg.api_name,
                display_name: m_cfg.display_name,
                protocol: m_cfg.protocol,
                capabilities: m_cfg.capabilities,
                limits: m_cfg.limits,
                pricing: m_cfg.pricing,
                cache: m_cfg.cache,
                preset: m_cfg.preset,
            };
            catalog.register_model(spec)?;
        }

        Ok(catalog)
    }

    /// Registers a new endpoint.
    pub fn register_endpoint(&mut self, endpoint: Endpoint) -> Result<(), ConfigError> {
        if self.endpoints.contains_key(&endpoint.id) {
            return Err(ConfigError::DuplicateEndpoint(endpoint.id));
        }
        validate_endpoint(&endpoint)?;
        self.endpoints
            .insert(endpoint.id.clone(), Arc::new(endpoint));
        Ok(())
    }

    /// Sets an optional human-facing label for an endpoint.
    pub fn set_endpoint_label(
        &mut self,
        id: EndpointId,
        label: impl Into<String>,
    ) -> Result<(), ConfigError> {
        if !self.endpoints.contains_key(&id) {
            return Err(ConfigError::UnknownEndpoint(id));
        }
        let label = label.into();
        if !label.trim().is_empty() {
            self.endpoint_labels.insert(id, label);
        }
        Ok(())
    }

    /// Returns the optional human-facing endpoint label.
    pub fn endpoint_label(&self, id: &EndpointId) -> Option<&str> {
        self.endpoint_labels.get(id).map(String::as_str)
    }

    /// Registers a new model specification.
    pub fn register_model(&mut self, mut spec: ModelSpec) -> Result<(), ConfigError> {
        if self.models.contains_key(&spec.id) {
            return Err(ConfigError::DuplicateModel(spec.id.clone()));
        }
        if !self.endpoints.contains_key(&spec.endpoint) {
            return Err(ConfigError::UnknownEndpoint(spec.endpoint.clone()));
        }

        // Pricing is immutable once the spec is stored in the catalog. Keep
        // tiers canonical here so every response cost calculation can iterate
        // without cloning or sorting.
        if let Some(pricing) = spec.pricing.as_mut() {
            pricing
                .tiers
                .sort_unstable_by_key(|tier| tier.min_input_tokens);
        }
        validate_model_spec(&spec)?;

        self.models.insert(spec.id.clone(), Arc::new(spec));
        Ok(())
    }

    /// Resolves a Model ID into its endpoint binding.
    pub fn resolve(&self, id: &ModelId) -> Result<Model, ConfigError> {
        let spec = self
            .models
            .get(id)
            .ok_or_else(|| ConfigError::UnknownModel(id.clone()))?;
        let endpoint = self
            .endpoints
            .get(&spec.endpoint)
            .ok_or_else(|| ConfigError::UnknownEndpoint(spec.endpoint.clone()))?;

        Ok(Model {
            spec: spec.clone(),
            endpoint: endpoint.clone(),
        })
    }

    /// Returns an iterator over all registered model specifications.
    pub fn models(&self) -> impl Iterator<Item = &ModelSpec> {
        self.models.values().map(|m| m.as_ref())
    }

    /// Returns whether an endpoint with this id is registered.
    pub fn has_endpoint(&self, id: &EndpointId) -> bool {
        self.endpoints.contains_key(id)
    }

    /// Removes a model only when it still belongs to the supplied endpoint.
    ///
    /// Dynamic host-owned catalogs use this fence to avoid deleting a model
    /// which another catalog source registered under the same identifier.
    pub fn remove_model_if_endpoint(&mut self, id: &ModelId, endpoint: &EndpointId) -> bool {
        let belongs_to_endpoint = self
            .models
            .get(id)
            .is_some_and(|model| &model.endpoint == endpoint);
        if belongs_to_endpoint {
            self.models.remove(id);
        }
        belongs_to_endpoint
    }

    /// Removes an endpoint only when no remaining model uses it.
    ///
    /// Returns `true` when an endpoint was removed. Endpoint labels are
    /// removed with the endpoint so a future registration cannot inherit a
    /// stale presentation label.
    pub fn remove_endpoint_if_unused(&mut self, id: &EndpointId) -> bool {
        if self.models.values().any(|model| &model.endpoint == id) {
            return false;
        }
        self.endpoint_labels.remove(id);
        self.endpoints.remove(id).is_some()
    }

    /// Removes models whose endpoint has no usable credentials.
    ///
    /// Endpoints are retained because they may be shared with models registered
    /// later. This is intended for application-facing catalogs: a model with an
    /// unset `*_API_KEY` cannot be selected, while local unauthenticated and
    /// dynamically authenticated endpoints remain available.
    pub fn retain_configured_models(&mut self) {
        let configured: HashSet<_> = self
            .endpoints
            .iter()
            .filter(|(_, endpoint)| endpoint.auth.is_configured())
            .map(|(id, _)| id)
            .collect();
        self.models
            .retain(|_, model| configured.contains(&model.endpoint));
    }
}

/// Google request URLs interpolate `api_name` into one fixed path segment.
/// Restrict it at catalog ingress so discovery/configuration cannot escape the
/// configured Gemini or Vertex endpoint scope.
fn google_api_name_is_safe(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub(crate) fn validate_model_spec(spec: &ModelSpec) -> Result<(), ConfigError> {
    spec.preset
        .validate()
        .map_err(|_| ConfigError::InvalidModel(spec.id.clone()))?;
    spec.preset
        .validate_protocol(spec.protocol)
        .map_err(|_| ConfigError::InvalidModel(spec.id.clone()))?;
    if ((spec.preset.thinking_format.is_some()
        || spec.preset.thinking_token_budget_field.is_some()
        || spec.preset.chat_template_args.is_some()
        || spec.preset.chat_template_kwargs.is_some()
        || spec.preset.mistral_reasoning.is_some()
        || !spec.preset.thinking_level_map.is_empty())
        && spec.capabilities.reasoning.is_none())
        || spec.api_name.is_empty()
        || spec.capabilities.deferred_tool_loading
        || !spec.capabilities.input_modalities.is_valid()
        || !spec.capabilities.output_modalities.is_valid()
        || spec.limits.context_window == 0
        || spec.limits.max_output_tokens == 0
        || spec.limits.max_output_tokens > spec.limits.context_window
        || (spec.protocol == Protocol::GoogleGenerativeAi
            && !google_api_name_is_safe(&spec.api_name))
        || (spec.protocol != Protocol::OpenAiChat
            && (spec.capabilities.input_modalities.contains(Modality::Audio)
                || spec
                    .capabilities
                    .output_modalities
                    .contains(Modality::Audio)))
    {
        return Err(ConfigError::InvalidModel(spec.id.clone()));
    }

    if let Some(reasoning) = &spec.capabilities.reasoning {
        let valid = match (reasoning.control, reasoning.effort_budgets) {
            (ReasoningControl::TokenBudget, Some(budgets)) => {
                budgets.minimal >= 1024
                    && budgets.minimal <= budgets.low
                    && budgets.low <= budgets.medium
                    && budgets.medium <= budgets.high
                    && budgets.high <= budgets.xhigh
                    && budgets.xhigh <= budgets.max
                    && budgets.max < spec.limits.max_output_tokens
            }
            (
                ReasoningControl::Effort | ReasoningControl::AlwaysOn | ReasoningControl::Toggle,
                None,
            ) => true,
            _ => false,
        };
        let protocol_matches = match spec.protocol {
            // Anthropic supports both explicit token budgets (extended thinking)
            // and effort control (adaptive thinking + `output_config.effort`).
            Protocol::AnthropicMessages => true,
            Protocol::OpenAiChat => {
                matches!(
                    reasoning.control,
                    ReasoningControl::Effort
                        | ReasoningControl::AlwaysOn
                        | ReasoningControl::Toggle
                ) || (reasoning.control == ReasoningControl::TokenBudget
                    && spec.preset.thinking_token_budget_field.is_some())
            }
            Protocol::OpenAiResponses => reasoning.control == ReasoningControl::Effort,
            Protocol::GoogleGenerativeAi => matches!(
                reasoning.control,
                ReasoningControl::Effort | ReasoningControl::TokenBudget
            ),
            Protocol::BedrockConverse => reasoning.control == ReasoningControl::TokenBudget,
            // This codec does not yet map native reasoning controls or content.
            Protocol::MistralConversations => false,
            // Pi's `thinkingLevel` is an effort control; a token budget has no
            // native field and fails closed in the codec.
            Protocol::PiMessages => matches!(
                reasoning.control,
                ReasoningControl::Effort | ReasoningControl::AlwaysOn | ReasoningControl::Toggle
            ),
        };
        let chat_mode_matches = reasoning.openai_chat_mode == OpenAiChatReasoningMode::Standard
            || (spec.protocol == Protocol::OpenAiChat
                && matches!(
                    reasoning.control,
                    ReasoningControl::Effort
                        | ReasoningControl::AlwaysOn
                        | ReasoningControl::Toggle
                )
                && reasoning.exposes_text);
        let effort_range_valid = reasoning.min_effort <= reasoning.max_effort;
        let options_valid = reasoning.options.as_ref().is_none_or(|o| o.is_valid())
            && match &reasoning.openai_chat_mode {
                OpenAiChatReasoningMode::ProviderValues {
                    values, default, ..
                } => {
                    let legacy = crate::types::ReasoningOptions {
                        values: values.clone(),
                        default: default.clone(),
                    };
                    legacy.is_valid() && reasoning.options.as_ref().is_none_or(|o| o == &legacy)
                }
                _ => true,
            };
        let declared_choices = reasoning
            .options
            .as_ref()
            .map(|o| o.choices())
            .unwrap_or_else(|| match &reasoning.openai_chat_mode {
                // AlwaysOn's synthesized On choice must not hide a conflicting
                // legacy exact declaration during catalog validation.
                OpenAiChatReasoningMode::ProviderValues { values, .. } => values
                    .iter()
                    .filter_map(|v| crate::types::ReasoningConfig::from_provider_value(v))
                    .collect(),
                _ => reasoning.choices(),
            });
        let choices_valid = declared_choices.iter().all(|choice| match choice {
            crate::types::ReasoningConfig::Off => reasoning.control != ReasoningControl::AlwaysOn,
            crate::types::ReasoningConfig::On => matches!(
                reasoning.control,
                ReasoningControl::Toggle | ReasoningControl::AlwaysOn
            ),
            crate::types::ReasoningConfig::Effort(e) => {
                matches!(
                    reasoning.control,
                    ReasoningControl::Effort | ReasoningControl::TokenBudget
                ) && *e >= reasoning.min_effort
                    && *e <= reasoning.max_effort
            }
            crate::types::ReasoningConfig::Budget(_) => false,
        });
        let google_values_valid = spec.protocol != Protocol::GoogleGenerativeAi
            || reasoning.control == ReasoningControl::TokenBudget
            || reasoning.options.as_ref().is_none_or(|o| {
                o.values.iter().all(|v| {
                    matches!(
                        v.as_str(),
                        "MINIMAL"
                            | "LOW"
                            | "MEDIUM"
                            | "HIGH"
                            | "minimal"
                            | "low"
                            | "medium"
                            | "high"
                    )
                })
            });
        let profile_valid = match &reasoning.openai_chat_mode {
            OpenAiChatReasoningMode::DeepSeekToggle
            | OpenAiChatReasoningMode::QwenEnableThinking
            | OpenAiChatReasoningMode::QwenChatTemplate { .. }
            | OpenAiChatReasoningMode::Together { effort: false } => {
                reasoning.control == ReasoningControl::Toggle
            }
            _ => true,
        };
        if !valid
            || !protocol_matches
            || !chat_mode_matches
            || !effort_range_valid
            || !options_valid
            || !choices_valid
            || !profile_valid
            || !google_values_valid
        {
            return Err(ConfigError::InvalidReasoningConfig(spec.id.clone()));
        }
    }
    if let Some(pricing) = &spec.pricing {
        let mut thresholds = std::collections::HashSet::new();
        if pricing
            .tiers
            .iter()
            .any(|tier| !thresholds.insert(tier.min_input_tokens))
            || pricing
                .tiers
                .windows(2)
                .any(|pair| pair[0].min_input_tokens > pair[1].min_input_tokens)
        {
            return Err(ConfigError::InvalidPricing(spec.id.clone()));
        }
    }
    Ok(())
}

pub(crate) fn validate_endpoint(endpoint: &Endpoint) -> Result<(), ConfigError> {
    validate_base_url(&endpoint.base_url)?;
    if endpoint.timeout.is_zero() {
        return Err(ConfigError::InvalidTimeout(endpoint.id.clone()));
    }
    if let Some(auth_header) = crate::auth::auth_header_name(&endpoint.auth) {
        if endpoint.default_headers.contains_key(&auth_header) {
            return Err(ConfigError::AuthHeaderCollision(auth_header));
        }
    }
    Ok(())
}

fn validate_base_url(url: &url::Url) -> Result<(), ConfigError> {
    let allowed_version_query = url.query_pairs().collect::<Vec<_>>();
    let has_only_api_version = allowed_version_query.len() == 1
        && allowed_version_query[0].0 == "api-version"
        && !allowed_version_query[0].1.is_empty()
        && allowed_version_query[0].1.len() <= 128
        && allowed_version_query[0]
            .1
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'));
    if !url.cannot_be_a_base()
        && (url.scheme() == "http" || url.scheme() == "https")
        && url.username().is_empty()
        && url.password().is_none()
        && (url.query().is_none() || has_only_api_version)
        && url.fragment().is_none()
        && url.path().ends_with('/')
    {
        Ok(())
    } else {
        // Invalid URLs may contain userinfo or query credentials. The only
        // query accepted on an endpoint base is Azure's non-secret
        // `api-version`; return the contract, not attacker-controlled URL text,
        // because catalog errors can be persisted or presented directly.
        Err(ConfigError::InvalidBaseUrl(
            "expected an absolute HTTP(S) URL without userinfo or fragment, with a trailing slash and at most one non-secret api-version query"
                .to_owned(),
        ))
    }
}

fn resolve_auth(
    auth_cfg: AuthConfig,
    resolvers: &CredentialResolverRegistry,
) -> Result<crate::auth::Auth, ConfigError> {
    match auth_cfg {
        AuthConfig::None => Ok(crate::auth::Auth::None),
        AuthConfig::BearerEnv { var } => Ok(crate::auth::Auth::bearer_env(var)),
        AuthConfig::HeaderEnv { name, var } => {
            let header_name = http::HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| ConfigError::InvalidHeader(e.to_string()))?;
            Ok(crate::auth::Auth::header_env(header_name, var))
        }
        AuthConfig::HeaderBearerEnv { name, var } => {
            let header_name = http::HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| ConfigError::InvalidHeader(e.to_string()))?;
            Ok(crate::auth::Auth::header_bearer_env(header_name, var))
        }
        AuthConfig::Dynamic { resolver_id } => {
            let resolver = resolvers
                .get(&resolver_id)
                .ok_or_else(|| ConfigError::MissingCredentialResolver(resolver_id.clone()))?;
            Ok(crate::auth::Auth::dynamic(resolver.clone()))
        }
    }
}

fn translate_endpoint(
    cfg: EndpointConfig,
    resolvers: &CredentialResolverRegistry,
) -> Result<Endpoint, ConfigError> {
    validate_base_url(&cfg.base_url)?;
    if cfg.timeout_secs == 0 {
        return Err(ConfigError::InvalidTimeout(cfg.id));
    }

    let auth = resolve_auth(cfg.auth, resolvers)?;

    let mut default_headers = http::HeaderMap::new();
    for (k, v) in cfg.default_headers {
        let name = http::HeaderName::from_bytes(k.as_bytes())
            .map_err(|e| ConfigError::InvalidHeader(e.to_string()))?;
        let value = http::HeaderValue::from_str(&v)
            .map_err(|e| ConfigError::InvalidHeader(e.to_string()))?;
        default_headers.insert(name, value);
    }

    // Auth header collision check
    if let Some(auth_hdr) = crate::auth::auth_header_name(&auth) {
        if default_headers.contains_key(&auth_hdr) {
            return Err(ConfigError::AuthHeaderCollision(auth_hdr));
        }
    }

    Ok(Endpoint {
        id: cfg.id,
        base_url: cfg.base_url,
        auth,
        default_headers,
        transport: cfg.transport,
        runtime: cfg.runtime,
        timeout: std::time::Duration::from_secs(cfg.timeout_secs),
    })
}

#[cfg(test)]
mod tests;
