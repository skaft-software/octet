//! Tool registry
//!
//! Part of the `app::bootstrap` test suite; see `tests/mod.rs` for how the
//! suite is organised and why it is split this way.
use super::support::*;
use super::*;

#[test]
fn disabled_tools_are_absent_from_both_schema_and_execution_registry() {
    let directory = tempfile::tempdir().unwrap();
    let skills: Arc<dyn SkillRegistry> =
        Arc::new(FileSystemSkillRegistry::new(directory.path().to_owned(), vec![], false).unwrap());
    let mut config = config(directory.path(), Some("gpt-4o-mini"));
    config.sandbox.allow_edit = false;
    config.sandbox.allow_write = false;
    config.sandbox.allow_process = false;
    config.sandbox.allow_shell = false;
    let extensions = configured_test_extensions(skills, &config);
    let names = extensions
        .tool_definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["read"]);
}

#[test]
fn legacy_skill_tools_are_not_registered_by_default() {
    let directory = tempfile::tempdir().unwrap();
    let skills: Arc<dyn SkillRegistry> =
        Arc::new(FileSystemSkillRegistry::new(directory.path().to_owned(), vec![], true).unwrap());
    let mut config = config(directory.path(), Some("gpt-4o-mini"));
    config.tools =
        crate::config::ToolPolicy::only(["read".to_owned(), "load_skill".to_owned()]).unwrap();
    config.sandbox.allow_edit = false;
    let extensions = configured_test_extensions(skills, &config);
    let registered = extensions
        .tool_definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect::<Vec<_>>();
    assert_eq!(registered, vec!["read"]);
}

#[test]
fn initial_build_ignores_legacy_active_skill_tool_requirements() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("active-skill.jsonl");
    let mut session = Session::create(&path).unwrap();
    append_active_skill(&mut session, "editor", &["edit"]);
    drop(session);

    let mut config = config(directory.path(), Some("gpt-4o-mini"));
    config.tools = crate::config::ToolPolicy::only(["read".to_owned()]).unwrap();
    let boot = bootstrap(config).unwrap();
    let app = build_app(
        boot,
        LaunchSelection {
            model: ModelId("gpt-4o-mini".into()),
            session: SessionSelection::OpenExisting(path),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: octet_ai::ReasoningMode::Standard,
        },
        "system".into(),
    )
    .unwrap();
    assert!(!app.system.contains("test instructions"));
}

#[test]
fn rebuild_ignores_legacy_active_skill_tool_requirements() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("active-skill-rebuild.jsonl");
    let mut session = Session::create(&path).unwrap();
    append_active_skill(&mut session, "editor", &["edit"]);
    drop(session);

    let config = config(directory.path(), Some("gpt-4o-mini"));
    let boot = bootstrap(config).unwrap();
    let mut app = build_app(
        boot,
        LaunchSelection {
            model: ModelId("gpt-4o-mini".into()),
            session: SessionSelection::OpenExisting(path),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: octet_ai::ReasoningMode::Standard,
        },
        "system".into(),
    )
    .unwrap();
    app.config.tools = crate::config::ToolPolicy::only(["read".to_owned()]).unwrap();

    let rebuilt = rebuild_app(app, None, None, None, None).unwrap();
    assert!(!rebuilt.system.contains("test instructions"));
}

#[test]
fn explicit_unavailable_tools_report_final_available_names_and_policy_gates() {
    let directory = tempfile::tempdir().unwrap();
    let skills: Arc<dyn SkillRegistry> =
        Arc::new(FileSystemSkillRegistry::new(directory.path().to_owned(), vec![], false).unwrap());
    let mut config = config(directory.path(), Some("gpt-4o-mini"));
    config.tools = crate::config::ToolPolicy::only([
        "read".to_owned(),
        "edit".to_owned(),
        "missing-extension".to_owned(),
    ])
    .unwrap();
    config.tools.exclude("edit").unwrap();
    config.sandbox.allow_edit = false;
    let extensions = configured_test_extensions(skills, &config);
    let boot = bootstrap(config.clone()).unwrap();
    let model = boot
        .catalog
        .resolve(config.model.as_ref().unwrap())
        .unwrap();

    let error =
        validate_explicit_tool_policy(&config, &extensions.tool_definitions(), &model, false)
            .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("edit, missing-extension"), "{message}");
    assert!(
        message.contains("allowlists, sandbox gates, and extension registration"),
        "{message}"
    );
    assert!(message.contains("available tools: read"), "{message}");
    let mut dynamic_config = config.clone();
    dynamic_config.tools =
        crate::config::ToolPolicy::only(["read".to_owned(), "missing-extension".to_owned()])
            .unwrap();
    validate_explicit_tool_policy(
        &dynamic_config,
        &extensions.tool_definitions(),
        &model,
        true,
    )
    .expect("a negotiated live catalog may publish explicitly allowed names later");
}

