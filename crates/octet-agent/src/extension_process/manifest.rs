//! Extension manifests, discovery, start policy and the extension catalog.

use super::*;

/// Lifecycle ownership selected by an executable-extension manifest.
///
/// Profiles are deliberately independent from an extension implementation
/// language. A runtime manager may share only profiles that also opt into an
/// explicit workspace sharing scope and whose content digest matches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionLifecycleProfile {
    /// Preserve the API 0.1/0.2 resident-process behavior. This is the
    /// manifest default and is isolated to the current session binding.
    #[default]
    LegacyResident,
    /// Start only when a caller explicitly activates the catalog entry, then
    /// retain the resident process according to its explicit sharing scope.
    LazyResident,
    /// Start for one admitted operation and stop at its settlement boundary.
    #[serde(rename = "oneshot", alias = "one_shot")]
    OneShot,
    /// Start with a session binding and stop when that binding is released.
    Session,
    /// A workspace-scoped service. It must opt into `sharing = "workspace"`.
    WorkspaceService,
    /// Start when the entry is eligible and retain it for the host lifetime.
    /// It must opt into `sharing = "workspace"`.
    Always,
    /// One exact ordered Pi source aggregate. It must opt into
    /// `sharing = "workspace"`; individual Pi sources are never pooled.
    PiAggregate,
}

/// Explicit scope in which a lifecycle profile may share one process.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionRuntimeSharing {
    /// Do not share this process across session bindings.
    #[default]
    Isolated,
    /// Permit sharing only after the runtime manager matches canonical
    /// workspace, explicit trust domain, and complete content digest.
    Workspace,
}

/// Optional runtime-manager settings from `[runtime]` in `extension.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionRuntimeSettings {
    /// Selected process lifecycle profile.
    #[serde(default)]
    pub lifecycle: ExtensionLifecycleProfile,
    /// Explicit sharing scope. Sharing is never inferred from implementation
    /// language or a matching extension name.
    #[serde(default)]
    pub sharing: ExtensionRuntimeSharing,
}

impl ExtensionRuntimeSettings {
    pub(super) fn validate(&self) -> Result<(), ExtensionRuntimeError> {
        let requires_workspace_sharing = matches!(
            self.lifecycle,
            ExtensionLifecycleProfile::WorkspaceService
                | ExtensionLifecycleProfile::Always
                | ExtensionLifecycleProfile::PiAggregate
        );
        if requires_workspace_sharing && self.sharing != ExtensionRuntimeSharing::Workspace {
            return Err(ExtensionRuntimeError::InvalidManifest(
                "runtime lifecycle requires explicit sharing = \"workspace\"".into(),
            ));
        }
        if matches!(
            self.lifecycle,
            ExtensionLifecycleProfile::LegacyResident
                | ExtensionLifecycleProfile::OneShot
                | ExtensionLifecycleProfile::Session
        ) && self.sharing != ExtensionRuntimeSharing::Isolated
        {
            return Err(ExtensionRuntimeError::InvalidManifest(
                "legacy_resident, oneshot, and session runtimes must use sharing = \"isolated\""
                    .into(),
            ));
        }
        Ok(())
    }
}

/// Parsed `extension.toml` metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionManifest {
    /// Stable lowercase extension identifier.
    pub name: String,
    /// Semantic extension version.
    pub version: String,
    /// octet extension API required by the extension.
    pub api_version: String,
    /// Optional octet version requirement carried by installable bundles.
    ///
    /// Locally authored, unpackaged extensions may omit this field. When it is
    /// present, discovery rejects a manifest that does not match this octet
    /// binary; the bundle installer applies the stricter exact-version rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_octet: Option<String>,
    /// Optional human-readable summary.
    #[serde(default)]
    pub description: Option<String>,
    /// Process launch configuration.
    pub entrypoint: ExtensionEntrypoint,
    /// Privileges requested by the extension.
    #[serde(default)]
    pub capabilities: ExtensionCapabilities,
    /// Typed contribution points declared by the extension.
    #[serde(default)]
    pub contributes: ManifestContributions,
    /// Optional process-fleet lifecycle and explicit sharing policy.
    #[serde(default)]
    pub runtime: ExtensionRuntimeSettings,
}

