//! Deterministic accepted-result handoff, failed append and wire-cap regressions.
use super::*;
use octet_ai::{ModelId, Response, ResponsesRuntimeProfile};
use std::sync::atomic::AtomicUsize;

const SETTLED_USAGE: Usage = Usage {
    input_tokens: 11,
    cache_read_tokens: 0,
    cache_write_tokens: 0,
    cache_write_1h_tokens: 0,
    output_tokens: 3,
    reasoning_tokens: 0,
    total_tokens: 14,
};

fn settled_cost() -> Cost {
    Cost {
        total: 17,
        total_picodollars_remainder: 123,
        ..Cost::default()
    }
}

struct FinishTransport {
    cancel: Option<CancellationToken>,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl octet_ai::HostStreamTransport for FinishTransport {
    async fn stream(
        &self,
        model: octet_ai::HostStreamModel,
        _: Request,
        _: Vec<octet_ai::Diagnostic>,
    ) -> Result<octet_ai::ResponseStream, AiError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let cancel = self.cancel.clone();
        // A host stream must open with `Started`; cancellation is raised only on
        // the terminal item so the abort branch was already polled Pending when
        // the accepted result becomes Ready in that same poll.
        Ok(Box::pin(futures_util::stream::iter(vec![
            Ok(StreamEvent::Started {
                response_id: None,
            }),
            once_finished(model, cancel),
        ])))
    }
}

fn once_finished(
    model: octet_ai::HostStreamModel,
    cancel: Option<CancellationToken>,
) -> Result<StreamEvent, AiError> {
    if let Some(cancel) = cancel {
        cancel.cancel();
    }
    Ok(StreamEvent::Finished(Response {
        message: AssistantMessage {
            content: vec![AssistantPart::Text("R".into())],
            model: model.id,
            protocol: model.protocol,
        },
        stop_reason: StopReason::EndTurn,
        usage: SETTLED_USAGE,
        cost: Some(settled_cost()),
        response_id: None,
        responses_output: None,
        deferred: None,
        diagnostics: Vec::new(),
    }))
}

