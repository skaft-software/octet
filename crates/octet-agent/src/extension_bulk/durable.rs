//! Host-only durable session retention; no transfer locator or lease is saved.
use super::*;

const MAX_METADATA_BYTES: usize = 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DurableCatalog {
    version: u32,
    session: String,
    pub(super) blobs: Vec<BlobRef>,
    #[serde(skip)]
    original: Option<Vec<u8>>,
}

/// Bounded even when an explicitly configured record limit is very large.
struct MetadataBytes(Vec<u8>);
impl Write for MetadataBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_METADATA_BYTES {
            return Err(std::io::Error::other("bulk retention metadata limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct UnpublishedFile(Option<PathBuf>);
impl Drop for UnpublishedFile {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = secure_fs::remove_regular_file_if_exists(path);
        }
    }
}

pub(super) struct Reservation {
    pub session: String,
    pub reference: BlobRef,
    pub live: Arc<AtomicBool>,
}

struct Operation {
    id: String,
    session: String,
    reference: BlobRef,
    pending: Pending,
    root_lock: Arc<File>,
}

pub(crate) struct DurableRetainJob {
    operation: Operation,
    source: Arc<LocalFile>,
    directory: PathBuf,
    existing: bool,
}

pub(crate) struct PreparedDurableRetain {
    // Cleanup precedes the operation's reservation/root-lock drop.
    unpublished: Option<UnpublishedFile>,
    job: DurableRetainJob,
}

impl DurableRetainJob {
    /// Run on the blocking-I/O lane with no storage/disposition mutex held.
    pub(crate) fn run(self) -> Result<PreparedDurableRetain, BulkError> {
        if !self.operation.pending.live() {
            return Err(BulkError::Unavailable);
        }
        if self.existing {
            return Ok(PreparedDurableRetain {
                job: self,
                unpublished: None,
            });
        }
        let anchor = secure_fs::open_private_directory_for_lock(&self.directory)?;
        self.operation.root_lock.sync_all()?;
        let reference = &self.operation.reference;
        let path = self.directory.join(format!("{}.blob", reference.id));
        secure_fs::remove_regular_file_if_exists(&path)?; // unpublished crash orphan only
        let output = secure_fs::create_regular_file_for_append(&path)?;
        let unpublished = UnpublishedFile(Some(path));
        copy_files(
            self.source.open()?,
            output.try_clone()?,
            reference,
            reference.bytes,
            &self.operation.pending,
        )?;
        output.sync_all()?;
        anchor.sync_all()?;
        Ok(PreparedDurableRetain {
            job: self,
            unpublished: Some(unpublished),
        })
    }
}

pub(crate) struct DurableRecoverJob {
    operation: Operation,
    source: File,
    snapshot: LocalFile,
}

pub(crate) struct PreparedDurableRecover(DurableRecoverJob);

impl DurableRecoverJob {
    /// Reverify into a fresh host snapshot outside every admission mutex.
    pub(crate) fn run(self) -> Result<PreparedDurableRecover, BulkError> {
        let reference = &self.operation.reference;
        copy_files(
            self.source.try_clone()?,
            self.snapshot.open()?,
            reference,
            reference.bytes,
            &self.operation.pending,
        )?;
        Ok(PreparedDurableRecover(self))
    }
}

impl BulkStore {
    fn reserve_durable(
        &mut self,
        session: &str,
        reference: &BlobRef,
    ) -> Result<Operation, BulkError> {
        self.sweep();
        if self
            .durable_jobs
            .values()
            .any(|j| j.reference.id == reference.id)
            || self
                .durable_jobs
                .values()
                .filter(|j| j.session == session)
                .count()
                >= self.limits.write_tickets_per_generation
        {
            return Err(BulkError::QuotaExceeded);
        }
        let id = token()?;
        let live = Arc::new(AtomicBool::new(true));
        self.durable_jobs.insert(
            id.clone(),
            Reservation {
                session: session.to_owned(),
                reference: reference.clone(),
                live: live.clone(),
            },
        );
        Ok(Operation {
            id,
            session: session.to_owned(),
            reference: reference.clone(),
            pending: Pending::new(live),
            root_lock: self._root_lock.clone(),
        })
    }

