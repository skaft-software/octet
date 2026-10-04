//! Immutable byte snapshots for the negotiated `local-file.v1` bulk profile.
//!
//! The host must serialize parent admission/retirement with its resource gate.
//! Copy jobs run outside that gate; finishing a job rechecks its reservation.
//! Durable recovery requires an explicit host session regrant. Neither retained
//! bytes nor local-file grants imply an OS sandbox or native-resource persistence.

use crate::secure_fs::{self, PrivateDirectory};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const PROFILE: &str = "local-file.v1";
const PORTABLE_MAX: u64 = (1_u64 << 53) - 1;

/// Integrity metadata for an immutable [`BlobRef`].
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BlobDigest {
    /// Integrity algorithm; v1 supports only `sha256`.
    pub algorithm: String,
    /// Lowercase 64-character SHA-256 hexadecimal digest.
    pub value: String,
}

impl BlobDigest {
    fn validate(&self) -> Result<(), BulkError> {
        if self.algorithm != "sha256" {
            return Err(BulkError::UnsupportedFeature);
        }
        if self.value.len() != 64
            || !self
                .value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(BulkError::IntegrityMismatch);
        }
        Ok(())
    }
}

/// An immutable host-verified byte sequence. Identity is the opaque token, not
/// the digest; knowledge of this descriptor does not confer a read grant.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BlobRef {
    /// Opaque host-issued identity, encoded as `$blob` on the wire.
    #[serde(rename = "$blob")]
    pub id: String,
    /// Verified length, restricted to portable nonnegative JSON integers.
    pub bytes: u64,
    /// Host-verified integrity metadata.
    pub digest: BlobDigest,
    /// Bounded validated MIME media type.
    pub media_type: String,
}

/// Constructed only from authenticated host context, never extension wire data.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BulkOwner {
    pub session: String,
    pub extension: String,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BulkParent {
    pub owner: BulkOwner,
    pub request_id: String,
}

/// Finite negotiated byte, transfer, and record limits for bulk storage.
#[derive(Clone, Debug, Serialize)]
pub struct BulkLimits {
    /// Maximum bytes in one immutable object or write reservation.
    pub object_bytes: u64,
    /// Maximum retained/reserved bytes for one host session.
    pub owner_bytes: u64,
    /// Maximum outstanding write tickets for one process generation; also
    /// bounds host-only durable copy operations per session.
    pub write_tickets_per_generation: usize,
    /// Maximum outstanding read leases for one process generation.
    pub read_leases_per_generation: usize,
    /// Also bounds zero-byte objects and unpublished record reservations.
    pub blobs_per_owner: usize,
}

impl Default for BulkLimits {
    fn default() -> Self {
        Self {
            object_bytes: 256 * 1024 * 1024,
            owner_bytes: 512 * 1024 * 1024,
            write_tickets_per_generation: 8,
            read_leases_per_generation: 32,
            blobs_per_owner: 256,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct WriteTicket {
    pub ticket: String,
    pub profile: &'static str,
    pub locator: String,
    pub capacity: u64,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ReadLease {
    pub lease: String,
    pub profile: &'static str,
    pub locator: String,
    pub bytes: u64,
}

/// Bounded storage refusals; never contains payloads, private paths, or
/// another owner's metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BulkError {
    /// Unknown, foreign, retired, ungranted, or mismatched reference/transfer.
    #[error("blob_unavailable")]
    Unavailable,
    /// A finite byte, record, or transfer reservation limit was exceeded.
    #[error("quota_exceeded")]
    QuotaExceeded,
    /// The supplied or retained digest does not match the verified bytes.
    #[error("integrity_mismatch")]
    IntegrityMismatch,
    /// The actual file length differs from the declared length.
    #[error("size_mismatch")]
    SizeMismatch,
    /// Secure file access, entropy, copying, synchronization, or publication failed.
    #[error("storage_unavailable")]
    StorageUnavailable,
    /// The requested integrity algorithm or metadata format is unsupported.
    #[error("unsupported_feature")]
    UnsupportedFeature,
}

impl BulkError {
    /// Stable bounded protocol error code.
    pub fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "blob_unavailable",
            Self::QuotaExceeded => "quota_exceeded",
            Self::IntegrityMismatch => "integrity_mismatch",
            Self::SizeMismatch => "size_mismatch",
            Self::StorageUnavailable => "storage_unavailable",
            Self::UnsupportedFeature => "unsupported_feature",
        }
    }
}

impl From<std::io::Error> for BulkError {
    fn from(_: std::io::Error) -> Self {
        Self::StorageUnavailable
    }
}

impl From<secure_fs::SecureFileError> for BulkError {
    fn from(_: secure_fs::SecureFileError) -> Self {
        Self::StorageUnavailable
    }
}

struct Directory(PrivateDirectory);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = self.0.remove_empty_if_exists();
    }
}

