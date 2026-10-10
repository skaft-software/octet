//! API 0.4 filesystem resource paths, not extension-local opaque resources.
use super::*;

/// Optional bounded path-discovery contract. Never offer without a loader consumer.
pub const EXTENSION_FEATURE_RESOURCE_PATHS: &str = "resource_paths_v1";
const MAX_PATHS: usize = 64;
const MAX_PATH_BYTES: usize = 4096;
const MAX_TOTAL_PATH_BYTES: usize = 64 * 1024;
const DISCOVERY_DEADLINE: Duration = Duration::from_secs(5);

/// Host-owned reason, mapped to Pi's resources_discover event without guessing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionResourceDiscoveryReason {
    /// Initial active-session resource binding, after session_start.
    Startup,
    /// An explicit resource/extension reload, after the replacement session_start.
    Reload,
}

/// One session-only default route selection reported by a resource contributor.
/// The product still resolves it against its own catalog and admission rules.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionDefaultModel {
    /// Pi-style provider id (for example `deepseek`).
    pub provider: String,
    /// Model id within that provider (for example `deepseek-v4-pro`).
    pub model: String,
}

/// Absolute, temporary roots. The product still owns filesystem authorization,
/// format parsing, precedence, diagnostics and publication of the new snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionResourcePaths {
    /// Skill directories or supported Markdown entrypoints.
    #[serde(default)]
    pub skill_paths: Vec<String>,
    /// Prompt directories or supported Markdown/TOML files.
    #[serde(default)]
    pub prompt_paths: Vec<String>,
    /// Native theme directories or supported TOML/Pi JSON files.
    #[serde(default)]
    pub theme_paths: Vec<String>,
    /// Optional session-only startup preference. Must be an exact file entry in
    /// theme_paths; host parsing, trust and explicit selections remain authoritative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_theme: Option<String>,
    /// Native keybinding JSON files whose overrides apply only while this
    /// resource binding is live. The product owns schema validation and merging.
    #[serde(default)]
    pub keybindings_paths: Vec<String>,
    /// Context/instruction Markdown files composed into this session's system
    /// prompt only while the resource binding is live.
    #[serde(default)]
    pub context_paths: Vec<String>,
    /// Optional session-only default model selection. Honored only when the
    /// invocation has no explicit model choice; never persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<ExtensionDefaultModel>,
    /// Optional exact portable Pi thinking preference. Session-only; explicit
    /// invocation reasoning and the native model's supported levels take priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_thinking_level: Option<String>,
}

impl ExtensionResourcePaths {
    /// Validate the untrusted response before any filesystem access. Bounds
    /// apply before deduplication; null, unknown fields and wrong types fail.
    pub fn validate(&self) -> Result<(), ExtensionRuntimeError> {
        if self
            .default_theme
            .as_ref()
            .is_some_and(|path| !self.theme_paths.contains(path))
        {
            return Err(ExtensionRuntimeError::Protocol(
                "default_theme must reference a contributed theme path".into(),
            ));
        }
        let all = self
            .skill_paths
            .iter()
            .chain(&self.prompt_paths)
            .chain(&self.theme_paths)
            .chain(&self.keybindings_paths)
            .chain(&self.context_paths);
        let mut count = 0;
        let mut bytes = 0;
        for path in all {
            count += 1;
            bytes += path.len();
            if count > MAX_PATHS
                || bytes > MAX_TOTAL_PATH_BYTES
                || path.is_empty()
                || path.len() > MAX_PATH_BYTES
                || path.chars().any(char::is_control)
                || !Path::new(path).is_absolute()
                || Path::new(path)
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(ExtensionRuntimeError::Protocol(
                    "invalid or over-budget resource_paths_v1 paths".into(),
                ));
            }
        }
        if let Some(selection) = &self.default_model {
            let bounded = |value: &str| {
                !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
            };
            if !bounded(&selection.provider) || !bounded(&selection.model) {
                return Err(ExtensionRuntimeError::Protocol(
                    "invalid resource_paths_v1 default_model".into(),
                ));
            }
        }
        if let Some(level) = &self.default_thinking_level {
            if !matches!(
                level.as_str(),
                "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
            ) {
                return Err(ExtensionRuntimeError::Protocol(
                    "invalid resource_paths_v1 default_thinking_level; use off, minimal, low, medium, high, xhigh, or max".into(),
                ));
            }
        }
        Ok(())
    }
}

/// Intentionally separate from ExtensionHookOutput: a discovery reply must not
/// silently discard a veto, context mutation, notification or other hook result.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourcePathsReply {
    resource_paths: ExtensionResourcePaths,
}

