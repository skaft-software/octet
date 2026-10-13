//! Negotiated presentation metadata, including the actual Agent/provider boundary.
use super::*;
use pretty_assertions::assert_eq;
use serde_json::{json, Value};

fn definition() -> ToolDefinition {
    serde_json::from_value(json!({
        "name":"metadata_tool", "description":"ordinary tool", "parameters":{"type":"object"},
        "prompt_snippet":"Inspect the project", "prompt_guidelines":["Report verified findings"]
    }))
    .unwrap()
}

fn protocol() -> ExtensionNegotiatedProtocol {
    ExtensionNegotiatedProtocol {
        version: "0.4".into(),
        features: BTreeSet::from([EXTENSION_FEATURE_TOOL_PROMPT_METADATA.into()]),
        max_concurrent_requests: 1,
        lifecycle_events: BTreeSet::new(),
    }
}

#[test]
fn tool_prompt_metadata_is_optional_closed_and_negotiated() {
    let tool = definition();
    validate_tool_definitions_for_protocol(std::slice::from_ref(&tool), &protocol()).unwrap();
    for version in ["0.1", "0.2", "0.3"] {
        assert!(validate_tool_definitions(std::slice::from_ref(&tool), version).is_err());
    }
    let mut absent = protocol();
    absent.features.clear();
    assert!(validate_tool_definitions_for_protocol(std::slice::from_ref(&tool), &absent).is_err());
    for (field, value) in [
        ("prompt_snippet", json!(3)),
        ("prompt_guidelines", json!([3])),
        ("prompt_guidelines", Value::Null),
        ("promptSnippet", json!("wrong wire")),
    ] {
        let mut wire = serde_json::to_value(&tool).unwrap();
        wire[field] = value;
        assert!(serde_json::from_value::<ToolDefinition>(wire).is_err());
    }
    let plain: ToolDefinition = serde_json::from_value(json!({
        "name":"plain", "description":"plain", "parameters":{"type":"object"}
    }))
    .unwrap();
    for version in ["0.1", "0.2", "0.3", "0.4"] {
        validate_tool_definitions(std::slice::from_ref(&plain), version).unwrap();
    }
    let wire = serde_json::to_value(plain).unwrap();
    assert!(wire.get("prompt_snippet").is_none());
    assert!(wire.get("prompt_guidelines").is_none());
}

#[test]
fn tool_prompt_metadata_has_utf8_control_and_count_bounds() {
    let mut tool = definition();
    tool.prompt_snippet = Some("é".repeat(512));
    tool.prompt_guidelines = vec!["\n\t".into(); 16];
    validate_tool_definitions(std::slice::from_ref(&tool), "0.4").unwrap();
    tool.prompt_snippet.as_mut().unwrap().push('x');
    assert!(validate_tool_definitions(std::slice::from_ref(&tool), "0.4").is_err());
    tool.prompt_snippet = None;
    tool.prompt_guidelines.push("extra".into());
    assert!(validate_tool_definitions(std::slice::from_ref(&tool), "0.4").is_err());
    for text in [
        "\u{1b}[31m",
        "a\rb",
        "\u{7f}",
        "\u{85}",
        "\0",
        &"x".repeat(1025),
    ] {
        tool.prompt_guidelines = vec![text.into()];
        assert!(validate_tool_definitions(std::slice::from_ref(&tool), "0.4").is_err());
    }
}

