//! Persisted ~150K estimated-token coding session through real agent/tools/HTTP.
//! The scripted model's usage counters are fixtures, NOT cache measurements.

use super::*;
use serde_json::{json, Value};

#[path = "responses.rs"]
mod responses;

struct CacheProbe;

#[async_trait::async_trait]
impl Tool for CacheProbe {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: "cache_probe".into(),
            description: "Native extension diagnostic probe".into(),
            parameters: json!({"type":"object","properties":{}}),
            constrained_sampling: None,
            async_execution: false,
        }
    }
    fn effect(&self, _: &Value, _: &ToolContext<'_>) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Pure)
    }
    async fn execute(&self, _: Value, _: &ToolContext<'_>) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new("extension ready"))
    }
}

fn agent(client: AiClient, model: Model, workspace: &Path, session: Session) -> Agent {
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    extensions.tool(CacheProbe);
    let mut sandbox = SandboxConfig::new(workspace);
    sandbox.allow_edit = true;
    sandbox.allow_process = true;
    sandbox.allow_shell = true;
    Agent::new(AgentConfig {
        client,
        model,
        session,
        system: "You are a coding agent. Preserve source and verify changes.".repeat(32),
        sandbox,
        extensions,
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        max_turns: Some(24),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap()
}

fn unmarked(mut value: Value) -> Value {
    match &mut value {
        Value::Array(values) => {
            for value in values {
                *value = unmarked(value.take());
            }
        }
        Value::Object(object) => {
            object.remove("cache_control");
            // Fixture fields contain no user keys called cache_control. The
            // production observer strips annotations at known boundaries only.
            for value in object.values_mut() {
                *value = unmarked(value.take());
            }
        }
        _ => {}
    }
    value
}

fn ordered(body: &Value) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in [&body["tools"], &body["system"]]
        .into_iter()
        .chain(body["messages"].as_array().unwrap().iter())
    {
        serde_json::to_writer(&mut bytes, &unmarked(value.clone())).unwrap();
        bytes.push(b'\n');
    }
    bytes
}

fn common(a: &[u8], b: &[u8]) -> usize {
    a.iter()
        .zip(b)
        .position(|(a, b)| a != b)
        .unwrap_or(a.len().min(b.len()))
}

#[tokio::test]
async fn persisted_large_coding_session_prefix_audit() {
    let server = MockServer::start().await;
    let mut script = Vec::new();
    for turn in 0..16 {
        let (name, args) = match turn % 4 {
            0 => ("read", json!({"path":"fixture.rs"})),
            1 => (
                "edit",
                json!({"path":"fixture.rs","oldText":format!("value = {}", turn / 4),"newText":format!("value = {}", turn / 4 + 1)}),
            ),
            2 => (
                "bash",
                json!({"command":format!("rustc --crate-type lib fixture.rs -o fixture.rlib && grep -q 'let value = {};' fixture.rs", turn / 4 + 1)}),
            ),
            _ => ("cache_probe", json!({})),
        };
        script.push(tool_turn(&[(&format!("cache_{turn}"), name, args)]));
        if turn == 11 || turn == 15 {
            script.push(text_turn("verified"));
        }
    }
    script.push(text_turn("checkpoint verified"));
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: script,
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().canonicalize().unwrap();
    let path = workspace.join("cache-audit.jsonl");
    std::fs::write(
        workspace.join("fixture.rs"),
        "pub fn fixture() { let value = 0; }\n",
    )
    .unwrap();
    let source = (0..6000).map(|i| format!("fn case_{i:05}() {{ let actual = parser::parse(source_{i:05}); assert_eq!(actual, expected_{i:05}); }}\n")).collect::<String>();
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text(source)],
        })))
        .unwrap();
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            model: ModelId("scripted".into()),
            protocol: Protocol::AnthropicMessages,
            content: vec![AssistantPart::Text(
                "Source reviewed; ready to patch.".into(),
            )],
        })))
        .unwrap();
    let identity = session.resource_owner_key();
    let client = AiClient::new();
    let model = scripted_model(&server.uri());
    let mut running = agent(client.clone(), model.clone(), &workspace, session);
    let result = running
        .complete("Inspect, edit, and test the fixture.")
        .await
        .unwrap();
    assert!(
        matches!(result.reason, FinishReason::Completed),
        "{:?}",
        result.reason
    );
    let before_resume = running.session().context().unwrap();
    drop(running);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(identity, reopened.resource_owner_key());
    assert_eq!(
        serde_json::to_value(before_resume).unwrap(),
        serde_json::to_value(reopened.context().unwrap()).unwrap()
    );
    let mut running = agent(client, model, &workspace, reopened);
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
    for message in running.session().context().unwrap() {
        if let Message::User(user) = message {
            for part in user.content {
                if let UserPart::ToolResult(result) = part {
                    assert!(!result.is_error, "coding tool failed: {result:?}");
                }
            }
        }
    }
    let kept = running.session().entries().iter().rev().find(|entry| matches!(&entry.value, EntryValue::Message(Message::User(user)) if user.content.iter().any(|part| matches!(part, UserPart::Text(_)))))
        .unwrap().id.clone();
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
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 19);
    let bodies: Vec<Value> = requests
        .iter()
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert!(bodies[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "cache_probe"));
    let estimated = ordered(&bodies[0]).len().div_ceil(4);
    assert!((100_000..=200_000).contains(&estimated), "{estimated}");
    let mut read = 0;
    let mut total = 0;
    for index in 1..bodies.len() {
        assert_eq!(bodies[0]["tools"], bodies[index]["tools"]);
        assert_eq!(bodies[0]["system"], bodies[index]["system"]);
        let previous = ordered(&bodies[index - 1]);
        let current = ordered(&bodies[index]);
        let prefix = common(&previous, &current);
        let changed = bodies[index - 1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .zip(bodies[index]["messages"].as_array().unwrap())
            .position(|(a, b)| unmarked(a.clone()) != unmarked(b.clone()));
        println!("PREFIX request={index} raw_lcp={} ordered_lcp={prefix} previous_bytes={} current_bytes={} changed_message={changed:?} resume={} compaction={}", common(&requests[index - 1].body, &requests[index].body), previous.len(), current.len(), index == 13, index == 18);
        if index < 18 {
            assert!(prefix as f64 / current.len() as f64 >= 0.98);
            read += prefix;
            total += current.len();
        } else {
            assert_eq!(
                changed,
                Some(0),
                "compaction is an intentional early rewrite"
            );
        }
    }
    println!("PERSISTED_PREFIX estimated_initial_tokens={estimated} requests={} steady_requests=17 ordered_input_bytes={total} reusable_bytes={read} fresh_est_tokens={} efficiency={:.6}% live_provider_usage=NOT_RUN", bodies.len(), (total-read).div_ceil(4), 100.0*read as f64/total as f64);
}
