//! R13 owner retirement through the actual manager, without UI/lifecycle hooks.
use super::*;
use crate::extension_process::ResourceRef;
use serde_json::{json, Value};

#[tokio::test]
async fn resource_owner_release_and_drop_do_not_revive_shared_native_objects() {
    for drop_binding in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let mut selected = descriptor(
            temp.path(),
            "resources",
            ExtensionLifecycleProfile::WorkspaceService,
        );
        let directory = selected.manifest_path.parent().unwrap();
        write_script(
            &directory.join("runner.py"),
            include_str!("../../extension_process/resource_fixture.py"),
        );
        selected.manifest.api_version = "0.4".into();
        selected.manifest.entrypoint.command = "runner.py".into();
        selected
            .manifest
            .entrypoint
            .env
            .insert("HOME".into(), temp.path().to_string_lossy().into());
        selected.manifest.contributes.tools = vec!["create".into(), "use".into()];
        selected.manifest.contributes.notifications = true;
        fs::write(
            &selected.manifest_path,
            toml::to_string(&selected.manifest).unwrap(),
        )
        .unwrap();
        let reference_schema = json!({"type":"object","properties":{"$resource":{"type":"string"},"type":{"type":"string","const":"demo.Circuit"}},"required":["$resource","type"],"additionalProperties":false});
        let catalog = json!([
            {"name":"create","description":"Create","parameters":{"type":"object","additionalProperties":false},
             "output_schema":{"type":"object","properties":{"resource":reference_schema},"required":["resource"],"additionalProperties":false},
             "operation":{"id":"demo.create","resource_inputs":[],"resource_outputs":[{"path":"/resource","type":"demo.Circuit"}]}},
            {"name":"use","description":"Use","parameters":{"type":"object","properties":{"resource":reference_schema},"required":["resource"],"additionalProperties":false},
             "output_schema":{"type":"object","properties":{"count":{"type":"integer"}},"required":["count"],"additionalProperties":false},
             "operation":{"id":"demo.use","resource_inputs":[{"path":"/resource","type":"demo.Circuit","access":"exclusive"}],"resource_outputs":[]}}
        ]);
        fs::write(temp.path().join("catalog.json"), catalog.to_string()).unwrap();
        let manager =
            ExtensionRuntimeManager::new(ExtensionRuntimeDomain::ordinary(temp.path()).unwrap());
        manager
            .replace_catalog(ExtensionRuntimeCatalog::from_descriptors([selected]))
            .await;
        let binding = manager.bind_session("A").unwrap();
        let config = || {
            let mut config = ExtensionRuntimeConfig::new(temp.path());
            config.supervise = false;
            config.request_timeout = Duration::from_secs(5);
            config.shutdown_timeout = Duration::from_secs(1);
            config
        };
        let process = binding
            .activate("resources", config())
            .await
            .unwrap()
            .process;
        let output = process
            .call_tool(
                "create",
                json!({}),
                process.current_context_for_resource_owner("A"),
            )
            .await
            .unwrap();
        let reference: ResourceRef =
            serde_json::from_value(output.structured_content.unwrap()["resource"].clone()).unwrap();
        process.lookup_resource("A", &reference).unwrap();
        let generation = process.health_snapshot().generation;
        if drop_binding {
            drop(binding);
        } else {
            binding.release().await;
        }
        // Drop invalidation is synchronous even though fleet detachment is async.
        assert!(process.lookup_resource("A", &reference).is_err());
        assert!(
            process.is_running(),
            "workspace process survives owner retirement"
        );
        let b = manager.bind_session("B").unwrap();
        b.activate("resources", config()).await.unwrap();
        let a = manager.bind_session("A").unwrap();
        let rebound = a.activate("resources", config()).await.unwrap().process;
        assert_eq!(rebound.health_snapshot().generation, generation);
        let calls = || {
            let log = fs::read_to_string(temp.path().join("calls.jsonl")).unwrap();
            // All tool calls have settled; cleanup may be appending a final
            // record concurrently. Validate every fully committed log line.
            log.rsplit_once('\n')
                .unwrap()
                .0
                .lines()
                .filter(|line| serde_json::from_str::<Value>(line).unwrap()["kind"] == "call")
                .count()
        };
        let before = calls();
        for owner in ["A", "B"] {
            assert!(rebound
                .call_tool(
                    "use",
                    json!({"resource":reference}),
                    rebound.current_context_for_resource_owner(owner)
                )
                .await
                .is_err());
        }
        assert_eq!(
            calls(),
            before,
            "retired/foreign references never reach the child"
        );
        a.release().await;
        b.release().await;
        manager.shutdown().await;
    }
}