#[test]
fn model_without_tool_capability_gets_no_default_surface_and_rejects_explicit_tools() {
    let directory = tempfile::tempdir().unwrap();
    let mut default_config = config(directory.path(), Some("gpt-4o-mini"));
    let boot = bootstrap(default_config.clone()).unwrap();
    let resolved = boot
        .catalog
        .resolve(default_config.model.as_ref().unwrap())
        .unwrap();
    let mut spec = (*resolved.spec).clone();
    spec.capabilities.tools = false;
    spec.capabilities.parallel_tool_calls = false;
    let model = Model {
        spec: Arc::new(spec),
        endpoint: resolved.endpoint,
    };
    let session = Session::create(directory.path().join("no-tools-default.jsonl")).unwrap();
    let (extensions, _) = configured_extensions(
        &default_config,
        &session,
        &model,
        &ReasoningConfig::Off,
        &boot.sessions,
    )
    .unwrap();
    assert!(extensions.tool_definitions().is_empty());
    validate_explicit_tool_policy(
        &default_config,
        &extensions.tool_definitions(),
        &model,
        false,
    )
    .unwrap();

    default_config.tools = crate::config::ToolPolicy::only(["read".to_owned()]).unwrap();
    let explicit_session =
        Session::create(directory.path().join("no-tools-explicit.jsonl")).unwrap();
    let (extensions, _) = configured_extensions(
        &default_config,
        &explicit_session,
        &model,
        &ReasoningConfig::Off,
        &boot.sessions,
    )
    .unwrap();
    let error = validate_explicit_tool_policy(
        &default_config,
        &extensions.tool_definitions(),
        &model,
        false,
    )
    .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("gpt-4o-mini does not support tools"),
        "{message}"
    );
    assert!(
        message.contains("explicit tool policy requested: read"),
        "{message}"
    );
}

#[test]
fn initial_build_records_configuration_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let boot = bootstrap(config(directory.path(), Some("gpt-4o-mini"))).unwrap();
    let launch = resolve_launch_print(&boot, "initial-config").unwrap();
    let app = build_app(boot, launch, "system".to_string()).unwrap();
    assert_eq!(
        app.agent.completion_policy(),
        octet_agent::CompletionPolicy::Natural,
        "ordinary coding turns must not pay for a hidden second inference"
    );
    assert!(matches!(
        app.agent.session().entries().first().map(|entry| &entry.value),
        Some(EntryValue::Config {
            model: Some(model),
            reasoning: Some(reasoning),
            reasoning_mode: Some(reasoning_mode),
        }) if model == "gpt-4o-mini" && reasoning == "off" && reasoning_mode == "standard"
    ));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]