impl ExtensionManifest {
    /// Parses and validates a TOML manifest string.
    pub fn parse(source: &str) -> Result<Self, ExtensionRuntimeError> {
        let manifest: Self = toml::from_str(source)
            .map_err(|error| ExtensionRuntimeError::ManifestParse(error.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Reads and validates a manifest with the default 64 KiB bound.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ExtensionRuntimeError> {
        Self::load_bounded(path, DEFAULT_EXTENSION_MANIFEST_BYTES)
    }

    /// Reads and validates a manifest without ever buffering more than
    /// `max_bytes + 1` bytes.
    pub fn load_bounded(
        path: impl AsRef<Path>,
        max_bytes: u64,
    ) -> Result<Self, ExtensionRuntimeError> {
        let path = path.as_ref();
        let metadata =
            std::fs::metadata(path).map_err(|error| ExtensionRuntimeError::ManifestIo {
                path: path.to_path_buf(),
                message: error.to_string(),
            })?;
        if metadata.len() > max_bytes {
            return Err(ExtensionRuntimeError::ManifestTooLarge {
                path: path.to_path_buf(),
                bytes: metadata.len(),
                limit: max_bytes,
            });
        }

        let file = File::open(path).map_err(|error| ExtensionRuntimeError::ManifestIo {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
        let mut bytes = Vec::with_capacity(metadata.len().min(max_bytes) as usize);
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| ExtensionRuntimeError::ManifestIo {
                path: path.to_path_buf(),
                message: error.to_string(),
            })?;
        if bytes.len() as u64 > max_bytes {
            return Err(ExtensionRuntimeError::ManifestTooLarge {
                path: path.to_path_buf(),
                bytes: bytes.len() as u64,
                limit: max_bytes,
            });
        }
        let source = std::str::from_utf8(&bytes).map_err(|_| {
            ExtensionRuntimeError::InvalidManifest("manifest is not valid UTF-8".into())
        })?;
        Self::parse(source)
    }

    /// Validates identifiers, versions, launch data, and contribution lists.
    pub fn validate(&self) -> Result<(), ExtensionRuntimeError> {
        self.runtime.validate()?;
        validate_identifier("extension name", &self.name, false)?;
        semver::Version::parse(&self.version).map_err(|error| {
            ExtensionRuntimeError::InvalidManifest(format!(
                "version `{}` is not semantic versioning: {error}",
                self.version
            ))
        })?;
        if !api_v03::runtime_supports_api_version(&self.api_version) {
            return Err(ExtensionRuntimeError::UnsupportedApiVersion {
                extension: self.api_version.clone(),
                host: "0.1, 0.2, 0.3, or 0.4".into(),
            });
        }
        if self.runtime.sharing == ExtensionRuntimeSharing::Workspace
            && self.api_version == EXTENSION_API_VERSION_0_1
        {
            return Err(ExtensionRuntimeError::InvalidManifest(
                "workspace sharing requires extension API 0.2 or later resource-owner fences"
                    .into(),
            ));
        }
        if let Some(requires_octet) = &self.requires_octet {
            let requirement = semver::VersionReq::parse(requires_octet).map_err(|error| {
                ExtensionRuntimeError::InvalidManifest(format!(
                    "requires_octet `{requires_octet}` is not a semantic version requirement: {error}"
                ))
            })?;
            let host = semver::Version::parse(env!("CARGO_PKG_VERSION")).map_err(|error| {
                ExtensionRuntimeError::InvalidManifest(format!(
                    "host version is not semantic versioning: {error}"
                ))
            })?;
            if !requirement.matches(&host) {
                return Err(ExtensionRuntimeError::InvalidManifest(format!(
                    "extension requires octet `{requires_octet}`, but this binary is {host}"
                )));
            }
        }
        if self.api_version == EXTENSION_API_VERSION_0_1 && self.contributes.presentation {
            return Err(ExtensionRuntimeError::InvalidManifest(
                "semantic presentation requires extension API 0.2".into(),
            ));
        }
        if self.contributes.menu {
            if self.api_version == EXTENSION_API_VERSION_0_1 {
                return Err(ExtensionRuntimeError::InvalidManifest(
                    "extension menus require extension API 0.2".into(),
                ));
            }
            if self.contributes.commands.is_empty() {
                return Err(ExtensionRuntimeError::InvalidManifest(
                    "an extension menu needs at least one declared command to route to".into(),
                ));
            }
        }
        if self.api_version == EXTENSION_API_VERSION_0_1 && !self.contributes.shortcuts.is_empty() {
            return Err(ExtensionRuntimeError::InvalidManifest(
                "shortcuts require extension API 0.2".into(),
            ));
        }
        if !self.contributes.flags.is_empty() && self.api_version == EXTENSION_API_VERSION_0_1 {
            return Err(ExtensionRuntimeError::InvalidManifest(
                "CLI flags require extension API 0.2 or later".into(),
            ));
        }
        if self.api_version == EXTENSION_API_VERSION_0_1
            && self.contributes.hooks.iter().any(|hook| {
                matches!(
                    hook,
                    ExtensionHook::ProviderRetry
                        | ExtensionHook::BeforePersistence
                        | ExtensionHook::PostMutation
                )
            })
        {
            return Err(ExtensionRuntimeError::InvalidManifest(
                "provider_retry, before_persistence, and post_mutation hooks require extension API 0.2"
                    .into(),
            ));
        }
        if self.api_version != EXTENSION_API_VERSION_0_4
            && self
                .contributes
                .hooks
                .iter()
                .any(|hook| hook.is_session_operation())
        {
            return Err(ExtensionRuntimeError::InvalidManifest(
                "session operation hooks require extension API 0.4".into(),
            ));
        }
        if self.api_version != EXTENSION_API_VERSION_0_4
            && self
                .contributes
                .hooks
                .iter()
                .any(|hook| hook.is_provider_pipeline())
        {
            return Err(ExtensionRuntimeError::InvalidManifest(
                "provider pipeline hooks require extension API 0.4".into(),
            ));
        }
        if self
            .contributes
            .hooks
            .contains(&ExtensionHook::CompactionStrategy)
            && self.api_version != EXTENSION_API_VERSION_0_4
        {
            return Err(ExtensionRuntimeError::InvalidManifest(
                "compaction_strategy requires extension API 0.4".into(),
            ));
        }
        if self
            .contributes
            .hooks
            .contains(&ExtensionHook::CacheWarmingDecision)
            && self.api_version != EXTENSION_API_VERSION_0_4
        {
            return Err(ExtensionRuntimeError::InvalidManifest(
                "cache_warming_decision requires extension API 0.4".into(),
            ));
        }
        if self
            .contributes
            .hooks
            .contains(&ExtensionHook::ProviderContext)
            && self.api_version != EXTENSION_API_VERSION_0_4
        {
            return Err(ExtensionRuntimeError::InvalidManifest(
                "provider_context requires extension API 0.4".into(),
            ));
        }
        if self
            .contributes
            .hooks
            .contains(&ExtensionHook::ResourcesDiscover)
            && self.api_version != EXTENSION_API_VERSION_0_4
        {
            return Err(ExtensionRuntimeError::InvalidManifest(
                "resources_discover requires extension API 0.4".into(),
            ));
        }
        if self.api_version == EXTENSION_API_VERSION_0_1 && self.contributes.providers {
            return Err(ExtensionRuntimeError::InvalidManifest(
                "provider catalogs require extension API 0.2 or later".into(),
            ));
        }
        let declares_session_start = self
            .contributes
            .hooks
            .contains(&ExtensionHook::SessionStart);
        let declares_session_end = self.contributes.hooks.contains(&ExtensionHook::SessionEnd);
        if self.api_version == EXTENSION_API_VERSION_0_1
            && (declares_session_start || declares_session_end)
        {
            return Err(ExtensionRuntimeError::InvalidManifest(
                "session_start and session_end hooks require extension API 0.2 or later".into(),
            ));
        }
        if self.entrypoint.command.trim().is_empty()
            || self.entrypoint.command.chars().any(char::is_control)
        {
            return Err(ExtensionRuntimeError::InvalidManifest(
                "entrypoint.command must be non-empty and contain no control characters".into(),
            ));
        }
        for argument in &self.entrypoint.args {
            if argument.contains('\0') {
                return Err(ExtensionRuntimeError::InvalidManifest(
                    "entrypoint arguments cannot contain NUL".into(),
                ));
            }
        }
        for (name, value) in &self.entrypoint.env {
            if !valid_environment_name(name) || value.contains('\0') {
                return Err(ExtensionRuntimeError::InvalidManifest(format!(
                    "invalid entrypoint environment variable `{name}`"
                )));
            }
        }
        validate_identifiers("tool", &self.contributes.tools, true)?;
        validate_identifiers("command", &self.contributes.commands, true)?;
        validate_shortcut_definitions(&self.contributes.shortcuts)
            .map_err(ExtensionRuntimeError::InvalidManifest)?;
        validate_identifiers("tool renderer", &self.contributes.tool_renderers, true)?;
        validate_extension_flags(&self.contributes.flags)?;
        validate_identifiers("secret", &self.capabilities.secrets, true)?;
        validate_identifiers(
            "brokered environment variable",
            &self.capabilities.environment,
            true,
        )?;
        for name in &self.capabilities.environment {
            if !BROKERED_EXTENSION_ENVIRONMENT.contains(&name.as_str()) {
                return Err(ExtensionRuntimeError::InvalidManifest(format!(
                    "unsupported brokered environment variable `{name}`"
                )));
            }
        }
        if self.api_version == EXTENSION_API_VERSION_0_1
            && !self.capabilities.environment.is_empty()
        {
            return Err(ExtensionRuntimeError::InvalidManifest(
                "brokered environment variables require extension API 0.2".into(),
            ));
        }
        validate_unique("hook", &self.contributes.hooks)?;
        validate_unique("UI contribution", &self.contributes.ui)?;
        Ok(())
    }
}

/// Process launch configuration from an extension manifest.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionEntrypoint {
    /// Executable name or path. A bare name found beside the manifest wins
    /// over `PATH`, which makes self-contained extension folders convenient.
    pub command: String,
    /// Arguments passed directly without shell interpretation.
    #[serde(default)]
    pub args: Vec<String>,
    /// Additional environment variables for this child only.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Privileges declared by an executable extension.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionCapabilities {
    /// Filesystem scope requested by the extension.
    #[serde(default)]
    pub filesystem: ExtensionFilesystemAccess,
    /// Whether the extension intends to launch additional processes.
    #[serde(default)]
    pub process: bool,
    /// Whether the extension intends to access the network.
    #[serde(default)]
    pub network: bool,
    /// Exact logical secret names this extension may request from a configured
    /// host broker. An empty list disables secret negotiation for the process.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
    /// Narrow ambient variables explicitly brokered from the host environment.
    /// Only host-reviewed non-value names such as `SSH_AUTH_SOCK` are accepted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environment: Vec<String>,
    /// Whether this extension may read the composed host system prompt.
    ///
    /// `system_prompt_read` is offered and negotiable only when this is declared,
    /// because the reply discloses host-owned prompt text (project context,
    /// skills, injected file contents) across the process boundary. An
    /// extension that does not declare it cannot negotiate the feature at all.
    #[serde(default, skip_serializing_if = "is_false")]
    pub system_prompt: bool,
}

/// Filesystem access declared by an extension.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionFilesystemAccess {
    /// The extension declares no filesystem access.
    #[default]
    None,
    /// The extension needs files under the active workspace.
    Workspace,
    /// The extension asks for unrestricted user-level filesystem access.
    Unrestricted,
}

/// One typed command-line flag declared by an API `0.3` extension.
///
/// The long option spelling is `--{name}`. Boolean flags additionally receive
/// the explicit inverse spelling `--no-{name}` from the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionFlag {
    /// Stable global CLI long-option name.
    pub name: String,
    /// Runtime value type and parser selected by the host.
    #[serde(rename = "type")]
    pub kind: ExtensionFlagType,
    /// Required typed fallback used when the invocation omits this flag.
    pub default: serde_json::Value,
    /// Optional short help text shown in `octet --help`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Types supported by extension-declared command-line flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExtensionFlagType {
    /// A true/false switch exposed as both `--name` and `--no-name`.
    #[serde(rename = "boolean")]
    Boolean,
    /// One bounded UTF-8 command-line value.
    #[serde(rename = "string")]
    String,
    /// One signed portable JSON integer command-line value.
    #[serde(rename = "integer")]
    Integer,
}

/// Contribution names declared in `extension.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestContributions {
    /// Typed CLI flags registered before extension startup (API `0.3` only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<ExtensionFlag>,
    /// Model-callable tool names.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Slash-command names.
    #[serde(default)]
    pub commands: Vec<String>,
    /// Global terminal shortcuts, bound only after host validation.
    #[serde(default)]
    pub shortcuts: Vec<ShortcutDefinition>,
    /// Agent lifecycle hooks.
    #[serde(default)]
    pub hooks: Vec<ExtensionHook>,
    /// Semantic terminal surfaces.
    #[serde(default)]
    pub ui: Vec<ExtensionUiSurface>,
    /// Whether the extension can contribute prompt context.
    #[serde(default)]
    pub context: bool,
    /// Tool names for which the extension supplies semantic render output.
    #[serde(default)]
    pub tool_renderers: Vec<String>,
    /// Whether the process may emit user-visible notifications.
    #[serde(default)]
    pub notifications: bool,
    /// Whether the process may request interactive confirmation.
    #[serde(default)]
    pub confirmations: bool,
    /// Whether API `0.2` semantic presentation snapshots may arrive.
    #[serde(default, skip_serializing_if = "is_false")]
    pub presentation: bool,
    /// Whether the extension answers `menu/collect` with its `/extensions`
    /// options menu (API `0.2`/`0.4`). Items route to declared commands.
    #[serde(default, skip_serializing_if = "is_false")]
    pub menu: bool,
    /// Whether this API `0.3` extension may register a lifecycle-owned,
    /// secret-free provider/model catalog.
    #[serde(default, skip_serializing_if = "is_false")]
    pub providers: bool,
}

