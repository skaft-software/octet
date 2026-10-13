use super::*;
use std::cell::RefCell;
use std::fs;
use std::sync::Barrier;

// Fault/barrier injection occurs only after a real file chunk has been written.
type CopyHook = Box<dyn FnMut(u64) -> Result<(), BulkError>>;
thread_local! { static COPY_HOOK: RefCell<Option<CopyHook>> = RefCell::new(None); }

pub(super) fn after_copy_chunk(bytes: u64) -> Result<(), BulkError> {
    COPY_HOOK.with(|hook| match hook.borrow_mut().as_mut() {
        Some(hook) => hook(bytes),
        None => Ok(()),
    })
}

pub(super) struct Hook;
impl Hook {
    pub(super) fn install(hook: impl FnMut(u64) -> Result<(), BulkError> + 'static) -> Self {
        COPY_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
        Self
    }
}
impl Drop for Hook {
    fn drop(&mut self) {
        COPY_HOOK.with(|slot| *slot.borrow_mut() = None);
    }
}

pub(super) fn fixture(limits: BulkLimits) -> (tempfile::TempDir, BulkStore, BulkParent) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let mut store = BulkStore::new(&root, limits).unwrap();
    let parent = BulkParent {
        owner: BulkOwner {
            session: "session-a".into(),
            extension: "instance-a".into(),
            generation: 1,
        },
        request_id: "call-1".into(),
    };
    store.begin_parent(&parent);
    (temp, store, parent)
}

pub(super) fn digest(bytes: &[u8]) -> BlobDigest {
    BlobDigest {
        algorithm: "sha256".into(),
        value: format!("{:x}", Sha256::digest(bytes)),
    }
}

fn write_bytes(store: &mut BulkStore, parent: &BulkParent, bytes: &[u8]) -> WriteTicket {
    let ticket = store
        .write(parent, bytes.len() as u64, "application/octet-stream")
        .unwrap();
    assert!(!Path::new(&ticket.locator).is_absolute());
    assert_eq!(Path::new(&ticket.locator).components().count(), 1);
    fs::write(store.transfer_directory().join(&ticket.locator), bytes).unwrap();
    ticket
}

fn provisional(store: &mut BulkStore, parent: &BulkParent, bytes: &[u8]) -> BlobRef {
    let ticket = write_bytes(store, parent, bytes);
    let job = store
        .prepare_commit(parent, &ticket.ticket, bytes.len() as u64, &digest(bytes))
        .unwrap();
    let prepared = job.run().unwrap();
    store.finish_commit(prepared).unwrap()
}

pub(super) fn publish(store: &mut BulkStore, parent: &BulkParent, bytes: &[u8]) -> BlobRef {
    let blob = provisional(store, parent, bytes);
    store
        .admit_parent(parent, std::slice::from_ref(&blob))
        .unwrap();
    blob
}

pub(super) fn lease(store: &mut BulkStore, owner: &BulkOwner, blob: &BlobRef) -> ReadLease {
    let prepared = store.prepare_read(owner, blob).unwrap().run().unwrap();
    store.finish_read(prepared).unwrap()
}

fn file_count(directory: &Path) -> usize {
    fs::read_dir(directory).unwrap().count()
}

fn empty(store: &mut BulkStore) {
    store.sweep();
    assert!(store.tickets.is_empty());
    assert!(store.blobs.is_empty());
    assert!(store.leases.is_empty());
    assert!(store.durable_jobs.is_empty());
    assert_eq!(file_count(store.transfer_directory()), 0);
    assert_eq!(file_count(store.backing.0.path()), 0);
}

