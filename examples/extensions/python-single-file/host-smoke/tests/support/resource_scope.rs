//! R03/R04/R05/R22: real Python SDK counter, production owner/instance fences.
use super::Fixture;
use octet_agent::extension_process::{
    ExtensionHostState, ExtensionRuntimeError, ResourceRef, ToolCallOutput,
};
use serde_json::{json, Value};
use std::time::Duration;

const A: &str = "python-test-owner";
const B: &str = "other-python-test-owner";

async fn call(
    f: &Fixture,
    owner: &str,
    name: &str,
    args: Value,
) -> Result<ToolCallOutput, ExtensionRuntimeError> {
    f.process
        .call_tool(
            name,
            args,
            f.process.current_context_for_resource_owner(owner),
        )
        .await
}

async fn create(f: &Fixture, owner: &str, n: i64) -> ResourceRef {
    let result = call(f, owner, "create", json!({"n":n})).await.unwrap();
    assert!(!result.is_error);
    serde_json::from_value(result.structured_content.unwrap()["counter"].clone()).unwrap()
}

async fn increment(f: &Fixture, owner: &str, reference: &ResourceRef, expected: i64) {
    let before = f.log();
    let result = call(f, owner, "increment", json!({"counter":reference}))
        .await
        .unwrap();
    assert!(!result.is_error);
    assert_eq!(result.structured_content, Some(json!(expected)));
    assert_eq!(result.content, "Incremented counter.");
    let after = f.log();
    assert_eq!(after.len(), before.len() + 1);
    assert_eq!(&after[..before.len()], before.as_slice());
    assert_eq!(after.last().unwrap()["event"], "increment");
}

fn fabricated(reference: &ResourceRef) -> ResourceRef {
    // Keep fresh host entropy and the genuine nominal type, but use an unissued
    // token. This is a well-formed ResourceRef, not a malformed-schema refusal.
    let token = format!("unissued-{}", reference.resource);
    assert!(token.is_ascii() && token.len() <= 128);
    ResourceRef {
        resource: token,
        ..reference.clone()
    }
}

async fn unavailable(f: &Fixture, owner: &str, reference: &ResourceRef) -> Value {
    let before = f.log();
    let error = call(f, owner, "increment", json!({"counter":reference}))
        .await
        .unwrap_err();
    let envelope = match error {
        ExtensionRuntimeError::Remote {
            code,
            message,
            data,
        } => {
            json!({"code":code,"message":message,"data":data})
        }
        other => panic!("expected resource_unavailable, got {other:?}"),
    };
    // Exact closed refusal: no foreign token, nominal metadata, owner, instance,
    // generation or private path may be disclosed in either message or data.
    assert_eq!(
        envelope,
        json!({
            "code":-32000,"message":"resource_unavailable",
            "data":{"code":"resource_unavailable"}
        })
    );
    let after = f.log();
    assert_eq!(after, before, "refused call appended to the target log");
    println!(
        "resource refusal: {}",
        json!({
            "host_pid":std::process::id(),"child_pid":before[0]["pid"],
            "owner":owner,"before":before.len(),"after":after.len(),"error":envelope
        })
    );
    envelope
}

