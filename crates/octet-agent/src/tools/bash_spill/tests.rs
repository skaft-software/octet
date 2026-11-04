//! Regressions for bash output spilling to disk.
//!
//! Separate from `bash_spill.rs` so the spill module stays a readable
//! description of the spill file format and its ownership rules.
use super::super::{
    read_bounded_with_spill_limit, rebalance_captures, Capture, OutputStream, ToolProgressSink,
};
use super::*;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf};

struct TestOwner {
    key: String,
    store: Owner,
}

impl TestOwner {
    fn new() -> Self {
        let key = format!(
            "lazy-spill-test-{}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        );
        let store = owner(&key);
        Self { key, store }
    }

    fn work(&self) -> [usize; 4] {
        let work = &self.store.work;
        [&work.workers, &work.files, &work.chunks, &work.copied_bytes]
            .map(|counter| counter.load(Ordering::Relaxed))
    }
}

impl Drop for TestOwner {
    fn drop(&mut self) {
        release_owner(&self.key);
    }
}

struct Chunked<'a> {
    bytes: &'a [u8],
    chunk_size: usize,
}

impl AsyncRead for Chunked<'_> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let take = self.bytes.len().min(self.chunk_size).min(buf.remaining());
        buf.put_slice(&self.bytes[..take]);
        self.bytes = &self.bytes[take..];
        Poll::Ready(Ok(()))
    }
}

async fn capture(
    owner: &TestOwner,
    bytes: &[u8],
    chunk_size: usize,
    budget: usize,
    limit: usize,
) -> Capture {
    read_bounded_with_spill_limit(
        &mut Some(Chunked { bytes, chunk_size }),
        budget,
        &ToolProgressSink::null(),
        OutputStream::Stdout,
        None,
        limit,
        (&owner.key, "capture"),
    )
    .await
}

#[tokio::test]
async fn production_stream_limit_preserves_full_and_partial_prefix_labels() {
    let limit = super::super::MAX_BASH_SPILL_BYTES;
    for length in [limit, limit + CHUNK_BYTES] {
        let owner = TestOwner::new();
        let bytes = vec![b'x'; length];
        let mut out = capture(&owner, &bytes, CHUNK_BYTES, 1024, limit).await;
        out.fit_to_budget(1024);
        assert_eq!(out.total_bytes, length);
        assert_eq!(out.spill_bytes, limit);
        assert_eq!(out.spill_truncated, length > limit);
        assert!(!out.spill_error);
        let path = out.spill.as_ref().unwrap().path.clone();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), limit as u64);
        let rendered = out.render("stdout");
        assert_eq!(rendered.contains("full_output_path="), length == limit);
        assert_eq!(rendered.contains("partial_output_path="), length > limit);
        drop(out);
        assert!(path.exists(), "retained result outlives its capture");
    }
}

