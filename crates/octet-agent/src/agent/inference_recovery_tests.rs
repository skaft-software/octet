use super::*;

fn request() -> Request {
    Request {
        system: Some("system".into()),
        messages: Vec::new(),
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        max_output_tokens: Some(16),
        temperature: None,
        stop: Vec::new(),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: CacheRetention::default(),
        session_id: None,
    }
}

pub(super) fn model() -> Model {
    let mut model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
        octet_ai::ResponsesRuntimeProfile::Codex;
    model
}

#[test]
fn qualification_uses_host_runtime_and_rejects_indeterminate_remote_options() {
    let mut model = model();
    let mut request = request();
    assert!(qualified_inference_replacement(&model, &request));
    Arc::make_mut(&mut model.spec).capabilities.responses_lite = true;
    assert!(qualified_inference_replacement(&model, &request));
    Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
        octet_ai::ResponsesRuntimeProfile::Default;
    assert!(!qualified_inference_replacement(&model, &request));
    Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
        octet_ai::ResponsesRuntimeProfile::Codex;
    Arc::make_mut(&mut model.spec).protocol = octet_ai::Protocol::OpenAiChat;
    assert!(!qualified_inference_replacement(&model, &request));
    Arc::make_mut(&mut model.spec).protocol = octet_ai::Protocol::OpenAiResponses;
    for kind in [
        "web_search_call",
        "computer_call",
        "mcp_call",
        "future_effect",
    ] {
        request.responses = Some(ResponsesOptions::full_replay(
            octet_ai::responses::ResponsesInput::new(vec![
                octet_ai::responses::ResponsesItem::new(serde_json::json!({"type": kind})).unwrap(),
            ]),
        ));
        assert!(!qualified_inference_replacement(&model, &request), "{kind}");
    }
    request.responses = Some(ResponsesOptions {
        previous_response_id: Some("remote".into()),
        ..Default::default()
    });
    assert!(!qualified_inference_replacement(&model, &request));
    request.responses = Some(ResponsesOptions {
        context_management: Some(serde_json::json!([])),
        ..Default::default()
    });
    assert!(!qualified_inference_replacement(&model, &request));
    request.responses = Some(ResponsesOptions {
        store: true,
        ..Default::default()
    });
    assert!(!qualified_inference_replacement(&model, &request));
}

#[test]
fn nonresumable_websocket_keeps_host_qualified_replacement_budget() {
    for qualified in [false, true] {
        let recovery = PendingProviderRecovery {
            error: AiError::StreamProtocol(octet_ai::StreamProtocolError::ResponseNotResumable {
                attempts: 0,
                visible_output: true,
                detail: "connection reset".into(),
            }),
            qualified,
            saw_generation: true,
            opened: true,
            exposure: None,
        };
        assert_eq!(
            recovery.replacement_limit(),
            if qualified {
                MAX_INFERENCE_REPLACEMENTS
            } else {
                0
            }
        );
        assert!(recovery.usage_unknown());
    }
}

