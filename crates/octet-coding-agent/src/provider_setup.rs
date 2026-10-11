#![allow(missing_docs)]

//! Frontend-neutral setup for explicitly selected providers.
//!
//! This module deliberately owns no terminal state and constructs no Agent. A
//! frontend gathers a draft, asks this service to probe only that draft's
//! endpoint, presents the receipt, and commits only after an explicit final
//! confirmation. Custom endpoints use the existing custom registry; built-in
//! API keys use a separate private credential record without redefining their
//! native routes. `ModelCatalog` remains the sole runtime catalog.

use std::fmt;
use std::io::Read;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use octet_ai::{ModelCatalog, ModelId};

use crate::auth::custom::{
    CredentialStore, CustomAuthConfig, CustomCredential, CustomModel, CustomNativeMetadata,
    CustomProvider, CustomRegistry, HeaderEntry, RegistryCommitError, RegistrySnapshot,
};

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_DISCOVERY_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_DISCOVERED_MODELS: usize = 512;
const MAX_PROVIDER_ID_BYTES: usize = 64;
const MAX_LABEL_BYTES: usize = 128;
const MAX_MODEL_ID_BYTES: usize = 256;
const MAX_ENDPOINT_BYTES: usize = 512;

/// A built-in route that can be configured with just an API key. Providers
/// requiring account/region/deployment setup are deliberately not offered by
/// this one-field flow. Values here are declaration metadata, never secrets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ApiKeyProvider {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) credential_variable: &'static str,
}

pub(crate) fn builtin_api_key_providers() -> Vec<ApiKeyProvider> {
    crate::providers::BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .filter(|declaration| {
            declaration.base_url_environment.is_empty()
                && matches!(
                    declaration.runtime_configuration,
                    crate::providers::ProviderRuntimeConfiguration::Default
                )
        })
        .filter_map(|declaration| {
            let variable = declaration
                .authentication
                .environment_variables()?
                .iter()
                .copied()
                .find(|variable| !octet_ai::ANTHROPIC_BEARER_TOKEN_VARIABLES.contains(variable))?;
            Some(ApiKeyProvider {
                id: declaration.id,
                label: declaration.name,
                credential_variable: variable,
            })
        })
        .collect()
}

/// Owner-private, recoverable API keys for native built-in routes. The existing
/// custom registry cannot represent native Messages/Google/Responses presets;
/// reuse its secure filesystem primitives, not an OpenAI-compatible surrogate.
/// No keys are put in process environment, config, receipts, or model metadata.
#[derive(Clone, Debug)]
pub(crate) struct BuiltinApiKeyStore {
    directory: PathBuf,
}

const MAX_API_KEY_FILE_BYTES: usize = 8 * 1024;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiKeyFile {
    version: u8,
    api_key: String,
}

impl Drop for ApiKeyFile {
    fn drop(&mut self) {
        // Take ownership of the allocation rather than leave the original
        // String's bytes behind while replacing it.
        let mut bytes = std::mem::take(&mut self.api_key).into_bytes();
        bytes.fill(0);
    }
}

impl BuiltinApiKeyStore {
    #[cfg(test)]
    pub(crate) fn for_test(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub(crate) fn default_store() -> Result<Self, ProviderSetupError> {
        let home = dirs::home_dir()
            .filter(|home| home.is_absolute())
            .ok_or(ProviderSetupError::Storage)?;
        Ok(Self {
            directory: home.join(".octet/credentials/api-keys"),
        })
    }

    pub(crate) fn path(&self, provider_id: &str) -> Result<PathBuf, ProviderSetupError> {
        // Resolve only declaration-owned IDs before constructing a filename.
        if !builtin_api_key_providers()
            .iter()
            .any(|provider| provider.id == provider_id)
        {
            return Err(ProviderSetupError::InvalidConfiguration);
        }
        Ok(self.directory.join(format!("{provider_id}.json")))
    }

    /// Call only after explicit confirmation. No discovery or inference occurs.
    /// Replacement requires a separate explicit choice and uses the same private
    /// compare-and-swap writer as custom-provider setup.
    pub(crate) fn save(
        &self,
        provider_id: &str,
        value: String,
        replace_existing: bool,
    ) -> Result<PathBuf, ProviderSetupError> {
        let input = SecretInput::new(value);
        let value = input
            .as_str()
            .ok_or(ProviderSetupError::InvalidConfiguration)?
            .trim();
        if !valid_api_key(value) {
            return Err(ProviderSetupError::InvalidConfiguration);
        }
        let path = self.path(provider_id)?;
        let previous = crate::auth::read_bounded_private(&path, MAX_API_KEY_FILE_BYTES)
            .map_err(|_| ProviderSetupError::Storage)?;
        if previous.is_some() && !replace_existing {
            return Err(ProviderSetupError::State(
                ProviderSetupState::ProviderAlreadyConfigured,
            ));
        }
        let credential = ApiKeyFile {
            version: 1,
            api_key: value.to_owned(),
        };
        let mut bytes = serde_json::to_vec(&credential).map_err(|_| ProviderSetupError::Storage)?;
        let result = octet_agent::secure_fs::write_private_atomic_if_unchanged(
            &path,
            previous.as_deref(),
            &bytes,
            MAX_API_KEY_FILE_BYTES,
        );
        bytes.fill(0);
        result.map_err(|_| ProviderSetupError::Storage)?;
        Ok(path)
    }

    /// Private runtime lookup; callers must never place the result in a public
    /// provider definition. Environment precedence is decided by provider auth.
    pub(crate) fn load(&self, provider_id: &str) -> Result<Option<String>, ProviderSetupError> {
        let path = self.path(provider_id)?;
        let Some(mut bytes) = crate::auth::read_bounded_private(&path, MAX_API_KEY_FILE_BYTES)
            .map_err(|_| ProviderSetupError::Storage)?
        else {
            return Ok(None);
        };
        let parsed = serde_json::from_slice::<ApiKeyFile>(&bytes);
        bytes.fill(0);
        let mut credential = parsed.map_err(|_| ProviderSetupError::Storage)?;
        if credential.version != 1 || !valid_api_key(&credential.api_key) {
            return Err(ProviderSetupError::Storage);
        }
        Ok(Some(std::mem::take(&mut credential.api_key)))
    }
}

fn valid_api_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= octet_ai::auth::MAX_ENV_VALUE_BYTES
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}

/// A credential source used only while setting up one explicitly selected
/// provider. Its secret-bearing variant never implements a revealing `Debug`.
pub(crate) enum SetupAuthentication {
    None,
    Environment { variable: String },
    ApiKey(SecretInput),
    PreserveExisting,
    Preserved(ExistingSetupAuthentication),
}

/// Secret-bearing snapshot of an existing provider's authentication settings.
/// Its Debug implementation is intentionally unavailable; the containing setup
/// types redact it at their own boundary.
pub(crate) struct ExistingSetupAuthentication {
    api_key: String,
    headers: Vec<HeaderEntry>,
    auth: Option<CustomAuthConfig>,
    api_key_env: Option<String>,
    legacy_single_endpoint: bool,
}

struct StoredSetupAuthentication {
    api_key: String,
    headers: Vec<HeaderEntry>,
    auth: Option<CustomAuthConfig>,
    api_key_env: Option<String>,
}

impl Drop for ExistingSetupAuthentication {
    fn drop(&mut self) {
        let mut key = std::mem::take(&mut self.api_key).into_bytes();
        key.fill(0);
        for header in &mut self.headers {
            let mut value = std::mem::take(&mut header.value).into_bytes();
            value.fill(0);
        }
    }
}

impl SetupAuthentication {
    pub(crate) fn no_authentication() -> Self {
        Self::None
    }

    pub(crate) fn environment(variable: impl Into<String>) -> Self {
        Self::Environment {
            variable: variable.into(),
        }
    }

    pub(crate) fn api_key(value: String) -> Self {
        Self::ApiKey(SecretInput::new(value))
    }

    pub(crate) fn preserve_existing() -> Self {
        Self::PreserveExisting
    }

    fn validate(&self) -> Result<(), ProviderSetupError> {
        match self {
            Self::Environment { variable } if valid_environment_name(variable) => Ok(()),
            Self::Environment { .. } => Err(ProviderSetupError::InvalidConfiguration),
            Self::ApiKey(value) if value.is_empty() => {
                Err(ProviderSetupError::InvalidConfiguration)
            }
            Self::Preserved(_) => Err(ProviderSetupError::InvalidConfiguration),
            Self::None | Self::ApiKey(_) | Self::PreserveExisting => Ok(()),
        }
    }

    fn authorization_headers(&self) -> Result<http::HeaderMap, ProbeFailure> {
        let mut headers = http::HeaderMap::new();
        let bearer = match self {
            Self::None => None,
            Self::PreserveExisting => return Err(ProbeFailure::AuthenticationFailed),
            Self::Environment { variable } => Some(
                octet_ai::auth::read_bounded_env(variable)
                    .ok()
                    .flatten()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or(ProbeFailure::AuthenticationFailed)?,
            ),
            Self::ApiKey(value) => Some(
                value
                    .as_str()
                    .ok_or(ProbeFailure::AuthenticationFailed)?
                    .to_owned(),
            ),
            Self::Preserved(existing) => {
                for entry in &existing.headers {
                    let name = http::HeaderName::from_bytes(entry.name.as_bytes())
                        .map_err(|_| ProbeFailure::AuthenticationFailed)?;
                    let mut value = http::HeaderValue::from_str(&entry.value)
                        .map_err(|_| ProbeFailure::AuthenticationFailed)?;
                    value.set_sensitive(true);
                    headers.insert(name, value);
                }
                let environment = |variable: &str| {
                    octet_ai::auth::read_bounded_env(variable)
                        .ok()
                        .flatten()
                        .filter(|value| !value.trim().is_empty())
                        .ok_or(ProbeFailure::AuthenticationFailed)
                };
                match &existing.auth {
                    Some(CustomAuthConfig::None) => None,
                    Some(CustomAuthConfig::BearerEnv { var }) => Some(environment(var)?),
                    None => match existing.api_key_env.as_deref() {
                        Some(variable) => Some(environment(variable)?),
                        None if !existing.api_key.is_empty() => Some(existing.api_key.clone()),
                        None if existing.legacy_single_endpoint => {
                            octet_ai::auth::read_bounded_env("OCTET_CUSTOM_API_KEY")
                                .ok()
                                .flatten()
                                .filter(|value| !value.trim().is_empty())
                        }
                        None => None,
                    },
                }
            }
        };
        if let Some(value) = bearer {
            let mut header = http::HeaderValue::from_str(&format!("Bearer {value}"))
                .map_err(|_| ProbeFailure::AuthenticationFailed)?;
            header.set_sensitive(true);
            headers.insert(http::header::AUTHORIZATION, header);
        }
        Ok(headers)
    }

