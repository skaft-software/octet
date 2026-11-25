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
        assert_eq!(
            recovery.replacement_limit(),
            if saw_generation {
                0
            } else {
                MAX_PROVIDER_RETRIES
            }
        );
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
fn historical_bounded_exposure_accounts_exactly_but_cannot_authorize_a_new_request() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bounded.jsonl");
    let mut model = model();
    Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
        octet_ai::ResponsesRuntimeProfile::Default;
    // This fixture exercises the durable ledger's existing bounded-record
    // arithmetic, not a provider route with a trusted input cap.
    assert!(request_uncertainty_bound(&model, None, 300, None, CacheRetention::Short).is_none());
    let bound = UsageUncertaintyBound {
        tokens: 400,
        cost_microdollars: Some(worst_case_request_cost(&model, 100, 300, None).unwrap()),
    };
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
    assert!(matches!(
        require_enforceable_output_cap(&session, Some(10), Some(u64::MAX), Some(u64::MAX)),
        Err(AgentError::InputLimitUnavailable)
    ));
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
async fn unsupported_input_caps_refuse_before_dispatch_and_uncapped_hook_veto_still_stops_replacement(
) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    // Output-capped routes still lack an authoritative input bound. Both
    // finite ceiling kinds must refuse even when the nominal budget is roomy.
    for case in ["token_ceiling", "cost_ceiling", "hook_veto"] {
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
            // Enforced output caps alone cannot guarantee total-token bounds.
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
            "token_ceiling" => agent.set_max_session_tokens(Some(u64::MAX)),
            "cost_ceiling" => agent.set_max_session_cost_microdollars(Some(u64::MAX)),
            _ => {}
        }
        let error = agent.complete("finish").await.unwrap_err();
        let requests = server.received_requests().await.unwrap();
        if hard_token_limit {
            assert!(
                matches!(error, AgentError::InputLimitUnavailable),
                "{error:?}"
            );
            assert!(requests.is_empty());
            assert!(agent.session.usage_records().is_empty());
            assert!(agent.session.usage_uncertainty_records().is_empty());
        } else {
            assert_eq!(requests.len(), 1);
            assert!(matches!(error, AgentError::Ai(_)), "{error:?}");
            assert_eq!(agent.session.usage_uncertainty_records().len(), 1);
            assert!(agent.session.usage_uncertainty_exposure().is_none());
        }
    }
}

