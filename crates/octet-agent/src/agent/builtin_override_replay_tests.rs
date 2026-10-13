//! Crash recovery must not retarget an unsafe extension call to a restored builtin.
use super::*;

#[tokio::test]
async fn builtin_override_recovery_keeps_original_replay_classification_after_restoration() {
    for overridden in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("fixture.txt"), "builtin read bytes\n").unwrap();
        let path = temp.path().join("replay.jsonl");
        let mut session = Session::create(&path).unwrap();
        let mut host = ExtensionHost::new();
        host.load(&crate::tools::CoreTools);
        let registration = overridden.then(|| {
            host.dynamic_tools_with_authority(
                "reviewed-instance",
                vec![Arc::new(PromptTool {
                    name: "read",
                    snippet: None,
                    guidelines: &[],
                })],
                BTreeSet::from(["read".into()]),
                None,
                |_, _| {},
            )
            .unwrap()
        });
        let call = ToolCall {
            async_execution: false,
            id: octet_ai::ToolCallId("read-call".into()),
            name: "read".into(),
            arguments_json: serde_json::json!({"path":"fixture.txt"}).to_string(),
            argument_error: None,
        };
        let tools = host
            .tool_snapshot()
            .1
            .into_iter()
            .map(|tool| (tool.definition().name, tool))
            .collect();
        let metadata = capture_tool_replay_safety(std::slice::from_ref(&call), &tools, None);
        session
            .append_assistant_turn_with_metadata(
                AssistantMessage {
                    content: vec![AssistantPart::ToolCall(call)],
                    model: octet_ai::ModelId("test".into()),
                    protocol: Protocol::AnthropicMessages,
                },
                octet_ai::EndpointId("test".into()),
                octet_ai::ModelId("test".into()),
                Usage::default(),
                None,
                StopReason::ToolUse,
                None,
                metadata,
            )
            .unwrap();
        if let Some(registration) = registration {
            registration.remove();
        }
        drop(session);
        let session = Session::open(&path).unwrap();
        let mut agent = active_tool_test_agent(temp.path(), session, host);
        agent.recover_pending_tools(false).await.unwrap();
        let results = persisted_tool_result_texts(agent.session());
        assert_eq!(results.len(), 1);
        if overridden {
            assert!(
                results[0].contains("`read` was not replayed"),
                "{results:?}"
            );
            assert!(!results[0].contains("builtin read bytes"));
        } else {
            assert!(results[0].contains("builtin read bytes"), "{results:?}");
        }
    }
}

#[test]
fn builtin_override_replay_safety_is_per_call_and_uses_frozen_tools() {
    let safe: Arc<dyn Tool> = Arc::new(crate::tools::ReadTool);
    let unsafe_tool: Arc<dyn Tool> = Arc::new(PromptTool {
        name: "extension",
        snippet: None,
        guidelines: &[],
    });
    let tools = HashMap::from([("read".into(), safe), ("extension".into(), unsafe_tool)]);
    let calls = ["extension", "read", "missing"]
        .into_iter()
        .map(|name| ToolCall {
            async_execution: false,
            id: octet_ai::ToolCallId(name.into()),
            name: name.into(),
            arguments_json: "{}".into(),
            argument_error: None,
        })
        .collect::<Vec<_>>();
    let metadata = capture_tool_replay_safety(&calls, &tools, None).unwrap();
    assert_eq!(metadata.replay_safe_tool_calls, Some(BTreeSet::from([1])));
}
