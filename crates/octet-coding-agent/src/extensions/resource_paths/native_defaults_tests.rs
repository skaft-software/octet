//! Synthetic native catalog + local resource peer; no provider requests or credentials.
use super::*;
use crate::app::App;
use crate::tui::theme::TerminalBackground;
use octet_ai::types::ReasoningOptions;
use octet_ai::{
    Auth, EndpointId, Model, ModelCatalog, ModelId, OpenAiChatReasoningMode, Protocol,
    ReasoningCapability, ReasoningConfig, ReasoningControl, ReasoningEffort,
};
use serde_json::{json, Value};

fn register_model(app: &mut App, name: &str, endpoint: &str, levels: &[&str]) -> Model {
    let mut route = (*app.model.endpoint).clone();
    route.id = EndpointId(endpoint.into());
    route.base_url = url::Url::parse("http://127.0.0.1:9/synthetic/").unwrap();
    route.auth = Auth::None;
    route.runtime = Default::default();
    route.transport = Default::default();
    app.catalog.register_endpoint(route.clone()).unwrap();
    let mut spec = (*app.model.spec).clone();
    spec.id = ModelId(format!("synthetic/{name}"));
    spec.api_name = name.into();
    spec.endpoint = route.id;
    spec.protocol = Protocol::OpenAiChat;
    spec.preset = Default::default();
    spec.cache = Default::default();
    spec.capabilities.responses_features = Default::default();
    spec.capabilities.responses_lite = false;
    spec.capabilities.agent_delegation = None;
    spec.capabilities.reasoning = (!levels.is_empty()).then(|| ReasoningCapability {
        options: Some(ReasoningOptions {
            values: levels.iter().map(|level| (*level).into()).collect(),
            default: Some(levels[0].into()),
        }),
        control: ReasoningControl::Effort,
        exposes_text: false,
        preserves_state: false,
        effort_budgets: None,
        openai_chat_mode: OpenAiChatReasoningMode::Standard,
        min_effort: ReasoningEffort::Minimal,
        max_effort: ReasoningEffort::Max,
    });
    let id = spec.id.clone();
    app.catalog.register_model(spec).unwrap();
    app.catalog.resolve(&id).unwrap()
}

fn choose(app: &mut App, model: Model, reasoning: ReasoningConfig) {
    app.agent
        .select_model_at_idle(
            model.clone(),
            reasoning.clone(),
            crate::app::reasoning_label(&reasoning),
        )
        .unwrap();
    app.model = model;
    app.reasoning = reasoning;
}

async fn fixture(root: &Path) -> (App, ExtensionProcess, Model, Model) {
    let (mut app, process, _) = consumer_tests::fixture(root).await;
    // The catalog is entirely synthetic. Fleet admission forbids enrichment
    // from loading any live provider inventory during missing-model tests.
    app.catalog = ModelCatalog::default();
    app.readiness = crate::app::bootstrap::CatalogReadiness::Fleet;
    app.config.offline = true;
    let baseline = register_model(
        &mut app,
        "baseline",
        "synthetic-baseline",
        &["off", "low", "high", "max"],
    );
    let preferred = register_model(
        &mut app,
        "preferred",
        "synthetic-preferred",
        &["off", "low", "high", "max"],
    );
    choose(
        &mut app,
        baseline.clone(),
        ReasoningConfig::Effort(ReasoningEffort::Low),
    );
    app.config.model = Some(baseline.spec.id.clone());
    app.config.reasoning = Some(app.reasoning.clone());
    (app, process, baseline, preferred)
}

async fn publish(app: &mut App, root: &Path, paths: Value) -> Vec<String> {
    std::fs::write(root.join("reply.json"), paths.to_string()).unwrap();
    app.mark_resource_paths_reload();
    let lease = app
        .executable_extensions
        .resource_session_starts()
        .unwrap()
        .await
        .unwrap();
    let (loaded, lease) = app
        .prepare_resource_paths(TerminalBackground::Dark, lease)
        .unwrap()
        .await
        .unwrap();
    app.apply_extension_resource_paths(loaded, lease).unwrap().1
}

