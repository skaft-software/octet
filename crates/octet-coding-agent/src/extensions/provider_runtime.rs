//! Providers served by extensions and their catalog projection.

use super::*;

/// Product-owned authorization boundary for API 0.3 extension providers.
///
/// The coding agent does not currently expose a credential or OAuth setup
/// surface for extension providers. Explicitly park those routes instead of
/// allowing an extension declaration to imply credential authority. The
/// registry handles unauthenticated providers without consulting this policy.
#[derive(Default)]
pub(super) struct CodingAgentProviderAuthorizationPolicy;

impl ExtensionProviderAuthorizationPolicy for CodingAgentProviderAuthorizationPolicy {
    fn authorize(
        &self,
        _owner: &ExtensionProviderOwner,
        _provider: &octet_agent::extension_api_v03::ProviderDefinition,
        request: &octet_agent::extension_api_v03::ProviderAuthorizationRequest,
    ) -> octet_agent::extension_api_v03::ProviderAuthorizationResult {
        octet_agent::extension_api_v03::ProviderAuthorizationResult {
            status: if request.action == "revoke" {
                "revoked"
            } else {
                "unavailable"
            }
            .to_owned(),
            lease: None,
        }
    }
}

#[derive(Default)]
pub(super) struct ProviderCatalogProjection {
    pub(super) revision: Option<usize>,
    /// Model ids the last synchronization attempted to project. A desired
    /// model the catalog refused (for example a conflicting identifier) stays
    /// here so an unchanged registry is never re-projected on every boundary.
    pub(super) desired: BTreeSet<String>,
    pub(super) routes: BTreeMap<String, EndpointId>,
    pub(super) problems: Vec<String>,
}

#[derive(Default)]
pub(crate) struct ProviderCatalogReport {
    pub checked: bool,
    pub problems: Vec<String>,
    pub details: Vec<String>,
}

impl ProviderCatalogReport {
    pub(super) fn into_notices(self) -> Vec<String> {
        if self.checked {
            self.problems.into_iter().chain(self.details).collect()
        } else {
            Vec::new()
        }
    }
}

/// Shared owner for extension-provider declarations and their local catalog
/// projection. The registry itself stores only secret-free declarations; this
/// product layer synthesizes opaque local endpoints and host stream routes.
#[derive(Clone)]
pub(crate) struct ExtensionProviderRuntime {
    pub(super) registry: Arc<ExtensionProviderRegistry>,
    pub(super) projection: Arc<Mutex<ProviderCatalogProjection>>,
}

impl Default for ExtensionProviderRuntime {
    fn default() -> Self {
        Self {
            registry: Arc::new(ExtensionProviderRegistry::with_authorization_policy(
                Arc::new(CodingAgentProviderAuthorizationPolicy),
            )),
            projection: Arc::new(Mutex::new(ProviderCatalogProjection::default())),
        }
    }
}

impl ExtensionProviderRuntime {
    pub(super) fn registry(&self) -> Arc<ExtensionProviderRegistry> {
        Arc::clone(&self.registry)
    }

    /// Returns every recorded declaration owned by one extension instance,
    /// paired with whether the owning generation's initial batch completed.
    pub(super) fn recorded_providers_for(
        &self,
        instance_id: &str,
    ) -> Vec<(ExtensionProviderCatalogEntry, bool)> {
        self.registry
            .recorded_providers()
            .into_iter()
            .filter(|(entry, _)| entry.owner.extension_instance_id == instance_id)
            .collect()
    }

    pub(super) fn initial_provider_owners(
        processes: &[ExtensionProcess],
    ) -> Vec<ExtensionProviderOwner> {
        processes
            .iter()
            .filter(|process| process.contributions().providers)
            .map(|process| ExtensionProviderOwner {
                extension_instance_id: process.extension_instance_id().to_owned(),
                generation: process.health_snapshot().generation,
            })
            .collect()
    }

    pub(super) fn await_initial_registrations(&self, processes: &[ExtensionProcess]) {
        let owners = Self::initial_provider_owners(processes);
        // A completion notification follows every bridge-owned initial batch,
        // including an empty one. Extensions predating that additive API surface
        // time out here and their incomplete declarations remain unprojected.
        let _ = self
            .registry
            .wait_for_owners(&owners, PROVIDER_REGISTRATION_BARRIER);
    }

    pub(super) async fn await_initial_registrations_async(&self, processes: &[ExtensionProcess]) {
        let owners = Self::initial_provider_owners(processes);
        if owners.is_empty() {
            return;
        }
        let registry = Arc::clone(&self.registry);
        // Reload runs from the Tokio runtime. Keep its completion wait off a
        // core worker so post-initialize reverse registration can make progress
        // even when the runtime has only one configured worker thread.
        let _ = tokio::task::spawn_blocking(move || {
            registry.wait_for_owners(&owners, PROVIDER_REGISTRATION_BARRIER)
        })
        .await;
    }

