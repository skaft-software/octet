//! Shared fixtures for the `app::bootstrap` test suite.
//!
//! The config/extension/credential builders that several bootstrap test groups
//! need. They live here so a change to the private bootstrap builder shapes is
//! one compile error in one file rather than drift across every group module.

use super::*;

pub(super) fn config(directory: &std::path::Path, model: Option<&str>) -> Config {
    Config {
        workspace: directory.to_path_buf(),
        invocation_cwd: directory.to_path_buf(),
        model: model.map(|model| ModelId(model.to_owned())),
        model_explicit: model.is_some(),
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
        plain: false,
        show_images: false,
        session_dir: directory.join("sessions"),
        compaction: CompactionPolicy::default(),
        max_cost_microdollars: None,
        cost_warning_microdollars: None,
        max_turns: Some(40),
        show_reasoning_in_print: false,
        initial_prompt: None,
        prompt_template: None,
        debug_prompt: false,
        prompt_paths: vec![],
        mode: Mode::Print {
            prompt: "hi".to_owned(),
        },
        resume: ResumeSelector::New,
        mouse: crate::config::MouseMode::Auto,
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

pub(super) fn configured_test_extensions(
    _skills: Arc<dyn SkillRegistry>,
    config: &Config,
) -> ExtensionHost {
    let boot = bootstrap(config.clone()).unwrap();
    let model_id = config.model.as_ref().expect("test model");
    let model = boot.catalog.resolve(model_id).unwrap();
    let session = Session::create(config.workspace.join("tool-policy-test.jsonl")).unwrap();
    let reasoning = config
        .reasoning
        .clone()
        .unwrap_or_else(|| default_reasoning_for_model(&model));
    configured_extensions(config, &session, &model, &reasoning, &boot.sessions)
        .unwrap()
        .0
}

pub(super) fn append_active_skill(session: &mut Session, id: &str, required_tools: &[&str]) {
    session
        .append(EntryValue::SkillActivated {
            descriptor: octet_agent::SkillDescriptor {
                id: id.into(),
                name: id.into(),
                description: "test active skill".into(),
                license: None,
                compatibility: None,
                metadata: Default::default(),
                allowed_tools: vec![],
                disable_model_invocation: false,
                version: None,
                source: octet_agent::SkillSource::BuiltIn,
                trust: octet_agent::SkillTrust::BuiltIn,
                required_tools: required_tools
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect(),
                tags: vec![],
            },
            instructions_hash: "test-hash".into(),
            instructions: "test instructions".into(),
        })
        .unwrap();
}

use crate::config::{CompactionPolicy, Mode, ResumeSelector, SandboxPolicy};

pub(super) fn write_codex_credential(path: &std::path::Path, localhost: bool, plan: &str) {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;

    let payload = serde_json::json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "acct_test",
            "chatgpt_plan_type": plan,
            "localhost": localhost
        }
    });
    let access = format!(
        "h.{}.s",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap())
    );
    let bytes = serde_json::to_vec(&serde_json::json!({
        "tokens": {
            "access_token": access,
            "refresh_token": "refresh",
            "account_id": "acct_test"
        },
        "expires_at": u64::MAX
    }))
    .unwrap();
    octet_agent::secure_fs::write_private_atomic(path, &bytes, 1024 * 1024).unwrap();
}

pub(super) fn register_test_openrouter_endpoint(catalog: &mut ModelCatalog) {
    let credential = crate::providers::EnvironmentCredential::for_test(
        "OPENROUTER_API_KEY",
        "test-openrouter-key",
    );
    crate::providers::register_environment_endpoints(
        catalog,
        openrouter_declaration(),
        &credential,
        PROVIDER_RESPONSE_HEADER_TIMEOUT,
    )
    .unwrap();
}

pub(super) fn fresh_app(directory: &std::path::Path) -> App {
    let boot = bootstrap(config(directory, Some("gpt-4o-mini"))).unwrap();
    let launch = resolve_launch_print(&boot, "test-session").unwrap();
    build_app(boot, launch, "system".into()).unwrap()
}

pub(super) fn thinking_hotfix_fixture() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../../fixtures/providers/thinking-hotfix.json"
    ))
    .unwrap()
}

pub(super) fn metadata_fixture_catalog(
    declaration: &ProviderDeclaration,
    base_url: &str,
) -> ModelCatalog {
    let mut catalog = ModelCatalog::default();
    let route = declaration.inventory_route().unwrap();
    catalog
        .register_endpoint(Endpoint {
            id: EndpointId(route.endpoint_id.into()),
            base_url: url::Url::parse(base_url).unwrap(),
            auth: Auth::None,
            default_headers: Default::default(),
            transport: EndpointTransport::Http,
            runtime: Default::default(),
            timeout: Duration::from_secs(5),
        })
        .unwrap();
    catalog
}