/// Supported extension lifecycle hooks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionHook {
    /// Supplies temporary filesystem resource roots to a real host loader.
    /// Requires API 0.4 and the resource_paths_v1 consumer.
    ResourcesDiscover,
    /// Prepares canonical model-visible context with a run-owned private append
    /// consumer; requires API 0.4 and negotiated session_entries.
    ProviderContext,
    /// Mutate the real codec-produced HTTP body before dispatch (API 0.4).
    BeforeProviderRequest,
    /// Patch non-reserved headers before authoritative authentication (API 0.4).
    BeforeProviderHeaders,
    /// Observe actual HTTP status and headers before body consumption (API 0.4).
    AfterProviderResponse,
    /// Observe the start of one actual model iteration with an awaited leaf consumer.
    ModelTurnStart,
    /// Observe the durable assistant and settled tool results for one model iteration.
    ModelTurnEnd,
    /// Await a cancellable decision before a prepared local compaction.
    SessionBeforeCompact,
    /// Observe the actual durable compaction record.
    SessionCompact,
    /// Await a cancellable decision before a durable tree checkout.
    SessionBeforeTree,
    /// Observe an actual durable tree checkout.
    SessionTree,
    /// Runs immediately before prompt composition.
    BeforePrompt,
    /// Runs after a complete assistant response.
    AfterResponse,
    /// Runs before a tool is dispatched.
    BeforeToolCall,
    /// Runs after a tool result is available.
    AfterToolCall,
    /// Advises on a host-admitted provider retry without changing retry safety
    /// or the host retry budget.
    ProviderRetry,
    /// Advises on one due refresh without changing deadlines, budget, or
    /// provider replay eligibility. Available only on API 0.4.
    CacheWarmingDecision,
    /// Replaces the parent-model local compaction call on vision routes.
    CompactionStrategy,
    /// Proposes one namespaced metadata value for a completed assistant turn
    /// before its atomic durable persistence boundary.
    BeforePersistence,
    /// Observes a completed host resource mutation and may request a bounded
    /// rescan of its declared affected resource identifiers.
    PostMutation,
    /// Runs once for an API `0.3` declared session-hook binding.
    SessionStart,
    /// Runs once when an API `0.3` declared session-hook binding settles.
    SessionEnd,
}

