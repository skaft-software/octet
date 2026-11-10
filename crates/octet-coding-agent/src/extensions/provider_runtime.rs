//! Providers served by extensions and their catalog projection.

use super::*;

/// Only portable Pi thinking levels cross this facade. Native On/Ultra and
/// arbitrary token budgets are not given invented Pi equivalents.
pub(crate) fn pi_thinking_level(model: &Model, reasoning: &ReasoningConfig) -> Option<String> {
    let level = crate::app::level_from_reasoning(reasoning, model).ok()?;
    match level {
        crate::config::ThinkingLevel::On | crate::config::ThinkingLevel::Ultra => None,
        _ => Some(level.label().to_owned()),
    }
}

impl ExecutableExtensions {
    pub(crate) fn prepare_selected_provider_transport(
        &self,
        model: &Model,
    ) -> Result<Option<Arc<dyn octet_ai::HostStreamTransport>>, octet_ai::AiError> {
        let unavailable = || {
            octet_ai::AiError::Config(octet_ai::ConfigError::Parse(
                "the selected extension provider is unavailable at prompt preparation".into(),
            ))
        };
        let facts = {
            let projection = self
                .provider_runtime
                .projection
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            match projection.facts.get(&model.spec.id.0) {
                Some(facts) => facts.clone(),
                None if projection.routes.contains_key(&model.spec.id.0) => {
                    return Err(unavailable())
                }
                None => return Ok(None),
            }
        };
        let route = &facts.route;
        let mut capabilities = self
            .provider_runtime
            .native_model(route)
            .map(|model| model.spec.capabilities.clone())
            .unwrap_or_else(|| {
                extension_provider_capabilities(
                    &route.model.capabilities,
                    route
                        .model_metadata
                        .as_ref()
                        .and_then(|metadata| metadata.reasoning_options.as_ref()),
                )
            });
        if route
            .model_metadata
            .as_ref()
            .is_some_and(|metadata| metadata.input.iter().any(|input| input == "image"))
        {
            capabilities.input_modalities = capabilities
                .input_modalities
                .with(octet_ai::Modality::Image);
        }
        if facts.model_id != model.spec.id.0
            || facts.endpoint != model.endpoint.id
            || facts.endpoint != model.spec.endpoint
            || extension_provider_projected_model_id(
                &route.provider.id,
                &route.model.id,
                route.model_metadata.as_ref(),
            ) != model.spec.id
            || extension_provider_endpoint_id(route) != model.endpoint.id
            || extension_provider_protocol(&route.model.protocol) != Some(model.spec.protocol)
            || route.model.api_name != model.spec.api_name
            || route.model.display_name != model.spec.display_name
            || capabilities != model.spec.capabilities
            || u64::try_from(route.model.context_window).ok()
                != Some(model.spec.limits.context_window)
            || u64::try_from(route.model.max_output_tokens).ok()
                != Some(model.spec.limits.max_output_tokens)
            || route
                .model_metadata
                .as_ref()
                .and_then(|metadata| metadata.pricing.clone())
                != model.spec.pricing
        {
            return Err(unavailable());
        }
        let process = self
            .processes
            .iter()
            .find(|process| {
                let descriptor = process.descriptor();
                process.extension_instance_id() == route.owner.extension_instance_id
                    && process.health_snapshot().generation == route.owner.generation
                    && descriptor.manifest.name == facts.extension.0
                    && descriptor.manifest_path == facts.extension.1
            })
            .ok_or_else(unavailable)?;
        process
            .prepare_provider_stream_transport(route, octet_ai::HostStreamModel::from(model))
            .map(Some)
    }

    pub(crate) fn refresh_pi_model_catalog(
        &mut self,
        catalog: &ModelCatalog,
        scope: Option<&[crate::cli::parity::ScopedModel]>,
        _current_model: &Model,
    ) {
        self.refresh_pi_model_catalog_without_selection(catalog, scope);
    }

    pub(crate) fn refresh_pi_model_catalog_without_selection(
        &mut self,
        catalog: &ModelCatalog,
        scope: Option<&[crate::cli::parity::ScopedModel]>,
    ) {
        self.refresh_native_credential_catalog(catalog);
        let mut all = Vec::new();
        let mut all_representable = true;
        let mut available = Vec::new();
        let mut available_representable = true;
        let mut configured_auth = BTreeMap::<String, bool>::new();
        for spec in catalog.models() {
            let model = match catalog.resolve(&spec.id) {
                Ok(model) => model,
                Err(_) => {
                    all_representable = false;
                    available_representable = false;
                    continue;
                }
            };
            let configured = model.endpoint.auth.is_configured();
            let provider = pi_provider_id(&model);
            configured_auth
                .entry(provider)
                .and_modify(|value| *value |= configured)
                .or_insert(configured);
            let Some(view) = self.provider_runtime.pi_model_view(&model) else {
                all_representable = false;
                if configured {
                    available_representable = false;
                }
                continue;
            };
            if configured {
                available.push(view.clone());
            }
            all.push(view);
        }
        let mut scoped = Vec::new();
        let mut representable = true;
        for entry in scope.into_iter().flatten() {
            let model = match catalog.resolve(&entry.id) {
                Ok(model) => model,
                Err(_) => {
                    representable = false;
                    break;
                }
            };
            let Some(view) = self.provider_runtime.pi_model_view(&model) else {
                representable = false;
                break;
            };
            let thinking = match entry.reasoning.as_deref() {
                None => None,
                Some(value) => match crate::config::parse_reasoning(value)
                    .ok()
                    .and_then(|reasoning| pi_thinking_level(&model, &reasoning))
                {
                    Some(level) => Some(level),
                    None => {
                        representable = false;
                        break;
                    }
                },
            };
            let mut value = serde_json::json!({"model": view});
            if let Some(thinking) = thinking {
                value["thinkingLevel"] = Value::String(thinking);
            }
            scoped.push(value);
        }
        let mut snapshot = serde_json::json!({
            "all_models_status": {"status": "unavailable"},
            "provider_auth_status": {"status": "unavailable"},
            "configured_auth": configured_auth,
            "provider_oauth": {},
        });
        if all_representable {
            snapshot["all_models"] = serde_json::json!(all);
            snapshot["all_models_status"] = serde_json::json!({"status": "catalog"});
        }
        if available_representable {
            snapshot["available_models"] = serde_json::json!(available);
        }
        if representable {
            snapshot["scoped_models"] = serde_json::json!(scoped);
        }
        // Preserve the existing bounded JSON-RPC transport; refuse the whole
        // snapshot rather than claim a silently truncated available catalog.
        let snapshot = serde_json::to_vec(&snapshot)
            .ok()
            .filter(|bytes| bytes.len() <= 256 * 1024)
            .map(|_| snapshot);
        let mut state = self
            .host_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if state.pi_models == snapshot {
            return;
        }
        state.pi_models = snapshot;
        for process in &self.processes {
            if process.descriptor().manifest.runtime.sharing == ExtensionRuntimeSharing::Isolated {
                process.set_host_state(state.clone());
            }
        }
        *self
            .host_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = state;
    }

