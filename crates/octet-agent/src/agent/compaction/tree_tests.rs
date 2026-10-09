use super::*;
use crate::compaction::{SessionOperationFuture, SessionOperationInvocation};

#[derive(Clone, Copy)]
enum Reply {
    Normal,
    Empty,
    ToolCall,
    Failure,
    Pending,
    Oversized,
    RetryOnce,
}
struct Provider {
    reply: Reply,
    requests: Arc<Mutex<Vec<Request>>>,
}
#[async_trait::async_trait]
impl octet_ai::HostStreamTransport for Provider {
    async fn stream(
        &self,
        model: octet_ai::HostStreamModel,
        request: Request,
        _: Vec<octet_ai::Diagnostic>,
    ) -> Result<octet_ai::ResponseStream, AiError> {
        let attempt = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request);
            requests.len()
        };
        if matches!(self.reply, Reply::RetryOnce) && attempt == 1 {
            return Err(AiError::Http(octet_ai::HttpError {
                status: http::StatusCode::SERVICE_UNAVAILABLE,
                request_id: None,
                retry_after: Some(Duration::from_millis(1)),
                provider_code: None,
                body_snippet: None,
                retryable: true,
            }));
        }
        if matches!(self.reply, Reply::Failure) {
            return Err(AiError::Decode(DecodeError::Json(
                "summary unavailable".into(),
            )));
        }
        if matches!(self.reply, Reply::Pending) {
            return Ok(Box::pin(
                futures_util::stream::once(async {
                    Ok(StreamEvent::Started { response_id: None })
                })
                .chain(futures_util::stream::pending()),
            ));
        }
        let text = match self.reply {
            Reply::Empty => " \n\t".into(),
            Reply::Oversized => "x".repeat(MAX_COMPACTION_HANDOFF_BYTES + 1),
            _ => "## Goal\ncarry abandoned work".into(),
        };
        let mut content = vec![AssistantPart::Text(text)];
        if matches!(self.reply, Reply::ToolCall) {
            content.push(AssistantPart::ToolCall(ToolCall {
                async_execution: false,
                id: octet_ai::ToolCallId("unexpected".into()),
                name: "read".into(),
                arguments_json: "{}".into(),
                argument_error: None,
            }));
        }
        Ok(Box::pin(futures_util::stream::iter(vec![
            Ok(StreamEvent::Started { response_id: None }),
            Ok(StreamEvent::Finished(octet_ai::Response {
                message: AssistantMessage {
                    content,
                    model: model.id,
                    protocol: model.protocol,
                },
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    total_tokens: 15,
                    ..Usage::default()
                },
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

#[derive(Clone, Copy)]
enum HookAction {
    Observe,
    Veto,
    CancelBefore,
    FailAfter,
}
struct Hook {
    action: HookAction,
    seen: Arc<Mutex<Vec<SessionOperation>>>,
    cancellation: CancellationToken,
}
struct Invocation(Option<SessionOperationFuture>);
impl SessionOperationInvocation for Invocation {
    fn take_future(&mut self) -> SessionOperationFuture {
        self.0.take().unwrap()
    }
}
impl SessionOperationHook for Hook {
    fn begin(
        &self,
        session: &Session,
        operation: &SessionOperation,
    ) -> Result<Option<Box<dyn SessionOperationInvocation>>, String> {
        self.seen.lock().unwrap().push(operation.clone());
        let before = matches!(operation, SessionOperation::BeforeTree { .. });
        if let SessionOperation::Tree {
            new_head,
            summary_entry,
            ..
        } = operation
        {
            assert_eq!(&session.head(), new_head);
            assert_eq!(
                Session::open_read_only(session.path()).unwrap().head(),
                *new_head
            );
            if let Some(entry) = summary_entry {
                assert!(matches!(
                    &session.entry(&entry.id).unwrap().value,
                    EntryValue::BranchSummary { .. }
                ));
                assert_eq!(Some(&entry.id), new_head.as_ref());
            }
        }
        let action = self.action;
        let cancellation = self.cancellation.clone();
        Ok(Some(Box::new(Invocation(Some(Box::pin(async move {
            match (action, before) {
                (HookAction::Veto, true) => Ok(SessionOperationDecision::Cancel),
                (HookAction::CancelBefore, true) => {
                    cancellation.cancel();
                    Ok(SessionOperationDecision::Continue)
                }
                (HookAction::FailAfter, false) => Err("observer failed".into()),
                _ => Ok(SessionOperationDecision::Continue),
            }
        }))))))
    }
}

struct Fixture {
    agent: Agent,
    requests: Arc<Mutex<Vec<Request>>>,
    seen: Arc<Mutex<Vec<SessionOperation>>>,
    cancellation: CancellationToken,
    dir: tempfile::TempDir,
    root: EntryId,
    head: EntryId,
}
fn fixture(reply: Reply, action: HookAction) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    Arc::make_mut(&mut model.spec).limits.context_window = 50_000;
    Arc::make_mut(&mut model.spec).limits.max_output_tokens = 16_384;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let client = AiClient::new();
    client.register_host_stream_transport(
        model.endpoint.id.clone(),
        Arc::new(Provider {
            reply,
            requests: requests.clone(),
        }),
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let cancellation = CancellationToken::default();
    let mut extensions = ExtensionHost::new();
    extensions.session_operation_hook(Hook {
        action,
        seen: seen.clone(),
        cancellation: cancellation.clone(),
    });
    let mut agent = Agent::new(AgentConfig {
        client,
        model,
        session: Session::create(dir.path().join("session.jsonl")).unwrap(),
        extensions,
        system: "test system".into(),
        sandbox: SandboxConfig::new(dir.path()),
        effect_broker: EffectBroker::default(),
        max_turns: Some(1),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: Some("tree-navigation".into()),
    })
    .unwrap();
    agent.set_provider_retries_enabled(false);
    let root = user(&mut agent, "shared root");
    let head = assistant(&mut agent, "abandoned answer");
    Fixture {
        agent,
        requests,
        seen,
        cancellation,
        dir,
        root,
        head,
    }
}
fn user(agent: &mut Agent, text: &str) -> EntryId {
    agent
        .session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text(text.into())],
        })))
        .unwrap()
}
fn assistant(agent: &mut Agent, text: &str) -> EntryId {
    agent
        .session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text(text.into())],
            model: agent.model.spec.id.clone(),
            protocol: agent.model.spec.protocol,
        })))
        .unwrap()
}
fn summary_count(agent: &Agent) -> usize {
    agent
        .session
        .entries()
        .iter()
        .filter(|entry| matches!(entry.value, EntryValue::BranchSummary { .. }))
        .count()
}

