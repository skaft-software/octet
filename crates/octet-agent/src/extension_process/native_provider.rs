use super::*;
use futures_util::StreamExt;

mod custom_http;

#[allow(missing_docs)]
#[derive(Clone)]
pub struct NativeProviderCatalog {
    pub models: Vec<(String, octet_ai::Model, ExtensionModelView)>,
}

impl NativeProviderCatalog {
    pub(super) fn model(&self, provider: &str, id: &str) -> Option<octet_ai::Model> {
        let mut matches = self
            .models
            .iter()
            .filter(|(name, model, _)| name == provider && model.spec.api_name == id);
        let model = matches.next()?.1.clone();
        matches.next().is_none().then_some(model)
    }

    pub(super) fn views(&self, provider: &str) -> serde_json::Value {
        serde_json::Value::Array(self.models.iter().filter(|(name, _, _)| name == provider).map(|(_, _, view)| {
            serde_json::json!({
                "id": view.id, "name": view.name.as_deref().unwrap_or(&view.id), "api": view.api, "provider": view.provider,
                "reasoning": view.reasoning, "input": view.input,
                "contextWindow": view.context_window, "maxTokens": view.max_tokens,
                "cost": view.cost.as_ref().map(|cost| serde_json::json!({
                    "input": cost.input as f64 / 1e6, "output": cost.output as f64 / 1e6,
                    "cacheRead": cost.cache_read as f64 / 1e6, "cacheWrite": cost.cache_write as f64 / 1e6,
                })),
            })
        }).collect())
    }
}

