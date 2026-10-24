#![allow(missing_docs)]

//! Byte transport, not audio-understanding qualification: real read/tool/session/
//! Google HTTP paths against a scripted loopback endpoint, with synthetic media.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use base64::Engine;
use octet_agent::{
    Agent, AgentConfig, AgentEvent, CoreTools, EffectBroker, EffectPolicy, ExtensionHost,
    FinishReason, SandboxConfig, Session,
};
use octet_ai::{
    AiClient, Auth, Capabilities, Endpoint, EndpointId, Modality, ModalitySet, Model, ModelId,
    ModelLimits, ModelSpec, Protocol,
};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

struct Script(AtomicUsize);

impl Respond for Script {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        let part = if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            json!({"functionCall": {"name": "read", "id": "read-audio", "args": {"path": "clip.mp3"}}})
        } else {
            json!({"text": "Received the audio."})
        };
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(format!("data: {}\n\n", json!({
                "candidates": [{"index": 0, "content": {"parts": [part]}, "finishReason": "STOP"}],
                "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 4, "totalTokenCount": 14}
            })))
    }
}

fn model(base_url: &str) -> Model {
    Model {
        spec: Arc::new(ModelSpec {
            preset: Default::default(),
            id: ModelId("gemini-audio-fixture".into()),
            endpoint: EndpointId("google-fixture".into()),
            api_name: "gemini-4-argon".into(),
            display_name: None,
            protocol: Protocol::GoogleGenerativeAi,
            capabilities: Capabilities {
                input_modalities: ModalitySet::none().with(Modality::Audio),
                output_modalities: ModalitySet::none(),
                tools: true,
                parallel_tool_calls: true,
                reasoning: None,
                responses_lite: false,
                agent_delegation: None,
                structured_output: false,
                deferred_tool_loading: false,
                responses_features: Default::default(),
            },
            limits: ModelLimits {
                context_window: 1_048_576,
                max_output_tokens: 65_536,
            },
            pricing: None,
            cache: Default::default(),
        }),
        endpoint: Arc::new(Endpoint {
            id: EndpointId("google-fixture".into()),
            base_url: url::Url::parse(&format!("{base_url}/v1beta/")).unwrap(),
            auth: Auth::None,
            default_headers: Default::default(),
            transport: Default::default(),
            runtime: Default::default(),
            timeout: Duration::from_secs(10),
        }),
    }
}

fn agent(model: Model, session: Session, workspace: &std::path::Path) -> Agent {
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    Agent::new(AgentConfig {
        client: AiClient::new(),
        model,
        session,
        system: "Read the requested audio.".into(),
        sandbox: SandboxConfig::new(workspace),
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns: Some(4),
        reasoning: octet_ai::ReasoningConfig::Off,
        reasoning_mode: Default::default(),
        cache_retention: Default::default(),
        session_id: None,
    })
    .unwrap()
}

#[tokio::test]
async fn mp3_read_reaches_google_inline_data_and_replays_after_reopen() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-4-argon:streamGenerateContent"))
        .respond_with(Script(AtomicUsize::new(0)))
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let session_path = sessions.path().join("audio.jsonl");
    // Same byte size as the reported MP3. No private/user audio is committed.
    let mut bytes = vec![0; 5_113_964];
    bytes[..3].copy_from_slice(b"ID3");
    std::fs::write(workspace.path().join("clip.mp3"), &bytes).unwrap();
    let selected = model(&server.uri());
    let mut first = agent(
        selected.clone(),
        Session::create(&session_path).unwrap(),
        workspace.path(),
    );
    assert_eq!(
        first.complete("Listen to clip.mp3").await.unwrap().text,
        "Received the audio."
    );
    drop(first);
    let mut resumed = agent(
        selected,
        Session::open(&session_path).unwrap(),
        workspace.path(),
    );
    resumed.complete("Compare it again.").await.unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    let expected = json!({"mimeType": "audio/mp3", "data": base64::engine::general_purpose::STANDARD.encode(&bytes)});
    for request in &requests[1..] {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let parts = body["contents"][2]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["functionResponse"]["name"], "read");
        assert_eq!(parts[0]["functionResponse"]["id"], "read-audio");
        assert!(parts[0]["functionResponse"]["response"]["output"]
            .as_str()
            .unwrap()
            .contains("read=audio"));
        assert_eq!(parts[1]["inlineData"], expected);
    }
}

struct ReplayScript(AtomicUsize);