/// A private, exclusively created file. No borrowed/supplied locator is joined.
struct LocalFile {
    directory: Arc<Directory>,
    path: PathBuf,
}

impl LocalFile {
    fn create(directory: &Arc<Directory>) -> Result<Self, BulkError> {
        let path = directory
            .0
            .path()
            .join(format!("octet-transfer-{}", token()?));
        directory.0.create_regular_file_for_append(&path)?;
        Ok(Self {
            directory: directory.clone(),
            path,
        })
    }

    fn open(&self) -> Result<File, BulkError> {
        // An independent descriptor/cursor, bound to the authorized directory;
        // regular, private, unlinked-from-other-names, and no-follow checked.
        Ok(self.directory.0.open_regular_file_for_append(&self.path)?)
    }

    fn locator(&self) -> String {
        self.path
            .file_name()
            .expect("host-created basename")
            .to_string_lossy()
            .into_owned()
    }
}

impl Drop for LocalFile {
    fn drop(&mut self) {
        let _ = self.directory.0.remove_regular_file_if_exists(&self.path);
    }
}

/// A failed/dropped job invalidates its reservation without acquiring a host lock.
struct Pending {
    live: Arc<AtomicBool>,
    armed: bool,
}
impl Pending {
    fn new(live: Arc<AtomicBool>) -> Self {
        Self { live, armed: true }
    }
    fn live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }
    fn disarm(mut self) {
        self.armed = false;
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        if self.armed {
            self.live.store(false, Ordering::Release);
        }
    }
}

struct TicketRecord {
    parent: BulkParent,
    capacity: u64,
    media_type: String,
    live: Arc<AtomicBool>,
    scratch: Option<LocalFile>,
}

struct BlobRecord {
    reference: BlobRef,
    session: String,
    parent: Option<BulkParent>,
    retained: bool,
    backing: Arc<LocalFile>,
}

struct LeaseRecord {
    owner: BulkOwner,
    blob: String,
    live: Arc<AtomicBool>,
    file: Option<LocalFile>,
}

pub(crate) struct CommitJob {
    ticket: String,
    parent: BulkParent,
    pending: Pending,
    source: LocalFile,
    snapshot: LocalFile,
    reference: BlobRef,
    capacity: u64,
}

pub(crate) struct PreparedCommit(CommitJob);

impl CommitJob {
    /// Bounded real-file copy and hash. Errors/drop release the reservation and
    /// delete owned scratch/partial snapshot files automatically.
    pub(crate) fn run(self) -> Result<PreparedCommit, BulkError> {
        copy_verified(
            &self.source,
            &self.snapshot,
            &self.reference,
            self.capacity,
            &self.pending,
        )?;
        Ok(PreparedCommit(self))
    }
}

pub(crate) struct ReadJob {
    lease: String,
    owner: BulkOwner,
    pending: Pending,
    source: Arc<LocalFile>,
    transfer: LocalFile,
    reference: BlobRef,
}

pub(crate) struct PreparedRead(ReadJob);

