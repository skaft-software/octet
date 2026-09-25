//! Public run-control contracts for retractable live steering.

use std::sync::Arc;
use std::time::Duration;

use octet_agent::{
    Agent, AgentConfig, AgentError, AgentEvent, EffectBroker, ExtensionHost, SandboxConfig, Session,
};
use octet_ai::{
    AiClient, AiError, AssistantMessage, AssistantPart, CacheRetention, Diagnostic,
    HostStreamModel, HostStreamTransport, Message, Model, ModelCatalog, ModelId, Request, Response,
    ResponseStream, StopReason, StreamEvent, Usage, UserPart,
};

struct FinishedTransport;

#[async_trait::async_trait]
impl HostStreamTransport for FinishedTransport {
    async fn stream(
        &self,
        model: HostStreamModel,
        _request: Request,
        _diagnostics: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        let response = Response {
            message: AssistantMessage {
                content: vec![AssistantPart::Text("done".into())],
                model: model.id,
                protocol: model.protocol,
            },
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            cost: None,
            response_id: None,
            responses_output: None,
            deferred: None,
            diagnostics: Vec::new(),
        };
        Ok(Box::pin(futures_util::stream::iter(vec![
            Ok(StreamEvent::Started { response_id: None }),
            Ok(StreamEvent::TextStart { index: 0 }),
            Ok(StreamEvent::TextDelta {
                index: 0,
                delta: "done".into(),
            }),
            Ok(StreamEvent::TextEnd { index: 0 }),
            Ok(StreamEvent::Finished(response)),
        ])))
    }
}

fn test_model() -> Model {
    ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap()
}

fn test_agent() -> (Agent, tempfile::TempDir) {
    let model = test_model();
    let client = AiClient::new();
    client.register_host_stream_transport(model.endpoint.id.clone(), Arc::new(FinishedTransport));
    let workspace = tempfile::tempdir().unwrap();
    let agent = Agent::new(AgentConfig {
        client,
        model,
        session: Session::create(workspace.path().join("retractable-steering.jsonl")).unwrap(),
        system: "test".into(),
        sandbox: SandboxConfig::new(workspace.path()),
        effect_broker: EffectBroker::default(),
        extensions: ExtensionHost::new(),
        max_turns: Some(4),
        reasoning: octet_ai::ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: CacheRetention::default(),
        session_id: None,
    })
    .unwrap();
    (agent, workspace)
}

async fn drain(run: &mut octet_agent::Run<'_>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = run.next().await {
        events.push(event);
    }
    events
}

fn durable_user_texts(agent: &Agent) -> Vec<String> {
    agent
        .session()
        .context()
        .unwrap()
        .into_iter()
        .filter_map(|message| match message {
            Message::User(user) => Some(
                user.content
                    .into_iter()
                    .filter_map(|part| match part {
                        UserPart::Text(text) => Some(text),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn recalled_before_send_is_a_noop_without_a_durable_append() {
    let (mut agent, _workspace) = test_agent();
    let mut run = agent.prompt("initial").await.unwrap();
    let control = run.control();
    let (prepared, receipt) = control.prepare_steer("recalled before send").unwrap();

    assert!(receipt.try_retract());
    control.steer_retractable(prepared).await.unwrap();
    let events = drain(&mut run).await;
    drop(run);

    assert!(!receipt.is_pending());
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::SteeringDelivered { .. })));
    assert_eq!(durable_user_texts(&agent), vec!["initial"]);
}

#[tokio::test]
async fn recalled_after_send_before_the_safe_boundary_has_no_delivery_or_duplicate() {
    let (mut agent, _workspace) = test_agent();
    let mut run = agent.prompt("initial").await.unwrap();
    let control = run.control();
    let (prepared, receipt) = control.prepare_steer("recalled pending").unwrap();

    control.steer_retractable(prepared).await.unwrap();
    assert!(receipt.try_retract());
    let events = drain(&mut run).await;
    drop(run);

    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::SteeringDelivered { .. })));
    assert_eq!(durable_user_texts(&agent), vec!["initial"]);
}

#[tokio::test]
async fn identical_steering_receipts_are_independent_and_only_unrecalled_text_delivers() {
    let (mut agent, _workspace) = test_agent();
    let mut run = agent.prompt("initial").await.unwrap();
    let control = run.control();
    let (first, first_receipt) = control.prepare_steer("same text").unwrap();
    let (second, second_receipt) = control.prepare_steer("same text").unwrap();

    control.steer_retractable(first).await.unwrap();
    control.steer_retractable(second).await.unwrap();
    assert!(first_receipt.try_retract());
    assert!(second_receipt.is_pending());
    let events = drain(&mut run).await;
    drop(run);

    let delivered = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::SteeringDelivered { messages } => Some(messages.as_slice()),
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(delivered, vec!["same text"]);
    assert_eq!(durable_user_texts(&agent), vec!["initial", "same text"]);
    assert!(!second_receipt.is_pending());
    assert!(!second_receipt.try_retract());
}

#[tokio::test]
async fn recall_and_drop_release_reserved_control_capacity() {
    let (mut agent, _workspace) = test_agent();
    let run = agent.prompt("initial").await.unwrap();
    let control = run.control();
    let reservations = (0..64)
        .map(|_| control.prepare_steer("reserved").unwrap())
        .collect::<Vec<_>>();
    assert!(matches!(
        control.prepare_steer("full"),
        Err(AgentError::ControlQueueFull)
    ));

    assert!(reservations[0].1.try_retract());
    let replacement = control.prepare_steer("replacement").unwrap();
    assert!(matches!(
        control.prepare_steer("full again"),
        Err(AgentError::ControlQueueFull)
    ));
    drop(replacement);
    control.prepare_steer("released after drop").unwrap();

    drop(reservations);
    drop(run);
}

#[tokio::test]
async fn blocked_retractable_send_wakes_when_its_receipt_is_recalled() {
    let (mut agent, _workspace) = test_agent();
    let run = agent.prompt("initial").await.unwrap();
    let control = run.control();
    for _ in 0..8 {
        let (prepared, _) = control.prepare_steer("fills ingress").unwrap();
        control.steer_retractable(prepared).await.unwrap();
    }
    let (prepared, receipt) = control.prepare_steer("blocked ingress").unwrap();
    let send = control.steer_retractable(prepared);
    tokio::pin!(send);
    tokio::select! {
        result = &mut send => panic!("send unexpectedly completed: {result:?}"),
        _ = tokio::time::sleep(Duration::from_millis(10)) => {}
    }

    assert!(receipt.try_retract());
    tokio::time::timeout(Duration::from_millis(100), &mut send)
        .await
        .expect("receipt recall wakes the blocked send")
        .unwrap();
    assert!(!receipt.is_pending());
    drop(run);
}

#[tokio::test]
async fn run_end_rejects_a_prepared_submission_and_clears_its_receipt() {
    let (mut agent, _workspace) = test_agent();
    let mut run = agent.prompt("initial").await.unwrap();
    let control = run.control();
    let (prepared, receipt) = control.prepare_steer("run ended").unwrap();
    let _ = drain(&mut run).await;
    drop(run);

    assert!(matches!(
        control.steer_retractable(prepared).await,
        Err(AgentError::RunEnded)
    ));
    assert!(!receipt.is_pending());
    assert!(!receipt.try_retract());
    assert_eq!(durable_user_texts(&agent), vec!["initial"]);
}