fn fixture(cancel: Option<CancellationToken>) -> (Agent, Arc<FinishTransport>, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    let client = AiClient::new();
    let transport = Arc::new(FinishTransport {
        cancel,
        calls: AtomicUsize::new(0),
    });
    client.register_host_stream_transport(model.endpoint.id.clone(), transport.clone());
    let agent = Agent::new(AgentConfig {
        client,
        model,
        session: Session::create(directory.path().join("session.jsonl")).unwrap(),
        system: "test".into(),
        sandbox: SandboxConfig::new(directory.path()),
        effect_broker: EffectBroker::default(),
        extensions: ExtensionHost::new(),
        max_turns: Some(1),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    (agent, transport, directory)
}

async fn gate(agent: &mut Agent, abort: &AbortFlag) -> Result<TerminalGateDecision, AgentError> {
    let (events, _) = mpsc::unbounded_channel();
    let mut usage = Usage::default();
    let mut cost = CostAccumulator::default();
    let result = TerminalGateContext {
        run_id: "test",
        resource_owner: "test",
        retry_hooks: &[],
        max_network_wait: None,
        provider_retries_enabled: true,
        events: &events,
        client: &agent.client,
        model: &agent.model,
        session: &mut agent.session,
        usage: &mut usage,
        run_cost: &mut cost,
        cache_retention: CacheRetention::Short,
        session_id: "test",
        max_session_tokens: agent.max_session_tokens,
        max_session_cost_microdollars: agent.max_session_cost_microdollars,
        abort,
    }
    .decide("candidate".into())
    .await;
    if !agent.session.usage_records().is_empty() {
        assert_eq!(usage, SETTLED_USAGE);
        assert_eq!(cost.microdollars, settled_cost().total);
        assert_eq!(
            cost.picodollars_remainder,
            settled_cost().total_picodollars_remainder
        );
    }
    result
}

#[tokio::test]
async fn successful_same_poll_cancellation_and_failed_settlement_never_erase_exposure() {
    for operation in ["local", "branch", "gate"] {
        for failure in ["none", "once", "persistent"] {
            let cancellation = CancellationToken::default();
            let (mut agent, transport, _directory) = fixture(Some(cancellation.clone()));
            let path = agent.session.path().to_owned();
            if failure == "once" {
                agent.session.fail_next_append();
            }
            if failure == "persistent" {
                agent.session = Session::open_read_only(&path).unwrap();
            }
            let model = agent.model.clone();
            let result = match operation {
                "local" => agent
                    .summarize_with_retry(
                        &model,
                        "summary",
                        Vec::new(),
                        128,
                        cancellation.clone(),
                        std::mem::drop,
                    )
                    .await
                    .map(|_| ()),
                "branch" => agent
                    .summarize_branch_with_retry(
                        &crate::compaction::prepare_branch_handoff(Vec::new(), &Default::default()),
                        cancellation.clone(),
                        std::mem::drop,
                    )
                    .await
                    .map(|_| ()),
                _ => gate(
                    &mut agent,
                    &AbortFlag {
                        cancellation: cancellation.clone(),
                        ..Default::default()
                    },
                )
                .await
                .map(|_| ()),
            };
            assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
            assert!(
                agent.session.entries().is_empty(),
                "cancelled output must not commit"
            );
            if failure == "none" {
                assert!(matches!(result, Err(AgentError::Cancelled)), "{result:?}");
                assert_eq!(agent.session.usage_records().len(), 1);
                assert_eq!(agent.session.usage_records()[0].usage, SETTLED_USAGE);
                assert_eq!(agent.session.usage_records()[0].cost, Some(settled_cost()));
                assert!(!agent.session.has_uncertain_usage());
            } else {
                assert!(matches!(result, Err(AgentError::Session(_))), "{result:?}");
                assert!(agent.session.usage_records().is_empty());
                assert_eq!(agent.session.usage_uncertainty_records().len(), 1);
                agent.set_max_session_cost_microdollars(Some(u64::MAX));
                assert!(matches!(
                    agent.ensure_request_cost_capacity(&model, 1, 1),
                    Err(AgentError::UsageUncertain)
                ));
            }
            drop(agent);
            let reopened = Session::open_read_only(&path).unwrap();
            if failure == "none" {
                assert_eq!(reopened.usage_records()[0].cost, Some(settled_cost()));
            } else if failure == "once" {
                assert_eq!(reopened.usage_uncertainty_records().len(), 1);
            } else {
                // A permanently unwritable descriptor cannot promise durable
                // fallback. The live owner nevertheless stayed budget-closed.
                assert!(reopened.usage_uncertainty_records().is_empty());
            }
        }
    }
}

#[tokio::test]
async fn native_compact_success_handoff_keeps_guard_until_accounting_is_durable() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    for fail_append in [false, true] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("responses/compact"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "output": [{"type":"compaction","encrypted_content":"checkpoint"}],
                "usage": {"input_tokens":11,"output_tokens":3,"total_tokens":14}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("native.jsonl")).unwrap();
        if fail_append {
            session.fail_next_append();
        }
        let mut model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("gpt-5.4-mini-responses".into()))
            .unwrap();
        Arc::make_mut(&mut model.endpoint).base_url =
            url::Url::parse(&format!("{}/", server.uri())).unwrap();
        Arc::make_mut(&mut model.endpoint).auth = octet_ai::Auth::bearer("test");
        let request = ResponsesCompactRequest::for_model(
            &model,
            ResponsesInput::new(vec![octet_ai::ResponsesItem::new(serde_json::json!({
                "type":"message","role":"user","content":"summarize"
            }))
            .unwrap()]),
            None,
            &[],
            &ReasoningConfig::Off,
            ReasoningMode::Standard,
            &OutputFormat::Text,
            CacheRetention::Short,
            Some("native-test"),
        )
        .unwrap();
        let client = AiClient::new();
        let abort = AbortFlag::default();
        let (events, _) = mpsc::unbounded_channel();
        let result = recover_auxiliary(
            AuxiliaryRecovery {
                dispatch: AuxiliaryDispatch::default(),
                session: &mut session,
                run_id: "test",
                resource_owner: "test",
                retry_hooks: &[],
                max_network_wait: None,
                model: &model,
                qualified: false,
                enabled: true,
                hard_budget: false,
                abort: &abort,
                events: &events,
                operation: crate::events::ProviderOperation::NativeCompaction,
                session_id: "test",
            },
            |deadline, dispatch| {
                let client = &client;
                let model = &model;
                let request = request.clone();
                let abort = &abort;
                async move {
                    let response =
                        auxiliary_compact(client, model, request, deadline, None, dispatch).await?;
                    // The actual compact transport completed; cancel before this
                    // successful call future returns Ready to the recovery owner.
                    abort.set();
                    Ok(response)
                }
            },
            |session, response| {
                let cost = octet_ai::pricing::cost_of(
                    model.spec.pricing.as_ref().unwrap(),
                    &response.usage,
                )
                .unwrap();
                session.record_compaction_usage(
                    model.endpoint.id.clone(),
                    model.spec.id.clone(),
                    response.usage,
                    Some(cost),
                )?;
                Ok(())
            },
        )
        .await;
        assert!(abort.is_set());
        assert!(session.entries().is_empty());
        if fail_append {
            assert!(matches!(result, Err(AgentError::Session(_))));
            assert_eq!(session.usage_uncertainty_records().len(), 1);
            assert_eq!(
                session.usage_uncertainty_records()[0].operation,
                "native_compaction"
            );
        } else {
            assert!(result.is_ok());
            assert_eq!(session.usage_records()[0].usage, SETTLED_USAGE);
            assert!(session.usage_records()[0].cost.is_some());
        }
        drop(session);
        let reopened = Session::open_read_only(directory.path().join("native.jsonl")).unwrap();
        assert_eq!(reopened.has_uncertain_usage(), fail_append);
        assert_eq!(reopened.usage_records().len(), usize::from(!fail_append));
    }
}

