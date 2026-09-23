#![allow(missing_docs)]

use std::cell::RefCell;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::tui::terminal::TerminalInput as EventStream;
use anyhow::Context;
use futures_util::StreamExt;
use octet_agent::extension_runtime::ExtensionRuntimeManager;
use octet_agent::secure_fs::{create_regular_file_for_append, open_regular_file_for_append};
use octet_agent::{
    Agent, AgentCompactionMode, AgentConfig, CoreTools, DelegationConfig, DurableGoalStore,
    EffectBroker, EntryValue, ExtensionHost, GoalDriver, Session, SkillRegistry, TelemetryObserver,
};
use octet_ai::{
    AgentDelegation, AiClient, Auth, CacheCompatibility, Capabilities, Endpoint, EndpointId,
    EndpointTransport, ModalitySet, Model, ModelCatalog, ModelId, ModelLimits, ModelSpec,
    OpenAiChatReasoningMode, Pricing, PricingTier, Protocol, ReasoningCapability, ReasoningConfig,
    ReasoningControl, ReasoningMode, RequestRuntime, TokenRate, ToolDef,
};
use sha2::{Digest as _, Sha256};

use crate::app::{
    default_reasoning_for_model, level_from_reasoning, model_supports_ultra,
    normalize_reasoning_for_model, normalize_reasoning_selection_for_model_with_subagents,
    thinking_to_reasoning, App,
};
use crate::codex_context::{
    resolve_codex_context_window, CodexContextOverride, CodexContextTier,
    CODEX_ASTRA_MAX_CONTEXT_WINDOW, CODEX_CONTEXT_ACKNOWLEDGE_ENV, CODEX_CONTEXT_OVERRIDE_ENV,
    CODEX_MAX_OUTPUT_TOKENS,
};
use crate::config::{CompactionMode, Config, ResumeSelector};
use crate::extensions::{
    provider_preflight_config, ExecutableExtensions, ExtensionProviderRuntime,
    SUBAGENTS_EXTENSION_NAME,
};
use crate::modes::interactive::run_blocking_startup_lifecycle;
use crate::prompts::PromptRegistry;
use crate::providers::{
    ModelDiscovery, ModelFilter, ProviderAuthentication, ProviderDeclaration, ProviderRoute,
    ProviderRuntimeConfiguration, BUILTIN_PROVIDER_DECLARATIONS,
};
use crate::resources::{format_skills_for_prompt, FileSystemSkillRegistry};
use crate::session_store::SessionStore;
use crate::tui::pickers::{model_picker, session_picker};
use crate::tui::view::InteractiveShell;

/// Inputs needed to resolve a launch without constructing an Agent or a TUI.
pub struct Bootstrap {
    pub config: Config,
    pub catalog: ModelCatalog,
    pub sessions: SessionStore,
    pub client: AiClient,
    provider_runtime: ExtensionProviderRuntime,
    /// Activated extension host retained while provider declarations are needed
    /// to resolve the user's first model selection.
    prestarted_extensions: RefCell<Option<(ExtensionHost, ExecutableExtensions)>>,
    /// Session opened while resolving resume provenance. Keeping it here
    /// avoids replaying the same JSONL file a second time in `build_app`.
    prepared_session: RefCell<Option<Session>>,
    /// Interactive startup can remain useful as a read-only session viewer
    /// when no configured model exists.
    modeless: std::cell::Cell<bool>,
    /// The one Codex context note each catalog model needs, recorded while the
    /// catalog was built. Catalog enumeration never prints; only the effective
    /// session model's note is ever shown.
    codex_context_notes: CodexContextNotes,
    /// Which provider inventories readiness initialized for this launch.
    ///
    /// Kept so [`Bootstrap::enrich_catalog`] can complete exactly what the plan
    /// deferred, without re-running the fleet initialization a second time.
    readiness: CatalogReadiness,
}

/// The single user-facing Codex context note for every catalog model that needs
/// one, recorded during catalog construction.
///
/// Recording is not printing, and startup no longer prints either: a frontend
/// pulls the note with [`Bootstrap::take_codex_context_note`] when the user can
/// act on it (the first assistant turn, or an on-demand status/context surface),
/// and the latch here makes it a once-per-session note even if a later pull
/// repeats the lookup. See [`crate::codex_context::codex_context_session_note`]
/// for the wording contract.
#[derive(Clone, Debug, Default)]
pub struct CodexContextNotes {
    notes: std::collections::HashMap<ModelId, String>,
    /// Set by the first delivery, so the note can never be shown twice in one
    /// session (and never for a second, different model).
    delivered: std::cell::Cell<bool>,
}

impl CodexContextNotes {
    fn record(&mut self, model: ModelId, note: String) {
        self.notes.insert(model, note);
    }

    /// The note for one model, if its window needs one. A non-Codex model has no
    /// note, so a non-Codex session prints nothing.
    ///
    /// This is the non-consuming peek: it neither delivers nor latches.
    pub fn note_for(&self, model: &ModelId) -> Option<&str> {
        self.notes.get(model).map(String::as_str)
    }

    /// Take the note for `model`, at most once per session.
    ///
    /// Returns `None` when this model needs no note (a non-Codex route) or when
    /// a note was already delivered for this session, so a lazy surface can be
    /// re-entered freely without ever repeating the note.
    pub fn take_for(&self, model: &ModelId) -> Option<String> {
        let note = self.notes.get(model)?;
        if self.delivered.replace(true) {
            return None;
        }
        Some(note.clone())
    }

    /// Merge notes recorded by a later catalog build (catalog enrichment).
    ///
    /// Only missing entries are added: a note already delivered or already
    /// recorded for this session is never replaced, so enrichment cannot unset
    /// or rewrite the one note the session owes. The delivery latch is not
    /// touched.
    pub fn merge(&mut self, other: Self) {
        for (model, note) in other.notes {
            self.notes.entry(model).or_insert(note);
        }
    }
}

impl Bootstrap {
    /// The single Codex context note for the effective session model, if any.
    ///
    /// This is a read-only peek for tests and diagnostics; frontends deliver the
    /// note with [`Bootstrap::take_codex_context_note`]. It returns nothing for a
    /// non-Codex model, so a session on another provider prints no Codex note at
    /// all. See [`crate::codex_context::codex_context_session_note`] for the
    /// wording contract.
    pub fn codex_context_note(&self, model: &ModelId) -> Option<&str> {
        self.codex_context_notes.note_for(model)
    }

    /// Take the one Codex context note for this session, lazily, at most once.
    ///
    /// Startup deliberately shows nothing: the note is not part of the initial
    /// frame or the pre-ready transcript. A frontend calls this when the user can
    /// act on it — attached to the first assistant turn after readiness, or from
    /// an on-demand status/context surface — and gets the same bounded,
    /// effective-model-only wording once; every later call returns `None`.
    ///
    /// The note is unchanged in substance: it still names the model, the
    /// advertised/entitled/effective windows, the deliberate 272K policy and its
    /// double-pricing cliff, and the remedy, and it still carries no internal
    /// Rust API name or operation id. A Codex route above 272K is still marked as
    /// uncertain usage at both session boundaries, independently of whether this
    /// note has ever been shown.
    pub fn take_codex_context_note(&self, model: &ModelId) -> Option<String> {
        self.codex_context_notes.take_for(model)
    }

    /// Starts only provider-capable API 0.3 extensions when their declarations
    /// are required to resolve an otherwise unknown initial model.
    ///
    /// The temporary session is deliberately not a user session: provider
    /// discovery must not create or mutate a session before launch validation
    /// succeeds. The temporary process set is shut down before final startup so
    /// every final App extension initializes against the selected real state.
    fn preflight_extension_providers(&self) -> anyhow::Result<()> {
        if self.prestarted_extensions.borrow().is_some() {
            return Ok(());
        }
        let config = provider_preflight_config(&self.config);
        if config.enabled_extensions.is_empty() {
            return Ok(());
        }

        let temporary = tempfile::tempdir()
            .context("could not create temporary extension-provider bootstrap session")?;
        let session = Session::create(temporary.path().join("provider-bootstrap.jsonl"))
            .context("could not create temporary extension-provider bootstrap session")?;
        let model = extension_provider_bootstrap_model(&self.catalog);
        let reasoning = self
            .config
            .reasoning
            .clone()
            .unwrap_or_else(|| default_reasoning_for_model(&model));
        let (host, mut extensions) = configured_extensions_with_runtime_manager(
            &config,
            &session,
            &model,
            &reasoning,
            &self.sessions,
            None,
            self.provider_runtime.clone(),
        )?;
        // Populate a throwaway copy now so callers can validate/select the
        // projected models. `build_app` repeats this against its owned catalog.
        let mut catalog = self.catalog.clone();
        extensions.synchronize_provider_catalog(&mut catalog, &self.client);
        *self.prestarted_extensions.borrow_mut() = Some((host, extensions));
        startup_phase("extensions.provider-preflight");
        Ok(())
    }

    /// Returns a selection catalog that includes ready host-owned provider
    /// routes without exposing extension declarations as catalog authority.
    fn catalog_with_extension_providers(&self) -> ModelCatalog {
        let mut catalog = self.catalog.clone();
        if let Some((_, extensions)) = self.prestarted_extensions.borrow_mut().as_mut() {
            extensions.synchronize_provider_catalog(&mut catalog, &self.client);
        }
        catalog
    }

    /// Supply an already-open session for the next launch.
    ///
    /// Hosts use this to keep authorization bound to a caller-opened file
    /// descriptor instead of reopening the session by pathname in `build_app`.
    pub(crate) fn set_prepared_session(&mut self, session: Session) {
        *self.prepared_session.get_mut() = Some(session);
    }

    pub(crate) fn take_prepared_session(&self) -> Option<Session> {
        self.prepared_session.borrow_mut().take()
    }

    fn enter_modeless_mode(&self) {
        self.modeless.set(true);
    }

    pub(crate) fn is_modeless(&self) -> bool {
        self.modeless.get()
    }
}

/// Supplies extension initialization with a safe local model snapshot before a
/// provider declaration has made the user's requested model resolvable.
fn extension_provider_bootstrap_model(catalog: &ModelCatalog) -> Model {
    if let Some(model) = catalog
        .models()
        .next()
        .and_then(|specification| catalog.resolve(&specification.id).ok())
    {
        return model;
    }

    let endpoint = Arc::new(Endpoint {
        id: EndpointId("extension-provider-bootstrap".to_owned()),
        base_url: url::Url::parse("http://127.0.0.1:9/")
            .expect("fixed extension-provider bootstrap URL is valid"),
        auth: Auth::None,
        default_headers: http::HeaderMap::new(),
        transport: EndpointTransport::Http,
        runtime: RequestRuntime::default(),
        timeout: Duration::from_secs(30),
    });
    Model {
        spec: Arc::new(ModelSpec {
            preset: Default::default(),
            id: ModelId("extension-provider-bootstrap".to_owned()),
            endpoint: endpoint.id.clone(),
            api_name: "extension-provider-bootstrap".to_owned(),
            display_name: None,
            protocol: Protocol::OpenAiChat,
            capabilities: Capabilities {
                input_modalities: ModalitySet::none(),
                output_modalities: ModalitySet::none(),
                tools: true,
                parallel_tool_calls: false,
                reasoning: None,
                responses_lite: false,
                agent_delegation: None,
                structured_output: false,
                deferred_tool_loading: false,
                responses_features: Default::default(),
            },
            limits: ModelLimits {
                context_window: 128_000,
                max_output_tokens: 16_000,
            },
            pricing: None,
            cache: CacheCompatibility::default(),
        }),
        endpoint,
    }
}

/// Selected persistent session operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionSelection {
    OpenExisting(PathBuf),
    CreateNew(PathBuf),
}

/// Resolved model and session for one launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchSelection {
    pub model: ModelId,
    pub session: SessionSelection,
    /// Effective reasoning restored from session state or invocation defaults.
    pub reasoning: ReasoningConfig,
    /// Effective execution mode restored independently from reasoning effort.
    pub reasoning_mode: ReasoningMode,
}

fn validate_compaction_route(
    mode: CompactionMode,
    active: &Model,
    compact_model: Option<&Model>,
) -> anyhow::Result<()> {
    if mode != CompactionMode::NativeResponses {
        return Ok(());
    }
    if active.spec.protocol != Protocol::OpenAiResponses {
        anyhow::bail!(
            "native Responses compaction requires an OpenAI Responses route; model {} uses {:?}",
            active.spec.id.0,
            active.spec.protocol
        );
    }
    if let Some(compact_model) = compact_model {
        if compact_model.endpoint.id != active.endpoint.id
            || compact_model.spec.id != active.spec.id
        {
            anyhow::bail!(
                "native Responses compaction requires exact route affinity; compaction.compact_model must match active endpoint/model {}/{}",
                active.endpoint.id.0,
                active.spec.id.0
            );
        }
    }
    Ok(())
}

fn validate_native_compaction_replay(
    mode: CompactionMode,
    session: &Session,
    model: &Model,
) -> anyhow::Result<()> {
    if mode != CompactionMode::NativeResponses {
        return Ok(());
    }
    match session.responses_replay_items(&model.endpoint.id, &model.spec.id)? {
        Some(_) => Ok(()),
        None => anyhow::bail!(
            "native Responses compaction requires complete route-affine opaque replay on the active branch"
        ),
    }
}

fn agent_compaction_mode(mode: CompactionMode) -> AgentCompactionMode {
    match mode {
        CompactionMode::Disabled => AgentCompactionMode::Disabled,
        CompactionMode::Local => AgentCompactionMode::Local,
        CompactionMode::NativeResponses => AgentCompactionMode::NativeResponses,
    }
}

const DEEPSEEK_MODEL_ID: &str = "deepseek-v4-pro";
const DEEPSEEK_DEFAULT_CONTEXT_WINDOW: u64 = 1_000_000;
// Only a local capacity reserve; it never becomes an implicit request cap.
const DEEPSEEK_DEFAULT_MAX_OUTPUT_TOKENS: u64 = 384_000;

#[cfg(test)]
const OPENCODE_ANTHROPIC_ENDPOINT_ID: &str = "opencode-anthropic";
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);
// A provider may spend minutes queueing or processing a large prompt before
// it emits response headers. Connection establishment remains separately
// bounded in octet-ai; this phase needs a generous, cancellable allowance.
const PROVIDER_RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(15 * 60);
// Local servers may need to load a model before they can return response
// headers. Keep the same fifteen-minute default for custom endpoints while
// allowing each provider to override it for its own cold-start behavior.
const CUSTOM_ENDPOINT_STARTUP_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const MAX_DISCOVERY_BODY_BYTES: usize = 8 * 1024 * 1024;
// Version 2 invalidated inventories whose llama.cpp context length was guessed
// because older discovery ignored hlid's nested `meta.n_ctx` field. Version 4
// invalidated sparse local inventories that were incorrectly cached as
// tool-incompatible by version 3. Version 5 invalidates v4 entries produced by
// the secondary hlid discovery path before it adopted the same tri-state
// local-tool fallback. Version 6 also scopes a cache entry to the configured
// model metadata, so removing or changing an override immediately re-runs
// discovery. Version 7 invalidates inventories created before the built-in
// Apple Foundation Models metadata was applied to sparse model responses.
// Version 8 gives PCC its distinct 32,768-token context window.
// Version 9 decodes endpoint-owned v1 self-descriptions instead of sparse defaults.
const CUSTOM_MODEL_CACHE_VERSION: u8 = 9;
const PROVIDER_INVENTORY_CACHE_VERSION: u8 = 1;
const MAX_PROVIDER_INVENTORY_CACHE_BYTES: usize = MAX_DISCOVERY_BODY_BYTES + 1024 * 1024;
const PROVIDER_INVENTORY_REFRESH_INTERVAL: Duration = Duration::from_secs(60 * 60);
const NEGATIVE_INVENTORY_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

fn optional_env_value(
    _name: &str,
    value: Result<Option<String>, octet_ai::ConfigError>,
) -> anyhow::Result<Option<String>> {
    match value {
        Ok(value) => Ok(value),
        // Preserve the existing optional-reader policy for invalid Unicode:
        // std::env::var(...).ok() treated it like an unavailable value.
        Err(octet_ai::ConfigError::InvalidEnv(_)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn optional_env(name: &str) -> anyhow::Result<Option<String>> {
    optional_env_value(name, octet_ai::auth::read_bounded_env(name))
}

#[cfg(test)]
fn required_env_value(
    name: &str,
    value: Result<Option<String>, octet_ai::ConfigError>,
) -> anyhow::Result<String> {
    match value? {
        Some(value) => Ok(value),
        None => Err(anyhow::anyhow!("environment variable {name} not found")),
    }
}

fn strict_env_value(
    name: &str,
    value: Result<Option<String>, octet_ai::ConfigError>,
) -> anyhow::Result<Option<String>> {
    match value {
        Ok(value) => Ok(value),
        Err(octet_ai::ConfigError::InvalidEnv(_)) => Err(anyhow::anyhow!(
            "could not read {name}: invalid environment value"
        )),
        Err(error) => Err(error.into()),
    }
}

fn strict_env(name: &str) -> anyhow::Result<Option<String>> {
    strict_env_value(name, octet_ai::auth::read_bounded_env(name))
}

fn blocking_discovery_client(timeout: Duration) -> reqwest::Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

fn discovery_client(timeout: Duration) -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

fn bounded_discovery_json(
    response: reqwest::blocking::Response,
    label: &str,
) -> anyhow::Result<serde_json::Value> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DISCOVERY_BODY_BYTES as u64)
    {
        anyhow::bail!(
            "{label} response exceeds the {}-byte limit",
            MAX_DISCOVERY_BODY_BYTES
        );
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_DISCOVERY_BODY_BYTES.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_DISCOVERY_BODY_BYTES {
        anyhow::bail!(
            "{label} response exceeds the {}-byte limit",
            MAX_DISCOVERY_BODY_BYTES
        );
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| anyhow::anyhow!("invalid {label} response: {error}"))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ProviderInventoryCache {
    version: u8,
    provider_id: String,
    inventory_url: String,
    credential_fingerprint: String,
    body: Option<serde_json::Value>,
    /// An opaque HTTP validator; preserve quotes and weak-validator prefixes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    etag: Option<String>,
    /// Unix milliseconds of the last successful 200/304 catalog check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checked_at: Option<u64>,
}

enum ProviderInventoryResponse {
    Modified {
        body: serde_json::Value,
        etag: Option<String>,
    },
    NotModified {
        etag: Option<String>,
    },
}

enum CachedProviderInventory {
    Available(serde_json::Value),
    Unavailable,
}

fn fingerprint_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn credential_fingerprint(credential: &str) -> String {
    fingerprint_bytes(credential.as_bytes())
}

fn custom_model_cache_fingerprint(
    credential_fingerprint: &str,
    configured: &[crate::auth::custom::CustomModel],
) -> String {
    let configured =
        serde_json::to_vec(configured).expect("custom model metadata must always be serializable");
    let mut scoped = Vec::with_capacity(credential_fingerprint.len() + configured.len() + 40);
    scoped.extend_from_slice(b"octet-custom-model-cache-config-v1");
    scoped.extend_from_slice(&(credential_fingerprint.len() as u64).to_be_bytes());
    scoped.extend_from_slice(credential_fingerprint.as_bytes());
    scoped.extend_from_slice(&(configured.len() as u64).to_be_bytes());
    scoped.extend_from_slice(&configured);
    fingerprint_bytes(&scoped)
}

fn custom_credential_fingerprint(api_key: &str, headers: &http::HeaderMap) -> String {
    fn add_component(hasher: &mut Sha256, bytes: &[u8]) {
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }

    // HeaderMap names are case-normalized, but its iteration order is not a
    // stable cache key. Sort the effective on-wire name/value pairs and frame
    // every component so distinct credentials cannot collide by concatenation.
    let mut header_scope = headers
        .iter()
        .map(|(name, value)| (name.as_str().as_bytes(), value.as_bytes()))
        .collect::<Vec<_>>();
    header_scope.sort_unstable();

    let mut hasher = Sha256::new();
    hasher.update(b"octet-custom-model-cache-scope-v1");
    add_component(&mut hasher, api_key.as_bytes());
    for (name, value) in header_scope {
        add_component(&mut hasher, name);
        add_component(&mut hasher, value);
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn provider_inventory_cache_path(provider_id: &str) -> PathBuf {
    // Keep a short readable prefix for diagnostics, but include the complete
    // digest of the original identifier. Replacing punctuation with `_` alone
    // lets distinct provider IDs map to the same cache file.
    let safe_id = provider_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .take(64)
        .collect::<String>();
    let readable = if safe_id.is_empty() {
        "provider"
    } else {
        safe_id.as_str()
    };
    let digest = fingerprint_bytes(provider_id.as_bytes());
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".octet")
        .join("cache")
        .join("model-inventories")
        .join(format!("{readable}-{digest}.json"))
}

fn load_provider_inventory_cache(
    path: &std::path::Path,
    provider_id: &str,
    inventory_url: &str,
    credential_fingerprint: &str,
) -> anyhow::Result<Option<CachedProviderInventory>> {
    Ok(
        load_provider_inventory_record(path, provider_id, inventory_url, credential_fingerprint)?
            .map(|cache| match cache.body {
                Some(body) => CachedProviderInventory::Available(body),
                None => CachedProviderInventory::Unavailable,
            }),
    )
}

fn load_provider_inventory_record(
    path: &std::path::Path,
    provider_id: &str,
    inventory_url: &str,
    credential_fingerprint: &str,
) -> anyhow::Result<Option<ProviderInventoryCache>> {
    let Some(bytes) = crate::auth::read_bounded_private(path, MAX_PROVIDER_INVENTORY_CACHE_BYTES)?
    else {
        return Ok(None);
    };
    let cache: ProviderInventoryCache =
        serde_json::from_slice(&bytes).context("invalid provider inventory cache")?;
    if cache.version != PROVIDER_INVENTORY_CACHE_VERSION
        || cache.provider_id != provider_id
        || cache.inventory_url != inventory_url
        || cache.credential_fingerprint != credential_fingerprint
    {
        return Ok(None);
    }
    Ok(Some(cache))
}

fn inventory_checked_at() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn save_provider_inventory_cache(
    path: &std::path::Path,
    provider_id: &str,
    inventory_url: &str,
    credential_fingerprint: &str,
    body: Option<&serde_json::Value>,
) -> anyhow::Result<()> {
    let cache = ProviderInventoryCache {
        version: PROVIDER_INVENTORY_CACHE_VERSION,
        provider_id: provider_id.to_owned(),
        inventory_url: inventory_url.to_owned(),
        credential_fingerprint: credential_fingerprint.to_owned(),
        body: body.cloned(),
        etag: None,
        checked_at: body.map(|_| inventory_checked_at()),
    };
    crate::auth::write_private_atomic(path, &serde_json::to_vec(&cache)?, ".provider-models-")
}

fn cache_modified_is_stale(modified: std::time::SystemTime, refresh_interval: Duration) -> bool {
    // A clock rollback or attacker-controlled future timestamp must never pin a
    // cache entry indefinitely. Treat an unmeasurable age as stale.
    modified
        .elapsed()
        .map_or(true, |age| age >= refresh_interval)
}

fn provider_inventory_cache_is_stale(path: &std::path::Path) -> bool {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .map_or(true, |modified| {
            cache_modified_is_stale(modified, PROVIDER_INVENTORY_REFRESH_INTERVAL)
        })
}

fn inventory_etag(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 4096
                && value.is_ascii()
                && http::HeaderValue::from_str(value).is_ok()
        })
        .map(str::to_owned)
}

fn fetch_provider_inventory(
    inventory_url: String,
    headers: http::HeaderMap,
) -> anyhow::Result<ProviderInventoryResponse> {
    let response = blocking_discovery_client(DISCOVERY_TIMEOUT)?
        .get(inventory_url)
        .headers(headers)
        .send()
        .map_err(|_| anyhow::anyhow!("model discovery request failed"))?;
    let etag = inventory_etag(
        response
            .headers()
            .get(http::header::ETAG)
            .and_then(|value| value.to_str().ok()),
    );
    match response.status() {
        http::StatusCode::NOT_MODIFIED => Ok(ProviderInventoryResponse::NotModified { etag }),
        http::StatusCode::OK => Ok(ProviderInventoryResponse::Modified {
            body: bounded_discovery_json(response, "model discovery")?,
            etag,
        }),
        _ => anyhow::bail!("model discovery request was rejected"),
    }
}

fn schedule_provider_inventory_refresh(
    path: PathBuf,
    provider_id: &'static str,
    inventory_url: String,
    credential_fingerprint: String,
    headers: http::HeaderMap,
    force: bool,
) {
    if cfg!(test) || (!force && !provider_inventory_cache_is_stale(&path)) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name(format!("octet-{provider_id}-catalog-refresh"))
        .spawn(move || {
            let _ = refresh_provider_inventory_with(
                &path,
                provider_id,
                inventory_url,
                headers,
                &credential_fingerprint,
                fetch_provider_inventory,
            );
        });
}

fn refresh_provider_inventory_with<F>(
    path: &std::path::Path,
    provider_id: &'static str,
    inventory_url: String,
    headers: http::HeaderMap,
    credential_fingerprint: &str,
    fetch: F,
) -> anyhow::Result<serde_json::Value>
where
    F: FnOnce(String, http::HeaderMap) -> anyhow::Result<ProviderInventoryResponse>,
{
    // Validators are scoped to the same provider, URL and credential as the body.
    // A corrupt/foreign cache must never send its validator to another endpoint.
    let cached =
        load_provider_inventory_record(path, provider_id, &inventory_url, credential_fingerprint)
            .ok()
            .flatten();
    let validator = cached
        .as_ref()
        .filter(|cache| cache.body.is_some())
        .and_then(|cache| inventory_etag(cache.etag.as_deref()));
    let mut headers = headers;
    headers.remove(http::header::IF_NONE_MATCH);
    if let Some(etag) = &validator {
        headers.insert(
            http::header::IF_NONE_MATCH,
            http::HeaderValue::from_str(etag)?,
        );
    }
    let fetched = fetch(inventory_url.clone(), headers).and_then(|response| {
        let (body, etag) = match response {
            ProviderInventoryResponse::Modified { body, etag } => {
                (body, inventory_etag(etag.as_deref()))
            }
            ProviderInventoryResponse::NotModified { etag } => {
                let Some(cache) = cached.filter(|_| validator.is_some()) else {
                    anyhow::bail!("model discovery returned 304 without a scoped validator");
                };
                let body = cache.body.expect("validator requires a cached body");
                (body, inventory_etag(etag.as_deref()).or(validator))
            }
        };
        let cache = ProviderInventoryCache {
            version: PROVIDER_INVENTORY_CACHE_VERSION,
            provider_id: provider_id.to_owned(),
            inventory_url: inventory_url.clone(),
            credential_fingerprint: credential_fingerprint.to_owned(),
            body: Some(body.clone()),
            etag,
            checked_at: Some(inventory_checked_at()),
        };
        let _ = bootstrap_check(
            format!("inventory-write:{provider_id}"),
            crate::auth::write_private_atomic(
                path,
                &serde_json::to_vec(&cache)?,
                ".provider-models-",
            ),
            |error| format!("warning: could not persist {provider_id} model metadata: {error}"),
        );
        Ok(body)
    });
    match fetched {
        Ok(body) => Ok(body),
        // Never replace a last-good inventory with failure state. A concurrent
        // refresh may have installed one while this request was in flight, so
        // re-read once and use it before surfacing the transient error. Legacy
        // negative markers remain readable, but new failures stay in-process.
        Err(fetch_error) => match load_provider_inventory_cache(
            path,
            provider_id,
            &inventory_url,
            credential_fingerprint,
        ) {
            Ok(Some(CachedProviderInventory::Available(body))) => Ok(body),
            _ => Err(fetch_error),
        },
    }
}

fn cached_provider_inventory(
    provider_id: &'static str,
    inventory_url: String,
    headers: http::HeaderMap,
    credential: &str,
) -> anyhow::Result<Option<serde_json::Value>> {
    let path = provider_inventory_cache_path(provider_id);
    cached_provider_inventory_with_response_fetch(
        path,
        provider_id,
        inventory_url,
        headers,
        credential,
        fetch_provider_inventory,
    )
}

fn cached_provider_inventory_with_response_fetch<F>(
    path: PathBuf,
    provider_id: &'static str,
    inventory_url: String,
    headers: http::HeaderMap,
    credential: &str,
    fetch: F,
) -> anyhow::Result<Option<serde_json::Value>>
where
    F: FnOnce(String, http::HeaderMap) -> anyhow::Result<ProviderInventoryResponse>,
{
    let fingerprint = credential_fingerprint(credential);
    match bootstrap_check(
        format!("inventory-cache:{provider_id}"),
        load_provider_inventory_cache(&path, provider_id, &inventory_url, &fingerprint),
        |error| format!("warning: {provider_id} model cache unavailable: {error}"),
    ) {
        Ok(Some(CachedProviderInventory::Available(body))) => {
            schedule_provider_inventory_refresh(
                path,
                provider_id,
                inventory_url,
                fingerprint,
                headers,
                false,
            );
            Ok(Some(body))
        }
        Ok(Some(CachedProviderInventory::Unavailable)) => {
            // Dynamic-only providers cannot usefully continue without models.
            // Retry in the foreground so a recovered endpoint becomes usable
            // in this launch, rather than refreshing a file that only a later
            // process could observe.
            refresh_provider_inventory_with(
                &path,
                provider_id,
                inventory_url,
                headers,
                &fingerprint,
                fetch,
            )
            .map(Some)
        }
        Ok(None) => refresh_provider_inventory_with(
            &path,
            provider_id,
            inventory_url,
            headers,
            &fingerprint,
            fetch,
        )
        .map(Some),
        Err(cache_error) => {
            let _ = cache_error;
            refresh_provider_inventory_with(
                &path,
                provider_id,
                inventory_url,
                headers,
                &fingerprint,
                fetch,
            )
            .map(Some)
        }
    }
}

#[cfg(test)]
fn fetch_and_cache_provider_inventory_with<F>(
    path: &std::path::Path,
    provider_id: &'static str,
    inventory_url: String,
    headers: http::HeaderMap,
    fingerprint: &str,
    fetch: F,
) -> anyhow::Result<serde_json::Value>
where
    F: FnOnce(String, http::HeaderMap) -> anyhow::Result<serde_json::Value>,
{
    refresh_provider_inventory_with(
        path,
        provider_id,
        inventory_url,
        headers,
        fingerprint,
        |url, headers| {
            fetch(url, headers).map(|body| ProviderInventoryResponse::Modified { body, etag: None })
        },
    )
}

#[cfg(test)]
fn cached_provider_inventory_with_fetch<F>(
    path: PathBuf,
    provider_id: &'static str,
    inventory_url: String,
    headers: http::HeaderMap,
    credential: &str,
    fetch: F,
) -> anyhow::Result<Option<serde_json::Value>>
where
    F: FnOnce(String, http::HeaderMap) -> anyhow::Result<serde_json::Value>,
{
    cached_provider_inventory_with_response_fetch(
        path,
        provider_id,
        inventory_url,
        headers,
        credential,
        |url, headers| {
            fetch(url, headers).map(|body| ProviderInventoryResponse::Modified { body, etag: None })
        },
    )
}

fn cached_provider_inventory_offline(
    provider_id: &'static str,
    inventory_url: String,
    credential: &str,
) -> anyhow::Result<Option<serde_json::Value>> {
    cached_provider_inventory_offline_at(
        provider_inventory_cache_path(provider_id),
        provider_id,
        inventory_url,
        credential,
    )
}

fn cached_provider_inventory_offline_at(
    path: PathBuf,
    provider_id: &'static str,
    inventory_url: String,
    credential: &str,
) -> anyhow::Result<Option<serde_json::Value>> {
    let fingerprint = credential_fingerprint(credential);
    match bootstrap_check(
        format!("inventory-cache:{provider_id}"),
        load_provider_inventory_cache(&path, provider_id, &inventory_url, &fingerprint),
        |error| format!("warning: {provider_id} model cache unavailable: {error}"),
    ) {
        Ok(Some(CachedProviderInventory::Available(body))) => Ok(Some(body)),
        Ok(Some(CachedProviderInventory::Unavailable)) | Ok(None) => Ok(None),
        Err(error) => {
            let _ = error;
            Ok(None)
        }
    }
}

/// Use an existing inventory immediately, but never make startup wait for a
/// cold supplemental catalog. This is used by providers such as OpenCode that
/// already have a substantial embedded model set; discovery fills the cache for
/// the next launch in the background.
fn cached_provider_inventory_or_schedule(
    provider_id: &'static str,
    inventory_url: String,
    headers: http::HeaderMap,
    credential: &str,
) -> Option<serde_json::Value> {
    let path = provider_inventory_cache_path(provider_id);
    let fingerprint = credential_fingerprint(credential);
    match bootstrap_check(
        format!("inventory-cache:{provider_id}"),
        load_provider_inventory_cache(&path, provider_id, &inventory_url, &fingerprint),
        |error| format!("warning: {provider_id} model cache unavailable: {error}"),
    ) {
        Ok(Some(CachedProviderInventory::Available(body))) => {
            schedule_provider_inventory_refresh(
                path,
                provider_id,
                inventory_url,
                fingerprint,
                headers,
                false,
            );
            Some(body)
        }
        Ok(Some(CachedProviderInventory::Unavailable)) => {
            schedule_provider_inventory_refresh(
                path,
                provider_id,
                inventory_url,
                fingerprint,
                headers,
                true,
            );
            None
        }
        Ok(None) => {
            schedule_provider_inventory_refresh(
                path,
                provider_id,
                inventory_url,
                fingerprint,
                headers,
                true,
            );
            None
        }
        Err(error) => {
            let _ = error;
            schedule_provider_inventory_refresh(
                path,
                provider_id,
                inventory_url,
                fingerprint,
                headers,
                true,
            );
            None
        }
    }
}

async fn bounded_discovery_json_async(
    response: reqwest::Response,
    label: &str,
) -> anyhow::Result<serde_json::Value> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DISCOVERY_BODY_BYTES as u64)
    {
        anyhow::bail!(
            "{label} response exceeds the {}-byte limit",
            MAX_DISCOVERY_BODY_BYTES
        );
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if bytes
            .len()
            .checked_add(chunk.len())
            .is_none_or(|length| length > MAX_DISCOVERY_BODY_BYTES)
        {
            anyhow::bail!(
                "{label} response exceeds the {}-byte limit",
                MAX_DISCOVERY_BODY_BYTES
            );
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| anyhow::anyhow!("invalid {label} response: {error}"))
}

/// Conservative fallback for provider `/models` responses that omit
/// architecture metadata. Gemini and Claude families are image-capable by
/// contract, as are the explicitly listed open multimodal families below.
fn model_id_implies_vision(id: &str) -> bool {
    let id = id.to_ascii_lowercase().replace('_', ".");
    id.contains("gemini")
        || id.contains("claude")
        || id.contains("gpt-5.1-codex")
        || id.contains("gpt-5.2-codex")
        || id.contains("gpt-5.3-codex")
        || id.contains("gpt-5.4")
        || id.contains("gpt-5.5")
        || id.contains("gpt-5.6")
        || (id.contains("deepseek") && id.contains("vision"))
        || id.contains("codex-mini")
        || id.contains("qwen3.5")
        || id.contains("qwen3.6")
        || id.contains("qwen2-vl")
        || id.contains("qwen2.5-vl")
        || id.contains("qwen3-vl")
        || id.contains("qwen-vl")
        || id.contains("llava")
        || id.contains("internvl")
        || id.contains("pixtral")
}

/// Shared bounded metadata ingress for hosted discovery, custom bootstrap and
/// guided setup. A malformed assertion fails, absent and unknown stay distinct,
/// and explicit false wins before any route-scoped fallback is consulted.
#[derive(Clone, Debug)]
struct DiscoveredReasoning {
    source: octet_ai::types::ReasoningMetadataSource,
    supported: Option<bool>,
    control: Option<ReasoningControl>,
    options: Option<octet_ai::types::ReasoningOptions>,
    profile: Option<OpenAiChatReasoningMode>,
}

fn decode_reasoning_metadata(entry: &serde_json::Value) -> anyhow::Result<DiscoveredReasoning> {
    use octet_ai::types::{ReasoningMetadataSource as Source, ReasoningOptions};
    let mut fields = Vec::new();
    for metadata in [
        Some(entry),
        entry.get("top_provider"),
        entry.get("provider"),
    ]
    .into_iter()
    .flatten()
    {
        for name in ["reasoning", "supports_reasoning", "reasoning_effort"] {
            if let Some(value) = metadata.get(name) {
                fields.push(value);
            }
            if let Some(value) = metadata.get("capabilities").and_then(|c| c.get(name)) {
                fields.push(value);
            }
        }
    }
    let mut result = DiscoveredReasoning {
        source: Source::Absent,
        supported: None,
        control: None,
        options: None,
        profile: None,
    };
    if fields
        .iter()
        .any(|v| metadata_capability_flag(v) == Some(false))
    {
        result.source = Source::Explicit;
        result.supported = Some(false);
        return Ok(result);
    }
    for field in fields {
        result.source = Source::Unknown;
        if field.is_null() {
            continue;
        }
        anyhow::ensure!(
            field.is_boolean() || field.is_object(),
            "malformed reasoning metadata"
        );
        if let Some(supported) = field.get("supported") {
            anyhow::ensure!(
                supported.is_boolean(),
                "malformed reasoning supported assertion"
            );
        }
        if let Some(supported) = metadata_capability_flag(field) {
            result.supported = Some(supported);
            result.source = Source::Explicit;
        }
        if let Some(control) = field.get("control") {
            let control = match control.as_str() {
                Some("effort" | "levels") => ReasoningControl::Effort,
                Some("toggle" | "binary") => ReasoningControl::Toggle,
                Some("always_on") => ReasoningControl::AlwaysOn,
                Some("token_budget") => ReasoningControl::TokenBudget,
                Some(_) => {
                    result.source = Source::Unknown;
                    result.supported = None;
                    return Ok(result);
                }
                None => anyhow::bail!("malformed reasoning control"),
            };
            result.control = Some(control);
        }
        if let Some(values) = field.get("values") {
            result.options = Some(decode_reasoning_options(values, field.get("default"))?);
        } else if field.get("default").is_some() {
            anyhow::bail!("reasoning default requires exact values");
        }
        if let Some(profile) = field.get("profile") {
            result.profile =
                Some(serde_json::from_value(profile.clone()).context("invalid reasoning profile")?);
        }
    }
    if let Some(values) = entry
        .get("supported_reasoning_levels")
        .or_else(|| entry.get("supported_reasoning_efforts"))
    {
        result.options = Some(decode_reasoning_options(
            values,
            entry
                .get("default_reasoning_level")
                .or_else(|| entry.get("default_reasoning_effort")),
        )?);
        result.source = Source::Explicit;
        result.supported = Some(true);
        result.control = Some(ReasoningControl::Effort);
    }
    if let Some(options) = entry.get("reasoning_options") {
        let options = options
            .as_array()
            .filter(|o| o.len() <= 3)
            .ok_or_else(|| anyhow::anyhow!("malformed reasoning options"))?;
        for option in options {
            match option.get("type").and_then(serde_json::Value::as_str) {
                Some("effort") => {
                    anyhow::ensure!(
                        result.options.is_none(),
                        "conflicting reasoning option sources"
                    );
                    result.options = Some(decode_reasoning_options(
                        option
                            .get("values")
                            .ok_or_else(|| anyhow::anyhow!("missing effort values"))?,
                        option.get("default"),
                    )?);
                    result.control = Some(ReasoningControl::Effort);
                }
                Some("toggle" | "budget_tokens") => {
                    if result.options.is_none() {
                        result.source = Source::Unknown;
                    }
                }
                _ => anyhow::bail!("malformed reasoning options"),
            }
        }
    }
    if result.options.is_none() {
        if let Some(parameters) = entry
            .get("supported_parameters")
            .and_then(serde_json::Value::as_array)
        {
            if parameters.iter().any(|p| {
                matches!(
                    p.as_str(),
                    Some("reasoning_effort" | "reasoning.effort" | "reasoning")
                )
            }) {
                result.supported = Some(true);
                result.source = Source::Explicit;
                if parameters
                    .iter()
                    .any(|p| matches!(p.as_str(), Some("reasoning_effort" | "reasoning.effort")))
                {
                    result.control = Some(ReasoningControl::Effort);
                }
            }
        }
    }
    if let Some(options) = &result.options {
        let choices = options.choices();
        let has_effort = choices
            .iter()
            .any(|v| matches!(v, ReasoningConfig::Effort(_)));
        let inferred = if result.control == Some(ReasoningControl::AlwaysOn) {
            anyhow::ensure!(
                choices.iter().all(|choice| *choice == ReasoningConfig::On),
                "always-on reasoning requires only On choices"
            );
            ReasoningControl::AlwaysOn
        } else if has_effort {
            ReasoningControl::Effort
        } else {
            ReasoningControl::Toggle
        };
        anyhow::ensure!(
            result.control.is_none_or(|c| c == inferred),
            "reasoning control disagrees with exact values"
        );
        result.control = Some(inferred);
        result.supported = Some(choices.iter().any(|v| *v != ReasoningConfig::Off));
        result.source = Source::Explicit;
    }
    // Binary controls are semantic, not an effort range.
    if result.control == Some(ReasoningControl::Toggle) && result.options.is_none() {
        result.options = Some(ReasoningOptions {
            values: vec!["false".into(), "true".into()],
            default: None,
        });
    }
    Ok(result)
}

fn decode_reasoning_options(
    values: &serde_json::Value,
    default: Option<&serde_json::Value>,
) -> anyhow::Result<octet_ai::types::ReasoningOptions> {
    let values = values
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 9)
        .ok_or_else(|| anyhow::anyhow!("invalid reasoning values"))?;
    let values = values
        .iter()
        .map(|v| {
            v.as_str().or_else(|| {
                v.get("effort")
                    .or_else(|| v.get("value"))
                    .and_then(serde_json::Value::as_str)
            })
        })
        .map(|v| {
            v.map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("invalid reasoning value"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let default = default
        .filter(|d| !d.is_null())
        .map(|d| {
            d.as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("invalid reasoning default"))
        })
        .transpose()?;
    let options = octet_ai::types::ReasoningOptions { values, default };
    anyhow::ensure!(
        options.is_valid(),
        "unknown, duplicate or inconsistent reasoning values/default"
    );
    Ok(options)
}

/// Only genuine inventory labels cross the display-name boundary. A synthetic
/// id fallback would obscure builtin spelling and cannot later be distinguished
/// from a user's intentional raw-looking label.
pub(crate) fn discovered_display_name(entry: &serde_json::Value, id: &str) -> Option<String> {
    ["display_name", "name"]
        .into_iter()
        .filter_map(|key| entry.get(key).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .find(|name| !name.is_empty() && *name != id)
        .map(str::to_owned)
}

pub(crate) fn apply_discovered_reasoning(
    entry: &serde_json::Value,
    model: &mut crate::auth::custom::CustomModel,
) -> anyhow::Result<()> {
    let metadata = decode_reasoning_metadata(entry)?;
    model.reasoning_source = Some(metadata.source);
    model.reasoning = metadata.source != octet_ai::types::ReasoningMetadataSource::Unknown
        && metadata.supported.unwrap_or(false);
    model.reasoning_configurable = metadata.control != Some(ReasoningControl::AlwaysOn);
    model.reasoning_profile = metadata.profile;
    model.reasoning_uses_system_message = true;
    if let Some(options) = metadata.options {
        model.reasoning_values = options.values;
        model.reasoning_default = options.default.unwrap_or_default();
    }
    // Generic custom reasoning=true is an explicit endpoint assertion and keeps
    // the legacy effort contract. Hosted boolean metadata is handled separately.
    Ok(())
}

/// Presence, not successful decoding, controls enrichment. Unknown/null and
/// malformed endpoint assertions must never be replaced by catalog optimism.
fn has_metadata_assertion(entry: &serde_json::Value, names: &[&str]) -> bool {
    [
        Some(entry),
        entry.get("provider"),
        entry.get("top_provider"),
    ]
    .into_iter()
    .flatten()
    .any(|metadata| {
        let asserted = |object: &serde_json::Value, name: &str| {
            if let Some((container, field)) = name.split_once('/') {
                object
                    .get(container)
                    .is_some_and(|value| !value.is_object() || value.get(field).is_some())
            } else {
                object.get(name).is_some()
            }
        };
        names.iter().any(|name| asserted(metadata, name))
            || metadata.get("capabilities").is_some_and(|caps| {
                !caps.is_object() || names.iter().any(|name| asserted(caps, name))
            })
    })
}

const MODALITY_FIELDS: &[&str] = &[
    "architecture",
    "input_modalities",
    "modalities",
    "vision",
    "audio",
];
const TOOL_FIELDS: &[&str] = &[
    "supports_tools",
    "tools",
    "tool_call",
    "tool_calling",
    "function_calling",
    "supported_parameters",
];
/// Fields an endpoint may publish instead of, or in addition to, `limit`.
const LIMIT_FIELDS: &[&str] = &[
    "limit",
    "top_provider",
    "context_window",
    "context_length",
    "max_model_len",
    "max_context_tokens",
    "max_output_tokens",
    "max_completion_tokens",
];
const REASONING_FIELDS: &[&str] = &[
    "reasoning",
    "supports_reasoning",
    "reasoning_effort",
    "reasoning_options",
    "supported_reasoning_levels",
    "supported_reasoning_efforts",
    "default_reasoning_level",
    "default_reasoning_effort",
    "interleaved",
];

/// One documented limit from the pinned provider record, if it publishes one.
fn pinned_limit(snapshot: Option<&serde_json::Value>, field: &str) -> Option<u64> {
    snapshot
        .and_then(|snapshot| snapshot.get("limit"))
        .and_then(|limit| positive_u64(limit, &[field]))
}

/// `builtin_display_entry` itself stays name-only. Its caller additionally uses
/// the pinned record for input modalities and, when the endpoint asserts none,
/// documented limits: a sparse inventory that says nothing must not silently
/// downgrade a 1M-token model to a generic placeholder. Operational limits,
/// capability flags and reasoning controls declared by the endpoint (or by a
/// declaration-owned standardized surface) always take precedence, and a model
/// absent from the pinned record gains nothing.
fn builtin_display_entry(
    entry: &serde_json::Value,
    snapshot: &serde_json::Value,
) -> serde_json::Value {
    let mut result = entry.clone();
    if !has_metadata_assertion(entry, &["display_name", "name"]) {
        if let Some(name) = snapshot.get("name") {
            result["display_name"] = name.clone();
        }
    }
    result
}

/// Normalize only a declaration returned by the selected endpoint. Legacy
/// assertions (including null/false/malformed) retain precedence per leaf; the
/// pinned display/pricing supplement never enters this authority boundary.
fn self_described_entry(
    entry: &serde_json::Value,
    endpoint: EndpointId,
    api_name: &str,
    protocol: Protocol,
) -> anyhow::Result<Option<serde_json::Value>> {
    use octet_ai::discovery::{DiscoverySource, ModelSelfDescription};
    let Some(description) = ModelSelfDescription::from_entry(
        entry,
        DiscoverySource {
            endpoint,
            api_name: api_name.to_owned(),
            protocol,
        },
    )?
    else {
        return Ok(None);
    };
    let capabilities = description.capabilities();
    let mut result = entry.clone();
    for (names, key, value) in [
        (
            &[
                "context_window",
                "context_length",
                "max_model_len",
                "max_context_tokens",
                "limit/context",
                "meta/n_ctx",
                "meta/n_ctx_train",
                "status",
            ][..],
            "context_window",
            serde_json::json!(description.limits().context_window),
        ),
        (
            &["max_output_tokens", "max_completion_tokens", "limit/output"][..],
            "max_output_tokens",
            serde_json::json!(description.limits().max_output_tokens),
        ),
        (TOOL_FIELDS, "tools", serde_json::json!(capabilities.tools)),
        (
            &["parallel_tool_calls", "supported_parameters"][..],
            "parallel_tool_calls",
            serde_json::json!(capabilities.parallel_tool_calls),
        ),
        (
            &[
                "structured_output",
                "supports_structured_output",
                "supported_parameters",
            ][..],
            "structured_output",
            serde_json::json!(capabilities.structured_output),
        ),
    ] {
        if !has_metadata_assertion(entry, names) {
            result[key] = value;
        }
    }
    if !has_metadata_assertion(entry, MODALITY_FIELDS) {
        result["input_modalities"] = if capabilities
            .input_modalities
            .contains(octet_ai::Modality::Image)
        {
            serde_json::json!(["text", "image"])
        } else {
            serde_json::json!(["text"])
        };
    }
    if !has_reasoning_assertion(entry) {
        result["reasoning"] = match &capabilities.reasoning {
            Some(reasoning) => {
                let options = reasoning
                    .options
                    .as_ref()
                    .expect("validated exact effort description");
                serde_json::json!({"supported":true,"control":"effort","values":options.values,"default":options.default})
            }
            None => serde_json::json!(false),
        };
    }
    Ok(Some(result))
}

fn has_reasoning_assertion(entry: &serde_json::Value) -> bool {
    has_metadata_assertion(entry, REASONING_FIELDS)
        || [
            Some(entry),
            entry.get("top_provider"),
            entry.get("provider"),
        ]
        .into_iter()
        .flatten()
        .any(|metadata| {
            metadata
                .get("supported_parameters")
                .is_some_and(|parameters| {
                    // A tools-only parameter list is not a negative reasoning
                    // assertion in the existing discovery contract.
                    parameters.as_array().is_none_or(|parameters| {
                        parameters.iter().any(|p| {
                            matches!(
                                p.as_str(),
                                Some("reasoning" | "reasoning_effort" | "reasoning.effort")
                            )
                        })
                    })
                })
        })
}

/// Decode endpoint reasoning metadata without importing semantic controls from
/// the pinned catalog. Declaration-owned profiles are applied later, only when
/// the endpoint did not assert an unknown or malformed reasoning surface.
fn builtin_discovery_reasoning(
    entry: &serde_json::Value,
    declaration: Option<&ProviderDeclaration>,
) -> anyhow::Result<DiscoveredReasoning> {
    let mut metadata = decode_reasoning_metadata(entry)?;
    if declaration.is_some_and(|declaration| declaration.id == "openrouter")
        && metadata.supported != Some(false)
    {
        if let Some(reasoning) = entry.get("reasoning").filter(|value| {
            [
                "mandatory",
                "supported_efforts",
                "default_effort",
                "default_enabled",
            ]
            .iter()
            .any(|key| value.get(*key).is_some())
        }) {
            return decode_openrouter_reasoning(reasoning);
        }
    }
    if metadata.source == octet_ai::types::ReasoningMetadataSource::Absent {
        if advertised_reasoning_parameter(entry)
            && declaration.is_some_and(|declaration| declaration.id == "openrouter")
        {
            // OpenRouter publishes its reasoning primitive through the accepted
            // parameter list rather than a `reasoning` field. Treating that as
            // an undecodable assertion is what left a newly released model with
            // thinking permanently Off until the pinned snapshot was refreshed
            // and the binary rebuilt; the parameter list is the endpoint
            // asserting the capability, so decode it.
            metadata.source = octet_ai::types::ReasoningMetadataSource::Explicit;
            metadata.supported = Some(true);
        } else if has_reasoning_assertion(entry) {
            metadata.source = octet_ai::types::ReasoningMetadataSource::Unknown;
        }
    }
    Ok(metadata)
}

/// OpenRouter's endpoint contract is not the generic `reasoning.values` schema.
/// In particular, accepting a reasoning parameter does not prove that Off (or
/// any particular effort) is supported. Keep the advertised efforts exact.
fn decode_openrouter_reasoning(value: &serde_json::Value) -> anyhow::Result<DiscoveredReasoning> {
    use octet_ai::types::{ReasoningMetadataSource, ReasoningOptions};
    let mandatory = value
        .get("mandatory")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| anyhow::anyhow!("invalid OpenRouter reasoning mandatory flag"))?;
    let enabled = value
        .get("default_enabled")
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("invalid OpenRouter reasoning default_enabled flag"))
        })
        .transpose()?;
    anyhow::ensure!(
        !mandatory || enabled != Some(false),
        "mandatory reasoning cannot default to disabled"
    );
    let (control, options) = if let Some(efforts) = value.get("supported_efforts") {
        let mut options = decode_reasoning_options(efforts, value.get("default_effort"))?;
        anyhow::ensure!(options.values.iter().all(|value| {
            value == "none" || matches!(ReasoningConfig::from_provider_value(value), Some(ReasoningConfig::Effort(e)) if e != octet_ai::ReasoningEffort::Ultra)
        }), "invalid OpenRouter reasoning effort");
        let has_off = options.choices().contains(&ReasoningConfig::Off);
        anyhow::ensure!(
            !mandatory || !has_off,
            "mandatory reasoning cannot advertise Off"
        );
        if !mandatory && !has_off {
            // Optionality explicitly permits Off, but does not advertise an
            // effort named `none`. Keep it separate from the exact effort set.
            options.values.insert(0, "false".into());
        }
        if enabled == Some(false) {
            options.default = Some(if has_off { "none" } else { "false" }.into());
        }
        // `default_enabled: true` does not override an exact `none` default:
        // OpenRouter publishes that combination for optional GPT-5.1 routes.
        (ReasoningControl::Effort, options)
    } else {
        anyhow::ensure!(
            value.get("default_effort").is_none(),
            "reasoning default_effort requires supported_efforts"
        );
        if mandatory {
            (
                ReasoningControl::AlwaysOn,
                ReasoningOptions {
                    values: vec!["default".into()],
                    default: Some("default".into()),
                },
            )
        } else {
            (
                ReasoningControl::Toggle,
                ReasoningOptions {
                    values: vec!["false".into(), "true".into()],
                    default: enabled.map(|enabled| enabled.to_string()),
                },
            )
        }
    };
    Ok(DiscoveredReasoning {
        source: ReasoningMetadataSource::Explicit,
        supported: Some(true),
        control: Some(control),
        options: Some(options),
        profile: None,
    })
}

