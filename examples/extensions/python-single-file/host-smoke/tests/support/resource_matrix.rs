//! Actual SDK provisional transactions, quotas, and production reload lifecycle.
use super::Fixture;
use octet_agent::extension::ExtensionHost;
use octet_agent::extension_operations::ApplicableOperationsRequest;
use octet_agent::extension_process::{ExtensionRuntimeError, ResourceCleanupStatus, ResourceRef};
use serde_json::json;
use std::{fs, time::Duration};

const OWNER: &str = "python-test-owner";
async fn fixture() -> Fixture {
    Fixture::start_fixture(
        "resource_matrix_fixture.py",
        &[
            "create",
            "increment",
            "invalid_output",
            "failed_parent",
            "hold",
            "create_held",
            "invalid_pair",
            "quota",
            "invalid_captured",
            "failed_captured",
        ],
    )
    .await
}
fn references(f: &Fixture) -> Vec<ResourceRef> {
    f.log()
        .iter()
        .filter(|row| row["event"] == "registered")
        .map(|row| serde_json::from_value(row["reference"].clone()).unwrap())
        .collect()
}
async fn unavailable(f: &Fixture, reference: &ResourceRef) {
    let before = f.log();
    let error = f
        .owned_call("increment", json!({"counter":reference}))
        .await
        .unwrap_err();
    match error {
        ExtensionRuntimeError::Remote {
            code,
            message,
            data,
        } => {
            assert_eq!(code, -32000);
            assert_eq!(message, "resource_unavailable");
            assert_eq!(data, Some(json!({"code":"resource_unavailable"})));
        }
        other => panic!("wrong resource refusal: {other:?}"),
    }
    assert_eq!(f.log(), before, "unavailable ref entered target handler");
    assert!(f.process.lookup_resource(OWNER, reference).is_err());
}
async fn cleanup(f: &Fixture, references: &[ResourceRef]) {
    // Observe retirement BEFORE repeated release; the observation must not itself
    // retire an accidentally activated output and hide an admission regression.
    for reference in references {
        assert!(f.process.lookup_resource(OWNER, reference).is_err());
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if references.iter().all(|reference| {
                f.process
                    .release_resource(OWNER, reference)
                    .is_ok_and(|status| {
                        status.retired && status.cleanup == ResourceCleanupStatus::Completed
                    })
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("independent cleanup completion");
}
async fn healthy(f: &Fixture) {
    let reference = f.create(json!({"n":40})).await;
    assert_eq!(
        f.owned_call("increment", json!({"counter":reference}))
            .await
            .unwrap()
            .structured_content,
        Some(json!(41))
    );
    f.process.release_resource(OWNER, &reference).unwrap();
    cleanup(f, &[reference]).await;
}

#[tokio::test]
async fn r15_invalid_and_r16_rpc_failed_parents_retire_captured_tokens() {
    for name in ["invalid_captured", "failed_captured"] {
        let f = fixture().await;
        assert!(f.owned_call(name, json!({})).await.is_err());
        let refs = references(&f);
        assert_eq!(refs.len(), 1);
        cleanup(&f, &refs).await;
        unavailable(&f, &refs[0]).await;
        assert_eq!(
            f.log()
                .iter()
                .filter(|row| row["event"] == "disposed")
                .count(),
            1
        );
        healthy(&f).await;
        f.shutdown().await;
    }
}

#[tokio::test]
async fn r21_two_invalid_outputs_activate_neither_reference() {
    let f = fixture().await;
    assert!(f.owned_call("invalid_pair", json!({"n":10})).await.is_err());
    let refs = references(&f);
    assert_eq!(refs.len(), 2);
    assert_ne!(refs[0].resource, refs[1].resource);
    cleanup(&f, &refs).await;
    for reference in &refs {
        unavailable(&f, reference).await;
    }
    assert_eq!(
        f.log()
            .iter()
            .filter(|row| row["event"] == "disposed" && row["invalidated"] == true)
            .count(),
        2
    );
    healthy(&f).await;
    f.shutdown().await;
}

#[tokio::test]
async fn r19_parent_quota_no_partial_publication_and_capacity_recovery() {
    let f = fixture().await;
    let error = f
        .owned_call("quota", json!({"count":33}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("quota_exceeded"));
    let refs = references(&f);
    assert_eq!(refs.len(), 32);
    let refusal = f
        .log()
        .into_iter()
        .find(|row| row["event"] == "registration_refused")
        .unwrap();
    assert_eq!(refusal["index"], 32);
    assert_eq!(refusal["data"], json!({"code":"quota_exceeded"}));
    cleanup(&f, &refs).await;
    for reference in &refs {
        unavailable(&f, reference).await;
    }
    // A new full parent batch is allowed after all failed-parent cleanup settles.
    assert_eq!(
        f.owned_call("quota", json!({"count":32}))
            .await
            .unwrap()
            .structured_content,
        Some(json!(32))
    );
    let all = references(&f);
    assert_eq!(all.len(), 64);
    cleanup(&f, &all[32..]).await;
    for reference in &all[32..] {
        unavailable(&f, reference).await;
    }
    healthy(&f).await;
    f.shutdown().await;
}

#[tokio::test]
async fn r19_generation_quota_and_capacity_recovery() {
    let f = fixture().await;
    let mut refs = Vec::new();
    for n in 0..256 {
        refs.push(f.create(json!({"n":n})).await);
    }
    let error = f.owned_call("create", json!({"n":256})).await.unwrap_err();
    assert!(error.to_string().contains("quota_exceeded"));
    assert_eq!(refs.len(), 256);
    // Active references remain usable; the rejected 257th registration has no ref.
    assert_eq!(
        f.owned_call("increment", json!({"counter":refs[0]}))
            .await
            .unwrap()
            .structured_content,
        Some(json!(1))
    );
    for reference in &refs {
        f.process.release_resource(OWNER, reference).unwrap();
    }
    cleanup(&f, &refs).await;
    for reference in &refs {
        unavailable(&f, reference).await;
    }
    healthy(&f).await;
    f.shutdown().await;
}

#[tokio::test]
async fn r20_provisional_use_and_discovery_refused_until_admission() {
    let f = fixture().await;
    let mut host = ExtensionHost::new();
    host.load(&f.process);
    host.finalize_tool_surface();
    let child = f.process.clone();
    let call = tokio::spawn(async move {
        child
            .call_tool(
                "create_held",
                json!({"n":10}),
                child.current_context_for_resource_owner(OWNER),
            )
            .await
    });
    f.barrier("output_ready").await;
    let reference = references(&f).remove(0);
    unavailable(&f, &reference).await;
    let before = f.log();
    assert!(host
        .applicable_operations(
            OWNER,
            ApplicableOperationsRequest {
                resource: reference.clone(),
                limit: None,
                cursor: None,
            }
        )
        .is_err());
    assert_eq!(f.log(), before);
    fs::write(f.root.join("allow_terminal"), "go").unwrap();
    let output = call.await.unwrap().unwrap();
    assert_eq!(
        output.structured_content,
        Some(json!({"counter":reference}))
    );
    let page = host
        .applicable_operations(
            OWNER,
            ApplicableOperationsRequest {
                resource: reference.clone(),
                limit: None,
                cursor: None,
            },
        )
        .unwrap();
    assert!(page
        .operations
        .iter()
        .any(|op| op.id == "increment" && op.path == "/counter"));
    assert_eq!(
        f.owned_call("increment", json!({"counter":reference}))
            .await
            .unwrap()
            .structured_content,
        Some(json!(11))
    );
    f.process.release_resource(OWNER, &reference).unwrap();
    cleanup(&f, &[reference]).await;
    f.shutdown().await;
}

#[tokio::test]
async fn r09_cancel_provisional_creator_before_terminal() {
    let f = fixture().await;
    let generation = f.process.health_snapshot().generation;
    let child = f.process.clone();
    let call = tokio::spawn(async move {
        child
            .call_tool(
                "create_held",
                json!({}),
                child.current_context_for_resource_owner(OWNER),
            )
            .await
    });
    f.barrier("output_ready").await;
    let refs = references(&f);
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    f.barrier("cancel_observed").await;
    assert!(!f.log().iter().any(|row| row["event"] == "disposed"));
    assert!(f.process.lookup_resource(OWNER, &refs[0]).is_err());
    fs::write(f.root.join("allow_terminal"), "go").unwrap();
    f.barrier("terminal_ready").await;
    cleanup(&f, &refs).await;
    unavailable(&f, &refs[0]).await;
    assert_eq!(f.process.health_snapshot().generation, generation);
    healthy(&f).await;
    f.shutdown().await;
}

#[tokio::test]
async fn r06_stale_generation_and_r12_accepted_reload() {
    let f = fixture().await;
    let generation = f.process.health_snapshot().generation;
    let old = f.create(json!({"n":1})).await;
    f.process.reload().await.unwrap();
    assert!(f.process.health_snapshot().generation > generation);
    unavailable(&f, &old).await;
    let fresh = f.create(json!({"n":40})).await;
    assert_ne!(fresh.resource, old.resource);
    assert_eq!(fresh.resource_type, old.resource_type);
    assert_eq!(
        f.owned_call("increment", json!({"counter":fresh}))
            .await
            .unwrap()
            .structured_content,
        Some(json!(41))
    );
    f.process.release_resource(OWNER, &fresh).unwrap();
    cleanup(&f, &[fresh]).await;
    assert!(f.process.shutdown().await);
    let log = f.log();
    let starts: Vec<_> = log.iter().filter(|row| row["event"] == "started").collect();
    assert_eq!(starts.len(), 2);
    assert_ne!(starts[0]["pid"], starts[1]["pid"]);
    assert_eq!(
        log.iter().filter(|row| row["event"] == "shutdown").count(),
        2
    );
    println!("reload child log: {}", json!(log));
}

#[tokio::test]
async fn r13_failed_candidate_keeps_old_generation_and_reference() {
    let f = fixture().await;
    let generation = f.process.health_snapshot().generation;
    let old = f.create(json!({"n":40})).await;
    fs::write(f.root.join("reject_candidate"), "reject").unwrap();
    assert!(f.process.reload().await.is_err());
    f.barrier("candidate_rejected").await;
    assert_eq!(f.process.health_snapshot().generation, generation);
    assert_eq!(
        f.owned_call("increment", json!({"counter":old}))
            .await
            .unwrap()
            .structured_content,
        Some(json!(41))
    );
    assert!(f.process.lookup_resource(OWNER, &old).is_ok());
    f.process.release_resource(OWNER, &old).unwrap();
    cleanup(&f, &[old]).await;
    assert!(f.process.shutdown().await);
    let log = f.log();
    assert_eq!(
        log.iter().filter(|row| row["event"] == "started").count(),
        1
    );
    assert_eq!(
        log.iter().filter(|row| row["event"] == "shutdown").count(),
        1
    );
    println!("failed candidate child log: {}", json!(log));
}
