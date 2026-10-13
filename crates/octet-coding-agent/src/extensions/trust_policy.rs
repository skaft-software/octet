//! Extension start policy, trust and host-authority grants, and descriptor loading.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ConfiguredTrustGrant {
    Global { name: String },
    Exact { name: String, path: PathBuf },
}

impl ConfiguredTrustGrant {
    pub(super) fn matches(&self, descriptor: &DiscoveredExtension) -> bool {
        match self {
            Self::Global { name } => {
                descriptor.source == ExtensionSource::Global && descriptor.manifest.name == *name
            }
            Self::Exact { name, path } => {
                descriptor.manifest.name == *name && descriptor.manifest_path == *path
            }
        }
    }

    pub(super) fn display(&self) -> String {
        match self {
            Self::Global { name } => name.clone(),
            Self::Exact { name, path } => format!("{name}@{}", path.display()),
        }
    }
}

pub(super) fn extension_policy(
    config: &Config,
    diagnostics: &mut Vec<String>,
) -> (ExtensionPolicy, Vec<ConfiguredTrustGrant>) {
    // Implicit full-access trust is derived anew, never copied into either
    // persistent or invocation-specific grants in the product configuration.
    let mut policy = ExtensionPolicy::for_effect_policy(config.effect_policy);
    for name in &config.enabled_extensions {
        policy.enable(name.clone());
    }

    let mut grants = Vec::new();
    for grant in &config.trusted_extensions {
        if let Some((name, path)) = grant.split_once('@') {
            match normalize_trusted_manifest_path(Path::new(path)) {
                Ok(path) => {
                    policy.trust_source(name.to_owned(), path.clone());
                    grants.push(ConfiguredTrustGrant::Exact {
                        name: name.to_owned(),
                        path,
                    });
                }
                Err(error) => diagnostics.push(format!(
                    "warning: invalid source-bound extension trust grant {grant:?}: {error}"
                )),
            }
        } else {
            policy.trust(grant.clone());
            grants.push(ConfiguredTrustGrant::Global {
                name: grant.clone(),
            });
        }
    }
    for name in &config.invocation_trusted_extensions {
        policy.trust_for_invocation(name.clone());
    }
    (policy, grants)
}

/// Returns a copy of the product configuration narrowed to extensions that can
/// statically negotiate API 0.3 provider catalogs.
///
/// Provider discovery is the only reason bootstrap may activate an extension
/// before the final session/model exists. Keep that exceptional process set
/// narrow: ordinary tools, hooks, and UI extensions must first observe the
/// selected real launch state.
pub(crate) fn provider_preflight_config(config: &Config) -> Config {
    let resolver = ResourceResolver::new(config.workspace.clone(), config.workspace_trusted);
    let snapshot = resolver.discover(ResourceKind::Extension, &config.extension_paths);
    let mut diagnostics = Vec::new();
    let (policy, _) = extension_policy(config, &mut diagnostics);
    let mut descriptors = BTreeMap::<String, DiscoveredExtension>::new();
    for resource in snapshot.resources() {
        let Some(descriptor) =
            load_extension_descriptor(&resolver, resource, &policy, &mut diagnostics)
        else {
            continue;
        };
        descriptors
            .entry(descriptor.manifest.name.clone())
            .or_insert(descriptor);
    }
    let provider_names = descriptors
        .into_values()
        .filter(|descriptor| {
            descriptor.activation.enabled
                && descriptor.manifest.api_version == EXTENSION_API_VERSION_0_3
                && descriptor.manifest.contributes.providers
        })
        .map(|descriptor| descriptor.manifest.name)
        .collect::<BTreeSet<_>>();
    let mut preflight = config.clone();
    preflight
        .enabled_extensions
        .retain(|name| provider_names.contains(name));
    preflight
}

pub(super) fn normalize_trusted_manifest_path(path: &Path) -> anyhow::Result<PathBuf> {
    if !path.is_absolute() {
        anyhow::bail!("the manifest path must be absolute");
    }
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("the manifest path has no file name"))?;
    if file_name != EXTENSION_MANIFEST_FILENAME {
        anyhow::bail!("the manifest path must end in {EXTENSION_MANIFEST_FILENAME}");
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("the manifest path has no parent directory"))?
        .canonicalize()
        .with_context(|| format!("cannot normalize manifest parent for {}", path.display()))?;
    Ok(parent.join(file_name))
}

pub(super) fn persistent_trust_grant(descriptor: &DiscoveredExtension) -> String {
    persistent_host_authority_grant(
        descriptor.source,
        &descriptor.manifest.name,
        &descriptor.manifest_path,
    )
}