#[tokio::test]
async fn user_root_navigation_summarizes_only_abandoned_span_and_persists_once() {
    let mut f = fixture(Reply::Normal, HookAction::Observe);
    let result = f
        .agent
        .navigate_session_tree_with_summary(
            f.root.clone(),
            true,
            Some("retain exact paths"),
            f.cancellation.clone(),
            drop,
        )
        .await
        .unwrap();
    assert_eq!(result.editor_text.as_deref(), Some("shared root"));
    let summary = result.summary_entry.unwrap();
    let entry = f.agent.session.entry(&summary).unwrap();
    assert_eq!(entry.parent, None);
    assert!(
        matches!(&entry.value, EntryValue::BranchSummary { from_entry, .. } if from_entry == &f.head)
    );
    assert_eq!(summary_count(&f.agent), 1);
    assert_eq!(f.agent.session.usage_records().len(), 1);
    assert_eq!(f.agent.session.usage_records()[0].usage.total_tokens, 15);
    let requests = f.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0]
        .system
        .as_ref()
        .unwrap()
        .contains("retain exact paths"));
    let source = serialize_conversation(&requests[0].messages);
    assert!(source.contains("abandoned answer"));
    assert!(!source.contains("shared root"));
    drop(requests);
    let seen = f.seen.lock().unwrap();
    assert!(
        matches!(&seen[0], SessionOperation::BeforeTree { preparation: Some(preparation), .. }
        if preparation.common_ancestor_id == Some(f.root.clone()) && preparation.entries_to_summarize.len() == 1
        && preparation.user_wants_summary && preparation.custom_instructions.as_deref() == Some("retain exact paths"))
    );
    assert!(
        matches!(&seen[1], SessionOperation::Tree { summary_entry: Some(entry), .. } if entry.id == summary)
    );
    drop(seen);
    let cost = f.agent.session.total_cost_microdollars();
    drop(f.agent);
    let reopened = Session::open(f.dir.path().join("session.jsonl")).unwrap();
    assert_eq!(reopened.head(), Some(summary));
    assert_eq!(reopened.usage_records().len(), 1);
    assert_eq!(reopened.total_cost_microdollars(), cost);
    let context = serialize_conversation(&reopened.context().unwrap());
    assert!(context.contains("carry abandoned work"));
    assert!(!context.contains("shared root"));
}