    pub(crate) fn reloaded_provider_model(
        &self,
        catalog: &ModelCatalog,
        active: &Model,
    ) -> Option<Model> {
        self.provider_runtime.reloaded_model(
            catalog,
            active,
            &self.processes,
            self.session_id.as_deref(),
            self.resource_owner.as_deref(),
        )
    }

    pub(crate) fn prepare_provider_reload(&self) {
        let model = self
            .host_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .model
            .clone();
        let mut projection = self
            .provider_runtime
            .projection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let pending = projection.reload.take();
        let (Some(session_id), Some(binding), Some(model)) =
            (&self.session_id, &self.resource_owner, model)
        else {
            return;
        };
        let Some(previous) = projection.facts.get(&model).cloned() else {
            return;
        };
        let Some(current) = self
            .provider_runtime
            .registry
            .resolve(&previous.route.provider.id, &previous.route.model.id)
        else {
            return;
        };
        if !self.processes.iter().any(|process| {
            process.extension_instance_id() == previous.route.owner.extension_instance_id
                && process.health_snapshot().generation == previous.route.owner.generation
                && process.is_running()
        }) || current.owner != previous.route.owner
            || current.provider != previous.route.provider
            || current.model != previous.route.model
            || current.model_metadata != previous.route.model_metadata
        {
            return;
        }
        let previous = pending
            .filter(|pending| {
                pending.session_id == *session_id
                    && pending.binding == *binding
                    && pending.replacement.as_ref() == Some(&previous.route.owner)
                    && pending.previous.same_declaration(&previous)
            })
            .map_or(previous, |pending| pending.previous);
        projection.reload = Some(ProviderReloadSelection {
            session_id: session_id.clone(),
            binding: binding.clone(),
            previous,
            replacement: None,
        });
    }

    pub(crate) fn complete_provider_reload(&self, reloaded: Option<&BTreeSet<String>>) {
        let mut projection = self
            .provider_runtime
            .projection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(mut selection) = projection.reload.take() else {
            return;
        };
        if self.session_id.as_ref() != Some(&selection.session_id) {
            return;
        }
        let Some(binding) = &self.resource_owner else {
            return;
        };
        let Some(process) = self.processes.iter().find(|process| {
            let descriptor = process.descriptor();
            descriptor.manifest.name == selection.previous.extension.0
                && descriptor.manifest_path == selection.previous.extension.1
                && reloaded.is_none_or(|names| names.contains(&descriptor.manifest.name))
                && process.is_running()
        }) else {
            return;
        };
        let Some(route) = self.provider_runtime.registry.resolve(
            &selection.previous.route.provider.id,
            &selection.previous.route.model.id,
        ) else {
            return;
        };
        if route.owner.extension_instance_id != process.extension_instance_id()
            || route.owner.generation != process.health_snapshot().generation
            || route.owner == selection.previous.route.owner
        {
            return;
        }
        let current = ProjectedProviderRoute {
            model_id: selection.previous.model_id.clone(),
            endpoint: extension_provider_endpoint_id(&route),
            extension: selection.previous.extension.clone(),
            route,
        };
        if !current.same_declaration(&selection.previous) {
            return;
        }
        selection.binding = binding.clone();
        selection.replacement = Some(current.route.owner);
        projection.reload = Some(selection);
    }

    pub(crate) fn discard_provider_reload(&self) {
        self.provider_runtime
            .projection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .reload = None;
    }

