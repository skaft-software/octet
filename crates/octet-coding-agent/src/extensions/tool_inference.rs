use super::*;
use octet_agent::tool_inference::{ResolvedToolInferenceModel, ToolInferenceModelResolver};

struct ConfiguredToolInferenceModels {
    models: BTreeMap<(String, String), Option<ResolvedToolInferenceModel>>,
}

impl ToolInferenceModelResolver for ConfiguredToolInferenceModels {
    fn resolve(
        &self,
        provider: &str,
        model: &str,
    ) -> Result<ResolvedToolInferenceModel, octet_agent::ToolError> {
        let resolved = self
            .models
            .get(&(provider.to_owned(), model.to_owned()))
            .and_then(Option::as_ref)
            .ok_or_else(|| octet_agent::ToolError::new("configured inference route unavailable"))?;
        Ok(ResolvedToolInferenceModel {
            model: resolved.model.clone(),
            fetch: resolved.fetch.clone(),
        })
    }
}

impl ExecutableExtensions {
    pub(crate) fn tool_inference_model_resolver(
        &self,
        catalog: &ModelCatalog,
    ) -> Arc<dyn ToolInferenceModelResolver> {
        let mut models = BTreeMap::new();
        for spec in catalog.models() {
            let Ok(model) = catalog.resolve(&spec.id) else {
                continue;
            };
            let Some(view) = self.provider_runtime.pi_model_view(&model) else {
                continue;
            };
            let Ok(fetch) = self.prepare_selected_provider_transport(&model) else {
                continue;
            };
            if fetch.is_none() && !model.endpoint.auth.is_configured() {
                continue;
            }
            let key = (view.provider, view.id);
            match models.entry(key) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(Some(ResolvedToolInferenceModel { model, fetch }));
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    entry.insert(None);
                }
            }
        }
        Arc::new(ConfiguredToolInferenceModels { models })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_inference_resolves_only_exact_configured_native_routes() {
        let builtin = ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("gpt-6-astra".into()))
            .unwrap();
        let mut spec = (*builtin.spec).clone();
        spec.id = ModelId("fixture/configured".into());
        spec.api_name = "configured".into();
        spec.endpoint = EndpointId("fixture-local".into());
        let mut endpoint = (*builtin.endpoint).clone();
        endpoint.id = spec.endpoint.clone();
        endpoint.base_url = url::Url::parse("http://127.0.0.1:9/v1/").unwrap();
        endpoint.auth = octet_ai::Auth::None;
        endpoint.default_headers = Default::default();
        let mut catalog = ModelCatalog::default();
        catalog.register_endpoint(endpoint).unwrap();
        catalog.register_model(spec.clone()).unwrap();
        let extensions = ExecutableExtensions::default();
        let resolver = extensions.tool_inference_model_resolver(&catalog);
        let route = resolver.resolve("fixture", "configured").unwrap();
        assert_eq!(route.model.spec.id, spec.id);
        assert_eq!(route.model.endpoint.id, spec.endpoint);
        assert!(route.fetch.is_none());
        assert!(resolver.resolve("other", "configured").is_err());
        assert!(resolver.resolve("fixture", "unknown").is_err());
        assert!(resolver
            .resolve("fixture", "http://127.0.0.1:18080/v1")
            .is_err());

        let mut missing_endpoint = (*builtin.endpoint).clone();
        missing_endpoint.id = EndpointId("missing-credential".into());
        missing_endpoint.auth = octet_ai::Auth::bearer_env(
            "OCTET_TOOL_INFERENCE_MISSING_BD37F2CCEDC44797B3E56000EFBB8990",
        );
        spec.id = ModelId("fixture/unconfigured".into());
        spec.api_name = "unconfigured".into();
        spec.endpoint = missing_endpoint.id.clone();
        catalog.register_endpoint(missing_endpoint).unwrap();
        catalog.register_model(spec.clone()).unwrap();
        let resolver = extensions.tool_inference_model_resolver(&catalog);
        assert!(resolver.resolve("fixture", "unconfigured").is_err());
        assert!(resolver.resolve("fixture", "configured").is_ok());

        spec.id = ModelId("fixture/ambiguous".into());
        spec.api_name = "configured".into();
        spec.endpoint = EndpointId("fixture-local".into());
        catalog.register_model(spec).unwrap();
        let resolver = extensions.tool_inference_model_resolver(&catalog);
        assert!(resolver.resolve("fixture", "configured").is_err());
    }
}
