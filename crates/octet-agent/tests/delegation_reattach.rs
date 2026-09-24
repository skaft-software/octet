#![allow(missing_docs)]

//! Restart reattachment tests for the bounded V2 delegation runtime.
//!
//! These tests simulate a process restart with the *same durable store*: the
//! first agent owns the session and its worker, the second agent opens the same
//! transcript and delegation directory and must reattach the worker instead of
//! double-running it. A real process restart additionally requires the old
//! process to be gone; the exact command is reported with the parent deliverable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use octet_agent::{
    Agent, AgentConfig, CoreTools, DelegationConfig, DelegationLimits, EffectBroker, EffectPolicy,
    EntryValue, ExtensionHost, SandboxConfig, Session,
};
use octet_ai::{
    AiClient, Auth, Capabilities, Endpoint, EndpointId, Message, ModalitySet, Model, ModelId,
    ModelLimits, ModelSpec, Protocol, ReasoningConfig, ToolResultPart, UserPart,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

fn frame(event: &str, data: serde_json::Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

fn message_start() -> String {
    frame(
        "message_start",
        serde_json::json!({
            "type": "message_start",
            "message": {"id": "reattach-test", "usage": {"input_tokens": 5, "output_tokens": 0}}
        }),
    )
}

fn message_end(stop_reason: &str) -> String {
    frame(
        "message_delta",
        serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason},
            "usage": {"output_tokens": 3}
        }),
    ) + &frame("message_stop", serde_json::json!({"type": "message_stop"}))
}

fn text_turn(text: &str) -> String {
    message_start()
        + &frame(
            "content_block_start",
            serde_json::json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text", "text": ""}
            }),
        )
        + &frame(
            "content_block_delta",
            serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": text}
            }),
        )
        + &frame(
            "content_block_stop",
            serde_json::json!({"type": "content_block_stop", "index": 0}),
        )
        + &message_end("end_turn")
}

fn tool_turn(calls: &[(&str, &str, serde_json::Value)]) -> String {
    let mut body = message_start();
    for (index, (id, name, arguments)) in calls.iter().enumerate() {
        body += &frame(
            "content_block_start",
            serde_json::json!({
                "type": "content_block_start",
                "index": index,
                "content_block": {"type": "tool_use", "id": id, "name": name}
            }),
        );
        body += &frame(
            "content_block_delta",
            serde_json::json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {"type": "input_json_delta", "partial_json": arguments.to_string()}
            }),
        );
        body += &frame(
            "content_block_stop",
            serde_json::json!({"type": "content_block_stop", "index": index}),
        );
    }
    body + &message_end("tool_use")
}

fn response(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .set_body_string(body)
        .insert_header("content-type", "text/event-stream")
}

fn request_system(body: &serde_json::Value) -> String {
    body.get("system")
        .map(serde_json::Value::to_string)
        .unwrap_or_default()
}

#[derive(Default)]
struct ScriptState {
    counters: Mutex<BTreeMap<String, usize>>,
    requests: Mutex<Vec<serde_json::Value>>,
    unexpected: Mutex<Vec<String>>,
}

impl ScriptState {
    fn record(&self, request: &wiremock::Request) -> serde_json::Value {
        let body: serde_json::Value =
            serde_json::from_slice(&request.body).expect("request body must be JSON");
        self.requests.lock().unwrap().push(body.clone());
        body
    }

    fn next_index(&self, route: &str) -> usize {
        let mut counters = self.counters.lock().unwrap();
        let next = counters.entry(route.to_owned()).or_default();
        let index = *next;
        *next += 1;
        index
    }

    fn unexpected(&self, route: &str, index: usize) -> ResponseTemplate {
        self.unexpected
            .lock()
            .unwrap()
            .push(format!("{route}:{index}"));
        response(text_turn(&format!(
            "unexpected script step {route}:{index}"
        )))
    }
}

