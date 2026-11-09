use super::*;
use crate::extension::ProviderContextProjection;
use std::sync::atomic::AtomicUsize;

async fn project_with_fresh_session(
    request: Request,
    hooks: &[Arc<dyn ProviderContextHook>],
    context: &ProviderContextProjectionContext,
    model: &Model,
    abort: &AbortFlag,
) -> Result<Request, AgentError> {
    let workspace = tempfile::tempdir().unwrap();
    let mut session = Session::create(workspace.path().join("projection.jsonl")).unwrap();
    let mut notices = Vec::new();
    super::project_provider_context(
        request,
        hooks,
        context,
        model,
        abort,
        &mut session,
        &mut notices,
    )
    .await
}

fn model() -> Model {
    octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap()
}
fn projected_user(text: &str) -> Message {
    Message::User(UserMessage {
        content: vec![UserPart::Text(text.into())],
    })
}
fn request() -> Request {
    Request {
        system: Some("canonical system".into()),
        messages: vec![projected_user("canonical user")],
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        max_output_tokens: Some(4096),
        temperature: None,
        stop: Vec::new(),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: CacheRetention::Short,
        session_id: Some("provider affinity".into()),
    }
}
fn identity() -> ProviderContextProjectionContext {
    ProviderContextProjectionContext {
        resource_owner: "owner".into(),
        session_id: "host-session".into(),
        head: None,
        tool_generation: 9,
    }
}

struct Rewrite {
    system: Option<String>,
    text: String,
    observations: Arc<Mutex<Vec<(Request, ProviderContextProjectionContext)>>>,
}
#[async_trait::async_trait]
impl ProviderContextHook for Rewrite {
    async fn project_context(
        &self,
        request: &Request,
        context: &ProviderContextProjectionContext,
    ) -> Result<Option<ProviderContextProjection>, String> {
        self.observations
            .lock()
            .unwrap()
            .push((request.clone(), context.clone()));
        Ok(Some(ProviderContextProjection {
            system: self.system.clone(),
            tools: None,
            messages: vec![projected_user(&self.text)],
        }))
    }
}
/// A recoverable hook whose projection the provider cannot represent: the
/// messages carry inline audio, which the fixture model cannot accept. The tool
/// set is untouched, so the authority/loadout checks pass and only the provider
/// validator refuses. A 116-tool Pi fleet must not be able to end the session
/// with this, so the projection is dropped and the run continues.
struct InvalidProjection {
    failure_notice_id: &'static str,
}
#[async_trait::async_trait]
impl ProviderContextHook for InvalidProjection {
    fn continues_on_failure(&self) -> bool {
        true
    }
    fn failure_notice_id(&self) -> Option<&str> {
        Some(self.failure_notice_id)
    }
    async fn project_context(
        &self,
        _request: &Request,
        _context: &ProviderContextProjectionContext,
    ) -> Result<Option<ProviderContextProjection>, String> {
        let audio = Message::User(UserMessage {
            content: vec![UserPart::Media(Media::audio_bytes(
                bytes::Bytes::from("not-a-real-wav"),
                octet_ai::AudioFormat::Wav,
            ))],
        });
        Ok(Some(ProviderContextProjection {
            system: None,
            tools: None,
            messages: vec![audio],
        }))
    }
}


/// A projection that produces an unrepresentable request must not kill the run
/// (the 116-tool Pi-fleet failure), and the notice must name the validator
/// reason once. Authority/identity refusals stay fatal and have their own tests.
#[tokio::test]
async fn invalid_projection_is_skipped_with_the_validator_reason() {
    let hooks: Vec<Arc<dyn ProviderContextHook>> =
        vec![Arc::new(InvalidProjection { failure_notice_id: "pi-bridge" })];
    let mut notices = Vec::new();
    let workspace = tempfile::tempdir().unwrap();
    let mut session = Session::create(workspace.path().join("invalid-projection.jsonl")).unwrap();
    let mut context = identity();
    context.session_id = "invalid-projection".into();
    let canonical = request();
    let result = super::project_provider_context(
        canonical.clone(),
        &hooks,
        &context,
        &model(),
        &AbortFlag::default(),
        &mut session,
        &mut notices,
    )
    .await
    .expect("an invalid projection must not end the run");
    let messages = serialize_conversation(&result.messages);
    assert!(messages.contains("canonical user"), "{messages}");
    assert!(
        !messages.contains("audio"),
        "unrepresentable media reached the provider: {messages}"
    );
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].contains("context projection was invalid and was skipped"),
        "{}",
        notices[0]
    );
    let mut more = Vec::new();
    super::project_provider_context(
        canonical,
        &hooks,
        &context,
        &model(),
        &AbortFlag::default(),
        &mut session,
        &mut more,
    )
    .await
    .expect("second preparation also survives");
    assert!(more.is_empty(), "notice reported once per session: {more:?}");
}

