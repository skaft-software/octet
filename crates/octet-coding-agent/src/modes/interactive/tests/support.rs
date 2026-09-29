//! Fixtures shared by more than one interactive test area: the theme and theme-picker
//! builders, the idle-shell driver, the keystroke constructors, the scripted model and
//! agent factories, the run-inspection snapshots, and the held-request transports.
//! One module rather than one copy per area so the shapes they hand out cannot drift
//! apart and a change to a builder is one edit in one file.

use super::*;

pub(super) fn test_theme() -> crate::tui::theme::OctetTheme {
    crate::tui::theme::test_theme()
}

pub(in crate::modes::interactive) fn terminal_theme_test_config(workspace: PathBuf) -> Config {
    use crate::config::{CompactionPolicy, Mode, ResumeSelector, SandboxPolicy};

    Config {
        workspace: workspace.clone(),
        invocation_cwd: workspace,
        model: None,
        model_explicit: false,
        reasoning: None,
        reasoning_explicit: false,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        reasoning_mode_explicit: false,
        cache_retention: octet_ai::CacheRetention::Short,
        effect_policy: octet_agent::EffectPolicy::Controlled,
        sandbox: SandboxPolicy::default(),
        theme: None,
        system_prompt: None,
        theme_paths: vec![],
        color: crate::config::ColorMode::Auto,
        mouse: crate::config::MouseMode::Auto,
        plain: false,
        show_images: false,
        session_dir: PathBuf::from("sessions"),
        compaction: CompactionPolicy::default(),
        max_cost_microdollars: None,
        cost_warning_microdollars: None,
        max_turns: Some(40),
        show_reasoning_in_print: false,
        initial_prompt: None,
        prompt_template: None,
        debug_prompt: false,
        prompt_paths: vec![],
        mode: Mode::Interactive,
        resume: ResumeSelector::New,
        skill_paths: vec![],
        extension_paths: vec![],
        enabled_extensions: vec![],
        extension_activation_overridden: false,
        trusted_extensions: vec![],
        invocation_trusted_extensions: vec![],
        experimental_streamable_http_mcp: false,
        extension_flag_values: Default::default(),
        tools: crate::config::ToolPolicy::default(),
        telemetry: None,
        context_files: true,
        offline: true,
        workspace_trusted: true,
    }
}

pub(super) fn theme_picker_key(code: KeyCode) -> std::io::Result<Event> {
    Ok(Event::Key(crossterm::event::KeyEvent::new(
        code,
        KeyModifiers::NONE,
    )))
}
/// Drive the idle input owner with a fixed event list and return the shell.
pub(super) async fn idle_shell_after(events: Vec<Event>) -> (InteractiveShell, Idle) {
    use tokio_stream::wrappers::ReceiverStream;

    let mut shell = InteractiveShell::test_shell();
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    for event in events {
        sender.send(Ok(event)).await.unwrap();
    }
    let _sender = sender;
    let mut input = ReceiverStream::new(receiver);
    let mut scroll_tick = tokio::time::interval(Duration::from_millis(16));
    let mut extension_tick = tokio::time::interval(Duration::from_millis(50));
    let mut extensions = crate::extensions::ExecutableExtensions::default();
    let (reload_watcher, mut reload, mut reload_tick) = test_reload();
    let idle = wait_for_prompt(
        &mut shell,
        &mut input,
        &mut scroll_tick,
        &mut extension_tick,
        &mut extensions,
        None,
        &mut reload_tick,
        &reload_watcher,
        &mut reload,
    )
    .await
    .unwrap();
    (shell, idle)
}

/// A disabled live-reload supervisor for idle-loop tests: it samples
/// nothing and can never report a due pass, so the existing idle-loop
/// assertions keep their meaning unchanged.
pub(super) fn test_reload() -> (
    crate::reload::ReloadWatcher,
    crate::reload::ReloadSupervisor,
    tokio::time::Interval,
) {
    let reload = crate::reload::ReloadSupervisor::new(crate::reload::ReloadSettings::disabled());
    let watcher = crate::reload::ReloadWatcher::new(crate::reload::ReloadWatchSet::new());
    let mut tick = tokio::time::interval(reload.settings().tick_interval());
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    (watcher, reload, tick)
}