    /// Snapshot only the native catalog an approved credential request resolves
    /// against.
    ///
    /// Run modes that serve extension requests without the interactive model
    /// facts projection still need Pi's request-time credential resolution (Pi's
    /// own registry answers in every mode). Publishing exactly this snapshot
    /// keeps the projected model facts unchanged.
    pub(crate) fn refresh_native_credential_catalog(&mut self, catalog: &ModelCatalog) {
        let mut native = catalog.clone();
        let projection = self
            .provider_runtime
            .projection
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        for original in projection.native_models.values() {
            if let Ok(projected) = native.resolve(&original.spec.id) {
                native.remove_model_if_endpoint(&original.spec.id, &projected.endpoint.id);
            }
            let _ = native.register_model((*original.spec).clone());
        }
        drop(projection);
        *self
            .provider_runtime
            .native_catalog
            .write()
            .unwrap_or_else(|p| p.into_inner()) = native;
    }
}

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
        _provider: &octet_agent::extension_api_v04::ProviderDefinition,
        request: &octet_agent::extension_api_v04::ProviderAuthorizationRequest,
    ) -> octet_agent::extension_api_v04::ProviderAuthorizationResult {
        octet_agent::extension_api_v04::ProviderAuthorizationResult {
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
    native_models: BTreeMap<String, Model>,
    pub(super) problems: Vec<String>,
    pub(super) facts: BTreeMap<String, ProjectedProviderRoute>,
    reload: Option<ProviderReloadSelection>,
}

struct ProviderReloadSelection {
    session_id: String,
    binding: String,
    previous: ProjectedProviderRoute,
    replacement: Option<ExtensionProviderOwner>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ProjectedProviderRoute {
    pub(super) model_id: String,
    pub(super) endpoint: EndpointId,
    pub(super) extension: (String, PathBuf),
    pub(super) route: octet_agent::ExtensionProviderRoute,
}

impl ProjectedProviderRoute {
    fn same_declaration(&self, other: &Self) -> bool {
        self.model_id == other.model_id
            && self.extension == other.extension
            && self.route.provider == other.route.provider
            && self.route.model == other.route.model
            && self.route.model_metadata == other.route.model_metadata
    }
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
    native_catalog: Arc<std::sync::RwLock<ModelCatalog>>,
}

impl Default for ExtensionProviderRuntime {
    fn default() -> Self {
        Self {
            registry: Arc::new(ExtensionProviderRegistry::with_authorization_policy(
                Arc::new(CodingAgentProviderAuthorizationPolicy),
            )),
            projection: Arc::new(Mutex::new(ProviderCatalogProjection::default())),
            native_catalog: Arc::new(std::sync::RwLock::new(ModelCatalog::default())),
        }
    }
}

impl ExtensionProviderRuntime {
    pub(crate) fn bind_native_provider_catalog(&self, catalog: &ModelCatalog, _client: &AiClient) {
        let projection = self.projection.lock().unwrap_or_else(|p| p.into_inner());
        let models = catalog
            .models()
            .filter(|spec| !projection.routes.contains_key(&spec.id.0))
            .filter_map(|spec| {
                let model = catalog.resolve(&spec.id).ok()?;
                let view = extension_model_view(&model)?;
                Some((view.provider.clone(), model, view))
            })
            .collect();
        drop(projection);
        *self
            .native_catalog
            .write()
            .unwrap_or_else(|p| p.into_inner()) = catalog.clone();
        self.registry.set_native_provider_catalog(
            octet_agent::extension_process::NativeProviderCatalog { models },
        );
    }

    pub(super) fn projected_model_id(&self, provider: &str, model: &str) -> ModelId {
        self.registry
            .resolve(provider, model)
            .map(|route| {
                extension_provider_projected_model_id(
                    provider,
                    model,
                    route.model_metadata.as_ref(),
                )
            })
            .unwrap_or_else(|| extension_provider_model_id(provider, model))
    }

    fn native_model(&self, route: &octet_agent::ExtensionProviderRoute) -> Option<Model> {
        let id = route.model_metadata.as_ref()?.native_model_id.as_ref()?;
        self.native_catalog
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .resolve(&ModelId(id.clone()))
            .ok()
    }

    /// Pi's `ResolvedRequestAuth` for one exact native provider/model.
    ///
    /// Every failure — an identity this host does not own, a request signer
    /// Pi's key/header result cannot represent, or an unresolved environment or
    /// OAuth refresh — settles as Pi's own data-free result, so a refusal can
    /// neither echo a credential nor widen disclosure.
    pub(super) async fn pi_provider_credentials_response(
        &self,
        provider: &str,
        model_id: &str,
    ) -> serde_json::Value {
        self.pi_provider_credentials(provider, model_id)
            .await
            .unwrap_or_else(|()| Self::pi_credentials_unavailable(provider))
    }

    /// Pi 1.0.2's exact result for a model whose credential cannot be resolved.
    pub(super) fn pi_credentials_unavailable(provider: &str) -> serde_json::Value {
        serde_json::json!({
            "ok": false,
            "error": format!("No API key found for \"{provider}\""),
        })
    }

    /// Resolve one endpoint's credential for an authorized extension.
    async fn pi_provider_credentials(
        &self,
        provider: &str,
        model_id: &str,
    ) -> Result<serde_json::Value, ()> {
        let catalog = self
            .native_catalog
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let mut selected = None;
        for spec in catalog.models() {
            let Ok(model) = catalog.resolve(&spec.id) else {
                continue;
            };
            if spec.api_name == model_id && pi_provider_id(&model) == provider {
                if selected.is_some() {
                    return Err(());
                }
                selected = Some(model);
            }
        }
        let model = selected.ok_or(())?;
        let credentials = octet_ai::resolve_extension_provider_credentials(&model.endpoint.auth)
            .await
            .map_err(|_| ())?;
        let mut headers = BTreeMap::new();
        for (name, value) in model.endpoint.default_headers.iter() {
            let value = value.to_str().map_err(|_| ())?;
            if headers
                .insert(name.as_str().to_owned(), value.to_owned())
                .is_some()
            {
                return Err(());
            }
        }
        headers.extend(credentials.headers);
        // Pi omits `apiKey` when the resolution has none (a header-only or
        // unauthenticated endpoint): never send a JSON null the Pi facade would
        // have to invent absent-ness for.
        let mut result = serde_json::Map::new();
        result.insert("ok".into(), serde_json::Value::Bool(true));
        if let Some(api_key) = credentials.api_key {
            result.insert("apiKey".into(), serde_json::Value::String(api_key));
        }
        result.insert("headers".into(), serde_json::json!(headers));
        Ok(serde_json::Value::Object(result))
    }

    /// Project explicit custom-stream facts only while the native route is
    /// owned by its current live declaration. Built-in endpoints stay withheld.
    pub(super) fn pi_model_view(
        &self,
        model: &Model,
    ) -> Option<octet_agent::extension_process::ExtensionModelView> {
        let mut view = extension_model_view(model)?;
        if let Some((provider, id)) = model.spec.id.0.split_once('/') {
            if let Some(route) = self.registry.resolve(provider, id) {
                if extension_provider_endpoint_id(&route) == model.endpoint.id {
                    if let Some(metadata) = &route.model_metadata {
                        view.base_url = Some(metadata.base_url.clone());
                    }
                }
            }
        }
        Some(view)
    }

    #[cfg(test)]
    pub(crate) fn registered_route_for_test(
        &self,
        model: &ModelId,
    ) -> Option<(octet_agent::ExtensionProviderRoute, EndpointId)> {
        let (provider, model) = model.0.split_once('/')?;
        let route = self.registry.resolve(provider, model)?;
        let endpoint = extension_provider_endpoint_id(&route);
        Some((route, endpoint))
    }

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
                        .map(|_| self.projected_model_id(&entry.provider.id, &model.id).0)
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
            projection.facts.remove(&model);
            let model = ModelId(model);
            catalog.remove_model_if_endpoint(&model, &endpoint);
            if let Some(native) = projection.native_models.remove(&model.0) {
                let _ = catalog.register_model((*native.spec).clone());
            }
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

                let model_id = extension_provider_projected_model_id(
                    &entry.provider.id,
                    &provider_model.id,
                    route.model_metadata.as_ref(),
                );
                let native = self.native_model(&route);
                if route
                    .model_metadata
                    .as_ref()
                    .is_some_and(|metadata| metadata.inherit_provider.is_some())
                    && native.is_none()
                {
                    diagnostics
                        .push("warning: inherited native model is no longer available".into());
                    continue;
                }
                if catalog.resolve(&model_id).is_ok_and(|existing| {
                    native
                        .as_ref()
                        .is_none_or(|native| existing.endpoint.id != native.endpoint.id)
                }) {
                    diagnostics.push(format!(
                        "warning: extension provider model {:?} conflicts with an existing catalog model",
                        model_id.0
                    ));
                    continue;
                }
                let endpoint_id = extension_provider_endpoint_id(&route);
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
                let mut capabilities = extension_provider_capabilities(
                    &provider_model.capabilities,
                    route
                        .model_metadata
                        .as_ref()
                        .and_then(|metadata| metadata.reasoning_options.as_ref()),
                );
                if route
                    .model_metadata
                    .as_ref()
                    .is_some_and(|metadata| metadata.input.iter().any(|input| input == "image"))
                {
                    capabilities.input_modalities = capabilities
                        .input_modalities
                        .with(octet_ai::Modality::Image);
                }
                let mut specification = ModelSpec {
                    id: model_id.clone(),
                    endpoint: endpoint_id.clone(),
                    api_name: provider_model.api_name.clone(),
                    display_name: provider_model.display_name.clone(),
                    protocol,
                    capabilities,
                    limits: ModelLimits {
                        context_window,
                        max_output_tokens,
                    },
                    pricing: route
                        .model_metadata
                        .as_ref()
                        .and_then(|metadata| metadata.pricing.clone()),
                    // API 0.3 provider declarations carry no model presets or
                    // HTTP headers; never infer unnegotiated transport authority.
                    preset: Default::default(),
                    cache: CacheCompatibility {
                        prompt_cache: Default::default(),
                        supports_long_retention: false,
                        supports_explicit_prompt_cache_mode: false,
                        send_session_id_header: false,
                        send_session_affinity_headers: false,
                        session_affinity_format: None,
                        cache_control_format: None,
                        supports_cache_control_on_tools: false,
                    },
                };
                if let Some(native) = &native {
                    specification = (*native.spec).clone();
                    specification.endpoint = endpoint_id.clone();
                    if !catalog.remove_model_if_endpoint(&native.spec.id, &native.endpoint.id) {
                        catalog.remove_endpoint_if_unused(&endpoint_id);
                        diagnostics
                            .push("warning: inherited native model is no longer available".into());
                        continue;
                    }
                }
                if catalog.register_model(specification).is_err() {
                    if let Some(native) = &native {
                        let _ = catalog.register_model((*native.spec).clone());
                    }
                    catalog.remove_endpoint_if_unused(&endpoint_id);
                    diagnostics.push(format!(
                        "warning: extension provider model {:?} could not be registered",
                        model_id.0
                    ));
                    continue;
                }
                if let Some(native) = native {
                    projection.native_models.insert(model_id.0.clone(), native);
                }
                client.register_host_stream_transport(
                    endpoint_id.clone(),
                    process.provider_stream_transport(route.clone()),
                );
                if live_change && !previously_projected.contains(&model_id.0) {
                    newly_live.push(model_id.0.clone());
                }
                let descriptor = process.descriptor();
                projection.facts.insert(
                    model_id.0.clone(),
                    ProjectedProviderRoute {
                        model_id: model_id.0.clone(),
                        endpoint: endpoint_id.clone(),
                        extension: (
                            descriptor.manifest.name.clone(),
                            descriptor.manifest_path.clone(),
                        ),
                        route,
                    },
                );
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
            projection.facts.remove(&model);
            catalog.remove_model_if_endpoint(&ModelId(model.clone()), &endpoint);
            if let Some(native) = projection.native_models.remove(&model) {
                let _ = catalog.register_model((*native.spec).clone());
            }
            client.remove_host_stream_transport(&endpoint);
            catalog.remove_endpoint_if_unused(&endpoint);
        }
        projection.revision = None;
        projection.desired.clear();
    }

    pub(super) fn reloaded_model(
        &self,
        catalog: &ModelCatalog,
        active: &Model,
        processes: &[ExtensionProcess],
        session_id: Option<&str>,
        binding: Option<&str>,
    ) -> Option<Model> {
        let projection = self
            .projection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let selection = projection.reload.as_ref()?;
        if session_id != Some(selection.session_id.as_str())
            || binding != Some(selection.binding.as_str())
            || selection.previous.endpoint != active.endpoint.id
            || selection.previous.model_id != active.spec.id.0
        {
            return None;
        }
        let retired = &selection.previous;
        let current = projection.facts.get(&active.spec.id.0)?;
        let model = catalog.resolve(&active.spec.id).ok()?;
        let live = |owner: &ExtensionProviderOwner| {
            processes.iter().any(|process| {
                process.extension_instance_id() == owner.extension_instance_id
                    && process.health_snapshot().generation == owner.generation
                    && process.is_running()
            })
        };
        let rebindable = model.endpoint.id == current.endpoint
            && projection.routes.get(&current.model_id) == Some(&current.endpoint)
            && selection.replacement.as_ref() == Some(&current.route.owner)
            && current.route.owner != retired.route.owner
            && live(&current.route.owner)
            && !live(&retired.route.owner)
            && current.same_declaration(retired)
            && extension_provider_endpoint_id(&current.route) == current.endpoint
            && self.registry.route_has_continuous_authority(&current.route);
        rebindable.then_some(model)
    }
}

