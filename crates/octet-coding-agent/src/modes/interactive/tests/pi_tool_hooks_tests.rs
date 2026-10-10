//! Real App/agent + reviewed Node Pi factory, built-in tools, policy and durable
//! session. Only inference is scripted on loopback; no synthetic protocol peer.
#![cfg(unix)]
use super::pi_contract_support::pi_app;
use super::support::{fast_response, scripted_model, text_turn};
use super::*;
use base64::Engine as _;
use crossterm::event::KeyEvent;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn tool_turn(calls: &[(&str, Value)]) -> String {
    let mut events = vec![json!({"type":"message_start", "message":{
        "id":"tool-hooks", "usage":{"input_tokens":1,"output_tokens":0}}})];
    for (index, (name, arguments)) in calls.iter().enumerate() {
        events.extend([
            json!({"type":"content_block_start", "index":index,
                "content_block":{"type":"tool_use", "id":format!("call-{index}"), "name":name}}),
            json!({"type":"content_block_delta", "index":index,
                "delta":{"type":"input_json_delta", "partial_json":arguments.to_string()}}),
            json!({"type":"content_block_stop", "index":index}),
        ]);
    }
    events.extend([
        json!({"type":"message_delta", "delta":{"stop_reason":"tool_use"}, "usage":{"output_tokens":1}}),
        json!({"type":"message_stop"}),
    ]);
    events
        .into_iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect()
}

#[derive(Clone)]
struct CaptureToolFacts {
    events: Arc<Mutex<Vec<AgentEvent>>>,
    input: tokio::sync::mpsc::UnboundedSender<std::io::Result<Event>>,
}

impl octet_agent::EventObserver for CaptureToolFacts {
    fn on_event(&self, event: &AgentEvent) {
        // Drive genuine approval UI by cancelling its picker. Never
        // authorize through a test-only broker or respond to the receipt here.
        if matches!(
            event,
            AgentEvent::ToolProgress {
                progress: ToolProgress::Confirmation(_),
                ..
            }
        ) {
            let _ = self.input.send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            ))));
        }
        let captured = match event {
            AgentEvent::ToolProgress {
                id,
                progress: ToolProgress::Confirmation(request),
            } => Some(AgentEvent::ToolProgress {
                id: id.clone(),
                progress: ToolProgress::Confirmation(request.clone()),
            }),
            AgentEvent::ToolPolicyDecision { id, name, decision } => {
                Some(AgentEvent::ToolPolicyDecision {
                    id: id.clone(),
                    name: name.clone(),
                    decision: decision.clone(),
                })
            }
            AgentEvent::ToolFinished {
                id,
                result,
                duration,
            } => Some(AgentEvent::ToolFinished {
                id: id.clone(),
                result: result.clone(),
                duration: *duration,
            }),
            _ => None,
        };
        if let Some(event) = captured {
            self.events.lock().unwrap().push(event);
        }
    }
}

