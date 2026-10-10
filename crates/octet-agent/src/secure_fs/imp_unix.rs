//! Unix (Linux and macOS) half of the `imp` backend declared in
//! `super`.
//!
//! This owns every operation whose safety story depends on a POSIX
//! primitive: `O_NOFOLLOW` component-at-a-time traversal, `openat`-relative
//! mutation bound to the already-authorized parent directory, `flock` on the
//! target file to serialize octet's own conditional mutations, and
//! `renameat2(RENAME_EXCHANGE)` on Linux to publish a replacement while
//! rolling back if the displaced object is not the one observed.
//!
//! It is a separate file because it is the only place in the crate that
//! names a `rustix` or `std::os::unix` item, and because the sibling
//! `imp_windows.rs` and `imp_other.rs` backends implement the same private
//! interface with an entirely different syscall vocabulary. Keeping the
//! platform vocabularies in sibling files means neither has to be
//! cfg-pruned out of the other.

use super::*;
use rustix::fd::OwnedFd;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use rustix::fs::RenameFlags;
use rustix::fs::{AtFlags, FlockOperation, Mode, OFlags};
use rustix::io::Errno;
use std::ffi::{OsStr, OsString};
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

/// Longest wait for another conditional mutation of the same file.
const TARGET_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Takes an exclusive advisory lock on the object `name` currently names,
/// serializing octet's conditional mutations of one file across threads
/// and processes. Holding it across the `unchanged` check and publication
/// means only a writer whose snapshot is current can publish, so a stale
/// writer never swaps its bytes in and then has to roll them back.
///
/// Best effort: a missing or unreadable target, an unsupported lock, or a
/// holder that outlasts `TARGET_LOCK_WAIT` (an external tool) returns
/// `None`, and the caller continues with the unlocked checks alone.
fn lock_current_target(parent: &OwnedFd, name: &OsStr) -> Option<OwnedFd> {
    let target = rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    let deadline = std::time::Instant::now() + TARGET_LOCK_WAIT;
    loop {
        match rustix::fs::flock(&target, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Some(target),
            Err(Errno::WOULDBLOCK | Errno::INTR) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(_) => return None,
        }
    }
}

fn io_error(error: Errno) -> std::io::Error {
    std::io::Error::from_raw_os_error(error.raw_os_error())
}

fn effective_user_id() -> u32 {
    // SAFETY: `geteuid` has no preconditions and does not dereference
    // caller-provided memory.
    unsafe { libc::geteuid() }
}

fn insecure_private(reason: &str) -> SecureFileError {
    SecureFileError::InsecurePrivateObject(reason.to_owned())
}

fn validate_private_file_identity(
    file: &std::fs::File,
) -> Result<std::fs::Metadata, SecureFileError> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(SecureFileError::NotRegular);
    }
    if metadata.uid() != effective_user_id() {
        return Err(insecure_private("file is not owned by the current user"));
    }
    if metadata.nlink() != 1 {
        return Err(insecure_private("file has additional hard links"));
    }
    Ok(metadata)
}

fn validate_private_file(file: &std::fs::File) -> Result<(), SecureFileError> {
    let metadata = validate_private_file_identity(file)?;
    if metadata.mode() & 0o7777 != 0o600 {
        return Err(insecure_private("file mode is not 0600"));
    }
    Ok(())
}

fn make_private_file(file: &std::fs::File) -> Result<(), SecureFileError> {
    let metadata = validate_private_file_identity(file)?;
    if metadata.mode() & 0o7777 != 0o600 {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    validate_private_file(file)
}

fn validate_private_directory(directory: &OwnedFd) -> Result<(), SecureFileError> {
    let metadata =
        rustix::fs::fstat(directory).map_err(|error| SecureFileError::Io(io_error(error)))?;
    if rustix::fs::FileType::from_raw_mode(metadata.st_mode) != rustix::fs::FileType::Directory {
        return Err(SecureFileError::NotRegular);
    }
    if metadata.st_uid != effective_user_id() {
        return Err(insecure_private(
            "directory is not owned by the current user",
        ));
    }
    if metadata.st_mode & 0o7777 != 0o700 {
        return Err(insecure_private("directory mode is not 0700"));
    }
    Ok(())
}

fn make_private_directory(directory: &OwnedFd) -> Result<(), SecureFileError> {
    let metadata =
        rustix::fs::fstat(directory).map_err(|error| SecureFileError::Io(io_error(error)))?;
    if rustix::fs::FileType::from_raw_mode(metadata.st_mode) != rustix::fs::FileType::Directory {
        return Err(SecureFileError::NotRegular);
    }
    if metadata.st_uid != effective_user_id() {
        return Err(insecure_private(
            "directory is not owned by the current user",
        ));
    }
    if metadata.st_mode & 0o7777 != 0o700 {
        rustix::fs::fchmod(directory, Mode::from_raw_mode(0o700))
            .map_err(|error| SecureFileError::Io(io_error(error)))?;
    }
    validate_private_directory(directory)
}

#[derive(Clone, Copy)]
pub(super) struct PrivateDirectoryIdentity {
    device: u64,
    inode: u64,
}

fn directory_identity(
    directory: &impl rustix::fd::AsFd,
) -> Result<PrivateDirectoryIdentity, SecureFileError> {
    let metadata =
        rustix::fs::fstat(directory).map_err(|error| SecureFileError::Io(io_error(error)))?;
    if rustix::fs::FileType::from_raw_mode(metadata.st_mode) != rustix::fs::FileType::Directory {
        return Err(SecureFileError::NotRegular);
    }
    Ok(PrivateDirectoryIdentity {
        device: metadata.st_dev as u64,
        inode: metadata.st_ino as u64,
    })
}

fn validate_bound_directory(
    directory: &OwnedFd,
    expected: &PrivateDirectoryIdentity,
) -> Result<(), SecureFileError> {
    validate_private_directory(directory)?;
    let actual = directory_identity(directory)?;
    if (actual.device, actual.inode) != (expected.device, expected.inode) {
        return Err(SecureFileError::Changed);
    }
    Ok(())
}

fn components(path: &Path) -> Result<Vec<OsString>, SecureFileError> {
    validate_absolute_file_path(path)?;
    Ok(path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_os_string()),
            _ => None,
        })
        .collect())
}

