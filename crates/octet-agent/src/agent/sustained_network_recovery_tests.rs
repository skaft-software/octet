use super::*;
// The /fast tier-reservation test builds the same Codex model as the
// inference-recovery tests; share that one constructor rather than
// letting two copies drift.
use super::inference_recovery_tests::model;
use std::sync::atomic::{AtomicUsize, Ordering};

struct OfflineTransport {
    calls: AtomicUsize,
    fail_until: usize,
}
#[async_trait::async_trait]
impl octet_ai::HostStreamTransport for OfflineTransport {
    async fn stream(
        &self,
        model: octet_ai::HostStreamModel,
        _request: Request,
        _: Vec<octet_ai::Diagnostic>,
    ) -> Result<octet_ai::ResponseStream, AiError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call < self.fail_until {
            return Err(AiError::Auth(octet_ai::AuthError::Unavailable));
        }
        Ok(Box::pin(futures_util::stream::iter([
            Ok(StreamEvent::Started { response_id: None }),
            Ok(StreamEvent::Finished(octet_ai::Response {
                message: AssistantMessage {
                    content: vec![AssistantPart::Text("recovered".into())],
                    model: model.id.clone(),
                    protocol: model.protocol,
                },
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                cost: None,
                response_id: None,
                responses_output: None,
                deferred: None,
                inference: None,
                diagnostics: Vec::new(),
            })),
        ])))
    }
}