fn scripted_model(uri: &str) -> Model {
    Model {
        spec: Arc::new(ModelSpec {
            id: ModelId("reattach-scripted".into()),
            endpoint: EndpointId("reattach-test".into()),
            api_name: "reattach-scripted".into(),
            display_name: None,
            protocol: Protocol::AnthropicMessages,
            capabilities: Capabilities {
                responses_features: Default::default(),
                input_modalities: ModalitySet::none(),
                output_modalities: ModalitySet::none(),
                tools: true,
                parallel_tool_calls: true,
                reasoning: None,
                responses_lite: false,
                agent_delegation: None,
                structured_output: false,
                deferred_tool_loading: false,
            },
            limits: ModelLimits {
                context_window: 200_000,
                max_output_tokens: 8_192,
            },
            pricing: None,
            cache: octet_ai::CacheCompatibility::default(),
            preset: Default::default(),
        }),
        endpoint: Arc::new(Endpoint {
            id: EndpointId("reattach-test".into()),
            base_url: url::Url::parse(uri).unwrap(),
            auth: Auth::bearer("test-key"),
            default_headers: http::HeaderMap::new(),
            transport: octet_ai::EndpointTransport::Http,
            runtime: octet_ai::RequestRuntime::default(),
            timeout: Duration::from_secs(60),
        }),
    }
}

struct Rig {
    workspace: tempfile::TempDir,
    sessions: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        Self {
            workspace: tempfile::tempdir().unwrap(),
            sessions: tempfile::tempdir().unwrap(),
        }
    }

    fn session_path(&self) -> PathBuf {
        self.sessions.path().join("root.jsonl")
    }

    fn delegation_directory(&self) -> PathBuf {
        self.sessions.path().join("delegation")
    }

    fn roster_path(&self) -> PathBuf {
        self.delegation_directory().join("fleet.json")
    }

    /// Builds one owning session over an already-created session file.
    fn open_agent(&self, server: &MockServer, session_path: &Path) -> Agent {
        let workspace = self.workspace.path().canonicalize().unwrap();
        let mut extensions = ExtensionHost::new();
        extensions.load(&CoreTools);
        let mut sandbox = SandboxConfig::new(&workspace);
        sandbox.allow_edit = true;
        sandbox.allow_write = true;
        sandbox.allow_process = true;
        sandbox.allow_shell = true;
        let mut agent = Agent::new(AgentConfig {
            client: AiClient::new(),
            model: scripted_model(&server.uri()),
            session: Session::open(session_path).unwrap(),
            system: "You are a delegation restart test agent.".into(),
            sandbox,
            effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
            extensions,
            max_turns: Some(40),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: octet_ai::ReasoningMode::Standard,
            cache_retention: octet_ai::CacheRetention::Short,
            session_id: Some("delegation-restart".into()),
        })
        .unwrap();
        let mut config = DelegationConfig::new(self.delegation_directory());
        config.limits = DelegationLimits::default();
        // The scripted fleet is driven through the native collaboration tools
        // (`spawn_agent`, `wait_agent`, `list_agents`, `followup_task`), so the
        // root must install them: `enable_v2_delegation_extension_only` leaves
        // the user-facing surface to an extension process and does not register
        // these tools. The delegation manager, durable roster, lease, and
        // reattach path are identical in both configurations.
        agent.enable_v2_delegation(config).unwrap();
        agent
    }
}

async fn mount_script(server: &MockServer, responder: impl Respond + 'static) {
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(responder)
        .mount(server)
        .await;
}

fn tool_results(session: &Session) -> BTreeMap<String, (bool, String)> {
    let mut results = BTreeMap::new();
    for entry in session.entries() {
        let EntryValue::Message(Message::User(message)) = &entry.value else {
            continue;
        };
        for part in &message.content {
            let UserPart::ToolResult(result) = part else {
                continue;
            };
            let text = result
                .content
                .iter()
                .filter_map(|part| match part {
                    ToolResultPart::Text(text) => Some(text.as_str()),
                    ToolResultPart::Media(_) => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            results.insert(result.tool_call_id.0.clone(), (result.is_error, text));
        }
    }
    results
}

fn parse_result(results: &BTreeMap<String, (bool, String)>, id: &str) -> serde_json::Value {
    let (is_error, text) = results
        .get(id)
        .unwrap_or_else(|| panic!("missing result {id}"));
    assert!(!is_error, "{id} failed: {text}");
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("invalid result {id}: {error}: {text}"))
}

fn fleet_record(roster: &Path, agent_id: &str) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(roster).ok()?;
    let fleet: serde_json::Value = serde_json::from_str(&text).ok()?;
    fleet["records"]
        .as_array()?
        .iter()
        .find(|record| record["agent_id"] == agent_id)
        .cloned()
}

