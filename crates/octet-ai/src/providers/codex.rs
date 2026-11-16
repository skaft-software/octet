//! Account-scoped Codex model metadata and route binding.
#![allow(missing_docs)]
use super::codex_context::codex_gpt_6_model as known_gpt_6_model;
use super::codex_context::{
    resolve_codex_context_window, CodexContextOverride, CODEX_MAX_OUTPUT_TOKENS,
};
use super::inventory_metadata::*;
use crate::provider_auth::codex::{self, CredentialStore, SubscriptionClaims};
use crate::{
    AgentDelegation, Auth, Capabilities, Endpoint, EndpointId, ModalitySet, ModelCatalog, ModelId,
    ModelLimits, ModelSpec, OpenAiChatReasoningMode, Protocol, ReasoningCapability,
    ReasoningConfig, ReasoningControl,
};
use anyhow::Context;
use std::time::Duration;
const MODALITY_FIELDS: &[&str] = &[
    "architecture/input_modalities",
    "input_modalities",
    "modalities/input",
    "vision",
    "audio",
];

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiscoveredCodexModel {
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    pub input_modalities: ModalitySet,
    pub output_modalities: ModalitySet,
    pub reasoning_options: crate::types::ReasoningOptions,
    pub context_window: u64,
    /// Backend default retained separately from the user-selected budget.
    #[serde(default)]
    pub default_context_window: u64,
    pub max_context_window: u64,
    pub max_output_tokens: u64,
    pub min_effort: crate::ReasoningEffort,
    pub max_effort: crate::ReasoningEffort,
    /// Positive account-inventory authority, never inferred from a model name.
    #[serde(default)]
    pub reasoning_effort_updates: bool,
    pub responses_lite: bool,
    // `Option<T>` normally treats a missing key as `None`; the custom decoder
    // keeps explicit null valid while making incomplete dynamic metadata fail.
    #[serde(deserialize_with = "deserialize_required_agent_delegation")]
    pub agent_delegation: Option<AgentDelegation>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct CodexModelCache {
    pub version: u8,
    pub account_id: String,
    pub plan: Option<String>,
    pub models: Vec<DiscoveredCodexModel>,
}

#[derive(Clone)]
pub struct CodexDiscovery {
    pub claims: crate::provider_auth::codex::SubscriptionClaims,
    pub models: Vec<DiscoveredCodexModel>,
}

pub struct CodexInventory {
    pub models: Vec<DiscoveredCodexModel>,
    /// `id: reason` for each entry whose reasoning metadata was unusable.
    pub skipped: Vec<String>,
}

pub const CODEX_MODEL_CACHE_VERSION: u8 = 12;
pub const CODEX_MODEL_CACHE_REFRESH_INTERVAL: Duration = Duration::from_secs(60 * 60);
pub const CODEX_MODELS_CLIENT_VERSION: &str = "0.159.2";
pub fn deserialize_required_agent_delegation<'de, D>(
    deserializer: D,
) -> Result<Option<AgentDelegation>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    <Option<AgentDelegation> as serde::Deserialize>::deserialize(deserializer)
}

pub fn positive_u64(entry: &serde_json::Value, names: &[&str]) -> Option<u64> {
    names.iter().find_map(|name| {
        entry
            .get(*name)
            .and_then(serde_json::Value::as_u64)
            .filter(|value| *value > 0)
    })
}

