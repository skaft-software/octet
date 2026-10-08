//! Real App + reviewed Pi adapter process + native read execution/persistence.
//! Only the model transport is scripted, entirely in process (no network).
//! Native ANSI frames and Tern protocol/tree output are not PTY/desktop coverage.
#![cfg(unix)]
use super::pi_contract_support::{command, pi_workspace_ui_app};
use super::*;
use octet_ai::{
    AiError, AssistantMessage, AssistantPart, Cost, Diagnostic, HostStreamModel,
    HostStreamTransport, Request, Response, ResponseStream, StopReason, StreamEvent, ToolCall,
    ToolCallId, Usage,
};
use octet_tern::wire::{Kind, Node};
use serde_json::{json, Value};

const CALL_ID: &str = "renderer-read-call";
const READ_PATH: &str = "renderer-source.txt";
const SOURCE_ASSISTANT: &str = "SOURCE_ASSISTANT **canonical Markdown**";
const PRESENTATIONS: &[&str] = &[
    "VIEW_MESSAGE",
    "VIEW_ENTRY",
    "VIEW_CALL",
    "VIEW_RESULT",
    "VIEW_MARKDOWN",
];

struct ReadPolicy(Arc<Mutex<Vec<octet_agent::ToolPolicyDecision>>>);
impl octet_agent::EventObserver for ReadPolicy {
    fn on_event(&self, event: &AgentEvent) {
        if let AgentEvent::ToolPolicyDecision { id, name, decision } = event {
            if id.0 == CALL_ID && name == "read" {
                self.0.lock().unwrap().push(decision.clone());
            }
        }
    }
}

struct ScriptedProvider {
    requests: Arc<Mutex<Vec<Request>>>,
}
#[async_trait::async_trait]
impl HostStreamTransport for ScriptedProvider {
    async fn stream(
        &self,
        model: HostStreamModel,
        request: Request,
        _: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        let first = {
            let mut captured = self.requests.lock().unwrap();
            let first = captured.is_empty();
            captured.push(request);
            first
        };
        let mut events = vec![Ok(StreamEvent::Started { response_id: None })];
        let (content, stop_reason) = if first {
            let arguments = json!({"path":READ_PATH}).to_string();
            events.extend([
                Ok(StreamEvent::ToolCallStart {
                    async_execution: false,
                    index: 0,
                    id: ToolCallId(CALL_ID.into()),
                    name: "read".into(),
                }),
                Ok(StreamEvent::ToolCallArgsDelta {
                    index: 0,
                    delta: arguments.clone(),
                }),
                Ok(StreamEvent::ToolCallEnd {
                    index: 0,
                    argument_error: None,
                }),
            ]);
            (
                vec![AssistantPart::ToolCall(ToolCall {
                    id: ToolCallId(CALL_ID.into()),
                    name: "read".into(),
                    arguments_json: arguments,
                    async_execution: false,
                    argument_error: None,
                })],
                StopReason::ToolUse,
            )
        } else {
            events.extend([
                Ok(StreamEvent::TextStart { index: 0 }),
                Ok(StreamEvent::TextDelta {
                    index: 0,
                    delta: SOURCE_ASSISTANT.into(),
                }),
                Ok(StreamEvent::TextEnd { index: 0 }),
            ]);
            (
                vec![AssistantPart::Text(SOURCE_ASSISTANT.into())],
                StopReason::EndTurn,
            )
        };
        events.push(Ok(StreamEvent::Finished(Response {
            message: AssistantMessage {
                model: model.id,
                protocol: model.protocol,
                content,
            },
            stop_reason,
            usage: Usage::default(),
            cost: Some(Cost::default()),
            response_id: None,
            responses_output: None,
            deferred: None,
            inference: None,
            diagnostics: Vec::new(),
        })));
        Ok(Box::pin(futures_util::stream::iter(events)))
    }
}

