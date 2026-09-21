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
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
type Owner = Arc<OwnerState>;

pub(super) struct OwnerState {
    // Retirement never waits for the disk worker's mutex. Queued writers
    // recheck this fence after obtaining that mutex and before storing bytes.
    retired: AtomicBool,
    retention: Mutex<Retention>,
}

impl OwnerState {
    fn new(max_bytes: usize, max_files: usize) -> Self {
        Self {
            retired: AtomicBool::new(false),
            retention: Mutex::new(Retention::new(max_bytes, max_files)),
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
    Chunk(Vec<u8>),
    Finish,
}

pub(super) struct Writer {
    sender: tokio::sync::mpsc::Sender<Message>,
    result: tokio::sync::oneshot::Receiver<Outcome>,
}

impl Writer {
    pub(super) fn start(owner: Owner, scope: String, limit: usize) -> Self {
        // Four 8-KiB pipe chunks: backpressure bounds queued disk work. The
        // worker drains this queue when a cancelled reader drops its sender.
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
                let Message::Chunk(chunk) = message else {
                    let _ = result_sender.send(outcome);
                    return;
                };
                if outcome.error
                    || outcome.truncated
                    || outcome.spill.as_ref().is_some_and(Spill::expired)
                {
                    continue;
                }
                let take = chunk.len().min(limit.saturating_sub(outcome.bytes));
                outcome.truncated = take < chunk.len();
                if take == 0 {
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
                match retention.append(id, &chunk[..take]) {
                    Ok(n) => outcome.bytes += n,
                    Err(()) => outcome.error = true,
                }
            }
            // No Finish means cancellation. Dropping the uncommitted lease
            // schedules safe cleanup, including a cancelled result receiver.
        });
        Self { sender, result }
    }

    pub(super) async fn chunk(&self, bytes: &[u8]) {
        let _ = self.sender.send(Message::Chunk(bytes.to_vec())).await;
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
    use super::*;

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
        let writer = Writer::start(owner.clone(), "cancelled-call".into(), 32);
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
        let writer = Writer::start(store.clone(), "queued-before-retirement".into(), 8);
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
        let writer = Writer::start(store.clone(), "host-call-scope".into(), 8);
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
        let writer = Writer::start(store, "late-call".into(), 8);
        writer.chunk(b"late").await;
        assert!(writer.finish().await.error);
    }

    #[tokio::test]
    async fn owners_are_isolated_and_expired_capture_never_advertises_a_path() {
        let first = Arc::new(OwnerState::new(8, 1));
        let other = Arc::new(OwnerState::new(8, 1));
        let writer = Writer::start(first.clone(), "first-scope".into(), 8);
        writer.chunk(b"abcdef").await;
        let outcome = writer.finish().await;
        let mut capture = super::super::Capture::empty();
        capture.total_bytes = 6;
        capture.spill = outcome.spill;
        capture.fit_to_budget(0);
        let first_path = capture.spill.as_ref().unwrap().path.clone();
        let writer = Writer::start(other.clone(), "other-scope".into(), 8);
        writer.chunk(b"other").await;
        let mut other_outcome = writer.finish().await;
        other_outcome.spill.as_mut().unwrap().retain();
        let other_path = other_outcome.spill.as_ref().unwrap().path.clone();
        let writer = Writer::start(first.clone(), "new-scope".into(), 8);
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
