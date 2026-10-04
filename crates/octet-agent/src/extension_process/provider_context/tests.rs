use super::super::tests::{trusted_descriptor, write_executable_script};
use super::*;
use crate::{Agent, AgentConfig, EffectBroker, ExtensionHost, SandboxConfig};
use octet_ai::{
    AssistantMessage, AssistantPart, CacheRetention, Cost, ModelCatalog, ModelId, ReasoningConfig,
    ReasoningMode, Request, Response,
};
use tempfile::TempDir;

#[test]
fn pipeline_header_patch_preserves_repeated_values_and_explicit_deletions() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-remove", reqwest::header::HeaderValue::from_static("old"));
    headers.insert("x-stays", reqwest::header::HeaderValue::from_static("kept"));
    let reply: ProviderPipelineReply = serde_json::from_value(serde_json::json!({
        "provider_headers":{"x-remove":null,"x-multi":["first","second"],"x-new":"inserted"}
    })).unwrap();
    apply_pipeline_headers(&mut headers, reply.provider_headers.unwrap()).unwrap();
    assert!(!headers.contains_key("x-remove"));
    assert_eq!(headers["x-stays"], "kept");
    assert_eq!(headers["x-new"], "inserted");
    assert_eq!(headers.get_all("x-multi").iter().map(|value| value.to_str().unwrap()).collect::<Vec<_>>(), ["first", "second"]);
    assert!(headers["x-new"].is_sensitive());
    let projected = serde_json::to_value(pipeline_header_projection(&headers).unwrap()).unwrap();
    assert_eq!(projected["x-new"], "inserted");
    assert_eq!(projected["x-multi"], serde_json::json!(["first", "second"]));
}

#[test]
fn pipeline_rejects_ambiguous_or_unrepresentable_header_patches_privately() {
    for patch in [
        serde_json::json!({"X-Name":"a", "x-name":"b"}),
        serde_json::json!({"x-name":[]}),
        serde_json::json!({"x-name":"secret\r\ninjected"}),
        serde_json::json!({"invalid name":"private-value"}),
    ] {
        let mut headers = reqwest::header::HeaderMap::new();
        let patch = serde_json::from_value(patch).unwrap();
        let error = apply_pipeline_headers(&mut headers, patch).unwrap_err();
        assert!(!error.to_string().contains("secret"));
        assert!(!error.to_string().contains("private-value"));
    }
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-binary", reqwest::header::HeaderValue::from_bytes(&[0x80]).unwrap());
    assert!(pipeline_header_projection(&headers).is_err());
    assert!(serde_json::from_value::<ProviderPipelineReply>(serde_json::json!({"provider_context":{}})).is_err());
}

fn manifest() -> ExtensionManifest {
    ExtensionManifest::parse("name = \"context-process\"\nversion = \"0.1.0\"\napi_version = \"0.4\"\n[entrypoint]\ncommand = \"extension.py\"\n[contributes]\nhooks = [\"provider_context\"]\n").unwrap()
}
#[test]
fn preparation_hook_requires_api_04_and_negotiated_existing_session_entries() {
    let declared = manifest();
    for version in ["0.1", "0.2", "0.3"] {
        let mut old = declared.clone();
        old.api_version = version.into();
        assert!(old.validate().is_err());
    }
    for selected in [false, true] {
        let mut features = API_0_2_REQUIRED_FEATURES.to_vec();
        if selected {
            features.push(EXTENSION_FEATURE_SESSION_ENTRIES);
        }
        let response: InitializeResponse = serde_json::from_value(serde_json::json!({
            "api_version":"0.4", "protocol":{"version":"0.4", "features": features,
                "limits":{"max_concurrent_requests":1}},
        }))
        .unwrap();
        assert_eq!(
            negotiate_contributions_with_host_services(
                &declared,
                response,
                DEFAULT_PENDING_REQUESTS,
                OfferedHostServices::default()
            )
            .is_ok(),
            selected
        );
    }
}

struct Capture(Arc<StdMutex<Vec<Request>>>);
#[async_trait::async_trait]
impl HostStreamTransport for Capture {
    async fn stream(
        &self,
        model: HostStreamModel,
        request: Request,
        _: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        self.0.lock().unwrap().push(request);
        Ok(Box::pin(futures_util::stream::iter([
            Ok(StreamEvent::Started { response_id: None }),
            Ok(StreamEvent::TextStart { index: 0 }),
            Ok(StreamEvent::TextDelta {
                index: 0,
                delta: "answer".into(),
            }),
            Ok(StreamEvent::TextEnd { index: 0 }),
            Ok(StreamEvent::Finished(Response {
                message: AssistantMessage {
                    model: model.id,
                    protocol: model.protocol,
                    content: vec![AssistantPart::Text("answer".into())],
                },
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                cost: Some(Cost::default()),
                response_id: None,
                responses_output: None,
                deferred: None,
                inference: None,
                diagnostics: Vec::new(),
            })),
        ])))
    }
}

