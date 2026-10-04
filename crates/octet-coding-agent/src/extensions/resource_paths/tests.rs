use super::*;
use crate::tui::theme::TerminalBackground;
use octet_agent::extension_process::ExtensionActivation;

async fn fixture(root: &Path) -> (ExecutableExtensions, Config) {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let skill = root.join("assets/skills/discovery-proof/SKILL.md");
    let prompt = root.join("assets/prompts/discovery-proof.md");
    let theme = root.join("assets/themes/discovery-proof.toml");
    for path in [&skill, &prompt, &theme] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    }
    std::fs::write(&skill, "---\nname: discovery-proof\ndescription: resource loader sentinel\n---\nSKILL BODY SENTINEL\n").unwrap();
    std::fs::write(
        &prompt,
        "---\ndescription: resource prompt\n---\nPROMPT SENTINEL $1\n",
    )
    .unwrap();
    std::fs::write(
        &theme,
        "[metadata]\nname = \"Resource Proof\"\n[glyphs]\nprompt = \":\"\n[glyphs_ascii]\nprompt = \":\"\n",
    )
    .unwrap();
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/extensions/resource_paths/fixture.py");
    let manifest = ExtensionManifest::parse(&format!(
        r#"
name = "resource-path-fixture"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "python3"
args = [{script:?}, {root:?}]
[contributes]
hooks = ["session_start", "resources_discover"]
"#
    ))
    .unwrap();
    let mut runtime = ExtensionRuntimeConfig::new(&workspace);
    runtime.resource_paths = true;
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
    // Actual awaited session hook, not a log line standing in for dispatch.
    process
        .start_session_hook_binding("resource-path-owner")
        .await
        .unwrap();
    let mut extensions = ExecutableExtensions::default();
    extensions.resource_owner = Some("resource-path-owner".into());
    extensions.session_lifecycle_started = true;
    extensions.processes.push(process);
    let mut config =
        super::super::tests::executable_extension_config(&workspace, root, "resource-path-fixture");
    config.theme = Some("discovery-proof".into());
    (extensions, config)
}

#[tokio::test]
async fn discovery_reply_feeds_real_skill_prompt_and_theme_loaders_then_replaces_roots() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (extensions, base) = fixture(&root).await;
    let batch = extensions
        .prepare_resource_discovery(ExtensionResourceDiscoveryReason::Startup)
        .unwrap()
        .await;
    let loaded = extensions
        .load_resource_discovery(&base, batch, TerminalBackground::Dark)
        .unwrap();
    assert!(loaded.is_current(&extensions));
    let descriptor = loaded
        .skills
        .descriptors()
        .iter()
        .find(|s| s.id.as_str() == "discovery-proof")
        .unwrap()
        .clone();
    assert!(loaded
        .skills
        .load(&descriptor.id)
        .unwrap()
        .instructions
        .contains("SKILL BODY SENTINEL"));
    assert!(
        crate::resources::format_skills_for_prompt(&loaded.skills.descriptors())
            .contains("resource loader sentinel")
    );
    let rendered = loaded
        .prompts
        .render(
            "discovery-proof",
            "argument",
            &crate::prompts::PromptRenderContext {
                workspace: &base.workspace,
                selection: None,
                active_skills: &[],
            },
        )
        .unwrap();
    assert!(rendered.text.contains("PROMPT SENTINEL argument"));
    assert_eq!(
        loaded.selected_theme.source_path(),
        Some(root.join("assets/themes/discovery-proof.toml").as_path())
    );
    assert_eq!(loaded.selected_theme.glyph("prompt"), ":");
    // Fresh baseline plus an empty authoritative reply removes old roots.
    let batch = extensions
        .prepare_resource_discovery(ExtensionResourceDiscoveryReason::Reload)
        .unwrap()
        .await;
    let replacement = extensions
        .load_resource_discovery(&base, batch, TerminalBackground::Dark)
        .unwrap();
    assert!(!replacement
        .skills
        .descriptors()
        .iter()
        .any(|s| s.id.as_str() == "discovery-proof"));
    assert!(!replacement.prompts.contains("discovery-proof"));
    assert!(replacement.selected_theme.source_path().is_none());
    let calls = std::fs::read_to_string(root.join("calls.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        calls
            .iter()
            .map(|v| v["hook"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["session_start", "resources_discover", "resources_discover"]
    );
    assert_eq!(calls[1]["payload"]["cwd"], base.workspace.to_str().unwrap());
    assert_eq!(calls[2]["payload"]["reason"], "reload");
    extensions.processes[0].shutdown().await;
}

#[tokio::test]
async fn discovery_refuses_owner_change_and_generation_retirement_before_publication() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (mut extensions, base) = fixture(&root).await;
    let batch = extensions
        .prepare_resource_discovery(ExtensionResourceDiscoveryReason::Startup)
        .unwrap()
        .await;
    extensions.resource_owner = Some("another-owner".into());
    assert!(extensions
        .load_resource_discovery(&base, batch, TerminalBackground::Dark)
        .is_err());
    extensions.resource_owner = Some("resource-path-owner".into());
    let batch = extensions
        .prepare_resource_discovery(ExtensionResourceDiscoveryReason::Startup)
        .unwrap()
        .await;
    let loaded = extensions
        .load_resource_discovery(&base, batch, TerminalBackground::Dark)
        .unwrap();
    extensions.processes[0].reload().await.unwrap();
    assert!(!loaded.is_current(&extensions));
    extensions.processes[0].shutdown().await;
}

#[test]
fn path_admission_does_not_launder_workspace_trust_links_or_theme_parsing() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let config = super::super::tests::executable_extension_config(&root, &root, "paths");
    let path = root.join("prompt.md");
    std::fs::write(&path, "prompt").unwrap();
    assert!(admit_path(&path, &config, &[], PathKind::Prompt).is_err());
    assert!(admit_path(
        &path,
        &config,
        std::slice::from_ref(&path),
        PathKind::Prompt
    )
    .is_ok());
    let link = root.join("linked.md");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(admit_path(
        &link,
        &config,
        std::slice::from_ref(&root),
        PathKind::Prompt
    )
    .is_err());
    let json = root.join("pi.json");
    std::fs::write(&json, "{}").unwrap();
    assert!(admit_path(&json, &config, &[], PathKind::Theme).is_err());
    assert!(admit_path(&json, &config, std::slice::from_ref(&root), PathKind::Theme).is_ok());
    // A supported extension is not a successful parse. Actual native loading
    // still rejects a malformed palette after path/trust admission.
    assert!(crate::tui::theme::load_theme_path(&json, &config).is_err());
}