impl ReadJob {
    pub(crate) fn run(self) -> Result<PreparedRead, BulkError> {
        copy_verified(
            &self.source,
            &self.transfer,
            &self.reference,
            self.reference.bytes,
            &self.pending,
        )?;
        Ok(PreparedRead(self))
    }
}

pub(crate) struct BulkStore {
    limits: BulkLimits,
    transfer: Arc<Directory>,
    backing: Arc<Directory>,
    parents: HashSet<BulkParent>,
    tickets: HashMap<String, TicketRecord>,
    blobs: HashMap<String, BlobRecord>,
    leases: HashMap<String, LeaseRecord>,
    durable_jobs: HashMap<String, durable::Reservation>,
    root: PathBuf,
    // Last to drop: serializes the durable root across host processes.
    _root_lock: Arc<File>,
}

impl BulkStore {
    pub(crate) fn new(root: &Path, limits: BulkLimits) -> Result<Self, BulkError> {
        if limits.object_bytes == 0
            || limits.owner_bytes == 0
            || limits.object_bytes > PORTABLE_MAX
            || limits.owner_bytes > PORTABLE_MAX
            || limits.write_tickets_per_generation == 0
            || limits.read_leases_per_generation == 0
            || limits.blobs_per_owner == 0
        {
            return Err(BulkError::QuotaExceeded);
        }
        let root_lock = secure_fs::open_private_directory_for_lock(root)?;
        fs2::FileExt::try_lock_exclusive(&root_lock)?;
        let transfer = Arc::new(Directory(secure_fs::create_bound_private_directory(
            root,
            "bulk-transfer",
        )?));
        let backing = Arc::new(Directory(secure_fs::create_bound_private_directory(
            root,
            "bulk-backing",
        )?));
        Ok(Self {
            limits,
            transfer,
            backing,
            parents: HashSet::new(),
            tickets: HashMap::new(),
            blobs: HashMap::new(),
            leases: HashMap::new(),
            durable_jobs: HashMap::new(),
            root: root.to_owned(),
            _root_lock: Arc::new(root_lock),
        })
    }

    pub(crate) fn limits(&self) -> &BulkLimits {
        &self.limits
    }

    /// Transport negotiation only; never include this in a domain/model result.
    pub(crate) fn transfer_directory(&self) -> &Path {
        self.transfer.0.path()
    }

    /// Only the host may begin a freshly admitted, uniquely identified parent.
    pub(crate) fn begin_parent(&mut self, parent: &BulkParent) {
        self.parents.insert(parent.clone());
    }

    pub(crate) fn write(
        &mut self,
        parent: &BulkParent,
        capacity: u64,
        media_type: &str,
    ) -> Result<WriteTicket, BulkError> {
        self.sweep();
        self.check_parent(parent)?;
        validate_media_type(media_type)?;
        if capacity > self.limits.object_bytes
            || self
                .tickets
                .values()
                .filter(|t| t.parent.owner == parent.owner)
                .count()
                >= self.limits.write_tickets_per_generation
        {
            return Err(BulkError::QuotaExceeded);
        }
        let session = &parent.owner.session;
        let reserved: u64 = self
            .tickets
            .values()
            .filter(|t| &t.parent.owner.session == session)
            .map(|t| t.capacity)
            .sum();
        let durable = self.load_durable(session)?;
        let dormant: Vec<_> = durable
            .blobs
            .iter()
            .filter(|b| !self.blobs.contains_key(&b.id))
            .chain(
                self.durable_jobs
                    .values()
                    .filter(|j| {
                        &j.session == session
                            && !self.blobs.contains_key(&j.reference.id)
                            && !durable.blobs.iter().any(|b| b.id == j.reference.id)
                    })
                    .map(|j| &j.reference),
            )
            .collect();
        let stored: u64 = self
            .blobs
            .values()
            .filter(|b| &b.session == session)
            .map(|b| b.reference.bytes)
            .chain(dormant.iter().map(|b| b.bytes))
            .sum();
        let records = self
            .tickets
            .values()
            .filter(|t| &t.parent.owner.session == session)
            .count()
            + self
                .blobs
                .values()
                .filter(|b| &b.session == session)
                .count()
            + dormant.len();
        if reserved.saturating_add(stored).saturating_add(capacity) > self.limits.owner_bytes
            || records >= self.limits.blobs_per_owner
        {
            return Err(BulkError::QuotaExceeded);
        }
        let ticket = token()?;
        let scratch = LocalFile::create(&self.transfer)?;
        let result = WriteTicket {
            ticket: ticket.clone(),
            profile: PROFILE,
            locator: scratch.locator(),
            capacity,
        };
        self.tickets.insert(
            ticket,
            TicketRecord {
                parent: parent.clone(),
                capacity,
                media_type: media_type.to_owned(),
                live: Arc::new(AtomicBool::new(true)),
                scratch: Some(scratch),
            },
        );
        Ok(result)
    }