#[tokio::test]
async fn sibling_navigation_keeps_target_ancestry_but_never_summarizes_siblings_or_results() {
    let mut f = fixture(Reply::Normal, HookAction::Observe);
    f.agent.session.checkout(f.root.clone()).unwrap();
    let sibling = assistant(&mut f.agent, "sibling-only evidence");
    f.agent.session.checkout(f.head.clone()).unwrap();
    f.agent
        .session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::ToolResult(ToolResult {
                tool_call_id: octet_ai::ToolCallId("read".into()),
                content: vec![ToolResultPart::Text("secret result body".into())],
                is_error: false,
                added_tool_names: None,
            })],
        })))
        .unwrap();
    let result = f
        .agent
        .navigate_session_tree_with_summary(
            sibling.clone(),
            true,
            None,
            f.cancellation.clone(),
            drop,
        )
        .await
        .unwrap();
    assert_eq!(result.editor_text, None);
    assert_eq!(
        f.agent
            .session
            .entry(&result.summary_entry.unwrap())
            .unwrap()
            .parent,
        Some(sibling)
    );
    let request = serialize_conversation(&f.requests.lock().unwrap()[0].messages);
    assert!(request.contains("abandoned answer"));
    assert!(!request.contains("sibling-only evidence"));
    assert!(!request.contains("secret result body"));
    let context = serialize_conversation(&f.agent.session.context().unwrap());
    assert!(context.contains("sibling-only evidence"));
    assert!(context.contains("carry abandoned work"));
    assert!(!context.contains("abandoned answer"));
    assert!(f.agent.session.entry(&f.head).is_some());
}

#[tokio::test]
async fn ancestor_descendant_custom_and_current_head_navigation_have_native_positions() {
    let mut f = fixture(Reply::Normal, HookAction::Observe);
    let before = std::fs::read(f.agent.session.path()).unwrap();
    let noop = f
        .agent
        .navigate_session_tree_with_summary(
            f.head.clone(),
            true,
            None,
            f.cancellation.clone(),
            drop,
        )
        .await
        .unwrap();
    assert_eq!(noop, TreeNavigationResult::default());
    assert!(f.requests.lock().unwrap().is_empty());
    assert!(f.seen.lock().unwrap().is_empty());
    assert_eq!(std::fs::read(f.agent.session.path()).unwrap(), before);
    let descendant = assistant(&mut f.agent, "future answer");
    f.agent.session.checkout(f.head.clone()).unwrap();
    let result = f
        .agent
        .navigate_session_tree_with_summary(
            descendant.clone(),
            true,
            None,
            f.cancellation.clone(),
            drop,
        )
        .await
        .unwrap();
    assert_eq!(
        result.summary_entry, None,
        "no source span when advancing to a descendant"
    );
    assert_eq!(f.agent.session.head(), Some(descendant.clone()));
    assert!(f.requests.lock().unwrap().is_empty());
    f.agent
        .navigate_session_tree_with_summary(
            f.head.clone(),
            false,
            None,
            f.cancellation.clone(),
            drop,
        )
        .await
        .unwrap();
    assert_eq!(f.agent.session.head(), Some(f.head.clone()));
    let custom = f
        .agent
        .session
        .append_custom_message(
            crate::session::CustomMessage {
                custom_type: "note".into(),
                content: crate::session::CustomMessageContent::Parts(vec![
                    crate::session::CustomMessagePart::Text {
                        text: "first".into(),
                    },
                    crate::session::CustomMessagePart::Text {
                        text: "second".into(),
                    },
                ]),
                display: false,
                details: None,
            },
            None,
        )
        .unwrap();
    assistant(&mut f.agent, "after custom");
    let result = f
        .agent
        .navigate_session_tree_with_summary(custom, false, None, f.cancellation.clone(), drop)
        .await
        .unwrap();
    assert_eq!(result.editor_text.as_deref(), Some("firstsecond"));
    assert_eq!(f.agent.session.head(), Some(f.head));
    assert_eq!(summary_count(&f.agent), 0);
}

