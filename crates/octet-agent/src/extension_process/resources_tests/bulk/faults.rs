//! B07: real process commit failure after partial filesystem I/O, not SDK parity
//! or actual disk exhaustion. Only the selected blocking job receives the fault.
use super::*;
use std::sync::{atomic::AtomicBool, mpsc as std_mpsc};

const DOUBLE_PAYLOAD_BYTES: u64 = PAYLOAD_BYTES * 2;
const DOUBLE_PAYLOAD_DIGEST: &str =
    "d6a67e2c98975a9abd9f450005e8196710e5d94d3b9861ec6c259b682d45124d";
const WAIT: Duration = Duration::from_secs(10);

// Every assertion/timeout unwind releases both possible pauses. The bounded
// synchronous wait is failure cleanup, never the ordering/progress oracle.
struct ResumeCopy {
    preparation: Option<Arc<ReferenceTestBarrier>>,
    copy: Option<std_mpsc::SyncSender<()>>,
}
impl Drop for ResumeCopy {
    fn drop(&mut self) {
        if let Some(barrier) = self.preparation.take() {
            barrier.proceed.notify_one();
        }
        if let Some(sender) = self.copy.take() {
            let _ = sender.send(());
        }
    }
}

fn entries(directory: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    paths
}
fn only(paths: Vec<PathBuf>) -> PathBuf {
    assert_eq!(
        paths.len(),
        1,
        "expected exactly one private file/directory"
    );
    paths.into_iter().next().unwrap()
}
fn assert_empty(transfer: &Path, backing: &Path) {
    assert!(entries(transfer).is_empty(), "scratch/lease leaked");
    assert!(entries(backing).is_empty(), "snapshot/publication leaked");
}
async fn verify_double(fixture: &Fixture, owner: &str, reference: &BlobRef) {
    assert_eq!(reference.bytes, DOUBLE_PAYLOAD_BYTES);
    assert_eq!(reference.digest.algorithm, "sha256");
    assert_eq!(reference.digest.value, DOUBLE_PAYLOAD_DIGEST);
    let output = fixture
        .call(owner, "bulk_read", json!({"blob":reference}))
        .await
        .unwrap();
    assert!(!output.is_error);
    assert_eq!(
        output.structured_content,
        Some(json!({"bytes":DOUBLE_PAYLOAD_BYTES,"verified":true}))
    );
}

