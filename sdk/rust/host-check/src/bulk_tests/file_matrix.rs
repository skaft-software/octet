//! Real SDK callbacks + actual host snapshots. No synthetic bulk RPC broker.
use super::{call, events, start_with_transfers};
use octet_agent::extension_process::{ExtensionRuntimeError, ResourceRef};
use octet_agent::{BlobRef, BulkLimits, BulkStorage, ExtensionProcess};
use serde_json::json;
use std::path::{Path, PathBuf};

struct Harness {
    process: ExtensionProcess,
    storage: BulkStorage,
    workspace: tempfile::TempDir,
    transfers: PathBuf,
    backing: PathBuf,
    generation: u64,
    _root: tempfile::TempDir,
}
fn files(path: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect()
}
impl Harness {
    async fn new(limits: BulkLimits) -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = root.path().join("store");
        let storage = BulkStorage::with_root_and_limits(&store, limits).unwrap();
        let directory = |prefix: &str| {
            files(&store)
                .into_iter()
                .find(|p| p.file_name().unwrap().to_str().unwrap().starts_with(prefix))
                .unwrap()
        };
        let transfers = directory("bulk-transfer");
        let backing = directory("bulk-backing");
        let process = start_with_transfers(workspace.path(), &storage, Some(&transfers)).await;
        let generation = process.health_snapshot().generation;
        Self {
            process,
            storage,
            workspace,
            transfers,
            backing,
            generation,
            _root: root,
        }
    }
    async fn write(
        &self,
        owner: &str,
        bytes: u64,
        mode: &str,
    ) -> Result<BlobRef, ExtensionRuntimeError> {
        let output = call(
            &self.process,
            owner,
            "write",
            json!({"bytes":bytes,"mode":mode}),
        )
        .await?;
        assert!(!output.is_error);
        assert_eq!(output.content, "Binary data committed");
        assert!(!output
            .content
            .contains(&self.transfers.to_string_lossy().to_string()));
        Ok(serde_json::from_value(output.structured_content.unwrap()["data"].clone()).unwrap())
    }
    async fn read(&self, owner: &str, blob: &BlobRef) {
        let output = call(&self.process, owner, "read", json!({"data":blob}))
            .await
            .unwrap();
        assert_eq!(output.structured_content, Some(json!({"bytes":blob.bytes})));
        assert!(
            files(&self.transfers).is_empty(),
            "read callback closed and released its lease"
        );
    }
    async fn unavailable(&self, owner: &str, blob: &BlobRef) {
        let before = std::fs::read(self.workspace.path().join("bulk.jsonl")).unwrap();
        let error = call(&self.process, owner, "read", json!({"data":blob}))
            .await
            .unwrap_err();
        let ExtensionRuntimeError::Remote {
            code,
            message,
            data,
        } = error
        else {
            panic!("unexpected bulk refusal: {error:?}")
        };
        assert_eq!(
            json!({"code":code,"message":message,"data":data}),
            json!({"code":-32000,"message":"blob_unavailable","data":{"code":"blob_unavailable"}})
        );
        assert_eq!(
            std::fs::read(self.workspace.path().join("bulk.jsonl")).unwrap(),
            before
        );
        assert!(files(&self.transfers).is_empty());
    }
    fn empty(&self) {
        assert!(
            files(&self.transfers).is_empty(),
            "no scratch tickets or leases"
        );
        assert!(
            files(&self.backing).is_empty(),
            "no provisional/partial snapshot"
        );
    }
    async fn finish(self) {
        assert_eq!(self.process.health_snapshot().generation, self.generation);
        assert!(self.process.is_running());
        let log = events(self.workspace.path());
        let pids: Vec<_> = log
            .iter()
            .filter(|e| e["kind"] == "call")
            .map(|e| e["pid"].as_u64().unwrap())
            .collect();
        assert!(!pids.is_empty());
        assert_ne!(pids[0], u64::from(std::process::id()));
        assert!(pids.iter().all(|pid| *pid == pids[0]));
        eprintln!("SDK file-matrix child log: {}", json!(log));
        assert!(self.process.shutdown().await);
    }
}
fn error_is(error: ExtensionRuntimeError, expected_code: i64, expected: &str) {
    let ExtensionRuntimeError::Remote {
        code,
        message,
        data,
    } = error
    else {
        panic!("unexpected refusal: {error:?}")
    };
    assert_eq!(
        (code, message.as_str(), data),
        (expected_code, expected, None)
    );
}
async fn failed_write(mode: &str, code: i64, message: &str) {
    let h = Harness::new(BulkLimits {
        object_bytes: 64,
        owner_bytes: 64,
        write_tickets_per_generation: 1,
        read_leases_per_generation: 1,
        blobs_per_owner: 1,
    })
    .await;
    error_is(h.write("A", 32, mode).await.unwrap_err(), code, message);
    assert!(!events(h.workspace.path())
        .iter()
        .any(|e| e["kind"] == "committed"));
    h.empty();
    // Full capacity fits again only if the failed reservation was reclaimed.
    let healthy = h.write("A", 64, "").await.unwrap();
    h.read("A", &healthy).await;
    h.storage.release_blob("A", &healthy).unwrap();
    h.empty();
    h.finish().await;
}

