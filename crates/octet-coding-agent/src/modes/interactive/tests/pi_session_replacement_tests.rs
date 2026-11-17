//! Pi 1.0.2 command-context session replacement through the real interactive
//! host: the real App, the reviewed Node adapter fleet, the real lifecycle
//! driver and the same command pump a typed `/probe` uses. Only the Pi factory
//! is inline; no protocol peer, owner or session is fabricated.
#![cfg(unix)]
use super::*;

/// Configures the real adapter with one inline reviewed Pi factory and starts
/// the real fleet with the session lifecycle driver active. `TRACE` in the
/// factory is replaced by an absolute JSON-lines trace path.
fn pi_app(factory: &str) -> (tempfile::TempDir, App) {
    let (directory, mut app) = crate::compaction::tests::app_for_estimate();
    let entry = directory.path().join("probe.ts");
    let trace = serde_json::to_string(&directory.path().join("trace.jsonl")).unwrap();
    std::fs::write(&entry, factory.replace("TRACE", &trace)).unwrap();
    let extension_root = directory.path().join("extensions");
    let adapter = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../extensions/octet-pi-compat")
        .canonicalize()
        .unwrap();
    let configured = std::process::Command::new("node")
        .arg(adapter.join("configure.mjs"))
        .arg("--reviewed")
        .arg("--output")
        .arg(extension_root.join("octet-pi-compat"))
        .arg(&entry)
        .current_dir(&app.config.workspace)
        .env("PI_OFFLINE", "1")
        .env("OCTET_PI_AGENT_DIR", directory.path().join("pi-agent"))
        .output()
        .expect("existing Node and local adapter dependencies are required");
    assert!(
        configured.status.success(),
        "{}",
        String::from_utf8_lossy(&configured.stderr)
    );
    app.config.extension_paths = vec![extension_root];
    app.config.enabled_extensions = vec!["octet-pi-compat".into()];
    app.config.invocation_trusted_extensions = vec!["octet-pi-compat".into()];
    app.config.workspace_trusted = true;
    app.config.mode = crate::config::Mode::Interactive;
    app.config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    app.config.sandbox.allow_process = true;
    app.config.sandbox.allow_shell = true;
    let mut host = octet_agent::ExtensionHost::new();
    let mut extensions =
        crate::extensions::ExecutableExtensions::discover_and_start_with_provider_runtime(
            &app.config,
            app.agent.session(),
            &app.model,
            &app.reasoning,
            &app.sessions,
            &mut host,
            None,
            crate::extensions::ExtensionProviderRuntime::default(),
            crate::app::resource_paths::ResourceConsumerCapability::AppFrontend,
            None,
        );
    assert!(
        extensions
            .summaries()
            .iter()
            .any(|summary| summary.name == "octet-pi-compat" && summary.running),
        "{}",
        extensions.inspect_text()
    );
    // The estimate fixture has no resource consumer. Preserve the actual App
    // frontend offer through model/session rebuilds now that every configured
    // bridge reserves palette discovery.
    app.resource_paths = crate::app::resource_paths::ResourcePathConsumer::new(
        &app.config,
        &app.skills,
        &app.prompts,
        &mut extensions,
        &mut host,
        crate::app::resource_paths::ResourceConsumerCapability::AppFrontend,
    );
    app.executable_extensions = extensions;
    app.executable_extensions
        .activate_session_lifecycle_driver();
    (directory, app)
}

/// Runs `/probe` through the real interactive command pump.
async fn probe(app: &mut App, shell: &mut InteractiveShell) -> anyhow::Result<String> {
    probe_with_arguments(app, shell, Vec::new()).await
}