async fn unknown_api_03_last_initial_provider_model_preflights_restarts_and_reloads_with_fresh_routes(
) {
    let directory = tempfile::tempdir().unwrap();
    let extension_root = directory.path().join("extensions");
    let provider = extension_root.join("native-provider");
    std::fs::create_dir_all(&provider).unwrap();
    // This retained provider lifecycle is independent of the removed Pi bridge.
    std::fs::write(
        provider.join("provider.py"),
        r#"#!/usr/bin/env python3
import json
import sys
import time


def canonical(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def receive():
    line = sys.stdin.readline()
    assert line, "host closed stdin"
    value = json.loads(line)
    assert line.rstrip("\n") == canonical(value), line
    return value


def send(value):
    sys.stdout.write(canonical(value) + "\n")
    sys.stdout.flush()


def provider(provider_id, model_id):
    return {
        "provider": {
            "id": provider_id,
            "label": provider_id + " provider",
            "auth": {"kind": "none"},
        },
        "models": [{
            "id": model_id,
            "api_name": model_id,
            "protocol": "openai_chat",
            "context_window": 8192,
            "max_output_tokens": 1024,
            "capabilities": {
                "tools": False,
                "parallel_tool_calls": False,
                "structured_output": False,
                "reasoning": False,
            },
        }],
    }


def reverse_request(identifier, method, params):
    send({"jsonrpc": "2.0", "id": identifier, "method": method, "params": params})
    response = receive()
    assert response.get("id") == identifier and "result" in response, response
    return response["result"]


initialize = receive()
assert initialize["method"] == "initialize", initialize
contract = initialize["params"]["contract"]
provider_capabilities = {"provider_catalog", "provider_stream", "provider_auth"}
provider_methods = {
    "providers/complete",
    "providers/register",
    "providers/update",
    "providers/unregister",
    "provider/stream",
    "provider/event",
    "provider/cancel",
    "provider/auth/request",
    "provider/auth/revoke",
}
selection = {
    "schema": contract["schema"],
    "encoding": contract["encoding"],
    "capabilities": [
        capability
        for capability in contract["required_capabilities"] + contract["optional_capabilities"]
        if capability in contract["required_capabilities"] or capability in provider_capabilities
    ],
    "methods": [
        method
        for method in contract["required_methods"] + contract["optional_methods"]
        if method in contract["required_methods"] or method in provider_methods
    ],
    "limits": contract["limits"],
}
send({
    "jsonrpc": "2.0",
    "id": initialize["id"],
    "result": {
        "api_version": "0.3",
        "tools": [],
        "contract": selection,
    },
})
reverse_request("first", "providers/register", provider("fixture-first-provider", "fixture-first-model"))
time.sleep(0.075)
reverse_request("second", "providers/register", provider("fixture-second-provider", "fixture-second-model"))
send({"jsonrpc": "2.0", "method": "providers/complete", "params": {}})
shutdown = receive()
assert shutdown["method"] == "shutdown", shutdown
send({"jsonrpc": "2.0", "id": shutdown["id"], "result": {"terminal": "shutdown"}})
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(provider.join("provider.py"))
        .unwrap()
        .permissions();
    use std::os::unix::fs::PermissionsExt as _;
    permissions.set_mode(0o700);
    std::fs::set_permissions(provider.join("provider.py"), permissions).unwrap();
    std::fs::write(
        provider.join("extension.toml"),
        r#"name = "native-provider"
version = "0.3.0"
api_version = "0.3"

[entrypoint]
command = "provider.py"

[contributes]
providers = true
"#,
    )
    .unwrap();

    // The native fixture queues this declaration after the first provider.
    // Selecting it proves preflight waits for the owner's complete batch rather
    // than returning after the first reverse registration.
    let model_id = "fixture-second-provider/fixture-second-model";
    let mut config = config(directory.path(), Some(model_id));
    config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    config.extension_paths = vec![extension_root];
    config.enabled_extensions = vec!["native-provider".into()];
    config.invocation_trusted_extensions = vec!["native-provider".into()];

    let boot = bootstrap(config).unwrap();
    boot.preflight_extension_providers().unwrap();
    let preflight_catalog = boot.catalog_with_extension_providers();
    let preflight_status = boot
        .prestarted_extensions
        .borrow()
        .as_ref()
        .map(|(_, extensions)| (extensions.status_summary(), extensions.summaries()));
    assert!(
        preflight_catalog.resolve(&ModelId(model_id.into())).is_ok(),
        "preflight did not project {model_id}; startup: {preflight_status:#?}"
    );
    let launch = resolve_launch_print(&boot, "delayed-provider-model").unwrap();
    assert_eq!(launch.model.0, model_id);

    let mut app = build_app(boot, launch, "system".into()).unwrap();
    assert_eq!(app.model.spec.id.0, model_id);
    assert!(app.catalog.resolve(&ModelId(model_id.into())).is_ok());

    let old_endpoint = app.model.endpoint.id.clone();
    let reloads = app.executable_extensions.reload().await;
    assert!(
        reloads
            .iter()
            .any(|message| message.starts_with("reloaded native-provider (generation ")),
        "provider reload failed: {reloads:?}"
    );
    app.synchronize_extension_provider_catalog();
    let refreshed = app.catalog.resolve(&ModelId(model_id.into())).unwrap();
    assert_ne!(refreshed.endpoint.id, old_endpoint);
    let error = app
        .synchronize_extension_provider_catalog_for_request()
        .unwrap_err();
    assert!(error.to_string().contains("route changed"), "{error}");

    app.executable_extensions.shutdown().await;
}

#[test]
fn tool_schema_reserve_is_positive_and_deterministic() {
    let directory = tempfile::tempdir().unwrap();
    let skills: Arc<dyn SkillRegistry> =
        Arc::new(FileSystemSkillRegistry::new(directory.path().to_owned(), vec![], true).unwrap());
    let config = config(directory.path(), Some("gpt-4o-mini"));
    let extensions = configured_test_extensions(skills, &config);
    let definitions = extensions.tool_definitions();
    let names = definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["read", "edit", "write", "bash", "search"]);
    let default_reserve = tool_schema_reserve(&definitions);
    assert!(default_reserve > 0);
    assert_eq!(default_reserve, tool_schema_reserve(&definitions));

    let mut all_core = ExtensionHost::new();
    all_core.load(&CoreTools);
    let all_core_definitions = all_core.tool_definitions();
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut expected_all = vec!["read", "edit", "write", "bash", "search"];
    #[cfg(windows)]
    expected_all.push("powershell");
    assert_eq!(
        all_core_definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<Vec<_>>(),
        expected_all
    );
    // The reserve tracks the exact surface: dropping one schema drops its bytes,
    // while the registered core set is a superset of the coding default.
    let narrowed = definitions
        .iter()
        .filter(|definition| definition.name != "search")
        .cloned()
        .collect::<Vec<_>>();
    assert!(tool_schema_reserve(&narrowed) < default_reserve);
    assert!(tool_schema_reserve(&all_core_definitions) >= default_reserve);
}