impl ExtensionHook {
    pub(super) fn is_session_operation(self) -> bool {
        matches!(
            self,
            Self::ModelTurnStart
                | Self::ModelTurnEnd
                | Self::SessionBeforeCompact
                | Self::SessionCompact
                | Self::SessionBeforeTree
                | Self::SessionTree
        )
    }

    pub(super) fn is_provider_pipeline(self) -> bool {
        matches!(
            self,
            Self::BeforeProviderRequest | Self::BeforeProviderHeaders | Self::AfterProviderResponse
        )
    }

    pub(super) fn is_session_hook(self) -> bool {
        matches!(self, Self::SessionStart | Self::SessionEnd)
    }
}

/// Semantic terminal surfaces an extension may populate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionUiSurface {
    /// A compact status item.
    Status,
    /// The semantic header region.
    Header,
    /// The semantic footer region.
    Footer,
}

/// Where an extension manifest came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionSource {
    /// `.octet/extensions/` under the active workspace.
    Project,
    /// `~/.octet/extensions/` under the user's home directory.
    Global,
    /// A directory supplied explicitly by the caller.
    Explicit,
}

/// One extension search root. Roots are consulted in caller-provided order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionRoot {
    /// Directory whose direct children are extension directories.
    pub directory: PathBuf,
    /// Provenance attached to discovered manifests.
    pub source: ExtensionSource,
}

