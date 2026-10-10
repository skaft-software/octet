//! Real SDK + ExtensionProcess + ngspice. Missing prerequisites FAIL, never skip.
use octet_agent::extension::ExtensionHost;
use octet_agent::extension_operations::ApplicableOperationsRequest;
use octet_agent::extension_process::{
    DiscoveredExtension, ExtensionActivation, ExtensionManifest, ExtensionProcess,
    ExtensionRuntimeConfig, ExtensionSource, ExtensionTrust, ResourceRef,
};
use octet_agent::tool::ToolProgress;
use octet_agent::{
    Agent, AgentConfig, AgentEvent, BulkStorage, EffectBroker, EffectPolicy, FinishReason,
    SandboxConfig, Session,
};
use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Fixture {
    process: ExtensionProcess,
    root: tempfile::TempDir,
}
impl Fixture {
    async fn start(interrupt: bool) -> Self {
        let example = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let check = Command::new("python3")
            .arg(example.join("check_prerequisites.py"))
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .output()
            .expect("Python prerequisite");
        assert!(
            check.status.success(),
            "BLOCKED F01/F02: {}",
            String::from_utf8_lossy(&check.stderr)
        );
        let root = tempfile::tempdir().unwrap();
        let mut args = vec![
            example.join("extension.py").to_string_lossy().into_owned(),
            "--events".into(),
            root.path()
                .join("events.jsonl")
                .to_string_lossy()
                .into_owned(),
        ];
        if interrupt {
            args.extend([
                "--interrupt-barrier".into(),
                root.path().to_string_lossy().into_owned(),
            ]);
        }
        let text = format!(
            r#"
name = "spice"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "python3"
args = {}
[entrypoint.env]
HOME = {}
TMPDIR = {}
PYTHONDONTWRITEBYTECODE = "1"
[capabilities]
filesystem = "unrestricted"
process = true
network = false
[contributes]
tools = ["spice_open", "spice_instantiate", "spice_transient", "spice_measure"]
"#,
            json!(args),
            json!(root.path()),
            json!(root.path())
        );
        let manifest_path = root.path().join("extension.toml");
        fs::write(&manifest_path, &text).unwrap();
        let descriptor = DiscoveredExtension {
            manifest: ExtensionManifest::parse(&text).unwrap(),
            manifest_path,
            source: ExtensionSource::Explicit,
            activation: ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        };
        let mut config = ExtensionRuntimeConfig::new(root.path());
        config.request_timeout = Duration::from_secs(35);
        config.cancellation_grace = Duration::from_secs(10);
        config.supervise = false;
        config.bulk_store = Some(BulkStorage::new().unwrap());
        let process = ExtensionProcess::start(descriptor, config).await.unwrap();
        for feature in [
            "resource_refs_v1",
            "operation_descriptors_v1",
            "bulk_objects_v1",
            "request_progress",
        ] {
            assert!(process.negotiated_features().contains(feature));
        }
        Self { process, root }
    }
    async fn call(&self, owner: &str, name: &str, args: Value) -> Value {
        let result = self
            .process
            .call_tool(
                name,
                args,
                self.process.current_context_for_resource_owner(owner),
            )
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.content);
        result.structured_content.unwrap()
    }
    fn log(&self) -> Vec<Value> {
        let data = fs::read(self.root.path().join("events.jsonl")).unwrap_or_default();
        assert!(data.len() <= 64 * 1024);
        // An append in progress is not malformed evidence; consume complete lines only.
        data.split_inclusive(|b| *b == b'\n')
            .filter(|line| line.ends_with(b"\n"))
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect()
    }
    async fn event(&self, name: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let log = self.log();
                assert!(
                    !log.iter()
                        .any(|row| row["event"] == "interrupt_unavailable"),
                    "BLOCKED F02: real ngspice exited before the native stopped-state barrier"
                );
                if let Some(row) = log.into_iter().find(|row| row["event"] == name) {
                    return row;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("explicit child event barrier")
    }
    async fn close(&self) {
        assert!(self.process.shutdown().await);
        assert!(!self.process.is_running());
        let log = self.log();
        assert_eq!(
            log.iter().filter(|row| row["event"] == "shutdown").count(),
            1
        );
        assert!(log.iter().all(|row| row["pid"] == log[0]["pid"]));
    }
    fn evidence(&self, name: &str, detail: Value) {
        let value = json!({"host_pid":std::process::id(), "child_log":self.log(), "detail":detail});
        let bytes = serde_json::to_vec_pretty(&value).unwrap();
        assert!(bytes.len() <= 256 * 1024);
        if let Some(path) = std::env::var_os("OCTET_SPICE_EVIDENCE_DIR") {
            fs::create_dir_all(&path).unwrap();
            fs::write(PathBuf::from(path).join(format!("{name}.json")), bytes).unwrap();
        }
        println!(
            "{name}: host_pid={} child_log={}",
            std::process::id(),
            json!(self.log())
        );
    }
}