/// Waits until the durable roster marks the worker detached.
async fn wait_for_detached_record(roster: &Path, agent_id: &str) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(record) = fleet_record(roster, agent_id) {
                if record["detached"] == true {
                    return record;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the detached worker was never published in the durable roster")
}

/// Waits until the released session parked the worker as recoverable.
async fn wait_for_parked_record(roster: &Path, agent_id: &str) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(record) = fleet_record(roster, agent_id) {
                if record["detached"] == true && record["status"]["state"] == "detached" {
                    return record;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the released session never parked its worker")
}

/// First owning session: spawn a worker that completes a task and then idles.
struct FirstOwnerScript {
    state: Arc<ScriptState>,
}

impl Respond for FirstOwnerScript {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let body = self.state.record(request);
        let route = if request_system(&body).contains("You are /root/survivor") {
            "child"
        } else {
            "root"
        };
        let index = self.state.next_index(route);
        match (route, index) {
            ("root", 0) => response(tool_turn(&[(
                "spawn-survivor",
                "spawn_agent",
                serde_json::json!({
                    "task_name": "survivor",
                    "message": "complete one task before the owning session restarts"
                }),
            )])),
            ("root", 1) => response(tool_turn(&[(
                "wait-survivor",
                "wait_agent",
                serde_json::json!({"timeout_ms": 3_000}),
            )])),
            ("root", 2) => response(text_turn("first owning session complete")),
            ("child", 0) => response(text_turn("survivor first task complete")),
            _ => self.state.unexpected(route, index),
        }
    }
}

/// Second owning session (the simulated restart): reattach, wait, then steer.
struct RestartedOwnerScript {
    state: Arc<ScriptState>,
}

impl Respond for RestartedOwnerScript {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let body = self.state.record(request);
        let route = if request_system(&body).contains("You are /root/survivor") {
            "child"
        } else {
            "root"
        };
        let index = self.state.next_index(route);
        match (route, index) {
            ("root", 0) => response(tool_turn(&[(
                "list-reattached",
                "list_agents",
                serde_json::json!({}),
            )])),
            ("root", 1) => response(tool_turn(&[(
                "steer-reattached",
                "followup_task",
                serde_json::json!({
                    "target": "/root/survivor",
                    "message": "continue after the restart"
                }),
            )])),
            ("root", 2) => response(tool_turn(&[(
                "wait-reattached",
                "wait_agent",
                serde_json::json!({"timeout_ms": 3_000}),
            )])),
            ("root", 3) => response(tool_turn(&[(
                "list-after-continue",
                "list_agents",
                serde_json::json!({}),
            )])),
            ("root", 4) => response(text_turn("second owning session complete")),
            ("child", 0) => response(text_turn("survivor continued after restart")),
            _ => self.state.unexpected(route, index),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restarted_session_reattaches_and_continues_its_worker() {
    let server = MockServer::start().await;
    let first_state = Arc::new(ScriptState::default());
    mount_script(
        &server,
        FirstOwnerScript {
            state: Arc::clone(&first_state),
        },
    )
    .await;
    let rig = Rig::new();
    let session_path = rig.session_path();
    Session::create(&session_path).unwrap();

    {
        let mut first = rig.open_agent(&server, &session_path);
        let output = first.complete("spawn the survivor").await.unwrap();
        assert_eq!(output.text, "first owning session complete");
        assert!(first_state.unexpected.lock().unwrap().is_empty());
        // The worker must really exist before any durable effect is awaited: a
        // failed spawn would otherwise surface only as a timeout.
        let results = tool_results(first.session());
        let spawned = parse_result(&results, "spawn-survivor");
        assert_eq!(spawned["agent_id"], "agent-1", "{spawned}");
        assert_eq!(spawned["agent_path"], "/root/survivor", "{spawned}");
        assert_eq!(
            parse_result(&results, "wait-survivor")["timed_out"],
            false,
            "the first task must complete before the session is released"
        );
    }
    // Dropping the first owning session releases its workers: the idle worker
    // parks as a recoverable durable record instead of retiring.
    let parked = wait_for_parked_record(&rig.roster_path(), "agent-1").await;
    assert_eq!(parked["agent_path"], "/root/survivor", "{parked}");
    assert_eq!(parked["turn_count"], 1, "{parked}");
    // The released manager drops its lease as soon as the parked worker returns;
    // the simulated restart reopens the same transcript and delegation store.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // The restarted process serves a fresh script.
    server.reset().await;
    let second_state = Arc::new(ScriptState::default());
    mount_script(
        &server,
        RestartedOwnerScript {
            state: Arc::clone(&second_state),
        },
    )
    .await;
    let mut second = rig.open_agent(&server, &session_path);
    let output = second
        .complete("reattach the survivor")
        .await
        .expect("the restarted session must reattach its worker");
    assert_eq!(output.text, "second owning session complete");
    assert!(second_state.unexpected.lock().unwrap().is_empty());

    let results = tool_results(second.session());
    let listed = parse_result(&results, "list-reattached");
    let agents = listed["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 1, "{listed}");
    let restored = &agents[0];
    assert_eq!(restored["agent_id"], "agent-1");
    assert_eq!(restored["agent_path"], "/root/survivor");
    // Reattachment retains the identity and accounting without claiming that
    // an idle worker is running. An explicit continuation starts its next run.
    assert_eq!(restored["detached"], false, "{listed}");
    assert_eq!(restored["live_task"], false, "{listed}");
    assert_eq!(restored["turn_count"], 1, "{listed}");
    assert_eq!(restored["status"]["state"], "interrupted", "{listed}");
    assert!(restored["diagnostic"]
        .as_str()
        .unwrap()
        .contains("subagent_continue"));

    let steered = parse_result(&results, "steer-reattached");
    assert_eq!(steered["agent_id"], "agent-1");
    // The accepted follow-up starts a new run of the same retained child
    // session, rather than joining a fictitious active task.
    assert_eq!(steered["delivery"], "new_run", "{steered}");
    let waited = parse_result(&results, "wait-reattached");
    assert!(
        waited.to_string().contains("survivor continued after restart"),
        "{waited}"
    );
    // The continuation is the same worker, the same child session, and the
    // durable turn count continues from the pre-restart value rather than being
    // reset by either reattachment or the queued task.
    let after = parse_result(&results, "list-after-continue");
    let continued = &after["agents"][0];
    assert_eq!(continued["agent_id"], "agent-1", "{after}");
    assert_eq!(continued["agent_path"], "/root/survivor", "{after}");
    assert_eq!(continued["session"], restored["session"], "{after}");
    assert_eq!(continued["turn_count"], 2, "{after}");
    assert_eq!(continued["status"]["state"], "completed", "{after}");
    assert_eq!(continued["detached"], false, "{after}");
    assert_eq!(continued["live_task"], true, "{after}");

    let requests = second_state.requests.lock().unwrap();
    let child_requests = requests
        .iter()
        .filter(|request| request_system(request).contains("You are /root/survivor"))
        .collect::<Vec<_>>();
    assert_eq!(child_requests.len(), 1, "{child_requests:?}");
    assert!(child_requests[0]
        .to_string()
        .contains("continue after the restart"));
}

/// Two live sessions over the same durable store: the second is refused by name
/// and never starts the first session's worker a second time. Both sessions
/// share the mock server, so the script routes on the owning session's own
/// prompt text instead of a shared counter.
struct DuplicateOpenScript {
    state: Arc<ScriptState>,
}

impl Respond for DuplicateOpenScript {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let body = self.state.record(request);
        if request_system(&body).contains("You are /root/long-runner") {
            let index = self.state.next_index("child");
            return match index {
                0 => response(text_turn("long runner still working"))
                    .set_delay(Duration::from_secs(30)),
                _ => self.state.unexpected("child", index),
            };
        }
        let messages = body["messages"].to_string();
        let route = if messages.contains("observe the refusal") {
            "duplicate"
        } else if messages.contains("confirm my worker") {
            "owner-second"
        } else {
            "owner-first"
        };
        let index = self.state.next_index(route);
        match (route, index) {
            ("owner-first", 0) => response(tool_turn(&[(
                "spawn-long-runner",
                "spawn_agent",
                serde_json::json!({
                    "task_name": "long-runner",
                    "message": "keep working across the other session's start"
                }),
            )])),
            ("owner-first", 1) => response(text_turn("first session complete")),
            ("owner-second", 0) => response(tool_turn(&[(
                "list-first-session",
                "list_agents",
                serde_json::json!({}),
            )])),
            ("owner-second", 1) => response(text_turn("first session observed its worker")),
            ("duplicate", 0) => response(tool_turn(&[(
                "list-duplicate",
                "list_agents",
                serde_json::json!({}),
            )])),
            ("duplicate", 1) => response(text_turn("duplicate session observed the refusal")),
            _ => self.state.unexpected(route, index),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_duplicate_session_open_is_refused_and_never_double_runs_a_worker() {
    let server = MockServer::start().await;
    let state = Arc::new(ScriptState::default());
    mount_script(
        &server,
        DuplicateOpenScript {
            state: Arc::clone(&state),
        },
    )
    .await;
    let rig = Rig::new();
    let session_path = rig.session_path();
    Session::create(&session_path).unwrap();

    let mut owner = rig.open_agent(&server, &session_path);
    let output = owner.complete("spawn the long runner").await.unwrap();
    assert_eq!(output.text, "first session complete");
    // The worker must really exist (and still be running) before the roster is
    // awaited: a failed spawn would otherwise surface only as a timeout.
    let owner_results = tool_results(owner.session());
    let spawned = parse_result(&owner_results, "spawn-long-runner");
    assert_eq!(spawned["agent_id"], "agent-1", "{spawned}");
    assert_eq!(spawned["agent_path"], "/root/long-runner", "{spawned}");
    // The first session stays alive and its worker keeps running.
    let running = wait_for_detached_record(&rig.roster_path(), "agent-1").await;
    assert_eq!(running["status"]["state"], "running", "{running}");

    // The first session's next turn reattaches its still-live worker in place:
    // it clears the run-scoped detachment marker and never starts a second one.
    let output = owner.complete("confirm my worker").await.unwrap();
    assert_eq!(output.text, "first session observed its worker");
    let owner_results = tool_results(owner.session());
    let listed = parse_result(&owner_results, "list-first-session");
    let agents = listed["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 1, "{listed}");
    assert_eq!(agents[0]["detached"], false, "{listed}");
    assert_eq!(agents[0]["live_task"], true, "{listed}");

    // A duplicate open of the same session cannot take the durable fleet.
    let mut duplicate = rig.open_agent(&server, &session_path);
    let output = duplicate
        .complete("observe the refusal")
        .await
        .expect("the duplicate session must still run its own turn");
    assert_eq!(output.text, "duplicate session observed the refusal");

    let duplicate_results = tool_results(duplicate.session());
    let listed = parse_result(&duplicate_results, "list-duplicate");
    let agents = listed["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 1, "{listed}");
    let refused = &agents[0];
    assert_eq!(refused["agent_id"], "agent-1");
    assert_eq!(refused["detached"], true, "{listed}");
    assert_eq!(refused["live_task"], false, "{listed}");
    let diagnostic = refused["diagnostic"]
        .as_str()
        .expect("a refused reattach names why");
    assert!(
        diagnostic.contains("another live session owner holds the durable fleet lease"),
        "{diagnostic}"
    );
    assert!(state.unexpected.lock().unwrap().is_empty());

    // Exactly one worker was ever spawned, and exactly one provider request
    // reached the child session across both sessions.
    let requests = state.requests.lock().unwrap();
    let child_requests = requests
        .iter()
        .filter(|request| request_system(request).contains("You are /root/long-runner"))
        .collect::<Vec<_>>();
    assert_eq!(child_requests.len(), 1, "{child_requests:?}");
}