pub fn codex_fallback_reasoning_options(model_id: &str) -> crate::types::ReasoningOptions {
    // Sparse Codex metadata cannot establish Off or Ultra for generic fallback
    // models. Bundled 6.1 Sol/Terra advertise Ultra, but it is only usable when the
    // account inventory also positively advertises V2; offline fallback strips it.
    let floor = codex_min_effort(model_id);
    let ceiling = codex_max_effort(model_id);
    let candidates: &[&str] = if matches!(model_id, "gpt-6.1-sol" | "gpt-6.1-terra") {
        &["low", "medium", "high", "xhigh", "max", "ultra"]
    } else if model_id == "gpt-5.6-luna" {
        &["none", "low", "medium", "high", "xhigh", "max"]
    } else {
        &["minimal", "low", "medium", "high", "xhigh", "max"]
    };
    let values = candidates
        .iter()
        .copied()
        .filter(|value| match ReasoningConfig::from_provider_value(value) {
            Some(ReasoningConfig::Off) => true,
            Some(ReasoningConfig::Effort(effort)) => {
                effort >= floor
                    && (effort <= ceiling
                        || (matches!(model_id, "gpt-6.1-sol" | "gpt-6.1-terra")
                            && effort == crate::ReasoningEffort::Ultra))
            }
            _ => false,
        })
        .map(str::to_owned)
        .collect();
    // Defaults follow the bundled Codex catalog.
    let default = match model_id {
        "gpt-6.1-sol" | "gpt-6.1-terra" => Some("low"),
        "gpt-6-sol" | "gpt-6-luna" => Some("medium"),
        _ => None,
    };
    crate::types::ReasoningOptions {
        values,
        default: default.map(str::to_owned),
    }
}

pub fn strip_codex_ultra(options: &mut crate::types::ReasoningOptions) {
    options.values.retain(|v| {
        ReasoningConfig::from_provider_value(v)
            != Some(ReasoningConfig::Effort(crate::ReasoningEffort::Ultra))
    });
    if options
        .default
        .as_ref()
        .is_some_and(|d| !options.values.contains(d))
    {
        options.default = options.values.first().cloned();
    }
}

pub fn codex_reasoning_range(
    options: &crate::types::ReasoningOptions,
    id: &str,
) -> (crate::ReasoningEffort, crate::ReasoningEffort) {
    let efforts = options
        .choices()
        .into_iter()
        .filter_map(|c| match c {
            ReasoningConfig::Effort(e) => Some(e),
            _ => None,
        })
        .collect::<Vec<_>>();
    (
        efforts
            .iter()
            .copied()
            .min()
            .unwrap_or(codex_min_effort(id)),
        efforts
            .iter()
            .copied()
            .max()
            .unwrap_or(codex_max_effort(id)),
    )
}

pub fn codex_models_from_response(
    body: &serde_json::Value,
) -> anyhow::Result<Vec<DiscoveredCodexModel>> {
    codex_inventory_from_response(body).map(|inventory| inventory.models)
}