fn discover(host: &ExtensionHost, owner: &str, reference: &ResourceRef, tool: &str, path: &str) {
    let page = host
        .applicable_operations(
            owner,
            ApplicableOperationsRequest {
                resource: reference.clone(),
                limit: None,
                cursor: None,
            },
        )
        .unwrap();
    assert!(page.next_cursor.is_none());
    assert_eq!(page.operations.len(), 1);
    let found = &page.operations[0];
    assert_eq!(found.tool, tool);
    assert_eq!(found.path, path);
    assert!(found.primary_receiver);
}

fn turn(call: Option<(&str, Value)>) -> ResponseTemplate {
    let frame = |event: &str, value: Value| format!("event: {event}\ndata: {value}\n\n");
    let mut body = frame(
        "message_start",
        json!({"type":"message_start","message":{"id":"local","usage":{"input_tokens":5,"output_tokens":0}}}),
    );
    let stop = if let Some((name, args)) = call {
        body += &frame(
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":format!("call-{name}"),"name":name}}),
        );
        body += &frame(
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":args.to_string()}}),
        );
        "tool_use"
    } else {
        body += &frame(
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        );
        body += &frame(
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"done"}}),
        );
        "end_turn"
    };
    body += &frame(
        "content_block_stop",
        json!({"type":"content_block_stop","index":0}),
    );
    body += &frame(
        "message_delta",
        json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":3}}),
    );
    body += &frame("message_stop", json!({"type":"message_stop"}));
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

