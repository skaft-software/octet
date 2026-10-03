use super::*;
use crate::extension::ProviderContextProjection;
use std::sync::atomic::AtomicUsize;

async fn project_provider_context(
    request: Request,
    hooks: &[Arc<dyn ProviderContextHook>],
    context: &ProviderContextProjectionContext,
    model: &Model,
    abort: &AbortFlag,
) -> Result<Request, AgentError> {
    let workspace = tempfile::tempdir().unwrap();
    let mut session = Session::create(workspace.path().join("projection.jsonl")).unwrap();
    super::project_provider_context(request, hooks, context, model, abort, &mut session).await
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
            messages: vec![projected_user(&self.text)],
        }))
    }
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
#[tokio::test(start_paused = true)]
async fn hook_error_deadline_and_same_poll_cancellation_abort_without_fallback() {
    for case in ["error", "deadline", "cancel"] {
        let calls = Arc::new(AtomicUsize::new(0));
        let cancellation = CancellationToken::default();
        let abort = AbortFlag {
            cancellation: cancellation.clone(),
            ..AbortFlag::default()
        };
        let hooks: Vec<Arc<dyn ProviderContextHook>> = vec![
            Arc::new(Refuse {
                calls: calls.clone(),
                wait: case == "deadline",
                cancel: (case == "cancel").then_some(cancellation),
            }),
            Arc::new(Refuse {
                calls: calls.clone(),
                wait: false,
                cancel: None,
            }),
        ];
        let error = project_provider_context(request(), &hooks, &identity(), &model(), &abort)
            .await
            .unwrap_err();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(if case == "cancel" {
            matches!(error, AgentError::Cancelled)
        } else {
            matches!(error, AgentError::ProviderContextPreparation(_))
        });
        assert!(!error.to_string().contains("secret-bearing"));
    }
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
        project_provider_context(
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
        project_provider_context(
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
        Ok(Some(ProviderContextProjection {
            system: None,
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
    let error = super::project_provider_context(
        request(),
        &[Arc::new(hook)],
        &identity(),
        &model(),
        &abort,
        &mut session,
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
