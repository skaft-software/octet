//! Real source SDK children driven exclusively by production ExtensionProcess.
use super::*;
use octet_agent::ExtensionHook;
use octet_agent::extension_process::{ResourceCleanupStatus, ResourceRef, ToolCallOutput};

async fn call(
    f: &Fixture,
    owner: &str,
    tool: &str,
    args: Value,
) -> Result<ToolCallOutput, ExtensionRuntimeError> {
    f.process
        .call_tool(
            tool,
            args,
            f.process.current_context_for_resource_owner(owner),
        )
        .await
}

async fn create(f: &Fixture, value: i64) -> ResourceRef {
    let output = call(f, "A", "create", json!({"value": value}))
        .await
        .unwrap();
    assert!(!output.is_error);
    assert_eq!(output.content, "Created counter");
    serde_json::from_value(output.structured_content.unwrap()["counter"].clone()).unwrap()
}

fn unavailable(error: ExtensionRuntimeError) {
    assert!(
        matches!(error, ExtensionRuntimeError::Remote {code: -32000, ref message, ..} if message == "resource_unavailable"),
        "{error:?}"
    );
}

async fn cleanup(f: &Fixture, reference: &ResourceRef, expected: ResourceCleanupStatus) {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            // Observe actual host receipt of resource/dispose, not just the author log.
            let status = f.process.release_resource("A", reference).unwrap();
            assert!(status.retired);
            if status.cleanup != ResourceCleanupStatus::Pending {
                assert_eq!(status.cleanup, expected);
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("host-acknowledged SDK disposal");
}

fn same_generation(f: &Fixture) {
    let health = f.process.health_snapshot();
    assert_eq!(health.generation, f.generation);
    assert_eq!(health.state, ExtensionHealthState::Ready);
    assert_eq!(health.pending_requests, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn ts_native_resource_export_resolution_release_and_zero_entry_refusals() {
    let f = Fixture::start_source(true).await;
    let definitions = f.process.tool_definitions();
    let increment =
        serde_json::to_value(definitions.iter().find(|d| d.name == "increment").unwrap()).unwrap();
    assert_eq!(
        increment["operation"],
        json!({"id": "increment", "receiver": "/counter",
        "resource_inputs": [{"path": "/counter", "type": "native.Counter", "access": "exclusive"}], "resource_outputs": []})
    );
    let reference = create(&f, 40).await;
    let args = json!({"counter": reference});
    for expected in [41, 42] {
        let output = call(&f, "A", "increment", args.clone()).await.unwrap();
        assert_eq!(output.structured_content, Some(json!(expected)));
        assert_eq!(output.content, expected.to_string());
    }
    let before = f.count("increment");
    unavailable(call(&f, "B", "increment", args.clone()).await.unwrap_err());
    unavailable(
        call(
            &f,
            "A",
            "increment",
            json!({"counter": {"$resource": "unknown", "type": "native.Counter"}}),
        )
        .await
        .unwrap_err(),
    );
    assert_eq!(
        f.count("increment"),
        before,
        "foreign/unknown refs entered handler"
    );
    let released = call(&f, "A", "release", json!({})).await.unwrap();
    let status: Value = serde_json::from_str(&released.content).unwrap();
    assert_eq!(status["retired"], true);
    cleanup(&f, &reference, ResourceCleanupStatus::Completed).await;
    assert_eq!(f.count("disposed"), 1);
    assert!(
        f.records()
            .iter()
            .any(|row| row["event"] == "disposed" && row["value"] == 42)
    );
    unavailable(call(&f, "A", "increment", args).await.unwrap_err());
    assert_eq!(f.count("increment"), before, "retired ref entered handler");
    same_generation(&f);
    f.close().await;
    assert_eq!(
        f.count("disposed"),
        1,
        "shutdown double-disposed a retired value"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ts_native_failed_output_retires_provisional_and_disposer_failure_stays_retired() {
    let f = Fixture::start_source(true).await;
    let error = call(&f, "A", "create", json!({"value": 9, "invalid": true}))
        .await
        .unwrap_err();
    assert!(
        matches!(error, ExtensionRuntimeError::Remote { code: -32603, .. }),
        "{error:?}"
    );
    let provisional: ResourceRef = serde_json::from_value(
        f.records()
            .into_iter()
            .find(|row| row["event"] == "exported")
            .unwrap()["resource"]
            .clone(),
    )
    .unwrap();
    f.wait_for("disposed").await;
    cleanup(&f, &provisional, ResourceCleanupStatus::Completed).await;
    unavailable(
        call(&f, "A", "increment", json!({"counter": provisional}))
            .await
            .unwrap_err(),
    );
    assert_eq!(f.count("increment"), 0);

    let failing = create(&f, -1).await;
    assert!(f.process.release_resource("A", &failing).unwrap().retired);
    cleanup(&f, &failing, ResourceCleanupStatus::Failed).await;
    unavailable(
        call(&f, "A", "increment", json!({"counter": failing}))
            .await
            .unwrap_err(),
    );
    assert_eq!(f.count("increment"), 0);
    assert_eq!(f.count("disposed"), 2);
    let live = create(&f, 2).await;
    assert_eq!(
        call(&f, "A", "increment", json!({"counter": live}))
            .await
            .unwrap()
            .structured_content,
        Some(json!(3))
    );
    same_generation(&f);
    f.close().await;
    assert_eq!(
        f.count("disposed"),
        3,
        "shutdown cleans remaining native state once"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ts_native_reverse_policy_typed_diagnostics_and_invalid_recovery() {
    let f = Fixture::start_source(true).await;
    for invalid in [false, true, false] {
        // A native policy request needs an application consumer; ExtensionProcess
        // does not invent a policy decision. This test chooses deny, then uses
        // the production response path to verify correlation and SDK delivery.
        let mut events = f.process.subscribe();
        let request = call(&f, "A", "service", json!({"invalid": invalid}));
        tokio::pin!(request);
        loop {
            tokio::select! {
                result = &mut request => panic!("tool settled before policy consumer: {result:?}"),
                event = events.recv() => {
                    if let ExtensionEvent::PolicyEvaluationRequested { request_id, generation, intent, .. } = event.unwrap() {
                        assert_eq!(generation, f.generation);
                        assert_eq!(intent.operation, "fixture.inspect");
                        f.process.respond_to_policy_evaluation(request_id, generation,
                            octet_agent::extension_process::ExtensionPolicyEvaluationResponse {
                                decision: octet_agent::ExtensionPolicyDecision::Deny,
                                approval_token: None,
                            }).await.unwrap();
                        break;
                    }
                }
            }
        }
        let result = request.await;
        if invalid {
            assert!(
                matches!(
                    result,
                    Err(ExtensionRuntimeError::Remote { code: -32603, .. })
                ),
                "{result:?}"
            );
        } else {
            let output = result.unwrap();
            assert!(!output.is_error);
            assert_eq!(output.structured_content, Some(json!({"decision": "deny"})));
            assert_eq!(
                output.metadata,
                json!({"octet_diagnostics_v1": [{"severity": "warning", "code": "policy.denied", "message": "No effects\nperformed"}]})
            );
            assert!(output.content.contains("Policy checked"));
            assert_eq!(
                output
                    .content
                    .matches("warning[policy.denied]: No effects performed")
                    .count(),
                1
            );
        }
    }
    assert_eq!(
        f.count("policy"),
        3,
        "real reverse replies reached the handler"
    );
    assert!(
        f.records()
            .iter()
            .filter(|r| r["event"] == "policy")
            .all(|r| r["result"]["decision"] == "deny")
    );
    same_generation(&f);
    f.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn ts_native_artifact_publication_host_validation_and_hook_dispatch() {
    let f = Fixture::start_source(true).await;
    let invalid = call(&f, "A", "media", json!({"invalid": true}))
        .await
        .unwrap();
    assert!(invalid.is_error);
    assert_eq!(invalid.content, "Host refused artifact: -32602");
    assert_eq!(f.count("artifact"), 0);
    assert_eq!(f.count("artifact-refused"), 1);
    let media = call(&f, "A", "media", json!({})).await.unwrap();
    assert!(!media.is_error);
    assert!(media.content.contains("Verified preview"));
    assert_eq!(f.count("artifact"), 1);
    // call_tool's public DTO exposes text/details, not native media bytes. Successful
    // return still requires the production decoder to resolve the authorized PNG.
    let hook = f
        .process
        .run_hook(
            ExtensionHook::BeforePrompt,
            json!({"prompt": "Native hook input"}),
            f.process.current_context_for_resource_owner("A"),
        )
        .await
        .unwrap();
    assert_eq!(hook.context.len(), 1);
    assert_eq!(hook.context[0].label, "native-note");
    assert_eq!(hook.context[0].content, "Native hook input");
    assert_eq!(f.count("hook"), 1);
    same_generation(&f);
    f.close().await;
}