#[cfg(any(unix, windows))]
async fn process(temp: &TempDir, invalid: bool) -> ExtensionProcess {
    let source = r#"#!/usr/bin/env python3
import json, sys
from pathlib import Path

def receive(): return json.loads(sys.stdin.readline())
def send(value): print(json.dumps(value, separators=(",",":")), flush=True)
init = receive()
protocol = init["params"]["protocol"]
assert init["params"]["contributes"]["hooks"] == ["provider_context"]
send({"jsonrpc":"2.0","id":init["id"],"result":{
    "api_version":"0.4","tools":[],"commands":[],
    "protocol":{"version":"0.4","features":protocol["required_features"]+["session_entries"],
        "limits":{"max_concurrent_requests":1}}}})
while True:
    request = receive()
    if request["method"] == "shutdown":
        send({"jsonrpc":"2.0","id":request["id"],"result":{}})
        break
    assert request["method"] == "hook/run", request
    p = request["params"]
    assert p["hook"] == "provider_context"
    grant = p["session_leaf"]
    assert grant["owner"] == p["context"]["resource_owner"]
    assert grant["owner"]["session_id"] == p["payload"]["preparation"]["resource_owner"]
    assert grant["expected_head"] == p["payload"]["preparation"]["head"]
    assert p["payload"]["request"]["system"] == "canonical system"
    host = p["context"]["host"]
    assert host["session_id"] == p["payload"]["preparation"]["session_id"]
    assert host["session_leaf_id"] == grant["expected_head"]
    assert Path(host["session_file"]).is_file()
    assert len(host["session_entries"]) == 1
    assert host["session_branch"] == host["session_entries"]
    assert host["session_entries"][0]["id"] == grant["expected_head"]
    calls = Path("hook-calls")
    calls.write_text(str(int(calls.read_text())+1 if calls.exists() else 1))
    send({"jsonrpc":"2.0","id":100,"method":"session/append_entry","params":{
        "parent_request_id":request["id"],"entry_type":"checkpoint",
        "data":{"revision":1,"text":"private\ncheckpoint"},
        "session_leaf":{"grant_id":grant["grant_id"],"activation_epoch":grant["activation_epoch"],
            "operation_id":grant["operation_id"]}}})
    reply = receive()
    assert reply["id"] == 100 and "result" in reply, reply
    result = reply["result"]
    assert result["entry_id"] == result["head"] == result["successor"]["expected_head"]
    assert result["successor"]["grant_id"] != grant["grant_id"]
    assert result["successor"]["owner"] == grant["owner"]
    projection = {"messages":[{"User":{"content":[{"Text":"effective leaf context"}]}}],"system":None}
    if $INVALID: projection["tools"] = []
    send({"jsonrpc":"2.0","id":request["id"],"result":{"provider_context":projection}})
"#.replace("$INVALID", if invalid { "True" } else { "False" });
    write_executable_script(&temp.path().join("extension.py"), &source);
    ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest()),
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .unwrap()
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn real_run_process_hook_waits_for_original_writer_commit_once_and_refuses_bad_projection_or_cap(
) {
    for case in ["valid", "invalid", "finite"] {
        let temp = TempDir::new().unwrap();
        let process = process(&temp, case == "invalid").await;
        let mut host = ExtensionHost::new();
        process.register(&mut host);
        assert_eq!(host.provider_context_hooks.len(), 1);
        let model = ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("gpt-4o-mini".into()))
            .unwrap();
        let client = octet_ai::AiClient::new();
        let captures = Arc::new(StdMutex::new(Vec::new()));
        client.register_host_stream_transport(
            model.endpoint.id.clone(),
            Arc::new(Capture(captures.clone())),
        );
        let mut agent = Agent::new(AgentConfig {
            client,
            model,
            session: Session::create(temp.path().join("session.jsonl")).unwrap(),
            extensions: host,
            system: "canonical system".into(),
            sandbox: SandboxConfig::new(temp.path()),
            effect_broker: EffectBroker::default(),
            max_turns: Some(1),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: ReasoningMode::Standard,
            cache_retention: CacheRetention::Short,
            session_id: Some("host-affinity".into()),
        })
        .unwrap();
        if case == "finite" {
            agent.set_max_session_cost_microdollars(Some(1_000_000));
        }
        let result =
            tokio::time::timeout(Duration::from_secs(10), agent.complete("canonical task"))
                .await
                .unwrap();
        match case {
            "valid" => assert_eq!(result.unwrap().text, "answer"),
            "invalid" => assert!(matches!(
                result,
                Err(crate::AgentError::ProviderContextPreparation(_))
            )),
            _ => assert!(matches!(
                result,
                Err(crate::AgentError::InputLimitUnavailable)
            )),
        }
        assert_eq!(
            std::fs::read_to_string(temp.path().join("hook-calls")).unwrap(),
            "1"
        );
        assert_eq!(captures.lock().unwrap().len(), usize::from(case == "valid"));
        if case == "valid" {
            let request = &captures.lock().unwrap()[0];
            assert_eq!(request.system, None);
            assert!(serde_json::to_string(&request.messages)
                .unwrap()
                .contains("effective leaf context"));
            assert!(!serde_json::to_string(&request.messages)
                .unwrap()
                .contains("private"));
        }
        let private = &agent.session().entries()[1];
        let checkpoint = agent
            .session()
            .extension_entry(&private.id, "context-process")
            .unwrap();
        assert_eq!(checkpoint.data["text"], "private\ncheckpoint");
        assert!(private
            .metadata
            .as_ref()
            .unwrap()
            .public_extension_metadata()
            .is_empty());
        let reopened = Session::open_read_only(agent.session().path()).unwrap();
        assert_eq!(
            reopened
                .extension_entry(&private.id, "context-process")
                .unwrap()
                .data,
            checkpoint.data
        );
        assert!(agent.session().usage_uncertainty_records().is_empty());
        assert!(process.shutdown().await);
    }
}