/// Whether an inventory advertises a reasoning request parameter.
fn advertised_reasoning_parameter(entry: &serde_json::Value) -> bool {
    [
        Some(entry),
        entry.get("top_provider"),
        entry.get("provider"),
    ]
    .into_iter()
    .flatten()
    .any(|metadata| {
        metadata
            .get("supported_parameters")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|parameters| {
                parameters.iter().any(|parameter| {
                    matches!(
                        parameter.as_str(),
                        Some("reasoning" | "reasoning_effort" | "reasoning.effort")
                    )
                })
            })
    })
}

#[derive(Clone, Debug)]
struct DiscoveredApiModel {
    id: String,
    context_window: Option<u64>,
    max_output_tokens: Option<u64>,
    tools: bool,
    parallel_tool_calls: Option<bool>,
    #[cfg(test)]
    reasoning: bool,
    reasoning_metadata: DiscoveredReasoning,
    display_name: Option<String>,
    vision: bool,
    audio: bool,
    modalities_asserted: bool,
    structured_output: Option<bool>,
}

fn is_deepseek_v4_model(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    id == "deepseek-v4" || id.starts_with("deepseek-v4-")
}

/// DeepSeek's sparse V4 inventory omits its documented limits. Preserve any
/// explicit positive metadata, but use the embedded V4 limits instead of the
/// generic placeholder when the provider leaves them out.
fn deepseek_discovered_limits(model: &DiscoveredApiModel) -> (u64, u64) {
    let (default_context_window, default_max_output_tokens) = if is_deepseek_v4_model(&model.id) {
        (
            DEEPSEEK_DEFAULT_CONTEXT_WINDOW,
            DEEPSEEK_DEFAULT_MAX_OUTPUT_TOKENS,
        )
    } else {
        (128_000, 64_000)
    };
    let context_window = model.context_window.unwrap_or(default_context_window);
    let max_output_tokens = model
        .max_output_tokens
        .unwrap_or(default_max_output_tokens)
        .min(context_window);
    (context_window, max_output_tokens)
}

fn metadata_capability_flag(value: &serde_json::Value) -> Option<bool> {
    value
        .as_bool()
        .or_else(|| value.get("supported").and_then(serde_json::Value::as_bool))
}

fn asserted_capability(entry: &serde_json::Value, names: &[&str]) -> Option<bool> {
    let mut supported = None;
    for metadata in [
        Some(entry),
        entry.get("top_provider"),
        entry.get("provider"),
    ]
    .into_iter()
    .flatten()
    {
        for object in [Some(metadata), metadata.get("capabilities")]
            .into_iter()
            .flatten()
        {
            for name in names {
                if let Some(value) = object.get(*name) {
                    match metadata_capability_flag(value) {
                        Some(true) => supported = Some(true),
                        // Unknown/malformed explicit flags are not positive support.
                        Some(false) | None => return Some(false),
                    }
                }
            }
        }
    }
    supported
}

fn discovered_structured_output(entry: &serde_json::Value) -> Option<bool> {
    asserted_capability(entry, &["structured_output", "supports_structured_output"]).or_else(|| {
        has_metadata_assertion(entry, &["supported_parameters"]).then(|| {
            entry
                .get("supported_parameters")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|parameters| {
                    parameters
                        .iter()
                        .any(|p| p.as_str() == Some("response_format"))
                })
        })
    })
}

/// Inventory schemas are not standardized, but the common gateways expose
/// tool support either as a capability flag or as a list of accepted request
/// parameters. Keep unknown distinct from an explicit false so hosted and
/// user-configured local endpoints can apply different safe defaults.
fn model_metadata_tool_support(entry: &serde_json::Value) -> Option<bool> {
    if let Some(supported) = asserted_capability(
        entry,
        &[
            "supports_tools",
            "tools",
            "tool_call",
            "tool_calling",
            "function_calling",
        ],
    ) {
        return Some(supported);
    }
    for metadata in [
        Some(entry),
        entry.get("top_provider"),
        entry.get("provider"),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(parameters) = metadata.get("supported_parameters") {
            return Some(parameters.as_array().is_some_and(|parameters| {
                parameters.iter().any(|parameter| {
                    matches!(
                        parameter.as_str(),
                        Some("tools" | "tool_choice" | "functions" | "function_call")
                    )
                })
            }));
        }
    }
    None
}

/// Hosted inventories must positively advertise tools. Sending schemas to an
/// unknown text-only route can otherwise make an ordinary prompt fail before
/// generation begins.
fn model_metadata_supports_tools(entry: &serde_json::Value) -> bool {
    model_metadata_tool_support(entry).unwrap_or(false)
}

/// Hosted inventories must explicitly advertise reasoning controls. This keeps
/// unverified OpenAI-compatible model names from enabling unsupported requests.
#[cfg(test)]
fn model_metadata_supports_reasoning(entry: &serde_json::Value) -> bool {
    decode_reasoning_metadata(entry)
        .ok()
        .and_then(|m| m.supported)
        .unwrap_or(false)
}

/// A custom endpoint is an explicit user-selected OpenAI-compatible runtime.
/// Preserve octet's historical/local default when its sparse `/models` response
/// says nothing about tools, while still honoring every explicit false.
fn custom_model_metadata_supports_tools(entry: &serde_json::Value) -> bool {
    model_metadata_tool_support(entry)
        .unwrap_or_else(|| !has_metadata_assertion(entry, TOOL_FIELDS))
}

/// Read provider model-inventory modality metadata without assuming a single
/// envelope. OpenAI-compatible servers put it under `architecture`, while
/// several gateways expose it at the top level (and some call it
/// `modalities`). Keeping this normalization in one place prevents a model
/// from being incorrectly treated as text-only just because its inventory
/// shape differs.
fn input_modalities_from_entry(entry: &serde_json::Value) -> ModalitySet {
    let values = entry
        .get("architecture")
        .and_then(|value| value.get("input_modalities"))
        .or_else(|| entry.get("input_modalities"))
        .or_else(|| entry.get("modalities").map(|m| m.get("input").unwrap_or(m)))
        .and_then(serde_json::Value::as_array);
    let mut result = ModalitySet::none();
    for value in values.into_iter().flatten() {
        let Some(value) = value.as_str() else {
            continue;
        };
        let value = value.to_ascii_lowercase();
        if value == "image" || value == "vision" || value.contains("image") {
            result = result.with(octet_ai::Modality::Image);
        }
        if value == "audio" || value.contains("audio") {
            result = result.with(octet_ai::Modality::Audio);
        }
    }
    result
}

/// Parse the two inventory envelopes used by supported providers: OpenAI-style
/// `{ "data": [...] }` and Codex-style `{ "models": [...] }`. Some local
/// servers return the array directly, so that shape is accepted as well.
#[cfg(test)]
fn api_models_from_response(body: &serde_json::Value) -> anyhow::Result<Vec<DiscoveredApiModel>> {
    api_models_from_response_for(body, None)
}

fn api_models_from_response_for(
    body: &serde_json::Value,
    declaration: Option<&ProviderDeclaration>,
) -> anyhow::Result<Vec<DiscoveredApiModel>> {
    let entries = body
        .get("data")
        .or_else(|| body.get("models"))
        .and_then(serde_json::Value::as_array)
        .or_else(|| body.as_array())
        .ok_or_else(|| anyhow::anyhow!("models response has no data/models array"))?;
    let mut models = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(id) = entry
            .get("id")
            .or_else(|| entry.get("slug"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty() && *id != "default")
        else {
            continue;
        };
        let described = declaration
            .and_then(|d| d.route_for_model(id))
            .map(|route| {
                self_described_entry(
                    entry,
                    EndpointId(route.endpoint_id.into()),
                    id,
                    route.protocol,
                )
            })
            .transpose()?
            .flatten();
        let entry = described.as_ref().unwrap_or(entry);
        let snapshot = declaration.and_then(|d| {
            d.route_for_model(id)?;
            octet_ai::model_metadata::model_capability_metadata(d.id, id)
        });
        let reasoning_metadata = builtin_discovery_reasoning(entry, declaration)?;
        let enriched = snapshot
            .as_ref()
            .map(|snapshot| builtin_display_entry(entry, snapshot));
        let entry = enriched.as_ref().unwrap_or(entry);
        // Input modalities are resolved before the enrichment swap so a live
        // assertion always wins. Sparse inventories (for example DeepSeek's
        // model list) assert nothing, so the pinned snapshot may then supply the
        // documented image/audio input; every other pinned field stays out of
        // this authority boundary.
        let modalities_asserted = has_metadata_assertion(entry, MODALITY_FIELDS);
        let input_modalities = if modalities_asserted {
            input_modalities_from_entry(entry)
        } else {
            let asserted = input_modalities_from_entry(entry);
            let pinned = snapshot
                .as_ref()
                .map(input_modalities_from_entry)
                .unwrap_or_else(octet_ai::ModalitySet::none);
            let mut modalities = asserted;
            for candidate in [octet_ai::Modality::Image, octet_ai::Modality::Audio] {
                if pinned.contains(candidate) {
                    modalities = modalities.with(candidate);
                }
            }
            modalities
        };
        let limits_asserted = has_metadata_assertion(entry, LIMIT_FIELDS);
        let vision = asserted_capability(entry, &["vision"]).unwrap_or_else(|| {
            input_modalities.contains(octet_ai::Modality::Image)
                || (!modalities_asserted && model_id_implies_vision(id))
        });
        let audio = asserted_capability(entry, &["audio"])
            .unwrap_or_else(|| input_modalities.contains(octet_ai::Modality::Audio));
        models.push(DiscoveredApiModel {
            id: id.to_owned(),
            context_window: positive_u64(
                entry,
                &[
                    "context_window",
                    "context_length",
                    "max_model_len",
                    "max_context_tokens",
                ],
            )
            .or_else(|| {
                entry
                    .get("limit")
                    .and_then(|limit| positive_u64(limit, &["context"]))
            })
            // Sparse inventories publish identifiers only. When the endpoint
            // asserts no limit field at all, the pinned provider-scoped snapshot
            // supplies its documented window instead of a generic placeholder.
            // A partially asserting endpoint keeps independent leaves and is
            // never mixed with snapshot values.
            .or_else(|| {
                (!limits_asserted)
                    .then(|| pinned_limit(snapshot.as_ref(), "context"))
                    .flatten()
            }),
            max_output_tokens: positive_u64(entry, &["max_output_tokens", "max_completion_tokens"])
                .or_else(|| {
                    entry
                        .get("limit")
                        .and_then(|limit| positive_u64(limit, &["output"]))
                })
                .or_else(|| {
                    entry
                        .get("top_provider")
                        .and_then(|provider| positive_u64(provider, &["max_completion_tokens"]))
                })
                .or_else(|| {
                    (!limits_asserted)
                        .then(|| pinned_limit(snapshot.as_ref(), "output"))
                        .flatten()
                }),
            tools: custom_model_metadata_supports_tools(entry),
            parallel_tool_calls: asserted_capability(entry, &["parallel_tool_calls"]),
            #[cfg(test)]
            reasoning: model_metadata_supports_reasoning(entry),
            reasoning_metadata,
            display_name: discovered_display_name(entry, id),
            vision,
            audio,
            modalities_asserted,
            structured_output: discovered_structured_output(entry),
        });
    }
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    Ok(models)
}

fn get_models_json_blocking(
    url: &str,
    headers: http::HeaderMap,
) -> anyhow::Result<serde_json::Value> {
    let response = blocking_discovery_client(DISCOVERY_TIMEOUT)?
        .get(url)
        .headers(headers)
        .send()
        .map_err(|_| anyhow::anyhow!("model discovery request failed"))?
        .error_for_status()
        .map_err(|_| anyhow::anyhow!("model discovery request was rejected"))?;
    bounded_discovery_json(response, "model discovery")
}

fn has_api_model(catalog: &ModelCatalog, endpoint: &str, api_name: &str) -> bool {
    catalog
        .models()
        .any(|model| model.endpoint.0 == endpoint && model.api_name == api_name)
}

fn declaration_discovery_headers(
    declaration: &ProviderDeclaration,
    credential: &crate::providers::EnvironmentCredential,
) -> anyhow::Result<http::HeaderMap> {
    let route = declaration.inventory_route().ok_or_else(|| {
        anyhow::anyhow!(
            "{} provider declaration has no discovery route",
            declaration.id
        )
    })?;
    let mut headers = crate::providers::environment_discovery_headers(route, credential)?;
    add_declared_headers(&mut headers, declaration)?;
    Ok(headers)
}

fn add_declared_headers(
    target: &mut http::HeaderMap,
    declaration: &ProviderDeclaration,
) -> anyhow::Result<()> {
    let headers = crate::providers::public_headers(declaration.extra_headers)?;
    for (name, value) in &headers {
        target.insert(name.clone(), value.clone());
    }
    Ok(())
}

fn model_filter_matches(filter: ModelFilter, id: &str) -> bool {
    match filter {
        ModelFilter::All => true,
        ModelFilter::Prefix(prefixes) => prefixes.iter().any(|prefix| id.starts_with(prefix)),
    }
}

fn has_model_id(catalog: &ModelCatalog, id: &str) -> bool {
    catalog.resolve(&ModelId(id.to_owned())).is_ok()
}

fn gpt_6_family_model(id: &str) -> bool {
    id.rsplit('/')
        .next()
        .is_some_and(|id| id.to_ascii_lowercase().starts_with("gpt-6"))
}

/// Sparse public OpenAI inventory entries may use the documented GPT-6 family
/// fallback. Other compatible providers must supply capability metadata.
fn public_openai_gpt_6_model(declaration: &ProviderDeclaration, id: &str) -> bool {
    declaration.id == "openai" && matches!(id, "gpt-6-astra" | "gpt-6-sol" | "gpt-6-luna")
}

fn effort_capability(
    mode: OpenAiChatReasoningMode,
    values: &[&str],
    default: Option<&str>,
) -> ReasoningCapability {
    let efforts = values
        .iter()
        .filter_map(|v| match ReasoningConfig::from_provider_value(v) {
            Some(ReasoningConfig::Effort(e)) => Some(e),
            _ => None,
        })
        .collect::<Vec<_>>();
    ReasoningCapability {
        options: Some(octet_ai::types::ReasoningOptions {
            values: values.iter().map(|v| (*v).to_owned()).collect(),
            default: default.map(str::to_owned),
        }),
        control: if efforts.is_empty() {
            ReasoningControl::Toggle
        } else {
            ReasoningControl::Effort
        },
        exposes_text: true,
        preserves_state: true,
        effort_budgets: None,
        openai_chat_mode: mode,
        min_effort: efforts
            .iter()
            .copied()
            .min()
            .unwrap_or(octet_ai::ReasoningEffort::Minimal),
        max_effort: efforts
            .iter()
            .copied()
            .max()
            .unwrap_or(octet_ai::ReasoningEffort::High),
    }
}

/// Source facts are provider-scoped and only applied to an actual inventory
/// entry. This does not inject availability, infer a Qwen server, or set prices.
fn sparse_route_reasoning(
    declaration: &ProviderDeclaration,
    protocol: Protocol,
    id: &str,
) -> Option<ReasoningCapability> {
    use OpenAiChatReasoningMode as Mode;
    if declaration.id == "cerebras" && protocol == Protocol::OpenAiChat {
        // https://inference-docs.cerebras.ai/capabilities/reasoning
        // Official public documentation, not a live /models capture.
        return Some(match id {
            "qwen-3.8-27b" => effort_capability(
                Mode::Cerebras,
                &["none", "low", "medium", "high"],
                Some("high"),
            ),
            "gpt-oss-120b" => {
                effort_capability(Mode::Cerebras, &["low", "medium", "high"], Some("medium"))
            }
            // Dedicated/trial only: never inserted unless actually discovered.
            "gemma-4-31b" => effort_capability(
                Mode::Cerebras,
                &["none", "low", "medium", "high"],
                Some("none"),
            ),
            "kimi-k2.7-code" => {
                let mut c = effort_capability(Mode::Cerebras, &["default"], Some("default"));
                c.control = ReasoningControl::AlwaysOn;
                c
            }
            _ => return None,
        });
    }
    if declaration.id == "deepseek" && protocol == Protocol::OpenAiChat {
        return Some(match id {
            "deepseek-flash" => effort_capability(
                Mode::DeepSeekThinking,
                &["none", "low", "high", "max"],
                None,
            ),
            "deepseek-v4-pro" | "deepseek-v4-flash" | "deepseek-v4" => effort_capability(
                Mode::DeepSeekThinking,
                &["none", "high", "xhigh"],
                Some("high"),
            ),
            "deepseek-reasoner" => {
                let mut c =
                    effort_capability(Mode::DeepSeekThinking, &["default"], Some("default"));
                c.control = ReasoningControl::AlwaysOn;
                c
            }
            _ => return None,
        });
    }
    // Preserve the documented public Responses sparse fallback without leaking
    // a name-based control assertion to Azure deployments or other providers.
    if declaration.id == "openai" && protocol == Protocol::OpenAiResponses {
        if id == "gpt-6-astra" {
            return Some(effort_capability(
                Mode::Standard,
                &["low", "medium", "high", "xhigh", "max"],
                Some("low"),
            ));
        }
        if matches!(id, "gpt-6-sol" | "gpt-6-luna") {
            return Some(effort_capability(
                Mode::Standard,
                &["none", "low", "medium", "high", "xhigh", "max"],
                Some("medium"),
            ));
        }
        if id.starts_with("gpt-5")
            || matches!(id, "o1" | "o3" | "o3-mini" | "o4-mini")
        {
            return Some(effort_capability(
                Mode::Standard,
                &["low", "medium", "high"],
                Some("medium"),
            ));
        }
    }
    None
}

