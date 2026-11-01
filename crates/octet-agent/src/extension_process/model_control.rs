//! Bounded owner-scoped model controls on the existing foreground driver.
use super::*;

/// API 0.4-only, secret-free facts needed for a faithful Pi catalog projection.
/// The API 0.3 canonical schema continues to reject this additive field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PiProviderModelMetadata {
    #[allow(missing_docs)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherit_provider: Option<String>,
    #[allow(missing_docs)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_model_id: Option<String>,
    /// Explicit credential-free URL passed only to the process-owned streamer.
    pub base_url: String,
    /// Declared input modalities (text, and optionally image).
    pub input: Vec<String>,
    /// Host-accounted immutable per-million-token rates, in microdollars.
    pub pricing: Option<octet_ai::Pricing>,
}
impl PiProviderModelMetadata {
    /// Validate this system boundary before recording or projecting a route.
    pub fn validate(&self) -> Result<(), String> {
        if self.inherit_provider.as_ref().is_some_and(|provider| {
            provider.is_empty()
                || provider.len() > 64
                || !provider.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'-' | b'_')
                })
        }) {
            return Err("invalid inherited provider".into());
        }
        if self.base_url.len() > 8192 {
            return Err("provider URL exceeds bounds".into());
        }
        let opaque_endpoint = self.base_url.len() <= 64
            && self
                .base_url
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
            && self
                .base_url
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
        if !self.base_url.is_empty() && !opaque_endpoint {
            let url = url::Url::parse(&self.base_url).map_err(|_| "invalid provider URL")?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(
                    "provider URL must be HTTP(S) without credentials, query or fragment".into(),
                );
            }
        }
        if !matches!(self.input.as_slice(), [text] if text == "text")
            && !matches!(self.input.as_slice(), [text, image] if text == "text" && image == "image")
            && !matches!(self.input.as_slice(), [image, text] if text == "text" && image == "image")
        {
            return Err("provider input must contain text and optionally image".into());
        }
        if self.inherit_provider.is_none()
            && self.pricing.as_ref().is_none_or(|pricing| {
                pricing.tiers.len() > 256
                    || pricing.cache_write_1h.is_some()
                    || pricing.reasoning.is_some()
                    || pricing.tiers.iter().any(|tier| {
                        tier.min_input_tokens == 0
                            || tier.min_input_tokens > 9_007_199_254_740_991
                            || tier.input.is_none()
                            || tier.output.is_none()
                            || tier.cache_read.is_none()
                            || tier.cache_write_5m.is_none()
                            || tier.cache_write_1h.is_some()
                            || tier.reasoning.is_some()
                    })
            })
        {
            return Err(
                "provider pricing requires complete bounded tiers without separate reasoning/cache-write rates"
                    .into(),
            );
        }
        Ok(())
    }
}

/// Reuse the canonical declaration validator, then validate the additive Pi
/// facts separately. Credentials and functions never enter the registry.
pub(super) fn parse_provider_proxy_registration(
    mut value: serde_json::Value,
) -> Result<
    (
        api_v03::ProviderRegisterParams,
        BTreeMap<String, PiProviderModelMetadata>,
    ),
    ProviderHostResponseError,
> {
    let models = value
        .get_mut("models")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or(ProviderHostResponseError::Invalid)?;
    let mut facts = Vec::with_capacity(models.len());
    for model in models {
        let fact = model
            .as_object_mut()
            .and_then(|model| model.remove("pi_metadata"))
            .ok_or(ProviderHostResponseError::Invalid)?;
        let fact: PiProviderModelMetadata =
            serde_json::from_value(fact).map_err(|_| ProviderHostResponseError::Invalid)?;
        if fact.native_model_id.is_some() {
            return Err(ProviderHostResponseError::Invalid);
        }
        fact.validate()
            .map_err(|_| ProviderHostResponseError::Invalid)?;
        facts.push(fact);
    }
    let request = api_v03::parse_provider_register_params(value)
        .map_err(|_| ProviderHostResponseError::Invalid)?;
    if request.provider.auth.kind != "none" {
        // These routes execute an already trusted process's custom streamer;
        // they do not obtain any host credential/OAuth authority.
        return Err(ProviderHostResponseError::Invalid);
    }
    let facts = request
        .models
        .iter()
        .zip(facts)
        .map(|(model, metadata)| (model.id.clone(), metadata))
        .collect();
    Ok((request, facts))
}

