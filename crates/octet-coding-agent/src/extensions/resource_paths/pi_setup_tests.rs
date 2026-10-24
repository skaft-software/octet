//! Fixture Pi home -> reviewed mirror configuration -> real adapter -> real App.
//!
//! The acceptance path is the product's own: an explicit `configure.mjs --reviewed
//! --from-pi --mirror` records the mirror opt-in, then the ordinary native host
//! starts the generated `octet-pi-compat` extension. Nothing here hand-writes a
//! bridge, replaces the adapter, or fabricates a resource snapshot.
use crate::app::App;
use std::path::{Path, PathBuf};
use std::process::Command;

struct PiHome {
    home: PathBuf,
    agent: PathBuf,
}

fn put(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) {
    let path = path.as_ref();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// One fixture Pi 1.0.2 setup: settings, two extensions, a theme, a keybinding,
/// a skill, a prompt template and a selected model. Every item is read by the
/// mirror and must be untouched afterwards.
fn fixture_pi_home(root: &Path) -> PiHome {
    let home = root.join("home");
    let agent = home.join(".pi/agent");
    let extensions = agent.join("extensions");
    put(
        extensions.join("pi-setup-one.js"),
        r#"export default function (pi) {
  pi.registerCommand('pi-setup-one', { description: 'Pi setup one', handler() {} });
  // A Pi setup's custom provider, declared exactly as a Pi extension declares one.
  pi.registerProvider('pi-mirror-provider', {
    baseUrl: 'http://127.0.0.1:9/pi-mirror/',
    api: 'openai-completions',
    apiKey: 'pi-mirror-local-key',
    models: [{ id: 'pi-mirror-model', name: 'Pi Mirror Model', reasoning: false, input: ['text'],
      cost: { input: 1, output: 2, cacheRead: 0.5, cacheWrite: 0.5 }, contextWindow: 8192, maxTokens: 1024 }],
    streamSimple: () => (async function* () {})(),
  });
}
"#,
    );
    put(
        extensions.join("pi-setup-two.js"),
        "export default function (pi) {\n  pi.registerCommand('pi-setup-two', { description: 'Pi setup two', handler() {} });\n}\n",
    );
    put(
        agent.join("themes/pi-setup-theme.json"),
        serde_json::to_vec_pretty(&crate::extensions::resource_paths::pi_theme::fixture(
            "Pi Setup Theme",
            "#00ff88",
        ))
        .unwrap(),
    );
    put(
        agent.join("skills/pi-setup-skill/SKILL.md"),
        "---\nname: pi-setup-skill\ndescription: PI-SETUP-SKILL-CATALOG\n---\nPI-SETUP-SKILL-BODY\n",
    );
    put(
        agent.join("prompts/pi-setup-prompt.md"),
        "PI-SETUP-PROMPT $1\n",
    );
    put(
        agent.join("keybindings.json"),
        "{\"app.exit\": [\"ctrl+q\"]}\n",
    );
    put(agent.join("AGENTS.md"), "PI-SETUP-CONTEXT\n");
    put(
        agent.join("settings.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "defaultProvider": "pi-mirror-provider",
            "defaultModel": "pi-mirror-model",
            "theme": "pi-setup-theme",
        }))
        .unwrap(),
    );
    PiHome { home, agent }
}

/// Every regular file under the fixture home, path and bytes, so a mirror pass
/// that wrote anything is a visible failure.
fn tree_contents(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(root: &Path, current: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
        let mut entries = std::fs::read_dir(current)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect::<Vec<_>>();
        entries.sort_by_key(std::fs::DirEntry::path);
        for entry in entries {
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                walk(root, &path, files);
            } else {
                files.push((
                    path.strip_prefix(root).unwrap().to_owned(),
                    std::fs::read(&path).unwrap(),
                ));
            }
        }
    }
    let mut files = Vec::new();
    walk(root, root, &mut files);
    files
}

/// The adapter keeps its transform caches in a `.cache` directory of the bundle
/// it runs from (see lib/startup-cache.mjs); that is its own state, not part of
/// what an install ships.
fn without_adapter_cache(files: Vec<(PathBuf, Vec<u8>)>) -> Vec<(PathBuf, Vec<u8>)> {
    files
        .into_iter()
        .filter(|(path, _)| !path.components().any(|part| part.as_os_str() == ".cache"))
        .collect()
}

