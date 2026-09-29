//! The `/fast` control surface: live request routing, compaction under a fast
//! selection, cost ceilings, and rebuild barriers.
//! Separate because the fast route is the only control path that reaches a provider
//! outside the normal turn loop.

use super::*;

use super::support::*;

/// Exercise the slash handler, actual HTTP/SSE requests, status, route
/// transitions, and rebuilds together; a setter-only assertion is not enough.
#[tokio::test]
async fn fast_commands_reach_live_requests_and_fail_closed_on_other_routes() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(fast_response()),
        )
        .mount(&server)
        .await;
    let mut model = scripted_codex_model(&server.uri());
    Arc::make_mut(&mut model.spec).limits.context_window = 272_000;
    let (_directory, mut app) = fast_test_app(model.clone());
    let mut shell = InteractiveShell::test_shell();
    let before = std::fs::read(app.agent.session().path()).unwrap();
    app = fast_idle(app, &mut shell, "/fast").await;
    assert!(shell.debug_snapshot().contains("Fast mode: off"));
    assert_eq!(std::fs::read(app.agent.session().path()).unwrap(), before);
    assert!(!app.agent.session().has_uncertain_usage());
    assert!(server.received_requests().await.unwrap().is_empty());

    app = fast_idle(app, &mut shell, "/fast on").await;
    assert!(shell.debug_snapshot().contains("Fast mode: on"));
    assert!(commands::status_text(&app, None).contains("Fast mode: on"));
    assert!(commands::status_text(&app, None).contains("known subtotal"));
    assert!(app.agent.session().has_uncertain_usage());
    assert_eq!(app.model.spec.limits.context_window, 272_000);
    app.agent.complete("priority turn").await.unwrap();
    // Rebuilds for reasoning/reload must retain the real request control.
    app = rebuild_app(app, None, Some(ReasoningConfig::Off), None, None).unwrap();
    app.agent.complete("priority after rebuild").await.unwrap();
    let before = std::fs::read(app.agent.session().path()).unwrap();
    app = fast_idle(app, &mut shell, "/fast status").await;
    assert_eq!(std::fs::read(app.agent.session().path()).unwrap(), before);
    app = fast_idle(app, &mut shell, "/fast off").await;
    app.agent.complete("ordinary turn").await.unwrap();
    assert!(commands::status_text(&app, None).contains("Fast mode: off"));
    assert!(commands::cost_text(app.agent.session(), &app.model).contains("Known subtotal only"));
    // Clearing a request control does not erase uncertain spend.
    assert!(app.agent.session().has_uncertain_usage());
    assert_eq!(
        app.agent
            .session()
            .usage_uncertainty_records()
            .iter()
            .filter(|record| record.operation == "responses-priority-tier")
            .count(),
        1
    );

    app = fast_idle(app, &mut shell, "/fast on").await;
    let mut ordinary = model.clone();
    Arc::make_mut(&mut ordinary.endpoint)
        .runtime
        .responses_profile = octet_ai::ResponsesRuntimeProfile::Default;
    Arc::make_mut(&mut ordinary.endpoint).id = octet_ai::EndpointId("ordinary".into());
    Arc::make_mut(&mut ordinary.spec).endpoint = ordinary.endpoint.id.clone();
    Arc::make_mut(&mut ordinary.spec).id = ModelId("ordinary".into());
    app.catalog
        .register_endpoint((*ordinary.endpoint).clone())
        .unwrap();
    app.catalog
        .register_model((*ordinary.spec).clone())
        .unwrap();
    app = rebuild_app(app, Some(ordinary), None, None, None).unwrap();
    assert_eq!(
        app.agent.service_tier(),
        None,
        "unsupported model switches clear the tier"
    );
    let before = std::fs::read(app.agent.session().path()).unwrap();
    for command in ["/fast on", "/fast off", "/fast", "/fast status"] {
        app = fast_idle(app, &mut shell, command).await;
        assert!(shell
            .debug_error()
            .unwrap()
            .contains("only available on Codex Responses routes"));
    }
    assert_eq!(std::fs::read(app.agent.session().path()).unwrap(), before);
    app.agent
        .complete("unsupported route stays ordinary")
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests.len(),
        4,
        "status and rejected controls never invoke inference"
    );
    let bodies: Vec<serde_json::Value> = requests
        .iter()
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect();
    assert_eq!(bodies[0]["service_tier"], "priority");
    assert_eq!(bodies[1]["service_tier"], "priority");
    assert!(bodies[2].get("service_tier").is_none());
    assert!(bodies[3].get("service_tier").is_none());
    for body in bodies {
        let body = body.to_string();
        assert!(
            !body.contains("/fast"),
            "local controls must not enter model context"
        );
    }
    app.agent.set_max_session_cost_microdollars(Some(u64::MAX));
    assert!(app
        .agent
        .complete("hard ceilings must fail closed")
        .await
        .is_err());
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
}

