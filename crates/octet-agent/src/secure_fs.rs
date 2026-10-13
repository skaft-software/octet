//! Descriptor-bound, bounded local-file operations.
//!
//! On Unix, path components are opened one at a time with `O_NOFOLLOW`; on
//! Windows they are opened relative to already-authorized directory handles
//! with reparse-point traversal disabled. Mutations stay bound to those parent
//! handles, and private Windows objects use a protected current-user-only ACL.
//! Replacing an existing file is compare-and-swap on both: Linux and macOS
//! exchange the names atomically and roll back if the displaced object is not
//! the one observed; Windows, which has no exchange, pins the verified object
//! against writers and deleters, renames it aside, and publishes with a
//! no-replace rename. On Unix, octet's own conditional mutations of one file
//! also serialize on an advisory lock of that file, so a writer with a stale
//! snapshot fails its check before publishing anything. Platforms without
//! descriptor-relative primitives fail closed.
//!
//! This file owns the platform-independent surface only: the single
//! `SecureFileError` vocabulary, and the free functions and handle types
//! that every caller uses. The three `imp` backends behind those functions
//! live beside it as `secure_fs/imp_unix.rs`, `secure_fs/imp_windows.rs`
//! and `secure_fs/imp_other.rs`, one per platform family, so that the two
//! large syscall vocabularies do not have to be cfg-pruned out of each
//! other. The `#[path]` attributes below are what mount them all under the
//! one name `imp`, which is the only name the functions above refer to.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// Failures produced by bounded descriptor-based file access.
#[derive(Debug, thiserror::Error)]
pub enum SecureFileError {
    /// The path shape cannot identify a normal file.
    #[error("invalid file path: {0}")]
    InvalidPath(String),
    /// The opened object is not a regular file.
    #[error("not a regular file")]
    NotRegular,
    /// A secret-bearing file or directory does not have owner-only identity
    /// and permissions.
    #[error("private filesystem object is not owner-only: {0}")]
    InsecurePrivateObject(String),
    /// Reading one regular file crossed the supplied hard byte limit.
    #[error("file is too large to read ({actual} bytes, limit {limit})")]
    TooLarge {
        /// Bytes observed, or the minimum known size when a stream crossed the cap.
        actual: u64,
        /// Configured maximum bytes.
        limit: usize,
    },
    /// The target changed between inspection and commit.
    #[error("file changed while the operation was in progress")]
    Changed,
    /// Cooperative cancellation won before the rename commit point.
    #[error("file operation cancelled")]
    Cancelled,
    /// The platform or filesystem cannot atomically replace an existing target
    /// while preserving compare-and-swap semantics.
    #[error("atomic conditional file replacement is unavailable")]
    PublicationUnavailable,
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn validate_absolute_file_path(path: &Path) -> Result<(), SecureFileError> {
    if !path.is_absolute() {
        return Err(SecureFileError::InvalidPath(format!(
            "{} is not absolute",
            path.display()
        )));
    }
    let mut normal = 0usize;
    for component in path.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => {}
            Component::Normal(_) => normal += 1,
            Component::CurDir | Component::ParentDir => {
                return Err(SecureFileError::InvalidPath(path.display().to_string()))
            }
        }
    }
    if normal == 0 {
        return Err(SecureFileError::InvalidPath(path.display().to_string()));
    }
    Ok(())
}

const INSPECTION_BYTES: usize = 512;
const TEMP_NAME_ATTEMPTS: usize = 128;

fn random_temp_suffix() -> Result<String, SecureFileError> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| {
        SecureFileError::Io(std::io::Error::other(format!(
            "secure random generation failed: {error}"
        )))
    })?;
    let mut suffix = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        suffix.push(char::from(HEX[usize::from(byte >> 4)]));
        suffix.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(suffix)
}