    /// Consumes a matching ticket even when metadata or file validation fails.
    pub(crate) fn prepare_commit(
        &mut self,
        parent: &BulkParent,
        ticket: &str,
        bytes: u64,
        digest: &BlobDigest,
    ) -> Result<CommitJob, BulkError> {
        self.sweep();
        self.check_parent(parent)?;
        let record = self
            .tickets
            .get_mut(ticket)
            .filter(|t| &t.parent == parent)
            .ok_or(BulkError::Unavailable)?;
        let source = record.scratch.take().ok_or(BulkError::Unavailable)?;
        let pending = Pending::new(record.live.clone());
        digest.validate()?;
        if bytes > record.capacity {
            return Err(BulkError::QuotaExceeded);
        }
        let reference = BlobRef {
            id: token()?,
            bytes,
            digest: digest.clone(),
            media_type: record.media_type.clone(),
        };
        let snapshot = LocalFile::create(&self.backing)?;
        Ok(CommitJob {
            ticket: ticket.to_owned(),
            parent: parent.clone(),
            pending,
            source,
            snapshot,
            reference,
            capacity: record.capacity,
        })
    }

    pub(crate) fn finish_commit(&mut self, prepared: PreparedCommit) -> Result<BlobRef, BulkError> {
        let job = prepared.0;
        self.check_parent(&job.parent)?;
        if !job.pending.live() || !self.tickets.contains_key(&job.ticket) {
            return Err(BulkError::Unavailable);
        }
        self.tickets.remove(&job.ticket);
        let result = job.reference.clone();
        self.blobs.insert(
            result.id.clone(),
            BlobRecord {
                reference: job.reference,
                session: job.parent.owner.session.clone(),
                parent: Some(job.parent),
                retained: false,
                backing: Arc::new(job.snapshot),
            },
        );
        Ok(result)
    }

    /// Call under the shared resource/blob disposition lock AFTER complete
    /// schema/envelope/diagnostic validation and BEFORE publishing any success.
    pub(crate) fn validate_outputs(
        &self,
        parent: &BulkParent,
        outputs: &[BlobRef],
    ) -> Result<(), BulkError> {
        self.check_parent(parent)?;
        for reference in outputs {
            let blob = self
                .blobs
                .get(&reference.id)
                .ok_or(BulkError::Unavailable)?;
            if blob.session != parent.owner.session
                || &blob.reference != reference
                || match &blob.parent {
                    Some(p) => p != parent,
                    None => !blob.retained,
                }
            {
                return Err(BulkError::Unavailable);
            }
        }
        Ok(())
    }

    /// Atomically activates all exported provisional blobs, retaining them for
    /// their host session, and discards every unexported blob/write reservation.
    pub(crate) fn admit_parent(
        &mut self,
        parent: &BulkParent,
        outputs: &[BlobRef],
    ) -> Result<(), BulkError> {
        if let Err(error) = self.validate_outputs(parent, outputs) {
            self.retire_parent(parent);
            return Err(error);
        }
        let ids: HashSet<&str> = outputs.iter().map(|b| b.id.as_str()).collect();
        for blob in self.blobs.values_mut() {
            if blob.parent.as_ref() == Some(parent) && ids.contains(blob.reference.id.as_str()) {
                blob.parent = None;
                blob.retained = true;
            }
        }
        self.retire_parent(parent);
        Ok(())
    }

