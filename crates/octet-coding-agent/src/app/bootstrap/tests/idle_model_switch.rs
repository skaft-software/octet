//! Idle selection preservation, route validation, and real adapter attribution.
use super::support::*;
use super::*;

fn pi_switch_app(directory: &Path) -> App {
    let adapter = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/octet-pi-compat");
    let entry = directory.join("probe.mjs");
    let loads = directory.join("loads.jsonl");
    std::fs::write(
        &entry,
        format!(
            r#"import {{ appendFileSync }} from 'node:fs';
export default function (pi) {{
  appendFileSync({loads}, JSON.stringify({{pid: process.pid}}) + '\n');
  pi.on('session_start', () => {{}});
  pi.on('model_select', () => {{}});
}}
"#,
            loads = serde_json::to_string(&loads).unwrap(),
        ),
    )
    .unwrap();
    let root = directory.join("extensions");
    let captured = std::process::Command::new("node")
        .arg(adapter.join("configure.mjs"))
        .args(["--reviewed", "--output"])
        .arg(root.join("octet-pi-compat"))
        .arg(entry)
        .env("PI_OFFLINE", "1")
        .env("OCTET_PI_AGENT_DIR", directory.join("pi-agent"))
        .current_dir(directory)
        .output()
        .expect("Node and the adapter's locked dependencies are required");
    assert!(
        captured.status.success(),
        "{}",
        String::from_utf8_lossy(&captured.stderr)
    );
    std::fs::write(loads, "").unwrap();
    let mut config = config(directory, Some("gpt-4o-mini"));
    config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    config.extension_paths = vec![root];
    config.enabled_extensions = vec!["octet-pi-compat".into()];
    config.invocation_trusted_extensions = config.enabled_extensions.clone();
    let boot = bootstrap(config).unwrap();
    let launch = resolve_launch_print(&boot, "idle-switch").unwrap();
    let mut app = build_app_with_resource_consumer(boot, launch, "system".into()).unwrap();
    assert!(
        app.executable_extensions
            .summaries()
            .iter()
            .any(|s| s.running),
        "{}",
        app.executable_extensions.inspect_text()
    );
    app
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_model_switch_does_not_restart_the_real_adapter_factory() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = pi_switch_app(directory.path());
    let skills = Arc::clone(&app.skills);
    let prompts = Arc::clone(&app.prompts);
    let session = app.agent.session().path().to_owned();
    let initial_load = std::fs::read_to_string(directory.path().join("loads.jsonl")).unwrap();
    assert_eq!(initial_load.lines().count(), 1);
    for id in ["gpt-audio-1.5", "gpt-4o-mini", "gpt-audio-1.5"] {
        app = crate::app::apply_reconfig(app, crate::app::Reconfig::Model(ModelId(id.into())))
            .unwrap();
        assert_eq!(app.model.spec.id.0, id);
        assert_eq!(app.agent.model().spec.id.0, id);
        assert_eq!(app.agent.session().path(), session);
        assert!(Arc::ptr_eq(&skills, &app.skills));
        assert!(Arc::ptr_eq(&prompts, &app.prompts));
        assert_eq!(
            std::fs::read_to_string(directory.path().join("loads.jsonl")).unwrap(),
            initial_load,
            "a model selection must not rerun any factory or replace its process"
        );
    }
    app.executable_extensions.shutdown_blocking();
}

#[test]
fn idle_model_switch_updates_output_compaction_config_and_durable_selection() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = fresh_app(directory.path());
    let mut target = (*app.model.spec).clone();
    target.id = ModelId("idle-small-window".into());
    target.limits.context_window = 64_000;
    target.limits.max_output_tokens = 2_048;
    let id = target.id.clone();
    app.catalog.register_model(target).unwrap();
    app.config.compaction.compact_model = Some(app.model.spec.id.clone());
    app.config.compaction.max_active_tokens = Some(8_000);
    app.config.compaction.keep_recent_tokens = 123;
    app.user_keybindings
        .insert("send".into(), vec!["ctrl+x".into()]);
    let skills = Arc::clone(&app.skills);
    let prompts = Arc::clone(&app.prompts);
    let goal_session_id = app.goal_session_id.clone();
    let history = app
        .agent
        .session_mut()
        .append(EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text("preserve history".into())],
            },
        )))
        .unwrap();
    let app = crate::app::apply_reconfig(app, crate::app::Reconfig::Model(id.clone())).unwrap();
    assert!(Arc::ptr_eq(&skills, &app.skills));
    assert!(Arc::ptr_eq(&prompts, &app.prompts));
    assert_eq!(app.goal_session_id, goal_session_id);
    assert!(app.agent.session().entry(&history).is_some());
    assert!(app.user_keybindings.contains_key("send"));
    assert_eq!(app.agent.max_output_tokens(), 2_048);
    assert_eq!(app.agent.compaction_token_policy(), (true, 0.125, 123));
    assert_eq!(
        app.agent.compaction_model().unwrap().spec.id.0,
        "gpt-4o-mini"
    );
    assert_eq!(app.config.model, Some(id.clone()));
    assert_eq!(app.config.reasoning, Some(app.reasoning.clone()));
    let persisted = persisted_session_config(app.agent.session()).unwrap();
    assert_eq!(persisted.model, Some(id));
    assert_eq!(persisted.reasoning, Some(app.reasoning.clone()));
}