pub(super) struct NativeDelegate {
    pub(super) continuation: StdMutex<Option<octet_ai::NativeRequestContinuation>>,
    pub(super) max_output_tokens: u64,
    pub(super) custom_http: bool,
    pub(super) original: octet_ai::Request,
    pub(super) connection: Weak<ProcessConnection>,
    pub(super) route: crate::ExtensionProviderRoute,
    pub(super) used: AtomicBool,
    pub(super) response: StdMutex<Option<octet_ai::Response>>,
    pub(super) cancelled: Arc<Notify>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DelegateRequest {
    stream_id: String,
    request: octet_ai::Request,
    on_payload: bool,
    #[serde(default)]
    custom_http: bool,
}

struct NativePayloadHook {
    connection: Weak<ProcessConnection>,
    stream_id: String,
    custom_http: bool,
}

#[async_trait::async_trait]
impl octet_ai::ProviderRequestHook for NativePayloadHook {
    async fn before_request(
        &self,
        _: &octet_ai::ProviderRequestContext,
        payload: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, AiError> {
        let connection = self
            .connection
            .upgrade()
            .ok_or_else(provider_unavailable_error)?;
        let mut expected = payload.clone();
        let response = connection
            .request(
                "provider/delegate_payload",
                serde_json::json!({"stream_id": self.stream_id, "payload": payload}),
                Duration::from_secs(5),
            )
            .await
            .map_err(|_| provider_unavailable_error())?;
        let value = response
            .as_object()
            .filter(|value| value.len() == 1)
            .and_then(|value| value.get("payload"))
            .cloned()
            .ok_or_else(provider_unavailable_error)?;
        if protected_wire_content(&expected) != protected_wire_content(&value) {
            return Err(provider_unavailable_error());
        }
        if self.custom_http
            && value.get("prompt_cache_key").is_some_and(|key| {
                key.as_str()
                    .is_none_or(|key| key.len() > 128 || key.chars().any(char::is_control))
            })
        {
            return Err(provider_unavailable_error());
        }
        let mut actual = value.clone();
        for payload in [&mut expected, &mut actual] {
            let object = payload
                .as_object_mut()
                .ok_or_else(provider_unavailable_error)?;
            object.remove("messages");
            object.remove("system");
            if self.custom_http {
                object.remove("prompt_cache_key");
            }
        }
        if expected != actual {
            return Err(provider_unavailable_error());
        }
        Ok(Some(value))
    }
}

fn protected_wire_content(value: &serde_json::Value) -> Vec<serde_json::Value> {
    fn collect(value: &serde_json::Value, values: &mut Vec<serde_json::Value>) {
        match value {
            serde_json::Value::Object(object) => {
                if matches!(
                    object.get("type").and_then(serde_json::Value::as_str),
                    Some("thinking" | "redacted_thinking" | "reasoning")
                ) {
                    values.push(value.clone());
                } else {
                    for (key, value) in object {
                        if key == "cache_control" {
                            values.push(serde_json::json!({"cache_control": value}));
                        } else {
                            collect(value, values);
                        }
                    }
                }
            }
            serde_json::Value::Array(array) => {
                array.iter().for_each(|value| collect(value, values))
            }
            _ => {}
        }
    }
    let mut values = Vec::new();
    collect(value, &mut values);
    values
}

fn native_reasoning_preserved(
    original: &[octet_ai::Message],
    proposed: &[octet_ai::Message],
) -> bool {
    fn states(messages: &[octet_ai::Message]) -> Vec<serde_json::Value> {
        messages
            .iter()
            .filter_map(|message| match message {
                octet_ai::Message::Assistant(assistant) => Some(assistant),
                _ => None,
            })
            .flat_map(|assistant| {
                assistant
                    .content
                    .iter()
                    .enumerate()
                    .filter_map(move |(index, part)| match part {
                        octet_ai::AssistantPart::Reasoning(reasoning)
                            if reasoning.state.is_some() =>
                        {
                            Some(serde_json::json!([
                                assistant.model,
                                assistant.protocol,
                                part
                            ]))
                        }
                        octet_ai::AssistantPart::ProviderMetadata(_) => Some(serde_json::json!([
                            assistant.model,
                            assistant.protocol,
                            part,
                            assistant.content.get(index + 1)
                        ])),
                        _ => None,
                    })
            })
            .collect()
    }
    states(original) == states(proposed)
}

pub(super) fn inherited_models(
    state: &ProtocolReadState,
    params: serde_json::Value,
) -> Result<serde_json::Value, ProviderHostResponseError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Request {
        provider_id: String,
    }
    let request: Request =
        serde_json::from_value(params).map_err(|_| ProviderHostResponseError::Invalid)?;
    if request.provider_id.is_empty()
        || request.provider_id.len() > 64
        || !request.provider_id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        return Err(ProviderHostResponseError::Invalid);
    }
    let registry = provider_registry_for_request(state)?;
    let native = registry
        .native_catalog()
        .ok_or(ProviderHostResponseError::Unavailable)?;
    let models = native.views(&request.provider_id);
    if models.as_array().is_none_or(|models| models.len() > 256)
        || serde_json::to_vec(&models)
            .map_err(|_| ProviderHostResponseError::Unavailable)?
            .len()
            > 256 * 1024
    {
        return Err(ProviderHostResponseError::ResourceExhausted);
    }
    Ok(serde_json::json!({"models": models}))
}