#[test]
fn bulk_reference_for_session_requires_current_published_grant() {
    let (_temp, mut store, parent) = fixture(BulkLimits::default());
    let blob = provisional(&mut store, &parent, b"abc");
    for (session, id) in [
        (parent.owner.session.as_str(), blob.id.as_str()),
        ("foreign-owner", blob.id.as_str()),
        (parent.owner.session.as_str(), "unknown"),
    ] {
        assert_eq!(
            store.reference_for_session(session, id),
            Err(BulkError::Unavailable)
        );
    }
    store
        .admit_parent(&parent, std::slice::from_ref(&blob))
        .unwrap();
    assert_eq!(
        store.reference_for_session(&parent.owner.session, &blob.id),
        Ok(blob.clone())
    );
    assert_eq!(
        store.reference_for_session("foreign-owner", &blob.id),
        Err(BulkError::Unavailable)
    );
    let read = lease(&mut store, &parent.owner, &blob);
    store
        .release_retention(&parent.owner.session, &blob)
        .unwrap();
    assert!(store.blobs.contains_key(&blob.id)); // pinned storage is not a grant
    assert_eq!(
        store.reference_for_session(&parent.owner.session, &blob.id),
        Err(BulkError::Unavailable)
    );
    store.release(&parent.owner, &read.lease).unwrap();
    empty(&mut store);
}