pub fn codex_inventory_from_response(body: &serde_json::Value) -> anyhow::Result<CodexInventory> {
    // The subscription backend uses `models`, while OpenAI-compatible proxies
    // commonly expose the same inventory under `data`. Accepting both keeps
    // OAuth discovery working through enterprise gateways as well.
    let entries = body
        .get("models")
        .or_else(|| body.get("data"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("Codex models response has no models array"))?;
    let mut models = Vec::with_capacity(entries.len());
    let mut skipped = Vec::new();
    for entry in entries {
        let Some(id) = entry
            .as_str()
            .or_else(|| {
                entry
                    .get("slug")
                    .or_else(|| entry.get("id"))
                    .and_then(serde_json::Value::as_str)
            })
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        let fallback = codex_model_context_limits(id);
        let advertised_context = positive_u64(
            entry,
            &["context_window", "context_length", "max_context_tokens"],
        );
        let advertised_max = positive_u64(entry, &["max_context_window"]);
        let (default_context_window, max_context_window) =
            match (advertised_context, advertised_max) {
                (Some(context), Some(maximum)) => (context.min(maximum), maximum),
                (Some(context), None) => (context, context),
                (None, Some(maximum)) => (maximum, maximum),
                (None, None) => fallback,
            };
        let resolution = resolve_codex_context_window(
            id,
            default_context_window,
            max_context_window,
            positive_u64(entry, &["max_output_tokens", "max_completion_tokens"]),
            CodexContextOverride::NONE,
        )
        .expect("resolving a Codex context window without a user override cannot fail");
        let context_window = resolution.context_window;
        // Astra's advertised input envelope never changes its 128K output
        // contract, while lower live metadata remains authoritative.
        let max_output_tokens = resolution.max_output_tokens;
        let agent_delegation = entry
            .get("multi_agent_version")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|version| version.eq_ignore_ascii_case("v2"))
            .then_some(AgentDelegation::V2);
        let metadata = match decode_reasoning_metadata(entry) {
            Ok(metadata) => metadata,
            Err(error) => {
                skipped.push(format!("{id}: {error}"));
                continue;
            }
        };
        if metadata.source == crate::types::ReasoningMetadataSource::Unknown {
            skipped.push(format!("{id}: unknown reasoning metadata"));
            continue;
        }
        let mut reasoning_options = if metadata.supported == Some(false) {
            crate::types::ReasoningOptions {
                values: vec!["none".into()],
                default: Some("none".into()),
            }
        } else {
            metadata
                .options
                .unwrap_or_else(|| codex_fallback_reasoning_options(id))
        };
        if agent_delegation != Some(AgentDelegation::V2) {
            strip_codex_ultra(&mut reasoning_options);
        }
        if !reasoning_options.is_valid() {
            skipped.push(format!("{id}: no usable reasoning choices"));
            continue;
        }
        let (min_effort, max_effort) = codex_reasoning_range(&reasoning_options, id);
        let responses_lite = entry
            .get("use_responses_lite")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        models.push(DiscoveredCodexModel {
            id: id.to_owned(),
            display_name: discovered_display_name(entry, id)
                .or_else(|| (id == "gpt-6.1-sol").then(|| "GPT-6.1-Sol".to_owned())),
            input_modalities: codex_input_modalities(entry, id),
            output_modalities: output_modalities_from_entry(entry),
            reasoning_options,
            context_window,
            default_context_window,
            max_context_window,
            max_output_tokens,
            min_effort,
            max_effort,
            reasoning_effort_updates: entry
                .get("supports_reasoning_effort_updates")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            responses_lite,
            agent_delegation,
        });
    }
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    if models.is_empty() {
        anyhow::bail!("Codex models response contained no usable models");
    }
    Ok(CodexInventory { models, skipped })
}

pub fn codex_model_context_limits(model_id: &str) -> (u64, u64) {
    super::codex_context::fallback_context_windows(model_id)
}

pub fn codex_model_limits(model_id: &str) -> (ModelLimits, u64) {
    let (context_window, maximum) = codex_model_context_limits(model_id);
    (
        ModelLimits {
            context_window,
            max_output_tokens: CODEX_MAX_OUTPUT_TOKENS.min(context_window),
        },
        maximum,
    )
}

pub fn codex_min_effort(model_id: &str) -> crate::ReasoningEffort {
    if known_gpt_6_model(model_id) || model_id == "gpt-5.6-luna" {
        crate::ReasoningEffort::Low
    } else {
        crate::ReasoningEffort::Minimal
    }
}

pub fn codex_max_effort(model_id: &str) -> crate::ReasoningEffort {
    if known_gpt_6_model(model_id) || model_id.starts_with("gpt-5.6-") {
        crate::ReasoningEffort::Max
    } else {
        crate::ReasoningEffort::High
    }
}

pub fn codex_supports_image_input(model_id: &str) -> bool {
    model_id == "codex-mini-latest"
        || known_gpt_6_model(model_id)
        || model_id.starts_with("gpt-5.4")
        || model_id.starts_with("gpt-5.5")
        || model_id.starts_with("gpt-5.6")
        || model_id.starts_with("gpt-5.3-codex")
        || model_id.starts_with("gpt-5.2-codex")
        || model_id.starts_with("gpt-5.1-codex")
}

