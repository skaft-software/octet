//! Recoverable Windows bundle replacement. Windows cannot exchange two populated
//! directories. Keep a synced intent and the displaced bundle in a non-temporary
//! directory until publication succeeds. The package lock covers recovery and
//! discovery as well as writers; these two renames are deliberately NOT described
//! as atomic or as a power-loss durability guarantee.

use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};

use super::*;
use crate::extension_bundle::{installed_record_digest, validate_bundle_id};

const UPDATE_PREFIX: &str = ".octet-extension-update-";
const JOURNAL: &str = "intent.json";
const PREVIOUS: &str = "previous";
const MAX_JOURNAL_BYTES: usize = 4096;
const MAX_ROOT_ENTRIES: usize = 4096;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct UpdateIntent {
    schema_version: u32,
    previous_record: String,
    candidate_record: String,
}

/// Check without following symlinks OR Windows junctions/reparse points.
/// Missing is distinct from inaccessible or redirected.
pub(super) fn directory_exists(path: &Path) -> anyhow::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.is_dir() && metadata.file_attributes() & 0x400 == 0,
                "extension update path is not a regular directory: {}",
                path.display()
            );
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("cannot inspect {}", path.display())),
    }
}

/// Unlike std::fs::rename's Windows implementation, never request replacement
/// of a destination name. This also applies to rollback/recovery: a foreign
/// writer that fills the publication gap must not lose its files.
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let wide = |path: &Path| -> io::Result<Vec<u16>> {
        let mut value = path.as_os_str().encode_wide().collect::<Vec<_>>();
        if value.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path contains a NUL byte",
            ));
        }
        value.push(0);
        Ok(value)
    };
    let from = wide(from)?;
    let to = wide(to)?;
    // SAFETY: both buffers are NUL-terminated and live through the call. No
    // COPY_ALLOWED or REPLACE_EXISTING: both directories stay on this volume.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// A committed update may still have a partly deleted previous bundle. Do not
/// remove/replace its destination until cleanup finishes: recovery could otherwise
/// mistake removal for interrupted publication and restore the stale backup.
/// Called under the package lock with an already validated bundle ID.
pub(crate) fn ensure_update_finished(root: &Path, id: &str) -> anyhow::Result<()> {
    let transaction = root.join(format!("{UPDATE_PREFIX}{id}"));
    anyhow::ensure!(
        !directory_exists(&transaction)?,
        "extension update cleanup is still pending at {}; close programs using the old bundle and retry 'octet extension list' before removing or updating it",
        transaction.display()
    );
    Ok(())
}

pub(super) fn publish_update(
    root: &Path,
    staging: &Path,
    destination: &Path,
    id: &str,
) -> anyhow::Result<()> {
    publish_update_observed(root, staging, destination, id, |_| {})
}

// The observer lets tests interrupt the real publication path in a child
// process or arrange filesystem failures at exact rename boundaries. Release
// callers use the no-op above; there is no environment-controlled fault hook.
fn publish_update_observed(
    root: &Path,
    staging: &Path,
    destination: &Path,
    id: &str,
    mut observe: impl FnMut(&str),
) -> anyhow::Result<()> {
    validate_bundle_id(id)?;
    ensure_update_finished(root, id)?;
    anyhow::ensure!(directory_exists(staging)?, "extension candidate is missing");
    anyhow::ensure!(
        directory_exists(destination)?,
        "installed extension is missing"
    );
    let intent = UpdateIntent {
        schema_version: 1,
        previous_record: installed_record_digest(destination, id)?,
        candidate_record: installed_record_digest(staging, id)?,
    };
    let transaction = root.join(format!("{UPDATE_PREFIX}{id}"));
    fs::create_dir(&transaction).with_context(|| {
        format!(
            "cannot prepare extension update at {}",
            transaction.display()
        )
    })?;
    observe("created");
    let prepare = (|| -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(transaction.join(JOURNAL))?;
        file.write_all(&serde_json::to_vec(&intent)?)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = prepare {
        cleanup_warning(&transaction);
        return Err(error)
            .context("cannot record extension update; previous install was not moved");
    }
    observe("prepared");
    let previous = transaction.join(PREVIOUS);
    if let Err(error) = rename_no_replace(destination, &previous) {
        cleanup_warning(&transaction);
        return Err(error).context(
            "cannot move installed extension; previous install was not changed; close other octet instances and programs using the bundle, then retry",
        );
    }
    observe("displaced");
    if let Err(error) = rename_no_replace(staging, destination) {
        match rename_no_replace(&previous, destination) {
            Ok(()) => {
                observe("restored");
                cleanup_warning(&transaction);
                return Err(error).context(
                    "cannot publish extension update; previous install was restored; close programs using the bundle, then retry",
                );
            }
            Err(restore_error) => anyhow::bail!(
                "cannot publish extension update: {error}; cannot restore previous install: {restore_error}; previous bundle retained at {}; close programs using the bundle; if {} is occupied by a conflicting file or directory, move it aside; retry 'octet extension list' to recover; do not delete this backup",
                previous.display(), destination.display()
            ),
        }
    }
    observe("published");
    // Publication is committed. Cleanup may fail on Windows while another
    // process still holds old files; retain the intent and retry next time.
    cleanup_warning(&transaction);
    Ok(())
}

fn cleanup(transaction: &Path) -> anyhow::Result<()> {
    let previous = transaction.join(PREVIOUS);
    if directory_exists(&previous)? {
        fs::remove_dir_all(&previous)?;
    }
    let journal = transaction.join(JOURNAL);
    match fs::symlink_metadata(&journal) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.is_file() && metadata.file_attributes() & 0x400 == 0,
                "extension update intent is not a regular file: {}",
                journal.display()
            );
            fs::remove_file(journal)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    // Do not recursively remove the transaction itself: unexpected files are
    // not ours. In particular, do not turn an invalid journal into a delete list.
    fs::remove_dir(transaction)?;
    Ok(())
}

