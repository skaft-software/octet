use super::*;
use crate::tool_inference::ResolvedToolInferenceModel;
use futures_util::FutureExt;
use octet_ai::{
    Diagnostic, HostStreamModel, HostStreamTransport, ModelCatalog, ModelId, Response,
    ResponseStream,
};
use std::sync::atomic::AtomicUsize;

#[test]
fn inference_steps_do_not_inline_large_requests_or_responses() {
    assert!(std::mem::size_of::<InferenceStep>() <= 3 * std::mem::size_of::<usize>());
}

#[derive(Clone)]
enum Behavior {
    Finish,
    CancelAtFinish(CancellationToken),
    Pending,
    Fail,
    Overflow,
    ExcessBytes,
    ExcessEvents,
    ExcessTerminal,
}

struct Script {
    behavior: Behavior,
    calls: AtomicUsize,
    requests: Mutex<Vec<Request>>,
}

#[async_trait::async_trait]
impl HostStreamTransport for Script {
    async fn stream(
        &self,
        model: HostStreamModel,
        request: Request,
        _: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request);
        match &self.behavior {
            Behavior::Pending => Ok(Box::pin(futures_util::stream::pending())),
            Behavior::Fail => Err(AiError::Canceled),
            Behavior::ExcessBytes | Behavior::ExcessEvents => {
                let (events, bytes) = if matches!(&self.behavior, Behavior::ExcessBytes) {
                    (1025, 1024)
                } else {
                    (MAX_EVENTS + 1, 0)
                };
                Ok(Box::pin(futures_util::stream::iter(
                    [
                        Ok(StreamEvent::Started { response_id: None }),
                        Ok(StreamEvent::TextStart { index: 0 }),
                    ]
                    .into_iter()
                    .chain((0..events).map(move |_| {
                        Ok(StreamEvent::TextDelta {
                            index: 0,
                            delta: "x".repeat(bytes),
                        })
                    }))
                    .chain([
                        Ok(StreamEvent::TextEnd { index: 0 }),
                        Ok(StreamEvent::Finished(response(model))),
                    ]),
                )))
            }
            Behavior::ExcessTerminal => {
                let mut result = response(model);
                result.message.content = vec![AssistantPart::Text("x".repeat(MAX_OUTPUT_BYTES))];
                Ok(Box::pin(futures_util::stream::iter([
                    Ok(StreamEvent::Started { response_id: None }),
                    Ok(StreamEvent::Finished(result)),
                ])))
            }
            Behavior::Overflow => Ok(Box::pin(futures_util::stream::iter([
                Ok(StreamEvent::Started { response_id: None }),
                Ok(StreamEvent::TextStart { index: 0 }),
                Ok(StreamEvent::TextDelta {
                    index: 0,
                    delta: "never dropped silently".into(),
                }),
            ]))),
            behavior => {
                let cancel = match behavior {
                    Behavior::CancelAtFinish(cancel) => Some(cancel.clone()),
                    _ => None,
                };
                Ok(Box::pin(
                    futures_util::stream::iter([
                        Ok(StreamEvent::Started { response_id: None }),
                        Ok(StreamEvent::Finished(response(model))),
                    ])
                    .map(move |event| {
                        if matches!(&event, Ok(StreamEvent::Finished(_))) {
                            if let Some(cancel) = &cancel {
                                cancel.cancel();
                            }
                        }
                        event
                    }),
                ))
            }
        }
    }
}

fn response(model: HostStreamModel) -> Response {
    Response {
        message: AssistantMessage {
            content: vec![AssistantPart::Text("auxiliary".into())],
            model: model.id,
            protocol: model.protocol,
        },
        stop_reason: StopReason::EndTurn,
        usage: Usage {
            input_tokens: 11,
            output_tokens: 3,
            total_tokens: 14,
            ..Default::default()
        },
        cost: Some(Cost {
            total: 17,
            total_picodollars_remainder: 123,
            ..Default::default()
        }),
        response_id: None,
        responses_output: None,
        deferred: None,
        inference: None,
        diagnostics: vec![],
    }
}

struct ConsultationTurn(AtomicUsize);