async fn disposed(f: &Fixture, n: i64) {
    // The native value identifies each cleanup; never mistake an earlier
    // disposal for completion of the next owner transition.
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(row) = f
                .log()
                .iter()
                .find(|row| row["event"] == "disposed" && row["n"] == n)
            {
                assert_eq!(row["invalidated"], true);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native disposal barrier");
}

async fn release(f: &Fixture, owner: &str, reference: &ResourceRef, n: i64) {
    assert!(
        f.process
            .release_resource(owner, reference)
            .unwrap()
            .retired
    );
    disposed(f, n).await;
}

fn owner(f: &Fixture, owner: &str) {
    // Exercise the ordinary frontend transition, not a manually edited registry.
    f.process.set_host_state(ExtensionHostState {
        session_id: Some(owner.into()),
        ..ExtensionHostState::default()
    });
}

#[tokio::test]
async fn r03_fabricated() {
    let f = Fixture::resources().await;
    let generation = f.process.health_snapshot().generation;
    let reference = create(&f, A, 40).await;
    increment(&f, A, &reference, 41).await;
    let unknown = fabricated(&reference);
    assert_ne!(unknown.resource, reference.resource);
    unavailable(&f, A, &unknown).await;
    increment(&f, A, &reference, 42).await;
    assert_eq!(f.process.health_snapshot().generation, generation);
    release(&f, A, &reference, 42).await;
    f.shutdown().await;
}

#[tokio::test]
async fn r04_foreign_session_nondisclosure() {
    let f = Fixture::resources().await;
    let generation = f.process.health_snapshot().generation;
    let a = create(&f, A, 40).await;
    let b = create(&f, B, 90).await;
    increment(&f, A, &a, 41).await;
    increment(&f, B, &b, 91).await;
    let foreign = unavailable(&f, B, &a).await;
    let unknown = unavailable(&f, B, &fabricated(&a)).await;
    assert_eq!(
        foreign, unknown,
        "foreign identity must not be an existence oracle"
    );
    increment(&f, A, &a, 42).await;
    increment(&f, B, &b, 92).await;
    assert_eq!(f.process.health_snapshot().generation, generation);
    release(&f, A, &a, 42).await;
    release(&f, B, &b, 92).await;
    f.shutdown().await;
}

#[tokio::test]
async fn r05_foreign_extension_same_nominal_type() {
    let source = Fixture::resources().await;
    let target = Fixture::resources().await;
    let source_generation = source.process.health_snapshot().generation;
    let target_generation = target.process.health_snapshot().generation;
    assert_ne!(source.log()[0]["pid"], target.log()[0]["pid"]);
    let source_owner = source
        .process
        .current_context_for_resource_owner(A)
        .resource_owner
        .unwrap();
    let target_owner = target
        .process
        .current_context_for_resource_owner(A)
        .resource_owner
        .unwrap();
    assert_eq!(source_owner.session_id, target_owner.session_id);
    assert_ne!(
        source_owner.extension_instance_id,
        target_owner.extension_instance_id
    );
    let a = create(&source, A, 10).await;
    let b = create(&target, A, 100).await;
    assert_eq!(a.resource_type, "example.Counter.v1");
    assert_eq!(a.resource_type, b.resource_type);
    assert_ne!(a.resource, b.resource);
    increment(&source, A, &a, 11).await;
    increment(&target, A, &b, 101).await;
    let source_log = source.log();
    let target_log = target.log();
    unavailable(&target, A, &a).await;
    unavailable(&source, A, &b).await;
    assert_eq!(source.log(), source_log);
    assert_eq!(target.log(), target_log);
    increment(&source, A, &a, 12).await;
    increment(&target, A, &b, 102).await;
    assert_eq!(
        source.process.health_snapshot().generation,
        source_generation
    );
    assert_eq!(
        target.process.health_snapshot().generation,
        target_generation
    );
    release(&source, A, &a, 12).await;
    release(&target, A, &b, 102).await;
    source.shutdown().await;
    target.shutdown().await;
}

#[tokio::test]
async fn r22_owner_roundtrip() {
    let f = Fixture::resources().await;
    let generation = f.process.health_snapshot().generation;
    let pid = f.log()[0]["pid"].clone();
    owner(&f, A);
    let old_a = create(&f, A, 10).await;
    increment(&f, A, &old_a, 11).await;

    owner(&f, B);
    disposed(&f, 11).await;
    let b = create(&f, B, 20).await;
    increment(&f, B, &b, 21).await;
    unavailable(&f, B, &old_a).await;
    increment(&f, B, &b, 22).await;

    owner(&f, A);
    disposed(&f, 22).await;
    unavailable(&f, A, &old_a).await;
    unavailable(&f, B, &b).await;
    let fresh_a = create(&f, A, 30).await;
    assert_eq!(fresh_a.resource_type, old_a.resource_type);
    assert_ne!(fresh_a.resource, old_a.resource);
    assert_ne!(fresh_a.resource, b.resource);
    unavailable(&f, A, &old_a).await;
    increment(&f, A, &fresh_a, 31).await;
    assert_eq!(f.process.health_snapshot().generation, generation);
    assert!(f.log().iter().all(|row| row["pid"] == pid));
    release(&f, A, &fresh_a, 31).await;
    assert_eq!(
        f.log()
            .iter()
            .filter(|row| row["event"] == "disposed")
            .count(),
        3
    );
    f.shutdown().await;
}