#[tokio::test]
async fn fast_compaction_preserves_selection_and_restart_keeps_uncertainty_fenced() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(fast_response()),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/responses/compact"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "output":[{"type":"compaction", "id":"compact-fast", "encrypted_content":"checkpoint"}],
            "usage":{"input_tokens":5,"output_tokens":2}
        })))
        .mount(&server)
        .await;
    let (_directory, mut app) = fast_test_app(scripted_codex_model(&server.uri()));
    let mut shell = InteractiveShell::test_shell();
    app = fast_idle(app, &mut shell, "/fast on").await;
    app.agent.complete("before compaction").await.unwrap();
    app.config.compaction.mode = CompactionMode::NativeResponses;
    assert_eq!(
        attempt_compaction(&mut app).await.unwrap(),
        CompactionOutcome::NativeCompacted
    );
    assert_eq!(
        app.agent.service_tier(),
        Some(octet_ai::ServiceTier::Priority)
    );
    assert!(app.agent.session().has_uncertain_usage());
    app.agent.complete("after compaction").await.unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        if request.url.path().ends_with("/compact") {
            // Compact has no declared tier field: never invent one or
            // promise that this auxiliary operation used priority.
            assert!(body.get("service_tier").is_none());
        } else {
            assert_eq!(body["service_tier"], "priority");
        }
    }

    // Reconstruct through startup, not a compatible idle rebuild: fast is
    // process-scoped, whereas past uncertain spend is durable and sticky.
    let session_path = app.agent.session().path().to_owned();
    let model = app.model.spec.id.clone();
    let catalog = app.catalog.clone();
    let mut config = app.config.clone();
    config.max_cost_microdollars = Some(u64::MAX);
    drop(app);
    let mut boot = crate::app::bootstrap::bootstrap(config).unwrap();
    boot.catalog = catalog;
    let mut app = build_app(
        boot,
        crate::app::bootstrap::LaunchSelection {
            model,
            session: SessionSelection::OpenExisting(session_path),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: ReasoningMode::Standard,
        },
        "system".into(),
    )
    .unwrap();
    assert_eq!(app.agent.service_tier(), None);
    assert!(app.agent.session().has_uncertain_usage());
    // Match the budget error itself, not a guessed word in its display:
    // UsageUncertain reports "unsettled provider usage".
    assert_eq!(
        attempt_compaction(&mut app).await.unwrap(),
        CompactionOutcome::Skipped {
            reason: octet_agent::AgentError::UsageUncertain.to_string(),
        },
        "compaction must refuse the unsettled ledger before network I/O",
    );
    assert!(app.agent.complete("ceiling after restart").await.is_err());
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[test]
fn unpriced_usage_marks_local_and_native_compaction_reports_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("compaction-report.jsonl");
    let mut session = Session::create(&path).unwrap();
    let first_kept = session
        .append(EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text("retained turn".into())],
            },
        )))
        .unwrap();
    session
        .record_compaction_usage(
            octet_ai::EndpointId("fixture".into()),
            ModelId("fixture".into()),
            octet_ai::Usage::default(),
            None,
        )
        .unwrap();
    // A later fully priced compaction does not repair the old missing cost.
    session
        .record_compaction_usage(
            octet_ai::EndpointId("fixture".into()),
            ModelId("fixture".into()),
            octet_ai::Usage::default(),
            Some(octet_ai::Cost::default()),
        )
        .unwrap();
    session.compact("summary", first_kept).unwrap();
    drop(session);
    let session = Session::open_read_only(path).unwrap();
    assert!(session.has_unpriced_usage());
    assert!(!session.has_uncertain_usage());
    for outcome in [
        CompactionOutcome::Compacted { elided: 1 },
        CompactionOutcome::NativeCompacted,
    ] {
        let mut shell = InteractiveShell::test_shell();
        report_compaction(&mut shell, &outcome, &session);
        assert!(shell
            .debug_snapshot()
            .contains("session usage or pricing uncertain"));
    }
}

