#![allow(missing_docs)]

//! Transport-half regressions for the deferred lifecycle (ledger 1e.1 / 4.12)
//! plus the in-process faux provider double.

use std::sync::Arc;

use futures_util::StreamExt;

use octet_ai::{
    AiClient, AiError, AssistantPart, DeferredPollPermit, DeferredPollRefusalKind,
    FauxDeferredStatus, FauxMessage, FauxOptions, FauxProvider, FauxResponse, FauxToolCall,
    Message, OutputFormat, OutputModalities, ReasoningConfig, ReasoningMode, Request,
    RequestOverrides, StopReason, StreamEvent, ToolChoice, Usage, UserMessage, UserPart,
};

fn minimal_request() -> Request {
    Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("hello".to_owned())],
        })],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: octet_ai::CompatibilityMode::Strict,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    }
}

async fn finished(stream: &mut octet_ai::ResponseStream) -> octet_ai::Response {
    let mut terminal = None;
    while let Some(event) = stream.next().await {
        match event.expect("stream event") {
            StreamEvent::Finished(response) => terminal = Some(response),
            _ => {}
        }
    }
    terminal.expect("stream must finish")
}

#[tokio::test]
async fn faux_deferred_pending_ready_and_permit_consumption() {
    let provider = FauxProvider::new(FauxOptions {
        pending_fetches: 1,
        poll_after_ms: Some(7),
        ..FauxOptions::default()
    });
    provider.set_responses(vec![FauxResponse::Message(FauxMessage::new("deferred done"))]);
    let client = AiClient::new();
    provider.register(&client);
    let model = provider.model().clone();

    let mut stream = client
        .submit_deferred(
            &model,
            minimal_request(),
            RequestOverrides::default(),
            None,
        )
        .await
        .expect("deferred submission");
    let parked = finished(&mut stream).await;
    assert_eq!(parked.stop_reason, StopReason::Deferred);
    let handle = parked.deferred.clone().expect("deferred handle");
    assert_eq!(handle.provider, "faux");
    assert_eq!(handle.model_id, "faux-1");
    assert_eq!(handle.api, "faux");
    assert_eq!(handle.poll_after_ms, Some(7));
    assert_eq!(
        provider.deferred_status(&handle),
        Some(FauxDeferredStatus::Pending)
    );

    // First permitted poll still reports the handle: it is not a new request,
    // and the permit is consumed exactly once.
    let mut stream = client
        .fetch_deferred(
            &model,
            handle.clone(),
            DeferredPollPermit::one("pass-1", 0),
            0,
            Some(0),
        )
        .await
        .expect("pending poll");
    let still_parked = finished(&mut stream).await;
    assert_eq!(still_parked.stop_reason, StopReason::Deferred);
    assert_eq!(
        provider.deferred_status(&handle),
        Some(FauxDeferredStatus::Ready)
    );

    // Second permitted poll settles the scripted response.
    let mut stream = client
        .fetch_deferred(
            &model,
            handle.clone(),
            DeferredPollPermit::one("pass-2", 0),
            0,
            None,
        )
        .await
        .expect("ready poll");
    let settled = finished(&mut stream).await;
    assert_eq!(settled.stop_reason, StopReason::EndTurn);
    assert!(settled.deferred.is_none());
    assert!(matches!(
        settled.message.content.as_slice(),
        [AssistantPart::Text(text)] if text == "deferred done"
    ));

    let state = provider.state();
    assert_eq!(state.deferred_submission_count, 1);
    assert_eq!(state.deferred_fetch_count, 2);

    // A repeated ready poll returns the cached terminal response, never a
    // second script execution.
    let mut stream = client
        .fetch_deferred(
            &model,
            handle.clone(),
            DeferredPollPermit::one("pass-3", 0),
            0,
            None,
        )
        .await
        .expect("cached poll");
    let cached = finished(&mut stream).await;
    assert_eq!(cached.stop_reason, StopReason::EndTurn);
    assert_eq!(provider.state().deferred_fetch_count, 3);
}