#[tokio::test]
async fn discovery_reply_loads_actual_pi_json_and_native_explicit_precedence() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (extensions, mut base) = fixture(&root).await;
    let theme_path = root.join("assets/themes/pi-resource-proof.json");
    std::fs::write(&theme_path, pi_theme::fixture("Contributed Pi", "#123456").to_string()).unwrap();
    base.theme = Some("pi-resource-proof.json".into());
    let batch = extensions.prepare_resource_discovery(ExtensionResourceDiscoveryReason::Startup).unwrap().await;
    let loaded = extensions.load_resource_discovery(&base, batch, TerminalBackground::Dark).unwrap();
    assert_eq!(loaded.selected_theme.source_path(), Some(theme_path.as_path()));
    assert_eq!(loaded.selected_theme.metadata().name, "Contributed Pi");
    assert_eq!(loaded.selected_theme.resolve::<String>("accent").as_deref(), Some("#123456"));
    assert_eq!(loaded.selected_theme.background(), TerminalBackground::Dark);
    assert!(loaded.is_current(&extensions));
    assert!(!loaded.diagnostics.iter().any(|message| message.contains("theme failed")));
    let choices = crate::tui::theme::selectable_file_themes(&loaded.config, TerminalBackground::Dark);
    assert!(choices.iter().any(|(name, theme)| name == "pi-resource-proof" && theme.source_path() == Some(theme_path.as_path())));
    // Native snapshot conversion must not reinterpret already-normalized TOML
    // as JSON just because its inspectable source still ends in .json.
    let light = loaded.selected_theme.for_native_background(TerminalBackground::Light).unwrap();
    assert_eq!(light.resolve::<String>("accent").as_deref(), Some("#123456"));
    assert_eq!(light.source_path(), Some(theme_path.as_path()));
    std::fs::write(&theme_path, pi_theme::fixture("Reloaded Pi", "#654321").to_string()).unwrap();
    let reloaded = loaded.selected_theme.reload().unwrap();
    assert_eq!(reloaded.metadata().name, "Reloaded Pi");
    assert_eq!(reloaded.resolve::<String>("accent").as_deref(), Some("#654321"));

    let user_theme = root.join("explicit/pi-resource-proof.toml");
    std::fs::create_dir_all(user_theme.parent().unwrap()).unwrap();
    std::fs::write(&user_theme, "[metadata]\nname = 'Explicit Native'\n[colors]\naccent = '#abcdef'\n").unwrap();
    base.theme_paths.push(user_theme.clone());
    let batch = extensions.prepare_resource_discovery(ExtensionResourceDiscoveryReason::Startup).unwrap().await;
    let winner = extensions.load_resource_discovery(&base, batch, TerminalBackground::Dark).unwrap();
    assert_eq!(winner.selected_theme.source_path(), Some(user_theme.as_path()));
    assert_eq!(winner.selected_theme.metadata().name, "Explicit Native");
    assert!(winner.diagnostics.iter().any(|message| message.contains("shadowed")));
    // Invalid higher-precedence TOML is not silently replaced by lower JSON.
    std::fs::write(&user_theme, "[colors\naccent = '#abcdef'\n").unwrap();
    let batch = extensions.prepare_resource_discovery(ExtensionResourceDiscoveryReason::Startup).unwrap().await;
    let invalid = extensions.load_resource_discovery(&base, batch, TerminalBackground::Dark).unwrap();
    assert!(invalid.selected_theme.source_path().is_none());
    assert!(invalid.diagnostics.iter().any(|message| message.contains("theme failed")));
    extensions.processes[0].shutdown().await;
}

