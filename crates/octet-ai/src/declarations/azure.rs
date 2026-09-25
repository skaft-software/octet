//! Declared Azure Responses routing, resolved without touching credentials.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::DeclarationError;
use crate::{AiError, ConfigError, Model, Protocol, ResponsesRuntimeProfile};

/// Request-local Azure Responses configuration.
///
/// Deployment maps use the logical API model name as their key. Supplying
/// `base_url` or `resource_name` explicitly authorizes that selected destination;
/// environment-only changes may not move credentials to another origin.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AzureRequestOptions {
    /// Deployment used instead of the logical API model name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment_name: Option<String>,
    /// Model-to-deployment mappings, overriding the environment map.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub deployment_map: BTreeMap<String, String>,
    /// Explicit endpoint base URL; selecting it authorizes an origin change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Azure resource name, used when neither an explicit nor environment base URL is selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_name: Option<String>,
    /// Non-secret API version query value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_version: Option<String>,
}

impl std::fmt::Debug for AzureRequestOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AzureRequestOptions")
            .field("configuration", &"<redacted>")
            .finish_non_exhaustive()
    }
}

fn invalid() -> DeclarationError {
    DeclarationError::Invalid("invalid Azure Responses configuration".into())
}

fn valid_name(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn valid_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

fn valid_resource(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn parse_base(value: &str) -> Result<url::Url, DeclarationError> {
    let mut url = url::Url::parse(value.trim()).map_err(|_| invalid())?;
    let query = url.query_pairs().collect::<Vec<_>>();
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || (url.query().is_some()
            && !(query.len() == 1 && query[0].0 == "api-version" && valid_version(&query[0].1)))
    {
        return Err(invalid());
    }
    let azure_host = url.host_str().is_some_and(|host| {
        [
            ".openai.azure.com",
            ".cognitiveservices.azure.com",
            ".ai.azure.com",
        ]
        .iter()
        .any(|suffix| host.ends_with(suffix))
    });
    let path = url.path().trim_end_matches('/');
    if azure_host && matches!(path, "" | "/openai" | "/openai/v1/responses") {
        url.set_path("/openai/v1/");
    } else if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

impl AzureRequestOptions {
    /// Validates size, deployment names, endpoint syntax and version/resource values.
    pub fn validate(&self) -> Result<(), DeclarationError> {
        super::check_object_size(self)?;
        if self
            .deployment_name
            .as_deref()
            .is_some_and(|v| !valid_name(v))
            || self
                .deployment_map
                .iter()
                .any(|(key, value)| !valid_name(key) || !valid_name(value))
            || self
                .resource_name
                .as_deref()
                .is_some_and(|v| !valid_resource(v))
            || self
                .api_version
                .as_deref()
                .is_some_and(|v| !valid_version(v))
        {
            return Err(invalid());
        }
        if let Some(base) = &self.base_url {
            parse_base(base)?;
        }
        Ok(())
    }
}

fn map_from_environment(value: &str) -> Result<BTreeMap<String, String>, AiError> {
    let mut map = BTreeMap::new();
    for entry in value.split(',').map(str::trim).filter(|v| !v.is_empty()) {
        let Some((key, value)) = entry.split_once('=') else {
            return Err(config_error());
        };
        let (key, value) = (key.trim(), value.trim());
        if !valid_name(key) || !valid_name(value) {
            return Err(config_error());
        }
        if map
            .insert(key.to_owned(), value.to_owned())
            .is_some_and(|previous| previous != value)
        {
            return Err(config_error());
        }
    }
    Ok(map)
}

fn config_error() -> AiError {
    ConfigError::Parse("invalid Azure Responses configuration".into()).into()
}

/// Applies only declared Azure routing. The immutable catalog and canonical
/// request identity/limits are not changed; the private clone gets wire routing.
pub(crate) fn apply(
    model: &mut Model,
    options: Option<&AzureRequestOptions>,
    env: &BTreeMap<String, String>,
) -> Result<(), AiError> {
    if model.endpoint.runtime.responses_profile != ResponsesRuntimeProfile::Azure {
        return if options.is_some() {
            Err(ConfigError::Parse(
                "Azure overrides require a declared Azure Responses profile".into(),
            )
            .into())
        } else {
            Ok(())
        };
    }
    if model.spec.protocol != Protocol::OpenAiResponses {
        return Err(config_error());
    }
    let defaults = AzureRequestOptions::default();
    let options = options.unwrap_or(&defaults);
    options.validate().map_err(|_| config_error())?;
    let read = |name: &str| -> Result<Option<String>, AiError> {
        let value = crate::auth::read_request_env(env, name)?;
        Ok(value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()))
    };
    // The authorization to move origins comes from the SELECTED explicit field,
    // not an unrelated explicit resource hidden by an environment base URL.
    let (base, explicit_destination) = if let Some(base) = &options.base_url {
        (base.clone(), true)
    } else if let Some(base) = read("AZURE_OPENAI_BASE_URL")? {
        (base, false)
    } else if let Some(resource) = &options.resource_name {
        (
            format!("https://{resource}.openai.azure.com/openai/v1/"),
            true,
        )
    } else if let Some(resource) = read("AZURE_OPENAI_RESOURCE_NAME")? {
        if !valid_resource(&resource) {
            return Err(config_error());
        }
        (
            format!("https://{resource}.openai.azure.com/openai/v1/"),
            false,
        )
    } else {
        (model.endpoint.base_url.to_string(), false)
    };
    let mut url = parse_base(&base).map_err(|_| config_error())?;
    if url.origin() != model.endpoint.base_url.origin() && !explicit_destination {
        return Err(ConfigError::Parse("Azure environment cannot change credential origin; select an explicit Azure base_url or resource_name".into()).into());
    }
    let configured_version = url
        .query_pairs()
        .find(|(name, _)| name == "api-version")
        .map(|(_, value)| value.into_owned());
    let version = if let Some(version) = &options.api_version {
        version.clone()
    } else {
        read("AZURE_OPENAI_API_VERSION")?
            .or(configured_version)
            .unwrap_or_else(|| "v1".into())
    };
    if !valid_version(&version) {
        return Err(config_error());
    }
    url.set_query(None);
    url.query_pairs_mut().append_pair("api-version", &version);
    let deployment = if let Some(deployment) = &options.deployment_name {
        deployment.clone()
    } else if let Some(deployment) = options.deployment_map.get(&model.spec.api_name) {
        deployment.clone()
    } else {
        read("AZURE_OPENAI_DEPLOYMENT_NAME_MAP")?
            .map(|value| map_from_environment(&value))
            .transpose()?
            .and_then(|map| map.get(&model.spec.api_name).cloned())
            .unwrap_or_else(|| model.spec.api_name.clone())
    };
    if !valid_name(&deployment) {
        return Err(config_error());
    }
    Arc::make_mut(&mut model.endpoint).base_url = url;
    Arc::make_mut(&mut model.spec).api_name = deployment;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn azure_base_normalization_is_host_specific_and_rejects_query_credentials() {
        for suffix in [
            "openai.azure.com",
            "cognitiveservices.azure.com",
            "ai.azure.com",
        ] {
            for path in ["", "/", "/openai/", "/openai/v1/responses"] {
                let url = parse_base(&format!("https://resource.{suffix}{path}")).unwrap();
                assert_eq!(url.path(), "/openai/v1/");
            }
        }
        let custom = parse_base("https://proxy.example/custom%20path?api-version=v1").unwrap();
        assert_eq!(custom.path(), "/custom%20path/");
        assert_eq!(custom.query(), Some("api-version=v1"));
        for url in [
            "https://user:secret@resource.openai.azure.com/",
            "https://resource.openai.azure.com/?secret=value",
            "https://resource.openai.azure.com/#secret",
        ] {
            assert!(parse_base(url).is_err());
        }
    }

    #[test]
    fn explicit_resource_version_and_deployment_mapping_are_resolved_without_network() {
        let mut model = crate::ModelCatalog::builtin()
            .unwrap()
            .resolve(&crate::ModelId("gpt-4o-mini".into()))
            .unwrap();
        Arc::make_mut(&mut model.spec).protocol = Protocol::OpenAiResponses;
        Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
            ResponsesRuntimeProfile::Azure;
        let api_name = model.spec.api_name.clone();
        let mut env: BTreeMap<String, String> = [
            "AZURE_OPENAI_BASE_URL",
            "AZURE_OPENAI_RESOURCE_NAME",
            "AZURE_OPENAI_API_VERSION",
            "AZURE_OPENAI_DEPLOYMENT_NAME_MAP",
        ]
        .into_iter()
        .map(|name| (name.into(), String::new()))
        .collect();
        let options = AzureRequestOptions {
            resource_name: Some("selected-resource".into()),
            ..Default::default()
        };
        apply(&mut model, Some(&options), &env).unwrap();
        assert_eq!(
            model.endpoint.base_url.as_str(),
            "https://selected-resource.openai.azure.com/openai/v1/?api-version=v1"
        );
        assert_eq!(model.spec.api_name, api_name);
        env.insert(
            "AZURE_OPENAI_API_VERSION".into(),
            "2025-04-01-preview".into(),
        );
        env.insert(
            "AZURE_OPENAI_DEPLOYMENT_NAME_MAP".into(),
            format!("{api_name}=chosen"),
        );
        apply(&mut model, None, &env).unwrap();
        assert_eq!(model.spec.api_name, "chosen");
        assert_eq!(
            model.endpoint.base_url.query(),
            Some("api-version=2025-04-01-preview")
        );
    }
}