    /// Reconciles ready provider declarations into the local model catalog.
    ///
    /// Each synthesized endpoint is fenced by the owner instance and process
    /// generation. A replacement or authorization revocation therefore removes
    /// the old model and host stream transport before a newer route is made
    /// selectable. Extension-declared URLs, headers, credentials, and leases
    /// never enter this projection.
    pub(super) fn synchronize(
        &self,
        catalog: &mut ModelCatalog,
        client: &AiClient,
        processes: &[ExtensionProcess],
    ) -> Vec<String> {
        self.synchronize_report(catalog, client, processes)
            .into_notices()
    }

    pub(super) fn synchronize_report(
        &self,
        catalog: &mut ModelCatalog,
        client: &AiClient,
        processes: &[ExtensionProcess],
    ) -> ProviderCatalogReport {
        let (revision, entries) = self.registry.snapshot();
        let desired = entries
            .iter()
            .filter(|entry| entry.authorization == ExtensionProviderAuthorizationStatus::Ready)
            .flat_map(|entry| {
                entry.models.iter().filter_map(|model| {
                    extension_provider_protocol(&model.protocol)
                        .map(|_| extension_provider_model_id(&entry.provider.id, &model.id).0)
                })
            })
            .collect::<BTreeSet<_>>();
        let mut projection = self
            .projection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = projection.revision == Some(revision)
            && projection.desired == desired
            && projection.routes.iter().all(|(model, endpoint)| {
                catalog
                    .resolve(&ModelId(model.clone()))
                    .is_ok_and(|registered| registered.endpoint.id == *endpoint)
            });
        if current {
            return ProviderCatalogReport {
                problems: projection.problems.clone(),
                ..Default::default()
            };
        }

        // A synchronization after the first projection is a live change to a
        // running session. Models that were not projected before are reported
        // through the existing notice surface; the first projection stays quiet.
        let live_change = projection.revision.is_some();
        let previously_projected = projection.routes.keys().cloned().collect::<BTreeSet<_>>();
        let mut newly_live = Vec::new();
        for (model, endpoint) in std::mem::take(&mut projection.routes) {
            let model = ModelId(model);
            catalog.remove_model_if_endpoint(&model, &endpoint);
            client.remove_host_stream_transport(&endpoint);
            catalog.remove_endpoint_if_unused(&endpoint);
        }

        let mut diagnostics = Vec::new();
        for entry in entries {
            if entry.authorization != ExtensionProviderAuthorizationStatus::Ready {
                continue;
            }
            let Some(process) = processes.iter().find(|process| {
                process.extension_instance_id() == entry.owner.extension_instance_id
                    && process.health_snapshot().generation == entry.owner.generation
                    && process.is_running()
            }) else {
                diagnostics.push(format!(
                    "warning: extension provider {:?} has no live owning process",
                    entry.provider.id
                ));
                continue;
            };

            for provider_model in entry.models {
                let Some(protocol) = extension_provider_protocol(&provider_model.protocol) else {
                    diagnostics.push(format!(
                        "warning: extension provider {:?} model {:?} declares an unsupported protocol",
                        entry.provider.id, provider_model.id
                    ));
                    continue;
                };
                let Some(route) = self
                    .registry
                    .resolve(&entry.provider.id, &provider_model.id)
                else {
                    continue;
                };
                if route.owner != entry.owner || route.model != provider_model {
                    continue;
                }

                let model_id = extension_provider_model_id(&entry.provider.id, &provider_model.id);
                if catalog.resolve(&model_id).is_ok() {
                    diagnostics.push(format!(
                        "warning: extension provider model {:?} conflicts with an existing catalog model",
                        model_id.0
                    ));
                    continue;
                }
                let endpoint_id = extension_provider_endpoint_id(
                    &entry.owner,
                    &entry.provider.id,
                    &provider_model.id,
                );
                if catalog.has_endpoint(&endpoint_id) {
                    diagnostics.push(
                        "warning: extension provider endpoint identity conflicts with an existing catalog endpoint"
                            .to_owned(),
                    );
                    continue;
                }
                let (Ok(context_window), Ok(max_output_tokens)) = (
                    u64::try_from(provider_model.context_window),
                    u64::try_from(provider_model.max_output_tokens),
                ) else {
                    diagnostics.push(format!(
                        "warning: extension provider model {:?} has token limits outside the local catalog range",
                        model_id.0
                    ));
                    continue;
                };
                let endpoint = Endpoint {
                    id: endpoint_id.clone(),
                    // This inert local URL is required by ModelCatalog's
                    // endpoint invariant. AiClient dispatches the registered
                    // host transport before any HTTP codec can inspect it.
                    base_url: url::Url::parse("http://127.0.0.1:9/")
                        .expect("fixed extension-provider endpoint URL is valid"),
                    auth: Auth::None,
                    default_headers: http::HeaderMap::new(),
                    transport: EndpointTransport::Http,
                    runtime: RequestRuntime::default(),
                    timeout: Duration::from_secs(30),
                };
                if catalog.register_endpoint(endpoint).is_err() {
                    diagnostics.push(
                        "warning: extension provider endpoint could not be registered".to_owned(),
                    );
                    continue;
                }
                let _ =
                    catalog.set_endpoint_label(endpoint_id.clone(), entry.provider.label.clone());
                let specification = ModelSpec {
                    id: model_id.clone(),
                    endpoint: endpoint_id.clone(),
                    api_name: provider_model.api_name.clone(),
                    display_name: provider_model.display_name.clone(),
                    protocol,
                    capabilities: extension_provider_capabilities(&provider_model.capabilities),
                    limits: ModelLimits {
                        context_window,
                        max_output_tokens,
                    },
                    pricing: None,
                    // API 0.3 provider declarations carry no model presets or
                    // HTTP headers; never infer unnegotiated transport authority.
                    preset: Default::default(),
                    cache: CacheCompatibility {
                        supports_long_retention: false,
                        supports_explicit_prompt_cache_mode: false,
                        send_session_id_header: false,
                        send_session_affinity_headers: false,
                        session_affinity_format: None,
                        cache_control_format: None,
                        supports_cache_control_on_tools: false,
                    },
                };
                if catalog.register_model(specification).is_err() {
                    catalog.remove_endpoint_if_unused(&endpoint_id);
                    diagnostics.push(format!(
                        "warning: extension provider model {:?} could not be registered",
                        model_id.0
                    ));
                    continue;
                }
                client.register_host_stream_transport(
                    endpoint_id.clone(),
                    process.provider_stream_transport(entry.provider.id.clone(), provider_model.id),
                );
                if live_change && !previously_projected.contains(&model_id.0) {
                    newly_live.push(model_id.0.clone());
                }
                projection.routes.insert(model_id.0, endpoint_id);
            }
        }
        projection.revision = Some(revision);
        projection.desired = desired;
        projection.problems = diagnostics.clone();
        let mut details = Vec::new();
        for (index, model) in newly_live.iter().enumerate() {
            if index == MAX_LIVE_REGISTRATION_NOTICES {
                details.push(format!(
                    "extension provider: {} more model(s) registered while this session was running",
                    newly_live.len().saturating_sub(MAX_LIVE_REGISTRATION_NOTICES)
                ));
                break;
            }
            details.push(format!(
                "extension provider model {model:?} is now live; it is available for the next request"
            ));
        }
        ProviderCatalogReport {
            checked: true,
            problems: diagnostics,
            details,
        }
    }