pub(super) fn extension_provider_model_id(provider_id: &str, model_id: &str) -> ModelId {
    ModelId(format!("{provider_id}/{model_id}"))
}

pub(super) fn extension_provider_projected_model_id(
    provider_id: &str,
    model_id: &str,
    metadata: Option<&octet_agent::extension_process::PiProviderModelMetadata>,
) -> ModelId {
    metadata
        .and_then(|metadata| metadata.native_model_id.clone())
        .map(ModelId)
        .unwrap_or_else(|| extension_provider_model_id(provider_id, model_id))
}

pub(super) fn extension_provider_endpoint_id(
    route: &octet_agent::ExtensionProviderRoute,
) -> EndpointId {
    let mut digest = Sha256::new();
    digest.update(b"octet-coding-agent-extension-provider-endpoint-v2\0");
    digest.update(route.owner.extension_instance_id.as_bytes());
    digest.update([0]);
    digest.update(route.owner.generation.to_le_bytes());
    digest.update(
        serde_json::to_vec(&(&route.provider, &route.model, &route.model_metadata))
            .expect("provider route declarations serialize"),
    );
    let digest = format!("{:x}", digest.finalize());
    EndpointId(format!("extension-provider-{}", &digest[..32]))
}

pub(super) fn extension_provider_protocol(protocol: &str) -> Option<Protocol> {
    match protocol {
        "openai_chat" => Some(Protocol::OpenAiChat),
        "openai_responses" => Some(Protocol::OpenAiResponses),
        "anthropic_messages" => Some(Protocol::AnthropicMessages),
        "google_generative_ai" => Some(Protocol::GoogleGenerativeAi),
        "extension_stream" => Some(Protocol::ExtensionStream),
        _ => None,
    }
}

