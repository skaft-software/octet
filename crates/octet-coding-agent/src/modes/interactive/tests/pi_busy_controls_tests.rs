//! Real native App/adapter controls while the Agent owns a request.
#![cfg(unix)]
use super::pi_contract_support::pi_app;
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_busy_model_controls_queue_without_mutating_the_active_request() {
    use super::support::{scripted_model, text_turn};
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };
    for (retained, retire) in [(false, false), (true, false), (false, true)] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(text_turn())
                    .set_delay(Duration::from_millis(400)),
            )
            .mount(&server)
            .await;
        let factory = r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  pi.on('turn_start', async (_, ctx) => {
    const select = async () => {
    const before = ctx.model.id;
    const target = ctx.modelRegistry.getAvailable().find(m => m.id === 'queued-model');
    const selected = await pi.setModel(target);
    pi.setThinkingLevel('off');
    appendFileSync(TRACE, JSON.stringify({selected, before, after:ctx.model.id})+'\n');
    };
    DISPATCH
  });
};
"#;
        let factory = factory.replace(
            "DISPATCH",
            if retained {
                "setTimeout(select, 100);"
            } else {
                "await select();"
            },
        );
        let (directory, mut app) = pi_app(&factory);
        let model = scripted_model(&server.uri());
        app.catalog
            .register_endpoint((*model.endpoint).clone())
            .unwrap();
        app.catalog.register_model((*model.spec).clone()).unwrap();
        let mut target = (*model.spec).clone();
        target.id = ModelId("queued-model".into());
        target.api_name = "queued-model".into();
        app.catalog.register_model(target).unwrap();
        app = rebuild_app(app, Some(model), None, None, None).unwrap();
        app.executable_extensions
            .activate_session_lifecycle_driver();
        let mut shell = InteractiveShell::test_shell();
        let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
        resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut input)
            .await
            .unwrap();
        request_extension_ui(&mut shell, &mut app);
        let inspection = ActiveRunInspection::capture(&app);
        let mut run = app.agent.prompt("exercise busy selection").await.unwrap();
        let control = run.control();
        let id = shell.begin_run("busy selection");
        shell.set_awaiting_provider(id);
        let mut pending = VecDeque::new();
        let outcome = tokio::time::timeout(
            Duration::from_secs(20),
            drive_active_run(
                &mut run,
                &control,
                &mut shell,
                &mut input,
                &mut pending,
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
        .expect("hook setter must not deadlock its own run")
        .unwrap();
        drop(run);
        assert_eq!(
            outcome,
            HostRunOutcome::Completed,
            "{}",
            shell.debug_snapshot()
        );
        let trace =
            std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap_or_default();
        assert!(
            !trace.is_empty(),
            "active hook setters must succeed, not be refused: {}",
            shell.debug_snapshot()
        );
        let receipt: serde_json::Value = serde_json::from_str(trace.trim()).unwrap();
        assert_eq!(receipt["selected"], true);
        assert_eq!(
            receipt["before"], receipt["after"],
            "queue admission is not application"
        );
        assert_eq!(app.agent.model().spec.id.0, "scripted");
        assert_eq!(pending.len(), 2, "both controls must survive until idle");
        let visible = shell.debug_pending_controls();
        assert!(visible.contains("model test/queued-model"), "{visible}");
        assert!(visible.contains("thinking off"), "{visible}");
        let before = app.agent.session().entries().len();
        if retire {
            app.executable_extensions.shutdown().await;
        }
        while let Some(action) = pending.pop_front() {
            let PendingIdleAction::ExtensionModelControl { owner, selection } = action else {
                panic!("extension control")
            };
            model_controls::apply_queued_control(&mut app, &mut shell, &owner, &selection);
        }
        let expected = if retire { "scripted" } else { "queued-model" };
        assert_eq!(app.model.spec.id.0, expected);
        assert_eq!(app.agent.model().spec.id.0, expected);
        assert_eq!(
            app.agent.session().entries().len(),
            before + if retire { 0 } else { 2 },
            "each live control commits once; retired owners never apply"
        );
        if retire {
            assert!(shell.debug_error().unwrap().contains("owner retired"));
        }
        assert!(pending.is_empty());
        shell.set_pending_controls(model_controls::queue_labels(&pending));
        assert!(shell.debug_pending_controls().is_empty());
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let wire: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(wire["model"], "scripted");
        app.executable_extensions.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_busy_qualified_thinking_applies_once_at_response_boundary() {
    use super::support::fast_response;
    use super::thinking_and_consent_controls_tests::reasoning_control_model;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(fast_response())
                .set_delay(Duration::from_millis(500)),
        )
        .mount(&server)
        .await;
    let (directory, mut app) = pi_app(
        r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  let once = false;
  pi.on('turn_start', (_, ctx) => {
    if (once) return; once = true;
    setTimeout(() => {
      pi.setThinkingLevel('high');
      appendFileSync(TRACE, JSON.stringify({level:pi.getThinkingLevel()})+'\n');
    }, 150);
  });
};
"#,
    );
    let model = reasoning_control_model(&server.uri());
    app.catalog
        .register_endpoint((*model.endpoint).clone())
        .unwrap();
    app.catalog.register_model((*model.spec).clone()).unwrap();
    app = rebuild_app(app, Some(model), Some(ReasoningConfig::Off), None, None).unwrap();
    app.executable_extensions
        .activate_session_lifecycle_driver();
    let mut shell = InteractiveShell::test_shell();
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut input)
        .await
        .unwrap();
    request_extension_ui(&mut shell, &mut app);
    let inspection = ActiveRunInspection::capture(&app);
    let mut run = app.agent.prompt("busy reasoning").await.unwrap();
    let control = run.control();
    let id = shell.begin_run("busy reasoning");
    shell.set_awaiting_provider(id);
    let mut pending = VecDeque::new();
    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        drive_active_run(
            &mut run,
            &control,
            &mut shell,
            &mut input,
            &mut pending,
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
    .unwrap()
    .unwrap();
    drop(run);
    assert_eq!(
        outcome,
        HostRunOutcome::Completed,
        "{}",
        shell.debug_snapshot()
    );
    let trace = std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(trace.trim()).unwrap()["level"],
        "off"
    );
    assert!(
        pending.is_empty(),
        "applied control must not replay or persist a startup preference: {pending:?}"
    );
    assert_eq!(
        app.agent.reasoning(),
        &ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let bodies: Vec<serde_json::Value> = requests
        .iter()
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect();
    for body in &bodies {
        assert_eq!(
            body["reasoning"]["effort"], "none",
            "wire baseline is immutable"
        );
    }
    assert!(bodies[1]["input"]
        .as_array()
        .unwrap()
        .iter()
        .any(
            |item| item["type"] == "configuration_update" && item["reasoning"]["effort"] == "high"
        ));
    assert_eq!(
        app.agent
            .session()
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry.value,
                EntryValue::ResponsesReasoning {
                    update: Some(_),
                    ..
                }
            ))
            .count(),
        1
    );
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_busy_hook_keeps_slash_and_picker_controls_visible_and_theme_immediate() {
    use super::support::{scripted_model, text_turn, theme_picker_key};
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };
    for picker in [false, true] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(text_turn()),
            )
            .mount(&server)
            .await;
        let (directory, mut app) = pi_app(
            r#"
import { writeFileSync, existsSync } from 'node:fs';
export default pi => pi.on('turn_start', async () => {
  writeFileSync(TRACE, 'hook active');
  while (!existsSync(TRACE + '.release')) await new Promise(resolve => setTimeout(resolve, 10));
});
"#,
        );
        let model = scripted_model(&server.uri());
        app.catalog
            .register_endpoint((*model.endpoint).clone())
            .unwrap();
        app.catalog.register_model((*model.spec).clone()).unwrap();
        let mut target = (*model.spec).clone();
        target.id = ModelId("queued-model".into());
        target.api_name = "queued-model".into();
        app.catalog.register_model(target).unwrap();
        app = rebuild_app(app, Some(model), None, None, None).unwrap();
        app.executable_extensions
            .activate_session_lifecycle_driver();
        let mut shell = InteractiveShell::test_shell();
        shell.set_runtime_config(app.config.clone());
        let mut startup = futures_util::stream::pending::<std::io::Result<Event>>();
        resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut startup)
            .await
            .unwrap();
        request_extension_ui(&mut shell, &mut app);
        let (sender, receiver) = tokio::sync::mpsc::channel(128);
        let mut input = tokio_stream::wrappers::ReceiverStream::new(receiver);
        let trace = directory.path().join("trace.jsonl");
        let stimulus = async move {
            while !trace.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let mut events = Vec::new();
            let commands = if picker {
                vec![
                    "/model",
                    "queued-model",
                    "/thinking",
                    "",
                    "/theme",
                    "light",
                    "/new",
                    "/compact",
                    "/reload",
                ]
            } else {
                vec![
                    "/model queued-model",
                    "/thinking off",
                    "/theme light",
                    "/new",
                    "/compact",
                    "/reload",
                ]
            };
            for command in commands {
                events.extend(command.chars().map(|c| theme_picker_key(KeyCode::Char(c))));
                events.push(theme_picker_key(KeyCode::Enter));
            }
            for event in events {
                sender.send(event).await.unwrap();
            }
            // The active hook remains awaited while the native input driver
            // drains all gestures. No provider request can yet have started.
            tokio::time::sleep(Duration::from_millis(300)).await;
            std::fs::write(trace.with_extension("jsonl.release"), "release").unwrap();
            std::future::pending::<()>().await;
        };
        let inspection = ActiveRunInspection::capture(&app);
        let mut run = app.agent.prompt("busy hook UI").await.unwrap();
        let control = run.control();
        let id = shell.begin_run("busy hook UI");
        shell.set_awaiting_provider(id);
        let mut pending = VecDeque::new();
        let mut quit = false;
        let mut made_tool_call = false;
        let mut deadline = None;
        let driver = drive_active_run(
            &mut run,
            &control,
            &mut shell,
            &mut input,
            &mut pending,
            &mut quit,
            None,
            None,
            &mut app.executable_extensions,
            &mut made_tool_call,
            &inspection,
            &mut deadline,
        );
        let outcome = tokio::time::timeout(Duration::from_secs(20), async {
            tokio::select! { outcome = driver => outcome.unwrap(), _ = stimulus => unreachable!() }
        })
        .await
        .unwrap();
        drop(run);
        assert_eq!(
            outcome,
            HostRunOutcome::Completed,
            "{}",
            shell.debug_snapshot()
        );
        assert_eq!(app.agent.model().spec.id.0, "scripted");
        assert_eq!(app.model.spec.id.0, "scripted");
        assert_eq!(
            shell.runtime_config().unwrap().theme.as_deref(),
            Some("light"),
            "theme applies without the idle dispatcher"
        );
        assert!(
            matches!(&pending[0], PendingIdleAction::ChangeModel(id) if id.0 == "queued-model"),
            "{pending:?}"
        );
        assert!(
            matches!(
                &pending[1],
                PendingIdleAction::ChangeThinkingLevel(ThinkingLevel::Off)
            ),
            "{pending:?}"
        );
        assert_eq!(pending[2], PendingIdleAction::SyncTheme("light".into()));
        assert_eq!(pending[3], PendingIdleAction::NewSession);
        assert_eq!(pending[4], PendingIdleAction::Compact);
        assert_eq!(pending[5], PendingIdleAction::ReloadResources);
        let visible = shell.debug_pending_controls();
        for label in [
            "model queued-model",
            "thinking off",
            "new session",
            "compact",
            "reload",
        ] {
            assert!(visible.contains(label), "{label}: {visible}");
        }
        app.executable_extensions.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_startup_hook_model_controls_use_the_idle_owner_without_deadlock() {
    let (directory, mut app) = pi_app(
        r#"
import { appendFileSync } from 'node:fs';
export default pi => pi.on('session_start', async (_, ctx) => {
  const selected = await pi.setModel(ctx.model);
  pi.setThinkingLevel('off');
  appendFileSync(TRACE, JSON.stringify({selected, level:pi.getThinkingLevel()})+'\n');
});
"#,
    );
    let model = super::support::scripted_model("http://127.0.0.1:9");
    app.catalog
        .register_endpoint((*model.endpoint).clone())
        .unwrap();
    app.catalog.register_model((*model.spec).clone()).unwrap();
    app = rebuild_app(app, Some(model), None, None, None).unwrap();
    app.executable_extensions
        .activate_session_lifecycle_driver();
    let mut shell = InteractiveShell::test_shell();
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    tokio::time::timeout(
        Duration::from_secs(10),
        resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut input),
    )
    .await
    .unwrap()
    .unwrap();
    let trace = std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap();
    let receipt: serde_json::Value = serde_json::from_str(trace.trim()).unwrap();
    assert_eq!(receipt["selected"], true);
    assert_eq!(receipt["level"], "off");
    app.executable_extensions.shutdown().await;
}