fn cleanup_warning(transaction: &Path) {
    if let Err(error) = cleanup(transaction) {
        crate::output::stderr_line(format!(
            "warning: extension update cleanup pending at {}: {error:#}; close programs using the old bundle and retry 'octet extension list'",
            transaction.display()
        ));
    }
}

/// Called only while holding the stable .package.lock, never from extension
/// code or untrusted project discovery. Recovery never runs a candidate.
pub(super) fn recover_updates(root: &Path) -> anyhow::Result<()> {
    let mut transactions = Vec::new();
    for (index, entry) in fs::read_dir(root)?.enumerate() {
        anyhow::ensure!(
            index < MAX_ROOT_ENTRIES,
            "extension root exceeds the {MAX_ROOT_ENTRIES}-entry recovery limit"
        );
        let entry = entry?;
        let name = entry.file_name();
        let Some(id) = name
            .to_str()
            .and_then(|name| name.strip_prefix(UPDATE_PREFIX))
        else {
            continue;
        };
        validate_bundle_id(id)?;
        transactions.push((entry.path(), id.to_owned()));
    }
    transactions.sort();
    for (transaction, id) in transactions {
        recover_update(root, &transaction, &id).with_context(|| {
            format!(
                "cannot recover extension update at {}; retained files were not discarded",
                transaction.display()
            )
        })?;
    }
    Ok(())
}

fn recover_update(root: &Path, transaction: &Path, id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        directory_exists(transaction)?,
        "update recovery directory is missing"
    );
    let destination = root.join(id);
    let previous = transaction.join(PREVIOUS);
    let has_destination = directory_exists(&destination)?;
    if !directory_exists(&previous)? {
        // Interrupted before displacement (possibly during the intent write),
        // after rollback, or after previous-package cleanup. No bundle is at
        // risk here, but don't remove metadata unless a managed install exists.
        anyhow::ensure!(
            has_destination,
            "both installed and previous bundle are missing"
        );
        installed_record_digest(&destination, id)?;
        cleanup(transaction)?;
        return Ok(());
    }
    let bytes = octet_agent::secure_fs::read_regular_file_bounded(
        &transaction.join(JOURNAL),
        MAX_JOURNAL_BYTES,
    )?;
    let intent: UpdateIntent = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        intent.schema_version == 1,
        "unsupported extension update intent schema"
    );
    validate_sha256(&intent.previous_record)?;
    validate_sha256(&intent.candidate_record)?;
    if has_destination {
        anyhow::ensure!(
            installed_record_digest(&destination, id)? == intent.candidate_record,
            "extension destination changed during update; previous bundle retained at {}; move the conflicting destination aside before retrying recovery",
            previous.display()
        );
        // The candidate is already published. Previous may be partly deleted
        // if cleanup was interrupted, so its install record need not survive.
        cleanup_warning(transaction);
    } else {
        anyhow::ensure!(
            installed_record_digest(&previous, id)? == intent.previous_record,
            "previous extension install record changed; refusing to restore it"
        );
        rename_no_replace(&previous, &destination).context(
            "cannot restore previous extension; close programs using the bundle, then retry",
        )?;
        cleanup(transaction)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