    pub(super) fn cancel_durable(&mut self, session: &str, blob: Option<&str>) {
        for job in self
            .durable_jobs
            .values()
            .filter(|j| j.session == session && blob.is_none_or(|id| j.reference.id == id))
        {
            job.live.store(false, Ordering::Release);
        }
    }

    fn durable_directory(&self, session: &str) -> PathBuf {
        self.root.join(format!(
            "bulk-retained-{:x}",
            Sha256::digest(session.as_bytes())
        ))
    }

    pub(super) fn load_durable(&self, session: &str) -> Result<DurableCatalog, BulkError> {
        let path = self.durable_directory(session).join("metadata.json");
        let raw = match secure_fs::read_private_file_bounded(&path, MAX_METADATA_BYTES) {
            Ok(raw) => raw,
            Err(secure_fs::SecureFileError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DurableCatalog {
                    version: 1,
                    session: session.to_owned(),
                    blobs: Vec::new(),
                    original: None,
                });
            }
            Err(_) => return Err(BulkError::StorageUnavailable),
        };
        let mut catalog: DurableCatalog =
            serde_json::from_slice(&raw).map_err(|_| BulkError::StorageUnavailable)?;
        if catalog.version != 1 {
            return Err(BulkError::UnsupportedFeature);
        }
        if catalog.session != session {
            return Err(BulkError::Unavailable);
        }
        if catalog.blobs.len() > self.limits.blobs_per_owner {
            return Err(BulkError::QuotaExceeded);
        }
        let mut ids = HashSet::new();
        let mut bytes = 0_u64;
        for blob in &catalog.blobs {
            if blob.id.len() != 48
                || !blob
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || !ids.insert(&blob.id)
            {
                return Err(BulkError::StorageUnavailable);
            }
            blob.digest.validate()?;
            validate_media_type(&blob.media_type)?;
            if self
                .blobs
                .get(&blob.id)
                .is_some_and(|active| active.session != session || active.reference != *blob)
            {
                return Err(BulkError::StorageUnavailable);
            }
            bytes = bytes.saturating_add(blob.bytes);
            if blob.bytes > self.limits.object_bytes || bytes > self.limits.owner_bytes {
                return Err(BulkError::QuotaExceeded);
            }
        }
        catalog.original = Some(raw);
        Ok(catalog)
    }

    fn save_durable(&self, catalog: &DurableCatalog) -> Result<(), BulkError> {
        let mut bytes = MetadataBytes(Vec::new());
        serde_json::to_writer(&mut bytes, catalog).map_err(|_| BulkError::QuotaExceeded)?;
        let path = self
            .durable_directory(&catalog.session)
            .join("metadata.json");
        #[cfg(test)]
        tests::before_metadata_publish(&path)?;
        if let Err(error) = secure_fs::write_private_atomic_if_unchanged(
            &path,
            catalog.original.as_deref(),
            &bytes.0,
            MAX_METADATA_BYTES,
        ) {
            // A directory-sync error can follow the atomic rename. Roll back
            // only our exact newly written metadata, never another writer's.
            if let Some(old) = &catalog.original {
                let _ = secure_fs::write_private_atomic_if_unchanged(
                    &path,
                    Some(&bytes.0),
                    old,
                    MAX_METADATA_BYTES,
                );
            } else {
                let _ = secure_fs::remove_private_file_if_unchanged(
                    &path,
                    &bytes.0,
                    MAX_METADATA_BYTES,
                );
            }
            return Err(error.into());
        }
        Ok(())
    }