pub fn codex_input_modalities(entry: &serde_json::Value, id: &str) -> ModalitySet {
    let mut modalities = input_modalities_from_entry(entry);
    if !has_metadata_assertion(entry, MODALITY_FIELDS) && codex_supports_image_input(id) {
        modalities = modalities.with(crate::Modality::Image);
    }
    for (field, modality) in [
        ("vision", crate::Modality::Image),
        ("audio", crate::Modality::Audio),
    ] {
        match asserted_capability(entry, &[field]) {
            Some(true) => modalities = modalities.with(modality),
            Some(false) => modalities = modalities.without(modality),
            None => {}
        }
    }
    modalities
}

pub fn codex_plan_cache_key(
    claims: &crate::provider_auth::codex::SubscriptionClaims,
) -> Option<&str> {
    claims.plan.as_ref().map(|plan| plan.raw_value())
}

pub fn save_codex_model_cache(
    store: &crate::provider_auth::codex::CredentialStore,
    discovery: &CodexDiscovery,
) -> anyhow::Result<()> {
    let cache = CodexModelCache {
        version: CODEX_MODEL_CACHE_VERSION,
        account_id: discovery.claims.account_id.clone(),
        plan: codex_plan_cache_key(&discovery.claims).map(str::to_owned),
        models: discovery.models.clone(),
    };
    store.save_model_cache(&serde_json::to_vec_pretty(&cache)?)
}

pub fn load_codex_model_cache(
    store: &crate::provider_auth::codex::CredentialStore,
    claims: &crate::provider_auth::codex::SubscriptionClaims,
) -> anyhow::Result<Option<Vec<DiscoveredCodexModel>>> {
    let Some(bytes) = store.load_fresh_model_cache(CODEX_MODEL_CACHE_REFRESH_INTERVAL)? else {
        return Ok(None);
    };
    let mut cache: CodexModelCache =
        serde_json::from_slice(&bytes).context("invalid Codex model cache")?;
    if cache.version != CODEX_MODEL_CACHE_VERSION
        || cache.account_id != claims.account_id
        || cache.plan.as_deref() != codex_plan_cache_key(claims)
        || cache.models.is_empty()
    {
        return Ok(None);
    }
    for model in &mut cache.models {
        if known_gpt_6_model(&model.id) {
            // Current-schema caches can still contain a previously accepted
            // over-cap GPT-6 entry; normalize it to the fixed contract.
            model.max_output_tokens = model.max_output_tokens.min(CODEX_MAX_OUTPUT_TOKENS);
        }
    }
    let mut ids = std::collections::BTreeSet::new();
    for model in &cache.models {
        if model.id.trim() != model.id
            || model.id.is_empty()
            || !ids.insert(model.id.as_str())
            || model.context_window == 0
            || model.default_context_window == 0
            || model.default_context_window > model.max_context_window
            || model.max_context_window < model.context_window
            || model.max_output_tokens == 0
            || model.max_output_tokens > model.context_window
            || !model.reasoning_options.is_valid()
            || codex_reasoning_range(&model.reasoning_options, &model.id)
                != (model.min_effort, model.max_effort)
            || model.min_effort > model.max_effort
            || (model.max_effort == crate::ReasoningEffort::Ultra
                && model.agent_delegation != Some(AgentDelegation::V2))
        {
            anyhow::bail!("invalid Codex model cache: incomplete or inconsistent model metadata");
        }
    }
    Ok(Some(cache.models))
}

pub fn conservative_offline_codex_models(
    mut models: Vec<DiscoveredCodexModel>,
) -> Vec<DiscoveredCodexModel> {
    for model in &mut models {
        model.reasoning_effort_updates = false;
        model.responses_lite = false;
        model.agent_delegation = None;
        strip_codex_ultra(&mut model.reasoning_options);
        (model.min_effort, model.max_effort) =
            codex_reasoning_range(&model.reasoning_options, &model.id);
    }
    models.retain(|m| m.reasoning_options.is_valid());
    models
}