pub(super) fn extension_provider_capabilities(
    capabilities: &octet_agent::extension_api_v04::ProviderModelCapabilities,
    options: Option<&octet_ai::types::ReasoningOptions>,
) -> Capabilities {
    let efforts: Vec<_> = options
        .into_iter()
        .flat_map(|options| options.choices())
        .filter_map(|choice| {
            if let ReasoningConfig::Effort(effort) = choice {
                Some(effort)
            } else {
                None
            }
        })
        .collect();
    Capabilities {
        responses_features: Default::default(),
        input_modalities: Default::default(),
        output_modalities: Default::default(),
        tools: capabilities.tools,
        parallel_tool_calls: capabilities.parallel_tool_calls,
        reasoning: capabilities.reasoning.then_some(ReasoningCapability {
            options: options.cloned(),
            control: ReasoningControl::Effort,
            exposes_text: true,
            preserves_state: false,
            effort_budgets: None,
            openai_chat_mode: OpenAiChatReasoningMode::Standard,
            min_effort: efforts
                .iter()
                .copied()
                .min()
                .unwrap_or(ReasoningEffort::Minimal),
            max_effort: efforts
                .iter()
                .copied()
                .max()
                .unwrap_or(ReasoningEffort::High),
        }),
        responses_lite: false,
        agent_delegation: None,
        structured_output: capabilities.structured_output,
        deferred_tool_loading: false,
    }
}

#[cfg(test)]
mod credential_tests {
    use super::*;

    #[tokio::test]
    async fn provider_credentials_require_exact_registered_provider_and_model() {
        let mut catalog = ModelCatalog::builtin().unwrap();
        let mut spec = (*catalog
            .resolve(&ModelId("gpt-6-astra".into()))
            .unwrap()
            .spec)
            .clone();
        spec.id = ModelId("test/test-model".into());
        spec.endpoint = EndpointId("test-credentials".into());
        spec.api_name = "test-model".into();
        catalog
            .register_endpoint(Endpoint {
                id: spec.endpoint.clone(),
                base_url: url::Url::parse("https://example.test/v1/").unwrap(),
                auth: octet_ai::Auth::bearer("scoped-fixture-secret"),
                default_headers: http::HeaderMap::new(),
                transport: Default::default(),
                runtime: Default::default(),
                timeout: Duration::from_secs(30),
            })
            .unwrap();
        catalog.register_model(spec).unwrap();

        // A header-only endpoint keeps Pi's optional `apiKey` absent instead of
        // sending a JSON null the facade would reject.
        let mut header_spec = (*catalog
            .resolve(&ModelId("test/test-model".into()))
            .unwrap()
            .spec)
            .clone();
        header_spec.id = ModelId("test/header-model".into());
        header_spec.endpoint = EndpointId("test-header-credentials".into());
        header_spec.api_name = "header-model".into();
        catalog
            .register_endpoint(Endpoint {
                id: header_spec.endpoint.clone(),
                base_url: url::Url::parse("https://example.test/v1/").unwrap(),
                auth: octet_ai::Auth::header(http::header::AUTHORIZATION, "header-only-secret"),
                default_headers: http::HeaderMap::new(),
                transport: Default::default(),
                runtime: Default::default(),
                timeout: Duration::from_secs(30),
            })
            .unwrap();
        catalog.register_model(header_spec).unwrap();

        let runtime = ExtensionProviderRuntime::default();
        *runtime
            .native_catalog
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = catalog;

        let credentials = runtime
            .pi_provider_credentials("test", "test-model")
            .await
            .unwrap();
        assert_eq!(credentials["apiKey"], "scoped-fixture-secret");
        assert_eq!(credentials["headers"], serde_json::json!({}));
        let header_credentials = runtime
            .pi_provider_credentials("test", "header-model")
            .await
            .unwrap();
        assert!(header_credentials.get("apiKey").is_none());
        assert_eq!(
            header_credentials["headers"],
            serde_json::json!({"authorization": "header-only-secret"})
        );
        assert!(runtime
            .pi_provider_credentials("other", "test-model")
            .await
            .is_err());
        assert!(runtime
            .pi_provider_credentials("test", "other-model")
            .await
            .is_err());
        assert!(runtime
            .pi_provider_credentials("test", "gpt-6-astra")
            .await
            .is_err());
    }
}

