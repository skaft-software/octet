//! Persisted coding-session matrix for the shared Responses codec.
//! Loopback model/tier fixtures are not live route or prompt-cache qualification.

use super::*;
use octet_ai::{ResponsesRuntimeProfile, ServiceTier};

fn coding_turn(turn: usize, name: &str, arguments: Value) -> String {
    let output = json!([
        {"type":"reasoning","id":format!("rs_{turn}"),"summary":[],
         "encrypted_content":format!("opaque-reasoning-{turn}"),"future_field":{"keep":turn}},
        {"type":"function_call","id":format!("fc_{turn}"),"call_id":format!("cache_{turn}"),
         "name":name,"arguments":arguments.to_string(),"future_field":{"keep":turn}}
    ]);
    let mut events =
        vec![json!({"type":"response.created","response":{"id":format!("resp_{turn}")}})];
    for (index, item) in output.as_array().unwrap().iter().enumerate() {
        events.push(json!({"type":"response.output_item.added","output_index":index,"item":item}));
        events.push(json!({"type":"response.output_item.done","output_index":index,"item":item}));
    }
    events.push(json!({"type":"response.completed","response":{
        "id":format!("resp_{turn}"),"output":output,
        "usage":{"input_tokens":9,"output_tokens":3,"total_tokens":12}
    }}));
    events
        .into_iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}