fn configure_mirror(root: &Path, pi: &PiHome, workspace: &Path) -> PathBuf {
    let adapter = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/octet-pi-compat");
    let adapter = adapter.canonicalize().expect("real adapter package");
    let output = root.join("extensions/octet-pi-compat");
    // Exercise the nonexecuting review boundary before admitting fixture factories.
    let before_prepare = tree_contents(&pi.home);
    let prepared = Command::new("node")
        .arg(adapter.join("configure.mjs"))
        .args(["--prepare-mirror", "--json", "--output"])
        .arg(&output)
        .current_dir(workspace)
        .env("OCTET_PI_AGENT_DIR", &pi.agent)
        .output()
        .expect("existing Node and real adapter preparation required");
    assert!(
        prepared.status.success(),
        "mirror preparation failed: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    assert_eq!(tree_contents(&pi.home), before_prepare);
    let review: serde_json::Value = serde_json::from_slice(&prepared.stdout).unwrap();
    assert_eq!(review["extensions"].as_array().unwrap().len(), 2);
    let fingerprint = review["fingerprint"].as_str().unwrap();
    let configured = Command::new("node")
        .arg(adapter.join("configure.mjs"))
        .arg("--reviewed")
        .arg("--from-pi")
        .arg("--mirror")
        .arg("--expected-fingerprint")
        .arg(fingerprint)
        .arg("--output")
        .arg(&output)
        .current_dir(workspace)
        .env("OCTET_PI_AGENT_DIR", &pi.agent)
        .output()
        .expect("existing Node 22.19+ and local adapter dependencies required; never auto-install");
    assert!(
        configured.status.success(),
        "mirror configure failed: {}",
        String::from_utf8_lossy(&configured.stderr)
    );
    let bridge: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output.join("bridge.json")).unwrap()).unwrap();
    assert_eq!(bridge["mirror_pi_setup"], serde_json::json!(true));
    assert_eq!(
        bridge["pi_agent_dir"],
        serde_json::json!(pi.agent.to_string_lossy())
    );
    output
}

