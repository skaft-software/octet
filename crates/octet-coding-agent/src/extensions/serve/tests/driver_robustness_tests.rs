//! The session driver's own liveness and error reporting.
//! These two cases hang off a live driver rather than on a projection: a long
//! provider wait must not retract or settle a run, and a transport failure must
//! surface its status and request id so the host can attribute it.

use super::*;
use octet_agent::AgentError;
use octet_ai::{AiError, TransportPhase};

use super::test_support::*;

#[tokio::test]
async fn repeated_network_waits_do_not_retract_or_settle_serve_projection() {
    let directory = tempfile::tempdir().unwrap();
    let plan = pull_request_worker_plan(directory.path(), "network-wait");
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    let run_id = RunId::new("run-network-wait").unwrap();
    let mut projection = ProjectionState::new(7);
    let committed = ItemId::new("committed-assistant").unwrap();
    let turn = TurnId::new("committed-turn").unwrap();
    projection
        .completed_assistant_items
        .push_back(Some((committed.clone(), turn.clone())));
    let tool = ItemId::new("committed-tool").unwrap();
    projection.tool_items.insert("call".into(), tool.clone());
    projection.assistant_item = Some(ItemId::new("stale-assistant").unwrap());
    projection.reasoning_item = Some(ItemId::new("stale-reasoning").unwrap());
    let mut context = RunContextProjection::new(0, 0, 0);
    let (events, mut receiver) = mpsc::channel(8);
    let mut response = "COMMITTED".to_owned();
    for _ in 0..2 {
        assert!(project_agent_event(
            AgentEvent::ProviderUsageUncertain,
            &run_id,
            &plan,
            &model,
            &mut projection,
            &mut context,
            &events,
            &mut response
        )
        .await
        .unwrap()
        .is_none());
        assert!(projection.usage_uncertain);
        assert!(matches!(
            receiver.try_recv().unwrap().payload,
            EventPayload::ContextUpdated { context } if context.usage_uncertain
        ));
        assert!(receiver.try_recv().is_err());
        assert_eq!(
            projection.assistant_item.as_ref().unwrap().as_str(),
            "stale-assistant"
        );
        assert_eq!(response, "COMMITTED");
    }
    for operation in [
        octet_agent::ProviderOperation::LocalCompaction,
        octet_agent::ProviderOperation::NativeCompaction,
        octet_agent::ProviderOperation::TerminalGate,
    ] {
        for max_attempts in [None, Some(5)] {
            assert!(project_agent_event(
                AgentEvent::ProviderOperationRetry {
                    operation,
                    attempt: 8,
                    max_attempts,
                    delay: std::time::Duration::ZERO,
                    error: "offline".into(),
                },
                &run_id,
                &plan,
                &model,
                &mut projection,
                &mut context,
                &events,
                &mut response
            )
            .await
            .unwrap()
            .is_none());
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            assert_eq!(
                projection.assistant_item.as_ref().unwrap().as_str(),
                "stale-assistant"
            );
            assert_eq!(
                projection.reasoning_item.as_ref().unwrap().as_str(),
                "stale-reasoning"
            );
            assert_eq!(projection.provider_attempt, 1);
            assert_eq!(response, "COMMITTED");
        }
    }
    let outcome = project_agent_event(
        AgentEvent::ProviderRetry {
            attempt: 1,
            max_attempts: 5,
            delay: std::time::Duration::ZERO,
            error: "disconnect".into(),
        },
        &run_id,
        &plan,
        &model,
        &mut projection,
        &mut context,
        &events,
        &mut response,
    )
    .await
    .unwrap();
    assert!(outcome.is_none());
    for expected in ["stale-assistant", "stale-reasoning"] {
        let event = receiver.try_recv().unwrap();
        assert!(
            matches!(event.payload, EventPayload::ItemRetracted { item_id, .. } if item_id.as_str() == expected)
        );
    }
    for attempt in 1..=32 {
        let outcome = project_agent_event(
            AgentEvent::ProviderWaitingForNetwork {
                attempt,
                delay: std::time::Duration::from_secs(30),
                error: "offline".into(),
            },
            &run_id,
            &plan,
            &model,
            &mut projection,
            &mut context,
            &events,
            &mut response,
        )
        .await
        .unwrap();
        assert!(outcome.is_none());
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(projection.provider_attempt, 2);
        assert_eq!(projection.known_entries, 7);
        assert!(projection.assistant_item.is_none());
        assert!(projection.reasoning_item.is_none());
        assert_eq!(
            projection.completed_assistant_items.front(),
            Some(&Some((committed.clone(), turn.clone())))
        );
        assert_eq!(projection.tool_items.get("call"), Some(&tool));
        assert_eq!(response, "COMMITTED");
    }
    projection.finish_turn();
    assert!(projection.usage_uncertain);
    let review = build_completion_review(
        &TerminalProjection::completed(),
        0,
        1,
        &projection,
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
    );
    assert!(review
        .summary
        .contains("known subtotals, not complete totals"));
}

#[test]
fn provider_failure_diagnostics_include_status_and_request_id() {
    let error = AgentError::Ai(AiError::Http(octet_ai::HttpError {
        status: http::StatusCode::BAD_REQUEST,
        request_id: Some("req-400".to_owned()),
        retry_after: None,
        provider_code: Some("invalid_request".to_owned()),
        body_snippet: Some(r#"{"error":{"message":"model does not support this request"}}"#.into()),
        retryable: false,
    }));
    let message = octet_agent::public_error_diagnostic(&error, "custom/e2e", "e2e-model");
    assert!(message.contains("status=400 (bad request)"));
    assert!(message.contains("code=invalid_request"));
    assert!(message.contains("request_id=req-400"));

    let timeout = AgentError::Ai(AiError::Transport(octet_ai::TransportError {
        phase: TransportPhase::Body,
        timeout: true,
        message: "stream idle beyond its timeout".to_owned(),
    }));
    let timeout_message = octet_agent::public_error_diagnostic(&timeout, "custom/e2e", "e2e-model");
    assert!(timeout_message.contains("phase=response body timeout"));
    assert!(timeout_message.contains("detail=stream idle beyond its timeout"));
}

#[test]
fn serve_worker_future_has_bounded_inline_state() {
    let directory = tempfile::tempdir().unwrap();
    let plan = pull_request_worker_plan(directory.path(), "worker-frame");
    let (_commands, receiver) = mpsc::channel(1);
    let (events, _receiver) = mpsc::channel(1);
    let worker = run_worker(plan, receiver, events, 0);
    let bytes = std::mem::size_of_val(&worker);
    // Keeping App inline made this future roughly 96 KiB and its debug poll
    // frame over 1 MiB, exhausting the Tokio stack when polling a provider run.
    assert!(
        bytes < 32 * 1024,
        "Serve worker retains {bytes} bytes inline"
    );
}