    fn receipt_description(&self) -> String {
        match self {
            Self::None => "no credential is sent or stored".to_owned(),
            Self::PreserveExisting | Self::Preserved(_) => {
                "existing credential source is preserved".to_owned()
            }
            Self::Environment { variable } => format!(
                "Bearer credential is read from {variable} at runtime; its value is not stored"
            ),
            Self::ApiKey(_) => {
                "Bearer credential is stored only in the owner-private custom registry".to_owned()
            }
        }
    }

    fn into_storage(self) -> StoredSetupAuthentication {
        match self {
            Self::None => StoredSetupAuthentication {
                api_key: String::new(),
                headers: Vec::new(),
                auth: Some(CustomAuthConfig::None),
                api_key_env: None,
            },
            Self::PreserveExisting => StoredSetupAuthentication {
                api_key: String::new(),
                headers: Vec::new(),
                auth: None,
                api_key_env: None,
            },
            Self::Environment { variable } => StoredSetupAuthentication {
                api_key: String::new(),
                headers: Vec::new(),
                auth: Some(CustomAuthConfig::BearerEnv { var: variable }),
                api_key_env: None,
            },
            Self::ApiKey(value) => StoredSetupAuthentication {
                api_key: value.into_string(),
                headers: Vec::new(),
                auth: None,
                api_key_env: None,
            },
            Self::Preserved(mut existing) => {
                let legacy_environment_fallback = existing.legacy_single_endpoint
                    && existing.auth.is_none()
                    && existing.api_key_env.is_none()
                    && existing.api_key.is_empty();
                StoredSetupAuthentication {
                    api_key: std::mem::take(&mut existing.api_key),
                    headers: std::mem::take(&mut existing.headers),
                    auth: if legacy_environment_fallback {
                        Some(CustomAuthConfig::BearerEnv {
                            var: "OCTET_CUSTOM_API_KEY".to_owned(),
                        })
                    } else {
                        existing.auth.take()
                    },
                    api_key_env: existing.api_key_env.take(),
                }
            }
        }
    }
}

impl fmt::Debug for SetupAuthentication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("SetupAuthentication::None"),
            Self::Environment { variable } => formatter
                .debug_struct("SetupAuthentication::Environment")
                .field("variable", variable)
                .finish(),
            Self::ApiKey(_) => formatter.write_str("SetupAuthentication::ApiKey([REDACTED])"),
            Self::PreserveExisting => formatter.write_str("SetupAuthentication::PreserveExisting"),
            Self::Preserved(_) => formatter.write_str("SetupAuthentication::Preserved([REDACTED])"),
        }
    }
}

/// Bounded secret material that is zeroed when the setup draft is dropped.
pub(crate) struct SecretInput(Vec<u8>);

impl SecretInput {
    fn new(value: String) -> Self {
        Self(value.into_bytes())
    }

    fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.0).ok()
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn into_string(mut self) -> String {
        let bytes = std::mem::take(&mut self.0);
        String::from_utf8(bytes).unwrap_or_default()
    }
}

impl fmt::Debug for SecretInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

impl Drop for SecretInput {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

/// The metadata routes explicitly selected for setup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DiscoveryProfile {
    OpenAiCompatible,
    LmStudioNative,
}

/// User-provided setup values before any network traffic or persistence.
pub(crate) struct SetupDraft {
    provider_id: String,
    label: String,
    endpoint: String,
    authentication: SetupAuthentication,
    replace_existing: bool,
    edit_existing: bool,
    allow_credential_reuse_at_new_endpoint: bool,
    discovery_profile: Option<DiscoveryProfile>,
}

impl SetupDraft {
    pub(crate) fn new(
        provider_id: impl Into<String>,
        label: impl Into<String>,
        endpoint: impl Into<String>,
        authentication: SetupAuthentication,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            label: label.into(),
            endpoint: endpoint.into(),
            authentication,
            replace_existing: false,
            edit_existing: false,
            allow_credential_reuse_at_new_endpoint: false,
            discovery_profile: None,
        }
    }

    pub(crate) fn edit_existing(
        provider_id: impl Into<String>,
        label: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Self {
        let mut draft = Self::new(
            provider_id,
            label,
            endpoint,
            SetupAuthentication::preserve_existing(),
        );
        draft.edit_existing = true;
        draft.replace_existing = true;
        draft
    }

    pub(crate) fn lm_studio() -> Self {
        let mut draft = Self::new(
            "local",
            "LM Studio",
            "http://localhost:1234/v1/",
            SetupAuthentication::None,
        );
        draft.discovery_profile = Some(DiscoveryProfile::LmStudioNative);
        draft
    }

    pub(crate) fn with_authentication(mut self, authentication: SetupAuthentication) -> Self {
        self.authentication = authentication;
        self
    }

    pub(crate) fn with_discovery_profile(mut self, profile: DiscoveryProfile) -> Self {
        self.discovery_profile = Some(profile);
        self
    }

    pub(crate) fn allow_credential_reuse_at_new_endpoint(mut self, allow: bool) -> Self {
        self.allow_credential_reuse_at_new_endpoint = allow;
        self
    }

    pub(crate) fn replace_existing(mut self, replace_existing: bool) -> Self {
        self.replace_existing = replace_existing;
        self
    }
}

impl fmt::Debug for SetupDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SetupDraft")
            .field("provider_id", &self.provider_id)
            .field("label", &self.label)
            .field("endpoint", &self.endpoint)
            .field("authentication", &self.authentication)
            .field("replace_existing", &self.replace_existing)
            .field("edit_existing", &self.edit_existing)
            .field(
                "allow_credential_reuse_at_new_endpoint",
                &self.allow_credential_reuse_at_new_endpoint,
            )
            .field("discovery_profile", &self.discovery_profile)
            .finish()
    }
}

struct ValidatedDraft {
    provider_id: String,
    label: String,
    endpoint: url::Url,
    authentication: SetupAuthentication,
    replace_existing: bool,
    edit_existing: bool,
    allow_credential_reuse_at_new_endpoint: bool,
    discovery_profile: Option<DiscoveryProfile>,
}

impl ValidatedDraft {
    fn endpoint_string(&self) -> String {
        self.endpoint.to_string()
    }
}

/// Credential-source summary safe for interactive display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SetupCredentialSummary {
    None,
    Environment { variable: String },
    StoredCredentialConfigured,
}

/// Secret-free summary of one configured provider suitable for edit selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExistingProviderSummary {
    pub(crate) provider_id: String,
    pub(crate) label: String,
    pub(crate) endpoint: String,
    pub(crate) authentication: SetupCredentialSummary,
    pub(crate) discovery_profile: DiscoveryProfile,
    pub(crate) models: Vec<SetupModel>,
}

/// One discovered/configured model and the metadata provenance needed by setup UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SetupModel {
    pub(crate) api_name: String,
    pub(crate) display_name: String,
    /// Endpoint-asserted effective context, or `None` when unknown/unasserted.
    pub(crate) context_window: Option<u64>,
    /// Explicitly configured operational context, distinct from server metadata.
    pub(crate) configured_context_window: Option<u64>,
    pub(crate) context_window_asserted: bool,
    /// True when neither endpoint metadata nor an explicit saved pin establishes
    /// a usable effective context for catalog registration.
    pub(crate) requires_context_configuration: bool,
    pub(crate) max_output_tokens: Option<u64>,
    pub(crate) stored_max_output_tokens: u64,
    pub(crate) max_output_tokens_asserted: bool,
    /// Route-level reasoning support is populated only for an explicit Chat-route
    /// assertion. Native metadata is deliberately carried separately.
    pub(crate) reasoning_supported: Option<bool>,
    pub(crate) reasoning_source: Option<octet_ai::types::ReasoningMetadataSource>,
    pub(crate) reasoning_values: Vec<String>,
    pub(crate) reasoning_default: Option<String>,
    pub(crate) reasoning_profile: Option<octet_ai::OpenAiChatReasoningMode>,
    pub(crate) native_metadata: Option<CustomNativeMetadata>,
}

/// A no-write setup transaction. Dropping or cancelling it cannot affect disk.
pub(crate) struct SetupTransaction {
    snapshot: RegistrySnapshot,
    registry_path: PathBuf,
    draft: ValidatedDraft,
    discovered: Vec<CustomModel>,
}

impl SetupTransaction {
    pub(crate) fn cancel(self) -> ProviderSetupState {
        ProviderSetupState::Cancelled
    }
}

impl fmt::Debug for SetupTransaction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SetupTransaction")
            .field("provider_id", &self.draft.provider_id)
            .field("endpoint", &self.draft.endpoint_string())
            .field("discovered_models", &self.discovered.len())
            .finish()
    }
}

/// A prepared, reviewed setup transaction. It has not written any state yet.
pub(crate) struct SetupPrepared {
    snapshot: RegistrySnapshot,
    draft: ValidatedDraft,
    models: Vec<CustomModel>,
    selected: CustomModel,
    receipt: SetupReceipt,
}

impl SetupPrepared {
    pub(crate) fn receipt(&self) -> &SetupReceipt {
        &self.receipt
    }

    pub(crate) fn cancel(self) -> ProviderSetupState {
        ProviderSetupState::Cancelled
    }
}

impl fmt::Debug for SetupPrepared {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SetupPrepared")
            .field("provider_id", &self.draft.provider_id)
            .field("endpoint", &self.draft.endpoint_string())
            .field("models", &self.models.len())
            .field("selected", &self.selected.api_name)
            .field("receipt", &self.receipt)
            .finish()
    }
}

/// Stable, secret-free result of a successful setup operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SetupReceipt {
    provider_id: String,
    provider_label: String,
    endpoint: String,
    endpoint_kind: EndpointKind,
    selected_model: String,
    context_window: u64,
    context_source: ContextWindowSource,
    credential_policy: String,
    registry_path: PathBuf,
}

