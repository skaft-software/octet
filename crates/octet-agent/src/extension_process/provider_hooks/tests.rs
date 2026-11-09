#![cfg(unix)]

use super::*;
use crate::extension_process::tests::{
    protocol_read_state_for_test, trusted_descriptor, wave1_line, write_executable_script,
};
use tempfile::TempDir;

const PROVIDER: &str = r#"#!/usr/bin/env python3
import base64, json, os, sys

def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)

def recv():
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    return json.loads(line)

def call(identifier, method, params):
    send({"jsonrpc":"2.0", "id":identifier, "method":method, "params":params})
    response = recv()
    assert response.get("id") == identifier and "result" in response, response

init = recv()
protocol = init["params"]["protocol"]
wanted = {"provider_proxy_v1", "provider_inheritance_v1", "provider_request_hooks_v1"}
features = protocol["required_features"] + [f for f in protocol["optional_features"] if f in wanted]
assert wanted <= set(features), features
send({"jsonrpc":"2.0", "id":init["id"], "result":{"api_version":"0.4", "tools":[], "protocol":{"version":"0.4", "features":features, "limits":{"max_concurrent_requests":4}, "lifecycle_events":[]}}})
call("register", "providers/register", {
    "provider":{"id":"hook-provider", "label":"Hook provider", "auth":{"kind":"none"}},
    "models":[{"id":"selected", "api_name":"selected", "protocol":"openai_chat", "context_window":8192, "max_output_tokens":1024,
      "capabilities":{"tools":False, "parallel_tool_calls":False, "structured_output":False, "reasoning":False},
      "pi_metadata":{"base_url":"https://example.invalid/v1", "input":["text"], "pricing":{"input":0, "output":0, "cache_read":0, "cache_write_5m":0, "cache_write_1h":None, "reasoning":None, "tiers":[]}}}]
})
send({"jsonrpc":"2.0", "method":"providers/complete", "params":{}})
chunk_sent = False
EXPECT_BRIDGE = True
while True:
    msg = recv()
    method = msg.get("method")
    if method == "provider/stream":
        assert msg["params"].get("request_hooks", False) is EXPECT_BRIDGE, msg
        send({"jsonrpc":"2.0", "id":msg["id"], "result":{"stream_id":msg["params"]["stream_id"], "accepted":True}})
    elif method == "provider/delegate_http":
        with open(os.path.join(os.getcwd(), "http-dispatches"), "a") as ledger:
            ledger.write("dispatch\n")
        send({"jsonrpc":"2.0", "id":msg["id"], "result":{"status":200, "headers":{"content-type":"text/event-stream"}}})
    elif method == "provider/delegate_http_chunk":
        body = (b'data: {"id":"chatcmpl-test","choices":[{"delta":{"content":"ok"}}]}\n\n'
                b'data: {"id":"chatcmpl-test","choices":[{"delta":{},"finish_reason":"stop"}]}\n\n'
                b'data: [DONE]\n\n')
        result = {"done":True} if chunk_sent else {"chunk":base64.b64encode(body).decode()}
        chunk_sent = True
        send({"jsonrpc":"2.0", "id":msg["id"], "result":result})
    elif method == "provider/delegate_event":
        send({"jsonrpc":"2.0", "id":msg["id"], "result":{"accepted":True}})
    elif method == "shutdown":
        send({"jsonrpc":"2.0", "id":msg["id"], "result":{}})
        break
    elif method not in ("provider/cancel", "$/cancelRequest"):
        raise AssertionError(msg)
"#;

struct Probe {
    observations: StdMutex<Vec<(String, &'static str)>>,
    block_payload: AtomicBool,
    entered: Notify,
    release: Semaphore,
    completed_payloads: AtomicUsize,
}

impl Probe {
    fn new(block_payload: bool) -> Self {
        Self {
            observations: StdMutex::new(Vec::new()),
            block_payload: AtomicBool::new(block_payload),
            entered: Notify::new(),
            release: Semaphore::new(0),
            completed_payloads: AtomicUsize::new(0),
        }
    }