struct Capture(Arc<Mutex<Vec<Request>>>);
#[async_trait::async_trait]
impl octet_ai::HostStreamTransport for Capture {
    async fn stream(
        &self,
        model: octet_ai::HostStreamModel,
        request: Request,
        _: Vec<octet_ai::Diagnostic>,
    ) -> Result<octet_ai::ResponseStream, AiError> {
        self.0.lock().unwrap().push(request);
        Ok(Box::pin(futures_util::stream::iter(vec![
            Ok(StreamEvent::Started { response_id: None }),
            Ok(StreamEvent::TextStart { index: 0 }),
            Ok(StreamEvent::TextDelta {
                index: 0,
                delta: "answer".into(),
            }),
            Ok(StreamEvent::TextEnd { index: 0 }),
            Ok(StreamEvent::Finished(octet_ai::Response {
                message: AssistantMessage {
                    content: vec![AssistantPart::Text("answer".into())],
                    model: model.id,
                    protocol: model.protocol,
                },
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                cost: Some(Cost::default()),
                response_id: None,
                responses_output: None,
                deferred: None,
                inference: None,
                diagnostics: Vec::new(),
            })),
        ])))
    }
}
fn agent(host: ExtensionHost) -> (Agent, Arc<Mutex<Vec<Request>>>, tempfile::TempDir) {
    let workspace = tempfile::tempdir().unwrap();
    let mut model = model();
    // This is a context-planning limit only, never trusted input-cap metadata.
    Arc::make_mut(&mut model.spec).limits.context_window = 20_000;
    Arc::make_mut(&mut model.spec).limits.max_output_tokens = 4096;
    let captures = Arc::new(Mutex::new(Vec::new()));
    let client = AiClient::new();
    client.register_host_stream_transport(
        model.endpoint.id.clone(),
        Arc::new(Capture(captures.clone())),
    );
    let mut agent = Agent::new(AgentConfig {
        model,
        client,
        session: Session::create(workspace.path().join("session.jsonl")).unwrap(),
        extensions: host,
        system: "canonical system".into(),
        sandbox: SandboxConfig::new(workspace.path()),
        effect_broker: EffectBroker::default(),
        max_turns: Some(1),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: Some("explicit-host-session".into()),
    })
    .unwrap();
    agent
        .set_compaction_token_mode(AgentCompactionMode::Disabled, 0.9, 2)
        .unwrap();
    (agent, captures, workspace)
}

#[tokio::test]
async fn ordered_effective_projection_precedes_planning_and_preserves_canonical_session() {
    let observations = Arc::new(Mutex::new(Vec::new()));
    let mut host = ExtensionHost::new();
    host.provider_context_hook(Rewrite {
        system: Some("first".into()),
        text: "first messages".into(),
        observations: observations.clone(),
    });
    host.provider_context_hook(Rewrite {
        system: None,
        text: "final messages".into(),
        observations: observations.clone(),
    });
    let (mut agent, captures, _workspace) = agent(host);
    let owner = agent.resource_owner.clone();
    let original = "large canonical history ".repeat(5000);
    let output = agent.complete(original.clone()).await.unwrap();
    assert_eq!(output.text, "answer");
    let observations = observations.lock().unwrap();
    assert_eq!(observations.len(), 2);
    assert_eq!(
        observations[0].0.system.as_deref(),
        Some("canonical system")
    );
    assert_eq!(observations[1].0.system.as_deref(), Some("first"));
    assert!(serialize_conversation(&observations[1].0.messages).contains("first messages"));
    let context = &observations[0].1;
    assert_eq!(context, &observations[1].1);
    assert_eq!(context.resource_owner, owner);
    assert_eq!(context.session_id, "explicit-host-session");
    assert_ne!(context.session_id, "provider affinity");
    assert_eq!(context.tool_generation, agent.extensions.tool_snapshot().0);
    assert_eq!(context.head, Some(agent.session.entries()[0].id.clone()));
    let sent = captures.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].system, None);
    assert!(serialize_conversation(&sent[0].messages).contains("final messages"));
    assert!(!serialize_conversation(&sent[0].messages).contains(&original));
    assert_eq!(sent[0].max_output_tokens, Some(4096));
    assert_eq!(sent[0].session_id.as_deref(), Some("explicit-host-session"));
    assert!(serialize_conversation(&agent.session.context().unwrap()).contains(&original));
}

#[tokio::test]
async fn effective_growth_and_finite_ceiling_denials_never_dispatch_or_warm() {
    for finite in [false, true] {
        let observations = Arc::new(Mutex::new(Vec::new()));
        let mut host = ExtensionHost::new();
        host.provider_context_hook(Rewrite {
            system: None,
            text: if finite {
                "small projection".into()
            } else {
                "large projection".repeat(10_000)
            },
            observations: observations.clone(),
        });
        let (mut agent, captures, _workspace) = agent(host);
        if finite {
            agent.set_max_session_tokens(Some(u64::MAX));
        }
        let error = agent.complete("small canonical request").await.unwrap_err();
        assert!(
            if finite {
                matches!(error, AgentError::InputLimitUnavailable)
            } else {
                matches!(error, AgentError::ContextExceeded { .. })
            },
            "{error:?}"
        );
        assert_eq!(observations.lock().unwrap().len(), 1);
        assert!(captures.lock().unwrap().is_empty());
        assert!(agent.session.usage_records().is_empty());
        assert!(agent.session.cache_warm_records().is_empty());
        assert!(!agent.session.has_uncertain_usage());
    }
}

struct Refuse {
    calls: Arc<AtomicUsize>,
    wait: bool,
    cancel: Option<CancellationToken>,
}
#[async_trait::async_trait]
impl ProviderContextHook for Refuse {
    async fn project_context(
        &self,
        _: &Request,
        _: &ProviderContextProjectionContext,
    ) -> Result<Option<ProviderContextProjection>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        if self.wait {
            std::future::pending().await
        } else {
            Err("secret-bearing hook detail".into())
        }
    }
}
struct ContinueRefuse(Refuse);
#[async_trait::async_trait]
impl ProviderContextHook for ContinueRefuse {
    fn continues_on_failure(&self) -> bool {
        true
    }
    fn failure_notice_id(&self) -> Option<&str> {
        Some("test-extension")
    }
    async fn project_context(
        &self,
        request: &Request,
        context: &ProviderContextProjectionContext,
    ) -> Result<Option<ProviderContextProjection>, String> {
        self.0.project_context(request, context).await
    }
}