/// Returns the conventional project-first extension roots.
pub fn default_extension_roots(workspace: &Path, home: Option<&Path>) -> Vec<ExtensionRoot> {
    let mut roots = vec![ExtensionRoot {
        directory: workspace.join(".octet/extensions"),
        source: ExtensionSource::Project,
    }];
    if let Some(home) = home {
        roots.push(ExtensionRoot {
            directory: home.join(".octet/extensions"),
            source: ExtensionSource::Global,
        });
    }
    roots
}

/// A resolved manifest path ready for bounded loading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionManifestInput {
    /// Exact `extension.toml` path.
    pub path: PathBuf,
    /// Discovery provenance.
    pub source: ExtensionSource,
}

/// Severity of a non-fatal catalog diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionDiagnosticLevel {
    /// The entry was loaded but deserves attention.
    Warning,
    /// The entry could not be loaded.
    Error,
}

/// A path-scoped extension discovery or loading diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionDiagnostic {
    /// Severity of the diagnostic.
    pub level: ExtensionDiagnosticLevel,
    /// Path involved, when known.
    pub path: PathBuf,
    /// Human-readable explanation.
    pub message: String,
}

/// Scans direct child directories for [`EXTENSION_MANIFEST_FILENAME`].
/// Missing roots are normal and produce no diagnostic.
pub fn discover_extension_manifests(
    roots: &[ExtensionRoot],
) -> (Vec<ExtensionManifestInput>, Vec<ExtensionDiagnostic>) {
    let mut manifests = Vec::new();
    let mut diagnostics = Vec::new();
    for root in roots {
        let entries = match std::fs::read_dir(&root.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                diagnostics.push(ExtensionDiagnostic {
                    level: ExtensionDiagnosticLevel::Error,
                    path: root.directory.clone(),
                    message: error.to_string(),
                });
                continue;
            }
        };
        let mut paths = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => {
                    let manifest = entry.path().join(EXTENSION_MANIFEST_FILENAME);
                    if manifest.is_file() {
                        paths.push(manifest);
                    }
                }
                Err(error) => diagnostics.push(ExtensionDiagnostic {
                    level: ExtensionDiagnosticLevel::Warning,
                    path: root.directory.clone(),
                    message: error.to_string(),
                }),
            }
        }
        paths.sort();
        manifests.extend(paths.into_iter().map(|path| ExtensionManifestInput {
            path,
            source: root.source,
        }));
    }
    (manifests, diagnostics)
}