fn read_open_regular_bounded_by(
    mut file: std::fs::File,
    upper_limit: usize,
    byte_limit: &dyn Fn(&[u8]) -> usize,
) -> Result<Vec<u8>, SecureFileError> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(SecureFileError::NotRegular);
    }
    if metadata.len() > upper_limit as u64 {
        return Err(SecureFileError::TooLarge {
            actual: metadata.len(),
            limit: upper_limit,
        });
    }

    // Inspect a fixed-size prefix before reserving for the complete file. This
    // lets callers apply a tighter content-derived cap without first buffering
    // up to the more permissive fallback limit.
    let prefix_len = (metadata.len() as usize)
        .min(upper_limit)
        .min(INSPECTION_BYTES);
    let mut bytes = Vec::with_capacity(prefix_len);
    Read::by_ref(&mut file)
        .take(prefix_len as u64)
        .read_to_end(&mut bytes)?;
    let limit = byte_limit(&bytes).min(upper_limit);
    if metadata.len() > limit as u64 {
        return Err(SecureFileError::TooLarge {
            actual: metadata.len(),
            limit,
        });
    }

    bytes.reserve((metadata.len() as usize).saturating_sub(bytes.len()));
    let remaining_limit = limit.saturating_add(1).saturating_sub(bytes.len());
    Read::by_ref(&mut file)
        .take(remaining_limit as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(SecureFileError::TooLarge {
            actual: bytes.len() as u64,
            limit,
        });
    }
    Ok(bytes)
}

fn read_open_regular(file: std::fs::File, limit: usize) -> Result<Vec<u8>, SecureFileError> {
    read_open_regular_bounded_by(file, limit, &|_| limit)
}

/// Read exactly one regular file, rejecting symlinks and special files and
/// enforcing the byte limit on bytes actually read rather than metadata alone.
///
/// `path` must be absolute. On Unix and Windows every component is opened
/// relative to the previously opened directory handle, so parent replacement
/// cannot redirect the read after validation.
pub fn read_regular_file_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, SecureFileError> {
    validate_absolute_file_path(path)?;
    imp::read_regular_file_bounded(path, limit)
}

/// Remove one existing regular file through a descriptor-bound path walk.
///
/// Returns `true` if a file was removed and `false` when it was already
/// absent. Symbolic links and special files are rejected. On Unix, the final
/// name is atomically moved to a private random name and revalidated before it
/// is unlinked; on Windows the opened object is deleted by handle. Either way a
/// replacement cannot be removed by cleanup.
pub fn remove_regular_file_if_exists(path: &Path) -> Result<bool, SecureFileError> {
    validate_absolute_file_path(path)?;
    imp::remove_regular_file_if_exists(path)
}

/// Remove a regular file only when its bytes and filesystem identity still
/// match `expected`.
///
/// This is a descriptor-bound compare-and-delete operation. It rejects missing,
/// replaced, linked, symbolic, special, or changed targets rather than deleting
/// a concurrent writer's file.
pub fn remove_regular_file_if_unchanged(
    path: &Path,
    expected: &[u8],
    limit: usize,
) -> Result<(), SecureFileError> {
    validate_absolute_file_path(path)?;
    if expected.len() > limit {
        return Err(SecureFileError::TooLarge {
            actual: expected.len() as u64,
            limit,
        });
    }
    let prepared = PreparedMutation::prepare(path, false, limit)?;
    if prepared.original() != Some(expected) {
        return Err(SecureFileError::Changed);
    }
    prepared.remove()
}

/// Remove an owner-only regular file only when its bytes and filesystem
/// identity still match `expected`.
pub fn remove_private_file_if_unchanged(
    path: &Path,
    expected: &[u8],
    limit: usize,
) -> Result<(), SecureFileError> {
    validate_absolute_file_path(path)?;
    if expected.len() > limit {
        return Err(SecureFileError::TooLarge {
            actual: expected.len() as u64,
            limit,
        });
    }
    let prepared = PreparedMutation::prepare_private(path, limit)?;
    if prepared.original() != Some(expected) {
        return Err(SecureFileError::Changed);
    }
    prepared.remove()
}

/// Remove one existing empty owner-only directory through a descriptor-bound
/// path walk.
///
/// Returns `true` if the directory was removed and `false` when it was already
/// absent. Symbolic links, reparse points, non-directories, non-private
/// directories, and non-empty directories are rejected. This is intended for
/// rollback of freshly allocated private directories.
#[cfg(test)]
pub(crate) fn remove_empty_private_directory_if_exists(
    path: &Path,
) -> Result<bool, SecureFileError> {
    validate_absolute_file_path(path)?;
    imp::remove_empty_private_directory_if_exists(path)
}

/// Read one owner-only regular file through a descriptor-bound path walk.
///
/// In addition to rejecting symbolic links and special files, this requires
/// the file to be owned by the current user, to have no additional hard links,
/// and to expose no access outside the owner security boundary.
pub fn read_private_file_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, SecureFileError> {
    validate_absolute_file_path(path)?;
    imp::read_private_file_bounded(path, limit)
}