pub(crate) fn persistent_host_authority_grant(
    source: ExtensionSource,
    name: &str,
    manifest_path: &Path,
) -> String {
    if source == ExtensionSource::Global {
        name.to_owned()
    } else {
        format!("{name}@{}", manifest_path.display())
    }
}

pub(super) fn sha256_manifest(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => format!("{:x}", Sha256::digest(&bytes)),
        Err(_) => "unavailable".to_owned(),
    }
}

pub(super) fn installed_bundle_digest(manifest_path: &Path) -> Option<String> {
    let install_path = manifest_path.parent()?.join("install.json");
    let bytes = std::fs::read(&install_path).ok()?;
    if bytes.len() > 64 * 1024 {
        return None;
    }
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let digest = value.get("archive_sha256")?.as_str()?;
    (digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| digest.to_ascii_lowercase())
}

pub(super) fn extension_compatibility(
    name: &str,
    running: bool,
    features: &[String],
    health: Option<&ExtensionHealthSnapshot>,
) -> (Option<String>, String) {
    if name != SUBAGENTS_EXTENSION_NAME {
        return (None, "not_applicable".to_owned());
    }
    if features
        .iter()
        .any(|feature| feature == EXTENSION_FEATURE_DELEGATION_TELEMETRY)
    {
        return (
            Some(DELEGATION_TELEMETRY_SCHEMA.to_owned()),
            "compatible".to_owned(),
        );
    }
    if running {
        return (
            None,
            "incompatible: delegation telemetry was not negotiated; rebuild/reinstall the current workspace bundle"
                .to_owned(),
        );
    }
    let error = health
        .and_then(|health| health.last_error.as_deref())
        .unwrap_or_default();
    if error.contains(EXTENSION_FEATURE_DELEGATION_TELEMETRY)
        || error.contains("required API 0.2 features")
    {
        (
            None,
            "incompatible: rebuild/reinstall the current workspace bundle".to_owned(),
        )
    } else {
        (None, "unavailable".to_owned())
    }
}

/// Read only selected, trusted manifest metadata for CLI construction.
///
/// This deliberately shares the runtime resolver and policy calculation but
/// never starts or imports an extension process.
pub(crate) fn selected_extension_flag_declarations(
    config: &Config,
) -> Vec<(String, ExtensionFlag)> {
    let resolver = ResourceResolver::new(config.workspace.clone(), config.workspace_trusted);
    let snapshot = resolver.discover(ResourceKind::Extension, &config.extension_paths);
    let mut diagnostics = Vec::new();
    let (policy, _) = extension_policy(config, &mut diagnostics);
    let mut by_name = BTreeMap::<String, DiscoveredExtension>::new();
    for resource in snapshot.resources() {
        let Some(descriptor) =
            load_extension_descriptor(&resolver, resource, &policy, &mut diagnostics)
        else {
            continue;
        };
        by_name
            .entry(descriptor.manifest.name.clone())
            .or_insert(descriptor);
    }
    by_name
        .into_values()
        .filter(|descriptor| {
            descriptor
                .activation
                .start_decision(descriptor.source, config.workspace_trusted)
                == ExtensionStartDecision::Allowed
                && config.start_extension_processes
                && config.sandbox.process_execution_allowed()
        })
        .flat_map(|descriptor| {
            let name = descriptor.manifest.name;
            descriptor
                .manifest
                .contributes
                .flags
                .into_iter()
                .map(move |flag| (name.clone(), flag))
        })
        .collect()
}

pub(super) fn load_extension_descriptor(
    resolver: &ResourceResolver,
    resource: &ResolvedResource,
    policy: &ExtensionPolicy,
    diagnostics: &mut Vec<String>,
) -> Option<DiscoveredExtension> {
    let manifest = match resolver
        .read_text(resource)
        .and_then(|source| ExtensionManifest::parse(&source).map_err(anyhow::Error::from))
    {
        Ok(manifest) => manifest,
        Err(error) => {
            diagnostics.push(format!("error: {}: {error}", resource.path.display()));
            return None;
        }
    };
    if resource.name != manifest.name {
        diagnostics.push(format!(
            "warning: {}: extension directory name {:?} must match manifest name {:?}; ignored",
            resource.path.display(),
            resource.name,
            manifest.name
        ));
        return None;
    }
    let source = match resource.scope {
        ResourceScope::Global => ExtensionSource::Global,
        ResourceScope::Project => ExtensionSource::Project,
        ResourceScope::Explicit => ExtensionSource::Explicit,
    };
    Some(DiscoveredExtension {
        activation: policy.activation(&manifest.name, &resource.path, source),
        manifest,
        manifest_path: resource.path.clone(),
        source,
    })
}