/// Trust state required before an executable manifest may launch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionTrust {
    /// Discovery alone never grants code-execution permission.
    #[default]
    Untrusted,
    /// Trusted by the host's current full-access policy or an explicit grant.
    Trusted,
}

/// Why a selected extension can or cannot start as a host process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionStartDecision {
    /// The user has not switched this extension on.
    Disabled,
    /// Project code cannot run before the workspace is trusted.
    NeedsWorkspaceTrust,
    /// The selected source has no host process authority grant.
    NeedsHostAuthority,
    /// Activation and host process authority permit startup.
    Allowed,
}

/// Explicit activation state for one discovered extension.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionActivation {
    /// Whether this extension is enabled.
    pub enabled: bool,
    /// Whether launching its executable is trusted.
    pub trust: ExtensionTrust,
}

/// Explicit enablement plus executable trust for the current host policy.
/// The default requires explicit trust: persistent name-only grants apply only
/// to the user's global extension directory; other sources need an exact path
/// or one-invocation grant. [`Self::for_effect_policy`] adds non-persistent
/// implicit trust only for full access, never implicit enablement.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtensionPolicy {
    pub(super) enabled: BTreeSet<String>,
    pub(super) trusted_global: BTreeSet<String>,
    pub(super) trusted_sources: BTreeSet<(String, PathBuf)>,
    pub(super) trusted_for_invocation: BTreeSet<String>,
    pub(super) implicit_trust: bool,
}