/// Open one owner-only regular file for descriptor-bound reads.
///
/// This applies the same ownership, link-count, permissions/ACL, and no-follow
/// checks as [`read_private_file_bounded`] while allowing callers to inspect
/// metadata and consume bytes through the exact same descriptor.
pub fn open_private_file_for_read(path: &Path) -> Result<std::fs::File, SecureFileError> {
    validate_absolute_file_path(path)?;
    imp::open_private_file_for_read(path)
}

/// Read one regular file with a content-derived limit selected from its first
/// 512 bytes. `upper_limit` remains an unconditional hard cap.
///
/// Inspection and reading use the same descriptor, so a path replacement
/// cannot switch content between classification and buffering.
pub fn read_regular_file_bounded_by(
    path: &Path,
    upper_limit: usize,
    byte_limit: impl Fn(&[u8]) -> usize,
) -> Result<Vec<u8>, SecureFileError> {
    validate_absolute_file_path(path)?;
    imp::read_regular_file_bounded_by(path, upper_limit, &byte_limit)
}

/// Open one existing regular file for descriptor-bound reads.
///
/// `path` must be absolute. Symbolic links and special files are rejected.
pub fn open_regular_file_for_read(path: &Path) -> Result<std::fs::File, SecureFileError> {
    validate_absolute_file_path(path)?;
    imp::open_regular_file_for_read(path)
}

/// Open one existing regular file for reading and durable appends.
///
/// `path` must be absolute. On Unix every component is opened relative to the
/// previously opened directory descriptor with symlink following disabled.
/// The returned descriptor therefore remains bound to the validated file even
/// if an ancestor or the final pathname is replaced concurrently.
pub fn open_regular_file_for_append(path: &Path) -> Result<std::fs::File, SecureFileError> {
    validate_absolute_file_path(path)?;
    imp::open_regular_file_for_append(path)
}

/// Atomically create one new regular file for reading and durable appends.
///
/// `path` must be absolute and its parent directories must already exist. On
/// Unix every parent component and the final create are descriptor-relative
/// with symlink following disabled. Existing targets are never overwritten.
pub fn create_regular_file_for_append(path: &Path) -> Result<std::fs::File, SecureFileError> {
    validate_absolute_file_path(path)?;
    imp::create_regular_file_for_append(path)
}

/// Create an absolute directory tree without following symbolic links and make
/// the final directory owner-only. Existing non-directory components fail.
pub fn create_private_directory_all(path: &Path) -> Result<(), SecureFileError> {
    validate_absolute_file_path(path)?;
    imp::create_private_directory_all(path)
}

fn validate_private_directory_prefix(prefix: &str) -> Result<(), SecureFileError> {
    if prefix.is_empty()
        || !prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(SecureFileError::InvalidPath(prefix.to_owned()));
    }
    Ok(())
}

/// A private directory paired with the stable filesystem identity captured
/// when it was authorized. Child operations refuse a replacement directory.
pub(crate) struct PrivateDirectory {
    path: PathBuf,
    identity: imp::PrivateDirectoryIdentity,
    // Keep the authorized object alive so its filesystem identity cannot be
    // recycled into a replacement while this capability is in use.
    _anchor: std::fs::File,
}

impl PrivateDirectory {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    fn validate_child_path(&self, path: &Path) -> Result<(), SecureFileError> {
        validate_absolute_file_path(path)?;
        if path.parent() != Some(self.path.as_path()) || path.file_name().is_none() {
            return Err(SecureFileError::InvalidPath(path.display().to_string()));
        }
        Ok(())
    }

    pub(crate) fn create_regular_file_for_append(
        &self,
        path: &Path,
    ) -> Result<std::fs::File, SecureFileError> {
        self.validate_child_path(path)?;
        imp::create_regular_file_for_append_in(path, &self.identity)
    }

    #[cfg(test)]
    pub(crate) fn open_regular_file_for_read(
        &self,
        path: &Path,
    ) -> Result<std::fs::File, SecureFileError> {
        self.validate_child_path(path)?;
        imp::open_regular_file_for_read_in(path, &self.identity)
    }