async fn drive(app: &mut App, shell: &mut InteractiveShell) {
    let prompt = "Exercise the real renderer read tool";
    app.executable_extensions.refresh_host_state(
        app.agent.session(),
        &app.model,
        &app.reasoning,
        &app.sessions,
    );
    let composition = app
        .executable_extensions
        .compose_prompt(&app.system, prompt.into())
        .await
        .unwrap();
    app.agent.set_system_prompt(composition.system);
    let mut input: octet_agent::UserInput = composition.prompt.into();
    input.custom_messages.extend(composition.custom_messages);
    let inspection = ActiveRunInspection::capture(app);
    let mut run = app.agent.prompt(input).await.unwrap();
    let turn = app.executable_extensions.begin_turn().await;
    app.executable_extensions
        .commit_prompt_context(composition.pending_context_count);
    shell.on_prompt_submitted(prompt);
    let id = shell.begin_run("renderer-acceptance");
    shell.set_awaiting_provider(id);
    let control = run.control();
    let mut terminal = futures_util::stream::pending::<std::io::Result<Event>>();
    let mut ticker = tokio::time::interval(Duration::from_millis(16));
    let outcome = tokio::time::timeout(
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
    .expect("real renderer acceptance run timed out")
    .unwrap();
    drop(run);
    app.executable_extensions.settle_turn(turn, &outcome).await;
    assert_eq!(
        outcome,
        HostRunOutcome::Completed,
        "{}",
        shell.debug_snapshot()
    );
}

async fn rendered(shell: &mut InteractiveShell) -> String {
    shell.render();
    shell
        .dump_rendered_frame()
        .await
        .expect("native frame")
        .join("\n")
}

async fn pump_until(
    app: &mut App,
    shell: &mut InteractiveShell,
    ready: impl Fn(&str) -> bool,
) -> String {
    let mut last = String::new();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            for message in app.executable_extensions.drain_events_for_shell(shell) {
                shell.notice(message);
            }
            last = rendered(shell).await;
            if ready(&sexy_tui_rs::strip_terminal_sequences(&last))
                && shell.transcript_render_candidates().is_empty()
            {
                return last.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "real adapter renderer barrier timed out:\n{last}\n{}",
            shell.debug_snapshot()
        )
    })
}