pub(super) fn ctrl_key(character: char) -> Event {
    Event::Key(crossterm::event::KeyEvent::new(
        KeyCode::Char(character),
        KeyModifiers::CONTROL,
    ))
}
pub(super) fn transcript_search_open_key() -> Event {
    let bindings = keymap::keybindings::KeybindingsManager::current_platform();
    let mut key = crossterm::event::KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
    // Windows/WSL use Ctrl-F; other platforms use Ctrl-Shift-F.
    if !bindings.matches(&key, "tui.altScreen.search") {
        key.modifiers |= KeyModifiers::SHIFT;
    }
    assert!(bindings.matches(&key, "tui.altScreen.search"));
    Event::Key(key)
}

pub(super) fn fast_test_app(model: Model) -> (tempfile::TempDir, App) {
    let (directory, mut app) = crate::compaction::tests::app_for_estimate();
    app.catalog
        .register_endpoint((*model.endpoint).clone())
        .unwrap();
    app.catalog.register_model((*model.spec).clone()).unwrap();
    let app = rebuild_app(app, Some(model), None, None, None).unwrap();
    (directory, app)
}

pub(super) fn fast_response() -> String {
    let output = serde_json::json!([{
        "id": "fast-message", "type": "message", "role": "assistant",
        "content": [{"type": "output_text", "text": "done", "annotations": []}]
    }]);
    [
        serde_json::json!({"type":"response.created", "response":{"id":"fast-response"}}),
        serde_json::json!({"type":"response.output_item.added", "output_index":0,
            "item":{"id":"fast-message", "type":"message"}}),
        serde_json::json!({"type":"response.output_text.delta", "output_index":0, "delta":"done"}),
        serde_json::json!({"type":"response.output_text.done", "output_index":0}),
        serde_json::json!({"type":"response.completed", "response":{"output":output,
            "usage":{"input_tokens":5,"output_tokens":2,"total_tokens":7}}}),
    ]
    .into_iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect()
}

pub(super) async fn fast_idle(mut app: App, shell: &mut InteractiveShell, command: &str) -> App {
    let command = commands::parse(command);
    let Command::Fast(requested) = command else {
        panic!("expected fast control")
    };
    apply_fast_command(&mut app, shell, requested);
    schedule_idle_responses_prewarm(&app, &command);
    app
}
pub(super) fn text_turn() -> String {
    concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n"
    )
    .to_owned()
}

pub(super) fn scripted_model(uri: &str) -> octet_ai::Model {
    use octet_ai::{
        Auth, Capabilities, Endpoint, EndpointId, Modality, ModalitySet, ModelLimits, ModelSpec,
        Protocol,
    };
    use std::sync::Arc;
    use std::time::Duration;

    octet_ai::Model {
        spec: Arc::new(ModelSpec {
            preset: Default::default(),
            id: ModelId("scripted".into()),
            endpoint: EndpointId("test".into()),
            api_name: "scripted".into(),
            display_name: None,
            protocol: Protocol::AnthropicMessages,
            capabilities: Capabilities {
                responses_features: Default::default(),
                input_modalities: ModalitySet::none().with(Modality::Image),
                output_modalities: ModalitySet::none(),
                tools: true,
                parallel_tool_calls: false,
                reasoning: None,
                responses_lite: false,
                agent_delegation: None,
                structured_output: false,
                deferred_tool_loading: false,
            },
            limits: ModelLimits {
                context_window: 16_000,
                max_output_tokens: 1024,
            },
            pricing: None,
            cache: octet_ai::CacheCompatibility::default(),
        }),
        endpoint: Arc::new(Endpoint {
            id: EndpointId("test".into()),
            base_url: url::Url::parse(&format!("{uri}/v1/")).unwrap(),
            auth: Auth::None,
            default_headers: http::HeaderMap::new(),
            transport: octet_ai::EndpointTransport::Http,
            runtime: octet_ai::RequestRuntime::default(),
            timeout: Duration::from_secs(5),
        }),
    }
}

