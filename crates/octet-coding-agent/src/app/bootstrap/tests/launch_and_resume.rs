//! Launch and resume
//!
//! Part of the `app::bootstrap` test suite; see `tests/mod.rs` for how the
//! suite is organised and why it is split this way.
use super::support::*;
use super::*;

#[test]
fn deepseek_v4_pro_is_registered_as_openai_chat_with_env_auth() {
    let directory = tempfile::tempdir().unwrap();
    let boot = bootstrap(config(directory.path(), Some(DEEPSEEK_MODEL_ID))).unwrap();
    let model = boot
        .catalog
        .resolve(&ModelId(DEEPSEEK_MODEL_ID.into()))
        .unwrap();
    assert_eq!(model.spec.protocol, Protocol::OpenAiChat);
    assert_eq!(
        model.endpoint.id.0,
        crate::providers::DEEPSEEK.routes[0].endpoint_id
    );
    assert_eq!(
        model.spec.api_name,
        std::env::var("OCTET_DEEPSEEK_MODEL").unwrap_or_else(|_| DEEPSEEK_MODEL_ID.into())
    );
    assert!(model.spec.capabilities.tools);
    assert!(matches!(
        model.spec.capabilities.reasoning.as_ref(),
        Some(ReasoningCapability {
            options: None,
            control: ReasoningControl::Effort,
            exposes_text: true,
            openai_chat_mode: OpenAiChatReasoningMode::DeepSeekThinking,
            ..
        })
    ));
    assert_eq!(
        model.spec.limits.context_window,
        std::env::var("OCTET_DEEPSEEK_CONTEXT_WINDOW")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(DEEPSEEK_DEFAULT_CONTEXT_WINDOW)
    );
    assert_eq!(
        model.spec.limits.max_output_tokens,
        std::env::var("OCTET_DEEPSEEK_MAX_OUTPUT_TOKENS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(DEEPSEEK_DEFAULT_MAX_OUTPUT_TOKENS)
    );
}

#[test]
fn first_launch_uses_custom_server_reasoning_default() {
    let directory = tempfile::tempdir().unwrap();
    let mut boot = bootstrap(config(directory.path(), Some("custom/onboarding/probe"))).unwrap();
    let mut spec = (*boot
        .catalog
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap()
        .spec)
        .clone();
    spec.id = ModelId("custom/onboarding/probe".into());
    spec.capabilities.reasoning = custom_reasoning_capability(&crate::auth::custom::CustomModel {
        reasoning: true,
        reasoning_values: vec!["none".into(), "default".into()],
        reasoning_default: "default".into(),
        ..Default::default()
    });
    boot.catalog.register_model(spec).unwrap();

    let launch = resolve_launch_print(&boot, "first-run").unwrap();
    let app = build_app(boot, launch, "system".into()).unwrap();
    assert_eq!(app.reasoning, ReasoningConfig::On);
    assert_eq!(
        persisted_session_config(app.agent.session())
            .unwrap()
            .reasoning,
        Some(ReasoningConfig::On)
    );
}

#[test]
fn deepseek_v4_pro_accepts_high_reasoning_at_startup() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = config(directory.path(), Some(DEEPSEEK_MODEL_ID));
    config.reasoning = Some(ReasoningConfig::Effort(octet_ai::ReasoningEffort::High));
    let boot = bootstrap(config).unwrap();
    let launch = resolve_launch_print(&boot, "test-session").unwrap();
    let app = build_app(boot, launch, "system".into()).unwrap();
    assert_eq!(
        app.reasoning,
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)
    );
}

#[test]
fn print_launch_errors_without_model() {
    let directory = tempfile::tempdir().unwrap();
    let boot = bootstrap(config(directory.path(), None)).unwrap();
    let error = resolve_launch_print(&boot, "2026-07-12T00-00-00Z").unwrap_err();
    assert!(error.to_string().contains("no model configured"));
}

#[test]
fn interactive_launch_replaces_an_unavailable_persisted_model() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = config(directory.path(), None);
    config.model_explicit = false;
    let boot = bootstrap(config).unwrap();
    let unavailable = ModelId("provider-model-without-a-credential".into());

    assert!(should_pick_interactive_model(
        &boot.config,
        &boot.catalog,
        Some(&unavailable),
    ));
    assert!(!should_pick_interactive_model(
        &boot.config,
        &boot.catalog,
        Some(&ModelId("gpt-4o-mini".into())),
    ));

    let mut explicit = boot.config.clone();
    explicit.model_explicit = true;
    assert!(!should_pick_interactive_model(
        &explicit,
        &boot.catalog,
        Some(&unavailable),
    ));
}

#[test]
fn print_launch_creates_new_session_path_with_model() {
    let directory = tempfile::tempdir().unwrap();
    let boot = bootstrap(config(directory.path(), Some("gpt-4o-mini"))).unwrap();
    let launch = resolve_launch_print(&boot, "2026-07-12T00-00-00Z").unwrap();
    assert_eq!(launch.model.0, "gpt-4o-mini");
    assert!(matches!(launch.session, SessionSelection::CreateNew(_)));
}