#[tokio::test]
async fn veto_cancellation_and_failed_provider_never_publish_navigation() {
    for (reply, action, pre_cancel) in [
        (Reply::Normal, HookAction::Veto, false),
        (Reply::Normal, HookAction::CancelBefore, false),
        (Reply::Normal, HookAction::Observe, true),
        (Reply::Failure, HookAction::Observe, false),
        (Reply::Empty, HookAction::Observe, false),
        (Reply::ToolCall, HookAction::Observe, false),
        (Reply::Oversized, HookAction::Observe, false),
    ] {
        let mut f = fixture(reply, action);
        if pre_cancel {
            f.cancellation.cancel();
        }
        assert!(f
            .agent
            .navigate_session_tree_with_summary(
                f.root.clone(),
                true,
                None,
                f.cancellation.clone(),
                drop
            )
            .await
            .is_err());
        assert_eq!(f.agent.session.head(), Some(f.head));
        assert_eq!(summary_count(&f.agent), 0);
        assert_eq!(f.agent.session.entries().len(), 2);
        assert!(!f
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, SessionOperation::Tree { .. })));
        if matches!(reply, Reply::Empty | Reply::ToolCall | Reply::Oversized) {
            assert_eq!(
                f.agent.session.usage_records().len(),
                1,
                "malformed output still settles billing exactly once"
            );
        } else if !matches!(reply, Reply::Failure) {
            assert!(f.requests.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn cancellation_after_dispatch_keeps_source_and_provider_exposure() {
    let mut f = fixture(Reply::Pending, HookAction::Observe);
    let requests = f.requests.clone();
    let cancellation = f.cancellation.clone();
    let result = {
        let pending = f.agent.navigate_session_tree_with_summary(
            f.root.clone(),
            true,
            None,
            f.cancellation.clone(),
            drop,
        );
        let cancel = async move {
            while requests.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
            cancellation.cancel();
        };
        let (result, ()) = tokio::join!(pending, cancel);
        result
    };
    assert!(matches!(result, Err(AgentError::Cancelled)));
    assert_eq!(f.agent.session.head(), Some(f.head));
    assert_eq!(summary_count(&f.agent), 0);
    assert!(f.agent.session.usage_records().is_empty());
    assert!(f.agent.session.has_uncertain_usage());
}

#[tokio::test(start_paused = true)]
async fn retried_navigation_accounts_and_commits_once() {
    let mut f = fixture(Reply::RetryOnce, HookAction::Observe);
    f.agent.set_provider_retries_enabled(true);
    let mut events = Vec::new();
    let result = f
        .agent
        .navigate_session_tree_with_summary(
            f.root.clone(),
            true,
            None,
            f.cancellation.clone(),
            |event| events.push(event),
        )
        .await
        .unwrap();
    assert!(result.summary_entry.is_some());
    assert_eq!(f.requests.lock().unwrap().len(), 2);
    assert_eq!(summary_count(&f.agent), 1);
    assert_eq!(f.agent.session.usage_records().len(), 1);
    assert_eq!(f.seen.lock().unwrap().len(), 2);
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ProviderOperationRetry {
            operation: crate::events::ProviderOperation::BranchSummary,
            ..
        }
    )));
}

#[tokio::test]
async fn non_root_user_summary_attaches_to_parent_and_preserves_nested_file_details() {
    let mut f = fixture(Reply::Normal, HookAction::Observe);
    let target = user(&mut f.agent, "editable request");
    assistant(&mut f.agent, "exploration");
    f.agent
        .session
        .branch_with_summary(
            Some(target.clone()),
            "earlier exploration".into(),
            crate::compaction::CompactionDetails {
                read_files: vec!["read.rs".into()],
                modified_files: vec!["changed.rs".into()],
            },
        )
        .unwrap();
    assistant(&mut f.agent, "more exploration");
    let result = f
        .agent
        .navigate_session_tree_with_summary(target, true, None, f.cancellation.clone(), drop)
        .await
        .unwrap();
    assert_eq!(result.editor_text.as_deref(), Some("editable request"));
    let entry = f
        .agent
        .session
        .entry(&result.summary_entry.unwrap())
        .unwrap();
    assert_eq!(entry.parent, Some(f.head));
    let EntryValue::BranchSummary {
        summary, details, ..
    } = &entry.value
    else {
        panic!("durable summary")
    };
    assert_eq!(details.read_files, vec!["read.rs"]);
    assert_eq!(details.modified_files, vec!["changed.rs"]);
    assert!(summary.contains("<modified-files>\nchanged.rs\n</modified-files>"));
    let source = serialize_conversation(&f.requests.lock().unwrap()[0].messages);
    assert!(source.contains("earlier exploration"));
    assert!(source.contains("more exploration"));
    assert!(!source.contains("editable request"));
}

#[tokio::test]
async fn post_commit_failure_reports_durable_head_without_retry_or_rollback() {
    let mut f = fixture(Reply::Normal, HookAction::FailAfter);
    let error = f
        .agent
        .navigate_session_tree_with_summary(
            f.root.clone(),
            true,
            None,
            f.cancellation.clone(),
            drop,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("committed"));
    assert!(error.to_string().contains("do not retry"));
    let committed = f.agent.session.head().unwrap();
    assert_ne!(committed, f.head);
    assert_eq!(summary_count(&f.agent), 1);
    assert_eq!(f.agent.session.usage_records().len(), 1);
    assert_eq!(f.requests.lock().unwrap().len(), 1);
    assert_eq!(
        Session::open_read_only(f.agent.session.path())
            .unwrap()
            .head(),
        Some(committed)
    );
}