pub fn fallback_codex_models() -> Vec<DiscoveredCodexModel> {
    crate::provider_auth::codex::MODELS
        .iter()
        .map(|model_id| {
            let (default_context_window, _) = codex_model_context_limits(model_id);
            let (limits, max_context_window) = codex_model_limits(model_id);
            let mut reasoning_options = codex_fallback_reasoning_options(model_id);
            // The checked-in model contract alone cannot establish account
            // authority for V2/Ultra when the live inventory is unreachable.
            strip_codex_ultra(&mut reasoning_options);
            DiscoveredCodexModel {
                id: (*model_id).to_owned(),
                display_name: (*model_id == "gpt-6.1-sol").then(|| "GPT-6.1-Sol".to_owned()),
                input_modalities: codex_input_modalities(&serde_json::Value::Null, model_id),
                output_modalities: ModalitySet::none(),
                reasoning_options,
                context_window: limits.context_window,
                default_context_window,
                max_context_window,
                max_output_tokens: limits.max_output_tokens,
                min_effort: codex_min_effort(model_id),
                max_effort: codex_max_effort(model_id),
                reasoning_effort_updates: false,
                responses_lite: false,
                agent_delegation: None,
            }
        })
        .collect()
}

/// Inventory requests carry account credentials, so they never follow redirects.
fn inventory_client(timeout: Duration) -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
}

pub fn codex_models_url() -> anyhow::Result<url::Url> {
    let mut url = url::Url::parse(super::contract::CODEX.base_url)?.join("models")?;
    url.query_pairs_mut()
        .append_pair("client_version", CODEX_MODELS_CLIENT_VERSION);
    Ok(url)
}

pub fn codex_user_agent() -> String {
    format!(
        "octet/{} ({})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS
    )
}

pub fn codex_inference_capabilities(model: &DiscoveredCodexModel) -> Capabilities {
    Capabilities {
        // Preserve known advertised inputs, but Responses has no audio codec.
        input_modalities: model.input_modalities.without(crate::Modality::Audio),
        output_modalities: conversation_output_modalities(
            Protocol::OpenAiResponses,
            model.output_modalities,
        ),
        tools: true,
        parallel_tool_calls: true,
        reasoning: Some(ReasoningCapability {
            options: Some(model.reasoning_options.clone()),
            control: ReasoningControl::Effort,
            exposes_text: true,
            preserves_state: true,
            effort_budgets: None,
            openai_chat_mode: OpenAiChatReasoningMode::Standard,
            min_effort: model.min_effort,
            max_effort: model.max_effort,
        }),
        responses_lite: false,
        agent_delegation: model.agent_delegation,
        structured_output: false,
        deferred_tool_loading: false,
        responses_features: crate::ResponsesFeatures {
            reasoning_effort_updates: model.reasoning_effort_updates,
            ..Default::default()
        },
    }
}

/// Evidence behind a local catalog snapshot; fallback is not account availability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InventorySource {
    SuppliedInventory,
    OnlineDiscovery,
    FreshCache,
    ConservativeFallback,
}

/// Credential-free immutable model snapshot. Constructors do no I/O.
#[derive(Clone, Debug)]
pub struct CodexCatalog {
    models: Vec<DiscoveredCodexModel>,
    source: InventorySource,
    claims: Option<SubscriptionClaims>,
}
impl CodexCatalog {
    /// Conservative suggestions only, not confirmed account availability.
    pub fn fallback() -> Self {
        Self {
            models: fallback_codex_models(),
            source: InventorySource::ConservativeFallback,
            claims: None,
        }
    }
    /// Decode an inventory supplied by the caller. This does not authenticate its origin.
    pub fn from_inventory(body: &serde_json::Value) -> anyhow::Result<Self> {
        Ok(Self {
            models: codex_models_from_response(body)?,
            source: InventorySource::SuppliedInventory,
            claims: None,
        })
    }
    pub fn models(&self) -> &[DiscoveredCodexModel] {
        &self.models
    }
    pub fn source(&self) -> InventorySource {
        self.source
    }
    pub fn is_account_inventory(&self) -> bool {
        self.source != InventorySource::ConservativeFallback
    }