#[derive(Clone)]
struct StreamingInput {
    sender: tokio::sync::mpsc::UnboundedSender<std::io::Result<Event>>,
    fired: Arc<std::sync::atomic::AtomicBool>,
    quit: bool,
}

impl octet_agent::EventObserver for StreamingInput {
    fn on_event(&self, event: &AgentEvent) {
        if matches!(event, AgentEvent::OutputDelta { .. })
            && !self.fired.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let (code, modifiers) = if self.quit {
                (KeyCode::Char('d'), KeyModifiers::CONTROL)
            } else {
                (KeyCode::Esc, KeyModifiers::NONE)
            };
            self.sender
                .send(Ok(Event::Key(crossterm::event::KeyEvent::new(
                    code, modifiers,
                ))))
                .unwrap();
        }
    }
}

// Pi runner.ts:265-274 awaits session_shutdown. Native terminal/process ownership
// deliberately bounds that wait so a never-settling callback cannot trap exit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_qualification_stream_cancel_quit_shutdown_and_descendant_cleanup() {
    use super::pi_contract_support::command;
    use super::support::{scripted_model, text_turn, HeldApi};
    for quit in [false, true] {
        let (server, _started, release) = HeldApi::streaming(text_turn()).await;
        let (directory, mut app) = pi_app(
            r#"
import { appendFileSync } from 'node:fs';
import { spawn } from 'node:child_process';
const trace = value => appendFileSync(TRACE, JSON.stringify(value) + '\n');
export default pi => {
  pi.on('session_start', async () => {
    const child = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], {stdio:'ignore'});
    await new Promise((resolve, reject) => { child.once('spawn', resolve); child.once('error', reject); });
    trace({stage:'started', pid:process.pid, child:child.pid});
  });
  pi.registerCommand('abort-probe', {handler: (_, ctx) => {
    try { ctx.abort(); trace({stage:'abort-accepted'}); }
    catch (error) { trace({stage:'abort-refused', code:error.code}); }
  }});
  pi.on('session_shutdown', async () => { trace({stage:'shutdown'}); await new Promise(() => {}); });
};
"#,
        );
        let model = scripted_model(&server.uri);
        app.catalog
            .register_endpoint((*model.endpoint).clone())
            .unwrap();
        app.catalog.register_model((*model.spec).clone()).unwrap();
        app = rebuild_app(app, Some(model), None, None, None).unwrap();
        app.executable_extensions
            .activate_session_lifecycle_driver();
        let mut shell = InteractiveShell::test_shell();
        command(&mut app, &mut shell, "abort-probe").await.unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        app.agent.observe(StreamingInput {
            sender,
            fired: fired.clone(),
            quit,
        });
        let mut input = futures_util::stream::poll_fn(move |cx| receiver.poll_recv(cx));
        let inspection = ActiveRunInspection::capture(&app);
        let mut run = app
            .agent
            .prompt("cancel actual streamed output")
            .await
            .unwrap();
        let control = run.control();
        let id = shell.begin_run("stream probe");
        shell.set_awaiting_provider(id);
        let mut quitting = false;
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            drive_active_run(
                &mut run,
                &control,
                &mut shell,
                &mut input,
                &mut VecDeque::new(),
                &mut quitting,
                None,
                None,
                &mut app.executable_extensions,
                &mut false,
                &inspection,
                &mut None,
            ),
        )
        .await
        .expect("stream cancellation must settle without provider completion")
        .unwrap();
        drop(run);
        assert!(fired.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            outcome,
            HostRunOutcome::Aborted,
            "{}",
            shell.debug_snapshot()
        );
        assert_eq!(quitting, quit);
        drop(release); // Never finish the interrupted response, even after cancellation.
        let path = app.agent.session().path().to_owned();
        let prefix = std::fs::read(&path).unwrap();
        if !quit {
            let mut later = app
                .agent
                .prompt("later turn after cancellation")
                .await
                .unwrap();
            let mut finished = 0;
            while let Some(event) = tokio::time::timeout(Duration::from_secs(10), later.next())
                .await
                .unwrap()
            {
                if let AgentEvent::RunFinished { reason, .. } = event {
                    assert!(matches!(reason, octet_agent::FinishReason::Completed));
                    finished += 1;
                }
            }
            drop(later);
            assert_eq!(finished, 1, "later turn must actually complete");
            assert!(std::fs::read(&path).unwrap().starts_with(&prefix));
        }
        tokio::time::timeout(
            Duration::from_secs(10),
            app.executable_extensions.shutdown(),
        )
        .await
        .expect("never-settling Pi shutdown must not trap terminal ownership");
        assert!(app.executable_extensions.running_generations().is_empty());
        let rows: Vec<serde_json::Value> =
            std::fs::read_to_string(directory.path().join("trace.jsonl"))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
        assert!(rows
            .iter()
            .any(|row| row["stage"] == "abort-refused" && row["code"] == -32601));
        assert!(!rows.iter().any(|row| row["stage"] == "abort-accepted"));
        assert!(rows.iter().any(|row| row["stage"] == "shutdown"));
        let start = rows.iter().find(|row| row["stage"] == "started").unwrap();
        for key in ["pid", "child"] {
            let pid = start[key].as_u64().unwrap();
            let output = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .unwrap();
            let status = String::from_utf8_lossy(&output.stdout);
            assert!(
                status.trim().is_empty() || status.trim_start().starts_with('Z'),
                "owned {key} {pid} still running: {status}"
            );
        }
        Session::open_read_only(&path).expect("interrupted session remains resumable");
    }
}