    fn phases(&self) -> Vec<&'static str> {
        lock_std_mutex(&self.observations)
            .iter()
            .map(|(_, phase)| *phase)
            .collect()
    }

    fn observe(&self, context: &octet_ai::ProviderRequestContext, phase: &'static str) {
        lock_std_mutex(&self.observations).push((context.operation_id.clone(), phase));
    }
}

#[async_trait::async_trait]
impl octet_ai::ProviderRequestHook for Probe {
    async fn before_request(
        &self,
        context: &octet_ai::ProviderRequestContext,
        mut payload: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, AiError> {
        self.observe(context, "payload");
        self.entered.notify_one();
        if self.block_payload.load(Ordering::Acquire) {
            self.release.acquire().await.unwrap().forget();
        }
        self.completed_payloads.fetch_add(1, Ordering::AcqRel);
        payload["hooked"] = true.into();
        Ok(Some(payload))
    }

    async fn before_headers(
        &self,
        context: &octet_ai::ProviderRequestContext,
        headers: &mut http::HeaderMap,
    ) -> Result<(), AiError> {
        self.observe(context, "headers");
        headers.insert("x-hooked", http::HeaderValue::from_static("yes"));
        Ok(())
    }

    async fn after_response(
        &self,
        context: &octet_ai::ProviderRequestContext,
        _: http::StatusCode,
        _: &http::HeaderMap,
    ) -> Result<(), AiError> {
        self.observe(context, "response");
        Ok(())
    }
}

struct Fixture {
    directory: TempDir,
    process: ExtensionProcess,
    registry: Arc<ExtensionProviderRegistry>,
    connection: Arc<ProcessConnection>,
    stream_id: String,
    stream: Option<ResponseStream>,
    probe: Arc<Probe>,
}

impl Fixture {
    async fn start(block_payload: bool) -> Self {
        Self::start_with_bridge(block_payload, true).await
    }