#[test]
fn print_resume_restores_session_model_and_reasoning_unless_cli_overrides() {
    let directory = tempfile::tempdir().unwrap();
    let mut process_config = config(directory.path(), None);
    process_config.resume = ResumeSelector::Continue;
    process_config.model_explicit = false;
    process_config.reasoning_explicit = false;
    let boot = bootstrap(process_config).unwrap();
    let path = boot.sessions.new_path("2026-07-12T00-00-00Z");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Config {
            model: Some("gpt-5.4-mini-responses".to_string()),
            reasoning: Some("high".to_string()),
            reasoning_mode: None,
        })
        .unwrap();
    session
        .append(EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text("resumable prompt".into())],
            },
        )))
        .unwrap();
    drop(session);

    let launch = resolve_launch_print(&boot, "unused").unwrap();
    assert_eq!(launch.model.0, "gpt-5.4-mini-responses");
    assert_eq!(
        launch.reasoning,
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)
    );

    let mut overridden = config(directory.path(), Some("gpt-4o-mini"));
    overridden.resume = ResumeSelector::Continue;
    overridden.model_explicit = true;
    overridden.reasoning = Some(ReasoningConfig::Off);
    overridden.reasoning_explicit = true;
    let launch = resolve_launch_print(&bootstrap(overridden).unwrap(), "unused").unwrap();
    assert_eq!(launch.model.0, "gpt-4o-mini");
    assert_eq!(launch.reasoning, ReasoningConfig::Off);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]

async fn resumed_ultra_override_binds_observation_before_selection() {
    use octet_ai::{ReasoningEffort, ResponsesFeatures};

    let directory = tempfile::tempdir().unwrap();
    let model_id = "fixture-ultra-resume";
    let ultra = ReasoningConfig::Effort(ReasoningEffort::Ultra);
    let low = ReasoningConfig::Effort(ReasoningEffort::Low);
    let mut process_config = config(directory.path(), Some(model_id));
    process_config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    // Extension roots contain named bundle directories; discovery scans their direct children.
    let extension_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../extensions")
        .canonicalize()
        .unwrap();
    process_config.extension_paths = vec![extension_root.clone()];
    process_config.enabled_extensions = vec!["octet-subagents".into()];
    process_config.invocation_trusted_extensions = vec!["octet-subagents".into()];
    process_config.resume = ResumeSelector::Continue;
    process_config.reasoning = Some(ultra.clone());
    process_config.reasoning_explicit = true;
    let mut boot = bootstrap(process_config).unwrap();
    let mut model = boot
        .catalog
        .resolve(&ModelId("gpt-6-astra".into()))
        .unwrap();
    let features = ResponsesFeatures {
        reasoning_effort_updates: true,
        ..Default::default()
    };
    let endpoint = Arc::make_mut(&mut model.endpoint);
    endpoint.id = octet_ai::EndpointId("fixture-ultra-resume".into());
    endpoint.runtime.responses_features = features;
    boot.catalog.register_endpoint(endpoint.clone()).unwrap();
    let spec = Arc::make_mut(&mut model.spec);
    spec.id = ModelId(model_id.into());
    spec.endpoint = model.endpoint.id.clone();
    spec.capabilities.responses_features = features;
    spec.capabilities.agent_delegation = Some(octet_ai::AgentDelegation::V2);
    let capability = spec.capabilities.reasoning.as_mut().unwrap();
    capability.max_effort = ReasoningEffort::Ultra;
    capability.options = Some(octet_ai::types::ReasoningOptions {
        values: vec!["none".into(), "low".into(), "max".into(), "ultra".into()],
        default: Some("low".into()),
    });
    boot.catalog.register_model(spec.clone()).unwrap();
    let path = boot.sessions.new_path("ultra-resume");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Config {
            model: Some(model_id.into()),
            reasoning: Some("low".into()),
            reasoning_mode: None,
        })
        .unwrap();
    let history = session
        .append(EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text("preserve history".into())],
            },
        )))
        .unwrap();
    session
        .append(EntryValue::ResponsesReasoning {
            endpoint: model.endpoint.id.clone(),
            model: model.spec.id.clone(),
            baseline: low.clone(),
            update: None,
        })
        .unwrap();
    drop(session);

    let launch = resolve_launch_print(&boot, "unused").unwrap();
    assert_eq!(launch.reasoning, ultra);
    let mut app = build_app(boot, launch, "system".into()).unwrap();
    assert!(
        app.executable_extensions.has_agent_session_service(),
        "{}",
        app.executable_extensions.inspect_text()
    );
    // An installed global copy must not hide a broken source-fixture root.
    let subagents = app
        .executable_extensions
        .summaries()
        .into_iter()
        .find(|summary| summary.name == "octet-subagents")
        .expect("the source bundle is discovered");
    assert_eq!(
        subagents.manifest_path,
        extension_root.join("octet-subagents/extension.toml")
    );
    assert!(app.agent.delegation_team_directory().is_some());
    assert_eq!(app.agent.reasoning(), &ultra);
    assert_eq!(app.reasoning, ultra);
    assert_eq!(app.agent.session().path(), path);
    assert!(app.agent.session().entry(&history).is_some());
    assert_eq!(
        app.agent
            .session()
            .responses_reasoning(&model.endpoint.id, &model.spec.id)
            .unwrap(),
        Some((ultra.clone(), ultra.clone()))
    );

    // Exercise the consuming rebuild path independently of launch restoration.
    // Its existing durable pin is lower than the explicit requested override.
    app.agent.set_reasoning(low.clone()).unwrap();
    app.reasoning = low.clone();
    app.config.reasoning = Some(low);
    let mut app = rebuild_app(app, None, Some(ultra.clone()), None, None).unwrap();
    assert!(
        app.executable_extensions.has_agent_session_service(),
        "{}",
        app.executable_extensions.inspect_text()
    );
    assert!(app.agent.delegation_team_directory().is_some());
    assert_eq!(app.agent.reasoning(), &ultra);
    assert_eq!(app.reasoning, ultra);
    assert_eq!(app.agent.session().path(), path);
    assert!(app.agent.session().entry(&history).is_some());
    assert_eq!(
        app.agent
            .session()
            .responses_reasoning(&model.endpoint.id, &model.spec.id)
            .unwrap(),
        Some((ultra.clone(), ultra))
    );
    app.executable_extensions.shutdown_blocking();
}

