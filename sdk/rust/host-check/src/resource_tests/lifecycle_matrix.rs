//! Independent lifecycle rows sharing genuine SDK fixtures and public host APIs.
use super::owner_refusals::{add, child_pid, log_bytes, still_same_process, unavailable};
use super::{call, create, event, events, start};
use octet_agent::extension_operations::ApplicableOperationsRequest;
use octet_agent::extension_process::{ExtensionRuntimeError, ResourceCleanupStatus, ResourceRef};
use octet_agent::{ExtensionHost, ExtensionProcess};
use serde_json::json;
use std::{future::Future, path::Path, pin::Pin, task::Poll, time::Duration};

async fn finish(process: &ExtensionProcess, workspace: &Path, expected_pids: usize) {
    assert!(process.is_running());
    assert!(process.shutdown().await);
    let log = events(workspace);
    let pids: std::collections::BTreeSet<_> =
        log.iter().map(|e| e["pid"].as_u64().unwrap()).collect();
    assert_eq!(pids.len(), expected_pids);
    assert!(!pids.contains(&u64::from(std::process::id())));
    let bytes = serde_json::to_vec(&log).unwrap();
    assert!(bytes.len() < 512 * 1024, "bounded native fixture evidence");
    eprintln!(
        "SDK resource matrix log: {}",
        std::str::from_utf8(&bytes).unwrap()
    );
}

fn refusal(error: ExtensionRuntimeError, expected: &str) {
    let ExtensionRuntimeError::Remote {
        code,
        message,
        data,
    } = error
    else {
        panic!("unexpected refusal: {error:?}")
    };
    assert_eq!(
        json!({"code":code,"message":message,"data":data}),
        json!({"code":-32000,"message":expected,"data":{"code":expected}})
    );
}
async fn cleanup(
    process: &ExtensionProcess,
    reference: &ResourceRef,
    expected: ResourceCleanupStatus,
) {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let status = process.release_resource("A", reference).unwrap();
            assert!(status.retired);
            if status.cleanup != ResourceCleanupStatus::Pending {
                assert_eq!(status.cleanup, expected);
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("host-acknowledged cleanup, not just a disposer log");
}
fn exported(workspace: &Path, name: &str) -> ResourceRef {
    serde_json::from_value(
        events(workspace)
            .into_iter()
            .find(|e| e["kind"] == "exported" && e["name"] == name)
            .unwrap()["resource"]
            .clone(),
    )
    .unwrap()
}
fn allow_terminal(workspace: &Path) {
    std::fs::write(
        workspace.join("allow-terminal"),
        b"release explicit fixture barrier",
    )
    .unwrap();
}
async fn poll_queued<F: Future>(mut future: Pin<&mut F>) {
    std::future::poll_fn(|cx| match future.as_mut().poll(cx) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(_) => panic!("request completed while SDK capacity was held"),
    })
    .await;
}
async fn held_add(
    process: &ExtensionProcess,
    workspace: &Path,
    reference: ResourceRef,
) -> tokio::task::JoinHandle<
    Result<octet_agent::extension_process::ToolCallOutput, ExtensionRuntimeError>,
> {
    let child = process.clone();
    let pending = tokio::spawn(async move {
        call(
            &child,
            "A",
            "add",
            json!({"counter":reference,"delta":1,"mode":"hold"}),
        )
        .await
    });
    event(workspace, "holding", "add").await;
    pending
}

#[tokio::test]
async fn r01_release_invalidates_reuse_with_zero_sdk_entry() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path()).await;
    let generation = process.health_snapshot().generation;
    let reference = create(&process, "r01").await;
    let pid = child_pid(workspace.path());
    add(&process, "A", &reference, 4, 4).await;
    add(&process, "A", &reference, 3, 7).await;
    let output = call(&process, "A", "release_saved", json!({}))
        .await
        .unwrap();
    assert_eq!(output.structured_content, Some(json!({"value":1})));
    cleanup(&process, &reference, ResourceCleanupStatus::Completed).await;
    unavailable(&process, workspace.path(), "A", &reference).await;
    let fresh = create(&process, "r01-fresh").await;
    assert_ne!(reference, fresh);
    add(&process, "A", &fresh, 2, 2).await;
    still_same_process(&process, workspace.path(), generation, pid);
    finish(&process, workspace.path(), 1).await;
}

async fn reload_case(hold: bool) {
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path()).await;
    let reference = create(&process, "before-reload").await;
    let generation = process.health_snapshot().generation;
    let pid = child_pid(workspace.path());
    let pending = if hold {
        Some(held_add(&process, workspace.path(), reference.clone()).await)
    } else {
        None
    };
    let report = tokio::time::timeout(Duration::from_secs(8), process.reload())
        .await
        .unwrap()
        .unwrap();
    assert!(report.generation > generation);
    if let Some(pending) = pending {
        assert!(pending.await.unwrap().is_err());
    }
    // reload has drained/terminated the old generation; no teardown log can race this snapshot.
    unavailable(&process, workspace.path(), "A", &reference).await;
    let fresh = create(&process, "after-reload").await;
    assert_ne!(reference, fresh);
    add(&process, "A", &fresh, 9, 9).await;
    assert_ne!(events(workspace.path()).last().unwrap()["pid"], pid);
    assert_eq!(process.health_snapshot().generation, report.generation);
    eprintln!("SDK reload log: {}", json!(events(workspace.path())));
    finish(&process, workspace.path(), 2).await;
}
#[tokio::test]
async fn r06_stale_generation_refused_new_identity_works() {
    reload_case(false).await;
}
#[tokio::test]
async fn r12_reload_cancels_old_execution_and_retires_identity() {
    reload_case(true).await;
}

