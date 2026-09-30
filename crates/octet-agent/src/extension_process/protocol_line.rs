//! Handling one protocol line from an extension.

use super::*;

pub(super) fn reject_duplicate_json_keys(line: &[u8]) -> Result<(), serde_json::Error> {
    struct JsonValue;
    struct JsonValueVisitor;

    impl<'de> Deserialize<'de> for JsonValue {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            deserializer.deserialize_any(JsonValueVisitor)
        }
    }

    impl<'de> serde::de::Visitor<'de> for JsonValueVisitor {
        type Value = JsonValue;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a JSON value without duplicate object keys")
        }

        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(JsonValue)
        }

        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(JsonValue)
        }

        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(JsonValue)
        }

        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(JsonValue)
        }

        fn visit_str<E>(self, _: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(JsonValue)
        }

        fn visit_borrowed_str<E>(self, _: &'de str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(JsonValue)
        }

        fn visit_string<E>(self, _: String) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(JsonValue)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(JsonValue)
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            while sequence.next_element::<JsonValue>()?.is_some() {}
            Ok(JsonValue)
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut keys = HashSet::new();
            while let Some(key) = map.next_key::<String>()? {
                if !keys.insert(key.clone()) {
                    return Err(serde::de::Error::custom(format!(
                        "duplicate JSON object key {key:?}"
                    )));
                }
                map.next_value::<JsonValue>()?;
            }
            Ok(JsonValue)
        }
    }

    let mut deserializer = serde_json::Deserializer::from_slice(line);
    JsonValue::deserialize(&mut deserializer)?;
    deserializer.end()
}

