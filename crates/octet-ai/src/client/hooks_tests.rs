use super::*;
use crate::{CacheRetention, Protocol};
use serde_json::{json, Value};

struct Echo(bool);
impl crate::PayloadHook for Echo {
    fn on_payload(
        &self,
        mut payload: Value,
        _: &HookModelContext,
    ) -> Result<Option<Value>, AiError> {
        if self.0 {
            payload["temperature"] = json!(0.25);
        }
        Ok(Some(payload))
    }
}

#[async_trait::async_trait]
impl ProviderRequestHook for Echo {
    async fn before_request(
        &self,
        _: &ProviderRequestContext,
        mut payload: Value,
    ) -> Result<Option<Value>, AiError> {
        if self.0 {
            payload["temperature"] = json!(0.25);
        }
        Ok(Some(payload))
    }
}

#[tokio::test]
async fn payload_hooks_preserve_final_wire_prefix_across_eight_turns() {
    // Pi's final payload seams: packages/ai/src/api/{anthropic-messages.ts:656,
    // openai-responses.ts:211, openai-codex-responses.ts:283,
    // google-generative-ai.ts:96}. Native HTTP owns byte preservation here.
    for (lane, protocol) in [
        Protocol::AnthropicMessages,
        Protocol::OpenAiChat,
        Protocol::OpenAiResponses,
        Protocol::GoogleGenerativeAi,
        Protocol::OpenAiResponses,
    ]
    .into_iter()
    .enumerate()
    {
        let catalog = crate::ModelCatalog::builtin().unwrap();
        let id = catalog.models().next().unwrap().id.clone();
        let mut model = catalog.resolve(&id).unwrap();
        let spec = Arc::make_mut(&mut model.spec);
        spec.protocol = protocol;
        spec.api_name = "fixture-model".into();
        spec.capabilities.reasoning = None;
        spec.preset = Default::default();
        let endpoint = Arc::make_mut(&mut model.endpoint);
        endpoint.runtime = Default::default();
        if lane == 4 {
            endpoint.runtime.responses_profile = crate::ResponsesRuntimeProfile::Codex;
        }
        endpoint.auth = crate::Auth::None;
        endpoint.base_url = url::Url::parse("https://provider.example/v1beta/").unwrap();
        let mut request = crate::test_fixtures::base_request();
        request.cache_retention = CacheRetention::None;
        request.system = Some("stable system".into());
        request.reasoning = crate::ReasoningConfig::Off;
        request.tools = vec![crate::ToolDef {
            name: "read".into(),
            description: "inspect source".into(),
            parameters: json!({"type":"object","properties":{"z":{"type":"string"},"a":{"type":"number"}}}),
            constrained_sampling: None,
            async_execution: false,
        }];
        request
            .messages
            .push(crate::Message::User(crate::UserMessage {
                content: vec![crate::UserPart::Text("stable first turn".into())],
            }));
        let mut stable_wire = None;
        for turn in 0..8 {
            let parts = match protocol {
                Protocol::AnthropicMessages => {
                    crate::protocol::anthropic::build_request(&model, &request)
                }
                Protocol::OpenAiChat => {
                    crate::protocol::openai_chat::build_request(&model, &request)
                }
                Protocol::OpenAiResponses => {
                    crate::protocol::openai_responses::build_request(&model, &request)
                }
                Protocol::GoogleGenerativeAi => {
                    crate::protocol::google::build_request(&model, &request)
                }
                _ => unreachable!(),
            }
            .unwrap();
            let echo: Arc<dyn crate::PayloadHook> = Arc::new(Echo(false));
            let unchanged = apply_payload_hook(&echo, &model, parts.body.clone()).unwrap();
            assert_eq!(unchanged, parts.body, "{protocol:?} turn {turn}");
            let change: Arc<dyn crate::PayloadHook> = Arc::new(Echo(true));
            let changed = apply_payload_hook(&change, &model, parts.body.clone()).unwrap();
            let pipeline = ProviderRequestAttempt::new(
                &model,
                &[Arc::new(Echo(false)), Arc::new(Echo(true))],
                &[],
            )
            .unwrap();
            assert_eq!(pipeline.payload(parts.body.clone()).await.unwrap(), changed);
            let fields: std::collections::BTreeMap<String, &serde_json::value::RawValue> =
                serde_json::from_slice(&changed).unwrap();
            let history = fields
                .get("messages")
                .or_else(|| fields.get("input"))
                .or_else(|| fields.get("contents"))
                .unwrap();
            let items: Vec<&serde_json::value::RawValue> =
                serde_json::from_str(history.get()).unwrap();
            let prefix = ["system", "systemInstruction", "instructions", "tools"]
                .into_iter()
                .filter_map(|key| fields.get(key).map(|value| (key, value.get().to_owned())))
                .chain(std::iter::once(("first_turn", items[0].get().to_owned())))
                .collect::<Vec<_>>();
            if let Some(previous) = &stable_wire {
                assert_eq!(previous, &prefix, "{protocol:?} turn {turn}");
            }
            stable_wire = Some(prefix);
            let original = std::str::from_utf8(&parts.body).unwrap();
            let changed = std::str::from_utf8(&changed).unwrap();
            // Adding a control cannot rewrite/reorder the stable encoded body.
            assert!(
                changed.starts_with(original.strip_suffix('}').unwrap()),
                "{protocol:?} turn {turn}"
            );
            request
                .messages
                .push(crate::Message::Assistant(crate::AssistantMessage {
                    content: vec![crate::AssistantPart::Text(format!("answer {turn}"))],
                    model: model.spec.id.clone(),
                    protocol,
                }));
            request
                .messages
                .push(crate::Message::User(crate::UserMessage {
                    content: vec![crate::UserPart::Text(format!("next {turn}"))],
                }));
        }
    }
}