#[tokio::test(start_paused = true)]
async fn extension_hook_error_and_deadline_continue_but_same_poll_cancellation_aborts() {
    for case in ["error", "deadline", "cancel"] {
        let calls = Arc::new(AtomicUsize::new(0));
        let cancellation = CancellationToken::default();
        let abort = AbortFlag {
            cancellation: cancellation.clone(),
            ..AbortFlag::default()
        };
        let mut hooks: Vec<Arc<dyn ProviderContextHook>> = vec![Arc::new(ContinueRefuse(Refuse {
            calls: calls.clone(),
            wait: case == "deadline",
            cancel: (case == "cancel").then_some(cancellation),
        }))];
        if case != "deadline" {
            hooks.push(Arc::new(Rewrite {
                system: None,
                text: "later hook ran".into(),
                observations: Arc::new(Mutex::new(Vec::new())),
            }));
        }
        let mut notices = Vec::new();
        let workspace = tempfile::tempdir().unwrap();
        let mut session = Session::create(workspace.path().join("hook-failure.jsonl")).unwrap();
        let mut context = identity();
        context.session_id = format!("hook-error-{case}");
        let result = super::project_provider_context(
            request(),
            &hooks,
            &context,
            &model(),
            &abort,
            &mut session,
            &mut notices,
        )
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(if case == "cancel" {
            matches!(result, Err(AgentError::Cancelled)) && notices.is_empty()
        } else {
            result.is_ok() && notices.len() == 1 && notices[0].contains("context hook failed")
        });
        if let Ok(result) = result {
            let messages = serialize_conversation(&result.messages);
            assert!(messages.contains(if case == "deadline" {
                "canonical user"
            } else {
                "later hook ran"
            }));
        }
        assert!(notices
            .iter()
            .all(|notice| !notice.contains("secret-bearing")));
    }
}

#[tokio::test]
async fn extension_hook_failure_keeps_canonical_request_and_reports_once_per_extension_session() {
    let calls = Arc::new(AtomicUsize::new(0));
    let hook: Arc<dyn ProviderContextHook> = Arc::new(ContinueRefuse(Refuse {
        calls,
        wait: false,
        cancel: None,
    }));
    let mut context = identity();
    context.session_id = "dedupe-session-w7".into();
    let workspace = tempfile::tempdir().unwrap();
    let mut session = Session::create(workspace.path().join("unused.jsonl")).unwrap();
    let canonical = request();
    let mut first_notices = Vec::new();
    let first = super::project_provider_context(
        canonical.clone(),
        std::slice::from_ref(&hook),
        &context,
        &model(),
        &AbortFlag::default(),
        &mut session,
        &mut first_notices,
    )
    .await
    .unwrap();
    assert_eq!(
        serialize_conversation(&first.messages),
        serialize_conversation(&canonical.messages)
    );
    assert_eq!(first.system, canonical.system);
    assert_eq!(first_notices.len(), 1);
    let mut second_notices = Vec::new();
    let second = super::project_provider_context(
        canonical.clone(),
        &[hook],
        &context,
        &model(),
        &AbortFlag::default(),
        &mut session,
        &mut second_notices,
    )
    .await
    .unwrap();
    assert_eq!(
        serialize_conversation(&second.messages),
        serialize_conversation(&canonical.messages)
    );
    assert!(second_notices.is_empty(), "notice repeated in one session");
    // A reopened session has its own bounded, non-durable notice lifetime.
    let path = session.path().to_owned();
    drop(session);
    let mut reopened = Session::open(path).unwrap();
    let mut reopened_notices = Vec::new();
    let hook: Arc<dyn ProviderContextHook> = Arc::new(ContinueRefuse(Refuse {
        calls: Arc::new(AtomicUsize::new(0)),
        wait: false,
        cancel: None,
    }));
    super::project_provider_context(
        canonical,
        &[hook],
        &context,
        &model(),
        &AbortFlag::default(),
        &mut reopened,
        &mut reopened_notices,
    )
    .await
    .unwrap();
    assert_eq!(reopened_notices.len(), 1);
}

struct NamedFailure(String);
#[async_trait::async_trait]
impl ProviderContextHook for NamedFailure {
    fn continues_on_failure(&self) -> bool {
        true
    }
    fn failure_notice_id(&self) -> Option<&str> {
        Some(&self.0)
    }
    async fn project_context(
        &self,
        _: &Request,
        _: &ProviderContextProjectionContext,
    ) -> Result<Option<ProviderContextProjection>, String> {
        Err("private error".into())
    }
}
#[tokio::test]
async fn session_notice_ledger_is_capped_pruned_and_does_not_hide_overflow() {
    let workspace = tempfile::tempdir().unwrap();
    let mut session = Session::create(workspace.path().join("notices.jsonl")).unwrap();
    let hooks: Vec<Arc<dyn ProviderContextHook>> = (0..=MAX_CONTEXT_FAILURE_NOTICES)
        .map(|n| Arc::new(NamedFailure(format!("extension-{n}"))) as Arc<dyn ProviderContextHook>)
        .collect();
    let mut notices = Vec::new();
    super::project_provider_context(
        request(),
        &hooks,
        &identity(),
        &model(),
        &AbortFlag::default(),
        &mut session,
        &mut notices,
    )
    .await
    .unwrap();
    assert_eq!(
        session.context_failure_notices.len(),
        MAX_CONTEXT_FAILURE_NOTICES
    );
    assert_eq!(notices.len(), MAX_CONTEXT_FAILURE_NOTICES + 1);
    notices.clear();
    super::project_provider_context(
        request(),
        &hooks,
        &identity(),
        &model(),
        &AbortFlag::default(),
        &mut session,
        &mut notices,
    )
    .await
    .unwrap();
    assert_eq!(notices.len(), 1, "untracked overflow must remain visible");
    notices.clear();
    super::project_provider_context(
        request(),
        &hooks[MAX_CONTEXT_FAILURE_NOTICES..],
        &identity(),
        &model(),
        &AbortFlag::default(),
        &mut session,
        &mut notices,
    )
    .await
    .unwrap();
    assert_eq!(session.context_failure_notices.len(), 1);
    assert_eq!(notices.len(), 1);
    assert!(notices.iter().all(|n| !n.contains("private error")));
}