    pub(crate) fn retire_parent(&mut self, parent: &BulkParent) {
        self.parents.remove(parent);
        self.tickets.retain(|_, t| {
            if &t.parent == parent {
                t.live.store(false, Ordering::Release);
                t.scratch.is_none() // copying jobs remain charged until they settle
            } else {
                true
            }
        });
        self.blobs.retain(|_, b| b.parent.as_ref() != Some(parent));
        self.sweep();
    }

    /// Host-internal descriptor lookup for an already published session grant.
    /// Never resolves provisional objects or discloses transfer locators.
    pub(crate) fn reference_for_session(
        &self,
        session: &str,
        id: &str,
    ) -> Result<BlobRef, BulkError> {
        self.blobs
            .get(id)
            .filter(|blob| blob.session == session && blob.retained && blob.parent.is_none())
            .map(|blob| blob.reference.clone())
            .ok_or(BulkError::Unavailable)
    }

    /// `owner` must be a currently authenticated live generation. The process
    /// host fences retired generations before calling this session-grant check.
    pub(crate) fn prepare_read(
        &mut self,
        owner: &BulkOwner,
        reference: &BlobRef,
    ) -> Result<ReadJob, BulkError> {
        self.sweep();
        let blob = self
            .blobs
            .get(&reference.id)
            .filter(|b| {
                b.session == owner.session
                    && b.retained
                    && b.parent.is_none()
                    && &b.reference == reference
            })
            .ok_or(BulkError::Unavailable)?;
        if self.leases.values().filter(|l| &l.owner == owner).count()
            >= self.limits.read_leases_per_generation
        {
            return Err(BulkError::QuotaExceeded);
        }
        let lease = token()?;
        let transfer = LocalFile::create(&self.transfer)?;
        let live = Arc::new(AtomicBool::new(true));
        self.leases.insert(
            lease.clone(),
            LeaseRecord {
                owner: owner.clone(),
                blob: reference.id.clone(),
                live: live.clone(),
                file: None,
            },
        );
        Ok(ReadJob {
            lease,
            owner: owner.clone(),
            pending: Pending::new(live),
            source: blob.backing.clone(),
            transfer,
            reference: reference.clone(),
        })
    }

    pub(crate) fn finish_read(&mut self, prepared: PreparedRead) -> Result<ReadLease, BulkError> {
        let job = prepared.0;
        // The transfer area is disclosed to the peer; reject a substituted
        // symlink/special file again before returning its relative locator.
        job.transfer.open()?;
        let record = self
            .leases
            .get_mut(&job.lease)
            .filter(|l| l.owner == job.owner && job.pending.live())
            .ok_or(BulkError::Unavailable)?;
        let result = ReadLease {
            lease: job.lease,
            profile: PROFILE,
            locator: job.transfer.locator(),
            bytes: job.reference.bytes,
        };
        record.file = Some(job.transfer);
        // Hand responsibility for this live lease from the job to the store.
        // Dropping the job guard would otherwise invalidate an admitted lease.
        job.pending.disarm();
        Ok(result)
    }

    pub(crate) fn release(&mut self, owner: &BulkOwner, id: &str) -> Result<(), BulkError> {
        self.sweep();
        if let Some(ticket) = self
            .tickets
            .get(id)
            .filter(|t| &t.parent.owner == owner && t.live.load(Ordering::Acquire))
        {
            ticket.live.store(false, Ordering::Release);
        } else if let Some(lease) = self
            .leases
            .get(id)
            .filter(|l| &l.owner == owner && l.live.load(Ordering::Acquire))
        {
            lease.live.store(false, Ordering::Release);
        } else {
            return Err(BulkError::Unavailable);
        }
        self.sweep();
        Ok(())
    }