#[test]
fn replacement_taxonomy_only_admits_explicit_transient_boundaries() {
    for phase in [
        octet_ai::TransportPhase::Body,
        octet_ai::TransportPhase::ResponseHeaders,
    ] {
        for timeout in [true, false] {
            let error = AiError::Transport(octet_ai::TransportError {
                phase,
                timeout,
                message: "interrupted".into(),
            });
            assert!(interrupted_inference_error(&error));
            let recovery = PendingProviderRecovery {
                error,
                qualified: true,
                saw_generation: true,
                opened: true,
                exposure: None,
            };
            assert_eq!(recovery.replacement_limit(), MAX_INFERENCE_REPLACEMENTS);
            assert!(recovery.usage_unknown());
        }
    }
    for error in [
        AiError::Decode(octet_ai::DecodeError::InvalidUtf8),
        AiError::Decode(octet_ai::DecodeError::Json("broken".into())),
        AiError::Decode(octet_ai::DecodeError::ResponseTooLarge),
        AiError::Decode(octet_ai::DecodeError::TooManyStreamEvents),
        AiError::StreamProtocol(octet_ai::StreamProtocolError::UnbalancedPart { index: 0 }),
        AiError::StreamProtocol(octet_ai::StreamProtocolError::UnexpectedEvent("bad".into())),
        AiError::Auth(octet_ai::AuthError::Resolve),
        AiError::Canceled,
        AiError::Provider(octet_ai::ProviderError {
            code: Some("invalid_request_error".into()),
            kind: None,
            message: "please try again".into(),
            request_id: None,
        }),
    ] {
        assert!(!interrupted_inference_error(&error), "{error:?}");
        let recovery = PendingProviderRecovery {
            error,
            qualified: true,
            saw_generation: true,
            opened: true,
            exposure: None,
        };
        assert_eq!(recovery.replacement_limit(), 0);
    }
    let failure = || {
        AiError::Transport(octet_ai::TransportError {
            phase: octet_ai::TransportPhase::Body,
            timeout: false,
            message: "reset".into(),
        })
    };
    for saw_generation in [false, true] {
        let recovery = PendingProviderRecovery {
            error: failure(),
            qualified: false,
            saw_generation,
            opened: true,
            exposure: None,
        };
        assert_eq!(recovery.replacement_limit(), 0);
    }
}

#[test]
fn only_provider_stream_json_gets_qualified_parse_recovery() {
    let wrap = |inner| AiError::StreamFailure {
        inner: Box::new(inner),
        progress: octet_ai::StreamProgress {
            provider_events: 1,
            decoded_events: 0,
            content_bytes: 0,
            buffered_bytes: 0,
            first_body_seen: true,
            elapsed_ms: 1,
            last_event_ms: Some(1),
        },
    };
    let malformed = wrap(AiError::Decode(octet_ai::DecodeError::Json(
        "malformed provider frame".into(),
    )));
    assert!(interrupted_inference_error(&malformed));
    assert!(interrupted_inference_error(&wrap(AiError::Decode(
        octet_ai::DecodeError::InvalidUtf8
    ))));
    assert!(!interrupted_inference_error(&AiError::Decode(
        octet_ai::DecodeError::InvalidUtf8
    )));
    assert_eq!(
        PendingProviderRecovery {
            error: malformed,
            qualified: false,
            saw_generation: true,
            opened: true,
            exposure: None,
        }
        .replacement_limit(),
        0
    );
    for error in [
        AiError::Decode(octet_ai::DecodeError::InvalidProviderField(
            "usage overflow".into(),
        )),
        AiError::Decode(octet_ai::DecodeError::TooManyStreamEvents),
        AiError::Decode(octet_ai::DecodeError::ResponseTooLarge),
        AiError::StreamProtocol(octet_ai::StreamProtocolError::UnbalancedPart { index: 0 }),
    ] {
        assert!(!interrupted_inference_error(&wrap(error)));
    }
    assert!(!interrupted_inference_error(&AiError::Decode(
        octet_ai::DecodeError::Json("local request serialization".into())
    )));
}

#[test]
fn durable_uncertainty_blocks_later_token_and_cost_ceilings() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("uncertain.jsonl");
    let model = model();
    let mut session = Session::create(&path).unwrap();
    session
        .record_usage_uncertainty(
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            "assistant_turn",
        )
        .unwrap();
    drop(session);
    let session = Session::open(&path).unwrap();
    assert!(matches!(
        reserve_request_tokens(&session, 1, 1, Some(u64::MAX)),
        Err(AgentError::UsageUncertain)
    ));
    assert!(matches!(
        reserve_request_cost(
            &session,
            &model,
            1,
            1,
            Some(u64::MAX),
            CacheRetention::Short
        ),
        Err(AgentError::UsageUncertain)
    ));
    assert!(reserve_request_tokens(&session, 1, 1, None).is_ok());
    assert!(reserve_request_cost(&session, &model, 1, 1, None, CacheRetention::Short).is_ok());
    for code in [
        "usage_not_included",
        "insufficient_quota",
        "billing_hard_limit_reached",
    ] {
        let error = AiError::Provider(octet_ai::ProviderError {
            code: Some(code.into()),
            kind: Some("rate_limit_exceeded".into()),
            message: "try again in 1s".into(),
            request_id: None,
        });
        assert!(!interrupted_inference_error(&error));
    }
}