impl SetupReceipt {
    pub(crate) fn selected_model_id(&self) -> ModelId {
        ModelId(format!(
            "custom/{}/{}",
            self.provider_id, self.selected_model
        ))
    }

    pub(crate) fn render(&self, authority: Option<&SetupAuthority>) -> String {
        let locality = match self.endpoint_kind {
            EndpointKind::Local => "local endpoint",
            EndpointKind::Network => "network endpoint",
        };
        let mut lines = vec![
            "Provider setup ready".to_owned(),
            format!("provider: {} ({})", self.provider_label, self.provider_id),
            format!("model: {}", self.selected_model_id().0),
            format!(
                "effective context window: {} tokens ({})",
                self.context_window,
                match self.context_source {
                    ContextWindowSource::DiscoveredEffective => "reported for the active endpoint",
                    ContextWindowSource::ExplicitConfiguration => "explicitly configured",
                }
            ),
            format!("endpoint: {} ({locality})", self.endpoint),
            "traffic: direct only from this octet process to the selected endpoint; no endpoint scanning or setup telemetry".to_owned(),
            format!("credentials: {}", self.credential_policy),
            format!(
                "registry storage: owner-private atomic file at {}",
                self.registry_path.display()
            ),
        ];
        if let Some(authority) = authority {
            lines.extend(authority.render_lines());
        }
        lines.join("\n")
    }
}

/// The caller-owned authority facts shown alongside an otherwise
/// frontend-neutral receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SetupAuthority {
    session_dir: String,
    workspace_trusted: bool,
    enabled_tools: String,
}

impl SetupAuthority {
    pub(crate) fn from_config(config: &crate::config::Config) -> Self {
        let tools = ["read", "edit", "write", "bash"]
            .into_iter()
            .filter(|name| config.tool_available(name))
            .collect::<Vec<_>>();
        Self {
            session_dir: bounded_copy(
                &config.session_dir.display().to_string(),
                MAX_ENDPOINT_BYTES,
            ),
            workspace_trusted: config.workspace_trusted,
            enabled_tools: if tools.is_empty() {
                "none of read, edit, write, or bash".to_owned()
            } else {
                tools.join(", ")
            },
        }
    }

    fn render_lines(&self) -> Vec<String> {
        vec![
            format!("session storage: local files at {}", self.session_dir),
            format!(
                "workspace trust: {}",
                if self.workspace_trusted {
                    "trusted"
                } else {
                    "not trusted"
                }
            ),
            format!("tool authority: {}", self.enabled_tools),
            "OS isolation: none; enabled tools retain their normal host authority".to_owned(),
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContextWindowSource {
    DiscoveredEffective,
    ExplicitConfiguration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EndpointKind {
    Local,
    Network,
}

/// Recoverable readiness and setup states. They contain no endpoint response,
/// header, or credential value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProviderSetupState {
    NoProvider,
    EndpointUnreachable,
    AuthenticationFailed,
    EmptyDiscovery,
    ManualModelRequired,
    ContextWindowRequired,
    UnavailableResumedModel { model: String },
    Offline,
    ConcurrentRegistry,
    Cancelled,
    ProviderAlreadyConfigured,
    ProviderNotConfigured,
    CredentialReuseRequiresConfirmation,
    Ready,
}

impl fmt::Display for ProviderSetupState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoProvider => formatter.write_str(
                "no provider is configured; run `octet setup --yes` or start interactive octet",
            ),
            Self::EndpointUnreachable => formatter.write_str(
                "the selected endpoint could not be reached; retry, edit it, or enter a model manually",
            ),
            Self::AuthenticationFailed => formatter.write_str(
                "the selected endpoint rejected authentication; change the credential source and retry",
            ),
            Self::EmptyDiscovery => formatter.write_str(
                "model discovery returned no usable models; enter an explicit model ID",
            ),
            Self::ManualModelRequired => formatter.write_str(
                "the requested model was not discovered; enter it with --manual-model",
            ),
            Self::ContextWindowRequired => formatter.write_str(
                "the selected model has no known effective context; enter an explicit context limit before saving",
            ),
            Self::UnavailableResumedModel { model } => {
                write!(formatter, "the resumed model {model:?} is unavailable; select a replacement or cancel resume")
            }
            Self::Offline => formatter.write_str(
                "setup discovery is disabled by offline mode; provide --manual-model or retry online",
            ),
            Self::ConcurrentRegistry => formatter.write_str(
                "the custom provider registry changed during setup; reload and merge deliberately",
            ),
            Self::Cancelled => formatter.write_str("setup cancelled; no provider state was written"),
            Self::ProviderAlreadyConfigured => formatter.write_str(
                "that provider ID is already configured; choose another ID or explicitly replace it",
            ),
            Self::ProviderNotConfigured => formatter.write_str(
                "the selected provider configuration no longer exists; reload setup choices",
            ),
            Self::CredentialReuseRequiresConfirmation => formatter.write_str(
                "the endpoint changed; confirm credential reuse at the new endpoint or choose a replacement source",
            ),
            Self::Ready => formatter.write_str("provider setup is ready"),
        }
    }
}

/// Safe failure boundary for a setup operation.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ProviderSetupError {
    #[error("{0}")]
    State(ProviderSetupState),
    #[error("provider setup configuration is invalid")]
    InvalidConfiguration,
    #[error("could not read or save the provider credential store")]
    Storage,
    #[error("the rebuilt model catalog did not expose the selected model")]
    CatalogUnavailable,
}

impl ProviderSetupError {
    pub(crate) fn state(&self) -> Option<&ProviderSetupState> {
        match self {
            Self::State(state) => Some(state),
            Self::InvalidConfiguration | Self::Storage | Self::CatalogUnavailable => None,
        }
    }
}

/// An intentionally narrow network-probe contract. Test fixtures can replace
/// it without exposing credentials or constructing an Agent.
pub(crate) trait SetupProbe {
    fn discover(
        &self,
        endpoint: &url::Url,
        authentication: &SetupAuthentication,
        profile: DiscoveryProfile,
    ) -> Result<Vec<CustomModel>, ProbeFailure>;
}

/// Secret-free classification of an explicit endpoint probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProbeFailure {
    Unreachable,
    AuthenticationFailed,
    InvalidDiscovery,
}

/// The production probe for one selected OpenAI-compatible `/models` URL.
#[derive(Clone, Debug)]
pub(crate) struct HttpSetupProbe {
    timeout: Duration,
}

impl Default for HttpSetupProbe {
    fn default() -> Self {
        Self {
            timeout: DISCOVERY_TIMEOUT,
        }
    }
}

fn setup_model_summary(model: &CustomModel) -> Option<SetupModel> {
    if !valid_model_id(model.api_name.trim()) {
        return None;
    }
    let api_name = bounded_copy(model.api_name.trim(), MAX_MODEL_ID_BYTES);
    let display_name = if valid_display_text(model.display_name.trim(), MAX_MODEL_ID_BYTES) {
        bounded_copy(model.display_name.trim(), MAX_MODEL_ID_BYTES)
    } else {
        api_name.clone()
    };
    let reasoning_values = octet_ai::types::ReasoningOptions {
        values: model.reasoning_values.clone(),
        default: (!model.reasoning_default.is_empty()).then(|| model.reasoning_default.clone()),
    };
    let valid_reasoning_options = reasoning_values.is_valid()
        && reasoning_values.values.iter().all(|value| {
            value.len() <= 32
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        });
    let discovered_context = model
        .discovered_context_window
        .filter(|context| *context > 0)
        .or_else(|| {
            model
                .context_window_asserted
                .then_some(model.context_window)
                .filter(|context| *context > 0)
        });
    let configured_context = (model.context_window_configured && model.context_window > 0)
        .then_some(model.context_window);
    let effective_context = if model.context_window_asserted {
        discovered_context
    } else {
        None
    };
    Some(SetupModel {
        api_name,
        display_name,
        context_window: effective_context,
        configured_context_window: configured_context,
        context_window_asserted: model.context_window_asserted,
        requires_context_configuration: configured_context.is_none() && effective_context.is_none(),
        max_output_tokens: model
            .max_output_tokens_asserted
            .then_some(model.max_output_tokens),
        stored_max_output_tokens: model.max_output_tokens,
        max_output_tokens_asserted: model.max_output_tokens_asserted,
        reasoning_supported: (model.reasoning_source
            == Some(octet_ai::types::ReasoningMetadataSource::Explicit))
        .then_some(model.reasoning),
        reasoning_source: model.reasoning_source,
        reasoning_values: if valid_reasoning_options {
            reasoning_values.values
        } else {
            Vec::new()
        },
        reasoning_default: valid_reasoning_options
            .then_some(reasoning_values.default)
            .flatten(),
        reasoning_profile: model.reasoning_profile.clone(),
        native_metadata: model.native_metadata.clone(),
    })
}

fn sanitized_endpoint(value: &str) -> String {
    let Ok(mut endpoint) = url::Url::parse(value) else {
        return "<invalid endpoint>".to_owned();
    };
    let _ = endpoint.set_username("");
    let _ = endpoint.set_password(None);
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    bounded_copy(endpoint.as_str(), MAX_ENDPOINT_BYTES)
}

fn setup_credential_summary(
    provider: &CustomProvider,
    legacy_single_endpoint: bool,
) -> SetupCredentialSummary {
    if let Some(auth) = &provider.auth {
        return match auth {
            CustomAuthConfig::None if provider.credential.headers.is_empty() => {
                SetupCredentialSummary::None
            }
            CustomAuthConfig::None => SetupCredentialSummary::StoredCredentialConfigured,
            CustomAuthConfig::BearerEnv { var } => SetupCredentialSummary::Environment {
                variable: bounded_copy(var, 128),
            },
        };
    }
    if let Some(variable) = provider.api_key_env.as_deref() {
        return SetupCredentialSummary::Environment {
            variable: bounded_copy(variable, 128),
        };
    }
    if !provider.credential.api_key.is_empty() || !provider.credential.headers.is_empty() {
        return SetupCredentialSummary::StoredCredentialConfigured;
    }
    if legacy_single_endpoint {
        SetupCredentialSummary::Environment {
            variable: "OCTET_CUSTOM_API_KEY".to_owned(),
        }
    } else {
        SetupCredentialSummary::None
    }
}