struct SimulatedProcessTimeout {
    revoked: Arc<AtomicUsize>,
}
impl Drop for SimulatedProcessTimeout {
    fn drop(&mut self) {
        self.revoked.fetch_add(1, Ordering::SeqCst);
    }
}
impl crate::ProviderContextSessionWait for SimulatedProcessTimeout {
    fn projection_future(
        &mut self,
        _: &Request,
        _: &ProviderContextProjectionContext,
    ) -> Option<
        futures_util::future::BoxFuture<
            'static,
            Result<Option<ProviderContextProjection>, ProviderContextHookError>,
        >,
    > {
        Some(Box::pin(async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            Err(ProviderContextHookError::Failed(
                "simulated process request timeout".into(),
            ))
        }))
    }
    fn ready(&self) -> Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        Box::pin(std::future::pending())
    }
    fn consume_next(
        &mut self,
        _: &mut Session,
        _: &ProviderContextProjectionContext,
    ) -> Result<(), String> {
        unreachable!()
    }
}
struct SimulatedProcessTimeoutHook(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl ProviderContextHook for SimulatedProcessTimeoutHook {
    fn continues_on_failure(&self) -> bool {
        true
    }
    fn begin_session_wait(
        &self,
        _: &Session,
        _: &ProviderContextProjectionContext,
    ) -> Result<Option<Box<dyn crate::ProviderContextSessionWait>>, String> {
        Ok(Some(Box::new(SimulatedProcessTimeout {
            revoked: self.0.clone(),
        })))
    }
    async fn project_context(
        &self,
        _: &Request,
        _: &ProviderContextProjectionContext,
    ) -> Result<Option<ProviderContextProjection>, String> {
        unreachable!()
    }
}
struct SimulatedSuccessfulExtension(Rewrite);
#[async_trait::async_trait]
impl ProviderContextHook for SimulatedSuccessfulExtension {
    fn continues_on_failure(&self) -> bool {
        true
    }
    async fn project_context(
        &self,
        request: &Request,
        context: &ProviderContextProjectionContext,
    ) -> Result<Option<ProviderContextProjection>, String> {
        self.0.project_context(request, context).await
    }
}
struct AcceptingHostGuard(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl ProviderContextHook for AcceptingHostGuard {
    async fn project_context(
        &self,
        request: &Request,
        _: &ProviderContextProjectionContext,
    ) -> Result<Option<ProviderContextProjection>, String> {
        assert!(serialize_conversation(&request.messages).contains("later extension"));
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
}
#[tokio::test(start_paused = true)]
async fn process_request_timeout_before_aggregate_deadline_allows_later_extension_and_host_guard() {
    let workspace = tempfile::tempdir().unwrap();
    let mut session = Session::create(workspace.path().join("timeout-success.jsonl")).unwrap();
    let mut notices = Vec::new();
    let revoked = Arc::new(AtomicUsize::new(0));
    let guarded = Arc::new(AtomicUsize::new(0));
    let hooks: Vec<Arc<dyn ProviderContextHook>> = vec![
        Arc::new(SimulatedProcessTimeoutHook(revoked.clone())),
        Arc::new(SimulatedSuccessfulExtension(Rewrite {
            system: None,
            text: "later extension".into(),
            observations: Arc::new(Mutex::new(Vec::new())),
        })),
        Arc::new(AcceptingHostGuard(guarded.clone())),
    ];
    let start = tokio::time::Instant::now();
    let result = super::project_provider_context(
        request(),
        &hooks,
        &identity(),
        &model(),
        &AbortFlag::default(),
        &mut session,
        &mut notices,
    )
    .await
    .unwrap();
    assert!(serialize_conversation(&result.messages).contains("later extension"));
    assert_eq!(guarded.load(Ordering::SeqCst), 1);
    assert_eq!(revoked.load(Ordering::SeqCst), 1);
    assert_eq!(notices.len(), 1);
    assert_eq!(tokio::time::Instant::now() - start, Duration::from_secs(1));
}

struct RemoteCancelAtDeadline;
impl crate::ProviderContextSessionWait for RemoteCancelAtDeadline {
    fn projection_future(
        &mut self,
        _: &Request,
        _: &ProviderContextProjectionContext,
    ) -> Option<
        futures_util::future::BoxFuture<
            'static,
            Result<Option<ProviderContextProjection>, ProviderContextHookError>,
        >,
    > {
        Some(Box::pin(async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Err(ProviderContextHookError::Cancelled)
        }))
    }
    fn ready(&self) -> Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        Box::pin(std::future::pending())
    }
    fn consume_next(
        &mut self,
        _: &mut Session,
        _: &ProviderContextProjectionContext,
    ) -> Result<(), String> {
        unreachable!()
    }
}
#[async_trait::async_trait]
impl ProviderContextHook for RemoteCancelAtDeadline {
    fn continues_on_failure(&self) -> bool {
        true
    }
    fn begin_session_wait(
        &self,
        _: &Session,
        _: &ProviderContextProjectionContext,
    ) -> Result<Option<Box<dyn crate::ProviderContextSessionWait>>, String> {
        Ok(Some(Box::new(RemoteCancelAtDeadline)))
    }
    async fn project_context(
        &self,
        _: &Request,
        _: &ProviderContextProjectionContext,
    ) -> Result<Option<ProviderContextProjection>, String> {
        unreachable!()
    }
}
#[tokio::test(start_paused = true)]
async fn remote_cancellation_wins_same_poll_as_deadline() {
    let workspace = tempfile::tempdir().unwrap();
    let mut session = Session::create(workspace.path().join("remote-cancel.jsonl")).unwrap();
    let mut notices = Vec::new();
    let hook: Arc<dyn ProviderContextHook> = Arc::new(RemoteCancelAtDeadline);
    assert!(matches!(
        super::project_provider_context(
            request(),
            &[hook],
            &identity(),
            &model(),
            &AbortFlag::default(),
            &mut session,
            &mut notices
        )
        .await,
        Err(AgentError::Cancelled)
    ));
    assert!(notices.is_empty());
}

#[tokio::test(start_paused = true)]
async fn exhausted_aggregate_deadline_cannot_skip_a_later_host_guard() {
    let workspace = tempfile::tempdir().unwrap();
    let mut session = Session::create(workspace.path().join("deadline-guard.jsonl")).unwrap();
    let mut notices = Vec::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let hooks: Vec<Arc<dyn ProviderContextHook>> = vec![
        Arc::new(ContinueRefuse(Refuse {
            calls: calls.clone(),
            wait: true,
            cancel: None,
        })),
        Arc::new(Refuse {
            calls: calls.clone(),
            wait: false,
            cancel: None,
        }),
    ];
    let start = tokio::time::Instant::now();
    assert!(matches!(
        super::project_provider_context(
            request(),
            &hooks,
            &identity(),
            &model(),
            &AbortFlag::default(),
            &mut session,
            &mut notices,
        )
        .await,
        Err(AgentError::ProviderContextPreparation(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2, "later guard was polled");
    assert_eq!(notices.len(), 1);
    assert_eq!(tokio::time::Instant::now() - start, Duration::from_secs(5));
}

struct DeadlineSuccess {
    extension: bool,
    calls: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl ProviderContextHook for DeadlineSuccess {
    fn continues_on_failure(&self) -> bool {
        self.extension
    }
    async fn project_context(
        &self,
        _: &Request,
        _: &ProviderContextProjectionContext,
    ) -> Result<Option<ProviderContextProjection>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
}
#[tokio::test(start_paused = true)]
async fn aggregate_extension_timeout_skips_later_extensions_but_runs_successful_guard() {
    let workspace = tempfile::tempdir().unwrap();
    let mut session = Session::create(workspace.path().join("deadline-success.jsonl")).unwrap();
    let mut notices = Vec::new();
    let extension_calls = Arc::new(AtomicUsize::new(0));
    let guard_calls = Arc::new(AtomicUsize::new(0));
    let hooks: Vec<Arc<dyn ProviderContextHook>> = vec![
        Arc::new(ContinueRefuse(Refuse {
            calls: Arc::new(AtomicUsize::new(0)),
            wait: true,
            cancel: None,
        })),
        Arc::new(DeadlineSuccess {
            extension: true,
            calls: extension_calls.clone(),
        }),
        Arc::new(DeadlineSuccess {
            extension: false,
            calls: guard_calls.clone(),
        }),
    ];
    let canonical = request();
    let start = tokio::time::Instant::now();
    let result = super::project_provider_context(
        canonical.clone(),
        &hooks,
        &identity(),
        &model(),
        &AbortFlag::default(),
        &mut session,
        &mut notices,
    )
    .await
    .unwrap();
    assert_eq!(
        serialize_conversation(&result.messages),
        serialize_conversation(&canonical.messages)
    );
    assert_eq!(result.system, canonical.system);
    assert_eq!(
        extension_calls.load(Ordering::SeqCst),
        0,
        "no new extension authority is activated after the budget expires"
    );
    assert_eq!(
        guard_calls.load(Ordering::SeqCst),
        1,
        "the authority guard must run despite the timeout"
    );
    assert_eq!(notices.len(), 2);
    assert_eq!(tokio::time::Instant::now() - start, Duration::from_secs(5));
}

struct LateProjection;
#[async_trait::async_trait]
impl ProviderContextHook for LateProjection {
    fn continues_on_failure(&self) -> bool {
        true
    }
    async fn project_context(
        &self,
        _: &Request,
        _: &ProviderContextProjectionContext,
    ) -> Result<Option<ProviderContextProjection>, String> {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok(Some(ProviderContextProjection {
            system: Some("late system".into()),
            messages: vec![projected_user("late context")],
            tools: None,
        }))
    }
}

#[tokio::test(start_paused = true)]
async fn extension_success_at_deadline_is_discarded_before_guard_and_dispatch() {
    let workspace = tempfile::tempdir().unwrap();
    let mut session = Session::create(workspace.path().join("late-projection.jsonl")).unwrap();
    let mut notices = Vec::new();
    let guarded = Arc::new(AtomicUsize::new(0));
    let hooks: Vec<Arc<dyn ProviderContextHook>> = vec![
        Arc::new(LateProjection),
        Arc::new(DeadlineSuccess {
            extension: false,
            calls: guarded.clone(),
        }),
    ];
    let canonical = request();
    let result = super::project_provider_context(
        canonical.clone(),
        &hooks,
        &identity(),
        &model(),
        &AbortFlag::default(),
        &mut session,
        &mut notices,
    )
    .await
    .unwrap();
    assert_eq!(
        serialize_conversation(&result.messages),
        serialize_conversation(&canonical.messages)
    );
    assert_eq!(result.system, canonical.system);
    assert_eq!(guarded.load(Ordering::SeqCst), 1);
    assert_eq!(notices.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn host_guard_timeout_after_extension_timeout_still_fails_closed() {
    let workspace = tempfile::tempdir().unwrap();
    let mut session = Session::create(workspace.path().join("late-guard.jsonl")).unwrap();
    let mut notices = Vec::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let hooks: Vec<Arc<dyn ProviderContextHook>> = vec![
        Arc::new(ContinueRefuse(Refuse {
            calls: calls.clone(),
            wait: true,
            cancel: None,
        })),
        Arc::new(Refuse {
            calls: calls.clone(),
            wait: true,
            cancel: None,
        }),
    ];
    let start = tokio::time::Instant::now();
    assert!(matches!(
        super::project_provider_context(
            request(),
            &hooks,
            &identity(),
            &model(),
            &AbortFlag::default(),
            &mut session,
            &mut notices,
        )
        .await,
        Err(AgentError::ProviderContextPreparation(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(notices.len(), 1);
    assert_eq!(tokio::time::Instant::now() - start, Duration::from_secs(10));
}

#[tokio::test]
async fn host_owned_context_hook_still_fails_closed_by_default() {
    let workspace = tempfile::tempdir().unwrap();
    let mut session = Session::create(workspace.path().join("host-guard.jsonl")).unwrap();
    let mut notices = Vec::new();
    let hook: Arc<dyn ProviderContextHook> = Arc::new(Refuse {
        calls: Arc::new(AtomicUsize::new(0)),
        wait: false,
        cancel: None,
    });
    assert!(matches!(
        super::project_provider_context(
            request(),
            &[hook],
            &identity(),
            &model(),
            &AbortFlag::default(),
            &mut session,
            &mut notices,
        )
        .await,
        Err(AgentError::ProviderContextPreparation(_))
    ));
    assert!(notices.is_empty());
}

#[tokio::test]
async fn context_refusal_describes_the_original_request_after_a_projection() {
    let workspace = tempfile::tempdir().unwrap();
    let mut session = Session::create(workspace.path().join("descriptor.jsonl")).unwrap();
    let mut notices = Vec::new();
    let canonical = request();
    let descriptor = super::request_descriptor(&canonical);
    let hooks: Vec<Arc<dyn ProviderContextHook>> = vec![
        Arc::new(Rewrite {
            system: Some("projected system".into()),
            text: "projected content".repeat(100),
            observations: Arc::new(Mutex::new(Vec::new())),
        }),
        Arc::new(Refuse {
            calls: Arc::new(AtomicUsize::new(0)),
            wait: false,
            cancel: None,
        }),
    ];
    let error = super::project_provider_context(
        canonical,
        &hooks,
        &identity(),
        &model(),
        &AbortFlag::default(),
        &mut session,
        &mut notices,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, AgentError::ProviderContextPreparation(reason) if reason.ends_with(&format!("; request {descriptor}")))
    );
}

// Optional local microbenchmark; no timing assertion because hosts vary.
#[tokio::test]
#[ignore = "performance probe"]
async fn context_hook_preparation_performance_probe() {
    let workspace = tempfile::tempdir().unwrap();
    let mut session = Session::create(workspace.path().join("perf.jsonl")).unwrap();
    let mut canonical = request();
    canonical.messages = (0..150)
        .map(|_| projected_user(&"x".repeat(2048)))
        .collect();
    let hook: Arc<dyn ProviderContextHook> = Arc::new(DeadlineSuccess {
        extension: false,
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let identity = identity();
    let model = model();
    let abort = AbortFlag::default();
    let mut notices = Vec::new();
    let start = std::time::Instant::now();
    for _ in 0..500 {
        super::project_provider_context(
            canonical.clone(),
            std::slice::from_ref(&hook),
            &identity,
            &model,
            &abort,
            &mut session,
            &mut notices,
        )
        .await
        .unwrap();
    }
    eprintln!(
        "context_hook_probe_ms={:.3}",
        start.elapsed().as_secs_f64() * 1000.0
    );
}

fn tool_history() -> Request {
    let mut request = request();
    request.messages = vec![
        Message::Assistant(AssistantMessage {
            model: model().spec.id.clone(),
            protocol: Protocol::OpenAiChat,
            content: vec![AssistantPart::ToolCall(ToolCall {
                id: octet_ai::ToolCallId("c1".into()),
                name: "old-tool".into(),
                arguments_json: "{}".into(),
                async_execution: false,
                argument_error: None,
            })],
        }),
        Message::User(UserMessage {
            content: vec![UserPart::ToolResult(ToolResult {
                tool_call_id: octet_ai::ToolCallId("c1".into()),
                content: vec![ToolResultPart::Text("original".into())],
                is_error: false,
                added_tool_names: None,
            })],
        }),
    ];
    request
}
#[test]
fn canonical_tool_identity_pairing_cannot_be_forged_and_complete_pairs_can_be_omitted() {
    let canonical = tool_history();
    for case in [
        "rename",
        "arguments",
        "id",
        "orphan",
        "missing",
        "duplicate",
        "status",
        "new-call",
    ] {
        let mut projected = canonical.clone();
        match case {
            "rename" | "arguments" | "id" => {
                let Message::Assistant(assistant) = &mut projected.messages[0] else {
                    unreachable!()
                };
                let AssistantPart::ToolCall(call) = &mut assistant.content[0] else {
                    unreachable!()
                };
                match case {
                    "rename" => call.name = "other-tool".into(),
                    "arguments" => call.arguments_json = "{\"new\":true}".into(),
                    _ => call.id.0 = "other-id".into(),
                }
            }
            "orphan" => {
                projected.messages.remove(0);
            }
            "missing" => {
                projected.messages.pop();
            }
            "duplicate" => projected.messages.push(projected.messages[1].clone()),
            "new-call" => projected.messages.push(projected.messages[0].clone()),
            "status" => {
                let Message::User(user) = &mut projected.messages[1] else {
                    unreachable!()
                };
                let UserPart::ToolResult(result) = &mut user.content[0] else {
                    unreachable!()
                };
                result.is_error = true;
            }
            _ => unreachable!(),
        }
        assert!(
            validate_identities(&canonical, &projected, &model()).is_err(),
            "{case}"
        );
    }
    let mut omitted = canonical.clone();
    omitted.messages.clear();
    assert!(validate_identities(&canonical, &omitted, &model()).is_ok());
    let mut rewritten = canonical.clone();
    let Message::User(user) = &mut rewritten.messages[1] else {
        unreachable!()
    };
    let UserPart::ToolResult(result) = &mut user.content[0] else {
        unreachable!()
    };
    result.content = vec![ToolResultPart::Text("model-visible summary".into())];
    assert!(validate_identities(&canonical, &rewritten, &model()).is_ok());
}

#[tokio::test]
async fn oversized_replacements_fail_before_the_next_hook() {
    let observations = Arc::new(Mutex::new(Vec::new()));
    let hooks: Vec<Arc<dyn ProviderContextHook>> = vec![
        Arc::new(Rewrite {
            system: None,
            text: "x".repeat(MAX_CONTEXT_REQUEST_BYTES),
            observations: observations.clone(),
        }),
        Arc::new(Rewrite {
            system: None,
            text: "safe again".into(),
            observations: observations.clone(),
        }),
    ];
    assert!(matches!(
        project_with_fresh_session(
            request(),
            &hooks,
            &identity(),
            &model(),
            &AbortFlag::default()
        )
        .await,
        Err(AgentError::ProviderContextPreparation(_))
    ));
    assert_eq!(observations.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn opaque_replay_change_is_refused_instead_of_ignored_or_discarded() {
    let observations = Arc::new(Mutex::new(Vec::new()));
    let hook: Arc<dyn ProviderContextHook> = Arc::new(Rewrite {
        system: None,
        text: "changed".into(),
        observations,
    });
    let mut request = request();
    request.responses = Some(ResponsesOptions::full_replay(ResponsesInput::default()));
    assert!(matches!(
        project_with_fresh_session(
            request,
            &[hook],
            &identity(),
            &model(),
            &AbortFlag::default()
        )
        .await,
        Err(AgentError::ProviderContextPreparation(_))
    ));
}

struct AppendHook {
    ready: Arc<tokio::sync::Notify>,
    queued: Arc<AtomicBool>,
    receipt: Mutex<Option<tokio::sync::oneshot::Receiver<Result<crate::EntryId, String>>>>,
    sender: Mutex<Option<tokio::sync::oneshot::Sender<Result<crate::EntryId, String>>>>,
    calls: Arc<AtomicUsize>,
    revoked: Arc<AtomicUsize>,
    cancel: Option<CancellationToken>,
    pending_after_commit: bool,
}
struct AppendWait {
    ready: Arc<tokio::sync::Notify>,
    queued: Arc<AtomicBool>,
    sender: Option<tokio::sync::oneshot::Sender<Result<crate::EntryId, String>>>,
    context: ProviderContextProjectionContext,
    revoked: Arc<AtomicUsize>,
}
impl Drop for AppendWait {
    fn drop(&mut self) {
        self.revoked.fetch_add(1, Ordering::SeqCst);
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(Err("grant revoked".into()));
        }
    }
}
impl crate::ProviderContextSessionWait for AppendWait {
    fn ready(&self) -> Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        let ready = self.ready.clone();
        let queued = self.queued.clone();
        Box::pin(async move {
            let notified = ready.notified_owned();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !queued.load(Ordering::SeqCst) {
                notified.await;
            }
        })
    }
    fn consume_next(
        &mut self,
        session: &mut Session,
        context: &ProviderContextProjectionContext,
    ) -> Result<(), String> {
        assert_eq!(context, &self.context);
        assert_eq!(session.head(), context.head);
        assert!(self.queued.swap(false, Ordering::SeqCst));
        let result = session
            .append_extension_entry(
                "context-checkpoint",
                Some(7),
                "checkpoint",
                serde_json::json!({"state":"private checkpoint"}),
            )
            .map_err(|error| error.to_string());
        self.sender.take().unwrap().send(result).unwrap();
        Ok(())
    }
}
#[async_trait::async_trait]
impl ProviderContextHook for AppendHook {
    fn continues_on_failure(&self) -> bool {
        self.pending_after_commit
    }
    fn begin_session_wait(
        &self,
        session: &Session,
        context: &ProviderContextProjectionContext,
    ) -> Result<Option<Box<dyn crate::ProviderContextSessionWait>>, String> {
        assert_eq!(session.head(), context.head);
        Ok(Some(Box::new(AppendWait {
            ready: self.ready.clone(),
            queued: self.queued.clone(),
            sender: self.sender.lock().unwrap().take(),
            context: context.clone(),
            revoked: self.revoked.clone(),
        })))
    }
    async fn project_context(
        &self,
        _: &Request,
        _: &ProviderContextProjectionContext,
    ) -> Result<Option<ProviderContextProjection>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let receipt = self.receipt.lock().unwrap().take().unwrap();
        self.queued.store(true, Ordering::SeqCst);
        self.ready.notify_one();
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        // Projection becomes active only after the real synced append receipt.
        receipt.await.unwrap()?;
        if self.pending_after_commit {
            std::future::pending::<()>().await;
        }
        Ok(Some(ProviderContextProjection {
            system: None,
            tools: None,
            messages: vec![projected_user("effective checkpoint context")],
        }))
    }
}
fn append_hook(
    cancel: Option<CancellationToken>,
) -> (AppendHook, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let (sender, receipt) = tokio::sync::oneshot::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let revoked = Arc::new(AtomicUsize::new(0));
    (
        AppendHook {
            ready: Arc::new(tokio::sync::Notify::new()),
            queued: Arc::new(AtomicBool::new(false)),
            receipt: Mutex::new(Some(receipt)),
            sender: Mutex::new(Some(sender)),
            calls: calls.clone(),
            revoked: revoked.clone(),
            cancel,
            pending_after_commit: false,
        },
        calls,
        revoked,
    )
}
#[tokio::test]
async fn owning_driver_services_synced_private_append_without_reexecuting_mutating_hook() {
    let (hook, calls, revoked) = append_hook(None);
    let mut host = ExtensionHost::new();
    host.provider_context_hook(hook);
    let (mut agent, captures, _workspace) = agent(host);
    assert!(agent.responses_prewarm_request().unwrap().is_none());
    assert_eq!(
        agent.complete("canonical task").await.unwrap().text,
        "answer"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "private append must not trigger a second hook execution"
    );
    assert_eq!(revoked.load(Ordering::SeqCst), 1);
    assert_eq!(agent.session.entries().len(), 3);
    let private = &agent.session.entries()[1];
    assert!(private
        .metadata
        .as_ref()
        .unwrap()
        .extension_metadata
        .values()
        .all(|metadata| !metadata.public));
    let reopened = Session::open_read_only(agent.session.path()).unwrap();
    assert_eq!(reopened.entries()[1].id, private.id);
    let request = &captures.lock().unwrap()[0];
    assert!(serialize_conversation(&request.messages).contains("effective checkpoint context"));
    assert!(!serialize_conversation(&request.messages).contains("private checkpoint"));
}
#[tokio::test(start_paused = true)]
async fn no_reply_after_private_append_revokes_service_and_keeps_session_usable() {
    let workspace = tempfile::tempdir().unwrap();
    let mut session = Session::create(workspace.path().join("timeout-append.jsonl")).unwrap();
    let (mut hook, calls, revoked) = append_hook(None);
    hook.pending_after_commit = true;
    let mut notices = Vec::new();
    let canonical = request();
    let result = super::project_provider_context(
        canonical.clone(),
        &[Arc::new(hook)],
        &identity(),
        &model(),
        &AbortFlag::default(),
        &mut session,
        &mut notices,
    )
    .await
    .unwrap();
    assert_eq!(
        serialize_conversation(&result.messages),
        serialize_conversation(&canonical.messages)
    );
    assert_eq!(result.system, canonical.system);
    assert_eq!(notices.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(revoked.load(Ordering::SeqCst), 1);
    assert_eq!(session.entries().len(), 1);
    assert_eq!(
        Session::open_read_only(session.path())
            .unwrap()
            .entries()
            .len(),
        1
    );
    session
        .append_extension_entry(
            "after-timeout",
            Some(1),
            "still-usable",
            serde_json::json!({}),
        )
        .unwrap();
    assert_eq!(
        Session::open_read_only(session.path())
            .unwrap()
            .entries()
            .len(),
        2
    );
}

#[tokio::test]
async fn cancellation_wins_before_queued_private_leaf_commit_and_revokes_service() {
    let workspace = tempfile::tempdir().unwrap();
    let mut session = Session::create(workspace.path().join("cancel.jsonl")).unwrap();
    let cancellation = CancellationToken::default();
    let abort = AbortFlag {
        cancellation: cancellation.clone(),
        ..AbortFlag::default()
    };
    let (hook, calls, revoked) = append_hook(Some(cancellation));
    let mut notices = Vec::new();
    let error = super::project_provider_context(
        request(),
        &[Arc::new(hook)],
        &identity(),
        &model(),
        &abort,
        &mut session,
        &mut notices,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, AgentError::Cancelled));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(revoked.load(Ordering::SeqCst), 1);
    assert!(session.entries().is_empty());
    assert!(Session::open_read_only(session.path())
        .unwrap()
        .entries()
        .is_empty());
}

#[test]
fn async_projection_registration_refuses_optional_synchronous_responses_prewarm() {
    let (mut agent, captures, _workspace) = agent(ExtensionHost::new());
    let mut model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    Arc::make_mut(&mut model.endpoint).transport = octet_ai::EndpointTransport::WebSocketPreferred;
    agent.model = model;
    assert!(
        agent.responses_prewarm_request().unwrap().is_some(),
        "uncapped canonical setup is otherwise eligible"
    );
    let observations = Arc::new(Mutex::new(Vec::new()));
    agent.extensions.provider_context_hook(Rewrite {
        system: None,
        text: "effective context".into(),
        observations: observations.clone(),
    });
    assert!(
        agent.responses_prewarm_request().unwrap().is_none(),
        "synchronous setup cannot bypass async projection"
    );
    assert!(observations.lock().unwrap().is_empty());
    assert!(captures.lock().unwrap().is_empty());
}