fn open_root() -> Result<OwnedFd, SecureFileError> {
    rustix::fs::open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| SecureFileError::Io(io_error(error)))
}

fn open_directory(parent: &OwnedFd, name: &OsStr) -> Result<OwnedFd, Errno> {
    rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
}

/// Directories created while walking a path. On an unsuccessful walk these
/// are removed deepest-first, but only while their original parent entry
/// still names the exact directory we created.
struct CreatedDirectories {
    entries: Vec<CreatedDirectory>,
}

struct CreatedDirectory {
    parent: OwnedFd,
    name: OsString,
    device: rustix::fs::Dev,
    inode: u64,
}

impl CreatedDirectories {
    fn record(
        &mut self,
        parent: &OwnedFd,
        name: &OsStr,
        directory: &OwnedFd,
    ) -> Result<(), SecureFileError> {
        let metadata =
            rustix::fs::fstat(directory).map_err(|error| SecureFileError::Io(io_error(error)))?;
        if rustix::fs::FileType::from_raw_mode(metadata.st_mode) != rustix::fs::FileType::Directory
        {
            return Err(SecureFileError::NotRegular);
        }
        let parent =
            rustix::io::dup(parent).map_err(|error| SecureFileError::Io(io_error(error)))?;
        self.entries.push(CreatedDirectory {
            parent,
            name: name.to_os_string(),
            device: metadata.st_dev,
            inode: metadata.st_ino,
        });
        Ok(())
    }

    fn disarm(mut self) {
        self.entries.clear();
    }
}

impl Drop for CreatedDirectories {
    fn drop(&mut self) {
        for created in self.entries.iter().rev() {
            let Ok(actual) =
                rustix::fs::statat(&created.parent, &created.name, AtFlags::SYMLINK_NOFOLLOW)
            else {
                continue;
            };
            if rustix::fs::FileType::from_raw_mode(actual.st_mode)
                != rustix::fs::FileType::Directory
                || (actual.st_dev, actual.st_ino) != (created.device, created.inode)
            {
                continue;
            }
            // The parent descriptor and the checked directory identity keep
            // cleanup confined to the path walk. A non-empty or concurrently
            // replaced directory is deliberately retained.
            let _ = rustix::fs::unlinkat(&created.parent, &created.name, AtFlags::REMOVEDIR);
        }
    }
}

fn create_directory_at(
    parent: &OwnedFd,
    name: &OsStr,
    mode: Mode,
    created: &mut CreatedDirectories,
) -> Result<Option<OwnedFd>, SecureFileError> {
    match rustix::fs::mkdirat(parent, name, mode) {
        Ok(()) => {}
        Err(Errno::EXIST) => return Ok(None),
        Err(error) => return Err(SecureFileError::Io(io_error(error))),
    }
    let directory =
        open_directory(parent, name).map_err(|error| SecureFileError::Io(io_error(error)))?;
    // Record only after acquiring a descriptor for the object we made. If
    // a hostile rename wins before this open, leaving the entry behind is
    // safer than guessing which object to remove.
    created.record(parent, name, &directory)?;
    Ok(Some(directory))
}

#[cfg(not(target_os = "macos"))]
fn open_root_component(parent: &OwnedFd, name: &OsStr) -> Result<OwnedFd, Errno> {
    open_directory(parent, name)
}