impl SetupProbe for HttpSetupProbe {
    fn discover(
        &self,
        endpoint: &url::Url,
        authentication: &SetupAuthentication,
        profile: DiscoveryProfile,
    ) -> Result<Vec<CustomModel>, ProbeFailure> {
        let models_url = endpoint
            .join("models")
            .map_err(|_| ProbeFailure::Unreachable)?;
        let client = reqwest::blocking::Client::builder()
            .timeout(self.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| ProbeFailure::Unreachable)?;
        let authorization = authentication.authorization_headers()?;
        let request = client.get(models_url).headers(authorization.clone());
        let response = request.send().map_err(|_| ProbeFailure::Unreachable)?;
        let status = response.status();
        if matches!(status.as_u16(), 401 | 403) {
            return Err(ProbeFailure::AuthenticationFailed);
        }
        if !status.is_success() {
            return Err(ProbeFailure::Unreachable);
        }
        let body = bounded_json(response).map_err(|_| ProbeFailure::InvalidDiscovery)?;
        let mut models = models_from_json(&body)?;
        if profile == DiscoveryProfile::LmStudioNative {
            let native_inventory = crate::app::bootstrap::lm_studio_native_models_url(endpoint)
                .and_then(|url| {
                    let request = client.get(url).headers(authorization);
                    let response = request.send().ok()?;
                    if !response.status().is_success() {
                        return None;
                    }
                    bounded_json(response).ok()
                });
            crate::app::bootstrap::apply_lm_studio_native_metadata(
                &mut models,
                native_inventory.as_ref(),
            );
        }
        Ok(models)
    }
}

/// Transactional coordinator shared by CLI and TUI adapters.
pub(crate) struct ProviderSetupService<P = HttpSetupProbe> {
    store: CredentialStore,
    offline: bool,
    probe: P,
}

impl ProviderSetupService<HttpSetupProbe> {
    pub(crate) fn new(store: CredentialStore, offline: bool) -> Self {
        Self {
            store,
            offline,
            probe: HttpSetupProbe::default(),
        }
    }
}

impl<P> ProviderSetupService<P>
where
    P: SetupProbe,
{
    #[cfg(test)]
    fn with_probe(store: CredentialStore, offline: bool, probe: P) -> Self {
        Self {
            store,
            offline,
            probe,
        }
    }

    /// Classify startup without launching an onboarding surface.
    pub(crate) fn existing_providers(
        &self,
    ) -> Result<Vec<ExistingProviderSummary>, ProviderSetupError> {
        let snapshot = self
            .store
            .load_registry_snapshot()
            .map_err(|_| ProviderSetupError::Storage)?;
        let Some(registry) = snapshot.registry() else {
            return Ok(Vec::new());
        };
        Ok(registry
            .providers
            .iter()
            .map(|(provider_id, provider)| ExistingProviderSummary {
                provider_id: bounded_copy(provider_id, MAX_PROVIDER_ID_BYTES),
                label: bounded_copy(&provider.label, MAX_LABEL_BYTES),
                endpoint: sanitized_endpoint(&provider.credential.base_url),
                authentication: setup_credential_summary(provider, registry.legacy_single_endpoint),
                discovery_profile: if provider
                    .credential
                    .models
                    .iter()
                    .any(|model| model.native_metadata.is_some())
                {
                    DiscoveryProfile::LmStudioNative
                } else {
                    DiscoveryProfile::OpenAiCompatible
                },
                models: crate::app::bootstrap::configured_custom_models(&provider.credential)
                    .iter()
                    .filter_map(setup_model_summary)
                    .collect(),
            })
            .collect())
    }

    pub(crate) fn readiness(
        catalog: &ModelCatalog,
        requested_model: Option<&ModelId>,
    ) -> ProviderSetupState {
        if let Some(model) = requested_model {
            if catalog.resolve(model).is_err() {
                return ProviderSetupState::UnavailableResumedModel {
                    model: bounded_copy(&model.0, MAX_MODEL_ID_BYTES),
                };
            }
        }
        if catalog.models().next().is_none() {
            ProviderSetupState::NoProvider
        } else {
            ProviderSetupState::Ready
        }
    }

    /// Capture a private registry snapshot and validate a draft. This does not
    /// probe, write, cache, or otherwise mutate persistent state.
    pub(crate) fn begin(&self, draft: SetupDraft) -> Result<SetupTransaction, ProviderSetupError> {
        let mut draft = validate_draft(draft)?;
        let snapshot = self
            .store
            .load_registry_snapshot()
            .map_err(|_| ProviderSetupError::Storage)?;
        let existing = snapshot
            .registry()
            .and_then(|registry| registry.providers.get(&draft.provider_id));
        if draft.edit_existing {
            let Some(existing) = existing else {
                return Err(ProviderSetupError::State(
                    ProviderSetupState::ProviderNotConfigured,
                ));
            };
            if matches!(&draft.authentication, SetupAuthentication::PreserveExisting) {
                let endpoint_changed =
                    !same_endpoint(&existing.credential.base_url, &draft.endpoint);
                if endpoint_changed
                    && provider_has_credentials(
                        existing,
                        snapshot
                            .registry()
                            .is_some_and(|r| r.legacy_single_endpoint),
                    )
                    && !draft.allow_credential_reuse_at_new_endpoint
                {
                    return Err(ProviderSetupError::State(
                        ProviderSetupState::CredentialReuseRequiresConfirmation,
                    ));
                }
                draft.authentication =
                    SetupAuthentication::Preserved(ExistingSetupAuthentication {
                        api_key: existing.credential.api_key.clone(),
                        headers: existing.credential.headers.clone(),
                        auth: existing.auth.clone(),
                        api_key_env: existing.api_key_env.clone(),
                        legacy_single_endpoint: snapshot
                            .registry()
                            .is_some_and(|registry| registry.legacy_single_endpoint),
                    });
            }
            if draft.discovery_profile.is_none() {
                draft.discovery_profile = existing
                    .credential
                    .models
                    .iter()
                    .any(|model| model.native_metadata.is_some())
                    .then_some(DiscoveryProfile::LmStudioNative);
            }
        } else {
            if matches!(&draft.authentication, SetupAuthentication::PreserveExisting) {
                return Err(ProviderSetupError::InvalidConfiguration);
            }
            if existing.is_some() && !draft.replace_existing {
                return Err(ProviderSetupError::State(
                    ProviderSetupState::ProviderAlreadyConfigured,
                ));
            }
        }
        Ok(SetupTransaction {
            snapshot,
            registry_path: self.store.path().to_owned(),
            draft,
            discovered: Vec::new(),
        })
    }

    /// Probe only the transaction's explicitly selected endpoint. No cache or
    /// registry is written, and offline mode returns before credential lookup or
    /// network construction.
    pub(crate) fn discover(
        &self,
        transaction: &mut SetupTransaction,
    ) -> Result<Vec<SetupModel>, ProviderSetupError> {
        if self.offline {
            return Err(ProviderSetupError::State(ProviderSetupState::Offline));
        }
        let models = self
            .probe
            .discover(
                &transaction.draft.endpoint,
                &transaction.draft.authentication,
                transaction
                    .draft
                    .discovery_profile
                    .unwrap_or(DiscoveryProfile::OpenAiCompatible),
            )
            .map_err(probe_error)?;
        if models.is_empty() {
            transaction.discovered.clear();
            return Err(ProviderSetupError::State(
                ProviderSetupState::EmptyDiscovery,
            ));
        }
        transaction.discovered = merge_existing_model_settings(transaction, models);
        Ok(transaction
            .discovered
            .iter()
            .filter_map(setup_model_summary)
            .collect())
    }

    /// Select one previously discovered model without any new network traffic.
    pub(crate) fn prepare_discovered(
        &self,
        transaction: SetupTransaction,
        model_id: &str,
    ) -> Result<SetupPrepared, ProviderSetupError> {
        let model_id = validate_model_id(model_id)?;
        let selected = transaction
            .discovered
            .iter()
            .find(|model| model.api_name == model_id)
            .cloned()
            .ok_or(ProviderSetupError::State(
                ProviderSetupState::ManualModelRequired,
            ))?;
        let models = transaction.discovered.clone();
        prepared_from(transaction, models, selected)
    }

    /// Prepare an explicit manual model inventory. This path intentionally does
    /// not probe, so it is also the safe recovery path for offline or unsupported
    /// discovery.
    pub(crate) fn prepare_manual(
        &self,
        transaction: SetupTransaction,
        model_id: impl AsRef<str>,
    ) -> Result<SetupPrepared, ProviderSetupError> {
        let api_name = validate_model_id(model_id.as_ref())?;
        let model = CustomModel {
            api_name: api_name.clone(),
            display_name: api_name.clone(),
            ..CustomModel::default()
        };
        let models = merge_existing_model_settings(&transaction, vec![model]);
        let selected = models
            .iter()
            .find(|model| model.api_name == api_name)
            .cloned()
            .ok_or(ProviderSetupError::CatalogUnavailable)?;
        prepared_from(transaction, models, selected)
    }

    /// Prepare a manually entered model with an explicit operational context.
    /// The value is a user pin, not endpoint-discovered metadata.
    pub(crate) fn prepare_manual_with_context(
        &self,
        transaction: SetupTransaction,
        model_id: impl AsRef<str>,
        context_window: u64,
    ) -> Result<SetupPrepared, ProviderSetupError> {
        let api_name = validate_model_id(model_id.as_ref())?;
        let mut model = CustomModel {
            api_name: api_name.clone(),
            display_name: api_name.clone(),
            ..CustomModel::default()
        };
        apply_context_configuration(&mut model, context_window)?;
        let models = merge_existing_model_settings(&transaction, vec![model]);
        let mut selected = models
            .iter()
            .find(|model| model.api_name == api_name)
            .cloned()
            .ok_or(ProviderSetupError::CatalogUnavailable)?;
        apply_context_configuration(&mut selected, context_window)?;
        let mut models = models;
        if let Some(stored) = models.iter_mut().find(|model| model.api_name == api_name) {
            apply_context_configuration(stored, context_window)?;
        }
        prepared_from(transaction, models, selected)
    }

    /// Override only the effective context for a discovered model when the user
    /// explicitly configures it. Other discovery facts remain provenance-bearing.
    pub(crate) fn prepare_discovered_with_context(
        &self,
        transaction: SetupTransaction,
        model_id: &str,
        context_window: u64,
    ) -> Result<SetupPrepared, ProviderSetupError> {
        let model_id = validate_model_id(model_id)?;
        let mut models = transaction.discovered.clone();
        let selected = models
            .iter_mut()
            .find(|model| model.api_name == model_id)
            .ok_or(ProviderSetupError::State(
                ProviderSetupState::ManualModelRequired,
            ))?;
        apply_context_configuration(selected, context_window)?;
        let selected = selected.clone();
        prepared_from(transaction, models, selected)
    }

    /// Persist a prepared transaction with owner-private compare-and-swap
    /// semantics, then rebuild and verify the same canonical model catalog.
    /// No Agent is constructed at this boundary.
    pub(crate) fn commit_and_rebuild(
        &self,
        prepared: SetupPrepared,
    ) -> Result<CompletedSetup, ProviderSetupError> {
        let selected_model = prepared.receipt.selected_model_id();
        let receipt = prepared.receipt.clone();
        let registry = registry_for_prepared(prepared)?;
        self.store
            .save_registry_if_unchanged(&registry.0, &registry.1)
            .map_err(registry_error)?;
        let catalog = crate::app::bootstrap::model_catalog_with_setup_store(&self.store, true)
            .map_err(|_| ProviderSetupError::CatalogUnavailable)?;
        if catalog.resolve(&selected_model).is_err() {
            return Err(ProviderSetupError::CatalogUnavailable);
        }
        Ok(CompletedSetup {
            catalog,
            model: selected_model,
            receipt,
        })
    }
}