// End-to-end admission and retry checks against a loopback Responses server.
fn loopback_route(uri: &str, window: u64, codex: bool) -> Model {
    let mut model = model();
    let spec = Arc::make_mut(&mut model.spec);
    spec.limits.context_window = window;
    spec.limits.max_output_tokens = 512;
    let endpoint = Arc::make_mut(&mut model.endpoint);
    endpoint.runtime.responses_profile = if codex {
        octet_ai::ResponsesRuntimeProfile::Codex
    } else {
        octet_ai::ResponsesRuntimeProfile::Default
    };
    endpoint.transport = octet_ai::EndpointTransport::Http;
    endpoint.base_url = url::Url::parse(&format!("{uri}/")).unwrap();
    endpoint.auth = octet_ai::Auth::bearer("synthetic");
    model
}
fn loopback_agent(
    dir: &std::path::Path,
    model: Model,
    session: Session,
    system: String,
    extensions: ExtensionHost,
) -> Agent {
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model,
        session,
        system,
        sandbox: SandboxConfig::new(dir),
        effect_broker: EffectBroker::default(),
        extensions,
        max_turns: Some(1),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    agent
        .set_compaction_token_mode(AgentCompactionMode::Disabled, 0.9, 2)
        .unwrap();
    agent
}
#[tokio::test]
async fn numeric_permanent_code_under_503_sends_one_post() {
    use std::sync::atomic::AtomicUsize;
    use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};
    let mut counts = Vec::new();
    for codex in [false, true] {
        for code in [serde_json::json!(422), serde_json::json!("422")] {
            let server = MockServer::start().await;
            let calls = AtomicUsize::new(0);
            Mock::given(method("POST"))
                .respond_with(move |_: &wiremock::Request| {
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        ResponseTemplate::new(503)
                            .insert_header("Retry-After", "0")
                            .set_body_json(serde_json::json!({"error":{"code":code}}))
                    } else {
                        ResponseTemplate::new(400)
                    }
                })
                .mount(&server)
                .await;
            let dir = tempfile::tempdir().unwrap();
            let mut agent = loopback_agent(
                dir.path(),
                loopback_route(&server.uri(), 32768, codex),
                Session::create(dir.path().join("s.jsonl")).unwrap(),
                "system".into(),
                ExtensionHost::new(),
            );
            agent.set_provider_retries_enabled(true);
            let _ = agent.complete("fixture").await.unwrap_err();
            counts.push(server.received_requests().await.unwrap().len());
        }
    }
    assert_eq!(
        counts,
        [1, 1, 1, 1],
        "HTTP posts [default numeric, default string, Codex numeric, Codex string]"
    );
}
struct LongSystemOnlyHook;
#[async_trait::async_trait]
impl crate::extension::ProviderContextHook for LongSystemOnlyHook {
    async fn project_context(
        &self,
        request: &Request,
        _ctx: &crate::extension::ProviderContextProjectionContext,
    ) -> Result<Option<crate::extension::ProviderContextProjection>, String> {
        Ok(Some(crate::extension::ProviderContextProjection {
            system: Some("system instruction ".repeat(1000)),
            tools: None,
            messages: request.messages.clone(),
        }))
    }
}
#[tokio::test]
async fn system_hook_and_direct_instructions_share_checkpoint_admission() {
    use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};
    let mut posts = Vec::new();
    for hooked in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400))
            .mount(&server)
            .await;
        let route = loopback_route(&server.uri(), 12_000, false);
        let mut s = Session::create(dir.path().join("s.jsonl")).unwrap();
        s.append(user_message(UserInput::from("before checkpoint")))
            .unwrap();
        s.append_responses_compaction(route.endpoint.id.clone(),route.spec.id.clone(),octet_ai::ResponsesOutput::new(vec![octet_ai::ResponsesItem::new(serde_json::json!({"type":"compaction","id":"cmp","encrypted_content":"C".repeat(32768)})).unwrap()])).unwrap();
        let mut host = ExtensionHost::new();
        if hooked {
            host.provider_context_hook(LongSystemOnlyHook);
        }
        let system = if hooked {
            "system".into()
        } else {
            "system instruction ".repeat(1000)
        };
        let mut agent = loopback_agent(dir.path(), route, s, system, host);
        agent.set_provider_retries_enabled(false);
        let err = agent.complete("after checkpoint").await.unwrap_err();
        let requests = server.received_requests().await.unwrap();
        if !hooked {
            assert!(
                matches!(err, AgentError::ContextExceeded { .. }),
                "unexpected control: {err:?}"
            );
        }
        if let Some(sent) = requests.first() {
            let body: serde_json::Value = serde_json::from_slice(&sent.body).unwrap();
            assert!(body.get("instructions").is_none());
            assert_eq!(body["input"][0]["role"], "developer");
            assert_eq!(
                body["input"][0]["content"][0]["text"].as_str().unwrap(),
                "system instruction ".repeat(1000)
            );
            assert!(body["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["encrypted_content"].as_str().map(str::len) == Some(32768)));
        }
        eprintln!(
            "hooked={hooked} requests={} outcome={err:?}",
            requests.len()
        );
        posts.push(requests.len());
    }
    assert_eq!(
        posts,
        [0, 0],
        "same checkpoint and instructions, unhooked vs hooked"
    );
}
#[tokio::test]
async fn text_only_video_replay_and_literal_placeholder_share_admission() {
    use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};
    let mut posts = Vec::new();
    let mut expected_input = None;
    for video in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400))
            .mount(&server)
            .await;
        let mut route = loopback_route(&server.uri(), 4096, false);
        Arc::make_mut(&mut route.spec).capabilities.input_modalities =
            octet_ai::ModalitySet::none();
        let clip = Media::video_bytes(
            bytes::Bytes::from_static(include_bytes!("../../../octet-ai/src/video/tiny.mp4")),
            "video/mp4".parse().unwrap(),
        );
        let with_clip = UserMessage {
            content: vec![UserPart::Media(clip)],
        };
        let wire = octet_ai::responses::encode_responses_replay(
            &route,
            None,
            &[ResponsesReplayItem::User(with_clip.clone())],
        )
        .unwrap();
        let text = wire.items()[0].as_json()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(text.contains("original retained"));
        let path = dir.path().join("s.jsonl");
        let mut s = Session::create(&path).unwrap();
        s.append(EntryValue::Message(Message::User(if video {
            with_clip
        } else {
            UserMessage {
                content: vec![UserPart::Text(text)],
            }
        })))
        .unwrap();
        let assistant = s
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                model: route.spec.id.clone(),
                protocol: Protocol::OpenAiResponses,
                content: vec![AssistantPart::Text("prior answer".into())],
            })))
            .unwrap();
        s.append_responses_turn(assistant,route.endpoint.id.clone(),route.spec.id.clone(),octet_ai::ResponsesOutput::new(vec![octet_ai::ResponsesItem::new(serde_json::json!({"type":"message","id":"prior","role":"assistant","content":[{"type":"output_text","text":"prior answer"}]})).unwrap()])).unwrap();
        drop(s);
        let mut agent = loopback_agent(
            dir.path(),
            route.clone(),
            Session::open(&path).unwrap(),
            "system".into(),
            ExtensionHost::new(),
        );
        agent.set_provider_retries_enabled(false);
        let mut replay = (*agent
            .session()
            .responses_replay_snapshot(&route.endpoint.id, &route.spec.id)
            .unwrap()
            .unwrap())
        .clone();
        replay.push(ResponsesReplayItem::User(UserMessage {
            content: vec![UserPart::Text("follow-up".into())],
        }));
        let input =
            octet_ai::responses::encode_responses_replay(&route, Some("system"), &replay).unwrap();
        let items = serde_json::to_value(input.items()).unwrap();
        if let Some(e) = expected_input.as_ref() {
            assert_eq!(e, &items);
        } else {
            expected_input = Some(items);
        }
        let err = agent.complete("follow-up").await.unwrap_err();
        let requests = server.received_requests().await.unwrap();
        if let Some(sent) = requests.first() {
            let b: serde_json::Value = serde_json::from_slice(&sent.body).unwrap();
            assert_eq!(Some(&b["input"]), expected_input.as_ref());
        }
        eprintln!("video={video} requests={} outcome={err:?}", requests.len());
        posts.push(requests.len());
    }
    assert_eq!(
        posts,
        [1, 1],
        "identical Responses input, retained video vs literal text"
    );
}

