//! Owner-scoped spill retention. All filesystem work runs on blocking workers;
//! the async pipe reader only sends bounded chunks and receives metadata.
use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::secure_fs::{create_bound_private_directory, PrivateDirectory};

const OWNER_BYTES: usize = 64 * 1024 * 1024;
const OWNER_FILES: usize = 32;
const CHUNK_BYTES: usize = 8192;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
type Owner = Arc<OwnerState>;

pub(super) struct OwnerState {
    // Retirement never waits for the disk worker's mutex. Queued writers
    // recheck this fence after obtaining that mutex and before storing bytes.
    retired: AtomicBool,
    retention: Mutex<Retention>,
    #[cfg(test)]
    work: Work,
}

#[cfg(test)]
#[derive(Default)]
struct Work {
    workers: std::sync::atomic::AtomicUsize,
    files: std::sync::atomic::AtomicUsize,
    chunks: std::sync::atomic::AtomicUsize,
    copied_bytes: std::sync::atomic::AtomicUsize,
}

impl OwnerState {
    fn new(max_bytes: usize, max_files: usize) -> Self {
        Self {
            retired: AtomicBool::new(false),
            retention: Mutex::new(Retention::new(max_bytes, max_files)),
            #[cfg(test)]
            work: Work::default(),
        }
    }
}

fn owners() -> &'static Mutex<HashMap<String, Owner>> {
    static OWNERS: OnceLock<Mutex<HashMap<String, Owner>>> = OnceLock::new();
    OWNERS.get_or_init(Default::default)
}

pub(super) fn owner(key: &str) -> Owner {
    owners()
        .lock()
        .unwrap()
        .entry(key.to_owned())
        .or_insert_with(|| Arc::new(OwnerState::new(OWNER_BYTES, OWNER_FILES)))
        .clone()
}

/// Called at the host resource-owner boundary, never with model arguments.
/// Admission is fenced synchronously without acquiring a disk-held lock.
pub(super) fn release_owner(key: &str) {
    let owner = {
        let stores = owners().lock().unwrap();
        let Some(owner) = stores.get(key).cloned() else {
            return;
        };
        owner.retired.store(true, Ordering::Release);
        owner
    };
    let key = key.to_owned();
    blocking_cleanup(move || {
        let empty = {
            let mut retention = owner.retention.lock().unwrap();
            retention.close();
            retention.entries.is_empty()
        };
        // Keep the fenced entry visible until deletion finishes. A concurrent
        // lookup of this same resource owner cannot create a second 64-MiB
        // store while the first still occupies disk. Failed cleanup retains
        // the tombstone and its charged quota rather than enabling a bypass.
        if empty {
            let mut stores = owners().lock().unwrap();
            if stores
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current, &owner))
            {
                stores.remove(&key);
            }
        }
    });
}

fn blocking_cleanup(f: impl FnOnce() + Send + 'static) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn_blocking(f);
    } else {
        // Agent destruction can follow runtime destruction. Never run disk
        // cleanup inline on a potential UI/async owner thread.
        let _ = std::thread::Builder::new()
            .name("bash-spill-cleanup".into())
            .spawn(f);
    }
}

struct Entry {
    id: u64,
    _scope: String,
    path: PathBuf,
    file: std::fs::File,
    bytes: usize,
    expired: Arc<AtomicBool>,
}

pub(super) struct Retention {
    directory: Option<PrivateDirectory>,
    entries: VecDeque<Entry>,
    bytes: usize,
    max_bytes: usize,
    max_files: usize,
    closed: bool,
}

impl Retention {
    fn new(max_bytes: usize, max_files: usize) -> Self {
        Self {
            directory: None,
            entries: VecDeque::new(),
            bytes: 0,
            max_bytes,
            max_files,
            closed: false,
        }
    }

