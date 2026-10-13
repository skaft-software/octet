//! Fail-closed backend for platforms with neither Unix nor Windows
//! descriptor-relative primitives. It is declared in `super` under
//! `#[cfg(not(any(unix, windows)))]` and satisfies the same private
//! `imp` interface as its two siblings by returning
//! `SecureFileError::Unsupported` from every operation, so a platform we
//! have not audited cannot silently degrade to path-based access.
//!
//! It is a separate file so the fail-closed guarantee is readable in one
//! place: a reviewer confirming "does octet touch the filesystem
//! unsafely on an unknown target?" reads only this file.

use super::*;

pub(super) struct PrivateDirectoryIdentity;

fn unsupported<T>() -> Result<T, SecureFileError> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "descriptor-bound filesystem access is unavailable on this platform",
    )
    .into())
}

pub(super) fn create_private_directory_all(_path: &Path) -> Result<(), SecureFileError> {
    unsupported()
}

pub(super) fn create_unique_private_directory(
    _parent: &Path,
    _prefix: &str,
) -> Result<(std::ffi::OsString, PrivateDirectoryIdentity, std::fs::File), SecureFileError> {
    unsupported()
}

pub(super) fn open_private_directory_for_lock(
    _path: &Path,
) -> Result<std::fs::File, SecureFileError> {
    unsupported()
}

pub(super) fn remove_regular_file_if_exists(_path: &Path) -> Result<bool, SecureFileError> {
    unsupported()
}

pub(super) fn remove_regular_file_if_exists_in(
    _path: &Path,
    _expected: &PrivateDirectoryIdentity,
) -> Result<bool, SecureFileError> {
    unsupported()
}

#[cfg(test)]
pub(super) fn remove_empty_private_directory_if_exists(
    _path: &Path,
) -> Result<bool, SecureFileError> {
    unsupported()
}

pub(super) fn remove_empty_private_directory_if_exists_bound(
    _path: &Path,
    _expected: &PrivateDirectoryIdentity,
) -> Result<bool, SecureFileError> {
    unsupported()
}

pub(super) fn read_regular_file_bounded(
    _path: &Path,
    _limit: usize,
) -> Result<Vec<u8>, SecureFileError> {
    unsupported()
}

pub(super) fn read_regular_file_bounded_by(
    _path: &Path,
    _upper_limit: usize,
    _byte_limit: &dyn Fn(&[u8]) -> usize,
) -> Result<Vec<u8>, SecureFileError> {
    unsupported()
}

pub(super) fn read_private_file_bounded(
    _path: &Path,
    _limit: usize,
) -> Result<Vec<u8>, SecureFileError> {
    unsupported()
}

pub(super) fn open_private_file_for_read(_path: &Path) -> Result<std::fs::File, SecureFileError> {
    unsupported()
}

pub(super) fn open_regular_file_for_read(_path: &Path) -> Result<std::fs::File, SecureFileError> {
    unsupported()
}

#[cfg(test)]
pub(super) fn open_regular_file_for_read_in(
    _path: &Path,
    _expected: &PrivateDirectoryIdentity,
) -> Result<std::fs::File, SecureFileError> {
    unsupported()
}

pub(super) fn open_regular_file_for_append(_path: &Path) -> Result<std::fs::File, SecureFileError> {
    unsupported()
}

pub(super) fn open_regular_file_for_append_in(
    _path: &Path,
    _expected: &PrivateDirectoryIdentity,
) -> Result<std::fs::File, SecureFileError> {
    unsupported()
}

pub(super) fn create_regular_file_for_append(
    _path: &Path,
) -> Result<std::fs::File, SecureFileError> {
    unsupported()
}

pub(super) fn create_regular_file_for_append_in(
    _path: &Path,
    _expected: &PrivateDirectoryIdentity,
) -> Result<std::fs::File, SecureFileError> {
    unsupported()
}

pub(super) fn open_private_lock_file(_path: &Path) -> Result<std::fs::File, SecureFileError> {
    unsupported()
}

pub(super) struct PrivateLockIdentity;

pub(super) fn validate_private_lock_after_acquire(
    _path: &Path,
    _file: &std::fs::File,
) -> Result<PrivateLockIdentity, SecureFileError> {
    unsupported()
}

pub(super) fn revalidate_private_lock_before_release(
    _path: &Path,
    _file: &std::fs::File,
    _expected: &PrivateLockIdentity,
) -> Result<(), SecureFileError> {
    unsupported()
}

pub(super) struct PreparedMutation;

impl PreparedMutation {
    pub(super) fn prepare(
        _path: &Path,
        _create_parents: bool,
        _limit: usize,
    ) -> Result<Self, SecureFileError> {
        unsupported()
    }

    pub(super) fn prepare_private(_path: &Path, _limit: usize) -> Result<Self, SecureFileError> {
        unsupported()
    }

    pub(super) fn original(&self) -> Option<&[u8]> {
        None
    }

    pub(super) fn remove(self) -> Result<(), SecureFileError> {
        unsupported()
    }

    pub(super) fn commit(
        self,
        _data: &[u8],
        _cancelled: &dyn Fn() -> bool,
    ) -> Result<(), SecureFileError> {
        unsupported()
    }

    pub(super) fn commit_private(
        self,
        _data: &[u8],
        _cancelled: &dyn Fn() -> bool,
    ) -> Result<(), SecureFileError> {
        unsupported()
    }
}
