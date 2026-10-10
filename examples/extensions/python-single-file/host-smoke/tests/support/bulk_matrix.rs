//! Real SDK helpers with real-file commit faults, finite one-ticket storage.
use super::Fixture;
use serde_json::json;
use std::{fs, time::Duration};

async fn fixture() -> Fixture {
    Fixture::start_fixture(
        "bulk_matrix_fixture.py",
        &[
            "publish",
            "measure",
            "invalid_output",
            "failed_parent",
            "cancel_publish",
            "cancel_read",
            "fault_publish",
            "measure_small",
        ],
    )
    .await
}
fn stored_files(f: &Fixture) -> usize {
    let directories: Vec<_> = fs::read_dir(f.root.join("bulk-store"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("bulk-")
        })
        .collect();
    assert_eq!(
        directories.len(),
        2,
        "private transfer and backing roots required"
    );
    directories
        .iter()
        .map(|path| fs::read_dir(path).unwrap().count())
        .sum()
}
async fn empty_storage(f: &Fixture) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while stored_files(f) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("scratch and partial backing reclaimed");
    assert!(!f.log().iter().any(|row| row["event"] == "committed"));
}
async fn recovered_capacity(f: &Fixture) {
    // One blob/ticket and 64 owner bytes: success requires complete reservation
    // recovery, not just failure to return a descriptor from the preceding call.
    let output = f.owned_call("publish", json!({"length":64})).await.unwrap();
    let reference = output.structured_content.unwrap()["data"].clone();
    assert_eq!(reference["bytes"], 64);
    assert_eq!(
        f.owned_call("measure_small", json!({"data":reference}))
            .await
            .unwrap()
            .structured_content,
        Some(json!({"bytes":64,"sha256":reference["digest"]["value"]}))
    );
    assert_eq!(
        stored_files(f),
        1,
        "only immutable backing remains after lease close"
    );
}
async fn refused(mode: &str, code: &str) {
    let f = fixture().await;
    let error = f
        .owned_call("fault_publish", json!({"mode":mode}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains(code), "wrong refusal: {error}");
    let row = f
        .log()
        .into_iter()
        .find(|row| row["event"] == "commit_refused")
        .unwrap();
    assert_eq!(row["data"], json!({"code":code}));
    empty_storage(&f).await;
    recovered_capacity(&f).await;
    f.shutdown().await;
}
#[tokio::test]
async fn b02_short_file_size_mismatch_reclaims_reservation() {
    refused("short", "size_mismatch").await;
}
#[tokio::test]
async fn b02_declared_short_size_mismatch_reclaims_reservation() {
    refused("declared_short", "size_mismatch").await;
}
#[tokio::test]
async fn b03_wrong_commit_digest_reclaims_reservation() {
    refused("digest", "integrity_mismatch").await;
}
#[tokio::test]
async fn b04_oversize_file_refused_before_copy() {
    refused("long", "quota_exceeded").await;
}
#[tokio::test]
async fn b05_abandoned_sdk_write_reclaims_scratch_and_reservation() {
    let f = fixture().await;
    let error = f
        .owned_call("fault_publish", json!({"mode":"abandon"}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("fixture_abandon"));
    f.barrier("fault_settled").await;
    empty_storage(&f).await;
    recovered_capacity(&f).await;
    f.shutdown().await;
}
#[tokio::test]
async fn b06_cancel_before_commit_reclaims_scratch_and_reservation() {
    let f = fixture().await;
    let generation = f.process.health_snapshot().generation;
    let child = f.process.clone();
    let call = tokio::spawn(async move {
        child
            .call_tool(
                "fault_publish",
                json!({"mode":"cancel"}),
                child.current_context_for_resource_owner("python-test-owner"),
            )
            .await
    });
    f.barrier("commit_ready").await;
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    f.barrier("fault_settled").await;
    empty_storage(&f).await;
    recovered_capacity(&f).await;
    assert_eq!(f.process.health_snapshot().generation, generation);
    f.shutdown().await;
}