#[tokio::test]
async fn omitted_output_caps_refuse_every_hard_ceiling_consumer_before_dispatch() {
    for codex in [false, true] {
        for token_ceiling in [false, true] {
            for operation in ["main", "local", "branch", "gate", "prospective"] {
                let server = wiremock::MockServer::start().await;
                let (mut agent, transport, _directory) = fixture(None);
                agent
                    .client
                    .remove_host_stream_transport(&agent.model.endpoint.id);
                Arc::make_mut(&mut agent.model.endpoint).base_url =
                    url::Url::parse(&format!("{}/", server.uri())).unwrap();
                Arc::make_mut(&mut agent.model.endpoint).auth = octet_ai::Auth::bearer("test");
    Arc::make_mut(&mut agent.model.endpoint).transport = octet_ai::EndpointTransport::Http;
                let spec = Arc::make_mut(&mut agent.model.spec);
                spec.limits.max_output_tokens = 4096;
                if codex {
                    Arc::make_mut(&mut agent.model.endpoint)
                        .runtime
                        .responses_profile = ResponsesRuntimeProfile::Codex;
                } else {
                    spec.preset.supports_max_output_tokens = Some(false);
                }
                agent.inherit_max_output_tokens(128);
                if token_ceiling {
                    agent.set_max_session_tokens(Some(2048));
                } else {
                    agent.set_max_session_cost_microdollars(Some(2048));
                }
                let model = agent.model.clone();
                let result = match operation {
                    "main" => agent.complete("small request").await.map(|_| ()),
                    "local" => agent
                        .summarize_with_retry(
                            &model,
                            "summary",
                            Vec::new(),
                            128,
                            CancellationToken::default(),
                            std::mem::drop,
                        )
                        .await
                        .map(|_| ()),
                    "branch" => agent
                        .summarize_branch_with_retry(
                            &crate::compaction::prepare_branch_handoff(
                                Vec::new(),
                                &Default::default(),
                            ),
                            CancellationToken::default(),
                            std::mem::drop,
                        )
                        .await
                        .map(|_| ()),
                    "gate" => gate(&mut agent, &AbortFlag::default()).await.map(|_| ()),
                    _ => agent.ensure_request_cost_capacity(&model, 1, 128),
                };
                assert!(
                    matches!(result, Err(AgentError::OutputLimitUnavailable)),
                    "{operation}/{codex}/{token_ceiling}: {result:?}"
                );
                assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
                assert!(
                    server.received_requests().await.unwrap().is_empty(),
                    "uncapped operation must never reach HTTP"
                );
                assert!(agent.session.usage_records().is_empty());
                assert!(!agent.session.has_uncertain_usage());
            }
        }
    }
}

#[tokio::test]
async fn capped_main_and_gate_reservations_match_the_actual_wire_fields() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    let frames = [
        serde_json::json!({"type":"response.created","response":{"id":"bounded"}}),
        serde_json::json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"message"}}),
        serde_json::json!({"type":"response.output_text.delta","output_index":0,"delta":"R"}),
        serde_json::json!({"type":"response.output_text.done","output_index":0}),
        serde_json::json!({"type":"response.completed","response":{"output":[{"type":"message","id":"message","role":"assistant","content":[{"type":"output_text","text":"R"}]}],"usage":{"input_tokens":5,"output_tokens":1,"total_tokens":6}}}),
    ].into_iter().map(|frame| format!("data: {frame}\n\n")).collect::<String>();
    Mock::given(method("POST"))
        .and(path("responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(frames),
        )
        .expect(2)
        .mount(&server)
        .await;
    let (mut agent, _, _directory) = fixture(None);
    agent
        .client
        .remove_host_stream_transport(&agent.model.endpoint.id);
    Arc::make_mut(&mut agent.model.endpoint).base_url =
        url::Url::parse(&format!("{}/", server.uri())).unwrap();
    Arc::make_mut(&mut agent.model.endpoint).auth = octet_ai::Auth::bearer("test");
    Arc::make_mut(&mut agent.model.endpoint).transport = octet_ai::EndpointTransport::Http;
    Arc::make_mut(&mut agent.model.spec)
        .limits
        .max_output_tokens = 4096;
    agent.inherit_max_output_tokens(128);
    agent.set_completion_policy(CompletionPolicy::TerminalGate);
    agent.set_max_session_tokens(Some(2048));
    agent.set_max_session_cost_microdollars(Some(2048));
    assert_eq!(agent.complete("small request").await.unwrap().text, "R");
    let requests = server.received_requests().await.unwrap();
    let main: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let gate: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(main["max_output_tokens"], 128);
    assert_eq!(gate["max_output_tokens"], 1);
}
