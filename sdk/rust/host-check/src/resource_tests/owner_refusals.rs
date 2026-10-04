//! Independent R03/R04/R05/R22 oracles against the real Rust SDK counter.
use super::{call, event, events, start};
use octet_agent::extension_process::{ExtensionRuntimeError, ResourceRef};
use octet_agent::{ExtensionHostState, ExtensionProcess};
use serde_json::{json, Value};
use std::path::Path;

fn log_bytes(workspace: &Path) -> Vec<u8> {
    std::fs::read(workspace.join("resources.jsonl")).unwrap()
}

async fn create_owned(process: &ExtensionProcess, owner: &str, name: &str) -> ResourceRef {
    let output = call(process, owner, "create", json!({"name":name}))
        .await
        .unwrap();
    assert!(!output.is_error);
    serde_json::from_value(output.structured_content.unwrap()["counter"].clone()).unwrap()
}

fn fabricated(workspace: &Path, nominal: &str) -> ResourceRef {
    // A random private tempdir basename supplies an unissued, valid opaque
    // token without a new randomness dependency or another protocol peer.
    ResourceRef {
        resource: format!(
            "unissued-{}",
            workspace.file_name().unwrap().to_str().unwrap()
        ),
        resource_type: nominal.into(),
    }
}

async fn unavailable(
    process: &ExtensionProcess,
    workspace: &Path,
    owner: &str,
    reference: &ResourceRef,
) -> Value {
    let before = log_bytes(workspace);
    let error = call(
        process,
        owner,
        "add",
        json!({"counter":reference,"delta":100}),
    )
    .await
    .unwrap_err();
    let ExtensionRuntimeError::Remote {
        code,
        message,
        data,
    } = error
    else {
        panic!("expected exact resource refusal, got {error:?}");
    };
    let envelope = json!({"code":code,"message":message,"data":data});
    assert_eq!(
        envelope,
        json!({"code":-32000,"message":"resource_unavailable","data":{"code":"resource_unavailable"}}),
        "closed error must disclose no token, nominal type, owner, instance, generation or path"
    );
    assert_eq!(
        log_bytes(workspace),
        before,
        "zero target append-log byte delta"
    );
    eprintln!("SDK resource refusal: {envelope}; target append-log delta=0");
    envelope
}

async fn add(
    process: &ExtensionProcess,
    owner: &str,
    reference: &ResourceRef,
    delta: i64,
    expected: i64,
) {
    let output = call(
        process,
        owner,
        "add",
        json!({"counter":reference,"delta":delta}),
    )
    .await
    .expect("healthy native counter control must dispatch");
    assert!(!output.is_error);
    assert_eq!(output.content, "Counter updated");
    assert_eq!(output.structured_content, Some(json!({"value":expected})));
}

fn child_pid(workspace: &Path) -> u64 {
    let entries = events(workspace);
    let pid = entries[0]["pid"].as_u64().unwrap();
    assert!(pid > 0);
    assert_ne!(pid, u64::from(std::process::id()));
    assert!(entries.iter().all(|entry| entry["pid"] == pid));
    pid
}

fn still_same_process(process: &ExtensionProcess, workspace: &Path, generation: u64, pid: u64) {
    assert!(process.is_running());
    assert_eq!(process.health_snapshot().generation, generation);
    assert_eq!(child_pid(workspace), pid);
    eprintln!("SDK resource child log: {}", json!(events(workspace)));
}

#[tokio::test]
async fn r03_fabricated_ref_unavailable_zero_sdk_invocations() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path()).await;
    let generation = process.health_snapshot().generation;
    let live = create_owned(&process, "A", "r03-live").await;
    let pid = child_pid(workspace.path());
    add(&process, "A", &live, 2, 2).await;
    let unknown = fabricated(workspace.path(), &live.resource_type);
    assert_ne!(unknown, live);
    unavailable(&process, workspace.path(), "A", &unknown).await;
    add(&process, "A", &live, 3, 5).await;
    still_same_process(&process, workspace.path(), generation, pid);
    assert!(process.shutdown().await);
}

#[tokio::test]
async fn r04_foreign_session_matches_unknown_without_metadata() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path()).await;
    let generation = process.health_snapshot().generation;
    let a = create_owned(&process, "A", "r04-owner-a-private").await;
    let b = create_owned(&process, "B", "r04-owner-b-private").await;
    let pid = child_pid(workspace.path());
    add(&process, "A", &a, 2, 2).await;
    add(&process, "B", &b, 7, 7).await;
    let unknown = fabricated(workspace.path(), &a.resource_type);
    assert_ne!(unknown, a);
    assert_ne!(unknown, b);
    let foreign = unavailable(&process, workspace.path(), "B", &a).await;
    let missing = unavailable(&process, workspace.path(), "B", &unknown).await;
    assert_eq!(
        foreign, missing,
        "foreign identity must reveal no A metadata"
    );
    // Both owners are still authorized for their own native objects; a blanket
    // rejection of B, or mutation of either object on refusal, cannot pass.
    add(&process, "A", &a, 3, 5).await;
    add(&process, "B", &b, 1, 8).await;
    still_same_process(&process, workspace.path(), generation, pid);
    assert!(process.shutdown().await);
}