#[tokio::test]
async fn idle_model_switch_updates_prompt_metadata_without_a_provider_request() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = fresh_app(directory.path());
    let id = ModelId("claude-sonnet-4-5".into());
    assert!(crate::app::try_apply_model_reconfig(&mut app, &id).unwrap());
    let source = crate::tui::theme::model_lab(&app.model).key().to_owned();
    let color = crate::tui::theme::prompt_color_for_model(&app.model);
    // A Run persists the prompt immediately; its unpolled stream makes no
    // provider request. This inspects the metadata used by the next real turn.
    drop(app.agent.prompt("metadata only").await.unwrap());
    let metadata = app
        .agent
        .session()
        .entries()
        .last()
        .unwrap()
        .metadata
        .as_ref()
        .unwrap();
    assert_eq!(metadata.prompt_model.as_ref(), Some(&id));
    assert_eq!(metadata.prompt_model_source.as_ref(), Some(&source));
    assert_eq!(metadata.prompt_color.as_ref(), Some(&color));
}

#[test]
fn idle_model_switch_keeps_the_rebuild_fallback_for_changed_tool_capabilities() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = fresh_app(directory.path());
    let mut target = (*app.model.spec).clone();
    target.id = ModelId("idle-without-tools".into());
    target.capabilities.tools = false;
    let id = target.id.clone();
    app.catalog.register_model(target).unwrap();
    let skills = Arc::clone(&app.skills);
    assert!(!crate::app::try_apply_model_reconfig(&mut app, &id).unwrap());
    assert_eq!(app.model.spec.id.0, "gpt-4o-mini");
    let app = crate::app::apply_reconfig(app, crate::app::Reconfig::Model(id.clone())).unwrap();
    assert_eq!(app.model.spec.id, id);
    assert!(
        !Arc::ptr_eq(&skills, &app.skills),
        "incompatible tools require a rebuilt host"
    );
    assert!(app.agent.registered_tool_names().is_empty());
}

#[test]
fn idle_model_switch_rejects_an_invalid_native_compaction_route_without_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = config(directory.path(), Some("gpt-5.4-mini-responses"));
    config.compaction.mode = CompactionMode::NativeResponses;
    let boot = bootstrap(config).unwrap();
    let launch = resolve_launch_print(&boot, "native-idle-switch").unwrap();
    let mut app = build_app(boot, launch, "system".into()).unwrap();
    let head = app.agent.session().head_ref().cloned();
    let error =
        crate::app::try_apply_model_reconfig(&mut app, &ModelId("gpt-4o-mini".into())).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("requires an OpenAI Responses route"),
        "{error:#}"
    );
    assert_eq!(app.model.spec.id.0, "gpt-5.4-mini-responses");
    assert_eq!(app.agent.model().spec.id, app.model.spec.id);
    assert_eq!(app.agent.session().head_ref(), head.as_ref());
}

#[test]
fn idle_model_switch_revalidates_explicit_tool_policy_before_committing() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = fresh_app(directory.path());
    app.config.tools = crate::config::ToolPolicy::only(["missing_tool".to_owned()]).unwrap();
    let head = app.agent.session().head_ref().cloned();
    let error = crate::app::try_apply_model_reconfig(&mut app, &ModelId("gpt-audio-1.5".into()))
        .unwrap_err();
    assert!(error.to_string().contains("missing_tool"), "{error:#}");
    assert_eq!(app.model.spec.id.0, "gpt-4o-mini");
    assert_eq!(app.agent.session().head_ref(), head.as_ref());
}

#[test]
fn idle_model_switch_applies_scoped_effort_and_preserves_its_portable_level() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = fresh_app(directory.path());
    let id = ModelId("gpt-5.4-mini-responses".into());
    app.model_scope = Some(vec![crate::cli::parity::ScopedModel {
        id: id.clone(),
        pattern: id.0.clone(),
        reasoning: Some("high".into()),
    }]);
    assert!(crate::app::try_apply_model_reconfig(&mut app, &id).unwrap());
    assert_eq!(
        app.reasoning,
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)
    );
    assert_eq!(app.agent.reasoning(), &app.reasoning);
    assert_eq!(app.config.reasoning, Some(app.reasoning.clone()));
    assert!(
        crate::app::try_apply_model_reconfig(&mut app, &ModelId("claude-sonnet-4-5".into()))
            .unwrap()
    );
    assert_eq!(
        crate::app::level_from_reasoning(&app.reasoning, &app.model).unwrap(),
        crate::config::ThinkingLevel::High
    );
    assert_eq!(app.agent.reasoning(), &app.reasoning);
}