impl<P> Clone for ProviderSetupService<P>
where
    P: Clone,
{
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            offline: self.offline,
            probe: self.probe.clone(),
        }
    }
}

/// The canonical catalog and selected model returned after a confirmed setup.
pub(crate) struct CompletedSetup {
    pub(crate) catalog: ModelCatalog,
    pub(crate) model: ModelId,
    pub(crate) receipt: SetupReceipt,
}

/// Thin deterministic CLI adapter over the same transaction used by guided
/// onboarding. It never reads stdin, opens a picker, or emits a secret value.
pub(crate) fn run_cli(
    command: &crate::cli::SetupCommand,
    config: &crate::config::Config,
) -> anyhow::Result<()> {
    if command.cancel {
        crate::output::stdout_line(ProviderSetupState::Cancelled);
        return Ok(());
    }

    let (preset, endpoint) = cli_preset_and_endpoint(command)?;
    let provider_id = command.provider.clone().unwrap_or_else(|| match preset {
        crate::cli::SetupPreset::LmStudio => "local".to_owned(),
        crate::cli::SetupPreset::OpenAiCompatible => "custom".to_owned(),
    });
    let label = command.label.clone().unwrap_or_else(|| match preset {
        crate::cli::SetupPreset::LmStudio => "LM Studio".to_owned(),
        crate::cli::SetupPreset::OpenAiCompatible => "OpenAI-compatible endpoint".to_owned(),
    });
    let authentication = match &command.api_key_env {
        Some(variable) => SetupAuthentication::environment(variable.clone()),
        None => SetupAuthentication::no_authentication(),
    };
    let service = ProviderSetupService::new(
        CredentialStore::new(crate::auth::custom::default_path()),
        command.offline || config.offline,
    );
    let mut draft = SetupDraft::new(provider_id, label, endpoint, authentication)
        .replace_existing(command.replace);
    if preset == crate::cli::SetupPreset::LmStudio && command.endpoint.is_none() {
        draft = draft.with_discovery_profile(DiscoveryProfile::LmStudioNative);
    }
    let transaction = service.begin(draft)?;
    let prepared = if let Some(model) = command.manual_model.as_deref() {
        match command.context_window {
            Some(context_window) => {
                service.prepare_manual_with_context(transaction, model, context_window)?
            }
            None => service.prepare_manual(transaction, model)?,
        }
    } else {
        let mut transaction = transaction;
        let discovered = service.discover(&mut transaction)?;
        let selected = command
            .model
            .as_deref()
            .or_else(|| discovered.first().map(|model| model.api_name.as_str()))
            .ok_or(ProviderSetupError::State(
                ProviderSetupState::EmptyDiscovery,
            ))?;
        let selected = selected.to_owned();
        match command.context_window {
            Some(context_window) => {
                service.prepare_discovered_with_context(transaction, &selected, context_window)?
            }
            None => service.prepare_discovered(transaction, &selected)?,
        }
    };
    let authority = SetupAuthority::from_config(config);
    let review = prepared.receipt().render(Some(&authority));
    if !command.yes {
        let _ = prepared.cancel();
        crate::output::stdout_multiline(format!(
            "{review}\nreview: not saved; pass --yes to confirm\nstatus: {}",
            ProviderSetupState::Cancelled
        ));
        return Ok(());
    }

    let completed = service.commit_and_rebuild(prepared)?;
    // This is the same user-level selection persistence performed by the model
    // picker. It happens only after the registry CAS and catalog verification.
    crate::cli::persist_model(&completed.model.0).map_err(|_| {
        anyhow::anyhow!(
            "provider setup committed, but the selected model preference could not be saved; rerun with --model {}",
            completed.model.0
        )
    })?;
    crate::output::stdout_multiline(completed.receipt.render(Some(&authority)));
    Ok(())
}

/// Resolve the command's endpoint before any credential lookup, registry read,
/// or network traffic. An LM Studio probe is therefore possible only after its
/// preset was explicitly selected.
fn cli_preset_and_endpoint(
    command: &crate::cli::SetupCommand,
) -> anyhow::Result<(crate::cli::SetupPreset, String)> {
    let preset = match (command.preset, command.endpoint.as_ref()) {
        // An entered URL is always a generic compatible endpoint. In
        // particular, it cannot inherit the LM Studio profile merely because
        // that profile was also named on the command line.
        (_, Some(_)) => crate::cli::SetupPreset::OpenAiCompatible,
        (Some(preset), None) => preset,
        (None, None) => anyhow::bail!(
            "select an explicit endpoint with --endpoint <URL> or --preset lm-studio; setup never probes an implicit endpoint"
        ),
    };
    let endpoint = match (preset, command.endpoint.as_deref()) {
        (_, Some(endpoint)) => endpoint.to_owned(),
        (crate::cli::SetupPreset::LmStudio, None) => "http://localhost:1234/v1/".to_owned(),
        (crate::cli::SetupPreset::OpenAiCompatible, None) => {
            anyhow::bail!("--endpoint is required with --preset openai-compatible")
        }
    };
    Ok((preset, endpoint))
}

fn validate_draft(draft: SetupDraft) -> Result<ValidatedDraft, ProviderSetupError> {
    let provider_id = draft.provider_id.trim();
    let label = draft.label.trim();
    if !valid_provider_id(provider_id)
        || !valid_display_text(label, MAX_LABEL_BYTES)
        || draft.endpoint.len() > MAX_ENDPOINT_BYTES
    {
        return Err(ProviderSetupError::InvalidConfiguration);
    }
    if matches!(&draft.authentication, SetupAuthentication::PreserveExisting)
        && !draft.edit_existing
    {
        return Err(ProviderSetupError::InvalidConfiguration);
    }
    draft.authentication.validate()?;
    let mut endpoint = url::Url::parse(draft.endpoint.trim())
        .map_err(|_| ProviderSetupError::InvalidConfiguration)?;
    if !matches!(endpoint.scheme(), "http" | "https")
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(ProviderSetupError::InvalidConfiguration);
    }
    if !endpoint.path().ends_with('/') {
        let path = format!("{}/", endpoint.path());
        endpoint.set_path(&path);
    }
    if endpoint.to_string().len() > MAX_ENDPOINT_BYTES {
        return Err(ProviderSetupError::InvalidConfiguration);
    }
    Ok(ValidatedDraft {
        provider_id: provider_id.to_owned(),
        label: label.to_owned(),
        endpoint,
        authentication: draft.authentication,
        replace_existing: draft.replace_existing,
        edit_existing: draft.edit_existing,
        allow_credential_reuse_at_new_endpoint: draft.allow_credential_reuse_at_new_endpoint,
        discovery_profile: draft.discovery_profile,
    })
}

fn apply_context_configuration(
    model: &mut CustomModel,
    context_window: u64,
) -> Result<(), ProviderSetupError> {
    if context_window == 0 {
        return Err(ProviderSetupError::InvalidConfiguration);
    }
    model.context_window = context_window;
    model.context_window_configured = true;
    model.max_output_tokens = model.max_output_tokens.min(context_window);
    Ok(())
}

fn merge_existing_model_settings(
    transaction: &SetupTransaction,
    discovered: Vec<CustomModel>,
) -> Vec<CustomModel> {
    if !transaction.draft.edit_existing {
        return discovered;
    }
    let mut configured = transaction
        .snapshot
        .registry()
        .and_then(|registry| registry.providers.get(&transaction.draft.provider_id))
        .map(|provider| crate::app::bootstrap::configured_custom_models(&provider.credential))
        .unwrap_or_default();
    if transaction.draft.discovery_profile == Some(DiscoveryProfile::OpenAiCompatible) {
        for model in &mut configured {
            model.native_metadata = None;
        }
    }
    crate::app::bootstrap::apply_configured_custom_model_overrides(discovered, &configured, true)
}

fn same_endpoint(old: &str, new: &url::Url) -> bool {
    let Ok(mut old) = url::Url::parse(old) else {
        return false;
    };
    if !old.username().is_empty()
        || old.password().is_some()
        || old.query().is_some()
        || old.fragment().is_some()
    {
        return false;
    }
    if !old.path().ends_with('/') {
        let path = format!("{}/", old.path());
        old.set_path(&path);
    }
    old == *new
}

fn provider_has_credentials(provider: &CustomProvider, legacy_single_endpoint: bool) -> bool {
    !provider.credential.headers.is_empty()
        || match provider.auth.as_ref() {
            Some(CustomAuthConfig::None) => false,
            Some(CustomAuthConfig::BearerEnv { .. }) => true,
            None => {
                provider.api_key_env.is_some()
                    || !provider.credential.api_key.is_empty()
                    || legacy_single_endpoint
            }
        }
}