#[async_trait::async_trait]
impl HostStreamTransport for ConsultationTurn {
    async fn stream(
        &self,
        model: HostStreamModel,
        _: Request,
        _: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        let mut result = response(model);
        result.message.content = vec![AssistantPart::Text("primary complete".into())];
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            result.message.content = vec![AssistantPart::ToolCall(ToolCall {
                id: octet_ai::ToolCallId("consultation".into()),
                name: "consult".into(),
                arguments_json: "{}".into(),
                argument_error: None,
                async_execution: false,
            })];
            result.stop_reason = StopReason::ToolUse;
        }
        Ok(Box::pin(futures_util::stream::iter([
            Ok(StreamEvent::Started { response_id: None }),
            Ok(StreamEvent::Finished(result)),
        ])))
    }
}

struct Consult;

#[async_trait::async_trait]
impl Tool for Consult {
    fn definition(&self) -> ToolDef {
        ToolDef {
            name: "consult".into(),
            description: "Consult the configured model".into(),
            parameters: serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
            async_execution: false,
            constrained_sampling: None,
        }
    }

    fn effect(&self, _: &serde_json::Value, _: &ToolContext<'_>) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Pure)
    }

    async fn execute(
        &self,
        _: serde_json::Value,
        context: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        let result = context
            .progress
            .inference_service()
            .unwrap()
            .stream(Fixture::request(), context.cancellation.clone(), None)
            .await?;
        assert_eq!(result.usage.total_tokens, 14);
        Ok(ToolOutput::new("consultation complete"))
    }
}

struct Resolver {
    model: Model,
    script: Arc<Script>,
}

impl ToolInferenceModelResolver for Resolver {
    fn resolve(
        &self,
        provider: &str,
        model: &str,
    ) -> Result<ResolvedToolInferenceModel, ToolError> {
        if provider != "fixture" || model != "configured" {
            return Err(refused("not configured"));
        }
        Ok(ResolvedToolInferenceModel {
            model: self.model.clone(),
            fetch: Some(self.script.clone()),
        })
    }
}

struct Fixture {
    directory: tempfile::TempDir,
    session: Session,
    driver: InferenceDriver,
    scope: InferenceScope,
    parent: CancellationToken,
    script: Arc<Script>,
}

impl Fixture {
    fn new(behavior: Behavior) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let session = Session::create(directory.path().join("session.jsonl")).unwrap();
        let model = ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("claude-sonnet-4-6".into()))
            .unwrap();
        let script = Arc::new(Script {
            behavior,
            calls: AtomicUsize::new(0),
            requests: Mutex::new(vec![]),
        });
        let resolver = Arc::new(Resolver {
            model,
            script: script.clone(),
        });
        let parent = CancellationToken::default();
        let (driver, scope) = InferenceDriver::new(
            session.resource_owner_key(),
            "outer".into(),
            "root-session".into(),
            resolver,
            AiClient::new(),
            parent.clone(),
            InferenceLimits {
                max_output_tokens: 128,
                ..Default::default()
            },
        );
        Self {
            directory,
            session,
            driver,
            scope,
            parent,
            script,
        }
    }

    fn request() -> ToolInferenceRequest {
        ToolInferenceRequest {
            provider: "fixture".into(),
            model: "configured".into(),
            request: Request {
                system: Some("isolated request".into()),
                messages: vec![Message::User(UserMessage {
                    content: vec![UserPart::Text("test".into())],
                })],
                tools: vec![],
                tool_choice: ToolChoice::Auto,
                max_output_tokens: Some(64),
                temperature: None,
                stop: vec![],
                reasoning: ReasoningConfig::Off,
                reasoning_mode: ReasoningMode::Standard,
                responses: None,
                output_format: OutputFormat::Text,
                output_modalities: OutputModalities::Text,
                compatibility: CompatibilityMode::Strict,
                cache_retention: CacheRetention::None,
                session_id: None,
            },
        }
    }

    fn submit(
        &self,
        request: ToolInferenceRequest,
        cancel: CancellationToken,
        updates: Option<mpsc::Sender<StreamEvent>>,
    ) -> tokio::task::JoinHandle<Result<Response, ToolError>> {
        let service = self.scope.service.clone();
        tokio::spawn(async move { service.stream(request, cancel, updates).await })
    }

    async fn admit(&mut self) {
        let step = self.driver.next_step().await;
        assert!(self
            .driver
            .advance(step, &mut self.session)
            .unwrap()
            .is_none());
    }

    async fn finish(&mut self) -> InferenceSettlement {
        let step = self.driver.next_step().await;
        self.driver
            .advance(step, &mut self.session)
            .unwrap()
            .unwrap()
    }
}