fn trace(directory: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(directory.join("trace.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn latest<'a>(trace: &'a [Value], kind: &str) -> &'a Value {
    trace
        .iter()
        .rev()
        .find(|event| event["kind"] == kind)
        .unwrap_or_else(|| panic!("missing real Pi {kind} callback: {trace:#?}"))
}

fn node_with_text(nodes: &[Node], kind: Kind, text: &str) -> bool {
    nodes.iter().any(|node| {
        (node.k == kind
            && node
                .p
                .as_ref()
                .and_then(|props| props.as_map().get("text"))
                .and_then(Value::as_str)
                .is_some_and(|value| value.contains(text)))
            || node_with_text(node.c.as_deref().unwrap_or_default(), kind, text)
    })
}

fn assert_presentations(shell: &InteractiveShell, frame: &str, trace: &[Value], version: u64) {
    let visible = sexy_tui_rs::strip_terminal_sequences(frame);
    let nodes = crate::tui::view::tern::pi_renderer_test_support::project(shell);
    for (kind, marker) in [
        ("message", "VIEW_MESSAGE"),
        ("entry", "VIEW_ENTRY"),
        ("call", "VIEW_CALL"),
        ("result", "VIEW_RESULT"),
        ("markdown", "VIEW_MARKDOWN"),
    ] {
        let width = latest(trace, kind)["width"].as_u64().unwrap();
        assert!(width > 0 && width <= 110);
        let text = if kind == "call" || kind == "result" {
            format!("{marker} v={version} w={width}")
        } else {
            format!("{marker} w={width}")
        };
        assert!(visible.contains(&text), "missing {text}: {visible}");
        let node_kind = if kind == "markdown" {
            Kind::Md
        } else {
            Kind::Ansi
        };
        assert!(
            node_with_text(&nodes, node_kind, &text),
            "Tern missing {text}: {nodes:#?}"
        );
        if kind != "markdown" {
            assert!(
                frame
                    .lines()
                    .any(|row| row.contains(&text) && row.contains("\x1b[")),
                "approved SGR must survive native presentation: {frame}"
            );
        }
    }
    for source in [
        "SOURCE_MESSAGE",
        "SOURCE_ASSISTANT",
        "PRIVATE_ENTRY_SECRET",
        "HIDDEN_MESSAGE",
    ] {
        assert!(
            !visible.contains(source),
            "source leaked into custom presentation: {visible}"
        );
        assert!(!serde_json::to_string(&nodes).unwrap().contains(source));
    }
}

fn assert_canonical_copy(shell: &mut InteractiveShell) {
    shell.select_all_transcript();
    let copy = shell
        .selected_plain_text()
        .expect("semantic transcript selection");
    // Native semantic copy strips Markdown markup and uses tool summaries,
    // not tool output. Preserve that contract rather than copying display rows.
    for source in [
        "SOURCE_MESSAGE",
        "SOURCE_ASSISTANT",
        "canonical Markdown",
        "renderer-source.txt",
    ] {
        assert!(
            copy.contains(source),
            "canonical copy lost {source}: {copy}"
        );
    }
    for forbidden in PRESENTATIONS.iter().copied().chain([
        "PRIVATE_ENTRY_SECRET",
        "MESSAGE_DETAILS",
        "HIDDEN_MESSAGE",
        "HIDDEN_DETAILS",
    ]) {
        assert!(
            !copy.contains(forbidden),
            "noncanonical/private copy: {copy}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_renderers_real_app_presentation_persistence_invalidation_resize_and_tern() {
    let (directory, mut app, mut shell) =
        pi_workspace_ui_app(include_str!("pi_renderers_fixture.mjs"));
    std::fs::write(app.config.workspace.join(READ_PATH), "SOURCE_TOOL\n").unwrap();
    let decisions = Arc::new(Mutex::new(Vec::new()));
    app.agent.observe(ReadPolicy(decisions.clone()));
    let captured = Arc::new(Mutex::new(Vec::new()));
    app.client.register_host_stream_transport(
        app.model.endpoint.id.clone(),
        Arc::new(ScriptedProvider {
            requests: captured.clone(),
        }),
    );
    shell.set_size(110, 48);
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    tokio::time::timeout(
        Duration::from_secs(8),
        resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut input),
    )
    .await
    .expect("real frontend startup timed out")
    .unwrap();
    shell.finish_startup();
    command(&mut app, &mut shell, "seed-renderers")
        .await
        .unwrap();
    assert!(
        !wake_for_extension_messages(
            &mut shell,
            Some(&mut app.agent),
            &mut app.executable_extensions,
        )
        .await
        .unwrap(),
        "triggerTurn:false must not start inference"
    );
    assert!(captured.lock().unwrap().is_empty());
    pump_until(&mut app, &mut shell, |frame| {
        frame.contains("VIEW_MESSAGE") && frame.contains("VIEW_ENTRY")
    })
    .await;
    drive(&mut app, &mut shell).await;
    {
        let decisions = decisions.lock().unwrap();
        assert_eq!(decisions.len(), 1, "real read admission must be observed");
        let decision = &decisions[0];
        assert_eq!(
            decision.effect,
            Some(octet_agent::ToolEffect::WorkspaceRead)
        );
        assert_eq!(
            decision.policy.effect_policy.value,
            octet_agent::EffectPolicy::Controlled
        );
        assert!(decision.policy.workspace_confinement.value);
        assert!(
            decision.allowed,
            "real workspace read was denied: {decision:?}"
        );
        assert_eq!(
            decision.authorization,
            Some(octet_agent::EffectAuthorization::Policy)
        );
    }
    let frame = pump_until(&mut app, &mut shell, |frame| {
        PRESENTATIONS.iter().all(|text| frame.contains(text))
    })
    .await;
    let initial = trace(directory.path());
    assert_presentations(&shell, &frame, &initial, 0);
    assert_canonical_copy(&mut shell);

    // These are real provider requests after the native read finished, not a
    // hand-built session projection. Presentation/private metadata never enter.
    {
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let messages = serde_json::to_string(&requests[1].messages).unwrap();
        for source in ["SOURCE_MESSAGE", "HIDDEN_MESSAGE", "SOURCE_TOOL", CALL_ID] {
            assert!(
                messages.contains(source),
                "provider lost canonical {source}: {messages}"
            );
        }
        for request in requests.iter() {
            let request = serde_json::to_string(request).unwrap();
            for forbidden in PRESENTATIONS.iter().copied().chain([
                "PRIVATE_ENTRY_SECRET",
                "MESSAGE_DETAILS",
                "HIDDEN_DETAILS",
            ]) {
                assert!(
                    !request.contains(forbidden),
                    "presentation/private data reached provider: {request}"
                );
            }
        }
    }
    let session_path = app.agent.session().path().to_owned();
    let durable = std::fs::read(&session_path).unwrap();
    let reopened = Session::open_read_only(&session_path).unwrap();
    let visible_entry = reopened
        .entries()
        .iter()
        .find(|entry| {
            entry
                .metadata
                .as_ref()
                .and_then(|m| m.custom_message.as_ref())
                .is_some_and(|m| m.display && m.custom_type == "renderer-message")
        })
        .unwrap();
    let custom = visible_entry
        .metadata
        .as_ref()
        .unwrap()
        .custom_message
        .as_ref()
        .unwrap();
    assert_eq!(
        latest(&initial, "message")["message"],
        custom.lifecycle_value(visible_entry.timestamp_unix_ms.unwrap())
    );
    assert_eq!(custom.text(), "SOURCE_MESSAGE");
    assert_eq!(
        custom.details,
        Some(json!({"private":"MESSAGE_DETAILS", "count":9}))
    );
    let private_entry = reopened
        .entries()
        .iter()
        .find(|entry| {
            reopened
                .extension_entry(&entry.id, "octet-pi-compat")
                .is_some_and(|entry| entry.entry_type == "renderer-entry")
        })
        .unwrap();
    let projected = &latest(&initial, "entry")["entry"];
    assert_eq!(projected["id"], private_entry.id.0);
    assert_eq!(
        projected["parentId"],
        json!(private_entry.parent.as_ref().map(|id| &id.0))
    );
    assert_eq!(
        projected["data"],
        json!({"private":"PRIVATE_ENTRY_SECRET", "count":7})
    );
    assert_eq!(
        latest(&initial, "entry")["timestampMs"],
        private_entry.timestamp_unix_ms.unwrap()
    );
    assert_eq!(latest(&initial, "call")["toolCallId"], CALL_ID);
    assert_eq!(latest(&initial, "call")["args"], json!({"path":READ_PATH}));
    assert_eq!(latest(&initial, "result")["toolCallId"], CALL_ID);
    assert_eq!(latest(&initial, "result")["options"]["isPartial"], false);
    assert_eq!(latest(&initial, "result")["result"]["isError"], false);
    assert!(latest(&initial, "result")["result"]["content"]
        .to_string()
        .contains("SOURCE_TOOL"));
    assert_eq!(latest(&initial, "markdown")["text"], SOURCE_ASSISTANT);
    assert_eq!(
        latest(&initial, "markdown")["context"]["messageType"],
        "assistant"
    );
    assert_eq!(
        latest(&initial, "markdown")["context"]["isStreaming"],
        false
    );
    assert!(String::from_utf8_lossy(&durable).contains(SOURCE_ASSISTANT));
    assert!(String::from_utf8_lossy(&durable).contains("SOURCE_TOOL"));
    for marker in PRESENTATIONS {
        assert!(!String::from_utf8_lossy(&durable).contains(marker));
    }
    assert!(initial
        .iter()
        .filter(|event| event["kind"] == "message")
        .all(|event| event["message"]["display"] == true));

    // Repainting either native frontend consumes snapshots, never renderer RPC.
    let count = trace(directory.path()).len();
    for _ in 0..3 {
        rendered(&mut shell).await;
    }
    crate::tui::view::tern::pi_renderer_test_support::project(&shell);
    assert_eq!(trace(directory.path()).len(), count);

    command(&mut app, &mut shell, "invalidate-renderer")
        .await
        .unwrap();
    let invalidated = pump_until(&mut app, &mut shell, |frame| {
        frame.contains("VIEW_CALL v=1") && frame.contains("VIEW_RESULT v=1")
    })
    .await;
    let after_invalidation = trace(directory.path());
    assert_presentations(&shell, &invalidated, &after_invalidation, 1);
    assert_eq!(
        latest(&after_invalidation, "call")["width"],
        latest(&initial, "call")["width"]
    );
    assert!(
        latest(&after_invalidation, "call")["calls"]
            .as_u64()
            .unwrap()
            > latest(&initial, "call")["calls"].as_u64().unwrap()
    );
    assert_eq!(latest(&after_invalidation, "call")["lastComponent"], true);
    assert_eq!(std::fs::read(&session_path).unwrap(), durable);

    // Paint before pumping the adapter: old-width frames must already be gone.
    shell.set_size(74, 48);
    let stale = rendered(&mut shell).await;
    let stale_tern = serde_json::to_string(
        &crate::tui::view::tern::pi_renderer_test_support::project(&shell),
    )
    .unwrap();
    for marker in PRESENTATIONS {
        assert!(
            !stale.contains(marker),
            "stale geometry in native frame: {stale}"
        );
        assert!(
            !stale_tern.contains(marker),
            "stale geometry in Tern: {stale_tern}"
        );
    }
    let resized = pump_until(&mut app, &mut shell, |frame| {
        PRESENTATIONS.iter().all(|text| frame.contains(text))
    })
    .await;
    let after_resize = trace(directory.path());
    assert_presentations(&shell, &resized, &after_resize, 1);
    for kind in ["message", "entry", "call", "result", "markdown"] {
        let width = latest(&after_resize, kind)["width"].as_u64().unwrap();
        assert!(width > 0 && width <= 74);
        assert_ne!(
            latest(&initial, kind)["width"],
            width,
            "{kind} did not receive new geometry"
        );
    }
    assert_eq!(std::fs::read(&session_path).unwrap(), durable);

    // Rehydration consumes the real persisted records, retaining private entry
    // identity and canonical copy while rebuilding only presentation snapshots.
    shell.hydrate(&reopened).unwrap();
    let resumed = pump_until(&mut app, &mut shell, |frame| {
        PRESENTATIONS.iter().all(|text| frame.contains(text))
    })
    .await;
    let after_resume = trace(directory.path());
    assert_presentations(&shell, &resumed, &after_resume, 1);
    assert_eq!(latest(&after_resume, "entry")["entry"], *projected);
    assert_canonical_copy(&mut shell);
    assert_eq!(std::fs::read(&session_path).unwrap(), durable);

    // Public late registration withdraws presentation. Native fallback is still
    // canonical, and a private entry never becomes visible by default.
    command(&mut app, &mut shell, "fallback-renderers")
        .await
        .unwrap();
    let fallback = pump_until(&mut app, &mut shell, |frame| {
        frame.contains("SOURCE_MESSAGE")
            && frame.contains("SOURCE_ASSISTANT")
            && PRESENTATIONS.iter().all(|text| !frame.contains(text))
    })
    .await;
    assert!(!fallback.contains("PRIVATE_ENTRY_SECRET"));
    assert!(!fallback.contains("HIDDEN_MESSAGE"));
    let nodes = crate::tui::view::tern::pi_renderer_test_support::project(&shell);
    assert!(node_with_text(&nodes, Kind::Md, "SOURCE_ASSISTANT"));
    let native = serde_json::to_string(&nodes).unwrap();
    for forbidden in PRESENTATIONS
        .iter()
        .copied()
        .chain(["PRIVATE_ENTRY_SECRET", "HIDDEN_MESSAGE"])
    {
        assert!(!native.contains(forbidden));
    }
    assert_canonical_copy(&mut shell);
    assert_eq!(std::fs::read(&session_path).unwrap(), durable);
    assert_eq!(
        captured.lock().unwrap().len(),
        2,
        "rendering must never trigger inference"
    );
    app.executable_extensions.shutdown().await;
}