    fn remove(&mut self, id: u64) -> Result<(), ()> {
        let Some(index) = self.entries.iter().position(|entry| entry.id == id) else {
            return Ok(());
        };
        let entry = &self.entries[index];
        // Expire the capability even if filesystem tampering prevents cleanup.
        // Failed deletion keeps its quota charged and prevents further growth.
        entry.expired.store(true, Ordering::Release);
        // Check the original file identity before asking the descriptor-bound
        // private-directory capability to remove its generated child name.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let original = entry.file.metadata().map_err(|_| ())?;
            match std::fs::symlink_metadata(&entry.path) {
                Ok(current)
                    if current.dev() == original.dev() && current.ino() == original.ino() => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err(()),
            }
        }
        self.directory
            .as_ref()
            .unwrap()
            .remove_regular_file_if_exists(&entry.path)
            .map_err(|_| ())?;
        let entry = self.entries.remove(index).unwrap();
        entry.expired.store(true, Ordering::Release);
        self.bytes -= entry.bytes;
        Ok(())
    }

    fn create(&mut self, scope: String) -> Result<(u64, PathBuf, Arc<AtomicBool>), ()> {
        if self.closed || self.max_files == 0 {
            return Err(());
        }
        while self.entries.len() >= self.max_files {
            self.remove(self.entries.front().unwrap().id)?;
        }
        if self.directory.is_none() {
            let root = std::env::temp_dir()
                .canonicalize()
                .map_err(|_| ())?
                .join("octet-bash-spills");
            self.directory = Some(create_bound_private_directory(&root, "owner-").map_err(|_| ())?);
        }
        let directory = self.directory.as_ref().unwrap();
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = directory.path().join(format!("{id}.log"));
        let file = directory
            .create_regular_file_for_append(&path)
            .map_err(|_| ())?;
        let expired = Arc::new(AtomicBool::new(false));
        self.entries.push_back(Entry {
            id,
            _scope: scope,
            path: path.clone(),
            file,
            bytes: 0,
            expired: expired.clone(),
        });
        Ok((id, path, expired))
    }

    fn append(&mut self, id: u64, bytes: &[u8]) -> Result<usize, ()> {
        if self.closed {
            return Err(());
        }
        if !self.entries.iter().any(|entry| entry.id == id) {
            return Ok(0);
        }
        while bytes.len() > self.max_bytes.saturating_sub(self.bytes) {
            let oldest = self.entries.front().unwrap().id;
            self.remove(oldest)?;
            if oldest == id {
                // An old active capture may evict itself. Never evict newer
                // output for a writer whose lease has already expired.
                return Ok(0);
            }
        }
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) else {
            return Ok(0);
        };
        // Account even a short successful write before any subsequent error.
        let mut written = 0;
        while written < bytes.len() {
            let n = entry.file.write(&bytes[written..]).map_err(|_| ())?;
            if n == 0 {
                return Err(());
            }
            written += n;
            entry.bytes += n;
            self.bytes += n;
        }
        Ok(written)
    }

    fn close(&mut self) {
        self.closed = true;
        let ids: Vec<_> = self.entries.iter().map(|entry| entry.id).collect();
        for id in ids {
            let _ = self.remove(id);
        }
        if self.entries.is_empty() {
            if let Some(directory) = self.directory.take() {
                let _ = directory.remove_empty_if_exists();
            }
        }
    }
}

/// The capture's pending lease cleans up on cancellation or complete inline
/// output. Only a final truncated result commits it to owner retention.
pub(super) struct Spill {
    owner: Owner,
    id: u64,
    pub(super) path: PathBuf,
    expired: Arc<AtomicBool>,
    retained: bool,
}

impl Spill {
    pub(super) fn expired(&self) -> bool {
        self.expired.load(Ordering::Acquire)
    }
    pub(super) fn retain(&mut self) {
        self.retained = true;
    }
}

impl Drop for Spill {
    fn drop(&mut self) {
        if !self.retained {
            let owner = self.owner.clone();
            let id = self.id;
            blocking_cleanup(move || {
                let _ = owner.retention.lock().unwrap().remove(id);
            });
        }
    }
}

pub(super) struct Outcome {
    pub(super) spill: Option<Spill>,
    pub(super) bytes: usize,
    pub(super) truncated: bool,
    pub(super) error: bool,
}

enum Message {
    Chunk { bytes: Vec<u8>, truncated: bool },
    Finish,
}

