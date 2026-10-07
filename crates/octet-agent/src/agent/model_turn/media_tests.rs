//! Chat-completions image tool results in the exact model-turn observation.
//! The committed entry for an OpenAiChat image result is a tool result plus its
//! protocol-adjacent media part; `model_turn_end` must carry that durable record
//! unchanged, because it is the payload the Pi adapter converts into a Pi
//! `turn_end` event (Pi keeps the image inside the tool result message).
use super::*;
use crate::compaction::{
    SessionOperationDecision, SessionOperationFuture, SessionOperationInvocation,
};
use octet_ai::{FauxMessage, FauxOptions, FauxProvider, FauxResponse, FauxToolCall, ModalitySet};

/// A deterministic 1x1-independent PNG payload standing in for a rendered frame.
const FRAME_BYTES: &[u8] = b"\x89PNG\r\n\x1a\npi-doom-frame";
/// The durable entry value the adapter must convert, pinned in a fixture the
/// adapter suite reads too (extensions/octet-pi-compat/test/model-turns.test.mjs).
const ENTRY_FIXTURE: &str = include_str!(
    "../../../../../extensions/octet-pi-compat/test/fixtures/model-turn-image-entry.json"
);

struct Capture(Arc<Mutex<Vec<serde_json::Value>>>);
struct Continue;
impl SessionOperationInvocation for Continue {
    fn take_future(&mut self) -> SessionOperationFuture {
        Box::pin(async { Ok(SessionOperationDecision::Continue) })
    }
}
impl SessionOperationHook for Capture {
    fn begin(
        &self,
        _: &Session,
        operation: &SessionOperation,
    ) -> Result<Option<Box<dyn SessionOperationInvocation>>, String> {
        self.0
            .lock()
            .unwrap()
            .push(serde_json::to_value(operation).unwrap());
        Ok(Some(Box::new(Continue)))
    }
}

struct Frame;
#[async_trait::async_trait]
impl Tool for Frame {
    fn definition(&self) -> ToolDef {
        ToolDef {
            name: "frame".into(),
            description: "Render one fullscreen frame".into(),
            parameters: serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
            constrained_sampling: None,
            async_execution: false,
        }
    }
    fn effect(&self, _: &serde_json::Value, _: &ToolContext<'_>) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Pure)
    }
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Parallel
    }
    async fn execute(
        &self,
        _: serde_json::Value,
        _: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        Ok(
            ToolOutput::new("frame rendered").with_media(Media::image_bytes(
                bytes::Bytes::from_static(FRAME_BYTES),
                "image/png".parse().unwrap(),
            )),
        )
    }
}

#[tokio::test]
async fn chat_completions_image_tool_results_reach_turn_end_with_their_protocol_media() {
    let dir = tempfile::tempdir().unwrap();
    let provider = FauxProvider::new(FauxOptions::default());
    provider.set_responses(vec![
        FauxResponse::Message(
            FauxMessage::new("drawing").with_tool_call(FauxToolCall::with_id(
                "frame-call",
                "frame",
                serde_json::json!({}),
            )),
        ),
        FauxResponse::Message(FauxMessage::new("done")),
    ]);
    // The chat-completions wire cannot carry media inside a tool message, so the
    // lowering only accepts the image when the selected model reads images.
    let mut model = provider.model().clone();
    Arc::make_mut(&mut model.spec).capabilities.input_modalities =
        ModalitySet::none().with(Modality::Image);
    let client = AiClient::new();
    provider.register(&client);
    let mut host = ExtensionHost::new();
    host.tool(Frame);
    let seen = Arc::new(Mutex::new(Vec::new()));
    host.session_operation_hook(Capture(seen.clone()));
    let mut agent = Agent::new(AgentConfig {
        client,
        model,
        extensions: host,
        session: Session::create(dir.path().join("session.jsonl")).unwrap(),
        system: "test".into(),
        sandbox: SandboxConfig::new(dir.path()),
        effect_broker: EffectBroker::default(),
        max_turns: Some(5),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: None,
    })
    .unwrap();

    let output = agent.complete("draw a frame").await.unwrap();
    assert!(output.text.ends_with("done"), "{}", output.text);
    assert_eq!(provider.state().call_count, 2);

    let entries = agent.session().entries().to_vec();
    let results = entries
        .iter()
        .filter(|entry| {
            matches!(&entry.value,
                EntryValue::Message(Message::User(user))
                    if user.content.iter().any(|part| matches!(part, UserPart::ToolResult(_))))
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 1, "one committed tool result entry");
    let result = results[0];
    let expected: serde_json::Value = serde_json::from_str(ENTRY_FIXTURE).unwrap();
    // Chat completions lower the image beside the tool result, not inside it.
    assert_eq!(serde_json::to_value(&result.value).unwrap(), expected);
    let EntryValue::Message(Message::User(user)) = &result.value else {
        unreachable!("filtered tool result entry");
    };
    let UserPart::Media(Media::Image(image)) = &user.content[1] else {
        panic!("adjacent tool media must stay a Pi-convertible image part");
    };
    assert!(matches!(
        &image.source,
        ImageSource::Inline(bytes) if bytes.as_ref() == FRAME_BYTES
    ));

    // The observation is the durable record itself: same id, same value.
    let seen = seen.lock().unwrap();
    let end = seen
        .iter()
        .find(|value| value["kind"] == "model_turn_end")
        .expect("model turn end observation");
    assert_eq!(end["turn_index"], serde_json::json!(0));
    assert_eq!(end["tool_result_entries"].as_array().unwrap().len(), 1);
    assert_eq!(
        end["tool_result_entries"][0]["id"],
        serde_json::json!(result.id.0)
    );
    assert_eq!(end["tool_result_entries"][0]["value"], expected);
    // The assistant entry also carries the matching call, so the adapter can
    // pair the result without inventing identities.
    let calls = end["assistant_entry"]["value"]["Assistant"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|part| part["ToolCall"]["id"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(calls, ["frame-call"]);
}