impl Respond for ReplayScript {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        let step = self.0.fetch_add(1, Ordering::SeqCst);
        let chunks = if step < 2 {
            vec![
                vec![
                    json!({"text": format!("before-{step}"), "thoughtSignature": format!("before-sig-{step}")}),
                ],
                vec![
                    json!({"functionCall": {"id": format!("read-{step}"), "name": "read", "args": {"path": format!("clip-{step}.mp3")}}, "thoughtSignature": format!("call-sig-{step}")}),
                ],
                vec![
                    json!({"text": format!("after-{step}"), "thoughtSignature": format!("after-sig-{step}")}),
                ],
            ]
        } else {
            vec![
                vec![
                    json!({"text": "planning", "thought": true}),
                    json!({"text": "Both "}),
                ],
                vec![json!({"text": "clips received."})],
                vec![json!({"text": "", "thoughtSignature": "final-sig"})],
            ]
        };
        let mut body = String::new();
        for parts in chunks {
            body.push_str(&format!(
                "data: {}\n\n",
                json!({"candidates": [{"content": {"parts": parts}}]})
            ));
        }
        body.push_str("data: {\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n");
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(body)
    }
}

#[tokio::test]
async fn opencode_go_audio_tool_steering_and_reopen_preserve_signed_part_boundaries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/inference/go/v1/models/gemini-4-argon-contributor:streamGenerateContent",
        ))
        .respond_with(ReplayScript(AtomicUsize::new(0)))
        .expect(4)
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let session_path = sessions.path().join("signed-audio.jsonl");
    let clips = [
        b"ID3synthetic-first".as_slice(),
        b"ID3synthetic-second".as_slice(),
    ];
    for (step, bytes) in clips.iter().enumerate() {
        std::fs::write(workspace.path().join(format!("clip-{step}.mp3")), bytes).unwrap();
    }
    let mut selected = model(&server.uri());
    let spec = Arc::make_mut(&mut selected.spec);
    spec.id = ModelId("opencode-go/gemini-4-argon-contributor".into());
    spec.api_name = "gemini-4-argon-contributor".into();
    spec.endpoint = EndpointId("opencode-go-google".into());
    spec.cache.send_session_affinity_headers = true;
    let endpoint = Arc::make_mut(&mut selected.endpoint);
    endpoint.id = selected.spec.endpoint.clone();
    endpoint.base_url = url::Url::parse(&format!("{}/inference/go/v1/", server.uri())).unwrap();
    endpoint.auth = Auth::bearer("fixture-key");
    let mut first = agent(
        selected.clone(),
        Session::create(&session_path).unwrap(),
        workspace.path(),
    );
    let mut run = first.prompt("Read both clips.").await.unwrap();
    let mut tools_finished = 0;
    let mut delivered = 0;
    let mut completed = false;
    while let Some(event) = run.next().await {
        match event {
            AgentEvent::ToolFinished { .. } => {
                tools_finished += 1;
                if tools_finished == 1 {
                    run.control()
                        .steer("Continue with both clips.")
                        .await
                        .unwrap();
                }
            }
            AgentEvent::SteeringDelivered { messages } => {
                assert_eq!(messages, vec!["Continue with both clips."]);
                delivered += 1;
            }
            AgentEvent::RunFinished { reason, .. } => {
                assert!(matches!(reason, FinishReason::Completed), "{reason:?}");
                completed = true;
            }
            _ => {}
        }
    }
    drop(run);
    assert!(completed);
    assert_eq!(tools_finished, 2);
    assert_eq!(delivered, 1);
    drop(first);
    let mut resumed = agent(
        selected,
        Session::open(&session_path).unwrap(),
        workspace.path(),
    );
    assert_eq!(
        resumed.complete("Continue.").await.unwrap().text,
        "Both clips received."
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 4);
    for (request_index, request) in requests.iter().enumerate().skip(1) {
        assert_eq!(request.headers["authorization"], "Bearer fixture-key");
        assert!(request.headers.contains_key("x-opencode-session"));
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let contents = body["contents"].as_array().unwrap();
        for (step, bytes) in clips.iter().enumerate().take(request_index.min(2)) {
            let id = format!("read-{step}");
            let parts = contents
                .iter()
                .filter(|content| content["role"] == "model")
                .map(|content| content["parts"].as_array().unwrap())
                .find(|parts| parts.iter().any(|part| part["functionCall"]["id"] == id))
                .unwrap();
            assert_eq!(
                *parts,
                vec![
                    json!({"text": format!("before-{step}"), "thoughtSignature": format!("before-sig-{step}")}),
                    json!({"functionCall": {"id": id, "name": "read", "args": {"path": format!("clip-{step}.mp3")}}, "thoughtSignature": format!("call-sig-{step}")}),
                    json!({"text": format!("after-{step}"), "thoughtSignature": format!("after-sig-{step}")}),
                ]
            );
            let expected = json!({"mimeType": "audio/mp3", "data": base64::engine::general_purpose::STANDARD.encode(bytes)});
            assert_eq!(
                contents
                    .iter()
                    .flat_map(|content| content["parts"].as_array().unwrap())
                    .filter(|part| part.get("inlineData") == Some(&expected))
                    .count(),
                1
            );
        }
        if request_index == 3 {
            assert!(contents
                .iter()
                .flat_map(|content| content["parts"].as_array().unwrap())
                .any(|part| part
                    == &json!({"text": "Both clips received.", "thoughtSignature": "final-sig"})));
        }
    }
}