pub(super) fn handle_protocol_line(line: &[u8], state: &ProtocolReadState) -> Result<(), String> {
    let is_api_v03 = read_std_lock(&state.protocol).version == EXTENSION_API_VERSION_0_3;
    if is_api_v03 {
        // Deserialize once with a streaming visitor before `Value` normalizes
        // duplicate object keys. Canonical reserialization alone would reject
        // them eventually, but cannot identify the hostile raw wire shape.
        reject_duplicate_json_keys(line)
            .map_err(|error| format!("invalid API 0.3 raw JSON: {error}"))?;
    }
    let value: serde_json::Value =
        serde_json::from_slice(line).map_err(|error| format!("invalid JSON on stdout: {error}"))?;
    if is_api_v03 {
        let canonical = api_v03::canonical_json(&value)
            .map_err(|error| format!("invalid API 0.3 canonical frame: {error}"))?;
        if canonical.as_bytes() != line {
            return Err("API 0.3 frame is not canonical JSON".into());
        }
        api_v03::parse_json_rpc_envelope(value.clone())
            .map_err(|error| format!("invalid API 0.3 JSON-RPC envelope: {error}"))?;
    }
    let object = value
        .as_object()
        .ok_or_else(|| "protocol message must be a JSON object".to_owned())?;
    if object.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
        return Err("protocol message must set jsonrpc to 2.0".into());
    }

    if let Some(method) = object.get("method").and_then(serde_json::Value::as_str) {
        if read_std_lock(&state.protocol).version == EXTENSION_API_VERSION_0_3 {
            let contract = read_std_lock(&state.api_v03_contract)
                .clone()
                .ok_or_else(|| {
                    "API 0.3 extension sent a method before contract initialization".to_owned()
                })?;
            if let Err(error) = api_v03::require_method(
                &contract,
                method,
                api_v03::MethodDirection::ExtensionToHost,
            ) {
                if let Some(id) = object.get("id") {
                    queue_api_v03_unknown_method(state, id.clone(), method)?;
                    return Ok(());
                }
                return Err(format!("API 0.3 method `{method}` rejected: {error}"));
            }
        }
        let params = object
            .get("params")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        match method {
            "bus/declare" | "bus/subscribe" | "bus/unsubscribe" | "bus/publish" if is_api_v03 => {
                let id = parse_child_request_id(object, method)?;
                insert_child_request(state, id.clone(), None, None)?;
                if let Some(bus) = &state.event_bus {
                    bus.dispatch_with_response(state, method, params, |result| {
                        let result = result.map_err(|error| match error.code {
                            -32012 => ProviderHostResponseError::ResourceExhausted,
                            -32011 => ProviderHostResponseError::Unavailable,
                            _ => ProviderHostResponseError::Invalid,
                        });
                        queue_provider_host_response(state, &id, result)
                    })?;
                } else {
                    queue_provider_host_response(
                        state,
                        &id,
                        Err(ProviderHostResponseError::Unavailable),
                    )?;
                }
            }
            methods::NOTIFICATION => {
                require_declared(state.declared.notifications, "notifications")?;
                let notification = serde_json::from_value(params)
                    .map_err(|error| format!("invalid notification: {error}"))?;
                let _ = state
                    .events
                    .send(ExtensionEvent::Notification { notification });
            }
            methods::CONFIRMATION_REQUEST => {
                require_declared(state.declared.confirmations, "confirmations")?;
                let id = object
                    .get("id")
                    .cloned()
                    .ok_or_else(|| "confirmation request requires an id".to_owned())?;
                let request_id: ExtensionRequestId = serde_json::from_value(id)
                    .map_err(|error| format!("invalid confirmation request id: {error}"))?;
                request_id
                    .validate_confirmation_id()
                    .map_err(|error| format!("invalid confirmation request id: {error}"))?;
                let request: ConfirmationRequest = serde_json::from_value(params)
                    .map_err(|error| format!("invalid confirmation request: {error}"))?;
                let Some(registered) = register_child_request(
                    state,
                    request_id.clone(),
                    request.parent_request_id,
                    methods::CONFIRMATION_REQUEST,
                )?
                else {
                    return Ok(());
                };
                let parent_request_id = registered.parent_request_id;
                let progress = registered.progress;
                let response_state = registered.response_state;
                let worker = if progress.is_some() {
                    Some(
                        state
                            .child_work_slots
                            .clone()
                            .try_acquire_owned()
                            .map_err(|_| {
                                settle_child_request(&state.child_requests, &request_id);
                                format!("extension child worker limit {MAX_CHILD_WORKERS} exceeded")
                            })?,
                    )
                } else {
                    None
                };
                if let (Some(progress), Some(_), Some(worker)) =
                    (progress, parent_request_id, worker)
                {
                    let writer = state.writer.clone();
                    let child_requests = Arc::clone(&state.child_requests);
                    let health = Arc::clone(&state.health);
                    let events = state.events.clone();
                    let max_message_bytes = state.max_message_bytes();
                    let response_id = request_id;
                    tokio::spawn(async move {
                        let confirmation = progress.confirmation(
                            request.prompt,
                            request.detail,
                            request.destructive,
                            request.default,
                        );
                        tokio::pin!(confirmation);
                        let confirmed = tokio::select! {
                            confirmed = &mut confirmation => confirmed,
                            _ = child_response_settled(Arc::clone(&response_state)) => return,
                        };
                        let response = try_queue_child_response(
                            &child_requests,
                            &response_id,
                            &writer,
                            max_message_bytes,
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": response_id,
                                "result": {"confirmed": confirmed},
                            }),
                        );
                        if let Err(error) = response {
                            update_health(
                                &health,
                                ExtensionHealthState::Degraded,
                                Some(error.clone()),
                            );
                            let _ = events.send(ExtensionEvent::Diagnostic { message: error });
                            settle_child_request(&child_requests, &response_id);
                        }
                        drop(worker);
                    });
                } else {
                    state
                        .events
                        .send(ExtensionEvent::ConfirmationRequested {
                            request_id,
                            generation: state.generation,
                            parent_request_id,
                            request,
                        })
                        .map_err(|_| {
                            "confirmation request arrived without an active event subscriber"
                                .to_owned()
                        })?;
                }
            }
            methods::CONTEXT_CONTRIBUTION => {
                require_declared(state.declared.context, "context contributions")?;
                let contribution = serde_json::from_value(params)
                    .map_err(|error| format!("invalid context contribution: {error}"))?;
                let _ = state
                    .events
                    .send(ExtensionEvent::ContextContributed { contribution });
            }
            methods::STATUS_CONTRIBUTION => {
                let contribution = serde_json::from_value(params)
                    .map_err(|error| format!("invalid status contribution: {error}"))?;
                let ExtensionStatusContribution { surface, .. } = &contribution;
                require_declared(state.declared.ui.contains(surface), "UI contributions")?;
                let _ = state
                    .events
                    .send(ExtensionEvent::StatusContributed { contribution });
            }
            methods::UI_CONTRIBUTION => {
                require_feature(state, EXTENSION_FEATURE_SEMANTIC_UI)?;
                let contribution: ExtensionUiContribution = serde_json::from_value(params)
                    .map_err(|error| format!("invalid semantic UI contribution: {error}"))?;
                contribution
                    .validate()
                    .map_err(|error| format!("invalid semantic UI contribution: {error}"))?;
                let _ = state.events.send(ExtensionEvent::UiContributed {
                    generation: state.generation,
                    contribution,
                });
            }
            methods::UI_EDITOR => {
                require_feature(state, EXTENSION_FEATURE_EDITOR_HANDOFF)?;
                let id = parse_child_request_id(object, methods::UI_EDITOR)?;
                let request: ExtensionEditorRequest = serde_json::from_value(params)
                    .map_err(|error| format!("invalid editor request: {error}"))?;
                request
                    .validate()
                    .map_err(|error| format!("invalid editor request: {error}"))?;
                let _registered = insert_child_request(state, id.clone(), None, None)?;
                if state
                    .events
                    .send(ExtensionEvent::EditorRequested {
                        request_id: id.clone(),
                        generation: state.generation,
                        request,
                    })
                    .is_err()
                {
                    try_queue_child_response(
                        &state.child_requests,
                        &id,
                        &state.writer,
                        state.max_message_bytes(),
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {
                                "code": -32000,
                                "message": "no active host-owned editor frontend",
                            },
                        }),
                    )?;
                }
            }
            methods::AUTOCOMPLETE_REGISTER => {
                require_feature(state, EXTENSION_FEATURE_AUTOCOMPLETE)?;
                let id = parse_child_request_id(object, methods::AUTOCOMPLETE_REGISTER)?;
                let registration: ExtensionAutocompleteRegistration =
                    serde_json::from_value(params)
                        .map_err(|error| format!("invalid autocomplete registration: {error}"))?;
                let _registered = insert_child_request(state, id.clone(), None, None)?;
                if state
                    .events
                    .send(ExtensionEvent::AutocompleteRegistered {
                        request_id: id.clone(),
                        generation: state.generation,
                        registration,
                    })
                    .is_err()
                {
                    try_queue_child_response(
                        &state.child_requests,
                        &id,
                        &state.writer,
                        state.max_message_bytes(),
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {"accepted": false},
                        }),
                    )?;
                }
            }
            methods::PRESENTATION_UPDATE => {
                require_declared(state.declared.presentation, "semantic presentation")?;
                if !is_stateful_api(&read_std_lock(&state.protocol).version) {
                    return Err("semantic presentation requires extension API 0.2".into());
                }
                let request: PresentationUpdateRequest = serde_json::from_value(params)
                    .map_err(|error| format!("invalid presentation update: {error}"))?;
                request
                    .snapshot
                    .validate(&state.declared.commands)
                    .map_err(|error| format!("invalid presentation snapshot: {error}"))?;
                let resource_owner = presentation_update_owner(state, &request)?;
                if let Some(updates) = &state.presentation_updates {
                    let sequence = state.presentation_sequence.fetch_add(1, Ordering::Relaxed) + 1;
                    updates.send_replace(Some((sequence, resource_owner, request.snapshot)));
                    return Ok(());
                }
                let (admitted, first_rejection) = lock_std_mutex(&state.presentation_rate).admit();
                if !admitted {
                    if first_rejection {
                        let _ = state.events.send(ExtensionEvent::Diagnostic {
                            message: format!(
                                "semantic presentation update rate exceeded {MAX_PRESENTATION_UPDATES_PER_SECOND} snapshots per second; excess updates were dropped"
                            ),
                        });
                    }
                    return Ok(());
                }
                let _ = state.events.send(ExtensionEvent::PresentationUpdated {
                    generation: state.generation,
                    resource_owner,
                    snapshot: request.snapshot,
                });
            }
            methods::PROGRESS => {
                require_feature(state, EXTENSION_FEATURE_REQUEST_PROGRESS)?;
                let progress: ExtensionProgressNotification = serde_json::from_value(params)
                    .map_err(|error| format!("invalid progress notification: {error}"))?;
                dispatch_progress(state, progress)?;
            }
            methods::CANCEL_REQUEST => {
                if read_std_lock(&state.protocol).version == EXTENSION_API_VERSION_0_3 {
                    api_v03::parse_cancel_request_params(params.clone())
                        .map_err(|error| format!("invalid API 0.3 cancellation: {error}"))?;
                }
                let id = params
                    .get("id")
                    .cloned()
                    .ok_or_else(|| "cancel request requires id".to_owned())?;
                let request_id: ExtensionRequestId = serde_json::from_value(id)
                    .map_err(|error| format!("invalid cancel request id: {error}"))?;
                settle_child_request(&state.child_requests, &request_id);
            }
            // API 0.3 provider catalog reverse requests. The always-running
            // protocol reader dispatches these inline, so a registration issued
            // after the initial load phase (for example from a command or tool
            // handler) mutates the host registry the moment its frame arrives;
            // it is never queued until a reload. Product catalog projection is a
            // separate, host-owned synchronization boundary that runs before the
            // next request, so an in-flight request is never mutated.
            methods::PROVIDERS_COMPLETE => {
                require_declared(state.declared.providers, "provider catalogs")?;
                api_v03::parse_provider_catalog_complete_params(params)
                    .map_err(|error| format!("invalid provider catalog completion: {error}"))?;
                let registry = provider_registry_for_request(state)
                    .map_err(|_| "provider registry is unavailable".to_owned())?;
                registry.complete_initial_catalog(&state.provider_owner);
            }
            methods::PROVIDERS_REGISTER => {
                require_declared(state.declared.providers, "provider catalogs")?;
                let id = parse_child_request_id(object, methods::PROVIDERS_REGISTER)?;
                insert_child_request(state, id.clone(), None, None)?;
                let result = api_v03::parse_provider_register_params(params)
                    .map_err(|_| ProviderHostResponseError::Invalid)
                    .and_then(|request| {
                        provider_registry_for_request(state)?
                            .register(state.provider_owner.clone(), request)
                            .map_err(provider_registry_response_error)
                    })
                    .and_then(provider_catalog_response_value);
                queue_provider_host_response(state, &id, result)?;
            }
            methods::PROVIDERS_UPDATE => {
                require_declared(state.declared.providers, "provider catalogs")?;
                let id = parse_child_request_id(object, methods::PROVIDERS_UPDATE)?;
                insert_child_request(state, id.clone(), None, None)?;
                let result = api_v03::parse_provider_update_params(params)
                    .map_err(|_| ProviderHostResponseError::Invalid)
                    .and_then(|request| {
                        provider_registry_for_request(state)?
                            .update(state.provider_owner.clone(), request)
                            .map_err(provider_registry_response_error)
                    })
                    .and_then(provider_catalog_response_value);
                queue_provider_host_response(state, &id, result)?;
            }
            methods::PROVIDERS_UNREGISTER => {
                require_declared(state.declared.providers, "provider catalogs")?;
                let id = parse_child_request_id(object, methods::PROVIDERS_UNREGISTER)?;
                insert_child_request(state, id.clone(), None, None)?;
                let result = api_v03::parse_provider_unregister_params(params)
                    .map_err(|_| ProviderHostResponseError::Invalid)
                    .and_then(|request| {
                        provider_registry_for_request(state)?
                            .unregister(&state.provider_owner, &request.provider_id)
                            .map_err(provider_registry_response_error)
                    })
                    .and_then(provider_catalog_response_value);
                queue_provider_host_response(state, &id, result)?;
            }
            methods::PROVIDER_AUTH_REQUEST | methods::PROVIDER_AUTH_REVOKE => {
                require_declared(state.declared.providers, "provider authorization")?;
                let id = parse_child_request_id(object, method)?;
                insert_child_request(state, id.clone(), None, None)?;
                let expected_action = if method == methods::PROVIDER_AUTH_REVOKE {
                    "revoke"
                } else {
                    "authorize"
                };
                let result = api_v03::parse_provider_authorization_request(params)
                    .map_err(|_| ProviderHostResponseError::Invalid)
                    .and_then(|request| {
                        if (method == methods::PROVIDER_AUTH_REVOKE
                            && request.action != expected_action)
                            || (method == methods::PROVIDER_AUTH_REQUEST
                                && !matches!(request.action.as_str(), "authorize" | "refresh"))
                        {
                            return Err(ProviderHostResponseError::Invalid);
                        }
                        provider_registry_for_request(state)?
                            .request_authorization(&state.provider_owner, request)
                            .map_err(provider_registry_response_error)
                    })
                    .and_then(provider_authorization_response_value);
                queue_provider_host_response(state, &id, result)?;
            }
            methods::PROVIDER_EVENT => {
                require_declared(state.declared.providers, "provider streams")?;
                let event = api_v03::parse_provider_stream_event(params)
                    .map_err(|_| "invalid API 0.3 provider stream event".to_owned())?;
                dispatch_provider_stream_event(state, event)?;
            }
            methods::SESSION_CREATE => {
                let id = parse_child_request_id(object, methods::SESSION_CREATE)?;
                api_v03::parse_session_create_params(params)
                    .map_err(|error| format!("invalid API 0.3 session/create request: {error}"))?;
                queue_api_v03_session_lifecycle_operation(
                    state,
                    id,
                    ExtensionSessionLifecycleOperation::Create,
                )?;
            }
            methods::SESSION_FORK => {
                let id = parse_child_request_id(object, methods::SESSION_FORK)?;
                api_v03::parse_session_fork_params(params)
                    .map_err(|error| format!("invalid API 0.3 session/fork request: {error}"))?;
                queue_api_v03_session_lifecycle_operation(
                    state,
                    id,
                    ExtensionSessionLifecycleOperation::Fork,
                )?;
            }
            methods::SESSION_RELOAD => {
                let id = parse_child_request_id(object, methods::SESSION_RELOAD)?;
                api_v03::parse_session_reload_params(params)
                    .map_err(|error| format!("invalid API 0.3 session/reload request: {error}"))?;
                queue_api_v03_session_lifecycle_operation(
                    state,
                    id,
                    ExtensionSessionLifecycleOperation::Reload,
                )?;
            }
            methods::SESSION_SWITCH => {
                let id = parse_child_request_id(object, methods::SESSION_SWITCH)?;
                let params = api_v03::parse_session_switch_params(params)
                    .map_err(|error| format!("invalid API 0.3 session/switch request: {error}"))?;
                queue_api_v03_session_lifecycle_operation(
                    state,
                    id,
                    ExtensionSessionLifecycleOperation::Switch {
                        session_id: params.session_id,
                    },
                )?;
            }
            methods::POLICY_EVALUATE => {
                require_feature(state, EXTENSION_FEATURE_POLICY_INTENTS)?;
                let id = parse_child_request_id(object, methods::POLICY_EVALUATE)?;
                let request: ExtensionPolicyEvaluationRequest = serde_json::from_value(params)
                    .map_err(|error| format!("invalid policy evaluation request: {error}"))?;
                request
                    .intent
                    .canonical_hash()
                    .map_err(|error| format!("invalid policy action intent: {error}"))?;
                let Some(registered) = register_child_request(
                    state,
                    id.clone(),
                    Some(request.parent_request_id),
                    methods::POLICY_EVALUATE,
                )?
                else {
                    return Ok(());
                };
                let parent = registered
                    .parent_request_id
                    .expect("API 0.2 child registration returns a parent ID");
                if let Some(token) = request.approval_token {
                    if !read_std_lock(&state.protocol).supports(EXTENSION_FEATURE_APPROVALS) {
                        try_queue_child_response(
                            &state.child_requests,
                            &id,
                            &state.writer,
                            state.max_message_bytes(),
                            serde_json::json!({
                                "jsonrpc":"2.0",
                                "id":id,
                                "error":{
                                    "code":-32602,
                                    "message":"approval token requires negotiated approvals",
                                },
                            }),
                        )?;
                        return Ok(());
                    }
                    let approved = state
                        .approval_store
                        .consume(
                            &token,
                            &request.intent,
                            state.generation,
                            &ExtensionRequestId::Number(parent),
                        )
                        .map_err(|error| format!("invalid policy action intent: {error}"))?;
                    try_queue_child_response(
                        &state.child_requests,
                        &id,
                        &state.writer,
                        state.max_message_bytes(),
                        serde_json::json!({
                            "jsonrpc":"2.0",
                            "id":id,
                            "result":ExtensionPolicyEvaluationResponse {
                                decision: if approved {
                                    ExtensionPolicyDecision::Allow
                                } else {
                                    ExtensionPolicyDecision::Deny
                                },
                                approval_token: None,
                            },
                        }),
                    )?;
                    return Ok(());
                }
                let intent = request.intent;
                if let Some(child) = lock_std_mutex(&state.child_requests).get_mut(&id) {
                    child.policy_intent = Some(intent.clone());
                }
                state
                    .events
                    .send(ExtensionEvent::PolicyEvaluationRequested {
                        request_id: id,
                        generation: state.generation,
                        parent_request_id: parent,
                        intent,
                    })
                    .map_err(|_| {
                        "policy evaluation arrived without an active event subscriber".to_owned()
                    })?;
            }
            methods::INPUT_REQUEST => {
                if !is_stateful_api(&read_std_lock(&state.protocol).version) {
                    return Err("input/request requires API 0.2".into());
                }
                let id = parse_child_request_id(object, methods::INPUT_REQUEST)?;
                let request: ExtensionInputRequest = serde_json::from_value(params)
                    .map_err(|error| format!("invalid input request: {error}"))?;
                if request.prompt.trim().is_empty() {
                    return Err("input prompt must not be empty".into());
                }
                if request.prompt.len() > MAX_EXTENSION_INPUT_PROMPT_BYTES {
                    return Err(format!(
                        "input prompt exceeded {MAX_EXTENSION_INPUT_PROMPT_BYTES} UTF-8 bytes"
                    ));
                }
                let Some(registered) = register_child_request(
                    state,
                    id.clone(),
                    Some(request.parent_request_id),
                    methods::INPUT_REQUEST,
                )?
                else {
                    return Ok(());
                };
                let progress = registered.progress;
                let response_state = registered.response_state;
                let worker = if progress.is_some() {
                    Some(
                        state
                            .child_work_slots
                            .clone()
                            .try_acquire_owned()
                            .map_err(|_| {
                                settle_child_request(&state.child_requests, &id);
                                format!("extension child worker limit {MAX_CHILD_WORKERS} exceeded")
                            })?,
                    )
                } else {
                    None
                };
                if let (Some(progress), Some(worker)) = (progress, worker) {
                    let writer = state.writer.clone();
                    let child_requests = Arc::clone(&state.child_requests);
                    let health = Arc::clone(&state.health);
                    let events = state.events.clone();
                    let max_message_bytes = state.max_message_bytes();
                    let response_id = id;
                    tokio::spawn(async move {
                        let input = progress.input(request.prompt, request.secret);
                        tokio::pin!(input);
                        let value = tokio::select! {
                            answer = &mut input => answer.and_then(|answer| {
                                let bytes = answer.as_bytes();
                                (bytes.len() <= MAX_EXTENSION_INPUT_VALUE_BYTES)
                                    .then(|| std::str::from_utf8(bytes).ok().map(str::to_owned))
                                    .flatten()
                            }),
                            _ = child_response_settled(Arc::clone(&response_state)) => return,
                        };
                        let response = try_queue_child_response(
                            &child_requests,
                            &response_id,
                            &writer,
                            max_message_bytes,
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": response_id,
                                "result": {"value": value},
                            }),
                        );
                        if let Err(error) = response {
                            update_health(
                                &health,
                                ExtensionHealthState::Degraded,
                                Some(error.clone()),
                            );
                            let _ = events.send(ExtensionEvent::Diagnostic { message: error });
                            settle_child_request(&child_requests, &response_id);
                        }
                        drop(worker);
                    });
                } else if state
                    .events
                    .send(ExtensionEvent::InputRequested {
                        request_id: id.clone(),
                        generation: state.generation,
                        parent_request_id: request.parent_request_id,
                        request: request.clone(),
                    })
                    .is_err()
                {
                    try_queue_child_response(
                        &state.child_requests,
                        &id,
                        &state.writer,
                        state.max_message_bytes(),
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {"value": serde_json::Value::Null},
                        }),
                    )?;
                }
            }
            methods::TOOLS_REGISTER => {
                require_feature(state, EXTENSION_FEATURE_DYNAMIC_TOOLS)?;
                let id = parse_child_request_id(object, methods::TOOLS_REGISTER)?;
                let request: ToolRegistrationRequest = match serde_json::from_value(params) {
                    Ok(request) => request,
                    Err(error) => {
                        return reject_unparented_child_request(
                            state,
                            id,
                            format!("invalid tool registration: {error}"),
                        )
                    }
                };
                if let Err(error) =
                    validate_tool_definitions(&request.tools, EXTENSION_API_VERSION_0_2)
                {
                    return reject_unparented_child_request(state, id, error.to_string());
                }
                queue_catalog_update(state, id, CatalogMutation::Register(request.tools))?;
            }
            methods::TOOLS_UNREGISTER => {
                require_feature(state, EXTENSION_FEATURE_DYNAMIC_TOOLS)?;
                let id = parse_child_request_id(object, methods::TOOLS_UNREGISTER)?;
                let request: ToolUnregistrationRequest = match serde_json::from_value(params) {
                    Ok(request) => request,
                    Err(error) => {
                        return reject_unparented_child_request(
                            state,
                            id,
                            format!("invalid tool unregistration: {error}"),
                        )
                    }
                };
                if request.names.len() > MAX_DYNAMIC_EXTENSION_TOOLS {
                    return reject_unparented_child_request(
                        state,
                        id,
                        format!(
                            "tool unregistration contains {} names; limit is {MAX_DYNAMIC_EXTENSION_TOOLS}",
                            request.names.len()
                        ),
                    );
                }
                if let Err(error) = validate_identifiers("tool", &request.names, true) {
                    return reject_unparented_child_request(state, id, error.to_string());
                }
                queue_catalog_update(state, id, CatalogMutation::Unregister(request.names))?;
            }
            methods::COMPOSER_GET => {
                let Some((_request, admitted)) = admit_host_request::<ComposerGetRequest>(
                    state,
                    object,
                    methods::COMPOSER_GET,
                    EXTENSION_FEATURE_COMPOSER,
                    params,
                )?
                else {
                    return Ok(());
                };
                dispatch_host_request_event(state, &admitted, |admitted| {
                    ExtensionEvent::ComposerRequested {
                        request_id: admitted.request_id.clone(),
                        generation: admitted.generation,
                        owner: Some(admitted.owner.clone()),
                        operation: ExtensionComposerOperation::Get,
                    }
                })?;
            }
            methods::COMPOSER_SET | methods::COMPOSER_INSERT => {
                let Some((request, admitted)) = admit_host_request::<ComposerTextRequest>(
                    state,
                    object,
                    method,
                    EXTENSION_FEATURE_COMPOSER,
                    params,
                )?
                else {
                    return Ok(());
                };
                if let Err(failure) = bounded_plain_text_failure(
                    "composer text",
                    &request.text,
                    MAX_EXTENSION_COMPOSER_TEXT_BYTES,
                ) {
                    return refuse_admitted_request(state, &admitted, failure);
                }
                let operation = if method == methods::COMPOSER_SET {
                    ExtensionComposerOperation::Set { text: request.text }
                } else {
                    ExtensionComposerOperation::Insert { text: request.text }
                };
                if let Err(detail) = operation.validate() {
                    return refuse_admitted_request(
                        state,
                        &admitted,
                        (ExtensionRequestFailure::InvalidRequest, detail),
                    );
                }
                dispatch_host_request_event(state, &admitted, move |admitted| {
                    ExtensionEvent::ComposerRequested {
                        request_id: admitted.request_id.clone(),
                        generation: admitted.generation,
                        owner: Some(admitted.owner.clone()),
                        operation,
                    }
                })?;
            }
            methods::SHORTCUT_REGISTER => {
                let Some((request, admitted)) = admit_host_request::<ShortcutRegisterRequest>(
                    state,
                    object,
                    methods::SHORTCUT_REGISTER,
                    EXTENSION_FEATURE_SHORTCUTS,
                    params,
                )?
                else {
                    return Ok(());
                };
                if request.key.trim().is_empty() || request.key.trim() != request.key {
                    return refuse_admitted_request(
                        state,
                        &admitted,
                        (
                            ExtensionRequestFailure::InvalidRequest,
                            "shortcut key must be a non-empty trimmed terminal key spelling"
                                .to_owned(),
                        ),
                    );
                }
                if request.description.trim().is_empty() {
                    return refuse_admitted_request(
                        state,
                        &admitted,
                        (
                            ExtensionRequestFailure::InvalidRequest,
                            "shortcut description must not be empty".to_owned(),
                        ),
                    );
                }
                if let Err(failure) = bounded_plain_text_failure(
                    "shortcut id",
                    &request.id,
                    MAX_EXTENSION_SHORTCUT_ID_BYTES,
                ) {
                    return refuse_admitted_request(state, &admitted, failure);
                }
                if let Err(error) = validate_identifier("shortcut", &request.id, true) {
                    return refuse_admitted_request(
                        state,
                        &admitted,
                        (ExtensionRequestFailure::InvalidRequest, error.to_string()),
                    );
                }
                if let Err(failure) = bounded_plain_text_failure(
                    "shortcut key",
                    &request.key,
                    MAX_EXTENSION_SHORTCUT_KEY_BYTES,
                ) {
                    return refuse_admitted_request(state, &admitted, failure);
                }
                if let Err(failure) = bounded_plain_text_failure(
                    "shortcut description",
                    &request.description,
                    MAX_EXTENSION_SHORTCUT_DESCRIPTION_BYTES,
                ) {
                    return refuse_admitted_request(state, &admitted, failure);
                }
                dispatch_host_request_event(state, &admitted, move |admitted| {
                    ExtensionEvent::ShortcutRequested {
                        request_id: admitted.request_id.clone(),
                        generation: admitted.generation,
                        owner: Some(admitted.owner.clone()),
                        shortcut_id: request.id,
                        key: request.key,
                        description: request.description,
                    }
                })?;
            }
            methods::SESSION_APPEND_ENTRY => {
                let Some((request, admitted)) = admit_host_request::<SessionAppendEntryRequest>(
                    state,
                    object,
                    methods::SESSION_APPEND_ENTRY,
                    EXTENSION_FEATURE_SESSION_ENTRIES,
                    params,
                )?
                else {
                    return Ok(());
                };
                if request.entry_type.trim().is_empty() {
                    return refuse_admitted_request(
                        state,
                        &admitted,
                        (
                            ExtensionRequestFailure::InvalidRequest,
                            "session entry type must not be empty".to_owned(),
                        ),
                    );
                }
                if let Err(failure) = bounded_plain_text_failure(
                    "session entry type",
                    &request.entry_type,
                    MAX_EXTENSION_SESSION_ENTRY_TYPE_BYTES,
                ) {
                    return refuse_admitted_request(state, &admitted, failure);
                }
                match serde_json::to_vec(&request.data) {
                    Ok(bytes) if bytes.len() > MAX_EXTENSION_SESSION_ENTRY_DATA_BYTES => {
                        return refuse_admitted_request(
                            state,
                            &admitted,
                            (
                                ExtensionRequestFailure::BoundsExceeded,
                                format!(
                                    "session entry data is {} JSON bytes; limit is {MAX_EXTENSION_SESSION_ENTRY_DATA_BYTES}",
                                    bytes.len()
                                ),
                            ),
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        return refuse_admitted_request(
                            state,
                            &admitted,
                            (
                                ExtensionRequestFailure::InvalidRequest,
                                format!("session entry data is not serializable: {error}"),
                            ),
                        );
                    }
                }
                let operation = ExtensionSessionEntryOperation::Append {
                    entry_type: request.entry_type,
                    data: request.data,
                };
                if let Err(detail) = operation.validate() {
                    return refuse_admitted_request(
                        state,
                        &admitted,
                        (ExtensionRequestFailure::InvalidRequest, detail),
                    );
                }
                dispatch_host_request_event(state, &admitted, move |admitted| {
                    ExtensionEvent::SessionEntryRequested {
                        request_id: admitted.request_id.clone(),
                        generation: admitted.generation,
                        owner: Some(admitted.owner.clone()),
                        operation,
                    }
                })?;
            }
            methods::SESSION_SET_NAME => {
                let Some((request, admitted)) = admit_host_request::<SessionSetNameRequest>(
                    state,
                    object,
                    methods::SESSION_SET_NAME,
                    EXTENSION_FEATURE_SESSION_ENTRIES,
                    params,
                )?
                else {
                    return Ok(());
                };
                if let Err(failure) = bounded_plain_text_failure(
                    "session name",
                    &request.name,
                    MAX_EXTENSION_SESSION_NAME_BYTES,
                ) {
                    return refuse_admitted_request(state, &admitted, failure);
                }
                let operation = ExtensionSessionEntryOperation::SetName { name: request.name };
                dispatch_host_request_event(state, &admitted, move |admitted| {
                    ExtensionEvent::SessionEntryRequested {
                        request_id: admitted.request_id.clone(),
                        generation: admitted.generation,
                        owner: Some(admitted.owner.clone()),
                        operation,
                    }
                })?;
            }
            methods::SESSION_SET_LABEL => {
                let Some((request, admitted)) = admit_host_request::<SessionSetLabelRequest>(
                    state,
                    object,
                    methods::SESSION_SET_LABEL,
                    EXTENSION_FEATURE_SESSION_ENTRIES,
                    params,
                )?
                else {
                    return Ok(());
                };
                if request.entry_id.trim().is_empty() {
                    return refuse_admitted_request(
                        state,
                        &admitted,
                        (
                            ExtensionRequestFailure::InvalidRequest,
                            "session entry id must not be empty".to_owned(),
                        ),
                    );
                }
                if let Err(failure) = bounded_plain_text_failure(
                    "session entry id",
                    &request.entry_id,
                    MAX_CONFIRMATION_REQUEST_ID_BYTES,
                ) {
                    return refuse_admitted_request(state, &admitted, failure);
                }
                if let Err(failure) = bounded_plain_text_failure(
                    "session entry label",
                    &request.label,
                    MAX_EXTENSION_SESSION_LABEL_BYTES,
                ) {
                    return refuse_admitted_request(state, &admitted, failure);
                }
                let operation = ExtensionSessionEntryOperation::SetLabel {
                    entry_id: request.entry_id,
                    label: request.label,
                };
                if let Err(detail) = operation.validate() {
                    return refuse_admitted_request(
                        state,
                        &admitted,
                        (ExtensionRequestFailure::InvalidRequest, detail),
                    );
                }
                dispatch_host_request_event(state, &admitted, move |admitted| {
                    ExtensionEvent::SessionEntryRequested {
                        request_id: admitted.request_id.clone(),
                        generation: admitted.generation,
                        owner: Some(admitted.owner.clone()),
                        operation,
                    }
                })?;
            }
            methods::SESSION_SEND_MESSAGE => {
                let Some((request, admitted)) = admit_host_request::<SessionSendMessageRequest>(
                    state,
                    object,
                    methods::SESSION_SEND_MESSAGE,
                    EXTENSION_FEATURE_MESSAGE_INJECTION,
                    params,
                )?
                else {
                    return Ok(());
                };
                let injection = match request.role.as_str() {
                    "assistant" => ExtensionMessageInjection::Assistant { text: request.text },
                    "system" => ExtensionMessageInjection::System { text: request.text },
                    role => {
                        return refuse_admitted_request(
                            state,
                            &admitted,
                            (
                                ExtensionRequestFailure::InvalidRequest,
                                format!(
                                    "session/send_message role `{role}` must be exactly `assistant` or `system`; user text uses session/send_user_message"
                                ),
                            ),
                        );
                    }
                };
                if let Err(failure) = bounded_plain_text_failure(
                    "injected message",
                    match &injection {
                        ExtensionMessageInjection::Assistant { text }
                        | ExtensionMessageInjection::System { text }
                        | ExtensionMessageInjection::User { text } => text,
                    },
                    MAX_EXTENSION_INJECTED_MESSAGE_BYTES,
                ) {
                    return refuse_admitted_request(state, &admitted, failure);
                }
                dispatch_host_request_event(state, &admitted, move |admitted| {
                    ExtensionEvent::MessageInjectionRequested {
                        request_id: admitted.request_id.clone(),
                        generation: admitted.generation,
                        owner: Some(admitted.owner.clone()),
                        injection,
                    }
                })?;
            }
            methods::SESSION_SEND_USER_MESSAGE => {
                let Some((request, admitted)) = admit_host_request::<SessionSendUserMessageRequest>(
                    state,
                    object,
                    methods::SESSION_SEND_USER_MESSAGE,
                    EXTENSION_FEATURE_MESSAGE_INJECTION,
                    params,
                )?
                else {
                    return Ok(());
                };
                if let Err(failure) = bounded_plain_text_failure(
                    "injected message",
                    &request.text,
                    MAX_EXTENSION_INJECTED_MESSAGE_BYTES,
                ) {
                    return refuse_admitted_request(state, &admitted, failure);
                }
                let injection = ExtensionMessageInjection::User { text: request.text };
                if let Err(detail) = injection.validate() {
                    return refuse_admitted_request(
                        state,
                        &admitted,
                        (ExtensionRequestFailure::InvalidRequest, detail),
                    );
                }
                dispatch_host_request_event(state, &admitted, move |admitted| {
                    ExtensionEvent::MessageInjectionRequested {
                        request_id: admitted.request_id.clone(),
                        generation: admitted.generation,
                        owner: Some(admitted.owner.clone()),
                        injection,
                    }
                })?;
            }
            methods::TOOLS_SET_ACTIVE => {
                let Some((request, admitted)) = admit_host_request::<ToolsSetActiveRequest>(
                    state,
                    object,
                    methods::TOOLS_SET_ACTIVE,
                    EXTENSION_FEATURE_ACTIVE_TOOLS,
                    params,
                )?
                else {
                    return Ok(());
                };
                if request.names.len() > MAX_DYNAMIC_EXTENSION_TOOLS {
                    return refuse_admitted_request(
                        state,
                        &admitted,
                        (
                            ExtensionRequestFailure::BoundsExceeded,
                            format!(
                                "active tool set contains {} names; limit is {MAX_DYNAMIC_EXTENSION_TOOLS}",
                                request.names.len()
                            ),
                        ),
                    );
                }
                if let Err(error) = validate_identifiers("tool", &request.names, true) {
                    return refuse_admitted_request(
                        state,
                        &admitted,
                        (ExtensionRequestFailure::InvalidRequest, error.to_string()),
                    );
                }
                dispatch_host_request_event(state, &admitted, move |admitted| {
                    ExtensionEvent::ActiveToolsRequested {
                        request_id: admitted.request_id.clone(),
                        generation: admitted.generation,
                        owner: Some(admitted.owner.clone()),
                        names: request.names,
                    }
                })?;
            }
            methods::TERMINAL_ACQUIRE => {
                let Some((_request, admitted)) = admit_host_request::<TerminalAcquireRequest>(
                    state,
                    object,
                    methods::TERMINAL_ACQUIRE,
                    EXTENSION_FEATURE_TERMINAL_HANDOFF,
                    params,
                )?
                else {
                    return Ok(());
                };
                // The child request stays registered: the frontend that owns
                // the foreground tty answers later with `{grant_id, columns,
                // rows}` through the ordinary response path.
                dispatch_host_request_event(state, &admitted, |admitted| {
                    ExtensionEvent::TerminalRequested {
                        request_id: admitted.request_id.clone(),
                        generation: admitted.generation,
                        owner: Some(admitted.owner.clone()),
                        operation: ExtensionTerminalOperation::Acquire,
                    }
                })?;
            }
            methods::TERMINAL_RELEASE => {
                let Some((_request, admitted)) = admit_host_request::<TerminalReleaseRequest>(
                    state,
                    object,
                    methods::TERMINAL_RELEASE,
                    EXTENSION_FEATURE_TERMINAL_HANDOFF,
                    params,
                )?
                else {
                    return Ok(());
                };
                // Release is answered on the same child-request path once the
                // frontend re-entered its own terminal.
                dispatch_host_request_event(state, &admitted, |admitted| {
                    ExtensionEvent::TerminalRequested {
                        request_id: admitted.request_id.clone(),
                        generation: admitted.generation,
                        owner: Some(admitted.owner.clone()),
                        operation: ExtensionTerminalOperation::Release,
                    }
                })?;
            }
            methods::CONTEXT_SESSION_MANAGER | methods::CONTEXT_PENDING_MESSAGES => {
                let Some((_request, admitted)) = admit_host_request::<ContextSnapshotRequest>(
                    state,
                    object,
                    method,
                    EXTENSION_FEATURE_SESSION_CONTEXT,
                    params,
                )?
                else {
                    return Ok(());
                };
                // The child request stays registered: the foreground session
                // answers later through `respond_to_extension_request`.
                let operation = if method == methods::CONTEXT_SESSION_MANAGER {
                    ExtensionContextOperation::SessionManager
                } else {
                    ExtensionContextOperation::PendingMessages
                };
                dispatch_host_request_event(state, &admitted, |admitted| {
                    ExtensionEvent::ContextSnapshotRequested {
                        request_id: admitted.request_id.clone(),
                        generation: admitted.generation,
                        owner: Some(admitted.owner.clone()),
                        operation,
                    }
                })?;
            }
            methods::CONTEXT_SYSTEM_PROMPT => {
                // `system_prompt_read` is its own negotiated feature because the
                // reply discloses host-owned prompt text, AND it is capability-
                // gated: the feature is offered and negotiable only for an
                // extension whose manifest declares `capabilities.system_prompt`
                // (mirroring how `capabilities.secrets` allow-lists secret
                // names). An undeclared extension never sees the feature, and
                // echoing it in the initialize response fails negotiation as an
                // unknown feature. An unnegotiated request is refused with
                // `unsupported_feature` before the owner check.
                let Some((_request, admitted)) = admit_host_request::<ContextSnapshotRequest>(
                    state,
                    object,
                    methods::CONTEXT_SYSTEM_PROMPT,
                    EXTENSION_FEATURE_SYSTEM_PROMPT_READ,
                    params,
                )?
                else {
                    return Ok(());
                };
                // The child request stays registered until the frontend answers
                // with bounded prompt text.
                dispatch_host_request_event(state, &admitted, |admitted| {
                    ExtensionEvent::ContextSnapshotRequested {
                        request_id: admitted.request_id.clone(),
                        generation: admitted.generation,
                        owner: Some(admitted.owner.clone()),
                        operation: ExtensionContextOperation::SystemPrompt,
                    }
                })?;
            }
            methods::CONTEXT_MODEL | methods::CONTEXT_MODEL_CATALOG => {
                let Some((_request, admitted)) = admit_host_request::<ContextSnapshotRequest>(
                    state,
                    object,
                    method,
                    EXTENSION_FEATURE_MODEL_CATALOG,
                    params,
                )?
                else {
                    return Ok(());
                };
                // The child request stays registered: the foreground session
                // answers later through `respond_to_extension_request` with a
                // bounded, secret-free model view or catalog.
                let operation = if method == methods::CONTEXT_MODEL {
                    ExtensionModelOperation::Current
                } else {
                    ExtensionModelOperation::Catalog
                };
                dispatch_host_request_event(state, &admitted, |admitted| {
                    ExtensionEvent::ModelViewRequested {
                        request_id: admitted.request_id.clone(),
                        generation: admitted.generation,
                        owner: Some(admitted.owner.clone()),
                        operation,
                    }
                })?;
            }
            methods::AGENT_SPAWN => {
                require_feature(state, EXTENSION_FEATURE_AGENT_SESSIONS)?;
                let id = parse_child_request_id(object, methods::AGENT_SPAWN)?;
                let request: AgentSessionSpawnRequest = match serde_json::from_value(params) {
                    Ok(request) => request,
                    Err(error) => {
                        return reject_unparented_child_request(
                            state,
                            id,
                            format!("invalid agent spawn request: {error}"),
                        )
                    }
                };
                if request.policy.model_selection.is_some() {
                    if let Err(error) =
                        require_feature(state, EXTENSION_FEATURE_AGENT_MODEL_SELECTION_V1)
                    {
                        return reject_unparented_child_request(state, id, error);
                    }
                }
                let policy: ExtensionAgentSessionPolicy = request.policy.into();
                if let Err(error) = policy.validate() {
                    return reject_unparented_child_request(
                        state,
                        id,
                        format!("invalid agent spawn policy: {error}"),
                    );
                }
                queue_agent_session_operation(
                    state,
                    id,
                    request.parent_request_id,
                    methods::AGENT_SPAWN,
                    AgentSessionOperation::Spawn {
                        task_name: request.task_name,
                        profile: request.profile,
                        fingerprint: request.fingerprint,
                        message: request.message,
                        idempotency_key: request.idempotency_key,
                        policy: Box::new(policy),
                    },
                )?;
            }
            methods::AGENT_MESSAGE => {
                require_feature(state, EXTENSION_FEATURE_AGENT_SESSIONS)?;
                let id = parse_child_request_id(object, methods::AGENT_MESSAGE)?;
                let request: AgentSessionMessageRequest = match serde_json::from_value(params) {
                    Ok(request) => request,
                    Err(error) => {
                        return reject_unparented_child_request(
                            state,
                            id,
                            format!("invalid agent message request: {error}"),
                        )
                    }
                };
                queue_agent_session_operation(
                    state,
                    id,
                    request.parent_request_id,
                    methods::AGENT_MESSAGE,
                    AgentSessionOperation::Message {
                        target: request.target,
                        message: request.message,
                    },
                )?;
            }
            methods::AGENT_FOLLOW_UP => {
                require_feature(state, EXTENSION_FEATURE_AGENT_SESSIONS)?;
                let id = parse_child_request_id(object, methods::AGENT_FOLLOW_UP)?;
                let request: AgentSessionMessageRequest = match serde_json::from_value(params) {
                    Ok(request) => request,
                    Err(error) => {
                        return reject_unparented_child_request(
                            state,
                            id,
                            format!("invalid agent follow-up request: {error}"),
                        )
                    }
                };
                queue_agent_session_operation(
                    state,
                    id,
                    request.parent_request_id,
                    methods::AGENT_FOLLOW_UP,
                    AgentSessionOperation::FollowUp {
                        target: request.target,
                        message: request.message,
                    },
                )?;
            }
            methods::AGENT_MODELS => {
                let id = parse_child_request_id(object, methods::AGENT_MODELS)?;
                if let Err(error) = require_feature(state, EXTENSION_FEATURE_AGENT_SESSIONS)
                    .and_then(|()| {
                        require_feature(state, EXTENSION_FEATURE_AGENT_MODEL_SELECTION_V1)
                    })
                {
                    return reject_unparented_child_request(state, id, error);
                }
                let request: AgentSessionModelsRequest = match serde_json::from_value(params) {
                    Ok(request) => request,
                    Err(error) => {
                        return reject_unparented_child_request(
                            state,
                            id,
                            format!("invalid model discovery request: {error}"),
                        )
                    }
                };
                queue_agent_session_operation(
                    state,
                    id,
                    request.parent_request_id,
                    methods::AGENT_MODELS,
                    AgentSessionOperation::Models {
                        query: request.query,
                        limit: request.limit.unwrap_or(50),
                    },
                )?;
            }
            methods::AGENT_LIST => {
                require_feature(state, EXTENSION_FEATURE_AGENT_SESSIONS)?;
                let id = parse_child_request_id(object, methods::AGENT_LIST)?;
                let request: AgentSessionListRequest = match serde_json::from_value(params) {
                    Ok(request) => request,
                    Err(error) => {
                        return reject_unparented_child_request(
                            state,
                            id,
                            format!("invalid agent list request: {error}"),
                        )
                    }
                };
                queue_agent_session_operation(
                    state,
                    id,
                    request.parent_request_id,
                    methods::AGENT_LIST,
                    AgentSessionOperation::List,
                )?;
            }
            methods::AGENT_WAIT => {
                require_feature(state, EXTENSION_FEATURE_AGENT_SESSIONS)?;
                let id = parse_child_request_id(object, methods::AGENT_WAIT)?;
                let request: AgentSessionWaitRequest = match serde_json::from_value(params) {
                    Ok(request) => request,
                    Err(error) => {
                        return reject_unparented_child_request(
                            state,
                            id,
                            format!("invalid agent wait request: {error}"),
                        )
                    }
                };
                let timeout = Duration::from_millis(
                    request
                        .timeout_ms
                        .unwrap_or(30_000)
                        .clamp(1, MAX_EXTENSION_AGENT_WAIT_MS),
                );
                queue_agent_session_operation(
                    state,
                    id,
                    request.parent_request_id,
                    methods::AGENT_WAIT,
                    AgentSessionOperation::Wait { timeout },
                )?;
            }
            methods::AGENT_INTERRUPT => {
                require_feature(state, EXTENSION_FEATURE_AGENT_SESSIONS)?;
                let id = parse_child_request_id(object, methods::AGENT_INTERRUPT)?;
                let request: AgentSessionTargetRequest = match serde_json::from_value(params) {
                    Ok(request) => request,
                    Err(error) => {
                        return reject_unparented_child_request(
                            state,
                            id,
                            format!("invalid agent interrupt request: {error}"),
                        )
                    }
                };
                queue_agent_session_operation(
                    state,
                    id,
                    request.parent_request_id,
                    methods::AGENT_INTERRUPT,
                    AgentSessionOperation::Interrupt {
                        target: request.target,
                    },
                )?;
            }
            methods::SECRET_GET => {
                require_feature(state, EXTENSION_FEATURE_SECRETS)?;
                let id = parse_child_request_id(object, methods::SECRET_GET)?;
                let request: ExtensionSecretGetRequest = match serde_json::from_value(params) {
                    Ok(request) => request,
                    Err(error) => {
                        return reject_unparented_child_request(
                            state,
                            id,
                            format!("invalid secret lookup request: {error}"),
                        )
                    }
                };
                queue_secret_lookup(state, id, request)?;
            }
            methods::ARTIFACT_PUBLISH => {
                require_feature(state, EXTENSION_FEATURE_ARTIFACTS)?;
                let id = parse_child_request_id(object, methods::ARTIFACT_PUBLISH)?;
                let request: ArtifactPublishRequest = serde_json::from_value(params)
                    .map_err(|error| format!("invalid artifact publication: {error}"))?;
                let parent_request_id = request.parent_request_id;
                match artifact_publication(request) {
                    Ok(publication) => {
                        let worker =
                            state
                                .child_work_slots
                                .clone()
                                .try_acquire_owned()
                                .map_err(|_| {
                                    format!(
                                        "extension child worker limit {MAX_CHILD_WORKERS} exceeded"
                                    )
                                })?;
                        let Some(registered) = register_child_request(
                            state,
                            id.clone(),
                            Some(parent_request_id),
                            methods::ARTIFACT_PUBLISH,
                        )?
                        else {
                            return Ok(());
                        };
                        let Some(resource_owner) = registered.resource_owner else {
                            let delivery = try_queue_child_response(
                                &state.child_requests,
                                &id,
                                &state.writer,
                                state.max_message_bytes(),
                                serde_json::json!({
                                    "jsonrpc":"2.0",
                                    "id":id,
                                    "error":{
                                        "code":-32002,
                                        "message":"artifact publication requires a host-owned session context",
                                    },
                                }),
                            );
                            drop(worker);
                            return delivery.map(|_| ());
                        };
                        let resource_owner = resource_owner.session_id;
                        let store = state.artifact_store.clone();
                        let writer = state.writer.clone();
                        let child_requests = Arc::clone(&state.child_requests);
                        let health = Arc::clone(&state.health);
                        let events = state.events.clone();
                        let response_id = id;
                        let max_message_bytes = state.max_message_bytes();
                        let generation = state.generation;
                        tokio::spawn(async move {
                            let publication = store
                                .publish_async_for_owner(generation, resource_owner, publication)
                                .await;
                            let published_id = publication
                                .as_ref()
                                .ok()
                                .map(|published| published.id.clone());
                            let value = match publication {
                                Ok(published) => serde_json::json!({
                                    "jsonrpc":"2.0",
                                    "id":response_id,
                                    "result":{"artifact_id":published.id.to_string()},
                                }),
                                Err(error) => serde_json::json!({
                                    "jsonrpc":"2.0",
                                    "id":response_id,
                                    "error":{
                                        "code":-32602,
                                        "message":format!("artifact publication rejected: {error}"),
                                    },
                                }),
                            };
                            let delivery = try_queue_child_response(
                                &child_requests,
                                &response_id,
                                &writer,
                                max_message_bytes,
                                value,
                            );
                            rollback_undelivered_artifact(
                                &store,
                                generation,
                                published_id.as_ref(),
                                &delivery,
                            );
                            if let Err(error) = delivery {
                                update_health(
                                    &health,
                                    ExtensionHealthState::Degraded,
                                    Some(error.clone()),
                                );
                                let _ = events.send(ExtensionEvent::Diagnostic { message: error });
                                settle_child_request(&child_requests, &response_id);
                            }
                            drop(worker);
                        });
                    }
                    Err(message) => {
                        let Some(_registered) = register_child_request(
                            state,
                            id.clone(),
                            Some(parent_request_id),
                            methods::ARTIFACT_PUBLISH,
                        )?
                        else {
                            return Ok(());
                        };
                        try_queue_child_response(
                            &state.child_requests,
                            &id,
                            &state.writer,
                            state.max_message_bytes(),
                            serde_json::json!({
                                "jsonrpc":"2.0",
                                "id":id,
                                "error":{"code":-32602,"message":message},
                            }),
                        )?;
                    }
                }
            }
            _ => {
                if let Some(id) = object.get("id").cloned() {
                    let id: ExtensionRequestId = serde_json::from_value(id)
                        .map_err(|error| format!("invalid unknown-method request id: {error}"))?;
                    queue_writer_value(
                        &state.writer,
                        &state.frame_limit,
                        serde_json::json!({
                            "jsonrpc":"2.0",
                            "id":id,
                            "error":{
                                "code":-32601,
                                "message":format!("method not found: {method}"),
                            },
                        }),
                    )?;
                } else {
                    let _ = state.events.send(ExtensionEvent::Diagnostic {
                        message: format!("ignored unknown extension notification `{method}`"),
                    });
                }
            }
        }
        return Ok(());
    }

    let id = object
        .get("id")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "response requires a numeric id".to_owned())?;
    let reply = if let Some(error) = object.get("error") {
        if read_std_lock(&state.protocol).version == EXTENSION_API_VERSION_0_3 {
            let error: api_v03::ErrorObject = serde_json::from_value(error.clone())
                .map_err(|decode| format!("invalid API 0.3 JSON-RPC error: {decode}"))?;
            api_v03::validate_error_object(&error)
                .map_err(|error| format!("invalid API 0.3 JSON-RPC error: {error}"))?;
        }
        let error: RpcErrorObject = serde_json::from_value(error.clone())
            .map_err(|decode| format!("invalid JSON-RPC error: {decode}"))?;
        Err(PendingError::Remote {
            code: error.code,
            message: error.message,
            data: error.data,
        })
    } else if let Some(result) = object.get("result") {
        Ok(result.clone())
    } else {
        Err(PendingError::Protocol(
            "response requires result or error".into(),
        ))
    };
    let request = {
        let mut pending = lock_std_mutex(&state.pending);
        let completed = pending.get(&id).is_some_and(|request| {
            request
                .terminal
                .compare_exchange(
                    REQUEST_ACTIVE,
                    REQUEST_COMPLETED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
        });
        if completed {
            pending.remove(&id)
        } else {
            None
        }
    };
    if let Some(request) = request {
        state.pending_changed.notify_waiters();
        cancel_children_from_reader(state, id, "parent settled");
        let _ = request.sender.send(reply);
    } else if lock_std_mutex(&state.tombstones).remove(id) {
        let _ = state.events.send(ExtensionEvent::Diagnostic {
            message: format!("ignored late response for cancelled request {id}"),
        });
    } else {
        let _ = state.events.send(ExtensionEvent::Diagnostic {
            message: format!("ignored response for unknown request {id}"),
        });
    }
    Ok(())
}

pub(super) fn parse_child_request_id(
    object: &serde_json::Map<String, serde_json::Value>,
    method: &str,
) -> Result<ExtensionRequestId, String> {
    let id = object
        .get("id")
        .cloned()
        .ok_or_else(|| format!("{method} requires an id"))?;
    let id = serde_json::from_value(id)
        .map_err(|error| format!("invalid {method} request id: {error}"))?;
    ExtensionRequestId::validate_confirmation_id(&id)
        .map_err(|error| format!("invalid {method} request id: {error}"))?;
    Ok(id)
}

pub(super) fn require_feature(state: &ProtocolReadState, feature: &str) -> Result<(), String> {
    if read_std_lock(&state.protocol).supports(feature) {
        Ok(())
    } else {
        Err(format!("extension did not negotiate `{feature}`"))
    }
}