/// Install the shipped manifest, not a preconfigured import. The helper wrapper
/// points at this checkout's adapter; the native activation must do all setup.
fn install_automatic_mirror(root: &Path, pi: &PiHome) {
    let adapter = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../extensions/octet-pi-compat")
        .canonicalize()
        .unwrap();
    let mut manifest = octet_agent::ExtensionManifest::parse(
        &std::fs::read_to_string(adapter.join("extension.toml")).unwrap(),
    )
    .unwrap();
    manifest.entrypoint.env.insert(
        "OCTET_PI_AGENT_DIR".into(),
        pi.agent.to_string_lossy().into(),
    );
    manifest
        .entrypoint
        .env
        .insert("HOME".into(), pi.home.to_string_lossy().into());
    // The adapter's transform cache is its own state, not the Pi setup's: keep
    // it out of both the Pi home and the installed bundle that are compared.
    manifest.entrypoint.env.insert(
        "OCTET_PI_COMPAT_CACHE".into(),
        root.join("adapter-cache").to_string_lossy().into(),
    );
    let output = root.join("extensions/octet-pi-compat");
    put(
        output.join("extension.toml"),
        toml::to_string(&manifest).unwrap(),
    );
    let helper = url::Url::from_file_path(adapter.join("auto-configure.mjs")).unwrap();
    put(
        output.join("auto-configure.mjs"),
        format!(
            "import {};\n",
            serde_json::to_string(helper.as_str()).unwrap()
        ),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enabling_shipped_pi_compat_mirrors_from_any_workspace_without_configure() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.join("unrelated-workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let pi = fixture_pi_home(&root);
    install_automatic_mirror(&root, &pi);
    let before = tree_contents(&pi.home);
    let installed = tree_contents(&root.join("extensions"));
    let off = app_state(&app_for(&root, &workspace, false));
    let mut app = app_for(&root, &workspace, true);
    app.refresh_resource_paths_headless().await.unwrap();
    assert_pi_setup_active(&app, &pi);
    let generated = app
        .executable_extensions
        .pi_mirror_directory
        .as_ref()
        .unwrap()
        .path()
        .to_owned();
    assert!(generated.join("octet-pi-compat/bridge.json").is_file());
    app.executable_extensions.shutdown().await;
    drop(app);
    assert!(!generated.exists(), "per-fleet mirror must be cleaned up");
    assert_eq!(tree_contents(&pi.home), before);
    assert_eq!(
        without_adapter_cache(tree_contents(&root.join("extensions"))),
        installed
    );
    assert_eq!(app_state(&app_for(&root, &workspace, false)), off);
    // A later activation sees a changed enabled factory without a manual
    // fingerprint/configure step or a stale captured entrypoint list.
    put(
        pi.agent.join("extensions/pi-setup-two.js"),
        "export default pi => pi.registerCommand('changed-after-enable', {handler(){}});",
    );
    let mut app = app_for(&root, &workspace, true);
    assert_eq!(
        extension_commands(&app),
        vec!["changed-after-enable", "pi-setup-one"]
    );
    app.executable_extensions.shutdown().await;
}

/// The interactive frontend attaches the mirror after its first frame. Pi lets a
/// `session_start` handler read the session manager, so the late-attached
/// process must already hold the native session view; a start that fails on it
/// refuses resource discovery and the mirror's skills, prompts and theme never
/// apply while its footer and composer still draw.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_attached_mirror_reads_the_session_in_session_start_and_admits_resources() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.join("unrelated-workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let pi = fixture_pi_home(&root);
    let seen = root.join("session-start-seen.json");
    put(
        pi.agent.join("extensions/pi-setup-two.js"),
        format!(
            r#"import {{ appendFileSync }} from 'node:fs';
export default function (pi) {{
  pi.registerCommand('pi-setup-two', {{ description: 'Pi setup two', handler() {{}} }});
  pi.on('session_start', (event, ctx) => {{
    appendFileSync({seen}, JSON.stringify({{ reason: event.reason, id: ctx.sessionManager.getSessionId(), branch: ctx.sessionManager.getBranch().map(entry => entry.type) }}) + '\n');
  }});
}}
"#,
            seen = serde_json::to_string(&seen.to_string_lossy()).unwrap()
        ),
    );
    install_automatic_mirror(&root, &pi);
    let mut app = app_with_timing(&root, &workspace, true, true);
    assert!(
        app.executable_extensions.processes.is_empty(),
        "the mirror attaches after the first frame"
    );
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while app.executable_extensions.startup_pending() {
            app.pump_extension_startup().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the mirror attaches");
    app.refresh_resource_paths_headless().await.unwrap();
    assert_pi_setup_active(&app, &pi);
    let seen = std::fs::read_to_string(&seen).expect("session_start ran to completion");
    let starts = seen
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 1, "one session start: {seen}");
    assert_eq!(starts[0]["reason"], "startup");
    assert_eq!(
        starts[0]["id"].as_str(),
        app.executable_extensions.session_id.as_deref()
    );
    // getBranch answered from the native history as it stood at attach.
    assert!(starts[0]["branch"].is_array(), "{seen}");
    app.executable_extensions.shutdown().await;
}

fn app_for(root: &Path, workspace: &Path, enabled: bool) -> App {
    app_with_timing(root, workspace, enabled, false)
}

/// `deferred` builds the App the interactive frontend does: extensions attach
/// after the first frame instead of inside the constructor.
fn app_with_timing(root: &Path, workspace: &Path, enabled: bool, deferred: bool) -> App {
    let mut config = crate::extensions::tests::executable_extension_config(
        workspace,
        &root.join("extensions"),
        "octet-pi-compat",
    );
    config.mode = crate::config::Mode::Print {
        prompt: "Pi setup mirror".into(),
    };
    config.workspace_trusted = true;
    config.context_files = true;
    if !enabled {
        config.enabled_extensions.clear();
    }
    let boot = crate::app::bootstrap::bootstrap(config).unwrap();
    let launch =
        crate::app::bootstrap::resolve_launch_print(&boot, "pi-mirror-acceptance").unwrap();
    if deferred {
        crate::app::bootstrap::build_app_first_frame_first(boot, launch, "BASE INSTRUCTIONS".into())
            .unwrap()
    } else {
        crate::app::bootstrap::build_app_with_resource_consumer(
            boot,
            launch,
            "BASE INSTRUCTIONS".into(),
        )
        .unwrap()
    }
}

fn render(app: &App, name: &str) -> String {
    app.prompts
        .render(
            name,
            "argument",
            &crate::prompts::PromptRenderContext {
                workspace: &app.config.workspace,
                selection: None,
                active_skills: &[],
            },
        )
        .unwrap()
        .text
}

fn extension_commands(app: &App) -> Vec<String> {
    let mut commands = app
        .executable_extensions
        .processes
        .iter()
        .flat_map(|process| process.contributions().commands.iter())
        .map(|command| command.name.clone())
        .collect::<Vec<_>>();
    commands.sort();
    commands
}

/// A complete observable state of one app, used to compare two "compat off"
/// runs byte-for-byte.
fn app_state(app: &App) -> Vec<String> {
    let mut skills = app
        .skills
        .descriptors()
        .iter()
        .map(|descriptor| format!("skill:{}", descriptor.id))
        .collect::<Vec<_>>();
    skills.sort();
    let mut prompts = app
        .prompts
        .descriptors()
        .iter()
        .map(|descriptor| descriptor.name.clone())
        .collect::<Vec<_>>();
    prompts.sort();
    let themes = crate::tui::theme::selectable_file_themes(
        &app.config,
        crate::tui::theme::TerminalBackground::Dark,
    )
    .iter()
    .map(|(name, _)| format!("theme:{name}"))
    .collect::<Vec<_>>();
    let mut keybindings = app
        .user_keybindings
        .iter()
        .map(|(id, keys)| format!("keybinding:{id}={}", keys.join("+")))
        .collect::<Vec<_>>();
    keybindings.sort();
    let mut state = vec![
        format!("theme-selected:{:?}", app.config.theme),
        format!(
            "model:{}:{}",
            crate::extensions::composition::pi_provider_id(&app.model),
            app.model.spec.api_name
        ),
        format!("extension-commands:{}", extension_commands(app).join(",")),
        format!(
            "system-has-pi-context:{}",
            app.agent.system_prompt().contains("PI-SETUP-CONTEXT")
        ),
        format!(
            "system-has-pi-catalog:{}",
            app.agent.system_prompt().contains("PI-SETUP-SKILL-CATALOG")
        ),
    ];
    state.extend(skills);
    state.extend(prompts.into_iter().map(|name| format!("prompt:{name}")));
    state.extend(themes);
    state.extend(keybindings);
    state
}

fn assert_pi_setup_active(app: &App, pi: &PiHome) {
    // Skills: the real native registry loaded the Pi skill.
    let skill = app.skills.load(&"pi-setup-skill".into()).unwrap();
    assert!(skill.instructions.contains("PI-SETUP-SKILL-BODY"));
    assert!(app.agent.system_prompt().contains("PI-SETUP-SKILL-CATALOG"));
    // Prompt templates: native expansion.
    assert!(render(app, "pi-setup-prompt").contains("PI-SETUP-PROMPT argument"));
    // Theme selection from Pi settings.json, parsed by the native Pi JSON projection.
    assert_eq!(app.config.theme.as_deref(), Some("pi-setup-theme"));
    assert_eq!(
        crate::tui::theme::load_theme(&app.config).source_path(),
        Some(pi.agent.join("themes/pi-setup-theme.json").as_path())
    );
    // Keybindings: the Pi keybindings.json overlay is native session state.
    assert_eq!(
        app.user_keybindings.get("app.exit"),
        Some(&vec!["ctrl+q".to_owned()])
    );
    // Model: the Pi defaultProvider/defaultModel selection resolved in the
    // catalog. Extension-provider routes use a host-minted wire id, so the
    // portable identity is the provider plus its api model name.
    assert_eq!(app.model.spec.api_name, "pi-mirror-model");
    assert_eq!(
        crate::extensions::composition::pi_provider_id(&app.model),
        "pi-mirror-provider"
    );
    // Extensions: both enabled Pi factories loaded through the adapter.
    assert_eq!(app.executable_extensions.processes.len(), 1);
    assert_eq!(
        extension_commands(app),
        vec!["pi-setup-one".to_owned(), "pi-setup-two".to_owned()]
    );
    // Global Pi context file is native system-prompt input.
    assert!(app.agent.system_prompt().contains("PI-SETUP-CONTEXT"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manually_enabled_pi_compat_lists_all_live_palettes_and_withdraws_on_retirement() {
    use crate::tui::theme::{selectable_file_themes, TerminalBackground, ThemeSource};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let pi = fixture_pi_home(&root);
    put(
        pi.agent.join("themes/second.json"),
        super::pi_theme::fixture("Second Pi palette", "#abcdef").to_string(),
    );
    put(pi.agent.join("themes/broken.json"), "{}");
    put(
        root.join("manual.mjs"),
        "export default pi => pi.registerCommand('manual-only', {handler(){}});",
    );
    let adapter = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/octet-pi-compat");
    let output = root.join("extensions/octet-pi-compat");
    let configured = Command::new("node")
        .arg(adapter.join("configure.mjs"))
        .args(["--reviewed", "--output"])
        .arg(&output)
        .arg(root.join("manual.mjs"))
        .current_dir(&workspace)
        .env("OCTET_PI_AGENT_DIR", &pi.agent)
        .output()
        .unwrap();
    assert!(
        configured.status.success(),
        "{}",
        String::from_utf8_lossy(&configured.stderr)
    );
    let manifest = std::fs::read(output.join("extension.toml")).unwrap();
    let bridge = std::fs::read(output.join("bridge.json")).unwrap();
    let before = tree_contents(&pi.home);
    let mut on = app_for(&root, &workspace, true);
    // Explicit built-in selection is unaffected by Pi's saved preference.
    on.config.theme = Some("pi".into());
    on.config.theme_explicit = true;
    on.refresh_resource_paths_headless().await.unwrap();
    let names = |app: &App| {
        selectable_file_themes(&app.config, TerminalBackground::Dark)
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>()
    };
    let choices = names(&on);
    assert!(choices.contains(&"pi-setup-theme".into()), "{choices:?}");
    assert!(choices.contains(&"second".into()), "{choices:?}");
    assert!(!choices.contains(&"broken".into()));
    assert_eq!(
        crate::tui::theme::load_theme(&on.config).source(),
        &ThemeSource::CompiledPi
    );
    assert_eq!(extension_commands(&on), vec!["manual-only"]);
    assert!(
        on.skills.load(&"pi-setup-skill".into()).is_err(),
        "themes must not enable mirror skills or factories"
    );
    assert_eq!(
        tree_contents(&pi.home),
        before,
        "discovery must not write Pi settings or resources"
    );

    std::fs::remove_file(pi.agent.join("themes/second.json")).unwrap();
    put(
        pi.agent.join("themes/new.json"),
        super::pi_theme::fixture("New Pi palette", "#123abc").to_string(),
    );
    let reloaded_home = tree_contents(&pi.home);
    on.mark_resource_paths_reload();
    on.refresh_resource_paths_headless().await.unwrap();
    let choices = names(&on);
    assert!(
        !choices.contains(&"second".into()),
        "deleted palette must not accumulate"
    );
    assert!(choices.contains(&"new".into()));
    assert_eq!(on.config.theme.as_deref(), Some("pi"));
    assert_eq!(tree_contents(&pi.home), reloaded_home);
    assert_eq!(
        std::fs::read(output.join("extension.toml")).unwrap(),
        manifest
    );
    assert_eq!(std::fs::read(output.join("bridge.json")).unwrap(), bridge);
    // Retire the contributor, not the entire App/session binding. Terminal
    // fleet shutdown deliberately forbids all later publication.
    on.executable_extensions.processes[0].shutdown().await;
    on.refresh_resource_paths_headless().await.unwrap();
    assert!(names(&on)
        .iter()
        .all(|name| name != "pi-setup-theme" && name != "new"));
    let off = app_for(&root, &workspace, false);
    assert!(names(&off)
        .iter()
        .all(|name| name != "pi-setup-theme" && name != "new"));
}

fn assert_octet_only(app: &App) {
    assert!(app.skills.load(&"pi-setup-skill".into()).is_err());
    assert!(!app.prompts.contains("pi-setup-prompt"));
    assert_ne!(app.config.theme.as_deref(), Some("pi-setup-theme"));
    assert!(app.user_keybindings.is_empty());
    assert_eq!(app.model.spec.api_name, "gpt-4o-mini");
    assert!(extension_commands(app).is_empty());
    assert!(!app.agent.system_prompt().contains("PI-SETUP-CONTEXT"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mirrored_pi_setup_toggles_off_on_off_without_residue() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let pi = fixture_pi_home(&root);
    let before = tree_contents(&pi.home);
    let output = configure_mirror(&root, &pi, &workspace);
    let configured_manifest = std::fs::read(output.join("extension.toml")).unwrap();

    // Compat off: ordinary Octet with no Pi residue.
    let off_first = app_for(&root, &workspace, false);
    assert_octet_only(&off_first);
    let state_first = app_state(&off_first);
    drop(off_first);

    // Compat on: every fixture item is active through the real host and adapter.
    let mut on = app_for(&root, &workspace, true);
    assert_eq!(on.executable_extensions.processes.len(), 1);
    on.refresh_resource_paths_headless().await.unwrap();
    assert!(!on.resource_paths_pending());
    assert_pi_setup_active(&on, &pi);
    let state_on = app_state(&on);
    assert!(state_on.len() > state_first.len());
    drop(on);

    // Compat off again: identical to the first off state, and neither home was
    // written by either pass.
    let off_second = app_for(&root, &workspace, false);
    assert_octet_only(&off_second);
    assert_eq!(app_state(&off_second), state_first);
    drop(off_second);
    assert_eq!(tree_contents(&pi.home), before);
    assert_eq!(
        std::fs::read(output.join("extension.toml")).unwrap(),
        configured_manifest
    );
}

/// Toggling is idempotent: a second publication from the same bridge must
/// reproduce the same active set, not accumulate duplicates or stale roots.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mirrored_pi_setup_reload_is_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let pi = fixture_pi_home(&root);
    let before = tree_contents(&pi.home);
    configure_mirror(&root, &pi, &workspace);

    let mut app = app_for(&root, &workspace, true);
    app.refresh_resource_paths_headless().await.unwrap();
    let first = app_state(&app);
    app.mark_resource_paths_reload();
    app.refresh_resource_paths_headless().await.unwrap();
    assert_eq!(app_state(&app), first);
    assert_eq!(tree_contents(&pi.home), before);
    assert_eq!(extension_commands(&app).len(), 2);
    // The overlay is session-only: dropping the extension restores the saved
    // configuration exactly.
    let restored = app.original_resource_config();
    assert_ne!(restored.theme.as_deref(), Some("pi-setup-theme"));
    drop(app);
}

/// A narrowed native catalog can be completed while resolving a real mirrored
/// resource default. That completion must also replace the pre-start facts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mirrored_default_catalog_enrichment_republishes_headless_model_facts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let pi = fixture_pi_home(&root);
    configure_mirror(&root, &pi, &workspace);
    let mut app = app_for(&root, &workspace, true);
    // Model the narrowed launch catalog: retain the real provider declaration,
    // but force its catalog completion through apply_resource_defaults rather
    // than letting the initial projection accidentally satisfy the assertion.
    let contributed = app
        .catalog
        .models()
        .find(|spec| spec.api_name == "pi-mirror-model")
        .unwrap()
        .clone();
    assert!(app
        .catalog
        .remove_model_if_endpoint(&contributed.id, &contributed.endpoint));
    app.readiness = crate::app::bootstrap::CatalogReadiness::Routes(vec!["openai"]);
    app.refresh_resource_paths_headless().await.unwrap();
    assert_pi_setup_active(&app, &pi);
    {
        let state = app.executable_extensions.host_state.lock().unwrap();
        let snapshot = state.pi_models.as_ref().unwrap();
        assert!(snapshot["available_models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|model| {
                model["provider"] == "pi-mirror-provider" && model["id"] == "pi-mirror-model"
            }));
        assert_eq!(snapshot["scoped_models"], serde_json::json!([]));
        assert!(snapshot.get("all_models").is_none());
        assert_eq!(snapshot["provider_auth_status"]["status"], "unavailable");
    }
    app.executable_extensions.shutdown().await;
}

/// An explicit in-session model choice (the provenance `transition` records for
/// `/model` and the picker) must stay authoritative over the mirrored default,
/// and withdrawal must not undo it either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mirrored_pi_setup_yields_to_an_explicit_user_model_choice() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let pi = fixture_pi_home(&root);
    configure_mirror(&root, &pi, &workspace);

    let mut app = app_for(&root, &workspace, true);
    app.refresh_resource_paths_headless().await.unwrap();
    assert_eq!(app.model.spec.api_name, "pi-mirror-model");

    // The user picks another route; the real interactive transition records the
    // same provenance bit this test sets.
    let user_model = app
        .catalog
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    let reasoning = crate::app::default_reasoning_for_model(&user_model);
    app.agent
        .select_model_at_idle(
            user_model.clone(),
            reasoning.clone(),
            crate::app::reasoning_label(&reasoning),
        )
        .unwrap();
    app.model = user_model;
    app.config.model_explicit = true;
    app.mark_resource_paths_reload();
    app.refresh_resource_paths_headless().await.unwrap();
    assert_eq!(app.model.spec.api_name, "gpt-4o-mini");
    // The mirrored resources stay active; only the route preference yields.
    assert!(app.user_keybindings.contains_key("app.exit"));
}
