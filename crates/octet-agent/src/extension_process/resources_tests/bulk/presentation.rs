use super::*;
use crate::{Agent, AgentConfig, EffectBroker, EffectPolicy, SandboxConfig, Session};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn turn(create: bool) -> ResponseTemplate {
    let frame = |event: &str, value: Value| format!("event: {event}\ndata: {value}\n\n");
    let mut body = frame(
        "message_start",
        json!({"type":"message_start","message":{"id":"local","usage":{"input_tokens":5,"output_tokens":0}}}),
    );
    if create {
        body += &frame(
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"bulk-call","name":"bulk_create"}}),
        );
        body += &frame(
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}),
        );
    } else {
        body += &frame(
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        );
        body += &frame(
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"done"}}),
        );
    }
    body += &frame(
        "content_block_stop",
        json!({"type":"content_block_stop","index":0}),
    );
    body += &frame(
        "message_delta",
        json!({"type":"message_delta","delta":{"stop_reason":if create {"tool_use"} else {"end_turn"}},"usage":{"output_tokens":3}}),
    );
    body += &frame("message_stop", json!({"type":"message_stop"}));
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

#[tokio::test]
async fn b15_actual_agent_model_gets_descriptor_not_payload_or_locator() {
    for rust in [false, true] {
        let storage = storage();
        let mut f = fixture(rust, true, &storage, false).await;
        let mut host = ExtensionHost::new();
        f.process.register_dynamic_tool_catalog(&mut host);
        host.finalize_tool_surface();
        let session = Session::create(f.temp.path().join("session.jsonl")).unwrap();
        let server = MockServer::start().await;
        let turns = Arc::new(AtomicUsize::new(0));
        let counter = turns.clone();
        Mock::given(wiremock::matchers::method("POST"))
            .respond_with(move |_: &wiremock::Request| {
                match counter.fetch_add(1, Ordering::SeqCst) {
                    0 => turn(true),
                    1 => turn(false),
                    _ => panic!("unexpected model request"),
                }
            })
            .mount(&server)
            .await;
        let mut model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
            .unwrap();
        Arc::make_mut(&mut model.spec).protocol = octet_ai::Protocol::AnthropicMessages;
        Arc::make_mut(&mut model.endpoint).base_url =
            url::Url::parse(&format!("{}/", server.uri())).unwrap();
        Arc::make_mut(&mut model.endpoint).auth =
            octet_ai::Auth::bearer("local-scripted-no-inference");
        Arc::make_mut(&mut model.endpoint).transport = octet_ai::EndpointTransport::Http;
        let mut agent = Agent::new(AgentConfig {
            client: octet_ai::AiClient::new(),
            model,
            session,
            system: "Local bulk projection conformance probe".into(),
            sandbox: SandboxConfig::new(f.temp.path()),
            effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
            extensions: host,
            max_turns: Some(3),
            reasoning: octet_ai::ReasoningConfig::Off,
            reasoning_mode: octet_ai::ReasoningMode::Standard,
            cache_retention: octet_ai::CacheRetention::Short,
            session_id: None,
        })
        .unwrap();
        assert_eq!(
            agent
                .complete("create one immutable blob")
                .await
                .unwrap()
                .text,
            "done"
        );
        let ready = f.event("output_ready").await;
        let reference: BlobRef = serde_json::from_value(ready["blobs"][0].clone()).unwrap();
        let requests = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.body_json::<Value>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(requests.len(), 2);
        let request = requests[1].to_string();
        assert!(request.contains(&reference.id));
        assert!(request.contains(&reference.digest.value));
        assert!(request.contains("immutable blob"));
        assert!(!request.contains("immutable-octet-bulk-v1"));
        assert!(!request.contains("octet-transfer-"));
        assert!(!request.contains("transfer_directory"));
        assert!(!request.contains(
            &storage
                .lock()
                .transfer_directory()
                .to_string_lossy()
                .to_string()
        ));
        if let Some(root) = std::env::var_os("OCTET_RESOURCE_EVIDENCE_DIR") {
            std::fs::write(
                PathBuf::from(root).join(format!(
                    "{}-model-{}.json",
                    std::process::id(),
                    if rust { "rust" } else { "python" }
                )),
                serde_json::to_vec_pretty(&requests).unwrap(),
            )
            .unwrap();
        }
        assert_eq!(f.calls(), 1);
        f.process.shutdown().await;
    }
}

#[tokio::test]
async fn b11_host_restart_durable_retention_requires_fresh_reauthorization() {
    const CHILD: &str = "OCTET_BULK_RESTART_CHILD";
    if let Ok(mode) = std::env::var(CHILD) {
        let root = PathBuf::from(std::env::var_os("OCTET_BULK_RESTART_ROOT").unwrap());
        let rust = std::env::var("OCTET_BULK_RESTART_RUST").unwrap() == "true";
        let controlled = std::env::var("OCTET_BULK_RESTART_CONTROLLED").unwrap() == "true";
        let store_root = root.join("store");
        crate::secure_fs::create_private_directory_all(&store_root).unwrap();
        let storage =
            BulkStorage::with_root_and_limits(&store_root, BulkLimits::default()).unwrap();
        let f = fixture(rust, controlled, &storage, true).await;
        if mode == "create" {
            let output = f
                .call("A", "bulk_create", json!({"mixed":true}))
                .await
                .unwrap();
            let bytes = blob(&output);
            storage.retain_durable("A", &bytes).await.unwrap();
            std::fs::write(root.join("saved.json"), serde_json::to_vec(&json!({"blob":bytes,"resource":reference(&output),"host_pid":std::process::id()})).unwrap()).unwrap();
        } else {
            let saved: Value =
                serde_json::from_slice(&std::fs::read(root.join("saved.json")).unwrap()).unwrap();
            assert_ne!(saved["host_pid"], std::process::id());
            assert_eq!(f.process.health_snapshot().generation, 1);
            let bytes: BlobRef = serde_json::from_value(saved["blob"].clone()).unwrap();
            unavailable_blob(&f, "A", &bytes).await;
            unavailable(
                f.call("A", "use", json!({"resource":saved["resource"]}))
                    .await,
            );
            assert_eq!(f.calls(), 0);
            storage.recover_durable("A", &bytes).await.unwrap();
            verify(&f, "A", &bytes).await;
            storage.release_durable("A", &bytes).unwrap();
            storage.release_blob("A", &bytes).unwrap();
        }
        f.process.shutdown().await;
        return;
    }
    for (rust, controlled) in VARIANTS {
        let root = TempDir::new().unwrap();
        for mode in ["create", "recover"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "extension_process::resources_tests::bulk::presentation::b11_host_restart_durable_retention_requires_fresh_reauthorization", "--nocapture", "--test-threads=1"])
                .env(CHILD, mode).env("OCTET_BULK_RESTART_ROOT", root.path())
                .env("OCTET_BULK_RESTART_RUST", rust.to_string()).env("OCTET_BULK_RESTART_CONTROLLED", controlled.to_string())
                .output().unwrap();
            if let Some(directory) = std::env::var_os("OCTET_RESOURCE_EVIDENCE_DIR") {
                let base = PathBuf::from(directory).join(format!(
                    "{}-bulk-host-{mode}-{rust}-{controlled}",
                    std::process::id()
                ));
                std::fs::write(base.with_extension("stdout"), &output.stdout).unwrap();
                std::fs::write(base.with_extension("stderr"), &output.stderr).unwrap();
                std::fs::write(
                    base.with_extension("exit"),
                    output.status.code().unwrap_or(-1).to_string(),
                )
                .unwrap();
            }
            assert!(
                output.status.success(),
                "bulk host child failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