    /// Explicit local storage read. Never refreshes, imports credentials or contacts a provider.
    pub fn load_local(store: &CredentialStore) -> anyhow::Result<Option<Self>> {
        let Some(claims) = codex::local_subscription_claims(store)? else {
            return Ok(None);
        };
        let (models, source) = match load_codex_model_cache(store, &claims)? {
            Some(models) => (
                conservative_offline_codex_models(models),
                InventorySource::FreshCache,
            ),
            None => (
                fallback_codex_models(),
                InventorySource::ConservativeFallback,
            ),
        };
        Ok(Some(Self {
            models,
            source,
            claims: Some(claims),
        }))
    }

    /// Explicit network authorization for this account's inventory and necessary credential refresh.
    /// No unrelated provider or metadata service is initialized. Publishes only an account-bound cache.
    pub async fn discover(store: &CredentialStore) -> anyhow::Result<Self> {
        let (discovery, _) =
            discover_inventory_with(store, codex::REFRESH_LOCK_WAIT, || Ok(())).await?;
        save_codex_model_cache(store, &discovery)?;
        Ok(Self {
            models: discovery.models,
            source: InventorySource::OnlineDiscovery,
            claims: Some(discovery.claims),
        })
    }

    /// Bind exactly the declared Codex route. Local storage access is explicit; no network occurs.
    /// Missing or replaced accounts fail instead of selecting a direct-API billing route.
    pub fn bind_into(
        &self,
        catalog: &mut ModelCatalog,
        store: CredentialStore,
        user_override: CodexContextOverride,
        timeout: Duration,
    ) -> anyhow::Result<()> {
        let claims = codex::local_subscription_claims(&store)?
            .ok_or_else(|| anyhow::anyhow!("Codex login required"))?;
        anyhow::ensure!(
            self.claims
                .as_ref()
                .is_none_or(|expected| *expected == claims),
            "Codex account changed; reload the catalog"
        );
        let mut limits = std::collections::BTreeMap::new();
        for model in &self.models {
            let resolved = resolve_codex_context_window(
                &model.id,
                model.default_context_window,
                model.max_context_window,
                Some(model.max_output_tokens),
                user_override,
            )?;
            limits.insert(
                model.id.clone(),
                ModelLimits {
                    context_window: resolved.context_window,
                    max_output_tokens: resolved.max_output_tokens,
                },
            );
        }
        register_codex_models(
            catalog,
            std::sync::Arc::new(codex::CodexResolver::for_account(store, &claims)),
            &self.models,
            timeout,
            |_, model| limits[&model.id],
        )
    }
}