pub(super) fn dispatch_delegate(
    state: &ProtocolReadState,
    object: &serde_json::Map<String, serde_json::Value>,
    params: serde_json::Value,
) -> Result<(), String> {
    let id = parse_child_request_id(object, "provider/delegate")?;
    insert_child_request(state, id.clone(), None, None)?;
    let request: DelegateRequest = match serde_json::from_value(params) {
        Ok(request) => request,
        Err(_) => {
            return queue_provider_host_response(
                state,
                &id,
                Err(ProviderHostResponseError::Invalid),
            )
        }
    };
    let delegate = lock_std_mutex(&state.provider_streams)
        .get(&request.stream_id)
        .and_then(|stream| stream.delegate.clone());
    let Some(delegate) = delegate else {
        return queue_provider_host_response(
            state,
            &id,
            Err(ProviderHostResponseError::Unavailable),
        );
    };
    let mut comparison = request.request.clone();
    comparison.messages = delegate.original.messages.clone();
    comparison.system = delegate.original.system.clone();
    comparison.max_output_tokens = delegate.original.max_output_tokens;
    comparison.temperature = delegate.original.temperature;
    let valid = request.custom_http == delegate.custom_http
        && serde_json::to_value(comparison).ok() == serde_json::to_value(&delegate.original).ok()
        && request.request.max_output_tokens.map_or(
            delegate.original.max_output_tokens.is_none(),
            |limit| {
                limit > 0
                    && limit
                        <= delegate
                            .original
                            .max_output_tokens
                            .unwrap_or(delegate.max_output_tokens)
            },
        )
        && request.request.temperature.is_none_or(|temperature| {
            temperature.is_finite() && (0.0..=2.0).contains(&temperature)
        })
        && native_reasoning_preserved(&delegate.original.messages, &request.request.messages);
    if !valid || delegate.used.swap(true, Ordering::AcqRel) {
        return queue_provider_host_response(state, &id, Err(ProviderHostResponseError::Invalid));
    }
    let Ok(worker) = state.child_work_slots.clone().try_acquire_owned() else {
        return queue_provider_host_response(
            state,
            &id,
            Err(ProviderHostResponseError::ResourceExhausted),
        );
    };
    let writer = state.writer.clone();
    let limit = Arc::clone(&state.frame_limit);
    let children = Arc::clone(&state.child_requests);
    let registry = state
        .provider_registry
        .clone()
        .ok_or("provider registry unavailable")?;
    tokio::spawn(async move {
        let result = async {
            let connection = delegate
                .connection
                .upgrade()
                .ok_or_else(provider_unavailable_error)?;
            if !connection_is_usable(&connection) || !registry.route_is_active(&delegate.route) {
                return Err(provider_unavailable_error());
            }
            let mut hooks: Vec<Arc<dyn octet_ai::ProviderRequestHook>> = Vec::new();
            if request.on_payload {
                hooks.push(Arc::new(NativePayloadHook {
                    connection: delegate.connection.clone(),
                    stream_id: request.stream_id.clone(),
                    custom_http: delegate.custom_http,
                }));
            }
            let continuation = lock_std_mutex(&delegate.continuation)
                .take()
                .ok_or_else(provider_unavailable_error)?;
            let mut stream = if delegate.custom_http {
                continuation
                    .stream_custom_http(
                        request.request,
                        hooks,
                        Arc::new(custom_http::ProcessHttpSender {
                            connection: delegate.connection.clone(),
                            stream_id: request.stream_id.clone(),
                        }),
                    )
                    .await?
            } else {
                continuation.stream(request.request, hooks).await?
            };
            while let Some(event) = stream.next().await {
                let event = event?;
                if let Some((kind, payload)) = native_stream_event(&event)? {
                    let reply = connection.request("provider/delegate_event", serde_json::json!({"stream_id": request.stream_id, "kind": kind, "payload": payload}), Duration::from_secs(5)).await.map_err(|_| provider_unavailable_error())?;
                    if reply != serde_json::json!({"accepted":true}) {
                        return Err(provider_unavailable_error());
                    }
                }
                if let StreamEvent::Finished(response) = event {
                    let value = serde_json::json!({"message": response.message, "usage": response.usage, "stop_reason": response.stop_reason.as_canonical(), "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis()});
                    if serde_json::to_vec(&value)
                        .map_err(|_| provider_unavailable_error())?
                        .len()
                        > 786432
                    {
                        return Err(provider_unavailable_error());
                    }
                    *lock_std_mutex(&delegate.response) = Some(response);
                    return Ok(value);
                }
            }
            Err(provider_unavailable_error())
        };
        let result = tokio::select! {
            biased;
            _ = delegate.cancelled.notified() => Err(provider_unavailable_error()),
            _ = registry.route_invalidated(&delegate.route) => Err(provider_unavailable_error()),
            result = tokio::time::timeout(delegate.connection.upgrade().map(|connection| connection.provider_stream_deadline).unwrap_or(Duration::ZERO), result) => result.unwrap_or_else(|_| Err(provider_unavailable_error())),
        };
        let response = match result {
            Ok(result) => serde_json::json!({"jsonrpc":"2.0", "id": id, "result": result}),
            Err(_) => {
                serde_json::json!({"jsonrpc":"2.0", "id": id, "error":{"code":-32002,"message":"native provider attempt failed"}})
            }
        };
        let _ = queue_writer_value(&writer, &limit, response);
        settle_child_request(&children, &id);
        drop(worker);
    });
    Ok(())
}

fn native_stream_event(
    event: &StreamEvent,
) -> Result<Option<(&'static str, serde_json::Value)>, AiError> {
    let event = match event {
        StreamEvent::Started { .. } => ("started", serde_json::json!({})),
        StreamEvent::TextStart { index } => ("text_start", serde_json::json!({"index":index})),
        StreamEvent::TextDelta { index, delta } => (
            "text_delta",
            serde_json::json!({"index":index,"delta":delta}),
        ),
        StreamEvent::TextEnd { index } => ("text_end", serde_json::json!({"index":index})),
        StreamEvent::ReasoningStart { index } => {
            ("reasoning_start", serde_json::json!({"index":index}))
        }
        StreamEvent::ReasoningDelta { index, delta } => (
            "reasoning_delta",
            serde_json::json!({"index":index,"delta":delta}),
        ),
        StreamEvent::ReasoningEnd { index } => {
            ("reasoning_end", serde_json::json!({"index":index}))
        }
        StreamEvent::ToolCallStart {
            index,
            id,
            name,
            async_execution: false,
        } => (
            "tool_call_start",
            serde_json::json!({"index":index,"id":id,"name":name}),
        ),
        StreamEvent::ToolCallArgsDelta { index, delta } => (
            "tool_call_args_delta",
            serde_json::json!({"index":index,"delta":delta}),
        ),
        StreamEvent::ToolCallEnd {
            index,
            argument_error: None,
        } => ("tool_call_end", serde_json::json!({"index":index})),
        StreamEvent::ProviderLifecycle(_) | StreamEvent::Usage(_) | StreamEvent::Finished(_) => {
            return Ok(None)
        }
        _ => return Err(provider_unavailable_error()),
    };
    Ok(Some(event))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_pi_pricing_tiers_use_strict_cached_input_thresholds() {
        let metadata: PiProviderModelMetadata = serde_json::from_value(serde_json::json!({
            "base_url":"", "input":["text"], "pricing":{
                "input":1_000_000, "output":2_000_000, "cache_read":100_000,
                "cache_write_5m":1_250_000, "cache_write_1h":null, "reasoning":null,
                "tiers":[
                    {"min_input_tokens":201,"input":6_000_000,"output":12_000_000,"cache_read":600_000,"cache_write_5m":7_500_000,"cache_write_1h":null,"reasoning":null},
                    {"min_input_tokens":101,"input":3_000_000,"output":6_000_000,"cache_read":300_000,"cache_write_5m":3_750_000,"cache_write_1h":null,"reasoning":null}
                ]
            }
        })).unwrap();
        metadata.validate().unwrap();
        let pricing = metadata.pricing.as_ref().unwrap();
        for (input_tokens, cache_write_1h_tokens, total, remainder) in [
            (70, 0, 104, 500_000),
            (71, 0, 316, 500_000),
            (71, 5, 327, 750_000),
            (171, 0, 1233, 0),
        ] {
            let usage = octet_ai::Usage {
                input_tokens,
                output_tokens: 10,
                cache_read_tokens: 20,
                cache_write_tokens: 10,
                cache_write_1h_tokens,
                total_tokens: input_tokens + 40,
                ..Default::default()
            };
            let cost = octet_ai::pricing::cost_of(pricing, &usage).unwrap();
            assert_eq!(
                (cost.total, cost.total_picodollars_remainder),
                (total, remainder)
            );
        }
        let mut invalid = metadata.clone();
        invalid.pricing.as_mut().unwrap().tiers[0].input = None;
        assert!(invalid.validate().is_err());
        let mut invalid = metadata.clone();
        invalid.pricing.as_mut().unwrap().tiers[0].min_input_tokens = 0;
        assert!(invalid.validate().is_err());
        let mut invalid = metadata.clone();
        invalid.pricing.as_mut().unwrap().tiers[0].reasoning = Some(octet_ai::TokenRate(1));
        assert!(invalid.validate().is_err());
        let mut invalid = metadata;
        invalid.pricing.as_mut().unwrap().tiers[0].cache_write_1h = Some(octet_ai::TokenRate(1));
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn native_delegate_preserves_opaque_reasoning_and_its_producer() {
        let message = serde_json::json!({"Assistant":{"model":"native-model","protocol":"anthropic_messages","content":[
            {"Reasoning":{"text":"native thought","state":{"model":"native-model","protocol":"anthropic_messages","kind":{"AnthropicSignature":{"signature":"native-signature"}}}}},
            {"Text":"editable text"}
        ]}});
        let original: Vec<octet_ai::Message> =
            serde_json::from_value(serde_json::json!([message.clone()])).unwrap();
        assert!(native_reasoning_preserved(&original, &original));
        let mut edited = message.clone();
        edited["Assistant"]["content"][1] = serde_json::json!({"Text":"changed plain text"});
        let edited: Vec<octet_ai::Message> =
            serde_json::from_value(serde_json::json!([edited])).unwrap();
        assert!(native_reasoning_preserved(&original, &edited));
        for mutation in ["signature", "text", "producer", "remove", "duplicate"] {
            let mut changed = message.clone();
            match mutation {
                "signature" => {
                    changed["Assistant"]["content"][0]["Reasoning"]["state"]["kind"]
                        ["AnthropicSignature"]["signature"] = serde_json::json!("forged")
                }
                "text" => {
                    changed["Assistant"]["content"][0]["Reasoning"]["text"] =
                        serde_json::json!("forged")
                }
                "producer" => changed["Assistant"]["model"] = serde_json::json!("different-model"),
                "remove" => {
                    changed["Assistant"]["content"]
                        .as_array_mut()
                        .unwrap()
                        .remove(0);
                }
                "duplicate" => {
                    let part = changed["Assistant"]["content"][0].clone();
                    changed["Assistant"]["content"]
                        .as_array_mut()
                        .unwrap()
                        .push(part);
                }
                _ => unreachable!(),
            };
            let changed: Vec<octet_ai::Message> =
                serde_json::from_value(serde_json::json!([changed])).unwrap();
            assert!(
                !native_reasoning_preserved(&original, &changed),
                "{mutation}"
            );
        }
        assert!(!native_reasoning_preserved(&[], &original));
    }

    #[test]
    fn native_payload_hooks_cannot_change_reasoning_or_cache_policy() {
        let original = serde_json::json!({"model":"native-model","system":[{"type":"text","text":"original system","cache_control":{"type":"ephemeral"}}],
            "messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"native thought","signature":"native-signature"},{"type":"text","text":"editable"}]}]});
        let mut changed = original.clone();
        changed["system"][0]["text"] = serde_json::json!("wrapped system");
        changed["messages"][0]["content"][1]["text"] = serde_json::json!("wrapped text");
        assert_eq!(
            protected_wire_content(&original),
            protected_wire_content(&changed)
        );
        changed["messages"][0]["content"][0]["signature"] = serde_json::json!("forged");
        assert_ne!(
            protected_wire_content(&original),
            protected_wire_content(&changed)
        );
        let mut changed = original.clone();
        changed["system"][0]["cache_control"]["ttl"] = serde_json::json!("1h");
        assert_ne!(
            protected_wire_content(&original),
            protected_wire_content(&changed)
        );
        let changed = serde_json::json!({"system":"wrapper deleted caching and signed reasoning"});
        assert_ne!(
            protected_wire_content(&original),
            protected_wire_content(&changed)
        );
    }

    #[test]
    fn extension_registration_cannot_choose_a_native_model_identity() {
        let mut value = serde_json::json!({
            "provider": {"id":"anthropic","label":"Anthropic","auth":{"kind":"none"}},
            "models": [{"id":"selected","api_name":"native-model","protocol":"anthropic_messages","context_window":8192,"max_output_tokens":1024,
                "capabilities":{"tools":false,"parallel_tool_calls":false,"structured_output":false,"reasoning":false},
                "pi_metadata":{"base_url":"","input":["text"],"pricing":null,"inherit_provider":"anthropic"}}]
        });
        assert!(model_control::parse_provider_proxy_registration(value.clone()).is_ok());
        value["models"][0]["pi_metadata"]["native_model_id"] =
            serde_json::json!("host-owned-native-id");
        assert!(matches!(
            model_control::parse_provider_proxy_registration(value),
            Err(ProviderHostResponseError::Invalid)
        ));
    }

    #[test]
    fn registration_endpoint_labels_preserve_credential_free_url_validation() {
        let mut value = serde_json::json!({
            "provider": {"id":"anthropic","label":"Anthropic","auth":{"kind":"none"}},
            "models": [{"id":"selected","api_name":"custom-model","protocol":"openai_chat","context_window":8192,"max_output_tokens":1024,
                "capabilities":{"tools":false,"parallel_tool_calls":false,"structured_output":false,"reasoning":false},
                "pi_metadata":{"base_url":"","input":["text"],"pricing":null,"inherit_provider":"anthropic"}}]
        });
        for endpoint in [
            "",
            "claude-bridge",
            "https://custom.invalid/v1",
            "http://127.0.0.1:9/v1",
        ] {
            value["models"][0]["pi_metadata"]["base_url"] = serde_json::json!(endpoint);
            assert!(
                model_control::parse_provider_proxy_registration(value.clone()).is_ok(),
                "{endpoint}"
            );
        }
        for endpoint in [
            "https://user:secret@custom.invalid/v1",
            "https://custom.invalid/v1?key=secret",
            "https://custom.invalid/v1#fragment",
            "file:///secret",
            "9invalid-label",
            "invalid label",
        ] {
            value["models"][0]["pi_metadata"]["base_url"] = serde_json::json!(endpoint);
            assert!(
                model_control::parse_provider_proxy_registration(value.clone()).is_err(),
                "{endpoint}"
            );
        }
    }

    #[test]
    fn inheritance_negotiation_requires_offered_native_catalog_and_selected_proxy() {
        let manifest = ExtensionManifest::parse(
            r#"name = "native-inheritance"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "native-inheritance"
[contributes]
providers = true
"#,
        )
        .unwrap();
        for (offered, proxy, expected) in [
            (false, true, false),
            (true, false, false),
            (true, true, true),
        ] {
            let mut features = API_0_2_REQUIRED_FEATURES
                .iter()
                .map(|feature| (*feature).to_owned())
                .collect::<Vec<_>>();
            features.push("provider_inheritance_v1".into());
            if proxy {
                features.push("provider_proxy_v1".into());
            }
            let response = InitializeResponse {
                api_version: "0.4".into(),
                tools: Vec::new(),
                commands: Vec::new(),
                tool_renderers: Vec::new(),
                shortcuts: Vec::new(),
                protocol: Some(ExtensionProtocolResponse {
                    session_snapshot_transport_v1: None,
                    version: "0.4".into(),
                    features,
                    limits: ExtensionProtocolLimits {
                        max_message_bytes: None,
                        resource_refs_v1: None,
                        max_concurrent_requests: 4,
                    },
                    lifecycle_events: Vec::new(),
                }),
            };
            let result = negotiate_contributions_with_host_services(
                &manifest,
                response,
                4,
                OfferedHostServices {
                    provider_proxy: true,
                    provider_inheritance: offered,
                    ..OfferedHostServices::default()
                },
            );
            assert_eq!(
                result.is_ok(),
                expected,
                "offered={offered}, proxy={proxy}: {result:?}"
            );
        }
    }
}