    pub(crate) fn open_regular_file_for_append(
        &self,
        path: &Path,
    ) -> Result<std::fs::File, SecureFileError> {
        self.validate_child_path(path)?;
        imp::open_regular_file_for_append_in(path, &self.identity)
    }

    pub(crate) fn remove_regular_file_if_exists(
        &self,
        path: &Path,
    ) -> Result<bool, SecureFileError> {
        self.validate_child_path(path)?;
        imp::remove_regular_file_if_exists_in(path, &self.identity)
    }

    pub(crate) fn remove_empty_if_exists(&self) -> Result<bool, SecureFileError> {
        imp::remove_empty_private_directory_if_exists_bound(&self.path, &self.identity)
    }
}

pub(crate) fn create_bound_private_directory(
    parent: &Path,
    prefix: &str,
) -> Result<PrivateDirectory, SecureFileError> {
    validate_absolute_file_path(parent)?;
    validate_private_directory_prefix(prefix)?;
    imp::create_private_directory_all(parent)?;
    let (name, identity, anchor) = imp::create_unique_private_directory(parent, prefix)?;
    Ok(PrivateDirectory {
        path: parent.join(name),
        identity,
        _anchor: anchor,
    })
}

/// Create a uniquely named owner-only child directory without following path
/// replacements during allocation.
///
/// `parent` must be absolute. `prefix` is restricted to portable filename
/// characters and the random suffix is generated from the operating system's
/// cryptographically secure random source. The final directory create is
/// exclusive and descriptor-relative to the validated private parent.
pub fn create_unique_private_directory(
    parent: &Path,
    prefix: &str,
) -> Result<PathBuf, SecureFileError> {
    Ok(create_bound_private_directory(parent, prefix)?.path)
}

/// Open an owner-only directory as a stable advisory-lock anchor.
///
/// The returned descriptor is bound to the private directory rather than to a
/// replaceable lock-file pathname. Retain it while holding the OS lock.
pub fn open_private_directory_for_lock(path: &Path) -> Result<std::fs::File, SecureFileError> {
    validate_absolute_file_path(path)?;
    imp::create_private_directory_all(path)?;
    imp::open_private_directory_for_lock(path)
}

/// Atomically publish owner-only bytes beneath an owner-only parent directory.
/// Existing targets must be regular files no larger than `limit`; concurrent
/// target replacement is rejected rather than overwritten.
pub fn write_private_atomic(path: &Path, data: &[u8], limit: usize) -> Result<(), SecureFileError> {
    validate_absolute_file_path(path)?;
    if data.len() > limit {
        return Err(SecureFileError::TooLarge {
            actual: data.len() as u64,
            limit,
        });
    }
    let parent = path
        .parent()
        .ok_or_else(|| SecureFileError::InvalidPath(path.display().to_string()))?;
    imp::create_private_directory_all(parent)?;
    imp::PreparedMutation::prepare_private(path, limit)?.commit_private(data, &|| false)
}

/// Atomically publish owner-only bytes only if the target still matches an
/// earlier caller snapshot.
///
/// This is the private counterpart to [`write_atomic_if_unchanged`]. Both the
/// snapshot and final comparison are descriptor-bound, while the target and
/// every parent retain the owner-only checks used by [`write_private_atomic`].
/// A symlink, hard-link, ownership, permission, or concurrent replacement is
/// rejected as [`SecureFileError::Changed`] rather than overwritten.
pub fn write_private_atomic_if_unchanged(
    path: &Path,
    expected: Option<&[u8]>,
    data: &[u8],
    limit: usize,
) -> Result<(), SecureFileError> {
    validate_absolute_file_path(path)?;
    if data.len() > limit {
        return Err(SecureFileError::TooLarge {
            actual: data.len() as u64,
            limit,
        });
    }
    let parent = path
        .parent()
        .ok_or_else(|| SecureFileError::InvalidPath(path.display().to_string()))?;
    imp::create_private_directory_all(parent)?;
    let prepared = imp::PreparedMutation::prepare_private(path, limit)?;
    if prepared.original() != expected {
        return Err(SecureFileError::Changed);
    }
    prepared.commit_private(data, &|| false)
}