#[tokio::test(start_paused = true)]
async fn qualified_presend_outage_waits_beyond_finite_budget_and_is_cancellable() {
    let directory = tempfile::tempdir().unwrap();
    let mut model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
        octet_ai::ResponsesRuntimeProfile::Codex;
    let client = AiClient::new();
    let transport = Arc::new(OfflineTransport {
        calls: AtomicUsize::new(0),
        fail_until: usize::MAX,
    });
    client.register_host_stream_transport(model.endpoint.id.clone(), transport.clone());
    let mut agent = Agent::new(AgentConfig {
        client,
        model,
        session: Session::create(directory.path().join("session.jsonl")).unwrap(),
        system: "system".into(),
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
    agent.set_max_session_tokens(Some(u64::MAX));
    assert!(matches!(
        agent.complete("bounded uncapped route").await,
        Err(AgentError::OutputLimitUnavailable)
    ));
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    // The original long-outage behavior remains available only without a
    // hard ceiling on this cap-omitting Codex route.
    agent.set_max_session_tokens(None);
    let mut run = agent.prompt("wait through the outage").await.unwrap();
    let control = run.control();
    let mut waits = 0;
    let mut terminals = 0;
    let started = tokio::time::Instant::now();
    while let Some(event) = run.next().await {
        match event {
            AgentEvent::ProviderWaitingForNetwork { attempt, delay, .. } => {
                waits += 1;
                assert_eq!(attempt, waits);
                assert!((Duration::from_secs(4)..=Duration::from_secs(60)).contains(&delay));
                if waits == 20_200 {
                    control.abort();
                }
            }
            AgentEvent::ProviderRetry { .. } => {
                panic!("pre-send waiting spent inference replacements")
            }
            AgentEvent::RunFinished { reason, .. } => {
                terminals += 1;
                assert!(matches!(reason, FinishReason::Aborted));
            }
            _ => {}
        }
    }
    drop(run);
    assert_eq!(terminals, 1);
    assert_eq!(transport.calls.load(Ordering::SeqCst), waits);
    assert!(started.elapsed() > Duration::from_secs(14 * 24 * 60 * 60));
    // The refused hard-ceiling admission leaves the prompt's own user entry
    // plus the durable failed-turn marker, and the aborted prompt adds one
    // user entry. No assistant answer and no usage may be invented for
    // either of them.
    let entries = agent.session().entries();
    assert_eq!(entries.len(), 3, "{entries:#?}");
    assert!(matches!(
        &entries[0].value,
        EntryValue::Message(Message::User(user))
            if matches!(user.content.as_slice(), [UserPart::Text(text)] if text == "bounded uncapped route")
    ));
    assert!(matches!(
        &entries[1].value,
        EntryValue::Message(Message::Assistant(assistant))
            if assistant.content.iter().all(|part| matches!(part, AssistantPart::Text(_)))
    ));
    assert!(matches!(
        &entries[2].value,
        EntryValue::Message(Message::User(user))
            if matches!(user.content.as_slice(), [UserPart::Text(text)] if text == "wait through the outage")
    ));
    assert!(agent.session().usage_records().is_empty());
    assert!(!agent.session().has_uncertain_usage());
}

#[tokio::test(start_paused = true)]
async fn auxiliary_recovery_is_scoped_bounded_and_budget_conservative() {
    use crate::events::ProviderOperation;
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    for operation in [
        ProviderOperation::LocalCompaction,
        ProviderOperation::NativeCompaction,
        ProviderOperation::TerminalGate,
    ] {
        for hard_budget in [false, true] {
            let (events, mut receiver) = mpsc::unbounded_channel();
            let abort = AbortFlag::default();
            let calls = AtomicUsize::new(0);
            let directory = tempfile::tempdir().unwrap();
            let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
            let result = recover_auxiliary(
                AuxiliaryRecovery {
                    dispatch: AuxiliaryDispatch::default(),
                    session: &mut session,
                    run_id: "auxiliary-test",
                    resource_owner: "auxiliary-test",
                    retry_hooks: &[],
                    max_network_wait: None,
                    model: &model,
                    qualified: true,
                    enabled: true,
                    hard_budget,
                    exposure: None,
                    input_tokens: 1,
                    output_tokens: 1,
                    token_limit: hard_budget.then_some(u64::MAX),
                    cost_limit: None,
                    retention: CacheRetention::Short,
                    abort: &abort,
                    events: &events,
                    operation,
                    session_id: "auxiliary-test",
                },
                |_deadline, _dispatch| {
                    let call = calls.fetch_add(1, Ordering::SeqCst);
                    async move {
                        if call == 0 {
                            Err(AiError::StreamProtocol(
                                octet_ai::StreamProtocolError::PrematureEof,
                            )
                            .into())
                        } else {
                            Ok(())
                        }
                    }
                },
                |_, _| Ok(()),
            )
            .await;
            assert!(matches!(
                receiver.try_recv(),
                Ok(AgentEvent::ProviderUsageUncertain)
            ));
            if hard_budget {
                assert!(matches!(
                    result,
                    Err(AgentError::ProviderRecovery {
                        retries: 0,
                        usage_unknown: true,
                        ..
                    })
                ));
                assert!(receiver.try_recv().is_err());
            } else {
                assert!(result.is_ok());
                assert!(
                    matches!(receiver.try_recv().unwrap(), AgentEvent::ProviderOperationRetry { operation: observed, max_attempts: Some(11), .. } if observed == operation)
                );
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn auxiliary_network_wait_obeys_host_outage_limit() {
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    let (events, mut receiver) = mpsc::unbounded_channel();
    let abort = AbortFlag::default();
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let result: Result<(), _> = recover_auxiliary(
        AuxiliaryRecovery {
            dispatch: AuxiliaryDispatch::default(),
            session: &mut session,
            run_id: "auxiliary-test",
            resource_owner: "auxiliary-test",
            retry_hooks: &[],
            max_network_wait: Some(Duration::from_secs(1)),
            model: &model,
            qualified: true,
            enabled: true,
            hard_budget: true,
            exposure: None,
            input_tokens: 1,
            output_tokens: 1,
            token_limit: Some(u64::MAX),
            cost_limit: None,
            retention: CacheRetention::Short,
            abort: &abort,
            events: &events,
            operation: crate::events::ProviderOperation::LocalCompaction,
            session_id: "bounded",
        },
        |_deadline, _dispatch| async {
            Err(AiError::Auth(octet_ai::AuthError::Unavailable).into())
        },
        |_, _| Ok(()),
    )
    .await;
    assert!(matches!(result, Err(AgentError::NetworkWaitLimit { .. })));
    assert!(matches!(
        receiver.try_recv(),
        Ok(AgentEvent::ProviderOperationRetry { .. })
    ));
}

#[test]
fn network_backoff_is_bounded_and_jittered_even_after_counter_saturation() {
    for attempt in [0, 1, 2, 3, 100, usize::MAX] {
        let delay = network_wait_delay("run-a", attempt);
        assert!((Duration::from_secs(4)..=Duration::from_secs(60)).contains(&delay));
    }
    assert_ne!(
        network_wait_delay("run-a", 0),
        network_wait_delay("run-b", 0)
    );
}

#[test]
fn typed_unknown_failed_and_all_5xx_require_qualified_replacement_authority() {
    for qualified in [false, true] {
        let provider = || octet_ai::ProviderError {
            code: Some("future_unknown_reason".into()),
            kind: None,
            message: "unknown".into(),
            request_id: None,
        };
        for (error, expected) in [
            (
                AiError::ResponsesFailed(provider()),
                if qualified { 11 } else { 0 },
            ),
            (AiError::Provider(provider()), 0),
            (
                AiError::Http(octet_ai::HttpError {
                    status: http::StatusCode::from_u16(520).unwrap(),
                    request_id: None,
                    retry_after: None,
                    provider_code: None,
                    body_snippet: None,
                    retryable: false,
                }),
                if qualified { 29 } else { 0 },
            ),
        ] {
            let recovery = PendingProviderRecovery {
                error,
                qualified,
                saw_generation: false,
                opened: true,
                exposure: None,
            };
            assert_eq!(recovery.replacement_limit(), expected);
            assert!(recovery.usage_unknown());
        }
    }
}

#[test]
fn permanent_response_codes_veto_context_sounding_text_and_preserve_rate_hints() {
    for code in [
        "cyber_policy",
        "bio_policy",
        "invalid_prompt",
        "misalignment_policy_violation",
        "server_is_overloaded",
        "slow_down",
        "invalid_api_key",
        "insufficient_quota",
    ] {
        let error = AiError::ResponsesFailed(octet_ai::ProviderError {
            code: Some(code.into()),
            kind: Some("context_length_exceeded".into()),
            message: "context length exceeded; try again in 1s".into(),
            request_id: None,
        });
        assert!(!looks_like_context_error(&error), "{code}");
        assert!(!interrupted_inference_error(&error), "{code}");
        assert!(!retryable_stream_start(&error), "{code}");
    }
    let error = AiError::ResponsesFailed(octet_ai::ProviderError {
        code: Some("rate_limit_exceeded".into()),
        kind: None,
        message: "try again in 11054ms".into(),
        request_id: None,
    });
    assert_eq!(retry_after(&error, 0), Duration::from_millis(11054));
}
#[test]
fn hard_cost_reservation_covers_pricier_anthropic_server_fallbacks() {
    use octet_ai::declarations::{
        AnthropicCompatPreset, AnthropicFallbackCost, AnthropicFallbackModel,
    };
    use octet_ai::{Pricing, TokenRate};

    let mut model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("claude-sonnet-4-5".into()))
        .unwrap();
    Arc::make_mut(&mut model.spec).pricing = Some(Pricing {
        input: TokenRate(1_000_000),
        output: TokenRate(1_000_000),
        cache_read: TokenRate(1_000_000),
        cache_write_5m: TokenRate(1_000_000),
        cache_write_1h: None,
        reasoning: None,
        tiers: vec![],
    });
    let base = worst_case_request_cost(&model, 1_000_000, 1_000_000, None).unwrap();
    assert_eq!(base, 3_000_000);
    Arc::make_mut(&mut model.spec).preset.anthropic_compat = Some(AnthropicCompatPreset {
        allowed_fallback_models: vec![AnthropicFallbackModel {
            provider: "anthropic".into(),
            model: "dearer".into(),
            cost: Some(AnthropicFallbackCost {
                input: 5.0,
                output: 20.0,
                cache_read: 0.5,
                cache_write: 6.0,
            }),
        }],
        ..Default::default()
    });
    assert_eq!(
        worst_case_request_cost(&model, 1_000_000, 1_000_000, None),
        Some(30_000_000)
    );
    let directory = tempfile::tempdir().unwrap();
    let session = Session::create(directory.path().join("cost.jsonl")).unwrap();
    assert!(matches!(
        reserve_request_cost(
            &session,
            &model,
            1_000_000,
            1_000_000,
            Some(base + 1),
            CacheRetention::Short
        ),
        Err(AgentError::CostLimit { .. })
    ));
    assert!(model.spec.cache.supports_long_retention);
    assert!(reserve_request_cost(
        &session,
        &model,
        1,
        1,
        Some(u64::MAX),
        CacheRetention::Short,
    )
    .is_ok());
    assert!(matches!(
        reserve_request_cost(&session, &model, 1, 1, Some(u64::MAX), CacheRetention::Long),
        Err(AgentError::CostUnavailable { .. })
    ));
    assert!(reserve_request_cost(&session, &model, 1, 1, None, CacheRetention::Long,).is_ok());
    Arc::make_mut(&mut model.spec)
        .preset
        .anthropic_compat
        .as_mut()
        .unwrap()
        .allowed_fallback_models[0]
        .cost = None;
    assert!(matches!(
        reserve_request_cost(
            &session,
            &model,
            1,
            1,
            Some(u64::MAX),
            CacheRetention::Short
        ),
        Err(AgentError::CostUnavailable { .. })
    ));
}

#[test]
fn tier_reservation_uses_the_declared_tariff_and_blocks_unpriced_history() {
    let mut model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
        octet_ai::ResponsesRuntimeProfile::Codex;
    Arc::make_mut(&mut model.spec).api_name = "gpt-5.5".into();
    let base = worst_case_request_cost(&model, 1_000_000, 1_000_000, None).unwrap();
    let priority =
        worst_case_request_cost(&model, 1_000_000, 1_000_000, Some(ServiceTier::Priority)).unwrap();
    assert!(priority >= base.saturating_mul(5) / 2);
    assert!(worst_case_request_cost(&model, 1, 1, Some(ServiceTier::Auto)).is_none());
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unpriced.jsonl");
    let mut session = Session::create(&path).unwrap();
    assert!(matches!(
        reserve_request_cost_with_tier(
            &session,
            &model,
            1_000_000,
            1_000_000,
            Some(base + 1),
            Some(ServiceTier::Priority),
            CacheRetention::Short,
        ),
        Err(AgentError::CostLimit { .. })
    ));
    session
        .record_compaction_usage(
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            Usage {
                input_tokens: 1,
                ..Usage::default()
            },
            None,
        )
        .unwrap();
    drop(session);
    let reopened = Session::open(path).unwrap();
    assert!(matches!(
        reserve_request_cost(
            &reopened,
            &model,
            1,
            1,
            Some(u64::MAX),
            CacheRetention::Short
        ),
        Err(AgentError::CostUnavailable { .. })
    ));
    assert!(reserve_request_cost(&reopened, &model, 1, 1, None, CacheRetention::Short).is_ok());
}

#[test]
fn tariff_reservations_bound_exact_cost_across_usage_buckets_and_context_tiers() {
    use octet_ai::{Pricing, PricingTier, ResponsesRuntimeProfile, TokenRate};
    let mut model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    let mut checked = 0usize;
    for one_hour in [None, Some(TokenRate(31_000_000))] {
        let pricing = Pricing {
            input: TokenRate(2_000_000),
            output: TokenRate(10_000_000),
            cache_read: TokenRate(700_000),
            cache_write_5m: TokenRate(3_000_000),
            cache_write_1h: one_hour,
            reasoning: Some(TokenRate(13_000_000)),
            tiers: vec![PricingTier {
                min_input_tokens: 200_000,
                input: Some(TokenRate(12_000_000)),
                output: Some(TokenRate(20_000_000)),
                cache_read: None,
                cache_write_5m: Some(TokenRate(2_000_000)),
                cache_write_1h: None,
                reasoning: Some(TokenRate(30_000_000)),
            }],
        };
        Arc::make_mut(&mut model.spec).pricing = Some(pricing.clone());
        for profile in [
            ResponsesRuntimeProfile::Default,
            ResponsesRuntimeProfile::Codex,
        ] {
            Arc::make_mut(&mut model.endpoint).runtime.responses_profile = profile;
            for api_name in ["gpt-5.4", "gpt-5.5"] {
                Arc::make_mut(&mut model.spec).api_name = api_name.into();
                for tier in [
                    None,
                    Some(ServiceTier::Default),
                    Some(ServiceTier::Flex),
                    Some(ServiceTier::Priority),
                ] {
                    if profile == ResponsesRuntimeProfile::Default
                        && matches!(tier, Some(ServiceTier::Flex | ServiceTier::Priority))
                    {
                        assert!(worst_case_request_cost(&model, 200_001, 8192, tier).is_none());
                        continue;
                    }
                    for input in [0, 1, 7, 199_999, 200_000, 200_001] {
                        for output in [0, 1, 29, 8192] {
                            let reserved =
                                worst_case_request_cost(&model, input, output, tier).unwrap();
                            for bucket in 0..5 {
                                let mut usage = Usage {
                                    output_tokens: output,
                                    total_tokens: input + output,
                                    ..Usage::default()
                                };
                                match bucket {
                                    0 => usage.input_tokens = input,
                                    1 => usage.cache_read_tokens = input,
                                    2 => usage.cache_write_tokens = input,
                                    3 => {
                                        usage.cache_write_tokens = input;
                                        usage.cache_write_1h_tokens = input;
                                    }
                                    _ => {
                                        usage.input_tokens = input / 3;
                                        usage.cache_read_tokens = input / 3;
                                        usage.cache_write_tokens =
                                            input - usage.input_tokens - usage.cache_read_tokens;
                                        usage.cache_write_1h_tokens = usage.cache_write_tokens / 2;
                                    }
                                }
                                for reasoning in [0, output / 2, output] {
                                    usage.reasoning_tokens = reasoning;
                                    let actual = octet_ai::responses_cost_of(
                                        &pricing, &usage, profile, api_name, tier, None,
                                    )
                                    .unwrap()
                                    .unwrap();
                                    let picodollars = u128::from(actual.total)
                                        * u128::from(PICODOLLARS_PER_MICRODOLLAR)
                                        + u128::from(actual.total_picodollars_remainder);
                                    assert!(picodollars <= u128::from(reserved) * u128::from(PICODOLLARS_PER_MICRODOLLAR),
                                            "under-reservation: profile={profile:?} model={api_name} tier={tier:?} usage={usage:?} actual={actual:?} reserved={reserved}");
                                    checked += 1;
                                }
                            }
                        }
                    }
                }
                assert!(
                    worst_case_request_cost(&model, 200_001, 8192, Some(ServiceTier::Auto))
                        .is_none()
                );
            }
        }
    }
    assert_eq!(checked, 8640);
}

/// Row `/fast`: the worst-case reservation must hold across a restart, a
/// durable exact auxiliary cost, and a hard budget boundary. A selected
/// priority tier raises the pre-request reservation above the untiered
/// value; a cheaper provider echo cannot lower it because the reservation
/// helper has no echo input at all.
#[test]
fn tier_reservations_hold_across_restart_and_bound_the_hard_budget() {
    use octet_ai::{Cost, Pricing, TokenRate};
    let mut model = model();
    Arc::make_mut(&mut model.spec).api_name = "gpt-5.5".into();
    assert_eq!(
        model.endpoint.runtime.responses_profile,
        octet_ai::ResponsesRuntimeProfile::Codex
    );
    let pricing = Pricing {
        input: TokenRate(2_000_000),
        output: TokenRate(10_000_000),
        cache_read: TokenRate(700_000),
        cache_write_5m: TokenRate(3_000_000),
        cache_write_1h: None,
        reasoning: Some(TokenRate(13_000_000)),
        tiers: Vec::new(),
    };
    let mut model = model;
    Arc::make_mut(&mut model.spec).pricing = Some(pricing);
    let input = 12_345u64;
    let output = 4_096u64;
    let base = worst_case_request_cost(&model, input, output, None).unwrap();
    let priority =
        worst_case_request_cost(&model, input, output, Some(ServiceTier::Priority)).unwrap();
    assert!(
        priority >= base.saturating_mul(2),
        "the gpt-5.5 priority tariff must bound the reservation: base={base} priority={priority}"
    );
    // `auto` cannot be reserved at all; unknown metadata is never priced.
    assert!(worst_case_request_cost(&model, input, output, Some(ServiceTier::Auto)).is_none());

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("restart-budget.jsonl");
    let mut session = Session::create(&path).unwrap();
    // One durable, exactly priced auxiliary operation: local summaries do
    // not carry the main request tier, and their known cost must still
    // count against a later main-request reservation after a restart.
    let durable_cost = Cost {
        input: 400,
        output: 600,
        total: 1_000,
        ..Cost::default()
    };
    session
        .record_compaction_usage(
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            Usage {
                input_tokens: 20,
                output_tokens: 30,
                total_tokens: 50,
                ..Usage::default()
            },
            Some(durable_cost),
        )
        .unwrap();
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.total_cost_microdollars(), 1_000);
    assert!(!reopened.has_unpriced_usage());

    // Exact boundary: current + the tier-aware reservation is allowed.
    assert!(reserve_request_cost_with_tier(
        &reopened,
        &model,
        input,
        output,
        Some(1_000 + priority),
        Some(ServiceTier::Priority),
        CacheRetention::Short,
    )
    .is_ok());
    // One microdollar tighter is refused with the same reservation.
    assert!(matches!(
        reserve_request_cost_with_tier(
            &reopened,
            &model,
            input,
            output,
            Some(1_000 + priority - 1),
            Some(ServiceTier::Priority),
            CacheRetention::Short,
        ),
        Err(AgentError::CostLimit {
            current: 1_000,
            reserved,
            ..
        }) if reserved == priority
    ));
    // The selected tier is load-bearing: the same budget admits the
    // untiered reservation used by auxiliary operations, which is exactly
    // why a priority main request must reserve the tier-aware amount.
    assert!(reserve_request_cost(
        &reopened,
        &model,
        input,
        output,
        Some(1_000 + priority - 1),
        CacheRetention::Short
    )
    .is_ok());
    // A cheap provider echo cannot reduce the reservation: the helper has
    // no echo input and always prices the requested tier.
    let reserved_again =
        worst_case_request_cost(&model, input, output, Some(ServiceTier::Priority)).unwrap();
    assert_eq!(reserved_again, priority);
    // Restart does not erase exposure either: an unpriced durable record
    // blocks the same hard budget on a reopened session.
    let mut session = reopened;
    session
        .record_terminal_gate_usage(
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            Usage {
                input_tokens: 1,
                total_tokens: 1,
                ..Usage::default()
            },
            None,
            Some(true),
        )
        .unwrap();
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert!(matches!(
        reserve_request_cost_with_tier(
            &reopened,
            &model,
            input,
            output,
            Some(u64::MAX),
            Some(ServiceTier::Priority),
            CacheRetention::Short,
        ),
        Err(AgentError::CostUnavailable { .. })
    ));
}