impl ExtensionPolicy {
    /// Creates an empty policy for the host's effective authority profile.
    ///
    /// Full access trusts selected sources without recording a grant. Rebuild
    /// this policy when authority changes; controlled profiles have no implicit
    /// host authority. Enablement, workspace trust, and source validation are
    /// independent requirements enforced by the host.
    pub fn for_effect_policy(effect_policy: EffectPolicy) -> Self {
        Self {
            implicit_trust: effect_policy == EffectPolicy::UnsafeHost,
            ..Self::default()
        }
    }

    /// Explicitly enables an extension name without changing its trust policy.
    pub fn enable(&mut self, name: impl Into<String>) {
        self.enabled.insert(name.into());
    }

    /// Persistently trusts a name from the user's global extension directory.
    /// This grant never transfers to a project or explicit manifest with the
    /// same name.
    pub fn trust(&mut self, name: impl Into<String>) {
        self.trusted_global.insert(name.into());
    }

    /// Persistently trusts one exact, normalized manifest path.
    pub fn trust_source(&mut self, name: impl Into<String>, manifest_path: impl Into<PathBuf>) {
        self.trusted_sources
            .insert((name.into(), manifest_path.into()));
    }

    /// Trusts whichever descriptor with this name was selected for the
    /// current process invocation. Frontends should expose this only through
    /// an explicit one-shot CLI/action boundary, never persistent config.
    pub fn trust_for_invocation(&mut self, name: impl Into<String>) {
        self.trusted_for_invocation.insert(name.into());
    }

    /// Removes an extension from the enabled set.
    pub fn disable(&mut self, name: &str) {
        self.enabled.remove(name);
    }

    /// Revokes explicit grants; this does not override full-access implicit trust.
    pub fn revoke_trust(&mut self, name: &str) {
        self.trusted_global.remove(name);
        self.trusted_for_invocation.remove(name);
        self.trusted_sources
            .retain(|(trusted_name, _)| trusted_name != name);
    }

    /// Returns the two independent decisions for one selected source.
    pub fn activation(
        &self,
        name: &str,
        manifest_path: &Path,
        source: ExtensionSource,
    ) -> ExtensionActivation {
        let source_bound = self
            .trusted_sources
            .contains(&(name.to_owned(), manifest_path.to_owned()));
        let trusted = self.implicit_trust
            || source == ExtensionSource::Explicit
            || self.trusted_for_invocation.contains(name)
            || source_bound
            || (source == ExtensionSource::Global && self.trusted_global.contains(name));
        ExtensionActivation {
            enabled: self.enabled.contains(name),
            trust: if trusted {
                ExtensionTrust::Trusted
            } else {
                ExtensionTrust::Untrusted
            },
        }
    }
}