#[cfg(target_os = "macos")]
fn open_root_component(parent: &OwnedFd, name: &OsStr) -> Result<OwnedFd, Errno> {
    // Root components are no different from caller-controlled descendants,
    // except for macOS's system-owned `/var -> private/var` and
    // `/tmp -> private/tmp` aliases. Never follow arbitrary root links.
    match open_directory(parent, name) {
        Ok(directory) => Ok(directory),
        Err(error) if name == OsStr::new("var") || name == OsStr::new("tmp") => {
            open_macos_system_alias(parent, name).or(Err(error))
        }
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "macos")]
fn open_macos_system_alias(root: &OwnedFd, name: &OsStr) -> Result<OwnedFd, Errno> {
    let before = rustix::fs::statat(root, name, AtFlags::SYMLINK_NOFOLLOW)?;
    if rustix::fs::FileType::from_raw_mode(before.st_mode) != rustix::fs::FileType::Symlink
        || before.st_uid != 0
    {
        return Err(Errno::LOOP);
    }
    let target = rustix::fs::readlinkat(root, name, Vec::new())?;
    match (name.to_str(), target.as_bytes()) {
        (Some("var"), b"private/var" | b"/private/var")
        | (Some("tmp"), b"private/tmp" | b"/private/tmp") => {}
        _ => return Err(Errno::LOOP),
    }

    let followed = rustix::fs::openat(
        root,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let private = open_directory(root, OsStr::new("private"))?;
    let expected = open_directory(&private, name)?;
    let followed_stat = rustix::fs::fstat(&followed)?;
    let expected_stat = rustix::fs::fstat(&expected)?;
    let after = rustix::fs::statat(root, name, AtFlags::SYMLINK_NOFOLLOW)?;
    if rustix::fs::FileType::from_raw_mode(followed_stat.st_mode) != rustix::fs::FileType::Directory
        || followed_stat.st_uid != 0
        || (followed_stat.st_dev, followed_stat.st_ino)
            != (expected_stat.st_dev, expected_stat.st_ino)
        || (before.st_dev, before.st_ino) != (after.st_dev, after.st_ino)
    {
        return Err(Errno::LOOP);
    }
    Ok(followed)
}

fn open_parent(path: &Path, create_parents: bool) -> Result<(OwnedFd, OsString), SecureFileError> {
    let mut components = components(path)?;
    let name = components
        .pop()
        .ok_or_else(|| SecureFileError::InvalidPath(path.display().to_string()))?;
    let mut current = open_root()?;
    let mut created = CreatedDirectories {
        entries: Vec::new(),
    };
    for (index, component) in components.into_iter().enumerate() {
        let opened = if index == 0 {
            open_root_component(&current, &component)
        } else {
            open_directory(&current, &component)
        };
        match opened {
            Ok(next) => current = next,
            Err(Errno::NOENT) if create_parents => {
                current = match create_directory_at(
                    &current,
                    &component,
                    Mode::from_raw_mode(0o755),
                    &mut created,
                )? {
                    Some(next) => next,
                    None => open_directory(&current, &component)
                        .map_err(|error| SecureFileError::Io(io_error(error)))?,
                };
            }
            Err(error) => return Err(SecureFileError::Io(io_error(error))),
        }
    }
    created.disarm();
    Ok((current, name))
}

fn open_regular_at(parent: &OwnedFd, name: &OsStr) -> Result<std::fs::File, SecureFileError> {
    let descriptor = rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| SecureFileError::Io(io_error(error)))?;
    let file = std::fs::File::from(descriptor);
    if !file.metadata()?.file_type().is_file() {
        return Err(SecureFileError::NotRegular);
    }
    Ok(file)
}

pub(super) fn create_private_directory_all(path: &Path) -> Result<(), SecureFileError> {
    let path_components = components(path)?;
    let mut current = open_root()?;
    let mut created = CreatedDirectories {
        entries: Vec::new(),
    };
    for (index, component) in path_components.iter().enumerate() {
        let opened = if index == 0 {
            open_root_component(&current, component)
        } else {
            open_directory(&current, component)
        };
        let (next, was_created) = match opened {
            Ok(next) => (next, false),
            Err(Errno::NOENT) => match create_directory_at(
                &current,
                component,
                Mode::from_raw_mode(0o700),
                &mut created,
            )? {
                Some(next) => (next, true),
                None => (
                    open_directory(&current, component)
                        .map_err(|error| SecureFileError::Io(io_error(error)))?,
                    false,
                ),
            },
            Err(error) => return Err(SecureFileError::Io(io_error(error))),
        };
        if was_created || index + 1 == path_components.len() {
            make_private_directory(&next)?;
        }
        current = next;
    }
    created.disarm();
    Ok(())
}

pub(super) fn create_unique_private_directory(
    parent: &Path,
    prefix: &str,
) -> Result<(OsString, PrivateDirectoryIdentity, std::fs::File), SecureFileError> {
    let parent: OwnedFd = open_private_directory_for_lock(parent)?.into();
    for _ in 0..TEMP_NAME_ATTEMPTS {
        let name = OsString::from(format!("{prefix}{}", random_temp_suffix()?));
        let mut created = CreatedDirectories {
            entries: Vec::new(),
        };
        let Some(directory) =
            create_directory_at(&parent, &name, Mode::from_raw_mode(0o700), &mut created)?
        else {
            continue;
        };
        make_private_directory(&directory)?;

        let expected =
            rustix::fs::fstat(&directory).map_err(|error| SecureFileError::Io(io_error(error)))?;
        let actual = rustix::fs::statat(&parent, &name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|error| SecureFileError::Io(io_error(error)))?;
        if rustix::fs::FileType::from_raw_mode(actual.st_mode) != rustix::fs::FileType::Directory
            || (actual.st_dev, actual.st_ino) != (expected.st_dev, expected.st_ino)
        {
            return Err(SecureFileError::Changed);
        }

        let identity = directory_identity(&directory)?;
        created.disarm();
        return Ok((name, identity, std::fs::File::from(directory)));
    }
    Err(SecureFileError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique private directory",
    )))
}

fn open_existing_private_directory(path: &Path) -> Result<OwnedFd, SecureFileError> {
    let path_components = components(path)?;
    let mut current = open_root()?;
    for (index, component) in path_components.iter().enumerate() {
        current = if index == 0 {
            open_root_component(&current, component)
        } else {
            open_directory(&current, component)
        }
        .map_err(|error| SecureFileError::Io(io_error(error)))?;
    }
    validate_private_directory(&current)?;
    Ok(current)
}