/// Atomically publish regular-file bytes only if the target still matches an
/// earlier caller snapshot. The descriptor-bound mutation revalidates both
/// bytes and file identity, uses no-replace creation for a missing target, and
/// for an existing target uses a rollback-safe exchange (Linux, macOS) or a
/// pinned rename-aside followed by no-replace publication (Windows, where
/// readers can briefly find the target missing).
pub fn write_atomic_if_unchanged(
    path: &Path,
    expected: Option<&[u8]>,
    data: &[u8],
    limit: usize,
) -> Result<(), SecureFileError> {
    validate_absolute_file_path(path)?;
    if data.len() > limit {
        return Err(SecureFileError::TooLarge {
            actual: data.len() as u64,
            limit,
        });
    }
    let prepared = PreparedMutation::prepare(path, true, limit)?;
    if prepared.original() != expected {
        return Err(SecureFileError::Changed);
    }
    prepared.commit_if(data, || false)
}

/// Identity captured after a private lock file has been acquired and repaired.
///
/// Retain this value and revalidate it immediately before releasing the
/// operating-system lock.
pub struct PrivateLockIdentity(imp::PrivateLockIdentity);

/// Open or create a private regular advisory-lock file without following
/// symbolic links.
///
/// Existing metadata is checked for safe ownership, type, and link count, but
/// mode or ACL repair is deferred until after the caller acquires the
/// operating-system lock.
pub fn open_private_lock_file(path: &Path) -> Result<std::fs::File, SecureFileError> {
    validate_absolute_file_path(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| SecureFileError::InvalidPath(path.display().to_string()))?;
    imp::create_private_directory_all(parent)?;
    imp::open_private_lock_file(path)
}

/// Repair and bind a private lock file after acquiring its OS-level lock.
pub fn validate_private_lock_after_acquire(
    path: &Path,
    file: &std::fs::File,
) -> Result<PrivateLockIdentity, SecureFileError> {
    validate_absolute_file_path(path)?;
    imp::validate_private_lock_after_acquire(path, file).map(PrivateLockIdentity)
}

/// Revalidate a private lock file immediately before releasing its OS lock.
pub fn revalidate_private_lock_before_release(
    path: &Path,
    file: &std::fs::File,
    identity: &PrivateLockIdentity,
) -> Result<(), SecureFileError> {
    validate_absolute_file_path(path)?;
    imp::revalidate_private_lock_before_release(path, file, &identity.0)
}

/// A target inspected through an already-open parent directory. The original
/// bytes are retained both for caller-side edits/diffs and for the final
/// compare-before-rename conflict check.
pub(crate) struct PreparedMutation {
    inner: imp::PreparedMutation,
}

impl PreparedMutation {
    /// Open a target for a later atomic replacement. Missing parents are made
    /// only when `create_parents` is true. Existing targets must be regular
    /// files no larger than `limit`.
    pub(crate) fn prepare(
        path: &Path,
        create_parents: bool,
        limit: usize,
    ) -> Result<Self, SecureFileError> {
        validate_absolute_file_path(path)?;
        Ok(Self {
            inner: imp::PreparedMutation::prepare(path, create_parents, limit)?,
        })
    }

    fn prepare_private(path: &Path, limit: usize) -> Result<Self, SecureFileError> {
        validate_absolute_file_path(path)?;
        Ok(Self {
            inner: imp::PreparedMutation::prepare_private(path, limit)?,
        })
    }

    /// Original target bytes, or `None` when the target did not exist.
    pub(crate) fn original(&self) -> Option<&[u8]> {
        self.inner.original()
    }

    /// Atomically install `data` if the target still has exactly the state
    /// observed by [`prepare`](Self::prepare).
    #[cfg(test)]
    pub(crate) fn commit(self, data: &[u8]) -> Result<(), SecureFileError> {
        self.commit_if(data, || false)
    }

    /// Commit while polling a cooperative cancellation flag during bounded
    /// writes and immediately before rename.
    pub(crate) fn commit_if(
        self,
        data: &[u8],
        cancelled: impl Fn() -> bool,
    ) -> Result<(), SecureFileError> {
        self.inner.commit(data, &cancelled)
    }

    fn remove(self) -> Result<(), SecureFileError> {
        self.inner.remove()
    }
}

#[cfg(unix)]
#[path = "secure_fs/imp_unix.rs"]
mod imp;

#[cfg(windows)]
#[path = "secure_fs/imp_windows.rs"]
mod imp;

#[cfg(not(any(unix, windows)))]
#[path = "secure_fs/imp_other.rs"]
mod imp;

#[cfg(test)]
mod tests;