#[test]
fn json_themes_share_native_trust_order_reserved_names_bounds_and_no_follow() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    let global = root.join("global");
    let project = workspace.join(".octet/themes");
    let contributed = root.join("contributed");
    let explicit = root.join("explicit");
    for dir in [global.join("themes"), project.clone(), contributed.clone(), explicit.clone()] {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("precedence.json"), pi_theme::fixture("Pi", "#123456").to_string()).unwrap();
    }
    let trusted = ResourceResolver::with_global_octet_dir(workspace.clone(), true, global.clone());
    assert_eq!(trusted.discover(ResourceKind::Theme, &[]).get("precedence").unwrap().path, project.join("precedence.json"));
    let untrusted = ResourceResolver::with_global_octet_dir(workspace.clone(), false, global.clone());
    assert_eq!(untrusted.discover(ResourceKind::Theme, &[]).get("precedence").unwrap().path, global.join("themes/precedence.json"));
    assert_eq!(trusted.discover(ResourceKind::Theme, &[contributed.clone(), explicit.clone()]).get("precedence").unwrap().path, explicit.join("precedence.json"));
    // Within one directory the existing lexicographic/native later-wins rule
    // means .toml wins over .json, independently of read_dir enumeration order.
    std::fs::write(explicit.join("precedence.toml"), "[metadata]\nname = 'Native'\n").unwrap();
    assert_eq!(trusted.discover(ResourceKind::Theme, &[explicit.clone()]).get("precedence").unwrap().path, explicit.join("precedence.toml"));

    let mut config = super::super::tests::executable_extension_config(&workspace, &root, "pi-paths");
    config.theme_paths = vec![explicit.clone()];
    for name in ["default", "auto", "dark", "light", "Cards", "Still"] {
        std::fs::write(explicit.join(format!("{name}.json")), pi_theme::fixture("Must Not Shadow", "#123456").to_string()).unwrap();
        let theme = crate::tui::theme::load_named_theme_for_background(&format!("{name}.json"), &config, TerminalBackground::Dark).unwrap();
        assert!(theme.source_path().is_none());
    }
    let choices = crate::tui::theme::selectable_file_themes(&config, TerminalBackground::Dark);
    assert!(!choices.iter().any(|(name, _)| crate::tui::theme::is_reserved_theme_name(name)));
    let oversized = explicit.join("oversized.json");
    std::fs::write(&oversized, vec![b' '; 256 * 1024 + 1]).unwrap();
    assert!(crate::tui::theme::load_theme_path(&oversized, &config).is_err());
    let link = explicit.join("linked.json");
    std::os::unix::fs::symlink(explicit.join("precedence.json"), &link).unwrap();
    assert!(crate::tui::theme::load_theme_path(&link, &config).is_err());
    assert!(trusted.discover(ResourceKind::Theme, &[explicit]).get("linked").is_none());
}