#[tokio::test]
async fn r05_live_sdk_instances_refuse_same_type_transfer() {
    let a_workspace = tempfile::tempdir().unwrap();
    let b_workspace = tempfile::tempdir().unwrap();
    let a_process = start(a_workspace.path()).await;
    let b_process = start(b_workspace.path()).await;
    let a_generation = a_process.health_snapshot().generation;
    let b_generation = b_process.health_snapshot().generation;
    let a = create_owned(&a_process, "A", "r05-instance-a-private").await;
    let b = create_owned(&b_process, "A", "r05-instance-b-private").await;
    assert_eq!(a.resource_type, b.resource_type);
    assert_eq!(a.resource_type, "fixture.Counter");
    assert_ne!(a.resource, b.resource);
    let a_owner = a_process.lookup_resource("A", &a).unwrap();
    let b_owner = b_process.lookup_resource("A", &b).unwrap();
    assert_eq!(a_owner.session_id, b_owner.session_id);
    assert_ne!(a_owner.extension_instance_id, b_owner.extension_instance_id);
    let a_pid = child_pid(a_workspace.path());
    let b_pid = child_pid(b_workspace.path());
    assert_ne!(a_pid, b_pid, "two simultaneously live actual SDK processes");
    add(&a_process, "A", &a, 2, 2).await;
    add(&b_process, "A", &b, 9, 9).await;
    let a_before = log_bytes(a_workspace.path());
    let b_before = log_bytes(b_workspace.path());
    let foreign = unavailable(&b_process, b_workspace.path(), "A", &a).await;
    let unknown = fabricated(b_workspace.path(), &b.resource_type);
    let missing = unavailable(&b_process, b_workspace.path(), "A", &unknown).await;
    assert_eq!(foreign, missing);
    unavailable(&a_process, a_workspace.path(), "A", &b).await;
    assert_eq!(log_bytes(a_workspace.path()), a_before);
    assert_eq!(log_bytes(b_workspace.path()), b_before);
    add(&a_process, "A", &a, 3, 5).await;
    add(&b_process, "A", &b, 1, 10).await;
    still_same_process(&a_process, a_workspace.path(), a_generation, a_pid);
    still_same_process(&b_process, b_workspace.path(), b_generation, b_pid);
    assert!(a_process.shutdown().await);
    assert!(b_process.shutdown().await);
}

#[tokio::test]
async fn r22_owner_roundtrip_does_not_revive_retired_sdk_refs() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path()).await;
    let generation = process.health_snapshot().generation;
    process.set_host_state(ExtensionHostState {
        session_id: Some("A".into()),
        ..ExtensionHostState::default()
    });
    let old_a = create_owned(&process, "A", "r22-old-a").await;
    let pid = child_pid(workspace.path());
    add(&process, "A", &old_a, 7, 7).await;

    // Exercise the real host owner transition, not an SDK cache mutation or
    // explicit release substituted for owner retirement.
    process.set_host_state(ExtensionHostState {
        session_id: Some("B".into()),
        ..ExtensionHostState::default()
    });
    // Predicate barriers separate disposal log appends from zero-dispatch
    // snapshots; no fixed scheduling delay or claimed cancellation race.
    event(workspace.path(), "dispose", "r22-old-a").await;
    let b = create_owned(&process, "B", "r22-b").await;
    add(&process, "B", &b, 4, 4).await;
    let before_return = unavailable(&process, workspace.path(), "A", &old_a).await;

    process.set_host_state(ExtensionHostState {
        session_id: Some("A".into()),
        ..ExtensionHostState::default()
    });
    event(workspace.path(), "dispose", "r22-b").await;
    let new_a = create_owned(&process, "A", "r22-new-a").await;
    assert_eq!(old_a.resource_type, new_a.resource_type);
    assert_ne!(old_a.resource, new_a.resource);
    add(&process, "A", &new_a, 10, 10).await;
    let after_return = unavailable(&process, workspace.path(), "A", &old_a).await;
    assert_eq!(before_return, after_return);
    unavailable(&process, workspace.path(), "B", &b).await;
    let unknown = fabricated(workspace.path(), &new_a.resource_type);
    assert_eq!(
        after_return,
        unavailable(&process, workspace.path(), "A", &unknown).await
    );
    add(&process, "A", &new_a, 3, 13).await;
    assert!(process.lookup_resource("A", &new_a).is_ok());
    still_same_process(&process, workspace.path(), generation, pid);
    assert!(process.shutdown().await);
}