fn discovered_reasoning_capability(
    declaration: &ProviderDeclaration,
    protocol: Protocol,
    id: &str,
    metadata: &DiscoveredReasoning,
) -> Option<ReasoningCapability> {
    if metadata.supported == Some(false) {
        return None;
    }
    let known = sparse_route_reasoning(declaration, protocol, id)
        .or_else(|| declaration.static_reasoning_for(id, protocol));
    if metadata.source == octet_ai::types::ReasoningMetadataSource::Unknown {
        return None;
    }
    if metadata.options.is_none() && metadata.control.is_none() {
        if known.is_some() {
            return known;
        }
        // OpenRouter explicitly advertises its own nested reasoning primitive.
        if declaration.id != "openrouter" || metadata.supported != Some(true) {
            return None;
        }
    }
    if metadata.supported != Some(true) {
        return known;
    }
    let mode = match protocol {
        Protocol::OpenAiChat => match declaration.id {
            "cerebras" => OpenAiChatReasoningMode::Cerebras,
            "deepseek" => {
                if metadata.control == Some(ReasoningControl::Toggle) {
                    OpenAiChatReasoningMode::DeepSeekToggle
                } else {
                    OpenAiChatReasoningMode::DeepSeekThinking
                }
            }
            "openrouter" => OpenAiChatReasoningMode::OpenRouter,
            "together" => OpenAiChatReasoningMode::Together {
                effort: metadata.control == Some(ReasoningControl::Effort),
            },
            // An explicit exact effort schema is an endpoint assertion. Boolean
            // metadata alone above is not enough to select an arbitrary profile.
            _ => known
                .as_ref()
                .map(|capability| capability.openai_chat_mode.clone())
                .unwrap_or(OpenAiChatReasoningMode::SystemMessage),
        },
        Protocol::OpenAiResponses => OpenAiChatReasoningMode::Standard,
        // This codec does not support Conversations reasoning controls yet.
        // Discovery must not manufacture a Chat control for the native route.
        Protocol::MistralConversations => return None,
        // Native codecs need a declaration-owned control/budget contract, but
        // that must not broaden an endpoint's narrower exact choices/default.
        // Pi-messages is native in the same sense: its reasoning state is
        // carried on the wire, so the declaration owns the contract rather than
        // a manufactured Chat control.
        Protocol::AnthropicMessages
        | Protocol::GoogleGenerativeAi
        | Protocol::BedrockConverse
        | Protocol::PiMessages => {
            let mut capability = known?;
            if metadata.control.is_some_and(|control| {
                control != ReasoningControl::Effort && control != capability.control
            }) {
                return None;
            }
            if let Some(options) = &metadata.options {
                if options
                    .choices()
                    .iter()
                    .any(|choice| !capability.supports(choice))
                {
                    return None;
                }
                capability.options = Some(options.clone());
            }
            return Some(capability);
        }
    };
    // A parameter-only OpenRouter inventory proves neither optionality nor
    // effort choices. Use the endpoint default, not a fabricated disabling value.
    if declaration.id == "openrouter" && metadata.options.is_none() {
        let mut capability = effort_capability(mode, &["default"], Some("default"));
        capability.control = ReasoningControl::AlwaysOn;
        return Some(capability);
    }
    let mut capability = known.unwrap_or_else(|| {
        effort_capability(
            mode.clone(),
            &["none", "minimal", "low", "medium", "high"],
            None,
        )
    });
    capability.openai_chat_mode = mode;
    if let Some(options) = &metadata.options {
        let values = options
            .values
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        capability = effort_capability(
            capability.openai_chat_mode,
            &values,
            options.default.as_deref(),
        );
    }
    if let Some(control) = metadata.control {
        capability.control = control;
    }
    if capability.control == ReasoningControl::AlwaysOn {
        capability.options = Some(octet_ai::types::ReasoningOptions {
            values: vec!["default".into()],
            default: Some("default".into()),
        });
    }
    Some(capability)
}

#[cfg(test)]
fn discovered_model_supports_reasoning(
    declaration: &ProviderDeclaration,
    protocol: Protocol,
    id: &str,
) -> bool {
    sparse_route_reasoning(declaration, protocol, id).is_some()
}

fn discovered_preset_binding<'a>(
    declaration: &'a ProviderDeclaration,
    model_id: &str,
) -> Option<&'a ProviderRoute> {
    declaration.route_for_model(model_id)
}

fn register_openai_compatible_models(
    catalog: &mut ModelCatalog,
    declaration: &ProviderDeclaration,
    filter: ModelFilter,
    credential: &crate::providers::EnvironmentCredential,
) -> anyhow::Result<()> {
    let models_url = url::Url::parse(declaration.base_url)?.join("models")?;
    let headers = declaration_discovery_headers(declaration, credential)?;
    let body = match declaration.inventory_cache {
        crate::providers::InventoryCacheMode::Supplemental => {
            cached_provider_inventory_or_schedule(
                declaration.id,
                models_url.to_string(),
                headers,
                credential.value(),
            )
        }
        crate::providers::InventoryCacheMode::Required => cached_provider_inventory(
            declaration.id,
            models_url.to_string(),
            headers,
            credential.value(),
        )?,
    };
    let Some(body) = body else {
        return Ok(());
    };
    register_openai_compatible_models_from_response(catalog, declaration, filter, &body)
}

fn register_openai_compatible_models_from_response(
    catalog: &mut ModelCatalog,
    declaration: &ProviderDeclaration,
    filter: ModelFilter,
    body: &serde_json::Value,
) -> anyhow::Result<()> {
    for model in api_models_from_response_for(body, Some(declaration))? {
        let api_name = model.id.as_str();
        let catalog_id = format!("{}/{}", declaration.id, api_name);
        let Some(route) = discovered_preset_binding(declaration, api_name) else {
            continue;
        };
        if !model_filter_matches(filter, api_name)
            || has_api_model(catalog, route.endpoint_id, api_name)
            || has_model_id(catalog, &catalog_id)
        {
            continue;
        }
        let protocol = route.protocol;
        let public_gpt_6 = protocol == Protocol::OpenAiResponses
            && public_openai_gpt_6_model(declaration, api_name);
        let context_window =
            model
                .context_window
                .unwrap_or(if public_gpt_6 { 1_050_000 } else { 128_000 });
        let max_output_tokens = model
            .max_output_tokens
            .unwrap_or(if public_gpt_6 { 128_000 } else { 32_768 })
            .min(context_window);
        let reasoning = discovered_reasoning_capability(
            declaration,
            protocol,
            api_name,
            &model.reasoning_metadata,
        )
        .filter(|capability| {
            // A native declaration's budget table may not fit the discovered
            // ceiling (or the sparse fallback). Do not enlarge that ceiling or
            // invent a different budget contract to make registration succeed.
            capability
                .effort_budgets
                .is_none_or(|budgets| budgets.max < max_output_tokens)
        });
        // GPT-6 family fallbacks belong only to the verified public OpenAI
        // declaration. Other providers must advertise image input directly.
        let gpt_vision_fallback = declaration
            .discovery_capabilities
            .gpt_vision_fallback(api_name)
            && (!gpt_6_family_model(api_name) || public_openai_gpt_6_model(declaration, api_name));
        let mut input_modalities =
            if model.vision || (!model.modalities_asserted && gpt_vision_fallback) {
                ModalitySet::none().with(octet_ai::Modality::Image)
            } else {
                ModalitySet::none()
            };
        // Audio inventory metadata is only actionable on the Chat codec; the
        // Responses and Anthropic codecs intentionally have no audio mapping.
        if model.audio && protocol == Protocol::OpenAiChat {
            input_modalities = input_modalities.with(octet_ai::Modality::Audio);
        }
        crate::providers::register_discovered_model(
            catalog,
            declaration,
            api_name,
            model.display_name.clone(),
            Capabilities {
                input_modalities,
                output_modalities: ModalitySet::none(),
                tools: model.tools,
                parallel_tool_calls: model.tools
                    && model
                        .parallel_tool_calls
                        .unwrap_or(protocol != Protocol::OpenAiChat),
                reasoning,
                responses_lite: false,
                agent_delegation: None,
                structured_output: model
                    .structured_output
                    .unwrap_or(protocol != Protocol::OpenAiChat),
                deferred_tool_loading: false,
                responses_features: octet_ai::ResponsesFeatures {
                    async_tools: public_gpt_6 && model.tools,
                    steering: public_gpt_6,
                    reasoning_effort_updates: public_gpt_6,
                    compact_reasoning_effort_updates: false,
                },
            },
            ModelLimits {
                context_window,
                max_output_tokens,
            },
            None,
        )?;
    }
    Ok(())
}

fn register_anthropic_compatible_models(
    catalog: &mut ModelCatalog,
    declaration: &ProviderDeclaration,
    filter: ModelFilter,
    credential: &crate::providers::EnvironmentCredential,
) -> anyhow::Result<()> {
    let mut headers = declaration_discovery_headers(declaration, credential)?;
    headers.insert(
        http::HeaderName::from_static("anthropic-version"),
        http::HeaderValue::from_static("2023-06-01"),
    );
    let models_url = url::Url::parse(declaration.base_url)?.join("models?limit=1000")?;
    let Some(body) = cached_provider_inventory(
        declaration.id,
        models_url.to_string(),
        headers,
        credential.value(),
    )?
    else {
        return Ok(());
    };
    register_anthropic_compatible_models_from_response(catalog, declaration, filter, &body)
}

fn register_anthropic_compatible_models_from_response(
    catalog: &mut ModelCatalog,
    declaration: &ProviderDeclaration,
    filter: ModelFilter,
    body: &serde_json::Value,
) -> anyhow::Result<()> {
    for model in api_models_from_response_for(body, Some(declaration))? {
        let api_name = model.id.as_str();
        let catalog_id = format!("{}/{}", declaration.id, api_name);
        let Some(route) = declaration.route_for_model(api_name) else {
            continue;
        };
        if !model_filter_matches(filter, api_name)
            || has_api_model(catalog, route.endpoint_id, api_name)
            || has_model_id(catalog, &catalog_id)
        {
            continue;
        }
        let context_window = model.context_window.unwrap_or(200_000);
        let max_output_tokens = model
            .max_output_tokens
            .unwrap_or(64_000)
            .min(context_window);
        crate::providers::register_discovered_model(
            catalog,
            declaration,
            api_name,
            model.display_name.clone(),
            Capabilities {
                input_modalities: if model.vision
                    || (!model.modalities_asserted
                        && declaration.discovery_capabilities.assumes_image_input())
                {
                    ModalitySet::none().with(octet_ai::Modality::Image)
                } else {
                    ModalitySet::none()
                },
                output_modalities: ModalitySet::none(),
                tools: model.tools,
                parallel_tool_calls: model.tools && model.parallel_tool_calls.unwrap_or(true),
                // Only a separately declared native contract is a fallback;
                // a source boolean cannot invent adaptive-thinking support.
                reasoning: discovered_reasoning_capability(
                    declaration,
                    route.protocol,
                    api_name,
                    &model.reasoning_metadata,
                )
                .filter(|capability| {
                    // Keep the declaration's budgets only when the endpoint's
                    // effective output ceiling can accommodate the full table.
                    capability
                        .effort_budgets
                        .is_none_or(|budgets| budgets.max < max_output_tokens)
                }),
                responses_lite: false,
                agent_delegation: None,
                structured_output: model.structured_output.unwrap_or(true),
                deferred_tool_loading: false,
                responses_features: Default::default(),
            },
            ModelLimits {
                context_window,
                max_output_tokens,
            },
            None,
        )?;
    }
    Ok(())
}

fn deepseek_base_url(declaration: &ProviderDeclaration) -> anyhow::Result<url::Url> {
    let configured =
        optional_env("OCTET_DEEPSEEK_BASE_URL")?.unwrap_or_else(|| declaration.base_url.to_owned());
    let normalized = if configured.ends_with('/') {
        configured
    } else {
        format!("{configured}/")
    };
    url::Url::parse(&normalized)
        .map_err(|error| anyhow::anyhow!("invalid OCTET_DEEPSEEK_BASE_URL: {error}"))
}

fn deepseek_limit(name: &str, default: u64) -> anyhow::Result<u64> {
    let value = match strict_env(name)? {
        Some(value) => value,
        None => return Ok(default),
    };
    value
        .parse()
        .map_err(|error| anyhow::anyhow!("invalid {name}: {error}"))
}

fn declared_deepseek_route(declaration: &ProviderDeclaration) -> anyhow::Result<&ProviderRoute> {
    declaration
        .route_for_model(DEEPSEEK_MODEL_ID)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} declaration has no route for the DeepSeek fallback model",
                declaration.id
            )
        })
}

fn register_deepseek_v4_pro(
    catalog: &mut ModelCatalog,
    declaration: &ProviderDeclaration,
) -> anyhow::Result<()> {
    let api_name =
        optional_env("OCTET_DEEPSEEK_MODEL")?.unwrap_or_else(|| DEEPSEEK_MODEL_ID.to_owned());
    let discovered = discovered_deepseek_spec(catalog, declaration, &api_name);
    let context_window = deepseek_limit(
        "OCTET_DEEPSEEK_CONTEXT_WINDOW",
        discovered
            .as_ref()
            .map_or(DEEPSEEK_DEFAULT_CONTEXT_WINDOW, |m| m.limits.context_window),
    )?;
    let max_output_tokens = deepseek_limit(
        "OCTET_DEEPSEEK_MAX_OUTPUT_TOKENS",
        discovered
            .as_ref()
            .map_or(DEEPSEEK_DEFAULT_MAX_OUTPUT_TOKENS, |m| {
                m.limits.max_output_tokens
            }),
    )?;
    register_deepseek_legacy_alias(
        catalog,
        declaration,
        &api_name,
        context_window,
        max_output_tokens,
    )
}

fn discovered_deepseek_spec(
    catalog: &ModelCatalog,
    declaration: &ProviderDeclaration,
    api_name: &str,
) -> Option<ModelSpec> {
    let route = declaration.route_for_model(api_name)?;
    catalog
        .models()
        .find(|model| {
            model.endpoint.0 == route.endpoint_id
                && model.api_name == api_name
                && model.id.0 != DEEPSEEK_MODEL_ID
        })
        .cloned()
}

/// Keep the historical selector (including OCTET_DEEPSEEK_MODEL routing) without
/// letting a capability seed outrank an actually admitted provider inventory.
fn register_deepseek_legacy_alias(
    catalog: &mut ModelCatalog,
    declaration: &ProviderDeclaration,
    api_name: &str,
    context_window: u64,
    max_output_tokens: u64,
) -> anyhow::Result<()> {
    let route = declared_deepseek_route(declaration)?;
    let endpoint_id = EndpointId(route.endpoint_id.into());
    if !catalog.has_endpoint(&endpoint_id) {
        anyhow::bail!(
            "{} declaration endpoint must be registered before fallback models",
            declaration.id
        );
    }
    if has_model_id(catalog, DEEPSEEK_MODEL_ID) {
        // A pre-existing configured model is not this function's fallback.
        return Ok(());
    }
    if max_output_tokens > context_window {
        anyhow::bail!(
            "OCTET_DEEPSEEK_MAX_OUTPUT_TOKENS must not exceed OCTET_DEEPSEEK_CONTEXT_WINDOW"
        );
    }
    if let Some(mut discovered) = discovered_deepseek_spec(catalog, declaration, api_name) {
        discovered.id = ModelId(DEEPSEEK_MODEL_ID.into());
        discovered.limits = ModelLimits {
            context_window,
            max_output_tokens,
        };
        catalog.register_model(discovered)?;
        return Ok(());
    }
    let cache =
        crate::providers::cache_compatibility(declaration.compatibility, api_name, route.protocol);
    let pricing = crate::providers::pricing_for(declaration, api_name);
    catalog.register_model(ModelSpec {
        preset: Default::default(),
        id: ModelId(DEEPSEEK_MODEL_ID.into()),
        endpoint: endpoint_id,
        api_name: api_name.to_owned(),
        display_name: None,
        protocol: route.protocol,
        capabilities: Capabilities {
            input_modalities: ModalitySet::none(),
            output_modalities: ModalitySet::none(),
            tools: true,
            parallel_tool_calls: false,
            reasoning: Some(ReasoningCapability {
                options: None,
                control: ReasoningControl::Effort,
                exposes_text: true,
                preserves_state: false,
                effort_budgets: None,
                openai_chat_mode: OpenAiChatReasoningMode::DeepSeekThinking,
                min_effort: octet_ai::ReasoningEffort::High,
                max_effort: octet_ai::ReasoningEffort::Xhigh,
            }),
            responses_lite: false,
            agent_delegation: None,
            structured_output: false,

            deferred_tool_loading: false,

            responses_features: Default::default(),
        },
        limits: ModelLimits {
            context_window,
            max_output_tokens,
        },
        pricing,
        cache,
    })?;
    Ok(())
}

fn register_discovered_deepseek_models(
    catalog: &mut ModelCatalog,
    declaration: &ProviderDeclaration,
    credential: &crate::providers::EnvironmentCredential,
    base_url: &url::Url,
) -> anyhow::Result<()> {
    let discovery_route = declared_deepseek_route(declaration)?;
    let url = base_url.join("models")?.to_string();
    let mut headers = crate::providers::environment_discovery_headers(discovery_route, credential)?;
    add_declared_headers(&mut headers, declaration)?;
    let Some(body) = cached_provider_inventory(declaration.id, url, headers, credential.value())?
    else {
        return Ok(());
    };
    register_deepseek_models_from_response(catalog, declaration, &body)
}

fn register_deepseek_models_from_response(
    catalog: &mut ModelCatalog,
    declaration: &ProviderDeclaration,
    body: &serde_json::Value,
) -> anyhow::Result<()> {
    for model in api_models_from_response_for(body, Some(declaration))? {
        let api_name = model.id.as_str();
        let Some(route) = declaration.route_for_model(api_name) else {
            continue;
        };
        if has_api_model(catalog, route.endpoint_id, api_name) {
            continue;
        }
        let reasoning = discovered_reasoning_capability(
            declaration,
            route.protocol,
            api_name,
            &model.reasoning_metadata,
        );
        let (context_window, max_output_tokens) = deepseek_discovered_limits(&model);
        crate::providers::register_discovered_model(
            catalog,
            declaration,
            api_name,
            model.display_name.clone(),
            Capabilities {
                input_modalities: if model.vision {
                    ModalitySet::none().with(octet_ai::Modality::Image)
                } else {
                    ModalitySet::none()
                },
                output_modalities: ModalitySet::none(),
                tools: model.tools,
                parallel_tool_calls: model.tools && model.parallel_tool_calls.unwrap_or(false),
                reasoning,
                responses_lite: false,
                agent_delegation: None,
                structured_output: model.structured_output.unwrap_or(false),

                deferred_tool_loading: false,

                responses_features: Default::default(),
            },
            ModelLimits {
                context_window,
                max_output_tokens,
            },
            None,
        )?;
    }
    Ok(())
}

/// Populate OpenRouter from its live inventory while retaining provider-specific
/// capability and pricing metadata.
fn register_openrouter_models(
    catalog: &mut ModelCatalog,
    declaration: &ProviderDeclaration,
    credential: &crate::providers::EnvironmentCredential,
) -> anyhow::Result<()> {
    let models_url = url::Url::parse(declaration.base_url)?.join("models")?;
    let headers = declaration_discovery_headers(declaration, credential)?;
    let Some(body) = cached_provider_inventory(
        declaration.id,
        models_url.to_string(),
        headers,
        credential.value(),
    )?
    else {
        return Ok(());
    };
    register_openrouter_models_from_response(catalog, declaration, &body)
}

fn openrouter_declaration() -> &'static ProviderDeclaration {
    BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .find(|declaration| declaration.id == "openrouter")
        .expect("built-in OpenRouter declaration")
}

fn register_cached_openrouter_models_offline(catalog: &mut ModelCatalog) -> anyhow::Result<()> {
    let declaration = openrouter_declaration();
    let Some(credential) = crate::providers::resolve_environment(declaration)? else {
        return Ok(());
    };
    crate::providers::register_environment_endpoints(
        catalog,
        declaration,
        &credential,
        PROVIDER_RESPONSE_HEADER_TIMEOUT,
    )?;
    let models_url = url::Url::parse(declaration.base_url)?.join("models")?;
    let Some(body) = cached_provider_inventory_offline(
        declaration.id,
        models_url.to_string(),
        credential.value(),
    )?
    else {
        return Ok(());
    };
    register_openrouter_models_from_response(catalog, declaration, &body)
}

fn register_openrouter_models_from_response(
    catalog: &mut ModelCatalog,
    declaration: &ProviderDeclaration,
    body: &serde_json::Value,
) -> anyhow::Result<()> {
    for model in openrouter_models_from_response(declaration, body)? {
        if !has_model_id(catalog, &model.id.0) {
            catalog.register_model(model)?;
        }
    }
    Ok(())
}

fn openrouter_pricing_value<'a>(
    pricing: &'a serde_json::Value,
    names: &[&str],
) -> Option<&'a serde_json::Value> {
    names.iter().find_map(|name| pricing.get(name))
}

fn openrouter_token_rate(value: Option<&serde_json::Value>) -> Option<TokenRate> {
    let value = value?;
    let raw = match value {
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::Object(object) => {
            return ["value", "price", "rate", "per_token"]
                .iter()
                .find_map(|name| openrouter_token_rate(object.get(*name)));
        }
        _ => return None,
    };
    let raw = raw.trim();
    let (whole, fraction) = raw.split_once('.').unwrap_or((raw, ""));
    if whole.starts_with('-') || !whole.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let whole = whole.parse::<u64>().ok()?.checked_mul(1_000_000_000_000)?;
    if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let mut fractional = fraction
        .bytes()
        .take(12)
        .fold(0u64, |value, digit| value * 10 + u64::from(digit - b'0'));
    let places = fraction.len().min(12);
    fractional = fractional.checked_mul(10u64.pow((12 - places) as u32))?;
    // Round values more precise than one microdollar per million tokens to the
    // nearest representable TokenRate rather than silently charging zero.
    if fraction
        .as_bytes()
        .get(12)
        .is_some_and(|digit| *digit >= b'5')
    {
        fractional = fractional.checked_add(1)?;
    }
    whole.checked_add(fractional).map(TokenRate)
}

fn openrouter_pricing(entry: &serde_json::Value) -> Option<Pricing> {
    let pricing = entry.get("pricing")?;
    let input = openrouter_token_rate(openrouter_pricing_value(pricing, &["prompt", "input"]))?;
    let output =
        openrouter_token_rate(openrouter_pricing_value(pricing, &["completion", "output"]))?;
    let cache_read = openrouter_token_rate(openrouter_pricing_value(
        pricing,
        &["input_cache_read", "cache_read"],
    ))
    .unwrap_or(input);
    let cache_write = openrouter_token_rate(openrouter_pricing_value(
        pricing,
        &["input_cache_write", "cache_write"],
    ))
    .unwrap_or(input);
    let reasoning = openrouter_token_rate(openrouter_pricing_value(
        pricing,
        &["internal_reasoning", "reasoning"],
    ));

    let tiers = pricing
        .get("tiers")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tier| {
            let min_input_tokens = ["min_input_tokens", "min_tokens", "min"]
                .iter()
                .find_map(|name| tier.get(*name).and_then(serde_json::Value::as_u64))?;
            Some(PricingTier {
                min_input_tokens,
                input: openrouter_token_rate(openrouter_pricing_value(tier, &["prompt", "input"])),
                output: openrouter_token_rate(openrouter_pricing_value(
                    tier,
                    &["completion", "output"],
                )),
                cache_read: openrouter_token_rate(openrouter_pricing_value(
                    tier,
                    &["input_cache_read", "cache_read"],
                )),
                cache_write_5m: openrouter_token_rate(openrouter_pricing_value(
                    tier,
                    &["input_cache_write", "cache_write"],
                )),
                cache_write_1h: None,
                reasoning: openrouter_token_rate(openrouter_pricing_value(
                    tier,
                    &["internal_reasoning", "reasoning"],
                )),
            })
        })
        .collect();

    Some(Pricing {
        input,
        output,
        cache_read,
        cache_write_5m: cache_write,
        cache_write_1h: None,
        reasoning,
        tiers,
    })
}

fn openrouter_models_from_response(
    declaration: &ProviderDeclaration,
    body: &serde_json::Value,
) -> anyhow::Result<Vec<ModelSpec>> {
    let entries = body
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("OpenRouter models response is missing a data array"))?;

    let mut models = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(api_name) = entry.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if api_name.trim().is_empty() {
            continue;
        }
        let Some(route) = declaration.route_for_model(api_name) else {
            continue;
        };
        let described = self_described_entry(
            entry,
            EndpointId(route.endpoint_id.into()),
            api_name,
            route.protocol,
        )?;
        let entry = described.as_ref().unwrap_or(entry);
        let snapshot =
            octet_ai::model_metadata::model_capability_metadata(declaration.id, api_name);
        let reasoning_metadata = builtin_discovery_reasoning(entry, Some(declaration))?;
        let enriched = snapshot
            .as_ref()
            .map(|snapshot| builtin_display_entry(entry, snapshot));
        let entry = enriched.as_ref().unwrap_or(entry);
        let context_window = entry
            .get("context_length")
            .or_else(|| entry.get("context_window"))
            .and_then(serde_json::Value::as_u64)
            .filter(|value| *value > 0)
            .unwrap_or(131_072);
        let Some(max_output_tokens) = entry
            .get("top_provider")
            .and_then(|provider| provider.get("max_completion_tokens"))
            .or_else(|| entry.get("max_completion_tokens"))
            .or_else(|| entry.get("max_output_tokens"))
            .and_then(serde_json::Value::as_u64)
            .filter(|value| *value > 0)
            .map(|value| value.min(context_window))
        else {
            // Only the endpoint can supply this ceiling; the pinned display/
            // pricing supplement must not make an incomplete route admissible.
            continue;
        };
        // OpenRouter may expose modality metadata under architecture or at
        // the top level (depending on the inventory proxy). Normalize both so
        // attachments are not rejected before the request reaches the API.
        let mut input_modalities = input_modalities_from_entry(entry);
        if !has_metadata_assertion(entry, MODALITY_FIELDS) && model_id_implies_vision(api_name) {
            input_modalities = input_modalities.with(octet_ai::Modality::Image);
        }
        let supports_tools = model_metadata_supports_tools(entry);

        models.push(ModelSpec {
            preset: Default::default(),
            id: ModelId(format!("{}/{api_name}", declaration.id)),
            endpoint: EndpointId(route.endpoint_id.into()),
            api_name: api_name.into(),
            display_name: discovered_display_name(entry, api_name),
            protocol: route.protocol,
            capabilities: Capabilities {
                input_modalities,
                output_modalities: ModalitySet::none(),
                tools: supports_tools,
                parallel_tool_calls: supports_tools
                    && asserted_capability(entry, &["parallel_tool_calls"]).unwrap_or(false),
                reasoning: discovered_reasoning_capability(
                    declaration,
                    route.protocol,
                    api_name,
                    &reasoning_metadata,
                ),
                responses_lite: false,
                agent_delegation: None,
                structured_output: discovered_structured_output(entry).unwrap_or(false),

                deferred_tool_loading: false,

                responses_features: Default::default(),
            },
            limits: ModelLimits {
                context_window,
                max_output_tokens,
            },
            pricing: if entry.get("pricing").is_some() {
                openrouter_pricing(entry)
            } else {
                crate::providers::pricing_for(declaration, api_name)
            },
            cache: crate::providers::cache_compatibility(
                declaration.compatibility,
                api_name,
                route.protocol,
            ),
        });
    }
    models.sort_by(|left, right| left.api_name.cmp(&right.api_name));
    Ok(models)
}

fn azure_openai_configuration(
    declaration: &ProviderDeclaration,
) -> anyhow::Result<Option<(url::Url, String)>> {
    let endpoint = optional_env("AZURE_OPENAI_ENDPOINT")?;
    let resource = optional_env("AZURE_OPENAI_RESOURCE")?;
    let version = optional_env("AZURE_OPENAI_API_VERSION")?;
    let deployment = optional_env("AZURE_OPENAI_DEPLOYMENT")?;
    azure_openai_configuration_from_values(
        declaration,
        endpoint.as_deref(),
        resource.as_deref(),
        version.as_deref(),
        deployment.as_deref(),
    )
}

fn azure_openai_configuration_from_values(
    declaration: &ProviderDeclaration,
    endpoint: Option<&str>,
    resource: Option<&str>,
    version: Option<&str>,
    deployment: Option<&str>,
) -> anyhow::Result<Option<(url::Url, String)>> {
    let endpoint = match endpoint {
        Some(endpoint) => endpoint.to_owned(),
        None => {
            let Some(resource) = resource else {
                return Ok(None);
            };
            if resource.is_empty()
                || resource.len() > 128
                || !resource
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            {
                anyhow::bail!("invalid AZURE_OPENAI_RESOURCE");
            }
            format!("https://{resource}.openai.azure.com/")
        }
    };
    let mut endpoint =
        url::Url::parse(&endpoint).map_err(|_| anyhow::anyhow!("invalid AZURE_OPENAI_ENDPOINT"))?;
    if !matches!(endpoint.scheme(), "https" | "http")
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        anyhow::bail!("invalid AZURE_OPENAI_ENDPOINT");
    }
    if !endpoint.path().ends_with('/') {
        endpoint.set_path(&format!("{}/", endpoint.path()));
    }
    let mut base_url = endpoint.join("openai/")?;
    let version = version
        .map(str::to_owned)
        .or_else(|| {
            url::Url::parse(declaration.base_url).ok().and_then(|url| {
                url.query_pairs()
                    .find(|(name, _)| name == "api-version")
                    .map(|(_, value)| value.into_owned())
            })
        })
        .ok_or_else(|| anyhow::anyhow!("AZURE_OPENAI_API_VERSION is required"))?;
    if version.is_empty()
        || version.len() > 96
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
    {
        anyhow::bail!("invalid AZURE_OPENAI_API_VERSION");
    }
    base_url.set_query(Some(&format!("api-version={version}")));
    let deployment = deployment
        .ok_or_else(|| anyhow::anyhow!("AZURE_OPENAI_DEPLOYMENT is required"))?
        .to_owned();
    if deployment.is_empty() || deployment.len() > 256 || deployment.chars().any(char::is_control) {
        anyhow::bail!("invalid AZURE_OPENAI_DEPLOYMENT");
    }
    Ok(Some((base_url, deployment)))
}

fn register_azure_openai(
    catalog: &mut ModelCatalog,
    declaration: &ProviderDeclaration,
) -> anyhow::Result<()> {
    let Some(credential) = crate::providers::resolve_environment(declaration)? else {
        return Ok(());
    };
    let Some((base_url, deployment)) = azure_openai_configuration(declaration)? else {
        return Ok(());
    };
    crate::providers::register_environment_endpoints_at_base_url(
        catalog,
        declaration,
        &credential,
        &base_url,
        PROVIDER_RESPONSE_HEADER_TIMEOUT,
    )?;
    let Some(route) = declaration.route_for_model(&deployment) else {
        anyhow::bail!("Azure OpenAI declaration has no default route");
    };
    crate::providers::register_discovered_model(
        catalog,
        declaration,
        &deployment,
        Some(format!("Azure OpenAI: {deployment}")),
        Capabilities {
            input_modalities: ModalitySet::none().with(octet_ai::Modality::Image),
            output_modalities: ModalitySet::none(),
            tools: true,
            parallel_tool_calls: true,
            reasoning: sparse_route_reasoning(declaration, route.protocol, &deployment),
            responses_lite: false,
            agent_delegation: None,
            structured_output: true,
            deferred_tool_loading: false,
            responses_features: Default::default(),
        },
        ModelLimits {
            context_window: 128_000,
            max_output_tokens: 16_384,
        },
        None,
    )
}

fn register_aws_bedrock(
    catalog: &mut ModelCatalog,
    declaration: &ProviderDeclaration,
) -> anyhow::Result<()> {
    let region = crate::providers::aws_bedrock_region()?;
    let Some(auth) = crate::providers::aws_bedrock_auth(&region)? else {
        return Ok(());
    };
    let base_url = crate::providers::aws_bedrock_base_url(&region)?;
    crate::providers::register_private_endpoints_at_base_url(
        catalog,
        declaration,
        auth,
        &base_url,
        PROVIDER_RESPONSE_HEADER_TIMEOUT,
    )?;
    crate::providers::register_static_models(catalog, declaration)
}

fn try_register_declaration(
    catalog: &mut ModelCatalog,
    declaration: &ProviderDeclaration,
) -> anyhow::Result<()> {
    declaration.validate().map_err(|error| {
        anyhow::anyhow!("invalid {} provider declaration: {error}", declaration.id)
    })?;
    match declaration.runtime_configuration {
        ProviderRuntimeConfiguration::AwsBedrock => {
            return register_aws_bedrock(catalog, declaration);
        }
        ProviderRuntimeConfiguration::AzureOpenAi => {
            return register_azure_openai(catalog, declaration);
        }
        ProviderRuntimeConfiguration::Default => {}
    }

    match declaration.authentication {
        ProviderAuthentication::Environment { .. } => {
            try_register_environment_declaration(catalog, declaration)
        }
        ProviderAuthentication::ApplicationDefaultCredentials => {
            let Some(configuration) = crate::providers::resolve_vertex_configuration()? else {
                return Ok(());
            };
            match declaration.model_discovery {
                ModelDiscovery::Static | ModelDiscovery::None => {}
                _ => anyhow::bail!(
                    "dynamic-credential provider {} must use static model discovery",
                    declaration.id
                ),
            }
            crate::providers::register_dynamic_endpoints_at_base_url(
                catalog,
                declaration,
                configuration.auth,
                &configuration.base_url,
                PROVIDER_RESPONSE_HEADER_TIMEOUT,
            )?;
            crate::providers::register_static_models(catalog, declaration)
        }
        // Built-in AWS declarations are handled by their runtime configuration
        // above; no generic endpoint construction can safely invent a signer.
        ProviderAuthentication::Aws { .. } => {
            unreachable!("AWS provider declarations require a runtime configuration")
        }
        // Host-owned credentials can only enter through an explicit embedding
        // integration; preset bootstrap must never synthesize that authority.
        ProviderAuthentication::Subscription { .. } | ProviderAuthentication::HostOwned { .. } => {
            Ok(())
        }
    }
}