#[test]
fn bounded_attempts_charge_ceiling_and_unpriced_cost_still_fails_closed() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bounded.jsonl");
    let mut model = model();
    Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
        octet_ai::ResponsesRuntimeProfile::Default;
    let bound = request_uncertainty_bound(&model, 100, 300, None, CacheRetention::Short).unwrap();
    let cost = bound.cost_microdollars.unwrap();
    let next_cost = worst_case_request_cost(&model, 20, 10, None).unwrap();
    let mut session = Session::create(&path).unwrap();
    session
        .record_usage_uncertainty_with_bound(
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            "assistant_turn",
            Some(bound),
        )
        .unwrap();
    drop(session);
    let session = Session::open(&path).unwrap();
    assert!(
        require_enforceable_output_cap(&session, Some(10), Some(u64::MAX), Some(u64::MAX)).is_ok()
    );
    assert!(reserve_request_tokens(&session, 20, 10, Some(bound.tokens + 30)).is_ok());
    assert!(
        matches!(reserve_request_tokens(&session, 20, 10, Some(bound.tokens + 29)),
            Err(AgentError::TokenLimit { current, reserved: 30, .. }) if current == bound.tokens)
    );
    assert!(reserve_request_cost(
        &session,
        &model,
        20,
        10,
        Some(cost + next_cost),
        CacheRetention::Short
    )
    .is_ok());
    assert!(
        matches!(reserve_request_cost(&session, &model, 20, 10, Some(cost + next_cost - 1), CacheRetention::Short),
            Err(AgentError::CostLimit { current, reserved, .. }) if current == cost && reserved == next_cost)
    );
    drop(session);
    let mut session = Session::open(&path).unwrap();
    session
        .record_usage_uncertainty_with_bound(
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            "assistant_turn",
            Some(UsageUncertaintyBound {
                tokens: 7,
                cost_microdollars: None,
            }),
        )
        .unwrap();
    assert!(reserve_request_tokens(&session, 1, 1, Some(u64::MAX)).is_ok());
    assert!(matches!(
        reserve_request_cost(
            &session,
            &model,
            1,
            1,
            Some(u64::MAX),
            CacheRetention::Short
        ),
        Err(AgentError::UsageUncertain)
    ));
}

#[test]
fn permanent_codes_and_generic_connect_errors_never_authorize_outage_waiting() {
    let error = AiError::Provider(octet_ai::ProviderError {
        code: Some("invalid_request_error".into()),
        kind: Some("server_error".into()),
        message: "please try again after timeout".into(),
        request_id: None,
    });
    assert!(!retryable_stream_start(&error));
    assert!(!interrupted_inference_error(&error));
    assert!(!looks_like_context_error(&AiError::Decode(
        octet_ai::DecodeError::Json("context_length_exceeded".into())
    )));
    let recovery = PendingProviderRecovery {
        error: AiError::Transport(octet_ai::TransportError {
            phase: octet_ai::TransportPhase::Connect,
            timeout: false,
            message: "invalid certificate".into(),
        }),
        qualified: true,
        saw_generation: false,
        opened: false,
        exposure: None,
    };
    assert!(!recovery.waiting_for_network());
}

#[test]
fn presend_credential_unavailability_has_no_unknown_billable_usage() {
    for qualified in [false, true] {
        let recovery = PendingProviderRecovery {
            error: AiError::Auth(octet_ai::AuthError::Unavailable),
            qualified,
            saw_generation: false,
            opened: false,
            exposure: None,
        };
        assert_eq!(
            recovery.replacement_limit(),
            if qualified { MAX_NETWORK_RETRIES } else { 0 }
        );
        assert!(!recovery.usage_unknown());
    }
}