    /// Host lifecycle control, not an extension assertion of session ownership.
    /// Existing leases pin the backing; no new read is admitted after this call.
    pub(crate) fn release_retention(
        &mut self,
        session: &str,
        reference: &BlobRef,
    ) -> Result<(), BulkError> {
        let blob = self
            .blobs
            .get_mut(&reference.id)
            .filter(|b| b.session == session && b.retained && &b.reference == reference)
            .ok_or(BulkError::Unavailable)?;
        blob.retained = false;
        self.cancel_durable(session, Some(&reference.id));
        self.sweep();
        Ok(())
    }

    pub(crate) fn retire_generation(&mut self, owner: &BulkOwner) {
        let parents: Vec<_> = self
            .parents
            .iter()
            .filter(|p| &p.owner == owner)
            .cloned()
            .collect();
        for parent in parents {
            self.retire_parent(&parent);
        }
        self.leases.retain(|_, l| {
            if &l.owner == owner {
                l.live.store(false, Ordering::Release);
                l.file.is_none()
            } else {
                true
            }
        });
        self.sweep();
    }

    pub(crate) fn retire_owner(&mut self, session: &str) {
        self.cancel_durable(session, None);
        let parents: Vec<_> = self
            .parents
            .iter()
            .filter(|p| p.owner.session == session)
            .cloned()
            .collect();
        for parent in parents {
            self.retire_parent(&parent);
        }
        self.leases.retain(|_, l| {
            if l.owner.session == session {
                l.live.store(false, Ordering::Release);
                l.file.is_none()
            } else {
                true
            }
        });
        for blob in self.blobs.values_mut().filter(|b| b.session == session) {
            blob.retained = false;
        }
        self.sweep();
    }

    fn check_parent(&self, parent: &BulkParent) -> Result<(), BulkError> {
        if self.parents.contains(parent) {
            Ok(())
        } else {
            Err(BulkError::Unavailable)
        }
    }

    fn sweep(&mut self) {
        self.tickets
            .retain(|_, t| t.live.load(Ordering::Acquire) || Arc::strong_count(&t.live) > 1);
        self.leases
            .retain(|_, l| l.live.load(Ordering::Acquire) || Arc::strong_count(&l.live) > 1);
        self.durable_jobs
            .retain(|_, j| j.live.load(Ordering::Acquire) || Arc::strong_count(&j.live) > 1);
        self.blobs.retain(|id, b| {
            b.retained
                || b.parent.is_some()
                || self.leases.values().any(|l| &l.blob == id)
                || self.durable_jobs.values().any(|j| &j.reference.id == id)
        });
    }
}

impl Drop for BulkStore {
    fn drop(&mut self) {
        for t in self.tickets.values() {
            t.live.store(false, Ordering::Release);
        }
        for l in self.leases.values() {
            l.live.store(false, Ordering::Release);
        }
        for job in self.durable_jobs.values() {
            job.live.store(false, Ordering::Release);
        }
    }
}