fn open_bound_parent(
    path: &Path,
    expected: &PrivateDirectoryIdentity,
) -> Result<(OwnedFd, OsString), SecureFileError> {
    let parent_path = path
        .parent()
        .ok_or_else(|| SecureFileError::InvalidPath(path.display().to_string()))?;
    let name = path
        .file_name()
        .ok_or_else(|| SecureFileError::InvalidPath(path.display().to_string()))?
        .to_os_string();
    let parent = open_existing_private_directory(parent_path)?;
    validate_bound_directory(&parent, expected)?;
    Ok((parent, name))
}

pub(super) fn open_private_directory_for_lock(
    path: &Path,
) -> Result<std::fs::File, SecureFileError> {
    let path_components = components(path)?;
    let mut current = open_root()?;
    for (index, component) in path_components.iter().enumerate() {
        current = if index == 0 {
            open_root_component(&current, component)
        } else {
            open_directory(&current, component)
        }
        .map_err(|error| SecureFileError::Io(io_error(error)))?;
    }
    make_private_directory(&current)?;
    Ok(std::fs::File::from(current))
}

pub(super) fn open_private_lock_file(path: &Path) -> Result<std::fs::File, SecureFileError> {
    let (parent, name) = open_parent(path, false)?;
    let mut transient_missing = 0;
    let descriptor = loop {
        match rustix::fs::openat(
            &parent,
            &name,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        ) {
            Ok(descriptor) => break descriptor,
            // APFS can report a transient ENOENT when two creators race on
            // the same absent name. The already-open parent still binds
            // every retry to the authorized directory.
            Err(Errno::NOENT) if transient_missing < 4 => {
                transient_missing += 1;
                std::thread::yield_now();
            }
            Err(error) => return Err(SecureFileError::Io(io_error(error))),
        }
    };
    let file = std::fs::File::from(descriptor);
    validate_private_file_identity(&file)?;
    Ok(file)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn remove_regular_file_if_exists(path: &Path) -> Result<bool, SecureFileError> {
    let (parent, name) = open_parent(path, false)?;
    remove_regular_file_if_exists_at(&parent, &name)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn remove_regular_file_if_exists_at(
    parent: &OwnedFd,
    name: &OsStr,
) -> Result<bool, SecureFileError> {
    let file = match open_regular_at(parent, name) {
        Ok(file) => file,
        Err(SecureFileError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    let expected = file_identity(&file.metadata()?);

    for _ in 0..TEMP_NAME_ATTEMPTS {
        let temporary = OsString::from(format!(".octet-delete-{}", random_temp_suffix()?));
        match rustix::fs::renameat_with(parent, name, parent, &temporary, RenameFlags::NOREPLACE) {
            Ok(()) => {}
            Err(Errno::EXIST) => continue,
            Err(Errno::NOENT) => return Err(SecureFileError::Changed),
            Err(Errno::NOSYS | Errno::OPNOTSUPP | Errno::INVAL) => {
                return Err(SecureFileError::PublicationUnavailable);
            }
            Err(error) => return Err(SecureFileError::Io(io_error(error))),
        }

        let moved_is_expected = matches!(
            named_file_identity(parent, &temporary, false),
            Ok(actual) if same_object(actual, expected)
        );
        if !moved_is_expected {
            // Restore only into an empty original name. If another writer
            // has already published there, preserve both objects and let
            // later recovery handle the randomized orphan.
            let _ =
                rustix::fs::renameat_with(parent, &temporary, parent, name, RenameFlags::NOREPLACE);
            return Err(SecureFileError::Changed);
        }

        match rustix::fs::unlinkat(parent, &temporary, AtFlags::empty()) {
            Ok(()) => {
                rustix::fs::fsync(parent).map_err(|error| SecureFileError::Io(io_error(error)))?;
                return Ok(true);
            }
            Err(error) => {
                let _ = rustix::fs::renameat_with(
                    parent,
                    &temporary,
                    parent,
                    name,
                    RenameFlags::NOREPLACE,
                );
                return Err(SecureFileError::Io(io_error(error)));
            }
        }
    }
    Err(SecureFileError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique secure deletion name",
    )))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn remove_regular_file_if_exists(_path: &Path) -> Result<bool, SecureFileError> {
    Err(SecureFileError::PublicationUnavailable)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn remove_regular_file_if_exists_in(
    path: &Path,
    expected: &PrivateDirectoryIdentity,
) -> Result<bool, SecureFileError> {
    let (parent, name) = open_bound_parent(path, expected)?;
    remove_regular_file_if_exists_at(&parent, &name)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn remove_regular_file_if_exists_in(
    _path: &Path,
    _expected: &PrivateDirectoryIdentity,
) -> Result<bool, SecureFileError> {
    Err(SecureFileError::PublicationUnavailable)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg(test)]
pub(super) fn remove_empty_private_directory_if_exists(
    path: &Path,
) -> Result<bool, SecureFileError> {
    let (parent, name) = open_parent(path, false)?;
    remove_empty_private_directory_if_exists_at(&parent, &name, None)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn remove_empty_private_directory_if_exists_at(
    parent: &OwnedFd,
    name: &OsStr,
    bound: Option<&PrivateDirectoryIdentity>,
) -> Result<bool, SecureFileError> {
    let directory = match open_directory(parent, name) {
        Ok(directory) => directory,
        Err(Errno::NOENT) => return Ok(false),
        Err(error) => return Err(SecureFileError::Io(io_error(error))),
    };
    validate_private_directory(&directory)?;
    if let Some(bound) = bound {
        validate_bound_directory(&directory, bound)?;
    }
    let expected =
        rustix::fs::fstat(&directory).map_err(|error| SecureFileError::Io(io_error(error)))?;

    for _ in 0..TEMP_NAME_ATTEMPTS {
        let temporary =
            OsString::from(format!(".octet-directory-delete-{}", random_temp_suffix()?));
        match rustix::fs::renameat_with(parent, name, parent, &temporary, RenameFlags::NOREPLACE) {
            Ok(()) => {}
            Err(Errno::EXIST) => continue,
            Err(Errno::NOENT) => return Err(SecureFileError::Changed),
            Err(Errno::NOSYS | Errno::OPNOTSUPP | Errno::INVAL) => {
                return Err(SecureFileError::PublicationUnavailable);
            }
            Err(error) => return Err(SecureFileError::Io(io_error(error))),
        }

        let moved_is_expected = open_directory(parent, &temporary)
            .and_then(|moved| rustix::fs::fstat(&moved))
            .is_ok_and(|actual| {
                (actual.st_dev, actual.st_ino) == (expected.st_dev, expected.st_ino)
            });
        if !moved_is_expected {
            // Never delete a replacement that won the race. Restore it only
            // into an empty original name; otherwise preserve both paths.
            let _ =
                rustix::fs::renameat_with(parent, &temporary, parent, name, RenameFlags::NOREPLACE);
            return Err(SecureFileError::Changed);
        }

        match rustix::fs::unlinkat(parent, &temporary, AtFlags::REMOVEDIR) {
            Ok(()) => {
                rustix::fs::fsync(parent).map_err(|error| SecureFileError::Io(io_error(error)))?;
                return Ok(true);
            }
            Err(error) => {
                let _ = rustix::fs::renameat_with(
                    parent,
                    &temporary,
                    parent,
                    name,
                    RenameFlags::NOREPLACE,
                );
                return Err(SecureFileError::Io(io_error(error)));
            }
        }
    }
    Err(SecureFileError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique secure directory deletion name",
    )))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[cfg(test)]
pub(super) fn remove_empty_private_directory_if_exists(
    _path: &Path,
) -> Result<bool, SecureFileError> {
    Err(SecureFileError::PublicationUnavailable)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn remove_empty_private_directory_if_exists_bound(
    path: &Path,
    expected: &PrivateDirectoryIdentity,
) -> Result<bool, SecureFileError> {
    let (parent, name) = open_parent(path, false)?;
    remove_empty_private_directory_if_exists_at(&parent, &name, Some(expected))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn remove_empty_private_directory_if_exists_bound(
    _path: &Path,
    _expected: &PrivateDirectoryIdentity,
) -> Result<bool, SecureFileError> {
    Err(SecureFileError::PublicationUnavailable)
}

pub(super) fn read_regular_file_bounded(
    path: &Path,
    limit: usize,
) -> Result<Vec<u8>, SecureFileError> {
    let (parent, name) = open_parent(path, false)?;
    read_open_regular(open_regular_at(&parent, &name)?, limit)
}

pub(super) fn read_private_file_bounded(
    path: &Path,
    limit: usize,
) -> Result<Vec<u8>, SecureFileError> {
    read_open_regular(open_private_file_for_read(path)?, limit)
}

pub(super) fn open_private_file_for_read(path: &Path) -> Result<std::fs::File, SecureFileError> {
    let (parent, name) = open_parent(path, false)?;
    let file = open_regular_at(&parent, &name)?;
    validate_private_file(&file)?;
    Ok(file)
}

pub(super) fn read_regular_file_bounded_by(
    path: &Path,
    upper_limit: usize,
    byte_limit: &dyn Fn(&[u8]) -> usize,
) -> Result<Vec<u8>, SecureFileError> {
    let (parent, name) = open_parent(path, false)?;
    read_open_regular_bounded_by(open_regular_at(&parent, &name)?, upper_limit, byte_limit)
}

pub(super) fn open_regular_file_for_read(path: &Path) -> Result<std::fs::File, SecureFileError> {
    let (parent, name) = open_parent(path, false)?;
    open_regular_at(&parent, &name)
}

#[cfg(test)]
pub(super) fn open_regular_file_for_read_in(
    path: &Path,
    expected: &PrivateDirectoryIdentity,
) -> Result<std::fs::File, SecureFileError> {
    let (parent, name) = open_bound_parent(path, expected)?;
    let file = open_regular_at(&parent, &name)?;
    validate_private_file(&file)?;
    Ok(file)
}

pub(super) fn open_regular_file_for_append(path: &Path) -> Result<std::fs::File, SecureFileError> {
    let (parent, name) = open_parent(path, false)?;
    let descriptor = rustix::fs::openat(
        &parent,
        &name,
        OFlags::RDWR | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| SecureFileError::Io(io_error(error)))?;
    let file = std::fs::File::from(descriptor);
    make_private_file(&file)?;
    Ok(file)
}

pub(super) fn open_regular_file_for_append_in(
    path: &Path,
    expected: &PrivateDirectoryIdentity,
) -> Result<std::fs::File, SecureFileError> {
    let (parent, name) = open_bound_parent(path, expected)?;
    let descriptor = rustix::fs::openat(
        &parent,
        &name,
        OFlags::RDWR | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| SecureFileError::Io(io_error(error)))?;
    let file = std::fs::File::from(descriptor);
    make_private_file(&file)?;
    Ok(file)
}

pub(super) fn create_regular_file_for_append(
    path: &Path,
) -> Result<std::fs::File, SecureFileError> {
    let (parent, name) = open_parent(path, false)?;
    let descriptor = rustix::fs::openat(
        &parent,
        &name,
        OFlags::RDWR
            | OFlags::APPEND
            | OFlags::CREATE
            | OFlags::EXCL
            | OFlags::NOFOLLOW
            | OFlags::NONBLOCK
            | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|error| SecureFileError::Io(io_error(error)))?;
    let file = std::fs::File::from(descriptor);
    validate_private_file(&file)?;
    Ok(file)
}

pub(super) fn create_regular_file_for_append_in(
    path: &Path,
    expected: &PrivateDirectoryIdentity,
) -> Result<std::fs::File, SecureFileError> {
    let (parent, name) = open_bound_parent(path, expected)?;
    let descriptor = rustix::fs::openat(
        &parent,
        &name,
        OFlags::RDWR
            | OFlags::APPEND
            | OFlags::CREATE
            | OFlags::EXCL
            | OFlags::NOFOLLOW
            | OFlags::NONBLOCK
            | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|error| SecureFileError::Io(io_error(error)))?;
    let file = std::fs::File::from(descriptor);
    validate_private_file(&file)?;
    Ok(file)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    owner: u32,
    group: u32,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

fn file_identity(metadata: &std::fs::Metadata) -> FileIdentity {
    FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        links: metadata.nlink(),
        owner: metadata.uid(),
        group: metadata.gid(),
        size: metadata.size(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    }
}

pub(super) struct PrivateLockIdentity(FileIdentity);

fn current_private_file_identity(path: &Path) -> Result<FileIdentity, SecureFileError> {
    let (parent, name) = open_parent(path, false)?;
    let file = open_regular_at(&parent, &name)?;
    validate_private_file(&file)?;
    Ok(file_identity(&file.metadata()?))
}

pub(super) fn validate_private_lock_after_acquire(
    path: &Path,
    file: &std::fs::File,
) -> Result<PrivateLockIdentity, SecureFileError> {
    make_private_file(file)?;
    let identity = file_identity(&file.metadata()?);
    if current_private_file_identity(path)? != identity {
        return Err(SecureFileError::Changed);
    }
    Ok(PrivateLockIdentity(identity))
}

pub(super) fn revalidate_private_lock_before_release(
    path: &Path,
    file: &std::fs::File,
    expected: &PrivateLockIdentity,
) -> Result<(), SecureFileError> {
    validate_private_file(file)?;
    if file_identity(&file.metadata()?) != expected.0
        || current_private_file_identity(path)? != expected.0
    {
        return Err(SecureFileError::Changed);
    }
    Ok(())
}

fn read_for_mutation(
    file: std::fs::File,
    limit: usize,
) -> Result<(Vec<u8>, std::fs::Permissions, FileIdentity), SecureFileError> {
    let before = file.metadata()?;
    let permissions = before.permissions();
    let identity = file_identity(&before);
    let bytes = read_open_regular(file.try_clone()?, limit)?;
    if file_identity(&file.metadata()?) != identity {
        return Err(SecureFileError::Changed);
    }
    Ok((bytes, permissions, identity))
}

fn read_optional(
    parent: &OwnedFd,
    name: &OsStr,
    limit: usize,
) -> Result<Option<(Vec<u8>, std::fs::Permissions, FileIdentity)>, SecureFileError> {
    match open_regular_at(parent, name) {
        Ok(file) => Ok(Some(read_for_mutation(file, limit)?)),
        Err(SecureFileError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn read_optional_private(
    parent: &OwnedFd,
    name: &OsStr,
    limit: usize,
) -> Result<Option<(Vec<u8>, std::fs::Permissions, FileIdentity)>, SecureFileError> {
    match open_regular_at(parent, name) {
        Ok(file) => {
            make_private_file(&file)?;
            Ok(Some(read_for_mutation(file, limit)?))
        }
        Err(SecureFileError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn read_optional_private_strict(
    parent: &OwnedFd,
    name: &OsStr,
    limit: usize,
) -> Result<Option<(Vec<u8>, std::fs::Permissions, FileIdentity)>, SecureFileError> {
    match open_regular_at(parent, name) {
        Ok(file) => {
            validate_private_file(&file)?;
            Ok(Some(read_for_mutation(file, limit)?))
        }
        Err(SecureFileError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn named_file_identity(
    parent: &OwnedFd,
    name: &OsStr,
    private: bool,
) -> Result<FileIdentity, SecureFileError> {
    let file = open_regular_at(parent, name)?;
    if private {
        validate_private_file(&file)?;
    }
    Ok(file_identity(&file.metadata()?))
}

fn read_named_state(
    parent: &OwnedFd,
    name: &OsStr,
    limit: usize,
    private: bool,
) -> Result<(Vec<u8>, FileIdentity), SecureFileError> {
    let file = open_regular_at(parent, name)?;
    if private {
        validate_private_file(&file)?;
    }
    let (bytes, _, identity) = read_for_mutation(file, limit)?;
    Ok((bytes, identity))
}

fn same_object(left: FileIdentity, right: FileIdentity) -> bool {
    (left.device, left.inode) == (right.device, right.inode)
}

fn same_stable_state(left: FileIdentity, right: FileIdentity) -> bool {
    left.device == right.device
        && left.inode == right.inode
        && left.mode == right.mode
        && left.links == right.links
        && left.owner == right.owner
        && left.group == right.group
        && left.size == right.size
        && left.modified_seconds == right.modified_seconds
        && left.modified_nanoseconds == right.modified_nanoseconds
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn exchange_names(
    parent: &OwnedFd,
    source: &OsStr,
    destination: &OsStr,
) -> Result<(), SecureFileError> {
    match rustix::fs::renameat_with(parent, source, parent, destination, RenameFlags::EXCHANGE) {
        Ok(()) => Ok(()),
        Err(Errno::NOSYS | Errno::OPNOTSUPP | Errno::INVAL) => {
            Err(SecureFileError::PublicationUnavailable)
        }
        Err(error) => Err(SecureFileError::Io(io_error(error))),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn exchange_names(
    _parent: &OwnedFd,
    _source: &OsStr,
    _destination: &OsStr,
) -> Result<(), SecureFileError> {
    Err(SecureFileError::PublicationUnavailable)
}

fn unlink_if_still_named(parent: &OwnedFd, name: &OsStr, expected: FileIdentity) {
    let Ok(actual) = named_file_identity(parent, name, false) else {
        return;
    };
    if same_object(actual, expected) {
        let _ = rustix::fs::unlinkat(parent, name, AtFlags::empty());
    }
}

pub(super) struct PreparedMutation {
    parent: OwnedFd,
    name: OsString,
    original: Option<Vec<u8>>,
    original_identity: Option<FileIdentity>,
    permissions: Option<std::fs::Permissions>,
    limit: usize,
    private: bool,
}

impl PreparedMutation {
    pub(super) fn prepare(
        path: &Path,
        create_parents: bool,
        limit: usize,
    ) -> Result<Self, SecureFileError> {
        Self::prepare_impl(path, create_parents, limit, false)
    }

    pub(super) fn prepare_private(path: &Path, limit: usize) -> Result<Self, SecureFileError> {
        Self::prepare_impl(path, false, limit, true)
    }

    fn prepare_impl(
        path: &Path,
        create_parents: bool,
        limit: usize,
        private: bool,
    ) -> Result<Self, SecureFileError> {
        let (parent, name) = open_parent(path, create_parents)?;
        let current = if private {
            read_optional_private(&parent, &name, limit)
        } else {
            read_optional(&parent, &name, limit)
        }?;
        let (original, permissions, original_identity) = match current {
            Some((bytes, permissions, identity)) => {
                (Some(bytes), Some(permissions), Some(identity))
            }
            None => (None, None, None),
        };
        Ok(Self {
            parent,
            name,
            original,
            original_identity,
            permissions,
            limit,
            private,
        })
    }

    pub(super) fn original(&self) -> Option<&[u8]> {
        self.original.as_deref()
    }

    fn unchanged(&self) -> Result<bool, SecureFileError> {
        let current = if self.private {
            read_optional_private_strict(&self.parent, &self.name, self.limit)?
        } else {
            read_optional(&self.parent, &self.name, self.limit)?
        };
        Ok(match (&self.original, self.original_identity, current) {
            (None, None, None) => true,
            (Some(expected), Some(expected_identity), Some((actual, _, actual_identity))) => {
                expected == &actual && expected_identity == actual_identity
            }
            _ => false,
        })
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn remove(self) -> Result<(), SecureFileError> {
        let expected = self.original_identity.ok_or(SecureFileError::Changed)?;
        let _target_lock = lock_current_target(&self.parent, &self.name);
        if !self.unchanged()? {
            return Err(SecureFileError::Changed);
        }
        for _ in 0..TEMP_NAME_ATTEMPTS {
            let temporary = OsString::from(format!(".octet-delete-{}", random_temp_suffix()?));
            match rustix::fs::renameat_with(
                &self.parent,
                &self.name,
                &self.parent,
                &temporary,
                RenameFlags::NOREPLACE,
            ) {
                Ok(()) => {}
                Err(Errno::EXIST) => continue,
                Err(Errno::NOENT) => return Err(SecureFileError::Changed),
                Err(Errno::NOSYS | Errno::OPNOTSUPP | Errno::INVAL) => {
                    return Err(SecureFileError::PublicationUnavailable)
                }
                Err(error) => return Err(SecureFileError::Io(io_error(error))),
            }
            let moved_is_expected = matches!(
                named_file_identity(&self.parent, &temporary, self.private),
                Ok(actual) if same_stable_state(actual, expected)
            );
            if !moved_is_expected {
                let _ = rustix::fs::renameat_with(
                    &self.parent,
                    &temporary,
                    &self.parent,
                    &self.name,
                    RenameFlags::NOREPLACE,
                );
                return Err(SecureFileError::Changed);
            }
            match rustix::fs::unlinkat(&self.parent, &temporary, AtFlags::empty()) {
                Ok(()) => {
                    rustix::fs::fsync(&self.parent)
                        .map_err(|error| SecureFileError::Io(io_error(error)))?;
                    return Ok(());
                }
                Err(error) => {
                    let _ = rustix::fs::renameat_with(
                        &self.parent,
                        &temporary,
                        &self.parent,
                        &self.name,
                        RenameFlags::NOREPLACE,
                    );
                    return Err(SecureFileError::Io(io_error(error)));
                }
            }
        }
        Err(SecureFileError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate a unique secure deletion name",
        )))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(super) fn remove(self) -> Result<(), SecureFileError> {
        let _ = self;
        Err(SecureFileError::PublicationUnavailable)
    }

    /// Atomically swap the staged file with an existing destination, then
    /// verify that the displaced object is still the one observed during
    /// preparation. Displacing exactly that object is the commit point,
    /// even if another writer has since replaced our file. If it is not,
    /// restore the displaced object only while the destination still names
    /// our staged file.
    fn publish_existing(
        &self,
        temp_name: &OsStr,
        temporary_identity: FileIdentity,
    ) -> Result<(), SecureFileError> {
        let expected_bytes = self
            .original
            .as_deref()
            .expect("existing target has original bytes");
        let expected_identity = self
            .original_identity
            .expect("existing target has original identity");

        exchange_names(&self.parent, temp_name, &self.name)?;

        let displaced = read_named_state(&self.parent, temp_name, self.limit, self.private).ok();
        let destination_is_temporary = matches!(
            named_file_identity(&self.parent, &self.name, self.private),
            Ok(identity) if same_object(identity, temporary_identity)
        );
        let displaced_is_expected = matches!(
            displaced.as_ref(),
            Some((bytes, identity))
                if bytes == expected_bytes && same_stable_state(*identity, expected_identity)
        );

        if displaced_is_expected {
            let (_, displaced_identity) = displaced.expect("displaced state was checked");
            unlink_if_still_named(&self.parent, temp_name, displaced_identity);
            return Ok(());
        }

        // A concurrent writer won the race. Do not replace anything it
        // published after the swap; roll back only while the destination
        // still names our staged object.
        if destination_is_temporary {
            exchange_names(&self.parent, temp_name, &self.name)?;
            unlink_if_still_named(&self.parent, temp_name, temporary_identity);
        }
        Err(SecureFileError::Changed)
    }

    pub(super) fn commit(
        self,
        data: &[u8],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), SecureFileError> {
        self.commit_with_permissions(data, cancelled, None)
    }

    pub(super) fn commit_private(
        self,
        data: &[u8],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), SecureFileError> {
        self.commit_with_permissions(
            data,
            cancelled,
            Some(std::fs::Permissions::from_mode(0o600)),
        )
    }

    fn commit_with_permissions(
        self,
        data: &[u8],
        cancelled: &dyn Fn() -> bool,
        forced_permissions: Option<std::fs::Permissions>,
    ) -> Result<(), SecureFileError> {
        let (temp_name, mut temp_file) = {
            let mut created = None;
            for _ in 0..TEMP_NAME_ATTEMPTS {
                let candidate = OsString::from(format!(".octet-tmp-{}", random_temp_suffix()?));
                match rustix::fs::openat(
                    &self.parent,
                    &candidate,
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::from_raw_mode(0o600),
                ) {
                    Ok(descriptor) => {
                        created = Some((candidate, std::fs::File::from(descriptor)));
                        break;
                    }
                    Err(Errno::EXIST) => continue,
                    Err(error) => return Err(SecureFileError::Io(io_error(error))),
                }
            }
            created.ok_or_else(|| {
                SecureFileError::Io(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "could not allocate a unique secure temporary file",
                ))
            })?
        };

        let result = (|| -> Result<(), SecureFileError> {
            for chunk in data.chunks(64 * 1024) {
                if cancelled() {
                    return Err(SecureFileError::Cancelled);
                }
                temp_file.write_all(chunk)?;
            }
            if cancelled() {
                return Err(SecureFileError::Cancelled);
            }
            temp_file.sync_all()?;
            if let Some(permissions) = forced_permissions.or_else(|| self.permissions.clone()) {
                temp_file.set_permissions(permissions)?;
                temp_file.sync_all()?;
            }
            let temporary_identity = file_identity(&temp_file.metadata()?);
            // Held through publication; see `lock_current_target`.
            let _target_lock = self
                .original
                .is_some()
                .then(|| lock_current_target(&self.parent, &self.name))
                .flatten();
            if !self.unchanged()? {
                return Err(SecureFileError::Changed);
            }
            if cancelled() {
                return Err(SecureFileError::Cancelled);
            }
            if self.original.is_none() {
                // Publishing a newly created file through a hard link is an
                // atomic no-replace operation. A plain rename here would
                // overwrite a target created after `unchanged()` returned.
                match rustix::fs::linkat(
                    &self.parent,
                    &temp_name,
                    &self.parent,
                    &self.name,
                    AtFlags::empty(),
                ) {
                    Ok(()) => {
                        unlink_if_still_named(&self.parent, &temp_name, temporary_identity);
                    }
                    Err(Errno::EXIST) => return Err(SecureFileError::Changed),
                    Err(error) => return Err(SecureFileError::Io(io_error(error))),
                }
            } else {
                self.publish_existing(&temp_name, temporary_identity)?;
            }
            rustix::fs::fsync(&self.parent)
                .map_err(|error| SecureFileError::Io(io_error(error)))?;
            Ok(())
        })();

        if result.is_err() {
            if let Ok(metadata) = temp_file.metadata() {
                unlink_if_still_named(&self.parent, &temp_name, file_identity(&metadata));
            }
        }
        result
    }
}