impl ExtensionProcess {
    /// Ask one negotiated process generation for temporary resource paths.
    /// The product must call after session_start settles, recheck the owner and
    /// generation immediately before publishing, and never persist these roots.
    pub async fn discover_resource_paths(
        &self,
        session_owner: &str,
        reason: ExtensionResourceDiscoveryReason,
    ) -> Result<(u64, ExtensionResourcePaths), ExtensionRuntimeError> {
        validate_session_hook_id(session_owner)?;
        let deadline = tokio::time::Instant::now() + DISCOVERY_DEADLINE;
        let _start_and_reload = tokio::time::timeout_at(deadline, self.inner.reload_guard.lock())
            .await
            .map_err(|_| {
                ExtensionRuntimeError::Protocol(
                    "resource discovery session-start wait timed out".into(),
                )
            })?;
        if self.declares_session_hooks() {
            let bindings = lock_std_mutex(&self.inner.session_hooks);
            let ready = bindings.get(session_owner).is_some_and(|binding| {
                binding.endpoint.generation == read_std_lock(&self.inner.connection).generation
                    && binding.start_outcome.succeeded()
            });
            if !ready {
                return Err(ExtensionRuntimeError::Protocol(
                    "resource discovery requires successful settled session_start".into(),
                ));
            }
        }
        if self.api_version() != EXTENSION_API_VERSION_0_4
            || !self
                .inner
                .contributions
                .hooks
                .contains(&ExtensionHook::ResourcesDiscover)
        {
            return Err(self.undeclared("hook", "resources_discover".into()));
        }
        let connection = read_std_lock(&self.inner.connection).clone();
        if !read_std_lock(&connection.protocol)
            .features
            .contains(EXTENSION_FEATURE_RESOURCE_PATHS)
        {
            return Err(ExtensionRuntimeError::Protocol(
                "resource_paths_v1 not negotiated".into(),
            ));
        }
        let generation = connection.generation;
        let mut context = self.execution_context();
        context.resource_owner = Some(ExtensionResourceOwner {
            session_id: session_owner.to_owned(),
            extension_instance_id: self.inner.instance_id.clone(),
            process_generation: generation,
        });
        let owner = context.resource_owner.clone();
        let request = serde_json::to_value(HookRequest {
            hook: ExtensionHook::ResourcesDiscover,
            payload: serde_json::json!({"cwd": context.workspace, "reason": reason}),
            context,
        })
        .map_err(|_| {
            ExtensionRuntimeError::Protocol("invalid resource discovery context".into())
        })?;
        let response = connection
            .request_with_resource_owner(
                methods::HOOK_RUN,
                request,
                self.inner
                    .config
                    .request_timeout
                    .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
                owner,
            )
            .await?;
        if read_std_lock(&self.inner.connection).generation != generation
            || connection.draining.load(Ordering::Acquire)
        {
            return Err(ExtensionRuntimeError::Closed(
                "stale resource discovery generation".into(),
            ));
        }
        let reply: ResourcePathsReply = serde_json::from_value(response).map_err(|_| {
            ExtensionRuntimeError::Protocol("invalid resources_discover result shape".into())
        })?;
        reply.resource_paths.validate()?;
        Ok((generation, reply.resource_paths))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> String {
        std::env::temp_dir()
            .join("resource-fixture")
            .display()
            .to_string()
    }
    #[test]
    fn resource_paths_require_api04_declaration_and_actual_consumer_offer() {
        let manifest = ExtensionManifest::parse("name = \"paths\"\nversion = \"0.1.0\"\napi_version = \"0.4\"\n[entrypoint]\ncommand = \"fixture\"\n[contributes]\nhooks = [\"resources_discover\"]\n").unwrap();
        for api in ["0.1", "0.2", "0.3"] {
            let mut old = manifest.clone();
            old.api_version = api.into();
            assert!(old.validate().is_err());
        }
        for offered in [false, true] {
            for selected in [false, true] {
                let mut features = API_0_2_REQUIRED_FEATURES.to_vec();
                if selected {
                    features.push(EXTENSION_FEATURE_RESOURCE_PATHS);
                }
                let response: InitializeResponse = serde_json::from_value(serde_json::json!({
                    "api_version": "0.4", "protocol": {"version": "0.4", "features": features,
                        "limits": {"max_concurrent_requests": 1}}
                }))
                .unwrap();
                assert_eq!(
                    negotiate_contributions_with_host_services(
                        &manifest,
                        response,
                        DEFAULT_PENDING_REQUESTS,
                        OfferedHostServices {
                            resource_paths: offered,
                            ..Default::default()
                        }
                    )
                    .is_ok(),
                    offered && selected
                );
            }
        }
    }
    #[test]
    fn resource_paths_wire_is_strict_and_counts_before_deduplication() {
        assert!(serde_json::from_value::<ResourcePathsReply>(serde_json::json!({})).is_err());
        assert!(
            serde_json::from_value::<ResourcePathsReply>(serde_json::json!({
                "resource_paths": {"skill_paths": null}
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ResourcePathsReply>(serde_json::json!({
                "resource_paths": {}, "disposition": {"action": "continue"}
            }))
            .is_err()
        );
        let mut paths = ExtensionResourcePaths {
            skill_paths: vec![root(); 64],
            ..Default::default()
        };
        paths.validate().unwrap();
        paths.prompt_paths.push(root());
        assert!(paths.validate().is_err());
    }
    #[test]
    fn resource_theme_default_must_reference_a_contributed_theme_path() {
        let theme = format!("{}/palette.toml", root());
        let reply: ResourcePathsReply = serde_json::from_value(serde_json::json!({
            "resource_paths": {"theme_paths": [theme], "default_theme": theme}
        }))
        .unwrap();
        reply.resource_paths.validate().unwrap();
        for invalid in ["dark", "", "/other/palette.toml"] {
            let reply: ResourcePathsReply = serde_json::from_value(serde_json::json!({
                "resource_paths": {"theme_paths": [theme], "default_theme": invalid}
            }))
            .unwrap();
            assert!(reply.resource_paths.validate().is_err());
        }
    }

    #[test]
    fn resource_paths_reject_relative_controls_parent_and_byte_overflow() {
        for bad in [
            "".into(),
            "relative/skills".into(),
            format!("{}/../other", root()),
            format!("{}/\u{009b}", root()),
            format!("{}/{}", root(), "é".repeat(2048)),
        ] {
            let paths = ExtensionResourcePaths {
                skill_paths: vec![bad],
                ..Default::default()
            };
            assert!(paths.validate().is_err());
        }
        let paths = ExtensionResourcePaths {
            skill_paths: vec![format!("{}/{}", root(), "x".repeat(2048)); 64],
            ..Default::default()
        };
        assert!(paths.validate().is_err());
    }

    #[test]
    fn default_thinking_wire_is_optional_strict_and_bounded() {
        let empty = serde_json::to_value(ExtensionResourcePaths::default()).unwrap();
        assert!(empty.get("default_thinking_level").is_none());
        for level in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] {
            let reply: ResourcePathsReply = serde_json::from_value(serde_json::json!({
                "resource_paths": {"default_thinking_level": level}
            }))
            .unwrap();
            reply.resource_paths.validate().unwrap();
            assert_eq!(
                reply.resource_paths.default_thinking_level.as_deref(),
                Some(level)
            );
            assert_eq!(
                serde_json::to_value(reply.resource_paths).unwrap()["default_thinking_level"],
                level
            );
        }
        for level in [
            "", "on", "ultra", "MAX", " max", "max\n", "x-high", "min", "maximum",
        ] {
            let paths = ExtensionResourcePaths {
                default_thinking_level: Some(level.into()),
                ..Default::default()
            };
            assert!(paths.validate().is_err(), "{level:?}");
        }
        let oversized = ExtensionResourcePaths {
            default_thinking_level: Some("x".repeat(4097)),
            ..Default::default()
        };
        assert!(oversized.validate().is_err());
        for value in [
            serde_json::json!(123),
            serde_json::json!(["max"]),
            serde_json::json!({"level": "max"}),
        ] {
            assert!(
                serde_json::from_value::<ResourcePathsReply>(serde_json::json!({
                    "resource_paths": {"default_thinking_level": value}
                }))
                .is_err()
            );
        }
        assert!(
            serde_json::from_value::<ResourcePathsReply>(serde_json::json!({
                "resource_paths": {"default_thinkingLevel": "max"}
            }))
            .is_err()
        );
    }

    #[test]
    fn session_only_keybindings_context_and_default_model_validate_and_share_the_path_budget() {
        let theme = format!("{}/theme.json", root());
        let valid = ExtensionResourcePaths {
            theme_paths: vec![theme.clone()],
            default_theme: Some(theme),
            keybindings_paths: vec![format!("{}/keybindings.json", root())],
            context_paths: vec![format!("{}/AGENTS.md", root())],
            default_model: Some(ExtensionDefaultModel {
                provider: "deepseek".into(),
                model: "deepseek-v4-pro".into(),
            }),
            ..Default::default()
        };
        assert!(valid.validate().is_ok());
        // Every path kind shares one bound; the new kinds cannot bypass it.
        let oversized = ExtensionResourcePaths {
            skill_paths: vec![format!("{}/skill", root()); 63],
            keybindings_paths: vec![format!("{}/keybindings.json", root())],
            context_paths: vec![format!("{}/AGENTS.md", root())],
            ..Default::default()
        };
        assert!(oversized.validate().is_err());
        for paths in [
            ExtensionResourcePaths {
                keybindings_paths: vec![format!("{}/../escape.json", root())],
                ..Default::default()
            },
            ExtensionResourcePaths {
                context_paths: vec!["relative/AGENTS.md".into()],
                ..Default::default()
            },
        ] {
            assert!(paths.validate().is_err());
        }
        for selection in [
            ExtensionDefaultModel {
                provider: String::new(),
                model: "deepseek-v4-pro".into(),
            },
            ExtensionDefaultModel {
                provider: "deepseek".into(),
                model: "x".repeat(257),
            },
            ExtensionDefaultModel {
                provider: "deep\nseek".into(),
                model: "deepseek-v4-pro".into(),
            },
        ] {
            let paths = ExtensionResourcePaths {
                default_model: Some(selection),
                ..Default::default()
            };
            assert!(paths.validate().is_err());
        }
    }
}