#[tokio::test]
async fn r13_failed_candidate_preserves_live_generation_and_state() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path()).await;
    let generation = process.health_snapshot().generation;
    let reference = create(&process, "r13-live").await;
    let pid = child_pid(workspace.path());
    add(&process, "A", &reference, 6, 6).await;
    let before = log_bytes(workspace.path());
    std::fs::write(workspace.path().join("reject-start"), b"candidate only").unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(8), process.reload())
            .await
            .unwrap()
            .is_err()
    );
    std::fs::remove_file(workspace.path().join("reject-start")).unwrap();
    assert_eq!(log_bytes(workspace.path()), before);
    add(&process, "A", &reference, 2, 8).await;
    still_same_process(&process, workspace.path(), generation, pid);
    finish(&process, workspace.path(), 1).await;
}

#[tokio::test]
async fn r07_queued_use_revalidates_after_idle_release() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path()).await;
    let busy = create(&process, "r07-busy").await;
    let idle = create(&process, "r07-idle").await;
    let pending = held_add(&process, workspace.path(), busy.clone()).await;
    let mut queued = Box::pin(call(
        &process,
        "A",
        "add",
        json!({"counter":idle,"delta":100}),
    ));
    poll_queued(queued.as_mut()).await;
    assert_eq!(process.health_snapshot().pending_requests, 1);
    assert!(process.release_resource("A", &idle).unwrap().retired);
    allow_terminal(workspace.path());
    assert_eq!(
        pending.await.unwrap().unwrap().structured_content,
        Some(json!({"value":1}))
    );
    cleanup(&process, &idle, ResourceCleanupStatus::Completed).await;
    let before = log_bytes(workspace.path());
    refusal(queued.await.unwrap_err(), "resource_unavailable");
    assert_eq!(log_bytes(workspace.path()), before);
    add(&process, "A", &busy, 1, 2).await;
    finish(&process, workspace.path(), 1).await;
}

#[tokio::test]
async fn r08_admitted_use_pins_until_terminal_then_release_succeeds() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path()).await;
    let reference = create(&process, "r08-pinned").await;
    let pending = held_add(&process, workspace.path(), reference.clone()).await;
    let before = log_bytes(workspace.path());
    refusal(
        process.release_resource("A", &reference).unwrap_err(),
        "resource_busy",
    );
    assert_eq!(log_bytes(workspace.path()), before);
    assert!(!events(workspace.path())
        .iter()
        .any(|e| e["kind"] == "dispose"));
    allow_terminal(workspace.path());
    assert_eq!(
        pending.await.unwrap().unwrap().structured_content,
        Some(json!({"value":1}))
    );
    assert!(process.release_resource("A", &reference).unwrap().retired);
    cleanup(&process, &reference, ResourceCleanupStatus::Completed).await;
    unavailable(&process, workspace.path(), "A", &reference).await;
    let log = events(workspace.path());
    assert!(
        log.iter().position(|e| e["kind"] == "settled").unwrap()
            < log.iter().position(|e| e["kind"] == "dispose").unwrap()
    );
    finish(&process, workspace.path(), 1).await;
}

async fn rejected_parent(mode: &str, rpc: bool) {
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path()).await;
    let result = call(&process, "A", "create", json!({"name":mode})).await;
    if rpc {
        assert!(result.is_err());
    } else {
        let output = result.unwrap();
        assert!(output.is_error);
        assert!(output.structured_content.is_none());
    }
    event(workspace.path(), "dispose", mode).await;
    let reference = exported(workspace.path(), mode);
    unavailable(&process, workspace.path(), "A", &reference).await;
    assert!(process.lookup_resource("A", &reference).is_err());
    let fresh = create(&process, "healthy").await;
    add(&process, "A", &fresh, 2, 2).await;
    finish(&process, workspace.path(), 1).await;
}
#[tokio::test]
async fn r15_invalid_output_retires_saved_provisional_ref() {
    rejected_parent("invalid-output", true).await;
}
#[tokio::test]
async fn r16_domain_failure_has_no_projection_or_live_provisional_ref() {
    rejected_parent("error", false).await;
}

#[tokio::test]
async fn r17_failed_and_panicking_disposers_report_failed_not_completed() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path()).await;
    for mode in ["fail-dispose", "panic-dispose"] {
        let reference = create(&process, mode).await;
        assert!(process.release_resource("A", &reference).unwrap().retired);
        cleanup(&process, &reference, ResourceCleanupStatus::Failed).await;
        unavailable(&process, workspace.path(), "A", &reference).await;
    }
    let fresh = create(&process, "healthy").await;
    add(&process, "A", &fresh, 1, 1).await;
    finish(&process, workspace.path(), 1).await;
}