struct Acceptance {
    _directory: tempfile::TempDir,
    app: App,
    requests: Vec<Value>,
    events: Vec<AgentEvent>,
    trace: Vec<Value>,
    durable: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Execution {
    Live,
    Terminate,
    Trusted,
    Recovery,
    Background,
}

async fn exercise(factory: &str, calls: Vec<(&str, Value)>, external_paths: bool) -> Acceptance {
    exercise_mode(factory, calls, external_paths, Execution::Live).await
}

async fn exercise_mode(
    factory: &str,
    calls: Vec<(&str, Value)>,
    external_paths: bool,
    execution: Execution,
) -> Acceptance {
    let server = MockServer::start().await;
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let capture = requests.clone();
    let first = match execution {
        Execution::Live | Execution::Trusted | Execution::Terminate => tool_turn(&calls),
        Execution::Recovery => text_turn(),
        Execution::Background => {
            let item = json!({"id":"fc-read", "type":"function_call", "call_id":"call-0", "name":"read", "async":true, "arguments":calls[0].1.to_string()});
            [json!({"type":"response.created", "response":{"id":"background"}}),
             json!({"type":"response.output_item.added", "output_index":0, "item":item}),
             json!({"type":"response.output_item.done", "output_index":0, "item":item}),
             json!({"type":"response.completed", "response":{"id":"background", "output":[item], "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}})]
                .into_iter().map(|event| format!("data: {event}\n\n")).collect()
        }
    };
    Mock::given(method("POST"))
        .and(path(if execution == Execution::Background {
            "/v1/responses"
        } else {
            "/v1/messages"
        }))
        .respond_with(move |request: &wiremock::Request| {
            let mut requests = capture.lock().unwrap();
            let response = if requests.is_empty() {
                first.clone()
            } else if execution == Execution::Background {
                fast_response()
            } else {
                text_turn()
            };
            requests.push(serde_json::from_slice(&request.body).unwrap());
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(response)
        })
        .mount(&server)
        .await;
    let (directory, mut app) = pi_app(factory);
    let mut model = scripted_model(&server.uri());
    if execution == Execution::Background {
        Arc::make_mut(&mut model.spec).protocol = octet_ai::Protocol::OpenAiResponses;
        Arc::make_mut(&mut model.spec)
            .capabilities
            .responses_features
            .async_tools = true;
        Arc::make_mut(&mut model.endpoint)
            .runtime
            .responses_features
            .async_tools = true;
    }
    app.catalog
        .register_endpoint((*model.endpoint).clone())
        .unwrap();
    app.catalog.register_model((*model.spec).clone()).unwrap();
    std::fs::write(
        directory.path().join("original.txt"),
        "ORIGINAL_PRIVATE_CONTENT",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("changed.txt"),
        "CHANGED_PRIVATE_CONTENT",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("fixture.png"),
        base64::engine::general_purpose::STANDARD
            .decode(HOOK_PNG)
            .unwrap(),
    )
    .unwrap();
    let trace_path = directory.path().join("trace.jsonl");
    app.config.effect_policy = if execution == Execution::Trusted {
        octet_agent::EffectPolicy::UnsafeHost
    } else {
        octet_agent::EffectPolicy::Controlled
    };
    app.config.sandbox.allow_external_paths = external_paths;
    // Rebuild is the real App's normal extension-host/tool-hook installation,
    // not a hand-built ExtensionHost that accidentally omits the fleet's hooks.
    app = rebuild_app(app, Some(model), None, None, None).unwrap();
    assert!(
        app.executable_extensions
            .summaries()
            .iter()
            .any(|s| s.name == "octet-pi-compat" && s.running),
        "{}",
        app.executable_extensions.inspect_text()
    );
    // The reviewed adapter reserves resources_discover even without callbacks.
    // Like the interactive prompt path, settle its idle-owned startup barrier
    // before inference; otherwise ResourceProviderGuard rejects every request.
    app.executable_extensions
        .activate_session_lifecycle_driver();
    let mut startup_shell = InteractiveShell::test_shell();
    let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut startup_input = futures_util::stream::poll_fn(move |cx| input_rx.poll_recv(cx));
    tokio::time::timeout(
        Duration::from_secs(20),
        resource_paths::refresh_resource_paths(&mut app, &mut startup_shell, &mut startup_input),
    )
    .await
    .expect("native tool-hook startup timed out")
    .unwrap();
    assert!(!app.resource_paths_pending());
    if execution == Execution::Recovery {
        app.agent
            .session_mut()
            .append(EntryValue::Message(octet_ai::Message::Assistant(
                octet_ai::AssistantMessage {
                    content: calls
                        .iter()
                        .enumerate()
                        .map(|(index, (name, arguments))| {
                            octet_ai::AssistantPart::ToolCall(octet_ai::ToolCall {
                                id: octet_ai::ToolCallId(format!("call-{index}")),
                                name: (*name).into(),
                                arguments_json: arguments.to_string(),
                                argument_error: None,
                                async_execution: false,
                            })
                        })
                        .collect(),
                    model: app.model.spec.id.clone(),
                    protocol: app.model.spec.protocol,
                },
            )))
            .unwrap();
    }
    let captured_events = Arc::new(Mutex::new(Vec::new()));
    app.agent.observe(CaptureToolFacts {
        events: captured_events.clone(),
        input: input_tx,
    });
    app.executable_extensions.refresh_host_state(
        app.agent.session(),
        &app.model,
        &app.reasoning,
        &app.sessions,
    );
    let composition = tokio::time::timeout(
        Duration::from_secs(20),
        app.executable_extensions
            .compose_prompt(&app.system, "exercise reviewed tool hooks".into()),
    )
    .await
    .expect("native tool-hook prompt composition timed out")
    .unwrap();
    app.agent.set_system_prompt(composition.system);
    let mut prompt: octet_agent::UserInput = composition.prompt.into();
    prompt.custom_messages.extend(composition.custom_messages);
    let inspection = ActiveRunInspection::capture(&app);
    let mut run = tokio::time::timeout(Duration::from_secs(20), app.agent.prompt(prompt))
        .await
        .expect("native tool-hook prompt admission timed out")
        .unwrap();
    let turn = tokio::time::timeout(
        Duration::from_secs(20),
        app.executable_extensions.begin_turn(),
    )
    .await
    .expect("native tool-hook lifecycle start timed out");
    app.executable_extensions
        .commit_prompt_context(composition.pending_context_count);
    let id = startup_shell.begin_run("tool-hooks");
    startup_shell.set_awaiting_provider(id);
    let control = run.control();
    let mut ticker = tokio::time::interval(Duration::from_millis(16));
    let ended = tokio::time::timeout(
        Duration::from_secs(20),
        drive_active_run(
            &mut run,
            &control,
            &mut startup_shell,
            &mut startup_input,
            &mut ticker,
            &mut VecDeque::new(),
            &mut false,
            None,
            None,
            &mut app.executable_extensions,
            &mut false,
            &inspection,
            &mut None,
        ),
    )
    .await
    .expect("native tool-hook run timed out")
    .unwrap();
    drop(run);
    tokio::time::timeout(
        Duration::from_secs(20),
        app.executable_extensions.settle_turn(turn, &ended),
    )
    .await
    .expect("native tool-hook lifecycle settlement timed out");
    assert_eq!(
        ended,
        HostRunOutcome::Completed,
        "{}",
        startup_shell.debug_snapshot()
    );
    let events = std::mem::take(&mut *captured_events.lock().unwrap());
    tokio::time::timeout(
        Duration::from_secs(20),
        app.executable_extensions.shutdown(),
    )
    .await
    .expect("native tool-hook process shutdown timed out");
    let count = requests.lock().unwrap().len();
    match execution {
        Execution::Live | Execution::Trusted => assert_eq!(count, 2, "{events:#?}"),
        Execution::Terminate => assert_eq!(count, 1, "{events:#?}"),
        Execution::Recovery => assert_eq!(count, 1, "{events:#?}"),
        Execution::Background => assert!((2..=3).contains(&count), "{events:#?}"),
    }
    let durable = std::fs::read_to_string(app.agent.session().path()).unwrap();
    // Opening the real durable session must succeed after replacement.
    let reopened = Session::open_read_only(app.agent.session().path()).unwrap();
    assert_eq!(
        reopened.entries().len(),
        app.agent.session().entries().len()
    );
    let trace = std::fs::read_to_string(trace_path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let requests = requests.lock().unwrap().clone();
    Acceptance {
        _directory: directory,
        app,
        requests,
        events,
        trace,
        durable,
    }
}

fn results(acceptance: &Acceptance) -> Vec<&Value> {
    acceptance.requests.last().unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter(|part| part["type"] == "tool_result")
        .collect()
}

fn decision(acceptance: &Acceptance) -> &octet_agent::ToolPolicyDecision {
    acceptance
        .events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolPolicyDecision { decision, .. } => Some(decision),
            _ => None,
        })
        .expect("real host must publish its policy decision")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_tool_hooks_parallel_wave_executes_chained_changed_arguments() {
    let acceptance = exercise(r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  pi.on('tool_call', e => { e.input.path = 'changed'; });
  pi.on('tool_call', e => { e.input.path += '.txt'; });
  pi.on('tool_result', e => { appendFileSync(TRACE, JSON.stringify({input:e.input, content:e.content}) + '\n'); });
};"#, vec![("read", json!({"path":"original.txt"})), ("read", json!({"path":"original.txt"}))], false).await;
    let results = results(&acceptance);
    assert_eq!(results.len(), 2);
    for result in results {
        assert!(
            result["content"]
                .to_string()
                .contains("CHANGED_PRIVATE_CONTENT"),
            "{result}"
        );
        assert!(!result.to_string().contains("ORIGINAL_PRIVATE_CONTENT"));
        assert_ne!(result["is_error"], true);
    }
    assert_eq!(acceptance.trace.len(), 2);
    assert!(acceptance
        .trace
        .iter()
        .all(|e| e["input"]["path"] == "changed.txt"));
    assert!(decision(&acceptance).allowed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_tool_hooks_mutation_is_authorized_not_original_arguments() {
    let outside = tempfile::tempdir().unwrap();
    let outside_path = outside.path().join("outside.txt");
    std::fs::write(&outside_path, "OUTSIDE_MUST_NOT_BE_READ").unwrap();
    let factory = format!(
        r#"export default pi => pi.on('tool_call', e => {{ e.input.path = {}; }});"#,
        serde_json::to_string(&outside_path).unwrap()
    );
    let denied = exercise(
        &factory,
        vec![("read", json!({"path":"original.txt"}))],
        true,
    )
    .await;
    assert!(!decision(&denied).allowed);
    assert_eq!(
        decision(&denied).denial_code,
        Some(octet_agent::ToolPolicyDenialCode::EffectHostReadDenied)
    );
    assert_eq!(results(&denied)[0]["is_error"], true);
    assert!(!denied.requests[1]
        .to_string()
        .contains("OUTSIDE_MUST_NOT_BE_READ"));
    assert!(!denied.durable.contains("OUTSIDE_MUST_NOT_BE_READ"));
    let allowed = exercise(
        "export default pi => pi.on('tool_call', e => { e.input.path = 'changed.txt'; });",
        vec![("read", json!({"path":outside_path}))],
        false,
    )
    .await;
    // Original out-of-workspace args are not a parallel candidate: this proves
    // the sequential dispatch also mutates before its real confinement gate.
    assert!(decision(&allowed).allowed);
    assert!(results(&allowed)[0]
        .to_string()
        .contains("CHANGED_PRIVATE_CONTENT"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_tool_hooks_mutated_workspace_write_requires_real_approval() {
    let acceptance = exercise(
        "export default pi => pi.on('tool_call', e => { e.input.path = 'changed.txt'; });",
        vec![(
            "write",
            json!({"path":"original.txt", "content":"MUST_NOT_BE_WRITTEN"}),
        )],
        false,
    )
    .await;
    assert!(!decision(&acceptance).allowed);
    assert_eq!(
        decision(&acceptance).effect,
        Some(octet_agent::ToolEffect::WorkspaceMutation)
    );
    assert_eq!(
        decision(&acceptance).denial_code,
        Some(octet_agent::ToolPolicyDenialCode::ApprovalDenied)
    );
    let details: Vec<_> = acceptance
        .events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolProgress {
                progress: ToolProgress::Confirmation(request),
                ..
            } => request.detail.as_deref(),
            _ => None,
        })
        .collect();
    assert_eq!(
        details.len(),
        1,
        "the real broker must request exactly one approval"
    );
    assert!(details[0].contains("changed.txt"));
    assert!(!details[0].contains("original.txt"));
    assert_eq!(results(&acceptance)[0]["is_error"], true);
    assert_eq!(
        std::fs::read_to_string(acceptance._directory.path().join("original.txt")).unwrap(),
        "ORIGINAL_PRIVATE_CONTENT"
    );
    assert_eq!(
        std::fs::read_to_string(acceptance._directory.path().join("changed.txt")).unwrap(),
        "CHANGED_PRIVATE_CONTENT"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_tool_hooks_block_stops_handlers_and_execution() {
    let acceptance = exercise(
        r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  pi.on('tool_call', () => ({block:true, reason:'PI_BLOCK_REASON'}));
  pi.on('tool_call', () => { appendFileSync(TRACE, 'unreachable\n'); });
};"#,
        vec![("read", json!({"path":"original.txt"}))],
        false,
    )
    .await;
    assert!(!decision(&acceptance).allowed);
    assert_eq!(results(&acceptance)[0]["is_error"], true);
    assert!(results(&acceptance)[0]
        .to_string()
        .contains("PI_BLOCK_REASON"));
    assert!(acceptance.trace.is_empty());
    assert!(!acceptance.durable.contains("ORIGINAL_PRIVATE_CONTENT"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_tool_hooks_redaction_reaches_provider_and_disk_only_after_real_execution() {
    let acceptance = exercise(
        r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  pi.on('tool_result', e => {
    if (!e.content[0].text.includes('ORIGINAL_PRIVATE_CONTENT')) throw new Error('not a real read');
    appendFileSync(TRACE, JSON.stringify({input:e.input, sawRealRead:true}) + '\n');
    return {content:[{type:'text', text:'REDACTED_ONE'}], details:{redacted:true}};
  });
  pi.on('tool_result', () => { throw new Error('skip this handler'); });
  pi.on('tool_result', e => ({content:[{type:'text',text:e.content[0].text + '_TWO'}]}));
};"#,
        vec![("read", json!({"path":"original.txt"}))],
        false,
    )
    .await;
    assert_eq!(acceptance.trace.len(), 1);
    assert_eq!(acceptance.trace[0]["sawRealRead"], true);
    assert!(results(&acceptance)[0]
        .to_string()
        .contains("REDACTED_ONE_TWO"));
    assert!(acceptance.durable.contains("REDACTED_ONE_TWO"));
    assert!(!acceptance.requests[1]
        .to_string()
        .contains("ORIGINAL_PRIVATE_CONTENT"));
    assert!(!acceptance.durable.contains("ORIGINAL_PRIVATE_CONTENT"));
    assert!(acceptance.durable.contains("redacted"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_tool_hooks_policy_denial_cannot_be_replaced_with_success() {
    let outside = tempfile::tempdir().unwrap();
    let outside_path = outside.path().join("outside.txt");
    std::fs::write(&outside_path, "OUTSIDE_MUST_NOT_BE_READ").unwrap();
    let acceptance = exercise(
        r#"
import { appendFileSync } from 'node:fs';
export default pi => pi.on('tool_result', e => {
  appendFileSync(TRACE, JSON.stringify({isError:e.isError}) + '\n');
  return {content:[{type:'text', text:'SAFE_DENIAL_REPLACEMENT'}], isError:false};
});"#,
        vec![("read", json!({"path":outside_path}))],
        true,
    )
    .await;
    assert_eq!(acceptance.trace, vec![json!({"isError":true})]);
    assert!(!decision(&acceptance).allowed);
    assert_eq!(decision(&acceptance).authorization, None);
    assert_eq!(results(&acceptance)[0]["is_error"], true);
    assert!(results(&acceptance)[0]
        .to_string()
        .contains("SAFE_DENIAL_REPLACEMENT"));
    assert!(!acceptance.durable.contains("OUTSIDE_MUST_NOT_BE_READ"));
    for entry in acceptance.app.agent.session().entries() {
        if let EntryValue::Message(octet_ai::Message::User(message)) = &entry.value {
            for part in &message.content {
                if let octet_ai::UserPart::ToolResult(result) = part {
                    assert!(result.is_error);
                }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_tool_hooks_ordinary_errors_can_be_replaced_with_success() {
    let acceptance = exercise("export default pi => pi.on('tool_result', () => ({content:[{type:'text',text:'RECOVERED'}],isError:false}));", vec![("read", json!({"path":"missing.txt"}))], false).await;
    assert!(decision(&acceptance).allowed);
    assert!(results(&acceptance)[0].to_string().contains("RECOVERED"));
    assert_ne!(results(&acceptance)[0]["is_error"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_tool_hooks_does_not_revalidate_schema_after_mutation() {
    let acceptance = exercise_mode(r#"
import { Type } from '@sinclair/typebox';
export default pi => {
  pi.registerTool({name:'echo', label:'Echo', description:'Echo input type', parameters:Type.Object({n:Type.Number()}),
    execute: async (_id, input) => ({content:[{type:'text', text:typeof input.n + ':' + input.n}], details:undefined})});
  pi.on('tool_call', e => { e.input.n = 'mutated'; });
};"#, vec![("echo", json!({"n":1}))], false, Execution::Trusted).await;
    assert!(decision(&acceptance).allowed);
    assert!(results(&acceptance)[0]
        .to_string()
        .contains("string:mutated"));
    assert_ne!(results(&acceptance)[0]["is_error"], true);
}

const HOOK_PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=";

fn finished(
    acceptance: &Acceptance,
) -> Vec<&Result<octet_agent::ToolOutput, octet_agent::ToolError>> {
    acceptance
        .events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolFinished { result, .. } => Some(result),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_tool_hooks_block_termination_is_unanimous_and_persists_siblings() {
    let factory = r#"export default pi => {
      pi.on('tool_call', e => ({block:true, reason:'TERMINATED_' + e.toolCallId, terminate:true}));
      pi.on('tool_result', () => ({isError:false, details:{stillDenied:true}}));
    };"#;
    let acceptance = exercise_mode(
        factory,
        vec![
            ("read", json!({"path":"original.txt"})),
            ("read", json!({"path":"changed.txt"})),
        ],
        false,
        Execution::Terminate,
    )
    .await;
    assert_eq!(finished(&acceptance).len(), 2);
    for result in finished(&acceptance) {
        let error = result.as_ref().unwrap_err();
        assert!(error.output().unwrap().terminates_run());
        assert!(error.output().unwrap().is_error());
        assert_eq!(
            error.output().unwrap().metadata().unwrap()["pi_details"]["stillDenied"],
            true
        );
    }
    assert!(acceptance.durable.contains("TERMINATED_call-0"));
    assert!(acceptance.durable.contains("TERMINATED_call-1"));
    assert!(!acceptance.durable.contains("ORIGINAL_PRIVATE_CONTENT"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_tool_hooks_one_termination_hint_does_not_discard_sibling() {
    let acceptance = exercise("export default pi => pi.on('tool_call', e => e.toolCallId === 'call-0' ? {block:true, terminate:true, reason:'ONE_TERMINATION'} : undefined);", vec![("read",json!({"path":"original.txt"})), ("read",json!({"path":"changed.txt"}))], false).await;
    assert_eq!(results(&acceptance).len(), 2);
    assert!(results(&acceptance)[1]
        .to_string()
        .contains("CHANGED_PRIVATE_CONTENT"));
    assert!(acceptance.durable.contains("ONE_TERMINATION"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_tool_hooks_usage_chains_is_billed_and_is_not_provider_context() {
    let acceptance = exercise(r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  pi.on('tool_result', () => ({usage:{input:7,output:3,cacheRead:2,cacheWrite:1,totalTokens:13,cost:{input:.7,output:.3,cacheRead:.2,cacheWrite:.1,total:1.3}}}));
  pi.on('tool_result', e => { appendFileSync(TRACE, JSON.stringify({usage:e.usage, id:e.toolCallId})+'\n'); return {usage:{...e.usage,input:9,totalTokens:15}}; });
};"#, vec![("read",json!({"path":"original.txt"}))], false).await;
    assert_eq!(acceptance.trace[0]["id"], "call-0");
    assert_eq!(acceptance.trace[0]["usage"]["input"], 7);
    let usage = finished(&acceptance)[0].as_ref().unwrap().usage().unwrap();
    assert_eq!(usage.input_tokens, 9);
    assert_eq!(usage.total_tokens, 15);
    assert!(acceptance
        .app
        .agent
        .session()
        .usage_records()
        .iter()
        .any(|record| record.usage == *usage));
    assert!(acceptance.durable.contains("pi_usage"));
    assert!(!acceptance.requests[1].to_string().contains("pi_usage"));
    assert!(!acceptance.requests[1].to_string().contains("totalTokens"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_tool_hooks_image_replacement_uses_owned_native_artifacts_and_order() {
    let acceptance = exercise(r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  pi.on('tool_result', e => {
    const image = e.content.find(p => p.type === 'image');
    if (!image || image.mimeType !== 'image/png') throw new Error('native read image missing');
    appendFileSync(TRACE, JSON.stringify({image,id:e.toolCallId})+'\n');
    return {content:[{type:'text',text:'IMAGE_PREFIX'},image,{type:'text',text:'IMAGE_SUFFIX'}]};
  });
  pi.on('tool_result', e => {
    if (e.content.map(p=>p.type).join(',') !== 'text,image,text') throw new Error('ordered chaining lost');
  });
};"#, vec![("read",json!({"path":"fixture.png"}))], false).await;
    assert_eq!(acceptance.trace[0]["image"]["data"], HOOK_PNG);
    assert_eq!(acceptance.trace[0]["id"], "call-0");
    let parts = results(&acceptance)[0]["content"].as_array().unwrap();
    assert_eq!(
        parts
            .iter()
            .map(|p| p["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["text", "image", "text"]
    );
    assert_eq!(parts[1]["source"]["data"], HOOK_PNG);
    assert_eq!(parts[0]["text"], "IMAGE_PREFIX");
    assert_eq!(parts[2]["text"], "IMAGE_SUFFIX");
    // Session ImageSource has no local media-reference variant (Url | Inline |
    // ProviderRef), so the replacement image is persisted inline. It must be
    // persisted once: the pre-hook original is not retained as a second copy.
    assert_eq!(
        acceptance.durable.matches(HOOK_PNG).count(),
        1,
        "replacement image persisted exactly once"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_tool_hooks_invalid_image_does_not_replace_real_result() {
    let acceptance = exercise("export default pi => pi.on('tool_result', () => ({content:[{type:'image',mimeType:'image/png',data:Buffer.from('not an image').toString('base64')}]}));", vec![("read",json!({"path":"original.txt"}))], false).await;
    assert!(results(&acceptance)[0]
        .to_string()
        .contains("ORIGINAL_PRIVATE_CONTENT"));
    assert_eq!(finished(&acceptance)[0].as_ref().unwrap().media().len(), 0);
}

const MUTATE_AND_REDACT: &str = r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  pi.on('tool_call', e => { e.input.path = 'changed.txt'; });
  pi.on('tool_result', e => {
    appendFileSync(TRACE, JSON.stringify({input:e.input, sawChanged:e.content[0].text.includes('CHANGED_PRIVATE_CONTENT')}) + '\n');
    return {content:[{type:'text', text:'FINAL_REDACTION'}]};
  });
  pi.on('turn_end', e => {
    for (const call of e.message.content.filter(p => p.type === 'toolCall')) {
      const result = e.toolResults.find(p => p.toolCallId === call.id);
      if (!result || result.content[0].text !== 'FINAL_REDACTION')
        throw new Error('model turn ended before the replaced result settled');
    }
  });
};"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_tool_hooks_recovery_uses_mutated_arguments_and_replaced_result() {
    let acceptance = exercise_mode(
        MUTATE_AND_REDACT,
        vec![("read", json!({"path":"original.txt"}))],
        false,
        Execution::Recovery,
    )
    .await;
    assert_eq!(
        acceptance.trace,
        vec![json!({"input":{"path":"changed.txt"}, "sawChanged":true})]
    );
    assert!(results(&acceptance)[0]
        .to_string()
        .contains("FINAL_REDACTION"));
    assert!(acceptance.durable.contains("FINAL_REDACTION"));
    assert!(!acceptance.durable.contains("CHANGED_PRIVATE_CONTENT"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_tool_hooks_background_uses_mutated_arguments_and_replaced_result() {
    let acceptance = exercise_mode(
        MUTATE_AND_REDACT,
        vec![("read", json!({"path":"original.txt"}))],
        false,
        Execution::Background,
    )
    .await;
    assert_eq!(
        acceptance.trace,
        vec![json!({"input":{"path":"changed.txt"}, "sawChanged":true})]
    );
    assert!(decision(&acceptance).allowed);
    assert!(acceptance.requests.last().unwrap()["input"]
        .to_string()
        .contains("FINAL_REDACTION"));
    assert!(!acceptance
        .requests
        .last()
        .unwrap()
        .to_string()
        .contains("CHANGED_PRIVATE_CONTENT"));
    assert!(!acceptance.durable.contains("CHANGED_PRIVATE_CONTENT"));
}