fn assert_selection(app: &App, model: &Model, reasoning: ReasoningConfig) {
    assert_eq!(app.model.spec.id, model.spec.id);
    assert_eq!(app.agent.model().spec.id, model.spec.id);
    assert_eq!(app.reasoning, reasoning);
    assert_eq!(app.agent.reasoning(), &reasoning);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_is_preserved_same_model_reasoning_changes_and_withdrawal_restores_launch() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (mut app, process, baseline, preferred) = fixture(&root).await;
    let original_config = (app.config.model.clone(), app.config.reasoning.clone());
    let diagnostics = publish(
        &mut app,
        &root,
        json!({
            "default_model": {"provider": "synthetic", "model": "preferred"},
            "default_thinking_level": "max"
        }),
    )
    .await;
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_selection(
        &app,
        &preferred,
        ReasoningConfig::Effort(ReasoningEffort::Max),
    );
    assert_eq!(
        (app.config.model.clone(), app.config.reasoning.clone()),
        original_config
    );
    let diagnostics = publish(
        &mut app,
        &root,
        json!({
            "default_model": {"provider": "synthetic", "model": "preferred"},
            "default_thinking_level": "high"
        }),
    )
    .await;
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_selection(
        &app,
        &preferred,
        ReasoningConfig::Effort(ReasoningEffort::High),
    );
    let diagnostics = publish(&mut app, &root, json!({})).await;
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_selection(
        &app,
        &baseline,
        ReasoningConfig::Effort(ReasoningEffort::Low),
    );
    process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_model_and_reasoning_independently_override_resource_defaults() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (mut app, process, baseline, preferred) = fixture(&root).await;
    app.config.model_explicit = true;
    let diagnostics = publish(
        &mut app,
        &root,
        json!({
            "default_model": {"provider": "missing", "model": "must-not-resolve"},
            "default_thinking_level": "max"
        }),
    )
    .await;
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_selection(
        &app,
        &baseline,
        ReasoningConfig::Effort(ReasoningEffort::Max),
    );
    // Withdrawal restores effort but never changes an explicit model.
    publish(&mut app, &root, json!({})).await;
    assert_selection(
        &app,
        &baseline,
        ReasoningConfig::Effort(ReasoningEffort::Low),
    );
    app.config.model_explicit = false;
    app.config.reasoning_explicit = true;
    app.config.reasoning = Some(ReasoningConfig::Effort(ReasoningEffort::High));
    choose(
        &mut app,
        baseline.clone(),
        ReasoningConfig::Effort(ReasoningEffort::High),
    );
    let diagnostics = publish(
        &mut app,
        &root,
        json!({
            "default_model": {"provider": "synthetic", "model": "preferred"},
            "default_thinking_level": "max"
        }),
    )
    .await;
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_selection(
        &app,
        &preferred,
        ReasoningConfig::Effort(ReasoningEffort::High),
    );
    publish(&mut app, &root, json!({})).await;
    assert_selection(
        &app,
        &baseline,
        ReasoningConfig::Effort(ReasoningEffort::High),
    );
    app.config.model_explicit = true;
    publish(
        &mut app,
        &root,
        json!({
            "default_model": {"provider": "synthetic", "model": "preferred"},
            "default_thinking_level": "max"
        }),
    )
    .await;
    assert_selection(
        &app,
        &baseline,
        ReasoningConfig::Effort(ReasoningEffort::High),
    );
    process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsupported_thinking_and_missing_model_leave_route_and_effort_together() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (mut app, process, baseline, _) = fixture(&root).await;
    let _unsupported = register_model(&mut app, "no-thinking", "synthetic-no-thinking", &[]);
    let head = app.agent.session().head_ref().cloned();
    let diagnostics = publish(
        &mut app,
        &root,
        json!({
            "default_model": {"provider": "synthetic", "model": "no-thinking"},
            "default_thinking_level": "max"
        }),
    )
    .await;
    assert!(
        diagnostics
            .iter()
            .any(|line| line.contains("max") && line.contains("not supported")),
        "{diagnostics:?}"
    );
    assert_selection(
        &app,
        &baseline,
        ReasoningConfig::Effort(ReasoningEffort::Low),
    );
    assert_eq!(app.agent.session().head_ref(), head.as_ref());
    let diagnostics = publish(
        &mut app,
        &root,
        json!({
            "default_model": {"provider": "not-configured", "model": "requested-model"},
            "default_thinking_level": "high"
        }),
    )
    .await;
    let diagnostic = diagnostics
        .iter()
        .find(|line| line.contains("not-configured/requested-model"))
        .expect("missing request must be diagnosed");
    assert!(diagnostic.contains("configure") && diagnostic.contains("--model"));
    assert!(diagnostic.chars().count() <= 4096);
    assert!(!diagnostic.chars().any(char::is_control));
    assert_selection(
        &app,
        &baseline,
        ReasoningConfig::Effort(ReasoningEffort::Low),
    );
    assert_eq!(app.agent.session().head_ref(), head.as_ref());
    process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_wire_thinking_is_diagnosed_without_selecting_a_different_model() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (mut app, process, baseline, _) = fixture(&root).await;
    let diagnostics = publish(
        &mut app,
        &root,
        json!({
            "default_model": {"provider": "synthetic", "model": "preferred"},
            "default_thinking_level": "maximum"
        }),
    )
    .await;
    assert!(
        diagnostics
            .iter()
            .any(|line| line.contains("default_thinking_level")),
        "{diagnostics:?}"
    );
    assert_selection(
        &app,
        &baseline,
        ReasoningConfig::Effort(ReasoningEffort::Low),
    );
    process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn theme_only_reload_preserves_live_thinking_and_disable_restores_baseline() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (mut app, process, baseline, _) = fixture(&root).await;
    publish(
        &mut app,
        &root,
        json!({"theme_paths": [root.join("themes")]}),
    )
    .await;
    choose(
        &mut app,
        baseline.clone(),
        ReasoningConfig::Effort(ReasoningEffort::High),
    );
    let head = app.agent.session().head_ref().cloned();
    publish(
        &mut app,
        &root,
        json!({"theme_paths": [root.join("themes")]}),
    )
    .await;
    assert_selection(
        &app,
        &baseline,
        ReasoningConfig::Effort(ReasoningEffort::High),
    );
    assert_eq!(app.agent.session().head_ref(), head.as_ref());
    publish(
        &mut app,
        &root,
        json!({
            "default_model": {"provider": "synthetic", "model": "preferred"},
            "default_thinking_level": "max"
        }),
    )
    .await;
    let work = app
        .prepare_resource_withdrawal(TerminalBackground::Dark, anyhow::anyhow!("disabled"))
        .unwrap();
    let (loaded, lease) = work.await.unwrap();
    app.apply_extension_resource_paths(loaded, lease).unwrap();
    assert_selection(
        &app,
        &baseline,
        ReasoningConfig::Effort(ReasoningEffort::Low),
    );
    process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openai_codex_alias_resolves_only_the_declared_endpoint_not_openai() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (mut app, process, _, _) = fixture(&root).await;
    let codex = register_model(
        &mut app,
        "codex-test",
        "openai-codex",
        &["off", "high", "max"],
    );
    let openai = register_model(&mut app, "openai-test", "openai", &["off", "high", "max"]);
    // Both routes expose the same upstream name; provider identity disambiguates.
    let mut spec = (*openai.spec).clone();
    app.catalog
        .remove_model_if_endpoint(&spec.id, &openai.endpoint.id);
    spec.api_name = "codex-test".into();
    app.catalog.register_model(spec).unwrap();
    let diagnostics = publish(
        &mut app,
        &root,
        json!({
            "default_model": {"provider": "openai-codex", "model": "codex-test"},
            "default_thinking_level": "max"
        }),
    )
    .await;
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_selection(&app, &codex, ReasoningConfig::Effort(ReasoningEffort::Max));
    assert_eq!(app.model.endpoint.id.0, "openai-codex");
    process.shutdown().await;
}
