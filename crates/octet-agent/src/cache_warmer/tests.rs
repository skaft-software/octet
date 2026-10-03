use super::*;
use crate::session::UsageRecordKind;
use futures_util::FutureExt;
use octet_ai::{
    AssistantMessage, AssistantPart, CompatibilityMode, Diagnostic, HostStreamModel,
    HostStreamTransport, Message, ModelCatalog, ModelId, OutputFormat, OutputModalities, Pricing,
    ReasoningMode, ResponseStream, StopReason, TokenRate, ToolChoice, UserMessage, UserPart,
};
use std::sync::Mutex;

fn model() -> Model {
    let mut model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("claude-sonnet-4-6".into()))
        .unwrap();
    let spec = Arc::make_mut(&mut model.spec);
    spec.limits.context_window = 2_000_000;
    spec.pricing = Some(Pricing {
        input: TokenRate(3_000_000),
        output: TokenRate(15_000_000),
        cache_read: TokenRate(300_000),
        cache_write_5m: TokenRate(3_750_000),
        cache_write_1h: Some(TokenRate(6_000_000)),
        reasoning: None,
        tiers: vec![],
    });
    spec.cache.prompt_cache.short = Some(300);
    spec.cache.prompt_cache.long = Some(3600);
    model
}

fn request(session: &Session) -> Request {
    Request {
        system: Some("original system".into()),
        messages: session.context().unwrap(),
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: Some(4096),
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: CacheRetention::Short,
        session_id: Some("original-session".into()),
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    session: Session,
    model: Model,
    request: Request,
    warmer: CacheWarmer,
    client: AiClient,
    captures: Arc<Mutex<Vec<Request>>>,
}

struct CaptureTransport {
    captures: Arc<Mutex<Vec<Request>>>,
    pending: bool,
    fail: bool,
}

#[async_trait::async_trait]
impl HostStreamTransport for CaptureTransport {
    async fn stream(
        &self,
        model: HostStreamModel,
        request: Request,
        _: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        self.captures.lock().unwrap().push(request);
        if self.pending {
            return Ok(Box::pin(futures_util::stream::pending()));
        }
        if self.fail {
            return Err(AiError::Canceled);
        }
        Ok(Box::pin(futures_util::stream::iter([
            Ok(StreamEvent::Started { response_id: None }),
            Ok(StreamEvent::Finished(Response {
                message: AssistantMessage {
                    content: vec![AssistantPart::Text("discard this generated output".into())],
                    model: model.id,
                    protocol: model.protocol,
                },
                stop_reason: StopReason::MaxTokens,
                usage: Usage {
                    input_tokens: 0,
                    cache_read_tokens: 500_000,
                    output_tokens: 1,
                    total_tokens: 500_001,
                    ..Usage::default()
                },
                cost: Some(octet_ai::Cost {
                    total: 150_015,
                    ..octet_ai::Cost::default()
                }),
                response_id: None,
                responses_output: None,
                deferred: None,
                inference: None,
                diagnostics: vec![],
            })),
        ])))
    }
}

impl Fixture {
    fn new(mode: CacheWarmMode, tokens: u64) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let model = model();
        let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
        let anchor = session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text("original question".into())],
            })))
            .unwrap();
        let request = request(&session);
        let assistant = session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("original answer".into())],
                model: model.spec.id.clone(),
                protocol: model.spec.protocol,
            })))
            .unwrap();
        session
            .record_assistant_usage_with_stop_reason(
                assistant,
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                Usage {
                    cache_write_tokens: tokens,
                    total_tokens: tokens,
                    ..Usage::default()
                },
                Some(octet_ai::Cost::default()),
                StopReason::EndTurn,
            )
            .unwrap();
        let captures = Arc::new(Mutex::new(vec![]));
        let client = AiClient::new();
        client.register_host_stream_transport(
            model.endpoint.id.clone(),
            Arc::new(CaptureTransport {
                captures: captures.clone(),
                pending: false,
                fail: false,
            }),
        );
        let mut warmer = CacheWarmer::default();
        warmer.set_mode(mode, &mut session).unwrap();
        warmer
            .start(&model, &request, Some(anchor), tokens, 0, &mut session)
            .unwrap();
        Self {
            _dir: dir,
            session,
            model,
            request,
            warmer,
            client,
            captures,
        }
    }

    async fn step(
        &mut self,
        hooks: &[Arc<dyn CacheWarmingDecisionHook>],
        limits: CacheWarmLimits,
    ) -> Option<AgentEvent> {
        let step = self.warmer.next_step().await;
        self.warmer
            .advance(
                step,
                CacheWarmHost {
                    session: &mut self.session,
                    client: &self.client,
                    hooks,
                    resource_owner: "test-owner",
                    tool_generation: 0,
                    limits,
                },
            )
            .unwrap()
    }

    async fn refresh(&mut self) -> Option<AgentEvent> {
        assert!(self.step(&[], CacheWarmLimits::default()).await.is_none());
        assert!(self.step(&[], CacheWarmLimits::default()).await.is_none());
        self.step(&[], CacheWarmLimits::default()).await
    }
}

