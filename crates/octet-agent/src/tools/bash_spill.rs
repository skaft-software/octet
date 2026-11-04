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

// `spill` is mounted from `bash.rs` through `#[path = "bash_spill.rs"]`, so this
// file's submodules resolve against `tools/` rather than a `bash_spill/`
// directory of their own; the tests are named explicitly to say where they live.
#[cfg(test)]
#[path = "bash_spill/tests.rs"]
mod tests;