#[test]
fn tool_prompt_metadata_uses_existing_aggregate_catalog_budget() {
    let mut tool = definition();
    let schema_overhead = serde_json::to_vec(&tool.parameters).unwrap().len();
    let metadata_bytes = serde_json::to_vec(&(&tool.prompt_snippet, &tool.prompt_guidelines))
        .unwrap()
        .len();
    // Adding a description key itself consumes 17 JSON bytes.
    tool.parameters["description"] =
        json!("x".repeat(MAX_TOOL_CATALOG_SCHEMA_BYTES - schema_overhead - metadata_bytes - 17));
    validate_tool_definitions(std::slice::from_ref(&tool), "0.4").unwrap();
    tool.prompt_guidelines[0].push('x');
    assert!(validate_tool_definitions(&[tool], "0.4").is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn tool_prompt_metadata_reaches_actual_model_only_for_active_tools() {
    use crate::{Agent, AgentConfig, EffectBroker, SandboxConfig, Session};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let temp = TempDir::new().unwrap();
    write_executable_script(
        &temp.path().join("metadata.py"),
        r#"#!/usr/bin/env python3
import json, sys
send = lambda value: print(json.dumps(value), flush=True)
init = json.loads(sys.stdin.readline())
assert 'tool_prompt_metadata_v1' in init['params']['protocol']['optional_features']
tools = [
 {'name':'listed','description':'listed description','parameters':{'type':'object'},
  'prompt_snippet':'EXPLICIT_SNIPPET','prompt_guidelines':['EXPLICIT_RULE']},
 {'name':'rules_only','description':'rules description','parameters':{'type':'object'},
  'prompt_guidelines':['GUIDELINE_WITHOUT_SNIPPET']},
 {'name':'plain','description':'plain description','parameters':{'type':'object'}}]
send({'jsonrpc':'2.0','id':init['id'],'result':{'api_version':'0.4','tools':tools,'commands':[],
 'protocol':{'version':'0.4','features':['request_cancellation','content_parts','tool_prompt_metadata_v1'],
 'limits':{'max_concurrent_requests':1}}}})
for line in sys.stdin:
 request = json.loads(line)
 if request.get('method') == 'shutdown':
  send({'jsonrpc':'2.0','id':request['id'],'result':{}})
  break
 assert request.get('method') != 'tool/call', 'provider must not execute tools in this test'
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"name = "prompt-metadata"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "metadata.py"
[contributes]
tools = ["listed", "rules_only", "plain"]
"#,
    )
    .unwrap();
    let process = ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest),
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .unwrap();
    let mut host = ExtensionHost::new();
    process.register_dynamic_tool_catalog(&mut host);
    host.finalize_tool_surface();
    let (_, tools) = host.tool_snapshot();
    let selected = tools
        .iter()
        .find(|tool| tool.definition().name == "listed")
        .unwrap();
    // Frozen catalog entries retain their own metadata, not a live name lookup.
    let connection = read_std_lock(&process.inner.connection).clone();
    let mut replacement = process.tool_definitions();
    replacement[0].prompt_snippet = Some("replacement".into());
    let newer = process.process_tools(connection, &replacement);
    assert_eq!(
        selected.prompt_metadata().unwrap().snippet,
        "EXPLICIT_SNIPPET"
    );
    assert_eq!(
        newer.tools[0].prompt_metadata().unwrap().snippet,
        "replacement"
    );

    let server = MockServer::start().await;
    let event = |name: &str, value: Value| format!("event: {name}\ndata: {value}\n\n");
    let body = event(
        "message_start",
        json!({"type":"message_start","message":{"id":"local","usage":{"input_tokens":5,"output_tokens":0}}}),
    ) + &event(
        "content_block_start",
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
    ) + &event(
        "content_block_delta",
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"done"}}),
    ) + &event(
        "content_block_stop",
        json!({"type":"content_block_stop","index":0}),
    ) + &event(
        "message_delta",
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}),
    ) + &event("message_stop", json!({"type":"message_stop"}));
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .expect(4)
        .mount(&server)
        .await;
    let mut model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    Arc::make_mut(&mut model.spec).protocol = octet_ai::Protocol::AnthropicMessages;
    Arc::make_mut(&mut model.endpoint).base_url =
        url::Url::parse(&format!("{}/", server.uri())).unwrap();
    Arc::make_mut(&mut model.endpoint).auth = octet_ai::Auth::bearer("local-scripted-no-inference");
    Arc::make_mut(&mut model.endpoint).transport = octet_ai::EndpointTransport::Http;
    let mut agent = Agent::new(AgentConfig {
        client: octet_ai::AiClient::new(),
        model,
        session: Session::create(temp.path().join("session.jsonl")).unwrap(),
        system: "unchanged base".into(),
        sandbox: SandboxConfig::new(temp.path()),
        effect_broker: EffectBroker::default(),
        extensions: host,
        max_turns: Some(1),
        reasoning: octet_ai::ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    assert!(!agent.tool_prompt_section_enabled());
    assert_eq!(agent.complete("inspect").await.unwrap().text, "done");
    agent
        .set_active_tool_names(Some(BTreeSet::from(["plain".into()])))
        .unwrap();
    agent.complete("plain").await.unwrap();
    agent
        .set_active_tool_names(Some(BTreeSet::from(["rules_only".into()])))
        .unwrap();
    agent.complete("rules").await.unwrap();
    {
        let mut run = agent.prompt_without_tools("answer only").await.unwrap();
        while let Some(event) = run.next().await {
            assert!(!matches!(
                event,
                crate::AgentEvent::RunFinished {
                    reason: crate::FinishReason::Failed(_),
                    ..
                }
            ));
        }
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 4);
    let systems: Vec<_> = requests
        .iter()
        .map(|r| r.body_json::<Value>().unwrap()["system"].to_string())
        .collect();
    assert!(systems[0].contains("EXPLICIT_SNIPPET"));
    assert!(systems[0].contains("EXPLICIT_RULE"));
    assert!(systems[0].contains("GUIDELINE_WITHOUT_SNIPPET"));
    assert!(!systems[1].contains("EXPLICIT_"));
    assert!(!systems[1].contains("GUIDELINE_"));
    assert!(!systems[1].contains("Available tools:"));
    assert!(systems[2].contains("GUIDELINE_WITHOUT_SNIPPET"));
    assert!(!systems[2].contains("EXPLICIT_"));
    assert!(!systems[3].contains("Available tools:"));
    assert!(process.shutdown().await);
}