/// The same scripted fixture on the Codex Responses route, the only profile
/// that declares the `service_tier` capability.
pub(super) fn scripted_codex_model(uri: &str) -> octet_ai::Model {
    let mut model = scripted_model(uri);
    std::sync::Arc::make_mut(&mut model.spec).protocol = octet_ai::Protocol::OpenAiResponses;
    std::sync::Arc::make_mut(&mut model.endpoint)
        .runtime
        .responses_profile = octet_ai::ResponsesRuntimeProfile::Codex;
    model
}

/// The effort menu must offer the Codex context-window surface on a Codex
/// route and nowhere else, and it must report the live effective window.
pub(super) fn octet_ai_operation_name() -> &'static str {
    crate::codex_context::CODEX_ABOVE_STANDARD_TIER_OPERATION
}

/// Inspection facts for tests that drive `drive_active_run` directly. The
/// paths do not exist: only the mechanics tests use this, and none of them
/// issue an inspection command.
///
/// The durable goal is deliberately unaddressable here, so this fixture is
/// also the fail-closed case: a mechanics test that issued `/goal` would
/// have to observe the typed error rather than a silent no-op.
pub(super) fn test_run_inspection() -> &'static ActiveRunInspection {
    static INSPECTION: std::sync::OnceLock<ActiveRunInspection> = std::sync::OnceLock::new();
    INSPECTION.get_or_init(|| {
        let missing = PathBuf::from("/nonexistent/octet-run-inspection");
        ActiveRunInspection {
            workspace: missing.clone(),
            invocation_cwd: missing.clone(),
            session_path: missing.join("session.jsonl"),
            resource_owner: "fixture-owner".into(),
            model: scripted_model("http://127.0.0.1:1"),
            catalog: octet_ai::ModelCatalog::default(),
            reasoning: octet_ai::ReasoningConfig::Off,
            sessions: crate::session_store::SessionStore::new(&missing, &missing),
            sandbox: SandboxPolicy::default(),
            effect_policy: octet_agent::EffectPolicy::UnsafeHost,
            subagents_available: false,
            service_tier: None,
            goal: Err(ActiveGoalError::UnaddressableSession),
            settings: commands::SettingsSurface {
                default_model: None,
                reasoning: "off".into(),
                theme: None,
                transport: "http",
                endpoint: "test-endpoint".into(),
                show_images: false,
            },
            model_scope: None,
            catalog_is_narrowed: false,
        }
    })
}

/// Inspection whose session path is a real, empty session file, so
/// session-scoped reports render instead of failing to open.
pub(super) fn test_run_inspection_with_session(dir: &Path) -> ActiveRunInspection {
    let session_path = dir.join("session.jsonl");
    let mut created = octet_agent::Session::create(&session_path).expect("inspection session");
    // `/export` refuses a session with no resumable conversation, so the
    // fixture carries the smallest resumable turn.
    created
        .append(EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text("inspection fixture".into())],
            },
        )))
        .expect("inspection fixture prompt");
    let resource_owner = created.resource_owner_key();
    drop(created);
    ActiveRunInspection {
        workspace: dir.to_path_buf(),
        invocation_cwd: dir.to_path_buf(),
        session_path,
        resource_owner,
        model: scripted_model("http://127.0.0.1:1"),
        catalog: octet_ai::ModelCatalog::default(),
        reasoning: octet_ai::ReasoningConfig::Off,
        sessions: crate::session_store::SessionStore::for_directory(dir, dir),
        sandbox: SandboxPolicy::default(),
        effect_policy: octet_agent::EffectPolicy::UnsafeHost,
        subagents_available: false,
        service_tier: None,
        goal: Err(ActiveGoalError::UnaddressableSession),
        settings: commands::SettingsSurface {
            default_model: None,
            reasoning: "off".into(),
            theme: None,
            transport: "http",
            endpoint: "test-endpoint".into(),
            show_images: false,
        },
        model_scope: None,
        catalog_is_narrowed: false,
    }
}

/// A run inspection whose durable goal is addressable, exactly as `App`
/// addresses it: the same store, the same driver state, the same session
/// key. The caller keeps its own store handle to read the mutation back.
pub(super) fn test_run_inspection_with_goal(
    dir: &Path,
    store: Arc<octet_agent::DurableGoalStore>,
    driver: octet_agent::GoalDriver,
    session_id: &str,
) -> ActiveRunInspection {
    let mut inspection = test_run_inspection_with_session(dir);
    inspection.goal = GoalAccess::from_parts(store, driver, session_id.to_owned());
    inspection
}