#[cfg(test)]
mod pi_catalog_fact_tests {
    #[test]
    fn google_extension_projection_uses_only_its_native_protocol() {
        assert_eq!(
            super::extension_provider_protocol("google_generative_ai"),
            Some(Protocol::GoogleGenerativeAi)
        );
        assert_eq!(
            super::extension_provider_protocol("google_interactions"),
            None
        );
        assert_eq!(super::extension_provider_protocol("bedrock_converse"), None);
    }

    use super::*;

    fn credential_catalog() -> ModelCatalog {
        let builtins = ModelCatalog::builtin().unwrap();
        let mut spec = (*builtins
            .resolve(&ModelId("gpt-6-astra".into()))
            .unwrap()
            .spec)
            .clone();
        spec.id = ModelId("fixture/test-model".into());
        spec.endpoint = EndpointId("fixture-static-auth".into());
        spec.api_name = "test-model".into();

        let mut catalog = ModelCatalog::default();
        catalog
            .register_endpoint(Endpoint {
                id: spec.endpoint.clone(),
                base_url: url::Url::parse("https://example.test/v1/").unwrap(),
                auth: octet_ai::Auth::bearer("catalog-fixture-secret"),
                default_headers: {
                    let mut headers = http::HeaderMap::new();
                    headers.insert("x-fixture-auth", "catalog-header-secret".parse().unwrap());
                    headers
                },
                transport: Default::default(),
                runtime: Default::default(),
                timeout: Duration::from_secs(30),
            })
            .unwrap();
        let mut unconfigured = spec.clone();
        unconfigured.id = ModelId("fixture/unconfigured-model".into());
        unconfigured.api_name = "unconfigured-model".into();
        unconfigured.endpoint = EndpointId("fixture-unconfigured-auth".into());
        catalog.register_model(spec).unwrap();
        catalog
            .register_endpoint(Endpoint {
                id: unconfigured.endpoint.clone(),
                base_url: url::Url::parse("https://unconfigured.example.test/v1/").unwrap(),
                auth: octet_ai::Auth::bearer_env(
                    "OCTET_PI_CATALOG_MISSING_8A260AD050A84E32996CD633450D8715",
                ),
                default_headers: http::HeaderMap::new(),
                transport: Default::default(),
                runtime: Default::default(),
                timeout: Duration::from_secs(30),
            })
            .unwrap();
        catalog.register_model(unconfigured).unwrap();
        catalog
    }

    #[test]
    fn all_catalog_includes_unconfigured_models_without_disclosing_auth() {
        let catalog = credential_catalog();
        let mut extensions = ExecutableExtensions::default();

        extensions.refresh_pi_model_catalog_without_selection(&catalog, None);

        let state = extensions
            .host_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let snapshot = state.pi_models.as_ref().unwrap();
        assert_eq!(snapshot["all_models_status"]["status"], "catalog");
        let all = snapshot["all_models"].as_array().unwrap();
        assert_eq!(all.len(), catalog.models().count());
        assert!(all.iter().any(|model| {
            model["provider"] == "fixture" && model["id"] == "unconfigured-model"
        }));
        assert!(all.iter().all(|model| model.get("base_url").is_none()));
        assert_eq!(snapshot["provider_auth_status"]["status"], "unavailable");
        assert_eq!(snapshot["scoped_models"], serde_json::json!([]));
        assert_eq!(
            snapshot["configured_auth"],
            serde_json::json!({"fixture": true})
        );
        assert_eq!(snapshot["provider_oauth"], serde_json::json!({}));
        assert!(snapshot.get("model_auth").is_none());
        assert_eq!(snapshot["available_models"].as_array().unwrap().len(), 1);
        assert_eq!(snapshot["available_models"][0]["provider"], "fixture");
        assert_eq!(snapshot["available_models"][0]["id"], "test-model");
        for withheld in [
            "catalog-fixture-secret",
            "catalog-header-secret",
            "x-fixture-auth",
            "example.test",
            "OCTET_PI_CATALOG_MISSING_",
        ] {
            assert!(!snapshot.to_string().contains(withheld), "{withheld}");
        }
    }