pub(super) fn register_provider_proxy(
    state: &ProtocolReadState,
    value: serde_json::Value,
    update: bool,
) -> Result<api_v03::ProviderCatalogResult, ProviderHostResponseError> {
    let (mut request, mut metadata) = parse_provider_proxy_registration(value)?;
    if metadata.values().any(|metadata| {
        metadata
            .inherit_provider
            .as_ref()
            .is_some_and(|provider| provider != &request.provider.id)
    }) || (metadata
        .values()
        .any(|metadata| metadata.inherit_provider.is_some())
        && !read_std_lock(&state.protocol).supports("provider_inheritance_v1"))
    {
        return Err(ProviderHostResponseError::Invalid);
    }
    let registry = provider_registry_for_request(state)?;
    for model in &mut request.models {
        let fact = metadata
            .get_mut(&model.id)
            .ok_or(ProviderHostResponseError::Invalid)?;
        if fact.inherit_provider.is_none() {
            continue;
        }
        let native = registry
            .native_catalog()
            .and_then(|catalog| catalog.model(&request.provider.id, &model.api_name))
            .ok_or(ProviderHostResponseError::Unavailable)?;
        model.protocol = match native.spec.protocol {
            Protocol::AnthropicMessages => "anthropic_messages",
            Protocol::OpenAiChat => "openai_chat",
            Protocol::OpenAiResponses => "openai_responses",
            _ => return Err(ProviderHostResponseError::Unavailable),
        }
        .to_owned();
        model.display_name = native.spec.display_name.clone();
        model.context_window = usize::try_from(native.spec.limits.context_window)
            .map_err(|_| ProviderHostResponseError::Invalid)?;
        model.max_output_tokens = usize::try_from(native.spec.limits.max_output_tokens)
            .map_err(|_| ProviderHostResponseError::Invalid)?;
        model.capabilities.tools = native.spec.capabilities.tools;
        model.capabilities.parallel_tool_calls = native.spec.capabilities.parallel_tool_calls;
        model.capabilities.structured_output = native.spec.capabilities.structured_output;
        model.capabilities.reasoning = native.spec.capabilities.reasoning.is_some();
        fact.native_model_id = Some(native.spec.id.0.clone());
        fact.pricing = native.spec.pricing.clone();
    }
    registry
        .replace_pi_provider(state.provider_owner.clone(), request, metadata, update)
        .map_err(provider_registry_response_error)
}

/// Secret-free request for an authoritative host selection.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExtensionModelControl {
    /// Select an already configured provider/model pair.
    Model {
        /// Pi provider identity, not a credential or arbitrary endpoint.
        provider: String,
        /// Provider-facing model identifier.
        id: String,
    },
    /// Change the effective portable reasoning selection.
    Thinking {
        /// One of the seven public Pi thinking levels.
        level: String,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    parent_request_id: u64,
    resource_owner: ExtensionResourceOwner,
    selection: ExtensionModelControl,
}
impl OwnerScopedHostRequest for Request {
    fn parent_request_id(&self) -> u64 {
        self.parent_request_id
    }
    fn request_resource_owner(&self) -> Option<&ExtensionResourceOwner> {
        Some(&self.resource_owner)
    }
}