#[tokio::test]
async fn owning_agent_settles_inference_and_reports_it_after_the_last_model_turn() {
    for turns in [1, 2] {
        owning_agent_accounting(turns).await;
    }
}

async fn owning_agent_accounting(turns: u64) {
    let directory = tempfile::tempdir().unwrap();
    let session = Session::create(directory.path().join("owning-agent.jsonl")).unwrap();
    let catalog = ModelCatalog::builtin().unwrap();
    let model = catalog.resolve(&ModelId("gpt-4o-mini".into())).unwrap();
    let selected = model.spec.id.clone();
    let script = Arc::new(Script {
        behavior: Behavior::Finish,
        calls: AtomicUsize::new(0),
        requests: Mutex::new(vec![]),
    });
    let client = AiClient::new();
    client.register_host_stream_transport(
        model.endpoint.id.clone(),
        Arc::new(ConsultationTurn(AtomicUsize::new(0))),
    );
    let mut extensions = ExtensionHost::new();
    extensions.tool(Consult);
    let mut agent = Agent::new(AgentConfig {
        client,
        model,
        session,
        system: "system".into(),
        sandbox: SandboxConfig::new(directory.path()),
        effect_broker: EffectBroker::default(),
        extensions,
        max_turns: Some(turns),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    agent.set_tool_inference_resolver(Some(Arc::new(Resolver {
        model: catalog
            .resolve(&ModelId("claude-sonnet-4-6".into()))
            .unwrap(),
        script: script.clone(),
    })));
    let output = tokio::time::timeout(Duration::from_secs(10), agent.complete("consult"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(script.calls.load(Ordering::SeqCst), 1);
    assert_eq!(agent.model().spec.id, selected);
    assert_eq!(agent.session().usage_records().len(), turns as usize + 1);
    assert_eq!(agent.session().total_cost_microdollars(), 17 * (turns + 1));
    assert!(!agent.session().has_uncertain_usage());
    assert!(!serde_json::to_string(&*agent.session().context().unwrap())
        .unwrap()
        .contains("auxiliary"));
    assert_eq!(output.usage.total_tokens, 14 * (turns + 1));
    assert_eq!(output.cost_microdollars, 17 * (turns + 1));
}

#[tokio::test]
async fn completion_is_accounted_before_delivery_without_history_or_tool_usage() {
    let mut f = Fixture::new(Behavior::Finish);
    let task = f.submit(Fixture::request(), CancellationToken::default(), None);
    f.admit().await;
    assert_eq!(f.script.calls.load(Ordering::SeqCst), 0);
    assert!(f.session.has_uncertain_usage());
    assert!(!task.is_finished());
    let settled = f.finish().await;
    assert_eq!(settled.usage.total_tokens, 14);
    assert_eq!(settled.cost.unwrap().total, 17);
    assert!(!settled.uncertain);
    assert_eq!(task.await.unwrap().unwrap().usage.total_tokens, 14);
    assert_eq!(f.session.usage_records().len(), 1);
    assert_eq!(f.session.total_cost_microdollars(), 17);
    assert!(f.session.context().unwrap().is_empty());
    assert!(!f.session.has_uncertain_usage());
    let requests = f.script.requests.lock().unwrap();
    assert_eq!(
        requests[0].session_id.as_deref(),
        Some("root-session:tool-inference:1")
    );
    assert_eq!(requests[0].cache_retention, CacheRetention::Short);
}

#[tokio::test]
async fn a_successful_consultation_does_not_cancel_the_callers_token() {
    let mut f = Fixture::new(Behavior::Finish);
    let cancel = CancellationToken::default();
    let task = f.submit(Fixture::request(), cancel.clone(), None);
    f.admit().await;
    let settled = f.finish().await;
    assert!(settled.reported);
    task.await.unwrap().unwrap();
    assert!(!cancel.is_cancelled());
    assert!(!f.parent.is_cancelled());
}

#[tokio::test]
async fn cancelled_in_the_terminal_poll_keeps_exact_billing() {
    let cancel = CancellationToken::default();
    let mut f = Fixture::new(Behavior::CancelAtFinish(cancel.clone()));
    let task = f.submit(Fixture::request(), cancel, None);
    f.admit().await;
    let settled = f.finish().await;
    assert_eq!(settled.usage.total_tokens, 14);
    assert!(task.await.unwrap().is_err());
    assert_eq!(f.session.usage_records().len(), 1);
    assert!(!f.session.has_uncertain_usage());
}

#[tokio::test]
async fn parent_cancel_before_provider_poll_has_no_exposure() {
    let mut f = Fixture::new(Behavior::Pending);
    let task = f.submit(Fixture::request(), CancellationToken::default(), None);
    f.admit().await;
    f.parent.cancel();
    let settled = f.finish().await;
    assert!(!settled.uncertain);
    assert!(task.await.unwrap().is_err());
    assert_eq!(f.script.calls.load(Ordering::SeqCst), 0);
    assert!(!f.session.has_uncertain_usage());
}

#[tokio::test]
async fn parent_cancel_after_dispatch_records_unknown_usage_once() {
    let mut f = Fixture::new(Behavior::Pending);
    let task = f.submit(Fixture::request(), CancellationToken::default(), None);
    f.admit().await;
    assert!(f.driver.next_step().now_or_never().is_none());
    assert_eq!(f.script.calls.load(Ordering::SeqCst), 1);
    f.parent.cancel();
    assert!(f.finish().await.uncertain);
    assert!(task.await.unwrap().is_err());
    assert_eq!(f.session.usage_uncertainty_records().len(), 1);
    assert!(f.session.usage_uncertainty_exposure().is_none());
    assert!(f.driver.cancel(&mut f.session).unwrap().is_none());
    assert_eq!(f.session.usage_uncertainty_records().len(), 1);
}

#[tokio::test]
async fn dropped_caller_cancels_the_flight() {
    let mut f = Fixture::new(Behavior::Pending);
    let task = f.submit(Fixture::request(), CancellationToken::default(), None);
    f.admit().await;
    assert!(f.driver.next_step().now_or_never().is_none());
    task.abort();
    let _ = task.await;
    assert!(f.finish().await.uncertain);
    assert_eq!(f.script.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn deadline_includes_the_entire_request_and_never_retries() {
    let mut f = Fixture::new(Behavior::Pending);
    let task = f.submit(Fixture::request(), CancellationToken::default(), None);
    f.admit().await;
    assert!(f.driver.next_step().now_or_never().is_none());
    f.driver.active.as_mut().unwrap().deadline = Instant::now();
    assert!(f.finish().await.uncertain);
    assert!(task.await.unwrap().is_err());
    assert_eq!(f.script.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_provider_does_not_retry() {
    let mut f = Fixture::new(Behavior::Fail);
    let task = f.submit(Fixture::request(), CancellationToken::default(), None);
    f.admit().await;
    assert!(f.finish().await.uncertain);
    assert!(task.await.unwrap().is_err());
    assert_eq!(f.script.calls.load(Ordering::SeqCst), 1);
    assert!(f.session.usage_records().is_empty());
}

#[tokio::test]
async fn full_updates_are_terminal_not_silently_dropped() {
    let mut f = Fixture::new(Behavior::Overflow);
    let (updates, _receiver) = mpsc::channel(1);
    let task = f.submit(
        Fixture::request(),
        CancellationToken::default(),
        Some(updates),
    );
    f.admit().await;
    assert!(f.finish().await.uncertain);
    assert!(task.await.unwrap().is_err());
}

#[tokio::test]
async fn cumulative_bytes_and_events_are_bounded_without_an_update_consumer() {
    for behavior in [Behavior::ExcessBytes, Behavior::ExcessEvents] {
        let mut f = Fixture::new(behavior);
        let task = f.submit(Fixture::request(), CancellationToken::default(), None);
        f.admit().await;
        assert!(f.finish().await.uncertain);
        assert!(task.await.unwrap().is_err());
        assert_eq!(f.script.calls.load(Ordering::SeqCst), 1);
        assert!(f.session.usage_records().is_empty());
    }
}

#[tokio::test]
async fn oversized_terminal_response_is_accounted_but_not_delivered() {
    let mut f = Fixture::new(Behavior::ExcessTerminal);
    let task = f.submit(Fixture::request(), CancellationToken::default(), None);
    f.admit().await;
    let settled = f.finish().await;
    assert_eq!(settled.usage.total_tokens, 14);
    assert!(!settled.uncertain);
    assert!(task.await.unwrap().is_err());
    assert_eq!(f.session.usage_records().len(), 1);
    assert!(!f.session.has_uncertain_usage());
}

#[tokio::test]
async fn inference_capability_is_not_inherited_by_nested_tools() {
    let f = Fixture::new(Behavior::Finish);
    let sink = ToolProgressSink::null().with_inference(f.scope.service.clone());
    assert!(sink.inference_service().is_some());
    assert!(sink.for_nested_call().inference_service().is_none());
}

#[tokio::test]
async fn second_request_is_refused_without_waiting_for_first() {
    let mut f = Fixture::new(Behavior::Pending);
    let first = f.submit(Fixture::request(), CancellationToken::default(), None);
    f.admit().await;
    let second = f.submit(Fixture::request(), CancellationToken::default(), None);
    assert!(second.await.unwrap().is_err());
    f.driver.cancel(&mut f.session).unwrap();
    assert!(first.await.unwrap().is_err());
    assert_eq!(f.script.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn finite_budget_uses_existing_enforceability_refusal_before_dispatch() {
    for (tokens, cost) in [(Some(1000), None), (None, Some(1000))] {
        let mut f = Fixture::new(Behavior::Finish);
        f.driver.limits.tokens = tokens;
        f.driver.limits.cost = cost;
        let task = f.submit(Fixture::request(), CancellationToken::default(), None);
        f.admit().await;
        assert!(task.await.unwrap().is_err());
        assert_eq!(f.script.calls.load(Ordering::SeqCst), 0);
        assert!(!f.session.has_uncertain_usage());
        assert!(f.session.usage_records().is_empty());
    }
}

#[tokio::test]
async fn invalid_boundaries_never_dispatch() {
    let mut unknown = Fixture::request();
    unknown.model = "not-configured".into();
    let mut replay = Fixture::request();
    replay.request.session_id = Some("forged-owner".into());
    let mut oversized = Fixture::request();
    oversized.request.system = Some("x".repeat(MAX_REQUEST_BYTES));
    for request in [unknown, replay, oversized] {
        let mut f = Fixture::new(Behavior::Finish);
        let task = f.submit(request, CancellationToken::default(), None);
        f.admit().await;
        assert!(task.await.unwrap().is_err());
        assert_eq!(f.script.calls.load(Ordering::SeqCst), 0);
        assert!(!f.session.has_uncertain_usage());
    }
}

#[tokio::test]
async fn dropping_driver_leaves_durable_uncertainty_across_reopen() {
    let mut f = Fixture::new(Behavior::Pending);
    let task = f.submit(Fixture::request(), CancellationToken::default(), None);
    f.admit().await;
    assert!(f.driver.next_step().now_or_never().is_none());
    let path = f.directory.path().join("session.jsonl");
    drop(f.driver);
    assert!(task.await.unwrap().is_err());
    drop(f.session);
    let reopened = Session::open(path).unwrap();
    assert!(reopened.has_uncertain_usage());
    assert!(reopened.usage_uncertainty_exposure().is_none());
    assert!(reopened.usage_records().is_empty());
}

#[tokio::test]
async fn failed_synced_usage_commit_cannot_deliver_an_unaccounted_success() {
    let mut f = Fixture::new(Behavior::Finish);
    let task = f.submit(Fixture::request(), CancellationToken::default(), None);
    f.admit().await;
    let path = f.directory.path().join("session.jsonl");
    let mut external = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(external, "{{\"foreign_writer\":true}}").unwrap();
    let step = f.driver.next_step().await;
    assert!(matches!(
        f.driver.advance(step, &mut f.session),
        Err(SessionError::ConcurrentModification)
    ));
    assert!(!task.is_finished());
    assert!(f.session.has_uncertain_usage());
    assert!(f.session.usage_records().is_empty());
    drop(f.driver);
    assert!(task.await.unwrap().is_err());
}

#[tokio::test]
async fn known_usage_reopen_does_not_reintroduce_pending_exposure() {
    let mut f = Fixture::new(Behavior::Finish);
    let task = f.submit(Fixture::request(), CancellationToken::default(), None);
    f.admit().await;
    f.finish().await;
    task.await.unwrap().unwrap();
    let path = f.directory.path().join("session.jsonl");
    drop(f.session);
    let reopened = Session::open(path).unwrap();
    assert!(!reopened.has_uncertain_usage());
    assert_eq!(reopened.usage_records().len(), 1);
    assert_eq!(reopened.total_cost_microdollars(), 17);
}