pub(super) struct Writer {
    sender: tokio::sync::mpsc::Sender<Message>,
    result: tokio::sync::oneshot::Receiver<Outcome>,
    remaining: usize,
    truncated: bool,
    #[cfg(test)]
    owner: Owner,
}

impl Writer {
    pub(super) fn start(owner: Owner, scope: String, limit: usize) -> Self {
        // Four 8-KiB chunks, including promotion of a larger provisional
        // capture: backpressure bounds queued disk work. The worker drains this
        // queue when a cancelled reader drops its sender.
        #[cfg(test)]
        owner.work.workers.fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        let work_owner = owner.clone();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        let (result_sender, result) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let mut outcome = Outcome {
                spill: None,
                bytes: 0,
                truncated: false,
                error: false,
            };
            while let Some(message) = receiver.blocking_recv() {
                let Message::Chunk { bytes, truncated } = message else {
                    let _ = result_sender.send(outcome);
                    return;
                };
                if outcome.error
                    || outcome.truncated
                    || outcome.spill.as_ref().is_some_and(Spill::expired)
                {
                    continue;
                }
                outcome.truncated = truncated;
                if bytes.is_empty() {
                    continue;
                }
                let mut retention = owner.retention.lock().unwrap();
                if owner.retired.load(Ordering::Acquire) {
                    outcome.error = true;
                    continue;
                }
                if outcome.spill.is_none() {
                    match retention.create(scope.clone()) {
                        Ok((id, path, expired)) => {
                            #[cfg(test)]
                            owner.work.files.fetch_add(1, Ordering::Relaxed);
                            outcome.spill = Some(Spill {
                                owner: owner.clone(),
                                id,
                                path,
                                expired,
                                retained: false,
                            })
                        }
                        Err(()) => {
                            outcome.error = true;
                            continue;
                        }
                    }
                }
                let id = outcome.spill.as_ref().unwrap().id;
                if owner.retired.load(Ordering::Acquire) {
                    outcome.error = true;
                    continue;
                }
                match retention.append(id, &bytes) {
                    Ok(n) => outcome.bytes += n,
                    Err(()) => outcome.error = true,
                }
            }
            // No Finish means cancellation. Dropping the uncommitted lease
            // schedules safe cleanup, including a cancelled result receiver.
        });
        Self {
            sender,
            result,
            remaining: limit,
            truncated: false,
            #[cfg(test)]
            owner: work_owner,
        }
    }

    pub(super) async fn chunk(&mut self, bytes: &[u8]) {
        // Stop copying/queueing as soon as the exact prefix limit is reached,
        // not just writing. The pipe reader must still drain and publish output.
        let take = bytes.len().min(self.remaining);
        let truncated = !self.truncated && take < bytes.len();
        self.truncated |= truncated;
        self.remaining -= take;
        let mut chunks = bytes[..take].chunks(CHUNK_BYTES).peekable();
        while let Some(chunk) = chunks.next() {
            #[cfg(test)]
            {
                self.owner.work.chunks.fetch_add(1, Ordering::Relaxed);
                self.owner
                    .work
                    .copied_bytes
                    .fetch_add(chunk.len(), Ordering::Relaxed);
            }
            let _ = self
                .sender
                .send(Message::Chunk {
                    bytes: chunk.to_vec(),
                    truncated: truncated && chunks.peek().is_none(),
                })
                .await;
        }
        if take == 0 && truncated {
            // Exactly filling the cap is not truncation until the next byte.
            // Report that boundary once, ordered after all prefix writes, so
            // earlier storage errors/expiry retain their original diagnostics.
            let _ = self
                .sender
                .send(Message::Chunk {
                    bytes: Vec::new(),
                    truncated: true,
                })
                .await;
        }
    }

    pub(super) async fn finish(self) -> Outcome {
        let _ = self.sender.send(Message::Finish).await;
        self.result.await.unwrap_or(Outcome {
            spill: None,
            bytes: 0,
            truncated: false,
            error: true,
        })
    }
}

#[cfg(test)]
mod tests {
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
        let checkpoints = super::super::BashCheckpoints::new(
            sink.clone(),
            super::super::BASH_CHECKPOINT_INTERVAL,
        );
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
}