fn prepared_from(
    transaction: SetupTransaction,
    mut models: Vec<CustomModel>,
    mut selected: CustomModel,
) -> Result<SetupPrepared, ProviderSetupError> {
    let configured_context = selected
        .context_window_configured
        .then_some(selected.context_window);
    let discovered_context = selected.context_window_asserted.then_some(
        selected
            .discovered_context_window
            .unwrap_or(selected.context_window),
    );
    let (context_window, context_source) = match configured_context.filter(|value| *value > 0) {
        Some(context_window) => (context_window, ContextWindowSource::ExplicitConfiguration),
        None => match discovered_context.filter(|value| *value > 0) {
            Some(context_window) => (context_window, ContextWindowSource::DiscoveredEffective),
            None => {
                return Err(ProviderSetupError::State(
                    ProviderSetupState::ContextWindowRequired,
                ));
            }
        },
    };
    selected.context_window = context_window;
    selected.max_output_tokens = selected.max_output_tokens.min(context_window);
    if selected.max_output_tokens == 0 {
        return Err(ProviderSetupError::InvalidConfiguration);
    }
    if let Some(model) = models
        .iter_mut()
        .find(|model| model.api_name == selected.api_name)
    {
        *model = selected.clone();
    } else {
        models.push(selected.clone());
    }
    models.sort_by(|left, right| left.api_name.cmp(&right.api_name));
    models.dedup_by(|left, right| left.api_name == right.api_name);
    validate_prepared_models(&models)?;
    let receipt = SetupReceipt {
        provider_id: transaction.draft.provider_id.clone(),
        provider_label: transaction.draft.label.clone(),
        endpoint: transaction.draft.endpoint_string(),
        endpoint_kind: endpoint_kind(&transaction.draft.endpoint),
        selected_model: selected.api_name.clone(),
        context_window,
        context_source,
        credential_policy: transaction.draft.authentication.receipt_description(),
        registry_path: transaction.registry_path.clone(),
    };
    Ok(SetupPrepared {
        snapshot: transaction.snapshot,
        draft: transaction.draft,
        models,
        selected,
        receipt,
    })
}

fn validate_prepared_models(models: &[CustomModel]) -> Result<(), ProviderSetupError> {
    for model in models {
        let configured_context = model.context_window_configured && model.context_window > 0;
        let discovered_context = model.context_window_asserted
            && model
                .discovered_context_window
                .unwrap_or(model.context_window)
                > 0;
        let effective_context = if configured_context {
            Some(model.context_window)
        } else if discovered_context {
            Some(
                model
                    .discovered_context_window
                    .unwrap_or(model.context_window),
            )
        } else {
            None
        };
        if let Some(effective_context) = effective_context {
            if model.max_output_tokens.min(effective_context) == 0 {
                return Err(ProviderSetupError::InvalidConfiguration);
            }
        }
        if !model.reasoning_values.is_empty() {
            if !(octet_ai::types::ReasoningOptions {
                values: model.reasoning_values.clone(),
                default: (!model.reasoning_default.is_empty())
                    .then(|| model.reasoning_default.clone()),
            })
            .is_valid()
            {
                return Err(ProviderSetupError::InvalidConfiguration);
            }
        } else if !model.reasoning_default.is_empty() {
            return Err(ProviderSetupError::InvalidConfiguration);
        }
        model
            .preset
            .validate()
            .map_err(|_| ProviderSetupError::InvalidConfiguration)?;
    }
    Ok(())
}

fn registry_for_prepared(
    prepared: SetupPrepared,
) -> Result<(RegistrySnapshot, CustomRegistry), ProviderSetupError> {
    let SetupPrepared {
        snapshot,
        draft,
        models,
        selected,
        ..
    } = prepared;
    let ValidatedDraft {
        provider_id,
        label,
        endpoint,
        authentication,
        replace_existing,
        ..
    } = draft;
    let previous = snapshot
        .registry()
        .and_then(|registry| registry.providers.get(&provider_id))
        .cloned();
    let base_url = endpoint.to_string();
    let stored = authentication.into_storage();
    let provider = CustomProvider {
        label,
        credential: CustomCredential {
            base_url,
            api_key: stored.api_key,
            api_name: selected.api_name,
            headers: stored.headers,
            models,
            // The setup transaction already probed exactly this endpoint and
            // persists its reviewed inventory. Rebuild from that snapshot without
            // a second request; normal startup refreshes it when auto-discovery is on.
            auto_discover: true,
        },
        auth: stored.auth,
        api_key_env: stored.api_key_env,
        cache: previous
            .as_ref()
            .and_then(|provider| provider.cache.clone()),
        startup_timeout_secs: previous
            .as_ref()
            .and_then(|provider| provider.startup_timeout_secs),
        // Preserve explicit opt-ins during an edit; new providers never gain
        // lifecycle feedback merely by being created.
        lifecycle_feedback: previous
            .as_ref()
            .is_some_and(|provider| provider.lifecycle_feedback),
    };
    let mut registry = snapshot.registry().cloned().unwrap_or(CustomRegistry {
        version: 1,
        providers: Default::default(),
        legacy_single_endpoint: false,
    });
    if registry.providers.contains_key(&provider_id) && !replace_existing {
        return Err(ProviderSetupError::State(
            ProviderSetupState::ProviderAlreadyConfigured,
        ));
    }
    registry.legacy_single_endpoint = false;
    registry.providers.insert(provider_id, provider);
    Ok((snapshot, registry))
}

fn registry_error(error: RegistryCommitError) -> ProviderSetupError {
    match error {
        RegistryCommitError::Changed => {
            ProviderSetupError::State(ProviderSetupState::ConcurrentRegistry)
        }
        RegistryCommitError::Storage => ProviderSetupError::Storage,
    }
}

fn probe_error(error: ProbeFailure) -> ProviderSetupError {
    ProviderSetupError::State(match error {
        ProbeFailure::Unreachable | ProbeFailure::InvalidDiscovery => {
            ProviderSetupState::EndpointUnreachable
        }
        ProbeFailure::AuthenticationFailed => ProviderSetupState::AuthenticationFailed,
    })
}

fn bounded_json(response: reqwest::blocking::Response) -> Result<serde_json::Value, ()> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DISCOVERY_BODY_BYTES as u64)
    {
        return Err(());
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_DISCOVERY_BODY_BYTES.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() > MAX_DISCOVERY_BODY_BYTES {
        return Err(());
    }
    serde_json::from_slice(&bytes).map_err(|_| ())
}