impl ExtensionActivation {
    /// The host-process admission decision. The effect broker still governs
    /// tools, but cannot confine an extension process running with OS authority.
    pub fn start_decision(
        self,
        source: ExtensionSource,
        workspace_trusted: bool,
    ) -> ExtensionStartDecision {
        if !self.enabled {
            ExtensionStartDecision::Disabled
        } else if source == ExtensionSource::Project && !workspace_trusted {
            ExtensionStartDecision::NeedsWorkspaceTrust
        } else if self.trust != ExtensionTrust::Trusted {
            ExtensionStartDecision::NeedsHostAuthority
        } else {
            ExtensionStartDecision::Allowed
        }
    }
}

/// A valid manifest plus its provenance and activation decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredExtension {
    /// Validated manifest.
    pub manifest: ExtensionManifest,
    /// Exact manifest file used.
    pub manifest_path: PathBuf,
    /// Resource provenance.
    pub source: ExtensionSource,
    /// Explicit enablement and effective trust state.
    pub activation: ExtensionActivation,
}

impl DiscoveredExtension {
    pub(super) fn ensure_startable(&self) -> Result<(), ExtensionRuntimeError> {
        if !self.activation.enabled {
            return Err(ExtensionRuntimeError::Disabled(self.manifest.name.clone()));
        }
        if self.activation.trust != ExtensionTrust::Trusted {
            return Err(ExtensionRuntimeError::Untrusted(self.manifest.name.clone()));
        }
        Ok(())
    }
}

/// Loaded extension catalog. Invalid or shadowed entries are diagnostics,
/// allowing one bad tinkerer extension to leave the rest usable.
#[derive(Clone, Debug, Default)]
pub struct ExtensionCatalog {
    /// First valid manifest for each name, preserving input precedence.
    pub extensions: Vec<DiscoveredExtension>,
    /// Non-fatal load, validation, and duplicate diagnostics.
    pub diagnostics: Vec<ExtensionDiagnostic>,
}

impl ExtensionCatalog {
    /// Loads caller-resolved paths in order. The first manifest for a name
    /// wins, so a shared resource resolver can authoritatively set precedence.
    pub fn load_resolved(
        inputs: impl IntoIterator<Item = ExtensionManifestInput>,
        policy: &ExtensionPolicy,
        max_manifest_bytes: u64,
    ) -> Self {
        let mut catalog = Self::default();
        let mut names = BTreeMap::<String, PathBuf>::new();
        for input in inputs {
            match ExtensionManifest::load_bounded(&input.path, max_manifest_bytes) {
                Ok(manifest) => {
                    if let Some(first) = names.get(&manifest.name) {
                        catalog.diagnostics.push(ExtensionDiagnostic {
                            level: ExtensionDiagnosticLevel::Warning,
                            path: input.path,
                            message: format!(
                                "extension `{}` is shadowed by {}",
                                manifest.name,
                                first.display()
                            ),
                        });
                        continue;
                    }
                    names.insert(manifest.name.clone(), input.path.clone());
                    let activation = policy.activation(&manifest.name, &input.path, input.source);
                    catalog.extensions.push(DiscoveredExtension {
                        activation,
                        manifest,
                        manifest_path: input.path,
                        source: input.source,
                    });
                }
                Err(error) => catalog.diagnostics.push(ExtensionDiagnostic {
                    level: ExtensionDiagnosticLevel::Error,
                    path: input.path,
                    message: error.to_string(),
                }),
            }
        }
        catalog
    }
}

/// Convenience loader for manifest paths already resolved by another resource
/// system. Paths are tagged as [`ExtensionSource::Explicit`].
pub fn load_extension_manifest_paths<I, P>(
    paths: I,
    policy: &ExtensionPolicy,
    max_manifest_bytes: u64,
) -> ExtensionCatalog
where
    I: IntoIterator<Item = P>,
    P: Into<PathBuf>,
{
    ExtensionCatalog::load_resolved(
        paths.into_iter().map(|path| ExtensionManifestInput {
            path: path.into(),
            source: ExtensionSource::Explicit,
        }),
        policy,
        max_manifest_bytes,
    )
}