#[tokio::test]
async fn f01_spice_acceptance() {
    let f = Fixture::start(false).await;
    let mut host = ExtensionHost::new();
    host.load(&f.process);
    host.finalize_tool_surface();
    let session = Session::create(f.root.path().join("session.jsonl")).unwrap();
    let owner = session.resource_owner_key();
    let transient_schema = f
        .process
        .tool_definitions()
        .into_iter()
        .find(|tool| tool.name == "spice_transient")
        .unwrap()
        .parameters;
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let waveform = Arc::new(Mutex::new(None::<Value>));
    let measured = waveform.clone();
    let selected = Arc::new(Mutex::new(None::<ResourceRef>));
    let receiver = selected.clone();
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(
            move |_: &wiremock::Request| match counter.fetch_add(1, Ordering::SeqCst) {
                0 => turn(None), // Observe the actual initial provider surface before discovery.
                1 => turn(Some((
                    "spice_transient",
                    json!({"session":receiver.lock().unwrap().as_ref().expect("selected native session")}),
                ))),
                2 => turn(Some((
                    "spice_measure",
                    measured
                        .lock()
                        .unwrap()
                        .clone()
                        .expect("admitted typed waveform"),
                ))),
                3 => turn(None),
                _ => panic!("unexpected scripted model request"),
            },
        )
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
        session,
        system: "Local SPICE conformance; no external inference".into(),
        sandbox: SandboxConfig::new(f.root.path()),
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions: host.clone(),
        max_turns: Some(4),
        reasoning: octet_ai::ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    // Qualify lazy visibility at the real provider boundary, not through a
    // private host projection method or the unfiltered registered-tool catalog.
    let mut initial = agent
        .prompt("inspect the initial tool surface")
        .await
        .unwrap();
    let mut initial_completed = false;
    while let Some(event) = initial.next().await {
        match event {
            AgentEvent::ToolStarted { .. } | AgentEvent::ToolFinished { .. } => {
                panic!("initial surface probe must not invoke a tool")
            }
            AgentEvent::RunFinished { reason, .. } => {
                assert!(matches!(reason, FinishReason::Completed));
                initial_completed = true;
            }
            _ => {}
        }
    }
    drop(initial);
    assert!(initial_completed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let initial_requests = server.received_requests().await.unwrap();
    assert_eq!(initial_requests.len(), 1);
    let initial_request: Value = initial_requests[0].body_json().unwrap();
    let initial_tools = initial_request["tools"].as_array().unwrap();
    assert!(initial_tools
        .iter()
        .any(|tool| tool["name"] == "spice_open"));
    assert!(!initial_tools.iter().any(|tool| matches!(
        tool["name"].as_str(),
        Some("spice_instantiate" | "spice_transient")
    )));

    let opened = f.call(&owner, "spice_open", json!({})).await;
    let circuit: ResourceRef = serde_json::from_value(opened["circuit"].clone()).unwrap();
    discover(&host, &owner, &circuit, "spice_instantiate", "/circuit");
    let instantiated = f
        .call(&owner, "spice_instantiate", json!({"circuit":circuit}))
        .await;
    let native: ResourceRef = serde_json::from_value(instantiated["session"].clone()).unwrap();
    discover(&host, &owner, &native, "spice_transient", "/session");
    *selected.lock().unwrap() = Some(native.clone());

    let mut run = agent
        .prompt("run and measure the selected RC session")
        .await
        .unwrap();
    let mut progress = Vec::new();
    let mut measurement = None;
    let mut completed = false;
    while let Some(event) = run.next().await {
        match event {
            AgentEvent::ToolProgress {
                progress: ToolProgress::Status(text),
                ..
            } => progress.push(text),
            AgentEvent::ToolFinished { result, .. } => {
                let output = result.unwrap();
                assert!(!output.is_error());
                let value = output.structured_content().unwrap().clone();
                if value.get("blob").is_some() {
                    assert!(output.text.len() < 1024);
                    let (prefix, descriptor) = output.text.split_once(": ").unwrap();
                    assert_eq!(
                        prefix,
                        format!("RC transient ({} samples)", value["samples"])
                    );
                    assert_eq!(
                        serde_json::from_str::<Value>(descriptor).unwrap(),
                        value["blob"]
                    );
                    *waveform.lock().unwrap() = Some(value);
                } else {
                    assert!(measurement.replace(value).is_none());
                }
            }
            AgentEvent::RunFinished { reason, .. } => {
                assert!(matches!(reason, FinishReason::Completed));
                completed = true;
            }
            _ => {}
        }
    }
    drop(run);
    assert!(completed);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert!(progress
        .iter()
        .any(|text| text.contains("Starting ngspice")));
    assert!(progress
        .iter()
        .any(|text| text.contains("ngspice finished")));
    let waveform = waveform.lock().unwrap().clone().unwrap();
    let measurement = measurement.unwrap();
    let voltage = measurement["final_voltage_v"].as_f64().unwrap();
    assert!(voltage.is_finite() && (voltage - (1.0 - (-5.0_f64).exp())).abs() < 0.002);
    assert!((measurement["final_time_s"].as_f64().unwrap() - 0.005).abs() < 1e-9);
    assert_eq!(waveform["samples"], measurement["samples"]);
    assert_eq!(
        waveform["blob"]["bytes"].as_u64().unwrap(),
        16 * waveform["samples"].as_u64().unwrap()
    );
    let requests: Vec<Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| request.body_json().unwrap())
        .collect();
    assert_eq!(
        requests[1]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "spice_transient")
            .unwrap()["input_schema"],
        transient_schema
    );
    let next = requests[2].to_string();
    assert!(next.contains(waveform["blob"]["$blob"].as_str().unwrap()));
    assert!(next.contains(waveform["blob"]["digest"]["value"].as_str().unwrap()));
    for request in &requests {
        let text = request.to_string();
        assert!(text.len() < 64 * 1024);
        for forbidden in [
            "octet-transfer-",
            "transfer_directory",
            "locator",
            "Starting ngspice",
            "ngspice finished",
            "Index   time",
        ] {
            assert!(!text.contains(forbidden), "model leaked {forbidden}");
        }
    }
    assert!(f.process.release_resource(&owner, &native).unwrap().retired);
    f.event("disposed_SimulationSession").await;
    let before = f.log().len();
    assert!(f
        .process
        .call_tool(
            "spice_transient",
            json!({"session":native}),
            f.process.current_context_for_resource_owner(&owner)
        )
        .await
        .is_err());
    assert_eq!(
        f.log().len(),
        before,
        "released resource reached the domain handler"
    );
    assert!(
        f.process
            .release_resource(&owner, &circuit)
            .unwrap()
            .retired
    );
    f.event("disposed_Circuit").await;
    f.close().await;
    f.evidence(
        "f01_spice_acceptance",
        json!({"requests":requests,"progress":progress,"measurement":measurement}),
    );
}