fn try_register_environment_declaration(
    catalog: &mut ModelCatalog,
    declaration: &ProviderDeclaration,
) -> anyhow::Result<()> {
    let Some(credential) = crate::providers::resolve_environment(declaration)? else {
        return Ok(());
    };

    if matches!(declaration.model_discovery, ModelDiscovery::DeepSeekModels) {
        let base_url = deepseek_base_url(declaration)?;
        crate::providers::register_environment_endpoints_at_base_url(
            catalog,
            declaration,
            &credential,
            &base_url,
            PROVIDER_RESPONSE_HEADER_TIMEOUT,
        )?;
        register_discovered_deepseek_models(catalog, declaration, &credential, &base_url)?;
        register_deepseek_v4_pro(catalog, declaration)?;
        return Ok(());
    }

    crate::providers::register_environment_endpoints(
        catalog,
        declaration,
        &credential,
        PROVIDER_RESPONSE_HEADER_TIMEOUT,
    )?;
    // Provider discovery owns explicit metadata; static declarations only fill
    // missing inventory afterwards. Configured entries already present still win.
    match declaration.model_discovery {
        ModelDiscovery::Static | ModelDiscovery::None => {}
        ModelDiscovery::OpenAiModels { filter } => {
            register_openai_compatible_models(catalog, declaration, filter, &credential)?;
        }
        ModelDiscovery::AnthropicModels { filter } => {
            register_anthropic_compatible_models(catalog, declaration, filter, &credential)?;
        }
        ModelDiscovery::OpenRouterModels => {
            register_openrouter_models(catalog, declaration, &credential)?;
        }
        ModelDiscovery::DeepSeekModels => unreachable!("handled before endpoint registration"),
        // Host-owned subscription discovery is registered by its embedding
        // integration, never by the environment-backed preset bootstrap.
        ModelDiscovery::CodexSubscription | ModelDiscovery::HostOwnedSubscription => {}
    }
    crate::providers::register_static_models(catalog, declaration)?;
    Ok(())
}

fn merge_provider_catalog(target: &mut ModelCatalog, source: ModelCatalog) -> anyhow::Result<()> {
    let models = source.models().cloned().collect::<Vec<_>>();
    for spec in models {
        let resolved = source.resolve(&spec.id)?;
        if !target.has_endpoint(&resolved.endpoint.id) {
            target.register_endpoint((*resolved.endpoint).clone())?;
        }
        if let Some(label) = source.endpoint_label(&resolved.endpoint.id) {
            target.set_endpoint_label(resolved.endpoint.id.clone(), label.to_owned())?;
        }
        if !has_model_id(target, &spec.id.0) {
            target.register_model(spec)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod stored_api_key_catalog_tests {
    use super::*;
    use crate::provider_setup::BuiltinApiKeyStore;

    fn saved_keys() -> (tempfile::TempDir, BuiltinApiKeyStore) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("credentials/api-keys");
        let store = BuiltinApiKeyStore::for_test(root.clone());
        for provider in ["openai", "anthropic", "gemini"] {
            store
                .save(provider, "synthetic-stored-key".into(), false)
                .unwrap();
        }
        // Subsequent startup resolves a fresh store, not an in-memory key.
        (directory, BuiltinApiKeyStore::for_test(root))
    }

    fn offline_catalog(store: &BuiltinApiKeyStore) -> ModelCatalog {
        let mut catalog = bind_embedded_provider_credentials(
            ModelCatalog::builtin().unwrap(),
            &CatalogReadiness::Fleet,
            |declaration| {
                crate::providers::resolve_environment_with(
                    declaration,
                    |_| Ok(None),
                    |id| store.load(id).map_err(Into::into),
                )
            },
        )
        .unwrap();
        // Exactly the production ordering: bind existing endpoints, then prune.
        // Both embedded endpoints have synthetic stored credentials, so this
        // does not consult ambient environment values or make network requests.
        catalog.retain_configured_models();
        catalog
    }

    #[test]
    fn offline_stored_keys_preserve_exact_embedded_inventory_and_native_auth() {
        let (_directory, store) = saved_keys();
        let catalog = offline_catalog(&store);
        let embedded = ModelCatalog::builtin().unwrap();
        assert_eq!(catalog.models().count(), embedded.models().count());
        assert!(catalog.models().count() > 0);
        for spec in embedded.models() {
            let model = catalog.resolve(&spec.id).unwrap();
            assert_eq!(model.spec.protocol, spec.protocol);
            assert_eq!(model.spec.api_name, spec.api_name);
            assert_eq!(
                model.endpoint.base_url,
                embedded.resolve(&spec.id).unwrap().endpoint.base_url
            );
            match spec.endpoint.0.as_str() {
                "openai" => assert!(matches!(model.endpoint.auth, Auth::Bearer(_))),
                "anthropic" => assert!(
                    matches!(&model.endpoint.auth, Auth::Header { name, .. } if name == "x-api-key")
                ),
                other => panic!("unexpected embedded endpoint: {other}"),
            }
            assert!(!format!("{:?}", model.endpoint.auth).contains("synthetic-stored-key"));
        }
        // Saved Gemini credentials do not authorize expanding offline inventory.
        assert!(!catalog.models().any(|model| model.endpoint.0 == "gemini"));
    }

    #[test]
    fn online_discovery_failure_keeps_stored_key_embedded_fallback() {
        let (_directory, store) = saved_keys();
        let mut catalog = offline_catalog(&store);
        let expected = catalog
            .models()
            .map(|model| model.id.clone())
            .collect::<Vec<_>>();
        merge_declaration_inventory(
            &mut catalog,
            &crate::providers::OPENAI,
            Ok(Err(anyhow::anyhow!("synthetic discovery failure"))),
        );
        catalog.retain_configured_models();
        for id in expected {
            assert!(catalog.resolve(&id).is_ok());
        }
    }

    #[test]
    fn embedded_binding_reads_only_readiness_selected_credential_sources() {
        let (_directory, store) = saved_keys();
        let mut consulted = Vec::new();
        let catalog = bind_embedded_provider_credentials(
            ModelCatalog::builtin().unwrap(),
            &CatalogReadiness::Routes(vec!["openai"]),
            |declaration| {
                consulted.push(declaration.id);
                crate::providers::resolve_environment_with(
                    declaration,
                    |_| Ok(None),
                    |id| store.load(id).map_err(Into::into),
                )
            },
        )
        .unwrap();
        assert_eq!(consulted, ["openai"]);
        for spec in catalog.models() {
            let model = catalog.resolve(&spec.id).unwrap();
            match spec.endpoint.0.as_str() {
                "openai" => assert!(matches!(model.endpoint.auth, Auth::Bearer(_))),
                "anthropic" => assert!(matches!(model.endpoint.auth, Auth::HeaderEnv { .. })),
                other => panic!("unexpected embedded endpoint: {other}"),
            }
        }
        bind_embedded_provider_credentials(
            ModelCatalog::builtin().unwrap(),
            &CatalogReadiness::Routes(vec!["codex"]),
            |_| panic!("Codex readiness cannot inspect unrelated API-key stores"),
        )
        .unwrap();
    }

    #[test]
    fn embedded_binding_preserves_environment_precedence_and_dynamic_aliases() {
        let catalog = bind_embedded_provider_credentials(
            ModelCatalog::builtin().unwrap(),
            &CatalogReadiness::Fleet,
            |declaration| {
                crate::providers::resolve_environment_with(
                    declaration,
                    |_| Ok(Some("synthetic-environment-key".into())),
                    |_| panic!("a configured environment must never access stored keys"),
                )
            },
        )
        .unwrap();
        for spec in catalog.models() {
            let model = catalog.resolve(&spec.id).unwrap();
            assert!(matches!(&model.endpoint.auth, Auth::BearerEnv { var }
                if var == "OPENAI_API_KEY" || var == "ANTHROPIC_AUTH_TOKEN"));
        }
    }

    #[test]
    fn embedded_binding_isolates_invalid_environment_without_stored_fallback() {
        let (_directory, store) = saved_keys();
        let mut catalog = bind_embedded_provider_credentials(
            ModelCatalog::builtin().unwrap(),
            &CatalogReadiness::Fleet,
            |declaration| {
                crate::providers::resolve_environment_with(
                    declaration,
                    |variable| {
                        if declaration.id == "openai" {
                            Err(octet_ai::ConfigError::InvalidEnv(variable.to_owned()))
                        } else {
                            Ok(None)
                        }
                    },
                    |id| {
                        assert_ne!(id, "openai", "invalid environment must not change identity");
                        store.load(id).map_err(Into::into)
                    },
                )
            },
        )
        .unwrap();
        catalog.retain_configured_models();
        assert!(catalog
            .models()
            .any(|model| model.endpoint.0 == "anthropic"));
        assert!(!catalog.models().any(|model| model.endpoint.0 == "openai"));
    }

    #[test]
    fn embedded_binding_isolates_invalid_stored_credentials_from_valid_accounts() {
        let (_directory, store) = saved_keys();
        octet_agent::secure_fs::write_private_atomic(
            &store.path("openai").unwrap(),
            b"synthetic-secret-invalid-json",
            8192,
        )
        .unwrap();
        let mut consulted = Vec::new();
        let mut catalog = bind_embedded_provider_credentials(
            ModelCatalog::builtin().unwrap(),
            &CatalogReadiness::Fleet,
            |declaration| {
                consulted.push(declaration.id);
                crate::providers::resolve_environment_with(
                    declaration,
                    |_| Ok(None),
                    |id| store.load(id).map_err(Into::into),
                )
            },
        )
        .unwrap();
        catalog.retain_configured_models();
        consulted.sort_unstable();
        assert_eq!(consulted, ["anthropic", "openai"]);
        assert!(catalog
            .models()
            .any(|model| model.endpoint.0 == "anthropic"));
        assert!(!catalog.has_endpoint(&EndpointId("openai".into())));
        assert!(!catalog.models().any(|model| model.endpoint.0 == "openai"));
    }
}

/// Declarations whose configuration was consulted while building a catalog.
///
/// Test-only observability for the readiness plan: an unrelated provider being
/// *not consulted at all* is the property under test, and that is a request
/// count rather than a wall-clock threshold.
#[cfg(test)]
pub(crate) fn readiness_declarations_consulted() -> Vec<&'static str> {
    READINESS_DECLARATIONS_CONSULTED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

#[cfg(test)]
pub(crate) fn reset_readiness_declarations_consulted() {
    READINESS_DECLARATIONS_CONSULTED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

#[cfg(test)]
static READINESS_DECLARATIONS_CONSULTED: std::sync::Mutex<Vec<&'static str>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn readiness_note_declaration(provider_id: &'static str) {
    READINESS_DECLARATIONS_CONSULTED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(provider_id);
}

#[cfg(test)]
thread_local! {
    /// Test-only fail-closed probe for the readiness plan.
    ///
    /// While this is set, consulting that declaration panics. The narrowed plan
    /// is initialized sequentially on the calling thread, so a thread-local is
    /// exactly the assertion "this provider was never entered" — the strongest
    /// form of "an unrelated provider cannot delay readiness", with no timing
    /// threshold and no cross-test interference.
    static READINESS_FORBIDDEN_CONSULTATION: std::cell::RefCell<Option<&'static str>> =
        std::cell::RefCell::new(None);
}

/// Test-only guard that makes one declaration's consultation fail closed.
///
/// See [`READINESS_FORBIDDEN_CONSULTATION`]; dropping the guard restores the
/// previous value so guards nest safely.
#[cfg(test)]
pub(crate) struct ForbiddenReadinessConsultation(Option<&'static str>);

#[cfg(test)]
impl ForbiddenReadinessConsultation {
    pub(crate) fn new(provider_id: &'static str) -> Self {
        READINESS_FORBIDDEN_CONSULTATION
            .with(|forbidden| Self(forbidden.replace(Some(provider_id))))
    }
}

#[cfg(test)]
impl Drop for ForbiddenReadinessConsultation {
    fn drop(&mut self) {
        READINESS_FORBIDDEN_CONSULTATION.with(|forbidden| {
            *forbidden.borrow_mut() = self.0;
        });
    }
}

fn declaration_is_configured(declaration: &ProviderDeclaration) -> anyhow::Result<bool> {
    #[cfg(test)]
    readiness_note_declaration(declaration.id);
    #[cfg(test)]
    READINESS_FORBIDDEN_CONSULTATION.with(|forbidden| {
        if forbidden.borrow().as_deref() == Some(declaration.id) {
            panic!(
                "readiness consulted `{}` although the plan never named it; an unrelated provider must not be entered",
                declaration.id
            );
        }
    });
    match declaration.runtime_configuration {
        // The AWS chain includes EC2 instance metadata, which has no local
        // configuration marker. Schedule one bounded private registration job;
        // it decides whether a credential exists and avoids probing the chain
        // once here and again when constructing the endpoint.
        ProviderRuntimeConfiguration::AwsBedrock => Ok(true),
        ProviderRuntimeConfiguration::AzureOpenAi => {
            if crate::providers::resolve_environment(declaration)?.is_none() {
                return Ok(false);
            }
            Ok(azure_openai_configuration(declaration)?.is_some())
        }
        ProviderRuntimeConfiguration::Default => match declaration.authentication {
            ProviderAuthentication::Environment { .. } => {
                Ok(crate::providers::resolve_environment(declaration)?.is_some())
            }
            ProviderAuthentication::ApplicationDefaultCredentials => {
                Ok(crate::providers::resolve_vertex_configuration()?.is_some())
            }
            // A host-owned integration must call CopilotProvider explicitly;
            // environment/configuration bootstrap has no authority to enable it.
            ProviderAuthentication::Subscription { .. }
            | ProviderAuthentication::HostOwned { .. } => Ok(false),
            ProviderAuthentication::Aws { .. } => {
                unreachable!("AWS provider declarations require a runtime configuration")
            }
        },
    }
}

/// Spawn one declaration's bounded inventory discovery on its own thread.
///
/// A fleet sweep runs all configured providers at once; the narrowed readiness
/// path runs exactly the route the selection proved, through this same body, so
/// both initialize a provider identically.
fn spawn_declaration_inventory(
    declaration: &'static ProviderDeclaration,
) -> std::io::Result<std::thread::JoinHandle<anyhow::Result<ModelCatalog>>> {
    std::thread::Builder::new()
        .name(format!("octet-{}-catalog", declaration.id))
        .spawn(move || {
            let mut provider_catalog = ModelCatalog::default();
            try_register_declaration(&mut provider_catalog, declaration)?;
            Ok::<_, anyhow::Error>(provider_catalog)
        })
}

/// Attach a diagnostic to the operation that was actually checked. Calling a
/// different provider, or skipping discovery, cannot clear this component.
fn bootstrap_check<T, E: std::fmt::Display>(
    component: impl Into<String>,
    result: Result<T, E>,
    message: impl FnOnce(&E) -> String,
) -> Result<T, E> {
    crate::output::checked_diagnostics(
        crate::output::DiagnosticComponent::Bootstrap(component.into()),
        result.as_ref().err().map(message).into_iter().collect(),
        true,
    );
    result
}

/// Merge one provider inventory into the launch catalog.
///
/// Discovery is always non-fatal: a provider that cannot authenticate, times
/// out, or panics must not block the other routes, and the same bounded warning
/// is reported on every path.
fn merge_declaration_inventory(
    catalog: &mut ModelCatalog,
    declaration: &ProviderDeclaration,
    outcome: std::thread::Result<anyhow::Result<ModelCatalog>>,
) {
    let mut diagnostics = crate::output::DiagnosticCheck::new(
        crate::output::DiagnosticComponent::Bootstrap(format!("provider-merge:{}", declaration.id)),
    );
    let result = (|| match outcome {
        Ok(Ok(provider_catalog)) => {
            if let Err(error) = merge_provider_catalog(catalog, provider_catalog) {
                diagnostics.problem(format!(
                    "warning: {} unavailable: {error}",
                    declaration.name
                ));
            }
        }
        Ok(Err(error)) => diagnostics.problem(format!(
            "warning: {} unavailable: {error}",
            declaration.name
        )),
        Err(_) => diagnostics.problem(format!(
            "warning: {} unavailable: model discovery thread panicked",
            declaration.name
        )),
    })();
    diagnostics.finish(true);
    result
}

/// Initialize one declaration's inventory on the readiness path.
///
/// Only reached for a route the launch proved it needs (or, below, for one
/// configured compaction override), so this wait is attributable to the user's
/// own selection. It is never the fleet sweep: an unrelated configured provider
/// is not consulted here at all.
fn register_declaration_inventory(
    catalog: &mut ModelCatalog,
    declaration: &'static ProviderDeclaration,
) {
    match bootstrap_check(
        format!("provider-credential:{}", declaration.id),
        declaration_is_configured(declaration),
        |error| format!("warning: {} unavailable: {error}", declaration.name),
    ) {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            let _ = error;
            return;
        }
    }
    match bootstrap_check(
        format!("provider-spawn:{}", declaration.id),
        spawn_declaration_inventory(declaration),
        |error| {
            format!(
                "warning: could not start {} model discovery: {error}",
                declaration.name
            )
        },
    ) {
        Ok(handle) => merge_declaration_inventory(catalog, declaration, handle.join()),
        Err(_) => {}
    }
}

/// Initialize exactly the named declarations, in order.
///
/// The narrowed plan is normally one entry, so the calls are sequential: a
/// wait that cannot be attributed to a single route is worse than a slightly
/// longer one, and the plan already excludes everything the launch cannot use.
/// A named subscription route (Codex) has no environment credential to probe:
/// `declaration_is_configured` returns `false` and the route is initialized by
/// its own catalog-registration branch instead.
fn register_selected_preset_inventories(catalog: &mut ModelCatalog, routes: &[&'static str]) {
    for route in routes {
        if let Some(declaration) =
            readiness_declarations().find(|declaration| declaration.id == *route)
        {
            register_declaration_inventory(catalog, declaration);
        }
    }
    // One boundary for the selected-route inventory wait. `catalog.base` is
    // emitted before it, so the harness reads the delta as the time readiness
    // may attribute to the user's own selection; with a Codex route this phase
    // is skipped because that route initializes inside `catalog.codex`.
    if routes
        .iter()
        .any(|route| *route != crate::providers::CODEX.id)
    {
        startup_phase("catalog.selected");
    }
}

/// Discover configured provider catalogs concurrently, then merge them on the
/// launch thread. A fleet outage therefore costs at most one bounded discovery
/// interval instead of one interval per configured account.
fn register_configured_presets_parallel(catalog: &mut ModelCatalog) {
    let mut jobs = Vec::new();
    for declaration in BUILTIN_PROVIDER_DECLARATIONS {
        match bootstrap_check(
            format!("provider-credential:{}", declaration.id),
            declaration_is_configured(declaration),
            |error| format!("warning: {} unavailable: {error}", declaration.name),
        ) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                // An invalid/oversized optional credential must not become a
                // request, but one unusable provider must not block other
                // configured providers from starting.
                let _ = error;
                continue;
            }
        }
        match bootstrap_check(
            format!("provider-spawn:{}", declaration.id),
            spawn_declaration_inventory(declaration),
            |error| {
                format!(
                    "warning: could not start {} model discovery: {error}",
                    declaration.name
                )
            },
        ) {
            Ok(handle) => jobs.push((declaration, handle)),
            Err(_) => {}
        }
    }

    for (declaration, job) in jobs {
        merge_declaration_inventory(catalog, declaration, job.join());
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CustomModelCache {
    version: u8,
    base_url: String,
    credential_fingerprint: String,
    models: Vec<crate::auth::custom::CustomModel>,
}

enum CachedCustomInventory {
    Available(Vec<crate::auth::custom::CustomModel>),
    Unavailable,
}

fn load_custom_model_cache_for(
    store: &crate::auth::custom::CredentialStore,
    provider_id: &str,
    base_url: &str,
    credential_fingerprint: &str,
) -> anyhow::Result<Option<CachedCustomInventory>> {
    let Some(bytes) = store.load_model_cache_for(provider_id)? else {
        return Ok(None);
    };
    let mut cache: CustomModelCache =
        serde_json::from_slice(&bytes).context("invalid custom model cache")?;
    if cache.version != CUSTOM_MODEL_CACHE_VERSION
        || cache.base_url != base_url
        || cache.credential_fingerprint != credential_fingerprint
    {
        return Ok(None);
    }
    // Only the private configured registry can supply credential-bearing model
    // headers, even if an older or edited inventory cache contains that field.
    for model in &mut cache.models {
        model.preset.headers.clear();
    }
    Ok(Some(if cache.models.is_empty() {
        CachedCustomInventory::Unavailable
    } else {
        CachedCustomInventory::Available(cache.models)
    }))
}

#[cfg(test)]
fn load_custom_model_cache(
    store: &crate::auth::custom::CredentialStore,
    base_url: &str,
    credential_fingerprint: &str,
) -> anyhow::Result<Option<CachedCustomInventory>> {
    load_custom_model_cache_for(
        store,
        crate::auth::custom::ENDPOINT_ID,
        base_url,
        credential_fingerprint,
    )
}

fn save_custom_model_cache_for(
    store: &crate::auth::custom::CredentialStore,
    provider_id: &str,
    base_url: &str,
    credential_fingerprint: &str,
    models: &[crate::auth::custom::CustomModel],
) -> anyhow::Result<()> {
    // Cache a redacted copy, never mutate the credential registry's configured
    // model: those private headers must remain available to actual requests.
    let mut models = models.to_vec();
    for model in &mut models {
        model.preset.headers.clear();
    }
    let cache = CustomModelCache {
        version: CUSTOM_MODEL_CACHE_VERSION,
        base_url: base_url.to_owned(),
        credential_fingerprint: credential_fingerprint.to_owned(),
        models,
    };
    store.save_model_cache_for(provider_id, &serde_json::to_vec_pretty(&cache)?)
}

#[cfg(test)]
fn save_custom_model_cache(
    store: &crate::auth::custom::CredentialStore,
    base_url: &str,
    credential_fingerprint: &str,
    models: &[crate::auth::custom::CustomModel],
) -> anyhow::Result<()> {
    save_custom_model_cache_for(
        store,
        crate::auth::custom::ENDPOINT_ID,
        base_url,
        credential_fingerprint,
        models,
    )
}

fn schedule_custom_model_cache_refresh_for(
    store: crate::auth::custom::CredentialStore,
    provider_id: String,
    cred: crate::auth::custom::CustomCredential,
    credential_fingerprint: String,
    refresh_interval: Duration,
) {
    if cfg!(test)
        || !store
            .model_cache_is_stale_for(&provider_id, refresh_interval)
            .unwrap_or(true)
    {
        return;
    }
    let configured = configured_custom_models(&cred);
    let cache_fingerprint = custom_model_cache_fingerprint(&credential_fingerprint, &configured);
    let _ = std::thread::Builder::new()
        .name(format!("octet-custom-{provider_id}-catalog-refresh"))
        .spawn(move || {
            let discovered = apply_configured_custom_model_overrides(
                apply_known_custom_model_defaults(
                    &cred,
                    discover_models_blocking(&cred, &provider_id, false),
                ),
                &configured,
            );
            if !discovered.is_empty() {
                let _ = save_custom_model_cache_for(
                    &store,
                    &provider_id,
                    &cred.base_url,
                    &cache_fingerprint,
                    &discovered,
                );
            }
        });
}

fn refresh_stale_custom_models_with_for<F>(
    store: &crate::auth::custom::CredentialStore,
    provider_id: &str,
    cred: &crate::auth::custom::CustomCredential,
    credential_fingerprint: &str,
    cached: Vec<crate::auth::custom::CustomModel>,
    refresh_interval: Duration,
    discover: F,
) -> Vec<crate::auth::custom::CustomModel>
where
    F: FnOnce(&crate::auth::custom::CustomCredential) -> Vec<crate::auth::custom::CustomModel>,
{
    if !store
        .model_cache_is_stale_for(provider_id, refresh_interval)
        .unwrap_or(true)
    {
        return cached;
    }

    let discovered = discover_and_cache_custom_models_with_for(
        store,
        provider_id,
        cred,
        credential_fingerprint,
        false,
        discover,
    );
    if discovered.is_empty() {
        // A transient discovery failure must not discard a last-good catalog.
        cached
    } else {
        discovered
    }
}

#[cfg(test)]
fn refresh_stale_custom_models_with<F>(
    store: &crate::auth::custom::CredentialStore,
    cred: &crate::auth::custom::CustomCredential,
    credential_fingerprint: &str,
    cached: Vec<crate::auth::custom::CustomModel>,
    refresh_interval: Duration,
    discover: F,
) -> Vec<crate::auth::custom::CustomModel>
where
    F: FnOnce(&crate::auth::custom::CustomCredential) -> Vec<crate::auth::custom::CustomModel>,
{
    refresh_stale_custom_models_with_for(
        store,
        crate::auth::custom::ENDPOINT_ID,
        cred,
        credential_fingerprint,
        cached,
        refresh_interval,
        discover,
    )
}

fn discover_and_cache_custom_models_with_for<F>(
    store: &crate::auth::custom::CredentialStore,
    provider_id: &str,
    cred: &crate::auth::custom::CustomCredential,
    credential_fingerprint: &str,
    persist_empty: bool,
    discover: F,
) -> Vec<crate::auth::custom::CustomModel>
where
    F: FnOnce(&crate::auth::custom::CustomCredential) -> Vec<crate::auth::custom::CustomModel>,
{
    let discovered = apply_configured_custom_model_overrides(
        apply_known_custom_model_defaults(cred, discover(cred)),
        &configured_custom_models(cred),
    );
    if persist_empty || !discovered.is_empty() {
        let _ = bootstrap_check(
            format!("custom-cache-write:{provider_id}"),
            save_custom_model_cache_for(
                store,
                provider_id,
                &cred.base_url,
                credential_fingerprint,
                &discovered,
            ),
            |error| format!("warning: could not persist custom model metadata: {error}"),
        );
    }
    discovered
}

#[cfg(test)]
fn discover_and_cache_custom_models_with<F>(
    store: &crate::auth::custom::CredentialStore,
    cred: &crate::auth::custom::CustomCredential,
    credential_fingerprint: &str,
    persist_empty: bool,
    discover: F,
) -> Vec<crate::auth::custom::CustomModel>
where
    F: FnOnce(&crate::auth::custom::CustomCredential) -> Vec<crate::auth::custom::CustomModel>,
{
    discover_and_cache_custom_models_with_for(
        store,
        crate::auth::custom::ENDPOINT_ID,
        cred,
        credential_fingerprint,
        persist_empty,
        discover,
    )
}

fn configured_custom_models(
    cred: &crate::auth::custom::CustomCredential,
) -> Vec<crate::auth::custom::CustomModel> {
    if !cred.models.is_empty() {
        cred.models.clone()
    } else if !cred.api_name.is_empty() {
        vec![crate::auth::custom::CustomModel {
            api_name: cred.api_name.clone(),
            display_name: String::new(),
            ..Default::default()
        }]
    } else {
        Vec::new()
    }
}

fn apply_configured_custom_model_overrides(
    discovered: Vec<crate::auth::custom::CustomModel>,
    configured: &[crate::auth::custom::CustomModel],
) -> Vec<crate::auth::custom::CustomModel> {
    if configured.is_empty() {
        return discovered;
    }

    let mut merged = Vec::with_capacity(discovered.len() + configured.len());
    for model in discovered {
        merged.push(
            configured
                .iter()
                .find(|override_model| override_model.api_name == model.api_name)
                .cloned()
                .unwrap_or(model),
        );
    }
    for model in configured {
        if !merged
            .iter()
            .any(|existing| existing.api_name == model.api_name)
        {
            merged.push(model.clone());
        }
    }
    merged
}

fn resolve_custom_startup_timeout(
    configured_secs: Option<u64>,
    environment: Option<&str>,
) -> anyhow::Result<Duration> {
    let seconds = match environment.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value.parse::<u64>().map_err(|error| {
            anyhow::anyhow!("invalid OCTET_CUSTOM_STARTUP_TIMEOUT_SECS: {error}")
        })?,
        None => configured_secs.unwrap_or(CUSTOM_ENDPOINT_STARTUP_TIMEOUT.as_secs()),
    };
    anyhow::ensure!(
        seconds > 0,
        "custom endpoint startup timeout must be greater than zero"
    );
    Ok(Duration::from_secs(seconds))
}

fn custom_reasoning_effort(value: &str) -> Option<octet_ai::ReasoningEffort> {
    match value.trim().to_ascii_lowercase().as_str() {
        "minimal" | "min" => Some(octet_ai::ReasoningEffort::Minimal),
        "low" => Some(octet_ai::ReasoningEffort::Low),
        "medium" | "med" => Some(octet_ai::ReasoningEffort::Medium),
        "high" => Some(octet_ai::ReasoningEffort::High),
        "xhigh" | "x-high" | "extra_high" => Some(octet_ai::ReasoningEffort::Xhigh),
        "max" => Some(octet_ai::ReasoningEffort::Max),
        _ => None,
    }
}

fn custom_reasoning_is_on(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "default" | "on" | "enabled" | "true"
    )
}

#[cfg(test)]
fn discovered_custom_reasoning(entry: &serde_json::Value) -> (bool, Vec<String>, String) {
    let metadata = decode_reasoning_metadata(entry).unwrap();
    let options = metadata.options;
    (
        metadata.supported.unwrap_or(false),
        options
            .as_ref()
            .map(|o| o.values.clone())
            .unwrap_or_default(),
        options.and_then(|o| o.default).unwrap_or_default(),
    )
}

fn custom_reasoning_capability(
    model: &crate::auth::custom::CustomModel,
) -> Option<ReasoningCapability> {
    if !model.reasoning {
        return None;
    }
    let fixed_mode = model.reasoning_profile.clone().unwrap_or({
        if model.reasoning_uses_system_message {
            OpenAiChatReasoningMode::SystemMessage
        } else {
            OpenAiChatReasoningMode::Standard
        }
    });
    if !model.reasoning_configurable {
        // Some providers think by default but reject every reasoning control
        // parameter. Keep that fact visible to octet as a single `on` option while
        // retaining a parameter-free request path.
        return Some(ReasoningCapability {
            options: None,
            control: ReasoningControl::AlwaysOn,
            exposes_text: true,
            preserves_state: false,
            effort_budgets: None,
            openai_chat_mode: fixed_mode,
            min_effort: octet_ai::ReasoningEffort::Minimal,
            max_effort: octet_ai::ReasoningEffort::High,
        });
    }
    let efforts = model
        .reasoning_values
        .iter()
        .filter_map(|value| custom_reasoning_effort(value))
        .collect::<Vec<_>>();
    let control = if !efforts.is_empty() {
        ReasoningControl::Effort
    } else if model
        .reasoning_values
        .iter()
        .any(|value| custom_reasoning_is_on(value))
        || matches!(
            model.reasoning_profile,
            Some(
                OpenAiChatReasoningMode::DeepSeekToggle
                    | OpenAiChatReasoningMode::QwenEnableThinking
                    | OpenAiChatReasoningMode::QwenChatTemplate { .. }
                    | OpenAiChatReasoningMode::Together { effort: false }
            )
        )
    {
        ReasoningControl::Toggle
    } else if model.reasoning_values.is_empty() {
        // Legacy/manual `reasoning = true` configurations predate provider
        // value discovery and retain the portable effort range.
        ReasoningControl::Effort
    } else {
        return None;
    };
    let min_effort = efforts
        .iter()
        .copied()
        .min()
        .unwrap_or(octet_ai::ReasoningEffort::Minimal);
    let max_effort = efforts
        .iter()
        .copied()
        .max()
        .unwrap_or(octet_ai::ReasoningEffort::High);
    let openai_chat_mode = if model.reasoning_values.is_empty() || model.reasoning_profile.is_some()
    {
        fixed_mode
    } else {
        OpenAiChatReasoningMode::ProviderValues {
            values: model.reasoning_values.clone(),
            default: (!model.reasoning_default.is_empty()).then(|| model.reasoning_default.clone()),
            system_message: model.reasoning_uses_system_message,
        }
    };
    Some(ReasoningCapability {
        options: (!model.reasoning_values.is_empty()).then(|| octet_ai::types::ReasoningOptions {
            values: model.reasoning_values.clone(),
            default: (!model.reasoning_default.is_empty()).then(|| model.reasoning_default.clone()),
        }),
        control,
        exposes_text: true,
        preserves_state: false,
        effort_budgets: None,
        openai_chat_mode,
        min_effort,
        max_effort,
    })
}

fn validate_custom_provider_id(provider_id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !provider_id.is_empty()
            && provider_id.len() <= 64
            && provider_id.chars().all(
                |character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            ),
        "custom provider id must be 1-64 ASCII letters, digits, '-' or '_': {provider_id:?}"
    );
    Ok(())
}

fn resolve_custom_provider_auth(
    provider: &crate::auth::custom::CustomProvider,
    legacy_single_endpoint: bool,
) -> anyhow::Result<(Auth, String)> {
    anyhow::ensure!(
        !(provider.auth.is_some() && provider.api_key_env.is_some()),
        "custom provider cannot set both auth and api_key_env"
    );

    let environment_auth = |var: &str| -> anyhow::Result<(Auth, String)> {
        let var = var.trim();
        anyhow::ensure!(
            !var.is_empty()
                && var
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_'),
            "custom provider auth environment variable must be a valid name: {var:?}"
        );
        let key = optional_env(var)?
            .filter(|key| !key.trim().is_empty())
            .unwrap_or_default();
        Ok((Auth::bearer_env(var), key))
    };

    if let Some(auth) = &provider.auth {
        return match auth {
            crate::auth::custom::CustomAuthConfig::None => Ok((Auth::None, String::new())),
            crate::auth::custom::CustomAuthConfig::BearerEnv { var } => environment_auth(var),
        };
    }
    if let Some(var) = provider.api_key_env.as_deref() {
        return environment_auth(var);
    }
    if !provider.credential.api_key.is_empty() {
        return Ok((
            Auth::bearer(provider.credential.api_key.as_str()),
            provider.credential.api_key.clone(),
        ));
    }
    if legacy_single_endpoint {
        let Some(key) = optional_env("OCTET_CUSTOM_API_KEY")?.filter(|key| !key.trim().is_empty())
        else {
            return Ok((Auth::None, String::new()));
        };
        return Ok((Auth::bearer_env("OCTET_CUSTOM_API_KEY"), key));
    }
    Ok((Auth::None, String::new()))
}

const APPLE_FM_PROVIDER_ID: &str = "apple-fm";
const APPLE_FM_BASE_URL: &str = "http://127.0.0.1:1976/v1/";
const APPLE_FM_LABEL: &str = "Apple Foundation Models";
const APPLE_FM_SYSTEM_CONTEXT_WINDOW: u64 = 8_192;
const APPLE_FM_PCC_CONTEXT_WINDOW: u64 = 32_768;
const APPLE_FM_MAX_OUTPUT_TOKENS: u64 = 1_024;
const APPLE_FM_PROBE_TIMEOUT: Duration = Duration::from_millis(250);

fn is_apple_foundation_models_endpoint(base_url: &str) -> bool {
    let Ok(url) = url::Url::parse(base_url) else {
        return false;
    };
    matches!(url.scheme(), "http")
        && url.port() == Some(1976)
        && url
            .host_str()
            .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost" | "::1"))
        && matches!(url.path().trim_end_matches('/'), "/v1")
}

fn custom_model_discovery_is_available(
    base_url: &str,
    apple_server_is_running: impl FnOnce() -> bool,
) -> bool {
    !is_apple_foundation_models_endpoint(base_url) || apple_server_is_running()
}

