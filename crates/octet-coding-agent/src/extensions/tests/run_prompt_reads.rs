use super::*;
use crate::app::App;
use octet_ai::{
    AiError, AssistantMessage, AssistantPart, Cost, Diagnostic, HostStreamModel,
    HostStreamTransport, Request, Response, ResponseStream, StopReason, StreamEvent, Usage,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const EXTENSION: &str = "run-prompt-reader";
const FIXTURE: &str = r#"import collections
import json
import os
import sys

pending = {}
turns = collections.deque()
scopes = collections.deque()
latest = None
sequence = 0
trace = os.path.join(os.environ['OCTET_WORKSPACE'], 'prompt-reads.jsonl')

def record(row):
    row['pid'] = os.getpid()
    with open(trace, 'a', encoding='utf-8') as log:
        log.write(json.dumps(row, ensure_ascii=False) + '\n')

def send(frame):
    print(json.dumps(frame, ensure_ascii=False), flush=True)

def read_prompt(phase, owner, scope, parent, completion=None):
    global sequence
    sequence += 1
    params = {'parent_request_id': parent, 'resource_owner': owner}
    if scope is not None:
        params['scope'] = scope
    request = {'jsonrpc': '2.0', 'id': 'prompt-' + str(sequence),
               'method': 'context/system_prompt', 'params': params}
    pending[request['id']] = (phase, request, completion)
    send(request)

for line in sys.stdin:
    frame = json.loads(line)
    record({'kind': 'wire', 'frame': frame})
    method = frame.get('method')
    params = frame.get('params', {})
    if method == 'initialize':
        features = frame['params']['protocol']['required_features'] + [
            'lifecycle_events', 'session_entries', 'system_prompt_read',
            'run_system_prompt_read_v1']
        send({'jsonrpc': '2.0', 'id': frame['id'], 'result': {
            'api_version': '0.4', 'tools': [],
            'commands': [{'name': name, 'description': name} for name in ['idle', 'scoped']],
            'protocol': {'version': '0.4', 'features': features,
                         'limits': {'max_concurrent_requests': 1},
                         'lifecycle_events': ['turn/started', 'turn/settled']}}})
    elif method == 'turn/started':
        turns.append(params)
    elif method == 'context/system_prompt_scope':
        latest = params
        scopes.append(params)
    elif method == 'hook/run':
        context = params['context']
        read_prompt(params['hook'], context['resource_owner'],
                    context['system_prompt_scope'], frame['id'], ('hook', frame['id']))
    elif method == 'command/execute':
        context = params['context']
        if params['name'] == 'idle':
            assert 'system_prompt_scope' not in context
            read_prompt('idle', context['resource_owner'], None, frame['id'],
                        ('command', frame['id']))
        else:
            assert latest is not None
            read_prompt('scoped', latest['resource_owner'], latest['scope'], 0,
                        ('command', frame['id']))
    elif method == 'shutdown':
        send({'jsonrpc': '2.0', 'id': frame['id'], 'result': {}})
        break
    elif frame.get('id') in pending:
        phase, request, completion = pending.pop(frame['id'])
        row = {'kind': 'read', 'phase': phase, 'request': request, 'response': frame}
        record(row)
        if completion is not None:
            kind, parent = completion
            result = ({'disposition': {'action': 'continue'}, 'context': [], 'notifications': []}
                      if kind == 'hook' else
                      {'text': json.dumps(row, ensure_ascii=False), 'context': [], 'notifications': []})
            send({'jsonrpc': '2.0', 'id': parent, 'result': result})
    while turns and scopes:
        turns.popleft()
        scope = scopes.popleft()
        read_prompt('turn/started', scope['resource_owner'], scope['scope'], 0)
"#;

struct LocalInference(Arc<Mutex<Vec<Request>>>);

#[async_trait::async_trait]
impl HostStreamTransport for LocalInference {
    async fn stream(
        &self,
        model: HostStreamModel,
        request: Request,
        _: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        self.0.lock().unwrap().push(request);
        Ok(Box::pin(futures_util::stream::iter([
            Ok(StreamEvent::Started { response_id: None }),
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

async fn disposable_home(name: &str) -> bool {
    const CHILD: &str = "OCTET_TEST_RUN_PROMPT_READ_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        return false;
    }
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().canonicalize().unwrap();
    let temporary = home.join("tmp");
    std::fs::create_dir(&temporary).unwrap();
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("extensions::tests::run_prompt_reads::{name}"),
                "--nocapture",
                "--test-threads=1",
            ])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env(CHILD, name)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("TMPDIR", &temporary)
            .env("TMP", &temporary)
            .env("TEMP", &temporary)
            .env("LANG", "C.UTF-8")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .current_dir(&home)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("isolated prompt-read test exceeded its deadline")
    .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "isolated {name} failed\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

fn app(root: &Path) -> (App, Arc<Mutex<Vec<Request>>>) {
    let workspace = root.join("workspace");
    let extensions = root.join("extensions");
    let extension = extensions.join(EXTENSION);
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&extension).unwrap();
    let script = extension.join("reader.py");
    std::fs::write(&script, FIXTURE).unwrap();
    std::fs::write(
        extension.join(EXTENSION_MANIFEST_FILENAME),
        format!(
            r#"name = "run-prompt-reader"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "python3"
args = [{}]
[capabilities]
system_prompt = true
[contributes]
commands = ["idle", "scoped"]
hooks = ["model_turn_start", "model_turn_end"]
"#,
            json!(script),
        ),
    )
    .unwrap();
    let mut config = executable_extension_config(&workspace, &extensions, EXTENSION);
    config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    config.sandbox.allow_process = true;
    let boot = crate::app::bootstrap::bootstrap(config).unwrap();
    let launch = crate::app::bootstrap::resolve_launch_print(&boot, EXTENSION).unwrap();
    let mut app =
        crate::app::bootstrap::build_app(boot, launch, "App fallback prompt".into()).unwrap();
    assert!(
        app.executable_extensions
            .processes
            .iter()
            .any(|process| process.descriptor().manifest.name == EXTENSION && process.is_running()),
        "native reader was not admitted: {}",
        app.executable_extensions.inspect_text()
    );
    let requests = Arc::new(Mutex::new(Vec::new()));
    app.client.register_host_stream_transport(
        app.model.endpoint.id.clone(),
        Arc::new(LocalInference(requests.clone())),
    );
    (app, requests)
}

fn reader(app: &App) -> ExtensionProcess {
    app.executable_extensions
        .processes
        .iter()
        .find(|process| process.descriptor().manifest.name == EXTENSION)
        .unwrap()
        .clone()
}

fn rows(app: &App) -> Vec<Value> {
    std::fs::read_to_string(app.config.workspace.join("prompt-reads.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

async fn idle_read(app: &mut App) -> Value {
    let process = reader(app);
    let context = extension_execution_context(
        &process,
        app.executable_extensions.resource_owner.as_deref(),
    );
    let mut events = process.subscribe();
    let mut executing = Box::pin(process.execute_command("idle", Vec::new(), context));
    let output = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                output = &mut executing => break output.unwrap(),
                event = events.recv() => {
                    event.unwrap();
                    app.executable_extensions
                        .drain_events_inner(ForegroundAdmission::Queued);
                    app.executable_extensions
                        .apply_session_host_requests(&mut app.agent, &app.sessions);
                }
            }
        }
    })
    .await
    .expect("unscoped idle read did not reach the real App Agent");
    serde_json::from_str(&output.text).unwrap()
}

async fn scoped_read(
    process: &ExtensionProcess,
    context: octet_agent::extension_process::ExtensionExecutionContext,
) -> Value {
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        process.execute_command("scoped", Vec::new(), context),
    )
    .await
    .expect("run-scoped read waited for a frontend Agent borrow")
    .unwrap();
    serde_json::from_str(&output.text).unwrap()
}

fn assert_expired(row: &Value, prompt: &str) {
    assert_eq!(row["response"]["error"]["code"], -32002, "{row:#?}");
    assert!(row["response"].get("result").is_none(), "{row:#?}");
    assert!(!row["response"].to_string().contains(prompt));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_run_prompt_reads_during_started_and_model_callbacks_then_reads_live_idle_agent() {
    if disposable_home(
        "native_run_prompt_reads_during_started_and_model_callbacks_then_reads_live_idle_agent",
    )
    .await
    {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let (mut app, requests) = app(directory.path());
    let expected = "  exact Agent system 文\nwith whitespace\t  ";
    app.agent.set_system_prompt(expected);
    assert_ne!(app.agent.system_prompt(), app.system);
    let initial = idle_read(&mut app).await;
    assert_eq!(initial["response"]["result"], json!({"text": expected}));
    let owner = app.agent.session().resource_owner_key();
    let process = reader(&app);
    assert!(process.supports_feature("run_system_prompt_read_v1"));
    let context = extension_execution_context(
        &process,
        app.executable_extensions.resource_owner.as_deref(),
    );
    let mut run = app.agent.prompt("hello").await.unwrap();
    let turn = app.executable_extensions.begin_turn().await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = run.next().await.expect("run lost before terminal event");
            if let octet_agent::AgentEvent::RunFinished { reason, .. } = event {
                assert!(
                    matches!(reason, octet_agent::FinishReason::Completed),
                    "{reason:?}"
                );
                break;
            }
        }
    })
    .await
    .expect("native callbacks deadlocked on the mutably borrowed Agent");
    assert_expired(&scoped_read(&process, context).await, expected);
    drop(run);
    app.executable_extensions
        .settle_turn(turn, &crate::modes::HostRunOutcome::Completed)
        .await;
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert_eq!(app.agent.system_prompt(), expected);
    let transcript = rows(&app);
    let reads = transcript
        .iter()
        .filter(|row| row["kind"] == "read")
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 5, "{transcript:#?}");
    let scope = transcript
        .iter()
        .find(|row| row["frame"]["method"] == "context/system_prompt_scope")
        .unwrap();
    for phase in ["turn/started", "model_turn_start", "model_turn_end"] {
        let row = reads.iter().find(|row| row["phase"] == phase).unwrap();
        assert_eq!(
            row["response"]["result"],
            json!({"text": expected}),
            "{row:#?}"
        );
        assert!(row["response"].get("error").is_none());
        let params = &row["request"]["params"];
        assert_eq!(params["scope"], scope["frame"]["params"]["scope"]);
        assert_eq!(
            params["resource_owner"],
            scope["frame"]["params"]["resource_owner"]
        );
        assert_eq!(params["resource_owner"]["session_id"], owner);
        assert_eq!(
            params["resource_owner"]["extension_instance_id"],
            process.extension_instance_id()
        );
        assert_eq!(
            params["resource_owner"]["process_generation"],
            process.health_snapshot().generation
        );
        if phase == "turn/started" {
            assert_eq!(params["parent_request_id"], 0);
            assert!(transcript.iter().any(|row| row["frame"]["method"] == phase));
        } else {
            let callback = transcript
                .iter()
                .find(|row| {
                    row["frame"]["method"] == "hook/run" && row["frame"]["params"]["hook"] == phase
                })
                .unwrap();
            assert_eq!(params["parent_request_id"], callback["frame"]["id"]);
            assert_eq!(
                callback["frame"]["params"]["context"]["system_prompt_scope"],
                params["scope"]
            );
        }
    }
    for changed in ["new idle Agent system\nnot the old run", ""] {
        app.agent.set_system_prompt(changed);
        let row = idle_read(&mut app).await;
        assert_eq!(row["response"]["result"], json!({"text": changed}));
        assert!(row["request"]["params"].get("scope").is_none());
        assert_eq!(row["pid"], reads[0]["pid"]);
    }
    assert_eq!(requests.lock().unwrap().len(), 1);
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_unpolled_run_prompt_scope_expires_on_abort_and_drop() {
    if disposable_home("native_unpolled_run_prompt_scope_expires_on_abort_and_drop").await {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let (mut app, requests) = app(directory.path());
    let process = reader(&app);
    let context = extension_execution_context(
        &process,
        app.executable_extensions.resource_owner.as_deref(),
    );
    let mut previous_scope = Value::Null;
    for abort in [false, true] {
        let expected = if abort {
            "cancelled run secret"
        } else {
            "dropped run secret"
        };
        app.agent.set_system_prompt(expected);
        let run = app.agent.prompt("unpolled local run").await.unwrap();
        let live = scoped_read(&process, context.clone()).await;
        assert_eq!(live["response"]["result"], json!({"text": expected}));
        assert_ne!(live["request"]["params"]["scope"], previous_scope);
        previous_scope = live["request"]["params"]["scope"].clone();
        if abort {
            run.control().abort();
            assert_expired(&scoped_read(&process, context.clone()).await, expected);
        }
        drop(run);
        assert_expired(&scoped_read(&process, context.clone()).await, expected);
        app.agent.set_system_prompt("idle after unpolled run");
        let idle = idle_read(&mut app).await;
        assert_eq!(
            idle["response"]["result"],
            json!({"text": app.agent.system_prompt()})
        );
    }
    assert!(requests.lock().unwrap().is_empty());
    app.executable_extensions.shutdown().await;
}