    /// Host request after successful result admission; reserves a bounded copy.
    pub(crate) fn prepare_retain_durable(
        &mut self,
        session: &str,
        reference: &BlobRef,
    ) -> Result<DurableRetainJob, BulkError> {
        let source = self
            .blobs
            .get(&reference.id)
            .filter(|b| {
                b.session == session
                    && b.retained
                    && b.parent.is_none()
                    && &b.reference == reference
            })
            .ok_or(BulkError::Unavailable)?
            .backing
            .clone();
        let catalog = self.load_durable(session)?;
        let existing = catalog.blobs.iter().any(|b| b == reference);
        let operation = self.reserve_durable(session, reference)?;
        Ok(DurableRetainJob {
            operation,
            source,
            directory: self.durable_directory(session),
            existing,
        })
    }

    /// Rechecks session retention and cancellation after expensive I/O. Only
    /// bounded catalog publication/sync occurs while holding the host gate.
    pub(crate) fn finish_retain_durable(
        &mut self,
        mut prepared: PreparedDurableRetain,
    ) -> Result<(), BulkError> {
        let op = &prepared.job.operation;
        if !op.pending.live()
            || !self.durable_jobs.contains_key(&op.id)
            || !self.blobs.get(&op.reference.id).is_some_and(|b| {
                b.session == op.session && b.retained && b.reference == op.reference
            })
        {
            return Err(BulkError::Unavailable);
        }
        let mut catalog = self.load_durable(&op.session)?;
        if !catalog.blobs.contains(&op.reference) {
            if prepared.unpublished.is_none() {
                return Err(BulkError::Unavailable);
            }
            catalog.blobs.push(op.reference.clone());
            self.save_durable(&catalog)?;
        }
        if let Some(file) = prepared.unpublished.as_mut() {
            file.0 = None;
        }
        self.durable_jobs.remove(&op.id);
        Ok(())
    }

    /// Explicit authenticated host reauthorization, never inferred from a transcript.
    pub(crate) fn prepare_recover_durable(
        &mut self,
        session: &str,
        reference: &BlobRef,
    ) -> Result<DurableRecoverJob, BulkError> {
        let catalog = self.load_durable(session)?;
        if !catalog.blobs.contains(reference) {
            return Err(BulkError::Unavailable);
        }
        let operation = self.reserve_durable(session, reference)?;
        let source = secure_fs::open_private_file_for_read(
            &self
                .durable_directory(session)
                .join(format!("{}.blob", reference.id)),
        )?;
        let snapshot = LocalFile::create(&self.backing)?;
        Ok(DurableRecoverJob {
            operation,
            source,
            snapshot,
        })
    }

    /// The reverified snapshot grants access only if session disposition and
    /// durable retention still permit it. No old process lease is restored.
    pub(crate) fn finish_recover_durable(
        &mut self,
        prepared: PreparedDurableRecover,
    ) -> Result<(), BulkError> {
        let job = prepared.0;
        let op = job.operation;
        if !op.pending.live()
            || !self.durable_jobs.contains_key(&op.id)
            || !self
                .load_durable(&op.session)?
                .blobs
                .contains(&op.reference)
        {
            return Err(BulkError::Unavailable);
        }
        self.durable_jobs.remove(&op.id);
        self.blobs.insert(
            op.reference.id.clone(),
            BlobRecord {
                reference: op.reference,
                session: op.session,
                parent: None,
                retained: true,
                backing: Arc::new(job.snapshot),
            },
        );
        Ok(())
    }