fn apple_foundation_model_defaults(api_name: &str) -> Option<crate::auth::custom::CustomModel> {
    let (context_window, reasoning_configurable, reasoning_values, reasoning_default) =
        match api_name {
            // The on-device model always thinks and rejects reasoning_effort. Keep
            // it visible as a fixed `on` capability while emitting no control field.
            "system" => (
                APPLE_FM_SYSTEM_CONTEXT_WINDOW,
                false,
                Vec::new(),
                String::new(),
            ),
            // fm serve accepts low/medium/high for the PCC route. PCC may be
            // unavailable on this device, but its wire contract is still stable.
            "pcc" => (
                APPLE_FM_PCC_CONTEXT_WINDOW,
                true,
                vec!["low".to_owned(), "medium".to_owned(), "high".to_owned()],
                "medium".to_owned(),
            ),
            _ => return None,
        };
    Some(crate::auth::custom::CustomModel {
        api_name: api_name.to_owned(),
        display_name: api_name.to_owned(),
        context_window,
        max_output_tokens: APPLE_FM_MAX_OUTPUT_TOKENS,
        tools: true,
        parallel_tool_calls: false,
        vision: false,
        structured_output: false,
        reasoning: true,
        reasoning_profile: None,
        reasoning_source: Some(octet_ai::types::ReasoningMetadataSource::Explicit),
        reasoning_configurable,
        reasoning_values,
        reasoning_default,
        reasoning_uses_system_message: true,
        pricing: None,
        preset: Default::default(),
    })
}

fn apply_known_custom_model_defaults(
    cred: &crate::auth::custom::CustomCredential,
    models: Vec<crate::auth::custom::CustomModel>,
) -> Vec<crate::auth::custom::CustomModel> {
    if !is_apple_foundation_models_endpoint(&cred.base_url) {
        return models;
    }
    models
        .into_iter()
        .map(|model| {
            if model.reasoning_source == Some(octet_ai::types::ReasoningMetadataSource::Absent) {
                apple_foundation_model_defaults(&model.api_name).unwrap_or(model)
            } else {
                model
            }
        })
        .collect()
}

fn apple_foundation_models_health_is_valid(body: &serde_json::Value) -> bool {
    body.get("status").and_then(serde_json::Value::as_str) == Some("fm serve is running")
        && body
            .get("models")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|models| {
                models.iter().any(|model| {
                    model.get("name").and_then(serde_json::Value::as_str) == Some("system")
                        && model.get("available").and_then(serde_json::Value::as_bool) == Some(true)
                })
            })
}

fn apple_foundation_models_server_is_running() -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    std::thread::spawn(apple_foundation_models_server_is_running_blocking)
        .join()
        .unwrap_or(false)
}

fn apple_foundation_models_server_is_running_blocking() -> bool {
    let Ok(health_url) = url::Url::parse(APPLE_FM_BASE_URL).and_then(|url| url.join("../health"))
    else {
        return false;
    };
    let Ok(client) = blocking_discovery_client(APPLE_FM_PROBE_TIMEOUT) else {
        return false;
    };
    let Ok(response) = client.get(health_url).send() else {
        return false;
    };
    if !response.status().is_success() {
        return false;
    }
    bounded_discovery_json(response, "Apple Foundation Models health")
        .is_ok_and(|body| apple_foundation_models_health_is_valid(&body))
}

fn default_apple_foundation_models_provider() -> crate::auth::custom::CustomProvider {
    crate::auth::custom::CustomProvider {
        label: APPLE_FM_LABEL.to_owned(),
        credential: crate::auth::custom::CustomCredential {
            base_url: APPLE_FM_BASE_URL.to_owned(),
            api_key: String::new(),
            api_name: String::new(),
            headers: Vec::new(),
            models: Vec::new(),
            auto_discover: true,
        },
        auth: Some(crate::auth::custom::CustomAuthConfig::None),
        api_key_env: None,
        cache: None,
        startup_timeout_secs: Some(CUSTOM_ENDPOINT_STARTUP_TIMEOUT.as_secs()),
        lifecycle_feedback: false,
    }
}

/// Trusted pricing for a custom-provider model.
///
/// User-configured endpoints are treated as user-trusted: a declared
/// `pricing` block is honored, and an undeclared one defaults to zero rates
/// so local/self-hosted models count as free while still satisfying
/// guardrails that require trusted model pricing (such as subagent cost
/// ceilings).
fn custom_model_pricing(model: &crate::auth::custom::CustomModel) -> Pricing {
    let rates: crate::auth::custom::CustomPricing = model.pricing.unwrap_or_default();
    Pricing {
        input: TokenRate(rates.input),
        output: TokenRate(rates.output),
        cache_read: TokenRate(rates.cache_read),
        cache_write_5m: TokenRate(rates.cache_write_5m),
        cache_write_1h: None,
        reasoning: None,
        tiers: Vec::new(),
    }
}

fn custom_model_id(
    provider_id: &str,
    legacy_single_endpoint: bool,
    model: &crate::auth::custom::CustomModel,
) -> String {
    if legacy_single_endpoint {
        let configured_display =
            (!model.display_name.trim().is_empty()).then(|| model.display_name.trim().to_owned());
        let canonical_label = configured_display.as_deref().unwrap_or(&model.api_name);
        format!("custom/{canonical_label}")
    } else {
        format!("custom/{provider_id}/{}", model.api_name)
    }
}

fn register_custom_openai_endpoints_from_store(
    catalog: &mut ModelCatalog,
    store: &crate::auth::custom::CredentialStore,
    offline: bool,
) -> anyhow::Result<()> {
    let Some(registry) = store.load_registry()? else {
        return Ok(());
    };
    // Custom inventories are independent and have provider-specific cache
    // paths. Bound concurrency rather than multiplying a cold endpoint's
    // discovery deadline by every configured provider. Join and merge in the
    // original registry order before publishing a complete catalog.
    const MAX_CONCURRENT_CUSTOM_PROVIDERS: usize = 4;
    let providers = registry.providers.iter().collect::<Vec<_>>();
    for batch in providers.chunks(MAX_CONCURRENT_CUSTOM_PROVIDERS) {
        std::thread::scope(|scope| {
            let build = |provider_id: &String, provider: &crate::auth::custom::CustomProvider| {
                let legacy_single_endpoint = registry.legacy_single_endpoint
                    && provider_id == crate::auth::custom::ENDPOINT_ID;
                let mut provider_catalog = ModelCatalog::default();
                register_custom_openai_provider(
                    &mut provider_catalog,
                    store,
                    provider_id,
                    provider,
                    legacy_single_endpoint,
                    offline,
                )?;
                Ok::<_, anyhow::Error>(provider_catalog)
            };
            let jobs = batch
                .iter()
                .map(|&(provider_id, provider)| {
                    // No threads for offline/manual inventories or a single provider.
                    if offline || !provider.credential.auto_discover || batch.len() == 1 {
                        return None;
                    }
                    std::thread::Builder::new()
                        .name("octet-custom-catalog".into())
                        .spawn_scoped(scope, move || build(provider_id, provider))
                        .ok()
                })
                .collect::<Vec<_>>();
            for (&(provider_id, provider), job) in batch.iter().zip(jobs) {
                // Thread-resource exhaustion falls back to the same validation
                // and discovery path instead of silently dropping a provider.
                let result = match job {
                    Some(job) => job.join().unwrap_or_else(|_| {
                        Err(anyhow::anyhow!("custom model discovery thread panicked"))
                    }),
                    None => build(provider_id, provider),
                };
                let label = provider.label.trim();
                let label = if label.is_empty() { provider_id } else { label };
                match bootstrap_check(format!("custom-provider:{provider_id}"), result, |error| {
                    format!("warning: custom provider {label:?} unavailable: {error}")
                }) {
                    Ok(provider_catalog) => {
                        let _ = bootstrap_check(
                            format!("custom-merge:{provider_id}"),
                            merge_provider_catalog(catalog, provider_catalog),
                            |error| {
                                format!(
                                    "warning: custom provider {provider_id:?} unavailable: {error}"
                                )
                            },
                        );
                    }
                    Err(_) => {}
                }
            }
        });
    }
    Ok(())
}

fn register_default_apple_foundation_models(
    catalog: &mut ModelCatalog,
    store: &crate::auth::custom::CredentialStore,
    offline: bool,
) -> anyhow::Result<()> {
    if offline
        || !cfg!(target_os = "macos")
        || catalog.has_endpoint(&EndpointId(crate::auth::custom::endpoint_id(
            APPLE_FM_PROVIDER_ID,
        )))
        || !apple_foundation_models_server_is_running()
    {
        return Ok(());
    }

    let provider = default_apple_foundation_models_provider();
    let mut provider_catalog = ModelCatalog::default();
    register_custom_openai_provider(
        &mut provider_catalog,
        store,
        APPLE_FM_PROVIDER_ID,
        &provider,
        false,
        false,
    )?;
    merge_provider_catalog(catalog, provider_catalog)
}

fn register_custom_openai_provider(
    catalog: &mut ModelCatalog,
    store: &crate::auth::custom::CredentialStore,
    provider_id: &str,
    provider: &crate::auth::custom::CustomProvider,
    legacy_single_endpoint: bool,
    offline: bool,
) -> anyhow::Result<()> {
    use crate::auth::custom::CustomModel;

    validate_custom_provider_id(provider_id)?;
    let (auth, effective_key) = resolve_custom_provider_auth(provider, legacy_single_endpoint)?;
    let mut cred = provider.credential.clone();
    // Discovery uses the resolved value in memory; it is never written back to
    // the provider registry. Requests use the redacted Auth strategy below.
    cred.api_key = effective_key.clone();

    let startup_timeout_env = if legacy_single_endpoint {
        optional_env("OCTET_CUSTOM_STARTUP_TIMEOUT_SECS")?
    } else {
        None
    };
    let startup_timeout = resolve_custom_startup_timeout(
        provider.startup_timeout_secs,
        startup_timeout_env.as_deref(),
    )?;

    let base_url = if cred.base_url.ends_with('/') {
        url::Url::parse(&cred.base_url)
    } else {
        url::Url::parse(&format!("{}/", cred.base_url))
    }
    .map_err(|_| anyhow::anyhow!("invalid custom provider {provider_id:?} base URL"))?;

    let mut default_headers = http::HeaderMap::new();
    for header in &cred.headers {
        let name = http::HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|e| anyhow::anyhow!("invalid header name {}: {e}", header.name))?;
        let value = http::HeaderValue::from_str(&header.value)
            .map_err(|e| anyhow::anyhow!("invalid header value for {}: {e}", header.name))?;
        default_headers.insert(name, value);
    }
    let custom_credential_fingerprint =
        custom_credential_fingerprint(&effective_key, &default_headers);
    let endpoint_id = EndpointId(crate::auth::custom::endpoint_id(provider_id));

    catalog.register_endpoint(Endpoint {
        id: endpoint_id.clone(),
        base_url,
        auth,
        default_headers,
        transport: octet_ai::EndpointTransport::Http,
        runtime: octet_ai::RequestRuntime {
            lifecycle_feedback: provider.lifecycle_feedback,
            ..octet_ai::RequestRuntime::default()
        },
        timeout: startup_timeout,
    })?;
    let label = provider.label.trim();
    let label = if label.is_empty() {
        if legacy_single_endpoint {
            "local endpoint"
        } else {
            provider_id
        }
    } else {
        label
    };
    catalog.set_endpoint_label(endpoint_id.clone(), label.to_owned())?;

    // A successful inventory is durable startup metadata, not something every
    // invocation should fetch again. The provider-specific cache path prevents
    // one endpoint's inventory from being used by another endpoint.
    let configured = configured_custom_models(&cred);
    let configured_overrides = configured.clone();
    let cache_fingerprint =
        custom_model_cache_fingerprint(&custom_credential_fingerprint, &configured);
    let cached = if cred.auto_discover {
        match bootstrap_check(
            format!("custom-cache:{provider_id}"),
            load_custom_model_cache_for(store, provider_id, &cred.base_url, &cache_fingerprint),
            |error| format!("warning: custom provider model cache unavailable: {error}"),
        ) {
            Ok(models) => models,
            Err(_) => None,
        }
    } else {
        None
    };
    let models: Vec<CustomModel> = match cached {
        Some(CachedCustomInventory::Available(models)) => {
            if offline {
                models
            } else {
                refresh_stale_custom_models_with_for(
                    store,
                    provider_id,
                    &cred,
                    &cache_fingerprint,
                    models,
                    PROVIDER_INVENTORY_REFRESH_INTERVAL,
                    |cred| discover_models(cred, provider_id),
                )
            }
        }
        Some(CachedCustomInventory::Unavailable)
            if cred.auto_discover && !offline && configured.is_empty() =>
        {
            let discovered = discover_and_cache_custom_models_with_for(
                store,
                provider_id,
                &cred,
                &cache_fingerprint,
                false,
                |cred| discover_models(cred, provider_id),
            );
            if !discovered.is_empty() {
                discovered
            } else {
                configured
            }
        }
        Some(CachedCustomInventory::Unavailable) => {
            if !offline {
                schedule_custom_model_cache_refresh_for(
                    store.clone(),
                    provider_id.to_owned(),
                    cred.clone(),
                    custom_credential_fingerprint.clone(),
                    NEGATIVE_INVENTORY_REFRESH_INTERVAL,
                );
            }
            configured
        }
        None if cred.auto_discover && !offline => {
            let discovered = discover_and_cache_custom_models_with_for(
                store,
                provider_id,
                &cred,
                &cache_fingerprint,
                true,
                |cred| discover_models(cred, provider_id),
            );
            if discovered.is_empty() {
                configured
            } else {
                discovered
            }
        }
        None => configured,
    };
    let models = apply_configured_custom_model_overrides(
        apply_known_custom_model_defaults(&cred, models),
        &configured_overrides,
    );
    if models.is_empty() {
        return Ok(());
    }

    let cache = provider.cache.clone().unwrap_or_default();
    for model in &models {
        if !model.reasoning_values.is_empty() {
            anyhow::ensure!(
                octet_ai::types::ReasoningOptions {
                    values: model.reasoning_values.clone(),
                    default: (!model.reasoning_default.is_empty())
                        .then(|| model.reasoning_default.clone())
                }
                .is_valid(),
                "invalid custom reasoning values/default"
            );
        } else {
            anyhow::ensure!(
                model.reasoning_default.is_empty(),
                "custom reasoning default requires exact values"
            );
        }
        let configured_display =
            (!model.display_name.trim().is_empty()).then(|| model.display_name.trim().to_owned());
        let input_mods = if model.vision {
            ModalitySet::none().with(octet_ai::Modality::Image)
        } else {
            ModalitySet::none()
        };

        catalog.register_model(ModelSpec {
            preset: model.preset.clone(),
            id: ModelId(custom_model_id(provider_id, legacy_single_endpoint, model)),
            endpoint: endpoint_id.clone(),
            api_name: model.api_name.clone(),
            display_name: configured_display,
            protocol: Protocol::OpenAiChat,
            capabilities: Capabilities {
                input_modalities: input_mods,
                output_modalities: ModalitySet::none(),
                tools: model.tools,
                parallel_tool_calls: model.tools && model.parallel_tool_calls,
                reasoning: custom_reasoning_capability(model),
                responses_lite: false,
                agent_delegation: None,
                structured_output: model.structured_output,

                deferred_tool_loading: false,

                responses_features: Default::default(),
            },
            limits: ModelLimits {
                context_window: model.context_window,
                max_output_tokens: model.max_output_tokens,
            },
            pricing: Some(custom_model_pricing(model)),
            cache: cache.clone(),
        })?;
    }
    Ok(())
}

/// Call GET /v1/models on the custom endpoint and convert the response into
/// `CustomModel` entries. Returns an empty Vec on any error (non-fatal).
fn discover_models(
    cred: &crate::auth::custom::CustomCredential,
    provider_id: &str,
) -> Vec<crate::auth::custom::CustomModel> {
    // Apple Foundation Models is an optional local integration. Its health
    // endpoint gives us a cheap, exact readiness signal, so do not issue the
    // noisier /v1/models request when `fm serve` is absent.
    if !custom_model_discovery_is_available(
        &cred.base_url,
        apple_foundation_models_server_is_running,
    ) {
        return Vec::new();
    }

    // Run blocking HTTP work on a separate thread so the reqwest::blocking
    // Client's internal tokio runtime is created and dropped outside the
    // outer #[tokio::main] async context, avoiding:
    //   "Cannot drop a runtime in a context where blocking is not allowed."
    let cred = cred.clone();
    let provider_id = provider_id.to_owned();
    std::thread::spawn(move || discover_models_blocking(&cred, &provider_id, true))
        .join()
        .unwrap_or_default()
}

