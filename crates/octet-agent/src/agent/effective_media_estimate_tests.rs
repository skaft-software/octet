//! A provider-context hook selects the effective-request estimator. Canonical
//! media carried by an explicit Responses replay window must use the same
//! semantic allowance there as in the extension-free estimator, while opaque
//! provider content stays byte-counted.

use super::*;
use std::sync::atomic::AtomicUsize;

/// Deterministic incompressible bytes the size of the reported screenshot
/// fixture (110,882 raw bytes, 147,844 base64 characters).
fn screenshot_bytes(seed: u8) -> bytes::Bytes {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64 ^ u64::from(seed);
    bytes::Bytes::from(
        (0..110_882)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect::<Vec<_>>(),
    )
}

fn screenshot(seed: u8) -> Media {
    Media::image_bytes(screenshot_bytes(seed), "image/png".parse().unwrap())
}

fn responses_model() -> Model {
    let mut model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    Arc::make_mut(&mut model.spec).limits.context_window = 32_768;
    Arc::make_mut(&mut model.spec).limits.max_output_tokens = 1_024;
    model
}

/// A persisted local compaction whose retained tail holds one screenshot.
fn compacted_session_with_retained_image(session: &mut Session, image: Media) {
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("summarized away".into())],
        })))
        .unwrap();
    let kept = session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![
                UserPart::Text("retained screenshot".into()),
                UserPart::Media(image),
            ],
        })))
        .unwrap();
    session.compact("earlier work summary", kept).unwrap();
}

fn explicit_request(session: &Session, model: &Model) -> Request {
    let exact = exact_responses_replay(session, model, "system").expect("route-affine replay");
    Request {
        system: Some("system".into()),
        messages: session.context().unwrap(),
        tools: Vec::new(),
        tool_choice: Default::default(),
        max_output_tokens: Some(1_024),
        temperature: None,
        stop: Vec::new(),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        responses: Some(octet_ai::ResponsesOptions::full_replay(exact.input)),
        output_format: OutputFormat::Text,
        output_modalities: Default::default(),
        compatibility: CompatibilityMode::Strict,
        cache_retention: CacheRetention::Short,
        session_id: None,
    }
}

#[test]
fn explicit_replay_window_counts_canonical_media_semantically() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let model = responses_model();
    compacted_session_with_retained_image(&mut session, screenshot(1));
    let request = explicit_request(&session, &model);
    let wire = serde_json::to_string(&request.responses).unwrap();
    assert!(
        wire.len() > 147_844,
        "the replay window carries the payload"
    );

    let estimate = effective_request_estimate(&request);
    let canonical = estimate_request_tokens("system", &request.messages, &request.tools);
    assert!(estimate >= ESTIMATED_IMAGE_TOKENS, "{estimate}");
    assert!(
        estimate < 4_000,
        "a replayed screenshot was sized as base64 text: {estimate}"
    );
    assert!(estimate >= canonical, "{estimate} < {canonical}");
}

#[test]
fn explicit_replay_window_keeps_opaque_and_unmatched_payloads_charged() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let model = responses_model();
    compacted_session_with_retained_image(&mut session, screenshot(1));
    let request = explicit_request(&session, &model);
    let baseline = effective_request_estimate(&request);

    let canonical_payload =
        base64::engine::general_purpose::STANDARD.encode(screenshot_bytes(1).as_ref());
    let foreign_payload =
        base64::engine::general_purpose::STANDARD.encode(screenshot_bytes(2).as_ref());
    for (label, payload) in [
        // A second copy of a canonical payload: each canonical item
        // discounts at most one representation.
        ("duplicate canonical", canonical_payload),
        // A data URL with an image-shaped tag that no canonical media backs.
        ("unmatched", foreign_payload),
    ] {
        let mut items = request
            .responses
            .as_ref()
            .and_then(|options| options.input.clone())
            .unwrap()
            .into_items();
        items.push(
            octet_ai::ResponsesItem::new(serde_json::json!({
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_image",
                    "image_url": format!("data:image/png;base64,{payload}"),
                }],
            }))
            .unwrap(),
        );
        let mut opaque = request.clone();
        opaque.responses = Some(octet_ai::ResponsesOptions::full_replay(
            octet_ai::ResponsesInput::new(items),
        ));
        let estimate = effective_request_estimate(&opaque);
        assert!(
            estimate >= baseline + 147_844 / 4,
            "{label} payload was exempted from byte sizing: {estimate} vs {baseline}"
        );
    }
}

struct NoopContextHook(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl crate::extension::ProviderContextHook for NoopContextHook {
    async fn project_context(
        &self,
        _: &Request,
        _: &crate::extension::ProviderContextProjectionContext,
    ) -> Result<Option<crate::extension::ProviderContextProjection>, String> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(None)
    }
}

struct CountingProvider(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl octet_ai::HostStreamTransport for CountingProvider {
    async fn stream(
        &self,
        model: octet_ai::HostStreamModel,
        _: Request,
        _: Vec<octet_ai::Diagnostic>,
    ) -> Result<octet_ai::ResponseStream, AiError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(Box::pin(futures_util::stream::iter(vec![
            Ok(StreamEvent::Started { response_id: None }),
            Ok(StreamEvent::Finished(octet_ai::Response {
                message: AssistantMessage {
                    content: vec![AssistantPart::Text("answer".into())],
                    model: model.id,
                    protocol: model.protocol,
                },
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                cost: Some(octet_ai::Cost::default()),
                response_id: None,
                responses_output: None,
                deferred: None,
                inference: None,
                diagnostics: Vec::new(),
            })),
        ])))
    }
}

async fn run_after_compaction(hooked: bool, mode: AgentCompactionMode) -> (usize, usize) {
    let workspace = tempfile::tempdir().unwrap();
    let model = responses_model();
    let calls = Arc::new(AtomicUsize::new(0));
    let projections = Arc::new(AtomicUsize::new(0));
    let client = AiClient::new();
    client.register_host_stream_transport(
        model.endpoint.id.clone(),
        Arc::new(CountingProvider(calls.clone())),
    );
    let mut host = ExtensionHost::new();
    if hooked {
        host.provider_context_hook(NoopContextHook(projections.clone()));
    }
    let mut session = Session::create(workspace.path().join("session.jsonl")).unwrap();
    compacted_session_with_retained_image(&mut session, screenshot(1));
    let mut agent = Agent::new(AgentConfig {
        client,
        model,
        session,
        extensions: host,
        system: "test system".into(),
        sandbox: SandboxConfig::new(workspace.path()),
        effect_broker: EffectBroker::default(),
        max_turns: Some(1),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: Some("effective-media-estimate".into()),
    })
    .unwrap();
    agent.set_compaction_token_mode(mode, 0.8, 1).unwrap();
    agent
        .complete("next prompt")
        .await
        .expect("a retained screenshot fits the context window");
    (
        calls.load(Ordering::Relaxed),
        projections.load(Ordering::Relaxed),
    )
}

#[tokio::test]
async fn noop_context_hook_admits_retained_media_after_compaction() {
    for mode in [AgentCompactionMode::Disabled, AgentCompactionMode::Local] {
        assert_eq!(run_after_compaction(false, mode).await, (1, 0), "{mode:?}");
        let (calls, projections) = run_after_compaction(true, mode).await;
        assert_eq!(
            calls, 1,
            "{mode:?}: the hooked request must reach the provider"
        );
        assert!(
            projections >= 1,
            "{mode:?}: the hook path must be exercised"
        );
    }
}