    /// Removes only routes this runtime previously projected, including their
    /// host-stream transport registrations.
    pub(super) fn clear(&self, catalog: &mut ModelCatalog, client: &AiClient) {
        let mut projection = self
            .projection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (model, endpoint) in std::mem::take(&mut projection.routes) {
            catalog.remove_model_if_endpoint(&ModelId(model), &endpoint);
            client.remove_host_stream_transport(&endpoint);
            catalog.remove_endpoint_if_unused(&endpoint);
        }
        projection.revision = None;
        projection.desired.clear();
    }
}

pub(super) fn extension_provider_model_id(provider_id: &str, model_id: &str) -> ModelId {
    ModelId(format!("{provider_id}/{model_id}"))
}

pub(super) fn extension_provider_endpoint_id(
    owner: &ExtensionProviderOwner,
    provider_id: &str,
    model_id: &str,
) -> EndpointId {
    let mut digest = Sha256::new();
    digest.update(b"octet-coding-agent-extension-provider-endpoint-v1\0");
    digest.update(owner.extension_instance_id.as_bytes());
    digest.update([0]);
    digest.update(owner.generation.to_le_bytes());
    digest.update(provider_id.as_bytes());
    digest.update([0]);
    digest.update(model_id.as_bytes());
    let digest = format!("{:x}", digest.finalize());
    EndpointId(format!("extension-provider-{}", &digest[..32]))
}

pub(super) fn extension_provider_protocol(protocol: &str) -> Option<Protocol> {
    match protocol {
        "openai_chat" => Some(Protocol::OpenAiChat),
        "openai_responses" => Some(Protocol::OpenAiResponses),
        "anthropic_messages" => Some(Protocol::AnthropicMessages),
        _ => None,
    }
}

pub(super) fn extension_provider_capabilities(
    capabilities: &octet_agent::extension_api_v03::ProviderModelCapabilities,
) -> Capabilities {
    Capabilities {
        responses_features: Default::default(),
        input_modalities: Default::default(),
        output_modalities: Default::default(),
        tools: capabilities.tools,
        parallel_tool_calls: capabilities.parallel_tool_calls,
        reasoning: capabilities.reasoning.then_some(ReasoningCapability {
            options: None,
            control: ReasoningControl::Effort,
            exposes_text: true,
            preserves_state: false,
            effort_budgets: None,
            openai_chat_mode: OpenAiChatReasoningMode::Standard,
            min_effort: ReasoningEffort::Minimal,
            max_effort: ReasoningEffort::High,
        }),
        responses_lite: false,
        agent_delegation: None,
        structured_output: capabilities.structured_output,
        deferred_tool_loading: false,
    }
}