fn discover_models_blocking(
    cred: &crate::auth::custom::CustomCredential,
    provider_id: &str,
    report_errors: bool,
) -> Vec<crate::auth::custom::CustomModel> {
    let mut diagnostics = crate::output::DiagnosticCheck::new(
        crate::output::DiagnosticComponent::Bootstrap(format!("custom-discovery:{provider_id}")),
    );
    let result = (|| {
        use crate::auth::custom::CustomModel;

        // Build the models URL following octet's convention: base_url is versioned
        // (e.g. http://host/v1/) and we join the path segment.
        let base = if cred.base_url.ends_with('/') {
            cred.base_url.clone()
        } else {
            format!("{}/", cred.base_url)
        };
        let models_url = match url::Url::parse(&base).and_then(|u| u.join("models")) {
            Ok(u) => u.to_string(),
            Err(e) => {
                if report_errors {
                    diagnostics.problem(format!("warning: auto-discover URL parse failed: {e}"));
                }
                return Vec::new();
            }
        };

        let client = match blocking_discovery_client(std::time::Duration::from_secs(10)) {
            Ok(c) => c,
            Err(e) => {
                if report_errors {
                    diagnostics.problem(format!("warning: auto-discover client build failed: {e}"));
                }
                return Vec::new();
            }
        };

        let mut req = client.get(&models_url);
        let discovery_key = (!cred.api_key.trim().is_empty()).then(|| cred.api_key.clone());
        if let Some(key) = discovery_key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }
        for h in &cred.headers {
            req = req.header(&h.name, &h.value);
        }

        let resp = match req
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
        {
            Ok(r) => r,
            Err(e) => {
                if report_errors {
                    diagnostics.problem(format!(
                        "warning: auto-discover GET {} failed: {e}",
                        models_url
                    ));
                }
                return Vec::new();
            }
        };

        let status = resp.status();
        let body = match bounded_discovery_json(resp, "custom models") {
            Ok(value) => value,
            Err(error) => {
                if report_errors {
                    diagnostics.problem(format!(
                        "warning: auto-discover {} returned HTTP {} with an invalid or oversized body: {error}",
                        models_url,
                        status.as_u16())
                    );
                }
                return Vec::new();
            }
        };

        let data = match body
            .get("data")
            .or_else(|| body.get("models"))
            .and_then(serde_json::Value::as_array)
            .or_else(|| body.as_array())
        {
            Some(arr) => arr,
            None => {
                if report_errors {
                    diagnostics.problem(format!(
                        "warning: auto-discover {} missing 'data'/'models' array",
                        models_url
                    ));
                }
                return Vec::new();
            }
        };

        let mut models = Vec::new();
        for entry in data {
            let id = entry
                .get("id")
                .or_else(|| entry.get("slug"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if id.is_empty() || id == "default" {
                continue;
            }

            let described = match self_described_entry(
                entry,
                EndpointId(crate::auth::custom::endpoint_id(provider_id)),
                id,
                Protocol::OpenAiChat,
            ) {
                Ok(value) => value,
                Err(_) => {
                    if report_errors {
                        diagnostics.problem(format!(
                            "warning: invalid endpoint capability self-description"
                        ));
                    }
                    continue;
                }
            };
            let entry = described.as_ref().unwrap_or(entry);
            let ctx = extract_ctx_from_model_entry(entry);
            let vision = entry
                .get("architecture")
                .and_then(|a| a.get("input_modalities"))
                .and_then(|m| m.as_array())
                .map(|arr| arr.iter().any(|v| v.as_str() == Some("image")))
                .unwrap_or(false)
                || input_modalities_from_entry(entry).contains(octet_ai::Modality::Image)
                || (!has_metadata_assertion(entry, MODALITY_FIELDS) && model_id_implies_vision(id));
            let vision = asserted_capability(entry, &["vision"]).unwrap_or(vision);

            let supported_parameters = entry
                .get("supported_parameters")
                .and_then(serde_json::Value::as_array);
            let supports = |name: &str| {
                supported_parameters.is_some_and(|parameters| {
                    parameters
                        .iter()
                        .any(|parameter| parameter.as_str() == Some(name))
                })
            };
            let max_output_tokens =
                positive_u64(entry, &["max_output_tokens", "max_completion_tokens"])
                    .unwrap_or(16_384)
                    .min(ctx);
            let mut model = CustomModel {
                api_name: id.to_string(),
                display_name: discovered_display_name(entry, id).unwrap_or_default(),
                context_window: ctx,
                max_output_tokens,
                tools: custom_model_metadata_supports_tools(entry),
                parallel_tool_calls: asserted_capability(entry, &["parallel_tool_calls"])
                    .unwrap_or_else(|| supports("parallel_tool_calls")),
                vision,
                structured_output: discovered_structured_output(entry)
                    .unwrap_or_else(|| supports("response_format")),
                reasoning: false,
                reasoning_configurable: true,
                reasoning_values: Vec::new(),
                reasoning_default: String::new(),
                reasoning_profile: None,
                reasoning_source: None,
                // Auto-discovered local models are not guaranteed to implement
                // OpenAI's newer `developer` role. vLLM Qwen chat templates, in
                // particular, reject it while still accepting `system`.
                reasoning_uses_system_message: true,
                pricing: None,
                // Inventory is not authority for request headers or wire overrides.
                preset: Default::default(),
            };
            if apply_discovered_reasoning(entry, &mut model).is_err() {
                if report_errors {
                    diagnostics.problem(format!(
                        "warning: model discovery contains invalid reasoning metadata"
                    ));
                }
                continue;
            }
            models.push(model);
        }
        apply_known_custom_model_defaults(cred, models)
    })();
    diagnostics.finish(report_errors);
    result
}

/// Walk the model metadata looking for a context length. vLLM emits
/// `--max-model-len`, while llama.cpp-style servers expose `--ctx-size` or
/// `meta.n_ctx` through OpenAI-compatible gateways such as hlid.
fn extract_ctx_from_model_entry(entry: &serde_json::Value) -> u64 {
    let args = match entry
        .get("status")
        .and_then(|s| s.get("args"))
        .and_then(|a| a.as_array())
    {
        Some(a) => a,
        None => {
            // vLLM and hosted OpenAI-compatible APIs expose one of these
            // top-level names in their model object.
            return positive_u64(
                entry,
                &[
                    "max_model_len",
                    "context_window",
                    "context_length",
                    "max_context_tokens",
                ],
            )
            .or_else(|| {
                entry
                    .get("meta")
                    .and_then(|meta| positive_u64(meta, &["n_ctx", "n_ctx_train"]))
            })
            .unwrap_or(262_144);
        }
    };

    let mut next_is_ctx = false;
    for arg in args {
        let s = arg.as_str().unwrap_or("");
        if next_is_ctx {
            if let Ok(v) = s.parse::<u64>() {
                return v;
            }
            next_is_ctx = false;
        }
        if matches!(s, "--ctx-size" | "--max-model-len") {
            next_is_ctx = true;
        }
    }
    positive_u64(
        entry,
        &[
            "max_model_len",
            "context_window",
            "context_length",
            "max_context_tokens",
        ],
    )
    .or_else(|| {
        entry
            .get("meta")
            .and_then(|meta| positive_u64(meta, &["n_ctx", "n_ctx_train"]))
    })
    .unwrap_or(262_144) // sensible default for modern local models
}

// Codex's checked-in defaults are only a discovery fallback. The authenticated
// `/models` response is authoritative for plan-specific advertised limits; octet
// then applies the bounded working-window policy implemented by
// `crate::codex_context` (the deliberate 272K cap with its `gpt-5.6-luna` 372K
// exception, the per-family entitlement ceiling, the explicit opt-in override,
// and the clamp notice), so frontends and bootstrap share one definition.
/// Codex retains the provider-advertised maximum as discovery metadata, while
/// octet budgets ordinary Codex families against Pi's 272K working window. GPT-5.6
/// Luna uses its 372K default; smaller advertised windows remain authoritative.
/// Version 8 invalidates inventories filtered by the pre-0.155 Codex client
/// version, so a fresh cache cannot hide GPT-6 Sol/Luna after upgrading.
const CODEX_MODEL_CACHE_VERSION: u8 = 8;
const CODEX_MODEL_CACHE_REFRESH_INTERVAL: Duration = Duration::from_secs(60 * 60);
// This is the Codex `/models` schema compatibility version octet implements,
// not octet's package version. Sending an older version causes the backend to
// filter out models that require a contemporary Codex client.
const CODEX_MODELS_CLIENT_VERSION: &str = "0.156.1";

pub(crate) fn effective_compaction_threshold_fraction(config: &Config, model: &Model) -> f64 {
    let Some(max_active_tokens) = config
        .compaction
        .max_active_tokens
        .filter(|tokens| *tokens > 0)
    else {
        return config.compaction.threshold_fraction;
    };
    let context_window = model.spec.limits.context_window.max(1);
    let absolute_fraction =
        (max_active_tokens.min(context_window) as f64) / (context_window as f64);
    config.compaction.threshold_fraction.min(absolute_fraction)
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct DiscoveredCodexModel {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
    reasoning_options: octet_ai::types::ReasoningOptions,
    context_window: u64,
    /// Backend default window before octet's deliberate working cap. Kept so the
    /// explicit override and its clamp notice can be re-resolved exactly.
    #[serde(default)]
    default_context_window: u64,
    max_context_window: u64,
    max_output_tokens: u64,
    min_effort: octet_ai::ReasoningEffort,
    max_effort: octet_ai::ReasoningEffort,
    /// Positive account-inventory authority, never inferred from a model name.
    #[serde(default)]
    reasoning_effort_updates: bool,
    responses_lite: bool,
    // `Option<T>` normally treats a missing key as `None`; the custom decoder
    // keeps explicit null valid while making incomplete dynamic metadata fail.
    #[serde(deserialize_with = "deserialize_required_agent_delegation")]
    agent_delegation: Option<AgentDelegation>,
}

fn deserialize_required_agent_delegation<'de, D>(
    deserializer: D,
) -> Result<Option<AgentDelegation>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    <Option<AgentDelegation> as serde::Deserialize>::deserialize(deserializer)
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct CodexModelCache {
    version: u8,
    account_id: String,
    plan: Option<String>,
    models: Vec<DiscoveredCodexModel>,
}

#[derive(Clone)]
struct CodexDiscovery {
    claims: crate::auth::codex::SubscriptionClaims,
    models: Vec<DiscoveredCodexModel>,
}

fn positive_u64(entry: &serde_json::Value, names: &[&str]) -> Option<u64> {
    names.iter().find_map(|name| {
        entry
            .get(*name)
            .and_then(serde_json::Value::as_u64)
            .filter(|value| *value > 0)
    })
}

fn codex_fallback_reasoning_options(model_id: &str) -> octet_ai::types::ReasoningOptions {
    // Sparse Codex metadata cannot establish Off or Ultra for generic fallback
    // models. The observed Luna route has an exact supported set of
    // none/low/medium/high/xhigh/max; keep that narrow evidence scoped to Luna.
    let floor = codex_min_effort(model_id);
    let ceiling = codex_max_effort(model_id);
    let candidates = if model_id == "gpt-5.6-luna" {
        ["none", "low", "medium", "high", "xhigh", "max"]
    } else {
        ["minimal", "low", "medium", "high", "xhigh", "max"]
    };
    let values = candidates
        .into_iter()
        .filter(|value| match ReasoningConfig::from_provider_value(value) {
            Some(ReasoningConfig::Off) => true,
            Some(ReasoningConfig::Effort(effort)) => effort >= floor && effort <= ceiling,
            _ => false,
        })
        .map(str::to_owned)
        .collect();
    octet_ai::types::ReasoningOptions {
        values,
        default: None,
    }
}

fn strip_codex_ultra(options: &mut octet_ai::types::ReasoningOptions) {
    options.values.retain(|v| {
        ReasoningConfig::from_provider_value(v)
            != Some(ReasoningConfig::Effort(octet_ai::ReasoningEffort::Ultra))
    });
    if options
        .default
        .as_ref()
        .is_some_and(|d| !options.values.contains(d))
    {
        options.default = options.values.first().cloned();
    }
}

fn codex_reasoning_range(
    options: &octet_ai::types::ReasoningOptions,
    id: &str,
) -> (octet_ai::ReasoningEffort, octet_ai::ReasoningEffort) {
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

fn codex_models_from_response(
    body: &serde_json::Value,
    plan: Option<&crate::auth::codex::ChatGptPlan>,
) -> anyhow::Result<Vec<DiscoveredCodexModel>> {
    // The subscription backend uses `models`, while OpenAI-compatible proxies
    // commonly expose the same inventory under `data`. Accepting both keeps
    // OAuth discovery working through enterprise gateways as well.
    let entries = body
        .get("models")
        .or_else(|| body.get("data"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("Codex models response has no models array"))?;
    let mut models = Vec::with_capacity(entries.len());
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
        let (mut default_context_window, mut max_context_window) =
            match (advertised_context, advertised_max) {
                (Some(context), Some(maximum)) => (context.min(maximum), maximum),
                (Some(context), None) => (context, context),
                (None, Some(maximum)) => (maximum, maximum),
                (None, None) => fallback,
            };
        if matches!(id, "gpt-6-astra" | "gpt-6-sol" | "gpt-6-luna") {
            // Keep the observed GPT-6 input envelope distinct from the
            // conservative Codex working budget, and never overstate the
            // provider's 872K input allowance when only a total window appears.
            max_context_window = max_context_window.min(CODEX_ASTRA_MAX_CONTEXT_WINDOW);
            default_context_window = default_context_window.min(max_context_window);
        }
        // The plan-selected, pre-cap backend window for this model. octet's
        // deliberate working cap is applied by `crate::codex_context`, which also
        // reports the clamp and applies the explicit opt-in override.
        let resolution = resolve_codex_context_window(
            id,
            codex_context_tier(plan),
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
        let metadata = decode_reasoning_metadata(entry)?;
        let mut reasoning_options = if metadata.supported == Some(false) {
            octet_ai::types::ReasoningOptions {
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
        anyhow::ensure!(
            reasoning_options.is_valid(),
            "Codex inventory has no usable reasoning choices"
        );
        let (min_effort, max_effort) = codex_reasoning_range(&reasoning_options, id);
        let responses_lite = entry
            .get("use_responses_lite")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        models.push(DiscoveredCodexModel {
            id: id.to_owned(),
            display_name: discovered_display_name(entry, id),
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
    Ok(models)
}

/// Checked-in discovery fallback windows for a Codex family. The policy itself
/// (working cap and entitlement ceiling) is owned by `crate::codex_context`.
fn codex_model_context_limits(model_id: &str) -> (u64, u64) {
    crate::codex_context::entitled_context_windows(model_id)
}

/// The plan tier that selects between the backend default and advertised maximum.
fn codex_context_tier(plan: Option<&crate::auth::codex::ChatGptPlan>) -> CodexContextTier {
    CodexContextTier::from_plan_entitlement(
        plan.is_some_and(crate::auth::codex::ChatGptPlan::uses_max_context_window),
    )
}

/// Resolve one Codex model's context envelope without a user override.
///
/// Used by discovery and the conservative fallback catalog. The explicit opt-in
/// override is applied once, where discovered metadata becomes catalog limits.
fn codex_context_window_for_plan(
    model_id: &str,
    default_context_window: u64,
    max_context_window: u64,
    plan: Option<&crate::auth::codex::ChatGptPlan>,
) -> u64 {
    resolve_codex_context_window(
        model_id,
        codex_context_tier(plan),
        default_context_window,
        max_context_window,
        None,
        CodexContextOverride::NONE,
    )
    .expect("resolving a Codex context window without a user override cannot fail")
    .context_window
}

fn codex_model_limits(
    model_id: &str,
    plan: Option<&crate::auth::codex::ChatGptPlan>,
) -> (ModelLimits, u64) {
    let (default_context_window, max_context_window) = codex_model_context_limits(model_id);
    (
        ModelLimits {
            context_window: codex_context_window_for_plan(
                model_id,
                default_context_window,
                max_context_window,
                plan,
            ),
            max_output_tokens: CODEX_MAX_OUTPUT_TOKENS,
        },
        max_context_window,
    )
}

fn codex_min_effort(model_id: &str) -> octet_ai::ReasoningEffort {
    if matches!(
        model_id,
        "gpt-6-astra" | "gpt-6-sol" | "gpt-6-luna" | "gpt-5.6-luna"
    ) {
        octet_ai::ReasoningEffort::Low
    } else {
        octet_ai::ReasoningEffort::Minimal
    }
}

// New Codex families accept the top `max` effort tier. Live discovery narrows
// this range when the backend publishes explicit supported reasoning levels.
fn codex_max_effort(model_id: &str) -> octet_ai::ReasoningEffort {
    if matches!(model_id, "gpt-6-astra" | "gpt-6-sol" | "gpt-6-luna")
        || model_id.starts_with("gpt-5.6-")
    {
        octet_ai::ReasoningEffort::Max
    } else {
        octet_ai::ReasoningEffort::High
    }
}

/// Current Codex vision-capable families. The Codex backend's inventory does
/// not reliably include modality metadata, so keep this capability aligned
/// with the provider's published model contract instead of defaulting every
/// OAuth model to text-only.
fn codex_supports_image_input(model_id: &str) -> bool {
    model_id == "codex-mini-latest"
        || matches!(model_id, "gpt-6-astra" | "gpt-6-sol" | "gpt-6-luna")
        || model_id.starts_with("gpt-5.4")
        || model_id.starts_with("gpt-5.5")
        || model_id.starts_with("gpt-5.6")
        || model_id.starts_with("gpt-5.3-codex")
        || model_id.starts_with("gpt-5.2-codex")
        || model_id.starts_with("gpt-5.1-codex")
}

fn codex_plan_cache_key(claims: &crate::auth::codex::SubscriptionClaims) -> Option<&str> {
    claims.plan.as_ref().map(|plan| plan.raw_value())
}

fn save_codex_model_cache(
    store: &crate::auth::codex::CredentialStore,
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

fn load_codex_model_cache(
    store: &crate::auth::codex::CredentialStore,
    claims: &crate::auth::codex::SubscriptionClaims,
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
        if matches!(
            model.id.as_str(),
            "gpt-6-astra" | "gpt-6-sol" | "gpt-6-luna"
        ) {
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
            || (model.max_effort == octet_ai::ReasoningEffort::Ultra
                && model.agent_delegation != Some(AgentDelegation::V2))
        {
            anyhow::bail!("invalid Codex model cache: incomplete or inconsistent model metadata");
        }
    }
    Ok(Some(cache.models))
}

/// Which source produced the Codex inventory for one registration.
///
/// Kept as a typed outcome (not a log line) so the freshness and
/// account-binding rules can be asserted directly: a fresh, account- and
/// plan-matched cache must not trigger a discovery request, and a cache that is
/// stale, invalid, or bound to another account must never be trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CodexInventorySource {
    /// A fresh, account- and plan-matched cache file. Dynamic capability
    /// metadata from this source is validated by the freshness boundary.
    FreshCache,
    /// One synchronous online discovery, which also seeds the cache.
    OnlineDiscovery,
    /// The checked-in conservative fallback catalog. Used when the cache is
    /// missing, stale, invalid, or unusable and no discovery could complete:
    /// carries no dynamic capability and keeps the plan's entitlement ceiling.
    ConservativeFallback,
    /// The unit-test registration path: no ambient HOME and no network. A
    /// fresh, account- and plan-matched cache is still authoritative, but it is
    /// reduced to the conservative contract (see [`conservative_offline_codex_models`]),
    /// so a test can never advertise dynamic capability that was not
    /// revalidated online.
    Fixture,
}

/// Decide the Codex model inventory for one registration.
///
/// The paths deliberately have different freshness behaviour, and this is the
/// only place that decides which one applies:
///
/// * offline never refreshes: a fresh cache is *reduced* to the conservative
///   contract ([`conservative_offline_codex_models`]), because a cache that
///   cannot be revalidated must not advertise dynamic capability;
/// * online uses a fresh, account- and plan-matched cache as-is (its dynamic
///   capability was validated within the freshness window) and otherwise
///   performs exactly one bounded discovery, which also seeds the cache;
/// * a unit-test (fixture) registration never contacts the provider, but a
///   fresh account- and plan-matched cache is still authoritative: skipping it
///   would make the freshness, account-binding and reduction rules untestable
///   through the real registration path;
/// * any failure — invalid cache, discovery error, timeout — falls back to the
///   checked-in catalog, never to a partly-trusted cache.
fn codex_inventory_models(
    store: &crate::auth::codex::CredentialStore,
    initial_claims: &crate::auth::codex::SubscriptionClaims,
    offline: bool,
    fixture: bool,
    discover: impl FnOnce(crate::auth::codex::CredentialStore) -> anyhow::Result<CodexDiscovery>,
) -> (Vec<DiscoveredCodexModel>, CodexInventorySource) {
    let cache = bootstrap_check(
        "codex-cache",
        load_codex_model_cache(store, initial_claims),
        |error| {
            format!("warning: Codex model cache was unusable ({error}); using discovery or conservative fallback")
        },
    );
    if fixture || offline {
        // No provider request on either path. A fresh, account- and
        // plan-matched cache is still used, reduced to the conservative
        // contract because it cannot be revalidated online; only a missing or
        // unusable cache uses the checked-in catalog. The source label keeps
        // the two paths distinguishable.
        let fallback_source = if fixture {
            CodexInventorySource::Fixture
        } else {
            CodexInventorySource::ConservativeFallback
        };
        return match cache {
            Ok(Some(models)) => (
                conservative_offline_codex_models(models),
                CodexInventorySource::FreshCache,
            ),
            Ok(None) => (
                fallback_codex_models(initial_claims.plan.as_ref()),
                fallback_source,
            ),
            Err(error) => {
                let _ = error;
                (
                    fallback_codex_models(initial_claims.plan.as_ref()),
                    fallback_source,
                )
            }
        };
    }
    match cache {
        Ok(Some(models)) => (models, CodexInventorySource::FreshCache),
        _cache_result => {
            match bootstrap_check("codex-discovery", discover(store.clone()), |error| {
                format!("warning: Codex model auto-discovery failed; using conservative fallback catalog: {error}")
            }) {
                Ok(discovery) => {
                    let _ = bootstrap_check(
                        "codex-cache-write",
                        save_codex_model_cache(store, &discovery),
                        |error| format!("warning: could not persist Codex model metadata: {error}"),
                    );
                    (discovery.models, CodexInventorySource::OnlineDiscovery)
                }
                Err(_) => {
                    // Discovery may have refreshed a token before the inventory
                    // request failed, so re-read claims for the fallback limits.
                    let current_claims = crate::auth::codex::usable_subscription_claims(store)
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| initial_claims.clone());
                    (
                        fallback_codex_models(current_claims.plan.as_ref()),
                        CodexInventorySource::ConservativeFallback,
                    )
                }
            }
        }
    }
}

/// The bounded readiness step for a selected Codex route.
///
/// Credential refresh and inventory fetch share one deadline, so the wait the
/// user sees is the advertised one. A timeout is typed
/// ([`RouteReadinessTimeout`]) and returns no partial inventory.
fn discover_codex_models_within_envelope(
    store: crate::auth::codex::CredentialStore,
) -> anyhow::Result<CodexDiscovery> {
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    run_route_readiness(
        "codex route readiness",
        CODEX_READINESS_ENVELOPE,
        cancel,
        move |cancel| {
            discover_codex_models_with(store, cancel, crate::auth::codex::REFRESH_LOCK_WAIT)
        },
    )
}

fn conservative_offline_codex_models(
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

fn fallback_codex_models(
    plan: Option<&crate::auth::codex::ChatGptPlan>,
) -> Vec<DiscoveredCodexModel> {
    crate::auth::codex::MODELS
        .iter()
        .map(|model_id| {
            let (default_context_window, _) = codex_model_context_limits(model_id);
            let (limits, max_context_window) = codex_model_limits(model_id, plan);
            DiscoveredCodexModel {
                id: (*model_id).to_owned(),
                display_name: None,
                reasoning_options: codex_fallback_reasoning_options(model_id),
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

fn codex_models_url() -> anyhow::Result<url::Url> {
    let mut url = url::Url::parse(crate::providers::CODEX.base_url)?.join("models")?;
    url.query_pairs_mut()
        .append_pair("client_version", CODEX_MODELS_CLIENT_VERSION);
    Ok(url)
}

/// Wall-clock envelope for the SELECTED route's Codex inventory work.
///
/// The inventory request is bounded at ten seconds and a token refresh at sixty,
/// so their sum used to be the real wait even though discovery advertised ten.
/// Readiness gives the whole selected-route initialization one deadline instead:
/// a subscription route that cannot become usable inside it is reported as a
/// timeout and the conservative fallback catalog is used, rather than blocking
/// the launch on an unrelated wait.
pub(crate) const CODEX_READINESS_ENVELOPE: Duration = Duration::from_secs(10);

/// Typed, bounded cause for a selected-route wait that did not finish in time.
///
/// Carries the phase name and the deadline, never a path or a credential, and
/// its message is the whole diagnosis a user needs to retry.
#[derive(Debug)]
pub(crate) struct RouteReadinessTimeout {
    pub phase: &'static str,
    pub waited: Duration,
}

impl std::fmt::Display for RouteReadinessTimeout {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} did not finish within {} ms",
            self.phase,
            self.waited.as_millis()
        )
    }
}

impl std::error::Error for RouteReadinessTimeout {}

/// Run one selected-route initialization step under a bounded, cancellable
/// envelope.
///
/// The worker checks `cancel` between its own bounded steps, and every step that
/// can block is already bounded (the inventory request, the token refresh, and
/// the cross-process refresh lock via
/// [`crate::auth::codex::store::REFRESH_LOCK_WAIT`]). A timeout therefore never
/// abandons an indefinitely blocked worker: the worker either observes the
/// cancellation or finishes its own bounded step and exits. A timed-out attempt
/// holds nothing and rotates nothing, so the next launch finds the credential
/// store exactly as the other owner left it.
///
/// The deadline signals cancellation before it returns: without that, a timed-out
/// envelope would leave the worker's own `cancel` flag clear, so the credential
/// step could still start an inventory request the launch no longer wants. The
/// worker is never detached from its bound — it is only ever *asked* to stop
/// between bounded steps, because interrupting a token refresh in flight can
/// strand a rotated refresh token.
pub(crate) fn run_route_readiness<T: Send + 'static>(
    phase: &'static str,
    envelope: Duration,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    work: impl FnOnce(std::sync::Arc<std::sync::atomic::AtomicBool>) -> anyhow::Result<T>
        + Send
        + 'static,
) -> anyhow::Result<T> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker_cancel = std::sync::Arc::clone(&cancel);
    let worker = std::thread::Builder::new()
        .name(format!("octet-route-{phase}"))
        .spawn(move || {
            let _ = sender.send(work(worker_cancel));
        })?;
    match receiver.recv_timeout(envelope) {
        Ok(result) => {
            let _ = worker.join();
            result
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            Err(anyhow::Error::new(RouteReadinessTimeout {
                phase,
                waited: envelope,
            }))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            anyhow::bail!("{phase} worker stopped without reporting a result")
        }
    }
}

fn discover_codex_models(
    store: crate::auth::codex::CredentialStore,
) -> anyhow::Result<CodexDiscovery> {
    discover_codex_models_with(
        store,
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        crate::auth::codex::REFRESH_LOCK_WAIT,
    )
}

/// The bounded online discovery body.
///
/// `refresh_lock_wait` bounds the cross-process refresh-lock acquisition; it is
/// a parameter so tests never depend on a production deadline. `cancel` is
/// observed between the credential step and the inventory request, so a
/// readiness timeout stops the sequence instead of starting a request the
/// launch no longer wants.
fn discover_codex_models_with(
    store: crate::auth::codex::CredentialStore,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    refresh_lock_wait: Duration,
) -> anyhow::Result<CodexDiscovery> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        #[cfg(test)]
        let resolver =
            crate::auth::codex::CodexResolver::with_refresh_lock_wait(store, refresh_lock_wait);
        #[cfg(not(test))]
        let resolver = {
            let _ = refresh_lock_wait;
            crate::auth::codex::CodexResolver::new(store)
        };
        let (mut headers, claims) = resolver.discovery_headers().await?;
        startup_phase("codex.credentials");
        if cancel.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("Codex model discovery cancelled before the inventory request");
        }
        let static_headers =
            crate::providers::public_headers(crate::providers::CODEX.extra_headers)?;
        for (name, value) in &static_headers {
            headers.insert(name.clone(), value.clone());
        }
        headers.insert(
            http::header::USER_AGENT,
            http::HeaderValue::from_str(&codex_user_agent())?,
        );

        let url = codex_models_url()?;
        startup_phase("codex.inventory");
        let response = discovery_client(DISCOVERY_TIMEOUT)?
            .get(url)
            .headers(headers)
            .send()
            .await
            .map_err(|error| anyhow::anyhow!("GET Codex models failed: {error}"))?
            .error_for_status()
            .map_err(|error| anyhow::anyhow!("GET Codex models failed: {error}"))?;
        let body = bounded_discovery_json_async(response, "Codex models").await?;
        let models = codex_models_from_response(&body, claims.plan.as_ref())?;
        Ok(CodexDiscovery { claims, models })
    })
}

fn codex_user_agent() -> String {
    format!(
        "octet/{} ({})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS
    )
}

/// Read the explicit opt-in Codex context-window override.
///
/// Fail-closed: an unreadable, unrecognised, or out-of-range value leaves the
/// deliberate cap in force and reports why.
fn codex_context_override_from_env() -> anyhow::Result<CodexContextOverride> {
    let mut diagnostics = crate::output::DiagnosticCheck::new(
        crate::output::DiagnosticComponent::Bootstrap("codex-context-override".into()),
    );
    let result = (|| {
        let requested = optional_env(CODEX_CONTEXT_OVERRIDE_ENV)?;
        let acknowledged = optional_env(CODEX_CONTEXT_ACKNOWLEDGE_ENV)?;
        match CodexContextOverride::parse(requested.as_deref(), acknowledged.as_deref()) {
            Ok(user_override) => Ok(user_override),
            Err(error) => {
                diagnostics.problem(format!(
                    "warning: {error}; keeping the deliberate Codex context cap"
                ));
                Ok(CodexContextOverride::NONE)
            }
        }
    })();
    diagnostics.finish(result.is_ok());
    result
}

/// Resolve the context envelope for one discovered Codex model.
///
/// The deliberate working cap is applied here as it always has been; a
/// deliberate reduction is reported through the typed [`CodexContextClamp`] the
/// caller observes, and the explicit opt-in override can raise the window (never
/// past the model's entitlement) when the operator acknowledged it. A refused
/// override keeps the deliberate cap.
fn codex_context_resolve_for_registration(
    model: &DiscoveredCodexModel,
    tier: CodexContextTier,
    user_override: CodexContextOverride,
) -> crate::codex_context::CodexContextWindow {
    let mut diagnostics = crate::output::DiagnosticCheck::new(
        crate::output::DiagnosticComponent::Bootstrap(format!("codex-context:{}", model.id)),
    );
    let result = (|| match resolve_codex_context_window(
        &model.id,
        tier,
        model.default_context_window,
        model.max_context_window,
        Some(model.max_output_tokens),
        user_override,
    ) {
        Ok(resolution) => resolution,
        Err(error) => {
            diagnostics.problem(format!(
                "warning: {error}; keeping the deliberate Codex context cap"
            ));
            resolve_codex_context_window(
                &model.id,
                tier,
                model.default_context_window,
                model.max_context_window,
                Some(model.max_output_tokens),
                CodexContextOverride::NONE,
            )
            .expect("resolving a Codex context window without a user override cannot fail")
        }
    })();
    diagnostics.finish(true);
    result
}

/// Record the single user-facing note one catalog model needs, if any.
///
/// Catalog enumeration must never print: a user running a non-Codex model would
/// otherwise read a note about every Codex model in the catalog. The note is
/// shown by [`Bootstrap::codex_context_note`] for the effective session model
/// only. Above the standard tier the whole request is metered differently, which
/// is why such a route carries a note at all.
fn codex_context_record_note(
    notes: &mut CodexContextNotes,
    catalog_id: &ModelId,
    model_id: &str,
    resolution: &crate::codex_context::CodexContextWindow,
) {
    if let Some(note) = crate::codex_context::codex_context_session_note(model_id, resolution) {
        notes.record(catalog_id.clone(), note);
    }
}

/// The durable uncertainty operation for one resolved catalog model, if any.
///
/// `Some` only for a Codex route whose effective window is above the 272K
/// standard tier, where the whole request is priced differently. A public
/// provider with a large context window (1M Gemini, for example) is not a Codex
/// route and keeps exact accounting, so the endpoint is part of the decision.
fn codex_context_uncertainty_operation(model: &Model) -> Option<&'static str> {
    (model.endpoint.id.0 == crate::auth::codex::ENDPOINT_ID
        && model.spec.limits.context_window > crate::codex_context::CODEX_CONTEXT_WINDOW_CAP)
        .then_some(crate::codex_context::CODEX_ABOVE_STANDARD_TIER_OPERATION)
}

/// Mark a route whose effective Codex context window is above the 272K standard
/// tier as uncertain on the session before the first request.
///
/// Above 272K the whole request is priced differently, so the durable session
/// record must say the known totals are only a subtotal instead of letting an
/// exact-looking cost stand. The uncertainty record is sticky and written at
/// most once per session; a route at or below 272K, or any non-Codex route, is
/// left exact. Called at every launch boundary, including a mid-session model
/// switch, so a switch onto a raised Codex route cannot leave an exact-looking
/// cost behind.
fn record_codex_context_uncertainty(session: &mut Session, model: &Model) -> anyhow::Result<()> {
    let Some(operation) = codex_context_uncertainty_operation(model) else {
        return Ok(());
    };
    // This deduplicates durable exposure markers, not a cost-exactness report.
    // Unpriced completed receipts alone must not suppress this route exposure.
    if session.has_uncertain_usage() {
        return Ok(());
    }
    session.record_usage_uncertainty(
        model.endpoint.id.clone(),
        model.spec.id.clone(),
        operation,
    )?;
    Ok(())
}

/// Register the OpenAI Codex (Sign in with ChatGPT) endpoint and discover the
/// account's current model inventory, but only for a validated subscription
/// credential. Codex-specific headers are composed from static endpoint
/// headers, request-scoped session affinity, and resolver account routing.
fn register_openai_codex(
    catalog: &mut ModelCatalog,
    store: crate::auth::codex::CredentialStore,
    offline: bool,
) -> anyhow::Result<()> {
    let mut discarded = CodexContextNotes::default();
    register_openai_codex_with_notes(catalog, store, offline, &mut discarded)
}

/// Register the Codex route while recording the one note each registered model
/// needs. See [`codex_context_record_note`].
fn register_openai_codex_with_notes(
    catalog: &mut ModelCatalog,
    store: crate::auth::codex::CredentialStore,
    offline: bool,
    notes: &mut CodexContextNotes,
) -> anyhow::Result<()> {
    use crate::auth::codex;

    let declaration = &crate::providers::CODEX;
    declaration.validate().map_err(|error| {
        anyhow::anyhow!("invalid {} provider declaration: {error}", declaration.id)
    })?;
    let route = declaration.inventory_route().ok_or_else(|| {
        anyhow::anyhow!(
            "{} provider declaration has no subscription route",
            declaration.id
        )
    })?;

    let Some(initial_claims) = codex::usable_subscription_claims(&store)? else {
        return Ok(());
    };

    // Tests use synthetic JWTs and must not contact the production catalog.
    // At runtime only a fresh, account-and-plan-matched cache is authoritative
    // for a launch. Stale or future-dated metadata is synchronously refreshed
    // online and reduced to the conservative fallback offline, so dynamic
    // capabilities can never survive past the freshness boundary. A first
    // launch performs one bounded discovery to seed the cache.
    let (models, _source) = codex_inventory_models(
        &store,
        &initial_claims,
        offline,
        cfg!(test),
        discover_codex_models_within_envelope,
    );
    let resolver = std::sync::Arc::new(codex::CodexResolver::new(store));

    let mut default_headers = crate::providers::public_headers(declaration.extra_headers)?;
    default_headers.insert(
        http::header::USER_AGENT,
        http::HeaderValue::from_str(&codex_user_agent())?,
    );

    catalog.register_endpoint(Endpoint {
        id: EndpointId(route.endpoint_id.into()),
        base_url: url::Url::parse(declaration.base_url)?,
        auth: Auth::dynamic(resolver),
        default_headers,
        // The declaration prefers the cached Responses WebSocket. AiClient
        // retains HTTP/SSE as a conservative fallback for unavailable or
        // provider-rejected sockets.
        transport: route.transport,
        runtime: route.runtime,
        timeout: PROVIDER_RESPONSE_HEADER_TIMEOUT,
    })?;

    let user_override = codex_context_override_from_env()?;
    let tier = codex_context_tier(initial_claims.plan.as_ref());

    for model in models {
        // GPT-6 routes are always namespaced so an OAuth selection cannot be
        // confused with the public OpenAI route when credentials change. Other
        // Codex ids retain their historical collision-based compatibility.
        let catalog_id =
            if matches!(model.id.as_str(), "gpt-6-astra" | "gpt-6-sol" | "gpt-6-luna")
                || catalog.resolve(&ModelId(model.id.clone())).is_ok()
            {
                ModelId(format!("{}/{}", declaration.id, model.id))
            } else {
                ModelId(model.id.clone())
            };
        let limits = {
            let resolution = codex_context_resolve_for_registration(&model, tier, user_override);
            // Recording is not printing: the note is shown once, for the
            // effective session model only (see `codex_context_note`).
            codex_context_record_note(notes, &catalog_id, &model.id, &resolution);
            ModelLimits {
                context_window: resolution.context_window,
                max_output_tokens: resolution.max_output_tokens,
            }
        };
        let pricing = crate::providers::pricing_for(declaration, &model.id);
        let supports_image_input = codex_supports_image_input(&model.id);
        // The declaration keeps application session identity separate from the
        // resolver's credential/account routing.
        let cache = crate::providers::cache_compatibility(
            declaration.compatibility,
            &model.id,
            route.protocol,
        );
        catalog.register_model(ModelSpec {
            preset: Default::default(),
            id: catalog_id,
            endpoint: EndpointId(route.endpoint_id.into()),
            api_name: model.id,
            display_name: model.display_name,
            protocol: route.protocol,
            capabilities: Capabilities {
                input_modalities: if supports_image_input {
                    ModalitySet::none().with(octet_ai::Modality::Image)
                } else {
                    ModalitySet::none()
                },
                output_modalities: ModalitySet::none(),
                tools: true,
                parallel_tool_calls: true,
                reasoning: Some(ReasoningCapability {
                    options: Some(model.reasoning_options),
                    control: ReasoningControl::Effort,
                    exposes_text: true,
                    preserves_state: true,
                    effort_budgets: None,
                    openai_chat_mode: OpenAiChatReasoningMode::Standard,
                    min_effort: model.min_effort,
                    max_effort: model.max_effort,
                }),
                responses_lite: model.responses_lite,
                agent_delegation: model.agent_delegation,
                structured_output: false,

                deferred_tool_loading: false,

                responses_features: octet_ai::ResponsesFeatures {
                    reasoning_effort_updates: model.reasoning_effort_updates,
                    ..Default::default()
                },
            },
            limits,
            pricing,
            cache,
        })?;
    }
    Ok(())
}

pub(crate) fn register_offline_openrouter_model(
    catalog: &mut ModelCatalog,
    raw_model: &str,
) -> anyhow::Result<bool> {
    let raw_model = raw_model.trim();
    let api_name = raw_model.strip_prefix("openrouter/").unwrap_or(raw_model);
    let mut components = api_name.split('/');
    let Some(provider) = components.next() else {
        return Ok(false);
    };
    let Some(model_name) = components.next() else {
        return Ok(false);
    };
    if provider.is_empty()
        || model_name.is_empty()
        || components.next().is_some()
        || !api_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:/".contains(&byte))
    {
        return Ok(false);
    }

    let declaration = openrouter_declaration();
    let Some(route) = declaration.route_for_model(api_name) else {
        return Ok(false);
    };
    let endpoint_id = EndpointId(route.endpoint_id.into());
    if !catalog.has_endpoint(&endpoint_id) {
        return Ok(false);
    }
    let catalog_id = ModelId(format!("{}/{api_name}", declaration.id));
    if catalog.resolve(&catalog_id).is_ok() {
        return Ok(true);
    }

    // Batch submission only needs the provider slug and protocol. Keep this
    // conservative synthetic spec out of the interactive catalog; it is used
    // when an explicit offline batch model cannot be found in the inventory
    // cache, where the provider remains the authority on actual availability.
    catalog.register_model(ModelSpec {
        preset: Default::default(),
        id: catalog_id,
        endpoint: endpoint_id,
        api_name: api_name.to_owned(),
        display_name: None,
        protocol: route.protocol,
        capabilities: Capabilities {
            input_modalities: ModalitySet::none(),
            output_modalities: ModalitySet::none(),
            tools: false,
            parallel_tool_calls: false,
            reasoning: None,
            responses_lite: false,
            agent_delegation: None,
            structured_output: false,
            deferred_tool_loading: false,
            responses_features: Default::default(),
        },
        limits: ModelLimits {
            context_window: 131_072,
            max_output_tokens: 16_384,
        },
        pricing: None,
        cache: octet_ai::CacheCompatibility::default(),
    })?;
    Ok(true)
}

/// The provider inventories a launch must initialize before it is usable.
///
/// Readiness is deliberately narrower than discovery. A launch that already
/// names its route must not wait for unrelated configured providers: an AWS
/// credential-chain probe, a subscription inventory fetch, or a local-server
/// probe belongs to a route this launch cannot use. Anything the plan does not
/// name is still available — the embedded static catalog is built in full and
/// every skipped inventory can be completed later by
/// [`Bootstrap::enrich_catalog`] — and the plan falls back to
/// [`CatalogReadiness::Fleet`] whenever it cannot name the route, so narrowing
/// is an optimization and never a selector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CatalogReadiness {
    /// Nothing could be named (a model-less launch, first-run setup, a resumed
    /// session whose provenance only the session file knows) or the selection
    /// may belong to more than one route (an un-namespaced id, a custom
    /// OpenAI-compatible registry, an extension-provided model). The historical
    /// full inventory initialization, unchanged.
    Fleet,
    /// Exactly these builtin provider declarations participate in readiness,
    /// in order. Used only when the selection proves the route, so the user's
    /// chosen model is always initialized before it is resolved.
    Routes(Vec<&'static str>),
}

impl CatalogReadiness {
    /// Whether the plan includes one builtin provider declaration.
    pub(crate) fn includes(&self, provider_id: &str) -> bool {
        match self {
            Self::Fleet => true,
            Self::Routes(routes) => routes.iter().any(|route| *route == provider_id),
        }
    }

    /// Whether this plan is the full historical initialization.
    pub(crate) fn is_fleet(&self) -> bool {
        matches!(self, Self::Fleet)
    }

    /// Route ids named by the plan, for diagnostics and tests.
    #[cfg(test)]
    pub(crate) fn route_ids(&self) -> Vec<&'static str> {
        match self {
            Self::Fleet => Vec::new(),
            Self::Routes(routes) => routes.clone(),
        }
    }
}

/// Declarations the readiness plan may name as a proven route.
///
/// The generated [`BUILTIN_PROVIDER_DECLARATIONS`] carries the environment/ADC
/// built-ins only; the product-owned subscription routes whose credentials are
/// owned by another module (Codex today) are appended here. Every entry must
/// have a matching catalog-registration branch in
/// [`model_catalog_for_readiness`], otherwise naming it would prove a route that
/// nothing initializes. Copilot is deliberately absent: host-owned integrations
/// are not nameable by a builtin id, so a Copilot selection stays on the fleet
/// plan.
fn readiness_declarations() -> impl Iterator<Item = &'static ProviderDeclaration> {
    BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .chain([&crate::providers::CODEX])
}

/// Resolve the builtin declaration that provably owns one selected model id.
///
/// Only an explicit `<declaration-id>/<model>` namespace is trusted. Every
/// other id — un-namespaced, a custom registry, an extension provider, an
/// unknown namespace — returns `None`, which makes the caller take
/// [`CatalogReadiness::Fleet`]: a wrong narrowing would silently drop the route
/// the model actually needs, so ambiguity always falls back to the full path.
fn builtin_declaration_for_model(model: &ModelId) -> Option<&'static str> {
    let (prefix, _) = model.0.split_once('/')?;
    readiness_declarations()
        .find(|declaration| declaration.id == prefix)
        .map(|declaration| declaration.id)
}

/// Which provider inventories readiness must initialize for this launch.
///
/// Order of guarantees:
/// 1. an explicit CLI selection always wins and names its own route, even when
///    a session is resumed (the session's provenance is overridden);
/// 2. a resumed/continued/forked session without an explicit selection may
///    carry provenance only the session file knows, so readiness stays full;
/// 3. a configuration-level selection on a NEW session is the effective
///    selection (nothing can override it), so it may narrow;
/// 4. no selection at all needs the fleet: the picker, model-less mode and
///    first-run setup all enumerate every provider;
/// 5. a configured compaction route is retained even when the session model is
///    on another provider, so compaction can never point at a missing route.
pub(crate) fn catalog_readiness(config: &Config) -> CatalogReadiness {
    let provenance_possible = !matches!(config.resume, ResumeSelector::New);
    if provenance_possible && !config.model_explicit {
        // A session may have stored a model that config cannot see yet.
        return CatalogReadiness::Fleet;
    }
    let Some(selection) = config.model.as_ref() else {
        // Model-less launch: setup and the picker enumerate every provider.
        return CatalogReadiness::Fleet;
    };
    let Some(provider_id) = builtin_declaration_for_model(selection) else {
        // Un-namespaced, custom, or extension-provided: ambiguous on purpose.
        return CatalogReadiness::Fleet;
    };
    let mut routes = vec![provider_id];
    // Native compaction can be pinned to a different provider than the session
    // model; that route has to exist before the first turn, not after. An
    // un-namespaced or extension-provided compaction id cannot be proven to be
    // a builtin route, and `build_app` fails closed when it does not resolve, so
    // ambiguity here is the fleet rather than a narrowed plan that drops it.
    if let Some(compaction_model) = config.compaction.compact_model.as_ref() {
        let Some(compaction_provider) = builtin_declaration_for_model(compaction_model) else {
            return CatalogReadiness::Fleet;
        };
        if !routes.contains(&compaction_provider) {
            routes.push(compaction_provider);
        }
    }
    CatalogReadiness::Routes(routes)
}

fn base_model_catalog_with_custom_store(
    offline: bool,
    explicit_custom_store: Option<&crate::auth::custom::CredentialStore>,
) -> anyhow::Result<ModelCatalog> {
    base_model_catalog_with_readiness(offline, explicit_custom_store, &CatalogReadiness::Fleet)
}

/// Bind only the embedded catalog's existing endpoints, before credential
/// pruning. This is local credential resolution, not discovery or registration
/// of additional static inventories. Only readiness-selected declarations may
/// access their private credential source; protocol/model metadata stays intact.
fn bind_embedded_provider_credentials(
    embedded: ModelCatalog,
    readiness: &CatalogReadiness,
    mut resolve: impl FnMut(
        &ProviderDeclaration,
    ) -> anyhow::Result<Option<crate::providers::EnvironmentCredential>>,
) -> anyhow::Result<ModelCatalog> {
    let mut catalog = ModelCatalog::default();
    let mut unavailable = std::collections::HashSet::new();
    for spec in embedded.models() {
        if unavailable.contains(&spec.endpoint) {
            continue;
        }
        let model = embedded.resolve(&spec.id)?;
        if !catalog.has_endpoint(&model.endpoint.id) {
            let mut endpoint = (*model.endpoint).clone();
            if let Some((declaration, route)) = BUILTIN_PROVIDER_DECLARATIONS
                .iter()
                .filter(|declaration| readiness.includes(declaration.id))
                .find_map(|declaration| {
                    declaration
                        .routes
                        .iter()
                        .find(|route| route.endpoint_id == endpoint.id.0)
                        .map(|route| (declaration, route))
                })
            {
                let authentication = resolve(declaration).and_then(|credential| {
                    credential
                        .map(|credential| crate::providers::environment_auth(route, &credential))
                        .transpose()
                });
                match bootstrap_check(
                    format!("provider-credential:{}", declaration.id),
                    authentication,
                    |_| {
                        format!(
                            "warning: {} unavailable: could not resolve provider credentials",
                            declaration.name
                        )
                    },
                ) {
                    Ok(Some(authentication)) => endpoint.auth = authentication,
                    Ok(None) => {}
                    Err(_) => {
                        // An invalid source cannot fall back to another identity.
                        // Omit only this account's embedded models; other accounts
                        // and later custom-provider registration remain usable.
                        unavailable.insert(endpoint.id.clone());
                        continue;
                    }
                }
            }
            catalog.register_endpoint(endpoint)?;
            if let Some(label) = embedded.endpoint_label(&model.endpoint.id) {
                catalog.set_endpoint_label(model.endpoint.id.clone(), label)?;
            }
        }
        catalog.register_model(spec.clone())?;
    }
    Ok(catalog)
}

fn base_model_catalog_with_readiness(
    offline: bool,
    explicit_custom_store: Option<&crate::auth::custom::CredentialStore>,
    readiness: &CatalogReadiness,
) -> anyhow::Result<ModelCatalog> {
    let mut catalog = ModelCatalog::builtin()?;
    // The embedded catalog describes supported integrations, not enabled
    // accounts. Do not offer a cloud model until its endpoint can resolve a
    // credential from the environment or owner-private key store. Bind existing
    // native endpoints before pruning so offline startup and discovery failure
    // retain exactly the same embedded fallback models for either key source.
    // Unit tests use injected local stores rather than ambient credentials.
    #[cfg(not(test))]
    {
        catalog = bind_embedded_provider_credentials(
            catalog,
            readiness,
            crate::providers::resolve_environment,
        )?;
        catalog.retain_configured_models();
    }
    startup_phase("catalog.base");
    if cfg!(test) && readiness.is_fleet() {
        // Tests keep the historical deterministic DeepSeek fixture and never
        // use ambient credentials or contact provider discovery endpoints. A
        // narrowed plan is exercised through the runtime path so tests assert
        // the readiness decision itself, not a fixture shortcut.
        let declaration = &crate::providers::DEEPSEEK;
        let credential = crate::providers::EnvironmentCredential::for_test(
            "DEEPSEEK_API_KEY",
            "test-deepseek-key",
        );
        let base_url = deepseek_base_url(declaration)?;
        crate::providers::register_environment_endpoints_at_base_url(
            &mut catalog,
            declaration,
            &credential,
            &base_url,
            PROVIDER_RESPONSE_HEADER_TIMEOUT,
        )?;
        register_deepseek_v4_pro(&mut catalog, declaration)?;
    } else if offline {
        let _ = bootstrap_check(
            "openrouter-offline",
            register_cached_openrouter_models_offline(&mut catalog),
            |error| format!("warning: OpenRouter model cache unavailable: {error}"),
        );
    } else {
        match readiness {
            // The historical full initialization: every detected provider.
            CatalogReadiness::Fleet => register_configured_presets_parallel(&mut catalog),
            // Only the route the selection proves it needs. The other
            // declarations are deliberately not touched: their credential
            // chains, inventory probes and local-server scans cannot serve this
            // launch, and waiting for one of them is what makes startup slow.
            CatalogReadiness::Routes(routes) => {
                register_selected_preset_inventories(&mut catalog, routes)
            }
        }
    }

    // Explicit custom models remain usable offline; only auto-discovery is skipped.
    // Normal tests never inspect ambient HOME credentials, while provider setup
    // passes its explicit existing store so it can rebuild the same catalog
    // immediately without introducing a second registry or catalog type.
    //
    // A narrowed plan skips both registries: a custom OpenAI-compatible model is
    // never namespaced by a builtin declaration (it would have failed the route
    // proof in [`catalog_readiness`]), and the caller falls back to the fleet
    // whenever the selected model still does not resolve.
    if !readiness.is_fleet() {
        if !cfg!(test) {
            catalog.retain_configured_models();
        }
        return Ok(catalog);
    }
    if let Some(store) = explicit_custom_store {
        let _ = bootstrap_check(
            "custom-registry",
            register_custom_openai_endpoints_from_store(&mut catalog, store, offline),
            |error| format!("warning: custom provider registry unavailable: {error}"),
        );
        if !cfg!(test) {
            let _ = bootstrap_check(
                "apple-foundation",
                register_default_apple_foundation_models(&mut catalog, store, offline),
                |error| format!("warning: Apple Foundation Models unavailable: {error}"),
            );
            catalog.retain_configured_models();
        }
    } else if !cfg!(test) {
        let store = crate::auth::custom::CredentialStore::new(crate::auth::custom::default_path());
        let _ = bootstrap_check(
            "custom-registry",
            register_custom_openai_endpoints_from_store(&mut catalog, &store, offline),
            |error| format!("warning: custom provider registry unavailable: {error}"),
        );
        let _ = bootstrap_check(
            "apple-foundation",
            register_default_apple_foundation_models(&mut catalog, &store, offline),
            |error| format!("warning: Apple Foundation Models unavailable: {error}"),
        );
        // Custom providers may use provider-scoped environment credentials;
        // hide models whose referenced variable is not configured, just as the
        // built-in provider catalog does above.
        catalog.retain_configured_models();
    }
    Ok(catalog)
}

fn base_model_catalog(offline: bool) -> anyhow::Result<ModelCatalog> {
    base_model_catalog_with_custom_store(offline, None)
}

/// Environment switch for the off-screen startup phase trace.
///
/// This is the one timing channel every frontend shares for the "startup shows
/// nothing, but latency must stay attributable" contract: set to a non-empty
/// value other than `0` and `startup_phase` writes one
/// `octet-startup: <phase> elapsed=<micros>us` line per boundary to stderr.
/// Frontends that own a phase boundary (`history.hydrate`, `frame.ready`) call
/// [`startup_phase`] with their stable name; they never render the trace.
pub(crate) const STARTUP_TRACE_ENV: &str = "OCTET_STARTUP_TRACE";
/// Monotonic origin for the phase trace, shared by every phase in this process.
static STARTUP_STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Whether the startup phase trace is enabled for one env value.
///
/// Off unless the switch is explicitly set to something other than empty/`0`, so
/// the default startup prints nothing.
fn startup_trace_enabled(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|value| !value.is_empty() && value != "0")
}

/// One stable, single-line phase record. No timestamps are asserted anywhere:
/// the harness compares *which* phases ran and how often.
fn startup_phase_line(phase: &str, elapsed: std::time::Duration) -> String {
    format!("octet-startup: {phase} elapsed={}us", elapsed.as_micros())
}