    #[test]
    fn configured_auth_is_provider_scoped_and_refreshes_without_resolving_credentials() {
        let mut catalog = credential_catalog();
        let mut extensions = ExecutableExtensions::default();
        extensions.refresh_pi_model_catalog_without_selection(&catalog, None);
        assert_eq!(
            extensions
                .host_state
                .lock()
                .unwrap()
                .pi_models
                .as_ref()
                .unwrap()["configured_auth"],
            serde_json::json!({"fixture": true})
        );
        let model = catalog
            .resolve(&ModelId("fixture/test-model".into()))
            .unwrap();
        assert!(catalog.remove_model_if_endpoint(&model.spec.id, &model.endpoint.id));
        assert!(catalog.remove_endpoint_if_unused(&model.endpoint.id));
        let mut endpoint = (*model.endpoint).clone();
        endpoint.auth =
            octet_ai::Auth::bearer_env("OCTET_PI_CATALOG_MISSING_8A260AD050A84E32996CD633450D8715");
        catalog.register_endpoint(endpoint).unwrap();
        catalog.register_model((*model.spec).clone()).unwrap();
        extensions.refresh_pi_model_catalog_without_selection(&catalog, None);
        assert_eq!(
            extensions
                .host_state
                .lock()
                .unwrap()
                .pi_models
                .as_ref()
                .unwrap()["configured_auth"],
            serde_json::json!({"fixture": false})
        );
        assert!(extensions
            .host_state
            .lock()
            .unwrap()
            .pi_models
            .as_ref()
            .unwrap()["available_models"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn unrepresentable_unconfigured_models_withdraw_all_but_preserve_available() {
        let mut catalog = credential_catalog();
        let mut extensions = ExecutableExtensions::default();
        extensions.refresh_pi_model_catalog_without_selection(&catalog, None);
        assert!(extensions
            .host_state
            .lock()
            .unwrap()
            .pi_models
            .as_ref()
            .unwrap()
            .get("all_models")
            .is_some());
        let model = catalog
            .resolve(&ModelId("fixture/unconfigured-model".into()))
            .unwrap();
        assert!(!model.endpoint.auth.is_configured());
        let mut spec = (*model.spec).clone();
        spec.display_name =
            Some("x".repeat(octet_agent::extension_process::MAX_EXTENSION_MODEL_FIELD_BYTES + 1));
        assert!(catalog.remove_model_if_endpoint(&spec.id, &spec.endpoint));
        catalog.register_model(spec).unwrap();
        extensions.refresh_pi_model_catalog_without_selection(&catalog, None);
        let state = extensions.host_state.lock().unwrap();
        let snapshot = state.pi_models.as_ref().unwrap();
        assert!(snapshot.get("all_models").is_none());
        assert_eq!(snapshot["all_models_status"]["status"], "unavailable");
        assert_eq!(snapshot["available_models"].as_array().unwrap().len(), 1);
        assert_eq!(snapshot["available_models"][0]["id"], "test-model");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn catalog_publication_reaches_real_adapter_with_extension_and_unconfigured_models() {
        let directory = tempfile::tempdir().unwrap();
        let trace = directory.path().join("inventory.json");
        let entry = directory.path().join("inventory.mjs");
        std::fs::write(
            &entry,
            r#"
import { writeFileSync } from 'node:fs';
export default pi => {
  pi.registerProvider('catalog-extension', {
    baseUrl: 'http://127.0.0.1:9/', api: 'openai-completions', apiKey: 'extension-catalog-secret',
    models: [{ id: 'extension-model', name: 'Extension model', reasoning: false,
      input: ['text'], cost: {input: 0, output: 0, cacheRead: 0, cacheWrite: 0},
      contextWindow: 8192, maxTokens: 1024 }],
    streamSimple: async function* () { throw new Error('inventory must not infer'); },
  });
  pi.registerCommand('inventory', {handler: (_, ctx) => {
    writeFileSync(TRACE, JSON.stringify({all: ctx.modelRegistry.getAll(),
      available: ctx.modelRegistry.getAvailable(),
      found: ctx.modelRegistry.find('fixture', 'unconfigured-model'),
      extension: ctx.modelRegistry.find('catalog-extension', 'extension-model'),
      missing: ctx.modelRegistry.find('fixture', 'missing') === undefined,
      configured: ctx.modelRegistry.hasConfiguredAuth({provider:'fixture'}),
      unknownAuth: ctx.modelRegistry.hasConfiguredAuth({provider:'missing'}),
      extensionAuth: ctx.modelRegistry.hasConfiguredAuth({provider:'catalog-extension'})}));
  }});
};
"#
            .replace("TRACE", &serde_json::to_string(&trace).unwrap()),
        )
        .unwrap();
        let adapter = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../extensions/octet-pi-compat")
            .canonicalize()
            .unwrap();
        let bridge = directory.path().join("bridge.json");
        std::fs::write(
            &bridge,
            serde_json::to_vec(&serde_json::json!({"extensions": [entry]})).unwrap(),
        )
        .unwrap();
        let manifest_path = directory.path().join(EXTENSION_MANIFEST_FILENAME);
        let manifest = ExtensionManifest::parse(&format!(
            r#"
name = "catalog-adapter"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "node"
args = [{runner}, "--config", {bridge}]
[capabilities]
filesystem = "unrestricted"
[contributes]
commands = ["inventory"]
hooks = ["session_start", "session_end"]
providers = true
"#,
            runner = serde_json::to_string(&adapter.join("runner.mjs")).unwrap(),
            bridge = serde_json::to_string(&bridge).unwrap(),
        ))
        .unwrap();
        let mut extensions = ExecutableExtensions::default();
        extensions.host_state.lock().unwrap().session_id = Some("catalog-session".into());
        extensions.host_state.lock().unwrap().has_ui = Some(false);
        let mut runtime = ExtensionRuntimeConfig::new(directory.path());
        runtime.provider_registry = Some(extensions.provider_runtime.registry());
        runtime.host_state = extensions.host_state.lock().unwrap().clone();
        runtime.request_timeout = Duration::from_secs(10);
        runtime.supervise = false;
        let process = ExtensionProcess::start(
            DiscoveredExtension {
                manifest,
                manifest_path,
                source: ExtensionSource::Explicit,
                activation: octet_agent::extension_process::ExtensionActivation {
                    enabled: true,
                    trust: ExtensionTrust::Trusted,
                },
            },
            runtime,
        )
        .await
        .unwrap();
        extensions.processes.push(process.clone());
        extensions
            .provider_runtime
            .await_initial_registrations_async(&extensions.processes)
            .await;
        let fixtures = credential_catalog();
        let mut catalog = ModelCatalog::builtin().unwrap();
        for spec in fixtures.models() {
            let model = fixtures.resolve(&spec.id).unwrap();
            catalog
                .register_endpoint((*model.endpoint).clone())
                .unwrap();
            catalog.register_model((*model.spec).clone()).unwrap();
        }
        let expected_all = catalog.models().count() + 1;
        let expected_available = catalog
            .models()
            .filter(|spec| {
                catalog
                    .resolve(&spec.id)
                    .unwrap()
                    .endpoint
                    .auth
                    .is_configured()
            })
            .count()
            + 1;
        let notices = extensions.provider_runtime.synchronize(
            &mut catalog,
            &AiClient::new(),
            &extensions.processes,
        );
        assert!(notices.is_empty(), "{notices:?}");
        assert!(catalog.models().any(|spec| {
            let model = catalog.resolve(&spec.id).unwrap();
            extensions
                .provider_runtime
                .pi_model_view(&model)
                .is_some_and(|view| {
                    view.provider == "catalog-extension" && view.id == "extension-model"
                })
        }));
        extensions.refresh_pi_model_catalog_without_selection(&catalog, None);
        let context = process.current_context_for_resource_owner("catalog-owner");
        assert_eq!(context.host.has_ui, Some(false));
        let mut command_events = process.subscribe();
        process
            .execute_command("inventory", Vec::new(), context)
            .await
            .unwrap();
        while let Ok(event) = command_events.try_recv() {
            assert!(!matches!(
                event,
                octet_agent::extension_process::ExtensionEvent::ComposerRequested { .. }
            ));
        }
        let captured = std::fs::read_to_string(trace).unwrap();
        let snapshot: Value = serde_json::from_str(&captured).unwrap();
        assert_eq!(snapshot["all"].as_array().unwrap().len(), expected_all);
        assert_eq!(
            snapshot["available"].as_array().unwrap().len(),
            expected_available
        );
        let published_bytes = serde_json::to_vec(
            extensions
                .host_state
                .lock()
                .unwrap()
                .pi_models
                .as_ref()
                .unwrap(),
        )
        .unwrap()
        .len();
        assert!(published_bytes <= 256 * 1024);
        assert_eq!(snapshot["found"]["id"], "unconfigured-model");
        assert_eq!(snapshot["extension"]["id"], "extension-model");
        assert_eq!(snapshot["missing"], true);
        assert_eq!(snapshot["configured"], true);
        assert_eq!(snapshot["unknownAuth"], false);
        assert_eq!(snapshot["extensionAuth"], true);
        for withheld in [
            "catalog-fixture-secret",
            "catalog-header-secret",
            "extension-catalog-secret",
            "x-fixture-auth",
            "unconfigured.example.test",
        ] {
            assert!(!captured.contains(withheld), "{withheld}");
        }
        assert!(process.shutdown().await);
    }

    #[test]
    fn unrepresentable_scopes_withdraw_old_facts_instead_of_inventing_empty_scope() {
        let catalog = ModelCatalog::builtin().unwrap();
        let current = catalog.resolve(&ModelId("gpt-6-astra".into())).unwrap();
        let mut extensions = ExecutableExtensions::default();
        for (id, reasoning) in [
            ("gpt-6-astra", "on"),
            ("gpt-6-astra", "ultra"),
            ("gpt-6-astra", "budget=1337"),
            ("missing/model", "high"),
        ] {
            extensions.refresh_pi_model_catalog(&catalog, None, &current);
            let scope = [crate::cli::parity::ScopedModel {
                id: ModelId(id.into()),
                pattern: id.into(),
                reasoning: Some(reasoning.into()),
            }];
            extensions.refresh_pi_model_catalog(&catalog, Some(&scope), &current);
            let state = extensions.host_state.lock().unwrap();
            let snapshot = state.pi_models.as_ref().unwrap();
            assert!(snapshot.get("scoped_models").is_none(), "{id}:{reasoning}");
            assert!(snapshot.get("available_models").is_some());
            assert_eq!(snapshot["all_models_status"]["status"], "catalog");
            assert_eq!(
                snapshot["all_models"].as_array().unwrap().len(),
                catalog.models().count()
            );
        }
    }

    #[test]
    fn overbudget_model_facts_withdraw_the_whole_snapshot_without_truncation() {
        let catalog = ModelCatalog::builtin().unwrap();
        let current = catalog.resolve(&ModelId("gpt-6-astra".into())).unwrap();
        let mut extensions = ExecutableExtensions::default();
        extensions.refresh_pi_model_catalog(&catalog, None, &current);
        assert!(extensions.host_state.lock().unwrap().pi_models.is_some());
        let scope = vec![
            crate::cli::parity::ScopedModel {
                id: current.spec.id.clone(),
                pattern: "gpt-6-astra".into(),
                reasoning: Some("high".into()),
            };
            2048
        ];
        extensions.refresh_pi_model_catalog(&catalog, Some(&scope), &current);
        assert!(extensions.host_state.lock().unwrap().pi_models.is_none());
        extensions.refresh_pi_model_catalog(&catalog, None, &current);
        assert_eq!(
            extensions
                .host_state
                .lock()
                .unwrap()
                .pi_models
                .as_ref()
                .unwrap()["scoped_models"],
            serde_json::json!([])
        );
    }
}