#[tokio::test]
async fn r20_provisional_creation_is_hidden_until_parent_admission() {
    for success in [false, true] {
        let workspace = tempfile::tempdir().unwrap();
        let process = start(workspace.path()).await;
        let mode = if success { "hold" } else { "hold-error" };
        let child = process.clone();
        let parent =
            tokio::spawn(async move { call(&child, "A", "create", json!({"name":mode})).await });
        event(workspace.path(), "exported", mode).await;
        let reference = exported(workspace.path(), mode);
        refusal(
            process.lookup_resource("A", &reference).unwrap_err(),
            "resource_unavailable",
        );
        let mut host = ExtensionHost::new();
        process.register_dynamic_tool_catalog(&mut host);
        host.enable_operation_discovery();
        host.finalize_tool_surface();
        assert!(host
            .applicable_operations(
                "A",
                ApplicableOperationsRequest {
                    resource: reference.clone(),
                    limit: None,
                    cursor: None
                }
            )
            .is_err());
        let before = log_bytes(workspace.path());
        let mut queued = Box::pin(call(
            &process,
            "A",
            "add",
            json!({"counter":reference,"delta":3}),
        ));
        poll_queued(queued.as_mut()).await;
        assert_eq!(log_bytes(workspace.path()), before);
        allow_terminal(workspace.path());
        let output = parent.await.unwrap().unwrap();
        assert_eq!(output.is_error, !success);
        if success {
            assert_eq!(
                queued.await.unwrap().structured_content,
                Some(json!({"value":3}))
            );
            assert!(!host
                .applicable_operations(
                    "A",
                    ApplicableOperationsRequest {
                        resource: reference.clone(),
                        limit: None,
                        cursor: None
                    }
                )
                .unwrap()
                .operations
                .is_empty());
        } else {
            event(workspace.path(), "dispose", mode).await;
            let before = log_bytes(workspace.path());
            refusal(queued.await.unwrap_err(), "resource_unavailable");
            assert_eq!(log_bytes(workspace.path()), before);
        }
        eprintln!(
            "R20 parent success={success}, child log={}",
            json!(events(workspace.path()))
        );
        finish(&process, workspace.path(), 1).await;
    }
}

#[tokio::test]
async fn r21_two_provisional_outputs_fail_atomically() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path()).await;
    assert!(call(&process, "A", "pair", json!({"name":"invalid"}))
        .await
        .is_err());
    event(workspace.path(), "dispose", "pair-first").await;
    event(workspace.path(), "dispose", "pair-second").await;
    let pair = events(workspace.path())
        .into_iter()
        .find(|e| e["kind"] == "pair")
        .unwrap();
    for key in ["first", "second"] {
        let reference = serde_json::from_value(pair[key].clone()).unwrap();
        unavailable(&process, workspace.path(), "A", &reference).await;
    }
    let valid = call(&process, "A", "pair", json!({"name":"valid"}))
        .await
        .unwrap()
        .structured_content
        .unwrap();
    for key in ["first", "second"] {
        let reference = serde_json::from_value(valid[key].clone()).unwrap();
        add(&process, "A", &reference, 5, 5).await;
    }
    finish(&process, workspace.path(), 1).await;
}

#[tokio::test]
async fn r19_parent_and_generation_quotas_recover_after_cleanup() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path()).await;
    let error = call(&process, "A", "create", json!({"name":"quota-parent"}))
        .await
        .unwrap_err();
    assert!(
        matches!(error, ExtensionRuntimeError::Remote { code: -32000, ref message, .. } if message == "resource quota exceeded")
    );
    for index in 0..33 {
        event(
            workspace.path(),
            "dispose",
            &format!("quota-parent-{index}"),
        )
        .await;
    }
    let provisional: Vec<ResourceRef> = events(workspace.path())
        .iter()
        .filter(|e| e["kind"] == "exported")
        .map(|e| serde_json::from_value(e["resource"].clone()).unwrap())
        .collect();
    assert_eq!(provisional.len(), 32);
    for reference in provisional {
        cleanup(&process, &reference, ResourceCleanupStatus::Completed).await;
        unavailable(&process, workspace.path(), "A", &reference).await;
    }
    let mut live = Vec::new();
    for index in 0..256 {
        live.push(create(&process, &format!("quota-{index}")).await);
    }
    let error = call(&process, "A", "create", json!({"name":"over-generation"}))
        .await
        .unwrap_err();
    assert!(
        matches!(error, ExtensionRuntimeError::Remote { code: -32000, ref message, .. } if message == "resource quota exceeded")
    );
    add(&process, "A", &live[1], 1, 1).await;
    cleanup(&process, &live[0], ResourceCleanupStatus::Completed).await;
    let fresh = create(&process, "quota-recovered").await;
    assert_ne!(fresh, live[0]);
    add(&process, "A", &fresh, 7, 7).await;
    finish(&process, workspace.path(), 1).await;
    eprintln!(
        "R19 bounded SDK quota log: {}",
        json!(events(workspace.path()))
    );
}