#[test]
fn bulk_transfer_basenames_are_reserved_not_identity_tokens() {
    let (_temp, mut store, parent) = fixture(BulkLimits::default());
    let ticket = write_bytes(&mut store, &parent, b"abc");
    let prepared = store
        .prepare_commit(&parent, &ticket.ticket, 3, &digest(b"abc"))
        .unwrap()
        .run()
        .unwrap();
    let blob = store.finish_commit(prepared).unwrap();
    store
        .admit_parent(&parent, std::slice::from_ref(&blob))
        .unwrap();
    let read = lease(&mut store, &parent.owner, &blob);
    for locator in [&ticket.locator, &read.locator] {
        let suffix = locator.strip_prefix("octet-transfer-").unwrap();
        assert_eq!(suffix.len(), 48);
        assert!(suffix
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert_eq!(Path::new(locator).components().count(), 1);
        assert_ne!(suffix, blob.id);
        assert_ne!(suffix, ticket.ticket);
        assert_ne!(suffix, read.lease);
    }
    for id in [&blob.id, &ticket.ticket, &read.lease] {
        assert_eq!(id.len(), 48);
        assert!(!id.starts_with("octet-transfer-"));
    }
}

#[test]
fn bulk_b01_lifecycle_and_closed_wire() {
    let (_temp, mut store, parent) = fixture(BulkLimits::default());
    let blob = provisional(&mut store, &parent, b"immutable payload");
    assert_eq!(
        store.prepare_read(&parent.owner, &blob).err(),
        Some(BulkError::Unavailable)
    );
    let wire = serde_json::to_value(&blob).unwrap();
    assert_eq!(wire.as_object().unwrap().len(), 4);
    assert_eq!(wire["$blob"], blob.id);
    assert!(wire.get("locator").is_none());
    let mut bad = wire.clone();
    bad["path"] = "/private/data".into();
    assert!(serde_json::from_value::<BlobRef>(bad).is_err());
    let mut bad = wire;
    bad["digest"]["extra"] = true.into();
    assert!(serde_json::from_value::<BlobRef>(bad).is_err());
    store
        .admit_parent(&parent, std::slice::from_ref(&blob))
        .unwrap();
    let lease = lease(&mut store, &parent.owner, &blob);
    assert_eq!(lease.profile, PROFILE);
    assert_eq!(lease.bytes, blob.bytes);
    assert_eq!(
        fs::read(store.transfer_directory().join(&lease.locator)).unwrap(),
        b"immutable payload"
    );
    store.release(&parent.owner, &lease.lease).unwrap();
    store
        .release_retention(&parent.owner.session, &blob)
        .unwrap();
    empty(&mut store);
}

#[test]
fn bulk_b02_wrong_size_consumes_ticket_and_cleans_files() {
    for declared in [2, 4] {
        let (_temp, mut store, parent) = fixture(BulkLimits::default());
        let ticket = store.write(&parent, 8, "text/plain").unwrap();
        fs::write(store.transfer_directory().join(&ticket.locator), b"abc").unwrap();
        let job = store
            .prepare_commit(&parent, &ticket.ticket, declared, &digest(b"abc"))
            .unwrap();
        assert_eq!(job.run().err(), Some(BulkError::SizeMismatch));
        assert_eq!(
            store
                .prepare_commit(&parent, &ticket.ticket, 3, &digest(b"abc"))
                .err(),
            Some(BulkError::Unavailable)
        );
        empty(&mut store);
    }
}

#[test]
fn bulk_b03_wrong_digest_and_unsupported_algorithm() {
    for algorithm in ["sha256", "md5"] {
        let (_temp, mut store, parent) = fixture(BulkLimits::default());
        let ticket = write_bytes(&mut store, &parent, b"abc");
        let mut wrong = digest(b"xyz");
        wrong.algorithm = algorithm.into();
        let result = store
            .prepare_commit(&parent, &ticket.ticket, 3, &wrong)
            .and_then(CommitJob::run);
        assert_eq!(
            result.err(),
            Some(if algorithm == "sha256" {
                BulkError::IntegrityMismatch
            } else {
                BulkError::UnsupportedFeature
            })
        );
        empty(&mut store);
    }
}

#[test]
fn bulk_b04_oversize_sparse_file_and_growing_file_are_bounded() {
    let (_temp, mut store, parent) = fixture(BulkLimits {
        object_bytes: 8,
        ..BulkLimits::default()
    });
    assert_eq!(
        store.write(&parent, 9, "application/octet-stream").err(),
        Some(BulkError::QuotaExceeded)
    );
    let ticket = store.write(&parent, 8, "application/octet-stream").unwrap();
    let path = store.transfer_directory().join(&ticket.locator);
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(1 << 30)
        .unwrap();
    let job = store
        .prepare_commit(&parent, &ticket.ticket, 8, &digest(b"12345678"))
        .unwrap();
    assert_eq!(job.run().err(), Some(BulkError::QuotaExceeded));
    empty(&mut store);

    let ticket = write_bytes(&mut store, &parent, b"12345678");
    let path = store.transfer_directory().join(&ticket.locator);
    let job = store
        .prepare_commit(&parent, &ticket.ticket, 8, &digest(b"12345678"))
        .unwrap();
    let _hook = Hook::install(move |_| {
        fs::OpenOptions::new()
            .append(true)
            .open(&path)?
            .write_all(b"9")?;
        Ok(())
    });
    assert_eq!(job.run().err(), Some(BulkError::QuotaExceeded));
    empty(&mut store);
}

#[test]
fn bulk_b05_abandon_and_dropped_job_restore_capacity() {
    let (_temp, mut store, parent) = fixture(BulkLimits {
        write_tickets_per_generation: 1,
        owner_bytes: 3,
        ..BulkLimits::default()
    });
    let ticket = write_bytes(&mut store, &parent, b"abc");
    assert_eq!(
        store.write(&parent, 1, "text/plain").err(),
        Some(BulkError::QuotaExceeded)
    );
    store.release(&parent.owner, &ticket.ticket).unwrap();
    let ticket = write_bytes(&mut store, &parent, b"abc");
    let job = store
        .prepare_commit(&parent, &ticket.ticket, 3, &digest(b"abc"))
        .unwrap();
    assert_eq!(
        store
            .prepare_commit(&parent, &ticket.ticket, 3, &digest(b"abc"))
            .err(),
        Some(BulkError::Unavailable)
    );
    drop(job);
    empty(&mut store);
    write_bytes(&mut store, &parent, b"abc");
    store.retire_parent(&parent);
    empty(&mut store);
}

#[test]
fn bulk_b06_cancel_during_real_copy_and_complete_first() {
    let (_temp, mut store, parent) = fixture(BulkLimits {
        write_tickets_per_generation: 1,
        ..BulkLimits::default()
    });
    let payload = vec![42; 128 * 1024];
    let ticket = write_bytes(&mut store, &parent, &payload);
    let job = store
        .prepare_commit(
            &parent,
            &ticket.ticket,
            payload.len() as u64,
            &digest(&payload),
        )
        .unwrap();
    let entered = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    let worker_entered = entered.clone();
    let worker_resume = resume.clone();
    let worker = std::thread::spawn(move || {
        let _hook = Hook::install(move |_| {
            worker_entered.wait();
            worker_resume.wait();
            Ok(())
        });
        job.run().err()
    });
    entered.wait();
    store.retire_parent(&parent);
    let mut next = parent.clone();
    next.request_id = "call-2".into();
    store.begin_parent(&next);
    // The cancelled but still executing copy keeps its reservation until stop.
    assert_eq!(
        store.write(&next, 1, "text/plain").err(),
        Some(BulkError::QuotaExceeded)
    );
    resume.wait();
    assert_eq!(worker.join().unwrap(), Some(BulkError::Unavailable));
    empty(&mut store);

    let blob = publish(&mut store, &next, b"complete first");
    store.retire_parent(&next);
    let read = lease(&mut store, &next.owner, &blob);
    store.release(&next.owner, &read.lease).unwrap();
    store.release_retention(&next.owner.session, &blob).unwrap();
    empty(&mut store);
}

#[test]
fn bulk_b06_cancel_after_copy_before_finish_rejects_late_snapshot() {
    let (_temp, mut store, parent) = fixture(BulkLimits::default());
    let ticket = write_bytes(&mut store, &parent, b"abc");
    let prepared = store
        .prepare_commit(&parent, &ticket.ticket, 3, &digest(b"abc"))
        .unwrap()
        .run()
        .unwrap();
    store.retire_parent(&parent);
    assert_eq!(
        store.finish_commit(prepared).err(),
        Some(BulkError::Unavailable)
    );
    empty(&mut store);
}

#[cfg(unix)]
#[test]
fn bulk_b07_enospc_after_real_partial_write_publishes_nothing() {
    let (_temp, mut store, parent) = fixture(BulkLimits::default());
    let payload = vec![42; 128 * 1024];
    let ticket = write_bytes(&mut store, &parent, &payload);
    let job = store
        .prepare_commit(
            &parent,
            &ticket.ticket,
            payload.len() as u64,
            &digest(&payload),
        )
        .unwrap();
    let partial = job.snapshot.path.clone();
    let observed = partial.clone();
    let _hook = Hook::install(move |copied| {
        assert_eq!(copied, 64 * 1024);
        assert_eq!(fs::metadata(&observed)?.len(), copied);
        Err(std::io::Error::from_raw_os_error(libc::ENOSPC).into())
    });
    assert_eq!(job.run().err(), Some(BulkError::StorageUnavailable));
    assert!(!partial.exists());
    empty(&mut store);
    // Admission capacity is available again, without a caller cleanup RPC.
    assert!(store
        .write(&parent, 128 * 1024, "application/octet-stream")
        .is_ok());
}

#[test]
fn bulk_b08_lease_release_pins_and_fresh_lease() {
    let (_temp, mut store, parent) = fixture(BulkLimits::default());
    let blob = publish(&mut store, &parent, b"abc");
    let first = lease(&mut store, &parent.owner, &blob);
    store.release(&parent.owner, &first.lease).unwrap();
    assert!(!store.transfer_directory().join(first.locator).exists());
    assert_eq!(
        store.release(&parent.owner, &first.lease),
        Err(BulkError::Unavailable)
    );
    let second = lease(&mut store, &parent.owner, &blob);
    assert_ne!(first.lease, second.lease);
    store
        .release_retention(&parent.owner.session, &blob)
        .unwrap();
    assert_eq!(
        store.prepare_read(&parent.owner, &blob).err(),
        Some(BulkError::Unavailable)
    );
    assert_eq!(file_count(store.backing.0.path()), 1);
    assert_eq!(
        fs::read(store.transfer_directory().join(second.locator)).unwrap(),
        b"abc"
    );
    store.release(&parent.owner, &second.lease).unwrap();
    empty(&mut store);
}

#[test]
fn bulk_b09_no_grant_and_exact_metadata() {
    let (_temp, mut store, parent) = fixture(BulkLimits::default());
    let blob = publish(&mut store, &parent, b"abc");
    let mut foreign = parent.owner.clone();
    foreign.session = "session-b".into();
    assert_eq!(
        store.prepare_read(&foreign, &blob).err(),
        Some(BulkError::Unavailable)
    );
    for field in 0..4 {
        let mut changed = blob.clone();
        match field {
            0 => changed.id = "unknown".into(),
            1 => changed.bytes += 1,
            2 => changed.media_type = "text/plain".into(),
            _ => changed.digest = digest(b"xyz"),
        }
        assert_eq!(
            store.prepare_read(&parent.owner, &changed).err(),
            Some(BulkError::Unavailable)
        );
    }
    assert_eq!(file_count(store.transfer_directory()), 0);
}

#[test]
fn bulk_b10_retained_blob_survives_producer_restart_but_grants_do_not() {
    let (_temp, mut store, parent) = fixture(BulkLimits::default());
    let blob = publish(&mut store, &parent, b"abc");
    let old_lease = lease(&mut store, &parent.owner, &blob);
    let mut ongoing = parent.clone();
    ongoing.request_id = "ongoing".into();
    store.begin_parent(&ongoing);
    let old_ticket = write_bytes(&mut store, &ongoing, b"def");
    let prepared_read = store
        .prepare_read(&parent.owner, &blob)
        .unwrap()
        .run()
        .unwrap();
    store.retire_generation(&parent.owner);
    assert_eq!(
        store.finish_read(prepared_read).err(),
        Some(BulkError::Unavailable)
    );
    assert_eq!(
        store.release(&parent.owner, &old_lease.lease),
        Err(BulkError::Unavailable)
    );
    assert_eq!(
        store
            .prepare_commit(&ongoing, &old_ticket.ticket, 3, &digest(b"def"))
            .err(),
        Some(BulkError::Unavailable)
    );
    let mut restarted = parent.owner.clone();
    restarted.generation += 1;
    let new_lease = lease(&mut store, &restarted, &blob);
    assert_ne!(old_lease.lease, new_lease.lease);
    assert_eq!(
        fs::read(store.transfer_directory().join(new_lease.locator)).unwrap(),
        b"abc"
    );
    store.retire_owner(&parent.owner.session);
    assert_eq!(
        store.prepare_read(&restarted, &blob).err(),
        Some(BulkError::Unavailable)
    );
    empty(&mut store);
}

#[test]
fn bulk_b12_producer_and_reader_mutation_cannot_change_snapshot() {
    let (_temp, mut store, parent) = fixture(BulkLimits::default());
    let ticket = write_bytes(&mut store, &parent, b"abc");
    let path = store.transfer_directory().join(&ticket.locator);
    let mut producer_fd = fs::OpenOptions::new().write(true).open(&path).unwrap();
    let prepared = store
        .prepare_commit(&parent, &ticket.ticket, 3, &digest(b"abc"))
        .unwrap()
        .run()
        .unwrap();
    let blob = store.finish_commit(prepared).unwrap();
    store
        .admit_parent(&parent, std::slice::from_ref(&blob))
        .unwrap();
    producer_fd.seek(SeekFrom::Start(0)).unwrap();
    producer_fd.write_all(b"xyz").unwrap();
    drop(producer_fd);
    let first = lease(&mut store, &parent.owner, &blob);
    let path = store.transfer_directory().join(first.locator);
    assert_eq!(fs::read(&path).unwrap(), b"abc");
    fs::write(&path, b"bad").unwrap();
    let second = lease(&mut store, &parent.owner, &blob);
    assert_eq!(
        fs::read(store.transfer_directory().join(second.locator)).unwrap(),
        b"abc"
    );
    assert_eq!(blob.digest, digest(b"abc"));
}

#[cfg(unix)]
#[test]
fn bulk_b13_symlink_foreign_ticket_and_unsafe_locator_refused() {
    let (temp, mut store, parent) = fixture(BulkLimits::default());
    let ticket = write_bytes(&mut store, &parent, b"abc");
    let mut other = parent.clone();
    other.request_id = "call-2".into();
    store.begin_parent(&other);
    assert_eq!(
        store
            .prepare_commit(&other, &ticket.ticket, 3, &digest(b"abc"))
            .err(),
        Some(BulkError::Unavailable)
    );
    for invalid in ["../outside", "/etc/passwd", &ticket.locator] {
        assert_eq!(
            store
                .prepare_commit(&parent, invalid, 3, &digest(b"abc"))
                .err(),
            Some(BulkError::Unavailable)
        );
    }
    let outside = temp.path().join("outside");
    fs::write(&outside, b"do not touch").unwrap();
    let scratch = store.transfer_directory().join(ticket.locator);
    fs::remove_file(&scratch).unwrap();
    std::os::unix::fs::symlink(&outside, &scratch).unwrap();
    let job = store
        .prepare_commit(&parent, &ticket.ticket, 3, &digest(b"abc"))
        .unwrap();
    assert_eq!(job.run().err(), Some(BulkError::StorageUnavailable));
    assert_eq!(fs::read(&outside).unwrap(), b"do not touch");
    // Secure cleanup deliberately refuses to follow/delete the substituted link.
    fs::remove_file(scratch).unwrap();
    empty(&mut store);

    let blob = publish(&mut store, &parent, b"abc");
    let prepared = store
        .prepare_read(&parent.owner, &blob)
        .unwrap()
        .run()
        .unwrap();
    let transfer = prepared.0.transfer.path.clone();
    fs::remove_file(&transfer).unwrap();
    std::os::unix::fs::symlink(&outside, &transfer).unwrap();
    assert_eq!(
        store.finish_read(prepared).err(),
        Some(BulkError::StorageUnavailable)
    );
    assert_eq!(fs::read(&outside).unwrap(), b"do not touch");
    fs::remove_file(transfer).unwrap();
    store
        .release_retention(&parent.owner.session, &blob)
        .unwrap();
    empty(&mut store);
}

#[test]
fn bulk_b14_reservation_retained_and_lease_limits() {
    let (_temp, mut store, parent) = fixture(BulkLimits {
        object_bytes: 4,
        owner_bytes: 4,
        read_leases_per_generation: 1,
        blobs_per_owner: 2,
        ..BulkLimits::default()
    });
    let blob = publish(&mut store, &parent, b"abcd");
    let mut next = parent.clone();
    next.request_id = "call-2".into();
    store.begin_parent(&next);
    assert_eq!(
        store.write(&next, 1, "text/plain").err(),
        Some(BulkError::QuotaExceeded)
    );
    let pending = store.prepare_read(&parent.owner, &blob).unwrap();
    assert_eq!(
        store.prepare_read(&parent.owner, &blob).err(),
        Some(BulkError::QuotaExceeded)
    );
    drop(pending);
    let read = lease(&mut store, &parent.owner, &blob);
    store
        .release_retention(&parent.owner.session, &blob)
        .unwrap();
    assert_eq!(
        store.write(&next, 1, "text/plain").err(),
        Some(BulkError::QuotaExceeded)
    );
    store.release(&parent.owner, &read.lease).unwrap();
    let zero = provisional(&mut store, &next, b"");
    let zero2 = provisional(&mut store, &next, b"");
    assert_ne!(zero.id, zero2.id);
    assert_eq!(
        store.write(&next, 0, "text/plain").err(),
        Some(BulkError::QuotaExceeded)
    );
    store.retire_parent(&next);
    empty(&mut store);
}

#[test]
fn bulk_b16_equal_bytes_never_share_identity_or_owner_grants() {
    let (_temp, mut store, parent) = fixture(BulkLimits::default());
    let first = publish(&mut store, &parent, b"abc");
    let mut other = parent.clone();
    other.owner.session = "session-b".into();
    store.begin_parent(&other);
    let second = publish(&mut store, &other, b"abc");
    assert_eq!(first.digest, second.digest);
    assert_ne!(first.id, second.id);
    assert_eq!(
        store.prepare_read(&parent.owner, &second).err(),
        Some(BulkError::Unavailable)
    );
    assert_eq!(
        store.prepare_read(&other.owner, &first).err(),
        Some(BulkError::Unavailable)
    );
}

#[test]
fn bulk_b17_invalid_output_is_atomic_and_other_parent_cannot_export() {
    let (_temp, mut store, parent) = fixture(BulkLimits::default());
    let first = provisional(&mut store, &parent, b"abc");
    let second = provisional(&mut store, &parent, b"def");
    let mut other = parent.clone();
    other.request_id = "other".into();
    store.begin_parent(&other);
    assert_eq!(
        store.validate_outputs(&other, std::slice::from_ref(&first)),
        Err(BulkError::Unavailable)
    );
    let mut bad = second.clone();
    bad.bytes += 1;
    assert_eq!(
        store.admit_parent(&parent, &[first.clone(), bad]),
        Err(BulkError::Unavailable)
    );
    assert_eq!(
        store.prepare_read(&parent.owner, &first).err(),
        Some(BulkError::Unavailable)
    );
    assert_eq!(
        store.prepare_read(&parent.owner, &second).err(),
        Some(BulkError::Unavailable)
    );
    empty(&mut store);
}

#[test]
fn bulk_unexported_snapshot_and_pending_ticket_are_discarded_on_success() {
    let (_temp, mut store, parent) = fixture(BulkLimits::default());
    let exported = provisional(&mut store, &parent, b"abc");
    let hidden = provisional(&mut store, &parent, b"def");
    write_bytes(&mut store, &parent, b"ghi");
    store
        .admit_parent(&parent, std::slice::from_ref(&exported))
        .unwrap();
    assert_eq!(
        store.prepare_read(&parent.owner, &hidden).err(),
        Some(BulkError::Unavailable)
    );
    assert!(store.tickets.is_empty());
    assert_eq!(file_count(store.backing.0.path()), 1);
    assert_eq!(file_count(store.transfer_directory()), 0);
}

#[test]
fn bulk_limits_mime_and_digest_are_bounded() {
    let (temp, mut store, parent) = fixture(BulkLimits::default());
    assert_eq!(store.limits().object_bytes, 256 * 1024 * 1024);
    assert_eq!(store.limits().owner_bytes, 512 * 1024 * 1024);
    assert_eq!(BulkError::StorageUnavailable.code(), "storage_unavailable");
    assert!(BulkStore::new(
        &temp.path().canonicalize().unwrap(),
        BulkLimits {
            object_bytes: u64::MAX,
            ..BulkLimits::default()
        }
    )
    .is_err());
    for invalid in [
        "",
        "text",
        "text/*",
        "text/plain\r\n",
        "text/plain/other",
        "text/plain;invalid",
        "text/plain; charset=\"unterminated",
        "text/plain; charset=utf-8;",
        "text/plain;\x0bcharset=utf-8",
    ] {
        assert_eq!(
            store.write(&parent, 0, invalid).err(),
            Some(BulkError::UnsupportedFeature)
        );
    }
    let ticket = write_bytes(&mut store, &parent, b"abc");
    for valid in [
        "text/plain; charset=utf-8",
        "application/octet-stream; label=\"a;b\"",
        "text/plain; a=\"escaped\\\"quote\"",
    ] {
        let ticket = store.write(&parent, 0, valid).unwrap();
        store.release(&parent.owner, &ticket.ticket).unwrap();
    }
    let mut wrong = digest(b"abc");
    wrong.value = wrong.value.to_uppercase();
    assert_eq!(
        store
            .prepare_commit(&parent, &ticket.ticket, 3, &wrong)
            .err(),
        Some(BulkError::IntegrityMismatch)
    );
    empty(&mut store);
}

#[test]
fn bulk_store_drop_cleans_files_and_fences_inflight_copy() {
    let (temp, mut store, parent) = fixture(BulkLimits::default());
    let ticket = write_bytes(&mut store, &parent, b"abc");
    let job = store
        .prepare_commit(&parent, &ticket.ticket, 3, &digest(b"abc"))
        .unwrap();
    drop(store);
    assert_eq!(job.run().err(), Some(BulkError::Unavailable));
    assert_eq!(file_count(temp.path()), 0);
}