#[test]
fn explicit_reasoning_clears_a_persisted_legacy_pro_mode() {
    let directory = tempfile::tempdir().unwrap();
    let mut process_config = config(directory.path(), Some("gpt-5.4-mini-responses"));
    process_config.resume = ResumeSelector::Continue;
    process_config.reasoning = Some(ReasoningConfig::Effort(octet_ai::ReasoningEffort::High));
    process_config.reasoning_explicit = true;
    process_config.reasoning_mode_explicit = false;
    let boot = bootstrap(process_config).unwrap();
    let path = boot.sessions.new_path("2026-07-12T00-00-00Z");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Config {
            model: Some("gpt-5.4-mini-responses".to_string()),
            reasoning: Some("max".to_string()),
            reasoning_mode: Some("pro".to_string()),
        })
        .unwrap();
    session
        .append(EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text("resumable prompt".into())],
            },
        )))
        .unwrap();
    drop(session);

    let launch = resolve_launch_print(&boot, "unused").unwrap();
    assert_eq!(
        launch.reasoning,
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)
    );
    assert_eq!(launch.reasoning_mode, ReasoningMode::Standard);
}

#[test]
fn launch_configuration_parts_returns_the_preopened_resume_session() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = config(directory.path(), None);
    config.model_explicit = false;
    config.reasoning_explicit = false;
    let path = directory.path().join("preopened.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Config {
            model: Some("gpt-5.4-mini-responses".to_owned()),
            reasoning: Some("high".to_owned()),
            reasoning_mode: None,
        })
        .unwrap();
    drop(session);

    let (
        prepared,
        LaunchConfiguration {
            model,
            reasoning,
            reasoning_mode,
        },
        persisted,
    ) = launch_configuration_parts(&config, &SessionSelection::OpenExisting(path.clone())).unwrap();

    assert_eq!(prepared.as_ref().map(Session::path), Some(path.as_path()));
    assert_eq!(
        persisted.model,
        Some(ModelId("gpt-5.4-mini-responses".into()))
    );
    assert_eq!(model, Some(ModelId("gpt-5.4-mini-responses".into())));
    assert_eq!(
        reasoning,
        Some(ReasoningConfig::Effort(octet_ai::ReasoningEffort::High))
    );
    assert_eq!(reasoning_mode, ReasoningMode::Standard);
}

#[test]
fn prepared_configuration_skips_a_second_scan_and_preserves_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("resumed.jsonl");
    let mut session = Session::create(&path).unwrap();
    let model = ModelId("gpt-4o-mini".into());
    let high = ReasoningConfig::Effort(octet_ai::ReasoningEffort::High);
    append_config_if_changed(&mut session, None, &model, &high, ReasoningMode::Standard).unwrap();
    let cached = persisted_session_config(&session).unwrap();
    let count = session.entries().len();
    append_config_if_changed(
        &mut session,
        Some(&cached),
        &model,
        &high,
        ReasoningMode::Standard,
    )
    .unwrap();
    assert_eq!(session.entries().len(), count);
    append_config_if_changed(
        &mut session,
        Some(&cached),
        &model,
        &ReasoningConfig::Off,
        ReasoningMode::Standard,
    )
    .unwrap();
    assert_eq!(session.entries().len(), count + 1);
    assert_eq!(
        persisted_session_config(&session).unwrap().reasoning,
        Some(ReasoningConfig::Off)
    );
}