/// Report one startup phase boundary out of band.
///
/// Startup renders nothing, but the latency acceptance criteria still need the
/// phases to be distinguishable, so `OCTET_STARTUP_TRACE=1` writes one line per
/// boundary to stderr (`octet-startup: <phase> elapsed=<micros>us`) and the
/// default (unset) prints nothing and changes no behavior.
///
/// This trace is the off-screen timing signal every frontend shares; nothing in
/// it is ever rendered on the startup screen. Stable phase names:
/// `catalog.base`, `catalog.selected`, `catalog.codex`, `catalog.copilot`,
/// `catalog.fallback`, `catalog.enrich`, `codex.credentials`, `codex.inventory`,
/// `bootstrap.ready`, `session.resolve`, `session.replay`,
/// `extensions.provider-preflight`, `extensions.prestart`, `extensions.activate`,
/// `app.build`, `history.hydrate`, `frame.ready`. The last two are emitted by the
/// interactive frontend; `codex.credentials` and `codex.inventory` separate
/// credential refresh from the inventory request inside one selected-route wait,
/// and `catalog.selected` bounds the selected-route inventory wait itself (the
/// delta after `catalog.base`) so a configured but unselected provider's
/// discovery is never attributed to readiness.
pub(crate) fn startup_phase(phase: &str) {
    if !startup_trace_enabled(std::env::var_os(STARTUP_TRACE_ENV).as_deref()) {
        return;
    }
    let started = STARTUP_STARTED.get_or_init(std::time::Instant::now);
    crate::output::stderr_line(startup_phase_line(phase, started.elapsed()));
}

/// Build the runtime model catalog, exposing subscription models only through
/// their authenticated product-owned registration boundary.
pub fn model_catalog() -> anyhow::Result<ModelCatalog> {
    model_catalog_with_offline(false)
}

pub fn model_catalog_with_offline(offline: bool) -> anyhow::Result<ModelCatalog> {
    Ok(model_catalog_with_offline_and_codex_notes(offline)?.0)
}

/// Build the runtime catalog and the Codex context notes recorded for it.
///
/// Recording happens here because the deliberate cap, the entitlement ceiling
/// and the explicit override are all resolved while Codex models are registered.
/// Nothing is printed: frontends ask [`Bootstrap::codex_context_note`] for the
/// effective session model's single note instead.
pub fn model_catalog_with_offline_and_codex_notes(
    offline: bool,
) -> anyhow::Result<(ModelCatalog, CodexContextNotes)> {
    model_catalog_for_readiness(offline, &CatalogReadiness::Fleet)
}

/// Build the catalog for one readiness plan.
///
/// The plan decides *which provider inventories are initialized now*:
/// [`CatalogReadiness::Fleet`] keeps the historical full initialization, while a
/// narrowed plan touches only the route the selection proved. The embedded
/// static catalog is built in full either way, so a narrowed plan can never
/// remove a model that was listed before; it only defers subscription/host
/// inventories until [`Bootstrap::enrich_catalog`].
pub(crate) fn model_catalog_for_readiness(
    offline: bool,
    readiness: &CatalogReadiness,
) -> anyhow::Result<(ModelCatalog, CodexContextNotes)> {
    let mut catalog = base_model_catalog_with_readiness(offline, None, readiness)?;
    let mut notes = CodexContextNotes::default();
    if readiness.includes(crate::providers::CODEX.id) {
        register_codex_catalog(&mut catalog, offline, &mut notes);
    }
    startup_phase("catalog.codex");
    // Copilot is a host-owned subscription route: no builtin declaration id ever
    // names it, so a narrowed plan can never include it and it stays on the
    // historical fleet path (see `catalog_readiness`'s proof rule).
    if readiness.is_fleet() {
        register_copilot_catalog(&mut catalog, offline);
    }
    startup_phase("catalog.copilot");
    Ok((catalog, notes))
}

/// Rebuild the canonical catalog against one explicit existing custom store.
/// Provider setup uses this after its final atomic write so a selected model is
/// immediately available without restarting or constructing a partial Agent.
pub(crate) fn model_catalog_with_setup_store(
    custom_store: &crate::auth::custom::CredentialStore,
    offline: bool,
) -> anyhow::Result<ModelCatalog> {
    let mut catalog = base_model_catalog_with_custom_store(offline, Some(custom_store))?;
    let mut notes = CodexContextNotes::default();
    register_codex_catalog(&mut catalog, offline, &mut notes);
    register_copilot_catalog(&mut catalog, offline);
    Ok(catalog)
}

fn register_codex_catalog(
    catalog: &mut ModelCatalog,
    offline: bool,
    notes: &mut CodexContextNotes,
) {
    // Unit tests use explicit temporary credential stores and must never inspect
    // the developer's ambient HOME. Runtime offline mode still registers a
    // locally authenticated Codex endpoint, but never discovers or refreshes
    // its inventory over the network.
    if !cfg!(test) {
        let store = crate::auth::codex::CredentialStore::new(crate::auth::codex::default_path());
        // Non-fatal: a stale or malformed OAuth file must never block octet startup.
        let _ = bootstrap_check(
            "codex-registration",
            register_openai_codex_with_notes(catalog, store, offline, notes),
            |error| format!("warning: OpenAI Codex models unavailable: {error}"),
        );
    }
}

fn register_copilot_catalog(catalog: &mut ModelCatalog, offline: bool) {
    // No ambient credential access in unit tests. Unlike Codex, this adapter has
    // no offline inventory: offline must return before even resolving its store.
    if cfg!(test) || offline {
        return;
    }
    let _ = bootstrap_check(
        "copilot-registration",
        crate::auth::copilot::register_available_models_blocking(catalog, offline),
        |error| format!("warning: GitHub Copilot models unavailable: {error}"),
    );
}

/// Build the catalog without ChatGPT models, used to make `/logout` atomic when
/// its active model belongs to ChatGPT. Other authenticated providers are kept.
pub fn model_catalog_without_codex() -> anyhow::Result<ModelCatalog> {
    let mut catalog = base_model_catalog(false)?;
    register_copilot_catalog(&mut catalog, false);
    Ok(catalog)
}

/// Build bootstrap state from resolved configuration.
///
/// Startup order is: (1) cheap local provider availability — the embedded
/// catalog filtered by which endpoints can already resolve a credential, no
/// network; (2) the session/model selection this configuration already proves
/// ([`catalog_readiness`]); (3) initialization of the SELECTED route only; and
/// (4) optional enrichment of every other provider, later and never awaited by
/// readiness ([`Bootstrap::enrich_catalog`]). A launch that cannot name its
/// route takes the fleet plan, which is the historical behavior.
pub fn bootstrap(config: Config) -> anyhow::Result<Bootstrap> {
    let mut readiness = catalog_readiness(&config);
    let (mut catalog, mut codex_context_notes) =
        model_catalog_for_readiness(config.offline, &readiness)?;
    // The plan is a proof about configuration, not a promise: the selection can
    // still belong to a route the plan could not name (an extension provider, a
    // custom registry, an inventory the provider no longer offers). Complete the
    // catalog instead of failing the launch or switching the user's model.
    if !readiness.is_fleet()
        && config
            .model
            .as_ref()
            .is_some_and(|model| catalog.resolve(model).is_err())
    {
        startup_phase("catalog.fallback");
        let (fleet_catalog, fleet_notes) =
            model_catalog_for_readiness(config.offline, &CatalogReadiness::Fleet)?;
        catalog = fleet_catalog;
        codex_context_notes = fleet_notes;
        // The catalog is already the fleet one, so record that: a later
        // `enrich_catalog` must not rebuild it a second time.
        readiness = CatalogReadiness::Fleet;
    }
    let sessions = SessionStore::new(&config.session_dir, &config.workspace);
    // Record the workspace path so cross-workspace browsing can name each
    // session's home. Non-fatal: pickers fall back to directory names.
    let _ = bootstrap_check(
        "session-workspace-marker",
        sessions.write_workspace_marker(),
        |error| format!("warning: could not write session workspace marker: {error}"),
    );
    let client = AiClient::try_new()?;
    startup_phase("bootstrap.ready");
    Ok(Bootstrap {
        config,
        catalog,
        sessions,
        client,
        provider_runtime: ExtensionProviderRuntime::default(),
        prestarted_extensions: RefCell::new(None),
        prepared_session: RefCell::new(None),
        modeless: std::cell::Cell::new(false),
        codex_context_notes,
        readiness,
    })
}

impl Bootstrap {
    /// Whether this launch initialized the full provider fleet.
    #[cfg(test)]
    pub(crate) fn readiness_plan(&self) -> &CatalogReadiness {
        &self.readiness
    }

    /// Complete the catalog with every provider the readiness plan deferred.
    ///
    /// This is enrichment, not readiness: nothing the launch needs for its first
    /// turn waits on it. A frontend calls it before opening a surface that
    /// enumerates every route (the model picker, `/model`, a status page) — the
    /// provider inventories skipped at startup are exactly the ones such a
    /// surface must list, and each keeps its own freshness rules.
    ///
    /// Idempotent: a launch that already took the fleet plan (or one that has
    /// already been enriched) returns immediately, so a second call can never
    /// duplicate a provider registration or re-run credential refresh.
    pub fn enrich_catalog(&mut self) -> anyhow::Result<()> {
        if self.readiness.is_fleet() {
            return Ok(());
        }
        startup_phase("catalog.enrich");
        let (catalog, notes) =
            model_catalog_for_readiness(self.config.offline, &CatalogReadiness::Fleet)?;
        self.catalog = catalog;
        // Notes were recorded for the deferred Codex route; merge rather than
        // replace so a note already delivered for this session stays delivered.
        self.codex_context_notes.merge(notes);
        self.readiness = CatalogReadiness::Fleet;
        Ok(())
    }
}

/// Resolve model configuration precedence. The caller supplies values from
/// distinct configuration layers; explicit CLI selection always wins.
pub fn resolve_model_id(
    cli: Option<ModelId>,
    project: Option<ModelId>,
    global: Option<ModelId>,
) -> Option<ModelId> {
    cli.or(project).or(global)
}

#[derive(Default)]
struct PersistedSessionConfig {
    model: Option<ModelId>,
    reasoning: Option<ReasoningConfig>,
    reasoning_mode: Option<ReasoningMode>,
}

fn persisted_session_config(session: &Session) -> anyhow::Result<PersistedSessionConfig> {
    let path = session.path();
    let mut persisted = PersistedSessionConfig::default();
    let mut cursor = session.head_ref();
    while let Some(id) = cursor {
        let entry = session
            .entry(id)
            .ok_or_else(|| anyhow::anyhow!("session head references missing entry {}", id.0))?;
        if let EntryValue::ResponsesReasoning {
            model,
            baseline,
            update,
            ..
        } = &entry.value
        {
            // A host-issued update is the effective selection, not the pinned
            // request baseline. Newer route selections must never inherit an
            // older route's effort while walking the active ancestry backwards.
            if persisted
                .model
                .as_ref()
                .is_none_or(|selected| selected == model)
            {
                if persisted.model.is_none() {
                    persisted.model = Some(model.clone());
                }
                if persisted.reasoning.is_none() {
                    persisted.reasoning = Some(update.as_ref()
                        .map(|update| update.reasoning.clone())
                        .unwrap_or_else(|| baseline.clone()));
                }
            }
        }
        if let EntryValue::Config {
            model,
            reasoning,
            reasoning_mode,
        } = &entry.value
        {
            if persisted.model.is_none() {
                persisted.model = model.clone().map(ModelId);
            }
            if persisted.reasoning.is_none() {
                persisted.reasoning = reasoning
                    .as_deref()
                    .map(crate::config::parse_reasoning)
                    .transpose()
                    .map_err(|error| {
                        anyhow::anyhow!(
                            "invalid reasoning state in session {} at entry {}: {error}",
                            path.display(),
                            id.0
                        )
                    })?;
            }
            if persisted.reasoning_mode.is_none() {
                persisted.reasoning_mode = reasoning_mode
                    .as_deref()
                    .map(crate::config::parse_reasoning_mode)
                    .transpose()
                    .map_err(|error| {
                        anyhow::anyhow!(
                            "invalid reasoning mode in session {} at entry {}: {error}",
                            path.display(),
                            id.0
                        )
                    })?;
            }
            if persisted.model.is_some()
                && persisted.reasoning.is_some()
                && persisted.reasoning_mode.is_some()
            {
                break;
            }
        }
        cursor = entry.parent.as_ref();
    }
    Ok(persisted)
}

fn append_config_if_changed(
    session: &mut Session,
    model: &ModelId,
    reasoning: &ReasoningConfig,
    reasoning_mode: ReasoningMode,
) -> anyhow::Result<()> {
    let persisted = persisted_session_config(session)?;
    if persisted.model.as_ref() == Some(model)
        && persisted.reasoning.as_ref() == Some(reasoning)
        && persisted.reasoning_mode == Some(reasoning_mode)
    {
        return Ok(());
    }
    session.append(EntryValue::Config {
        model: Some(model.0.clone()),
        reasoning: Some(crate::app::reasoning_label(reasoning)),
        reasoning_mode: Some(
            match reasoning_mode {
                ReasoningMode::Standard => "standard",
                ReasoningMode::Pro => "pro",
            }
            .to_owned(),
        ),
    })?;
    Ok(())
}

/// Invocation/session preferences, before model-specific defaults are resolved.
struct LaunchConfiguration {
    model: Option<ModelId>,
    reasoning: Option<ReasoningConfig>,
    reasoning_mode: ReasoningMode,
}

fn launch_configuration_parts(
    config: &Config,
    session: &SessionSelection,
) -> anyhow::Result<(Option<Session>, LaunchConfiguration)> {
    startup_phase("session.resolve");
    let prepared = match session {
        SessionSelection::OpenExisting(path) => {
            let descriptor_path = descriptor_session_path(path)?;
            let file = open_regular_file_for_append(&descriptor_path)?;
            Some(Session::open_with_file(path, file)?)
        }
        SessionSelection::CreateNew(_) => None,
    };
    let persisted = prepared
        .as_ref()
        .map(persisted_session_config)
        .transpose()?
        .unwrap_or_default();
    let model = if config.model_explicit {
        config.model.clone()
    } else {
        persisted.model.or_else(|| config.model.clone())
    };
    let reasoning = if config.reasoning_explicit {
        config.reasoning.clone()
    } else {
        persisted.reasoning.or_else(|| config.reasoning.clone())
    };
    let reasoning_mode = if config.reasoning_mode_explicit {
        config.reasoning_mode
    } else if config.reasoning_explicit {
        // A current explicit effort selection supersedes the obsolete persisted
        // Pro bit; otherwise migration would silently replace the user's
        // requested effort with Ultra.
        ReasoningMode::Standard
    } else {
        persisted.reasoning_mode.unwrap_or(config.reasoning_mode)
    };
    Ok((
        prepared,
        LaunchConfiguration {
            model,
            reasoning,
            reasoning_mode,
        },
    ))
}

fn should_pick_interactive_model(
    config: &Config,
    catalog: &ModelCatalog,
    model: Option<&ModelId>,
) -> bool {
    match model {
        None => true,
        // Keep an explicit CLI selection authoritative so invalid values still
        // produce the usual configuration error rather than silently changing
        // the requested model.
        Some(_) if config.model_explicit => false,
        // A session may outlive the credential that made its model available.
        // Do not carry that stale model into build_app; let the user choose a
        // currently configured route instead.
        Some(model) => catalog.resolve(model).is_err(),
    }
}

fn launch_configuration(
    boot: &Bootstrap,
    session: &SessionSelection,
) -> anyhow::Result<LaunchConfiguration> {
    let (prepared, configuration) = launch_configuration_parts(&boot.config, session)?;
    *boot.prepared_session.borrow_mut() = prepared;
    Ok(configuration)
}

fn resolve_fork_source_path(
    config: &Config,
    store: &SessionStore,
    source: &str,
) -> anyhow::Result<PathBuf> {
    let candidate = Path::new(source);
    if candidate.is_absolute()
        || candidate.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
    {
        let path = if candidate.is_absolute() {
            candidate.to_owned()
        } else {
            config.invocation_cwd.join(candidate)
        };
        return path
            .canonicalize()
            .with_context(|| format!("could not resolve fork source {}", path.display()));
    }
    store.path_by_id(source)
}

fn fork_session_into(
    store: &SessionStore,
    source_path: &std::path::Path,
    destination_path: PathBuf,
) -> anyhow::Result<PathBuf> {
    let source = Session::open_read_only(source_path).with_context(|| {
        format!(
            "could not open source session for forking: {}",
            source_path.display()
        )
    })?;
    let source_id = source
        .path()
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| anyhow::anyhow!("source session has no valid id"))?;
    let checkpoint = source.head();
    let forked = source
        .fork_to(destination_path.clone(), checkpoint.as_ref())
        .with_context(|| {
            format!(
                "could not fork session {} into {}",
                source_path.display(),
                destination_path.display()
            )
        })?;
    drop(forked);
    if let Some(checkpoint) = checkpoint {
        let destination_id = destination_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| anyhow::anyhow!("forked session has no valid id"))?;
        if let Err(error) =
            store.set_fork_provenance(destination_id, source_id, checkpoint.0.as_str())
        {
            let _ = std::fs::remove_file(&destination_path);
            return Err(error);
        }
    }
    Ok(destination_path)
}

/// Resolve an interactive launch and open pickers only while no Agent exists.
pub async fn resolve_launch_interactive(
    boot: &Bootstrap,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
) -> anyhow::Result<LaunchSelection> {
    let session = match boot.config.resume.clone() {
        ResumeSelector::New => {
            SessionSelection::CreateNew(boot.sessions.new_path(&crate::modes::timestamp()))
        }
        ResumeSelector::Continue => {
            let sessions = boot.sessions.clone();
            let path =
                run_blocking_startup_lifecycle(shell, input, "latest session lookup", move || {
                    Ok(sessions.latest()?.path)
                })
                .await?;
            SessionSelection::OpenExisting(path)
        }
        ResumeSelector::Resume(Some(id)) => {
            let sessions = boot.sessions.clone();
            let path = run_blocking_startup_lifecycle(shell, input, "session lookup", move || {
                sessions.path_by_id(&id)
            })
            .await?;
            SessionSelection::OpenExisting(path)
        }
        ResumeSelector::Fork(source_id) => {
            let source_path = if let Some(id) = source_id {
                let sessions = boot.sessions.clone();
                let config = boot.config.clone();
                run_blocking_startup_lifecycle(shell, input, "source session lookup", move || {
                    resolve_fork_source_path(&config, &sessions, &id)
                })
                .await?
            } else {
                let sessions = boot.sessions.clone();
                let available =
                    run_blocking_startup_lifecycle(shell, input, "session discovery", move || {
                        Ok(sessions.list())
                    })
                    .await?;
                session_picker(shell, input, &available, &boot.sessions, None)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("session selection cancelled"))?
            };
            let store = boot.sessions.clone();
            let destination = boot.sessions.new_path(&crate::modes::timestamp());
            let path = run_blocking_startup_lifecycle(shell, input, "session fork", move || {
                fork_session_into(&store, &source_path, destination)
            })
            .await?;
            SessionSelection::OpenExisting(path)
        }
        ResumeSelector::Resume(None) => {
            let sessions = boot.sessions.clone();
            let available =
                run_blocking_startup_lifecycle(shell, input, "session discovery", move || {
                    Ok(sessions.list())
                })
                .await?;
            session_picker(shell, input, &available, &boot.sessions, None)
                .await?
                .map(SessionSelection::OpenExisting)
                .ok_or_else(|| anyhow::anyhow!("session selection cancelled"))?
        }
    };
    let (
        prepared,
        LaunchConfiguration {
            model,
            reasoning,
            reasoning_mode,
        },
    ) = if matches!(&session, SessionSelection::CreateNew(_)) {
        // A fresh launch only copies configuration: no file exists to replay.
        // Avoid cloning the whole Config and dispatching a blocking worker.
        launch_configuration_parts(&boot.config, &session)?
    } else {
        let config = boot.config.clone();
        let selected_session = session.clone();
        run_blocking_startup_lifecycle(shell, input, "session replay", move || {
            launch_configuration_parts(&config, &selected_session)
        })
        .await?
    };
    startup_phase("session.replay");
    *boot.prepared_session.borrow_mut() = prepared;
    // Provider declarations are only needed before launch when no static model
    // can satisfy the restored/explicit selection. Do not start ordinary
    // extensions on a temporary session just to confirm an already-known route.
    let needs_extension_provider_catalog = model
        .as_ref()
        .is_none_or(|model| boot.catalog.resolve(model).is_err());
    if needs_extension_provider_catalog {
        boot.preflight_extension_providers()?;
    }
    let catalog = boot.catalog_with_extension_providers();
    let no_configured_model = !boot.config.model_explicit && catalog.models().next().is_none();
    let pick_model = should_pick_interactive_model(&boot.config, &catalog, model.as_ref());
    let model = if no_configured_model {
        boot.enter_modeless_mode();
        shell.notice("no configured model; opening session read-only");
        shell.render();
        model.unwrap_or_else(|| ModelId(String::new()))
    } else {
        match model {
            Some(model) if !pick_model => model,
            Some(_) => {
                shell.notice("selected model is unavailable; select a configured model");
                shell.render();
                model_picker(shell, input, &catalog).await?
            }
            None => model_picker(shell, input, &catalog).await?,
        }
    };
    let reasoning = match reasoning {
        Some(reasoning) => reasoning,
        None if boot.is_modeless() => ReasoningConfig::Off,
        None => default_reasoning_for_model(&catalog.resolve(&model)?),
    };
    // The Codex context note is NOT emitted here: startup shows nothing, so the
    // note is delivered lazily by whoever owns the transcript
    // (`Bootstrap::take_codex_context_note`: the first assistant turn after
    // readiness, or an on-demand context surface). Recording it stayed in
    // catalog construction, so the wording is still effective-model-only.
    Ok(LaunchSelection {
        model,
        session,
        reasoning,
        reasoning_mode,
    })
}

/// Resolve a print launch without opening an interactive picker.
pub fn resolve_launch_print(boot: &Bootstrap, stamp: &str) -> anyhow::Result<LaunchSelection> {
    let session = match &boot.config.resume {
        ResumeSelector::New => SessionSelection::CreateNew(boot.sessions.new_path(stamp)),
        ResumeSelector::Continue => SessionSelection::OpenExisting(boot.sessions.latest()?.path),
        ResumeSelector::Resume(Some(id)) => {
            SessionSelection::OpenExisting(boot.sessions.path_by_id(id)?)
        }
        ResumeSelector::Fork(Some(id)) => {
            let source = resolve_fork_source_path(&boot.config, &boot.sessions, id)?;
            let destination = boot.sessions.new_path(stamp);
            SessionSelection::OpenExisting(fork_session_into(&boot.sessions, &source, destination)?)
        }
        ResumeSelector::Fork(None) => {
            anyhow::bail!("--fork needs a session id in print mode")
        }
        ResumeSelector::Resume(None) => {
            anyhow::bail!("--resume needs a session id in print mode")
        }
    };
    let LaunchConfiguration {
        model,
        reasoning,
        reasoning_mode,
    } = launch_configuration(boot, &session)?;
    let catalog = if model
        .as_ref()
        .is_some_and(|model| boot.catalog.resolve(model).is_err())
    {
        boot.preflight_extension_providers()?;
        boot.catalog_with_extension_providers()
    } else {
        boot.catalog.clone()
    };
    let model = model.ok_or_else(|| {
        let mut models = catalog
            .models()
            .map(|model| model.id.0.clone())
            .collect::<Vec<_>>();
        models.sort();
        let diagnosis = match crate::provider_setup::ProviderSetupService::<
            crate::provider_setup::HttpSetupProbe,
        >::readiness(&catalog, None)
        {
            crate::provider_setup::ProviderSetupState::NoProvider => {
                "no provider is configured; run `octet setup --yes` for an explicitly selected OpenAI-compatible endpoint".to_owned()
            }
            crate::provider_setup::ProviderSetupState::Ready => {
                "no model configured; run `octet setup --yes` for an explicitly selected OpenAI-compatible endpoint".to_owned()
            }
            state => state.to_string(),
        };
        anyhow::anyhow!(
            "{diagnosis}, pass --model <id>, resume a session with model provenance, or set model in .octet/config.toml (available: {})",
            models.join(", ")
        )
    })?;
    if catalog.resolve(&model).is_err() {
        let mut available = catalog
            .models()
            .map(|model| model.id.0.clone())
            .collect::<Vec<_>>();
        available.sort();
        let diagnosis = crate::provider_setup::ProviderSetupService::<
            crate::provider_setup::HttpSetupProbe,
        >::readiness(&catalog, Some(&model));
        anyhow::bail!(
            "{diagnosis}; run `octet setup --yes` or choose an available model (available: {})",
            available.join(", ")
        );
    }

    let reasoning = match reasoning {
        Some(reasoning) => reasoning,
        None => default_reasoning_for_model(&catalog.resolve(&model)?),
    };
    // Headless print/RPC launches have no lazy surface to deliver the note on,
    // and stderr is neither the TUI frame nor the transcript, so the one bounded
    // line still goes there. The interactive launch above stays silent.
    if let Some(note) = boot.codex_context_note(&model) {
        crate::output::stderr!("{note}");
    }
    Ok(LaunchSelection {
        model,
        session,
        reasoning,
        reasoning_mode,
    })
}

/// Conservative character-based token estimate used for capacity reserves.
pub fn estimate_text_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(4)
}

/// Estimate the reserved serialized size of the exact tool schemas registered
/// for the agent, including optional product extensions such as skills.
pub fn tool_schema_reserve(definitions: &[ToolDef]) -> u64 {
    estimate_text_tokens(&serde_json::to_string(definitions).unwrap_or_default())
}

fn create_private_session_dir(path: &std::path::Path) -> std::io::Result<()> {
    octet_agent::secure_fs::create_private_directory_all(path).map_err(std::io::Error::other)
}

fn descriptor_session_path(path: &std::path::Path) -> std::io::Result<PathBuf> {
    if !path.is_absolute() || path.file_name().is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session path must be absolute and identify a file",
        ));
    }
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::CurDir | std::path::Component::ParentDir
        )
    }) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session path must not contain traversal components",
        ));
    }
    Ok(path.to_owned())
}

fn validate_explicit_tool_policy(
    config: &Config,
    extensions: &ExtensionHost,
    model: &Model,
    has_dynamic_tool_provider: bool,
) -> anyhow::Result<()> {
    let Some(requested) = config.tools.explicit_names() else {
        return Ok(());
    };
    let requested = requested.collect::<Vec<_>>();
    if !model.spec.capabilities.tools && !requested.is_empty() {
        anyhow::bail!(
            "model {} does not support tools, but the explicit tool policy requested: {}",
            model.spec.id.0,
            requested.join(", "),
        );
    }
    let registered = extensions
        .tool_definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect::<std::collections::BTreeSet<_>>();
    let missing = requested
        .into_iter()
        .filter(|name| {
            !registered.contains(*name)
                && (!has_dynamic_tool_provider || !config.tool_available(name))
        })
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        let available = if registered.is_empty() {
            "(none)".to_owned()
        } else {
            registered.iter().cloned().collect::<Vec<_>>().join(", ")
        };
        anyhow::bail!(
            "requested tool(s) are unavailable after allowlists, sandbox gates, and extension registration: {}; available tools: {available}",
            missing.join(", "),
        )
    }
}

fn apply_extension_tool_policy(host: &mut ExtensionHost, config: &Config, model: &Model) {
    let tool_config = config.clone();
    let model_supports_tools = model.spec.capabilities.tools;
    host.set_tool_policy(move |name| model_supports_tools && tool_config.tool_available(name));
    host.finalize_tool_surface();
}

fn configured_extension_host(
    config: &Config,
    model: &Model,
) -> anyhow::Result<(ExtensionHost, Option<TelemetryObserver>)> {
    let mut host = ExtensionHost::new();
    host.load(&CoreTools);
    apply_extension_tool_policy(&mut host, config, model);
    let telemetry = config
        .telemetry
        .as_deref()
        .map(|path| TelemetryObserver::new(path, env!("CARGO_PKG_VERSION")))
        .transpose()?;
    if let Some(observer) = &telemetry {
        host.observe(observer.clone());
    }
    Ok((host, telemetry))
}

#[cfg(test)]
fn configured_extensions(
    config: &Config,
    session: &Session,
    model: &Model,
    reasoning: &ReasoningConfig,
    sessions: &SessionStore,
) -> anyhow::Result<(ExtensionHost, ExecutableExtensions)> {
    configured_extensions_with_runtime_manager(
        config,
        session,
        model,
        reasoning,
        sessions,
        None,
        ExtensionProviderRuntime::default(),
    )
}

fn configured_extensions_with_runtime_manager(
    config: &Config,
    session: &Session,
    model: &Model,
    reasoning: &ReasoningConfig,
    sessions: &SessionStore,
    runtime_manager: Option<ExtensionRuntimeManager>,
    provider_runtime: ExtensionProviderRuntime,
) -> anyhow::Result<(ExtensionHost, ExecutableExtensions)> {
    let (mut extensions, telemetry) = configured_extension_host(config, model)?;
    let mut executable_extensions = ExecutableExtensions::discover_and_start_with_provider_runtime(
        config,
        session,
        model,
        reasoning,
        sessions,
        &mut extensions,
        runtime_manager,
        provider_runtime,
    );
    executable_extensions.set_telemetry(telemetry);
    startup_phase("extensions.activate");
    Ok((extensions, executable_extensions))
}

fn terminal_goal_store(config: &Config) -> anyhow::Result<Arc<DurableGoalStore>> {
    // Serve and the terminal intentionally use the same private directory and
    // file schema. The terminal does not depend on Serve being enabled; this
    // is only a shared on-disk location for first-party frontends.
    let session_dir = if config.session_dir.is_absolute() {
        config.session_dir.clone()
    } else {
        std::env::current_dir()?.join(&config.session_dir)
    };
    let root = session_dir.join(".serve").join("goals");
    DurableGoalStore::open(&root)
        .map(Arc::new)
        .map_err(|error| anyhow::anyhow!("unable to open durable goal store: {error}"))
}

pub(crate) fn terminal_goal_session_id(session: &Session) -> anyhow::Result<String> {
    session
        .path()
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("current session has no valid goal identity"))
}

fn subagents_surface_available(
    executable_extensions: &ExecutableExtensions,
    extensions: &ExtensionHost,
    model: &Model,
) -> bool {
    executable_extensions.has_agent_session_service()
        && model.spec.capabilities.tools
        && extensions
            .tool_definitions()
            .iter()
            .any(|definition| definition.name == "subagent_spawn")
}

/// A live worker surface can select any configured provider while the root run
/// borrows the App. Complete deferred inventories at this idle boundary, not in
/// a model tool on a Tokio worker. Disabled/unavailable extensions retain the
/// ordinary narrow startup. Extension routes are reprojected by the caller.
fn complete_delegation_catalog(
    service_available: bool,
    config: &Config,
    catalog: &mut ModelCatalog,
    readiness: &mut CatalogReadiness,
    notes: &mut CodexContextNotes,
) -> anyhow::Result<()> {
    if service_available && !readiness.is_fleet() {
        let (complete, additional_notes) =
            model_catalog_for_readiness(config.offline, &CatalogReadiness::Fleet)?;
        *catalog = complete;
        notes.merge(additional_notes);
        *readiness = CatalogReadiness::Fleet;
    }
    Ok(())
}

fn configure_v2_delegation(
    agent: &mut Agent,
    model: &Model,
    reasoning: &ReasoningConfig,
    service_available: bool,
) -> anyhow::Result<()> {
    if !service_available {
        if matches!(
            reasoning,
            ReasoningConfig::Effort(octet_ai::ReasoningEffort::Ultra)
        ) {
            anyhow::bail!(
                "Ultra requires the trusted {SUBAGENTS_EXTENSION_NAME} extension to observe delegated work"
            );
        }
        return Ok(());
    }
    if matches!(
        reasoning,
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::Ultra)
    ) && !model_supports_ultra(model)
    {
        anyhow::bail!(
            "Ultra requires an available V2 delegation runtime for model {}",
            model.spec.id.0
        );
    }
    let session_parent = agent
        .session()
        .path()
        .parent()
        .ok_or_else(|| anyhow::anyhow!("session path has no parent directory"))?;
    agent
        .enable_v2_delegation_extension_only(DelegationConfig::new(
            session_parent.join(".delegation"),
        ))
        .with_context(|| "could not initialize the extension-owned delegation runtime")?;
    Ok(())
}

pub(crate) fn open_launch_session(
    prepared_session: &mut Option<Session>,
    selection: SessionSelection,
) -> anyhow::Result<Session> {
    match selection {
        SessionSelection::CreateNew(path) => {
            if let Some(parent) = path.parent() {
                create_private_session_dir(parent)?;
            }
            let descriptor_path = descriptor_session_path(&path)?;
            let file = create_regular_file_for_append(&descriptor_path)?;
            Ok(Session::create_with_file(path, file)?)
        }
        SessionSelection::OpenExisting(path) => match prepared_session.take() {
            Some(session) if session.path() == path => Ok(session),
            _ => {
                let descriptor_path = descriptor_session_path(&path)?;
                let file = open_regular_file_for_append(&descriptor_path)?;
                Ok(Session::open_with_file(path, file)?)
            }
        },
    }
}

/// Builds an App with a fresh ordinary-host extension runtime manager.
pub fn build_app(boot: Bootstrap, launch: LaunchSelection, system: String) -> anyhow::Result<App> {
    build_app_with_runtime_manager(boot, launch, system, None)
}