/// Drive one active-run slash command with the minimum test scaffolding.
pub(super) async fn run_active_command(
    shell: &mut InteractiveShell,
    command: Command,
    inspection: &ActiveRunInspection,
) -> (VecDeque<PendingIdleAction>, bool) {
    let (queue, quit_requested, _) =
        run_active_command_observing_deadline(shell, command, inspection).await;
    (queue, quit_requested)
}

/// [`run_active_command`] plus the goal deadline the command armed or
/// cleared, so a mid-run `/goal` is provably applied to the same driver
/// state an idle `/goal` reaches.
pub(super) async fn run_active_command_observing_deadline(
    shell: &mut InteractiveShell,
    command: Command,
    inspection: &ActiveRunInspection,
) -> (VecDeque<PendingIdleAction>, bool, Option<Instant>) {
    let mut queue = VecDeque::new();
    let mut quit_requested = false;
    let mut goal_deadline = None;
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    let mut extensions = crate::extensions::ExecutableExtensions::default();
    let context = octet_agent::ContextSnapshot::default();
    handle_active_command(
        shell,
        command,
        inspection,
        &mut extensions,
        &context,
        &mut goal_deadline,
        |_, _| Ok(None),
        &mut input,
        &mut queue,
        &mut quit_requested,
    )
    .await
    .expect("active command");
    (queue, quit_requested, goal_deadline)
}

pub(super) async fn scripted_agent_with_delay(
    response_delay: Duration,
) -> (wiremock::MockServer, tempfile::TempDir, octet_agent::Agent) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_delay(response_delay)
                .set_body_string(text_turn()),
        )
        .mount(&server)
        .await;

    let (workspace, agent) =
        scripted_agent_for_route(scripted_model(&server.uri()), octet_ai::AiClient::new());
    (server, workspace, agent)
}

pub(super) fn scripted_agent_for_route(
    model: octet_ai::Model,
    client: octet_ai::AiClient,
) -> (tempfile::TempDir, octet_agent::Agent) {
    use octet_agent::{
        Agent, AgentConfig, CoreTools, EffectBroker, ExtensionHost, SandboxConfig, Session,
    };
    let workspace = tempfile::tempdir().unwrap();
    let session_path = workspace.path().join("session.jsonl");
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    let mut sandbox = SandboxConfig::new(workspace.path());
    sandbox.allow_edit = true;
    sandbox.allow_process = true;
    let agent = Agent::new(AgentConfig {
        client,
        model,
        session: Session::create(&session_path).unwrap(),
        system: "test".into(),
        sandbox,
        effect_broker: EffectBroker::default(),
        extensions,
        max_turns: Some(4),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::default(),
        session_id: None,
    })
    .unwrap();
    (workspace, agent)
}

pub(super) async fn scripted_agent() -> (wiremock::MockServer, tempfile::TempDir, octet_agent::Agent)
{
    scripted_agent_with_delay(Duration::ZERO).await
}

