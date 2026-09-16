//! Real product discovery binds two ordinary processes to one private bus.
#![cfg(unix)]
use super::*;
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn product_discovery_connects_isolated_peers_without_model_or_durable_events() {
    let root = tempfile::tempdir().unwrap();
    let extension_root = root.path().join("extensions");
    let fixture = include_str!("../../../octet-agent/tests/support/extension_bus_peer.py");
    for name in ["alpha", "beta"] {
        let directory = extension_root.join(name);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("peer.py"),
            fixture.replace("__EXTENSION_NAME__", name)
                .replace("__SDK_PATH__", &format!("{}/../../sdk/python", env!("CARGO_MANIFEST_DIR")))
                .replace(
                "\"name\": \"probe\"",
                &format!("\"name\": \"probe_{name}\""),
            ),
        )
        .unwrap();
        // The host runs entrypoint arguments from the workspace, not the
        // extension directory. Keep the script path bound to this fixture.
        let script_argument = serde_json::to_string(&directory.join("peer.py")).unwrap();
        std::fs::write(
            directory.join(EXTENSION_MANIFEST_FILENAME),
            format!(
                r#"name = "{name}"
version = "0.1.0"
api_version = "0.3"
[entrypoint]
command = "python3"
args = [{script_argument}]
[contributes]
tools = ["probe_{name}"]
"#
            ),
        )
        .unwrap();
    }
    let mut config =
        super::tests::executable_extension_config(root.path(), &extension_root, "alpha");
    config.enabled_extensions.push("beta".into());
    config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    config.sandbox.allow_process = true;
    config.sandbox.allow_shell = true;
    let session_path = root.path().join("session.jsonl");
    let session = Session::create(&session_path).unwrap();
    let before = std::fs::read(&session_path).unwrap();
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    let sessions = SessionStore::new(&config.session_dir, root.path());
    let mut host = ExtensionHost::new();
    let mut extensions = ExecutableExtensions::discover_and_start(
        &config,
        &session,
        &model,
        &ReasoningConfig::Off,
        &sessions,
        &mut host,
    );
    assert_eq!(
        extensions.processes.len(),
        2,
        "{}\n{}",
        extensions.status_summary(),
        extensions
            .diagnostics
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    );
    let alpha = extensions
        .processes
        .iter()
        .find(|process| process.descriptor().manifest.name == "alpha")
        .unwrap()
        .clone();
    let beta = extensions
        .processes
        .iter()
        .find(|process| process.descriptor().manifest.name == "beta")
        .unwrap()
        .clone();
    for process in [&alpha, &beta] {
        assert!(process.negotiated_features().contains("event_bus"));
    }
    for (process, method, params) in [
        (
            &alpha,
            "bus/declare",
            json!({"topic":"bus.alpha.status","fields":[{"name":"summary","kind":"string","required":true,"max_bytes":128,"values":[]}]}),
        ),
        (&beta, "bus/subscribe", json!({"topic":"bus.alpha.status"})),
        (
            &alpha,
            "bus/publish",
            json!({"topic":"bus.alpha.status","payload":{"summary":"inert-data"}}),
        ),
    ] {
        let name = format!("probe_{}", process.descriptor().manifest.name);
        let output = process
            .call_tool(
                &name,
                json!({"method":method,"params":params}),
                process.current_context(),
            )
            .await
            .unwrap();
        assert!(output.structured_content.unwrap().get("result").is_some());
    }
    let output = beta
        .call_tool(
            "probe_beta",
            json!({"method":"events","count":1}),
            beta.current_context(),
        )
        .await
        .unwrap();
    assert_eq!(
        output.structured_content.unwrap()[0]["payload"],
        json!({"summary":"inert-data"})
    );
    assert!(
        extensions.drain_events().is_empty(),
        "bus events are not general host observations"
    );
    assert_eq!(
        std::fs::read(&session_path).unwrap(),
        before,
        "bus events must not be persisted"
    );
    extensions.shutdown().await;
}