    async fn start_with_bridge(block_payload: bool, bridge_allowed: bool) -> Self {
        let directory = TempDir::new().unwrap();
        let script = PROVIDER.replace(
            "EXPECT_BRIDGE = True",
            if bridge_allowed {
                "EXPECT_BRIDGE = True"
            } else {
                "EXPECT_BRIDGE = False"
            },
        );
        write_executable_script(&directory.path().join("provider.py"), &script);
        let manifest = ExtensionManifest::parse("name = \"hook-fixture\"\nversion = \"0.1.0\"\napi_version = \"0.4\"\n[entrypoint]\ncommand = \"provider.py\"\n[contributes]\nproviders = true\n").unwrap();
        let registry = Arc::new(ExtensionProviderRegistry::new());
        registry.set_native_provider_catalog(NativeProviderCatalog { models: Vec::new() });
        let mut runtime = ExtensionRuntimeConfig::new(directory.path());
        runtime.provider_registry = Some(registry.clone());
        let process =
            ExtensionProcess::start(trusted_descriptor(directory.path(), manifest), runtime)
                .await
                .unwrap();
        let route = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(route) = registry.resolve("hook-provider", "selected") {
                    break route;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("provider registration");
        let endpoint = octet_ai::Endpoint {
            id: octet_ai::EndpointId("hook-fixture-endpoint".into()),
            base_url: url::Url::parse("https://example.invalid/").unwrap(),
            auth: octet_ai::Auth::None,
            default_headers: Default::default(),
            transport: octet_ai::EndpointTransport::Http,
            runtime: Default::default(),
            timeout: Duration::from_secs(5),
        };
        let model = octet_ai::Model {
            spec: Arc::new(octet_ai::ModelSpec {
                preset: Default::default(),
                id: octet_ai::ModelId("hook-fixture-model".into()),
                api_name: "selected".into(),
                display_name: None,
                endpoint: endpoint.id.clone(),
                protocol: Protocol::OpenAiChat,
                capabilities: octet_ai::Capabilities {
                    responses_features: Default::default(),
                    input_modalities: Default::default(),
                    output_modalities: Default::default(),
                    tools: false,
                    parallel_tool_calls: false,
                    reasoning: None,
                    responses_lite: false,
                    agent_delegation: None,
                    structured_output: false,
                    deferred_tool_loading: false,
                },
                limits: octet_ai::ModelLimits {
                    context_window: 8192,
                    max_output_tokens: 1024,
                },
                pricing: route.model_metadata.as_ref().unwrap().pricing.clone(),
                cache: Default::default(),
            }),
            endpoint: Arc::new(endpoint),
        };
        let connection = read_std_lock(&process.inner.connection).clone();
        if !bridge_allowed {
            write_std_lock(&connection.protocol)
                .features
                .remove(FEATURE);
        }
        let probe = Arc::new(Probe::new(block_payload));
        let client = octet_ai::AiClient::new().with_provider_request_hooks(vec![probe.clone()]);
        client.register_host_stream_transport(
            model.endpoint.id.clone(),
            process
                .prepare_provider_stream_transport(&route, HostStreamModel::from(&model))
                .unwrap(),
        );
        let request: octet_ai::Request = serde_json::from_value(serde_json::json!({
            "system":null, "messages":[{"User":{"content":[{"Text":"test"}]}}],
            "tools":[], "tool_choice":"auto", "max_output_tokens":32, "temperature":null,
            "stop":[], "reasoning":{"type":"off"}, "output_modalities":{"type":"text"}
        }))
        .unwrap();
        let stream = tokio::time::timeout(Duration::from_secs(5), client.stream(&model, request))
            .await
            .expect("stream admission timeout")
            .expect("stream admission");
        let connection = read_std_lock(&process.inner.connection).clone();
        let stream_id = {
            let streams = lock_std_mutex(&connection.provider_streams);
            assert_eq!(streams.len(), 1);
            let (id, ingress) = streams.iter().next().unwrap();
            assert!(ingress.request_hooks.is_some());
            assert!(ingress.delegate.is_some());
            id.clone()
        };
        Self {
            directory,
            process,
            registry,
            connection,
            stream_id,
            stream: Some(stream),
            probe,
        }
    }

    fn state(&self) -> (ProtocolReadState, mpsc::Receiver<WriterFrame>) {
        let (events, _) = broadcast::channel(8);
        let (mut state, frames) = protocol_read_state_for_test(
            ManifestContributions {
                providers: true,
                ..Default::default()
            },
            events,
        );
        state.provider_streams = self.connection.provider_streams.clone();
        state.provider_registry = Some(self.registry.clone());
        state.provider_owner = self.connection.provider_owner.clone();
        state.generation = self.connection.generation;
        state.instance_id = self.connection.provider_owner.extension_instance_id.clone();
        enable(&state);
        (state, frames)
    }

    fn payload(&self) -> serde_json::Value {
        serde_json::json!({"stream_id":self.stream_id, "payload":{"model":"wire"}})
    }

    fn delegate(&self) -> serde_json::Value {
        let streams = lock_std_mutex(&self.connection.provider_streams);
        let delegate = streams[&self.stream_id].delegate.as_ref().unwrap();
        serde_json::json!({"stream_id":self.stream_id, "request":delegate.original, "on_payload":false, "custom_http":true})
    }

    fn dispatch_count(&self) -> usize {
        std::fs::read_to_string(self.directory.path().join("http-dispatches"))
            .map(|ledger| ledger.lines().count())
            .unwrap_or(0)
    }

    async fn finish(mut self) {
        drop(self.stream.take());
        assert!(self.process.shutdown().await);
    }
}

fn enable(state: &ProtocolReadState) {
    let mut protocol = write_std_lock(&state.protocol);
    protocol.version = EXTENSION_API_VERSION_0_4.into();
    protocol.features = ["provider_proxy_v1", "provider_inheritance_v1", FEATURE]
        .into_iter()
        .map(str::to_owned)
        .collect();
}

fn send(state: &ProtocolReadState, id: u64, method: &str, params: serde_json::Value) {
    handle_protocol_line(&wave1_line(id, method, params), state).expect("reader remains usable");
}

async fn reply(frames: &mut mpsc::Receiver<WriterFrame>) -> serde_json::Value {
    let frame = tokio::time::timeout(Duration::from_secs(10), frames.recv())
        .await
        .expect("reply timeout")
        .expect("writer closed");
    serde_json::from_slice(&frame.line).expect("JSON reply")
}

fn assert_error(reply: &serde_json::Value, id: u64, code: i64) {
    assert_eq!(reply["id"], id);
    assert_eq!(reply["error"]["code"], code, "{reply}");
    assert!(reply.get("result").is_none(), "{reply}");
}

async fn mark_terminal(fixture: &Fixture, state: &ProtocolReadState) {
    dispatch_provider_stream_event(
        state,
        api_v04::ProviderStreamEvent {
            stream_id: fixture.stream_id.clone(),
            sequence: 0,
            kind: "finished".into(),
            payload: serde_json::json!({"stop_reason":"stop"}),
        },
    )
    .unwrap();
}

#[tokio::test]
async fn hook_dispatch_refuses_unnegotiated_malformed_foreign_and_terminal_jobs() {
    let fixture = Fixture::start(false).await;
    let (mut state, mut frames) = fixture.state();

    for (id, version, features) in [
        (1, "0.2", vec!["provider_proxy_v1", FEATURE]),
        (2, "0.4", vec!["provider_proxy_v1"]),
        (3, "0.4", vec![FEATURE]),
    ] {
        {
            let mut protocol = write_std_lock(&state.protocol);
            protocol.version = version.into();
            protocol.features = features.into_iter().map(str::to_owned).collect();
        }
        send(&state, id, "provider/hooks/payload", fixture.payload());
        assert_error(&reply(&mut frames).await, id, -32011);
    }
    enable(&state);
    state.declared.providers = false;
    send(&state, 4, "provider/hooks/payload", fixture.payload());
    assert_error(&reply(&mut frames).await, 4, -32011);
    state.declared.providers = true;

    for (id, params) in [
        (5, serde_json::json!({"payload":{}})),
        (
            6,
            serde_json::json!({"stream_id":fixture.stream_id, "payload":{}, "owner":"forged"}),
        ),
        (7, serde_json::json!({"stream_id":23, "payload":{}})),
    ] {
        send(&state, id, "provider/hooks/payload", params);
        assert_error(&reply(&mut frames).await, id, -32602);
    }
    assert!(fixture.probe.phases().is_empty());

    let (events, _) = broadcast::channel(8);
    let (mut foreign, mut foreign_frames) = protocol_read_state_for_test(
        ManifestContributions {
            providers: true,
            ..Default::default()
        },
        events,
    );
    foreign.provider_streams = fixture.connection.provider_streams.clone();
    foreign.provider_registry = Some(fixture.registry.clone());
    foreign.provider_owner.extension_instance_id = "foreign-process".into();
    foreign.provider_owner.generation = fixture.connection.generation + 1;
    enable(&foreign);
    send(&foreign, 12, "provider/hooks/payload", fixture.payload());
    assert_error(&reply(&mut foreign_frames).await, 12, -32011);
    assert!(fixture.probe.phases().is_empty());

    send(&state, 13, "provider/hooks/payload", fixture.payload());
    let accepted = reply(&mut frames).await;
    assert_eq!(accepted["id"], 13);
    assert_eq!(
        accepted["result"]["payload"],
        serde_json::json!({"model":"wire", "hooked":true})
    );
    assert_eq!(fixture.probe.phases(), ["payload"]);
    let job = {
        let streams = lock_std_mutex(&fixture.connection.provider_streams);
        streams[&fixture.stream_id]
            .request_hooks
            .as_ref()
            .unwrap()
            .clone()
    };
    assert!(!job.completed());
    send(
        &state,
        15,
        "provider/hooks/headers",
        serde_json::json!({
            "stream_id": fixture.stream_id, "headers": {"x-original": "yes"}
        }),
    );
    let headers = reply(&mut frames).await;
    assert_eq!(headers["result"]["headers"]["x-original"], "yes");
    assert_eq!(headers["result"]["headers"]["x-hooked"], "yes");
    assert!(!job.completed());
    send(
        &state,
        16,
        "provider/hooks/response",
        serde_json::json!({
            "stream_id": fixture.stream_id, "headers": {"x-response": "yes"}, "status": 200
        }),
    );
    let response = reply(&mut frames).await;
    assert_eq!(response["result"]["accepted"], true);
    assert!(job.completed());
    {
        let observations = lock_std_mutex(&fixture.probe.observations);
        assert_eq!(
            observations
                .iter()
                .map(|(_, phase)| *phase)
                .collect::<Vec<_>>(),
            ["payload", "headers", "response"]
        );
        assert!(observations.iter().all(|(id, _)| *id == observations[0].0));
    }
    mark_terminal(&fixture, &state).await;
    send(&state, 14, "provider/hooks/payload", fixture.payload());
    assert_error(&reply(&mut frames).await, 14, -32002);
    assert_eq!(fixture.probe.phases(), ["payload", "headers", "response"]);
    fixture.finish().await;
}

#[tokio::test]
async fn malformed_phases_poison_their_job_without_invoking_hooks() {
    for (method, field) in [
        ("provider/hooks/payload", "scalar"),
        ("provider/hooks/payload", "mixed"),
        ("provider/hooks/headers", "header"),
        ("provider/hooks/response", "status"),
    ] {
        let fixture = Fixture::start(false).await;
        let (state, mut frames) = fixture.state();
        let params = match field {
            "scalar" => serde_json::json!({"stream_id":fixture.stream_id, "payload":"scalar"}),
            "mixed" => {
                serde_json::json!({"stream_id":fixture.stream_id, "payload":{}, "headers":{}})
            }
            "header" => {
                serde_json::json!({"stream_id":fixture.stream_id, "headers":{"bad name":"x"}})
            }
            "status" => {
                serde_json::json!({"stream_id":fixture.stream_id, "headers":{}, "status":99})
            }
            _ => unreachable!(),
        };
        send(&state, 1, method, params);
        assert_error(&reply(&mut frames).await, 1, -32002);
        send(&state, 2, "provider/hooks/payload", fixture.payload());
        assert_error(&reply(&mut frames).await, 2, -32002);
        {
            let streams = lock_std_mutex(&fixture.connection.provider_streams);
            assert!(!streams[&fixture.stream_id]
                .request_hooks
                .as_ref()
                .unwrap()
                .completed());
        }
        assert!(fixture.probe.phases().is_empty());
        fixture.finish().await;
    }
}

#[tokio::test]
async fn cancellation_withdrawal_and_terminal_event_fence_inflight_results() {
    for mutation in ["cancel", "withdraw", "terminal"] {
        let fixture = Fixture::start(true).await;
        let (state, mut frames) = fixture.state();
        send(&state, 1, "provider/hooks/payload", fixture.payload());
        tokio::time::timeout(Duration::from_secs(5), fixture.probe.entered.notified())
            .await
            .expect("hook did not enter");
        match mutation {
            "cancel" => fixture
                .connection
                .cancel_provider_stream(&fixture.stream_id, "test cancellation"),
            "withdraw" => {
                fixture
                    .registry
                    .unregister(&fixture.connection.provider_owner, "hook-provider")
                    .unwrap();
            }
            "terminal" => mark_terminal(&fixture, &state).await,
            _ => unreachable!(),
        }
        fixture.probe.release.add_permits(1);
        assert_error(&reply(&mut frames).await, 1, -32002);
        assert_eq!(fixture.probe.completed_payloads.load(Ordering::Acquire), 0);
        send(&state, 2, "provider/hooks/payload", fixture.payload());
        let error = reply(&mut frames).await;
        assert_error(
            &error,
            2,
            if mutation == "cancel" { -32011 } else { -32002 },
        );
        assert_eq!(fixture.probe.phases(), ["payload"]);
        assert!(
            frames.try_recv().is_err(),
            "late or duplicate reply for {mutation}"
        );
        fixture.finish().await;
    }
}

#[tokio::test]
async fn unadvertised_bridge_cannot_preempt_native_hook_ownership() {
    let fixture = Fixture::start_with_bridge(false, false).await;
    let (state, mut frames) = fixture.state();
    for (id, method, params) in [
        (1, "provider/hooks/payload", fixture.payload()),
        (
            2,
            "provider/hooks/headers",
            serde_json::json!({"stream_id": fixture.stream_id, "headers": {}}),
        ),
        (
            3,
            "provider/hooks/response",
            serde_json::json!({"stream_id": fixture.stream_id, "status": 200, "headers": {}}),
        ),
    ] {
        send(&state, id, method, params);
        assert_error(&reply(&mut frames).await, id, -32011);
    }
    assert!(fixture.probe.phases().is_empty());
    {
        let streams = lock_std_mutex(&fixture.connection.provider_streams);
        let job = streams[&fixture.stream_id].request_hooks.as_ref().unwrap();
        assert!(!job.bridge_allowed);
        assert_eq!(job.claim.load(Ordering::Acquire), 0);
        assert!(!job.completed());
    }
    send(&state, 4, "provider/delegate", fixture.delegate());
    let native = reply(&mut frames).await;
    assert_eq!(native["id"], 4);
    assert!(native.get("result").is_some(), "{native}");
    assert_eq!(fixture.probe.phases(), ["payload", "headers", "response"]);
    assert_eq!(fixture.dispatch_count(), 1);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_and_bridge_claim_are_mutually_exclusive_under_race() {
    for ordering in ["bridge-first", "native-first", "race"] {
        let fixture = Fixture::start(false).await;
        let (state, mut frames) = fixture.state();
        let state = Arc::new(state);
        let payload = fixture.payload();
        let delegate = fixture.delegate();
        match ordering {
            "bridge-first" => {
                send(&state, 1, "provider/hooks/payload", payload);
                let accepted = reply(&mut frames).await;
                assert_eq!(accepted["result"]["payload"]["hooked"], true);
                send(&state, 2, "provider/delegate", delegate);
                assert_error(&reply(&mut frames).await, 2, -32602);
                assert_eq!(fixture.probe.phases(), ["payload"]);
                assert_eq!(fixture.dispatch_count(), 0);
            }
            "native-first" => {
                send(&state, 2, "provider/delegate", delegate);
                send(&state, 1, "provider/hooks/payload", payload);
                let first = reply(&mut frames).await;
                let second = reply(&mut frames).await;
                let (bridge, native) = if first["id"] == 1 {
                    (first, second)
                } else {
                    (second, first)
                };
                assert_error(&bridge, 1, -32002);
                assert_eq!(native["id"], 2);
                assert!(
                    native.get("result").is_some(),
                    "{native}; phases={:?}; dispatches={}",
                    fixture.probe.phases(),
                    fixture.dispatch_count()
                );
                assert_eq!(fixture.probe.phases(), ["payload", "headers", "response"]);
                assert_eq!(fixture.dispatch_count(), 1);
            }
            "race" => {
                let barrier = Arc::new(tokio::sync::Barrier::new(3));
                let run = |id, method: &'static str, params| {
                    let state = state.clone();
                    let barrier = barrier.clone();
                    tokio::spawn(async move {
                        barrier.wait().await;
                        send(&state, id, method, params);
                    })
                };
                let bridge_task = run(1, "provider/hooks/payload", payload);
                let native_task = run(2, "provider/delegate", delegate);
                barrier.wait().await;
                bridge_task.await.unwrap();
                native_task.await.unwrap();
                let first = reply(&mut frames).await;
                let second = reply(&mut frames).await;
                let (bridge, native) = if first["id"] == 1 {
                    (first, second)
                } else {
                    (second, first)
                };
                if bridge.get("result").is_some() {
                    assert_eq!(bridge["result"]["payload"]["hooked"], true);
                    assert_error(&native, 2, -32602);
                    assert_eq!(fixture.probe.phases(), ["payload"]);
                    assert_eq!(fixture.dispatch_count(), 0);
                } else {
                    assert_error(&bridge, 1, -32002);
                    assert_eq!(native["id"], 2);
                    assert!(native.get("result").is_some(), "{native}");
                    assert_eq!(fixture.probe.phases(), ["payload", "headers", "response"]);
                    assert_eq!(fixture.dispatch_count(), 1);
                }
            }
            _ => unreachable!(),
        }
        fixture.finish().await;
    }
}