async fn probe_with_arguments(
    app: &mut App,
    shell: &mut InteractiveShell,
    arguments: Vec<String>,
) -> anyhow::Result<String> {
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    resource_paths::refresh_resource_paths(app, shell, &mut input).await?;
    let dialogs = app.executable_extensions.lifecycle_snapshot();
    let mut frontend = InteractiveExtensionConfirmations {
        shell,
        input: &mut input,
        dialogs: &dialogs,
        input_retired: false,
    };
    tokio::time::timeout(
        Duration::from_secs(20),
        run_interactive_extension_command(
            app,
            &mut frontend,
            Some("octet-pi-compat"),
            "probe",
            arguments,
            false,
            0,
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("probe command timed out"))?
    .map(|output| output.expect("probe command is registered"))
}

fn trace(directory: &tempfile::TempDir) -> Vec<serde_json::Value> {
    std::fs::read_to_string(directory.path().join("trace.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn step<'a>(trace: &'a [serde_json::Value], name: &str) -> &'a serde_json::Value {
    trace
        .iter()
        .find(|value| value["step"] == name)
        .unwrap_or_else(|| panic!("no {name} step in {trace:#?}"))
}

fn session_id(session: &Session) -> String {
    session
        .path()
        .file_stem()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned()
}

/// Every durable entry of a session file whose record names `needle`.
fn entries_naming(path: &std::path::Path, needle: &str) -> Vec<serde_json::Value> {
    Session::open_read_only(path)
        .unwrap()
        .entries()
        .iter()
        .map(|entry| serde_json::to_value(entry).unwrap())
        .filter(|entry| entry.to_string().contains(needle))
        .collect()
}

const NEW_SESSION: &str = r#"
import { appendFileSync } from 'node:fs';
const trace = value => appendFileSync(TRACE, JSON.stringify(value) + '\n');
export default pi => {
  pi.registerCommand('probe', { handler: async (_args, ctx) => {
    const before = ctx.sessionManager.getSessionId();
    trace({ step: 'before', id: before });
    const result = await ctx.newSession({ withSession: async fresh => {
      const id = fresh.sessionManager.getSessionId();
      trace({ step: 'with', id });
      pi.appendEntry('probe-marker', { id, before });
      trace({ step: 'appended', id });
      fresh.ui.notify('fresh ' + id);
    } });
    trace({ step: 'after', result });
  } });
};
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_new_session_runs_with_session_bound_to_the_replacement() {
    assert_new_session(NEW_SESSION, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_new_session_with_explicit_start_hook_preserves_command_parent() {
    assert_new_session(
        &NEW_SESSION.replace(
            "export default pi => {",
            "export default pi => { pi.on('session_start', () => {});",
        ),
        false,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_new_session_with_delegation_keeps_extension_process() {
    assert_new_session(NEW_SESSION, true).await;
}

fn enable_test_delegation(mut app: App, uri: &str) -> App {
    let mut model = super::support::scripted_model(uri);
    Arc::make_mut(&mut model.spec).capabilities.agent_delegation =
        Some(octet_ai::AgentDelegation::V2);
    app.catalog
        .register_endpoint((*model.endpoint).clone())
        .unwrap();
    app.catalog.register_model((*model.spec).clone()).unwrap();
    let app = rebuild_app(app, Some(model), None, None, None).unwrap();
    assert!(app.agent.delegation_team_directory().is_some());
    app.executable_extensions
        .activate_session_lifecycle_driver();
    app
}

async fn assert_new_session(factory: &str, delegation: bool) {
    let (directory, mut app) = pi_app(factory);
    if delegation {
        app = enable_test_delegation(app, "http://127.0.0.1:9");
    }
    let old_team = app
        .agent
        .delegation_team_directory()
        .map(std::path::Path::to_path_buf);
    let generations = app.executable_extensions.running_generations();
    let old_path = app.agent.session().path().to_owned();
    let old_id = session_id(app.agent.session());
    let old_bytes = std::fs::read(&old_path).unwrap();
    let mut shell = InteractiveShell::test_shell();
    let output = probe(&mut app, &mut shell).await;
    let new_path = app.agent.session().path().to_owned();
    let new_id = session_id(app.agent.session());
    let actual_generations = app.executable_extensions.running_generations();
    app.executable_extensions.shutdown().await;
    let trace = trace(&directory);
    let observed = format!("{output:?}\n{trace:#?}\n{}", shell.debug_snapshot());
    let output = output.unwrap_or_else(|error| panic!("{error:#}\n{observed}"));
    assert_ne!(new_id, old_id, "{observed}");
    if delegation {
        assert_ne!(app.agent.delegation_team_directory(), old_team.as_deref());
        assert_eq!(actual_generations, generations);
    }
    assert_eq!(step(&trace, "before")["id"], old_id, "{observed}");
    assert_eq!(step(&trace, "with")["id"], new_id, "{observed}");
    assert_eq!(step(&trace, "appended")["id"], new_id, "{observed}");
    assert_eq!(
        step(&trace, "after")["result"],
        serde_json::json!({"cancelled": false}),
        "{observed}"
    );
    // Notifications are painted by the live command pump, not necessarily
    // repeated in the command's returned text after its final drain.
    assert!(
        output.contains(&format!("fresh {new_id}"))
            || shell.debug_snapshot().contains(&format!("fresh {new_id}")),
        "{observed}"
    );
    let markers = entries_naming(&new_path, "probe-marker");
    assert_eq!(markers.len(), 1, "{markers:#?}\n{observed}");
    assert!(markers[0].to_string().contains(&new_id), "{markers:#?}");
    assert_eq!(std::fs::read(&old_path).unwrap(), old_bytes, "{observed}");
}

const SWITCH_SESSION: &str = r#"
import { appendFileSync } from 'node:fs';
const trace = value => appendFileSync(TRACE, JSON.stringify(value) + '\n');
export default pi => {
  pi.on('session_before_switch', (event, ctx) => {
    trace({ step: 'before-switch', event, id: ctx.sessionManager.getSessionId() });
  });
  pi.registerCommand('probe', { handler: async (targetPath, ctx) => {
    const before = ctx.sessionManager.getSessionId();
    trace({ step: 'before', id: before });
    const result = await ctx.switchSession(targetPath, { withSession: async fresh => {
      const id = fresh.sessionManager.getSessionId();
      trace({ step: 'with', id, file: fresh.sessionManager.getSessionFile() });
      pi.appendEntry('switch-marker', { id, before, targetPath });
      trace({ step: 'appended', id });
      fresh.ui.notify('switched ' + id);
    } });
    trace({ step: 'after', result });
  } });
};
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_switch_session_runs_with_session_on_the_saved_target_and_completes_command() {
    assert_switch_session(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_switch_session_with_delegation_keeps_extension_process() {
    assert_switch_session(true).await;
}

async fn assert_switch_session(delegation: bool) {
    let (directory, mut app) = pi_app(SWITCH_SESSION);
    if delegation {
        app = enable_test_delegation(app, "http://127.0.0.1:9");
    }
    let old_team = app
        .agent
        .delegation_team_directory()
        .map(std::path::Path::to_path_buf);
    let generations = app.executable_extensions.running_generations();
    let old_path = app.agent.session().path().to_owned();
    let old_id = session_id(app.agent.session());
    let old_bytes = std::fs::read(&old_path).unwrap();
    std::fs::create_dir_all(app.sessions.dir()).unwrap();
    let target_path = app.sessions.new_path("pi-switch-target");
    let mut saved = Session::create(&target_path).unwrap();
    saved
        .append_extension_entry(
            "octet-pi-compat",
            None,
            "saved-target",
            serde_json::json!({"saved": true}),
        )
        .unwrap();
    let target_id = session_id(&saved);
    // Release the real saved file's writer before the App resumes it.
    drop(saved);
    assert_eq!(app.sessions.path_by_id(&target_id).unwrap(), target_path);
    let mut shell = InteractiveShell::test_shell();
    let output = probe_with_arguments(
        &mut app,
        &mut shell,
        vec![target_path.to_str().unwrap().to_owned()],
    )
    .await;
    let actual_path = app.agent.session().path().to_owned();
    let actual_generations = app.executable_extensions.running_generations();
    app.executable_extensions.shutdown().await;
    let trace = trace(&directory);
    let observed = format!("{output:?}\n{trace:#?}\n{}", shell.debug_snapshot());
    // Successful completion of the original command RPC is required, not just
    // observing a session switch or an invocation of withSession.
    let output = output.unwrap_or_else(|error| panic!("{error:#}\n{observed}"));
    assert_eq!(actual_path, target_path, "{observed}");
    if delegation {
        assert_ne!(app.agent.delegation_team_directory(), old_team.as_deref());
        assert_eq!(actual_generations, generations);
    }
    assert_ne!(target_id, old_id, "{observed}");
    assert_eq!(step(&trace, "before")["id"], old_id, "{observed}");
    let before: Vec<_> = trace
        .iter()
        .filter(|event| event["step"] == "before-switch")
        .collect();
    assert_eq!(before.len(), 1, "{observed}");
    assert_eq!(before[0]["id"], old_id, "{observed}");
    assert_eq!(
        before[0]["event"],
        serde_json::json!({
            "type": "session_before_switch",
            "reason": "resume",
            "targetSessionFile": target_path,
        }),
        "{observed}"
    );
    assert_eq!(step(&trace, "with")["id"], target_id, "{observed}");
    assert_eq!(
        step(&trace, "with")["file"],
        serde_json::json!(target_path),
        "{observed}"
    );
    assert_eq!(step(&trace, "appended")["id"], target_id, "{observed}");
    assert_eq!(
        step(&trace, "after")["result"],
        serde_json::json!({"cancelled": false}),
        "{observed}"
    );
    assert!(
        output.contains(&format!("switched {target_id}"))
            || shell
                .debug_snapshot()
                .contains(&format!("switched {target_id}")),
        "{observed}"
    );
    assert_eq!(
        entries_naming(&target_path, "saved-target").len(),
        1,
        "{observed}"
    );
    let markers = entries_naming(&target_path, "switch-marker");
    assert_eq!(markers.len(), 1, "{markers:#?}\n{observed}");
    assert!(
        markers[0].to_string().contains(&target_id),
        "{markers:#?}\n{observed}"
    );
    assert_eq!(std::fs::read(old_path).unwrap(), old_bytes, "{observed}");
}

const SETUP_SESSION: &str = r#"
import { appendFileSync } from 'node:fs';
const trace = value => appendFileSync(TRACE, JSON.stringify(value) + '\n');
export default pi => {
  pi.on('session_shutdown', (_event, ctx) => trace({ step: 'end-options', id: ctx.sessionManager.getSessionId() }));
  pi.on('session_before_switch', (event, ctx) => trace({ step: 'before-options', event, id: ctx.sessionManager.getSessionId() }));
  pi.on('session_start', (_event, ctx) => trace({ step: 'start-options', id: ctx.sessionManager.getSessionId(), entries: ctx.sessionManager.getEntries() }));
  pi.registerCommand('probe', { handler: async (parentSession, ctx) => {
    const before = ctx.sessionManager.getSessionId();
    let captured;
    const result = await ctx.newSession({ parentSession, setup: async sm => {
      captured = sm;
      trace({ step: 'setup', id: sm.getSessionId(), file: sm.getSessionFile(), header: sm.getHeader(), leaf: sm.getLeafId() });
      const user = sm.appendMessage({ role: 'user', content: 'setup-user-body', timestamp: 123 });
      const custom = sm.appendCustomEntry('setup-private', { kept: true });
      const thinking = sm.appendThinkingLevelChange('high');
      const model = sm.appendModelChange('setup-provider', 'setup-model');
      const title = sm.appendSessionInfo('  Setup\n session  ');
      const label = sm.appendLabelChange(user, 'seed');
      if (sm.getEntry(user).message.content !== 'setup-user-body' || sm.getLabel(user) !== 'seed' || sm.getSessionName() !== 'Setup  session') throw new Error('setup did not read its actual writes');
      sm.branch(user);
      const child = sm.appendCustomEntry('setup-branch', { user });
      sm.resetLeaf();
      if (sm.getLeafId() !== null || sm.getBranch().length !== 0) throw new Error('real root checkout failed');
      sm.branch(child);
      const message = sm.appendCustomMessageEntry('setup-visible', 'setup-custom-body', false, { kept: true });
      await new Promise(resolve => setTimeout(resolve, 5));
      trace({ step: 'setup-written', ids: { user, custom, thinking, model, title, label, child, message }, entries: sm.getEntries(), branch: sm.getBranch(), leaf: sm.getLeafId() });
    }, withSession: fresh => {
      let oldRetired = false, setupClosed = false;
      try { ctx.sessionManager.getSessionId(); } catch { oldRetired = true; }
      try { captured.appendCustomEntry('late-setup', {}); } catch { setupClosed = true; }
      if (!oldRetired || !setupClosed) throw new Error('replacement authority escaped its lifetime');
      trace({ step: 'with-options', id: fresh.sessionManager.getSessionId(), header: fresh.sessionManager.getHeader(), branch: fresh.sessionManager.getBranch(), oldRetired, setupClosed });
      pi.appendEntry('setup-complete', { before, id: fresh.sessionManager.getSessionId() });
      fresh.ui.notify('setup ready ' + fresh.sessionManager.getSessionId());
    } });
    trace({ step: 'after-options', result });
  } });
};
"#;

// Pi awaits setup before finishing replacement (packages/coding-agent/src/core/
// agent-session-runtime.ts:226-260).
// The actual idle loop refreshes resources between lifecycle requests, unlike a
// command pump that keeps borrowing the App across the entire setup sequence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_setup_defers_resource_refresh_until_setup_completes() {
    struct RefreshingFrontend<'a> {
        shell: &'a mut InteractiveShell,
        input: futures_util::stream::Pending<std::io::Result<Event>>,
    }
    impl crate::extensions::ExtensionConfirmationHandler for RefreshingFrontend<'_> {
        fn command_shell(&mut self) -> Option<&mut InteractiveShell> {
            Some(self.shell)
        }
        fn confirm<'a>(
            &'a mut self,
            _: &'a str,
            _: &'a octet_agent::extension_process::ConfirmationRequest,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + 'a>> {
            Box::pin(std::future::ready(Ok(false)))
        }
    }
    impl InteractiveCommandFrontend for RefreshingFrontend<'_> {
        fn apply_lifecycle<'a>(
            &'a mut self,
            app: &'a mut App,
            request: ExtensionSessionLifecycleRequest,
        ) -> Pin<Box<dyn Future<Output = bool> + 'a>> {
            Box::pin(async move {
                let replaced =
                    execute_command_session_lifecycle(app, self.shell, &mut self.input, request)
                        .await;
                let diagnostics = resource_paths::refresh_resource_paths_reporting(
                    app,
                    self.shell,
                    &mut self.input,
                )
                .await
                .unwrap();
                assert!(
                    diagnostics.is_empty(),
                    "resource refresh interrupted setup: {diagnostics:?}"
                );
                replaced
            })
        }
    }
    let (directory, mut app) = pi_app(SETUP_SESSION);
    let parent = app.agent.session().path().to_str().unwrap().to_owned();
    let mut shell = InteractiveShell::test_shell();
    let mut input = futures_util::stream::pending();
    resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut input)
        .await
        .unwrap();
    let mut frontend = RefreshingFrontend {
        shell: &mut shell,
        input,
    };
    tokio::time::timeout(
        Duration::from_secs(20),
        run_interactive_extension_command(
            &mut app,
            &mut frontend,
            Some("octet-pi-compat"),
            "probe",
            vec![parent],
            false,
            0,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(step(&trace(&directory), "with-options")["oldRetired"]
        .as_bool()
        .unwrap());
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_new_session_parent_and_writable_setup_commit_to_the_real_replacement() {
    let (directory, mut app) = pi_app(SETUP_SESSION);
    let old_path = app.agent.session().path().to_owned();
    let old_id = session_id(app.agent.session());
    let old_bytes = std::fs::read(&old_path).unwrap();
    let mut shell = InteractiveShell::test_shell();
    let output = probe_with_arguments(
        &mut app,
        &mut shell,
        vec![old_path.to_str().unwrap().into()],
    )
    .await;
    let new_path = app.agent.session().path().to_owned();
    let new_id = session_id(app.agent.session());
    app.executable_extensions.shutdown().await;
    let trace = trace(&directory);
    let observed = format!("{output:?}\n{trace:#?}\n{}", shell.debug_snapshot());
    let output = output.unwrap_or_else(|error| panic!("{error:#}\n{observed}"));
    assert_ne!(new_id, old_id, "{observed}");
    assert_eq!(step(&trace, "before-options")["id"], old_id, "{observed}");
    assert_eq!(
        step(&trace, "before-options")["event"],
        serde_json::json!({"type":"session_before_switch","reason":"new"}),
        "{observed}"
    );
    assert_eq!(step(&trace, "end-options")["id"], old_id, "{observed}");
    let ended = trace
        .iter()
        .position(|event| event["step"] == "end-options")
        .unwrap();
    let setup_started = trace
        .iter()
        .position(|event| event["step"] == "setup")
        .unwrap();
    assert!(
        ended < setup_started,
        "old owner must retire before setup: {observed}"
    );
    let setup = step(&trace, "setup");
    assert_eq!(setup["id"], new_id, "{observed}");
    assert_eq!(setup["file"], serde_json::json!(new_path), "{observed}");
    assert!(setup["leaf"].is_null(), "{observed}");
    let saved = Session::open_read_only(&new_path).unwrap();
    let header = saved.header().expect("a real durable creation header");
    assert_eq!(header.id, new_id);
    assert_eq!(header.cwd, app.config.workspace);
    assert_eq!(header.parent_session.as_deref(), old_path.to_str());
    assert_eq!(setup["header"]["id"], header.id);
    assert_eq!(
        setup["header"]["parentSession"],
        serde_json::json!(old_path)
    );
    assert_eq!(
        step(&trace, "with-options")["header"],
        setup["header"],
        "{observed}"
    );
    assert_eq!(
        step(&trace, "after-options")["result"],
        serde_json::json!({"cancelled":false}),
        "{observed}"
    );
    let written = step(&trace, "setup-written");
    let ids = written["ids"].as_object().unwrap();
    assert_eq!(
        written["entries"].as_array().unwrap().len(),
        ids.len(),
        "{observed}"
    );
    for id in ids.values() {
        assert!(
            saved
                .entry(&octet_agent::EntryId(id.as_str().unwrap().into()))
                .is_some(),
            "{observed}"
        );
    }
    let user = octet_agent::EntryId(ids["user"].as_str().unwrap().into());
    assert_eq!(saved.entry_label(&user), Some("seed"));
    assert_eq!(saved.entries().len(), ids.len() + 1, "{observed}");
    let branch_ids: Vec<_> = written["branch"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].clone())
        .collect();
    assert_eq!(
        branch_ids,
        vec![
            ids["user"].clone(),
            ids["child"].clone(),
            ids["message"].clone()
        ],
        "{observed}"
    );
    assert_eq!(
        step(&trace, "with-options")["branch"],
        written["branch"],
        "{observed}"
    );
    let start = trace
        .iter()
        .position(|entry| entry["step"] == "start-options" && entry["id"] == new_id)
        .unwrap();
    let end_setup = trace
        .iter()
        .position(|entry| entry["step"] == "setup-written")
        .unwrap();
    let with = trace
        .iter()
        .position(|entry| entry["step"] == "with-options")
        .unwrap();
    assert!(end_setup < start && start < with, "{observed}");
    assert_eq!(trace[start]["entries"], written["entries"], "{observed}");
    let context = serde_json::to_value(saved.context().unwrap())
        .unwrap()
        .to_string();
    assert!(
        context.contains("setup-user-body") && context.contains("setup-custom-body"),
        "{context}"
    );
    assert!(
        !context.contains("setup-private")
            && !context.contains("kept")
            && !context.contains("setup-complete"),
        "{context}"
    );
    assert_eq!(
        entries_naming(&new_path, "setup-complete").len(),
        1,
        "{observed}"
    );
    assert_eq!(
        entries_naming(&new_path, "late-setup").len(),
        0,
        "{observed}"
    );
    assert!(
        output.contains(&format!("setup ready {new_id}"))
            || shell
                .debug_snapshot()
                .contains(&format!("setup ready {new_id}")),
        "{observed}"
    );
    assert_eq!(std::fs::read(old_path).unwrap(), old_bytes, "{observed}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_before_switch_cancels_parent_and_setup_before_new_file_creation() {
    let factory = CANCEL_NEW.replace("ctx.newSession({ withSession:", "ctx.newSession({ parentSession: '/not-created.jsonl', setup: () => { throw new Error('cancelled setup ran'); }, withSession:");
    let (directory, mut app) = pi_app(&factory);
    let old_path = app.agent.session().path().to_owned();
    let old_bytes = std::fs::read(&old_path).unwrap();
    let files = || {
        std::fs::read_dir(app.sessions.dir())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let before_files = files();
    let mut shell = InteractiveShell::test_shell();
    probe(&mut app, &mut shell).await.unwrap();
    let after_files = std::fs::read_dir(app.sessions.dir())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<std::collections::BTreeSet<_>>();
    app.executable_extensions.shutdown().await;
    let trace = trace(&directory);
    assert_eq!(
        step(&trace, "after")["result"],
        serde_json::json!({"cancelled":true}),
        "{trace:#?}"
    );
    assert_eq!(app.agent.session().path(), old_path);
    assert_eq!(std::fs::read(old_path).unwrap(), old_bytes);
    assert_eq!(after_files, before_files);
}

const CANCEL_NEW: &str = r#"
import { appendFileSync } from 'node:fs';
const trace = value => appendFileSync(TRACE, JSON.stringify(value) + '\n');
export default pi => {
  pi.on('session_before_switch', (event, ctx) => {
    trace({ step: 'cancel', event, id: ctx.sessionManager.getSessionId() });
    return { cancel: true };
  });
  pi.registerCommand('probe', { handler: async (_args, ctx) => {
    const result = await ctx.newSession({ withSession: () => { throw new Error('cancelled withSession ran'); } });
    trace({ step: 'after', result, id: ctx.sessionManager.getSessionId() });
  } });
};
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_before_switch_cancels_command_and_user_new_without_mutation() {
    let (directory, mut app) = pi_app(CANCEL_NEW);
    let path = app.agent.session().path().to_owned();
    let bytes = std::fs::read(&path).unwrap();
    let id = session_id(app.agent.session());
    let mut shell = InteractiveShell::test_shell();
    probe(&mut app, &mut shell).await.unwrap();
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    app = transition(app, &mut shell, &mut input, Reconfig::NewSession)
        .await
        .unwrap();
    assert_eq!(app.agent.session().path(), path);
    app.executable_extensions.shutdown().await;
    let trace = trace(&directory);
    assert_eq!(
        step(&trace, "after")["result"],
        serde_json::json!({"cancelled":true}),
        "{trace:#?}"
    );
    assert_eq!(step(&trace, "after")["id"], id);
    let before: Vec<_> = trace
        .iter()
        .filter(|event| event["step"] == "cancel")
        .collect();
    assert_eq!(before.len(), 2, "{trace:#?}");
    for event in before {
        assert_eq!(
            event["event"],
            serde_json::json!({"type":"session_before_switch","reason":"new"})
        );
        assert_eq!(event["id"], id);
    }
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_before_fork_cancels_before_entry_lookup() {
    let factory = CANCEL_NEW
        .replace("session_before_switch", "session_before_fork")
        .replace(
            "ctx.newSession({ withSession:",
            "ctx.fork('not-reached', { position: 'at', withSession:",
        );
    let (directory, mut app) = pi_app(&factory);
    let path = app.agent.session().path().to_owned();
    let bytes = std::fs::read(&path).unwrap();
    let mut shell = InteractiveShell::test_shell();
    probe(&mut app, &mut shell).await.unwrap();
    assert_eq!(app.agent.session().path(), path);
    app.executable_extensions.shutdown().await;
    let trace = trace(&directory);
    assert_eq!(
        step(&trace, "after")["result"],
        serde_json::json!({"cancelled":true}),
        "{trace:#?}"
    );
    assert_eq!(
        step(&trace, "cancel")["event"],
        serde_json::json!({"type":"session_before_fork","entryId":"not-reached","position":"at"})
    );
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_new_session_requested_while_busy_waits_for_settlement_before_setup() {
    use super::support::{scripted_model, text_turn, HeldApi};
    let factory = SETUP_SESSION
        .replace("const result = await ctx.newSession", "const replacement = ctx.newSession")
        .replace("trace({ step: 'after-options', result });", "trace({ step: 'requested-options', id: before }); const result = await replacement; trace({ step: 'after-options', result });");
    let (directory, mut app) = pi_app(&factory);
    let (server, started, release) = HeldApi::start(text_turn()).await;
    let model = scripted_model(&server.uri);
    app.catalog
        .register_endpoint((*model.endpoint).clone())
        .unwrap();
    app.catalog.register_model((*model.spec).clone()).unwrap();
    app = rebuild_app(app, Some(model), None, None, None).unwrap();
    app.executable_extensions
        .activate_session_lifecycle_driver();
    let mut shell = InteractiveShell::test_shell();
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut input)
        .await
        .unwrap();
    let old_path = app.agent.session().path().to_owned();
    let old_id = session_id(app.agent.session());
    let inspection = ActiveRunInspection::capture(&app);
    let run_id = shell.begin_run("test");
    let mut run = app
        .agent
        .prompt("finish in the original session")
        .await
        .unwrap();
    shell.set_awaiting_provider(run_id);
    // Reach an actual held provider response, not a fabricated busy shell flag.
    tokio::pin!(started);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            tokio::select! {
                result = &mut started => { result.unwrap(); break; }
                event = run.next() => shell.on_run_event(run_id, &event.expect("run must remain busy")),
            }
        }
    }).await.unwrap();
    let old_busy_bytes = std::fs::read(&old_path).unwrap();
    // Submit through the real composer while the response owns the Agent.
    // Only the session replacement, not its command handler, may wait for idle.
    let text = format!("/probe {}", old_path.display());
    let events = text
        .chars()
        .map(|ch| {
            Ok(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char(ch),
                KeyModifiers::NONE,
            )))
        })
        .chain(std::iter::once(Ok(Event::Key(
            crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        ))));
    let mut input = futures_util::stream::iter(events).chain(futures_util::stream::pending());
    let mut pending = VecDeque::new();
    let control = run.control();
    let mut quit = false;
    let mut made_tool_call = false;
    let mut deadline = None;
    let requested = async {
        loop {
            if trace(&directory)
                .iter()
                .any(|event| event["step"] == "requested-options")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            result = drive_active_run(&mut run, &control, &mut shell, &mut input,
                &mut pending, &mut quit, None, None, &mut app.executable_extensions,
                &mut made_tool_call, &inspection, &mut deadline) => panic!("held response settled: {result:?}"),
            _ = requested => {}
        }
    })
    .await
    .unwrap();
    assert!(pending.is_empty(), "command must not wait for idle");
    assert!(shell.is_agent_run_active());
    let busy_trace = trace(&directory);
    assert_eq!(step(&busy_trace, "requested-options")["id"], old_id);
    assert!(
        !busy_trace
            .iter()
            .any(|event| event["step"] == "setup" || event["step"] == "before-options"),
        "{busy_trace:#?}"
    );
    assert_eq!(std::fs::read(&old_path).unwrap(), old_busy_bytes);
    release.send(true).unwrap();
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
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
    assert_eq!(outcome, HostRunOutcome::Completed);
    drop(run); // Only now may the idle lifecycle owner replace the writer.
    assert!(!shell.is_agent_run_active());
    let settled_bytes = std::fs::read(&old_path).unwrap();
    assert_ne!(settled_bytes, old_busy_bytes);
    let (_, mut command) = app.executable_extensions.active_commands.remove(0);
    let dialogs = app.executable_extensions.lifecycle_snapshot();
    let output = tokio::time::timeout(Duration::from_secs(20), async {
        let mut frontend = InteractiveExtensionConfirmations {
            shell: &mut shell,
            input: &mut input,
            dialogs: &dialogs,
            input_retired: false,
        };
        let failure = loop {
            match command
                .advance(
                    &mut app.executable_extensions,
                    &mut frontend,
                    true,
                    Some((&mut app.agent, &app.sessions)),
                )
                .await
            {
                Ok(Some(request)) => {
                    if command
                        .during_lifecycle(frontend.apply_lifecycle(&mut app, request))
                        .await
                    {
                        command.session_replaced();
                    }
                }
                Ok(None) => break None,
                Err(error) => break Some(error),
            }
        };
        command
            .finish(&mut app.executable_extensions, &mut frontend, failure)
            .await
    })
    .await
    .unwrap();
    let new_path = app.agent.session().path().to_owned();
    let new_id = session_id(app.agent.session());
    app.executable_extensions.shutdown().await;
    let observed = trace(&directory);
    output.unwrap_or_else(|error| {
        panic!(
            "{error:#}\n{observed:#?}\ndelegation={:?}\n{}",
            app.agent.delegation_team_directory(),
            shell.debug_snapshot()
        )
    });
    assert_ne!(new_id, old_id);
    assert_eq!(step(&observed, "setup")["id"], new_id);
    assert_eq!(step(&observed, "with-options")["id"], new_id);
    assert_eq!(
        step(&observed, "after-options")["result"],
        serde_json::json!({"cancelled": false})
    );
    assert_eq!(
        app.agent
            .session()
            .header()
            .unwrap()
            .parent_session
            .as_deref(),
        old_path.to_str()
    );
    assert_eq!(entries_naming(&new_path, "setup-complete").len(), 1);
    assert_eq!(std::fs::read(&old_path).unwrap(), settled_bytes);
    assert!(!serde_json::to_string(app.agent.session().entries())
        .unwrap()
        .contains("finish in the original session"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_reload_after_pending_request_preserves_start_discovery_and_next_turn() {
    assert_reload_after_pending_request(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_resource_rebuild_after_pending_request_preserves_start_discovery_and_next_turn() {
    assert_reload_after_pending_request(true).await;
}

async fn assert_reload_after_pending_request(rebuild: bool) {
    use super::pi_contract_support::pi_ui_app;
    let factory = r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  const record = row => appendFileSync(TRACE, JSON.stringify(row) + '\n');
  pi.on('session_start', (event, ctx) => {
    pi.appendEntry('reload-ready', { reason: event.reason });
    ctx.ui.setStatus('reload', 'ready');
    record({ step: 'start', reason: event.reason, pid: process.pid });
  });
  pi.on('resources_discover', event => { record({ step: 'discover', reason: event.reason }); return {}; });
  pi.on('turn_start', () => record({ step: 'turn' }));
  pi.on('context', () => {});
  pi.registerCommand('pending', { handler: async (_, ctx) => { await ctx.ui.setEditorText('retired-draft'); } });
};
"#;
    let (directory, mut app, mut shell) = pi_ui_app(factory);
    let provider = octet_ai::FauxProvider::new(octet_ai::FauxOptions::default());
    provider.set_responses(vec![
        octet_ai::FauxResponse::Message(octet_ai::FauxMessage::new("before reload")),
        octet_ai::FauxResponse::Message(octet_ai::FauxMessage::new("after reload")),
    ]);
    app.client
        .register_host_stream_transport(app.model.endpoint.id.clone(), Arc::new(provider));
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut input)
        .await
        .unwrap();
    assert_eq!(
        app.agent.complete("before").await.unwrap().text,
        "before reload"
    );
    let process = app
        .executable_extensions
        .test_process("octet-pi-compat")
        .unwrap();
    request_extension_ui(&mut shell, &mut app);
    let command_process = process.clone();
    let context =
        process.current_context_for_resource_owner(app.agent.session().resource_owner_key());
    let mut command = tokio::spawn(async move {
        command_process
            .execute_command("pending", Vec::new(), context)
            .await
    });
    // Hold an admitted reverse request across cutover, without lending the shell.
    tokio::time::timeout(Duration::from_secs(5), async {
        while app.executable_extensions.pending_host_request_count() == 0 {
            app.executable_extensions.drain_events();
            if command.is_finished() {
                panic!("command settled: {:?}", (&mut command).await);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    if rebuild {
        let (next, applied) = tokio::time::timeout(
            Duration::from_secs(20),
            reload_resources_announcing(app, &mut shell, &mut input, true),
        )
        .await
        .unwrap()
        .unwrap();
        app = next;
        assert!(applied);
    } else {
        let reloading = process.clone();
        let reload = tokio::spawn(async move { reloading.reload().await });
        tokio::time::timeout(Duration::from_secs(10), async {
            while process.health_snapshot().generation == 1 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let report = tokio::time::timeout(Duration::from_secs(20), async {
            while !reload.is_finished() {
                request_extension_ui(&mut shell, &mut app);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            reload.await.unwrap().unwrap()
        })
        .await
        .unwrap();
        assert_eq!(report.generation, 2);
    }
    let _ = command.await.unwrap();
    let process = app
        .executable_extensions
        .test_process("octet-pi-compat")
        .unwrap();
    let owner = app.agent.session().resource_owner_key();
    let discovery = process
        .discover_resource_paths(
            &owner,
            octet_agent::extension_process::ExtensionResourceDiscoveryReason::Reload,
        )
        .await;
    let visible = shell.debug_snapshot();
    assert_eq!(
        app.agent.complete("after").await.unwrap().text,
        "after reload"
    );
    app.executable_extensions.shutdown().await;
    assert!(discovery.is_ok(), "{discovery:?}\n{visible}");
    assert!(!visible.contains("stale generation"), "{visible}");
    assert!(!visible.contains("reload failed"), "{visible}");
    assert_eq!(
        trace(&directory)
            .iter()
            .filter(|row| row["step"] == "start")
            .count(),
        2
    );
}