    /// Remove durable retention without revoking any independently retained
    /// in-memory session result or read lease. Host session deletion calls this
    /// and `release_retention`; ordinary generation retirement must not call it.
    pub(crate) fn release_durable(
        &mut self,
        session: &str,
        reference: &BlobRef,
    ) -> Result<(), BulkError> {
        let mut catalog = self.load_durable(session)?;
        let index = catalog
            .blobs
            .iter()
            .position(|b| b == reference)
            .ok_or(BulkError::Unavailable)?;
        catalog.blobs.remove(index);
        self.save_durable(&catalog)?;
        self.cancel_durable(session, Some(&reference.id));
        let directory = self.durable_directory(session);
        secure_fs::remove_regular_file_if_exists(
            &directory.join(format!("{}.blob", reference.id)),
        )?;
        secure_fs::open_private_directory_for_lock(&directory)?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_bulk::tests::{digest, fixture, lease, publish};
    use std::cell::RefCell;
    use std::fs;

    // Test shorthand only: production deliberately has no synchronous method
    // that holds the storage gate throughout payload I/O.
    impl BulkStore {
        fn retain_durable(&mut self, session: &str, blob: &BlobRef) -> Result<(), BulkError> {
            let prepared = self.prepare_retain_durable(session, blob)?.run()?;
            self.finish_retain_durable(prepared)
        }
        fn recover_durable(&mut self, session: &str, blob: &BlobRef) -> Result<(), BulkError> {
            let prepared = self.prepare_recover_durable(session, blob)?.run()?;
            self.finish_recover_durable(prepared)
        }
    }

    type MetadataHook = Box<dyn Fn(&Path) -> Result<(), BulkError>>;
    thread_local! { static METADATA_HOOK: RefCell<Option<MetadataHook>> = RefCell::new(None); }
    pub(super) fn before_metadata_publish(path: &Path) -> Result<(), BulkError> {
        METADATA_HOOK.with(|slot| match slot.borrow().as_ref() {
            Some(hook) => hook(path),
            None => Ok(()),
        })
    }
    struct Hook;
    impl Drop for Hook {
        fn drop(&mut self) {
            METADATA_HOOK.with(|s| *s.borrow_mut() = None);
        }
    }

    #[test]
    fn bulk_b11_host_restart_requires_regrant_and_reverifies_bytes() {
        let (temp, mut store, parent) = fixture(BulkLimits::default());
        let blob = publish(&mut store, &parent, b"survives host restart");
        store.retain_durable(&parent.owner.session, &blob).unwrap();
        let old = lease(&mut store, &parent.owner, &blob);
        let old_path = store.transfer_directory().join(&old.locator);
        let metadata = fs::read(
            store
                .durable_directory(&parent.owner.session)
                .join("metadata.json"),
        )
        .unwrap();
        assert!(!String::from_utf8_lossy(&metadata).contains("locator"));
        assert!(!String::from_utf8_lossy(&metadata).contains(&old.lease));
        drop(store);
        assert!(!old_path.exists());
        let mut store =
            BulkStore::new(&temp.path().canonicalize().unwrap(), BulkLimits::default()).unwrap();
        assert_eq!(
            store.prepare_read(&parent.owner, &blob).err(),
            Some(BulkError::Unavailable)
        );
        assert_eq!(
            store.recover_durable("foreign-owner", &blob),
            Err(BulkError::Unavailable)
        );
        store.recover_durable(&parent.owner.session, &blob).unwrap();
        let new = lease(&mut store, &parent.owner, &blob);
        assert_ne!(old.lease, new.lease);
        assert_ne!(old.locator, new.locator);
        assert_eq!(
            fs::read(store.transfer_directory().join(&new.locator)).unwrap(),
            b"survives host restart"
        );
        assert_eq!(
            store.release(&parent.owner, &old.lease),
            Err(BulkError::Unavailable)
        );
        store.release(&parent.owner, &new.lease).unwrap();
        store.release_durable(&parent.owner.session, &blob).unwrap();
        store
            .release_retention(&parent.owner.session, &blob)
            .unwrap();
        assert_eq!(
            store.recover_durable(&parent.owner.session, &blob),
            Err(BulkError::Unavailable)
        );
    }

    #[test]
    fn bulk_b11_corrupt_retained_bytes_fail_before_new_grant() {
        let (temp, mut store, parent) = fixture(BulkLimits::default());
        let blob = publish(&mut store, &parent, b"abc");
        store.retain_durable(&parent.owner.session, &blob).unwrap();
        let path = store
            .durable_directory(&parent.owner.session)
            .join(format!("{}.blob", blob.id));
        drop(store);
        fs::write(path, b"bad").unwrap();
        let mut store =
            BulkStore::new(&temp.path().canonicalize().unwrap(), BulkLimits::default()).unwrap();
        assert_eq!(
            store.recover_durable(&parent.owner.session, &blob),
            Err(BulkError::IntegrityMismatch)
        );
        assert_eq!(
            store.prepare_read(&parent.owner, &blob).err(),
            Some(BulkError::Unavailable)
        );
        assert_eq!(fs::read_dir(store.backing.0.path()).unwrap().count(), 0);
    }

    #[test]
    fn bulk_b07_metadata_filesystem_failure_rolls_back_durable_copy() {
        let (temp, mut store, parent) = fixture(BulkLimits::default());
        let blob = publish(&mut store, &parent, b"abc");
        let directory = store.durable_directory(&parent.owner.session);
        let payload = directory.join(format!("{}.blob", blob.id));
        let observed = payload.clone();
        let _hook = Hook;
        METADATA_HOOK.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |path| {
                assert_eq!(fs::read(&observed)?, b"abc"); // real copy already complete
                fs::create_dir(path)?; // real atomic metadata publication will fail
                Ok(())
            }))
        });
        assert_eq!(
            store.retain_durable(&parent.owner.session, &blob),
            Err(BulkError::StorageUnavailable)
        );
        assert!(!payload.exists());
        fs::remove_dir(directory.join("metadata.json")).unwrap();
        drop(store);
        let mut store =
            BulkStore::new(&temp.path().canonicalize().unwrap(), BulkLimits::default()).unwrap();
        assert_eq!(
            store.recover_durable(&parent.owner.session, &blob),
            Err(BulkError::Unavailable)
        );
        assert_eq!(digest(b"abc"), blob.digest);
    }

    #[test]
    fn bulk_durable_records_reserve_quota_before_recovery() {
        let limits = BulkLimits {
            object_bytes: 3,
            owner_bytes: 3,
            ..BulkLimits::default()
        };
        let (temp, mut store, parent) = fixture(limits.clone());
        let blob = publish(&mut store, &parent, b"abc");
        store.retain_durable(&parent.owner.session, &blob).unwrap();
        drop(store);
        let mut store = BulkStore::new(&temp.path().canonicalize().unwrap(), limits).unwrap();
        store.begin_parent(&parent);
        assert_eq!(
            store.write(&parent, 1, "text/plain").err(),
            Some(BulkError::QuotaExceeded)
        );
        store.release_durable(&parent.owner.session, &blob).unwrap();
        assert!(store.write(&parent, 3, "text/plain").is_ok());
    }

    #[test]
    fn bulk_durable_copy_cancellation_does_not_hold_disposition_lock() {
        use crate::extension_bulk::tests::Hook as CopyHook;
        use std::sync::Barrier;
        let payload = vec![17; 128 * 1024];
        let (_temp, mut store, parent) = fixture(BulkLimits {
            owner_bytes: payload.len() as u64,
            ..BulkLimits::default()
        });
        let blob = publish(&mut store, &parent, &payload);
        let job = store
            .prepare_retain_durable(&parent.owner.session, &blob)
            .unwrap();
        let directory = store.durable_directory(&parent.owner.session);
        let entered = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let a = entered.clone();
        let b = resume.clone();
        let worker = std::thread::spawn(move || {
            let _hook = CopyHook::install(move |_| {
                a.wait();
                b.wait();
                Ok(())
            });
            job.run().err()
        });
        entered.wait();
        // Actual store access is free while the 128-KiB file copy is paused.
        store.retire_owner(&parent.owner.session);
        let mut next = parent.clone();
        next.request_id = "next".into();
        store.begin_parent(&next);
        assert_eq!(
            store.write(&next, 1, "text/plain").err(),
            Some(BulkError::QuotaExceeded)
        );
        resume.wait();
        assert_eq!(worker.join().unwrap(), Some(BulkError::Unavailable));
        assert_eq!(fs::read_dir(directory).unwrap().count(), 0);
        store.sweep();
        assert!(store.blobs.is_empty());
        assert!(store.durable_jobs.is_empty());

        let blob = publish(&mut store, &next, &payload);
        store.retain_durable(&next.owner.session, &blob).unwrap();
        store.retire_owner(&next.owner.session);
        let job = store
            .prepare_recover_durable(&next.owner.session, &blob)
            .unwrap();
        let a = entered.clone();
        let b = resume.clone();
        let worker = std::thread::spawn(move || {
            let _hook = CopyHook::install(move |_| {
                a.wait();
                b.wait();
                Ok(())
            });
            job.run().err()
        });
        entered.wait();
        store.retire_owner(&next.owner.session);
        resume.wait();
        assert_eq!(worker.join().unwrap(), Some(BulkError::Unavailable));
        assert_eq!(
            store.prepare_read(&next.owner, &blob).err(),
            Some(BulkError::Unavailable)
        );
    }

    #[test]
    fn bulk_durable_finish_rechecks_disposition_and_merges_concurrent_copies() {
        let (temp, mut store, parent) = fixture(BulkLimits::default());
        let first = publish(&mut store, &parent, b"abc");
        let late = store
            .prepare_retain_durable(&parent.owner.session, &first)
            .unwrap()
            .run()
            .unwrap();
        store.retire_owner(&parent.owner.session);
        assert_eq!(
            store.finish_retain_durable(late),
            Err(BulkError::Unavailable)
        );
        assert!(store
            .load_durable(&parent.owner.session)
            .unwrap()
            .blobs
            .is_empty());
        let mut next = parent.clone();
        next.request_id = "next".into();
        store.begin_parent(&next);
        let first = publish(&mut store, &next, b"abc");
        next.request_id = "last".into();
        store.begin_parent(&next);
        let second = publish(&mut store, &next, b"def");
        let a = store
            .prepare_retain_durable(&next.owner.session, &first)
            .unwrap();
        let b = store
            .prepare_retain_durable(&next.owner.session, &second)
            .unwrap();
        let a = a.run().unwrap();
        let b = b.run().unwrap();
        store.finish_retain_durable(b).unwrap();
        store.finish_retain_durable(a).unwrap();
        drop(store);
        let mut store =
            BulkStore::new(&temp.path().canonicalize().unwrap(), BulkLimits::default()).unwrap();
        assert_eq!(
            store.load_durable(&next.owner.session).unwrap().blobs.len(),
            2
        );
        let prepared = store
            .prepare_recover_durable(&next.owner.session, &first)
            .unwrap()
            .run()
            .unwrap();
        store.release_durable(&next.owner.session, &first).unwrap();
        assert_eq!(
            store.finish_recover_durable(prepared),
            Err(BulkError::Unavailable)
        );
        store.recover_durable(&next.owner.session, &second).unwrap();
        assert!(store.prepare_read(&next.owner, &second).is_ok());
    }

    #[test]
    fn bulk_durable_root_has_one_host_writer() {
        let (temp, store, _) = fixture(BulkLimits::default());
        assert_eq!(
            BulkStore::new(&temp.path().canonicalize().unwrap(), BulkLimits::default())
                .err()
                .map(|e| e.code()),
            Some("storage_unavailable")
        );
        drop(store);
        assert!(
            BulkStore::new(&temp.path().canonicalize().unwrap(), BulkLimits::default()).is_ok()
        );
    }
}