#[tokio::test]
async fn b01_scoped_read_releases_files_and_lease_capacity() {
    let h = Harness::new(BulkLimits {
        read_leases_per_generation: 1,
        ..BulkLimits::default()
    })
    .await;
    let blob = h.write("A", 64, "").await.unwrap();
    assert_eq!(blob.bytes, 64);
    assert_eq!(blob.media_type, "application/octet-stream");
    assert_eq!(blob.digest.algorithm, "sha256");
    for _ in 0..40 {
        h.read("A", &blob).await;
    }
    h.storage.release_blob("A", &blob).unwrap();
    h.unavailable("A", &blob).await;
    h.empty();
    let fresh = h.write("A", 64, "").await.unwrap();
    h.read("A", &fresh).await;
    h.finish().await;
}
#[tokio::test]
async fn b02_short_and_long_scratch_refuse_commit_and_reclaim() {
    for mode in ["short", "long"] {
        failed_write(mode, -32000, "size_mismatch").await;
    }
}
#[tokio::test]
async fn b03_modified_scratch_digest_refuses_real_host_commit() {
    failed_write("wrong-digest", -32000, "integrity_mismatch").await;
}
#[tokio::test]
async fn b04_bounded_writer_overflow_releases_its_ticket() {
    failed_write("overflow", -32000, "bulk transfer I/O failed").await;
}
#[tokio::test]
async fn b05_callback_failure_abandons_real_scratch_before_commit() {
    failed_write("abandon", -32000, "bulk transfer I/O failed").await;
}
#[tokio::test]
async fn b12_producer_rewrite_after_publication_cannot_mutate_snapshot() {
    let h = Harness::new(BulkLimits::default()).await;
    let blob = h.write("A", 64, "immutable").await.unwrap();
    h.read("A", &blob).await;
    let identity = serde_json::to_value(&blob).unwrap();
    let result = call(&h.process, "A", "rewrite_saved", json!({}))
        .await
        .unwrap();
    assert_eq!(result.structured_content, Some(json!({"bytes":1})));
    h.read("A", &blob).await;
    assert_eq!(serde_json::to_value(&blob).unwrap(), identity);
    h.storage.release_blob("A", &blob).unwrap();
    h.empty();
    h.finish().await;
}
#[tokio::test]
async fn b14_retained_byte_and_record_quota_recovers_after_release() {
    for (owner_bytes, records) in [(128, 10), (1024, 2)] {
        let h = Harness::new(BulkLimits {
            object_bytes: 64,
            owner_bytes,
            blobs_per_owner: records,
            ..BulkLimits::default()
        })
        .await;
        let first = h.write("A", 64, "").await.unwrap();
        let second = h.write("A", 64, "").await.unwrap();
        error_is(
            h.write("A", 1, "").await.unwrap_err(),
            -32000,
            "quota_exceeded",
        );
        assert!(files(&h.transfers).is_empty());
        assert_eq!(files(&h.backing).len(), 2);
        h.storage.release_blob("A", &first).unwrap();
        h.unavailable("A", &first).await;
        let fresh = h.write("A", 64, "").await.unwrap();
        assert_ne!(first.id, fresh.id);
        h.read("A", &second).await;
        h.read("A", &fresh).await;
        h.finish().await;
    }
}
#[tokio::test]
async fn b14_ticket_and_lease_limits_refuse_nested_acquisition_then_recover() {
    let h = Harness::new(BulkLimits {
        write_tickets_per_generation: 1,
        read_leases_per_generation: 1,
        ..BulkLimits::default()
    })
    .await;
    let blob = h.write("A", 64, "ticket-quota").await.unwrap();
    let output = call(
        &h.process,
        "A",
        "read",
        json!({"data":blob,"mode":"lease-quota"}),
    )
    .await
    .unwrap();
    assert_eq!(output.structured_content, Some(json!({"bytes":64})));
    assert!(files(&h.transfers).is_empty());
    let log = events(h.workspace.path());
    let quota: Vec<_> = log.iter().filter(|e| e["kind"] == "quota").collect();
    assert_eq!(quota.len(), 2);
    assert_eq!(quota[0]["what"], "ticket");
    assert_eq!(quota[1]["what"], "lease");
    assert!(quota.iter().all(|e| e["message"] == "quota_exceeded"));
    h.read("A", &blob).await;
    let fresh = h.write("A", 64, "").await.unwrap();
    h.read("A", &fresh).await;
    h.finish().await;
}

