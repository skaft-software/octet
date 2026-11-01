#![cfg(unix)]
use super::*;
use crate::session_store::SessionStore;
use serde_json::Value;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn modeless_extension_host_has_real_session_and_no_model_authority() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let config = terminal_theme_test_config(workspace.clone());
    let sessions = SessionStore::for_directory(&workspace, &workspace);
    let mut session = Session::create(workspace.join("no-provider.jsonl")).unwrap();
    let original = std::fs::read(session.path()).unwrap();
    let mut shell = InteractiveShell::test_shell();
    let mut state = tokio::task::block_in_place(|| {
        modeless::ModelessExtensions::new(&config, &session, &sessions, "startup", Some(&shell))
    })
    .unwrap();
    state.pump(&mut shell, &mut session, &sessions);
    assert!(state.extensions.is_modeless());
    assert!(state.host.pi_tool_snapshot()["active_tools"].is_array());
    assert_eq!(std::fs::read(session.path()).unwrap(), original);
    assert!(config.model.is_none());
    assert!(config.reasoning.is_none());
    state.extensions.shutdown().await;
}

const FACTORY: &str = r#"
import { appendFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';
export default pi => {
  const record = value => appendFileSync(TRACE, JSON.stringify(value) + '\n');
  pi.on('session_start', async (_, ctx) => {
    record({start: true, noModel: ctx.model === undefined, noUsage: ctx.getContextUsage() === undefined,
      sessionId: ctx.sessionId, available: ctx.modelRegistry.getAvailable().length, idle: ctx.isIdle(), hasUI: ctx.hasUI});
    await ctx.ui.setFooter(() => ({render: () => ['NO-PROVIDER-FOOTER'], invalidate() {}}));
  });
  pi.on('resources_discover', (_, ctx) => existsSync(join(ctx.cwd, 'resources-disabled')) ? {} : ({
    skillPaths: [join(ctx.cwd, 'resources/skills')],
    promptPaths: [join(ctx.cwd, 'resources/prompts')],
  }));
  pi.on('session_shutdown', () => record({end: true}));
  pi.registerCommand('modeless-probe', {handler: async (_, ctx) => {
    pi.appendEntry('no-provider-probe', {realSession: true});
    const entry = ctx.sessionManager.getEntries().find(value => value.type === 'custom' && value.customType === 'no-provider-probe');
    await pi.setLabel(entry.id, 'no-provider-label');
    await pi.setSessionName('No provider session');
    await ctx.ui.setEditorText('no-provider-draft');
    record({command: true, noModel: ctx.model === undefined, commands: pi.getCommands(),
      entryId: entry.id, name: pi.getSessionName()});
  }});
  pi.registerCommand('modeless-refusals', {handler: async (_, ctx) => {
    for (const [api, invoke] of [
      ['systemPrompt', () => ctx.getSystemPrompt()],
      ['customMessage', () => pi.sendMessage({customType: 'no-provider-custom', content: 'MUST-NOT-PERSIST', display: false}, {triggerTurn: true})],
      ['setModel', () => pi.setModel({provider: 'absent', id: 'absent'})],
      ['setThinking', () => pi.setThinkingLevel('high')],
    ]) {
      try { await invoke(); record({api, unexpectedSuccess: true}); }
      catch (error) { record({api, refused: String(error.message)}); }
    }
  }});
  pi.registerCommand('modeless-send', {handler: async () => {
    await pi.sendUserMessage('MUST-NOT-QUEUE');
    record({injected: true});
  }});
};
"#;

fn rows(root: &Path) -> Vec<Value> {
    std::fs::read_to_string(root.join("trace.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

async fn fixture(
    policy: octet_agent::EffectPolicy,
    enabled: bool,
    granted: bool,
) -> (
    tempfile::TempDir,
    modeless::ModelessExtensions,
    Session,
    SessionStore,
    InteractiveShell,
    Config,
) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    for (path, contents) in [
        ("resources/skills/no-provider-skill/SKILL.md", "---\nname: no-provider-skill\ndescription: NO-PROVIDER-SKILL\n---\nNative model-less skill.\n"),
        ("resources/prompts/no-provider-prompt.md", "Native model-less prompt $1\n"),
    ] {
        let path = workspace.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    let entry = workspace.join("probe.ts");
    std::fs::write(
        &entry,
        FACTORY.replace(
            "TRACE",
            &serde_json::to_string(&workspace.join("trace.jsonl")).unwrap(),
        ),
    )
    .unwrap();
    let adapter = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../extensions/octet-pi-compat")
        .canonicalize()
        .unwrap();
    let extensions = workspace.join(".octet/extensions");
    let output = std::process::Command::new("node")
        .arg(adapter.join("configure.mjs"))
        .arg("--reviewed")
        .arg("--output")
        .arg(extensions.join("octet-pi-compat"))
        .arg(&entry)
        .current_dir(&workspace)
        .env("HOME", &workspace)
        .env("XDG_CONFIG_HOME", workspace.join("config"))
        .env("PI_CODING_AGENT_DIR", workspace.join("pi-agent"))
        .env("OCTET_PI_AGENT_DIR", workspace.join("pi-agent"))
        .env("PI_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest_path = extensions.join("octet-pi-compat/extension.toml");
    let mut manifest =
        octet_agent::ExtensionManifest::parse(&std::fs::read_to_string(&manifest_path).unwrap())
            .unwrap();
    assert!(!manifest.capabilities.provider_credentials);
    for (name, path) in [
        ("HOME", workspace.join("home")),
        ("XDG_CONFIG_HOME", workspace.join("config")),
        ("XDG_CACHE_HOME", workspace.join("cache")),
        ("PI_CODING_AGENT_DIR", workspace.join("pi-agent")),
        ("OCTET_PI_AGENT_DIR", workspace.join("pi-agent")),
        ("OCTET_PI_COMPAT_CACHE", workspace.join("adapter-cache")),
    ] {
        manifest
            .entrypoint
            .env
            .insert(name.into(), path.to_string_lossy().into());
    }
    manifest
        .entrypoint
        .env
        .insert("PI_OFFLINE".into(), "1".into());
    std::fs::write(manifest_path, toml::to_string(&manifest).unwrap()).unwrap();
    let mut config = terminal_theme_test_config(workspace.clone());
    config.effect_policy = policy;
    config.extension_paths = Vec::new();
    config.enabled_extensions = if enabled {
        vec!["octet-pi-compat".into()]
    } else {
        Vec::new()
    };
    config.invocation_trusted_extensions = if granted {
        vec!["octet-pi-compat".into()]
    } else {
        Vec::new()
    };
    config.sandbox.allow_process = true;
    config.sandbox.allow_shell = true;
    let sessions = SessionStore::for_directory(&workspace, &workspace);
    let session = Session::create(workspace.join("no-provider.jsonl")).unwrap();
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(100, 30);
    shell.finish_startup();
    let mut state = tokio::task::block_in_place(|| {
        modeless::ModelessExtensions::new(&config, &session, &sessions, "startup", Some(&shell))
    })
    .unwrap();
    state
        .extensions
        .refresh_pi_model_catalog_without_selection(&octet_ai::ModelCatalog::default(), None);
    tokio::time::timeout(Duration::from_secs(10), async {
        while state.extensions.startup_pending() {
            state.attach(&mut shell, &session).await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("model-less extension handshake must settle");
    (root, state, session, sessions, shell, config)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn modeless_pi_startup_footer_command_append_and_prompt_refusal() {
    let (root, mut state, mut session, sessions, mut shell, config) =
        fixture(octet_agent::EffectPolicy::UnsafeHost, true, true).await;
    assert_eq!(
        state.extensions.attached_count(),
        1,
        "{}",
        state.extensions.inspect_text()
    );
    assert!(state.extensions.remote_ui_wake().is_some());
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    tokio::time::timeout(
        Duration::from_secs(15),
        state.refresh_resources(&mut shell, &mut input, &mut session, &sessions, &config),
    )
    .await
    .unwrap()
    .unwrap();
    let starts = rows(root.path());
    assert_eq!(starts.len(), 1, "{starts:?}");
    assert_eq!(starts[0]["noModel"], true);
    assert_eq!(starts[0]["noUsage"], true);
    assert_eq!(starts[0]["hasUI"], true);
    assert_eq!(starts[0]["idle"], true);
    assert_eq!(starts[0]["available"], 0);
    assert_eq!(starts[0]["sessionId"], "no-provider");
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            state.pump(&mut shell, &mut session, &sessions);
            shell.render();
            let frame = shell.dump_rendered_frame().await.unwrap().join("\n");
            if sexy_tui_rs::strip_terminal_sequences(&frame).contains("NO-PROVIDER-FOOTER") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the native shell must paint the no-provider footer");
    tokio::time::timeout(
        Duration::from_secs(10),
        state.command(
            &mut shell,
            &mut input,
            &mut session,
            &sessions,
            "/modeless-probe",
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(rows(root.path())
        .iter()
        .any(|row| row["command"] == true && row["noModel"] == true));
    assert_eq!(shell.extension_editor_snapshot().text, "no-provider-draft");
    let durable = std::fs::read_to_string(session.path()).unwrap();
    assert!(durable.contains("no-provider-probe"), "{durable}");
    assert!(durable.contains("no-provider-label"), "{durable}");
    assert_eq!(
        sessions
            .load_metadata("no-provider")
            .unwrap()
            .name
            .as_deref(),
        Some("No provider session")
    );
    let command = rows(root.path())
        .into_iter()
        .find(|row| row["command"] == true)
        .unwrap();
    let reopened = Session::open_read_only(session.path()).unwrap();
    let entry_id = octet_agent::EntryId(command["entryId"].as_str().unwrap().into());
    assert_eq!(reopened.entry_label(&entry_id), Some("no-provider-label"));
    let stored = reopened
        .extension_entry(&entry_id, "octet-pi-compat")
        .unwrap();
    assert_eq!(stored.entry_type, "no-provider-probe");
    assert_eq!(stored.data, serde_json::json!({"realSession": true}));
    assert_eq!(command["name"], "No provider session");
    assert!(
        command["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value["name"] == "no-provider-prompt" && value["source"] == "prompt"),
        "{command}"
    );
    assert!(
        command["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value["source"] == "skill" && value["description"] == "NO-PROVIDER-SKILL"),
        "{command}"
    );
    let before = durable;
    tokio::time::timeout(
        Duration::from_secs(10),
        state.command(
            &mut shell,
            &mut input,
            &mut session,
            &sessions,
            "/modeless-send",
        ),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        state.command(
            &mut shell,
            &mut input,
            &mut session,
            &sessions,
            "/modeless-refusals",
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let refusals = rows(root.path())
        .into_iter()
        .filter(|row| row["api"].is_string())
        .collect::<Vec<_>>();
    assert_eq!(refusals.len(), 4, "{refusals:?}");
    for refusal in &refusals {
        assert!(
            refusal["refused"]
                .as_str()
                .is_some_and(|message| !message.is_empty()),
            "{refusal}"
        );
        let expected = if matches!(
            refusal["api"].as_str(),
            Some("systemPrompt" | "customMessage")
        ) {
            "no configured model"
        } else {
            "session_control_v1"
        };
        assert!(
            refusal["refused"].as_str().unwrap().contains(expected),
            "{refusal}"
        );
        assert!(refusal.get("unexpectedSuccess").is_none(), "{refusal}");
    }
    assert!(!rows(root.path()).iter().any(|row| row["injected"] == true));
    assert!(shell.take_extension_messages().is_empty());
    assert_eq!(std::fs::read_to_string(session.path()).unwrap(), before);
    assert!(config.model.is_none());
    state.extensions.shutdown().await;
    assert_eq!(
        rows(root.path())
            .iter()
            .filter(|row| row["end"] == true)
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn modeless_pi_resource_reload_withdraws_native_catalog() {
    let (root, mut state, mut session, sessions, mut shell, config) =
        fixture(octet_agent::EffectPolicy::UnsafeHost, true, true).await;
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    tokio::time::timeout(
        Duration::from_secs(15),
        state.refresh_resources(&mut shell, &mut input, &mut session, &sessions, &config),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(state
        .config
        .prompt_paths
        .iter()
        .any(|path| path.ends_with("resources/prompts")));
    std::fs::write(root.path().join("resources-disabled"), "1").unwrap();
    let report = tokio::time::timeout(
        Duration::from_secs(15),
        state.reload(&mut shell, &mut input, &mut session, &sessions, &config),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        report
            .processes
            .iter()
            .any(|(name, result)| name == "octet-pi-compat" && result.is_ok()),
        "{report:?}"
    );
    assert_eq!(state.config.prompt_paths, config.prompt_paths);
    assert_eq!(state.config.skill_paths, config.skill_paths);
    tokio::time::timeout(
        Duration::from_secs(10),
        state.command(
            &mut shell,
            &mut input,
            &mut session,
            &sessions,
            "/modeless-probe",
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let command = rows(root.path())
        .into_iter()
        .find(|row| row["command"] == true)
        .unwrap();
    assert!(
        !command["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value["name"] == "no-provider-prompt"
                || value["description"] == "NO-PROVIDER-SKILL"),
        "{command}"
    );
    assert!(state.extensions.is_modeless());
    assert!(state.config.model.is_none());
    state.extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn modeless_pi_startup_preserves_activation_and_source_authority() {
    for (policy, enabled, granted, expected) in [
        (octet_agent::EffectPolicy::UnsafeHost, false, true, false),
        (octet_agent::EffectPolicy::Controlled, true, false, false),
        (octet_agent::EffectPolicy::Controlled, true, true, true),
    ] {
        let (root, mut state, mut session, sessions, mut shell, config) =
            fixture(policy, enabled, granted).await;
        assert_eq!(
            state.extensions.attached_count() != 0,
            expected,
            "{}",
            state.extensions.inspect_text()
        );
        if expected {
            let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
            state
                .refresh_resources(&mut shell, &mut input, &mut session, &sessions, &config)
                .await
                .unwrap();
            assert!(rows(root.path()).iter().any(|row| row["start"] == true));
        } else {
            assert!(rows(root.path()).is_empty());
        }
        state.extensions.shutdown().await;
    }
}