#[test]
fn retry_after_is_not_shortened_even_through_stream_failure_wrapper() {
    let error = AiError::Http(octet_ai::HttpError {
        status: http::StatusCode::SERVICE_UNAVAILABLE,
        request_id: None,
        retry_after: Some(Duration::from_secs(120)),
        provider_code: None,
        body_snippet: None,
        retryable: true,
    });
    assert_eq!(retry_after(&error, 0), Duration::from_secs(120));
}

struct StopRecovery;
#[async_trait::async_trait]
impl ProviderRetryHook for StopRecovery {
    async fn provider_retry(&self, context: &ProviderRetryContext) -> ProviderRetryAdvice {
        assert_eq!(context.kind, ProviderRetryKind::InterruptedInference);
        ProviderRetryAdvice::Stop
    }
}

#[tokio::test]
async fn hard_token_ceiling_without_room_and_hook_veto_stop_interrupted_inference_before_replacement(
) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    // A ceiling with room charges each interrupted attempt its admitted
    // bound and replays as usual. Once that charge leaves no room, the
    // replacement is refused before dispatch, as it is by a hook veto.
    let mut attempt_bound = None;
    for case in ["roomy_ceiling", "tight_ceiling", "hook_veto"] {
        let hard_token_limit = case != "hook_veto";
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("responses"))
                .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream")
                    .set_body_string("data: {\"type\":\"response.created\",\"response\":{\"id\":\"failed\"}}\n\ndata: {\"type\":\"error\",\"code\":\"server_error\",\"message\":\"interrupted\"}\n\n"))
                .mount(&server).await;
        let directory = tempfile::tempdir().unwrap();
        let mut model = model();
        Arc::make_mut(&mut model.endpoint).transport = octet_ai::EndpointTransport::Http;
        if hard_token_limit {
            // HTTP uncertainty coverage needs a genuinely capped route;
            // uncapped Codex hard ceilings now refuse before dispatch.
            Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
                octet_ai::ResponsesRuntimeProfile::Default;
        }
        Arc::make_mut(&mut model.endpoint).base_url =
            url::Url::parse(&format!("{}/", server.uri())).unwrap();
        Arc::make_mut(&mut model.endpoint).auth = octet_ai::Auth::bearer("synthetic");
        let mut extensions = ExtensionHost::new();
        if !hard_token_limit {
            extensions.provider_retry_hook(StopRecovery);
        }
        let mut agent = Agent::new(AgentConfig {
            client: AiClient::new(),
            model,
            session: Session::create(directory.path().join("session.jsonl")).unwrap(),
            system: "system".into(),
            sandbox: SandboxConfig::new(directory.path()),
            effect_broker: EffectBroker::default(),
            extensions,
            max_turns: Some(1),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: ReasoningMode::Standard,
            cache_retention: CacheRetention::Short,
            session_id: None,
        })
        .unwrap();
        match case {
            "roomy_ceiling" => agent.set_max_session_tokens(Some(u64::MAX)),
            "tight_ceiling" => agent.set_max_session_tokens(attempt_bound),
            _ => {}
        }
        let error = agent.complete("finish").await.unwrap_err();
        let requests = server.received_requests().await.unwrap();
        match case {
            "roomy_ceiling" => {
                assert!(
                    matches!(
                        error,
                        AgentError::ProviderRecovery {
                            usage_unknown: true,
                            ..
                        }
                    ),
                    "{error:?}"
                );
                assert!(requests.len() > 1);
                let records = agent.session.usage_uncertainty_records().len();
                assert_eq!(records, requests.len());
                let exposure = agent
                    .session
                    .usage_uncertainty_exposure()
                    .expect("capped attempts are bounded");
                assert_eq!(exposure.tokens % records as u64, 0);
                attempt_bound = Some(exposure.tokens / records as u64);
            }
            "tight_ceiling" => {
                let bound = attempt_bound.unwrap();
                assert!(
                    matches!(
                        error,
                        AgentError::TokenLimit { current, limit, .. }
                            if current == bound && limit == bound
                    ),
                    "{error:?}"
                );
                assert_eq!(requests.len(), 1);
            }
            _ => assert_eq!(requests.len(), 1),
        }
    }
}