// A real loopback HTTP response held after headers, independently of tokens.
// No prompts or response bytes are written to diagnostics.
pub(super) struct HeldApi {
    pub(super) uri: String,
    pub(super) requests: Arc<std::sync::atomic::AtomicUsize>,
    pub(super) bodies: Arc<Mutex<Vec<serde_json::Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for HeldApi {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl HeldApi {
    pub(super) async fn start(
        body: String,
    ) -> (
        Self,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<bool>,
    ) {
        Self::start_with_repeat(body, false).await
    }

    pub(super) async fn start_with_repeat(
        body: String,
        repeat: bool,
    ) -> (
        Self,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<bool>,
    ) {
        use std::sync::atomic::Ordering;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = requests.clone();
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let captured = bodies.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut started_tx = Some(started_tx);
            let mut release_rx = Some(release_rx);
            // The held first body must not block admission of a replacement
            // request after its client times out. This set owns that one
            // body task, so aborting the fixture also retires the socket.
            let mut held_responses = tokio::task::JoinSet::new();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let header_end = loop {
                    let mut bytes = [0; 1024];
                    let n = socket.read(&mut bytes).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&bytes[..n]);
                    assert!(request.len() < 128 * 1024, "bounded fixture request");
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                assert!(length < 128 * 1024);
                while request.len() < header_end + length {
                    let mut bytes = [0; 1024];
                    let n = socket.read(&mut bytes).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&bytes[..n]);
                }
                if repeat {
                    captured.lock().unwrap().push(
                        serde_json::from_slice(&request[header_end..header_end + length]).unwrap(),
                    );
                }
                let attempt = counted.fetch_add(1, Ordering::SeqCst);
                if attempt != 0 && !repeat {
                    // Bound an existing recovery policy without ever replaying
                    // the fixture's successful result on an unexpected POST.
                    socket.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").await.unwrap();
                    continue;
                }
                let headers = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                socket.write_all(headers.as_bytes()).await.unwrap();
                if attempt != 0 {
                    socket.write_all(body.as_bytes()).await.unwrap();
                    continue;
                }
                let _ = started_tx.take().unwrap().send(());
                let release = release_rx.take().unwrap();
                let body = body.clone();
                held_responses.spawn(async move {
                    if let Ok(complete) = release.await {
                        let response = if complete {
                            body.as_bytes()
                        } else {
                            &body.as_bytes()[..1]
                        };
                        let _ = socket.write_all(response).await;
                    }
                });
            }
        });
        (
            Self {
                uri,
                requests,
                bodies,
                task,
            },
            started_rx,
            release_tx,
        )
    }
}

/// Acknowledges only on the poll after the event's handler returned. This
/// proves input handling while the API gate is still held, not after reply.
pub(super) struct ProbedInput {
    pub(super) input: tokio_stream::wrappers::ReceiverStream<std::io::Result<Event>>,
    pub(super) remaining: usize,
    pub(super) handled: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Stream for ProbedInput {
    type Item = std::io::Result<Event>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if self.remaining == 0 {
            if let Some(handled) = self.handled.take() {
                let _ = handled.send(());
            }
        }
        let event = Pin::new(&mut self.input).poll_next(context);
        if matches!(event, std::task::Poll::Ready(Some(_))) {
            self.remaining = self.remaining.saturating_sub(1);
        }
        event
    }
}

/// A real registered first-party command goes through the active dispatcher,
/// not the idle extension dispatcher, while the root provider is held.
pub(super) fn seed_compaction_session(agent: &mut octet_agent::Agent) {
    for index in 0..5 {
        agent
            .session_mut()
            .append(octet_agent::EntryValue::Message(octet_ai::Message::User(
                octet_ai::UserMessage {
                    content: vec![octet_ai::UserPart::Text(format!("fixture user {index}"))],
                },
            )))
            .unwrap();
        agent
            .session_mut()
            .append(octet_agent::EntryValue::Message(
                octet_ai::Message::Assistant(octet_ai::AssistantMessage {
                    content: vec![octet_ai::AssistantPart::Text(format!(
                        "fixture assistant {index}"
                    ))],
                    model: ModelId("scripted".into()),
                    protocol: octet_ai::Protocol::AnthropicMessages,
                }),
            ))
            .unwrap();
    }
    // Retain a user boundary, so this fixture exercises one summary
    // request rather than the separate split-turn-prefix summary request.
    agent
        .session_mut()
        .append(octet_agent::EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text("retained fixture user".into())],
            },
        )))
        .unwrap();
}

/// The platform's "restore queued message" gesture (`app.message.dequeue`):
/// `alt+up` on Unix, `alt+q` in the win32 keymap.
pub(super) fn dequeue_gesture() -> Event {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    Event::Key(KeyEvent::new(
        if cfg!(windows) {
            KeyCode::Char('q')
        } else {
            KeyCode::Up
        },
        KeyModifiers::ALT,
    ))
}
pub(super) fn text_delta(text: &str) -> AgentEvent {
    AgentEvent::OutputDelta {
        channel: OutputChannel::Text,
        text: text.to_owned(),
    }
}
