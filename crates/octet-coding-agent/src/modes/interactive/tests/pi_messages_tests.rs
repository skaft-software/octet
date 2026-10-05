//! Pi custom messages through the real App, adapter process, session and run pump.
//! Only inference is replaced by a local scripted HTTP provider.
//! Lifecycle callbacks are recorded by the real adapter, never injected by the fixture.
#![cfg(unix)]
use super::pi_contract_support::{command, pi_app};
use super::support::{scripted_model, text_turn};
use super::*;
use octet_agent::{session::CustomMessage, UserInput};
use serde_json::{json, Value};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

async fn app_with_provider(
    factory: &str,
    tool: bool,
) -> (tempfile::TempDir, App, MockServer, Arc<Mutex<Vec<Value>>>) {
    let server = MockServer::start().await;
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let capture = requests.clone();
    let first = tool_turn();
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |request: &wiremock::Request| {
            let mut requests = capture.lock().unwrap();
            let body = if tool && requests.is_empty() {
                first.clone()
            } else {
                text_turn()
            };
            requests.push(serde_json::from_slice(&request.body).unwrap());
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body)
        })
        .mount(&server)
        .await;
    let factory = format!(r#"
import {{ appendFileSync }} from 'node:fs';
{}
export default pi => {{
  fixtureFactory(pi);
  for (const type of ['message_start', 'message_end']) pi.on(type, event => {{
    if ('message' in event && event.message.role === 'custom')
      appendFileSync(TRACE, JSON.stringify({{ type: event.type, id: event.message_id, message: event.message }}) + '\n');
  }});
}};
"#, factory.replacen("export default ", "const fixtureFactory = ", 1));
    let (directory, mut app) = pi_app(&factory);
    let model = scripted_model(&server.uri());
    app.catalog
        .register_endpoint((*model.endpoint).clone())
        .unwrap();
    app.catalog.register_model((*model.spec).clone()).unwrap();
    std::fs::write(directory.path().join("fixture.txt"), "REAL_TOOL_RESULT").unwrap();
    app = rebuild_app(app, Some(model), None, None, None).unwrap();
    assert!(
        app.executable_extensions.summaries().iter()
            .any(|summary| summary.name == "octet-pi-compat" && summary.running),
        "{}", app.executable_extensions.inspect_text()
    );
    app.executable_extensions.activate_session_lifecycle_driver();
    let mut startup_shell = InteractiveShell::test_shell();
    let mut startup_input = futures_util::stream::pending::<std::io::Result<Event>>();
    resource_paths::refresh_resource_paths(&mut app, &mut startup_shell, &mut startup_input)
        .await.unwrap();
    (directory, app, server, requests)
}