fn models_from_json(value: &serde_json::Value) -> Result<Vec<CustomModel>, ProbeFailure> {
    let entries = value
        .get("data")
        .or_else(|| value.get("models"))
        .and_then(serde_json::Value::as_array)
        .or_else(|| value.as_array())
        .ok_or(ProbeFailure::InvalidDiscovery)?;
    let mut models = Vec::new();
    for entry in entries.iter().take(MAX_DISCOVERED_MODELS) {
        let Some(id) = entry
            .get("id")
            .or_else(|| entry.get("slug"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|id| valid_model_id(id))
        else {
            continue;
        };
        let mut model = CustomModel {
            api_name: id.to_owned(),
            display_name: crate::app::bootstrap::discovered_display_name(entry, id)
                .unwrap_or_default(),
            ..CustomModel::default()
        };
        if let Some(context_window) = crate::app::bootstrap::extract_ctx_from_model_entry(entry) {
            model.context_window = context_window;
            model.context_window_asserted = true;
            model.discovered_context_window = Some(context_window);
        }
        if let Some(max_output_tokens) =
            positive_u64(entry, &["max_output_tokens", "max_completion_tokens"])
        {
            model.max_output_tokens = max_output_tokens.min(model.context_window);
            model.max_output_tokens_asserted = true;
        }
        crate::app::bootstrap::apply_discovered_reasoning(entry, &mut model)
            .map_err(|_| ProbeFailure::InvalidDiscovery)?;
        models.push(model);
    }
    models.sort_by(|left, right| left.api_name.cmp(&right.api_name));
    models.dedup_by(|left, right| left.api_name == right.api_name);
    Ok(models)
}

fn positive_u64(entry: &serde_json::Value, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .find_map(|name| entry.get(*name).and_then(serde_json::Value::as_u64))
        .filter(|value| *value > 0)
}

fn endpoint_kind(endpoint: &url::Url) -> EndpointKind {
    let local = endpoint.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host.ends_with(".local")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if local {
        EndpointKind::Local
    } else {
        EndpointKind::Network
    }
}

fn valid_provider_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PROVIDER_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_environment_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(byte) if byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn valid_display_text(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

fn valid_model_id(value: &str) -> bool {
    valid_display_text(value, MAX_MODEL_ID_BYTES)
}

fn validate_model_id(value: &str) -> Result<String, ProviderSetupError> {
    let value = value.trim();
    if valid_model_id(value) {
        Ok(value.to_owned())
    } else {
        Err(ProviderSetupError::InvalidConfiguration)
    }
}

fn bounded_copy(value: &str, maximum: usize) -> String {
    let mut output = String::new();
    for character in value.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if output.len().saturating_add(character.len_utf8()) > maximum {
            break;
        }
        output.push(character);
    }
    output.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone)]
    struct FixtureProbe {
        result: Arc<Mutex<Result<Vec<CustomModel>, ProbeFailure>>>,
        requests: Arc<Mutex<usize>>,
    }

    impl FixtureProbe {
        fn models(names: &[&str]) -> Self {
            Self {
                result: Arc::new(Mutex::new(Ok(names
                    .iter()
                    .map(|name| CustomModel {
                        api_name: (*name).to_owned(),
                        display_name: (*name).to_owned(),
                        context_window: 8_192,
                        max_output_tokens: 1_024,
                        context_window_asserted: true,
                        discovered_context_window: Some(8_192),
                        ..CustomModel::default()
                    })
                    .collect()))),
                requests: Arc::new(Mutex::new(0)),
            }
        }
    }

    impl SetupProbe for FixtureProbe {
        fn discover(
            &self,
            _endpoint: &url::Url,
            _authentication: &SetupAuthentication,
            _profile: DiscoveryProfile,
        ) -> Result<Vec<CustomModel>, ProbeFailure> {
            *self.requests.lock().unwrap() += 1;
            self.result.lock().unwrap().clone()
        }
    }

    fn service(probe: FixtureProbe) -> (tempfile::TempDir, ProviderSetupService<FixtureProbe>) {
        let directory = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(directory.path().join("credentials/custom.json"));
        let service = ProviderSetupService::with_probe(store, false, probe);
        (directory, service)
    }

    fn draft() -> SetupDraft {
        SetupDraft::new(
            "local",
            "Fixture Local",
            "http://127.0.0.1:1234/v1/",
            SetupAuthentication::no_authentication(),
        )
    }

    fn cli_command() -> crate::cli::SetupCommand {
        crate::cli::SetupCommand {
            preset: None,
            endpoint: None,
            provider: None,
            label: None,
            model: None,
            manual_model: None,
            context_window: None,
            api_key_env: None,
            no_auth: false,
            replace: false,
            yes: false,
            cancel: false,
            offline: false,
        }
    }

    fn api_key_store(directory: &tempfile::TempDir) -> BuiltinApiKeyStore {
        BuiltinApiKeyStore {
            directory: directory.path().join("credentials/api-keys"),
        }
    }

    #[test]
    fn api_key_choices_preserve_native_presets_and_exclude_extra_setup() {
        let providers = builtin_api_key_providers();
        for id in ["openai", "anthropic", "gemini", "openrouter", "deepseek"] {
            assert!(providers.iter().any(|provider| provider.id == id), "{id}");
        }
        for id in [
            "codex",
            "github-copilot",
            "vertex",
            "bedrock",
            "azure-openai",
            "cloudflare-workers-ai",
            "cloudflare-ai-gateway",
        ] {
            assert!(!providers.iter().any(|provider| provider.id == id), "{id}");
        }
        assert_eq!(
            providers
                .iter()
                .find(|provider| provider.id == "anthropic")
                .unwrap()
                .credential_variable,
            "ANTHROPIC_API_KEY"
        );
    }

    #[test]
    fn builtin_api_key_round_trips_without_environment_or_config() {
        let directory = tempfile::tempdir().unwrap();
        let store = api_key_store(&directory);
        assert!(store.load("anthropic").unwrap().is_none());
        let path = store
            .save("anthropic", "  synthetic-key\n".into(), false)
            .unwrap();
        // Only the permission assertions below name the returned path.
        #[cfg(not(unix))]
        let _ = path;
        // A fresh store models the subsequent process's credential lookup.
        let restarted = api_key_store(&directory);
        assert_eq!(
            restarted.load("anthropic").unwrap().as_deref(),
            Some("synthetic-key")
        );
        assert!(!format!("{store:?}").contains("synthetic-key"));
        assert!(!directory.path().join("config.toml").exists());
        assert!(store
            .save("anthropic", "replacement".into(), false)
            .is_err());
        assert_eq!(
            restarted.load("anthropic").unwrap().as_deref(),
            Some("synthetic-key")
        );
        store.save("anthropic", "replacement".into(), true).unwrap();
        assert_eq!(
            restarted.load("anthropic").unwrap().as_deref(),
            Some("replacement")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn builtin_api_key_validation_is_bounded_and_errors_are_secret_free() {
        let directory = tempfile::tempdir().unwrap();
        let store = api_key_store(&directory);
        for invalid in [
            String::new(),
            " \n".into(),
            "secret\r\nheader:value".into(),
            "secret with space".into(),
            "x".repeat(octet_ai::auth::MAX_ENV_VALUE_BYTES + 1),
        ] {
            let error = store.save("openai", invalid, false).unwrap_err();
            assert!(!error.to_string().contains("secret"));
        }
        for id in ["../other", "/tmp/other", "codex", "unknown"] {
            assert!(store.save(id, "synthetic-key".into(), false).is_err());
        }
        assert!(!store.directory.exists());
    }

    #[cfg(unix)]
    #[test]
    fn builtin_api_key_store_rejects_links_and_public_files() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let directory = tempfile::tempdir().unwrap();
        let store = api_key_store(&directory);
        let path = store.save("openai", "synthetic-key".into(), false).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.load("openai").is_err());
        assert!(store.save("openai", "replacement".into(), true).is_err());
        std::fs::remove_file(&path).unwrap();
        let target = directory.path().join("unrelated");
        std::fs::write(&target, b"unchanged").unwrap();
        symlink(&target, &path).unwrap();
        assert!(store.load("openai").is_err());
        assert!(store.save("openai", "replacement".into(), true).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"unchanged");
        let linked_store = BuiltinApiKeyStore {
            directory: directory.path().join("linked"),
        };
        symlink(&store.directory, &linked_store.directory).unwrap();
        assert!(linked_store
            .save("anthropic", "synthetic-key".into(), false)
            .is_err());
        assert!(!store.directory.join("anthropic.json").exists());
    }

    #[test]
    fn builtin_api_key_store_rejects_malformed_and_oversized_records() {
        let directory = tempfile::tempdir().unwrap();
        let store = api_key_store(&directory);
        let path = store.path("openai").unwrap();
        for bytes in [
            br#"{"version":2,"api_key":"synthetic-key"}"#.to_vec(),
            br#"{"version":1,"api_key":"synthetic-key","extra":true}"#.to_vec(),
            vec![b'x'; MAX_API_KEY_FILE_BYTES + 1],
        ] {
            octet_agent::secure_fs::write_private_atomic(&path, &bytes, MAX_API_KEY_FILE_BYTES + 1)
                .unwrap();
            let error = store.load("openai").unwrap_err();
            assert!(!error.to_string().contains("synthetic-key"));
        }
    }

    #[test]
    fn cli_requires_an_explicit_endpoint_selection_before_any_probe() {
        let command = cli_command();
        assert!(cli_preset_and_endpoint(&command)
            .unwrap_err()
            .to_string()
            .contains("never probes an implicit endpoint"));

        let mut lm_studio = cli_command();
        lm_studio.preset = Some(crate::cli::SetupPreset::LmStudio);
        assert_eq!(
            cli_preset_and_endpoint(&lm_studio).unwrap(),
            (
                crate::cli::SetupPreset::LmStudio,
                "http://localhost:1234/v1/".to_owned()
            )
        );

        let mut endpoint = cli_command();
        endpoint.endpoint = Some("https://models.example.test/v1/".to_owned());
        assert_eq!(
            cli_preset_and_endpoint(&endpoint).unwrap(),
            (
                crate::cli::SetupPreset::OpenAiCompatible,
                "https://models.example.test/v1/".to_owned()
            )
        );

        endpoint.preset = Some(crate::cli::SetupPreset::LmStudio);
        assert_eq!(
            cli_preset_and_endpoint(&endpoint).unwrap(),
            (
                crate::cli::SetupPreset::OpenAiCompatible,
                "https://models.example.test/v1/".to_owned()
            )
        );
    }

    #[test]
    fn discovered_provider_is_prepared_then_committed_without_secret_receipt() {
        let (directory, service) = service(FixtureProbe::models(&["alpha", "beta"]));
        let mut transaction = service.begin(draft()).unwrap();
        let models = service.discover(&mut transaction).unwrap();
        assert_eq!(
            models
                .iter()
                .map(|model| model.api_name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
        let prepared = service.prepare_discovered(transaction, "beta").unwrap();
        let preview = prepared.receipt().render(None);
        assert!(preview.contains("direct only"));
        assert!(!preview.contains("secret-value"));
        let completed = service.commit_and_rebuild(prepared).unwrap();
        assert_eq!(completed.model.0, "custom/local/beta");
        let model = completed.catalog.resolve(&completed.model).unwrap();
        assert!(!model.endpoint.runtime.lifecycle_feedback);
        let registry = CredentialStore::new(directory.path().join("credentials/custom.json"))
            .load_registry()
            .unwrap()
            .unwrap();
        assert!(registry.providers["local"].credential.auto_discover);
        assert!(completed.receipt.render(None).contains("Fixture Local"));
    }

    #[test]
    fn cancellation_and_offline_leave_the_registry_absent_without_a_probe() {
        let probe = FixtureProbe::models(&["alpha"]);
        let requests = Arc::clone(&probe.requests);
        let (directory, service) = service(probe);
        let transaction = service.begin(draft()).unwrap();
        assert_eq!(transaction.cancel(), ProviderSetupState::Cancelled);
        assert!(!directory.path().join("credentials/custom.json").exists());
        assert_eq!(*requests.lock().unwrap(), 0);

        let offline = ProviderSetupService::with_probe(
            CredentialStore::new(directory.path().join("offline/custom.json")),
            true,
            FixtureProbe::models(&["alpha"]),
        );
        let mut transaction = offline.begin(draft()).unwrap();
        assert!(matches!(
            offline.discover(&mut transaction),
            Err(ProviderSetupError::State(ProviderSetupState::Offline))
        ));
        assert!(!directory.path().join("offline/custom.json").exists());
    }

    #[test]
    fn manual_model_recovers_without_discovery_and_rebuilds_offline() {
        let directory = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(directory.path().join("credentials/custom.json"));
        let probe = FixtureProbe::models(&["ignored"]);
        let service = ProviderSetupService::with_probe(store, true, probe.clone());
        let transaction = service.begin(draft()).unwrap();
        assert!(matches!(
            service.prepare_manual(transaction, "manual-model"),
            Err(ProviderSetupError::State(
                ProviderSetupState::ContextWindowRequired
            ))
        ));
        let transaction = service.begin(draft()).unwrap();
        let prepared = service
            .prepare_manual_with_context(transaction, "manual-model", 32_000)
            .unwrap();
        let completed = service.commit_and_rebuild(prepared).unwrap();
        assert_eq!(completed.model.0, "custom/local/manual-model");
        assert_eq!(*probe.requests.lock().unwrap(), 0);
    }

    #[test]
    fn existing_provider_requires_explicit_replacement_before_discovery() {
        let probe = FixtureProbe::models(&["alpha"]);
        let requests = Arc::clone(&probe.requests);
        let (_directory, service) = service(probe);
        let transaction = service.begin(draft()).unwrap();
        let prepared = service
            .prepare_manual_with_context(transaction, "manual-model", 8_192)
            .unwrap();
        service.commit_and_rebuild(prepared).unwrap();

        assert!(matches!(
            service.begin(draft()),
            Err(ProviderSetupError::State(
                ProviderSetupState::ProviderAlreadyConfigured
            ))
        ));
        assert_eq!(*requests.lock().unwrap(), 0);

        assert!(service.begin(draft().replace_existing(true)).is_ok());
    }

    #[test]
    fn existing_provider_edit_preserves_credentials_settings_and_context_provenance() {
        let directory = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(directory.path().join("credentials/custom.json"));
        let mut configured_model = CustomModel {
            api_name: "alpha".into(),
            context_window: 4_096,
            context_window_configured: true,
            max_output_tokens: 1_024,
            tools: false,
            ..Default::default()
        };
        configured_model
            .preset
            .sampling_params
            .insert("temperature".into(), serde_json::json!(0.25));
        store
            .save_registry(&CustomRegistry::single(
                "local",
                CustomProvider {
                    label: "Original label".into(),
                    credential: CustomCredential {
                        base_url: "http://127.0.0.1:1234/v1/".into(),
                        api_key: "stored-secret".into(),
                        api_name: String::new(),
                        headers: vec![HeaderEntry {
                            name: "x-provider-secret".into(),
                            value: "header-secret".into(),
                        }],
                        models: vec![configured_model.clone()],
                        auto_discover: true,
                    },
                    auth: None,
                    api_key_env: None,
                    cache: Some(octet_ai::CacheCompatibility::default()),
                    startup_timeout_secs: Some(73),
                    lifecycle_feedback: true,
                },
            ))
            .unwrap();

        let probe = FixtureProbe::models(&["alpha"]);
        let requests = Arc::clone(&probe.requests);
        let service = ProviderSetupService::with_probe(store.clone(), false, probe);
        let summaries = service.existing_providers().unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].provider_id, "local");
        assert_eq!(summaries[0].endpoint, "http://127.0.0.1:1234/v1/");
        assert_eq!(
            summaries[0].authentication,
            SetupCredentialSummary::StoredCredentialConfigured
        );
        assert!(!format!("{summaries:?}").contains("stored-secret"));
        assert!(!format!("{summaries:?}").contains("header-secret"));

        assert!(matches!(
            service.begin(SetupDraft::edit_existing(
                "local",
                "Renamed provider",
                "http://127.0.0.1:9999/v1/",
            )),
            Err(ProviderSetupError::State(
                ProviderSetupState::CredentialReuseRequiresConfirmation
            ))
        ));
        assert_eq!(*requests.lock().unwrap(), 0);

        let mut transaction = service
            .begin(SetupDraft::edit_existing(
                "local",
                "Renamed provider",
                "http://127.0.0.1:1234/v1/",
            ))
            .unwrap();
        let discovered = service.discover(&mut transaction).unwrap();
        assert_eq!(discovered[0].configured_context_window, Some(4_096));
        assert_eq!(discovered[0].context_window, Some(8_192));
        assert!(!discovered[0].requires_context_configuration);
        let prepared = service.prepare_discovered(transaction, "alpha").unwrap();
        let completed = service.commit_and_rebuild(prepared).unwrap();
        assert_eq!(
            completed
                .catalog
                .resolve(&completed.model)
                .unwrap()
                .spec
                .limits
                .context_window,
            4_096
        );

        let saved = store.load_registry().unwrap().unwrap();
        let provider = &saved.providers["local"];
        assert_eq!(provider.label, "Renamed provider");
        assert_eq!(provider.credential.api_key, "stored-secret");
        assert_eq!(provider.credential.headers[0].value, "header-secret");
        assert!(provider.credential.auto_discover);
        assert_eq!(provider.startup_timeout_secs, Some(73));
        assert!(provider.cache.is_some());
        assert!(provider.lifecycle_feedback);
        let saved_model = &provider.credential.models[0];
        assert_eq!(saved_model.context_window, 4_096);
        assert!(saved_model.context_window_configured);
        assert_eq!(saved_model.discovered_context_window, Some(8_192));
        assert!(!saved_model.tools);
        assert_eq!(saved_model.preset, configured_model.preset);
        assert!(!format!("{provider:?}").contains("stored-secret"));
        assert!(!format!("{provider:?}").contains("header-secret"));
        assert_eq!(*requests.lock().unwrap(), 1);
    }

    #[test]
    fn legacy_edit_makes_implicit_custom_api_key_source_explicit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("credentials/custom.json");
        octet_agent::secure_fs::write_private_atomic(
            &path,
            br#"{"base_url":"http://127.0.0.1:1234/v1/","api_key":"","api_name":"alpha"}"#,
            64 * 1024,
        )
        .unwrap();
        let store = CredentialStore::new(&path);
        let service = ProviderSetupService::with_probe(
            store.clone(),
            false,
            FixtureProbe::models(&["alpha"]),
        );
        let mut transaction = service
            .begin(SetupDraft::edit_existing(
                "custom-openai",
                "Legacy endpoint",
                "http://127.0.0.1:1234/v1/",
            ))
            .unwrap();
        service.discover(&mut transaction).unwrap();
        let prepared = service.prepare_discovered(transaction, "alpha").unwrap();
        service.commit_and_rebuild(prepared).unwrap();
        let registry = store.load_registry().unwrap().unwrap();
        assert!(matches!(
            &registry.providers["custom-openai"].auth,
            Some(CustomAuthConfig::BearerEnv { var }) if var == "OCTET_CUSTOM_API_KEY"
        ));
    }

    #[test]
    fn setup_keeps_unknown_siblings_actionable_but_out_of_catalog() {
        let probe = FixtureProbe::models(&["known", "unknown"]);
        *probe.result.lock().unwrap() = Ok(vec![
            CustomModel {
                api_name: "known".into(),
                context_window: 8_192,
                context_window_asserted: true,
                discovered_context_window: Some(8_192),
                ..Default::default()
            },
            CustomModel {
                api_name: "unknown".into(),
                ..Default::default()
            },
        ]);
        let (directory, service) = service(probe);
        let mut transaction = service.begin(draft()).unwrap();
        let summaries = service.discover(&mut transaction).unwrap();
        assert_eq!(summaries.len(), 2);
        assert!(summaries[1].requires_context_configuration);
        assert_eq!(summaries[1].context_window, None);

        let prepared = service.prepare_discovered(transaction, "known").unwrap();
        let completed = service.commit_and_rebuild(prepared).unwrap();
        assert!(completed.catalog.resolve(&completed.model).is_ok());
        assert!(completed
            .catalog
            .resolve(&ModelId("custom/local/unknown".to_owned()))
            .is_err());

        let registry = CredentialStore::new(directory.path().join("credentials/custom.json"))
            .load_registry()
            .unwrap()
            .unwrap();
        let unknown = registry.providers["local"]
            .credential
            .models
            .iter()
            .find(|model| model.api_name == "unknown")
            .unwrap();
        assert!(!unknown.context_window_asserted);
        assert!(!unknown.context_window_configured);
        assert_eq!(unknown.discovered_context_window, None);
    }

    #[test]
    fn concurrent_registry_change_is_not_overwritten() {
        let (directory, service) = service(FixtureProbe::models(&["alpha"]));
        let mut transaction = service.begin(draft()).unwrap();
        service.discover(&mut transaction).unwrap();
        let prepared = service.prepare_discovered(transaction, "alpha").unwrap();

        let store = CredentialStore::new(directory.path().join("credentials/custom.json"));
        store
            .save_registry(&CustomRegistry::single(
                "other",
                CustomProvider {
                    label: "Other".to_owned(),
                    credential: CustomCredential {
                        base_url: "http://127.0.0.1:9999/v1/".to_owned(),
                        api_key: String::new(),
                        api_name: "other".to_owned(),
                        headers: Vec::new(),
                        models: vec![CustomModel {
                            api_name: "other".to_owned(),
                            display_name: "other".to_owned(),
                            ..CustomModel::default()
                        }],
                        auto_discover: false,
                    },
                    auth: Some(CustomAuthConfig::None),
                    api_key_env: None,
                    cache: None,
                    startup_timeout_secs: None,
                    lifecycle_feedback: false,
                },
            ))
            .unwrap();

        assert!(matches!(
            service.commit_and_rebuild(prepared),
            Err(ProviderSetupError::State(
                ProviderSetupState::ConcurrentRegistry
            ))
        ));
        let registry = store.load_registry().unwrap().unwrap();
        assert!(registry.providers.contains_key("other"));
        assert!(!registry.providers.contains_key("local"));
    }

    #[test]
    fn receipt_and_errors_do_not_format_api_keys() {
        let secret = "secret-value-must-never-appear";
        let probe = FixtureProbe::models(&["alpha"]);
        let (_directory, service) = service(probe);
        let mut transaction = service
            .begin(SetupDraft::new(
                "local",
                "Fixture Local",
                "http://127.0.0.1:1234/v1/",
                SetupAuthentication::api_key(secret.to_owned()),
            ))
            .unwrap();
        service.discover(&mut transaction).unwrap();
        let prepared = service.prepare_discovered(transaction, "alpha").unwrap();
        assert!(!format!("{prepared:?}").contains(secret));
        assert!(!prepared.receipt().render(None).contains(secret));
    }

    #[test]
    fn readiness_covers_no_provider_unavailable_resume_and_ready() {
        let catalog = ModelCatalog::default();
        assert_eq!(
            ProviderSetupService::<HttpSetupProbe>::readiness(&catalog, None),
            ProviderSetupState::NoProvider
        );
        assert!(matches!(
            ProviderSetupService::<HttpSetupProbe>::readiness(
                &catalog,
                Some(&ModelId("gone".to_owned())),
            ),
            ProviderSetupState::UnavailableResumedModel { .. }
        ));
    }

    #[test]
    fn endpoint_and_model_validation_reject_credential_bearing_or_control_values() {
        let probe = FixtureProbe::models(&["alpha"]);
        let (_directory, service) = service(probe);
        assert!(matches!(
            service.begin(SetupDraft::new(
                "local",
                "Local",
                "https://user:password@example.test/v1/",
                SetupAuthentication::no_authentication(),
            )),
            Err(ProviderSetupError::InvalidConfiguration)
        ));
        assert!(matches!(
            service.begin(SetupDraft::new(
                "local",
                "Local",
                "http://127.0.0.1:1234/v1/?token=secret",
                SetupAuthentication::no_authentication(),
            )),
            Err(ProviderSetupError::InvalidConfiguration)
        ));
        assert!(validate_model_id("bad\u{1b}model").is_err());
    }
}
