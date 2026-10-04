//! Native process + real App/Agent consumer tests, including configured startup.
//! Isolated HOME, local transport only; not CLI/PTY or adapter qualification.
use super::*;
use crate::app::{
    bootstrap::{bootstrap, build_app, resolve_launch_print},
    resource_paths::ResourcePathConsumer,
    App,
};
use octet_agent::extension_process::ExtensionActivation;
use octet_agent::{Agent, AgentConfig, EffectBroker, SandboxConfig, Session};
use octet_ai::{
    AiError, AssistantMessage, AssistantPart, Cost, Diagnostic, HostStreamModel,
    HostStreamTransport, Request, Response, ResponseStream, StopReason, StreamEvent, Usage,
};
use std::sync::Mutex;

struct Capture(Arc<Mutex<Vec<Request>>>);
#[async_trait::async_trait]
impl HostStreamTransport for Capture {
    async fn stream(
        &self,
        model: HostStreamModel,
        request: Request,
        _: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        self.0.lock().unwrap().push(request);
        Ok(Box::pin(futures_util::stream::iter([
            Ok(StreamEvent::Started { response_id: None }),
            Ok(StreamEvent::TextStart { index: 0 }),
            Ok(StreamEvent::TextDelta {
                index: 0,
                delta: "answer".into(),
            }),
            Ok(StreamEvent::TextEnd { index: 0 }),
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

pub(crate) async fn fixture(root: &Path) -> (App, ExtensionProcess, Arc<Mutex<Vec<Request>>>) {
    fixture_with_start(root, false).await
}

async fn fixture_with_start(
    root: &Path,
    hold: bool,
) -> (App, ExtensionProcess, Arc<Mutex<Vec<Request>>>) {
    if hold {
        std::fs::write(root.join("hold-start"), "").unwrap();
    }
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut config =
        super::super::tests::executable_extension_config(&workspace, root, "consumer-peer");
    config.start_extension_processes = false; // Production offer remains off.
    config.enabled_extensions.clear();
    config.extension_paths.clear();
    config.mode = crate::config::Mode::Print {
        prompt: "test".into(),
    };
    config.theme = Some("consumer-proof".into());
    let explicit = root.join("explicit-prompts");
    std::fs::create_dir_all(&explicit).unwrap();
    std::fs::write(explicit.join("user-proof.md"), "USER PROMPT").unwrap();
    config.prompt_paths.push(explicit);
    let boot = bootstrap(config).unwrap();
    let launch = resolve_launch_print(&boot, "consumer-test").unwrap();
    let mut app = build_app(boot, launch, "BASE INSTRUCTIONS".into()).unwrap();
    assert!(!app.resource_paths_pending());
    for dir in ["skills/consumer-proof", "prompts", "themes"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(
        root.join("skills/consumer-proof/SKILL.md"),
        "---\nname: consumer-proof\ndescription: CATALOG-SENTINEL\n---\nSKILL-BODY-SENTINEL\n",
    )
    .unwrap();
    std::fs::write(
        root.join("prompts/consumer-proof.md"),
        "PROMPT-SENTINEL $1\n",
    )
    .unwrap();
    std::fs::write(
        root.join("themes/consumer-proof.toml"),
        "[metadata]\nname = \"Consumer Proof\"\n[glyphs]\nprompt = \":\"\n[glyphs_ascii]\nprompt = \":\"\n",
    )
    .unwrap();
    std::fs::write(root.join("reply.json"), serde_json::to_vec(&serde_json::json!({
        "skill_paths": [root.join("skills")], "prompt_paths": [root.join("prompts")], "theme_paths": [root.join("themes")],
    })).unwrap()).unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/extensions/resource_paths/consumer_fixture.py");
    let manifest = ExtensionManifest::parse(&format!(
        r#"
name = "consumer-peer"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "python3"
args = [{script:?}, {root:?}]
[contributes]
hooks = ["session_start", "resources_discover"]
commands = ["release-start"]
"#
    ))
    .unwrap();
    let mut runtime = ExtensionRuntimeConfig::new(&workspace);
    runtime.resource_paths = true; // Handwritten test peer, not a production offer.
    runtime.supervise = false;
    if hold {
        runtime.request_timeout = Duration::from_millis(250);
    }
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path: root.join("extension.toml"),
            source: ExtensionSource::Explicit,
            activation: ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        runtime,
    )
    .await
    .unwrap();
    let session = Session::create(root.join("consumer-session.jsonl")).unwrap();
    let owner = session.resource_owner_key();
    let mut extensions = ExecutableExtensions::default();
    extensions.resource_owner = Some(owner.clone());
    extensions.session_lifecycle_started = true;
    // Exercise the actual deferred hook task and its awaited terminal response.
    extensions
        .pending_session_hook_starts
        .push((process.clone(), owner));
    extensions.processes.push(process.clone());
    let mut host = octet_agent::ExtensionHost::new();
    app.resource_paths = ResourcePathConsumer::new(
        &app.config,
        &app.skills,
        &app.prompts,
        &extensions,
        &mut host,
        crate::app::resource_paths::ResourceConsumerCapability::AppFrontend,
    );
    let captures = Arc::new(Mutex::new(Vec::new()));
    app.client.register_host_stream_transport(
        app.model.endpoint.id.clone(),
        Arc::new(Capture(captures.clone())),
    );
    app.agent = Agent::new(AgentConfig {
        client: app.client.clone(),
        model: app.model.clone(),
        session,
        extensions: host,
        system: app.system.clone(),
        sandbox: SandboxConfig::new(&workspace),
        effect_broker: EffectBroker::default(),
        max_turns: Some(2),
        reasoning: app.reasoning.clone(),
        reasoning_mode: app.reasoning_mode,
        cache_retention: app.config.cache_retention,
        session_id: None,
    })
    .unwrap();
    app.executable_extensions = extensions;
    (app, process, captures)
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_app_publishes_pi_json_theme_and_withdraws_after_empty_reply() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (mut app, process, _) = fixture(&root).await;
    let original = app.original_resource_config();
    let theme_path = root.join("pi-themes/consumer-proof.json");
    std::fs::create_dir_all(theme_path.parent().unwrap()).unwrap();
    std::fs::write(&theme_path, pi_theme::fixture("Published Pi", "#2468ac").to_string()).unwrap();
    std::fs::write(root.join("reply.json"), serde_json::json!({
        "skill_paths": [root.join("skills")],
        "prompt_paths": [root.join("prompts")],
        "theme_paths": [theme_path],
    }).to_string()).unwrap();
    let lease = app.executable_extensions.resource_session_starts().unwrap().await.unwrap();
    let (loaded, lease) = app.prepare_resource_paths(crate::tui::theme::TerminalBackground::Dark, lease).unwrap().await.unwrap();
    let (theme, diagnostics) = app.apply_extension_resource_paths(loaded, lease).unwrap();
    assert_eq!(theme.source_path(), Some(theme_path.as_path()));
    assert_eq!(theme.metadata().name, "Published Pi");
    assert_eq!(theme.resolve::<String>("accent").as_deref(), Some("#2468ac"));
    assert!(!diagnostics.iter().any(|message| message.contains("theme failed")));
    assert!(app.prompts.contains("consumer-proof"));
    assert!(app.system.contains("CATALOG-SENTINEL"));
    assert_eq!(crate::tui::theme::load_theme(&app.config).source_path(), Some(theme_path.as_path()));
    let old_prompts = app.prompts.clone();
    let old_skills = app.skills.clone();
    std::fs::write(root.join("reply.json"), "{}").unwrap();
    app.mark_resource_paths_reload();
    let lease = app.executable_extensions.resource_session_starts().unwrap().await.unwrap();
    let (loaded, lease) = app.prepare_resource_paths(crate::tui::theme::TerminalBackground::Dark, lease).unwrap().await.unwrap();
    let (theme, _) = app.apply_extension_resource_paths(loaded, lease).unwrap();
    assert!(theme.source_path().is_none());
    assert_eq!(app.config.theme_paths, original.theme_paths);
    assert_eq!(app.config.skill_paths, original.skill_paths);
    assert_eq!(app.config.prompt_paths, original.prompt_paths);
    assert!(old_prompts.descriptors().is_empty());
    assert!(old_skills.descriptors().is_empty());
    assert!(!app.system.contains("CATALOG-SENTINEL"));
    assert!(crate::tui::theme::load_theme(&app.config).source_path().is_none());
    assert!(!app.resource_paths_pending());
    process.shutdown().await;
    // An empty responder is still leased: retirement invalidates the baseline.
    assert!(app.resource_paths_pending());
}

async fn complete(app: &mut App) -> Result<octet_agent::RunOutput, octet_agent::AgentError> {
    tokio::time::timeout(Duration::from_secs(10), app.agent.complete("hello"))
        .await
        .unwrap()
}
fn render(
    app: &App,
    name: &str,
) -> Result<crate::prompts::RenderedPrompt, crate::prompts::PromptError> {
    app.prompts.render(
        name,
        "argument",
        &crate::prompts::PromptRenderContext {
            workspace: &app.config.workspace,
            selection: None,
            active_skills: &[],
        },
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manually_admitted_process_app_first_request_reload_and_empty_replace_resources() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (mut app, process, captures) = fixture(&root).await;
    assert!(complete(&mut app).await.is_err());
    assert!(captures.lock().unwrap().is_empty());
    let original = app.original_resource_config();
    app.refresh_resource_paths_headless().await.unwrap();
    assert!(render(&app, "consumer-proof")
        .unwrap()
        .text
        .contains("PROMPT-SENTINEL argument"));
    assert!(render(&app, "user-proof")
        .unwrap()
        .text
        .contains("USER PROMPT"));
    assert!(app
        .skills
        .load(&"consumer-proof".into())
        .unwrap()
        .instructions
        .contains("SKILL-BODY-SENTINEL"));
    assert_eq!(
        crate::tui::theme::load_theme(&app.config).glyph("prompt"),
        ":"
    );
    complete(&mut app).await.unwrap();
    let first = captures.lock().unwrap()[0].system.clone().unwrap();
    assert!(first.contains("BASE INSTRUCTIONS"));
    assert_eq!(first.matches("CATALOG-SENTINEL").count(), 1);
    assert!(!first.contains("SKILL-BODY-SENTINEL"));
    let old_prompts = app.prompts.clone();
    let old_skills = app.skills.clone();
    // Turn-local composition must not become the idle resource baseline.
    app.agent.set_system_prompt("TURN LOCAL");
    app.agent.set_system_prompt(app.system.clone());
    std::fs::write(root.join("reply.json"), "{}").unwrap();
    app.mark_resource_paths_reload();
    app.refresh_resource_paths_headless().await.unwrap();
    assert!(old_prompts.descriptors().is_empty());
    assert!(old_skills.descriptors().is_empty());
    assert!(!app.prompts.contains("consumer-proof"));
    assert!(!app.system.contains("CATALOG-SENTINEL"));
    assert_eq!(app.config.prompt_paths, original.prompt_paths);
    assert_eq!(app.config.skill_paths, original.skill_paths);
    assert_eq!(app.config.theme_paths, original.theme_paths);
    assert!(crate::tui::theme::load_theme(&app.config)
        .source_path()
        .is_none());
    complete(&mut app).await.unwrap();
    assert!(!captures
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .system
        .as_ref()
        .unwrap()
        .contains("CATALOG-SENTINEL"));
    let calls = std::fs::read_to_string(root.join("consumer-calls.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        calls
            .iter()
            .map(|v| v["hook"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["session_start", "resources_discover", "resources_discover"]
    );
    assert_eq!(calls[1]["payload"]["reason"], "startup");
    assert_eq!(calls[2]["payload"]["reason"], "reload");
    process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_blocks_next_actual_provider_and_old_clones_then_withdraws_to_original_paths() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (mut app, process, captures) = fixture(&root).await;
    app.refresh_resource_paths_headless().await.unwrap();
    complete(&mut app).await.unwrap();
    let count = captures.lock().unwrap().len();
    let old = app.prompts.clone();
    process.shutdown().await;
    assert!(app.resource_paths_pending());
    assert!(app.skills.descriptors().is_empty());
    assert!(old.descriptors().is_empty());
    assert!(complete(&mut app).await.is_err());
    assert_eq!(captures.lock().unwrap().len(), count);
    app.refresh_resource_paths_headless().await.unwrap();
    assert!(render(&app, "user-proof").is_ok());
    assert!(!app.system.contains("CATALOG-SENTINEL"));
    complete(&mut app).await.unwrap();
    assert_eq!(captures.lock().unwrap().len(), count + 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_app_rejects_generation_and_owner_retirement_after_loader_completion() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (mut app, process, _) = fixture(&root).await;
    let lease = app
        .executable_extensions
        .resource_session_starts()
        .unwrap()
        .await
        .unwrap();
    let (loaded, lease) = app
        .prepare_resource_paths(crate::tui::theme::TerminalBackground::Dark, lease)
        .unwrap()
        .await
        .unwrap();
    let system = app.system.clone();
    let paths = app.config.prompt_paths.clone();
    process.reload().await.unwrap();
    assert!(app.apply_extension_resource_paths(loaded, lease).is_err());
    assert_eq!(app.system, system);
    assert_eq!(app.config.prompt_paths, paths);
    app.refresh_resource_paths_headless().await.unwrap();
    app.executable_extensions.retire_active_resources();
    assert!(app.resource_paths_pending());
    assert!(app.prompts.descriptors().is_empty());
    process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_catalog_suffix_refuses_actual_app_publication_without_partial_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (mut app, process, _) = fixture(&root).await;
    app.refresh_resource_paths_headless().await.unwrap();
    app.mark_resource_paths_reload();
    let lease = app
        .executable_extensions
        .resource_session_starts()
        .unwrap()
        .await
        .unwrap();
    let (loaded, lease) = app
        .prepare_resource_paths(crate::tui::theme::TerminalBackground::Dark, lease)
        .unwrap()
        .await
        .unwrap();
    app.system = "CORRUPTED BASE".into();
    let paths = app.config.prompt_paths.clone();
    let prompts = app.prompts.clone();
    assert!(app.apply_extension_resource_paths(loaded, lease).is_err());
    assert_eq!(app.system, "CORRUPTED BASE");
    assert_eq!(app.config.prompt_paths, paths);
    assert!(Arc::ptr_eq(&app.prompts, &prompts));
    process.shutdown().await;
}

pub(crate) fn hold_session_start(app: &mut App, wait: tokio::sync::oneshot::Receiver<()>) {
    let (process, owner) = app
        .executable_extensions
        .pending_session_hook_starts
        .pop()
        .unwrap();
    app.executable_extensions
        .session_hook_start_tasks
        .push(tokio::spawn(async move {
            wait.await.unwrap();
            process.start_session_hook_binding(owner).await.unwrap();
        }));
}

struct HeldCapture {
    requests: Arc<Mutex<Vec<Request>>>,
    barrier: Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
}
#[async_trait::async_trait]
impl HostStreamTransport for HeldCapture {
    async fn stream(
        &self,
        model: HostStreamModel,
        request: Request,
        diagnostics: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        let barrier = self.barrier.lock().unwrap().take();
        if let Some((entered, release)) = barrier {
            entered.send(()).unwrap();
            release.await.unwrap();
        }
        Capture(self.requests.clone())
            .stream(model, request, diagnostics)
            .await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_during_actual_run_refuses_queued_next_turn_before_transport() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (mut app, process, captures) = fixture(&root).await;
    app.refresh_resource_paths_headless().await.unwrap();
    let (entered, arrived) = tokio::sync::oneshot::channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    app.client.register_host_stream_transport(
        app.model.endpoint.id.clone(),
        Arc::new(HeldCapture {
            requests: captures.clone(),
            barrier: Mutex::new(Some((entered, wait))),
        }),
    );
    let mut run = app.agent.prompt("first request").await.unwrap();
    let control = run.control();
    let driver = async {
        arrived.await.unwrap();
        control.follow_up("second request").await.unwrap();
        process.shutdown().await;
        release.send(()).unwrap();
    };
    let consume = async {
        let mut reason = None;
        while let Some(event) = run.next().await {
            if let octet_agent::AgentEvent::RunFinished {
                reason: finished, ..
            } = event
            {
                reason = Some(finished);
            }
        }
        assert!(matches!(
            reason,
            Some(octet_agent::FinishReason::Failed(
                octet_agent::AgentError::ProviderContextPreparation(_)
            ))
        ));
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(driver, consume);
    })
    .await
    .unwrap();
    assert_eq!(
        captures.lock().unwrap().len(),
        1,
        "no post-retirement next request"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_or_cancelled_start_never_discovers_even_after_late_terminal_reply() {
    for cancel in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let (mut app, process, captures) = fixture_with_start(&root, true).await;
        if cancel {
            let (p, owner) = app
                .executable_extensions
                .pending_session_hook_starts
                .pop()
                .unwrap();
            let task = tokio::spawn(async move { p.start_session_hook_binding(owner).await });
            // An explicit subprocess log receipt, not a fixed timer grace.
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if std::fs::read_to_string(root.join("consumer-calls.jsonl"))
                        .unwrap_or_default()
                        .contains("session_start")
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        }
        app.refresh_resource_paths_headless().await.unwrap();
        assert!(
            !app.resource_paths_pending(),
            "failed contributor must not spin the barrier"
        );
        assert!(!app.prompts.contains("consumer-proof"));
        assert!(render(&app, "user-proof").is_ok());
        complete(&mut app).await.unwrap();
        assert_eq!(captures.lock().unwrap().len(), 1);
        let owner = app.executable_extensions.resource_owner.as_deref();
        let context = super::super::extension_execution_context(&process, owner);
        assert_eq!(
            process
                .execute_command("release-start", vec![], context)
                .await
                .unwrap()
                .text,
            "late-start-sent"
        );
        app.mark_resource_paths_reload();
        app.refresh_resource_paths_headless().await.unwrap();
        let calls = std::fs::read_to_string(root.join("consumer-calls.jsonl")).unwrap();
        assert_eq!(calls.matches("session_start").count(), 1, "no start replay");
        assert!(
            !calls.contains("resources_discover"),
            "failed binding remains excluded after its late terminal"
        );
        process.shutdown().await;
    }
}

/// Normal discovery/trust/activation and App construction. No Agent replacement
/// and no low-level process/runtime flag injection in this fixture.
pub(crate) fn configured_app(root: &Path, authorized: bool, start_processes: bool) -> App {
    let workspace = root.join("workspace");
    let extensions = root.join("extensions");
    let bundle = extensions.join("consumer-peer");
    for dir in [
        &workspace,
        &bundle,
        &root.join("skills/consumer-proof"),
        &root.join("prompts"),
        &root.join("themes"),
    ] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(
        root.join("skills/consumer-proof/SKILL.md"),
        "---\nname: consumer-proof\ndescription: CATALOG-SENTINEL\n---\nSKILL-BODY-SENTINEL\n",
    )
    .unwrap();
    std::fs::write(
        root.join("prompts/consumer-proof.md"),
        "PROMPT-SENTINEL $1\n",
    )
    .unwrap();
    std::fs::write(
        root.join("themes/consumer-proof.toml"),
        "[metadata]\nname = \"Consumer Proof\"\n[glyphs]\nprompt = \":\"\n[glyphs_ascii]\nprompt = \":\"\n",
    )
    .unwrap();
    std::fs::write(root.join("reply.json"), serde_json::to_vec(&serde_json::json!({
        "skill_paths": [root.join("skills")], "prompt_paths": [root.join("prompts")], "theme_paths": [root.join("themes")],
    })).unwrap()).unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join(if root.join("reverse-requests").exists() {
        "src/extensions/resource_paths/fixture.py"
    } else {
        "src/extensions/resource_paths/consumer_fixture.py"
    });
    std::fs::write(
        bundle.join("extension.toml"),
        format!(
            r#"
name = "consumer-peer"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "python3"
args = [{script:?}, {root:?}]
[contributes]
hooks = ["session_start", "resources_discover"]
commands = ["release-start"]
"#
        ),
    )
    .unwrap();
    let mut config =
        super::super::tests::executable_extension_config(&workspace, &extensions, "consumer-peer");
    config.mode = crate::config::Mode::Print {
        prompt: "test".into(),
    };
    config.start_extension_processes = start_processes;
    config.theme = Some("consumer-proof".into());
    let boot = bootstrap(config).unwrap();
    let launch = resolve_launch_print(&boot, "configured-consumer").unwrap();
    if authorized {
        crate::app::bootstrap::build_app_with_resource_consumer(
            boot,
            launch,
            "BASE INSTRUCTIONS".into(),
        )
        .unwrap()
    } else {
        // Same Mode::Print: capability comes from constructor, not Mode.
        build_app(boot, launch, "BASE INSTRUCTIONS".into()).unwrap()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authorized_frontend_constructor_drives_genuine_app_startup_before_first_request() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let mut app = configured_app(&root, true, true);
    assert!(app.resource_paths_pending());
    assert_eq!(app.executable_extensions.processes.len(), 1);
    let captures = Arc::new(Mutex::new(Vec::new()));
    app.client.register_host_stream_transport(
        app.model.endpoint.id.clone(),
        Arc::new(Capture(captures.clone())),
    );
    app.refresh_resource_paths_headless().await.unwrap();
    assert!(render(&app, "consumer-proof")
        .unwrap()
        .text
        .contains("PROMPT-SENTINEL argument"));
    assert!(app
        .skills
        .load(&"consumer-proof".into())
        .unwrap()
        .instructions
        .contains("SKILL-BODY-SENTINEL"));
    complete(&mut app).await.unwrap();
    assert_eq!(
        captures.lock().unwrap()[0]
            .system
            .as_ref()
            .unwrap()
            .matches("CATALOG-SENTINEL")
            .count(),
        1
    );
    let calls = std::fs::read_to_string(root.join("consumer-calls.jsonl")).unwrap();
    assert!(calls.find("session_start").unwrap() < calls.find("resources_discover").unwrap());
    assert_eq!(calls.matches("resources_discover").count(), 1);
    let offer = std::fs::read_to_string(root.join("initialize.jsonl")).unwrap();
    assert!(offer.contains("resource_paths_v1"));
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mode_does_not_authorize_discovery_and_process_denial_stays_authoritative() {
    for (authorized, start_processes) in [(false, true), (true, false)] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut app = configured_app(&root, authorized, start_processes);
        assert!(!app.resource_paths_pending());
        app.refresh_resource_paths_headless().await.unwrap();
        let offer = std::fs::read_to_string(root.join("initialize.jsonl")).unwrap_or_default();
        assert!(!offer.contains("resource_paths_v1"));
        let calls = std::fs::read_to_string(root.join("consumer-calls.jsonl")).unwrap_or_default();
        assert!(!calls.contains("resources_discover"));
        if !start_processes {
            assert!(offer.is_empty());
        }
        app.executable_extensions.shutdown().await;
    }
}


#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn headless_resource_phase_answers_reverse_requests_with_real_refusals() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::write(root.join("reverse-requests"), "headless").unwrap();
    let mut app = configured_app(&root, true, true);
    tokio::time::timeout(Duration::from_secs(10), app.refresh_resource_paths_headless())
        .await.unwrap().unwrap();
    assert!(app.prompts.contains("consumer-proof"), "a refusal must let the peer finish, not timeout and withdraw");
    let replies = std::fs::read_to_string(root.join("reverse-replies.jsonl")).unwrap();
    let replies = replies.lines().map(|line| serde_json::from_str::<Value>(line).unwrap()).collect::<Vec<_>>();
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0]["hook"], "session_start");
    assert_eq!(replies[1]["hook"], "resources_discover");
    assert!(replies.iter().all(|reply| reply["reply"]["error"]["message"].as_str().unwrap().contains("no foreground session")));
    app.executable_extensions.shutdown().await;
}