/// Builds an App using an explicit host-owned extension runtime manager.
///
/// Serve supplies a separately trust-partitioned manager through this seam;
/// ordinary callers retain the historical local-host domain via [`build_app`].
pub(crate) fn build_app_with_runtime_manager(
    boot: Bootstrap,
    launch: LaunchSelection,
    system: String,
    runtime_manager: Option<ExtensionRuntimeManager>,
) -> anyhow::Result<App> {
    let Bootstrap {
        mut config,
        mut catalog,
        sessions,
        client,
        provider_runtime,
        prestarted_extensions,
        prepared_session,
        modeless: _,
        mut codex_context_notes,
        mut readiness,
    } = boot;
    let mut system = system;
    startup_phase("app.build");
    let mut prestarted_extensions = prestarted_extensions.into_inner();
    if let Some((_, extensions)) = prestarted_extensions.as_mut() {
        extensions.synchronize_provider_catalog(&mut catalog, &client);
        startup_phase("extensions.prestart");
    }
    // A temporary provider preflight has just enough catalog state to identify
    // the requested model. Keep that snapshot only as an initialization input;
    // the final extension fleet starts after it is torn down and sees the real
    // selected session/model state.
    let bootstrap_model = catalog.resolve(&launch.model)?;
    let bootstrap_reasoning = normalize_reasoning_for_model(&launch.reasoning, &bootstrap_model)?;
    let requested_reasoning_mode = launch.reasoning_mode;
    let mut prepared_session = prepared_session.into_inner();
    let mut session = open_launch_session(&mut prepared_session, launch.session)?;
    // Above 272K the whole request is priced differently, so the durable record
    // must mark the route uncertain at launch rather than let a later
    // exact-looking cost claim stand. Sticky, and written at most once.
    record_codex_context_uncertainty(&mut session, &bootstrap_model)?;

    if let Some((_, mut extensions)) = prestarted_extensions.take() {
        extensions.clear_provider_catalog(&mut catalog, &client);
        extensions.shutdown_blocking();
    }

    let skills: Arc<dyn SkillRegistry> = Arc::new(FileSystemSkillRegistry::new_with_invocation(
        config.workspace.clone(),
        config.invocation_cwd.clone(),
        config.skill_paths.clone(),
        config.workspace_trusted,
    )?);
    system.push_str(&format_skills_for_prompt(&skills.descriptors()));
    let prompts = Arc::new(PromptRegistry::discover(
        &config.workspace,
        &config.prompt_paths,
        config.workspace_trusted,
    ));
    let (mut extensions, mut executable_extensions) = configured_extensions_with_runtime_manager(
        &config,
        &session,
        &bootstrap_model,
        &bootstrap_reasoning,
        &sessions,
        runtime_manager,
        provider_runtime,
    )?;
    complete_delegation_catalog(
        executable_extensions.has_agent_session_service(),
        &config,
        &mut catalog,
        &mut readiness,
        &mut codex_context_notes,
    )?;
    executable_extensions.synchronize_provider_catalog(&mut catalog, &client);
    let model = catalog.resolve(&launch.model)?;
    // Keep the original persisted/explicit choice until the diagnostic boundary;
    // pre-normalizing it there would silently hide an unsupported Off selection.
    let requested_reasoning = launch.reasoning;
    let normalized_reasoning = normalize_reasoning_for_model(&requested_reasoning, &model)?;
    // `bootstrap_model` can be an old provider generation. The extension host
    // state exposes only the selected model identity, but refresh both the
    // policy surface and snapshots from the final catalog before any App work.
    apply_extension_tool_policy(&mut extensions, &config, &model);
    executable_extensions.refresh_host_state(&session, &model, &normalized_reasoning, &sessions);
    let compact_model = config
        .compaction
        .compact_model
        .as_ref()
        .map(|id| catalog.resolve(id))
        .transpose()
        .with_context(|| "configured compaction model could not be resolved")?;
    validate_compaction_route(config.compaction.mode, &model, compact_model.as_ref())?;
    validate_native_compaction_replay(config.compaction.mode, &session, &model)?;
    let service_available = executable_extensions.has_agent_session_service();
    let subagents_available = service_available
        && subagents_surface_available(&executable_extensions, &extensions, &model);
    let (reasoning, reasoning_mode, migration_diagnostic) =
        normalize_reasoning_selection_for_model_with_subagents(
            &requested_reasoning,
            requested_reasoning_mode,
            &model,
            subagents_available,
        )?;
    crate::output::checked_diagnostics(
        crate::output::DiagnosticComponent::Bootstrap("reasoning-migration".into()),
        migration_diagnostic
            .into_iter()
            .map(|diagnostic| format!("warning: {diagnostic}"))
            .collect(),
        true,
    );
    config.model = Some(model.spec.id.clone());
    config.reasoning = Some(reasoning.clone());
    config.reasoning_mode = reasoning_mode;
    append_config_if_changed(&mut session, &model.spec.id, &reasoning, reasoning_mode)?;
    validate_explicit_tool_policy(
        &config,
        &extensions,
        &model,
        executable_extensions.has_dynamic_tool_provider(),
    )?;
    let goal_store = terminal_goal_store(&config)?;
    let goal_session_id = terminal_goal_session_id(&session)?;
    let goal_driver = GoalDriver::new(goal_store.clone(), goal_session_id.clone());
    let mut agent = Agent::new(AgentConfig {
        client: client.clone(),
        model: model.clone(),
        session,
        system: system.clone(),
        sandbox: config.sandbox.to_sandbox_config(&config.workspace),
        effect_broker: EffectBroker::new(config.effect_policy),
        extensions,
        max_turns: config.max_turns,
        reasoning: reasoning.clone(),
        reasoning_mode,
        cache_retention: config.cache_retention,
        session_id: None,
    })?;
    if agent.reasoning() != &reasoning {
        // Agent resumes the durable effective effort. Apply an explicit launch
        // override without overwriting the request's cached reasoning baseline.
        agent.set_reasoning(reasoning.clone())?;
    }
    #[cfg(any(unix, windows))]
    agent.enable_session_partial_output_checkpoints(
        "bash",
        octet_agent::tools::BASH_CHECKPOINT_INTERVAL,
    );
    agent.set_prompt_model_source(Some(crate::tui::theme::model_lab(&model).key().to_owned()));
    agent.set_prompt_color(Some(crate::tui::theme::prompt_color_for_model(&model)));
    agent.set_compaction_model(compact_model);
    agent.set_compaction_token_mode(
        agent_compaction_mode(config.compaction.mode),
        effective_compaction_threshold_fraction(&config, &model),
        config.compaction.keep_recent_tokens,
    )?;
    agent.set_max_session_cost_microdollars(config.max_cost_microdollars);
    if service_available {
        agent.set_delegation_model_resolver(Arc::new(
            super::delegation_models::CodingAgentModelResolver::new(catalog.clone()),
        ));
    }
    configure_v2_delegation(&mut agent, &model, &reasoning, service_available)?;
    executable_extensions.bind_agent_sessions(&agent)?;
    agent.finalize_tool_surface();
    let system_tokens = estimate_text_tokens(agent.system_prompt());

    Ok(App {
        agent,
        model,
        client,
        config,
        catalog,
        model_scope: None,
        sessions,
        reasoning,
        reasoning_mode,
        system,
        system_tokens,
        skills,
        prompts,
        executable_extensions,
        goal_store,
        goal_driver,
        goal_session_id,
        codex_context_notes,
        readiness,
    })
}

struct ReleasedExtensionBindingCleanup {
    extensions: Option<ExecutableExtensions>,
}

impl ReleasedExtensionBindingCleanup {
    fn new(extensions: ExecutableExtensions) -> Self {
        Self {
            extensions: Some(extensions),
        }
    }

    fn disarm(mut self) {
        // The replacement App now owns a clone of the same manager. Dropping
        // the released wrapper is intentional: its process list is empty, so
        // its normal Drop path cannot terminate the shared fleet.
        self.extensions.take();
    }
}

impl Drop for ReleasedExtensionBindingCleanup {
    fn drop(&mut self) {
        if let Some(mut extensions) = self.extensions.take() {
            // Once the original binding has been released, any later rebuild
            // error has no App left to own terminal fleet shutdown. Do not
            // leave a workspace service behind on this failure path.
            extensions.shutdown_blocking();
        }
    }
}

/// Recreate the Agent at an idle boundary. Taking `App` by value guarantees the
/// old Agent and its session file are dropped before a session is reopened.
pub fn rebuild_app(
    mut app: App,
    new_model: Option<Model>,
    new_reasoning: Option<ReasoningConfig>,
    new_reasoning_mode: Option<ReasoningMode>,
    selection: Option<SessionSelection>,
) -> anyhow::Result<App> {
    app.synchronize_extension_provider_catalog();
    let mut config = app.config.clone();
    let mut catalog = app.catalog.clone();
    let model_scope = app.model_scope.clone();
    let sessions = app.sessions.clone();
    let client = app.client.clone();
    let model = app.model.clone();
    let reasoning = app.reasoning.clone();
    let reasoning_mode = app.reasoning_mode;
    let system = app.system.clone();
    let old_skills = Arc::clone(&app.skills);
    let goal_store = Arc::clone(&app.goal_store);
    // Idle rebuilds of the same session preserve delivery. A new or resumed
    // different session owns a fresh latch; the previous session's notice must
    // not suppress the effective-model note in the newly opened transcript.
    let mut codex_context_notes = app.codex_context_notes.clone();
    if selection.as_ref().is_some_and(|selection| {
        let path = match selection {
            SessionSelection::CreateNew(path) | SessionSelection::OpenExisting(path) => path,
        };
        path != app.agent.session().path()
    }) {
        codex_context_notes.delivered.set(false);
    }
    let mut readiness = app.readiness.clone();
    let compact_model = config
        .compaction
        .compact_model
        .as_ref()
        .map(|id| catalog.resolve(id))
        .transpose()
        .with_context(|| "configured compaction model could not be resolved")?;
    let current_path = app.agent.session().path().to_owned();
    let same_session = selection.as_ref().is_none_or(|selection| match selection {
        SessionSelection::CreateNew(_) => false,
        SessionSelection::OpenExisting(path) => path == &current_path,
    });
    let service_tier = same_session.then(|| app.agent.service_tier()).flatten();
    let old_skill_metadata = format_skills_for_prompt(&old_skills.descriptors());
    let mut system = system;
    if !old_skill_metadata.is_empty() && system.ends_with(&old_skill_metadata) {
        system.truncate(system.len() - old_skill_metadata.len());
    }

    let (persisted, mut prepared_session) = match selection.as_ref() {
        Some(SessionSelection::OpenExisting(path)) => {
            let descriptor_path = descriptor_session_path(path)?;
            let file = open_regular_file_for_append(&descriptor_path)?;
            let session = Session::open_with_file(path, file)?;
            let persisted = persisted_session_config(&session)?;
            (persisted, Some(session))
        }
        Some(SessionSelection::CreateNew(_)) | None => (PersistedSessionConfig::default(), None),
    };
    let restored_model = persisted
        .model
        .as_ref()
        .map(|id| catalog.resolve(id))
        .transpose()?;
    let changing_model = new_model.is_some() || restored_model.is_some();
    let explicit_reasoning = new_reasoning.is_some();
    let old_model = model;
    let model = new_model
        .or(restored_model)
        .unwrap_or_else(|| old_model.clone());
    // A tier cannot leak across routes. A compatible same-session rebuild
    // retains it; changing to an unsupported route clears it explicitly.
    let service_tier = if crate::commands::codex_fast_tier_endpoint(&model) {
        service_tier
    } else {
        if service_tier.is_some() {
            crate::output::stderr!(
                "warning: fast mode disabled: the selected model route does not support it"
            );
        }
        None
    };
    validate_compaction_route(config.compaction.mode, &model, compact_model.as_ref())?;
    let requested_reasoning = match (new_reasoning, persisted.reasoning) {
        (Some(reasoning), _) | (None, Some(reasoning)) => reasoning,
        (None, None) if changing_model => {
            let level = level_from_reasoning(&reasoning, &old_model)?;
            thinking_to_reasoning(level, &model)?
        }
        (None, None) => reasoning,
    };
    // Validate before releasing the old agent, but retain the original choice
    // for the visible migration diagnostic after extension gates are known.
    let normalized_reasoning = normalize_reasoning_for_model(&requested_reasoning, &model)?;
    let requested_reasoning_mode = if let Some(mode) = new_reasoning_mode {
        mode
    } else if explicit_reasoning {
        // Rebuilds use the same precedence as startup: an explicit current
        // effort supersedes the obsolete Pro bit persisted in a session.
        ReasoningMode::Standard
    } else {
        persisted.reasoning_mode.unwrap_or(reasoning_mode)
    };
    let candidate_session = match selection.as_ref() {
        Some(SessionSelection::OpenExisting(_)) => prepared_session.as_ref(),
        Some(SessionSelection::CreateNew(_)) => None,
        None => Some(app.agent.session()),
    };
    if let Some(candidate_session) = candidate_session {
        validate_native_compaction_replay(config.compaction.mode, candidate_session, &model)?;
    }
    // Do not tear down the working agent or its executable extensions until
    // the complete candidate route and reasoning configuration is known valid.
    // Keep the host-level runtime manager, but release the old App binding so
    // only an explicitly workspace-shared, content-identical process can
    // survive the compatible rebuild.
    let mut released_extensions = std::mem::take(&mut app.executable_extensions);
    let runtime_manager = released_extensions.runtime_manager();
    let provider_runtime = released_extensions.provider_runtime();
    released_extensions.clear_provider_catalog(&mut catalog, &client);
    released_extensions.release_binding_blocking();
    // These are actual queue discards, not inferred interruption risks. Use the
    // existing terminal-aware diagnostic route: interactive lifecycle callers
    // defer this queue through final hydration; never print into the raw frame.
    for notice in released_extensions.discard_stale_host_requests() {
        crate::output::stderr!("{notice}");
    }
    let released_extensions = ReleasedExtensionBindingCleanup::new(released_extensions);
    drop(app);
    let mut session = match selection {
        Some(SessionSelection::CreateNew(path)) => {
            if let Some(parent) = path.parent() {
                create_private_session_dir(parent)?;
            }
            let descriptor_path = descriptor_session_path(&path)?;
            let file = create_regular_file_for_append(&descriptor_path)?;
            Session::create_with_file(path, file)?
        }
        Some(SessionSelection::OpenExisting(path)) => match prepared_session.take() {
            Some(session) if session.path() == path => session,
            _ => {
                let descriptor_path = descriptor_session_path(&path)?;
                let file = open_regular_file_for_append(&descriptor_path)?;
                Session::open_with_file(path, file)?
            }
        },
        None => {
            let descriptor_path = descriptor_session_path(&current_path)?;
            let file = open_regular_file_for_append(&descriptor_path)?;
            Session::open_with_file(current_path, file)?
        }
    };
    // A mid-session switch onto a raised Codex route must not leave an
    // exact-looking cost behind; the record is sticky, so an ordinary rebuild is
    // a no-op.
    record_codex_context_uncertainty(&mut session, &model)?;
    let goal_session_id = terminal_goal_session_id(&session)?;
    let goal_driver = GoalDriver::new(goal_store.clone(), goal_session_id.clone());

    let skills: Arc<dyn SkillRegistry> = Arc::new(FileSystemSkillRegistry::new_with_invocation(
        config.workspace.clone(),
        config.invocation_cwd.clone(),
        config.skill_paths.clone(),
        config.workspace_trusted,
    )?);
    system.push_str(&format_skills_for_prompt(&skills.descriptors()));
    let prompts = Arc::new(PromptRegistry::discover(
        &config.workspace,
        &config.prompt_paths,
        config.workspace_trusted,
    ));
    let (extensions, mut executable_extensions) = configured_extensions_with_runtime_manager(
        &config,
        &session,
        &model,
        &normalized_reasoning,
        &sessions,
        runtime_manager,
        provider_runtime,
    )?;
    complete_delegation_catalog(
        executable_extensions.has_agent_session_service(),
        &config,
        &mut catalog,
        &mut readiness,
        &mut codex_context_notes,
    )?;
    executable_extensions.synchronize_provider_catalog(&mut catalog, &client);
    let service_available = executable_extensions.has_agent_session_service();
    let subagents_available = service_available
        && subagents_surface_available(&executable_extensions, &extensions, &model);
    let (reasoning, reasoning_mode, migration_diagnostic) =
        normalize_reasoning_selection_for_model_with_subagents(
            &requested_reasoning,
            requested_reasoning_mode,
            &model,
            subagents_available,
        )?;
    crate::output::checked_diagnostics(
        crate::output::DiagnosticComponent::Bootstrap("reasoning-migration".into()),
        migration_diagnostic
            .into_iter()
            .map(|diagnostic| format!("warning: {diagnostic}"))
            .collect(),
        true,
    );
    config.model = Some(model.spec.id.clone());
    config.reasoning = Some(reasoning.clone());
    config.reasoning_mode = reasoning_mode;
    append_config_if_changed(&mut session, &model.spec.id, &reasoning, reasoning_mode)?;
    validate_explicit_tool_policy(
        &config,
        &extensions,
        &model,
        executable_extensions.has_dynamic_tool_provider(),
    )?;
    let mut agent = Agent::new(AgentConfig {
        client: client.clone(),
        model: model.clone(),
        session,
        system: system.clone(),
        sandbox: config.sandbox.to_sandbox_config(&config.workspace),
        effect_broker: EffectBroker::new(config.effect_policy),
        extensions,
        max_turns: config.max_turns,
        reasoning: reasoning.clone(),
        reasoning_mode,
        cache_retention: config.cache_retention,
        session_id: None,
    })?;
    if agent.reasoning() != &reasoning {
        agent.set_reasoning(reasoning.clone())?;
    }
    agent.set_service_tier(service_tier)?;
    #[cfg(any(unix, windows))]
    agent.enable_session_partial_output_checkpoints(
        "bash",
        octet_agent::tools::BASH_CHECKPOINT_INTERVAL,
    );
    agent.set_prompt_model_source(Some(crate::tui::theme::model_lab(&model).key().to_owned()));
    agent.set_prompt_color(Some(crate::tui::theme::prompt_color_for_model(&model)));
    agent.set_compaction_model(compact_model);
    agent.set_compaction_token_mode(
        agent_compaction_mode(config.compaction.mode),
        effective_compaction_threshold_fraction(&config, &model),
        config.compaction.keep_recent_tokens,
    )?;
    agent.set_max_session_cost_microdollars(config.max_cost_microdollars);
    if service_available {
        agent.set_delegation_model_resolver(Arc::new(
            super::delegation_models::CodingAgentModelResolver::new(catalog.clone()),
        ));
    }
    configure_v2_delegation(&mut agent, &model, &reasoning, service_available)?;
    executable_extensions.bind_agent_sessions(&agent)?;
    agent.finalize_tool_surface();
    let system_tokens = estimate_text_tokens(agent.system_prompt());

    released_extensions.disarm();
    Ok(App {
        agent,
        model,
        client,
        config,
        catalog,
        model_scope,
        sessions,
        reasoning,
        reasoning_mode,
        system,
        system_tokens,
        skills,
        prompts,
        executable_extensions,
        goal_store,
        goal_driver,
        goal_session_id,
        codex_context_notes,
        readiness,
    })
}

#[cfg(test)]
mod bounded_env_tests {
    use super::*;

    fn oversized_result(name: &str) -> Result<Option<String>, octet_ai::ConfigError> {
        Err(octet_ai::ConfigError::EnvironmentValueTooLarge {
            var: name.to_owned(),
            max_bytes: octet_ai::auth::MAX_ENV_VALUE_BYTES,
        })
    }

    #[test]
    fn bootstrap_env_adapters_accept_the_limit_and_propagate_oversized_values() {
        let at_limit = "x".repeat(octet_ai::auth::MAX_ENV_VALUE_BYTES);
        assert_eq!(
            optional_env_value("OPTIONAL", Ok(Some(at_limit.clone()))).unwrap(),
            Some(at_limit.clone())
        );
        assert_eq!(
            required_env_value("REQUIRED", Ok(Some(at_limit.clone()))).unwrap(),
            at_limit.clone()
        );
        assert_eq!(
            strict_env_value("STRICT", Ok(Some(at_limit))).unwrap(),
            Some("x".repeat(octet_ai::auth::MAX_ENV_VALUE_BYTES))
        );

        let errors = [
            optional_env_value("OPTIONAL", oversized_result("OPTIONAL")),
            required_env_value("REQUIRED", oversized_result("REQUIRED")).map(|_| None),
            strict_env_value("STRICT", oversized_result("STRICT")),
        ];
        for result in errors {
            let error = result.expect_err("oversized environment values must fail");
            assert!(error.to_string().contains("4096-byte limit"));
        }
    }

    #[test]
    fn bootstrap_env_adapters_preserve_unset_and_invalid_unicode_policies() {
        assert_eq!(optional_env_value("OPTIONAL", Ok(None)).unwrap(), None);
        assert_eq!(
            optional_env_value(
                "OPTIONAL",
                Err(octet_ai::ConfigError::InvalidEnv("OPTIONAL".to_owned()))
            )
            .unwrap(),
            None
        );
        assert_eq!(strict_env_value("STRICT", Ok(None)).unwrap(), None);
        assert!(strict_env_value(
            "STRICT",
            Err(octet_ai::ConfigError::InvalidEnv("STRICT".to_owned()))
        )
        .is_err());
        assert!(required_env_value("REQUIRED", Ok(None)).is_err());
        assert!(required_env_value(
            "REQUIRED",
            Err(octet_ai::ConfigError::InvalidEnv("REQUIRED".to_owned()))
        )
        .is_err());
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod reasoning_ingress_review_tests {
    use super::*;
    use octet_ai::types::ReasoningMetadataSource as Source;

    #[test]
    fn reasoning_ingress_always_on_preserves_decode_and_applied_semantics() {
        let entry = serde_json::json!({"reasoning": {
            "supported": true, "control": "always_on",
            "values": ["default"], "default": "default"
        }});
        let decoded = decode_reasoning_metadata(&entry).unwrap();
        assert_eq!(decoded.source, Source::Explicit);
        assert_eq!(decoded.supported, Some(true));
        assert_eq!(decoded.control, Some(ReasoningControl::AlwaysOn));
        assert_eq!(
            decoded.options.unwrap().choices(),
            vec![ReasoningConfig::On]
        );
        let mut model = crate::auth::custom::CustomModel::default();
        apply_discovered_reasoning(&entry, &mut model).unwrap();
        assert_eq!(model.reasoning_source, Some(Source::Explicit));
        assert!(model.reasoning);
        assert!(!model.reasoning_configurable);
        assert_eq!(model.reasoning_values, ["default"]);
        assert_eq!(model.reasoning_default, "default");
        let capability = custom_reasoning_capability(&model).unwrap();
        assert_eq!(capability.control, ReasoningControl::AlwaysOn);
        assert_eq!(capability.choices(), vec![ReasoningConfig::On]);
        assert!(!capability.supports(&ReasoningConfig::Off));
        assert!(!capability.supports(&ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)));
        for values in [
            serde_json::json!(["none"]),
            serde_json::json!(["none", "default"]),
            serde_json::json!(["high"]),
            serde_json::json!(["default", "high"]),
        ] {
            let invalid = serde_json::json!({"reasoning": {
                "supported": true, "control": "always_on", "values": values
            }});
            assert!(decode_reasoning_metadata(&invalid).is_err());
            assert!(apply_discovered_reasoning(&invalid, &mut model).is_err());
            assert!(model.reasoning);
            assert!(!model.reasoning_configurable);
            assert_eq!(model.reasoning_values, ["default"]);
        }
    }

    #[test]
    fn reasoning_ingress_rejects_malformed_nested_supported_assertions() {
        for supported in [
            serde_json::json!("false"),
            serde_json::json!("true"),
            serde_json::json!(0),
            serde_json::json!(1),
            serde_json::json!(null),
            serde_json::json!([]),
            serde_json::json!({}),
        ] {
            let assertion = serde_json::json!({
                "supported": supported, "values": ["low", "high"], "default": "high"
            });
            for entry in [
                serde_json::json!({"reasoning": assertion}),
                serde_json::json!({"capabilities": {"reasoning": assertion}}),
                serde_json::json!({"provider": {"reasoning": assertion}}),
                serde_json::json!({"top_provider": {"capabilities": {"reasoning": assertion}}}),
            ] {
                assert!(decode_reasoning_metadata(&entry).is_err());
                let mut model = crate::auth::custom::CustomModel::default();
                let before = serde_json::to_value(&model).unwrap();
                assert!(apply_discovered_reasoning(&entry, &mut model).is_err());
                assert_eq!(serde_json::to_value(&model).unwrap(), before);
            }
            // An explicit false still short-circuits all competing metadata.
            let disabled = serde_json::json!({"reasoning": false,
                "provider": {"reasoning": assertion}});
            let decoded = decode_reasoning_metadata(&disabled).unwrap();
            assert_eq!(decoded.source, Source::Explicit);
            assert_eq!(decoded.supported, Some(false));
        }
        assert_eq!(
            decode_reasoning_metadata(&serde_json::json!({}))
                .unwrap()
                .source,
            Source::Absent
        );
        assert_eq!(
            decode_reasoning_metadata(&serde_json::json!({"reasoning": null}))
                .unwrap()
                .source,
            Source::Unknown
        );
    }
}

#[cfg(test)]
mod codex_context_note_regression_tests {
    use super::tests::codex_discovered_model;
    use super::*;
    use crate::codex_context::codex_context_session_note;

    /// A synthetic, non-localhost subscription credential (the shape
    /// `crate::auth::codex` accepts) so registration produces the full fallback
    /// Codex inventory without any network access.
    fn write_codex_credential(path: &std::path::Path, plan: &str) {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;

        let payload = serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct_test",
                "chatgpt_plan_type": plan,
                "localhost": false
            }
        });
        let access = format!(
            "h.{}.s",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap())
        );
        let bytes = serde_json::to_vec(&serde_json::json!({
            "tokens": {
                "access_token": access,
                "refresh_token": "refresh",
                "account_id": "acct_test"
            },
            "expires_at": u64::MAX
        }))
        .unwrap();
        octet_agent::secure_fs::write_private_atomic(path, &bytes, 1024 * 1024).unwrap();
    }

    fn registered_notes(plan: &str) -> (ModelCatalog, CodexContextNotes) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("codex.json");
        write_codex_credential(&path, plan);
        let store = crate::auth::codex::CredentialStore::new(&path);
        let mut catalog = base_model_catalog(true).unwrap();
        let mut notes = CodexContextNotes::default();
        register_openai_codex_with_notes(&mut catalog, store, true, &mut notes).unwrap();
        (catalog, notes)
    }

    /// The catalog id a Codex api id was registered under.
    ///
    /// Registration namespaces by collision (`codex/gpt-6-astra` is always
    /// namespaced; other ids only when the bare id was already taken), so the
    /// test resolves the actual catalog id from the Codex endpoint instead of
    /// re-deriving the rule after the fact.
    fn catalog_id(catalog: &ModelCatalog, model_id: &str) -> ModelId {
        let codex_endpoint = EndpointId(crate::auth::codex::ENDPOINT_ID.to_owned());
        catalog
            .models()
            .find(|model| model.endpoint == codex_endpoint && model.api_name == model_id)
            .map(|model| model.id.clone())
            .unwrap_or_else(|| panic!("{model_id} is registered on the Codex endpoint"))
    }

    /// Catalog enumeration must never print. It records at most one note for the
    /// models whose effective window needs one, and nothing for a route whose
    /// window is the deliberate cap itself.
    ///
    /// A "reduced" route is one whose effective window is below what the plan
    /// advertises, or above the 272K standard tier (`gpt-5.6-luna`). On a Plus
    /// plan the backend applies its own default window, so `gpt-6-astra` is not
    /// reduced even though its entitlement ceiling is 872K; on a Pro plan the
    /// advertised window is raised and every 872K/1M family is reduced.
    #[test]
    fn registration_records_one_note_per_reduced_model_and_none_for_an_unreduced_route() {
        for (plan, reduced, plain) in [
            (
                "plus",
                &["gpt-5.6-luna", "gpt-5.6-sol", "gpt-5.6-terra"][..],
                &["gpt-6-astra", "gpt-5.5", "gpt-5.4", "gpt-5.4-mini"][..],
            ),
            (
                "pro",
                &[
                    "gpt-6-astra",
                    "gpt-5.4",
                    "gpt-5.6-luna",
                    "gpt-5.6-sol",
                    "gpt-5.6-terra",
                ][..],
                &["gpt-5.5", "gpt-5.4-mini"][..],
            ),
        ] {
            let (catalog, notes) = registered_notes(plan);
            assert!(
                !crate::auth::codex::MODELS.is_empty(),
                "the fallback Codex inventory is registered"
            );
            for model_id in crate::auth::codex::MODELS {
                assert!(
                    reduced.contains(model_id) || plain.contains(model_id),
                    "{plan}: {model_id} is not covered by the expectation table"
                );
                let id = catalog_id(&catalog, model_id);
                let note = notes.note_for(&id);
                if reduced.contains(model_id) {
                    let note = note.unwrap_or_else(|| {
                        panic!("{plan}: {model_id} is reduced and needs a note")
                    });
                    assert!(note.starts_with("note: Codex model"), "{note}");
                    assert!(!note.contains("Session::"), "{note}");
                    assert!(!note.contains("record_usage_uncertainty"), "{note}");
                    assert!(
                        !note.contains(crate::codex_context::CODEX_ABOVE_STANDARD_TIER_OPERATION),
                        "{note}"
                    );
                    // Exactly one note per model: the lookup is stable.
                    assert_eq!(notes.note_for(&id), Some(note));
                } else {
                    assert_eq!(
                        note, None,
                        "{plan}: {model_id} is not reduced and needs no note"
                    );
                }
            }
        }
    }

    /// A session whose effective model is not a Codex route prints nothing, even
    /// though the catalog is full of Codex models.
    #[test]
    fn a_non_codex_effective_model_has_no_note_while_the_catalog_has_codex_models() {
        let (catalog, notes) = registered_notes("plus");
        let codex_endpoint = EndpointId(crate::auth::codex::ENDPOINT_ID.to_owned());
        assert!(
            catalog
                .models()
                .any(|model| model.endpoint == codex_endpoint),
            "the catalog carries Codex models"
        );

        let mut non_codex = catalog
            .models()
            .filter(|model| model.endpoint != codex_endpoint)
            .map(|model| model.id.clone())
            .collect::<Vec<_>>();
        non_codex.sort_by(|left, right| left.0.cmp(&right.0));
        assert!(!non_codex.is_empty(), "the catalog carries other providers");
        for id in &non_codex {
            assert_eq!(
                notes.note_for(id),
                None,
                "{} is not a Codex route and must print no Codex note",
                id.0
            );
        }

        // `Bootstrap::codex_context_note` is exactly this lookup, so the
        // effective-model boundary returns one note for a reduced Codex route
        // and nothing for any other model. The process-boundary test in
        // `tests/parity_cli.rs` asserts the same through the real CLI.
        let effective = catalog_id(&catalog, "gpt-5.6-sol");
        let note = notes
            .note_for(&effective)
            .expect("a reduced Codex model has one note")
            .to_owned();
        assert_eq!(notes.note_for(&effective), Some(note.as_str()));
    }

    /// Above the 272K standard tier the session is durably marked uncertain
    /// before its first request, so no exact-looking cost is ever claimed. The
    /// record is sticky and written at most once; a route at or below the
    /// standard tier stays exact.
    #[test]
    fn an_above_standard_tier_route_marks_the_session_uncertain_once() {
        let (catalog, notes) = registered_notes("plus");
        let directory = tempfile::tempdir().unwrap();

        // `gpt-5.6-luna`'s documented 372K working window is above the tier.
        let luna_id = catalog_id(&catalog, "gpt-5.6-luna");
        let luna = catalog.resolve(&luna_id).unwrap();
        assert!(
            notes.note_for(&luna_id).is_some(),
            "an above-tier route carries the one session note"
        );
        assert_eq!(
            codex_context_uncertainty_operation(&luna),
            Some(crate::codex_context::CODEX_ABOVE_STANDARD_TIER_OPERATION)
        );
        let mut session = Session::create(directory.path().join("luna.jsonl")).unwrap();
        assert!(!session.has_uncertain_usage());
        record_codex_context_uncertainty(&mut session, &luna).unwrap();
        assert!(
            session.has_uncertain_usage(),
            "372K must route cost/usage through has_uncertain_usage"
        );
        let records = session.usage_uncertainty_records().to_vec();
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(
            records[0].operation,
            crate::codex_context::CODEX_ABOVE_STANDARD_TIER_OPERATION
        );
        assert_eq!(records[0].endpoint.0, crate::auth::codex::ENDPOINT_ID);
        assert_eq!(records[0].model, luna_id);

        // Sticky: a second launch of the same session appends nothing.
        record_codex_context_uncertainty(&mut session, &luna).unwrap();
        assert_eq!(session.usage_uncertainty_records().len(), 1);

        // A route whose effective window is the deliberate 272K cap is exact.
        let capped_id = catalog_id(&catalog, "gpt-5.5");
        let capped = catalog.resolve(&capped_id).unwrap();
        assert_eq!(codex_context_uncertainty_operation(&capped), None);
        let mut exact = Session::create(directory.path().join("exact.jsonl")).unwrap();
        record_codex_context_uncertainty(&mut exact, &capped).unwrap();
        assert!(!exact.has_uncertain_usage());
        assert!(exact.usage_uncertainty_records().is_empty());

        // A non-Codex route is never marked uncertain, even when its context
        // window is far above 272K: the cap is a Codex policy, not a global one.
        let non_codex_id = catalog
            .models()
            .find(|model| model.endpoint != EndpointId(crate::auth::codex::ENDPOINT_ID.to_owned()))
            .map(|model| model.id.clone())
            .expect("the catalog carries other providers");
        let non_codex = catalog.resolve(&non_codex_id).unwrap();
        assert_eq!(codex_context_uncertainty_operation(&non_codex), None);
        record_codex_context_uncertainty(&mut exact, &non_codex).unwrap();
        assert!(!exact.has_uncertain_usage());
    }

    /// The note the effective model needs is produced from the same resolution
    /// every route in the catalog is recorded from.
    #[test]
    fn the_recorded_note_matches_the_effective_resolution() {
        let luna = codex_discovered_model(
            "gpt-5.6-luna",
            crate::codex_context::CODEX_5_6_CONTEXT_WINDOW,
            crate::codex_context::CODEX_5_6_CONTEXT_WINDOW,
            128_000,
        );
        let resolution = codex_context_resolve_for_registration(
            &luna,
            CodexContextTier::Default,
            CodexContextOverride::NONE,
        );
        let note = codex_context_session_note("gpt-5.6-luna", &resolution).unwrap();
        assert!(note.contains("effective 372K"), "{note}");
        let mut notes = CodexContextNotes::default();
        codex_context_record_note(
            &mut notes,
            &ModelId("gpt-5.6-luna".to_owned()),
            "gpt-5.6-luna",
            &resolution,
        );
        assert_eq!(
            notes.note_for(&ModelId("gpt-5.6-luna".to_owned())),
            Some(note.as_str())
        );
    }

    /// The note is delivered lazily and at most once per session, and a route
    /// that needs no note never consumes the delivery.
    #[test]
    fn the_codex_context_note_is_delivered_lazily_and_at_most_once() {
        let (catalog, notes) = registered_notes("plus");
        let effective = catalog_id(&catalog, "gpt-5.6-sol");
        let expected = notes
            .note_for(&effective)
            .expect("a reduced Codex route has one note")
            .to_owned();

        // A model with no note never consumes the once-per-session delivery, so
        // a non-Codex first turn cannot swallow the Codex route's note.
        let other = catalog
            .models()
            .map(|model| model.id.clone())
            .find(|id| notes.note_for(id).is_none())
            .expect("the catalog carries non-Codex models");
        assert_eq!(notes.take_for(&other), None);
        assert_eq!(notes.note_for(&effective), Some(expected.as_str()));

        // The first pull delivers exactly the recorded wording, still without any
        // internal API name or operation id, and still naming the deliberate cap.
        let delivered = notes
            .take_for(&effective)
            .expect("the first pull delivers the note");
        assert_eq!(delivered, expected);
        assert!(delivered.starts_with("note: Codex model"), "{delivered}");
        assert!(!delivered.contains("Session::"), "{delivered}");
        assert!(
            !delivered.contains(crate::codex_context::CODEX_ABOVE_STANDARD_TIER_OPERATION),
            "{delivered}"
        );
        assert!(delivered.contains("deliberate"), "{delivered}");
        assert!(delivered.contains("double-priced"), "{delivered}");

        // Startup showed nothing, and every later pull stays silent: the note can
        // never be repeated in one session, and the peek is still available to
        // diagnostics.
        assert_eq!(notes.take_for(&effective), None);
        assert_eq!(notes.take_for(&effective), None);
        assert_eq!(notes.note_for(&effective), Some(expected.as_str()));
    }

    /// The startup phase trace is off by default, and its gate plus line format
    /// are stable for the latency acceptance harness.
    #[test]
    fn the_startup_phase_trace_is_off_by_default_and_named_consistently() {
        assert!(!startup_trace_enabled(None));
        assert!(!startup_trace_enabled(Some(std::ffi::OsStr::new(""))));
        assert!(!startup_trace_enabled(Some(std::ffi::OsStr::new("0"))));
        assert!(startup_trace_enabled(Some(std::ffi::OsStr::new("1"))));
        let line = startup_phase_line("catalog.codex", std::time::Duration::from_micros(1_250));
        assert_eq!(line, "octet-startup: catalog.codex elapsed=1250us");
        assert!(!line.contains('\n'));
    }
}

#[cfg(test)]
#[path = "../providers/self_description_tests.rs"]
mod provider_self_description_tests;

#[cfg(test)]
#[path = "../providers/conditional_inventory_tests.rs"]
mod provider_conditional_inventory_tests;
