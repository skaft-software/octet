//! Ultra's complete tool/extension import surface, without an agent loop.
//!
//! ```compile_fail
//! use octet_agent::extension_runtime;
//! ```
//!
//! ```compile_fail
//! use octet_agent::session_writer;
//! ```
//!
//! ```compile_fail
//! use octet_agent::skills;
//! ```
//!
//! ```compile_fail
//! use octet_agent::Agent;
//! ```
//!
//! ```compile_fail
//! use octet_agent::AgentConfig;
//! ```
//!
//! ```compile_fail
//! use octet_agent::Run;
//! ```
//!
//! ```compile_fail
//! use octet_agent::RunControl;
//! ```
//!
//! ```compile_fail
//! use octet_agent::Session;
//! ```
//!
//! ```compile_fail
//! use octet_agent::agent;
//! ```
//!
//! ```compile_fail
//! use octet_agent::session;
//! ```
//!
//! ```compile_fail
//! use octet_agent::session_leaf;
//! ```
//!
//! ```compile_fail
//! use octet_agent::compaction;
//! ```
//!
//! ```compile_fail
//! use octet_agent::delegation;
//! ```
//!
//! ```compile_fail
//! use octet_agent::cache;
//! ```
//!
//! ```compile_fail
//! use octet_agent::cache_warmer;
//! ```
//!
//! ```compile_fail
//! use octet_agent::context;
//! ```
//!
//! ```compile_fail
//! use octet_agent::telemetry;
//! ```
//!
//! ```compile_fail
//! use octet_agent::goal_driver;
//! ```
//!
//! ```compile_fail
//! use octet_agent::goal_store;
//! ```

pub use octet_agent::extension_process::ManifestContributions;
pub use octet_agent::tools::{BashTool, EditTool, ReadTool, WriteTool};
pub use octet_agent::{
    CancellationToken, DiscoveredExtension, EffectBroker, EffectIntent, EffectPolicy,
    ExtensionActivation, ExtensionExecutionContext, ExtensionHook, ExtensionHostState,
    ExtensionManifest, ExtensionProcess, ExtensionResourceOwner, ExtensionRuntimeConfig,
    ExtensionSource, ExtensionToolCallOutput, ExtensionToolDefinition, ExtensionTrust,
    SandboxConfig, Tool, ToolContext, ToolEffect, ToolOutput, ToolOutputContentPart, ToolProgress,
    ToolProgressSink,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn builtins_execute_with_caller_owned_context_and_effect_admission() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().canonicalize().unwrap();
        let mut sandbox = SandboxConfig::new(&workspace);
        sandbox.allow_edit = true;
        sandbox.allow_write = true;
        sandbox.allow_process = true;
        sandbox.allow_shell = true;
        let cancellation = CancellationToken::default();
        let context = ToolContext {
            workspace: &workspace,
            sandbox: &sandbox,
            execution_scope: "fixture-execution",
            resource_owner: "fixture-owner",
            registered_tools: &[],
            progress: ToolProgressSink::null(),
            cancellation: cancellation.clone(),
        };
        let broker = EffectBroker::new(EffectPolicy::UnsafeHost);
        let calls: Vec<(Box<dyn Tool>, serde_json::Value)> = vec![
            (
                Box::new(WriteTool),
                json!({"path":"note.txt", "content":"before\n"}),
            ),
            (
                Box::new(EditTool),
                json!({"path":"note.txt", "oldText":"before", "newText":"after"}),
            ),
            (Box::new(ReadTool), json!({"path":"note.txt"})),
            (
                Box::new(BashTool),
                json!({"command":"printf shell-fixture"}),
            ),
        ];
        for (index, (tool, arguments)) in calls.into_iter().enumerate() {
            let effect: ToolEffect = tool.effect(&arguments, &context).unwrap();
            let intent = EffectIntent::new(
                "fixture",
                "run",
                1,
                index.to_string(),
                tool.definition().name,
                effect,
                &arguments,
            )
            .unwrap();
            broker
                .authorize(&intent, Some(&context.progress))
                .await
                .unwrap();
            let output: ToolOutput = tool.execute(arguments, &context).await.unwrap();
            assert!(!output.content_parts().is_empty());
            assert!(output
                .content_parts()
                .iter()
                .any(|part| matches!(part, ToolOutputContentPart::Text(_))));
        }
        assert_eq!(
            std::fs::read_to_string(workspace.join("note.txt")).unwrap(),
            "after\n"
        );
        cancellation.cancel();
        assert!(cancellation.is_cancelled());
    }

    #[tokio::test]
    async fn progress_is_usable_without_session_event_variants() {
        let (sink, mut receiver) = ToolProgressSink::bounded_channel();
        sink.status("caller-owned progress");
        assert!(
            matches!(receiver.recv().await, Some(ToolProgress::Status(text)) if text == "caller-owned progress")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_host_loads_manifest_calls_tool_and_shuts_down() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("extension.py");
        std::fs::write(&script, include_str!("extension.py")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let manifest_path = root.path().join("extension.toml");
        std::fs::write(
            &manifest_path,
            r#"
name = "fixture"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "extension.py"
[contributes]
tools = ["echo"]
"#,
        )
        .unwrap();
        let manifest = ExtensionManifest::load(&manifest_path).unwrap();
        let _: &ManifestContributions = &manifest.contributes;
        let descriptor = DiscoveredExtension {
            manifest,
            manifest_path,
            source: ExtensionSource::Explicit,
            activation: ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        };
        let host_state = ExtensionHostState {
            session_id: Some("caller-session".into()),
            ..Default::default()
        };
        let mut config = ExtensionRuntimeConfig::new(root.path());
        config.host_state = host_state;
        // Even an explicit request cannot offer the excluded concrete agent runtime.
        config.agent_sessions = true;
        let process = ExtensionProcess::start(descriptor, config).await.unwrap();
        let definition: ExtensionToolDefinition = process.contributions().tools[0].clone();
        assert_eq!(definition.name, "echo");
        let mut context = ExtensionExecutionContext {
            workspace: root.path().to_path_buf(),
            execution_scope: None,
            resource_owner: None,
            mcp_registration_owner: None,
            host: ExtensionHostState::default(),
        };
        context.resource_owner = Some(ExtensionResourceOwner {
            session_id: "caller-owner".into(),
            extension_instance_id: "caller".into(),
            process_generation: 1,
        });
        let output: ExtensionToolCallOutput = process
            .call_tool("echo", json!({"text":"caller-run"}), context)
            .await
            .unwrap();
        assert_eq!(output.content, "caller-run");
        let _: ExtensionHook = ExtensionHook::BeforeToolCall;
        assert!(process.shutdown().await);
    }
}