#[tokio::test]
async fn permits_fail_closed_before_any_provider_work() {
    let provider = FauxProvider::new(FauxOptions::default());
    provider.set_responses(vec![FauxResponse::Message(FauxMessage::new("ready"))]);
    let client = AiClient::new();
    provider.register(&client);
    let model = provider.model().clone();

    let mut stream = client
        .submit_deferred(
            &model,
            minimal_request(),
            RequestOverrides::default(),
            None,
        )
        .await
        .expect("deferred submission");
    let handle = finished(&mut stream).await.deferred.expect("handle");

    let mut consumed = DeferredPollPermit::one("pass", 0);
    consumed.consume(0).unwrap();
    assert!(matches!(
        client
            .fetch_deferred(&model, handle.clone(), consumed, 0, None)
            .await,
        Err(AiError::Deferred(DeferredPollRefusalKind::AlreadyConsumed))
    ));
    assert!(matches!(
        client
            .fetch_deferred(
                &model,
                handle.clone(),
                DeferredPollPermit::one("pass", 4),
                0,
                None
            )
            .await,
        Err(AiError::Deferred(DeferredPollRefusalKind::StaleGeneration { .. }))
    ));
    assert!(matches!(
        client
            .fetch_deferred(
                &model,
                handle.clone(),
                DeferredPollPermit::none("pass", 0),
                0,
                None
            )
            .await,
        Err(AiError::Deferred(DeferredPollRefusalKind::NoPermit))
    ));
    // Refusals never reached the provider.
    assert_eq!(provider.state().deferred_fetch_count, 0);

    // A handle for another model is refused before the transport sees it.
    let mut foreign = handle.clone();
    foreign.model_id = "other-model".to_owned();
    assert!(client
        .fetch_deferred(
            &model,
            foreign,
            DeferredPollPermit::one("pass", 0),
            0,
            None
        )
        .await
        .is_err());
    assert_eq!(provider.state().deferred_fetch_count, 0);
}

#[tokio::test]
async fn faux_deferred_failure_and_cancellation_are_terminal() {
    let provider = FauxProvider::new(FauxOptions::default());
    provider.set_responses(vec![
        FauxResponse::Failure("provider exploded".to_owned()),
        FauxResponse::Message(FauxMessage::new("second")),
    ]);
    let client = AiClient::new();
    provider.register(&client);
    let model = provider.model().clone();

    let mut stream = client
        .submit_deferred(
            &model,
            minimal_request(),
            RequestOverrides::default(),
            None,
        )
        .await
        .expect("deferred submission");
    let failed_handle = finished(&mut stream).await.deferred.expect("handle");
    assert_eq!(
        provider.deferred_status(&failed_handle),
        Some(FauxDeferredStatus::Failed)
    );
    let mut stream = client
        .fetch_deferred(
            &model,
            failed_handle.clone(),
            DeferredPollPermit::one("pass", 0),
            0,
            None,
        )
        .await
        .expect("failed poll returns a stream");
    let error = stream
        .next()
        .await
        .expect("terminal event")
        .expect_err("failed script must be terminal");
    assert!(matches!(error, AiError::Provider(_)));

    let mut stream = client
        .submit_deferred(
            &model,
            minimal_request(),
            RequestOverrides::default(),
            None,
        )
        .await
        .expect("second deferred submission");
    let cancelled_handle = finished(&mut stream).await.deferred.expect("handle");
    client
        .cancel_deferred(&model, cancelled_handle.clone())
        .await
        .expect("cancellation");
    assert_eq!(
        provider.deferred_status(&cancelled_handle),
        Some(FauxDeferredStatus::Cancelled)
    );
    let error = client
        .fetch_deferred(
            &model,
            cancelled_handle.clone(),
            DeferredPollPermit::one("pass", 0),
            0,
            None,
        )
        .await
        .err()
        .expect("cancelled polls are terminal");
    assert!(matches!(error, AiError::Provider(_)));
    assert_eq!(provider.state().cancelled_deferred, vec![cancelled_handle]);

    // Unknown handles are refused, never treated as pending.
    let unknown =
        octet_ai::DeferredHandle::new("faux", "faux-1", "faux", "not-a-real-handle");
    assert!(matches!(
        client
            .fetch_deferred(
                &model,
                unknown,
                DeferredPollPermit::one("pass", 0),
                0,
                None
            )
            .await,
        Err(AiError::Provider(_))
    ));
}