fn running_agent(
    client: AiClient,
    model: Model,
    workspace: &Path,
    session: Session,
    flex: bool,
) -> Agent {
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    extensions.tool(CacheProbe);
    let mut sandbox = SandboxConfig::new(workspace);
    sandbox.allow_edit = true;
    sandbox.allow_process = true;
    sandbox.allow_shell = true;
    let mut agent = Agent::new(AgentConfig {
        client,
        model,
        session,
        system: "You are a coding agent. Preserve source and verify changes.".repeat(32),
        sandbox,
        extensions,
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        max_turns: Some(24),
        reasoning: ReasoningConfig::Effort(octet_ai::ReasoningEffort::Medium),
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    if flex {
        agent.set_service_tier(Some(ServiceTier::Flex)).unwrap();
    }
    agent
}

fn ordered(body: &Value) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in [&body["tools"]]
        .into_iter()
        .chain(body["input"].as_array().unwrap())
    {
        serde_json::to_writer(&mut bytes, value).unwrap();
        bytes.push(b'\n');
    }
    bytes
}

#[tokio::test]
async fn persisted_responses_coding_session_prefix_audit() {
    let catalog = octet_ai::ModelCatalog::builtin().unwrap();
    let models: Vec<_> = catalog
        .models()
        .filter(|spec| spec.protocol == Protocol::OpenAiResponses)
        .map(|spec| catalog.resolve(&spec.id).unwrap())
        .collect();
    assert!(models
        .iter()
        .any(|model| model.spec.api_name == "gpt-6-luna"));
    for base in models {
        for flex in [false, true] {
            let server = MockServer::start().await;
            let mut script = Vec::new();
            for turn in 0..16 {
                let (name, args) = match turn % 4 {
                    0 => ("read", json!({"path":"fixture.rs"})),
                    1 => (
                        "edit",
                        json!({"path":"fixture.rs","oldText":format!("value = {}",turn/4),"newText":format!("value = {}",turn/4+1)}),
                    ),
                    2 => (
                        "bash",
                        json!({"command":format!("rustc --crate-type lib fixture.rs -o fixture.rlib && grep -q 'let value = {};' fixture.rs",turn/4+1)}),
                    ),
                    _ => ("cache_probe", json!({})),
                };
                script.push(coding_turn(turn, name, args));
                if turn == 11 || turn == 15 {
                    script.push(responses_text_turn(
                        &format!("done_{turn}"),
                        "verified",
                        "response.completed",
                        "opaque-finish",
                    ));
                }
            }
            script.push(responses_text_turn(
                "done_compacted",
                "checkpoint verified",
                "response.completed",
                "opaque-checkpoint-finish",
            ));
            Mock::given(method("POST"))
                .and(path("responses"))
                .respond_with(Script {
                    bodies: script,
                    next: AtomicUsize::new(0),
                })
                .expect(19)
                .mount(&server)
                .await;
            let directory = tempfile::tempdir().unwrap();
            let workspace = directory.path().canonicalize().unwrap();
            let path = workspace.join("responses-audit.jsonl");
            std::fs::write(
                workspace.join("fixture.rs"),
                "pub fn fixture() { let value = 0; }\n",
            )
            .unwrap();
            let mut source = (0..6000).map(|i| format!("fn case_{i:05}() {{ let actual = parser::parse(source_{i:05}); assert_eq!(actual, expected_{i:05}); }}\n")).collect::<String>();
            if base.spec.limits.context_window < 200_000 {
                source.truncate(400_000);
            }
            let mut session = Session::create(&path).unwrap();
            session
                .append(EntryValue::Message(Message::User(UserMessage {
                    content: vec![UserPart::Text(source)],
                })))
                .unwrap();
            let identity = session.resource_owner_key();
            let mut model = base.clone();
            let endpoint = Arc::make_mut(&mut model.endpoint);
            endpoint.base_url = url::Url::parse(&format!("{}/", server.uri())).unwrap();
            endpoint.auth = Auth::bearer("loopback-key");
            endpoint.transport = octet_ai::EndpointTransport::Http;
            // This fixture declares the existing tier-capable profile, not a new
            // public API Flex contract or live Codex model availability.
            endpoint.runtime.responses_profile = if flex {
                ResponsesRuntimeProfile::Codex
            } else {
                ResponsesRuntimeProfile::Default
            };
            endpoint.runtime.responses_features = octet_ai::ResponsesFeatures::default();
            Arc::make_mut(&mut model.spec).pricing = None;
            let client = AiClient::new();
            let mut running =
                running_agent(client.clone(), model.clone(), &workspace, session, flex);
            assert!(matches!(
                running
                    .complete("Inspect, edit, and test the fixture.")
                    .await
                    .unwrap()
                    .reason,
                FinishReason::Completed
            ));
            let before_resume = running.session().context().unwrap();
            let replay_before = running
                .session()
                .responses_replay_items(&model.endpoint.id, &model.spec.id)
                .unwrap()
                .unwrap();
            drop(running);
            let reopened = Session::open(&path).unwrap();
            assert_eq!(identity, reopened.resource_owner_key());
            assert_eq!(
                serde_json::to_value(before_resume).unwrap(),
                serde_json::to_value(reopened.context().unwrap()).unwrap()
            );
            let replay_after = reopened
                .responses_replay_items(&model.endpoint.id, &model.spec.id)
                .unwrap()
                .unwrap();
            assert_eq!(
                octet_ai::responses::encode_responses_replay(&model, None, &replay_before).unwrap(),
                octet_ai::responses::encode_responses_replay(&model, None, &replay_after).unwrap()
            );
            let mut running = running_agent(client, model.clone(), &workspace, reopened, flex);
            assert!(matches!(
                running
                    .complete("Continue verification.")
                    .await
                    .unwrap()
                    .reason,
                FinishReason::Completed
            ));
            assert!(std::fs::read_to_string(workspace.join("fixture.rs"))
                .unwrap()
                .contains("value = 4"));
            let mut tool_results = 0;
            for message in running.session().context().unwrap() {
                if let Message::User(user) = message {
                    for part in user.content {
                        if let UserPart::ToolResult(result) = part {
                            assert!(!result.is_error, "coding tool failed: {result:?}");
                            tool_results += 1;
                        }
                    }
                }
            }
            assert_eq!(tool_results, 16);
            let kept = running.session().entries().iter().rev().find(|entry| matches!(&entry.value, EntryValue::Message(Message::User(user)) if user.content.iter().any(|part| matches!(part, UserPart::Text(_))))).unwrap().id.clone();
            running
                .session_mut()
                .compact("Source examined; fixture updated and tests passed.", kept)
                .unwrap();
            assert!(matches!(
                running
                    .complete("Verify the checkpoint.")
                    .await
                    .unwrap()
                    .reason,
                FinishReason::Completed
            ));
            let requests = wire_requests(&server).await;
            assert_eq!(requests.len(), 19);
            let estimate = ordered(&requests[0]).len().div_ceil(4);
            assert!((100_000..=200_000).contains(&estimate), "{estimate}");
            let mut reused = 0;
            let mut total = 0;
            let mut minimum = 1.0_f64;
            for (index, current) in requests.iter().enumerate() {
                for key in [
                    "tools",
                    "reasoning",
                    "text",
                    "prompt_cache_key",
                    "prompt_cache_options",
                    "service_tier",
                ] {
                    assert_eq!(
                        current.get(key),
                        requests[0].get(key),
                        "{} flex={flex}: {key}",
                        model.spec.id.0
                    );
                }
                assert_eq!(
                    current.get("service_tier").and_then(Value::as_str),
                    flex.then_some("flex")
                );
                assert!(current.get("previous_response_id").is_none());
                assert_eq!(current["store"], false);
                if (1..18).contains(&index) {
                    let previous = requests[index - 1]["input"].as_array().unwrap();
                    assert_eq!(
                        previous,
                        &current["input"].as_array().unwrap()[..previous.len()],
                        "{} flex={flex}: request={index}",
                        model.spec.id.0
                    );
                    let old_bytes = ordered(&requests[index - 1]);
                    let new_bytes = ordered(current);
                    assert_eq!(common(&old_bytes, &new_bytes), old_bytes.len());
                    let ratio = old_bytes.len() as f64 / new_bytes.len() as f64;
                    assert!(
                        ratio >= 0.98,
                        "{} flex={flex}: request={index} ratio={ratio}",
                        model.spec.id.0
                    );
                    minimum = minimum.min(ratio);
                    reused += old_bytes.len();
                    total += new_bytes.len();
                }
            }
            // Provider-owned reasoning/IDs/unknown fields survive exact replay
            // across reopen; no canonical reconstruction is substituted.
            let input = requests[17]["input"].as_array().unwrap();
            let reasoning: Vec<_> = input
                .iter()
                .filter(|item| item["type"] == "reasoning")
                .collect();
            assert_eq!(reasoning.len(), 16);
            for (turn, item) in reasoning.iter().enumerate() {
                assert_eq!(item["id"], format!("rs_{turn}"));
                assert_eq!(
                    item["encrypted_content"],
                    format!("opaque-reasoning-{turn}")
                );
                assert_eq!(item["future_field"]["keep"], turn);
            }
            assert_ne!(requests[17]["input"][1], requests[18]["input"][1]);
            assert!(ordered(&requests[18]).len() < ordered(&requests[17]).len() / 2);
            println!("PERSISTED_RESPONSES model={} tier={} estimated_initial_tokens={estimate} requests=19 steady_requests=17 ordered_input_bytes={total} reusable_bytes={reused} efficiency={:.6}% minimum={:.6}% live_provider_usage=NOT_RUN", model.spec.api_name, if flex { "declared_codex_flex_fixture" } else { "default_profile_fixture" }, 100.0*reused as f64/total as f64, 100.0*minimum);
        }
    }
}