#[test]
fn pi_delay_and_default_profile() {
    assert_eq!(CacheWarmMode::default(), CacheWarmMode::Streaming);
    for (ttl, delay) in [
        (300_000, Some(270_000)),
        (60_000, Some(50_000)),
        (10_000, None),
    ] {
        assert_eq!(
            cache_warming_delay(Duration::from_millis(ttl)).map(|d| d.as_millis()),
            delay
        );
    }
}

#[tokio::test(start_paused = true)]
async fn exact_replay_is_accounted_once_and_never_changes_conversation() {
    let mut f = Fixture::new(CacheWarmMode::Idle, 500_000);
    f.warmer.on_agent_settled(&mut f.session).unwrap();
    let head = f.session.head();
    let context = serde_json::to_value(f.session.context().unwrap()).unwrap();
    let decision = f.warmer.status(&f.session, 0).decision.unwrap();
    assert_eq!(decision.continuation_probability, 0.15);
    assert_eq!(decision.warm_cost_microdollars, 150_015);
    assert_eq!(decision.miss_cost_microdollars, 1_725_000);
    assert_eq!(decision.expected_savings_microdollars, 108_735);
    tokio::time::advance(Duration::from_secs(270)).await;
    let outcome = f.refresh().await;
    assert!(
        matches!(
            outcome,
            Some(AgentEvent::CacheWarmed {
                extension_override: false,
                ..
            })
        ),
        "outcome={outcome:?}; records={:?}; captures={}",
        f.session.cache_warm_records(),
        f.captures.lock().unwrap().len()
    );
    let captures = f.captures.lock().unwrap();
    assert_eq!(captures.len(), 1);
    let mut expected = f.request.clone();
    expected.max_output_tokens = Some(1);
    assert_eq!(
        serde_json::to_value(&captures[0]).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    drop(captures);
    assert_eq!(f.session.head(), head);
    assert_eq!(
        serde_json::to_value(f.session.context().unwrap()).unwrap(),
        context
    );
    assert_eq!(f.session.usage_records().len(), 2);
    assert!(matches!(
        f.session.usage_records()[1].kind,
        UsageRecordKind::CacheWarm
    ));
    assert_eq!(f.session.total_cost_microdollars(), 150_015);
    assert_eq!(
        crate::cache::analyze_session_cache_stats(&f.session).assistant_turns,
        1
    );
    assert!(!f.session.has_uncertain_usage());
    let reopened = Session::open(f.session.path()).unwrap();
    assert_eq!(reopened.total_cost_microdollars(), 150_015);
    assert_eq!(reopened.cache_warm_records().len(), 2);
    assert!(!reopened.has_uncertain_usage());
}

#[tokio::test(start_paused = true)]
async fn streaming_stops_when_settled_and_idle_recomputes_probability() {
    let mut f = Fixture::new(CacheWarmMode::Streaming, 50_000);
    assert_eq!(
        f.warmer.status(&f.session, 0).decision.unwrap().action,
        CacheWarmingAction::Warm
    );
    f.warmer.on_agent_settled(&mut f.session).unwrap();
    assert!(f.warmer.active.is_none());
    let mut f = Fixture::new(CacheWarmMode::Idle, 50_000);
    f.warmer.on_agent_settled(&mut f.session).unwrap();
    assert_eq!(
        f.warmer.status(&f.session, 0).decision.unwrap().action,
        CacheWarmingAction::Stop
    );
    tokio::time::advance(Duration::from_secs(270)).await;
    f.step(&[], CacheWarmLimits::default()).await;
    f.step(&[], CacheWarmLimits::default()).await;
    assert!(f.captures.lock().unwrap().is_empty());
    assert_eq!(
        f.warmer.inactive.reason.as_deref(),
        Some("expected savings below threshold")
    );
}

#[tokio::test(start_paused = true)]
async fn off_unknown_lifetime_disabled_cache_and_budget_thinking_make_no_call() {
    let mut f = Fixture::new(CacheWarmMode::Off, 500_000);
    assert!(f.warmer.active.is_none());
    for variant in 0..3 {
        let mut model = f.model.clone();
        let mut request = f.request.clone();
        match variant {
            0 => Arc::make_mut(&mut model.spec).cache.prompt_cache.short = None,
            1 => request.cache_retention = CacheRetention::None,
            _ => {
                request.reasoning = ReasoningConfig::Budget(1024);
                Arc::make_mut(&mut model.spec).preset.anthropic_compat = None;
            }
        }
        f.warmer
            .set_mode(CacheWarmMode::Idle, &mut f.session)
            .unwrap();
        f.warmer
            .start(
                &model,
                &request,
                f.session.head(),
                500_000,
                0,
                &mut f.session,
            )
            .unwrap();
        assert!(f.warmer.active.is_none());
    }
    assert!(f.captures.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn adaptive_thinking_request_can_be_replayed_without_budget_changes() {
    let mut f = Fixture::new(CacheWarmMode::Streaming, 500_000);
    let mut request = f.request.clone();
    request.reasoning = ReasoningConfig::Effort(octet_ai::ReasoningEffort::High);
    let mut model = f.model.clone();
    Arc::make_mut(&mut model.spec)
        .preset
        .anthropic_compat
        .get_or_insert_with(Default::default)
        .force_adaptive_thinking = Some(true);
    f.warmer
        .start(
            &model,
            &request,
            f.session.head(),
            500_000,
            0,
            &mut f.session,
        )
        .unwrap();
    tokio::time::advance(Duration::from_secs(270)).await;
    f.refresh().await;
    assert_eq!(f.captures.lock().unwrap()[0].reasoning, request.reasoning);
}

struct Override(CacheWarmingAction);
#[async_trait::async_trait]
impl CacheWarmingDecisionHook for Override {
    async fn cache_warming_decision(
        &self,
        _: &CacheWarmingDecisionContext,
    ) -> Option<CacheWarmingAction> {
        Some(self.0)
    }
}

#[tokio::test(start_paused = true)]
async fn last_hook_wins_and_can_force_unknown_economics_but_not_ceiling() {
    let mut f = Fixture::new(CacheWarmMode::Idle, 0);
    let hooks: Vec<Arc<dyn CacheWarmingDecisionHook>> = vec![
        Arc::new(Override(CacheWarmingAction::Stop)),
        Arc::new(Override(CacheWarmingAction::Warm)),
    ];
    tokio::time::advance(Duration::from_secs(270)).await;
    f.step(&hooks, CacheWarmLimits::default()).await;
    f.step(&hooks, CacheWarmLimits::default()).await;
    assert!(matches!(
        f.step(&hooks, CacheWarmLimits::default()).await,
        Some(AgentEvent::CacheWarmed {
            extension_override: true,
            ..
        })
    ));
    let mut f = Fixture::new(CacheWarmMode::Idle, 500_000);
    tokio::time::advance(Duration::from_secs(270)).await;
    let limits = CacheWarmLimits {
        max_session_tokens: Some(500_000),
        ..CacheWarmLimits::default()
    };
    f.step(&hooks, limits).await;
    f.step(&hooks, limits).await;
    assert!(f.captures.lock().unwrap().is_empty());
    assert!(f.session.cache_warm_records().is_empty());
}

#[tokio::test(start_paused = true)]
async fn pending_hook_keeps_its_original_decision_when_agent_settles() {
    struct Delayed;
    #[async_trait::async_trait]
    impl CacheWarmingDecisionHook for Delayed {
        async fn cache_warming_decision(
            &self,
            context: &CacheWarmingDecisionContext,
        ) -> Option<CacheWarmingAction> {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Some(context.decision.action)
        }
    }
    let mut f = Fixture::new(CacheWarmMode::Idle, 30_000);
    tokio::time::advance(Duration::from_secs(270)).await;
    f.step(&[Arc::new(Delayed)], CacheWarmLimits::default())
        .await;
    assert_eq!(
        f.warmer
            .active
            .as_ref()
            .unwrap()
            .decision
            .as_ref()
            .unwrap()
            .action,
        CacheWarmingAction::Warm
    );
    assert!(f.warmer.next_step().now_or_never().is_none());
    f.warmer.on_agent_settled(&mut f.session).unwrap();
    assert_eq!(
        evaluate(f.warmer.active.as_ref().unwrap(), &f.session).action,
        CacheWarmingAction::Stop
    );
    f.step(&[], CacheWarmLimits::default()).await;
    assert!(matches!(
        f.step(&[], CacheWarmLimits::default()).await,
        Some(AgentEvent::CacheWarmed {
            extension_override: false,
            ..
        })
    ));
    let record = f.session.cache_warm_records().last().unwrap();
    assert_eq!(record.state, CacheWarmState::Completed);
    assert_eq!(record.anchor, f.warmer.active.as_ref().unwrap().anchor);
    assert!(!record.extension_override);
}

#[tokio::test(start_paused = true)]
async fn real_inflight_reservation_is_not_spent_twice() {
    for cost_limit in [false, true] {
        let mut f = Fixture::new(CacheWarmMode::Streaming, 500_000);
        tokio::time::advance(Duration::from_secs(270)).await;
        let limits = if cost_limit {
            CacheWarmLimits {
                max_session_cost_microdollars: Some(3_100_000),
                pending_request: Some(UsageUncertaintyBound {
                    tokens: 10,
                    cost_microdollars: Some(3_000_000),
                }),
                ..Default::default()
            }
        } else {
            CacheWarmLimits {
                max_session_tokens: Some(1_100_000),
                pending_request: Some(UsageUncertaintyBound {
                    tokens: 100_000,
                    cost_microdollars: Some(0),
                }),
                ..Default::default()
            }
        };
        f.step(&[], limits).await;
        f.step(&[], limits).await;
        assert!(f.captures.lock().unwrap().is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn late_timer_and_late_hook_do_not_rebuild_expired_cache() {
    let mut f = Fixture::new(CacheWarmMode::Streaming, 500_000);
    tokio::time::advance(Duration::from_secs(286)).await;
    f.step(&[], CacheWarmLimits::default()).await;
    assert_eq!(
        f.warmer.inactive.reason.as_deref(),
        Some("cache refresh deadline missed")
    );
    assert!(f.captures.lock().unwrap().is_empty());
    struct Slow;
    #[async_trait::async_trait]
    impl CacheWarmingDecisionHook for Slow {
        async fn cache_warming_decision(
            &self,
            _: &CacheWarmingDecisionContext,
        ) -> Option<CacheWarmingAction> {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Some(CacheWarmingAction::Warm)
        }
    }
    let mut f = Fixture::new(CacheWarmMode::Streaming, 500_000);
    tokio::time::advance(Duration::from_secs(285)).await;
    f.step(&[Arc::new(Slow)], CacheWarmLimits::default()).await;
    // At the final admissible instant the hook has no remaining margin. Its
    // timeout response cannot grant a post-expiry dispatch.
    tokio::time::advance(Duration::from_millis(1)).await;
    f.step(&[], CacheWarmLimits::default()).await;
    assert!(f.captures.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn refreshes_do_not_extend_age_and_long_retention_uses_its_own_ttl() {
    let mut f = Fixture::new(CacheWarmMode::Streaming, 500_000);
    let mut count = 0;
    while f.warmer.active.is_some() {
        tokio::time::advance(Duration::from_secs(270)).await;
        f.refresh().await;
        count += 1;
    }
    assert!(count > 6);
    assert_eq!(
        f.warmer.inactive.reason.as_deref(),
        Some("one-hour safety limit reached")
    );
    let mut f = Fixture::new(CacheWarmMode::Idle, 500_000);
    let mut request = f.request.clone();
    request.cache_retention = CacheRetention::Long;
    f.warmer
        .start(
            &f.model,
            &request,
            f.session.head(),
            500_000,
            0,
            &mut f.session,
        )
        .unwrap();
    assert_eq!(
        f.warmer.active.as_ref().unwrap().delay,
        Duration::from_secs(3240)
    );
    f.warmer.on_agent_settled(&mut f.session).unwrap();
    assert_eq!(
        f.warmer.inactive.reason.as_deref(),
        Some("30-minute idle safety limit reached")
    );
}

#[tokio::test(start_paused = true)]
async fn idle_horizon_branch_tool_generation_and_mode_changes_stop_warming() {
    let mut f = Fixture::new(CacheWarmMode::Idle, 500_000);
    f.warmer.on_agent_settled(&mut f.session).unwrap();
    while f.warmer.active.is_some() {
        tokio::time::advance(Duration::from_secs(270)).await;
        f.refresh().await;
    }
    assert_eq!(
        f.warmer.inactive.reason.as_deref(),
        Some("30-minute idle safety limit reached")
    );
    let mut f = Fixture::new(CacheWarmMode::Idle, 500_000);
    f.session.checkout_root().unwrap();
    assert!(!f.warmer.valid(&mut f.session, 0).unwrap());
    let mut f = Fixture::new(CacheWarmMode::Idle, 500_000);
    assert!(!f.warmer.valid(&mut f.session, 1).unwrap());
    let mut f = Fixture::new(CacheWarmMode::Idle, 500_000);
    f.warmer.on_agent_settled(&mut f.session).unwrap();
    f.warmer
        .set_mode(CacheWarmMode::Streaming, &mut f.session)
        .unwrap();
    assert!(f.warmer.active.is_none());
}

#[tokio::test(start_paused = true)]
async fn dropping_idle_poll_keeps_post_but_explicit_cancel_records_bounded_uncertainty() {
    let mut f = Fixture::new(CacheWarmMode::Idle, 500_000);
    f.client.register_host_stream_transport(
        f.model.endpoint.id.clone(),
        Arc::new(CaptureTransport {
            captures: f.captures.clone(),
            pending: true,
            fail: false,
        }),
    );
    tokio::time::advance(Duration::from_secs(270)).await;
    f.step(&[], CacheWarmLimits::default()).await;
    f.step(&[], CacheWarmLimits::default()).await;
    assert!(f.warmer.next_step().now_or_never().is_none());
    assert!(f.warmer.next_step().now_or_never().is_none());
    assert_eq!(f.captures.lock().unwrap().len(), 1);
    f.warmer.cancel(&mut f.session, "new prompt").unwrap();
    assert!(f.session.has_uncertain_usage());
    assert_eq!(
        f.session.usage_uncertainty_exposure().unwrap().tokens,
        500_001
    );
    assert_eq!(
        f.session.cache_warm_records().last().unwrap().state,
        CacheWarmState::Failed
    );
    let reopened = Session::open(f.session.path()).unwrap();
    assert_eq!(
        reopened.usage_uncertainty_exposure().unwrap().tokens,
        500_001
    );
}

#[tokio::test(start_paused = true)]
async fn failed_and_timed_out_attempts_never_invent_zero_usage_or_retry_immediately() {
    for pending in [false, true] {
        let mut f = Fixture::new(CacheWarmMode::Idle, 500_000);
        f.client.register_host_stream_transport(
            f.model.endpoint.id.clone(),
            Arc::new(CaptureTransport {
                captures: f.captures.clone(),
                pending,
                fail: !pending,
            }),
        );
        tokio::time::advance(Duration::from_secs(270)).await;
        assert!(matches!(
            f.refresh().await,
            Some(AgentEvent::ProviderUsageUncertain)
        ));
        assert_eq!(f.captures.lock().unwrap().len(), 1);
        assert_eq!(f.session.usage_records().len(), 1);
        assert!(f.session.has_uncertain_usage());
        assert_eq!(
            f.session.cache_warm_records().last().unwrap().state,
            if pending {
                CacheWarmState::TimedOut
            } else {
                CacheWarmState::Failed
            }
        );
        assert!(f.warmer.active.as_ref().unwrap().next_at > Instant::now());
    }
}

#[tokio::test(start_paused = true)]
async fn admission_does_not_dispatch_after_consumer_suspension_expires_cache() {
    for suspended in [Duration::from_secs(16), Duration::from_secs(3600)] {
        let mut f = Fixture::new(CacheWarmMode::Streaming, 500_000);
        tokio::time::advance(Duration::from_secs(270)).await;
        f.step(&[], CacheWarmLimits::default()).await;
        f.step(&[], CacheWarmLimits::default()).await;
        assert!(matches!(f.warmer.work, Work::Refreshing(_)));
        assert!(f.captures.lock().unwrap().is_empty());
        tokio::time::advance(suspended).await;
        f.step(&[], CacheWarmLimits::default()).await;
        assert!(f.captures.lock().unwrap().is_empty());
        assert!(f.warmer.active.is_none());
        assert!(!f.session.has_uncertain_usage());
        assert_eq!(
            f.session.cache_warm_records().last().unwrap().state,
            CacheWarmState::Failed
        );
    }
}

#[tokio::test(start_paused = true)]
async fn declared_ttl_cannot_enable_a_route_without_a_one_token_wire_cap() {
    let mut f = Fixture::new(CacheWarmMode::Streaming, 500_000);
    let mut model = f.model.clone();
    let spec = Arc::make_mut(&mut model.spec);
    spec.protocol = Protocol::OpenAiResponses;
    spec.preset.supports_max_output_tokens = Some(false);
    f.warmer
        .start(
            &model,
            &f.request,
            f.session.head(),
            500_000,
            0,
            &mut f.session,
        )
        .unwrap();
    assert!(f.warmer.active.is_none());
    tokio::time::advance(Duration::from_secs(600)).await;
    assert!(f.warmer.next_step().now_or_never().is_none());
    assert!(f.captures.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn unfinished_attempt_remains_uncertain_after_reopen() {
    let mut f = Fixture::new(CacheWarmMode::Idle, 500_000);
    f.session
        .record_cache_warm_status(CacheWarmRecord {
            attempt: 1,
            endpoint: f.model.endpoint.id.clone(),
            model: f.model.spec.id.clone(),
            state: CacheWarmState::Started,
            at_unix_ms: now_unix_millis(),
            anchor: f.session.head(),
            extension_override: false,
        })
        .unwrap();
    let mut reopened = Session::open(f.session.path()).unwrap();
    assert!(reopened.has_uncertain_usage());
    let mut resumed = CacheWarmer::default();
    resumed
        .start(
            &f.model,
            &f.request,
            reopened.head(),
            500_000,
            0,
            &mut reopened,
        )
        .unwrap();
    assert_eq!(
        resumed.inactive.reason.as_deref(),
        Some("previous cache refresh remains unresolved")
    );
    tokio::time::advance(Duration::from_secs(600)).await;
    assert!(resumed.next_step().now_or_never().is_none());
    assert!(reopened.has_uncertain_usage());
    assert!(f.captures.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn watched_modes_cancel_pending_hook_without_reviving_the_old_prefix() {
    struct PendingHook;
    #[async_trait::async_trait]
    impl CacheWarmingDecisionHook for PendingHook {
        async fn cache_warming_decision(
            &self,
            _: &CacheWarmingDecisionContext,
        ) -> Option<CacheWarmingAction> {
            std::future::pending().await
        }
    }
    let mut f = Fixture::new(CacheWarmMode::Streaming, 500_000);
    let diagnostics = f.warmer.diagnostics();
    let control = f.warmer.mode_control();
    assert_eq!(diagnostics.borrow().state, CacheWarmingState::Scheduled);
    f.step(&[Arc::new(PendingHook)], CacheWarmLimits::default())
        .await;
    assert!(f.warmer.next_step().now_or_never().is_none());
    assert_eq!(diagnostics.borrow().state, CacheWarmingState::Refreshing);
    control.send_modify(|policy| policy.set_mode(CacheWarmMode::Off));
    control.send_modify(|policy| policy.set_mode(CacheWarmMode::Idle));
    assert!(f.step(&[], CacheWarmLimits::default()).await.is_none());
    assert_eq!(f.warmer.mode(), CacheWarmMode::Idle);
    assert!(f.warmer.active.is_none());
    assert!(matches!(&f.warmer.work, Work::Waiting));
    assert_eq!(diagnostics.borrow().state, CacheWarmingState::Inactive);
    assert!(f.warmer.next_step().now_or_never().is_none());
    assert!(f.captures.lock().unwrap().is_empty());
    assert!(f.session.cache_warm_records().is_empty());
}

#[tokio::test(start_paused = true)]
async fn watched_modes_wake_inactive_and_reconcile_before_settlement() {
    let mut f = Fixture::new(CacheWarmMode::Off, 500_000);
    let control = f.warmer.mode_control();
    control.send_modify(|policy| policy.set_mode(CacheWarmMode::Idle));
    assert!(f.step(&[], CacheWarmLimits::default()).await.is_none());
    assert!(f.warmer.active.is_none());
    assert!(f.warmer.next_step().now_or_never().is_none());

    let mut f = Fixture::new(CacheWarmMode::Streaming, 500_000);
    let control = f.warmer.mode_control();
    control.send_modify(|policy| policy.set_mode(CacheWarmMode::Off));
    control.send_modify(|policy| policy.set_mode(CacheWarmMode::Idle));
    // A ready real-provider event can outrank the warming watch arm.
    f.warmer.on_agent_settled(&mut f.session).unwrap();
    assert!(f.warmer.active.is_none());
    assert_eq!(
        f.warmer.diagnostics().borrow().state,
        CacheWarmingState::Inactive
    );
    assert!(f.warmer.next_step().now_or_never().is_none());
}

#[tokio::test(start_paused = true)]
async fn watched_streaming_cutoff_cancels_only_an_already_idle_prefix() {
    for settled in [false, true] {
        let mut f = Fixture::new(CacheWarmMode::Idle, 500_000);
        if settled {
            f.warmer.on_agent_settled(&mut f.session).unwrap();
        }
        let control = f.warmer.mode_control();
        control.send_modify(|policy| policy.set_mode(CacheWarmMode::Streaming));
        control.send_modify(|policy| policy.set_mode(CacheWarmMode::Idle));
        assert!(f.step(&[], CacheWarmLimits::default()).await.is_none());
        assert_eq!(f.warmer.active.is_none(), settled);
        if !settled {
            f.warmer.on_agent_settled(&mut f.session).unwrap();
            assert_eq!(
                f.warmer.active.as_ref().unwrap().phase,
                CacheWarmingPhase::Idle
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn watched_off_cancels_dispatched_refresh_and_publishes_uncertainty() {
    let mut f = Fixture::new(CacheWarmMode::Streaming, 500_000);
    f.client.register_host_stream_transport(
        f.model.endpoint.id.clone(),
        Arc::new(CaptureTransport {
            captures: f.captures.clone(),
            pending: true,
            fail: false,
        }),
    );
    let diagnostics = f.warmer.diagnostics();
    f.step(&[], CacheWarmLimits::default()).await;
    f.step(&[], CacheWarmLimits::default()).await;
    assert!(f.warmer.next_step().now_or_never().is_none());
    assert_eq!(f.captures.lock().unwrap().len(), 1);
    f.warmer
        .mode_control()
        .send_modify(|policy| policy.set_mode(CacheWarmMode::Off));
    assert!(matches!(
        f.step(&[], CacheWarmLimits::default()).await,
        Some(AgentEvent::ProviderUsageUncertain)
    ));
    assert_eq!(diagnostics.borrow().state, CacheWarmingState::Inactive);
    assert_eq!(f.session.usage_uncertainty_records().len(), 1);
    assert!(f.session.usage_uncertainty_exposure().is_some());
    assert_eq!(
        f.session.cache_warm_records().last().unwrap().state,
        CacheWarmState::Failed
    );
    assert!(f.warmer.active.is_none());
}
