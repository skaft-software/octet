//! Host-owned bulk storage shared across extension instances in one trust domain.

use crate::extension_bulk::{BlobRef, BulkError, BulkLimits, BulkStore};
use crate::secure_fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

/// Shared storage authority, configured by the host, never by extension RPC.
///
/// Clone this handle into runtime configurations that share a trust domain.
/// Actual access still requires the authenticated session owner and a fresh
/// generation-bound lease. A separate HOME is not a syscall sandbox.
#[derive(Clone)]
pub struct BulkStorage {
    inner: Arc<StorageInner>,
}

struct StorageInner {
    store: Mutex<BulkStore>,
    durable: bool,
    // Dropped after the store and its files. Durable roots are never temporary.
    _temporary: Option<tempfile::TempDir>,
}

impl std::fmt::Debug for BulkStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BulkStorage")
            .field("limits", &self.limits())
            .field("durable", &self.inner.durable)
            .finish_non_exhaustive()
    }
}

impl BulkStorage {
    /// Create temporary storage with default finite quotas.
    pub fn new() -> Result<Self, BulkError> {
        Self::with_limits(BulkLimits::default())
    }

    /// Create temporary storage with explicit finite quotas.
    pub fn with_limits(limits: BulkLimits) -> Result<Self, BulkError> {
        let directory = tempfile::Builder::new().prefix("octet-bulk-").tempdir()?;
        // Use the existing secure-store convention, including owner-only ACLs
        // for a child created by an elevated Windows process.
        let root = directory.path().join("store");
        secure_fs::create_private_directory_all(&root)?;
        let store = BulkStore::new(&root, limits)?;
        Ok(Self {
            inner: Arc::new(StorageInner {
                store: Mutex::new(store),
                durable: false,
                _temporary: Some(directory),
            }),
        })
    }

    /// Open a host-selected persistent root with explicit finite quotas.
    ///
    /// Merely opening storage does not restore grants. Only explicitly retained
    /// blobs may be recovered after verification and host reauthorization.
    /// One host owns this root at a time; concurrent opens fail closed.
    pub fn with_root_and_limits(
        root: impl Into<PathBuf>,
        limits: BulkLimits,
    ) -> Result<Self, BulkError> {
        let store = BulkStore::new(&root.into(), limits)?;
        Ok(Self {
            inner: Arc::new(StorageInner {
                store: Mutex::new(store),
                durable: true,
                _temporary: None,
            }),
        })
    }

    /// The configured, enforced finite bounds; independent of media artifacts.
    pub fn limits(&self) -> BulkLimits {
        self.lock().limits().clone()
    }

    /// Explicitly retain an admitted result across host restart.
    ///
    /// The authenticated owning host session supplies `session`; it must never
    /// be copied from extension arguments. Bytes are copied/verified outside
    /// the disposition lock, then retention is rechecked and synced before ack.
    /// Temporary stores refuse durable retention rather than promise recovery.
    pub async fn retain_durable(
        &self,
        session: &str,
        reference: &BlobRef,
    ) -> Result<(), BulkError> {
        self.require_durable(session)?;
        let job = self.lock().prepare_retain_durable(session, reference)?;
        let prepared = tokio::task::spawn_blocking(move || job.run())
            .await
            .map_err(|_| BulkError::StorageUnavailable)??;
        self.lock().finish_retain_durable(prepared)
    }

    /// Reauthorize one explicitly retained blob after verifying its bytes.
    ///
    /// This host-only call restores no native resource, old locator or lease.
    /// Reading still requires a fresh extension-generation lease.
    pub async fn recover_durable(
        &self,
        session: &str,
        reference: &BlobRef,
    ) -> Result<(), BulkError> {
        self.require_durable(session)?;
        let job = self.lock().prepare_recover_durable(session, reference)?;
        let prepared = tokio::task::spawn_blocking(move || job.run())
            .await
            .map_err(|_| BulkError::StorageUnavailable)??;
        self.lock().finish_recover_durable(prepared)
    }

    /// Remove durable retention; independent live result/lease ownership remains.
    pub fn release_durable(&self, session: &str, reference: &BlobRef) -> Result<(), BulkError> {
        self.require_durable(session)?;
        self.lock().release_durable(session, reference)
    }

