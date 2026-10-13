//! Platform seams for the private-file checks. Unix pins identity by
//! dev/inode and refuses symlinks at open; Windows opens reparse points
//! as themselves and compares size and timestamps, relying on the private
//! per-user temp directory for the rest.
use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

/// Open an existing regular file for reading without following a final symlink.
pub(super) fn open_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    #[cfg(windows)]
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    options.open(path)
}

#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// Whether `opened` is the same file that `before` described.
pub(super) fn same_file(before: &Metadata, opened: &Metadata) -> bool {
    #[cfg(unix)]
    {
        opened.dev() == before.dev() && opened.ino() == before.ino()
    }
    #[cfg(not(unix))]
    {
        opened.len() == before.len() && modified(opened) == modified(before)
    }
}

/// Whether a file's content stamps are unchanged between two reads.
pub(super) fn unchanged(before: &Metadata, after: &Metadata) -> bool {
    #[cfg(unix)]
    {
        after.mtime() == before.mtime()
            && after.mtime_nsec() == before.mtime_nsec()
            && after.ctime() == before.ctime()
            && after.ctime_nsec() == before.ctime_nsec()
    }
    #[cfg(not(unix))]
    {
        modified(after) == modified(before)
    }
}

#[cfg(not(unix))]
fn modified(metadata: &Metadata) -> Option<std::time::SystemTime> {
    metadata.modified().ok()
}

/// Restrict a freshly created directory to its owner.
pub(super) fn make_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        // Per-user temp directories already carry an owner-only ACL.
        fs::metadata(path).map(|_| ())
    }
}

/// Create a new owner-only executable file for writing.
pub(super) fn create_private_executable(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o700);
    options.open(path)
}

/// Drop write permission from a fully written executable.
pub(super) fn seal_executable(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        file.set_permissions(fs::Permissions::from_mode(0o500))
    }
    #[cfg(not(unix))]
    {
        let mut permissions = file.metadata()?.permissions();
        permissions.set_readonly(true);
        file.set_permissions(permissions)
    }
}