#[tokio::test]
async fn empty_and_fitting_streams_do_no_spill_work() {
    let owner = TestOwner::new();
    for budget in [0, 1, 7, 32, CHUNK_BYTES * 3 + 1] {
        let bytes: Vec<_> = (0..budget).map(|n| n as u8).collect();
        for length in [0, budget / 2, budget] {
            // Fitting output does no spill work even above the spill cap.
            let mut out = capture(&owner, &bytes[..length], 3, budget, 2).await;
            let mut err = capture(&owner, &bytes[..budget - length], 3, budget, 2).await;
            rebalance_captures(&mut out, &mut err, budget).await;
            assert_eq!(out.head, bytes[..length]);
            assert_eq!(err.head, bytes[..budget - length]);
            assert!(!err.truncated);
            assert!(err.spill.is_none());
            assert!(out.tail.is_empty());
            assert!(!out.truncated);
            assert!(out.spill.is_none());
            assert_eq!(owner.work(), [0, 0, 0, 0]);
        }
    }
    let mut absent: Option<std::io::Cursor<&[u8]>> = None;
    let out = read_bounded_with_spill_limit(
        &mut absent,
        0,
        &ToolProgressSink::null(),
        OutputStream::Stdout,
        None,
        8,
        (&owner.key, "absent"),
    )
    .await;
    assert_eq!(out.total_bytes, 0);
    assert_eq!(owner.work(), [0, 0, 0, 0]);
    assert!(owner.store.retention.lock().unwrap().directory.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn fitting_output_progress_and_checkpoints_are_live_before_eof() {
    use crate::tool::{PartialOutputCheckpointSink, ToolError, ToolProgress};
    use tokio::io::AsyncWriteExt;

    #[derive(Default)]
    struct Checkpoints(Mutex<Vec<String>>);
    impl PartialOutputCheckpointSink for Checkpoints {
        fn checkpoint_partial_output(&self, snapshot: &str) -> Result<(), ToolError> {
            self.0.lock().unwrap().push(snapshot.to_owned());
            Ok(())
        }
    }
    let owner = TestOwner::new();
    let sink = Arc::new(Checkpoints::default());
    let checkpoints =
        super::super::BashCheckpoints::new(sink.clone(), super::super::BASH_CHECKPOINT_INTERVAL);
    let (mut input, reader) = tokio::io::duplex(64);
    let mut reader = Some(reader);
    let (progress, mut updates) = ToolProgressSink::bounded_channel();
    let mut drain = Box::pin(read_bounded_with_spill_limit(
        &mut reader,
        64,
        &progress,
        OutputStream::Stdout,
        Some(&checkpoints),
        8,
        (&owner.key, "live-fitting"),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::select! {
            _ = &mut drain => panic!("pipe has not reached EOF"),
            update = async {
                input.write_all(b"first\n").await.unwrap();
                updates.recv().await.unwrap()
            } => {
                let ToolProgress::Output { stream, bytes } = update else {
                    panic!("unexpected progress");
                };
                assert_eq!(stream, OutputStream::Stdout);
                assert_eq!(bytes.as_ref(), b"first\n");
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        sink.0.lock().unwrap().as_slice(),
        ["stdout: 6 bytes seen\nfirst\nstderr: 0 bytes seen"]
    );
    assert_eq!(checkpoints.stats().requested, 1);
    drop(drain);
    assert_eq!(owner.work(), [0, 0, 0, 0]);
    assert!(owner.store.retention.lock().unwrap().directory.is_none());
}

#[tokio::test]
async fn promotion_preserves_binary_prefix_at_raw_byte_boundaries() {
    let owner = TestOwner::new();
    for budget in [0_usize, 1, 7, 16, CHUNK_BYTES + 1] {
        // Includes NUL, invalid UTF-8 and multibyte sequences split across
        // the provisional head/tail, pipe chunks and the spill limit.
        let pattern = b"\0\xff\xe2\x82\xac\xf0\x9f\x98\x80\n";
        let bytes: Vec<_> = pattern.iter().copied().cycle().take(budget + 19).collect();
        for chunk_size in [1, 3, CHUNK_BYTES] {
            for limit in [budget.saturating_sub(1), budget + 1, bytes.len()] {
                let before = owner.work();
                let mut out = capture(&owner, &bytes, chunk_size, budget, limit).await;
                assert_eq!(out.total_bytes, bytes.len());
                assert_eq!(out.head, bytes[..budget / 2]);
                let tail_cap = budget - budget / 2;
                assert_eq!(
                    out.tail.iter().copied().collect::<Vec<_>>(),
                    bytes[bytes.len() - tail_cap..]
                );
                assert_eq!(out.spill_bytes, limit);
                assert_eq!(out.spill_truncated, limit < bytes.len());
                assert!(!out.spill_error);
                if limit == 0 {
                    assert!(out.spill.is_none());
                } else {
                    assert_eq!(
                        std::fs::read(&out.spill.as_ref().unwrap().path).unwrap(),
                        bytes[..limit]
                    );
                }
                let after = owner.work();
                assert_eq!(after[0] - before[0], 1);
                assert_eq!(after[1] - before[1], usize::from(limit > 0));
                assert_eq!(
                    after[3] - before[3],
                    limit,
                    "prefix must be copied only once"
                );
                out.fit_to_budget(budget);
                let rendered = out.render("stdout");
                assert_eq!(
                    rendered.contains("spill_truncated=true"),
                    limit < bytes.len()
                );
                assert_eq!(rendered.contains("full_output_path="), limit == bytes.len());
            }
        }
    }
}

#[tokio::test]
async fn shared_budget_only_spills_materialize_before_shrinking() {
    let owner = TestOwner::new();
    let budget = 4096;
    let large = b"large\0\xff\n".repeat(512);
    let small = b"small\n";
    let mut out = capture(&owner, small, 1, budget, budget).await;
    let mut err = capture(&owner, &large, 3, budget, budget).await;
    assert_eq!(owner.work(), [0, 0, 0, 0]);
    rebalance_captures(&mut out, &mut err, budget).await;
    assert_eq!(out.head, small);
    assert!(out.spill.is_none());
    assert!(!out.truncated);
    assert!(err.truncated);
    assert_eq!(
        std::fs::read(&err.spill.as_ref().unwrap().path).unwrap(),
        large
    );
    assert_eq!(owner.work()[0..2], [1, 1]);
    assert_eq!(owner.work()[3], large.len());

    // Only stderr initially needs a spill. Its path overhead then truncates
    // stdout too, entirely within stdout's provisional head. Neither stream
    // may be shrunk until both exact prefixes have been materialized.
    let bytes: Vec<_> = (0..budget / 2 - 64).map(|n| n as u8).collect();
    let mut out = capture(&owner, &bytes, 3, budget, budget).await;
    let mut err = capture(&owner, &large, 3, budget, budget).await;
    assert_eq!(owner.work()[0..2], [1, 1]);
    rebalance_captures(&mut out, &mut err, budget).await;
    assert!(out.truncated && err.truncated);
    for (stream, original) in [(&out, bytes.as_slice()), (&err, large.as_slice())] {
        assert_eq!(
            std::fs::read(&stream.spill.as_ref().unwrap().path).unwrap(),
            original
        );
        assert_eq!(stream.head, original[..stream.head.len()]);
        let tail: Vec<_> = stream.tail.iter().copied().collect();
        assert_eq!(tail, original[original.len() - tail.len()..]);
        assert!(stream.tail.len().abs_diff(stream.head.len()) <= 1);
        assert!(stream.render("stdout").contains("full_output_path="));
    }
    assert_eq!(owner.work()[0..2], [3, 3]);
    assert_eq!(owner.work()[3], 2 * large.len() + bytes.len());
}

#[tokio::test]
async fn late_promotion_bounds_queue_chunks_and_copies_only_the_spill_prefix() {
    let owner = TestOwner::new();
    let bytes: Vec<_> = (0..CHUNK_BYTES * 10).map(|n| n as u8).collect();
    let limit = CHUNK_BYTES * 3 + 7;
    let mut out = capture(&owner, &bytes, CHUNK_BYTES, bytes.len(), limit).await;
    assert_eq!(owner.work(), [0, 0, 0, 0]);
    assert!(out.spill_if_truncated(0).await);
    assert_eq!(owner.work(), [1, 1, 4, limit]);
    assert_eq!(
        std::fs::read(&out.spill.as_ref().unwrap().path).unwrap(),
        bytes[..limit]
    );
    assert!(out.spill_truncated);
    // A failed or successful promotion is never repeated during budgeting.
    assert!(!out.spill_if_truncated(0).await);
    assert_eq!(owner.work(), [1, 1, 4, limit]);
}

#[tokio::test]
async fn lazy_spills_preserve_retirement_and_storage_errors() {
    let owner = TestOwner::new();
    let bytes = b"complete provisional bytes";
    let mut out = capture(&owner, bytes, 3, bytes.len(), bytes.len()).await;
    release_owner(&owner.key);
    assert!(owner.store.retired.load(Ordering::Acquire));
    assert!(out.spill_if_truncated(0).await);
    assert!(out.spill_error);
    assert!(out.spill.is_none());
    assert_eq!(owner.work()[0..2], [1, 0]);

    let owner = TestOwner::new();
    owner.store.retention.lock().unwrap().max_files = 0;
    let mut out = capture(&owner, bytes, 3, 4, 4).await;
    out.fit_to_budget(4);
    assert_eq!(out.total_bytes, bytes.len());
    assert!(out.spill_error);
    assert!(
        !out.spill_truncated,
        "storage failed before the quota boundary"
    );
    assert!(out.spill.is_none());
    assert_eq!(owner.work()[0..2], [1, 0]);
    let rendered = out.render("stdout");
    assert!(rendered.contains("spill_error=true"));
    assert!(!rendered.contains("output_path="));
}

#[tokio::test(flavor = "current_thread")]
async fn beyond_cap_drains_and_cancels_even_with_the_disk_worker_blocked() {
    use crate::tool::ToolProgress;
    use tokio::io::AsyncWriteExt;

    let owner = TestOwner::new();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let gated_store = owner.store.clone();
    let gate = tokio::task::spawn_blocking(move || {
        let _guard = gated_store.retention.lock().unwrap();
        let _ = ready_tx.send(());
        // Dropping the sender on panic also releases the worker.
        let _ = release_rx.recv();
    });
    ready_rx.await.unwrap();
    let (mut input, reader) = tokio::io::duplex(64);
    let mut reader = Some(reader);
    let (progress, mut updates) = ToolProgressSink::bounded_channel();
    let mut drain = Box::pin(read_bounded_with_spill_limit(
        &mut reader,
        12,
        &progress,
        OutputStream::Stdout,
        None,
        9,
        (&owner.key, "cancel-beyond-cap"),
    ));
    let bytes: Vec<_> = (0..4096).map(|n| n as u8).collect();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::select! {
            _ = &mut drain => panic!("pipe has not reached EOF"),
            () = async {
                let (written, seen) = tokio::join!(input.write_all(&bytes), async {
                    let mut seen = Vec::new();
                    while seen.len() < bytes.len() {
                        let ToolProgress::Output { stream, bytes } = updates.recv().await.unwrap() else {
                            panic!("unexpected progress");
                        };
                        assert_eq!(stream, OutputStream::Stdout);
                        seen.extend_from_slice(&bytes);
                    }
                    seen
                });
                written.unwrap();
                assert_eq!(seen, bytes);
            } => {}
        }
    })
    .await
    .expect("a full spill must not backpressure later pipe output");
    assert_eq!(owner.work(), [1, 0, 1, 9]);
    drop(drain);
    release_tx.send(()).unwrap();
    gate.await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let store = owner.store.clone();
            let cleaned = tokio::task::spawn_blocking(move || {
                let retention = store.retention.lock().unwrap();
                retention.directory.is_some() && retention.entries.is_empty()
            })
            .await
            .unwrap();
            if cleaned {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled promotion must remove its uncommitted spill");
    assert_eq!(owner.work(), [1, 1, 1, 9]);
}

#[test]
fn aggregate_count_and_bytes_evict_oldest_even_when_active() {
    let mut retention = Retention::new(6, 2);
    let (first, first_path, first_expired) = retention.create("call-a".into()).unwrap();
    retention.append(first, b"aaa").unwrap();
    let (second, second_path, second_expired) = retention.create("call-b".into()).unwrap();
    retention.append(second, b"bbb").unwrap();
    let (third, third_path, _) = retention.create("call-c".into()).unwrap();
    assert!(first_expired.load(Ordering::Acquire));
    assert!(!first_path.exists());
    assert_eq!(retention.append(first, b"x").unwrap(), 0);
    retention.append(third, b"cccc").unwrap();
    assert!(second_expired.load(Ordering::Acquire));
    assert!(!second_path.exists());
    assert_eq!(retention.bytes, 4);
    assert_eq!(retention.entries.len(), 1);
    assert_eq!(std::fs::read(&third_path).unwrap(), b"cccc");
    let directory = retention.directory.as_ref().unwrap().path().to_owned();
    retention.close();
    assert!(!third_path.exists());
    assert!(!directory.exists());
    assert!(retention.create("retired".into()).is_err());
}

#[test]
fn expired_writer_cannot_evict_newer_output() {
    let mut retention = Retention::new(6, 2);
    let (first, _, expired) = retention.create("old".into()).unwrap();
    retention.append(first, b"aaa").unwrap();
    let (second, path, _) = retention.create("new".into()).unwrap();
    retention.append(second, b"bbb").unwrap();
    assert_eq!(retention.append(first, b"123456").unwrap(), 0);
    assert!(expired.load(Ordering::Acquire));
    assert_eq!(retention.append(first, b"123456").unwrap(), 0);
    assert_eq!(std::fs::read(path).unwrap(), b"bbb");
    assert_eq!(retention.bytes, 3);
    retention.close();
}

#[test]
fn cleanup_never_follows_replaced_files_or_parent_symlinks() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let external = tempfile::tempdir().unwrap();
        let external_file = external.path().join("keep");
        std::fs::write(&external_file, b"external").unwrap();
        let mut retention = Retention::new(6, 1);
        let (id, path, _) = retention.create("call".into()).unwrap();
        retention.append(id, b"ours").unwrap();
        std::fs::remove_file(&path).unwrap();
        symlink(&external_file, &path).unwrap();
        assert!(retention.remove(id).is_err());
        // Failed eviction must not release quota for another file.
        assert!(retention.create("next".into()).is_err());
        assert_eq!(std::fs::read(&external_file).unwrap(), b"external");
        std::fs::remove_file(&path).unwrap();
        retention.remove(id).unwrap();

        let (id, path, _) = retention.create("parent".into()).unwrap();
        let directory = path.parent().unwrap();
        let moved = directory.with_extension("moved");
        std::fs::rename(directory, &moved).unwrap();
        symlink(external.path(), directory).unwrap();
        assert!(retention.remove(id).is_err());
        assert_eq!(std::fs::read(&external_file).unwrap(), b"external");
        std::fs::remove_file(directory).unwrap();
        std::fs::rename(moved, directory).unwrap();
        retention.close();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn blocked_disk_worker_does_not_block_control_and_cancel_cleans_up() {
    let owner = Arc::new(OwnerState::new(64, 2));
    // An uncontended test-only gate models a blocked filesystem operation
    // on the capture worker, not on Tokio's current-thread executor.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let gated_owner = owner.clone();
    let gate = tokio::task::spawn_blocking(move || {
        let _guard = gated_owner.retention.lock().unwrap();
        let _ = ready_tx.send(());
        let _ = release_rx.recv();
    });
    ready_rx.await.unwrap();
    let mut writer = Writer::start(owner.clone(), "cancelled-call".into(), 32);
    writer.chunk(b"first").await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    drop(writer);
    release_tx.send(()).unwrap();
    gate.await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let owner = owner.clone();
            let settled = tokio::task::spawn_blocking(move || {
                let retention = owner.retention.lock().unwrap();
                // The worker must have created and then removed its file.
                retention.directory.is_some() && retention.entries.is_empty()
            })
            .await
            .unwrap();
            if settled {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    tokio::task::spawn_blocking(move || owner.retention.lock().unwrap().close())
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn retirement_fences_queued_writes_without_waiting_for_disk() {
    let store = owner("spill-retirement-gate-test");
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let gated_store = store.clone();
    let gate = tokio::task::spawn_blocking(move || {
        let _guard = gated_store.retention.lock().unwrap();
        let _ = ready_tx.send(());
        let _ = release_rx.recv();
    });
    ready_rx.await.unwrap();
    let mut writer = Writer::start(store.clone(), "queued-before-retirement".into(), 8);
    writer.chunk(b"queued").await;
    release_owner("spill-retirement-gate-test");
    assert!(store.retired.load(Ordering::Acquire));
    assert!(Arc::ptr_eq(&owner("spill-retirement-gate-test"), &store));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    release_tx.send(()).unwrap();
    gate.await.unwrap();
    let outcome = writer.finish().await;
    assert!(outcome.error);
    assert!(outcome.spill.is_none());
    assert_eq!(outcome.bytes, 0);
}

#[tokio::test]
async fn host_owner_teardown_removes_retained_paths_and_closes_active_store() {
    let store = owner("spill-teardown-test");
    let mut writer = Writer::start(store.clone(), "host-call-scope".into(), 8);
    writer.chunk(b"retained").await;
    let mut outcome = writer.finish().await;
    outcome.spill.as_mut().unwrap().retain();
    let path = outcome.spill.as_ref().unwrap().path.clone();
    assert!(path.exists());
    release_owner("spill-teardown-test");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let store = store.clone();
            if tokio::task::spawn_blocking(move || store.retention.lock().unwrap().closed)
                .await
                .unwrap()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!path.exists());
    assert!(outcome.spill.as_ref().unwrap().expired());
    let mut writer = Writer::start(store, "late-call".into(), 8);
    writer.chunk(b"late").await;
    assert!(writer.finish().await.error);
}

#[tokio::test]
async fn owners_are_isolated_and_expired_capture_never_advertises_a_path() {
    let first = Arc::new(OwnerState::new(8, 1));
    let other = Arc::new(OwnerState::new(8, 1));
    let mut writer = Writer::start(first.clone(), "first-scope".into(), 8);
    writer.chunk(b"abcdef").await;
    let outcome = writer.finish().await;
    let mut capture = super::super::Capture::empty();
    capture.total_bytes = 6;
    capture.spill = outcome.spill;
    capture.fit_to_budget(0);
    let first_path = capture.spill.as_ref().unwrap().path.clone();
    let mut writer = Writer::start(other.clone(), "other-scope".into(), 8);
    writer.chunk(b"other").await;
    let mut other_outcome = writer.finish().await;
    other_outcome.spill.as_mut().unwrap().retain();
    let other_path = other_outcome.spill.as_ref().unwrap().path.clone();
    let mut writer = Writer::start(first.clone(), "new-scope".into(), 8);
    writer.chunk(b"new").await;
    let _outcome = writer.finish().await;
    assert!(!first_path.exists());
    assert!(other_path.exists());
    let rendered = capture.render("stdout");
    assert!(rendered.contains("spill_expired=true"));
    assert!(!rendered.contains("output_path="));
    tokio::task::spawn_blocking(move || {
        first.retention.lock().unwrap().close();
        other.retention.lock().unwrap().close();
    })
    .await
    .unwrap();
}