#[test]
fn idle_model_switch_does_not_admit_ultra_without_bound_observation() {
    use octet_ai::ReasoningEffort;
    let directory = tempfile::tempdir().unwrap();
    let mut app = fresh_app(directory.path());
    let mut spec = (*app
        .catalog
        .resolve(&ModelId("gpt-6-astra".into()))
        .unwrap()
        .spec)
        .clone();
    let id = ModelId("idle-ultra-fixture".into());
    spec.id = id.clone();
    spec.capabilities.agent_delegation = Some(octet_ai::AgentDelegation::V2);
    let capability = spec.capabilities.reasoning.as_mut().unwrap();
    capability.max_effort = ReasoningEffort::Ultra;
    capability.options = Some(octet_ai::types::ReasoningOptions {
        values: vec!["none".into(), "low".into(), "max".into(), "ultra".into()],
        default: Some("low".into()),
    });
    app.catalog.register_model(spec).unwrap();
    app.model_scope = Some(vec![crate::cli::parity::ScopedModel {
        id: id.clone(),
        pattern: id.0.clone(),
        reasoning: Some("ultra".into()),
    }]);
    assert!(!app.subagents_available());
    assert!(crate::app::try_apply_model_reconfig(&mut app, &id).unwrap());
    assert_eq!(app.reasoning, ReasoningConfig::Effort(ReasoningEffort::Max));
    assert_eq!(app.agent.reasoning(), &app.reasoning);
    assert_eq!(app.reasoning_mode, ReasoningMode::Standard);
}

#[test]
fn idle_model_switch_retains_fast_only_on_supported_routes() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = fresh_app(directory.path());
    let base = app
        .catalog
        .resolve(&ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    let mut endpoint = (*base.endpoint).clone();
    endpoint.id = EndpointId("idle-fast-fixture".into());
    endpoint.runtime.responses_profile = octet_ai::ResponsesRuntimeProfile::Codex;
    app.catalog.register_endpoint(endpoint.clone()).unwrap();
    for id in ["idle-fast-a", "idle-fast-b"] {
        let mut spec = (*base.spec).clone();
        spec.id = ModelId(id.into());
        spec.endpoint = endpoint.id.clone();
        app.catalog.register_model(spec).unwrap();
    }
    assert!(
        crate::app::try_apply_model_reconfig(&mut app, &ModelId("idle-fast-a".into())).unwrap()
    );
    app.agent
        .set_service_tier(Some(octet_ai::ServiceTier::Priority))
        .unwrap();
    assert!(
        crate::app::try_apply_model_reconfig(&mut app, &ModelId("idle-fast-b".into())).unwrap()
    );
    assert_eq!(
        app.agent.service_tier(),
        Some(octet_ai::ServiceTier::Priority)
    );
    assert!(
        crate::app::try_apply_model_reconfig(&mut app, &ModelId("gpt-4o-mini".into())).unwrap()
    );
    assert_eq!(app.agent.service_tier(), None);
}

/// Diagnostic only: no wall-clock threshold, provider request, or real Pi setup.
/// Run identically before/after with OCTET_STARTUP_TRACE=1 and --nocapture.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "manual real-adapter timing profile; requires Node and npm ci"]
async fn profile_idle_model_switch() {
    octet_agent::extension_process::set_startup_trace_sink(|line| eprintln!("{line}"));
    let directory = tempfile::tempdir().unwrap();
    let mut app = pi_switch_app(directory.path());
    for id in ["gpt-audio-1.5", "gpt-4o-mini"]
        .into_iter()
        .cycle()
        .take(10)
    {
        startup_phase("model-switch.begin");
        let started = std::time::Instant::now();
        app = crate::app::apply_reconfig(app, crate::app::Reconfig::Model(ModelId(id.into())))
            .unwrap();
        startup_phase("model-switch.end");
        eprintln!("model-switch-sample: {}us", started.elapsed().as_micros());
        assert_eq!(app.model.spec.id.0, id);
    }
    let loads = std::fs::read_to_string(directory.path().join("loads.jsonl")).unwrap();
    eprintln!("model-switch-factory-loads: {}", loads.lines().count());
    app.executable_extensions.shutdown_blocking();
}