fn frame_error(kind: Option<&str>, message: &str) -> octet_ai::ProviderError {
    octet_ai::ProviderError {
        kind: kind.map(str::to_owned),
        code: None,
        message: message.into(),
        request_id: None,
    }
}

#[test]
fn socket_error_frames_retry_only_for_known_words() {
    for word in [
        "read ECONNRESET",
        "write EPIPE",
        "connect ETIMEDOUT",
        "ECONNREFUSED",
        "ECONNABORTED",
        "Connection closed.",
        "Connection reset by peer",
    ] {
        assert!(retryable_provider_error(&frame_error(None, word)), "{word}");
    }
    for word in ["EXECONNRESET", "ETIMEDOUTX", "ERR_EPIPE_ID"] {
        assert!(
            !retryable_provider_error(&frame_error(None, word)),
            "{word}"
        );
    }
}

#[test]
fn temporary_provider_frames_retry_without_permanent_denials() {
    for (kind, message) in [
        (Some("api_error"), "Internal failure"),
        (None, "Provider returned an empty response"),
        (None, "The model stopped before completing the response."),
        (None, "Repeat the request later"),
        (None, "admission ceiling"),
        (None, "The provider is temporarily unavailable."),
        (
            None,
            "The provider ended the response before it was complete.",
        ),
        (None, "The connection to the provider was reset."),
        (
            None,
            "The provider reported a failure while generating the response.",
        ),
    ] {
        assert!(
            retryable_provider_error(&frame_error(kind, message)),
            "{message}"
        );
        assert!(!retryable_provider_error(&frame_error(
            Some("insufficient_quota"),
            message
        )));
    }
    assert!(!retryable_provider_error(&frame_error(
        Some("api_error"),
        "Monthly quota exceeded"
    )));
    assert!(!retryable_provider_error(&frame_error(
        None,
        "provider unavailable for this model"
    )));
}

#[test]
fn temporary_rate_ramp_retries_even_with_slow_down_code() {
    for code in [None, Some("slow_down"), Some("insufficient_quota")] {
        let mut error = frame_error(None, "Your request rate increased too quickly");
        error.code = code.map(str::to_owned);
        assert_eq!(
            retryable_provider_error(&error),
            code != Some("insufficient_quota")
        );
    }
}
