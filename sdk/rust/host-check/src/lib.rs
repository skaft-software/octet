//! Qualification against the production host, with a local scripted provider for projection tests.
#[cfg(test)]
mod author_examples;
#[cfg(test)]
mod typed_tests;
#[cfg(test)]
mod resource_tests;
#[cfg(test)]
mod bulk_tests;
#[cfg(test)]
mod tests {
    use octet_agent::extension_process::ExtensionRuntimeError;
    use octet_agent::{
        DiscoveredExtension, ExtensionActivation, ExtensionManifest, ExtensionProcess,
        ExtensionRuntimeConfig, ExtensionSource, ExtensionTrust,
    };
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn binaries() -> PathBuf {
        std::env::var_os("OCTET_NATIVE_BIN_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../examples/extensions/native-hello/build")
            })
            .canonicalize()
            .expect("build the native examples before running the host check")
    }
    async fn start(language: &str, workspace: &Path) -> ExtensionProcess {
        let manifest_path =
            binaries().join(format!("extensions/native-hello-{language}/extension.toml"));
        let manifest =
            ExtensionManifest::load(&manifest_path).expect("real local example manifest");
        assert_eq!(manifest.api_version, "0.4");
        assert_eq!(manifest.requires_octet.as_deref(), Some("=0.9.0"));
        let descriptor = DiscoveredExtension {
            manifest,
            manifest_path,
            source: ExtensionSource::Explicit,
            activation: ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        };
        let mut config = ExtensionRuntimeConfig::new(workspace);
        config.request_timeout = Duration::from_secs(3);
        config.shutdown_timeout = Duration::from_secs(1);
        ExtensionProcess::start(descriptor, config)
            .await
            .expect("actual host initialize acceptance")
    }
    async fn admitted(process: &ExtensionProcess) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while process.health_snapshot().pending_requests == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actual host request admission");
    }

    #[tokio::test]
    async fn rust_c_cpp_negotiate_validate_execute_cancel_and_shutdown_with_actual_host() {
        for language in ["rust", "c", "cpp"] {
            let workspace = tempfile::tempdir().unwrap();
            let process = start(language, workspace.path()).await;
            assert_eq!(process.api_version(), "0.4");
            assert!(process.is_running());
            let selected = process.negotiated_features();
            assert_eq!(selected.len(), 3);
            assert!(selected.contains("request_progress"));
            assert!(selected.contains("request_cancellation"));
            assert!(selected.contains("content_parts"));
            let tools = process.tool_definitions();
            assert_eq!(tools.len(), 1);
            let definitions = tools
                .iter()
                .map(|t| octet_ai::ToolDef {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                    constrained_sampling: None,
                    async_execution: false,
                })
                .collect::<Vec<_>>();
            // Use the actual model-tool schema validator, not a hand-written
            // substitute. Its keyword restrictions are stricter than startup.
            for input in [
                json!({"name":"host λ😀"}),
                json!({"name":"x","delay_ms":5000}),
            ] {
                assert_eq!(
                    octet_ai::validate_tool_arguments("hello", &input, &definitions).unwrap(),
                    octet_ai::ToolArgumentValidation::Valid,
                    "{language}"
                );
            }
            for input in [
                json!({}),
                json!({"name":true}),
                json!({"name":"x","extra":1}),
                json!({"name":"x","delay_ms":5001}),
            ] {
                assert_eq!(
                    octet_ai::validate_tool_arguments("hello", &input, &definitions).unwrap(),
                    octet_ai::ToolArgumentValidation::SchemaMismatch,
                    "{language}"
                );
            }
            let output = process
                .call_tool(
                    "hello",
                    json!({"name":"host λ😀"}),
                    process.current_context(),
                )
                .await
                .unwrap();
            assert_eq!(output.content, "Hello, host λ😀!", "{language}");
            assert!(!output.is_error);
            assert!(output.structured_content.is_none());
            assert_eq!(output.metadata, serde_json::Value::Null);
            let error = process
                .call_tool("hello", json!({"name":false}), process.current_context())
                .await
                .unwrap_err();
            assert!(
                matches!(error, ExtensionRuntimeError::Remote { code: -32602, .. }),
                "{language}: {error:?}"
            );

            // Dropping a live host request drives the real serialized writer's
            // cancellation path; host tombstones the late -32800 terminal.
            let child = process.clone();
            let pending = tokio::spawn(async move {
                child
                    .call_tool(
                        "hello",
                        json!({"name":"cancel","delay_ms":5000}),
                        child.current_context(),
                    )
                    .await
            });
            admitted(&process).await;
            pending.abort();
            assert!(pending.await.unwrap_err().is_cancelled());
            // Host admission drops before the child receives cancellation. Wait
            // one bounded scheduling interval, then prove generation health and
            // successful reuse without supervisor replacement after host grace.
            tokio::time::sleep(Duration::from_millis(100)).await;
            let generation = process.health_snapshot().generation;
            let output = process
                .call_tool(
                    "hello",
                    json!({"name":"after cancel"}),
                    process.current_context(),
                )
                .await
                .unwrap();
            assert_eq!(output.content, "Hello, after cancel!");
            tokio::time::sleep(Duration::from_millis(2100)).await;
            assert!(process.is_running());
            assert_eq!(
                process.health_snapshot().generation,
                generation,
                "cancellation must not force restart"
            );

            let child = process.clone();
            let pending = tokio::spawn(async move {
                child
                    .call_tool(
                        "hello",
                        json!({"name":"shutdown","delay_ms":5000}),
                        child.current_context(),
                    )
                    .await
            });
            admitted(&process).await;
            assert!(
                process.shutdown().await,
                "{language}: shutdown must acknowledge"
            );
            assert!(
                matches!(
                    pending.await.unwrap(),
                    Err(ExtensionRuntimeError::Cancelled { .. })
                ),
                "{language}"
            );
            assert!(!process.is_running());
            eprintln!("real-host {language}: negotiate/schema/tool/cancel/reuse/shutdown PASS");
        }
    }

    #[tokio::test]
    async fn source_manifest_discovery_gates_and_relative_entrypoint_are_real() {
        let source_manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../examples/extensions/native-hello/extension.toml")
            .canonicalize()
            .unwrap();
        let mut policy = octet_agent::ExtensionPolicy::default();
        // Controlled/default policy: a global source needs an exact source grant.
        // Explicit CLI roots already convey authority, so do not conflate them.
        let load = |policy: &octet_agent::ExtensionPolicy| {
            octet_agent::ExtensionCatalog::load_resolved(
                [octet_agent::extension_process::ExtensionManifestInput {
                    path: source_manifest.clone(),
                    source: ExtensionSource::Global,
                }],
                policy,
                256 * 1024,
            )
        };
        let catalog = load(&policy);
        assert!(catalog.diagnostics.is_empty());
        assert_eq!(catalog.extensions.len(), 1);
        let workspace = tempfile::tempdir().unwrap();
        let disabled = ExtensionProcess::start(
            catalog.extensions[0].clone(),
            ExtensionRuntimeConfig::new(workspace.path()),
        )
        .await;
        assert!(matches!(disabled, Err(ExtensionRuntimeError::Disabled(_))));
        policy.enable("native-hello");
        let catalog = load(&policy);
        let untrusted = ExtensionProcess::start(
            catalog.extensions[0].clone(),
            ExtensionRuntimeConfig::new(workspace.path()),
        )
        .await;
        assert!(matches!(
            untrusted,
            Err(ExtensionRuntimeError::Untrusted(_))
        ));
        policy.trust_source("native-hello", &source_manifest);
        let catalog = load(&policy);
        let process = ExtensionProcess::start(
            catalog.extensions[0].clone(),
            ExtensionRuntimeConfig::new(workspace.path()),
        )
        .await
        .unwrap();
        // Checked-in relative native entrypoint, not a mock launcher/manifest.
        let output = process
            .call_tool(
                "hello",
                json!({"name":"source manifest"}),
                process.current_context(),
            )
            .await
            .unwrap();
        assert_eq!(output.content, "Hello, source manifest!");
        assert!(process.shutdown().await);
        assert!(!process.is_running());
    }

    #[tokio::test]
    async fn retained_canonical_03_process_contract_remains_live() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../examples/extensions/api-v03-minimal");
        let source_manifest = std::fs::read_to_string(source.join("extension.toml")).unwrap();
        assert!(source_manifest.contains("requires_octet = \"=0.8.0\""));
        let staging = tempfile::tempdir().unwrap();
        let script = staging.path().join("extension.py");
        std::fs::copy(source.join("extension.py"), &script).unwrap();
        assert_eq!(
            std::fs::read(&script).unwrap(),
            std::fs::read(source.join("extension.py")).unwrap()
        );
        // Private candidate qualification only: never retag canonical 0.3 or
        // alter the retained example's checked-in release pin/source bytes.
        let staged_manifest =
            source_manifest.replace("requires_octet = \"=0.8.0\"", "requires_octet = \"=0.9.0\"");
        let manifest_path = staging.path().join("extension.toml");
        std::fs::write(&manifest_path, staged_manifest).unwrap();
        let manifest = ExtensionManifest::load(&manifest_path).unwrap();
        assert_eq!(manifest.api_version, "0.3");
        assert_eq!(manifest.version, "0.1.0");
        let descriptor = DiscoveredExtension {
            manifest,
            manifest_path,
            source: ExtensionSource::Explicit,
            activation: ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        };
        let mut config = ExtensionRuntimeConfig::new(staging.path());
        config.request_timeout = Duration::from_secs(3);
        config.shutdown_timeout = Duration::from_secs(1);
        let process = ExtensionProcess::start(descriptor, config).await.unwrap();
        assert_eq!(process.api_version(), "0.3");
        let output = process
            .call_tool(
                "echo",
                json!({"text":"canonical 0.3 remains live"}),
                process.current_context(),
            )
            .await
            .unwrap();
        assert_eq!(output.content, "canonical 0.3 remains live");
        assert_eq!(
            output.structured_content,
            Some(json!({"text":"canonical 0.3 remains live"}))
        );
        let child = process.clone();
        let pending = tokio::spawn(async move {
            child
                .call_tool(
                    "echo",
                    json!({"text":"cancel", "delay_ms":5000}),
                    child.current_context(),
                )
                .await
        });
        admitted(&process).await;
        assert!(process.shutdown().await);
        assert!(matches!(
            pending.await.unwrap(),
            Err(ExtensionRuntimeError::Cancelled { .. })
        ));
        assert!(!process.is_running());
        assert_eq!(
            std::fs::read_to_string(source.join("extension.toml")).unwrap(),
            source_manifest
        );
        eprintln!("real-host canonical 0.3: unchanged wire/source, private candidate pin, tool/cancel/shutdown PASS");
    }

    #[tokio::test]
    async fn actual_host_and_sdk_both_refuse_unsupported_hook_declarations() {
        let workspace = tempfile::tempdir().unwrap();
        let binary = binaries().join("hello-rust");
        let source = format!("name = \"hook-refusal\"\nversion = \"0.1.0\"\napi_version = \"0.4\"\n[entrypoint]\ncommand = {}\n[contributes]\ntools = [\"hello\"]\nhooks = [\"before_prompt\"]\n", serde_json::to_string(&binary.to_string_lossy()).unwrap());
        let manifest = ExtensionManifest::parse(&source).unwrap();
        let manifest_path = workspace.path().join("extension.toml");
        std::fs::write(&manifest_path, source).unwrap();
        let descriptor = DiscoveredExtension {
            manifest,
            manifest_path,
            source: ExtensionSource::Explicit,
            activation: ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        };
        let mut config = ExtensionRuntimeConfig::new(workspace.path());
        config.request_timeout = Duration::from_secs(2);
        match ExtensionProcess::start(descriptor, config).await {
            Err(ExtensionRuntimeError::Remote { code: -32602, .. }) => {}
            Ok(process) => {
                process.shutdown().await;
                panic!("unsupported native hook was accepted");
            }
            Err(error) => panic!("unexpected host refusal: {error:?}"),
        }
    }
}