/// Shared route composition; the frontend may supply its context-budget note policy.
pub fn register_codex_models(
    catalog: &mut ModelCatalog,
    resolver: std::sync::Arc<codex::CodexResolver>,
    models: &[DiscoveredCodexModel],
    timeout: Duration,
    mut limits: impl FnMut(&ModelId, &DiscoveredCodexModel) -> ModelLimits,
) -> anyhow::Result<()> {
    let declaration = &super::contract::CODEX;
    declaration.validate()?;
    let route = declaration
        .inventory_route()
        .ok_or_else(|| anyhow::anyhow!("Codex declaration has no route"))?;
    let mut default_headers = super::headers::public_headers(declaration.extra_headers)?;
    default_headers.insert(
        http::header::USER_AGENT,
        http::HeaderValue::from_str(&codex_user_agent())?,
    );
    catalog.register_endpoint(Endpoint {
        id: EndpointId(route.endpoint_id.into()),
        base_url: url::Url::parse(declaration.base_url)?,
        auth: Auth::dynamic(resolver),
        default_headers,
        transport: route.transport,
        runtime: route.runtime,
        timeout,
    })?;
    for model in models {
        let id = if known_gpt_6_model(&model.id)
            || catalog.resolve(&ModelId(model.id.clone())).is_ok()
        {
            ModelId(format!("{}/{}", declaration.id, model.id))
        } else {
            ModelId(model.id.clone())
        };
        let limits = limits(&id, model);
        catalog.register_model(ModelSpec {
            context_window_options: Some(crate::types::ContextWindowOptions {
                default: model.default_context_window,
                maximum: model.max_context_window,
            }),
            preset: Default::default(),
            id,
            endpoint: EndpointId(route.endpoint_id.into()),
            api_name: model.id.clone(),
            display_name: model.display_name.clone(),
            protocol: route.protocol,
            capabilities: codex_inference_capabilities(model),
            limits,
            pricing: super::pricing::pricing_for(declaration, &model.id),
            cache: super::compatibility::cache_compatibility(
                declaration.compatibility,
                &model.id,
                route.protocol,
            ),
        })?;
    }
    Ok(())
}

/// Explicit inventory operation. The callback may refuse after credential readiness,
/// before any inventory request (for example a caller-owned cancellation envelope).
pub async fn discover_inventory_with(
    store: &CredentialStore,
    refresh_lock_wait: Duration,
    before_inventory: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<(CodexDiscovery, Vec<String>)> {
    let claims = codex::local_subscription_claims(store)?
        .ok_or_else(|| anyhow::anyhow!("Codex login required"))?;
    let resolver =
        codex::CodexResolver::for_account(store.clone(), &claims).with_lock_wait(refresh_lock_wait);
    let (mut headers, claims) = resolver.discovery_headers().await?;
    before_inventory()?;
    headers.extend(super::headers::public_headers(
        super::contract::CODEX.extra_headers,
    )?);
    headers.insert(
        http::header::USER_AGENT,
        http::HeaderValue::from_str(&codex_user_agent())?,
    );
    let response = inventory_client(Duration::from_secs(10))?
        .get(codex_models_url()?)
        .headers(headers)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("GET Codex models failed: transport failure"))?;
    anyhow::ensure!(
        response.status().is_success(),
        "GET Codex models failed: HTTP {}",
        response.status().as_u16()
    );
    anyhow::ensure!(
        response
            .content_length()
            .is_none_or(|size| size <= 8 * 1024 * 1024),
        "Codex models response exceeds byte limit"
    );
    use futures_util::StreamExt;
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| anyhow::anyhow!("Codex models response body failed"))?;
        anyhow::ensure!(
            body.len().saturating_add(chunk.len()) <= 8 * 1024 * 1024,
            "Codex models response exceeds byte limit"
        );
        body.extend_from_slice(&chunk);
    }
    let body = serde_json::from_slice(&body)
        .map_err(|_| anyhow::anyhow!("invalid Codex models response JSON"))?;
    let inventory = codex_inventory_from_response(&body)?;
    Ok((
        CodexDiscovery {
            claims,
            models: inventory.models,
        },
        inventory.skipped,
    ))
}

#[cfg(test)]
mod inventory_client_tests {
    use super::inventory_client;
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn inventory_client_does_not_follow_authenticated_redirects() {
        let origin = MockServer::start().await;
        let destination = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/stolen", destination.uri())),
            )
            .mount(&origin)
            .await;
        let status = inventory_client(Duration::from_secs(2))
            .unwrap()
            .get(format!("{}/models", origin.uri()))
            .header("authorization", "Bearer inventory-secret")
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, reqwest::StatusCode::FOUND);
        assert!(destination.received_requests().await.unwrap().is_empty());
    }
}