#[tokio::test]
async fn fast_local_summary_cost_ceiling_fails_closed_before_network_io() {
    let server = wiremock::MockServer::start().await;
    let (_directory, mut app) = fast_test_app(scripted_codex_model(&server.uri()));
    seed_compaction_session(&mut app.agent);
    app.config.compaction.keep_recent_tokens = 1;
    app.set_fast_mode(true).unwrap();
    app.agent.set_max_session_cost_microdollars(Some(u64::MAX));
    // Match the budget error itself, not a guessed word in its display:
    // UsageUncertain reports "unsettled provider usage".
    assert_eq!(
        attempt_compaction(&mut app).await.unwrap(),
        CompactionOutcome::Skipped {
            reason: octet_agent::AgentError::UsageUncertain.to_string(),
        },
        "compaction must refuse the unsettled ledger before network I/O",
    );
    assert_eq!(
        app.agent.service_tier(),
        Some(octet_ai::ServiceTier::Priority)
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn fast_unsupported_active_controls_are_not_queued() {
    let mut inspection = test_run_inspection().clone();
    // A profile alone must not lend a Chat route Responses capabilities.
    Arc::make_mut(&mut inspection.model.spec).protocol = octet_ai::Protocol::OpenAiChat;
    Arc::make_mut(&mut inspection.model.endpoint)
        .runtime
        .responses_profile = octet_ai::ResponsesRuntimeProfile::Codex;
    let mut shell = InteractiveShell::test_shell();
    shell.begin_run("test");
    for command in ["/fast", "/fast status", "/fast on", "/fast off"] {
        let (queue, quit) =
            run_active_command(&mut shell, commands::parse(command), &inspection).await;
        assert!(queue.is_empty());
        assert!(!quit);
        assert!(shell
            .debug_error()
            .unwrap()
            .contains("only available on Codex Responses routes"));
    }
}

#[test]
fn fast_rebuild_scope_and_queue_barriers_are_explicit() {
    let model = scripted_codex_model("http://127.0.0.1:1");
    let (directory, mut app) = fast_test_app(model);
    app.set_fast_mode(true).unwrap();
    let path = app.agent.session().path().to_path_buf();
    app = rebuild_app(
        app,
        None,
        None,
        None,
        Some(SessionSelection::OpenExisting(path.clone())),
    )
    .unwrap();
    assert_eq!(
        app.agent.service_tier(),
        Some(octet_ai::ServiceTier::Priority)
    );
    let mut next = app.model.clone();
    Arc::make_mut(&mut next.spec).id = ModelId("another-scripted".into());
    app.catalog.register_model((*next.spec).clone()).unwrap();
    app = rebuild_app(app, Some(next), None, None, None).unwrap();
    assert_eq!(
        app.agent.service_tier(),
        Some(octet_ai::ServiceTier::Priority)
    );
    app = rebuild_app(
        app,
        None,
        None,
        None,
        Some(SessionSelection::CreateNew(
            directory.path().join("new-fast.jsonl"),
        )),
    )
    .unwrap();
    assert_eq!(app.agent.service_tier(), None);
    assert!(!app.agent.session().has_uncertain_usage());
    app = rebuild_app(
        app,
        None,
        None,
        None,
        Some(SessionSelection::OpenExisting(path)),
    )
    .unwrap();
    assert_eq!(
        app.agent.service_tier(),
        None,
        "resuming a different session does not silently opt into billing"
    );
    assert!(
        app.agent.session().has_uncertain_usage(),
        "old accounting remains sticky"
    );

    let mut queue = VecDeque::new();
    for action in [
        PendingIdleAction::Fast(true),
        PendingIdleAction::Fast(false),
        PendingIdleAction::NewSession,
        PendingIdleAction::Fast(true),
    ] {
        push_pending_action(&mut queue, action);
    }
    assert_eq!(
        queue.into_iter().collect::<Vec<_>>(),
        vec![
            PendingIdleAction::Fast(false),
            PendingIdleAction::NewSession,
            PendingIdleAction::Fast(true)
        ]
    );
}