#[tokio::test]
async fn deferred_requests_require_a_deferred_capable_transport() {
    let catalog = octet_ai::ModelCatalog::builtin().unwrap();
    let spec = catalog.models().next().expect("built-in model").clone();
    // Deferred entry points take a resolved model, exactly like `stream` and
    // `complete`; the catalog's model list is a list of specifications.
    let model = catalog.resolve(&spec.id).expect("built-in model endpoint");
    let client = AiClient::new();
    assert!(matches!(
        client
            .submit_deferred(
                &model,
                minimal_request(),
                RequestOverrides::default(),
                None
            )
            .await,
        Err(AiError::Unsupported(octet_ai::UnsupportedError::Deferred))
    ));
    // The handle must name this model: a foreign handle is refused before the
    // transport is consulted, which would not exercise the refusal under test.
    let handle = octet_ai::DeferredHandle::new("none", spec.id.0.clone(), "none", "id");
    assert!(matches!(
        client
            .fetch_deferred(
                &model,
                handle,
                DeferredPollPermit::one("pass", 0),
                0,
                None
            )
            .await,
        Err(AiError::Unsupported(octet_ai::UnsupportedError::Deferred))
    ));
}

#[tokio::test]
async fn faux_ordinary_stream_emits_text_reasoning_tools_and_usage() {
    let provider = FauxProvider::new(FauxOptions::default());
    provider.set_responses(vec![FauxResponse::Message(
        FauxMessage::new("answer")
            .with_reasoning("think")
            .with_tool_call(FauxToolCall::new(
                "lookup",
                serde_json::json!({"q": "value"}),
            ))
            .with_usage(Usage {
                input_tokens: 3,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cache_write_1h_tokens: 0,
                output_tokens: 2,
                reasoning_tokens: 1,
                total_tokens: 5,
            }),
    )]);
    let client = AiClient::new();
    provider.register(&client);
    let model = provider.model().clone();

    let mut stream = client
        .stream(&model, minimal_request())
        .await
        .expect("faux stream");
    let response = finished(&mut stream).await;
    assert_eq!(response.stop_reason, StopReason::ToolUse);
    assert!(response.message.content.iter().any(
        |part| matches!(part, AssistantPart::Reasoning(reasoning) if reasoning.text.as_deref() == Some("think"))
    ));
    assert!(response.message.content.iter().any(
        |part| matches!(part, AssistantPart::ToolCall(call) if call.name == "lookup" && call.arguments_json == "{\"q\":\"value\"}")
    ));
    assert_eq!(response.usage.total_tokens, 5);
    assert_eq!(provider.state().call_count, 1);
}

#[tokio::test]
async fn faux_exhausted_script_fails_without_retry() {
    let provider = FauxProvider::new(FauxOptions::default());
    let client = AiClient::new();
    provider.register(&client);
    let model = provider.model().clone();
    let error = client
        .stream(&model, minimal_request())
        .await
        .err()
        .expect("empty script is terminal");
    assert!(matches!(error, AiError::Provider(_)));
    assert_eq!(provider.state().call_count, 1);
}

#[test]
fn faux_provider_model_identity_is_self_consistent() {
    let provider = FauxProvider::new(FauxOptions::default());
    let model = provider.model();
    assert_eq!(model.spec.id.0, "faux-1");
    assert_eq!(model.endpoint.id.0, "faux");
    let handle = octet_ai::DeferredHandle::new(
        "faux",
        model.spec.id.0.clone(),
        "faux",
        "faux-call-1",
    );
    assert_eq!(provider.deferred_status(&handle), None);
    // `Arc` is only used to prove the provider is shareable/registerable.
    let provider = Arc::new(provider);
    assert!(provider.pending_response_count() == 0);
}