pub(super) fn dispatch_model_control(
    state: &ProtocolReadState,
    object: &serde_json::Map<String, serde_json::Value>,
    params: serde_json::Value,
) -> Result<(), String> {
    let Some((request, admitted)) = admit_host_request::<Request>(
        state,
        object,
        "model/select",
        EXTENSION_FEATURE_SESSION_CONTROL_V1,
        params,
    )?
    else {
        return Ok(());
    };
    if let Err(failure) = validate_explicit_request_owner(state, &admitted.owner) {
        return refuse_admitted_request(state, &admitted, failure);
    }
    let valid = match &request.selection {
        ExtensionModelControl::Model { provider, id } => [provider, id].into_iter().all(|value| {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        }),
        ExtensionModelControl::Thinking { level } => matches!(
            level.as_str(),
            "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
        ),
    };
    if !valid {
        return refuse_admitted_request(
            state,
            &admitted,
            (
                ExtensionRequestFailure::InvalidRequest,
                "invalid model selection".into(),
            ),
        );
    }
    if lock_std_mutex(&state.tombstones).contains(request.parent_request_id) {
        return refuse_admitted_request(
            state,
            &admitted,
            (
                ExtensionRequestFailure::NotForegroundOwner,
                "model selection parent was cancelled".into(),
            ),
        );
    }
    let Some(service) = state
        .session_lifecycle
        .as_ref()
        .filter(|service| service.supports_model_control())
    else {
        return refuse_admitted_request(
            state,
            &admitted,
            (
                ExtensionRequestFailure::UnsupportedFeature,
                "model selection consumer unavailable".into(),
            ),
        );
    };
    let response_state = {
        let children = lock_std_mutex(&state.child_requests);
        let Some(child) = children.get(&admitted.request_id) else {
            return Ok(());
        };
        Arc::clone(&child.response_state)
    };
    let worker = match state.child_work_slots.clone().try_acquire_owned() {
        Ok(worker) => worker,
        Err(_) => {
            return refuse_admitted_request(
                state,
                &admitted,
                (
                    ExtensionRequestFailure::BoundsExceeded,
                    "model selection worker limit exceeded".into(),
                ),
            )
        }
    };
    let authority = SessionCompactionAuthority {
        parent_request_id: request.parent_request_id,
        callback: false,
        owner: request.resource_owner,
        issued: Arc::clone(&state.issued_resource_owners),
        closed: Arc::clone(&state.closed),
        draining: Arc::clone(&state.draining),
        response: Arc::clone(&response_state),
    };
    let receiver = match service.try_submit_model_control(request.selection, authority) {
        Ok(receiver) => receiver,
        Err(error) => {
            return refuse_admitted_request(
                state,
                &admitted,
                (
                    match error {
                        SessionLifecycleSubmitError::Full => {
                            ExtensionRequestFailure::BoundsExceeded
                        }
                        SessionLifecycleSubmitError::Unavailable => {
                            ExtensionRequestFailure::NotForegroundOwner
                        }
                    },
                    "model selection driver unavailable or full".into(),
                ),
            )
        }
    };
    let writer = state.writer.clone();
    let frame_limit = Arc::clone(&state.frame_limit);
    let child_requests = Arc::clone(&state.child_requests);
    let health = Arc::clone(&state.health);
    let events = state.events.clone();
    let request_id = admitted.request_id;
    tokio::spawn(async move {
        let result = tokio::select! {
            biased;
            _ = child_response_settled(response_state) => return,
            result = receiver => result.unwrap_or_else(|_| Err("model selection consumer retired".into())),
        };
        let response = match result {
            Ok(result) => serde_json::json!({"jsonrpc":"2.0","id":request_id,"result":result}),
            Err(message) => {
                serde_json::json!({"jsonrpc":"2.0","id":request_id,"error":{"code":-32002,"message":message}})
            }
        };
        deliver_api_v03_session_lifecycle_response(
            child_requests,
            request_id,
            writer,
            frame_limit,
            health,
            events,
            response,
        )
        .await;
        drop(worker);
    });
    Ok(())
}