#[tokio::test]
async fn b07_enospc_after_real_partial_copy_has_no_publication_and_recovers_quota() {
    assert_eq!(
        format!(
            "{:x}",
            Sha256::digest(b"immutable-octet-bulk-v1".repeat(4096))
        ),
        DOUBLE_PAYLOAD_DIGEST
    );
    for (rust, controlled) in VARIANTS {
        let storage = BulkStorage::with_limits(BulkLimits {
            object_bytes: DOUBLE_PAYLOAD_BYTES,
            owner_bytes: DOUBLE_PAYLOAD_BYTES,
            write_tickets_per_generation: 1,
            read_leases_per_generation: 1,
            blobs_per_owner: 1,
        })
        .unwrap();
        let mut target = fixture(rust, controlled, &storage, false).await;
        let control = fixture(rust, controlled, &storage, false).await;
        for peer in [&target, &control] {
            std::fs::write(peer.temp.path().join("bulk-double-payload"), "").unwrap();
        }
        let generation = target.process.health_snapshot().generation;
        let control_generation = control.process.health_snapshot().generation;
        let connection = read_std_lock(&target.process.inner.connection).clone();
        let transfer = storage.lock().transfer_directory().to_owned();
        // These random, host-owned paths are test observations, never wire data.
        let backing = only(
            entries(transfer.parent().unwrap())
                .into_iter()
                .filter(|path| {
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("bulk-backing")
                })
                .collect(),
        );
        assert_empty(&transfer, &backing);
        let barrier = Arc::new(ReferenceTestBarrier::default());
        let (resume_tx, resume_rx) = std_mpsc::sync_channel(1);
        let mut resume = ResumeCopy {
            preparation: Some(barrier.clone()),
            copy: Some(resume_tx),
        };
        lock_std_mutex(&connection.resources).before_bulk_copy = Some(barrier.clone());
        let running = target.blocked("bulk_create", json!({}), CancellationToken::default());
        tokio::time::timeout(WAIT, barrier.entered.notified())
            .await
            .expect("commit must enter its prepared-job barrier");
        let entered = target.event("entered").await;
        assert_eq!(entered["name"], "bulk_create");
        let parent_id = entered["request"].as_u64().unwrap();
        let child_id = {
            let children = lock_std_mutex(&connection.child_requests);
            let active: Vec<_> = children
                .iter()
                .filter(|(_, child)| {
                    child.response_state.state.load(Ordering::Acquire) == CHILD_ACTIVE
                })
                .collect();
            assert_eq!(active.len(), 1, "select the actual active commit child");
            let (id, child) = active[0];
            assert_eq!(child.parent_request_id, parent_id);
            id.clone()
        };
        let scratch = only(entries(&transfer));
        let partial = only(entries(&backing));
        assert_eq!(
            std::fs::metadata(&scratch).unwrap().len(),
            DOUBLE_PAYLOAD_BYTES
        );
        assert_eq!(std::fs::metadata(&partial).unwrap().len(), 0);
        let observed_path = partial.clone();
        let (observed_tx, observed_rx) = oneshot::channel();
        let mut observed_tx = Some(observed_tx);
        let enospc_returned = Arc::new(AtomicBool::new(false));
        let injected = enospc_returned.clone();
        {
            let mut registry = lock_std_mutex(&connection.resources);
            assert!(registry.bulk_copy_hook.is_none());
            registry.bulk_copy_hook = Some((
                child_id.clone(),
                Box::new(move |copied| {
                    assert!(copied > 0 && copied <= 64 * 1024);
                    assert!(copied < DOUBLE_PAYLOAD_BYTES, "must fail during the copy");
                    let file_bytes = std::fs::metadata(&observed_path)?.len();
                    assert_eq!(file_bytes, copied, "hook must follow actual file writes");
                    observed_tx
                        .take()
                        .expect("one selected chunk/job only")
                        .send(json!({"copied_bytes":copied,"file_bytes":file_bytes}))
                        .expect("test must observe the partial write");
                    resume_rx
                        .recv_timeout(WAIT)
                        .expect("test must explicitly resume the blocked copy");
                    injected.store(true, Ordering::Release);
                    Err(std::io::Error::from_raw_os_error(libc::ENOSPC).into())
                }),
            ));
        }
        resume.preparation.take().unwrap().proceed.notify_one();
        let observed = tokio::time::timeout(WAIT, observed_rx)
            .await
            .expect("blocking job must reach its actual partial write")
            .expect("blocking hook must report its byte observation");
        println!("B07 rust={rust} controlled={controlled} partial={observed}");
        assert!(lock_std_mutex(&connection.resources)
            .bulk_copy_hook
            .is_none());
        assert!(!enospc_returned.load(Ordering::Acquire));
        assert!(!running.is_finished());

        // A different real process/session must publish and verify through the
        // same storage while A is paused inside its selected blocking callback.
        let control_output = control.call("B", "bulk_create", json!({})).await.unwrap();
        assert!(!control_output.is_error);
        let control_blob = blob(&control_output);
        verify_double(&control, "B", &control_blob).await;
        storage.release_blob("B", &control_blob).unwrap();
        assert_eq!(entries(&transfer), vec![scratch.clone()]);
        assert_eq!(entries(&backing), vec![partial.clone()]);
        assert_eq!(
            std::fs::metadata(&partial).unwrap().len(),
            observed["copied_bytes"].as_u64().unwrap()
        );
        assert!(!enospc_returned.load(Ordering::Acquire));
        assert!(
            !running.is_finished(),
            "control must progress before fault return"
        );
        resume.copy.take().unwrap().send(()).unwrap();

        let failed = tokio::time::timeout(WAIT, running)
            .await
            .expect("failed commit must settle")
            .unwrap()
            .unwrap();
        assert!(enospc_returned.load(Ordering::Acquire));
        assert!(failed.is_error);
        assert!(
            failed.structured_content.is_none(),
            "no failed-call BlobRef publication"
        );
        let failure = target.event("bulk_error").await;
        assert_eq!(failure["request"], parent_id);
        assert_eq!(failure["code"], "storage_unavailable");
        assert_eq!(target.event("terminal").await["request"], parent_id);
        until(|| lock_std_mutex(&connection.child_requests).is_empty()).await;
        assert_eq!(target.process.health_snapshot().generation, generation);
        assert_eq!(target.calls(), 1, "no replay or failed-call cleanup RPC");
        assert!(!scratch.exists());
        assert!(!partial.exists());
        assert_empty(&transfer, &backing);

        // No failed-ticket release, owner retirement, reload, or limit increase.
        // A leaked reservation/record/byte prevents these exact-quota operations.
        let mut recovered: Vec<BlobRef> = Vec::new();
        for _ in 0..2 {
            let output = target.call("A", "bulk_create", json!({})).await.unwrap();
            assert!(!output.is_error);
            let reference = blob(&output);
            assert_ne!(reference.id, control_blob.id);
            assert!(recovered.iter().all(|prior| prior.id != reference.id));
            verify_double(&target, "A", &reference).await;
            storage.release_blob("A", &reference).unwrap();
            assert_empty(&transfer, &backing);
            assert_eq!(target.process.health_snapshot().generation, generation);
            recovered.push(reference);
        }
        assert_eq!(
            control.process.health_snapshot().generation,
            control_generation
        );
        let target_log = target.log();
        let control_log = control.log();
        for log in [&target_log, &control_log] {
            assert_eq!(
                log.iter()
                    .filter(|entry| entry["kind"] == "process")
                    .count(),
                1
            );
        }
        assert_ne!(target_log[0]["pid"], control_log[0]["pid"]);
        let target_calls: Vec<_> = target_log
            .iter()
            .filter(|entry| entry["kind"] == "call")
            .collect();
        assert_eq!(target_calls[0]["request"], parent_id);
        assert_eq!(
            target_calls
                .iter()
                .map(|entry| entry["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "bulk_create",
                "bulk_create",
                "bulk_read",
                "bulk_create",
                "bulk_read"
            ]
        );
        assert_eq!(control.calls(), 2);
        let report = json!({
            "case":"B07 actual-process partial-copy ENOSPC",
            "host_pid":std::process::id(),"rust":rust,"controlled":controlled,
            "payload_marker":"bulk-double-payload","payload_bytes":DOUBLE_PAYLOAD_BYTES,
            "limits":storage.limits(),"generation":generation,
            "control_generation":control_generation,"parent_request":parent_id,
            "commit_child_request":child_id,"partial_copy":observed,"injected_errno":libc::ENOSPC,
            "failure":failure,"failed_is_error":failed.is_error,
            "failed_structured_content":failed.structured_content,
            "concurrent_control_blob":control_blob,"concurrent_control_verified":true,
            "empty_after_failure":true,"recovered_blobs":recovered,
            "recovered_verified":true,"empty_after_each_recovery_release":true,
            "target_log":target_log,"control_log":control_log
        });
        println!("B07 {report}");
        if let Some(root) = std::env::var_os("OCTET_RESOURCE_EVIDENCE_DIR") {
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                PathBuf::from(root).join(format!(
                    "{}-{}-b07-{rust}-{controlled}.json",
                    std::process::id(),
                    target.serial,
                )),
                serde_json::to_vec_pretty(&report).unwrap(),
            )
            .unwrap();
        }
        target.process.shutdown().await;
        control.process.shutdown().await;
    }
}
