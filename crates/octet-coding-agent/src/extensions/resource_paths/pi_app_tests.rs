//! Actual reviewed configure -> Node adapter -> authorized App -> local Agent transport.
//! No handwritten protocol peer, low-level offer, Agent replacement or CLM shortcut.
use crate::app::App;
use octet_ai::{
    AiError, AssistantMessage, AssistantPart, Cost, Diagnostic, HostStreamModel,
    HostStreamTransport, Message, Request, Response, ResponseStream, StopReason, StreamEvent,
    Usage, UserPart,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

fn configured_app_from_config(config: crate::config::Config, authorized: bool) -> App {
    assert!(authorized);
    let boot = crate::app::bootstrap::bootstrap(config).unwrap();
    let launch =
        crate::app::bootstrap::resolve_launch_print(&boot, "pi-configured-consumer").unwrap();
    crate::app::bootstrap::build_app_with_resource_consumer(
        boot,
        launch,
        "BASE INSTRUCTIONS".into(),
    )
    .unwrap()
}

struct ConfiguredPi {
    app: App,
    capture_pid: u64,
    manifest: Vec<u8>,
    bridge: Vec<u8>,
}

fn put(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) {
    let path = path.as_ref();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn phase(root: &Path, phase: &str, fail_start: bool) {
    put(
        root.join("workspace/resource-state.json"),
        serde_json::to_vec(&json!({"phase": phase, "failStart": fail_start})).unwrap(),
    );
}

fn trace(root: &Path) -> Vec<Value> {
    std::fs::read_to_string(root.join("factories/trace.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn adapter_root() -> PathBuf {
    // Test-only path selection. Default is the product's real adapter package;
    // the staging receipt also supports replaying its frozen dependency snapshot.
    std::env::var_os("OCTET_PI_COMPAT_TEST_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/octet-pi-compat")
        })
        .canonicalize()
        .expect("reviewed Pi adapter and its existing local dependencies are required")
}

fn configured_pi(root: &Path, fail_start: bool) -> ConfiguredPi {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    for (name, code) in [
        ("common.mjs", include_str!("pi_app_fixture/common.mjs")),
        ("first.mjs", include_str!("pi_app_fixture/first.mjs")),
        ("second.mjs", include_str!("pi_app_fixture/second.mjs")),
    ] {
        put(root.join("factories").join(name), code);
    }
    for current in ["a", "b"] {
        for factory in ["first", "second"] {
            let assets = workspace.join("resources").join(current).join(factory);
            let name = format!("pi-{factory}-{current}");
            put(
                assets.join("skills").join(&name).join("SKILL.md"),
                format!("---\nname: {name}\ndescription: PI-{factory}-{current}-CATALOG\n---\nPI-{factory}-{current}-BODY\n"),
            );
            put(
                assets.join("prompts").join(format!("{name}.md")),
                format!("PI-{factory}-{current}-PROMPT $1\n"),
            );
        }
        put(
            workspace
                .join("resources")
                .join(current)
                .join("first/themes/pi-resource-proof.toml"),
            format!(
                "[metadata]\nname = \"Pi Resource {current}\"\n[glyphs]\nprompt = \"{glyph}\"\n[glyphs_ascii]\nprompt = \"{glyph}\"\n",
                glyph = if current == "a" { ":" } else { ">" }
            ),
        );
    }
    put(
        root.join("explicit/skills/pi-explicit/SKILL.md"),
        "---\nname: pi-explicit\ndescription: PI-EXPLICIT-CATALOG\n---\nPI-EXPLICIT-BODY\n",
    );
    put(
        root.join("explicit/prompts/user-proof.md"),
        "USER PROMPT $1\n",
    );
    std::fs::create_dir_all(root.join("explicit/themes")).unwrap();
    phase(root, "a", fail_start);

    let adapter = adapter_root();
    let output = root.join("extensions/octet-pi-compat");
    let configured = Command::new("node")
        .arg(adapter.join("configure.mjs"))
        .arg("--reviewed")
        .arg("--output")
        .arg(&output)
        .arg(root.join("factories/first.mjs"))
        .arg(root.join("factories/second.mjs"))
        .current_dir(&workspace)
        .env("PI_OFFLINE", "1")
        .output()
        .expect("existing Node 22.19+ and local adapter dependencies required; never auto-install");
    assert!(
        configured.status.success(),
        "configure capture failed: {}",
        String::from_utf8_lossy(&configured.stderr)
    );
    let captured = trace(root);
    assert_eq!(
        captured.len(),
        2,
        "configure must execute factories, not their hooks"
    );
    assert_eq!(captured[0]["kind"], "load");
    assert_eq!(captured[0]["factory"], "first");
    assert_eq!(captured[1]["kind"], "load");
    assert_eq!(captured[1]["factory"], "second");
    let capture_pid = captured[0]["pid"].as_u64().unwrap();
    assert_eq!(captured[1]["pid"], capture_pid);

    let manifest = std::fs::read(output.join("extension.toml")).unwrap();
    let bridge = std::fs::read(output.join("bridge.json")).unwrap();
    let registrations: Value = serde_json::from_slice(&bridge).unwrap();
    // configure reserves every Pi-mapped native hook (d7be74f4) so handlers
    // registered after load still receive events, except provider wire hooks
    // no factory registered (they would refuse host-stream providers).
    assert_eq!(
        registrations["registrations"]["hooks"],
        json!([
            "after_response",
            "after_tool_call",
            "before_prompt",
            "before_tool_call",
            "model_turn_end",
            "model_turn_start",
            "provider_context",
            "resources_discover",
            "session_before_compact",
            "session_before_fork",
            "session_before_switch",
            "session_before_tree",
            "session_compact",
            "session_end",
            "session_start",
            "session_tree"
        ])
    );
    assert_eq!(
        registrations["registrations"]["events"],
        json!(["resources_discover", "session_start"])
    );
    assert_eq!(
        registrations["extensions"],
        json!([
            root.join("factories/first.mjs"),
            root.join("factories/second.mjs")
        ])
    );
    // Use the generated, user-facing manifest verbatim. No alternate runner or
    // fabricated catalog/owner, and no direct ExtensionProcess::start call.
    assert!(
        String::from_utf8_lossy(&manifest).contains(adapter.join("runner.mjs").to_str().unwrap())
    );
    let mut config = crate::extensions::tests::executable_extension_config(
        &workspace,
        &root.join("extensions"),
        "octet-pi-compat",
    );
    config.mode = crate::config::Mode::Print {
        prompt: "Pi resource integration".into(),
    };
    config.workspace_trusted = true; // Explicit test workspace authorization, not discovery laundering.
    config.skill_paths = vec![root.join("explicit/skills")];
    config.prompt_paths = vec![root.join("explicit/prompts")];
    config.theme_paths = vec![root.join("explicit/themes")];
    config.theme = Some("pi-resource-proof".into());
    let app = configured_app_from_config(config, true);
    assert_eq!(
        app.executable_extensions.processes.len(),
        1,
        "real configured adapter failed admission"
    );
    assert!(app.executable_extensions.processes[0].supports_feature("resource_paths_v1"));
    assert!(app.resource_paths_pending());
    ConfiguredPi {
        app,
        capture_pid,
        manifest,
        bridge,
    }
}

fn assert_unchanged_capture(root: &Path, fixture: &ConfiguredPi) {
    assert_eq!(
        std::fs::read(root.join("extensions/octet-pi-compat/extension.toml")).unwrap(),
        fixture.manifest
    );
    assert_eq!(
        std::fs::read(root.join("extensions/octet-pi-compat/bridge.json")).unwrap(),
        fixture.bridge
    );
}

fn assert_phase(app: &App, root: &Path, current: &str, captures: &[Request]) {
    let old = if current == "a" { "b" } else { "a" };
    assert_eq!(
        app.config.skill_paths,
        vec![
            root.join(format!("workspace/resources/{current}/first/skills")),
            root.join(format!("workspace/resources/{current}/second/skills")),
            root.join("explicit/skills")
        ]
    );
    assert_eq!(
        app.config.prompt_paths,
        vec![
            root.join(format!("workspace/resources/{current}/first/prompts")),
            root.join(format!("workspace/resources/{current}/second/prompts")),
            root.join("explicit/prompts")
        ]
    );
    assert_eq!(
        app.config.theme_paths,
        vec![
            root.join(format!("workspace/resources/{current}/first/themes")),
            root.join("explicit/themes")
        ]
    );
    let system = captures.last().unwrap().system.as_ref().unwrap();
    assert!(system.contains("BASE INSTRUCTIONS"));
    assert_eq!(system.matches("PI-EXPLICIT-CATALOG").count(), 1);
    for factory in ["first", "second"] {
        let name = format!("pi-{factory}-{current}");
        assert!(app
            .skills
            .load(&name.clone().into())
            .unwrap()
            .instructions
            .contains(&format!("PI-{factory}-{current}-BODY")));
        assert!(render(app, &name)
            .unwrap()
            .text
            .contains(&format!("PI-{factory}-{current}-PROMPT argument")));
        assert!(!app.prompts.contains(&format!("pi-{factory}-{old}")));
        assert!(app
            .skills
            .load(&format!("pi-{factory}-{old}").into())
            .is_err());
        assert_eq!(
            system
                .matches(&format!("PI-{factory}-{current}-CATALOG"))
                .count(),
            1
        );
        assert!(!system.contains(&format!("PI-{factory}-{old}-CATALOG")));
        assert!(!system.contains(&format!("PI-{factory}-{current}-BODY")));
    }
    let selected = crate::tui::theme::load_theme(&app.config);
    assert_eq!(
        selected.source_path(),
        Some(
            root.join(format!(
                "workspace/resources/{current}/first/themes/pi-resource-proof.toml"
            ))
            .as_path()
        )
    );
    assert_eq!(
        selected.glyph("prompt"),
        if current == "a" { ":" } else { ">" }
    );
    assert!(render(app, "user-proof")
        .unwrap()
        .text
        .contains("USER PROMPT argument"));
    assert!(!app.resource_paths_pending());
}

async fn complete_rendered(app: &mut App, name: &str) {
    // Real native template expansion becomes the actual Agent user input.
    let text = render(app, name).unwrap().text;
    tokio::time::timeout(Duration::from_secs(10), app.agent.complete(text))
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_pi_factories_first_provider_reload_and_empty_withdrawal() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let mut fixture = configured_pi(&root, false);
    let captures = Arc::new(Mutex::new(Vec::new()));
    fixture.app.client.register_host_stream_transport(
        fixture.app.model.endpoint.id.clone(),
        Arc::new(Capture(captures.clone())),
    );
    let original = fixture.app.original_resource_config();
    // No discovery has run during capture/initialize/session_start and no
    // provider is contacted until the actual App headless publication barrier.
    assert!(trace(&root).iter().all(|row| row["kind"] != "discover"));
    assert!(captures.lock().unwrap().is_empty());
    fixture.app.refresh_resource_paths_headless().await.unwrap();
    complete_rendered(&mut fixture.app, "pi-first-a").await;
    {
        let requests = captures.lock().unwrap();
        assert_eq!(requests.len(), 1, "first actual provider request only");
        assert_phase(&fixture.app, &root, "a", &requests);
        assert!(requests[0].messages.iter().any(|message| matches!(message, Message::User(user) if user.content.iter().any(|part| matches!(part, UserPart::Text(text) if text.contains("PI-first-a-PROMPT argument"))))));
    }
    let old_prompts = fixture.app.prompts.clone();
    let old_skills = fixture.app.skills.clone();
    phase(&root, "b", false);
    fixture.app.mark_resource_paths_reload();
    fixture.app.refresh_resource_paths_headless().await.unwrap();
    assert!(old_prompts.descriptors().is_empty());
    assert!(old_skills.descriptors().is_empty());
    complete_rendered(&mut fixture.app, "pi-first-b").await;
    {
        let requests = captures.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_phase(&fixture.app, &root, "b", &requests);
    }
    phase(&root, "empty", false);
    fixture.app.mark_resource_paths_reload();
    fixture.app.refresh_resource_paths_headless().await.unwrap();
    for current in ["a", "b"] {
        for factory in ["first", "second"] {
            let name = format!("pi-{factory}-{current}");
            assert!(!fixture.app.prompts.contains(&name));
            assert!(fixture.app.skills.load(&name.into()).is_err());
        }
    }
    assert_eq!(fixture.app.config.skill_paths, original.skill_paths);
    assert_eq!(fixture.app.config.prompt_paths, original.prompt_paths);
    assert_eq!(fixture.app.config.theme_paths, original.theme_paths);
    assert!(crate::tui::theme::load_theme(&fixture.app.config)
        .source_path()
        .is_none());
    assert!(fixture.app.skills.load(&"pi-explicit".into()).is_ok());
    complete_rendered(&mut fixture.app, "user-proof").await;
    {
        let requests = captures.lock().unwrap();
        assert_eq!(requests.len(), 3);
        let system = requests[2].system.as_ref().unwrap();
        assert!(system.contains("BASE INSTRUCTIONS"));
        assert_eq!(system.matches("PI-EXPLICIT-CATALOG").count(), 1);
        assert!(!system.contains("PI-first-") && !system.contains("PI-second-"));
    }
    assert_unchanged_capture(&root, &fixture);
    let rows = trace(&root);
    let active_pid = rows[2]["pid"].as_u64().unwrap();
    assert_ne!(
        active_pid, fixture.capture_pid,
        "configure capture is not the native runtime process"
    );
    assert_eq!(
        rows.len(),
        12,
        "no hidden preflight process, replay, or duplicate discovery"
    );
    assert_eq!(rows[2]["kind"], "load");
    assert_eq!(rows[3]["kind"], "load");
    assert_eq!(rows[4]["kind"], "started");
    assert_eq!(rows[5]["kind"], "started");
    assert_eq!(rows[4]["factory"], "first");
    assert_eq!(rows[5]["factory"], "second");
    for (index, row) in rows.iter().skip(2).enumerate() {
        assert_eq!(row["pid"], active_pid);
        if index >= 4 {
            let discovery = index - 4;
            assert_eq!(row["kind"], "discover");
            assert_eq!(
                row["factory"],
                if discovery % 2 == 0 {
                    "first"
                } else {
                    "second"
                }
            );
            assert_eq!(row["cwd"], json!(root.join("workspace")));
            assert_eq!(row["contextCwd"], row["cwd"]);
            assert_eq!(row["phase"], ["a", "b", "empty"][discovery / 2]);
            assert_eq!(
                row["reason"],
                if discovery < 2 { "startup" } else { "reload" }
            );
        }
    }
    fixture.app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_pi_failed_start_cannot_discover_or_become_success_by_reload() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let mut fixture = configured_pi(&root, true);
    let captures = Arc::new(Mutex::new(Vec::new()));
    fixture.app.client.register_host_stream_transport(
        fixture.app.model.endpoint.id.clone(),
        Arc::new(Capture(captures.clone())),
    );
    fixture.app.refresh_resource_paths_headless().await.unwrap();
    // Removing the local cause is not permission to replay session_start or
    // convert an idempotent binding lookup into a successful start outcome.
    phase(&root, "b", false);
    fixture.app.mark_resource_paths_reload();
    fixture.app.refresh_resource_paths_headless().await.unwrap();
    assert!(!fixture.app.prompts.contains("pi-first-a"));
    assert!(!fixture.app.prompts.contains("pi-first-b"));
    assert!(render(&fixture.app, "user-proof").is_ok());
    complete(&mut fixture.app).await.unwrap();
    let requests = captures.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let system = requests[0].system.as_ref().unwrap();
    assert_eq!(system.matches("PI-EXPLICIT-CATALOG").count(), 1);
    assert!(!system.contains("PI-first-") && !system.contains("PI-second-"));
    drop(requests);
    let rows = trace(&root);
    assert_eq!(
        rows.iter()
            .filter(|row| row["kind"] == "start_failed")
            .count(),
        1
    );
    assert!(!rows
        .iter()
        .any(|row| row["kind"] == "started" || row["kind"] == "discover"));
    assert_unchanged_capture(&root, &fixture);
    fixture.app.executable_extensions.shutdown().await;
}