#[tokio::test]
async fn b16_equal_digest_does_not_share_identity_or_owner_grants() {
    let h = Harness::new(BulkLimits::default()).await;
    let a = h.write("A", 64, "").await.unwrap();
    let b = h.write("B", 64, "").await.unwrap();
    assert_eq!(a.digest, b.digest);
    assert_ne!(a.id, b.id);
    h.unavailable("B", &a).await;
    h.unavailable("A", &b).await;
    h.read("A", &a).await;
    h.read("B", &b).await;
    h.storage.release_blob("A", &a).unwrap();
    h.read("B", &b).await;
    h.finish().await;
}
#[tokio::test]
async fn b17_invalid_parent_output_reclaims_provisional_snapshot() {
    let h = Harness::new(BulkLimits {
        object_bytes: 64,
        owner_bytes: 64,
        blobs_per_owner: 1,
        ..BulkLimits::default()
    })
    .await;
    assert!(call(
        &h.process,
        "A",
        "write",
        json!({"bytes":64,"mode":"invalid-output"})
    )
    .await
    .is_err());
    let log = events(h.workspace.path());
    let blob: BlobRef = serde_json::from_value(
        log.iter().find(|e| e["kind"] == "committed").unwrap()["blob"].clone(),
    )
    .unwrap();
    h.unavailable("A", &blob).await;
    h.empty();
    let fresh = h.write("A", 64, "").await.unwrap();
    h.read("A", &fresh).await;
    h.finish().await;
}
#[tokio::test]
async fn sdk_reload_preserves_admitted_blob_but_not_native_resource() {
    // B10 partial oracle: old ticket/lease reuse remains a separate requirement.
    let h = Harness::new(BulkLimits::default()).await;
    let output = call(&h.process, "A", "joint", json!({"bytes":64}))
        .await
        .unwrap()
        .structured_content
        .unwrap();
    let native: ResourceRef = serde_json::from_value(output["native"].clone()).unwrap();
    let blob: BlobRef = serde_json::from_value(output["data"].clone()).unwrap();
    h.read("A", &blob).await;
    let old_pid = events(h.workspace.path())[0]["pid"].clone();
    let reload = h.process.reload().await.unwrap();
    assert!(reload.generation > h.generation);
    assert!(h.process.lookup_resource("A", &native).is_err());
    h.read("A", &blob).await;
    assert_ne!(events(h.workspace.path()).last().unwrap()["pid"], old_pid);
    assert_eq!(h.process.health_snapshot().generation, reload.generation);
    eprintln!(
        "SDK blob/resource reload log: {}",
        json!(events(h.workspace.path()))
    );
    assert!(h.process.shutdown().await);
}