fn tool_turn() -> String {
    [
        json!({"type":"message_start","message":{"id":"custom-tool","usage":{"input_tokens":1,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"message-read","name":"read"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"fixture.txt\"}"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}),
        json!({"type":"message_stop"}),
    ].into_iter().map(|event| format!("event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap())).collect()
}

async fn drive(app: &mut App, shell: &mut InteractiveShell, mut input: UserInput) {
    app.executable_extensions.refresh_host_state(
        app.agent.session(),
        &app.model,
        &app.reasoning,
        &app.sessions,
    );
    let composition = app
        .executable_extensions
        .compose_prompt(&app.system, input.text_summary())
        .await
        .unwrap();
    if !input.parts.is_empty() && composition.prompt != input.text_summary() {
        let mut composed = ComposedInput::from_text(String::new());
        composed.parts = std::mem::take(&mut input.parts);
        composed.replace_model_text(composition.prompt);
        input.parts = composed.parts;
    }
    input.custom_messages.extend(composition.custom_messages);
    app.agent.set_system_prompt(composition.system);
    let inspection = ActiveRunInspection::capture(app);
    let mut run = app.agent.prompt(input).await.unwrap();
    let turn = app.executable_extensions.begin_turn().await;
    app.executable_extensions
        .commit_prompt_context(composition.pending_context_count);
    let id = shell.begin_run("test");
    shell.set_awaiting_provider(id);
    let control = run.control();
    let mut terminal = futures_util::stream::pending::<std::io::Result<Event>>();
    let mut ticker = tokio::time::interval(Duration::from_millis(16));
    let ended = tokio::time::timeout(
        Duration::from_secs(20),
        drive_active_run(
            &mut run,
            &control,
            shell,
            &mut terminal,
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
    .expect("real custom-message run timed out")
    .unwrap();
    drop(run);
    app.executable_extensions.settle_turn(turn, &ended).await;
    assert_custom_lifecycle(app).await;
    assert_eq!(
        ended,
        HostRunOutcome::Completed,
        "{}",
        shell.debug_snapshot()
    );
}

async fn assert_custom_lifecycle(app: &App) {
    let expected: Vec<_> = app.agent.session().entries().iter().filter_map(|entry| {
        let custom = entry.metadata.as_ref()?.custom_message.as_ref()?;
        Some((entry.id.0.clone(), custom.lifecycle_value(entry.timestamp_unix_ms.unwrap())))
    }).collect();
    let path = app.config.workspace.join("trace.jsonl");
    let read = || -> Vec<Value> {
        std::fs::read_to_string(&path).unwrap_or_default().lines()
            .map(|line| serde_json::from_str(line).unwrap()).collect()
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        while read().len() < expected.len() * 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("custom lifecycle notifications did not arrive");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let events = read();
    assert_eq!(events.len(), expected.len() * 2, "exactly one pair per durable commit: {events:#?}");
    for (pair, (id, message)) in events.chunks_exact(2).zip(expected) {
        assert_eq!(pair[0]["type"], "message_start");
        assert_eq!(pair[1]["type"], "message_end");
        for event in pair {
            assert_eq!(event["id"], id, "not an invented assistant sequence ID");
            assert_eq!(event["message"], message, "actual persisted content/details/timestamp");
        }
    }
}

// One white grayscale/alpha pixel; the IDAT CRC must pass the native decoder.
const MESSAGE_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=";

#[test]
fn native_pi_messages_image_fixture_decodes_strictly() {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD.decode(MESSAGE_PNG).unwrap();
    let image = octet_ai::ImageMedia {
        source: octet_ai::ImageSource::Inline(bytes.clone().into()),
        media_type: Some("image/png".parse().unwrap()), detail: None,
    };
    let limits = octet_ai::ImageInputLimits { max_width: 1, max_height: 1, max_bytes: 1024 };
    let prepared = octet_ai::prepare_user_image(&image, limits).expect("valid fixture pixels and CRC");
    assert_eq!(serde_json::to_value(&prepared).unwrap(), serde_json::to_value(&image).unwrap(),
        "native no-resize preparation preserves exact bytes");
    let mut corrupt = bytes;
    corrupt[52] ^= 1; // IDAT CRC: a valid header/base64 alone must not pass admission.
    let invalid = octet_ai::ImageMedia {
        source: octet_ai::ImageSource::Inline(corrupt.into()), ..image
    };
    assert_eq!(octet_ai::prepare_user_image(&invalid, limits).unwrap_err(), octet_ai::ImageInputError::InvalidImage);
}

fn image_content() -> Value {
    json!([{ "type": "text", "text": "CUSTOM_IMAGE_TEXT" },
        { "type": "image", "data": MESSAGE_PNG, "mimeType": "image/png" }])
}

fn assert_provider_image(request: &Value) {
    let images: Vec<_> = request["messages"].as_array().unwrap().iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter(|part| part["type"] == "image").collect();
    assert_eq!(images.len(), 1, "native media must reach the provider: {request:#?}");
    assert_eq!(images[0]["source"]["type"], "base64");
    assert_eq!(images[0]["source"]["media_type"], "image/png");
    assert_eq!(images[0]["source"]["data"], MESSAGE_PNG);
    assert!(!request.to_string().contains("IMAGE_SECRET"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_messages_images_persist_resume_and_emit_committed_events() {
    for mode in ["wake", "idle", "next", "before"] {
        let message = json!({"customType":"image", "content":image_content(), "display":false,
            "details":{"private":"IMAGE_SECRET"}});
        let factory = if mode == "before" {
            format!("export default pi => pi.on('before_agent_start', () => ({{ message: {message} }}));")
        } else {
            let options = match mode {
                "wake" => "{ triggerTurn: true }", "next" => "{ deliverAs: 'nextTurn', triggerTurn: true }",
                _ => "{ triggerTurn: false }",
            };
            format!("export default pi => pi.registerCommand('probe', {{ handler: () => pi.sendMessage({message}, {options}) }});")
        };
        let (_directory, mut app, _server, requests) = app_with_provider(&factory, false).await;
        let mut shell = InteractiveShell::test_shell();
        let input = if mode == "before" {
            UserInput::from("ORIGINAL_IMAGE_PROMPT")
        } else {
            command(&mut app, &mut shell, "probe").await.unwrap();
            let wake = wake_for_extension_messages(&mut shell, Some(&mut app.agent), &mut app.executable_extensions).await.unwrap();
            assert_eq!(wake, mode == "wake");
            assert!(requests.lock().unwrap().is_empty());
            assert_custom_lifecycle(&app).await;
            if wake {
                shell.take_ready_follow_up().unwrap().into_user_input()
            } else {
                let mut input = ComposedInput::from_text("NEXT_IMAGE_PROMPT".into());
                shell.attach_next_turn(&mut input);
                input.into_user_input()
            }
        };
        drive(&mut app, &mut shell, input).await;
        {
            let captured = requests.lock().unwrap();
            assert_eq!(captured.len(), 1, "{mode}");
            assert_provider_image(&captured[0]);
        }
        let reopened = Session::open_read_only(app.agent.session().path()).unwrap();
        let messages = custom_entries(&reopened);
        assert_eq!(messages.len(), 1);
        assert_eq!(serde_json::to_value(&messages[0].content).unwrap(), image_content());
        assert_eq!(messages[0].details.as_ref().unwrap()["private"], "IMAGE_SECRET");
        assert!(reopened.entries().iter().any(|entry| entry.metadata.as_ref().is_some_and(|m| m.custom_message.is_some())
            && matches!(&entry.value, EntryValue::Message(octet_ai::Message::User(user))
                if user.content.iter().any(|part| matches!(part, octet_ai::UserPart::Media(octet_ai::Media::Image(_)))))));
        shell.hydrate(&reopened).unwrap();
        assert!(!shell.debug_snapshot().contains("CUSTOM_IMAGE_TEXT"));
        assert!(!shell.debug_snapshot().contains(MESSAGE_PNG));
        drop(reopened);
        if mode == "idle" {
            // Rebuild releases the writer and reopens the actual session file.
            // A subsequent provider request must replay the canonical image;
            // historical entries must not announce a second lifecycle pair.
            app = rebuild_app(app, None, None, None, None).unwrap();
            app.executable_extensions.activate_session_lifecycle_driver();
            let mut startup_input = futures_util::stream::pending::<std::io::Result<Event>>();
            resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut startup_input).await.unwrap();
            drive(&mut app, &mut shell, UserInput::from("RESUMED_IMAGE_PROMPT")).await;
            let captured = requests.lock().unwrap();
            assert_eq!(captured.len(), 2);
            assert_provider_image(&captured[1]);
            assert_eq!(custom_entries(app.agent.session()).len(), 1, "replay is not a new custom commit");
        }
        app.executable_extensions.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_messages_images_in_run_preserve_real_tool_pairs() {
    for (options, count, at) in [("{ deliverAs: 'steer' }", 2, 1),
        ("{ deliverAs: 'followUp' }", 3, 2), ("{ triggerTurn: false }", 2, 1)] {
        let message = json!({"customType":"image", "content":image_content(), "display":false,
            "details":{"private":"IMAGE_SECRET"}});
        let factory = format!("export default pi => pi.on('tool_call', async () => {{ await pi.sendMessage({message}, {options}); }});");
        let (_directory, mut app, _server, requests) = app_with_provider(&factory, true).await;
        let mut shell = InteractiveShell::test_shell();
        drive(&mut app, &mut shell, UserInput::from("IMAGE_WITH_REAL_READ")).await;
        app.executable_extensions.shutdown().await;
        let captured = requests.lock().unwrap();
        assert_eq!(captured.len(), count, "{options}");
        assert!(!captured[0].to_string().contains(MESSAGE_PNG));
        assert_provider_image(&captured[at]);
        if at == 2 { assert!(!captured[1].to_string().contains(MESSAGE_PNG)); }
        let reopened = Session::open_read_only(app.agent.session().path()).unwrap();
        let custom = reopened.entries().iter().position(|entry| entry.metadata.as_ref().is_some_and(|m| m.custom_message.is_some())).unwrap();
        let result = reopened.entries().iter().position(|entry| matches!(&entry.value,
            EntryValue::Message(octet_ai::Message::User(user)) if user.content.iter().any(|part| matches!(part, octet_ai::UserPart::ToolResult(_))))).unwrap();
        assert!(custom > result, "image custom entry must follow the complete real tool pair");
        assert_eq!(reopened.checkpoints().len(), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_messages_send_user_image_is_native_media_not_custom_or_text() {
    let factory = format!("export default pi => pi.registerCommand('probe', {{ handler: () => pi.sendUserMessage({}, {{ deliverAs: 'steer' }}) }});", image_content());
    let (_directory, mut app, _server, requests) = app_with_provider(&factory, false).await;
    let mut shell = InteractiveShell::test_shell();
    command(&mut app, &mut shell, "probe").await.unwrap();
    assert!(wake_for_extension_messages(&mut shell, Some(&mut app.agent), &mut app.executable_extensions).await.unwrap());
    let input = shell.take_ready_follow_up().unwrap().into_user_input();
    drive(&mut app, &mut shell, input).await;
    assert_provider_image(&requests.lock().unwrap()[0]);
    assert!(custom_entries(app.agent.session()).is_empty());
    let reopened = Session::open_read_only(app.agent.session().path()).unwrap();
    assert!(reopened.entries().iter().any(|entry| matches!(&entry.value,
        EntryValue::Message(octet_ai::Message::User(user)) if user.content.iter().any(|part| matches!(part, octet_ai::UserPart::Media(octet_ai::Media::Image(_)))))));
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_messages_invalid_image_never_commits_or_emits() {
    let factory = r#"export default pi => pi.registerCommand('probe', { handler: () => pi.sendMessage({
        customType: 'bad-image', content: [{ type: 'image', data: 'bm90LWFuLWltYWdl', mimeType: 'image/png' }], display: false
    }, { triggerTurn: false }) });"#;
    let (_directory, mut app, _server, requests) = app_with_provider(factory, false).await;
    let mut shell = InteractiveShell::test_shell();
    command(&mut app, &mut shell, "probe").await.unwrap();
    assert!(wake_for_extension_messages(&mut shell, Some(&mut app.agent), &mut app.executable_extensions).await.is_err());
    assert!(custom_entries(app.agent.session()).is_empty());
    assert!(requests.lock().unwrap().is_empty());
    assert_custom_lifecycle(&app).await;
    app.executable_extensions.shutdown().await;
}

fn custom_entries(session: &Session) -> Vec<&CustomMessage> {
    session
        .entries()
        .iter()
        .filter_map(|entry| entry.metadata.as_ref()?.custom_message.as_ref())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_messages_idle_wake_persists_and_hides_on_resume() {
    let (_directory, mut app, _server, requests) = app_with_provider(
        r#"
export default pi => pi.registerCommand('probe', { handler: () => pi.sendMessage({
  customType: 'wake', content: 'CUSTOM_WAKE_CONTENT', display: false,
  details: { private: 'NEVER_IN_PROVIDER' }
}, { triggerTurn: true }) });
"#,
        false,
    )
    .await;
    let mut shell = InteractiveShell::test_shell();
    command(&mut app, &mut shell, "probe").await.unwrap();
    assert!(wake_for_extension_messages(&mut shell, Some(&mut app.agent), &mut app.executable_extensions).await.unwrap());
    let input = shell
        .take_ready_follow_up()
        .expect("custom message must wake without editor input");
    drive(&mut app, &mut shell, input.into_user_input()).await;
    app.executable_extensions.shutdown().await;
    let captured = requests.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert!(captured[0].to_string().contains("CUSTOM_WAKE_CONTENT"));
    assert!(!captured[0].to_string().contains("NEVER_IN_PROVIDER"));
    let reopened = Session::open_read_only(app.agent.session().path()).unwrap();
    let messages = custom_entries(&reopened);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].custom_type, "wake");
    assert_eq!(
        messages[0].details.as_ref().unwrap()["private"],
        "NEVER_IN_PROVIDER"
    );
    assert!(!messages[0].display);
    shell.hydrate(&reopened).unwrap();
    assert!(!shell.debug_snapshot().contains("CUSTOM_WAKE_CONTENT"));
    assert_eq!(reopened.checkpoints().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_messages_idle_no_trigger_and_next_turn_order() {
    let (_directory, mut app, _server, requests) = app_with_provider(r#"
export default pi => {
  pi.registerCommand('probe', { handler: () => {
    pi.sendMessage({ customType: 'idle', content: 'IDLE_CONTEXT_ONLY', display: false });
    pi.sendMessage({ customType: 'next', content: [{ type: 'text', text: 'NEXT_TURN_CONTEXT' }], display: true }, { deliverAs: 'nextTurn', triggerTurn: true });
  } });
};
"#, false).await;
    let mut shell = InteractiveShell::test_shell();
    command(&mut app, &mut shell, "probe").await.unwrap();
    assert!(!wake_for_extension_messages(&mut shell, Some(&mut app.agent), &mut app.executable_extensions).await.unwrap());
    assert!(
        requests.lock().unwrap().is_empty(),
        "context-only idle message must not call the provider"
    );
    assert_eq!(custom_entries(app.agent.session()).len(), 1);
    assert_custom_lifecycle(&app).await;
    let mut next = ComposedInput::from_text("NEXT_USER_PROMPT".into());
    shell.attach_next_turn(&mut next);
    drive(&mut app, &mut shell, next.into_user_input()).await;
    app.executable_extensions.shutdown().await;
    let captured = requests.lock().unwrap();
    assert_eq!(captured.len(), 1);
    let messages = captured[0]["messages"].to_string();
    assert!(
        messages.find("NEXT_USER_PROMPT").unwrap() < messages.find("NEXT_TURN_CONTEXT").unwrap()
    );
    let reopened = Session::open_read_only(app.agent.session().path()).unwrap();
    let entries = custom_entries(&reopened);
    assert_eq!(entries.len(), 2);
    shell.hydrate(&reopened).unwrap();
    assert!(!shell.debug_snapshot().contains("IDLE_CONTEXT_ONLY"));
    assert!(shell.debug_snapshot().contains("[next]\nNEXT_TURN_CONTEXT"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_messages_before_agent_start_appends_independent_custom_entries() {
    let (_directory, mut app, _server, requests) = app_with_provider(r#"
export default pi => {
  pi.on('before_agent_start', () => ({ message: { customType: 'first', content: 'BEFORE_FIRST', display: false, details: { private: 'BEFORE_SECRET' } } }));
  pi.on('before_agent_start', () => ({ message: { customType: 'second', content: 'BEFORE_SECOND', display: true } }));
};
"#, false).await;
    let mut shell = InteractiveShell::test_shell();
    drive(&mut app, &mut shell, UserInput::from("ORIGINAL_PROMPT")).await;
    app.executable_extensions.shutdown().await;
    let captured = requests.lock().unwrap();
    assert_eq!(captured.len(), 1);
    let body = captured[0].to_string();
    assert!(body.find("ORIGINAL_PROMPT").unwrap() < body.find("BEFORE_FIRST").unwrap());
    assert!(body.find("BEFORE_FIRST").unwrap() < body.find("BEFORE_SECOND").unwrap());
    assert!(!body.contains("BEFORE_SECRET"));
    let reopened = Session::open_read_only(app.agent.session().path()).unwrap();
    assert_eq!(custom_entries(&reopened).len(), 2);
    shell.hydrate(&reopened).unwrap();
    assert!(!shell.debug_snapshot().contains("BEFORE_FIRST"));
    assert!(shell.debug_snapshot().contains("BEFORE_SECOND"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_messages_in_run_delivery_preserves_tool_pairing_and_no_trigger() {
    for (options, expected_requests, expected_at) in [
        ("{ deliverAs: 'steer' }", 2, 1),
        ("{ deliverAs: 'followUp' }", 3, 2),
        ("{ triggerTurn: false }", 2, 1),
    ] {
        let factory = format!(
            r#"
export default pi => {{
  pi.on('tool_call', async () => {{
    await pi.sendMessage({{ customType: 'inrun', content: 'INRUN_CUSTOM_CONTENT', display: false, details: {{ private: 'INRUN_SECRET' }} }}, {options});
  }});
}};
"#
        );
        let (_directory, mut app, _server, requests) = app_with_provider(&factory, true).await;
        let mut shell = InteractiveShell::test_shell();
        drive(&mut app, &mut shell, UserInput::from("RUN_WITH_REAL_READ")).await;
        app.executable_extensions.shutdown().await;
        let captured = requests.lock().unwrap();
        assert_eq!(
            captured.len(),
            expected_requests,
            "{options}: {captured:#?}"
        );
        assert!(!captured[0].to_string().contains("INRUN_CUSTOM_CONTENT"));
        assert!(
            captured[expected_at]
                .to_string()
                .contains("INRUN_CUSTOM_CONTENT"),
            "{options}"
        );
        assert!(!captured
            .iter()
            .any(|request| request.to_string().contains("INRUN_SECRET")));
        if expected_at == 2 {
            assert!(!captured[1].to_string().contains("INRUN_CUSTOM_CONTENT"));
        }
        let reopened = Session::open_read_only(app.agent.session().path()).unwrap();
        assert_eq!(custom_entries(&reopened).len(), 1);
        assert_eq!(
            reopened.checkpoints().len(),
            1,
            "delivery continues the same run"
        );
        let custom_at = reopened
            .entries()
            .iter()
            .position(|entry| {
                entry
                    .metadata
                    .as_ref()
                    .is_some_and(|m| m.custom_message.is_some())
            })
            .unwrap();
        let result_at = reopened.entries().iter().position(|entry| matches!(&entry.value, EntryValue::Message(octet_ai::Message::User(user)) if user.content.iter().any(|part| matches!(part, octet_ai::UserPart::ToolResult(_))))).unwrap();
        assert!(
            custom_at > result_at,
            "custom append must not split a call/result pair: {options}"
        );
        shell.hydrate(&reopened).unwrap();
        assert!(!shell.debug_snapshot().contains("INRUN_CUSTOM_CONTENT"));
    }
}