#[tokio::test]
async fn f02_spice_interrupt() {
    let f = Fixture::start(true).await;
    let owner = "spice-interrupt-owner";
    let circuit = f.call(owner, "spice_open", json!({})).await["circuit"].clone();
    let native: ResourceRef = serde_json::from_value(
        f.call(owner, "spice_instantiate", json!({"circuit":circuit}))
            .await["session"]
            .clone(),
    )
    .unwrap();
    let process = f.process.clone();
    let reference = native.clone();
    let call = tokio::spawn(async move {
        process
            .call_tool(
                "spice_transient",
                json!({"session":reference}),
                process.current_context_for_resource_owner(owner),
            )
            .await
    });
    let ready = f.event("interrupt_ready").await; // OS confirmed real ngspice stopped after exec.
    let pid = ready["solver_pid"].as_i64().unwrap() as libc::pid_t;
    let generation = f.process.health_snapshot().generation;
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    f.event("interrupt_cancelled").await;
    assert!(f
        .process
        .release_resource(owner, &native)
        .unwrap_err()
        .to_string()
        .contains("resource_busy"));
    assert!(!f.log().iter().any(|row| row["event"] == "solver_settled"));
    fs::write(f.root.path().join("allow_stop"), b"").unwrap();
    let settled = f.event("solver_settled").await;
    assert_eq!(settled["solver_pid"], ready["solver_pid"]);
    // SAFETY: signal zero only queries existence; the owned child has been waited.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match f.process.release_resource(owner, &native) {
                Ok(status) => {
                    assert!(status.retired);
                    break;
                }
                Err(error) => assert!(error.to_string().contains("resource_busy")),
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    f.event("disposed_SimulationSession").await;
    assert_eq!(f.process.health_snapshot().generation, generation);
    assert!(!f
        .log()
        .iter()
        .any(|row| row["event"] == "waveform_committed_provisionally"));
    let before = f.log().len();
    assert!(f
        .process
        .call_tool(
            "spice_transient",
            json!({"session":native}),
            f.process.current_context_for_resource_owner(owner)
        )
        .await
        .is_err());
    assert_eq!(f.log().len(), before);
    let circuit: ResourceRef = serde_json::from_value(circuit).unwrap();
    assert!(f.process.release_resource(owner, &circuit).unwrap().retired);
    f.event("disposed_Circuit").await;
    f.close().await;
    f.evidence(
        "f02_spice_interrupt",
        json!({"solver_pid":pid,"generation":generation}),
    );
}