    /// Release this host session's in-memory result retention. Existing read
    /// leases still pin storage; no new lease is admitted afterwards.
    pub fn release_blob(&self, session: &str, reference: &BlobRef) -> Result<(), BulkError> {
        self.lock().release_retention(session, reference)
    }

    /// End all in-memory ownership for a host session, including pending copies.
    /// Durable records require separate explicit release; foreground switching
    /// must revoke generation grants instead of calling this method.
    pub fn retire_session(&self, session: &str) {
        self.lock().retire_owner(session);
    }

    fn require_durable(&self, session: &str) -> Result<(), BulkError> {
        if !self.inner.durable {
            return Err(BulkError::UnsupportedFeature);
        }
        if session.is_empty() || session.len() > 512 {
            return Err(BulkError::Unavailable);
        }
        Ok(())
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, BulkStore> {
        self.inner
            .store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_bulk::{BlobDigest, BulkOwner, BulkParent};
    use sha2::{Digest, Sha256};

    fn publish(storage: &BulkStorage) -> (BulkOwner, BlobRef) {
        let parent = BulkParent {
            owner: BulkOwner {
                session: "session".into(),
                extension: "instance".into(),
                generation: 1,
            },
            request_id: "request".into(),
        };
        let bytes = b"verified bytes";
        let job = {
            let mut store = storage.lock();
            store.begin_parent(&parent);
            let ticket = store
                .write(&parent, bytes.len() as u64, "application/octet-stream")
                .unwrap();
            std::fs::write(store.transfer_directory().join(&ticket.locator), bytes).unwrap();
            store
                .prepare_commit(
                    &parent,
                    &ticket.ticket,
                    bytes.len() as u64,
                    &BlobDigest {
                        algorithm: "sha256".into(),
                        value: format!("{:x}", Sha256::digest(bytes)),
                    },
                )
                .unwrap()
        };
        let prepared = job.run().unwrap();
        let reference = storage.lock().finish_commit(prepared).unwrap();
        storage
            .lock()
            .admit_parent(&parent, std::slice::from_ref(&reference))
            .unwrap();
        (parent.owner, reference)
    }

    #[tokio::test]
    async fn temporary_storage_never_promises_durability() {
        let storage = BulkStorage::new().unwrap();
        let (owner, reference) = publish(&storage);
        assert_eq!(
            storage.retain_durable(&owner.session, &reference).await,
            Err(BulkError::UnsupportedFeature)
        );
        assert_eq!(
            storage.recover_durable(&owner.session, &reference).await,
            Err(BulkError::UnsupportedFeature)
        );
        storage.release_blob(&owner.session, &reference).unwrap();
        assert_eq!(
            storage.lock().prepare_read(&owner, &reference).err(),
            Some(BulkError::Unavailable)
        );
    }

    #[tokio::test]
    async fn persistent_storage_reopen_requires_explicit_host_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("store");
        let storage = BulkStorage::with_root_and_limits(&root, BulkLimits::default()).unwrap();
        let (mut owner, reference) = publish(&storage);
        assert_eq!(
            storage.retain_durable("foreign", &reference).await,
            Err(BulkError::Unavailable)
        );
        storage
            .retain_durable(&owner.session, &reference)
            .await
            .unwrap();
        drop(storage);
        let storage = BulkStorage::with_root_and_limits(&root, BulkLimits::default()).unwrap();
        owner.generation += 1;
        assert_eq!(
            storage.lock().prepare_read(&owner, &reference).err(),
            Some(BulkError::Unavailable)
        );
        assert_eq!(
            storage.recover_durable("foreign", &reference).await,
            Err(BulkError::Unavailable)
        );
        storage
            .recover_durable(&owner.session, &reference)
            .await
            .unwrap();
        let job = storage.lock().prepare_read(&owner, &reference).unwrap();
        let prepared = job.run().unwrap();
        let lease = storage.lock().finish_read(prepared).unwrap();
        let path = storage.lock().transfer_directory().join(&lease.locator);
        assert_eq!(std::fs::read(path).unwrap(), b"verified bytes");
        storage.lock().release(&owner, &lease.lease).unwrap();
        storage.release_durable(&owner.session, &reference).unwrap();
        storage.retire_session(&owner.session);
        assert_eq!(
            storage.recover_durable(&owner.session, &reference).await,
            Err(BulkError::Unavailable)
        );
        assert_eq!(
            storage.lock().prepare_read(&owner, &reference).err(),
            Some(BulkError::Unavailable)
        );
    }
}