fn token() -> Result<String, BulkError> {
    let mut bytes = [0_u8; 24];
    getrandom::fill(&mut bytes).map_err(|_| BulkError::StorageUnavailable)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn validate_media_type(value: &str) -> Result<(), BulkError> {
    let valid_name = |s: &str| {
        !s.is_empty()
            && s.len() <= 127
            && s.as_bytes()[0].is_ascii_alphanumeric()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&b))
    };
    let (essence, parameters) = value.split_once(';').unwrap_or((value, ""));
    if value.len() > 255
        || !essence
            .trim_end_matches([' ', '\t'])
            .split_once('/')
            .is_some_and(|(a, b)| valid_name(a) && valid_name(b))
        || value.ends_with(';')
    {
        return Err(BulkError::UnsupportedFeature);
    }
    let token_len = |s: &[u8]| {
        s.iter()
            .take_while(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(b))
            .count()
    };
    let mut rest = parameters.as_bytes();
    while !rest.is_empty() {
        rest = rest.trim_ascii_start();
        let name = token_len(rest);
        if name == 0 || rest.get(name) != Some(&b'=') {
            return Err(BulkError::UnsupportedFeature);
        }
        rest = &rest[name + 1..];
        if rest.first() == Some(&b'"') {
            rest = &rest[1..];
            loop {
                let Some((&byte, tail)) = rest.split_first() else {
                    return Err(BulkError::UnsupportedFeature);
                };
                rest = tail;
                if byte == b'"' {
                    break;
                }
                let byte = if byte == b'\\' {
                    let Some((&escaped, tail)) = rest.split_first() else {
                        return Err(BulkError::UnsupportedFeature);
                    };
                    rest = tail;
                    escaped
                } else {
                    byte
                };
                if !(byte == b'\t' || (0x20..=0x7e).contains(&byte)) {
                    return Err(BulkError::UnsupportedFeature);
                }
            }
        } else {
            let length = token_len(rest);
            if length == 0 {
                return Err(BulkError::UnsupportedFeature);
            }
            rest = &rest[length..];
        }
        rest = rest.trim_ascii_start();
        if !rest.is_empty() {
            if rest[0] != b';' || rest.len() == 1 {
                return Err(BulkError::UnsupportedFeature);
            }
            rest = &rest[1..];
        }
    }
    if value
        .bytes()
        .any(|b| !(b == b'\t' || (0x20..=0x7e).contains(&b)))
    {
        return Err(BulkError::UnsupportedFeature);
    }
    Ok(())
}

fn copy_verified(
    source: &LocalFile,
    target: &LocalFile,
    reference: &BlobRef,
    capacity: u64,
    pending: &Pending,
) -> Result<(), BulkError> {
    copy_files(source.open()?, target.open()?, reference, capacity, pending)
}

fn copy_files(
    mut input: File,
    mut output: File,
    reference: &BlobRef,
    capacity: u64,
    pending: &Pending,
) -> Result<(), BulkError> {
    if input.metadata()?.len() > capacity {
        return Err(BulkError::QuotaExceeded);
    }
    if input.metadata()?.len() != reference.bytes {
        return Err(BulkError::SizeMismatch);
    }
    input.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut copied = 0_u64;
    loop {
        if !pending.live() {
            return Err(BulkError::Unavailable);
        }
        // At most declared length + one byte is read, even if a producer grows
        // its scratch after the metadata check. Never allocate by payload size.
        let wanted = ((reference.bytes - copied + 1).min(buffer.len() as u64)) as usize;
        let count = input.read(&mut buffer[..wanted])?;
        if count == 0 {
            break;
        }
        copied += count as u64;
        if copied > capacity {
            return Err(BulkError::QuotaExceeded);
        }
        if copied > reference.bytes {
            return Err(BulkError::SizeMismatch);
        }
        output.write_all(&buffer[..count])?;
        #[cfg(test)]
        tests::after_copy_chunk(copied)?;
        hasher.update(&buffer[..count]);
    }
    if copied != reference.bytes {
        return Err(BulkError::SizeMismatch);
    }
    if format!("{:x}", hasher.finalize()) != reference.digest.value {
        return Err(BulkError::IntegrityMismatch);
    }
    output.flush()?;
    // Surface deferred write failures before admitting any snapshot/lease.
    // Durable retention additionally syncs the directory and its metadata.
    output.sync_data()?;
    Ok(())
}

mod durable;

// Test-only callback is carried into one blocking job, never installed on the
// async runtime thread or shared globally between jobs.
#[cfg(test)]
pub(crate) type CopyTestHook = Box<dyn FnMut(u64) -> Result<(), BulkError> + Send + 'static>;

#[cfg(test)]
pub(crate) fn with_copy_test_hook<T>(hook: Option<CopyTestHook>, run: impl FnOnce() -> T) -> T {
    let _guard = hook.map(tests::Hook::install);
    run()
}

#[cfg(test)]
mod tests;
